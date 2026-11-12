//! 실제 gRPC(mTLS)로 이은 두 노드: 한 노드의 API로 다른 노드에 VM을 띄우고 콘솔을 중계한다.
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기(HA VM 포함), 공유 디스크의 잠금 바꾸기,
//! 로컬 디스크 복사(대상에 만들기, 볼륨 기록 옮기기, 원본 지우기, 실패 때 대상 사본 지우기, 되돌아오는 VM).
//! 노드 비우기: VM을 다른 노드로 옮기고(들어오던 VM도, 로컬 디스크 VM은 디스크를 복사해) 스냅샷이 있는 로컬
//! 디스크 VM은 남기며, 유지보수가 끝나면 멈춘다.
//! 감사 로그: 한 노드의 `GET /audit`가 두 노드의 항목을 합치고, 닿지 않는 노드를 알린다.
//! 지표: 한 노드의 지표 API가 다른 노드의 지표를 읽고, 지표를 모르는 노드와 닿지 않는 노드를 알린다.
//! OIDC: 한 노드에서 시작한 로그인을 다른 노드가 끝낸다(상태 쿠키의 키는 클러스터 CA 키에서).

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{
    AppState, AuditConfig, AuditLog, IncomingConfig, IncomingMigrations, TaskRunner,
    UefiVarsFetcher, hash_password,
};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::mock::MOCK_MIRROR_BYTES;
use opl_driver::{
    DriverError, Hypervisor, MigrateTarget, MirrorTarget, MockHypervisor, RuntimeStatus, Storage,
    uefi_vars_path,
};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, MetricsReply, MetricsSource, PeerService, Peers};
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{FirmwareFetcher, HaConfig, HaGuard, Reconciler};
use opl_types::auth::{Role, UserSpec};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample, VmSeries};
use opl_types::node::{FEATURES, Fencing, NodeHa, NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{Arch, DiskMove, PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use common::{
    ADMIN_PASSWORD, FakeStorage, MOUNT_ROOT, WAIT, login_at, reconciler_config, runner_config,
    vm_request,
};

/// 받는 노드가 마이그레이션을 받는 포트 범위. Mock 하이퍼바이저는 첫 포트를 연다.
const MIGRATION_PORTS: std::ops::RangeInclusive<u16> = 61000..=61009;

struct Node {
    id: Uuid,
    /// Node 리소스와 감사 로그의 노드 이름
    name: String,
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 볼륨 호출을 적는 저장소(reconciler, Task 실행기, 마이그레이션 받기가 함께 쓴다)
    storage: Arc<FakeStorage>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    /// 감사 로그를 끈 노드(opld의 `[audit] enabled = false`)
    audit_off: bool,
    /// 지표 소스가 없는 노드(지표를 모르는 옛 노드처럼)
    metrics_off: bool,
    /// 노드 간 서버를 멈춘다.
    peer_stop: Option<tokio::sync::oneshot::Sender<()>>,
    peer_server: Option<tokio::task::JoinHandle<()>>,
    base: String,
    /// reconciler의 HA guard. None이면 펜싱이 꺼진 노드다.
    ha: Option<Arc<HaGuard>>,
    reconciler: Option<Reconciler>,
    runner: Option<TaskRunner>,
    /// Task 실행기를 다시 띄울 때 쓰는 설정
    runner_config: Option<opl_api::TaskRunnerConfig>,
}

impl Node {
    async fn open(ca: &ClusterCa, root: &TempDir, name: &str) -> Node {
        let id = Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(NodeId(id), root.path().join(id.to_string()));
        // 테스트가 동시에 많이 돌면 첫 선거가 몇 초씩 걸린다.
        config.write_timeout = Duration::from_secs(30);
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        Node {
            id,
            name: name.into(),
            addr,
            cluster,
            peers,
            hv: Arc::new(MockHypervisor::default()),
            storage: Arc::new(FakeStorage::default()),
            tls,
            listener: Some(listener),
            audit_off: false,
            metrics_off: false,
            peer_stop: None,
            peer_server: None,
            base: String::new(),
            ha: None,
            reconciler: None,
            runner: None,
            runner_config: None,
        }
    }

    /// 노드 간 서버(마이그레이션 받기 포함), reconciler, Task 실행기, API 서버(HTTP)를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let storage: Arc<dyn Storage> = self.storage.clone();
        let work = root.path().join(format!("{}-work", self.id));
        let incoming = IncomingMigrations::new(
            self.cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node: self.id,
                mount_root: MOUNT_ROOT.into(),
                storage: storage.clone(),
                listen_host: "127.0.0.1".into(),
                ports: MIGRATION_PORTS,
                // Task 실행기(`runner_config`)와 같다.
                migration_disks: work.join("migration-disks"),
            },
        );
        // opld처럼 API와 노드 간 서버가 같은 감사 로그를 쓴다.
        let audit = Arc::new(if self.audit_off {
            AuditLog::disabled()
        } else {
            AuditLog::open(AuditConfig {
                dir: work.join("audit"),
                node: self.name.clone(),
                max_file_size: 1 << 20,
                max_files: 2,
            })
            .unwrap()
        });
        // opld처럼 API와 노드 간 서버가 같은 지표 저장소를 읽는다.
        let metrics: Option<Arc<dyn MetricsSource>> = (!self.metrics_off).then(|| {
            // 두 노드의 VM 점 시각이 엇갈려야 합친 뒤의 정렬이 보인다.
            let vm_times = if self.name == "node1" {
                vec![1_000, 3_000]
            } else {
                vec![2_000]
            };
            Arc::new(FakeMetrics {
                node: self.id,
                vm_times,
            }) as Arc<dyn MetricsSource>
        });
        let mut service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(work.join("state"))
            .with_audit(audit.clone());
        if let Some(metrics) = &metrics {
            service = service.with_metrics(metrics.clone());
        }
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        self.peer_stop = Some(stop);
        self.peer_server = Some(tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        }));
        // UEFI 변수 파일을 가진 노드에서 받아 온다(opld처럼 reconciler와 Task 실행기가 함께 쓴다).
        // 시험의 변수 파일은 모두 템플릿과 같은 6바이트다.
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(template(root, self), b"vars 0").unwrap();
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            self.cluster.clone(),
            self.peers.clone(),
            work.join("state"),
            template(root, self),
        ));
        let mut reconciler_config = reconciler_config(self.id);
        reconciler_config.storage = storage.clone();
        reconciler_config.firmware = Some(firmware.clone());
        reconciler_config.ha = self.ha.clone();
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut state = AppState::new(
            self.cluster.clone(),
            self.id,
            hv.clone(),
            work.join("uploads"),
        );
        state.peers = Some(self.peers.clone());
        state.audit = audit;
        state.metrics = metrics;
        let state = Arc::new(state);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        config.firmware = Some(firmware);
        // opld처럼 노드 비우기 Task가 API 상태를 쓴다.
        config.api = Some(state.clone());
        self.reconciler = Some(reconciler);
        self.runner_config = Some(config);
        self.start_runner().await;
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(state);
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
    }

    /// Task 실행기를 멈춘다(opld가 내려간 것처럼 실행 중인 Task도 멈춘다).
    async fn stop_runner(&mut self) {
        self.runner.take().unwrap().shutdown().await;
    }

    /// Task 실행기를 (다시) 띄운다. 시작할 때 남은 마이그레이션을 정리한다.
    async fn start_runner(&mut self) {
        self.runner = Some(
            TaskRunner::spawn(
                self.cluster.clone(),
                self.storage.clone(),
                self.hv.clone(),
                self.reconciler.as_ref().unwrap().handle(),
                self.runner_config.clone().unwrap(),
            )
            .await,
        );
    }
}

/// 두 노드를 띄워 클러스터를 만들고 Node 리소스와 관리자를 채운다. 노드 간 서버가 먼저 떠야
/// 선거가 끝나므로 서버부터 띄운다.
async fn two_nodes() -> (TempDir, Node, Node) {
    two_nodes_with(None, true, true).await
}

