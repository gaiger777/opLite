//! 관찰 노드가 투표 멤버 하나에서 받는 스트림(설계 §6): 스냅샷(처음·뒤처졌을 때) + 적용 묶음 + 1초마다 진행.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use openraft::{BasicNode, LogId, StoredMembership};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::cluster::{Backend, ClusterError, NodeHandle};
use crate::state_machine::snapshot_objects;
use crate::types::{NodeId, RawResource};
use crate::watch::{Feed, FeedBatch, FeedItem, WatchError, WatchEvent};

/// 서버가 이만큼마다 `Progress`를 보낸다. 관찰 노드는 이것으로 연결이 살아 있는지 안다.
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// 스냅샷 조각 하나에 담는 리소스 수
const SNAPSHOT_PART_OBJECTS: usize = 256;

/// 서버가 보내고 아직 읽히지 않은 프레임 수. 받는 쪽이 느리면 서버의 피드가 `Lagged`로 끝난다.
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
        membership: StoredMembership<NodeId, BasicNode>,
    },
    /// 서버가 적용한 묶음([`FeedBatch`]). 이벤트가 없는 묶음도 적용 위치로 온다.
    Batch {
        events: Vec<WatchEvent>,
        applied: LogId<NodeId>,
        membership: Option<StoredMembership<NodeId, BasicNode>>,
    },
    /// 서버가 적용한 위치와 서버가 아는 리더
    Progress {
        applied: Option<LogId<NodeId>>,
        leader: Option<NodeId>,
        leader_addr: Option<String>,
    },
}

impl FollowFrame {
    fn batch(b: &FeedBatch) -> Self {
        FollowFrame::Batch {
            events: b.events.clone(),
            applied: b.applied,
            membership: b.membership.clone(),
        }
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
                .and_then(|l| membership.get_node(&l).map(|n| (l, n.addr.clone())));
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
        .filter_map(|id| membership.get_node(&id).map(|n| (id, n.addr.clone())))
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
    let mut feed = match node.hub().feed(since) {
        Ok(feed) => feed,
        Err(WatchError::Expired { .. }) => send_snapshot(node, tx).await?,
        Err(e) => return Err(e.to_string()),
    };
    let mut tick = tokio::time::interval(PROGRESS_INTERVAL);
    loop {
        let frame = tokio::select! {
            item = feed.next() => match item.map_err(|e| e.to_string())? {
                FeedItem::Batch(b) => FollowFrame::batch(&b),
                // 이 노드가 스냅샷을 설치해 상태가 통째로 바뀌었다. 받는 쪽에도 스냅샷을 보낸다.
                FeedItem::Reset { .. } => {
                    feed = send_snapshot(node, tx).await?;
                    continue;
                }
            },
            _ = tick.tick() => {
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
                FollowFrame::Progress {
                    applied: m.last_applied,
                    leader: m.current_leader,
                    leader_addr: m
                        .current_leader
                        .and_then(|l| membership.get_node(&l).map(|n| n.addr.clone())),
                }
            }
        };
        tx.send(frame)
            .await
            .map_err(|_| "the follower went away".to_string())?;
    }
}

/// 지금 상태를 스냅샷으로 보내고, 그 리비전 뒤의 피드를 돌려준다.
async fn send_snapshot(node: &NodeHandle, tx: &mpsc::Sender<FollowFrame>) -> Result<Feed, String> {
    let (applied, membership, objects, feed) = loop {
        let db = node.db().clone();
        let (applied, membership, objects) =
            tokio::task::spawn_blocking(move || snapshot_objects(&db))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        // 읽은 뒤 기록이 그 리비전을 지나쳤으면(아주 드묾) 다시 뜬다.
        match node.hub().feed(applied.map_or(0, |l| l.index)) {
            Ok(feed) => break (applied, membership, objects, feed),
            Err(WatchError::Expired { .. }) => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let gone = |_| "the follower went away".to_string();
    let mut objects = objects.into_iter().peekable();
    while objects.peek().is_some() {
        let part = objects.by_ref().take(SNAPSHOT_PART_OBJECTS).collect();
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

#[cfg(test)]
mod tests {
    use openraft::CommittedLeaderId;
    use opl_types::resource::Resource;
    use serde_json::json;

    use super::*;

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
