//! 백업 Task(설계 §2.3), 백업 지우기 Task, 복원 Task(설계 §2.4). 백업은 VM의 줄에 서서 그 VM의
//! 스냅샷·복제 Task와 차례로 돌고, 지우기와 복원은 백업의 줄에 선다. API가 확인한 뒤 줄에서 기다리는
//! 사이 VM이나 풀이 바뀌었을 수 있으므로 다시 확인한다.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{BackupTarget, check_pool_root, uefi_vars_path};
use opl_types::backup::{
    BITMAP_PREFIX, Backup, BackupDisk, BackupManifest, BackupPhase, BackupSpec, BackupStatus,
    MANIFEST_FILE, MANIFEST_VERSION, MANIFEST_VERSION_INCREMENTAL, backup_dir, bitmap_name,
};
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumeSpec, pool_error_on, pool_listed_on,
    pool_usable_on,
};
use opl_types::task::Task;
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use super::clone::{copy_format, create_vm, made, volume_name};
use super::snapshot::{
    check_local, check_settled, copy_file, fetch_vars, get_vm, is_down, now_secs, ready_disks,
    remove_tags, take_tags,
};
use super::{Ctx, mark_ready};
use crate::backups::parent_in_use;

/// 백업 디렉터리 안의 UEFI 변수 파일 이름
pub(crate) const VARS_FILE: &str = "vars.fd";

pub(super) fn get_backup(ctx: &Ctx, id: Uuid) -> Result<Option<Backup>, String> {
    ctx.cluster.get::<BackupSpec>(id).map_err(|e| e.to_string())
}

/// 실행 중인 VM의 디스크에 찍는 임시 스냅샷의 태그: 백업 id의 simple 문자열. 스냅샷 태그(하이픈 있는
/// id)와 구분되고, 남으면 태그 청소가 백업 리소스를 보고 지운다.
fn temp_tag(backup: Uuid) -> String {
    backup.simple().to_string()
}

/// 이 노드에서 준비된 풀 `name`. 준비되지 않았으면(마운트되지 않은 NFS 풀의 빈 마운트 지점에 쓰지
/// 않는다) 오류다.
pub(super) fn ready_pool(ctx: &Ctx, name: &str) -> Result<StoragePool, String> {
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {name:?} not found"))?;
    if !pool_listed_on(&pool, ctx.config.node) {
        return Err(format!(
            "storage pool {name:?} is not available on this node"
        ));
    }
    let me = ctx
        .cluster
        .get::<NodeSpec>(ctx.config.node)
        .map_err(|e| e.to_string())?
        .ok_or("this node is not registered")?;
    if !pool_usable_on(&pool, &me) {
        return Err(format!(
            "storage pool {name:?} is not ready on this node: {}",
            pool_error_on(&pool, &me)
        ));
    }
    Ok(pool)
}

/// 백업 풀 `name`과 이 노드에서 그 풀의 루트. 풀이 이 노드에서 준비되지 않았으면 오류다.
pub(super) fn backup_root(ctx: &Ctx, name: &str) -> Result<(StoragePool, PathBuf), String> {
    let pool = ready_pool(ctx, name)?;
    let root = check_pool_root(&pool, &ctx.config.mount_root)
        .map_err(|e| format!("storage pool {name:?}: {e}"))?;
    Ok((pool, root))
}

/// VM의 UEFI 변수 파일(있으면)을 `to`로 복사한다. 변수 파일이 다른 노드에 있으면 받아 온다.
/// BIOS VM이거나 한 번도 켠 적 없는 UEFI VM이면 복사할 것이 없다.
async fn copy_vars(ctx: &Ctx, vm: &Vm, to: &Path) -> std::io::Result<()> {
    let Some(dir) = &ctx.config.state_dir else {
        return Ok(());
    };
    fetch_vars(ctx, vm).await;
    let from = uefi_vars_path(dir, vm.id);
    if !tokio::fs::try_exists(&from).await? {
        return Ok(());
    }
    copy_file(&from, to).await
}

/// 디스크마다 `tag`(없으면 지금) 때의 내용을 `{dir}/disk{i}.qcow2`로 내보낸다. `force_share`는 실행
/// 중인 VM이 쥔 디스크의 스냅샷을 읽을 때 쓴다(스냅샷 데이터는 바뀌지 않는다).
async fn export_disks(
    ctx: &Ctx,
    disks: &[(Volume, StoragePool)],
    tag: Option<&str>,
    force_share: bool,
    dir: &Path,
) -> Result<Vec<BackupDisk>, String> {
    let mut out = Vec::with_capacity(disks.len());
    for (i, (volume, pool)) in disks.iter().enumerate() {
        let name = format!("disk{i}");
        let file = format!("{name}.qcow2");
        let dest = dir.join(&file);
        let format = volume.spec.format;
        let source = ctx
            .storage
            .volume_source(pool, volume.id, format, tag)
            .await
            .map_err(|e| format!("cannot read volume {:?}: {e}", volume.name))?;
        let exported = ctx.storage.export_volume(&source, force_share, &dest).await;
        // 내보내기는 끝났다. 내놓지 못하면 켜진 채 남을 뿐이다(opld가 뒤에 정리한다).
        if let Err(e) = ctx
            .storage
            .release_source(pool, volume.id, format, tag)
            .await
        {
            tracing::warn!(volume = %volume.name, error = %e, "cannot release backup source");
        }
        exported.map_err(|e| format!("cannot back up volume {:?}: {e}", volume.name))?;
        let stored_bytes = tokio::fs::metadata(&dest)
            .await
            .map_err(|e| format!("cannot read size of {file}: {e}"))?
            .len();
        out.push(BackupDisk {
            name,
            file,
            virtual_bytes: source.size_bytes,
            stored_bytes,
            sha256: None,
        });
    }
    Ok(out)
}

