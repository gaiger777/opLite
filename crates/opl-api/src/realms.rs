//! 인증 영역 `/api/v1/realms`와 로그인 화면이 읽는 `/api/v1/auth/realms`. 영역은 계정 종류처럼
//! admin만 읽고 바꾼다. 서비스 계정 비밀번호와 OIDC 클라이언트 비밀은 응답에 넣지 않고
//! `has_bind_password`·`has_client_secret`만 보인다. 영역은 watch로도 내보내지 않는다(비밀을 담고 있다).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::auth::Role;
use opl_types::realm::{
    LdapRealm, OidcRealm, Realm, RealmBackend, RealmSpec, RoleMapping, validate_realm_name,
};
use opl_types::resource::{Resource, ResourceSpec};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::{List, check_name, if_match, lookup};
use crate::state::AppState;

/// 영역 만들기·바꾸기 본문. 바꾸기는 통째로 바꾸되, `backend.bind_password`를 빼면 저장된 값을
/// 그대로 두고(`url`·`bind_dn`·`ca_pem`이 모두 그대로일 때만 — 하나라도 바뀌면 400), 빈 글이면
/// 지운다. `bind_dn`에는 비밀번호가 있어야 한다(익명 검색은 둘 다 없을 때). `url`·`bind_dn`은 앞뒤
/// 공백을 떼고 `ca_pem`은 CRLF를 LF로 바꿔 앞뒤 공백을 뗀 뒤 비교·저장한다.
/// OIDC 영역의 `backend.client_secret`도 같다: 빼면 `issuer`·`client_id`·`ca_pem`이 모두 그대로일 때만
/// 저장된 값을 두고(저장된 비밀이 없으면 공개 클라이언트로 남는다), 빈 글이면 지운다. `issuer`·`client_id`는
/// 앞뒤 공백을 떼고 `ca_pem`은 LDAP처럼 다듬는다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RealmRequest {
    /// 만들 때 필요하다. 바꿀 때 주면 지금 이름과 같아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub name: Option<String>,
    /// 로그인 화면에 보일 이름
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub display: Option<String>,
    pub backend: RealmBackend,
    #[serde(default)]
    pub roles: RoleMapping,
}

/// 응답의 영역. 서비스 계정 비밀번호·클라이언트 비밀 대신 있는지만 보인다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub display: Option<String>,
    pub backend: RealmBackendView,
    pub roles: RoleMapping,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RealmBackendView {
    Ldap(LdapRealmView),
    Oidc(OidcRealmView),
}

/// [`LdapRealm`]에서 `bind_password`를 뺀 것
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LdapRealmView {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub bind_dn: Option<String>,
    pub base_dn: String,
    pub user_filter: String,
    pub group_attr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub ca_pem: Option<String>,
    pub timeout_secs: u64,
    /// 서비스 계정 비밀번호가 저장되어 있는지
    pub has_bind_password: bool,
}

/// [`OidcRealm`]에서 `client_secret`을 뺀 것
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OidcRealmView {
    pub issuer: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub username_claim: String,
    pub groups_claim: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub ca_pem: Option<String>,
    pub timeout_secs: u64,
    /// 클라이언트 비밀이 저장되어 있는지(없으면 공개 클라이언트)
    pub has_client_secret: bool,
}

impl From<Realm> for RealmView {
    fn from(r: Realm) -> Self {
        let backend = match r.spec.backend {
            RealmBackend::Ldap(l) => RealmBackendView::Ldap(LdapRealmView {
                url: l.url,
                bind_dn: l.bind_dn,
                base_dn: l.base_dn,
                user_filter: l.user_filter,
                group_attr: l.group_attr,
                ca_pem: l.ca_pem,
                timeout_secs: l.timeout_secs,
                has_bind_password: l.bind_password.is_some(),
            }),
            RealmBackend::Oidc(o) => RealmBackendView::Oidc(OidcRealmView {
                issuer: o.issuer,
                client_id: o.client_id,
                scopes: o.scopes,
                username_claim: o.username_claim,
                groups_claim: o.groups_claim,
                ca_pem: o.ca_pem,
                timeout_secs: o.timeout_secs,
                has_client_secret: o.client_secret.is_some(),
            }),
        };
        Self {
            id: r.id,
            name: r.name,
            revision: r.revision,
            display: r.spec.display,
            backend,
            roles: r.spec.roles,
        }
    }
}

