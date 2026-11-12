//! 감사 로그. 항목을 채널로 받아 쓰기 스레드가 `{dir}/audit.log`에 한 줄 JSON으로 덧붙이고
//! (요청이 디스크를 기다리지 않음), 같은 줄을 tracing 대상 `opl_audit`로도 내보낸다.
//! 파일이 커지면 `audit.log.1`, `.2`… 로 밀고, 읽을 때는 새 파일부터 거꾸로 훑는다.
//! 항목은 미들웨어 [`layer`]가 바꾸는 요청마다 만든다. 인증 추출기와 핸들러는 요청의
//! [`AuditSlot`]에 주체와 짧은 설명을 남긴다. `GET /api/v1/audit`는 모든 노드의 감사 로그를
//! 노드 사이 RPC(`ReadAudit`)로 읽어 합친다.

use std::convert::Infallible;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::extract::{FromRequestParts, MatchedPath, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::Response;
use opl_cluster::types::NodeId;
use opl_peer::AuditSource;
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery, AuditResponse, AuditTarget};
use opl_types::auth::Role;
use opl_types::node::NodeSpec;
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::Principal;
use crate::error::{ApiError, Json, Query};
use crate::limit::ClientIp;
use crate::state::AppState;

/// 쓰기 채널의 용량. 가득 차면 항목을 버리고 버린 수를 센다.
const CHANNEL_CAPACITY: usize = 10_000;
/// 쓴 것이 있으면 이만큼마다 `sync_data`
const SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// `shutdown`이 남은 것을 쓰기를 기다리는 최대 시간
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// 항목의 문자열 필드(`path`·`target.key`·`detail`·`user`)의 최대 바이트. 넘으면 잘라 `…`을 붙인다.
const MAX_FIELD_BYTES: usize = 512;
/// 인증 없는 항목(`user` 없음)은 IP마다 이 창에 `UNAUTHENTICATED_PER_WINDOW`개까지만 적는다.
const UNAUTHENTICATED_WINDOW: Duration = Duration::from_secs(60);
const UNAUTHENTICATED_PER_WINDOW: u32 = 60;
/// 인증 없는 항목을 세는 IP의 최대 수. 넘치면 가장 오래된 창부터 버린다.
const UNAUTHENTICATED_IPS: usize = 10_000;
const FILE_NAME: &str = "audit.log";
/// 다른 노드의 감사 로그를 읽는 데 기다리는 최대 시간
const PEER_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// 읽기 API의 기본 범위(`until`부터 거슬러)
const DEFAULT_RANGE_MS: u64 = 24 * 60 * 60 * 1000;
const DEFAULT_LIMIT: u32 = 200;

/// 감사 로그 설정
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// 파일을 두는 디렉터리(없으면 0700으로 만든다)
    pub dir: PathBuf,
    /// 항목의 `node`에 넣을 이 노드의 이름
    pub node: String,
    /// 한 파일의 최대 크기(바이트). 넘으면 회전한다.
    pub max_file_size: u64,
    /// 지금 파일을 포함한 최대 파일 수
    pub max_files: usize,
}

/// 노드의 감사 로그. 꺼져 있으면 아무것도 적지 않고 읽으면 빈 목록이다.
pub struct AuditLog {
    inner: Option<Inner>,
}

struct Inner {
    node: String,
    dir: PathBuf,
    tx: SyncSender<Msg>,
    /// 채널이 가득 차 버린 항목 수. 쓰기 스레드가 다음 항목 앞에 `audit.dropped`로 적는다.
    dropped: Arc<AtomicU64>,
    /// 인증 없는 항목의 IP별 창(쓰기 스레드가 끝난 창을 훑는다)
    unauthenticated: Arc<std::sync::Mutex<Unauthenticated>>,
    /// 쓰기 스레드가 끝난 뒤의 기록을 이미 경고했다.
    stopped: std::sync::atomic::AtomicBool,
}

enum Msg {
    Entry(Box<AuditEntry>),
    /// 남은 것을 쓰고 fsync한 뒤 스레드를 끝낸다.
    Shutdown(tokio::sync::oneshot::Sender<()>),
}

impl AuditLog {
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// 감사 로그를 적는지(`disabled`가 아닌지)
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// 디렉터리를 만들고 쓰기 스레드를 띄운다.
    pub fn open(config: AuditConfig) -> io::Result<Self> {
        Self::start(config, CHANNEL_CAPACITY, None)
    }