/// `ha`가 있으면 A의 reconciler에 넣고 두 노드를 HA 가능 노드(`process`)로 등록한다.
/// `b_audit`가 거짓이면 B는 감사 로그를 끈 노드, `b_metrics`가 거짓이면 지표 소스가 없는 노드다.
async fn two_nodes_with(
    ha: Option<Arc<HaGuard>>,
    b_audit: bool,
    b_metrics: bool,
) -> (TempDir, Node, Node) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root, "node1").await;
    let mut b = Node::open(&ca, &root, "node2").await;
    let node_ha = ha.as_ref().map(|_| NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 60,
    });
    a.ha = ha;
    b.audit_off = !b_audit;
    b.metrics_off = !b_metrics;
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    for node in [&a, &b] {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            node.name.clone(),
            NodeSpec {
                address: node.base.clone(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        r.id = node.id;
        r.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
            ha: node_ha,
            underlay: None,
            firewall: false,
            version: None,
            updates: None,
            // 노드가 등록할 때처럼 이 빌드의 기능을 적는다(노드 비우기).
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
        };
        // 리더가 뽑힐 때까지 쓰기가 기다린다.
        a.cluster.create(&r).await.unwrap();
    }
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
            role: Role::Admin,
            session_epoch: 0,
            realm: None,
            subject: None,
        },
    );
    a.cluster.create(&admin).await.unwrap();
    (root, a, b)
}

