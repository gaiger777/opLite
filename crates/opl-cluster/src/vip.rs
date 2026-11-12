//! 리더의 VIP 리스 표(설계 §4 내장 VIP). HA 리스 표(`lease.rs`)와 같은 규칙으로, 노드가 보낸 갱신
//! (`RenewVip`)을 받은 시각과 그 노드가 가질 수 있는 VIP를 임기별로 메모리에 둔다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Duration;

use openraft::{Raft, ServerState};
use tokio::time::Instant;

use crate::cluster::ClusterError;
use crate::leader::not_leader;
use crate::lease::ConfirmBatch;
use crate::types::{NodeId, Rejection, TypeConfig};

/// 노드 하나의 마지막 VIP 갱신
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VipRenewal {
    /// 이 리더가 확인해 준 시각
    pub at: Instant,
    /// 그 노드가 가질 수 있는 VIP 주소(IP)
    pub vips: BTreeSet<String>,
}

/// 리더가 아는 VIP 리스. `renewed`는 `term` 임기에 이 리더가 확인해 준 노드별 마지막 갱신이다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VipLeases {
    pub term: u64,
    pub renewed: BTreeMap<NodeId, VipRenewal>,
}

#[derive(Default)]
struct Inner {
    term: u64,
    renewed: BTreeMap<NodeId, VipRenewal>,
    /// VIP를 다른 노드로 옮기는 중인 옛 주인. 이 노드의 갱신은 `Fenced`로 거부한다. 옮기는 쓰기의 결과를
    /// 안 뒤에 풀면 그 뒤의 갱신이 돌려주는 인덱스에 그 쓰기가 들어 있다.
    fencing: BTreeSet<NodeId>,
}

#[derive(Default)]
pub(crate) struct VipTable {
    inner: Mutex<Inner>,
}

impl VipTable {
    /// 리더로서 `node`의 VIP 갱신을 처리한다. 성공하면 리더가 적용한 마지막 로그 인덱스를 돌려준다.
    /// 확인 순서와 이유는 `LeaseTable::renew`와 같다.
    pub(crate) async fn renew(
        &self,
        raft: &Raft<TypeConfig>,
        confirm: &ConfirmBatch,
        node: NodeId,
        vips: Vec<String>,
        timeout: Duration,
    ) -> Result<u64, ClusterError> {
        let term = {
            let m = raft.metrics().borrow().clone();
            if m.state != ServerState::Leader {
                return Err(not_leader(&m));
            }
            m.current_term
        };
        let deadline = Instant::now() + timeout;
        confirm.confirm(deadline).await?;
        {
            let mut inner = self.inner.lock().unwrap();
            let m = raft.metrics().borrow().clone();
            if m.state != ServerState::Leader || m.current_term != term {
                return Err(not_leader(&m));
            }
            if inner.fencing.contains(&node) {
                return Err(ClusterError::Rejected(Rejection::Fenced { node }));
            }
            if term > inner.term {
                inner.term = term;
                inner.renewed.clear();
            }
            inner.renewed.insert(
                node,
                VipRenewal {
                    at: Instant::now(),
                    vips: vips.into_iter().collect(),
                },
            );
        }
        let read = confirm.confirm(deadline).await?;
        Ok(read.map_or(0, |l| l.index))
    }

    /// 지금 이 노드의 임기 기준의 VIP 리스. 표가 더 낮은 임기의 것이면 비운다.
    pub(crate) fn leases(&self, raft: &Raft<TypeConfig>) -> VipLeases {
        let mut inner = self.inner.lock().unwrap();
        let term = raft.metrics().borrow().current_term;
        if term > inner.term {
            inner.term = term;
            inner.renewed.clear();
        }
        VipLeases {
            term,
            renewed: inner.renewed.clone(),
        }
    }

    pub(crate) fn begin_fence(&self, node: NodeId) {
        self.inner.lock().unwrap().fencing.insert(node);
    }

    pub(crate) fn end_fence(&self, node: NodeId) {
        self.inner.lock().unwrap().fencing.remove(&node);
    }
}
