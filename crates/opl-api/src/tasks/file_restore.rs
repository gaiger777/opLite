//! 파일 복원 세션 Task(계획 69 §10.1): 백업을 일회용 도우미 VM에 붙여 열고, 닫기 요청·유휴·최대 시간까지 기다린 뒤 치운다.
//!
//! 걸음: 노드의 세션 자리 쥐기 → 원본 준비(저장소 백업은 매니페스트·인덱스를 읽어 opld 안 NBD 서버를 세션 디렉터리의
//! `nbd.sock`에 띄움, 옛 풀 백업은 qcow2 체인 확인) → 도우미 띄우기(덮개·QEMU·`Hello`) → `Probe` → 단계 `ready`(볼륨
//! 목록은 세션 등록부 — [`FileRestoreSessions`](crate::file_restore::FileRestoreSessions)) → 기다림 → `stop_helper` → NBD
//! 서버 멈춤 → 세션 디렉터리 지움. 어디서 끝나든(실패·취소·실행기 멈춤) 같은 치우기를 한다. 이어 가지 않는 Task다 —
//! opld가 다시 뜨면 실패로 남고 시작 때 치우기가 남은 도우미를 지운다(§10.5).

use std::sync::Arc;

use opl_driver::LiveRestoreSource;
use opl_driver::MirrorProgress;
use opl_driver::file_restore::{HelperLaunch, HelperPaths, HelperSource, stop_helper, volume_view};
use opl_types::backup::{Backup, backup_dir};
use opl_types::file_restore::VolumeState;
use opl_types::task::Task;
use uuid::Uuid;

use super::backup::{backup_root, disk_chains, get_backup, read_manifest};
use super::backup_store::{Serving, open_store, read_snapshot, serve};
use super::migrate::write_progress;
use super::{Ctx, REQUESTED_BY};
use crate::file_restore::{FileRestoreSessions, Session, SessionEnd};

/// 세션 단계(`TaskStatus.progress.stage` — 다른 노드의 API가 Raft에서 읽는다)
pub(crate) const STAGE_BOOTING: &str = "booting";
pub(crate) const STAGE_READY: &str = "ready";
pub(crate) const STAGE_CLOSING: &str = "closing";

/// 등록부에서 빼고 세션 디렉터리를 지운다(버려도 — 실행기가 멈춰 Task가 끊겨도).
struct Registered {
    sessions: Arc<FileRestoreSessions>,
    session: Arc<Session>,
    paths: HelperPaths,
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.session.set_closing();
        self.sessions.remove(self.session.id);
        self.paths.remove();
    }
}

