//! 관찰 노드의 복제본(설계 §6). Raft 구성원이 아니고, 투표 멤버 하나에 Follow로 붙어 받은 스냅샷과 적용 묶음을
//! 자기 저장소에 쓴다. 쓰기와 리스 갱신은 `Cluster`가 리더로 전달한다.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::StoredMembership;
use redb::{Database, ReadableDatabase};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::cluster::ClusterError;
use crate::follow::{FollowClient, FollowFrame};
use crate::health::ApplyLag;
use crate::state_machine::{Objects, apply_feed, install_objects};
use crate::store::{KEY_LAST_MEMBERSHIP, META, Store, StoreError, get_json};
use crate::types::{NodeId, OrbNode};
use crate::watch::{FeedBatch, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, OrbNode>;

/// 붙은 노드에서 이만큼 아무 프레임도 오지 않으면 끊고 다시 붙는다(서버는 1초마다 `Progress`를 보낸다).
pub(crate) const FOLLOW_STALL: Duration = Duration::from_secs(3);

/// 다시 붙기 전에 쉬는 시간
const RECONNECT_DELAY: Duration = Duration::from_millis(100);

/// 리더가 돌려준 인덱스(전달한 쓰기·리스 갱신)를 붙은 노드가 이보다 오래 알리지 않으면(`Progress`·묶음의 적용 위치)
/// 뒤처진 것으로 보고 다른 투표 멤버로 옮긴다(살아 있어 `Progress`는 오지만 로그를 받지 못하는 노드).
pub(crate) const UPSTREAM_LAG_MAX: Duration = Duration::from_secs(3);

/// 연결 하나가 끝난 까닭
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// 붙지 못했거나 진행을 하나도 받지 못하고 끊겼다. 다음에는 다른 노드를 고른다.
    Failed,
    /// 진행을 받다가 끊겼다.
    Progressed,
    /// 붙은 노드가 리더다(리더를 모르고 골랐거나 그 노드가 리더가 됐다). 다른 투표 멤버로 옮긴다.
    Move,
    /// 붙은 노드가 리더보다 뒤처졌다. 다음에는 다른 노드를 고른다.
    Lagging,
}

/// 관찰 노드의 복제본. 복제 작업은 `start`에서 띄우고 `shutdown`으로 멈춘다.
#[derive(Clone)]
pub(crate) struct Replica {
    inner: Arc<Inner>,
}

struct Inner {
    id: NodeId,
    db: Arc<Database>,
    hub: Arc<WatchHub>,
    writer: StoreWriter,
    follow: Arc<dyn FollowClient>,
    status: Mutex<Status>,
    /// `shutdown`이 `true`로 바꾼다.
    stopped: watch::Sender<bool>,
    /// `follow_another`가 올린다. 붙은 노드를 버리고 다른 노드로 옮긴다.
    leave_upstream: watch::Sender<u64>,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// 붙은 노드에게서 들은 것
#[derive(Debug, Clone, Default)]
pub(crate) struct Status {
    pub(crate) leader: Option<(NodeId, String)>,
    /// 붙을 후보(투표 멤버와 클러스터 주소)
    pub(crate) voters: BTreeMap<NodeId, String>,
    /// 지금 붙어 있는 노드
    pub(crate) upstream: Option<NodeId>,
    /// 붙은 노드가 마지막으로 알린 적용 인덱스
    pub(crate) upstream_applied: u64,
    /// 리더가 돌려준 가장 큰 인덱스(전달한 쓰기·리스 갱신). 이만큼 적용하지 못한 시간이 적용 지연이다.
    pub(crate) leader_applied: u64,
    /// 붙은 노드에서 마지막으로 진행(스냅샷·묶음·`Progress`)을 받은 때
    pub(crate) last_progress: Option<Instant>,
    /// 연결이 끝나 다시 붙은 횟수
    pub(crate) reconnects: u64,
}

impl Replica {
    /// 저장소의 멤버십에서 후보를 읽고 복제 작업을 띄운다(멤버십에 투표 멤버가 없으면 `seed`). 저장소의 적용
    /// 위치(`revision`)부터 이어 받는다.
    pub(crate) fn start(
        id: NodeId,
        store: &Store,
        follow: Arc<dyn FollowClient>,
        seed: &BTreeMap<NodeId, String>,
    ) -> Result<Self, StoreError> {
        let db = store.db().clone();
        let mut voters = voter_addrs(&read_membership(&db)?);
        if voters.is_empty() {
            voters.clone_from(seed);
        }
        let inner = Arc::new(Inner {
            id,
            db,
            hub: store.watch_hub().clone(),
            writer: store.writer().clone(),
            follow,
            status: Mutex::new(Status {
                voters,
                ..Default::default()
            }),
            stopped: watch::channel(false).0,
            leave_upstream: watch::channel(0).0,
            task: Mutex::new(None),
        });
        let task = tokio::spawn(inner.clone().run());
        *inner.task.lock().unwrap() = Some(task);
        Ok(Self { inner })
    }

    pub(crate) fn status(&self) -> Status {
        self.inner.status.lock().unwrap().clone()
    }

    pub(crate) fn leader(&self) -> Option<(NodeId, String)> {
        self.inner.status.lock().unwrap().leader.clone()
    }

    /// 저장소에 적용된 마지막 멤버십
    pub(crate) fn membership(&self) -> Result<Membership, StoreError> {
        read_membership(&self.inner.db)
    }

    /// 리더를 묻는 쪽에 돌려줄 `NotLeader`
    pub(crate) fn not_leader(&self) -> ClusterError {
        let leader = self.leader();
        ClusterError::NotLeader {
            leader_id: leader.as_ref().map(|(id, _)| *id),
            leader_addr: leader.map(|(_, addr)| addr),
        }
    }

    /// 리더가 돌려준 인덱스(그때 리더가 적용했거나 커밋한 것)를 적는다. 붙은 노드가 이것을 오래 보내지 않으면
    /// 옮긴다.
    pub(crate) fn heard_from_leader(&self, index: u64) {
        let mut status = self.inner.status.lock().unwrap();
        status.leader_applied = status.leader_applied.max(index);
    }

