//! 실제 gRPC(mTLS)로 이은 두 노드: 한 노드의 API로 다른 노드에 VM을 띄우고 콘솔을 중계한다.
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기, 공유 디스크의 잠금 바꾸기.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{
    AppState, IncomingConfig, IncomingMigrations, TaskRunner, UefiVarsFetcher, hash_password,
};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{
    DriverError, Hypervisor, MigrateTarget, MockHypervisor, RuntimeStatus, Storage, uefi_vars_path,
};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_reconciler::{FirmwareFetcher, Reconciler};
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::{Arch, PowerState, VmPhase, VmSpec};
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
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 볼륨 호출을 적는 저장소(reconciler, Task 실행기, 마이그레이션 받기가 함께 쓴다)
    storage: Arc<FakeStorage>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    base: String,
    _workers: Option<(Reconciler, TaskRunner)>,
}

impl Node {
    async fn open(ca: &ClusterCa, root: &TempDir) -> Node {
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
            addr,
            cluster,
            peers,
            hv: Arc::new(MockHypervisor::default()),
            storage: Arc::new(FakeStorage::default()),
            tls,
            listener: Some(listener),
            base: String::new(),
            _workers: None,
        }
    }

    /// 노드 간 서버(마이그레이션 받기 포함), reconciler, Task 실행기, API 서버(HTTP)를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let storage: Arc<dyn Storage> = self.storage.clone();
        let incoming = IncomingMigrations::new(
            self.cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node: self.id,
                mount_root: MOUNT_ROOT.into(),
                storage: storage.clone(),
                listen_host: "127.0.0.1".into(),
                ports: MIGRATION_PORTS,
            },
        );
        let work = root.path().join(format!("{}-work", self.id));
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(work.join("state"));
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, std::future::pending())
                .await
                .unwrap();
        });
        let mut reconciler_config = reconciler_config(self.id);
        reconciler_config.storage = storage.clone();
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        let runner = TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            hv.clone(),
            reconciler.handle(),
            config,
        )
        .await;
        let mut state = AppState::new(self.cluster.clone(), self.id, hv, work.join("uploads"));
        state.peers = Some(self.peers.clone());
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(Arc::new(state));
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
        self._workers = Some((reconciler, runner));
    }
}

/// 두 노드를 띄워 클러스터를 만들고 Node 리소스와 관리자를 채운다. 노드 간 서버가 먼저 떠야
/// 선거가 끝나므로 서버부터 띄운다.
async fn two_nodes() -> (TempDir, Node, Node) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root).await;
    let mut b = Node::open(&ca, &root).await;
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    for (i, node) in [&a, &b].into_iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node{}", i + 1),
            NodeSpec {
                address: node.base.clone(),
                maintenance: false,
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

#[tokio::test]
async fn uefi_vars_are_fetched_from_the_node_that_has_them() {
    let (root, a, b) = two_nodes().await;
    let vm = Uuid::now_v7();
    let src = uefi_vars_path(&state_dir(&root, &b), vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"vars of b").unwrap();
    let fetcher = UefiVarsFetcher::new(a.cluster.clone(), a.peers.clone(), state_dir(&root, &a));
    let dest = uefi_vars_path(&state_dir(&root, &a), vm);

    fetcher.fetch(vm, b.id).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars of b");

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
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars of b");
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
    assert_eq!(runtime_status(&a, vm.id).await, None);
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
        let migrate = put_task(&a, TaskOp::VmMigrate { node: b.id }, vm.id, a.id).await;
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