/// 파일이나 디렉터리 `path`를 디스크에 내려 쓴다(fsync). 디렉터리는 그 안의 이름 바꾸기를 남긴다.
pub(crate) async fn sync_path(path: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

/// 매니페스트를 `{dir}/manifest.json.part`에 쓰고 디스크에 내려 쓴 뒤 이름을 바꾸고, 디렉터리도 내려
/// 쓴다. 매니페스트가 있으면 다 쓴 백업이다(디스크 파일은 `qemu-img`가 닫을 때 내려 썼다).
pub(crate) async fn write_manifest(dir: &Path, manifest: &BackupManifest) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let text = serde_json::to_vec_pretty(manifest).map_err(std::io::Error::other)?;
    let path = dir.join(MANIFEST_FILE);
    let part = dir.join(format!("{MANIFEST_FILE}.part"));
    let mut file = tokio::fs::File::create(&part).await?;
    file.write_all(&text).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&part, &path).await?;
    sync_path(dir).await
}

/// 백업 디렉터리 `dir`에 백업 `id`의 매니페스트가 있는지. 형식 버전은 보지 않는다.
async fn has_manifest(dir: &Path, id: Uuid) -> bool {
    let Ok(text) = tokio::fs::read(dir.join(MANIFEST_FILE)).await else {
        return false;
    };
    let backup = serde_json::from_slice::<serde_json::Value>(&text)
        .ok()
        .and_then(|m| m.get("backup")?.as_str()?.parse::<Uuid>().ok());
    backup == Some(id)
}

/// 부모 `parent`가 없어졌거나 `ready`가 아니거나 지우는 Task가 끝나지 않았으면 전체로 하는 이유. 부모
/// 지우기는 자식이 적히기 전에 확인을 지났으면 그대로 돈다(백업의 줄에 서므로 VM의 줄과 따로 돈다).
fn parent_going(ctx: &Ctx, parent: &Backup) -> Result<Option<String>, String> {
    use opl_types::task::{TaskOp, TaskSpec};
    let now = get_backup(ctx, parent.id)?;
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    let deleting = tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && t.spec.op == TaskOp::BackupDelete
            && t.spec.target == parent.id
    });
    let ready = now.is_some_and(|b| b.status.phase == BackupPhase::Ready);
    Ok((!ready || deleting).then(|| format!("backup {:?} is being deleted", parent.name)))
}

/// 증분 백업의 부모(설계 §3.3)와 디스크마다 지금의 가상 크기
struct Parent {
    backup: Backup,
    sizes: Vec<u64>,
}

/// 백업 방법
enum Plan {
    /// 전체 백업. 증분을 요청했으면 전체로 한 이유
    Full(Option<String>),
    Incremental(Box<Parent>),
}

/// 증분 백업의 부모를 고른다(설계 §3.3). 증분으로 할 수 없으면 `Ok(Err(전체로 하는 이유))`다. 비트맵이나
/// 볼륨 크기를 읽지 못하면 오류다.
async fn choose_parent(
    ctx: &Ctx,
    backup: &Backup,
    vm: &Vm,
    disks: &[(Volume, StoragePool)],
) -> Result<Result<Parent, String>, String> {
    if vm.status.phase != VmPhase::Running {
        return Ok(Err("VM is stopped".into()));
    }
    let pool = &backup.spec.pool;
    let (_, backups) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    let earlier: Vec<&Backup> = backups
        .iter()
        .filter(|b| {
            b.id != backup.id
                && b.spec.vm == vm.id
                && b.spec.pool == *pool
                && b.status.phase == BackupPhase::Ready
        })
        .collect();
    if earlier.is_empty() {
        return Ok(Err(format!(
            "no earlier backup of this VM in pool {pool:?}"
        )));
    }
    // 디스크마다 `opl-` 비트맵이 정확히 하나이고 모두 같은 이름이어야 한다.
    let found = ctx
        .hv
        .backup_bitmaps(vm.id)
        .await
        .map_err(|e| format!("cannot read dirty bitmaps: {e}"))?;
    let mut name: Option<&str> = None;
    let mut same = found.len() == disks.len() && !found.is_empty();
    for disk in &found {
        let ours: Vec<_> = disk
            .bitmaps
            .iter()
            .filter(|b| b.name.starts_with(BITMAP_PREFIX))
            .collect();
        if ours.is_empty() {
            return Ok(Err(format!("no dirty bitmap on {}", disk.node)));
        }
        // 기록하지 않는 비트맵도 바뀐 블록을 모두 담지 않으므로 믿지 않는다.
        if ours.iter().any(|b| b.inconsistent || !b.recording) {
            return Ok(Err(format!(
                "dirty bitmap on {} is inconsistent",
                disk.node
            )));
        }
        match ours[..] {
            [one] if *name.get_or_insert(&one.name) == one.name => {}
            _ => same = false,
        }
    }
    let parent = name
        .filter(|_| same)
        .and_then(|name| earlier.into_iter().find(|b| bitmap_name(b.id) == name))
        .cloned();
    let Some(parent) = parent else {
        return Ok(Err(format!(
            "dirty bitmaps do not match a ready backup in pool {pool:?}"
        )));
    };
    // 공유가 아닌 풀의 파일은 쓴 노드에만 있다. VM이 다른 노드로 옮겨졌으면 비트맵은 맞아도 체인을
    // 이 노드에서 복원할 수 없다.
    if parent.status.node.is_some_and(|n| n != ctx.config.node) {
        return Ok(Err(format!(
            "backup {:?} is stored on another node",
            parent.name
        )));
    }
    let (_, root) = backup_root(ctx, pool)?;
    if !has_manifest(&backup_dir(&root, parent.id), parent.id).await {
        return Ok(Err(format!(
            "files of backup {:?} are missing on this node",
            parent.name
        )));
    }
    // 디스크가 부모와 같아야 한다: 수, 순서마다 같은 볼륨, 같은 가상 크기.
    let mut sizes = Vec::with_capacity(disks.len());
    for (volume, pool) in disks {
        let info = ctx
            .storage
            .volume_info(pool, volume.id, volume.spec.format)
            .await
            .map_err(|e| format!("cannot read size of volume {:?}: {e}", volume.name))?;
        sizes.push(info.virtual_bytes);
    }
    let volumes =
        |spec: &VmSpec| -> Vec<String> { spec.disks.iter().map(|d| d.volume.clone()).collect() };
    let parent_sizes: Vec<u64> = parent
        .status
        .disks
        .iter()
        .map(|d| d.virtual_bytes)
        .collect();
    if volumes(&parent.spec.vm_spec) != volumes(&vm.spec) || parent_sizes != sizes {
        return Ok(Err(format!("disks changed since backup {:?}", parent.name)));
    }
    // 체인의 증분이 한도(백업 작업의 `full_every`)만큼 쌓였으면 새 체인을 시작한다.
    if let Some(max) = backup.spec.max_chain {
        let n = increments(&parent, &backups);
        if n >= max {
            return Ok(Err(format!("chain already has {n} incremental backup(s)")));
        }
    }
    Ok(Ok(Parent {
        backup: parent,
        sizes,
    }))
}

