//! VM 스냅샷 목록·조회·만들기·되돌리기·삭제. 권한은 VM을 따른다(보기는 Read, 나머지는 Operate).
//! 실제 작업은 VM이 있는 노드의 Task 실행기가 한다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::Cluster;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{VolumePhase, VolumeSpec};
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
use crate::vms::prepare;

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

/// VM의 디스크 작업을 할 노드: 다음에 돌 노드(`spec.node`), 지금(또는 마지막으로) 돈 노드
/// (`status.node`), 배치된 적이 없으면 첫 디스크 볼륨을 만든 노드. 꺼진 VM을 다른 노드로 옮겼으면
/// 그 디스크는 옮긴 노드에서 쓰인다. 셋 다 없으면 `None`.
pub(crate) fn vm_placement(cluster: &Cluster, vm: &Vm) -> Option<Uuid> {
    vm.spec.node.or(vm.status.node).or_else(|| {
        let disk = vm.spec.disks.first()?;
        let volume = cluster.get_by_name::<VolumeSpec>(&disk.volume).ok()??;
        volume.status.node
    })
}

/// 스냅샷 Task를 실행할 노드: [`vm_placement`], 없으면 요청을 받은 이 노드(설계 §3).
pub(crate) fn vm_node(state: &AppState, vm: &Vm) -> Uuid {
    vm_placement(&state.cluster, vm).unwrap_or(state.node)
}

/// 시작·정지 중인 VM은 스냅샷 작업을 받지 않는다.
fn check_not_busy(vm: &Vm) -> Result<(), ApiError> {
    let doing = match vm.status.phase {
        VmPhase::Starting => "starting",
        VmPhase::Stopping => "stopping",
        _ => return Ok(()),
    };
    Err(ApiError::conflict(
        "vm_busy",
        format!("VM {:?} is {doing}; try again when it has settled", vm.name),
    ))
}

/// 모든 디스크가 스냅샷을 담을 수 있고 다 만들어졌는지. dir 백엔드(`Storage::supports_snapshots`)는
/// qcow2만 담는다. raw가 하나라도 있으면 일부 디스크만 되돌아가므로 거부한다.
fn check_disks(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    for disk in &vm.spec.disks {
        let volume = state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ApiError::bad_request(format!("volume {:?} not found", disk.volume)))?;
        if volume.spec.format != ImageFormat::Qcow2 {
            return Err(ApiError::conflict(
                "snapshot_unsupported",
                format!(
                    "VM {:?} has non-qcow2 disk {:?}; only qcow2 disks can be snapshotted",
                    vm.name, disk.volume
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
    Ok(())
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
/// 돌려준다(202). 시작·정지 중이면 409 `vm_busy`, qcow2가 아닌 디스크가 있으면 409
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
    check_not_busy(&vm)?;
    check_disks(&state, &vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let has_disk = !vm.spec.disks.is_empty();
    let memory = req.memory.unwrap_or(running && has_disk);
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
            name: req.name,
            parent: vm.status.snapshot,
            memory,
            description: req.description,
            scheduled: false,
            vm_spec: vm.spec.clone(),
        },
    );
    snapshot.name = snapshot.id.to_string();
    // 같은 VM에 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&snapshot).await?;
    let op = TaskOp::SnapshotCreate {
        snapshot: snapshot.id,
    };
    match create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await {
        Ok(task) => Ok((StatusCode::ACCEPTED, Json(task))),
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
    check_not_busy(&vm)?;
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
    check_not_busy(&vm)?;
    let snapshot = find_snapshot(&state, &vm, &name)?;
    check_ready(&snapshot)?;
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
    };
    let task = create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM의 스냅샷 수. VM 삭제를 막는 데 쓴다.
pub(crate) fn count_snapshots(state: &AppState, vm: Uuid) -> Result<usize, ApiError> {
    Ok(snapshots_of(state, vm)?.1.len())
}
