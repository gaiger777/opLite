//! 클러스터 통신 다중 링크(설계 §7): 노드마다 링크 둘(`n{i}-a` 우선, `n{i}-b`)로 멤버십을 만들고, 모든 노드
//! 사이의 우선 링크를 끊어도 리더·쓰기·리스 갱신·관찰 노드의 Follow가 둘째 링크로 이어지며, 우선 링크를 풀면
//! 재시도 간격 뒤 다시 우선 링크를 쓴다.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use opl_cluster::memnet::MemRouter;
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
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
