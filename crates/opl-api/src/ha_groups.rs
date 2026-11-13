//! HA 그룹 만들기·바꾸기·지우기. 목록과 조회는 `resources`의 공통 핸들러다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_types::node::{HA_PLACEMENT, NodeSpec};
use opl_types::placement::{HaGroup, HaGroupSpec, HaGroupStatus};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::vm::VmSpec;

use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, UpdateRequest, check_name, if_match, lookup};
use crate::state::AppState;

/// 형식(400), 그룹 노드가 모두 있는지(400), 모든 노드가 새 종류를 아는지(409 `cluster_upgrading`)
fn check(state: &AppState, spec: &HaGroupSpec) -> Result<(), ApiError> {
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let (_, nodes) = state.cluster.list::<NodeSpec>()?;
    for n in &spec.nodes {
        if !nodes.iter().any(|node| node.name == n.node) {
            return Err(ApiError::bad_request(format!(
                "node {:?} not found",
                n.node
            )));
        }
    }
    // 옛 노드는 이 종류의 로그 항목을 풀지 못한다.
    require_feature(&state.cluster, HA_PLACEMENT)
}

/// HA VM이 `ha.group`으로 쓰는 노드 묶음과 우선순위(클수록 먼저). `restricted`면 그룹 밖 노드에 놓지 않고,
/// `nofailback`이면 우선 노드가 돌아와도 되돌아가지 않는다. 모든 노드가 `ha-placement`를 알아야 한다.
#[utoipa::path(post, path = "/ha-groups", tag = "ha",
    request_body = CreateRequest<HaGroupSpec>,
    responses((status = 201, body = Resource<HaGroupSpec, HaGroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_ha_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<HaGroupSpec>>,
) -> Result<(StatusCode, Json<HaGroup>), ApiError> {
    state.authorize(&principal, Action::Write, HaGroupSpec::KIND)?;
    check_name(&req.name)?;
    check(&state, &req.spec)?;
    let mut group = Resource::new(req.name, req.spec);
    group.labels = req.labels;
    group.revision = state.cluster.create(&group).await?;
    Ok((StatusCode::CREATED, Json(group)))
}

/// 그룹을 통째로 바꾼다. 이미 놓인 VM은 옮기지 않는다.
#[utoipa::path(put, path = "/ha-groups/{key}", tag = "ha",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<HaGroupSpec>,
    responses((status = 200, body = Resource<HaGroupSpec, HaGroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_ha_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<HaGroupSpec>>,
) -> Result<Json<HaGroup>, ApiError> {
    state.authorize(&principal, Action::Write, HaGroupSpec::KIND)?;
    let group = lookup::<HaGroupSpec>(&state.cluster, &key)?;
    check(&state, &req.spec)?;
    state
        .cluster
        .update_spec(group.id, if_match(&headers)?, req.labels, &req.spec)
        .await?;
    Ok(Json(lookup::<HaGroupSpec>(
        &state.cluster,
        &group.id.to_string(),
    )?))
}

/// VM이 쓰고 있으면 409 `group_in_use`
#[utoipa::path(delete, path = "/ha-groups/{key}", tag = "ha",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_ha_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, HaGroupSpec::KIND)?;
    // VM 만들기·바꾸기와 같은 잠금: 검사와 지우기 사이에 그룹을 쓰는 VM이 생기지 않게.
    let _lock = state.vm_lock.lock().await;
    let group = lookup::<HaGroupSpec>(&state.cluster, &key)?;
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    if let Some(vm) = vms.iter().find(|vm| {
        vm.spec
            .ha
            .as_ref()
            .is_some_and(|ha| ha.group.as_ref() == Some(&group.name))
    }) {
        return Err(ApiError::conflict(
            "group_in_use",
            format!("HA group {:?} is used by VM {:?}", group.name, vm.name),
        ));
    }
    state
        .cluster
        .delete::<HaGroupSpec>(group.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
