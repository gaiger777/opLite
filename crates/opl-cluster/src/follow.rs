//! 관찰 노드가 투표 멤버 하나에서 받는 스트림(설계 §6): 스냅샷(처음·뒤처졌을 때) + 적용 묶음 + 1초마다 진행.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use openraft::{LogId, RaftMetrics, StoredMembership};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::cluster::{Backend, Cluster, ClusterError, NodeHandle, observers_of};
use crate::state_machine::snapshot_objects;
use crate::types::{NodeId, OrbNode, RawResource};
use crate::watch::{Feed, FeedBatch, FeedItem, WatchError, WatchEvent};

/// 서버가 이만큼마다 `Progress`를 보낸다. 관찰 노드는 이것으로 연결이 살아 있는지 안다.
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// 스냅샷 조각 하나에 담는 리소스 수
const SNAPSHOT_PART_OBJECTS: usize = 256;

/// 스냅샷 조각 하나에 담는 리소스의 JSON 크기(이를 넘기 전에 자른다. 리소스 하나가 이보다 크면 그것만 담는다).
/// 조각은 gRPC 메시지 하나로 가므로 노드 간 메시지 한도(opl-peer `MAX_MESSAGE_BYTES`, 64 MiB)보다 넉넉히 작게 둔다.
/// 큰 적용 묶음도 이 크기로 나눠 보낸다(`BatchPart`).
const SNAPSHOT_PART_BYTES: usize = 4 << 20;

/// 서버가 보내고 아직 읽히지 않은 프레임 수. 이만큼 쌓이면 서버는 보내기를 멈추고, 그동안 서버의 피드가 기록
/// (`HISTORY` 묶음)보다 뒤처지면 `Lagged`로 끝난다(받는 쪽은 다시 붙는다).
const FRAME_BUFFER: usize = 16;

/// 노드가 클러스터에 무엇으로 참여하는지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    Voter,
    Learner,
    /// Raft 구성원이 아니다(관찰 노드).
    Observer,
}

/// Follow 스트림의 프레임
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "kebab-case")]
pub enum FollowFrame {
    /// 처음 프레임. `role`은 서버가 보는 부른 노드의 역할이다. 서버가 Raft 구성원이 아니면 이것만 보내고
    /// 닫는다(다른 투표 멤버로 가라는 뜻).
    Hello {
        role: NodeRole,
        leader: Option<(NodeId, String)>,
        voters: BTreeMap<NodeId, String>,
    },
    /// 스냅샷의 리소스 일부. `SnapshotEnd`까지 모아 한 번에 설치한다.
    SnapshotPart { objects: Vec<(Kind, RawResource)> },
    /// 스냅샷 끝. 상태 전체를 앞 조각들로 바꾼다(구독자에게는 `Reset`).
    SnapshotEnd {
        applied: Option<LogId<NodeId>>,
        membership: StoredMembership<NodeId, OrbNode>,
    },
    /// 큰 묶음의 앞부분 이벤트(메시지 한도 때문에 나눔). 뒤따르는 `Batch`와 함께 한 번에 적용한다.
    BatchPart { events: Vec<WatchEvent> },
    /// 서버가 적용한 묶음([`FeedBatch`])(앞서 온 `BatchPart`의 이벤트가 그 앞에 붙는다). 이벤트가 없는 묶음도 적용
    /// 위치로 온다.
    Batch {
        events: Vec<WatchEvent>,
        applied: LogId<NodeId>,
        membership: Option<StoredMembership<NodeId, OrbNode>>,
    },
    /// 서버가 적용한 위치와 서버가 아는 리더
    Progress {
        applied: Option<LogId<NodeId>>,
        leader: Option<NodeId>,
        leader_addr: Option<String>,
    },
}

