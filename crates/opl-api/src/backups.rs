//! VM 백업 만들기·목록·읽기·지우기·복원(설계 §2). 만들기는 VM 쓰기, 지우기·복원은 백업 쓰기 권한이다.
//! 실제 작업은 Task 실행기가 한다: 백업은 VM의 노드, 지우기·복원은 백업 풀을 쓸 수 있는 노드(공유가
//! 아닌 풀의 백업은 파일을 쓴 노드).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus, default_backup_name};
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, is_shared,
    pool_kind_supports_snapshots, pool_usable_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{Vm, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{List, check_name, lookup};
use crate::snapshots::{check_disks, check_not_busy};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vms::{check_security_groups, is_stopped};
use crate::volumes::{check_pool_on, pool_node};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBackupRequest {
    /// 백업 풀(`backups`를 켠 풀)
    pub pool: String,
    /// 백업 이름(클러스터에서 고유). 없으면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    /// 새 VM의 이름
    pub name: String,
    /// 새 디스크를 둘 풀
    pub pool: String,
    /// 복원을 실행할 노드(이름이나 id). 없으면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
    pub node: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct BackupQuery {
    /// 이 VM(id나 이름)의 백업만. 이름이면 지금 그 이름의 VM 백업과 그 이름으로 남은(지운 VM의) 백업이다.
    pub vm: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct DeleteBackupQuery {
    /// 파일을 지울 노드가 없어도 리소스만 지운다. 파일은 백업 풀에 남는다.
    #[serde(default)]
    pub force: bool,
}

/// 리소스만 지운 백업 지우기(`force`)의 응답
#[derive(Debug, Serialize, ToSchema)]
pub struct BackupForgotten {
    pub message: String,
}

/// 지금 시각(유닉스 초)
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
        .cluster
        .get_by_name::<StoragePoolSpec>(name)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))
}

/// `fits`를 만족하는 온라인 노드 가운데 이름이 가장 앞선 노드
fn first_node(state: &AppState, fits: impl Fn(&Node) -> bool) -> Result<Option<Uuid>, ApiError> {
    let (_, mut nodes) = state.cluster.list::<NodeSpec>()?;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(nodes
        .iter()
        .find(|n| n.status.online && fits(n))
        .map(|n| n.id))
}

/// 백업 `id`를 다루는 끝나지 않은 Task가 하는 일(만들기, 복원, 지우기)
fn backup_tasks(state: &AppState, id: Uuid) -> Result<Vec<&'static str>, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .filter_map(|t| match t.spec.op {
            TaskOp::VmBackup { backup } if backup == id => Some("being created"),
            TaskOp::BackupRestore { backup, .. } if backup == id => Some("being restored"),
            TaskOp::BackupDelete if t.spec.target == id => Some("being deleted"),
            _ => None,
        })
        .collect())
}

/// 노드 `id`가 온라인인지 확인한다. 아니면 409 `node_offline`.
fn check_online(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    match state.cluster.get::<NodeSpec>(id)? {
        Some(n) if n.status.online => Ok(()),
        n => Err(ApiError::conflict(
            "node_offline",
            format!(
                "node {:?} is offline",
                n.map_or_else(|| id.to_string(), |n| n.name)
            ),
        )),
    }
}

/// 노드 `id`의 이름(없으면 id)
fn node_name(state: &AppState, id: Uuid) -> Result<String, ApiError> {
    Ok(state
        .cluster
        .get::<NodeSpec>(id)?
        .map_or_else(|| id.to_string(), |n| n.name))
}

fn backup_busy(backup: &Backup, doing: &str) -> ApiError {
    ApiError::conflict(
        "backup_busy",
        format!(
            "backup {:?} is {doing}; try again when the task has finished",
            backup.name
        ),
    )
}

/// 풀에 백업이 남아 있으면 409 `pool_in_use`. 풀 지우기와 백업 표시 끄기가 쓴다.
pub(crate) fn check_no_backups(state: &AppState, pool: &StoragePool) -> Result<(), ApiError> {
    let (_, backups) = state.cluster.list::<BackupSpec>()?;
    if backups.iter().any(|b| b.spec.pool == pool.name) {
        return Err(ApiError::conflict(
            "pool_in_use",
            format!("pool {:?} holds backups", pool.name),
        ));
    }
    Ok(())
}

/// VM 디스크의 볼륨과 풀. 없으면 400, 만드는 중이면 409 `volume_not_ready`.
fn vm_disks(state: &AppState, vm: &Vm) -> Result<Vec<(Volume, StoragePool)>, ApiError> {
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
    for disk in &vm.spec.disks {
        let volume = state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ApiError::bad_request(format!("volume {:?} not found", disk.volume)))?;
        if volume.status.phase != VolumePhase::Ready {
            return Err(ApiError::conflict(
                "volume_not_ready",
                format!("volume {:?} is still being created", disk.volume),
            ));
        }
        let pool = find_pool(state, &volume.spec.pool)?;
        disks.push((volume, pool));
    }
    Ok(disks)
}

