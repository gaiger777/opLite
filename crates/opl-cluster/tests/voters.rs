//! 투표 멤버 상한(`VoterKeeper`): 노드 일곱·상한 다섯에서 zone을 나눠 내리고, 죽은 투표 멤버를 learner로
//! 바꾸고, 과반이 없으면 바꾸지 않는다. 옛 노드(기능 없음)가 섞이면 움직이지 않는다. 끊긴 두 단계 변경을
//! 마무리하고(API로 지운 노드는 learner로 남기지 않는다), 바꿀 learner가 없으면 오프라인 투표 멤버를 그대로
//! 두고, 새 리더는 살아 있는 투표 멤버를 내리지 않는다.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use openraft::ChangeMembers;
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode};
use opl_cluster::{
    Cluster, ClusterConfig, Members, VOTER_LIMIT, VoterKeeper, VoterKeeperConfig, ZONE_LABEL,
};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
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

/// 노드 하나: 저장소, 리스 갱신, VoterKeeper
struct Running {
    cluster: Arc<Cluster>,
    renew: JoinHandle<()>,
    keeper: VoterKeeper,
}

struct Harness {
    router: MemRouter,
    nodes: BTreeMap<NodeId, Running>,
    _root: TempDir,
}

impl Harness {
    /// 노드 `1..=n`을 모두 투표 멤버로 띄우고 리더를 기다린다.
    async fn start(n: u64) -> Self {
        Self::start_with(n, keeper_config()).await
    }

    async fn start_with(n: u64, keeper: VoterKeeperConfig) -> Self {
        let root = TempDir::new().unwrap();
        let router = MemRouter::default();
        let mut nodes = BTreeMap::new();
        for i in 1..=n {
            let id = NodeId::from(i);
            let mut config = ClusterConfig::new(id, root.path().join(i.to_string()));
            config.raft = openraft::Config {
                heartbeat_interval: 50,
                election_timeout_min: 1000,
                election_timeout_max: 2000,
                ..Default::default()
            };
            config.write_timeout = Duration::from_secs(10);
            let cluster = Cluster::open(config, router.network(id))
                .await
                .unwrap()
                .with_forwarder(router.forwarder(id));
            router.register(id, &cluster);
            let cluster = Arc::new(cluster);
            let renew = {
                let cluster = cluster.clone();
                tokio::spawn(async move {
                    loop {
                        let _ = cluster.renew_lease(RENEW * 2).await;
                        tokio::time::sleep(RENEW).await;
                    }
                })
            };
            let keeper = VoterKeeper::spawn(cluster.clone(), keeper.clone());
            nodes.insert(
                id,
                Running {
                    cluster,
                    renew,
                    keeper,
                },
            );
        }
        let members = (1..=n)
            .map(|i| (NodeId::from(i), OrbNode::single(format!("node-{i}"))))
            .collect();
        nodes[&NodeId::from(1)]
            .cluster
            .initialize(members)
            .await
            .unwrap();
        let h = Harness {
            router,
            nodes,
            _root: root,
        };
        eventually("a leader", || async { h.leader().is_some() }).await;
        h
    }

    fn any(&self) -> &Cluster {
        &self.nodes.values().next().unwrap().cluster
    }

    /// 모든 노드가 같은 리더를 알면 그 리더(멈춘 노드를 아직 리더로 알면 None)
    fn leader(&self) -> Option<NodeId> {
        let leader = self.any().members().leader?;
        (self.nodes.contains_key(&leader)
            && self
                .nodes
                .values()
                .all(|n| n.cluster.members().leader == Some(leader)))
        .then_some(leader)
    }

    /// 리더가 아는 투표 멤버(두 단계 변경 중이면 None)
    fn voters(&self) -> Option<BTreeSet<NodeId>> {
        let leader = self.leader()?;
        let m = self.nodes[&leader]
            .cluster
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let membership = m.membership_config.membership();
        (membership.get_joint_config().len() == 1).then(|| membership.voter_ids().collect())
    }

