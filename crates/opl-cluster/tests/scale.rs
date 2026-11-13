//! 규모 시험(설계 §5): 한 프로세스에 노드 여럿(기본 300, `OPL_SCALE_NODES`)을 memnet으로 띄운다. 투표 멤버는
//! 다섯이고 나머지는 learner다. 모든 노드가 2초마다 리스를 갱신하고 `VoterKeeper`를 띄운 채로 리더의 초당
//! RPC 수, 쓰기 지연 p99, 노드 하나 추가, 리더 장애 뒤 새 리더까지를 재고 기준을 넘으면 실패한다.
//!
//! 저장소는 모두 메모리에 둔다. 한 컴퓨터의 디스크 하나에 노드 수백 개(투표 멤버 다섯만이어도)의 fsync가
//! 몰리면 그 줄서기가 잰 값을 차지한다(실제 노드는 디스크가 따로다). 그래서 쓰기 지연은 Raft와 리더의 일만
//! 잰다. 디스크를 넣은 값은 설계 §14에 적었다.
//!
//! `cargo test -p opl-cluster --features scale-it --test scale -- --nocapture`

#![cfg(feature = "scale-it")]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use openraft::error::{InstallSnapshotError, RPCError, RaftError};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, ServerState};
use opl_cluster::memnet::{MemConnection, MemForwarder, MemNetwork, MemRouter};
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{
    Cluster, ClusterConfig, ClusterError, Forwarder, Typed, VOTER_LIMIT, VoterKeeper,
    VoterKeeperConfig, ZONE_LABEL,
};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::VolumeSpec;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// 투표 멤버 수(클러스터 설정 `voters`의 기본값)
const VOTERS: u64 = 5;
/// 리스 갱신 간격(opld `LIVENESS_RENEW`, `HaGuard`)
const RENEW: Duration = Duration::from_secs(2);
/// 쓰기 지연을 재는 쓰기 수
const WRITES: usize = 200;

/// 리더 장애 뒤 새 리더가 자기 임기의 항목을 적용하기까지. 설계는 3초였지만 openraft 0.9.25는 리더가 있던
/// 팔로워가 마지막 하트비트 뒤 리더 리스(`election_timeout_max` 2초)와 선거 제한 시간(1~2초)을 더 기다린 뒤
/// 선거를 시작해 노드 수와 관계없이 3~4초다(노드 여섯에서도 같다). 그 위에 투표·빈 항목 확정과 바쁜 CI의
/// 여유로 1초를 둔다(설계 §14). `OPL_SCALE_FAILOVER_MS`로 바꾼다.
const FAILOVER_MAX_MS: u64 = 5000;
/// 리더 장애 뒤 새 리더를 기다리는 한도. 넘으면 기준과 관계없이 그 자리에서 실패한다.
const FAILOVER_GIVE_UP: Duration = Duration::from_secs(10);
/// 쓰기 지연 p99 기준(ms). 코어가 적은 CI 러너는 `OPL_SCALE_P99_MS`로 바꾼다(설계 §14, README).
const WRITE_P99_MAX_MS: u64 = 50;
const ADD_NODE_MAX: Duration = Duration::from_secs(2);
/// 리더가 노드 하나 때문에 쓰는 초당 RPC 상한: 하트비트 5(200 ms), 리스 갱신 전달 0.5, 그 갱신의 리더 확인을
/// 묶지 못했을 때 2번 × 투표 멤버 넷 = 4. 합 9.5를 10으로 둔다.
const RPCS_PER_NODE_MAX: f64 = 10.0;
/// 하트비트 간격(`raft_config`)
const HEARTBEAT: Duration = Duration::from_millis(200);
/// RPC를 재는 동안 투표 멤버로 가는 `append_entries`(하트비트·리더 확인)에 넣는 지연. 확인 왕복이 늘 갱신이
/// 오는 간격보다 길어(노드 100에서 갱신 50/s, 확인 요청 100/s) 기계·빌드와 관계없이 확인이 묶인다.
const VOTER_RPC_DELAY: Duration = Duration::from_millis(20);
/// 그때 투표 멤버에게 간 RPC가 묶지 않은 최악의 이 비율 아래여야 한다(노드 수와 관계없이, CI의 노드 100에서도).
/// 확인은 한 번에 하나라 초당 많아야 1/지연(50)번이므로 노드 100에서 0.42를 넘을 수 없다(잰 값 0.38, 노드 300은
/// 0.10). 묶지 않게 고치면 노드 100·300 모두 0.79~0.80이었다.
const BATCHED_RATIO_MAX: f64 = 0.6;

