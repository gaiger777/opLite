//! 리더가 처리하는 요청(쓰기, 멤버십 변경)과, 팔로워가 그것을 리더에 전달하는 통로.

use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use openraft::{BasicNode, ChangeMembers, Raft};
use serde::{Deserialize, Serialize};

use crate::cluster::ClusterError;
use crate::types::{LeaderRequest, NodeId, Rejection, Response, TypeConfig};

/// learner가 로그를 따라잡기를 기다리는 최대 시간. 스냅샷을 받아야 하면 오래 걸린다.
pub const CATCH_UP_TIMEOUT: Duration = Duration::from_secs(60);

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
            let resp =
                tokio::time::timeout(timeout, raft.add_learner(id, BasicNode::new(addr), false))
                    .await
                    .map_err(|_| ClusterError::Timeout)??;
            Ok(resp.log_id.index)
        }
        LeaderRequest::AddVoter { id } => {
            let m = raft.metrics().borrow().clone();
            let membership = m.membership_config.membership();
            if membership.voter_ids().any(|v| v == id) {
                return Ok(m.last_applied.map_or(0, |l| l.index));
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
            let resp = tokio::time::timeout(timeout, raft.change_membership(change, false))
                .await
                .map_err(|_| ClusterError::Timeout)??;
            Ok(resp.log_id.index)
        }
        LeaderRequest::Remove { id } => {
            let m = raft.metrics().borrow().clone();
            let membership = m.membership_config.membership();
            let change = if membership.voter_ids().any(|v| v == id) {
                ChangeMembers::RemoveVoters(BTreeSet::from([id]))
            } else if membership.learner_ids().any(|v| v == id) {
                ChangeMembers::RemoveNodes(BTreeSet::from([id]))
            } else {
                return Ok(m.last_applied.map_or(0, |l| l.index));
            };
            let resp = tokio::time::timeout(timeout, raft.change_membership(change, false))
                .await
                .map_err(|_| ClusterError::Timeout)??;
            Ok(resp.log_id.index)
        }
    }
}
