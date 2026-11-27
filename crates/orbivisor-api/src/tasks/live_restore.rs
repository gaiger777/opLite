//! 라이브 복원 Task(계획 68 설계 §11.3·§11.4, `backup-live-restore` — opld가 다시 떠도 이어 가는 Task).
//!
//! 1. 보통 복원처럼 검사하고 빈 대상 볼륨·펌웨어·새 VM을 만든다(VM은 이 노드에서 `running`, `status.live_restore` =
//!    `Starting` — reconciler는 켜지 않는다).
//! 2. 백업을 내보낸다(저장소 백업은 `backup-serve` 헬퍼 — opld의 설정에 실행 파일이 없으면(시험) opld 안 NBD 서버, 옛 풀
//!    백업은 QEMU가 qcow2 체인을 곧바로 연다), 디스크마다 오버레이를 만들고 하이퍼바이저에 배선을 둔 뒤 `Copying`으로
//!    넘긴다 — reconciler가 그 배선으로 켠다(CPU·NUMA·펌웨어·방화벽은 보통 켜기와 같다).
//! 3. VM이 돌면 디스크마다 mirror를 걸고 지켜본다: 헬퍼가 죽으면 다시 띄우고, 백업을 읽지 못해 VM이 멈추면(`rerror=stop`)
//!    `Paused`로 적고 백업이 돌아오면 `cont`, 강제로 끄면 `Stopped`(`vm start`가 같은 배선으로 켠다), QEMU가 죽으면
//!    reconciler가 같은 배선으로 다시 켜고 mirror는 처음부터 다시 건다.
//! 4. 모두 `ready`(대상과 맞춰짐)면 `Pivoting`을 적고 갈아탄다 → 배선·오버레이·헬퍼를 치우고 `live_restore`를 지운다(보통
//!    VM). 갈아타기가 실패하면 `Copying`으로 돌아가 다시 옮긴다.
//!
//! 할 일은 모두 Raft의 VM 상태와 하이퍼바이저의 배선에서 다시 읽으므로 opld가 다시 떠도 같은 걸음에서 잇는다. 취소
//! (`live_restore.cancel`)는 VM을 끄고 VM·대상 볼륨·오버레이를 지운다.

use std::path::PathBuf;
use std::time::Duration;

use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_driver::{
    LiveRestoreDisk, LiveRestoreSource, RuntimeStatus, StopMode, backup_serve_alive,
    create_overlay, start_backup_serve, stop_backup_serve, vm_state_dir,
};
use orbivisor_store::nbd::serve::ServeConfig;
use orbivisor_types::backup::{Backup, RestoreRemap, backup_dir};
use orbivisor_types::backup_store::{LiveRestore, LiveRestorePhase};
use orbivisor_types::resource::Resource;
use orbivisor_types::storage::VolumeSpec;
use orbivisor_types::task::{Task, TaskOp, TaskSpec};
use orbivisor_types::vm::{PowerState, Vm, VmSpec};
use uuid::Uuid;

use super::backup::{backup_root, disk_chains, get_backup, read_manifest};
use super::backup_store::{open_store, read_snapshot, serve, socket_path};
use super::{Ctx, remove_volume};
use crate::stores;

/// 진행(`done_bytes`)을 Raft에 적고 멈춘 VM에 `cont`를 다시 해 보는 간격(저장소 폴링 몇 번마다 — 기본 5초 × 6 = 30초)
const POLLS_PER_REPORT: u32 = 6;
/// 취소가 VM이 꺼지기를 기다리는 한도
const STOP_WAIT: Duration = Duration::from_secs(60);
/// 백업을 읽지 못해 멈춘 VM의 까닭
const PAUSED: &str = "the VM paused because it could not read the backup; it continues when the backup can be read again";

/// 복원이 새 VM `new`를 라이브 복원으로 만들 때(`live`): 상태를 `Starting`으로 두고 이 노드를 돌려준다(새 VM은 이 노드에서
/// 켜진 채 만든다). 보통 복원이면 아무것도 하지 않는다.
pub(super) fn begin(
    ctx: &Ctx,
    task: &Task,
    new: &mut Vm,
    backup: Uuid,
    live: bool,
) -> Option<Uuid> {
    if !live {
        return None;
    }
    new.status.live_restore = Some(LiveRestore {
        backup,
        task: task.id,
        phase: LiveRestorePhase::Starting,
        done_bytes: 0,
        total_bytes: 0,
        started_at: crate::auth::unix_now(),
        error: None,
        cancel: false,
    });
    Some(ctx.config.node)
}