/// opld와 같은 Raft 설정(`member.rs` `raft_config`)
fn raft_config() -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

/// 노드별 RPC 수: 그 노드가 투표 멤버·learner에게 보낸 Raft RPC와 리더로서 전달받은 요청(리스 갱신)
#[derive(Default)]
struct Counter {
    to_voters: AtomicU64,
    to_learners: AtomicU64,
    forwarded: AtomicU64,
}

type Counters = Arc<HashMap<NodeId, Counter>>;

/// 보낸 Raft RPC를 세는 네트워크
struct Counting {
    inner: MemNetwork,
    from: NodeId,
    counters: Counters,
    /// 켜면 투표 멤버로 가는 `append_entries`가 [`VOTER_RPC_DELAY`]만큼 늦다.
    slow_voters: Arc<AtomicBool>,
}

impl RaftNetworkFactory<TypeConfig> for Counting {
    type Network = CountingConnection;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> CountingConnection {
        CountingConnection {
            inner: self.inner.new_client(target, node).await,
            from: self.from,
            // 재는 동안 투표 멤버는 처음의 `1..=VOTERS` 그대로다.
            to_voter: (1..=VOTERS).any(|i| NodeId::from(i) == target),
            counters: self.counters.clone(),
            slow_voters: self.slow_voters.clone(),
        }
    }
}

struct CountingConnection {
    inner: MemConnection,
    from: NodeId,
    to_voter: bool,
    counters: Counters,
    slow_voters: Arc<AtomicBool>,
}

impl CountingConnection {
    fn count(&self) {
        let c = &self.counters[&self.from];
        if self.to_voter {
            &c.to_voters
        } else {
            &c.to_learners
        }
        .fetch_add(1, Ordering::Relaxed);
    }
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for CountingConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        self.count();
        if self.to_voter && self.slow_voters.load(Ordering::Relaxed) {
            tokio::time::sleep(VOTER_RPC_DELAY).await;
        }
        self.inner.append_entries(rpc, option).await
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        self.count();
        self.inner.install_snapshot(rpc, option).await
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        self.count();
        self.inner.vote(rpc, option).await
    }
}

/// 리더가 전달받은 요청(리스 갱신)을 세는 전달자
struct CountingForwarder {
    inner: Arc<MemForwarder>,
    counters: Counters,
}

#[async_trait]
impl Forwarder for CountingForwarder {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        if let Some(c) = self.counters.get(&leader) {
            c.forwarded.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.forward(leader, addr, req).await
    }
}

/// 노드 하나: 저장소, 리스 갱신, VoterKeeper
struct Running {
    cluster: Arc<Cluster>,
    renew: JoinHandle<()>,
    keeper: VoterKeeper,
}

struct Harness {
    router: MemRouter,
    counters: Counters,
    nodes: BTreeMap<NodeId, Running>,
    /// 실패한 리스 갱신 수
    renew_failures: Arc<AtomicU64>,
    /// 투표 멤버로 가는 RPC를 늦춘다(RPC를 잴 때만).
    slow_voters: Arc<AtomicBool>,
    /// 노드 수(리스 갱신 시작을 고르게 나눈다)
    total: u64,
}

