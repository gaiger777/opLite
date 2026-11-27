//! DR 오케스트레이션 API(계획 76 설계 §3): 복구 계획 `/dr-plans`(만들기·바꾸기·지우기 — 목록과 조회는 `resources`의 공통
//! 핸들러), 보고 `/dr-plans/{key}/report`, 실행 기록 `/dr-plans/{key}/runs`·`/dr-runs/{key}`, 실행 끝점
//! `/dr-plans/{key}/actions/{test|failover|cleanup|cancel|reprotect}`.
//!
//! - 권한: 읽기는 `cluster.read`(viewer), 쓰기·실행은 `cluster.admin`(종류 단위 기본 규칙 — 설계의 `dr.read`·admin에 가장
//!   가까운 것). 쓰기·실행은 모든 노드가 기능 `dr-plans`를 알 때만 받는다(409 `cluster_upgrading`, 검증보다 먼저).
//! - 실행 끝점은 요청을 보고 실행 리소스(`DrRun`, 시작한 때의 계획 사본과 [`DrRunStatus::start`]의 첫 상태)를 만들 뿐이다.
//!   진행은 리더의 DR 스케줄러가 한다. 계획 하나에 도는 실행은 하나다(상태 머신이 `dr-run` 독점 값으로 지킨다 — 409
//!   `dr_run_active`).
//! - `reprotect`는 여기서 끝난다: 장애 조치로 되살린 VM(라벨 `orbivisor.io/dr` = 계획)에 백업 작업
//!   `dr-reprotect-<plan>`(복제 `{remote, pool}`)을 만들거나 바꾼다.

pub(crate) mod protection;
pub(crate) mod scheduler;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_types::backup::BackupSpec;
use orbivisor_types::backup_job::{
    BackupJob, BackupJobSpec, BackupJobStatus, JobSchedule, Keep, ReplicateTarget, VmSelector,
    next_run,
};
use orbivisor_types::dr::{
    self, DEFAULT_MAX_PARALLEL, DR_LABEL, DR_TEST_LABEL, DrCleanupTarget, DrPhase, DrRun,
    DrRunKind, DrRunSpec, DrRunState, DrRunStatus, ERR_CLEANUP_REQUIRED, ERR_RUN_ACTIVE,
    ERR_SOURCE_MISSING, ERR_TEST_NETWORK_REQUIRED, MAX_RUNS_PER_PLAN, RecoveryPlan,
    RecoveryPlanSpec, RecoveryPlanStatus, VmProtection,
};
use orbivisor_types::network::NetworkSpec;
use orbivisor_types::node::{DR_PLANS, NodeSpec};
use orbivisor_types::remote::{Remote, RemoteSpec};
use orbivisor_types::resource::{Kind, Resource, ResourceSpec};
use orbivisor_types::security_group::SecurityGroupSpec;
use orbivisor_types::storage::StoragePoolSpec;
use orbivisor_types::vm::VmSpec;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::remote_client::RemoteClient;
use crate::resources::{CreateRequest, List, UpdateRequest, check_name, if_match, lookup};
use crate::state::AppState;

pub use protection::{DrProtection, DrProtectionConfig};
pub use scheduler::{DrScheduler, DrSchedulerConfig};

/// 계획된 이전 앞에 운영 쪽(원격)에 닿는지 보는 제한 시간
const REACH_TIMEOUT: Duration = Duration::from_secs(10);
/// 보고에 넣는 최근 시험 수
const REPORT_TESTS: usize = 3;

/// 원격이 없음(409 `dr_source_missing`)
fn source_missing(source: &str) -> ApiError {
    ApiError::conflict(
        ERR_SOURCE_MISSING,
        format!("remote {source:?} not found; register the production cluster as a remote first"),
    )
}

fn bad(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(e.to_string())
}

