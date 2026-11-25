//! 개발·시험용 서버(계획 73 설계 §10.3): 임시 디렉터리에 노드 하나짜리 클러스터를 세운다 — 가짜 하이퍼바이저(QEMU 없이 VM이
//! "켜짐"까지 감), 파일 없는 메모리 스토리지, reconciler, Task 실행기, 기능 모두. 클러스터 CA로 서명한 HTTPS(127.0.0.1)로 API를
//! 내고, admin API 토큰을 만들어 연결 정보 한 줄(JSON `{"url","token","ca_pem",…}`)을 표준 출력에 쓴 뒤 Ctrl-C(또는 표준 입력이
//! 닫힐 때)까지 돈다. Terraform 공급자 수용 시험(`terraform/`)·웹 데모가 쓴다. 배포 빌드에는 없다.
//!
//! `cargo run -p orbivisord --features devserver --bin orbivisor-devserver -- --port 8006`

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::serve::ListenerExt;
use clap::Parser;
use openraft::ServerState;
use orbivisor_api::{AppState, TaskRunner, TaskRunnerConfig, hash_password};
use orbivisor_cluster::memnet::MemRouter;
use orbivisor_cluster::types::{NodeId, OrbNode};
use orbivisor_cluster::{Cluster, ClusterConfig};
use orbivisor_driver::{
    Capacity, DriverError, Hypervisor, MockHypervisor, Storage, VolumeInfo, VolumeSource,
};
use orbivisor_reconciler::{Reconciler, ReconcilerConfig, RestartBackoff};
use orbivisor_types::acme::AcmeAccountSpec;
use orbivisor_types::auth::{Role, UserSpec};
use orbivisor_types::cluster::ClusterSpec;
use orbivisor_types::node::{FEATURES, FirmwareCaps, HTTP01, NodeSpec, NodeStatus, PoolState};
use orbivisor_types::resource::Resource;
use orbivisor_types::runtime::ImageFormat;
use orbivisor_types::sdn::{SDN_NAME, SdnCentral, SdnPhase, SdnSpec, SdnStatus};
use orbivisor_types::storage::{SourceFormat, StoragePool, StoragePoolSpec};
use orbivisor_types::version::BUILD;
use orbivisor_types::vm::Arch;
use serde_json::{Value, json};
use uuid::Uuid;

/// 기다리는 한도(리더 뽑기)
const WAIT: Duration = Duration::from_secs(30);
/// NFS 풀을 마운트할 자리(쓰지 않는다)
const MOUNT_ROOT: &str = "/nonexistent/orbivisor-devserver-pools";

#[derive(Parser)]
#[command(
    about = "Single-node Orbivisor API for Terraform acceptance tests and demos (fake hypervisor and storage)"
)]
struct Args {
    /// 받을 포트(0이면 빈 포트)
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// 받을 주소(루프백만 — 다른 주소는 `--insecure-expose`가 있어야 한다)
    #[arg(long, default_value = "127.0.0.1")]
    address: std::net::IpAddr,
    /// 루프백이 아닌 주소에서도 받는다(같은 네트워크의 누구나 admin 로그인 화면에 닿는다)
    #[arg(long)]
    insecure_expose: bool,
    /// 상태 디렉터리(없으면 임시 디렉터리를 만들고 끝날 때 지운다)
    #[arg(long)]
    dir: Option<PathBuf>,
    /// 표준 입력이 닫히면 끝난다(시험 도구가 자식으로 띄울 때 — 부모가 죽어도 남지 않게)
    #[arg(long)]
    exit_on_stdin_eof: bool,
}

/// 파일 없는 스토리지: 볼륨 크기만 기억한다(만들기·지우기·늘리기·복사). 용량은 1 TiB로 알린다.
#[derive(Default)]
struct MemStorage {
    sizes: Mutex<HashMap<Uuid, u64>>,
}

impl MemStorage {
    fn put(&self, id: Uuid, size: u64) -> Result<PathBuf, DriverError> {
        let mut sizes = self.sizes.lock().unwrap();
        if sizes.contains_key(&id) {
            return Err(DriverError::VolumeExists(id));
        }
        sizes.insert(id, size);
        Ok(format!("/devserver/{id}").into())
    }

    fn size(&self, id: Uuid) -> Result<u64, DriverError> {
        self.sizes
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .ok_or(DriverError::VolumeNotFound(id))
    }
}