impl FollowFrame {
    /// 묶음 하나를 보낼 프레임들: 이벤트의 JSON이 `max_bytes`를 넘으면 앞부분을 `BatchPart`로 나누고 마지막 `Batch`가
    /// 적용 위치와 멤버십을 싣는다.
    fn batch(b: &FeedBatch, max_bytes: usize) -> Vec<Self> {
        let mut events = b.events.clone().into_iter().peekable();
        let mut frames = Vec::new();
        loop {
            let part = take_part(&mut events, usize::MAX, max_bytes);
            if events.peek().is_none() {
                frames.push(FollowFrame::Batch {
                    events: part,
                    applied: b.applied,
                    membership: b.membership.clone(),
                });
                return frames;
            }
            frames.push(FollowFrame::BatchPart { events: part });
        }
    }

    /// 이 프레임이 바꾼 클러스터 정보의 관찰 노드 목록(프레임 안의 마지막 것). 클러스터 정보를 싣지 않으면 `None`이다.
    /// 지운 관찰 노드의 열린 스트림이 목록에서 빠지는 프레임부터 받지 않게 한다(목록 캐시의 갱신을 기다리지 않고).
    pub fn observers(&self) -> Option<BTreeMap<NodeId, OrbNode>> {
        let events = match self {
            FollowFrame::Batch { events, .. } | FollowFrame::BatchPart { events } => events,
            FollowFrame::SnapshotPart { objects } => {
                return objects
                    .iter()
                    .rev()
                    .find(|(kind, _)| *kind == Kind::Cluster)
                    .map(|(_, info)| observers_of(info));
            }
            _ => return None,
        };
        events.iter().rev().find_map(|e| match e {
            WatchEvent::Put {
                kind: Kind::Cluster,
                resource,
            } => Some(observers_of(resource)),
            WatchEvent::Deleted {
                kind: Kind::Cluster,
                ..
            } => Some(BTreeMap::new()),
            _ => None,
        })
    }
}

/// 열린 Follow 스트림을 프레임마다 다시 허가한다(노드 간 네트워크가 쓴다). 부른 쪽이 이 노드의 Raft 멤버십이나
/// 관찰 노드 목록에 있어야 한다. 목록은 이 스트림의 프레임에 실린 것을 먼저 본다(이 노드의 목록 캐시는 따로 갱신되어
/// 늦을 수 있다): 목록에서 부른 쪽을 빼는 프레임부터 허가하지 않는다(지운 관찰 노드가 그 뒤의 쓰기를 받지 않게).
pub struct FollowGate {
    caller: NodeId,
    /// 이 스트림이 마지막으로 실은 관찰 노드 목록에 부른 쪽이 있는지(아직 싣지 않았으면 `None`)
    listed: Option<bool>,
}

impl FollowGate {
    pub fn new(caller: NodeId) -> Self {
        Self {
            caller,
            listed: None,
        }
    }

    /// `frame`을 부른 쪽에 보내도 되는지
    pub fn admits(&mut self, cluster: &Cluster, frame: &FollowFrame) -> bool {
        if let Some(observers) = frame.observers() {
            self.listed = Some(observers.contains_key(&self.caller));
        }
        cluster.in_membership(self.caller)
            || self
                .listed
                .unwrap_or_else(|| cluster.knows_node(self.caller))
    }
}

/// 마지막으로 프레임으로 나눈 묶음과 그 프레임. 관찰 노드마다의 스트림이 같은 묶음(`Arc`)을 받으므로 한 번만
/// 나눈다(이벤트를 복제하고 JSON 크기를 재는 일을 관찰 노드마다 하지 않게).
#[derive(Default)]
pub(crate) struct BatchFrames(Mutex<Option<SplitBatch>>);

/// 나눈 묶음과 그 프레임
type SplitBatch = (Weak<FeedBatch>, Arc<[FollowFrame]>);