    /// `gate`가 있으면 쓰기 스레드는 그것을 받을 때까지 기다린다(시험에서 채널을 채우려고).
    fn start(config: AuditConfig, capacity: usize, gate: Option<Receiver<()>>) -> io::Result<Self> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.dir)?;
        // 이미 있던 디렉터리도 0700으로 맞춘다.
        std::fs::set_permissions(&config.dir, std::fs::Permissions::from_mode(0o700))?;
        let writer = Writer::open(&config)?;
        let (tx, rx) = sync_channel(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let counter = dropped.clone();
        let node = config.node.clone();
        let unauthenticated = Arc::new(std::sync::Mutex::new(Unauthenticated::default()));
        let table = unauthenticated.clone();
        std::thread::Builder::new()
            .name("opl-audit".into())
            .spawn(move || {
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
                run(writer, rx, &counter, &node, &table);
            })?;
        Ok(Self {
            inner: Some(Inner {
                node: config.node,
                dir: config.dir,
                tx,
                dropped,
                unauthenticated,
                stopped: Default::default(),
            }),
        })
    }

    /// 항목을 적는다(기다리지 않음). `node`는 이 노드의 이름으로 바꾸고 긴 필드는 자른다.
    /// 인증 없는 항목은 IP(IPv6는 /64)마다 1분에 60개까지만 적고, 넘친 수는 창이 끝나면 쓰기
    /// 스레드가 1초 안에(그 전에 그 IP가 다시 오면 그 항목 앞에, 또는 `shutdown` 때)
    /// `audit.suppressed`로 적는다.
    pub fn record(&self, entry: AuditEntry) {
        self.record_at(entry, Instant::now());
    }

    /// `now`는 인증 없는 항목의 창을 재는 시각이다(시험이 넣는다).
    fn record_at(&self, mut entry: AuditEntry, now: Instant) {
        let Some(inner) = &self.inner else { return };
        truncate_fields(&mut entry);
        if entry.user.is_none() {
            let key = unauthenticated_key(&entry.ip);
            let (write, suppressed) = inner.unauthenticated.lock().unwrap().admit(&key, now);
            for s in suppressed {
                inner.send(suppressed_entry(s));
            }
            if !write {
                return;
            }
        }
        inner.send(entry);
    }

    /// 조건에 맞는 항목을 새 것부터 `limit + 1`개까지 돌려준다(시간 내림차순).
    pub async fn read(&self, query: AuditQuery) -> io::Result<Vec<AuditEntry>> {
        let Some(inner) = &self.inner else {
            return Ok(Vec::new());
        };
        let dir = inner.dir.clone();
        tokio::task::spawn_blocking(move || read_dir(&dir, &query))
            .await
            .map_err(io::Error::other)?
    }

    /// 남은 `audit.suppressed`와 채널에 남은 것을 쓰고 fsync한다. 그 뒤의 항목은 적지 않는다.
    /// 5초 안에 끝나지 않으면 경고하고 돌아간다.
    pub async fn shutdown(&self) {
        self.shutdown_within(SHUTDOWN_TIMEOUT).await;
    }

    async fn shutdown_within(&self, timeout: Duration) {
        let Some(inner) = &self.inner else { return };
        let (ack, done) = tokio::sync::oneshot::channel();
        let tx = inner.tx.clone();
        let finish = async move {
            // 채널이 가득 차 있을 수 있다. 막히는 send 대신 자리가 날 때까지 다시 시도해,
            // 제한 시간이 지나면 남는 것 없이 그만둔다.
            let mut msg = Msg::Shutdown(ack);
            loop {
                match tx.try_send(msg) {
                    Ok(()) => break,
                    Err(TrySendError::Full(back)) => {
                        msg = back;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            let _ = done.await;
        };
        if tokio::time::timeout(timeout, finish).await.is_err() {
            tracing::warn!("the audit log did not finish writing within {timeout:?}; giving up");
        }
    }
}

/// 다른 노드가 노드 사이 RPC로 이 노드의 감사 로그를 읽는다.
#[async_trait]
impl AuditSource for AuditLog {
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String> {
        AuditLog::read(self, query).await.map_err(|e| e.to_string())
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }
}

/// `GET /audit`의 조건
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AuditParams {
    /// 이 시각부터(포함): 유닉스 밀리초나 RFC 3339. 기본은 `until`의 24시간 전
    pub since: Option<String>,
    /// 이 시각까지(포함): 유닉스 밀리초나 RFC 3339. 기본은 지금
    pub until: Option<String>,
    /// 사용자 이름(정확히 같은 것)
    pub user: Option<String>,
    /// 노드 이름(정확히 같은 것)
    pub node: Option<String>,
    /// 동작(`POST /vms/{key}`)이나 경로에 들어 있는 문자열
    pub action: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    pub limit: Option<u32>,
}

/// 모든 노드의 감사 로그를 새 것부터 읽는다. admin만 읽는다. 읽지 못한 노드는 `unreachable`에,
/// 감사 로그를 끈 노드는 `disabled`에, `limit`보다 많으면 잘라 `truncated`다.
#[utoipa::path(get, path = "/audit", tag = "audit",
    params(AuditParams),
    responses((status = 200, body = AuditResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_audit(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(params): Query<AuditParams>,
) -> Result<Json<AuditResponse>, ApiError> {
    if principal.role != Role::Admin {
        return Err(ApiError::forbidden());
    }
    let until = match &params.until {
        Some(s) => parse_time("until", s)?,
        None => now_ms(),
    };
    let since = match &params.since {
        Some(s) => parse_time("since", s)?,
        None => until.saturating_sub(DEFAULT_RANGE_MS),
    };
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=AuditQuery::MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {}",
            AuditQuery::MAX_LIMIT
        )));
    }
    let query = AuditQuery {
        since,
        until,
        user: params.user,
        action: params.action,
        limit,
    };
    // 위트니스는 API가 없어 감사 로그도 없다. Node 리소스가 없는 멤버는 id를 이름으로 쓴다.
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let members = state.cluster.members();
    let reads = members
        .voters
        .iter()
        .chain(&members.learners)
        .filter_map(|(id, addr)| {
            let node = nodes.iter().find(|n| n.id == id.0);
            if node.is_some_and(|n| n.spec.witness) {
                return None;
            }
            let name = node.map_or_else(|| id.0.to_string(), |n| n.name.clone());
            if params.node.as_ref().is_some_and(|want| *want != name) {
                return None;
            }
            Some(read_node(&state, *id, addr, name, query.clone()))
        });
    let mut entries = Vec::new();
    let mut unreachable = Vec::new();
    let mut disabled = Vec::new();
    for (name, read) in crate::fan_out::gather(reads.collect()).await {
        match read {
            Ok(Some(read)) => entries.extend(read),
            Ok(None) => disabled.push(name),
            Err(e) => {
                tracing::warn!(node = %name, error = %e, "cannot read the audit log of a node");
                unreachable.push(name);
            }
        }
    }
    unreachable.sort();
    disabled.sort();
    entries.sort_by_key(|e| std::cmp::Reverse((e.time, e.id)));
    let truncated = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    Ok(Json(AuditResponse {
        entries,
        unreachable,
        disabled,
        truncated,
    }))
}

/// 노드 하나의 감사 로그. 이 노드면 파일을 직접, 다른 노드면 노드 사이 RPC로 읽는다.
/// 감사 로그를 끈 노드면 None이다.
async fn read_node(
    state: &AppState,
    id: NodeId,
    addr: &str,
    name: String,
    query: AuditQuery,
) -> (String, Result<Option<Vec<AuditEntry>>, String>) {
    if id.0 == state.node {
        if !state.audit.is_enabled() {
            return (name, Ok(None));
        }
        let read = state.audit.read(query).await.map(Some);
        return (name, read.map_err(|e| e.to_string()));
    }
    let Some(peers) = &state.peers else {
        return (name, Err("no node-to-node connection".into()));
    };
    let read = tokio::time::timeout(
        PEER_READ_TIMEOUT,
        peers.read_audit(addr, query, PEER_READ_TIMEOUT),
    )
    .await;
    let read = match read {
        Ok(Ok(entries)) => Ok(entries),
        Ok(Err(status)) => Err(status.message().to_string()),
        Err(_) => Err("timed out".into()),
    };
    (name, read)
}

/// 유닉스 밀리초나 RFC 3339 시각
fn parse_time(field: &str, value: &str) -> Result<u64, ApiError> {
    if let Ok(ms) = value.parse::<u64>() {
        return Ok(ms);
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(|t| u64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok())
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "{field} must be unix milliseconds or an RFC 3339 time, not {value:?}"
            ))
        })
}

