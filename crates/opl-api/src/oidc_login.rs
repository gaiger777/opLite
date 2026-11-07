//! OIDC 로그인 흐름의 끝점. `start`는 로그인 상태 쿠키를 놓고 IdP로 보내고, `callback`은 쿠키를
//! 확인한 뒤 코드를 토큰으로 바꾸고 ID 토큰을 확인해 외부 사용자의 세션을 만든다. 상태(리디렉트 URI 포함)가
//! 쿠키에 있으므로 어느 노드나 콜백을 받는다. 실패는 원인을 로그에 남기고 `/login?error=<코드>`로
//! 돌려보낸다. 콜백은 시도 제한을 쓰지 않는다: 맞힐 비밀이 없고(state는 서명됐고 코드는 한 번 쓰며 PKCE에
//! 묶인다), 같은 NAT·프록시 뒤의 정상 실패가 모두의 비밀번호 로그인을 막지 않게 한다.

use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::header::{COOKIE, HOST, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use opl_types::audit::AuditAuth;
use opl_types::auth::{User, external_name_oidc};
use opl_types::realm::{OidcRealm, RealmBackend, RealmSpec, RoleMapping};
use serde::Deserialize;
use serde_json::{Map, Value};
use utoipa::IntoParams;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::error::ApiError;
use crate::oidc::{self, LoginState, OidcError, RealmKey, STATE_COOKIE, STATE_TTL_SECS};
use crate::state::AppState;

/// 콜백 경로. 노드 주소 앞에 붙여 IdP에 리디렉트 URI로 등록한다.
pub const CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";
/// 로그인 상태 쿠키의 Path
const STATE_COOKIE_PATH: &str = "/api/v1/auth/oidc";

/// 로그인 화면에 알리는 실패 코드
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Code {
    SsoFailed,
    NoRole,
    RealmUnavailable,
}

impl Code {
    fn as_str(self) -> &'static str {
        match self {
            Code::SsoFailed => "sso_failed",
            Code::NoRole => "no_role",
            Code::RealmUnavailable => "realm_unavailable",
        }
    }
}

/// 실패 코드와 로그에 남길 원인
#[derive(Debug)]
struct Failed {
    code: Code,
    reason: String,
}

fn failed(code: Code, reason: impl Into<String>) -> Failed {
    Failed {
        code,
        reason: reason.into(),
    }
}

/// IdP에 닿지 못했으면 `realm_unavailable`, 그 밖은 `sso_failed`
impl From<OidcError> for Failed {
    fn from(e: OidcError) -> Self {
        match e {
            OidcError::Unavailable(reason) => failed(Code::RealmUnavailable, reason),
            OidcError::Invalid(reason) => failed(Code::SsoFailed, reason),
        }
    }
}

impl From<ApiError> for Failed {
    fn from(e: ApiError) -> Self {
        failed(Code::SsoFailed, e.message)
    }
}

/// 302 응답과 Set-Cookie들
fn redirect(location: &str, cookies: impl IntoIterator<Item = HeaderValue>) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    let headers = response.headers_mut();
    match HeaderValue::from_str(location) {
        Ok(location) => headers.insert(LOCATION, location),
        // Url이 만든 주소라 일어나지 않는다.
        Err(_) => headers.insert(
            LOCATION,
            HeaderValue::from_static("/login?error=sso_failed"),
        ),
    };
    for cookie in cookies {
        headers.append(SET_COOKIE, cookie);
    }
    response
}

/// 로그인 상태 쿠키(값이 비고 `max_age`가 0이면 지우기)
fn state_cookie(state: &AppState, value: &str, max_age: u64) -> HeaderValue {
    let mut cookie = format!(
        "{STATE_COOKIE}={value}; Path={STATE_COOKIE_PATH}; HttpOnly; SameSite=Lax; Max-Age={max_age}"
    );
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).expect("ASCII cookie")
}

/// 이 요청이 온 주소의 콜백 URI. scheme은 `secure_cookies`면 https, 아니면 http(시험)다.
/// Host 헤더가 없거나 호스트·포트 글자가 아닌 것이 있으면 None.
fn redirect_uri(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let host = headers.get(HOST)?.to_str().ok()?;
    let valid = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    if !valid {
        return None;
    }
    let scheme = if state.secure_cookies {
        "https"
    } else {
        "http"
    };
    Some(format!("{scheme}://{host}{CALLBACK_PATH}"))
}