    /// 붙은 노드를 버리고 다른 투표 멤버로 옮긴다(리더가 돌려준 인덱스를 기다리다 시간이 지났을 때).
    pub(crate) fn follow_another(&self) {
        self.inner.leave_upstream.send_modify(|n| *n += 1);
    }

    /// 복제 작업을 멈춘다. 작업이 저장소 스레드에 넣어 둔 적용(작업을 멈춰도 저장소 스레드는 한다)까지 끝난 뒤
    /// 돌아오므로, 그 뒤에 읽은 리비전은 저장소와 같고 더 움직이지 않는다.
    pub(crate) async fn shutdown(&self) {
        let task = self.inner.task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        // 저장소 스레드는 넣은 차례대로 하므로 빈 쓰기가 끝나면 앞서 넣은 적용도 끝났다(스레드가 멈췄으면 남은 것이 없다).
        let _ = self.inner.writer.run(|_| Ok(())).await;
        self.inner.stopped.send_replace(true);
    }

    /// `shutdown`하면 `true`가 되는 수신자
    pub(crate) fn stopped(&self) -> watch::Receiver<bool> {
        self.inner.stopped.subscribe()
    }
}

impl Inner {
    async fn run(self: Arc<Self>) {
        // 이번 차례에 붙지 못했거나 아무것도 받지 못한 노드. 진행을 받으면 비운다.
        let mut skip = BTreeSet::new();
        loop {
            let Some((to, addr)) = self.pick(&skip) else {
                skip.clear();
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            };
            let since = self.hub.revision();
            // 다른 노드에 붙지 못해 리더까지 왔으면 리더에 머문다.
            let may_move = skip.is_empty();
            let outcome = match self.follow.follow(to, &addr, since).await {
                Ok(frames) => self.consume(to, frames, may_move).await,
                Err(e) => {
                    tracing::debug!(upstream = %to, error = %e, "cannot follow");
                    Outcome::Failed
                }
            };
            {
                let mut status = self.status.lock().unwrap();
                status.upstream = None;
                status.reconnects += 1;
            }
            match outcome {
                Outcome::Failed | Outcome::Lagging => {
                    skip.insert(to);
                }
                Outcome::Progressed => skip.clear(),
                Outcome::Move => {}
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    }

    /// 붙을 노드: 리더가 아닌 투표 멤버 가운데 이 노드와의 rendezvous 해시가 가장 큰 것(관찰 노드가 고르게
    /// 퍼진다), 없으면 리더. `skip`에 든 노드는 건너뛴다.
    fn pick(&self, skip: &BTreeSet<NodeId>) -> Option<(NodeId, String)> {
        let status = self.status.lock().unwrap();
        let leader = status.leader.as_ref().map(|(id, _)| *id);
        let mut ranked: Vec<(&NodeId, &String)> = status
            .voters
            .iter()
            .filter(|(id, _)| Some(**id) != leader && **id != self.id)
            .collect();
        ranked.sort_by_key(|(id, _)| std::cmp::Reverse(rendezvous(self.id, **id)));
        ranked
            .into_iter()
            .map(|(id, addr)| (*id, addr.clone()))
            .chain(status.leader.clone())
            .find(|(id, _)| !skip.contains(id))
    }

    /// 스트림이 끝나거나 `FOLLOW_STALL` 동안 멈출 때까지 프레임을 적용한다. `may_move`면 붙은 노드가 리더일 때
    /// 다른 투표 멤버가 있으면 끊는다.
    async fn consume(
        &self,
        to: NodeId,
        mut frames: BoxStream<'static, Result<FollowFrame, ClusterError>>,
        may_move: bool,
    ) -> Outcome {
        let mut progressed = false;
        let ended = |progressed| {
            if progressed {
                Outcome::Progressed
            } else {
                Outcome::Failed
            }
        };
        let mut parts = Vec::new();
        // 스냅샷 조각을 받는 중이다(첫 `SnapshotPart`부터 `SnapshotEnd`까지).
        let mut in_snapshot = false;
        // 다음 `Batch`와 한 번에 적용할 나눠 온 묶음의 앞부분
        let mut batch_parts = Vec::new();
        // 리더가 돌려준 인덱스를 붙은 노드가 알리지 않은 시간(붙을 때마다, 스냅샷을 설치할 때마다 새로 잰다)
        let mut lag = ApplyLag::default();
        let mut leave = self.leave_upstream.subscribe();
        leave.borrow_and_update();
        loop {
            if may_move && self.upstream_is_leader(to) {
                return Outcome::Move;
            }
            // 스냅샷이나 나눈 묶음을 받는 동안은 붙은 노드가 알린 적용이 움직이지 않는다(스냅샷이 오래 걸려도 끊지 않게).
            let receiving = in_snapshot || !batch_parts.is_empty();
            if !receiving && self.upstream_lag(&mut lag) > UPSTREAM_LAG_MAX {
                tracing::warn!(upstream = %to, "the followed node lags behind the leader; following another");
                return Outcome::Lagging;
            }
            let next = tokio::select! {
                next = tokio::time::timeout(FOLLOW_STALL, frames.next()) => next,
                _ = leave.changed(), if !receiving => {
                    // 붙은 노드는 리더의 인덱스를 알렸는데 이 노드가 늦게 적용하는 것이면 머문다.
                    if self.upstream_behind() {
                        tracing::warn!(upstream = %to, "a forwarded request was not applied in time; following another");
                        return Outcome::Lagging;
                    }
                    continue;
                }
            };
            let frame = match next {
                Ok(Some(Ok(frame))) => frame,
                Ok(Some(Err(e))) => {
                    tracing::debug!(upstream = %to, error = %e, "follow stream failed");
                    return ended(progressed);
                }
                Ok(None) => return ended(progressed),
                Err(_) => {
                    tracing::warn!(upstream = %to, "no progress from the followed node; following another");
                    return ended(progressed);
                }
            };
            // 스냅샷 조각은 `SnapshotEnd`까지, 나눈 묶음은 `Batch`까지 이어 와야 한다(서버는 그 사이에 다른 프레임을
            // 넣지 않는다). 아니면 모은 것을 버리고 다시 붙는다.
            let in_order = match frame {
                FollowFrame::SnapshotPart { .. } | FollowFrame::SnapshotEnd { .. } => {
                    batch_parts.is_empty()
                }
                FollowFrame::BatchPart { .. } | FollowFrame::Batch { .. } => !in_snapshot,
                FollowFrame::Hello { .. } | FollowFrame::Progress { .. } => !receiving,
            };
            if !in_order {
                tracing::warn!(upstream = %to, "the followed node sent frames out of order; following again");
                return ended(progressed);
            }
            let result = match frame {
                FollowFrame::Hello { leader, voters, .. } => {
                    let mut status = self.status.lock().unwrap();
                    status.upstream = Some(to);
                    status.leader = leader;
                    if !voters.is_empty() {
                        status.voters = voters;
                    }
                    continue;
                }
                FollowFrame::SnapshotPart { objects } => {
                    in_snapshot = true;
                    parts.extend(objects);
                    Ok(())
                }
                FollowFrame::BatchPart { events } => {
                    batch_parts.extend(events);
                    Ok(())
                }
                FollowFrame::SnapshotEnd {
                    applied,
                    membership,
                } => {
                    in_snapshot = false;
                    let result = self
                        .install(applied, membership, std::mem::take(&mut parts))
                        .await;
                    // 설치하는 동안 늦은 전달 기다림이나 지연은 붙은 노드 탓이 아니다. 새로 잰다.
                    lag = ApplyLag::default();
                    leave.borrow_and_update();
                    result
                }
                FollowFrame::Batch {
                    events,
                    applied,
                    membership,
                } => {
                    batch_parts.extend(events);
                    self.apply(FeedBatch {
                        events: std::mem::take(&mut batch_parts),
                        applied,
                        membership,
                    })
                    .await
                }
                FollowFrame::Progress {
                    applied,
                    leader,
                    leader_addr,
                } => {
                    let mut status = self.status.lock().unwrap();
                    status.upstream_applied = applied.map_or(0, |l| l.index);
                    status.leader = leader.zip(leader_addr);
                    Ok(())
                }
            };
            if let Err(e) = result {
                tracing::error!(upstream = %to, error = %e, "cannot write the followed state");
                return ended(progressed);
            }
            progressed = true;
            self.status.lock().unwrap().last_progress = Some(Instant::now());
        }
    }

    /// 리더가 돌려준 인덱스를 붙은 노드가 알리지 않은 시간(이 노드의 적용이 아니라 붙은 노드가 알린 적용으로 잰다:
    /// 이 노드의 디스크가 느린 것을 붙은 노드 탓으로 돌리지 않게)
    fn upstream_lag(&self, lag: &mut ApplyLag) -> Duration {
        let status = self.status.lock().unwrap();
        lag.observe(
            Some(status.leader_applied),
            Some(status.upstream_applied),
            Instant::now(),
        )
    }

    /// 붙은 노드가 알린 적용이 리더가 돌려준 인덱스보다 뒤다.
    fn upstream_behind(&self) -> bool {
        let status = self.status.lock().unwrap();
        status.upstream_applied < status.leader_applied
    }

    /// 붙은 노드가 리더이고 붙을 다른 투표 멤버가 있다.
    fn upstream_is_leader(&self, to: NodeId) -> bool {
        let status = self.status.lock().unwrap();
        status.leader.as_ref().is_some_and(|(l, _)| *l == to)
            && status.voters.keys().any(|v| *v != to && *v != self.id)
    }

    async fn install(
        &self,
        applied: Option<openraft::LogId<NodeId>>,
        membership: Membership,
        objects: Objects,
    ) -> Result<(), StoreError> {
        let voters = voter_addrs(&membership);
        let revision = applied.map_or(0, |l| l.index);
        // 붙은 노드가 이 노드보다 뒤다(뒤처진 노드에 붙은 뒤 그 노드가 스냅샷을 설치했다). 설치하면 이미 돌려준 리비전
        // 읽기·리스 갱신이 거짓이 되므로 두고, 이어 오는 묶음에서 이미 적용한 것은 건너뛴다. 같은 리비전이면 상태가
        // 같으므로 설치하지 않는다(구독자에게 헛된 `Reset`을 내지 않게).
        if revision <= self.hub.revision() {
            tracing::warn!(
                revision,
                applied = self.hub.revision(),
                "ignoring a snapshot not newer than this node's state"
            );
            return Ok(());
        }
        // 구독자에게 알리는 것도 저장소 스레드의 쓰기 안에서 한다(전환의 쓰기와 차례가 어긋나지 않게).
        let hub = self.hub.clone();
        self.writer
            .run(move |db| {
                install_objects(db, applied, membership, objects)?;
                hub.reset(revision);
                Ok(())
            })
            .await?;
        let mut status = self.status.lock().unwrap();
        status.upstream_applied = status.upstream_applied.max(revision);
        if !voters.is_empty() {
            status.voters = voters;
        }
        Ok(())
    }

    async fn apply(&self, mut batch: FeedBatch) -> Result<(), StoreError> {
        // 다시 붙은 뒤 이미 적용한 묶음이 올 수 있다.
        if batch.applied.index <= self.hub.revision() {
            return Ok(());
        }
        // 투표 멤버마다 묶음 경계가 달라, 옮긴 뒤 첫 묶음이 이미 적용한 리비전의 변경을 품을 수 있다. 그것은 쓰지도
        // 내지도 않는다(옛 값으로 되돌리거나 감시에 두 번 보이지 않게). 적용 위치는 묶음의 것이다.
        let applied_here = self.hub.revision();
        batch.events.retain(|e| e.revision() > applied_here);
        let (applied, voters) = (
            batch.applied.index,
            batch.membership.as_ref().map(voter_addrs),
        );
        let hub = self.hub.clone();
        self.writer
            .run(move |db| {
                apply_feed(db, &batch)?;
                hub.publish(batch);
                Ok(())
            })
            .await?;
        let mut status = self.status.lock().unwrap();
        status.upstream_applied = status.upstream_applied.max(applied);
        if let Some(voters) = voters.filter(|v| !v.is_empty()) {
            status.voters = voters;
        }
        Ok(())
    }
}

/// 같은 관찰 노드는 늘 같은 순서로, 관찰 노드마다는 다른 순서로 투표 멤버를 고르게 하는 점수
fn rendezvous(me: NodeId, voter: NodeId) -> u64 {
    let mut h = DefaultHasher::new();
    (me, voter).hash(&mut h);
    h.finish()
}

pub(crate) fn read_membership(db: &Database) -> Result<Membership, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    Ok(get_json(&meta, KEY_LAST_MEMBERSHIP)?.unwrap_or_default())
}

fn voter_addrs(m: &Membership) -> BTreeMap<NodeId, String> {
    let m = m.membership();
    m.voter_ids()
        .filter_map(|id| m.get_node(&id).map(|n| (id, n.addr().to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::{Volume, VolumeSpec};
    use tempfile::TempDir;

    use super::*;
    use crate::cluster::{Cluster, ClusterConfig};
    use crate::memnet::MemRouter;
    use crate::types::LeaderRequest;
    use crate::watch::{HISTORY, WatchError, WatchEvent};

    const WAIT: Duration = Duration::from_secs(10);

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

    fn config(root: &TempDir, i: u64, in_memory: bool) -> ClusterConfig {
        let mut config = ClusterConfig::new(NodeId::from(i), root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = WAIT;
        config.in_memory = in_memory;
        config
    }

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !cond() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 투표 멤버 1..=3(메모리 저장소)과 관찰 노드 4..(파일 저장소). 관찰 노드는 실제 길처럼 learner로 가입해
    /// 따라잡은 뒤 멤버십에서 빠지고, 같은 저장소로 관찰 노드로 다시 열린다.
    struct Net {
        router: MemRouter,
        root: TempDir,
        voters: BTreeMap<NodeId, Cluster>,
        observers: BTreeMap<NodeId, Cluster>,
    }

    impl Net {
        async fn start(observers: u64) -> Self {
            Self::start_with(observers, |_| {}).await
        }

        /// 투표 멤버의 Raft 설정을 `raft`로 고쳐 띄운다.
        async fn start_with(observers: u64, raft: impl Fn(&mut openraft::Config)) -> Self {
            let mut net = Net {
                router: MemRouter::default(),
                root: TempDir::new().unwrap(),
                voters: BTreeMap::new(),
                observers: BTreeMap::new(),
            };
            for i in 1..=3 {
                let id = NodeId::from(i);
                let mut config = config(&net.root, i, true);
                raft(&mut config.raft);
                let c = Cluster::open(config, net.router.network(id))
                    .await
                    .unwrap()
                    .with_forwarder(net.router.forwarder(id));
                net.router.register(id, &c);
                net.voters.insert(id, c);
            }
            let members = (1..=3u64)
                .map(|i| (NodeId::from(i), OrbNode::single(format!("node-{i}"))))
                .collect();
            net.voters[&NodeId::from(1)]
                .initialize(members)
                .await
                .unwrap();
            for i in 4..4 + observers {
                net.add_observer(i).await;
            }
            net
        }

        fn leader(&self) -> &Cluster {
            self.voters
                .values()
                .find(|c| c.members().leader == Some(c.id()))
                .expect("a leader")
        }

        async fn settled_leader(&self) -> NodeId {
            wait_until("a leader", || {
                self.voters
                    .values()
                    .any(|c| c.members().leader == Some(c.id()))
            })
            .await;
            self.leader().id()
        }

        async fn add_observer(&mut self, i: u64) {
            let id = NodeId::from(i);
            self.settled_leader().await;
            let learner = Cluster::open(config(&self.root, i, false), self.router.network(id))
                .await
                .unwrap();
            self.router.register(id, &learner);
            self.leader()
                .add_learner(id, OrbNode::single(format!("node-{i}")))
                .await
                .unwrap();
            self.leader()
                .request(LeaderRequest::Remove { id })
                .await
                .unwrap();
            self.router.unregister(id);
            learner.shutdown().await.unwrap();
            drop(learner);
            let observer = loop {
                match Cluster::open_observer(
                    config(&self.root, i, false),
                    self.router.network(id),
                    self.router.follower(id),
                )
                .await
                {
                    Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen))) => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    other => break other.unwrap(),
                }
            }
            .with_forwarder(self.router.forwarder(id));
            self.router.register(id, &observer);
            self.observers.insert(id, observer);
        }

        fn observer(&self, n: usize) -> &Cluster {
            self.observers.values().nth(n).unwrap()
        }

        /// 모든 관찰 노드가 리더의 지금 리비전까지 적용하기를 기다린다.
        async fn sync(&self) -> u64 {
            let revision = self.leader().revision();
            for o in self.observers.values() {
                o.wait_revision(revision, WAIT).await.unwrap();
            }
            revision
        }

        async fn stop_voter(&mut self, id: NodeId) {
            self.router.unregister(id);
            self.voters.remove(&id).unwrap().shutdown().await.unwrap();
        }

        /// 모든 투표 멤버의 기록이 `revision`을 지나칠 때까지 리더에 쓴다.
        async fn write_past_history(&self, revision: u64) {
            let mut n = 0;
            while self.voters.values().any(|v| {
                !matches!(
                    v.node_handle().hub().feed(revision),
                    Err(WatchError::Expired { .. })
                )
            }) {
                for _ in 0..64 {
                    self.leader()
                        .create(&volume(&format!("bulk-{n}")))
                        .await
                        .unwrap();
                    n += 1;
                }
                assert!(n < 4 * HISTORY, "history never expired");
            }
        }

        async fn shutdown(self) {
            for c in self.observers.values().chain(self.voters.values()) {
                c.shutdown().await.unwrap();
            }
        }
    }

    fn replica(c: &Cluster) -> Replica {
        match c.node_handle().backend() {
            crate::cluster::Backend::Observer(r) => r,
            crate::cluster::Backend::Member { .. } => panic!("not an observer"),
        }
    }

    async fn next_events(w: &mut crate::watch::Watch, n: usize) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(tokio::time::timeout(WAIT, w.next()).await.unwrap().unwrap());
        }
        out
    }

