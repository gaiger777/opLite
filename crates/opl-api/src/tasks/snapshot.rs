//! 스냅샷 Task: 만들기·되돌리기·삭제(설계 §5). 같은 VM의 스냅샷 Task는 실행기가 차례로 돌린다.
//! API가 확인한 뒤 Task가 줄 서 기다리는 사이 VM이 바뀌었을 수 있으므로 여기서 다시 확인한다.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{DriverError, uefi_vars_path, vm_state_dir};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp};
use opl_types::vm::{PowerState, ScheduleError, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;

use crate::schedule::set_schedule_error;

use super::{Ctx, set_power, set_volume_size, started, stopped, superseded, wait_vm};

pub(super) fn get_snapshot(ctx: &Ctx, id: Uuid) -> Result<Option<Snapshot>, String> {
    ctx.cluster
        .get::<SnapshotSpec>(id)
        .map_err(|e| e.to_string())
}

pub(super) fn get_vm(ctx: &Ctx, id: Uuid) -> Result<Vm, String> {
    ctx.cluster
        .get::<VmSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "VM was deleted".into())
}

/// 이름으로 찾은 볼륨과 그 풀. 볼륨이 없으면 `None`.
fn volume_by_name(ctx: &Ctx, name: &str) -> Result<Option<(Volume, StoragePool)>, String> {
    let Some(volume) = ctx
        .cluster
        .get_by_name::<VolumeSpec>(name)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok(Some((volume, pool)))
}

/// VM 스펙의 디스크 볼륨들. 모두 있고 다 만들어졌어야 한다.
pub(super) fn ready_disks(ctx: &Ctx, spec: &VmSpec) -> Result<Vec<(Volume, StoragePool)>, String> {
    let mut disks = Vec::with_capacity(spec.disks.len());
    for disk in &spec.disks {
        let (volume, pool) = volume_by_name(ctx, &disk.volume)?
            .ok_or_else(|| format!("volume {:?} not found", disk.volume))?;
        if volume.status.phase != VolumePhase::Ready {
            return Err(format!("volume {:?} is not ready", volume.name));
        }
        disks.push((volume, pool));
    }
    Ok(disks)
}

/// 시작·정지 중인 VM은 스냅샷 작업을 받지 않는다.
pub(super) fn check_settled(vm: &Vm) -> Result<(), String> {
    let doing = match vm.status.phase {
        VmPhase::Starting => "starting",
        VmPhase::Stopping => "stopping",
        _ => return Ok(()),
    };
    Err(format!("VM is {doing}; try again when it has settled"))
}

/// 스냅샷의 UEFI 변수 사본 `{state_dir}/vm/{vm}/snapshots/{snapshot}.vars`
pub(super) fn vars_copy(ctx: &Ctx, vm: Uuid, snapshot: Uuid) -> Option<PathBuf> {
    let dir = ctx.config.state_dir.as_ref()?;
    Some(
        vm_state_dir(dir, vm)
            .join("snapshots")
            .join(format!("{snapshot}.vars")),
    )
}

/// `from`을 `to`로 복사한다. 디렉터리가 없으면 만든다.
pub(super) async fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::copy(from, to).await.map(|_| ())
}

/// 이 노드에서 파일이 보이는 볼륨인지. 다른 노드에 만든 볼륨은 풀이 공유되지 않으면 오류다.
pub(super) fn check_local(ctx: &Ctx, volume: &Volume, pool: &StoragePool) -> Result<(), String> {
    match volume.status.node {
        Some(node) if node != ctx.config.node && !pool.spec.shared => {
            Err(format!("volume {:?} is on node {node}", volume.name))
        }
        _ => Ok(()),
    }
}

