//! 백업 Task(설계 §2.3)와 백업 지우기 Task. 백업은 VM의 줄에 서서 그 VM의 스냅샷·복제 Task와 차례로
//! 돌고, 지우기는 백업의 줄에 선다. API가 확인한 뒤 줄에서 기다리는 사이 VM이나 풀이 바뀌었을 수
//! 있으므로 다시 확인한다.

use std::path::{Path, PathBuf};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{check_pool_root, uefi_vars_path};
use opl_types::backup::{
    Backup, BackupDisk, BackupManifest, BackupPhase, BackupSpec, BackupStatus, MANIFEST_FILE,
    MANIFEST_VERSION, backup_dir,
};
use opl_types::node::NodeSpec;
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, pool_error_on, pool_listed_on, pool_usable_on,
};
use opl_types::task::Task;
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use super::Ctx;
use super::snapshot::{
    check_local, check_settled, copy_file, fetch_vars, get_vm, is_down, now_secs, ready_disks,
    remove_tags, take_tags,
};

/// 백업 디렉터리 안의 UEFI 변수 파일 이름
pub(super) const VARS_FILE: &str = "vars.fd";

fn get_backup(ctx: &Ctx, id: Uuid) -> Result<Option<Backup>, String> {
    ctx.cluster.get::<BackupSpec>(id).map_err(|e| e.to_string())
}

/// 실행 중인 VM의 디스크에 찍는 임시 스냅샷의 태그: 백업 id의 simple 문자열. 스냅샷 태그(하이픈 있는
/// id)와 구분되고, 남으면 태그 청소가 백업 리소스를 보고 지운다.
fn temp_tag(backup: Uuid) -> String {
    backup.simple().to_string()
}

/// 백업 풀 `name`과 이 노드에서 그 풀의 루트. 풀이 이 노드에서 준비되지 않았으면(마운트되지 않은 NFS
/// 풀의 빈 마운트 지점에 쓰지 않는다) 오류다.
fn backup_root(ctx: &Ctx, name: &str) -> Result<(StoragePool, PathBuf), String> {
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
        });
    }
    Ok(out)
}

/// 파일이나 디렉터리 `path`를 디스크에 내려 쓴다(fsync). 디렉터리는 그 안의 이름 바꾸기를 남긴다.
async fn sync_path(path: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

/// 매니페스트를 `{dir}/manifest.json.part`에 쓰고 디스크에 내려 쓴 뒤 이름을 바꾸고, 디렉터리도 내려
/// 쓴다. 매니페스트가 있으면 다 쓴 백업이다(디스크 파일은 `qemu-img`가 닫을 때 내려 썼다).
async fn write_manifest(dir: &Path, manifest: &BackupManifest) -> std::io::Result<()> {
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

/// `pending` 백업 `id`를 채운다(설계 §2.3): 백업 풀의 `{루트}/backups/{id}/`에 디스크마다 압축 qcow2,
/// UEFI 변수, 매니페스트를 쓰고 `ready`로 바꾼다. 실행 중인 VM은 디스크에 메모리 없는 임시 스냅샷을
/// 찍어 그것을 읽고 끝나면 지운다(스냅샷 리소스는 만들지 않는다). 꺼진 VM은 지금 디스크를 읽는다.
/// 실패하면 실행기의 정리([`discard`])가 디렉터리를 지우고 `failed`로 남긴다.
pub(super) async fn backup(ctx: &Ctx, task: &Task, id: Uuid) -> Result<(), String> {
    use super::snapshot::resume_vm;
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

    if node.is_some() {
        let status = BackupStatus {
            node,
            ..backup.status.clone()
        };
        ctx.cluster
            .update_status::<BackupSpec>(id, &status)
            .await
            .map_err(|e| e.to_string())?;
    }
    let dir = backup_dir(&root, id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("cannot create backup directory: {e}"))?;
    // 새 디렉터리 이름이 남게 `backups`와(처음 만들었을 수 있으니) 풀 루트도 내려 쓴다.
    for parent in [root.join("backups"), root.clone()] {
        sync_path(&parent)
            .await
            .map_err(|e| format!("cannot create backup directory: {e}"))?;
    }
    let vars = dir.join(VARS_FILE);
    let tag = temp_tag(id);
    // 실행 중이면 디스크를 임시 스냅샷으로 얼리고 그때의 UEFI 변수를 복사한다.
    let tagged = running && !disks.is_empty();
    let resumed = if tagged {
        take_tags(ctx, &vm, &disks, id, &tag, false, || {
            copy_vars(ctx, &vm, &vars)
        })
        .await?
    } else {
        copy_vars(ctx, &vm, &vars)
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
        Ok(()) => export_disks(ctx, &disks, tagged.then_some(&*tag), tagged, &dir).await,
        Err(e) => Err(e.clone()),
    };
    // 임시 스냅샷은 결과와 상관없이 지운다. 남으면 공간만 쓴다(블록 디스크는 태그 청소가 지운다).
    if tagged {
        let running = !matches!(ctx.hv.inspect(vm.id).await, Ok(None));
        if let Err(e) = remove_tags(ctx, vm.id, running, &vm.spec, &tag).await {
            tracing::warn!(backup = %id, error = %e, "cannot remove temporary snapshot of backup");
        }
    }
    let disks = exported?;
    let uefi_vars = tokio::fs::try_exists(&vars).await.unwrap_or(false);
    if uefi_vars {
        sync_path(&vars)
            .await
            .map_err(|e| format!("cannot write UEFI variables: {e}"))?;
    }
    let manifest = BackupManifest {
        version: MANIFEST_VERSION,
        backup: id,
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

    let status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(created_at),
        disks,
        uefi_vars,
        error: None,
        node,
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

/// 백업 디렉터리를 지운다. 없으면 이미 지운 것이다.
async fn remove_dir(dir: &Path) -> Result<(), String> {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove backup directory: {e}")),
    }
}

/// 실패한 백업 Task의 흔적을 지운다: 아직 `pending`인 백업이면 디렉터리를 지우고 `failed`와 `error`를
/// 남긴다(리소스는 사용자가 보고 지운다). `unfinished`면(opld가 재시작해 중단) 실행 중인 VM의 디스크에
/// 남았을 수 있는 임시 스냅샷도 지운다(블록 디스크의 것은 태그 청소도 지운다).
pub(super) async fn discard(ctx: &Ctx, task: &Task, id: Uuid, error: &str, unfinished: bool) {
    let backup = match get_backup(ctx, id) {
        Ok(Some(b)) if b.status.phase == BackupPhase::Pending => b,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(backup = %id, error = %e, "cannot read backup of failed task");
            return;
        }
    };
    if unfinished
        && let Ok(Some(vm)) = ctx.cluster.get::<VmSpec>(task.spec.target)
        && matches!(ctx.hv.inspect(vm.id).await, Ok(Some(_)))
        && let Err(e) = remove_tags(ctx, vm.id, true, &vm.spec, &temp_tag(id)).await
    {
        tracing::warn!(backup = %id, error = %e, "cannot remove temporary snapshot of backup");
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
/// 않았으면(다른 노드에서 돌 수 있다) 실패다.
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    use opl_types::task::{TaskOp, TaskSpec};
    let Some(backup) = get_backup(ctx, id)? else {
        return Ok(());
    };
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    if tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::BackupRestore { backup, .. } if backup == id)
    }) {
        return Err("backup is being restored".into());
    }
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)
        .map_err(|e| e.to_string())?;
    if pool.is_none() {
        tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, "storage pool of backup is gone; deleting only the backup");
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
        remove_dir(&dir).await?;
    }
    match ctx.cluster.delete::<BackupSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