    /// 관찰 노드는 Raft 밖에서 투표 멤버와 같은 객체를 같은 `list` 리비전으로 보인다.
    #[tokio::test]
    async fn observers_serve_the_same_objects_at_the_same_list_revision() {
        let net = Net::start(2).await;
        let vols: Vec<Volume> = (0..3).map(|i| volume(&format!("v{i}"))).collect();
        for v in &vols {
            net.leader().create(v).await.unwrap();
        }
        let gone = vols[0].id;
        net.leader().delete::<VolumeSpec>(gone, None).await.unwrap();
        net.sync().await;
        let expected = net.leader().list::<VolumeSpec>().unwrap();
        for o in net.observers.values() {
            assert!(o.raft().is_none());
            assert_eq!(o.list::<VolumeSpec>().unwrap(), expected);
            assert!(o.get::<VolumeSpec>(gone).unwrap().is_none());
            assert!(o.get_by_name::<VolumeSpec>("v0").unwrap().is_none());
            assert_eq!(
                o.get_by_name::<VolumeSpec>("v1").unwrap().map(|v| v.id),
                Some(vols[1].id)
            );
            // 멤버십은 저장소에 적용된 것, 리더는 붙은 노드가 알린 것이다.
            let members = o.members();
            assert_eq!(members.leader, Some(net.leader().id()));
            assert_eq!(members.voters.len(), 3);
            assert!(!members.contains(o.id()));
        }
        net.shutdown().await;
    }

