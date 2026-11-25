//! 로그인, 사용자, API 토큰. 비밀번호 해시와 토큰 해시는 응답에 넣지 않는다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{AppendHeaders, IntoResponse, Response};
use orbivisor_cluster::ClusterError;
use orbivisor_cluster::types::Rejection;
use orbivisor_types::audit::AuditAuth;
use orbivisor_types::auth::{ApiToken, ApiTokenSpec, Role, User, UserSpec, external_name};
use orbivisor_types::authz::{MAX_USER_TOKENS, TokenScope};
use orbivisor_types::node::{FOLDERS, NodeSpec};
use orbivisor_types::realm::{LOCAL_REALM, RealmBackend, RealmSpec};
use orbivisor_types::resource::{Resource, ResourceSpec, validate_name};
use orbivisor_types::two_factor::TwoFactorSpec;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{
    Action, LEGACY_SESSION_COOKIE, Principal, SESSION_COOKIE, SESSION_TTL, cookie_value,
    dummy_hash, hash_password, new_token, session_cookie, verify_password,
};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::limit::ClientIp;
use crate::proxy::FromWebOrigin;
use crate::resources::{List, check_name, lookup};
use crate::state::AppState;

pub const MIN_PASSWORD_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct UserView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub role: Role,
    /// 외부 사용자의 영역. 로컬 사용자는 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub realm: Option<String>,
    /// 2단계 인증 요소(확인된 TOTP 또는 패스키)가 있다(계획 62)
    #[serde(default, skip_serializing_if = "is_false")]
    #[schema(default)]
    pub two_factor: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            name: u.name,
            revision: u.revision,
            role: u.spec.role,
            realm: u.spec.realm,
            two_factor: false,
        }
    }
}

/// 사용자 보기(2단계 인증 여부를 채운다)
pub(crate) fn view(cluster: &orbivisor_cluster::Cluster, user: User) -> Result<UserView, ApiError> {
    let on = cluster
        .get_by_name::<TwoFactorSpec>(&user.id.to_string())?
        .is_some_and(|t| t.spec.has_factor());
    Ok(UserView {
        two_factor: on,
        ..user.into()
    })
}

/// 로그인 응답: 사용자와, 둘째 단계가 남았으면 그 안내(쿠키는 대기 세션이다)
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LoginResponse {
    #[serde(flatten)]
    pub user: UserView,
    /// 둘째 단계가 남았다(`POST /auth/login/second-factor`, 강제 등록이면 `/auth/2fa/…`). 없으면 로그인이 끝났다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub second_factor: Option<crate::two_factor::SecondFactorPrompt>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct TokenView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub user: Uuid,
    /// 권한 범위(계획 62). 없으면 주인 권한 그대로다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub scope: Option<TokenScope>,
}

impl From<ApiToken> for TokenView {
    fn from(t: ApiToken) -> Self {
        Self {
            id: t.id,
            name: t.name,
            revision: t.revision,
            user: t.spec.user,
            scope: t.spec.scope,
        }
    }
}

fn check_password(password: &str) -> Result<(), ApiError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::bad_request(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    /// 영역 로그인이면 영역 안의 이름이다(`alice`로 로그인하면 사용자 `alice@<영역>`).
    pub username: String,
    pub password: String,
    /// 없거나 `local`이면 로컬 사용자
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub realm: Option<String>,
}

