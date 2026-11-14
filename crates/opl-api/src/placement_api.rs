//! 최적 배치 API(설계 §8.3): 리더가 아는 노드 부하와 균형 조정기의 결과, 추천 적용.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_types::node::{NodeSpec, PLACEMENT};
use opl_types::placement::{MoveReason, PlacementMove, PlacementStatus};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::settings::PlacementMode;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{Vm, VmSpec};
use tokio::time::Instant;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal, unix_now};
use crate::balance::{self, MAX_CONCURRENT};
use crate::cluster::missing_feature;
use crate::error::{ApiError, Json};
use crate::ha_health::Health;
use crate::placement::{Assign, allocated_mib};
use crate::resources::lookup;
use crate::scheduler::Inventory;
use crate::state::AppState;
use crate::vms::{RuleCheck, start_migration};

/// 최적 배치 상태: 설정, 노드마다의 부하(리더가 리스로 받은 요약), 균형 조정기의 마지막 결과. 로그인한 누구나 읽는다.
/// 리더가 아닌 노드는 리더에 묻고, 묻지 못하면 503 `placement_unavailable`이다.
#[utoipa::path(get, path = "/placement", tag = "placement",
    responses((status = 200, body = PlacementStatus), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn get_placement(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<PlacementStatus>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    Ok(Json(crate::score::status(&state).await?))
}

/// 균형 조정기의 마지막 회차가 추천한 옮기기(auto면 시작한 옮기기와 그 Task). 회차마다 새로 계산한다.
/// 리더가 아닌 노드는 리더에 묻고, 묻지 못하면 503 `placement_unavailable`이다.
#[utoipa::path(get, path = "/placement/recommendations", tag = "placement",
    responses((status = 200, body = Vec<PlacementMove>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn list_recommendations(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Vec<PlacementMove>>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    Ok(Json(crate::score::status(&state).await?.recommendations))
}

/// 추천 `id`를 적용한다: 그 VM을 추천한 노드로 옮기는 마이그레이션 Task(202, 권한은 VM 마이그레이션과 같음).
/// 리더 노드의 API에서만 적용한다(리더가 아니면 409 `recommendation_stale` — 리더 메모리의 HA 검사 결과·균형 조정기의
/// 상태로 다시 보므로). 추천이 없으면(회차마다 새로 계산한다) 404 `recommendation_not_found`, VM이 고정
/// (`placement.pinned`)이면 409 `vm_pinned`, 클러스터에 끝나지 않은 마이그레이션이 둘 이상이면 409 `placement_busy`다.
/// 추천은 회차 때의 상태로 계산했으므로 지금 상태로 균형 조정기의 조건을 다시 본다: 모드가 `off`이거나, 균형 조정기가 쉬는
/// 이유가 있거나(유지보수 노드·HA 되돌아가기 기다림 등), VM을 옮길 수 없거나(옮기는 중·Task 중·1시간 안에 옮김 등),
/// 대상이 조건에 맞지 않으면(메모리·규칙·HA 검사·살아 있음을 본 시간 등) 409 `recommendation_stale`(문구에 이유).
/// 적용은 한 번에 하나씩 다시 본다. 그 밖의 오류는 마이그레이션과 같다(VM이 이미 다른 노드면 409 `vm_moved`,
/// `no_capacity`, `placement_rule` 등).
#[utoipa::path(post, path = "/placement/recommendations/{id}/apply", tag = "placement",
    params(("id" = String, Path, description = "recommendation id")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn apply_recommendation(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let cluster = &state.cluster;
    let leader = cluster.members().leader;
    if leader != Some(cluster.id()) {
        let name = leader
            .and_then(|id| cluster.get::<NodeSpec>(id.0).ok().flatten())
            .map_or_else(
                || "the leader node".to_string(),
                |n| format!("node {:?}", n.name),
            );
        return Err(ApiError::conflict(
            "recommendation_stale",
            format!(
                "recommendations are applied on the leader node, which checks them again; send the request to {name}"
            ),
        ));
    }
    // 다시 보기와 마이그레이션 만들기 사이에 다른 적용이 끼지 않게 하나씩 한다.
    let _one = state.placement_apply.lock().await;
    let status = crate::score::status(&state).await?;
    let id = Uuid::parse_str(&id).ok();
    let found = status
        .recommendations
        .into_iter()
        .find(|m| Some(m.id) == id);
    let (Some(id), Some(m)) = (id, found) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "recommendation_not_found",
            "the recommendation is gone; recommendations are recomputed every run",
        ));
    };
    let vm = lookup::<VmSpec>(&state.cluster, &m.vm.to_string())?;
    if vm.spec.pinned() {
        return Err(ApiError::conflict(
            "vm_pinned",
            format!(
                "VM {:?} is pinned; the balancer's recommendations do not move it",
                vm.name
            ),
        ));
    }
    recheck(&state, &vm, &m)?;
    let (task, _) =
        start_migration(&state, m.vm, m.to, None, Some(m.from), RuleCheck::Enforce).await?;
    audit.detail(format!("placement recommendation {id}"));
    // 게시판의 추천에도 Task를 적는다.
    state.placement.set_task(id, task.id);
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 추천을 적용하기 전에 리더가 지금 상태로 균형 조정기의 조건을 다시 본다([`apply_recommendation`]): 모드, 진행 중인
/// 마이그레이션 수, 쉬는 이유([`balance::paused`]), 옮길 수 있는 VM, 대상 조건(균형 옮기기는 [`balance::target_ok`]와
/// 살아 있음을 본 시간, 규칙 바로잡기는 살아 있음과 HA 검사).
fn recheck(state: &AppState, vm: &Vm, m: &PlacementMove) -> Result<(), ApiError> {
    let stale = |reason: &str| {
        ApiError::conflict(
            "recommendation_stale",
            format!(
                "the recommendation is stale ({reason}); recommendations are recomputed every run"
            ),
        )
    };
    let cluster = &state.cluster;
    let settings = crate::settings::current(cluster)?.spec.placement();
    if settings.mode == PlacementMode::Off {
        return Err(stale("the balancer is off"));
    }
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let migrating = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished() && matches!(t.spec.op, TaskOp::VmMigrate { .. }))
        .count();
    if migrating >= MAX_CONCURRENT {
        return Err(ApiError::conflict(
            "placement_busy",
            format!(
                "{migrating} migrations are in progress; apply the recommendation when one has finished"
            ),
        ));
    }
    let inv = Inventory::load(cluster)?;
    let leases = cluster.leases();
    let health = Health::load(cluster, &leases)?;
    let board = state.placement.read();
    let Some(b) = board.base else {
        return Err(stale("the balancer has not run on this leader"));
    };
    let now = Instant::now();
    let missing = missing_feature(cluster, PLACEMENT)?;
    let paused = balance::paused(
        &inv,
        &leases,
        now,
        b,
        &settings,
        &tasks,
        &missing,
        &health,
        &board.alive_since,
    );
    if let Some(reason) = paused {
        return Err(stale(&format!("balancer paused: {reason}")));
    }
    balance::movable(vm, &inv, &tasks, &BTreeMap::new(), unix_now()).map_err(stale)?;
    let node = |id: Uuid| inv.nodes.iter().find(|n| n.id == id);
    let (Some(from), Some(to)) = (node(m.from), node(m.to)) else {
        return Err(stale("a node is gone"));
    };
    let alive = board.alive_since.get(&to.id);
    let none = Assign::default();
    match m.reason {
        MoveReason::Balance => {
            if alive.is_none_or(|since| now.saturating_duration_since(*since) < board.settle) {
                return Err(stale("the target node has not been up long enough"));
            }
            let free =
                to.status.memory_mib as i64 - allocated_mib(to, &inv, &none, &[vm.id]) as i64;
            balance::target_ok(vm, from, to, &inv, &none, &health, free).map_err(stale)
        }
        // 규칙 바로잡기의 메모리·노드 조건·규칙은 마이그레이션이 본다.
        MoveReason::Rule if alive.is_none() => Err(stale("the target node is not alive")),
        MoveReason::Rule if health.blocks(vm, to, &inv) => {
            Err(stale("a failing HA check affects the VM"))
        }
        MoveReason::Rule => Ok(()),
    }
}
