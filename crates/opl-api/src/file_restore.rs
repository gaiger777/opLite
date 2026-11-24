//! 파일 단위 복원 API(계획 69 §10·§11·§13·§14): 백업을 일회용 도우미 VM에 붙인 세션을 열고(Task `file-restore-session`),
//! 볼륨·디렉터리를 보고, 파일 하나(`raw`)나 여러 항목(zip·tar.gz)을 흘려 내려받고, 닫는다.
//!
//! - **세션 등록부**([`FileRestoreSessions`])는 세션 노드의 메모리에 있다: 세션 Task가 도우미를 띄워 넣고, 그 노드의 API와
//!   노드 사이 RPC `FileRestoreCall`·`FileRestoreDownload`([`FileRestoreSource`])가 쓴다. Raft에는 Task 레코드(주인 =
//!   `requested-by` 레이블, 노드, 단계 = `progress.stage`)만 간다.
//! - **권한**: 열기는 백업의 `backup.file_restore`(경로표), 세션 경로는 핸들러가 요청마다 다시 본다 — 그 백업의
//!   `backup.file_restore`와 세션 주인(남의 세션은 루트 `cluster.admin`만, 아니면 없는 것처럼 404).
//! - **내려받기**는 두 걸음이다: `download-link`가 세션 노드 메모리에 60초·한 번만 쓰는 토큰을 만들고(사용자에 묶임),
//!   `GET …/download/{token}`이 같은 사용자의 인증을 다시 보고 흘려 보낸다. 감사는 흐름이 끝날 때 바이트·항목 수와 함께.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use opl_cluster::types::NodeId;
use opl_driver::file_restore::{
    FileRestoreState, HelperClient, HelperError, ReplyStream, StreamItem, decode_cursor,
    encode_cursor, entry_view, path_names,
};
use opl_peer::{
    DownloadHead, DownloadPiece, DownloadStream, FileRestoreAnswer, FileRestoreCallRequest,
    FileRestoreDownloadRequest, FileRestoreFault, FileRestoreOp, FileRestoreReply,
    FileRestoreSource,
};
use opl_restore::archive::tar::TarGzWriter;
use opl_restore::archive::zip::ZipWriter;
use opl_restore::archive::{Archive, Feeder};
use opl_restore::proto::{ErrorCode, Event, Name};
use opl_types::authz::Privilege;
use opl_types::backup::{Backup, BackupPhase, BackupSpec};
use opl_types::file_restore::{
    DownloadFormat, DownloadLink, DownloadLinkRequest, EntryPath, EntryView, FileRestoreOpened,
    FileRestoreSessionView, GuestConflict, GuestDir, ListPage, MAX_DOWNLOAD_NAME,
    MAX_DOWNLOAD_PATHS, MAX_PAGE_ENTRIES, PathPart, RestoreToVmRequest, SessionNodeCandidate,
    SessionPhase, VolumeState, VolumeView, choose_session_node, content_disposition,
    valid_volume_id,
};
use opl_types::node::{FILE_RESTORE, NodeSpec};
use opl_types::storage::{StoragePoolSpec, pool_usable_on};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use tokio::sync::{mpsc, watch};
use utoipa::IntoParams;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::authz_routes::{Obj, resolve};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::lookup;
use crate::state::AppState;
use crate::tasks::{REQUESTED_BY, create_task};

/// 내려받기 토큰의 수명(§11)
pub const LINK_TTL: Duration = Duration::from_secs(60);
/// 나열·항목 RPC의 한도(에이전트의 60초 답 한도 + 마운트 여유)
const CALL_TIMEOUT: Duration = Duration::from_secs(90);
/// 세션 보기 RPC의 한도
const INFO_TIMEOUT: Duration = Duration::from_secs(5);
/// 나열 한 쪽의 기본 항목 수
const DEFAULT_PAGE: u32 = 200;
/// 아카이브 조각을 흘려 보내는 크기(노드 사이 메시지 ≤ 1 MiB)
const CHUNK: usize = 256 << 10;
/// 감사 문구에 넣는 경로 수와 이름 길이(§13)
const AUDIT_PATHS: usize = 5;
const AUDIT_NAME: usize = 200;

// ---------------------------------------------------------------------------------------------------------------
// 세션 등록부(세션 노드 메모리)
// ---------------------------------------------------------------------------------------------------------------

/// 이 노드의 파일 복원 세션. 세션 Task가 넣고 빼며, 이 노드의 API와 노드 사이 RPC가 쓴다.
pub struct FileRestoreSessions {
    /// 이 노드의 도우미 설정·이미지·세션 자리(없으면 이 노드는 세션을 돌리지 않는다 — 위트니스·시험)
    pub node: Option<Arc<FileRestoreState>>,
    /// 하이퍼바이저의 `run_dir`·`state_dir`(세션 디렉터리 `{…}/file-restore/<id>` — 시작 때 치우기와 같은 자리)
    pub run_dir: PathBuf,
    pub state_dir: PathBuf,
    sessions: Mutex<HashMap<Uuid, Arc<Session>>>,
    /// Task가 시작하기 전에 닫기를 받은 세션(Task가 시작하면 곧바로 끝낸다)
    closed_early: Mutex<HashSet<Uuid>>,
}

impl Default for FileRestoreSessions {
    fn default() -> Self {
        Self::new(None, std::env::temp_dir(), std::env::temp_dir())
    }
}

impl FileRestoreSessions {
    pub fn new(node: Option<Arc<FileRestoreState>>, run_dir: PathBuf, state_dir: PathBuf) -> Self {
        FileRestoreSessions {
            node,
            run_dir,
            state_dir,
            sessions: Mutex::default(),
            closed_early: Mutex::default(),
        }
    }

    pub(crate) fn get(&self, id: Uuid) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(&id).cloned()
    }

    pub(crate) fn insert(&self, session: Arc<Session>) {
        self.sessions.lock().unwrap().insert(session.id, session);
    }

    pub(crate) fn remove(&self, id: Uuid) {
        self.sessions.lock().unwrap().remove(&id);
        self.closed_early.lock().unwrap().remove(&id);
    }

    /// Task가 시작하기 전에 닫기를 받았는지(받았으면 지운다)
    pub(crate) fn take_closed_early(&self, id: Uuid) -> bool {
        self.closed_early.lock().unwrap().remove(&id)
    }

    /// 열린 세션 수(시험)
    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    async fn local_call(&self, req: FileRestoreCallRequest) -> Result<FileRestoreAnswer, ApiError> {
        let Some(session) = self.get(req.session) else {
            if let FileRestoreOp::Close { .. } = req.op {
                self.closed_early.lock().unwrap().insert(req.session);
                return Ok(FileRestoreAnswer::Closed);
            }
            return Err(not_ready());
        };
        match req.op {
            FileRestoreOp::Info => Ok(FileRestoreAnswer::Session(Box::new(session.view()))),
            FileRestoreOp::Close { reason } => {
                session.request_close(reason);
                Ok(FileRestoreAnswer::Closed)
            }
            FileRestoreOp::List {
                volume,
                path,
                cursor,
                limit,
            } => session
                .list(&volume, &path, cursor.as_deref(), limit)
                .await
                .map(FileRestoreAnswer::Page),
            FileRestoreOp::Stat { volume, path } => session
                .stat(&volume, &path)
                .await
                .map(FileRestoreAnswer::Entry),
            FileRestoreOp::Link { link } => {
                let token = session.link(req.user, link)?;
                Ok(FileRestoreAnswer::Link {
                    token,
                    expires_in: LINK_TTL.as_secs(),
                })
            }
        }
    }

    async fn local_download(
        &self,
        req: FileRestoreDownloadRequest,
    ) -> Result<(DownloadHead, DownloadStream), ApiError> {
        let session = self.get(req.session).ok_or_else(link_expired)?;
        let link = session.take_link(&req.token, req.user)?;
        session.download(link).await
    }
}

#[async_trait]
impl FileRestoreSource for FileRestoreSessions {
    async fn call(&self, req: FileRestoreCallRequest) -> FileRestoreReply {
        self.local_call(req).await.map_err(fault)
    }

    async fn download(
        &self,
        req: FileRestoreDownloadRequest,
    ) -> Result<(DownloadHead, DownloadStream), FileRestoreFault> {
        self.local_download(req).await.map_err(fault)
    }
}

/// 열린 세션 하나(세션 노드 메모리)
pub(crate) struct Session {
    pub id: Uuid,
    pub backup: Uuid,
    pub backup_name: String,
    pub node: Uuid,
    pub opened_by: Uuid,
    pub opened_at: u64,
    idle: Duration,
    max: Duration,
    started: Instant,
    state: Mutex<SessionState>,
    /// 도는 내려받기 수(그동안은 유휴가 아니다)
    busy: AtomicUsize,
    /// 닫기 요청(까닭)
    close: watch::Sender<Option<String>>,
    links: Mutex<HashMap<String, PendingLink>>,
}

