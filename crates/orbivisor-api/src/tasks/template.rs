//! 연결 복제 평탄화(`volume-flatten`, 설계 §7)와 템플릿 되돌리기(`vm-untemplate`, 설계 §3) Task. 둘 다 저장소 작업을
//! 먼저 하고 Raft 기록을 마지막에 고치므로, 중간에 끊기면 다시 하면 된다(저장소 작업은 멱등이다).

use orbivisor_cluster::ClusterError;
use orbivisor_cluster::types::Rejection;
use orbivisor_types::node::NodeSpec;
use orbivisor_types::storage::{StoragePoolSpec, VolumeSpec, is_shared};
use orbivisor_types::task::Task;
use orbivisor_types::vm::VmSpec;
use uuid::Uuid;

use super::snapshot::get_vm;
use super::{Ctx, mark_ready, volume_and_pool};
use crate::vms::{is_stopped, vm_uses_volume};
use crate::volumes::{clone_names, linked_clones, template_in_use_message};

/// 연결 복제 볼륨 `id`를 기반과 떼어 독립 볼륨으로 만들고 기록의 `base`를 지운다. 이미 독립이면(앞 시도가 기록까지
/// 마쳤다) 할 일이 없다. 볼륨을 쓰는 VM은 꺼져 있어야 한다(API가 확인했지만 줄에서 기다리는 사이 켜졌을 수 있다).
/// 저장소 작업 뒤 기록 전에 끊기면 다시 할 때 `flatten_volume`이 할 일 없이 성공하고 기록만 고친다.
pub(super) async fn flatten(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    let Some(base) = volume.spec.base else {
        return Ok(());
    };
    if volume.status.phase != orbivisor_types::storage::VolumePhase::Ready {
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
/// 있다), 디스크마다 기반 스냅샷을 푼 뒤(`release_base`, 백엔드가 자식이 있으면 거절한다) `template: false`를 쓴다. 풀이
/// 없어졌거나 로컬 풀의 볼륨이 있던 노드가 클러스터에서 빠진 디스크는 풀 것이 없으므로 건너뛴다. 중간에 끊기면 표시가
/// 남으므로 다시 하면 된다.
///
/// 다른 노드의 배포 Task와 엇갈릴 수 있으므로 "쓰고 다시 보기"다: `false`를 쓴 뒤 연결 복제(만드는 중인 것 포함)를 다시
/// 보고, 있으면 `true`로 되돌리고 실패한다. 배포 Task는 복제 볼륨을 기록한 뒤 템플릿인지 다시 보므로 Raft 순서에서 둘
/// 중 뒤에 쓴 쪽이 앞의 것을 보고 물러난다. 기반 스냅샷을 푼 뒤 생긴 연결 복제는 `link_clone`이 기반 스냅샷을 다시 찍는다.
/// 되돌리기는 스펙이 이 Task가 쓴 그대로일 때만 한다(다른 쓰기를 덮지 않는다 — 그새 바뀌었으면 크게 실패한다). 쓰기가
/// 오류를 냈어도(적용됐는지 모름) VM을 다시 읽어 표시가 지워졌으면 다시 본다. 이미 보통 VM이면(끊긴 Task를 다시 하거나
/// 같은 VM의 다른 되돌리기가 먼저 마쳤다) 다시 보기만 한다.
pub(super) async fn untemplate(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let vm = get_vm(ctx, id)?;
    if !vm.spec.template {
        return recheck_clones(ctx, id, None).await.map(|()| None);
    }
    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let disks: Vec<_> = volumes
        .iter()
        .filter(|v| vm.spec.disks.iter().any(|d| d.volume == v.name))
        .collect();
    let bases: Vec<Uuid> = disks.iter().map(|d| d.id).collect();
    check_no_clones(ctx, &vm.name, &volumes, &bases)?;
    for disk in &disks {
        let Some(pool) = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&disk.spec.pool)
            .map_err(|e| e.to_string())?
        else {
            tracing::warn!(volume = %disk.name, pool = %disk.spec.pool, "the pool is gone; not releasing the base");
            continue;
        };
        if !is_shared(&pool)
            && let Some(node) = disk.status.node
            && ctx
                .cluster
                .get::<NodeSpec>(node)
                .map_err(|e| e.to_string())?
                .is_none()
        {
            tracing::warn!(volume = %disk.name, %node, "the node holding the volume left the cluster; not releasing the base");
            continue;
        }
        ctx.storage
            .release_base(&pool, disk.id, disk.spec.format)
            .await
            .map_err(|e| format!("cannot release the base of volume {:?}: {e}", disk.name))?;
    }
    // 다른 쓰기(메모리, 상태)를 덮지 않게 읽은 revision으로 쓴다.
    let (written, warnings) = loop {
        let vm = get_vm(ctx, id)?;
        if !vm.spec.template {
            return recheck_clones(ctx, id, None).await.map(|()| None);
        }
        let mut spec = VmSpec {
            template: false,
            ..vm.spec.clone()
        };
        // 보통 VM이 된 NIC는 SDN 주소를 새로 받는다(계획 65 §9). 빈 주소가 없으면 주소 없이 두고 Task 경고로 알린다
        // (리뷰 2 N-4 — 그 NIC의 OVN 포트는 꺼진다. `network ips --fill`이나 VM 편집으로 나중에 채운다).
        let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        let (_, networks) = ctx
            .cluster
            .list::<orbivisor_types::network::NetworkSpec>()
            .map_err(|e| e.to_string())?;
        let others: Vec<&orbivisor_types::vm::Vm> = vms.iter().filter(|v| v.id != id).collect();
        let held = crate::ipam::load_held(&ctx.cluster).map_err(|e| e.message)?;
        let warnings = match crate::ipam::assign_new(&mut spec, &others, &networks, &held) {
            Ok(notes) => notes,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot give the untemplated VM SDN addresses; leaving them empty");
                spec.nics.iter_mut().for_each(|n| n.ips.clear());
                vec![format!(
                    "{e}; its SDN interfaces have no address and their OVN ports stay off until they get one \
                     (`orbivisorctl network ips NETWORK --fill`, or edit the VM)"
                )]
            }
        };
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => break (spec, warnings),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            // 다른 노드가 그새 같은 주소를 잡았다: 다시 고른다.
            Err(e) if crate::ipam::is_ip_taken(&e) => {}
            Err(e) => {
                // 적용됐는지 모른다(시간 초과 뒤 적용). 표시가 남았으면 다시 하면 되고, 지워졌으면 다시 본다.
                if get_vm(ctx, id)?.spec.template {
                    return Err(format!("cannot record the VM: {e}"));
                }
                break (spec, warnings);
            }
        }
    };
    recheck_clones(ctx, id, Some(&written)).await?;
    Ok((!warnings.is_empty()).then(|| warnings.join("; ")))
}

