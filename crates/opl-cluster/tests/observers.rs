//! 관찰 노드 전환(설계 §6): 노드 여덟이 투표 멤버 다섯·대기 learner 둘·관찰 노드 하나로 나뉘고, 투표 멤버가 죽으면
//! 관찰 노드가 learner를 거쳐 투표 멤버가 된다. 내려가는 노드는 그동안 읽고 쓰고, 다시 시작한 관찰 노드는 자기
//! 리비전부터 이어 가며, 꺼진 동안 내려진 노드는 대비책으로 관찰 노드가 된다. 관찰 노드 기능을 모르는 노드가
//! 있으면 아무도 내리지 않는다.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use opl_cluster::memnet::MemRouter;
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::watch::WatchEvent;
use opl_cluster::{
    Cluster, ClusterConfig, ClusterError, OBSERVER, RoleKeeper, RoleKeeperConfig, VOTER_LIMIT,
    VoterKeeper, VoterKeeperConfig, ZONE_LABEL,
};
use opl_types::cluster::{ClusterSpec, ClusterStatus};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// 리스 갱신 간격
const RENEW: Duration = Duration::from_millis(200);

fn keeper_config() -> VoterKeeperConfig {
    VoterKeeperConfig {
        interval: Duration::from_millis(100),
        healthy_within: Duration::from_secs(1),
        alive_within: Duration::from_millis(600),
        offline_after: Duration::from_secs(2),
        change_timeout: Duration::from_secs(5),
    }
}

fn role_config() -> RoleKeeperConfig {
    RoleKeeperConfig {
        interval: Duration::from_millis(100),
        stalled_after: Duration::from_secs(2),
    }
}

