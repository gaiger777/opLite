//! 실제 gRPC + mTLS로 잇는 노드들(한 프로세스 안, 127.0.0.1의 빈 포트).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
use opl_peer::pki::{ClusterCa, NodeTls, api_cert, node_key_and_csr};
use opl_peer::{
    AuditSource, GrpcNetwork, IncomingDisk, IncomingError, IncomingMigration, LEASE_LIVENESS,
    MetricsReply, MetricsSource, MigrateIncomingRequest, MigrateIncomingResponse,
    MigrationReceiver, NodeMonitor, NodeMonitorConfig, PeerService, Peers, UefiVarsError,
    renew_liveness,
};
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

/// 테스트가 동시에 많이 돌아 CPU가 바쁘면 선거가 몇 초씩 걸린다(openraft는 리더 임기 동안 다른
/// 노드에 투표하지 않으므로 리더를 잃으면 보통 선거 제한 시간의 두 배쯤 걸린다).
const WAIT: Duration = Duration::from_secs(30);

fn tls_for(ca: &ClusterCa, id: NodeId) -> NodeTls {
    let (key_pem, csr) = node_key_and_csr().unwrap();
    NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: ca.sign_node(&csr, id.0, &["127.0.0.1".into()]).unwrap(),
        key_pem,
    }
}

fn volume(name: &str) -> Volume {
    Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

struct Node {
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 이 노드의 드라이버 `state_dir`(UEFI 변수 파일을 보내는 곳)
    state_dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Node {
    async fn start(ca: &ClusterCa, root: &TempDir) -> Node {
        Self::start_with(ca, root, None, None, None, None).await
    }

    /// `migrations`가 있으면 마이그레이션을 그것으로 받고, `audit`이 있으면 감사 로그를, `metrics`가
    /// 있으면 지표를 그것으로 읽는다. `work`가 있으면 Raft·리스가 아닌 처리를 그 실행기에서 돌린다.
    async fn start_with(
        ca: &ClusterCa,
        root: &TempDir,
        migrations: Option<Arc<dyn MigrationReceiver>>,
        audit: Option<Arc<dyn AuditSource>>,
        metrics: Option<Arc<dyn MetricsSource>>,
        work: Option<tokio::runtime::Handle>,
    ) -> Node {
        let id = NodeId(Uuid::now_v7());
        let tls = tls_for(ca, id);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
        config.write_timeout = WAIT;
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
        let hv = Arc::new(MockHypervisor::default());
        let state_dir = root.path().join(format!("{id}-state"));
        let mut service =
            PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>))
                .with_uefi_vars(&state_dir);
        if let Some(migrations) = migrations {
            service = service.with_migrations(migrations);
        }
        if let Some(audit) = audit {
            service = service.with_audit(audit);
        }
        if let Some(metrics) = metrics {
            service = service.with_metrics(metrics);
        }
        if let Some(work) = work {
            service = service.with_work(work);
        }
        let (stop, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        Node {
            addr,
            cluster,
            peers,
            hv,
            state_dir,
            stop: Some(stop),
            server: Some(server),
        }
    }

    fn id(&self) -> NodeId {
        self.cluster.id()
    }

    async fn stop(&mut self) {
        // opld처럼 노드 간 서버가 끝난 뒤 Raft를 멈춘다. 먼저 멈추면 서버가 아직 받는 전달 요청에
        // 다시 시도하지 않는 "raft stopped"로 답한다.
        let _ = self.stop.take().unwrap().send(());
        self.server.take().unwrap().await.unwrap();
        self.cluster.shutdown().await.unwrap();
    }
}

/// 받은 요청을 적고 정해 둔 결과를 돌려주는 받는 쪽
#[derive(Default)]
struct FakeReceiver {
    calls: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl MigrationReceiver for FakeReceiver {
    async fn incoming(
        &self,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        self.calls.lock().unwrap().push(format!(
            "incoming {} {} {} {:?}",
            req.vm, req.task, req.qemu_version, req.disks
        ));
        if req.qemu_version != "8.2.2" {
            return Err(IncomingError::Incompatible(format!(
                "QEMU {} differs from 8.2.2",
                req.qemu_version
            )));
        }
        // 복사할 디스크가 있으면 NBD 포트도 연다.
        Ok(MigrateIncomingResponse {
            port: 60007,
            nbd_port: (!req.disks.is_empty()).then_some(60008),
        })
    }

    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("wait {} {}", req.vm, req.task));
        Err(IncomingError::Failed("the receiving QEMU exited".into()))
    }

    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("abort {} {}", req.vm, req.task));
        Ok(())
    }
}

