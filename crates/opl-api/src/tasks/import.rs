//! VM 가져오기 Task(설계 §2.4, ESXi 원본은 §3.4)와 가져오기 지우기 Task. 둘 다 가져오기의 줄에 서서
//! 겹치지 않고, 원본이 있는 노드(ESXi 원본은 자격 증명을 가진 노드)에서 돈다. API가 확인한 뒤 줄에서
//! 기다리는 사이 이름이나 풀이 바뀌었을 수 있으므로 다시 확인한다.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::resource::Resource;
use opl_types::storage::{SourceFormat, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{MacAddr, Vm, VmSpec};
use opl_types::vm_import::{
    EsxiConnection, ImportPhase, ImportRun, ImportSource, SourceDisk, VmImport, VmImportSpec,
    VmImportStatus,
};
use uuid::Uuid;

use super::backup::ready_pool;
use super::clone::{create_vm, made, volume_name};
use super::{Ctx, mark_ready};
use crate::vm_imports::esxi;
use crate::vm_imports::ova::{Ova, extract, parse_ova};
use crate::vm_imports::{
    CREDENTIALS_LOST, DOWNLOADED_DISK, EXTRACTED_DISK, SOURCE_OVA, import_dir, import_spec,
};
use crate::volumes::{UPLOAD_RESERVE_BYTES, default_format};

fn get_import(ctx: &Ctx, id: Uuid) -> Result<Option<VmImport>, String> {
    ctx.cluster
        .get::<VmImportSpec>(id)
        .map_err(|e| e.to_string())
}

/// 이 노드에 있는 가져오기 `id`의 원본 디렉터리
fn source_dir(ctx: &Ctx, id: Uuid) -> Result<PathBuf, String> {
    let dir = ctx
        .config
        .upload_dir
        .as_deref()
        .ok_or("this node has no upload directory")?;
    Ok(import_dir(dir, id))
}

/// 가져오기의 status를 `change`로 바꿔 기록한다. `change`가 `false`면 쓰지 않는다. 다른 쓰기와 부딪치면
/// 다시 읽어 바꾸고, 저장소에 닿지 않으면 기록될 때까지 다시 시도한다. 가져오기가 없어졌으면 그만둔다.
async fn update_import(ctx: &Ctx, id: Uuid, change: impl Fn(&mut VmImportStatus) -> bool) {
    loop {
        let import = match get_import(ctx, id) {
            Ok(Some(import)) => import,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(import = %id, error = %e, "cannot read vm-import; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        let mut status = import.status.clone();
        if !change(&mut status) {
            return;
        }
        match ctx
            .cluster
            .update_status_checked::<VmImportSpec>(id, import.revision, &status)
            .await
        {
            Ok(_) => return,
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(import = %id, error = %e, "cannot record vm-import state; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 디스크를 얻는 곳
enum Origin {
    /// 올린 OVA(`source`)에서 추출한다.
    Ova { source: PathBuf, ova: Ova },
    /// ESXi 데이터스토어에서 extent를 받는다.
    Esxi {
        conn: EsxiConnection,
        datastore: String,
    },
}

/// `importing` 가져오기 `id`를 `run`의 값으로 대상 id의 새 VM으로 만든다(설계 §2.4): 원본 디스크마다
/// OVA에서 추출하거나(`.mf`가 있으면 해시 확인) ESXi에서 extent를 받아(§3.4, 이 노드 메모리의 자격 증명)
/// 풀 `run.pool`에 새 볼륨 `{name}-disk{i}`(풀의 기본 형식)를 만들어 옮긴 뒤, 꺼진 VM을 만든다. 끝나면
/// 가져오기를 `done`으로 바꾸고 원본 디렉터리와 자격 증명을 지운다.
/// 원본 MAC을 쓰는데 다른 VM이 그 MAC을 쓰면 디스크를 옮기기 전에 실패한다. 디스크마다 업로드
/// 디렉터리에 추출하거나 받을 여유를 본다. ESXi 자격 증명이 없으면(opld가 다시 시작했다) 바로 실패한다.
/// 실패하면 실행기의 정리([`discard`])가 만든 볼륨과 추출한 파일을 지우고 가져오기를 `ready`로 되돌린다.
pub(super) async fn import(
    ctx: &Ctx,
    task: &Task,
    id: Uuid,
    run: &ImportRun,
) -> Result<(), String> {
    let import = get_import(ctx, id)?.ok_or("vm-import was deleted")?;
    if import.status.phase != ImportPhase::Importing {
        return Err("the import is not being run".into());
    }
    let (Some(src), Some(proposal)) = (&import.status.source_vm, &import.status.proposal) else {
        return Err("the import has no source VM".into());
    };
    let esxi = match &import.spec.source {
        ImportSource::Esxi(source) => {
            let conn = ctx
                .config
                .import_credentials
                .get(id)
                .ok_or(CREDENTIALS_LOST)?;
            Some((conn, source.datastore.clone()))
        }
        ImportSource::Ova(_) => None,
    };
    if ctx
        .cluster
        .get_by_name::<VmSpec>(&run.name)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("VM {:?} already exists", run.name));
    }
    if run.keep_macs {
        let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        let used: BTreeSet<MacAddr> = vms
            .iter()
            .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
            .collect();
        if let Some(mac) = src
            .nics
            .iter()
            .filter_map(|n| n.mac)
            .find(|m| used.contains(m))
        {
            return Err(format!("MAC address {mac} is already in use"));
        }
    }
    let pool = ready_pool(ctx, &run.pool)?;
    let format = default_format(&pool);
    let dir = source_dir(ctx, id)?;
    let origin = match esxi {
        Some((conn, datastore)) => {
            // ESXi 가져오기의 디렉터리는 실행할 때 만든다.
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            Origin::Esxi { conn, datastore }
        }
        None => {
            let source = dir.join(SOURCE_OVA);
            let path = source.clone();
            let ova = tokio::task::spawn_blocking(move || parse_ova(&path))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("cannot read the OVA: {e}"))?;
            Origin::Ova { source, ova }
        }
    };

    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let mut copies: Vec<Volume> = Vec::with_capacity(src.disks.len());
    for disk in &src.disks {
        let failed = |e: &dyn std::fmt::Display| format!("cannot import disk {:?}: {e}", disk.name);
        let (file, from) = fetch(ctx, &origin, &dir, disk)
            .await
            .map_err(|e| failed(&e))?;
        let name = volume_name(&run.name, &disk.name, |n| taken.contains(n));
        taken.insert(name.clone());
        let mut volume: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: pool.name.clone(),
                size_bytes: disk.capacity_bytes,
                format,
            },
        );
        volume.status.node = Some(ctx.config.node);
        // 만들기 전에 적는다. 만들기가 끝났는지 모르는 채 끊겨도 정리가 찾는다.
        made(ctx, task.id, |m| m.volumes.push(volume.id));
        ctx.cluster
            .create(&volume)
            .await
            .map_err(|e| format!("cannot create volume {:?}: {e}", volume.name))?;
        ctx.storage
            .import_volume(&pool, volume.id, &file, from, format)
            .await
            .map_err(|e| failed(&e))?;
        remove_file(&file).await;
        copies.push(volume);
    }

    // 볼륨은 VM을 만들기 바로 전에 `ready`로 바꾼다(복제·복원과 같다).
    for volume in &copies {
        mark_ready(ctx, volume, &pool).await;
    }
    let names: Vec<String> = copies.iter().map(|v| v.name.clone()).collect();
    let spec = import_spec(src, proposal, run, &names)?;
    spec.validate()
        .map_err(|e| format!("invalid VM spec: {e}"))?;
    let mut new: Vm = Resource::new(run.name.as_str(), spec);
    new.id = task.spec.target;
    create_vm(ctx, task, new).await?;

    let vm = task.spec.target;
    update_import(ctx, id, |s| {
        s.phase = ImportPhase::Done;
        s.vm = Some(vm);
        s.error = None;
        true
    })
    .await;
    ctx.config.import_credentials.remove(id);
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
        tracing::warn!(path = %dir.display(), error = %e, "cannot remove the import files");
    }
    Ok(())
}

/// 원본 디스크 `disk`를 가져오기 디렉터리 `dir`에 꺼내 두고 그 파일과 형식을 돌려준다. 꺼내기 전에 업로드
/// 디렉터리에 여유(꺼낼 크기 + 1 GiB)가 있는지 본다. ESXi extent는 씬 디스크도 전체 크기로 받고, 받은
/// 길이가 디스크 용량과 다르면 실패한다.
async fn fetch(
    ctx: &Ctx,
    origin: &Origin,
    dir: &Path,
    disk: &SourceDisk,
) -> Result<(PathBuf, SourceFormat), String> {
    let (path, format) = match origin {
        Origin::Ova { source, ova } => {
            let entry = ova
                .index
                .get(&disk.file)
                .cloned()
                .ok_or_else(|| format!("{:?} is not in the OVA", disk.file))?;
            check_space(dir, entry.size, "extract", &disk.file)?;
            let path = dir.join(EXTRACTED_DISK);
            let digest = ova.manifest.get(&disk.file).cloned();
            let (from, to) = (source.clone(), path.clone());
            tokio::task::spawn_blocking(move || extract(&from, &entry, &to, digest.as_ref()))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            (path, SourceFormat::Vmdk)
        }
        Origin::Esxi { conn, datastore } => {
            check_space(dir, disk.capacity_bytes, "download", &disk.file)?;
            let path = dir.join(DOWNLOADED_DISK);
            esxi::download(conn, datastore, &disk.file, &path, disk.capacity_bytes)
                .await
                .map_err(|e| e.to_string())?;
            (path, SourceFormat::Raw)
        }
    };
    ctx.storage
        .check_import_source(&path, format)
        .await
        .map_err(|e| e.to_string())?;
    Ok((path, format))
}

/// 업로드 디렉터리에 `size` 바이트를 쓰고도 여유가 남는지 본다.
fn check_space(dir: &Path, size: u64, doing: &str, file: &str) -> Result<(), String> {
    let available = fs4::available_space(dir).map_err(|e| e.to_string())?;
    if size.saturating_add(UPLOAD_RESERVE_BYTES) > available {
        return Err(format!(
            "not enough space in the upload directory to {doing} {file}"
        ));
    }
    Ok(())
}

async fn remove_file(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "cannot remove extracted disk");
    }
}

