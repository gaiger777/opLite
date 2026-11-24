//! 파일 복원 세션 Task(계획 69 §10.1): 백업을 일회용 도우미 VM에 붙여 열고, 닫기 요청·유휴·최대 시간까지 기다린 뒤 치운다.
//!
//! 걸음: 노드의 세션 자리 쥐기 → 원본 준비(저장소 백업은 매니페스트·인덱스를 읽어 opld 안 NBD 서버를 세션 디렉터리의
//! `nbd.sock`에 띄움, 옛 풀 백업은 qcow2 체인 확인) → 도우미 띄우기(덮개·QEMU·`Hello`) → `Probe` → 단계 `ready`(볼륨
//! 목록은 세션 등록부 — [`FileRestoreSessions`](crate::file_restore::FileRestoreSessions)) → 기다림 → `stop_helper` → NBD
//! 서버 멈춤 → 세션 디렉터리 지움. 어디서 끝나든(실패·취소·실행기 멈춤) 같은 치우기를 한다. 이어 가지 않는 Task다 —
//! opld가 다시 뜨면 실패로 남고 시작 때 치우기가 남은 도우미를 지운다(§10.5).

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use opl_driver::LiveRestoreSource;
use opl_driver::MirrorProgress;
use opl_driver::file_restore::{
    HelperLaunch, HelperPaths, HelperSource, StreamItem, path_names, stop_helper, volume_view,
};
use opl_restore::archive::PathGuard;
use opl_restore::proto::{EntryKind, Item, Name, Reply};
use opl_types::backup::{Backup, backup_dir};
use opl_types::file_restore::{
    EntryPath, GUEST_WRITE_CHUNK, GuestConflict, GuestDir, GuestFileAnswer, GuestFileOp,
    GuestOsKind, NoFreeName, VolumeState, guest_keep_names, guest_mkdir_command,
    guest_move_command, guest_path_ok, guest_remove_command, guest_temp_name,
};
use opl_types::task::Task;
use opl_types::vm::VmSpec;
use uuid::Uuid;

use super::backup::{backup_root, disk_chains, get_backup, read_manifest};
use super::backup_store::{Serving, open_store, read_snapshot, serve};
use super::migrate::write_progress;
use super::{Ctx, REQUESTED_BY};
use crate::file_restore::{Busy, FileRestoreSessions, Session, SessionEnd};
use crate::state::AppState;

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

// ---------------------------------------------------------------------------------------------------------------
// 게스트로 되돌려 쓰기(계획 69 §12)
// ---------------------------------------------------------------------------------------------------------------

/// 되돌려 쓰기 진행률의 단계(`TaskStatus.progress.stage`)
const STAGE_WRITING: &str = "writing";
/// 결과 문구에 이름을 적는 항목 수(넘으면 `and N more`)
const REPORT_NAMES: usize = 10;

/// 되돌려 쓰기 Task의 요청(`TaskOp::FileRestoreToGuest`)
pub(super) struct ToGuest<'a> {
    pub session: Uuid,
    pub volume: &'a str,
    pub paths: &'a [EntryPath],
    pub target_dir: &'a str,
    pub conflict: GuestConflict,
}

/// 게스트 에이전트에 파일 일을 보내는 쪽: VM이 도는 노드에(이 노드면 그 노드의 게스트 에이전트 처리, 아니면 노드 사이
/// RPC). 일마다 그 노드가 에이전트 잠금을 쥐고 놓는다.
#[derive(Clone)]
struct Guest {
    api: Arc<AppState>,
    vm: Uuid,
    vm_name: String,
    node: Uuid,
    os: GuestOsKind,
    exec: bool,
}

/// 파일 일의 결과: 바깥 `Err`는 에이전트·노드가 사라진 것(Task를 멈춤), 안쪽 `Err`는 이 일만 실패(에이전트가 거절)
type Step<T> = Result<Result<T, String>, String>;

impl Guest {
    async fn call(&self, op: GuestFileOp) -> Result<GuestFileAnswer, String> {
        crate::guest::guest_file(&self.api, self.vm, self.node, op)
            .await
            .map_err(|e| format!("the guest agent of VM {:?}: {e}", self.vm_name))
    }

