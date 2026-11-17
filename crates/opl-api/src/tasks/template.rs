//! 연결 복제 평탄화(`volume-flatten`, 설계 §7)와 템플릿 되돌리기(`vm-untemplate`, 설계 §3) Task. 둘 다 저장소 작업을
//! 먼저 하고 Raft 기록을 마지막에 고치므로, 중간에 끊기면 다시 하면 된다(저장소 작업은 멱등이다).

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::task::Task;
use opl_types::vm::VmSpec;
use uuid::Uuid;

use super::snapshot::get_vm;
use super::{Ctx, mark_ready, volume_and_pool};
use crate::vms::{is_stopped, vm_uses_volume};
use crate::volumes::template_in_use_message;

/// 연결 복제 볼륨 `id`를 기반과 떼어 독립 볼륨으로 만들고 기록의 `base`를 지운다. 이미 독립이면(앞 시도가 기록까지
/// 마쳤다) 할 일이 없다. 볼륨을 쓰는 VM은 꺼져 있어야 한다(API가 확인했지만 줄에서 기다리는 사이 켜졌을 수 있다).
/// 저장소 작업 뒤 기록 전에 끊기면 다시 할 때 `flatten_volume`이 할 일 없이 성공하고 기록만 고친다.
pub(super) async fn flatten(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    let Some(base) = volume.spec.base else {
        return Ok(());
    };
    if volume.status.phase != opl_types::storage::VolumePhase::Ready {
        return Err("volume is not ready".into());
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm_uses_volume(vm, &volume.name) && !is_stopped(vm))
    {
        return Err(format!(
            "VM {:?} is running; stop it before flattening its disk",
            vm.name
        ));
    }
    ctx.storage
        .flatten_volume(&pool, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot flatten volume {:?}: {e}", volume.name))?;
    tracing::info!(volume = %volume.name, base = %base, "flattened a linked clone");
    // 다른 쓰기(라벨, 크기)를 덮지 않게 읽은 revision으로 쓴다.
    loop {
        let (volume, pool) = volume_and_pool(ctx, id)?;
        if volume.spec.base.is_none() {
            // 기반의 데이터를 가져와 할당량이 늘었다.
            mark_ready(ctx, &volume, &pool).await;
            return Ok(());
        }
        let spec = VolumeSpec {
            base: None,
            ..volume.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(volume.revision), volume.labels.clone(), &spec)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(format!("cannot record the flattened volume: {e}")),
        }
    }
}

/// 템플릿(대상 VM)을 보통 VM으로 되돌린다: 연결 복제가 남았으면 실패하고(API가 확인한 뒤 복제 Task가 그새 만들었을 수
/// 있다), 디스크마다 기반 스냅샷을 푼 뒤(`release_base`, 백엔드가 자식이 있으면 거절한다) 마지막에 `template: false`를
/// 쓴다. 이미 보통 VM이면 할 일이 없다. 중간에 끊기면 표시가 남으므로 다시 하면 된다.
pub(super) async fn untemplate(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    let vm = get_vm(ctx, id)?;
    if !vm.spec.template {
        return Ok(());
    }
    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let disks: Vec<_> = volumes
        .iter()
        .filter(|v| vm.spec.disks.iter().any(|d| d.volume == v.name))
        .collect();
    let clones: Vec<_> = volumes
        .iter()
        .filter(|v| v.spec.base.is_some_and(|b| disks.iter().any(|d| d.id == b)))
        .collect();
    if !clones.is_empty() {
        let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        // 쓰는 VM의 이름, 붙지 않은 복제는 볼륨 이름
        let mut names: Vec<&str> = clones
            .iter()
            .map(|c| {
                vms.iter()
                    .find(|vm| vm.spec.disks.iter().any(|d| d.volume == c.name))
                    .map_or(c.name.as_str(), |vm| vm.name.as_str())
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        return Err(template_in_use_message(&vm.name, &names));
    }
    for disk in &disks {
        let pool = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&disk.spec.pool)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("storage pool {:?} not found", disk.spec.pool))?;
        ctx.storage
            .release_base(&pool, disk.id, disk.spec.format)
            .await
            .map_err(|e| format!("cannot release the base of volume {:?}: {e}", disk.name))?;
    }
    loop {
        let vm = get_vm(ctx, id)?;
        if !vm.spec.template {
            return Ok(());
        }
        let spec = VmSpec {
            template: false,
            ..vm.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(format!("cannot record the VM: {e}")),
        }
    }
}
