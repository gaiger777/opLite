//! 백업 작업 만들기·바꾸기·지우기·지금 실행(설계 §3). 목록과 조회는 `resources`의 공통 핸들러다.
//! 쓰기는 백업과 같은 권한(operator 이상)이다. 작업을 지워도 그 작업이 만든 백업은 남는다.
//!
//! [`BackupJobScheduler`]는 리더에서 `tick`마다 작업을 이름 순으로 보며 실행을 시작하고, 대상 VM을
//! 하나씩 꺼내 백업을 요청하고(노드당 동시 백업 Task 수 제한), 실행이 끝나면 결과를 적고 보존 규칙대로
//! 지운다. 진행 상태는 작업의 `status.run`에 있으므로 리더가 바뀌어도 이어 간다. 리더가 바뀌는 순간
//! 두 스케줄러가 잠깐 겹치거나 상태 쓰기가 실패해도, 이번 실행이 이미 만든 VM의 백업은 다시 만들지 않고
//! 받아들인다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::backup::{Backup, BackupPhase, BackupSpec, default_backup_name};
use opl_types::backup_job::{
    BackupJob, BackupJobSpec, BackupJobStatus, JobError, JobRun, JobRunResult, JobSchedule, Keep,
    ReplicateTarget, VmSelector, next_run, prune,
};
use opl_types::remote::RemoteSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::StoragePoolSpec;
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{
    check_backup, files_node, find_pool, now, start_backup, start_backup_delete,
    start_backup_replicate,
};
use crate::cluster::require_current;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, check_name, if_match, lookup};
use crate::state::AppState;

/// 스펙 검증과 풀 검사: 풀이 있고 백업 풀이어야 한다(400). 요일은 주의 순서로 정리한다. 원격 복제를
/// 켠 작업은 옛 노드가 풀지 못하므로 모든 노드가 이 빌드일 때만 쓰고(409 `cluster_upgrading`), 원격은
/// 등록돼 있어야 한다(400). 원격의 풀은 보낼 때 원격이 확인한다.
fn check_spec(state: &AppState, spec: &mut BackupJobSpec) -> Result<(), ApiError> {
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    spec.schedule.days.sort();
    let pool = state
        .cluster
        .get_by_name::<StoragePoolSpec>(&spec.pool)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {:?} not found", spec.pool)))?;
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    if let Some(target) = &spec.replicate {
        require_current(&state.cluster)?;
        if state
            .cluster
            .get_by_name::<RemoteSpec>(&target.remote)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "remote {:?} not found",
                target.remote
            )));
        }
    }
    Ok(())
}

/// 작업을 만들고 다음 실행 시각(`status.next_run`)을 적는다. 이름이 겹치면 409 `name_taken`.
#[utoipa::path(post, path = "/backup-jobs", tag = "backups",
    request_body = CreateRequest<BackupJobSpec>,
    responses((status = 201, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(mut req): Json<CreateRequest<BackupJobSpec>>,
) -> Result<(StatusCode, Json<BackupJob>), ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    check_name(&req.name)?;
    check_spec(&state, &mut req.spec)?;
    let mut job: BackupJob = Resource::new(req.name, req.spec);
    job.labels = req.labels;
    job.status.next_run = Some(next_run(&job.spec.schedule, now()));
    job.revision = state.cluster.create(&job).await?;
    Ok((StatusCode::CREATED, Json(job)))
}

/// 백업 작업에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupJobPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub vms: Option<VmSelector>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub pool: Option<String>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub schedule: Option<JobSchedule>,
    /// 보존 규칙 전체를 이것으로 바꾼다(적지 않은 단위는 0).
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub keep: Option<Keep>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub enabled: Option<bool>,
    /// 만든 백업을 보낼 원격의 백업 풀. `null`이면 보내지 않는다.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<ReplicateTarget>, nullable, default)]
    pub replicate: Option<Option<ReplicateTarget>>,
}

/// 적은 값은 `null`이어도 `Some`이다(적지 않으면 `serde(default)`의 `None`).
fn present<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// 스펙의 주어진 값을 바꾼다. 아무것도 없으면 400. 일정을 바꾸거나 작업을 다시 켜면 다음 실행 시각을
/// 지금부터 다시 계산한다(꺼 둔 사이 지난 실행은 하지 않는다). 다만 켜 둔 작업의 지금 실행 요청이
/// 기다리고 있으면(다음 실행이 지났으면) 그대로 둔다. 진행 중인 실행은 그대로 끝까지 간다.
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
#[utoipa::path(patch, path = "/backup-jobs/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = BackupJobPatch,
    responses((status = 200, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<BackupJobPatch>,
) -> Result<Json<BackupJob>, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    if req.vms.is_none()
        && req.pool.is_none()
        && req.schedule.is_none()
        && req.keep.is_none()
        && req.enabled.is_none()
        && req.replicate.is_none()
    {
        return Err(ApiError::bad_request(
            "give vms, pool, schedule, keep, enabled or replicate",
        ));
    }
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let job = lookup::<BackupJobSpec>(&state.cluster, &key)?;
        if let Some(expected) = expected
            && expected != job.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: job.revision,
            })
            .into());
        }
        let mut spec = job.spec.clone();
        if let Some(vms) = &req.vms {
            spec.vms = vms.clone();
        }
        if let Some(pool) = &req.pool {
            spec.pool = pool.clone();
        }
        if let Some(schedule) = &req.schedule {
            spec.schedule = schedule.clone();
        }
        if let Some(keep) = req.keep {
            spec.keep = keep;
        }
        if let Some(enabled) = req.enabled {
            spec.enabled = enabled;
        }
        if let Some(replicate) = &req.replicate {
            spec.replicate = replicate.clone();
        }
        check_spec(&state, &mut spec)?;
        if spec == job.spec {
            return Ok(Json(job));
        }
        let now = now();
        let requested = job.spec.enabled && job.status.next_run.is_some_and(|t| t <= now);
        let reschedule = (spec.schedule != job.spec.schedule
            || (spec.enabled && !job.spec.enabled))
            && !requested;
        // 다음 실행을 스펙보다 먼저 쓴다. 스펙 쓰기가 실패해도 스케줄러는 새 다음 실행(앞날)만 보므로
        // 옛 일정이나 꺼 둔 사이 지난 실행을 바로 돌리지 않는다.
        let mut revision = job.revision;
        if reschedule {
            let status = BackupJobStatus {
                next_run: Some(next_run(&spec.schedule, now)),
                ..job.status.clone()
            };
            match state
                .cluster
                .update_status_checked::<BackupJobSpec>(job.id, revision, &status)
                .await
            {
                Ok(r) => revision = r,
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                    if expected.is_none() =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
        match state
            .cluster
            .update_spec(job.id, Some(revision), job.labels.clone(), &spec)
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
        return Ok(Json(lookup::<BackupJobSpec>(
            &state.cluster,
            &job.id.to_string(),
        )?));
    }
    Err(ApiError::conflict(
        "busy",
        "the backup job kept changing while being updated; try again",
    ))
}