/// `backup-live-restore` Task. 대상(새 VM)이 아직 없으면 만들고, 라이브 복원 중이면 그 걸음에서 잇는다(`resume` — opld가
/// 다시 떴다 — 이거나 취소·다시 하기로 새로 만든 Task).
#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    ctx: &Ctx,
    task: &Task,
    backup: Uuid,
    name: &str,
    pool: &str,
    folder: Option<Uuid>,
    remap: Option<&RestoreRemap>,
    resume: bool,
) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let mut warning = None;
    match ctx.cluster.get::<VmSpec>(id).map_err(|e| e.to_string())? {
        Some(vm) if vm.status.live_restore.is_some() => {}
        // 갈아탄 뒤 끝을 적기 전에 opld가 죽었다.
        Some(_) if resume => return Ok(None),
        Some(vm) => return Err(format!("VM {:?} already exists", vm.name)),
        None if resume => {
            return Err(
                "interrupted: orbivisord restarted before the restored VM was created".into(),
            );
        }
        None => {
            warning = Box::pin(super::backup::restore(
                ctx, task, backup, name, pool, folder, remap, true,
            ))
            .await?;
        }
    }
    let done = Box::pin(drive(ctx, task, id)).await?;
    let warnings: Vec<String> = warning.into_iter().chain(done).collect();
    Ok((!warnings.is_empty()).then(|| warnings.join("; ")))
}

/// 지켜보며 걸음을 옮긴다. 끝나면(갈아탐) 경고, 취소면 오류다.
async fn drive(ctx: &Ctx, task: &Task, id: Uuid) -> Result<Option<String>, String> {
    let mut polls: u32 = POLLS_PER_REPORT;
    loop {
        let Some(vm) = ctx.cluster.get::<VmSpec>(id).map_err(|e| e.to_string())? else {
            release(ctx, id).await;
            return Err("the VM was deleted during its live restore".into());
        };
        let Some(lr) = vm.status.live_restore.clone() else {
            release(ctx, id).await;
            return Ok(None);
        };
        if lr.cancel {
            return cancel(ctx, &vm).await;
        }
        polls = polls.saturating_add(1);
        match step(ctx, &vm, &lr, polls >= POLLS_PER_REPORT).await {
            Ok(Next::Finish) => return finish(ctx, id).await,
            Ok(Next::Report(r)) => {
                let progress_due = polls >= POLLS_PER_REPORT;
                let changed = r.phase != lr.phase || r.error != lr.error || lr.task != task.id;
                let moved = (r.done, r.total) != (lr.done_bytes, lr.total_bytes);
                if changed || (moved && progress_due) {
                    write(ctx, id, task.id, |l| {
                        l.phase = r.phase;
                        l.error = r.error.clone();
                        l.done_bytes = r.done;
                        l.total_bytes = r.total;
                    })
                    .await?;
                }
                if progress_due {
                    polls = 0;
                }
            }
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "live restore step failed; will retry");
                if lr.error.as_deref() != Some(e.as_str()) || lr.task != task.id {
                    write(ctx, id, task.id, |l| l.error = Some(e.clone())).await?;
                }
            }
        }
        tokio::time::sleep(ctx.config.store_poll).await;
    }
}

/// 한 걸음의 결과
enum Next {
    /// 이 단계·까닭·진행을 적는다
    Report(Report),
    /// 갈아탔다(또는 대상이 이미 완전하다) — 치우고 끝낸다
    Finish,
}

struct Report {
    phase: LiveRestorePhase,
    error: Option<String>,
    done: u64,
    total: u64,
}