/// `backup`에서 부모를 거슬러 전체 백업까지 센 증분 백업 수(`backup`이 증분이면 그것도 센다). 없어진
/// 부모에서 멈춘다.
fn increments(backup: &Backup, backups: &[Backup]) -> u32 {
    let mut n = 0;
    let mut seen = HashSet::new();
    let mut parent = backup.status.parent;
    while let Some(id) = parent
        && seen.insert(id)
    {
        n += 1;
        parent = backups
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| b.status.parent);
    }
    n
}

/// 실패한 전체 백업이 실행 중인 VM에 만든 비트맵 `new`를 지운다. 앞 백업의 비트맵(모든 디스크에 같은
/// 하나)은 남겨 다음 증분이 그대로 쓴다. 그런 것이 없으면 `opl-` 비트맵을 모두 지운다.
async fn drop_new_bitmap(ctx: &Ctx, vm: Uuid, new: &str) {
    let disks = match ctx.hv.backup_bitmaps(vm).await {
        Ok(disks) => disks,
        Err(e) => {
            tracing::warn!(vm = %vm, error = %e, "cannot read dirty bitmaps of failed backup");
            return;
        }
    };
    let mut others = disks.iter().map(|d| {
        let mut names = d
            .bitmaps
            .iter()
            .filter(|b| b.name.starts_with(BITMAP_PREFIX) && b.name != new)
            .map(|b| b.name.as_str());
        match (names.next(), names.next()) {
            (Some(name), None) => Some(name),
            _ => None,
        }
    });
    let first = others.next().flatten();
    let keep = first.filter(|_| others.all(|o| o == first));
    if let Err(e) = ctx.hv.remove_bitmaps(vm, keep).await {
        tracing::warn!(vm = %vm, error = %e, "cannot remove dirty bitmap of failed backup");
    }
}

/// 전체 백업: 디스크마다 지금(실행 중이면 임시 스냅샷) 내용을 `{dir}/disk{i}.qcow2`로 내보내고 UEFI
/// 변수를 `vars`로 복사한다. `bitmap`을 주면 실행 중일 때 임시 스냅샷과 같은 시점에 그 비트맵을 만들고,
/// 꺼진 VM은 내보낸 뒤 qcow2 디스크에 비트맵을 둔다(설계 §3.1). 담은 디스크와 그 시각을 돌려준다.
async fn full_disks(
    ctx: &Ctx,
    vm: &Vm,
    disks: &[(Volume, StoragePool)],
    id: Uuid,
    bitmap: Option<&str>,
    dir: &Path,
    vars: &Path,
) -> Result<(Vec<BackupDisk>, u64), String> {
    use super::snapshot::resume_vm;
    let running = vm.status.phase == VmPhase::Running;
    let tag = temp_tag(id);
    // 실행 중이면 디스크를 임시 스냅샷으로 얼리고 그때의 UEFI 변수를 복사한다.
    let tagged = running && !disks.is_empty();
    let resumed = if tagged {
        take_tags(ctx, vm, disks, id, &tag, false, bitmap, || {
            copy_vars(ctx, vm, vars)
        })
        .await?
    } else {
        copy_vars(ctx, vm, vars)
            .await
            .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        Ok(())
    };
    // 블록 스냅샷을 위해 멈춘 VM을 이어서 실행하지 못했으면 한 번 더 해 본다. 그래도 멈춰 있으면 디스크를
    // 읽지 않고 실패한다(임시 스냅샷은 아래에서 지운다). 오래 걸리는 내보내기 동안 VM을 멈춰 두지 않는다.
    let resumed = match resumed {
        Err(e) => {
            tracing::warn!(vm = %vm.id, error = %e, "cannot resume VM after the temporary snapshot; retrying");
            resume_vm(&*ctx.hv, vm.id).await.map_err(|r| {
                format!(
                    "cannot resume the VM after its temporary snapshot: {e}; retrying failed: {r}"
                )
            })
        }
        Ok(()) => Ok(()),
    };
    let created_at = now_secs();
    let exported = match &resumed {
        Ok(()) => export_disks(ctx, disks, tagged.then_some(&*tag), tagged, dir).await,
        Err(e) => Err(e.clone()),
    };
    // 임시 스냅샷은 결과와 상관없이 지운다. 남으면 공간만 쓴다(블록 디스크는 태그 청소가 지운다).
    if tagged {
        let running = !matches!(ctx.hv.inspect(vm.id).await, Ok(None));
        if let Err(e) = remove_tags(ctx, vm.id, running, &vm.spec, &tag).await {
            tracing::warn!(backup = %id, error = %e, "cannot remove temporary snapshot of backup");
        }
    }
    let exported = exported?;
    // 꺼진 VM은 잠금을 쥐고 있어 내보낸 내용이 지금 디스크와 같다. 비트맵을 두지 못하면 다음 증분이
    // 전체가 될 뿐이다.
    if let Some(bitmap) = bitmap.filter(|_| !running) {
        for (volume, pool) in disks {
            if let Err(e) = ctx
                .storage
                .set_backup_bitmap(pool, volume.id, volume.spec.format, bitmap)
                .await
            {
                tracing::warn!(backup = %id, volume = %volume.name, error = %e, "cannot set dirty bitmap of backup");
            }
        }
    }
    Ok((exported, created_at))
}