/// 끊긴 되돌리기 Task(`unfinished`)의 다시 보기: 템플릿 표시가 이미 지워졌는데 연결 복제가 남았으면 그 오류다(표시는
/// 되돌리지 않는다 — 이 Task가 쓴 스펙을 모른다). VM이 없거나 아직 템플릿이면 할 일이 없다.
pub(super) async fn recheck_unfinished(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    match ctx.cluster.get::<VmSpec>(id) {
        Ok(Some(vm)) if !vm.spec.template => recheck_clones(ctx, id, None).await,
        _ => Ok(()),
    }
}

/// 템플릿 표시를 지운 뒤 연결 복제를 다시 본다. 남았으면 표시를 되돌리고 실패한다. 되돌리기는 VM 스펙이 `written`(이
/// Task가 쓴 스펙)과 같을 때만 한다. 그새 다른 쓰기가 있었거나 이 Task가 쓴 스펙을 모르면(`None`) 덮지 않고 크게 실패한다.
async fn recheck_clones(ctx: &Ctx, id: Uuid, written: Option<&VmSpec>) -> Result<(), String> {
    let vm = get_vm(ctx, id)?;
    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let bases: Vec<Uuid> = volumes
        .iter()
        .filter(|v| vm.spec.disks.iter().any(|d| d.volume == v.name))
        .map(|v| v.id)
        .collect();
    let Err(e) = check_no_clones(ctx, &vm.name, &volumes, &bases) else {
        return Ok(());
    };
    let name = vm.name.clone();
    let changed = || {
        format!(
            "{e}; VM {name:?} was left as a regular VM because it may have changed meanwhile — make it a template again"
        )
    };
    let mut vm = vm;
    loop {
        if vm.spec.template {
            return Err(e);
        }
        if written != Some(&vm.spec) {
            return Err(changed());
        }
        let spec = VmSpec {
            template: true,
            ..vm.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Err(e),
            // 상태 쓰기도 revision을 올린다. 다시 읽어 스펙이 그대로면 다시 쓴다.
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(err) => {
                return Err(format!(
                    "{e}; cannot make VM {:?} a template again: {err}",
                    vm.name
                ));
            }
        }
        vm = get_vm(ctx, id)?;
    }
}

/// 기반 `bases`의 연결 복제가 남았으면 그 이름을 담은 오류(쓰는 VM의 이름, 붙지 않은 복제는 볼륨 이름)
fn check_no_clones(
    ctx: &Ctx,
    template: &str,
    volumes: &[orbivisor_types::storage::Volume],
    bases: &[Uuid],
) -> Result<(), String> {
    let clones = linked_clones(volumes, bases);
    if clones.is_empty() {
        return Ok(());
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    Err(template_in_use_message(
        template,
        &clone_names(&clones, &vms),
    ))
}