impl BatchFrames {
    fn get(&self, batch: &Arc<FeedBatch>) -> Arc<[FollowFrame]> {
        let mut last = self.0.lock().unwrap();
        // `Weak`을 쥐는 동안 그 자리는 다른 묶음에 쓰이지 않으므로 주소로 같은 묶음인지 안다.
        if let Some((seen, frames)) = &*last
            && std::ptr::eq(seen.as_ptr(), Arc::as_ptr(batch))
        {
            return frames.clone();
        }
        let frames: Arc<[FollowFrame]> = FollowFrame::batch(batch, SNAPSHOT_PART_BYTES).into();
        *last = Some((Arc::downgrade(batch), frames.clone()));
        frames
    }
}

/// 관찰 노드가 투표 멤버에 Follow를 거는 통로. 노드 간 네트워크가 구현한다.
#[async_trait]
pub trait FollowClient: Send + Sync + 'static {
    /// `addr`(클러스터 주소)의 노드 `to`에서 `since` 뒤의 상태를 받는다. 스트림의 `Err`나 끝은 연결이 끊긴 것이다.
    async fn follow(
        &self,
        to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError>;
}

/// 노드 `caller`에게 보낼 Follow 스트림. 프레임은 따로 띄운 작업이 만들고, 스트림을 버리면 그 작업도 끝난다.
pub(crate) fn serve(
    node: NodeHandle,
    caller: NodeId,
    since: u64,
) -> Result<BoxStream<'static, FollowFrame>, ClusterError> {
    let (hello, member) = hello(&node, caller)?;
    let (tx, rx) = mpsc::channel(FRAME_BUFFER);
    // 부른 쪽이 이 노드보다 앞섰다(다른 투표 멤버에서 옮겨 왔다). 받으면 이 노드가 그 사이 스냅샷을 설치할 때 부른
    // 쪽이 뒤로 가므로 `Hello`만 보내고 닫아 다른 투표 멤버로 가게 한다.
    let ahead = since > node.hub().revision();
    tokio::spawn(async move {
        if tx.send(hello).await.is_err() || !member {
            return;
        }
        if ahead {
            tracing::debug!(%caller, since, "the follower is ahead of this node; turning it away");
            return;
        }
        if let Err(why) = stream_changes(&node, since, &tx).await {
            tracing::debug!(%caller, %why, "follow stream ended");
        }
    });
    Ok(Box::pin(stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|frame| (frame, rx))
    })))
}

/// 처음 프레임과, 이 노드가 Raft 구성원(스트림을 이어 보냄)인지
fn hello(node: &NodeHandle, caller: NodeId) -> Result<(FollowFrame, bool), ClusterError> {
    let (membership, leader, member) = match node.backend() {
        Backend::Member { raft, .. } => {
            let m = raft.metrics().borrow().clone();
            let membership = m.membership_config.membership().clone();
            let leader = m
                .current_leader
                .and_then(|l| membership.get_node(&l).map(|n| (l, n.addr().to_string())));
            let member = membership.get_node(&node.id()).is_some();
            (membership, leader, member)
        }
        Backend::Observer(replica) => (
            replica.membership()?.membership().clone(),
            replica.leader(),
            false,
        ),
    };
    let voters: BTreeMap<NodeId, String> = membership
        .voter_ids()
        .filter_map(|id| membership.get_node(&id).map(|n| (id, n.addr().to_string())))
        .collect();
    let role = if voters.contains_key(&caller) {
        NodeRole::Voter
    } else if membership.get_node(&caller).is_some() {
        NodeRole::Learner
    } else {
        NodeRole::Observer
    };
    Ok((
        FollowFrame::Hello {
            role,
            leader,
            voters,
        },
        member,
    ))
}