/// 요청을 저장할 spec으로. `stored`는 바꿀 때의 지금 값이다(빠진 비밀번호를 채운다).
fn spec_of(req: &RealmRequest, stored: Option<&RealmSpec>) -> Result<RealmSpec, ApiError> {
    let backend = match req.backend.clone() {
        RealmBackend::Ldap(ldap) => RealmBackend::Ldap(ldap_of(ldap, stored)?),
        RealmBackend::Oidc(oidc) => RealmBackend::Oidc(oidc_of(oidc, stored)?),
    };
    let spec = RealmSpec {
        display: req.display.clone().filter(|d| !d.trim().is_empty()),
        backend,
        roles: req.roles.clone(),
    };
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(spec)
}

/// CRLF를 LF로 바꾸고 앞뒤 공백을 뗀다.
fn trim_pem(pem: Option<String>) -> Option<String> {
    pem.map(|pem| pem.replace("\r\n", "\n").trim().to_string())
}

fn ldap_of(mut ldap: LdapRealm, stored: Option<&RealmSpec>) -> Result<LdapRealm, ApiError> {
    // 저장·비교 전에 다듬는다: 끝 줄바꿈이 있는 CA 파일과 웹이 다듬어 보낸 같은 CA를 같은 값으로 본다.
    ldap.url = ldap.url.trim().to_string();
    ldap.bind_dn = ldap.bind_dn.map(|dn| dn.trim().to_string());
    ldap.ca_pem = trim_pem(ldap.ca_pem);
    ldap.bind_password = match ldap.bind_password.take() {
        Some(p) if p.is_empty() => None,
        Some(p) => Some(p),
        None => match stored.map(|s| &s.backend) {
            Some(RealmBackend::Ldap(old)) if old.bind_password.is_some() => {
                // 저장된 비밀번호를 다른 서버·계정으로 보내지 않는다.
                if old.url != ldap.url || old.bind_dn != ldap.bind_dn || old.ca_pem != ldap.ca_pem {
                    return Err(ApiError::bad_request(
                        "bind_password must be given again when url, bind_dn or ca_pem changes",
                    ));
                }
                old.bind_password.clone()
            }
            _ => None,
        },
    };
    Ok(ldap)
}

fn oidc_of(mut oidc: OidcRealm, stored: Option<&RealmSpec>) -> Result<OidcRealm, ApiError> {
    oidc.issuer = oidc.issuer.trim().to_string();
    oidc.client_id = oidc.client_id.trim().to_string();
    oidc.ca_pem = trim_pem(oidc.ca_pem);
    oidc.client_secret = match oidc.client_secret.take() {
        Some(s) if s.is_empty() => None,
        Some(s) => Some(s),
        None => match stored.map(|s| &s.backend) {
            Some(RealmBackend::Oidc(old)) if old.client_secret.is_some() => {
                // 저장된 비밀을 다른 IdP·클라이언트로 보내지 않는다.
                if old.issuer != oidc.issuer
                    || old.client_id != oidc.client_id
                    || old.ca_pem != oidc.ca_pem
                {
                    return Err(ApiError::bad_request(
                        "client_secret must be given again when issuer, client_id or ca_pem changes",
                    ));
                }
                old.client_secret.clone()
            }
            _ => None,
        },
    };
    Ok(oidc)
}