/// 작업의 상태를 `change`가 돌려준 값으로 쓰고 쓴 작업을 돌려준다. 그 사이 스케줄러가 상태를 썼으면
/// 다시 읽어 바꾼다.
async fn update_status(
    state: &AppState,
    id: Uuid,
    change: impl Fn(&BackupJob) -> Result<BackupJobStatus, ApiError>,
) -> Result<BackupJob, ApiError> {
    for _ in 0..10 {
        let job = lookup::<BackupJobSpec>(&state.cluster, &id.to_string())?;
        let status = change(&job)?;
        match state
            .cluster
            .update_status_checked::<BackupJobSpec>(job.id, job.revision, &status)
            .await
        {
            Ok(_) => return lookup::<BackupJobSpec>(&state.cluster, &id.to_string()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the backup job kept changing while being updated; try again",
    ))
}

/// 작업만 지운다. 작업이 만든 백업은 남는다(백업의 `job`도 그대로).
#[utoipa::path(delete, path = "/backup-jobs/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    let job = lookup::<BackupJobSpec>(&state.cluster, &key)?;
    state
        .cluster
        .delete::<BackupJobSpec>(job.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 다음 실행 시각을 지금으로 당긴다. 리더의 스케줄러가 다음 차례에 실행을 시작한다. 실행이 진행
/// 중이면 409 `job_running`, 꺼 둔 작업이면 409 `job_disabled`.
#[utoipa::path(post, path = "/backup-jobs/{key}/run", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn run_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<BackupJob>, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    let id = lookup::<BackupJobSpec>(&state.cluster, &key)?.id;
    let job = update_status(&state, id, |job| {
        if job.status.run.is_some() {
            return Err(ApiError::conflict(
                "job_running",
                format!("backup job {:?} is already running", job.name),
            ));
        }
        if !job.spec.enabled {
            return Err(ApiError::conflict(
                "job_disabled",
                format!("backup job {:?} is disabled; enable it first", job.name),
            ));
        }
        Ok(BackupJobStatus {
            next_run: Some(now()),
            ..job.status.clone()
        })
    })
    .await?;
    Ok(Json(job))
}

#[derive(Debug, Clone)]
pub struct BackupJobSchedulerConfig {
    /// 작업을 확인하는 간격
    pub tick: Duration,
    /// 노드마다 동시에 도는 백업 Task의 최대 수. 손으로 요청한 백업과 복제 Task도 센다.
    pub max_concurrent: usize,
}

impl Default for BackupJobSchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            max_concurrent: 2,
        }
    }
}

/// 백업 작업 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct BackupJobScheduler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl BackupJobScheduler {
    pub fn spawn(state: Arc<AppState>, config: BackupJobSchedulerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    state: Arc<AppState>,
    config: BackupJobSchedulerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config) => {}
        }
    }
}

async fn tick(state: &AppState, config: &BackupJobSchedulerConfig) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok((_, mut jobs)) = cluster.list::<BackupJobSpec>() else {
        return;
    };
    jobs.sort_by(|a, b| a.name.cmp(&b.name));
    let now = now();
    for job in jobs {
        // 쓰기가 엇갈렸거나(revision) 저장소 오류면 다음 tick에 다시 본다.
        if let Err(e) = advance(state, config, job.clone(), now).await {
            tracing::warn!(job = %job.name, error = %e.message, "cannot advance backup job");
        }
        if let Err(e) = prune_after_run(state, job.id).await {
            tracing::warn!(job = %job.name, error = %e.message, "cannot prune backups of job");
        }
        if let Err(e) = replicate(state, config, job.id).await {
            tracing::warn!(job = %job.name, error = %e.message, "cannot replicate backups of job");
        }
    }
}