/// 한 걸음: 백업 내보내기·배선을 갖추고, VM이 돌면 mirror를 지켜보고 다 옮겼으면 갈아탄다. `retry`면 멈춘 VM에 `cont`를
/// 다시 해 본다.
async fn step(ctx: &Ctx, vm: &Vm, lr: &LiveRestore, retry: bool) -> Result<Next, String> {
    let id = vm.id;
    let hv = |e: orbivisor_driver::DriverError| e.to_string();
    let state = ctx.hv.inspect(id).await.map_err(hv)?;
    if lr.phase == LiveRestorePhase::Pivoting {
        // 모든 mirror가 `ready`이고 대상과 맞춰져 있었다(`actively-synced` — write-blocking이라 그때부터 대상은 게스트가 본
        // 디스크와 늘 같다). QEMU가 죽었어도 대상이 완전하므로 배선 없이 끝낸다(reconciler는 `Pivoting`인 VM을 켜지 않는다).
        // 돌고 있으면 갈아탄다 — 실패하면(어느 디스크의 `job-complete`가 실패해 작업이 없어졌다) `Copying`으로 돌아가 다시
        // 옮긴다(갈아탄 디스크는 드라이버가 배선에 적어 다시 옮기지 않는다, 리뷰 1 I3).
        if state.is_some() {
            return Ok(pivot(ctx, id, lr).await);
        }
        return Ok(Next::Finish);
    }
    let backup = get_backup(ctx, lr.backup)?.ok_or("the backup was deleted")?;
    let restarted = ensure_source(ctx, &backup, id).await?;
    if !ctx.hv.live_restore_wired(id) {
        wire(ctx, &backup, id).await?;
    }
    let Some(state) = state else {
        // 꺼져 있다: 강제로 껐거나(`Stopped`), reconciler가 켜기를 기다린다(준비가 끝나면 `Copying`으로 넘겨 켜게 한다).
        let phase = if vm.spec.power == PowerState::Stopped {
            LiveRestorePhase::Stopped
        } else {
            LiveRestorePhase::Copying
        };
        return Ok(Next::Report(Report {
            phase,
            error: None,
            done: lr.done_bytes,
            total: lr.total_bytes,
        }));
    };
    let jobs = ctx.hv.start_live_restore(id).await.map_err(hv)?;
    let done = jobs.iter().map(|j| j.done_bytes).sum();
    let total = jobs.iter().map(|j| j.total_bytes).sum();
    let job_error = jobs.iter().find_map(|j| j.error.clone());
    if state.status == RuntimeStatus::Other("io-error".into()) {
        // 백업을 읽지 못해 멈췄다(`rerror=stop`). 헬퍼를 다시 띄웠거나 때가 되면 이어 본다(아직 못 읽으면 다시 멈춘다).
        if restarted || retry {
            ctx.hv.resume(id).await.map_err(hv)?;
        }
        return Ok(Next::Report(Report {
            phase: LiveRestorePhase::Paused,
            error: Some(match job_error {
                Some(e) => format!("{PAUSED} ({e})"),
                None => PAUSED.into(),
            }),
            done,
            total,
        }));
    }
    if !jobs.is_empty() && jobs.iter().all(|j| j.ready && j.error.is_none()) {
        write(ctx, id, lr.task, |l| {
            l.phase = LiveRestorePhase::Pivoting;
            l.error = None;
            l.done_bytes = done;
            l.total_bytes = total;
        })
        .await?;
        let lr = LiveRestore {
            done_bytes: done,
            total_bytes: total,
            ..lr.clone()
        };
        return Ok(pivot(ctx, id, &lr).await);
    }
    Ok(Next::Report(Report {
        phase: LiveRestorePhase::Copying,
        error: job_error,
        done,
        total,
    }))
}

