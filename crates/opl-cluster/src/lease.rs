//! 리더의 HA 리스 표. 노드가 보낸 갱신(`RenewLease`)을 받은 시각을 임기별로 메모리에 둔다.
//! 안전 조건은 설계 문서(2026-11-02-ha-fencing-witness-design.md) §3.2·§4.2를 따른다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Duration;

use openraft::error::{CheckIsLeaderError, RaftError};
use openraft::{LogId, Raft, ServerState};
use tokio::time::Instant;

use crate::cluster::ClusterError;
use crate::leader::not_leader;
use crate::types::{NodeId, Rejection, TypeConfig};

/// 리더가 아는 리스. `renewed`는 `term` 임기에 이 리더가 확인해 준 노드별 마지막 갱신 시각이다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leases {
    pub term: u64,
    pub renewed: BTreeMap<NodeId, Instant>,
}

#[derive(Default)]
struct Inner {
    /// `renewed`가 속한 임기. 더 높은 임기를 보면 `renewed`를 비운다.
    term: u64,
    renewed: BTreeMap<NodeId, Instant>,
    /// 펜싱 중인 노드. 이 노드의 갱신은 `Fenced`로 거부한다. 임기가 바뀌어도 지우지 않는다.
    /// 지우면 이전 임기에 시작한 복구 쓰기가 끝나기 전에 그 노드가 리스를 받을 수 있다.
    fencing: BTreeSet<NodeId>,
}

impl Inner {
    /// 임기가 바뀌었으면 갱신 기록을 비운다.
    fn enter(&mut self, term: u64) {
        if term > self.term {
            self.term = term;
            self.renewed.clear();
        }
    }
}

#[derive(Default)]
pub(crate) struct LeaseTable {
    inner: Mutex<Inner>,
}

impl LeaseTable {
    /// 리더로서 `node`의 갱신을 처리한다. 성공하면 리더가 적용한 마지막 로그 인덱스를 돌려준다.
    pub(crate) async fn renew(
        &self,
        raft: &Raft<TypeConfig>,
        node: NodeId,
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
        // 아직 과반수의 리더인지 확인한다. 노드는 보내기 전에 시각을 쟀으므로 이 확인 뒤에 적는
        // 시각은 그보다 늦다. 새 임기의 과반수가 투표한 뒤에는 이 확인이 성공할 수 없다.
        confirm_leader(raft, deadline).await?;
        {
            let mut inner = self.inner.lock().unwrap();
            let m = raft.metrics().borrow().clone();
            // 확인하는 사이 임기가 바뀌었으면 그 임기의 표에 적지 않는다.
            if m.state != ServerState::Leader || m.current_term != term {
                return Err(not_leader(&m));
            }
            // 펜싱 검사와 기록은 한 잠금 안에서 한다(`Cluster::begin_fence` 참고).
            if inner.fencing.contains(&node) {
                return Err(ClusterError::Rejected(Rejection::Fenced { node }));
            }
            inner.enter(term);
            inner.renewed.insert(node, Instant::now());
        }
        // 돌려주는 인덱스는 펜싱 검사 뒤에 다시 확인한 커밋 인덱스다. 펜싱 표시를 지우기 전에 끝난
        // 복구 쓰기가 모두 들어 있다. 첫 확인의 인덱스는 그보다 앞설 수 있고, 지표(metrics)의 적용
        // 인덱스는 쓰기 응답보다 늦게 바뀔 수 있어 쓰지 않는다.
        let read = confirm_leader(raft, deadline).await?;
        Ok(read.map_or(0, |l| l.index))
    }

    /// 지금 이 노드의 임기 기준의 리스. 표가 더 낮은 임기의 것이면 비운다.
    pub(crate) fn leases(&self, raft: &Raft<TypeConfig>) -> Leases {
        let mut inner = self.inner.lock().unwrap();
        let term = raft.metrics().borrow().current_term;
        inner.enter(term);
        Leases {
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

/// 과반수에게 아직 리더인지 확인하고, 그 시점의 커밋 인덱스까지 적용되기를 기다린다.
/// `deadline`까지 끝나지 않으면 `Timeout`이다.
async fn confirm_leader(
    raft: &Raft<TypeConfig>,
    deadline: Instant,
) -> Result<Option<LogId<NodeId>>, ClusterError> {
    match tokio::time::timeout_at(deadline, raft.ensure_linearizable()).await {
        Err(_) => Err(ClusterError::Timeout),
        Ok(Err(RaftError::APIError(CheckIsLeaderError::ForwardToLeader(f)))) => {
            Err(ClusterError::NotLeader {
                leader_id: f.leader_id,
                leader_addr: f.leader_node.map(|n| n.addr),
            })
        }
        Ok(Err(e)) => Err(ClusterError::Raft(e.to_string())),
        Ok(Ok(read)) => Ok(read),
    }
}
