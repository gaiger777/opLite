//! 백업 작업 만들기·바꾸기·지우기·지금 실행(설계 §3). 목록과 조회는 `resources`의 공통 핸들러다.
//! 쓰기는 백업과 같은 권한(operator 이상)이다. 작업을 지워도 그 작업이 만든 백업은 남는다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::backup_job::{
    BackupJob, BackupJobSpec, BackupJobStatus, JobSchedule, Keep, VmSelector, next_run,
};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::StoragePoolSpec;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, check_name, if_match, lookup};
use crate::state::AppState;

/// 스펙 검증과 풀 검사: 풀이 있고 백업 풀이어야 한다(400). 요일은 주의 순서로 정리한다.
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
    {
        return Err(ApiError::bad_request(
            "give vms, pool, schedule, keep or enabled",
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
