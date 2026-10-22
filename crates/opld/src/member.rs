//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{AppState, TaskRunner, TaskRunnerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError};
use opl_driver::{DirStorage, Hypervisor, QemuConfig, QemuHypervisor, Storage};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_qemu::HostProfile;
use opl_reconciler::{Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);

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
    /// 합류가 받아들여졌다. `via`(받아 준 노드의 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config() -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
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

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    register: JoinHandle<()>,
    /// 서버 태스크가 끝나면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 처음 멤버가 되는 경우(`Init`, `Join`)는 마지막에 노드 인증서를 저장한다.
    /// 인증서가 있으면 멤버로 보므로, 도중에 실패하면 이 노드는 계속 미초기화 상태다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        let id = NodeId(node);
        let peers = Arc::new(Peers::new(&tls));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config();
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let cluster = Arc::new(
            Cluster::open(cluster_config, GrpcNetwork::new(peers.clone()))
                .await?
                .with_forwarder(peers.clone()),
        );
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
        let hv: Arc<dyn Hypervisor> = match QemuHypervisor::new(QemuConfig {
            host: HostProfile::homebrew_hvf(&config.qemu_share_dir),
            run_dir: config.run_dir.clone(),
            state_dir: config.data_dir.clone(),
        })
        .await
        {
            Ok(hv) => Arc::new(hv),
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e.into());
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        let (stop, rx) = oneshot::channel::<()>();
        let service = PeerService::new(cluster.clone(), Some(hv.clone()));
        let peer_tls = tls.clone();
        let tx = exited_tx.clone();
        let task = tokio::spawn(async move {
            let r = opl_peer::serve(peer_listener, &peer_tls, service, async {
                let _ = rx.await;
            })
            .await;
            let _ = tx.send(format!("cluster server stopped: {r:?}")).await;
            r
        });
        let peer = Server { stop, task };

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password = Some(bootstrap::create_cluster(&cluster, config, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    promote(&cluster, &peers, &via).await?;
                    bootstrap::register_node(&cluster, config).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
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
            if first_time {
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
        let register = if first_time {
            tokio::spawn(async {})
        } else {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
                while let Err(e) = bootstrap::register_node(&cluster, &config).await {
                    tracing::warn!(error = %e, "cannot register this node; will retry");
                    tokio::time::sleep(REGISTER_RETRY).await;
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node);
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        let reconciler = Reconciler::spawn(cluster.clone(), hv, reconciler_config);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img));
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        let runner =
            TaskRunner::spawn(cluster.clone(), storage, reconciler.handle(), runner_config).await;
        let monitor = NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default());

        let listener = TlsListener::new(api_listener, api_tls)?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "opld is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let app = opl_api::router(Arc::new(state));
        let task = tokio::spawn(async move {
            let r = axum::serve(listener, app)
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
            api: Server { stop, task },
            reconciler,
            runner,
            monitor,
            register,
            exited,
        };
        Ok((member, created))
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// API 서버나 노드 간 서버가 스스로 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let members = self.cluster.members();
        if members.voters.len() == 1 && members.voters.contains_key(&me) {
            anyhow::bail!(
                "this node is the only voting member; stop opld and remove its data directory instead"
            );
        }
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
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.monitor.shutdown().await;
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        self.register.abort();
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        let _ = self.peer.task.await;
        self.cluster.shutdown().await?;
        if left {
            std::fs::remove_dir_all(pki_dir(&self.config))?;
            remove_cluster_data(&self.config)?;
        }
        Ok(())
    }
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: &str) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut addr = via.to_string();
    loop {
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => addr = leader,
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
