//! 한 프로세스 안의 3노드 클러스터: 리더 장애, 네트워크 분할, 스냅샷으로 따라잡기, 리더로 전달.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::{TestCluster, addr, node, raft_config, volume};
use openraft::SnapshotPolicy;
use opl_cluster::memnet::MemForwarder;
use opl_cluster::types::{LeaderRequest, NodeId, Rejection};
use opl_cluster::watch::WatchEvent;
use opl_cluster::{ClusterError, Forwarder};
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

#[tokio::test]
async fn follower_forwards_writes_and_sees_them_at_once() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();

    let v = volume("disk-a");
    let rev = tc.get(follower).create(&v).await.unwrap();
    // 전달한 쓰기는 이 노드에도 적용된 뒤에 돌아온다.
    let got = tc.get(follower).get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev);
    // 리더의 거부도 그대로 돌아온다.
    let err = tc.get(follower).create(&v).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::AlreadyExists { .. })
    ));
}

#[tokio::test]
async fn forwarded_writes_wait_for_a_new_leader() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();
    tc.stop(leader).await;
    // 남은 두 노드가 새 리더를 뽑는 동안 쓰기는 기다렸다가 성공한다.
    let v = volume("disk-b");
    tc.get(follower).create(&v).await.unwrap();
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_some());
}

#[tokio::test]
async fn membership_changes_go_through_any_member() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();
    let rev = tc.get(leader).create(&volume("before")).await.unwrap();

    let n4 = node(4);
    tc.open(n4).await;
    tc.get(follower)
        .request(LeaderRequest::AddLearner {
            id: n4,
            addr: addr(4).addr,
        })
        .await
        .unwrap();
    assert!(tc.get(follower).members().learners.contains_key(&n4));
    tc.get(follower)
        .request(LeaderRequest::AddVoter { id: n4 })
        .await
        .unwrap();
    tc.wait_applied(n4, rev).await;
    let members = tc.get(follower).members();
    assert_eq!(members.voters.len(), 4);
    assert!(members.voters.contains_key(&n4));
    // 바쁘면 그 사이 리더가 바뀔 수 있다.
    assert!(members.leader.is_some());
    // 이미 투표 멤버면 그대로 성공한다.
    tc.get(n4)
        .request(LeaderRequest::AddVoter { id: n4 })
        .await
        .unwrap();

    // 자기 자신을 빼면 이 노드에 적용되기를 기다리지 않고 곧바로 돌아온다.
    let started = std::time::Instant::now();
    tc.get(n4)
        .request(LeaderRequest::Remove { id: n4 })
        .await
        .unwrap();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(3), "self-removal took {took:?}");
    let members = tc.get(leader).members();
    assert!(!members.contains(n4));
    assert_eq!(members.voters.len(), 3);
    // 멤버십에 없는 노드는 따라잡기를 기다리지 않고 바로 거부한다.
    let started = std::time::Instant::now();
    let err = tc
        .get(follower)
        .request(LeaderRequest::AddVoter { id: n4 })
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClusterError::Rejected(Rejection::NotFound { .. })),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// 첫 전달은 영영 돌아오지 않는다(연결을 끊지 않고 죽은 리더 호스트).
struct HangOnce {
    hung: AtomicBool,
    inner: Arc<MemForwarder>,
}

#[async_trait]
impl Forwarder for HangOnce {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        if !self.hung.swap(true, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.inner.forward(leader, addr, req).await
    }
}

#[tokio::test]
async fn hung_forward_is_retried_before_the_write_timeout() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();
    let cluster = tc.nodes.remove(&follower).unwrap();
    let cluster = cluster.with_forwarder(Arc::new(HangOnce {
        hung: AtomicBool::new(false),
        inner: tc.router.forwarder(follower),
    }));
    tc.nodes.insert(follower, cluster);

    let started = std::time::Instant::now();
    let v = volume("disk-a");
    tc.get(follower).create(&v).await.unwrap();
    let took = started.elapsed();
    assert!(took < tc.write_timeout, "forward took {took:?}");
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_some());
}

/// 첫 전달은 리더가 처리하지만 응답이 오지 않는다. `hang`이면 시도 제한 시간까지 붙잡고,
/// 아니면 보낸 뒤 연결이 끊긴 것처럼 `Unreachable`을 돌려준다. 리더가 처리하지 못한 전달
/// (`NotLeader` 등)은 그대로 돌려주고 다음 전달에서 다시 잃는다(`lost`는 실제로 잃었을 때만).
struct LoseFirstReply {
    lost: AtomicBool,
    hang: bool,
    inner: Arc<MemForwarder>,
}

