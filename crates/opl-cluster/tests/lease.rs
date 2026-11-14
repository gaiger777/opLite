//! 리더의 HA 리스 표: 갱신, 고립된 노드와 리더, 펜싱 중 거부, 임기가 바뀌면 비우기, 동시 갱신(HA·VIP)의
//! 리더 확인 묶기, 갱신에 실린 HA 검사 결과와 부하 요약.

mod common;

use std::time::Duration;

use common::{TestCluster, WAIT, volume};
use openraft::ServerState;
use opl_cluster::types::{LeaderRequest, NodeId, Rejection};
use opl_cluster::{ClusterError, MAX_LEASE_CHECKS};
use opl_types::ha_check::{CheckReport, CheckState};
use opl_types::placement::{LoadSummary, MAX_LOAD_VMS, VmLoad};
use opl_types::storage::VolumeSpec;
use tokio::time::Instant;

/// 실패해야 하는 갱신에 주는 제한 시간. 선거 제한 시간(1초 이상)보다 짧아 그 사이 재선거가 없다.
const SHORT: Duration = Duration::from_millis(300);

fn follower_of(tc: &TestCluster, leader: NodeId) -> NodeId {
    *tc.nodes.keys().find(|id| **id != leader).unwrap()
}

fn term(tc: &TestCluster, id: NodeId) -> u64 {
    tc.get(id).raft().unwrap().metrics().borrow().current_term
}

#[tokio::test]
async fn leader_records_renewals_from_followers_and_itself() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    let before = Instant::now();
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    tc.get(leader).renew_lease(WAIT).await.unwrap();
    let after = Instant::now();

    let leases = tc.get(leader).leases();
    assert_eq!(leases.term, term(&tc, leader));
    assert_eq!(leases.renewed.len(), 2, "{leases:?}");
    for id in [follower, leader] {
        let r = leases.renewed[&id];
        assert!(
            before <= r && r <= after,
            "{id}: {r:?} not in renewal window"
        );
    }
    // 팔로워는 표를 갖지 않는다(갱신은 리더가 받는다).
    assert!(tc.get(follower).leases().renewed.is_empty());
}

fn report(name: &str, state: CheckState) -> CheckReport {
    CheckReport::new(name, state, None, None)
}

/// 노드가 둔 검사 결과는 갱신마다 실려 가 리더 표의 그 노드 결과를 통째로 바꾼다. 빈 목록은 결과 없음이다.
#[tokio::test]
async fn renewals_carry_the_nodes_check_results() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    // 결과를 두지 않은 노드는 표에 없다.
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert!(tc.get(leader).leases().checks.is_empty());

    let first = vec![
        report("stor", CheckState::Ok),
        CheckReport::new(
            "vmnet",
            CheckState::Failing,
            Some("no carrier".into()),
            Some(3),
        ),
    ];
    tc.get(follower).set_lease_checks(first.clone());
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    let leases = tc.get(leader).leases();
    assert_eq!(leases.checks[&follower], first);
    assert_eq!(leases.checks.len(), 1, "{leases:?}");
    // 결과는 다음 갱신에도 그대로 실린다.
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert_eq!(tc.get(leader).leases().checks[&follower], first);

    let second = vec![report("vmnet", CheckState::Ok)];
    tc.get(follower).set_lease_checks(second.clone());
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert_eq!(tc.get(leader).leases().checks[&follower], second);

    // 너무 많으면 앞의 것만 둔다.
    let many: Vec<_> = (0..MAX_LEASE_CHECKS + 3)
        .map(|i| report(&format!("c{i}"), CheckState::Ok))
        .collect();
    tc.get(follower).set_lease_checks(many.clone());
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert_eq!(
        tc.get(leader).leases().checks[&follower],
        many[..MAX_LEASE_CHECKS]
    );

    tc.get(follower).set_lease_checks(vec![]);
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert!(!tc.get(leader).leases().checks.contains_key(&follower));
    // 팔로워는 표를 갖지 않는다.
    assert!(tc.get(follower).leases().checks.is_empty());
}

fn load(cpu: u16, vms: u64) -> LoadSummary {
    LoadSummary {
        cpu1: Some(cpu),
        cpu5: Some(cpu),
        mem: Some(1024),
        vms: (0..vms)
            .map(|i| VmLoad {
                id: uuid::Uuid::from_u64_pair(1, i),
                cpu: Some(100),
                mem: Some(i),
            })
            .collect(),
    }
}

