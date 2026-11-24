//! VM 내보내기 Task(계획 72 §12.1): 꺼진 VM의 디스크마다 streamOptimized VMDK를 쓰고 다시 읽어 SHA-256을 잰 뒤 OVF 서술자와
//! 매니페스트를 내보내기 디렉터리(`{풀}/orbivisor-exports/{id}/`)에 둔다. VM의 줄에 서서 스냅샷·백업·복제와 차례로 돈다.
//! 실패하면(다시 시작으로 끊겨도) 디렉터리를 지우고 `failed`로 남긴다. 지우기 Task는 디렉터리와 리소스를 지운다.

use std::path::{Path, PathBuf};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::storage::StoragePoolSpec;
use opl_types::task::Task;
use opl_types::vm::{PowerState, Vm};
use opl_types::vm_export::{VmExport, VmExportFile, VmExportPhase, VmExportSpec, VmExportStatus};
use opl_types::vm_import::VmImportSpec;
use sha2::Digest as _;
use uuid::Uuid;

use super::Ctx;
use super::backup::{backup_root, sync_path};
use super::snapshot::{check_local, check_settled, get_vm, is_down, ready_disks};
use crate::vm_exports::tar::ova_size;
use crate::vm_imports::ovf_write::{
    OTHER_GUEST, OvfDisk, OvfVm, export_warnings, write_manifest, write_ovf,
};

fn get_export(ctx: &Ctx, id: Uuid) -> Result<Option<VmExport>, String> {
    ctx.cluster
        .get::<VmExportSpec>(id)
        .map_err(|e| e.to_string())
}

/// 상태를 적는다(기록될 때까지 다시 — 지워졌으면 그만둔다).
async fn record(ctx: &Ctx, id: Uuid, status: &VmExportStatus) -> Result<(), String> {
    match ctx.cluster.update_status::<VmExportSpec>(id, status).await {
        Ok(_) => Ok(()),
        Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Err("export was deleted".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// 파일의 SHA-256(소문자 16진)과 크기. 멈춘 NFS가 워커를 붙잡지 않게 blocking 스레드에서 읽는다.
async fn hash_file(path: &Path) -> Result<(String, u64), String> {
    let shown = path.display().to_string();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        Ok::<_, std::io::Error>((hex::encode(hasher.finalize()), size))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("cannot read {shown}: {e}"))
}

/// 작은 파일을 쓰고 내려 쓴다.
async fn write_small(path: &Path, data: &[u8]) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let what = || path.file_name().map(|n| n.to_string_lossy().into_owned());
    let mut f = tokio::fs::File::create(path)
        .await
        .map_err(|e| format!("cannot write {:?}: {e}", what()))?;
    f.write_all(data)
        .await
        .map_err(|e| format!("cannot write {:?}: {e}", what()))?;
    f.sync_all()
        .await
        .map_err(|e| format!("cannot write {:?}: {e}", what()))
}

/// VMware guestId처럼 보이는지(`ubuntu64Guest` — 글자·숫자·`_`이고 `Guest`로 끝남)
fn is_guest_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && s.ends_with("Guest")
}

/// OVF `vmw:osType`: 요청한 값, 없으면 이 VM을 만든 가져오기(계획 29·30)의 원본 guestId. 모르면 None
fn guest_os(ctx: &Ctx, export: &VmExport, vm: &Vm) -> Option<String> {
    if let Some(g) = &export.spec.guest_os {
        return Some(g.clone());
    }
    let (_, imports) = ctx.cluster.list::<VmImportSpec>().ok()?;
    imports
        .iter()
        .filter(|i| i.status.vm == Some(vm.id))
        .find_map(|i| i.status.source_vm.as_ref())
        .map(|s| s.guest_os.clone())
        .filter(|g| is_guest_id(g))
}

/// 내보내기 디렉터리 `{root}/orbivisor-exports/{id}`
pub(crate) fn export_dir(root: &Path, id: Uuid) -> PathBuf {
    root.join(VmExportSpec::dir(id))
}

