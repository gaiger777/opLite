//! VM 복제 Task(설계 §5 "복제"). 원본 VM의 줄에 서서 그 VM의 스냅샷 Task와 차례로 돈다. API가
//! 확인한 뒤 줄에서 기다리는 사이 원본 VM이나 스냅샷이 바뀌거나 없어졌을 수 있으므로 다시 확인한다.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use opl_cluster::ClusterError;
use opl_driver::uefi_vars_path;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::SnapshotPhase;
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumeSpec, pool_error_on, pool_listed_on, pool_usable_on,
};
use opl_types::task::Task;
use opl_types::vm::{MacAddr, PowerState, Vm, VmSpec};
use uuid::Uuid;

use super::snapshot::{
    check_local, check_settled, copy_file, get_snapshot, get_vm, is_down, ready_disks, vars_copy,
};
use super::{Ctx, mark_ready, remove_volume};
use crate::volumes::block_pool_kind;

/// 리소스 이름의 최대 길이
const MAX_NAME: usize = 63;

/// 복제 Task가 지금까지 만든 것. Task가 실패하거나 시간이 다 되면 실행기의 정리([`discard`])가
/// 지운다. opld가 죽으면 사라진다. 복사 중이던 볼륨은 아직 `pending`이라 다음 시작의 고아 정리가
/// 지우지만(볼륨은 모두 복사한 뒤 VM을 만들기 바로 전에 `ready`로 바꾼다), `ready`로 바꾼 뒤 VM을
/// 만들기 전에 죽으면 볼륨과 UEFI 변수 사본이 남는다(설계 §11).
#[derive(Default)]
pub(super) struct Made {
    volumes: Vec<Uuid>,
    vars: Option<PathBuf>,
}

fn made(ctx: &Ctx, task: Uuid, f: impl FnOnce(&mut Made)) {
    f(ctx.cloned.lock().unwrap().entry(task).or_default());
}

/// 새 볼륨 이름 `{vm}-{volume}`. 63자로 자르고, `taken`이면 `-2`, `-3`…을 붙인다(붙인 뒤에도 63자).
fn volume_name(vm: &str, volume: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{vm}-{volume}");
    // 이름은 ASCII다. 자른 끝이 `-`면 이름 규칙에 어긋나므로 뗀다.
    let cut = |max: usize| {
        base[..base.len().min(max)]
            .trim_end_matches('-')
            .to_string()
    };
    let first = cut(MAX_NAME);
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            format!("{}{suffix}", cut(MAX_NAME - suffix.len()))
        })
        .find(|name| !taken(name))
        .expect("some suffix is free")
}

/// 복사본의 형식(설계 §5): 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw만 담는다. 블록 볼륨(raw)을
/// 파일 풀로 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
fn copy_format(source: &StoragePool, target: &StoragePool, format: ImageFormat) -> ImageFormat {
    match (
        block_pool_kind(source).is_some(),
        block_pool_kind(target).is_some(),
    ) {
        (_, true) => ImageFormat::Raw,
        (true, false) => ImageFormat::Qcow2,
        (false, false) => format,
    }
}