/// 받은 조건을 적고 항목 하나를 돌려주는 감사 로그
#[derive(Default)]
struct FakeAudit {
    queries: std::sync::Mutex<Vec<AuditQuery>>,
}

#[async_trait::async_trait]
impl AuditSource for FakeAudit {
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String> {
        if query.user.as_deref() == Some("broken") {
            return Err("cannot read the audit log".into());
        }
        self.queries.lock().unwrap().push(query);
        Ok(vec![AuditEntry {
            id: Uuid::now_v7(),
            time: 1_000,
            node: "n2".into(),
            user: Some("alice".into()),
            user_id: None,
            role: None,
            auth: AuditAuth::Session,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms".into(),
            action: "POST /vms".into(),
            target: None,
            status: 201,
            duration_ms: 2,
            detail: None,
        }])
    }
}

/// 받은 조건을 적고 `since`보다 뒤의 점을 돌려주는 지표(노드 점은 `t` 1000·2000)
#[derive(Default)]
struct FakeMetrics {
    queries: std::sync::Mutex<Vec<MetricsQuery>>,
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        self.queries.lock().unwrap().push(q.clone());
        let samples = [1_000, 2_000]
            .into_iter()
            .filter(|t| q.since.is_none_or(|since| *t > since))
            .map(|t| NodeSample {
                t,
                cpu: Some(12.5),
                mem_used_mib: Some(100),
                mem_total_mib: Some(200),
                disk_read_bps: None,
                disk_write_bps: None,
                net_rx_bps: None,
                net_tx_bps: None,
                fs_used_mib: None,
                fs_total_mib: None,
            })
            .collect();
        MetricsReply {
            node: Some(NodeSeries {
                node: Uuid::nil(),
                samples,
            }),
            vms: vec![],
        }
    }
}

/// 노드 `n`개를 띄우고 첫 노드에서 클러스터를 초기화한 뒤 모두가 같은 리더를 알 때까지 기다린다.
async fn start(n: usize) -> (ClusterCa, TempDir, Vec<Node>) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut nodes = Vec::new();
    for _ in 0..n {
        nodes.push(Node::start(&ca, &root).await);
    }
    let members: BTreeMap<_, _> = nodes
        .iter()
        .map(|n| (n.id(), BasicNode::new(&n.addr)))
        .collect();
    nodes[0].cluster.initialize(members).await.unwrap();
    wait_leader(&nodes).await;
    (ca, root, nodes)
}

