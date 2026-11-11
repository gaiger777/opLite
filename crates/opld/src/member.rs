//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태, 사용량 지표, VXLAN 장치, VM 방화벽, APT
//! 업데이트. 위트니스는 노드 간 서버와 클러스터 저장소, 승격, Node 등록만 띄운다.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::serve::ListenerExt;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{
    AppState, AuditLog, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig,
    IncomingConfig, IncomingMigrations, RemotePoller, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig, TelemetryReporter, UefiVarsFetcher, UpdateAgent, Updates,
};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError, Typed};
use opl_driver::{
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage, migration_tls_dir, write_migration_tls,
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirewallGate, FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::firewall::{FirewallAgent, FirewallRules};
use crate::host::Host;
use crate::metrics::host as metrics_host;
use crate::metrics::sampler::MetricsSampler;
use crate::metrics::store::{MetricsStore, StoreSource};
use crate::notify::Notifier;
use crate::overlay::{OverlayAgent, cluster_tag};
use crate::pools::PoolMounter;
use crate::runtime::{Handles, on, run_on};

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 데이터 디렉터리 아래 마이그레이션 디스크 기록 파일을 두는 디렉터리(로컬 디스크 마이그레이션 설계 §3.6)
const MIGRATION_DISKS_DIR: &str = "migration-disks";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록과 기능 표시(위트니스는 기능 표시만)를 다시 확인하는 간격. 관리자가 브리지를 만들면
/// 이만큼 뒤에 스케줄러가 알고, 옛 버전 리더가 지운 기능 표시는 이만큼 뒤에 되살아난다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
/// 다시 시작한 learner가 승격을 다시 요청하는 간격. 실패할 때마다 두 배로 늘려 최대 시간까지 둔다.
const PROMOTE_RETRY_MIN: Duration = Duration::from_secs(5);
const PROMOTE_RETRY_MAX: Duration = Duration::from_secs(60);

pub fn pki_dir(config: &Config) -> PathBuf {
    config.data_dir.join(PKI_DIR)
}

pub fn cluster_dir(config: &Config) -> PathBuf {
    config.data_dir.join(CLUSTER_DIR)
}

/// 멤버가 되는 방법
pub enum Bootstrap {
    /// 이미 멤버다(다시 시작).
    Restart,
    /// 이 노드 하나로 새 클러스터를 만든다.
    Init { name: String, ca: ClusterCa },
    /// 합류가 받아들여졌다(이 노드는 이미 learner다). 인증서를 저장한 뒤 `via`(받아 준 노드의
    /// 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 위트니스가 `oplctl cluster init`을 받았을 때의 답
pub const WITNESS_CANNOT_INIT: &str =
    "a witness cannot start a cluster; join an existing cluster instead";

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config(witness: bool) -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        // 위트니스는 투표는 하지만 후보가 되지 않는다. 리더 일은 늘 일반 노드가 한다.
        enable_elect: !witness,
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        // 스냅샷 조각마다 걸리는 제한 시간(밀리초). 기본 200ms로는 JSON으로 부푼 1MiB 조각(3.5MiB쯤)이
        // 바쁜 노드에서 끝나지 못할 수 있다.
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

struct Server<E> {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), E>>,
}

/// 위트니스가 띄우지 않는 것: HTTPS API와 VM·풀을 다스리는 일, 리더의 일
struct Services {
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    schedule: SnapshotScheduler,
    janitor: ClusterJanitor,
    /// 리더가 백업 작업을 실행한다.
    backups: BackupJobScheduler,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
    recovery: HaManager,
    pools: PoolMounter,
    /// 이 노드와 그 VM의 사용량 지표를 잰다.
    metrics: MetricsSampler,
    /// VXLAN 네트워크의 장치를 맞춘다(호스트 네트워크가 없으면 없다).
    overlay: Option<OverlayAgent>,
    /// VM 방화벽 규칙을 맞춘다(방화벽이 없으면 없다).
    firewall: Option<FirewallAgent>,
    /// HA 리스와 watchdog(`[ha] fencing = "off"`면 없다)
    ha: Option<Arc<HaGuard>>,
    /// APT 소스 파일과 주기 업데이트 확인(업데이트가 꺼졌으면 없다)
    updates: Option<UpdateAgent>,
    /// 리더가 텔레메트리 보고를 보낸다(설정에서 켰을 때).
    telemetry: TelemetryReporter,
    /// 리더가 원격 클러스터의 상태를 모은다.
    remotes: RemotePoller,
    /// API가 멈춘 뒤 남은 감사 항목을 쓰고 닫는다.
    audit: Arc<AuditLog>,
}

impl Services {
    /// `left`면 클러스터에서 빠진 것이다(VXLAN 장치와 방화벽 테이블을 지운다).
    async fn shutdown(mut self, left: bool) {
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.audit.shutdown().await;
        self.monitor.shutdown().await;
        self.schedule.shutdown().await;
        self.janitor.shutdown().await;
        self.backups.shutdown().await;
        self.telemetry.shutdown().await;
        self.remotes.shutdown().await;
        self.recovery.shutdown().await;
        self.pools.shutdown().await;
        self.metrics.shutdown().await;
        if let Some(updates) = self.updates {
            updates.shutdown().await;
        }
        if let Some(overlay) = self.overlay {
            overlay.shutdown(left).await;
        }
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        // 작업자가 모두 멈춘 뒤에 멈춰야 테이블을 지운 뒤 시작 직전 확인이 다시 넣지 않는다.
        if let Some(firewall) = self.firewall {
            firewall.shutdown(left).await;
        }
        // 작업자가 모두 멈춘 뒤에 닫아야 도는 HA VM 목록이 맞다.
        if let Some(ha) = &self.ha {
            ha.close();
        }
    }
}

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    /// 위트니스면 없다.
    services: Option<Services>,
    register: JoinHandle<()>,
    promotion: JoinHandle<()>,
    /// Raft가 멈추면 `exited`로 알린다. 정상 종료는 저장소를 닫기 전에 이것부터 멈춘다.
    raft_watch: JoinHandle<()>,
    /// 서버 태스크가 끝나거나 Raft가 멈추면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 인증서가 있으면 멤버로 보므로, 처음 멤버가 되는 경우 `Init`은 마지막에
    /// 노드 인증서를 저장해 도중에 실패하면 이 노드는 계속 미초기화 상태다. `Join`은 이미 learner로
    /// 더해졌으므로 승격을 요청하기 전에 저장한다. 그 뒤에 실패하거나 죽어도 클러스터가 투표 멤버로
    /// 세는 노드가 미초기화로 남지 않고, 다시 시작하면(`Restart`) 승격을 다시 요청한다.
    ///
    /// 서비스를 실행기 셋에 나눠 띄운다(설계 §3.1). Raft(openraft 작업은 `Raft::new`를 부른 실행기에
    /// 생긴다), 노드 간 서버, `HaGuard`, `HaManager`, 노드 감시, 승격은 `rt.core`, API는 `rt.api`, VM·풀을
    /// 다스리는 일과 스케줄러는 `rt.work`에서 돈다. 노드 간 서버가 받은 Raft·리스가 아닌 요청(마이그레이션
    /// 받기, 콘솔 중계, UEFI 변수·감사 로그·지표 읽기)도 `rt.work`에서 돈다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
        rt: &Handles,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        // `[telemetry] ca_file`을 읽지 못하면 아무것도 열기 전에 실패한다.
        let telemetry_config = config.telemetry.telemetry_config()?;
        let id = NodeId(node);
        // Raft·리스 전달·ping 채널의 연결 작업은 core, 콘솔·마이그레이션·감사·지표 채널의 것은 work에서
        // 돈다(처음 연 쪽 실행기와 관계없이).
        let peers = Arc::new(Peers::new(&tls).with_runtimes(rt.core.clone(), rt.work.clone()));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config(config.witness);
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let network = GrpcNetwork::new(peers.clone());
        let cluster = Arc::new(
            run_on(&rt.core, Cluster::open(cluster_config, network))
                .await?
                .with_forwarder(peers.clone()),
        );
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
        if matches!(bootstrap, Bootstrap::Restart)
            && let Err(e) = bootstrap::check_role(&cluster, config)
        {
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        if config.witness {
            let member = Self::start_witness(config, cluster, peers, tls, bootstrap, rt).await?;
            return Ok((member, None));
        }
        // 포트가 이미 쓰이고 있으면 아무것도 띄우기 전에 실패한다.
        let ports = async {
            let api = TcpListener::bind(config.listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.listen))?;
            let peer = TcpListener::bind(config.cluster_listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.cluster_listen))?;
            anyhow::Ok((api, peer))
        };
        let (api_listener, peer_listener) = match ports.await {
            Ok(listeners) => listeners,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 이 클러스터가 만든 VXLAN 장치의 표시. 남의 관리 이름 브리지는 브리지 목록에서 뺀다.
        let host = cluster_tag(&tls.ca_pem).and_then(|tag| {
            let host = Host::from_config(config, &tag)?;
            Ok((host, tag))
        });
        let (mut host, tag) = match host {
            Ok(host) => host,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 등록 때 방화벽 여부를 기록하므로 등록 전에 찾는다.
        host.detect_firewall(&tag).await;
        // 워커와 마이그레이션 받기가 VM을 켜기 직전에 규칙을 넣는다. 에이전트는 아래에서 띄운다.
        let firewall = FirewallRules::new(cluster.clone(), config, &host, tag.clone());
        let gate = firewall.clone().map(|f| f as Arc<dyn FirewallGate>);
        // 살아 있는 VM에 다시 붙으며 띄우는 QMP·모니터 작업이 `work`에 생긴다.
        let hv: Arc<dyn Hypervisor> =
            match run_on(&rt.work, QemuHypervisor::new(host.qemu_config(config))).await {
                Ok(hv) => Arc::new(hv),
                Err(e) => {
                    let _ = cluster.shutdown().await;
                    return Err(e.into());
                }
            };
        // QEMU가 마이그레이션 TLS에 쓰는 노드 인증서. 드라이버의 state_dir은 데이터 디렉터리다(host.rs).
        let tls_dir = migration_tls_dir(&config.data_dir);
        if let Err(e) = write_migration_tls(&tls_dir, &tls.ca_pem, &tls.cert_pem, &tls.key_pem) {
            let _ = cluster.shutdown().await;
            return Err(
                anyhow::Error::new(e).context(format!("cannot write {}", tls_dir.display()))
            );
        }
        // 계획 20 전에 받은 노드 인증서는 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가
        // 실패한다. 노드가 클러스터를 나갔다 다시 들어오면 새 인증서를 받는다.
        if let Ok(false) = opl_peer::pki::allows_key_encipherment(&tls.cert_pem) {
            tracing::warn!(
                "this node's certificate lacks the keyEncipherment key usage; live migration over TLS \
                 will fail with QEMU 8.2 or older until the node leaves and rejoins the cluster"
            );
        }
        // NFS 풀의 마운트 지점은 `{data_dir}/pools/{풀 id}`다.
        let mount_root = config.data_dir.join("pools");
        // 풀 백엔드 종류로 나눠 보내는 저장소 하나를 reconciler와 Task 실행기가 함께 쓴다.
        let files: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, &mount_root));
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let lvm_shared = LvmSharedStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd))
                .with_lvm_shared(Arc::new(lvm_shared)),
        );
        // API 서버, 노드 간 서버, Raft 감시가 보낸다.
        let (exited_tx, exited) = mpsc::channel(3);
        // 받는 VM은 원본 노드에 배정된 채이므로 reconciler 없이 받는다.
        let incoming = IncomingMigrations::new(
            cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node,
                mount_root: mount_root.clone(),
                storage: storage.clone(),
                listen_host: config.migration_listen_host(),
                ports: config.migration.ports.clone(),
                migration_disks: config.data_dir.join(MIGRATION_DISKS_DIR),
            },
        );
        // API가 적고, 다른 노드가 노드 간 서버로 읽는다.
        let audit = match config.audit.audit_config(&config.data_dir, &config.name) {
            Some(audit) => {
                let dir = audit.dir.clone();
                match AuditLog::open(audit) {
                    Ok(audit) => Arc::new(audit),
                    Err(e) => {
                        let _ = cluster.shutdown().await;
                        return Err(anyhow::Error::new(e)
                            .context(format!("cannot open the audit log in {}", dir.display())));
                    }
                }
            }
            None => Arc::new(AuditLog::disabled()),
        };
        // 측정기가 적고, API와 다른 노드(노드 간 서버)가 읽는다.
        let metrics_store = Arc::new(MetricsStore::default());
        let metrics_source = Arc::new(StoreSource::new(node, metrics_store.clone()));
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming.with_firewall(gate.clone())))
            .with_uefi_vars(config.data_dir.clone())
            .with_audit(audit.clone())
            .with_metrics(metrics_source.clone())
            .with_work(rt.work.clone());
        let peer = on(&rt.core, || {
            serve_peer(peer_listener, &tls, service, exited_tx.clone())
        });

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let init = matches!(bootstrap, Bootstrap::Init { .. });
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password =
                        Some(bootstrap::create_cluster(&cluster, config, &host, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_node(&cluster, config, &host).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.mount_root = mount_root.clone();
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
            state.audit = audit;
            state.metrics = Some(metrics_source);
            let created = match password {
                Some(admin_password) => {
                    let token = issue_join_token(&state, DEFAULT_JOIN_TTL)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.message))?;
                    Some(Created {
                        admin_password,
                        join_url: token.url,
                        ca_fingerprint: fingerprint(&tls.ca_pem)?,
                    })
                }
                None => None,
            };
            let api_tls = api_server_config(&tls)?;
            if init {
                tls.save(&pki_dir(config))?;
            }
            anyhow::Ok((state, created, api_tls))
        };
        let (state, created, api_tls) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = peer.stop.send(());
                let _ = peer.task.await;
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };

        // 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스는 뒤에서 기록한다.
        let promotion = on(&rt.core, || spawn_promotion(first_time, &cluster, &peers));
        let raft_watch = on(&rt.core, || watch_raft(&cluster, exited_tx.clone()));
        // 풀 마운터가 QEMU의 rbd 블록 드라이버를 확인한다.
        let qemu_binary = host.profile.qemu_binary.clone();
        // 받은 UEFI 변수 파일의 크기를 이 노드의 템플릿과 견준다.
        let uefi_vars_template = host.profile.uefi_vars_template.clone();
        // 장치를 만들거나 지우면 곧바로 브리지 목록을 기록한다(아래 주기 갱신을 기다리지 않는다).
        let overlay = on(&rt.work, || {
            OverlayAgent::spawn(cluster.clone(), config, &host, tag)
        });
        let firewall = on(&rt.work, || firewall.map(FirewallAgent::spawn));
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            rt.work.spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_node(&cluster, &config, &host).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_bridges(&cluster, &host).await {
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다(시작, 스냅샷, 복제).
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
            uefi_vars_template,
        ));
        reconciler_config.firmware = Some(firmware.clone());
        // 펜싱이 켜져 있으면 지금부터 리스를 갱신한다(지금이 stale을 세는 기준점이다).
        let ha = config
            .ha
            .guard_config()
            .map(|ha| on(&rt.core, || HaGuard::spawn(cluster.clone(), ha)));
        reconciler_config.ha = ha.clone();
        reconciler_config.firewall = gate;
        let reconciler = on(&rt.work, || {
            Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config)
        });
        let pools = on(&rt.work, || {
            PoolMounter::spawn(
                cluster.clone(),
                mount_root.clone(),
                qemu_binary,
                config.pools.refresh(),
            )
        });
        // 지표는 메모리에만 둔다(Raft에 쓰지 않는다).
        let metrics = on(&rt.work, || {
            MetricsSampler::spawn(
                hv.clone(),
                metrics_host::native(),
                cluster.clone(),
                config.data_dir.clone(),
                metrics_store,
            )
        });
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        // QEMU 드라이버의 state_dir과 같다(host.rs).
        runner_config.state_dir = Some(config.data_dir.clone());
        runner_config.mount_root = mount_root.clone();
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
        runner_config.peers = Some(peers.clone());
        runner_config.firmware = Some(firmware);
        runner_config.migration_bandwidth =
            (config.migration.bandwidth_mib > 0).then_some(config.migration.bandwidth_mib << 20);
        runner_config.migration_timeout = config.migration.timeout;
        runner_config.migration_disk_timeout = config.migration.disk_timeout;
        // 받는 쪽(IncomingConfig)과 같은 디렉터리다.
        runner_config.migration_disks = Some(config.data_dir.join(MIGRATION_DISKS_DIR));
        // ESXi 가져오기의 자격 증명은 API가 받아 두고 가져오기 Task가 쓴다.
        runner_config.import_credentials = state.import_credentials.clone();
        // 노드 비우기 Task가 이 노드의 API와 같은 확인으로 마이그레이션 Task를 만든다.
        let state = Arc::new(state);
        runner_config.api = Some(state.clone());
        // 업데이트 확인은 주기 에이전트와 다시 확인 Task가 함께 쓴다(한 번에 하나만 돈다).
        let updates = config
            .updates
            .updates_config()
            .map(|c| Arc::new(Updates::new(c)));
        runner_config.updates = updates.clone();
        // Task 실행기의 고아 정리는 쓰기마다 10초까지 기다리므로 정족수 없이 다시 켤 때 systemd의 시작
        // 제한(90초)을 넘길 수 있다. 정리는 API보다 먼저 끝나야 하므로(API가 막 받는 업로드 파일을
        // 고아로 지우지 않게) 뒤로 미루지 않고 READY=1을 그 전에 보낸다(API가 뜬 뒤 다시 보낸다).
        Notifier::from_env().notify("READY=1");
        let runner = run_on(
            &rt.work,
            TaskRunner::spawn(
                cluster.clone(),
                storage,
                hv,
                reconciler.handle(),
                runner_config,
            ),
        )
        .await;
        let monitor = on(&rt.core, || {
            NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default())
        });
        let updates = on(&rt.work, || {
            updates.map(|u| UpdateAgent::spawn(cluster.clone(), node, u))
        });

        // TLS 핸드셰이크 작업이 `api`에 생긴다.
        let listener = on(&rt.api, || TlsListener::new(api_listener, api_tls))?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "orbivisord is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let (schedule, janitor, backups, telemetry, remotes) = on(&rt.work, || {
            (
                SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config()),
                ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config()),
                BackupJobScheduler::spawn(state.clone(), config.backup.scheduler_config()),
                TelemetryReporter::spawn(state.clone(), telemetry_config),
                RemotePoller::spawn(state.clone(), config.remotes.poller_config()),
            )
        });
        let recovery = on(&rt.core, || {
            HaManager::spawn(state.clone(), HaManagerConfig::default())
        });
        let audit = state.audit.clone();
        let app = opl_api::router(state).merge(crate::web::router());
        let task = rt.api.spawn(async move {
            // 연결 주소(클라이언트 IP)를 시도 제한에 넘긴다. axum은 `TapIo`로 감싼 리스너에
            // `ConnectInfo<SocketAddr>`를 만들어 주므로 아무 일도 하지 않는 tap을 붙인다.
            let listener = listener.tap_io(|_| {});
            let r = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
            let _ = exited_tx.send(format!("API server stopped: {r:?}")).await;
            r
        });
        let member = Member {
            config: config.clone(),
            cluster,
            peer,
            services: Some(Services {
                api: Server { stop, task },
                reconciler,
                runner,
                monitor,
                schedule,
                janitor,
                backups,
                telemetry,
                remotes,
                audit,
                recovery,
                pools,
                metrics,
                overlay,
                firewall,
                ha,
                updates,
            }),
            register,
            promotion,
            raft_watch,
            exited,
        };
        Ok((member, created))
    }

    /// 위트니스로 시작한다. 노드 간 서버(하이퍼바이저 없음)와 승격, Node 등록만 띄우고 QEMU를 찾지
    /// 않는다. 위트니스는 클러스터를 만들 수 없다(`Init`은 main이 먼저 거부한다).
    async fn start_witness(
        config: &Config,
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        tls: NodeTls,
        bootstrap: Bootstrap,
        rt: &Handles,
    ) -> anyhow::Result<Self> {
        let peer_listener = match TcpListener::bind(config.cluster_listen)
            .await
            .with_context(|| format!("cannot listen on {}", config.cluster_listen))
        {
            Ok(listener) => listener,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        // 마이그레이션과 UEFI 변수는 받지 않는다.
        let service = PeerService::new(cluster.clone(), None);
        let peer = on(&rt.core, || {
            serve_peer(peer_listener, &tls, service, exited_tx.clone())
        });
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { .. } => anyhow::bail!(WITNESS_CANNOT_INIT),
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_witness(&cluster, config).await?;
                }
            }
            anyhow::Ok(())
        };
        if let Err(e) = prepared.await {
            let _ = peer.stop.send(());
            let _ = peer.task.await;
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        let promotion = on(&rt.core, || spawn_promotion(first_time, &cluster, &peers));
        let raft_watch = on(&rt.core, || watch_raft(&cluster, exited_tx));
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            rt.work.spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
        };
        tracing::info!(
            node = %cluster.id(),
            cluster = %config.cluster_listen,
            "orbivisord is ready as a witness"
        );
        Ok(Member {
            config: config.clone(),
            cluster,
            peer,
            services: None,
            register,
            promotion,
            raft_watch,
            exited,
        })
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// 시험 고리가 쥐는 클러스터 저장소(약한 참조라 닫힌 저장소를 붙잡지 않는다)
    #[cfg(feature = "starve-it")]
    pub fn cluster_weak(&self) -> std::sync::Weak<Cluster> {
        Arc::downgrade(&self.cluster)
    }

    /// API 서버나 노드 간 서버가 스스로 멈추거나 Raft가 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나, 남는 투표 멤버가 없거나 모두 위트니스면
    /// 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let (_, nodes) = self.cluster.list::<NodeSpec>()?;
        check_leave(me, &self.cluster.members().voters, &nodes)?;
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        if let Some(vm) = vms
            .iter()
            .find(|vm| vm.spec.node == Some(me.0) || vm.status.node == Some(me.0))
        {
            anyhow::bail!(
                "VM {:?} is still assigned to this node; delete it or move it to another node first",
                vm.name
            );
        }
        // 멤버십에서 빠지면 다른 노드가 이 노드의 요청을 받지 않으므로 Node 리소스부터 지운다.
        match self.cluster.delete::<NodeSpec>(me.0, None).await {
            Ok(_) | Err(ClusterError::Rejected(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.cluster
            .request(LeaderRequest::Remove { id: me })
            .await?;
        Ok(())
    }

    /// 모든 것을 멈춘다. `left`면 클러스터에서 빠진 것이므로 클러스터 저장소와 노드 인증서를 지운다.
    /// 아니면 오프라인으로 기록한다. 실행 중인 VM은 그대로 둔다.
    pub async fn close(mut self, left: bool) -> anyhow::Result<()> {
        // 아래에서 저장소를 닫아 Raft가 멈춰도 실패로 알리지 않는다.
        self.raft_watch.abort();
        if let Some(services) = self.services.take() {
            services.shutdown(left).await;
        }
        self.register.abort();
        self.promotion.abort();
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        // 오래 가는 콘솔 중계 스트림이 있으면 서버가 끝나지 않으므로 기다리는 시간을 제한한다.
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.peer.task)
            .await
            .is_err()
        {
            self.peer.task.abort();
        }
        let shutdown = self.cluster.shutdown().await;
        if left {
            // 이미 클러스터에서 빠졌으므로 저장소를 닫지 못했어도 지운다. 남기면 빠진 멤버로 다시 뜬다.
            if let Err(e) = &shutdown {
                tracing::warn!(error = %e, "cannot shut down the cluster store; removing it anyway");
            }
            match std::fs::remove_dir_all(pki_dir(&self.config)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            }
            remove_cluster_data(&self.config)?;
        }
        shutdown?;
        Ok(())
    }
}

