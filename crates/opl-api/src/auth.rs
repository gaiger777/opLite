//! 인증(로컬 사용자, 세션 쿠키, API 토큰)과 권한 확인.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN};
use axum::http::request::Parts;
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::auth::{ApiTokenSpec, Role, Session, SessionSpec, SessionStatus, User, UserSpec};
use opl_types::realm::RealmSpec;
use opl_types::resource::Kind;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "opl_session";
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// 세션의 마지막 사용 시각을 다시 적기까지의 간격. 요청마다 Raft에 쓰지 않으려고 묶는다.
pub const SESSION_TOUCH_INTERVAL: Duration = Duration::from_secs(60);
/// 모든 노드가 이 기능을 알면 세션을 Raft에 둔다(옛 노드는 `session` 종류를 풀지 못한다).
pub const SHARED_SESSIONS: &str = "shared-sessions";
/// 한 사용자의 Raft 세션 상한. 넘으면 가장 오래된 것부터 지운다.
pub const MAX_USER_SESSIONS: usize = 32;
/// 로그아웃 표시(다른 노드의 메모리 세션을 끝내려고 Raft에 남기는 만료된 `Session`)의 상한. 로그아웃은
/// 인증 없이 부르므로 아무 쿠키로 표시를 쌓지 못하게 한다(IP마다는 로그인 실패 제한으로 센다).
const MAX_LOGOUT_MARKS: usize = 2_000;
const TOKEN_PREFIX: &str = "opl_";

/// 인증된 요청의 주체. 요청마다 저장소의 User에서 다시 읽으므로 역할 변경과 삭제가 곧바로 반영된다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Principal {
    pub user: Uuid,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    /// 생성, 수정, 삭제
    Write,
    /// 시작·정지 같은 동작과 콘솔
    Operate,
}

/// 모든 핸들러가 거치는 권한 확인 훅(설계 4절). 엔터프라이즈 플러그인이 세분화된 RBAC로 바꾼다.
pub trait Authorizer: Send + Sync {
    fn check(&self, principal: &Principal, action: Action, kind: Kind) -> bool;
}

/// 고정 역할 3개. 자기 API 토큰은 역할과 관계없이 누구나 다룬다(토큰 핸들러가 따로 확인한다).
pub struct RoleAuthorizer;

impl Authorizer for RoleAuthorizer {
    fn check(&self, principal: &Principal, action: Action, kind: Kind) -> bool {
        let account = matches!(
            kind,
            Kind::User | Kind::ApiToken | Kind::JoinToken | Kind::Realm | Kind::Session
        );
        match principal.role {
            Role::Admin => true,
            Role::Operator => match action {
                Action::Read => !account,
                Action::Write | Action::Operate => {
                    matches!(
                        kind,
                        Kind::Vm | Kind::Volume | Kind::Backup | Kind::BackupJob | Kind::VmImport
                    )
                }
            },
            Role::Viewer => action == Action::Read && !account,
        }
    }
}

/// argon2id로 해시한다. CPU를 오래 쓰므로 블로킹 스레드에서 돈다.
pub async fn hash_password(password: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)
}

/// 해시가 깨졌으면 false다.
pub async fn verify_password(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false)
}

/// 존재하지 않는 사용자로 로그인할 때도 같은 시간이 걸리도록 비교에 쓰는 해시.
pub(crate) async fn dummy_hash() -> String {
    static HASH: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    HASH.get_or_init(|| async {
        hash_password(random_hex(16))
            .await
            .expect("hashing a random password")
    })
    .await
    .clone()
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system random source");
    hex::encode(buf)
}

pub(crate) fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// 새 토큰의 (비밀 값 해시, 사용자에게 한 번만 보여 줄 토큰 문자열)
pub(crate) fn new_token(id: Uuid) -> (String, String) {
    let secret = random_hex(32);
    let hash = sha256_hex(&secret);
    (hash, format!("{TOKEN_PREFIX}{}_{secret}", id.simple()))
}

/// `opl_<id>_<secret>`을 나눈다.
fn parse_token(token: &str) -> Option<(Uuid, &str)> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    Some((Uuid::try_parse(id).ok()?, secret))
}