#[tokio::test]
async fn console_of_a_vm_on_another_node_is_relayed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    b.hv.set_console_dir(tmp.path());

    // 노드 A의 API로 노드 B에 VM을 만들고 켠다. Task는 B의 실행기가 끝낸다.
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(b.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = b
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert!(a.hv.inspect(vm.id).await.unwrap().is_none());

    let listener =
        tokio::net::UnixListener::bind(tmp.path().join(format!("{}.vnc", vm.id))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });
    let url = format!("{}/api/v1/vms/web/console", a.base).replacen("http", "ws", 1);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", admin.token.as_ref().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let greeting = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    let echo = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echo, Message::binary(b"ping".to_vec()));

    // A의 API로 만든 VM은 B에서도 곧 보인다(같은 저장소).
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let deadline = Instant::now() + WAIT;
    while admin_b.get("/vms/web").await.0 != StatusCode::OK {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 노드의 드라이버 `state_dir`(Task 실행기 설정과 노드 간 서버가 같은 곳을 쓴다)
fn state_dir(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path().join(format!("{}-work", node.id)).join("state")
}

/// 노드의 UEFI 변수 템플릿(받은 변수 파일의 크기를 이와 견준다)
fn template(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path()
        .join(format!("{}-work", node.id))
        .join("vars-template.fd")
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_the_node_that_has_them() {
    let (root, a, b) = two_nodes().await;
    let vm = Uuid::now_v7();
    let src = uefi_vars_path(&state_dir(&root, &b), vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"vars b").unwrap();
    let fetcher = UefiVarsFetcher::new(
        a.cluster.clone(),
        a.peers.clone(),
        state_dir(&root, &a),
        template(&root, &a),
    );
    let dest = uefi_vars_path(&state_dir(&root, &a), vm);

    fetcher.fetch(vm, b.id).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 크기가 이 노드의 템플릿과 다르면(다른 펌웨어 빌드) 받은 것을 버린다.
    std::fs::write(&src, b"other firmware").unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert!(err.0.contains("14 bytes"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 파일이 없거나 멤버가 아닌 노드면 실패하고 이 노드의 파일은 그대로다.
    let err = fetcher.fetch(Uuid::now_v7(), b.id).await.unwrap_err();
    assert!(err.0.contains("no UEFI variables"), "{err}");
    let err = fetcher.fetch(vm, Uuid::now_v7()).await.unwrap_err();
    assert!(err.0.contains("not a cluster member"), "{err}");

    // 오프라인으로 기록된 노드에서는 받지 않는다.
    std::fs::write(&src, b"newer").unwrap();
    let mut node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    node.status.online = false;
    a.cluster
        .update_status::<NodeSpec>(b.id, &node.status)
        .await
        .unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert_eq!(err.0, "node \"node2\" is offline");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");
}

/// 노드의 VM `vm` UEFI 변수 파일에 쓴다.
fn write_vars(root: &TempDir, node: &Node, vm: Uuid, data: &[u8]) {
    let path = uefi_vars_path(&state_dir(root, node), vm);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

fn read_vars(root: &TempDir, node: &Node, vm: Uuid) -> Vec<u8> {
    std::fs::read(uefi_vars_path(&state_dir(root, node), vm)).unwrap()
}

/// 멈춘 UEFI VM을 다른 노드로 옮기면, 그 노드의 스냅샷·복제·시작이 앞 노드의 변수를 받아 쓴다.
/// 되돌린 변수는 그 노드의 것이 된다(다시 받지 않는다).
#[tokio::test]
async fn variables_follow_a_stopped_vm_to_another_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    write_vars(&root, &a, vm.id, b"vars 1");

    // 멈춘 채 B로 옮긴다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = serde_json::json!(b.id);
    let (status, body) = admin
        .put("/vms/web", serde_json::json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 스냅샷은 B에서 찍고 A의 변수를 받아 담는다.
    let (status, task) = admin
        .post("/vms/web/snapshots", serde_json::json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    let s1 = b
        .cluster
        .list::<opl_types::snapshot::SnapshotSpec>()
        .unwrap()
        .1
        .into_iter()
        .find(|s| s.spec.vm == vm.id)
        .unwrap();
    let copy = state_dir(&root, &b).join(format!("vm/{}/snapshots/{}.vars", vm.id, s1.id));
    assert_eq!(std::fs::read(copy).unwrap(), b"vars 1");

    // 복제도 B에서 A의 변수를 받아 복사하고, 새 VM의 변수는 B에 있다.
    write_vars(&root, &a, vm.id, b"vars 2");
    let (status, task) = admin
        .post("/vms/web/clone", serde_json::json!({ "name": "web-2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = b.cluster.get_by_name::<VmSpec>("web-2").unwrap().unwrap();
    assert_eq!(read_vars(&root, &b, copy.id), b"vars 2");
    assert_eq!(copy.status.firmware_node, Some(b.id));

    // 켜면 B의 reconciler가 A의 변수를 받아 켜고 B를 변수 노드로 남긴다.
    write_vars(&root, &a, vm.id, b"vars 3");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 3");
    wait_vm(&b, vm.id, "variables on B", |v| {
        v.status.firmware_node == Some(b.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    // 되돌리기는 스냅샷 때의 변수를 B에 쓰고 B를 변수 노드로 삼는다. 다음 시작은 A에서 받지 않는다.
    let mut moved = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    moved.status.firmware_node = Some(a.id);
    b.cluster
        .update_status::<VmSpec>(vm.id, &moved.status)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", serde_json::Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let reverted = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(reverted.status.firmware_node, Some(b.id));
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 1");
}

/// A에서 도는 VM `web`(디스크 없음)을 만든다.
async fn running_vm(a: &Node, admin: &common::Client) -> opl_types::vm::Vm {
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap()
}

async fn migrate(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": node }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

async fn runtime_status(node: &Node, id: Uuid) -> Option<RuntimeStatus> {
    node.hv.inspect(id).await.unwrap().map(|s| s.status)
}

/// 두 노드에서 VM이 원하는 대로 보일 때까지 기다린다.
async fn wait_vm(node: &Node, id: Uuid, what: &str, pred: impl Fn(&opl_types::vm::Vm) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let vm = node.cluster.get::<VmSpec>(id).unwrap().unwrap();
        if pred(&vm) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {:?}", vm.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 확정 전에 실패한 마이그레이션 뒤: VM은 A에 남아 돌고, B에는 받는 QEMU가 없다.
async fn assert_still_on_a(a: &Node, b: &Node, id: Uuid) {
    let vm = a.cluster.get::<VmSpec>(id).unwrap().unwrap();
    assert_eq!(vm.spec.node, Some(a.id));
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(vm.status.node, Some(a.id));
    assert_eq!(vm.status.migration, None);
    assert_eq!(runtime_status(a, id).await, Some(RuntimeStatus::Running));
    assert_eq!(runtime_status(b, id).await, None);
    assert!(!b.hv.is_incoming(id));
}

#[tokio::test]
async fn running_vm_migrates_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    // B의 reconciler가 넘겨받으며 `firmware_node`를 쓰기 전에, 확정 뒤 기록이 B를 쓰는지 본다.
    let b_reconciler = b.reconciler.as_ref().unwrap().handle();
    b_reconciler.begin_migration(vm.id).await;

    // B의 API로 요청해도 Task는 원본 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = migrate(&admin_b, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], a.id.to_string());

    // A의 QEMU는 B의 클러스터 주소의 호스트, B가 연 포트로 보냈다.
    assert_eq!(
        a.hv.migrations(),
        vec![(
            vm.id,
            MigrateTarget {
                host: "127.0.0.1".into(),
                port: *MIGRATION_PORTS.start(),
                tls_hostname: "127.0.0.1".into(),
            }
        )]
    );
    let vm_now = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(vm_now.spec.node, Some(b.id));
    assert_eq!(vm_now.status.node, Some(b.id));
    assert_eq!(vm_now.status.migration, None);
    // QEMU가 변수를 B로 옮겼다.
    assert_eq!(vm_now.status.firmware_node, Some(b.id));
    assert_eq!(runtime_status(&a, vm.id).await, None);
    b_reconciler.end_migration(vm.id);
    // B의 reconciler가 받은 QEMU를 넘겨받아 이어서 실행한다(새로 띄우지 않는다).
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id) && v.status.pid.is_some()
    })
    .await;
    let deadline = Instant::now() + WAIT;
    while b.hv.is_incoming(vm.id) {
        assert!(Instant::now() < deadline, "B did not take over the VM");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    assert!(b.hv.launches().is_empty(), "{:?}", b.hv.launches());

    // 옮긴 VM은 B에서 다룬다: 끄면 B의 QEMU가 꺼진다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

#[tokio::test]
async fn failed_send_keeps_the_vm_running_on_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration(
        "Failed to connect to '127.0.0.1:61000': Connection refused".into(),
    ));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("Connection refused"), "{error}");
    assert_still_on_a(&a, &b, vm.id).await;
    // 다른 동작이 다시 된다.
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 원본은 다 보냈지만(`postmigrate`) 대상이 받기에 실패했다: 확정하지 않고 원본을 이어서 실행한다.
#[tokio::test]
async fn failed_receive_resumes_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("TLS handshake failed"), "{error}");
    assert_eq!(a.hv.migrations().len(), 1);
    assert_still_on_a(&a, &b, vm.id).await;
}

/// 정해 둔 대로 곧바로 갱신에 성공하거나 실패한다.
struct FakeRenewer(std::sync::Mutex<bool>);

#[async_trait::async_trait]
impl LeaseRenewer for FakeRenewer {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), opl_cluster::ClusterError> {
        if *self.0.lock().unwrap() {
            Ok(())
        } else {
            Err(opl_cluster::ClusterError::Timeout)
        }
    }
}

/// A에 가짜 리스의 HA guard(`process`, 리스는 `fence_after` 뒤에 끝난다)를 두고 HA VM `web`을 켠다.
async fn ha_vm_on_a(
    fence_after: Duration,
) -> (TempDir, Node, Node, Arc<FakeRenewer>, opl_types::vm::Vm) {
    let renewer = Arc::new(FakeRenewer(std::sync::Mutex::new(true)));
    let guard = HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after,
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    );
    let (root, a, b) = two_nodes_with(Some(guard), true, true).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["ha"] = serde_json::json!(true);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    wait_vm(&a, vm.id, "running", |v| v.status.phase == VmPhase::Running).await;
    (root, a, b, renewer, vm)
}

/// 되돌린 HA VM의 원본은 reconciler가 리스를 확인한 뒤 이어서 실행한다.
#[tokio::test]
async fn failed_receive_of_an_ha_vm_resumes_the_source_with_the_lease() {
    let (_root, a, b, _renewer, vm) = ha_vm_on_a(Duration::from_secs(60)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let deadline = Instant::now() + WAIT;
    while runtime_status(&a, vm.id).await != Some(RuntimeStatus::Running) {
        assert!(Instant::now() < deadline, "source not resumed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_still_on_a(&a, &b, vm.id).await;
    assert_eq!(a.hv.launches().len(), 1);
}

/// 마이그레이션하는 동안 리스가 끝났으면 되돌려도 원본을 이어서 실행하지 않고 끈다. 리스가 돌아오면
/// 다시 켠다.
#[tokio::test]
async fn failed_receive_of_an_ha_vm_without_the_lease_stops_the_source() {
    let (_root, a, b, renewer, vm) = ha_vm_on_a(Duration::from_millis(500)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    a.hv.delay_migrations(Duration::from_millis(1500));
    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    *renewer.0.lock().unwrap() = false;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    wait_vm(&a, vm.id, "fenced", |v| {
        v.status.last_error.as_deref() == Some("fenced: lost the cluster majority")
    })
    .await;
    assert_eq!(runtime_status(&a, vm.id).await, None);
    assert!(
        !a.hv.pause_calls(vm.id).contains(&"resume"),
        "{:?}",
        a.hv.pause_calls(vm.id)
    );

    *renewer.0.lock().unwrap() = true;
    wait_vm(&a, vm.id, "running again", |v| {
        v.status.phase == VmPhase::Running && v.status.last_error.is_none()
    })
    .await;
    assert_eq!(a.hv.launches().len(), 2);
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

/// 원본 QEMU가 보내는 중인데 기다리기가 실패했다: 되돌리며 그 마이그레이션을 취소한다. 나중에 끝나
/// 원본을 멈추지 않는다.
#[tokio::test]
async fn unfinished_send_is_cancelled_by_the_rollback() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate_unfinished(DriverError::Timeout("QMP query-migrate"));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!a.hv.is_outgoing(vm.id));
    a.hv.complete_outgoing(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_still_on_a(&a, &b, vm.id).await;
}

/// API 검사를 거치지 않고 Task를 만든다(검사와 마이그레이션 기록 사이에 들어온 요청).
async fn put_task(node: &Node, op: TaskOp, target: Uuid, on: Uuid) -> Uuid {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: on,
        },
    );
    task.name = task.id.to_string();
    node.cluster.create(&task).await.unwrap();
    task.id
}

/// Task가 끝날 때까지 기다린다.
async fn finished_task(node: &Node, id: Uuid) -> Task {
    let deadline = Instant::now() + WAIT;
    loop {
        let task = node.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        if task.status.state.is_finished() {
            return task;
        }
        assert!(Instant::now() < deadline, "task {id}: {:?}", task.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// API가 마이그레이션을 받은 뒤 같은 VM의 스냅샷·디스크 크기 Task가 들어왔다: 원본 Task가 기록한 뒤
/// 다시 보고 되돌린다.
#[tokio::test]
async fn task_started_alongside_stops_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    // 실행하는 노드가 없어 끝나지 않는다.
    let elsewhere = Uuid::now_v7();
    let others = [
        (
            TaskOp::SnapshotCreate {
                snapshot: Uuid::now_v7(),
            },
            vm.id,
            "snapshotted",
        ),
        (
            TaskOp::VolumeResize {
                size_bytes: 2 << 20,
            },
            volume,
            "resized",
        ),
    ];
    for (op, target, what) in others {
        let other = put_task(&a, op, target, elsewhere).await;
        let migrate = put_task(
            &a,
            TaskOp::VmMigrate {
                node: b.id,
                pool: None,
            },
            vm.id,
            a.id,
        )
        .await;
        let task = finished_task(&a, migrate).await;
        assert_eq!(task.status.state, TaskState::Failed, "{:?}", task.status);
        let error = task.status.error.unwrap();
        assert!(error.contains(what), "{error}");
        assert!(a.hv.migrations().is_empty());
        assert_still_on_a(&a, &b, vm.id).await;
        a.cluster.delete::<TaskSpec>(other, None).await.unwrap();
    }
}

/// 보내는 동안 VM을 껐다(검사와 기록 사이에 들어온 정지): 확정하지 않는다.
#[tokio::test]
async fn vm_stopped_during_the_migration_is_not_committed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    wait_vm(&a, vm.id, "migrating", |v| {
        v.status.phase == VmPhase::Migrating
    })
    .await;
    let current = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.power = PowerState::Stopped;
    a.cluster
        .update_spec(vm.id, None, current.labels, &spec)
        .await
        .unwrap();

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("stopped"), "{error}");
    let got = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(got.spec.node, Some(a.id));
    assert_eq!(runtime_status(&b, vm.id).await, None);
    // A의 reconciler가 원하는 대로 끈다.
    wait_vm(&a, vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped).await;
}

#[tokio::test]
async fn different_qemu_versions_do_not_migrate() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.set_qemu_version("9.0.0");
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("QEMU 9.0.0"), "{error}");
    assert!(a.hv.migrations().is_empty());
    assert_still_on_a(&a, &b, vm.id).await;
}

/// A에서 도는 VM `web`. 디스크는 공유 dir 풀 `nas`의 볼륨 `root`다. 볼륨 id를 함께 돌려준다.
async fn running_vm_with_disk(a: &Node, admin: &common::Client) -> (opl_types::vm::Vm, Uuid) {
    let pool: StoragePool = Resource::new(
        "nas",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/nonexistent/opl-nas".into(),
            },
            nodes: vec![],
            shared: true,
            backups: false,
        },
    );
    a.cluster.create(&pool).await.unwrap();
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "nas".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
        },
    );
    volume.status.node = Some(a.id);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["disks"] = serde_json::json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    (vm, volume.id)
}

/// 노드의 볼륨 `id`에 대한 공유·배타·끄기 호출(차례대로)
fn lock_calls(node: &Node, id: Uuid) -> Vec<&'static str> {
    node.storage
        .calls()
        .into_iter()
        .filter(|(op, v)| *v == id && ["share", "exclusive", "deactivate"].contains(op))
        .map(|(op, _)| op)
        .collect()
}

/// 공유 디스크(공유 LVM이면 LV 잠금): 원본은 보내기 전에 공유로 바꾸고 확정 뒤 끈다. 대상은 공유로
/// 켜서 받고, 넘겨받은 뒤 원본이 끌 때까지(일시 오류) 다시 시도해 배타로 바꾼다.
#[tokio::test]
async fn shared_disks_are_shared_during_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    assert!(lock_calls(&a, volume).is_empty());

    b.storage.hold_exclusives(1);
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(lock_calls(&a, volume), ["share", "deactivate"]);
    let deadline = Instant::now() + WAIT;
    while lock_calls(&b, volume).len() < 3 {
        assert!(
            Instant::now() < deadline,
            "B did not make the disk exclusive: {:?}",
            lock_calls(&b, volume)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(lock_calls(&b, volume), ["share", "exclusive", "exclusive"]);
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    // 한 번 배타로 바꾼 뒤에는 다시 하지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&b, volume).len(), 3);
}

/// 확정 전 실패: 대상은 받으려고 켠 디스크를 끄고, 원본은(보류가 풀린 reconciler가) 다시 배타로
/// 바꾼다.
#[tokio::test]
async fn failed_migration_makes_shared_disks_exclusive_again() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration("Connection refused".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_still_on_a(&a, &b, vm.id).await;
    let deadline = Instant::now() + WAIT;
    while lock_calls(&a, volume).len() < 2 {
        assert!(Instant::now() < deadline, "{:?}", lock_calls(&a, volume));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&a, volume), ["share", "exclusive"]);
    assert_eq!(lock_calls(&b, volume), ["share", "deactivate"]);
}

/// 노드마다 따로인 로컬 디스크의 크기
const LOCAL_SIZE: u64 = 1 << 20;

/// A에서 도는 VM `web`. 디스크는 로컬 디렉터리 풀 `local`(노드마다 따로 있는 같은 이름의 풀)의 볼륨
/// `root`(qcow2)이고 그 파일은 A에 있다. 볼륨 id를 함께 돌려준다.
async fn running_vm_with_local_disk(a: &Node, admin: &common::Client) -> (opl_types::vm::Vm, Uuid) {
    dir_pool(a, "local", false).await;
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: LOCAL_SIZE,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(a.id);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    a.storage.place(volume.id, LOCAL_SIZE);
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["disks"] = serde_json::json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    (vm, volume.id)
}

/// 노드의 마이그레이션 디스크 기록 파일(`runner_config`의 디렉터리)
fn disk_record(root: &TempDir, node: &Node, task: &serde_json::Value) -> std::path::PathBuf {
    root.path()
        .join(format!("{}-work", node.id))
        .join("migration-disks")
        .join(format!("{}.json", task["id"].as_str().unwrap()))
}

fn volume_of(a: &Node, id: Uuid) -> Volume {
    a.cluster.get::<VolumeSpec>(id).unwrap().unwrap()
}

/// 로컬 디스크는 B에 같은 id로 새로 만들어 미러로 복사하고(NBD 포트, 내보내기 `disk0`), 스위치오버에서
/// 미러를 끝낸다. 확정 뒤 볼륨 기록이 B로 옮겨지고 A의 원본 파일과 기록이 지워진다. 메모리 단계에
/// 들어가면 Task에 진행률이 보이고 끝나면 지운다.
#[tokio::test]
async fn local_disks_are_copied_to_the_other_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    let deadline = Instant::now() + WAIT;
    let progress = loop {
        let t = a.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        if let Some(p) = t
            .status
            .progress
            .clone()
            .filter(|p| p.stage == "migrating-memory")
        {
            break p;
        }
        assert!(Instant::now() < deadline, "no progress: {:?}", t.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(progress.done_bytes, MOCK_MIRROR_BYTES);
    assert_eq!(progress.total_bytes, MOCK_MIRROR_BYTES);
    // 복사하는 동안 VM 기록에 계획이 있다.
    let migration = a
        .cluster
        .get::<VmSpec>(vm.id)
        .unwrap()
        .unwrap()
        .status
        .migration
        .unwrap();
    assert_eq!(
        migration.disks,
        [DiskMove {
            volume,
            pool: "local".into(),
            format: ImageFormat::Qcow2,
        }]
    );
    assert!(disk_record(&root, &a, &task).exists());
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(task["status"].get("progress").is_none(), "{task}");

    assert_eq!(
        a.hv.mirrors(),
        [(
            vm.id,
            vec![MirrorTarget {
                disk: 0,
                host: "127.0.0.1".into(),
                port: MIGRATION_PORTS.start() + 1,
                export: "disk0".into(),
                tls_hostname: "127.0.0.1".into(),
                format: ImageFormat::Qcow2,
            }]
        )]
    );
    assert_eq!(a.hv.switchovers(), [vm.id]);
    let moved = volume_of(&a, volume);
    assert_eq!(moved.status.node, Some(b.id));
    assert_eq!(moved.spec.pool, "local");
    assert_eq!(moved.spec.format, ImageFormat::Qcow2);
    assert!(b.storage.has_file(volume));
    assert!(b.storage.deleted().is_empty());
    assert!(!a.storage.has_file(volume));
    assert_eq!(a.storage.deleted(), [volume]);
    assert!(!disk_record(&root, &a, &task).exists());
    // B의 reconciler가 넘겨받아(NBD 내보내기를 멈추고) 이어서 실행한다.
    let deadline = Instant::now() + WAIT;
    while b.hv.is_incoming(vm.id) {
        assert!(Instant::now() < deadline, "B did not take over the VM");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(b.hv.exports(vm.id).is_empty());
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
}

/// `pool`을 주면 로컬 디스크를 B의 그 풀에 만든다. 디스크를 끌 때는 그 노드가 실제로 가진 풀을 쓴다:
/// B는 계획의 풀, 확정 뒤 A는 원래 풀(볼륨 기록은 이미 B의 풀이다).
#[tokio::test]
async fn local_disks_go_to_the_given_pool() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;
    dir_pool(&a, "fast", false).await;
    let pool_of = |node: &Node| -> Vec<String> {
        let calls = node.storage.deactivations().into_iter();
        calls
            .filter(|(_, id)| *id == volume)
            .map(|(p, _)| p)
            .collect()
    };

    // 받기 확인이 실패하면 B는 받으려고 켠 사본을 계획의 풀에서 끈다.
    b.hv.fail_next_incoming_wait(DriverError::Migration("load of migration failed".into()));
    let (status, task) = admin
        .post(
            "/vms/web/migrate",
            serde_json::json!({ "node": "node2", "pool": "fast" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!pool_of(&b).is_empty());
    assert!(pool_of(&b).iter().all(|p| p == "fast"), "{:?}", pool_of(&b));
    assert!(pool_of(&a).is_empty());

    let (status, task) = admin
        .post(
            "/vms/web/migrate",
            serde_json::json!({ "node": "node2", "pool": "fast" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["pool"], "fast");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let moved = volume_of(&a, volume);
    assert_eq!(moved.spec.pool, "fast");
    assert_eq!(moved.status.node, Some(b.id));
    assert!(b.storage.has_file(volume));
    assert_eq!(a.storage.deleted(), [volume]);
    assert!(!pool_of(&a).is_empty());
    assert!(
        pool_of(&a).iter().all(|p| p == "local"),
        "{:?}",
        pool_of(&a)
    );
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
    })
    .await;
}

/// 다음 마이그레이션 하나를 실패하게 한다.
type Inject<'a> = Box<dyn Fn() + 'a>;

/// 복사하는 마이그레이션이 단계마다 실패한다: VM은 A에서 계속 돌고, 볼륨 기록은 그대로이며, B에
/// 만든 디스크는 지워진다(B에 이미 있던 같은 볼륨은 지우지 않는다). A의 기록 파일도 지워진다.
#[tokio::test]
async fn failed_disk_copies_are_deleted_on_the_target() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;
    let local = a
        .cluster
        .get_by_name::<StoragePoolSpec>("local")
        .unwrap()
        .unwrap();
    let set_space = |available: Option<u64>| {
        let cluster = a.cluster.clone();
        let (b_id, local_id) = (b.id, local.id);
        async move {
            let mut node = cluster.get::<NodeSpec>(b_id).unwrap().unwrap();
            match available {
                Some(available_bytes) => node.status.pools.insert(
                    "local".into(),
                    PoolState {
                        pool_id: local_id,
                        ready: true,
                        error: None,
                        total_bytes: 1 << 40,
                        available_bytes,
                    },
                ),
                None => node.status.pools.remove("local"),
            };
            cluster
                .update_status::<NodeSpec>(b_id, &node.status)
                .await
                .unwrap();
        }
    };

    // 공간: B가 알린 남은 공간보다 할당량이 크다. B를 부르지 않는다.
    a.storage.set_allocated(volume, LOCAL_SIZE);
    set_space(Some(1000)).await;
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "not enough space in storage pool \"local\" on node \"node2\": need 1048576, available 1000"
    );
    assert!(b.storage.deleted().is_empty());
    assert!(!disk_record(&root, &a, &task).exists());
    set_space(None).await;

    let failures: [(&str, Inject); 4] = [
        (
            "would change size on pool \"local\" (1048576 → 4194304 bytes)",
            Box::new(|| b.storage.round_creates(4 << 20)),
        ),
        (
            "NBD server closed the connection",
            Box::new(|| {
                a.hv.fail_next_mirror(DriverError::Migration(
                    "disk mirror opl-mirror-0 failed: NBD server closed the connection".into(),
                ))
            }),
        ),
        (
            "disk mirror opl-mirror-0 failed: Input/output error",
            Box::new(|| a.hv.fail_next_switchover("Input/output error")),
        ),
        (
            "load of migration failed",
            Box::new(|| {
                b.hv.fail_next_incoming_wait(DriverError::Migration(
                    "load of migration failed".into(),
                ))
            }),
        ),
    ];
    for (n, (why, inject)) in failures.iter().enumerate() {
        inject();
        let task = migrate(&admin, "node2").await;
        b.storage.round_creates(0);
        assert_eq!(task["status"]["state"], "failed", "{task}");
        let error = task["status"]["error"].as_str().unwrap();
        assert!(error.contains(why), "{error}");
        assert_still_on_a(&a, &b, vm.id).await;
        assert!(!b.storage.has_file(volume), "{why}");
        assert_eq!(b.storage.deleted().len(), n + 1, "{why}");
        assert!(a.storage.has_file(volume));
        assert!(a.storage.deleted().is_empty());
        assert!(!disk_record(&root, &a, &task).exists(), "{why}");
        assert!(!disk_record(&root, &b, &task).exists(), "{why}");
        let current = volume_of(&a, volume);
        assert_eq!(current.status.node, Some(a.id));
        assert_eq!(current.spec.pool, "local");
        assert!(a.hv.mirroring(vm.id).is_empty());
    }

    // B에 같은 볼륨이 이미 있다(공유인 풀을 로컬로 설정했다): 그 파일은 지우지 않는다.
    b.storage.place(volume, LOCAL_SIZE);
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.contains(
            "volume \"root\" already exists on node \"node2\" in pool \"local\"; is the pool really node-local?"
        ),
        "{error}"
    );
    assert!(b.storage.has_file(volume));
    assert_eq!(b.storage.deleted().len(), failures.len());
    assert_still_on_a(&a, &b, vm.id).await;
    // 다시 하면 된다.
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// A → B로 옮긴 뒤 A의 원본을 지우지 못했다(남은 사본과 기록). VM이 A로 되돌아오면 A가 받기 전에
/// 기록대로 남은 사본을 지우고 새로 만든다. B는 자기 사본을 지운다.
#[tokio::test]
async fn a_vm_coming_back_replaces_the_copy_left_on_its_old_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;

    a.storage.fail_deletes_unmounted(1);
    let first = migrate(&admin, "node2").await;
    assert_eq!(first["status"]["state"], "succeeded", "{first}");
    assert!(a.storage.has_file(volume));
    assert!(disk_record(&root, &a, &first).exists());
    assert_eq!(volume_of(&a, volume).status.node, Some(b.id));
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
    })
    .await;

    let back = migrate(&admin, "node1").await;
    assert_eq!(back["status"]["state"], "succeeded", "{back}");
    assert_eq!(back["spec"]["node"], b.id.to_string());
    assert_eq!(a.storage.deleted(), [volume]);
    assert!(a.storage.has_file(volume));
    assert!(!disk_record(&root, &a, &first).exists());
    assert!(!b.storage.has_file(volume));
    assert!(b.storage.deleted().contains(&volume));
    assert_eq!(volume_of(&a, volume).status.node, Some(a.id));
    wait_vm(&a, vm.id, "running on A", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(a.id)
    })
    .await;
}

