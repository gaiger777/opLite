//! 로컬 디스크 복제의 원본 쪽(계획 55): 실제 gRPC(mTLS)로 이은 두 노드에서 A의 VM을 B로 복제하는 `vm-replicate`
//! Task(처음 전체 → 증분, 비트맵 정리와 남기기, 받는 쪽 거절의 전체 재시도, 실패 기록), 지금 복제 API의 검사,
//! 리더의 주기 스케줄러, 복제본이 있는 노드로의 마이그레이션(따라잡기와 복제 방향 뒤집기). 하이퍼바이저는 Mock, 저장소는
//! 가짜다.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use opl_api::{
    AppState, HaManager, HaManagerConfig, IncomingConfig, IncomingMigrations, IncomingReplicas,
    ReplicaConfig, ReplicationScheduler, ReplicationSchedulerConfig, TaskRunner, hash_password,
};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::mock::MOCK_MIRROR_BYTES;
use opl_driver::{
    DriverError, Hypervisor, MOCK_INCREMENT_BYTES, MockHypervisor, ReplicaDiskTarget, Storage,
};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{
    GrpcNetwork, PeerService, Peers, ReplicaAbortRequest, ReplicaError, ReplicaFinishRequest,
    ReplicaPrepareRequest, ReplicaPrepareResponse, ReplicaReceiver,
};
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::replicas::{self, ReplicaState, Role};
use opl_reconciler::{HaConfig, HaGuard, Reconciler};
use opl_types::auth::{Role as UserRole, UserSpec};
use opl_types::backup::bitmap_name;
use opl_types::node::{
    FEATURES, Fencing, NODE_REPLICATION, NodeHa, NodeHotplug, NodeSpec, NodeStatus,
};
use opl_types::replication::{
    ReplicaStatus, ReplicaVolume, ReplicationState, ReplicationTarget, replica_bitmap,
};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::{Arch, Vm, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use uuid::Uuid;

use common::{
    ADMIN_PASSWORD, Client, FakeStorage, MOUNT_ROOT, WAIT, login_at, reconciler_config,
    runner_config, vm_request,
};

/// 받는 노드의 NBD 포트 범위. Mock 하이퍼바이저는 첫 포트를 연다.
const PORTS: std::ops::RangeInclusive<u16> = 61100..=61109;

/// 마이그레이션을 받는 포트 범위
const MIGRATION_PORTS: std::ops::RangeInclusive<u16> = 61110..=61119;

const SIZE: u64 = 1 << 20;

/// 받는 쪽 처리를 감싸 다음 끝내기의 응답을 잃게 할 수 있다(받는 쪽은 끝까지 하고, 원본은 연결이 끊긴 것처럼 받는다).
struct LossyReceiver {
    inner: IncomingReplicas,
    lose_next_finish: AtomicBool,
}

#[async_trait]
impl ReplicaReceiver for LossyReceiver {
    async fn prepare(
        &self,
        req: ReplicaPrepareRequest,
    ) -> Result<ReplicaPrepareResponse, ReplicaError> {
        self.inner.prepare(req).await
    }

    async fn finish(&self, req: ReplicaFinishRequest) -> Result<(), ReplicaError> {
        self.inner.finish(req).await?;
        if self.lose_next_finish.swap(false, Ordering::SeqCst) {
            return Err(ReplicaError::Unavailable("the reply was lost".into()));
        }
        Ok(())
    }

    async fn abort(&self, req: ReplicaAbortRequest) -> Result<(), ReplicaError> {
        self.inner.abort(req).await
    }
}

/// 리스 갱신이 늘 성공하는 HA guard 갱신기(리더의 리스 표는 시험이 따로 갱신한다)
struct AlwaysRenewed;

#[async_trait]
impl LeaseRenewer for AlwaysRenewed {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), opl_cluster::ClusterError> {
        Ok(())
    }
}

struct Node {
    id: Uuid,
    name: String,
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    storage: Arc<FakeStorage>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    receiver: Option<Arc<LossyReceiver>>,
    /// reconciler의 HA guard(가짜 갱신 — 늘 성공). None이면 HA VM을 켜지 않는다.
    ha: Option<Arc<HaGuard>>,
    base: String,
    /// 복제본 기록 디렉터리(받는 쪽·reconciler·Task 실행기가 함께 쓴다)
    replica_dir: PathBuf,
    state: Option<Arc<AppState>>,
    reconciler: Option<Reconciler>,
    runner: Option<TaskRunner>,
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
            receiver: None,
            ha: None,
            base: String::new(),
            replica_dir: root.path().join(format!("{id}-work")).join("replicas"),
            state: None,
            reconciler: None,
            runner: None,
        }
    }

    /// 노드 간 서버(복제 받기 포함), reconciler, Task 실행기, API 서버를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let storage: Arc<dyn Storage> = self.storage.clone();
        let work = root.path().join(format!("{}-work", self.id));
        let receiver = Arc::new(LossyReceiver {
            inner: IncomingReplicas::new(
                self.cluster.clone(),
                hv.clone(),
                ReplicaConfig {
                    node: self.id,
                    mount_root: MOUNT_ROOT.into(),
                    storage: storage.clone(),
                    listen_host: "127.0.0.1".into(),
                    ports: PORTS,
                    replica_dir: self.replica_dir.clone(),
                },
            ),
            lose_next_finish: AtomicBool::new(false),
        });
        self.receiver = Some(receiver.clone());
        // 복제본으로 옮기는 마이그레이션도 받는다(opld처럼 복제본 받기와 기록을 함께 쓴다).
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
        )
        .with_replicas(receiver.inner.clone());
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(work.join("state"))
            .with_replicas(receiver);
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, std::future::pending())
                .await
                .unwrap();
        });
        let mut rc = reconciler_config(self.id);
        rc.storage = storage.clone();
        rc.replica_dir = self.replica_dir.clone();
        rc.ha = self.ha.clone();
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), rc);
        let mut state = AppState::new(
            self.cluster.clone(),
            self.id,
            hv.clone(),
            work.join("uploads"),
        );
        state.peers = Some(self.peers.clone());
        let state = Arc::new(state);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        config.api = Some(state.clone());
        config.replica_dir = Some(self.replica_dir.clone());
        self.runner = Some(
            TaskRunner::spawn(
                self.cluster.clone(),
                storage,
                hv,
                reconciler.handle(),
                config,
            )
            .await,
        );
        self.reconciler = Some(reconciler);
        self.state = Some(state.clone());
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(state);
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
    }

    fn state(&self) -> Arc<AppState> {
        self.state.clone().unwrap()
    }

    fn record(&self, vm: Uuid) -> Option<replicas::ReplicaRecord> {
        replicas::read(&self.replica_dir, vm).unwrap()
    }
}