/// 갈아탄다. 드라이버가 갈아타지 못한 디스크를 알린 실패(`LiveRestore` — 갈아탄 디스크는 배선에 적었다)면 `Copying`으로
/// 돌아가 그 까닭을 적는다: 다음 걸음이 그 디스크를 다시 옮긴다(`Pivoting`에 머물며 같은 갈아타기를 끝없이 되풀이하지
/// 않는다). 그 밖의 실패(QMP 시간 초과 등 — 어느 디스크가 갈아탔는지 아직 모른다)는 `Pivoting`에 머물러 다시 해 본다.
async fn pivot(ctx: &Ctx, id: Uuid, lr: &LiveRestore) -> Next {
    match ctx.hv.pivot_live_restore(id).await {
        Ok(()) => Next::Finish,
        Err(e) => {
            let phase = if matches!(e, orbivisor_driver::DriverError::LiveRestore(_)) {
                tracing::warn!(vm = %id, error = %e, "switching to the restored disks failed; copying again");
                LiveRestorePhase::Copying
            } else {
                tracing::warn!(vm = %id, error = %e, "switching to the restored disks failed; will retry");
                LiveRestorePhase::Pivoting
            };
            Next::Report(Report {
                phase,
                error: Some(format!("switching to the restored disks failed: {e}")),
                done: lr.done_bytes,
                total: lr.total_bytes,
            })
        }
    }
}

/// VM `id`의 라이브 복원 백업 소켓
fn source_socket(ctx: &Ctx, id: Uuid) -> PathBuf {
    socket_path(ctx, "lr", id)
}

fn helper_files(ctx: &Ctx, id: Uuid) -> (PathBuf, PathBuf) {
    let dir = &ctx.config.backup_run_dir;
    (
        dir.join(format!("lr-{}.pid", id.simple())),
        dir.join(format!("lr-{}.log", id.simple())),
    )
}

/// 오버레이를 두는 디렉터리: VM 상태 디렉터리(지운 VM의 것은 상태 디렉터리 정리가 지운다)
fn overlay_dir(ctx: &Ctx, id: Uuid) -> PathBuf {
    match &ctx.config.state_dir {
        Some(state) => vm_state_dir(state, id),
        None => ctx
            .config
            .backup_run_dir
            .join(format!("lr-{}", id.simple())),
    }
}

/// 저장소 백업을 내보내는 헬퍼(또는 opld 안 서버)가 돌게 한다. 새로 띄웠으면 true(풀 백업은 늘 false).
async fn ensure_source(ctx: &Ctx, backup: &Backup, id: Uuid) -> Result<bool, String> {
    let Some(name) = backup.spec.store.as_deref() else {
        return Ok(false);
    };
    let socket = source_socket(ctx, id);
    match &ctx.config.backup_serve_exe {
        Some(exe) => {
            let (pid_file, log) = helper_files(ctx, id);
            if backup_serve_alive(&pid_file).is_some() && socket.exists() {
                return Ok(false);
            }
            let store = stores::get(&ctx.cluster, name)?;
            let backend = stores::serve_backend(
                &ctx.cluster,
                &store,
                ctx.config.node,
                &ctx.config.mount_root,
            )?;
            let cfg = ServeConfig {
                socket,
                backend,
                key: store.spec.encryption.as_ref().map(|e| e.key.clone()),
                vm: backup.spec.vm,
                backup: backup.id,
                cache_mib: ctx.config.serve_cache_mib,
                pid_file: Some(pid_file),
            };
            tracing::info!(vm = %id, "starting the backup-serve helper of a live restore");
            start_backup_serve(exe, ctx.config.backup_serve_scope, id, &cfg, &log)
                .await
                .map_err(|e| format!("cannot serve the backup: {e}"))?;
            Ok(true)
        }
        None => {
            {
                let mut serving = ctx.live_serving.lock().unwrap();
                match serving.get(&id) {
                    Some(s) if s.alive() && socket.exists() => return Ok(false),
                    Some(_) => {
                        serving.remove(&id);
                    }
                    None => {}
                }
            }
            let (_, store) = open_store(ctx, name).await?;
            let (m, _) = read_snapshot(&store, backup).await?;
            let s = serve(ctx, &store, &m, socket).await?;
            ctx.live_serving.lock().unwrap().insert(id, s);
            Ok(true)
        }
    }
}