fn no_host() -> ApiError {
    ApiError::bad_request("the request has no valid Host header")
}

/// 이름의 OIDC 영역: 캐시 키, 설정, 역할 매핑
fn oidc_realm(state: &AppState, name: &str) -> Result<(RealmKey, OidcRealm, RoleMapping), Failed> {
    let realm = state
        .cluster
        .get_by_name::<RealmSpec>(name)
        .map_err(|e| failed(Code::SsoFailed, e.to_string()))?
        .ok_or_else(|| failed(Code::SsoFailed, format!("there is no realm {name:?}")))?;
    match realm.spec.backend {
        RealmBackend::Oidc(oidc) => Ok(((realm.id, realm.revision), oidc, realm.spec.roles)),
        RealmBackend::Ldap(_) => Err(failed(
            Code::SsoFailed,
            format!("realm {name} is not an OIDC realm"),
        )),
    }
}

/// 상태 쿠키의 HMAC 키(클러스터 CA 개인키에서)
fn state_key(state: &AppState) -> Result<ring::hmac::Key, Failed> {
    let info = crate::cluster::cluster_info(state)?;
    Ok(oidc::state_key(&info.spec.ca_key_pem))
}

/// 영역의 IdP로 로그인을 시작한다. 로그인 상태(state·nonce·PKCE 검증자)를 서명한 쿠키
/// `opl_oidc`(10분)에 넣고 IdP의 authorization endpoint로 보낸다. OIDC 영역이 아니면
/// `/login?error=sso_failed`, IdP의 discovery 문서를 읽지 못하면 `/login?error=realm_unavailable`로 보낸다.
#[utoipa::path(get, path = "/auth/oidc/{realm}/start", tag = "auth", security(()),
    params(("realm" = String, Path, description = "OIDC realm name")),
    responses(
        (status = 302, description = "To the identity provider with the login state cookie, or to /login?error=<code>",
            headers(("location" = String), ("set-cookie" = String))),
        (status = 400, description = "The request has no valid Host header", body = crate::error::ErrorBody)))]
pub async fn oidc_start(
    State(state): State<Arc<AppState>>,
    Path(realm): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let redirect_uri = redirect_uri(&state, &headers).ok_or_else(no_host)?;
    match begin(&state, &realm, &redirect_uri).await {
        Ok((location, cookie)) => Ok(redirect(
            &location,
            [state_cookie(&state, &cookie, STATE_TTL_SECS)],
        )),
        Err(f) => {
            tracing::warn!(realm, reason = %f.reason, "cannot start an OIDC login");
            Ok(redirect(&format!("/login?error={}", f.code.as_str()), []))
        }
    }
}

/// authorization endpoint 주소와 상태 쿠키 값
async fn begin(
    state: &AppState,
    realm: &str,
    redirect_uri: &str,
) -> Result<(String, String), Failed> {
    let (key, oidc, _) = oidc_realm(state, realm)?;
    let discovery = state
        .oidc
        .discovery(key, &oidc)
        .await
        .map_err(|e| failed(Code::RealmUnavailable, e.to_string()))?;
    let cookie_key = state_key(state)?;
    let login = LoginState::new(realm, redirect_uri, oidc::unix_now());
    let mut url = reqwest::Url::parse(&discovery.authorization_endpoint).map_err(|e| {
        failed(
            Code::RealmUnavailable,
            format!("authorization_endpoint: {e}"),
        )
    })?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &oidc.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &oidc.scopes.join(" "))
        .append_pair("state", &login.state)
        .append_pair("nonce", &login.nonce)
        .append_pair("code_challenge", &oidc::code_challenge(&login.verifier))
        .append_pair("code_challenge_method", "S256");
    Ok((url.into(), login.seal(&cookie_key)))
}