struct Setup {
    _root: TempDir,
    a: Node,
    b: Node,
    admin: Client,
    /// A에서 도는 VM `web`(B의 풀 `b`로 복제)
    vm: Uuid,
    /// 그 디스크(A의 로컬 풀 `a`의 `root`)
    volume: Uuid,
}

/// 두 노드 A(node1)·B(node2)를 띄우고, A만의 dir 풀 `a`에 볼륨 `root`를 둔 VM `web`을 A에서 켠다. VM은 B의 풀
/// `b`로 복제한다(`schedule`). `a_hotplug`면 A가 CPU·메모리 핫플러그를 할 수 있다고 알린다(실행 중 디스크 붙이기).
async fn setup_with(schedule: &str, a_hotplug: bool) -> Setup {
    setup_full(schedule, a_hotplug, false).await
}

/// [`setup_with`]와 같되 `ha`면 두 노드가 HA 가능 노드(`process` 펜싱, `recover_after` 1초)이고 VM이 HA VM이며, A가
/// 리더가 아닌 노드다(리더는 자기 노드를 복구하지 않으므로 죽일 노드를 팔로워로 둔다).
async fn setup_full(schedule: &str, a_hotplug: bool, ha: bool) -> Setup {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root, "node1").await;
    let mut b = Node::open(&ca, &root, "node2").await;
    if ha {
        for node in [&mut a, &mut b] {
            node.ha = Some(HaGuard::spawn(
                Arc::new(AlwaysRenewed),
                HaConfig {
                    fence_after: Duration::from_secs(60),
                    renew_interval: Duration::from_millis(20),
                    watchdog: None,
                },
            ));
        }
    }
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), OrbNode::single(&a.addr)),
        (NodeId(b.id), OrbNode::single(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    if ha {
        wait_until("a leader", || a.cluster.members().leader.is_some()).await;
        if a.cluster.members().leader == Some(NodeId(a.id)) {
            std::mem::swap(&mut a, &mut b);
        }
    }
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
        r.status.online = true;
        r.status.cpus = 4;
        r.status.memory_mib = 8192;
        r.status.hypervisor = "mock".into();
        r.status.arch = Some(Arch::Aarch64);
        r.status.features = FEATURES.iter().map(|f| f.to_string()).collect();
        if ha {
            r.status.ha = Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 1,
                fence_after_secs: None,
            });
        }
        if a_hotplug && node.id == a.id {
            r.status.hotplug = Some(NodeHotplug {
                cpu: true,
                memory: true,
            });
        }
        a.cluster.create(&r).await.unwrap();
    }
    let admin_user = Resource::new(
        "admin",
        UserSpec {
            password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
            role: UserRole::Admin,
            session_epoch: 0,
            realm: None,
            subject: None,
        },
    );
    a.cluster.create(&admin_user).await.unwrap();
    for (name, node) in [("a", a.id), ("b", b.id)] {
        let pool: StoragePool = Resource::new(
            name,
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: format!("/nonexistent/opl-{name}").into(),
                },
                nodes: vec![node],
                shared: false,
                backups: false,
            },
        );
        a.cluster.create(&pool).await.unwrap();
    }
    let volume = local_volume(&a, "root").await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let mut req = vm_request("web");
    req["spec"]["node"] = json!(a.id);
    req["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    req["spec"]["replication"] = json!([{ "node": b.id, "schedule": schedule, "pool": "b" }]);
    if ha {
        req["spec"]["ha"] = json!(true);
    }
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = body["id"].as_str().unwrap().parse().unwrap();
    Setup {
        _root: root,
        a,
        b,
        admin,
        vm,
        volume,
    }
}

async fn setup() -> Setup {
    setup_with("*/15", false).await
}