/// 길이가 같은 두 문자열을 내용과 관계없이 같은 시간에 비교한다.
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// 이 노드 메모리의 세션 기록. 세션은 보통 Raft의 `Session` 리소스에 두고(모든 노드가 본다), 옛 노드가
/// 남은 클러스터(롤링 업그레이드 중)에서는 여기 둔다. 메모리 세션은 opld가 재시작하면 다시 로그인한다.
/// 만들 때 사용자의 세션 세대를 함께 둔다(다른 노드에서 비밀번호가 바뀌면 끊으려고).
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<HashMap<String, (Uuid, u64, Instant)>>,
    /// 이 노드가 마지막 사용 시각을 적으러 보낸 Raft 세션과 보낸 때
    touched: Mutex<HashMap<Uuid, Instant>>,
}

impl Sessions {
    pub fn create(&self, user: Uuid, epoch: u64) -> String {
        let id = random_hex(32);
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        map.retain(|_, (_, _, expires)| *expires > now);
        map.insert(id.clone(), (user, epoch, now + SESSION_TTL));
        id
    }

    /// (사용자, 만들 때의 세션 세대)
    pub fn get(&self, id: &str) -> Option<(Uuid, u64)> {
        let map = self.inner.lock().unwrap();
        map.get(id)
            .filter(|(_, _, expires)| *expires > Instant::now())
            .map(|(user, epoch, _)| (*user, *epoch))
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    /// 사용자의 세션을 모두 끝낸다(비밀번호를 바꿨을 때).
    pub fn remove_user(&self, user: Uuid) {
        self.inner.lock().unwrap().retain(|_, (u, _, _)| *u != user);
    }

    /// Raft 세션 `id`의 마지막 사용 시각을 지금 적을 차례면 참이고 보낸 것으로 적는다. 이 노드에서
    /// [`SESSION_TOUCH_INTERVAL`]에 한 번까지다(같은 세션의 동시 요청이 함께 쓰지 않게).
    fn touch_due(&self, id: Uuid, now: Instant) -> bool {
        let mut map = self.touched.lock().unwrap();
        map.retain(|_, at| now.duration_since(*at) < SESSION_TOUCH_INTERVAL);
        if map.contains_key(&id) {
            return false;
        }
        map.insert(id, now);
        true
    }
}

/// 쿠키로 찾은 세션
struct FoundSession {
    user: Uuid,
    /// 만들 때 사용자의 세션 세대
    epoch: u64,
    /// Raft 세션이면 그 리소스(메모리 세션은 None)
    shared: Option<Session>,
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 지우려던 것이 이미 없으면 지운 것으로 본다.
fn deleted(result: Result<u64, ClusterError>) -> Result<(), ApiError> {
    match result {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('='))
}

/// `Origin`이 없거나(브라우저 밖의 클라이언트) `Host`와 같다. 믿는 프록시가 scheme(`proto`)을 알렸으면
/// scheme도 같아야 한다.
pub(crate) fn same_origin(headers: &HeaderMap, proto: Option<&str>) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let origin = origin.to_str().unwrap_or_default();
    let Some((scheme, origin_host)) = origin
        .split_once("://")
        .filter(|(scheme, _)| matches!(*scheme, "http" | "https"))
    else {
        return false;
    };
    if proto.is_some_and(|p| p != scheme) {
        return false;
    }
    headers.get(HOST).and_then(|h| h.to_str().ok()) == Some(origin_host)
}

fn principal_of(u: User) -> Principal {
    Principal {
        user: u.id,
        name: u.name,
        role: u.spec.role,
    }
}

impl AppState {
    /// 사용자의 세션을 만들고 쿠키 값을 돌려준다. 모든 노드가 [`SHARED_SESSIONS`]를 알면 Raft에 두고
    /// (쿠키 값은 해시만), 아니면 이 노드 메모리에 둔다.
    /// 정족수가 없거나 쓰기가 리더에 닿지 못하면 이 노드 메모리에 둔다(장애 때도 관리자가 들어와 상태를 본다).
    pub(crate) async fn create_session(&self, user: &User) -> Result<String, ApiError> {
        if !crate::cluster::missing_feature(&self.cluster, SHARED_SESSIONS)?.is_empty() {
            return Ok(self.sessions.create(user.id, user.spec.session_epoch));
        }
        if !self.cluster.health().quorum {
            tracing::warn!(user = %user.name, "the cluster has no quorum; the session works only on this node");
            return Ok(self.sessions.create(user.id, user.spec.session_epoch));
        }
        let cookie = random_hex(32);
        let now = unix_now();
        self.limit_user_sessions(user.id).await?;
        let mut session = Session::new(
            sha256_hex(&cookie),
            SessionSpec {
                user: user.id,
                epoch: user.spec.session_epoch,
                expires_at: now + SESSION_TTL.as_secs(),
            },
        );
        session.status.last_used = now;
        // 이 노드에 적용될 때까지 기다리므로 곧바로 쓸 수 있다.
        match self.cluster.create(&session).await {
            Ok(_) => Ok(cookie),
            Err(
                e @ (ClusterError::Timeout
                | ClusterError::Unreachable(_)
                | ClusterError::NotLeader { .. }),
            ) => {
                // 쓰기가 나중에 커밋돼도 그 세션의 쿠키는 아무도 모르고, 정리 작업이 만료 뒤 지운다.
                tracing::warn!(user = %user.name, error = %e, "cannot write the session to the cluster; it works only on this node");
                Ok(self.sessions.create(user.id, user.spec.session_epoch))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// 쿠키의 세션. 메모리에 없으면 Raft에서 찾는다. 끝난 세션은 없는 것과 같다. 메모리 세션도 같은
    /// 이름의 끝난 Raft 세션(다른 노드의 로그아웃 표시)이 있으면 끝난 것이다.
    fn find_session(&self, cookie: &str) -> Result<Option<FoundSession>, ApiError> {
        if let Some((user, epoch)) = self.sessions.get(cookie) {
            if self
                .cluster
                .get_by_name::<SessionSpec>(&sha256_hex(cookie))?
                .is_some_and(|s| s.spec.expires_at <= unix_now())
            {
                self.sessions.remove(cookie);
                return Ok(None);
            }
            return Ok(Some(FoundSession {
                user,
                epoch,
                shared: None,
            }));
        }
        Ok(self
            .cluster
            .get_by_name::<SessionSpec>(&sha256_hex(cookie))?
            .filter(|s| s.spec.expires_at > unix_now())
            .map(|s| FoundSession {
                user: s.spec.user,
                epoch: s.spec.epoch,
                shared: Some(s),
            }))
    }

    /// 쿠키 세션의 사용자(감사 로그용). 없거나 읽지 못하면 None
    pub(crate) fn session_user(&self, cookie: &str) -> Option<Uuid> {
        self.find_session(cookie).ok().flatten().map(|s| s.user)
    }

    /// Raft 세션의 마지막 사용 시각을 가끔 적는다. 기다리지 않는다(그사이 지워졌으면 그대로 둔다).
    fn touch_session(&self, session: &Session) {
        let now = unix_now();
        if now < session.status.last_used + SESSION_TOUCH_INTERVAL.as_secs()
            || !self.sessions.touch_due(session.id, Instant::now())
        {
            return;
        }
        let cluster = self.cluster.clone();
        let id = session.id;
        tokio::spawn(async move {
            let status = SessionStatus { last_used: now };
            if let Err(e) = cluster.update_status::<SessionSpec>(id, &status).await {
                tracing::debug!(session = %id, error = %e, "session last use not recorded");
            }
        });
    }

    /// 쿠키의 세션을 끝낸다(메모리와 Raft 모두). 어디에도 없으면 다른 노드의 메모리 세션일 수 있으므로
    /// 로그아웃 표시를 남긴다([`AppState::mark_logged_out`]). `ip`는 요청한 클라이언트다.
    pub(crate) async fn end_session(&self, cookie: &str, ip: IpAddr) -> Result<(), ApiError> {
        let local = self.sessions.get(cookie).is_some();
        self.sessions.remove(cookie);
        match self
            .cluster
            .get_by_name::<SessionSpec>(&sha256_hex(cookie))?
        {
            Some(s) if s.spec.expires_at > unix_now() => {
                deleted(self.cluster.delete::<SessionSpec>(s.id, None).await)?;
            }
            // 이미 끝났다(로그아웃 표시는 정리 작업이 때가 되면 지운다).
            Some(_) => {}
            None if !local => self.mark_logged_out(cookie, ip).await,
            None => {}
        }
        Ok(())
    }

    /// 다른 노드의 메모리 세션(옛 노드가 남았거나 정족수가 없을 때 만든 것)을 끝내려고, 쿠키 해시 이름으로
    /// 이미 끝난 `Session`(주인 없음, 만료 지금)을 Raft에 쓴다. 그 노드는 메모리 세션을 쓸 때마다 이것을
    /// 보고 거절한다. 쓰지 못하면(정족수 없음 등) 경고만 한다. 표시를 쓰는 로그아웃은 클라이언트 IP의
    /// 로그인 실패로 세고, 막혔으면 쓰지 않는다(로그아웃은 그래도 성공으로 답해 알려 주지 않는다).
    async fn mark_logged_out(&self, cookie: &str, ip: IpAddr) {
        // 이 노드가 만드는 쿠키 모양(32바이트 hex)만
        if cookie.len() != 64 || !cookie.bytes().all(|b| b.is_ascii_hexdigit()) {
            return;
        }
        let shared = crate::cluster::missing_feature(&self.cluster, SHARED_SESSIONS)
            .is_ok_and(|missing| missing.is_empty());
        if !shared {
            return;
        }
        if !self.cluster.health().quorum {
            tracing::warn!("the cluster has no quorum; a session on another node is not ended");
            return;
        }
        let marks = self.cluster.list::<SessionSpec>().map(|(_, sessions)| {
            sessions
                .iter()
                .filter(|s| s.spec.user == Uuid::nil())
                .count()
        });
        if marks.as_ref().is_ok_and(|n| *n >= MAX_LOGOUT_MARKS) {
            tracing::warn!("too many logout marks; a session on another node is not ended");
            return;
        }
        if self.attempts.begin_ip(ip).is_err() {
            tracing::debug!(%ip, "too many logouts of unknown sessions; no logout mark");
            return;
        }
        let mark = Session::new(
            sha256_hex(cookie),
            SessionSpec {
                user: Uuid::nil(),
                epoch: 0,
                expires_at: unix_now(),
            },
        );
        match self.cluster.create(&mark).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NameTaken { .. })) => {}
            Err(e) => {
                tracing::warn!(error = %e, "cannot mark the session logged out; a session on another node is not ended");
            }
        }
    }

