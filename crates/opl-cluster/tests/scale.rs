//! 규모 시험(설계 §5·§6): 한 프로세스에 노드 여럿(기본 300, `OPL_SCALE_NODES`)을 memnet으로 띄운다. 투표 멤버
//! 다섯과 대기 learner 둘이 Raft 구성원이고 나머지는 관찰 노드다(투표 멤버 하나를 Follow로 따름). 모든 노드가
//! 2초마다 리스를 갱신하고 `VoterKeeper`·`RoleKeeper`를 띄운 채로 리더의 초당 RPC 수, 쓰기 지연 p99, 관찰 노드의
//! 뒤처짐 p99, 노드 하나 추가, 투표 멤버 장애 뒤 관찰 노드가 다시 붙기까지, 리더 장애 뒤 새 리더까지를 재고
//! 기준을 넘으면 실패한다.
//!
//! 관찰 노드는 실제 노드처럼 learner로 들어와 따라잡은 뒤 내려가지 않고 빈 저장소로 바로 관찰 노드로 연다(시작
//! 시간을 줄인다, `ClusterConfig::follow_seed`). 클러스터 정보의 관찰 노드 목록에는 넣는다(키퍼가 보는 모양을
//! 실제와 같게).
//!
//! 저장소는 모두 메모리에 둔다. 한 컴퓨터의 디스크 하나에 노드 수백 개(투표 멤버 다섯만이어도)의 fsync가
//! 몰리면 그 줄서기가 잰 값을 차지한다(실제 노드는 디스크가 따로다). 그래서 쓰기 지연은 Raft와 리더의 일만
//! 잰다. 디스크를 넣은 값은 설계 §14에 적었다.
//!
//! `cargo test -p opl-cluster --features scale-it --test scale -- --nocapture`

#![cfg(feature = "scale-it")]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use openraft::ServerState;
use openraft::error::{InstallSnapshotError, RPCError, RaftError};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use opl_cluster::memnet::{MemConnection, MemFollow, MemForwarder, MemNetwork, MemRouter};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode, TypeConfig};
use opl_cluster::{
    Cluster, ClusterConfig, ClusterError, FollowClient, FollowFrame, Forwarder, OBSERVER,
    RoleKeeper, RoleKeeperConfig, STANDBY_LEARNERS, Typed, VOTER_LIMIT, VoterKeeper,
    VoterKeeperConfig, ZONE_LABEL,
};
use opl_types::cluster::{ClusterSpec, ClusterStatus};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::VolumeSpec;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// 투표 멤버 수(클러스터 설정 `voters`의 기본값)
const VOTERS: u64 = 5;
/// Raft 구성원 수(투표 멤버와 대기 learner). 나머지는 관찰 노드다.
const MEMBERS: u64 = VOTERS + STANDBY_LEARNERS as u64;
/// 리스 갱신 간격(opld `LIVENESS_RENEW`, `HaGuard`)
const RENEW: Duration = Duration::from_secs(2);
/// 쓰기 지연을 재는 쓰기 수
const WRITES: usize = 200;
/// 관찰 노드의 뒤처짐을 재는 쓰기 수와 그 간격
const STALENESS_WRITES: usize = 100;
const STALENESS_GAP: Duration = Duration::from_millis(20);
/// 리더가 쓰기를 적용한 뒤 모든 관찰 노드가 적용하기까지의 p99 기준. 관찰 노드는 리더가 아닌 투표 멤버를
/// 따르므로 그 투표 멤버가 커밋을 알기까지(다음 복제나 하트비트 200 ms)와 Follow 한 번이 더해진다.
const FOLLOW_STALENESS_P99: Duration = Duration::from_millis(500);
/// 투표 멤버 장애 뒤 모든 관찰 노드가 다른 투표 멤버로 다시 붙어 새 쓰기를 받기까지(관찰 노드는 진행이 3초
/// 없으면 다시 붙고, memnet은 끊긴 노드의 다음 프레임(1초마다 `Progress`)에서 끊는다).
const REATTACH_MAX: Duration = Duration::from_secs(5);

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
/// 리더가 비투표 구성원(대기 learner 둘)에게 보내는 초당 Raft RPC 상한: 하트비트 5(200 ms) × 둘 = 10에 쓰기
/// 복제의 여유. 관찰 노드에게는 보내지 않으므로 노드 수와 관계없다.
const NON_VOTER_RPCS_MAX: f64 = 15.0;
/// 노드 수와 관계없는 리더 RPC: 투표 멤버 넷에게 하트비트 20과 비투표 구성원 상한
const FIXED_RPCS_MAX: f64 = 20.0 + NON_VOTER_RPCS_MAX;
/// 노드(관찰 노드) 하나가 늘리는 리더의 초당 RPC 상한: 리스 갱신 전달 0.5, 그 갱신의 리더 확인을 묶지 못했을 때
/// 2번 × 투표 멤버 넷 = 4. 합 4.5(계획 47은 learner 하트비트 5를 더해 10이었다).
const RPCS_PER_NODE_MAX: f64 = 4.5;
/// 하트비트 간격(`raft_config`)
const HEARTBEAT: Duration = Duration::from_millis(200);
/// RPC를 재는 동안 투표 멤버로 가는 `append_entries`(하트비트·리더 확인)에 넣는 지연의 바닥.
/// [`voter_rpc_delay`]를 본다.
const VOTER_RPC_DELAY: Duration = Duration::from_millis(20);
/// 그때 투표 멤버에게 간 RPC가 묶지 않은 최악의 이 비율 아래여야 한다(노드 수와 관계없이, CI의 노드 100에서도).
/// 묶지 않게 고치면 노드 100·300 모두 0.79~0.80이었다(설계 §14).
const BATCHED_RATIO_MAX: f64 = 0.6;