/// 증분 백업(설계 §3.5): 디스크마다 backing 없는 빈 qcow2를 만들고 하이퍼바이저가 부모 비트맵 뒤에
/// 바뀐 블록만 쓰게 한 뒤 `{dir}/disk{i}.qcow2`로 이름을 바꾼다. 쓰기 시작하는 시점에 새 비트맵
/// `bitmap`이 생기고, 성공하면 부모 비트맵은 지워진다(실패하면 새 비트맵이 지워진다). 담은 디스크와
/// 그 시각을 돌려준다.
async fn incremental_disks(
    ctx: &Ctx,
    vm: &Vm,
    parent: &Parent,
    bitmap: &str,
    dir: &Path,
    vars: &Path,
) -> Result<(Vec<BackupDisk>, u64), String> {
    copy_vars(ctx, vm, vars)
        .await
        .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
    let mut targets = Vec::with_capacity(parent.sizes.len());
    for (i, size) in parent.sizes.iter().enumerate() {
        let path = dir.join(format!("disk{i}.qcow2.part"));
        ctx.storage
            .create_backup_target(&path, *size)
            .await
            .map_err(|e| format!("cannot create backup file for disk{i}: {e}"))?;
        targets.push(BackupTarget {
            node: format!("disk{i}"),
            path,
            size: *size,
        });
    }
    let created_at = now_secs();
    ctx.hv
        .incremental_backup(vm.id, &bitmap_name(parent.backup.id), bitmap, &targets)
        .await
        .map_err(|e| format!("incremental backup failed: {e}"))?;
    let mut out = Vec::with_capacity(targets.len());
    for target in targets {
        let name = target.node;
        let file = format!("{name}.qcow2");
        let dest = dir.join(&file);
        tokio::fs::rename(&target.path, &dest)
            .await
            .map_err(|e| format!("cannot write {file}: {e}"))?;
        // QEMU는 대상 노드를 닫을 때 플러시했지만 매니페스트처럼 이름 바꾼 파일을 내려 쓴다.
        sync_path(&dest)
            .await
            .map_err(|e| format!("cannot write {file}: {e}"))?;
        let stored_bytes = tokio::fs::metadata(&dest)
            .await
            .map_err(|e| format!("cannot read size of {file}: {e}"))?
            .len();
        out.push(BackupDisk {
            name,
            file,
            virtual_bytes: target.size,
            stored_bytes,
            sha256: None,
        });
    }
    sync_path(dir)
        .await
        .map_err(|e| format!("cannot write backup directory: {e}"))?;
    Ok((out, created_at))
}