/// 원본이 확정한 뒤 마무리 전에 A의 실행기가 멈췄다(opld가 내려감): B의 reconciler가 넘겨받으며
/// 볼륨 기록을 B로 고치고 표시를 지운다. A의 실행기가 다시 뜨면 정리가 남은 원본 QEMU를 끄고 기록
/// 파일대로 원본 볼륨을 지운다. B의 사본은 그대로다.
#[tokio::test]
async fn source_stopping_after_the_commit_is_finished_by_both_nodes() {
    let (root, mut a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;

    b.hv.hold_next_incoming_wait();
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    b.hv.incoming_wait_held().await;
    // 받기를 확인한 원본 Task의 확정 쓰기(`commit`)와 같다. 그 직후 마무리 전에 실행기가 멈춘다.
    let current = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.node = Some(b.id);
    a.cluster
        .update_spec(vm.id, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    a.stop_runner().await;
    b.hv.release_incoming_wait();

    wait_vm(&b, vm.id, "taken over by B", |v| {
        v.status.migration.is_none()
            && v.status.phase == VmPhase::Running
            && v.status.node == Some(b.id)
    })
    .await;
    let moved = volume_of(&a, volume);
    assert_eq!(moved.status.node, Some(b.id));
    assert_eq!(moved.spec.pool, "local");
    // A는 아직 원본을 지우지 않았다.
    assert!(a.storage.has_file(volume));
    assert!(disk_record(&root, &a, &task).exists());

    a.start_runner().await;
    until("source volume deleted", || !a.storage.has_file(volume)).await;
    assert_eq!(a.storage.deleted(), [volume]);
    assert!(!disk_record(&root, &a, &task).exists());
    assert_eq!(runtime_status(&a, vm.id).await, None);
    assert!(b.storage.has_file(volume));
    assert!(b.storage.deleted().is_empty());
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("interrupted"), "{error}");
}

/// 디스크를 복사하는 중에 A의 실행기가 다시 시작했다: Task는 끝나지 않은 것으로 실패하고, A의
/// reconciler가 남은 미러를 치우고 표시를 지우며 원본은 계속 돈다. B의 정리(시작할 때와 30분마다)가
/// 받는 QEMU를 끄고 기록 파일대로 만든 사본을 지운다. A의 원본 파일은 그대로다.
#[tokio::test]
async fn restarting_the_source_while_copying_rolls_the_migration_back() {
    let (root, mut a, mut b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;

    a.hv.hold_next_mirror();
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    a.hv.mirror_held().await;
    assert_eq!(a.hv.mirroring(vm.id), [0]);
    assert!(b.storage.has_file(volume));
    assert!(b.hv.is_incoming(vm.id));

    a.stop_runner().await;
    a.start_runner().await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("interrupted"), "{error}");
    wait_vm(&a, vm.id, "rolled back on A", |v| {
        v.status.migration.is_none() && v.status.phase == VmPhase::Running
    })
    .await;
    until("mirror cleared", || a.hv.mirroring(vm.id).is_empty()).await;
    assert_eq!(
        runtime_status(&a, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    let current = volume_of(&a, volume);
    assert_eq!(current.status.node, Some(a.id));
    assert!(a.storage.has_file(volume));
    assert!(a.storage.deleted().is_empty());

    // B의 정리는 B의 복제본을 읽는다: 되돌림이 B에 복제된 뒤 다시 시작한다(아니면 다음 정리는 30분 뒤).
    wait_vm(&b, vm.id, "rolled back on B", |v| {
        v.status.migration.is_none()
    })
    .await;
    b.stop_runner().await;
    b.start_runner().await;
    until("copy deleted on B", || !b.storage.has_file(volume)).await;
    assert_eq!(b.storage.deleted(), [volume]);
    assert!(!disk_record(&root, &b, &task).exists());
    assert!(!b.hv.is_incoming(vm.id));
    assert_eq!(runtime_status(&b, vm.id).await, None);
    assert!(a.storage.has_file(volume));
}

/// 복사하는 동안 다른 쪽이 Task를 끝냈으면(실패로 기록) 진행률 쓰기가 Task를 `running`으로 되살리지
/// 않는다.
#[tokio::test]
async fn progress_does_not_revive_a_finished_task() {
    let (_root, a, _b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    running_vm_with_local_disk(&a, &admin).await;

    a.hv.hold_next_mirror();
    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    a.hv.mirror_held().await;
    until("copy progress", || {
        let t = a.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        t.status
            .progress
            .is_some_and(|p| p.stage == "copying-disks")
    })
    .await;
    let failed = TaskStatus {
        state: TaskState::Failed,
        error: Some("ended elsewhere".into()),
        progress: None,
    };
    a.cluster
        .update_status::<TaskSpec>(id, &failed)
        .await
        .unwrap();
    a.hv.release_mirror();
    // 복사가 끝나 메모리 단계에 들어가도(보내기는 500ms 걸린다) 쓰지 않는다.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(400) {
        let t = a.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        assert_eq!(t.status, failed);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 디렉터리 풀 `pool`을 만든다. `shared`면 두 노드가 함께 쓰는 풀이다.
async fn dir_pool(a: &Node, pool: &str, shared: bool) {
    let pool: StoragePool = Resource::new(
        pool,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: format!("/nonexistent/opl-{pool}").into(),
            },
            nodes: vec![],
            shared,
            backups: false,
        },
    );
    a.cluster.create(&pool).await.unwrap();
}

/// 노드 `on`에서 도는 VM `name`. 디스크는 풀 `pool`의 볼륨 `{name}-root`다.
async fn running_vm_on_pool(
    a: &Node,
    admin: &common::Client,
    name: &str,
    pool: &str,
    on: Uuid,
) -> opl_types::vm::Vm {
    let mut volume: Volume = Resource::new(
        format!("{name}-root"),
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
        },
    );
    volume.status.node = Some(on);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    let mut req = vm_request(name);
    req["spec"]["node"] = serde_json::json!(on);
    req["spec"]["disks"] = serde_json::json!([{ "volume": volume.name, "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap()
}

/// 노드 `node`를 비우는 Task를 만든다.
async fn drain(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post(&format!("/nodes/{node}/drain"), serde_json::Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    task
}

fn node_of(a: &Node, id: Uuid) -> Option<Uuid> {
    a.cluster.get::<VmSpec>(id).unwrap().unwrap().spec.node
}

#[tokio::test]
async fn drain_moves_running_vms_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app1 = running_vm_on_pool(&a, &admin, "app1", "nas", a.id).await;
    let app2 = running_vm_on_pool(&a, &admin, "app2", "nas", a.id).await;

    // B의 API로 요청해도 비우기 Task는 비울 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = drain(&admin_b, "node1").await;
    assert_eq!(task["spec"]["node"], a.id.to_string());
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(node_of(&a, app1.id), Some(b.id));
    assert_eq!(node_of(&a, app2.id), Some(b.id));
    // 이름 순으로 하나씩 옮겼다.
    let moved: Vec<Uuid> = a.hv.migrations().into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, [app1.id, app2.id]);
    for vm in [&app1, &app2] {
        wait_vm(&b, vm.id, "running on B", |v| {
            v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
        })
        .await;
    }
    let node = a.cluster.get::<NodeSpec>(a.id).unwrap().unwrap();
    assert!(node.spec.maintenance);
}

/// 로컬 디스크 VM도 B의 같은 이름 풀로 디스크를 복사해 옮긴다(볼륨 기록이 B로 가고 A의 원본이
/// 지워진다). 스냅샷이 있는 로컬 디스크 VM은 이유와 함께 남아 A에서 계속 돈다.
#[tokio::test]
async fn drain_moves_vms_with_local_disks() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    dir_pool(&a, "local", false).await;
    let app = running_vm_on_pool(&a, &admin, "app", "nas", a.id).await;
    let db = running_vm_on_pool(&a, &admin, "db", "local", a.id).await;
    let snap = running_vm_on_pool(&a, &admin, "snap", "local", a.id).await;
    let mut volumes = Vec::new();
    for name in ["db-root", "snap-root"] {
        let volume = a.cluster.get_by_name::<VolumeSpec>(name).unwrap().unwrap();
        a.storage.place(volume.id, 1 << 20);
        volumes.push(volume.id);
    }
    let mut snapshot: opl_types::snapshot::Snapshot = Resource::new(
        "",
        opl_types::snapshot::SnapshotSpec {
            vm: snap.id,
            name: "s1".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: snap.spec.clone(),
        },
    );
    snapshot.name = snapshot.id.to_string();
    a.cluster.create(&snapshot).await.unwrap();

    let task = admin.wait_task(&drain(&admin, "node1").await).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "1 VM(s) were not moved: snap (disks on a local pool and snapshots)"
    );
    assert_eq!(node_of(&a, app.id), Some(b.id));
    assert_eq!(node_of(&a, db.id), Some(b.id));
    // 같은 이름의 풀로 옮겼다(`pool` 없음).
    let (_, tasks) = a.cluster.list::<TaskSpec>().unwrap();
    assert!(tasks.iter().any(|t| t.spec.target == db.id
        && t.spec.op
            == TaskOp::VmMigrate {
                node: b.id,
                pool: None
            }));
    let moved = volume_of(&a, volumes[0]);
    assert_eq!(moved.status.node, Some(b.id));
    assert_eq!(moved.spec.pool, "local");
    assert!(b.storage.has_file(volumes[0]));
    assert_eq!(a.storage.deleted(), [volumes[0]]);
    wait_vm(&b, db.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
    })
    .await;
    // 옮기지 못한 VM은 A에서 계속 돈다.
    let snap = a.cluster.get::<VmSpec>(snap.id).unwrap().unwrap();
    assert_eq!(snap.spec.node, Some(a.id));
    assert_eq!(snap.status.phase, VmPhase::Running);
    assert_eq!(
        runtime_status(&a, snap.id).await,
        Some(RuntimeStatus::Running)
    );
    assert!(a.storage.has_file(volumes[1]));
}

/// 비우기를 시작할 때 이 노드로 들어오던 VM도 들어온 뒤 옮긴다. 성공하면 노드가 비어 있다.
#[tokio::test]
async fn drain_also_moves_a_vm_that_was_coming_in() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app = running_vm_on_pool(&a, &admin, "app", "nas", b.id).await;

    b.hv.delay_migrations(Duration::from_millis(500));
    let (status, migrate) = admin
        .post("/vms/app/migrate", serde_json::json!({ "node": "node1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{migrate}");
    let task = drain(&admin, "node1").await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let migrate = admin.wait_task(&migrate).await;
    assert_eq!(migrate["status"]["state"], "succeeded", "{migrate}");
    // B → A로 들어온 뒤 비우기가 다시 B로 옮겼다.
    let moved: Vec<Uuid> = a.hv.migrations().into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, [app.id]);
    assert_eq!(node_of(&a, app.id), Some(b.id));
}

/// 옮기는 동안 유지보수를 끝내면 그 마이그레이션이 끝난 뒤 멈춘다.
#[tokio::test]
async fn ending_maintenance_stops_the_drain() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app1 = running_vm_on_pool(&a, &admin, "app1", "nas", a.id).await;
    let app2 = running_vm_on_pool(&a, &admin, "app2", "nas", a.id).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let task = drain(&admin, "node1").await;
    wait_vm(&a, app1.id, "migrating", |v| {
        v.status.phase == VmPhase::Migrating
    })
    .await;
    let (status, node) = admin.get("/nodes/node1").await;
    assert_eq!(status, StatusCode::OK, "{node}");
    let mut spec = node["spec"].clone();
    spec["maintenance"] = serde_json::json!(false);
    let (status, body) = admin
        .put(
            "/nodes/node1",
            serde_json::json!({ "spec": spec }),
            node["revision"].as_u64(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], "drain stopped: maintenance ended");
    assert_eq!(node_of(&a, app1.id), Some(b.id));
    assert_eq!(node_of(&a, app2.id), Some(a.id));
}

