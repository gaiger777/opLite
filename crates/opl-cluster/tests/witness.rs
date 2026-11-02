//! 선거에 나서지 않는 투표 멤버(위트니스, `enable_elect = false`).

mod common;

use std::time::{Duration, Instant};

use common::{TestCluster, node, volume};
use openraft::ServerState;
use opl_types::storage::VolumeSpec;

/// 위트니스만 남으면 과반수여도 리더가 생기지 않는다. 일반 노드가 돌아오면 다시 리더가 된다.
#[tokio::test]
async fn witnesses_never_become_leader() {
    let mut tc = TestCluster::start_with_witnesses(3, &[2, 3]).await;
    assert_eq!(tc.leader().await, node(1));

    tc.stop(node(1)).await;
    // 선거 제한 시간(최대 2초)의 두 배 넘게 지켜본다.
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        for id in [node(2), node(3)] {
            let state = tc.get(id).raft().metrics().borrow().state;
            assert_eq!(state, ServerState::Follower, "{id} stood for election");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    tc.open(node(1)).await;
    assert_eq!(tc.leader().await, node(1));
}

/// 일반 노드 둘과 위트니스에서 리더를 멈추면 남은 일반 노드가 리더가 되고 쓰기가 된다.
#[tokio::test]
async fn the_other_node_takes_over_when_the_leader_stops() {
    let mut tc = TestCluster::start_with_witnesses(3, &[3]).await;
    let old = tc.leader().await;
    assert_ne!(old, node(3));

    tc.stop(old).await;
    let rest = [node(1), node(2), node(3)]
        .into_iter()
        .filter(|id| *id != old);
    let new = tc.leader_among(&rest.collect::<Vec<_>>()).await;
    assert_ne!(new, node(3), "the witness became leader");

    let v = volume("after");
    let rev = tc.get(new).create(&v).await.unwrap();
    tc.wait_applied(node(3), rev).await;
    assert!(tc.get(node(3)).get::<VolumeSpec>(v.id).unwrap().is_some());
}