/// 요청 하나의 감사 정보. 미들웨어가 요청 확장에 넣고, 인증 추출기와 핸들러가 채운다.
#[derive(Clone, Default)]
pub(crate) struct AuditSlot(Arc<Mutex<SlotData>>);

#[derive(Default)]
struct SlotData {
    principal: Option<Principal>,
    auth: Option<AuditAuth>,
    detail: Option<String>,
}

impl AuditSlot {
    fn set_principal(&self, principal: Principal, auth: AuditAuth) {
        let mut data = self.0.lock().unwrap();
        data.principal = Some(principal);
        data.auth = Some(auth);
    }
}

/// 인증 결과를 요청의 slot에 적는다. 실패해도 Bearer 토큰으로 시도했으면 그것은 남긴다.
pub(crate) fn note_auth(parts: &Parts, principal: Option<&Principal>) {
    let Some(slot) = parts.extensions.get::<AuditSlot>() else {
        return;
    };
    let auth = if bearer(&parts.headers) {
        AuditAuth::Token
    } else {
        AuditAuth::Session
    };
    match principal {
        Some(p) => slot.set_principal(p.clone(), auth),
        None if auth == AuditAuth::Token => slot.0.lock().unwrap().auth = Some(auth),
        None => {}
    }
}

fn bearer(headers: &HeaderMap) -> bool {
    headers
        .get(AUTHORIZATION)
        .is_some_and(|v| v.as_bytes().starts_with(b"Bearer "))
}

/// 핸들러가 감사 항목에 남기는 것. 미들웨어 없이 불린 라우터에서는 아무것도 하지 않는다.
pub struct AuditNote(Option<AuditSlot>);

impl AuditNote {
    /// 짧은 설명(비밀은 넣지 않는다)
    pub(crate) fn detail(&self, detail: impl Into<String>) {
        if let Some(slot) = &self.0 {
            slot.0.lock().unwrap().detail = Some(detail.into());
        }
    }

    /// 요청의 주체(로그인·로그아웃처럼 인증 추출기를 쓰지 않는 핸들러)
    pub(crate) fn principal(&self, principal: Principal, auth: AuditAuth) {
        if let Some(slot) = &self.0 {
            slot.set_principal(principal, auth);
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuditNote {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(parts.extensions.get::<AuditSlot>().cloned()))
    }
}

/// 라우트가 맞은 요청 가운데 바꾸는 것(GET·HEAD·OPTIONS가 아닌 것)과 콘솔 열기, OIDC 로그인 콜백을 적는다.
pub(crate) async fn layer(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(template) = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| api_path(m.as_str()).to_string())
    else {
        return next.run(req).await;
    };
    let method = req.method().clone();
    let read = matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    if read && template != "/vms/{key}/console" && template != "/auth/oidc/callback" {
        return next.run(req).await;
    }
    let path = api_path(req.uri().path()).to_string();
    let slot = AuditSlot::default();
    req.extensions_mut().insert(slot.clone());
    let start = Instant::now();
    let response = next.run(req).await;
    let data = std::mem::take(&mut *slot.0.lock().unwrap());
    let principal = data.principal;
    state.audit.record(AuditEntry {
        id: Uuid::now_v7(),
        time: now_ms(),
        // record가 이 노드의 이름으로 채운다.
        node: String::new(),
        user: principal.as_ref().map(|p| p.name.clone()),
        user_id: principal.as_ref().map(|p| p.user),
        role: principal.as_ref().map(|p| p.role),
        auth: data.auth.unwrap_or(AuditAuth::None),
        ip: ip.to_canonical().to_string(),
        action: format!("{method} {template}"),
        target: target(&template, &path),
        method: method.to_string(),
        path,
        status: response.status().as_u16(),
        duration_ms: start.elapsed().as_millis() as u64,
        detail: data.detail,
    });
    response
}

/// `/api/v1` 아래의 경로
fn api_path(path: &str) -> &str {
    path.strip_prefix("/api/v1").unwrap_or(path)
}

/// 템플릿의 첫 조각과, 템플릿의 첫 `{…}` 자리에 있는 실제 경로 조각. 매개변수가 없으면 None.
fn target(template: &str, path: &str) -> Option<AuditTarget> {
    let mut segments = template.split('/').filter(|s| !s.is_empty());
    let kind = segments.next()?;
    let index = template.split('/').position(|s| s.starts_with('{'))?;
    let key = path.split('/').nth(index)?;
    Some(AuditTarget {
        kind: kind.into(),
        key: key.into(),
    })
}

/// 쓰기 스레드
fn run(
    mut writer: Writer,
    rx: Receiver<Msg>,
    dropped: &AtomicU64,
    node: &str,
    unauthenticated: &std::sync::Mutex<Unauthenticated>,
) {
    let write_suppressed = |writer: &mut Writer, suppressed: Vec<Suppressed>| {
        for s in suppressed {
            writer.write(&AuditEntry {
                node: node.into(),
                ..suppressed_entry(s)
            });
        }
    };
    // 1초마다 끝난 인증 없는 창의 넘친 수를 적고 `sync_data`한다.
    let mut last_sync = Instant::now();
    loop {
        let wait = SYNC_INTERVAL.saturating_sub(last_sync.elapsed());
        match rx.recv_timeout(wait) {
            Ok(Msg::Entry(entry)) => {
                write_dropped(&mut writer, dropped, node);
                writer.write(&entry);
            }
            Ok(Msg::Shutdown(ack)) => {
                write_dropped(&mut writer, dropped, node);
                let suppressed = unauthenticated.lock().unwrap().drain();
                write_suppressed(&mut writer, suppressed);
                writer.sync();
                let _ = ack.send(());
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                writer.sync();
                return;
            }
        }
        if last_sync.elapsed() >= SYNC_INTERVAL {
            let suppressed = unauthenticated.lock().unwrap().sweep(Instant::now());
            write_suppressed(&mut writer, suppressed);
            writer.sync();
            last_sync = Instant::now();
        }
    }
}