/// `pending` 백업 `id`를 채운다(설계 §2.3): 백업 풀의 `{루트}/backups/{id}/`에 디스크마다 압축 qcow2,
/// UEFI 변수, 매니페스트를 쓰고 `ready`로 바꾼다. 실행 중인 VM은 디스크에 메모리 없는 임시 스냅샷을
/// 찍어 그것을 읽고 끝나면 지운다(스냅샷 리소스는 만들지 않는다). 꺼진 VM은 지금 디스크를 읽는다.
/// `incremental`을 요청했고 부모를 고를 수 있으면 바뀐 블록만 담는다(증분 백업 설계 §3.3·§3.5). 성공하면
/// 디스크의 비트맵은 이 백업의 것(`opl-{id}`) 하나만 남는다(§3.1).
/// 실패하면 실행기의 정리([`discard`])가 디렉터리를 지우고 `failed`로 남긴다.
pub(super) async fn backup(ctx: &Ctx, task: &Task, id: Uuid) -> Result<(), String> {
    let backup = get_backup(ctx, id)?.ok_or("backup was deleted")?;
    if backup.status.phase != BackupPhase::Pending {
        return Err("backup is not pending".into());
    }
    let vm = get_vm(ctx, task.spec.target)?;
    check_settled(&vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let stopped = vm.spec.power == PowerState::Stopped && is_down(&vm);
    if !running && !stopped {
        return Err("VM is being started; try again when it has settled".into());
    }
    let (pool, root) = backup_root(ctx, &backup.spec.pool)?;
    if !pool.spec.backups {
        return Err(format!(
            "storage pool {:?} does not store backups",
            pool.name
        ));
    }
    let disks = ready_disks(ctx, &vm.spec)?;
    for (volume, pool) in &disks {
        check_local(ctx, volume, pool)?;
    }
    // 공유가 아닌 풀의 파일은 이 노드에만 생긴다. 쓰기 전에 적어 두어 실패한 백업도 이 노드에서 지운다.
    let node = (!opl_types::storage::is_shared(&pool)).then_some(ctx.config.node);
    if running
        && let Some((volume, _)) = disks
            .iter()
            .find(|(v, p)| !ctx.storage.supports_snapshots(p, v.spec.format))
    {
        return Err(format!(
            "volume {:?} ({:?}) cannot be snapshotted; stop the VM to back it up",
            volume.name, volume.spec.format
        ));
    }

    // 담는 VM 스펙은 실행하는 지금의 것이다(API가 확인한 뒤 VM 스펙이 바뀌었을 수 있다).
    if backup.spec.vm_spec != vm.spec || backup.spec.vm_name != vm.name {
        let spec = BackupSpec {
            vm_name: vm.name.clone(),
            vm_spec: vm.spec.clone(),
            ..backup.spec.clone()
        };
        ctx.cluster
            .update_spec(id, Some(backup.revision), backup.labels.clone(), &spec)
            .await
            .map_err(|e| e.to_string())?;
    }

    // 비트맵은 클러스터의 모든 노드가 증분 백업을 알 때만 만든다. 옛 노드는 스냅샷을 되돌리기 전에
    // 비트맵을 지우지 못해 되돌리기가 깨진다.
    let bitmaps = crate::cluster::missing_feature(&ctx.cluster, "incremental-backup")
        .map_err(|e| e.to_string())?
        .is_empty();
    let plan = if !bitmaps {
        Plan::Full(
            backup
                .spec
                .incremental
                .then(|| "some nodes do not support incremental backups yet".to_string()),
        )
    } else if backup.spec.incremental {
        match choose_parent(ctx, &backup, &vm, &disks).await? {
            Ok(parent) => Plan::Incremental(Box::new(parent)),
            Err(reason) => Plan::Full(Some(reason)),
        }
    } else {
        Plan::Full(None)
    };
    let (mut parent, mut full_reason) = match &plan {
        Plan::Incremental(p) => (Some(p.backup.id), None),
        Plan::Full(reason) => (None, reason.clone()),
    };
    // 부모는 만드는 동안에도 보인다(부모 지우기를 막는다).
    let record = async |parent: Option<Uuid>, full_reason: Option<String>| {
        let status = BackupStatus {
            node,
            parent,
            full_reason,
            ..backup.status.clone()
        };
        ctx.cluster
            .update_status::<BackupSpec>(id, &status)
            .await
            .map_err(|e| e.to_string())
    };
    if node.is_some() || parent.is_some() || full_reason.is_some() {
        record(parent, full_reason.clone()).await?;
    }
    // 부모를 적기 전에 시작한 부모 지우기는 막지 못했다. 그런 부모 위에는 쌓지 않는다.
    let going = match &plan {
        Plan::Incremental(p) => parent_going(ctx, &p.backup)?,
        Plan::Full(_) => None,
    };
    let plan = match going {
        Some(reason) => {
            record(None, Some(reason.clone())).await?;
            (parent, full_reason) = (None, Some(reason.clone()));
            Plan::Full(Some(reason))
        }
        None => plan,
    };
    let bitmap = bitmap_name(id);
    // 실행 중인 VM의 전체 백업은 임시 스냅샷과 함께 새 비트맵을 만든다. 끝나면 다른 비트맵을 지우고,
    // 실패하면 새 비트맵만 지운다.
    let live_bitmap = bitmaps && running && !disks.is_empty() && matches!(plan, Plan::Full(_));
    let made = async {
        let dir = backup_dir(&root, id);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("cannot create backup directory: {e}"))?;
        // 새 디렉터리 이름이 남게 `backups`와(처음 만들었을 수 있으니) 풀 루트도 내려 쓴다.
        for path in [root.join("backups"), root.clone()] {
            sync_path(&path)
                .await
                .map_err(|e| format!("cannot create backup directory: {e}"))?;
        }
        let vars = dir.join(VARS_FILE);
        let (disks, created_at) = match &plan {
            Plan::Incremental(p) => incremental_disks(ctx, &vm, p, &bitmap, &dir, &vars).await?,
            Plan::Full(_) => {
                let bitmap = bitmaps.then_some(bitmap.as_str());
                full_disks(ctx, &vm, &disks, id, bitmap, &dir, &vars).await?
            }
        };
        let uefi_vars = tokio::fs::try_exists(&vars).await.unwrap_or(false);
        if uefi_vars {
            sync_path(&vars)
                .await
                .map_err(|e| format!("cannot write UEFI variables: {e}"))?;
        }
        let manifest = BackupManifest {
            // 전체 백업은 옛 노드도 읽게 버전 1 그대로다.
            version: if parent.is_some() {
                MANIFEST_VERSION_INCREMENTAL
            } else {
                MANIFEST_VERSION
            },
            backup: id,
            parent,
            vm: vm.id,
            vm_name: vm.name.clone(),
            vm_spec: vm.spec.clone(),
            disks: disks.clone(),
            uefi_vars,
            created_at,
        };
        write_manifest(&dir, &manifest)
            .await
            .map_err(|e| format!("cannot write manifest: {e}"))?;
        // 그새 부모가 지워졌으면 이 증분은 복원할 수 없다. 새 비트맵도 지워 다음을 전체로 한다.
        if let Plan::Incremental(p) = &plan
            && get_backup(ctx, p.backup.id)?.is_none()
        {
            if let Err(e) = ctx.hv.remove_bitmaps(vm.id, None).await {
                tracing::warn!(backup = %id, error = %e, "cannot remove dirty bitmap of failed backup");
            }
            return Err(format!(
                "parent backup {:?} was deleted during the backup",
                p.backup.name
            ));
        }

        let status = BackupStatus {
            phase: BackupPhase::Ready,
            created_at: Some(created_at),
            disks,
            uefi_vars,
            error: None,
            node,
            replicas: Vec::new(),
            received_at: None,
            parent,
            full_reason,
        };
        // 다 만든 백업이다. 실패로 끝내면 정리하면서 지우게 되므로 기록될 때까지 다시 시도한다.
        loop {
            match ctx.cluster.update_status::<BackupSpec>(id, &status).await {
                Ok(_) => break,
                Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
                Err(e) => {
                    tracing::warn!(backup = %id, error = %e, "cannot mark backup ready; will retry");
                    tokio::time::sleep(ctx.config.poll_interval).await;
                }
            }
        }
        Ok(())
    }
    .await;
    if live_bitmap && matches!(ctx.hv.inspect(vm.id).await, Ok(Some(_))) {
        match &made {
            Ok(()) => {
                // 남은 앞 비트맵은 다음 증분을 막을 뿐이다(그 백업은 전체로 하고 다시 지운다).
                if let Err(e) = ctx.hv.remove_bitmaps(vm.id, Some(&bitmap)).await {
                    tracing::warn!(backup = %id, error = %e, "cannot remove older dirty bitmaps");
                }
            }
            Err(_) => drop_new_bitmap(ctx, vm.id, &bitmap).await,
        }
    }
    made
}