    async fn exists(&self, path: &str) -> Step<bool> {
        match self.call(GuestFileOp::Exists { path: path.into() }).await? {
            GuestFileAnswer::Exists { exists } => Ok(Ok(exists)),
            other => Err(unexpected(&other)),
        }
    }

    async fn open(&self, path: &str) -> Step<i64> {
        match self.call(GuestFileOp::Open { path: path.into() }).await? {
            GuestFileAnswer::Opened { handle } => Ok(Ok(handle)),
            GuestFileAnswer::Refused { message } => Ok(Err(message)),
            other => Err(unexpected(&other)),
        }
    }

    async fn write(&self, handle: i64, data: &[u8]) -> Step<()> {
        let op = GuestFileOp::Write {
            handle,
            data: STANDARD.encode(data),
        };
        match self.call(op).await? {
            GuestFileAnswer::Written { count } if count == data.len() as u64 => Ok(Ok(())),
            GuestFileAnswer::Written { count } => Ok(Err(format!(
                "the guest wrote {count} of {} bytes",
                data.len()
            ))),
            GuestFileAnswer::Refused { message } => Ok(Err(message)),
            other => Err(unexpected(&other)),
        }
    }

    async fn close(&self, handle: i64) -> Step<()> {
        match self.call(GuestFileOp::Close { handle }).await? {
            GuestFileAnswer::Closed => Ok(Ok(())),
            GuestFileAnswer::Refused { message } => Ok(Err(message)),
            other => Err(unexpected(&other)),
        }
    }

    /// argv 하나를 돌린다(0이 아닌 종료는 안쪽 `Err` — 표준 오류와 함께)
    async fn exec(&self, (path, args): (String, Vec<String>)) -> Step<()> {
        let shown = path.clone();
        match self.call(GuestFileOp::Exec { path, args }).await? {
            GuestFileAnswer::Exited { code: 0, .. } => Ok(Ok(())),
            GuestFileAnswer::Exited { code, stderr } => Ok(Err(if stderr.is_empty() {
                format!("{shown} exited with {code}")
            } else {
                format!("{shown} exited with {code}: {stderr}")
            })),
            GuestFileAnswer::Refused { message } => Ok(Err(message)),
            other => Err(unexpected(&other)),
        }
    }

    async fn mkdir(&self, path: &str) -> Step<()> {
        self.exec(guest_mkdir_command(self.os, path)).await
    }

    /// 다 쓰지 못한 파일을 지운다(`guest-exec`가 없으면 남긴다 — 결과에 적힘). 실패는 경고로만.
    async fn remove(&self, path: &str) -> bool {
        self.exec
            && matches!(
                self.exec(guest_remove_command(self.os, path)).await,
                Ok(Ok(()))
            )
    }
}

fn unexpected(a: &GuestFileAnswer) -> String {
    format!("unexpected answer from the guest agent: {a:?}")
}

/// 지금 게스트에 쓰는 파일. 끝내지 못하고 버려지면(Task가 멈춤) 핸들을 닫고 다 쓰지 못한 파일을 지우는 일을 띄운다.
struct OpenFile {
    guest: Guest,
    handle: i64,
    /// 쓰는 자리(덮어쓰기면 임시 이름)
    write_path: String,
    /// 원래 자리(덮어쓰기면 다 쓴 뒤 바꿔 넣을 이름)
    final_path: String,
    temp: bool,
    /// `keep`이 고른 새 이름이면 원래 이름
    renamed_from: Option<String>,
    remaining: u64,
    size: u64,
    buf: Vec<u8>,
    done: bool,
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let (guest, handle, path) = (self.guest.clone(), self.handle, self.write_path.clone());
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = guest.close(handle).await;
                guest.remove(&path).await;
            });
        }
    }
}

/// 걷기에서 지금 받는 데이터가 갈 곳
enum Sink {
    None,
    /// 건너뛰는 파일의 남은 바이트
    Discard(u64),
    File(Box<OpenFile>),
}

