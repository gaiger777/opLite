//! 커밋된 리소스 변경을 구독한다. 최근 변경은 메모리에 보관해 `since` 이후부터 이어 받을 수 있다.

use std::collections::VecDeque;
use std::sync::Mutex;

use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::types::RawResource;

/// 메모리에 보관하는 최근 변경 수. 이보다 오래된 `since`는 `Expired`가 된다.
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

struct History {
    events: VecDeque<WatchEvent>,
    /// 이 revision보다 큰 변경은 모두 `events`에 있다.
    earliest: u64,
}

pub(crate) struct WatchHub {
    history: Mutex<History>,
    tx: broadcast::Sender<WatchEvent>,
}

impl WatchHub {
    /// `applied`: 저장소를 연 시점의 마지막 적용 인덱스. 그 이전 변경은 보관하지 않는다.
    pub(crate) fn new(applied: u64) -> Self {
        Self {
            history: Mutex::new(History {
                events: VecDeque::new(),
                earliest: applied,
            }),
            tx: broadcast::channel(HISTORY).0,
        }
    }

    /// 커밋된 변경을 알린다. 상태 머신이 redb 커밋 뒤에 부른다.
    pub(crate) fn publish(&self, events: Vec<WatchEvent>) {
        let mut h = self.history.lock().unwrap();
        for event in events {
            if h.events.len() == HISTORY {
                let old = h.events.pop_front().expect("history is full");
                h.earliest = old.revision();
            }
            h.events.push_back(event.clone());
            let _ = self.tx.send(event);
        }
    }

    /// 스냅샷 설치 뒤에 부른다. 보관한 변경을 버리고 `Reset`을 알린다.
    pub(crate) fn reset(&self, revision: u64) {
        let mut h = self.history.lock().unwrap();
        h.events.clear();
        h.earliest = revision;
        let _ = self.tx.send(WatchEvent::Reset { revision });
    }

    pub(crate) fn subscribe(&self, since: u64) -> Result<Watch, WatchError> {
        let h = self.history.lock().unwrap();
        if since < h.earliest {
            return Err(WatchError::Expired {
                since,
                earliest: h.earliest,
            });
        }
        let backlog = h
            .events
            .iter()
            .filter(|e| e.revision() > since)
            .cloned()
            .collect();
        // 잠금을 쥔 채 구독해야 backlog와 이후 변경 사이에 빈틈이 없다.
        Ok(Watch {
            backlog,
            rx: self.tx.subscribe(),
            seen: since,
        })
    }
}

/// 변경 구독. `next()`는 revision 순서대로, `since` 이후의 변경만 돌려준다.
pub struct Watch {
    backlog: VecDeque<WatchEvent>,
    rx: broadcast::Receiver<WatchEvent>,
    seen: u64,
}

impl Watch {
    pub async fn next(&mut self) -> Result<WatchEvent, WatchError> {
        if let Some(event) = self.backlog.pop_front() {
            self.seen = event.revision();
            return Ok(event);
        }
        loop {
            match self.rx.recv().await {
                Ok(event @ WatchEvent::Reset { .. }) => {
                    self.seen = event.revision();
                    return Ok(event);
                }
                // 목록을 읽은 직후 구독하면 그 목록에 이미 반영된 변경이 올 수 있다.
                Ok(event) if event.revision() <= self.seen => continue,
                Ok(event) => {
                    self.seen = event.revision();
                    return Ok(event);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => return Err(WatchError::Lagged),
                Err(broadcast::error::RecvError::Closed) => return Err(WatchError::Closed),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::resource::Resource;
    use serde_json::json;

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
        hub.publish(vec![put(1), put(2), put(3)]);
        let mut w = hub.subscribe(1).unwrap();
        assert_eq!(w.next().await.unwrap().revision(), 2);
        assert_eq!(w.next().await.unwrap().revision(), 3);
        hub.publish(vec![put(4)]);
        assert_eq!(w.next().await.unwrap().revision(), 4);
    }

    #[tokio::test]
    async fn skips_live_events_at_or_before_since() {
        let hub = WatchHub::new(0);
        let mut w = hub.subscribe(5).unwrap();
        hub.publish(vec![put(5), put(6)]);
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
        hub.publish((1..=HISTORY as u64 + 1).map(put).collect());
        assert!(matches!(
            hub.subscribe(0),
            Err(WatchError::Expired { earliest: 1, .. })
        ));
        assert!(hub.subscribe(1).is_ok());
    }

    #[tokio::test]
    async fn reset_clears_history_and_is_always_delivered() {
        let hub = WatchHub::new(0);
        hub.publish(vec![put(1), put(2)]);
        let mut w = hub.subscribe(2).unwrap();
        hub.reset(2);
        assert_eq!(w.next().await.unwrap(), WatchEvent::Reset { revision: 2 });
        assert!(matches!(hub.subscribe(1), Err(WatchError::Expired { .. })));
    }

    #[tokio::test]
    async fn slow_watcher_gets_lagged() {
        let hub = WatchHub::new(0);
        let mut w = hub.subscribe(0).unwrap();
        hub.publish((1..=HISTORY as u64 + 1).map(put).collect());
        assert_eq!(w.next().await, Err(WatchError::Lagged));
    }
}