/// `found`가 참이 될 때까지 `GET /audit?{query}`를 다시 부른다(쓰기 스레드와 복제를 기다린다).
async fn audit_until(
    admin: &common::Client,
    query: &str,
    found: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let (status, body) = admin.get(&format!("/audit?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if found(&body) {
            return body;
        }
        assert!(start.elapsed() < WAIT, "audit: {body:#}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 응답 항목의 노드 이름
fn audit_nodes(body: &serde_json::Value) -> Vec<&str> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["node"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn audit_entries_of_both_nodes_are_merged() {
    let (_root, a, mut b) = two_nodes().await;
    // 로그인(세션과 토큰 만들기)이 각 노드의 감사 로그에 적힌다.
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let both = |body: &serde_json::Value| {
        let nodes = audit_nodes(body);
        nodes.contains(&"node1") && nodes.contains(&"node2")
    };
    let body = audit_until(&admin, "action=/tokens", both).await;
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["disabled"], serde_json::json!([]));
    assert_eq!(body["truncated"], false);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{body:#}");
    // 두 노드의 항목을 합쳐 새 것부터
    let times: Vec<u64> = entries
        .iter()
        .map(|e| e["time"].as_u64().unwrap())
        .collect();
    assert!(times[0] >= times[1], "{body:#}");
    // 노드로 거르면 그 노드만 묻는다. 한 노드가 limit을 넘기면 잘렸다고 알린다.
    let body = audit_until(&admin, "node=node2", |_| true).await;
    assert!(
        !audit_nodes(&body).is_empty() && audit_nodes(&body).iter().all(|n| *n == "node2"),
        "{body:#}"
    );
    let body = audit_until(&admin, "node=node2&limit=1", |_| true).await;
    assert_eq!(audit_nodes(&body), ["node2"]);
    assert_eq!(body["truncated"], true);

    // 위트니스는 API가 없어 묻지 않는다(닿지 않는 노드에도 넣지 않는다).
    let node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    let witness = NodeSpec {
        witness: true,
        ..node.spec.clone()
    };
    a.cluster
        .update_spec(b.id, None, node.labels.clone(), &witness)
        .await
        .unwrap();
    let body = audit_until(&admin, "", |body| !audit_nodes(body).contains(&"node2")).await;
    assert_eq!(body["unreachable"], serde_json::json!([]));
    a.cluster
        .update_spec(b.id, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    audit_until(&admin, "", |body| audit_nodes(body).contains(&"node2")).await;

    // B의 노드 간 서버를 멈추면 B는 닿지 않는 노드다. A의 항목은 그대로 온다.
    let _ = b.peer_stop.take().unwrap().send(());
    b.peer_server.take().unwrap().await.unwrap();
    let body = audit_until(&admin, "", |body| {
        body["unreachable"] != serde_json::json!([])
    })
    .await;
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    let nodes = audit_nodes(&body);
    assert!(
        !nodes.is_empty() && nodes.iter().all(|n| *n == "node1"),
        "{body:#}"
    );
}

#[tokio::test]
async fn a_node_with_the_audit_log_off_is_disabled_not_unreachable() {
    let (_root, a, _b) = two_nodes_with(None, false, true).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let body = audit_until(&admin, "", |body| {
        audit_nodes(body).contains(&"node1") && body["disabled"] != serde_json::json!([])
    })
    .await;
    assert_eq!(body["disabled"], serde_json::json!(["node2"]));
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert!(audit_nodes(&body).iter().all(|n| *n == "node1"), "{body:#}");
}

/// 노드 점은 `t` 1000·2000(CPU는 노드 id의 첫 바이트), VM 점은 어떤 VM이든 `vm_times`.
struct FakeMetrics {
    node: Uuid,
    vm_times: Vec<i64>,
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        let after = |t: &i64| q.since.is_none_or(|since| *t > since);
        match q.scope {
            MetricsScope::Nodes => MetricsReply {
                node: Some(NodeSeries {
                    node: self.node,
                    samples: [1_000, 2_000]
                        .into_iter()
                        .filter(after)
                        .map(|t| NodeSample {
                            t,
                            cpu: Some(f64::from(self.node.as_bytes()[0])),
                            mem_used_mib: None,
                            mem_total_mib: None,
                            disk_read_bps: None,
                            disk_write_bps: None,
                            net_rx_bps: None,
                            net_tx_bps: None,
                            fs_used_mib: None,
                            fs_total_mib: None,
                        })
                        .collect(),
                }),
                vms: vec![],
            },
            MetricsScope::Vm { id } => MetricsReply {
                node: None,
                vms: vec![VmSeries {
                    vm: id,
                    node: self.node,
                    samples: (self.vm_times.iter().copied())
                        .filter(after)
                        .map(|t| VmSample {
                            t,
                            cpu: None,
                            mem_rss_mib: None,
                            disk_read_bps: None,
                            disk_write_bps: None,
                            disk_read_iops: None,
                            disk_write_iops: None,
                            net_rx_bps: None,
                            net_tx_bps: None,
                        })
                        .collect(),
                }],
            },
        }
    }
}

/// 응답 노드 시리즈의 노드 id
fn metrics_nodes(body: &serde_json::Value) -> Vec<Uuid> {
    body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| serde_json::from_value(n["node"].clone()).unwrap())
        .collect()
}

#[tokio::test]
async fn metrics_of_the_other_node_are_read_over_rpc() {
    let (_root, a, mut b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin.get("/metrics/nodes?since=1000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 노드 이름 순
    assert_eq!(metrics_nodes(&body), [a.id, b.id], "{body:#}");
    assert_eq!(body["nodes"][1]["samples"][0]["t"], 2_000);
    assert_eq!(
        body["nodes"][1]["samples"][0]["cpu"],
        f64::from(b.id.as_bytes()[0])
    );
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["unsupported"], serde_json::json!([]));

    let (status, body) = admin.get("/nodes/node2/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["node"], serde_json::json!(b.id));
    assert_eq!(body["samples"].as_array().unwrap().len(), 2);

    // VM 지표는 두 노드의 점(A 1000·3000, B 2000)을 시각 순으로 합친다.
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let (status, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["vm"], vm["id"]);
    let times: Vec<_> = body["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["t"].as_i64().unwrap())
        .collect();
    assert_eq!(times, [1_000, 2_000, 3_000]);
    let (_, body) = admin.get("/vms/web/metrics?since=3000").await;
    assert_eq!(body["samples"], serde_json::json!([]));

    // B의 노드 간 서버를 멈추면 B는 닿지 않는 노드다. A의 점은 그대로 온다.
    let _ = b.peer_stop.take().unwrap().send(());
    b.peer_server.take().unwrap().await.unwrap();
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [a.id]);
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    assert_eq!(body["unsupported"], serde_json::json!([]));
    let (status, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["samples"].as_array().unwrap().len(), 2);
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    let (status, body) = admin.get("/nodes/node2/metrics").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "node_unreachable");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("node \"node2\" did not answer: "),
        "{body}"
    );
}

/// B의 Node 리소스를 `name`으로 다시 만든다(이름을 바꾸는 명령이 없다). A가 볼 때까지 기다린다.
async fn rename_node(a: &Node, id: Uuid, name: &str) {
    let mut node = a.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    a.cluster.delete::<NodeSpec>(id, None).await.unwrap();
    node.name = name.into();
    a.cluster.create(&node).await.unwrap();
    until("renamed node", || {
        a.cluster
            .get::<NodeSpec>(id)
            .unwrap()
            .is_some_and(|n| n.name == name)
    })
    .await;
}

/// 노드 목록은 id가 아니라 이름 순이고, 위트니스로 표시한 구성원은 어디에도 넣지 않는다.
#[tokio::test]
async fn node_metrics_are_sorted_by_name_and_witnesses_are_left_out() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    // 나중에 만든 B(id가 더 큼)의 이름이 앞으로 오게 한다.
    assert!(a.id < b.id);
    rename_node(&a, b.id, "node0").await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [b.id, a.id], "{body:#}");

    // 위트니스는 VM을 돌리지 않아 묻지 않는다(Raft 구성원이어도 `nodes`·목록 어디에도 없다).
    let node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    let witness = NodeSpec {
        witness: true,
        ..node.spec.clone()
    };
    a.cluster
        .update_spec(b.id, None, node.labels.clone(), &witness)
        .await
        .unwrap();
    until("witness", || {
        a.cluster
            .get::<NodeSpec>(b.id)
            .unwrap()
            .is_some_and(|n| n.spec.witness)
    })
    .await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [a.id], "{body:#}");
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["unsupported"], serde_json::json!([]));
    admin.post("/vms", vm_request("web")).await;
    let (_, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(body["samples"].as_array().unwrap().len(), 2, "{body:#}");
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["unsupported"], serde_json::json!([]));
}