    /// 사용자의 Raft 세션이 [`MAX_USER_SESSIONS`]개 이상이면 가장 오래된 것부터 지워 새 세션 자리를
    /// 만든다. 지우지 못하면 경고만 한다.
    async fn limit_user_sessions(&self, user: Uuid) -> Result<(), ApiError> {
        let (_, sessions) = self.cluster.list::<SessionSpec>()?;
        let mut mine: Vec<Session> = sessions
            .into_iter()
            .filter(|s| s.spec.user == user)
            .collect();
        if mine.len() < MAX_USER_SESSIONS {
            return Ok(());
        }
        // 만료 시각이 이른 것이 먼저 만든 것이다.
        mine.sort_by_key(|s| s.spec.expires_at);
        let extra = mine.len() + 1 - MAX_USER_SESSIONS;
        for s in mine.into_iter().take(extra) {
            if let Err(e) = deleted(self.cluster.delete::<SessionSpec>(s.id, None).await) {
                tracing::warn!(%user, error = ?e, "cannot delete an old session");
            }
        }
        Ok(())
    }

    /// 사용자의 세션을 모두 끝낸다(비밀번호를 바꿨거나 사용자를 지웠을 때).
    pub(crate) async fn end_user_sessions(&self, user: Uuid) -> Result<(), ApiError> {
        self.sessions.remove_user(user);
        let (_, sessions) = self.cluster.list::<SessionSpec>()?;
        for s in sessions.into_iter().filter(|s| s.spec.user == user) {
            deleted(self.cluster.delete::<SessionSpec>(s.id, None).await)?;
        }
        Ok(())
    }