struct SessionState {
    phase: SessionPhase,
    volumes: Vec<VolumeView>,
    client: Option<HelperClient>,
    last_used: Instant,
    last_used_unix: u64,
}

/// 쓰기 전 내려받기 링크
#[derive(Debug)]
pub(crate) struct PendingLink {
    user: Uuid,
    req: DownloadLinkRequest,
    expires: Instant,
}

/// 세션이 끝나는 까닭
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionEnd {
    /// 사용자가 닫음(까닭)
    Closed(String),
    /// 유휴·최대 시간
    Expired(String),
    /// 도우미가 끝남(실패)
    Failed(String),
}

impl Session {
    pub(crate) fn new(
        id: Uuid,
        backup: &Backup,
        node: Uuid,
        opened_by: Uuid,
        opened_at: u64,
        idle: Duration,
        max: Duration,
    ) -> Self {
        Session {
            id,
            backup: backup.id,
            backup_name: backup.name.clone(),
            node,
            opened_by,
            opened_at,
            idle,
            max,
            started: Instant::now(),
            state: Mutex::new(SessionState {
                phase: SessionPhase::Booting,
                volumes: Vec::new(),
                client: None,
                last_used: Instant::now(),
                last_used_unix: crate::backups::now(),
            }),
            busy: AtomicUsize::new(0),
            close: watch::channel(None).0,
            links: Mutex::default(),
        }
    }

    /// 도우미가 탐지를 마쳤다: 볼륨과 채널을 두고 `ready`로
    pub(crate) fn set_ready(&self, client: HelperClient, volumes: Vec<VolumeView>) {
        let mut st = self.state.lock().unwrap();
        st.phase = SessionPhase::Ready;
        st.client = Some(client);
        st.volumes = volumes;
        st.last_used = Instant::now();
        st.last_used_unix = crate::backups::now();
    }

    /// 끝내는 중(더 받지 않는다)
    pub(crate) fn set_closing(&self) {
        let mut st = self.state.lock().unwrap();
        st.phase = SessionPhase::Closed;
        st.client = None;
        self.links.lock().unwrap().clear();
    }

    fn touch(&self) {
        let mut st = self.state.lock().unwrap();
        st.last_used = Instant::now();
        st.last_used_unix = crate::backups::now();
    }

    pub(crate) fn request_close(&self, reason: String) {
        self.close.send_if_modified(|r| {
            if r.is_none() {
                *r = Some(reason);
                true
            } else {
                false
            }
        });
    }

    /// 닫기 요청을 받을 수신기
    pub(crate) fn close_requests(&self) -> watch::Receiver<Option<String>> {
        self.close.subscribe()
    }

    /// 유휴로 닫을 시각(내려받기가 도는 동안은 없음)과 최대 시간으로 닫을 시각
    fn deadlines(&self) -> (Option<Instant>, Instant) {
        let max = self.started + self.max;
        let idle = (self.busy.load(Ordering::Relaxed) == 0)
            .then(|| self.state.lock().unwrap().last_used + self.idle);
        (idle, max)
    }

    /// 닫기 요청·유휴·최대 시간까지 기다린다(도우미가 끝나는 것은 부르는 쪽이 함께 기다린다).
    pub(crate) async fn wait_close(&self) -> SessionEnd {
        let mut close = self.close_requests();
        loop {
            if let Some(reason) = close.borrow_and_update().clone() {
                return SessionEnd::Closed(reason);
            }
            let (idle, max) = self.deadlines();
            let now = Instant::now();
            if now >= max {
                return SessionEnd::Expired(format!(
                    "closed after the maximum session time ({})",
                    human_duration(self.max)
                ));
            }
            if idle.is_some_and(|i| now >= i) {
                return SessionEnd::Expired(format!(
                    "closed after {} without use",
                    human_duration(self.idle)
                ));
            }
            let wake = idle.map_or(max, |i| i.min(max));
            // 쓰면 유휴 시각이 뒤로 가므로 깨어나 다시 계산한다(내려받기가 도는 동안은 몇 초마다 본다).
            let wake = wake.min(now + Duration::from_secs(5));
            tokio::select! {
                _ = close.changed() => {}
                () = tokio::time::sleep_until(wake.into()) => {}
            }
        }
    }

    pub(crate) fn view(&self) -> FileRestoreSessionView {
        let st = self.state.lock().unwrap();
        let max_at = self.opened_at + self.max.as_secs();
        let idle_at = st.last_used_unix + self.idle.as_secs();
        let expires_at = if self.busy.load(Ordering::Relaxed) > 0 {
            max_at
        } else {
            idle_at.min(max_at)
        };
        FileRestoreSessionView {
            id: self.id,
            backup: self.backup,
            backup_name: self.backup_name.clone(),
            node: self.node,
            phase: st.phase,
            error: None,
            opened_by: self.opened_by,
            opened_at: self.opened_at,
            last_used_at: st.last_used_unix,
            expires_at,
            volumes: st.volumes.clone(),
        }
    }

    /// 게스트로 되돌려 쓰기 Task가 쓸 채널(계획 69 §12): 볼륨을 붙이고 채널을 돌려준다. Task는 걷는 동안 [`Busy`]를 쥔다.
    pub(crate) async fn helper_for(&self, volume: &str) -> Result<HelperClient, ApiError> {
        let client = self.client()?;
        self.ensure_mounted(&client, volume).await?;
        Ok(client)
    }

    /// 쓸 수 있는 채널(부팅 중이면 409 `session_not_ready`, 끝났으면 409 `session_closed`)
    fn client(&self) -> Result<HelperClient, ApiError> {
        let st = self.state.lock().unwrap();
        match (st.phase, &st.client) {
            (SessionPhase::Ready, Some(c)) => Ok(c.clone()),
            (SessionPhase::Booting, _) => Err(not_ready()),
            _ => Err(session_closed(None)),
        }
    }

    /// 볼륨 `id`(없으면 404 `volume_not_found`, 찾아볼 수 없으면 409 `volume_not_mountable`)
    fn volume(&self, id: &str) -> Result<VolumeView, ApiError> {
        let st = self.state.lock().unwrap();
        let v = st
            .volumes
            .iter()
            .find(|v| v.id == id)
            .cloned()
            .ok_or_else(|| volume_not_found(id))?;
        if !v.mountable || v.state == VolumeState::Failed {
            let why = v.note.clone().unwrap_or_else(|| match &v.fs {
                Some(fs) => format!("{fs} cannot be browsed"),
                None => "no filesystem found".into(),
            });
            return Err(ApiError::conflict(
                "volume_not_mountable",
                format!("volume {id:?} cannot be browsed: {why}"),
            ));
        }
        Ok(v)
    }

    fn set_volume(&self, id: &str, state: VolumeState, note: Option<String>) {
        let mut st = self.state.lock().unwrap();
        if let Some(v) = st.volumes.iter_mut().find(|v| v.id == id) {
            v.state = state;
            if note.is_some() {
                v.note = note;
            }
        }
    }

    /// 볼륨을 처음 쓸 때 붙인다(`Mounted`·`Failed`와 덧붙임을 볼륨 목록에 적는다).
    async fn ensure_mounted(&self, client: &HelperClient, id: &str) -> Result<(), ApiError> {
        let v = self.volume(id)?;
        if v.state == VolumeState::Mounted {
            return Ok(());
        }
        match client.mount(id).await {
            Ok(note) => {
                self.set_volume(id, VolumeState::Mounted, note);
                Ok(())
            }
            Err(HelperError::Agent(e))
                if matches!(e.code, ErrorCode::NotMountable | ErrorCode::MountFailed) =>
            {
                self.set_volume(id, VolumeState::Failed, Some(e.message.clone()));
                Err(ApiError::conflict(
                    "volume_not_mountable",
                    format!("volume {id:?} cannot be mounted: {}", e.message),
                ))
            }
            Err(e) => Err(helper_error(id, e)),
        }
    }