fn pool_by_name(ctx: &Ctx, name: &str) -> Result<StoragePool, String> {
    ctx.cluster
        .get_by_name::<StoragePoolSpec>(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {name:?} not found"))
}

/// 원본(스냅샷이나 꺼진 VM의 지금 상태)의 디스크를 이 노드에서 새 볼륨으로 복사하고 UEFI 변수를
/// 복사한 뒤 새 VM을 만든다. 새 VM은 꺼져 있고 노드·예약·스냅샷이 없으며 MAC은 새로 받는다.
pub(super) async fn clone(
    ctx: &Ctx,
    task: &Task,
    name: &str,
    snapshot: Option<Uuid>,
    pool: Option<&str>,
) -> Result<(), String> {
    let vm = get_vm(ctx, task.spec.target)?;
    let stopped = vm.spec.power == PowerState::Stopped && is_down(&vm);
    let (spec, tag) = match snapshot {
        Some(id) => {
            let snapshot = get_snapshot(ctx, id)?
                .filter(|s| s.spec.vm == vm.id)
                .ok_or("snapshot was deleted")?;
            if snapshot.status.phase != SnapshotPhase::Ready {
                return Err("snapshot is not ready".into());
            }
            check_settled(&vm)?;
            (snapshot.spec.vm_spec, Some(id.to_string()))
        }
        None if !stopped => return Err("VM is running; stop it or clone from a snapshot".into()),
        None => (vm.spec.clone(), None),
    };
    // 실행 중인 VM의 디스크는 QEMU가 쥐고 있다. 스냅샷 데이터는 바뀌지 않으므로 잠금을 무시하고 읽는다.
    let force_share = tag.is_some() && !stopped;
    if ctx
        .cluster
        .get_by_name::<VmSpec>(name)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("VM {name:?} already exists"));
    }
    let disks = ready_disks(ctx, &spec)?;
    let mut targets = Vec::with_capacity(disks.len());
    for (volume, source_pool) in &disks {
        check_local(ctx, volume, source_pool)?;
        let target = match pool {
            Some(p) => pool_by_name(ctx, p)?,
            None => source_pool.clone(),
        };
        if !pool_listed_on(&target, ctx.config.node) {
            return Err(format!(
                "storage pool {:?} is not available on this node",
                target.name
            ));
        }
        // 마운트되지 않은 NFS 풀의 빈 마운트 지점에 복사본을 만들지 않는다.
        let me = ctx
            .cluster
            .get::<NodeSpec>(ctx.config.node)
            .map_err(|e| e.to_string())?
            .ok_or("this node is not registered")?;
        if !pool_usable_on(&target, &me) {
            return Err(format!(
                "storage pool {:?} is not ready on this node: {}",
                target.name,
                pool_error_on(&target, &me)
            ));
        }
        targets.push(target);
    }

    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let mut copies: Vec<(Volume, &StoragePool)> = Vec::with_capacity(disks.len());
    for ((source, source_pool), target) in disks.iter().zip(&targets) {
        let copy_name = volume_name(name, &source.name, |n| taken.contains(n));
        taken.insert(copy_name.clone());
        let format = copy_format(source_pool, target, source.spec.format);
        let mut copy: Volume = Resource::new(
            copy_name,
            VolumeSpec {
                pool: target.name.clone(),
                size_bytes: source.spec.size_bytes,
                format,
            },
        );
        copy.status.node = Some(ctx.config.node);
        // 만들기 전에 적는다. 만들기가 끝났는지 모르는 채 끊겨도 정리가 찾는다.
        made(ctx, task.id, |m| m.volumes.push(copy.id));
        ctx.cluster
            .create(&copy)
            .await
            .map_err(|e| format!("cannot create volume {:?}: {e}", copy.name))?;
        ctx.storage
            .copy_volume(
                source_pool,
                source.id,
                source.spec.format,
                tag.as_deref(),
                force_share,
                target,
                copy.id,
                format,
            )
            .await
            .map_err(|e| format!("cannot copy volume {:?}: {e}", source.name))?;
        copies.push((copy, target));
    }

    let mut new: Vm = Resource::new(name, spec);
    if let Some(dir) = &ctx.config.state_dir {
        let from = match snapshot {
            Some(id) => vars_copy(ctx, vm.id, id).expect("state_dir is set"),
            None => uefi_vars_path(dir, vm.id),
        };
        // BIOS VM이거나 한 번도 켠 적 없는 UEFI VM이면 없다.
        if tokio::fs::try_exists(&from).await.unwrap_or(false) {
            let to = uefi_vars_path(dir, new.id);
            made(ctx, task.id, |m| m.vars = Some(to.clone()));
            copy_file(&from, &to)
                .await
                .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        }
    }
    for (copy, target) in &copies {
        mark_ready(ctx, copy, target).await;
    }

    for (disk, (copy, _)) in new.spec.disks.iter_mut().zip(&copies) {
        disk.volume = copy.name.clone();
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let mut used: BTreeSet<MacAddr> = vms
        .iter()
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    for nic in &mut new.spec.nics {
        let mac = std::iter::repeat_with(MacAddr::generate)
            .find(|m| !used.contains(m))
            .expect("random MACs eventually differ");
        used.insert(mac);
        nic.mac = Some(mac);
    }
    new.spec.node = None;
    new.spec.power = PowerState::Stopped;
    new.spec.snapshot_schedule = None;
    // 이름이나 MAC이 그새 쓰였으면 저장소가 거부한다.
    if let Err(e) = ctx.cluster.create(&new).await {
        // 나중에 만들어질 수도 있으면 새 VM이 볼륨을 쓸 수 있으므로 지우지 않고 남긴다(설계 §11).
        if matches!(e, ClusterError::Timeout | ClusterError::Unreachable(_)) {
            ctx.cloned.lock().unwrap().remove(&task.id);
        }
        return Err(format!("cannot create VM {name:?}: {e}"));
    }
    ctx.cloned.lock().unwrap().remove(&task.id);
    Ok(())
}

