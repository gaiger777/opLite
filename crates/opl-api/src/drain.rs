//! 노드 비우기 API(설계 §3.1). 노드를 유지보수로 바꾸고 비우기 Task를 그 노드에 만든다. 옮기는 일은
//! 그 노드의 실행기가 한다(`tasks::drain`). 호스트 종료 정책 `migrate`도 같은 일을 한다([`start_drain`]).
//! 얼린 노드 풀기(`POST /nodes/{key}/unfreeze`)도 여기 있다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::lookup;
use crate::state::AppState;
use crate::tasks::create_task;

/// 노드를 유지보수로 바꾸고(이미면 그대로), 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션할
/// Task(`node-drain`, 그 노드에서 돈다)를 돌려준다(202). Task가 실패해도 노드는 유지보수로 남는다.
/// 위트니스이거나 오프라인이면 409 `node_unavailable`, 그 노드의 비우기 Task가 끝나지 않았으면 409
/// `node_busy`, 비우기를 모르는 노드가 남았으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/nodes/{key}/drain", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn drain_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let task = start_drain(&state, &key).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 노드 `key`(id나 이름)를 유지보수로 바꾸고 비우기 Task를 만든다. [`drain_node`]의 확인과 오류가 같다
/// (권한은 보지 않는다). 호스트 종료 정책 `migrate`가 자기 노드에 부른다.
pub async fn start_drain(state: &AppState, key: &str) -> Result<Task, ApiError> {
    let node = lookup::<NodeSpec>(&state.cluster, key)?;
    if node.spec.witness {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!("node {:?} is a witness node and runs no VMs", node.name),
        ));
    }
    if !node.status.online {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!(
                "node {:?} is offline; HA recovers the HA VMs of an offline node",
                node.name
            ),
        ));
    }
    // 이 프로세스에 함께 온 요청이 둘 다 검사를 지나 비우기 둘을 만들지 않게 한다. 다른 노드로 온
    // 요청과 겹쳐도 실행기가 노드의 줄에 세워 차례로 한다.
    static REQUESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REQUESTS.lock().await;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if tasks.iter().any(|t| {
        t.spec.op == TaskOp::NodeDrain && t.spec.node == node.id && !t.status.state.is_finished()
    }) {
        return Err(ApiError::conflict(
            "node_busy",
            format!("node {:?} is already being drained", node.name),
        ));
    }
    // 롤링 업그레이드 중(버전이 섞여도) 모든 노드가 비우기 Task를 알면 된다.
    crate::cluster::require_feature(&state.cluster, "node-drain")?;
    set_maintenance(state, key).await?;
    create_task(&state.cluster, TaskOp::NodeDrain, node.id, node.id).await
}

/// 얼린 노드(호스트 종료 정책 `freeze`로 HA VM을 끄고 꺼진 노드)의 표시를 지워, 리더가 평소대로 그 노드의
/// HA VM을 다른 노드로 옮기게 한다. 돌아오지 않는 노드에 쓴다. 표시가 없으면 그대로 돌려준다. 온라인이면
/// 409 `node_online`(돌아온 노드는 등록하며 스스로 지운다).
#[utoipa::path(post, path = "/nodes/{key}/unfreeze", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 200, body = Resource<NodeSpec, opl_types::node::NodeStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn unfreeze_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Node>, ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    for _ in 0..10 {
        let node = lookup::<NodeSpec>(&state.cluster, &key)?;
        if node.status.online {
            return Err(ApiError::conflict(
                "node_online",
                format!(
                    "node {:?} is online; it clears its shutdown mark itself",
                    node.name
                ),
            ));
        }
        let Some(mark) = node.status.shutdown else {
            return Ok(Json(node));
        };
        let mut status = node.status.clone();
        status.shutdown = None;
        match state
            .cluster
            .update_status_checked::<NodeSpec>(node.id, node.revision, &status)
            .await
        {
            Ok(_) => {
                tracing::warn!(node = %node.name, action = ?mark.action, "shutdown mark cleared; HA recovers the node's VMs");
                return Ok(Json(lookup::<NodeSpec>(&state.cluster, &key)?));
            }
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the node kept changing while being updated; try again",
    ))
}

/// 노드의 `maintenance`를 참으로 쓴다(revision 확인). 다른 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_maintenance(state: &AppState, key: &str) -> Result<(), ApiError> {
    for _ in 0..10 {
        let node = lookup::<NodeSpec>(&state.cluster, key)?;
        if node.spec.maintenance {
            return Ok(());
        }
        let spec = NodeSpec {
            maintenance: true,
            ..node.spec
        };
        match state
            .cluster
            .update_spec(node.id, Some(node.revision), node.labels, &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the node kept changing while being updated; try again",
    ))
}