    async fn list(
        &self,
        volume: &str,
        path: &EntryPath,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ListPage, ApiError> {
        let client = self.client()?;
        let names = names(path)?;
        let cursor = cursor
            .map(|c| decode_cursor(c).ok_or_else(|| ApiError::bad_request("invalid cursor")))
            .transpose()?;
        self.touch();
        self.ensure_mounted(&client, volume).await?;
        let limit = if limit == 0 { DEFAULT_PAGE } else { limit }.min(MAX_PAGE_ENTRIES as u32);
        let (entries, next) = client
            .list(volume, names, cursor, limit)
            .await
            .map_err(|e| helper_error(volume, e))?;
        self.touch();
        Ok(ListPage {
            entries: entries.iter().map(entry_view).collect(),
            next: next.as_ref().map(encode_cursor),
        })
    }

    async fn stat(&self, volume: &str, path: &EntryPath) -> Result<EntryView, ApiError> {
        let client = self.client()?;
        let names = names(path)?;
        self.touch();
        self.ensure_mounted(&client, volume).await?;
        let e = client
            .stat(volume, names)
            .await
            .map_err(|e| helper_error(volume, e))?;
        Ok(entry_view(&e))
    }

    /// 내려받기 토큰을 만든다(`user`에 묶임, [`LINK_TTL`] 안에 한 번)
    fn link(&self, user: Uuid, mut req: DownloadLinkRequest) -> Result<String, ApiError> {
        self.client()?;
        self.volume(&req.volume)?;
        if req.paths.is_empty() || req.paths.len() > MAX_DOWNLOAD_PATHS {
            return Err(invalid_path(format!(
                "choose between 1 and {MAX_DOWNLOAD_PATHS} entries"
            )));
        }
        if req.format == DownloadFormat::Raw && req.paths.len() != 1 {
            return Err(invalid_path("a raw download takes exactly one file"));
        }
        if let Some(name) = &req.name {
            req.name = Some(clean_name(name)?);
        }
        self.touch();
        let token = crate::auth::random_hex(32);
        let mut links = self.links.lock().unwrap();
        let now = Instant::now();
        links.retain(|_, l| l.expires > now);
        links.insert(
            token.clone(),
            PendingLink {
                user,
                req,
                expires: now + LINK_TTL,
            },
        );
        Ok(token)
    }

    /// 토큰을 쓴다: 다른 사용자면 403(토큰은 남김), 없거나 지났으면 410 `link_expired`
    fn take_link(&self, token: &str, user: Uuid) -> Result<PendingLink, ApiError> {
        let mut links = self.links.lock().unwrap();
        let link = links.get(token).ok_or_else(link_expired)?;
        if link.expires <= Instant::now() {
            links.remove(token);
            return Err(link_expired());
        }
        if link.user != user {
            return Err(ApiError::forbidden());
        }
        Ok(links.remove(token).expect("checked above"))
    }

    /// 내려받기를 시작한다: 형식을 정하고 에이전트 흐름을 연 뒤(여기서 난 오류는 HTTP 상태로) 흘려 보낼 작업을 띄운다.
    async fn download(
        self: Arc<Self>,
        link: PendingLink,
    ) -> Result<(DownloadHead, DownloadStream), ApiError> {
        let client = self.client()?;
        let req = link.req;
        self.ensure_mounted(&client, &req.volume).await?;
        let paths = req.paths.iter().map(names).collect::<Result<Vec<_>, _>>()?;
        let format = match req.format {
            DownloadFormat::Auto | DownloadFormat::Raw => {
                let single_file = match paths.as_slice() {
                    [one] if !one.is_empty() => {
                        let e = client
                            .stat(&req.volume, one.clone())
                            .await
                            .map_err(|e| helper_error(&req.volume, e))?;
                        e.attrs.kind == opl_restore::proto::EntryKind::File
                    }
                    _ => false,
                };
                match (req.format, single_file) {
                    (_, true) => DownloadFormat::Raw,
                    (DownloadFormat::Raw, false) => {
                        return Err(invalid_path("a raw download takes one regular file"));
                    }
                    _ => DownloadFormat::Zip,
                }
            }
            f => f,
        };
        let describe = describe(&self.backup_name, &req.volume, &req.paths);
        let name = req
            .name
            .clone()
            .unwrap_or_else(|| default_name(&self.backup_name, &req, format));
        let (tx, rx) = mpsc::channel(4);
        let busy = Busy::new(self.clone());
        let head = match format {
            DownloadFormat::Raw => {
                let (size, stream) = client
                    .read(&req.volume, paths.into_iter().next().unwrap_or_default())
                    .await
                    .map_err(|e| helper_error(&req.volume, e))?;
                tokio::spawn(pump_raw(stream, size, tx, busy));
                DownloadHead {
                    name,
                    content_type: "application/octet-stream".into(),
                    size: Some(size),
                    format: format.as_str().into(),
                    describe,
                }
            }
            DownloadFormat::Zip | DownloadFormat::TarGz => {
                let tar = format == DownloadFormat::TarGz;
                let stream = client
                    .walk(&req.volume, paths, tar)
                    .await
                    .map_err(|e| helper_error(&req.volume, e))?;
                tokio::spawn(pump_archive(stream, tar, tx, busy));
                DownloadHead {
                    name,
                    content_type: if tar {
                        "application/gzip"
                    } else {
                        "application/zip"
                    }
                    .into(),
                    size: None,
                    format: format.as_str().into(),
                    describe,
                }
            }
            DownloadFormat::Auto => unreachable!("resolved above"),
        };
        Ok((head, rx))
    }
}

/// 도는 내려받기·되돌려 쓰기 하나(유휴가 아님). 버리면 마지막 사용 시각을 지금으로 둔다.
pub(crate) struct Busy(Arc<Session>);

impl Busy {
    pub(crate) fn new(session: Arc<Session>) -> Self {
        session.busy.fetch_add(1, Ordering::Relaxed);
        session.touch();
        Busy(session)
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.touch();
        self.0.busy.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 파일 하나를 그대로 흘린다. 선언한 크기와 다르면 흐름을 실패로 끝낸다(raw는 `Content-Length`라 끊김으로 보인다).
async fn pump_raw(
    mut stream: ReplyStream,
    size: u64,
    tx: mpsc::Sender<Result<DownloadPiece, String>>,
    _busy: Busy,
) {
    let mut sent = 0u64;
    loop {
        let piece = match stream.next().await {
            Ok(Some(StreamItem::Data(d))) => {
                sent += d.len() as u64;
                Ok(DownloadPiece::Data(Bytes::from(d)))
            }
            Ok(Some(StreamItem::Reply(_))) => Err("unexpected reply from the helper".to_string()),
            Ok(None) if sent == size => Ok(DownloadPiece::Done { entries: 1 }),
            Ok(None) => Err(format!("the helper sent {sent} of {size} bytes")),
            Err(e) => Err(e.to_string()),
        };
        let end = !matches!(piece, Ok(DownloadPiece::Data(_)));
        // 받는 쪽이 버렸으면 에이전트 흐름을 버린다(취소).
        if tx.send(piece).await.is_err() || end {
            return;
        }
    }
}

/// 아카이브 작성기가 쓰는 버퍼(조각으로 떼어 보낸다)
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedBuf {
    fn take(&self, min: usize) -> Option<Bytes> {
        let mut b = self.0.lock().unwrap();
        (b.len() >= min && !b.is_empty()).then(|| Bytes::from(std::mem::take(&mut *b)))
    }
}

enum Writer {
    Zip(ZipWriter<SharedBuf>),
    Tar(TarGzWriter<SharedBuf>),
}

impl Writer {
    fn feed(&mut self, feeder: &mut Feeder, ev: &Event) -> std::io::Result<()> {
        match self {
            Writer::Zip(w) => feeder.feed(w, ev),
            Writer::Tar(w) => feeder.feed(w, ev),
        }
    }

    fn finish(self) -> std::io::Result<u64> {
        match self {
            Writer::Zip(w) => {
                let n = w.entries();
                w.finish().map(|_| n)
            }
            Writer::Tar(w) => {
                let n = w.entries();
                w.finish().map(|_| n)
            }
        }
    }
}

/// 걷기 흐름을 zip·tar.gz로 묶어 흘린다.
async fn pump_archive(
    mut stream: ReplyStream,
    tar: bool,
    tx: mpsc::Sender<Result<DownloadPiece, String>>,
    busy: Busy,
) {
    let buf = SharedBuf::default();
    let mut w = if tar {
        Writer::Tar(TarGzWriter::new(buf.clone()))
    } else {
        Writer::Zip(ZipWriter::new(buf.clone()))
    };
    let mut feeder = Feeder::new();
    let mut last_touch = Instant::now();
    let failed = loop {
        let ev = match stream.next().await {
            Ok(Some(StreamItem::Reply(r))) => Event::Reply(r),
            Ok(Some(StreamItem::Data(d))) => Event::Data(d),
            Ok(None) => break None,
            Err(e) => break Some(e.to_string()),
        };
        if let Err(e) = w.feed(&mut feeder, &ev) {
            break Some(format!("cannot write the archive: {e}"));
        }
        if let Some(chunk) = buf.take(CHUNK)
            && tx.send(Ok(DownloadPiece::Data(chunk))).await.is_err()
        {
            return;
        }
        if last_touch.elapsed() > Duration::from_secs(10) {
            busy.0.touch();
            last_touch = Instant::now();
        }
    };
    if let Some(e) = failed {
        let _ = tx.send(Err(e)).await;
        return;
    }
    let entries = match w.finish() {
        Ok(n) => n,
        Err(e) => {
            let _ = tx
                .send(Err(format!("cannot finish the archive: {e}")))
                .await;
            return;
        }
    };
    if let Some(chunk) = buf.take(1)
        && tx.send(Ok(DownloadPiece::Data(chunk))).await.is_err()
    {
        return;
    }
    let _ = tx.send(Ok(DownloadPiece::Done { entries })).await;
}

// ---------------------------------------------------------------------------------------------------------------
// 작은 도우미
// ---------------------------------------------------------------------------------------------------------------

fn names(path: &EntryPath) -> Result<Vec<Name>, ApiError> {
    path_names(path).ok_or_else(|| invalid_path(format!("invalid path {}", path.display())))
}

fn invalid_path(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message).with_code("invalid_path")
}

fn not_ready() -> ApiError {
    ApiError::conflict(
        "session_not_ready",
        "the file restore session is still starting; try again when it is ready",
    )
}

fn session_closed(why: Option<&str>) -> ApiError {
    ApiError::conflict(
        "session_closed",
        match why {
            Some(why) => format!("the file restore session has ended: {why}"),
            None => "the file restore session has ended".into(),
        },
    )
}

fn link_expired() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "link_expired",
        "the download link has expired or was already used; ask for a new one",
    )
}

