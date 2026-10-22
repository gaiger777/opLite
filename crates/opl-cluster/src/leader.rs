//! 리더가 처리하는 요청(쓰기, 멤버십 변경)과, 팔로워가 그것을 리더에 전달하는 통로.

use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use openraft::error::{ChangeMembershipError, ClientWriteError, RaftError};
use openraft::raft::ClientWriteResponse;
use openraft::{BasicNode, ChangeMembers, Raft, RaftMetrics};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};

use crate::cluster::ClusterError;
use crate::types::{LeaderRequest, NodeId, Rejection, Response, TypeConfig};

/// learner가 로그를 따라잡기를 기다리는 최대 시간. 스냅샷을 받아야 하면 오래 걸린다.
pub const CATCH_UP_TIMEOUT: Duration = Duration::from_secs(60);

/// 앞선 멤버십 변경이 아직 커밋 중일 때 다시 시도하는 간격
const IN_PROGRESS_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// 팔로워가 받은 요청을 리더에게 보낸다. 노드 간 네트워크가 구현한다.
#[async_trait]
pub trait Forwarder: Send + Sync + 'static {
    /// `addr`(리더의 Raft 주소)의 리더에게 요청을 보내고 리더의 결과를 돌려준다. 리더가 그 사이
    /// 바뀌었으면 `NotLeader`, 리더에 닿지 못했으면 `Unreachable`이다.
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError>;
}

/// 노드 사이로 오가는 리더의 오류
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "kebab-case")]
pub enum LeaderError {
    Rejected {
        rejection: Rejection,
    },
    NotLeader {
        leader_id: Option<NodeId>,
        leader_addr: Option<String>,
    },
    Timeout,
    Other {
        message: String,
    },
}

impl From<ClusterError> for LeaderError {
    fn from(e: ClusterError) -> Self {
        match e {
            ClusterError::Rejected(rejection) => LeaderError::Rejected { rejection },
            ClusterError::NotLeader {
                leader_id,
                leader_addr,
            } => LeaderError::NotLeader {
                leader_id,
                leader_addr,
            },
            ClusterError::Timeout => LeaderError::Timeout,
            other => LeaderError::Other {
                message: other.to_string(),
            },
        }
    }
}

impl From<LeaderError> for ClusterError {
    fn from(e: LeaderError) -> Self {
        match e {
            LeaderError::Rejected { rejection } => ClusterError::Rejected(rejection),
            LeaderError::NotLeader {
                leader_id,
                leader_addr,
            } => ClusterError::NotLeader {
                leader_id,
                leader_addr,
            },
            LeaderError::Timeout => ClusterError::Timeout,
            LeaderError::Other { message } => ClusterError::Raft(message),
        }
    }
}

/// 이 노드가 리더라고 보고 요청을 처리한다. 리더가 아니면 `NotLeader`다. 성공하면 커밋된 로그 인덱스.
pub(crate) async fn execute(
    raft: &Raft<TypeConfig>,
    req: LeaderRequest,
    timeout: Duration,
) -> Result<u64, ClusterError> {
    match req {
        LeaderRequest::Write { command } => {
            let resp = tokio::time::timeout(timeout, raft.client_write(command))
                .await
                .map_err(|_| ClusterError::Timeout)??;
            let Response(result) = resp.data;
            Ok(result?)
        }
        LeaderRequest::AddLearner { id, addr } => {
            retry_in_progress(timeout, || {
                raft.add_learner(id, BasicNode::new(addr.clone()), false)
            })
            .await
        }
        LeaderRequest::AddVoter { id } => {
            let m = raft.metrics().borrow().clone();
            // 아래 지름길은 리더의 멤버십으로만 판단한다. 뒤처진 팔로워가 답하면 안 된다.
            if m.state != openraft::ServerState::Leader {
                return Err(not_leader(&m));
            }
            let membership = m.membership_config.membership();
            if membership.voter_ids().any(|v| v == id) {
                return Ok(m.last_applied.map_or(0, |l| l.index));
            }
            // learner가 아니면(빠졌거나 더한 적이 없으면) 따라잡기를 기다리지 않고 바로 거부한다.
            if membership.get_node(&id).is_none() {
                return Err(ClusterError::Rejected(Rejection::NotFound {
                    kind: Kind::Node,
                    id: id.0,
                }));
            }
            // learner가 지금 리더의 마지막 로그까지 받으면 올린다.
            let target = m.last_log_index;
            raft.wait(Some(CATCH_UP_TIMEOUT))
                .metrics(
                    |m| {
                        m.state != openraft::ServerState::Leader
                            || m.replication.as_ref().is_some_and(|r| {
                                r.get(&id).is_some_and(|l| l.map(|l| l.index) >= target)
                            })
                    },
                    "learner caught up",
                )
                .await
                .map_err(|_| ClusterError::Timeout)?;
            let change = ChangeMembers::AddVoterIds(BTreeSet::from([id]));
            retry_in_progress(timeout, || raft.change_membership(change.clone(), false)).await
        }
        LeaderRequest::Remove { id } => {
            let m = raft.metrics().borrow().clone();
            if m.state != openraft::ServerState::Leader {
                return Err(not_leader(&m));
            }
            let membership = m.membership_config.membership();
            let change = if membership.voter_ids().any(|v| v == id) {
                ChangeMembers::RemoveVoters(BTreeSet::from([id]))
            } else if membership.learner_ids().any(|v| v == id) {
                ChangeMembers::RemoveNodes(BTreeSet::from([id]))
            } else {
                return Ok(m.last_applied.map_or(0, |l| l.index));
            };
            retry_in_progress(timeout, || raft.change_membership(change.clone(), false)).await
        }
    }
}

/// 멤버십을 바꾼다. 앞선 멤버십 변경이 아직 커밋 중이면(`InProgress`, 예: 다른 노드의 승격이
/// 두 단계 중 하나를 커밋하는 중) `timeout`까지 다시 시도하고, 그래도 안 되면 `Timeout`이다.
async fn retry_in_progress<F, Fut>(timeout: Duration, mut change: F) -> Result<u64, ClusterError>
where
    F: FnMut() -> Fut,
    Fut: Future<
        Output = Result<
            ClientWriteResponse<TypeConfig>,
            RaftError<NodeId, ClientWriteError<NodeId, BasicNode>>,
        >,
    >,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, change())
            .await
            .map_err(|_| ClusterError::Timeout)?
        {
            Err(RaftError::APIError(ClientWriteError::ChangeMembershipError(
                ChangeMembershipError::InProgress(_),
            ))) => {
                if tokio::time::Instant::now() + IN_PROGRESS_RETRY_INTERVAL >= deadline {
                    return Err(ClusterError::Timeout);
                }
                tokio::time::sleep(IN_PROGRESS_RETRY_INTERVAL).await;
            }
            other => return Ok(other?.log_id.index),
        }
    }
}

/// 이 노드가 아는 리더와 그 주소로 `NotLeader`를 만든다. openraft의 `change_membership`은
/// 리더인지 보기 전에 변경을 검사하므로, 팔로워에서는 `ForwardToLeader` 대신 검사 오류가 날 수 있다.
fn not_leader(m: &RaftMetrics<NodeId, BasicNode>) -> ClusterError {
    let leader_addr = m.current_leader.and_then(|l| {
        m.membership_config
            .membership()
            .get_node(&l)
            .map(|n| n.addr.clone())
    });
    ClusterError::NotLeader {
        leader_id: m.current_leader,
        leader_addr,
    }
}