async fn wait_leader(nodes: &[Node]) -> NodeId {
    let deadline = Instant::now() + WAIT;
    loop {
        let leaders: Vec<_> = nodes
            .iter()
            .map(|n| n.cluster.raft().unwrap().metrics().borrow().current_leader)
            .collect();
        if let Some(Some(leader)) = leaders.first()
            && leaders.iter().all(|l| *l == Some(*leader))
            && let Some(node) = nodes.iter().find(|n| n.id() == *leader)
            && node.cluster.raft().unwrap().metrics().borrow().state == ServerState::Leader
        {
            return *leader;
        }
        assert!(Instant::now() < deadline, "no leader");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn nodes_replicate_and_forward_over_mtls() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();

    let v = volume("disk-a");
    nodes[follower].cluster.create(&v).await.unwrap();
    assert!(
        nodes[follower]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );

    // 리더가 죽어도 남은 두 노드로 쓰기가 이어진다.
    let leader = nodes.iter().position(|n| n.id() == leader).unwrap();
    nodes[leader].stop().await;
    let rest: Vec<usize> = (0..3).filter(|i| *i != leader).collect();
    let v = volume("disk-b");
    nodes[rest[0]].cluster.create(&v).await.unwrap();
    assert!(
        nodes[rest[0]]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn strangers_and_non_members_are_refused() {
    let (ca, root, nodes) = start(1).await;
    // 다른 CA의 인증서로는 TLS 연결이 맺어지지 않는다.
    let other = ClusterCa::generate("other").unwrap();
    let stranger = Peers::new(&tls_for(&other, NodeId(Uuid::now_v7())));
    assert!(stranger.ping(&nodes[0].addr, WAIT).await.is_err());
    // 같은 CA라도 멤버가 아니면 거부한다(제거된 노드).
    let outsider = Node::start(&ca, &root).await;
    let err = outsider.peers.ping(&nodes[0].addr, WAIT).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err = outsider
        .peers
        .leader_request(&nodes[0].addr, LeaderRequest::Remove { id: nodes[0].id() })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");
    // 멤버는 된다.
    assert_eq!(
        nodes[0].peers.ping(&nodes[0].addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
    // 멤버의 API 인증서(서버 인증만, 노드 id 없음)로는 피어가 되지 못한다.
    let member = tls_for(&ca, nodes[0].id());
    let (api_pem, api_key) = api_cert(&ca, &member.cert_pem, &[]).unwrap();
    let api = Peers::new(&NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: api_pem,
        key_pem: api_key,
    });
    assert!(api.ping(&nodes[0].addr, WAIT).await.is_err());
}

/// 시험이 실패해도 따로 만든 실행기를 끝낸다(async 문맥에서 그냥 놓으면 panic한다).
struct Background(Option<tokio::runtime::Runtime>);

impl Background {
    fn new() -> Self {
        Self::of(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        )
    }

    fn of(rt: tokio::runtime::Runtime) -> Self {
        Self(Some(rt))
    }

    fn handle(&self) -> &tokio::runtime::Handle {
        self.0.as_ref().unwrap().handle()
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            rt.shutdown_background();
        }
    }
}

/// 실행기를 받은 `Peers`의 control 채널은 처음 연 실행기가 끝나도 된다(채널의 연결 작업이 받은
/// control 실행기에서 돈다). 실행기를 주지 않으면 처음 연 실행기와 함께 채널이 멈춘다.
#[tokio::test]
async fn control_channels_outlive_the_runtime_that_opened_them() {
    let (ca, _root, nodes) = start(1).await;
    let control = tokio::runtime::Handle::current();
    let bulk = Background::new();
    let peers = Arc::new(
        Peers::new(&tls_for(&ca, nodes[0].id())).with_runtimes(control, bulk.handle().clone()),
    );
    let addr = nodes[0].addr.clone();

    // 따로 만든 실행기(work·api에 해당)가 먼저 채널을 열고 끝난다.
    let first = Background::new();
    let opened = {
        let (peers, addr) = (peers.clone(), addr.clone());
        first
            .handle()
            .spawn(async move { peers.ping(&addr, WAIT).await })
    };
    assert_eq!(opened.await.unwrap().unwrap(), nodes[0].id());
    drop(first);

    assert_eq!(peers.ping(&addr, WAIT).await.unwrap(), nodes[0].id());
}

#[tokio::test]
async fn a_new_node_joins_as_learner_then_voter() {
    let (ca, root, mut nodes) = start(1).await;
    let rev = nodes[0].cluster.create(&volume("before")).await.unwrap();

    let new = Node::start(&ca, &root).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: new.id(),
            addr: new.addr.clone(),
        })
        .await
        .unwrap();
    new.cluster
        .raft()
        .unwrap()
        .wait(Some(WAIT))
        .applied_index_at_least(Some(rev), "learner caught up")
        .await
        .unwrap();
    // 새 노드가 스스로 승격을 요청한다(리더로 전달된다).
    new.cluster
        .request(LeaderRequest::AddVoter { id: new.id() })
        .await
        .unwrap();
    let members = new.cluster.members();
    assert_eq!(members.voters.len(), 2);
    assert!(members.voters.contains_key(&new.id()));
    assert!(
        new.cluster
            .get_by_name::<VolumeSpec>("before")
            .unwrap()
            .is_some()
    );
    nodes.push(new);
    // 새 노드에서 한 쓰기도 커밋된다(이제 과반수는 2).
    nodes[1].cluster.create(&volume("after")).await.unwrap();
}

#[tokio::test]
async fn console_is_relayed_to_the_node_running_the_vm() {
    let (_ca, _root, nodes) = start(2).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let target = &nodes[1];
    target.hv.set_console_dir(tmp.path());
    let vm = Uuid::now_v7();
    let spec: VmRuntimeSpec = serde_json::from_value(serde_json::json!({
        "id": vm, "name": "web", "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256,
        "firmware": "uefi", "machine": "virt", "disks": [], "nics": []
    }))
    .unwrap();
    target.hv.start(&spec, None).await.unwrap();
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{vm}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    let mut console = nodes[0].peers.console(&target.addr, vm).await.unwrap();
    assert_eq!(&console.recv().await.unwrap()[..], b"RFB 003.008\n");
    assert!(console.send(bytes::Bytes::from_static(b"ping")).await);
    assert_eq!(&console.recv().await.unwrap()[..], b"ping");

    // 그 노드에서 돌지 않는 VM은 거부한다.
    let err = nodes[0]
        .peers
        .console(&nodes[0].addr, vm)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, opl_peer::ConsoleError::NotRunning), "{err}");
}