/// 세션 쿠키를 발급한다. 웹 UI가 쓴다. CLI는 로그인한 뒤 API 토큰을 만들어 쓴다.
/// 실패가 이어지면 잠시 막는다(429 `too_many_attempts`, `Retry-After`).
/// LDAP 영역 로그인은 디렉터리로 확인하고 외부 사용자 `<이름>@<영역>`을 만들거나 역할을 고친다.
/// 모르는 영역과 OIDC 영역(IdP에서 로그인한다)은 400, 맞는 역할이 없으면 403 `no_role`, 디렉터리에
/// 닿지 못하면 503 `realm_unavailable`.
/// 사용자에게 2단계 인증 요소가 있거나 정책이 요구하면(계획 62) 쿠키는 5분짜리 대기 세션이고 본문의
/// `second_factor`가 남은 단계를 알린다. 대기 세션으로는 둘째 단계·등록 경로만 쓴다(다른 경로는 401
/// `second_factor_required`).
#[utoipa::path(post, path = "/auth/login", tag = "auth", security(()),
    request_body = LoginRequest,
    responses((status = 200, body = LoginResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn login(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    FromWebOrigin(web_origin): FromWebOrigin,
    host: crate::two_factor::RequestHost,
    audit: AuditNote,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let user = match req.realm.as_deref() {
        Some(realm) if realm != LOCAL_REALM => login_realm(&state, ip, &audit, &req, realm).await?,
        _ => login_local(&state, ip, &audit, req).await?,
    };
    audit.principal(
        Principal {
            user: user.id,
            name: user.name.clone(),
            role: user.spec.role,
            token: None,
        },
        AuditAuth::Session,
    );
    let prompt = crate::two_factor::login_prompt(&state, &user, &host)?;
    let (pending, second_factor) = match prompt {
        Some((pending, prompt)) => (Some(pending), Some(prompt)),
        None => (None, None),
    };
    let session = state.create_session(&user, pending).await?;
    let cookies = session_set_cookie(&state, &session, SESSION_TTL.as_secs(), web_origin)?;
    let mut response = Json(LoginResponse {
        user: view(&state.cluster, user)?,
        second_factor,
    })
    .into_response();
    for cookie in cookies {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    Ok(response)
}

/// 사용자의 세션을 만들고 그 세션 쿠키(Set-Cookie 값 둘)를 돌려준다. `web_origin`은 요청이 허용 출처에서
/// 왔다는 뜻이다([`session_set_cookie`]).
pub(crate) async fn new_session(
    state: &AppState,
    user: &User,
    web_origin: bool,
) -> Result<[HeaderValue; 2], ApiError> {
    let session = state.create_session(user, None).await?;
    session_set_cookie(state, &session, SESSION_TTL.as_secs(), web_origin)
}

/// 세션 쿠키(Set-Cookie 값). 허용 출처(다른 사이트일 수 있는 웹 포털)의 요청이면 그 출처가 보내는 요청에도
/// 붙도록 `SameSite=None; Secure`, 아니면 `SameSite=Strict`다. 새 이름과 옛 이름을 같은 값·속성으로 둘 다
/// 쓴다(계획 74: 한 릴리스 동안 옛 판 노드와 세션을 나눈다, 다음 판에서 뺌).
fn session_set_cookie(
    state: &AppState,
    value: &str,
    max_age: u64,
    web_origin: bool,
) -> Result<[HeaderValue; 2], ApiError> {
    let same_site = if web_origin { "None" } else { "Strict" };
    let one = |name: &str| {
        let mut cookie =
            format!("{name}={value}; Path=/; HttpOnly; SameSite={same_site}; Max-Age={max_age}");
        if state.secure_cookies || web_origin {
            cookie.push_str("; Secure");
        }
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)
    };
    Ok([one(SESSION_COOKIE)?, one(LEGACY_SESSION_COOKIE)?])
}

/// 로컬 사용자의 비밀번호를 확인한다.
async fn login_local(
    state: &AppState,
    ip: std::net::IpAddr,
    audit: &AuditNote,
    req: LoginRequest,
) -> Result<User, ApiError> {
    // 이름 형식이 아닌 것은 감사 로그에 넣지 않는다(비밀번호를 잘못 넣었을 수 있다).
    let name_ok = validate_name(&req.username).is_ok();
    audit.detail(if name_ok {
        format!("username={}", req.username)
    } else {
        "username=<invalid>".into()
    });
    // 이름 형식이 아니면 그런 사용자는 있을 수 없다. 해시를 계산하지 않고 IP만 실패로 센다(긴 이름을
    // 기록에 넣지 않는다). 있을 수 없는 이름이라 응답 시간으로 드러나는 것도 없다.
    if !name_ok {
        state.attempts.begin_ip(ip)?;
        return Err(wrong_credentials());
    }
    // 막혀 있으면 해시를 계산하지 않는다. 이 시도는 미리 실패로 센다.
    let attempt = state.attempts.begin(&req.username, ip)?;
    let user = state.cluster.get_by_name::<UserSpec>(&req.username)?;
    // 사용자가 없어도 해시를 비교해 응답 시간으로 사용자 존재 여부를 알 수 없게 한다.
    let hash = match &user {
        Some(u) => u.spec.password_hash.clone(),
        None => dummy_hash().await,
    };
    let ok = verify_password(req.password, hash).await;
    // 없는 사용자도 실패다(begin이 이미 셌다).
    let Some(user) = user.filter(|_| ok) else {
        return Err(wrong_credentials());
    };
    state.attempts.success(&req.username, ip, attempt);
    Ok(user)
}

/// 영역의 디렉터리로 확인하고 외부 사용자를 만들거나 역할을 맞춘다. 시도 제한의 키는 외부 사용자 이름이다.
async fn login_realm(
    state: &AppState,
    ip: std::net::IpAddr,
    audit: &AuditNote,
    req: &LoginRequest,
    realm: &str,
) -> Result<User, ApiError> {
    // 영역 부분이 맞는 아무 이름으로 사용자 부분만 본다.
    let user_ok = external_name(&req.username, LOCAL_REALM).is_ok();
    audit.detail(format!(
        "username={} realm={}",
        if user_ok {
            req.username.as_str()
        } else {
            "<invalid>"
        },
        if validate_name(realm).is_ok() {
            realm
        } else {
            "<invalid>"
        },
    ));
    // OIDC 영역은 비밀번호를 받지 않는다. 이름 규칙(LDAP)과 관계없이 먼저 400으로 거절하고 시도로 세지 않는다.
    let stored = state.cluster.get_by_name::<RealmSpec>(realm)?;
    let ldap = match stored.as_ref().map(|r| &r.spec.backend) {
        Some(RealmBackend::Ldap(ldap)) => Some(ldap),
        Some(RealmBackend::Oidc(_)) => {
            return Err(ApiError::bad_request(format!(
                "realm {realm} signs in through its identity provider"
            )));
        }
        None => None,
    };
    let Ok(name) = external_name(&req.username, realm) else {
        state.attempts.begin_ip(ip)?;
        return Err(wrong_credentials());
    };
    let (Some(stored), Some(ldap)) = (&stored, ldap) else {
        return Err(ApiError::bad_request(format!("unknown realm {realm:?}")));
    };
    let attempt = state.attempts.begin(&name, ip)?;
    // 디렉터리에는 소문자로 바꾼 이름(외부 사용자 이름의 앞부분)을 묻는다.
    let username = &name[..name.len() - realm.len() - 1];
    let found = match crate::ldap::authenticate(ldap, username, &req.password).await {
        Ok(found) => found,
        Err(crate::ldap::LdapError::InvalidCredentials) => return Err(wrong_credentials()),
        Err(crate::ldap::LdapError::Unavailable(reason)) => {
            tracing::warn!(realm, %reason, "the directory of the realm is unavailable");
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "realm_unavailable",
                format!("the directory of realm {realm} is unavailable"),
            ));
        }
    };
    let role = stored.spec.roles.role_for(&found.groups).ok_or_else(|| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "no_role",
            "no role is mapped for this user",
        )
    })?;
    // 외부 사용자는 옛 노드가 모르는 필드(`realm`)를 쓴다.
    crate::cluster::require_current(&state.cluster)?;
    let user = upsert_external(state, &name, realm, role, None, &found.groups).await?;
    state.attempts.success(&name, ip, attempt);
    Ok(user)
}