/// 계획 검사: 형식(400), 원격(409 `dr_source_missing`), 이 클러스터의 풀·노드·네트워크·보안 그룹 이름이 있는지(400).
/// 기능 검사(409)는 부르는 쪽이 이름·키 검사보다 먼저 한다. 폴더는 복원 때 본다(없으면 복원 기본 폴더에 둔다).
fn check_plan(state: &AppState, spec: &RecoveryPlanSpec) -> Result<(), ApiError> {
    spec.validate().map_err(bad)?;
    let c = &state.cluster;
    if c.get_by_name::<RemoteSpec>(&spec.source)?.is_none() {
        return Err(source_missing(&spec.source));
    }
    let pools: BTreeSet<&str> = std::iter::once(spec.pool.as_str())
        .chain(spec.vms().filter_map(|(_, v)| v.pool.as_deref()))
        .collect();
    for pool in pools {
        if c.get_by_name::<StoragePoolSpec>(pool)?.is_none() {
            return Err(bad(format!("storage pool {pool:?} not found")));
        }
    }
    let nodes: BTreeSet<&str> = spec.vms().filter_map(|(_, v)| v.node.as_deref()).collect();
    for node in nodes {
        if c.get_by_name::<NodeSpec>(node)?.is_none() {
            return Err(bad(format!("node {node:?} not found")));
        }
    }
    for (field, map) in [
        ("networks", &spec.networks),
        ("test_networks", &spec.test_networks),
    ] {
        for to in map.values() {
            if c.get_by_name::<NetworkSpec>(to)?.is_none() {
                return Err(bad(format!("{field}: network {to:?} not found")));
            }
        }
    }
    for to in spec.security_groups.values() {
        if c.get_by_name::<SecurityGroupSpec>(to)?.is_none() {
            return Err(bad(format!(
                "security_groups: security group {to:?} not found"
            )));
        }
    }
    Ok(())
}

/// 새 계획의 첫 보호 상태(리더의 반복 작업을 기다리지 않게)
fn first_status(state: &AppState, plan: &RecoveryPlan) -> Result<RecoveryPlanStatus, ApiError> {
    let backups = state.cluster.list::<BackupSpec>()?.1;
    let remotes = state.cluster.list::<RemoteSpec>()?.1;
    Ok(protection::recount(plan, &backups, &remotes, now()).unwrap_or_default())
}