/// 노드가 둔 부하 요약은 다음 갱신 한 번에 실려 가 리더 표에 받은 시각과 함께 적힌다. 새 요약이 없는 갱신은 싣지
/// 않고 리더는 지난 것을 둔다(받은 시각이 그대로). 리더가 받지 못한 요약은 다음 갱신이 다시 싣는다.
#[tokio::test]
async fn renewals_carry_a_new_load_summary_once() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    // 요약을 두지 않은 노드는 표에 없다.
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert!(tc.get(leader).leases().loads.is_empty());

    let first = load(120, 2);
    tc.get(follower).set_lease_load(first.clone());
    assert_eq!(tc.get(follower).lease_load(), Some(first.clone()));
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    let leases = tc.get(leader).leases();
    assert_eq!(leases.loads.len(), 1, "{leases:?}");
    let got = leases.loads[&follower].clone();
    assert_eq!(got.load, first);
    assert_eq!(got.at, leases.renewed[&follower]);

    // 새 요약이 없으면 싣지 않는다: 리더의 요약은 그대로이고 받은 시각도 바뀌지 않는다.
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    let leases = tc.get(leader).leases();
    assert!(leases.renewed[&follower] > got.at);
    assert_eq!(leases.loads[&follower], got);

    let second = load(700, 1);
    tc.get(follower).set_lease_load(second.clone());
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    let leases = tc.get(leader).leases();
    assert_eq!(leases.loads[&follower].load, second);
    assert!(leases.loads[&follower].at > got.at);

    // 리더에 닿지 못한 요약은 다음 갱신이 싣는다.
    let third = load(300, 0);
    tc.router.isolate(follower);
    tc.get(follower).set_lease_load(third.clone());
    assert!(tc.get(follower).renew_lease(SHORT).await.is_err());
    tc.router.heal(follower);
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert_eq!(tc.get(leader).leases().loads[&follower].load, third);
    // 팔로워는 표를 갖지 않는다.
    assert!(tc.get(follower).leases().loads.is_empty());
}

/// VM이 너무 많은 요약은 노드가 둘 때와 리더가 받을 때 모두 RSS가 큰 `MAX_LOAD_VMS`개로 자른다.
#[tokio::test]
async fn load_summaries_keep_at_most_the_largest_vms() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);
    let many = load(100, MAX_LOAD_VMS as u64 + 1);

    tc.get(follower).set_lease_load(many.clone());
    let kept = tc.get(follower).lease_load().unwrap();
    assert_eq!(kept.vms.len(), MAX_LOAD_VMS);
    // RSS가 가장 작은 VM(mem 0)이 빠진다.
    assert!(kept.vms.iter().all(|v| v.mem != Some(0)));

    // 자르지 않은 요약(새 노드나 다른 버전)도 리더가 자른다.
    tc.get(leader)
        .execute_as_leader(LeaderRequest::RenewLease {
            node: follower,
            checks: vec![],
            load: Some(many),
        })
        .await
        .unwrap();
    let got = tc.get(leader).leases().loads[&follower].load.clone();
    assert_eq!(got.vms.len(), MAX_LOAD_VMS);
    assert!(got.vms.iter().all(|v| v.mem != Some(0)));
}

/// 응답 인덱스가 이 노드에 적용되어야 성공이다. 복제가 막힌 노드는 리더에 닿아도 실패한다.
#[tokio::test]
async fn renewal_waits_until_the_leaders_index_is_applied_here() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    tc.router.block_raft(follower);
    let v = volume("moved-while-away");
    tc.get(leader).create(&v).await.unwrap();
    let err = tc.get(follower).renew_lease(SHORT).await.unwrap_err();
    assert!(matches!(err, ClusterError::Timeout), "got {err:?}");
    // 리더는 갱신을 받았지만 노드는 그 사이의 쓰기를 아직 보지 못했으므로 성공이 아니다.
    assert!(tc.get(leader).leases().renewed.contains_key(&follower));
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_none());

    tc.router.heal(follower);
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    // 성공했다면 그 사이 리더가 쓴 것이 이미 보인다.
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_some());
}

#[tokio::test]
async fn isolated_follower_cannot_renew() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    tc.router.isolate(follower);
    let err = tc.get(follower).renew_lease(SHORT).await.unwrap_err();
    assert!(matches!(err, ClusterError::Timeout), "got {err:?}");
    assert!(!tc.get(leader).leases().renewed.contains_key(&follower));
}

/// 과반수와 끊긴 리더는 아직 자기를 리더로 알지만 자기 리스를 갱신하지 못한다.
#[tokio::test]
async fn isolated_leader_cannot_renew_its_own_lease() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    tc.get(leader).renew_lease(WAIT).await.unwrap();
    let first = tc.get(leader).leases().renewed[&leader];

    tc.router.isolate(leader);
    assert_eq!(
        tc.get(leader).raft().unwrap().metrics().borrow().state,
        ServerState::Leader
    );
    assert!(tc.get(leader).renew_lease(SHORT).await.is_err());
    assert_eq!(tc.get(leader).leases().renewed[&leader], first);
}

