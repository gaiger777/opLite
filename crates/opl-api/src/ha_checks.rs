//! HA 검사 만들기·바꾸기·지우기. 목록과 조회는 `resources`의 공통 핸들러다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_types::ha_check::{
    AUTO_PREFIX, HaCheck, HaCheckSpec, HaCheckStatus, HaCheckTarget, MAX_HA_CHECKS,
};
use opl_types::node::{HA_CHECKS, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::{StoragePoolSpec, is_shared};

use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, UpdateRequest, check_name, if_match, lookup};
use crate::state::AppState;

/// 형식(400), 자동 검사 이름(400), 덮기 노드가 있는지(400), 풀이 있고 공유인지(400), 만들 때 개수(400),
/// 모든 노드가 새 종류를 아는지(409 `cluster_upgrading`)
fn check(state: &AppState, name: &str, spec: &HaCheckSpec, creating: bool) -> Result<(), ApiError> {
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if name.starts_with(AUTO_PREFIX) {
        return Err(ApiError::bad_request(format!(
            "names starting with {AUTO_PREFIX:?} are reserved for the automatic cluster link checks"
        )));
    }
    let (_, nodes) = state.cluster.list::<NodeSpec>()?;
    for n in spec.nodes.keys() {
        if !nodes.iter().any(|node| &node.name == n) {
            return Err(ApiError::bad_request(format!("node {n:?} not found")));
        }
    }
    for target in std::iter::once(&spec.target).chain(spec.nodes.values()) {
        if let HaCheckTarget::Pool { name } = target {
            let pool = state
                .cluster
                .get_by_name::<StoragePoolSpec>(name)?
                .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))?;
            if !is_shared(&pool) {
                return Err(ApiError::bad_request(
                    "pool checks need a shared pool (nfs, lvm-shared, rbd or shared dir)",
                ));
            }
        }
    }
    // 검사 결과는 리스 갱신마다 실리므로 수를 묶는다.
    if creating && state.cluster.list::<HaCheckSpec>()?.1.len() >= MAX_HA_CHECKS {
        return Err(ApiError::bad_request(format!(
            "a cluster can have at most {MAX_HA_CHECKS} HA checks; delete one first"
        )));
    }
    // 옛 노드는 이 종류의 로그 항목을 풀지 못한다.
    require_feature(&state.cluster, HA_CHECKS)
}

/// 노드가 5초마다 링크·핑·스토리지 풀 경로를 검사하고, 리더가 `grace_secs` 넘게 실패한 검사에
/// `on_fail`대로 대응한다(경고·라이브 마이그레이션·다른 노드에서 다시 켜기·전원 끄기). `nodes`는 노드별
/// 대상이다. 모든 노드가 `ha-checks`를 알아야 한다.
#[utoipa::path(post, path = "/ha-checks", tag = "ha",
    request_body = CreateRequest<HaCheckSpec>,
    responses((status = 201, body = Resource<HaCheckSpec, HaCheckStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_ha_check(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<HaCheckSpec>>,
) -> Result<(StatusCode, Json<HaCheck>), ApiError> {
    state.authorize(&principal, Action::Write, HaCheckSpec::KIND)?;
    check_name(&req.name)?;
    check(&state, &req.name, &req.spec, true)?;
    let mut c = Resource::new(req.name, req.spec);
    c.labels = req.labels;
    c.revision = state.cluster.create(&c).await?;
    Ok((StatusCode::CREATED, Json(c)))
}

/// 검사를 통째로 바꾼다. 진행 중인 실패는 새 설정으로 다시 판단한다.
#[utoipa::path(put, path = "/ha-checks/{key}", tag = "ha",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<HaCheckSpec>,
    responses((status = 200, body = Resource<HaCheckSpec, HaCheckStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_ha_check(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<HaCheckSpec>>,
) -> Result<Json<HaCheck>, ApiError> {
    state.authorize(&principal, Action::Write, HaCheckSpec::KIND)?;
    let c = lookup::<HaCheckSpec>(&state.cluster, &key)?;
    check(&state, &c.name, &req.spec, false)?;
    state
        .cluster
        .update_spec(c.id, if_match(&headers)?, req.labels, &req.spec)
        .await?;
    Ok(Json(lookup::<HaCheckSpec>(
        &state.cluster,
        &c.id.to_string(),
    )?))
}

#[utoipa::path(delete, path = "/ha-checks/{key}", tag = "ha",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_ha_check(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, HaCheckSpec::KIND)?;
    let c = lookup::<HaCheckSpec>(&state.cluster, &key)?;
    state
        .cluster
        .delete::<HaCheckSpec>(c.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
