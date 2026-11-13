//! 실제 gRPC + mTLS로 잇는 노드들(한 프로세스 안, 127.0.0.1의 빈 포트).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::VoteRequest;
use openraft::{ServerState, Vote};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, FollowClient, FollowFrame, Forwarder, NodeRole};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
use opl_peer::pki::{ClusterCa, NodeTls, api_cert, node_key_and_csr};
use opl_peer::{
    AuditSource, GrpcNetwork, IncomingDisk, IncomingError, IncomingMigration, LEASE_LIVENESS,
    MetricsReply, MetricsSource, MigrateIncomingRequest, MigrateIncomingResponse,
    MigrationReceiver, NodeMonitor, NodeMonitorConfig, PeerService, Peers, UefiVarsError,
    renew_liveness,
};
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};
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
        .map(|n| (n.id(), OrbNode::single(&n.addr)))
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
    assert!(
        stranger
            .ping(nodes[0].id(), &nodes[0].addr, WAIT)
            .await
            .is_err()
    );
    // 같은 CA라도 멤버가 아니면 거부한다(제거된 노드).
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .ping(nodes[0].id(), &nodes[0].addr, WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err = outsider
        .peers
        .leader_request(&nodes[0].addr, LeaderRequest::Remove { id: nodes[0].id() })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");
    // 멤버는 된다.
    assert_eq!(
        nodes[0]
            .peers
            .ping(nodes[0].id(), &nodes[0].addr, WAIT)
            .await
            .unwrap(),
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
    assert!(api.ping(nodes[0].id(), &nodes[0].addr, WAIT).await.is_err());
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
        let (peers, addr, id) = (peers.clone(), addr.clone(), nodes[0].id());
        first
            .handle()
            .spawn(async move { peers.ping(id, &addr, WAIT).await })
    };
    assert_eq!(opened.await.unwrap().unwrap(), nodes[0].id());
    drop(first);

    assert_eq!(
        peers.ping(nodes[0].id(), &addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
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
            addrs: vec![],
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
            addrs: vec![],
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
            addrs: vec![],
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
    let members = BTreeMap::from([(node.id(), OrbNode::single(&node.addr))]);
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
        nodes[0]
            .peers
            .ping(nodes[0].id(), &nodes[0].addr, WAIT)
            .await
            .unwrap(),
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
        .uefi_vars(from.id(), &from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(len, vars.len() as u64);
    assert!(std::fs::read(&dest).unwrap() == vars, "content differs");

    // 0만 있는 파일, 빈 파일도 크기 그대로 온다.
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers
        .uefi_vars(from.id(), &from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(
        to.peers
            .uefi_vars(from.id(), &from.addr, vm, &dest, None)
            .await
            .unwrap(),
        0
    );
    assert!(std::fs::read(&dest).unwrap().is_empty());

    // 그 노드에 파일이 없으면 NotFound이고 받는 쪽 파일은 그대로다.
    std::fs::write(&dest, b"kept").unwrap();
    let err = to
        .peers
        .uefi_vars(from.id(), &from.addr, Uuid::now_v7(), &dest, None)
        .await
        .unwrap_err();
    assert_eq!(err, UefiVarsError::NotFound);
    assert_eq!(std::fs::read(&dest).unwrap(), b"kept");

    // 멤버가 아닌 노드에는 보내지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .uefi_vars(
            from.id(),
            &from.addr,
            vm,
            &root.path().join("outsider.fd"),
            None,
        )
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
            addrs: vec![],
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
        .read_audit(target.id(), &target.addr, query(7, None), WAIT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "POST /vms");
    // 너무 큰 limit은 받는 쪽이 줄인다.
    peers
        .read_audit(target.id(), &target.addr, query(1_000_000, None), WAIT)
        .await
        .unwrap();
    assert_eq!(
        *audit.queries.lock().unwrap(),
        [query(7, None), query(AuditQuery::MAX_LIMIT, None)]
    );
    // 읽기 실패는 오류로 온다.
    let err = peers
        .read_audit(target.id(), &target.addr, query(7, Some("broken")), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Internal, "{err:?}");

    // 감사 로그가 없는 노드는 거부한다.
    let err = target
        .peers
        .read_audit(nodes[0].id(), &nodes[0].addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented, "{err:?}");
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_audit(target.id(), &target.addr, query(7, None), WAIT)
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
            addrs: vec![],
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
        .read_metrics(target.id(), &target.addr, query(Some(1_000)), WAIT)
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
        .read_metrics(nodes[0].id(), &nodes[0].addr, query(None), WAIT)
        .await
        .unwrap();
    assert_eq!(none, None);
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_metrics(target.id(), &target.addr, query(None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(metrics.queries.lock().unwrap().len(), 1);
}

/// 리더가 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`)을 `observers`(id, 클러스터 주소)로 바꾼다(없으면
/// 정보를 만든다).
async fn set_observers(leader: &Node, observers: &[(NodeId, String)]) {
    let status = ClusterStatus {
        observers: observers
            .iter()
            .map(|(id, addr)| (id.0, addr.clone()))
            .collect(),
    };
    match leader.cluster.list::<ClusterSpec>().unwrap().1.first() {
        Some(info) => {
            leader
                .cluster
                .update_status::<ClusterSpec>(info.id, &status)
                .await
                .unwrap();
        }
        None => {
            let mut info: ClusterInfo = Resource::new(
                "test",
                ClusterSpec {
                    ca_cert_pem: String::new(),
                    ca_key_pem: String::new(),
                },
            );
            info.status = status;
            leader.cluster.create(&info).await.unwrap();
        }
    }
}

/// 관찰 노드를 띄운다. 실제 길처럼 learner로 가입해 따라잡은 뒤 관찰 노드 목록에 적히고 멤버십에서 빠진다. 같은
/// 저장소로 관찰 노드로 다시 열려 gRPC Follow로 투표 멤버를 따르고, 쓰기는 gRPC로 리더에 전달한다.
async fn start_observer(ca: &ClusterCa, root: &TempDir, leader: &Node) -> Node {
    let mut learner = Node::start(ca, root).await;
    let id = learner.id();
    let joined = leader
        .cluster
        .request(LeaderRequest::AddLearner {
            id,
            addr: learner.addr.clone(),
            addrs: vec![],
        })
        .await
        .unwrap();
    // `AddLearner`는 learner가 받기를 기다리지 않는다. 자기를 더한 멤버십까지 적용하기 전에 멈추면 관찰 노드의
    // 저장소에 따를 투표 멤버가 없다(느린 CI에서 그랬다).
    learner.cluster.wait_revision(joined, WAIT).await.unwrap();
    learner.stop().await;
    drop(learner);

    let tls = tls_for(ca, id);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    // 목록에 적은 뒤 멤버십에서 뺀다(그 사이에도 거절되지 않게). 리더의 키퍼처럼 `RemoveNodes`로 뺀다
    // (`LeaderRequest::Remove`는 노드를 지우는 것이라 목록에서도 뺀다).
    set_observers(leader, &[(id, addr.clone())]).await;
    leader
        .cluster
        .raft()
        .unwrap()
        .change_membership(openraft::ChangeMembers::RemoveNodes([id].into()), false)
        .await
        .unwrap();

    let peers = Arc::new(Peers::new(&tls));
    let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
    config.write_timeout = WAIT;
    // learner의 저장소 작업이 아직 파일을 쥐고 있으면 잠시 뒤 다시 연다.
    let deadline = Instant::now() + WAIT;
    let cluster = loop {
        match Cluster::open_observer(
            config.clone(),
            GrpcNetwork::new(peers.clone()),
            peers.clone(),
        )
        .await
        {
            Err(opl_cluster::ClusterError::Store(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => break other.unwrap(),
        }
    };
    let cluster = Arc::new(cluster.with_forwarder(peers.clone()));
    let hv = Arc::new(MockHypervisor::default());
    let service = PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>));
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
        state_dir: root.path().join(format!("{id}-state")),
        stop: Some(stop),
        server: Some(server),
    }
}

async fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 관찰 노드는 mTLS gRPC Follow로 투표 멤버의 상태를 받아 읽기에 답하고, 쓰기는 리더로 전달해 자기 쓰기를 바로 읽는다.
#[tokio::test]
async fn an_observer_follows_a_voter_over_grpc() {
    let (ca, root, nodes) = start(1).await;
    let before = volume("before");
    nodes[0].cluster.create(&before).await.unwrap();
    let observer = start_observer(&ca, &root, &nodes[0]).await;
    let o = &observer.cluster;
    assert!(o.raft().is_none());
    o.wait_revision(nodes[0].cluster.revision(), WAIT)
        .await
        .unwrap();
    assert!(o.get::<VolumeSpec>(before.id).unwrap().is_some());

    let after = volume("after");
    let revision = nodes[0].cluster.create(&after).await.unwrap();
    o.wait_revision(revision, WAIT).await.unwrap();
    assert!(o.get::<VolumeSpec>(after.id).unwrap().is_some());

    let mine = volume("mine");
    let revision = o.create(&mine).await.unwrap();
    assert!(o.revision() >= revision);
    assert!(o.get::<VolumeSpec>(mine.id).unwrap().is_some());
    assert!(
        nodes[0]
            .cluster
            .get::<VolumeSpec>(mine.id)
            .unwrap()
            .is_some()
    );
    o.renew_lease(WAIT).await.unwrap();
    wait_for("the observer knows the leader", || o.health().quorum).await;
    assert_eq!(o.members().leader, Some(nodes[0].id()));
    // 관찰 노드끼리도, 투표 멤버도 관찰 노드를 클러스터 노드로 안다(주소 찾기).
    wait_for("the leader lists the observer", || {
        nodes[0].cluster.members().addr(observer.id()) == Some(observer.addr.as_str())
    })
    .await;
    assert_eq!(
        o.members().addr(observer.id()),
        Some(observer.addr.as_str())
    );
}

/// 관찰 노드 목록에 든 노드는 피어로 받고, 목록에서도 빠진 노드는 거절한다. 관찰 노드는 Raft RPC를 받지 않고,
/// 관찰 노드에 건 Follow는 `Hello`만 오고 닫힌다.
#[tokio::test]
async fn observers_are_peers_but_not_raft_members() {
    let (ca, root, nodes) = start(1).await;
    let observer = start_observer(&ca, &root, &nodes[0]).await;
    let leader_addr = nodes[0].addr.clone();
    assert_eq!(
        observer
            .peers
            .ping(nodes[0].id(), &leader_addr, WAIT)
            .await
            .unwrap(),
        nodes[0].id()
    );
    assert_eq!(
        nodes[0]
            .peers
            .ping(observer.id(), &observer.addr, WAIT)
            .await
            .unwrap(),
        observer.id()
    );

    // 관찰 노드에 온 Raft RPC
    let mut raft = GrpcNetwork::new(nodes[0].peers.clone())
        .new_client(observer.id(), &OrbNode::single(&observer.addr))
        .await;
    let err = raft
        .vote(
            VoteRequest::new(Vote::new(1, nodes[0].id()), None),
            RPCOption::new(WAIT),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a Raft member"), "{err}");

    // 관찰 노드에 건 Follow: 부른 쪽(투표 멤버)의 역할만 알리고 닫는다.
    let frames: Vec<FollowFrame> =
        FollowClient::follow(nodes[0].peers.as_ref(), observer.id(), &observer.addr, 0)
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
    assert!(
        matches!(
            frames.as_slice(),
            [FollowFrame::Hello {
                role: NodeRole::Voter,
                ..
            }]
        ),
        "{frames:?}"
    );

    // 목록에서 빼면(지운 노드) 거절한다.
    set_observers(&nodes[0], &[]).await;
    wait_for("the observer is unlisted", || {
        !nodes[0].cluster.members().contains(observer.id())
    })
    .await;
    let err = observer
        .peers
        .ping(nodes[0].id(), &leader_addr, WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err =
        match FollowClient::follow(observer.peers.as_ref(), nodes[0].id(), &leader_addr, 0).await {
            Ok(_) => panic!("a removed node may follow"),
            Err(e) => e,
        };
    assert!(err.to_string().contains("not a member"), "{err}");
}

/// 열린 Follow 스트림도 다시 허가한다: 관찰 노드 목록과 멤버십 모두에서 빠진 노드(지운 노드)의 스트림은 끊긴다(그
/// 뒤의 쓰기, CA 키 같은 비밀을 받지 못한다). 목록 캐시가 갱신되기를 기다리지 않고 곧바로 써도, 목록에서 빼는 묶음부터
/// 받지 못한다.
#[tokio::test]
async fn a_removed_observer_stops_receiving_the_feed() {
    let (ca, root, nodes) = start(1).await;
    let observer = start_observer(&ca, &root, &nodes[0]).await;
    let mut frames = FollowClient::follow(
        observer.peers.as_ref(),
        nodes[0].id(),
        &nodes[0].addr,
        nodes[0].cluster.revision(),
    )
    .await
    .unwrap();
    assert!(matches!(
        frames.next().await,
        Some(Ok(FollowFrame::Hello { .. }))
    ));
    assert!(matches!(
        tokio::time::timeout(WAIT, frames.next()).await.unwrap(),
        Some(Ok(FollowFrame::Progress { .. }))
    ));

    set_observers(&nodes[0], &[]).await;
    // 목록에서 뺀 쓰기와 그 뒤에 쓴 것은 받지 못하고 스트림이 끝난다.
    let secret = volume("after-removal");
    nodes[0].cluster.create(&secret).await.unwrap();
    let rest: Vec<_> = tokio::time::timeout(Duration::from_secs(5), frames.collect::<Vec<_>>())
        .await
        .expect("the stream of a removed observer was not closed");
    for frame in rest.into_iter().flatten() {
        assert!(
            frame
                .observers()
                .is_none_or(|o| o.contains_key(&observer.id())),
            "a removed observer received its removal: {frame:?}"
        );
        if let FollowFrame::Batch { events, .. } = frame {
            assert!(
                events.iter().all(|e| !matches!(
                    e,
                    opl_cluster::watch::WatchEvent::Put { resource, .. } if resource.id == secret.id
                )),
                "a removed observer received a later write"
            );
        }
    }
}

/// 닫힌 포트(연결은 곧바로 거절된다)
async fn closed_port() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().to_string()
}

/// 연결을 받고 아무것도 답하지 않는 리스너. 멈추면 받은 연결도 닫는다.
struct Blackhole(tokio::task::JoinHandle<()>);

impl Blackhole {
    fn start(listener: TcpListener) -> Self {
        Self(tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        }))
    }

    /// 리스너와 받은 연결을 닫는다(그 포트에 다시 bind할 수 있다).
    async fn stop(self) {
        self.0.abort();
        let _ = self.0.await;
    }
}

/// 받고 답하지 않는 첫 링크로 보낸 짧은 호출(Raft 하트비트처럼 200 ms 제한 시간)은 연속 두 번 넘기면 1초 안에
/// 둘째 링크로 간다(선거 제한 시간 전에).
#[tokio::test]
async fn short_calls_leave_a_silent_link_within_a_second() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let hole = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = hole.local_addr().unwrap().to_string();
    let hole = Blackhole::start(hole);
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let peers = Peers::new(&tls_for(&ca, id));
    peers.set_link_book(book);

    let started = Instant::now();
    while peers.active_link(&first) == first {
        assert!(started.elapsed() < WAIT);
        let _ = peers.ping(id, &first, Duration::from_millis(200)).await;
    }
    let took = started.elapsed();
    assert!(took < Duration::from_secs(1), "{took:?}");
    assert_eq!(peers.active_link(&first), server);
    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    hole.stop().await;
}

/// 링크가 다른 노드를 가리키면(노드 A의 첫 링크가 B의 주소) 대상 id를 실은 호출(리더 전달·Raft RPC·ping·bulk)을 B가
/// 거절하고, 보내는 쪽은 그것을 링크 실패로 세어 A의 다음 링크로 다시 보낸다. 대상 id가 다른 호출은 처리하지
/// 않는다.
#[tokio::test]
async fn a_link_to_another_node_is_refused_and_skipped() {
    let (ca, _root, nodes) = start(2).await;
    let leader = wait_leader(&nodes).await;
    let a = nodes.iter().find(|n| n.id() == leader).unwrap();
    let b = nodes.iter().find(|n| n.id() != leader).unwrap();
    let wrong = OrbNode::new([b.addr.clone(), a.addr.clone()]);
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&wrong]);
    let peers = Arc::new(Peers::new(&tls_for(&ca, b.id())));
    peers.set_link_book(book.clone());

    let renew = LeaderRequest::RenewLease { node: b.id() };
    Forwarder::forward(peers.as_ref(), a.id(), &b.addr, renew)
        .await
        .unwrap();
    assert_eq!(peers.active_link(&b.addr), a.addr);

    // ping과 bulk 호출(지표 읽기)도 대상 id를 싣는다.
    let pinging = Peers::new(&tls_for(&ca, b.id()));
    pinging.set_link_book(book.clone());
    assert_eq!(pinging.ping(a.id(), &b.addr, WAIT).await.unwrap(), a.id());
    assert_eq!(pinging.active_link(&b.addr), a.addr);
    let reading = Peers::new(&tls_for(&ca, b.id()));
    reading.set_link_book(book);
    let query = MetricsQuery {
        scope: MetricsScope::Nodes,
        since: None,
    };
    let reply = reading.read_metrics(a.id(), &b.addr, query, WAIT).await;
    assert!(reply.unwrap().is_none());
    assert_eq!(reading.active_link(&b.addr), a.addr);

    // Raft RPC도 대상 id를 싣는다(링크 책이 없는 Peers).
    let raft_only = Arc::new(Peers::new(&tls_for(&ca, b.id())));
    let mut conn = GrpcNetwork::new(raft_only.clone())
        .new_client(a.id(), &wrong)
        .await;
    conn.vote(
        VoteRequest::new(Vote::new(0, b.id()), None),
        RPCOption::new(Duration::from_secs(3)),
    )
    .await
    .unwrap();
    assert_eq!(raft_only.active_link(&b.addr), a.addr);

    // 링크가 하나뿐이면 다시 보낼 곳이 없어 거절이 그대로 온다.
    let mut lone = GrpcNetwork::new(raft_only.clone())
        .new_client(a.id(), &OrbNode::single(&b.addr))
        .await;
    let err = lone
        .vote(
            VoteRequest::new(Vote::new(0, b.id()), None),
            RPCOption::new(Duration::from_secs(3)),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("wrong node"), "{err}");
}