fn volume_not_found(id: &str) -> ApiError {
    ApiError::not_found(format!("volume {id:?} not found in this session"))
        .with_code("volume_not_found")
}

/// 도우미 오류 → API 오류(설계 §14)
pub(crate) fn helper_error(volume: &str, e: HelperError) -> ApiError {
    match e {
        HelperError::Agent(a) => match a.code {
            ErrorCode::NotFound => ApiError::not_found(format!("entry not found: {}", a.message))
                .with_code("entry_not_found"),
            ErrorCode::NotDir | ErrorCode::NotFile | ErrorCode::Symlink | ErrorCode::BadRequest => {
                invalid_path(a.message)
            }
            ErrorCode::NotMountable | ErrorCode::MountFailed => ApiError::conflict(
                "volume_not_mountable",
                format!("volume {volume:?} cannot be mounted: {}", a.message),
            ),
            ErrorCode::Busy => busy(),
            ErrorCode::Io | ErrorCode::Cancelled => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "file_restore_io",
                format!("the backup could not be read: {}", a.message),
            ),
        },
        HelperError::Busy => busy(),
        HelperError::Timeout => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "file_restore_timeout",
            "the file restore helper did not answer within 60 seconds",
        ),
        HelperError::Closed(why) => session_closed(Some(&why)),
    }
}

fn busy() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "file_restore_busy",
        "too many requests are running in this file restore session; try again when one has finished",
    )
}

/// 이 모듈과 세션 노드가 내는 오류 코드(노드 사이에서 되살린다)
const CODES: &[&str] = &[
    "invalid_path",
    "bad_request",
    "session_not_ready",
    "session_closed",
    "link_expired",
    "volume_not_found",
    "entry_not_found",
    "volume_not_mountable",
    "file_restore_busy",
    "file_restore_io",
    "file_restore_timeout",
    "file_restore_unavailable",
    "forbidden",
    "not_found",
    "node_unreachable",
    "cluster_upgrading",
];

fn fault(e: ApiError) -> FileRestoreFault {
    FileRestoreFault::new(e.status.as_u16(), e.code, e.message)
}

fn from_fault(f: FileRestoreFault) -> ApiError {
    let status = StatusCode::from_u16(f.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let code = CODES
        .iter()
        .find(|c| **c == f.code)
        .copied()
        .unwrap_or("file_restore_error");
    ApiError::new(status, code, f.message)
}

/// 감사·로그에 넣는 이름(제어 문자 이스케이프, [`AUDIT_NAME`]자)
fn audit_text(s: &str) -> String {
    let escaped: String = s.chars().flat_map(char::escape_debug).collect();
    let escaped = escaped.replace("\\'", "'").replace("\\\"", "\"");
    if escaped.chars().count() > AUDIT_NAME {
        let mut cut: String = escaped.chars().take(AUDIT_NAME).collect();
        cut.push('…');
        cut
    } else {
        escaped
    }
}

/// `<백업> <볼륨>:<경로>, … and N more`(감사 문구)
fn describe(backup: &str, volume: &str, paths: &[EntryPath]) -> String {
    let mut shown: Vec<String> = paths
        .iter()
        .take(AUDIT_PATHS)
        .map(|p| format!("{volume}:{}", audit_text(&p.display())))
        .collect();
    if paths.len() > AUDIT_PATHS {
        shown.push(format!("and {} more", paths.len() - AUDIT_PATHS));
    }
    format!("{} {}", audit_text(backup), shown.join(", "))
}

/// 파일 이름으로 쓸 수 있게(`/`·`\`·제어 문자는 `_`)
fn safe_file_name(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

/// 요청의 이름(1~255바이트, 경로·제어 문자 없음)
fn clean_name(name: &str) -> Result<String, ApiError> {
    if name.is_empty()
        || name.len() > MAX_DOWNLOAD_NAME
        || name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
        || name == "."
        || name == ".."
    {
        return Err(ApiError::bad_request(format!(
            "invalid download name {name:?} (1-{MAX_DOWNLOAD_NAME} bytes, no path or control characters)"
        )));
    }
    Ok(name.to_string())
}

/// 기본 이름: 파일 하나면 그 이름, 아니면 `<백업>-<마지막 조각 또는 볼륨>.zip|tar.gz`
fn default_name(backup: &str, req: &DownloadLinkRequest, format: DownloadFormat) -> String {
    let last = match req.paths.as_slice() {
        [one] => one.last().map(|p| p.lossy()),
        _ => None,
    };
    let name = match (format, last) {
        (DownloadFormat::Raw, Some(last)) => return safe_file_name(&last),
        (_, Some(last)) => format!("{backup}-{last}"),
        (_, None) => format!("{backup}-{}", req.volume),
    };
    let ext = match format {
        DownloadFormat::TarGz => "tar.gz",
        _ => "zip",
    };
    format!("{}.{ext}", safe_file_name(&name))
}

/// `1 h`, `15 min`, `30 s`
fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 && s.is_multiple_of(3600) {
        format!("{} h", s / 3600)
    } else if s >= 60 && s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{} s", s.max(1))
    }
}