#[utoipa::path(get, path = "/realms", tag = "auth",
    responses((status = 200, body = List<RealmView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_realms(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<RealmView>>, ApiError> {
    state.authorize(&principal, Action::Read, RealmSpec::KIND)?;
    let (revision, realms) = state.cluster.list::<RealmSpec>()?;
    Ok(Json(List {
        revision,
        items: realms.into_iter().map(RealmView::from).collect(),
    }))
}

/// 이름은 리소스 이름 규칙이고 `local`은 쓸 수 없다. 나쁜 값은 400 `invalid_request`.
#[utoipa::path(post, path = "/realms", tag = "auth",
    request_body = RealmRequest,
    responses((status = 201, body = RealmView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<RealmRequest>,
) -> Result<(StatusCode, Json<RealmView>), ApiError> {
    state.authorize(&principal, Action::Write, RealmSpec::KIND)?;
    let name = req
        .name
        .clone()
        .ok_or_else(|| ApiError::bad_request("name is required"))?;
    check_name(&name)?;
    validate_realm_name(&name).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let spec = spec_of(&req, None)?;
    // 옛 노드는 새 종류를 풀지 못한다.
    crate::cluster::require_current(&state.cluster)?;
    let mut realm: Realm = Resource::new(name, spec);
    realm.revision = state.cluster.create(&realm).await?;
    Ok((StatusCode::CREATED, Json(realm.into())))
}

#[utoipa::path(get, path = "/realms/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = RealmView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<RealmView>, ApiError> {
    state.authorize(&principal, Action::Read, RealmSpec::KIND)?;
    Ok(Json(lookup::<RealmSpec>(&state.cluster, &key)?.into()))
}

/// 영역을 통째로 바꾼다(`bind_password` 규칙은 [`RealmRequest`]). `If-Match`가 없으면 읽은
/// revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
#[utoipa::path(put, path = "/realms/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = RealmRequest,
    responses((status = 200, body = RealmView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RealmRequest>,
) -> Result<Json<RealmView>, ApiError> {
    state.authorize(&principal, Action::Write, RealmSpec::KIND)?;
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let realm = lookup::<RealmSpec>(&state.cluster, &key)?;
        if let Some(name) = &req.name
            && *name != realm.name
        {
            return Err(ApiError::bad_request(
                "the name of a realm cannot be changed",
            ));
        }
        if let Some(expected) = expected
            && expected != realm.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: realm.revision,
            })
            .into());
        }
        let spec = spec_of(&req, Some(&realm.spec))?;
        if spec == realm.spec {
            return Ok(Json(realm.into()));
        }
        crate::cluster::require_current(&state.cluster)?;
        match state
            .cluster
            .update_spec(realm.id, Some(realm.revision), realm.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        return Ok(Json(
            lookup::<RealmSpec>(&state.cluster, &realm.id.to_string())?.into(),
        ));
    }
    Err(ApiError::conflict(
        "busy",
        "the realm kept changing while being updated; try again",
    ))
}

#[utoipa::path(delete, path = "/realms/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, RealmSpec::KIND)?;
    let realm = lookup::<RealmSpec>(&state.cluster, &key)?;
    crate::cluster::require_current(&state.cluster)?;
    state
        .cluster
        .delete::<RealmSpec>(realm.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 영역 시험 본문. LDAP 영역은 사용자 이름과 비밀번호가 필요하다. OIDC 영역은 본문 없이 부르고
/// (있으면 무시한다) IdP의 discovery와 JWKS만 확인한다.
#[derive(Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RealmTestRequest {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub username: Option<String>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub password: Option<String>,
}

// 비밀번호가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for RealmTestRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealmTestRequest")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

