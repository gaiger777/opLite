//! 리더의 VIP 리스 표(설계 §4 내장 VIP): 갱신과 VIP 목록, 고립된 노드, 펜싱 중 거부. HA 리스 표와 따로다.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::{TestCluster, WAIT};
use opl_cluster::ClusterError;
use opl_cluster::types::{NodeId, Rejection};
use tokio::time::Instant;

/// 실패해야 하는 갱신에 주는 제한 시간. 선거 제한 시간(1초 이상)보다 짧아 그 사이 재선거가 없다.
const SHORT: Duration = Duration::from_millis(300);

fn follower_of(tc: &TestCluster, leader: NodeId) -> NodeId {
    *tc.nodes.keys().find(|id| **id != leader).unwrap()
}

fn vips(list: &[&str]) -> Vec<String> {
    list.iter().map(|v| v.to_string()).collect()
}

#[tokio::test]
async fn leader_records_vip_renewals_with_their_addresses() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    let before = Instant::now();
    tc.get(follower)
        .renew_vip(vips(&["10.0.0.2", "10.0.0.1"]), WAIT)
        .await
        .unwrap();
    let after = Instant::now();

    let leases = tc.get(leader).vip_leases();
    let r = &leases.renewed[&follower];
    assert!(before <= r.at && r.at <= after, "{r:?}");
    assert_eq!(
        r.vips,
        BTreeSet::from(["10.0.0.1".to_string(), "10.0.0.2".to_string()])
    );
    // HA 리스 표와 섞이지 않는다.
    assert!(tc.get(leader).leases().renewed.is_empty());
    assert!(tc.get(follower).vip_leases().renewed.is_empty());
}

#[tokio::test]
async fn isolated_follower_cannot_renew_a_vip() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    tc.router.isolate(follower);
    let err = tc
        .get(follower)
        .renew_vip(vips(&["10.0.0.1"]), SHORT)
        .await
        .unwrap_err();
    assert!(matches!(err, ClusterError::Timeout), "got {err:?}");
    assert!(!tc.get(leader).vip_leases().renewed.contains_key(&follower));
}

#[tokio::test]
async fn vip_fenced_node_is_rejected_until_the_mark_is_cleared() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);
    tc.get(follower)
        .renew_vip(vips(&["10.0.0.1"]), WAIT)
        .await
        .unwrap();
    let first = tc.get(leader).vip_leases().renewed[&follower].at;

    tc.get(leader).begin_vip_fence(follower);
    let err = tc
        .get(follower)
        .renew_vip(vips(&["10.0.0.1"]), WAIT)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClusterError::Rejected(Rejection::Fenced { node }) if node == follower),
        "got {err:?}"
    );
    assert_eq!(tc.get(leader).vip_leases().renewed[&follower].at, first);
    // HA 리스는 VIP 펜싱과 상관없다.
    tc.get(follower).renew_lease(WAIT).await.unwrap();

    tc.get(leader).end_vip_fence(follower);
    tc.get(follower)
        .renew_vip(vips(&["10.0.0.1"]), WAIT)
        .await
        .unwrap();
    assert!(tc.get(leader).vip_leases().renewed[&follower].at > first);
}