/// 백업 디렉터리를 지운다. 없으면 이미 지운 것이다.
async fn remove_dir(dir: &Path) -> Result<(), String> {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove backup directory: {e}")),
    }
}

/// 실패한 백업 Task의 흔적을 지운다: 아직 `pending`인 백업이면 디렉터리를 지우고 `failed`와 `error`를
/// 남긴다(리소스는 사용자가 보고 지운다). VM이 실행 중이면 남았을 수 있는 증분 백업 작업을 치우고,
/// `unfinished`면(opld가 재시작해 중단) 디스크에 남았을 수 있는 임시 스냅샷도 지운다(블록 디스크의
/// 것은 태그 청소도 지운다).
pub(super) async fn discard(ctx: &Ctx, task: &Task, id: Uuid, error: &str, unfinished: bool) {
    let backup = match get_backup(ctx, id) {
        Ok(Some(b)) if b.status.phase == BackupPhase::Pending => b,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(backup = %id, error = %e, "cannot read backup of failed task");
            return;
        }
    };
    if let Ok(Some(vm)) = ctx.cluster.get::<VmSpec>(task.spec.target)
        && matches!(ctx.hv.inspect(vm.id).await, Ok(Some(_)))
    {
        // 증분 백업 작업이 남았으면(opld가 기다리다 죽었다) 다음 스냅샷이 실패한다.
        if let Err(e) = ctx.hv.clear_backup_leftovers(vm.id).await {
            tracing::warn!(backup = %id, error = %e, "cannot clear backup jobs of failed backup");
        }
        if unfinished && let Err(e) = remove_tags(ctx, vm.id, true, &vm.spec, &temp_tag(id)).await {
            tracing::warn!(backup = %id, error = %e, "cannot remove temporary snapshot of backup");
        }
    }
    match backup_root(ctx, &backup.spec.pool) {
        Ok((_, root)) => {
            if let Err(e) = remove_dir(&backup_dir(&root, id)).await {
                tracing::warn!(backup = %id, error = %e, "cannot clean up failed backup");
            }
        }
        Err(e) => tracing::warn!(backup = %id, error = %e, "cannot clean up failed backup"),
    }
    let status = BackupStatus {
        phase: BackupPhase::Failed,
        error: Some(error.to_string()),
        ..backup.status
    };
    loop {
        match ctx.cluster.update_status::<BackupSpec>(id, &status).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot mark backup failed; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 백업 파일(디렉터리)과 리소스를 지운다. 디렉터리가 이미 없어도 성공이므로 중간에 멈춰도 다시 하면 된다.
/// 다만 공유가 아닌 풀에서 다른 노드가 쓴 백업(`status.node`)은 여기에 없는 것이 당연하므로 실패다.
/// 풀이 없어졌으면 파일도 함께 없어진 것으로 보고 리소스만 지운다. 이 백업을 되살리는 Task가 끝나지
/// 않았으면(다른 노드에서 돌 수 있다) 실패다. 증분 백업(`failed`가 아닌 것)의 부모여도 실패다(자식부터
/// 지운다). 파일을 지운 뒤 리소스보다 먼저 원격 사본을 지우고(엔터프라이즈 설계 §6.2), 지우지 못한
/// 사본이 있어도 원본은 지우고 그 경고를 돌려준다.
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<Option<String>, String> {
    use opl_types::task::{TaskOp, TaskSpec};
    let Some(backup) = get_backup(ctx, id)? else {
        return Ok(None);
    };
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    if tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::BackupRestore { backup, .. } if backup == id)
    }) {
        return Err("backup is being restored".into());
    }
    let (_, backups) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    if let Some(e) = parent_in_use(&backups, &backup) {
        return Err(e);
    }
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)
        .map_err(|e| e.to_string())?;
    let dir = if pool.is_none() {
        tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, "storage pool of backup is gone; deleting only the backup");
        None
    } else {
        let (pool, root) = backup_root(ctx, &backup.spec.pool)?;
        let dir = backup_dir(&root, id);
        let elsewhere = (!opl_types::storage::is_shared(&pool))
            .then_some(backup.status.node)
            .flatten()
            .filter(|n| *n != ctx.config.node);
        if let Some(node) = elsewhere
            && !tokio::fs::try_exists(&dir).await.unwrap_or(true)
        {
            return Err(format!("backup files are on node {node}, not on this node"));
        }
        Some(dir)
    };
    // 원본 파일을 지우지 못하면 원본이 남으므로 원격 사본도 건드리지 않는다.
    if let Some(dir) = dir {
        remove_dir(&dir).await?;
    }
    let left = super::replicate::delete_copies(ctx, &backup).await;
    match ctx.cluster.delete::<BackupSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
        Err(e) => return Err(e.to_string()),
    }
    Ok((!left.is_empty()).then(|| {
        format!(
            "backup deleted, but remote copies may be left: {}",
            left.join("; ")
        )
    }))
}