/// 노드 수의 하한. 그보다 적으면 [`voter_rpc_delay`]가 하트비트 간격(200 ms)에 가까워져 선거가 나고 리스 갱신이
/// 실패한다(노드 8에서 지연 250 ms로 갱신 14번 실패, 노드 추가 실패).
const MIN_NODES: u64 = 20;

/// 투표 멤버로 가는 `append_entries`에 넣는 지연. 확인 왕복이 늘 갱신이 오는 간격보다 길어 기계·빌드와 관계없이
/// 확인이 묶이게 한다. 확인은 한 번에 하나라 초당 많아야 1/지연번이고, 확인 요청은 갱신(초당 n/2)마다 2번이라
/// 초당 n번이다. 지연을 2초/n(바닥 20 ms, 노드 20에서 100 ms)으로 두면 확인은 요청의 절반 이하로 묶이고, 투표
/// 멤버 RPC는 많아야 하트비트 20 + 확인 4 × n/2 = 20 + 2n, 묶지 않은 최악(20 + 10 × (n/2 + 0.5))의
/// (20 + 2n) / (25 + 5n)이다(노드 20에서 0.48, 30에서 0.46, 100에서 0.42). 노드 100부터는 20 ms라 확인이 초당
/// 50번까지다(노드 300에서 0.14). 잰 값: 노드 30 0.42, 100 0.37, 300 0.13. 확인을 묶지 않게 고친 빌드는 노드 30
/// 0.79, 100 0.80으로 실패했다. 계획 47처럼 노드 수와 관계없이 20 ms로 두면 확인 요청이 초당 50번이 안 되는
/// 노드 50 미만에서는 묶일 까닭이 없어 노드 30에서 0.79로 실패했다.
fn voter_rpc_delay(n: u64) -> Duration {
    VOTER_RPC_DELAY.max(Duration::from_secs(2) / u32::try_from(n).unwrap())
}

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
#[derive(Clone)]
struct Counting {
    inner: MemNetwork,
    from: NodeId,
    counters: Counters,
    /// 0이 아니면 투표 멤버로 가는 `append_entries`가 이만큼(마이크로초) 늦다.
    slow_voters: Arc<AtomicU64>,
}

impl RaftNetworkFactory<TypeConfig> for Counting {
    type Network = CountingConnection;