/// A의 풀 `a`에 Ready 볼륨(qcow2)을 만들고 그 파일을 A에 둔다.
async fn local_volume(a: &Node, name: &str) -> Uuid {
    let mut volume: Volume = Resource::new(
        name,
        VolumeSpec {
            pool: "a".into(),
            size_bytes: SIZE,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    volume.status.node = Some(a.id);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    a.storage.place(volume.id, SIZE);
    volume.id
}

impl Setup {
    fn vm(&self) -> Vm {
        self.a.cluster.get::<VmSpec>(self.vm).unwrap().unwrap()
    }

    /// B로의 복제 항목
    fn entry(&self) -> ReplicaStatus {
        let vm = self.vm();
        vm.status
            .replication
            .iter()
            .find(|r| r.node == self.b.id)
            .cloned()
            .unwrap_or_else(|| panic!("no replication entry: {:?}", vm.status.replication))
    }

    /// 지금 복제를 요청하고 Task가 끝날 때까지 기다린다.
    async fn replicate(&self) -> Value {
        let (status, task) = self.admin.post("/vms/web/replicate", json!({})).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        assert_eq!(task["spec"]["op"]["kind"], "vm-replicate", "{task}");
        assert_eq!(task["spec"]["node"], json!(self.a.id), "{task}");
        self.admin.wait_task(&task).await
    }

    /// A의 VM 디스크마다의 비트맵 이름
    fn bitmaps(&self) -> Vec<Vec<String>> {
        self.a
            .hv
            .bitmaps(self.vm)
            .into_iter()
            .map(|d| d.into_iter().map(|b| b.name).collect())
            .collect()
    }

    /// B로 보낸 마지막 `replicate_disks` 호출의 대상들
    fn last_targets(&self) -> Vec<ReplicaDiskTarget> {
        self.a.hv.replications().last().unwrap().targets.clone()
    }

    /// VM을 노드 `node`(이름)로 옮기고(`pool`이 있으면 그 풀로) Task가 끝날 때까지 기다린다.
    async fn migrate(&self, node: &str, pool: Option<&str>) -> Value {
        let mut body = json!({ "node": node });
        if let Some(pool) = pool {
            body["pool"] = json!(pool);
        }
        let (status, task) = self.admin.post("/vms/web/migrate", body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        self.admin.wait_task(&task).await
    }

    /// VM이 노드 `node`에서 이어서 실행될 때까지 기다린다(받은 QEMU를 그 노드의 reconciler가 넘겨받음).
    async fn wait_running_on(&self, node: &Node) {
        let id = self.vm;
        let hv = node.hv.clone();
        wait_until("the VM running on the target node", || {
            let vm = self.vm();
            vm.status.phase == VmPhase::Running
                && vm.status.node == Some(node.id)
                && vm.status.migration.is_none()
                && !hv.is_incoming(id)
        })
        .await;
    }

    fn unfinished_replications(&self) -> Vec<Task> {
        let (_, tasks) = self.a.cluster.list::<TaskSpec>().unwrap();
        tasks
            .into_iter()
            .filter(|t| {
                matches!(t.spec.op, TaskOp::VmReplicate { .. }) && !t.status.state.is_finished()
            })
            .collect()
    }
}

fn succeeded(task: &Value) -> Uuid {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    task["id"].as_str().unwrap().parse().unwrap()
}

fn failed(task: &Value) -> String {
    assert_eq!(task["status"]["state"], "failed", "{task}");
    task["status"]["error"].as_str().unwrap().to_string()
}

/// 조건이 될 때까지 기다린다.
async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 처음 회차는 디스크 전체를 B의 풀 `b`에 같은 id로 만든 복제본에 보내고, 끝나면 B로의 항목이 그 회차로 `ready`가
/// 된다. 다음 회차는 앞 회차의 비트맵 뒤에 바뀐 블록만 B의 겹층에 보내고(합침), 앞 비트맵을 지운다.
#[tokio::test]
async fn the_first_run_sends_the_disks_in_full_and_the_next_only_the_changes() {
    let s = setup().await;
    let first = succeeded(&s.replicate().await);
    let entry = s.entry();
    assert_eq!(entry.state, ReplicationState::Ready);
    assert_eq!(entry.generation, Some(first));
    assert!(entry.last_full);
    assert_eq!(entry.last_bytes, MOCK_MIRROR_BYTES);
    assert!(entry.synced_at.is_some() && entry.last_try.is_some());
    assert_eq!(entry.error, None);
    assert_eq!(entry.volumes.len(), 1);
    assert_eq!(
        (entry.volumes[0].id, entry.volumes[0].size_bytes),
        (s.volume, SIZE)
    );
    assert_eq!(
        s.last_targets(),
        [ReplicaDiskTarget {
            disk: 0,
            host: "127.0.0.1".into(),
            port: *PORTS.start(),
            export: s.volume.simple().to_string(),
            tls_hostname: "127.0.0.1".into(),
            base: None,
        }]
    );
    assert_eq!(
        s.a.hv.replications()[0].bitmap,
        replica_bitmap(s.b.id, first)
    );
    assert_eq!(s.bitmaps(), [[replica_bitmap(s.b.id, first)]]);
    // B: 같은 id의 복제본과 그 회차의 기록. A: 원본 기록.
    assert!(s.b.storage.has_file(s.volume));
    let record = s.b.record(s.vm).unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.generation, Some(first));
    assert_eq!(record.synced_at, entry.synced_at);
    assert_eq!(record.volumes[0].pool, "b");
    assert_eq!(record.volumes[0].state, ReplicaState::Ready);
    let primary = s.a.record(s.vm).unwrap();
    assert_eq!(primary.role, Role::Primary);
    assert_eq!(primary.volumes[0].id, s.volume);
    assert_eq!(primary.volumes[0].pool, "a");
    assert!(s.b.hv.receivers().is_empty());

    let second = succeeded(&s.replicate().await);
    let entry = s.entry();
    assert_eq!(entry.generation, Some(second));
    assert!(!entry.last_full);
    assert_eq!(entry.last_bytes, MOCK_INCREMENT_BYTES);
    assert_eq!(
        s.last_targets()[0].base,
        Some(replica_bitmap(s.b.id, first))
    );
    assert_eq!(s.bitmaps(), [[replica_bitmap(s.b.id, second)]]);
    assert_eq!(
        s.b.storage.overlays(),
        [("create", s.volume), ("commit", s.volume)]
    );
    assert_eq!(s.b.record(s.vm).unwrap().generation, Some(second));
}

/// 앞 회차의 비트맵이 없는 디스크(VM을 다시 켜 사라진 raw 비트맵 등)는 전체로 보낸다.
#[tokio::test]
async fn a_disk_without_its_bitmap_is_sent_in_full() {
    let s = setup().await;
    succeeded(&s.replicate().await);
    s.a.hv.set_bitmaps(s.vm, vec![vec![]]);
    let second = succeeded(&s.replicate().await);
    assert_eq!(s.last_targets()[0].base, None);
    let entry = s.entry();
    assert!(entry.last_full);
    assert_eq!(entry.generation, Some(second));
    // 전체 회차는 복제본에 바로 쓴다(겹층 없음).
    assert!(s.b.storage.overlays().is_empty());
}

/// 실행 중에 붙인 디스크는 그 디스크만 전체로 보내고, B는 그 볼륨의 복제본을 새로 만든다.
#[tokio::test]
async fn a_hotplugged_disk_alone_is_sent_in_full() {
    let s = setup_with("*/15", true).await;
    let first = succeeded(&s.replicate().await);
    let data = local_volume(&s.a, "data").await;
    loop {
        let vm = s.vm();
        let mut spec = vm.spec.clone();
        spec.disks.push(
            serde_json::from_value(json!({ "volume": "data", "bus": "virtio-blk" })).unwrap(),
        );
        if s.a
            .cluster
            .update_spec(vm.id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
            .is_ok()
        {
            break;
        }
    }
    let task = s.admin.act("web", "hotplug").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let second = succeeded(&s.replicate().await);
    let targets = s.last_targets();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].base, Some(replica_bitmap(s.b.id, first)));
    assert_eq!((targets[1].disk, targets[1].base.clone()), (1, None));
    assert_eq!(targets[1].export, data.simple().to_string());
    assert!(s.b.storage.has_file(data));
    let entry = s.entry();
    assert_eq!(entry.generation, Some(second));
    assert!(entry.last_full);
    let ids: Vec<Uuid> = entry.volumes.iter().map(|v| v.id).collect();
    assert_eq!(ids, [s.volume, data]);
    assert_eq!(
        s.bitmaps(),
        [
            [replica_bitmap(s.b.id, second)],
            [replica_bitmap(s.b.id, second)]
        ]
    );
}

/// 보내다 실패하면 Task가 실패하고 항목에 이유가 남는다. 새 비트맵은 없고 기준 비트맵은 그대로라 다음 회차가
/// 증분으로 이어진다. B는 그 회차의 겹층을 버리고 복제본은 앞 회차 그대로다.
#[tokio::test]
async fn a_failed_send_keeps_the_replica_and_the_base_bitmap() {
    let s = setup().await;
    let first = succeeded(&s.replicate().await);
    s.a.hv.fail_next_replicate("NBD connection reset");
    let error = failed(&s.replicate().await);
    assert!(error.contains("NBD connection reset"), "{error}");
    // 실패한 회차의 정리가 원본 QEMU에 남은 플리싱·작업을 치운다.
    assert_eq!(s.a.hv.leftover_clears(), [s.vm]);
    let entry = s.entry();
    assert_eq!(entry.state, ReplicationState::Ready);
    assert_eq!(entry.generation, Some(first));
    assert!(
        entry
            .error
            .as_deref()
            .unwrap()
            .contains("NBD connection reset"),
        "{entry:?}"
    );
    assert_eq!(s.bitmaps(), [[replica_bitmap(s.b.id, first)]]);
    // B: 겹층을 만들었다가 합치지 않고 버렸다.
    assert_eq!(s.b.storage.overlays(), [("create", s.volume)]);
    let record = s.b.record(s.vm).unwrap();
    assert_eq!(record.generation, Some(first));
    assert_eq!(record.volumes[0].overlay, None);
    assert!(s.b.hv.receivers().is_empty());

    let third = succeeded(&s.replicate().await);
    assert_eq!(
        s.last_targets()[0].base,
        Some(replica_bitmap(s.b.id, first))
    );
    let entry = s.entry();
    assert_eq!(entry.generation, Some(third));
    assert_eq!(entry.error, None);
}

/// 끝내기 응답을 잃으면(받는 쪽은 끝냈을 수 있다) 두 비트맵을 모두 남기고 실패한다. 다음 회차는 옛 회차의
/// 비트맵으로 증분하고(받는 쪽의 새 시점이 기준보다 새로우므로 받는다) 성공하면 둘 다 지운다.
#[tokio::test]
async fn a_lost_finish_reply_keeps_both_bitmaps_and_the_next_run_still_increments() {
    let s = setup().await;
    let first = succeeded(&s.replicate().await);
    s.b.receiver
        .as_ref()
        .unwrap()
        .lose_next_finish
        .store(true, Ordering::SeqCst);
    let task = s.replicate().await;
    let error = failed(&task);
    assert!(
        error.contains("cannot tell whether node \"node2\" kept the replica"),
        "{error}"
    );
    let lost: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        s.bitmaps(),
        [[replica_bitmap(s.b.id, first), replica_bitmap(s.b.id, lost)]]
    );
    assert_eq!(s.entry().generation, Some(first));
    assert_eq!(s.b.record(s.vm).unwrap().generation, Some(lost));

    let next = succeeded(&s.replicate().await);
    assert_eq!(
        s.last_targets()[0].base,
        Some(replica_bitmap(s.b.id, first))
    );
    assert_eq!(s.bitmaps(), [[replica_bitmap(s.b.id, next)]]);
    assert_eq!(s.entry().generation, Some(next));
}