/// IdP가 돌려보낸 콜백의 쿼리
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CallbackParams {
    /// 인가 코드
    pub code: Option<String>,
    /// 로그인 상태 쿠키의 state와 같아야 한다.
    pub state: Option<String>,
    /// IdP가 로그인을 거절한 이유(RFC 6749 §4.1.2.1)
    pub error: Option<String>,
    pub error_description: Option<String>,
    /// 인가 응답을 낸 IdP(RFC 9207). 있으면 영역의 issuer와 같아야 하고, IdP가 지원한다고 하면 있어야 한다.
    pub iss: Option<String>,
}

/// IdP가 돌려보내는 곳. 로그인 상태 쿠키(서명·10분·state 같음)를 확인하고, 인가 응답의 `iss`(RFC 9207)를
/// 보고, 코드를 토큰으로 바꾸고(리디렉트 URI는 상태 쿠키의 것), ID 토큰(서명·iss·aud·exp·iat·nbf·nonce)을
/// 확인한 뒤 이름·그룹 클레임(없으면 userinfo)으로 외부 사용자 `<이름>@<영역>`을 만들거나 역할을 고쳐
/// 세션 쿠키를 놓고 `/`로 보낸다. 외부 사용자는 처음 만든 IdP 사용자(iss·sub)에 묶인다. 실패하면
/// `/login?error=<sso_failed|no_role|realm_unavailable>`로 보낸다.
#[utoipa::path(get, path = "/auth/oidc/callback", tag = "auth", security(()),
    params(CallbackParams),
    responses(
        (status = 302, description = "To / with the session cookie, or to /login?error=<code>",
            headers(("location" = String), ("set-cookie" = String)))))]
pub async fn oidc_callback(
    State(state): State<Arc<AppState>>,
    audit: AuditNote,
    headers: HeaderMap,
    params: Result<Query<CallbackParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    let (login, params) = match open_state(&state, &headers, params) {
        Ok(opened) => opened,
        Err(f) => return Ok(failure(&state, &audit, None, f)),
    };
    match complete(&state, &login, &params).await {
        Ok((user, username)) => {
            audit.principal(
                Principal {
                    user: user.id,
                    name: user.name.clone(),
                    role: user.spec.role,
                },
                AuditAuth::Session,
            );
            audit.detail(format!("realm={} username={username}", login.realm));
            let session = crate::users::new_session(&state, &user)?;
            Ok(redirect("/", [session, state_cookie(&state, "", 0)]))
        }
        Err(f) => Ok(failure(&state, &audit, Some(&login.realm), f)),
    }
}

/// 쿼리와 상태 쿠키(서명·10분·state 같음)를 확인한다.
fn open_state(
    state: &AppState,
    headers: &HeaderMap,
    params: Result<Query<CallbackParams>, QueryRejection>,
) -> Result<(LoginState, CallbackParams), Failed> {
    let Query(params) = params.map_err(|e| failed(Code::SsoFailed, e.body_text()))?;
    let cookie = state_cookie_value(headers)
        .ok_or_else(|| failed(Code::SsoFailed, "there is no login state cookie"))?;
    let query_state = params.state.as_deref().unwrap_or_default();
    let login = LoginState::open(cookie, &state_key(state)?, query_state, oidc::unix_now())?;
    Ok((login, params))
}

fn state_cookie_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(STATE_COOKIE)?.strip_prefix('='))
}

/// 실패를 로그·감사 로그에 남기고 상태 쿠키를 지운 뒤 로그인 화면으로 보낸다.
fn failure(state: &AppState, audit: &AuditNote, realm: Option<&str>, f: Failed) -> Response {
    let realm = realm.unwrap_or("<unknown>");
    tracing::warn!(realm, code = f.code.as_str(), reason = %f.reason, "an OIDC login failed");
    audit.detail(format!("realm={realm} error={}", f.code.as_str()));
    redirect(
        &format!("/login?error={}", f.code.as_str()),
        [state_cookie(state, "", 0)],
    )
}

