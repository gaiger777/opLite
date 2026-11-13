//! 클러스터 통신 다중 링크(설계 §7): 노드마다 링크 둘(`n{i}-a` 우선, `n{i}-b`)로 멤버십을 만들고, 모든 노드
//! 사이의 우선 링크를 끊어도 리더·쓰기·리스 갱신·관찰 노드의 Follow가 둘째 링크로 이어지며, 우선 링크를 풀면
//! 재시도 간격 뒤 다시 우선 링크를 쓴다. 리더의 키퍼는 노드가 적은 링크로 멤버십과 관찰 노드 목록의 링크를
//! 맞춘다.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openraft::ChangeMembers;
use opl_cluster::memnet::MemRouter;
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{
    Cluster, ClusterConfig, ClusterError, MULTI_LINK, OBSERVER, RoleKeeper, RoleKeeperConfig,
    VOTER_LIMIT, VoterKeeper, VoterKeeperConfig,
};
use opl_types::cluster::{ClusterSpec, ClusterStatus};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const WAIT: Duration = Duration::from_secs(10);

/// 시험의 우선 링크 재시도 간격. 실패하는 시험 호출이 하트비트(openraft는 연결하지 못한 노드에 500 ms 쉬었다
/// 다시 보낸다)와 선거 투표(1~2초마다 한 번)를 거의 모두 먹지 않게 선거 제한 시간보다 길게 둔다.
const RETRY: Duration = Duration::from_millis(2500);

fn id(n: u64) -> NodeId {
    NodeId::from(n)
}

/// 노드 id의 번호
fn num(n: NodeId) -> u64 {
    (1..=4).find(|i| id(*i) == n).unwrap()
}

fn link(n: u64, which: &str) -> String {
    format!("n{n}-{which}")
}