/// 연결하지 못한 bulk 호출도 같은 호출 안에서 둘째 링크로 다시 보내고, 시험 호출은 control 채널의 짧은 호출만이라
/// 재시도 간격이 지나도 bulk 호출은 답 없는 우선 링크로 가지 않는다.
#[tokio::test]
async fn bulk_calls_retry_on_the_next_link_and_never_try_the_preferred_one() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let first = closed_port().await;
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let retry = Duration::from_millis(300);
    let peers = Peers::new(&tls_for(&ca, id)).with_link_retry(retry);
    peers.set_link_book(book);

    let query = MetricsQuery {
        scope: MetricsScope::Nodes,
        since: None,
    };
    // 이 노드는 지표를 모른다(옛 노드처럼 None).
    let reply = peers
        .read_metrics(id, &first, query.clone(), WAIT)
        .await
        .unwrap();
    assert!(reply.is_none());
    assert_eq!(peers.active_link(&first), server);

    // 우선 링크가 받고 답하지 않게 되고 재시도 간격이 지나도, bulk 호출은 지금 링크로 곧바로 답을 받는다.
    let hole = Blackhole::start(TcpListener::bind(&first).await.unwrap());
    tokio::time::sleep(retry * 2).await;
    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        peers.read_metrics(id, &first, query, WAIT),
    )
    .await
    .expect("a bulk call went to the silent preferred link");
    assert!(reply.unwrap().is_none());
    hole.stop().await;
}