/// 모든 노드가 HA·VIP 리스를 한꺼번에 갱신해도(리더 확인을 함께 써도) 모두 성공하고 모두 적힌다.
#[tokio::test]
async fn concurrent_lease_and_vip_renewals_all_succeed() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let ids: Vec<_> = tc.nodes.keys().copied().collect();
    let (a, b, c) = (tc.get(ids[0]), tc.get(ids[1]), tc.get(ids[2]));
    let vip = || vec!["10.0.0.1".to_string()];

    let before = Instant::now();
    for _ in 0..5 {
        let r = tokio::join!(
            a.renew_lease(WAIT),
            b.renew_lease(WAIT),
            c.renew_lease(WAIT),
            a.renew_vip(vip(), WAIT),
            b.renew_vip(vip(), WAIT),
            c.renew_vip(vip(), WAIT),
        );
        for res in [r.0, r.1, r.2, r.3, r.4, r.5] {
            res.unwrap();
        }
    }
    let leases = tc.get(leader).leases();
    let vips = tc.get(leader).vip_leases();
    for id in ids {
        assert!(leases.renewed[&id] >= before, "{leases:?}");
        assert!(vips.renewed[&id].at >= before, "{vips:?}");
    }
}

/// 과반수와 끊긴 리더에게 한꺼번에 온 갱신은 함께 쓴 확인이 실패하므로 모두 실패한다.
#[tokio::test]
async fn concurrent_renewals_on_an_isolated_leader_all_fail() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    tc.get(leader).renew_lease(WAIT).await.unwrap();
    let first = tc.get(leader).leases().renewed[&leader];

    tc.router.isolate(leader);
    let l = tc.get(leader);
    let vip = || vec!["10.0.0.1".to_string()];
    let r = tokio::join!(
        l.renew_lease(SHORT),
        l.renew_lease(SHORT),
        l.renew_vip(vip(), SHORT),
        l.renew_vip(vip(), SHORT),
    );
    for res in [r.0, r.1, r.2, r.3] {
        assert!(res.is_err(), "{res:?}");
    }
    assert_eq!(tc.get(leader).leases().renewed[&leader], first);
    assert!(!tc.get(leader).vip_leases().renewed.contains_key(&leader));
}

#[tokio::test]
async fn fenced_node_is_rejected_until_the_mark_is_cleared() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    let first = tc.get(leader).leases().renewed[&follower];

    tc.get(leader).begin_fence(follower);
    let err = tc.get(follower).renew_lease(WAIT).await.unwrap_err();
    assert!(
        matches!(err, ClusterError::Rejected(Rejection::Fenced { node }) if node == follower),
        "got {err:?}"
    );
    assert_eq!(tc.get(leader).leases().renewed[&follower], first);
    // 다른 노드는 그대로 갱신한다.
    tc.get(leader).renew_lease(WAIT).await.unwrap();

    tc.get(leader).end_fence(follower);
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert!(tc.get(leader).leases().renewed[&follower] > first);
}

/// 리더십을 잃었다가 더 높은 임기에 다시 얻은 리더는 이전 임기의 갱신을 쓰지 않는다.
#[tokio::test]
async fn table_is_cleared_when_the_term_changes() {
    let tc = TestCluster::start_forwarding(3).await;
    let a = tc.leader().await;
    let others: Vec<_> = tc.nodes.keys().copied().filter(|id| *id != a).collect();
    let (b, c) = (others[0], others[1]);
    tc.get(b)
        .set_lease_checks(vec![report("stor", CheckState::Failing)]);
    tc.get(b).set_lease_load(load(500, 1));
    tc.get(b).renew_lease(WAIT).await.unwrap();
    let old_term = term(&tc, a);
    assert!(tc.get(a).leases().renewed.contains_key(&b));
    assert!(tc.get(a).leases().checks.contains_key(&b));
    assert!(tc.get(a).leases().loads.contains_key(&b));

    // 선거에 나설 수 있는 노드를 하나씩만 두어 리더를 a → b → a로 정해서 바꾼다.
    tc.get(c).raft().unwrap().runtime_config().elect(false);
    tc.router.isolate(a);
    assert_eq!(tc.leader_among(&[b, c]).await, b);
    tc.get(b).raft().unwrap().runtime_config().elect(false);
    tc.router.heal(a);
    // a가 b의 로그를 받아야 c가 a에게 투표한다.
    assert_eq!(tc.leader().await, b);
    let last = tc
        .get(b)
        .raft()
        .unwrap()
        .metrics()
        .borrow()
        .last_log_index
        .unwrap();
    tc.wait_applied(a, last).await;
    tc.router.isolate(b);
    assert_eq!(tc.leader_among(&[a, c]).await, a);

    // 다시 리더가 된 a의 표에는 이전 임기의 b 갱신과 검사 결과가 없다.
    tc.get(c).renew_lease(WAIT).await.unwrap();
    let leases = tc.get(a).leases();
    assert!(leases.term > old_term, "{leases:?}");
    assert_eq!(leases.term, term(&tc, a));
    assert_eq!(
        leases.renewed.keys().copied().collect::<Vec<_>>(),
        vec![c],
        "{leases:?}"
    );
    assert!(leases.checks.is_empty(), "{leases:?}");
    assert!(leases.loads.is_empty(), "{leases:?}");
}