/// 꺼진 것으로 기록된 노드는 묻지 않고(연결 제한을 기다리지 않게) 닿지 않는 노드다. B의 서버는 떠 있으므로
/// 물었다면 B의 점이 왔을 것이다.
#[tokio::test]
async fn an_offline_node_is_unreachable_without_being_asked() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let mut node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    node.status.online = false;
    a.cluster
        .update_status::<NodeSpec>(b.id, &node.status)
        .await
        .unwrap();
    until("offline", || {
        a.cluster
            .get::<NodeSpec>(b.id)
            .unwrap()
            .is_some_and(|n| !n.status.online)
    })
    .await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [a.id], "{body:#}");
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    assert_eq!(body["unsupported"], serde_json::json!([]));
    admin.post("/vms", vm_request("web")).await;
    let (_, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(body["samples"].as_array().unwrap().len(), 2, "{body:#}");
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    let (status, body) = admin.get("/nodes/node2/metrics").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "node_unreachable");
    assert_eq!(body["message"], "node \"node2\" is offline");
}

#[tokio::test]
async fn a_node_without_metrics_is_unsupported_not_unreachable() {
    let (_root, a, _b) = two_nodes_with(None, true, false).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [a.id]);
    assert_eq!(body["unsupported"], serde_json::json!(["node2"]));
    assert_eq!(body["unreachable"], serde_json::json!([]));
    let (status, body) = admin.get("/nodes/node2/metrics").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert_eq!(
        body["message"],
        "node \"node2\" does not report metrics yet"
    );
}