/// 주소로 쓸 수 없는 링크는 연결 실패로 세어 다음 링크로 넘어가고, 시험 호출도 거기서 끝난다.
#[tokio::test]
async fn a_link_that_is_not_an_address_is_skipped() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let first = "not an address:1".to_string();
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let retry = Duration::from_millis(200);
    let peers = Peers::new(&tls_for(&ca, id)).with_link_retry(retry);
    peers.set_link_book(book);

    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    assert_eq!(peers.active_link(&first), server);
    tokio::time::sleep(retry * 2).await;
    // 시험 호출이 실패해도 다음 링크로 다시 보내 답을 받는다.
    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    assert_eq!(peers.active_link(&first), server);
}

/// 받은 연결을 `to`로 잇는 TCP 중계. 연결 수를 세고, 얼리면(`freeze`) 보내는 쪽의 바이트는 계속 넘기고 받는 쪽으로
/// 오는 답은 버린다(받고 답하지 않는 서버, 연결은 열어 둠).
struct Relay {
    addr: String,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
    frozen: Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Relay {
    async fn start(to: String) -> Self {
        use std::sync::atomic::Ordering;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let frozen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (count, freeze) = (accepted.clone(), frozen.clone());
        let task = tokio::spawn(async move {
            let mut conns = tokio::task::JoinSet::new();
            while let Ok((client, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let (to, freeze) = (to.clone(), freeze.clone());
                conns.spawn(async move {
                    let upstream = tokio::net::TcpStream::connect(&to).await.unwrap();
                    let (mut client_rx, mut client_tx) = client.into_split();
                    let (mut up_rx, mut up_tx) = upstream.into_split();
                    let sending = async move {
                        let _ = tokio::io::copy(&mut client_rx, &mut up_tx).await;
                    };
                    let answering = async move {
                        let mut buf = [0u8; 16 << 10];
                        loop {
                            match up_rx.read(&mut buf).await {
                                Ok(n) if n > 0 => {
                                    if !freeze.load(Ordering::SeqCst)
                                        && client_tx.write_all(&buf[..n]).await.is_err()
                                    {
                                        return;
                                    }
                                }
                                // 얼린 뒤 서버가 닫아도 받는 쪽 연결은 열어 둔다.
                                _ if freeze.load(Ordering::SeqCst) => std::future::pending().await,
                                _ => return,
                            }
                        }
                    };
                    tokio::join!(sending, answering);
                });
            }
        });
        Self {
            addr,
            accepted,
            frozen,
            task,
        }
    }

    fn freeze(&self) {
        self.frozen.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn accepted(&self) -> usize {
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 이미 보낸 쓰기가 연결에서 답을 받지 못하고 `Unavailable`로 끝나면(답하지 않는 연결의 keepalive ping 만료, 약 30초)
/// 받았을 수 있으므로 다른 링크로 다시 보내지 않는다. 링크 선택에는 연결 실패로 세어 다음 호출은 둘째 링크로 간다.
#[tokio::test]
async fn a_sent_write_that_loses_its_connection_is_not_sent_again() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let first = Relay::start(server.clone()).await;
    let second = Relay::start(server).await;
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.addr.clone(), second.addr.clone()])]);
    let peers = Peers::new(&tls_for(&ca, id));
    peers.set_link_book(book);
    assert_eq!(peers.ping(id, &first.addr, WAIT).await.unwrap(), id);