/// 작업 하나를 한 걸음 나아가게 한다: 때가 되면 실행을 시작하고, 큐에서 VM을 꺼내 백업을 요청하고,
/// 큐가 비고 이번 실행의 백업이 모두 끝났으면 실행을 끝낸다. 보존 규칙대로 지우기는 [`prune_after_run`].
async fn advance(
    state: &AppState,
    config: &BackupJobSchedulerConfig,
    mut job: BackupJob,
    now: u64,
) -> Result<(), ApiError> {
    let cluster = &state.cluster;
    if job.status.run.is_none() {
        if !job.spec.enabled || job.status.next_run.is_none_or(|t| t > now) {
            return Ok(());
        }
        let (_, mut vms) = cluster.list::<VmSpec>()?;
        vms.sort_by(|a, b| a.name.cmp(&b.name));
        let queue: Vec<Uuid> = vms
            .iter()
            .filter(|vm| job.spec.vms.matches(vm))
            .map(|vm| vm.id)
            .collect();
        // 이름으로 골랐는데 그 VM이 없으면 오류로 남긴다.
        let missing = match &job.spec.vms {
            VmSelector::Names(names) => names
                .iter()
                .filter(|name| !vms.iter().any(|vm| vm.name == **name))
                .map(|name| JobError {
                    vm: name.clone(),
                    error: format!("no VM named {name:?}"),
                })
                .collect(),
            VmSelector::All | VmSelector::Labels(_) => Vec::new(),
        };
        // 놓친 실행이 여럿이어도 한 번만 하고 다음 실행은 지금부터 센다.
        let status = BackupJobStatus {
            next_run: Some(next_run(&job.spec.schedule, now)),
            run: Some(JobRun {
                started_at: now,
                queue,
                backups: Vec::new(),
            }),
            last_run: job.status.last_run.clone(),
            errors: missing,
        };
        job.revision = cluster
            .update_status_checked::<BackupJobSpec>(job.id, job.revision, &status)
            .await?;
        job.status = status;
        tracing::info!(job = %job.name, "backup job run started");
    }

    let queue = job
        .status
        .run
        .as_ref()
        .map_or(Vec::new(), |r| r.queue.clone());
    for vm in queue {
        // 실행 도중 꺼졌으면 새 VM은 꺼내지 않는다.
        if !job.spec.enabled {
            break;
        }
        let outcome = dispatch(state, config, &job, vm).await?;
        if outcome == Dispatch::Wait {
            continue;
        }
        job = update_status(state, job.id, |job| take(job, vm, &outcome)).await?;
    }
    if !job.spec.enabled && job.status.run.as_ref().is_some_and(|r| !r.queue.is_empty()) {
        job = update_status(state, job.id, |job| {
            let mut status = job.status.clone();
            if let Some(run) = &mut status.run {
                run.queue.clear();
            }
            Ok(status)
        })
        .await?;
    }

    // Task를 먼저 읽는다: 백업 Task는 백업 상태를 쓴 뒤에 Task를 끝내므로, 이 순서면 끝난 Task의
    // 백업이 pending으로 보이지 않는다.
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let (_, backups) = cluster.list::<BackupSpec>()?;
    if finish(&job.status, &backups, &tasks, now).is_none() {
        return Ok(());
    }
    let job = update_status(state, job.id, |job| {
        finish(&job.status, &backups, &tasks, now)
            .ok_or_else(|| ApiError::conflict("job_running", "the backup job run changed"))
    })
    .await?;
    if let Some(last) = &job.status.last_run {
        tracing::info!(job = %job.name, ok = last.ok, failed = last.failed, "backup job run finished");
    }
    Ok(())
}

/// 끝난 실행의 보존 정리: 실행이 끝나 있으면 보존 규칙이 지울 백업([`retention`])마다 지우기 Task를
/// 만든다. 만들지 못한 백업(복제·복원 중이라 `backup_busy` 등)은 tick마다 다시 본다. 실행이 끝난 뒤
/// 이미 지우기 Task를 만든 백업은 그 Task가 실패했어도 다음 실행이 끝날 때까지 다시 만들지 않는다.
async fn prune_after_run(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let cluster = &state.cluster;
    let Some(job) = cluster.get::<BackupJobSpec>(id)? else {
        return Ok(());
    };
    let Some(last) = job
        .status
        .last_run
        .as_ref()
        .filter(|_| job.status.run.is_none())
    else {
        return Ok(());
    };
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let (_, backups) = cluster.list::<BackupSpec>()?;
    for id in retention(job.id, &job.spec.keep, &backups) {
        let Some(backup) = backups.iter().find(|b| b.id == id) else {
            continue;
        };
        let tried = tasks.iter().any(|t| {
            t.spec.op == TaskOp::BackupDelete
                && t.spec.target == id
                && t.id.get_timestamp().map(|ts| ts.to_unix().0) >= Some(last.finished_at)
        });
        if tried {
            continue;
        }
        match start_backup_delete(state, backup).await {
            Ok(task) => {
                tracing::info!(job = %job.name, backup = %backup.name, task = %task.id, "pruning backup")
            }
            Err(e) => {
                tracing::info!(job = %job.name, backup = %backup.name, error = %e.message, "cannot prune backup yet")
            }
        }
    }
    Ok(())
}

/// 노드의 동시 백업 수에 드는 끝나지 않은 Task: 백업과 복제
fn occupies(t: &Task) -> bool {
    matches!(
        t.spec.op,
        TaskOp::VmBackup { .. } | TaskOp::BackupReplicate { .. }
    ) && !t.status.state.is_finished()
}