/// 나열·항목 경로: `path`(UTF-8 문자열) 또는 `path_b64`(URL-safe base64 — 조각 바이트를 `/`로 이은 것)
fn query_path(path: Option<&str>, path_b64: Option<&str>) -> Result<EntryPath, ApiError> {
    match (path, path_b64) {
        (Some(_), Some(_)) => Err(invalid_path("give path or path_b64, not both")),
        (Some(p), None) => EntryPath::parse(p).map_err(|e| invalid_path(e.to_string())),
        (None, Some(b)) => {
            let raw = URL_SAFE_NO_PAD
                .decode(b.trim_end_matches('='))
                .map_err(|_| invalid_path("path_b64 is not base64url"))?;
            let rest = raw.strip_prefix(b"/").unwrap_or(&raw);
            if rest.is_empty() {
                return Ok(EntryPath::root());
            }
            let parts = rest
                .split(|b| *b == b'/')
                .map(|p| PathPart::new(p.to_vec()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| invalid_path(e.to_string()))?;
            EntryPath::from_parts(parts).map_err(|e| invalid_path(e.to_string()))
        }
        (None, None) => Ok(EntryPath::root()),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Raft에서 본 세션(아무 노드)
// ---------------------------------------------------------------------------------------------------------------

/// 세션 Task의 대상(세션 id)과 백업
fn session_of(task: &Task) -> Option<Uuid> {
    matches!(task.spec.op, TaskOp::FileRestoreSession { .. }).then_some(task.spec.target)
}

fn backup_of(task: &Task) -> Uuid {
    match task.spec.op {
        TaskOp::FileRestoreSession { backup } => backup,
        _ => Uuid::nil(),
    }
}

/// 세션을 연 사용자(`requested-by` 레이블)
fn owner_of(task: &Task) -> Option<Uuid> {
    task.labels.get(REQUESTED_BY).and_then(|s| s.parse().ok())
}

/// 세션 Task가 닫히는 중인지(단계 `closing`)
fn closing(task: &Task) -> bool {
    task.status
        .progress
        .as_ref()
        .is_some_and(|p| p.stage == crate::tasks::file_restore::STAGE_CLOSING)
}

/// Task id(UUID v7)의 시각(유닉스 초)
fn task_time(task: &Task) -> u64 {
    task.id.get_timestamp().map_or(0, |t| t.to_unix().0)
}

/// 노드 메모리 없이 Raft의 Task로 만든 보기(부팅 중·끝난 세션, 닿지 않는 노드). 볼륨은 없고 `expires_at`은 모르면 0이다.
fn raft_view(state: &AppState, task: &Task) -> FileRestoreSessionView {
    let backup = backup_of(task);
    let backup_name = state
        .cluster
        .get::<BackupSpec>(backup)
        .ok()
        .flatten()
        .map_or_else(|| backup.to_string(), |b| b.name);
    let phase = match task.status.state {
        TaskState::Succeeded => SessionPhase::Closed,
        TaskState::Failed => SessionPhase::Failed,
        _ => match task.status.progress.as_ref().map(|p| p.stage.as_str()) {
            Some(crate::tasks::file_restore::STAGE_READY) => SessionPhase::Ready,
            Some(crate::tasks::file_restore::STAGE_CLOSING) => SessionPhase::Closed,
            _ => SessionPhase::Booting,
        },
    };
    let opened_at = task_time(task);
    FileRestoreSessionView {
        id: task.spec.target,
        backup,
        backup_name,
        node: task.spec.node,
        phase,
        error: task.status.error.clone(),
        opened_by: owner_of(task).unwrap_or_default(),
        opened_at,
        last_used_at: opened_at,
        expires_at: 0,
        volumes: Vec::new(),
    }
}

fn session_tasks(state: &AppState) -> Result<Vec<Task>, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks
        .into_iter()
        .filter(|t| session_of(t).is_some())
        .collect())
}

/// 백업 `id`에 열린(끝나지 않은) 세션 Task
pub(crate) fn open_sessions(state: &AppState, id: Uuid) -> Result<Vec<Task>, ApiError> {
    Ok(session_tasks(state)?
        .into_iter()
        .filter(|t| !t.status.state.is_finished() && backup_of(t) == id)
        .collect())
}

fn is_admin(state: &AppState, principal: &Principal) -> bool {
    state
        .authz
        .require_root(principal, Privilege::ClusterAdmin)
        .is_ok()
}

/// 세션 경로의 세션 Task와 권한: 없거나 남의 세션(admin이 아니면)이면 404, 백업의 `backup.file_restore`가 없으면 403/404
fn find_session(state: &AppState, principal: &Principal, id: Uuid) -> Result<Task, ApiError> {
    require_feature(&state.cluster, FILE_RESTORE)?;
    let missing = || {
        ApiError::not_found(format!("file restore session {id} not found"))
            .with_code("session_not_found")
    };
    let task = session_tasks(state)?
        .into_iter()
        .filter(|t| t.spec.target == id)
        .max_by_key(|t| t.id)
        .ok_or_else(missing)?;
    let admin = is_admin(state, principal);
    if owner_of(&task) != Some(principal.user) && !admin {
        return Err(missing());
    }
    // 열린 세션의 요청마다 권한을 다시 본다(§13 — 권한을 빼면 다음 요청부터 403).
    match resolve(&state.cluster, Obj::Backup, &backup_of(&task).to_string())? {
        Some(found) => state
            .authz
            .require(principal, Privilege::BackupFileRestore, &found)?,
        None if admin => {}
        None => return Err(missing()),
    }
    Ok(task)
}

/// 세션 노드에 묻는다: 이 노드면 등록부, 아니면 RPC `FileRestoreCall`
async fn call(
    state: &AppState,
    task: &Task,
    user: Uuid,
    op: FileRestoreOp,
    timeout: Duration,
) -> Result<FileRestoreAnswer, ApiError> {
    let req = FileRestoreCallRequest {
        session: task.spec.target,
        user,
        op,
    };
    let node = task.spec.node;
    if node == state.node {
        return state.file_restore.local_call(req).await;
    }
    let (peers, addr) = peer_of(state, node)?;
    peers
        .file_restore_call(NodeId(node), &addr, req, timeout)
        .await
        .map_err(from_fault)
}

fn peer_of(state: &AppState, node: Uuid) -> Result<(Arc<opl_peer::Peers>, String), ApiError> {
    let unreachable = |why: &str| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("the file restore session node did not answer: {why}"),
        )
    };
    if state
        .cluster
        .get::<NodeSpec>(node)?
        .is_some_and(|n| !n.status.online)
    {
        return Err(unreachable("the node is offline"));
    }
    let peers = state
        .peers
        .clone()
        .ok_or_else(|| unreachable("no node-to-node connection"))?;
    let addr = state
        .cluster
        .members()
        .addr(NodeId(node))
        .map(str::to_string)
        .ok_or_else(|| unreachable("not a cluster member"))?;
    Ok((peers, addr))
}

/// 세션 보기: 열린 세션은 세션 노드에 묻고(부팅 전·닿지 않으면 Raft 보기), 끝난 세션은 Raft 보기
async fn view_of(state: &AppState, task: &Task, user: Uuid) -> FileRestoreSessionView {
    if task.status.state.is_finished() {
        return raft_view(state, task);
    }
    match call(state, task, user, FileRestoreOp::Info, INFO_TIMEOUT).await {
        Ok(FileRestoreAnswer::Session(v)) => *v,
        _ => raft_view(state, task),
    }
}

/// 열린 세션을 닫는다(끝난 세션은 그대로). 백업 지우기 `force`도 쓴다.
pub(crate) async fn close_session(
    state: &AppState,
    task: &Task,
    user: Uuid,
    reason: &str,
) -> Result<(), ApiError> {
    if task.status.state.is_finished() {
        return Ok(());
    }
    let op = FileRestoreOp::Close {
        reason: reason.into(),
    };
    call(state, task, user, op, INFO_TIMEOUT).await.map(|_| ())
}

/// 노드 `node`에서 열린 세션을 모두 닫는다(노드 비우기 — 설계 §13; 비우는 노드에는 새 세션을 열지 않는다). 닫기를 보낸
/// 수다. 세션 노드가 답하지 않으면 건너뛴다(그 세션은 유휴·최대 시간으로 닫힌다).
pub(crate) async fn close_node_sessions(state: &AppState, node: Uuid, reason: &str) -> usize {
    let Ok(tasks) = session_tasks(state) else {
        return 0;
    };
    let mut closed = 0;
    for task in tasks
        .iter()
        .filter(|t| t.spec.node == node && !t.status.state.is_finished())
    {
        match close_session(state, task, Uuid::nil(), reason).await {
            Ok(()) => closed += 1,
            Err(e) => tracing::warn!(session = %task.spec.target, error = %e.message,
                "cannot close a file restore session on a draining node"),
        }
    }
    closed
}

// ---------------------------------------------------------------------------------------------------------------
// 세션 노드 고르기(§10.2)
// ---------------------------------------------------------------------------------------------------------------

/// 노드를 고르는 판단
type NodeFilter = Box<dyn Fn(&opl_types::node::Node) -> bool>;

/// 백업을 읽을 수 있는 노드인지 판단하는 함수
fn backup_reader(state: &AppState, backup: &Backup) -> Result<NodeFilter, ApiError> {
    if let Some(name) = &backup.spec.store {
        let store = crate::backups::find_store(state, name)?;
        crate::backups::check_store(&store, false)?;
        let pool =
            crate::stores::backend_pool(&state.cluster, &store).map_err(ApiError::bad_request)?;
        return Ok(Box::new(move |n| {
            crate::stores::usable_on(&store, pool.as_ref(), n)
        }));
    }
    let pool = state
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)?
        .ok_or_else(|| {
            ApiError::conflict(
                "file_restore_unavailable",
                format!(
                    "storage pool {:?} of backup {:?} no longer exists",
                    backup.spec.pool, backup.name
                ),
            )
        })?;
    let stored = backup.status.node;
    Ok(Box::new(move |n| {
        pool_usable_on(&pool, n) && stored.is_none_or(|s| s == n.id)
    }))
}

fn choose_node(state: &AppState, backup: &Backup) -> Result<Uuid, ApiError> {
    let reads = backup_reader(state, backup)?;
    let (_, nodes) = state.cluster.list::<NodeSpec>()?;
    let candidates: Vec<SessionNodeCandidate<'_>> = nodes
        .iter()
        .map(|n| SessionNodeCandidate {
            id: n.id,
            name: &n.name,
            // 비우는(유지보수) 노드에는 열지 않는다(비우기가 그 노드의 세션을 닫는다).
            online: n.status.online && !n.spec.witness && !n.spec.maintenance,
            reads_backup: reads(n),
            file_restore: n.status.file_restore.as_ref(),
        })
        .collect();
    choose_session_node(&candidates, state.node)
        .map_err(|e| ApiError::conflict(e.code(), e.to_string()))
}

// ---------------------------------------------------------------------------------------------------------------
// 경로
// ---------------------------------------------------------------------------------------------------------------