    /// 리더가 아는 멤버십(다른 노드는 아직 적용하지 않았을 수 있다)
    fn leader_members(&self) -> Option<Members> {
        Some(self.nodes[&self.leader()?].cluster.members())
    }

    /// 클러스터 설정 `voters`
    async fn set_voters(&self, n: u8) {
        let spec = SettingsSpec {
            voters: Some(n),
            ..SettingsSpec::default()
        };
        match self
            .any()
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
        {
            None => {
                self.any()
                    .create(&Resource::new(SETTINGS_NAME, spec))
                    .await
                    .unwrap();
            }
            Some(s) => {
                self.any()
                    .update_spec(s.id, None, s.labels, &spec)
                    .await
                    .unwrap();
            }
        }
    }

    /// 노드 `i`의 Node 리소스(zone 레이블, 기능)
    async fn register(&self, i: u64, zone: &str, feature: bool) {
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
        node.labels.insert(ZONE_LABEL.into(), zone.into());
        node.status = status(feature);
        self.any().create(&node).await.unwrap();
    }

    /// 노드를 멈춘다(프로세스 종료에 해당).
    async fn stop(&mut self, id: NodeId) {
        let node = self.nodes.remove(&id).unwrap();
        node.renew.abort();
        node.keeper.shutdown().await;
        self.router.unregister(id);
        node.cluster.shutdown().await.unwrap();
    }

    async fn close(mut self) {
        for id in self.nodes.keys().copied().collect::<Vec<_>>() {
            self.stop(id).await;
        }
    }
}

fn status(feature: bool) -> NodeStatus {
    NodeStatus {
        online: true,
        features: if feature {
            vec![VOTER_LIMIT.into()]
        } else {
            vec![]
        },
        ..NodeStatus::default()
    }
}

/// `cond`가 참이 될 때까지 20초까지 기다린다.
async fn eventually<F, Fut>(what: &str, cond: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn zones(voters: &BTreeSet<NodeId>, zone: &BTreeMap<NodeId, &str>) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for v in voters {
        *out.entry(zone[v].to_string()).or_default() += 1;
    }
    out
}

/// 노드 일곱(zone a 셋, b 둘, c 둘)이 모두 투표 멤버로 시작한다. 옛 노드가 섞여 있으면 그대로 두고, 모두
/// 기능을 알면 zone이 고르게 다섯으로 줄인다. 투표 멤버 하나를 죽이면 learner로 내리고 다른 노드를 올린다.
/// 상한을 3으로 바꾸면 zone마다 하나씩 남는다.
#[tokio::test]
async fn seven_nodes_keep_five_voters_spread_over_zones() {
    let mut h = Harness::start(7).await;
    let zone: BTreeMap<NodeId, &str> = [
        (1, "a"),
        (2, "a"),
        (3, "a"),
        (4, "b"),
        (5, "b"),
        (6, "c"),
        (7, "c"),
    ]
    .into_iter()
    .map(|(i, z)| (NodeId::from(i), z))
    .collect();
    // 노드 7은 옛 노드다(기능을 모른다).
    for i in 1..=7 {
        h.register(i, zone[&NodeId::from(i)], i != 7).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.voters().map(|v| v.len()), Some(7));

    // 노드 7도 업그레이드했다.
    h.any()
        .update_status::<NodeSpec>(NodeId::from(7).0, &status(true))
        .await
        .unwrap();
    eventually("five voters", || async {
        h.voters().is_some_and(|v| v.len() == 5)
    })
    .await;
    let voters = h.voters().unwrap();
    let spread = zones(&voters, &zone);
    assert_eq!(spread.len(), 3, "{spread:?}");
    assert!(spread.values().all(|n| *n <= 2), "{spread:?}");
    // 내린 노드는 learner로 남는다(다른 노드는 아직 적용하지 않았을 수 있어 리더의 멤버십으로 본다).
    assert_eq!(h.leader_members().unwrap().learners.len(), 2);

    // 리더가 아닌 투표 멤버 하나를 죽이면 오프라인이 된 뒤 learner로 내리고, 다른 노드가 올라온다.
    let leader = h.leader().unwrap();
    let dead = *voters.iter().find(|v| **v != leader).unwrap();
    h.stop(dead).await;
    eventually("the dead voter replaced", || async {
        h.voters()
            .is_some_and(|v| v.len() == 5 && !v.contains(&dead))
    })
    .await;
    let members = h.leader_members().unwrap();
    assert!(members.learners.contains_key(&dead), "{members:?}");
    let spread = zones(&h.voters().unwrap(), &zone);
    assert_eq!(spread.len(), 3, "{spread:?}");

    // 상한을 셋으로 바꾸면 zone마다 하나가 남는다.
    h.set_voters(3).await;
    eventually("three voters", || async {
        h.voters().is_some_and(|v| v.len() == 3)
    })
    .await;
    let spread = zones(&h.voters().unwrap(), &zone);
    assert_eq!(spread.values().copied().collect::<Vec<_>>(), [1, 1, 1]);
    h.close().await;
}