    pub(crate) fn principal_for(&self, user: Uuid) -> Result<Option<Principal>, ApiError> {
        match self.cluster.get::<UserSpec>(user)? {
            Some(u) if self.realm_exists(&u)? => Ok(Some(principal_of(u))),
            _ => Ok(None),
        }
    }

    /// 외부 사용자의 영역이 있다(로컬 사용자는 늘 참). 영역을 지우면 그 사용자의 세션과 토큰은
    /// 멈추고, 같은 이름으로 다시 만들면 다시 쓸 수 있다.
    fn realm_exists(&self, user: &User) -> Result<bool, ApiError> {
        match &user.spec.realm {
            Some(realm) => Ok(self.cluster.get_by_name::<RealmSpec>(realm)?.is_some()),
            None => Ok(true),
        }
    }

    async fn authenticate(&self, parts: &Parts) -> Result<Principal, ApiError> {
        if let Some(value) = parts.headers.get(AUTHORIZATION) {
            let token = value
                .to_str()
                .ok()
                .and_then(|v| v.strip_prefix("Bearer "))
                .ok_or_else(ApiError::unauthorized)?;
            let (id, secret) = parse_token(token.trim()).ok_or_else(ApiError::unauthorized)?;
            let stored = self
                .cluster
                .get::<ApiTokenSpec>(id)?
                .ok_or_else(ApiError::unauthorized)?;
            if !constant_time_eq(&sha256_hex(secret), &stored.spec.secret_hash) {
                return Err(ApiError::unauthorized());
            }
            return self
                .principal_for(stored.spec.user)?
                .ok_or_else(ApiError::unauthorized);
        }
        let session = session_cookie(&parts.headers).ok_or_else(ApiError::unauthorized)?;
        let found = self
            .find_session(session)?
            .ok_or_else(ApiError::unauthorized)?;
        self.check_cookie_origin(parts)?;
        let user = self
            .cluster
            .get::<UserSpec>(found.user)?
            .ok_or_else(ApiError::unauthorized)?;
        // 세션을 만든 뒤 비밀번호가 바뀌었다(다른 노드에서 바뀌었을 수도 있다). Raft 세션은 리더의
        // 정리 작업이 지운다.
        if user.spec.session_epoch != found.epoch {
            self.sessions.remove(session);
            return Err(ApiError::unauthorized());
        }
        if !self.realm_exists(&user)? {
            return Err(ApiError::unauthorized());
        }
        if let Some(shared) = &found.shared {
            self.touch_session(shared);
        }
        Ok(principal_of(user))
    }