/// 복구 계획을 만든다(설계 §3.1). 이름이 겹치면 409 `name_taken`, 원격이 없으면 409 `dr_source_missing`. 첫 보호 상태를
/// 곧 센다.
#[utoipa::path(post, path = "/dr-plans", tag = "dr",
    request_body = CreateRequest<RecoveryPlanSpec>,
    responses((status = 201, body = Resource<RecoveryPlanSpec, RecoveryPlanStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_plan(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateRequest<RecoveryPlanSpec>>,
) -> Result<(StatusCode, Json<RecoveryPlan>), ApiError> {
    state.authorize(&principal, Action::Write, RecoveryPlanSpec::KIND)?;
    require_feature(&state.cluster, DR_PLANS)?;
    check_name(&req.name)?;
    audit.detail(format!("dr-plan create {}", req.name));
    check_plan(&state, &req.spec)?;
    let mut plan: RecoveryPlan = Resource::new(req.name, req.spec);
    plan.labels = req.labels;
    plan.status = first_status(&state, &plan)?;
    plan.revision = state.cluster.create(&plan).await?;
    Ok((StatusCode::CREATED, Json(plan)))
}

/// 계획을 통째로 바꾼다. 도는 실행은 시작한 때의 계획 그대로 간다. 보호 상태는 리더가 곧 다시 센다.
#[utoipa::path(put, path = "/dr-plans/{key}", tag = "dr",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<RecoveryPlanSpec>,
    responses((status = 200, body = Resource<RecoveryPlanSpec, RecoveryPlanStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_plan(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    audit: AuditNote,
    Json(req): Json<UpdateRequest<RecoveryPlanSpec>>,
) -> Result<Json<RecoveryPlan>, ApiError> {
    state.authorize(&principal, Action::Write, RecoveryPlanSpec::KIND)?;
    require_feature(&state.cluster, DR_PLANS)?;
    let plan = lookup::<RecoveryPlanSpec>(&state.cluster, &key)?;
    audit.detail(format!("dr-plan update {}", plan.name));
    check_plan(&state, &req.spec)?;
    state
        .cluster
        .update_spec(plan.id, if_match(&headers)?, req.labels, &req.spec)
        .await?;
    Ok(Json(lookup::<RecoveryPlanSpec>(
        &state.cluster,
        &plan.id.to_string(),
    )?))
}

/// 계획과 그 실행 기록을 지운다. 도는 실행이 있으면 409 `dr_run_active`(취소하고 끝나기를 기다린다). 실행이 만든 VM·
/// 네트워크는 그대로 둔다.
#[utoipa::path(delete, path = "/dr-plans/{key}", tag = "dr",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_plan(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    audit: AuditNote,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, RecoveryPlanSpec::KIND)?;
    require_feature(&state.cluster, DR_PLANS)?;
    let plan = lookup::<RecoveryPlanSpec>(&state.cluster, &key)?;
    audit.detail(format!("dr-plan delete {}", plan.name));
    let runs = runs_of(&state.cluster, plan.id)?;
    if let Some(run) = active(&runs) {
        return Err(run_active(&plan.name, run));
    }
    state
        .cluster
        .delete::<RecoveryPlanSpec>(plan.id, if_match(&headers)?)
        .await?;
    for run in runs {
        delete_run(&state.cluster, &run).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---- 실행 기록 ----

/// 계획의 실행(새것부터)
fn runs_of(cluster: &Cluster, plan: Uuid) -> Result<Vec<DrRun>, ApiError> {
    let mut runs: Vec<DrRun> = cluster
        .list::<DrRunSpec>()?
        .1
        .into_iter()
        .filter(|r| r.spec.plan == plan)
        .collect();
    runs.sort_by_key(|r| std::cmp::Reverse((r.status.started_at, r.id)));
    Ok(runs)
}

/// 도는 실행
fn active(runs: &[DrRun]) -> Option<&DrRun> {
    runs.iter().find(|r| !r.status.state.is_finished())
}

fn run_active(plan: &str, run: &DrRun) -> ApiError {
    ApiError::conflict(
        ERR_RUN_ACTIVE,
        format!(
            "plan {plan:?} has a running {} run {:?}; cancel it or wait for it to finish",
            kind_word(run.spec.kind),
            run.name
        ),
    )
}

async fn delete_run(cluster: &Cluster, run: &DrRun) {
    match cluster.delete::<DrRunSpec>(run.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
        Err(e) => tracing::warn!(run = %run.name, error = %e, "cannot delete a DR run"),
    }
}

/// 계획의 실행 기록(새것부터, 계획마다 최근 20개)
#[utoipa::path(get, path = "/dr-plans/{key}/runs", tag = "dr",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = List<Resource<DrRunSpec, DrRunStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_runs(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<List<DrRun>>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::DrRun)?;
    let plan = lookup::<RecoveryPlanSpec>(&state.cluster, &key)?;
    let revision = state.cluster.revision();
    Ok(Json(List {
        revision,
        items: runs_of(&state.cluster, plan.id)?,
    }))
}

/// 실행 하나(단계마다 그룹·VM·복구 지점·단계·새 VM·Task·오류)
#[utoipa::path(get, path = "/dr-runs/{key}", tag = "dr",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<DrRunSpec, DrRunStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_run(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<DrRun>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::DrRun)?;
    Ok(Json(lookup::<DrRunSpec>(&state.cluster, &key)?))
}

// ---- 보고 ----

/// 실행 하나의 요약(보고)
#[derive(Debug, Serialize, ToSchema)]
pub struct DrRunSummary {
    pub id: Uuid,
    pub name: String,
    pub kind: DrRunKind,
    pub state: DrRunState,
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub finished_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub rto_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub prepare_secs: Option<u64>,
    /// `failed`·`timed_out`으로 끝난 VM(원본 이름)
    pub failed_vms: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<String>,
}

fn summary(run: &DrRun) -> DrRunSummary {
    DrRunSummary {
        id: run.id,
        name: run.name.clone(),
        kind: run.spec.kind,
        state: run.status.state,
        started_at: run.status.started_at,
        finished_at: run.status.finished_at,
        rto_secs: run.status.rto_secs,
        prepare_secs: run.status.prepare_secs,
        failed_vms: run
            .status
            .steps
            .iter()
            .filter(|s| matches!(s.phase, DrPhase::Failed | DrPhase::TimedOut))
            .map(|s| s.vm.clone())
            .collect(),
        error: run.status.error.clone(),
    }
}

/// `GET /dr-plans/{key}/report`(설계 §3.5)
#[derive(Debug, Serialize, ToSchema)]
pub struct DrReport {
    pub plan: String,
    pub plan_id: Uuid,
    /// 운영 클러스터(원격 이름)
    pub source: String,
    /// 원격이 등록돼 있는지(없으면 모든 VM이 복구 지점 없음)
    pub source_registered: bool,
    pub rpo_target_secs: u64,
    /// 보고를 센 시각(유닉스 초) — 아래 나이는 이때의 것이다.
    pub at: u64,
    /// 계획의 VM 수와 그 가운데 RPO를 지키는(`ok`) 수
    pub total: usize,
    pub protected: usize,
    /// VM마다의 복구 지점(그룹 차례, 지금 다시 셈)
    pub protection: Vec<VmProtection>,
    /// 최근 끝난 시험(새것부터 셋까지)
    pub tests: Vec<DrRunSummary>,
    /// 마지막으로 끝난 장애 조치
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub last_failover: Option<DrRunSummary>,
    /// 지금 도는 실행
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub running: Option<DrRunSummary>,
}

/// 계획의 RPO/RTO 보고: VM별 보호 상태(복구 지점·나이·RPO 목표 — 지금 다시 셈), 최근 시험 셋의 RTO·결과·실패 VM,
/// 지난 장애 조치, 도는 실행.
#[utoipa::path(get, path = "/dr-plans/{key}/report", tag = "dr",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = DrReport), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn plan_report(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<DrReport>, ApiError> {
    state.authorize(&principal, Action::Read, RecoveryPlanSpec::KIND)?;
    let plan = lookup::<RecoveryPlanSpec>(&state.cluster, &key)?;
    let backups = state.cluster.list::<BackupSpec>()?.1;
    let remotes = state.cluster.list::<RemoteSpec>()?.1;
    let ca = protection::source_ca(&plan.spec, &remotes);
    let at = now();
    let protection = dr::protection(&plan.spec, &backups, ca, at);
    let runs = runs_of(&state.cluster, plan.id)?;
    let finished = |r: &&DrRun| r.status.state.is_finished();
    Ok(Json(DrReport {
        plan: plan.name.clone(),
        plan_id: plan.id,
        source: plan.spec.source.clone(),
        source_registered: ca.is_some(),
        rpo_target_secs: plan.spec.rpo_target_secs,
        at,
        total: protection.len(),
        protected: protection.iter().filter(|p| p.ok).count(),
        protection,
        tests: runs
            .iter()
            .filter(finished)
            .filter(|r| r.spec.kind == DrRunKind::Test)
            .take(REPORT_TESTS)
            .map(summary)
            .collect(),
        last_failover: runs
            .iter()
            .filter(finished)
            .find(|r| r.spec.kind.is_failover())
            .map(summary),
        running: active(&runs).map(summary),
    }))
}

// ---- 실행 끝점 ----

fn kind_word(kind: DrRunKind) -> &'static str {
    match kind {
        DrRunKind::Test => "test",
        DrRunKind::FailoverPlanned | DrRunKind::FailoverDisaster => "failover",
        DrRunKind::Cleanup => "cleanup",
    }
}

/// 실행 끝점의 계획: 기능(409), 계획(404), 도는 실행(409 `dr_run_active`)
fn plan_for_action(
    state: &AppState,
    principal: &Principal,
    key: &str,
) -> Result<(RecoveryPlan, Vec<DrRun>), ApiError> {
    state.authorize(principal, Action::Operate, RecoveryPlanSpec::KIND)?;
    require_feature(&state.cluster, DR_PLANS)?;
    let plan = lookup::<RecoveryPlanSpec>(&state.cluster, key)?;
    let runs = runs_of(&state.cluster, plan.id)?;
    Ok((plan, runs))
}

fn no_active(plan: &RecoveryPlan, runs: &[DrRun]) -> Result<(), ApiError> {
    match active(runs) {
        Some(run) => Err(run_active(&plan.name, run)),
        None => Ok(()),
    }
}

/// 계획의 원격(없으면 409 `dr_source_missing`)
fn source_of(state: &AppState, plan: &RecoveryPlan) -> Result<Remote, ApiError> {
    state
        .cluster
        .get_by_name::<RemoteSpec>(&plan.spec.source)?
        .ok_or_else(|| source_missing(&plan.spec.source))
}

/// 지난 시험이 남긴 것: 시험 라벨(`orbivisor.io/dr-test` = 계획)이 붙은 VM과 시험 실행이 정한 시험 VM·거품 네트워크
/// 가운데 아직 있는 것
fn leftovers(
    state: &AppState,
    plan: &RecoveryPlan,
    runs: &[DrRun],
) -> Result<DrCleanupTarget, ApiError> {
    let c = &state.cluster;
    let vms = c.list::<VmSpec>()?.1;
    let networks = c.list::<NetworkSpec>()?.1;
    let tests: Vec<&DrRun> = runs
        .iter()
        .filter(|r| r.spec.kind == DrRunKind::Test)
        .collect();
    let mut want_vms: BTreeSet<String> = vms
        .iter()
        .filter(|v| v.labels.get(DR_TEST_LABEL) == Some(&plan.name))
        .map(|v| v.name.clone())
        .collect();
    let mut want_nets = BTreeSet::new();
    for run in &tests {
        let t = run.status.cleanup_target(run.id);
        want_vms.extend(
            t.vms
                .into_iter()
                .filter(|n| vms.iter().any(|v| v.name == *n)),
        );
        want_nets.extend(
            run.status
                .bubbles
                .iter()
                .map(|b| b.name.clone())
                .filter(|n| networks.iter().any(|x| x.name == *n)),
        );
    }
    Ok(DrCleanupTarget {
        run: tests.first().map_or(Uuid::nil(), |r| r.id),
        vms: want_vms.into_iter().collect(),
        networks: want_nets.into_iter().collect(),
    })
}

/// 실행 리소스를 만들고 계획의 `status.run`에 적는다. 같은 계획의 다른 실행이 그사이 시작했으면 409 `dr_run_active`
/// (상태 머신의 독점 값). 오래된 끝난 실행은 계획마다 [`MAX_RUNS_PER_PLAN`]개만 남긴다.
async fn start_run(
    state: &AppState,
    plan: &RecoveryPlan,
    kind: DrRunKind,
    cleanup: Option<DrCleanupTarget>,
) -> Result<DrRun, ApiError> {
    let c = &state.cluster;
    let spec = DrRunSpec {
        plan: plan.id,
        plan_name: plan.name.clone(),
        kind,
        plan_spec: plan.spec.clone(),
        // 설정 `dr_max_parallel`(새 실행부터 — 도는 실행은 시작한 때의 값)
        max_parallel: crate::settings::current(c)
            .map_or(DEFAULT_MAX_PARALLEL, |s| s.spec.dr_max_parallel()),
        cleanup,
    };
    let backups = c.list::<BackupSpec>()?.1;
    let remotes = c.list::<RemoteSpec>()?.1;
    let at = now();
    let status = DrRunStatus::start(
        &spec,
        &backups,
        protection::source_ca(&plan.spec, &remotes),
        at,
    );
    let base = dr::run_name(&plan.name, kind_word(kind), at);
    let mut run: DrRun = Resource::new(base.clone(), spec);
    run.status = status;
    for n in 2.. {
        match c.create(&run).await {
            Ok(revision) => {
                run.revision = revision;
                break;
            }
            // 같은 초에 같은 종류(취소한 시험 뒤의 뒷정리 등): 꼬리에 수를 붙인다.
            Err(ClusterError::Rejected(Rejection::NameTaken { .. })) if n < 10 => {
                run.name = dr::run_name(&plan.name, kind_word(kind), at * 10 + n);
            }
            Err(e) => return Err(e.into()),
        }
    }
    claim(c, plan.id, run.id).await;
    prune(c, plan.id).await;
    Ok(run)
}

/// 계획의 `status.run`을 `run`으로(보호 상태를 쓰는 리더와 겹치면 다시 읽어 쓴다). 실패해도 실행은 그대로 간다 — 지금
/// 실행은 실행 기록이 정한다.
async fn claim(cluster: &Cluster, plan: Uuid, run: Uuid) {
    for _ in 0..10 {
        let Ok(Some(p)) = cluster.get::<RecoveryPlanSpec>(plan) else {
            return;
        };
        let status = RecoveryPlanStatus {
            run: Some(run),
            ..p.status.clone()
        };
        match cluster
            .update_status_checked::<RecoveryPlanSpec>(p.id, p.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => {
                tracing::warn!(plan = %p.name, error = %e, "cannot record the DR run on the plan");
                return;
            }
            Ok(_) => return,
        }
    }
}

/// 끝난 실행 가운데 오래된 것을 지워 계획마다 [`MAX_RUNS_PER_PLAN`]개만 남긴다.
async fn prune(cluster: &Cluster, plan: Uuid) {
    let Ok(runs) = runs_of(cluster, plan) else {
        return;
    };
    for run in runs
        .iter()
        .skip(MAX_RUNS_PER_PLAN)
        .filter(|r| r.status.state.is_finished())
    {
        delete_run(cluster, run).await;
    }
}

/// 시험 장애 조치를 시작한다(설계 §3.2): 운영 쪽을 건드리지 않고 시험 VM(`<vm>-drt`, 폴더 `dr-test/<plan>`)을 격리망
/// (`test_networks` 맵, 없는 원본 네트워크는 거품 SDN 네트워크)에서 되살린다. 도는 실행이 있으면 409 `dr_run_active`,
/// 지난 시험의 VM·네트워크가 남았으면 409 `dr_cleanup_required`(먼저 `cleanup`), 원격이 없으면 409 `dr_source_missing`,
/// 거품 네트워크가 필요한데 SDN이 세워지지 않았으면 400 `dr_test_network_required`.
#[utoipa::path(post, path = "/dr-plans/{key}/actions/test", tag = "dr",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<DrRunSpec, DrRunStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn test_plan(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<DrRun>), ApiError> {
    let (plan, runs) = plan_for_action(&state, &principal, &key)?;
    audit.detail(format!("dr-plan test {}", plan.name));
    no_active(&plan, &runs)?;
    let source = source_of(&state, &plan)?;
    let left = leftovers(&state, &plan, &runs)?;
    if !left.vms.is_empty() || !left.networks.is_empty() {
        let mut what: Vec<String> = left.vms.iter().map(|v| format!("VM {v}")).collect();
        what.extend(left.networks.iter().map(|n| format!("network {n}")));
        return Err(ApiError::conflict(
            ERR_CLEANUP_REQUIRED,
            format!(
                "the last test of plan {:?} left {}; run cleanup first",
                plan.name,
                what.join(", ")
            ),
        ));
    }
    let backups = state.cluster.list::<BackupSpec>()?.1;
    let bubbles = dr::bubble_sources(&plan.spec, &backups, Some(&source.spec.ca_fingerprint));
    if !bubbles.is_empty() && crate::sdn::require_ready(&state.cluster).is_err() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            ERR_TEST_NETWORK_REQUIRED,
            format!(
                "SDN is not set up, so a test cannot make isolated networks; map {} in test_networks",
                bubbles.join(", ")
            ),
        ));
    }
    let run = start_run(&state, &plan, DrRunKind::Test, None).await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FailoverMode {
    /// 운영 쪽이 살아 있음: 운영 VM을 끄고 마지막 백업을 이 클러스터로 보낸 뒤 넘긴다.
    Planned,
    /// 운영 쪽이 없음: 지금 있는 가장 새 사본으로 곧장
    Disaster,
}