/// 투표 멤버 다섯 가운데 셋이 죽어 과반이 없으면 오프라인이 되어도 멤버십을 바꾸지 않는다(바꿀 수도 없다).
#[tokio::test]
async fn nothing_changes_without_a_majority() {
    let mut h = Harness::start(5).await;
    for i in 1..=5 {
        h.register(i, "a", true).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let leader = h.leader().unwrap();
    let before = h.voters().unwrap();
    assert_eq!(before.len(), 5);
    let doomed: Vec<NodeId> = before
        .iter()
        .copied()
        .filter(|v| *v != leader)
        .take(3)
        .collect();
    for id in doomed {
        h.stop(id).await;
    }
    // 오프라인(2초)이 지나도 리더가 아는 멤버십은 그대로다(두 단계 변경도 시작하지 않는다).
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        let m = h.nodes[&leader]
            .cluster
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let membership = m.membership_config.membership();
        assert_eq!(membership.get_joint_config().len(), 1);
        assert_eq!(membership.voter_ids().collect::<BTreeSet<_>>(), before);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    h.close().await;
}

/// 노드가 상한 이하면 오프라인 투표 멤버도 내리지 않는다(바꿀 learner가 없다): 노드 셋에서 하나가 오래 꺼져도
/// 셋 모두 투표 멤버로 남는다(내리면 그 뒤 하나만 더 죽어도 정족수가 깨진다).
#[tokio::test]
async fn an_offline_voter_without_a_replacement_stays_a_voter() {
    let mut h = Harness::start(3).await;
    for i in 1..=3 {
        h.register(i, "a", true).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let leader = h.leader().unwrap();
    let before = h.voters().unwrap();
    assert_eq!(before.len(), 3);
    let dead = *before.iter().find(|v| **v != leader).unwrap();
    h.stop(dead).await;
    // 오프라인(2초)이 지나도 그대로다.
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        let m = h.nodes[&leader]
            .cluster
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let membership = m.membership_config.membership();
        assert_eq!(membership.get_joint_config().len(), 1);
        assert_eq!(membership.voter_ids().collect::<BTreeSet<_>>(), before);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    h.close().await;
}

/// 두 단계 변경의 첫 단계(joint)를 팔로워가 받은 뒤 리더가 죽으면 새 리더는 joint를 커밋하지만 두 번째 단계는
/// 오지 않는다. 새 리더의 키퍼가 그것을 uniform으로 마무리한다(그 뒤 상한을 넘는 죽은 옛 리더를 내린다).
#[tokio::test]
async fn a_new_leader_finishes_a_joint_change_left_by_the_old_one() {
    let mut h = Harness::start(4).await;
    for i in 1..=4 {
        h.register(i, "a", true).await;
    }
    h.set_voters(3).await;
    eventually("three voters", || async {
        h.voters().is_some_and(|v| v.len() == 3)
    })
    .await;
    let leader = h.leader().unwrap();
    let voters = h.voters().unwrap();
    let learner = *h.nodes.keys().find(|id| !voters.contains(id)).unwrap();
    let followers: Vec<NodeId> = voters.iter().copied().filter(|v| *v != leader).collect();
    // 앞선 변경(내리기)이 모든 노드에 적용되기를 기다린다. 아니면 리더가 그 커밋을 모른 채 다음 변경을 거절한다.
    let config_index = |id: &NodeId| {
        let m = h.nodes[id]
            .cluster
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let index = m.membership_config.log_id().map(|l| l.index);
        (index, m.last_applied.map(|l| l.index) >= index)
    };
    let before = config_index(&leader).0;
    eventually("the last change applied everywhere", || async {
        h.nodes.keys().all(|id| config_index(id) == (before, true))
    })
    .await;
    // 리더가 보낸 항목은 닿지만 답이 사라져 리더는 joint의 커밋을 모르고 두 번째 단계를 내지 않는다.
    h.router.lose_replies(leader);
    let raft = h.nodes[&leader].cluster.raft().unwrap().clone();
    let change = tokio::spawn(async move {
        let add = ChangeMembers::AddVoterIds(BTreeSet::from([learner]));
        let _ = raft.change_membership(add, true).await;
    });
    eventually("the followers have the joint config", || async {
        followers.iter().all(|f| {
            let m = h.nodes[f]
                .cluster
                .raft()
                .unwrap()
                .metrics()
                .borrow()
                .clone();
            m.membership_config.log_id().map(|l| l.index) > before
                && m.membership_config.membership().get_joint_config().len() == 2
        })
    })
    .await;
    change.abort();
    h.stop(leader).await;
    // 마무리한 구성은 joint의 새 구성(옛 리더도 든다)이다. 새 임기의 처음에는 내리지 않으므로 잠시 보인다.
    let finished: BTreeSet<NodeId> = voters.iter().copied().chain([learner]).collect();
    eventually("the joint config finished", || async {
        h.voters().is_some_and(|v| v == finished)
    })
    .await;
    eventually("a uniform config without the old leader", || async {
        h.voters()
            .is_some_and(|v| v.len() == 3 && !v.contains(&leader) && v.contains(&learner))
    })
    .await;
    h.close().await;
}

/// 노드를 지우는 변경(`LeaderRequest::Remove`, learner로 남기지 않음)의 두 번째 단계가 늦어 키퍼가 joint를 먼저
/// 마무리하면(learner로 남긴다) 늦은 두 번째 단계는 할 일이 없다. 지우는 요청은 그 뒤 멤버십을 다시 읽어 learner로
/// 남은 노드를 지운다.
#[tokio::test]
async fn a_removed_node_does_not_stay_a_learner_when_the_keeper_finishes_first() {
    let config = VoterKeeperConfig {
        change_timeout: Duration::from_secs(1),
        ..keeper_config()
    };
    let mut h = Harness::start_with(4, config).await;
    let leader = h.leader().unwrap();
    let gone = *h.nodes.keys().find(|id| **id != leader).unwrap();
    let rest: BTreeSet<NodeId> = h.nodes.keys().copied().filter(|id| *id != gone).collect();
    h.stop(gone).await;
    // 지우는 요청은 따로 된 실행기에서 한다. 첫 단계를 보낸 뒤 그 실행기의 하나뿐인 스레드를 3초 막아 두 번째
    // 단계를 늦춘다(Raft와 키퍼는 이 시험의 실행기에서 돈다).
    let cluster = h.nodes[&leader].cluster.clone();
    let (tx, mut removed) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let remove =
                tokio::spawn(
                    async move { cluster.request(LeaderRequest::Remove { id: gone }).await },
                );
            tokio::spawn(async { std::thread::sleep(Duration::from_secs(3)) });
            let _ = tx.send(remove.await.unwrap());
        });
    });
    // 키퍼가 먼저 마무리해 지운 노드가 learner로 보인다.
    eventually("the keeper finished the joint config", || async {
        h.voters().is_some_and(|v| v == rest)
    })
    .await;
    assert!(
        h.leader_members().unwrap().learners.contains_key(&gone),
        "{:?}",
        h.leader_members()
    );
    assert!(removed.try_recv().is_err(), "the removal was not delayed");
    removed.await.unwrap().unwrap();
    eventually("the removed node gone", || async {
        h.leader_members()
            .is_some_and(|m| !m.voters.contains_key(&gone) && !m.learners.contains_key(&gone))
    })
    .await;
    h.close().await;
}