/// 백업의 디스크마다 읽을 곳과 가상 크기
async fn sources(
    ctx: &Ctx,
    backup: &Backup,
    id: Uuid,
) -> Result<Vec<(LiveRestoreSource, u64)>, String> {
    if let Some(name) = backup.spec.store.as_deref() {
        let (_, store) = open_store(ctx, name).await?;
        let (m, _) = read_snapshot(&store, backup).await?;
        let socket = source_socket(ctx, id);
        return Ok(m
            .disks
            .iter()
            .map(|d| {
                let src = LiveRestoreSource::Nbd {
                    socket: socket.clone(),
                    export: d.name.clone(),
                };
                (src, d.virtual_bytes)
            })
            .collect());
    }
    // 옛 풀 백업(§11.5): 그 파일을 읽는 노드에서만
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
        return Err(format!("backup is stored on node {node}"));
    }
    let (_, root) = backup_root(ctx, &backup.spec.pool)?;
    let dir = backup_dir(&root, backup.id);
    let manifest = read_manifest(&dir).await?;
    let chains = disk_chains(ctx, backup, &root, &dir, &manifest).await?;
    Ok(chains
        .into_iter()
        .zip(&manifest.disks)
        .map(|(chain, d)| (LiveRestoreSource::Qcow2 { chain }, d.virtual_bytes))
        .collect())
}

/// 오버레이를 만들고(있으면 그대로 — 게스트가 쓴 것이 있다) 하이퍼바이저에 배선을 둔다.
async fn wire(ctx: &Ctx, backup: &Backup, id: Uuid) -> Result<(), String> {
    let dir = overlay_dir(ctx, id);
    let mut disks = Vec::new();
    for (i, (source, size)) in sources(ctx, backup, id).await?.into_iter().enumerate() {
        let overlay = dir.join(format!("lr-disk{i}.qcow2"));
        create_overlay(&ctx.config.qemu_img, &overlay, size)
            .await
            .map_err(|e| format!("cannot create the overlay of disk {i}: {e}"))?;
        disks.push(LiveRestoreDisk {
            source,
            overlay,
            pivoted: false,
        });
    }
    ctx.hv
        .set_live_restore(id, Some(&disks))
        .await
        .map_err(|e| e.to_string())
}

/// 배선·오버레이·백업 내보내기를 치운다(실패는 경고만).
async fn release(ctx: &Ctx, id: Uuid) {
    if let Err(e) = ctx.hv.set_live_restore(id, None).await {
        tracing::warn!(vm = %id, error = %e, "cannot remove the live restore wiring");
    }
    let (pid_file, _) = helper_files(ctx, id);
    stop_backup_serve(&pid_file, &source_socket(ctx, id)).await;
    ctx.live_serving.lock().unwrap().remove(&id);
    let dir = overlay_dir(ctx, id);
    if ctx.config.state_dir.is_none() {
        let _ = tokio::fs::remove_dir(&dir).await;
    }
}

/// 갈아탄 뒤(또는 대상이 완전한 채 QEMU가 죽은 뒤): 배선·오버레이·헬퍼를 치우고 라이브 복원 상태를 지운다 — 이제 보통 VM이다.
async fn finish(ctx: &Ctx, id: Uuid) -> Result<Option<String>, String> {
    release(ctx, id).await;
    update(ctx, id, |lr| {
        if lr.is_none() {
            return false;
        }
        *lr = None;
        true
    })
    .await?;
    Ok(None)
}

/// 취소(§11.4 `discard`): reconciler는 다시 켜지 않는다. VM을 끄고 VM·대상 볼륨·배선·오버레이·헬퍼를 지운다.
async fn cancel(ctx: &Ctx, vm: &Vm) -> Result<Option<String>, String> {
    let id = vm.id;
    let hv = |e: orbivisor_driver::DriverError| e.to_string();
    if ctx.hv.inspect(id).await.map_err(hv)?.is_some() {
        ctx.hv.stop(id, StopMode::Force).await.map_err(hv)?;
        let deadline = tokio::time::Instant::now() + STOP_WAIT;
        while ctx.hv.inspect(id).await.map_err(hv)?.is_some() {
            if tokio::time::Instant::now() >= deadline {
                return Err("cancelling the live restore: the VM did not stop".into());
            }
            tokio::time::sleep(ctx.config.poll_interval).await;
        }
    }
    release(ctx, id).await;
    match ctx.cluster.delete::<VmSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
        Err(e) => return Err(format!("cannot delete VM {:?}: {e}", vm.name)),
    }
    crate::permissions::drop_orphans(&ctx.cluster).await;
    if let Some(api) = &ctx.config.api {
        crate::routing::detach_vm(api, id).await;
    }
    for disk in &vm.spec.disks {
        if let Ok(Some(v)) = ctx.cluster.get_by_name::<VolumeSpec>(&disk.volume)
            && let Err(e) = remove_volume(ctx, v.id).await
        {
            tracing::warn!(volume = %v.name, error = %e, "cannot delete a disk of a cancelled live restore");
        }
    }
    Err(format!(
        "the live restore of VM {:?} was cancelled; the VM and its disks were deleted",
        vm.name
    ))
}

