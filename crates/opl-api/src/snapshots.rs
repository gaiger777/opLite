//! VM 스냅샷 목록·조회·만들기·되돌리기·삭제. 권한은 VM을 따른다(보기는 Read, 나머지는 Operate).
//! 스냅샷이나 꺼진 VM에서 VM 복제(VM·볼륨 Write).
//! 실제 작업은 VM이 있는 노드의 Task 실행기가 한다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, VolumePhase, VolumeSpec, is_shared,
    pool_kind_supports_snapshots,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::{List, check_name, lookup};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vms::{is_stopped, prepare};
use crate::volumes::{check_pool_on, usable_node};

/// 설명의 최대 글자 수
const MAX_DESCRIPTION_CHARS: usize = 1024;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSnapshotRequest {
    /// VM 안에서 고유한 이름
    pub name: String,
    /// 최대 1024자
    #[serde(default)]
    pub description: String,
    /// 메모리 상태도 담는다. 기본은 VM이 실행 중이고 디스크가 있으면 true다.
    pub memory: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneRequest {
    /// 새 VM의 이름
    pub name: String,
    /// 복제할 스냅샷(이름이나 id). 없으면 꺼진 VM의 지금 상태에서 복제한다.
    pub snapshot: Option<String>,
    /// 새 디스크를 둘 풀. 없으면 원본 볼륨마다 그 볼륨의 풀이다.
    pub pool: Option<String>,
}

/// VM의 디스크 작업을 할 노드: 다음에 돌 노드(`spec.node`), 지금(또는 마지막으로) 돈 노드
/// (`status.node`), 배치된 적이 없으면 첫 디스크 볼륨을 만든 노드. 꺼진 VM을 다른 노드로 옮겼으면
/// 그 디스크는 옮긴 노드에서 쓰인다. 첫 디스크가 공유 풀에 있으면 볼륨을 만든 노드 대신 풀을 쓸 수
/// 있는 온라인 노드(`prefer`가 그런 노드면 그 노드, 아니면 id가 가장 작은 노드)다. 정할 수 없으면 `None`.
pub(crate) fn vm_placement(cluster: &Cluster, prefer: Option<Uuid>, vm: &Vm) -> Option<Uuid> {
    vm.spec.node.or(vm.status.node).or_else(|| {
        let disk = vm.spec.disks.first()?;
        let volume = cluster.get_by_name::<VolumeSpec>(&disk.volume).ok()??;
        match cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
            .ok()?
        {
            Some(pool) if is_shared(&pool) => usable_node(cluster, &pool, prefer).ok()?,
            _ => volume.status.node,
        }
    })
}

/// 스냅샷 Task를 실행할 노드: [`vm_placement`], 없으면 요청을 받은 이 노드(설계 §3).
pub(crate) fn vm_node(state: &AppState, vm: &Vm) -> Uuid {
    vm_placement(&state.cluster, Some(state.node), vm).unwrap_or(state.node)
}

/// 시작·정지 중인 VM은 스냅샷 작업을 받지 않는다: phase가 Starting·Stopping이거나, 원하는 전원
/// 상태(`spec.power`)에 아직 닿지 않았거나(reconciler가 켜거나 끄는 중), 전원 Task(시작·정지·
/// 강제 정지·재시작)가 끝나지 않았으면 409 `vm_busy`. 전원 조작이 스냅샷 Task를 기다리는
/// [`check_no_snapshot_task`]의 반대쪽이다.
fn check_not_busy(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let settled = match vm.spec.power {
        PowerState::Running => vm.status.phase == VmPhase::Running,
        PowerState::Stopped => is_stopped(vm),
    };
    let doing = match vm.status.phase {
        VmPhase::Starting => Some("starting"),
        VmPhase::Stopping => Some("stopping"),
        _ if !settled => Some(match vm.spec.power {
            PowerState::Running => "starting",
            PowerState::Stopped => "stopping",
        }),
        _ => None,
    };
    if let Some(doing) = doing {
        return Err(ApiError::conflict(
            "vm_busy",
            format!("VM {:?} is {doing}; try again when it has settled", vm.name),
        ));
    }
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                TaskOp::VmStart | TaskOp::VmStop | TaskOp::VmForceStop | TaskOp::VmReboot
            )
    });
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is being started, stopped or rebooted; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}