/// 새 리더는 리스 표가 비어 죽은 투표 멤버도 한동안 건강하다고 본다. 그때 붐비는 zone의 살아 있는 투표 멤버를
/// 내리면 바꾼 뒤의 구성에서 살아 있는 것이 과반이 아니라 joint가 커밋되지 못하고 클러스터가 멈춘다. 키퍼는
/// 실제로 받은 갱신으로 과반을 세고, 새 임기의 처음에는 개수 때문에 내리지 않는다.
#[tokio::test]
async fn a_new_leader_does_not_demote_live_voters_while_dead_ones_look_healthy() {
    let config = VoterKeeperConfig {
        healthy_within: Duration::from_secs(3),
        offline_after: Duration::from_secs(6),
        ..keeper_config()
    };
    let mut h = Harness::start_with(5, config).await;
    let leader = h.leader().unwrap();
    let other = *h.nodes.keys().find(|id| **id != leader).unwrap();
    let survivors: BTreeSet<NodeId> = h
        .nodes
        .keys()
        .copied()
        .filter(|id| *id != leader && *id != other)
        .collect();
    // 죽을 둘(리더와 하나)은 혼자인 zone에, 남을 셋은 붐비는 zone a에 둔다. 남을 하나는 옛 노드라 그동안 키퍼가
    // 쉰다(상한 3이어도 다섯 그대로).
    let old = *survivors.iter().next().unwrap();
    for i in 1..=5 {
        let id = NodeId::from(i);
        let zone = if id == leader {
            "b"
        } else if id == other {
            "c"
        } else {
            "a"
        };
        h.register(i, zone, id != old).await;
    }
    h.set_voters(3).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.voters().map(|v| v.len()), Some(5));
    h.stop(leader).await;
    h.stop(other).await;
    // 옛 노드도 업그레이드했다. 새 리더의 키퍼가 움직이기 시작한다.
    let via = *survivors.iter().last().unwrap();
    h.nodes[&via]
        .cluster
        .update_status::<NodeSpec>(old.0, &status(true))
        .await
        .unwrap();
    eventually("only the live voters left", || async {
        h.voters().is_some_and(|v| v == survivors)
    })
    .await;
    h.close().await;
}