#[async_trait]
impl Forwarder for LoseFirstReply {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let result = self.inner.forward(leader, addr, req).await;
        if result.is_err() || self.lost.swap(true, Ordering::SeqCst) {
            return result;
        }
        if self.hang {
            std::future::pending::<()>().await;
        }
        Err(ClusterError::Unreachable("connection reset".into()))
    }
}

/// 리더가 아닌 노드 하나의 전달자를 첫 응답을 잃는 것으로 바꾸고 그 노드와 전달자를 돌려준다.
async fn follower_losing_first_reply(
    tc: &mut TestCluster,
    hang: bool,
) -> (NodeId, Arc<LoseFirstReply>) {
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();
    let forwarder = Arc::new(LoseFirstReply {
        lost: AtomicBool::new(false),
        hang,
        inner: tc.router.forwarder(follower),
    });
    let cluster = tc.nodes.remove(&follower).unwrap();
    let cluster = cluster.with_forwarder(forwarder.clone());
    tc.nodes.insert(follower, cluster);
    (follower, forwarder)
}

#[tokio::test]
async fn retried_create_after_a_lost_reply_succeeds() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, false).await;

    let v = volume("disk-a");
    let rev = tc.get(follower).create(&v).await.unwrap();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    // 다시 보낸 요청의 `AlreadyExists`는 첫 요청이 적용된 결과이므로 그 revision으로 성공한다.
    let got = tc.get(follower).get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev);
}

#[tokio::test]
async fn retried_create_after_a_timed_out_attempt_succeeds() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, true).await;

    let v = volume("disk-a");
    let rev = tc.get(follower).create(&v).await.unwrap();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    let got = tc.get(follower).get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev);
}

#[tokio::test]
async fn retried_delete_after_a_lost_reply_succeeds() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let v = volume("disk-a");
    let leader = tc.leader().await;
    let rev = tc.get(leader).create(&v).await.unwrap();
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, false).await;
    tc.wait_applied(follower, rev).await;

    // 다시 보낸 요청의 `NotFound`는 첫 요청이 지운 결과다. 이 노드에서도 곧바로 없다.
    tc.get(follower)
        .delete::<VolumeSpec>(v.id, None)
        .await
        .unwrap();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_none());
}

#[tokio::test]
async fn retried_conditional_delete_after_a_lost_reply_is_still_rejected() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let v = volume("disk-a");
    let leader = tc.leader().await;
    let rev = tc.get(leader).create(&v).await.unwrap();
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, false).await;
    tc.wait_applied(follower, rev).await;

    // 조건부 삭제(합류 토큰처럼 지운 쪽만 성공해야 하는 것)는 다른 쪽이 지운 것과 구별할 수 없어
    // 그대로 돌려준다.
    let err = tc
        .get(follower)
        .delete::<VolumeSpec>(v.id, Some(rev))
        .await
        .unwrap_err();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    assert!(
        matches!(err, ClusterError::Rejected(Rejection::NotFound { .. })),
        "{err:?}"
    );
}

#[tokio::test]
async fn retried_conditional_update_after_a_lost_reply_is_still_rejected() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let v = volume("disk-a");
    let leader = tc.leader().await;
    let rev = tc.get(leader).create(&v).await.unwrap();
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, false).await;
    tc.wait_applied(follower, rev).await;

    // 조건부 수정은 거부가 자기 요청 때문인지 알 수 없어 그대로 돌려준다.
    let err = tc
        .get(follower)
        .update_spec(v.id, Some(rev), v.labels.clone(), &v.spec)
        .await
        .unwrap_err();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    assert!(
        matches!(
            err,
            ClusterError::Rejected(Rejection::RevisionMismatch { .. })
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn concurrent_membership_changes_wait_for_each_other() {
    let mut tc = TestCluster::start(3).await;
    let leader = tc.leader().await;
    let (n4, n5) = (node(4), node(5));
    for (n, i) in [(n4, 4), (n5, 5)] {
        tc.open(n).await;
        tc.get(leader).add_learner(n, addr(i)).await.unwrap();
    }
    // 한쪽 승격의 두 단계가 커밋되는 동안 다른 쪽은 `InProgress`를 받고 다시 시도한다.
    let c = tc.get(leader);
    let (a, b) = tokio::join!(
        c.request(LeaderRequest::AddVoter { id: n4 }),
        c.request(LeaderRequest::AddVoter { id: n5 }),
    );
    a.unwrap();
    b.unwrap();
    let members = c.members();
    assert!(members.voters.contains_key(&n4) && members.voters.contains_key(&n5));
}