/// 이 노드가 빠진 뒤에도 리더가 될 수 있는 투표 멤버가 남는지 본다. 위트니스는 후보가 되지 않으므로
/// 위트니스만 남으면 클러스터에 리더가 영영 생기지 않는다. Node 리소스가 없는 투표 멤버는 일반 노드로
/// 본다(`get_cluster`처럼).
fn check_leave(
    me: NodeId,
    voters: &BTreeMap<NodeId, String>,
    nodes: &[Typed<NodeSpec>],
) -> anyhow::Result<()> {
    let mut rest = voters.keys().filter(|&&id| id != me).peekable();
    if rest.peek().is_none() {
        anyhow::bail!(
            "this node is the only voting member; stop orbivisord and remove its data directory instead"
        );
    }
    let witness = |id: &NodeId| nodes.iter().any(|n| n.id == id.0 && n.spec.witness);
    if rest.all(witness) {
        anyhow::bail!(
            "the remaining voting members are all witnesses, which cannot lead; add another \
             non-witness node first"
        );
    }
    Ok(())
}

/// 노드 간 서버를 띄운다. 서버가 스스로 멈추면 `exited`로 알린다.
fn serve_peer(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    exited: mpsc::Sender<String>,
) -> Server<opl_peer::ServeError> {
    let (stop, rx) = oneshot::channel::<()>();
    let tls = tls.clone();
    let task = tokio::spawn(async move {
        let r = opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await;
        let _ = exited.send(format!("cluster server stopped: {r:?}")).await;
        r
    });
    Server { stop, task }
}