/// 백업 Task를 실행할 노드. 실행 중인 VM은 그 노드, 꺼진 VM은 `spec.node`이고, 노드가 없으면 백업
/// 풀과 모든 디스크를 쓸 수 있는 온라인 노드 가운데 이름이 가장 앞선 노드다(로컬 풀의 디스크는 파일이
/// 있는 노드에서만 읽힌다). 그런 노드가 없으면 409 `no_node`.
fn backup_node(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
    disks: &[(Volume, StoragePool)],
) -> Result<Uuid, ApiError> {
    let placed = if is_stopped(vm) {
        vm.spec.node
    } else {
        vm.spec.node.or(vm.status.node)
    };
    if let Some(node) = placed {
        return Ok(node);
    }
    let reads = |node: &Node| {
        pool_usable_on(pool, node)
            && disks.iter().all(|(volume, p)| {
                pool_usable_on(p, node) && (is_shared(p) || volume.status.node == Some(node.id))
            })
    };
    first_node(state, reads)?.ok_or_else(|| {
        ApiError::conflict(
            "no_node",
            format!(
                "no online node can read the disks of VM {:?} and use storage pool {:?}",
                vm.name, pool.name
            ),
        )
    })
}

/// 모든 백업. `vm`(id나 이름)을 주면 그 VM의 백업만이다.
#[utoipa::path(get, path = "/backups", tag = "backups",
    params(BackupQuery),
    responses(
        (status = 200, body = List<Resource<BackupSpec, BackupStatus>>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn list_backups(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<BackupQuery>,
) -> Result<Json<List<Backup>>, ApiError> {
    state.authorize(&principal, Action::Read, BackupSpec::KIND)?;
    let (revision, mut items) = state.cluster.list::<BackupSpec>()?;
    if let Some(key) = q.vm {
        let id = match Uuid::try_parse(&key) {
            Ok(id) => Some(id),
            Err(_) => state.cluster.get_by_name::<VmSpec>(&key)?.map(|vm| vm.id),
        };
        // 이름이면 지금 그 이름인 VM의 백업과, 그 이름으로 남은 백업(지운 VM)을 함께 준다.
        let by_name = Uuid::try_parse(&key).is_err();
        items.retain(|b| Some(b.spec.vm) == id || (by_name && b.spec.vm_name == key));
    }
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/backups/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    responses(
        (status = 200, body = Resource<BackupSpec, BackupStatus>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn get_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Backup>, ApiError> {
    state.authorize(&principal, Action::Read, BackupSpec::KIND)?;
    Ok(Json(lookup::<BackupSpec>(&state.cluster, &key)?))
}

/// `pending` 백업을 만들고 그것을 채울 Task를 돌려준다(202). 풀은 백업 풀이어야 한다(400). 시작·정지
/// 중이면 409 `vm_busy`, 실행 중인데 스냅샷을 담지 못하는 디스크가 있으면 409 `vm_running`(스냅샷
/// 방식이 섞였으면 스냅샷처럼 409 `snapshot_unsupported`), Task를 돌릴 노드에서 백업 풀을 쓸 수 없으면
/// 409 `pool_not_on_node`·`pool_unavailable`, 돌릴 노드가 없으면 409 `no_node`, 이름이 겹치면 409
/// `name_taken`. 그사이 풀이 지워졌거나 백업 표시가 꺼졌으면 만든 백업을 지우고 409 `pool_unavailable`.
#[utoipa::path(post, path = "/vms/{key}/backups", tag = "backups",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CreateBackupRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CreateBackupRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    if let Some(name) = &req.name {
        check_name(name)?;
    }
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let pool = find_pool(&state, &req.pool)?;
    let node = check_backup(&state, &vm, &pool)?;
    let name = req
        .name
        .unwrap_or_else(|| default_backup_name(&vm.name, now()));
    let task = start_backup(&state, &vm, &pool, node, name, None).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM을 백업 풀 `pool`에 백업할 수 있는지 보고 백업 Task를 돌릴 노드를 돌려준다. 오류는
/// [`create_backup`]과 같다. 백업 요청 API와 백업 작업 스케줄러가 쓴다(권한 검사는 부르는 쪽).
pub(crate) fn check_backup(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
) -> Result<Uuid, ApiError> {
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    check_not_busy(state, vm)?;
    let disks = vm_disks(state, vm)?;
    // 실행 중인 VM은 임시 스냅샷에서 읽는다.
    if !is_stopped(vm)
        && let Some((volume, _)) = disks
            .iter()
            .find(|(v, p)| !pool_kind_supports_snapshots(&p.spec.backend, v.spec.format))
    {
        return Err(ApiError::conflict(
            "vm_running",
            format!(
                "VM {:?} is running and its disk {:?} cannot be snapshotted; stop it to back it up",
                vm.name, volume.name
            ),
        ));
    }
    if !is_stopped(vm) {
        check_disks(state, vm)?;
    }
    let node = backup_node(state, vm, pool, &disks)?;
    check_pool_on(state, pool, node, || {
        format!(
            "storage pool {:?} is not available on the node holding VM {:?}",
            pool.name, vm.name
        )
    })?;
    Ok(node)
}

/// [`check_backup`]을 거친 VM의 `pending` 백업 `name`을 만들고 노드 `node`에서 그것을 채울 Task를
/// 돌려준다. 백업 작업이 만드는 백업은 `job`에 작업 id를 적는다.
pub(crate) async fn start_backup(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
    node: Uuid,
    name: String,
    job: Option<Uuid>,
) -> Result<Task, ApiError> {
    let backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm: vm.id,
            vm_name: vm.name.clone(),
            pool: pool.name.clone(),
            vm_spec: vm.spec.clone(),
            job,
        },
    );
    // 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&backup).await?;
    // 풀 지우기·백업 표시 끄기는 백업이 없을 때만 된다. 그 확인과 엇갈렸으면 만든 백업을 거둔다.
    if !find_pool(state, &pool.name).is_ok_and(|p| p.spec.backups) {
        if let Err(d) = state.cluster.delete::<BackupSpec>(backup.id, None).await {
            tracing::warn!(backup = %backup.id, error = %d, "cannot remove backup of a removed pool");
        }
        return Err(ApiError::conflict(
            "pool_unavailable",
            format!(
                "storage pool {:?} was deleted or stopped storing backups",
                pool.name
            ),
        ));
    }
    let op = TaskOp::VmBackup { backup: backup.id };
    match create_task(&state.cluster, op, vm.id, node).await {
        Ok(task) => Ok(task),
        Err(e) => {
            // 채울 Task가 없는 pending 백업은 이름만 차지한다.
            if let Err(d) = state.cluster.delete::<BackupSpec>(backup.id, None).await {
                tracing::warn!(backup = %backup.id, error = %d, "cannot remove backup without a task");
            }
            Err(e)
        }
    }
}

/// 백업 파일을 지울 노드: 파일을 쓴 노드(`status.node`, 공유가 아닌 풀)나 백업 풀을 쓸 수 있는 노드.
/// 그 노드가 오프라인이면 409 `node_offline`, 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`.
fn files_node(state: &AppState, backup: &Backup, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let Some(node) = backup.status.node else {
        return pool_node(state, pool);
    };
    check_online(state, node)?;
    check_pool_on(state, pool, node, || {
        format!(
            "storage pool {:?} is not available on node {:?} holding backup {:?}",
            pool.name,
            node_name(state, node).unwrap_or_else(|_| node.to_string()),
            backup.name
        )
    })?;
    Ok(node)
}

/// 백업 파일과 리소스를 지울 Task를 돌려준다(202). 실패한 백업도 지운다. 백업을 만들거나 되살리거나
/// 지우는 Task가 끝나지 않았으면 409 `backup_busy`. Task는 파일이 있는 노드(공유가 아닌 풀은 파일을
/// 쓴 노드, 공유 풀은 백업 풀을 쓸 수 있는 노드)에서 돈다. 풀이 없어졌으면 이 노드의 Task가 리소스만
/// 지운다. 파일을 지울 노드가 없으면 409(`node_offline`, `pool_unavailable` 등)이고, `force`면 리소스만
/// 바로 지우고 200이다(파일은 백업 풀에 남는다).
#[utoipa::path(delete, path = "/backups/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), DeleteBackupQuery),
    responses(
        (status = 202, body = Resource<TaskSpec, TaskStatus>),
        (status = 200, body = BackupForgotten, description = "force: only the resource was deleted"),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn delete_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(q): Query<DeleteBackupQuery>,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    if q.force {
        check_backup_idle(&state, &backup)?;
        if let Err(e) = delete_node(&state, &backup)
            && e.status == StatusCode::CONFLICT
        {
            tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, reason = %e.message, "deleting backup without its files");
            state.cluster.delete::<BackupSpec>(backup.id, None).await?;
            let message = format!(
                "backup {:?} deleted; its files are left in storage pool {:?} ({})",
                backup.name, backup.spec.pool, e.message
            );
            return Ok(Json(BackupForgotten { message }).into_response());
        }
    }
    let task = start_backup_delete(&state, &backup).await?;
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// 백업을 만들거나 되살리거나 지우는 Task가 끝나지 않았으면 409 `backup_busy`
fn check_backup_idle(state: &AppState, backup: &Backup) -> Result<(), ApiError> {
    match backup_tasks(state, backup.id)?.first() {
        Some(doing) => Err(backup_busy(backup, doing)),
        None => Ok(()),
    }
}

/// 백업 지우기 Task를 돌릴 노드. 풀이 없어졌으면(파일도 함께 없어졌다) 이 노드다.
fn delete_node(state: &AppState, backup: &Backup) -> Result<Uuid, ApiError> {
    match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)?
    {
        None => Ok(state.node),
        Some(pool) => files_node(state, backup, &pool),
    }
}

/// 백업 파일과 리소스를 지울 Task를 만든다. 오류는 `force` 없는 [`delete_backup`]과 같다. 백업 지우기
/// API와 백업 작업의 보존이 쓴다(권한 검사는 부르는 쪽).
pub(crate) async fn start_backup_delete(
    state: &AppState,
    backup: &Backup,
) -> Result<Task, ApiError> {
    check_backup_idle(state, backup)?;
    let node = delete_node(state, backup)?;
    create_task(&state.cluster, TaskOp::BackupDelete, backup.id, node).await
}

/// `ready` 백업을 새 VM `name`으로 되살릴 Task를 돌려준다(202). Task의 대상은 미리 정한 새 VM id다.
/// 백업의 VM 스펙이 쓰는 네트워크·보안 그룹이 지금 있어야 한다(400). 백업이 준비되지 않았으면 409
/// `backup_not_ready`, 지우는 중이면 409 `backup_busy`, 이름이 겹치면 409 `name_taken`. 노드는 `node`나
/// 백업 풀과 대상 풀을 함께 쓸 수 있는 온라인 노드 가운데 이름이 가장 앞선 노드다(없으면 409 `no_node`).
/// 공유가 아닌 풀의 백업은 파일을 쓴 노드(`status.node`)에서만 되살린다: `node`가 다르면 409
/// `backup_on_other_node`. 고른 노드가 오프라인이면 409 `node_offline`.
#[utoipa::path(post, path = "/backups/{key}/restore", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    request_body = RestoreRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn restore_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<RestoreRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&req.name)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    // 이름이 그새 쓰이면 Task가 VM을 만들 때 실패한다.
    if state.cluster.get_by_name::<VmSpec>(&req.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", req.name),
        ));
    }
    if backup.status.phase != BackupPhase::Ready {
        return Err(ApiError::conflict(
            "backup_not_ready",
            format!("backup {:?} is not ready", backup.name),
        ));
    }
    if backup_tasks(&state, backup.id)?.contains(&"being deleted") {
        return Err(backup_busy(&backup, "being deleted"));
    }
    let spec = &backup.spec.vm_spec;
    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if state
            .cluster
            .get_by_name::<NetworkSpec>(&nic.network)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
    }
    check_security_groups(&state, spec)?;
    let source = find_pool(&state, &backup.spec.pool)?;
    let target = find_pool(&state, &req.pool)?;
    let node = match (&req.node, backup.status.node) {
        (key, Some(stored)) => {
            if let Some(key) = key
                && lookup::<NodeSpec>(&state.cluster, key)?.id != stored
            {
                return Err(ApiError::conflict(
                    "backup_on_other_node",
                    format!(
                        "backup {:?} is stored on node {:?}",
                        backup.name,
                        node_name(&state, stored)?
                    ),
                ));
            }
            check_online(&state, stored)?;
            let name = node_name(&state, stored)?;
            for pool in [&source, &target] {
                check_pool_on(&state, pool, stored, || {
                    format!(
                        "storage pool {:?} is not available on node {:?}",
                        pool.name, name
                    )
                })?;
            }
            stored
        }
        (Some(key), None) => {
            let node = lookup::<NodeSpec>(&state.cluster, key)?;
            check_online(&state, node.id)?;
            for pool in [&source, &target] {
                check_pool_on(&state, pool, node.id, || {
                    format!(
                        "storage pool {:?} is not available on node {:?}",
                        pool.name, node.name
                    )
                })?;
            }
            node.id
        }
        (None, None) => first_node(&state, |n| {
            pool_usable_on(&source, n) && pool_usable_on(&target, n)
        })?
        .ok_or_else(|| {
            ApiError::conflict(
                "no_node",
                format!(
                    "no online node can use both storage pools {:?} and {:?}",
                    source.name, target.name
                ),
            )
        })?,
    };
    let op = TaskOp::BackupRestore {
        backup: backup.id,
        name: req.name,
        pool: target.name,
    };
    let task = create_task(&state.cluster, op, Uuid::now_v7(), node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