/// `found`가 참이 될 때까지 기다린다(복제를 기다린다).
async fn until(what: &str, found: impl Fn() -> bool) {
    let start = Instant::now();
    while !found() {
        assert!(start.elapsed() < WAIT, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 한 노드에서 시작한 OIDC 로그인의 콜백이 다른 노드로 와도(같은 주소 뒤의 다른 노드) 끝난다.
#[tokio::test]
async fn an_oidc_login_started_on_one_node_finishes_on_the_other() {
    use opl_types::realm::RealmSpec;
    let (_root, a, b) = two_nodes().await;
    // opld가 클러스터를 만들 때처럼 클러스터 정보(CA 키)를 두고, 외부 사용자를 쓰도록 두 노드가 이 빌드를 돈다.
    common::seed_cluster_info(&a.cluster).await;
    for node in [&a, &b] {
        let mut status = a.cluster.get::<NodeSpec>(node.id).unwrap().unwrap().status;
        status.version = Some(opl_types::version::BUILD.into());
        a.cluster
            .update_status::<NodeSpec>(node.id, &status)
            .await
            .unwrap();
    }
    let idp = common::idp::Idp::start().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin
        .post(
            "/realms",
            serde_json::json!({
                "name": "okta",
                "backend": idp.backend(),
                "roles": { "groups": [{ "group": "opl-admins", "role": "admin" }] }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    until("B has the realm", || {
        b.cluster
            .get_by_name::<RealmSpec>("okta")
            .unwrap()
            .is_some()
    })
    .await;

    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let started = browser
        .get(format!("{}/api/v1/auth/oidc/okta/start", a.base))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::FOUND);
    let location = started.headers()["location"].to_str().unwrap().to_string();
    let cookie = started.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let callback = idp.follow(&location).await;
    assert!(callback.starts_with(&a.base), "{callback}");
    // A에서 시작했지만 B가 받는다. 토큰 교환에는 상태 쿠키의 리디렉트 URI(A의 주소)를 쓴다.
    let resp = browser
        .get(callback.replacen(&a.base, &b.base, 1))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()["location"], "/");
    let session = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .find(|c| c.starts_with("opl_session="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    until("B has the user", || {
        b.cluster
            .get_by_name::<UserSpec>("alice@okta")
            .unwrap()
            .is_some()
    })
    .await;
    let me: serde_json::Value = browser
        .get(format!("{}/api/v1/auth/whoami", b.base))
        .header("cookie", &session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["name"], "alice@okta", "{me}");
    assert_eq!(me["role"], "admin", "{me}");
}