/// 디스크의 태그를 지운다. 실행 중이면 QEMU가 디스크를 쥐고 있으므로 하이퍼바이저로, 아니면
/// `spec`의 디스크마다 스토리지로 지운다. 태그나 볼륨이 없는 디스크는 그냥 넘어가고, 한 디스크가
/// 실패해도 나머지를 지운 뒤 첫 오류를 돌려준다. 다른 노드에 있는 볼륨은 이 노드에서 파일이 보이지
/// 않으므로(없는 것으로 보면 태그가 남는다) 오류다.
async fn remove_tags(
    ctx: &Ctx,
    vm: Uuid,
    running: bool,
    spec: &VmSpec,
    tag: &str,
) -> Result<(), String> {
    if running {
        return ctx
            .hv
            .snapshot_delete(vm, tag)
            .await
            .map_err(|e| e.to_string());
    }
    let mut result = Ok(());
    for disk in &spec.disks {
        let Some((volume, pool)) = volume_by_name(ctx, &disk.volume)? else {
            continue;
        };
        if let Err(e) = check_local(ctx, &volume, &pool) {
            if result.is_ok() {
                result = Err(e);
            }
            continue;
        }
        match ctx
            .storage
            .snapshot_delete(&pool.spec, volume.id, volume.spec.format, tag)
            .await
        {
            // 이 노드의 볼륨인데 파일이 없으면 태그도 없다.
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => {
                if result.is_ok() {
                    result = Err(format!("volume {:?}: {e}", volume.name));
                }
            }
        }
    }
    result
}

