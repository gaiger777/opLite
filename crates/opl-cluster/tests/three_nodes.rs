//! 한 프로세스 안의 3노드 클러스터: 리더 장애, 네트워크 분할, 스냅샷으로 따라잡기.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::{TestCluster, addr, node, raft_config, volume};
use openraft::SnapshotPolicy;
use opl_cluster::ClusterError;
use opl_cluster::watch::WatchEvent;
use opl_types::storage::VolumeSpec;

#[tokio::test]
async fn writes_continue_after_leader_failure() {
    let mut tc = TestCluster::start(3).await;
    let old = tc.leader().await;
    let a = volume("disk-a");
    tc.get(old).create(&a).await.unwrap();

    tc.stop(old).await;
    let new = tc.leader().await;

    let b = volume("disk-b");
    let rev_b = tc.get(new).create(&b).await.unwrap();
    for id in tc.nodes.keys().copied().collect::<Vec<_>>() {
        tc.wait_applied(id, rev_b).await;
        let c = tc.get(id);
        assert!(c.get::<VolumeSpec>(a.id).unwrap().is_some(), "{id} lost a");
        assert!(
            c.get::<VolumeSpec>(b.id).unwrap().is_some(),
            "{id} missing b"
        );
    }

    // 예전 리더가 돌아오면 놓친 쓰기를 따라잡는다.
    tc.open(old).await;
    tc.wait_applied(old, rev_b).await;
    assert!(tc.get(old).get::<VolumeSpec>(b.id).unwrap().is_some());
}

#[tokio::test]
async fn follower_rejects_writes_with_leader_hint() {
    let tc = TestCluster::start(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();

    let err = tc
        .get(follower)
        .create(&volume("disk-a"))
        .await
        .unwrap_err();
    match err {
        ClusterError::NotLeader {
            leader_id,
            leader_addr,
        } => {
            assert_eq!(leader_id, Some(leader));
            assert!(leader_addr.is_some());
        }
        other => panic!("expected NotLeader, got {other:?}"),
    }
    assert!(matches!(
        tc.get(follower).read_barrier().await,
        Err(ClusterError::NotLeader { .. })
    ));
}

#[tokio::test]
async fn isolated_leader_cannot_commit_and_its_write_is_discarded() {
    let tc = TestCluster::start_with(3, raft_config(), Duration::from_secs(1)).await;
    let old = tc.leader().await;

    let last_log_index = |id| tc.get(id).raft().metrics().borrow().last_log_index;
    let before = last_log_index(old);

    tc.router.isolate(old);
    let lost = volume("lost");
    let err = tc.get(old).create(&lost).await.unwrap_err();
    assert!(matches!(err, ClusterError::Timeout), "got {err:?}");
    // 예전 리더의 로그에는 기록되었다. 아래에서 이 엔트리가 버려지는지 본다.
    assert!(
        last_log_index(old) > before,
        "isolated leader did not append"
    );

    let majority: Vec<_> = tc.nodes.keys().copied().filter(|id| *id != old).collect();
    let new = tc.leader_among(&majority).await;
    let kept = volume("kept");
    let rev = tc.get(new).create(&kept).await.unwrap();

    tc.router.heal(old);
    tc.wait_applied(old, rev).await;
    for id in tc.nodes.keys().copied().collect::<Vec<_>>() {
        tc.wait_applied(id, rev).await;
        let c = tc.get(id);
        assert!(c.get::<VolumeSpec>(kept.id).unwrap().is_some());
        assert!(
            c.get::<VolumeSpec>(lost.id).unwrap().is_none(),
            "{id} applied an uncommitted write"
        );
    }
}

#[tokio::test]
async fn new_node_catches_up_from_snapshot() {
    let raft = openraft::Config {
        snapshot_policy: SnapshotPolicy::LogsSinceLast(5),
        max_in_snapshot_log_to_keep: 0,
        purge_batch_size: 1,
        ..raft_config()
    };
    let mut tc = TestCluster::start_with(3, raft, Duration::from_secs(5)).await;
    let leader = tc.leader().await;
    let mut ids = Vec::new();
    let mut last = 0;
    for i in 0..20 {
        let v = volume(&format!("disk-{i}"));
        last = tc.get(leader).create(&v).await.unwrap();
        ids.push(v.id);
    }
    // 리더가 스냅샷을 만들고 로그를 지울 때까지 기다린다.
    let purged = tc
        .get(leader)
        .raft()
        .wait(Some(common::WAIT))
        .metrics(
            |m| m.purged.is_some_and(|p| p.index > 1),
            "leader purged logs",
        )
        .await
        .unwrap()
        .purged
        .unwrap();

    let n4 = node(4);
    tc.open(n4).await;
    let mut w = tc.get(n4).watch(0).unwrap();
    tc.get(leader).add_learner(n4, addr(4)).await.unwrap();
    tc.wait_applied(n4, last).await;

    let first = tokio::time::timeout(common::WAIT, w.next()).await.unwrap();
    assert!(matches!(first.unwrap(), WatchEvent::Reset { .. }));
    let (_, items) = tc.get(n4).list::<VolumeSpec>().unwrap();
    assert_eq!(items.len(), 20);
    for id in &ids {
        assert!(tc.get(n4).get::<VolumeSpec>(*id).unwrap().is_some());
    }
    // 지워진 로그는 스냅샷으로만 받을 수 있다.
    let snapshot = tc.get(n4).raft().metrics().borrow().snapshot;
    assert!(
        snapshot.is_some_and(|s| s.index >= purged.index),
        "learner snapshot {snapshot:?} does not cover purged {purged:?}"
    );

    // learner를 투표 멤버로 올려도 쓰기가 이어진다.
    let voters = BTreeSet::from([node(1), node(2), node(3), n4]);
    tc.get(leader).change_membership(voters).await.unwrap();
    let rev = tc.get(leader).create(&volume("after")).await.unwrap();
    tc.wait_applied(n4, rev).await;
}