/// 되돌려 쓴 결과(Task 경고·오류 문구)
#[derive(Default)]
struct GuestReport {
    files: u64,
    bytes: u64,
    dirs: u64,
    renamed: Vec<String>,
    skipped: Vec<String>,
    failed: Vec<String>,
    /// 지우지 못하고 남은 다 쓰지 못한 파일
    left: Vec<String>,
}

fn names_list(items: &[String]) -> String {
    let mut shown: Vec<&str> = items
        .iter()
        .take(REPORT_NAMES)
        .map(String::as_str)
        .collect();
    let more = items.len().saturating_sub(REPORT_NAMES);
    let more_text = format!("and {more} more");
    if more > 0 {
        shown.push(&more_text);
    }
    shown.join("; ")
}

impl GuestReport {
    fn summary(&self, vm: &str, dir: &str) -> String {
        let mut parts = vec![format!(
            "restored {} file(s) ({} bytes) and {} director{} into VM {vm}:{dir}",
            self.files,
            self.bytes,
            self.dirs,
            if self.dirs == 1 { "y" } else { "ies" }
        )];
        if !self.renamed.is_empty() {
            parts.push(format!(
                "kept existing files, wrote {}",
                names_list(&self.renamed)
            ));
        }
        if !self.skipped.is_empty() {
            parts.push(format!("skipped {}", names_list(&self.skipped)));
        }
        if !self.failed.is_empty() {
            parts.push(format!("failed {}", names_list(&self.failed)));
        }
        if !self.left.is_empty() {
            parts.push(format!(
                "partly written files were left (no guest-exec to remove them): {}",
                names_list(&self.left)
            ));
        }
        parts.push("owners, modes and times were not restored".into());
        parts.join("; ")
    }
}

/// 세션의 볼륨에서 고른 항목을 걸으며 실행 중인 VM의 게스트 에이전트로 `target_dir` 아래에 쓴다(설계 §12.2). 파일 하나의
/// 실패는 결과에 적고 이어 가며(하나라도 실패하면 Task 실패), 한도(`guest_max_bytes`·`guest_max_files`)를 넘거나 세션·에이전트가
/// 사라지면 그 자리에서 멈추고 실패한다(이미 쓴 파일은 남고 결과에 적힘). 소유자·모드·시각은 되돌리지 않는다.
pub(super) async fn to_guest(
    ctx: &Ctx,
    task: &Task,
    req: ToGuest<'_>,
) -> Result<Option<String>, String> {
    let api = ctx
        .config
        .api
        .clone()
        .ok_or("this node does not run file restore sessions")?;
    let limits = api
        .file_restore
        .node
        .as_ref()
        .map(|n| n.limits)
        .ok_or("file restore is not set up on this node")?;
    let session = api
        .file_restore
        .get(req.session)
        .ok_or("the file restore session is not open on this node")?;
    let vm = ctx
        .cluster
        .get::<VmSpec>(task.spec.target)
        .map_err(|e| e.to_string())?
        .ok_or("VM not found")?;
    let (node, caps) = crate::guest::vm_restore_caps(&api, &vm)
        .await
        .map_err(|e| e.message)?;
    if !caps.file_write {
        return Err(caps
            .reason
            .unwrap_or_else(|| "the guest agent blocks guest-file-*".into()));
    }
    if req.conflict == GuestConflict::Overwrite && !caps.exec {
        return Err("overwrite needs guest-exec, which the guest agent blocks".into());
    }
    let dir = GuestDir::parse(caps.os, req.target_dir)?;
    let guest = Guest {
        api: api.clone(),
        vm: vm.id,
        vm_name: vm.name.clone(),
        node,
        os: caps.os,
        exec: caps.exec,
    };
    let names = req
        .paths
        .iter()
        .map(|p| path_names(p).ok_or_else(|| format!("invalid path {}", p.display())))
        .collect::<Result<Vec<_>, _>>()?;
    let client = session
        .helper_for(req.volume)
        .await
        .map_err(|e| e.message)?;
    // 걷는 동안 세션은 유휴가 아니다.
    let _busy = Busy::new(session.clone());
    if guest.exec {
        guest
            .mkdir(&dir.path())
            .await?
            .map_err(|e| format!("cannot create {} in the guest: {e}", dir.path()))?;
    }
    let mut stream = client
        .walk(req.volume, names, false)
        .await
        .map_err(|e| crate::file_restore::helper_error(req.volume, e).message)?;
    tracing::info!(task = %task.id, vm = %vm.name, dir = %dir.path(), "restoring files into a guest");
    let mut w = Walker {
        guest: &guest,
        dir: &dir,
        task: task.id,
        conflict: req.conflict,
        limits: &limits,
        report: GuestReport::default(),
        guard: PathGuard::new(),
        bad_dirs: Vec::new(),
        sink: Sink::None,
        now: crate::backups::now(),
    };
    let mut last_progress = std::time::Instant::now();
    let stopped = loop {
        let item = match stream.next().await {
            Ok(Some(item)) => item,
            Ok(None) => break None,
            Err(e) => break Some(format!("the file restore helper failed: {e}")),
        };
        if let Err(e) = w.feed(item).await {
            break Some(e);
        }
        if last_progress.elapsed() > std::time::Duration::from_secs(2) {
            last_progress = std::time::Instant::now();
            let p = MirrorProgress {
                done_bytes: w.report.bytes,
                total_bytes: 0,
            };
            write_progress(ctx, task.id, STAGE_WRITING, p).await;
        }
    };
    let stopped = match (stopped, std::mem::replace(&mut w.sink, Sink::None)) {
        (Some(why), Sink::File(f)) => {
            w.abandon(*f, &why).await;
            Some(why)
        }
        (None, Sink::File(f)) => {
            let why = "the helper ended the walk in the middle of a file".to_string();
            w.abandon(*f, &why).await;
            Some(why)
        }
        (stopped, _) => stopped,
    };
    let summary = w.report.summary(&vm.name, &dir.path());
    tracing::info!(task = %task.id, vm = %vm.name, %summary, "restore into the guest ended");
    match stopped {
        Some(why) => Err(format!("{why} — {summary}")),
        None if !w.report.failed.is_empty() => Err(format!(
            "{} item(s) could not be restored — {summary}",
            w.report.failed.len()
        )),
        None => Ok(Some(summary)),
    }
}