/// B가 기준 회차를 잃었으면(기록이 옛것) 증분을 거절하고, 원본은 그 디스크를 전체로 바꿔 한 번 더 부른다.
#[tokio::test]
async fn a_target_without_the_base_run_gets_the_disk_in_full() {
    let s = setup().await;
    succeeded(&s.replicate().await);
    let mut record = s.b.record(s.vm).unwrap();
    record.generation = None;
    record.synced_at = None;
    replicas::write(&s.b.replica_dir, &record).unwrap();
    let second = succeeded(&s.replicate().await);
    // 비트맵은 있었으므로 증분으로 시도했고, 거절된 뒤 전체로 보냈다.
    assert_eq!(s.a.hv.replications().len(), 2);
    assert_eq!(s.last_targets()[0].base, None);
    let entry = s.entry();
    assert_eq!(entry.generation, Some(second));
    assert!(entry.last_full);
    assert_eq!(s.b.record(s.vm).unwrap().generation, Some(second));
}

/// 대상 노드가 오프라인이면: 스케줄러는 Task 없이 항목에 이유만 남기고, 지금 복제는 Task가 그 문구로 실패한다.
#[tokio::test]
async fn an_offline_target_fails_the_task_and_the_scheduler_only_records_it() {
    let s = setup_with("*/1", false).await;
    let mut b = s.a.cluster.get::<NodeSpec>(s.b.id).unwrap().unwrap();
    b.status.online = false;
    s.a.cluster
        .update_status::<NodeSpec>(s.b.id, &b.status)
        .await
        .unwrap();
    let leader = if s.a.cluster.members().leader == Some(s.a.cluster.id()) {
        &s.a
    } else {
        &s.b
    };
    let scheduler = ReplicationScheduler::spawn(leader.state(), fast_scheduler(2));
    wait_until("the offline error", || {
        s.vm()
            .status
            .replication
            .iter()
            .any(|r| r.error.as_deref() == Some("node \"node2\" is offline"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s.a.hv.replications().is_empty());
    let (_, tasks) = s.a.cluster.list::<TaskSpec>().unwrap();
    assert!(
        !tasks
            .iter()
            .any(|t| matches!(t.spec.op, TaskOp::VmReplicate { .. }))
    );
    scheduler.shutdown().await;

    let error = failed(&s.replicate().await);
    assert_eq!(error, "node \"node2\" is offline");
    assert_eq!(
        s.entry().error.as_deref(),
        Some("node \"node2\" is offline")
    );
    // Task를 만들 때 시도를 적었다(곧바로 실패해도 스케줄러가 주기 전에 다시 만들지 않는다).
    assert!(s.entry().last_try.is_some());
}

/// VM이 이 노드에서 돌지 않게 된 뒤(옮겨 갔거나 꺼짐) 시작한 복제 Task는 실패가 아니라 건너뛰어 끝나고(그 경고를
/// 남긴다) 항목에 오류를 남기지 않는다.
#[tokio::test]
async fn a_run_for_a_vm_no_longer_running_here_is_skipped() {
    let s = setup().await;
    let task = s.admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let mut run: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmReplicate { node: s.b.id },
            target: s.vm,
            node: s.a.id,
        },
    );
    run.name = run.id.to_string();
    s.a.cluster.create(&run).await.unwrap();
    let state = || s.a.cluster.get::<TaskSpec>(run.id).unwrap().unwrap().status;
    wait_until("the run to end", || state().state.is_finished()).await;
    assert_eq!(state().state, TaskState::Succeeded, "{:?}", state());
    assert_eq!(
        state().error.as_deref(),
        Some("replication run skipped: the VM is not running on this node")
    );
    assert!(s.a.hv.replications().is_empty());
    assert!(
        s.vm().status.replication.iter().all(|r| r.error.is_none()),
        "{:?}",
        s.vm().status.replication
    );
}

/// 복제 항목은 볼륨의 `spec.size_bytes`를 적는다(백엔드가 맞춘 이미지의 가상 크기가 달라도) — HA·마이그레이션이
/// 그 척도로 견준다. 크기가 그대로면 다음 회차는 증분이다.
#[tokio::test]
async fn replica_sizes_use_the_volume_size_not_the_image_size() {
    let s = setup().await;
    s.a.storage.set_size(s.volume, SIZE + 512);
    let first = succeeded(&s.replicate().await);
    let entry = s.entry();
    assert_eq!(entry.volumes.len(), 1);
    assert_eq!(entry.volumes[0].size_bytes, SIZE);
    succeeded(&s.replicate().await);
    assert_eq!(
        s.last_targets()[0].base,
        Some(replica_bitmap(s.b.id, first))
    );
    assert!(!s.entry().last_full);
}

/// 지금 복제의 검사: 꺼진 VM은 409 `vm_not_running`, 같은 대상의 Task가 끝나지 않았으면 409 `vm_busy`, 대상이
/// 아닌 노드는 400, 대상이 둘이면 `node`가 있어야 한다(400).
#[tokio::test]
async fn replicate_requests_are_checked() {
    let s = setup().await;
    // 같은 대상의 Task가 끝나지 않았다.
    s.a.hv.hold_next_replicate();
    let (status, task) = s.admin.post("/vms/web/replicate", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    tokio::time::timeout(WAIT, s.a.hv.replicate_held())
        .await
        .unwrap();
    let (status, body) = s
        .admin
        .post("/vms/web/replicate", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
    assert_eq!(
        body["message"],
        format!(
            "VM \"web\" is already being replicated to node \"node2\" (task {})",
            task["id"].as_str().unwrap()
        )
    );
    s.a.hv.release_replicate();
    succeeded(&s.admin.wait_task(&task).await);

    let (status, body) = s
        .admin
        .post("/vms/web/replicate", json!({ "node": "node1" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "node \"node1\" is not a replication target of VM \"web\""
    );
    let (status, body) = s
        .admin
        .post("/vms/web/replicate", json!({ "node": "nope" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 대상이 둘(저장소에만 있는 node3, 어디서나 쓰는 dir 풀 `c`)
    let mut c: Resource<NodeSpec, NodeStatus> = Resource::new(
        "node3",
        NodeSpec {
            address: "127.0.0.3:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    c.status.online = true;
    c.status.features = FEATURES.iter().map(|f| f.to_string()).collect();
    s.a.cluster.create(&c).await.unwrap();
    let pool: StoragePool = Resource::new(
        "c",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/nonexistent/opl-c".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    s.a.cluster.create(&pool).await.unwrap();
    let vm = s.vm();
    let mut spec = serde_json::to_value(&vm.spec).unwrap();
    spec["replication"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "node": c.id, "pool": "c" }));
    let (status, body) = s
        .admin
        .put("/vms/web", json!({ "spec": spec }), Some(vm.revision))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = s.admin.post("/vms/web/replicate", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "VM \"web\" has several replication targets; choose one with node"
    );
    // 복제를 모르는 노드가 있으면 409 `cluster_upgrading`
    let mut old = s.a.cluster.get::<NodeSpec>(c.id).unwrap().unwrap();
    old.status.features.retain(|f| f != NODE_REPLICATION);
    s.a.cluster
        .update_status::<NodeSpec>(c.id, &old.status)
        .await
        .unwrap();
    let (status, body) = s
        .admin
        .post("/vms/web/replicate", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    old.status.features = FEATURES.iter().map(|f| f.to_string()).collect();
    s.a.cluster
        .update_status::<NodeSpec>(c.id, &old.status)
        .await
        .unwrap();

    let task = s.admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, body) = s
        .admin
        .post("/vms/web/replicate", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_not_running");
}

/// 복제 Task는 VM의 줄에 선다: 복제가 도는 동안 만든 같은 VM의 핫플러그 Task는 복제가 끝난 뒤에 시작한다.
#[tokio::test]
async fn a_replication_task_holds_the_vm_queue() {
    let s = setup().await;
    s.a.hv.hold_next_replicate();
    let (status, task) = s.admin.post("/vms/web/replicate", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    tokio::time::timeout(WAIT, s.a.hv.replicate_held())
        .await
        .unwrap();
    let mut hotplug: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmHotplug,
            target: s.vm,
            node: s.a.id,
        },
    );
    hotplug.name = hotplug.id.to_string();
    s.a.cluster.create(&hotplug).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let state = |id| {
        s.a.cluster
            .get::<TaskSpec>(id)
            .unwrap()
            .unwrap()
            .status
            .state
    };
    assert_eq!(state(hotplug.id), TaskState::Pending);
    s.a.hv.release_replicate();
    succeeded(&s.admin.wait_task(&task).await);
    wait_until("the hotplug task", || state(hotplug.id).is_finished()).await;
}

/// 백업 비트맵과 복제 비트맵은 따로다: 백업이 다른 백업 비트맵을 지워도 복제는 증분으로 이어지고, 복제가 앞
/// 회차 비트맵을 지워도 백업 비트맵은 남는다.
#[tokio::test]
async fn backup_bitmaps_and_replication_bitmaps_stay_apart() {
    let s = setup().await;
    let first = succeeded(&s.replicate().await);
    // 증분 백업이 하듯 새 백업 비트맵을 만들고 다른 백업 비트맵을 지운다.
    let backup = bitmap_name(Uuid::now_v7());
    s.a.hv.add_bitmaps(s.vm, &backup).await.unwrap();
    s.a.hv.remove_bitmaps(s.vm, Some(&backup)).await.unwrap();
    let second = succeeded(&s.replicate().await);
    assert_eq!(
        s.last_targets()[0].base,
        Some(replica_bitmap(s.b.id, first))
    );
    assert_eq!(
        s.bitmaps(),
        [[backup.clone(), replica_bitmap(s.b.id, second)]]
    );
}

/// 주기 스케줄러를 짧게: `tick` 50ms, 일정의 한 분은 1초
fn fast_scheduler(max_concurrent: usize) -> ReplicationSchedulerConfig {
    ReplicationSchedulerConfig {
        tick: Duration::from_millis(50),
        max_concurrent,
        minute: Duration::from_secs(1),
    }
}

/// 리더만 주기가 지난 대상의 Task를 만들고, 주기마다 다시 한다. 스펙에서 뺀 대상의 항목은 지운다.
#[tokio::test]
async fn the_scheduler_replicates_due_targets_on_the_leader_only() {
    let s = setup_with("*/1", false).await;
    let leader_is_a = s.a.cluster.members().leader == Some(s.a.cluster.id());
    let (leader, follower) = if leader_is_a {
        (&s.a, &s.b)
    } else {
        (&s.b, &s.a)
    };
    let idle = ReplicationScheduler::spawn(follower.state(), fast_scheduler(2));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(s.a.hv.replications().is_empty());
    assert!(s.vm().status.replication.is_empty());

    let scheduler = ReplicationScheduler::spawn(leader.state(), fast_scheduler(2));
    let first = {
        let mut generation = None;
        wait_until("the first scheduled run", || {
            generation = s.vm().status.replication.first().and_then(|r| r.generation);
            generation.is_some()
        })
        .await;
        generation.unwrap()
    };
    assert!(s.entry().last_full);
    // 주기(1초) 뒤 다시 한다(증분).
    wait_until("the next scheduled run", || {
        s.entry().generation.is_some_and(|g| g != first)
    })
    .await;
    assert!(s.a.hv.replications().len() >= 2);
    assert_eq!(
        s.a.hv.replications()[1].targets[0].base,
        Some(replica_bitmap(s.b.id, first))
    );

    // 대상에서 빼면 항목을 지운다.
    loop {
        let vm = s.vm();
        let mut spec = vm.spec.clone();
        spec.replication.clear();
        if s.a
            .cluster
            .update_spec(vm.id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
            .is_ok()
        {
            break;
        }
    }
    wait_until("the entry to go", || s.vm().status.replication.is_empty()).await;
    scheduler.shutdown().await;
    idle.shutdown().await;
}

/// 원본 노드마다 끝나지 않은 복제 Task는 `max_concurrent`개까지다. 나머지는 앞 것이 끝난 뒤에 만든다.
#[tokio::test]
async fn the_scheduler_keeps_to_max_concurrent_per_source_node() {
    let s = setup_with("*/60", false).await;
    let leader = if s.a.cluster.members().leader == Some(s.a.cluster.id()) {
        &s.a
    } else {
        &s.b
    };
    local_volume(&s.a, "db-root").await;
    let mut req = vm_request("db");
    req["spec"]["node"] = json!(s.a.id);
    req["spec"]["disks"] = json!([{ "volume": "db-root", "bus": "virtio-blk" }]);
    req["spec"]["replication"] = json!([{ "node": s.b.id, "schedule": "*/60", "pool": "b" }]);
    let (status, body) = s.admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = s.admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    s.a.hv.hold_next_replicate();
    let scheduler = ReplicationScheduler::spawn(leader.state(), fast_scheduler(1));
    tokio::time::timeout(WAIT, s.a.hv.replicate_held())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.unfinished_replications().len(), 1);
    s.a.hv.release_replicate();
    wait_until("both VMs to be replicated", || {
        let (_, vms) = s.a.cluster.list::<VmSpec>().unwrap();
        vms.iter().all(|vm| {
            vm.status
                .replication
                .iter()
                .any(|r| r.state == ReplicationState::Ready)
        })
    })
    .await;
    scheduler.shutdown().await;
}

/// HA 복구(계획 55): A의 리스가 끊기면 리더(B)가 B의 복제본으로 VM을 옮기고, B가 복제본을 승격해 풀 `b`의 볼륨으로
/// 켠다. 복제 방향이 뒤집혀 A가 대상(풀 `a`, `stale`)이 되고, A가 돌아오면 청소가 A의 원본 기록을 옛 복제본으로 바꾸며
/// 다음 B→A 회차가 전체로 보내 `ready`가 된다.
#[tokio::test]
async fn an_ha_vm_restarts_from_its_replica_and_the_replication_turns_around() {
    let s = setup_full("*/15", false, true).await;
    succeeded(&s.replicate().await);
    let synced_at = s.entry().synced_at.unwrap();
    assert_eq!(s.a.record(s.vm).unwrap().role, Role::Primary);
    assert_eq!(s.b.cluster.members().leader, Some(NodeId(s.b.id)));
    // B의 리스만 갱신한다(A는 죽었다).
    let renewing = {
        let cluster = s.b.cluster.clone();
        let node = s.b.id;
        tokio::spawn(async move {
            loop {
                let _ = cluster
                    .execute_as_leader(LeaderRequest::RenewLease {
                        node: NodeId(node),
                        checks: vec![],
                        load: None,
                    })
                    .await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    };
    let manager = HaManager::spawn(
        s.b.state(),
        HaManagerConfig {
            tick: Duration::from_millis(100),
            ..Default::default()
        },
    );
    wait_until("the VM running on B", || {
        let vm = s.vm();
        vm.status.phase == VmPhase::Running && vm.status.node == Some(s.b.id)
    })
    .await;
    manager.shutdown().await;
    renewing.abort();

    // 볼륨은 B의 풀 `b`에 있고, B는 그 복제본으로 켰다.
    let volume = s.a.cluster.get::<VolumeSpec>(s.volume).unwrap().unwrap();
    assert_eq!(volume.spec.pool, "b");
    assert_eq!(volume.status.node, Some(s.b.id));
    let launched = s.b.hv.launch_specs().last().unwrap().disks[0].path.clone();
    assert_eq!(
        launched,
        PathBuf::from(format!("/nonexistent/opl-b/{}.qcow2", s.volume))
    );
    assert_eq!(s.b.record(s.vm).unwrap().role, Role::Primary);
    let vm = s.vm();
    assert_eq!(vm.spec.node, Some(s.b.id));
    assert_eq!(
        vm.spec.replication,
        [opl_types::replication::ReplicationTarget {
            node: s.a.id,
            schedule: "*/15".into(),
            pool: Some("a".into()),
        }]
    );
    assert_eq!(
        vm.status.replication,
        [ReplicaStatus {
            state: ReplicationState::Stale,
            ..ReplicaStatus::pending(s.a.id)
        }]
    );
    // 복제본으로 한 복구가 남는다(VM이 켜진 뒤에도).
    let recovery = vm.status.last_recovery.expect("the recovery is recorded");
    assert_eq!(
        (recovery.from, recovery.node, recovery.synced_at),
        (s.a.id, s.b.id, synced_at)
    );
    assert!(recovery.at >= synced_at);
    let (_, tasks) = s.a.cluster.list::<TaskSpec>().unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.spec.op == TaskOp::VmHaRecover { from: s.a.id })
    );

    // A가 돌아온다: 청소가 원본 기록을 옛 복제본으로 바꾸고, 다음 회차(B→A)는 전체다.
    s.a.receiver.as_ref().unwrap().inner.sweep().await;
    let record = s.a.record(s.vm).unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.volumes[0].state, ReplicaState::Stale);
    let (status, task) = s.admin.post("/vms/web/replicate", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], json!(s.b.id), "{task}");
    let run = succeeded(&s.admin.wait_task(&task).await);
    let entry = s
        .vm()
        .status
        .replication
        .into_iter()
        .find(|r| r.node == s.a.id)
        .unwrap();
    assert_eq!(entry.state, ReplicationState::Ready);
    assert_eq!(entry.generation, Some(run));
    assert!(entry.last_full);
    let record = s.a.record(s.vm).unwrap();
    assert_eq!(record.generation, Some(run));
    assert_eq!(record.volumes[0].state, ReplicaState::Ready);
    assert_eq!(record.volumes[0].pool, "a");
    // 다음 복제가 성공해도 복구 기록은 남는다.
    assert_eq!(s.vm().status.last_recovery, Some(recovery));
}

/// 복제 대상 노드로 옮기면(계획 55) 미러 대신 그 복제본을 따라잡는다: 복제 비트맵 뒤 바뀐 블록부터 보내고 원본 디스크는
/// 지우지 않는다. 확정하면 복제 방향이 뒤집혀 A가 대상(풀 `a`)이 되고, A의 디스크는 그 마이그레이션 시점의 `ready`
/// 복제본으로 남으며, B는 넘겨받으며 A로의 복제 비트맵을 둔다. 그래서 다음 B→A 회차는 바뀐 것만 보내고, A로 되돌아갈
/// 때도 따라잡는다.
#[tokio::test]
async fn migrating_to_the_replica_node_sends_only_the_changes_and_turns_the_replication_around() {
    let s = setup().await;
    let first = succeeded(&s.replicate().await);
    let task = succeeded(&s.migrate("node2", None).await);
    s.wait_running_on(&s.b).await;

    // 미러하지 않고 B의 복제본(풀 `b`)을 따라잡았다.
    assert!(s.a.hv.mirrors().is_empty());
    let catch_ups = s.a.hv.catch_ups();
    assert_eq!(catch_ups.len(), 1, "{catch_ups:?}");
    assert_eq!(catch_ups[0].task, task);
    assert_eq!(catch_ups[0].base, [Some(replica_bitmap(s.b.id, first))]);
    assert_eq!(catch_ups[0].targets[0].disk, 0);
    assert_eq!(s.a.hv.switchovers(), [s.vm]);
    let received = s.b.hv.incoming_specs().last().unwrap().disks[0]
        .path
        .clone();
    assert_eq!(
        received,
        PathBuf::from(format!("/nonexistent/opl-b/{}.qcow2", s.volume))
    );
    // 원본 파일은 남고 볼륨 기록은 B의 풀 `b`로 옮겨졌다.
    assert!(s.a.storage.has_file(s.volume));
    assert!(!s.a.storage.deleted().contains(&s.volume));
    let volume = s.a.cluster.get::<VolumeSpec>(s.volume).unwrap().unwrap();
    assert_eq!(volume.spec.pool, "b");
    assert_eq!(volume.status.node, Some(s.b.id));

    // 복제 방향이 뒤집혔다.
    let vm = s.vm();
    assert_eq!(vm.spec.node, Some(s.b.id));
    assert_eq!(
        vm.spec.replication,
        [ReplicationTarget {
            node: s.a.id,
            schedule: "*/15".into(),
            pool: Some("a".into()),
        }]
    );
    assert_eq!(
        vm.status.replication.len(),
        1,
        "{:?}",
        vm.status.replication
    );
    let entry = &vm.status.replication[0];
    assert_eq!(entry.node, s.a.id);
    assert_eq!(entry.state, ReplicationState::Ready);
    assert_eq!(entry.generation, Some(task));
    assert!(entry.synced_at.is_some());
    assert_eq!(
        entry.volumes,
        [ReplicaVolume {
            id: s.volume,
            size_bytes: SIZE
        }]
    );
    let record = s.a.record(s.vm).unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.generation, Some(task));
    assert_eq!(record.synced_at, entry.synced_at);
    assert_eq!(record.volumes[0].pool, "a");
    assert_eq!(record.volumes[0].state, ReplicaState::Ready);
    // B의 reconciler가 넘겨받으며 B의 기록을 원본으로 바꾼다.
    wait_until("B's record to become the primary", || {
        s.b.record(s.vm).is_some_and(|r| r.role == Role::Primary)
    })
    .await;
    let names: Vec<String> = s.b.hv.bitmaps(s.vm)[0]
        .iter()
        .map(|b| b.name.clone())
        .collect();
    assert!(names.contains(&replica_bitmap(s.a.id, task)), "{names:?}");

    // 다음 B→A 회차는 그 비트맵 뒤 바뀐 것만 보낸다.
    let (status, run) = s.admin.post("/vms/web/replicate", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    assert_eq!(run["spec"]["node"], json!(s.b.id), "{run}");
    let run = succeeded(&s.admin.wait_task(&run).await);
    let targets = s.b.hv.replications().last().unwrap().targets.clone();
    assert_eq!(targets[0].base, Some(replica_bitmap(s.a.id, task)));
    let entry = s.vm().status.replication[0].clone();
    assert_eq!(
        (entry.state, entry.generation, entry.last_full),
        (ReplicationState::Ready, Some(run), false)
    );

    // 되돌아온다: B가 A의 복제본을 따라잡고 방향이 다시 뒤집힌다.
    let back = succeeded(&s.migrate("node1", None).await);
    s.wait_running_on(&s.a).await;
    let catch_ups = s.b.hv.catch_ups();
    assert_eq!(catch_ups.len(), 1, "{catch_ups:?}");
    assert_eq!(catch_ups[0].base, [Some(replica_bitmap(s.a.id, run))]);
    let vm = s.vm();
    assert_eq!(
        vm.spec.replication,
        [ReplicationTarget {
            node: s.b.id,
            schedule: "*/15".into(),
            pool: Some("b".into()),
        }]
    );
    assert_eq!(vm.status.replication[0].node, s.b.id);
    assert_eq!(vm.status.replication[0].generation, Some(back));
    let volume = s.a.cluster.get::<VolumeSpec>(s.volume).unwrap().unwrap();
    assert_eq!(
        (volume.spec.pool.as_str(), volume.status.node),
        ("a", Some(s.a.id))
    );
    wait_until("A's record to become the primary", || {
        s.a.record(s.vm).is_some_and(|r| r.role == Role::Primary)
    })
    .await;
    assert_eq!(s.b.record(s.vm).unwrap().role, Role::Replica);
    assert!(s.b.storage.has_file(s.volume));
}

/// 이미지의 가상 크기가 볼륨 크기와 달라도(백엔드가 맞춘 크기) 마이그레이션 뒤 A에 남긴 복제본은 다음 B→A 회차의
/// 증분을 받는다: A의 기록은 이미지 크기, 복제 항목은 볼륨 크기를 적는다.
#[tokio::test]
async fn an_unaligned_disk_left_by_a_migration_still_takes_increments() {
    let s = setup().await;
    s.a.storage.set_size(s.volume, SIZE + 512);
    let first = succeeded(&s.replicate().await);
    let task = succeeded(&s.migrate("node2", None).await);
    s.wait_running_on(&s.b).await;
    // 따라잡기도 복제 비트맵 뒤 바뀐 것만 보냈다(복제 항목의 볼륨 크기로 견준다).
    let catch_ups = s.a.hv.catch_ups();
    assert_eq!(catch_ups[0].base, [Some(replica_bitmap(s.b.id, first))]);
    let entry = s.vm().status.replication[0].clone();
    assert_eq!(
        entry.volumes,
        [ReplicaVolume {
            id: s.volume,
            size_bytes: SIZE
        }]
    );
    assert_eq!(s.a.record(s.vm).unwrap().volumes[0].size_bytes, SIZE + 512);
    wait_until("B's record to become the primary", || {
        s.b.record(s.vm).is_some_and(|r| r.role == Role::Primary)
    })
    .await;
    let (status, run) = s.admin.post("/vms/web/replicate", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let run = succeeded(&s.admin.wait_task(&run).await);
    let targets = s.b.hv.replications().last().unwrap().targets.clone();
    assert_eq!(targets[0].base, Some(replica_bitmap(s.a.id, task)));
    let entry = s.vm().status.replication[0].clone();
    assert_eq!((entry.generation, entry.last_full), (Some(run), false));
}

/// 확정 쓰기 전에 A가 남길 디스크를 원본 기록에 적는다: 끝내기가 A의 디스크를 복제본으로 적지 못해도(원본 QEMU를
/// 끄지 못함 — opld가 확정 뒤 죽은 것과 같다) 청소가 그 디스크들을 옛 복제본으로 알아보고 다음 B→A 회차가 그 위에
/// 전체로 보낸다. 마지막 복제 뒤 더한 디스크도 그렇다.
#[tokio::test]
async fn a_migration_records_the_disks_it_leaves_before_committing() {
    let s = setup().await;
    succeeded(&s.replicate().await);
    // 마지막 복제 뒤 디스크 data를 더한다(A의 원본 기록에는 없다).
    let data = local_volume(&s.a, "data").await;
    let task = s.admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = s.vm();
    let mut spec = serde_json::to_value(&vm.spec).unwrap();
    spec["disks"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "volume": "data", "bus": "virtio-blk" }));
    let (status, body) = s
        .admin
        .put("/vms/web", json!({ "spec": spec }), Some(vm.revision))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let task = s.admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    s.a.hv.set_unkillable(s.vm, true);
    succeeded(&s.migrate("node2", None).await);
    s.wait_running_on(&s.b).await;
    let record = s.a.record(s.vm).unwrap();
    assert_eq!(record.role, Role::Primary);
    let mut ids: Vec<Uuid> = record.volumes.iter().map(|v| v.id).collect();
    ids.sort();
    let mut both = vec![s.volume, data];
    both.sort();
    assert_eq!(ids, both);
    s.a.hv.set_unkillable(s.vm, false);
    wait_until("B's record to become the primary", || {
        s.b.record(s.vm).is_some_and(|r| r.role == Role::Primary)
    })
    .await;

    let (status, run) = s.admin.post("/vms/web/replicate", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let run = succeeded(&s.admin.wait_task(&run).await);
    let entry = s.vm().status.replication[0].clone();
    assert_eq!(
        (entry.node, entry.state, entry.generation, entry.last_full),
        (s.a.id, ReplicationState::Ready, Some(run), true)
    );
    let record = s.a.record(s.vm).unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.volumes.len(), 2);
    assert!(
        record
            .volumes
            .iter()
            .all(|v| v.state == ReplicaState::Ready)
    );
    assert!(s.a.storage.deleted().is_empty());
}

/// 따라잡기가 실패하면 마이그레이션을 되돌리고 VM은 A에서 계속 돈다. B의 복제본은 반쯤 쓰였을 수 있어 `syncing`으로
/// 남고(HA가 쓰지 않는다), 다음 회차가 끝나면 다시 `ready`다.
#[tokio::test]
async fn a_failed_catch_up_rolls_back_and_leaves_the_replica_syncing() {
    let s = setup().await;
    succeeded(&s.replicate().await);
    s.a.hv
        .fail_next_catch_up(DriverError::Migration("Connection reset by peer".into()));
    let error = failed(&s.migrate("node2", None).await);
    assert!(error.contains("Connection reset by peer"), "{error}");
    // 되돌리기가 원본 QEMU에 남은 따라잡기의 플리싱·작업을 치운다.
    assert_eq!(s.a.hv.leftover_clears(), [s.vm]);
    let vm = s.vm();
    assert_eq!(vm.spec.node, Some(s.a.id));
    assert_eq!(vm.status.migration, None);
    assert_eq!(s.entry().state, ReplicationState::Syncing);
    let record = s.b.record(s.vm).unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.volumes[0].state, ReplicaState::Syncing);
    // 원본도 복제본도 지우지 않았다.
    assert!(s.a.storage.has_file(s.volume) && s.b.storage.has_file(s.volume));
    assert!(s.b.storage.deleted().is_empty());

    let run = succeeded(&s.replicate().await);
    let entry = s.entry();
    assert_eq!(
        (entry.state, entry.generation),
        (ReplicationState::Ready, Some(run))
    );
    assert_eq!(
        s.b.record(s.vm).unwrap().volumes[0].state,
        ReplicaState::Ready
    );
}

/// 복제본의 풀과 다른 풀로 복제 대상에 옮기려 하면 거절한다. 복제 대상이 아닌 노드로는 계획 42처럼 새 볼륨에 미러한다.
#[tokio::test]
async fn only_replication_targets_are_caught_up() {
    let s = setup().await;
    succeeded(&s.replicate().await);
    let (status, body) = s
        .admin
        .post("/vms/web/migrate", json!({ "node": "node2", "pool": "a" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "migration_unsupported", "{body}");
    assert_eq!(
        body["message"],
        "node \"node2\" holds the replica of VM \"web\" in pool \"b\"; migrate without --pool or remove the node from the replication targets"
    );

    // 복제 대상에서 빼면 B의 청소가 복제본을 지우고, 그 뒤 B로는 미러한다.
    let vm = s.vm();
    let mut spec = vm.spec.clone();
    spec.replication.clear();
    s.a.cluster
        .update_spec(vm.id, Some(vm.revision), vm.labels.clone(), &spec)
        .await
        .unwrap();
    wait_until("B to see the change", || {
        let vm = s.b.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
        vm.spec.replication.is_empty()
    })
    .await;
    s.b.receiver.as_ref().unwrap().inner.sweep().await;
    assert!(s.b.record(s.vm).is_none());
    assert!(!s.b.storage.has_file(s.volume));
    succeeded(&s.migrate("node2", Some("b")).await);
    s.wait_running_on(&s.b).await;
    assert!(s.a.hv.catch_ups().is_empty());
    assert_eq!(s.a.hv.mirrors().len(), 1);
    assert!(s.b.storage.has_file(s.volume));
}

/// 원본은 바뀐 것만 보내려 했지만 B의 복제본이 그 회차의 온전한 사본이 아니면(옛 복제본 등) B가 거절하고 원본은 그
/// 디스크를 전체부터 따라잡는다.
#[tokio::test]
async fn a_replica_that_cannot_take_the_changes_is_caught_up_in_full() {
    let s = setup().await;
    succeeded(&s.replicate().await);
    let mut record = s.b.record(s.vm).unwrap();
    record.volumes[0].state = ReplicaState::Stale;
    replicas::write(&s.b.replica_dir, &record).unwrap();
    succeeded(&s.migrate("node2", None).await);
    let catch_ups = s.a.hv.catch_ups();
    assert_eq!(catch_ups.len(), 1, "{catch_ups:?}");
    assert_eq!(catch_ups[0].base, [None]);
    s.wait_running_on(&s.b).await;
    assert_eq!(s.vm().status.replication[0].node, s.a.id);
}