    /// 쿠키는 브라우저가 알아서 붙이므로, 다른 출처의 페이지가 보낸 쿠키 요청(WebSocket 포함)은 받지 않는다
    /// (403). 허용 출처(클러스터 설정 `web_origins`)는 받는다. 허용 출처를 쓰면 쿠키가 `SameSite=None`일 수
    /// 있으므로, `Origin`이 없는 쓰기 요청·WebSocket 업그레이드는 `Sec-Fetch-Site`가 다른 사이트라고 알리면
    /// 받지 않는다(둘 다 없는 브라우저 밖의 클라이언트는 받는다).
    pub(crate) fn check_cookie_origin(&self, parts: &Parts) -> Result<(), ApiError> {
        let cross = || {
            ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "cross_origin",
                "cookie-authenticated requests must come from the same origin",
            )
        };
        if !same_origin(&parts.headers, crate::proxy::forwarded_proto(parts))
            && !crate::proxy::from_web_origin(parts)
        {
            return Err(cross());
        }
        if crate::proxy::unsafe_without_origin(parts)
            && !crate::settings::current(&self.cluster)?
                .spec
                .web_origins
                .is_empty()
        {
            return Err(cross());
        }
        Ok(())
    }

    /// 권한이 없으면 403
    pub fn authorize(
        &self,
        principal: &Principal,
        action: Action,
        kind: Kind,
    ) -> Result<(), ApiError> {
        if self.authorizer.check(principal, action, kind) {
            Ok(())
        } else {
            Err(ApiError::forbidden())
        }
    }
}

