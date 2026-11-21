//! 단순 → SDN 옮기기 API(계획 67 §9·§13·§14): `/network-migrations`. 엔진(계획·리소스 만들기·걸음 Task·멈춤)은
//! [`crate::tasks::network_migrate`]이고, 여기는 경로·권한·감사·게스트 에이전트 주소 읽기·다리 노드 바꾸기다.
//!
//! 권한(§13): 옮기기 리소스는 원본 네트워크의 폴더(네트워크 트리)에 산다. 만들기·시작·이어 가기·끝내기·되돌리기는
//! 원본 네트워크의 `network.config`, 대상 네트워크의 `network.assign`, 옮길 VM마다 `vm.config`다(계획에서는 권한 없는 VM이
//! "막힘"으로 보이고, 걸음은 남은 VM의 권한이 모두 있어야 한다). 끝내며 라우터를 붙이면 라우터 `PATCH`와 같이 그
//! 라우터와 대상 네트워크의 `network.config`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::authz::Privilege;
use opl_types::guest::GuestInfo;
use opl_types::network::NetworkSpec;
use opl_types::network_migration::{
    MigrationPlan, MigrationStep, MoveState, NetworkMigration, NetworkMigrationSpec,
    NetworkMigrationStatus, PlannedMode,
};
use opl_types::node::{NodeSpec, SDN_SERVICES};
use opl_types::resource::{Kind, Resource};
use opl_types::router::RouterSpec;
use opl_types::task::{Task, TaskSpec, TaskStatus};
use opl_types::vm::{Vm, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::authz::AuthzObject;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{check_name, lookup};
use crate::routing::warning_header;
use crate::state::AppState;
use crate::tasks::network_migrate as engine;

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message).with_code("invalid_migration")
}

/// 옮기기를 쓰려면 모든 노드가 `sdn-services`를 알고(409 `cluster_upgrading`) SDN이 세워졌어야 한다(409 `sdn_not_ready`).
fn require_services(state: &AppState) -> Result<(), ApiError> {
    require_feature(&state.cluster, SDN_SERVICES)?;
    crate::sdn::require_ready(&state.cluster)?;
    Ok(())
}

/// 원본 네트워크의 `network.config`와 대상 네트워크의 `network.assign`(없는 네트워크는 엔진이 400을 낸다)
fn require_networks(
    state: &AppState,
    principal: &Principal,
    spec: &NetworkMigrationSpec,
) -> Result<(), ApiError> {
    state.authz.require_networks_with(
        principal,
        Privilege::NetworkConfig,
        [spec.source.as_str()],
    )?;
    state
        .authz
        .require_networks(principal, [spec.target.as_str()])
}

/// 부른 사용자가 VM의 `vm.config`를 가졌는지(계획의 "권한 없음")
fn configures<'a>(
    state: &'a AppState,
    principal: &'a Principal,
) -> impl Fn(&Vm) -> bool + Sync + 'a {
    move |vm: &Vm| {
        state
            .authz
            .allows(principal, Privilege::VmConfig, vm.authz_target())
    }
}

/// 부른 사용자가 VM을 읽을 수 있는지(`system.read` — 계획에 이름·MAC·주소를 낼지, 리뷰 1 I3)
fn reads<'a>(state: &'a AppState, principal: &'a Principal) -> impl Fn(&Vm) -> bool + Sync + 'a {
    move |vm: &Vm| {
        state
            .authz
            .allows(principal, Privilege::SystemRead, vm.authz_target())
    }
}

/// 부른 사용자가 VM `id`를 읽을 수 있는지. 지운 VM은 감출 것이 없어 참이다.
pub(crate) fn sees_vm(state: &AppState, principal: &Principal, id: Uuid) -> bool {
    match state.cluster.get::<VmSpec>(id) {
        Ok(Some(vm)) => reads(state, principal)(&vm),
        Ok(None) => true,
        Err(_) => false,
    }
}