/// `since` 뒤의 묶음을 보낸다(기록보다 오래면 스냅샷부터). 받는 쪽이 끊거나, 피드가 끊기거나, 이 노드의 Raft가
/// 멈추거나 구성원에서 빠지면 끝난다.
async fn stream_changes(
    node: &NodeHandle,
    since: u64,
    tx: &mpsc::Sender<FollowFrame>,
) -> Result<(), String> {
    let Some(raft) = node.raft() else {
        return Err("not a Raft member".into());
    };
    let mut metrics = raft.metrics();
    let mut tick = tokio::time::interval(PROGRESS_INTERVAL);
    let mut feed = match node.hub().feed(since) {
        Ok(feed) => feed,
        Err(WatchError::Expired { .. }) => send_snapshot(node, tx, &mut tick, &mut metrics).await?,
        Err(e) => return Err(e.to_string()),
    };
    loop {
        let frame = tokio::select! {
            item = feed.next() => match item.map_err(|e| e.to_string())? {
                FeedItem::Batch(b) => {
                    for frame in node.batch_frames().get(&b).iter() {
                        tx.send(frame.clone())
                            .await
                            .map_err(|_| "the follower went away".to_string())?;
                    }
                    continue;
                }
                // 이 노드가 스냅샷을 설치해 상태가 통째로 바뀌었다. 받는 쪽에도 스냅샷을 보낸다.
                FeedItem::Reset { .. } => {
                    feed = send_snapshot(node, tx, &mut tick, &mut metrics).await?;
                    continue;
                }
            },
            _ = tick.tick() => progress(node, &mut metrics)?,
        };
        tx.send(frame)
            .await
            .map_err(|_| "the follower went away".to_string())?;
    }
}

type Metrics = tokio::sync::watch::Receiver<RaftMetrics<NodeId, OrbNode>>;

/// 이 노드가 적용한 위치와 아는 리더. 이 노드의 Raft가 멈췄거나 구성원에서 빠졌으면 오류다.
fn progress(node: &NodeHandle, metrics: &mut Metrics) -> Result<FollowFrame, String> {
    if metrics.has_changed().is_err() {
        return Err("the Raft task ended".into());
    }
    let m = metrics.borrow_and_update().clone();
    if let Err(e) = &m.running_state {
        return Err(e.to_string());
    }
    let membership = m.membership_config.membership();
    if membership.get_node(&node.id()).is_none() {
        return Err("no longer a Raft member".into());
    }
    Ok(FollowFrame::Progress {
        applied: m.last_applied,
        leader: m.current_leader,
        leader_addr: m
            .current_leader
            .and_then(|l| membership.get_node(&l).map(|n| n.addr().to_string())),
    })
}