/// 실패로 끝난 Task(정리, [`super::cleanup`]): 라이브 복원 상태가 이 Task의 것이면 `Failed`와 까닭을 남긴다(reconciler는
/// 켜지 않는다 — 취소나 새 Task로 잇는다).
pub(super) async fn failed(ctx: &Ctx, task: &Task, error: &str) {
    let r = update(ctx, task.spec.target, |lr| match lr {
        Some(l) if l.task == task.id && l.phase != LiveRestorePhase::Failed => {
            l.phase = LiveRestorePhase::Failed;
            l.error = Some(error.to_string());
            true
        }
        _ => false,
    })
    .await;
    if let Err(e) = r {
        tracing::warn!(task = %task.id, error = %e, "cannot record the failed live restore");
    }
}

/// 라이브 복원 상태를 이 Task(`task`)의 것으로 고쳐 쓴다(없으면 할 일이 없다).
async fn write(
    ctx: &Ctx,
    id: Uuid,
    task: Uuid,
    f: impl Fn(&mut LiveRestore),
) -> Result<(), String> {
    update(ctx, id, |lr| match lr {
        Some(l) => {
            l.task = task;
            f(l);
            true
        }
        None => false,
    })
    .await
}

/// VM 상태의 `live_restore`를 읽은 revision에서만 바꾼다(`f`가 false면 쓰지 않는다). VM이 없으면 할 일이 없다.
async fn update(
    ctx: &Ctx,
    id: Uuid,
    mut f: impl FnMut(&mut Option<LiveRestore>) -> bool,
) -> Result<(), String> {
    loop {
        let Some(vm) = ctx.cluster.get::<VmSpec>(id).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        let mut status = vm.status.clone();
        if !f(&mut status.live_restore) || status == vm.status {
            return Ok(());
        }
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot write the live restore state; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 라이브 복원을 취소하라고 적는다(§11.4 `POST /vms/{key}/live-restore/cancel { discard: true }`). VM의 라이브 복원
/// Task가 끝났으면(실패) 같은 백업의 라이브 복원 Task를 VM이 배치된 노드에 새로 만들어 취소를 하게 한다. 라이브 복원
/// 중이 아니면 오류다. 그 Task를 돌려준다.
pub async fn request_cancel(cluster: &Cluster, vm: Uuid) -> Result<Uuid, String> {
    loop {
        let v = cluster
            .get::<VmSpec>(vm)
            .map_err(|e| e.to_string())?
            .ok_or("VM not found")?;
        let Some(lr) = v.status.live_restore.clone() else {
            return Err(format!("VM {:?} is not being live-restored", v.name));
        };
        let mut status = v.status.clone();
        if let Some(l) = status.live_restore.as_mut() {
            l.cancel = true;
        }
        match cluster
            .update_status_checked::<VmSpec>(vm, v.revision, &status)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
        let (_, tasks) = cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
        if let Some(t) = tasks.iter().find(|t| {
            t.id == lr.task && !t.status.state.is_finished()
                || t.spec.target == vm
                    && matches!(t.spec.op, TaskOp::BackupLiveRestore { .. })
                    && !t.status.state.is_finished()
        }) {
            return Ok(t.id);
        }
        let node = v.spec.node.or(v.status.node).ok_or("the VM has no node")?;
        let op = TaskOp::BackupLiveRestore {
            backup: lr.backup,
            name: v.name.clone(),
            pool: String::new(),
            folder: None,
            // 다시 하기는 VM이 이미 있어 맵이 쓰이지 않는다.
            remap: None,
        };
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: vm,
                node,
            },
        );
        t.name = t.id.to_string();
        cluster.create(&t).await.map_err(|e| e.to_string())?;
        return Ok(t.id);
    }
}