/// 외부 사용자를 만들거나 역할을 고친다(역할이 바뀌면 옛 세션을 끊는다). 같으면 쓰지 않는다.
/// 같은 사용자의 로그인이 겹쳐 진 쪽은 다시 읽어 한 번 더 한다. LDAP·OIDC 로그인이 함께 쓴다.
/// OIDC는 `subject`(`"<iss> <sub>"`)를 준다: 만들 때 적고, 이미 다른 subject가 있으면 거절하고(같은 이름을
/// 다시 쓴 다른 사람), 없으면(옛 데이터) 적는다.
pub(crate) async fn upsert_external(
    state: &AppState,
    name: &str,
    realm: &str,
    role: Role,
    subject: Option<&str>,
    groups: &[String],
) -> Result<User, ApiError> {
    // 로그인 때 본 디렉터리 그룹(계획 62): 로컬 그룹의 외부 그룹 연결이 본다. 옛 노드는 이 칸을 모르므로 모든 노드가
    // `folders`를 알 때만 적는다(아니면 적지 않고 로그인은 그대로 된다).
    let groups: Vec<String> = if crate::cluster::missing_feature(&state.cluster, FOLDERS)
        .is_ok_and(|missing| missing.is_empty())
    {
        let mut g: Vec<String> = groups.iter().map(|g| g.trim().to_string()).collect();
        g.retain(|g| !g.is_empty());
        g.sort();
        g.dedup();
        g
    } else {
        Vec::new()
    };
    let mut retried = false;
    loop {
        let written = match state.cluster.get_by_name::<UserSpec>(name)? {
            Some(user)
                if subject.is_some()
                    && user.spec.subject.is_some()
                    && user.spec.subject.as_deref() != subject =>
            {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "subject_mismatch",
                    format!("subject mismatch: {name} belongs to another identity provider user"),
                ));
            }
            Some(user)
                if user.spec.role == role
                    && user.spec.external_groups == groups
                    && (subject.is_none() || user.spec.subject.as_deref() == subject) =>
            {
                return Ok(user);
            }
            Some(mut user) => {
                if user.spec.role != role {
                    user.spec.role = role;
                    user.spec.session_epoch += 1;
                }
                // 그룹이 바뀌어도 세션은 끊지 않는다(요청마다 색인을 본다).
                user.spec.external_groups = groups.clone();
                if let Some(subject) = subject {
                    user.spec.subject = Some(subject.into());
                }
                state
                    .cluster
                    .update_spec(
                        user.id,
                        Some(user.revision),
                        user.labels.clone(),
                        &user.spec,
                    )
                    .await
                    .map(|revision| User { revision, ..user })
            }
            None => {
                let user: User = Resource::new(
                    name,
                    UserSpec {
                        password_hash: String::new(),
                        role,
                        session_epoch: 0,
                        realm: Some(realm.into()),
                        subject: subject.map(str::to_string),
                        external_groups: groups.clone(),
                    },
                );
                state
                    .cluster
                    .create(&user)
                    .await
                    .map(|revision| User { revision, ..user })
            }
        };
        match written {
            Ok(user) => return Ok(user),
            Err(ClusterError::Rejected(
                Rejection::AlreadyExists { .. }
                | Rejection::NameTaken { .. }
                | Rejection::RevisionMismatch { .. },
            )) if !retried => retried = true,
            Err(e) => return Err(e.into()),
        }
    }
}