    first.freeze();
    let remove = LeaderRequest::Remove {
        id: NodeId(Uuid::now_v7()),
    };
    let started = Instant::now();
    let err = peers.leader_request(&first.addr, remove).await.unwrap_err();
    assert!(
        matches!(err, opl_cluster::ClusterError::Unreachable(_)),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(60), "{err:?}");
    assert_eq!(second.accepted(), 0, "the write was sent again");
    assert_eq!(first.accepted(), 1);
    assert_eq!(peers.active_link(&first.addr), second.addr);
    peers
        .leader_request(&first.addr, LeaderRequest::RenewLease { node: id })
        .await
        .unwrap();
}

/// 노드의 링크가 `[닫힌 포트, 서버]`면 첫 호출이 연결 실패로 같은 호출 안에서 둘째 링크로 다시 가고, 그 뒤의 ping·
/// 리더 요청·Follow·Raft RPC가 둘째 링크로 간다. Raft는 링크 책 없이도 멤버십의 링크로 넘어가고, 책이 있으면 책의
/// 목록을 쓴다(다른 호출과 같은 선택).
#[tokio::test]
async fn calls_fail_over_to_the_second_link() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let first = closed_port().await;
    let node = OrbNode::new([first.clone(), server.clone()]);
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&node]);
    let peers = Arc::new(Peers::new(&tls_for(&ca, id)));
    peers.set_link_book(book);
    assert_eq!(peers.active_link(&first), first);

    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    assert_eq!(peers.active_link(&first), server);
    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    peers
        .leader_request(&first, LeaderRequest::RenewLease { node: id })
        .await
        .unwrap();
    let mut frames = FollowClient::follow(peers.as_ref(), id, &first, 0)
        .await
        .unwrap();
    assert!(matches!(
        frames.next().await,
        Some(Ok(FollowFrame::Hello { .. }))
    ));

    let vote = || VoteRequest::new(Vote::new(0, id), None);
    let option = || RPCOption::new(Duration::from_secs(3));
    // 책이 있으면 Raft도 책의 링크를 쓴다(로그의 멤버십이 아직 링크 하나여도 지금 링크 그대로).
    let mut conn = GrpcNetwork::new(peers.clone())
        .new_client(id, &OrbNode::single(&first))
        .await;
    conn.vote(vote(), option()).await.unwrap();
    assert_eq!(peers.active_link(&first), server);

    // 링크 책이 없는 Peers의 Raft도 멤버십의 링크로 넘어간다.
    let raft_only = Arc::new(Peers::new(&tls_for(&ca, id)));
    let mut conn = GrpcNetwork::new(raft_only.clone())
        .new_client(id, &node)
        .await;
    conn.vote(vote(), option()).await.unwrap();
    assert_eq!(raft_only.active_link(&first), server);
    conn.vote(vote(), option()).await.unwrap();
}