/// 실패한 가져오기가 만든 볼륨(파일과 리소스)과 추출하거나 받은 파일을 지우고, 가져오기를 `ready`로
/// 되돌려 이유를 남긴다. 원본은 남으므로 값을 바꿔 다시 실행할 수 있다. 다만 ESXi 가져오기인데 이 노드
/// 메모리에 자격 증명이 없으면(opld가 다시 시작했다) 다시 실행해도 소용없으므로 `failed`로 끝낸다.
/// 실행 중이 아니던 가져오기는 그대로 둔다.
/// 대상 VM이 이미 있으면(VM을 만든 뒤 opld가 죽어 Task가 실패로 끝났다) 볼륨과 VM을 두고 가져오기를
/// `done`으로 끝내며 원본 디렉터리를 지운다. 대상 VM이 있는지 읽지 못하면 읽을 때까지 다시 시도한다(있는 VM의
/// 볼륨을 지우지 않게).
pub(super) async fn discard(ctx: &Ctx, task: &Task, id: Uuid, error: &str) {
    let vm = task.spec.target;
    let exists = loop {
        match ctx.cluster.get::<VmSpec>(vm) {
            Ok(found) => break found.is_some(),
            Err(e) => {
                tracing::warn!(vm = %vm, error = %e, "cannot read the imported VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    };
    if exists {
        ctx.cloned.lock().unwrap().remove(&task.id);
        ctx.config.import_credentials.remove(id);
        update_import(ctx, id, |s| {
            if s.phase != ImportPhase::Importing {
                return false;
            }
            s.phase = ImportPhase::Done;
            s.vm = Some(vm);
            s.error = None;
            true
        })
        .await;
        if let Ok(dir) = source_dir(ctx, id)
            && let Err(e) = tokio::fs::remove_dir_all(&dir).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %dir.display(), error = %e, "cannot remove the import files");
        }
        return;
    }
    super::clone::discard(ctx, task.id).await;
    if let Ok(dir) = source_dir(ctx, id) {
        remove_file(&dir.join(EXTRACTED_DISK)).await;
        remove_file(&dir.join(DOWNLOADED_DISK)).await;
    }
    // 자격 증명을 잃은 ESXi 가져오기(opld가 다시 시작했다)
    let esxi = get_import(ctx, id)
        .ok()
        .flatten()
        .is_some_and(|i| matches!(i.spec.source, ImportSource::Esxi(_)));
    let lost = esxi && ctx.config.import_credentials.get(id).is_none();
    update_import(ctx, id, |s| {
        if s.phase != ImportPhase::Importing {
            return false;
        }
        if lost {
            s.phase = ImportPhase::Failed;
            s.error = Some(CREDENTIALS_LOST.into());
        } else {
            s.phase = ImportPhase::Ready;
            s.error = Some(error.to_string());
        }
        true
    })
    .await;
}

/// 가져오기의 원본 디렉터리와 리소스, ESXi 자격 증명을 지운다. 실행 Task가 끝나지 않았으면 지우지 않는다(`importing`이어도
/// 그 Task가 끝났으면 지운다).
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    if get_import(ctx, id)?.is_none() {
        return Ok(());
    }
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    if tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::VmImport { import, .. } if import == id)
    }) {
        return Err("the import is being run".into());
    }
    let dir = source_dir(ctx, id)?;
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove the import files: {e}")),
    }
    match ctx.cluster.delete::<VmImportSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {
            ctx.config.import_credentials.remove(id);
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}