/// 원격 복제를 켠 작업 `id`의 백업 가운데 보낼 것([`to_replicate`])의 복제 Task를 만든다. 노드마다
/// 복제 Task는 하나씩, 백업 Task와 합쳐 `max_concurrent`개까지다. 꺼 둔 작업은 보내지 않는다. 만들지
/// 못한 백업(노드가 오프라인, 원격이 없어짐 등)은 다음 tick에 다시 본다. 옛 노드가 있으면 만들지 않는다.
async fn replicate(
    state: &AppState,
    config: &BackupJobSchedulerConfig,
    id: Uuid,
) -> Result<(), ApiError> {
    let cluster = &state.cluster;
    let Some(job) = cluster.get::<BackupJobSpec>(id)? else {
        return Ok(());
    };
    let Some(target) = job.spec.replicate.clone().filter(|_| job.spec.enabled) else {
        return Ok(());
    };
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let (_, backups) = cluster.list::<BackupSpec>()?;
    let candidates = to_replicate(&job, &target, &backups, &tasks);
    if candidates.is_empty() {
        return Ok(());
    }
    // 옛 노드는 복제 Task와 사본 기록을 모른다. 모든 노드가 올라오면 보낸다.
    if require_current(cluster).is_err() {
        return Ok(());
    }
    let mut running: BTreeMap<Uuid, usize> = BTreeMap::new();
    let mut replicating: Vec<Uuid> = Vec::new();
    for t in tasks.iter().filter(|t| occupies(t)) {
        *running.entry(t.spec.node).or_default() += 1;
        if matches!(t.spec.op, TaskOp::BackupReplicate { .. }) {
            replicating.push(t.spec.node);
        }
    }
    for backup in candidates {
        let node = match find_pool(state, &backup.spec.pool)
            .and_then(|pool| files_node(state, backup, &pool))
        {
            Ok(node) => node,
            Err(e) => {
                tracing::info!(job = %job.name, backup = %backup.name, error = %e.message, "cannot replicate backup yet");
                continue;
            }
        };
        if replicating.contains(&node)
            || running.get(&node).copied().unwrap_or(0) >= config.max_concurrent
        {
            continue;
        }
        match start_backup_replicate(state, backup, &target.remote, target.pool.clone()).await {
            Ok(task) => {
                tracing::info!(job = %job.name, backup = %backup.name, remote = %target.remote, task = %task.id, "replicating backup");
                replicating.push(node);
                *running.entry(node).or_default() += 1;
            }
            Err(e) if e.status.is_client_error() => {
                tracing::info!(job = %job.name, backup = %backup.name, error = %e.message, "cannot replicate backup");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// 작업 `job`의 백업 가운데 원격 `target`으로 보낼 것(만든 순서): `ready`이고, 보존 규칙이 지울 것이
/// 아니고([`retention`] — 지우기와 겹치지 않게), 그 사본 기록이 없거나 기록이 마지막 실행을 시작하기 전의
/// 것이다. 그래서 실패한(또는 복제 Task 없이 copying으로 남은) 사본은 실행마다 한 번 더 보내고, ready
/// 사본도 실행마다 한 번 원격에 있는지 확인한다(복제 Task가 원격에 물어 같은 파일이 있으면 기록 시각만
/// 고치고, 원격에서 지웠으면 다시 보낸다). 같은 원격으로 복제하는 Task가 끝나지 않은 백업은 뺀다.
fn to_replicate<'a>(
    job: &BackupJob,
    target: &ReplicateTarget,
    backups: &'a [Backup],
    tasks: &[Task],
) -> Vec<&'a Backup> {
    let started = job
        .status
        .run
        .as_ref()
        .map(|r| r.started_at)
        .or(job.status.last_run.as_ref().map(|l| l.started_at))
        .unwrap_or(0);
    let replicating = |b: &Backup| {
        tasks.iter().any(|t| {
            !t.status.state.is_finished()
                && t.spec.target == b.id
                && matches!(&t.spec.op, TaskOp::BackupReplicate { remote, .. } if *remote == target.remote)
        })
    };
    let pruned = retention(job.id, &job.spec.keep, backups);
    let mut out: Vec<&Backup> = backups
        .iter()
        .filter(|b| b.spec.job == Some(job.id) && b.status.phase == BackupPhase::Ready)
        .filter(|b| !pruned.contains(&b.id))
        .filter(|b| !replicating(b))
        .filter(|b| {
            match b
                .status
                .replicas
                .iter()
                .find(|r| r.remote == target.remote && r.pool == target.pool)
            {
                None => true,
                Some(r) => r.at < started,
            }
        })
        .collect();
    out.sort_by_key(|b| b.id);
    out
}

/// 큐에서 꺼낸 VM 하나에 한 일
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dispatch {
    /// 노드가 이미 백업 Task를 다 돌리고 있다. 큐에 남긴다.
    Wait,
    /// VM이 없어졌다.
    Gone,
    /// 이 백업을 만들었다(또는 이번 실행이 이미 만들었다).
    Started(Uuid),
    /// 백업하지 못했다.
    Refused(JobError),
}

/// 큐의 VM `vm`의 백업을 요청한다. 요청 검사는 백업 API와 같다(거절은 [`Dispatch::Refused`]).
/// 이번 실행이 이 VM의 백업을 이미 만들었으면(상태에 적지 못한 채 리더가 바뀌었거나 쓰기가 실패했다)
/// 그 백업을 받아들인다.
async fn dispatch(
    state: &AppState,
    config: &BackupJobSchedulerConfig,
    job: &BackupJob,
    vm: Uuid,
) -> Result<Dispatch, ApiError> {
    let cluster = &state.cluster;
    let Some(vm) = cluster.get::<VmSpec>(vm)? else {
        return Ok(Dispatch::Gone);
    };
    let (_, backups) = cluster.list::<BackupSpec>()?;
    if let Some(made) = backups
        .iter()
        .find(|b| b.spec.vm == vm.id && of_this_run(job, b))
    {
        return Ok(Dispatch::Started(made.id));
    }
    let refused = |error: String| {
        Ok(Dispatch::Refused(JobError {
            vm: vm.name.clone(),
            error,
        }))
    };
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let backing_up =
        |t: &&Task| matches!(t.spec.op, TaskOp::VmBackup { .. }) && !t.status.state.is_finished();
    if tasks
        .iter()
        .filter(backing_up)
        .any(|t| t.spec.target == vm.id)
    {
        return refused("a backup of this VM is already running".into());
    }
    let Some(pool) = cluster.get_by_name::<StoragePoolSpec>(&job.spec.pool)? else {
        return refused(format!("storage pool {:?} not found", job.spec.pool));
    };
    let node = match check_backup(state, &vm, &pool) {
        Ok(node) => node,
        Err(e) if e.status.is_client_error() => return refused(e.message),
        Err(e) => return Err(e),
    };
    let running = tasks
        .iter()
        .filter(|t| occupies(t))
        .filter(|t| t.spec.node == node)
        .count();
    if running >= config.max_concurrent {
        return Ok(Dispatch::Wait);
    }
    let name = backup_name(&vm.name, now(), &backups);
    match start_backup(state, &vm, &pool, node, name, Some(job.id)).await {
        Ok(task) => match task.spec.op {
            TaskOp::VmBackup { backup } => Ok(Dispatch::Started(backup)),
            _ => unreachable!("start_backup makes a backup task"),
        },
        Err(e) if e.status.is_client_error() => refused(e.message),
        Err(e) => Err(e),
    }
}

/// 백업 `backup`이 작업 `job`의 진행 중인 실행이 만든 것인지. 백업 id(v7)의 시각(초)이 실행을 시작한
/// 시각 이후이고 마지막으로 끝난 실행의 끝난 시각보다 뒤여야 한다(앞 실행의 백업은 그 실행이 끝나기 전에
/// 만들었다).
fn of_this_run(job: &BackupJob, backup: &Backup) -> bool {
    let Some(run) = &job.status.run else {
        return false;
    };
    let Some(made) = backup.id.get_timestamp().map(|t| t.to_unix().0) else {
        return false;
    };
    backup.spec.job == Some(job.id)
        && made >= run.started_at
        && job
            .status
            .last_run
            .as_ref()
            .is_none_or(|l| made > l.finished_at)
}

/// 실행의 큐에서 `vm`을 빼고 한 일을 적은 상태
fn take(job: &BackupJob, vm: Uuid, outcome: &Dispatch) -> Result<BackupJobStatus, ApiError> {
    let mut status = job.status.clone();
    let Some(run) = &mut status.run else {
        return Err(ApiError::conflict(
            "job_not_running",
            "the backup job run ended",
        ));
    };
    run.queue.retain(|id| *id != vm);
    match outcome {
        Dispatch::Started(backup) if !run.backups.contains(backup) => run.backups.push(*backup),
        Dispatch::Refused(error) => status.errors.push(error.clone()),
        Dispatch::Started(_) | Dispatch::Wait | Dispatch::Gone => {}
    }
    Ok(status)
}

/// 기본 백업 이름(`{VM 이름}-{yyyymmdd-HHMMSS}`). 같은 이름의 백업이 있으면 `-2`, `-3`…을 붙인다.
/// 번호를 붙이면 63자를 넘지 않게 VM 이름 부분을 번호 길이만큼 줄인다.
fn backup_name(vm: &str, now: u64, backups: &[Backup]) -> String {
    let base = default_backup_name(vm, now);
    let taken = |name: &str| backups.iter().any(|b| b.name == name);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            // 기본 이름은 VM 이름의 앞 47자를 쓴다.
            let short = &vm[..vm.len().min(47 - suffix.len())];
            format!("{}{suffix}", default_backup_name(short, now))
        })
        .find(|name| !taken(name))
        .unwrap()
}

