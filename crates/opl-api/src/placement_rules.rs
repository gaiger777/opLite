//! 배치 규칙 만들기·바꾸기·지우기. 목록과 조회는 `resources`의 공통 핸들러다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_types::node::HA_PLACEMENT;
use opl_types::placement::{PlacementRule, PlacementRuleSpec, PlacementRuleStatus};
use opl_types::resource::{Resource, ResourceSpec};

use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::placement::conflicts;
use crate::resources::{CreateRequest, UpdateRequest, check_name, if_match, lookup};
use crate::scheduler::Inventory;
use crate::state::AppState;

/// 형식(400), 규칙의 노드가 모두 있는지(400), 모든 노드가 새 종류를 아는지(409 `cluster_upgrading`),
/// 이 규칙이 다른 켜진 hard 규칙과 모순인지(422 `rule_conflict`, `details.rules`)
fn check(state: &AppState, rule: &PlacementRule) -> Result<(), ApiError> {
    rule.spec
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let mut inv = Inventory::load(&state.cluster)?;
    for node in &rule.spec.nodes {
        if !inv.nodes.iter().any(|n| &n.name == node) {
            return Err(ApiError::bad_request(format!("node {node:?} not found")));
        }
    }
    // 옛 노드는 이 종류의 로그 항목을 풀지 못한다.
    require_feature(&state.cluster, HA_PLACEMENT)?;
    inv.rules.retain(|r| r.name != rule.name);
    inv.rules.push(rule.clone());
    // 이 규칙이 든 모순만 거절한다(그 뒤 레이블이 바뀌어 생긴 다른 규칙끼리의 모순은 위반으로 보인다).
    if let Err(names) = conflicts(&inv.rules, &inv)
        && names.contains(&rule.name)
    {
        let mut e = ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "rule_conflict",
            format!(
                "placement rule {:?} contradicts other rules: {}",
                rule.name,
                names.join(", ")
            ),
        );
        e.details = Some(serde_json::json!({ "rules": names }));
        return Err(e);
    }
    Ok(())
}

/// VM끼리(`affinity`·`anti-affinity`와 `soft-` 형태) 또는 VM과 노드(`node-affinity`·`node-anti-affinity`와
/// `soft-` 형태) 배치 규칙. VM은 이름(`vms`)이나 레이블(`vm_tag`), 노드는 이름(`nodes`)이나 노드 레이블
/// (`node_tag`, 예: `zone=rack-a`)로 고른다. 켜진 hard 규칙끼리 모순이면 422 `rule_conflict`. 이미 놓인 VM은
/// 옮기지 않는다. 모든 노드가 `ha-placement`를 알아야 한다.
#[utoipa::path(post, path = "/placement-rules", tag = "ha",
    request_body = CreateRequest<PlacementRuleSpec>,
    responses((status = 201, body = Resource<PlacementRuleSpec, PlacementRuleStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_placement_rule(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<PlacementRuleSpec>>,
) -> Result<(StatusCode, Json<PlacementRule>), ApiError> {
    state.authorize(&principal, Action::Write, PlacementRuleSpec::KIND)?;
    check_name(&req.name)?;
    let mut rule = Resource::new(req.name, req.spec);
    rule.labels = req.labels;
    // VM 만들기·바꾸기와 같은 잠금: 함께 들어온 규칙끼리 모순 검사를 지나 함께 저장되지 않게.
    let _lock = state.vm_lock.lock().await;
    check(&state, &rule)?;
    rule.revision = state.cluster.create(&rule).await?;
    Ok((StatusCode::CREATED, Json(rule)))
}

/// 규칙을 통째로 바꾼다(`enabled`로 켜고 끈다).
#[utoipa::path(put, path = "/placement-rules/{key}", tag = "ha",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<PlacementRuleSpec>,
    responses((status = 200, body = Resource<PlacementRuleSpec, PlacementRuleStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_placement_rule(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<PlacementRuleSpec>>,
) -> Result<Json<PlacementRule>, ApiError> {
    state.authorize(&principal, Action::Write, PlacementRuleSpec::KIND)?;
    let _lock = state.vm_lock.lock().await;
    let mut rule = lookup::<PlacementRuleSpec>(&state.cluster, &key)?;
    rule.spec = req.spec;
    rule.labels = req.labels;
    check(&state, &rule)?;
    state
        .cluster
        .update_spec(
            rule.id,
            if_match(&headers)?,
            rule.labels.clone(),
            &rule.spec,
        )
        .await?;
    Ok(Json(lookup::<PlacementRuleSpec>(
        &state.cluster,
        &rule.id.to_string(),
    )?))
}

#[utoipa::path(delete, path = "/placement-rules/{key}", tag = "ha",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_placement_rule(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, PlacementRuleSpec::KIND)?;
    let rule = lookup::<PlacementRuleSpec>(&state.cluster, &key)?;
    state
        .cluster
        .delete::<PlacementRuleSpec>(rule.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