/// 백업 디렉터리의 매니페스트를 읽는다. 형식 버전이 1(전체)이나 2(증분)가 아니면 나머지를 읽지 않고
/// 오류다.
pub(super) async fn read_manifest(dir: &Path) -> Result<BackupManifest, String> {
    let text = tokio::fs::read(dir.join(MANIFEST_FILE))
        .await
        .map_err(|e| format!("cannot read backup manifest: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&text).map_err(|e| format!("invalid backup manifest: {e}"))?;
    let version = value.get("version").and_then(|v| v.as_u64());
    if ![MANIFEST_VERSION, MANIFEST_VERSION_INCREMENTAL]
        .iter()
        .any(|v| version == Some(u64::from(*v)))
    {
        let version = version.map_or("unknown".into(), |v| v.to_string());
        return Err(format!(
            "backup manifest version {version} is not supported (this node reads versions {MANIFEST_VERSION} and {MANIFEST_VERSION_INCREMENTAL})"
        ));
    }
    serde_json::from_value(value).map_err(|e| format!("invalid backup manifest: {e}"))
}

/// 증분 백업 `backup`(매니페스트 `manifest`)의 조상을 부모부터 전체 백업까지 읽는다: 백업 풀 루트
/// `root` 안의 디렉터리와 매니페스트. 조상은 모두 같은 풀에 `ready`로 있고 이 노드에서 읽을 수 있으며,
/// 매니페스트가 그 기록과 맞고, 디스크 수와 가상 크기가 `backup`과 같아야 한다. 아니면 "backup chain is
/// broken: …"이다. 전체 백업이면 비어 있다.
async fn read_chain(
    ctx: &Ctx,
    backup: &Backup,
    root: &Path,
    manifest: &BackupManifest,
) -> Result<Vec<(PathBuf, BackupManifest)>, String> {
    let broken = |e: String| format!("backup chain is broken: {e}");
    let sizes =
        |m: &BackupManifest| -> Vec<u64> { m.disks.iter().map(|d| d.virtual_bytes).collect() };
    let mut out = Vec::new();
    let mut seen = HashSet::from([backup.id]);
    let mut child = backup.name.clone();
    let mut parent = backup.status.parent;
    while let Some(id) = parent {
        if !seen.insert(id) {
            return Err(broken(format!("backup {child:?} is its own ancestor")));
        }
        let b = get_backup(ctx, id)?
            .ok_or_else(|| broken(format!("the parent of backup {child:?} was deleted")))?;
        if b.spec.pool != backup.spec.pool {
            return Err(broken(format!(
                "backup {:?} is in another storage pool",
                b.name
            )));
        }
        if b.status.phase != BackupPhase::Ready {
            return Err(broken(format!("backup {:?} is not ready", b.name)));
        }
        if let Some(node) = b.status.node.filter(|n| *n != ctx.config.node) {
            return Err(broken(format!(
                "backup {:?} is stored on node {node}",
                b.name
            )));
        }
        let dir = backup_dir(root, id);
        let m = read_manifest(&dir)
            .await
            .map_err(|e| broken(format!("backup {:?}: {e}", b.name)))?;
        if m.backup != id || m.parent != b.status.parent {
            return Err(broken(format!(
                "the manifest of backup {:?} does not match its record",
                b.name
            )));
        }
        if sizes(&m) != sizes(manifest) {
            return Err(broken(format!(
                "the disks of backup {:?} do not match backup {:?}",
                b.name, backup.name
            )));
        }
        parent = b.status.parent;
        child = b.name;
        out.push((dir, m));
    }
    Ok(out)
}

/// 백업 디렉터리 `dir` 안의 일반 파일 `file`의 경로. 심볼릭 링크(디렉터리 밖을 가리킬 수 있다)나 다른
/// 종류의 파일이면 오류다.
async fn regular_file(dir: &Path, file: &str) -> Result<PathBuf, String> {
    let path = dir.join(file);
    match tokio::fs::symlink_metadata(&path).await {
        Ok(m) if m.file_type().is_file() => Ok(path),
        Ok(_) => Err(format!("{file} is not a regular file")),
        Err(e) => Err(format!("cannot read {file}: {e}")),
    }
}

/// `ready` 백업 `id`를 새 VM `name`(id는 Task의 대상)으로 되살린다(설계 §2.4): 매니페스트의 디스크마다
/// 풀 `pool`에 새 볼륨 `{name}-disk{i}`를 만들어 백업 파일을 쓰고(블록 풀이면 raw, 아니면 qcow2),
/// UEFI 변수를 복사한 뒤 백업 리소스에 적힌 VM 스펙으로 꺼진 VM을 만든다(새 MAC, 노드·예약·HA 없음).
/// 매니페스트는 그 기록과 같아야 한다. 공유가 아닌 풀의 백업은 파일을 쓴 노드에서만 되살린다.
/// 증분 백업은 부모를 거슬러 전체 백업까지의 체인([`read_chain`])을 디스크마다 한 이미지로 연다(증분
/// 백업 설계 §3.6). 실패하면 실행기의 정리(복제와 같은 [`super::clone::discard`])가 만든 볼륨과 변수
/// 사본을 지운다.
pub(super) async fn restore(
    ctx: &Ctx,
    task: &Task,
    id: Uuid,
    name: &str,
    pool: &str,
) -> Result<(), String> {
    let backup = get_backup(ctx, id)?.ok_or("backup was deleted")?;
    if backup.status.phase != BackupPhase::Ready {
        return Err("backup is not ready".into());
    }
    if ctx
        .cluster
        .get_by_name::<VmSpec>(name)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("VM {name:?} already exists"));
    }
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
        return Err(format!("backup is stored on node {node}"));
    }
    let (source, root) = backup_root(ctx, &backup.spec.pool)?;
    let target = ready_pool(ctx, pool)?;
    let dir = backup_dir(&root, id);
    let manifest = read_manifest(&dir).await?;
    let spec = &backup.spec.vm_spec;
    if manifest.backup != id
        || manifest.parent != backup.status.parent
        || manifest.vm_spec != *spec
        || manifest.disks.len() != spec.disks.len()
    {
        return Err("backup manifest does not match the backup record".into());
    }
    // 증분 백업의 조상 디렉터리(부모부터)
    let ancestors = read_chain(ctx, &backup, &root, &manifest).await?;

    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let format = copy_format(&source, &target, ImageFormat::Qcow2);
    let mut copies: Vec<Volume> = Vec::with_capacity(manifest.disks.len());
    for (i, disk) in manifest.disks.iter().enumerate() {
        let failed =
            |e: &dyn std::fmt::Display| format!("cannot restore disk {:?}: {e}", disk.name);
        // 체인의 파일(자식부터). 전체 백업이면 하나다.
        let mut chain = Vec::with_capacity(ancestors.len() + 1);
        for (dir, m) in
            std::iter::once((&dir, &manifest)).chain(ancestors.iter().map(|(d, m)| (d, m)))
        {
            let file = m.disks[i].file.as_str();
            // 파일은 백업 디렉터리 바로 안에 있어야 한다. 다른 파일을 참조하는 이미지도 받지 않는다.
            if Path::new(file).file_name() != Some(OsStr::new(file)) {
                return Err(failed(&format!("invalid file name {file:?}")));
            }
            let file = regular_file(dir, file).await.map_err(|e| failed(&e))?;
            ctx.storage
                .check_import_source(&file, SourceFormat::Qcow2)
                .await
                .map_err(|e| failed(&e))?;
            chain.push(file);
        }
        let copy_name = volume_name(name, &disk.name, |n| taken.contains(n));
        taken.insert(copy_name.clone());
        let mut copy: Volume = Resource::new(
            copy_name,
            VolumeSpec {
                pool: target.name.clone(),
                size_bytes: disk.virtual_bytes,
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
        let imported = match &chain[..] {
            [file] => {
                ctx.storage
                    .import_volume(&target, copy.id, file, SourceFormat::Qcow2, format)
                    .await
            }
            chain => {
                ctx.storage
                    .import_chain(&target, copy.id, chain, format)
                    .await
            }
        };
        imported.map_err(|e| failed(&e))?;
        copies.push(copy);
    }

    let mut new: Vm = Resource::new(name, backup.spec.vm_spec.clone());
    new.id = task.spec.target;
    if manifest.uefi_vars
        && let Some(state) = &ctx.config.state_dir
    {
        let vars = regular_file(&dir, VARS_FILE).await?;
        let to = uefi_vars_path(state, new.id);
        made(ctx, task.id, |m| m.vars = Some(to.clone()));
        copy_file(&vars, &to)
            .await
            .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        // 새 VM의 변수는 이 노드에 있다. 다른 노드에서 켜면 여기서 받아 간다.
        new.status.firmware_node = Some(ctx.config.node);
    }
    for copy in &copies {
        mark_ready(ctx, copy, &target).await;
    }
    for (disk, copy) in new.spec.disks.iter_mut().zip(&copies) {
        disk.volume = copy.name.clone();
    }
    new.spec.ha = None;
    // 되살린 VM은 MAC을 새로 받는다.
    for nic in &mut new.spec.nics {
        nic.mac = None;
    }
    new.spec
        .validate()
        .map_err(|e| format!("invalid VM spec in backup: {e}"))?;
    create_vm(ctx, task, new).await
}
