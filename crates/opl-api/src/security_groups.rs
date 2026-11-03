//! 보안 그룹 만들기·바꾸기·지우기. 목록과 조회는 `resources`의 공통 핸들러다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::security_group::{Rule, SecurityGroup, SecurityGroupSpec, SecurityGroupStatus};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, check_name, if_match, lookup};
use crate::state::AppState;

fn validate(spec: &SecurityGroupSpec) -> Result<(), ApiError> {
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

/// 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다.
#[utoipa::path(post, path = "/security-groups", tag = "security-groups",
    request_body = CreateRequest<SecurityGroupSpec>,
    responses((status = 201, body = Resource<SecurityGroupSpec, SecurityGroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_security_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<SecurityGroupSpec>>,
) -> Result<(StatusCode, Json<SecurityGroup>), ApiError> {
    state.authorize(&principal, Action::Write, SecurityGroupSpec::KIND)?;
    check_name(&req.name)?;
    validate(&req.spec)?;
    let mut group = Resource::new(req.name, req.spec);
    group.labels = req.labels;
    group.revision = state.cluster.create(&group).await?;
    Ok((StatusCode::CREATED, Json(group)))
}

/// 보안 그룹에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecurityGroupPatch {
    /// 빈 글이면 설명을 지운다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub description: Option<String>,
    /// 규칙 전체를 이것으로 바꾼다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub rules: Option<Vec<Rule>>,
}

/// 설명이나 규칙을 바꾼다. 쓰는 VM의 노드가 곧바로 적용한다. `If-Match`가 없으면 읽은 revision에
/// 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
#[utoipa::path(patch, path = "/security-groups/{key}", tag = "security-groups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = SecurityGroupPatch,
    responses((status = 200, body = Resource<SecurityGroupSpec, SecurityGroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_security_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SecurityGroupPatch>,
) -> Result<Json<SecurityGroup>, ApiError> {
    state.authorize(&principal, Action::Write, SecurityGroupSpec::KIND)?;
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let group = lookup::<SecurityGroupSpec>(&state.cluster, &key)?;
        if let Some(expected) = expected
            && expected != group.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: group.revision,
            })
            .into());
        }
        let mut spec = group.spec.clone();
        if let Some(description) = &req.description {
            spec.description = Some(description.clone()).filter(|d| !d.is_empty());
        }
        if let Some(rules) = &req.rules {
            spec.rules = rules.clone();
        }
        validate(&spec)?;
        if spec == group.spec {
            return Ok(Json(group));
        }
        match state
            .cluster
            .update_spec(group.id, Some(group.revision), group.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        return Ok(Json(lookup::<SecurityGroupSpec>(
            &state.cluster,
            &group.id.to_string(),
        )?));
    }
    Err(ApiError::conflict(
        "busy",
        "the security group kept changing while being updated; try again",
    ))
}

/// VM의 NIC가 쓰고 있으면 409 `security_group_in_use`
#[utoipa::path(delete, path = "/security-groups/{key}", tag = "security-groups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_security_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, SecurityGroupSpec::KIND)?;
    // VM 만들기·바꾸기와 같은 잠금: 검사와 지우기 사이에 그룹을 거는 VM이 생기지 않게.
    let _lock = state.vm_lock.lock().await;
    let group = lookup::<SecurityGroupSpec>(&state.cluster, &key)?;
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    if let Some(vm) = vms.iter().find(|vm| {
        vm.spec
            .nics
            .iter()
            .any(|n| n.security_groups.contains(&group.name))
    }) {
        return Err(ApiError::conflict(
            "security_group_in_use",
            format!(
                "security group {:?} is used by VM {:?}",
                group.name, vm.name
            ),
        ));
    }
    state
        .cluster
        .delete::<SecurityGroupSpec>(group.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