/// 옮기기 보기에서 부른 사용자가 읽을 수 없는 VM의 기록(MAC·주소·까닭)과 준 주소를 뺀다(리뷰 1 I3 — `GET`·목록·watch).
pub(crate) fn redact(state: &AppState, principal: &Principal, m: &mut NetworkMigration) {
    m.status.vms.retain(|v| sees_vm(state, principal, v.vm));
    m.spec.addresses.retain(|a| sees_vm(state, principal, a.vm));
}

/// [`redact`]의 JSON 판(watch 이벤트의 스펙·상태)
pub(crate) fn redact_json(
    state: &AppState,
    principal: &Principal,
    spec: &mut serde_json::Value,
    status: &mut serde_json::Value,
) {
    let keep = |item: &serde_json::Value| {
        item["vm"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            .is_none_or(|id| sees_vm(state, principal, id))
    };
    if let Some(vms) = status.get_mut("vms").and_then(|v| v.as_array_mut()) {
        vms.retain(keep);
    }
    if let Some(addresses) = spec.get_mut("addresses").and_then(|v| v.as_array_mut()) {
        addresses.retain(keep);
    }
}

/// 이름을 주지 않은 옮기기의 이름: `<원본>-to-<대상>`, 쓰였으면 `-2`, `-3` …(63자 안 — 끝난 옮기기의 기록이 이름을 쥐고 있어도
/// 같은 원본·대상을 다시 옮길 수 있게, 리뷰 1 웹 I2)
pub(crate) fn pick_name(source: &str, target: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{source}-to-{target}");
    let fit = |suffix: &str| {
        let mut b = base.clone();
        b.truncate(63 - suffix.len());
        let b = b.trim_end_matches('-');
        format!("{b}{suffix}")
    };
    (1..)
        .map(|n| {
            fit(&if n == 1 {
                String::new()
            } else {
                format!("-{n}")
            })
        })
        .find(|name| !taken(name))
        .unwrap_or(base)
}

/// 계획할 VM(스펙의 `vms`, 없으면 원본에 NIC가 있는 VM) 가운데 켜진 것의 게스트 에이전트 정보(캐시 — §9.2 주소 출처
/// `guest-agent`). 답하지 않는 VM은 빠진다(주소를 모름). 부른 사용자가 읽을 수 없는 VM은 묻지 않는다(리뷰 1 I3).
async fn guests(
    state: &AppState,
    principal: &Principal,
    spec: &NetworkMigrationSpec,
) -> BTreeMap<Uuid, GuestInfo> {
    let Ok((_, vms)) = state.cluster.list::<VmSpec>() else {
        return BTreeMap::new();
    };
    let sees = reads(state, principal);
    let wanted: Vec<Vm> = vms
        .into_iter()
        .filter(|v| !v.spec.template && sees(v))
        .filter(|v| {
            if spec.vms.is_empty() {
                v.spec.nics.iter().any(|n| n.network == spec.source)
            } else {
                spec.vms.contains(&v.id)
            }
        })
        .collect();
    let answers = futures_util::future::join_all(
        wanted
            .iter()
            .map(|vm| async move { (vm.id, crate::guest::cached_info(state, vm).await) }),
    )
    .await;
    answers
        .into_iter()
        .filter_map(|(id, info)| info.map(|i| (id, i)))
        .collect()
}

/// 감사의 계획 요약: `3 VMs, live 2, restart 1`
fn plan_summary(plan: &MigrationPlan) -> String {
    let vms: std::collections::BTreeSet<Uuid> = plan.nics.iter().map(|n| n.vm).collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for n in &plan.nics {
        let mode = match n.mode {
            PlannedMode::Offline => "offline",
            PlannedMode::Live => "live",
            PlannedMode::Restart => "restart",
            PlannedMode::Blocked => "blocked",
        };
        *counts.entry(mode).or_default() += 1;
    }
    let mut out = format!("{} VMs", vms.len());
    for (mode, n) in counts {
        out.push_str(&format!(", {mode} {n}"));
    }
    out
}

/// `GET /network-migrations`
#[derive(Debug, Serialize, ToSchema)]
pub struct NetworkMigrationList {
    pub revision: u64,
    pub items: Vec<Resource<NetworkMigrationSpec, NetworkMigrationStatus>>,
}

/// 볼 수 있는 네트워크 옮기기(끝난 것도 기록으로 남는다)
#[utoipa::path(get, path = "/network-migrations", tag = "network-migrations",
    responses((status = 200, body = NetworkMigrationList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_network_migrations(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<NetworkMigrationList>, ApiError> {
    let (revision, items) = state.cluster.list::<NetworkMigrationSpec>()?;
    let mut items = state.authz.filter(&principal, items)?;
    items.sort_by(|a, b| a.name.cmp(&b.name));
    for m in &mut items {
        redact(&state, &principal, m);
    }
    Ok(Json(NetworkMigrationList { revision, items }))
}

/// 네트워크 옮기기 하나(VM NIC마다 방식·주소·상태, 다리)
#[utoipa::path(get, path = "/network-migrations/{key}", tag = "network-migrations",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<NetworkMigrationSpec, NetworkMigrationStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_network_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<NetworkMigration>, ApiError> {
    let mut m = lookup::<NetworkMigrationSpec>(&state.cluster, &key)?;
    redact(&state, &principal, &mut m);
    Ok(Json(m))
}

/// `POST /network-migrations`
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateNetworkMigration {
    /// 비면 `<원본>-to-<대상>`(쓰였으면 `-2`, `-3` …)
    #[serde(default)]
    #[schema(default)]
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub spec: NetworkMigrationSpec,
}

/// `POST /network-migrations`의 질의
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct DryRunQuery {
    /// 참이면 리소스를 만들지 않고 계획만 돌려준다(200).
    #[serde(default)]
    pub dry_run: bool,
}

/// 만든 옮기기와 그 계획(VM NIC마다 막는 까닭·경고)
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct NetworkMigrationCreated {
    #[serde(flatten)]
    pub migration: Resource<NetworkMigrationSpec, NetworkMigrationStatus>,
    pub plan: MigrationPlan,
}

/// 옮기기를 계획하고 만든다(계획 67 §9.2): `dry_run`이면 리소스 없이 계획만(200), 아니면 계획을 `status.vms`에 적은
/// `planned` 리소스(201 — 시작은 `POST …/start`). 이름을 주지 않으면 `<원본>-to-<대상>`(쓰였으면 `-2` …)이다. 켜진 VM의
/// 주소는 게스트 에이전트가 알린 것을 쓴다. 부른 사용자가 읽을 수 없는 VM은 계획에 없고 수(`hidden`)만 있다. 원본의
/// `network.config`·대상의 `network.assign`, 폴더(없으면 원본 네트워크의 폴더)의 `network.config`. 권한이 없는 VM은
/// 계획에서 막힌다. 옛 노드가 남았으면 409 `cluster_upgrading`, SDN이 없으면 409 `sdn_not_ready`, 스펙·네트워크는 400
/// `invalid_migration`, 같은 원본·같은 다리 대상의 끝나지 않은 옮기기·다른 옮기기의 VM은 409 `migration_overlap`. 계획에
/// 경고가 있으면
/// `orbivisor-warning`에 그 수.
#[utoipa::path(post, path = "/network-migrations", tag = "network-migrations",
    params(DryRunQuery),
    request_body = CreateNetworkMigration,
    responses((status = 200, description = "The plan only (dry_run)", body = MigrationPlan),
        (status = 201, body = NetworkMigrationCreated,
            headers(("orbivisor-warning" = String, description = "the plan has warnings (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_network_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Query(query): Query<DryRunQuery>,
    Json(req): Json<CreateNetworkMigration>,
) -> Result<Response, ApiError> {
    require_services(&state)?;
    if !req.name.is_empty() {
        check_name(&req.name)?;
    }
    require_networks(&state, &principal, &req.spec)?;
    // 읽을 수 없는 VM은 없는 VM과 같다(리뷰 1 I3 — 있는지도 캐 보지 못하게).
    for id in req
        .spec
        .vms
        .iter()
        .chain(req.spec.addresses.iter().map(|a| &a.vm))
    {
        if !sees_vm(&state, &principal, *id) {
            return Err(invalid(format!("VM {id} not found")));
        }
    }
    let folder = match req.spec.folder {
        Some(f) => Some(f),
        None => state
            .cluster
            .get_by_name::<NetworkSpec>(&req.spec.source)?
            .and_then(|n| n.spec.folder),
    };
    state.authz.require_new(
        &principal,
        Privilege::NetworkConfig,
        Kind::NetworkMigration,
        folder,
    )?;
    let guests = Box::pin(guests(&state, &principal, &req.spec)).await;
    let allowed = configures(&state, &principal);
    let sees = reads(&state, &principal);
    if query.dry_run {
        let plan = engine::plan_migration(&state.cluster, &req.spec, &guests, &allowed, &sees)?;
        return Ok((StatusCode::OK, Json(plan)).into_response());
    }
    // 이름을 주지 않았으면 고른다. 두 요청이 같은 이름을 골랐으면 다시 고른다.
    let mut tries = 0;
    let (mut migration, plan) = loop {
        tries += 1;
        let name = if req.name.is_empty() {
            pick_name(&req.spec.source, &req.spec.target, |n| {
                state
                    .cluster
                    .get_by_name::<NetworkMigrationSpec>(n)
                    .is_ok_and(|m| m.is_some())
            })
        } else {
            req.name.clone()
        };
        match engine::create_migration(
            &state.cluster,
            &name,
            req.spec.clone(),
            &guests,
            &allowed,
            &sees,
        )
        .await
        {
            Err(e)
                if req.name.is_empty()
                    && tries < 5
                    && matches!(e.code, "already_exists" | "name_taken") =>
            {
                continue;
            }
            other => break other?,
        }
    };
    if !req.labels.is_empty() {
        // 라벨은 만든 뒤 붙인다(엔진은 이름·스펙만 받는다).
        migration.revision = state
            .cluster
            .update_spec(
                migration.id,
                Some(migration.revision),
                req.labels.clone(),
                &migration.spec,
            )
            .await?;
        migration.labels = req.labels.clone();
    }
    audit.detail(format!(
        "network migrate {} → {} ({})",
        migration.spec.source,
        migration.spec.target,
        plan_summary(&plan)
    ));
    let warnings: usize =
        plan.nics.iter().map(|n| n.warnings.len()).sum::<usize>() + plan.bridge.warnings.len();
    let header = if warnings > 0 {
        vec![format!("the plan has {warnings} warning(s); see plan")]
    } else {
        Vec::new()
    };
    Ok((
        StatusCode::CREATED,
        warning_header(&header),
        Json(NetworkMigrationCreated { migration, plan }),
    )
        .into_response())
}

/// 걸음의 권한: 원본·대상 네트워크와 아직 끝나지 않은(옮길·되돌릴) VM마다 `vm.config`
fn require_step(
    state: &AppState,
    principal: &Principal,
    m: &NetworkMigration,
    vms: &[Uuid],
) -> Result<(), ApiError> {
    require_networks(state, principal, &m.spec)?;
    let pending: std::collections::BTreeSet<Uuid> = m
        .status
        .vms
        .iter()
        .filter(|v| v.mode != PlannedMode::Blocked)
        .filter(|v| !matches!(v.state, MoveState::RolledBack))
        .filter(|v| vms.is_empty() || vms.contains(&v.vm))
        .map(|v| v.vm)
        .collect();
    for id in pending {
        if let Some(vm) = state.cluster.get::<VmSpec>(id)? {
            state.authz.require(principal, Privilege::VmConfig, &vm)?;
        }
    }
    Ok(())
}

/// `POST /network-migrations/{key}/start`·`resume`
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationStartRequest {
    /// 다리 노드(이름이나 id)를 바꾼다(§13 — 다리 노드를 클러스터에서 지웠거나 그 노드가 꺼져 멈춘 옮기기를 다른 노드로
    /// 이어 간다). 놓인 다리가 온라인 노드에 있으면 바꿀 수 없다(409 `migration_bridge_laid`).
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub bridge_node: Option<String>,
}

/// 다리 노드를 바꾼다(스펙 `bridge_node`). 놓인 다리가 오프라인 노드에 있으면 그 기록을 지운다(지운 노드의 기록은
/// 엔진이 없는 것으로 본다).
async fn set_bridge_node(
    state: &AppState,
    m: &NetworkMigration,
    key: &str,
) -> Result<Option<String>, ApiError> {
    let node = lookup::<NodeSpec>(&state.cluster, key)
        .map_err(|_| invalid(format!("node {key:?} not found")))?;
    // 놓인 다리가 살아 있는 다른 노드에 있으면 바꾸지 않는다. 지운 노드·오프라인 노드면 바꾼다(리뷰 1 M5 — 꺼진 노드의
    // 다리는 이미 끊겼다. 그 노드가 돌아오면 에이전트가 기록에 없는 다리를 걷는다).
    let laid_elsewhere = match &m.status.bridge {
        Some(b) if b.node != node.id => state
            .cluster
            .get::<NodeSpec>(b.node)?
            .map(|n| n.status.online),
        _ => None,
    };
    if laid_elsewhere == Some(true) {
        return Err(ApiError::conflict(
            "migration_bridge_laid",
            format!(
                "network migration {:?} has its bridge on another node that is online; finish or roll back the migration first",
                m.name
            ),
        ));
    }
    if m.spec.bridge == Some(false) {
        return Err(invalid(format!(
            "network migration {:?} runs without a bridge",
            m.name
        )));
    }
    if m.spec.bridge_node != Some(node.id) {
        let mut spec = m.spec.clone();
        spec.bridge_node = Some(node.id);
        state
            .cluster
            .update_spec(m.id, Some(m.revision), m.labels.clone(), &spec)
            .await
            .map_err(|e| match e {
                ClusterError::Rejected(Rejection::RevisionMismatch { .. }) => ApiError::conflict(
                    "busy",
                    "the network migration changed while being updated; try again",
                ),
                e => e.into(),
            })?;
    }
    if laid_elsewhere == Some(false) {
        engine::forget_bridge(&state.cluster, m.id)
            .await
            .map_err(|e| ApiError::conflict("busy", e))?;
    }
    Ok(Some(node.name))
}

async fn start_or_resume(
    state: &AppState,
    principal: &Principal,
    audit: &AuditNote,
    key: &str,
    req: &MigrationStartRequest,
    what: &str,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_services(state)?;
    let m = lookup::<NetworkMigrationSpec>(&state.cluster, key)?;
    require_step(state, principal, &m, &[])?;
    let mut detail = format!("network migrate {} {what}", m.spec.source);
    if let Some(n) = &req.bridge_node
        && let Some(name) = set_bridge_node(state, &m, n).await?
    {
        detail.push_str(&format!(" --bridge-node {name}"));
    }
    let task = engine::start_step(
        &state.cluster,
        m.id,
        MigrationStep::Start,
        Vec::new(),
        None,
        state.node,
    )
    .await?;
    audit.detail(detail);
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 옮기기를 시작한다(`network-migrate` Task, 202): 다리를 놓고 VM NIC를 계획대로 옮긴다(VM마다 그때 다시 계획 — 달라진
/// VM은 `failed`). 원본 `network.config`·대상 `network.assign`·남은 VM마다 `vm.config`. 끝난 옮기기는 409
/// `migration_finished`, 이미 Task가 돌면 409 `migration_in_progress`.
#[utoipa::path(post, path = "/network-migrations/{key}/start", tag = "network-migrations",
    params(("key" = String, Path, description = "id or name")),
    request_body = MigrationStartRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn start_network_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<MigrationStartRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    start_or_resume(&state, &principal, &audit, &key, &req, "start").await
}

/// 멈췄거나 실패한 옮기기를 이어 간다(시작과 같은 Task — 옮긴 VM은 건너뛰고 `failed`는 다시 해 본다). 다리 노드를
/// 지워 멈췄으면 `bridge_node`로 다른 노드를 고른다.
#[utoipa::path(post, path = "/network-migrations/{key}/resume", tag = "network-migrations",
    params(("key" = String, Path, description = "id or name")),
    request_body = MigrationStartRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn resume_network_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<MigrationStartRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    start_or_resume(&state, &principal, &audit, &key, &req, "resume").await
}

/// 도는 옮기기를 멈춘다(지금 옮기는 VM이 끝난 뒤 Task가 멈춘다). `running`이 아니면 409 `migration_not_running`.
#[utoipa::path(post, path = "/network-migrations/{key}/pause", tag = "network-migrations",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<NetworkMigrationSpec, NetworkMigrationStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn pause_network_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<NetworkMigration>, ApiError> {
    let m = lookup::<NetworkMigrationSpec>(&state.cluster, &key)?;
    require_networks(&state, &principal, &m.spec)?;
    let m = engine::pause_migration(&state.cluster, m.id).await?;
    audit.detail(format!("network migrate {} pause", m.spec.source));
    Ok(Json(m))
}

/// `POST /network-migrations/{key}/finish`
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationFinishRequest {
    /// 다리를 걷은 뒤 대상 네트워크를 이 라우터(이름이나 id)의 인터페이스로 붙인다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub router: Option<String>,
}

/// 옮기기를 끝낸다(Task, 202 — §9.7): 다리를 걷고 `router`가 있으면 대상 네트워크를 그 라우터 인터페이스로 붙인다(라우터와
/// 대상 네트워크의 `network.config` — 라우터 `PATCH`와 같다). 원본에 남은 NIC는 Task 경고다.
#[utoipa::path(post, path = "/network-migrations/{key}/finish", tag = "network-migrations",
    params(("key" = String, Path, description = "id or name")),
    request_body = MigrationFinishRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn finish_network_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<MigrationFinishRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_services(&state)?;
    let m = lookup::<NetworkMigrationSpec>(&state.cluster, &key)?;
    require_networks(&state, &principal, &m.spec)?;
    let mut detail = format!("network migrate {} finish", m.spec.source);
    let router = match &req.router {
        Some(r) => {
            let router = lookup::<RouterSpec>(&state.cluster, r)
                .map_err(|_| invalid(format!("router {r:?} not found")))?;
            state
                .authz
                .require(&principal, Privilege::NetworkConfig, &router)?;
            state.authz.require_networks_with(
                &principal,
                Privilege::NetworkConfig,
                [m.spec.target.as_str()],
            )?;
            detail.push_str(&format!(" --router {}", router.name));
            Some(router.id.to_string())
        }
        None => None,
    };
    let task = engine::start_step(
        &state.cluster,
        m.id,
        MigrationStep::Finish,
        Vec::new(),
        router,
        state.node,
    )
    .await?;
    audit.detail(detail);
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// `POST /network-migrations/{key}/rollback`
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationRollbackRequest {
    /// 되돌릴 VM(이름이나 id). 비면 옮긴(또는 실패한) 모든 VM
    #[serde(default)]
    #[schema(default)]
    pub vms: Vec<String>,
}

/// VM을 원본 네트워크로 되돌린다(Task, 202 — §9.6): 켜진 VM은 라이브(MTU가 줄면 다시 켬), 꺼진 VM은 스펙만. 원본의
/// 보안 그룹·cloud-init 항목을 되살리고 주소를 놓는다. 모두 되돌리면 다리를 걷고 `rolled-back`. 옮기기에 없는 VM은 400.
#[utoipa::path(post, path = "/network-migrations/{key}/rollback", tag = "network-migrations",
    params(("key" = String, Path, description = "id or name")),
    request_body = MigrationRollbackRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn rollback_network_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<MigrationRollbackRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_services(&state)?;
    let m = lookup::<NetworkMigrationSpec>(&state.cluster, &key)?;
    let mut ids = Vec::new();
    let mut names = Vec::new();
    for v in &req.vms {
        let vm = lookup::<VmSpec>(&state.cluster, v)
            .map_err(|_| invalid(format!("VM {v:?} not found")))?;
        ids.push(vm.id);
        names.push(vm.name);
    }
    require_step(&state, &principal, &m, &ids)?;
    let task = engine::start_step(
        &state.cluster,
        m.id,
        MigrationStep::Rollback,
        ids,
        None,
        state.node,
    )
    .await?;
    let mut detail = format!("network migrate {} rollback", m.spec.source);
    if !names.is_empty() {
        detail.push(' ');
        detail.push_str(&names.join(" "));
    }
    audit.detail(detail);
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 옮기기 리소스를 지운다(끝난 기록도). 도는 중이면 409 `migration_in_progress`(먼저 `pause`). 놓인 다리는 함께 걷힌다
/// (OvnSync·다리 노드가 리소스를 따른다). 옮긴 VM은 그대로다.
#[utoipa::path(delete, path = "/network-migrations/{key}", tag = "network-migrations",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_network_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    let m = lookup::<NetworkMigrationSpec>(&state.cluster, &key)?;
    require_networks(&state, &principal, &m.spec)?;
    if let Some(task) = engine::live_task(&state.cluster, &m)? {
        return Err(ApiError::conflict(
            "migration_in_progress",
            format!(
                "network migration {:?} is running task {task}; pause it (and let the task stop) first",
                m.name
            ),
        ));
    }
    state
        .cluster
        .delete::<NetworkMigrationSpec>(m.id, Some(m.revision))
        .await?;
    crate::permissions::drop_orphans(&state.cluster).await;
    audit.detail(format!("network migration delete {}", m.name));
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::network_migration::{AddressSource, PlannedBridge, PlannedNic};

    #[test]
    fn plan_summaries_count_vms_and_modes() {
        let nic = |vm: Uuid, mode| PlannedNic {
            vm,
            name: "v".into(),
            nic: 0,
            mac: None,
            mode,
            ips: Vec::new(),
            address_source: AddressSource::Ipam,
            blockers: Vec::new(),
            warnings: Vec::new(),
        };
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let plan = MigrationPlan {
            nics: vec![
                nic(a, PlannedMode::Live),
                nic(b, PlannedMode::Live),
                nic(c, PlannedMode::Restart),
            ],
            bridge: PlannedBridge {
                needed: true,
                node: None,
                error: None,
                warnings: Vec::new(),
            },
            hidden: 0,
        };
        assert_eq!(plan_summary(&plan), "3 VMs, live 2, restart 1");
    }

    /// 리뷰 1 웹 I2: 이름을 주지 않은 옮기기는 쓰이지 않은 첫 이름(63자 안, `-`로 끝나지 않음)
    #[test]
    fn free_names_count_up_and_fit() {
        assert_eq!(pick_name("lan", "t-lan", |_| false), "lan-to-t-lan");
        let taken = ["lan-to-t-lan", "lan-to-t-lan-2"];
        assert_eq!(
            pick_name("lan", "t-lan", |n| taken.contains(&n)),
            "lan-to-t-lan-3"
        );
        let long = "a".repeat(60);
        let first = pick_name(&long, "bc", |_| false);
        assert_eq!(first.len(), 63, "{first}");
        let second = pick_name(&long, "bc", |n| n == first);
        assert!(second.len() <= 63 && second.ends_with("-2"), "{second}");
        // 잘린 끝의 `-`는 뺀다.
        let name = pick_name(&"x".repeat(58), "yy", |_| false);
        assert!(!name.ends_with('-'), "{name}");
        opl_types::resource::validate_name(&name).unwrap();
    }
}