    /// 관찰 노드에서 쓴 것은 돌아오자마자 그 노드에서 읽히고, 리스 갱신은 리더로 전달되어 성공한다.
    /// 리더로서 할 일은 아는 리더를 담은 `NotLeader`다.
    #[tokio::test]
    async fn an_observer_reads_its_own_writes_and_renews_its_lease() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let v = volume("mine");
        let revision = o.create(&v).await.unwrap();
        assert!(o.revision() >= revision);
        assert_eq!(
            o.get::<VolumeSpec>(v.id).unwrap().unwrap().revision,
            revision
        );

        o.renew_lease(Duration::from_secs(5)).await.unwrap();
        assert!(net.leader().leases().renewed.contains_key(&o.id()));
        assert!(o.leases().renewed.is_empty());
        let leader = net.leader().id();
        assert!(matches!(
            o.read_barrier().await,
            Err(ClusterError::NotLeader { leader_id: Some(l), leader_addr: Some(_) }) if l == leader
        ));
        assert!(matches!(
            o.execute_as_leader(LeaderRequest::RenewLease {
                node: o.id(),
                checks: vec![],
                load: None,
            }).await,
            Err(ClusterError::NotLeader { leader_id: Some(l), .. }) if l == leader
        ));

        // 헬스: 붙은 노드의 진행이 오고 리더를 안다.
        let health = o.health();
        assert_eq!(health.leader, Some(leader));
        assert!(health.quorum);