/// VM의 현재 위치(`status.snapshot`)를 `to(지금 위치)`로 바꾼다. reconciler도 status를 쓰므로
/// revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn set_position(
    ctx: &Ctx,
    vm: Uuid,
    to: impl Fn(Option<Uuid>) -> Option<Uuid>,
) -> Result<(), String> {
    loop {
        let current = get_vm(ctx, vm)?;
        let snapshot = to(current.status.snapshot);
        if snapshot == current.status.snapshot {
            return Ok(());
        }
        let status = VmStatus {
            snapshot,
            ..current.status.clone()
        };
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(vm, current.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `pending` 스냅샷을 채운다. 실행 중이면 하이퍼바이저로(메모리 포함 또는 디스크만), 정지했으면
/// 디스크마다 스토리지로 태그를 만들고, UEFI 변수를 복사한 뒤 `ready`로 바꾸고 VM의 현재 위치로
/// 삼는다. 실패를 확인했으면 만든 태그를 지운다(리소스는 실행기의 정리가 지운다). 응답을 기다리다
/// 시간이 다 됐으면 작업이 아직 돌 수 있으므로 태그를 두고 실패한다.
pub(super) async fn create(ctx: &Ctx, vm_id: Uuid, id: Uuid) -> Result<(), String> {
    let snapshot = get_snapshot(ctx, id)?.ok_or("snapshot was deleted")?;
    let vm = get_vm(ctx, vm_id)?;
    check_settled(&vm)?;
    let running = vm.status.phase == VmPhase::Running;
    if snapshot.spec.memory && !running {
        return Err("VM is no longer running; its memory cannot be saved".into());
    }
    let disks = ready_disks(ctx, &vm.spec)?;
    if let Some((volume, _)) = disks
        .iter()
        .find(|(v, _)| !ctx.storage.supports_snapshots(v.spec.format))
    {
        return Err(format!(
            "volume {:?} ({:?}) cannot be snapshotted",
            volume.name, volume.spec.format
        ));
    }
    if snapshot.spec.memory && disks.is_empty() {
        return Err("VM has no disk to hold the memory state".into());
    }

    // 부모와 VM 스펙은 실행하는 지금의 것이다. 앞에 줄 선 스냅샷 Task가 현재 위치를 바꿨을 수 있다.
    let mut spec = snapshot.spec.clone();
    spec.parent = vm.status.snapshot;
    spec.vm_spec = vm.spec.clone();
    if spec != snapshot.spec {
        ctx.cluster
            .update_spec(id, Some(snapshot.revision), snapshot.labels.clone(), &spec)
            .await
            .map_err(|e| e.to_string())?;
    }

    let tag = id.to_string();
    let made = if !running {
        let mut result = Ok(());
        for (volume, pool) in &disks {
            result = ctx
                .storage
                .snapshot_create(&pool.spec, volume.id, volume.spec.format, &tag)
                .await;
            if result.is_err() {
                break;
            }
        }
        result
    } else if disks.is_empty() {
        // 디스크 없는 VM은 설정만 담는다.
        Ok(())
    } else {
        ctx.hv.snapshot_save(vm_id, &tag, spec.memory).await
    };
    let copied = match (&made, ctx.config.state_dir.as_ref()) {
        (Ok(()), Some(dir)) => {
            let vars = uefi_vars_path(dir, vm_id);
            match tokio::fs::try_exists(&vars).await {
                // BIOS VM이거나 한 번도 켠 적 없는 UEFI VM
                Ok(false) => Ok(()),
                Ok(true) => copy_file(&vars, &vars_copy(ctx, vm_id, id).unwrap()).await,
                Err(e) => Err(e),
            }
        }
        _ => Ok(()),
    };
    let failure = match (made, copied) {
        (Err(e), _) if e.may_be_in_progress() => {
            return Err(format!(
                "{e}; the snapshot may still be written to the disks and its tag {tag} is left there"
            ));
        }
        (Err(e), _) => Some(e.to_string()),
        (Ok(()), Err(e)) => Some(format!("cannot copy UEFI variables: {e}")),
        (Ok(()), Ok(())) => None,
    };
    if let Some(error) = failure {
        if let Err(e) = remove_tags(ctx, vm_id, running, &vm.spec, &tag).await {
            tracing::warn!(snapshot = %id, error = %e, "cannot remove tags of failed snapshot");
        }
        if let Some(copy) = vars_copy(ctx, vm_id, id) {
            let _ = tokio::fs::remove_file(copy).await;
        }
        return Err(error);
    }

    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(now_secs()),
        vmstate_bytes: None,
    };
    // 다 만든 스냅샷이다. 실패로 끝내면 정리하면서 지우게 되므로 기록될 때까지 다시 시도한다.
    loop {
        match ctx.cluster.update_status::<SnapshotSpec>(id, &status).await {
            Ok(_) => break,
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(snapshot = %id, error = %e, "cannot mark snapshot ready; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
    set_position(ctx, vm_id, |_| Some(id)).await
}

/// 예약으로 만든 스냅샷의 만들기·삭제 Task면 그 VM
pub(super) fn scheduled_vm(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let (TaskOp::SnapshotCreate { snapshot } | TaskOp::SnapshotDelete { snapshot, .. }) =
        task.spec.op
    else {
        return None;
    };
    get_snapshot(ctx, snapshot)
        .ok()
        .flatten()
        .filter(|s| s.spec.scheduled)
        .map(|s| s.spec.vm)
}

/// 예약 스냅샷 만들기·삭제의 결과를 VM의 `schedule_error`에 남긴다. 성공(`None`)이면 지운다.
pub(super) async fn record_schedule(ctx: &Ctx, vm: Uuid, error: Option<ScheduleError>) {
    if let Err(e) = set_schedule_error(&ctx.cluster, vm, error).await {
        tracing::warn!(vm = %vm, error = %e, "cannot record schedule error");
    }
}

/// 실패한 만들기의 `pending` 리소스를 지운다. 이미 `ready`면 그대로 둔다.
pub(super) async fn discard_pending(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    match get_snapshot(ctx, id)? {
        Some(s) if s.status.phase == SnapshotPhase::Pending => delete_resource(ctx, &s).await,
        _ => Ok(()),
    }
}

async fn delete_resource(ctx: &Ctx, snapshot: &Snapshot) -> Result<(), String> {
    match ctx.cluster.delete::<SnapshotSpec>(snapshot.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 이전 opld가 끝내지 못한 `pending` 스냅샷의 태그(있으면)와 리소스를 지운다. 태그를 지우지
/// 못해도 리소스는 지운다(남은 태그는 공간만 쓴다).
pub(super) async fn discard(ctx: &Ctx, snapshot: &Snapshot) -> Result<(), String> {
    let vm = snapshot.spec.vm;
    let running = ctx
        .hv
        .inspect(vm)
        .await
        .map_err(|e| e.to_string())?
        .is_some();
    let tag = snapshot.id.to_string();
    if let Err(e) = remove_tags(ctx, vm, running, &snapshot.spec.vm_spec, &tag).await {
        tracing::warn!(snapshot = %snapshot.id, error = %e, "cannot remove tags of unfinished snapshot");
    }
    if let Some(copy) = vars_copy(ctx, vm, snapshot.id) {
        let _ = tokio::fs::remove_file(copy).await;
    }
    delete_resource(ctx, snapshot).await
}

/// VM을 스냅샷 때로 되돌린다(설계 §5): 강제로 끄고, 스펙을 스냅샷의 것으로(노드는 그대로) 바꾸고,
/// 디스크와 UEFI 변수를 되돌리고, 볼륨의 실제 크기를 다시 읽는다. 현재 위치를 이 스냅샷으로
/// 바꾼 뒤, 메모리 스냅샷이면 그 메모리를 불러오며 시작한다.
pub(super) async fn revert(ctx: &Ctx, task: &Task, id: Uuid) -> Result<(), String> {
    let vm_id = task.spec.target;
    let snapshot = get_snapshot(ctx, id)?.ok_or("snapshot was deleted")?;
    if snapshot.status.phase != SnapshotPhase::Ready {
        return Err("snapshot is not ready".into());
    }
    // 되돌릴 수 없는데 VM만 꺼지지 않도록 끄기 전에 볼륨을 확인한다: 이 노드에서 보이는지, API가
    // 확인한 뒤 다른 VM이 붙이지 않았는지.
    let disks = ready_disks(ctx, &snapshot.spec.vm_spec)?;
    for (volume, pool) in &disks {
        check_local(ctx, volume, pool)?;
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    for (volume, _) in &disks {
        if let Some(other) = vms
            .iter()
            .find(|vm| vm.id != vm_id && vm.spec.disks.iter().any(|d| d.volume == volume.name))
        {
            return Err(format!(
                "volume {:?} is attached to VM {:?}",
                volume.name, other.name
            ));
        }
    }
    let tag = id.to_string();

    let vm = get_vm(ctx, vm_id)?;
    if vm.spec.power != PowerState::Stopped || !is_down(&vm) {
        set_power(ctx, vm_id, PowerState::Stopped).await?;
        ctx.reconciler.force_stop(vm_id);
        wait_vm(ctx, vm_id, stopped).await?;
    }
    write_reverted_spec(ctx, vm_id, &snapshot).await?;

    // 여기서 멈추면 VM은 꺼진 채 남는다. 같은 되돌리기를 다시 하면 처음부터 한다(설계 §6).
    for (volume, pool) in &disks {
        // API는 이 Task가 도는 동안 전원 조작을 막지만, 그 확인과 겹친 요청이 켰을 수 있다.
        // QEMU가 연 디스크를 qemu-img로 바꾸면 안 된다.
        if get_vm(ctx, vm_id)?.spec.power != PowerState::Stopped {
            return Err("VM was started again".into());
        }
        ctx.storage
            .snapshot_revert(&pool.spec, volume.id, volume.spec.format, &tag)
            .await
            .map_err(|e| format!("cannot revert volume {:?}: {e}", volume.name))?;
    }
    if let (Some(copy), Some(dir)) = (vars_copy(ctx, vm_id, id), &ctx.config.state_dir)
        && tokio::fs::try_exists(&copy).await.unwrap_or(false)
    {
        copy_file(&copy, &uefi_vars_path(dir, vm_id))
            .await
            .map_err(|e| format!("cannot restore UEFI variables: {e}"))?;
    }
    // 스냅샷 뒤에 늘린 크기는 되돌아간다.
    for (volume, pool) in &disks {
        record_size(ctx, volume, pool).await;
    }
    // 디스크가 스냅샷 때로 돌아갔으므로 메모리를 불러오지 못하더라도 현재 위치는 이 스냅샷이다.
    set_position(ctx, vm_id, |_| Some(id)).await?;

    if snapshot.spec.memory {
        // 뒤에 온 전원 요청(정지 등)이 있으면 그것을 따르고 켜지 않는다. 메모리를 불러오며 켜면
        // 그 뒤의 정지를 덮는다.
        if superseded(ctx, task)? {
            return Err("superseded by a later request for this VM".into());
        }
        // power를 쓰기 전에 맡겨야 다음 시작이 받는다. 쓰지 못하면 다른 시작이 쓰지 않게 거둔다.
        ctx.reconciler.restore_on_start(vm_id, tag);
        if let Err(e) = set_power(ctx, vm_id, PowerState::Running).await {
            ctx.reconciler.clear_restore(vm_id);
            return Err(e);
        }
        wait_vm(ctx, vm_id, started).await?;
    }
    Ok(())
}

pub(super) fn is_down(vm: &Vm) -> bool {
    matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
}

/// VM 스펙을 스냅샷의 것으로 바꾼다. 노드와 예약은 지금 것을 두되, 메모리 스냅샷이면 이 노드에서 디스크를
/// 되돌리고 시작하므로 배치되지 않은 VM은 이 노드로 정한다. 끈 뒤 누가 다시 켰으면 디스크를
/// 건드리기 전에 그만둔다.
async fn write_reverted_spec(ctx: &Ctx, vm_id: Uuid, snapshot: &Snapshot) -> Result<(), String> {
    loop {
        let vm = get_vm(ctx, vm_id)?;
        if vm.spec.power != PowerState::Stopped || !is_down(&vm) {
            return Err("VM was started again".into());
        }
        let mut spec = snapshot.spec.vm_spec.clone();
        spec.node = vm.spec.node;
        // 예약은 스냅샷 때의 것이 아니라 지금 것을 지킨다.
        spec.snapshot_schedule = vm.spec.snapshot_schedule;
        if snapshot.spec.memory && spec.node.is_none() {
            spec.node = Some(ctx.config.node);
        }
        spec.power = PowerState::Stopped;
        match ctx
            .cluster
            .update_spec(vm_id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 볼륨의 실제 크기를 읽어 스펙에 반영한다. 실패해도 넘어간다(크기 표시만 틀린다).
async fn record_size(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
    let info = match ctx
        .storage
        .volume_info(&pool.spec, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            return;
        }
    };
    if let Err(e) = set_volume_size(ctx, volume.id, info.virtual_bytes).await {
        tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
    }
}

/// 스냅샷을 지운다: 디스크의 태그(실행 중이면 하이퍼바이저로, 그 뒤 떼어 낸 디스크는 스토리지로),
/// UEFI 변수 사본, 리소스. 자식은 지운 스냅샷의 부모로 옮겨 붙고, 현재 위치였으면 부모로 옮긴다.
/// 태그가 이미 없으면 성공으로 보므로 중간에 멈춰도 다시 하면 된다.
pub(super) async fn delete(ctx: &Ctx, vm_id: Uuid, id: Uuid) -> Result<(), String> {
    let Some(snapshot) = get_snapshot(ctx, id)? else {
        return Ok(());
    };
    let vm = ctx
        .cluster
        .get::<VmSpec>(vm_id)
        .map_err(|e| e.to_string())?;
    if let Some(vm) = &vm {
        check_settled(vm)?;
    }
    let tag = id.to_string();
    let running = vm.as_ref().filter(|vm| vm.status.phase == VmPhase::Running);
    let mut detached = snapshot.spec.vm_spec.clone();
    if let Some(vm) = running {
        remove_tags(ctx, vm_id, true, &vm.spec, &tag).await?;
        // 실행 중인 VM이 쓰는 디스크(실행 중에는 바뀌지 않는다)는 위에서 지웠다.
        detached
            .disks
            .retain(|d| !vm.spec.disks.iter().any(|a| a.volume == d.volume));
    }
    remove_tags(ctx, vm_id, false, &detached, &tag).await?;
    if let Some(copy) = vars_copy(ctx, vm_id, id) {
        match tokio::fs::remove_file(copy).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove UEFI variables copy: {e}")),
        }
    }

    let (_, all) = ctx
        .cluster
        .list::<SnapshotSpec>()
        .map_err(|e| e.to_string())?;
    for child in all.iter().filter(|s| s.spec.parent == Some(id)) {
        let mut spec = child.spec.clone();
        spec.parent = snapshot.spec.parent;
        match ctx
            .cluster
            .update_spec(child.id, None, child.labels.clone(), &spec)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if vm.is_some() {
        let parent = snapshot.spec.parent;
        set_position(ctx, vm_id, |at| if at == Some(id) { parent } else { at }).await?;
    }
    delete_resource(ctx, &snapshot).await
}