/// 이 VM의 스냅샷 만들기·되돌리기나 복제 Task가 끝나지 않았으면 409 `vm_busy`. 스냅샷 Task는
/// 전원을 끄고 켜며 디스크를 바꾸므로, 그사이 전원이나 스펙을 바꾸면 되돌리는 디스크를 QEMU가 열거나
/// 담는 스펙이 실제와 달라진다. 꺼진 VM을 복사하는 동안 켜면 디스크 잠금 때문에 시작이 실패한다. 시작한 Task는 실행기의 시간 제한 안에 끝나지만, 오프라인 노드에 배정된
/// Task는 그 노드가 돌아올 때까지 남는다(설계 §11).
pub(crate) fn check_no_snapshot_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(state, vm, "being snapshotted, reverted or cloned", |op| {
        matches!(
            op,
            TaskOp::SnapshotCreate { .. } | TaskOp::SnapshotRevert { .. } | TaskOp::VmClone { .. }
        )
    })
}

/// 이 VM의 스냅샷 되돌리기 Task가 끝나지 않았으면 409 `vm_busy`. 강제 정지는 이것만 막는다
/// (설계 §4). 만들기 중에 끄면 저장이 끊겨 만들기가 실패하고, 복제는 꺼진 VM이거나 실행 중인
/// VM의 스냅샷을 `-U`로 읽으므로 꺼도 된다. 되돌리기는 디스크를 바꾸는 중에 끄고 켜는 것이 섞인다.
pub(crate) fn check_no_revert_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(state, vm, "being reverted to a snapshot", |op| {
        matches!(op, TaskOp::SnapshotRevert { .. })
    })
}

fn check_no_task(
    state: &AppState,
    vm: &Vm,
    doing: &str,
    op: impl Fn(&TaskOp) -> bool,
) -> Result<(), ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks
        .iter()
        .any(|t| t.spec.target == vm.id && !t.status.state.is_finished() && op(&t.spec.op));
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is {doing}; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}

/// 디스크 스냅샷을 LV 스냅샷으로 두는 풀(`lvm-thin`)인지. 아니면 qcow2 내부 스냅샷이다.
pub(crate) fn uses_lv_snapshots(pool: &StoragePool) -> bool {
    matches!(pool.spec.backend, PoolBackend::LvmThin { .. })
}

