//! 리더가 처리하는 요청(쓰기, 멤버십 변경)과, 팔로워가 그것을 리더에 전달하는 통로.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use openraft::error::{ChangeMembershipError, ClientWriteError, RaftError};
use openraft::raft::ClientWriteResponse;
use openraft::{ChangeMembers, Raft, RaftMetrics};
use opl_types::cluster::ClusterStatus;
use opl_types::resource::Kind;
use redb::Database;
use serde::{Deserialize, Serialize};

use crate::cluster::ClusterError;
use crate::lease::{ConfirmBatch, LeaseTable};
use crate::store::list_objects;
use crate::types::{Command, LeaderRequest, NodeId, OrbNode, Rejection, Response, TypeConfig};
use crate::vip::VipTable;

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

/// 리더로서 요청을 처리하는 데 쓰는 것: Raft, 리스 표(HA·VIP)와 그 둘이 함께 쓰는 리더 확인
#[derive(Clone)]
pub(crate) struct LeaderCtx {
    pub(crate) raft: Raft<TypeConfig>,
    pub(crate) confirm: ConfirmBatch,
    /// 이 노드의 저장소(클러스터 정보의 관찰 노드 목록을 읽는다)
    pub(crate) db: Arc<Database>,
    pub(crate) leases: Arc<LeaseTable>,
    pub(crate) vips: Arc<VipTable>,
}

