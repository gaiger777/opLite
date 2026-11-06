//! 로그인, 사용자, API 토큰. 비밀번호 해시와 토큰 해시는 응답에 넣지 않는다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::audit::AuditAuth;
use opl_types::auth::{ApiToken, ApiTokenSpec, Role, User, UserSpec, external_name};
use opl_types::realm::{LOCAL_REALM, RealmBackend, RealmSpec};
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{
    Action, Principal, SESSION_COOKIE, SESSION_TTL, dummy_hash, hash_password, new_token,
    session_cookie, verify_password,
};
use crate::error::{ApiError, Json};
use crate::limit::ClientIp;
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
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            name: u.name,
            revision: u.revision,
            role: u.spec.role,
            realm: u.spec.realm,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct TokenView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub user: Uuid,
}

impl From<ApiToken> for TokenView {
    fn from(t: ApiToken) -> Self {
        Self {
            id: t.id,
            name: t.name,
            revision: t.revision,
            user: t.spec.user,
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
/// 모르는 영역은 400, 맞는 역할이 없으면 403 `no_role`, 디렉터리에 닿지 못하면 503 `realm_unavailable`.
#[utoipa::path(post, path = "/auth/login", tag = "auth", security(()),
    request_body = LoginRequest,
    responses((status = 200, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn login(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
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
        },
        AuditAuth::Session,
    );
    let session = state.sessions.create(user.id, user.spec.session_epoch);
    let mut cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
    let mut response = Json(UserView::from(user)).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    Ok(response)
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
    let Ok(name) = external_name(&req.username, realm) else {
        state.attempts.begin_ip(ip)?;
        return Err(wrong_credentials());
    };
    let stored = state
        .cluster
        .get_by_name::<RealmSpec>(realm)?
        .ok_or_else(|| ApiError::bad_request(format!("unknown realm {realm:?}")))?;
    let attempt = state.attempts.begin(&name, ip)?;
    let RealmBackend::Ldap(ldap) = &stored.spec.backend;
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
    let user = upsert_external(state, &name, realm, role).await?;
    state.attempts.success(&name, ip, attempt);
    Ok(user)
}

/// 외부 사용자를 만들거나 역할을 고친다(역할이 바뀌면 옛 세션을 끊는다). 같으면 쓰지 않는다.
/// 같은 사용자의 로그인이 겹쳐 진 쪽은 다시 읽어 한 번 더 한다.
async fn upsert_external(
    state: &AppState,
    name: &str,
    realm: &str,
    role: Role,
) -> Result<User, ApiError> {
    let mut retried = false;
    loop {
        let written = match state.cluster.get_by_name::<UserSpec>(name)? {
            Some(user) if user.spec.role == role => return Ok(user),
            Some(mut user) => {
                user.spec.role = role;
                user.spec.session_epoch += 1;
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

/// 세션을 끝내고 브라우저의 세션 쿠키를 지운다.
#[utoipa::path(post, path = "/auth/logout", tag = "auth", security(()),
    responses((status = 204, description = "Logged out; the session cookie is cleared",
        headers(("set-cookie" = String)))))]
pub async fn logout(
    State(state): State<Arc<AppState>>,
    audit: AuditNote,
    headers: HeaderMap,
) -> Response {
    if let Some(id) = session_cookie(&headers) {
        // 지우기 전에 세션의 사용자를 감사 로그에 남긴다.
        if let Some((user, _)) = state.sessions.get(id)
            && let Ok(Some(principal)) = state.principal_for(user)
        {
            audit.principal(principal, AuditAuth::Session);
        }
        state.sessions.remove(id);
    }
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
    (
        StatusCode::NO_CONTENT,
        [(
            SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("ASCII cookie"),
        )],
    )
        .into_response()
}

#[utoipa::path(get, path = "/auth/whoami", tag = "auth",
    responses((status = 200, body = Principal), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn whoami(principal: Principal) -> Json<Principal> {
    Json(principal)
}

#[utoipa::path(get, path = "/users", tag = "auth",
    responses((status = 200, body = List<UserView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_users(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<UserView>>, ApiError> {
    state.authorize(&principal, Action::Read, UserSpec::KIND)?;
    let (revision, users) = state.cluster.list::<UserSpec>()?;
    Ok(Json(List {
        revision,
        items: users.into_iter().map(UserView::from).collect(),
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
    check_name(&req.name)?;
    check_password(&req.password)?;
    let mut user: User = Resource::new(
        req.name,
        UserSpec {
            password_hash: hash_password(req.password).await?,
            role: req.role,
            session_epoch: 0,
            realm: None,
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
        .authorizer
        .check(&principal, Action::Write, UserSpec::KIND);
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
    if password_changed {
        state.sessions.remove_user(user.id);
    }
    Ok(Json(
        lookup::<UserSpec>(&state.cluster, &user.id.to_string())?.into(),
    ))
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
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    pub name: String,
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
            .authorizer
            .check(principal, Action::Write, ApiTokenSpec::KIND)
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
    let mut token: ApiToken = Resource::new(
        req.name,
        ApiTokenSpec {
            user: principal.user,
            secret_hash: String::new(),
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