/// 지금 상태를 스냅샷으로 보내고, 그 리비전 뒤의 피드를 돌려준다. 스냅샷을 뜨는 동안에도 `tick`마다 `Progress`를
/// 보낸다(큰 상태를 읽는 동안 받는 쪽이 진행이 없다고 끊지 않게).
async fn send_snapshot(
    node: &NodeHandle,
    tx: &mpsc::Sender<FollowFrame>,
    tick: &mut tokio::time::Interval,
    metrics: &mut Metrics,
) -> Result<Feed, String> {
    let gone = |_| "the follower went away".to_string();
    let (applied, membership, objects, feed) = loop {
        let db = node.db().clone();
        let mut read = tokio::task::spawn_blocking(move || snapshot_objects(&db));
        let read = loop {
            tokio::select! {
                read = &mut read => break read,
                _ = tick.tick() => tx.send(progress(node, metrics)?).await.map_err(gone)?,
            }
        };
        let (applied, membership, objects) = read
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        // 읽은 뒤 기록이 그 리비전을 지나쳤으면(아주 드묾) 다시 뜬다.
        match node.hub().feed(applied.map_or(0, |l| l.index)) {
            Ok(feed) => break (applied, membership, objects, feed),
            Err(WatchError::Expired { .. }) => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut objects = objects.into_iter().peekable();
    while objects.peek().is_some() {
        let part = take_part(&mut objects, SNAPSHOT_PART_OBJECTS, SNAPSHOT_PART_BYTES);
        tx.send(FollowFrame::SnapshotPart { objects: part })
            .await
            .map_err(gone)?;
    }
    tx.send(FollowFrame::SnapshotEnd {
        applied,
        membership,
    })
    .await
    .map_err(gone)?;
    Ok(feed)
}

/// 다음 조각(스냅샷의 리소스나 묶음의 이벤트): `max_objects`개까지, JSON 크기가 `max_bytes`를 넘기 전까지(적어도
/// 하나, 남은 것이 없으면 빈 조각)
fn take_part<T: Serialize>(
    objects: &mut std::iter::Peekable<std::vec::IntoIter<T>>,
    max_objects: usize,
    max_bytes: usize,
) -> Vec<T> {
    let mut part = Vec::new();
    let mut bytes = 0;
    while part.len() < max_objects {
        let Some(next) = objects.peek() else { break };
        let size = json_len(next);
        if !part.is_empty() && bytes + size > max_bytes {
            break;
        }
        bytes += size;
        part.extend(objects.next());
    }
    part
}

/// 값을 JSON으로 쓴 길이(쓰지 않고 센다)
fn json_len(value: &impl Serialize) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // 리소스는 이미 JSON에서 읽은 값이라 다시 쓰는 데 실패하지 않는다.
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

#[cfg(test)]
mod tests {
    use openraft::CommittedLeaderId;
    use opl_types::resource::Resource;
    use serde_json::json;

    use super::*;
    use crate::state_machine::Objects;

    /// 큰 묶음은 이벤트를 `BatchPart`로 나눠 보내고 마지막 `Batch`만 적용 위치와 멤버십을 싣는다. 작은 묶음은 그대로다.
    #[test]
    fn large_batches_are_split_and_the_last_frame_carries_the_applied_index() {
        let applied = LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), 9);
        let event = |i: u64| {
            let mut resource: RawResource = Resource::new("a", json!({ "x": "y".repeat(1000) }));
            resource.revision = i;
            WatchEvent::Put {
                kind: Kind::Vm,
                resource,
            }
        };
        let batch = FeedBatch {
            events: (1..=5).map(event).collect(),
            applied,
            membership: Some(StoredMembership::default()),
        };
        let frames = FollowFrame::batch(&batch, 2500);
        let shape: Vec<(bool, Vec<u64>)> = frames
            .iter()
            .map(|f| match f {
                FollowFrame::BatchPart { events } => {
                    (false, events.iter().map(WatchEvent::revision).collect())
                }
                FollowFrame::Batch { events, .. } => {
                    (true, events.iter().map(WatchEvent::revision).collect())
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            shape,
            [(false, vec![1, 2]), (false, vec![3, 4]), (true, vec![5])]
        );
        assert!(matches!(
            frames.last(),
            Some(FollowFrame::Batch { applied: a, membership: Some(_), .. }) if *a == applied
        ));
        assert_eq!(
            FollowFrame::batch(&batch, usize::MAX),
            [FollowFrame::Batch {
                events: batch.events.clone(),
                applied,
                membership: batch.membership.clone(),
            }]
        );
        // 이벤트 없는 묶음도 `Batch` 하나다.
        let empty = FeedBatch {
            events: vec![],
            applied,
            membership: None,
        };
        assert!(matches!(
            FollowFrame::batch(&empty, 10).as_slice(),
            [FollowFrame::Batch { events, .. }] if events.is_empty()
        ));
    }

    /// 같은 묶음(`Arc`)은 스트림이 몇이든 한 번만 프레임으로 나눈다. 다른 묶음이 오면 다시 나눈다.
    #[test]
    fn batch_frames_are_made_once_per_batch() {
        let applied = |i| LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), i);
        let batch = |i| {
            Arc::new(FeedBatch {
                events: vec![],
                applied: applied(i),
                membership: None,
            })
        };
        let cache = BatchFrames::default();
        let (first, second) = (batch(1), batch(2));
        let made = cache.get(&first);
        assert!(Arc::ptr_eq(&made, &cache.get(&first)));
        let other = cache.get(&second);
        assert!(!Arc::ptr_eq(&made, &other));
        assert!(matches!(&other[..], [FollowFrame::Batch { applied: a, .. }] if *a == applied(2)));
    }

    /// 클러스터 정보를 바꾸는 프레임은 그 관찰 노드 목록을 알린다(지운 관찰 노드의 스트림을 그 프레임에서 끊는다).
    #[test]
    fn frames_tell_the_observers_they_list() {
        let applied = LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), 9);
        let observer = NodeId::from(7);
        let info = |observers: &[NodeId]| -> RawResource {
            let mut r: RawResource = Resource::new("c", json!({}));
            r.status = serde_json::to_value(opl_types::cluster::ClusterStatus {
                observers: observers
                    .iter()
                    .map(|id| (id.0, format!("node-{id}")))
                    .collect(),
                ..Default::default()
            })
            .unwrap();
            r
        };
        let put = |r: RawResource| WatchEvent::Put {
            kind: Kind::Cluster,
            resource: r,
        };
        let vm = WatchEvent::Put {
            kind: Kind::Vm,
            resource: Resource::new("a", json!({})),
        };
        let batch = |events| FollowFrame::Batch {
            events,
            applied,
            membership: None,
        };
        assert_eq!(batch(vec![vm.clone()]).observers(), None);
        // 마지막 클러스터 정보가 이긴다.
        let listed = batch(vec![put(info(&[])), vm.clone(), put(info(&[observer]))]);
        assert!(listed.observers().unwrap().contains_key(&observer));
        let unlisted = FollowFrame::BatchPart {
            events: vec![put(info(&[observer])), put(info(&[]))],
        };
        assert_eq!(unlisted.observers(), Some(BTreeMap::new()));
        let snapshot = FollowFrame::SnapshotPart {
            objects: vec![(Kind::Cluster, info(&[observer]))],
        };
        assert!(snapshot.observers().unwrap().contains_key(&observer));
        let progress = FollowFrame::Progress {
            applied: None,
            leader: None,
            leader_addr: None,
        };
        assert_eq!(progress.observers(), None);
    }

    /// 열린 스트림의 허가는 목록 캐시보다 스트림이 실은 목록을 먼저 본다: 캐시가 아직 관찰 노드를 적고 있어도
    /// 그것을 목록에서 빼는 프레임부터 막는다. Raft 멤버는 목록과 관계없이 받는다.
    #[tokio::test]
    async fn the_gate_closes_at_the_frame_that_unlists_the_caller() {
        use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};

        let router = crate::memnet::MemRouter::default();
        let id = NodeId::from(1u64);
        let mut config = crate::cluster::ClusterConfig::new(id, "unused");
        config.in_memory = true;
        let c = Cluster::open(config, router.network(id)).await.unwrap();
        router.register(id, &c);
        c.initialize(BTreeMap::from([(id, OrbNode::single("node-1"))]))
            .await
            .unwrap();
        let observer = NodeId::from(9u64);
        let status = |observers: &[NodeId]| ClusterStatus {
            observers: observers
                .iter()
                .map(|id| (id.0, format!("node-{id}")))
                .collect(),
            ..ClusterStatus::default()
        };
        let mut info: ClusterInfo = Resource::new(
            "test",
            ClusterSpec {
                ca_cert_pem: String::new(),
                ca_key_pem: String::new(),
            },
        );
        info.status = status(&[observer]);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match c.create(&info).await {
                Ok(_) => break,
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => panic!("{e}"),
            }
        }
        while !c.knows_node(observer) {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let progress = FollowFrame::Progress {
            applied: None,
            leader: None,
            leader_addr: None,
        };
        let mut unlisted: RawResource =
            serde_json::from_value(serde_json::to_value(&info).unwrap()).unwrap();
        unlisted.status = serde_json::to_value(status(&[])).unwrap();
        let removal = FollowFrame::Batch {
            events: vec![WatchEvent::Put {
                kind: Kind::Cluster,
                resource: unlisted,
            }],
            applied: LogId::new(CommittedLeaderId::new(1, id), 99),
            membership: None,
        };

        let mut gate = FollowGate::new(observer);
        assert!(gate.admits(&c, &progress));
        // 캐시는 아직 관찰 노드를 적고 있다(이 프레임은 이 노드에 적용되지 않았다).
        assert!(!gate.admits(&c, &removal));
        assert!(c.knows_node(observer));
        assert!(!gate.admits(&c, &progress));
        // Raft 멤버는 목록에 없어도 받는다.
        let mut member = FollowGate::new(id);
        assert!(member.admits(&c, &removal));
        c.shutdown().await.unwrap();
    }

    /// 스냅샷 조각은 리소스 수와 JSON 크기로 자른다(gRPC 메시지 한도). 한도보다 큰 리소스도 혼자 간다.
    #[test]
    fn snapshot_parts_are_cut_by_count_and_bytes() {
        let object = |kb: usize| -> (Kind, RawResource) {
            (
                Kind::Vm,
                Resource::new("a", json!({ "x": "y".repeat(kb << 10) })),
            )
        };
        let sizes = |objects: Objects, max_objects, max_bytes| -> Vec<usize> {
            let mut it = objects.into_iter().peekable();
            let mut out = Vec::new();
            while it.peek().is_some() {
                let part = take_part(&mut it, max_objects, max_bytes);
                assert!(!part.is_empty());
                out.push(part.len());
            }
            out
        };
        let small: Objects = (0..10).map(|_| object(1)).collect();
        assert_eq!(sizes(small.clone(), 4, usize::MAX), [4, 4, 2]);
        // 1 KiB 남짓한 리소스는 2.5 KiB에 둘씩 든다.
        assert_eq!(sizes(small, 100, 2560), [2, 2, 2, 2, 2]);
        let mixed = vec![object(1), object(8), object(1), object(1)];
        assert_eq!(sizes(mixed, 100, 4096), [1, 1, 2]);
        // 기본 한도: 4 MiB를 넘지 않게 자른다.
        let big: Objects = (0..10).map(|_| object(1024)).collect();
        let parts = sizes(big, SNAPSHOT_PART_OBJECTS, SNAPSHOT_PART_BYTES);
        assert_eq!(parts, [3, 3, 3, 1]);
    }

    /// 프레임은 노드 사이로 JSON으로 오간다(gRPC). 리비전 틈을 메우는 적용 위치와 멤버십까지 그대로 돌아온다.
    #[test]
    fn frames_round_trip_through_json() {
        let applied = LogId::new(CommittedLeaderId::new(3, NodeId::from(1)), 42);
        let mut resource: RawResource = Resource::new("a", json!({"x": 1}));
        resource.revision = 41;
        let frames = [
            FollowFrame::Hello {
                role: NodeRole::Observer,
                leader: Some((NodeId::from(1), "node-1".into())),
                voters: BTreeMap::from([(NodeId::from(1), "node-1".into())]),
            },
            FollowFrame::SnapshotPart {
                objects: vec![(Kind::Vm, resource.clone())],
            },
            FollowFrame::SnapshotEnd {
                applied: Some(applied),
                membership: StoredMembership::default(),
            },
            FollowFrame::BatchPart {
                events: vec![WatchEvent::Put {
                    kind: Kind::Vm,
                    resource: resource.clone(),
                }],
            },
            FollowFrame::Batch {
                events: vec![WatchEvent::Put {
                    kind: Kind::Vm,
                    resource,
                }],
                applied,
                membership: Some(StoredMembership::default()),
            },
            FollowFrame::Progress {
                applied: Some(applied),
                leader: None,
                leader_addr: None,
            },
        ];
        for frame in frames {
            let json = serde_json::to_vec(&frame).unwrap();
            assert_eq!(serde_json::from_slice::<FollowFrame>(&json).unwrap(), frame);
        }
    }
}
