//! 커밋된 리소스 변경을 구독한다. 최근 적용 묶음은 메모리에 보관해 `since` 이후부터 이어 받을 수 있다.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use openraft::{BasicNode, LogId, StoredMembership};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use crate::types::{NodeId, RawResource};

/// 메모리에 보관하는 최근 적용 묶음 수. 이보다 오래된 `since`는 `Expired`가 된다.
pub const HISTORY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WatchEvent {
    /// 리소스가 생성되거나 바뀌었다. 이 이벤트의 revision은 `resource.revision`이다.
    Put { kind: Kind, resource: RawResource },
    /// 리소스가 삭제되었다. 삭제 직전의 리소스를 담는다.
    Deleted {
        revision: u64,
        kind: Kind,
        resource: RawResource,
    },
    /// 스냅샷 설치로 상태가 통째로 바뀌었다. 구독자는 목록을 다시 읽어야 한다.
    Reset { revision: u64 },
}

impl WatchEvent {
    pub fn revision(&self) -> u64 {
        match self {
            WatchEvent::Put { resource, .. } => resource.revision,
            WatchEvent::Deleted { revision, .. } | WatchEvent::Reset { revision } => *revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchError {
    /// 요청한 revision 이후의 변경 일부를 더는 보관하지 않는다. 목록을 다시 읽고 새로 구독한다.
    #[error("revision {since} is too old (history starts after {earliest})")]
    Expired { since: u64, earliest: u64 },
    /// 구독자가 너무 느려 변경을 놓쳤다. 목록을 다시 읽고 새로 구독한다.
    #[error("watcher lagged behind")]
    Lagged,
    #[error("cluster store closed")]
    Closed,
}

/// 상태 머신이 한 번에 적용한 항목 묶음. 이벤트가 없는 항목(blank, 멤버십, 거부된 명령)도 `applied`로
/// 적용 위치를 알린다(리비전에 틈이 있어 이벤트만으로는 어디까지 적용했는지 모른다).
#[derive(Debug, Clone, PartialEq)]
pub struct FeedBatch {
    /// 묶음의 리소스 변경(revision 순서)
    pub events: Vec<WatchEvent>,
    /// 묶음의 마지막 항목
    pub applied: LogId<NodeId>,
    /// 묶음에 든 마지막 멤버십 변경
    pub membership: Option<StoredMembership<NodeId, BasicNode>>,
}

/// 피드 구독이 받는 것
#[derive(Debug, Clone)]
pub(crate) enum FeedItem {
    Batch(Arc<FeedBatch>),
    /// 스냅샷 설치로 상태가 통째로 바뀌었다.
    Reset {
        revision: u64,
    },
}

impl FeedItem {
    fn revision(&self) -> u64 {
        match self {
            FeedItem::Batch(b) => b.applied.index,
            FeedItem::Reset { revision } => *revision,
        }
    }
}

struct History {
    batches: VecDeque<Arc<FeedBatch>>,
    /// 이 revision보다 뒤의 묶음은 모두 `batches`에 있다.
    earliest: u64,
}

pub(crate) struct WatchHub {
    history: Mutex<History>,
    tx: broadcast::Sender<FeedItem>,
    /// 저장소에 적용한 마지막 인덱스(revision). 묶음 적용과 스냅샷 설치가 올린다.
    applied: watch::Sender<u64>,
}

impl WatchHub {
    /// `applied`: 저장소를 연 시점의 마지막 적용 인덱스. 그 이전 변경은 보관하지 않는다.
    pub(crate) fn new(applied: u64) -> Self {
        Self {
            history: Mutex::new(History {
                batches: VecDeque::new(),
                earliest: applied,
            }),
            tx: broadcast::channel(HISTORY).0,
            applied: watch::channel(applied).0,
        }
    }

    /// 커밋된 묶음을 알린다. 상태 머신이 redb 커밋 뒤에 부른다.
    pub(crate) fn publish(&self, batch: FeedBatch) {
        let batch = Arc::new(batch);
        let mut h = self.history.lock().unwrap();
        if h.batches.len() == HISTORY {
            let old = h.batches.pop_front().expect("history is full");
            h.earliest = old.applied.index;
        }
        h.batches.push_back(batch.clone());
        let _ = self.tx.send(FeedItem::Batch(batch.clone()));
        self.applied.send_replace(batch.applied.index);
    }

    /// 스냅샷 설치 뒤에 부른다. 보관한 변경을 버리고 `Reset`을 알린다.
    pub(crate) fn reset(&self, revision: u64) {
        let mut h = self.history.lock().unwrap();
        h.batches.clear();
        h.earliest = revision;
        let _ = self.tx.send(FeedItem::Reset { revision });
        self.applied.send_replace(revision);
    }

    /// 저장소에 적용한 마지막 인덱스
    pub(crate) fn revision(&self) -> u64 {
        *self.applied.borrow()
    }

    /// 적용 인덱스가 바뀔 때마다 깨어나는 수신자
    pub(crate) fn applied(&self) -> watch::Receiver<u64> {
        self.applied.subscribe()
    }

    /// `since` 뒤의 적용 묶음을 구독한다.
    pub(crate) fn feed(&self, since: u64) -> Result<Feed, WatchError> {
        let h = self.history.lock().unwrap();
        if since < h.earliest {
            return Err(WatchError::Expired {
                since,
                earliest: h.earliest,
            });
        }
        let backlog = h
            .batches
            .iter()
            .filter(|b| b.applied.index > since)
            .map(|b| FeedItem::Batch(b.clone()))
            .collect();
        // 잠금을 쥔 채 구독해야 backlog와 이후 묶음 사이에 빈틈이 없다.
        Ok(Feed {
            backlog,
            rx: self.tx.subscribe(),
            seen: since,
        })
    }

    pub(crate) fn subscribe(&self, since: u64) -> Result<Watch, WatchError> {
        Ok(Watch {
            feed: self.feed(since)?,
            pending: VecDeque::new(),
            since,
        })
    }
}

/// 적용 묶음 구독. `next()`는 적용 순서대로, `since` 뒤의 묶음(과 `Reset`)만 돌려준다.
pub(crate) struct Feed {
    backlog: VecDeque<FeedItem>,
    rx: broadcast::Receiver<FeedItem>,
    seen: u64,
}

impl Feed {
    pub(crate) async fn next(&mut self) -> Result<FeedItem, WatchError> {
        if let Some(item) = self.backlog.pop_front() {
            self.seen = item.revision();
            return Ok(item);
        }
        loop {
            match self.rx.recv().await {
                Ok(item @ FeedItem::Reset { .. }) => {
                    self.seen = item.revision();
                    return Ok(item);
                }
                // 목록을 읽은 직후 구독하면 그 목록에 이미 반영된 묶음이 올 수 있다.
                Ok(item) if item.revision() <= self.seen => continue,
                Ok(item) => {
                    self.seen = item.revision();
                    return Ok(item);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => return Err(WatchError::Lagged),
                Err(broadcast::error::RecvError::Closed) => return Err(WatchError::Closed),
            }
        }
    }
}

/// 변경 구독. `next()`는 revision 순서대로, `since` 이후의 변경만 돌려준다. 적용 묶음을 이벤트로 펼친다.
pub struct Watch {
    feed: Feed,
    pending: VecDeque<WatchEvent>,
    since: u64,
}

impl Watch {
    pub async fn next(&mut self) -> Result<WatchEvent, WatchError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            match self.feed.next().await? {
                FeedItem::Reset { revision } => return Ok(WatchEvent::Reset { revision }),
                // `since`가 묶음 가운데면 그 앞의 변경은 건너뛴다.
                FeedItem::Batch(batch) => self.pending.extend(
                    batch
                        .events
                        .iter()
                        .filter(|e| e.revision() > self.since)
                        .cloned(),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openraft::CommittedLeaderId;
    use opl_types::resource::Resource;
    use serde_json::json;

    fn log_id(index: u64) -> LogId<NodeId> {
        LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), index)
    }

    /// `applied`까지 적용한 묶음
    fn batch(applied: u64, events: Vec<WatchEvent>) -> FeedBatch {
        FeedBatch {
            events,
            applied: log_id(applied),
            membership: None,
        }
    }

    fn put(revision: u64) -> WatchEvent {
        let mut resource: RawResource = Resource::new("a", json!({}));
        resource.revision = revision;
        WatchEvent::Put {
            kind: Kind::Vm,
            resource,
        }
    }

    #[tokio::test]
    async fn replays_history_after_since_then_follows_live() {
        let hub = WatchHub::new(0);
        hub.publish(batch(3, vec![put(1), put(2), put(3)]));
        let mut w = hub.subscribe(1).unwrap();
        assert_eq!(w.next().await.unwrap().revision(), 2);
        assert_eq!(w.next().await.unwrap().revision(), 3);
        hub.publish(batch(4, vec![put(4)]));
        assert_eq!(w.next().await.unwrap().revision(), 4);
    }

    #[tokio::test]
    async fn skips_live_events_at_or_before_since() {
        let hub = WatchHub::new(0);
        let mut w = hub.subscribe(5).unwrap();
        hub.publish(batch(6, vec![put(5), put(6)]));
        assert_eq!(w.next().await.unwrap().revision(), 6);
    }

    #[test]
    fn since_before_history_is_expired() {
        let hub = WatchHub::new(10);
        assert_eq!(
            hub.subscribe(9).err(),
            Some(WatchError::Expired {
                since: 9,
                earliest: 10
            })
        );
        assert!(hub.subscribe(10).is_ok());
    }

    #[test]
    fn evicting_old_events_moves_earliest() {
        let hub = WatchHub::new(0);
        for i in 1..=HISTORY as u64 + 1 {
            hub.publish(batch(i, vec![put(i)]));
        }
        assert!(matches!(
            hub.subscribe(0),
            Err(WatchError::Expired { earliest: 1, .. })
        ));
        assert!(hub.subscribe(1).is_ok());
    }

    #[tokio::test]
    async fn reset_clears_history_and_is_always_delivered() {
        let hub = WatchHub::new(0);
        hub.publish(batch(2, vec![put(1), put(2)]));
        let mut w = hub.subscribe(2).unwrap();
        hub.reset(2);
        assert_eq!(w.next().await.unwrap(), WatchEvent::Reset { revision: 2 });
        assert!(matches!(hub.subscribe(1), Err(WatchError::Expired { .. })));
    }

    #[tokio::test]
    async fn slow_watcher_gets_lagged() {
        let hub = WatchHub::new(0);
        let mut w = hub.subscribe(0).unwrap();
        for i in 1..=HISTORY as u64 + 1 {
            hub.publish(batch(i, vec![put(i)]));
        }
        assert_eq!(w.next().await, Err(WatchError::Lagged));
    }

    /// 이벤트가 없는 묶음(blank·멤버십)도 피드에 적용 위치로 온다. 변경 구독은 그것을 건너뛴다.
    #[tokio::test]
    async fn feed_carries_batches_without_events_and_watch_skips_them() {
        let hub = WatchHub::new(0);
        let mut feed = hub.feed(0).unwrap();
        let mut w = hub.subscribe(0).unwrap();
        hub.publish(batch(1, vec![]));
        hub.publish(batch(3, vec![put(2)]));
        let FeedItem::Batch(first) = feed.next().await.unwrap() else {
            panic!("expected a batch");
        };
        assert_eq!((first.applied.index, first.events.len()), (1, 0));
        let FeedItem::Batch(second) = feed.next().await.unwrap() else {
            panic!("expected a batch");
        };
        assert_eq!(second.applied.index, 3);
        assert_eq!(w.next().await.unwrap().revision(), 2);
        // 이벤트 없는 묶음 뒤에서 이어 구독해도 그 뒤 묶음만 온다.
        let mut late = hub.feed(1).unwrap();
        assert_eq!(late.next().await.unwrap().revision(), 3);
    }

    #[test]
    fn publish_and_reset_move_the_applied_revision() {
        let hub = WatchHub::new(4);
        let rx = hub.applied();
        assert_eq!(hub.revision(), 4);
        hub.publish(batch(5, vec![]));
        assert_eq!((hub.revision(), *rx.borrow()), (5, 5));
        hub.reset(9);
        assert_eq!((hub.revision(), *rx.borrow()), (9, 9));
    }
}