/// 받고 답하지 않는 우선 링크로 보낸 리스 갱신은 3초 제한 시간에 넘어가고 다음 갱신은 둘째 링크로 성공한다. 우선
/// 링크에 서버가 다시 뜨면 재시도 간격 뒤의 시험 호출로 돌아온다.
#[tokio::test]
async fn a_silent_link_times_out_and_the_preferred_link_comes_back() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let hole = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = hole.local_addr().unwrap().to_string();
    let hole = Blackhole::start(hole);
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let retry = Duration::from_secs(2);
    let peers = Peers::new(&tls_for(&ca, id)).with_link_retry(retry);
    peers.set_link_book(book);

    let started = Instant::now();
    let renew = || LeaderRequest::RenewLease { node: id };
    assert!(peers.leader_request(&first, renew()).await.is_err());
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(2500) && took < Duration::from_secs(10),
        "{took:?}"
    );
    assert_eq!(peers.active_link(&first), server);
    peers.leader_request(&first, renew()).await.unwrap();

    // 우선 링크에 같은 노드의 서버를 띄운다.
    hole.stop().await;
    let listener = TcpListener::bind(&first).await.unwrap();
    let tls = tls_for(&ca, id);
    let service = PeerService::new(nodes[0].cluster.clone(), None);
    let (stop, rx) = oneshot::channel::<()>();
    let revived = tokio::spawn(async move {
        opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    // 재시도 간격 전에는 둘째 링크에 머문다.
    peers.leader_request(&first, renew()).await.unwrap();
    assert_eq!(peers.active_link(&first), server);
    let deadline = Instant::now() + WAIT;
    while peers.active_link(&first) != first {
        assert!(
            Instant::now() < deadline,
            "did not return to the preferred link"
        );
        let _ = peers.leader_request(&first, renew()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(started.elapsed() >= retry);
    peers.leader_request(&first, renew()).await.unwrap();
    let _ = stop.send(());
    revived.await.unwrap();
}