async fn remove_dir(dir: &Path) -> Result<(), String> {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove export directory: {e}")),
    }
}

/// `pending` 내보내기 `id`를 채운다. 대상 VM이 꺼져 있어야 한다(API가 본 뒤 켰을 수 있다). OVF에 넣지 못한 것은 내보내기의
/// `warnings`에 적는다.
pub(super) async fn export(ctx: &Ctx, task: &Task, id: Uuid) -> Result<Option<String>, String> {
    let export = get_export(ctx, id)?.ok_or("export was deleted")?;
    if export.status.phase != VmExportPhase::Pending {
        return Err("export is not pending".into());
    }
    let vm = get_vm(ctx, task.spec.target)?;
    check_settled(&vm)?;
    if vm.spec.power != PowerState::Stopped || !is_down(&vm) {
        return Err(format!("VM {:?} is running; stop it to export it", vm.name));
    }
    let (_, root) = backup_root(ctx, &export.spec.pool)?;
    let disks = ready_disks(ctx, &vm.spec)?;
    for (volume, pool) in &disks {
        check_local(ctx, volume, pool)?;
    }
    let mut status = VmExportStatus {
        phase: VmExportPhase::Writing,
        ..Default::default()
    };
    record(ctx, id, &status).await?;

    let dir = export_dir(&root, id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("cannot create export directory: {e}"))?;
    for path in [
        dir.clone(),
        root.join(opl_types::vm_export::EXPORT_DIR),
        root.clone(),
    ] {
        sync_path(&path)
            .await
            .map_err(|e| format!("cannot create export directory: {e}"))?;
    }

    let mut ovf_disks = Vec::with_capacity(disks.len());
    let mut files = Vec::with_capacity(disks.len() + 2);
    for (i, ((volume, pool), disk)) in disks.iter().zip(&vm.spec.disks).enumerate() {
        let file = format!("{}-disk{i}.vmdk", vm.name);
        let dest = dir.join(&file);
        let format = volume.spec.format;
        let source = ctx
            .storage
            .volume_source(pool, volume.id, format, None)
            .await
            .map_err(|e| format!("cannot read volume {:?}: {e}", volume.name))?;
        let populated = ctx
            .storage
            .volume_info(pool, volume.id, format)
            .await
            .ok()
            .map(|i| i.allocated_bytes)
            .filter(|b| *b > 0);
        let written = ctx
            .storage
            .export_vmdk(&source, &dest, export.spec.hw_version)
            .await;
        if let Err(e) = ctx
            .storage
            .release_source(pool, volume.id, format, None)
            .await
        {
            tracing::warn!(volume = %volume.name, error = %e, "cannot release export source");
        }
        written.map_err(|e| format!("cannot export volume {:?}: {e}", volume.name))?;
        let (sha256, size) = hash_file(&dest).await?;
        ovf_disks.push(OvfDisk {
            file: file.clone(),
            size,
            capacity: source.size_bytes,
            populated,
            bus: disk.bus,
        });
        files.push(VmExportFile {
            name: file,
            size,
            sha256,
        });
    }

    let guest = guest_os(ctx, &export, &vm);
    let warnings = export_warnings(&vm.spec, guest.is_some(), export.spec.keep_macs);
    let ovf = write_ovf(&OvfVm {
        name: &vm.name,
        spec: &vm.spec,
        disks: &ovf_disks,
        guest_os: guest.as_deref().unwrap_or(OTHER_GUEST),
        hw_version: export.spec.hw_version,
        keep_macs: export.spec.keep_macs,
    });
    let ovf_name = format!("{}.ovf", vm.name);
    let ovf_sha = hex::encode(sha2::Sha256::digest(ovf.as_bytes()));
    write_small(&dir.join(&ovf_name), ovf.as_bytes()).await?;
    let mf_name = format!("{}.mf", vm.name);
    let manifest = write_manifest(
        std::iter::once((ovf_name.as_str(), ovf_sha.as_str()))
            .chain(files.iter().map(|f| (f.name.as_str(), f.sha256.as_str()))),
    );
    write_small(&dir.join(&mf_name), manifest.as_bytes()).await?;
    sync_path(&dir)
        .await
        .map_err(|e| format!("cannot write export directory: {e}"))?;

    // OVA 차례: 서술자 → 매니페스트 → 디스크(DSP0243)
    let mut all = vec![
        VmExportFile {
            name: ovf_name,
            size: ovf.len() as u64,
            sha256: ovf_sha,
        },
        VmExportFile {
            name: mf_name,
            size: manifest.len() as u64,
            sha256: hex::encode(sha2::Sha256::digest(manifest.as_bytes())),
        },
    ];
    all.extend(files);
    status = VmExportStatus {
        phase: VmExportPhase::Ready,
        ova_bytes: ova_size(all.iter().map(|f| (f.name.as_str(), f.size))),
        files: all,
        warnings,
        error: None,
    };
    record(ctx, id, &status).await?;
    Ok(None)
}