impl FromRequestParts<Arc<AppState>> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let mut principal = state.authenticate(parts).await;
        // `Orbivisor-Min-Revision`의 리비전까지 기다렸으면 다시 인증한다(다른 노드에서 막 만든 자격 증명은 받고,
        // 기다리는 사이 폐기된 것은 받지 않는다).
        let waited = crate::revision::wait_for(state, parts, principal.is_ok()).await;
        if let Ok(true) = waited {
            principal = state.authenticate(parts).await;
        }
        crate::audit::note_auth(parts, principal.as_ref().ok());
        let principal = principal?;
        crate::revision::authenticated(parts);
        waited?;
        Ok(principal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(role: Role) -> Principal {
        Principal {
            user: Uuid::nil(),
            name: "u".into(),
            role,
        }
    }

    #[test]
    fn roles_grant_expected_actions() {
        let a = RoleAuthorizer;
        let viewer = principal(Role::Viewer);
        assert!(a.check(&viewer, Action::Read, Kind::Vm));
        assert!(!a.check(&viewer, Action::Operate, Kind::Vm));
        assert!(!a.check(&viewer, Action::Read, Kind::User));
        assert!(!a.check(&viewer, Action::Read, Kind::Realm));

        let op = principal(Role::Operator);
        assert!(a.check(&op, Action::Write, Kind::Vm));
        assert!(a.check(&op, Action::Operate, Kind::Vm));
        assert!(a.check(&op, Action::Write, Kind::Volume));
        assert!(a.check(&op, Action::Write, Kind::Backup));
        assert!(a.check(&op, Action::Write, Kind::BackupJob));
        assert!(a.check(&op, Action::Write, Kind::VmImport));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
        assert!(!a.check(&op, Action::Write, Kind::Node));
        assert!(a.check(&op, Action::Read, Kind::HaGroup));
        assert!(!a.check(&op, Action::Write, Kind::HaGroup));
        assert!(!a.check(&op, Action::Write, Kind::PlacementRule));
        assert!(a.check(&viewer, Action::Read, Kind::PlacementRule));
        assert!(!a.check(&op, Action::Read, Kind::ApiToken));
        assert!(!a.check(&op, Action::Read, Kind::Realm));
        assert!(!a.check(&op, Action::Write, Kind::Realm));

        let admin = principal(Role::Admin);
        assert!(a.check(&admin, Action::Write, Kind::User));
        assert!(a.check(&admin, Action::Read, Kind::Realm));
        assert!(a.check(&admin, Action::Write, Kind::Realm));
    }

    #[test]
    fn token_round_trips_and_rejects_garbage() {
        let id = Uuid::now_v7();
        let (hash, token) = new_token(id);
        let (parsed, secret) = parse_token(&token).unwrap();
        assert_eq!(parsed, id);
        assert_eq!(sha256_hex(secret), hash);
        for bad in ["", "opl_", "opl_xyz_abc", "xyz", &token[4..]] {
            assert!(parse_token(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn compares_in_constant_time_shape() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }

    #[tokio::test]
    async fn verifies_passwords() {
        let hash = hash_password("correct horse".into()).await.unwrap();
        assert!(verify_password("correct horse".into(), hash.clone()).await);
        assert!(!verify_password("wrong".into(), hash).await);
        assert!(!verify_password("x".into(), "not a hash".into()).await);
    }

    #[test]
    fn sessions_can_be_removed() {
        let s = Sessions::default();
        let user = Uuid::now_v7();
        let id = s.create(user, 7);
        assert_eq!(s.get(&id), Some((user, 7)));
        s.remove(&id);
        assert_eq!(s.get(&id), None);
        assert_eq!(s.get("unknown"), None);

        let (a, b) = (s.create(user, 0), s.create(user, 0));
        let other = s.create(Uuid::now_v7(), 0);
        s.remove_user(user);
        assert_eq!((s.get(&a), s.get(&b)), (None, None));
        assert!(s.get(&other).is_some());
    }

    #[test]
    fn session_use_is_recorded_at_most_once_per_interval() {
        let s = Sessions::default();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let start = Instant::now();
        assert!(s.touch_due(a, start));
        assert!(!s.touch_due(a, start + Duration::from_secs(1)));
        assert!(s.touch_due(b, start + Duration::from_secs(1)));
        assert!(s.touch_due(a, start + SESSION_TOUCH_INTERVAL));
    }

    #[test]
    fn checks_origin_against_host() {
        fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.insert(*k, v.parse().unwrap());
            }
            h
        }
        assert!(same_origin(&headers(&[("host", "127.0.0.1:8006")]), None));
        let same = headers(&[
            ("host", "127.0.0.1:8006"),
            ("origin", "http://127.0.0.1:8006"),
        ]);
        assert!(same_origin(&same, None));
        // 믿는 프록시가 알린 scheme과도 맞아야 한다.
        assert!(same_origin(&same, Some("http")));
        assert!(!same_origin(&same, Some("https")));
        assert!(!same_origin(
            &headers(&[
                ("host", "127.0.0.1:8006"),
                ("origin", "http://127.0.0.1:9000")
            ]),
            None
        ));
        assert!(!same_origin(&headers(&[("origin", "null")]), None));
        assert!(!same_origin(
            &headers(&[("host", "a"), ("origin", "ftp://a")]),
            None
        ));
    }
}