fn wrong_credentials() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        "wrong username or password",
    )
}

/// 세션을 끝내고(모든 노드에서) 브라우저의 세션 쿠키를 지운다. 클러스터가 세션을 지우지 못하면
/// (리더 없음 등) 5XX이고 쿠키는 그대로다.
#[utoipa::path(post, path = "/auth/logout", tag = "auth", security(()),
    responses((status = 204, description = "Logged out; the session cookie is cleared",
        headers(("set-cookie" = String))),
        (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn logout(
    ClientIp(ip): ClientIp,
    State(state): State<Arc<AppState>>,
    FromWebOrigin(web_origin): FromWebOrigin,
    audit: AuditNote,
    parts: Parts,
) -> Response {
    if let Some(id) = session_cookie(&parts.headers) {
        // 다른 사이트의 페이지가 로그아웃시키지 못하게 쿠키 요청과 같은 출처 검사를 한다.
        if let Err(e) = state.check_cookie_origin(&parts) {
            return e.into_response();
        }
        // 지우기 전에 세션의 사용자를 감사 로그에 남긴다.
        if let Some(user) = state.session_user(id)
            && let Ok(Some(principal)) = state.principal_for(user)
        {
            audit.principal(principal, AuditAuth::Session);
        }
        if let Err(e) = state.end_session(id, ip).await {
            return e.into_response();
        }
        // 옛 이름 쿠키가 다른 세션(옛 판 노드에서 따로 로그인한 것)이면 그것도 끝낸다(계획 74, 다음 판에서 뺌).
        if let Some(legacy) = cookie_value(&parts.headers, LEGACY_SESSION_COOKIE)
            && legacy != id
            && let Err(e) = state.end_session(legacy, ip).await
        {
            return e.into_response();
        }
    }
    match session_set_cookie(&state, "", 0, web_origin) {
        Ok([new, legacy]) => (
            StatusCode::NO_CONTENT,
            AppendHeaders([(SET_COOKIE, new), (SET_COOKIE, legacy)]),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /auth/whoami`의 응답: 주체에 요청을 받은 노드를 더한다(계획 62 리뷰 2 — 노드 목록을 읽지 못하는 폴더 사용자의
/// 화면이 "이 노드"를 안다). 옛 응답과 같은 칸에 `node`만 더해진다(노드 기록을 읽지 못하면 빠진다).
#[derive(Debug, Serialize, ToSchema)]
pub struct WhoAmI {
    pub user: Uuid,
    pub name: String,
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub node: Option<ServingNode>,
}

/// 요청을 받은 노드.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServingNode {
    pub id: Uuid,
    pub name: String,
}

#[utoipa::path(get, path = "/auth/whoami", tag = "auth",
    responses((status = 200, body = WhoAmI), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn whoami(State(state): State<Arc<AppState>>, principal: Principal) -> Json<WhoAmI> {
    let node = match state.cluster.get::<NodeSpec>(state.node) {
        Ok(Some(n)) => Some(ServingNode {
            id: n.id,
            name: n.name,
        }),
        _ => None,
    };
    Json(WhoAmI {
        user: principal.user,
        name: principal.name,
        role: principal.role,
        node,
    })
}

#[utoipa::path(get, path = "/users", tag = "auth",
    responses((status = 200, body = List<UserView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_users(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<UserView>>, ApiError> {
    state.authorize(&principal, Action::Read, UserSpec::KIND)?;
    let (revision, users) = state.cluster.list::<UserSpec>()?;
    let two_factor = crate::two_factor::enabled_users(&state.cluster)?;
    Ok(Json(List {
        revision,
        items: users
            .into_iter()
            .map(|u| UserView {
                two_factor: two_factor.contains(&u.id),
                ..u.into()
            })
            .collect(),
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUserRequest {
    pub name: String,
    pub password: String,
    pub role: Role,
}

#[utoipa::path(post, path = "/users", tag = "auth",
    request_body = CreateUserRequest,
    responses((status = 201, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    // 전역 역할 없음(`none`)은 옛 노드가 풀지 못한다.
    if req.role == Role::None {
        require_feature(&state.cluster, FOLDERS)?;
    }
    check_name(&req.name)?;
    check_password(&req.password)?;
    let mut user: User = Resource::new(
        req.name,
        UserSpec {
            password_hash: hash_password(req.password).await?,
            role: req.role,
            session_epoch: 0,
            realm: None,
            subject: None,
            external_groups: vec![],
        },
    );
    user.revision = state.cluster.create(&user).await?;
    Ok((StatusCode::CREATED, Json(user.into())))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub password: Option<String>,
    /// 자기 비밀번호를 바꿀 때 필요하다.
    #[serde(default)]
    pub current_password: Option<String>,
    #[serde(default)]
    pub role: Option<Role>,
}

/// 관리자는 누구의 비밀번호와 역할이든 바꾼다. 다른 사용자는 자기 비밀번호만 바꾼다.
/// 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고(훔친 세션으로 계정을 빼앗지 못하게),
/// 바꾸면 모든 노드에서 그 사용자의 세션이 끝난다(API 토큰은 남는다). 마지막 관리자는 역할을 내려놓을 수 없다.
/// 현재 비밀번호 확인은 로그인과 같은 시도 제한을 받는다.
#[utoipa::path(put, path = "/users/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = UpdateUserRequest,
    responses((status = 200, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    ClientIp(ip): ClientIp,
    Path(key): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<UserView>, ApiError> {
    // 관리 권한이 없으면 다른 사용자가 있는지조차 알려 주지 않는다.
    let may_write = state
        .authorize(&principal, Action::Write, UserSpec::KIND)
        .is_ok();
    if !may_write && key != principal.name && key != principal.user.to_string() {
        return Err(ApiError::forbidden());
    }
    let user = lookup::<UserSpec>(&state.cluster, &key)?;
    let is_self = user.id == principal.user;
    if !is_self || req.role.is_some() {
        state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    }
    if req.password.is_some() && user.spec.realm.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "external_user",
            "the password of an external user is managed by its realm",
        ));
    }
    let mut spec = user.spec.clone();
    let password_changed = req.password.is_some();
    if let Some(password) = req.password {
        check_password(&password)?;
        if is_self {
            let attempt = state.attempts.begin(&principal.name, ip)?;
            let current = req.current_password.unwrap_or_default();
            if !verify_password(current, user.spec.password_hash.clone()).await {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "invalid_credentials",
                    "the current password is wrong",
                ));
            }
            state.attempts.success(&principal.name, ip, attempt);
        }
        spec.password_hash = hash_password(password).await?;
        // 모든 노드의 옛 세션을 끊는다(쿠키 인증이 세대를 비교한다).
        spec.session_epoch += 1;
    }
    if let Some(role) = req.role {
        if role == Role::None {
            require_feature(&state.cluster, FOLDERS)?;
        }
        if user.spec.role == Role::Admin && role != Role::Admin {
            let (_, users) = state.cluster.list::<UserSpec>()?;
            let admins = users.iter().filter(|u| u.spec.role == Role::Admin).count();
            if admins <= 1 {
                return Err(ApiError::conflict(
                    "last_admin",
                    "the last admin cannot give up the admin role",
                ));
            }
        }
        spec.role = role;
    }
    state
        .cluster
        .update_spec(user.id, Some(user.revision), user.labels, &spec)
        .await?;
    // 세션 세대가 이미 옛 세션을 막는다. 지우지 못한 Raft 세션은 리더의 정리 작업이 지운다.
    if password_changed && let Err(e) = state.end_user_sessions(user.id).await {
        tracing::warn!(user = %user.name, error = %e.message, "old sessions not deleted");
    }
    Ok(Json(view(
        &state.cluster,
        lookup::<UserSpec>(&state.cluster, &user.id.to_string())?,
    )?))
}

/// 사용자와 그 사용자의 토큰을 지운다. 자기 자신은 지울 수 없다(마지막 관리자를 잃지 않게).
#[utoipa::path(delete, path = "/users/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    let user = lookup::<UserSpec>(&state.cluster, &key)?;
    if user.id == principal.user {
        return Err(ApiError::conflict(
            "cannot_delete_self",
            "you cannot delete your own account",
        ));
    }
    // 토큰은 요청마다 주인을 다시 읽으므로, 사용자를 먼저 지우면 남은 토큰도 바로 쓸 수 없다.
    state.cluster.delete::<UserSpec>(user.id, None).await?;
    let (_, tokens) = state.cluster.list::<ApiTokenSpec>()?;
    for token in tokens.into_iter().filter(|t| t.spec.user == user.id) {
        state.cluster.delete::<ApiTokenSpec>(token.id, None).await?;
    }
    // 2단계 인증 요소(계획 62). 같은 이름으로 다시 만든 사용자는 id가 달라 이어받지 않는다.
    if let Some(tf) = state
        .cluster
        .get_by_name::<TwoFactorSpec>(&user.id.to_string())?
    {
        match state.cluster.delete::<TwoFactorSpec>(tf.id, None).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
            Err(e) => return Err(e.into()),
        }
    }
    // 그 사용자의 grant와 그룹 멤버 자리(리뷰 2 N4). 지우지 못한 것은 리더의 정리 작업이 지운다.
    crate::permissions::drop_orphans(&state.cluster).await;
    // 세션도 주인을 다시 읽으므로 이미 쓸 수 없다. 지우지 못한 것은 리더의 정리 작업이 지운다.
    if let Err(e) = state.end_user_sessions(user.id).await {
        tracing::warn!(user = %user.name, error = %e.message, "sessions not deleted");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    pub name: String,
    /// 권한 범위(계획 62): 토큰의 권한은 주인 권한과 이 범위(`targets`마다 `privileges`를 건 propagate grant)의
    /// 교집합이다. 없으면 주인과 같다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub scope: Option<TokenScope>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedToken {
    pub token: TokenView,
    /// `Authorization: Bearer <secret>`로 쓴다. 다시 볼 수 없다.
    pub secret: String,
}

fn may_manage(state: &AppState, principal: &Principal, token: &ApiToken) -> bool {
    token.spec.user == principal.user
        || state
            .authorize(principal, Action::Write, ApiTokenSpec::KIND)
            .is_ok()
}

/// 요청한 사용자의 토큰을 만든다. 토큰의 권한은 주인의 역할을 따른다.
#[utoipa::path(post, path = "/tokens", tag = "auth",
    request_body = CreateTokenRequest,
    responses((status = 201, body = CreatedToken), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    check_name(&req.name)?;
    if let Some(scope) = &req.scope {
        // 옛 노드는 범위를 모르고 주인 권한 전부를 준다.
        require_feature(&state.cluster, FOLDERS)?;
        crate::permissions::check_privilege_features(&state.cluster, &scope.privileges)?;
        scope
            .validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
    }
    // 범위 있는 토큰이 범위 없는(더 넓은) 토큰을 만들지 못하게 범위 토큰으로는 토큰을 만들지 않는다.
    if let Some(id) = principal.token
        && state
            .cluster
            .get::<ApiTokenSpec>(id)?
            .is_some_and(|t| t.spec.scope.is_some())
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a scoped token cannot create tokens",
        ));
    }
    let owned = state
        .cluster
        .list::<ApiTokenSpec>()?
        .1
        .iter()
        .filter(|t| t.spec.user == principal.user)
        .count();
    if owned >= MAX_USER_TOKENS {
        return Err(ApiError::conflict(
            "too_many_tokens",
            format!("a user can have at most {MAX_USER_TOKENS} API tokens"),
        ));
    }
    let mut token: ApiToken = Resource::new(
        req.name,
        ApiTokenSpec {
            user: principal.user,
            secret_hash: String::new(),
            scope: req.scope,
        },
    );
    let (hash, secret) = new_token(token.id);
    token.spec.secret_hash = hash;
    token.revision = state.cluster.create(&token).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            token: token.into(),
            secret,
        }),
    ))
}

/// 자기 토큰. 관리자는 모든 토큰을 본다.
#[utoipa::path(get, path = "/tokens", tag = "auth",
    responses((status = 200, body = List<TokenView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_tokens(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<TokenView>>, ApiError> {
    let (revision, tokens) = state.cluster.list::<ApiTokenSpec>()?;
    let items = tokens
        .into_iter()
        .filter(|t| may_manage(&state, &principal, t))
        .map(TokenView::from)
        .collect();
    Ok(Json(List { revision, items }))
}

#[utoipa::path(delete, path = "/tokens/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    let token = lookup::<ApiTokenSpec>(&state.cluster, &key)?;
    // 남의 토큰은 있는지조차 알리지 않는다.
    if !may_manage(&state, &principal, &token) {
        return Err(ApiError::not_found(format!("api-token {key:?} not found")));
    }
    state.cluster.delete::<ApiTokenSpec>(token.id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