fn id(n: u64) -> NodeId {
    NodeId::from(n)
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

/// 노드 하나: 저장소, 리스 갱신, VoterKeeper, RoleKeeper
struct Running {
    cluster: Arc<Cluster>,
    renew: JoinHandle<()>,
    voters: VoterKeeper,
    roles: RoleKeeper,
}

struct Harness {
    router: MemRouter,
    root: TempDir,
    nodes: BTreeMap<NodeId, Running>,
}

impl Harness {
    /// 노드 `1..=5`를 투표 멤버로 초기화하고 클러스터 정보와 Node 기록(`zones`의 zone, 기능 `voter-limit`)을 만든
    /// 뒤 나머지를 learner로 더한다. learner가 모두 따라잡은 뒤 `without_observer`가 아닌 노드가 기능 `observer`를
    /// 알린다(그 전에는 아무도 내리지 않으므로, 따라잡는 중인 learner가 먼저 내려가지 않는다).
    async fn start(zones: &[&str], without_observer: &[u64]) -> Self {
        let mut h = Harness {
            router: MemRouter::default(),
            root: TempDir::new().unwrap(),
            nodes: BTreeMap::new(),
        };
        let n = zones.len() as u64;
        for i in 1..=n {
            h.open(i).await;
        }
        let voters = (1..=5)
            .map(|i| (id(i), OrbNode::single(format!("node-{i}"))))
            .collect();
        h.get(1).initialize(voters).await.unwrap();
        eventually("a leader", || async { h.leader().is_some() }).await;
        let mut info = Resource::new(
            "c",
            ClusterSpec {
                ca_cert_pem: "CERT".into(),
                ca_key_pem: "KEY".into(),
            },
        );
        info.status = ClusterStatus::default();
        h.get(1).create(&info).await.unwrap();
        for (i, zone) in (1..=n).zip(zones) {
            h.register(i, zone).await;
        }
        for i in 6..=n {
            let leader = h.leader_cluster();
            leader
                .add_learner(id(i), OrbNode::single(format!("node-{i}")))
                .await
                .unwrap();
        }
        eventually("learners caught up", || async {
            let Some(raft) = h.leader().and_then(|l| h.nodes[&l].cluster.raft()) else {
                return false;
            };
            let m = raft.metrics().borrow().clone();
            let last = m.last_log_index;
            m.replication.as_ref().is_some_and(|r| {
                (6..=n).all(|i| r.get(&id(i)).is_some_and(|l| l.map(|l| l.index) == last))
            })
        })
        .await;
        for i in (1..=n).filter(|i| !without_observer.contains(i)) {
            h.enable_observer(i).await;
        }
        h
    }

    /// 노드를 (다시) 연다(저장소의 모드대로). 데이터 디렉터리는 노드마다 고정이다.
    async fn open(&mut self, i: u64) {
        let node = id(i);
        let mut config = ClusterConfig::new(node, self.root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = Duration::from_secs(10);
        let deadline = Instant::now() + Duration::from_secs(5);
        let cluster = loop {
            match Cluster::open_node(
                config.clone(),
                self.router.network(node),
                self.router.follower(node),
            )
            .await
            {
                // openraft의 상태 머신 작업이 아직 저장소를 쥐고 있다.
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen)))
                    if Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        };
        let cluster = Arc::new(cluster.with_forwarder(self.router.forwarder(node)));
        self.router.register(node, &cluster);
        let renew = {
            let cluster = cluster.clone();
            tokio::spawn(async move {
                loop {
                    let _ = cluster.renew_lease(RENEW * 2).await;
                    tokio::time::sleep(RENEW).await;
                }
            })
        };
        self.nodes.insert(
            node,
            Running {
                voters: VoterKeeper::spawn(cluster.clone(), keeper_config()),
                roles: RoleKeeper::spawn(cluster.clone(), role_config()),
                cluster,
                renew,
            },
        );
    }

    fn get(&self, i: u64) -> &Arc<Cluster> {
        &self.nodes[&id(i)].cluster
    }

    /// 투표 멤버 가운데 스스로 리더라고 보는 노드
    fn leader(&self) -> Option<NodeId> {
        self.nodes.values().find_map(|n| {
            let raft = n.cluster.raft()?;
            let m = raft.metrics().borrow().clone();
            (m.state == openraft::ServerState::Leader).then_some(n.cluster.id())
        })
    }

    fn leader_cluster(&self) -> &Arc<Cluster> {
        &self.nodes[&self.leader().expect("a leader")].cluster
    }

    /// 리더가 아는 (투표 멤버, learner), joint면 None
    fn membership(&self) -> Option<(BTreeSet<NodeId>, BTreeSet<NodeId>)> {
        let m = self.nodes[&self.leader()?]
            .cluster
            .raft()?
            .metrics()
            .borrow()
            .clone();
        let membership = m.membership_config.membership();
        if membership.get_joint_config().len() > 1 {
            return None;
        }
        let voters: BTreeSet<NodeId> = membership.voter_ids().collect();
        let learners = membership.learner_ids().collect();
        Some((voters, learners))
    }

    /// 클러스터 정보의 관찰 노드 목록(리더의 저장소)
    fn listed(&self) -> BTreeSet<NodeId> {
        let Some(leader) = self.leader() else {
            return BTreeSet::new();
        };
        let (_, infos) = self.nodes[&leader].cluster.list::<ClusterSpec>().unwrap();
        infos[0]
            .status
            .observers
            .keys()
            .map(|u| NodeId(*u))
            .collect()
    }

    /// 투표 멤버 `voters`, learner `learners`, 나머지가 관찰 노드(목록에 있고 관찰 노드 모드)
    fn settled(&self, voters: &[u64], learners: &[u64], observers: &[u64]) -> bool {
        let ids = |v: &[u64]| v.iter().map(|&i| id(i)).collect::<BTreeSet<_>>();
        self.membership() == Some((ids(voters), ids(learners)))
            && self.listed() == ids(observers)
            && observers
                .iter()
                .all(|&i| self.get(i).mode() == NodeMode::Observer)
            && learners
                .iter()
                .chain(voters)
                .all(|&i| self.get(i).mode() == NodeMode::Member)
    }

    /// `settled`가 될 때까지 30초까지 기다린다. 안 되면 지금 모양을 보인다.
    async fn settle(&self, voters: &[u64], learners: &[u64], observers: &[u64]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.settled(voters, learners, observers) {
            if Instant::now() >= deadline {
                let modes: Vec<_> = self
                    .nodes
                    .iter()
                    .map(|(id, n)| (id.0.as_u128(), n.cluster.mode()))
                    .collect();
                panic!(
                    "not settled: leader {:?} membership {:?} listed {:?} modes {modes:?}",
                    self.leader(),
                    self.membership(),
                    self.listed()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 노드 `i`의 Node 기록(zone 레이블, 기능)
    async fn register(&self, i: u64, zone: &str) {
        let mut node = Resource::new(
            format!("n{i}"),
            NodeSpec {
                address: format!("node-{i}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = id(i).0;
        node.labels.insert(ZONE_LABEL.into(), zone.into());
        node.status = features(false);
        self.get(1).create(&node).await.unwrap();
    }

    /// 노드 `i`가 관찰 노드 기능을 알린다.
    async fn enable_observer(&self, i: u64) {
        let node = self.get(1).get::<NodeSpec>(id(i).0).unwrap().unwrap();
        self.get(1)
            .update_status::<NodeSpec>(node.id, &features(true))
            .await
            .unwrap();
    }

    /// 노드를 멈춘다(프로세스 종료에 해당). 데이터 디렉터리는 남는다.
    async fn stop(&mut self, i: u64) {
        let node = self.nodes.remove(&id(i)).unwrap();
        node.renew.abort();
        node.roles.shutdown().await;
        node.voters.shutdown().await;
        self.router.unregister(id(i));
        node.cluster.shutdown().await.unwrap();
    }

    async fn close(mut self) {
        for i in self
            .nodes
            .keys()
            .map(|n| n.0.as_u128() as u64)
            .collect::<Vec<_>>()
        {
            self.stop(i).await;
        }
    }
}

fn features(observer: bool) -> NodeStatus {
    let mut features = vec![VOTER_LIMIT.to_string()];
    if observer {
        features.push(OBSERVER.into());
    }
    NodeStatus {
        online: true,
        features,
        ..NodeStatus::default()
    }
}

/// `cond`가 참이 될 때까지 30초까지 기다린다.
async fn eventually<F, Fut>(what: &str, cond: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 노드 여덟은 투표 멤버 다섯·대기 learner 둘·관찰 노드 하나가 된다. 내려가는 노드(8)는 그동안 쓰기를 리더로
/// 보내고 곧바로 자기 저장소에서 읽는다. 관찰 노드가 되면 Raft가 없고 로그는 비어 있다.
#[tokio::test]
async fn eight_nodes_split_into_voters_standbys_and_an_observer_without_interrupting_writes() {
    let h = Harness::start(&["a"; 8], &[]).await;
    let eight = h.get(8).clone();
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicU64::new(0));
    let writer = {
        let (stop, written) = (stop.clone(), written.clone());
        tokio::spawn(async move {
            let mut i = 0;
            while !stop.load(Ordering::SeqCst) {
                let v = volume(&format!("w{i}"));
                let revision = eight.create(&v).await.expect("a write from node 8");
                let read = eight
                    .get::<VolumeSpec>(v.id)
                    .unwrap()
                    .expect("read own write");
                assert_eq!(read.revision, revision);
                written.fetch_add(1, Ordering::SeqCst);
                i += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    let before = written.load(Ordering::SeqCst);
    eventually("more writes from the observer", || async {
        written.load(Ordering::SeqCst) > before + 5
    })
    .await;
    stop.store(true, Ordering::SeqCst);
    writer.await.unwrap();

    let eight = h.get(8);
    assert!(eight.raft().is_none());
    assert!(eight.members().observers.contains_key(&id(8)));
    // 관찰 노드도 리더와 같은 것을 읽는다.
    let revision = h.leader_cluster().revision();
    eight
        .wait_revision(revision, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        eight.list::<VolumeSpec>().unwrap().1,
        h.leader_cluster().list::<VolumeSpec>().unwrap().1
    );
    h.close().await;
}

/// 투표 멤버가 죽으면 대기 learner가 올라가고, 관찰 노드가 learner로 돌아와(로그를 적용 위치까지 지운 것으로 메운
/// 뒤 리더에게서 받는다) 다음 장애에 투표 멤버가 된다.
#[tokio::test]
async fn an_observer_returns_through_learner_to_voter_when_voters_die() {
    let mut h = Harness::start(&["a", "a", "b", "b", "c", "a", "b", "d"], &[]).await;
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    let observed = h.get(8).revision();
    h.stop(5).await;
    // 5(zone c)는 6으로 바뀌고(zone a·b가 같아 id가 작은 것) 대기가 하나라 8이 learner로 돌아온다.
    eventually("node 8 back as a learner", || async {
        h.membership()
            .is_some_and(|(v, l)| v.contains(&id(6)) && l.contains(&id(8)))
            && h.get(8).mode() == NodeMode::Member
            && !h.listed().contains(&id(8))
    })
    .await;
    // 다시 띄운 Raft는 지운 로그를 적용 위치까지 지운 것(purge)으로 메웠다.
    let m = h.get(8).raft().unwrap().metrics().borrow().clone();
    assert!(m.purged.map(|l| l.index) >= Some(observed), "{m:?}");
    // 따라잡은 뒤 4(zone b)가 죽으면 투표 멤버가 없는 zone d의 8이 올라간다.
    eventually("a leader", || async { h.leader().is_some() }).await;
    let target = h.leader_cluster().revision();
    h.get(8)
        .wait_revision(target, Duration::from_secs(10))
        .await
        .unwrap();
    h.stop(4).await;
    eventually("node 8 a voter", || async {
        h.membership().is_some_and(|(v, _)| v.contains(&id(8)))
    })
    .await;
    let v = volume("after");
    h.get(8).create(&v).await.unwrap();
    h.close().await;
}

/// 다시 시작한 관찰 노드(파일 저장소)는 관찰 노드로 뜨고 자기 리비전부터 이어 받는다(스냅샷·`Reset` 없이).
#[tokio::test]
async fn a_restarted_observer_resumes_from_its_revision() {
    let mut h = Harness::start(&["a"; 8], &[]).await;
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    let old = h.get(8).clone();
    h.stop(8).await;
    // 멈춘 뒤의 적용 위치(저장소에 남은 것)
    let revision = old.revision();
    drop(old);
    let mut written = Vec::new();
    for i in 0..3 {
        let v = volume(&format!("while-down-{i}"));
        h.leader_cluster().create(&v).await.unwrap();
        written.push(v.id);
    }
    h.open(8).await;
    let eight = h.get(8).clone();
    assert_eq!(eight.mode(), NodeMode::Observer);
    assert!(eight.raft().is_none());
    assert_eq!(eight.revision(), revision);
    let mut w = eight.watch(revision).unwrap();
    let mut seen = Vec::new();
    while seen.len() < written.len() {
        match tokio::time::timeout(Duration::from_secs(10), w.next())
            .await
            .unwrap()
            .unwrap()
        {
            WatchEvent::Put {
                kind: opl_types::resource::Kind::Volume,
                resource,
            } => seen.push(resource.id),
            WatchEvent::Put { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seen, written);
    h.close().await;
}

/// 내려가는 동안 목록을 받지 못한 노드(Raft가 끊긴 채 멤버십에서 빠지고 그 뒤 다시 시작함)는 적용이 멈춘 것을 보고
/// 투표 멤버에게 역할을 물어 관찰 노드가 된다.
#[tokio::test]
async fn a_node_demoted_while_cut_off_follows_the_fallback() {
    // 8이 내려가기 전에 Raft를 끊는다: 기능을 모두 알기 전에는 아무도 내리지 않는다.
    let mut h = Harness::start(&["a"; 8], &[8]).await;
    eventually("standbys caught up", || async {
        h.membership()
            .is_some_and(|(v, l)| v.len() == 5 && l.len() == 3)
    })
    .await;
    h.router.block_raft(id(8));
    h.enable_observer(8).await;
    // 목록에 오르지만 8은 그 쓰기를 받지 못한다. 그 사이 꺼지고, `change_timeout` 뒤 멤버십에서 빠진다.
    eventually("node 8 listed", || async { h.listed().contains(&id(8)) }).await;
    assert_eq!(h.get(8).mode(), NodeMode::Member);
    h.stop(8).await;
    eventually("node 8 out of Raft", || async {
        h.membership()
            .is_some_and(|(v, l)| v.len() == 5 && l.len() == 2 && !l.contains(&id(8)))
    })
    .await;
    h.router.heal(id(8));
    h.open(8).await;
    assert_eq!(h.get(8).mode(), NodeMode::Member);
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    let v = volume("after-fallback");
    let revision = h.get(8).create(&v).await.unwrap();
    assert_eq!(
        h.get(8).get::<VolumeSpec>(v.id).unwrap().unwrap().revision,
        revision
    );
    h.close().await;
}

/// 관찰 노드 기능을 모르는 노드가 하나라도 있으면 아무도 내리지 않는다. 그 노드가 기능을 알리면 내린다.
#[tokio::test]
async fn nobody_becomes_an_observer_until_every_node_knows_the_feature() {
    let h = Harness::start(&["a"; 8], &[3]).await;
    eventually("standbys caught up", || async {
        h.membership()
            .is_some_and(|(v, l)| v.len() == 5 && l.len() == 3)
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(h.listed().is_empty());
    assert_eq!(h.membership().map(|(_, l)| l.len()), Some(3));
    h.enable_observer(3).await;
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    h.close().await;
}