/// 백업 `key`의 파일 복원 세션을 연다(설계 §10.1). 같은 사용자(토큰이면 토큰 주인)의 같은 백업 세션이 열려 있으면 그것을
/// 돌려준다(200), 아니면 도우미 이미지가 있고 백업을 읽을 수 있는 노드(이 노드 먼저 → 세션이 적은 노드 → KVM·HVF)에
/// Task `file-restore-session`을 만든다(202 — 단계는 세션 보기나 Task로 본다). 백업의 `backup.file_restore`가 든다.
/// 모든 노드가 `file-restore`를 알아야 하고(409 `cluster_upgrading`), 백업이 `ready`가 아니면 409 `backup_not_ready`,
/// 지우는 중이면 409 `backup_busy`, 열 노드가 없으면 409 `file_restore_unavailable`(이미지 없음)·`file_restore_busy`(모두
/// 꽉 참)다.
#[utoipa::path(post, path = "/backups/{key}/file-restore", tag = "file-restore",
    params(("key" = String, Path, description = "backup id or name")),
    responses(
        (status = 202, body = FileRestoreOpened, description = "a new session is starting"),
        (status = 200, body = FileRestoreOpened, description = "your open session of this backup"),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn open_session(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<FileRestoreOpened>), ApiError> {
    require_feature(&state.cluster, FILE_RESTORE)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    if backup.status.phase != BackupPhase::Ready {
        return Err(ApiError::conflict(
            "backup_not_ready",
            format!("backup {:?} is not ready", backup.name),
        ));
    }
    crate::backups::check_not_deleting(&state, &backup)?;
    if let Some(task) = open_sessions(&state, backup.id)?
        .into_iter()
        // 닫히는 중인 세션은 다시 쓰지 않는다.
        .filter(|t| owner_of(t) == Some(principal.user) && !closing(t))
        .max_by_key(|t| t.id)
    {
        let session = view_of(&state, &task, principal.user).await;
        return Ok((
            StatusCode::OK,
            Json(FileRestoreOpened {
                session,
                task: task.id,
            }),
        ));
    }
    let node = choose_node(&state, &backup)?;
    let session = Uuid::now_v7();
    let node_name = crate::backups::node_name(&state, node)?;
    audit.detail(format!(
        "backup file-restore open {} (session {}, node {node_name})",
        audit_text(&backup.name),
        &session.simple().to_string()[..8],
    ));
    let task = create_task(
        &state.cluster,
        TaskOp::FileRestoreSession { backup: backup.id },
        session,
        node,
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(FileRestoreOpened {
            session: raft_view(&state, &task),
            task: task.id,
        }),
    ))
}

/// 세션 목록의 조건
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct SessionListParams {
    /// 모든 사용자의 세션(루트 `cluster.admin`)
    #[serde(default)]
    pub all: bool,
    /// 이 백업(id나 이름)의 세션만
    pub backup: Option<String>,
}

