//! 노드 전원 끄기 펜싱 설정 `/nodes/{key}/fence`와 `fence-test`(설계 §8.1). admin만 읽고 바꾼다. BMC 비밀번호는
//! 응답에 넣지 않고 `has_password`만 보이며, watch로도 내보내지 않는다(영역과 같다).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::types::NodeId;
use opl_types::fence::{
    CommandFence, FenceMethod, FenceMethodStatus, IpmiFence, NodeFence, NodeFenceSpec,
    RedfishFence, neighbour,
};
use opl_types::node::{FAST_HA, Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::power_fence::FENCE_CALL_TIMEOUT;
use crate::resources::{if_match, lookup};
use crate::state::AppState;

/// 이웃이 방법을 모두 읽기까지 기다리는 최대 시간(방법들은 함께 읽는다)
const NEIGHBOUR_TIMEOUT: Duration = Duration::from_secs(FENCE_CALL_TIMEOUT.as_secs() + 5);

/// 응답의 펜싱 설정. 비밀번호 대신 있는지만 보인다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct NodeFenceView {
    /// 노드 이름
    pub node: String,
    pub revision: u64,
    pub methods: Vec<FenceMethodView>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FenceMethodView {
    Ipmi(IpmiFenceView),
    Redfish(RedfishFenceView),
    Command(CommandFence),
}

/// [`IpmiFence`]에서 `password`를 뺀 것
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct IpmiFenceView {
    pub address: String,
    pub port: u16,
    pub user: String,
    pub lanplus: bool,
    /// 비밀번호가 저장되어 있는지
    pub has_password: bool,
}

/// [`RedfishFence`]에서 `password`를 뺀 것
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RedfishFenceView {
    pub url: String,
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub system_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub ca_pem: Option<String>,
    pub insecure: bool,
    /// 비밀번호가 저장되어 있는지
    pub has_password: bool,
}

impl From<NodeFence> for NodeFenceView {
    fn from(f: NodeFence) -> Self {
        let methods = f
            .spec
            .methods
            .into_iter()
            .map(|m| match m {
                FenceMethod::Ipmi(i) => FenceMethodView::Ipmi(IpmiFenceView {
                    has_password: i.password.is_some(),
                    address: i.address,
                    port: i.port,
                    user: i.user,
                    lanplus: i.lanplus,
                }),
                FenceMethod::Redfish(r) => FenceMethodView::Redfish(RedfishFenceView {
                    has_password: r.password.is_some(),
                    url: r.url,
                    user: r.user,
                    system_id: r.system_id,
                    ca_pem: r.ca_pem,
                    insecure: r.insecure,
                }),
                FenceMethod::Command(c) => FenceMethodView::Command(c),
            })
            .collect();
        Self {
            node: f.name,
            revision: f.revision,
            methods,
        }
    }
}

/// 비밀번호를 보낼 곳이 같은지(같으면 빠진 비밀번호에 저장된 값을 쓴다)
fn same_target(new: &FenceMethod, old: &FenceMethod) -> bool {
    match (new, old) {
        (FenceMethod::Ipmi(n), FenceMethod::Ipmi(o)) => {
            n.address == o.address && n.port == o.port && n.user == o.user
        }
        (FenceMethod::Redfish(n), FenceMethod::Redfish(o)) => {
            n.url == o.url && n.user == o.user && n.ca_pem == o.ca_pem && n.insecure == o.insecure
        }
        _ => false,
    }
}

fn password_mut(m: &mut FenceMethod) -> Option<&mut Option<String>> {
    match m {
        FenceMethod::Ipmi(IpmiFence { password, .. })
        | FenceMethod::Redfish(RedfishFence { password, .. }) => Some(password),
        FenceMethod::Command(_) => None,
    }
}

fn password_of(m: &FenceMethod) -> Option<&String> {
    match m {
        FenceMethod::Ipmi(i) => i.password.as_ref(),
        FenceMethod::Redfish(r) => r.password.as_ref(),
        FenceMethod::Command(_) => None,
    }
}

/// 요청을 저장할 spec으로. 빈 비밀번호는 지우고, 빠진 비밀번호는 같은 종류·주소·사용자(Redfish는 CA·`insecure`까지)의
/// 저장된 방법에서 가져온다. 그런 방법이 없는데 같은 자리의 같은 종류 방법에 비밀번호가 있었으면(주소나 사용자를
/// 바꿈) 400이다 — 저장된 비밀번호를 다른 BMC로 보내지 않는다.
fn spec_of(
    mut spec: NodeFenceSpec,
    stored: Option<&NodeFenceSpec>,
) -> Result<NodeFenceSpec, ApiError> {
    for (i, method) in spec.methods.iter_mut().enumerate() {
        if let FenceMethod::Redfish(r) = method {
            r.url = r.url.trim().to_string();
            r.ca_pem = r
                .ca_pem
                .take()
                .map(|pem| pem.replace("\r\n", "\n").trim().to_string());
        }
        let kept = stored.and_then(|s| {
            s.methods
                .iter()
                .find(|old| same_target(method, old))
                .map(|old| password_of(old).cloned())
        });
        let changed = stored
            .and_then(|s| s.methods.get(i))
            .is_some_and(|old| old.kind() == method.kind() && password_of(old).is_some());
        let Some(password) = password_mut(method) else {
            continue;
        };
        *password = match password.take() {
            Some(p) if p.is_empty() => None,
            Some(p) => Some(p),
            None => match kept {
                Some(kept) => kept,
                None if changed => {
                    return Err(ApiError::bad_request(format!(
                        "method {}: password must be given again when the address, port or user changes (redfish: url, user, ca_pem or insecure)",
                        i + 1
                    )));
                }
                None => None,
            },
        };
    }
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(spec)
}

/// 펜싱을 둘 수 있는 노드(위트니스는 VM을 돌리지 않으므로 400)
fn fenceable(state: &AppState, key: &str) -> Result<Node, ApiError> {
    let node = lookup::<NodeSpec>(&state.cluster, key)?;
    if node.spec.witness {
        return Err(ApiError::bad_request(format!(
            "node {:?} is a witness; it runs no VMs and needs no power fencing",
            node.name
        )));
    }
    Ok(node)
}

fn stored(state: &AppState, node: &Node) -> Result<Option<NodeFence>, ApiError> {
    Ok(state.cluster.get::<NodeFenceSpec>(node.id)?)
}

fn not_set(node: &Node) -> ApiError {
    ApiError::not_found(format!("node {:?} has no power fencing", node.name))
}

/// 노드의 전원 끄기 펜싱 설정. 없으면 404.
#[utoipa::path(get, path = "/nodes/{key}/fence", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 200, body = NodeFenceView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_node_fence(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<NodeFenceView>, ApiError> {
    state.authorize(&principal, Action::Read, NodeFenceSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    let fence = stored(&state, &node)?.ok_or_else(|| not_set(&node))?;
    Ok(Json(fence.into()))
}

/// 노드의 전원 끄기 펜싱 방법(1~4개, 차례로 해 본다)을 통째로 둔다. 비밀번호(`password`)를 빼면 같은 종류·주소·
/// 사용자의 저장된 값을 두고(주소나 사용자를 바꾸면 다시 줘야 한다 — 400), 빈 글이면 지운다. 위트니스는 400, 모든
/// 노드가 `fast-ha`를 알아야 한다(409 `cluster_upgrading`).
#[utoipa::path(put, path = "/nodes/{key}/fence", tag = "nodes",
    params(("key" = String, Path, description = "node id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = NodeFenceSpec,
    responses((status = 200, body = NodeFenceView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn put_node_fence(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<NodeFenceSpec>,
) -> Result<Json<NodeFenceView>, ApiError> {
    state.authorize(&principal, Action::Write, NodeFenceSpec::KIND)?;
    let expected = if_match(&headers)?;
    let node = fenceable(&state, &key)?;
    let old = stored(&state, &node)?;
    let spec = spec_of(req, old.as_ref().map(|f| &f.spec))?;
    // 옛 노드는 이 종류의 로그 항목을 풀지 못한다.
    require_feature(&state.cluster, FAST_HA)?;
    match old {
        Some(old) => {
            if spec != old.spec {
                state
                    .cluster
                    .update_spec(old.id, expected, old.labels, &spec)
                    .await?;
            }
        }
        None => {
            let mut fence: NodeFence = Resource::new(node.name.clone(), spec);
            fence.id = node.id;
            state.cluster.create(&fence).await?;
        }
    }
    let fence = stored(&state, &node)?.ok_or_else(|| not_set(&node))?;
    Ok(Json(fence.into()))
}

/// 노드의 전원 끄기 펜싱을 지운다(리더는 watchdog만 기다린다). 없으면 404.
#[utoipa::path(delete, path = "/nodes/{key}/fence", tag = "nodes",
    params(("key" = String, Path, description = "node id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_node_fence(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, NodeFenceSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if stored(&state, &node)?.is_none() {
        return Err(not_set(&node));
    }
    require_feature(&state.cluster, FAST_HA)?;
    state
        .cluster
        .delete::<NodeFenceSpec>(node.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `fence-test`의 결과
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FenceTestResult {
    /// 전원을 읽은 노드의 이름(시험하는 노드 자신이면 이웃)
    pub checked_by: String,
    /// 방법마다의 결과(방법 순서)
    pub methods: Vec<FenceMethodStatus>,
}

/// 노드의 펜싱 방법마다 전원을 읽는다(전원은 건드리지 않는다). 이 API 노드가 그 노드가 아니면 스스로 읽고, 그
/// 노드면 이웃(온라인·`fast-ha`인 다른 노드 가운데 id 순으로 다음)에게 노드 사이 RPC로 부탁한다. 이웃이 없으면
/// 409 `no_fence_checker`, 이웃이 답하지 않으면 502 `fence_check_failed`.
#[utoipa::path(post, path = "/nodes/{key}/fence-test", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 200, body = FenceTestResult), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn test_node_fence(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<FenceTestResult>, ApiError> {
    state.authorize(&principal, Action::Operate, NodeFenceSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    let fence = stored(&state, &node)?.ok_or_else(|| not_set(&node))?;
    if node.id != state.node {
        let me = state
            .cluster
            .get::<NodeSpec>(state.node)?
            .map_or_else(|| state.node.to_string(), |n| n.name);
        let methods = crate::power_fence::check(state.fence.as_ref(), &node, &fence.spec).await;
        return Ok(Json(FenceTestResult {
            checked_by: me,
            methods,
        }));
    }
    let (_, nodes) = state.cluster.list::<NodeSpec>()?;
    let no_checker = || {
        ApiError::conflict(
            "no_fence_checker",
            format!(
                "no other online node can check the power of node {:?}",
                node.name
            ),
        )
    };
    let via = neighbour(&nodes, node.id).ok_or_else(no_checker)?;
    let addr = state
        .cluster
        .members()
        .addr(NodeId(via.id))
        .map(str::to_string);
    let (Some(peers), Some(addr)) = (&state.peers, addr) else {
        return Err(no_checker());
    };
    let failed = |e: String| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "fence_check_failed",
            format!("node {:?} could not check the power: {e}", via.name),
        )
    };
    let methods = tokio::time::timeout(
        NEIGHBOUR_TIMEOUT,
        peers.fence_status(NodeId(via.id), &addr, node.id, NEIGHBOUR_TIMEOUT),
    )
    .await
    .map_err(|_| failed("timed out".into()))?
    .map_err(|s| failed(s.message().to_string()))?
    .ok_or_else(|| failed("it does not support power fencing".into()))?;
    Ok(Json(FenceTestResult {
        checked_by: via.name.clone(),
        methods,
    }))
}

/// 노드를 지울 때 그 펜싱 설정도 지운다(없으면 아무것도 쓰지 않는다 — 옛 노드가 이 종류를 풀지 못한다).
pub(crate) async fn delete_with_node(state: &AppState, node: &Node) -> Result<(), ApiError> {
    if stored(state, node)?.is_some() {
        match state.cluster.delete::<NodeFenceSpec>(node.id, None).await {
            Ok(_) | Err(opl_cluster::ClusterError::Rejected(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