/// 이 노드가 리더라고 보고 요청을 처리한다. 리더가 아니면 `NotLeader`다. 성공하면 커밋된 로그 인덱스
/// (리스 갱신은 리더가 적용한 마지막 인덱스).
pub(crate) async fn execute(
    ctx: &LeaderCtx,
    req: LeaderRequest,
    timeout: Duration,
) -> Result<u64, ClusterError> {
    let raft = &ctx.raft;
    match req {
        LeaderRequest::RenewLease { node } => {
            ctx.leases.renew(raft, &ctx.confirm, node, timeout).await
        }
        LeaderRequest::RenewVip { node, vips } => {
            ctx.vips
                .renew(raft, &ctx.confirm, node, vips, timeout)
                .await
        }
        LeaderRequest::Write { command } => {
            let resp = tokio::time::timeout(timeout, raft.client_write(command))
                .await
                .map_err(|_| ClusterError::Timeout)??;
            let Response(result) = resp.data;
            Ok(result?)
        }
        LeaderRequest::AddLearner { id, addr, addrs } => {
            let node = learner_node(addr, addrs);
            retry_in_progress(timeout, || raft.add_learner(id, node.clone(), false)).await
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
            // learner가 아니면(빠졌거나 더한 적이 없으면) 따라잡기를 기다리지 않고 바로 거부한다. 관찰 노드 목록에
            // 있는 learner(관찰 노드로 내려가는 중)도 올리지 않는다(내려가며 로그를 지우는 노드가 투표 멤버가 되면 안
            // 된다).
            if membership.get_node(&id).is_none() || observers(&ctx.db)?.contains_key(&id) {
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
            // 기다리는 사이 키퍼가 이 노드를 관찰 노드 목록에 적었을 수 있다(내려가며 로그를 지운다). 바로 앞에서 다시 본다.
            if observers(&ctx.db)?.contains_key(&id) {
                return Err(ClusterError::Rejected(Rejection::NotFound {
                    kind: Kind::Node,
                    id: id.0,
                }));
            }
            let change = ChangeMembers::AddVoterIds(BTreeSet::from([id]));
            retry_in_progress(timeout, || raft.change_membership(change.clone(), false)).await
        }
        LeaderRequest::Remove { id } => {
            let m = raft.metrics().borrow().clone();
            if m.state != openraft::ServerState::Leader {
                return Err(not_leader(&m));
            }
            let deadline = tokio::time::Instant::now() + timeout;
            let index = remove_member(raft, &m, id, deadline).await?;
            // 관찰 노드 목록에서도 뺀다(멤버십에서 먼저 빼야 키퍼가 목록에서만 빠진 learner를 보지 않는다).
            let unlisted = unlist_observer(ctx, id, deadline).await?;
            Ok(unlisted
                .or(index)
                .unwrap_or_else(|| m.last_applied.map_or(0, |l| l.index)))
        }
    }
}

/// 노드를 멤버십에서 뺀다(learner로 남기지 않는다). 멤버십에 없으면 `None`이다.
async fn remove_member(
    raft: &Raft<TypeConfig>,
    m: &RaftMetrics<NodeId, OrbNode>,
    id: NodeId,
    deadline: tokio::time::Instant,
) -> Result<Option<u64>, ClusterError> {
    let membership = m.membership_config.membership();
    let change = if membership.voter_ids().any(|v| v == id) {
        ChangeMembers::RemoveVoters(BTreeSet::from([id]))
    } else if membership.learner_ids().any(|v| v == id) {
        ChangeMembers::RemoveNodes(BTreeSet::from([id]))
    } else {
        return Ok(None);
    };
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let index =
        retry_in_progress(remaining, || raft.change_membership(change.clone(), false)).await?;
    // 두 단계 변경의 두 번째 단계보다 VoterKeeper가 joint를 먼저 마무리했으면(learner로 남긴다) 두 번째
    // 단계는 할 일이 없어 이 노드가 learner로 남는다. 다시 읽어 지운다.
    let m = raft.metrics().borrow().clone();
    if !m
        .membership_config
        .membership()
        .learner_ids()
        .any(|v| v == id)
    {
        return Ok(Some(index));
    }
    let change = ChangeMembers::RemoveNodes(BTreeSet::from([id]));
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    retry_in_progress(remaining, || raft.change_membership(change.clone(), false))
        .await
        .map(Some)
}

/// 이 노드의 저장소에 있는 클러스터 정보와 그 관찰 노드 목록(클러스터 정보가 없으면 `None`)
fn cluster_info(
    db: &Database,
) -> Result<Option<(crate::types::RawResource, ClusterStatus)>, ClusterError> {
    let Some(info) = list_objects(db, Kind::Cluster)?.1.into_iter().next() else {
        return Ok(None);
    };
    let status = serde_json::from_value(info.status.clone())?;
    Ok(Some((info, status)))
}

/// 클러스터 정보의 관찰 노드 목록
fn observers(db: &Database) -> Result<std::collections::BTreeMap<NodeId, String>, ClusterError> {
    Ok(cluster_info(db)?
        .map(|(_, status)| {
            status
                .observers
                .into_iter()
                .map(|(id, addr)| (NodeId(id), addr))
                .collect()
        })
        .unwrap_or_default())
}

/// 클러스터 정보의 관찰 노드 목록에서 `id`를 뺀다. 목록에 없으면 `None`, 뺐으면 그 쓰기의 인덱스다. 다른 쓰기와
/// 겹치면(revision 불일치, 리더가 아직 적용하지 않은 쓰기) 다시 읽어 `deadline`까지 다시 한다.
async fn unlist_observer(
    ctx: &LeaderCtx,
    id: NodeId,
    deadline: tokio::time::Instant,
) -> Result<Option<u64>, ClusterError> {
    loop {
        let Some((info, mut status)) = cluster_info(&ctx.db)? else {
            return Ok(None);
        };
        if status.observers.remove(&id.0).is_none() {
            return Ok(None);
        }
        let command = Command::UpdateStatusIf {
            kind: Kind::Cluster,
            id: info.id,
            expected_revision: info.revision,
            status: serde_json::to_value(&status)?,
        };
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let resp = tokio::time::timeout(remaining, ctx.raft.client_write(command))
            .await
            .map_err(|_| ClusterError::Timeout)??;
        match resp.data.0 {
            Ok(index) => return Ok(Some(index)),
            Err(Rejection::RevisionMismatch { .. })
                if tokio::time::Instant::now() + IN_PROGRESS_RETRY_INTERVAL < deadline =>
            {
                tokio::time::sleep(IN_PROGRESS_RETRY_INTERVAL).await;
            }
            Err(rejection) => return Err(rejection.into()),
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
            RaftError<NodeId, ClientWriteError<NodeId, OrbNode>>,
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
pub(crate) fn not_leader(m: &RaftMetrics<NodeId, OrbNode>) -> ClusterError {
    let leader_addr = m.current_leader.and_then(|l| {
        m.membership_config
            .membership()
            .get_node(&l)
            .map(|n| n.addr().to_string())
    });
    ClusterError::NotLeader {
        leader_id: m.current_leader,
        leader_addr,
    }
}

/// `AddLearner`의 노드 값: 링크 목록이 있으면 그것, 없으면(링크 하나, 옛 노드) `addr` 하나
fn learner_node(addr: String, addrs: Vec<String>) -> OrbNode {
    if addrs.is_empty() {
        OrbNode::single(addr)
    } else {
        OrbNode::new(addrs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learner_node_uses_every_link() {
        assert_eq!(
            learner_node("a:8007".into(), vec![]),
            OrbNode::single("a:8007")
        );
        let links = vec!["a:8007".to_string(), "b:8007".to_string()];
        assert_eq!(
            learner_node("a:8007".into(), links.clone()),
            OrbNode::new(links)
        );
    }
}