/// 버린 항목이 있으면 그 수를 `audit.dropped` 항목으로 적는다.
fn write_dropped(writer: &mut Writer, dropped: &AtomicU64, node: &str) {
    let n = dropped.swap(0, Ordering::Relaxed);
    if n == 0 {
        return;
    }
    writer.write(&AuditEntry {
        id: Uuid::now_v7(),
        time: now_ms(),
        node: node.into(),
        user: None,
        user_id: None,
        role: None,
        auth: AuditAuth::None,
        ip: String::new(),
        method: String::new(),
        path: String::new(),
        action: "audit.dropped".into(),
        target: None,
        status: 0,
        duration_ms: 0,
        detail: Some(format!("{n} entries dropped")),
    });
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Inner {
    /// 이 노드의 이름을 넣어 쓰기 스레드에 넘긴다. 채널이 가득 찼거나 스레드가 끝났으면 버리고 센다.
    fn send(&self, mut entry: AuditEntry) {
        entry.node.clone_from(&self.node);
        match self.tx.try_send(Msg::Entry(Box::new(entry))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                if !self.stopped.swap(true, Ordering::Relaxed) {
                    tracing::warn!("the audit log writer has stopped; entries are not recorded");
                }
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// 긴 문자열 필드를 `MAX_FIELD_BYTES`로 자른다(UTF-8 글자 경계, 잘랐으면 끝에 `…`).
fn truncate_fields(entry: &mut AuditEntry) {
    let fields = [
        Some(&mut entry.path),
        entry.target.as_mut().map(|t| &mut t.key),
        entry.detail.as_mut(),
        entry.user.as_mut(),
    ];
    for field in fields.into_iter().flatten() {
        if field.len() > MAX_FIELD_BYTES {
            let mut end = MAX_FIELD_BYTES;
            while !field.is_char_boundary(end) {
                end -= 1;
            }
            field.truncate(end);
            field.push('…');
        }
    }
}

/// 한 IP(IPv6는 /64)의 끝난(또는 버린) 창에서 적지 않은 수
#[derive(Debug, PartialEq)]
struct Suppressed {
    key: String,
    count: u64,
    /// 그 창의 시작
    since: Instant,
}

/// 인증 없는 항목 `count`개를 적지 않았다는 항목. `ip`는 세는 단위(IPv6는 `…::/64`)다.
fn suppressed_entry(s: Suppressed) -> AuditEntry {
    // 창 시작(Instant)을 벽시계 시각으로 옮긴다.
    let ago = Instant::now()
        .saturating_duration_since(s.since)
        .as_millis() as u64;
    let since =
        time::OffsetDateTime::from_unix_timestamp((now_ms().saturating_sub(ago) / 1000) as i64)
            .ok()
            .and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_default();
    AuditEntry {
        id: Uuid::now_v7(),
        time: now_ms(),
        // record·쓰기 스레드가 이 노드의 이름으로 채운다.
        node: String::new(),
        user: None,
        user_id: None,
        role: None,
        auth: AuditAuth::None,
        ip: s.key,
        method: String::new(),
        path: String::new(),
        action: "audit.suppressed".into(),
        target: None,
        status: 0,
        duration_ms: 0,
        detail: Some(format!(
            "{} unauthenticated requests not recorded since {since}",
            s.count
        )),
    }
}

/// 인증 없는 항목을 세는 단위. IPv4는 주소, IPv6는 /64 네트워크(로그인 시도 제한과 같은 규칙)
fn unauthenticated_key(ip: &str) -> String {
    match ip.parse::<std::net::IpAddr>().map(crate::limit::counted_ip) {
        Ok(v6 @ std::net::IpAddr::V6(_)) => format!("{v6}/64"),
        Ok(v4) => v4.to_string(),
        Err(_) => ip.to_string(),
    }
}

/// 인증 없는 항목의 IP별 1분 창
#[derive(Default)]
struct Unauthenticated {
    windows: std::collections::HashMap<String, Window>,
    /// (창 시작, 키) 순서. 가장 오래된 창과 끝난 창을 훑지 않고 찾는다.
    order: std::collections::BTreeSet<(Instant, String)>,
}

struct Window {
    start: Instant,
    written: u32,
    /// 이 창에서 적지 않은 수
    suppressed: u64,
}

impl Window {
    fn new(start: Instant) -> Self {
        Self {
            start,
            written: 1,
            suppressed: 0,
        }
    }

    fn suppressed(&self, key: String) -> Option<Suppressed> {
        (self.suppressed > 0).then_some(Suppressed {
            key,
            count: self.suppressed,
            since: self.start,
        })
    }
}

impl Unauthenticated {
    /// `key`의 인증 없는 항목을 적을지와, 그 앞에 먼저 적을 넘친 수를 돌려준다.
    fn admit(&mut self, key: &str, now: Instant) -> (bool, Vec<Suppressed>) {
        let mut first = Vec::new();
        if let Some(w) = self.windows.get_mut(key) {
            if now.duration_since(w.start) < UNAUTHENTICATED_WINDOW {
                if w.written < UNAUTHENTICATED_PER_WINDOW {
                    w.written += 1;
                    return (true, first);
                }
                w.suppressed += 1;
                return (false, first);
            }
            // 쓰기 스레드가 훑기 전에 다시 왔다. 앞 창에서 넘친 수를 먼저 적고 새 창을 연다.
            let old = std::mem::replace(w, Window::new(now));
            self.order.remove(&(old.start, key.to_string()));
            self.order.insert((now, key.to_string()));
            first.extend(old.suppressed(key.to_string()));
            return (true, first);
        }
        if self.windows.len() >= UNAUTHENTICATED_IPS
            && let Some((_, oldest)) = self.order.pop_first()
            && let Some(w) = self.windows.remove(&oldest)
        {
            first.extend(w.suppressed(oldest));
        }
        self.windows.insert(key.to_string(), Window::new(now));
        self.order.insert((now, key.to_string()));
        (true, first)
    }

    /// 끝난 창을 모두 지우고, 그 가운데 넘친 수가 있는 것을 돌려준다.
    fn sweep(&mut self, now: Instant) -> Vec<Suppressed> {
        let mut out = Vec::new();
        while let Some((start, _)) = self.order.first()
            && now.duration_since(*start) >= UNAUTHENTICATED_WINDOW
        {
            let (_, key) = self.order.pop_first().expect("first above");
            if let Some(w) = self.windows.remove(&key) {
                out.extend(w.suppressed(key));
            }
        }
        out
    }

    /// 넘친 수가 남은 것을 모두 꺼낸다(종료 때).
    fn drain(&mut self) -> Vec<Suppressed> {
        self.order.clear();
        self.windows
            .drain()
            .filter_map(|(key, w)| w.suppressed(key))
            .collect()
    }
}

/// `n`번째 파일(0은 지금 파일)
fn file_path(dir: &Path, n: usize) -> PathBuf {
    if n == 0 {
        dir.join(FILE_NAME)
    } else {
        dir.join(format!("{FILE_NAME}.{n}"))
    }
}

/// 지금 파일과 회전
struct Writer {
    dir: PathBuf,
    max_file_size: u64,
    max_files: usize,
    /// 열지 못했으면 없다(다음 쓰기에서 다시 연다).
    file: Option<File>,
    size: u64,
    dirty: bool,
}

impl Writer {
    fn open(config: &AuditConfig) -> io::Result<Self> {
        let mut writer = Self {
            dir: config.dir.clone(),
            max_file_size: config.max_file_size,
            max_files: config.max_files.max(1),
            file: None,
            size: 0,
            dirty: false,
        };
        // `max_files`를 줄였으면 남은 오래된 파일(`audit.log.N`, N ≥ max_files)을 지운다.
        let prefix = format!("{FILE_NAME}.");
        for item in std::fs::read_dir(&config.dir)? {
            let name = item?.file_name();
            let n = name
                .to_str()
                .and_then(|n| n.strip_prefix(&prefix))
                .and_then(|n| n.parse::<usize>().ok());
            if let Some(n) = n.filter(|n| *n >= writer.max_files) {
                remove(&file_path(&config.dir, n))?;
            }
        }
        writer.reopen()?;
        Ok(writer)
    }

    fn reopen(&mut self) -> io::Result<()> {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            // 심볼릭 링크를 따라가 다른 파일에 쓰지 않는다.
            .custom_flags(libc::O_NOFOLLOW)
            .open(file_path(&self.dir, 0))?;
        // 이미 있던 파일도 0600으로 맞춘다.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        self.size = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }

    /// 한 줄을 덧붙인다. 실패하면 경고만 남긴다(요청은 이미 끝났다).
    fn write(&mut self, entry: &AuditEntry) {
        let Ok(json) = serde_json::to_string(entry) else {
            return;
        };
        tracing::info!(target: "opl_audit", "{json}");
        if let Err(e) = self.append(&json) {
            tracing::warn!("cannot write the audit log: {e}");
        }
    }

    fn append(&mut self, json: &str) -> io::Result<()> {
        let line = format!("{json}\n");
        let len = line.len() as u64;
        if self.file.is_some() && self.size > 0 && self.size + len > self.max_file_size {
            self.rotate()?;
        }
        if self.file.is_none() {
            self.reopen()?;
        }
        let file = self.file.as_mut().expect("opened above");
        file.write_all(line.as_bytes())?;
        self.size += len;
        self.dirty = true;
        Ok(())
    }

    /// 가장 오래된 것을 지우고 `.N` → `.N+1`, 지금 파일 → `.1`로 민 뒤 새 파일을 연다.
    fn rotate(&mut self) -> io::Result<()> {
        self.sync();
        self.file = None;
        let keep = self.max_files - 1;
        if keep == 0 {
            remove(&file_path(&self.dir, 0))?;
        } else {
            remove(&file_path(&self.dir, keep))?;
            for n in (1..keep).rev() {
                rename(&file_path(&self.dir, n), &file_path(&self.dir, n + 1))?;
            }
            rename(&file_path(&self.dir, 0), &file_path(&self.dir, 1))?;
        }
        self.reopen()
    }

    fn sync(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(file) = &self.file
            && let Err(e) = file.sync_data()
        {
            tracing::warn!("cannot sync the audit log: {e}");
        }
        self.dirty = false;
    }
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn rename(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 새 파일부터 줄을 거꾸로 훑어 조건에 맞는 것을 `limit + 1`개까지 모은다. 망가진 줄은 건너뛴다.
fn read_dir(dir: &Path, query: &AuditQuery) -> io::Result<Vec<AuditEntry>> {
    let want = query.limit as usize + 1;
    let mut out = Vec::new();
    // 읽는 중에 회전하면 같은 항목이 두 파일에서 보일 수 있다.
    let mut seen = std::collections::HashSet::new();
    for n in 0.. {
        let text = match std::fs::read_to_string(file_path(dir, n)) {
            Ok(text) => text,
            // 지금 파일은 회전 중에 잠깐 없을 수 있다.
            Err(e) if e.kind() == io::ErrorKind::NotFound && n == 0 => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(e),
        };
        let lines: Vec<&str> = text.lines().collect();
        let (Some(first), Some(last)) = (
            lines.iter().find_map(|l| parse(l)),
            lines.iter().rev().find_map(|l| parse(l)),
        ) else {
            continue;
        };
        // 파일 안은 오래된 것부터다. 이 파일의 가장 새 것이 범위 앞이면 더 오래된 파일도 그렇다.
        if last.time < query.since {
            break;
        }
        if first.time > query.until {
            continue;
        }
        for entry in lines.iter().rev().filter_map(|l| parse(l)) {
            if query.matches(&entry) && seen.insert(entry.id) {
                out.push(entry);
                if out.len() >= want {
                    break;
                }
            }
        }
        if out.len() >= want {
            break;
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse((e.time, e.id)));
    Ok(out)
}

fn parse(line: &str) -> Option<AuditEntry> {
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn config(dir: &Path, max_file_size: u64, max_files: usize) -> AuditConfig {
        AuditConfig {
            dir: dir.join("audit"),
            node: "n1".into(),
            max_file_size,
            max_files,
        }
    }

    fn entry(time: u64, user: &str, action: &str) -> AuditEntry {
        AuditEntry {
            id: Uuid::now_v7(),
            time,
            node: "n1".into(),
            user: Some(user.into()),
            user_id: None,
            role: None,
            auth: AuditAuth::Session,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms".into(),
            action: action.into(),
            target: None,
            status: 201,
            duration_ms: 3,
            detail: None,
        }
    }

    fn all(limit: u32) -> AuditQuery {
        AuditQuery {
            since: 0,
            until: u64::MAX,
            user: None,
            action: None,
            limit,
        }
    }

    fn times(entries: &[AuditEntry]) -> Vec<u64> {
        entries.iter().map(|e| e.time).collect()
    }

    fn lines(path: &Path) -> Vec<AuditEntry> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// 한 줄의 크기(시각이 같은 자릿수면 같다)
    fn line_len(time: u64) -> u64 {
        serde_json::to_string(&entry(time, "alice", "POST /vms"))
            .unwrap()
            .len() as u64
            + 1
    }

    #[test]
    fn target_is_the_first_segment_and_the_first_parameter() {
        let t = |template, path| target(template, path).map(|t| (t.kind, t.key));
        assert_eq!(
            t("/vms/{key}/actions/{action}", "/vms/web-1/actions/start"),
            Some(("vms".into(), "web-1".into()))
        );
        assert_eq!(
            t("/vms/{key}/snapshots/{name}", "/vms/web-1/snapshots/s1"),
            Some(("vms".into(), "web-1".into()))
        );
        assert_eq!(t("/users", "/users"), None);
        assert_eq!(t("/auth/login", "/auth/login"), None);
        assert_eq!(api_path("/api/v1/vms/{key}"), "/vms/{key}");
        assert_eq!(api_path("/vms"), "/vms");
    }

    #[tokio::test]
    async fn writes_and_reads_back_with_private_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        let mut first = entry(1_000, "alice", "POST /vms");
        // node는 이 노드의 이름으로 바뀐다.
        first.node = "other".into();
        log.record(first.clone());
        log.record(entry(2_000, "bob", "DELETE /vms/{key}"));
        log.shutdown().await;
        // shutdown 뒤의 항목은 적지 않는다.
        log.record(entry(3_000, "carol", "POST /vms"));

        let dir_mode = std::fs::metadata(&config.dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        let file = config.dir.join("audit.log");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let read = log.read(all(10)).await.unwrap();
        assert_eq!(times(&read), [2_000, 1_000]);
        assert_eq!(read[1].node, "n1");
        assert_eq!(read[1].id, first.id);
        assert_eq!(lines(&file).len(), 2);
    }

    #[tokio::test]
    async fn disabled_writes_nothing_and_reads_empty() {
        let log = AuditLog::disabled();
        log.record(entry(1, "alice", "POST /vms"));
        log.shutdown().await;
        assert!(log.read(all(10)).await.unwrap().is_empty());
    }

    #[test]
    fn rotates_by_size_and_removes_the_oldest() {
        let tmp = tempfile::tempdir().unwrap();
        let line = line_len(1_000);
        // 한 파일에 세 줄, 파일은 모두 세 개
        let config = config(tmp.path(), line * 3, 3);
        std::fs::create_dir_all(&config.dir).unwrap();
        let mut writer = Writer::open(&config).unwrap();
        for t in 1_000..1_010 {
            writer.write(&entry(t, "alice", "POST /vms"));
        }
        writer.sync();
        let dir = &config.dir;
        assert_eq!(times(&lines(&dir.join("audit.log"))), [1_009]);
        assert_eq!(
            times(&lines(&dir.join("audit.log.1"))),
            [1_006, 1_007, 1_008]
        );
        assert_eq!(
            times(&lines(&dir.join("audit.log.2"))),
            [1_003, 1_004, 1_005]
        );
        assert!(!dir.join("audit.log.3").exists());
        let mode = std::fs::metadata(dir.join("audit.log"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        // 회전한 파일까지 새 것부터 읽는다.
        let read = read_dir(dir, &all(100)).unwrap();
        assert_eq!(times(&read), (1_003..1_010).rev().collect::<Vec<_>>());
    }

    #[test]
    fn a_single_file_is_replaced_when_full() {
        let tmp = tempfile::tempdir().unwrap();
        let line = line_len(1_000);
        let config = config(tmp.path(), line * 2, 1);
        std::fs::create_dir_all(&config.dir).unwrap();
        let mut writer = Writer::open(&config).unwrap();
        for t in 1_000..1_005 {
            writer.write(&entry(t, "alice", "POST /vms"));
        }
        let dir = &config.dir;
        assert_eq!(times(&lines(&dir.join("audit.log"))), [1_004]);
        assert!(!dir.join("audit.log.1").exists());
    }

    /// 파일들을 직접 만든다(각 목록은 오래된 것부터).
    fn write_files(dir: &Path, files: &[&[AuditEntry]]) {
        std::fs::create_dir_all(dir).unwrap();
        for (n, entries) in files.iter().enumerate() {
            let text: String = entries
                .iter()
                .map(|e| serde_json::to_string(e).unwrap() + "\n")
                .collect();
            std::fs::write(file_path(dir, n), text).unwrap();
        }
    }

    #[test]
    fn filters_newest_first_up_to_limit_plus_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_files(
            dir,
            &[
                &[
                    entry(500, "alice", "POST /vms"),
                    entry(600, "bob", "POST /vms/{key}/actions/start"),
                    entry(700, "alice", "POST /vms/{key}/actions/stop"),
                ],
                &[
                    entry(200, "alice", "POST /vms/{key}/actions/start"),
                    entry(300, "alice", "DELETE /vms/{key}"),
                    entry(400, "alice", "POST /vms/{key}/actions/start"),
                ],
            ],
        );
        let q = AuditQuery {
            since: 250,
            until: 650,
            user: Some("alice".into()),
            action: Some("/actions/".into()),
            limit: 10,
        };
        assert_eq!(times(&read_dir(dir, &q).unwrap()), [400]);

        let q = AuditQuery {
            user: Some("alice".into()),
            ..all(2)
        };
        // limit + 1개(합칠 때 넘침을 알 수 있게)
        assert_eq!(times(&read_dir(dir, &q).unwrap()), [700, 500, 400]);
        assert_eq!(times(&read_dir(dir, &all(0)).unwrap()), [700]);
    }

    #[test]
    fn skips_broken_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let good = |t| serde_json::to_string(&entry(t, "alice", "POST /vms")).unwrap();
        let text = format!(
            "{}\nnot json\n{{\"id\":1}}\n{}\n{{\"trunc",
            good(1),
            good(2)
        );
        std::fs::write(dir.join("audit.log"), text).unwrap();
        assert_eq!(times(&read_dir(dir, &all(10)).unwrap()), [2, 1]);
    }

    #[test]
    fn stops_at_files_older_than_since_and_skips_newer_than_until() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_files(
            dir,
            &[
                &[entry(900, "alice", "x")],
                &[entry(500, "alice", "x"), entry(600, "alice", "x")],
                &[entry(100, "alice", "x")],
            ],
        );
        // 가장 새 것이 since 앞인 파일(.2)에서 멈추므로 더 오래된 망가진 파일(.3)은 읽지 않는다.
        std::fs::write(file_path(dir, 3), [0xff, 0xfe]).unwrap();
        let q = AuditQuery {
            since: 550,
            until: 800,
            ..all(10)
        };
        assert_eq!(times(&read_dir(dir, &q).unwrap()), [600]);
        // 멈추지 않으면 망가진(UTF-8이 아닌) 파일을 읽다가 오류가 난다.
        assert!(read_dir(dir, &all(10)).is_err());
    }

    #[tokio::test]
    async fn an_overflow_is_recorded_as_audit_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let (open, gate) = std::sync::mpsc::channel();
        let log = AuditLog::start(config(tmp.path(), 1 << 20, 8), 2, Some(gate)).unwrap();
        for t in 1..=5 {
            log.record(entry(t, "alice", "POST /vms"));
        }
        open.send(()).unwrap();
        log.shutdown().await;
        let file = tmp.path().join("audit/audit.log");
        let written = lines(&file);
        assert_eq!(written.len(), 3);
        let dropped = &written[0];
        assert_eq!(dropped.action, "audit.dropped");
        assert_eq!(dropped.detail.as_deref(), Some("3 entries dropped"));
        assert_eq!((dropped.status, dropped.method.as_str()), (0, ""));
        assert_eq!(dropped.node, "n1");
        assert_eq!(times(&written[1..]), [1, 2]);
    }

    #[tokio::test]
    async fn long_fields_are_cut_at_512_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        let mut long = entry(1_000, &"u".repeat(600), "POST /vms");
        long.path = format!("/vms/{}", "a".repeat(60 * 1024));
        long.target = Some(opl_types::audit::AuditTarget {
            kind: "vms".into(),
            key: "k".repeat(1_000),
        });
        // UTF-8 글자 경계에서 자른다(한 글자 3바이트).
        long.detail = Some("가".repeat(300));
        log.record(long);
        log.shutdown().await;

        let file = config.dir.join("audit.log");
        let written = &lines(&file)[0];
        assert_eq!(written.path, format!("/vms/{}…", "a".repeat(507)));
        let key = &written.target.as_ref().unwrap().key;
        assert_eq!(*key, format!("{}…", "k".repeat(512)));
        let detail = written.detail.as_deref().unwrap();
        assert_eq!(detail, format!("{}…", "가".repeat(170)));
        assert_eq!(
            *written.user.as_ref().unwrap(),
            format!("{}…", "u".repeat(512))
        );
        assert!(std::fs::metadata(&file).unwrap().len() < 4096);
    }

    /// `ip`에서 온 인증 없는 항목(로그인 실패 같은)
    fn anonymous(time: u64, ip: &str) -> AuditEntry {
        AuditEntry {
            user: None,
            auth: AuditAuth::None,
            ip: ip.into(),
            ..entry(time, "-", "POST /auth/login")
        }
    }

    #[tokio::test]
    async fn unauthenticated_entries_are_limited_per_ip_and_window() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        let start = Instant::now();
        // 한 창에 61개면 61번째는 적지 않는다. 다른 IP와 인증된 항목은 따로 센다.
        for t in 1..=61 {
            log.record_at(anonymous(t, "10.0.0.1"), start);
        }
        log.record_at(anonymous(100, "10.0.0.2"), start);
        for t in 200..300 {
            let signed_in = AuditEntry {
                ip: "10.0.0.1".into(),
                ..entry(t, "alice", "POST /vms")
            };
            log.record_at(signed_in, start);
        }
        log.record_at(anonymous(62, "10.0.0.1"), start + Duration::from_secs(59));
        // 다음 창에서 처음 적을 때 그 앞에 넘친 수를 적는다.
        log.record_at(
            anonymous(1_000, "10.0.0.1"),
            start + Duration::from_secs(60),
        );
        // 끝날 때 남은 넘친 수도 적는다.
        for t in 2_000..2_062 {
            log.record_at(anonymous(t, "10.0.0.3"), start);
        }
        log.shutdown().await;

        let written = lines(&config.dir.join("audit.log"));
        let from = |ip: &str| -> Vec<u64> {
            written
                .iter()
                .filter(|e| e.ip == ip && e.user.is_none() && e.action != "audit.suppressed")
                .map(|e| e.time)
                .collect()
        };
        assert_eq!(
            from("10.0.0.1"),
            (1..=60).chain([1_000]).collect::<Vec<_>>()
        );
        assert_eq!(from("10.0.0.2"), [100]);
        assert_eq!(from("10.0.0.3"), (2_000..2_060).collect::<Vec<_>>());
        assert_eq!(written.iter().filter(|e| e.user.is_some()).count(), 100);
        let at = written
            .iter()
            .position(|e| e.action == "audit.suppressed")
            .unwrap();
        let first = &written[at];
        assert_eq!(first.ip, "10.0.0.1");
        assert_suppressed(first, 2);
        assert_eq!(
            (first.method.as_str(), first.path.as_str(), first.status),
            ("", "", 0)
        );
        assert_eq!((first.node.as_str(), &first.user), ("n1", &None));
        assert_eq!(written[at + 1].time, 1_000);
        let last = written.last().unwrap();
        assert_eq!(last.action, "audit.suppressed");
        assert_eq!(last.ip, "10.0.0.3");
        assert_suppressed(last, 2);
    }

    /// `n`개를 적지 않았다는 항목이고, `detail`의 창 시작이 RFC 3339다.
    fn assert_suppressed(entry: &AuditEntry, n: u64) -> u64 {
        assert_eq!(entry.action, "audit.suppressed");
        let detail = entry.detail.as_deref().unwrap();
        let prefix = format!("{n} unauthenticated requests not recorded since ");
        let since = detail.strip_prefix(&prefix).expect(detail);
        let since =
            time::OffsetDateTime::parse(since, &time::format_description::well_known::Rfc3339)
                .expect(since);
        (since.unix_timestamp() * 1000) as u64
    }

    #[tokio::test]
    async fn an_ended_window_is_written_without_new_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        // 61초 전에 시작한(이미 끝난) 창에서 넘쳤고, 그 IP는 다시 오지 않는다.
        let past = Instant::now().checked_sub(Duration::from_secs(61)).unwrap();
        for t in 1..=62 {
            log.record_at(anonymous(t, "10.0.0.1"), past);
        }
        // 넘치지 않은 끝난 창은 적지 않고 지운다.
        log.record_at(anonymous(100, "10.0.0.2"), past);
        let file = config.dir.join("audit.log");
        let deadline = Instant::now() + Duration::from_secs(5);
        let suppressed = loop {
            let written = lines(&file);
            if let Some(e) = written.iter().find(|e| e.action == "audit.suppressed") {
                break e.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no audit.suppressed: {written:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(
            (suppressed.ip.as_str(), suppressed.node.as_str()),
            ("10.0.0.1", "n1")
        );
        let since = assert_suppressed(&suppressed, 2);
        let ago = now_ms() - since;
        assert!((59_000..=64_000).contains(&ago), "{ago} ms ago");
        {
            let table = log.inner.as_ref().unwrap().unauthenticated.lock().unwrap();
            assert!(table.windows.is_empty() && table.order.is_empty());
        }
        log.shutdown().await;
        let written = lines(&file);
        let count = |action: &str| written.iter().filter(|e| e.action == action).count();
        assert_eq!(count("audit.suppressed"), 1);
        assert_eq!(count("POST /auth/login"), 61);
    }

    #[test]
    fn a_sweep_takes_ended_windows_only() {
        let mut table = Unauthenticated::default();
        let start = Instant::now();
        for _ in 0..=UNAUTHENTICATED_PER_WINDOW {
            table.admit("10.0.0.1", start);
        }
        table.admit("10.0.0.2", start);
        table.admit("10.0.0.3", start + Duration::from_secs(30));
        assert_eq!(table.sweep(start + Duration::from_secs(59)), []);
        assert_eq!(
            table.sweep(start + Duration::from_secs(60)),
            [Suppressed {
                key: "10.0.0.1".into(),
                count: 1,
                since: start
            }]
        );
        assert_eq!(table.windows.keys().collect::<Vec<_>>(), ["10.0.0.3"]);
        assert_eq!(table.order.len(), 1);
    }

    #[tokio::test]
    async fn ipv6_is_limited_per_slash_64() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        let now = Instant::now();
        // 같은 /64 안의 다른 주소들은 한 한도를 나눠 쓴다.
        for t in 1..=61 {
            log.record_at(anonymous(t, &format!("2001:db8:1:2::{t:x}")), now);
        }
        log.record_at(anonymous(100, "2001:db8:1:3::1"), now);
        log.shutdown().await;
        let written = lines(&config.dir.join("audit.log"));
        let logins: Vec<&str> = written
            .iter()
            .filter(|e| e.action != "audit.suppressed")
            .map(|e| e.ip.as_str())
            .collect();
        assert_eq!(logins.len(), 61);
        // 항목의 ip는 실제 주소 그대로다.
        assert_eq!(logins[0], "2001:db8:1:2::1");
        assert_eq!(logins[59], "2001:db8:1:2::3c");
        assert_eq!(logins[60], "2001:db8:1:3::1");
        let suppressed = written.last().unwrap();
        assert_eq!(suppressed.ip, "2001:db8:1:2::/64");
        assert_suppressed(suppressed, 1);
        assert_eq!(unauthenticated_key("10.0.0.1"), "10.0.0.1");
        assert_eq!(unauthenticated_key("::ffff:10.0.0.1"), "10.0.0.1");
        assert_eq!(unauthenticated_key("not an ip"), "not an ip");
    }

    #[test]
    fn the_oldest_window_goes_when_too_many_ips_are_counted() {
        let mut table = Unauthenticated::default();
        let start = Instant::now();
        for _ in 0..=UNAUTHENTICATED_PER_WINDOW {
            table.admit("10.0.0.1", start);
        }
        for n in 1..UNAUTHENTICATED_IPS {
            let later = start + Duration::from_secs(1);
            assert_eq!(table.admit(&format!("ip-{n}"), later), (true, vec![]));
        }
        // 가장 오래된 창(10.0.0.1)을 버리면서 그 넘친 수를 바로 적게 돌려준다.
        assert_eq!(
            table.admit("10.9.9.9", start + Duration::from_secs(2)),
            (
                true,
                vec![Suppressed {
                    key: "10.0.0.1".into(),
                    count: 1,
                    since: start
                }]
            )
        );
        assert_eq!(table.windows.len(), UNAUTHENTICATED_IPS);
        assert_eq!(table.order.len(), UNAUTHENTICATED_IPS);
        assert!(!table.windows.contains_key("10.0.0.1"));
    }

    #[test]
    fn an_entry_in_two_files_is_read_once() {
        // 읽는 중에 회전하면 같은 항목이 두 파일에서 보일 수 있다.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let moved = entry(500, "alice", "POST /vms");
        write_files(
            dir,
            &[
                std::slice::from_ref(&moved),
                &[entry(400, "alice", "POST /vms"), moved.clone()],
            ],
        );
        assert_eq!(times(&read_dir(dir, &all(10)).unwrap()), [500, 400]);
    }

    #[test]
    fn opening_removes_files_beyond_max_files() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 2);
        let old = |t| vec![entry(t, "alice", "POST /vms")];
        write_files(&config.dir, &[&old(4), &old(3), &old(2), &old(1)]);
        Writer::open(&config).unwrap();
        assert!(config.dir.join("audit.log.1").exists());
        assert!(!config.dir.join("audit.log.2").exists());
        assert!(!config.dir.join("audit.log.3").exists());
        assert_eq!(times(&read_dir(&config.dir, &all(10)).unwrap()), [4, 3]);
    }

    #[test]
    fn opening_tightens_permissions_and_refuses_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let mode = |path: &Path| {
            std::fs::symlink_metadata(path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        let chmod = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        std::fs::create_dir_all(&config.dir).unwrap();
        chmod(&config.dir, 0o755);
        let file = config.dir.join("audit.log");
        std::fs::write(&file, "").unwrap();
        chmod(&file, 0o644);
        drop(AuditLog::open(config.clone()).unwrap());
        assert_eq!(mode(&config.dir), 0o700);
        assert_eq!(mode(&file), 0o600);

        // 지금 파일이 심볼릭 링크면 열지 않는다.
        std::fs::remove_file(&file).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::write(&elsewhere, "").unwrap();
        chmod(&elsewhere, 0o644);
        std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
        assert!(AuditLog::open(config).is_err());
        assert_eq!(mode(&elsewhere), 0o644);
    }

    #[test]
    fn shutdown_gives_up_when_the_writer_is_stuck() {
        let tmp = tempfile::tempdir().unwrap();
        let (open, gate) = std::sync::mpsc::channel();
        let log = AuditLog::start(config(tmp.path(), 1 << 20, 8), 1, Some(gate)).unwrap();
        // 쓰기 스레드가 멈춘 채 채널이 찼다.
        log.record(entry(1, "alice", "POST /vms"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let started = Instant::now();
        runtime.block_on(log.shutdown_within(Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(5));
        // 막힌 blocking 태스크가 남았으면 런타임을 떨어뜨릴 때 그것을 기다린다.
        let (dropped, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(runtime);
            let _ = dropped.send(());
        });
        let finished = done.recv_timeout(Duration::from_secs(5)).is_ok();
        open.send(()).unwrap();
        assert!(finished, "dropping the runtime waited for a blocked task");
    }
}