/// 실패한 내보내기 Task의 흔적을 지운다: 끝나지 않은(`pending`·`writing`) 내보내기면 디렉터리를 지우고 `failed`와 `error`를
/// 남긴다(리소스는 사용자가 보고 지운다).
pub(super) async fn discard(ctx: &Ctx, id: Uuid, error: &str) {
    let export = match get_export(ctx, id) {
        Ok(Some(e))
            if matches!(
                e.status.phase,
                VmExportPhase::Pending | VmExportPhase::Writing
            ) =>
        {
            e
        }
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(export = %id, error = %e, "cannot read export of failed task");
            return;
        }
    };
    match backup_root(ctx, &export.spec.pool) {
        Ok((_, root)) => {
            if let Err(e) = remove_dir(&export_dir(&root, id)).await {
                tracing::warn!(export = %id, error = %e, "cannot clean up failed export");
            }
        }
        Err(e) => tracing::warn!(export = %id, error = %e, "cannot clean up failed export"),
    }
    let status = VmExportStatus {
        phase: VmExportPhase::Failed,
        error: Some(error.to_string()),
        ..Default::default()
    };
    loop {
        match ctx.cluster.update_status::<VmExportSpec>(id, &status).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(export = %id, error = %e, "cannot mark export failed; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 내보내기의 디렉터리와 리소스를 지운다. 풀이 없어졌으면 리소스만 지운다(파일도 함께 없어졌다). 공유가 아닌 풀의 파일이
/// 다른 노드에 있으면 실패한다(지우지 않은 파일을 잃은 것처럼 리소스를 지우지 않는다).
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<Option<String>, String> {
    let Some(export) = get_export(ctx, id)? else {
        return Ok(None);
    };
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&export.spec.pool)
        .map_err(|e| e.to_string())?;
    if pool.is_some() {
        let (pool, root) = backup_root(ctx, &export.spec.pool)?;
        let dir = export_dir(&root, id);
        if !opl_types::storage::is_shared(&pool)
            && export.spec.node != ctx.config.node
            && tokio::fs::try_exists(&dir).await.ok() != Some(true)
        {
            return Err(format!(
                "export files are on node {}, not on this node",
                export.spec.node
            ));
        }
        remove_dir(&dir).await?;
    } else {
        tracing::warn!(export = %export.name, pool = %export.spec.pool, "storage pool of export is gone; deleting only the export");
    }
    match ctx.cluster.delete::<VmExportSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_ids() {
        assert!(is_guest_id("ubuntu64Guest"));
        assert!(is_guest_id("windows2019srv_64Guest"));
        assert!(!is_guest_id("windows"));
        assert!(!is_guest_id("Ubuntu Linux (64-bit)"));
        assert!(!is_guest_id(""));
    }
}