/// 합류하자마자 죽어 `voter-limit`를 보고하지 못한 learner(기능·버전 없는 Node 기록, 리스 없음)가 있어도 키퍼는
/// 움직인다.
#[tokio::test]
async fn a_learner_that_never_registered_does_not_stop_the_keeper() {
    let h = Harness::start(4).await;
    for i in 1..=4 {
        h.register(i, "a", true).await;
    }
    h.set_voters(3).await;
    eventually("three voters", || async {
        h.voters().is_some_and(|v| v.len() == 3)
    })
    .await;
    // 합류 처리(Node 기록, learner 추가)만 되고 노드는 뜨지 못했다.
    let ghost = NodeId::from(9);
    let mut node = Resource::new(
        "n9",
        NodeSpec {
            address: "node-9:8006".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.id = ghost.0;
    h.any().create(&node).await.unwrap();
    let leader = h.leader().unwrap();
    h.nodes[&leader]
        .cluster
        .raft()
        .unwrap()
        .add_learner(ghost, OrbNode::single("node-9"), false)
        .await
        .unwrap();
    // 상한을 다섯으로 올리면 살아 있는 learner를 다시 올린다.
    h.set_voters(5).await;
    eventually("four voters", || async {
        h.voters()
            .is_some_and(|v| v.len() == 4 && !v.contains(&ghost))
    })
    .await;
    h.close().await;
}