/// 영역 시험의 결과. 단계는 LDAP이면 `connect`, `service_bind`, `search`, `user_bind`, `groups`,
/// OIDC면 `discovery`, `jwks` 순이고 실패한 단계에서 멈춘다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmTestResult {
    pub steps: Vec<RealmTestStep>,
    /// 찾은 사용자 항목. 찾기 전에 멈췄거나 OIDC 영역이면 null
    #[schema(required = true)]
    pub dn: Option<String>,
    /// 모든 단계가 성공했을 때의 그룹 속성 값들(OIDC 영역은 비어 있다)
    pub groups: Vec<String>,
    /// 그룹에서 정해질 역할. 실패했거나 맞는 매핑이 없거나 OIDC 영역이면 null
    #[schema(required = true)]
    pub role: Option<Role>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmTestStep {
    pub name: String,
    pub ok: bool,
    pub message: String,
}

/// 영역을 시험한다. LDAP 영역은 사용자 이름과 비밀번호로 디렉터리를 확인하고, OIDC 영역은 IdP의
/// discovery 문서와 서명 키를 받아 본다. 사용자도 세션도 만들지 않는다.
#[utoipa::path(post, path = "/realms/{key}/actions/test", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = Option<RealmTestRequest>,
    responses((status = 200, body = RealmTestResult), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn test_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    req: Option<Json<Option<RealmTestRequest>>>,
) -> Result<Json<RealmTestResult>, ApiError> {
    state.authorize(&principal, Action::Operate, RealmSpec::KIND)?;
    let realm = lookup::<RealmSpec>(&state.cluster, &key)?;
    // 본문이 없거나 null이면 빈 요청
    let req = req.and_then(|Json(req)| req).unwrap_or_default();
    match &realm.spec.backend {
        RealmBackend::Ldap(_) => {
            let (Some(username), Some(password)) = (&req.username, &req.password) else {
                return Err(ApiError::bad_request(
                    "username and password are required to test an LDAP realm",
                ));
            };
            Ok(Json(
                crate::ldap::test(&realm.spec, username, password).await,
            ))
        }
        RealmBackend::Oidc(oidc) => Ok(Json(crate::oidc::test(oidc).await)),
    }
}

/// 로그인 화면에 보일 영역의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RealmKind {
    /// 로컬 사용자
    Local,
    Ldap,
    Oidc,
}

/// 로그인 화면에 보일 영역
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmSummary {
    pub name: String,
    /// 없으면 null
    #[schema(required = true)]
    pub display: Option<String>,
    pub kind: RealmKind,
}

/// 로그인할 수 있는 영역. 로그인 전에 읽는다. `local`이 먼저, 나머지는 이름 순이다.
#[utoipa::path(get, path = "/auth/realms", tag = "auth", security(()),
    responses((status = 200, body = Vec<RealmSummary>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_login_realms(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RealmSummary>>, ApiError> {
    let (_, mut realms) = state.cluster.list::<RealmSpec>()?;
    realms.sort_by(|a, b| a.name.cmp(&b.name));
    let local = RealmSummary {
        name: opl_types::realm::LOCAL_REALM.into(),
        display: None,
        kind: RealmKind::Local,
    };
    let others = realms.into_iter().map(|r| RealmSummary {
        name: r.name,
        display: r.spec.display,
        kind: match r.spec.backend {
            RealmBackend::Ldap(_) => RealmKind::Ldap,
            RealmBackend::Oidc(_) => RealmKind::Oidc,
        },
    });
    Ok(Json(std::iter::once(local).chain(others).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_debug_hides_the_bind_password() {
        let req: RealmRequest = serde_json::from_value(serde_json::json!({
            "name": "corp",
            "backend": {
                "kind": "ldap",
                "url": "ldaps://h",
                "bind_dn": "cn=svc",
                "bind_password": "s3cret",
                "base_dn": "dc=x"
            }
        }))
        .unwrap();
        let shown = format!("{req:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains("***"), "{shown}");
    }

    #[test]
    fn test_request_debug_hides_the_password() {
        let req = RealmTestRequest {
            username: Some("alice".into()),
            password: Some("s3cret".into()),
        };
        let shown = format!("{req:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains("alice"), "{shown}");
    }
}