/// 확인한 로그인 상태로 로그인을 끝낸다. 사용자와 영역 안의 이름(소문자)을 돌려준다.
async fn complete(
    state: &AppState,
    login: &LoginState,
    params: &CallbackParams,
) -> Result<(User, String), Failed> {
    if let Some(error) = &params.error {
        return Err(failed(
            Code::SsoFailed,
            format!(
                "the identity provider returned {error:?}: {:?}",
                params.error_description.as_deref().unwrap_or_default()
            ),
        ));
    }
    let code = params
        .code
        .as_deref()
        .filter(|c| !c.is_empty())
        .ok_or_else(|| failed(Code::SsoFailed, "the callback has no code"))?;
    let realm = login.realm.as_str();
    let (key, oidc, roles) = oidc_realm(state, realm)?;
    // start처럼 discovery 실패(문서가 맞지 않음 포함)는 realm_unavailable
    let discovery = state
        .oidc
        .discovery(key, &oidc)
        .await
        .map_err(|e| failed(Code::RealmUnavailable, e.to_string()))?;
    // RFC 9207: 다른 IdP의 인가 응답을 이 영역의 것으로 받지 않는다(mix-up).
    match params.iss.as_deref() {
        Some(iss) if iss != oidc.issuer => {
            return Err(failed(
                Code::SsoFailed,
                format!("the authorization response is from issuer {iss:?}"),
            ));
        }
        None if discovery.authorization_response_iss_parameter_supported => {
            return Err(failed(
                Code::SsoFailed,
                "the authorization response has no iss",
            ));
        }
        _ => {}
    }
    let client = oidc::client(&oidc)?;
    let tokens = oidc::exchange_code(
        &client,
        &oidc,
        &discovery.token_endpoint,
        code,
        &login.redirect_uri,
        &login.verifier,
    )
    .await?;
    let mut claims = state
        .oidc
        .verify_id_token(key, &oidc, &discovery, &tokens.id_token, &login.nonce)
        .await?;
    // 이름이나 그룹이 ID 토큰에 없으면 userinfo에서 한 번 읽어 채운다.
    let missing = |claims: &Map<String, Value>| {
        !claims.contains_key(&oidc.username_claim) || !claims.contains_key(&oidc.groups_claim)
    };
    if missing(&claims)
        && let (Some(url), Some(token)) = (&discovery.userinfo_endpoint, &tokens.access_token)
    {
        let userinfo = oidc::get_json(&client, url, Some(token)).await?;
        if userinfo.get("sub") != claims.get("sub") {
            return Err(failed(
                Code::SsoFailed,
                "the userinfo sub is not the ID token sub",
            ));
        }
        if let Value::Object(userinfo) = userinfo {
            for name in [&oidc.username_claim, &oidc.groups_claim] {
                if !claims.contains_key(name)
                    && let Some(value) = userinfo.get(name)
                {
                    claims.insert(name.clone(), value.clone());
                }
            }
        }
    }
    let username = match claims.get(&oidc.username_claim) {
        Some(Value::String(name)) => name.as_str(),
        Some(_) => {
            return Err(failed(
                Code::SsoFailed,
                format!("the {} claim is not a string", oidc.username_claim),
            ));
        }
        None => {
            return Err(failed(
                Code::SsoFailed,
                format!("there is no {} claim", oidc.username_claim),
            ));
        }
    };
    let groups: Vec<String> = match claims.get(&oidc.groups_claim) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| not_strings(&oidc))?,
        Some(_) => return Err(not_strings(&oidc)),
    };
    let name = external_name_oidc(username, realm).map_err(|e| {
        failed(
            Code::SsoFailed,
            format!("the username {username:?} cannot be used: {e}"),
        )
    })?;
    let role = roles.role_for(&groups).ok_or_else(|| {
        failed(
            Code::NoRole,
            format!("no role is mapped for {name} (groups {groups:?})"),
        )
    })?;
    // IdP의 사용자. 이름을 다시 쓴 다른 사람이 이 사용자로 로그인하지 못하게 묶는다.
    let sub = claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let subject = format!("{} {sub}", oidc.issuer);
    // 외부 사용자는 옛 노드가 모르는 필드(`realm`·`subject`)를 쓴다.
    crate::cluster::require_current(&state.cluster)?;
    let user = crate::users::upsert_external(state, &name, realm, role, Some(&subject)).await?;
    let username = name[..name.len() - realm.len() - 1].to_string();
    Ok((user, username))
}

fn not_strings(oidc: &OidcRealm) -> Failed {
    failed(
        Code::SsoFailed,
        format!("the {} claim is not an array of strings", oidc.groups_claim),
    )
}