/// 끝난 실행을 적은 상태. 실행이 없거나, 큐가 남았거나, 이번 실행의 백업이 아직 만드는 중이면 없다.
/// 성공은 `ready` 백업, 실패는 `failed` 백업과 백업하지 못한 VM이다(없어진 백업은 세지 않는다).
/// 채우는 끝나지 않은 Task가 없는 `pending` 백업(죽은 노드의 Task를 실패 처리했다)도 실패다.
/// 실패한 백업의 오류는 `errors`에 더한다.
fn finish(
    status: &BackupJobStatus,
    backups: &[Backup],
    tasks: &[Task],
    now: u64,
) -> Option<BackupJobStatus> {
    let run = status.run.as_ref()?;
    if !run.queue.is_empty() {
        return None;
    }
    let mine: Vec<&Backup> = run
        .backups
        .iter()
        .filter_map(|id| backups.iter().find(|b| b.id == *id))
        .collect();
    let filling = |b: &Backup| {
        tasks.iter().any(|t| {
            matches!(t.spec.op, TaskOp::VmBackup { backup } if backup == b.id)
                && !t.status.state.is_finished()
        })
    };
    let (pending, mine): (Vec<&Backup>, Vec<&Backup>) = mine
        .into_iter()
        .partition(|b| b.status.phase == BackupPhase::Pending);
    if pending.iter().any(|b| filling(b)) {
        return None;
    }
    let ok = mine
        .iter()
        .filter(|b| b.status.phase == BackupPhase::Ready)
        .count() as u32;
    let mut errors = status.errors.clone();
    errors.extend(
        mine.iter()
            .filter(|b| b.status.phase == BackupPhase::Failed)
            .map(|b| JobError {
                vm: b.spec.vm_name.clone(),
                error: b
                    .status
                    .error
                    .clone()
                    .unwrap_or_else(|| "backup failed".into()),
            }),
    );
    errors.extend(pending.iter().map(|b| JobError {
        vm: b.spec.vm_name.clone(),
        error: "backup task ended without finishing the backup".into(),
    }));
    Some(BackupJobStatus {
        next_run: status.next_run,
        run: None,
        last_run: Some(JobRunResult {
            started_at: run.started_at,
            finished_at: now,
            ok,
            failed: errors.len() as u32,
        }),
        errors,
    })
}

