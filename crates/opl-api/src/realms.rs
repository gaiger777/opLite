//! 인증 영역 `/api/v1/realms`와 로그인 화면이 읽는 `/api/v1/auth/realms`. 영역은 계정 종류처럼
//! admin만 읽고 바꾼다. 서비스 계정 비밀번호는 응답에 넣지 않고 `has_bind_password`만 보인다.
//! 영역은 watch로도 내보내지 않는다(비밀번호를 담고 있다).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::realm::{
    LdapRealm, Realm, RealmBackend, RealmSpec, RoleMapping, validate_realm_name,
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

/// 응답의 영역. 서비스 계정 비밀번호 대신 있는지만 보인다.
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

impl From<Realm> for RealmView {
    fn from(r: Realm) -> Self {
        let RealmBackend::Ldap(l) = r.spec.backend;
        Self {
            id: r.id,
            name: r.name,
            revision: r.revision,
            display: r.spec.display,
            backend: RealmBackendView::Ldap(LdapRealmView {
                url: l.url,
                bind_dn: l.bind_dn,
                base_dn: l.base_dn,
                user_filter: l.user_filter,
                group_attr: l.group_attr,
                ca_pem: l.ca_pem,
                timeout_secs: l.timeout_secs,
                has_bind_password: l.bind_password.is_some(),
            }),
            roles: r.spec.roles,
        }
    }
}

/// 요청을 저장할 spec으로. `stored`는 바꿀 때의 지금 값이다(빠진 비밀번호를 채운다).
fn spec_of(req: &RealmRequest, stored: Option<&RealmSpec>) -> Result<RealmSpec, ApiError> {
    let RealmBackend::Ldap(mut ldap) = req.backend.clone();
    // 저장·비교 전에 다듬는다: 끝 줄바꿈이 있는 CA 파일과 웹이 다듬어 보낸 같은 CA를 같은 값으로 본다.
    ldap.url = ldap.url.trim().to_string();
    ldap.bind_dn = ldap.bind_dn.map(|dn| dn.trim().to_string());
    ldap.ca_pem = ldap
        .ca_pem
        .map(|pem| pem.replace("\r\n", "\n").trim().to_string());
    ldap.bind_password = match ldap.bind_password.take() {
        Some(p) if p.is_empty() => None,
        Some(p) => Some(p),
        None => match stored.map(stored_ldap) {
            Some(old) if old.bind_password.is_some() => {
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
    let spec = RealmSpec {
        display: req.display.clone().filter(|d| !d.trim().is_empty()),
        backend: RealmBackend::Ldap(ldap),
        roles: req.roles.clone(),
    };
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(spec)
}

fn stored_ldap(spec: &RealmSpec) -> &LdapRealm {
    let RealmBackend::Ldap(ldap) = &spec.backend;
    ldap
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

/// 로그인 화면에 보일 영역의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RealmKind {
    /// 로컬 사용자
    Local,
    Ldap,
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
}