/// 실패한 복제가 만든 볼륨(파일과 리소스)과 UEFI 변수 사본을 지운다.
pub(super) async fn discard(ctx: &Ctx, task: Uuid) {
    let Some(made) = ctx.cloned.lock().unwrap().remove(&task) else {
        return;
    };
    for id in made.volumes {
        if let Err(e) = remove_volume(ctx, id).await {
            tracing::warn!(volume = %id, error = %e, "cannot clean up volume of failed clone");
        }
    }
    if let Some(vars) = made.vars {
        let _ = tokio::fs::remove_file(&vars).await;
        // 새 VM의 상태 디렉터리. 비어 있을 때만 지워진다.
        if let Some(dir) = vars.parent() {
            let _ = tokio::fs::remove_dir(dir).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use opl_types::storage::PoolBackend;

    use super::*;

    fn pool(backend: PoolBackend) -> StoragePool {
        Resource::new(
            "p",
            StoragePoolSpec {
                backend,
                nodes: vec![],
                shared: false,
            },
        )
    }

    #[test]
    fn copies_take_the_format_the_target_pool_holds() {
        let dir = pool(PoolBackend::Dir {
            path: "/pools/local".into(),
        });
        let lvm = pool(PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "tp".into(),
        });
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);
        assert_eq!(copy_format(&dir, &dir, raw), raw);
        assert_eq!(copy_format(&dir, &dir, qcow2), qcow2);
        // LVM-thin 풀은 raw만 담는다.
        assert_eq!(copy_format(&dir, &lvm, qcow2), raw);
        assert_eq!(copy_format(&lvm, &lvm, raw), raw);
        // 파일 풀로 옮긴 LVM 볼륨은 스냅샷을 담을 수 있게 qcow2로 바꾼다.
        assert_eq!(copy_format(&lvm, &dir, raw), qcow2);
        // ZFS 풀도 블록 풀이다.
        let zfs = pool(PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        });
        assert_eq!(copy_format(&dir, &zfs, qcow2), raw);
        assert_eq!(copy_format(&zfs, &lvm, raw), raw);
        assert_eq!(copy_format(&lvm, &zfs, raw), raw);
        assert_eq!(copy_format(&zfs, &dir, raw), qcow2);
        // 스냅샷이 없는 공유 LVM 풀도 raw만 담는 블록 풀이다.
        let shared = pool(PoolBackend::LvmShared {
            vg: "san".into(),
            iscsi: None,
        });
        assert_eq!(copy_format(&dir, &shared, qcow2), raw);
        assert_eq!(copy_format(&shared, &dir, raw), qcow2);
        assert_eq!(copy_format(&shared, &lvm, raw), raw);
    }

    #[test]
    fn volume_name_joins_cuts_and_numbers() {
        let none = |_: &str| false;
        assert_eq!(volume_name("web-2", "root", none), "web-2-root");

        let taken = |n: &str| ["web-2-root", "web-2-root-2"].contains(&n);
        assert_eq!(volume_name("web-2", "root", taken), "web-2-root-3");

        // 63자로 자르고, 자른 끝의 `-`는 뗀다.
        let vm = "v".repeat(40);
        let volume = format!("{}-x", "d".repeat(21));
        let name = volume_name(&vm, &volume, none);
        assert_eq!(name, format!("{vm}-{}", "d".repeat(21)));
        assert_eq!(name.len(), 62);

        // 번호를 붙여도 63자를 넘지 않는다.
        let long = "a".repeat(63);
        let name = volume_name(&long, "root", |n| n.len() == 63 && !n.ends_with("-2"));
        assert_eq!(name, format!("{}-2", "a".repeat(61)));
    }
}