    async fn new_client(&mut self, target: NodeId, node: &OrbNode) -> CountingConnection {
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
    slow_voters: Arc<AtomicU64>,
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
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for CountingConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        self.count();
        let delay = self.slow_voters.load(Ordering::Relaxed);
        if self.to_voter && delay > 0 {
            tokio::time::sleep(Duration::from_micros(delay)).await;
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

/// 관찰 노드가 마지막으로 붙은 투표 멤버(관찰 노드 → 투표 멤버)
type Upstreams = Arc<Mutex<HashMap<NodeId, NodeId>>>;

/// 관찰 노드가 어느 투표 멤버에 붙었는지 적는 Follow 통로
struct TrackingFollow {
    inner: Arc<MemFollow>,
    from: NodeId,
    upstreams: Upstreams,
}

#[async_trait]
impl FollowClient for TrackingFollow {
    async fn follow(
        &self,
        to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
        let frames = self.inner.follow(to, addr, since).await?;
        self.upstreams.lock().unwrap().insert(self.from, to);
        Ok(frames)
    }
}

/// 노드 하나: 저장소, 리스 갱신, VoterKeeper, RoleKeeper
struct Running {
    cluster: Arc<Cluster>,
    renew: JoinHandle<()>,
    voters: VoterKeeper,
    roles: RoleKeeper,
}

struct Harness {
    router: MemRouter,
    counters: Counters,
    nodes: BTreeMap<NodeId, Running>,
    /// 실패한 리스 갱신 수
    renew_failures: Arc<AtomicU64>,
    /// 투표 멤버로 가는 RPC의 지연(마이크로초, RPC를 잴 때만)
    slow_voters: Arc<AtomicU64>,
    upstreams: Upstreams,
    /// 노드 수(리스 갱신 시작을 고르게 나눈다)
    total: u64,
}

impl Harness {
    /// 노드 `i`를 연다. 관찰 노드면 빈 저장소로 바로 관찰 노드로 열어 투표 멤버 `1..=VOTERS`를 따르고, 아니면
    /// 멤버로 연다(아직 멤버십에 없다).
    async fn open(&mut self, i: u64, observer: bool) {
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
        let follow = Arc::new(TrackingFollow {
            inner: self.router.follower(id),
            from: id,
            upstreams: self.upstreams.clone(),
        });
        let cluster = if observer {
            config.follow_seed = (1..=VOTERS)
                .map(|v| (NodeId::from(v), format!("node-{v}")))
                .collect();
            Cluster::open_observer(config, network, follow).await
        } else {
            Cluster::open_node(config, network, follow).await
        }
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
        self.nodes.insert(
            id,
            Running {
                voters: VoterKeeper::spawn(cluster.clone(), VoterKeeperConfig::default()),
                roles: RoleKeeper::spawn(cluster.clone(), RoleKeeperConfig::default()),
                cluster,
                renew,
            },
        );
    }

    fn get(&self, id: NodeId) -> &Cluster {
        &self.nodes[&id].cluster
    }

    fn observers(&self) -> impl Iterator<Item = &Arc<Cluster>> {
        self.nodes
            .range(NodeId::from(MEMBERS + 1)..)
            .map(|(_, n)| &n.cluster)
            .filter(|c| c.raft().is_none())
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

    /// 노드 `i`의 Node 리소스(zone 셋에 고르게, 기능 `voter-limit`·`observer`)
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
            features: vec![VOTER_LIMIT.into(), OBSERVER.into()],
            ..NodeStatus::default()
        };
        self.get(leader).create(&node).await.unwrap();
    }

    /// 모든 관찰 노드가 `revision`까지 적용하기를 기다리고 가장 늦은 것까지 걸린 시간을 돌려준다.
    async fn observers_reach(&self, revision: u64, within: Duration) -> Duration {
        let t = Instant::now();
        for o in self.observers() {
            let left = within.saturating_sub(t.elapsed());
            if let Err(e) = o.wait_revision(revision, left).await {
                panic!(
                    "observer {} stayed at {} below {revision} for {within:?}: {e}",
                    o.id(),
                    o.revision()
                );
            }
        }
        t.elapsed()
    }

    /// 노드를 멈춘다(프로세스 종료에 해당). 네트워크에서 먼저 뺀다.
    async fn stop(&mut self, id: NodeId) {
        self.router.unregister(id);
        let node = self.nodes.remove(&id).unwrap();
        node.renew.abort();
        node.roles.shutdown().await;
        node.voters.shutdown().await;
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

/// 정렬된 값의 p50·p99
fn percentiles(sorted: &[Duration]) -> (Duration, Duration) {
    let len = sorted.len();
    (sorted[len / 2], sorted[(len * 99).div_ceil(100) - 1])
}

#[tokio::test(flavor = "multi_thread")]
async fn hundreds_of_nodes_do_not_slow_the_leader() {
    let n = node_count();
    assert!(
        n >= MIN_NODES,
        "OPL_SCALE_NODES must be at least {MIN_NODES}"
    );
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
        upstreams: Arc::default(),
        total: n,
    };
    let started = Instant::now();
    for i in 1..=MEMBERS {
        h.open(i, false).await;
    }
    let voters: Vec<NodeId> = (1..=VOTERS).map(NodeId::from).collect();
    let members = (1..=VOTERS)
        .map(|i| (NodeId::from(i), OrbNode::single(format!("node-{i}"))))
        .collect();
    h.get(voters[0]).initialize(members).await.unwrap();
    let leader = h.wait_leader(&voters, Duration::from_secs(10)).await;
    for i in VOTERS + 1..=MEMBERS {
        h.get(leader)
            .add_learner(NodeId::from(i), OrbNode::single(format!("node-{i}")))
            .await
            .unwrap();
    }
    // 클러스터 정보의 관찰 노드 목록(실제로는 리더의 VoterKeeper가 내리며 쓴다)
    let mut info = Resource::new(
        "c",
        ClusterSpec {
            ca_cert_pem: "CERT".into(),
            ca_key_pem: "KEY".into(),
        },
    );
    info.status = ClusterStatus {
        observers: (MEMBERS + 1..=n)
            .map(|i| (NodeId::from(i).0, format!("node-{i}")))
            .collect(),
    };
    h.get(leader).create(&info).await.unwrap();
    for i in 1..=n {
        h.register(leader, i).await;
    }
    for i in MEMBERS + 1..=n {
        h.open(i, true).await;
    }
    h.observers_reach(h.get(leader).revision(), Duration::from_secs(30))
        .await;
    assert_eq!(h.observers().count() as u64, n - MEMBERS);
    eprintln!("scale: {n} nodes up in {:?}", started.elapsed());

    // 안정된 뒤(모든 노드가 한 번 넘게 갱신) 리더의 초당 RPC 수: 하트비트, 리스 갱신과 그 확인. 투표 멤버로 가는
    // RPC를 늦춰 확인 왕복이 갱신 간격보다 길게 한다(묶기 검사가 기계 속도에 좌우되지 않게). 쓰기 지연·장애 조치는
    // 늦추지 않고 잰다.
    let delay = voter_rpc_delay(n);
    h.slow_voters
        .store(delay.as_micros() as u64, Ordering::Relaxed);
    tokio::time::sleep(RENEW + Duration::from_secs(1)).await;
    let failures_before = h.renew_failures.load(Ordering::Relaxed);
    let [to_voters, to_learners, forwarded] =
        leader_rpcs(&h.counters[&leader], Duration::from_secs(5)).await;
    let renew_failures = h.renew_failures.load(Ordering::Relaxed) - failures_before;
    h.slow_voters.store(0, Ordering::Relaxed);
    // 투표 멤버로 센 RPC는 처음의 `1..=VOTERS`로 가는 것이다. 재는 동안 키퍼가 바꾸지 않았는지 본다.
    let measured = h.get(leader).members();
    assert_eq!(
        measured.voters.keys().copied().collect::<Vec<_>>(),
        voters,
        "the voting members changed while measuring"
    );
    let standby: BTreeSet<NodeId> = (VOTERS + 1..=MEMBERS).map(NodeId::from).collect();
    assert_eq!(
        measured.learners.keys().copied().collect::<BTreeSet<_>>(),
        standby,
        "the standby learners changed while measuring"
    );
    let rpcs = to_voters + to_learners + forwarded;
    let unbatched = unbatched_to_voters(forwarded);
    eprintln!(
        "scale: leader RPCs/s {rpcs:.0} ({:.2} per node; to voters {to_voters:.0} of {unbatched:.0} \
         unbatched with {delay:?} delay, ratio {:.2}, to learners {to_learners:.0}, forwarded \
         renewals {forwarded:.0}), failed renewals {renew_failures}",
        rpcs / n as f64,
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
    let (p50, p99) = percentiles(&latencies);
    eprintln!(
        "scale: write p50 {p50:?}, p99 {p99:?}, max {:?}",
        latencies[WRITES - 1]
    );

    // 관찰 노드의 뒤처짐: 리더가 쓰기를 마친(적용한) 때부터 관찰 노드 하나하나가 그것을 적용하기까지
    let mut waits = Vec::new();
    for i in 0..STALENESS_WRITES {
        let revision = h.get(leader).create(&volume(WRITES + i)).await.unwrap();
        let t = Instant::now();
        for o in h.observers() {
            let o = o.clone();
            waits.push(tokio::spawn(async move {
                o.wait_revision(revision, Duration::from_secs(10))
                    .await
                    .map(|_| t.elapsed())
            }));
        }
        tokio::time::sleep(STALENESS_GAP).await;
    }
    let mut staleness = Vec::with_capacity(waits.len());
    for w in waits {
        staleness.push(w.await.unwrap().expect("an observer fell behind for 10s"));
    }
    staleness.sort();
    let (stale_p50, stale_p99) = percentiles(&staleness);
    eprintln!(
        "scale: observer staleness p50 {stale_p50:?}, p99 {stale_p99:?}, max {:?} ({} samples)",
        staleness[staleness.len() - 1],
        staleness.len()
    );

    // 노드 하나 추가(learner로 들어와 로그를 따라잡기까지)
    let added = n + 1;
    h.open(added, false).await;
    let t = Instant::now();
    h.get(leader)
        .add_learner(
            NodeId::from(added),
            OrbNode::single(format!("node-{added}")),
        )
        .await
        .unwrap();
    let add = t.elapsed();
    eprintln!("scale: added a node in {add:?}");

    // 투표 멤버 장애: 관찰 노드가 가장 많이 붙은 투표 멤버(리더가 아닌 것)를 끊고 곧바로 쓴다. 끊긴 노드는 그
    // 쓰기를 받지 못하므로 그 노드를 따르던 관찰 노드는 다른 투표 멤버로 다시 붙어야 받는다.
    let followers = |v: NodeId| {
        h.upstreams
            .lock()
            .unwrap()
            .values()
            .filter(|u| **u == v)
            .count()
    };
    let failed = voters
        .iter()
        .copied()
        .filter(|v| *v != leader)
        .max_by_key(|v| followers(*v))
        .unwrap();
    let moved = followers(failed);
    assert!(
        moved > 0,
        "no observer follows a voter other than the leader"
    );
    let rest: Vec<NodeId> = voters
        .iter()
        .copied()
        .filter(|v| *v != leader && *v != failed)
        .collect();
    let t = Instant::now();
    h.router.isolate(failed);
    let revision = h.get(leader).create(&volume(usize::MAX)).await.unwrap();
    h.observers_reach(revision, FAILOVER_GIVE_UP).await;
    let reattach = t.elapsed();
    eprintln!(
        "scale: all observers past the failed voter {failed} ({moved} followed it) in {reattach:?}"
    );

    // 리더 장애: 네트워크에서 끊는 순간부터 남은 투표 멤버 가운데 새 리더가 자기 임기의 항목을 적용하기까지
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
    // 관찰 노드에게는 Raft RPC를 보내지 않는다.
    assert!(
        to_learners < NON_VOTER_RPCS_MAX,
        "leader Raft RPCs/s to non-voters {to_learners:.0} (max {NON_VOTER_RPCS_MAX})"
    );
    assert!(
        rpcs < FIXED_RPCS_MAX + RPCS_PER_NODE_MAX * n as f64,
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
    assert!(
        stale_p99 < FOLLOW_STALENESS_P99,
        "observer staleness p99 {stale_p99:?} (max {FOLLOW_STALENESS_P99:?})"
    );
    assert!(add < ADD_NODE_MAX, "adding a node took {add:?}");
    assert!(
        reattach < REATTACH_MAX,
        "observers took {reattach:?} to follow another voter (max {REATTACH_MAX:?})"
    );
    let failover_max = failover_max();
    assert!(
        failover < failover_max,
        "failover took {failover:?} (max {failover_max:?})"
    );
}