#[async_trait]
impl Storage for MemStorage {
    async fn create_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.put(id, size_bytes)
    }

    async fn import_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        src: &Path,
        _: SourceFormat,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let size = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
        self.put(id, size)
    }

    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        Ok(())
    }

    async fn delete_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.sizes.lock().unwrap().remove(&id);
        Ok(())
    }

    async fn activate_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    async fn volume_info(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        Ok(VolumeInfo {
            virtual_bytes: self.size(id)?,
            allocated_bytes: 0,
        })
    }

    async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
        Ok(Capacity {
            total_bytes: 1 << 40,
            available_bytes: 1 << 40,
        })
    }

    fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
        false
    }

    async fn snapshot_create(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Err(DriverError::InvalidSpec(
            "the devserver storage has no snapshots".into(),
        ))
    }

    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Err(DriverError::InvalidSpec(
            "the devserver storage has no snapshots".into(),
        ))
    }

    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        Ok(Vec::new())
    }

    async fn resize_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        self.size(id)?;
        self.sizes.lock().unwrap().insert(id, size_bytes);
        Ok(())
    }

    async fn volume_source(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        Ok(VolumeSource {
            path: format!("/devserver/{id}").into(),
            format,
            snapshot: tag.map(str::to_string),
            size_bytes: self.size(id)?,
            rbd: None,
        })
    }

    async fn release_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    async fn copy_volume(
        &self,
        _: &StoragePool,
        src_id: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePool,
        dst_id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        let size = self.size(src_id)?;
        self.put(dst_id, size).map(|_| ())
    }
}

fn reconciler_config(node: Uuid) -> ReconcilerConfig {
    ReconcilerConfig {
        node,
        mount_root: MOUNT_ROOT.into(),
        storage: Arc::new(MemStorage::default()),
        resync_interval: Duration::from_secs(3600),
        acpi_timeout: Duration::from_millis(300),
        retry_interval: Duration::from_millis(200),
        restart: RestartBackoff {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(2),
            max_failures: 3,
            window: Duration::from_secs(60),
        },
        guest_ping_interval: orbivisor_reconciler::GUEST_PING_INTERVAL,
        firmware: None,
        state_dir: None,
        firmware_sync_interval: orbivisor_reconciler::FIRMWARE_SYNC_INTERVAL,
        guest_info: None,
        guest_info_interval: orbivisor_reconciler::GUEST_INFO_INTERVAL,
        ha: None,
        firewall: None,
        replica_dir: "/nonexistent/orbivisor-devserver-replicas".into(),
        host_alloc: Arc::new(orbivisor_reconciler::HostAllocator::new(node, None)),
    }
}

fn n_status(cluster: &Cluster, node: Uuid) -> anyhow::Result<NodeStatus> {
    Ok(cluster
        .get::<NodeSpec>(node)?
        .ok_or_else(|| anyhow::anyhow!("node vanished"))?
        .status)
}