        // Raft 구성원이 아닌 노드에 건 Follow는 `Hello`(부른 쪽의 역할)만 오고 닫힌다.
        let frames: Vec<FollowFrame> = o.follow(NodeId::from(99), 0).unwrap().collect().await;
        assert!(
            matches!(
                frames.as_slice(),
                [FollowFrame::Hello { role: crate::follow::NodeRole::Observer, voters, .. }] if voters.len() == 3
            ),
            "{frames:?}"
        );
        net.shutdown().await;
    }

    /// `shutdown`은 복제 작업이 저장소 스레드에 넣어 둔 적용까지 끝난 뒤 돌아온다: 멈춘 뒤 읽은 리비전이 저장소와 같고
    /// 그 뒤로 움직이지 않는다.
    #[tokio::test]
    async fn shutdown_waits_for_a_queued_apply() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let r = replica(o);
        net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        // 저장소 스레드를 붙잡아 다음 적용이 줄을 서게 한다.
        let (release, held) = std::sync::mpsc::channel::<()>();
        o.store()
            .writer()
            .submit(move |_| {
                let _ = held.recv();
            })
            .unwrap();
        let written = net.leader().create(&volume("queued")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            release.send(()).unwrap();
        });
        r.shutdown().await;
        let stopped_at = o.revision();
        releaser.await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(o.revision(), stopped_at);
        assert!(stopped_at >= written);
        let stored: Option<openraft::LogId<NodeId>> = {
            let txn = o.store().db().begin_read().unwrap();
            get_json(
                &txn.open_table(META).unwrap(),
                crate::store::KEY_LAST_APPLIED,
            )
            .unwrap()
        };
        assert_eq!(stored.map(|l| l.index), Some(stopped_at));
        net.shutdown().await;
    }

    /// 관찰 노드는 리더가 아닌 투표 멤버에 붙는다. 시작할 때 리더를 몰라 리더에 붙었으면 `Hello`를 보고 옮긴다.
    #[tokio::test]
    async fn observers_follow_voters_other_than_the_leader() {
        let net = Net::start(4).await;
        let leader = net.settled_leader().await;
        for o in net.observers.values() {
            let r = replica(o);
            wait_until("following a non-leader voter", || {
                r.status().upstream.is_some_and(|u| u != leader)
            })
            .await;
        }
        net.shutdown().await;
    }

    /// 붙은 투표 멤버가 죽으면 다른 투표 멤버로 자기 리비전부터 다시 붙는다. 기록 안이라 `Reset`이 없다.
    #[tokio::test]
    async fn an_observer_moves_to_another_voter_without_reset() {
        let mut net = Net::start(1).await;
        let leader = net.settled_leader().await;
        let r = replica(net.observer(0));
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        // 리더가 아닌 투표 멤버에 붙는다.
        assert_ne!(upstream, leader);
        let before = r.status().reconnects;
        let mut w = net.observer(0).watch(net.sync().await).unwrap();

        net.stop_voter(upstream).await;
        let mut written = Vec::new();
        for i in 0..3 {
            let v = volume(&format!("after-{i}"));
            net.leader().create(&v).await.unwrap();
            written.push(v.id);
        }
        let events = next_events(&mut w, 3).await;
        let ids: Vec<_> = events
            .iter()
            .map(|e| match e {
                WatchEvent::Put { resource, .. } => resource.id,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(ids, written);
        let status = r.status();
        assert!(status.reconnects > before);
        assert!(status.upstream.is_some_and(|u| u != upstream));
        net.shutdown().await;
    }

    /// 끊긴 동안 투표 멤버의 기록이 지나가면 다시 붙을 때 스냅샷을 받고 구독자는 `Reset`을 본다.
    #[tokio::test]
    async fn an_observer_past_the_history_gets_a_snapshot_and_reset() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let revision = net.sync().await;
        let mut w = o.watch(revision).unwrap();
        net.router.isolate(o.id());
        net.write_past_history(revision).await;
        assert_eq!(o.revision(), revision);
        net.router.heal(o.id());

        let reset = tokio::time::timeout(WAIT, w.next()).await.unwrap().unwrap();
        assert!(matches!(reset, WatchEvent::Reset { .. }), "{reset:?}");
        net.sync().await;
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 적용이 막혀 뒤처진 관찰 노드는 서버 쪽 피드가 `Lagged`로 끝나 다시 붙고 따라잡는다.
    #[tokio::test]
    async fn a_lagging_observer_reconnects_and_catches_up() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let r = replica(o);
        let revision = net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        let before = r.status().reconnects;
        // 관찰 노드의 저장소 스레드를 붙잡는다.
        let (release, held) = std::sync::mpsc::channel::<()>();
        o.store()
            .writer()
            .submit(move |_| {
                let _ = held.recv();
            })
            .unwrap();
        net.write_past_history(revision + 64).await;
        release.send(()).unwrap();

        net.sync().await;
        assert!(r.status().reconnects > before);
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 빈 저장소로 바로 연 관찰 노드는 설정의 `follow_seed`(투표 멤버)에 붙어 스냅샷부터 받는다(규모 시험).
    #[tokio::test]
    async fn a_fresh_observer_follows_the_seed_voters() {
        let mut net = Net::start(0).await;
        net.settled_leader().await;
        let v = volume("before");
        net.leader().create(&v).await.unwrap();
        let id = NodeId::from(9);
        let mut config = config(&net.root, 9, true);
        config.follow_seed = (1..=3u64)
            .map(|i| (NodeId::from(i), format!("node-{i}")))
            .collect();
        let o = Cluster::open_observer(config, net.router.network(id), net.router.follower(id))
            .await
            .unwrap()
            .with_forwarder(net.router.forwarder(id));
        net.router.register(id, &o);
        net.observers.insert(id, o);
        let revision = net.sync().await;
        let o = net.observer(0);
        assert_eq!(o.revision(), revision);
        assert!(o.get::<VolumeSpec>(v.id).unwrap().is_some());
        assert_eq!(o.members().voters.len(), 3);
        net.shutdown().await;
    }

    /// 관찰 노드의 감시 이벤트는 투표 멤버의 것과 같다(같은 묶음을 그대로 낸다).
    #[tokio::test]
    async fn observer_watch_events_match_the_voter() {
        let net = Net::start(2).await;
        let revision = net.sync().await;
        let mut watches: Vec<_> = std::iter::once(net.leader())
            .chain(net.observers.values())
            .map(|c| c.watch(revision).unwrap())
            .collect();
        let v = volume("w");
        net.leader().create(&v).await.unwrap();
        net.observer(1)
            .update_spec(
                v.id,
                None,
                BTreeMap::from([("a".into(), "b".into())]),
                &v.spec,
            )
            .await
            .unwrap();
        net.observer(0)
            .update_status::<VolumeSpec>(v.id, &Default::default())
            .await
            .unwrap();
        net.leader().delete::<VolumeSpec>(v.id, None).await.unwrap();

        let expected = next_events(&mut watches[0], 4).await;
        assert!(matches!(expected[3], WatchEvent::Deleted { .. }));
        for w in &mut watches[1..] {
            assert_eq!(next_events(w, 4).await, expected);
        }
        net.shutdown().await;
    }

    fn log_id(index: u64) -> openraft::LogId<NodeId> {
        openraft::LogId::new(openraft::CommittedLeaderId::new(1, NodeId::from(1)), index)
    }

    /// `revision`에 쓰인 볼륨
    fn raw(name: &str, revision: u64) -> crate::types::RawResource {
        let mut r: crate::types::RawResource =
            serde_json::from_value(serde_json::to_value(volume(name)).unwrap()).unwrap();
        r.revision = revision;
        r
    }

    fn put(r: &crate::types::RawResource) -> WatchEvent {
        WatchEvent::Put {
            kind: opl_types::resource::Kind::Volume,
            resource: r.clone(),
        }
    }

    fn hello() -> FollowFrame {
        FollowFrame::Hello {
            role: crate::follow::NodeRole::Observer,
            leader: Some((NodeId::from(1), "node-1".into())),
            voters: BTreeMap::from([(NodeId::from(1), "node-1".into())]),
        }
    }

    /// 정해 둔 프레임을 돌려주는 Follow 통로(투표 멤버마다 묶음 경계·스냅샷이 다른 경우를 만든다). 부를 때마다 다음
    /// 대본을 쓰고, `hold`인 대본은 끝에서 끊지 않고 머문다. 대본이 떨어지면 붙지 못한다.
    #[derive(Default)]
    struct Scripted {
        scripts: Mutex<std::collections::VecDeque<(Vec<FollowFrame>, bool)>>,
    }

    impl Scripted {
        fn then(self, frames: Vec<FollowFrame>, hold: bool) -> Self {
            self.scripts.lock().unwrap().push_back((frames, hold));
            self
        }
    }

    #[async_trait::async_trait]
    impl FollowClient for Scripted {
        async fn follow(
            &self,
            _to: NodeId,
            _addr: &str,
            _since: u64,
        ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
            let Some((frames, hold)) = self.scripts.lock().unwrap().pop_front() else {
                return Err(ClusterError::Unreachable("no more scripts".into()));
            };
            let frames = futures_util::stream::iter(frames.into_iter().map(Ok));
            Ok(if hold {
                frames.chain(futures_util::stream::pending()).boxed()
            } else {
                frames.boxed()
            })
        }
    }

    /// 대본대로 받는 관찰 노드(빈 메모리 저장소, 투표 멤버 1을 따른다)
    async fn scripted_observer(root: &TempDir, script: Scripted) -> Cluster {
        let router = MemRouter::default();
        let id = NodeId::from(9);
        let mut config = config(root, 9, true);
        config.follow_seed = BTreeMap::from([(NodeId::from(1), "node-1".into())]);
        Cluster::open_observer(config, router.network(id), Arc::new(script))
            .await
            .unwrap()
    }

    /// 감시에서 더 오지 않을 때까지 받은 이벤트의 리비전
    async fn drain(w: &mut crate::watch::Watch) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        while let Ok(e) = tokio::time::timeout(Duration::from_millis(300), w.next()).await {
            out.push(e.unwrap());
        }
        out
    }

    /// 다른 투표 멤버로 옮기면 묶음 경계가 달라 새 묶음이 이미 적용한 리비전의 이벤트를 품을 수 있다. 관찰 노드는
    /// 그 이벤트를 다시 내지 않는다(감시 리비전이 엄격히 오른다).
    #[tokio::test]
    async fn events_already_applied_are_not_replayed_after_moving() {
        let root = TempDir::new().unwrap();
        let (a, b, c) = (raw("a", 11), raw("b", 12), raw("c", 13));
        let script = Scripted::default()
            .then(
                vec![
                    hello(),
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                false,
            )
            // 다른 투표 멤버는 11..=13을 한 묶음으로 적용했다.
            .then(
                vec![
                    hello(),
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b), put(&c)],
                        applied: log_id(13),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(13, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12, 13]);
        o.shutdown().await.unwrap();
    }

    /// 자기보다 뒤인 상류가 보낸 스냅샷(더 낮은 리비전)은 설치하지 않는다: 상태와 리비전이 뒤로 가지 않고 `Reset`도
    /// 없다. 그 뒤 묶음은 이어 받는다.
    #[tokio::test]
    async fn an_older_snapshot_from_the_upstream_is_not_installed() {
        let root = TempDir::new().unwrap();
        let (x, y, z, w) = (raw("x", 5), raw("y", 20), raw("z", 14), raw("w", 21));
        let snapshot = |objects: Vec<&crate::types::RawResource>, applied| {
            vec![
                FollowFrame::SnapshotPart {
                    objects: objects
                        .into_iter()
                        .map(|r| (opl_types::resource::Kind::Volume, r.clone()))
                        .collect(),
                },
                FollowFrame::SnapshotEnd {
                    applied: Some(log_id(applied)),
                    membership: Default::default(),
                },
            ]
        };
        let mut first = vec![hello()];
        first.extend(snapshot(vec![&x], 10));
        first.push(FollowFrame::Batch {
            events: vec![put(&y)],
            applied: log_id(20),
            membership: None,
        });
        let mut second = vec![hello()];
        second.extend(snapshot(vec![&x, &z], 15));
        second.push(FollowFrame::Batch {
            events: vec![put(&w)],
            applied: log_id(21),
            membership: None,
        });
        let script = Scripted::default().then(first, false).then(second, true);
        let o = scripted_observer(&root, script).await;
        o.wait_revision(20, WAIT).await.unwrap();
        let mut watch = o.watch(20).unwrap();
        o.wait_revision(21, WAIT).await.unwrap();
        let events = drain(&mut watch).await;
        assert!(
            matches!(events.as_slice(), [WatchEvent::Put { resource, .. }] if resource.id == w.id),
            "{events:?}"
        );
        assert!(o.get::<VolumeSpec>(y.id).unwrap().is_some());
        assert!(o.get::<VolumeSpec>(z.id).unwrap().is_none());
        assert_eq!(o.revision(), 21);
        o.shutdown().await.unwrap();
    }

    /// 자기와 같은 리비전의 스냅샷도 설치하지 않는다(상태가 같다): 구독자는 헛된 `Reset` 없이 다음 묶음만 본다.
    #[tokio::test]
    async fn a_snapshot_at_the_same_revision_is_not_installed_again() {
        let root = TempDir::new().unwrap();
        let (x, y) = (raw("x", 10), raw("y", 11));
        let snapshot = || {
            vec![
                FollowFrame::SnapshotPart {
                    objects: vec![(opl_types::resource::Kind::Volume, x.clone())],
                },
                FollowFrame::SnapshotEnd {
                    applied: Some(log_id(10)),
                    membership: Default::default(),
                },
            ]
        };
        let mut first = vec![hello()];
        first.extend(snapshot());
        let mut second = vec![hello()];
        second.extend(snapshot());
        second.push(FollowFrame::Batch {
            events: vec![put(&y)],
            applied: log_id(11),
            membership: None,
        });
        let script = Scripted::default().then(first, false).then(second, true);
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut watch = o.watch(10).unwrap();
        o.wait_revision(11, WAIT).await.unwrap();
        let events = drain(&mut watch).await;
        assert!(
            matches!(events.as_slice(), [WatchEvent::Put { resource, .. }] if resource.id == y.id),
            "{events:?}"
        );
        o.shutdown().await.unwrap();
    }

    /// 투표 멤버는 자기보다 앞선 관찰 노드(`since`가 자기 리비전보다 큼)를 받지 않는다: `Hello`만 보내고 닫아 다른
    /// 투표 멤버로 가게 한다(뒤처진 노드가 그 뒤 스냅샷을 설치하면 관찰 노드가 뒤로 가므로).
    #[tokio::test]
    async fn a_voter_turns_away_an_observer_ahead_of_it() {
        let net = Net::start(0).await;
        let leader = net.settled_leader().await;
        let voter = &net.voters[&leader];
        let frames: Vec<FollowFrame> = tokio::time::timeout(
            Duration::from_secs(3),
            voter
                .follow(NodeId::from(9), voter.revision() + 100)
                .unwrap()
                .collect(),
        )
        .await
        .expect("the stream was not closed");
        assert!(
            matches!(frames.as_slice(), [FollowFrame::Hello { .. }]),
            "{frames:?}"
        );
        net.shutdown().await;
    }

    /// 뒤처진 투표 멤버(Raft가 끊김)가 리더의 스냅샷을 설치해도, 그보다 앞선 관찰 노드는 뒤로 가지 않는다(그런 투표
    /// 멤버에 붙지 않는다).
    #[tokio::test]
    async fn an_observer_ahead_of_a_lagging_voter_never_goes_back() {
        let mut net = Net::start_with(1, |raft| {
            raft.max_in_snapshot_log_to_keep = 0;
            raft.purge_batch_size = 1;
        })
        .await;
        let leader = net.settled_leader().await;
        let r = replica(net.observer(0));
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        let lagging = *net
            .voters
            .keys()
            .find(|id| **id != leader && **id != upstream)
            .unwrap();
        net.router.block_raft(lagging);
        for i in 0..5 {
            net.leader()
                .create(&volume(&format!("old-{i}")))
                .await
                .unwrap();
        }
        // 리더가 스냅샷을 만들고 로그를 지워 뒤처진 노드는 스냅샷으로만 따라잡는다.
        let raft = net.leader().raft().unwrap();
        raft.trigger().snapshot().await.unwrap();
        let purged = raft
            .wait(Some(WAIT))
            .metrics(|m| m.purged.is_some(), "leader purged logs")
            .await
            .unwrap()
            .purged
            .unwrap()
            .index;
        for i in 0..20 {
            net.leader()
                .create(&volume(&format!("new-{i}")))
                .await
                .unwrap();
        }
        let ahead = net.sync().await;
        assert!(ahead > purged);
        let mut w = net.observer(0).watch(ahead).unwrap();

        // 붙은 노드를 잃은 관찰 노드는 리더가 아닌 투표 멤버인 뒤처진 노드를 먼저 시도한다(그 노드는 받지 않는다).
        let tried = net.router.follows(lagging);
        net.stop_voter(upstream).await;
        wait_until("the observer tried the lagging voter", || {
            net.router.follows(lagging) > tried
        })
        .await;
        net.router.heal(lagging);
        net.voters[&lagging]
            .wait_revision(ahead, WAIT)
            .await
            .unwrap();
        let v = volume("after");
        net.leader().create(&v).await.unwrap();
        net.sync().await;
        let o = net.observer(0);
        assert!(o.get::<VolumeSpec>(v.id).unwrap().is_some());
        let events = drain(&mut w).await;
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, WatchEvent::Reset { .. })),
            "{events:?}"
        );
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 살아 있지만 뒤처진 투표 멤버(Raft가 끊겨 `Progress`만 보냄)에 붙은 관찰 노드는 리스 갱신의 적용 기다림이 시간
    /// 초과하거나 리더가 돌려준 인덱스보다 오래 뒤처지면 다른 투표 멤버로 옮겨, 갱신이 다시 성공한다.
    #[tokio::test]
    async fn an_observer_leaves_a_lagging_voter_and_renews_its_lease() {
        let net = Net::start(1).await;
        let leader = net.settled_leader().await;
        let o = net.observer(0);
        let r = replica(o);
        net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        assert_ne!(upstream, leader);
        net.router.block_raft(upstream);
        net.leader().create(&volume("unseen")).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut failed = 0;
        while let Err(e) = o.renew_lease(Duration::from_secs(2)).await {
            failed += 1;
            assert!(
                tokio::time::Instant::now() < deadline,
                "renewals kept failing ({failed}): {e}"
            );
        }
        assert_ne!(r.status().upstream, Some(upstream));
        net.router.heal(upstream);
        net.shutdown().await;
    }

    /// 나눠 온 묶음(`BatchPart`… `Batch`)은 마지막 프레임에서 한 번에 적용한다. 그 전에 끊기면 앞부분은 버린다.
    #[tokio::test]
    async fn split_batches_are_applied_at_once() {
        let root = TempDir::new().unwrap();
        let (a, b, c) = (raw("a", 11), raw("b", 12), raw("c", 13));
        let snapshot = FollowFrame::SnapshotEnd {
            applied: Some(log_id(10)),
            membership: Default::default(),
        };
        let script = Scripted::default()
            // 앞부분만 오고 멈춘 뒤(진행 없음으로) 끊긴다.
            .then(
                vec![
                    hello(),
                    snapshot,
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                ],
                true,
            )
            .then(
                vec![
                    hello(),
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                    FollowFrame::BatchPart {
                        events: vec![put(&b)],
                    },
                    FollowFrame::Batch {
                        events: vec![put(&c)],
                        applied: log_id(13),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        // 앞부분만으로는 적용하지 않는다.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(o.revision(), 10);
        assert!(o.get::<VolumeSpec>(a.id).unwrap().is_none());
        o.wait_revision(13, WAIT).await.unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12, 13]);
        for r in [&a, &b, &c] {
            assert!(o.get::<VolumeSpec>(r.id).unwrap().is_some());
        }
        o.shutdown().await.unwrap();
    }

    /// 나눈 묶음 사이에 다른 프레임이 오면(서버는 그러지 않는다) 모은 앞부분을 버리고 다시 붙는다.
    #[tokio::test]
    async fn frames_out_of_order_drop_the_parts_and_reconnect() {
        let root = TempDir::new().unwrap();
        let (a, b) = (raw("a", 11), raw("b", 12));
        let script = Scripted::default()
            .then(
                vec![
                    hello(),
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::Batch {
                        events: vec![put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                true,
            )
            .then(
                vec![
                    hello(),
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        o.wait_revision(12, WAIT).await.unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12]);
        assert!(replica(&o).status().reconnects >= 1);
        o.shutdown().await.unwrap();
    }

    /// 스냅샷으로 따라잡는 관찰 노드는 스냅샷이 `UPSTREAM_LAG_MAX`보다 오래 걸려도(조각마다 1초) 리스 갱신이 도는
    /// 동안(리더가 돌려준 인덱스, 시간 초과한 적용 기다림) 붙은 노드를 버리지 않고 끝까지 받아 따라잡는다.
    #[tokio::test]
    async fn a_slow_snapshot_catch_up_is_not_cut_off_by_lag_checks() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let revision = net.sync().await;
        net.router.isolate(o.id());
        net.write_past_history(revision).await;
        let target = net.leader().revision();
        net.router.heal(o.id());
        net.router.slow_snapshots(o.id(), Duration::from_secs(1));
        let renew = async {
            loop {
                let _ = o.renew_lease(Duration::from_secs(1)).await;
            }
        };
        tokio::select! {
            _ = renew => unreachable!(),
            caught_up = o.wait_revision(target, Duration::from_secs(40)) => {
                caught_up.expect("the observer never finished the snapshot");
            }
        }
        o.renew_lease(Duration::from_secs(5)).await.unwrap();
        net.shutdown().await;
    }
}