/// Raft가 멈추면(저장소 쓰기 실패나 저장소 스레드 멈춤으로 openraft가 멈춘 경우) 그 이유를 `exited`로
/// 보낸다. 주 루프가 0이 아닌 코드로 끝나고 systemd(`Restart=on-failure`)가 다시 띄운다. 그러지 않으면
/// HA VM이 없는 노드는 API만 답하는 채로 남는다.
fn watch_raft(cluster: &Cluster, exited: mpsc::Sender<String>) -> JoinHandle<()> {
    let stopped = cluster.stopped();
    tokio::spawn(async move {
        let why = stopped.await;
        let _ = exited.send(format!("Raft stopped: {why}")).await;
    })
}

/// 다시 시작한 노드는 투표 멤버가 될 때까지 승격을 다시 요청한다. 처음 멤버가 된 노드는 이미 했다.
fn spawn_promotion(first_time: bool, cluster: &Arc<Cluster>, peers: &Arc<Peers>) -> JoinHandle<()> {
    if first_time {
        return tokio::spawn(async {});
    }
    let (cluster, peers) = (cluster.clone(), peers.clone());
    tokio::spawn(async move { promote_until_voter(&cluster, &peers).await })
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류하다 투표 멤버가 되기 전에 멈춘 노드는 learner로 남아 있다. 다시 시작하면 투표 멤버가 될
/// 때까지, 또는 클러스터에서 빠진 것을 알 때까지 승격을 다시 요청한다.
async fn promote_until_voter(cluster: &Cluster, peers: &Peers) {
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지 모른다.
    while cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .nodes()
                    .any(|(n, _)| *n == id)
            },
            "this node is in the membership",
        )
        .await
        .is_err()
    {
        tracing::info!("this node is not in the cluster membership it knows yet; still waiting");
    }
    let mut retry = PROMOTE_RETRY_MIN;
    let mut asked = false;
    loop {
        let members = cluster.members();
        if members.voters.contains_key(&id) {
            if asked {
                tracing::info!("this node became a voting member");
            } else {
                tracing::info!("this node is a voting member");
            }
            return;
        }
        if !members.learners.contains_key(&id) {
            tracing::info!("this node was removed from the cluster; not asking to be promoted");
            return;
        }
        tracing::info!("this node is not a voting member yet; asking to be promoted");
        asked = true;
        match promote(cluster, peers, None).await {
            Ok(()) => {}
            Err(e) if matches!(e.downcast_ref(), Some(ClusterError::Rejected(_))) => {
                tracing::info!(
                    error = format!("{e:#}"),
                    "this node was removed from the cluster; not asking to be promoted"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "cannot become a voting member; will retry"
                );
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(PROMOTE_RETRY_MAX);
            }
        }
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다. `via`가
/// 없으면(다시 시작한 노드) 이 노드가 아는 리더나 투표 멤버에게 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: Option<&str>) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut redirect: Option<String> = None;
    loop {
        // 다시 시작한 직후에는 리더를 아직 모를 수 있다. 알 때까지 기다린다.
        let Some(addr) = redirect
            .take()
            .or_else(|| via.map(str::to_string))
            .or_else(|| voter_addr(cluster))
        else {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("cannot become a voting member: no voting member is known");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => redirect = Some(leader),
            // 리더의 멤버십에 learner로 없다(클러스터에서 빠졌다). 다시 시도해도 소용없다.
            Err(e @ ClusterError::Rejected(_)) => {
                return Err(e).context("this node is not in the cluster membership");
            }
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::info!(error = %e, "waiting to become a voting member");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e).context("cannot become a voting member"),
        }
    }
    cluster
        .raft()
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .voter_ids()
                    .any(|v| v == id)
            },
            "this node is a voter",
        )
        .await?;
    Ok(())
}