impl Harness {
    /// 노드 `id`를 연다(아직 멤버십에 없다).
    async fn open(&mut self, i: u64) {
        let id = NodeId::from(i);
        let mut config = ClusterConfig::new(id, format!("node-{i}"));
        config.raft = raft_config();
        config.write_timeout = Duration::from_secs(10);
        config.in_memory = true;
        let network = Counting {
            inner: self.router.network(id),
            from: id,
            counters: self.counters.clone(),
            slow_voters: self.slow_voters.clone(),
        };
        let cluster = Cluster::open(config, network)
            .await
            .unwrap()
            .with_forwarder(Arc::new(CountingForwarder {
                inner: self.router.forwarder(id),
                counters: self.counters.clone(),
            }));
        self.router.register(id, &cluster);
        let cluster = Arc::new(cluster);
        // 노드마다 갱신 시작을 간격 안에서 고르게 늦춘다(실제 노드는 서로 맞춰 갱신하지 않는다).
        let offset =
            RENEW * u32::try_from(i % self.total).unwrap() / u32::try_from(self.total).unwrap();
        let renew = {
            let cluster = cluster.clone();
            let failures = self.renew_failures.clone();
            tokio::spawn(async move {
                tokio::time::sleep(offset).await;
                loop {
                    let sent = Instant::now();
                    if cluster.renew_lease(RENEW * 2).await.is_err() {
                        failures.fetch_add(1, Ordering::Relaxed);
                    }
                    tokio::time::sleep_until(sent + RENEW).await;
                }
            })
        };
        let keeper = VoterKeeper::spawn(cluster.clone(), VoterKeeperConfig::default());
        self.nodes.insert(
            id,
            Running {
                cluster,
                renew,
                keeper,
            },
        );
    }

    fn get(&self, id: NodeId) -> &Cluster {
        &self.nodes[&id].cluster
    }

    /// `ids` 가운데 리더가 되어 자기 임기의 빈 항목까지 적용한 노드
    fn settled_leader(&self, ids: &[NodeId]) -> Option<NodeId> {
        ids.iter().copied().find(|id| {
            let m = self.get(*id).raft().unwrap().metrics().borrow().clone();
            let applied = m.last_applied.map(|l| l.index);
            m.state == ServerState::Leader && applied.is_some() && applied >= m.last_log_index
        })
    }