fn links(n: u64) -> OrbNode {
    OrbNode::new([link(n, "a"), link(n, "b")])
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

async fn eventually<F, Fut>(what: &str, cond: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT;
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct Harness {
    router: MemRouter,
    root: TempDir,
    nodes: BTreeMap<NodeId, Cluster>,
}

impl Harness {
    async fn open(&mut self, i: u64) {
        let node = id(i);
        let mut config = ClusterConfig::new(node, self.root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = WAIT;
        config.link_retry = RETRY;
        let deadline = Instant::now() + Duration::from_secs(5);
        let cluster = loop {
            match Cluster::open_node(
                config.clone(),
                self.router.network(node),
                self.router.follower(node),
            )
            .await
            {
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen)))
                    if Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        };
        let cluster = cluster.with_forwarder(self.router.forwarder(node));
        self.router.register(node, &cluster);
        self.nodes.insert(node, cluster);
    }

    fn get(&self, i: u64) -> &Cluster {
        &self.nodes[&id(i)]
    }

    /// 스스로 리더라고 보는 투표 멤버
    fn leader(&self) -> Option<NodeId> {
        self.nodes.values().find_map(|c| {
            let m = c.raft()?.metrics().borrow().clone();
            (m.state == openraft::ServerState::Leader).then_some(c.id())
        })
    }

    fn term(&self, n: NodeId) -> u64 {
        self.nodes[&n]
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .current_term
    }
}

/// 노드 1~3은 링크 둘로 투표 멤버, 노드 4는 링크 둘로 learner로 더한 뒤 관찰 노드가 된다.
async fn start() -> Harness {
    let mut h = Harness {
        router: MemRouter::default(),
        root: TempDir::new().unwrap(),
        nodes: BTreeMap::new(),
    };
    for i in 1..=4 {
        h.open(i).await;
    }
    let voters = (1..=3).map(|i| (id(i), links(i))).collect();
    h.get(1).initialize(voters).await.unwrap();
    eventually("a leader", || async {
        h.leader().is_some_and(|l| {
            let m = h.nodes[&l].raft().unwrap().metrics().borrow().clone();
            m.last_applied.map(|l| l.index) >= m.last_log_index
        })
    })
    .await;
    let leader = h.leader().unwrap();
    h.nodes[&leader].add_learner(id(4), links(4)).await.unwrap();
    let written = h.nodes[&leader].create(&volume("before")).await.unwrap();
    h.get(4).wait_revision(written, WAIT).await.unwrap();
    // 자기를 learner로 둔 멤버십이 지표에 적용으로 보일 때까지 다시 시도한다.
    let deadline = Instant::now() + WAIT;
    while let Err(e) = h.get(4).become_observer().await {
        assert!(Instant::now() < deadline, "{e}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(h.get(4).mode(), NodeMode::Observer);
    h
}

#[tokio::test]
async fn the_cluster_runs_on_the_second_link_and_returns_to_the_first() {
    let mut h = start().await;
    let leader = h.leader().unwrap();
    let term = h.term(leader);
    // 모든 노드가 링크를 알고, 링크 책은 첫 링크로 목록을 찾는다.
    let members = h.get(2).members();
    assert_eq!(members.links[&leader], links(num(leader)).addrs);
    assert_eq!(members.voters[&id(1)], link(1, "a"));
    assert_eq!(
        h.get(4).link_book().links(&link(1, "a")),
        vec![link(1, "a"), link(1, "b")]
    );

    // 모든 노드 사이의 우선 링크를 끊는다.
    for from in 1..=4 {
        for to in 1..=4 {
            h.router.cut_link(id(from), &link(to, "a"));
        }
    }
    // 리더는 그대로이고 팔로워·관찰 노드의 쓰기와 리스 갱신이 둘째 링크로 전달된다.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.leader(), Some(leader));
    assert_eq!(h.term(leader), term);
    let follower = (1..=3).map(id).find(|n| *n != leader).unwrap();
    let written = h.nodes[&follower].create(&volume("cut")).await.unwrap();
    h.nodes[&follower].renew_lease(WAIT).await.unwrap();
    let observer = h.get(4);
    observer.renew_lease(WAIT).await.unwrap();
    observer.create(&volume("from-observer")).await.unwrap();
    // 관찰 노드의 Follow는 우선 링크가 끊겨 닫히고 둘째 링크로 다시 붙는다.
    observer.wait_revision(written, WAIT).await.unwrap();
    let later = h.nodes[&leader].create(&volume("later")).await.unwrap();
    observer.wait_revision(later, WAIT).await.unwrap();
    assert!(
        observer
            .get_by_name::<VolumeSpec>("later")
            .unwrap()
            .is_some()
    );

    // 리더가 죽으면 남은 둘이 둘째 링크로 새 리더를 뽑는다.
    let stopped = h.nodes.remove(&leader).unwrap();
    h.router.unregister(leader);
    stopped.shutdown().await.unwrap();
    eventually("a new leader", || async {
        h.leader().is_some_and(|l| l != leader)
    })
    .await;
    let rest: Vec<NodeId> = (1..=3).map(id).filter(|n| *n != leader).collect();
    h.nodes[&rest[0]]
        .create(&volume("after-leader"))
        .await
        .unwrap();
    h.nodes[&rest[1]]
        .create(&volume("after-leader-2"))
        .await
        .unwrap();

    // 끊긴 동안에도 우선 링크로는 재시도 간격마다 시험 호출 하나씩만 간다.
    let a_links = |h: &Harness| -> u64 {
        rest.iter()
            .map(|n| h.router.sent(&link(num(*n), "a")))
            .sum()
    };
    let b_links = |h: &Harness| -> u64 {
        rest.iter()
            .map(|n| h.router.sent(&link(num(*n), "b")))
            .sum()
    };
    let (a, b) = (a_links(&h), b_links(&h));
    tokio::time::sleep(Duration::from_secs(1)).await;
    // 보내는 쪽과 받는 쪽 짝(남은 둘 사이 둘, 관찰 노드에서 둘)마다 1초에 시험 하나쯤(넉넉히 둘)이다.
    assert!(a_links(&h) - a <= 4 * 2, "{}", a_links(&h) - a);
    assert!(b_links(&h) - b >= 5, "{}", b_links(&h) - b);

    // 우선 링크를 풀면 재시도 간격 뒤 모두 다시 우선 링크를 쓴다.
    for from in 1..=4 {
        h.router.heal(id(from));
    }
    tokio::time::sleep(RETRY * 2).await;
    let (a, b) = (a_links(&h), b_links(&h));
    tokio::time::sleep(Duration::from_secs(1)).await;
    // 리더가 남은 팔로워에 보내는 하트비트만으로도 1초에 여럿 간다.
    assert!(a_links(&h) - a >= 5, "{}", a_links(&h) - a);
    assert_eq!(b_links(&h), b);
    let written = h.nodes[&rest[0]].create(&volume("healed")).await.unwrap();
    h.get(4).wait_revision(written, WAIT).await.unwrap();

    for (_, c) in std::mem::take(&mut h.nodes) {
        c.shutdown().await.unwrap();
    }
}

// ---- 링크 맞추기: 리더의 `VoterKeeper`가 노드가 적은 링크(`NodeStatus.cluster_addresses`)로 멤버십과 관찰 노드
// 목록의 링크를 고친다.

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

/// 노드 하나: 저장소, 리스 갱신(`renewing`이 참일 때), VoterKeeper, RoleKeeper
struct Kept {
    cluster: Arc<Cluster>,
    renewing: Arc<AtomicBool>,
    renew: JoinHandle<()>,
    voters: VoterKeeper,
    roles: RoleKeeper,
}

/// 노드 1~3은 첫 링크 하나(`n{i}-a`)로 만든 투표 멤버이고 모든 기능과 `cluster_addresses = [n{i}-a]`를 적는다.
struct Keepers {
    router: MemRouter,
    root: TempDir,
    nodes: BTreeMap<NodeId, Kept>,
}

impl Keepers {
    async fn start() -> Self {
        let mut h = Keepers {
            router: MemRouter::default(),
            root: TempDir::new().unwrap(),
            nodes: BTreeMap::new(),
        };
        for i in 1..=3 {
            h.open(i).await;
        }
        let voters = (1..=3)
            .map(|i| (id(i), OrbNode::single(link(i, "a"))))
            .collect();
        h.get(1).initialize(voters).await.unwrap();
        eventually("a leader", || async { h.leader().is_some() }).await;
        let info = Resource::new(
            "c",
            ClusterSpec {
                ca_cert_pem: "CERT".into(),
                ca_key_pem: "KEY".into(),
            },
        );
        h.leader_cluster().create(&info).await.unwrap();
        for i in 1..=3 {
            h.register(i, &[link(i, "a")], true).await;
        }
        h
    }

    async fn open(&mut self, i: u64) {
        let node = id(i);
        let mut config = ClusterConfig::new(node, self.root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = WAIT;
        config.link_retry = RETRY;
        let cluster = Cluster::open_node(
            config,
            self.router.network(node),
            self.router.follower(node),
        )
        .await
        .unwrap();
        let cluster = Arc::new(cluster.with_forwarder(self.router.forwarder(node)));
        self.router.register(node, &cluster);
        let renewing = Arc::new(AtomicBool::new(true));
        let renew = {
            let (cluster, renewing) = (cluster.clone(), renewing.clone());
            tokio::spawn(async move {
                loop {
                    if renewing.load(Ordering::SeqCst) {
                        let _ = cluster.renew_lease(RENEW * 2).await;
                    }
                    tokio::time::sleep(RENEW).await;
                }
            })
        };
        let roles = RoleKeeperConfig {
            interval: Duration::from_millis(100),
            stalled_after: Duration::from_secs(2),
        };
        self.nodes.insert(
            node,
            Kept {
                voters: VoterKeeper::spawn(cluster.clone(), keeper_config()),
                roles: RoleKeeper::spawn(cluster.clone(), roles),
                cluster,
                renewing,
                renew,
            },
        );
    }

    fn get(&self, i: u64) -> &Arc<Cluster> {
        &self.nodes[&id(i)].cluster
    }

    fn leader(&self) -> Option<NodeId> {
        self.nodes.values().find_map(|n| {
            let m = n.cluster.raft()?.metrics().borrow().clone();
            (m.state == openraft::ServerState::Leader).then_some(n.cluster.id())
        })
    }

    fn leader_cluster(&self) -> &Arc<Cluster> {
        &self.nodes[&self.leader().expect("a leader")].cluster
    }

    /// 리더의 멤버십에 있는 노드의 링크(없으면 빈 목록)
    fn links_of(&self, i: u64) -> Vec<String> {
        let Some(raft) = self.leader().and_then(|l| self.nodes[&l].cluster.raft()) else {
            return Vec::new();
        };
        let m = raft.metrics().borrow().clone();
        m.membership_config
            .membership()
            .get_node(&id(i))
            .map(|n| n.addrs.clone())
            .unwrap_or_default()
    }

    /// 리더 저장소의 클러스터 정보 status
    fn cluster_status(&self) -> ClusterStatus {
        self.leader_cluster().list::<ClusterSpec>().unwrap().1[0]
            .status
            .clone()
    }

    /// 노드 `i`의 Node 기록: 적은 링크 `links`, `multi_link`면 그 기능도
    async fn register(&self, i: u64, links: &[String], multi_link: bool) {
        let mut features = vec![VOTER_LIMIT.to_string(), OBSERVER.into()];
        if multi_link {
            features.push(MULTI_LINK.into());
        }
        let status = NodeStatus {
            online: true,
            features,
            cluster_addresses: links.to_vec(),
            ..NodeStatus::default()
        };
        let leader = self.leader_cluster();
        match leader.get::<NodeSpec>(id(i).0).unwrap() {
            Some(_) => {
                leader
                    .update_status::<NodeSpec>(id(i).0, &status)
                    .await
                    .unwrap();
            }
            None => {
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
                node.status = status;
                leader.create(&node).await.unwrap();
            }
        }
    }

    async fn close(mut self) {
        for (_, n) in std::mem::take(&mut self.nodes) {
            n.renew.abort();
            n.roles.shutdown().await;
            n.voters.shutdown().await;
            n.cluster.shutdown().await.unwrap();
        }
    }
}

/// 노드가 링크를 더하면 리더가 멤버십을 고치고(그 뒤 첫 링크를 끊어도 된다), 순서를 바꾸면 첫 링크도 바뀐다. 옛
/// 리더가 링크를 떨어뜨린 멤버십(`SetNodes`로 링크 하나)도 다시 맞춘다.
#[tokio::test]
async fn the_leader_adopts_the_links_the_nodes_report() {
    let h = Keepers::start().await;
    let two = |i: u64| vec![link(i, "a"), link(i, "b")];
    // 적은 링크가 멤버십과 같으면 아무것도 하지 않는다.
    tokio::time::sleep(Duration::from_secs(1)).await;
    for i in 1..=3 {
        assert_eq!(h.links_of(i), vec![link(i, "a")]);
    }
    for i in 1..=3 {
        h.register(i, &two(i), true).await;
    }
    eventually("every member on two links", || async {
        (1..=3).all(|i| h.links_of(i) == two(i))
    })
    .await;
    // 모든 노드가 새 링크를 링크 책에서 찾는다.
    eventually("the link books", || async {
        (1..=3).all(|n| (1..=3).all(|i| h.get(n).link_book().links(&link(i, "a")) == two(i)))
    })
    .await;
    assert_eq!(h.get(2).members().links[&id(3)], two(3));

    // 이제 모든 노드 사이의 우선 링크를 끊어도 쓰기가 된다.
    for from in 1..=3 {
        for to in 1..=3 {
            h.router.cut_link(id(from), &link(to, "a"));
        }
    }
    let leader = h.leader().unwrap();
    let follower = (1..=3).find(|i| id(*i) != leader).unwrap();
    h.get(follower).create(&volume("cut")).await.unwrap();
    for from in 1..=3 {
        h.router.heal(id(from));
    }

    // 순서를 바꾸면 첫 링크(멤버 목록의 주소)도 바뀐다.
    h.register(3, &[link(3, "b"), link(3, "a")], true).await;
    eventually("node 3 prefers its second network", || async {
        h.links_of(3) == vec![link(3, "b"), link(3, "a")]
    })
    .await;
    eventually("the new address", || async {
        h.get(1).members().addr(id(3)) == Some(link(3, "b").as_str())
    })
    .await;

    // 옛 리더처럼 링크 하나로 다시 쓴 멤버십을 다시 맞춘다(앞선 변경이 아직 커밋 중이면 다시 한다).
    let raft = h.leader_cluster().raft().unwrap();
    let old = ChangeMembers::SetNodes(BTreeMap::from([(id(2), OrbNode::single(link(2, "a")))]));
    let deadline = Instant::now() + WAIT;
    while let Err(e) = raft.change_membership(old.clone(), false).await {
        assert!(Instant::now() < deadline, "{e}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eventually("node 2 back on two links", || async {
        h.links_of(2) == two(2)
    })
    .await;
    h.get(2).create(&volume("after")).await.unwrap();
    h.close().await;
}

/// `multi-link`를 알리지 않는 노드가 하나라도 있으면 링크를 맞추지 않는다.
#[tokio::test]
async fn links_wait_until_every_node_knows_the_feature() {
    let h = Keepers::start().await;
    h.register(3, &[link(3, "a")], false).await;
    h.register(1, &[link(1, "a"), link(1, "b")], true).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.links_of(1), vec![link(1, "a")]);
    h.register(3, &[link(3, "a")], true).await;
    eventually("node 1 on two links", || async {
        h.links_of(1) == vec![link(1, "a"), link(1, "b")]
    })
    .await;
    h.close().await;
}

/// 관찰 노드의 링크는 클러스터 정보의 `observer_links`에 쓰고, 관찰 노드를 learner로 되돌리면 그 링크로 더한다.
#[tokio::test]
async fn observer_links_live_in_the_cluster_info() {
    let mut h = Keepers::start().await;
    h.open(4).await;
    let four = vec![link(4, "a"), link(4, "b")];
    // 4는 링크 하나로 learner가 된 뒤 목록에 올라 관찰 노드가 된다. 리스를 갱신하지 않는 동안은 건강하지 않아
    // learner로 되돌리지 않는다.
    h.nodes[&id(4)].renewing.store(false, Ordering::SeqCst);
    h.register(4, &[link(4, "a")], true).await;
    h.leader_cluster()
        .add_learner(id(4), OrbNode::single(link(4, "a")))
        .await
        .unwrap();
    let info = h.leader_cluster().list::<ClusterSpec>().unwrap().1[0].clone();
    let status = ClusterStatus {
        observers: BTreeMap::from([(id(4).0, link(4, "a"))]),
        ..ClusterStatus::default()
    };
    h.leader_cluster()
        .update_status::<ClusterSpec>(info.id, &status)
        .await
        .unwrap();
    eventually("node 4 an observer", || async {
        h.get(4).mode() == NodeMode::Observer && h.links_of(4).is_empty()
    })
    .await;
    // 관찰 노드가 링크를 더하면 키퍼가 목록에 쓰고(첫 링크는 그대로), 모든 노드가 그것을 안다.
    h.register(4, &four, true).await;
    eventually("the observer links", || async {
        h.cluster_status().observer_links.get(&id(4).0) == Some(&four)
    })
    .await;
    assert_eq!(h.cluster_status().observers[&id(4).0], link(4, "a"));
    eventually("the links of the observer", || async {
        (1..=4).all(|n| {
            h.get(n).members().links.get(&id(4)) == Some(&four)
                && h.get(n).link_book().links(&link(4, "a")) == four
        })
    })
    .await;

    // 리스를 갱신하면 투표 멤버가 상한보다 적어 learner로 돌아온다(목록의 링크 그대로). 목록의 링크도 빠진다.
    // 링크 맞추기가 고친 것이 아니게 4가 기능을 내린 채로 본다.
    h.register(4, &four, false).await;
    h.nodes[&id(4)].renewing.store(true, Ordering::SeqCst);
    eventually("node 4 back in Raft on two links", || async {
        h.links_of(4) == four && h.cluster_status() == ClusterStatus::default()
    })
    .await;
    h.close().await;
}