/// 작업 `job`의 백업 가운데 지울 것: VM마다 `ready` 백업은 보존 규칙(`prune`)에 맞지 않는 것,
/// `failed` 백업은 같은 VM의 더 새(id가 큰) `ready` 백업이 있는 것. 다른 작업과 손으로 만든 백업은
/// 건드리지 않는다.
fn retention(job: Uuid, keep: &Keep, backups: &[Backup]) -> Vec<Uuid> {
    let mut by_vm: BTreeMap<Uuid, Vec<&Backup>> = BTreeMap::new();
    for b in backups.iter().filter(|b| b.spec.job == Some(job)) {
        by_vm.entry(b.spec.vm).or_default().push(b);
    }
    let mut out = Vec::new();
    for mine in by_vm.values() {
        let ready: Vec<(Uuid, u64)> = mine
            .iter()
            .filter(|b| b.status.phase == BackupPhase::Ready)
            .filter_map(|b| Some((b.id, b.status.created_at?)))
            .collect();
        out.extend(prune(&ready, keep));
        if let Some(newest) = ready.iter().map(|b| b.0).max() {
            out.extend(
                mine.iter()
                    .filter(|b| b.status.phase == BackupPhase::Failed && b.id < newest)
                    .map(|b| b.id),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use opl_types::backup::{BackupReplica, BackupStatus, ReplicaState};
    use opl_types::backup_job::JobSchedule;
    use opl_types::resource::validate_name;
    use opl_types::task::TaskState;
    use opl_types::vm::{CpuSpec, MachineType, PowerState};

    use super::*;

    /// 2026-10-27 05:07:00 UTC
    const NOW: u64 = 1_793_077_620;
    const DAY: u64 = 86400;

    fn vm_spec() -> VmSpec {
        VmSpec {
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: Default::default(),
            },
            memory_mib: 256,
            firmware: Default::default(),
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
            power: PowerState::Stopped,
            restart: Default::default(),
            node: None,
            snapshot_schedule: None,
            ha: false,
        }
    }

    /// VM `vm`의 백업. `created_at`이 있으면 `phase`로 끝났다.
    fn backup(
        name: &str,
        vm: Uuid,
        job: Option<Uuid>,
        phase: BackupPhase,
        created_at: Option<u64>,
    ) -> Backup {
        let mut b: Backup = Resource::new(
            name,
            BackupSpec {
                vm,
                vm_name: format!("vm-{}", &vm.simple().to_string()[..4]),
                pool: "bk".into(),
                vm_spec: vm_spec(),
                job,
                origin: None,
            },
        );
        b.status = BackupStatus {
            phase,
            created_at,
            error: (phase == BackupPhase::Failed).then(|| format!("{name} broke")),
            ..Default::default()
        };
        b
    }

    fn running(queue: Vec<Uuid>, backups: Vec<Uuid>, errors: Vec<JobError>) -> BackupJobStatus {
        BackupJobStatus {
            next_run: Some(NOW + DAY),
            run: Some(JobRun {
                started_at: NOW - 60,
                queue,
                backups,
            }),
            last_run: None,
            errors,
        }
    }

    /// 백업 `backup`을 채우는 Task
    fn task(backup: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "t",
            TaskSpec {
                op: TaskOp::VmBackup { backup },
                target: Uuid::now_v7(),
                node: Uuid::now_v7(),
            },
        );
        t.status.state = state;
        t
    }

    fn refused(vm: &str) -> JobError {
        JobError {
            vm: vm.into(),
            error: "no".into(),
        }
    }

    #[test]
    fn names_follow_the_default_with_a_number_when_taken() {
        let vm = Uuid::now_v7();
        assert_eq!(backup_name("web", NOW, &[]), "web-20261027-050700");
        let taken = [
            backup("web-20261027-050700", vm, None, BackupPhase::Ready, Some(1)),
            backup(
                "web-20261027-050700-2",
                vm,
                None,
                BackupPhase::Ready,
                Some(2),
            ),
        ];
        assert_eq!(backup_name("web", NOW, &taken), "web-20261027-050700-3");
        assert_eq!(backup_name("db", NOW, &taken), "db-20261027-050700");

        // 63자 이름 둘의 앞부분이 같으면 번호를 붙이되 63자를 넘지 않게 앞부분을 줄인다.
        let a = format!("{}a", "x".repeat(62));
        let b = format!("{}b", "x".repeat(62));
        let first = backup_name(&a, NOW, &[]);
        let taken = [backup(&first, vm, None, BackupPhase::Ready, Some(1))];
        let second = backup_name(&b, NOW, &taken);
        assert_ne!(first, second);
        for name in [&first, &second] {
            assert!(name.len() <= 63, "{name}");
            assert_eq!(validate_name(name), Ok(()), "{name}");
        }
        assert!(second.ends_with("-2"), "{second}");
        // 줄인 끝이 `-`이면 그것도 뗀다.
        let dashed = format!("{}-{}", "y".repeat(44), "z".repeat(18));
        let first = backup_name(&dashed, NOW, &[]);
        let taken = [backup(&first, vm, None, BackupPhase::Ready, Some(1))];
        let second = backup_name(&dashed, NOW, &taken);
        assert_eq!(second, format!("{}-20261027-050700-2", "y".repeat(44)));
    }

    #[test]
    fn a_run_finishes_when_its_queue_is_empty_and_its_backups_are_done() {
        let vm = Uuid::now_v7();
        let ready = backup("a", vm, None, BackupPhase::Ready, Some(NOW - 10));
        let failed = backup("b", vm, None, BackupPhase::Failed, None);
        let pending = backup("c", vm, None, BackupPhase::Pending, None);
        let all = [ready.clone(), failed.clone(), pending.clone()];
        let filling = [task(pending.id, TaskState::Running)];

        // 실행이 없거나, 큐가 남았거나, 만드는 중인 백업이 있으면 끝나지 않았다.
        assert_eq!(finish(&BackupJobStatus::default(), &all, &[], NOW), None);
        let queued = running(vec![vm], vec![ready.id], vec![]);
        assert_eq!(finish(&queued, &all, &[], NOW), None);
        let waiting = running(vec![], vec![ready.id, pending.id], vec![]);
        assert_eq!(finish(&waiting, &all, &filling, NOW), None);

        // 채우던 Task가 끝났는데(실패 처리 등) pending으로 남은 백업은 실패다.
        for tasks in [vec![], vec![task(pending.id, TaskState::Failed)]] {
            let done = finish(&waiting, &all, &tasks, NOW).unwrap();
            let last = done.last_run.unwrap();
            assert_eq!((last.ok, last.failed), (1, 1));
            assert_eq!(
                done.errors,
                [JobError {
                    vm: pending.spec.vm_name.clone(),
                    error: "backup task ended without finishing the backup".into(),
                }]
            );
        }

        // ready는 성공, failed와 백업하지 못한 VM은 실패다. 없어진 백업은 세지 않는다.
        let gone = Uuid::now_v7();
        let status = running(vec![], vec![ready.id, failed.id, gone], vec![refused("db")]);
        let done = finish(&status, &all, &filling, NOW).unwrap();
        assert_eq!(done.run, None);
        assert_eq!(done.next_run, Some(NOW + DAY));
        assert_eq!(
            done.last_run,
            Some(JobRunResult {
                started_at: NOW - 60,
                finished_at: NOW,
                ok: 1,
                failed: 2,
            })
        );
        assert_eq!(
            done.errors,
            [
                refused("db"),
                JobError {
                    vm: failed.spec.vm_name.clone(),
                    error: "b broke".into(),
                }
            ]
        );
        // 백업 없이 끝난 실행
        let empty = finish(&running(vec![], vec![], vec![]), &[], &[], NOW).unwrap();
        assert_eq!(empty.last_run.unwrap().ok, 0);
    }

    #[test]
    fn taking_a_vm_records_what_happened() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let mut job: BackupJob = Resource::new(
            "nightly",
            BackupJobSpec {
                vms: VmSelector::All,
                pool: "bk".into(),
                schedule: JobSchedule {
                    at: "03:00".into(),
                    days: vec![],
                },
                keep: Keep::default(),
                enabled: true,
                replicate: None,
            },
        );
        job.status = running(vec![a, b], vec![], vec![]);
        let backup = Uuid::now_v7();
        let status = take(&job, a, &Dispatch::Started(backup)).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [b]);
        assert_eq!(status.run.as_ref().unwrap().backups, [backup]);
        let status = take(&job, b, &Dispatch::Refused(refused("db"))).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [a]);
        assert_eq!(status.errors, [refused("db")]);
        let status = take(&job, b, &Dispatch::Gone).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [a]);
        assert_eq!(status.errors, []);
        job.status.run = None;
        assert!(take(&job, a, &Dispatch::Gone).is_err());
    }

    #[test]
    fn a_backup_belongs_to_the_run_made_after_it_started() {
        let vm = Uuid::now_v7();
        let mut job: BackupJob = Resource::new(
            "nightly",
            BackupJobSpec {
                vms: VmSelector::All,
                pool: "bk".into(),
                schedule: JobSchedule {
                    at: "03:00".into(),
                    days: vec![],
                },
                keep: Keep::default(),
                enabled: true,
                replicate: None,
            },
        );
        // 실행은 NOW - 60에 시작했다.
        job.status = running(vec![vm], vec![], vec![]);
        let made_at = |at: u64, job: Option<Uuid>| {
            let mut b = backup("b", vm, job, BackupPhase::Pending, None);
            b.id = Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, at, 0));
            b
        };
        assert!(of_this_run(&job, &made_at(NOW - 60, Some(job.id))));
        assert!(of_this_run(&job, &made_at(NOW, Some(job.id))));
        assert!(!of_this_run(&job, &made_at(NOW - 61, Some(job.id))));
        // 다른 작업과 손으로 만든 백업
        assert!(!of_this_run(&job, &made_at(NOW, Some(Uuid::now_v7()))));
        assert!(!of_this_run(&job, &made_at(NOW, None)));
        // 앞 실행이 실행을 시작한 그 초에 끝났으면 그 초의 백업은 앞 실행의 것이다.
        job.status.last_run = Some(JobRunResult {
            started_at: NOW - 120,
            finished_at: NOW - 60,
            ok: 1,
            failed: 0,
        });
        assert!(!of_this_run(&job, &made_at(NOW - 60, Some(job.id))));
        assert!(of_this_run(&job, &made_at(NOW - 59, Some(job.id))));
        // 실행이 없으면 아무것도 아니다.
        job.status.run = None;
        assert!(!of_this_run(&job, &made_at(NOW, Some(job.id))));
    }

    /// 사본 기록
    fn replica(remote: &str, pool: &str, state: ReplicaState, at: u64) -> BackupReplica {
        BackupReplica {
            remote: remote.into(),
            pool: pool.into(),
            backup: None,
            state,
            error: None,
            at,
        }
    }

    #[test]
    fn replication_picks_ready_backups_without_a_copy_and_retries_failures_once_a_run() {
        let vm = Uuid::now_v7();
        let mut job: BackupJob = Resource::new(
            "nightly",
            BackupJobSpec {
                vms: VmSelector::All,
                pool: "bk".into(),
                schedule: JobSchedule {
                    at: "03:00".into(),
                    days: vec![],
                },
                keep: Keep::default(),
                enabled: true,
                replicate: None,
            },
        );
        let target = ReplicateTarget {
            remote: "dr".into(),
            pool: "far".into(),
        };
        // 마지막 실행은 NOW - 60에 시작했다.
        job.status = running(vec![], vec![], vec![]);
        let with_id = |mut b: Backup, n: u128, replicas: Vec<BackupReplica>| {
            b.id = Uuid::from_u128(n);
            b.status.replicas = replicas;
            b
        };
        let ready = |n, replicas| {
            with_id(
                backup("r", vm, Some(job.id), BackupPhase::Ready, Some(1)),
                n,
                replicas,
            )
        };
        let fresh = ready(9, vec![]);
        // 다른 원격·풀의 사본만 있다.
        let elsewhere = ready(
            2,
            vec![
                replica("other", "far", ReplicaState::Ready, NOW),
                replica("dr", "near", ReplicaState::Ready, NOW),
            ],
        );
        let failed_before = ready(
            3,
            vec![replica("dr", "far", ReplicaState::Failed, NOW - 61)],
        );
        let failed_since = ready(
            4,
            vec![replica("dr", "far", ReplicaState::Failed, NOW - 60)],
        );
        // 실행 전에 ready였던 사본도 실행마다 한 번 원격에 있는지 확인한다.
        let copied = ready(5, vec![replica("dr", "far", ReplicaState::Ready, 1)]);
        let copied_since = ready(
            10,
            vec![replica("dr", "far", ReplicaState::Ready, NOW - 60)],
        );
        // 복제 Task 없이 copying으로 남은 기록은 실패와 같다.
        let stuck = ready(
            6,
            vec![replica("dr", "far", ReplicaState::Copying, NOW - 100)],
        );
        let copying = ready(7, vec![]);
        let pending = with_id(
            backup("p", vm, Some(job.id), BackupPhase::Pending, None),
            8,
            vec![],
        );
        let manual = with_id(
            backup("m", vm, None, BackupPhase::Ready, Some(1)),
            1,
            vec![],
        );
        let mut sending = task(copying.id, TaskState::Running);
        sending.spec.op = TaskOp::BackupReplicate {
            remote: "dr".into(),
            pool: "far".into(),
        };
        sending.spec.target = copying.id;
        let mut done = sending.clone();
        done.spec.target = fresh.id;
        done.status.state = TaskState::Failed;
        let all = [
            fresh.clone(),
            elsewhere.clone(),
            failed_before.clone(),
            failed_since,
            copied.clone(),
            copied_since,
            stuck.clone(),
            copying,
            pending,
            manual,
        ];
        let ids = |job: &BackupJob| -> Vec<Uuid> {
            to_replicate(job, &target, &all, &[sending.clone(), done.clone()])
                .iter()
                .map(|b| b.id)
                .collect()
        };
        assert_eq!(
            ids(&job),
            [
                elsewhere.id,
                failed_before.id,
                copied.id,
                stuck.id,
                fresh.id
            ]
        );
        // 실행이 끝났으면 그 실행의 시작이 기준이다.
        job.status.last_run = Some(JobRunResult {
            started_at: NOW - 60,
            finished_at: NOW,
            ok: 1,
            failed: 0,
        });
        job.status.run = None;
        assert_eq!(
            ids(&job),
            [
                elsewhere.id,
                failed_before.id,
                copied.id,
                stuck.id,
                fresh.id
            ]
        );
        // 실행한 적이 없으면 실패를 다시 보내지 않는다.
        job.status.last_run = None;
        assert_eq!(ids(&job), [elsewhere.id, fresh.id]);
        // 보존 규칙이 지울 백업(ready 여덟 가운데 id가 작은 셋)은 보내지 않는다.
        job.spec.keep = Keep {
            last: 5,
            ..Keep::default()
        };
        assert_eq!(ids(&job), [fresh.id]);
    }

    #[test]
    fn retention_prunes_ready_backups_per_vm_and_failed_ones_before_a_success() {
        let job = Uuid::now_v7();
        let (web, db) = (Uuid::now_v7(), Uuid::now_v7());
        let keep = Keep {
            last: 1,
            ..Keep::default()
        };
        // id가 만든 순서다(v7).
        let with_id = |mut b: Backup, n: u128| {
            b.id = Uuid::from_u128(n);
            b
        };
        let r = |vm, at, n| with_id(backup("r", vm, Some(job), BackupPhase::Ready, Some(at)), n);
        let f = |vm, n| with_id(backup("f", vm, Some(job), BackupPhase::Failed, None), n);
        // 만든 순서: web 실패 → web 성공 둘 → web 실패, db 성공 둘
        let web_failed_old = f(web, 1);
        let web_old = r(web, NOW - 2 * DAY, 2);
        let web_new = r(web, NOW - DAY, 3);
        let web_failed_new = f(web, 4);
        let db_old = r(db, NOW - 2 * DAY, 5);
        let db_new = r(db, NOW - DAY, 6);
        // 손으로 만든 백업, 다른 작업의 백업, 만드는 중인 백업은 건드리지 않는다.
        let manual = backup("m", web, None, BackupPhase::Ready, Some(1));
        let other = backup("o", web, Some(Uuid::now_v7()), BackupPhase::Ready, Some(1));
        let pending = backup("p", web, Some(job), BackupPhase::Pending, None);
        let all = [
            web_failed_old.clone(),
            web_old.clone(),
            web_new.clone(),
            web_failed_new.clone(),
            db_old.clone(),
            db_new.clone(),
            manual,
            other,
            pending,
        ];
        let mut got = retention(job, &keep, &all);
        got.sort();
        let mut want = vec![web_old.id, web_failed_old.id, db_old.id];
        want.sort();
        assert_eq!(got, want);

        // 규칙이 모두 0이면 성공한 백업은 지우지 않지만, 더 새 성공 앞의 실패는 지운다.
        assert_eq!(retention(job, &Keep::default(), &all), [web_failed_old.id]);
        // 성공이 없으면 실패는 남는다.
        assert!(retention(job, &keep, &[web_failed_old, web_failed_new]).is_empty());
    }
}