/// 열린 파일 복원 세션(자기 것, `all`이면 모두 — 루트 `cluster.admin`). 세션 노드에 물어 볼륨·만료 시각을 채운다(닿지
/// 않으면 Raft의 단계만).
#[utoipa::path(get, path = "/file-restore/sessions", tag = "file-restore",
    params(SessionListParams),
    responses((status = 200, body = Vec<FileRestoreSessionView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(params): Query<SessionListParams>,
) -> Result<Json<Vec<FileRestoreSessionView>>, ApiError> {
    require_feature(&state.cluster, FILE_RESTORE)?;
    if params.all && !is_admin(&state, &principal) {
        return Err(ApiError::forbidden());
    }
    let backup = params
        .backup
        .as_deref()
        .map(|k| lookup::<BackupSpec>(&state.cluster, k).map(|b| b.id))
        .transpose()?;
    let mut out = Vec::new();
    for task in session_tasks(&state)? {
        if task.status.state.is_finished()
            || (!params.all && owner_of(&task) != Some(principal.user))
            || backup.is_some_and(|b| b != backup_of(&task))
        {
            continue;
        }
        out.push(view_of(&state, &task, principal.user).await);
    }
    out.sort_by_key(|v| (v.opened_at, v.id));
    Ok(Json(out))
}

/// 세션 하나(부팅 중이면 `volumes`가 비어 있다). 끝난 세션은 `closed`·`failed`와 까닭.
#[utoipa::path(get, path = "/file-restore/sessions/{id}", tag = "file-restore",
    params(("id" = Uuid, Path, description = "session id")),
    responses((status = 200, body = FileRestoreSessionView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Json<FileRestoreSessionView>, ApiError> {
    let task = find_session(&state, &principal, id)?;
    Ok(Json(view_of(&state, &task, principal.user).await))
}

/// 세션을 닫는다(204 — Task가 도우미를 끝내고 치운다). 끝난 세션도 204.
#[utoipa::path(delete, path = "/file-restore/sessions/{id}", tag = "file-restore",
    params(("id" = Uuid, Path, description = "session id")),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn close_session_route(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let task = find_session(&state, &principal, id)?;
    let view = raft_view(&state, &task);
    audit.detail(format!(
        "backup file-restore close {} (session {})",
        audit_text(&view.backup_name),
        &id.simple().to_string()[..8]
    ));
    close_session(
        &state,
        &task,
        principal.user,
        &format!("closed by {}", principal.name),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 나열·항목의 경로 조건
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct EntriesParams {
    /// 디렉터리 경로(`/etc/nginx`, UTF-8 이름만 — 없으면 볼륨 루트)
    pub path: Option<String>,
    /// UTF-8이 아닌 이름이 든 경로: 조각 바이트를 `/`로 이은 것의 URL-safe base64
    pub path_b64: Option<String>,
    /// 앞 쪽의 `next`
    pub cursor: Option<String>,
    /// 한 쪽 항목 수(1~1000, 기본 200)
    pub limit: Option<u32>,
}

/// 디렉터리 한 쪽(이름 바이트 차례). 볼륨을 처음 보면 붙인다(저널이 재생되지 않았으면 볼륨 `note`). 볼륨이 없으면 404
/// `volume_not_found`, 항목이 없으면 404 `entry_not_found`, 디렉터리가 아니면 400 `invalid_path`, 찾아볼 수 없는 볼륨은 409
/// `volume_not_mountable`, 부팅 중이면 409 `session_not_ready`, 동시 요청이 많으면 429 `file_restore_busy`, 도우미가 60초 안에
/// 답하지 않으면 504 `file_restore_timeout`.
#[utoipa::path(get, path = "/file-restore/sessions/{id}/volumes/{volume}/entries", tag = "file-restore",
    params(("id" = Uuid, Path, description = "session id"),
        ("volume" = String, Path, description = "volume id (`disk0p1`, `lvm/vg/lv` — percent-encode `/`)"),
        EntriesParams),
    responses((status = 200, body = ListPage), (status = "4XX", body = crate::error::ErrorBody),
        (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn list_entries(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((id, volume)): Path<(Uuid, String)>,
    Query(q): Query<EntriesParams>,
) -> Result<Json<ListPage>, ApiError> {
    let task = find_session(&state, &principal, id)?;
    check_volume(&volume)?;
    let path = query_path(q.path.as_deref(), q.path_b64.as_deref())?;
    let op = FileRestoreOp::List {
        volume,
        path,
        cursor: q.cursor,
        limit: q.limit.unwrap_or(DEFAULT_PAGE),
    };
    match open_call(&state, &task, principal.user, op).await? {
        FileRestoreAnswer::Page(p) => Ok(Json(p)),
        _ => Err(ApiError::internal(
            "unexpected answer from the session node",
        )),
    }
}

/// 항목 조건
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct StatParams {
    pub path: Option<String>,
    pub path_b64: Option<String>,
}

/// 항목 하나(링크는 따라가지 않음). 오류는 나열과 같다.
#[utoipa::path(get, path = "/file-restore/sessions/{id}/volumes/{volume}/stat", tag = "file-restore",
    params(("id" = Uuid, Path, description = "session id"),
        ("volume" = String, Path, description = "volume id"), StatParams),
    responses((status = 200, body = EntryView), (status = "4XX", body = crate::error::ErrorBody),
        (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn stat_entry(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((id, volume)): Path<(Uuid, String)>,
    Query(q): Query<StatParams>,
) -> Result<Json<EntryView>, ApiError> {
    let task = find_session(&state, &principal, id)?;
    check_volume(&volume)?;
    let path = query_path(q.path.as_deref(), q.path_b64.as_deref())?;
    match open_call(
        &state,
        &task,
        principal.user,
        FileRestoreOp::Stat { volume, path },
    )
    .await?
    {
        FileRestoreAnswer::Entry(e) => Ok(Json(e)),
        _ => Err(ApiError::internal(
            "unexpected answer from the session node",
        )),
    }
}

fn check_volume(volume: &str) -> Result<(), ApiError> {
    if !valid_volume_id(volume) {
        return Err(volume_not_found(volume));
    }
    Ok(())
}

/// 열린 세션에만 묻는다(끝났으면 409 `session_closed`)
async fn open_call(
    state: &AppState,
    task: &Task,
    user: Uuid,
    op: FileRestoreOp,
) -> Result<FileRestoreAnswer, ApiError> {
    if task.status.state.is_finished() {
        return Err(session_closed(task.status.error.as_deref()));
    }
    call(state, task, user, op, CALL_TIMEOUT).await
}

/// 내려받기 링크(§11): 세션 노드 메모리에 60초·한 번만 쓰는 토큰을 만든다(요청한 사용자에 묶임). 브라우저·CLI가
/// `GET url`로 받는다(같은 사용자의 인증이 다시 필요하다). 항목은 1~1000개, `raw`는 일반 파일 하나만(400 `invalid_path`).
#[utoipa::path(post, path = "/file-restore/sessions/{id}/download-link", tag = "file-restore",
    params(("id" = Uuid, Path, description = "session id")),
    request_body = DownloadLinkRequest,
    responses((status = 200, body = DownloadLink), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn download_link(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<Uuid>,
    Json(req): Json<DownloadLinkRequest>,
) -> Result<Json<DownloadLink>, ApiError> {
    let task = find_session(&state, &principal, id)?;
    check_volume(&req.volume)?;
    let backup_name = raft_view(&state, &task).backup_name;
    audit.detail(format!(
        "backup file-restore download-link {} ({})",
        describe(&backup_name, &req.volume, &req.paths),
        req.format.as_str()
    ));
    match open_call(
        &state,
        &task,
        principal.user,
        FileRestoreOp::Link { link: req },
    )
    .await?
    {
        FileRestoreAnswer::Link { token, expires_in } => Ok(Json(DownloadLink {
            url: format!("/file-restore/sessions/{id}/download/{token}"),
            expires_in,
        })),
        _ => Err(ApiError::internal(
            "unexpected answer from the session node",
        )),
    }
}

/// 링크로 내려받는다(GET — 브라우저 내려받기). 링크를 만든 사용자만(403), 지났거나 쓴 링크는 410 `link_expired`. `raw`는
/// `Content-Length`와 `application/octet-stream`, zip·tar.gz는 길이 없이 흘린다. 도중의 읽기 실패는 연결을 끊는다(받은
/// 파일이 잘렸음을 클라이언트가 안다). 감사는 흐름이 끝날 때 바이트·항목 수와 함께 적는다.
#[utoipa::path(get, path = "/file-restore/sessions/{id}/download/{token}", tag = "file-restore",
    params(("id" = Uuid, Path, description = "session id"), ("token" = String, Path)),
    responses((status = 200, content_type = "application/octet-stream", description = "the file or archive"),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn download(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    crate::limit::ClientIp(ip): crate::limit::ClientIp,
    Path((id, token)): Path<(Uuid, String)>,
) -> Result<Response, ApiError> {
    let started = Instant::now();
    let task = find_session(&state, &principal, id)?;
    if task.status.state.is_finished() {
        return Err(session_closed(task.status.error.as_deref()));
    }
    let req = FileRestoreDownloadRequest {
        session: id,
        token,
        user: principal.user,
    };
    let node = task.spec.node;
    let (head, rx) = if node == state.node {
        state.file_restore.local_download(req).await?
    } else {
        let (peers, addr) = peer_of(&state, node)?;
        peers
            .file_restore_download(NodeId(node), &addr, req, CALL_TIMEOUT)
            .await
            .map_err(from_fault)?
    };
    let audit = DownloadAudit {
        state: state.clone(),
        principal,
        ip,
        path: format!("/file-restore/sessions/{id}/download/-"),
        started,
        detail: format!(
            "backup file-restore download {} ({}",
            head.describe, head.format
        ),
        bytes: 0,
        entries: None,
        failed: None,
    };
    let body = futures_util::stream::unfold((rx, audit), |(mut rx, mut audit)| async move {
        match rx.recv().await {
            Some(Ok(DownloadPiece::Data(d))) => {
                audit.bytes += d.len() as u64;
                Some((Ok::<_, std::io::Error>(d), (rx, audit)))
            }
            Some(Ok(DownloadPiece::Done { entries })) => {
                audit.done(entries);
                None
            }
            Some(Err(e)) => {
                audit.failed = Some(e.clone());
                Some((Err(std::io::Error::other(e)), (rx, audit)))
            }
            None => None,
        }
    });
    let mut resp = Body::from_stream(body).into_response();
    let headers = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&head.content_type) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&content_disposition(&head.name)) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(size) = head.size {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

/// 게스트로 되돌려 쓰기(설계 §12): 세션의 볼륨에서 고른 항목을 실행 중인 VM의 게스트 에이전트로 `target_dir` 아래에 쓰는
/// Task `file-restore-to-guest`를 만든다(202 — 대상은 VM, 세션 노드에서 돈다). 세션의 권한(위 경로들과 같음)에 더해 대상 VM의
/// `vm.config`가 든다. 항목은 1~1000개, 대상 디렉터리는 절대 경로(Linux `/…`, Windows `C:\\…`)여야 하고(400
/// `invalid_target_dir`), VM이 돌지 않으면 409 `vm_not_running`, 채널이 없거나 에이전트가 답하지 않으면 409
/// `guest_agent_unavailable`, 에이전트가 `guest-file-*`를 막으면 409 `guest_agent_restricted`(RHEL·Rocky 기본), `guest-exec`가
/// 막혔는데 디렉터리를 고르면 400 `guest_mkdir_unavailable`, `overwrite`를 고르면 400 `guest_overwrite_unavailable`이다.
/// 소유자·모드·시각은 되돌리지 않는다(Task 경고 — 권한까지는 tar.gz 내려받기).
#[utoipa::path(post, path = "/file-restore/sessions/{id}/restore-to-vm", tag = "file-restore",
    params(("id" = Uuid, Path, description = "session id")),
    request_body = RestoreToVmRequest,
    responses((status = 202, body = opl_types::resource::Resource<TaskSpec, opl_types::task::TaskStatus>),
        (status = "4XX", body = crate::error::ErrorBody), (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn restore_to_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<Uuid>,
    Json(req): Json<RestoreToVmRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let session = find_session(&state, &principal, id)?;
    if session.status.state.is_finished() {
        return Err(session_closed(session.status.error.as_deref()));
    }
    check_volume(&req.volume)?;
    if req.paths.is_empty() || req.paths.len() > MAX_DOWNLOAD_PATHS {
        return Err(invalid_path(format!(
            "choose between 1 and {MAX_DOWNLOAD_PATHS} entries"
        )));
    }
    let found = resolve(&state.cluster, Obj::Vm, &req.vm)?
        .ok_or_else(|| ApiError::not_found(format!("vm {:?} not found", req.vm)))?;
    state
        .authz
        .require(&principal, Privilege::VmConfig, &found)?;
    let vm = lookup::<VmSpec>(&state.cluster, &req.vm)?;
    let (_, caps) = crate::guest::vm_restore_caps(&state, &vm).await?;
    if !caps.file_write {
        return Err(ApiError::conflict(
            "guest_agent_restricted",
            format!(
                "files cannot be restored into VM {:?}: {}",
                vm.name,
                caps.reason
                    .as_deref()
                    .unwrap_or("the guest agent blocks guest-file-*")
            ),
        ));
    }
    GuestDir::parse(caps.os, &req.target_dir).map_err(|e| {
        ApiError::bad_request(format!("invalid target directory: {e}"))
            .with_code("invalid_target_dir")
    })?;
    if !caps.exec {
        if req.conflict == GuestConflict::Overwrite {
            return Err(ApiError::bad_request(format!(
                "overwrite needs guest-exec, which the guest agent of VM {:?} blocks; choose keep or skip",
                vm.name
            ))
            .with_code("guest_overwrite_unavailable"));
        }
        // 디렉터리를 만들 수 없다: 고른 항목이 모두 일반 파일이어야 한다.
        for path in &req.paths {
            let dir = path.is_root()
                || match open_call(
                    &state,
                    &session,
                    principal.user,
                    FileRestoreOp::Stat {
                        volume: req.volume.clone(),
                        path: path.clone(),
                    },
                )
                .await?
                {
                    FileRestoreAnswer::Entry(e) => {
                        e.kind != opl_types::file_restore::EntryKind::File
                    }
                    _ => true,
                };
            if dir {
                return Err(ApiError::bad_request(format!(
                    "{} is not a regular file, and directories need guest-exec, which the guest agent of VM {:?} blocks",
                    path.display(),
                    vm.name
                ))
                .with_code("guest_mkdir_unavailable"));
            }
        }
    }
    let backup_name = raft_view(&state, &session).backup_name;
    audit.detail(format!(
        "backup file-restore to-vm {} → vm {}:{} ({})",
        describe(&backup_name, &req.volume, &req.paths),
        audit_text(&vm.name),
        audit_text(&req.target_dir),
        conflict_name(req.conflict),
    ));
    let task = create_task(
        &state.cluster,
        TaskOp::FileRestoreToGuest {
            session: id,
            backup: backup_of(&session),
            volume: req.volume,
            paths: req.paths,
            target_dir: req.target_dir,
            conflict: req.conflict,
        },
        vm.id,
        session.spec.node,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

fn conflict_name(c: GuestConflict) -> &'static str {
    match c {
        GuestConflict::Keep => "keep",
        GuestConflict::Skip => "skip",
        GuestConflict::Overwrite => "overwrite",
    }
}

/// 내려받기 흐름이 끝날 때(끊겨도) 적는 감사 항목
struct DownloadAudit {
    state: Arc<AppState>,
    principal: Principal,
    ip: std::net::IpAddr,
    path: String,
    started: Instant,
    detail: String,
    bytes: u64,
    entries: Option<u64>,
    failed: Option<String>,
}

impl DownloadAudit {
    fn done(&mut self, entries: u64) {
        self.entries = Some(entries);
    }
}

impl Drop for DownloadAudit {
    fn drop(&mut self) {
        let size = human_bytes(self.bytes);
        let detail = match (&self.entries, &self.failed) {
            (Some(n), _) => format!("{}, {n} entries, {size})", self.detail),
            (None, Some(e)) => format!("{}, failed after {size}: {})", self.detail, audit_text(e)),
            (None, None) => format!("{}, interrupted after {size})", self.detail),
        };
        crate::audit::record_stream(
            &self.state,
            &self.principal,
            self.ip,
            "/file-restore/sessions/{id}/download/{token}",
            &self.path,
            200,
            self.started,
            detail,
        );
    }
}

/// `3.1 MiB`
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> EntryPath {
        EntryPath::parse(s).unwrap()
    }

    #[test]
    fn describe_cuts_paths_and_escapes_names() {
        let paths: Vec<EntryPath> = (0..7).map(|i| p(&format!("/etc/f{i}"))).collect();
        assert_eq!(
            describe("web-1", "disk0p1", &paths[..1]),
            "web-1 disk0p1:/etc/f0"
        );
        let d = describe("web-1", "disk0p1", &paths);
        assert!(d.ends_with("disk0p1:/etc/f4, and 2 more"), "{d}");
        let evil = EntryPath::from_parts(vec![PathPart::new("a\nb\u{1b}[31m").unwrap()]).unwrap();
        let d = describe("web", "disk0", &[evil]);
        assert!(!d.contains('\n') && !d.contains('\u{1b}'), "{d}");
        let long = EntryPath::from_parts(vec![PathPart::new("x".repeat(600)).unwrap()]).unwrap();
        assert!(describe("web", "disk0", &[long]).chars().count() < 230);
    }

    #[test]
    fn default_names_follow_the_selection() {
        let req = |paths: Vec<EntryPath>| DownloadLinkRequest {
            volume: "lvm/vg0/data".into(),
            paths,
            format: DownloadFormat::Auto,
            name: None,
        };
        let one = req(vec![p("/etc/nginx.conf")]);
        assert_eq!(
            default_name("web-1", &one, DownloadFormat::Raw),
            "nginx.conf"
        );
        assert_eq!(
            default_name("web-1", &one, DownloadFormat::Zip),
            "web-1-nginx.conf.zip"
        );
        let many = req(vec![p("/a"), p("/b")]);
        assert_eq!(
            default_name("web-1", &many, DownloadFormat::TarGz),
            "web-1-lvm_vg0_data.tar.gz"
        );
        let root = req(vec![EntryPath::root()]);
        assert_eq!(
            default_name("web-1", &root, DownloadFormat::Zip),
            "web-1-lvm_vg0_data.zip"
        );
        assert!(clean_name("a/b").is_err());
        assert!(clean_name("..").is_err());
        assert!(clean_name("").is_err());
        assert!(clean_name(&"x".repeat(256)).is_err());
        assert_eq!(clean_name("etc.zip").unwrap(), "etc.zip");
    }

    #[test]
    fn query_paths_take_strings_or_raw_bytes() {
        assert_eq!(query_path(None, None).unwrap(), EntryPath::root());
        assert_eq!(query_path(Some("/etc"), None).unwrap(), p("/etc"));
        let raw = URL_SAFE_NO_PAD.encode(b"/etc/\xff");
        let got = query_path(None, Some(&raw)).unwrap();
        assert_eq!(got.parts()[1].as_bytes(), [0xff]);
        assert_eq!(
            query_path(Some("/a/../b"), None).unwrap_err().code,
            "invalid_path"
        );
        assert_eq!(
            query_path(Some("/a"), Some(&raw)).unwrap_err().code,
            "invalid_path"
        );
        assert_eq!(
            query_path(None, Some("!!")).unwrap_err().code,
            "invalid_path"
        );
        assert_eq!(
            query_path(None, Some(&URL_SAFE_NO_PAD.encode(b"/a//b")))
                .unwrap_err()
                .code,
            "invalid_path"
        );
    }

    #[test]
    fn helper_errors_become_api_errors() {
        use opl_restore::proto::AgentError;
        let agent = |code| HelperError::Agent(AgentError::new(code, "x"));
        let cases = [
            (agent(ErrorCode::NotFound), 404, "entry_not_found"),
            (agent(ErrorCode::NotDir), 400, "invalid_path"),
            (agent(ErrorCode::MountFailed), 409, "volume_not_mountable"),
            (agent(ErrorCode::Busy), 429, "file_restore_busy"),
            (agent(ErrorCode::Io), 502, "file_restore_io"),
            (HelperError::Busy, 429, "file_restore_busy"),
            (HelperError::Timeout, 504, "file_restore_timeout"),
            (HelperError::Closed("gone".into()), 409, "session_closed"),
        ];
        for (e, status, code) in cases {
            let got = helper_error("disk0", e);
            assert_eq!((got.status.as_u16(), got.code), (status, code));
            // 노드 사이에서 같은 오류로 되살아난다.
            let back = from_fault(fault(got));
            assert_eq!((back.status.as_u16(), back.code), (status, code));
        }
        let unknown = from_fault(FileRestoreFault::new(418, "teapot", "x"));
        assert_eq!(unknown.code, "file_restore_error");
    }

    #[test]
    fn sizes_and_durations_read_well() {
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(3 << 20), "3.0 MiB");
        assert_eq!(human_duration(Duration::from_secs(900)), "15 min");
        assert_eq!(human_duration(Duration::from_secs(8 * 3600)), "8 h");
        assert_eq!(human_duration(Duration::from_millis(300)), "1 s");
    }

    fn backup() -> Backup {
        let vm_spec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt", "nics": [],
        }))
        .unwrap();
        opl_types::resource::Resource::new(
            "web-1",
            BackupSpec {
                vm: Uuid::nil(),
                vm_name: "web".into(),
                pool: "bk".into(),
                vm_spec,
                job: None,
                origin: None,
                incremental: false,
                max_chain: None,
                source: None,
                store: None,
            },
        )
    }

    #[tokio::test]
    async fn links_are_single_use_per_user_and_expire() {
        let s = Session::new(
            Uuid::now_v7(),
            &backup(),
            Uuid::nil(),
            Uuid::nil(),
            0,
            Duration::from_secs(60),
            Duration::from_secs(3600),
        );
        // 부팅 중에는 링크를 만들지 않는다.
        let req = DownloadLinkRequest {
            volume: "disk0p1".into(),
            paths: vec![p("/etc")],
            format: DownloadFormat::Zip,
            name: None,
        };
        assert_eq!(
            s.link(Uuid::nil(), req.clone()).unwrap_err().code,
            "session_not_ready"
        );
        let me = Uuid::now_v7();
        let other = Uuid::now_v7();
        s.links.lock().unwrap().insert(
            "t".into(),
            PendingLink {
                user: me,
                req: req.clone(),
                expires: Instant::now() + LINK_TTL,
            },
        );
        assert_eq!(
            s.take_link("t", other).unwrap_err().status,
            StatusCode::FORBIDDEN
        );
        assert!(s.take_link("t", me).is_ok());
        assert_eq!(s.take_link("t", me).unwrap_err().code, "link_expired");
        s.links.lock().unwrap().insert(
            "old".into(),
            PendingLink {
                user: me,
                req,
                expires: Instant::now(),
            },
        );
        assert_eq!(s.take_link("old", me).unwrap_err().code, "link_expired");
    }

    #[tokio::test]
    async fn sessions_close_when_idle_or_asked() {
        let s = Session::new(
            Uuid::now_v7(),
            &backup(),
            Uuid::nil(),
            Uuid::nil(),
            0,
            Duration::from_millis(200),
            Duration::from_secs(3600),
        );
        let started = Instant::now();
        assert!(matches!(s.wait_close().await, SessionEnd::Expired(_)));
        assert!(started.elapsed() >= Duration::from_millis(200));
        // 내려받기가 도는 동안은 유휴가 아니다.
        let s = Arc::new(Session::new(
            Uuid::now_v7(),
            &backup(),
            Uuid::nil(),
            Uuid::nil(),
            0,
            Duration::from_millis(100),
            Duration::from_millis(700),
        ));
        let busy = Busy::new(s.clone());
        let started = Instant::now();
        let end = s.wait_close().await;
        assert!(
            matches!(&end, SessionEnd::Expired(m) if m.contains("maximum")),
            "{end:?}"
        );
        assert!(started.elapsed() >= Duration::from_millis(600));
        drop(busy);
        let s = Arc::new(Session::new(
            Uuid::now_v7(),
            &backup(),
            Uuid::nil(),
            Uuid::nil(),
            0,
            Duration::from_secs(60),
            Duration::from_secs(60),
        ));
        let waiter = tokio::spawn({
            let s = s.clone();
            async move { s.wait_close().await }
        });
        s.request_close("closed by admin".into());
        assert_eq!(
            waiter.await.unwrap(),
            SessionEnd::Closed("closed by admin".into())
        );
    }
}