#[tokio::test]
async fn leader_records_when_nodes_go_offline_and_come_back() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        r.id = node.id().0;
        // 하나는 오프라인으로 시작해, 응답하면 온라인이 되는지 본다.
        r.status.online = node.id() == leader;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let online = |id: NodeId| {
        leader_cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };
    let wait_for = |want: bool, id: NodeId| {
        let online = &online;
        async move {
            let deadline = Instant::now() + WAIT;
            while online(id) != want {
                assert!(
                    Instant::now() < deadline,
                    "node {id} never became online={want}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let follower_id = nodes[follower].id();
    wait_for(true, follower_id).await;
    nodes[follower].stop().await;
    wait_for(false, follower_id).await;
    assert!(online(leader));
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn new_leader_does_not_revive_nodes_recorded_offline() {
    let (_ca, _root, mut nodes) = start(5).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        r.id = node.id().0;
        r.status.online = true;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let online = |cluster: &Cluster, id: NodeId| {
        cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };

    // 팔로워 하나를 멈추고 리더가 오프라인으로 기록할 때까지 기다린다.
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let mut dead = nodes.remove(follower);
    let dead_id = dead.id();
    dead.stop().await;
    let old = nodes.iter().position(|n| n.id() == leader).unwrap();
    let deadline = Instant::now() + WAIT;
    while online(&nodes[old].cluster, dead_id) {
        assert!(
            Instant::now() < deadline,
            "dead follower never went offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 리더도 멈추면 새 리더가 그 기록을 다시 온라인으로 바꾸지 않아야 한다.
    let mut old = nodes.remove(old);
    old.stop().await;
    let new_leader = wait_leader(&nodes).await;
    let new_leader = nodes.iter().find(|n| n.id() == new_leader).unwrap();
    // 새 리더가 이전 커밋을 모두 적용한 뒤부터 본다. openraft는 리더 확인 RPC를 하트비트 간격
    // (50ms)만 기다리고, 노드 둘이 죽어 남는 여유가 없어 CPU가 바쁘면 한 번에 실패할 수 있다.
    // 운영 코드(노드 감시, 예약 작업)처럼 다시 한다.
    let deadline = Instant::now() + WAIT;
    while let Err(e) = new_leader.cluster.read_barrier().await {
        assert!(
            Instant::now() < deadline,
            "read barrier never succeeded: {e}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 감시 간격보다 길고 `offline_after`보다 짧게 지켜본다.
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert!(
            !online(&new_leader.cluster, dead_id),
            "new leader revived a node recorded offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for m in monitors {
        m.shutdown().await;
    }
}

/// 노드 리소스를 기록한다. `lease`면 그 노드가 [`LEASE_LIVENESS`]를 안다고 적는다.
async fn register_nodes(
    nodes: &[Node],
    online: impl Fn(NodeId) -> bool,
    lease: impl Fn(usize) -> bool,
) {
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        r.id = node.id().0;
        r.status.online = online(node.id());
        if lease(i) {
            r.status.features = vec![LEASE_LIVENESS.into()];
        }
        nodes[0].cluster.create(&r).await.unwrap();
    }
}

fn recorded_online(cluster: &Cluster, id: NodeId) -> bool {
    cluster
        .get::<NodeSpec>(id.0)
        .unwrap()
        .unwrap()
        .status
        .online
}

async fn wait_online(cluster: &Cluster, id: NodeId, want: bool) {
    let deadline = Instant::now() + WAIT;
    while recorded_online(cluster, id) != want {
        assert!(
            Instant::now() < deadline,
            "node {id} never became online={want}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const LIVENESS_RENEW: Duration = Duration::from_millis(200);

/// 모든 노드가 리스로 생존을 알리면 리더는 ping이 아니라 리스를 본다: 핑에 답하는 노드도 갱신을 멈추면
/// 오프라인, 다시 갱신하면 온라인이다. 펜싱 없이 생존 표시로만 갱신하는 노드도 온라인으로 남는다.
#[tokio::test]
async fn with_lease_liveness_the_leader_follows_renewals_not_pings() {
    let (_ca, _root, nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    // 팔로워 하나는 오프라인으로 시작해, 갱신하면 온라인이 되는지 본다.
    register_nodes(&nodes, |id| id == leader, |_| true).await;
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let mut renewers: Vec<_> = nodes
        .iter()
        .map(|n| renew_liveness(n.cluster.clone(), LIVENESS_RENEW))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let follower_id = nodes[follower].id();
    let other = nodes
        .iter()
        .position(|n| n.id() != leader && n.id() != follower_id)
        .unwrap();
    wait_online(&leader_cluster, follower_id, true).await;

    // 노드는 그대로 돌고(핑에 답한다) 갱신만 멈춘다.
    renewers[follower].abort();
    wait_online(&leader_cluster, follower_id, false).await;
    assert!(recorded_online(&leader_cluster, nodes[other].id()));
    renewers[follower] = renew_liveness(nodes[follower].cluster.clone(), LIVENESS_RENEW);
    wait_online(&leader_cluster, follower_id, true).await;

    // 정상 종료처럼 갱신을 멈추고 스스로 오프라인으로 기록하면, 리스가 아직 살아 있어도 다시 온라인이
    // 되지 않는다.
    renewers[follower].abort();
    let mut status = nodes[follower]
        .cluster
        .get::<NodeSpec>(follower_id.0)
        .unwrap()
        .unwrap()
        .status;
    status.online = false;
    nodes[follower]
        .cluster
        .update_status::<NodeSpec>(follower_id.0, &status)
        .await
        .unwrap();
    wait_online(&leader_cluster, follower_id, false).await;
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert!(
            !recorded_online(&leader_cluster, follower_id),
            "a node that marked itself offline came back on an old renewal"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    renewers[follower] = renew_liveness(nodes[follower].cluster.clone(), LIVENESS_RENEW);
    wait_online(&leader_cluster, follower_id, true).await;
    assert!(recorded_online(&leader_cluster, leader));

    for r in renewers {
        r.abort();
    }
    for m in monitors {
        m.shutdown().await;
    }
}

/// 리스 생존을 모르는 옛 노드가 남았으면 지금처럼 ping으로 본다: 아무도 갱신하지 않아도 핑에 답하는
/// 노드는 온라인으로 남고, 멈춘 노드는 오프라인이 된다.
#[tokio::test]
async fn a_cluster_with_an_old_node_keeps_pinging() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    let old = nodes.iter().position(|n| n.id() != leader).unwrap();
    register_nodes(&nodes, |_| true, |i| i != old).await;
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    // `offline_after`보다 오래 지켜본다(리스로 보면 갱신이 없어 오프라인이 된다).
    let until = Instant::now() + Duration::from_secs(4);
    while Instant::now() < until {
        for n in &nodes {
            assert!(
                recorded_online(&leader_cluster, n.id()),
                "node {} went offline without a lease in a mixed cluster",
                n.id()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stopped = nodes.iter().position(|n| n.id() != leader).unwrap();
    let stopped_id = nodes[stopped].id();
    nodes[stopped].stop().await;
    wait_online(&leader_cluster, stopped_id, false).await;
    for m in monitors {
        m.shutdown().await;
    }
}

/// 멤버가 아닌 노드의 남은 기록(클러스터에서 빠진 옛 노드)은 리스 생존을 몰라도 리스 감시를 막지 않는다: 핑에
/// 답하는 노드도 갱신을 멈추면 오프라인이 된다.
#[tokio::test]
async fn a_leftover_record_of_a_removed_node_does_not_keep_pinging() {
    let (_ca, _root, nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    register_nodes(&nodes, |_| true, |_| true).await;
    let gone: Resource<NodeSpec, NodeStatus> = Resource::new(
        "gone",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    nodes[0].cluster.create(&gone).await.unwrap();
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let renewers: Vec<_> = nodes
        .iter()
        .map(|n| renew_liveness(n.cluster.clone(), LIVENESS_RENEW))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    // 노드는 그대로 돌고(핑에 답한다) 갱신만 멈춘다. ping으로 보면 온라인으로 남는다.
    renewers[follower].abort();
    wait_online(&leader_cluster, nodes[follower].id(), false).await;
    for r in renewers {
        r.abort();
    }
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn migration_requests_reach_the_receiver_of_a_member() {
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(FakeReceiver::default());
    let target = Node::start_with(&ca, &root, Some(receiver.clone()), None, None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    // 대상 노드가 멤버십을 받아 원본 노드를 멤버로 알 때까지 기다린다.
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (vm, task) = (Uuid::now_v7(), Uuid::now_v7());
    let peers = &nodes[0].peers;
    let req = |version: &str| MigrateIncomingRequest {
        vm,
        task,
        qemu_version: version.into(),
        disks: vec![],
    };
    assert_eq!(
        peers
            .migrate_incoming(&target.addr, req("8.2.2"))
            .await
            .unwrap(),
        MigrateIncomingResponse {
            port: 60007,
            nbd_port: None
        }
    );
    // 복사할 디스크는 요청에 실려 가고 NBD 포트가 돌아온다.
    let disk = IncomingDisk {
        volume: Uuid::now_v7(),
        size_bytes: 1 << 30,
    };
    let reply = peers
        .migrate_incoming(
            &target.addr,
            MigrateIncomingRequest {
                disks: vec![disk],
                ..req("8.2.2")
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.nbd_port, Some(60008));
    // 받는 쪽의 거절과 실패는 그 종류 그대로 온다.
    let err = peers
        .migrate_incoming(&target.addr, req("9.0.0"))
        .await
        .unwrap_err();
    assert!(matches!(err, IncomingError::Incompatible(_)), "{err:?}");
    let migration = IncomingMigration { vm, task };
    let err = peers
        .migrate_incoming_wait(&target.addr, migration)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        IncomingError::Failed("the receiving QEMU exited".into())
    );
    peers
        .migrate_incoming_abort(&target.addr, migration)
        .await
        .unwrap();
    assert_eq!(
        *receiver.calls.lock().unwrap(),
        [
            format!("incoming {vm} {task} 8.2.2 []"),
            format!("incoming {vm} {task} 8.2.2 {:?}", [disk]),
            format!("incoming {vm} {task} 9.0.0 []"),
            format!("wait {vm} {task}"),
            format!("abort {vm} {task}"),
        ]
    );

    // 받는 처리가 없는 노드는 거부한다.
    let err = target
        .peers
        .migrate_incoming_abort(&nodes[0].addr, migration)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("does not receive migrations")),
        "{err:?}"
    );
    // 멤버가 아닌 노드의 요청은 받는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .migrate_incoming(&target.addr, req("8.2.2"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert_eq!(receiver.calls.lock().unwrap().len(), 5);
}

/// 받는 처리가 돈 스레드와, 그 처리가 띄운 작업(받은 VM의 QMP·모니터에 해당)이 돈 스레드를 적는다.
#[derive(Default)]
struct ThreadReceiver {
    threads: std::sync::Mutex<Vec<String>>,
}

fn thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or_default()
        .to_string()
}

#[async_trait::async_trait]
impl MigrationReceiver for ThreadReceiver {
    async fn incoming(
        &self,
        _req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        let spawned = tokio::spawn(async { thread_name() }).await.unwrap();
        self.threads
            .lock()
            .unwrap()
            .extend([thread_name(), spawned]);
        Ok(MigrateIncomingResponse {
            port: 60007,
            nbd_port: None,
        })
    }

    async fn wait(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }

    async fn abort(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }
}

/// `work` 실행기를 받은 노드 간 서버는 마이그레이션 받기를 그 실행기에서 돌린다. 받는 처리가 띄운
/// 작업도 그 실행기에 생긴다(노드 간 서버의 실행기에는 Raft·리스만 남는다).
#[tokio::test]
async fn incoming_migrations_run_on_the_work_runtime() {
    let work = Background::of(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("peer-work")
            .enable_all()
            .build()
            .unwrap(),
    );
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(ThreadReceiver::default());
    let target = Node::start_with(
        &ca,
        &root,
        Some(receiver.clone()),
        None,
        None,
        Some(work.handle().clone()),
    )
    .await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(nodes[0].id()) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let req = MigrateIncomingRequest {
        vm: Uuid::now_v7(),
        task: Uuid::now_v7(),
        qemu_version: "8.2.2".into(),
        disks: vec![],
    };
    nodes[0]
        .peers
        .migrate_incoming(&target.addr, req)
        .await
        .unwrap();
    assert_eq!(
        *receiver.threads.lock().unwrap(),
        ["peer-work", "peer-work"]
    );
}

/// 받는 처리가 panic한다.
struct PanickingReceiver;

#[async_trait::async_trait]
impl MigrationReceiver for PanickingReceiver {
    async fn incoming(
        &self,
        _req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        panic!("receiver bug");
    }

    async fn wait(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }

    async fn abort(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }
}

/// `work` 실행기에서 돈 처리가 panic하면 그 요청만 `internal`로 답하고 서버는 계속 답한다.
#[tokio::test]
async fn a_panicking_handler_answers_internal() {
    let work = Background::new();
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let node = Node::start_with(
        &ca,
        &root,
        Some(Arc::new(PanickingReceiver)),
        None,
        None,
        Some(work.handle().clone()),
    )
    .await;
    let members = BTreeMap::from([(node.id(), BasicNode::new(&node.addr))]);
    node.cluster.initialize(members).await.unwrap();
    let nodes = [node];
    wait_leader(&nodes).await;

    let req = MigrateIncomingRequest {
        vm: Uuid::now_v7(),
        task: Uuid::now_v7(),
        qemu_version: "8.2.2".into(),
        disks: vec![],
    };
    let err = nodes[0]
        .peers
        .migrate_incoming(&nodes[0].addr, req)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("panicked")),
        "{err:?}"
    );
    assert_eq!(
        nodes[0].peers.ping(&nodes[0].addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_a_member() {
    let (ca, root, nodes) = start(2).await;
    let (from, to) = (&nodes[1], &nodes[0]);
    let vm = Uuid::now_v7();
    // 데이터 조각, 0만 있는 조각 둘, 끝이 짧은 조각(마지막 바이트만 0이 아님)
    let chunk = opl_peer::firmware::CHUNK_BYTES;
    let mut vars: Vec<u8> = (0..chunk).map(|i| (i % 251) as u8 + 1).collect();
    vars.resize(3 * chunk + 100, 0);
    *vars.last_mut().unwrap() = 0xaa;
    let src = uefi_vars_path(&from.state_dir, vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, &vars).unwrap();

    // 받는 노드의 옛 파일을 바꾼다.
    let dest = uefi_vars_path(&to.state_dir, vm);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, b"stale").unwrap();
    let len = to
        .peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(len, vars.len() as u64);
    assert!(std::fs::read(&dest).unwrap() == vars, "content differs");

    // 0만 있는 파일, 빈 파일도 크기 그대로 온다.
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(
        to.peers
            .uefi_vars(&from.addr, vm, &dest, None)
            .await
            .unwrap(),
        0
    );
    assert!(std::fs::read(&dest).unwrap().is_empty());

    // 그 노드에 파일이 없으면 NotFound이고 받는 쪽 파일은 그대로다.
    std::fs::write(&dest, b"kept").unwrap();
    let err = to
        .peers
        .uefi_vars(&from.addr, Uuid::now_v7(), &dest, None)
        .await
        .unwrap_err();
    assert_eq!(err, UefiVarsError::NotFound);
    assert_eq!(std::fs::read(&dest).unwrap(), b"kept");

    // 멤버가 아닌 노드에는 보내지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .uefi_vars(&from.addr, vm, &root.path().join("outsider.fd"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, UefiVarsError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert!(!root.path().join("outsider.fd").exists());
}

#[tokio::test]
async fn audit_logs_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let audit = Arc::new(FakeAudit::default());
    let target = Node::start_with(&ca, &root, None, Some(audit.clone()), None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |limit, user: Option<&str>| AuditQuery {
        since: 10,
        until: 20_000,
        user: user.map(Into::into),
        action: Some("/vms".into()),
        limit,
    };
    let peers = &nodes[0].peers;
    let entries = peers
        .read_audit(&target.addr, query(7, None), WAIT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "POST /vms");
    // 너무 큰 limit은 받는 쪽이 줄인다.
    peers
        .read_audit(&target.addr, query(1_000_000, None), WAIT)
        .await
        .unwrap();
    assert_eq!(
        *audit.queries.lock().unwrap(),
        [query(7, None), query(AuditQuery::MAX_LIMIT, None)]
    );
    // 읽기 실패는 오류로 온다.
    let err = peers
        .read_audit(&target.addr, query(7, Some("broken")), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Internal, "{err:?}");

    // 감사 로그가 없는 노드는 거부한다.
    let err = target
        .peers
        .read_audit(&nodes[0].addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented, "{err:?}");
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_audit(&target.addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(audit.queries.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn metrics_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let metrics = Arc::new(FakeMetrics::default());
    let target = Node::start_with(&ca, &root, None, None, Some(metrics.clone()), None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |since| MetricsQuery {
        scope: MetricsScope::Nodes,
        since,
    };
    let peers = &nodes[0].peers;
    let reply = peers
        .read_metrics(&target.addr, query(Some(1_000)), WAIT)
        .await
        .unwrap()
        .unwrap();
    let samples = reply.node.unwrap().samples;
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].t, 2_000);
    assert_eq!(samples[0].cpu, Some(12.5));
    assert_eq!(*metrics.queries.lock().unwrap(), [query(Some(1_000))]);

    // 지표 소스가 없는 노드(옛 노드처럼)는 None이다.
    let none = target
        .peers
        .read_metrics(&nodes[0].addr, query(None), WAIT)
        .await
        .unwrap();
    assert_eq!(none, None);
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_metrics(&target.addr, query(None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(metrics.queries.lock().unwrap().len(), 1);
}