/// `POST /dr-plans/{key}/actions/failover` 본문
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FailoverRequest {
    pub mode: FailoverMode,
    /// 계획 이름(실수로 누르지 않게 그대로 다시 친다)
    pub confirm: String,
}

/// 운영 쪽(원격)에 닿는지 본다(저장한 CA와 토큰으로 `GET /cluster`, 10초 제한).
async fn reach(remote: &Remote) -> Result<(), String> {
    let work = async {
        let client = RemoteClient::new(&remote.spec.url, &remote.spec.ca_pem, &remote.spec.token)
            .map_err(|e| e.to_string())?;
        client
            .get_json::<serde_json::Value>("/cluster")
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    tokio::time::timeout(REACH_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| Err("timed out".into()))
}

/// 실제 장애 조치를 시작한다(설계 §3.2). `confirm`이 계획 이름과 다르면 400 `dr_confirm_mismatch`. `planned`는 운영 쪽에
/// 닿아야 한다(409 `dr_source_unreachable`). 도는 실행이 있으면 409 `dr_run_active`, 원격이 없으면 409
/// `dr_source_missing`.
#[utoipa::path(post, path = "/dr-plans/{key}/actions/failover", tag = "dr",
    params(("key" = String, Path, description = "id or name")),
    request_body = FailoverRequest,
    responses((status = 202, body = Resource<DrRunSpec, DrRunStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn failover_plan(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<FailoverRequest>,
) -> Result<(StatusCode, Json<DrRun>), ApiError> {
    let (plan, runs) = plan_for_action(&state, &principal, &key)?;
    let mode = match req.mode {
        FailoverMode::Planned => "planned",
        FailoverMode::Disaster => "disaster",
    };
    audit.detail(format!("dr-plan failover {} {mode}", plan.name));
    if req.confirm != plan.name {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "dr_confirm_mismatch",
            format!("confirm must be the plan name {:?}", plan.name),
        ));
    }
    no_active(&plan, &runs)?;
    let source = source_of(&state, &plan)?;
    let kind = match req.mode {
        FailoverMode::Planned => {
            if let Err(e) = reach(&source).await {
                return Err(ApiError::conflict(
                    "dr_source_unreachable",
                    format!(
                        "a planned failover needs the production cluster {:?}, which cannot be reached: {e}; use mode disaster if it is down",
                        source.name
                    ),
                ));
            }
            DrRunKind::FailoverPlanned
        }
        FailoverMode::Disaster => DrRunKind::FailoverDisaster,
    };
    let run = start_run(&state, &plan, kind, None).await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// 지난 시험이 남긴 시험 VM(끄고 지움)과 거품 네트워크를 지우는 실행을 시작한다. 장애 조치로 만든 VM은 건드리지 않는다.
/// 남은 것이 없으면 409 `dr_nothing_to_clean`, 도는 실행이 있으면 409 `dr_run_active`.
#[utoipa::path(post, path = "/dr-plans/{key}/actions/cleanup", tag = "dr",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<DrRunSpec, DrRunStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn cleanup_plan(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<DrRun>), ApiError> {
    let (plan, runs) = plan_for_action(&state, &principal, &key)?;
    audit.detail(format!("dr-plan cleanup {}", plan.name));
    no_active(&plan, &runs)?;
    let target = leftovers(&state, &plan, &runs)?;
    if target.vms.is_empty() && target.networks.is_empty() {
        return Err(ApiError::conflict(
            "dr_nothing_to_clean",
            format!("no test VMs or networks of plan {:?} are left", plan.name),
        ));
    }
    let run = start_run(&state, &plan, DrRunKind::Cleanup, Some(target)).await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// 도는 실행을 지금 단계 뒤에 멈추게 한다(진행 중인 Task는 끝까지, 시험이면 그 뒤 뒷정리). 도는 실행이 없으면 409
/// `dr_no_active_run`.
#[utoipa::path(post, path = "/dr-plans/{key}/actions/cancel", tag = "dr",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<DrRunSpec, DrRunStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn cancel_plan(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<DrRun>, ApiError> {
    let (plan, _) = plan_for_action(&state, &principal, &key)?;
    audit.detail(format!("dr-plan cancel {}", plan.name));
    let c = &state.cluster;
    for _ in 0..10 {
        let runs = runs_of(c, plan.id)?;
        let Some(run) = active(&runs) else {
            return Err(ApiError::conflict(
                "dr_no_active_run",
                format!("plan {:?} has no running run", plan.name),
            ));
        };
        if run.status.cancel_requested {
            return Ok(Json(run.clone()));
        }
        let status = DrRunStatus {
            cancel_requested: true,
            ..run.status.clone()
        };
        match c
            .update_status_checked::<DrRunSpec>(run.id, run.revision, &status)
            .await
        {
            // 그사이 스케줄러가 실행을 적었다: 다시 읽는다.
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => return Ok(Json(lookup::<DrRunSpec>(c, &run.id.to_string())?)),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the DR run kept changing while being cancelled; try again",
    ))
}

/// `POST /dr-plans/{key}/actions/reprotect` 본문(설계 §3.6)
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReprotectRequest {
    /// 복제를 받을 원격(보통 되살아난 운영 클러스터)
    pub remote: String,
    /// 그 원격의 백업 풀
    pub pool: String,
    pub schedule: JobSchedule,
    /// 이 클러스터에서 백업을 쓸 백업 풀. 없으면 계획의 `pool`(백업 풀이어야 한다)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub backup_pool: Option<String>,
    #[serde(default)]
    #[schema(default)]
    pub keep: Keep,
}

/// 되돌리기 준비(설계 §3.6): 장애 조치로 되살린 VM(라벨 `orbivisor.io/dr` = 계획)에 백업 작업 `dr-reprotect-<plan>`
/// (라벨로 고름, 복제 `{remote, pool}`)을 만든다. 이미 있으면 이 값으로 바꾼다(201 대신 200). 되살린 VM이 없으면 409
/// `dr_no_recovered_vms`. 원격·풀 검사는 백업 작업과 같다(400).
#[utoipa::path(post, path = "/dr-plans/{key}/actions/reprotect", tag = "dr",
    params(("key" = String, Path, description = "id or name")),
    request_body = ReprotectRequest,
    responses((status = 201, body = Resource<BackupJobSpec, BackupJobStatus>), (status = 200, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn reprotect_plan(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<ReprotectRequest>,
) -> Result<(StatusCode, Json<BackupJob>), ApiError> {
    state.authorize(&principal, Action::Operate, RecoveryPlanSpec::KIND)?;
    require_feature(&state.cluster, DR_PLANS)?;
    let c = &state.cluster;
    let plan = lookup::<RecoveryPlanSpec>(c, &key)?;
    audit.detail(format!("dr-plan reprotect {} -> {}", plan.name, req.remote));
    let recovered = c
        .list::<VmSpec>()?
        .1
        .iter()
        .filter(|v| v.labels.get(DR_LABEL) == Some(&plan.name))
        .count();
    if recovered == 0 {
        return Err(ApiError::conflict(
            "dr_no_recovered_vms",
            format!(
                "no VMs recovered by plan {:?} are here; run a failover first",
                plan.name
            ),
        ));
    }
    let mut spec = BackupJobSpec {
        vms: VmSelector::Labels([(DR_LABEL.to_string(), plan.name.clone())].into()),
        pool: req.backup_pool.unwrap_or_else(|| plan.spec.pool.clone()),
        store: None,
        schedule: req.schedule,
        keep: req.keep,
        enabled: true,
        replicate: Some(ReplicateTarget {
            remote: req.remote,
            pool: req.pool,
        }),
        full_every: 0,
        copy_to: None,
    };
    crate::backup_jobs::check_spec(&state, &mut spec)?;
    let name = dr::reprotect_job_name(&plan.name);
    let labels = [(DR_LABEL.to_string(), plan.name.clone())].into();
    if let Some(job) = c.get_by_name::<BackupJobSpec>(&name)? {
        if job.spec != spec || job.labels != labels {
            let reschedule = job.spec.schedule != spec.schedule || !job.spec.enabled;
            c.update_spec(job.id, Some(job.revision), labels, &spec)
                .await?;
            if reschedule {
                let job = lookup::<BackupJobSpec>(c, &job.id.to_string())?;
                let status = BackupJobStatus {
                    next_run: Some(next_run(&spec.schedule, now())),
                    ..job.status.clone()
                };
                c.update_status::<BackupJobSpec>(job.id, &status).await?;
            }
        }
        return Ok((
            StatusCode::OK,
            Json(lookup::<BackupJobSpec>(c, &job.id.to_string())?),
        ));
    }
    let mut job: BackupJob = Resource::new(name, spec);
    job.labels = labels;
    job.status.next_run = Some(next_run(&job.spec.schedule, now()));
    job.revision = c.create(&job).await?;
    Ok((StatusCode::CREATED, Json(job)))
}