    async fn wait_leader(&self, ids: &[NodeId], within: Duration) -> NodeId {
        let deadline = Instant::now() + within;
        loop {
            if let Some(leader) = self.settled_leader(ids) {
                return leader;
            }
            assert!(Instant::now() < deadline, "no leader among {ids:?}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// 노드 `i`의 Node 리소스(zone 셋에 고르게, 기능 `voter-limit`)
    async fn register(&self, leader: NodeId, i: u64) {
        let mut node = Resource::new(
            format!("n{i}"),
            NodeSpec {
                address: format!("node-{i}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = NodeId::from(i).0;
        node.labels.insert(ZONE_LABEL.into(), format!("z{}", i % 3));
        node.status = NodeStatus {
            online: true,
            features: vec![VOTER_LIMIT.into()],
            ..NodeStatus::default()
        };
        self.get(leader).create(&node).await.unwrap();
    }

    /// 노드를 멈춘다(프로세스 종료에 해당). 네트워크에서 먼저 뺀다.
    async fn stop(&mut self, id: NodeId) {
        self.router.unregister(id);
        let node = self.nodes.remove(&id).unwrap();
        node.renew.abort();
        node.keeper.shutdown().await;
        node.cluster.shutdown().await.unwrap();
    }

    async fn close(mut self) {
        for id in self.nodes.keys().copied().collect::<Vec<_>>() {
            self.stop(id).await;
        }
    }
}

fn volume(i: usize) -> Typed<VolumeSpec> {
    Resource::new(
        format!("disk-{i}"),
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

/// 리더 확인을 묶지 않았을 때(최악) 리더가 투표 멤버에게 보내는 초당 RPC: 팔로워마다 하트비트와, 리스 갱신(전달받은
/// 것과 리더 자신의 것)마다 확인 2번 × 투표 멤버 수. 확인 하나는 팔로워 넷에게 가지만 투표 멤버 다섯으로 세어 하트비트와
/// 겹친 것·재시도의 여유를 둔다(묶지 않게 고친 빌드가 팔로워 넷으로 센 값을 1% 넘었다).
fn unbatched_to_voters(forwarded: f64) -> f64 {
    let followers = (VOTERS - 1) as f64;
    let renewals = forwarded + 1.0 / RENEW.as_secs_f64();
    followers / HEARTBEAT.as_secs_f64() + renewals * 2.0 * VOTERS as f64
}

/// 쓰기 지연 p99 기준. 기본 50 ms, `OPL_SCALE_P99_MS`로 바꾼다.
fn write_p99_max() -> Duration {
    let ms = std::env::var("OPL_SCALE_P99_MS")
        .ok()
        .map_or(WRITE_P99_MAX_MS, |v| {
            v.parse().expect("OPL_SCALE_P99_MS must be a number")
        });
    Duration::from_millis(ms)
}

/// 리더 장애 뒤 새 리더까지의 기준. 기본 5초, `OPL_SCALE_FAILOVER_MS`로 바꾼다.
fn failover_max() -> Duration {
    let ms = std::env::var("OPL_SCALE_FAILOVER_MS")
        .ok()
        .map_or(FAILOVER_MAX_MS, |v| {
            v.parse().expect("OPL_SCALE_FAILOVER_MS must be a number")
        });
    Duration::from_millis(ms)
}

/// 노드 수. 기본 300, CI는 `OPL_SCALE_NODES=100`.
fn node_count() -> u64 {
    std::env::var("OPL_SCALE_NODES")
        .ok()
        .map(|v| v.parse().expect("OPL_SCALE_NODES must be a number"))
        .unwrap_or(300)
}

/// `window` 동안 리더가 쓴 초당 RPC 수(투표 멤버에게, learner에게, 전달받은 것)
async fn leader_rpcs(c: &Counter, window: Duration) -> [f64; 3] {
    let read = || {
        [
            c.to_voters.load(Ordering::Relaxed),
            c.to_learners.load(Ordering::Relaxed),
            c.forwarded.load(Ordering::Relaxed),
        ]
    };
    let before = read();
    tokio::time::sleep(window).await;
    let after = read();
    std::array::from_fn(|k| (after[k] - before[k]) as f64 / window.as_secs_f64())
}

#[tokio::test(flavor = "multi_thread")]
async fn hundreds_of_nodes_do_not_slow_the_leader() {
    let n = node_count();
    assert!(n > VOTERS, "need more nodes than voters");
    // 나중에 추가할 노드(n + 1)까지 센다.
    let counters: Counters = Arc::new(
        (1..=n + 1)
            .map(|i| (NodeId::from(i), Counter::default()))
            .collect(),
    );
    let mut h = Harness {
        router: MemRouter::default(),
        counters,
        nodes: BTreeMap::new(),
        renew_failures: Arc::default(),
        slow_voters: Arc::default(),
        total: n,
    };
    let started = Instant::now();
    for i in 1..=n {
        h.open(i).await;
    }
    let voters: Vec<NodeId> = (1..=VOTERS).map(NodeId::from).collect();
    let members = (1..=VOTERS)
        .map(|i| (NodeId::from(i), BasicNode::new(format!("node-{i}"))))
        .collect();
    h.get(voters[0]).initialize(members).await.unwrap();
    let leader = h.wait_leader(&voters, Duration::from_secs(10)).await;
    for i in VOTERS + 1..=n {
        h.get(leader)
            .add_learner(NodeId::from(i), BasicNode::new(format!("node-{i}")))
            .await
            .unwrap();
    }
    for i in 1..=n {
        h.register(leader, i).await;
    }
    eprintln!("scale: {n} nodes up in {:?}", started.elapsed());

    // 안정된 뒤(모든 노드가 한 번 넘게 갱신) 리더의 초당 RPC 수: 하트비트, 리스 갱신과 그 확인. 투표 멤버로 가는
    // RPC를 늦춰 확인 왕복이 갱신 간격보다 길게 한다(묶기 검사가 기계 속도에 좌우되지 않게). 쓰기 지연·장애 조치는
    // 늦추지 않고 잰다.
    h.slow_voters.store(true, Ordering::Relaxed);
    tokio::time::sleep(RENEW + Duration::from_secs(1)).await;
    let failures_before = h.renew_failures.load(Ordering::Relaxed);
    let [to_voters, to_learners, forwarded] =
        leader_rpcs(&h.counters[&leader], Duration::from_secs(5)).await;
    let renew_failures = h.renew_failures.load(Ordering::Relaxed) - failures_before;
    h.slow_voters.store(false, Ordering::Relaxed);
    // 투표 멤버로 센 RPC는 처음의 `1..=VOTERS`로 가는 것이다. 재는 동안 키퍼가 바꾸지 않았는지 본다.
    let measured_voters: Vec<NodeId> = h.get(leader).members().voters.into_keys().collect();
    assert_eq!(
        measured_voters, voters,
        "the voting members changed while measuring"
    );
    let rpcs = to_voters + to_learners + forwarded;
    let unbatched = unbatched_to_voters(forwarded);
    eprintln!(
        "scale: leader RPCs/s {rpcs:.0} (to voters {to_voters:.0} of {unbatched:.0} unbatched, \
         ratio {:.2}, to learners {to_learners:.0}, forwarded renewals {forwarded:.0}), failed \
         renewals {renew_failures}",
        to_voters / unbatched
    );

    // 쓰기 지연(리더에서 하나씩)
    let mut latencies = Vec::with_capacity(WRITES);
    for i in 0..WRITES {
        let t = Instant::now();
        h.get(leader).create(&volume(i)).await.unwrap();
        latencies.push(t.elapsed());
    }
    latencies.sort();
    let p50 = latencies[WRITES / 2];
    let p99 = latencies[(WRITES * 99).div_ceil(100) - 1];
    eprintln!(
        "scale: write p50 {p50:?}, p99 {p99:?}, max {:?}",
        latencies[WRITES - 1]
    );

    // 노드 하나 추가(learner로 들어와 로그를 따라잡기까지)
    let added = n + 1;
    h.open(added).await;
    let t = Instant::now();
    h.get(leader)
        .add_learner(NodeId::from(added), BasicNode::new(format!("node-{added}")))
        .await
        .unwrap();
    let add = t.elapsed();
    eprintln!("scale: added a node in {add:?}");

    // 리더 장애: 네트워크에서 끊는 순간부터 남은 투표 멤버 가운데 새 리더가 자기 임기의 항목을 적용하기까지
    let rest: Vec<NodeId> = voters.iter().copied().filter(|v| *v != leader).collect();
    let t = Instant::now();
    h.router.isolate(leader);
    let elected = loop {
        let leading = rest
            .iter()
            .any(|id| h.get(*id).raft().unwrap().metrics().borrow().state == ServerState::Leader);
        if leading {
            break t.elapsed();
        }
        assert!(
            t.elapsed() < FAILOVER_GIVE_UP,
            "no new leader among {rest:?} within {FAILOVER_GIVE_UP:?} after isolating the leader {leader}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    h.wait_leader(&rest, FAILOVER_GIVE_UP.saturating_sub(t.elapsed()))
        .await;
    let failover = t.elapsed();
    eprintln!("scale: new leader elected in {elected:?}, ready in {failover:?}");

    h.close().await;

    assert_eq!(renew_failures, 0, "lease renewals failed");
    assert!(
        rpcs < RPCS_PER_NODE_MAX * n as f64,
        "leader RPCs/s {rpcs:.0} for {n} nodes"
    );
    // 확인 왕복이 갱신 간격보다 길면 묶기가 투표 멤버로 가는 RPC를 묶지 않은 최악보다 크게 줄인다.
    assert!(
        to_voters < unbatched * BATCHED_RATIO_MAX,
        "leader RPCs/s to voters {to_voters:.0}: confirmations are not batched \
         (unbatched {unbatched:.0})"
    );
    let p99_max = write_p99_max();
    assert!(p99 < p99_max, "write p99 {p99:?} (max {p99_max:?})");
    assert!(add < ADD_NODE_MAX, "adding a node took {add:?}");
    let failover_max = failover_max();
    assert!(
        failover < failover_max,
        "failover took {failover:?} (max {failover_max:?})"
    );
}