/// 세션 하나를 돌린다. 사용자가 닫으면 경고 없이, 유휴·최대 시간으로 닫히면 그 까닭을 경고로 성공한다. 도우미가 끝나면
/// (커널 패닉·`kill -9`·덮개 한도) 실패다.
pub(super) async fn run(ctx: &Ctx, task: &Task, backup: Uuid) -> Result<Option<String>, String> {
    let api = ctx
        .config
        .api
        .as_ref()
        .ok_or("this node does not run file restore sessions")?;
    let sessions = api.file_restore.clone();
    let id = task.spec.target;
    if sessions.take_closed_early(id) {
        return Ok(Some("closed before it started".into()));
    }
    let node = sessions
        .node
        .clone()
        .ok_or("file restore is not set up on this node")?;
    let image = node.image().ok_or_else(|| {
        node.image_error()
            .unwrap_or_else(|| "no file restore helper image on this node".into())
    })?;
    // 세션 자리는 Task가 끝날 때까지 쥔다(버리면 노드 상태의 세션 수가 준다).
    let _slot = node
        .try_open()
        .ok_or("this node already runs its maximum number of file restore sessions")?;
    let backup = get_backup(ctx, backup)?.ok_or("backup not found")?;
    let opened_by = task
        .labels
        .get(REQUESTED_BY)
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();
    let opened_at = task.id.get_timestamp().map_or(0, |t| t.to_unix().0);
    let session = Arc::new(Session::new(
        id,
        &backup,
        ctx.config.node,
        opened_by,
        opened_at,
        node.limits.idle,
        node.limits.max,
    ));
    let paths = HelperPaths::new(&sessions.run_dir, &sessions.state_dir, id);
    sessions.insert(session.clone());
    let _registered = Registered {
        sessions: sessions.clone(),
        session: session.clone(),
        paths: paths.clone(),
    };
    write_stage(ctx, task.id, STAGE_BOOTING).await;
    paths
        .prepare()
        .map_err(|e| format!("cannot create the session directories: {e}"))?;
    // 저장소 백업의 NBD 서버(버리면 멈춘다) — 도우미보다 먼저 띄우고 나중에 멈춘다.
    let (disks, _serving) = sources(ctx, &backup, &paths).await?;
    let launch = HelperLaunch { id, image, disks };
    tracing::info!(session = %id, backup = %backup.name, "starting a file restore helper");
    let helper = ctx
        .hv
        .start_restore_helper(&node.helper, &launch)
        .await
        .map_err(|e| format!("cannot start the file restore helper: {e}"))?;
    tracing::info!(session = %id, boot_ms = helper.boot_time.as_millis() as u64, "file restore helper is up");
    let volumes = match helper.client.probe().await {
        Ok(v) => v,
        Err(e) => {
            stop_helper(helper).await;
            return Err(format!(
                "the file restore helper could not list the disks: {e}"
            ));
        }
    };
    session.set_ready(
        helper.client.clone(),
        volumes
            .iter()
            .map(|v| volume_view(v, VolumeState::Unmounted))
            .collect(),
    );
    write_stage(ctx, task.id, STAGE_READY).await;
    let end = tokio::select! {
        end = session.wait_close() => end,
        reason = helper.ended() => SessionEnd::Failed(reason),
    };
    session.set_closing();
    write_stage(ctx, task.id, STAGE_CLOSING).await;
    stop_helper(helper).await;
    tracing::info!(session = %id, end = ?end, "file restore session ended");
    match end {
        SessionEnd::Closed(_) => Ok(None),
        SessionEnd::Expired(why) => Ok(Some(why)),
        SessionEnd::Failed(why) => Err(why),
    }
}

/// 세션 단계를 Task 진행률에 적는다(Raft 쓰기는 단계가 바뀔 때만).
async fn write_stage(ctx: &Ctx, task: Uuid, stage: &str) {
    let none = MirrorProgress {
        done_bytes: 0,
        total_bytes: 0,
    };
    write_progress(ctx, task, stage, none).await;
}

/// 백업의 디스크마다 도우미가 읽을 원본(읽기 전용)과 가상 크기. 저장소 백업이면 opld 안 NBD 서버를 세션의 `nbd.sock`에
/// 띄워 함께 돌려준다(버리면 멈춘다).
async fn sources(
    ctx: &Ctx,
    backup: &Backup,
    paths: &HelperPaths,
) -> Result<(Vec<HelperSource>, Option<Serving>), String> {
    if let Some(name) = backup.spec.store.as_deref() {
        let (_, store) = open_store(ctx, name).await?;
        let (m, _) = read_snapshot(&store, backup).await?;
        let socket = paths.nbd_socket();
        let serving = serve(ctx, &store, &m, socket.clone()).await?;
        let disks = m
            .disks
            .iter()
            .map(|d| HelperSource {
                source: LiveRestoreSource::Nbd {
                    socket: socket.clone(),
                    export: d.name.clone(),
                },
                size: d.virtual_bytes,
            })
            .collect();
        return Ok((disks, Some(serving)));
    }
    // 옛 풀 백업(증분이면 체인 전체): 그 파일을 읽는 노드에서만
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
        return Err(format!("backup is stored on node {node}"));
    }
    let (_, root) = backup_root(ctx, &backup.spec.pool)?;
    let dir = backup_dir(&root, backup.id);
    let manifest = read_manifest(&dir).await?;
    let chains = disk_chains(ctx, backup, &root, &dir, &manifest).await?;
    let disks = chains
        .into_iter()
        .zip(&manifest.disks)
        .map(|(chain, d)| HelperSource {
            source: LiveRestoreSource::Qcow2 { chain },
            size: d.virtual_bytes,
        })
        .collect();
    Ok((disks, None))
}