/// 걷기 흐름을 게스트 쓰기로 옮기는 상태
struct Walker<'a> {
    guest: &'a Guest,
    dir: &'a GuestDir,
    task: Uuid,
    conflict: GuestConflict,
    limits: &'a opl_driver::file_restore::SessionLimits,
    report: GuestReport,
    guard: PathGuard,
    /// 만들지 못했거나 건너뛴 디렉터리(그 아래 항목은 건너뜀)
    bad_dirs: Vec<Vec<Vec<u8>>>,
    sink: Sink,
    now: u64,
}

impl Walker<'_> {
    /// 걷기 사건 하나. `Err`면 Task를 멈춘다.
    async fn feed(&mut self, item: StreamItem) -> Result<(), String> {
        match item {
            StreamItem::Reply(Reply::Item(item)) => {
                if let Sink::File(f) = std::mem::replace(&mut self.sink, Sink::None) {
                    let why =
                        "the helper sent the next item before the end of the file".to_string();
                    self.abandon(*f, &why).await;
                }
                self.item(item).await
            }
            StreamItem::Data(d) => self.data(d).await,
            StreamItem::Reply(Reply::FileError { message }) => {
                if let Sink::File(f) = std::mem::replace(&mut self.sink, Sink::None) {
                    let why = format!("cannot read it from the backup: {message}");
                    self.abandon(*f, &why).await;
                }
                Ok(())
            }
            StreamItem::Reply(Reply::Skipped { path, reason }) => {
                let shown = path.iter().map(Name::lossy).collect::<Vec<_>>().join("/");
                self.report.skipped.push(format!("{shown} ({reason})"));
                Ok(())
            }
            StreamItem::Reply(other) => Err(format!("unexpected reply from the helper: {other:?}")),
        }
    }

    async fn item(&mut self, item: Item) -> Result<(), String> {
        let raw: Vec<Vec<u8>> = item.path.iter().map(|n| n.as_bytes().to_vec()).collect();
        let shown = item
            .path
            .iter()
            .map(Name::lossy)
            .collect::<Vec<_>>()
            .join("/");
        let is_dir = item.attrs.kind == EntryKind::Dir;
        let data = item.data_len();
        let skip = |this: &mut Self, why: String| {
            this.report.skipped.push(format!("{shown} ({why})"));
            if is_dir {
                this.bad_dirs.push(raw.clone());
            }
            this.sink = if data > 0 {
                Sink::Discard(data)
            } else {
                Sink::None
            };
            Ok(())
        };
        if let Err(why) = self.guard.admit(&raw, is_dir) {
            return skip(self, why.to_string());
        }
        if self
            .bad_dirs
            .iter()
            .any(|d| raw.len() > d.len() && raw.starts_with(d))
        {
            return skip(self, "its directory was not restored".into());
        }
        let mut parts = Vec::with_capacity(raw.len());
        for p in &raw {
            let Ok(name) = std::str::from_utf8(p) else {
                return skip(self, "the name is not UTF-8".into());
            };
            if let Err(e) = guest_path_ok(self.guest.os, name) {
                return skip(self, e.to_string());
            }
            parts.push(name.to_string());
        }
        let target = match self.dir.join(&parts) {
            Ok(t) => t,
            Err(e) => return skip(self, e),
        };
        match item.attrs.kind {
            EntryKind::Dir if !self.guest.exec => skip(
                self,
                "directories need guest-exec, which the agent blocks".into(),
            ),
            EntryKind::Dir => match self.guest.mkdir(&target).await? {
                Ok(()) => {
                    self.report.dirs += 1;
                    Ok(())
                }
                Err(e) => {
                    self.report.failed.push(format!("{shown}/ ({e})"));
                    self.bad_dirs.push(raw);
                    Ok(())
                }
            },
            EntryKind::Symlink => skip(self, "symbolic links are not restored".into()),
            EntryKind::Other => skip(self, "special files are not restored".into()),
            EntryKind::File => self.start_file(&shown, &parts, target, data).await,
        }
    }

    /// 파일 하나를 연다(충돌 정책과 한도).
    async fn start_file(
        &mut self,
        shown: &str,
        parts: &[String],
        target: String,
        size: u64,
    ) -> Result<(), String> {
        if self.report.files + 1 > self.limits.guest_max_files {
            return Err(format!(
                "stopped at {shown}: more than {} files (guest_max_files)",
                self.limits.guest_max_files
            ));
        }
        if self.report.bytes + size > self.limits.guest_max_bytes {
            return Err(format!(
                "stopped at {shown}: more than {} bytes (guest_max_bytes)",
                self.limits.guest_max_bytes
            ));
        }
        let discard = |this: &mut Self| {
            this.sink = if size > 0 {
                Sink::Discard(size)
            } else {
                Sink::None
            };
        };
        let name = parts.last().cloned().unwrap_or_default();
        let parent = &parts[..parts.len().saturating_sub(1)];
        let (write_name, temp, renamed_from) =
            if !self.guest.exists(&target).await?.unwrap_or(false) {
                (name.clone(), false, None)
            } else {
                match self.conflict {
                    GuestConflict::Skip => {
                        self.report.skipped.push(format!("{shown} (exists)"));
                        discard(self);
                        return Ok(());
                    }
                    GuestConflict::Overwrite => (guest_temp_name(&name, self.task), true, None),
                    GuestConflict::Keep => {
                        let mut chosen = None;
                        for candidate in guest_keep_names(&name, self.now) {
                            let mut p = parent.to_vec();
                            p.push(candidate.clone());
                            let path = self.dir.join(&p).map_err(|e| format!("{shown}: {e}"))?;
                            if !self.guest.exists(&path).await?.unwrap_or(true) {
                                chosen = Some(candidate);
                                break;
                            }
                        }
                        match chosen {
                            Some(c) => (c, false, Some(name.clone())),
                            None => {
                                self.report
                                    .failed
                                    .push(format!("{shown} ({})", NoFreeName(name.clone())));
                                discard(self);
                                return Ok(());
                            }
                        }
                    }
                }
            };
        let mut p = parent.to_vec();
        p.push(write_name);
        let write_path = match self.dir.join(&p) {
            Ok(w) => w,
            Err(e) => {
                self.report.skipped.push(format!("{shown} ({e})"));
                discard(self);
                return Ok(());
            }
        };
        let handle = match self.guest.open(&write_path).await? {
            Ok(h) => h,
            Err(e) => {
                self.report.failed.push(format!("{shown} ({e})"));
                discard(self);
                return Ok(());
            }
        };
        let file = OpenFile {
            guest: self.guest.clone(),
            handle,
            write_path,
            final_path: target,
            temp,
            renamed_from,
            remaining: size,
            size,
            buf: Vec::new(),
            done: false,
        };
        if size == 0 {
            return self.finish(Box::new(file)).await;
        }
        self.sink = Sink::File(Box::new(file));
        Ok(())
    }

    async fn data(&mut self, d: Vec<u8>) -> Result<(), String> {
        match std::mem::replace(&mut self.sink, Sink::None) {
            Sink::None => Err("the helper sent data outside a file".into()),
            Sink::Discard(left) => {
                let left = left
                    .checked_sub(d.len() as u64)
                    .ok_or("the helper sent more data than the file size")?;
                if left > 0 {
                    self.sink = Sink::Discard(left);
                }
                Ok(())
            }
            Sink::File(mut f) => {
                f.remaining = f
                    .remaining
                    .checked_sub(d.len() as u64)
                    .ok_or("the helper sent more data than the file size")?;
                f.buf.extend_from_slice(&d);
                while f.buf.len() >= GUEST_WRITE_CHUNK {
                    let chunk: Vec<u8> = f.buf.drain(..GUEST_WRITE_CHUNK).collect();
                    if let Err(e) = self.guest.write(f.handle, &chunk).await? {
                        let left = f.remaining;
                        self.abandon(*f, &e).await;
                        if left > 0 {
                            self.sink = Sink::Discard(left);
                        }
                        return Ok(());
                    }
                }
                if f.remaining == 0 {
                    return self.finish(f).await;
                }
                self.sink = Sink::File(f);
                Ok(())
            }
        }
    }

    /// 남은 버퍼를 쓰고 닫는다. 덮어쓰기면 임시 파일을 원래 이름으로 바꿔 넣는다.
    async fn finish(&mut self, mut f: Box<OpenFile>) -> Result<(), String> {
        if !f.buf.is_empty() {
            let chunk = std::mem::take(&mut f.buf);
            if let Err(e) = self.guest.write(f.handle, &chunk).await? {
                self.abandon(*f, &e).await;
                return Ok(());
            }
        }
        if let Err(e) = self.guest.close(f.handle).await? {
            f.done = true;
            let why = format!("cannot close it: {e}");
            self.failed(&f, &why).await;
            return Ok(());
        }
        f.done = true;
        if f.temp {
            let mv = guest_move_command(self.guest.os, &f.write_path, &f.final_path);
            if let Err(e) = self.guest.exec(mv).await? {
                let why = format!("cannot replace it: {e}");
                self.failed(&f, &why).await;
                return Ok(());
            }
        }
        self.report.files += 1;
        self.report.bytes += f.size;
        if let Some(from) = &f.renamed_from {
            self.report
                .renamed
                .push(format!("{} as {}", from, f.write_path));
        }
        Ok(())
    }

    /// 쓰는 중인 파일을 그만둔다: 닫고 다 쓰지 못한 파일을 지운다.
    async fn abandon(&mut self, mut f: OpenFile, why: &str) {
        f.done = true;
        let _ = self.guest.close(f.handle).await;
        self.failed(&f, why).await;
    }

    async fn failed(&mut self, f: &OpenFile, why: &str) {
        self.report.failed.push(format!("{} ({why})", f.final_path));
        if !self.guest.remove(&f.write_path).await {
            self.report.left.push(f.write_path.clone());
        }
    }
}
