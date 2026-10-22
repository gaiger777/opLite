//! 인증(로컬 사용자, 세션 쿠키, API 토큰)과 권한 확인.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN};
use axum::http::request::Parts;
use opl_types::auth::{ApiTokenSpec, Role, UserSpec};
use opl_types::resource::Kind;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "opl_session";
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
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
        let account = matches!(kind, Kind::User | Kind::ApiToken | Kind::JoinToken);
        match principal.role {
            Role::Admin => true,
            Role::Operator => match action {
                Action::Read => !account,
                Action::Write | Action::Operate => matches!(kind, Kind::Vm | Kind::Volume),
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

/// 노드 메모리에만 두는 세션. opld가 재시작하면 다시 로그인한다.
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<HashMap<String, (Uuid, Instant)>>,
}

impl Sessions {
    pub fn create(&self, user: Uuid) -> String {
        let id = random_hex(32);
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        map.retain(|_, (_, expires)| *expires > now);
        map.insert(id.clone(), (user, now + SESSION_TTL));
        id
    }

    pub fn get(&self, id: &str) -> Option<Uuid> {
        let map = self.inner.lock().unwrap();
        map.get(id)
            .filter(|(_, expires)| *expires > Instant::now())
            .map(|(user, _)| *user)
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    /// 사용자의 세션을 모두 끝낸다(비밀번호를 바꿨을 때).
    pub fn remove_user(&self, user: Uuid) {
        self.inner.lock().unwrap().retain(|_, (u, _)| *u != user);
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

/// `Origin`이 없거나(브라우저 밖의 클라이언트) `Host`와 같다.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let origin = origin.to_str().unwrap_or_default();
    let origin_host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    let host = headers.get(HOST).and_then(|h| h.to_str().ok());
    origin_host.is_some() && origin_host == host
}

impl AppState {
    fn principal_for(&self, user: Uuid) -> Result<Option<Principal>, ApiError> {
        Ok(self.cluster.get::<UserSpec>(user)?.map(|u| Principal {
            user: u.id,
            name: u.name,
            role: u.spec.role,
        }))
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
        let user = session_cookie(&parts.headers)
            .and_then(|id| self.sessions.get(id))
            .ok_or_else(ApiError::unauthorized)?;
        // 쿠키는 브라우저가 알아서 붙이므로, 다른 출처의 페이지가 보낸 요청(WebSocket 포함)은 받지 않는다.
        if !same_origin(&parts.headers) {
            return Err(ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "cross_origin",
                "cookie-authenticated requests must come from the same origin",
            ));
        }
        self.principal_for(user)?.ok_or_else(ApiError::unauthorized)
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
        state.authenticate(parts).await
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

        let op = principal(Role::Operator);
        assert!(a.check(&op, Action::Write, Kind::Vm));
        assert!(a.check(&op, Action::Operate, Kind::Vm));
        assert!(a.check(&op, Action::Write, Kind::Volume));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
        assert!(!a.check(&op, Action::Write, Kind::Node));
        assert!(!a.check(&op, Action::Read, Kind::ApiToken));

        let admin = principal(Role::Admin);
        assert!(a.check(&admin, Action::Write, Kind::User));
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
        let id = s.create(user);
        assert_eq!(s.get(&id), Some(user));
        s.remove(&id);
        assert_eq!(s.get(&id), None);
        assert_eq!(s.get("unknown"), None);

        let (a, b) = (s.create(user), s.create(user));
        let other = s.create(Uuid::now_v7());
        s.remove_user(user);
        assert_eq!((s.get(&a), s.get(&b)), (None, None));
        assert!(s.get(&other).is_some());
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
        assert!(same_origin(&headers(&[("host", "127.0.0.1:8006")])));
        assert!(same_origin(&headers(&[
            ("host", "127.0.0.1:8006"),
            ("origin", "http://127.0.0.1:8006")
        ])));
        assert!(!same_origin(&headers(&[
            ("host", "127.0.0.1:8006"),
            ("origin", "http://127.0.0.1:9000")
        ])));
        assert!(!same_origin(&headers(&[("origin", "null")])));
    }
}