/// VM의 디스크 중 LV 스냅샷을 쓰는 것([`uses_lv_snapshots`])이 있는지. 없는 볼륨·풀은 넘어간다.
pub(crate) fn vm_uses_lv_snapshots(cluster: &Cluster, vm: &Vm) -> Result<bool, ClusterError> {
    for disk in &vm.spec.disks {
        let Some(volume) = cluster.get_by_name::<VolumeSpec>(&disk.volume)? else {
            continue;
        };
        if let Some(pool) = cluster.get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
            && uses_lv_snapshots(&pool)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 모든 디스크가 스냅샷을 담을 수 있고 다 만들어졌는지. 풀 종류마다 담는 형식이 다르다
/// ([`pool_kind_supports_snapshots`]: 파일 풀은 qcow2, LVM-thin 풀은 raw). 담지 못하는 디스크가
/// 하나라도 있으면 일부 디스크만 되돌아가므로 거부하고, 방식(qcow2 내부 스냅샷, LV 스냅샷)이
/// 섞여도 거부한다. 디스크가 LV 스냅샷을 쓰면 true.
fn check_disks(state: &AppState, vm: &Vm) -> Result<bool, ApiError> {
    let mut lvm = None;
    for disk in &vm.spec.disks {
        let volume = state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ApiError::bad_request(format!("volume {:?} not found", disk.volume)))?;
        let pool = state
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
            .ok_or_else(|| {
                ApiError::bad_request(format!("storage pool {:?} not found", volume.spec.pool))
            })?;
        if !pool_kind_supports_snapshots(&pool.spec.backend, volume.spec.format) {
            let only = if uses_lv_snapshots(&pool) {
                "raw"
            } else {
                "qcow2"
            };
            return Err(ApiError::conflict(
                "snapshot_unsupported",
                format!(
                    "VM {:?} has disk {:?} that cannot be snapshotted; only {only} disks in \
                     storage pool {:?} can be snapshotted",
                    vm.name, disk.volume, pool.name
                ),
            ));
        }
        let this = uses_lv_snapshots(&pool);
        if lvm.replace(this).is_some_and(|before| before != this) {
            return Err(ApiError::conflict(
                "snapshot_unsupported",
                format!(
                    "VM {:?} cannot be snapshotted: disks use different snapshot methods",
                    vm.name
                ),
            ));
        }
        if volume.status.phase != VolumePhase::Ready {
            return Err(ApiError::conflict(
                "volume_not_ready",
                format!("volume {:?} is still being created", disk.volume),
            ));
        }
    }
    Ok(lvm == Some(true))
}

/// VM의 스냅샷 전부
fn snapshots_of(state: &AppState, vm: Uuid) -> Result<(u64, Vec<Snapshot>), ApiError> {
    let (revision, items) = state.cluster.list::<SnapshotSpec>()?;
    Ok((
        revision,
        items.into_iter().filter(|s| s.spec.vm == vm).collect(),
    ))
}

/// 경로의 `{name}`은 VM 안의 스냅샷 이름이나 id다.
fn find_snapshot(state: &AppState, vm: &Vm, key: &str) -> Result<Snapshot, ApiError> {
    let id = Uuid::try_parse(key).ok();
    snapshots_of(state, vm.id)?
        .1
        .into_iter()
        .find(|s| Some(s.id) == id || s.spec.name == key)
        .ok_or_else(|| {
            ApiError::not_found(format!("snapshot {key:?} of VM {:?} not found", vm.name))
        })
}

fn check_ready(snapshot: &Snapshot) -> Result<(), ApiError> {
    if snapshot.status.phase != SnapshotPhase::Ready {
        return Err(ApiError::conflict(
            "snapshot_not_ready",
            format!("snapshot {:?} is still being created", snapshot.spec.name),
        ));
    }
    Ok(())
}

/// 모든 VM의 스냅샷. watch와 함께 쓰는 목록이다.
#[utoipa::path(get, path = "/snapshots", tag = "snapshots", responses(
    (status = 200, body = List<Resource<SnapshotSpec, SnapshotStatus>>),
    (status = "4XX", body = crate::error::ErrorBody),
))]
pub async fn list_snapshots(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<Snapshot>>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let (revision, items) = state.cluster.list::<SnapshotSpec>()?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/vms/{key}/snapshots", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name")),
    responses(
        (status = 200, body = List<Resource<SnapshotSpec, SnapshotStatus>>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn list_vm_snapshots(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<List<Snapshot>>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let (revision, items) = snapshots_of(&state, vm.id)?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/vms/{key}/snapshots/{name}", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses(
        (status = 200, body = Resource<SnapshotSpec, SnapshotStatus>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn get_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<Json<Snapshot>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    Ok(Json(find_snapshot(&state, &vm, &name)?))
}

/// `pending` 스냅샷을 만들고(부모는 VM의 현재 위치, `vm_spec`은 지금 스펙) 그것을 채울 Task를
/// 돌려준다(202). 시작·정지 중이면 409 `vm_busy`, 스냅샷을 담지 못하는 디스크가 있거나 디스크의
/// 스냅샷 방식이 섞였거나 LVM-thin 디스크의 VM에 메모리 스냅샷을 요청하면 409
/// `snapshot_unsupported`. 실행 중이 아니거나 디스크가 없는 VM의 메모리 스냅샷은 400.
#[utoipa::path(post, path = "/vms/{key}/snapshots", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CreateSnapshotRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CreateSnapshotRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    check_name(&req.name)?;
    if req.description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(ApiError::bad_request(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let task = start_create(&state, &vm, req.name, req.description, req.memory, false).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`create_snapshot`]의 본문: `pending` 스냅샷과 그것을 채울 Task를 만든다. 예약 작업도 쓴다
/// (`scheduled`). 이름과 설명은 부르는 쪽이 확인한다.
pub(crate) async fn start_create(
    state: &AppState,
    vm: &Vm,
    name: String,
    description: String,
    memory: Option<bool>,
    scheduled: bool,
) -> Result<Task, ApiError> {
    check_not_busy(state, vm)?;
    let lvm = check_disks(state, vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let has_disk = !vm.spec.disks.is_empty();
    // LV 스냅샷에는 메모리 상태를 담을 qcow2가 없다.
    if lvm && memory == Some(true) {
        return Err(ApiError::conflict(
            "snapshot_unsupported",
            format!(
                "VM {:?} has lvm-thin disks; their snapshots cannot hold the memory state",
                vm.name
            ),
        ));
    }
    let memory = memory.unwrap_or(running && has_disk && !lvm);
    if memory && !running {
        return Err(ApiError::bad_request(format!(
            "VM {:?} is not running; only its disks can be snapshotted",
            vm.name
        )));
    }
    if memory && !has_disk {
        return Err(ApiError::bad_request(format!(
            "VM {:?} has no disk to hold the memory state",
            vm.name
        )));
    }
    let mut snapshot: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm: vm.id,
            name,
            parent: vm.status.snapshot,
            memory,
            description,
            scheduled,
            vm_spec: vm.spec.clone(),
        },
    );
    snapshot.name = snapshot.id.to_string();
    // 같은 VM에 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&snapshot).await?;
    let op = TaskOp::SnapshotCreate {
        snapshot: snapshot.id,
    };
    match create_task(&state.cluster, op, vm.id, vm_node(state, vm)).await {
        Ok(task) => Ok(task),
        Err(e) => {
            // 채울 Task가 없는 pending 스냅샷은 이름만 차지한다. 지우지 못하면 주인 노드의
            // Task 실행기가 다음 시작에 고아로 지운다.
            if let Err(d) = state
                .cluster
                .delete::<SnapshotSpec>(snapshot.id, None)
                .await
            {
                tracing::warn!(snapshot = %snapshot.id, error = %d, "cannot remove snapshot without a task");
            }
            Err(e)
        }
    }
}

/// 디스크·UEFI 변수·VM 스펙을 스냅샷 것으로 되돌릴 Task를 돌려준다(202). 실행 중이면 강제로 끈다.
/// 스냅샷 스펙의 볼륨이 모두 있고 다 만들어졌으며 다른 VM이 쓰지 않아야 한다.
#[utoipa::path(post, path = "/vms/{key}/snapshots/{name}/revert", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn revert_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_not_busy(&state, &vm)?;
    let snapshot = find_snapshot(&state, &vm, &name)?;
    check_ready(&snapshot)?;
    // 되돌린 스펙을 VM 수정처럼 검증한다. 노드는 지금 것을 유지하고, 시작은 Task가 한다.
    let mut spec = snapshot.spec.vm_spec.clone();
    spec.node = vm.spec.node;
    spec.power = PowerState::Stopped;
    prepare(&state, vm.id, &mut spec)?;
    let op = TaskOp::SnapshotRevert {
        snapshot: snapshot.id,
    };
    let task = create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 스냅샷을 지울 Task를 돌려준다(202). 자식은 지운 스냅샷의 부모로 옮겨 붙는다.
#[utoipa::path(delete, path = "/vms/{key}/snapshots/{name}", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let task = start_delete(&state, &vm, &name, false).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`delete_snapshot`]의 본문: VM의 스냅샷 `key`(이름이나 id)를 지울 Task를 만든다. 예약 작업도
/// 쓴다(`scheduled`).
pub(crate) async fn start_delete(
    state: &AppState,
    vm: &Vm,
    key: &str,
    scheduled: bool,
) -> Result<Task, ApiError> {
    check_not_busy(state, vm)?;
    let snapshot = find_snapshot(state, vm, key)?;
    check_ready(&snapshot)?;
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
        scheduled,
    };
    create_task(&state.cluster, op, vm.id, vm_node(state, vm)).await
}

/// VM을 `name`으로 복제할 Task를 돌려준다(202). 원본은 `ready` 스냅샷이나 꺼진 VM의 지금 상태다
/// (실행 중인데 스냅샷이 없으면 409 `vm_running`). 원본 스펙의 볼륨이 모두 있어야 하고, `pool`은 Task
/// 노드에서 쓸 수 있어야 한다. Task는 원본 VM을 대상으로 그 VM의 디스크 노드에서 돈다.
#[utoipa::path(post, path = "/vms/{key}/clone", tag = "vms",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CloneRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn clone_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CloneRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&req.name)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    // 이름이 그새 쓰이면 Task가 VM을 만들 때 실패한다.
    if state.cluster.get_by_name::<VmSpec>(&req.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", req.name),
        ));
    }
    check_not_busy(&state, &vm)?;
    let (snapshot, spec) = match &req.snapshot {
        Some(key) => {
            let snapshot = find_snapshot(&state, &vm, key)?;
            check_ready(&snapshot)?;
            (Some(snapshot.id), snapshot.spec.vm_spec)
        }
        None if !is_stopped(&vm) => {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "VM {:?} is running; stop it or clone from a snapshot",
                    vm.name
                ),
            ));
        }
        None => (None, vm.spec.clone()),
    };
    let node = vm_node(&state, &vm);
    if let Some(name) = &req.pool {
        let pool = state
            .cluster
            .get_by_name::<StoragePoolSpec>(name)?
            .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))?;
        check_pool_on(&state, &pool, node, || {
            format!(
                "storage pool {name:?} is not available on the node holding VM {:?}",
                vm.name
            )
        })?;
    }
    for disk in &spec.disks {
        if state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "volume {:?} not found",
                disk.volume
            )));
        }
    }
    // 새 VM은 원본 스펙의 NIC를 그대로 가진다(VM 만들기와 같은 검사).
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
    let op = TaskOp::VmClone {
        name: req.name,
        snapshot,
        pool: req.pool,
    };
    let task = create_task(&state.cluster, op, vm.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM의 스냅샷 수. VM 삭제를 막는 데 쓴다.
pub(crate) fn count_snapshots(state: &AppState, vm: Uuid) -> Result<usize, ApiError> {
    Ok(snapshots_of(state, vm)?.1.len())
}