/// 이 노드(가짜 하이퍼바이저, aarch64 `virt`, 지금 빌드의 기능 모두와 HTTP-01 리스너 알림)
fn node_resource(id: Uuid) -> Resource<NodeSpec, NodeStatus> {
    let mut me = Resource::new(
        "dev1",
        NodeSpec {
            sdn: None,
            host_resources: None,
            address: "127.0.0.1:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    me.id = id;
    me.status = NodeStatus {
        online: true,
        cpus: 8,
        memory_mib: 16384,
        hypervisor: "mock".into(),
        arch: Some(Arch::Aarch64),
        version: Some(BUILD.into()),
        features: FEATURES
            .iter()
            .map(|f| f.to_string())
            .chain([HTTP01.to_string()])
            .collect(),
        firmware_caps: Some(FirmwareCaps {
            tpm: true,
            secure_boot: true,
        }),
        ..NodeStatus::default()
    };
    me
}

/// 루프백이 아닌 주소는 `--insecure-expose`가 있을 때만
fn check_address(address: std::net::IpAddr, insecure_expose: bool) -> anyhow::Result<()> {
    if !address.is_loopback() && !insecure_expose {
        anyhow::bail!(
            "--address {address} is not a loopback address; this development server gives admin access to anyone who \
             can reach it — pass --insecure-expose to listen there anyway"
        );
    }
    Ok(())
}

/// 클러스터 정보·admin·풀 `local`·`backup`(준비됨)·세운 것처럼 적은 SDN·ACME 계정 `dev`(CA에 등록하지 않은 채 — 인증서 리소스를 만들 수 있게)를 심는다.
async fn seed(
    cluster: &Cluster,
    node: Uuid,
    dir: &Path,
    password: &str,
) -> anyhow::Result<orbivisor_peer::pki::ClusterCa> {
    cluster.create(&node_resource(node)).await?;
    let ca = orbivisor_peer::pki::ClusterCa::generate("devserver")?;
    cluster
        .create(&Resource::new(
            "devserver",
            ClusterSpec {
                ca_cert_pem: ca.cert_pem.clone(),
                ca_key_pem: ca.key_pem.clone(),
            },
        ))
        .await?;
    cluster
        .create(&Resource::new(
            "admin",
            UserSpec {
                password_hash: hash_password(password.into())
                    .await
                    .map_err(|e| anyhow::anyhow!("cannot hash the password: {}", e.message))?,
                role: Role::Admin,
                session_epoch: 0,
                realm: None,
                subject: None,
                external_groups: vec![],
            },
        ))
        .await?;
    // 풀 둘: VM 디스크 `local`, 백업 저장소를 둘 수 있는 `backup`(dir, `backups`)
    let mut ready = n_status(cluster, node)?;
    for (name, backups) in [("local", false), ("backup", true)] {
        let path = dir.join("pools").join(name);
        std::fs::create_dir_all(&path)?;
        let spec: StoragePoolSpec = serde_json::from_value(
            json!({ "backend": { "kind": "dir", "path": path }, "backups": backups }),
        )?;
        let pool = Resource::new(name, spec);
        cluster.create(&pool).await?;
        ready.pools.insert(
            name.into(),
            PoolState {
                ready: true,
                pool_id: pool.id,
                ..PoolState::default()
            },
        );
    }
    cluster.update_status::<NodeSpec>(node, &ready).await?;
    // SDN을 세운 것처럼 적는다(OVN은 없다 — 리더 동기화를 띄우지 않아 아무것도 설정하지 않는다). SDN 네트워크·라우터·
    // 유동 IP·부하 분산기를 API로 만들 수 있다.
    let mut sdn = Resource::new(SDN_NAME, SdnSpec::default());
    sdn.status = SdnStatus {
        phase: SdnPhase::Ready,
        centrals: vec![SdnCentral {
            node,
            address: "127.0.0.1".parse()?,
        }],
        ..SdnStatus::default()
    };
    cluster.create(&sdn).await?;
    cluster
        .create(&Resource::new(
            "dev",
            AcmeAccountSpec {
                directory: "https://127.0.0.1:14000/dir".into(),
                email: None,
                agree_tos: true,
                eab: None,
                ca_bundle: None,
                key: Default::default(),
            },
        ))
        .await?;
    Ok(ca)
}

/// 로그인해 admin API 토큰을 만든다.
async fn admin_token(url: &str, ca_pem: &str, password: &str) -> anyhow::Result<String> {
    let http = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(ca_pem.as_bytes())?)
        .build()?;
    let resp = http
        .post(format!("{url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": password }))
        .send()
        .await?
        .error_for_status()?;
    let cookie = resp
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .ok_or_else(|| anyhow::anyhow!("login gave no session cookie"))?
        .to_string();
    let body: Value = http
        .post(format!("{url}/api/v1/tokens"))
        .header("cookie", cookie)
        .json(&json!({ "name": "devserver" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    body["secret"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("token reply has no secret"))
}

/// 끝날 때 지우는 임시 디렉터리
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            // ORBIVISOR_LOG(옛 이름 OPL_LOG도 — 경고). 없거나 풀리지 않으면 warn.
            orbivisor_types::legacy::env("LOG")
                .and_then(|v| tracing_subscriber::EnvFilter::try_new(v).ok())
                .unwrap_or_else(|| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let args = Args::parse();
    check_address(args.address, args.insecure_expose)?;
    // admin 비밀번호는 실행마다 무작위(연결 정보 줄에 찍는다 — 웹 데모에서 로그인)
    let password = Uuid::new_v4().simple().to_string();
    let (dir, _cleanup) = match args.dir {
        Some(d) => {
            std::fs::create_dir_all(&d)?;
            (d, None)
        }
        None => {
            let d = std::env::temp_dir()
                .join(format!("orbivisor-devserver-{}", Uuid::now_v7().simple()));
            std::fs::create_dir_all(&d)?;
            (d.clone(), Some(TempDir(d)))
        }
    };

    let router = MemRouter::default();
    let id = NodeId(Uuid::now_v7());
    let mut config = ClusterConfig::new(id, dir.join("cluster"));
    config.raft = openraft::Config {
        heartbeat_interval: 50,
        election_timeout_min: 300,
        election_timeout_max: 600,
        ..Default::default()
    };
    let cluster = Arc::new(Cluster::open(config, router.network(id)).await?);
    router.register(id, &cluster);
    cluster
        .initialize(BTreeMap::from([(id, OrbNode::single(id.to_string()))]))
        .await?;
    cluster
        .raft()
        .ok_or_else(|| anyhow::anyhow!("no raft"))?
        .wait(Some(WAIT))
        .metrics(|m| m.state == ServerState::Leader, "leader")
        .await?;
    let node = id.0;
    let ca = seed(&cluster, node, &dir, &password).await?;

    let hv: Arc<dyn Hypervisor> = Arc::new(MockHypervisor::default());
    let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config(node));
    let storage: Arc<dyn Storage> = Arc::new(MemStorage::default());
    let state = Arc::new(AppState::new(
        cluster.clone(),
        node,
        hv.clone(),
        dir.join("uploads"),
    ));
    let mut rc = TaskRunnerConfig::new(node);
    rc.poll_interval = Duration::from_millis(50);
    rc.upload_dir = Some(dir.join("uploads"));
    rc.state_dir = Some(dir.join("state"));
    rc.migration_disks = Some(dir.join("migration-disks"));
    rc.api = Some(state.clone());
    let runner = Box::pin(TaskRunner::spawn(
        cluster.clone(),
        storage,
        hv,
        reconciler.handle(),
        rc,
    ))
    .await;

    let (key_pem, csr) = orbivisor_peer::pki::node_key_and_csr()?;
    let tls = orbivisor_peer::pki::NodeTls {
        cert_pem: ca.sign_node(&csr, node, &[args.address.to_string(), "localhost".into()])?,
        ca_pem: ca.cert_pem.clone(),
        key_pem,
    };
    let tcp = tokio::net::TcpListener::bind((args.address, args.port)).await?;
    let addr = tcp.local_addr()?;
    let url = match addr {
        std::net::SocketAddr::V6(a) => format!("https://[{}]:{}", a.ip(), a.port()),
        std::net::SocketAddr::V4(a) => format!("https://{a}"),
    };
    // opld처럼 연결 주소를 넘긴다(시도 제한이 클라이언트 IP로 센다).
    let listener =
        orbivisor_peer::tls::TlsListener::new(tcp, orbivisor_peer::tls::api_server_config(&tls)?)?
            .tap_io(|_| {});
    let app = orbivisor_api::router(state.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    let token = admin_token(&url, &ca.cert_pem, &password).await?;
    println!(
        "{}",
        json!({
            "url": url,
            "token": token,
            "ca_pem": ca.cert_pem,
            "ca_fingerprint": orbivisor_peer::pki::fingerprint(&ca.cert_pem)?,
            "admin_password": password,
            "node": node,
            "dir": dir,
        })
    );
    // 시험 도구가 자식으로 띄우고 표준 입력을 닫으면(부모가 끝남) 따라 끝난다.
    let watch_stdin = args.exit_on_stdin_eof;
    let stdin_closed = async move {
        if !watch_stdin {
            return std::future::pending::<()>().await;
        }
        let _ = tokio::task::spawn_blocking(|| {
            use std::io::Read;
            let mut buf = [0u8; 64];
            while matches!(std::io::stdin().read(&mut buf), Ok(n) if n > 0) {}
        })
        .await;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = stdin_closed => {}
    }
    server.abort();
    runner.shutdown().await;
    drop(reconciler);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_without_the_flag() {
        assert!(check_address("127.0.0.1".parse().unwrap(), false).is_ok());
        assert!(check_address("::1".parse().unwrap(), false).is_ok());
        assert!(check_address("0.0.0.0".parse().unwrap(), false).is_err());
        assert!(check_address("192.168.0.5".parse().unwrap(), false).is_err());
        assert!(check_address("0.0.0.0".parse().unwrap(), true).is_ok());
    }
}
