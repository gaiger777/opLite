//! 헬스 체크가 보는 Raft 상태: 리더, 정족수, 적용 지연.

mod common;

use std::time::{Duration, Instant};

use common::{TestCluster, WAIT, volume};
use openraft::ServerState;
use opl_cluster::QUORUM_ACK_MAX;

#[tokio::test]
async fn every_member_knows_the_leader_and_has_quorum() {
    let tc = TestCluster::start(3).await;
    let leader = tc.leader().await;
    tc.get(leader).create(&volume("v")).await.unwrap();
    for id in tc.nodes.keys() {
        let h = tc.get(*id).health();
        assert_eq!(h.leader, Some(leader), "{id}");
        assert!(h.quorum, "{id}");
        assert_eq!(h.apply_lag, Duration::ZERO, "{id}");
    }
}

#[tokio::test]
async fn a_single_voter_has_quorum() {
    let tc = TestCluster::start(1).await;
    let leader = tc.leader().await;
    let h = tc.get(leader).health();
    assert_eq!(h.leader, Some(leader));
    assert!(h.quorum);
}

/// 과반수와 끊긴 리더는 자기를 리더로 알지만, 과반수의 답이 `QUORUM_ACK_MAX`보다 오래 없으면 정족수가 없다.
#[tokio::test]
async fn an_isolated_leader_loses_quorum() {
    let tc = TestCluster::start(3).await;
    let leader = tc.leader().await;
    assert!(tc.get(leader).health().quorum);

    tc.router.isolate(leader);
    let isolated = Instant::now();
    let deadline = isolated + QUORUM_ACK_MAX + WAIT;
    loop {
        let h = tc.get(leader).health();
        if !h.quorum {
            assert_eq!(h.leader, Some(leader), "{h:?}");
            break;
        }
        assert!(Instant::now() < deadline, "the isolated leader kept quorum");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // 과반수의 답은 끊기기 전에 마지막으로 왔으므로 끊긴 뒤 `QUORUM_ACK_MAX`보다 일찍 잃지 않는다.
    assert!(isolated.elapsed() >= QUORUM_ACK_MAX - Duration::from_millis(500));
    assert_eq!(
        tc.get(leader).raft().unwrap().metrics().borrow().state,
        ServerState::Leader
    );
}