/// 이 노드가 아는 리더의 주소, 모르면 아무 투표 멤버의 주소
fn voter_addr(cluster: &Cluster) -> Option<String> {
    let members = cluster.members();
    members
        .leader
        .and_then(|leader| members.voters.get(&leader))
        .or_else(|| members.voters.values().next())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtimes;
    use opl_types::resource::Resource;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// `dir` 아래에 데이터를 두는 단일 노드 멤버의 설정
    fn member_config(dir: &std::path::Path) -> Config {
        let mut config: Config = toml::from_str(&format!(
            "name = \"n1\"\naddress = \"127.0.0.1\"\nlisten = \"127.0.0.1:{}\"\n\
             cluster_listen = \"127.0.0.1:{}\"\n[hypervisor]\naccel = \"tcg\"\n\
             systemd_scope = false\n[ha]\nfencing = \"process\"\n[updates]\nenabled = false",
            free_port(),
            free_port()
        ))
        .unwrap();
        config.data_dir = dir.join("data");
        config.run_dir = dir.join("run");
        config
    }

    /// 새 클러스터를 만든 멤버의 서비스가 실행기 셋에 나뉘어 돈다. 부른 쪽의 실행기에는 아무 작업도
    /// 남지 않고(고르지 않고 띄운 서비스가 없다), 닫으면 세 실행기의 작업이 모두 끝난다.
    #[test]
    fn services_run_on_the_three_runtimes() {
        let dir = tempfile::tempdir().unwrap();
        let config = member_config(dir.path());
        let runtimes = Runtimes::new(2, 2).unwrap();
        let rt = runtimes.handles();
        let alive = |h: &tokio::runtime::Handle| h.metrics().num_alive_tasks();
        // 주 루프 대신 부르는 쪽
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        caller.block_on(async {
            let node = Uuid::now_v7();
            let (ca, tls) = crate::new_cluster_tls(&config, node, "c").unwrap();
            let bootstrap = Bootstrap::Init {
                name: "c".into(),
                ca,
            };
            let (member, created) = Member::start(&config, node, tls, bootstrap, &rt)
                .await
                .unwrap();
            assert!(created.is_some());
            let here = tokio::runtime::Handle::current();
            assert_eq!(alive(&here), 0, "tasks left on the caller's runtime");
            for (name, h) in [("core", &rt.core), ("api", &rt.api), ("work", &rt.work)] {
                assert!(alive(h) > 0, "no task on {name}");
            }
            // 빠진 것처럼 닫아 root로 도는 Linux CI에서 만든 방화벽 테이블도 지운다.
            member.close(true).await.unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while [&rt.core, &rt.api, &rt.work].iter().any(|h| alive(h) > 0) {
            assert!(
                std::time::Instant::now() < deadline,
                "tasks left after close: core {} api {} work {}",
                alive(&rt.core),
                alive(&rt.api),
                alive(&rt.work)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Raft가 멈추면 API와 노드 간 서버가 돌아도 `failed`가 그 이유와 함께 끝난다(여기서는 저장소를
    /// 바깥에서 닫는다. 저장소 쓰기가 panic해 openraft가 멈추는 경우는 opl-cluster 시험이 본다).
    #[test]
    fn failed_ends_when_raft_stops() {
        let dir = tempfile::tempdir().unwrap();
        let config = member_config(dir.path());
        let runtimes = Runtimes::new(2, 2).unwrap();
        let rt = runtimes.handles();
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        caller.block_on(async {
            let node = Uuid::now_v7();
            let (ca, tls) = crate::new_cluster_tls(&config, node, "c").unwrap();
            let bootstrap = Bootstrap::Init {
                name: "c".into(),
                ca,
            };
            let (mut member, _) = Member::start(&config, node, tls, bootstrap, &rt)
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(300), member.failed())
                    .await
                    .is_err()
            );
            member.cluster().shutdown().await.unwrap();
            let e = tokio::time::timeout(Duration::from_secs(10), member.failed())
                .await
                .unwrap();
            assert!(e.to_string().starts_with("Raft stopped: "), "{e}");
            let _ = member.close(true).await;
        });
    }

    #[test]
    fn a_witness_does_not_stand_for_election() {
        assert!(raft_config(false).enable_elect);
        assert!(!raft_config(true).enable_elect);
    }

    /// 남는 투표 멤버가 없거나 모두 위트니스면(리더가 될 수 없다) 빠지지 않는다.
    #[test]
    fn leaving_keeps_a_voter_that_can_lead() {
        let node = |witness: bool| {
            let mut node = Resource::new(
                "n",
                NodeSpec {
                    address: "h:8006".into(),
                    maintenance: false,
                    witness,
                },
            );
            node.id = Uuid::now_v7();
            node
        };
        let (a, b, w) = (node(false), node(false), node(true));
        let id = |n: &Typed<NodeSpec>| NodeId(n.id);
        let voters = |nodes: &[&Typed<NodeSpec>]| {
            nodes
                .iter()
                .map(|n| (id(n), String::new()))
                .collect::<BTreeMap<_, _>>()
        };
        let nodes = [a.clone(), b.clone(), w.clone()];

        // 혼자 남은 투표 멤버
        let only = check_leave(id(&a), &voters(&[&a]), &nodes).unwrap_err();
        assert!(only.to_string().contains("only voting member"), "{only}");
        // 위트니스만 남는다.
        let err = check_leave(id(&a), &voters(&[&a, &w]), &nodes).unwrap_err();
        assert!(err.to_string().contains("all witnesses"), "{err}");
        // 일반 노드가 남으면 누구든 빠질 수 있다. 위트니스도.
        check_leave(id(&a), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &w]), &nodes).unwrap();
        // Node 리소스가 없는 투표 멤버는 일반 노드로 본다(get_cluster처럼).
        check_leave(id(&a), &voters(&[&a, &b, &w]), &[a.clone(), w.clone()]).unwrap();
    }
}
