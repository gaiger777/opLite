# opLite 계획 36: OIDC SSO 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 회사 IdP(Keycloak, Okta, Entra ID 같은 OpenID Connect 제공자)로 opLite 웹에 한 번에 로그인하게 한다. 관리자가 OIDC 영역(issuer, client ID·비밀, 클레임, 그룹 → 역할)을 만들면 로그인 화면에 "<영역>으로 로그인" 버튼이 생기고, IdP에서 인증한 사용자는 계획 35의 외부 사용자(`alice@okta`)로 들어온다. oplctl은 웹에서 만든 API 토큰으로 쓴다(`login --token-file`). 하위 프로젝트 10의 세 번째 계획이다.

**Architecture:**
- **흐름:** 인가 코드 흐름 + PKCE(S256)·state·nonce. 로그인 상태는 서버 메모리가 아니라 브라우저 쿠키(`opl_oidc`)에 두고 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한다 — 어느 노드로 돌아와도 끝낼 수 있고 새 저장 값이 없다.
- **ID 토큰:** `ring`으로 직접 확인(RS256·ES256만). discovery·JWKS는 노드 메모리에 캐시한다. 새 크레이트 없음.
- **사용자:** 계획 35의 외부 사용자 경로를 쓰고, IdP의 `sub`에 묶는다(이름 클레임이 다른 사람에게 다시 주어져도 계정을 넘겨받지 못하게). 이메일 모양 이름도 된다.
- **옛 노드 호환:** 새 종류·필드를 쓰기 전에 `require_current`.

**Tech Stack:** Rust 2024(MSRV 1.88), axum·utoipa, `ring`(HKDF·HMAC·서명 확인)·`base64`·`reqwest`(이미 `Cargo.lock`에 있는 판), tokio, clap. 웹은 React·TypeScript·react-query·vitest·msw. 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-06-enterprise-design.md` (§4와 §4.5 "구현(계획 36)")
**선행 계획:** 계획 1~35 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| OIDC 영역, 인가 코드 흐름(PKCE), ID 토큰 확인, `sub` 묶기 | SAML, IdP 로그아웃(opLite 세션만 끝냄) |
| 로그인 화면 SSO 버튼, OIDC 영역 화면과 리디렉트 URI | oplctl의 브라우저 SSO 로그인(웹에서 만든 토큰을 `login --token-file`로) |
| oplctl `realm add-oidc|set-oidc`, `login --token-file` | 원격 클러스터(계획 37), 원격 복제(계획 38) |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 Rust·웹 변경과 시험. OIDC 시험은 127.0.0.1의 가짜 IdP(시험 CA의 HTTPS)로 한다.
- **실제 IdP:** Keycloak·Okta·Entra ID 설정 메모는 각 제품 문서로 확인해야 한다(README).

## 설계 결정

- **쿠키 상태:** 콜백은 Host를 보지 않고 서명한 상태의 리디렉트 URI를 쓴다. 콜백은 시도 제한을 쓰지 않는다(맞힐 값이 없고, NAT 뒤의 정상 실패가 모두의 로그인을 막지 않게).
- **오류 리디렉트:** 실패는 `/login?error=<sso_failed|no_role|realm_unavailable>`. 원인은 로그에만.
- **비밀 옮기기:** client secret은 issuer·client ID·CA가 그대로일 때만 생략 유지(계획 35와 같은 규칙).
- **사용자 이름:** OIDC는 `[a-z0-9._+-]`와 안쪽 `@` 하나(1~128자)까지. `%`는 URL에서 다른 이름으로 풀릴 수 있어 넣지 않는다.

## 파일 구조

```
crates/opl-types/src/{realm.rs, auth.rs}
crates/opl-api/src/{oidc.rs, oidc_login.rs, realms.rs, users.rs, audit.rs, error.rs, state.rs, lib.rs, …}, crates/opl-api/tests/{oidc.rs, oidc_login.rs, realms.rs, two_nodes.rs, common/idp.rs, fixtures/oidc-rsa.pk8, …}
crates/oplctl/src/{cli.rs, client.rs, console.rs}
web/src/{pages/LoginPage.tsx, realms/RealmsPage.tsx, realms/realms.ts, App.test.tsx, i18n/*.json}
README.md, docs/superpowers/specs/…
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

`web/src/api/schema.d.ts`와 `Cargo.lock`은 싣지 않는다. 각 Task의 "생성 파일" 단계 명령으로 만든다. 이진 시험 견본 `crates/opl-api/tests/fixtures/oidc-rsa.pk8`은 Task 2의 "생성 파일" 단계가 base64로 풀어 만든다.

---

### Task 1: OIDC 영역 타입과 API

`RealmBackend::Oidc(OidcRealm)`: issuer(https, 쿼리·조각·사용자 정보 없음), client ID, client secret(선택 — 공개 클라이언트는 PKCE만), scopes(`openid` 포함), 사용자 이름·그룹 클레임, CA PEM, 시간 제한 1~30초. 영역 API의 OIDC 보기(`has_client_secret`)와 비밀 옮기기 규칙(issuer·client ID·CA가 그대로일 때만 생략 유지), 로그인 화면 목록의 `oidc` 종류, OIDC 영역에 비밀번호 로그인은 400. 외부 사용자에 IdP 주체 `subject`(`"<iss> <sub>"`)와 OIDC 이름 규칙(이메일 모양 `alice@corp.com@okta`까지).

**Files:**
- Modify: `crates/opl-api/src/ldap.rs`
- Modify: `crates/opl-api/src/realms.rs`
- Modify: `crates/opl-api/src/users.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/realms.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-types/src/auth.rs`
- Modify: `crates/opl-types/src/realm.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/realms/RealmsPage.tsx`
- Modify: `web/src/realms/realms.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/ldap.rs`에서

```rust
pub async fn test(spec: &RealmSpec, username: &str, password: &str) -> RealmTestResult {
    let opl_types::realm::RealmBackend::Ldap(realm) = &spec.backend;
    let mut trace = Trace::default();
```

을 다음으로 바꾼다:

```rust
pub async fn test(spec: &RealmSpec, username: &str, password: &str) -> RealmTestResult {
    let opl_types::realm::RealmBackend::Ldap(realm) = &spec.backend else {
        // LDAP 영역이 아니면 시험할 단계가 없다.
        return RealmTestResult {
            steps: Vec::new(),
            dn: None,
            groups: Vec::new(),
            role: None,
        };
    };
    let mut trace = Trace::default();
```

`crates/opl-api/src/realms.rs` 전체:

```rust
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

/// 영역 시험 본문
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RealmTestRequest {
    pub username: String,
    pub password: String,
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

/// 영역 시험의 결과. 단계는 `connect`, `service_bind`, `search`, `user_bind`, `groups` 순이고
/// 실패한 단계에서 멈춘다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmTestResult {
    pub steps: Vec<RealmTestStep>,
    /// 찾은 사용자 항목. 찾기 전에 멈췄으면 null
    #[schema(required = true)]
    pub dn: Option<String>,
    /// 모든 단계가 성공했을 때의 그룹 속성 값들
    pub groups: Vec<String>,
    /// 그룹에서 정해질 역할. 실패했거나 맞는 매핑이 없으면 null
    #[schema(required = true)]
    pub role: Option<Role>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmTestStep {
    pub name: String,
    pub ok: bool,
    pub message: String,
}

/// 사용자 이름과 비밀번호로 영역의 디렉터리를 시험한다. 사용자도 세션도 만들지 않는다.
#[utoipa::path(post, path = "/realms/{key}/actions/test", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = RealmTestRequest,
    responses((status = 200, body = RealmTestResult), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn test_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<RealmTestRequest>,
) -> Result<Json<RealmTestResult>, ApiError> {
    state.authorize(&principal, Action::Operate, RealmSpec::KIND)?;
    let realm = lookup::<RealmSpec>(&state.cluster, &key)?;
    // OIDC 영역의 시험은 다음 단계에서 더한다.
    if let RealmBackend::Oidc(_) = realm.spec.backend {
        return Err(ApiError::bad_request(
            "testing an OIDC realm is not supported yet",
        ));
    }
    Ok(Json(
        crate::ldap::test(&realm.spec, &req.username, &req.password).await,
    ))
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
            username: "alice".into(),
            password: "s3cret".into(),
        };
        let shown = format!("{req:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains("alice"), "{shown}");
    }
}
```

`crates/opl-api/src/users.rs`에서

```rust
/// LDAP 영역 로그인은 디렉터리로 확인하고 외부 사용자 `<이름>@<영역>`을 만들거나 역할을 고친다.
/// 모르는 영역은 400, 맞는 역할이 없으면 403 `no_role`, 디렉터리에 닿지 못하면 503 `realm_unavailable`.
#[utoipa::path(post, path = "/auth/login", tag = "auth", security(()),
```

을 다음으로 바꾼다:

```rust
/// LDAP 영역 로그인은 디렉터리로 확인하고 외부 사용자 `<이름>@<영역>`을 만들거나 역할을 고친다.
/// 모르는 영역과 OIDC 영역(IdP에서 로그인한다)은 400, 맞는 역할이 없으면 403 `no_role`, 디렉터리에
/// 닿지 못하면 503 `realm_unavailable`.
#[utoipa::path(post, path = "/auth/login", tag = "auth", security(()),
```

`crates/opl-api/src/users.rs`에서

```rust
        .ok_or_else(|| ApiError::bad_request(format!("unknown realm {realm:?}")))?;
    let attempt = state.attempts.begin(&name, ip)?;
    let RealmBackend::Ldap(ldap) = &stored.spec.backend;
    // 디렉터리에는 소문자로 바꾼 이름(외부 사용자 이름의 앞부분)을 묻는다.
```

을 다음으로 바꾼다:

```rust
        .ok_or_else(|| ApiError::bad_request(format!("unknown realm {realm:?}")))?;
    let ldap = match &stored.spec.backend {
        RealmBackend::Ldap(ldap) => ldap,
        // 비밀번호를 받지 않으므로 시도로 세지 않는다.
        RealmBackend::Oidc(_) => {
            return Err(ApiError::bad_request(format!(
                "realm {realm} signs in through its identity provider"
            )));
        }
    };
    let attempt = state.attempts.begin(&name, ip)?;
    // 디렉터리에는 소문자로 바꾼 이름(외부 사용자 이름의 앞부분)을 묻는다.
```

`crates/opl-api/src/users.rs`에서

```rust
                        realm: Some(realm.into()),
                    },
```

을 다음으로 바꾼다:

```rust
                        realm: Some(realm.into()),
                        subject: None,
                    },
```

`crates/opl-api/src/users.rs`에서

```rust
            realm: None,
        },
```

을 다음으로 바꾼다:

```rust
            realm: None,
            subject: None,
        },
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
                realm: None,
            },
```

을 다음으로 바꾼다:

```rust
                realm: None,
                subject: None,
            },
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    assert!(ldap_view["has_bind_password"].is_object(), "{ldap_view}");
    assert_eq!(
```

을 다음으로 바꾼다:

```rust
    assert!(ldap_view["has_bind_password"].is_object(), "{ldap_view}");
    let oidc_view = &doc["components"]["schemas"]["OidcRealmView"]["properties"];
    assert!(oidc_view["client_secret"].is_null(), "{oidc_view}");
    assert!(oidc_view["has_client_secret"].is_object(), "{oidc_view}");
    assert_eq!(
```

`crates/opl-api/tests/realms.rs`에서

```rust
//! 비밀번호를 내보내지 않음과 PUT의 bind_password 규칙, 옛 노드 409, 인증 없는 `/auth/realms`, 감사 로그.
```

을 다음으로 바꾼다:

```rust
//! 비밀번호를 내보내지 않음과 PUT의 bind_password 규칙, 옛 노드 409, 인증 없는 `/auth/realms`, 감사 로그.
//! OIDC 영역: 클라이언트 비밀을 내보내지 않음과 PUT의 client_secret 규칙, 공개 클라이언트, 로그인 400.
```

`crates/opl-api/tests/realms.rs`에서

```rust
    let realm = env.cluster.get_by_name::<RealmSpec>(name).unwrap().unwrap();
    let RealmBackend::Ldap(ldap) = realm.spec.backend;
    ldap.bind_password
}
```

을 다음으로 바꾼다:

```rust
    let realm = env.cluster.get_by_name::<RealmSpec>(name).unwrap().unwrap();
    let RealmBackend::Ldap(ldap) = realm.spec.backend else {
        panic!("{name} is not an LDAP realm");
    };
    ldap.bind_password
}

/// 기밀 클라이언트 OIDC 영역 만들기 본문
fn okta() -> Value {
    json!({
        "name": "okta",
        "display": "Okta",
        "backend": {
            "kind": "oidc",
            "issuer": "https://corp.okta.com",
            "client_id": "0oa1",
            "client_secret": "client-secret"
        },
        "roles": {
            "groups": [{ "group": "opl-admins", "role": "admin" }],
            "default": "viewer"
        }
    })
}

/// 저장된 클라이언트 비밀
fn stored_secret(env: &Env, name: &str) -> Option<String> {
    let realm = env.cluster.get_by_name::<RealmSpec>(name).unwrap().unwrap();
    let RealmBackend::Oidc(oidc) = realm.spec.backend else {
        panic!("{name} is not an OIDC realm");
    };
    oidc.client_secret
}

#[tokio::test]
async fn oidc_realms_are_created_without_showing_the_client_secret() {
    let env = Env::start().await;
    let admin = env.admin().await;

    let (status, created) = admin.post("/realms", okta()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "okta");
    // 비밀은 응답에 없고, 있는지만 보인다. 빠진 값은 기본값으로 채운다.
    assert_eq!(
        created["backend"],
        json!({
            "kind": "oidc",
            "issuer": "https://corp.okta.com",
            "client_id": "0oa1",
            "scopes": ["openid", "profile", "email"],
            "username_claim": "preferred_username",
            "groups_claim": "groups",
            "timeout_secs": 10,
            "has_client_secret": true
        })
    );
    assert_eq!(created["roles"], okta()["roles"]);
    assert!(!created.to_string().contains("client-secret"));
    assert_eq!(
        stored_secret(&env, "okta").as_deref(),
        Some("client-secret")
    );
    assert_eq!(admin.get("/realms/okta").await.1, created);
    let (_, list) = admin.get("/realms").await;
    assert!(!list.to_string().contains("client-secret"), "{list}");

    // 모든 필드를 주면 그대로(issuer 끝의 /도) 저장한다.
    let mut full = okta();
    full["name"] = json!("full");
    full["backend"] = json!({
        "kind": "oidc",
        "issuer": "https://idp.example.com/realms/corp/",
        "client_id": "oplite",
        "client_secret": "s",
        "scopes": ["openid", "groups"],
        "username_claim": "email",
        "groups_claim": "roles",
        "ca_pem": "-----BEGIN CERTIFICATE-----",
        "timeout_secs": 120
    });
    let (status, got) = admin.post("/realms", full).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(
        got["backend"],
        json!({
            "kind": "oidc",
            "issuer": "https://idp.example.com/realms/corp/",
            "client_id": "oplite",
            "scopes": ["openid", "groups"],
            "username_claim": "email",
            "groups_claim": "roles",
            "ca_pem": "-----BEGIN CERTIFICATE-----",
            "timeout_secs": 120,
            "has_client_secret": true
        })
    );
}

/// 공개 클라이언트는 비밀이 없다(PKCE만). 빈 글도 없는 것이다.
#[tokio::test]
async fn public_oidc_clients_have_no_secret() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for (name, secret) in [("public", None), ("empty", Some(""))] {
        let mut body = okta();
        body["name"] = json!(name);
        match secret {
            Some(s) => body["backend"]["client_secret"] = json!(s),
            None => {
                body["backend"]
                    .as_object_mut()
                    .unwrap()
                    .remove("client_secret");
            }
        }
        let (status, got) = admin.post("/realms", body).await;
        assert_eq!(status, StatusCode::CREATED, "{got}");
        assert_eq!(got["backend"]["has_client_secret"], false, "{name}");
        assert_eq!(stored_secret(&env, name), None);
    }

    // 저장된 비밀이 없으면 생략은 issuer를 바꿔도 그대로 공개 클라이언트다.
    let mut body = okta();
    body.as_object_mut().unwrap().remove("name");
    body["backend"]
        .as_object_mut()
        .unwrap()
        .remove("client_secret");
    body["backend"]["issuer"] = json!("https://other.example.com");
    body["backend"]["client_id"] = json!("other");
    let (status, got) = admin.put("/realms/public", body, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["issuer"], "https://other.example.com");
    assert_eq!(got["backend"]["has_client_secret"], false);
    assert_eq!(stored_secret(&env, "public"), None);
}

/// PUT에서 client_secret을 빼면 issuer·client_id·ca_pem이 모두 그대로일 때만 저장된 값을 두고(하나라도
/// 바뀌면 400 — 비밀을 다른 IdP로 보내지 않게), 빈 글이면 지우고, 값이면 바꾼다. 서버는 issuer·client_id의
/// 앞뒤 공백과 CA의 CRLF를 다듬은 뒤 비교·저장한다.
#[tokio::test]
async fn the_client_secret_is_kept_replaced_or_cleared() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let ca = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
    let mut created = okta();
    created["backend"]["ca_pem"] = json!(format!("{}\r\n", ca.replace('\n', "\r\n")));
    created["backend"]["issuer"] = json!(" https://corp.okta.com\n");
    created["backend"]["client_id"] = json!(" 0oa1 ");
    let (status, got) = admin.post("/realms", created).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(got["backend"]["issuer"], "https://corp.okta.com");
    assert_eq!(got["backend"]["client_id"], "0oa1");
    assert_eq!(got["backend"]["ca_pem"], ca);

    let without_secret = |f: &dyn Fn(&mut Value)| {
        let mut b = okta();
        b.as_object_mut().unwrap().remove("name");
        b["backend"]["ca_pem"] = json!(ca);
        b["backend"]
            .as_object_mut()
            .unwrap()
            .remove("client_secret");
        f(&mut b);
        b
    };

    // 1. 연결 대상이 같으면 생략은 그대로 둔다(다듬은 같은 CA, 끝 줄바꿈이 있는 CA도).
    for b in [
        without_secret(&|b| b["display"] = json!("Okta SSO")),
        without_secret(&|b| b["backend"]["ca_pem"] = json!(format!("{ca}\n"))),
    ] {
        let (status, got) = admin.put("/realms/okta", b, None).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["backend"]["has_client_secret"], true);
    }
    assert_eq!(
        stored_secret(&env, "okta").as_deref(),
        Some("client-secret")
    );

    // 2~4. issuer·client_id·ca_pem 가운데 하나라도 바꾸면서 생략하면 400이고 아무것도 바뀌지 않는다.
    for changed in [
        without_secret(&|b| b["backend"]["issuer"] = json!("https://evil.example.com")),
        without_secret(&|b| b["backend"]["issuer"] = json!("https://corp.okta.com/")),
        without_secret(&|b| b["backend"]["client_id"] = json!("0oa2")),
        without_secret(&|b| b["backend"]["ca_pem"] = json!("-----BEGIN CERTIFICATE-----")),
        without_secret(&|b| {
            b["backend"].as_object_mut().unwrap().remove("ca_pem");
        }),
    ] {
        let got = admin.put("/realms/okta", changed.clone(), None).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "client_secret must be given again when issuer, client_id or ca_pem changes",
            "{changed}"
        );
    }
    let (_, got) = admin.get("/realms/okta").await;
    assert_eq!(got["backend"]["issuer"], "https://corp.okta.com");
    assert_eq!(
        stored_secret(&env, "okta").as_deref(),
        Some("client-secret")
    );

    // 바꾸면서 새 비밀을 주면 된다.
    let mut moved = without_secret(&|b| b["backend"]["issuer"] = json!("https://new.okta.com"));
    moved["backend"]["client_secret"] = json!("new-secret");
    let (status, got) = admin.put("/realms/okta", moved.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["issuer"], "https://new.okta.com");
    assert_eq!(got["backend"]["has_client_secret"], true);
    assert!(!got.to_string().contains("new-secret"));
    assert_eq!(stored_secret(&env, "okta").as_deref(), Some("new-secret"));

    // 5. 빈 글은 지운다(공개 클라이언트가 된다).
    moved["backend"]["client_secret"] = json!("");
    let (status, got) = admin.put("/realms/okta", moved, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["has_client_secret"], false);
    assert_eq!(stored_secret(&env, "okta"), None);
}

#[tokio::test]
async fn bad_oidc_realms_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let with = |f: &dyn Fn(&mut Value)| {
        let mut body = okta();
        f(&mut body);
        body
    };
    for (body, message) in [
        (
            with(&|b| b["backend"]["issuer"] = json!("http://corp.okta.com")),
            "issuer must be an https:// URL with a host and no query or fragment",
        ),
        (
            with(&|b| b["backend"]["issuer"] = json!("https://corp.okta.com?x=1")),
            "issuer must be an https:// URL with a host and no query or fragment",
        ),
        (
            with(&|b| b["backend"]["client_id"] = json!(" ")),
            "client_id must not be empty",
        ),
        (
            with(&|b| b["backend"]["scopes"] = json!(["profile"])),
            "scopes must include openid, and each scope must be non-empty without spaces",
        ),
        (
            with(&|b| b["backend"]["username_claim"] = json!("")),
            "username_claim must not be empty",
        ),
        (
            with(&|b| b["backend"]["groups_claim"] = json!("")),
            "groups_claim must not be empty",
        ),
        (
            with(&|b| b["backend"]["timeout_secs"] = json!(121)),
            "timeout_secs must be between 1 and 120",
        ),
        // 모르는 필드와 다른 종류의 필드
        (with(&|b| b["backend"]["bogus"] = json!(1)), ""),
        (with(&|b| b["backend"]["base_dn"] = json!("dc=x")), ""),
    ] {
        let got = admin.post("/realms", body.clone()).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        if !message.is_empty() {
            assert_eq!(got.1["message"], message, "{body}");
        }
    }
    assert_eq!(admin.get("/realms").await.1["items"], json!([]));
}

/// OIDC 영역은 admin만 만들고, 옛 노드가 있으면 만들지 않는다.
#[tokio::test]
async fn oidc_realms_need_admins_and_nodes_that_run_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        assert_error(
            &client.post("/realms", okta()).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    assert_error(
        &admin.post("/realms", okta()).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    assert_eq!(admin.get("/realms").await.1["items"], json!([]));
}

/// OIDC 영역은 IdP에서 로그인한다. 로그인 화면 목록에는 kind oidc로 보이고, 비밀번호 로그인은 400이다.
#[tokio::test]
async fn oidc_realms_are_listed_for_the_login_screen_but_reject_password_logins() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", okta()).await.0, StatusCode::CREATED);
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let anonymous = env.anonymous();
    let (status, got) = anonymous.get("/auth/realms").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!([
            { "name": "local", "display": null, "kind": "local" },
            { "name": "corp", "display": "Corp directory", "kind": "ldap" },
            { "name": "okta", "display": "Okta", "kind": "oidc" },
        ])
    );

    // 시도 제한에 세지 않는다(몇 번이든 같은 400).
    for _ in 0..21 {
        let got = anonymous
            .post(
                "/auth/login",
                json!({ "username": "alice", "password": "pw", "realm": "okta" }),
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "realm okta signs in through its identity provider"
        );
    }
    assert!(
        env.cluster
            .get_by_name::<opl_types::auth::UserSpec>("alice@okta")
            .unwrap()
            .is_none()
    );
    // 같은 IP의 로컬 로그인은 막히지 않았다.
    let got = anonymous
        .post(
            "/auth/login",
            json!({ "username": "admin", "password": common::ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(got.0, StatusCode::OK, "{}", got.1);
}
```

`crates/opl-api/tests/realms.rs`에서

```rust
        (with(&|b| b["bogus"] = json!(1)), ""),
        (with(&|b| b["backend"]["kind"] = json!("oidc")), ""),
```

을 다음으로 바꾼다:

```rust
        (with(&|b| b["bogus"] = json!(1)), ""),
        (with(&|b| b["backend"]["kind"] = json!("saml")), ""),
        (with(&|b| b["backend"]["kind"] = json!("oidc")), ""),
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            realm: None,
        },
```

을 다음으로 바꾼다:

```rust
            realm: None,
            subject: None,
        },
```

`crates/opl-types/src/auth.rs` 전체:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, NameError, Resource, ResourceSpec, validate_name};

/// 사용자. 리소스 이름이 로그인 이름이다. 외부 사용자는 `<사용자>@<영역>`이다.
pub type User = Resource<UserSpec, UserStatus>;
/// API 토큰. 리소스 이름은 사용자가 붙인 이름이다. 비밀 값은 해시만 저장한다.
pub type ApiToken = Resource<ApiTokenSpec, ApiTokenStatus>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// 모든 작업
    Admin,
    /// VM과 볼륨을 만들고 다룬다.
    Operator,
    /// 읽기만 한다.
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserSpec {
    /// argon2 PHC 문자열. 외부 사용자는 빈 문자열이다.
    pub password_hash: String,
    pub role: Role,
    /// 비밀번호가 바뀔 때마다 1 올린다. 세션은 만들 때의 값과 다르면 끊긴다.
    /// 0이면 쓰지 않는다(이 필드를 모르는 옛 노드가 읽을 수 있게).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub session_epoch: u64,
    /// 외부 사용자의 영역 이름. 로컬 사용자는 None이고 쓰지 않는다(옛 노드가 읽을 수 있게).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
    /// OIDC 외부 사용자를 처음 만든 IdP의 사용자: `"<iss> <sub>"`(공백 하나로 이음). 같은 이름으로 다른
    /// 사람이 로그인하지 못하게 한다. 그 밖의 사용자는 None이고 쓰지 않는다(옛 노드가 읽을 수 있게).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl ResourceSpec for UserSpec {
    const KIND: Kind = Kind::User;
    type Status = UserStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserStatus {}

/// LDAP 영역의 외부 사용자 이름 규칙: `<사용자>@<영역>`. 마지막 `@`에서 나눈다(영역 이름에는 `@`가
/// 없다). 사용자 부분은 1~64자의 영문 소문자, 숫자, `.`, `_`, `-`이고 영역 부분은 리소스 이름 규칙이다.
pub fn validate_external_name(name: &str) -> Result<(), NameError> {
    validate_with(name, valid_external_user)
}

/// OIDC 영역의 외부 사용자 이름 규칙. 사용자 부분은 1~128자의 영문 소문자, 숫자, `.`, `_`, `+`, `-`와
/// 안쪽 `@` 하나까지다(이메일 모양 `alice@corp.com@okta` — 앞뒤가 비어 있지 않음).
pub fn validate_oidc_external_name(name: &str) -> Result<(), NameError> {
    validate_with(name, valid_oidc_user)
}

fn validate_with(name: &str, valid_user: fn(&str) -> bool) -> Result<(), NameError> {
    let ok = name
        .rsplit_once('@')
        .is_some_and(|(user, realm)| valid_user(user) && validate_name(realm).is_ok());
    if ok {
        Ok(())
    } else {
        Err(NameError::new(name))
    }
}

/// LDAP 영역 로그인 이름으로 외부 사용자 이름을 만든다. 사용자 부분은 소문자로 바꾼 뒤 검사한다.
pub fn external_name(user: &str, realm: &str) -> Result<String, NameError> {
    let name = format!("{}@{realm}", user.to_ascii_lowercase());
    validate_external_name(&name)?;
    Ok(name)
}

/// OIDC 이름 클레임으로 외부 사용자 이름을 만든다. 사용자 부분은 소문자로 바꾼 뒤 검사한다.
pub fn external_name_oidc(user: &str, realm: &str) -> Result<String, NameError> {
    let name = format!("{}@{realm}", user.to_ascii_lowercase());
    validate_oidc_external_name(&name)?;
    Ok(name)
}

fn valid_external_user(user: &str) -> bool {
    (1..=64).contains(&user.len())
        && user.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

fn valid_oidc_user(user: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes().all(|c| {
                c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, b'.' | b'_' | b'+' | b'-')
            })
    };
    (1..=128).contains(&user.len())
        && match user.split_once('@') {
            Some((local, domain)) => part(local) && part(domain),
            None => part(user),
        }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiTokenSpec {
    /// 토큰 주인(User id). 권한은 주인의 역할을 따른다.
    pub user: Uuid,
    /// 비밀 값의 SHA-256 (16진수)
    pub secret_hash: String,
}

impl ResourceSpec for ApiTokenSpec {
    const KIND: Kind = Kind::ApiToken;
    type Status = ApiTokenStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ApiTokenStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_session_epoch_defaults_to_zero() {
        // 옛 사용자에는 session_epoch가 없다.
        let old: UserSpec = serde_json::from_value(serde_json::json!({
            "password_hash": "h",
            "role": "admin"
        }))
        .unwrap();
        assert_eq!(old.session_epoch, 0);
        let user = UserSpec {
            session_epoch: 3,
            ..old
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(json["session_epoch"], 3);
        assert_eq!(serde_json::from_value::<UserSpec>(json).unwrap(), user);
    }

    #[test]
    fn zero_session_epoch_is_not_written() {
        // 옛 노드의 UserSpec은 모르는 필드를 거절하므로, 세대가 0이면 쓰지 않는다.
        let user = UserSpec {
            password_hash: "h".into(),
            role: Role::Admin,
            session_epoch: 0,
            realm: None,
            subject: None,
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "password_hash": "h", "role": "admin" })
        );
    }

    #[test]
    fn local_user_json_keeps_the_old_shape() {
        // 영역이 없는 사용자는 계획 34 모양 그대로다(옛 노드는 모르는 필드를 거절한다).
        let old = serde_json::json!({ "password_hash": "h", "role": "viewer", "session_epoch": 2 });
        let user: UserSpec = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(user.realm, None);
        assert_eq!(serde_json::to_value(&user).unwrap(), old);
    }

    #[test]
    fn external_user_round_trips_with_its_realm() {
        let user = UserSpec {
            password_hash: String::new(),
            role: Role::Operator,
            session_epoch: 0,
            realm: Some("corp".into()),
            subject: None,
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "password_hash": "", "role": "operator", "realm": "corp" })
        );
        assert_eq!(serde_json::from_value::<UserSpec>(json).unwrap(), user);
    }

    #[test]
    fn oidc_user_round_trips_with_its_subject() {
        let user = UserSpec {
            password_hash: String::new(),
            role: Role::Viewer,
            session_epoch: 0,
            realm: Some("okta".into()),
            subject: Some("https://idp.example.com u-1".into()),
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "password_hash": "", "role": "viewer", "realm": "okta",
                                "subject": "https://idp.example.com u-1" })
        );
        assert_eq!(serde_json::from_value::<UserSpec>(json).unwrap(), user);
    }

    #[test]
    fn validates_external_names() {
        let long = format!("{}@corp", "a".repeat(64));
        for ok in ["alice@corp", "a.b_c-d@ad-01", "0@a", long.as_str()] {
            assert_eq!(validate_external_name(ok), Ok(()), "{ok}");
        }
        let too_long = format!("{}@corp", "a".repeat(65));
        for bad in [
            "",
            "alice",
            "@corp",
            "alice@",
            "Alice@corp",
            "alice@Corp",
            "al ice@corp",
            "al*ce@corp",
            "a@b@corp",
            "alice@corp.com@okta",
            "alice@co.rp",
            "alice@-corp",
            too_long.as_str(),
        ] {
            assert!(validate_external_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_oidc_external_names() {
        let long = format!("{}@okta", "a".repeat(128));
        let long_email = format!("{}@corp.com@okta", "a".repeat(119));
        for ok in [
            "alice@okta",
            "alice@corp.com@okta",
            "a.b_c-d+e@sub.corp-1.com@okta",
            "0@a",
            long.as_str(),
            long_email.as_str(),
        ] {
            assert_eq!(validate_oidc_external_name(ok), Ok(()), "{ok}");
        }
        let too_long = format!("{}@okta", "a".repeat(129));
        let too_long_email = format!("{}@corp.com@okta", "a".repeat(120));
        for bad in [
            "",
            "alice",
            "@okta",
            "alice@",
            "Alice@okta",
            "alice@Okta",
            "@corp.com@okta",
            "alice@@okta",
            "a@b@c@okta",
            "al ice@okta",
            "a/b@okta",
            "a:b@okta",
            "a=b@okta",
            "a%40b@okta",
            "alice@corp%2ecom@okta",
            "alice@corp com@okta",
            "é@okta",
            too_long.as_str(),
            too_long_email.as_str(),
        ] {
            assert!(validate_oidc_external_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn external_names_split_at_the_last_at_sign() {
        // OIDC: 이메일 모양 이름의 사용자 부분은 `alice@corp.com`, 영역은 `okta`
        assert_eq!(
            external_name_oidc("Alice@Corp.com", "okta").unwrap(),
            "alice@corp.com@okta"
        );
        assert_eq!(
            "alice@corp.com@okta".rsplit_once('@'),
            Some(("alice@corp.com", "okta"))
        );
        assert_eq!(
            external_name_oidc("Bob+ops", "okta").unwrap(),
            "bob+ops@okta"
        );
        for bad in [
            "",
            "a b",
            "a@",
            "@b",
            "a@b@c",
            "a%40b",
            "é",
            &"a".repeat(129),
        ] {
            assert!(external_name_oidc(bad, "okta").is_err(), "{bad}");
        }
        assert!(external_name_oidc("alice", "Okta").is_err());
        // LDAP 영역에서는 `@`·`+`가 든 이름을 거절한다.
        for bad in ["alice@corp.com", "bob+ops"] {
            assert!(external_name(bad, "corp").is_err(), "{bad}");
        }
    }

    #[test]
    fn external_name_lowercases_the_user_part() {
        assert_eq!(external_name("Alice", "corp").unwrap(), "alice@corp");
        assert_eq!(external_name("j.doe_2", "ad").unwrap(), "j.doe_2@ad");
        // 영역 이름은 바꾸지 않는다.
        assert!(external_name("alice", "Corp").is_err());
        for bad in ["", "a@b", "a*", "a)", "a b", "é", &"a".repeat(65)] {
            assert!(external_name(bad, "corp").is_err(), "{bad}");
        }
    }
}
```

`crates/opl-types/src/realm.rs` 전체:

```rust
//! 인증 영역. 로컬 사용자 말고 바깥 디렉터리(LDAP)나 OIDC 제공자로 로그인하게 한다. 리소스 이름이 영역 이름이고,
//! 그 영역으로 로그인한 사용자는 `<사용자>@<영역>` 이름의 외부 사용자가 된다.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::auth::Role;
use crate::resource::{Kind, NameError, Resource, ResourceSpec, validate_name};

pub type Realm = Resource<RealmSpec, RealmStatus>;

/// 로컬 사용자의 영역 이름. 리소스로 만들 수 없다.
pub const LOCAL_REALM: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealmSpec {
    /// 로그인 화면에 보일 이름
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    pub backend: RealmBackend,
    /// 그룹에서 역할 정하기
    #[serde(default)]
    pub roles: RoleMapping,
}

impl ResourceSpec for RealmSpec {
    const KIND: Kind = Kind::Realm;
    type Status = RealmStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RealmStatus {}

/// 영역이 사용자를 확인하는 방법
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RealmBackend {
    Ldap(LdapRealm),
    Oidc(OidcRealm),
}

/// LDAP 디렉터리. `ldap://`는 StartTLS를 반드시 쓰고, `ldaps://`는 처음부터 TLS다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct LdapRealm {
    /// `ldap://host[:port]` 또는 `ldaps://host[:port]`
    pub url: String,
    /// 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bind_dn: Option<String>,
    /// 서비스 계정 비밀번호. API는 내보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bind_password: Option<String>,
    /// 사용자를 찾을 기준 DN(subtree)
    pub base_dn: String,
    /// 사용자 찾기 필터. `{username}`이 이스케이프된 로그인 이름으로 바뀐다.
    #[serde(default = "default_user_filter")]
    pub user_filter: String,
    /// 그룹 DN을 담은 사용자 항목의 속성
    #[serde(default = "default_group_attr")]
    pub group_attr: String,
    /// 서버 인증서를 확인할 CA(PEM). 없으면 시스템 루트
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ca_pem: Option<String>,
    /// 연결과 작업마다의 시간 제한(초)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

// 서비스 계정 비밀번호가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for LdapRealm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LdapRealm")
            .field("url", &self.url)
            .field("bind_dn", &self.bind_dn)
            .field("bind_password", &self.bind_password.as_ref().map(|_| "***"))
            .field("base_dn", &self.base_dn)
            .field("user_filter", &self.user_filter)
            .field("group_attr", &self.group_attr)
            .field("ca_pem", &self.ca_pem)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

/// 사용자 찾기 필터의 기본값
pub const DEFAULT_USER_FILTER: &str = "(&(objectClass=person)(uid={username}))";
/// 그룹 속성의 기본값
pub const DEFAULT_GROUP_ATTR: &str = "memberOf";
/// 시간 제한의 기본값(초)
pub const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// OpenID Connect 제공자(IdP). 브라우저가 IdP에서 로그인하고(인증 코드 흐름과 PKCE) 돌아온다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct OidcRealm {
    /// `https://host[:port][/path]`. discovery 문서의 `issuer`와 글자 그대로 같아야 한다.
    pub issuer: String,
    pub client_id: String,
    /// 기밀 클라이언트의 비밀. 공개 클라이언트면 없다. API는 내보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub client_secret: Option<String>,
    /// 요청할 scope. `openid`가 있어야 한다.
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// 사용자 이름을 담은 클레임
    #[serde(default = "default_username_claim")]
    pub username_claim: String,
    /// 그룹(문자열 배열)을 담은 클레임
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
    /// IdP 인증서를 확인할 CA(PEM). 없으면 시스템 루트
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ca_pem: Option<String>,
    /// IdP 요청마다의 시간 제한(초)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

// 클라이언트 비밀이 로그에 나가지 않게 가린다.
impl std::fmt::Debug for OidcRealm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcRealm")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "***"))
            .field("scopes", &self.scopes)
            .field("username_claim", &self.username_claim)
            .field("groups_claim", &self.groups_claim)
            .field("ca_pem", &self.ca_pem)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

/// OIDC scope의 기본값
pub const DEFAULT_SCOPES: [&str; 3] = ["openid", "profile", "email"];
/// 사용자 이름 클레임의 기본값
pub const DEFAULT_USERNAME_CLAIM: &str = "preferred_username";
/// 그룹 클레임의 기본값
pub const DEFAULT_GROUPS_CLAIM: &str = "groups";

fn default_scopes() -> Vec<String> {
    DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect()
}

fn default_username_claim() -> String {
    DEFAULT_USERNAME_CLAIM.into()
}

fn default_groups_claim() -> String {
    DEFAULT_GROUPS_CLAIM.into()
}

fn default_user_filter() -> String {
    DEFAULT_USER_FILTER.into()
}

fn default_group_attr() -> String {
    DEFAULT_GROUP_ATTR.into()
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

/// 그룹에서 역할 정하기
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RoleMapping {
    #[serde(default)]
    pub groups: Vec<GroupRole>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 로그인을 거절한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub default: Option<Role>,
}

/// 이 그룹(보통 DN)에 든 사용자는 이 역할
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct GroupRole {
    pub group: String,
    pub role: Role,
}

impl RoleMapping {
    /// 사용자 그룹에 맞는 매핑 가운데 가장 높은 역할. 그룹은 앞뒤 공백을 떼고 ASCII 대소문자를
    /// 무시해 비교한다. 맞는 것이 없으면 `default`.
    pub fn role_for(&self, groups: &[String]) -> Option<Role> {
        self.groups
            .iter()
            .filter(|m| {
                groups
                    .iter()
                    .any(|g| g.trim().eq_ignore_ascii_case(m.group.trim()))
            })
            .map(|m| m.role)
            .max_by_key(|r| rank(*r))
            .or(self.default)
    }
}

/// 역할의 높이: admin > operator > viewer
fn rank(role: Role) -> u8 {
    match role {
        Role::Admin => 2,
        Role::Operator => 1,
        Role::Viewer => 0,
    }
}

impl RealmSpec {
    pub fn validate(&self) -> Result<(), RealmError> {
        match &self.backend {
            RealmBackend::Ldap(ldap) => ldap.validate()?,
            RealmBackend::Oidc(oidc) => oidc.validate()?,
        }
        if self.roles.groups.iter().any(|m| m.group.trim().is_empty()) {
            return Err(RealmError::EmptyGroup);
        }
        Ok(())
    }
}

impl LdapRealm {
    fn validate(&self) -> Result<(), RealmError> {
        if !ldap_url_has_host(&self.url) {
            return Err(RealmError::Url);
        }
        if self.base_dn.trim().is_empty() {
            return Err(RealmError::BaseDn);
        }
        if self.user_filter.matches("{username}").count() != 1 {
            return Err(RealmError::UserFilter);
        }
        if self.group_attr.trim().is_empty() {
            return Err(RealmError::GroupAttr);
        }
        if self.bind_password.is_some() && self.bind_dn.is_none() {
            return Err(RealmError::BindPasswordWithoutDn);
        }
        if self.bind_dn.is_some() && self.bind_password.as_deref().unwrap_or("").is_empty() {
            return Err(RealmError::BindDnWithoutPassword);
        }
        if !(1..=30).contains(&self.timeout_secs) {
            return Err(RealmError::Timeout);
        }
        Ok(())
    }
}

impl OidcRealm {
    fn validate(&self) -> Result<(), RealmError> {
        if !https_url_has_host(&self.issuer) {
            return Err(RealmError::Issuer);
        }
        if self.client_id.trim().is_empty() {
            return Err(RealmError::ClientId);
        }
        if !self.scopes.iter().any(|s| s == "openid")
            || self
                .scopes
                .iter()
                .any(|s| s.is_empty() || s.chars().any(char::is_whitespace))
        {
            return Err(RealmError::Scopes);
        }
        if self.username_claim.trim().is_empty() {
            return Err(RealmError::UsernameClaim);
        }
        if self.groups_claim.trim().is_empty() {
            return Err(RealmError::GroupsClaim);
        }
        if !(1..=120).contains(&self.timeout_secs) {
            return Err(RealmError::OidcTimeout);
        }
        Ok(())
    }
}

/// `https://`로 시작하고 호스트가 있으며 쿼리·조각·공백이 없는지
fn https_url_has_host(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if url.contains(['?', '#']) || url.chars().any(char::is_whitespace) {
        return false;
    }
    let authority = rest.split('/').next().unwrap_or_default();
    // 사용자 정보는 받지 않는다.
    if authority.contains('@') {
        return false;
    }
    let host = match authority.strip_prefix('[') {
        // [IPv6]:port
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    !host.is_empty()
}

/// `ldap://` 또는 `ldaps://`로 시작하고 호스트가 있는지
fn ldap_url_has_host(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("ldaps://")
        .or_else(|| url.strip_prefix("ldap://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        // [IPv6]:port
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    !host.is_empty()
}

/// 영역 이름 규칙: 리소스 이름 규칙이고 `local`이 아니며 UUID로 풀리지 않는다(API가 id로 읽는다).
pub fn validate_realm_name(name: &str) -> Result<(), RealmError> {
    validate_name(name)?;
    if name == LOCAL_REALM {
        return Err(RealmError::ReservedName);
    }
    if uuid::Uuid::try_parse(name).is_ok() {
        return Err(RealmError::LooksLikeId(name.to_string()));
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RealmError {
    #[error(transparent)]
    Name(#[from] NameError),
    #[error("the realm name \"local\" is reserved for local users")]
    ReservedName,
    #[error("invalid name {0:?}: names must not look like an id")]
    LooksLikeId(String),
    #[error("url must be ldap://host[:port] or ldaps://host[:port]")]
    Url,
    #[error("base_dn must not be empty")]
    BaseDn,
    #[error("user_filter must contain {{username}} exactly once")]
    UserFilter,
    #[error("group_attr must not be empty")]
    GroupAttr,
    #[error("bind_password needs bind_dn")]
    BindPasswordWithoutDn,
    #[error("bind_dn needs bind_password; remove bind_dn for an anonymous search")]
    BindDnWithoutPassword,
    #[error("timeout_secs must be between 1 and 30")]
    Timeout,
    #[error("issuer must be an https:// URL with a host and no query or fragment")]
    Issuer,
    #[error("client_id must not be empty")]
    ClientId,
    #[error("scopes must include openid, and each scope must be non-empty without spaces")]
    Scopes,
    #[error("username_claim must not be empty")]
    UsernameClaim,
    #[error("groups_claim must not be empty")]
    GroupsClaim,
    #[error("timeout_secs must be between 1 and 120")]
    OidcTimeout,
    #[error("roles.groups[].group must not be empty")]
    EmptyGroup,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ldap() -> LdapRealm {
        serde_json::from_value(json!({
            "url": "ldaps://ldap.example.com",
            "base_dn": "dc=example,dc=com"
        }))
        .unwrap()
    }

    fn spec(ldap: LdapRealm) -> RealmSpec {
        RealmSpec {
            display: None,
            backend: RealmBackend::Ldap(ldap),
            roles: RoleMapping::default(),
        }
    }

    #[test]
    fn kind_is_realm() {
        assert_eq!(RealmSpec::KIND, Kind::Realm);
        assert_eq!(serde_json::to_value(Kind::Realm).unwrap(), "realm");
    }

    #[test]
    fn ldap_fields_have_defaults() {
        let l = ldap();
        assert_eq!(l.user_filter, "(&(objectClass=person)(uid={username}))");
        assert_eq!(l.group_attr, "memberOf");
        assert_eq!(l.timeout_secs, 10);
        assert_eq!(l.bind_dn, None);
        assert_eq!(l.bind_password, None);
        assert_eq!(l.ca_pem, None);
    }

    #[test]
    fn realm_spec_round_trips_as_tagged_json() {
        let full = json!({
            "display": "Corp",
            "backend": {
                "kind": "ldap",
                "url": "ldap://ldap.example.com:389",
                "bind_dn": "cn=svc,dc=example,dc=com",
                "bind_password": "s3cret",
                "base_dn": "dc=example,dc=com",
                "user_filter": "(sAMAccountName={username})",
                "group_attr": "memberOf",
                "ca_pem": "-----BEGIN CERTIFICATE-----",
                "timeout_secs": 5
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        });
        let s: RealmSpec = serde_json::from_value(full.clone()).unwrap();
        let RealmBackend::Ldap(l) = &s.backend else {
            panic!("not ldap: {s:?}");
        };
        assert_eq!(l.bind_password.as_deref(), Some("s3cret"));
        assert_eq!(l.timeout_secs, 5);
        assert_eq!(s.roles.default, Some(Role::Viewer));
        assert_eq!(serde_json::to_value(&s).unwrap(), full);

        // 없는 선택 필드는 쓰지 않는다.
        let small = spec(ldap());
        let json = serde_json::to_value(&small).unwrap();
        assert_eq!(
            json,
            json!({
                "backend": {
                    "kind": "ldap",
                    "url": "ldaps://ldap.example.com",
                    "base_dn": "dc=example,dc=com",
                    "user_filter": "(&(objectClass=person)(uid={username}))",
                    "group_attr": "memberOf",
                    "timeout_secs": 10
                },
                "roles": { "groups": [] }
            })
        );
        assert_eq!(serde_json::from_value::<RealmSpec>(json).unwrap(), small);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let backend = json!({ "kind": "ldap", "url": "ldaps://h", "base_dn": "dc=x" });
        let ok = json!({ "backend": backend });
        assert!(serde_json::from_value::<RealmSpec>(ok).is_ok());

        let mut extra_spec = json!({ "backend": backend });
        extra_spec["bogus"] = json!(1);
        let mut extra_ldap = backend.clone();
        extra_ldap["bogus"] = json!(1);
        let mut extra_kind = backend.clone();
        extra_kind["kind"] = json!("saml");
        for bad in [
            extra_spec,
            json!({ "backend": extra_ldap }),
            json!({ "backend": extra_kind }),
            json!({ "backend": backend, "roles": { "bogus": [] } }),
            json!({ "backend": backend, "roles": { "groups": [{ "group": "g", "role": "admin", "x": 1 }] } }),
        ] {
            assert!(
                serde_json::from_value::<RealmSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn validates_ldap_realms() {
        assert_eq!(spec(ldap()).validate(), Ok(()));
        for url in [
            "ldap://h",
            "ldaps://h:636",
            "ldap://10.0.0.1:389/",
            "ldaps://[::1]:636",
        ] {
            let l = LdapRealm {
                url: url.into(),
                ..ldap()
            };
            assert_eq!(spec(l).validate(), Ok(()), "{url}");
        }
        for url in [
            "",
            "h",
            "http://h",
            "ldap://",
            "ldaps://:636",
            "ldaps:///dc=x",
            "ldaps://[]:636",
        ] {
            let l = LdapRealm {
                url: url.into(),
                ..ldap()
            };
            assert_eq!(spec(l).validate(), Err(RealmError::Url), "{url}");
        }

        let cases = [
            (
                LdapRealm {
                    base_dn: " ".into(),
                    ..ldap()
                },
                RealmError::BaseDn,
            ),
            (
                LdapRealm {
                    user_filter: "(uid=x)".into(),
                    ..ldap()
                },
                RealmError::UserFilter,
            ),
            (
                LdapRealm {
                    user_filter: "(|(uid={username})(mail={username}))".into(),
                    ..ldap()
                },
                RealmError::UserFilter,
            ),
            (
                LdapRealm {
                    group_attr: "".into(),
                    ..ldap()
                },
                RealmError::GroupAttr,
            ),
            (
                LdapRealm {
                    bind_password: Some("p".into()),
                    ..ldap()
                },
                RealmError::BindPasswordWithoutDn,
            ),
            (
                LdapRealm {
                    timeout_secs: 0,
                    ..ldap()
                },
                RealmError::Timeout,
            ),
            (
                LdapRealm {
                    timeout_secs: 31,
                    ..ldap()
                },
                RealmError::Timeout,
            ),
        ];
        for (l, err) in cases {
            assert_eq!(spec(l.clone()).validate(), Err(err), "{l:?}");
        }

        // 경계값과 서비스 계정
        for timeout_secs in [1, 30] {
            let l = LdapRealm {
                timeout_secs,
                ..ldap()
            };
            assert_eq!(spec(l).validate(), Ok(()));
        }
        // 전체 마감(timeout_secs × 4 + 1초)이 121초를 넘지 않는다.
        assert_eq!(
            RealmError::Timeout.to_string(),
            "timeout_secs must be between 1 and 30"
        );
        let l = LdapRealm {
            bind_dn: Some("cn=svc".into()),
            bind_password: Some("p".into()),
            ..ldap()
        };
        assert_eq!(spec(l).validate(), Ok(()));
        // bind_dn이 있으면 비밀번호도 있어야 한다. 익명 검색은 둘 다 없을 때만이다.
        for bind_password in [None, Some(String::new())] {
            let l = LdapRealm {
                bind_dn: Some("cn=svc".into()),
                bind_password,
                ..ldap()
            };
            assert_eq!(spec(l).validate(), Err(RealmError::BindDnWithoutPassword));
        }
        assert_eq!(
            RealmError::BindDnWithoutPassword.to_string(),
            "bind_dn needs bind_password; remove bind_dn for an anonymous search"
        );

        let mut s = spec(ldap());
        s.roles.groups.push(GroupRole {
            group: "  ".into(),
            role: Role::Admin,
        });
        assert_eq!(s.validate(), Err(RealmError::EmptyGroup));
    }

    fn oidc() -> OidcRealm {
        serde_json::from_value(json!({
            "issuer": "https://idp.example.com/realms/corp",
            "client_id": "oplite"
        }))
        .unwrap()
    }

    fn oidc_spec(oidc: OidcRealm) -> RealmSpec {
        RealmSpec {
            display: None,
            backend: RealmBackend::Oidc(oidc),
            roles: RoleMapping::default(),
        }
    }

    #[test]
    fn oidc_fields_have_defaults() {
        let o = oidc();
        assert_eq!(o.scopes, ["openid", "profile", "email"]);
        assert_eq!(o.username_claim, "preferred_username");
        assert_eq!(o.groups_claim, "groups");
        assert_eq!(o.timeout_secs, 10);
        assert_eq!(o.client_secret, None);
        assert_eq!(o.ca_pem, None);
    }

    #[test]
    fn oidc_realm_round_trips_as_tagged_json() {
        let full = json!({
            "display": "Okta",
            "backend": {
                "kind": "oidc",
                "issuer": "https://corp.okta.com/",
                "client_id": "0oa1",
                "client_secret": "s3cret",
                "scopes": ["openid", "groups"],
                "username_claim": "email",
                "groups_claim": "roles",
                "ca_pem": "-----BEGIN CERTIFICATE-----",
                "timeout_secs": 60
            },
            "roles": {
                "groups": [{ "group": "opl-admins", "role": "admin" }]
            }
        });
        let s: RealmSpec = serde_json::from_value(full.clone()).unwrap();
        let RealmBackend::Oidc(o) = &s.backend else {
            panic!("not oidc: {s:?}");
        };
        assert_eq!(o.client_secret.as_deref(), Some("s3cret"));
        assert_eq!(o.issuer, "https://corp.okta.com/");
        assert_eq!(serde_json::to_value(&s).unwrap(), full);

        // 없는 선택 필드는 쓰지 않는다.
        let small = oidc_spec(oidc());
        let json = serde_json::to_value(&small).unwrap();
        assert_eq!(
            json,
            json!({
                "backend": {
                    "kind": "oidc",
                    "issuer": "https://idp.example.com/realms/corp",
                    "client_id": "oplite",
                    "scopes": ["openid", "profile", "email"],
                    "username_claim": "preferred_username",
                    "groups_claim": "groups",
                    "timeout_secs": 10
                },
                "roles": { "groups": [] }
            })
        );
        assert_eq!(serde_json::from_value::<RealmSpec>(json).unwrap(), small);

        // 모르는 필드와 LDAP 필드는 받지 않는다.
        for (field, value) in [("bogus", json!(1)), ("url", json!("ldaps://h"))] {
            let mut bad =
                json!({ "backend": { "kind": "oidc", "issuer": "https://h", "client_id": "c" } });
            bad["backend"][field] = value;
            assert!(
                serde_json::from_value::<RealmSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        for missing in ["issuer", "client_id"] {
            let mut bad =
                json!({ "backend": { "kind": "oidc", "issuer": "https://h", "client_id": "c" } });
            bad["backend"].as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<RealmSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn validates_oidc_realms() {
        assert_eq!(oidc_spec(oidc()).validate(), Ok(()));
        for issuer in [
            "https://h",
            "https://h/",
            "https://h:8443/realms/corp",
            "https://10.0.0.1",
            "https://[::1]:8443/x",
        ] {
            let o = OidcRealm {
                issuer: issuer.into(),
                ..oidc()
            };
            assert_eq!(oidc_spec(o).validate(), Ok(()), "{issuer}");
        }
        for issuer in [
            "",
            "h",
            "http://h",
            "https://",
            "https:///x",
            "https://:443",
            "https://[]:443",
            "https://h?x=1",
            "https://h/#frag",
            "https://u@h",
            "https://h /x",
            " https://h",
        ] {
            let o = OidcRealm {
                issuer: issuer.into(),
                ..oidc()
            };
            assert_eq!(oidc_spec(o).validate(), Err(RealmError::Issuer), "{issuer}");
        }

        let scopes = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for good in [scopes(&["openid"]), scopes(&["email", "openid", "groups"])] {
            let o = OidcRealm {
                scopes: good.clone(),
                ..oidc()
            };
            assert_eq!(oidc_spec(o).validate(), Ok(()), "{good:?}");
        }
        for bad in [
            scopes(&[]),
            scopes(&["profile", "email"]),
            scopes(&["OpenID"]),
            scopes(&["openid", ""]),
            scopes(&["openid", "a b"]),
            scopes(&["openid profile"]),
        ] {
            let o = OidcRealm {
                scopes: bad.clone(),
                ..oidc()
            };
            assert_eq!(oidc_spec(o).validate(), Err(RealmError::Scopes), "{bad:?}");
        }

        let cases = [
            (
                OidcRealm {
                    client_id: " ".into(),
                    ..oidc()
                },
                RealmError::ClientId,
            ),
            (
                OidcRealm {
                    username_claim: "".into(),
                    ..oidc()
                },
                RealmError::UsernameClaim,
            ),
            (
                OidcRealm {
                    groups_claim: " ".into(),
                    ..oidc()
                },
                RealmError::GroupsClaim,
            ),
            (
                OidcRealm {
                    timeout_secs: 0,
                    ..oidc()
                },
                RealmError::OidcTimeout,
            ),
            (
                OidcRealm {
                    timeout_secs: 121,
                    ..oidc()
                },
                RealmError::OidcTimeout,
            ),
        ];
        for (o, err) in cases {
            assert_eq!(oidc_spec(o.clone()).validate(), Err(err), "{o:?}");
        }
        for timeout_secs in [1, 120] {
            let o = OidcRealm {
                timeout_secs,
                ..oidc()
            };
            assert_eq!(oidc_spec(o).validate(), Ok(()));
        }
        assert_eq!(
            RealmError::OidcTimeout.to_string(),
            "timeout_secs must be between 1 and 120"
        );
        // 공개 클라이언트(비밀 없음)와 기밀 클라이언트 모두 된다.
        let o = OidcRealm {
            client_secret: Some("s".into()),
            ..oidc()
        };
        assert_eq!(oidc_spec(o).validate(), Ok(()));

        let mut s = oidc_spec(oidc());
        s.roles.groups.push(GroupRole {
            group: "".into(),
            role: Role::Admin,
        });
        assert_eq!(s.validate(), Err(RealmError::EmptyGroup));
    }

    #[test]
    fn debug_hides_the_client_secret() {
        let o = OidcRealm {
            client_secret: Some("s3cret".into()),
            ..oidc()
        };
        let shown = format!("{:?}", oidc_spec(o));
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains(r#"client_secret: Some("***")"#), "{shown}");
        assert!(shown.contains(r#"client_id: "oplite""#), "{shown}");
        assert!(format!("{:?}", oidc()).contains("client_secret: None"));
    }

    fn mapping() -> RoleMapping {
        RoleMapping {
            groups: vec![
                GroupRole {
                    group: "cn=viewers,dc=x".into(),
                    role: Role::Viewer,
                },
                GroupRole {
                    group: "cn=Admins,dc=x".into(),
                    role: Role::Admin,
                },
                GroupRole {
                    group: " cn=ops,dc=x ".into(),
                    role: Role::Operator,
                },
            ],
            default: None,
        }
    }

    fn groups(gs: &[&str]) -> Vec<String> {
        gs.iter().map(|g| g.to_string()).collect()
    }

    #[test]
    fn role_for_picks_the_highest_matching_role() {
        let m = mapping();
        assert_eq!(
            m.role_for(&groups(&["cn=viewers,dc=x", "cn=ops,dc=x"])),
            Some(Role::Operator)
        );
        assert_eq!(
            m.role_for(&groups(&[
                "cn=ops,dc=x",
                "cn=admins,dc=x",
                "cn=viewers,dc=x"
            ])),
            Some(Role::Admin)
        );
        assert_eq!(
            m.role_for(&groups(&["cn=viewers,dc=x"])),
            Some(Role::Viewer)
        );
    }

    #[test]
    fn role_for_ignores_ascii_case_and_surrounding_spaces() {
        let m = mapping();
        assert_eq!(m.role_for(&groups(&["CN=ADMINS,DC=X"])), Some(Role::Admin));
        assert_eq!(
            m.role_for(&groups(&["  cn=OPS,dc=x"])),
            Some(Role::Operator)
        );
        // 부분 문자열은 맞지 않는다.
        assert_eq!(m.role_for(&groups(&["cn=admins,dc=x,dc=y"])), None);
    }

    #[test]
    fn role_for_falls_back_to_default_or_none() {
        let mut m = mapping();
        assert_eq!(m.role_for(&groups(&["cn=other,dc=x"])), None);
        assert_eq!(m.role_for(&[]), None);
        m.default = Some(Role::Viewer);
        assert_eq!(m.role_for(&groups(&["cn=other,dc=x"])), Some(Role::Viewer));
        // 맞는 그룹이 있으면 default보다 그것이다(낮아도).
        m.default = Some(Role::Admin);
        assert_eq!(
            m.role_for(&groups(&["cn=viewers,dc=x"])),
            Some(Role::Viewer)
        );
    }

    #[test]
    fn debug_hides_the_bind_password() {
        let l = LdapRealm {
            bind_dn: Some("cn=svc".into()),
            bind_password: Some("s3cret".into()),
            ..ldap()
        };
        let shown = format!("{l:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains(r#"bind_password: Some("***")"#), "{shown}");
        assert!(shown.contains(r#"bind_dn: Some("cn=svc")"#), "{shown}");
        assert!(shown.contains("timeout_secs: 10"), "{shown}");
        let shown = format!("{:?}", spec(l));
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(format!("{:?}", ldap()).contains("bind_password: None"));
    }

    #[test]
    fn validates_realm_names() {
        for ok in ["corp", "ad-01", "a"] {
            assert_eq!(validate_realm_name(ok), Ok(()), "{ok}");
        }
        assert_eq!(validate_realm_name("local"), Err(RealmError::ReservedName));
        // id로 읽히는 이름은 이름으로 부를 수 없다.
        for id in [
            "0192f0c1-5d2e-7000-8000-000000000000",
            "0192f0c15d2e70008000000000000000",
        ] {
            assert_eq!(
                validate_realm_name(id),
                Err(RealmError::LooksLikeId(id.into())),
                "{id}"
            );
        }
        assert_eq!(
            RealmError::LooksLikeId("0192f0c15d2e70008000000000000000".into()).to_string(),
            "invalid name \"0192f0c15d2e70008000000000000000\": names must not look like an id"
        );
        for bad in ["", "Corp", "a@b", "-a", &"a".repeat(64)] {
            assert!(
                matches!(validate_realm_name(bad), Err(RealmError::Name(_))),
                "{bad}"
            );
        }
    }
}
```

`crates/opld/src/bootstrap.rs`에서

```rust
            realm: None,
        },
```

을 다음으로 바꾼다:

```rust
            realm: None,
            subject: None,
        },
```

`web/src/i18n/en.json`에서

```json
    "kinds": {
      "ldap": "LDAP"
    },
```

을 다음으로 바꾼다:

```json
    "kinds": {
      "ldap": "LDAP",
      "oidc": "OIDC"
    },
```

`web/src/i18n/ko.json`에서

```json
    "kinds": {
      "ldap": "LDAP"
    },
```

을 다음으로 바꾼다:

```json
    "kinds": {
      "ldap": "LDAP",
      "oidc": "OIDC"
    },
```

`web/src/realms/RealmsPage.tsx`에서

```tsx
              <TableCell>{t(`realms.kinds.${realm.backend.kind}`)}</TableCell>
              <TableCell className="font-mono text-xs">{realm.backend.url}</TableCell>
              <TableCell className="text-right whitespace-nowrap">
```

을 다음으로 바꾼다:

```tsx
              <TableCell>{t(`realms.kinds.${realm.backend.kind}`)}</TableCell>
              <TableCell className="font-mono text-xs">
                {realm.backend.kind === "ldap" ? realm.backend.url : realm.backend.issuer}
              </TableCell>
              <TableCell className="text-right whitespace-nowrap">
```

`web/src/realms/RealmsPage.tsx`에서

```tsx
  };
  const saved = realm?.backend.has_bind_password ?? false;
```

을 다음으로 바꾼다:

```tsx
  };
  const saved = (realm?.backend.kind === "ldap" && realm.backend.has_bind_password) || false;
```

`web/src/realms/realms.ts`에서

```ts
export function formOf(realm: RealmView): RealmForm {
  const ldap = realm.backend;
  return {
    name: realm.name,
    display: realm.display ?? "",
    url: ldap.url,
    baseDn: ldap.base_dn,
    bindDn: ldap.bind_dn ?? "",
    bindPassword: "",
    clearBindPassword: false,
    userFilter: ldap.user_filter,
    groupAttr: ldap.group_attr,
    caPem: ldap.ca_pem ?? "",
    timeout: String(ldap.timeout_secs),
    groups: (realm.roles.groups ?? []).map((g) => ({ ...g })),
```

을 다음으로 바꾼다:

```ts
export function formOf(realm: RealmView): RealmForm {
  // OIDC 영역의 폼은 아직 없다. LDAP 칸은 기본값으로 둔다.
  const ldap = realm.backend.kind === "ldap" ? realm.backend : null;
  return {
    name: realm.name,
    display: realm.display ?? "",
    url: ldap?.url ?? "",
    baseDn: ldap?.base_dn ?? "",
    bindDn: ldap?.bind_dn ?? "",
    bindPassword: "",
    clearBindPassword: false,
    userFilter: ldap?.user_filter ?? DEFAULT_USER_FILTER,
    groupAttr: ldap?.group_attr ?? DEFAULT_GROUP_ATTR,
    caPem: ldap?.ca_pem ?? "",
    timeout: String(ldap?.timeout_secs ?? DEFAULT_TIMEOUT_SECS),
    groups: (realm.roles.groups ?? []).map((g) => ({ ...g })),
```

`web/src/realms/realms.ts`에서

```ts
  const bindDn = form.bindDn.trim();
  const saved = realm?.backend.has_bind_password ?? false;
  if (form.bindPassword && !form.clearBindPassword && !bindDn) {
```

을 다음으로 바꾼다:

```ts
  const bindDn = form.bindDn.trim();
  const saved = (realm?.backend.kind === "ldap" && realm.backend.has_bind_password) || false;
  if (form.bindPassword && !form.clearBindPassword && !bindDn) {
```

`web/src/realms/realms.ts`에서

```ts
    errors.bindPassword = "realms.errors.clearSaved";
  } else if (realm && saved && !form.clearBindPassword && !form.bindPassword) {
    // 저장된 비밀번호를 다른 서버·계정으로 보내지 않는다(서버도 400).
```

을 다음으로 바꾼다:

```ts
    errors.bindPassword = "realms.errors.clearSaved";
  } else if (
    realm?.backend.kind === "ldap" &&
    saved &&
    !form.clearBindPassword &&
    !form.bindPassword
  ) {
    // 저장된 비밀번호를 다른 서버·계정으로 보내지 않는다(서버도 400).
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1356개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web test`
Expected: 모두 통과

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): OIDC 영역 타입과 API — opl-types RealmBackend::Oidc(OidcRealm{issuer, client_id, client_secret, scopes(기본 openid·profile·email), username_claim(기본 preferred_username), groups_claim(기본 groups), ca_pem, timeout_secs(기본 10)}, deny_unknown_fields, Debug는 client_secret을 가림). 검증: issuer는 호스트가 있는 https:// URL이고 쿼리·조각·사용자 정보·공백이 없음(끝 /는 그대로), client_id·두 클레임 이름 비어 있지 않음, scopes에 openid가 있고 값마다 공백 없는 비어 있지 않은 글, timeout 1~120. LDAP만 있던 반박 불가 패턴을 match로. opl-api RealmBackendView::Oidc(OidcRealmView{…, has_client_secret}), RealmKind::Oidc(로그인 화면 목록 kind oidc). 만들기·바꾸기는 issuer·client_id의 앞뒤 공백을 떼고 ca_pem을 LDAP처럼 다듬은 뒤, client_secret을 빼면 issuer·client_id·ca_pem이 모두 그대로일 때만 저장된 값을 두고 아니면 400 \"client_secret must be given again when issuer, client_id or ca_pem changes\"(저장된 비밀이 없으면 해당 없음 — 공개 클라이언트), \"\"는 지움. POST /auth/login에 OIDC 영역이면 시도로 세지 않고 400 invalid_request \"realm <r> signs in through its identity provider\". OIDC 영역의 시험 동작은 다음 스테이지까지 400. schema.d.ts 다시 만듦, 웹은 타입 컴파일만 맞춤(목록의 URL 칸에 issuer, 종류 OIDC)"
```

---

### Task 2: OIDC 핵심 모듈과 가짜 IdP

`opl_api::oidc`: IdP HTTP 클라이언트(https만, 리다이렉트 없음, 1 MiB 한도), discovery·JWKS 캐시(10분, 모르는 kid면 10초에 한 번 다시 받기, 실패 15초 기억, 영역마다 하나만 받기), ID 토큰 확인(RS256·ES256만, `crit` 거절, iss·aud·azp·exp·iat·nbf·nonce·sub), 상태 쿠키(클러스터 CA 키에서 HKDF로 만든 키로 HMAC)·PKCE, OIDC 영역 시험 동작(discovery·jwks 단계). 시험용 가짜 IdP와 RSA 시험 키 견본. opl-api 직접 의존 `ring`·`base64`(이미 `Cargo.lock`에 있는 판).

**Files:**
- Create: `crates/opl-api/src/oidc.rs`
- Create: `crates/opl-api/tests/common/idp.rs`
- Create: `crates/opl-api/tests/oidc.rs`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/error.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/realms.rs`
- Modify: `crates/opl-api/src/telemetry.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/ldap.rs`
- Modify: `crates/opl-api/tests/realms.rs`
- Modify: `crates/opl-types/src/realm.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/Cargo.toml`에서

```toml
axum.workspace = true
clap.workspace = true
```

을 다음으로 바꾼다:

```toml
axum.workspace = true
# OIDC: JWT·JWKS의 base64url
base64.workspace = true
clap.workspace = true
```

`crates/opl-api/Cargo.toml`에서

```toml
reqwest.workspace = true
roxmltree.workspace = true
```

을 다음으로 바꾼다:

```toml
reqwest.workspace = true
# OIDC: ID 토큰 서명 확인, 상태 쿠키의 HKDF·HMAC, 무작위 값
ring.workspace = true
roxmltree.workspace = true
```

`crates/opl-api/src/error.rs`에서

```rust

impl<T: Serialize> IntoResponse for Json<T> {
```

을 다음으로 바꾼다:

```rust

/// 본문이 없어도 되는 요청(`Option<Json<T>>`). `Content-Type`이 없으면 `None`이다.
impl<T, S> axum::extract::OptionalFromRequest<S> for Json<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(
        req: axum::extract::Request,
        state: &S,
    ) -> Result<Option<Self>, ApiError> {
        let body =
            <axum::Json<T> as axum::extract::OptionalFromRequest<S>>::from_request(req, state)
                .await?;
        Ok(body.map(|axum::Json(value)| Json(value)))
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod limit;
pub mod realms;
```

을 다음으로 바꾼다:

```rust
pub mod limit;
pub mod oidc;
pub mod realms;
```

`crates/opl-api/src/oidc.rs`를 만든다:

```rust
//! OIDC 영역의 핵심. IdP HTTP 클라이언트, discovery·JWKS(노드 메모리 캐시 10분), ID 토큰(JWT)
//! 확인(RS256·ES256만), 로그인 상태 쿠키(클러스터 CA 키에서 HKDF로 만든 키의 HMAC)와 PKCE,
//! 영역 시험 동작의 단계(`discovery`, `jwks`). 상태를 쿠키에 담으므로 어느 노드나 로그인을 끝낼 수 있다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use opl_types::realm::OidcRealm;
use ring::rand::SecureRandom;
use ring::{digest, hkdf, hmac, signature};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::realms::{RealmTestResult, RealmTestStep};

/// 시험 동작의 단계 이름
pub const STEP_DISCOVERY: &str = "discovery";
pub const STEP_JWKS: &str = "jwks";

/// 로그인 상태 쿠키 이름
pub const STATE_COOKIE: &str = "opl_oidc";
/// 로그인 상태 쿠키가 유효한 시간(초)
pub const STATE_TTL_SECS: u64 = 600;
/// ID 토큰의 `exp`·`iat`에 허용하는 시계 차(초)
pub const CLOCK_SKEW_SECS: i64 = 300;
/// discovery·JWKS를 노드 메모리에 두는 시간
pub const CACHE_TTL: Duration = Duration::from_secs(600);
/// 모르는 `kid`로 JWKS를 다시 받는 가장 짧은 간격(받기 폭주 방지)
pub const MIN_REFETCH: Duration = Duration::from_secs(10);
/// discovery·JWKS 받기 실패를 기억하는 시간(막힌 IdP로 요청을 쏟지 않게)
pub const FAILURE_TTL: Duration = Duration::from_secs(15);
/// IdP 응답 본문의 최대 크기
const MAX_BODY: usize = 1 << 20;
/// 상태 쿠키 HMAC 키를 만드는 HKDF 정보
const STATE_KEY_INFO: &[u8] = b"opl-oidc-state";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OidcError {
    /// IdP에 닿지 못했다(연결·TLS·시간 제한·2xx가 아닌 응답).
    #[error("{0}")]
    Unavailable(String),
    /// IdP의 응답이나 토큰·쿠키가 맞지 않다.
    #[error("{0}")]
    Invalid(String),
}

fn invalid(message: impl Into<String>) -> OidcError {
    OidcError::Invalid(message.into())
}

/// 캐시 키: 영역 id와 revision. 영역을 바꾸면 새로 받는다.
pub type RealmKey = (Uuid, u64);

/// discovery 문서에서 쓰는 값
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
}

/// JWKS의 서명 키 하나(RS256 또는 ES256)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jwk {
    pub kid: Option<String>,
    pub key: JwkKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwkKey {
    /// RS256: 모듈러스와 지수(빅 엔디언)
    Rsa { n: Vec<u8>, e: Vec<u8> },
    /// ES256: P-256 비압축 점 `04 || x || y`
    Ec { point: Vec<u8> },
}

impl Jwk {
    fn alg(&self) -> &'static str {
        match self.key {
            JwkKey::Rsa { .. } => "RS256",
            JwkKey::Ec { .. } => "ES256",
        }
    }
}

/// IdP 요청용 HTTP 클라이언트. `ca_pem`이 있으면 그 인증서만 루트로 믿고, 없으면 시스템 루트.
/// 리다이렉트를 따르지 않고 https만 쓴다.
pub fn client(realm: &OidcRealm) -> Result<reqwest::Client, OidcError> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(realm.timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true);
    if let Some(pem) = &realm.ca_pem {
        let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
            .map_err(|e| invalid(format!("ca_pem: {}", crate::telemetry::chain(&e))))?;
        if certs.is_empty() {
            return Err(invalid("ca_pem has no certificates"));
        }
        builder = builder.tls_certs_only(certs);
    }
    builder
        .build()
        .map_err(|e| invalid(crate::telemetry::chain(&e)))
}

/// `url`을 GET해 JSON으로 읽는다. 닿지 못했거나 2xx가 아니면 `Unavailable`.
pub(crate) async fn get_json(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
) -> Result<Value, OidcError> {
    let mut request = client.get(url).header("accept", "application/json");
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|e| OidcError::Unavailable(format!("{url}: {}", crate::telemetry::chain(&e))))?;
    let status = response.status();
    if !status.is_success() {
        return Err(OidcError::Unavailable(format!("{url}: HTTP {status}")));
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| OidcError::Unavailable(format!("{url}: {}", crate::telemetry::chain(&e))))?;
    if body.len() > MAX_BODY {
        return Err(invalid(format!("{url}: response is too large")));
    }
    serde_json::from_slice(&body).map_err(|e| invalid(format!("{url}: {e}")))
}

/// discovery 문서 주소. issuer 끝의 `/`는 하나로 합친다.
pub fn discovery_url(issuer: &str) -> String {
    let base = issuer.strip_suffix('/').unwrap_or(issuer);
    format!("{base}/.well-known/openid-configuration")
}

fn is_https(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty() && !rest.starts_with('/') && !url.chars().any(char::is_whitespace)
    })
}

/// discovery 문서를 받아 확인한다: `issuer`가 설정값과 글자 그대로 같고 끝점이 https다.
pub async fn fetch_discovery(
    client: &reqwest::Client,
    realm: &OidcRealm,
) -> Result<Discovery, OidcError> {
    let url = discovery_url(&realm.issuer);
    let doc: Discovery = serde_json::from_value(get_json(client, &url, None).await?)
        .map_err(|e| invalid(format!("{url}: {e}")))?;
    if doc.issuer != realm.issuer {
        return Err(invalid(format!(
            "the discovery document is for issuer {}, not {}",
            doc.issuer, realm.issuer
        )));
    }
    let endpoints = [
        ("authorization_endpoint", Some(&doc.authorization_endpoint)),
        ("token_endpoint", Some(&doc.token_endpoint)),
        ("jwks_uri", Some(&doc.jwks_uri)),
        ("userinfo_endpoint", doc.userinfo_endpoint.as_ref()),
    ];
    for (name, url) in endpoints {
        if let Some(url) = url
            && !is_https(url)
        {
            return Err(invalid(format!("{name} {url} is not an https:// URL")));
        }
    }
    Ok(doc)
}

/// JWKS를 받아 쓸 수 있는 서명 키만 고른다.
pub async fn fetch_jwks(client: &reqwest::Client, jwks_uri: &str) -> Result<Vec<Jwk>, OidcError> {
    Ok(parse_jwks(&get_json(client, jwks_uri, None).await?))
}

/// JWKS의 `keys`에서 RS256(`kty: RSA`)·ES256(`kty: EC`, `crv: P-256`) 서명 키만 고른다. `alg`가 있으면
/// 그 알고리즘이어야 하고 `use`가 있으면 `sig`여야 한다. 나머지와 깨진 키는 건너뛴다.
pub fn parse_jwks(doc: &Value) -> Vec<Jwk> {
    let Some(keys) = doc.get("keys").and_then(Value::as_array) else {
        return Vec::new();
    };
    keys.iter().filter_map(parse_jwk).collect()
}

fn parse_jwk(k: &Value) -> Option<Jwk> {
    let text = |name: &str| k.get(name).and_then(Value::as_str);
    let bytes = |name: &str| text(name).and_then(|v| URL_SAFE_NO_PAD.decode(v).ok());
    if text("use").is_some_and(|u| u != "sig") {
        return None;
    }
    let alg = text("alg");
    let key = match text("kty")? {
        "RSA" if alg.is_none_or(|a| a == "RS256") => JwkKey::Rsa {
            n: bytes("n")?,
            e: bytes("e")?,
        },
        "EC" if text("crv") == Some("P-256") && alg.is_none_or(|a| a == "ES256") => {
            let (x, y) = (bytes("x")?, bytes("y")?);
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            JwkKey::Ec {
                point: [&[0x04][..], &x, &y].concat(),
            }
        }
        _ => return None,
    };
    Some(Jwk {
        kid: text("kid").map(str::to_string),
        key,
    })
}

/// ID 토큰에서 확인할 값
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
}

/// JWT의 헤더
#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
    /// 꼭 이해해야 하는 확장(RFC 7515 §4.1.11). 아무것도 지원하지 않으므로 있으면 거절한다.
    #[serde(default)]
    crit: Option<Value>,
}

fn decode_part(part: &str, what: &str) -> Result<Vec<u8>, OidcError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| invalid(format!("the ID token {what} is not base64url")))
}

/// ID 토큰 헤더의 `kid`
pub fn token_kid(token: &str) -> Option<String> {
    let header = token.split('.').next()?;
    serde_json::from_slice::<Header>(&URL_SAFE_NO_PAD.decode(header).ok()?)
        .ok()?
        .kid
}

/// ID 토큰(JWS compact)을 `keys`로 확인하고 클레임을 돌려준다. `alg`는 RS256·ES256만이고 `crit`가 있으면
/// 거절한다. `kid`가 있으면 그 키로(JWKS에 그 kid가 없으면 kid 없는 키 가운데 alg가 맞는 것으로), 없으면
/// alg가 맞는 키를 모두 시도한다. 클레임: `iss`, `aud`(client_id 포함, 여럿이면 `azp`), `exp`·`iat`·`nbf`
/// (5분 오차), `nonce`, `sub`. `now`는 유닉스 초.
pub fn verify_id_token(
    token: &str,
    keys: &[Jwk],
    expected: &Expected<'_>,
    now: i64,
) -> Result<Map<String, Value>, OidcError> {
    let parts: Vec<&str> = token.split('.').collect();
    let [header_b64, payload_b64, signature_b64] = parts[..] else {
        return Err(invalid("the ID token is not a signed JWT"));
    };
    let header: Header = serde_json::from_slice(&decode_part(header_b64, "header")?)
        .map_err(|e| invalid(format!("the ID token header: {e}")))?;
    if header.alg != "RS256" && header.alg != "ES256" {
        return Err(invalid(format!(
            "the ID token is signed with {}, not RS256 or ES256",
            header.alg
        )));
    }
    if header.crit.is_some() {
        return Err(invalid("the ID token header has crit extensions"));
    }
    let same_alg = || keys.iter().filter(|k| k.alg() == header.alg);
    let mut candidates: Vec<&Jwk> = same_alg()
        .filter(|k| header.kid.is_none() || k.kid == header.kid)
        .collect();
    // kid가 JWKS에 없으면 kid 없이 내보인 키를 시도한다.
    if candidates.is_empty() && header.kid.is_some() {
        candidates = same_alg().filter(|k| k.kid.is_none()).collect();
    }
    if candidates.is_empty() {
        return Err(invalid(match &header.kid {
            Some(kid) => format!("no {} key with kid {kid} in the JWKS", header.alg),
            None => format!("no {} key in the JWKS", header.alg),
        }));
    }
    let signature = decode_part(signature_b64, "signature")?;
    let input = &token[..header_b64.len() + 1 + payload_b64.len()];
    let verified = candidates.iter().any(|k| match &k.key {
        JwkKey::Rsa { n, e } => signature::RsaPublicKeyComponents { n, e }
            .verify(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                input.as_bytes(),
                &signature,
            )
            .is_ok(),
        JwkKey::Ec { point } => {
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                .verify(input.as_bytes(), &signature)
                .is_ok()
        }
    });
    if !verified {
        return Err(invalid("the ID token signature is not valid"));
    }
    let claims: Map<String, Value> = serde_json::from_slice(&decode_part(payload_b64, "payload")?)
        .map_err(|e| invalid(format!("the ID token claims: {e}")))?;
    check_claims(&claims, expected, now)?;
    Ok(claims)
}

fn check_claims(
    claims: &Map<String, Value>,
    expected: &Expected<'_>,
    now: i64,
) -> Result<(), OidcError> {
    let text = |name: &str| claims.get(name).and_then(Value::as_str);
    let time = |name: &str| {
        claims
            .get(name)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .ok_or_else(|| invalid(format!("the ID token has no {name}")))
    };
    if text("iss") != Some(expected.issuer) {
        return Err(invalid(format!(
            "the ID token issuer is {}, not {}",
            claims.get("iss").unwrap_or(&Value::Null),
            expected.issuer
        )));
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !audiences.contains(&expected.client_id) {
        return Err(invalid("the ID token is not for this client (aud)"));
    }
    // 여러 대상에게 발급된 토큰은 이 클라이언트가 받은 것이어야 한다.
    let azp = text("azp");
    if (audiences.len() > 1 || azp.is_some()) && azp != Some(expected.client_id) {
        return Err(invalid("the ID token was not issued to this client (azp)"));
    }
    if time("exp")? <= now - CLOCK_SKEW_SECS {
        return Err(invalid("the ID token has expired"));
    }
    if time("iat")? > now + CLOCK_SKEW_SECS {
        return Err(invalid("the ID token was issued in the future (iat)"));
    }
    if claims.contains_key("nbf") && time("nbf")? > now + CLOCK_SKEW_SECS {
        return Err(invalid("the ID token is not valid yet (nbf)"));
    }
    if text("nonce") != Some(expected.nonce) {
        return Err(invalid("the ID token nonce does not match"));
    }
    if text("sub").is_none_or(str::is_empty) {
        return Err(invalid("the ID token has no sub"));
    }
    Ok(())
}

/// 지금 유닉스 초
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 영역마다 받은 값(받은 시각)과 최근 실패, 같은 영역의 받기를 하나로 줄이는 잠금
struct Cached<T> {
    entries: Mutex<HashMap<RealmKey, Entry<T>>>,
}

struct Entry<T> {
    value: Option<(Instant, T)>,
    failure: Option<(Instant, OidcError)>,
    fetching: Arc<tokio::sync::Mutex<()>>,
}

impl<T> Default for Cached<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
        }
    }
}

impl<T: Clone> Cached<T> {
    /// 키의 항목. 같은 영역의 더 높은 revision 항목이 있으면 None이다(늦게 끝난 옛 revision 받기가 새
    /// 항목을 지우지 않게). 낮은 revision 항목은 지운다.
    fn entry(map: &mut HashMap<RealmKey, Entry<T>>, key: RealmKey) -> Option<&mut Entry<T>> {
        if map.keys().any(|k| k.0 == key.0 && k.1 > key.1) {
            return None;
        }
        map.retain(|k, _| k.0 != key.0 || k.1 == key.1);
        Some(map.entry(key).or_insert_with(|| Entry {
            value: None,
            failure: None,
            fetching: Arc::default(),
        }))
    }

    /// 받은 값을 `usable`이 쓸 수 있다고 하면 그것, 최근(`FAILURE_TTL` 안)에 실패했으면 그 오류
    fn ready(
        &self,
        key: RealmKey,
        usable: &impl Fn(Instant, &T) -> bool,
    ) -> Option<Result<T, OidcError>> {
        let map = self.entries.lock().unwrap();
        let entry = map.get(&key)?;
        if let Some((at, value)) = &entry.value
            && usable(*at, value)
        {
            return Some(Ok(value.clone()));
        }
        entry
            .failure
            .as_ref()
            .filter(|(at, _)| at.elapsed() < FAILURE_TTL)
            .map(|(_, e)| Err(e.clone()))
    }

    /// 쓸 수 있는 값이나 최근 실패가 없으면 받는다. 같은 영역의 받기는 한 번에 하나만 하고, 기다린
    /// 요청은 그 결과를 쓴다.
    async fn get_or_fetch<F>(
        &self,
        key: RealmKey,
        usable: impl Fn(Instant, &T) -> bool,
        fetch: impl FnOnce() -> F,
    ) -> Result<T, OidcError>
    where
        F: Future<Output = Result<T, OidcError>>,
    {
        if let Some(ready) = self.ready(key, &usable) {
            return ready;
        }
        let lock = Self::entry(&mut self.entries.lock().unwrap(), key)
            .map_or_else(Arc::default, |e| e.fetching.clone());
        let _fetching = lock.lock().await;
        // 기다리는 동안 다른 요청이 받았을 수 있다.
        if let Some(ready) = self.ready(key, &usable) {
            return ready;
        }
        let result = fetch().await;
        if let Some(entry) = Self::entry(&mut self.entries.lock().unwrap(), key) {
            match &result {
                Ok(value) => {
                    entry.value = Some((Instant::now(), value.clone()));
                    entry.failure = None;
                }
                Err(e) => entry.failure = Some((Instant::now(), e.clone())),
            }
        }
        result
    }
}

/// 노드 메모리의 discovery·JWKS 캐시. 키는 (영역 id, revision)이고 영역마다 가장 높은 revision만 둔다.
/// 받기 실패는 `FAILURE_TTL` 동안 기억해 같은 오류를 돌려준다.
pub struct OidcCache {
    ttl: Duration,
    min_refetch: Duration,
    discovery: Cached<Arc<Discovery>>,
    jwks: Cached<Arc<Vec<Jwk>>>,
}

impl Default for OidcCache {
    fn default() -> Self {
        Self::with_times(CACHE_TTL, MIN_REFETCH)
    }
}

impl OidcCache {
    /// 캐시 시간(`ttl`)과 모르는 `kid`로 다시 받는 가장 짧은 간격(`min_refetch`)을 정한다(시험용).
    pub fn with_times(ttl: Duration, min_refetch: Duration) -> Self {
        Self {
            ttl,
            min_refetch,
            discovery: Cached::default(),
            jwks: Cached::default(),
        }
    }

    /// 영역의 discovery 문서. 캐시에 없거나 오래됐으면 받는다.
    pub async fn discovery(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
    ) -> Result<Arc<Discovery>, OidcError> {
        self.discovery
            .get_or_fetch(
                key,
                |at, _| at.elapsed() < self.ttl,
                || async { Ok(Arc::new(fetch_discovery(&client(realm)?, realm).await?)) },
            )
            .await
    }

    /// 영역의 서명 키. 캐시에 없거나 오래됐으면 받는다. `missing_kid`가 있고 캐시에 그 키가 없으면,
    /// 마지막으로 받은 지 `min_refetch`가 지났을 때만 다시 받는다.
    pub async fn keys(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
        jwks_uri: &str,
        kid: Option<&str>,
    ) -> Result<Arc<Vec<Jwk>>, OidcError> {
        let usable = |at: Instant, keys: &Arc<Vec<Jwk>>| {
            let known = kid.is_none_or(|kid| keys.iter().any(|k| k.kid.as_deref() == Some(kid)));
            at.elapsed() < self.ttl && (known || at.elapsed() < self.min_refetch)
        };
        self.jwks
            .get_or_fetch(key, usable, || async {
                Ok(Arc::new(fetch_jwks(&client(realm)?, jwks_uri).await?))
            })
            .await
    }

    /// 영역의 ID 토큰을 확인한다. 모르는 `kid`면 JWKS를 한 번 다시 받는다([`OidcCache::keys`]).
    pub async fn verify_id_token(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
        discovery: &Discovery,
        token: &str,
        nonce: &str,
    ) -> Result<Map<String, Value>, OidcError> {
        let kid = token_kid(token);
        let keys = self
            .keys(key, realm, &discovery.jwks_uri, kid.as_deref())
            .await?;
        let expected = Expected {
            issuer: &realm.issuer,
            client_id: &realm.client_id,
            nonce,
        };
        verify_id_token(token, &keys, &expected, unix_now())
    }
}

/// 영역의 IdP를 시험한다: discovery 문서(issuer 같음·끝점 https)와 JWKS(RS256·ES256 키 수). 캐시를
/// 쓰지 않고 새로 받는다. 실패한 단계에서 멈춘다.
pub async fn test(realm: &OidcRealm) -> RealmTestResult {
    let mut steps = Vec::new();
    let mut step = |name: &str, result: Result<String, OidcError>| {
        let ok = result.is_ok();
        steps.push(RealmTestStep {
            name: name.into(),
            ok,
            message: result.unwrap_or_else(|e| e.to_string()),
        });
        ok
    };
    let discovered = match client(realm) {
        Ok(client) => fetch_discovery(&client, realm)
            .await
            .map(|doc| (client, doc)),
        Err(e) => Err(e),
    };
    let found = discovered.as_ref().map(|(_, doc)| {
        format!(
            "issuer {}, authorization endpoint {}, token endpoint {}",
            doc.issuer, doc.authorization_endpoint, doc.token_endpoint
        )
    });
    if step(STEP_DISCOVERY, found.map_err(Clone::clone))
        && let Ok((client, doc)) = &discovered
    {
        let keys = fetch_jwks(client, &doc.jwks_uri).await.and_then(|keys| {
            let rsa = keys.iter().filter(|k| k.alg() == "RS256").count();
            let ec = keys.len() - rsa;
            if keys.is_empty() {
                Err(invalid(format!(
                    "{} has no RS256 or ES256 signing keys",
                    doc.jwks_uri
                )))
            } else {
                Ok(format!("{rsa} RS256 and {ec} ES256 signing keys"))
            }
        });
        step(STEP_JWKS, keys);
    }
    RealmTestResult {
        steps,
        dn: None,
        groups: Vec::new(),
        role: None,
    }
}

/// 무작위 32바이트(base64url). `state`·`nonce`·PKCE 검증자에 쓴다.
pub fn random_value() -> String {
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("system random");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE S256 `code_challenge` = base64url(SHA-256(verifier))
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}

/// 상태 쿠키의 HMAC 키. 클러스터 CA 개인키(PEM)에서 HKDF-SHA256(salt 없음, 정보 `opl-oidc-state`)으로
/// 만든 32바이트라 모든 노드가 같은 키를 얻는다.
pub fn state_key(ca_key_pem: &str) -> hmac::Key {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(ca_key_pem.as_bytes());
    let okm = prk
        .expand(&[STATE_KEY_INFO], hmac::HMAC_SHA256)
        .expect("32 bytes is a valid HKDF length");
    hmac::Key::from(okm)
}

/// 로그인을 시작할 때 만들어 쿠키에 담는 값
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginState {
    pub realm: String,
    pub state: String,
    pub nonce: String,
    /// PKCE 검증자
    pub verifier: String,
    /// 만든 시각(유닉스 초)
    pub created: i64,
}

// 검증자가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for LoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginState")
            .field("realm", &self.realm)
            .field("created", &self.created)
            .finish_non_exhaustive()
    }
}

/// 상수 시간 비교
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl LoginState {
    /// 영역의 새 로그인 상태(무작위 `state`·`nonce`·검증자)
    pub fn new(realm: &str, now: i64) -> Self {
        Self {
            realm: realm.into(),
            state: random_value(),
            nonce: random_value(),
            verifier: random_value(),
            created: now,
        }
    }

    /// 쿠키 값: base64url(JSON) `.` base64url(HMAC-SHA256)
    pub fn seal(&self, key: &hmac::Key) -> String {
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("serializable"));
        let tag = hmac::sign(key, body.as_bytes());
        format!("{body}.{}", URL_SAFE_NO_PAD.encode(tag))
    }

    /// 쿠키 값을 확인한다: 서명, 만든 지 10분 안, 쿼리의 `state`와 같음.
    pub fn open(
        cookie: &str,
        key: &hmac::Key,
        query_state: &str,
        now: i64,
    ) -> Result<Self, OidcError> {
        let (body, tag) = cookie
            .split_once('.')
            .ok_or_else(|| invalid("the login state cookie is malformed"))?;
        let tag = URL_SAFE_NO_PAD
            .decode(tag)
            .map_err(|_| invalid("the login state cookie is malformed"))?;
        hmac::verify(key, body.as_bytes(), &tag)
            .map_err(|_| invalid("the login state cookie signature is not valid"))?;
        let state: LoginState = URL_SAFE_NO_PAD
            .decode(body)
            .ok()
            .and_then(|json| serde_json::from_slice(&json).ok())
            .ok_or_else(|| invalid("the login state cookie is malformed"))?;
        if now - state.created > STATE_TTL_SECS as i64 || state.created > now + CLOCK_SKEW_SECS {
            return Err(invalid("the login state cookie has expired"));
        }
        if !same(state.state.as_bytes(), query_state.as_bytes()) {
            return Err(invalid("the state does not match the login state cookie"));
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, RsaKeyPair};
    use serde_json::json;

    const RSA_PK8: &[u8] = include_bytes!("../tests/fixtures/oidc-rsa.pk8");
    const ISSUER: &str = "https://idp.example.com/";
    const NOW: i64 = 1_800_000_000;

    fn b64(bytes: impl AsRef<[u8]>) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn rsa() -> RsaKeyPair {
        RsaKeyPair::from_pkcs8(RSA_PK8).unwrap()
    }

    fn rsa_jwk(kid: Option<&str>) -> Jwk {
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = rsa().public().into();
        Jwk {
            kid: kid.map(str::to_string),
            key: JwkKey::Rsa {
                n: public.n,
                e: public.e,
            },
        }
    }

    fn ec() -> EcdsaKeyPair {
        let rng = SystemRandom::new();
        let pk8 = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .unwrap();
        EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pk8.as_ref(),
            &rng,
        )
        .unwrap()
    }

    fn ec_jwk(key: &EcdsaKeyPair, kid: Option<&str>) -> Jwk {
        Jwk {
            kid: kid.map(str::to_string),
            key: JwkKey::Ec {
                point: key.public_key().as_ref().to_vec(),
            },
        }
    }

    fn claims() -> Value {
        json!({
            "iss": ISSUER,
            "sub": "u-1",
            "aud": "oplite",
            "exp": NOW + 60,
            "iat": NOW,
            "nonce": "n-1",
            "preferred_username": "Alice",
        })
    }

    fn input(header: &Value, claims: &Value) -> String {
        format!(
            "{}.{}",
            b64(serde_json::to_vec(header).unwrap()),
            b64(serde_json::to_vec(claims).unwrap())
        )
    }

    fn sign_rs256(header: &Value, claims: &Value) -> String {
        let input = input(header, claims);
        let key = rsa();
        let mut sig = vec![0; key.public().modulus_len()];
        key.sign(
            &signature::RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{input}.{}", b64(sig))
    }

    fn sign_es256(key: &EcdsaKeyPair, header: &Value, claims: &Value) -> String {
        let input = input(header, claims);
        let sig = key.sign(&SystemRandom::new(), input.as_bytes()).unwrap();
        format!("{input}.{}", b64(sig))
    }

    fn expected() -> Expected<'static> {
        Expected {
            issuer: ISSUER,
            client_id: "oplite",
            nonce: "n-1",
        }
    }

    fn verify(token: &str, keys: &[Jwk]) -> Result<Map<String, Value>, OidcError> {
        verify_id_token(token, keys, &expected(), NOW)
    }

    fn err(message: &str) -> Result<Map<String, Value>, OidcError> {
        Err(OidcError::Invalid(message.into()))
    }

    #[test]
    fn rs256_and_es256_tokens_are_verified() {
        let token = sign_rs256(&json!({ "alg": "RS256", "kid": "r1" }), &claims());
        let got = verify(&token, &[rsa_jwk(Some("r1"))]).unwrap();
        assert_eq!(got["preferred_username"], "Alice");
        assert_eq!(token_kid(&token).as_deref(), Some("r1"));

        let key = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256", "kid": "e1" }), &claims());
        let keys = [rsa_jwk(Some("r1")), ec_jwk(&key, Some("e1"))];
        assert_eq!(verify(&token, &keys).unwrap()["sub"], "u-1");

        // kid가 없으면 alg가 맞는 키를 모두 시도한다.
        let other = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256" }), &claims());
        let keys = [ec_jwk(&other, Some("x")), ec_jwk(&key, Some("e1"))];
        assert!(verify(&token, &keys).is_ok());
        assert_eq!(token_kid(&token), None);
    }

    #[test]
    fn other_algorithms_are_rejected() {
        let payload = b64(serde_json::to_vec(&claims()).unwrap());
        // 서명 없는 토큰
        let none = format!("{}.{payload}.", b64(br#"{"alg":"none"}"#));
        assert_eq!(
            verify(&none, &[rsa_jwk(None)]),
            err("the ID token is signed with none, not RS256 or ES256")
        );
        // 공개 키를 HMAC 비밀로 쓰게 하는 공격
        let header = b64(br#"{"alg":"HS256"}"#);
        let input = format!("{header}.{payload}");
        let JwkKey::Rsa { n, .. } = rsa_jwk(None).key else {
            unreachable!()
        };
        let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &n), input.as_bytes());
        let hs256 = format!("{input}.{}", b64(tag));
        assert_eq!(
            verify(&hs256, &[rsa_jwk(None)]),
            err("the ID token is signed with HS256, not RS256 or ES256")
        );
        // 점 둘이 아닌 토큰(JWE 등)
        assert_eq!(
            verify("a.b.c.d.e", &[rsa_jwk(None)]),
            err("the ID token is not a signed JWT")
        );
    }

    #[test]
    fn signatures_and_keys_must_match() {
        let token = sign_rs256(&json!({ "alg": "RS256", "kid": "r1" }), &claims());
        // 클레임을 바꾼 토큰
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        let mut changed = claims();
        changed["preferred_username"] = json!("root");
        parts[1] = b64(serde_json::to_vec(&changed).unwrap());
        assert_eq!(
            verify(&parts.join("."), &[rsa_jwk(Some("r1"))]),
            err("the ID token signature is not valid")
        );
        // 서명 바이트를 바꾼 토큰
        let mut sig = URL_SAFE_NO_PAD
            .decode(token.rsplit('.').next().unwrap())
            .unwrap();
        sig[10] ^= 1;
        let (input, _) = token.rsplit_once('.').unwrap();
        assert_eq!(
            verify(&format!("{input}.{}", b64(sig)), &[rsa_jwk(Some("r1"))]),
            err("the ID token signature is not valid")
        );
        // 다른 키로 서명한 ES256
        let token = sign_es256(&ec(), &json!({ "alg": "ES256", "kid": "e1" }), &claims());
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e1"))]),
            err("the ID token signature is not valid")
        );
        // 모르는 kid, alg가 맞는 키 없음
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e2"))]),
            err("no ES256 key with kid e1 in the JWKS")
        );
        let token = sign_es256(&ec(), &json!({ "alg": "ES256" }), &claims());
        assert_eq!(
            verify(&token, &[rsa_jwk(None)]),
            err("no ES256 key in the JWKS")
        );
    }

    #[test]
    fn claims_are_checked() {
        let keys = [rsa_jwk(None)];
        let check = |f: &dyn Fn(&mut Value)| {
            let mut c = claims();
            f(&mut c);
            verify(&sign_rs256(&json!({ "alg": "RS256" }), &c), &keys).map(|_| ())
        };
        let fail = |m: &str| Err(OidcError::Invalid(m.into()));
        assert_eq!(check(&|_| {}), Ok(()));
        // iss는 글자 그대로(끝 / 포함)
        assert_eq!(
            check(&|c| c["iss"] = json!("https://idp.example.com")),
            fail(
                "the ID token issuer is \"https://idp.example.com\", not https://idp.example.com/"
            )
        );
        let aud = "the ID token is not for this client (aud)";
        assert_eq!(check(&|c| c["aud"] = json!("other")), fail(aud));
        assert_eq!(check(&|c| c["aud"] = json!(["a", "b"])), fail(aud));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("aud");
            }),
            fail(aud)
        );
        assert_eq!(check(&|c| c["aud"] = json!(["oplite"])), Ok(()));
        // 대상이 여럿이면 azp가 이 클라이언트여야 한다.
        let azp = "the ID token was not issued to this client (azp)";
        assert_eq!(check(&|c| c["aud"] = json!(["oplite", "api"])), fail(azp));
        assert_eq!(
            check(&|c| {
                c["aud"] = json!(["oplite", "api"]);
                c["azp"] = json!("api");
            }),
            fail(azp)
        );
        assert_eq!(
            check(&|c| {
                c["aud"] = json!(["oplite", "api"]);
                c["azp"] = json!("oplite");
            }),
            Ok(())
        );
        // exp·iat는 5분 오차
        let expired = "the ID token has expired";
        assert_eq!(check(&|c| c["exp"] = json!(NOW - 300)), fail(expired));
        assert_eq!(check(&|c| c["exp"] = json!(NOW - 299)), Ok(()));
        assert_eq!(
            check(&|c| c["exp"] = json!("soon")),
            fail("the ID token has no exp")
        );
        let future = "the ID token was issued in the future (iat)";
        assert_eq!(check(&|c| c["iat"] = json!(NOW + 301)), fail(future));
        assert_eq!(check(&|c| c["iat"] = json!(NOW + 300)), Ok(()));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("iat");
            }),
            fail("the ID token has no iat")
        );
        let nonce = "the ID token nonce does not match";
        assert_eq!(check(&|c| c["nonce"] = json!("n-2")), fail(nonce));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("nonce");
            }),
            fail(nonce)
        );
        let sub = "the ID token has no sub";
        assert_eq!(check(&|c| c["sub"] = json!("")), fail(sub));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("sub");
            }),
            fail(sub)
        );
    }

    #[test]
    fn crit_headers_and_future_nbf_are_rejected() {
        let keys = [rsa_jwk(None)];
        let token = sign_rs256(&json!({ "alg": "RS256", "crit": ["exp"] }), &claims());
        assert_eq!(
            verify(&token, &keys),
            err("the ID token header has crit extensions")
        );
        let with_nbf = |nbf: Value| {
            let mut c = claims();
            c["nbf"] = nbf;
            verify(&sign_rs256(&json!({ "alg": "RS256" }), &c), &keys).map(|_| ())
        };
        assert_eq!(with_nbf(json!(NOW + 300)), Ok(()));
        assert_eq!(
            with_nbf(json!(NOW + 301)),
            Err(invalid("the ID token is not valid yet (nbf)"))
        );
        assert_eq!(
            with_nbf(json!("later")),
            Err(invalid("the ID token has no nbf"))
        );
    }

    #[test]
    fn an_unknown_kid_falls_back_to_keys_without_a_kid() {
        let key = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256", "kid": "e9" }), &claims());
        // JWKS에 e9가 없으면 kid 없는 ES256 키를 시도한다.
        assert!(verify(&token, &[rsa_jwk(None), ec_jwk(&key, None)]).is_ok());
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), None)]),
            err("the ID token signature is not valid")
        );
        // kid가 있는 키로는 바꾸지 않는다.
        assert_eq!(
            verify(&token, &[ec_jwk(&key, Some("e1"))]),
            err("no ES256 key with kid e9 in the JWKS")
        );
        // JWKS에 e9가 있으면 그 키만 쓴다.
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e9")), ec_jwk(&key, None)]),
            err("the ID token signature is not valid")
        );
    }

    #[test]
    fn jwks_keeps_only_rs256_and_es256_signing_keys() {
        let key = ec();
        let point = key.public_key().as_ref();
        let (x, y) = (b64(&point[1..33]), b64(&point[33..]));
        let JwkKey::Rsa { n, e } = rsa_jwk(None).key else {
            unreachable!()
        };
        let (n, e) = (b64(n), b64(e));
        let doc = json!({ "keys": [
            { "kty": "RSA", "kid": "r1", "n": n, "e": e },
            { "kty": "RSA", "kid": "r2", "alg": "RS256", "use": "sig", "n": n, "e": e },
            { "kty": "EC", "kid": "e1", "crv": "P-256", "x": x, "y": y },
            { "kty": "EC", "kid": "e2", "crv": "P-256", "alg": "ES256", "x": x, "y": y },
            // 쓰지 않는 키
            { "kty": "RSA", "kid": "enc", "use": "enc", "n": n, "e": e },
            { "kty": "RSA", "kid": "ps", "alg": "PS256", "n": n, "e": e },
            { "kty": "EC", "kid": "p384", "crv": "P-384", "x": x, "y": y },
            { "kty": "EC", "kid": "short", "crv": "P-256", "x": b64([1u8; 31]), "y": y },
            { "kty": "oct", "kid": "hs", "k": "c2VjcmV0" },
            { "kty": "RSA", "kid": "broken", "n": "!!", "e": e },
        ]});
        let keys = parse_jwks(&doc);
        let kids: Vec<_> = keys.iter().map(|k| k.kid.as_deref().unwrap()).collect();
        assert_eq!(kids, ["r1", "r2", "e1", "e2"]);
        // EC 점은 비압축 형식 04 || x || y
        assert_eq!(keys[2], ec_jwk(&key, Some("e1")));
        assert_eq!(keys[0], rsa_jwk(Some("r1")));
        assert!(parse_jwks(&json!({})).is_empty());
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        // RFC 7636 부록 B
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let a = random_value();
        assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), 32);
        assert_ne!(a, random_value());
    }

    #[test]
    fn discovery_url_joins_one_slash() {
        assert_eq!(
            discovery_url("https://idp.example.com/realms/corp/"),
            "https://idp.example.com/realms/corp/.well-known/openid-configuration"
        );
        assert_eq!(
            discovery_url("https://idp.example.com"),
            "https://idp.example.com/.well-known/openid-configuration"
        );
    }

    #[test]
    fn state_cookies_are_signed_and_expire() {
        let key = state_key("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        let state = LoginState::new("okta", NOW);
        assert_ne!(state.state, state.nonce);
        let cookie = state.seal(&key);
        assert_eq!(
            LoginState::open(&cookie, &key, &state.state, NOW),
            Ok(state.clone())
        );
        assert_eq!(
            LoginState::open(&cookie, &key, &state.state, NOW + 600),
            Ok(state.clone())
        );
        let open = |cookie: &str, key: &hmac::Key, query: &str, now: i64| {
            LoginState::open(cookie, key, query, now).map(|_| ())
        };
        let fail = |m: &str| Err(OidcError::Invalid(m.into()));
        assert_eq!(
            open(&cookie, &key, &state.state, NOW + 601),
            fail("the login state cookie has expired")
        );
        assert_eq!(
            open(&cookie, &key, "other", NOW),
            fail("the state does not match the login state cookie")
        );
        assert_eq!(
            open(&cookie, &key, &state.state[1..], NOW),
            fail("the state does not match the login state cookie")
        );
        // 다른 클러스터의 키
        let other = state_key("other");
        assert_eq!(
            open(&cookie, &other, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        // 바뀐 바이트(본문을 바꾸면 서명이 맞지 않는다)
        let mut forged = LoginState {
            created: NOW + 3600,
            ..state.clone()
        }
        .seal(&key);
        let (_, tag) = cookie.split_once('.').unwrap();
        let (body, _) = forged.split_once('.').unwrap();
        forged = format!("{body}.{tag}");
        assert_eq!(
            open(&forged, &key, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        let mut bytes = cookie.clone().into_bytes();
        bytes[3] = if bytes[3] == b'A' { b'B' } else { b'A' };
        assert_eq!(
            open(&String::from_utf8(bytes).unwrap(), &key, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        for bad in ["", "nodot", "a.!!"] {
            assert_eq!(
                open(bad, &key, &state.state, NOW),
                fail("the login state cookie is malformed")
            );
        }
        // 같은 CA 키에서는 같은 쿠키 키(다른 노드도 확인할 수 있다)
        let again = state_key("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        assert!(LoginState::open(&cookie, &again, &state.state, NOW).is_ok());
        // 로그에 검증자·state를 내지 않는다.
        let shown = format!("{state:?}");
        assert!(!shown.contains(&state.verifier), "{shown}");
        assert!(!shown.contains(&state.state), "{shown}");
    }
}
```

`crates/opl-api/src/realms.rs`에서

```rust

/// 영역 시험 본문
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RealmTestRequest {
    pub username: String,
    pub password: String,
}
```

을 다음으로 바꾼다:

```rust

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
```

`crates/opl-api/src/realms.rs`에서

```rust

/// 영역 시험의 결과. 단계는 `connect`, `service_bind`, `search`, `user_bind`, `groups` 순이고
/// 실패한 단계에서 멈춘다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmTestResult {
    pub steps: Vec<RealmTestStep>,
    /// 찾은 사용자 항목. 찾기 전에 멈췄으면 null
    #[schema(required = true)]
    pub dn: Option<String>,
    /// 모든 단계가 성공했을 때의 그룹 속성 값들
    pub groups: Vec<String>,
    /// 그룹에서 정해질 역할. 실패했거나 맞는 매핑이 없으면 null
    #[schema(required = true)]
```

을 다음으로 바꾼다:

```rust

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
```

`crates/opl-api/src/realms.rs`에서

```rust

/// 사용자 이름과 비밀번호로 영역의 디렉터리를 시험한다. 사용자도 세션도 만들지 않는다.
#[utoipa::path(post, path = "/realms/{key}/actions/test", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = RealmTestRequest,
    responses((status = 200, body = RealmTestResult), (status = "4XX", body = crate::error::ErrorBody)))]
```

을 다음으로 바꾼다:

```rust

/// 영역을 시험한다. LDAP 영역은 사용자 이름과 비밀번호로 디렉터리를 확인하고, OIDC 영역은 IdP의
/// discovery 문서와 서명 키를 받아 본다. 사용자도 세션도 만들지 않는다.
#[utoipa::path(post, path = "/realms/{key}/actions/test", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = Option<RealmTestRequest>,
    responses((status = 200, body = RealmTestResult), (status = "4XX", body = crate::error::ErrorBody)))]
```

`crates/opl-api/src/realms.rs`에서

```rust
    Path(key): Path<String>,
    Json(req): Json<RealmTestRequest>,
) -> Result<Json<RealmTestResult>, ApiError> {
    state.authorize(&principal, Action::Operate, RealmSpec::KIND)?;
    let realm = lookup::<RealmSpec>(&state.cluster, &key)?;
    // OIDC 영역의 시험은 다음 단계에서 더한다.
    if let RealmBackend::Oidc(_) = realm.spec.backend {
        return Err(ApiError::bad_request(
            "testing an OIDC realm is not supported yet",
        ));
    }
    Ok(Json(
        crate::ldap::test(&realm.spec, &req.username, &req.password).await,
    ))
}
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-api/src/realms.rs`에서

```rust
        let req = RealmTestRequest {
            username: "alice".into(),
            password: "s3cret".into(),
        };
```

을 다음으로 바꾼다:

```rust
        let req = RealmTestRequest {
            username: Some("alice".into()),
            password: Some("s3cret".into()),
        };
```

`crates/opl-api/src/telemetry.rs`에서

```rust
/// 오류와 그 원인들을 `: `로 잇는다(reqwest의 겉 오류만으로는 이유가 보이지 않는다).
fn chain(e: &dyn Error) -> String {
    let mut text = e.to_string();
```

을 다음으로 바꾼다:

```rust
/// 오류와 그 원인들을 `: `로 잇는다(reqwest의 겉 오류만으로는 이유가 보이지 않는다).
pub(crate) fn chain(e: &dyn Error) -> String {
    let mut text = e.to_string();
```

`crates/opl-api/tests/common/idp.rs`를 만든다:

```rust
//! 시험용 가짜 OIDC 제공자(IdP). 시험 CA가 서명한 `localhost`·`127.0.0.1` 인증서의 HTTPS로
//! discovery(`/.well-known/openid-configuration`), `/authorize`(쿼리를 기록하고 곧바로
//! `redirect_uri?code=..&state=..`로 302), `/token`(코드·PKCE S256·클라이언트 인증을 확인하고
//! ID 토큰·액세스 토큰), `/jwks`, `/userinfo`를 낸다. 서명 키는 RS256(저장소 견본
//! `tests/fixtures/oidc-rsa.pk8`, kid `rsa-1`)과 ES256(시험마다 새로, kid `ec-1`)이고 처음엔 RS256으로
//! 서명한다. 시험이 discovery 문서, 클레임, 서명 키·kid·알고리즘, 공개할 키, userinfo, 실패 응답을 정한다.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use opl_peer::pki::{ClusterCa, NodeTls, first_cert, node_key_and_csr};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair, RsaKeyPair};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 저장소의 RS256 시험 키(openssl로 한 번 만든 2048비트 PKCS#8 DER — ring은 RSA 키를 만들지 못한다)
const RSA_PK8: &[u8] = include_bytes!("../fixtures/oidc-rsa.pk8");

pub const CLIENT_ID: &str = "oplite";
pub const CLIENT_SECRET: &str = "client-secret";
/// 기본 ID 토큰의 `sub`
pub const SUBJECT: &str = "alice-sub";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alg {
    Rs256,
    Es256,
}

#[derive(Clone)]
enum Signer {
    Rsa(Arc<RsaKeyPair>),
    Ec(Arc<EcdsaKeyPair>),
}

#[derive(Clone)]
struct Key {
    kid: String,
    signer: Signer,
    /// JWKS에 내보이는지
    published: bool,
}

impl Key {
    fn new(kid: &str, alg: Alg) -> Key {
        let signer = match alg {
            Alg::Rs256 => Signer::Rsa(Arc::new(RsaKeyPair::from_pkcs8(RSA_PK8).unwrap())),
            Alg::Es256 => {
                let rng = SystemRandom::new();
                let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
                let pk8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
                Signer::Ec(Arc::new(
                    EcdsaKeyPair::from_pkcs8(alg, pk8.as_ref(), &rng).unwrap(),
                ))
            }
        };
        Key {
            kid: kid.into(),
            signer,
            published: true,
        }
    }

    fn alg(&self) -> &'static str {
        match self.signer {
            Signer::Rsa(_) => "RS256",
            Signer::Ec(_) => "ES256",
        }
    }

    fn jwk(&self) -> Value {
        match &self.signer {
            Signer::Rsa(k) => {
                let public: signature::RsaPublicKeyComponents<Vec<u8>> = k.public().into();
                json!({ "kty": "RSA", "kid": self.kid, "use": "sig", "alg": "RS256",
                        "n": b64(public.n), "e": b64(public.e) })
            }
            Signer::Ec(k) => {
                let point = k.public_key().as_ref();
                json!({ "kty": "EC", "kid": self.kid, "crv": "P-256",
                        "x": b64(&point[1..33]), "y": b64(&point[33..]) })
            }
        }
    }

    fn sign(&self, input: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        match &self.signer {
            Signer::Rsa(k) => {
                let mut sig = vec![0; k.public().modulus_len()];
                k.sign(&signature::RSA_PKCS1_SHA256, &rng, input, &mut sig)
                    .unwrap();
                sig
            }
            Signer::Ec(k) => k.sign(&rng, input).unwrap().as_ref().to_vec(),
        }
    }
}

fn b64(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// `/authorize`가 낸 코드
#[derive(Clone)]
struct Grant {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    nonce: Option<String>,
}

struct Inner {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    discovery: Value,
    keys: Vec<Key>,
    /// 서명할 키의 kid
    signing: String,
    /// ID 토큰 헤더에 kid를 넣는지
    header_kid: bool,
    /// 기본 클레임에 덮어쓸 값(null이면 뺀다)
    claims: Map<String, Value>,
    userinfo: Value,
    /// `/authorize`가 코드 대신 돌려보낼 오류
    authorize_error: Option<String>,
    /// `/token`의 실패 응답
    token_failure: Option<(u16, Value)>,
    codes: HashMap<String, Grant>,
    access_tokens: Vec<String>,
    authorize_queries: Vec<HashMap<String, String>>,
    token_requests: Vec<(Option<String>, HashMap<String, String>)>,
    hits: HashMap<&'static str, usize>,
}

#[derive(Clone)]
struct Site(Arc<Mutex<Inner>>);

impl Site {
    fn hit(&self, path: &'static str) -> std::sync::MutexGuard<'_, Inner> {
        let mut inner = self.0.lock().unwrap();
        *inner.hits.entry(path).or_default() += 1;
        inner
    }
}

pub struct Idp {
    /// `https://127.0.0.1:<port>`
    pub issuer: String,
    /// 서버 인증서에 서명한 CA(PEM)
    pub ca_pem: String,
    /// 같은 CA(DER)
    pub ca_der: Vec<u8>,
    site: Site,
    server: JoinHandle<()>,
}

impl Idp {
    /// 기밀 클라이언트(`oplite`/`client-secret`)의 IdP
    pub async fn start() -> Idp {
        Idp::start_with(CLIENT_ID, Some(CLIENT_SECRET)).await
    }

    /// `client_secret`이 없으면 공개 클라이언트(PKCE만)
    pub async fn start_with(client_id: &str, client_secret: Option<&str>) -> Idp {
        let ca = ClusterCa::generate("idp").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let sans = ["localhost".to_string(), "127.0.0.1".to_string()];
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, uuid::Uuid::new_v4(), &sans).unwrap(),
            key_pem,
        };
        let config = opl_peer::tls::api_server_config(&tls).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("https://{}", tcp.local_addr().unwrap());
        let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
        let site = Site(Arc::new(Mutex::new(Inner {
            discovery: json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "jwks_uri": format!("{issuer}/jwks"),
                "userinfo_endpoint": format!("{issuer}/userinfo"),
                "response_types_supported": ["code"],
                "id_token_signing_alg_values_supported": ["RS256", "ES256"],
            }),
            issuer: issuer.clone(),
            client_id: client_id.into(),
            client_secret: client_secret.map(str::to_string),
            keys: vec![Key::new("rsa-1", Alg::Rs256), Key::new("ec-1", Alg::Es256)],
            signing: "rsa-1".into(),
            header_kid: true,
            claims: Map::new(),
            userinfo: json!({
                "sub": SUBJECT,
                "preferred_username": "Alice",
                "groups": ["opl-admins"],
            }),
            authorize_error: None,
            token_failure: None,
            codes: HashMap::new(),
            access_tokens: Vec::new(),
            authorize_queries: Vec::new(),
            token_requests: Vec::new(),
            hits: HashMap::new(),
        })));
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get(discovery),
            )
            .route("/authorize", axum::routing::get(authorize))
            .route("/token", axum::routing::post(token))
            .route("/jwks", axum::routing::get(jwks))
            .route("/userinfo", axum::routing::get(userinfo))
            .with_state(site.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Idp {
            issuer,
            ca_der: first_cert(&ca.cert_pem).unwrap().to_vec(),
            ca_pem: ca.cert_pem,
            site,
            server,
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.site.0.lock().unwrap()
    }

    /// 이 IdP를 쓰는 영역의 `backend`(시험 CA를 믿는다)
    pub fn backend(&self) -> Value {
        let inner = self.inner();
        let mut backend = json!({
            "kind": "oidc",
            "issuer": inner.issuer,
            "client_id": inner.client_id,
            "ca_pem": self.ca_pem,
            // macOS에서는 새 시험 CA로 하는 첫 TLS 연결의 인증서 검증이 15초쯤 걸리기도 한다.
            "timeout_secs": 30,
        });
        if let Some(secret) = &inner.client_secret {
            backend["client_secret"] = json!(secret);
        }
        backend
    }

    /// 경로(`discovery`, `authorize`, `token`, `jwks`, `userinfo`)가 불린 횟수
    pub fn hits(&self, path: &str) -> usize {
        self.inner().hits.get(path).copied().unwrap_or(0)
    }

    /// discovery 문서를 고친다.
    pub fn set_discovery(&self, f: impl FnOnce(&mut Value)) {
        f(&mut self.inner().discovery);
    }

    /// 서명 키를 더한다(`published`면 JWKS에도). 서명 키는 [`Idp::sign_with`]로 고른다.
    pub fn add_key(&self, kid: &str, alg: Alg, published: bool) {
        let mut key = Key::new(kid, alg);
        key.published = published;
        self.inner().keys.push(key);
    }

    /// JWKS에 키를 내보일지
    pub fn publish(&self, kid: &str, published: bool) {
        for key in self.inner().keys.iter_mut().filter(|k| k.kid == kid) {
            key.published = published;
        }
    }

    /// 앞으로 `kid`의 키로 서명한다.
    pub fn sign_with(&self, kid: &str) {
        self.inner().signing = kid.into();
    }

    /// ID 토큰 헤더에 kid를 넣을지(기본 넣음)
    pub fn set_header_kid(&self, on: bool) {
        self.inner().header_kid = on;
    }

    /// 앞으로 낼 ID 토큰의 클레임을 덮어쓴다(null이면 뺀다). 앞서 정한 것에 더한다.
    pub fn set_claims(&self, claims: Value) {
        let mut inner = self.inner();
        for (k, v) in claims.as_object().unwrap() {
            inner.claims.insert(k.clone(), v.clone());
        }
    }

    pub fn set_userinfo(&self, userinfo: Value) {
        self.inner().userinfo = userinfo;
    }

    /// `/authorize`가 코드 대신 `error`로 돌려보낸다(None이면 되돌림).
    pub fn set_authorize_error(&self, error: Option<&str>) {
        self.inner().authorize_error = error.map(str::to_string);
    }

    /// `/token`이 `status`와 `body`로 실패한다(None이면 되돌림).
    pub fn fail_token(&self, failure: Option<(u16, Value)>) {
        self.inner().token_failure = failure;
    }

    /// `/authorize`가 받은 쿼리(받은 순서)
    pub fn authorize_queries(&self) -> Vec<HashMap<String, String>> {
        self.inner().authorize_queries.clone()
    }

    /// `/token`이 받은 (Authorization 헤더, 폼 본문)(받은 순서)
    pub fn token_requests(&self) -> Vec<(Option<String>, HashMap<String, String>)> {
        self.inner().token_requests.clone()
    }

    /// 기본 클레임(`iss`, `sub`, `aud`, `exp` 5분 뒤, `iat` 지금, `nonce`, `preferred_username` Alice,
    /// `groups` [opl-admins])에 [`Idp::set_claims`]를 덮어쓴 것
    pub fn claims(&self, nonce: Option<&str>) -> Value {
        default_claims(&self.inner(), nonce)
    }

    /// 지금 서명 키로 `claims`에 서명한 ID 토큰
    pub fn id_token(&self, claims: &Value) -> String {
        sign(&self.inner(), claims)
    }

    /// 헤더까지 정해 지금 서명 키로 서명한다(`alg` 거짓말 시험).
    pub fn sign_raw(&self, header: &Value, claims: &Value) -> String {
        let inner = self.inner();
        let key = inner.keys.iter().find(|k| k.kid == inner.signing).unwrap();
        signed(key, header, claims)
    }
}

impl Drop for Idp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn signed(key: &Key, header: &Value, claims: &Value) -> String {
    let input = format!(
        "{}.{}",
        b64(serde_json::to_vec(header).unwrap()),
        b64(serde_json::to_vec(claims).unwrap())
    );
    let sig = key.sign(input.as_bytes());
    format!("{input}.{}", b64(sig))
}

fn sign(inner: &Inner, claims: &Value) -> String {
    let key = inner.keys.iter().find(|k| k.kid == inner.signing).unwrap();
    let mut header = json!({ "alg": key.alg(), "typ": "JWT" });
    if inner.header_kid {
        header["kid"] = json!(key.kid);
    }
    signed(key, &header, claims)
}

async fn discovery(State(site): State<Site>) -> Response {
    axum::Json(site.hit("discovery").discovery.clone()).into_response()
}

async fn jwks(State(site): State<Site>) -> Response {
    let inner = site.hit("jwks");
    let keys: Vec<Value> = inner
        .keys
        .iter()
        .filter(|k| k.published)
        .map(Key::jwk)
        .collect();
    axum::Json(json!({ "keys": keys })).into_response()
}

async fn authorize(
    State(site): State<Site>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut inner = site.hit("authorize");
    inner.authorize_queries.push(query.clone());
    let redirect = query.get("redirect_uri").cloned().unwrap_or_default();
    let Ok(mut location) = reqwest::Url::parse(&redirect) else {
        return (StatusCode::BAD_REQUEST, "bad redirect_uri").into_response();
    };
    {
        let mut pairs = location.query_pairs_mut();
        if let Some(error) = &inner.authorize_error {
            pairs.append_pair("error", error);
            pairs.append_pair("error_description", "the user said no");
        } else {
            let code = b64(uuid::Uuid::new_v4().as_bytes());
            inner.codes.insert(
                code.clone(),
                Grant {
                    client_id: query.get("client_id").cloned().unwrap_or_default(),
                    redirect_uri: redirect.clone(),
                    challenge: query.get("code_challenge").cloned().unwrap_or_default(),
                    nonce: query.get("nonce").cloned(),
                },
            );
            pairs.append_pair("code", &code);
        }
        if let Some(state) = query.get("state") {
            pairs.append_pair("state", state);
        }
    }
    (
        StatusCode::FOUND,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}

fn token_error(status: StatusCode, error: &str) -> Response {
    (status, axum::Json(json!({ "error": error }))).into_response()
}

async fn token(
    State(site): State<Site>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> Response {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut inner = site.hit("token");
    inner
        .token_requests
        .push((authorization.clone(), form.clone()));
    if let Some((status, body)) = inner.token_failure.clone() {
        return (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response();
    }
    // 클라이언트 인증: 기밀 클라이언트는 client_secret_basic, 공개 클라이언트는 폼의 client_id
    let client_ok = match &inner.client_secret {
        Some(secret) => authorization
            .as_deref()
            .and_then(|a| a.strip_prefix("Basic "))
            .and_then(|b| STANDARD.decode(b).ok())
            .is_some_and(|b| b == format!("{}:{secret}", inner.client_id).into_bytes()),
        None => authorization.is_none() && form.get("client_id") == Some(&inner.client_id),
    };
    if !client_ok {
        return token_error(StatusCode::UNAUTHORIZED, "invalid_client");
    }
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return token_error(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }
    // 코드는 한 번만 쓴다.
    let Some(grant) = form.get("code").and_then(|c| inner.codes.remove(c)) else {
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    let challenge = b64(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ));
    if grant.client_id != inner.client_id
        || form.get("redirect_uri") != Some(&grant.redirect_uri)
        || challenge != grant.challenge
    {
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    let access_token = b64(uuid::Uuid::new_v4().as_bytes());
    inner.access_tokens.push(access_token.clone());
    let id_token = sign(&inner, &default_claims(&inner, grant.nonce.as_deref()));
    axum::Json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token,
    }))
    .into_response()
}

fn default_claims(inner: &Inner, nonce: Option<&str>) -> Value {
    let mut claims = json!({
        "iss": inner.issuer,
        "sub": SUBJECT,
        "aud": inner.client_id,
        "exp": now() + 300,
        "iat": now(),
        "preferred_username": "Alice",
        "groups": ["opl-admins"],
    });
    if let Some(nonce) = nonce {
        claims["nonce"] = json!(nonce);
    }
    let map = claims.as_object_mut().unwrap();
    for (k, v) in &inner.claims {
        if v.is_null() {
            map.remove(k);
        } else {
            map.insert(k.clone(), v.clone());
        }
    }
    claims
}

async fn userinfo(State(site): State<Site>, headers: HeaderMap) -> Response {
    let inner = site.hit("userinfo");
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|a| a.strip_prefix("Bearer "));
    if !token.is_some_and(|t| inner.access_tokens.iter().any(|a| a == t)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(inner.userinfo.clone()).into_response()
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
pub mod fake_apt;
pub mod ldap;
```

을 다음으로 바꾼다:

```rust
pub mod fake_apt;
pub mod idp;
pub mod ldap;
```

`crates/opl-api/tests/ldap.rs`에서

```rust

    // 없는 영역
```

을 다음으로 바꾼다:

```rust

    // LDAP 영역은 사용자와 비밀번호가 있어야 한다.
    for body in [json!({}), json!({ "username": "alice" })] {
        let (status, got) = admin.post("/realms/corp/actions/test", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
        assert_eq!(
            got["message"],
            "username and password are required to test an LDAP realm"
        );
    }

    // 없는 영역
```

`crates/opl-api/tests/oidc.rs`를 만든다:

```rust
//! OIDC 핵심 모듈과 가짜 IdP: discovery(issuer 다름·http 끝점·믿지 않는 CA 거절, 캐시), JWKS(캐시,
//! 모르는 kid 뒤 다시 받기, 10초 안에는 다시 받지 않음), 받기 실패 기억과 동시 받기 하나로, 가장 높은
//! revision, 캐시를 거친 ID 토큰 확인, 영역 시험 동작의 `discovery`·`jwks` 단계.

mod common;

use std::time::Duration;

use common::Env;
use common::idp::{Alg, Idp};
use opl_api::oidc::{OidcCache, OidcError, RealmKey};
use opl_types::realm::{OidcRealm, RealmBackend};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn realm(idp: &Idp) -> OidcRealm {
    match serde_json::from_value(idp.backend()).unwrap() {
        RealmBackend::Oidc(o) => o,
        RealmBackend::Ldap(_) => unreachable!(),
    }
}

fn key() -> RealmKey {
    (Uuid::new_v4(), 1)
}

fn invalid(message: impl Into<String>) -> OidcError {
    OidcError::Invalid(message.into())
}

#[tokio::test]
async fn discovery_is_checked_and_cached() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let k = key();
    let doc = cache.discovery(k, &realm).await.unwrap();
    assert_eq!(doc.issuer, idp.issuer);
    assert_eq!(doc.jwks_uri, format!("{}/jwks", idp.issuer));
    assert_eq!(
        doc.userinfo_endpoint.as_deref(),
        Some(format!("{}/userinfo", idp.issuer).as_str())
    );
    // 10분 동안 다시 받지 않는다.
    assert_eq!(*cache.discovery(k, &realm).await.unwrap(), *doc);
    assert_eq!(idp.hits("discovery"), 1);
    // 영역이 바뀌면(revision) 새로 받는다.
    cache.discovery((k.0, 2), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 2);
    // 캐시 시간이 지나면 새로 받는다.
    let cache = OidcCache::with_times(Duration::ZERO, Duration::ZERO);
    cache.discovery(k, &realm).await.unwrap();
    cache.discovery(k, &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 4);

    // issuer 끝의 /는 discovery 주소에서 하나로 합치지만, 문서의 issuer와는 글자 그대로 비교한다.
    let slash = OidcRealm {
        issuer: format!("{}/", idp.issuer),
        ..realm.clone()
    };
    assert_eq!(
        cache.discovery(key(), &slash).await,
        Err(invalid(format!(
            "the discovery document is for issuer {0}, not {0}/",
            idp.issuer
        )))
    );
    assert_eq!(idp.hits("discovery"), 5);
}

#[tokio::test]
async fn bad_discovery_documents_are_rejected() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    assert_eq!(
        cache.discovery(key(), &realm).await,
        Err(invalid(format!(
            "the discovery document is for issuer https://evil.example.com, not {}",
            idp.issuer
        )))
    );
    idp.set_discovery(|d| d["issuer"] = json!(idp.issuer));
    for field in [
        "authorization_endpoint",
        "token_endpoint",
        "jwks_uri",
        "userinfo_endpoint",
    ] {
        let old = idp.issuer.clone();
        let http = old.replace("https://", "http://") + "/x";
        idp.set_discovery(|d| d[field] = json!(http));
        assert_eq!(
            cache.discovery(key(), &realm).await,
            Err(invalid(format!("{field} {http} is not an https:// URL")))
        );
        idp.set_discovery(|d| d[field] = json!(format!("{old}/x")));
    }
    // userinfo는 없어도 된다. jwks_uri는 있어야 한다.
    idp.set_discovery(|d| {
        d.as_object_mut().unwrap().remove("userinfo_endpoint");
    });
    assert_eq!(
        cache
            .discovery(key(), &realm)
            .await
            .unwrap()
            .userinfo_endpoint,
        None
    );
    idp.set_discovery(|d| {
        d.as_object_mut().unwrap().remove("jwks_uri");
    });
    let Err(OidcError::Invalid(message)) = cache.discovery(key(), &realm).await else {
        panic!("a document without jwks_uri was accepted");
    };
    assert!(message.contains("missing field `jwks_uri`"), "{message}");
}

#[tokio::test]
async fn idps_are_reached_only_with_a_trusted_certificate() {
    let idp = Idp::start().await;
    let cache = OidcCache::default();
    // 시스템 루트는 시험 CA를 믿지 않는다.
    let system = OidcRealm {
        ca_pem: None,
        ..realm(&idp)
    };
    assert!(
        matches!(
            cache.discovery(key(), &system).await,
            Err(OidcError::Unavailable(_))
        ),
        "the test CA was trusted without ca_pem"
    );
    // 다른 CA를 주면 시스템 루트도 쓰지 않는다.
    let other = Idp::start().await;
    let wrong = OidcRealm {
        ca_pem: Some(other.ca_pem.clone()),
        ..realm(&idp)
    };
    assert!(matches!(
        cache.discovery(key(), &wrong).await,
        Err(OidcError::Unavailable(_))
    ));
    assert_eq!(idp.hits("discovery"), 0);
    // 닿지 않는 IdP
    let gone = realm(&other);
    drop(other);
    assert!(matches!(
        cache.discovery(key(), &gone).await,
        Err(OidcError::Unavailable(_))
    ));
}

#[tokio::test]
async fn id_tokens_are_verified_with_cached_keys() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let k = key();
    let doc = cache.discovery(k, &realm).await.unwrap();
    let token = idp.id_token(&idp.claims(Some("n-1")));
    let claims = cache
        .verify_id_token(k, &realm, &doc, &token, "n-1")
        .await
        .unwrap();
    assert_eq!(claims["preferred_username"], "Alice");
    assert_eq!(claims["groups"], json!(["opl-admins"]));
    // ES256 키도 같은 JWKS에 있다.
    idp.sign_with("ec-1");
    let token = idp.id_token(&idp.claims(Some("n-1")));
    cache
        .verify_id_token(k, &realm, &doc, &token, "n-1")
        .await
        .unwrap();
    assert_eq!(idp.hits("jwks"), 1);
    // nonce가 다르면 거절
    assert_eq!(
        cache.verify_id_token(k, &realm, &doc, &token, "n-2").await,
        Err(invalid("the ID token nonce does not match"))
    );
    // alg를 none으로 바꾼 토큰
    let none = idp.sign_raw(&json!({ "alg": "none" }), &idp.claims(Some("n-1")));
    assert_eq!(
        cache.verify_id_token(k, &realm, &doc, &none, "n-1").await,
        Err(invalid(
            "the ID token is signed with none, not RS256 or ES256"
        ))
    );
    // aud·iss가 다른 토큰
    idp.set_claims(json!({ "aud": "someone-else" }));
    let token = idp.id_token(&idp.claims(Some("n-1")));
    assert_eq!(
        cache.verify_id_token(k, &realm, &doc, &token, "n-1").await,
        Err(invalid("the ID token is not for this client (aud)"))
    );
    assert_eq!(idp.hits("jwks"), 1);
}

#[tokio::test]
async fn unknown_key_ids_refetch_the_jwks_at_most_every_ten_seconds() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    // 다시 받는 간격을 길게 해 느린 기계에서도 "방금 받음"이 유지되게 한다.
    let cache = OidcCache::with_times(Duration::from_secs(600), Duration::from_secs(3600));
    let k = key();
    let doc = cache.discovery(k, &realm).await.unwrap();
    let check = async |cache: &OidcCache, kid: &str| {
        idp.sign_with(kid);
        let token = idp.id_token(&idp.claims(Some("n")));
        cache.verify_id_token(k, &realm, &doc, &token, "n").await
    };
    check(&cache, "rsa-1").await.unwrap();
    assert_eq!(idp.hits("jwks"), 1);

    // IdP가 새 키로 바꿨다. 방금 받았으므로(10초 안) 다시 받지 않고 거절한다.
    idp.add_key("rsa-2", Alg::Rs256, true);
    assert_eq!(
        check(&cache, "rsa-2").await,
        Err(invalid("no RS256 key with kid rsa-2 in the JWKS"))
    );
    assert_eq!(idp.hits("jwks"), 1);

    // 10초가 지났다면 한 번 다시 받는다.
    let cache = OidcCache::with_times(Duration::from_secs(600), Duration::ZERO);
    check(&cache, "rsa-1").await.unwrap();
    assert_eq!(idp.hits("jwks"), 2);
    idp.add_key("ec-2", Alg::Es256, true);
    check(&cache, "ec-2").await.unwrap();
    assert_eq!(idp.hits("jwks"), 3);
    check(&cache, "ec-2").await.unwrap();
    assert_eq!(idp.hits("jwks"), 3);
    // JWKS에 없는 키는 다시 받아도 없다(한 번만 다시 받는다).
    idp.add_key("hidden", Alg::Es256, false);
    assert_eq!(
        check(&cache, "hidden").await,
        Err(invalid("no ES256 key with kid hidden in the JWKS"))
    );
    assert_eq!(idp.hits("jwks"), 4);
    // 키가 바뀐 뒤 옛 키로 서명한 토큰은 거절된다.
    idp.publish("rsa-1", false);
    idp.add_key("rsa-3", Alg::Rs256, true);
    check(&cache, "rsa-3").await.unwrap();
    assert_eq!(idp.hits("jwks"), 5);
    assert_eq!(
        check(&cache, "rsa-1").await,
        Err(invalid("no RS256 key with kid rsa-1 in the JWKS"))
    );
    assert_eq!(idp.hits("jwks"), 6);
}

/// 받기 실패는 같은 (영역, revision)에서 15초 동안 기억해 다시 받지 않고 같은 오류를 돌려준다.
#[tokio::test]
async fn fetch_failures_are_remembered() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let k = key();
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    let failed = cache.discovery(k, &realm).await;
    assert!(matches!(failed, Err(OidcError::Invalid(_))), "{failed:?}");
    // IdP가 고쳐져도 기억한 동안은 받지 않는다.
    idp.set_discovery(|d| d["issuer"] = json!(idp.issuer));
    assert_eq!(cache.discovery(k, &realm).await, failed);
    assert_eq!(idp.hits("discovery"), 1);
    // 영역이 바뀌면(revision) 새로 받는다.
    cache.discovery((k.0, 2), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 2);

    // JWKS도 같다(액세스 토큰 없는 userinfo는 401).
    let unauthorized = format!("{}/userinfo", idp.issuer);
    let failed = cache.keys(k, &realm, &unauthorized, None).await;
    assert!(
        matches!(failed, Err(OidcError::Unavailable(_))),
        "{failed:?}"
    );
    assert_eq!(cache.keys(k, &realm, &unauthorized, None).await, failed);
    assert_eq!(idp.hits("userinfo"), 1);
}

/// 같은 영역을 동시에 여러 요청이 물어도 IdP에서는 한 번만 받는다(실패도).
#[tokio::test]
async fn concurrent_requests_fetch_once() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let k = key();
    let docs = futures_util::future::join_all((0..8).map(|_| cache.discovery(k, &realm))).await;
    assert!(docs.iter().all(Result::is_ok), "{docs:?}");
    assert_eq!(idp.hits("discovery"), 1);
    let jwks = format!("{}/jwks", idp.issuer);
    let keys =
        futures_util::future::join_all((0..8).map(|_| cache.keys(k, &realm, &jwks, None))).await;
    assert!(keys.iter().all(Result::is_ok), "{keys:?}");
    assert_eq!(idp.hits("jwks"), 1);
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    let k = key();
    let docs = futures_util::future::join_all((0..8).map(|_| cache.discovery(k, &realm))).await;
    assert!(docs.iter().all(Result::is_err), "{docs:?}");
    assert_eq!(idp.hits("discovery"), 2);
}

/// 늦게 끝난 옛 revision 받기가 새 revision 항목을 지우지 않는다.
#[tokio::test]
async fn the_cache_keeps_the_highest_revision() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let id = Uuid::new_v4();
    cache.discovery((id, 2), &realm).await.unwrap();
    // 옛 revision을 읽은 요청: 받기는 하지만 남기지 않는다.
    cache.discovery((id, 1), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 2);
    cache.discovery((id, 2), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 2);
    // 더 높은 revision은 옛 항목을 바꾼다.
    cache.discovery((id, 3), &realm).await.unwrap();
    cache.discovery((id, 3), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 3);
}

fn steps(result: &Value) -> Vec<(String, bool)> {
    result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap().into(), s["ok"] == true))
        .collect()
}

/// 영역 시험 동작은 OIDC면 사용자·비밀번호 없이 discovery와 jwks만 확인한다.
#[tokio::test]
async fn the_test_action_checks_discovery_and_jwks() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let idp = Idp::start().await;
    let (status, body) = admin
        .post(
            "/realms",
            json!({ "name": "okta", "backend": idp.backend() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let url = format!("{}/api/v1/realms/okta/actions/test", admin.base);
    let token = admin.token.clone().unwrap();
    // 본문 없이
    let (status, result) = admin.send(admin.http.post(&url).bearer_auth(&token)).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        steps(&result),
        [("discovery".to_string(), true), ("jwks".to_string(), true)]
    );
    assert_eq!(
        result["steps"][1]["message"],
        "1 RS256 and 1 ES256 signing keys"
    );
    assert_eq!(result["dn"], Value::Null);
    assert_eq!(result["groups"], json!([]));
    assert_eq!(result["role"], Value::Null);
    assert!(!result.to_string().contains("client-secret"));
    // 본문이 null이어도 된다(OpenAPI의 nullable 본문).
    let (status, result) = admin
        .send(admin.http.post(&url).bearer_auth(&token).json(&Value::Null))
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(steps(&result).len(), 2);
    // 사용자·비밀번호는 무시한다. 시험은 캐시를 쓰지 않는다.
    let (status, result) = admin
        .post(
            "/realms/okta/actions/test",
            json!({ "username": "alice", "password": "pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(steps(&result).len(), 2);
    assert_eq!((idp.hits("discovery"), idp.hits("jwks")), (3, 3));

    // 서명 키가 없으면 jwks에서 실패
    idp.publish("rsa-1", false);
    idp.publish("ec-1", false);
    let (_, result) = admin.post("/realms/okta/actions/test", json!({})).await;
    assert_eq!(
        steps(&result),
        [("discovery".to_string(), true), ("jwks".to_string(), false)]
    );
    assert_eq!(
        result["steps"][1]["message"],
        format!("{}/jwks has no RS256 or ES256 signing keys", idp.issuer)
    );
    // issuer가 다르면 discovery에서 멈춘다.
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    let (_, result) = admin.post("/realms/okta/actions/test", json!({})).await;
    assert_eq!(steps(&result), [("discovery".to_string(), false)]);
    assert!(
        result["steps"][0]["message"]
            .as_str()
            .unwrap()
            .contains("https://evil.example.com"),
        "{result}"
    );
    // 닿지 않는 IdP
    drop(idp);
    let (_, result) = admin.post("/realms/okta/actions/test", json!({})).await;
    assert_eq!(steps(&result), [("discovery".to_string(), false)]);

    // operator는 시험할 수 없다(LDAP와 같은 권한).
    let operator = env.user("op", "operator").await;
    assert_eq!(
        operator
            .post("/realms/okta/actions/test", json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
```

`crates/opl-api/tests/realms.rs`에서

```rust
        "ca_pem": "-----BEGIN CERTIFICATE-----",
        "timeout_secs": 120
    });
```

을 다음으로 바꾼다:

```rust
        "ca_pem": "-----BEGIN CERTIFICATE-----",
        "timeout_secs": 30
    });
```

`crates/opl-api/tests/realms.rs`에서

```rust
            "ca_pem": "-----BEGIN CERTIFICATE-----",
            "timeout_secs": 120,
            "has_client_secret": true
```

을 다음으로 바꾼다:

```rust
            "ca_pem": "-----BEGIN CERTIFICATE-----",
            "timeout_secs": 30,
            "has_client_secret": true
```

`crates/opl-api/tests/realms.rs`에서

```rust
        (
            with(&|b| b["backend"]["timeout_secs"] = json!(121)),
            "timeout_secs must be between 1 and 120",
        ),
```

을 다음으로 바꾼다:

```rust
        (
            with(&|b| b["backend"]["timeout_secs"] = json!(31)),
            "timeout_secs must be between 1 and 30",
        ),
```

`crates/opl-types/src/realm.rs`에서

```rust
        }
        if !(1..=120).contains(&self.timeout_secs) {
            return Err(RealmError::OidcTimeout);
```

을 다음으로 바꾼다:

```rust
        }
        if !(1..=30).contains(&self.timeout_secs) {
            return Err(RealmError::OidcTimeout);
```

`crates/opl-types/src/realm.rs`에서

```rust
    GroupsClaim,
    #[error("timeout_secs must be between 1 and 120")]
    OidcTimeout,
```

을 다음으로 바꾼다:

```rust
    GroupsClaim,
    #[error("timeout_secs must be between 1 and 30")]
    OidcTimeout,
```

`crates/opl-types/src/realm.rs`에서

```rust
                OidcRealm {
                    timeout_secs: 121,
                    ..oidc()
```

을 다음으로 바꾼다:

```rust
                OidcRealm {
                    timeout_secs: 31,
                    ..oidc()
```

`crates/opl-types/src/realm.rs`에서

```rust
        }
        for timeout_secs in [1, 120] {
            let o = OidcRealm {
```

을 다음으로 바꾼다:

```rust
        }
        for timeout_secs in [1, 30] {
            let o = OidcRealm {
```

`crates/opl-types/src/realm.rs`에서

```rust
            RealmError::OidcTimeout.to_string(),
            "timeout_secs must be between 1 and 120"
        );
```

을 다음으로 바꾼다:

```rust
            RealmError::OidcTimeout.to_string(),
            "timeout_secs must be between 1 and 30"
        );
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `mkdir -p crates/opl-api/tests/fixtures && printf '%s' 'MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCmH8CcWtc0TtaNGQmtUPLkHTwT95Mit0+M1y1ObSo/i9qIGhsArZVgc0cDdGUU1CI/ozRm4j789B1jc139gfdPoxxv0dJouf7cOMlPSS1sJHDWoNgvKqajUzLF37GpnERxS7cVd5RyyC1Vg0KUw8h41i98yF85aplX3JC1zvDhT5+tGDParn62rY9RgYN3P9EsS6p5cEmtcayYcXxHAF06U0S5tJy9f5JBYAfaIVhgQdhgVXktNp9IGuYfVxIXDC7mVjldhRaa/0C/GjOW4ZZtFrgUo1rDNDq9KPcXhA0yiEWULDc1aYQFiG+AHWER0pj0UZs7pMThICaTaM/qhcN/AgMBAAECggEABc5VSoGoRNU+jWbjHjIWkx9scpw0AcIbXyZEGJobIGZwaFJVQERQ1LKiuD2wrLYvTj8TfUcKAYg8oLWvAsWwkZF3VHcmXe+mNycq7gD2O/HYGcA8XgWlFk85lo3XZUCnabhGOVNZFlvlrbftM0rWbla6CSftoBGYM2hrjFcjtxQla7bJaSNkGgJMbuEpSUFGBXg6Snp2eXHfH4PdmcjModZJGVTREx87VwnVG4AWOcxkf5FL/+BZs1TF7gtfUYaEyAsDM0+Mwq1IzUE9tmmiENngjzrthQHbEV5ei2ntNOLX+KuoKsdViJNAIAPcm3iHsgCMmGrancr626XHETY00QKBgQDPR+ukfcK9JvlPBmQxXoxGkkDfp13nkwHqE689XwPqSzIyjcQCU9Gzpdr6dyq7h0qwJFch9gHchdV98APyCxyT1/c99rnOmvfAuXNrR+hWD8uAcADR068eHJpR63AGJGZ5pFrt9ByDMXhCdeeMpxBjHk6thVQALdJ1BVwpI/7X8QKBgQDNK2x56YtxYWNlGbQTEeB/EAoJYvKz7SH2ju5EwdRHRjQ++gu3iOIuJ5lgnXN8ZG+p4ymWLSn3ffwry7UaeEFiU/foFfWTo+3aKC6xjAPfITnZBWz4mNwAYZWHTIyqXcJ55iKrgSzHnUQf7v8pmfbjrzOJLrFxMd4iXk9oWvpCbwKBgQCmPLfvpCyaJrereSTBs2Is9v6wYDofjvbqE/9m6Nzq+oO3p5qbgz2bJlkIpqnO3CI6QerfgbzcbzlZVzTS2baBm6LePxGlcQNhvVmjoHflDaXXy/vcTDkVnTrmmDuvsWG+DOIgFLcJQJOCOa0KULIDzDqtT1CfFCDFWRcgp42rAQKBgD3Hc5SKJHjxkFJjGPwdB/J6y+rjpp1TLETn6Qs/KkKD+ztzY4/SYfjv55lWD+5G9/C5/E3CL77catBtOI1N5LJNAnsaEaZxWC+2pcjElm3bvWWB4rxAmw5qU/I3tKugK+00xP/Ll8ZLrrXG64KFX/hTSztlmfyxvWev6k1MB8t1AoGAWAXrAilxo0zKjSCnRLNBf88CjgO6BP/IznlszDLkl2hSGfGP1yF29gEdgil2ZY6O8heZd8lfrvf0BR5sV2UoT9an0mTaC4xxpI/9mJwBCWMICZB+c0Tn1PuMUqastK2ApHA21H1sOBvYB/ovQ6fwSe9XBa6ZfAefgYIKHP+3qvU=' | base64 -d > crates/opl-api/tests/fixtures/oidc-rsa.pk8`
Expected: 출력 없음 (RSA 2048비트 PKCS#8 DER 시험 키 — `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -outform DER`로 만든 것)

Run: `cargo check --workspace`
Expected: `Cargo.lock`의 opl-api 의존 목록 줄만 바뀐다(새 패키지 없음, 이 Task의 커밋에 넣는다)

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1375개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add Cargo.lock crates web/src/api/schema.d.ts
git commit -m "feat(api): OIDC 핵심 모듈과 가짜 IdP — opl-api oidc.rs: IdP HTTP 클라이언트(reqwest, 리다이렉트 없음·https만·timeout_secs, ca_pem이 있으면 그 인증서만 루트), discovery(<issuer>/.well-known/openid-configuration — 끝 /는 하나로 합침, 문서의 issuer가 설정값과 글자 그대로 같고 authorization·token·jwks_uri·userinfo 끝점은 https)와 JWKS(kty RSA·EC P-256만, alg가 있으면 RS256·ES256, use가 있으면 sig) 노드 메모리 캐시 OidcCache(키는 (영역 id, revision), 10분, 모르는 kid면 마지막으로 받은 지 10초가 지났을 때만 한 번 다시 받음), ID 토큰 확인(ring: RS256 RSA_PKCS1_2048_8192_SHA256·ES256 ECDSA_P256_SHA256_FIXED 04||x||y만, none·HS256 등 거절, kid가 있으면 그 키 아니면 alg가 맞는 키 모두; iss 같음, aud에 client_id, aud가 여럿이거나 azp가 있으면 azp == client_id, exp·iat 5분 오차, nonce, sub), 로그인 상태 쿠키 LoginState{realm, state, nonce, verifier, created}(base64url(JSON).base64url(HMAC-SHA256), 키는 클러스터 CA 개인키 PEM에서 HKDF-SHA256(salt 없음, 정보 opl-oidc-state), 10분, state 상수 시간 비교), 무작위 32바이트 base64url 값과 PKCE S256 code_challenge. 영역 시험 동작은 OIDC면 사용자·비밀번호 없이(본문 없음·null도 됨) discovery·jwks 단계를 캐시 없이 돌리고, RealmTestRequest의 username·password는 선택(LDAP은 없으면 400). OIDC 시간 제한을 LDAP처럼 1~30으로. 시험 견본 RSA 키 tests/fixtures/oidc-rsa.pk8(openssl 2048비트 PKCS#8 DER), 가짜 IdP tests/common/idp.rs(시험 CA의 HTTPS, discovery·authorize·token(코드·PKCE·client_secret_basic 또는 공개 클라이언트)·jwks·userinfo, 클레임·키·kid·공개 여부·실패 응답을 시험이 정함). ring·base64를 opl-api 직접 의존으로(판 그대로). schema.d.ts 다시 만듦"
```

---

### Task 3: OIDC 로그인 흐름 끝점

`GET /api/v1/auth/oidc/{realm}/start`(상태 쿠키 `opl_oidc`를 놓고 IdP로 302 — 리디렉트 URI는 요청 Host로 만들어 서명한 상태에 넣음)와 `GET /api/v1/auth/oidc/callback`(쿠키·state 확인 → 토큰 교환(`client_secret_basic` 또는 공개 클라이언트) → ID 토큰 확인 → 필요하면 userinfo → 역할 → 외부 사용자 만들기(IdP `sub`에 묶음) → 세션 → `/`, 실패는 `/login?error=<sso_failed|no_role|realm_unavailable>`). RFC 9207 `iss` 확인, `invalid_grant`는 sso_failed. 콜백은 시도 제한을 쓰지 않는다. 감사 로그가 콜백 GET을 적는다.

**Files:**
- Create: `crates/opl-api/src/oidc_login.rs`
- Create: `crates/opl-api/tests/oidc_login.rs`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/audit.rs`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/oidc.rs`
- Modify: `crates/opl-api/src/state.rs`
- Modify: `crates/opl-api/src/users.rs`
- Modify: `crates/opl-api/tests/common/idp.rs`
- Modify: `crates/opl-api/tests/oidc.rs`
- Modify: `crates/opl-api/tests/realms.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/Cargo.toml`에서

```toml
opl-types = { workspace = true, features = ["openapi"] }
reqwest.workspace = true
# OIDC: ID 토큰 서명 확인, 상태 쿠키의 HKDF·HMAC, 무작위 값
```

을 다음으로 바꾼다:

```toml
opl-types = { workspace = true, features = ["openapi"] }
# OIDC 토큰 교환의 폼 본문
reqwest = { workspace = true, features = ["form"] }
# OIDC: ID 토큰 서명 확인, 상태 쿠키의 HKDF·HMAC, 무작위 값
```

`crates/opl-api/src/audit.rs`에서

```rust

/// 라우트가 맞은 요청 가운데 바꾸는 것(GET·HEAD·OPTIONS가 아닌 것)과 콘솔 열기를 적는다.
pub(crate) async fn layer(
```

을 다음으로 바꾼다:

```rust

/// 라우트가 맞은 요청 가운데 바꾸는 것(GET·HEAD·OPTIONS가 아닌 것)과 콘솔 열기, OIDC 로그인 콜백을 적는다.
pub(crate) async fn layer(
```

`crates/opl-api/src/audit.rs`에서

```rust
    let read = matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    if read && template != "/vms/{key}/console" {
        return next.run(req).await;
```

을 다음으로 바꾼다:

```rust
    let read = matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    if read && template != "/vms/{key}/console" && template != "/auth/oidc/callback" {
        return next.run(req).await;
```

`crates/opl-api/src/cluster.rs`에서

```rust
/// 클러스터 정보 리소스. 클러스터를 만들 때 하나 생긴다.
fn cluster_info(state: &AppState) -> Result<ClusterInfo, ApiError> {
    state
```

을 다음으로 바꾼다:

```rust
/// 클러스터 정보 리소스. 클러스터를 만들 때 하나 생긴다.
pub(crate) fn cluster_info(state: &AppState) -> Result<ClusterInfo, ApiError> {
    state
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod oidc;
pub mod realms;
```

을 다음으로 바꾼다:

```rust
pub mod oidc;
mod oidc_login;
pub mod realms;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(realms::list_login_realms))
        .routes(routes!(users::list_users, users::create_user))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(realms::list_login_realms))
        .routes(routes!(oidc_login::oidc_start))
        .routes(routes!(oidc_login::oidc_callback))
        .routes(routes!(users::list_users, users::create_user))
```

`crates/opl-api/src/oidc.rs` 전체:

```rust
//! OIDC 영역의 핵심. IdP HTTP 클라이언트, discovery·JWKS(노드 메모리 캐시 10분), ID 토큰(JWT)
//! 확인(RS256·ES256만), 로그인 상태 쿠키(클러스터 CA 키에서 HKDF로 만든 키의 HMAC)와 PKCE,
//! 영역 시험 동작의 단계(`discovery`, `jwks`). 상태를 쿠키에 담으므로 어느 노드나 로그인을 끝낼 수 있다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use opl_types::realm::OidcRealm;
use ring::rand::SecureRandom;
use ring::{digest, hkdf, hmac, signature};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::realms::{RealmTestResult, RealmTestStep};

/// 시험 동작의 단계 이름
pub const STEP_DISCOVERY: &str = "discovery";
pub const STEP_JWKS: &str = "jwks";

/// 로그인 상태 쿠키 이름
pub const STATE_COOKIE: &str = "opl_oidc";
/// 로그인 상태 쿠키가 유효한 시간(초)
pub const STATE_TTL_SECS: u64 = 600;
/// ID 토큰의 `exp`·`iat`에 허용하는 시계 차(초)
pub const CLOCK_SKEW_SECS: i64 = 300;
/// discovery·JWKS를 노드 메모리에 두는 시간
pub const CACHE_TTL: Duration = Duration::from_secs(600);
/// 모르는 `kid`로 JWKS를 다시 받는 가장 짧은 간격(받기 폭주 방지)
pub const MIN_REFETCH: Duration = Duration::from_secs(10);
/// discovery·JWKS 받기 실패를 기억하는 시간(막힌 IdP로 요청을 쏟지 않게)
pub const FAILURE_TTL: Duration = Duration::from_secs(15);
/// IdP 응답 본문의 최대 크기
const MAX_BODY: usize = 1 << 20;
/// 상태 쿠키 HMAC 키를 만드는 HKDF 정보
const STATE_KEY_INFO: &[u8] = b"opl-oidc-state";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OidcError {
    /// IdP에 닿지 못했다(연결·TLS·시간 제한·2xx가 아닌 응답 — 아래 것 말고).
    #[error("{0}")]
    Unavailable(String),
    /// IdP의 응답이나 토큰·쿠키가 맞지 않다. 토큰 끝점의 `invalid_grant`와 userinfo의 401·403도
    /// (IdP가 이 로그인을 거절했다).
    #[error("{0}")]
    Invalid(String),
}

fn invalid(message: impl Into<String>) -> OidcError {
    OidcError::Invalid(message.into())
}

/// 캐시 키: 영역 id와 revision. 영역을 바꾸면 새로 받는다.
pub type RealmKey = (Uuid, u64);

/// discovery 문서에서 쓰는 값
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    /// RFC 9207: 인가 응답에 `iss`를 넣는다.
    #[serde(default)]
    pub authorization_response_iss_parameter_supported: bool,
}

/// JWKS의 서명 키 하나(RS256 또는 ES256)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jwk {
    pub kid: Option<String>,
    pub key: JwkKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwkKey {
    /// RS256: 모듈러스와 지수(빅 엔디언)
    Rsa { n: Vec<u8>, e: Vec<u8> },
    /// ES256: P-256 비압축 점 `04 || x || y`
    Ec { point: Vec<u8> },
}

impl Jwk {
    fn alg(&self) -> &'static str {
        match self.key {
            JwkKey::Rsa { .. } => "RS256",
            JwkKey::Ec { .. } => "ES256",
        }
    }
}

/// IdP 요청용 HTTP 클라이언트. `ca_pem`이 있으면 그 인증서만 루트로 믿고, 없으면 시스템 루트.
/// 리다이렉트를 따르지 않고 https만 쓴다.
pub fn client(realm: &OidcRealm) -> Result<reqwest::Client, OidcError> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(realm.timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true);
    if let Some(pem) = &realm.ca_pem {
        let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
            .map_err(|e| invalid(format!("ca_pem: {}", crate::telemetry::chain(&e))))?;
        if certs.is_empty() {
            return Err(invalid("ca_pem has no certificates"));
        }
        builder = builder.tls_certs_only(certs);
    }
    builder
        .build()
        .map_err(|e| invalid(crate::telemetry::chain(&e)))
}

/// `url`을 GET해 JSON으로 읽는다. 닿지 못했거나 2xx가 아니면 `Unavailable`. 액세스 토큰(`bearer`)을
/// 보낸 요청(userinfo)이 401·403이면 IdP가 이 로그인을 거절한 것이므로 `Invalid`.
pub(crate) async fn get_json(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
) -> Result<Value, OidcError> {
    let mut request = client.get(url).header("accept", "application/json");
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    let response = request.send().await;
    if bearer.is_some()
        && let Ok(r) = &response
        && matches!(
            r.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        )
    {
        return Err(invalid(format!("{url}: HTTP {}", r.status())));
    }
    read_json(url, response).await
}

/// IdP 응답을 JSON으로 읽는다. 본문은 `MAX_BODY`까지만 읽는다(Content-Length가 크면 바로, 아니면 읽다가
/// 넘으면 멈춤). 닿지 못했거나 2xx가 아니면 `Unavailable`(오류 본문의 `error`를 붙임). 다만 4xx의
/// `invalid_grant`(토큰 끝점 — 코드가 틀렸거나 이미 썼거나 만료)는 `Invalid`.
async fn read_json(
    url: &str,
    response: reqwest::Result<reqwest::Response>,
) -> Result<Value, OidcError> {
    let unavailable = |e: reqwest::Error| {
        OidcError::Unavailable(format!("{url}: {}", crate::telemetry::chain(&e)))
    };
    let too_large = || invalid(format!("{url}: response is too large"));
    let mut response = response.map_err(unavailable)?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(unavailable)? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let error = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(str::to_string));
        let message = format!(
            "{url}: HTTP {status}{}",
            error
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        );
        if status.is_client_error() && error.as_deref() == Some("invalid_grant") {
            return Err(invalid(message));
        }
        return Err(OidcError::Unavailable(message));
    }
    serde_json::from_slice(&body).map_err(|e| invalid(format!("{url}: {e}")))
}

/// 토큰 끝점의 응답에서 쓰는 값
#[derive(Deserialize)]
pub struct Tokens {
    pub id_token: String,
    #[serde(default)]
    pub access_token: Option<String>,
}

// 토큰이 로그에 나가지 않게 가린다.
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").finish_non_exhaustive()
    }
}

/// application/x-www-form-urlencoded 값 하나(client_secret_basic의 client_id와 비밀, RFC 6749 §2.3.1)
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 인가 코드를 토큰으로 바꾼다(PKCE 검증자). 폼에는 늘 `client_id`를 넣고, 비밀이 있으면
/// client_secret_basic(client_id와 비밀을 각각 form-urlencode한 `id:secret`의 Basic 인증)으로 보낸다.
pub async fn exchange_code(
    client: &reqwest::Client,
    realm: &OidcRealm,
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Tokens, OidcError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
        ("client_id", realm.client_id.as_str()),
    ];
    let mut request = client
        .post(token_endpoint)
        .header("accept", "application/json")
        .form(&form);
    if let Some(secret) = &realm.client_secret {
        request = request.basic_auth(form_encode(&realm.client_id), Some(form_encode(secret)));
    }
    let body = read_json(token_endpoint, request.send().await).await?;
    serde_json::from_value(body).map_err(|e| invalid(format!("{token_endpoint}: {e}")))
}

/// discovery 문서 주소. issuer 끝의 `/`는 하나로 합친다.
pub fn discovery_url(issuer: &str) -> String {
    let base = issuer.strip_suffix('/').unwrap_or(issuer);
    format!("{base}/.well-known/openid-configuration")
}

fn is_https(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty() && !rest.starts_with('/') && !url.chars().any(char::is_whitespace)
    })
}

/// discovery 문서를 받아 확인한다: `issuer`가 설정값과 글자 그대로 같고 끝점이 https다.
pub async fn fetch_discovery(
    client: &reqwest::Client,
    realm: &OidcRealm,
) -> Result<Discovery, OidcError> {
    let url = discovery_url(&realm.issuer);
    let doc: Discovery = serde_json::from_value(get_json(client, &url, None).await?)
        .map_err(|e| invalid(format!("{url}: {e}")))?;
    if doc.issuer != realm.issuer {
        return Err(invalid(format!(
            "the discovery document is for issuer {}, not {}",
            doc.issuer, realm.issuer
        )));
    }
    let endpoints = [
        ("authorization_endpoint", Some(&doc.authorization_endpoint)),
        ("token_endpoint", Some(&doc.token_endpoint)),
        ("jwks_uri", Some(&doc.jwks_uri)),
        ("userinfo_endpoint", doc.userinfo_endpoint.as_ref()),
    ];
    for (name, url) in endpoints {
        if let Some(url) = url
            && !is_https(url)
        {
            return Err(invalid(format!("{name} {url} is not an https:// URL")));
        }
    }
    Ok(doc)
}

/// JWKS를 받아 쓸 수 있는 서명 키만 고른다.
pub async fn fetch_jwks(client: &reqwest::Client, jwks_uri: &str) -> Result<Vec<Jwk>, OidcError> {
    Ok(parse_jwks(&get_json(client, jwks_uri, None).await?))
}

/// JWKS의 `keys`에서 RS256(`kty: RSA`)·ES256(`kty: EC`, `crv: P-256`) 서명 키만 고른다. `alg`가 있으면
/// 그 알고리즘이어야 하고 `use`가 있으면 `sig`여야 한다. 나머지와 깨진 키는 건너뛴다.
pub fn parse_jwks(doc: &Value) -> Vec<Jwk> {
    let Some(keys) = doc.get("keys").and_then(Value::as_array) else {
        return Vec::new();
    };
    keys.iter().filter_map(parse_jwk).collect()
}

fn parse_jwk(k: &Value) -> Option<Jwk> {
    let text = |name: &str| k.get(name).and_then(Value::as_str);
    let bytes = |name: &str| text(name).and_then(|v| URL_SAFE_NO_PAD.decode(v).ok());
    if text("use").is_some_and(|u| u != "sig") {
        return None;
    }
    let alg = text("alg");
    let key = match text("kty")? {
        "RSA" if alg.is_none_or(|a| a == "RS256") => JwkKey::Rsa {
            n: bytes("n")?,
            e: bytes("e")?,
        },
        "EC" if text("crv") == Some("P-256") && alg.is_none_or(|a| a == "ES256") => {
            let (x, y) = (bytes("x")?, bytes("y")?);
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            JwkKey::Ec {
                point: [&[0x04][..], &x, &y].concat(),
            }
        }
        _ => return None,
    };
    Some(Jwk {
        kid: text("kid").map(str::to_string),
        key,
    })
}

/// ID 토큰에서 확인할 값
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
}

/// JWT의 헤더
#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
    /// 꼭 이해해야 하는 확장(RFC 7515 §4.1.11). 아무것도 지원하지 않으므로 있으면 거절한다.
    #[serde(default)]
    crit: Option<Value>,
}

fn decode_part(part: &str, what: &str) -> Result<Vec<u8>, OidcError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| invalid(format!("the ID token {what} is not base64url")))
}

/// ID 토큰 헤더의 `kid`
pub fn token_kid(token: &str) -> Option<String> {
    let header = token.split('.').next()?;
    serde_json::from_slice::<Header>(&URL_SAFE_NO_PAD.decode(header).ok()?)
        .ok()?
        .kid
}

/// ID 토큰(JWS compact)을 `keys`로 확인하고 클레임을 돌려준다. `alg`는 RS256·ES256만이고 `crit`가 있으면
/// 거절한다. `kid`가 있으면 그 키로(JWKS에 그 kid가 없으면 kid 없는 키 가운데 alg가 맞는 것으로), 없으면
/// alg가 맞는 키를 모두 시도한다. 클레임: `iss`, `aud`(client_id 포함, 여럿이면 `azp`), `exp`·`iat`·`nbf`
/// (5분 오차), `nonce`, `sub`. `now`는 유닉스 초.
pub fn verify_id_token(
    token: &str,
    keys: &[Jwk],
    expected: &Expected<'_>,
    now: i64,
) -> Result<Map<String, Value>, OidcError> {
    let parts: Vec<&str> = token.split('.').collect();
    let [header_b64, payload_b64, signature_b64] = parts[..] else {
        return Err(invalid("the ID token is not a signed JWT"));
    };
    let header: Header = serde_json::from_slice(&decode_part(header_b64, "header")?)
        .map_err(|e| invalid(format!("the ID token header: {e}")))?;
    if header.alg != "RS256" && header.alg != "ES256" {
        return Err(invalid(format!(
            "the ID token is signed with {}, not RS256 or ES256",
            header.alg
        )));
    }
    if header.crit.is_some() {
        return Err(invalid("the ID token header has crit extensions"));
    }
    let same_alg = || keys.iter().filter(|k| k.alg() == header.alg);
    let mut candidates: Vec<&Jwk> = same_alg()
        .filter(|k| header.kid.is_none() || k.kid == header.kid)
        .collect();
    // kid가 JWKS에 없으면 kid 없이 내보인 키를 시도한다.
    if candidates.is_empty() && header.kid.is_some() {
        candidates = same_alg().filter(|k| k.kid.is_none()).collect();
    }
    if candidates.is_empty() {
        return Err(invalid(match &header.kid {
            Some(kid) => format!("no {} key with kid {kid} in the JWKS", header.alg),
            None => format!("no {} key in the JWKS", header.alg),
        }));
    }
    let signature = decode_part(signature_b64, "signature")?;
    let input = &token[..header_b64.len() + 1 + payload_b64.len()];
    let verified = candidates.iter().any(|k| match &k.key {
        JwkKey::Rsa { n, e } => signature::RsaPublicKeyComponents { n, e }
            .verify(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                input.as_bytes(),
                &signature,
            )
            .is_ok(),
        JwkKey::Ec { point } => {
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                .verify(input.as_bytes(), &signature)
                .is_ok()
        }
    });
    if !verified {
        return Err(invalid("the ID token signature is not valid"));
    }
    let claims: Map<String, Value> = serde_json::from_slice(&decode_part(payload_b64, "payload")?)
        .map_err(|e| invalid(format!("the ID token claims: {e}")))?;
    check_claims(&claims, expected, now)?;
    Ok(claims)
}

fn check_claims(
    claims: &Map<String, Value>,
    expected: &Expected<'_>,
    now: i64,
) -> Result<(), OidcError> {
    let text = |name: &str| claims.get(name).and_then(Value::as_str);
    let time = |name: &str| {
        claims
            .get(name)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .ok_or_else(|| invalid(format!("the ID token has no {name}")))
    };
    if text("iss") != Some(expected.issuer) {
        return Err(invalid(format!(
            "the ID token issuer is {}, not {}",
            claims.get("iss").unwrap_or(&Value::Null),
            expected.issuer
        )));
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !audiences.contains(&expected.client_id) {
        return Err(invalid("the ID token is not for this client (aud)"));
    }
    // 여러 대상에게 발급된 토큰은 이 클라이언트가 받은 것이어야 한다.
    let azp = text("azp");
    if (audiences.len() > 1 || azp.is_some()) && azp != Some(expected.client_id) {
        return Err(invalid("the ID token was not issued to this client (azp)"));
    }
    if time("exp")? <= now - CLOCK_SKEW_SECS {
        return Err(invalid("the ID token has expired"));
    }
    if time("iat")? > now + CLOCK_SKEW_SECS {
        return Err(invalid("the ID token was issued in the future (iat)"));
    }
    if claims.contains_key("nbf") && time("nbf")? > now + CLOCK_SKEW_SECS {
        return Err(invalid("the ID token is not valid yet (nbf)"));
    }
    if text("nonce") != Some(expected.nonce) {
        return Err(invalid("the ID token nonce does not match"));
    }
    if text("sub").is_none_or(str::is_empty) {
        return Err(invalid("the ID token has no sub"));
    }
    Ok(())
}

/// 지금 유닉스 초
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 영역마다 받은 값(받은 시각)과 최근 실패, 같은 영역의 받기를 하나로 줄이는 잠금
struct Cached<T> {
    entries: Mutex<HashMap<RealmKey, Entry<T>>>,
}

struct Entry<T> {
    value: Option<(Instant, T)>,
    failure: Option<(Instant, OidcError)>,
    fetching: Arc<tokio::sync::Mutex<()>>,
}

impl<T> Default for Cached<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
        }
    }
}

impl<T: Clone> Cached<T> {
    /// 키의 항목. 같은 영역의 더 높은 revision 항목이 있으면 None이다(늦게 끝난 옛 revision 받기가 새
    /// 항목을 지우지 않게). 낮은 revision 항목은 지운다.
    fn entry(map: &mut HashMap<RealmKey, Entry<T>>, key: RealmKey) -> Option<&mut Entry<T>> {
        if map.keys().any(|k| k.0 == key.0 && k.1 > key.1) {
            return None;
        }
        map.retain(|k, _| k.0 != key.0 || k.1 == key.1);
        Some(map.entry(key).or_insert_with(|| Entry {
            value: None,
            failure: None,
            fetching: Arc::default(),
        }))
    }

    /// 받은 값을 `usable`이 쓸 수 있다고 하면 그것, 최근(`FAILURE_TTL` 안)에 실패했으면 그 오류
    fn ready(
        &self,
        key: RealmKey,
        usable: &impl Fn(Instant, &T) -> bool,
    ) -> Option<Result<T, OidcError>> {
        let map = self.entries.lock().unwrap();
        let entry = map.get(&key)?;
        if let Some((at, value)) = &entry.value
            && usable(*at, value)
        {
            return Some(Ok(value.clone()));
        }
        entry
            .failure
            .as_ref()
            .filter(|(at, _)| at.elapsed() < FAILURE_TTL)
            .map(|(_, e)| Err(e.clone()))
    }

    /// 쓸 수 있는 값이나 최근 실패가 없으면 받는다. 같은 영역의 받기는 한 번에 하나만 하고, 기다린
    /// 요청은 그 결과를 쓴다.
    async fn get_or_fetch<F>(
        &self,
        key: RealmKey,
        usable: impl Fn(Instant, &T) -> bool,
        fetch: impl FnOnce() -> F,
    ) -> Result<T, OidcError>
    where
        F: Future<Output = Result<T, OidcError>>,
    {
        if let Some(ready) = self.ready(key, &usable) {
            return ready;
        }
        let lock = Self::entry(&mut self.entries.lock().unwrap(), key)
            .map_or_else(Arc::default, |e| e.fetching.clone());
        let _fetching = lock.lock().await;
        // 기다리는 동안 다른 요청이 받았을 수 있다.
        if let Some(ready) = self.ready(key, &usable) {
            return ready;
        }
        let result = fetch().await;
        if let Some(entry) = Self::entry(&mut self.entries.lock().unwrap(), key) {
            match &result {
                Ok(value) => {
                    entry.value = Some((Instant::now(), value.clone()));
                    entry.failure = None;
                }
                Err(e) => entry.failure = Some((Instant::now(), e.clone())),
            }
        }
        result
    }
}

/// 노드 메모리의 discovery·JWKS 캐시. 키는 (영역 id, revision)이고 영역마다 가장 높은 revision만 둔다.
/// 받기 실패는 `FAILURE_TTL` 동안 기억해 같은 오류를 돌려준다.
pub struct OidcCache {
    ttl: Duration,
    min_refetch: Duration,
    discovery: Cached<Arc<Discovery>>,
    jwks: Cached<Arc<Vec<Jwk>>>,
}

impl Default for OidcCache {
    fn default() -> Self {
        Self::with_times(CACHE_TTL, MIN_REFETCH)
    }
}

impl OidcCache {
    /// 캐시 시간(`ttl`)과 모르는 `kid`로 다시 받는 가장 짧은 간격(`min_refetch`)을 정한다(시험용).
    pub fn with_times(ttl: Duration, min_refetch: Duration) -> Self {
        Self {
            ttl,
            min_refetch,
            discovery: Cached::default(),
            jwks: Cached::default(),
        }
    }

    /// 영역의 discovery 문서. 캐시에 없거나 오래됐으면 받는다.
    pub async fn discovery(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
    ) -> Result<Arc<Discovery>, OidcError> {
        self.discovery
            .get_or_fetch(
                key,
                |at, _| at.elapsed() < self.ttl,
                || async { Ok(Arc::new(fetch_discovery(&client(realm)?, realm).await?)) },
            )
            .await
    }

    /// 영역의 서명 키. 캐시에 없거나 오래됐으면 받는다. `missing_kid`가 있고 캐시에 그 키가 없으면,
    /// 마지막으로 받은 지 `min_refetch`가 지났을 때만 다시 받는다.
    pub async fn keys(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
        jwks_uri: &str,
        kid: Option<&str>,
    ) -> Result<Arc<Vec<Jwk>>, OidcError> {
        let usable = |at: Instant, keys: &Arc<Vec<Jwk>>| {
            let known = kid.is_none_or(|kid| keys.iter().any(|k| k.kid.as_deref() == Some(kid)));
            at.elapsed() < self.ttl && (known || at.elapsed() < self.min_refetch)
        };
        self.jwks
            .get_or_fetch(key, usable, || async {
                Ok(Arc::new(fetch_jwks(&client(realm)?, jwks_uri).await?))
            })
            .await
    }

    /// 영역의 ID 토큰을 확인한다. 모르는 `kid`면 JWKS를 한 번 다시 받는다([`OidcCache::keys`]).
    pub async fn verify_id_token(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
        discovery: &Discovery,
        token: &str,
        nonce: &str,
    ) -> Result<Map<String, Value>, OidcError> {
        let kid = token_kid(token);
        let keys = self
            .keys(key, realm, &discovery.jwks_uri, kid.as_deref())
            .await?;
        let expected = Expected {
            issuer: &realm.issuer,
            client_id: &realm.client_id,
            nonce,
        };
        verify_id_token(token, &keys, &expected, unix_now())
    }
}

/// 영역의 IdP를 시험한다: discovery 문서(issuer 같음·끝점 https)와 JWKS(RS256·ES256 키 수). 캐시를
/// 쓰지 않고 새로 받는다. 실패한 단계에서 멈춘다.
pub async fn test(realm: &OidcRealm) -> RealmTestResult {
    let mut steps = Vec::new();
    let mut step = |name: &str, result: Result<String, OidcError>| {
        let ok = result.is_ok();
        steps.push(RealmTestStep {
            name: name.into(),
            ok,
            message: result.unwrap_or_else(|e| e.to_string()),
        });
        ok
    };
    let discovered = match client(realm) {
        Ok(client) => fetch_discovery(&client, realm)
            .await
            .map(|doc| (client, doc)),
        Err(e) => Err(e),
    };
    let found = discovered.as_ref().map(|(_, doc)| {
        format!(
            "issuer {}, authorization endpoint {}, token endpoint {}",
            doc.issuer, doc.authorization_endpoint, doc.token_endpoint
        )
    });
    if step(STEP_DISCOVERY, found.map_err(Clone::clone))
        && let Ok((client, doc)) = &discovered
    {
        let keys = fetch_jwks(client, &doc.jwks_uri).await.and_then(|keys| {
            let rsa = keys.iter().filter(|k| k.alg() == "RS256").count();
            let ec = keys.len() - rsa;
            if keys.is_empty() {
                Err(invalid(format!(
                    "{} has no RS256 or ES256 signing keys",
                    doc.jwks_uri
                )))
            } else {
                Ok(format!("{rsa} RS256 and {ec} ES256 signing keys"))
            }
        });
        step(STEP_JWKS, keys);
    }
    RealmTestResult {
        steps,
        dn: None,
        groups: Vec::new(),
        role: None,
    }
}

/// 무작위 32바이트(base64url). `state`·`nonce`·PKCE 검증자에 쓴다.
pub fn random_value() -> String {
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("system random");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE S256 `code_challenge` = base64url(SHA-256(verifier))
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}

/// 상태 쿠키의 HMAC 키. 클러스터 CA 개인키(PEM)에서 HKDF-SHA256(salt 없음, 정보 `opl-oidc-state`)으로
/// 만든 32바이트라 모든 노드가 같은 키를 얻는다.
pub fn state_key(ca_key_pem: &str) -> hmac::Key {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(ca_key_pem.as_bytes());
    let okm = prk
        .expand(&[STATE_KEY_INFO], hmac::HMAC_SHA256)
        .expect("32 bytes is a valid HKDF length");
    hmac::Key::from(okm)
}

/// 로그인을 시작할 때 만들어 쿠키에 담는 값
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginState {
    pub realm: String,
    pub state: String,
    pub nonce: String,
    /// PKCE 검증자
    pub verifier: String,
    /// `start`가 IdP에 보낸 리디렉트 URI. 콜백은 이것으로 코드를 바꾼다(콜백의 Host를 보지 않는다).
    pub redirect_uri: String,
    /// 만든 시각(유닉스 초)
    pub created: i64,
}

// 검증자가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for LoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginState")
            .field("realm", &self.realm)
            .field("created", &self.created)
            .finish_non_exhaustive()
    }
}

/// 상수 시간 비교
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl LoginState {
    /// 영역의 새 로그인 상태(무작위 `state`·`nonce`·검증자)
    pub fn new(realm: &str, redirect_uri: &str, now: i64) -> Self {
        Self {
            realm: realm.into(),
            state: random_value(),
            nonce: random_value(),
            verifier: random_value(),
            redirect_uri: redirect_uri.into(),
            created: now,
        }
    }

    /// 쿠키 값: base64url(JSON) `.` base64url(HMAC-SHA256)
    pub fn seal(&self, key: &hmac::Key) -> String {
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("serializable"));
        let tag = hmac::sign(key, body.as_bytes());
        format!("{body}.{}", URL_SAFE_NO_PAD.encode(tag))
    }

    /// 쿠키 값을 확인한다: 서명, 만든 지 10분 안, 쿼리의 `state`와 같음.
    pub fn open(
        cookie: &str,
        key: &hmac::Key,
        query_state: &str,
        now: i64,
    ) -> Result<Self, OidcError> {
        let (body, tag) = cookie
            .split_once('.')
            .ok_or_else(|| invalid("the login state cookie is malformed"))?;
        let tag = URL_SAFE_NO_PAD
            .decode(tag)
            .map_err(|_| invalid("the login state cookie is malformed"))?;
        hmac::verify(key, body.as_bytes(), &tag)
            .map_err(|_| invalid("the login state cookie signature is not valid"))?;
        let state: LoginState = URL_SAFE_NO_PAD
            .decode(body)
            .ok()
            .and_then(|json| serde_json::from_slice(&json).ok())
            .ok_or_else(|| invalid("the login state cookie is malformed"))?;
        if now - state.created > STATE_TTL_SECS as i64 || state.created > now + CLOCK_SKEW_SECS {
            return Err(invalid("the login state cookie has expired"));
        }
        if !same(state.state.as_bytes(), query_state.as_bytes()) {
            return Err(invalid("the state does not match the login state cookie"));
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, RsaKeyPair};
    use serde_json::json;

    const RSA_PK8: &[u8] = include_bytes!("../tests/fixtures/oidc-rsa.pk8");
    const ISSUER: &str = "https://idp.example.com/";
    const NOW: i64 = 1_800_000_000;

    fn b64(bytes: impl AsRef<[u8]>) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn rsa() -> RsaKeyPair {
        RsaKeyPair::from_pkcs8(RSA_PK8).unwrap()
    }

    fn rsa_jwk(kid: Option<&str>) -> Jwk {
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = rsa().public().into();
        Jwk {
            kid: kid.map(str::to_string),
            key: JwkKey::Rsa {
                n: public.n,
                e: public.e,
            },
        }
    }

    fn ec() -> EcdsaKeyPair {
        let rng = SystemRandom::new();
        let pk8 = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .unwrap();
        EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pk8.as_ref(),
            &rng,
        )
        .unwrap()
    }

    fn ec_jwk(key: &EcdsaKeyPair, kid: Option<&str>) -> Jwk {
        Jwk {
            kid: kid.map(str::to_string),
            key: JwkKey::Ec {
                point: key.public_key().as_ref().to_vec(),
            },
        }
    }

    fn claims() -> Value {
        json!({
            "iss": ISSUER,
            "sub": "u-1",
            "aud": "oplite",
            "exp": NOW + 60,
            "iat": NOW,
            "nonce": "n-1",
            "preferred_username": "Alice",
        })
    }

    fn input(header: &Value, claims: &Value) -> String {
        format!(
            "{}.{}",
            b64(serde_json::to_vec(header).unwrap()),
            b64(serde_json::to_vec(claims).unwrap())
        )
    }

    fn sign_rs256(header: &Value, claims: &Value) -> String {
        let input = input(header, claims);
        let key = rsa();
        let mut sig = vec![0; key.public().modulus_len()];
        key.sign(
            &signature::RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{input}.{}", b64(sig))
    }

    fn sign_es256(key: &EcdsaKeyPair, header: &Value, claims: &Value) -> String {
        let input = input(header, claims);
        let sig = key.sign(&SystemRandom::new(), input.as_bytes()).unwrap();
        format!("{input}.{}", b64(sig))
    }

    fn expected() -> Expected<'static> {
        Expected {
            issuer: ISSUER,
            client_id: "oplite",
            nonce: "n-1",
        }
    }

    fn verify(token: &str, keys: &[Jwk]) -> Result<Map<String, Value>, OidcError> {
        verify_id_token(token, keys, &expected(), NOW)
    }

    fn err(message: &str) -> Result<Map<String, Value>, OidcError> {
        Err(OidcError::Invalid(message.into()))
    }

    #[test]
    fn rs256_and_es256_tokens_are_verified() {
        let token = sign_rs256(&json!({ "alg": "RS256", "kid": "r1" }), &claims());
        let got = verify(&token, &[rsa_jwk(Some("r1"))]).unwrap();
        assert_eq!(got["preferred_username"], "Alice");
        assert_eq!(token_kid(&token).as_deref(), Some("r1"));

        let key = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256", "kid": "e1" }), &claims());
        let keys = [rsa_jwk(Some("r1")), ec_jwk(&key, Some("e1"))];
        assert_eq!(verify(&token, &keys).unwrap()["sub"], "u-1");

        // kid가 없으면 alg가 맞는 키를 모두 시도한다.
        let other = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256" }), &claims());
        let keys = [ec_jwk(&other, Some("x")), ec_jwk(&key, Some("e1"))];
        assert!(verify(&token, &keys).is_ok());
        assert_eq!(token_kid(&token), None);
    }

    #[test]
    fn other_algorithms_are_rejected() {
        let payload = b64(serde_json::to_vec(&claims()).unwrap());
        // 서명 없는 토큰
        let none = format!("{}.{payload}.", b64(br#"{"alg":"none"}"#));
        assert_eq!(
            verify(&none, &[rsa_jwk(None)]),
            err("the ID token is signed with none, not RS256 or ES256")
        );
        // 공개 키를 HMAC 비밀로 쓰게 하는 공격
        let header = b64(br#"{"alg":"HS256"}"#);
        let input = format!("{header}.{payload}");
        let JwkKey::Rsa { n, .. } = rsa_jwk(None).key else {
            unreachable!()
        };
        let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &n), input.as_bytes());
        let hs256 = format!("{input}.{}", b64(tag));
        assert_eq!(
            verify(&hs256, &[rsa_jwk(None)]),
            err("the ID token is signed with HS256, not RS256 or ES256")
        );
        // 점 둘이 아닌 토큰(JWE 등)
        assert_eq!(
            verify("a.b.c.d.e", &[rsa_jwk(None)]),
            err("the ID token is not a signed JWT")
        );
    }

    #[test]
    fn signatures_and_keys_must_match() {
        let token = sign_rs256(&json!({ "alg": "RS256", "kid": "r1" }), &claims());
        // 클레임을 바꾼 토큰
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        let mut changed = claims();
        changed["preferred_username"] = json!("root");
        parts[1] = b64(serde_json::to_vec(&changed).unwrap());
        assert_eq!(
            verify(&parts.join("."), &[rsa_jwk(Some("r1"))]),
            err("the ID token signature is not valid")
        );
        // 서명 바이트를 바꾼 토큰
        let mut sig = URL_SAFE_NO_PAD
            .decode(token.rsplit('.').next().unwrap())
            .unwrap();
        sig[10] ^= 1;
        let (input, _) = token.rsplit_once('.').unwrap();
        assert_eq!(
            verify(&format!("{input}.{}", b64(sig)), &[rsa_jwk(Some("r1"))]),
            err("the ID token signature is not valid")
        );
        // 다른 키로 서명한 ES256
        let token = sign_es256(&ec(), &json!({ "alg": "ES256", "kid": "e1" }), &claims());
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e1"))]),
            err("the ID token signature is not valid")
        );
        // 모르는 kid, alg가 맞는 키 없음
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e2"))]),
            err("no ES256 key with kid e1 in the JWKS")
        );
        let token = sign_es256(&ec(), &json!({ "alg": "ES256" }), &claims());
        assert_eq!(
            verify(&token, &[rsa_jwk(None)]),
            err("no ES256 key in the JWKS")
        );
    }

    #[test]
    fn claims_are_checked() {
        let keys = [rsa_jwk(None)];
        let check = |f: &dyn Fn(&mut Value)| {
            let mut c = claims();
            f(&mut c);
            verify(&sign_rs256(&json!({ "alg": "RS256" }), &c), &keys).map(|_| ())
        };
        let fail = |m: &str| Err(OidcError::Invalid(m.into()));
        assert_eq!(check(&|_| {}), Ok(()));
        // iss는 글자 그대로(끝 / 포함)
        assert_eq!(
            check(&|c| c["iss"] = json!("https://idp.example.com")),
            fail(
                "the ID token issuer is \"https://idp.example.com\", not https://idp.example.com/"
            )
        );
        let aud = "the ID token is not for this client (aud)";
        assert_eq!(check(&|c| c["aud"] = json!("other")), fail(aud));
        assert_eq!(check(&|c| c["aud"] = json!(["a", "b"])), fail(aud));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("aud");
            }),
            fail(aud)
        );
        assert_eq!(check(&|c| c["aud"] = json!(["oplite"])), Ok(()));
        // 대상이 여럿이면 azp가 이 클라이언트여야 한다.
        let azp = "the ID token was not issued to this client (azp)";
        assert_eq!(check(&|c| c["aud"] = json!(["oplite", "api"])), fail(azp));
        assert_eq!(
            check(&|c| {
                c["aud"] = json!(["oplite", "api"]);
                c["azp"] = json!("api");
            }),
            fail(azp)
        );
        assert_eq!(
            check(&|c| {
                c["aud"] = json!(["oplite", "api"]);
                c["azp"] = json!("oplite");
            }),
            Ok(())
        );
        // exp·iat는 5분 오차
        let expired = "the ID token has expired";
        assert_eq!(check(&|c| c["exp"] = json!(NOW - 300)), fail(expired));
        assert_eq!(check(&|c| c["exp"] = json!(NOW - 299)), Ok(()));
        assert_eq!(
            check(&|c| c["exp"] = json!("soon")),
            fail("the ID token has no exp")
        );
        let future = "the ID token was issued in the future (iat)";
        assert_eq!(check(&|c| c["iat"] = json!(NOW + 301)), fail(future));
        assert_eq!(check(&|c| c["iat"] = json!(NOW + 300)), Ok(()));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("iat");
            }),
            fail("the ID token has no iat")
        );
        let nonce = "the ID token nonce does not match";
        assert_eq!(check(&|c| c["nonce"] = json!("n-2")), fail(nonce));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("nonce");
            }),
            fail(nonce)
        );
        let sub = "the ID token has no sub";
        assert_eq!(check(&|c| c["sub"] = json!("")), fail(sub));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("sub");
            }),
            fail(sub)
        );
    }

    #[test]
    fn crit_headers_and_future_nbf_are_rejected() {
        let keys = [rsa_jwk(None)];
        let token = sign_rs256(&json!({ "alg": "RS256", "crit": ["exp"] }), &claims());
        assert_eq!(
            verify(&token, &keys),
            err("the ID token header has crit extensions")
        );
        let with_nbf = |nbf: Value| {
            let mut c = claims();
            c["nbf"] = nbf;
            verify(&sign_rs256(&json!({ "alg": "RS256" }), &c), &keys).map(|_| ())
        };
        assert_eq!(with_nbf(json!(NOW + 300)), Ok(()));
        assert_eq!(
            with_nbf(json!(NOW + 301)),
            Err(invalid("the ID token is not valid yet (nbf)"))
        );
        assert_eq!(
            with_nbf(json!("later")),
            Err(invalid("the ID token has no nbf"))
        );
    }

    #[test]
    fn an_unknown_kid_falls_back_to_keys_without_a_kid() {
        let key = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256", "kid": "e9" }), &claims());
        // JWKS에 e9가 없으면 kid 없는 ES256 키를 시도한다.
        assert!(verify(&token, &[rsa_jwk(None), ec_jwk(&key, None)]).is_ok());
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), None)]),
            err("the ID token signature is not valid")
        );
        // kid가 있는 키로는 바꾸지 않는다.
        assert_eq!(
            verify(&token, &[ec_jwk(&key, Some("e1"))]),
            err("no ES256 key with kid e9 in the JWKS")
        );
        // JWKS에 e9가 있으면 그 키만 쓴다.
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e9")), ec_jwk(&key, None)]),
            err("the ID token signature is not valid")
        );
    }

    #[test]
    fn jwks_keeps_only_rs256_and_es256_signing_keys() {
        let key = ec();
        let point = key.public_key().as_ref();
        let (x, y) = (b64(&point[1..33]), b64(&point[33..]));
        let JwkKey::Rsa { n, e } = rsa_jwk(None).key else {
            unreachable!()
        };
        let (n, e) = (b64(n), b64(e));
        let doc = json!({ "keys": [
            { "kty": "RSA", "kid": "r1", "n": n, "e": e },
            { "kty": "RSA", "kid": "r2", "alg": "RS256", "use": "sig", "n": n, "e": e },
            { "kty": "EC", "kid": "e1", "crv": "P-256", "x": x, "y": y },
            { "kty": "EC", "kid": "e2", "crv": "P-256", "alg": "ES256", "x": x, "y": y },
            // 쓰지 않는 키
            { "kty": "RSA", "kid": "enc", "use": "enc", "n": n, "e": e },
            { "kty": "RSA", "kid": "ps", "alg": "PS256", "n": n, "e": e },
            { "kty": "EC", "kid": "p384", "crv": "P-384", "x": x, "y": y },
            { "kty": "EC", "kid": "short", "crv": "P-256", "x": b64([1u8; 31]), "y": y },
            { "kty": "oct", "kid": "hs", "k": "c2VjcmV0" },
            { "kty": "RSA", "kid": "broken", "n": "!!", "e": e },
        ]});
        let keys = parse_jwks(&doc);
        let kids: Vec<_> = keys.iter().map(|k| k.kid.as_deref().unwrap()).collect();
        assert_eq!(kids, ["r1", "r2", "e1", "e2"]);
        // EC 점은 비압축 형식 04 || x || y
        assert_eq!(keys[2], ec_jwk(&key, Some("e1")));
        assert_eq!(keys[0], rsa_jwk(Some("r1")));
        assert!(parse_jwks(&json!({})).is_empty());
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        // RFC 7636 부록 B
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let a = random_value();
        assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), 32);
        assert_ne!(a, random_value());
    }

    #[test]
    fn client_credentials_are_form_encoded() {
        assert_eq!(form_encode("oplite"), "oplite");
        assert_eq!(form_encode("a b+c:d/é*-._~"), "a+b%2Bc%3Ad%2F%C3%A9*-._%7E");
    }

    #[test]
    fn discovery_url_joins_one_slash() {
        assert_eq!(
            discovery_url("https://idp.example.com/realms/corp/"),
            "https://idp.example.com/realms/corp/.well-known/openid-configuration"
        );
        assert_eq!(
            discovery_url("https://idp.example.com"),
            "https://idp.example.com/.well-known/openid-configuration"
        );
    }

    #[test]
    fn state_cookies_are_signed_and_expire() {
        let key = state_key("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        let state = LoginState::new(
            "okta",
            "https://opl.example.com/api/v1/auth/oidc/callback",
            NOW,
        );
        assert_ne!(state.state, state.nonce);
        let cookie = state.seal(&key);
        assert_eq!(
            LoginState::open(&cookie, &key, &state.state, NOW),
            Ok(state.clone())
        );
        assert_eq!(
            LoginState::open(&cookie, &key, &state.state, NOW + 600),
            Ok(state.clone())
        );
        let open = |cookie: &str, key: &hmac::Key, query: &str, now: i64| {
            LoginState::open(cookie, key, query, now).map(|_| ())
        };
        let fail = |m: &str| Err(OidcError::Invalid(m.into()));
        assert_eq!(
            open(&cookie, &key, &state.state, NOW + 601),
            fail("the login state cookie has expired")
        );
        assert_eq!(
            open(&cookie, &key, "other", NOW),
            fail("the state does not match the login state cookie")
        );
        assert_eq!(
            open(&cookie, &key, &state.state[1..], NOW),
            fail("the state does not match the login state cookie")
        );
        // 다른 클러스터의 키
        let other = state_key("other");
        assert_eq!(
            open(&cookie, &other, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        // 바뀐 바이트(본문을 바꾸면 서명이 맞지 않는다)
        let mut forged = LoginState {
            created: NOW + 3600,
            ..state.clone()
        }
        .seal(&key);
        let (_, tag) = cookie.split_once('.').unwrap();
        let (body, _) = forged.split_once('.').unwrap();
        forged = format!("{body}.{tag}");
        assert_eq!(
            open(&forged, &key, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        let mut bytes = cookie.clone().into_bytes();
        bytes[3] = if bytes[3] == b'A' { b'B' } else { b'A' };
        assert_eq!(
            open(&String::from_utf8(bytes).unwrap(), &key, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        for bad in ["", "nodot", "a.!!"] {
            assert_eq!(
                open(bad, &key, &state.state, NOW),
                fail("the login state cookie is malformed")
            );
        }
        // 같은 CA 키에서는 같은 쿠키 키(다른 노드도 확인할 수 있다)
        let again = state_key("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        assert!(LoginState::open(&cookie, &again, &state.state, NOW).is_ok());
        // 로그에 검증자·state를 내지 않는다.
        let shown = format!("{state:?}");
        assert!(!shown.contains(&state.verifier), "{shown}");
        assert!(!shown.contains(&state.state), "{shown}");
    }
}
```

`crates/opl-api/src/oidc_login.rs`를 만든다:

```rust
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
```

`crates/opl-api/src/state.rs`에서

```rust
use crate::limit::AttemptLimiter;
use crate::vm_imports::ImportCredentials;
```

을 다음으로 바꾼다:

```rust
use crate::limit::AttemptLimiter;
use crate::oidc::OidcCache;
use crate::vm_imports::ImportCredentials;
```

`crates/opl-api/src/state.rs`에서

```rust
    pub audit: Arc<AuditLog>,
}
```

을 다음으로 바꾼다:

```rust
    pub audit: Arc<AuditLog>,
    /// OIDC 영역의 discovery·JWKS(노드 메모리)
    pub(crate) oidc: OidcCache,
}
```

`crates/opl-api/src/state.rs`에서

```rust
            audit: Arc::new(AuditLog::disabled()),
        }
```

을 다음으로 바꾼다:

```rust
            audit: Arc::new(AuditLog::disabled()),
            oidc: OidcCache::default(),
        }
```

`crates/opl-api/src/users.rs` 전체:

```rust
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
/// 모르는 영역과 OIDC 영역(IdP에서 로그인한다)은 400, 맞는 역할이 없으면 403 `no_role`, 디렉터리에
/// 닿지 못하면 503 `realm_unavailable`.
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
    let cookie = new_session(&state, &user)?;
    let mut response = Json(UserView::from(user)).into_response();
    response.headers_mut().insert(SET_COOKIE, cookie);
    Ok(response)
}

/// 사용자의 세션을 만들고 그 세션 쿠키(Set-Cookie 값)를 돌려준다.
pub(crate) fn new_session(state: &AppState, user: &User) -> Result<HeaderValue, ApiError> {
    let session = state.sessions.create(user.id, user.spec.session_epoch);
    let mut cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).map_err(ApiError::internal)
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
    let user = upsert_external(state, &name, realm, role, None).await?;
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
) -> Result<User, ApiError> {
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
                    && (subject.is_none() || user.spec.subject.as_deref() == subject) =>
            {
                return Ok(user);
            }
            Some(mut user) => {
                if user.spec.role != role {
                    user.spec.role = role;
                    user.spec.session_epoch += 1;
                }
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
            subject: None,
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
```

`crates/opl-api/tests/common/idp.rs` 전체:

```rust
//! 시험용 가짜 OIDC 제공자(IdP). 시험 CA가 서명한 `localhost`·`127.0.0.1` 인증서의 HTTPS로
//! discovery(`/.well-known/openid-configuration`), `/authorize`(쿼리를 기록하고 곧바로
//! `redirect_uri?code=..&state=..`로 302), `/token`(코드·PKCE S256·클라이언트 인증을 확인하고
//! ID 토큰·액세스 토큰), `/jwks`, `/userinfo`를 낸다. 서명 키는 RS256(저장소 견본
//! `tests/fixtures/oidc-rsa.pk8`, kid `rsa-1`)과 ES256(시험마다 새로, kid `ec-1`)이고 처음엔 RS256으로
//! 서명한다. 시험이 discovery 문서, 클레임, 서명 키·kid·알고리즘, 공개할 키, userinfo, 실패 응답, 인가 응답의
//! `iss`(RFC 9207), 큰 JWKS를 정한다.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use opl_peer::pki::{ClusterCa, NodeTls, first_cert, node_key_and_csr};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair, RsaKeyPair};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 저장소의 RS256 시험 키(openssl로 한 번 만든 2048비트 PKCS#8 DER — ring은 RSA 키를 만들지 못한다)
const RSA_PK8: &[u8] = include_bytes!("../fixtures/oidc-rsa.pk8");

pub const CLIENT_ID: &str = "oplite";
pub const CLIENT_SECRET: &str = "client-secret";
/// 기본 ID 토큰의 `sub`
pub const SUBJECT: &str = "alice-sub";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alg {
    Rs256,
    Es256,
}

#[derive(Clone)]
enum Signer {
    Rsa(Arc<RsaKeyPair>),
    Ec(Arc<EcdsaKeyPair>),
}

#[derive(Clone)]
struct Key {
    kid: String,
    signer: Signer,
    /// JWKS에 내보이는지
    published: bool,
}

impl Key {
    fn new(kid: &str, alg: Alg) -> Key {
        let signer = match alg {
            Alg::Rs256 => Signer::Rsa(Arc::new(RsaKeyPair::from_pkcs8(RSA_PK8).unwrap())),
            Alg::Es256 => {
                let rng = SystemRandom::new();
                let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
                let pk8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
                Signer::Ec(Arc::new(
                    EcdsaKeyPair::from_pkcs8(alg, pk8.as_ref(), &rng).unwrap(),
                ))
            }
        };
        Key {
            kid: kid.into(),
            signer,
            published: true,
        }
    }

    fn alg(&self) -> &'static str {
        match self.signer {
            Signer::Rsa(_) => "RS256",
            Signer::Ec(_) => "ES256",
        }
    }

    fn jwk(&self) -> Value {
        match &self.signer {
            Signer::Rsa(k) => {
                let public: signature::RsaPublicKeyComponents<Vec<u8>> = k.public().into();
                json!({ "kty": "RSA", "kid": self.kid, "use": "sig", "alg": "RS256",
                        "n": b64(public.n), "e": b64(public.e) })
            }
            Signer::Ec(k) => {
                let point = k.public_key().as_ref();
                json!({ "kty": "EC", "kid": self.kid, "crv": "P-256",
                        "x": b64(&point[1..33]), "y": b64(&point[33..]) })
            }
        }
    }

    fn sign(&self, input: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        match &self.signer {
            Signer::Rsa(k) => {
                let mut sig = vec![0; k.public().modulus_len()];
                k.sign(&signature::RSA_PKCS1_SHA256, &rng, input, &mut sig)
                    .unwrap();
                sig
            }
            Signer::Ec(k) => k.sign(&rng, input).unwrap().as_ref().to_vec(),
        }
    }
}

fn b64(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// `/authorize`가 낸 코드
#[derive(Clone)]
struct Grant {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    nonce: Option<String>,
}

struct Inner {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    discovery: Value,
    keys: Vec<Key>,
    /// 서명할 키의 kid
    signing: String,
    /// ID 토큰 헤더에 kid를 넣는지
    header_kid: bool,
    /// 기본 클레임에 덮어쓸 값(null이면 뺀다)
    claims: Map<String, Value>,
    userinfo: Value,
    /// `/authorize`가 코드 대신 돌려보낼 오류
    authorize_error: Option<String>,
    /// `/token`의 실패 응답
    token_failure: Option<(u16, Value)>,
    /// `/userinfo`의 실패 상태
    userinfo_failure: Option<u16>,
    /// `/authorize`가 돌려보낼 때 붙일 `iss`
    authorize_iss: Option<String>,
    /// JWKS 문서에 더할 바이트 수와 청크로 보낼지(본문 크기 제한 시험)
    jwks_padding: Option<(usize, bool)>,
    codes: HashMap<String, Grant>,
    access_tokens: Vec<String>,
    authorize_queries: Vec<HashMap<String, String>>,
    token_requests: Vec<(Option<String>, HashMap<String, String>)>,
    hits: HashMap<&'static str, usize>,
}

#[derive(Clone)]
struct Site(Arc<Mutex<Inner>>);

impl Site {
    fn hit(&self, path: &'static str) -> std::sync::MutexGuard<'_, Inner> {
        let mut inner = self.0.lock().unwrap();
        *inner.hits.entry(path).or_default() += 1;
        inner
    }
}

pub struct Idp {
    /// `https://127.0.0.1:<port>`
    pub issuer: String,
    /// 서버 인증서에 서명한 CA(PEM)
    pub ca_pem: String,
    /// 같은 CA(DER)
    pub ca_der: Vec<u8>,
    site: Site,
    server: JoinHandle<()>,
}

impl Idp {
    /// 기밀 클라이언트(`oplite`/`client-secret`)의 IdP
    pub async fn start() -> Idp {
        Idp::start_with(CLIENT_ID, Some(CLIENT_SECRET)).await
    }

    /// `client_secret`이 없으면 공개 클라이언트(PKCE만)
    pub async fn start_with(client_id: &str, client_secret: Option<&str>) -> Idp {
        let ca = ClusterCa::generate("idp").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let sans = ["localhost".to_string(), "127.0.0.1".to_string()];
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, uuid::Uuid::new_v4(), &sans).unwrap(),
            key_pem,
        };
        let config = opl_peer::tls::api_server_config(&tls).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("https://{}", tcp.local_addr().unwrap());
        let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
        let site = Site(Arc::new(Mutex::new(Inner {
            discovery: json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "jwks_uri": format!("{issuer}/jwks"),
                "userinfo_endpoint": format!("{issuer}/userinfo"),
                "response_types_supported": ["code"],
                "id_token_signing_alg_values_supported": ["RS256", "ES256"],
            }),
            issuer: issuer.clone(),
            client_id: client_id.into(),
            client_secret: client_secret.map(str::to_string),
            keys: vec![Key::new("rsa-1", Alg::Rs256), Key::new("ec-1", Alg::Es256)],
            signing: "rsa-1".into(),
            header_kid: true,
            claims: Map::new(),
            userinfo: json!({
                "sub": SUBJECT,
                "preferred_username": "Alice",
                "groups": ["opl-admins"],
            }),
            authorize_error: None,
            token_failure: None,
            userinfo_failure: None,
            authorize_iss: None,
            jwks_padding: None,
            codes: HashMap::new(),
            access_tokens: Vec::new(),
            authorize_queries: Vec::new(),
            token_requests: Vec::new(),
            hits: HashMap::new(),
        })));
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get(discovery),
            )
            .route("/authorize", axum::routing::get(authorize))
            .route("/token", axum::routing::post(token))
            .route("/jwks", axum::routing::get(jwks))
            .route("/userinfo", axum::routing::get(userinfo))
            .with_state(site.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Idp {
            issuer,
            ca_der: first_cert(&ca.cert_pem).unwrap().to_vec(),
            ca_pem: ca.cert_pem,
            site,
            server,
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.site.0.lock().unwrap()
    }

    /// 이 IdP를 쓰는 영역의 `backend`(시험 CA를 믿는다)
    pub fn backend(&self) -> Value {
        let inner = self.inner();
        let mut backend = json!({
            "kind": "oidc",
            "issuer": inner.issuer,
            "client_id": inner.client_id,
            "ca_pem": self.ca_pem,
            // macOS에서는 새 시험 CA로 하는 첫 TLS 연결의 인증서 검증이 15초쯤 걸리기도 한다.
            "timeout_secs": 30,
        });
        if let Some(secret) = &inner.client_secret {
            backend["client_secret"] = json!(secret);
        }
        backend
    }

    /// 경로(`discovery`, `authorize`, `token`, `jwks`, `userinfo`)가 불린 횟수
    pub fn hits(&self, path: &str) -> usize {
        self.inner().hits.get(path).copied().unwrap_or(0)
    }

    /// discovery 문서를 고친다.
    pub fn set_discovery(&self, f: impl FnOnce(&mut Value)) {
        f(&mut self.inner().discovery);
    }

    /// 서명 키를 더한다(`published`면 JWKS에도). 서명 키는 [`Idp::sign_with`]로 고른다.
    pub fn add_key(&self, kid: &str, alg: Alg, published: bool) {
        let mut key = Key::new(kid, alg);
        key.published = published;
        self.inner().keys.push(key);
    }

    /// JWKS에 키를 내보일지
    pub fn publish(&self, kid: &str, published: bool) {
        for key in self.inner().keys.iter_mut().filter(|k| k.kid == kid) {
            key.published = published;
        }
    }

    /// 앞으로 `kid`의 키로 서명한다.
    pub fn sign_with(&self, kid: &str) {
        self.inner().signing = kid.into();
    }

    /// ID 토큰 헤더에 kid를 넣을지(기본 넣음)
    pub fn set_header_kid(&self, on: bool) {
        self.inner().header_kid = on;
    }

    /// 앞으로 낼 ID 토큰의 클레임을 덮어쓴다(null이면 뺀다). 앞서 정한 것에 더한다.
    pub fn set_claims(&self, claims: Value) {
        let mut inner = self.inner();
        for (k, v) in claims.as_object().unwrap() {
            inner.claims.insert(k.clone(), v.clone());
        }
    }

    pub fn set_userinfo(&self, userinfo: Value) {
        self.inner().userinfo = userinfo;
    }

    /// `/authorize`가 코드 대신 `error`로 돌려보낸다(None이면 되돌림).
    pub fn set_authorize_error(&self, error: Option<&str>) {
        self.inner().authorize_error = error.map(str::to_string);
    }

    /// `/token`이 `status`와 `body`로 실패한다(None이면 되돌림).
    pub fn fail_token(&self, failure: Option<(u16, Value)>) {
        self.inner().token_failure = failure;
    }

    /// `/userinfo`가 `status`로 실패한다(None이면 되돌림).
    pub fn fail_userinfo(&self, status: Option<u16>) {
        self.inner().userinfo_failure = status;
    }

    /// `/authorize`가 돌려보내는 주소에 `iss`를 붙인다(None이면 붙이지 않음).
    pub fn set_authorize_iss(&self, iss: Option<&str>) {
        self.inner().authorize_iss = iss.map(str::to_string);
    }

    /// JWKS 문서에 `bytes`바이트를 더한다. `chunked`면 Content-Length 없이 청크로 보낸다.
    pub fn pad_jwks(&self, bytes: usize, chunked: bool) {
        self.inner().jwks_padding = Some((bytes, chunked));
    }

    /// `/authorize`가 받은 쿼리(받은 순서)
    pub fn authorize_queries(&self) -> Vec<HashMap<String, String>> {
        self.inner().authorize_queries.clone()
    }

    /// `/token`이 받은 (Authorization 헤더, 폼 본문)(받은 순서)
    pub fn token_requests(&self) -> Vec<(Option<String>, HashMap<String, String>)> {
        self.inner().token_requests.clone()
    }

    /// 기본 클레임(`iss`, `sub`, `aud`, `exp` 5분 뒤, `iat` 지금, `nonce`, `preferred_username` Alice,
    /// `groups` [opl-admins])에 [`Idp::set_claims`]를 덮어쓴 것
    pub fn claims(&self, nonce: Option<&str>) -> Value {
        default_claims(&self.inner(), nonce)
    }

    /// 지금 서명 키로 `claims`에 서명한 ID 토큰
    pub fn id_token(&self, claims: &Value) -> String {
        sign(&self.inner(), claims)
    }

    /// 브라우저처럼 `/authorize` 주소(opLite `start`의 Location)를 열어 돌려보내는 곳(콜백 주소)을 얻는다.
    pub async fn follow(&self, authorize_url: &str) -> String {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .tls_certs_only([reqwest::Certificate::from_pem(self.ca_pem.as_bytes()).unwrap()])
            .build()
            .unwrap();
        let resp = http.get(authorize_url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::FOUND, "{authorize_url}");
        resp.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string()
    }

    /// 헤더까지 정해 지금 서명 키로 서명한다(`alg` 거짓말 시험).
    pub fn sign_raw(&self, header: &Value, claims: &Value) -> String {
        let inner = self.inner();
        let key = inner.keys.iter().find(|k| k.kid == inner.signing).unwrap();
        signed(key, header, claims)
    }
}

impl Drop for Idp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn signed(key: &Key, header: &Value, claims: &Value) -> String {
    let input = format!(
        "{}.{}",
        b64(serde_json::to_vec(header).unwrap()),
        b64(serde_json::to_vec(claims).unwrap())
    );
    let sig = key.sign(input.as_bytes());
    format!("{input}.{}", b64(sig))
}

fn sign(inner: &Inner, claims: &Value) -> String {
    let key = inner.keys.iter().find(|k| k.kid == inner.signing).unwrap();
    let mut header = json!({ "alg": key.alg(), "typ": "JWT" });
    if inner.header_kid {
        header["kid"] = json!(key.kid);
    }
    signed(key, &header, claims)
}

async fn discovery(State(site): State<Site>) -> Response {
    axum::Json(site.hit("discovery").discovery.clone()).into_response()
}

async fn jwks(State(site): State<Site>) -> Response {
    let inner = site.hit("jwks");
    let keys: Vec<Value> = inner
        .keys
        .iter()
        .filter(|k| k.published)
        .map(Key::jwk)
        .collect();
    match inner.jwks_padding {
        None => axum::Json(json!({ "keys": keys })).into_response(),
        Some((bytes, chunked)) => {
            let body = json!({ "keys": keys, "padding": "x".repeat(bytes) }).to_string();
            if !chunked {
                return ([(header::CONTENT_TYPE, "application/json")], body).into_response();
            }
            let (a, b) = body.split_at(body.len() / 2);
            let chunks = [a.to_string(), b.to_string()].map(Ok::<_, std::io::Error>);
            let stream = futures_util::stream::iter(chunks);
            (
                [(header::CONTENT_TYPE, "application/json")],
                axum::body::Body::from_stream(stream),
            )
                .into_response()
        }
    }
}

async fn authorize(
    State(site): State<Site>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut inner = site.hit("authorize");
    inner.authorize_queries.push(query.clone());
    let redirect = query.get("redirect_uri").cloned().unwrap_or_default();
    let Ok(mut location) = reqwest::Url::parse(&redirect) else {
        return (StatusCode::BAD_REQUEST, "bad redirect_uri").into_response();
    };
    {
        let mut pairs = location.query_pairs_mut();
        if let Some(error) = &inner.authorize_error {
            pairs.append_pair("error", error);
            pairs.append_pair("error_description", "the user said no");
        } else {
            let code = b64(uuid::Uuid::new_v4().as_bytes());
            inner.codes.insert(
                code.clone(),
                Grant {
                    client_id: query.get("client_id").cloned().unwrap_or_default(),
                    redirect_uri: redirect.clone(),
                    challenge: query.get("code_challenge").cloned().unwrap_or_default(),
                    nonce: query.get("nonce").cloned(),
                },
            );
            pairs.append_pair("code", &code);
        }
        if let Some(state) = query.get("state") {
            pairs.append_pair("state", state);
        }
        if let Some(iss) = &inner.authorize_iss {
            pairs.append_pair("iss", iss);
        }
    }
    (
        StatusCode::FOUND,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}

/// application/x-www-form-urlencoded 값 하나를 푼다.
fn form_decode(value: &str) -> Option<String> {
    let uri: axum::http::Uri = format!("/?v={value}").parse().ok()?;
    let Query(mut query) = Query::<HashMap<String, String>>::try_from_uri(&uri).ok()?;
    query.remove("v")
}

fn token_error(status: StatusCode, error: &str) -> Response {
    (status, axum::Json(json!({ "error": error }))).into_response()
}

async fn token(
    State(site): State<Site>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> Response {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut inner = site.hit("token");
    inner
        .token_requests
        .push((authorization.clone(), form.clone()));
    if let Some((status, body)) = inner.token_failure.clone() {
        return (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response();
    }
    // 클라이언트 인증: 기밀 클라이언트는 client_secret_basic(RFC 6749 §2.3.1 — client_id와 비밀을 각각
    // form-urlencode한 뒤 `id:secret`), 공개 클라이언트는 폼의 client_id
    let client_ok = match &inner.client_secret {
        Some(secret) => authorization
            .as_deref()
            .and_then(|a| a.strip_prefix("Basic "))
            .and_then(|b| STANDARD.decode(b).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|pair| {
                let (id, s) = pair.split_once(':')?;
                Some((form_decode(id)?, form_decode(s)?))
            })
            .is_some_and(|(id, s)| id == inner.client_id && s == *secret),
        None => authorization.is_none() && form.get("client_id") == Some(&inner.client_id),
    };
    if !client_ok {
        return token_error(StatusCode::UNAUTHORIZED, "invalid_client");
    }
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return token_error(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }
    // 코드는 한 번만 쓴다.
    let Some(grant) = form.get("code").and_then(|c| inner.codes.remove(c)) else {
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    let challenge = b64(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ));
    if grant.client_id != inner.client_id
        || form.get("redirect_uri") != Some(&grant.redirect_uri)
        || challenge != grant.challenge
    {
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    let access_token = b64(uuid::Uuid::new_v4().as_bytes());
    inner.access_tokens.push(access_token.clone());
    let id_token = sign(&inner, &default_claims(&inner, grant.nonce.as_deref()));
    axum::Json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token,
    }))
    .into_response()
}

fn default_claims(inner: &Inner, nonce: Option<&str>) -> Value {
    let mut claims = json!({
        "iss": inner.issuer,
        "sub": SUBJECT,
        "aud": inner.client_id,
        "exp": now() + 300,
        "iat": now(),
        "preferred_username": "Alice",
        "groups": ["opl-admins"],
    });
    if let Some(nonce) = nonce {
        claims["nonce"] = json!(nonce);
    }
    let map = claims.as_object_mut().unwrap();
    for (k, v) in &inner.claims {
        if v.is_null() {
            map.remove(k);
        } else {
            map.insert(k.clone(), v.clone());
        }
    }
    claims
}

async fn userinfo(State(site): State<Site>, headers: HeaderMap) -> Response {
    let inner = site.hit("userinfo");
    if let Some(status) = inner.userinfo_failure {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|a| a.strip_prefix("Bearer "));
    if !token.is_some_and(|t| inner.access_tokens.iter().any(|a| a == t)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(inner.userinfo.clone()).into_response()
}
```

`crates/opl-api/tests/oidc.rs`에서

```rust
//! 모르는 kid 뒤 다시 받기, 10초 안에는 다시 받지 않음), 받기 실패 기억과 동시 받기 하나로, 가장 높은
//! revision, 캐시를 거친 ID 토큰 확인, 영역 시험 동작의 `discovery`·`jwks` 단계.
```

을 다음으로 바꾼다:

```rust
//! 모르는 kid 뒤 다시 받기, 10초 안에는 다시 받지 않음), 받기 실패 기억과 동시 받기 하나로, 가장 높은
//! revision, 응답 크기 제한, 캐시를 거친 ID 토큰 확인, 영역 시험 동작의 `discovery`·`jwks` 단계.
```

`crates/opl-api/tests/oidc.rs`에서

```rust

/// 받기 실패는 같은 (영역, revision)에서 15초 동안 기억해 다시 받지 않고 같은 오류를 돌려준다.
```

을 다음으로 바꾼다:

```rust

/// IdP 응답은 1 MiB까지만 읽는다: Content-Length가 크면 바로, 청크면 읽다가 넘을 때 멈춘다.
#[tokio::test]
async fn large_responses_are_refused_while_reading() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let jwks = format!("{}/jwks", idp.issuer);
    let too_large = invalid(format!("{jwks}: response is too large"));
    for chunked in [false, true] {
        idp.pad_jwks(2 << 20, chunked);
        let cache = OidcCache::default();
        assert_eq!(
            cache.keys(key(), &realm, &jwks, None).await,
            Err(too_large.clone()),
            "chunked: {chunked}"
        );
        // 1 MiB 안이면 읽는다.
        idp.pad_jwks(1 << 19, chunked);
        assert_eq!(
            cache.keys(key(), &realm, &jwks, None).await.unwrap().len(),
            2,
            "chunked: {chunked}"
        );
    }
}

/// 받기 실패는 같은 (영역, revision)에서 15초 동안 기억해 다시 받지 않고 같은 오류를 돌려준다.
```

`crates/opl-api/tests/oidc_login.rs`를 만든다:

```rust
//! OIDC 로그인 흐름(가짜 IdP): `start`의 리디렉트와 상태 쿠키, 콜백의 쿠키 확인(없음·state 다름·만료·
//! 변조), 상태에 담은 리디렉트 URI, IdP 오류, 인가 응답의 iss(RFC 9207), 토큰 끝점·userinfo 실패, ID
//! 토큰과 userinfo의 클레임, 그룹 → 역할, 이름 규칙(이메일 모양), sub 묶기, 공개 클라이언트,
//! client_secret_basic, 두 번째 로그인과 역할 바뀜, 감사 로그, 시도 제한을 쓰지 않음.

mod common;

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::idp::{CLIENT_ID, Idp, SUBJECT};
use common::{ADMIN_PASSWORD, Env, WAIT};
use opl_api::oidc::{LoginState, state_key, unix_now};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};

const CALLBACK: &str = "/api/v1/auth/oidc/callback";

/// opl-admins는 admin, opl-viewers는 viewer
fn realm_body(name: &str, idp: &Idp) -> Value {
    json!({
        "name": name,
        "display": "Okta",
        "backend": idp.backend(),
        "roles": {
            "groups": [
                { "group": "opl-admins", "role": "admin" },
                { "group": "opl-viewers", "role": "viewer" }
            ]
        }
    })
}

async fn add_realm(env: &Env, name: &str, idp: &Idp) {
    let (status, body) = env
        .admin()
        .await
        .post("/realms", realm_body(name, idp))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// 가짜 IdP와 그것을 쓰는 영역 `okta`
async fn setup_with(idp: Idp) -> (Env, Idp) {
    let env = Env::start().await;
    add_realm(&env, "okta", &idp).await;
    (env, idp)
}

async fn setup() -> (Env, Idp) {
    setup_with(Idp::start().await).await
}

/// 리다이렉트를 따르지 않는 브라우저의 응답 하나
struct Hop {
    status: StatusCode,
    location: String,
    /// Set-Cookie 줄 전체
    cookies: Vec<String>,
}

impl Hop {
    /// `name=`으로 시작하는 Set-Cookie 줄
    fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|c| c.starts_with(&format!("{name}=")))
            .map(String::as_str)
    }

    /// `name=value` 부분
    fn pair(&self, name: &str) -> Option<String> {
        self.cookie(name)
            .map(|c| c.split(';').next().unwrap().to_string())
    }
}

async fn open(url: &str, cookie: Option<&str>) -> Hop {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut req = http.get(url);
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    let resp = req.send().await.unwrap();
    Hop {
        status: resp.status(),
        location: resp
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default(),
        cookies: resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect(),
    }
}

async fn start(env: &Env, realm: &str) -> Hop {
    open(
        &format!("{}/api/v1/auth/oidc/{realm}/start", env.base),
        None,
    )
    .await
}

/// `start` → 가짜 `/authorize` → 콜백 주소와 상태 쿠키(`opl_oidc=…`)
async fn authorize(env: &Env, idp: &Idp) -> (String, String) {
    let started = start(env, "okta").await;
    assert_eq!(started.status, StatusCode::FOUND);
    let callback = idp.follow(&started.location).await;
    assert!(
        callback.starts_with(&format!("{}{CALLBACK}?", env.base)),
        "{callback}"
    );
    (callback, started.pair("opl_oidc").unwrap())
}

/// 처음부터 끝까지 로그인한다.
async fn sign_in(env: &Env, idp: &Idp) -> Hop {
    let (callback, cookie) = authorize(env, idp).await;
    open(&callback, Some(&cookie)).await
}

/// 로그인에 성공해 세션 쿠키(`opl_session=…`)를 얻는다.
async fn signed_in(env: &Env, idp: &Idp) -> String {
    let hop = sign_in(env, idp).await;
    assert_eq!(
        (hop.status, hop.location.as_str()),
        (StatusCode::FOUND, "/")
    );
    hop.pair("opl_session").unwrap()
}

/// 실패해 `/login?error=<code>`로 돌아가고 상태 쿠키를 지운다.
fn assert_failed(hop: &Hop, code: &str) {
    assert_eq!(hop.status, StatusCode::FOUND);
    assert_eq!(hop.location, format!("/login?error={code}"));
    assert!(hop.cookie("opl_session").is_none(), "{:?}", hop.cookies);
    assert_eq!(
        hop.cookie("opl_oidc"),
        Some("opl_oidc=; Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age=0")
    );
}

async fn whoami(env: &Env, cookie: &str) -> (StatusCode, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/auth/whoami", env.base))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

fn stored(env: &Env, name: &str) -> Option<Resource> {
    env.cluster.get_by_name::<UserSpec>(name).unwrap()
}

type Resource = opl_types::resource::Resource<UserSpec, opl_types::auth::UserStatus>;

fn query(url: &str) -> std::collections::HashMap<String, String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[tokio::test]
async fn a_login_goes_through_the_identity_provider() {
    let (env, idp) = setup().await;
    let started = start(&env, "okta").await;
    assert_eq!(started.status, StatusCode::FOUND);
    // IdP의 authorization endpoint로, PKCE S256과 state·nonce
    assert!(
        started
            .location
            .starts_with(&format!("{}/authorize?", idp.issuer)),
        "{}",
        started.location
    );
    let q = query(&started.location);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], CLIENT_ID);
    assert_eq!(q["redirect_uri"], format!("{}{CALLBACK}", env.base));
    assert_eq!(q["scope"], "openid profile email");
    assert_eq!(q["code_challenge_method"], "S256");
    for name in ["state", "nonce", "code_challenge"] {
        assert_eq!(q[name].len(), 43, "{name}: {}", q[name]);
    }
    assert_ne!(q["state"], q["nonce"]);
    // 상태 쿠키는 OIDC 경로에만, 10분
    let cookie = started.cookie("opl_oidc").unwrap();
    let (pair, attrs) = cookie.split_once(';').unwrap();
    assert_eq!(
        attrs,
        " Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age=600"
    );

    let callback = idp.follow(&started.location).await;
    let hop = open(&callback, Some(pair)).await;
    assert_eq!(
        (hop.status, hop.location.as_str()),
        (StatusCode::FOUND, "/")
    );
    assert_eq!(
        hop.cookie("opl_oidc"),
        Some("opl_oidc=; Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age=0")
    );
    let session = hop.cookie("opl_session").unwrap();
    assert!(
        session.ends_with("; Path=/; HttpOnly; SameSite=Strict; Max-Age=43200"),
        "{session}"
    );
    let (status, me) = whoami(&env, &hop.pair("opl_session").unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(
        (me["name"].as_str(), me["role"].as_str()),
        (Some("alice@okta"), Some("admin"))
    );
    let user = stored(&env, "alice@okta").unwrap();
    assert_eq!(user.spec.realm.as_deref(), Some("okta"));
    // IdP의 사용자(iss와 sub)에 묶인다.
    assert_eq!(user.spec.subject, Some(format!("{} {SUBJECT}", idp.issuer)));
    assert_eq!(user.spec.password_hash, "");
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 0));

    // 토큰 끝점: 코드와 검증자, client_secret_basic. 클레임이 ID 토큰에 있으므로 userinfo는 읽지 않는다.
    let requests = idp.token_requests();
    assert_eq!(requests.len(), 1);
    let (authorization, form) = &requests[0];
    assert_eq!(
        authorization.as_deref(),
        Some(format!("Basic {}", STANDARD.encode("oplite:client-secret")).as_str())
    );
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["redirect_uri"], format!("{}{CALLBACK}", env.base));
    assert_eq!(form["client_id"], CLIENT_ID);
    assert_eq!(form["code_verifier"].len(), 43);
    assert_eq!(idp.hits("userinfo"), 0);

    // 두 번째 로그인은 쓰지 않는다. discovery·JWKS는 캐시에서.
    signed_in(&env, &idp).await;
    assert_eq!(stored(&env, "alice@okta").unwrap().revision, user.revision);
    assert_eq!((idp.hits("discovery"), idp.hits("jwks")), (1, 1));
}

/// authorization endpoint에 쿼리가 있으면 `&`로 잇는다.
#[tokio::test]
async fn the_authorization_endpoint_keeps_its_query() {
    let (env, idp) = setup().await;
    idp.set_discovery(|d| {
        d["authorization_endpoint"] = json!(format!("{}/authorize?tenant=t1", idp.issuer))
    });
    let started = start(&env, "okta").await;
    assert!(
        started.location.starts_with(&format!(
            "{}/authorize?tenant=t1&response_type=code&",
            idp.issuer
        )),
        "{}",
        started.location
    );
}

#[tokio::test]
async fn starting_fails_for_other_realms_and_unreachable_providers() {
    let (env, idp) = setup().await;
    // 없는 영역, LDAP 영역
    assert_failed_start(&start(&env, "nowhere").await, "sso_failed");
    let (status, body) = env
        .admin()
        .await
        .post(
            "/realms",
            json!({ "name": "corp", "backend": {
                "kind": "ldap", "url": "ldap://127.0.0.1:1", "base_dn": "dc=example,dc=com" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_failed_start(&start(&env, "corp").await, "sso_failed");
    // discovery 실패(issuer 다름, 닿지 않음)
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    assert_failed_start(&start(&env, "okta").await, "realm_unavailable");
    drop(idp);
    assert_failed_start(&start(&env, "okta").await, "realm_unavailable");
}

fn assert_failed_start(hop: &Hop, code: &str) {
    assert_eq!(hop.status, StatusCode::FOUND);
    assert_eq!(hop.location, format!("/login?error={code}"));
    assert!(hop.cookie("opl_oidc").is_none(), "{:?}", hop.cookies);
}

/// Host 헤더가 없거나 이상하면 `start`가 리디렉트 URI를 만들 수 없다. 콜백은 Host를 보지 않는다
/// (리디렉트 URI는 상태 쿠키에 있다).
#[tokio::test]
async fn start_needs_a_valid_host_and_the_callback_does_not() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (env, _idp) = setup().await;
    let addr = env.base.strip_prefix("http://").unwrap();
    for (request, status) in [
        ("GET /api/v1/auth/oidc/okta/start HTTP/1.0\r\n\r\n", "400"),
        (
            "GET /api/v1/auth/oidc/okta/start HTTP/1.0\r\nHost: evil.example.com/x?\r\n\r\n",
            "400",
        ),
        (
            "GET /api/v1/auth/oidc/callback?code=c&state=s HTTP/1.0\r\n\r\n",
            "302",
        ),
    ] {
        let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        tcp.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tcp.read_to_string(&mut response).await.unwrap();
        assert_eq!(
            response.split_whitespace().nth(1),
            Some(status),
            "{request}: {response}"
        );
    }
}

/// 콜백은 `start`가 만든 리디렉트 URI(상태 쿠키 안)로 코드를 바꾼다. 다른 Host로 와도 같다.
#[tokio::test]
async fn the_callback_uses_the_redirect_uri_from_the_state() {
    let (env, idp) = setup().await;
    let (callback, cookie) = authorize(&env, &idp).await;
    let resp = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(&callback)
        .header("host", "other.example.com:8443")
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()["location"], "/");
    let (_, form) = &idp.token_requests()[0];
    assert_eq!(form["redirect_uri"], format!("{}{CALLBACK}", env.base));
}

#[tokio::test]
async fn the_state_cookie_must_be_present_signed_fresh_and_match() {
    let (env, idp) = setup().await;
    let (callback, cookie) = authorize(&env, &idp).await;
    // 쿠키 없음
    assert_failed(&open(&callback, None).await, "sso_failed");
    // state 다름
    let q = query(&callback);
    let other = format!("{}{CALLBACK}?code={}&state=other", env.base, q["code"]);
    assert_failed(&open(&other, Some(&cookie)).await, "sso_failed");
    // 바뀐 바이트
    let mut bytes = cookie.clone().into_bytes();
    let i = bytes.len() - 5;
    bytes[i] = if bytes[i] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(bytes).unwrap();
    assert_failed(&open(&callback, Some(&tampered)).await, "sso_failed");
    // 다른 클러스터의 키로 서명한 쿠키, 10분이 지난 쿠키
    let ca_key = env.cluster.list::<ClusterSpec>().unwrap().1[0]
        .spec
        .ca_key_pem
        .clone();
    let forged = |key: &str, created: i64| {
        let state = LoginState {
            realm: "okta".into(),
            state: q["state"].clone(),
            nonce: "n".into(),
            verifier: "v".into(),
            redirect_uri: format!("{}{CALLBACK}", env.base),
            created,
        };
        format!("opl_oidc={}", state.seal(&state_key(key)))
    };
    assert_failed(
        &open(&callback, Some(&forged("other", unix_now()))).await,
        "sso_failed",
    );
    assert_failed(
        &open(&callback, Some(&forged(&ca_key, unix_now() - 601))).await,
        "sso_failed",
    );
    // 여기까지는 토큰 끝점에 가지 않았다.
    assert_eq!(idp.hits("token"), 0);
    assert!(stored(&env, "alice@okta").is_none());
    // 같은 키로 서명한 새 쿠키는 확인을 지난다(검증자가 달라 IdP가 invalid_grant로 거절한다).
    assert_failed(
        &open(&callback, Some(&forged(&ca_key, unix_now()))).await,
        "sso_failed",
    );
    assert_eq!(idp.hits("token"), 1);
    // 그 코드는 IdP가 이미 썼다. 새로 시작하면 된다.
    let (callback, cookie) = authorize(&env, &idp).await;
    let hop = open(&callback, Some(&cookie)).await;
    assert_eq!(hop.location, "/");
}

#[tokio::test]
async fn an_error_from_the_identity_provider_fails_the_login() {
    let (env, idp) = setup().await;
    idp.set_authorize_error(Some("access_denied"));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert_eq!(idp.hits("token"), 0);
}

/// 토큰 끝점의 invalid_grant(코드가 틀림·씀·만료)는 sso_failed, 그 밖의 4xx·5xx·연결 오류는
/// realm_unavailable
#[tokio::test]
async fn token_endpoint_failures() {
    let (env, idp) = setup().await;
    idp.fail_token(Some((500, json!({ "error": "server_error" }))));
    assert_failed(&sign_in(&env, &idp).await, "realm_unavailable");
    idp.fail_token(Some((400, json!({ "error": "invalid_grant" }))));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    idp.fail_token(Some((401, json!({ "error": "invalid_client" }))));
    assert_failed(&sign_in(&env, &idp).await, "realm_unavailable");
    idp.fail_token(Some((400, json!({ "error": "invalid_request" }))));
    assert_failed(&sign_in(&env, &idp).await, "realm_unavailable");
    // 콜백 전에 IdP가 사라졌다.
    idp.fail_token(None);
    let (callback, cookie) = authorize(&env, &idp).await;
    drop(idp);
    assert_failed(&open(&callback, Some(&cookie)).await, "realm_unavailable");
}

#[tokio::test]
async fn the_id_token_must_carry_the_nonce() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "nonce": "someone-elses" }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    idp.set_claims(json!({ "nonce": null }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert!(stored(&env, "alice@okta").is_none());
}

/// userinfo가 액세스 토큰을 받지 않으면(401·403) sso_failed, 그 밖의 실패는 realm_unavailable
#[tokio::test]
async fn userinfo_failures() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "preferred_username": null }));
    for (status, code) in [
        (401, "sso_failed"),
        (403, "sso_failed"),
        (500, "realm_unavailable"),
    ] {
        idp.fail_userinfo(Some(status));
        assert_failed(&sign_in(&env, &idp).await, code);
    }
    assert!(stored(&env, "alice@okta").is_none());
}

/// 콜백에서도 discovery 실패(문서가 맞지 않음 포함)는 start처럼 realm_unavailable이다.
#[tokio::test]
async fn discovery_failures_at_the_callback_mean_the_realm_is_unavailable() {
    let (env, idp) = setup().await;
    let (callback, cookie) = authorize(&env, &idp).await;
    // 영역을 고쳐(revision) 콜백이 discovery를 새로 받게 하고, IdP의 문서가 다른 issuer다.
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    let admin = env.admin().await;
    let (_, realm) = admin.get("/realms/okta").await;
    let mut body = realm_body("okta", &idp);
    body["display"] = json!("Okta 2");
    let (status, updated) = admin
        .put("/realms/okta", body, realm["revision"].as_u64())
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_failed(&open(&callback, Some(&cookie)).await, "realm_unavailable");
    assert_eq!(idp.hits("token"), 0);
}

/// RFC 9207: IdP가 인가 응답의 iss를 지원한다고 하면 콜백의 iss가 있어야 하고 issuer와 같아야 한다.
/// 지원하지 않아도 iss가 오면 같아야 한다.
#[tokio::test]
async fn the_authorization_response_issuer_is_checked() {
    let (env, idp) = setup().await;
    // 지원하지 않음: 없으면 된다, 다르면 거절
    signed_in(&env, &idp).await;
    idp.set_authorize_iss(Some("https://evil.example.com"));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    let issuer = idp.issuer.clone();
    idp.set_authorize_iss(Some(&issuer));
    signed_in(&env, &idp).await;

    // 지원함(새 영역 — discovery를 새로 받음)
    let (env, idp) = setup().await;
    idp.set_discovery(|d| d["authorization_response_iss_parameter_supported"] = json!(true));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    idp.set_authorize_iss(Some("https://evil.example.com"));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    let issuer = idp.issuer.clone();
    idp.set_authorize_iss(Some(&issuer));
    signed_in(&env, &idp).await;
    // 토큰 끝점에 가기 전에 거절했다.
    assert_eq!(idp.token_requests().len(), 1);
}

#[tokio::test]
async fn missing_claims_are_read_from_userinfo() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "preferred_username": null, "groups": null }));
    idp.set_userinfo(json!({
        "sub": SUBJECT,
        "preferred_username": "Bob",
        "groups": ["opl-viewers"],
    }));
    let session = signed_in(&env, &idp).await;
    assert_eq!(idp.hits("userinfo"), 1);
    let (_, me) = whoami(&env, &session).await;
    assert_eq!(
        (me["name"].as_str(), me["role"].as_str()),
        (Some("bob@okta"), Some("viewer"))
    );
    // 이름만 없으면 userinfo의 이름과 ID 토큰의 그룹
    idp.set_claims(json!({ "groups": ["opl-admins"] }));
    let session = signed_in(&env, &idp).await;
    assert_eq!(whoami(&env, &session).await.1["role"], "admin");
    assert_eq!(idp.hits("userinfo"), 2);
    // userinfo의 sub가 다르면 다른 사람이다.
    idp.set_userinfo(json!({ "sub": "mallory", "preferred_username": "Bob" }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    // userinfo 끝점이 없고 이름이 없다.
    let (env, idp) = setup().await;
    idp.set_discovery(|d| {
        d.as_object_mut().unwrap().remove("userinfo_endpoint");
    });
    idp.set_claims(json!({ "preferred_username": null }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert_eq!(idp.hits("userinfo"), 0);
}

#[tokio::test]
async fn groups_without_a_role_fail_with_no_role() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "groups": ["nobody"] }));
    assert_failed(&sign_in(&env, &idp).await, "no_role");
    // 그룹이 없고 userinfo에도 없다.
    idp.set_claims(json!({ "groups": null }));
    idp.set_userinfo(json!({ "sub": SUBJECT }));
    assert_failed(&sign_in(&env, &idp).await, "no_role");
    // 그룹 클레임이 문자열 배열이 아니다.
    idp.set_claims(json!({ "groups": "opl-admins" }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert!(stored(&env, "alice@okta").is_none());
}

#[tokio::test]
async fn usernames_must_follow_the_external_name_rules() {
    let (env, idp) = setup().await;
    for bad in [
        json!("alice smith"),
        json!("a@b@c"),
        json!("@corp.com"),
        json!("a/b"),
        json!(""),
        json!(7),
    ] {
        idp.set_claims(json!({ "preferred_username": bad }));
        assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    }
    // Okta·Entra ID의 이메일 모양 이름은 그대로(소문자로) 쓴다.
    idp.set_claims(json!({ "preferred_username": "Alice.Smith+ops@Corp.com" }));
    let session = signed_in(&env, &idp).await;
    let (_, me) = whoami(&env, &session).await;
    assert_eq!(me["name"], "alice.smith+ops@corp.com@okta");
    let found = audit_entries(&env, |e| {
        e.iter()
            .any(|e| e.detail.as_deref() == Some("realm=okta username=alice.smith+ops@corp.com"))
    })
    .await;
    assert!(!found.is_empty());
    // 다른 클레임을 이름으로 쓰는 영역
    let (status, realm) = env.admin().await.get("/realms/okta").await;
    assert_eq!(status, StatusCode::OK, "{realm}");
    let mut body = realm_body("okta", &idp);
    body["backend"]["username_claim"] = json!("email_local");
    let (status, updated) = env
        .admin()
        .await
        .put("/realms/okta", body, realm["revision"].as_u64())
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    idp.set_claims(json!({ "email_local": "Alice.S" }));
    let session = signed_in(&env, &idp).await;
    assert_eq!(whoami(&env, &session).await.1["name"], "alice.s@okta");
}

/// 공개 클라이언트는 비밀 없이 PKCE만으로 로그인한다.
#[tokio::test]
async fn public_clients_sign_in_with_pkce_only() {
    let (env, idp) = setup_with(Idp::start_with(CLIENT_ID, None).await).await;
    signed_in(&env, &idp).await;
    let (authorization, form) = &idp.token_requests()[0];
    assert_eq!(*authorization, None);
    assert_eq!(form["client_id"], CLIENT_ID);
    assert!(form.contains_key("code_verifier"));
    assert!(!form.contains_key("client_secret"));
}

/// client_secret_basic은 client_id와 비밀을 각각 form-urlencode한다(RFC 6749 §2.3.1).
#[tokio::test]
async fn client_secret_basic_encodes_the_id_and_the_secret() {
    let (env, idp) = setup_with(Idp::start_with("opl:ite", Some("s3cr:t+/ é")).await).await;
    signed_in(&env, &idp).await;
    let (authorization, _) = &idp.token_requests()[0];
    assert_eq!(
        authorization.as_deref(),
        Some(
            format!(
                "Basic {}",
                STANDARD.encode("opl%3Aite:s3cr%3At%2B%2F+%C3%A9")
            )
            .as_str()
        )
    );
}

#[tokio::test]
async fn a_changed_group_changes_the_role_and_ends_old_sessions() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "groups": ["opl-viewers"] }));
    let old = signed_in(&env, &idp).await;
    assert_eq!(whoami(&env, &old).await.1["role"], "viewer");
    idp.set_claims(json!({ "groups": ["opl-viewers", "opl-admins"] }));
    let new = signed_in(&env, &idp).await;
    let user = stored(&env, "alice@okta").unwrap();
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 1));
    assert_eq!(whoami(&env, &new).await.1["role"], "admin");
    assert_eq!(whoami(&env, &old).await.0, StatusCode::UNAUTHORIZED);
}

/// 외부 사용자는 처음 만든 IdP 사용자(iss·sub)에 묶인다. 같은 이름의 다른 사람은 거절한다.
#[tokio::test]
async fn users_are_bound_to_their_subject() {
    let (env, idp) = setup().await;
    signed_in(&env, &idp).await;
    let user = stored(&env, "alice@okta").unwrap();
    // 이름을 다시 쓴 다른 사람(다른 sub)
    idp.set_claims(json!({ "sub": "mallory-sub", "groups": ["opl-viewers"] }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert_eq!(stored(&env, "alice@okta").unwrap(), user);
    // 같은 sub면 된다.
    idp.set_claims(json!({ "sub": SUBJECT, "groups": ["opl-admins"] }));
    signed_in(&env, &idp).await;
    assert_eq!(stored(&env, "alice@okta").unwrap().revision, user.revision);
}

/// subject 없이 만든 사용자(옛 데이터)는 이번 로그인의 subject를 적는다(역할이 같으면 세션은 그대로).
#[tokio::test]
async fn users_without_a_subject_get_one() {
    let (env, idp) = setup().await;
    let old: Resource = opl_types::resource::Resource::new(
        "alice@okta",
        UserSpec {
            password_hash: String::new(),
            role: Role::Admin,
            session_epoch: 0,
            realm: Some("okta".into()),
            subject: None,
        },
    );
    env.cluster.create(&old).await.unwrap();
    signed_in(&env, &idp).await;
    let user = stored(&env, "alice@okta").unwrap();
    assert_eq!(user.id, old.id);
    assert_eq!(user.spec.subject, Some(format!("{} {SUBJECT}", idp.issuer)));
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 0));
}

async fn audit_entries(env: &Env, enough: impl Fn(&[AuditEntry]) -> bool) -> Vec<AuditEntry> {
    let start = Instant::now();
    loop {
        let entries = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap();
        if enough(&entries) {
            return entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn callbacks_are_audited() {
    let (env, idp) = setup().await;
    signed_in(&env, &idp).await;
    idp.set_claims(json!({ "groups": ["nobody"] }));
    assert_failed(&sign_in(&env, &idp).await, "no_role");
    assert_failed(
        &open(&format!("{}{CALLBACK}?code=c&state=s", env.base), None).await,
        "sso_failed",
    );
    let callbacks = |entries: &[AuditEntry]| {
        entries
            .iter()
            .filter(|e| e.action == "GET /auth/oidc/callback")
            .cloned()
            .collect::<Vec<_>>()
    };
    let entries = audit_entries(&env, |e| callbacks(e).len() >= 3).await;
    let found = callbacks(&entries);
    let detail = |detail: &str| {
        found
            .iter()
            .find(|e| e.detail.as_deref() == Some(detail))
            .unwrap_or_else(|| panic!("{detail}: {found:#?}"))
    };
    let ok = detail("realm=okta username=alice");
    assert_eq!(ok.user.as_deref(), Some("alice@okta"));
    assert_eq!(ok.status, 302);
    assert_eq!(detail("realm=okta error=no_role").user, None);
    assert_eq!(detail("realm=<unknown> error=sso_failed").user, None);
    // 경로는 쿼리를 뺀 것이다. start는 적지 않는다. 코드·state·쿠키·토큰은 어디에도 남지 않는다.
    assert!(
        found.iter().all(|e| e.path == "/auth/oidc/callback"),
        "{found:#?}"
    );
    assert!(!entries.iter().any(|e| e.action.contains("/start")));
    let text = serde_json::to_string(&entries).unwrap();
    for secret in ["code=", "state=", "client-secret", "opl_oidc"] {
        assert!(!text.contains(secret), "{secret}: {text}");
    }
}

/// 콜백은 시도 제한을 쓰지 않는다(맞힐 비밀이 없다 — state는 서명됐고 코드는 한 번 쓰며 PKCE에 묶인다).
/// 실패한 콜백이 많아도 같은 주소의 비밀번호 로그인과 SSO는 막히지 않는다.
#[tokio::test]
async fn failed_callbacks_do_not_block_logins() {
    let (env, idp) = setup().await;
    let url = format!("{}{CALLBACK}?code=c&state=s", env.base);
    for _ in 0..25 {
        assert_failed(&open(&url, None).await, "sso_failed");
    }
    signed_in(&env, &idp).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&json!({ "username": "admin", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
```

`crates/opl-api/tests/realms.rs`에서

```rust

    // 시도 제한에 세지 않는다(몇 번이든 같은 400).
    for _ in 0..21 {
        let got = anonymous
            .post(
                "/auth/login",
                json!({ "username": "alice", "password": "pw", "realm": "okta" }),
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "realm okta signs in through its identity provider"
        );
    }
```

을 다음으로 바꾼다:

```rust

    // 시도 제한에 세지 않는다(몇 번이든 같은 400). LDAP 이름 규칙에 맞지 않는 이메일 모양 이름도 같다.
    for username in ["alice", "alice@corp.com"] {
        for _ in 0..21 {
            let got = anonymous
                .post(
                    "/auth/login",
                    json!({ "username": username, "password": "pw", "realm": "okta" }),
                )
                .await;
            assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
            assert_eq!(
                got.1["message"],
                "realm okta signs in through its identity provider"
            );
        }
    }
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
//! 감사 로그: 한 노드의 `GET /audit`가 두 노드의 항목을 합치고, 닿지 않는 노드를 알린다.
```

을 다음으로 바꾼다:

```rust
//! 감사 로그: 한 노드의 `GET /audit`가 두 노드의 항목을 합치고, 닿지 않는 노드를 알린다.
//! OIDC: 한 노드에서 시작한 로그인을 다른 노드가 끝낸다(상태 쿠키의 키는 클러스터 CA 키에서).
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
    assert!(audit_nodes(&body).iter().all(|n| *n == "node1"), "{body:#}");
}
```

을 다음으로 바꾼다:

```rust
    assert!(audit_nodes(&body).iter().all(|n| *n == "node1"), "{body:#}");
}

/// `found`가 참이 될 때까지 기다린다(복제를 기다린다).
async fn until(what: &str, found: impl Fn() -> bool) {
    let start = Instant::now();
    while !found() {
        assert!(start.elapsed() < WAIT, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 한 노드에서 시작한 OIDC 로그인의 콜백이 다른 노드로 와도(같은 주소 뒤의 다른 노드) 끝난다.
#[tokio::test]
async fn an_oidc_login_started_on_one_node_finishes_on_the_other() {
    use opl_types::realm::RealmSpec;
    let (_root, a, b) = two_nodes().await;
    // opld가 클러스터를 만들 때처럼 클러스터 정보(CA 키)를 두고, 외부 사용자를 쓰도록 두 노드가 이 빌드를 돈다.
    common::seed_cluster_info(&a.cluster).await;
    for node in [&a, &b] {
        let mut status = a.cluster.get::<NodeSpec>(node.id).unwrap().unwrap().status;
        status.version = Some(opl_types::version::BUILD.into());
        a.cluster
            .update_status::<NodeSpec>(node.id, &status)
            .await
            .unwrap();
    }
    let idp = common::idp::Idp::start().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin
        .post(
            "/realms",
            serde_json::json!({
                "name": "okta",
                "backend": idp.backend(),
                "roles": { "groups": [{ "group": "opl-admins", "role": "admin" }] }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    until("B has the realm", || {
        b.cluster
            .get_by_name::<RealmSpec>("okta")
            .unwrap()
            .is_some()
    })
    .await;

    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let started = browser
        .get(format!("{}/api/v1/auth/oidc/okta/start", a.base))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::FOUND);
    let location = started.headers()["location"].to_str().unwrap().to_string();
    let cookie = started.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let callback = idp.follow(&location).await;
    assert!(callback.starts_with(&a.base), "{callback}");
    // A에서 시작했지만 B가 받는다. 토큰 교환에는 상태 쿠키의 리디렉트 URI(A의 주소)를 쓴다.
    let resp = browser
        .get(callback.replacen(&a.base, &b.base, 1))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()["location"], "/");
    let session = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .find(|c| c.starts_with("opl_session="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    until("B has the user", || {
        b.cluster
            .get_by_name::<UserSpec>("alice@okta")
            .unwrap()
            .is_some()
    })
    .await;
    let me: serde_json::Value = browser
        .get(format!("{}/api/v1/auth/whoami", b.base))
        .header("cookie", &session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["name"], "alice@okta", "{me}");
    assert_eq!(me["role"], "admin", "{me}");
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1400개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(api): OIDC 로그인 흐름 끝점 — GET /auth/oidc/{realm}/start(인증 없음)는 Host로 리디렉트 URI(<http|https>://<Host>/api/v1/auth/oidc/callback — secure_cookies면 https, Host가 없거나 호스트 글자가 아니면 400)를 만들고, 로그인 상태(state·nonce·PKCE 검증자)를 서명한 쿠키 opl_oidc(HttpOnly; SameSite=Lax; Path=/api/v1/auth/oidc; Max-Age=600, secure_cookies면 Secure)를 놓고 authorization endpoint로 302(response_type=code, client_id, redirect_uri, scope 공백으로 이음, state, nonce, code_challenge, S256 — 기존 쿼리 뒤에 &로). OIDC 영역이 아니거나 없으면 /login?error=sso_failed, discovery 실패는 realm_unavailable. GET /auth/oidc/callback은 IP 키로 시도를 미리 세고(성공하면 되돌림, 막히면 too_many_attempts), 쿠키(서명·10분·state 같음)를 확인한 뒤 IdP error면 실패, 토큰 끝점에 코드·redirect_uri·code_verifier·client_id를 폼으로 보내고(비밀이 있으면 client_id·비밀을 각각 form-urlencode한 client_secret_basic, reqwest form 기능), ID 토큰을 노드 캐시로 확인, 이름·그룹 클레임이 ID 토큰에 없으면 access_token으로 userinfo를 한 번 읽어 빈 것만 채움(sub가 다르면 실패), 이름은 문자열(소문자 외부 사용자 이름 규칙), 그룹은 문자열 배열(없으면 빈 것) → role_for → 계획 35의 upsert_external → 세션 쿠키와 opl_oidc 지우기, /로 302. 실패는 원인을 warn!으로 남기고 opl_oidc를 지운 뒤 /login?error=<sso_failed|no_role|realm_unavailable|too_many_attempts>(IdP에 닿지 못하거나 2xx가 아니면 realm_unavailable). 감사 미들웨어가 GET /auth/oidc/callback을 적음(detail realm=<r> username=<u> 또는 realm=<r|<unknown>> error=<code>, 성공이면 주체). AppState.oidc(OidcCache), AttemptLimiter::begin_ip가 Attempt를 돌려주고 success_ip 추가, users::new_session·upsert_external·cluster::cluster_info를 pub(crate)로, 가짜 IdP는 client_secret_basic을 풀어 비교하고 Idp::follow로 authorize를 따라감. 시험: tests/oidc_login.rs(성공 흐름·쿠키 속성, 쿠키 없음·state 다름·만료·변조·다른 키, IdP error, 토큰 끝점 실패, nonce, userinfo 채우기·sub 다름, no_role, 이름 규칙, 공개 클라이언트, client_secret_basic 인코딩, 역할 바뀜, 감사, IP 시도 제한, Host 없음), two_nodes.rs의 다른 노드로 돌아온 콜백. schema.d.ts 다시 만듦"
```

---

### Task 4: oplctl OIDC 영역 명령

`oplctl realm add-oidc|set-oidc`(`--clear-*`), `realm show`·`test`의 OIDC, `oplctl login --token-file`(SSO 사용자가 웹에서 만든 API 토큰으로 로그인 — CA 지문 고정, whoami로 확인), `login --realm <OIDC 영역>`은 안내와 함께 거절, 로그아웃·다시 로그인은 oplctl이 만든 토큰(`oplctl-…`)만 서버에서 지움, 경로 조각 퍼센트 인코딩.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`
- Modify: `crates/oplctl/src/console.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{
    ApiError, BACKUP_WAIT, Client, IMPORT_WAIT, MIGRATE_WAIT, UPGRADE_WAIT, fetch_ca, fingerprint,
    local, seg,
};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version = opl_types::version::BUILD, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(login이 만든 토큰 `oplctl-…`이면 서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지나 opld가 만드는 VXLAN 네트워크)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    /// VM NIC에 거는 방화벽 규칙 묶음. nftables가 있는 노드가 적용한다.
    SecurityGroup {
        #[command(subcommand)]
        cmd: SecurityGroupCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    /// VM 백업(백업 풀에 담은 디스크 복사본과 VM 설정)과 복원
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// 백업 작업: 정한 시각(UTC)에 고른 VM들을 백업하고 보존 규칙대로 옛 백업을 지운다.
    BackupJob {
        #[command(subcommand)]
        cmd: BackupJobCmd,
    },
    /// 다른 하이퍼바이저(VMware)의 VM 가져오기(OVA, ESXi·vCenter)
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// 인증 영역(admin만): 바깥 LDAP 디렉터리나 OIDC IdP로 로그인하게 한다.
    Realm {
        #[command(subcommand)]
        cmd: RealmCmd,
    },
    /// 클러스터 설정(업데이트 채널)
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// 구독 키와 상태. 구독은 기능을 막지 않는다.
    Subscription {
        #[command(subcommand)]
        cmd: SubscriptionCmd,
    },
    /// 옵트인 텔레메트리(기본 꺼짐). 켜면 리더가 하루 한 번 클러스터 규모를 보낸다.
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },
    /// 감사 로그(admin만): 모든 노드의 바꾸는 요청·로그인·콘솔 열기를 새 것부터
    Audit(AuditArgs),
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin", conflicts_with = "token_file")]
    user: String,
    /// 인증 영역. `local`이 아니면 그 영역의 사용자 이름(`@영역` 없이)으로 로그인한다.
    #[arg(long, default_value = "local", conflicts_with = "token_file")]
    realm: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long, conflicts_with = "token_file")]
    password_file: Option<PathBuf>,
    /// 웹 UI에서 만든 API 토큰이 든 파일(`-`면 표준 입력). 비밀번호 대신 이 토큰을 확인해 저장한다
    /// (SSO(OIDC) 사용자).
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get {
        node: String,
    },
    /// 노드의 업데이트 확인 결과(올릴 수 있는 패키지)를 보인다.
    Updates {
        node: String,
        /// 먼저 노드에서 다시 확인하고(Task가 끝날 때까지 기다린다) 결과를 보인다.
        #[arg(long)]
        refresh: bool,
    },
    /// 노드의 패키지를 올린다(`apt-get dist-upgrade`). VM은 계속 돌고 opld가 잠깐 다시 시작한다. 한 번에
    /// 한 노드만 한다.
    Upgrade {
        node: String,
        /// 업그레이드 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
}

/// `node upgrade`: 업그레이드 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
/// 방법을 알린다.
async fn node_upgrade(
    client: &Client,
    node: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/nodes/{}/upgrade", seg(node)), &json!({}))
        .await?;
    if wait {
        client.wait_task_for(&task, UPGRADE_WAIT).await?;
        writeln!(out, "node {node} upgraded")?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "upgrade of {node} started (task {id}); follow it with `oplctl task get {id}`"
        )?;
    }
    Ok(())
}

/// `node updates`의 패키지 표 열
const UPDATE_HEADERS: &[&str] = &["PACKAGE", "CURRENT", "CANDIDATE"];

/// `node updates`: `--refresh`면 다시 확인 Task를 기다린 뒤, 노드의 `status.updates`를 보인다. 확인 시각,
/// 오류, 패키지 표(새로 설치할 패키지는 CURRENT가 `-`), 잘렸는지. `-o json`이면 `status.updates` 그대로.
async fn node_updates(
    client: &Client,
    node: &str,
    refresh: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if refresh {
        let task = client
            .post(&format!("/nodes/{}/updates/refresh", seg(node)), &json!({}))
            .await?;
        client.wait_task(&task).await?;
    }
    let found = client.get(&format!("/nodes/{}", seg(node))).await?;
    let updates = &found["status"]["updates"];
    if json_out {
        return Ok(print_json(out, updates)?);
    }
    if !updates.is_object() {
        bail!("node {node} does not check for updates");
    }
    let checked = updates["checked_at"]
        .as_i64()
        .map_or_else(|| "never".to_string(), unix_time);
    writeln!(out, "checked: {checked}")?;
    if let Some(error) = updates["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let packages = updates["packages"].as_array().cloned().unwrap_or_default();
    if packages.is_empty() {
        if updates["checked_at"].is_u64() && updates["error"].is_null() {
            writeln!(out, "no updates available")?;
        }
        return Ok(());
    }
    let rows: Vec<_> = packages
        .iter()
        .map(|p| vec![cell(&p["name"]), cell(&p["current"]), cell(&p["candidate"])])
        .collect();
    table(out, UPDATE_HEADERS, &rows)?;
    if updates["truncated"] == true {
        writeln!(out, "(only the first {} packages are shown)", rows.len())?;
    }
    Ok(())
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀, 공유 LVM 풀을 만든다.
    Create(Box<PoolCreate>),
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다. 백업을 담는 풀로 표시하거나 끈다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args(["shared", "no_shared", "backups"])))]
struct PoolSet {
    pool: String,
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long, conflicts_with = "no_shared")]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
    /// VM 백업을 담는 풀로 표시한다(true, 디렉터리·NFS 풀만)거나 끈다(false, 백업이 남아 있으면 거절한다).
    #[arg(long, value_name = "true|false")]
    backups: Option<bool>,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`). 주지 않은 값은 보내지 않는다.
fn pool_set_body(set: &PoolSet) -> Value {
    let mut body = json!({});
    if set.shared || set.no_shared {
        body["shared"] = json!(set.shared);
    }
    if let Some(backups) = set.backups {
        body["backups"] = json!(backups);
    }
    body
}

/// `pool set`이 바꾼 것: `shared, backups on`
fn pool_set_summary(set: &PoolSet) -> String {
    let mut parts = Vec::new();
    if set.shared {
        parts.push("shared");
    } else if set.no_shared {
        parts.push("node-local");
    }
    match set.backups {
        Some(true) => parts.push("backups on"),
        Some(false) => parts.push("backups off"),
        None => {}
    }
    parts.join(", ")
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd", "lvm_shared"])))]
struct PoolCreate {
    name: String,
    /// 풀 디렉터리(절대 경로)
    #[arg(long, conflicts_with = "nfs")]
    path: Option<PathBuf>,
    /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
    #[arg(long, value_parser = parse_nfs)]
    nfs: Option<NfsSource>,
    /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
    #[arg(long, requires = "nfs", conflicts_with = "path")]
    nfs_options: Option<String>,
    /// 관리자가 준비한 VG와 thin pool `VG/THINPOOL`. 노드 로컬 풀이고 볼륨은 raw만 담는다.
    #[arg(long, value_parser = parse_lvm_thin, conflicts_with_all = ["path", "nfs", "nfs_options", "shared"])]
    lvm_thin: Option<LvmThinSource>,
    /// 관리자가 준비한 ZFS 데이터셋 `POOL/DATASET`. 노드 로컬 풀이고 볼륨은 raw zvol만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "shared"])]
    zfs: Option<String>,
    /// 관리자가 준비한 Ceph 풀과 RBD 네임스페이스 `POOL[/NAMESPACE]`. 늘 공유 풀이고 볼륨은 raw
    /// 이미지만 담는다.
    #[arg(long, value_parser = parse_rbd, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "shared"])]
    rbd: Option<RbdSource>,
    /// RBD 풀에 쓸 Ceph 클라이언트 id(`client.` 없이). 생략하면 admin.
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_user: Option<String>,
    /// RBD 풀에 쓸 ceph.conf 절대 경로. 생략하면 Ceph 기본(/etc/ceph/ceph.conf).
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_conf: Option<String>,
    /// 관리자가 준비한 공유 VG(lvmlockd). 늘 공유 풀이고 볼륨은 raw 두꺼운 LV만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "rbd", "rbd_user", "rbd_conf", "shared"])]
    lvm_shared: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃의 포털 `HOST[:PORT]`(IPv6는 `[ADDR][:PORT]`). 노드가 로그인한다.
    #[arg(long, requires_all = ["lvm_shared", "iscsi_target"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_portal: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃 이름(IQN)
    #[arg(long, requires_all = ["lvm_shared", "iscsi_portal"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_target: Option<String>,
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD·공유 LVM 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
    /// VM 백업을 담는 풀로 표시한다. 디렉터리·NFS 풀만 된다.
    #[arg(long, conflicts_with_all = ["lvm_thin", "zfs", "rbd", "lvm_shared"])]
    backups: bool,
}

/// `pool create`의 요청 본문. NFS·RBD·공유 LVM 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
fn pool_create_body(args: PoolCreate) -> Value {
    let PoolCreate {
        name,
        path,
        nfs,
        nfs_options,
        lvm_thin,
        zfs,
        rbd,
        rbd_user,
        rbd_conf,
        lvm_shared,
        iscsi_portal,
        iscsi_target,
        shared,
        nodes,
        backups,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd, lvm_shared) {
        (Some(n), _, _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _, _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r), _) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None, Some(vg)) => {
            // 포털과 타깃은 clap이 함께 받게 한다.
            let iscsi = iscsi_portal
                .zip(iscsi_target)
                .map(|(portal, target)| json!({ "portal": portal, "target": target }));
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": iscsi })
        }
        (None, None, None, None, None) => json!({ "kind": "dir", "path": path }),
    };
    let mut spec = json!({ "backend": backend, "nodes": nodes });
    if shared {
        spec["shared"] = json!(true);
    }
    if backups {
        spec["backups"] = json!(true);
    }
    json!({ "name": name, "spec": spec })
}

/// `--nfs`로 받은 NFS 서버와 내보내기 경로
#[derive(Debug, Clone, PartialEq, Eq)]
struct NfsSource {
    server: String,
    export: String,
}

/// `nas:/srv/vm`, `[fd00::1]:/srv/vm`처럼 쓴다. 서버 이름의 나머지 검사는 API가 한다.
fn parse_nfs(s: &str) -> Result<NfsSource, String> {
    let split = match s.strip_prefix('[') {
        Some(rest) => rest.split_once("]:"),
        None => s.split_once(':'),
    };
    match split {
        Some((server, export)) if !server.is_empty() && export.starts_with('/') => Ok(NfsSource {
            server: server.into(),
            export: export.into(),
        }),
        _ => Err(format!(
            "invalid nfs source {s:?}: use SERVER:/EXPORT ([ADDR]:/EXPORT for IPv6)"
        )),
    }
}

/// `--lvm-thin`으로 받은 VG와 thin pool 이름
#[derive(Debug, Clone, PartialEq, Eq)]
struct LvmThinSource {
    vg: String,
    thinpool: String,
}

/// `vg0/tp`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_lvm_thin(s: &str) -> Result<LvmThinSource, String> {
    match s.split_once('/') {
        Some((vg, thinpool))
            if !vg.is_empty() && !thinpool.is_empty() && !thinpool.contains('/') =>
        {
            Ok(LvmThinSource {
                vg: vg.into(),
                thinpool: thinpool.into(),
            })
        }
        _ => Err(format!("invalid lvm-thin pool {s:?}: use VG/THINPOOL")),
    }
}

/// `--rbd`로 받은 Ceph 풀과 RBD 네임스페이스
#[derive(Debug, Clone, PartialEq, Eq)]
struct RbdSource {
    pool: String,
    namespace: Option<String>,
}

/// `rbd`, `rbd/opl`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_rbd(s: &str) -> Result<RbdSource, String> {
    let (pool, namespace) = match s.split_once('/') {
        Some((pool, namespace)) => (pool, Some(namespace)),
        None => (s, None),
    };
    let bad = |n: &str| n.is_empty() || n.contains('/');
    if bad(pool) || namespace.is_some_and(bad) {
        return Err(format!(
            "invalid rbd pool {s:?}: use POOL or POOL/NAMESPACE"
        ));
    }
    Ok(RbdSource {
        pool: pool.into(),
        namespace: namespace.map(Into::into),
    })
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크나 VXLAN 네트워크를 만든다.
    Create(NetworkCreate),
    Delete {
        network: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["bridge", "vxlan"])))]
struct NetworkCreate {
    name: String,
    /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
    #[arg(long)]
    bridge: Option<String>,
    /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
    #[arg(long, conflicts_with = "vxlan")]
    vlan: Option<u16>,
    /// VXLAN 네트워크 식별자(VNI, 1-16777215). opld가 모든 노드에 브리지 oplv<VNI>를 만든다.
    #[arg(long, value_name = "VNI")]
    vxlan: Option<u32>,
    /// VXLAN 네트워크의 MTU(1280-9000, 기본 1450)
    #[arg(long, conflicts_with = "bridge")]
    mtu: Option<u16>,
}

/// `network create`의 요청 본문(`POST /networks`). 주지 않은 값은 보내지 않는다.
fn network_create_body(c: &NetworkCreate) -> Value {
    let mut spec = serde_json::Map::new();
    if let Some(bridge) = &c.bridge {
        spec.insert("bridge".into(), json!(bridge));
    }
    if let Some(vlan) = c.vlan {
        spec.insert("vlan".into(), json!(vlan));
    }
    if let Some(vni) = c.vxlan {
        spec.insert("vxlan".into(), json!({ "vni": vni }));
    }
    if let Some(mtu) = c.mtu {
        spec.insert("mtu".into(), json!(mtu));
    }
    json!({ "name": c.name, "spec": spec })
}

#[derive(Subcommand)]
enum SecurityGroupCmd {
    List,
    Get {
        group: String,
    },
    /// 보안 그룹을 만든다. 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다. 맞는 규칙이 없으면
    /// 들어오는 것은 막고 나가는 것은 보낸다.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 규칙 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`
        /// (예: "in accept tcp 22 10.0.0.0/8"). 여러 번 줄 수 있다.
        #[arg(long = "rule", value_name = "RULE", value_parser = parse_rule)]
        rules: Vec<Value>,
    },
    /// 보안 그룹을 지운다. 그룹을 쓰는 VM NIC가 없어야 한다.
    Delete {
        group: String,
    },
    /// 규칙을 더하거나 뺀다. 그룹을 쓰는 노드가 곧바로 적용한다.
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// 규칙 하나를 더한다(RULE은 `security-group create --rule`과 같다).
    Add {
        group: String,
        #[arg(value_parser = parse_rule)]
        rule: Value,
        /// 이 번호(1부터) 자리에 넣는다. 생략하면 맨 끝이다.
        #[arg(long, value_name = "N")]
        at: Option<usize>,
    },
    /// N번 규칙(1부터, `security-group get`의 순서)을 뺀다.
    Rm { group: String, n: usize },
}

/// `in accept tcp 22 10.0.0.0/8`처럼 쓴다: 방향과 동작, 그 뒤로 프로토콜·포트·원격 CIDR을 이 차례로
/// (모두 생략할 수 있다). 값의 나머지 검사(포트 범위, 프로토콜과 포트·주소의 짝)는 API가 한다.
fn parse_rule(s: &str) -> Result<Value, String> {
    let bad = || {
        format!("invalid rule {s:?}: use in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]")
    };
    let mut words = s.split_whitespace().peekable();
    let direction = words
        .next()
        .filter(|w| matches!(*w, "in" | "out"))
        .ok_or_else(bad)?;
    let action = words
        .next()
        .filter(|w| matches!(*w, "accept" | "drop"))
        .ok_or_else(bad)?;
    let mut rule = json!({ "direction": direction, "action": action });
    if let Some(protocol) = words.next_if(|w| matches!(*w, "tcp" | "udp" | "icmp" | "icmpv6")) {
        rule["protocol"] = json!(protocol);
    }
    if let Some(ports) = words.next_if(|w| is_ports(w)) {
        rule["ports"] = json!(ports);
    }
    if let Some(remote) = words.next_if(|w| w.contains(['.', ':'])) {
        rule["remote"] = json!(remote);
    }
    if words.next().is_some() {
        return Err(bad());
    }
    Ok(rule)
}

/// `22`나 `8000-8080` 모양인지
fn is_ports(word: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match word.split_once('-') {
        Some((start, end)) => digits(start) && digits(end),
        None => digits(word),
    }
}

/// `security-group create`의 요청 본문(`POST /security-groups`)
fn security_group_create_body(name: &str, description: Option<&str>, rules: &[Value]) -> Value {
    let mut spec = json!({ "rules": rules });
    if let Some(description) = description {
        spec["description"] = json!(description);
    }
    json!({ "name": name, "spec": spec })
}

/// 그룹의 규칙에 `rule`을 `at`번(1부터, 생략하면 맨 끝) 자리에 넣은 목록
fn rules_with(group: &Value, rule: Value, at: Option<usize>) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let at = at.unwrap_or(rules.len() + 1);
    if at == 0 || at > rules.len() + 1 {
        bail!(
            "cannot add a rule at {at}: security group {} has {} rules (use 1 to {})",
            cell(&group["name"]),
            rules.len(),
            rules.len() + 1
        );
    }
    rules.insert(at - 1, rule);
    Ok(rules)
}

/// 그룹의 규칙에서 `n`번(1부터)을 뺀 목록
fn rules_without(group: &Value, n: usize) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if n == 0 || n > rules.len() {
        bail!(
            "security group {} has no rule {n} ({} rules, numbered from 1)",
            cell(&group["name"]),
            rules.len()
        );
    }
    rules.remove(n - 1);
    Ok(rules)
}

/// 그룹을 읽어 `edit`으로 만든 규칙 목록으로 바꾼다. 읽은 revision을 `If-Match`로 보내므로 그 사이
/// 다른 사람이 바꿨으면 실패한다(규칙 번호가 달라졌을 수 있다).
async fn patch_rules(
    client: &Client,
    group: &str,
    edit: impl FnOnce(&Value) -> anyhow::Result<Vec<Value>>,
) -> anyhow::Result<()> {
    let path = format!("/security-groups/{}", seg(group));
    let current = client.get(&path).await?;
    let rules = edit(&current)?;
    let r = client
        .request(Method::PATCH, &path)
        .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
        .json(&json!({ "rules": rules }));
    client.send(r).await?;
    Ok(())
}

/// `security-group list`의 열
const SECURITY_GROUP_HEADERS: &[&str] = &["NAME", "RULES", "DESCRIPTION"];

/// `security-group list`의 한 행
fn security_group_row(g: &Value) -> Vec<String> {
    vec![
        cell(&g["name"]),
        g["spec"]["rules"]
            .as_array()
            .map_or(0, Vec::len)
            .to_string(),
        cell(&g["spec"]["description"]),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
    },
    Delete {
        volume: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    /// VM 설정을 바꾼다. 실행 중이어도 된다.
    Set(VmSet),
    /// NIC의 보안 그룹을 바꾼다. 실행 중이어도 곧바로 적용된다. 그룹을 주지 않으면 모두 뺀다.
    NicSg {
        vm: String,
        /// NIC 번호(0부터, `vm get`의 nics 순서)
        index: usize,
        groups: Vec<String>,
    },
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(디스크가 모두 공유 풀에 있어야 한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
    },
    Delete {
        vm: String,
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다. `,sg=A+B`를 붙이면 그 NIC에
    /// 보안 그룹을 건다(예: `lan,sg=web+db`).
    #[arg(long = "nic", value_name = "NETWORK[,sg=GROUP+...]", value_parser = parse_nic)]
    nics: Vec<NicArg>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. 펜싱이 켜진 노드에서만 돈다.
    #[arg(long)]
    ha: bool,
}

/// `--nic`으로 받은 네트워크와 보안 그룹
#[derive(Debug, Clone, PartialEq, Eq)]
struct NicArg {
    network: String,
    security_groups: Vec<String>,
}

/// `lan`, `lan,sg=web+db`처럼 쓴다. 그룹이 있는지는 API가 본다.
fn parse_nic(s: &str) -> Result<NicArg, String> {
    let bad = || format!("invalid nic {s:?}: use NETWORK or NETWORK,sg=GROUP+GROUP");
    let (network, groups) = match s.split_once(',') {
        Some((network, option)) => {
            let groups = option.strip_prefix("sg=").ok_or_else(bad)?;
            let groups: Vec<String> = groups.split('+').map(str::to_string).collect();
            if groups.iter().any(|g| g.is_empty() || g.contains(',')) {
                return Err(bad());
            }
            (network, groups)
        }
        None => (s, Vec::new()),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NicArg {
        network: network.into(),
        security_groups: groups,
    })
}

/// `vm create`의 NIC 하나. 그룹이 없으면 `security_groups`를 보내지 않는다.
fn nic_body(nic: &NicArg) -> Value {
    let mut body = json!({ "network": nic.network, "model": "virtio" });
    if !nic.security_groups.is_empty() {
        body["security_groups"] = json!(nic.security_groups);
    }
    body
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).args(["ha", "no_ha"])))]
struct VmSet {
    vm: String,
    /// HA VM으로 만든다. VM이 멈춰 있어야 하고 VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
    ha: bool,
    /// HA를 끈다.
    #[arg(long)]
    no_ha: bool,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
}

#[derive(Subcommand)]
enum BackupCmd {
    /// VM을 백업한다. 실행 중이면 디스크를 잠깐 스냅샷한 때의 상태(crash-consistent)를 담는다.
    Create {
        vm: String,
        /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
        #[arg(long)]
        pool: String,
        /// 생략하면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
        #[arg(long)]
        name: Option<String>,
    },
    /// 백업 목록. VM을 지운 백업도 보인다.
    List {
        /// 이 VM(이름이나 id)의 백업만. 지운 VM은 백업에 남은 이름으로 찾는다.
        #[arg(long)]
        vm: Option<String>,
    },
    Get {
        backup: String,
    },
    /// 백업 파일과 기록을 지운다. 실패한 백업도 지운다.
    Delete {
        backup: String,
        /// 파일을 지울 노드가 없으면 기록만 지운다(파일은 백업 풀에 남는다).
        #[arg(long)]
        force: bool,
    },
    /// 백업에서 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Restore {
        backup: String,
        /// 새 VM 이름
        #[arg(long)]
        name: String,
        /// 새 디스크를 둘 풀
        #[arg(long)]
        pool: String,
        /// 복원을 실행할 노드(이름이나 id). 생략하면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
        #[arg(long)]
        node: Option<String>,
    },
}

#[derive(Subcommand)]
enum BackupJobCmd {
    /// 백업 작업을 만든다. 정한 요일의 시각(UTC)에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛
    /// 백업을 지운다.
    Create(BackupJobCreate),
    List,
    Get {
        job: String,
    },
    /// 준 값만 바꾼다. 일정이나 보존 규칙의 일부만 주면 나머지는 지금 값을 그대로 둔다.
    Set(BackupJobSet),
    /// 작업만 지운다. 작업이 만든 백업은 남는다.
    Delete {
        job: String,
    },
    /// 지금 실행한다. 리더의 스케줄러가 다음 차례에 시작하며 끝나기를 기다리지 않는다.
    Run {
        job: String,
    },
}

/// 백업할 VM 고르기. 셋 중 하나만 준다.
#[derive(Args)]
struct VmsArgs {
    /// 모든 VM
    #[arg(long)]
    all: bool,
    /// 이 이름의 VM(여러 번 줄 수 있다)
    #[arg(long = "vm", value_name = "NAME")]
    vms: Vec<String>,
    /// 이 라벨을 모두 가진 VM(여러 번 줄 수 있다)
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = parse_label)]
    labels: Vec<(String, String)>,
}

/// 보존 규칙. 모두 0이면 지우지 않는다.
#[derive(Args)]
struct KeepArgs {
    /// 최신 N개를 남긴다.
    #[arg(long, value_name = "N")]
    keep_last: Option<u32>,
    /// 그다음 하루(UTC)마다 가장 새 것 하나씩 N일
    #[arg(long, value_name = "N")]
    keep_daily: Option<u32>,
    /// 그다음 ISO 주마다 가장 새 것 하나씩 N주
    #[arg(long, value_name = "N")]
    keep_weekly: Option<u32>,
    /// 그다음 달마다 가장 새 것 하나씩 N달
    #[arg(long, value_name = "N")]
    keep_monthly: Option<u32>,
}

impl KeepArgs {
    fn given(&self) -> bool {
        self.keep_last.is_some()
            || self.keep_daily.is_some()
            || self.keep_weekly.is_some()
            || self.keep_monthly.is_some()
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").required(true).args(["all", "vms", "labels"])))]
struct BackupJobCreate {
    name: String,
    /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
    #[arg(long)]
    pool: String,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: String,
    /// 실행할 요일(쉼표로 나눔). 생략하면 매일.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 꺼 둔 채로 만든다.
    #[arg(long)]
    disabled: bool,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").args(["all", "vms", "labels"])))]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "all", "vms", "labels", "pool", "at", "days",
    "keep_last", "keep_daily", "keep_weekly", "keep_monthly", "enabled",
])))]
struct BackupJobSet {
    job: String,
    #[arg(long)]
    pool: Option<String>,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// 실행할 요일(쉼표로 나눔). 모든 요일을 주면 매일이다.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 켜거나(다음 실행 시각을 지금부터 다시 정한다) 끈다. 진행 중인 실행은 끝까지 간다.
    #[arg(long, value_name = "true|false")]
    enabled: Option<bool>,
}

/// `--label`의 `KEY=VALUE`. 값에는 `=`가 있어도 된다.
fn parse_label(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.into(), value.into())),
        _ => Err(format!("invalid label {s:?}: use KEY=VALUE")),
    }
}

/// VM 고르기(`vms` 값). 주지 않았으면 `None`이다.
fn vms_value(args: &VmsArgs) -> Option<Value> {
    if args.all {
        Some(json!({ "all": true }))
    } else if !args.vms.is_empty() {
        Some(json!({ "names": args.vms }))
    } else if !args.labels.is_empty() {
        let labels: serde_json::Map<String, Value> = args
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        Some(json!({ "labels": labels }))
    } else {
        None
    }
}

fn days_value(days: &[Day]) -> Value {
    json!(days.iter().map(|d| value_name(*d)).collect::<Vec<_>>())
}

/// 보존 규칙(`keep` 값). 주지 않은 단위는 `current`의 값, 거기도 없으면 0이다.
fn keep_value(args: &KeepArgs, current: &Value) -> Value {
    let pick = |arg: Option<u32>, field: &str| {
        arg.map_or_else(|| current[field].as_u64().unwrap_or(0), u64::from)
    };
    json!({
        "last": pick(args.keep_last, "last"),
        "daily": pick(args.keep_daily, "daily"),
        "weekly": pick(args.keep_weekly, "weekly"),
        "monthly": pick(args.keep_monthly, "monthly"),
    })
}

/// `backup-job create`의 요청 본문
fn backup_job_create_body(args: &BackupJobCreate) -> Value {
    json!({
        "name": args.name,
        "spec": {
            "vms": vms_value(&args.vms),
            "pool": args.pool,
            "schedule": { "at": args.at, "days": days_value(&args.days) },
            "keep": keep_value(&args.keep, &Value::Null),
            "enabled": !args.disabled,
        }
    })
}

/// `backup-job set`의 요청 본문(`PATCH /backup-jobs/{key}`). 준 값만 보낸다. 서버는 일정과 보존
/// 규칙을 통째로 바꾸므로 그 일부만 주었으면 나머지는 읽은 작업(`current`)의 값으로 채운다.
fn backup_job_set_body(args: &BackupJobSet, current: &Value) -> Value {
    let mut body = json!({});
    if let Some(vms) = vms_value(&args.vms) {
        body["vms"] = vms;
    }
    if let Some(pool) = &args.pool {
        body["pool"] = json!(pool);
    }
    let schedule = &current["spec"]["schedule"];
    if args.at.is_some() || !args.days.is_empty() {
        body["schedule"] = json!({
            "at": args.at.as_ref().map_or_else(|| schedule["at"].clone(), |at| json!(at)),
            "days": if args.days.is_empty() {
                schedule["days"].as_array().map_or_else(|| json!([]), |d| json!(d))
            } else {
                days_value(&args.days)
            },
        });
    }
    if args.keep.given() {
        body["keep"] = keep_value(&args.keep, &current["spec"]["keep"]);
    }
    if let Some(enabled) = args.enabled {
        body["enabled"] = json!(enabled);
    }
    body
}

#[derive(Subcommand)]
enum ImportCmd {
    /// VMware 등에서 내보낸 OVA를 이 서버의 노드에 올리고, 원본 VM을 읽어 VM 설정을 제안받는다.
    /// 원본 파일은 그 노드에 남고 실행도 그 노드에서 한다.
    Ova {
        file: PathBuf,
        /// 가져오기 이름
        #[arg(long)]
        name: String,
    },
    /// ESXi·vCenter의 데이터스토어에 있는 VM을 보인다. 비밀번호는 `--password-file`이나
    /// `OPL_ESXI_PASSWORD`에서, 없으면 터미널에서 묻는다. 지문을 주지 않으면 서버 인증서의 지문을 알려 준다.
    EsxiBrowse {
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// ESXi·vCenter의 꺼진 VM을 읽어 VM 설정을 제안받는다. 비밀번호는 `esxi-browse`처럼 받고 연결을
    /// 받은 노드의 메모리에만 남으므로, 그 opld를 다시 시작하면 가져오기를 다시 만들어야 한다.
    Esxi {
        /// 가져오기 이름
        name: String,
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문(`import esxi-browse`가 알려 준다)
        #[arg(long)]
        fingerprint: String,
        #[arg(long)]
        datastore: String,
        /// 데이터스토어 안의 `.vmx` 경로(예: `web01/web01.vmx`)
        #[arg(long)]
        vmx: String,
    },
    List,
    /// 원본 VM, 제안, 경고, 마지막 실행의 오류
    Get {
        import: String,
    },
    /// 원본 디스크를 볼륨으로 옮겨 새 VM(꺼짐)을 만든다. 주지 않은 값은 제안 값이다.
    Run(Box<ImportRunArgs>),
    /// 가져오기와 원본 파일을 지운다(만든 VM은 남는다).
    Delete {
        import: String,
        /// 원본 노드가 없거나 오프라인이면 기록만 지운다(원본 파일은 그 노드에 남는다).
        #[arg(long)]
        force: bool,
    },
}

/// ESXi·vCenter 연결. 비밀번호는 인자로 받지 않는다.
#[derive(Args)]
struct EsxiArgs {
    /// ESXi 호스트나 vCenter 주소
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 443)]
    port: u16,
    #[arg(long)]
    user: String,
    /// vCenter면 데이터센터 이름
    #[arg(long, default_value = "ha-datacenter")]
    datacenter: String,
    /// 비밀번호가 든 파일. 없으면 `OPL_ESXI_PASSWORD`, 그것도 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
}

/// ESXi 비밀번호. `--password-file`, `OPL_ESXI_PASSWORD` 순서로 보고, 둘 다 없으면 터미널에서 묻는다.
fn esxi_password(conn: &EsxiArgs) -> anyhow::Result<String> {
    if let Some(file) = &conn.password_file {
        return read_password(Some(file), "");
    }
    match std::env::var("OPL_ESXI_PASSWORD") {
        Ok(password) => Ok(password),
        Err(_) => Ok(rpassword::prompt_password(format!(
            "password for {}@{}: ",
            conn.user, conn.host
        ))?),
    }
}

/// API의 `EsxiConnection`
fn esxi_connection(conn: &EsxiArgs, fingerprint: Option<&str>, password: &str) -> Value {
    json!({
        "host": conn.host,
        "port": conn.port,
        "user": conn.user,
        "password": password,
        "datacenter": conn.datacenter,
        "fingerprint": fingerprint,
    })
}

/// `import esxi`의 요청 본문
fn esxi_import_body(name: &str, connection: &Value, datastore: &str, vmx: &str) -> Value {
    json!({ "name": name, "connection": connection, "datastore": datastore, "vmx": vmx })
}

#[derive(Args)]
struct ImportRunArgs {
    import: String,
    /// 새 VM 이름
    #[arg(long)]
    name: String,
    /// 디스크 볼륨을 만들 풀
    #[arg(long)]
    pool: String,
    /// 원본 네트워크를 opLite 네트워크에 잇는다(예: `"VM Network=lan"`). 원본 NIC의 네트워크마다
    /// 준다. 원본 네트워크가 하나면 `NETWORK`만 줘도 된다.
    #[arg(long = "net", value_name = "SOURCE=NETWORK", value_parser = parse_net)]
    nets: Vec<NetArg>,
    #[arg(long, value_enum)]
    disk_bus: Option<DiskBus>,
    #[arg(long, value_enum)]
    nic_model: Option<NicModel>,
    #[arg(long, value_enum)]
    firmware: Option<Firmware>,
    #[arg(long)]
    sockets: Option<u16>,
    #[arg(long)]
    cores: Option<u16>,
    /// 메모리(MiB)
    #[arg(long)]
    memory: Option<u32>,
    /// 원본 MAC 주소 대신 새 MAC을 받는다.
    #[arg(long)]
    new_macs: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum DiskBus {
    VirtioBlk,
    Sata,
}

#[derive(Clone, Copy, ValueEnum)]
enum NicModel {
    Virtio,
    E1000e,
}

/// `--net`으로 받은 매핑. `source`가 없으면 하나뿐인 원본 네트워크다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NetArg {
    source: Option<String>,
    network: String,
}

fn parse_net(s: &str) -> Result<NetArg, String> {
    let bad = || format!("invalid network mapping {s:?}: use SOURCE=NETWORK or NETWORK");
    // 원본 네트워크 이름에 `=`가 있을 수 있다. opLite 네트워크 이름에는 없다.
    let (source, network) = match s.rsplit_once('=') {
        Some(("", _)) => return Err(bad()),
        Some((source, network)) => (Some(source.to_string()), network),
        None => (None, s),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NetArg {
        source,
        network: network.into(),
    })
}

/// 가져오기의 원본 네트워크(제안의 네트워크 매핑 키)
fn source_networks(import: &Value) -> Vec<String> {
    import["status"]["proposal"]["networks"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `import run`의 요청 본문. `sources`는 `=` 없는 `--net`을 이을 원본 네트워크다.
fn import_run_body(args: &ImportRunArgs, sources: &[String]) -> anyhow::Result<Value> {
    let mut networks = serde_json::Map::new();
    for net in &args.nets {
        let source = match (&net.source, sources) {
            (Some(source), _) => source.clone(),
            (None, [only]) => only.clone(),
            (None, _) => bail!(
                "--net {} needs SOURCE=NETWORK: vm-import {} has {} source networks ({})",
                net.network,
                args.import,
                sources.len(),
                sources
                    .iter()
                    .map(|s| format!("{s:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        networks.insert(source, json!(net.network));
    }
    Ok(json!({
        "name": args.name,
        "pool": args.pool,
        "networks": networks,
        "disk_bus": args.disk_bus.map(value_name),
        "nic_model": args.nic_model.map(value_name),
        "firmware": args.firmware.map(value_name),
        "sockets": args.sockets,
        "cores": args.cores,
        "memory_mib": args.memory,
        "keep_macs": !args.new_macs,
    }))
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// 설정을 `키: 값` 줄로 보인다.
    Get,
    /// 설정을 바꾼다. 키: `updates.channel`(stable|testing|enterprise)
    Set {
        /// 예: `updates.channel=testing`
        #[arg(required = true, value_name = "KEY=VALUE")]
        values: Vec<String>,
    },
}

/// `settings set`이 아는 키
const SETTING_KEYS: &[&str] = &["updates.channel"];

/// `settings set`의 요청 본문(`PATCH /settings`). 준 키만 보낸다.
fn settings_set_body(values: &[String]) -> anyhow::Result<Value> {
    let mut body = json!({});
    for value in values {
        let Some((key, v)) = value.split_once('=') else {
            bail!("expected KEY=VALUE, got {value:?}");
        };
        match key {
            "updates.channel" => body["updates"]["channel"] = json!(v),
            _ => bail!("unknown setting {key}; known: {}", SETTING_KEYS.join(", ")),
        }
    }
    Ok(body)
}

/// 객체의 값들을 `a.b: 값` 줄로 펼친다.
fn setting_lines(prefix: &str, value: &Value, lines: &mut Vec<String>) {
    match value.as_object() {
        Some(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                setting_lines(&key, v, lines);
            }
        }
        None => lines.push(format!("{prefix}: {}", cell(value))),
    }
}

async fn settings(
    client: &Client,
    cmd: SettingsCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SettingsCmd::Get => {
            let settings = client.get("/settings").await?;
            if json_out {
                print_json(out, &settings)?;
            } else {
                let mut lines = Vec::new();
                setting_lines("", &settings["spec"], &mut lines);
                for line in lines {
                    writeln!(out, "{line}")?;
                }
            }
        }
        SettingsCmd::Set { values } => {
            client
                .patch("/settings", &settings_set_body(&values)?)
                .await?;
            writeln!(out, "settings updated")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum SubscriptionCmd {
    /// 구독 상태를 보인다.
    Show,
    /// 구독 키를 넣는다(있던 키는 바꾼다).
    #[command(group(ArgGroup::new("source").required(true).args(["key", "file"])))]
    Set {
        /// `opl1.<payload>.<signature>`
        key: Option<String>,
        /// 키가 든 파일(`-`면 표준 입력)
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 구독 키를 지운다.
    Remove,
}

/// 구독 상태(`SubscriptionInfo`)를 `키: 값` 줄로. 없는 값은 줄을 쓰지 않는다.
fn subscription_lines(info: &Value) -> Vec<String> {
    let mut state = cell(&info["state"]);
    if info["development"] == true {
        state.push_str(" (development key)");
    }
    let mut lines = vec![format!("state: {state}")];
    for key in ["customer", "level"] {
        if !info[key].is_null() {
            lines.push(format!("{key}: {}", cell(&info[key])));
        }
    }
    let used = cell(&info["used_nodes"]);
    lines.push(match info["nodes"].as_u64() {
        Some(limit) => format!("nodes: {used}/{limit}"),
        None => format!("nodes: {used}"),
    });
    if let Some(at) = info["expires_at"].as_u64() {
        lines.push(format!("expires: {}", opl_types::subscription::date(at)));
    }
    lines
}

fn print_subscription(
    out: &mut (dyn Write + Send),
    json_out: bool,
    info: &Value,
) -> anyhow::Result<()> {
    if json_out {
        print_json(out, info)?;
    } else {
        for line in subscription_lines(info) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

async fn subscription(
    client: &Client,
    cmd: SubscriptionCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SubscriptionCmd::Show => {
            let info = client.get("/subscription").await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Set { key, file } => {
            let key = match key {
                Some(key) => key,
                None => match file {
                    Some(path) if path.as_os_str() == "-" => {
                        read_stdin_key(&mut std::io::stdin().lock())?
                    }
                    file => read_password(file.as_ref(), "")?,
                },
            };
            let info = client
                .put("/subscription", &json!({ "key": key.trim() }))
                .await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Remove => {
            client.delete("/subscription").await?;
            writeln!(out, "subscription removed")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum TelemetryCmd {
    /// 텔레메트리 설정과 마지막 전송을 보인다.
    Show,
    /// 텔레메트리를 켠다. 무엇을 보내는지는 `preview`로 먼저 본다.
    Enable {
        /// 보낼 `https://` 주소. 없으면 지금 끝점(설정 또는 빌드 기본값)
        #[arg(long)]
        endpoint: Option<String>,
    },
    /// 텔레메트리를 끈다(id는 남는다).
    Disable,
    /// 지금 보낼 보고를 JSON 그대로 보인다(admin).
    Preview,
    /// 보고에 붙는 id를 새로 만든다.
    ResetId,
}

/// `telemetry enable|disable`의 요청 본문(`PATCH /settings`)
fn telemetry_body(enabled: bool, endpoint: Option<&str>) -> Value {
    let mut body = json!({ "telemetry": { "enabled": enabled } });
    if let Some(endpoint) = endpoint {
        body["telemetry"]["endpoint"] = json!(endpoint);
    }
    body
}

/// 설정 리소스의 텔레메트리 부분을 `키: 값` 줄로. 끝점이 없으면 이 oplctl 빌드의 기본 끝점을 보인다.
fn telemetry_lines(settings: &Value) -> Vec<String> {
    let spec = &settings["spec"]["telemetry"];
    let status = &settings["status"]["telemetry"];
    let endpoint = match spec["endpoint"].as_str() {
        Some(endpoint) => endpoint.to_string(),
        None if !opl_types::version::TELEMETRY_URL.is_empty() => {
            format!("{} (build default)", opl_types::version::TELEMETRY_URL)
        }
        None => "none".into(),
    };
    let last_sent = match status["last_sent_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    vec![
        format!("enabled: {}", spec["enabled"] == true),
        format!("endpoint: {endpoint}"),
        format!("id: {}", status["id"].as_str().unwrap_or("none")),
        format!("last sent: {last_sent}"),
        format!(
            "last error: {}",
            status["last_error"].as_str().unwrap_or("none")
        ),
    ]
}

async fn telemetry(
    client: &Client,
    cmd: TelemetryCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let settings = match cmd {
        TelemetryCmd::Show => client.get("/settings").await?,
        TelemetryCmd::Enable { endpoint } => {
            client
                .patch("/settings", &telemetry_body(true, endpoint.as_deref()))
                .await?
        }
        TelemetryCmd::Disable => {
            client
                .patch("/settings", &telemetry_body(false, None))
                .await?
        }
        TelemetryCmd::Preview => {
            // 보낼 JSON 그대로 보인다.
            let report = client.get("/telemetry/preview").await?;
            print_json(out, &report)?;
            return Ok(());
        }
        TelemetryCmd::ResetId => client.post("/telemetry/reset-id", &json!({})).await?,
    };
    if json_out {
        print_json(out, &settings)?;
    } else {
        for line in telemetry_lines(&settings) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

#[derive(Args)]
struct AuditArgs {
    /// 이 시각부터: `30m`·`1h`·`7d`(지금에서 뺌)나 RFC 3339. 기본은 `--until`의 24시간 전
    #[arg(long)]
    since: Option<String>,
    /// 이 시각까지(RFC 3339). 기본은 지금
    #[arg(long)]
    until: Option<String>,
    /// 사용자 이름(정확히 같은 것)
    #[arg(long)]
    user: Option<String>,
    /// 노드 이름
    #[arg(long)]
    node: Option<String>,
    /// 동작(`POST /vms/{key}`)이나 경로에 든 문자열
    #[arg(long)]
    action: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    #[arg(long)]
    limit: Option<u32>,
}

/// `--since`: 기간(`30m`·`1h`·`7d`, 초 `s`도)이면 지금(`now_ms`)에서 뺀 유닉스 밀리초, 아니면
/// 그대로(RFC 3339나 밀리초) 보내 서버가 푼다.
fn audit_since(s: &str, now_ms: u64) -> String {
    match audit_duration_ms(s) {
        Some(ms) => now_ms.saturating_sub(ms).to_string(),
        None => s.to_string(),
    }
}

/// `30m`·`1h`·`7d`(초 `s`도) 같은 기간의 밀리초. 기간이 아니면 None
fn audit_duration_ms(s: &str) -> Option<u64> {
    let unit = match s.chars().last()? {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    let n = s[..s.len() - 1].parse::<u64>().ok()?;
    Some(n.saturating_mul(unit))
}

/// 유닉스 밀리초를 `2026-11-06T09:30:05Z`처럼(UTC)
fn unix_ms_time(ms: u64) -> String {
    match time::OffsetDateTime::from_unix_timestamp((ms / 1000) as i64) {
        Ok(t) => format!(
            "{}T{:02}:{:02}:{:02}Z",
            t.date(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => ms.to_string(),
    }
}

/// `audit`의 표 한 줄
fn audit_row(e: &Value) -> Vec<String> {
    // 대상이 없으면 detail(예: `audit.dropped`의 수, 로그인의 `username=…`)
    let target = match (e["target"]["kind"].as_str(), e["target"]["key"].as_str()) {
        (Some(kind), Some(key)) => format!("{kind}/{key}"),
        _ => e["detail"].as_str().unwrap_or("-").to_string(),
    };
    vec![
        e["time"].as_u64().map_or_else(|| "-".into(), unix_ms_time),
        cell(&e["node"]),
        cell(&e["user"]),
        cell(&e["ip"]),
        cell(&e["action"]),
        target,
        cell(&e["status"]),
    ]
}

/// `GET /audit`를 부르고 표로 보인다. 읽지 못한 노드, 감사 로그를 끈 노드와 잘림은 표 뒤
/// `err`(stderr)에 알린다.
async fn audit(
    client: &Client,
    args: AuditArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    if args.until.is_some()
        && args
            .since
            .as_deref()
            .is_some_and(|s| audit_duration_ms(s).is_some())
    {
        bail!("a relative --since cannot be combined with --until; give a time");
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let query: Vec<(&str, String)> = [
        ("since", args.since.map(|s| audit_since(&s, now_ms))),
        ("until", args.until),
        ("user", args.user),
        ("node", args.node),
        ("action", args.action),
        ("limit", args.limit.map(|n| n.to_string())),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect();
    let mut r = client.request(Method::GET, "/audit");
    if !query.is_empty() {
        r = r.query(&query);
    }
    let resp = client.send(r).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = resp["entries"]
        .as_array()
        .map(|a| a.iter().map(audit_row).collect())
        .unwrap_or_default();
    table(
        out,
        &["TIME", "NODE", "USER", "IP", "ACTION", "TARGET", "STATUS"],
        &rows,
    )?;
    let unreachable: Vec<&str> = resp["unreachable"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !unreachable.is_empty() {
        writeln!(
            err,
            "warning: no audit records from: {}",
            unreachable.join(", ")
        )?;
    }
    let disabled: Vec<&str> = resp["disabled"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !disabled.is_empty() {
        writeln!(
            err,
            "note: the audit log is disabled on: {}",
            disabled.join(", ")
        )?;
    }
    if resp["truncated"] == true {
        writeln!(
            err,
            "note: more entries match; narrow the range or raise --limit"
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

#[derive(Subcommand)]
enum RealmCmd {
    List,
    /// 영역 설정을 보인다(서비스 계정 비밀번호·클라이언트 비밀은 있는지만).
    Show {
        name: String,
    },
    /// LDAP 영역을 만든다.
    AddLdap(RealmAdd),
    /// LDAP 영역을 바꾼다. 준 것만 바꾼다.
    SetLdap(RealmSet),
    /// OIDC 영역을 만든다. IdP에는 `<노드 주소>/api/v1/auth/oidc/callback`을 리디렉트 URI로 등록한다.
    AddOidc(RealmAddOidc),
    /// OIDC 영역을 바꾼다. 준 것만 바꾼다.
    SetOidc(RealmSetOidc),
    /// 영역을 지운다. 그 영역 사용자들의 세션과 토큰은 멈춘다.
    Remove {
        name: String,
    },
    /// 영역을 시험한다(사용자를 만들지 않는다). LDAP은 사용자 이름과 비밀번호로 디렉터리를,
    /// OIDC는 입력 없이 IdP의 discovery 문서와 키를 확인한다.
    Test {
        name: String,
        /// 영역 안의 사용자 이름(`@영역` 없이). LDAP 영역에는 꼭 필요하고, OIDC 영역이면 무시한다(경고).
        #[arg(long)]
        user: Option<String>,
        /// 비밀번호가 든 파일(`-`면 표준 입력). 없으면 터미널에서 묻는다.
        #[arg(long, requires = "user")]
        password_file: Option<PathBuf>,
    },
}

#[derive(Args)]
struct RealmAdd {
    name: String,
    /// `ldap://host[:port]`(StartTLS) 또는 `ldaps://host[:port]`
    #[arg(long)]
    url: String,
    /// 사용자를 찾을 기준 DN
    #[arg(long)]
    base_dn: String,
    #[command(flatten)]
    ldap: LdapArgs,
}

#[derive(Args)]
struct RealmSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    base_dn: Option<String>,
    #[command(flatten)]
    ldap: LdapArgs,
    /// 저장된 서비스 계정 비밀번호를 지운다. 서비스 계정 DN이 남으면 안 된다(`--clear-bind-dn`).
    #[arg(long, conflicts_with = "bind_password_file")]
    clear_bind_password: bool,
    /// 서비스 계정 DN과 비밀번호를 지운다(익명 검색).
    #[arg(long, conflicts_with_all = ["bind_dn", "bind_password_file"])]
    clear_bind_dn: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

#[derive(Args)]
struct RealmAddOidc {
    name: String,
    /// IdP의 issuer(`https://...`). discovery 문서의 `issuer`와 글자 그대로 같아야 한다.
    #[arg(long)]
    issuer: String,
    /// IdP에 등록한 클라이언트 ID
    #[arg(long)]
    client_id: String,
    #[command(flatten)]
    oidc: OidcArgs,
}

#[derive(Args)]
struct RealmSetOidc {
    name: String,
    #[arg(long)]
    issuer: Option<String>,
    #[arg(long)]
    client_id: Option<String>,
    #[command(flatten)]
    oidc: OidcArgs,
    /// 저장된 클라이언트 비밀을 지운다(공개 클라이언트).
    #[arg(long, conflicts_with = "client_secret_file")]
    clear_client_secret: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

/// `realm add-oidc`와 `set-oidc`의 공통 선택
#[derive(Args)]
struct OidcArgs {
    /// 클라이언트 비밀이 든 파일(`-`면 표준 입력). 없으면 공개 클라이언트(PKCE만)
    #[arg(long)]
    client_secret_file: Option<PathBuf>,
    /// 요청할 scope(여러 번, `openid`가 있어야 함). set-oidc에서 주면 목록 전체를 바꾼다.
    #[arg(long = "scope", value_name = "SCOPE")]
    scopes: Vec<String>,
    /// 사용자 이름을 담은 클레임(기본 preferred_username)
    #[arg(long)]
    username_claim: Option<String>,
    /// 그룹(문자열 배열)을 담은 클레임(기본 groups)
    #[arg(long)]
    groups_claim: Option<String>,
    /// IdP 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// IdP 요청마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹 클레임 값을 가진 사용자의 역할(`<그룹>=<role>`, 여러 번). set-oidc에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "GROUP=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `realm add-ldap`과 `set-ldap`의 공통 선택
#[derive(Args)]
struct LdapArgs {
    /// 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다.
    #[arg(long)]
    bind_dn: Option<String>,
    /// 서비스 계정 비밀번호가 든 파일(`-`면 표준 입력)
    #[arg(long)]
    bind_password_file: Option<PathBuf>,
    /// 사용자 찾기 필터. `{username}`이 로그인 이름으로 바뀐다.
    #[arg(long)]
    user_filter: Option<String>,
    /// 그룹 DN을 담은 사용자 속성(기본 memberOf)
    #[arg(long)]
    group_attr: Option<String>,
    /// 서버 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// 연결과 작업마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹에 든 사용자의 역할(`<그룹 DN>=<role>`, 여러 번). set-ldap에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "DN=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `--group`의 `<그룹 DN>=<role>`. DN에 `=`가 있으므로 마지막 `=`에서 나눈다.
fn parse_group_role(s: &str) -> Result<(String, Role), String> {
    let (group, role) = s
        .rsplit_once('=')
        .ok_or_else(|| format!("invalid group {s:?}: use <group DN>=<role>"))?;
    let role = Role::from_str(role, true)
        .map_err(|_| format!("invalid role {role:?} in {s:?}: use admin, operator or viewer"))?;
    if group.trim().is_empty() {
        return Err(format!("invalid group {s:?}: use <group DN>=<role>"));
    }
    Ok((group.to_string(), role))
}

/// 파일에서 읽은 영역 값들(서비스 계정 비밀번호, CA)
#[derive(Default)]
struct LdapFiles {
    bind_password: Option<String>,
    ca_pem: Option<String>,
}

impl LdapFiles {
    fn read(args: &LdapArgs) -> anyhow::Result<Self> {
        Ok(Self {
            bind_password: args
                .bind_password_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// 비밀번호 파일(`-`면 표준 입력). 끝의 줄바꿈만 뗀다.
fn read_secret_file(path: &PathBuf) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)
            .context("cannot read standard input")?;
        return Ok(s.trim_end_matches(['\r', '\n']).to_string());
    }
    read_password(Some(path), "")
}

fn group_roles(groups: &[(String, Role)]) -> Value {
    groups
        .iter()
        .map(|(group, role)| json!({ "group": group, "role": value_name(*role) }))
        .collect()
}

/// `realm add-ldap`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_body(args: &RealmAdd, files: LdapFiles) -> Value {
    let l = &args.ldap;
    let mut backend = json!({ "kind": "ldap", "url": args.url, "base_dn": args.base_dn });
    let optional = [
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("bind_password", files.bind_password.map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut body = json!({
        "name": args.name,
        "backend": backend,
        "roles": new_roles(&l.groups, l.default_role)
    });
    if let Some(display) = &l.display {
        body["display"] = json!(display);
    }
    body
}

/// 만들 영역의 그룹 → 역할 표와 기본 역할
fn new_roles(groups: &[(String, Role)], default: Option<Role>) -> Value {
    let mut roles = json!({ "groups": group_roles(groups) });
    if let Some(role) = default {
        roles["default"] = json!(value_name(role));
    }
    roles
}

/// 바꿀 영역의 역할: 지금 표(`current`)에 준 것만 바꾼다.
fn changed_roles(
    current: &Value,
    groups: &[(String, Role)],
    default: Option<Role>,
    clear_groups: bool,
    clear_default_role: bool,
) -> Value {
    let mut roles = current.clone();
    if !groups.is_empty() {
        roles["groups"] = group_roles(groups);
    }
    if clear_groups {
        roles["groups"] = json!([]);
    }
    if let Some(role) = default {
        roles["default"] = json!(value_name(role));
    }
    if clear_default_role && let Some(r) = roles.as_object_mut() {
        r.remove("default");
    }
    roles
}

/// `set-ldap`·`set-oidc`가 바꿀 영역의 종류가 맞는지
fn ensure_kind(current: &Value, name: &str, kind: &str) -> anyhow::Result<()> {
    if current["backend"]["kind"] != kind {
        anyhow::bail!(
            "realm {name} is not an {} realm (its kind is {})",
            kind.to_uppercase(),
            cell(&current["backend"]["kind"])
        );
    }
    Ok(())
}

/// 파일에서 읽은 OIDC 영역 값들(클라이언트 비밀, CA)
#[derive(Default)]
struct OidcFiles {
    client_secret: Option<String>,
    ca_pem: Option<String>,
}

impl OidcFiles {
    fn read(args: &OidcArgs) -> anyhow::Result<Self> {
        Ok(Self {
            client_secret: args
                .client_secret_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// `realm add-oidc`·`set-oidc`가 준 선택 값들(이름과 값)
fn oidc_options(o: &OidcArgs, files: OidcFiles) -> [(&'static str, Option<Value>); 6] {
    [
        ("client_secret", files.client_secret.map(Value::from)),
        (
            "scopes",
            (!o.scopes.is_empty()).then(|| Value::from(o.scopes.clone())),
        ),
        ("username_claim", o.username_claim.clone().map(Value::from)),
        ("groups_claim", o.groups_claim.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", o.timeout.map(Value::from)),
    ]
}

/// `realm add-oidc`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_oidc_body(args: &RealmAddOidc, files: OidcFiles) -> Value {
    let o = &args.oidc;
    let mut backend = json!({ "kind": "oidc", "issuer": args.issuer, "client_id": args.client_id });
    for (key, value) in oidc_options(o, files) {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut body = json!({
        "name": args.name,
        "backend": backend,
        "roles": new_roles(&o.groups, o.default_role)
    });
    if let Some(display) = &o.display {
        body["display"] = json!(display);
    }
    body
}

/// `realm set-oidc`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 클라이언트 비밀은 주지 않으면 빼서(서버가 저장된 값을 둔다 — issuer·client_id·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_oidc_body(
    current: &Value,
    args: &RealmSetOidc,
    files: OidcFiles,
) -> anyhow::Result<Value> {
    ensure_kind(current, &args.name, "oidc")?;
    let o = &args.oidc;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_client_secret");
    }
    let mut changes = vec![
        ("issuer", args.issuer.clone().map(Value::from)),
        ("client_id", args.client_id.clone().map(Value::from)),
    ];
    changes.extend(oidc_options(o, files));
    if args.clear_client_secret {
        changes.push(("client_secret", Some(json!(""))));
    }
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if args.clear_ca
        && let Some(b) = backend.as_object_mut()
    {
        b.remove("ca_pem");
    }
    let roles = changed_roles(
        &current["roles"],
        &o.groups,
        o.default_role,
        args.clear_groups,
        args.clear_default_role,
    );
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = o
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// `realm set-ldap`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 서비스 계정 비밀번호는 주지 않으면 빼서(서버가 저장된 값을 둔다 — url·bind_dn·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_body(current: &Value, args: &RealmSet, files: LdapFiles) -> anyhow::Result<Value> {
    ensure_kind(current, &args.name, "ldap")?;
    let l = &args.ldap;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_bind_password");
    }
    let changes = [
        ("url", args.url.clone().map(Value::from)),
        ("base_dn", args.base_dn.clone().map(Value::from)),
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
        (
            "bind_password",
            if args.clear_bind_password || args.clear_bind_dn {
                Some(json!(""))
            } else {
                files.bind_password.map(Value::from)
            },
        ),
    ];
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if let Some(b) = backend.as_object_mut() {
        if args.clear_bind_dn {
            b.remove("bind_dn");
        }
        if args.clear_ca {
            b.remove("ca_pem");
        }
        // 서비스 계정 DN에는 비밀번호가 있어야 한다.
        if args.clear_bind_password && b.get("bind_dn").is_some_and(Value::is_string) {
            anyhow::bail!(
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
    }
    let roles = changed_roles(
        &current["roles"],
        &l.groups,
        l.default_role,
        args.clear_groups,
        args.clear_default_role,
    );
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = l
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// OIDC 영역의 콜백 경로. IdP에는 노드 주소 뒤에 붙여 리디렉트 URI로 등록한다.
const OIDC_CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";

/// `realm show`의 `키: 값` 줄
fn realm_lines(realm: &Value) -> Vec<String> {
    let b = &realm["backend"];
    let set = |v: bool| if v { "set" } else { "none" };
    let ca = if b["ca_pem"].is_string() {
        "custom (PEM)"
    } else {
        "system roots"
    };
    let mut lines = vec![
        format!("name: {}", cell(&realm["name"])),
        format!("display: {}", cell(&realm["display"])),
        format!("kind: {}", cell(&b["kind"])),
    ];
    if b["kind"] == "oidc" {
        let scopes: Vec<String> = b["scopes"]
            .as_array()
            .map(|a| a.iter().map(cell).collect())
            .unwrap_or_default();
        lines.extend([
            format!("issuer: {}", cell(&b["issuer"])),
            format!("client id: {}", cell(&b["client_id"])),
            format!("client secret: {}", set(b["has_client_secret"] == true)),
            format!("scopes: {}", scopes.join(" ")),
            format!("username claim: {}", cell(&b["username_claim"])),
            format!("groups claim: {}", cell(&b["groups_claim"])),
            format!("ca: {ca}"),
            format!("timeout: {}s", cell(&b["timeout_secs"])),
            // IdP에는 노드 주소 뒤에 이 경로를 붙여 리디렉트 URI로 등록한다.
            format!("callback path: {OIDC_CALLBACK_PATH}"),
        ]);
    } else {
        lines.extend([
            format!("url: {}", cell(&b["url"])),
            format!("base dn: {}", cell(&b["base_dn"])),
            format!("bind dn: {}", cell(&b["bind_dn"])),
            format!("bind password: {}", set(b["has_bind_password"] == true)),
            format!("user filter: {}", cell(&b["user_filter"])),
            format!("group attribute: {}", cell(&b["group_attr"])),
            format!("ca: {ca}"),
            format!("timeout: {}s", cell(&b["timeout_secs"])),
        ]);
    }
    let groups = realm["roles"]["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if groups.is_empty() {
        lines.push("groups: none".into());
    } else {
        lines.push("groups:".into());
        for g in &groups {
            lines.push(format!("  {} = {}", cell(&g["group"]), cell(&g["role"])));
        }
    }
    lines.push(format!(
        "default role: {}",
        realm["roles"]["default"].as_str().unwrap_or("none")
    ));
    lines
}

async fn realm(
    client: &Client,
    cmd: RealmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        RealmCmd::List => {
            let list = client.get("/realms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "DISPLAY", "KIND", "URL"],
                |r| {
                    vec![
                        cell(&r["name"]),
                        cell(&r["display"]),
                        cell(&r["backend"]["kind"]),
                        // OIDC 영역은 issuer
                        cell(if r["backend"]["kind"] == "oidc" {
                            &r["backend"]["issuer"]
                        } else {
                            &r["backend"]["url"]
                        }),
                    ]
                },
            )?;
        }
        RealmCmd::Show { name } => {
            let realm = client.get(&format!("/realms/{}", seg(&name))).await?;
            if json_out {
                print_json(out, &realm)?;
            } else {
                for line in realm_lines(&realm) {
                    writeln!(out, "{line}")?;
                }
            }
        }
        RealmCmd::AddLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            client
                .post("/realms", &realm_add_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            let path = format!("/realms/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::AddOidc(args) => {
            let files = OidcFiles::read(&args.oidc)?;
            client
                .post("/realms", &realm_add_oidc_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetOidc(args) => {
            let files = OidcFiles::read(&args.oidc)?;
            let path = format!("/realms/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_oidc_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::Remove { name } => {
            client.delete(&format!("/realms/{}", seg(&name))).await?;
            writeln!(out, "realm {name} removed")?;
        }
        RealmCmd::Test {
            name,
            user,
            password_file,
        } => {
            // OIDC 영역은 입력 없이 시험한다(LDAP 영역이면 서버가 사용자를 달라고 한다).
            let realm = client.get(&format!("/realms/{}", seg(&name))).await?;
            let kind = realm["backend"]["kind"].as_str().unwrap_or_default();
            if kind == "oidc" && user.is_some() {
                eprintln!("warning: --user is ignored for an OIDC realm");
            }
            let body = match &user {
                Some(user) if kind != "oidc" => {
                    let password = match &password_file {
                        Some(f) => read_secret_file(f)?,
                        None => read_password(None, "Password: ")?,
                    };
                    json!({ "username": user, "password": password })
                }
                _ => json!({}),
            };
            let result = client
                .post(&format!("/realms/{}/actions/test", seg(&name)), &body)
                .await?;
            let steps = result["steps"].as_array().cloned().unwrap_or_default();
            if json_out {
                print_json(out, &result)?;
            } else {
                let rows: Vec<Vec<String>> = steps
                    .iter()
                    .map(|s| {
                        let ok = if s["ok"] == true { "ok" } else { "failed" };
                        vec![cell(&s["name"]), ok.into(), cell(&s["message"])]
                    })
                    .collect();
                table(out, &["STEP", "RESULT", "MESSAGE"], &rows)?;
            }
            // 사용자 결과(DN·그룹·역할)는 LDAP 시험에만 있다.
            if !json_out && kind == "ldap" {
                writeln!(out, "dn: {}", cell(&result["dn"]))?;
                let groups: Vec<String> = result["groups"]
                    .as_array()
                    .map(|a| a.iter().map(cell).collect())
                    .unwrap_or_default();
                writeln!(
                    out,
                    "groups: {}",
                    if groups.is_empty() {
                        "-".into()
                    } else {
                        groups.join(", ")
                    }
                )?;
                writeln!(out, "role: {}", result["role"].as_str().unwrap_or("none"))?;
            }
            if let Some(failed) = steps.iter().find(|s| s["ok"] != true) {
                bail!("realm {name}: the {} step failed", cell(&failed["name"]));
            }
        }
    }
    Ok(())
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

/// 표준 입력의 구독 키(앞뒤 공백을 다듬음). 키를 명령줄에 두지 않으려고 쓴다.
fn read_stdin_key(input: &mut dyn std::io::Read) -> anyhow::Result<String> {
    let mut key = String::new();
    input
        .read_to_string(&mut key)
        .context("cannot read the key from standard input")?;
    Ok(key.trim().to_string())
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 볼륨 형식. 주지 않았으면 풀을 읽어 그 종류의 기본값으로 정한다.
async fn volume_format(
    client: &Client,
    pool: &str,
    format: Option<Format>,
) -> anyhow::Result<Format> {
    match format {
        Some(f) => Ok(f),
        None => {
            let read = client.get(&format!("/storage-pools/{}", seg(pool))).await;
            let pool = read.with_context(|| {
                format!(
                    "cannot read storage pool {pool:?} to choose a volume format; pass --format"
                )
            })?;
            Ok(pool_default_format(&pool))
        }
    }
}

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw(스냅샷은 블록 스냅샷,
/// 공유 LVM은 스냅샷 없음), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" || kind == "lvm-shared" {
        Format::Raw
    } else {
        Format::Qcow2
    }
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let (token, name) = match &args.token_file {
            Some(file) => token_login(&server, file, ca_pem.clone()).await?,
            None => login(&server, args, ca_pem.clone()).await?,
        };
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        // 새 토큰과 같으면 두고, 이 CLI의 login이 만든 토큰(`oplctl-…`)만 지운다(`--token-file`로 넣은
        // 토큰은 사용자가 웹 UI에서 만든 것이다).
        if let Some((old, old_server)) = previous
            && old != token
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(old), ca_pem)?;
            remove_login_token(&client, id).await;
        }
        writeln!(out, "logged in to {server} as {name}")?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            // login이 만든 토큰(`oplctl-…`)만 서버에서 지운다. 이미 지워진 토큰이어도 로컬에서는 지운다.
            let kept = match config.token.as_deref().and_then(token_id) {
                Some(id) => remove_login_token(&client, id).await == Some(false),
                None => false,
            };
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
            if kept {
                writeln!(
                    out,
                    "the token stays valid on the server; delete it in the web UI"
                )?;
            }
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{}", seg(&node))).await?)?
            }
            NodeCmd::Updates { node, refresh } => {
                node_updates(&client, &node, refresh, json_out, out).await?
            }
            NodeCmd::Upgrade { node, wait } => node_upgrade(&client, &node, wait, out).await?,
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                let nodes = if json_out {
                    Vec::new()
                } else {
                    items(&client.get("/nodes").await?)
                };
                print_list(out, json_out, &list, POOL_HEADERS, |p| pool_row(p, &nodes))?;
            }
            PoolCmd::Get { pool } => {
                let pool = client
                    .get(&format!("/storage-pools/{}", seg(&pool)))
                    .await?;
                if json_out {
                    print_json(out, &pool)?;
                } else {
                    let nodes = items(&client.get("/nodes").await?);
                    write!(out, "{}", describe_pool(&pool))?;
                    table(out, POOL_NODE_HEADERS, &pool_node_rows(&pool, &nodes))?;
                }
            }
            PoolCmd::Create(args) => {
                let name = args.name.clone();
                client
                    .post("/storage-pools", &pool_create_body(*args))
                    .await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Set(set) => {
                client
                    .patch(
                        &format!("/storage-pools/{}", seg(&set.pool)),
                        &pool_set_body(&set),
                    )
                    .await?;
                writeln!(out, "storage pool {}: {}", set.pool, pool_set_summary(&set))?;
            }
            PoolCmd::Delete { pool } => {
                client
                    .delete(&format!("/storage-pools/{}", seg(&pool)))
                    .await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, NETWORK_HEADERS, network_row)?;
            }
            NetworkCmd::Get { network } => print_json(
                out,
                &client.get(&format!("/networks/{}", seg(&network))).await?,
            )?,
            NetworkCmd::Create(c) => {
                client.post("/networks", &network_create_body(&c)).await?;
                writeln!(out, "network {} created", c.name)?;
            }
            NetworkCmd::Delete { network } => {
                client
                    .delete(&format!("/networks/{}", seg(&network)))
                    .await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::SecurityGroup { cmd } => security_group(&client, cmd, json_out, out).await?,
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => print_json(
                out,
                &client.get(&format!("/volumes/{}", seg(&volume))).await?,
            )?,
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{}/resize", seg(&volume)), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{}", seg(&volume))).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Import { cmd } => import(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    task_row,
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{}", seg(&task))).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{}", seg(&name)), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{}", seg(&name))).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{}", seg(&token))).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
        Cmd::Realm { cmd } => realm(&client, cmd, json_out, out).await?,
        Cmd::Settings { cmd } => settings(&client, cmd, json_out, out).await?,
        Cmd::Subscription { cmd } => subscription(&client, cmd, json_out, out).await?,
        Cmd::Telemetry { cmd } => telemetry(&client, cmd, json_out, out).await?,
        Cmd::Audit(args) => audit(&client, args, json_out, out, &mut std::io::stderr()).await?,
    }
    Ok(())
}

async fn security_group(
    client: &Client,
    cmd: SecurityGroupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SecurityGroupCmd::List => {
            let list = client.get("/security-groups").await?;
            print_list(
                out,
                json_out,
                &list,
                SECURITY_GROUP_HEADERS,
                security_group_row,
            )?;
        }
        SecurityGroupCmd::Get { group } => print_json(
            out,
            &client
                .get(&format!("/security-groups/{}", seg(&group)))
                .await?,
        )?,
        SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } => {
            let body = security_group_create_body(&name, description.as_deref(), &rules);
            client.post("/security-groups", &body).await?;
            writeln!(out, "security group {name} created")?;
        }
        SecurityGroupCmd::Delete { group } => {
            client
                .delete(&format!("/security-groups/{}", seg(&group)))
                .await?;
            writeln!(out, "security group {group} deleted")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Add { group, rule, at },
        } => {
            patch_rules(client, &group, |g| rules_with(g, rule, at)).await?;
            writeln!(out, "security group {group}: rule added")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Rm { group, n },
        } => {
            patch_rules(client, &group, |g| rules_without(g, n)).await?;
            writeln!(out, "security group {group}: rule {n} removed")?;
        }
    }
    Ok(())
}

async fn backup(
    client: &Client,
    cmd: BackupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupCmd::Create { vm, pool, name } => {
            let body = json!({ "pool": pool, "name": name });
            let task = client
                .post(&format!("/vms/{}/backups", seg(&vm)), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            // 이름을 주지 않았으면 서버가 정한 이름을 읽는다.
            let name = match name {
                Some(name) => name,
                None => {
                    let id = cell(&task["spec"]["op"]["backup"]);
                    cell(&client.get(&format!("/backups/{}", seg(&id))).await?["name"])
                }
            };
            writeln!(out, "backup {name} of vm {vm} created")?;
        }
        BackupCmd::List { vm } => {
            let mut r = client.request(Method::GET, "/backups");
            if let Some(vm) = &vm {
                r = r.query(&[("vm", vm)]);
            }
            let list = client.send(r).await?;
            print_list(out, json_out, &list, BACKUP_HEADERS, backup_row)?;
        }
        BackupCmd::Get { backup } => print_json(
            out,
            &client.get(&format!("/backups/{}", seg(&backup))).await?,
        )?,
        BackupCmd::Delete { backup, force } => backup_delete(client, &backup, force, out).await?,
        BackupCmd::Restore {
            backup,
            name,
            pool,
            node,
        } => {
            let body = json!({ "name": name, "pool": pool, "node": node });
            let task = client
                .post(&format!("/backups/{}/restore", seg(&backup)), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            writeln!(out, "vm {name} restored from backup {backup}")?;
        }
    }
    Ok(())
}

/// 백업을 지운다. `force`로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
async fn backup_delete(
    client: &Client,
    backup: &str,
    force: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut r = client.request(Method::DELETE, &format!("/backups/{}", seg(backup)));
    if force {
        r = r.query(&[("force", "true")]);
    }
    let got = client.send(r).await?;
    if let Some(message) = got["message"].as_str() {
        writeln!(out, "{message}")?;
    } else {
        client.wait_task(&got).await?;
        writeln!(out, "backup {backup} deleted")?;
    }
    Ok(())
}

/// `backup list`의 열
const BACKUP_HEADERS: &[&str] = &["NAME", "VM", "POOL", "PHASE", "CREATED", "SIZE"];

/// `backup list`의 한 행. VM은 만든 때의 이름이고(VM을 지웠어도 남는다), SIZE는 백업 파일 크기의 합이다.
fn backup_row(b: &Value) -> Vec<String> {
    let status = &b["status"];
    let created = status["created_at"]
        .as_i64()
        .map_or_else(|| "-".to_string(), unix_time);
    let size = match status["disks"].as_array() {
        Some(disks) if status["phase"] == "ready" => bytes(
            &disks
                .iter()
                .filter_map(|d| d["stored_bytes"].as_u64())
                .sum::<u64>()
                .into(),
        ),
        _ => "-".into(),
    };
    vec![
        cell(&b["name"]),
        cell(&b["spec"]["vm_name"]),
        cell(&b["spec"]["pool"]),
        cell(&status["phase"]),
        created,
        size,
    ]
}

async fn backup_job(
    client: &Client,
    cmd: BackupJobCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupJobCmd::Create(args) => {
            let job = client
                .post("/backup-jobs", &backup_job_create_body(&args))
                .await?;
            if args.disabled {
                writeln!(out, "backup job {} created (disabled)", args.name)?;
            } else {
                let next = job["status"]["next_run"]
                    .as_i64()
                    .map_or_else(|| "-".to_string(), unix_time);
                writeln!(out, "backup job {} created; next run {next}", args.name)?;
            }
        }
        BackupJobCmd::List => {
            let list = client.get("/backup-jobs").await?;
            print_list(out, json_out, &list, BACKUP_JOB_HEADERS, backup_job_row)?;
        }
        BackupJobCmd::Get { job } => print_json(
            out,
            &client.get(&format!("/backup-jobs/{}", seg(&job))).await?,
        )?,
        BackupJobCmd::Set(args) => {
            // If-Match 없이 보낸다: 스케줄러가 status를 자주 써서 revision이 곧잘 바뀐다.
            let path = format!("/backup-jobs/{}", seg(&args.job));
            let current = client.get(&path).await?;
            client
                .patch(&path, &backup_job_set_body(&args, &current))
                .await?;
            writeln!(out, "backup job {} updated", args.job)?;
        }
        BackupJobCmd::Delete { job } => {
            client
                .delete(&format!("/backup-jobs/{}", seg(&job)))
                .await?;
            writeln!(out, "backup job {job} deleted; its backups are kept")?;
        }
        BackupJobCmd::Run { job } => {
            client
                .send(client.request(Method::POST, &format!("/backup-jobs/{}/run", seg(&job))))
                .await?;
            writeln!(
                out,
                "backup job {job} will run at the scheduler's next check"
            )?;
        }
    }
    Ok(())
}

/// `backup-job list`의 열
const BACKUP_JOB_HEADERS: &[&str] = &[
    "NAME", "POOL", "VMS", "SCHEDULE", "KEEP", "ENABLED", "NEXT", "LAST",
];

/// `backup-job list`의 한 행. 시각은 모두 UTC다. 끈 작업은 다음 실행이 없다.
fn backup_job_row(j: &Value) -> Vec<String> {
    let spec = &j["spec"];
    let status = &j["status"];
    let vms = &spec["vms"];
    let vms = if vms["all"] == true {
        "all".to_string()
    } else if let Some(names) = vms["names"].as_array() {
        names.iter().map(cell).collect::<Vec<_>>().join(",")
    } else if let Some(labels) = vms["labels"].as_object() {
        labels
            .iter()
            .map(|(k, v)| format!("{k}={}", cell(v)))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "-".to_string()
    };
    let days = match spec["schedule"]["days"].as_array() {
        Some(days) if !days.is_empty() => days.iter().map(cell).collect::<Vec<_>>().join(","),
        _ => "daily".to_string(),
    };
    let schedule = format!("{days} {} UTC", cell(&spec["schedule"]["at"]));
    let keep: Vec<String> = ["last", "daily", "weekly", "monthly"]
        .iter()
        .filter_map(|unit| match spec["keep"][unit].as_u64() {
            Some(n) if n > 0 => Some(format!("{unit}={n}")),
            _ => None,
        })
        .collect();
    let keep = if keep.is_empty() {
        "all".to_string()
    } else {
        keep.join(",")
    };
    let enabled = spec["enabled"] != false;
    let next = match status["next_run"].as_i64() {
        Some(t) if enabled => unix_time(t),
        _ => "-".to_string(),
    };
    let last = if !status["run"].is_null() {
        "running".to_string()
    } else {
        let r = &status["last_run"];
        match r["finished_at"].as_i64() {
            Some(t) => format!(
                "{} ({} ok, {} failed)",
                unix_time(t),
                cell(&r["ok"]),
                cell(&r["failed"])
            ),
            None => "-".to_string(),
        }
    };
    vec![
        cell(&j["name"]),
        cell(&spec["pool"]),
        vms,
        schedule,
        keep,
        enabled.to_string(),
        next,
        last,
    ]
}

async fn import(
    client: &Client,
    cmd: ImportCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ImportCmd::Ova { file, name } => {
            // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
            let size = tokio::fs::metadata(&file)
                .await
                .with_context(|| format!("cannot read {}", file.display()))?
                .len()
                .to_string();
            client
                .send(
                    client
                        .request(Method::POST, "/vm-imports/ova/check")
                        .query(&[("name", name.as_str()), ("size", size.as_str())]),
                )
                .await?;
            let file_name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{name}.ova"));
            let query = [("name", name.as_str()), ("file_name", file_name.as_str())];
            client.upload("/vm-imports/ova", &query, &file).await?;
            writeln!(
                out,
                "vm-import {name} uploaded; review it with `oplctl import get {name}`"
            )?;
        }
        ImportCmd::EsxiBrowse { conn, fingerprint } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, fingerprint.as_deref(), &password);
            esxi_browse(client, &connection, json_out, out).await?;
        }
        ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, Some(&fingerprint), &password);
            let body = esxi_import_body(&name, &connection, &datastore, &vmx);
            esxi_create(client, &body, out).await?;
        }
        ImportCmd::List => {
            let list = client.get("/vm-imports").await?;
            let (nodes, vms) = if json_out {
                (Vec::new(), Vec::new())
            } else {
                (
                    items(&client.get("/nodes").await?),
                    items(&client.get("/vms").await?),
                )
            };
            print_list(out, json_out, &list, IMPORT_HEADERS, |i| {
                import_row(i, &nodes, &vms)
            })?;
        }
        ImportCmd::Get { import } => {
            let import = client.get(&format!("/vm-imports/{}", seg(&import))).await?;
            if json_out {
                print_json(out, &import)?;
            } else {
                let nodes = items(&client.get("/nodes").await?);
                let vms = if import["status"]["vm"].is_null() {
                    Vec::new()
                } else {
                    items(&client.get("/vms").await?)
                };
                print_import(out, &import, &nodes, &vms)?;
            }
        }
        ImportCmd::Run(args) => {
            // `=` 없는 --net이 있을 때만 원본 네트워크를 읽는다.
            let sources = if args.nets.iter().any(|n| n.source.is_none()) {
                source_networks(
                    &client
                        .get(&format!("/vm-imports/{}", seg(&args.import)))
                        .await?,
                )
            } else {
                Vec::new()
            };
            let body = import_run_body(&args, &sources)?;
            let task = client
                .post(&format!("/vm-imports/{}/run", seg(&args.import)), &body)
                .await?;
            client.wait_task_for(&task, IMPORT_WAIT).await?;
            writeln!(out, "vm {} imported", args.name)?;
        }
        ImportCmd::Delete { import, force } => {
            let mut r = client.request(Method::DELETE, &format!("/vm-imports/{}", seg(&import)));
            if force {
                r = r.query(&[("force", "true")]);
            }
            let got = client.send(r).await?;
            // force로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
            if let Some(message) = got["message"].as_str() {
                writeln!(out, "{message}")?;
            } else {
                client.wait_task(&got).await?;
                writeln!(out, "vm-import {import} deleted")?;
            }
        }
    }
    Ok(())
}

/// ESXi 연결 오류. 믿을 지문이 없으면 서버 인증서의 지문을 확인하고 다시 하라고 알린다.
fn esxi_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.code == "untrusted_certificate" => {
            let fingerprint = cell(&api.details["fingerprint"]);
            anyhow::anyhow!(
                "the ESXi certificate fingerprint is {fingerprint}; check it and rerun with --fingerprint {fingerprint}"
            )
        }
        _ => e,
    }
}

/// `import esxi-browse`: 데이터스토어마다 VM 표
async fn esxi_browse(
    client: &Client,
    connection: &Value,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let got = client
        .post("/vm-imports/esxi/browse", connection)
        .await
        .map_err(esxi_error)?;
    if json_out {
        return Ok(print_json(out, &got)?);
    }
    let mut rows = Vec::new();
    for ds in got["datastores"].as_array().into_iter().flatten() {
        for vm in ds["vms"].as_array().into_iter().flatten() {
            rows.push(esxi_vm_row(&ds["name"], vm));
        }
    }
    table(out, ESXI_VM_HEADERS, &rows)?;
    Ok(())
}

/// `import esxi-browse`의 열
const ESXI_VM_HEADERS: &[&str] = &[
    "DATASTORE",
    "VMX",
    "NAME",
    "GUEST",
    "CPU",
    "MEMORY",
    "DISKS",
    "LOCKED",
];

/// `import esxi-browse`의 한 행. `.vmx`를 읽지 못한 VM은 NAME 자리에 오류를 보인다.
fn esxi_vm_row(datastore: &Value, vm: &Value) -> Vec<String> {
    let (name, guest, cpu, memory, disks) = match vm["error"].as_str() {
        Some(error) => (
            format!("error: {error}"),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ),
        None => (
            cell(&vm["name"]),
            cell(&vm["guest_os"]),
            cell(&vm["vcpus"]),
            bytes(&vm["memory_mib"].as_u64().map(|m| m << 20).into()),
            cell(&vm["disks"]),
        ),
    };
    vec![
        cell(datastore),
        cell(&vm["vmx"]),
        name,
        guest,
        cpu,
        memory,
        disks,
        cell(&vm["locked"]),
    ]
}

/// `import esxi`: 가져오기를 만든다.
async fn esxi_create(
    client: &Client,
    body: &Value,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    client
        .post("/vm-imports/esxi", body)
        .await
        .map_err(esxi_error)?;
    writeln!(
        out,
        "vm-import {name} created; review it with `oplctl import get {name}`",
        name = cell(&body["name"])
    )?;
    Ok(())
}

/// ESXi 원본의 표시(`esxi://호스트/데이터스토어/.vmx`). OVA 원본이면 `None`이다.
fn esxi_source(i: &Value) -> Option<String> {
    let esxi = &i["spec"]["source"]["esxi"];
    esxi.is_object().then(|| {
        format!(
            "esxi://{}/{}/{}",
            cell(&esxi["host"]),
            cell(&esxi["datastore"]),
            cell(&esxi["vmx"])
        )
    })
}

/// 목록에서 id가 `id`인 항목의 이름. 없으면 id다.
fn name_of(list: &[Value], id: &Value) -> String {
    list.iter()
        .find(|v| v["id"] == *id)
        .map_or_else(|| cell(id), |v| cell(&v["name"]))
}

/// `import list`의 열
const IMPORT_HEADERS: &[&str] = &["NAME", "NODE", "SOURCE", "PHASE", "VM"];

/// `import list`의 한 행. SOURCE는 원본 VM 이름이고, 원본을 읽지 못했으면 올린 파일 이름이다.
/// ESXi 원본이면 `esxi://호스트/데이터스토어/.vmx`다.
fn import_row(i: &Value, nodes: &[Value], vms: &[Value]) -> Vec<String> {
    let status = &i["status"];
    let source = match (esxi_source(i), status["source_vm"]["name"].as_str()) {
        (Some(esxi), _) => esxi,
        (None, Some(name)) => name.to_string(),
        (None, None) => cell(&i["spec"]["source"]["ova"]["file_name"]),
    };
    vec![
        cell(&i["name"]),
        name_of(nodes, &i["spec"]["node"]),
        source,
        cell(&status["phase"]),
        name_of(vms, &status["vm"]),
    ]
}

/// `import get`: 머리 줄들, 원본 디스크와 NIC 표, 경고
fn print_import(
    out: &mut (dyn Write + Send),
    i: &Value,
    nodes: &[Value],
    vms: &[Value],
) -> anyhow::Result<()> {
    let ova = &i["spec"]["source"]["ova"];
    let status = &i["status"];
    writeln!(out, "vm-import: {}", cell(&i["name"]))?;
    writeln!(out, "node: {}", name_of(nodes, &i["spec"]["node"]))?;
    match esxi_source(i) {
        Some(esxi) => writeln!(out, "source: {esxi}")?,
        None => writeln!(
            out,
            "source: {} ({})",
            cell(&ova["file_name"]),
            bytes(&ova["size_bytes"])
        )?,
    }
    writeln!(out, "phase: {}", cell(&status["phase"]))?;
    if !status["vm"].is_null() {
        writeln!(out, "vm: {}", name_of(vms, &status["vm"]))?;
    }
    if let Some(error) = status["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let src = &status["source_vm"];
    if src.is_object() {
        writeln!(
            out,
            "source vm: {} ({}, {}, {} vCPUs, {} cores per socket, {} MiB)",
            cell(&src["name"]),
            cell(&src["guest_os"]),
            cell(&src["firmware"]),
            cell(&src["vcpus"]),
            cell(&src["cores_per_socket"]),
            cell(&src["memory_mib"])
        )?;
    }
    let p = &status["proposal"];
    if p.is_object() {
        writeln!(
            out,
            "proposal: {} sockets x {} cores, {} MiB, {}, {} disks, {} NICs",
            cell(&p["sockets"]),
            cell(&p["cores"]),
            cell(&p["memory_mib"]),
            cell(&p["firmware"]),
            cell(&p["disk_bus"]),
            cell(&p["nic_model"])
        )?;
    }
    if src.is_object() {
        let disks: Vec<_> = src["disks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                vec![
                    cell(&d["name"]),
                    cell(&d["file"]),
                    bytes(&d["capacity_bytes"]),
                    cell(&d["controller"]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["DISK", "FILE", "SIZE", "CONTROLLER"], &disks)?;
        // PROPOSED는 이름이 같아 제안한 opLite 네트워크다.
        let nics: Vec<_> = src["nics"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                vec![
                    cell(&n["network"]),
                    cell(&n["adapter"]),
                    cell(&n["mac"]),
                    cell(&p["networks"][n["network"].as_str().unwrap_or_default()]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["NETWORK", "ADAPTER", "MAC", "PROPOSED"], &nics)?;
    }
    if let Some(warnings) = status["warnings"].as_array().filter(|w| !w.is_empty()) {
        writeln!(out, "\nwarnings:")?;
        for w in warnings {
            writeln!(out, "  {}", cell(w))?;
        }
    }
    Ok(())
}

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            print_cluster_status(out, &view)?;
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{}", seg(&node))).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"],
                vm_row,
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{}", seg(&vm))).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{}", seg(key))).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args.nics.iter().map(nic_body).collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                    "ha": args.ha,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Set(args) => {
            put_spec_field(client, &args.vm, "ha", json!(args.ha)).await?;
            let state = if args.ha { "on" } else { "off" };
            writeln!(out, "vm {}: HA {state}", args.vm)?;
        }
        VmCmd::NicSg { vm, index, groups } => {
            put_spec(client, &vm, |current| {
                nic_groups_update(current, index, &groups)
            })
            .await?;
            if groups.is_empty() {
                writeln!(out, "vm {vm}: NIC {index} has no security groups")?;
            } else {
                writeln!(
                    out,
                    "vm {vm}: NIC {index} security groups {}",
                    groups.join(", ")
                )?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Migrate { vm, node } => {
            let task = client
                .post(
                    &format!("/vms/{}/migrate", seg(&vm)),
                    &json!({ "node": node }),
                )
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{}", seg(&vm))).await?;
                let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{}", seg(&vm))).await?;
                    let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{}/snapshots/{}", seg(&vm), seg(&cell(&s["id"])));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{}", seg(&vm))).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client
                .post(&format!("/vms/{}/clone", seg(&vm)), &body)
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{}", seg(&vm))).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// `task list`의 한 행. HA 복구면 OP에 죽은 노드를 붙인다(`vm-ha-recover (from A)`).
fn task_row(t: &Value) -> Vec<String> {
    let op = &t["spec"]["op"];
    let mut kind = cell(&op["kind"]);
    if op["kind"] == "vm-ha-recover" {
        kind = format!("{kind} (from {})", cell(&op["from"]));
    }
    vec![
        cell(&t["id"]),
        kind,
        cell(&t["spec"]["target"]),
        cell(&t["status"]["state"]),
        cell(&t["status"]["error"]),
    ]
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        // 옛 서버는 보내지 않는다 — HA VM이 아니다.
        (v["spec"]["ha"] == true).to_string(),
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client
                .post(&format!("/vms/{}/snapshots", seg(&vm)), &body)
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current =
                client.get(&format!("/vms/{}", seg(&vm))).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{}/snapshots/{}/revert", seg(&vm), seg(&name));
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{}/snapshots/{}", seg(&vm), seg(&name)))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", seg(&args.vm));
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다).
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    put_spec_field(
        client,
        vm,
        "snapshot_schedule",
        schedule.unwrap_or(Value::Null),
    )
    .await
}

/// VM `vm`의 spec 필드 `field`를 `value`로 바꾼다.
async fn put_spec_field(
    client: &Client,
    vm: &str,
    field: &str,
    value: Value,
) -> anyhow::Result<()> {
    put_spec(client, vm, |current| {
        Ok(spec_update(current, field, value.clone()))
    })
    .await
}

/// VM `vm`을 읽어 `body`(읽은 VM → 수정 본문)로 만든 본문을 그 revision으로(`If-Match`) 다시 쓴다.
async fn put_spec(
    client: &Client,
    vm: &str,
    body: impl Fn(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", seg(vm));
    let mut retries = SpecRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&body(&current)?);
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// VM 수정 PUT을 다시 할 횟수
#[derive(Default)]
struct SpecRetries {
    mismatch: u32,
    busy: u32,
}

impl SpecRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 spec 필드 `field`만 `value`로 바꾼다.
fn spec_update(vm: &Value, field: &str, value: Value) -> Value {
    let mut spec = vm["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// VM 수정 본문: NIC `index`(0부터)의 보안 그룹만 `groups`로 바꾼다.
fn nic_groups_update(vm: &Value, index: usize, groups: &[String]) -> anyhow::Result<Value> {
    let mut nics = vm["spec"]["nics"].as_array().cloned().unwrap_or_default();
    let count = nics.len();
    let Some(nic) = nics.get_mut(index) else {
        bail!(
            "VM {} has no NIC {index} ({count} NICs, numbered from 0)",
            cell(&vm["name"])
        );
    };
    nic["security_groups"] = json!(groups);
    Ok(spec_update(vm, "nics", Value::Array(nics)))
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(
            Method::POST,
            // 동작은 이 파일이 정한 값이고 `stop?mode=force`처럼 쿼리를 담으므로 인코딩하지 않는다.
            &format!("/vms/{}/actions/{action}", seg(vm)),
        ))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "VERSION",
    "UNDERLAY",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
    "FENCING",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        // 옛 버전 노드는 기록하지 않는다.
        cell(&n["status"]["version"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
        cell(&n["status"]["underlay"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
        // 펜싱이 꺼진 노드(옛 서버도)는 `ha`가 없다.
        cell(&n["status"]["ha"]["fencing"]),
    ]
}

/// `network list`의 열
const NETWORK_HEADERS: &[&str] = &["NAME", "TYPE", "BRIDGE", "VLAN/VNI", "MTU"];

/// VXLAN 네트워크의 기본 MTU(서버의 `DEFAULT_VXLAN_MTU`)
const DEFAULT_VXLAN_MTU: u64 = 1450;

/// `network list`의 한 행. VXLAN 네트워크의 브리지는 opld가 만드는 `oplv<vni>`다.
fn network_row(n: &Value) -> Vec<String> {
    let spec = &n["spec"];
    match spec["vxlan"]["vni"].as_u64() {
        Some(vni) => vec![
            cell(&n["name"]),
            "vxlan".into(),
            format!("oplv{vni}"),
            vni.to_string(),
            spec["mtu"]
                .as_u64()
                .unwrap_or(DEFAULT_VXLAN_MTU)
                .to_string(),
        ],
        None => vec![
            cell(&n["name"]),
            "bridge".into(),
            cell(&spec["bridge"]),
            cell(&spec["vlan"]),
            "-".into(),
        ],
    }
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
        .iter()
        .map(|m| {
            vec![
                cell(&m["name"]),
                cell(&m["id"]),
                cell(&m["address"]),
                if m["voter"] == true {
                    "voter"
                } else {
                    "learner"
                }
                .into(),
                cell(&m["online"]),
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
                cell(&m["fencing"]),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS", "FENCING",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
    // 버전을 모르는 멤버(옛 버전 노드, 합류 중)는 세지 않는다.
    let mut versions: Vec<&str> = members
        .iter()
        .filter_map(|m| m["version"].as_str())
        .collect();
    versions.sort_by(|a, b| opl_types::version::compare(a, b));
    versions.dedup_by(|a, b| opl_types::version::compare(a, b).is_eq());
    if versions.len() > 1 {
        writeln!(
            out,
            "warning: nodes run different opLite versions ({}); finish the upgrade on every node",
            versions.join(", ")
        )?;
    }
    Ok(())
}

/// 목록 응답의 항목들
fn items(list: &Value) -> Vec<Value> {
    list["items"].as_array().cloned().unwrap_or_default()
}

/// `pool list`의 열
const POOL_HEADERS: &[&str] = &["NAME", "KIND", "LOCATION", "NODES", "READY"];

/// `pool list`의 한 행. READY는 이 풀을 쓸 수 있는 노드(없는 노드 포함) 가운데 온라인이고 준비된
/// 노드 수다.
fn pool_row(pool: &Value, nodes: &[Value]) -> Vec<String> {
    let listed = pool["spec"]["nodes"].as_array().map_or(0, Vec::len);
    let usable = pool_nodes(pool, nodes);
    let ready = usable
        .iter()
        .filter(|n| n.is_ok_and(|n| node_online(n) && pool_state(pool, n)["ready"] == true))
        .count();
    vec![
        cell(&pool["name"]),
        cell(&pool["spec"]["backend"]["kind"]),
        pool_location(pool),
        if listed == 0 {
            "all".into()
        } else {
            listed.to_string()
        },
        format!("{ready}/{}", usable.len()),
    ]
}

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`, 공유 LVM 풀은 `vg`(iSCSI면
/// `vg @ portal target`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    if b["kind"] == "lvm-shared" {
        let iscsi = &b["iscsi"];
        if iscsi.is_object() {
            return format!(
                "{} @ {} {}",
                cell(&b["vg"]),
                cell(&iscsi["portal"]),
                cell(&iscsi["target"])
            );
        }
        return cell(&b["vg"]);
    }
    if b["kind"] == "rbd" {
        return match b["namespace"].as_str() {
            Some(namespace) => format!("{}/{namespace}", cell(&b["pool"])),
            None => cell(&b["pool"]),
        };
    }
    if let Some(vg) = b["vg"].as_str() {
        return format!("{vg}/{}", cell(&b["thinpool"]));
    }
    if let Some(dataset) = b["dataset"].as_str() {
        return dataset.into();
    }
    match b["server"].as_str() {
        Some(server) if server.contains(':') => format!("[{server}]:{}", cell(&b["export"])),
        Some(server) => format!("{server}:{}", cell(&b["export"])),
        None => cell(&b["path"]),
    }
}

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
    }
}

fn node_online(node: &Value) -> bool {
    node["status"]["online"] == true
}

/// 노드가 알린 풀 상태. 알리지 않았거나 같은 이름의 다른 풀(`pool_id`가 다름)의 상태면 `null`이다.
fn pool_state<'a>(pool: &Value, node: &'a Value) -> &'a Value {
    let name = pool["name"].as_str().unwrap_or_default();
    let state = &node["status"]["pools"][name];
    if state["pool_id"] == pool["id"] {
        state
    } else {
        &Value::Null
    }
}

/// `pool get`의 머리 줄들
fn describe_pool(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    let mut text = format!(
        "pool: {}\nkind: {}\nlocation: {}\n",
        cell(&pool["name"]),
        cell(&b["kind"]),
        pool_location(pool)
    );
    if let Some(options) = b["options"].as_str() {
        text.push_str(&format!("options: {options}\n"));
    }
    // RBD 풀의 Ceph 사용자·conf(생략하면 Ceph 기본이라 적지 않는다)
    for key in ["user", "conf"] {
        if let Some(value) = b[key].as_str() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!("shared: {}\n", cell(&pool["spec"]["shared"])));
    // 백업 풀만 적는다(옛 서버와 끈 풀은 보내지 않는다).
    if pool["spec"]["backups"] == true {
        text.push_str("backups: true\n");
    }
    text
}

/// `pool get`의 노드별 상태 열
const POOL_NODE_HEADERS: &[&str] = &["NODE", "READY", "SIZE", "AVAILABLE", "ERROR"];

/// 풀을 쓸 수 있는 노드마다 상태·용량·오류. 용량은 온라인이고 준비된 노드만 보인다.
fn pool_node_rows(pool: &Value, nodes: &[Value]) -> Vec<Vec<String>> {
    pool_nodes(pool, nodes)
        .into_iter()
        .map(|n| {
            let n = match n {
                Ok(n) => n,
                Err(id) => {
                    return vec![
                        cell(id),
                        "false".into(),
                        "-".into(),
                        "-".into(),
                        "node not found".into(),
                    ];
                }
            };
            let state = pool_state(pool, n);
            let online = node_online(n);
            let ready = online && state["ready"] == true;
            let size = |v: &Value| if ready { bytes(v) } else { "-".into() };
            let error = if !online {
                "node offline".into()
            } else if state.is_null() {
                "no status reported yet".into()
            } else {
                cell(&state["error"])
            };
            vec![
                cell(&n["name"]),
                ready.to_string(),
                size(&state["total_bytes"]),
                size(&state["available_bytes"]),
                error,
            ]
        })
        .collect()
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 이 CLI의 login이 만든 토큰(이름 `oplctl-…`)이면 지운다. 토큰 자신으로 읽고 지운다(실패는 무시).
/// 지웠으면 `Some(true)`, 다른 토큰(웹 UI에서 만든 것)이라 두었으면 `Some(false)`, 목록을 읽지 못했거나
/// 목록에 없으면(이미 지워짐) `None`.
async fn remove_login_token(client: &Client, id: uuid::Uuid) -> Option<bool> {
    let tokens = client.get("/tokens").await.ok()?;
    let token = tokens["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["id"].as_str() == Some(id.to_string().as_str()))?;
    let made_by_login = token["name"]
        .as_str()
        .is_some_and(|n| n.starts_with(LOGIN_TOKEN_PREFIX));
    if made_by_login {
        let _ = client.delete(&format!("/tokens/{id}")).await;
    }
    Some(made_by_login)
}

/// 웹 UI에서 만든 API 토큰으로 로그인한다: 그 토큰으로 `GET /auth/whoami`를 불러 확인하고 사용자 이름을
/// 얻는다. 토큰과 사용자 이름을 돌려준다.
async fn token_login(
    server: &str,
    file: &PathBuf,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let token = read_secret_file(file)?.trim().to_string();
    if token.is_empty() {
        bail!("the token file is empty");
    }
    let client = Client::new(server, Some(token.clone()), ca_pem)?;
    let me = client
        .get("/auth/whoami")
        .await
        .context("the API token was not accepted")?;
    let name = me["name"]
        .as_str()
        .context("server did not return the user")?
        .to_string();
    Ok((token, name))
}

/// OIDC 영역은 브라우저로만 로그인하므로 비밀번호를 묻기 전에 거절한다. 로그인 화면의
/// 영역 목록(`GET /auth/realms`)을 읽지 못하면 그냥 지나간다(서버가 로그인에서 판단한다).
async fn oidc_login_refused(anon: &Client, server: &str, realm: &str) -> anyhow::Result<()> {
    let resp = anon
        .http
        .get(anon.url("/auth/realms"))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    let list: Value = if resp.status().is_success() {
        resp.json().await.unwrap_or_default()
    } else {
        Value::Null
    };
    let oidc = list
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| r["name"] == realm && r["kind"] == "oidc");
    if oidc {
        bail!(
            "sign in to {realm} in the web UI, create an API token there and run `oplctl login --token-file`"
        );
    }
    Ok(())
}

/// `login`이 만드는 API 토큰 이름의 앞부분
const LOGIN_TOKEN_PREFIX: &str = "oplctl-";

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다. 토큰과 서버가 정한
/// 사용자 이름(영역 사용자는 `<사용자>@<영역>`)을 돌려준다.
async fn login(
    server: &str,
    args: &LoginArgs,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let anon = Client::new(server, None, ca_pem)?;
    if args.realm != opl_types::realm::LOCAL_REALM {
        oidc_login_refused(&anon, server, &args.realm).await?;
    }
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let mut body = json!({ "username": args.user, "password": password });
    if args.realm != opl_types::realm::LOCAL_REALM {
        body["realm"] = json!(args.realm);
    }
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let user: Value = resp.json().await.unwrap_or_default();
    let user = user["name"].as_str().unwrap_or(&args.user).to_string();
    let name = format!(
        "{LOGIN_TOKEN_PREFIX}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    let token = created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")?;
    Ok((token, user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", schedule.clone()),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", Value::Null),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
        // `vm set --ha`는 ha만 바꾼다.
        assert_eq!(
            spec_update(&vm, "ha", json!(true))["spec"],
            json!({ "memory_mib": 256, "snapshot_schedule": vm["spec"]["snapshot_schedule"], "ha": true })
        );
    }

    #[test]
    fn parses_vm_ha_commands() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web", "--ha"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if !c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Set(VmSet { vm, ha: true, no_ha: false }) } if vm == "web"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--no-ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Set(VmSet {
                    ha: false,
                    no_ha: true,
                    ..
                })
            }
        ));
        // 바꿀 것이 하나는 있어야 하고, 둘을 함께 줄 수 없다.
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha", "--no-ha"]).is_err());
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node } } if vm == "web" && node == "n2"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            [
                "web",
                "running",
                "migrating",
                "id-a -> id-b",
                "false",
                "42",
                "-"
            ]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "false", "42", "-"]
        );
        // HA VM
        vm["spec"]["ha"] = json!(true);
        assert_eq!(vm_row(&vm)[4], "true");
    }

    #[test]
    fn task_row_shows_where_ha_moved_the_vm_from() {
        let task = json!({
            "id": "t1",
            "spec": { "op": { "kind": "vm-ha-recover", "from": "id-a" }, "target": "vm-1", "node": "id-b" },
            "status": { "state": "succeeded", "error": null },
        });
        assert_eq!(
            task_row(&task),
            ["t1", "vm-ha-recover (from id-a)", "vm-1", "succeeded", "-"]
        );
        // 다른 종류는 이름만
        let mut task = task;
        task["spec"]["op"] = json!({ "kind": "vm-start" });
        assert_eq!(task_row(&task)[1], "vm-start");
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn spec_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = SpecRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = SpecRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
        // 실행 중인 VM의 HA 켜기는 서버의 409 문구를 그대로 보인다.
        assert_eq!(
            r.next("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped (vm_running)"),
            None
        );
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool.clone(), pool_set_body(&s), pool_set_summary(&s)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }), "shared".into())
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            (
                "nas".into(),
                json!({ "shared": false }),
                "node-local".into()
            )
        );
        // 백업 표시는 따로 바꾸거나 공유 여부와 함께 바꾼다. 주지 않은 값은 보내지 않는다.
        assert_eq!(
            set(&["--backups", "true"]).unwrap(),
            (
                "nas".into(),
                json!({ "backups": true }),
                "backups on".into()
            )
        );
        assert_eq!(
            set(&["--no-shared", "--backups", "false"]).unwrap().1,
            json!({ "shared": false, "backups": false })
        );
        assert_eq!(
            set(&["--shared", "--backups", "false"]).unwrap().2,
            "shared, backups off"
        );
        // 하나는 꼭, 공유와 노드 로컬은 함께 못 준다, 백업은 true나 false
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
        assert!(set(&["--backups"]).is_err());
        assert!(set(&["--backups", "yes"]).is_err());
    }

    #[test]
    fn pool_create_marks_backup_pools() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "bk"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            body(&["--path", "/srv/bk", "--backups"]).unwrap()["spec"],
            json!({ "backend": { "kind": "dir", "path": "/srv/bk" }, "nodes": [], "backups": true })
        );
        assert_eq!(
            body(&["--nfs", "nas:/bk", "--backups"]).unwrap()["spec"]["backups"],
            json!(true)
        );
        // 끈 풀은 보내지 않는다.
        assert!(body(&["--path", "/srv/bk"]).unwrap()["spec"]["backups"].is_null());
        // 블록 풀은 백업을 담지 못한다.
        for args in [
            &["--lvm-thin", "vg0/tp", "--backups"][..],
            &["--zfs", "tank/opl", "--backups"],
            &["--rbd", "rbd", "--backups"],
            &["--lvm-shared", "san", "--backups"],
        ] {
            assert!(body(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn describes_a_backup_pool() {
        let pool = json!({
            "name": "bk",
            "spec": { "backend": { "kind": "dir", "path": "/srv/bk" }, "backups": true }
        });
        assert_eq!(
            describe_pool(&pool),
            "pool: bk\nkind: dir\nlocation: /srv/bk\nshared: -\nbackups: true\n"
        );
    }

    #[test]
    fn parses_backup_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Backup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk"]).unwrap(),
            BackupCmd::Create { vm, pool, name: None } if vm == "web" && pool == "bk"
        ));
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk", "--name", "before-upgrade"]).unwrap(),
            BackupCmd::Create { name: Some(n), .. } if n == "before-upgrade"
        ));
        assert!(matches!(
            parse(&["list", "--vm", "web"]).unwrap(),
            BackupCmd::List { vm: Some(v) } if v == "web"
        ));
        assert!(matches!(
            parse(&["restore", "web-1", "--name", "web2", "--pool", "local", "--node", "n1"])
                .unwrap(),
            BackupCmd::Restore { backup, name, pool, node: Some(node) }
                if backup == "web-1" && name == "web2" && pool == "local" && node == "n1"
        ));
        // 백업 풀과 복원의 새 이름·풀은 꼭 준다.
        for args in [
            &["create", "web"][..],
            &["restore", "web-1", "--pool", "local"],
            &["restore", "web-1", "--name", "web2"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_list_shows_the_vm_name_and_stored_size() {
        let ready = json!({
            "name": "web-20261104-030000",
            "spec": { "vm": "x", "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "disks": [
                    { "name": "disk0", "stored_bytes": 3u64 << 30 },
                    { "name": "disk1", "stored_bytes": 1u64 << 29 }
                ]
            }
        });
        assert_eq!(
            backup_row(&ready),
            [
                "web-20261104-030000",
                "web",
                "bk",
                "ready",
                "2026-11-04 03:00 UTC",
                "3.5GiB"
            ]
        );
        // 만드는 중이거나 실패한 백업은 크기가 없다.
        let pending = json!({
            "name": "b", "spec": { "vm_name": "web", "pool": "bk" },
            "status": { "phase": "pending", "created_at": null, "disks": [] }
        });
        assert_eq!(backup_row(&pending)[3..], ["pending", "-", "-"]);
    }

    /// `backup-job create`의 인자를 요청 본문으로
    fn backup_job_create(args: &[&str]) -> Result<Value, clap::Error> {
        let mut argv = vec!["oplctl", "backup-job", "create", "nightly"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::BackupJob {
                cmd: BackupJobCmd::Create(c),
            } => backup_job_create_body(&c),
            _ => unreachable!(),
        })
    }

    #[test]
    fn backup_job_create_builds_the_spec() {
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--label",
                "tier=prod",
                "--label",
                "env=a=b",
                "--at",
                "03:00",
                "--days",
                "mon,wed",
                "--keep-last",
                "2",
                "--keep-weekly",
                "4",
            ])
            .unwrap(),
            json!({
                "name": "nightly",
                "spec": {
                    "vms": { "labels": { "tier": "prod", "env": "a=b" } },
                    "pool": "bk",
                    "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                    "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                    "enabled": true
                }
            })
        );
        // 요일을 주지 않으면 매일, 보존 규칙을 주지 않으면 모두 0(지우지 않음)
        let all =
            backup_job_create(&["--pool", "bk", "--all", "--at", "00:00", "--disabled"]).unwrap();
        assert_eq!(all["spec"]["vms"], json!({ "all": true }));
        assert_eq!(
            all["spec"]["schedule"],
            json!({ "at": "00:00", "days": [] })
        );
        assert_eq!(
            all["spec"]["keep"],
            json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 })
        );
        assert_eq!(all["spec"]["enabled"], json!(false));
        assert_eq!(
            backup_job_create(&["--pool", "bk", "--vm", "web", "--vm", "db", "--at", "03:00"])
                .unwrap()["spec"]["vms"],
            json!({ "names": ["web", "db"] })
        );
        // VM 고르기는 셋 중 꼭 하나, 풀과 시각은 꼭, 라벨은 KEY=VALUE, 요일은 mon..sun
        for args in [
            &["--pool", "bk", "--at", "03:00"][..],
            &["--pool", "bk", "--all", "--vm", "web", "--at", "03:00"],
            &[
                "--pool", "bk", "--vm", "web", "--label", "a=b", "--at", "03:00",
            ],
            &["--all", "--at", "03:00"],
            &["--pool", "bk", "--all"],
            &["--pool", "bk", "--label", "tier", "--at", "03:00"],
            &["--pool", "bk", "--label", "=prod", "--at", "03:00"],
            &["--pool", "bk", "--all", "--at", "03:00", "--days", "monday"],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--keep-last",
                "-1",
            ],
        ] {
            assert!(backup_job_create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_set_changes_only_what_is_given() {
        let current = json!({
            "name": "nightly",
            "revision": 9,
            "spec": {
                "vms": { "all": true }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["sat"] },
                "keep": { "last": 2, "daily": 7, "weekly": 0, "monthly": 0 },
                "enabled": true
            }
        });
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup-job", "set", "nightly"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::BackupJob {
                    cmd: BackupJobCmd::Set(s),
                } => backup_job_set_body(&s, &current),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--enabled", "false"]).unwrap(),
            json!({ "enabled": false })
        );
        assert_eq!(
            set(&["--vm", "web", "--pool", "bk2"]).unwrap(),
            json!({ "vms": { "names": ["web"] }, "pool": "bk2" })
        );
        // 일정과 보존 규칙은 통째로 바뀌므로 주지 않은 부분은 지금 값을 보낸다.
        assert_eq!(
            set(&["--at", "04:30"]).unwrap(),
            json!({ "schedule": { "at": "04:30", "days": ["sat"] } })
        );
        assert_eq!(
            set(&["--days", "sun,mon"]).unwrap(),
            json!({ "schedule": { "at": "03:00", "days": ["sun", "mon"] } })
        );
        assert_eq!(
            set(&["--keep-monthly", "6", "--keep-last", "1"]).unwrap(),
            json!({ "keep": { "last": 1, "daily": 7, "weekly": 0, "monthly": 6 } })
        );
        // 바꿀 것은 꼭 하나, VM 고르기는 하나만, 사용 여부는 true나 false
        for args in [
            &[][..],
            &["--all", "--vm", "web"],
            &["--enabled"],
            &["--enabled", "yes"],
        ] {
            assert!(set(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_list_shows_the_schedule_and_runs() {
        let job = json!({
            "name": "nightly",
            "spec": {
                "vms": { "labels": { "env": "a", "tier": "prod" } }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                "enabled": true
            },
            "status": {
                "next_run": 1_793_761_200, "run": null,
                "last_run": { "started_at": 1_793_674_800, "finished_at": 1_793_675_100, "ok": 3, "failed": 1 },
                "errors": [{ "vm": "db", "error": "boom" }]
            }
        });
        assert_eq!(
            backup_job_row(&job),
            [
                "nightly",
                "bk",
                "env=a,tier=prod",
                "mon,wed 03:00 UTC",
                "last=2,weekly=4",
                "true",
                "2026-11-04 03:00 UTC",
                "2026-11-03 03:05 UTC (3 ok, 1 failed)"
            ]
        );
        // 매일, 보존 규칙 없음, 진행 중
        let mut running = job.clone();
        running["spec"]["vms"] = json!({ "names": ["web", "db"] });
        running["spec"]["schedule"]["days"] = json!([]);
        running["spec"]["keep"] = json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 });
        running["status"]["run"] = json!({ "started_at": 1, "queue": [], "backups": [] });
        assert_eq!(
            backup_job_row(&running)[2..],
            [
                "web,db",
                "daily 03:00 UTC",
                "all",
                "true",
                "2026-11-04 03:00 UTC",
                "running"
            ]
        );
        // 끈 작업은 다음 실행이 없고, 한 번도 돌지 않았으면 마지막 실행도 없다.
        let mut off = job.clone();
        off["spec"]["vms"] = json!({ "all": true });
        off["spec"]["enabled"] = json!(false);
        off["status"]["last_run"] = Value::Null;
        assert_eq!(backup_job_row(&off)[2], "all");
        assert_eq!(backup_job_row(&off)[5..], ["false", "-", "-"]);
    }

    #[test]
    fn parses_pool_create_commands() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.path, c.nfs, c.nfs_options, c.shared),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None, false)
        );
        // 디렉터리 풀은 공유 스토리지(밖에서 마운트한 공유 디렉터리)라고 적을 수 있다.
        assert!(create(&["--path", "/mnt/shared", "--shared"]).unwrap().3);
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into()),
                false
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로, NFS 풀은 늘 공유라 --shared가 없다
        for args in [
            &[][..],
            &["--nfs", "nas:/srv/vm", "--shared"],
            &["--shared"],
            &["--path", "/srv/pool", "--nfs", "nas:/srv/vm"],
            &["--path", "/srv/pool", "--nfs-options", "hard"],
            &["--nfs", "nas"],
            &["--nfs", "nas:srv"],
            &["--nfs", ":/srv"],
            &["--nfs", "fd00::1:/srv"],
            &["--nfs", "[fd00::1/srv"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 브리지와 VXLAN 중 정확히 하나. VLAN은 브리지와, MTU는 VXLAN과만. 주지 않은 값은 보내지 않는다.
    #[test]
    fn parses_network_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "network", "create", "lan"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Network {
                    cmd: NetworkCmd::Create(c),
                } => network_create_body(&c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--bridge", "vmbr0"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } })
        );
        assert_eq!(
            create(&["--bridge", "vmbr0", "--vlan", "10"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0", "vlan": 10 } })
        );
        assert_eq!(
            create(&["--vxlan", "100"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 } } })
        );
        assert_eq!(
            create(&["--vxlan", "100", "--mtu", "1400"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 }, "mtu": 1400 } })
        );
        for args in [
            &[][..],
            &["--bridge", "vmbr0", "--vxlan", "100"],
            &["--vxlan", "100", "--vlan", "10"],
            &["--bridge", "vmbr0", "--mtu", "1400"],
            &["--vlan", "10"],
            &["--mtu", "1400"],
            &["--vxlan", "-1"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 종류, 붙는 브리지(VXLAN은 관리 이름), VLAN이나 VNI, VXLAN MTU(없으면 기본값)
    #[test]
    fn network_list_shows_kind_bridge_and_mtu() {
        let rows: Vec<_> = [
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
            json!({ "name": "dmz", "spec": { "bridge": "vmbr0", "vlan": 20 } }),
            json!({ "name": "ov", "spec": { "vxlan": { "vni": 100 } } }),
            json!({ "name": "jumbo", "spec": { "vxlan": { "vni": 7 }, "mtu": 8950 } }),
        ]
        .iter()
        .map(network_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, NETWORK_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   TYPE    BRIDGE   VLAN/VNI  MTU\n\
             lan    bridge  vmbr0    -         -\n\
             dmz    bridge  vmbr0    20        -\n\
             ov     vxlan   oplv100  100       1450\n\
             jumbo  vxlan   oplv7    7         8950\n"
        );
    }

    #[test]
    fn parses_lvm_thin_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.lvm_thin,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-thin", "vg0/tp"]).unwrap(),
            Some(LvmThinSource {
                vg: "vg0".into(),
                thinpool: "tp".into()
            })
        );
        // 다른 종류·--shared와 함께 못 쓰고, VG와 thin pool이 모두 있어야 한다
        for args in [
            &["--lvm-thin", "vg0/tp", "--path", "/srv/pool"][..],
            &["--lvm-thin", "vg0/tp", "--nfs", "nas:/srv/vm"],
            &["--lvm-thin", "vg0/tp", "--shared"],
            &["--lvm-thin", "vg0/tp", "--nfs-options", "hard"],
            &["--lvm-thin", "vg0"],
            &["--lvm-thin", "/tp"],
            &["--lvm-thin", "vg0/"],
            &["--lvm-thin", "vg0/tp/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd, --lvm-shared) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in [
            "--path",
            "--nfs",
            "--lvm-thin",
            "--zfs",
            "--rbd",
            "--lvm-shared",
        ] {
            assert!(msg.contains(kind), "{msg}");
        }
        assert!(create(&["--node", "a"]).is_err());
        assert!(create(&["--path", "/srv/p"]).is_ok());
        assert!(create(&["--nfs", "nas:/srv/vm"]).is_ok());
    }

    #[test]
    fn parses_zfs_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.zfs,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--zfs", "tank/opl"]).unwrap(),
            Some("tank/opl".to_string())
        );
        assert_eq!(
            create(&["--zfs", "tank"]).unwrap(),
            Some("tank".to_string())
        );
        // 다른 종류·--shared와 함께 못 쓴다. 데이터셋 이름의 검사는 API가 한다.
        for args in [
            &["--zfs", "tank/opl", "--path", "/srv/pool"][..],
            &["--zfs", "tank/opl", "--nfs", "nas:/srv/vm"],
            &["--zfs", "tank/opl", "--nfs-options", "hard"],
            &["--zfs", "tank/opl", "--lvm-thin", "vg0/tp"],
            &["--zfs", "tank/opl", "--shared"],
            &["--zfs"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_rbd_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.rbd, c.rbd_user, c.rbd_conf),
                _ => unreachable!(),
            })
        };
        let rbd = |pool: &str, namespace: Option<&str>| RbdSource {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
        };
        assert_eq!(
            create(&["--rbd", "rbd"]).unwrap(),
            (Some(rbd("rbd", None)), None, None)
        );
        assert_eq!(
            create(&[
                "--rbd",
                "rbd/opl",
                "--rbd-user",
                "opl",
                "--rbd-conf",
                "/etc/ceph/opl.conf"
            ])
            .unwrap(),
            (
                Some(rbd("rbd", Some("opl"))),
                Some("opl".into()),
                Some("/etc/ceph/opl.conf".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 사용자·conf는 --rbd와 함께만 쓴다.
        // 이름 규칙의 나머지 검사는 API가 한다.
        for args in [
            &["--rbd", "rbd", "--path", "/srv/pool"][..],
            &["--rbd", "rbd", "--nfs", "nas:/srv/vm"],
            &["--rbd", "rbd", "--nfs-options", "hard"],
            &["--rbd", "rbd", "--lvm-thin", "vg0/tp"],
            &["--rbd", "rbd", "--zfs", "tank/opl"],
            &["--rbd", "rbd", "--shared"],
            &["--path", "/srv/pool", "--rbd-user", "opl"],
            &["--path", "/srv/pool", "--rbd-conf", "/etc/ceph/ceph.conf"],
            &["--rbd"],
            &["--rbd", ""],
            &["--rbd", "/opl"],
            &["--rbd", "rbd/"],
            &["--rbd", "rbd/opl/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// `pool create`가 보내는 요청 본문. RBD 풀은 네임스페이스·사용자·conf를 담고 `shared`는 적지
    /// 않는다.
    #[test]
    fn pool_create_sends_the_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        let args = [
            "--rbd",
            "rbd/opl",
            "--rbd-user",
            "opl",
            "--rbd-conf",
            "/etc/ceph/x.conf",
        ];
        assert_eq!(
            body(&args),
            json!({
                "name": "ceph",
                "spec": {
                    "backend": {
                        "kind": "rbd", "pool": "rbd", "namespace": "opl",
                        "user": "opl", "conf": "/etc/ceph/x.conf"
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--rbd", "rbd", "--node", "n1"])["spec"],
            json!({
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": null, "user": null, "conf": null
                },
                "nodes": ["n1"]
            })
        );
    }

    #[test]
    fn parses_lvm_shared_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.lvm_shared, c.iscsi_portal, c.iscsi_target),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-shared", "san"]).unwrap(),
            (Some("san".into()), None, None)
        );
        assert_eq!(
            create(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "10.0.0.5:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ])
            .unwrap(),
            (
                Some("san".into()),
                Some("10.0.0.5:3260".into()),
                Some("iqn.2003-01.org.example:san".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 포털·타깃은 --lvm-shared와 함께, 둘이 함께
        // 쓴다. 이름 규칙의 검사는 API가 한다.
        for args in [
            &["--lvm-shared", "san", "--path", "/srv/pool"][..],
            &["--lvm-shared", "san", "--nfs", "nas:/srv/vm"],
            &["--lvm-shared", "san", "--nfs-options", "hard"],
            &["--lvm-shared", "san", "--lvm-thin", "vg0/tp"],
            &["--lvm-shared", "san", "--zfs", "tank/opl"],
            &["--lvm-shared", "san", "--rbd", "rbd"],
            &["--lvm-shared", "san", "--rbd-user", "opl"],
            &["--lvm-shared", "san", "--shared"],
            &["--lvm-shared", "san", "--iscsi-portal", "nas"],
            &[
                "--lvm-shared",
                "san",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &[
                "--path",
                "/srv/pool",
                "--iscsi-portal",
                "nas",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &["--lvm-shared"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 공유 LVM 풀은 VG와 iSCSI 타깃(없으면 null)을 담고 `shared`는 적지 않는다.
    #[test]
    fn pool_create_sends_the_lvm_shared_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            body(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "[fd00::5]:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ]),
            json!({
                "name": "san",
                "spec": {
                    "backend": {
                        "kind": "lvm-shared", "vg": "san",
                        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.example:san" }
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--lvm-shared", "san", "--node", "n1"])["spec"],
            json!({
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": ["n1"]
            })
        );
    }

    /// 받은 요청 줄을 `seen`에 남기고 경로에 맞는 JSON을 200으로 돌려주는 작은 HTTP 서버
    async fn fake_api(
        reply: fn(&str) -> Value,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        fake_api_status(move |path| (200, reply(path))).await
    }

    /// `fake_api`와 같고 응답 상태 코드도 정한다.
    async fn fake_api_status(
        reply: impl Fn(&str) -> (u16, Value) + Send + 'static,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(line);
                let (status, body) = reply(&path);
                let body = body.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        (client, seen)
    }

    /// `backup delete --force`는 `?force=true`로 보내고, 기록만 지웠으면 서버의 알림을 보인다.
    /// 아니면 Task를 기다린다.
    #[tokio::test]
    async fn backup_delete_force_shows_the_server_message() {
        let cli = Cli::try_parse_from(["oplctl", "backup", "delete", "b1", "--force"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Backup {
                cmd: BackupCmd::Delete { force: true, .. }
            }
        ));

        let (client, seen) =
            fake_api(|_| json!({ "message": "backup \"b1\" deleted; its files are left" })).await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", true, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/backups/b1?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup \"b1\" deleted; its files are left\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/backups/b1 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "backup b1 deleted\n");
    }

    /// `backup-job set`은 작업을 읽어 그 값에 맞춰 PATCH하고, `run`은 본문 없이 POST한다.
    #[tokio::test]
    async fn backup_job_set_reads_then_patches_and_run_posts() {
        let (client, seen) = fake_api(|_| {
            json!({
                "name": "nightly", "revision": 3,
                "spec": { "schedule": { "at": "03:00", "days": [] }, "keep": {} },
                "status": {}
            })
        })
        .await;
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "set", "nightly", "--at", "04:00"])
            .unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        let mut out = Vec::new();
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "run", "nightly"]).unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/backup-jobs/nightly HTTP/1.1",
                "PATCH /api/v1/backup-jobs/nightly HTTP/1.1",
                "POST /api/v1/backup-jobs/nightly/run HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup job nightly updated\nbackup job nightly will run at the scheduler's next check\n"
        );
    }

    /// 풀을 읽지 못하면 --format을 주라고 알린다. 주면 풀을 읽지 않는다.
    #[tokio::test]
    async fn volume_format_explains_an_unreadable_pool() {
        // 아무도 듣지 않는 포트
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        let err = volume_format(&client, "fast", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.starts_with(
                "cannot read storage pool \"fast\" to choose a volume format; pass --format: "
            ),
            "{msg}"
        );
        let format = volume_format(&client, "fast", Some(Format::Raw)).await;
        assert_eq!(format.unwrap(), Format::Raw);
    }

    #[test]
    fn volume_format_defaults_to_pool_kind() {
        let pool = |kind: &str| json!({ "spec": { "backend": { "kind": kind } } });
        assert_eq!(pool_default_format(&pool("lvm-thin")), Format::Raw);
        assert_eq!(pool_default_format(&pool("zfs")), Format::Raw);
        assert_eq!(pool_default_format(&pool("rbd")), Format::Raw);
        assert_eq!(pool_default_format(&pool("lvm-shared")), Format::Raw);
        assert_eq!(pool_default_format(&pool("dir")), Format::Qcow2);
        assert_eq!(pool_default_format(&pool("nfs")), Format::Qcow2);
        // --format를 주지 않으면 풀을 읽어 정하고, 주면 그대로 쓴다.
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "create", "root", "--pool", "fast", "--size", "1G",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Create { format: None, .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "import", "root", "--pool", "fast", "--file", "a.vmdk", "--format",
            "raw",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Import {
                    format: Some(Format::Raw),
                    ..
                }
            }
        ));
    }

    #[test]
    fn pool_list_and_details_show_each_node() {
        let node = |name: &str, online: bool, pools: Value| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "status": { "online": online, "pools": pools },
            })
        };
        let gib = 1u64 << 30;
        let state = |ready: bool, error: Value, total: u64, available: u64| json!({ "pool_id": "id-nas", "ready": ready, "error": error, "total_bytes": total, "available_bytes": available });
        let nodes = [
            node(
                "a",
                true,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            node(
                "b",
                true,
                json!({ "nas": state(false, json!("mount failed"), 0, 0) }),
            ),
            node("c", true, json!({})),
            // 준비됐다고 알렸지만 꺼진 노드는 준비로 세지 않는다.
            node(
                "d",
                false,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
            node(
                "e",
                true,
                json!({ "nas": { "pool_id": "id-old", "ready": true, "error": null } }),
            ),
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
        ];
        let nas = json!({
            "id": "id-nas",
            "name": "nas",
            "spec": {
                "backend": { "kind": "nfs", "server": "fd00::1", "export": "/srv/vm", "options": "hard" },
                "nodes": [],
                "shared": true,
            },
        });
        // 노드 목록의 노드를 그 순서대로 본다. 없는 노드도 센다.
        let local = json!({
            "id": "id-local",
            "name": "local",
            "spec": {
                "backend": { "kind": "dir", "path": "/srv/pool" },
                "nodes": ["id-gone", "id-b"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&nas, &nodes),
            ["nas", "nfs", "[fd00::1]:/srv/vm", "all", "1/5"]
        );
        assert_eq!(
            pool_row(&local, &nodes),
            ["local", "dir", "/srv/pool", "2", "0/2"]
        );

        let mut out = describe_pool(&nas).into_bytes();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&nas, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "pool: nas\n\
             kind: nfs\n\
             location: [fd00::1]:/srv/vm\n\
             options: hard\n\
             shared: true\n\
             NODE  READY  SIZE      AVAILABLE  ERROR\n\
             a     true   100.0GiB  40.0GiB    -\n\
             b     false  -         -          mount failed\n\
             c     false  -         -          no status reported yet\n\
             d     false  -         -          node offline\n\
             e     false  -         -          no status reported yet\n"
        );
        assert_eq!(
            describe_pool(&local),
            "pool: local\nkind: dir\nlocation: /srv/pool\nshared: false\n"
        );
        let fast = json!({
            "id": "id-fast",
            "name": "fast",
            "spec": {
                "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&fast, &nodes),
            ["fast", "lvm-thin", "vg0/tp", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&fast),
            "pool: fast\nkind: lvm-thin\nlocation: vg0/tp\nshared: false\n"
        );
        let tank = json!({
            "id": "id-tank",
            "name": "tank",
            "spec": {
                "backend": { "kind": "zfs", "dataset": "tank/opl" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&tank, &nodes),
            ["tank", "zfs", "tank/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&tank),
            "pool: tank\nkind: zfs\nlocation: tank/opl\nshared: false\n"
        );
        // RBD 풀의 위치는 `POOL[/NAMESPACE]`, 상세는 사용자·conf가 있으면 보인다.
        let ceph = json!({
            "id": "id-ceph",
            "name": "ceph",
            "spec": {
                "backend": { "kind": "rbd", "pool": "rbd" },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&ceph, &nodes),
            ["ceph", "rbd", "rbd", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&ceph),
            "pool: ceph\nkind: rbd\nlocation: rbd\nshared: true\n"
        );
        let opl = json!({
            "id": "id-opl",
            "name": "opl",
            "spec": {
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                    "conf": "/etc/ceph/opl.conf"
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&opl, &nodes),
            ["opl", "rbd", "rbd/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&opl),
            "pool: opl\nkind: rbd\nlocation: rbd/opl\nuser: opl\nconf: /etc/ceph/opl.conf\n\
             shared: true\n"
        );
        // 공유 LVM 풀의 위치는 `VG`, iSCSI면 `VG @ PORTAL TARGET`
        let fc = json!({
            "id": "id-fc",
            "name": "fc",
            "spec": {
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&fc, &nodes),
            ["fc", "lvm-shared", "san", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&fc),
            "pool: fc\nkind: lvm-shared\nlocation: san\nshared: true\n"
        );
        let san = json!({
            "id": "id-san",
            "name": "san",
            "spec": {
                "backend": {
                    "kind": "lvm-shared", "vg": "san",
                    "iscsi": { "portal": "10.0.0.5:3260", "target": "iqn.2003-01.org.example:san" }
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&san, &nodes),
            [
                "san",
                "lvm-shared",
                "san @ 10.0.0.5:3260 iqn.2003-01.org.example:san",
                "1",
                "0/1"
            ]
        );
        assert_eq!(
            describe_pool(&san),
            "pool: san\nkind: lvm-shared\nlocation: san @ 10.0.0.5:3260 \
             iqn.2003-01.org.example:san\nshared: true\n"
        );
        let mut out = Vec::new();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&local, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE     READY  SIZE  AVAILABLE  ERROR\n\
             id-gone  false  -     -          node not found\n\
             b        false  -     -          no status reported yet\n"
        );
    }

    #[test]
    fn node_list_shows_witnesses_and_fencing() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let mut a = node("a", Some(false));
        a["status"]["ha"] = json!({ "fencing": "watchdog", "recover_after_secs": 65 });
        a["status"]["underlay"] = json!("10.0.0.1");
        a["status"]["version"] = json!("0.2.0");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
            .iter()
            .map(node_row)
            .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다. 펜싱이 꺼진 노드는 `ha`가 없다.
        // underlay가 없는 노드(위트니스, 옛 서버)는 `-`다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        VERSION  UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  0.2.0    10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -        -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -        -         true    4     1.0GiB  false        false    -\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다. 펜싱이 꺼졌으면(null) 또는 옛
        // 서버라 `fencing`이 없으면 `-`다.
        let mut a = member("a", true, false);
        a["fencing"] = json!("process");
        let mut b = member("b", true, false);
        b["fencing"] = Value::Null;
        assert_eq!(
            status(vec![a, b, member("w", true, true)]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS  FENCING\n\
             a     id-a  a:8006   voter  true    true    false    process\n\
             b     id-b  b:8006   voter  true    false   false    -\n\
             w     id-w  w:8006   voter  true    false   true     -\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 버전을 아는 멤버의 버전이 둘 이상이면 낮은 것부터 한 번씩 적어 경고한다. 모르는 멤버는 세지 않는다.
    #[test]
    fn cluster_status_warns_about_mixed_versions() {
        let member = |name: &str, version: Option<&str>| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": true,
                "online": true,
                "witness": false,
                "version": version,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.9.0")),
            member("c", Some("0.10.0")),
            member("d", Some("0.10.0~rc1")),
            member("e", None),
        ]);
        assert!(
            out.ends_with(
                "warning: nodes run different opLite versions (0.9.0, 0.10.0~rc1, 0.10.0); \
                 finish the upgrade on every node\n"
            ),
            "{out}"
        );
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.10.0")),
            member("c", None),
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 방향과 동작은 꼭, 프로토콜·포트·CIDR은 이 차례로 생략할 수 있다.
    #[test]
    fn parses_rules() {
        assert_eq!(
            parse_rule("in accept tcp 22 10.0.0.0/8"),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "tcp",
                "ports": "22", "remote": "10.0.0.0/8"
            }))
        );
        assert_eq!(
            parse_rule("out drop tcp 25"),
            Ok(json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }))
        );
        assert_eq!(
            parse_rule("in accept icmp"),
            Ok(json!({ "direction": "in", "action": "accept", "protocol": "icmp" }))
        );
        assert_eq!(
            parse_rule("  in   accept udp 8000-8080 "),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "udp", "ports": "8000-8080"
            }))
        );
        assert_eq!(
            parse_rule("out accept 2001:db8::/32"),
            Ok(json!({ "direction": "out", "action": "accept", "remote": "2001:db8::/32" }))
        );
        assert_eq!(
            parse_rule("in drop"),
            Ok(json!({ "direction": "in", "action": "drop" }))
        );
        // 포트 범위와 프로토콜·포트의 짝은 서버가 본다.
        assert_eq!(
            parse_rule("in accept 99999"),
            Ok(json!({ "direction": "in", "action": "accept", "ports": "99999" }))
        );
        for bad in [
            "",
            "in",
            "accept in",
            "up accept",
            "in allow",
            "in accept sctp",
            "in accept tcp 22 10.0.0.0/8 extra",
            "in accept 10.0.0.0/8 tcp",
            "in accept tcp 10.0.0.0/8 22",
            "in accept tcp ssh",
            "in accept tcp 22-",
            "in accept tcp -22",
            "IN accept",
        ] {
            let err = parse_rule(bad).unwrap_err();
            assert!(err.contains("in|out accept|drop"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parses_security_group_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "security-group"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::SecurityGroup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&[
            "create",
            "web",
            "--description",
            "web servers",
            "--rule",
            "in accept tcp 443",
            "--rule",
            "in accept icmp",
        ])
        .unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [
                    { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" },
                    { "direction": "in", "action": "accept", "protocol": "icmp" },
                ]
            }})
        );
        // 설명이 없으면 보내지 않는다. 규칙 없는 그룹도 된다.
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&["create", "empty"]).unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "empty", "spec": { "rules": [] } })
        );
        assert!(parse(&["create", "web", "--rule", "in maybe"]).is_err());
        assert!(matches!(
            parse(&["rule", "add", "web", "out drop tcp 25", "--at", "2"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Add { group, rule, at: Some(2) } }
                if group == "web" && rule["ports"] == "25"
        ));
        assert!(matches!(
            parse(&["rule", "rm", "web", "3"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Rm { group, n: 3 } } if group == "web"
        ));
        assert!(parse(&["rule", "add", "web", "sideways accept"]).is_err());
        assert!(parse(&["rule", "rm", "web", "first"]).is_err());
        assert!(matches!(
            parse(&["get", "web"]).unwrap(),
            SecurityGroupCmd::Get { group } if group == "web"
        ));
        assert!(matches!(
            parse(&["delete", "web"]).unwrap(),
            SecurityGroupCmd::Delete { group } if group == "web"
        ));
    }

    /// 규칙 번호는 1부터다. 범위 밖이면 아무것도 바꾸지 않고 실패한다.
    #[test]
    fn adds_and_removes_rules_by_number() {
        let rule =
            |n: u16| json!({ "direction": "in", "action": "accept", "ports": n.to_string() });
        let group = json!({ "name": "web", "spec": { "rules": [rule(1), rule(2)] } });
        assert_eq!(
            rules_with(&group, rule(9), None).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(1)).unwrap(),
            vec![rule(9), rule(1), rule(2)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(3)).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        for at in [0, 4] {
            let err = rules_with(&group, rule(9), Some(at))
                .unwrap_err()
                .to_string();
            assert!(err.contains("use 1 to 3"), "{err}");
        }
        assert_eq!(rules_without(&group, 1).unwrap(), vec![rule(2)]);
        assert_eq!(rules_without(&group, 2).unwrap(), vec![rule(1)]);
        for n in [0, 3] {
            let err = rules_without(&group, n).unwrap_err().to_string();
            assert!(err.contains(&format!("has no rule {n}")), "{err}");
        }
        // 규칙이 없는 그룹은 응답에 rules가 비어 있다.
        let empty = json!({ "name": "e", "spec": { "rules": [] } });
        assert_eq!(rules_with(&empty, rule(1), None).unwrap(), vec![rule(1)]);
        assert!(rules_without(&empty, 1).is_err());
    }

    #[test]
    fn security_group_list_shows_rule_counts() {
        let rows: Vec<_> = [
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [{ "direction": "in", "action": "accept" }, { "direction": "out", "action": "drop" }]
            }}),
            json!({ "name": "empty", "spec": { "rules": [] } }),
        ]
        .iter()
        .map(security_group_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, SECURITY_GROUP_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   RULES  DESCRIPTION\n\
             web    2      web servers\n\
             empty  0      -\n"
        );
    }

    /// `--nic NETWORK[,sg=A+B]`. 그룹이 없으면 `security_groups`를 보내지 않는다.
    #[test]
    fn parses_nics_with_security_groups() {
        let nics = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "create", "web"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Vm {
                    cmd: VmCmd::Create(c),
                } => c.nics.iter().map(nic_body).collect::<Vec<_>>(),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            nics(&[
                "--nic",
                "user",
                "--nic",
                "lan,sg=web+db",
                "--nic",
                "dmz,sg=web"
            ])
            .unwrap(),
            vec![
                json!({ "network": "user", "model": "virtio" }),
                json!({ "network": "lan", "model": "virtio", "security_groups": ["web", "db"] }),
                json!({ "network": "dmz", "model": "virtio", "security_groups": ["web"] }),
            ]
        );
        for bad in [
            "",
            ",sg=web",
            "lan,",
            "lan,sg=",
            "lan,sg=web+",
            "lan,sg=+web",
            "lan,group=web",
            "lan,sg=web,sg=db",
        ] {
            assert!(nics(&["--nic", bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_vm_nic_sg() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "1", "a", "b"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { vm, index: 1, groups } }
                if vm == "web" && groups == ["a", "b"]
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "0"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { index: 0, groups, .. } } if groups.is_empty()
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "-1"]).is_err());
    }

    /// 그 NIC의 그룹만 바꾸고 나머지(다른 NIC, 다른 필드, 라벨)는 그대로 둔다.
    #[test]
    fn nic_groups_update_changes_one_nic() {
        let vm = json!({
            "name": "web",
            "revision": 4,
            "labels": { "team": "a" },
            "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["old"] },
                ],
            },
        });
        let groups = ["web".to_string(), "db".to_string()];
        assert_eq!(
            nic_groups_update(&vm, 1, &groups).unwrap(),
            json!({ "labels": { "team": "a" }, "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["web", "db"] },
                ],
            }})
        );
        assert_eq!(
            nic_groups_update(&vm, 1, &[]).unwrap()["spec"]["nics"][1]["security_groups"],
            json!([])
        );
        let err = nic_groups_update(&vm, 2, &groups).unwrap_err().to_string();
        assert_eq!(err, "VM web has no NIC 2 (2 NICs, numbered from 0)");
    }

    /// `import` 명령의 인자
    fn parse_import(args: &[&str]) -> Result<ImportCmd, clap::Error> {
        let mut argv = vec!["oplctl", "import"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::Import { cmd } => cmd,
            _ => unreachable!(),
        })
    }

    /// `import run`의 인자를 요청 본문으로. `sources`는 가져오기의 원본 네트워크다.
    fn import_run(args: &[&str], sources: &[&str]) -> anyhow::Result<Value> {
        let mut argv = vec!["run", "web01"];
        argv.extend_from_slice(args);
        let ImportCmd::Run(run) = parse_import(&argv)? else {
            unreachable!()
        };
        let sources: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
        import_run_body(&run, &sources)
    }

    #[test]
    fn parses_import_commands() {
        assert!(matches!(
            parse_import(&["ova", "/tmp/web01.ova", "--name", "web01"]).unwrap(),
            ImportCmd::Ova { file, name } if file == std::path::Path::new("/tmp/web01.ova") && name == "web01"
        ));
        assert!(matches!(parse_import(&["list"]).unwrap(), ImportCmd::List));
        assert!(matches!(
            parse_import(&["get", "web01"]).unwrap(),
            ImportCmd::Get { import } if import == "web01"
        ));
        assert!(matches!(
            parse_import(&["delete", "web01", "--force"]).unwrap(),
            ImportCmd::Delete { import, force: true } if import == "web01"
        ));
        // 가져오기 이름, 새 VM 이름과 풀은 꼭 준다.
        for args in [
            &["ova", "/tmp/web01.ova"][..],
            &["run", "web01", "--pool", "local"],
            &["run", "web01", "--name", "web"],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--disk-bus",
                "ide",
            ],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--net",
                "VM Network=",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "=lan",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "",
            ],
        ] {
            assert!(parse_import(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn import_run_builds_the_body() {
        // 주지 않은 값은 비워 두어 서버가 제안 값을 쓴다.
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "VM Network=lan",
                    "--net",
                    "DMZ=dmz",
                    "--net",
                    "a=b=lan"
                ],
                &["DMZ", "VM Network", "a=b"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan", "DMZ": "dmz", "a=b": "lan" },
                "disk_bus": null, "nic_model": null, "firmware": null,
                "sockets": null, "cores": null, "memory_mib": null,
                "keep_macs": true
            })
        );
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "lan",
                    "--disk-bus",
                    "sata",
                    "--nic-model",
                    "e1000e",
                    "--firmware",
                    "bios",
                    "--sockets",
                    "2",
                    "--cores",
                    "4",
                    "--memory",
                    "8192",
                    "--new-macs"
                ],
                &["VM Network"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan" },
                "disk_bus": "sata", "nic_model": "e1000e", "firmware": "bios",
                "sockets": 2, "cores": 4, "memory_mib": 8192,
                "keep_macs": false
            })
        );
        assert_eq!(
            import_run(
                &["--name", "web", "--pool", "p", "--disk-bus", "virtio-blk"],
                &[]
            )
            .unwrap()["disk_bus"],
            "virtio-blk"
        );
        // `=` 없는 --net은 원본 네트워크가 하나일 때만 쓴다.
        let err = import_run(
            &["--name", "web", "--pool", "p", "--net", "lan"],
            &["A", "B"],
        );
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 2 source networks (\"A\", \"B\")"
        );
        let err = import_run(&["--name", "web", "--pool", "p", "--net", "lan"], &[]);
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 0 source networks ()"
        );
    }

    fn sample_import() -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "name": "web01",
            "spec": {
                "node": "n-1",
                "source": { "ova": { "file_name": "web01.ova", "size_bytes": 3u64 << 29 } }
            },
            "status": {
                "phase": "ready",
                "source_vm": {
                    "name": "web01-src", "guest_os": "otherLinux64Guest", "firmware": "uefi",
                    "vcpus": 2, "cores_per_socket": 2, "memory_mib": 1024,
                    "disks": [
                        { "name": "disk0", "file": "web01-disk1.vmdk", "capacity_bytes": 10u64 << 30, "controller": "scsi" }
                    ],
                    "nics": [
                        { "network": "VM Network", "adapter": "vmxnet3", "mac": "00:50:56:aa:bb:cc" },
                        { "network": "DMZ", "adapter": "e1000" }
                    ]
                },
                "proposal": {
                    "sockets": 1, "cores": 2, "memory_mib": 1024, "firmware": "uefi",
                    "disk_bus": "virtio-blk", "nic_model": "virtio",
                    "networks": { "DMZ": null, "VM Network": "VM Network" }
                },
                "warnings": ["first warning", "second warning"],
                "error": "checksum mismatch for web01-disk1.vmdk"
            }
        })
    }

    #[test]
    fn source_networks_are_the_proposal_keys() {
        assert_eq!(source_networks(&sample_import()), ["DMZ", "VM Network"]);
        assert!(source_networks(&json!({ "status": {} })).is_empty());
    }

    #[test]
    fn import_list_shows_node_source_and_vm_names() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let vms = [json!({ "id": "v-1", "name": "web" })];
        assert_eq!(
            import_row(&sample_import(), &nodes, &vms),
            ["web01", "node1", "web01-src", "ready", "-"]
        );
        // 만든 VM은 이름으로, 지운 VM·빠진 노드는 id로 보인다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        assert_eq!(import_row(&done, &nodes, &vms)[3..], ["done", "web"]);
        assert_eq!(
            import_row(&done, &[], &[])[1..],
            ["n-1", "web01-src", "done", "v-1"]
        );
        // 원본을 읽지 못한 가져오기는 파일 이름을 보인다.
        let mut failed = sample_import();
        failed["status"] = json!({ "phase": "failed" });
        assert_eq!(import_row(&failed, &nodes, &vms)[2], "web01.ova");
    }

    #[test]
    fn import_get_shows_the_source_proposal_and_warnings() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let mut out = Vec::new();
        print_import(&mut out, &sample_import(), &nodes, &[]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
vm-import: web01
node: node1
source: web01.ova (1.5GiB)
phase: ready
error: checksum mismatch for web01-disk1.vmdk
source vm: web01-src (otherLinux64Guest, uefi, 2 vCPUs, 2 cores per socket, 1024 MiB)
proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs

DISK   FILE              SIZE     CONTROLLER
disk0  web01-disk1.vmdk  10.0GiB  scsi

NETWORK     ADAPTER  MAC                PROPOSED
VM Network  vmxnet3  00:50:56:aa:bb:cc  VM Network
DMZ         e1000    -                  -

warnings:
  first warning
  second warning
"
        );
        // 끝난 가져오기는 만든 VM을, 경고가 없으면 경고 줄을 뺀다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        done["status"]["warnings"] = json!([]);
        done["status"]["error"] = Value::Null;
        let mut out = Vec::new();
        print_import(
            &mut out,
            &done,
            &nodes,
            &[json!({ "id": "v-1", "name": "web" })],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("phase: done\nvm: web\nsource vm: "), "{text}");
        assert!(
            !text.contains("warnings") && !text.contains("error"),
            "{text}"
        );
    }

    /// `import ova`는 크기로 먼저 확인하고 실제 파일 이름과 함께 올린다.
    #[tokio::test]
    async fn import_ova_checks_then_uploads_with_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        // 본문이 없어야 작은 시험 서버가 요청 줄만 읽고 답할 수 있다.
        let file = dir.path().join("web01-export.ova");
        std::fs::write(&file, b"").unwrap();
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let cmd = ImportCmd::Ova {
            file,
            name: "web01".into(),
        };
        let mut out = Vec::new();
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/vm-imports/ova/check?name=web01&size=0 HTTP/1.1",
                "POST /api/v1/vm-imports/ova?name=web01&file_name=web01-export.ova HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 uploaded; review it with `oplctl import get web01`\n"
        );
    }

    /// `=` 없는 `--net`이면 가져오기를 읽어 원본 네트워크를 찾고, 실행 Task를 기다린다.
    #[tokio::test]
    async fn import_run_reads_the_import_for_a_bare_net_and_waits() {
        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else if path.ends_with("/run") {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            } else {
                json!({ "status": { "proposal": { "networks": { "VM Network": null } } } })
            }
        })
        .await;
        let run = |args: &[&str]| {
            let mut argv = vec![
                "oplctl", "import", "run", "web01", "--name", "web", "--pool", "p",
            ];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Import { cmd } => cmd,
                _ => unreachable!(),
            }
        };
        let mut out = Vec::new();
        import(&client, run(&["--net", "lan"]), false, &mut out)
            .await
            .unwrap();
        // SOURCE=NETWORK만 주면 읽지 않는다.
        import(&client, run(&["--net", "VM Network=lan"]), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/vm-imports/web01 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm web imported\nvm web imported\n"
        );
    }

    /// `import delete --force`는 `?force=true`로 보내고 서버의 알림을 보인다. 아니면 Task를 기다린다.
    #[tokio::test]
    async fn import_delete_force_shows_the_server_message() {
        let (client, seen) = fake_api(|_| {
            json!({ "message": "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)" })
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: true,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/vm-imports/web01?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: false,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/vm-imports/web01 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "vm-import web01 deleted\n");
    }

    #[test]
    fn parses_import_esxi_commands() {
        let ImportCmd::EsxiBrowse { conn, fingerprint } =
            parse_import(&["esxi-browse", "--host", "esx1", "--user", "root"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(fingerprint, None);
        // 포트와 데이터센터는 ESXi 기본값이다.
        assert_eq!(
            esxi_connection(&conn, fingerprint.as_deref(), "secret"),
            json!({
                "host": "esx1", "port": 443, "user": "root", "password": "secret",
                "datacenter": "ha-datacenter", "fingerprint": null
            })
        );
        let ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } = parse_import(&[
            "esxi",
            "web01",
            "--host",
            "vc1",
            "--port",
            "8443",
            "--user",
            "admin@vsphere.local",
            "--datacenter",
            "DC1",
            "--datastore",
            "datastore1",
            "--vmx",
            "web01/web01.vmx",
            "--fingerprint",
            "AB:CD",
        ])
        .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            esxi_import_body(
                &name,
                &esxi_connection(&conn, Some(&fingerprint), "pw"),
                &datastore,
                &vmx
            ),
            json!({
                "name": "web01",
                "connection": {
                    "host": "vc1", "port": 8443, "user": "admin@vsphere.local", "password": "pw",
                    "datacenter": "DC1", "fingerprint": "AB:CD"
                },
                "datastore": "datastore1",
                "vmx": "web01/web01.vmx"
            })
        );
        // 만들기에는 지문·데이터스토어·.vmx가 꼭 있어야 하고, 비밀번호는 인자로 받지 않는다.
        let full = [
            "esxi",
            "web01",
            "--host",
            "esx1",
            "--user",
            "root",
            "--datastore",
            "ds",
            "--vmx",
            "a/a.vmx",
            "--fingerprint",
            "AB",
        ];
        assert!(parse_import(&full).is_ok());
        for skip in ["--datastore", "--vmx", "--fingerprint", "--host", "--user"] {
            let i = full.iter().position(|a| *a == skip).unwrap();
            let args: Vec<&str> = full[..i].iter().chain(&full[i + 2..]).copied().collect();
            assert!(parse_import(&args).is_err(), "{args:?}");
        }
        let mut with_password = full.to_vec();
        with_password.extend(["--password", "x"]);
        assert!(parse_import(&with_password).is_err());
        assert!(parse_import(&["esxi-browse", "--host", "esx1"]).is_err());

        // 비밀번호 파일(끝의 줄바꿈은 뺀다)
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("esxi-password");
        std::fs::write(&file, "VMware1!\n").unwrap();
        let mut with_file = full.to_vec();
        with_file.extend(["--password-file", file.to_str().unwrap()]);
        let ImportCmd::Esxi { conn, .. } = parse_import(&with_file).unwrap() else {
            unreachable!()
        };
        assert_eq!(esxi_password(&conn).unwrap(), "VMware1!");
    }

    fn sample_browse() -> Value {
        json!({
            "datastores": [
                {
                    "name": "datastore1",
                    "vms": [
                        {
                            "vmx": "web01/web01.vmx", "name": "web01", "guest_os": "ubuntu-64",
                            "vcpus": 2, "memory_mib": 1024, "disks": 1, "locked": false, "error": null
                        },
                        {
                            "vmx": "db/db.vmx", "name": "db", "guest_os": "", "vcpus": 0,
                            "memory_mib": 0, "disks": 0, "locked": true,
                            "error": "memsize is missing"
                        }
                    ]
                },
                { "name": "empty", "vms": [] }
            ]
        })
    }

    /// `import esxi-browse`는 연결을 보내고 데이터스토어의 VM을 표로 보인다. 읽지 못한 VM은 NAME
    /// 자리에 오류를 보인다.
    #[tokio::test]
    async fn import_esxi_browse_shows_the_vms() {
        let (client, seen) = fake_api(|_| sample_browse()).await;
        let mut out = Vec::new();
        let conn = json!({ "host": "esx1" });
        esxi_browse(&client, &conn, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi/browse HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
DATASTORE   VMX              NAME                       GUEST      CPU  MEMORY  DISKS  LOCKED
datastore1  web01/web01.vmx  web01                      ubuntu-64  2    1.0GiB  1      false
datastore1  db/db.vmx        error: memsize is missing  -          -    -       -      true
"
        );
        // --json이면 응답 그대로
        let mut out = Vec::new();
        esxi_browse(&client, &conn, true, &mut out).await.unwrap();
        let printed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(printed, sample_browse());
    }

    /// 지문을 주지 않았거나(409 `untrusted_certificate`) 틀리면 서버 인증서의 지문으로 다시 하라고 알린다.
    #[tokio::test]
    async fn import_esxi_explains_an_untrusted_certificate() {
        let (client, _) = fake_api_status(|_| {
            (
                409,
                json!({
                    "code": "untrusted_certificate",
                    "message": "verify the ESXi certificate fingerprint",
                    "details": { "fingerprint": "AB:CD:EF" }
                }),
            )
        })
        .await;
        let conn = json!({ "host": "esx1" });
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        let body = esxi_import_body("web01", &conn, "ds", "a/a.vmx");
        let err = esxi_create(&client, &body, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        // 다른 오류는 서버의 메시지와 코드 그대로
        let (client, _) = fake_api_status(|_| {
            (
                400,
                json!({ "code": "esxi_auth", "message": "ESXi rejected the user name or password" }),
            )
        })
        .await;
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "ESXi rejected the user name or password (esxi_auth)"
        );
    }

    /// `import esxi`는 가져오기를 만들고 검토 명령을 알린다.
    #[tokio::test]
    async fn import_esxi_creates_the_import() {
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let mut out = Vec::new();
        let body = esxi_import_body("web01", &json!({}), "ds", "a/a.vmx");
        esxi_create(&client, &body, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 created; review it with `oplctl import get web01`\n"
        );
    }

    fn sample_esxi_import() -> Value {
        let mut i = sample_import();
        i["spec"]["source"] = json!({ "esxi": {
            "host": "esx1.example.com", "port": 443, "datacenter": "ha-datacenter",
            "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": "root",
            "fingerprint": "AB:CD"
        } });
        i
    }

    /// ESXi에서 가져오면 SOURCE는 `esxi://호스트/데이터스토어/.vmx`다.
    #[test]
    fn import_list_and_get_show_an_esxi_source() {
        let url = "esxi://esx1.example.com/datastore1/web01/web01.vmx";
        assert_eq!(import_row(&sample_esxi_import(), &[], &[])[2], url);
        let mut out = Vec::new();
        print_import(&mut out, &sample_esxi_import(), &[], &[]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(&format!(
                "vm-import: web01\nnode: n-1\nsource: {url}\nphase: ready\n"
            )),
            "{text}"
        );
    }

    /// `node updates`는 확인 시각, 오류, 패키지 표, 잘렸는지를 보인다. `--refresh`면 다시 확인 Task를
    /// 기다린 뒤 읽는다.
    #[tokio::test]
    async fn node_updates_shows_the_check() {
        fn reply(path: &str) -> Value {
            match path {
                "/api/v1/nodes/n1/updates/refresh" => json!({ "id": "t1" }),
                "/api/v1/tasks/t1" => json!({ "id": "t1", "status": { "state": "succeeded" } }),
                "/api/v1/nodes/n1" => json!({
                    "name": "n1",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000,
                        "packages": [
                            { "name": "oplite", "current": "0.2.0", "candidate": "0.2.1" },
                            { "name": "linux-image-6.12.48+deb13-amd64", "current": null, "candidate": "6.12.48-1" }
                        ],
                        "truncated": true,
                        "error": null
                    } }
                }),
                "/api/v1/nodes/n2" => json!({
                    "name": "n2",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000, "packages": [],
                        "error": "apt-get update -q failed (exit status: 100): E: no network"
                    } }
                }),
                "/api/v1/nodes/n3" => json!({
                    "name": "n3",
                    "status": { "updates": { "checked_at": 1_793_000_000, "packages": [], "error": null } }
                }),
                "/api/v1/nodes/n4" => json!({
                    "name": "n4",
                    "status": { "updates": { "checked_at": null, "packages": [], "error": null } }
                }),
                _ => json!({ "name": "w", "status": { "online": true } }),
            }
        }
        let (client, seen) = fake_api(reply).await;
        let run = |args: &'static [&'static str], json_out: bool| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Updates { node, refresh },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_updates(&client, &node, refresh, json_out, &mut out)
                    .await
                    .map(|()| String::from_utf8(out).unwrap())
            }
        };

        let text = run(&["oplctl", "node", "updates", "n1", "--refresh"], false)
            .await
            .unwrap();
        assert_eq!(
            text,
            "checked: 2026-10-26 07:33 UTC\n\
             PACKAGE                          CURRENT  CANDIDATE\n\
             oplite                           0.2.0    0.2.1\n\
             linux-image-6.12.48+deb13-amd64  -        6.12.48-1\n\
             (only the first 2 packages are shown)\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/updates/refresh HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "GET /api/v1/nodes/n1 HTTP/1.1"
            ]
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n2"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\n\
             error: apt-get update -q failed (exit status: 100): E: no network\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n3"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\nno updates available\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n4"], false)
                .await
                .unwrap(),
            "checked: never\n"
        );
        let err = run(&["oplctl", "node", "updates", "w"], false)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "node w does not check for updates");
        let shown: Value = serde_json::from_str(
            &run(&["oplctl", "node", "updates", "n3"], true)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(shown["packages"], json!([]));
    }

    /// `node upgrade`는 업그레이드 Task를 만들고 따라가는 방법을 알린다. `--wait`이면 Task를 기다린다.
    #[tokio::test]
    async fn node_upgrade_starts_the_task() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes/n1/upgrade" => json!({ "id": "t1" }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Upgrade { node, wait },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_upgrade(&client, &node, wait, &mut out).await.unwrap();
                String::from_utf8(out).unwrap()
            }
        };

        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1"]).await,
            "upgrade of n1 started (task t1); follow it with `oplctl task get t1`\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/nodes/n1/upgrade HTTP/1.1"]
        );
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1", "--wait"]).await,
            "node n1 upgraded\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/upgrade HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
    }

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
    #[test]
    fn settings_set_body_maps_known_keys() {
        assert_eq!(
            settings_set_body(&["updates.channel=testing".into()]).unwrap(),
            json!({ "updates": { "channel": "testing" } })
        );
        let err = settings_set_body(&["updates.mirror=x".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown setting updates.mirror; known: updates.channel"
        );
        let err = settings_set_body(&["updates.channel".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected KEY=VALUE, got \"updates.channel\""
        );
    }

    /// `settings get`은 spec을 `키: 값` 줄로, `-o json`이면 리소스 그대로 보인다. `set`은 PATCH한다.
    #[tokio::test]
    async fn settings_get_and_set() {
        let (client, seen) = fake_api(|_| {
            json!({
                "id": "00000000-0000-0000-0000-000000000000", "name": "cluster", "revision": 0,
                "spec": { "updates": { "channel": "stable" } }, "status": {}
            })
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "settings", "get"][..],
            &["oplctl", "settings", "set", "updates.channel=testing"][..],
        ] {
            let Cmd::Settings { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            settings(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "updates.channel: stable\nsettings updated\n"
        );

        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "get"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let mut out = Vec::new();
        settings(&client, cmd, true, &mut out).await.unwrap();
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["spec"]["updates"]["channel"], "stable");

        // 모르는 키는 요청을 보내지 않는다.
        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "set", "x=1"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        assert!(
            settings(&client, cmd, false, &mut Vec::new())
                .await
                .is_err()
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    /// `subscription show`는 상태를 줄로 보인다. 한도가 없으면 쓰는 노드 수만.
    #[test]
    fn subscription_lines_show_the_state() {
        let active = json!({
            "state": "active", "id": "sub-1", "customer": "Example Corp", "level": "premium",
            "nodes": 3, "used_nodes": 2, "expires_at": 1_700_000_000u64, "development": true
        });
        assert_eq!(
            subscription_lines(&active),
            [
                "state: active (development key)",
                "customer: Example Corp",
                "level: premium",
                "nodes: 2/3",
                "expires: 2023-11-14",
            ]
        );
        let expired = json!({
            "state": "expired", "id": "sub-1", "customer": "C", "level": "standard",
            "nodes": 1, "used_nodes": 1, "expires_at": 0, "development": false
        });
        assert_eq!(subscription_lines(&expired)[0], "state: expired");
        assert_eq!(
            subscription_lines(&json!({ "state": "none", "used_nodes": 1 })),
            ["state: none", "nodes: 1"]
        );
    }

    /// `subscription set --file -`은 표준 입력에서 키를 읽는다(앞뒤 공백·줄바꿈을 다듬음).
    #[test]
    fn subscription_key_can_come_from_stdin() {
        let args = ["oplctl", "subscription", "set", "--file", "-"];
        let Cmd::Subscription {
            cmd: SubscriptionCmd::Set { key: None, file },
        } = Cli::try_parse_from(args).unwrap().cmd
        else {
            panic!("parse --file -")
        };
        assert_eq!(file.unwrap().to_str(), Some("-"));
        let mut input = std::io::Cursor::new(" opl1.a.b\r\n");
        assert_eq!(read_stdin_key(&mut input).unwrap(), "opl1.a.b");
    }

    /// `subscription set`은 키(인자나 파일)를 PUT하고 상태를 보인다. `remove`는 DELETE한다.
    #[tokio::test]
    async fn subscription_set_show_and_remove() {
        let (client, seen) = fake_api(|_| {
            json!({ "state": "active", "customer": "C", "level": "standard", "nodes": 2,
                    "used_nodes": 1, "expires_at": 1_700_000_000u64, "development": false })
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key");
        std::fs::write(&file, "opl1.a.b\r\n").unwrap();
        let file = file.to_str().unwrap();
        let mut out = Vec::new();
        for args in [
            &["oplctl", "subscription", "show"][..],
            &["oplctl", "subscription", "set", "opl1.a.b"][..],
            &["oplctl", "subscription", "set", "--file", file][..],
            &["oplctl", "subscription", "remove"][..],
        ] {
            let Cmd::Subscription { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            subscription(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "DELETE /api/v1/subscription HTTP/1.1",
            ]
        );
        let shown =
            "state: active\ncustomer: C\nlevel: standard\nnodes: 1/2\nexpires: 2023-11-14\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}subscription removed\n")
        );

        // 키나 파일 중 하나만 준다.
        for args in [
            &["oplctl", "subscription", "set"][..],
            &["oplctl", "subscription", "set", "k", "--file", "f"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    /// `telemetry show`는 켜짐·끝점·id·마지막 전송·오류를 줄로 보인다.
    #[test]
    fn telemetry_lines_show_the_state() {
        let settings = json!({
            "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
            "status": { "telemetry": {
                "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last_sent_at": 1_761_471_000, "last_error": "HTTP 500"
            } }
        });
        assert_eq!(
            telemetry_lines(&settings),
            [
                "enabled: true",
                "endpoint: https://t.example/r",
                "id: 6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last sent: 2025-10-26 09:30 UTC",
                "last error: HTTP 500",
            ]
        );
        let default_endpoint = if opl_types::version::TELEMETRY_URL.is_empty() {
            "endpoint: none".to_string()
        } else {
            format!(
                "endpoint: {} (build default)",
                opl_types::version::TELEMETRY_URL
            )
        };
        assert_eq!(
            telemetry_lines(&json!({ "spec": { "updates": {} }, "status": {} })),
            [
                "enabled: false".to_string(),
                default_endpoint,
                "id: none".into(),
                "last sent: never".into(),
                "last error: none".into(),
            ]
        );
    }

    #[test]
    fn telemetry_bodies_send_only_what_was_given() {
        assert_eq!(
            telemetry_body(true, None),
            json!({ "telemetry": { "enabled": true } })
        );
        assert_eq!(
            telemetry_body(true, Some("https://t.example/r")),
            json!({ "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } })
        );
        assert_eq!(
            telemetry_body(false, None),
            json!({ "telemetry": { "enabled": false } })
        );
    }

    /// `telemetry`의 하위 명령이 부르는 API. preview는 받은 JSON을 그대로 예쁘게 보인다.
    #[tokio::test]
    async fn telemetry_commands_call_the_api() {
        let (client, seen) = fake_api(|path| {
            if path == "/api/v1/telemetry/preview" {
                json!({ "schema": 1, "vms": { "total": 2 } })
            } else {
                json!({
                    "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
                    "status": { "telemetry": { "id": "abc" } }
                })
            }
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "telemetry", "show"][..],
            &[
                "oplctl",
                "telemetry",
                "enable",
                "--endpoint",
                "https://t.example/r",
            ][..],
            &["oplctl", "telemetry", "disable"][..],
            &["oplctl", "telemetry", "reset-id"][..],
            &["oplctl", "telemetry", "preview"][..],
        ] {
            let Cmd::Telemetry { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            telemetry(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "POST /api/v1/telemetry/reset-id HTTP/1.1",
                "GET /api/v1/telemetry/preview HTTP/1.1",
            ]
        );
        let shown = "enabled: true\nendpoint: https://t.example/r\nid: abc\nlast sent: never\nlast error: none\n";
        let preview = "{\n  \"schema\": 1,\n  \"vms\": {\n    \"total\": 2\n  }\n}\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}{shown}{preview}")
        );
    }

    /// `--since`의 기간(`30m`·`1h`·`7d`)은 지금에서 뺀 유닉스 밀리초로, 나머지는 그대로 보낸다.
    #[test]
    fn audit_since_turns_durations_into_milliseconds() {
        let now = 1_762_400_000_000;
        assert_eq!(audit_since("30m", now), (now - 30 * 60_000).to_string());
        assert_eq!(audit_since("1h", now), (now - 3_600_000).to_string());
        assert_eq!(audit_since("7d", now), (now - 7 * 86_400_000).to_string());
        assert_eq!(audit_since("90s", now), (now - 90_000).to_string());
        assert_eq!(
            audit_since("2026-11-06T00:00:00Z", now),
            "2026-11-06T00:00:00Z"
        );
        assert_eq!(audit_since("1762400000000", now), "1762400000000");
        assert_eq!(audit_since("xd", now), "xd");
    }

    /// `audit`는 거르기를 쿼리로 보내고 표를 보인다. 읽지 못한 노드와 잘림은 stderr에 알린다.
    /// `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
    #[tokio::test]
    async fn audit_shows_a_table_and_warnings() {
        let (client, seen) = fake_api(|_| {
            json!({
                "entries": [
                    { "id": "019a0000-0000-7000-8000-000000000002", "time": 1_762_387_205_123u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "token", "ip": "10.0.0.5", "method": "POST",
                      "path": "/vms/web-1/actions/start", "action": "POST /vms/{key}/actions/{action}",
                      "target": { "kind": "vms", "key": "web-1" }, "status": 202,
                      "duration_ms": 12, "detail": null },
                    { "id": "019a0000-0000-7000-8000-000000000001", "time": 1_762_387_200_000u64,
                      "node": "node-b", "user": null, "user_id": null, "role": null,
                      "auth": "none", "ip": "10.0.0.9", "method": "POST", "path": "/auth/login",
                      "action": "POST /auth/login", "target": null, "status": 401,
                      "duration_ms": 3, "detail": "username=bob" },
                    { "id": "019a0000-0000-7000-8000-000000000000", "time": 1_762_387_100_000u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "session", "ip": "10.0.0.5", "method": "POST", "path": "/auth/logout",
                      "action": "POST /auth/logout", "target": null, "status": 204,
                      "duration_ms": 1, "detail": null }
                ],
                "unreachable": ["node-c", "node-d"],
                "disabled": ["node-e"],
                "truncated": true
            })
        })
        .await;
        let args = [
            "oplctl",
            "audit",
            "--since",
            "2026-11-06T00:00:00Z",
            "--until",
            "1762400000000",
            "--user",
            "alice",
            "--node",
            "node-a",
            "--action",
            "start",
            "--limit",
            "5",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, false, &mut out, &mut err).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/audit?since=2026-11-06T00%3A00%3A00Z&until=1762400000000\
              &user=alice&node=node-a&action=start&limit=5 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "TIME                  NODE    USER   IP        ACTION                            TARGET        STATUS\n\
             2025-11-06T00:00:05Z  node-a  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202\n\
             2025-11-06T00:00:00Z  node-b  -      10.0.0.9  POST /auth/login                  username=bob  401\n\
             2025-11-05T23:58:20Z  node-a  alice  10.0.0.5  POST /auth/logout                 -             204\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no audit records from: node-c, node-d\n\
             note: the audit log is disabled on: node-e\n\
             note: more entries match; narrow the range or raise --limit\n"
        );

        // 거르기가 없으면 쿼리도 없다(서버 기본값: 지난 24시간, 200개).
        let Cmd::Audit(a) = Cli::try_parse_from(["oplctl", "audit"]).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, true, &mut out, &mut err).await.unwrap();
        assert_eq!(seen.lock().unwrap()[1], "GET /api/v1/audit HTTP/1.1");
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["unreachable"], json!(["node-c", "node-d"]));
        assert_eq!(shown["entries"][1]["detail"], "username=bob");
        assert!(err.is_empty());

        // 기간 --since는 --until과 함께 줄 수 없다(서버에 보내지 않는다).
        let args = [
            "oplctl",
            "audit",
            "--since",
            "1h",
            "--until",
            "1762400000000",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let e = audit(&client, a, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "a relative --since cannot be combined with --until; give a time"
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// `fake_api_bodies`가 남긴 요청
    #[derive(Debug)]
    struct SeenRequest {
        line: String,
        if_match: Option<String>,
        body: Value,
    }

    /// `fake_api`와 같고 요청의 `if-match`와 JSON 본문도 남긴다. 응답에는 세션 쿠키를 붙인다.
    async fn fake_api_bodies(
        reply: fn(&str) -> Value,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<SeenRequest>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = vec![0; 4096];
                // 머리와 content-length만큼의 본문을 다 읽는다.
                let (head, body) = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "connection closed mid-request");
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let length = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break (head.to_string(), body.to_string());
                    }
                };
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let if_match = head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("if-match")
                        .then(|| v.trim().to_string())
                });
                log.lock().unwrap().push(SeenRequest {
                    line,
                    if_match,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                });
                let body = reply(&path).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nset-cookie: opl_session=s1; Path=/\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        (base, seen)
    }

    fn realm_cmd(args: &[&str]) -> Result<RealmCmd, clap::Error> {
        let mut all = vec!["oplctl", "realm"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Realm { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `realm add-ldap`은 url·base DN이 필요하고, `--group`은 마지막 `=`에서 나눈다.
    #[test]
    fn parses_realm_commands() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--group",
            "cn=admins,dc=example,dc=com=admin",
            "--group",
            "cn=staff,dc=example,dc=com=Viewer",
        ])
        .unwrap() else {
            panic!("add-ldap")
        };
        assert_eq!(add.ldap.groups.len(), 2);
        assert_eq!(add.ldap.groups[0].0, "cn=admins,dc=example,dc=com");
        assert!(matches!(add.ldap.groups[0].1, Role::Admin));
        assert!(matches!(add.ldap.groups[1].1, Role::Viewer));

        assert_eq!(
            parse_group_role("cn=a,dc=x=root").unwrap_err(),
            "invalid role \"root\" in \"cn=a,dc=x=root\": use admin, operator or viewer"
        );
        assert!(parse_group_role("admins").is_err());
        assert!(parse_group_role("=admin").is_err());

        for args in [
            // url·base DN이 없다.
            &["add-ldap", "corp", "--url", "ldaps://h"][..],
            &["add-ldap", "corp", "--base-dn", "dc=x"][..],
            // 비밀번호를 주면서 지울 수는 없다.
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-password-file",
                "f",
            ][..],
            // 지우기와 설정은 함께 줄 수 없다.
            &["set-ldap", "corp", "--clear-bind-dn", "--bind-dn", "cn=x"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-dn",
                "--bind-password-file",
                "f",
            ][..],
            &["set-ldap", "corp", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &[
                "set-ldap",
                "corp",
                "--clear-groups",
                "--group",
                "cn=x=admin",
            ][..],
            // 비밀번호 파일은 사용자와 함께
            &["test", "corp", "--password-file", "f"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "corp", "--user", "alice"]).unwrap()
        else {
            panic!("test")
        };
        assert_eq!(user.as_deref(), Some("alice"));
    }

    /// `add-ldap`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_body_sends_only_what_is_given() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldap://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_body(&add, LdapFiles::default()),
            json!({
                "name": "corp",
                "backend": { "kind": "ldap", "url": "ldap://ldap.example.com", "base_dn": "dc=example,dc=com" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--bind-dn",
            "cn=svc,dc=example,dc=com",
            "--user-filter",
            "(sAMAccountName={username})",
            "--group-attr",
            "isMemberOf",
            "--timeout",
            "5",
            "--display",
            "Corp",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("svc-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_body(&add, files),
            json!({
                "name": "corp",
                "display": "Corp",
                "backend": {
                    "kind": "ldap", "url": "ldaps://ldap.example.com", "base_dn": "dc=example,dc=com",
                    "bind_dn": "cn=svc,dc=example,dc=com", "bind_password": "svc-pw",
                    "user_filter": "(sAMAccountName={username})", "group_attr": "isMemberOf",
                    "ca_pem": "PEM", "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000001", "name": "corp", "revision": 4,
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 10, "has_bind_password": true
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-ldap`은 지금 영역에 준 것만 바꾼다. 비밀번호는 주지 않으면 빼고(서버가 둔다),
    /// `--clear-bind-password`면 빈 글이다. `--group`을 주면 표 전체를 바꾼다.
    #[test]
    fn realm_set_body_changes_only_what_is_given() {
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        let kept = json!({
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 3
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        });
        assert_eq!(
            realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap(),
            kept
        );

        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--url",
            "ldap://other.example.com",
            "--clear-bind-dn",
            "--clear-ca",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "admin",
            "--display",
            "Corporate",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        let body = realm_set_body(&with_ca, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["display"], "Corporate");
        assert_eq!(body["backend"]["url"], "ldap://other.example.com");
        // --clear-bind-dn은 저장된 비밀번호도 지운다.
        assert_eq!(body["backend"]["bind_password"], "");
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
        assert!(body["backend"].get("ca_pem").is_none(), "{body}");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                "default": "admin"
            })
        );

        // 새 비밀번호와 CA
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("new-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_body(&sample_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["bind_password"], "new-pw");
        assert_eq!(body["backend"]["ca_pem"], "PEM");

        // 그룹 표와 기본 역할 지우기
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-groups", "--clear-default-role"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert_eq!(body["roles"], json!({ "groups": [] }));

        // 서비스 계정 DN이 남는데 비밀번호만 지울 수는 없다(새 DN을 줘도).
        for args in [
            &["set-ldap", "corp", "--clear-bind-password"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-dn",
                "cn=other",
            ][..],
        ] {
            let RealmCmd::SetLdap(set) = realm_cmd(args).unwrap() else {
                unreachable!()
            };
            let e = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap_err();
            assert_eq!(
                e.to_string(),
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
        // DN이 없는 영역이나 DN도 지우면 된다.
        let mut anon = sample_realm();
        anon["backend"].as_object_mut().unwrap().remove("bind_dn");
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-bind-password"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&anon, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["backend"]["bind_password"], "");
        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--clear-bind-password",
            "--clear-bind-dn",
        ])
        .unwrap() else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
    }

    /// url·bind_dn·CA를 바꾸면서 비밀번호를 주지 않으면 서버의 400을 그대로 보인다.
    #[tokio::test]
    async fn realm_set_shows_the_server_refusal() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                (200, sample_realm())
            } else {
                (
                    400,
                    json!({
                        "code": "invalid_request",
                        "message": "bind_password must be given again when url, bind_dn or ca_pem changes"
                    }),
                )
            }
        })
        .await;
        let e = realm(
            &client,
            realm_cmd(&["set-ldap", "corp", "--url", "ldaps://other.example.com"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("bind_password must be given again when url, bind_dn or ca_pem changes"),
            "{e}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1"
            ]
        );
    }

    #[test]
    fn realm_show_lines() {
        assert_eq!(
            realm_lines(&sample_realm()).join("\n"),
            "name: corp\n\
             display: Corp\n\
             kind: ldap\n\
             url: ldaps://ldap.example.com\n\
             base dn: dc=example,dc=com\n\
             bind dn: cn=svc,dc=example,dc=com\n\
             bind password: set\n\
             user filter: (uid={username})\n\
             group attribute: memberOf\n\
             ca: system roots\n\
             timeout: 10s\n\
             groups:\n  cn=admins,dc=example,dc=com = admin\n\
             default role: viewer"
        );
        let mut bare = sample_realm();
        bare["backend"]["has_bind_password"] = json!(false);
        bare["backend"]["ca_pem"] = json!("PEM");
        bare["roles"] = json!({ "groups": [] });
        let lines = realm_lines(&bare);
        for line in [
            "bind password: none",
            "ca: custom (PEM)",
            "groups: none",
            "default role: none",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// 영역 명령이 부르는 API: 파일의 비밀번호·CA는 본문에, `set-ldap`은 읽은 revision을
    /// If-Match로, `test`는 단계 표를 보이고 실패한 단계가 있으면 오류다.
    #[tokio::test]
    async fn realm_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_realm()] }),
            "/api/v1/realms/corp/actions/test" => json!({
                "steps": [
                    { "name": "connect", "ok": true, "message": "connected to ldaps://ldap.example.com" },
                    { "name": "service_bind", "ok": true, "message": "bound as cn=svc,dc=example,dc=com" },
                    { "name": "search", "ok": true, "message": "found uid=alice,dc=example,dc=com" },
                    { "name": "user_bind", "ok": true, "message": "the password is correct" },
                    { "name": "groups", "ok": true, "message": "2 groups" }
                ],
                "dn": "uid=alice,dc=example,dc=com",
                "groups": ["cn=admins,dc=example,dc=com", "cn=staff,dc=example,dc=com"],
                "role": "admin"
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "connect", "ok": false, "message": "connection refused" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("pw");
        std::fs::write(&secret, "svc-pw\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-ldap",
                "corp",
                "--url",
                "ldaps://ldap.example.com",
                "--base-dn",
                "dc=example,dc=com",
                "--bind-dn",
                "cn=svc,dc=example,dc=com",
                "--bind-password-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-ldap", "corp", "--bind-password-file", secret][..],
            &["remove", "corp"][..],
            &["test", "corp", "--user", "alice", "--password-file", secret][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken", "--user", "a", "--password-file", secret]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the connect step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1",
                "DELETE /api/v1/realms/corp HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "POST /api/v1/realms/corp/actions/test HTTP/1.1",
                "GET /api/v1/realms/broken HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["bind_password"], "svc-pw");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("4"));
        assert_eq!(seen[3].body["backend"]["bind_password"], "svc-pw");
        assert!(seen[3].body["backend"].get("has_bind_password").is_none());
        assert_eq!(
            seen[6].body,
            json!({ "username": "alice", "password": "svc-pw" })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             corp  Corp     ldap  ldaps://ldap.example.com\n\
             realm corp created\n\
             realm corp updated\n\
             realm corp removed\n\
             STEP          RESULT  MESSAGE\n\
             connect       ok      connected to ldaps://ldap.example.com\n\
             service_bind  ok      bound as cn=svc,dc=example,dc=com\n\
             search        ok      found uid=alice,dc=example,dc=com\n\
             user_bind     ok      the password is correct\n\
             groups        ok      2 groups\n\
             dn: uid=alice,dc=example,dc=com\n\
             groups: cn=admins,dc=example,dc=com, cn=staff,dc=example,dc=com\n\
             role: admin\n"
        );
    }

    /// `login --realm`은 로그인 본문에 영역을 넣고(local이면 넣지 않음) 서버가 정한 이름을 돌려준다.
    #[tokio::test]
    async fn login_sends_the_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/login" => json!({ "name": "alice@corp", "role": "admin" }),
            "/api/v1/tokens" => json!({ "secret": "opl_t_s" }),
            _ => json!({}),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let pw = dir.path().join("pw");
        std::fs::write(&pw, "alice-pw\n").unwrap();
        let pw = pw.to_str().unwrap();
        for (realm, expected) in [(Some("corp"), Some("corp")), (None, None)] {
            let mut args = vec!["oplctl", "login", "--user", "Alice", "--password-file", pw];
            if let Some(realm) = realm {
                args.extend(["--realm", realm]);
            }
            let Cmd::Login(args) = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            assert_eq!(
                login(&base, &args, None).await.unwrap(),
                ("opl_t_s".to_string(), "alice@corp".to_string())
            );
            let seen = seen.lock().unwrap();
            let sent = &seen[seen.len() - 3];
            assert_eq!(sent.line, "POST /api/v1/auth/login HTTP/1.1");
            assert_eq!(sent.body["username"], "Alice");
            assert_eq!(sent.body["password"], "alice-pw");
            assert_eq!(sent.body.get("realm").and_then(Value::as_str), expected);
        }
    }

    /// `realm add-oidc`는 issuer·client ID가 필요하고, 지우기와 설정은 함께 줄 수 없다.
    /// `test`는 사용자 없이도 된다(OIDC).
    #[test]
    fn parses_oidc_realm_commands() {
        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com",
            "--client-id",
            "oplite",
            "--scope",
            "openid",
            "--scope",
            "groups",
            "--group",
            "opl-admins=admin",
        ])
        .unwrap() else {
            panic!("add-oidc")
        };
        assert_eq!(add.oidc.scopes, ["openid", "groups"]);
        assert_eq!(add.oidc.groups[0].0, "opl-admins");
        assert!(matches!(add.oidc.groups[0].1, Role::Admin));

        for args in [
            &["add-oidc", "okta", "--issuer", "https://idp"][..],
            &["add-oidc", "okta", "--client-id", "oplite"][..],
            &[
                "set-oidc",
                "okta",
                "--clear-client-secret",
                "--client-secret-file",
                "f",
            ][..],
            &["set-oidc", "okta", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-oidc",
                "okta",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &["set-oidc", "okta", "--clear-groups", "--group", "g=admin"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "okta"]).unwrap() else {
            panic!("test")
        };
        assert_eq!(user, None);
    }

    /// `add-oidc`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_oidc_body_sends_only_what_is_given() {
        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com",
            "--client-id",
            "oplite",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_oidc_body(&add, OidcFiles::default()),
            json!({
                "name": "okta",
                "backend": { "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com/oauth2/default",
            "--client-id",
            "oplite",
            "--scope",
            "openid",
            "--scope",
            "groups",
            "--username-claim",
            "email",
            "--groups-claim",
            "roles",
            "--timeout",
            "5",
            "--display",
            "Okta",
            "--group",
            "opl-ops=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = OidcFiles {
            client_secret: Some("s3cret".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_oidc_body(&add, files),
            json!({
                "name": "okta",
                "display": "Okta",
                "backend": {
                    "kind": "oidc", "issuer": "https://idp.example.com/oauth2/default",
                    "client_id": "oplite", "client_secret": "s3cret", "scopes": ["openid", "groups"],
                    "username_claim": "email", "groups_claim": "roles", "ca_pem": "PEM",
                    "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "opl-ops", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_oidc_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000002", "name": "okta", "revision": 9,
            "display": "Okta",
            "backend": {
                "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite",
                "scopes": ["openid", "profile", "email"], "username_claim": "preferred_username",
                "groups_claim": "groups", "timeout_secs": 10, "has_client_secret": true
            },
            "roles": {
                "groups": [{ "group": "opl-admins", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-oidc`는 지금 영역에 준 것만 바꾼다. 비밀은 주지 않으면 빼고(서버가 둔다),
    /// `--clear-client-secret`면 빈 글이다. 다른 종류의 영역은 바꾸지 않는다.
    #[test]
    fn realm_set_oidc_body_changes_only_what_is_given() {
        let RealmCmd::SetOidc(set) = realm_cmd(&["set-oidc", "okta", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            realm_set_oidc_body(&sample_oidc_realm(), &set, OidcFiles::default()).unwrap(),
            json!({
                "display": "Okta",
                "backend": {
                    "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite",
                    "scopes": ["openid", "profile", "email"], "username_claim": "preferred_username",
                    "groups_claim": "groups", "timeout_secs": 3
                },
                "roles": {
                    "groups": [{ "group": "opl-admins", "role": "admin" }],
                    "default": "viewer"
                }
            })
        );

        let RealmCmd::SetOidc(set) = realm_cmd(&[
            "set-oidc",
            "okta",
            "--issuer",
            "https://other.example.com",
            "--client-id",
            "opl2",
            "--scope",
            "openid",
            "--username-claim",
            "email",
            "--groups-claim",
            "roles",
            "--clear-client-secret",
            "--clear-ca",
            "--clear-groups",
            "--clear-default-role",
            "--display",
            "Other",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_oidc_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        assert_eq!(
            realm_set_oidc_body(&with_ca, &set, OidcFiles::default()).unwrap(),
            json!({
                "display": "Other",
                "backend": {
                    "kind": "oidc", "issuer": "https://other.example.com", "client_id": "opl2",
                    "client_secret": "", "scopes": ["openid"], "username_claim": "email",
                    "groups_claim": "roles", "timeout_secs": 10
                },
                "roles": { "groups": [] }
            })
        );

        // 새 비밀·CA와 그룹 표
        let RealmCmd::SetOidc(set) = realm_cmd(&[
            "set-oidc",
            "okta",
            "--group",
            "opl-ops=operator",
            "--default-role",
            "admin",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = OidcFiles {
            client_secret: Some("new-secret".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_oidc_body(&sample_oidc_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["client_secret"], "new-secret");
        assert_eq!(body["backend"]["ca_pem"], "PEM");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "opl-ops", "role": "operator" }],
                "default": "admin"
            })
        );

        // 종류가 다른 영역
        let e = realm_set_oidc_body(&sample_realm(), &set, OidcFiles::default()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "realm okta is not an OIDC realm (its kind is ldap)"
        );
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let e = realm_set_body(&sample_oidc_realm(), &set, LdapFiles::default()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "realm corp is not an LDAP realm (its kind is oidc)"
        );
    }

    #[test]
    fn realm_show_lines_for_oidc() {
        assert_eq!(
            realm_lines(&sample_oidc_realm()).join("\n"),
            "name: okta\n\
             display: Okta\n\
             kind: oidc\n\
             issuer: https://idp.example.com\n\
             client id: oplite\n\
             client secret: set\n\
             scopes: openid profile email\n\
             username claim: preferred_username\n\
             groups claim: groups\n\
             ca: system roots\n\
             timeout: 10s\n\
             callback path: /api/v1/auth/oidc/callback\n\
             groups:\n  opl-admins = admin\n\
             default role: viewer"
        );
        let mut public = sample_oidc_realm();
        public["backend"]["has_client_secret"] = json!(false);
        public["backend"]["ca_pem"] = json!("PEM");
        let lines = realm_lines(&public);
        for line in ["client secret: none", "ca: custom (PEM)"] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// OIDC 영역 명령이 부르는 API: 목록의 URL 칸은 issuer, 파일의 비밀·CA는 본문에,
    /// `set-oidc`는 읽은 revision을 If-Match로, `test`는 영역 종류를 읽고 입력 없이 단계 표만 보인다
    /// (`--user`를 줘도 비밀번호를 묻지 않고 DN·그룹·역할 줄이 없다).
    #[tokio::test]
    async fn realm_oidc_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_oidc_realm()] }),
            "/api/v1/realms/okta/actions/test" => json!({
                "steps": [
                    { "name": "discovery", "ok": true, "message": "read https://idp.example.com/.well-known/openid-configuration" },
                    { "name": "jwks", "ok": true, "message": "2 signing keys (RS256 1, ES256 1)" }
                ],
                "dn": null, "groups": [], "role": null
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "discovery", "ok": false, "message": "issuer differs" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_oidc_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-oidc",
                "okta",
                "--issuer",
                "https://idp.example.com",
                "--client-id",
                "oplite",
                "--client-secret-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-oidc", "okta", "--client-secret-file", secret][..],
            &["test", "okta", "--user", "alice"][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the discovery step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/okta HTTP/1.1",
                "PUT /api/v1/realms/okta HTTP/1.1",
                "GET /api/v1/realms/okta HTTP/1.1",
                "POST /api/v1/realms/okta/actions/test HTTP/1.1",
                "GET /api/v1/realms/broken HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["kind"], "oidc");
        assert_eq!(seen[1].body["backend"]["client_secret"], "s3cret");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("9"));
        assert_eq!(seen[3].body["backend"]["client_secret"], "s3cret");
        assert!(seen[3].body["backend"].get("has_client_secret").is_none());
        assert_eq!(seen[5].body, json!({}));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             okta  Okta     oidc  https://idp.example.com\n\
             realm okta created\n\
             realm okta updated\n\
             STEP       RESULT  MESSAGE\n\
             discovery  ok      read https://idp.example.com/.well-known/openid-configuration\n\
             jwks       ok      2 signing keys (RS256 1, ES256 1)\n"
        );
    }

    /// `login --realm <OIDC 영역>`은 비밀번호를 묻기 전에 웹 UI로 보낸다.
    #[tokio::test]
    async fn login_refuses_an_oidc_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/realms" => json!([
                { "name": "local", "display": null, "kind": "local" },
                { "name": "corp", "display": null, "kind": "ldap" },
                { "name": "okta", "display": "Okta", "kind": "oidc" }
            ]),
            _ => json!({}),
        })
        .await;
        // 비밀번호 파일이 없으므로 묻는다면 터미널이 없어 다른 오류가 난다.
        let Cmd::Login(args) = Cli::try_parse_from(["oplctl", "login", "--realm", "okta"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let e = login(&base, &args, None).await.unwrap_err();
        assert_eq!(
            e.to_string(),
            "sign in to okta in the web UI, create an API token there and run `oplctl login --token-file`"
        );
        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["GET /api/v1/auth/realms HTTP/1.1"]);
    }

    /// `login --token-file`은 비밀번호 로그인 인자와 함께 줄 수 없다.
    #[test]
    fn token_file_login_conflicts_with_password_login() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["oplctl", "login", "--token-file", "t"];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args).map(|_| ())
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--ca-fingerprint", "sha256:ab"]).is_ok());
        for extra in [
            &["--user", "bob"][..],
            &["--realm", "okta"][..],
            &["--password-file", "pw"][..],
        ] {
            let e = parse(extra).unwrap_err();
            assert_eq!(
                e.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{extra:?}"
            );
        }
    }

    /// `login --token-file`: 토큰으로 whoami를 불러 확인하고 저장한다. 같은 토큰을 다시 넣으면 지우지 않고,
    /// 이전 토큰은 login이 만든 것(`oplctl-…`)만 지운다. 받지 않는 토큰은 저장하지 않는다.
    #[tokio::test]
    async fn token_file_login_checks_and_saves_the_token() {
        let ids = [
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
        ];
        let tokens = ids.map(|id| format!("opl_{}_secret", id.simple()));
        let listed = json!({ "revision": 1, "items": [
            { "id": ids[0], "name": "laptop", "revision": 1, "user": ids[2] },
            { "id": ids[1], "name": "oplctl-1a2b3c4d", "revision": 1, "user": ids[2] },
        ]});
        let (client, seen) = fake_api_status(move |path| match path {
            "/api/v1/auth/whoami" => (
                200,
                json!({ "name": "alice@corp.com@okta", "role": "admin" }),
            ),
            "/api/v1/tokens" => (200, listed.clone()),
            _ => (200, json!({})),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let file = dir.path().join("token");
        let login_with = async |token: &str| {
            std::fs::write(&file, format!("{token}\n")).unwrap();
            let mut out = Vec::new();
            let args = [
                "oplctl",
                "--config",
                config.to_str().unwrap(),
                "--server",
                client.base.as_str(),
                "login",
                "--token-file",
                file.to_str().unwrap(),
            ];
            run(args, &mut out)
                .await
                .map(|()| String::from_utf8(out).unwrap())
        };
        let saved = || CliConfig::load(&config).unwrap().token;
        let lines = || std::mem::take(&mut *seen.lock().unwrap());

        // 웹 UI에서 만든 토큰(laptop)
        assert_eq!(
            login_with(&tokens[0]).await.unwrap(),
            format!("logged in to {} as alice@corp.com@okta\n", client.base)
        );
        assert_eq!(saved().as_deref(), Some(tokens[0].as_str()));
        assert_eq!(lines(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
        // 같은 토큰을 다시 넣어도 지우지 않는다.
        login_with(&tokens[0]).await.unwrap();
        assert_eq!(lines(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
        // 다른 토큰: 이전 토큰(laptop)은 login이 만든 것이 아니므로 두다.
        login_with(&tokens[1]).await.unwrap();
        assert_eq!(
            lines(),
            [
                "GET /api/v1/auth/whoami HTTP/1.1",
                "GET /api/v1/tokens HTTP/1.1"
            ]
        );
        // 이전 토큰(oplctl-…)은 지운다.
        login_with(&tokens[2]).await.unwrap();
        assert_eq!(
            lines(),
            [
                "GET /api/v1/auth/whoami HTTP/1.1".to_string(),
                "GET /api/v1/tokens HTTP/1.1".to_string(),
                format!("DELETE /api/v1/tokens/{} HTTP/1.1", ids[1]),
            ]
        );
        assert_eq!(saved().as_deref(), Some(tokens[2].as_str()));
    }

    /// 서버가 받지 않는 토큰(401)은 저장하지 않는다.
    #[tokio::test]
    async fn token_file_login_does_not_save_a_refused_token() {
        let (client, seen) = fake_api_status(|_| {
            (
                401,
                json!({ "code": "unauthorized", "message": "invalid token" }),
            )
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let file = dir.path().join("token");
        std::fs::write(&file, "opl_bad_token\n").unwrap();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "login",
            "--token-file",
            file.to_str().unwrap(),
        ];
        let e = run(args, &mut Vec::new()).await.unwrap_err();
        assert_eq!(e.to_string(), "the API token was not accepted");
        assert!(!config.exists());
        assert_eq!(*seen.lock().unwrap(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
    }

    /// 시험 서버 쪽에서 경로 조각의 퍼센트 인코딩을 푼다.
    fn percent_decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    /// `vm stop --force`는 동작 뒤의 쿼리를 그대로 보낸다(VM 이름만 인코딩).
    #[tokio::test]
    async fn vm_stop_force_keeps_the_query() {
        let (client, seen) =
            fake_api(|_| json!({ "id": "t1", "status": { "state": "succeeded" } })).await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let mut out = Vec::new();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "vm",
            "stop",
            "web 1",
            "--force",
        ];
        run(args, &mut out).await.unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "vm web 1: stop done\n");
        let lines = seen.lock().unwrap().clone();
        assert_eq!(
            lines[0],
            "POST /api/v1/vms/web%201/actions/stop?mode=force HTTP/1.1"
        );
    }

    /// 이름을 경로에 넣을 때 퍼센트 인코딩해, `+`·`@`가 든 OIDC 사용자 이름도 서버에서 같은 이름으로 풀린다.
    #[tokio::test]
    async fn user_delete_encodes_the_name_in_the_path() {
        let (client, seen) = fake_api(|_| json!({})).await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let name = "alice+ops@corp.com@okta";
        let mut out = Vec::new();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "user",
            "delete",
            name,
        ];
        run(args, &mut out).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("user {name} deleted\n")
        );
        let lines = seen.lock().unwrap().clone();
        assert_eq!(
            lines,
            ["DELETE /api/v1/users/alice%2Bops%40corp.com%40okta HTTP/1.1"]
        );
        let path = lines[0].split(' ').nth(1).unwrap();
        assert_eq!(
            percent_decode(path.strip_prefix("/api/v1/users/").unwrap()),
            name
        );
    }

    /// `logout`은 login이 만든 토큰(`oplctl-…`)만 서버에서 지운다. 웹 UI에서 만든 토큰은 로컬 설정에서만
    /// 지우고 서버에서는 그대로라고 알린다.
    #[tokio::test]
    async fn logout_deletes_only_tokens_made_by_login() {
        let ids = [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()];
        let tokens = ids.map(|id| format!("opl_{}_secret", id.simple()));
        let listed = json!({ "revision": 1, "items": [
            { "id": ids[0], "name": "laptop", "revision": 1, "user": ids[1] },
            { "id": ids[1], "name": "oplctl-1a2b3c4d", "revision": 1, "user": ids[1] },
        ]});
        let (client, seen) = fake_api_status(move |path| match path {
            "/api/v1/tokens" => (200, listed.clone()),
            _ => (200, json!({})),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let logout_with = async |token: &str| {
            CliConfig {
                server: Some(client.base.clone()),
                token: Some(token.into()),
                ca_pem: None,
            }
            .save(&config)
            .unwrap();
            let mut out = Vec::new();
            let args = ["oplctl", "--config", config.to_str().unwrap(), "logout"];
            run(args, &mut out).await.unwrap();
            assert_eq!(CliConfig::load(&config).unwrap().token, None);
            String::from_utf8(out).unwrap()
        };
        let lines = || std::mem::take(&mut *seen.lock().unwrap());

        // 웹 UI에서 만든 토큰(laptop)은 서버에서 지우지 않는다.
        assert_eq!(
            logout_with(&tokens[0]).await,
            "logged out\nthe token stays valid on the server; delete it in the web UI\n"
        );
        assert_eq!(lines(), ["GET /api/v1/tokens HTTP/1.1"]);
        // login이 만든 토큰은 지운다.
        assert_eq!(logout_with(&tokens[1]).await, "logged out\n");
        assert_eq!(
            lines(),
            [
                "GET /api/v1/tokens HTTP/1.1".to_string(),
                format!("DELETE /api/v1/tokens/{} HTTP/1.1", ids[1]),
            ]
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

`crates/oplctl/src/client.rs`에서

```rust
impl std::error::Error for ApiError {}
```

을 다음으로 바꾼다:

```rust
impl std::error::Error for ApiError {}

/// 이름·키를 API 경로의 한 조각으로 넣을 때 쓴다. RFC 3986 unreserved(영문·숫자·`-._~`) 밖은 UTF-8
/// 바이트마다 `%XX`로 바꾼다(OIDC 사용자 이름의 `+`·`@` 등이 서버에서 같은 이름으로 풀리게).
pub fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
```

`crates/oplctl/src/client.rs`에서

```rust
            let poll = self
                .request(Method::GET, &format!("/tasks/{id}"))
                .timeout(TASK_POLL_TIMEOUT);
```

을 다음으로 바꾼다:

```rust
            let poll = self
                .request(Method::GET, &format!("/tasks/{}", seg(id)))
                .timeout(TASK_POLL_TIMEOUT);
```

`crates/oplctl/src/client.rs`에서

```rust

    fn cert_pem(name: &str) -> String {
```

을 다음으로 바꾼다:

```rust

    /// 경로 조각은 RFC 3986 unreserved(영문·숫자·`-._~`) 밖을 모두 퍼센트 인코딩한다(UTF-8 바이트마다).
    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(seg("web-1.a_b~"), "web-1.a_b~");
        assert_eq!(
            seg("alice+ops@corp.com@okta"),
            "alice%2Bops%40corp.com%40okta"
        );
        assert_eq!(seg("a b/c?d#e%f"), "a%20b%2Fc%3Fd%23e%25f");
        assert_eq!(seg("é"), "%C3%A9");
        assert_eq!(seg(""), "");
    }

    fn cert_pem(name: &str) -> String {
```

`crates/oplctl/src/console.rs`에서

```rust

use crate::client::Client;
```

을 다음으로 바꾼다:

```rust

use crate::client::{Client, seg};
```

`crates/oplctl/src/console.rs`에서

```rust
    let url = client
        .url(&format!("/vms/{vm}/console"))
        .replacen("http", "ws", 1);
```

을 다음으로 바꾼다:

```rust
    let url = client
        .url(&format!("/vms/{}/console", seg(vm)))
        .replacen("http", "ws", 1);
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1413개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): OIDC 영역 명령 — realm add-oidc <이름> --issuer --client-id [--client-secret-file <파일|->] [--scope]... [--username-claim] [--groups-claim] [--ca-file] [--timeout] [--display] [--group <값>=<role>]... [--default-role](준 것만 본문에, 나머지는 서버 기본값), realm set-oidc <이름>(지금 영역을 읽어 준 것만 바꾸고 If-Match로 PUT, --scope·--group은 목록 전체를 바꿈, --clear-client-secret은 빈 글, --clear-ca·--clear-default-role·--clear-groups, 비밀을 주지 않으면 빼서 보내 옮기기 규칙은 서버가 판단). set-ldap·set-oidc는 종류가 다른 영역을 거절(\"realm <r> is not an OIDC realm (its kind is ldap)\"). realm show는 OIDC면 issuer·client id·client secret: set|none·scopes·클레임·ca·timeout과 callback path: /api/v1/auth/oidc/callback, realm list의 URL 칸은 OIDC면 issuer. realm test의 --user를 선택으로(--password-file은 --user와 함께) — 없으면 묻지 않고 빈 본문 {}로 보내 단계 표만 보임(LDAP이면 서버가 400). login --realm은 local이 아니면 비밀번호를 묻기 전에 GET /auth/realms를 읽어 OIDC 영역이면 \"sign in to <r> in the web UI and create an API token there\"로 실패(목록을 읽지 못하면 지나감). 공통 new_roles·changed_roles로 LDAP 본문도 정리. 시험: 명령 파싱, add-oidc·set-oidc 본문, show 줄, fake_api로 list·add·set·test 요청과 출력, OIDC 로그인 거절"
```

---

### Task 5: 웹: OIDC SSO 로그인과 OIDC 영역 화면

로그인 화면의 OIDC 영역 버튼("<이름>으로 로그인")과 `?error=` 문구, `/realms`의 LDAP/OIDC 고르기·OIDC 폼(client secret 규칙 검사)·이 노드의 리디렉트 URI 복사, OIDC 시험 결과. ko·en.

**Files:**
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/pages/LoginPage.tsx`
- Modify: `web/src/realms/RealmsPage.test.tsx`
- Modify: `web/src/realms/RealmsPage.tsx`
- Modify: `web/src/realms/realms.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.test.tsx`에서

```tsx

it("says when sign-in attempts are throttled", async () => {
```

을 다음으로 바꾼다:

```tsx

it("offers single sign-on for OIDC realms", async () => {
  signedOut();
  server.use(
    http.get(apiUrl("/auth/realms"), () =>
      HttpResponse.json([
        { name: "local", display: null, kind: "local" },
        { name: "corp", display: "Corp directory", kind: "ldap" },
        { name: "okta", display: "Okta", kind: "oidc" },
        { name: "keycloak", display: null, kind: "oidc" },
      ]),
    ),
  );
  window.history.pushState({}, "", "/login");
  render(<App />);
  // OIDC 영역은 고르기에 없고 IdP로 가는 링크다(전체 페이지 이동).
  const realm = await screen.findByLabelText("Realm");
  expect(
    within(realm)
      .getAllByRole("option")
      .map((o) => o.getAttribute("value")),
  ).toEqual(["local", "corp"]);
  expect(screen.getByText("or")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Sign in with Okta" })).toHaveAttribute(
    "href",
    "/api/v1/auth/oidc/okta/start",
  );
  expect(screen.getByRole("link", { name: "Sign in with keycloak" })).toHaveAttribute(
    "href",
    "/api/v1/auth/oidc/keycloak/start",
  );
});

it("offers no realm choice when the only other realms are OIDC", async () => {
  signedOut();
  server.use(
    http.get(apiUrl("/auth/realms"), () =>
      HttpResponse.json([
        { name: "local", display: null, kind: "local" },
        { name: "okta", display: "Okta", kind: "oidc" },
      ]),
    ),
  );
  window.history.pushState({}, "", "/login");
  render(<App />);
  expect(await screen.findByRole("link", { name: "Sign in with Okta" })).toBeInTheDocument();
  // 비밀번호로 로그인하는 영역이 로컬뿐이면 고르지 않는다.
  expect(screen.queryByLabelText("Realm")).not.toBeInTheDocument();
});

it("explains why single sign-on failed and clears the code from the address", async () => {
  signedOut();
  for (const [code, text] of [
    [
      "no_role",
      "Your account at the identity provider has no role in opLite. Ask an administrator to map one of your groups.",
    ],
    [
      "realm_unavailable",
      "Cannot reach the identity provider of this realm. Try again later or ask an administrator.",
    ],
    ["too_many_attempts", "Too many attempts. Try again later."],
    ["sso_failed", "Single sign-on failed. Try again, or ask an administrator to check the realm."],
    // 모르는 코드는 일반 실패로 보인다(주소의 글을 그대로 보이지 않는다).
    [
      "<b>boom</b>",
      "Single sign-on failed. Try again, or ask an administrator to check the realm.",
    ],
  ]) {
    window.history.pushState({}, "", `/login?next=%2Fvms&error=${encodeURIComponent(code!)}`);
    const { unmount } = render(<App />);
    expect(await screen.findByRole("alert")).toHaveTextContent(text!);
    // 다른 값(`next`)은 두고 오류 코드만 지운다.
    expect(window.location.pathname).toBe("/login");
    expect(window.location.search).toBe("?next=%2Fvms");
    unmount();
  }

  // 비밀번호로 다시 로그인하면 SSO 오류는 사라진다.
  window.history.pushState({}, "", "/login?error=sso_failed");
  const user = userEvent.setup();
  render(<App />);
  await screen.findByRole("alert");
  expect(screen.getByLabelText("Password")).not.toHaveAttribute("aria-invalid");
  await user.type(screen.getByLabelText("Username"), "admin");
  await user.type(screen.getByLabelText("Password"), "nope");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  await vi.waitFor(() =>
    expect(screen.getByRole("alert")).toHaveTextContent("Wrong username or password."),
  );
  expect(screen.getAllByRole("alert")).toHaveLength(1);
});

it("reads the session cookie with whoami when single sign-on lands on /", async () => {
  signedOut();
  // SSO 콜백이 세션 쿠키(SameSite=Strict)를 놓고 `/`로 보낸다. 앱은 같은 출처의 fetch로 whoami를 읽어
  // 쿠키가 붙게 한다(credentials가 omit이면 브라우저가 쿠키를 붙이지 않는다).
  const whoami: Request[] = [];
  server.use(
    http.get(apiUrl("/auth/whoami"), ({ request }) => {
      whoami.push(request);
      return HttpResponse.json({ ...admin, name: "alice@okta" });
    }),
  );
  window.history.pushState({}, "", "/");
  render(<App />);
  expect(await screen.findByRole("heading", { level: 1, name: "Dashboard" })).toBeInTheDocument();
  expect(window.location.pathname).toBe("/");
  expect(whoami.length).toBeGreaterThan(0);
  for (const request of whoami) {
    expect(new URL(request.url).origin).toBe(window.location.origin);
    expect(request.credentials).not.toBe("omit");
  }
  expect(screen.getByRole("button", { name: "Account (alice@okta)" })).toBeInTheDocument();
});

it("says when sign-in attempts are throttled", async () => {
```

`web/src/i18n/en.json`에서

```json
    "realm_unavailable": "Cannot reach the directory of this realm. Try again later or ask an administrator.",
    "external_user": "This user signs in through a realm. Change the password in the directory."
  },
```

을 다음으로 바꾼다:

```json
    "realm_unavailable": "Cannot reach the directory of this realm. Try again later or ask an administrator.",
    "external_user": "This user signs in through a realm. Change the password in the directory.",
    "sso_failed": "Single sign-on failed. Try again, or ask an administrator to check the realm."
  },
```

`web/src/i18n/en.json`에서

```json
    "realm": "Realm",
    "localRealm": "Local users"
  },
```

을 다음으로 바꾼다:

```json
    "realm": "Realm",
    "localRealm": "Local users",
    "or": "or",
    "sso": {
      "signIn": "Sign in with {{name}}",
      "no_role": "Your account at the identity provider has no role in opLite. Ask an administrator to map one of your groups.",
      "realm_unavailable": "Cannot reach the identity provider of this realm. Try again later or ask an administrator."
    }
  },
```

`web/src/i18n/en.json`에서

```json
  "realms": {
    "pageHint": "Users of a realm sign in with their directory account as <name>@<realm>. Their role comes from their groups at each sign-in.",
    "adminOnly": "Only administrators can manage realms.",
```

을 다음으로 바꾼다:

```json
  "realms": {
    "pageHint": "Users of a realm sign in with their directory (LDAP) or identity provider (OIDC) account as <name>@<realm>. Their role comes from their groups at each sign-in.",
    "adminOnly": "Only administrators can manage realms.",
```

`web/src/i18n/en.json`에서

```json
      "user_bind": "User bind",
      "groups": "Groups"
    },
```

을 다음으로 바꾼다:

```json
      "user_bind": "User bind",
      "groups": "Groups",
      "discovery": "Discovery",
      "jwks": "Signing keys"
    },
```

`web/src/i18n/en.json`에서

```json
      "timeout": "Use a whole number of seconds from 1 to 30.",
      "group": "Enter a group DN or remove the row."
    }
  },
```

을 다음으로 바꾼다:

```json
      "timeout": "Use a whole number of seconds from 1 to 30.",
      "group": "Enter a group DN or remove the row.",
      "issuer": "Use https://host[:port][/path] without a query or fragment.",
      "scopes": "Include openid.",
      "clientSecretAgain": "Enter the client secret again: the issuer, client ID or CA certificate changed."
    },
    "createOidcTitle": "Create an OIDC realm",
    "issuer": "Issuer",
    "issuerHint": "https:// URL exactly as the identity provider reports it, including any trailing slash.",
    "clientId": "Client ID",
    "clientSecret": "Client secret",
    "clientSecretHint": "Leave empty for a public client (PKCE only).",
    "clientSecretSaved": "A secret is saved. Leave this empty to keep it.",
    "clearClientSecret": "Clear the saved secret (public client)",
    "scopes": "Scopes",
    "scopesHint": "Separated by spaces. openid is required.",
    "usernameClaim": "Username claim",
    "groupsClaim": "Groups claim",
    "groupsClaimHint": "Claim with the user's groups as a list of strings.",
    "redirectUri": "Redirect URI",
    "redirectUriHint": "Register this address with the identity provider. Each node address you sign in through needs its own. Behind a reverse proxy, register the address the proxy passes as the Host.",
    "copyRedirectUri": "Copy the redirect URI",
    "oidcCaPemHint": "Trust only this CA for the identity provider's certificate. Leave empty to use the system roots.",
    "oidcGroupsHint": "Group values from the groups claim match without regard to case. A user in several groups gets the highest role.",
    "oidcTestDescription": "Checks the identity provider's discovery document and signing keys. No one signs in."
  },
```

`web/src/i18n/ko.json`에서

```json
    "realm_unavailable": "이 영역의 디렉터리에 연결할 수 없습니다. 잠시 뒤 다시 시도하거나 관리자에게 문의하세요.",
    "external_user": "영역으로 로그인하는 사용자입니다. 비밀번호는 디렉터리에서 바꾸세요."
  },
```

을 다음으로 바꾼다:

```json
    "realm_unavailable": "이 영역의 디렉터리에 연결할 수 없습니다. 잠시 뒤 다시 시도하거나 관리자에게 문의하세요.",
    "external_user": "영역으로 로그인하는 사용자입니다. 비밀번호는 디렉터리에서 바꾸세요.",
    "sso_failed": "SSO 로그인에 실패했습니다. 다시 시도하거나 관리자에게 영역 확인을 요청하세요."
  },
```

`web/src/i18n/ko.json`에서

```json
    "realm": "인증 영역",
    "localRealm": "로컬 사용자"
  },
```

을 다음으로 바꾼다:

```json
    "realm": "인증 영역",
    "localRealm": "로컬 사용자",
    "or": "또는",
    "sso": {
      "signIn": "{{name}}(으)로 로그인",
      "no_role": "IdP 계정에 opLite 역할이 없습니다. 관리자에게 그룹의 역할 매핑을 요청하세요.",
      "realm_unavailable": "이 영역의 IdP에 연결할 수 없습니다. 잠시 뒤 다시 시도하거나 관리자에게 문의하세요."
    }
  },
```

`web/src/i18n/ko.json`에서

```json
  "realms": {
    "pageHint": "영역의 사용자는 디렉터리 계정으로 로그인하고 <이름>@<영역>이 됩니다. 역할은 로그인할 때마다 그룹으로 정해집니다.",
    "adminOnly": "인증 영역은 관리자만 관리할 수 있습니다.",
```

을 다음으로 바꾼다:

```json
  "realms": {
    "pageHint": "영역의 사용자는 디렉터리(LDAP)나 IdP(OIDC) 계정으로 로그인하고 <이름>@<영역>이 됩니다. 역할은 로그인할 때마다 그룹으로 정해집니다.",
    "adminOnly": "인증 영역은 관리자만 관리할 수 있습니다.",
```

`web/src/i18n/ko.json`에서

```json
      "user_bind": "사용자 바인드",
      "groups": "그룹"
    },
```

을 다음으로 바꾼다:

```json
      "user_bind": "사용자 바인드",
      "groups": "그룹",
      "discovery": "Discovery",
      "jwks": "서명 키"
    },
```

`web/src/i18n/ko.json`에서

```json
      "timeout": "1~30 사이의 정수(초)로 쓰세요.",
      "group": "그룹 DN을 입력하거나 행을 빼세요."
    }
  },
```

을 다음으로 바꾼다:

```json
      "timeout": "1~30 사이의 정수(초)로 쓰세요.",
      "group": "그룹 DN을 입력하거나 행을 빼세요.",
      "issuer": "쿼리·조각 없이 https://호스트[:포트][/경로] 형식으로 쓰세요.",
      "scopes": "openid를 넣으세요.",
      "clientSecretAgain": "issuer·클라이언트 ID·CA 인증서를 바꿨으니 클라이언트 비밀을 다시 넣으세요."
    },
    "createOidcTitle": "OIDC 영역 만들기",
    "issuer": "Issuer",
    "issuerHint": "IdP가 알리는 그대로의 https:// URL입니다(끝의 / 포함).",
    "clientId": "클라이언트 ID",
    "clientSecret": "클라이언트 비밀",
    "clientSecretHint": "공개 클라이언트(PKCE만)면 비워 두세요.",
    "clientSecretSaved": "저장된 비밀이 있습니다. 비워 두면 그대로 둡니다.",
    "clearClientSecret": "저장된 비밀 지우기(공개 클라이언트)",
    "scopes": "Scope",
    "scopesHint": "공백으로 나눕니다. openid가 있어야 합니다.",
    "usernameClaim": "사용자 이름 클레임",
    "groupsClaim": "그룹 클레임",
    "groupsClaimHint": "사용자의 그룹을 문자열 목록으로 담은 클레임입니다.",
    "redirectUri": "리디렉트 URI",
    "redirectUriHint": "이 주소를 IdP에 등록하세요. 로그인에 쓰는 노드 주소마다 따로 등록해야 합니다. 리버스 프록시 뒤라면 프록시가 Host로 넘기는 주소를 등록하세요.",
    "copyRedirectUri": "리디렉트 URI 복사",
    "oidcCaPemHint": "IdP 인증서를 이 CA로만 확인합니다. 비우면 시스템 루트를 씁니다.",
    "oidcGroupsHint": "그룹 클레임의 값은 대소문자를 가리지 않고 비교합니다. 여러 그룹에 속하면 가장 높은 역할을 받습니다.",
    "oidcTestDescription": "IdP의 discovery 문서와 서명 키를 확인합니다. 아무도 로그인하지 않습니다."
  },
```

`web/src/pages/LoginPage.tsx` 전체:

```tsx
import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Navigate, useSearchParams } from "react-router";

import { api, call } from "@/api/client";
import { useAuth } from "@/auth/AuthProvider";
import { safeNext } from "@/auth/RequireAuth";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { codeMessage, errorMessage } from "@/i18n/errors";

/** SSO 콜백이 `/login?error=<code>`로 돌려보내는 코드(`oidc_login`). 모르는 코드는 `sso_failed`로 보인다. */
const SSO_ERRORS = ["sso_failed", "no_role", "realm_unavailable", "too_many_attempts"];

export function LoginPage() {
  const { t } = useTranslation();
  const { principal, login } = useAuth();
  const [params, setParams] = useSearchParams();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [realm, setRealm] = useState("local");
  const [error, setError] = useState<string | null>(null);
  // SSO가 실패해 돌아왔으면 그 까닭을 처음부터 보인다(비밀번호 입력의 오류가 아니다).
  const [ssoError, setSsoError] = useState<string | null>(() => {
    const code = params.get("error");
    if (code === null) {
      return null;
    }
    const known = SSO_ERRORS.includes(code) ? code : "sso_failed";
    return codeMessage(t, known, "", "login.sso");
  });
  const [busy, setBusy] = useState(false);
  // 서버 버전은 로그인 없이 읽는다. 읽지 못하면(옛 서버) 보이지 않는다.
  const version = useQuery({
    queryKey: ["version"],
    queryFn: () => call(api.GET("/version")),
    staleTime: Infinity,
    retry: false,
  });
  // 고를 영역도 로그인 없이 읽는다. 읽지 못하면(옛 서버) 로컬로만 로그인한다.
  const realms = useQuery({
    queryKey: ["login-realms"],
    queryFn: () => call(api.GET("/auth/realms")),
    retry: false,
  });
  // 비밀번호로 로그인하는 영역은 고르고, OIDC 영역은 IdP로 가는 버튼이다.
  const choices = (realms.data ?? []).filter((r) => r.kind !== "oidc");
  const sso = (realms.data ?? []).filter((r) => r.kind === "oidc");

  // 읽은 오류 코드는 주소에서 지운다(새로 고쳐도 다시 보이지 않게). `next` 같은 다른 값은 둔다.
  useEffect(() => {
    if (params.has("error")) {
      setParams(
        (p) => {
          p.delete("error");
          return p;
        },
        { replace: true },
      );
    }
  }, [params, setParams]);

  // 세션을 확인하는 동안에는 폼을 그리지 않는다(로그인돼 있으면 잠깐 보였다 사라진다).
  if (principal === undefined) {
    return null;
  }
  if (principal) {
    return <Navigate to={safeNext(params.get("next"))} replace />;
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setSsoError(null);
    try {
      await login(username, password, realm === "local" ? undefined : realm);
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-4 p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle className="text-xl">
            <h1>opLite</h1>
          </CardTitle>
        </CardHeader>
        <CardContent>
          {ssoError && (
            <p role="alert" className="mb-4 text-sm text-destructive">
              {ssoError}
            </p>
          )}
          <form onSubmit={submit} className="flex flex-col gap-4">
            {/* 영역이 로컬뿐이면 고르지 않는다. */}
            {choices.length > 1 && (
              <div className="flex flex-col gap-2">
                <Label htmlFor="realm">{t("login.realm")}</Label>
                <NativeSelect
                  id="realm"
                  className="w-full"
                  value={realm}
                  onChange={(e) => setRealm(e.target.value)}
                >
                  {choices.map((r) => (
                    <NativeSelectOption key={r.name} value={r.name}>
                      {r.kind === "local" ? t("login.localRealm") : (r.display ?? r.name)}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </div>
            )}
            <div className="flex flex-col gap-2">
              <Label htmlFor="username">{t("login.username")}</Label>
              <Input
                id="username"
                autoComplete="username"
                required
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="password">{t("login.password")}</Label>
              <Input
                id="password"
                type="password"
                autoComplete="current-password"
                required
                aria-invalid={error ? true : undefined}
                aria-describedby={error ? "login-error" : undefined}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </div>
            {error && (
              <p id="login-error" role="alert" className="text-sm text-destructive">
                {error}
              </p>
            )}
            <Button type="submit" disabled={busy}>
              {t("login.submit")}
            </Button>
          </form>
          {sso.length > 0 && (
            <div className="mt-4 flex flex-col gap-4">
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <span className="h-px flex-1 bg-border" />
                {t("login.or")}
                <span className="h-px flex-1 bg-border" />
              </div>
              {/* 전체 페이지로 이동한다(IdP에 갔다가 콜백이 `/`로 돌려보낸다). */}
              {sso.map((r) => (
                <Button key={r.name} variant="outline" asChild>
                  <a href={`/api/v1/auth/oidc/${encodeURIComponent(r.name)}/start`}>
                    {t("login.sso.signIn", { name: r.display ?? r.name })}
                  </a>
                </Button>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
      {version.data && (
        <p className="text-xs text-muted-foreground">
          {t("login.version", { version: version.data.version })}
        </p>
      )}
    </div>
  );
}
```

`web/src/realms/RealmsPage.test.tsx`에서

```tsx
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
});
```

을 다음으로 바꾼다:

```tsx
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
});

const OKTA = {
  id: "00000000-0000-0000-0000-0000000000c3",
  name: "okta",
  revision: 9,
  display: "Okta",
  backend: {
    kind: "oidc",
    issuer: "https://corp.okta.example/oauth2/default",
    client_id: "oplite",
    has_client_secret: true,
    scopes: ["openid", "profile", "email", "groups"],
    username_claim: "preferred_username",
    groups_claim: "groups",
    timeout_secs: 10,
  },
  roles: { groups: [{ group: "opl-admins", role: "admin" }] },
};

it("lists an OIDC realm with its issuer", async () => {
  realms([OKTA, CORP]);
  renderPage(<RealmsPage />);
  const table = await screen.findByRole("table", { name: "Realms" });
  const row = (await within(table).findByText("okta")).closest("tr")!;
  expect(
    within(row)
      .getAllByRole("cell")
      .slice(0, 4)
      .map((c) => c.textContent),
  ).toEqual(["okta", "Okta", "OIDC", "https://corp.okta.example/oauth2/default"]);
});

it("creates an OIDC realm and shows the redirect URI to register", async () => {
  const change = realms([CORP]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/realms"), async ({ request }) => {
      body = await request.json();
      change([CORP, OKTA]);
      return HttpResponse.json(OKTA, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await screen.findByText("corp");
  await user.click(screen.getByRole("button", { name: "Create" }));
  let dialog = await screen.findByRole("dialog", { name: "Create an LDAP realm" });
  await user.selectOptions(within(dialog).getByLabelText("Kind"), "oidc");
  dialog = await screen.findByRole("dialog", { name: "Create an OIDC realm" });
  expect(within(dialog).queryByLabelText("Base DN")).not.toBeInTheDocument();
  // 서버의 기본값으로 시작한다.
  expect(within(dialog).getByLabelText("Scopes")).toHaveValue("openid profile email");
  expect(within(dialog).getByLabelText("Username claim")).toHaveValue("preferred_username");
  expect(within(dialog).getByLabelText("Groups claim")).toHaveValue("groups");
  expect(within(dialog).getByLabelText("Timeout (seconds)")).toHaveValue("10");
  // 이 노드의 콜백 주소를 IdP에 등록하도록 보이고 복사할 수 있다.
  const uri = `${window.location.origin}/api/v1/auth/oidc/callback`;
  expect(within(dialog).getByLabelText("Redirect URI")).toHaveValue(uri);
  expect(within(dialog).getByLabelText("Redirect URI")).toHaveAttribute("readonly");
  await user.click(within(dialog).getByRole("button", { name: "Copy the redirect URI" }));
  expect(await navigator.clipboard.readText()).toBe(uri);

  // 서버 검증과 같은 검사: https issuer(쿼리 없음), client ID, openid scope, 클레임 이름.
  await user.type(within(dialog).getByLabelText("Name"), "okta");
  await user.type(within(dialog).getByLabelText("Issuer"), "http://corp.okta.example");
  await user.clear(within(dialog).getByLabelText("Scopes"));
  await user.type(within(dialog).getByLabelText("Scopes"), "profile email");
  await user.clear(within(dialog).getByLabelText("Groups claim"));
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(
    within(dialog).getByText("Use https://host[:port][/path] without a query or fragment."),
  ).toBeVisible();
  expect(within(dialog).getByLabelText("Client ID")).toHaveAccessibleDescription("Enter a value.");
  expect(within(dialog).getByText("Include openid.")).toBeVisible();
  expect(within(dialog).getByLabelText("Groups claim")).toBeInvalid();
  await user.clear(within(dialog).getByLabelText("Issuer"));
  await user.type(within(dialog).getByLabelText("Issuer"), "https://corp.okta.example/?x=1");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Issuer")).toBeInvalid();
  expect(body).toBeUndefined();

  await user.type(within(dialog).getByLabelText("Display name"), "Okta");
  await user.clear(within(dialog).getByLabelText("Issuer"));
  await user.type(
    within(dialog).getByLabelText("Issuer"),
    "https://corp.okta.example/oauth2/default",
  );
  await user.type(within(dialog).getByLabelText("Client ID"), "oplite");
  await user.type(within(dialog).getByLabelText("Client secret"), "client-secret");
  await user.clear(within(dialog).getByLabelText("Scopes"));
  await user.type(within(dialog).getByLabelText("Scopes"), " openid  profile email groups ");
  await user.type(within(dialog).getByLabelText("Groups claim"), "groups");
  await user.click(within(dialog).getByRole("button", { name: "Add group" }));
  await user.type(within(dialog).getByLabelText("Group 1"), "opl-admins");
  await user.selectOptions(within(dialog).getByLabelText("Role of group 1"), "admin");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));

  expect(await screen.findByText("Created realm okta.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "okta",
    display: "Okta",
    backend: {
      kind: "oidc",
      issuer: "https://corp.okta.example/oauth2/default",
      client_id: "oplite",
      client_secret: "client-secret",
      scopes: ["openid", "profile", "email", "groups"],
      username_claim: "preferred_username",
      groups_claim: "groups",
      timeout_secs: 10,
    },
    roles: { groups: [{ group: "opl-admins", role: "admin" }] },
  });
});

it("keeps, replaces or clears the client secret when editing", async () => {
  realms([OKTA]);
  const sent: { ifMatch: string | null; body: { backend: Record<string, unknown> } }[] = [];
  server.use(
    http.put(apiUrl("/realms/okta"), async ({ request }) => {
      sent.push({
        ifMatch: request.headers.get("If-Match"),
        body: (await request.json()) as { backend: Record<string, unknown> },
      });
      return HttpResponse.json(OKTA);
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  async function edit() {
    await user.click(await screen.findByRole("button", { name: "Edit okta" }));
    return screen.findByRole("dialog", { name: "Edit realm okta" });
  }
  async function save(dialog: HTMLElement) {
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  }
  const again = "Enter the client secret again: the issuer, client ID or CA certificate changed.";

  // 비워 두면 보내지 않아 저장된 비밀이 남는다. 종류는 고칠 수 없다.
  let dialog = await edit();
  expect(within(dialog).queryByLabelText("Kind")).not.toBeInTheDocument();
  expect(within(dialog).getByLabelText("Issuer")).toHaveValue(
    "https://corp.okta.example/oauth2/default",
  );
  expect(within(dialog).getByLabelText("Scopes")).toHaveValue("openid profile email groups");
  expect(within(dialog).getByLabelText("Client secret")).toHaveValue("");
  expect(within(dialog).getByText("A secret is saved. Leave this empty to keep it.")).toBeVisible();
  await save(dialog);
  expect(sent[0]!.ifMatch).toBe("9");
  expect(sent[0]!.body.backend).toEqual({
    kind: "oidc",
    issuer: "https://corp.okta.example/oauth2/default",
    client_id: "oplite",
    scopes: ["openid", "profile", "email", "groups"],
    username_claim: "preferred_username",
    groups_claim: "groups",
    timeout_secs: 10,
  });

  // issuer·client ID·CA 가운데 하나라도 바꾸면 비밀을 다시 넣어야 한다(서버의 400과 같은 규칙).
  for (const [label, value] of [
    ["Issuer", "https://other.example"],
    ["Client ID", "other"],
    ["CA certificate (PEM)", "-----BEGIN CERT"],
  ] as const) {
    dialog = await edit();
    await user.clear(within(dialog).getByLabelText(label));
    await user.type(within(dialog).getByLabelText(label), value);
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(within(dialog).getByText(again)).toBeVisible();
    expect(sent).toHaveLength(1);
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  }

  // 새 비밀과 함께면 보낸다.
  dialog = await edit();
  await user.clear(within(dialog).getByLabelText("Client ID"));
  await user.type(within(dialog).getByLabelText("Client ID"), "other");
  await user.type(within(dialog).getByLabelText("Client secret"), "new-secret");
  await save(dialog);
  expect(sent[1]!.body.backend).toMatchObject({ client_id: "other", client_secret: "new-secret" });

  // 지우기를 고르면 빈 글을 보낸다(공개 클라이언트). 입력은 막히고 바뀐 issuer도 괜찮다.
  dialog = await edit();
  await user.click(within(dialog).getByLabelText("Clear the saved secret (public client)"));
  expect(within(dialog).getByLabelText("Client secret")).toBeDisabled();
  await user.clear(within(dialog).getByLabelText("Issuer"));
  await user.type(within(dialog).getByLabelText("Issuer"), "https://other.example");
  await save(dialog);
  expect(sent[2]!.body.backend).toMatchObject({
    issuer: "https://other.example",
    client_secret: "",
  });
});

it("edits a public client without asking for a secret", async () => {
  const pub = { ...OKTA, backend: { ...OKTA.backend, has_client_secret: false } };
  realms([pub]);
  let body: { backend: Record<string, unknown> } | undefined;
  server.use(
    http.put(apiUrl("/realms/okta"), async ({ request }) => {
      body = (await request.json()) as { backend: Record<string, unknown> };
      return HttpResponse.json(pub);
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await user.click(await screen.findByRole("button", { name: "Edit okta" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit realm okta" });
  expect(within(dialog).getByText("Leave empty for a public client (PKCE only).")).toBeVisible();
  expect(
    within(dialog).queryByLabelText("Clear the saved secret (public client)"),
  ).not.toBeInTheDocument();
  await user.clear(within(dialog).getByLabelText("Issuer"));
  await user.type(within(dialog).getByLabelText("Issuer"), "https://other.example/");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
  expect(body!.backend.issuer).toBe("https://other.example/");
  expect(body!.backend).not.toHaveProperty("client_secret");
});

it("tests an OIDC realm without asking for a user", async () => {
  realms([OKTA]);
  const bodies: string[] = [];
  server.use(
    http.post(apiUrl("/realms/okta/actions/test"), async ({ request }) => {
      bodies.push(await request.text());
      return HttpResponse.json({
        steps: [
          { name: "discovery", ok: true, message: "issuer matches" },
          { name: "jwks", ok: false, message: "no RS256 or ES256 signing keys" },
        ],
        dn: null,
        groups: [],
        role: null,
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await user.click(await screen.findByRole("button", { name: "Test okta" }));
  const dialog = await screen.findByRole("dialog", { name: "Test realm okta" });
  expect(dialog).toHaveTextContent(
    "Checks the identity provider's discovery document and signing keys. No one signs in.",
  );
  expect(within(dialog).queryByLabelText("Username")).not.toBeInTheDocument();
  expect(within(dialog).queryByLabelText("Password")).not.toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Test" }));
  const steps = await within(dialog).findByRole("table", { name: "Steps" });
  expect(
    within(steps)
      .getAllByRole("row")
      .slice(1)
      .map((r) =>
        within(r)
          .getAllByRole("cell")
          .map((c) => c.textContent),
      ),
  ).toEqual([
    ["Discovery", "OK", "issuer matches"],
    ["Signing keys", "Failed", "no RS256 or ES256 signing keys"],
  ]);
  // 사용자가 없으므로 DN·그룹·역할은 보이지 않는다.
  expect(within(dialog).queryByText("No role: sign-in would be refused.")).not.toBeInTheDocument();
  // 본문 없이 부른다.
  expect(bodies).toEqual([""]);
});
```

`web/src/realms/RealmsPage.tsx` 전체:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FlaskConical, Pencil, Plus, Trash2, X } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Role } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { focusBack } from "@/components/focusBack";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import {
  checkForm,
  emptyForm,
  formOf,
  type RealmForm,
  type RealmTestResult,
  type RealmView,
  redirectUri,
  requestOf,
} from "@/realms/realms";

const ROLES: readonly Role[] = ["admin", "operator", "viewer"];

type Dialogs = { kind: "create" } | { kind: "edit" | "test"; realm: RealmView };

/**
 * 인증 영역: 목록과 LDAP·OIDC 영역 만들기·고치기·지우기·시험. admin만 다룬다. 서비스 계정 비밀번호가 있어
 * watch로 받지 않으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다.
 */
export function RealmsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const admin = can(me.role, "read", "realm");
  const queryClient = useQueryClient();
  const realms = useQuery({
    queryKey: ["realms"],
    queryFn: () => call(api.GET("/realms")),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
    enabled: admin,
  });
  const [dialog, setDialog] = useState<Dialogs | null>(null);
  const [deleting, setDeleting] = useState<RealmView | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const close = () => setDialog(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["realms"] });

  // 메뉴에 없지만 주소로 직접 들어올 수 있다. 관리자가 아니면 읽지 않고 안내한다.
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.realms")}</h1>
        <p className="text-sm text-muted-foreground">{t("realms.adminOnly")}</p>
      </div>
    );
  }

  const list = realms.data ?? [];
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="realms-title" className="text-2xl font-semibold">
          {t("nav.realms")}
        </h1>
        <Button
          disabled={!realms.isSuccess}
          onClick={(e) => {
            opener.current = e.currentTarget;
            setDialog({ kind: "create" });
          }}
        >
          <Plus />
          {t("common.create")}
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">{t("realms.pageHint")}</p>
      <Table aria-labelledby="realms-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("realms.display")}</TableHead>
            <TableHead>{t("realms.kind")}</TableHead>
            <TableHead>{t("realms.url")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={5} query={realms} empty={t("realms.empty")} />
          )}
          {list.map((realm) => (
            <TableRow key={realm.id}>
              <TableCell className="font-medium">{realm.name}</TableCell>
              <TableCell>{realm.display ?? "—"}</TableCell>
              <TableCell>{t(`realms.kinds.${realm.backend.kind}`)}</TableCell>
              <TableCell className="font-mono text-xs">
                {realm.backend.kind === "ldap" ? realm.backend.url : realm.backend.issuer}
              </TableCell>
              <TableCell className="text-right whitespace-nowrap">
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={t("realms.testNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDialog({ kind: "test", realm });
                  }}
                >
                  <FlaskConical />
                  {t("realms.test")}
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className="ml-2"
                  aria-label={t("realms.editNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDialog({ kind: "edit", realm });
                  }}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("common.deleteNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDeleting(realm);
                    setDeleteOpen(true);
                  }}
                >
                  <Trash2 />
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {(dialog?.kind === "create" || dialog?.kind === "edit") && (
        <RealmDialog
          realm={dialog.kind === "edit" ? dialog.realm : null}
          taken={list.map((r) => r.name)}
          returnFocus={opener}
          onClose={close}
          onDone={changed}
        />
      )}
      {dialog?.kind === "test" && (
        <TestDialog realm={dialog.realm} returnFocus={opener} onClose={close} />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("realms.deleteTitle", { name: deleting?.name })}
        description={t("realms.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const realm = deleting!;
          await call(api.DELETE("/realms/{key}", { params: { path: { key: realm.name } } }));
          toast.success(t("realms.deleted", { name: realm.name }));
          await changed();
        }}
      />
    </div>
  );
}

function RoleOptions() {
  const { t } = useTranslation();
  return ROLES.map((role) => (
    <NativeSelectOption key={role} value={role}>
      {t(`roles.${role}`)}
    </NativeSelectOption>
  ));
}

/** 영역 만들기(`realm`이 null — 종류를 고른다)와 고치기. 열 때만 그린다. */
function RealmDialog({
  realm,
  taken,
  returnFocus,
  onClose,
  onDone,
}: {
  realm: RealmView | null;
  /** 있는 영역 이름 */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState<RealmForm>(() => (realm ? formOf(realm) : emptyForm()));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<RealmForm>) => setForm((f) => ({ ...f, ...part }));
  // 행을 빼면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const changeGroups = (groups: RealmForm["groups"]) => {
    change({ groups });
    setErrors({});
  };
  const saved = (realm?.backend.kind === "ldap" && realm.backend.has_bind_password) || false;
  const savedSecret = (realm?.backend.kind === "oidc" && realm.backend.has_client_secret) || false;
  const oidc = form.kind === "oidc";

  async function submit() {
    const found = checkForm(form, realm ? null : taken, realm);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const body = requestOf(form, realm !== null);
    if (realm) {
      await call(
        api.PUT("/realms/{key}", {
          params: { path: { key: realm.name }, header: { "If-Match": realm.revision } },
          body,
        }),
      );
      toast.success(t("realms.saved", { name: realm.name }));
    } else {
      await call(api.POST("/realms", { body }));
      toast.success(t("realms.created", { name: form.name }));
    }
    await onDone();
  }

  const text = (id: string, key: keyof RealmForm, label: string, hint?: string) => (
    <Field id={id} label={label} error={errors[key]} hint={hint}>
      {(a11y) => (
        <Input
          id={id}
          autoComplete="off"
          spellCheck={false}
          value={form[key] as string}
          onChange={(e) => change({ [key]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );

  return (
    <FormDialog
      title={
        realm
          ? t("realms.editTitle", { name: realm.name })
          : t(oidc ? "realms.createOidcTitle" : "realms.createTitle")
      }
      description={t("realms.formDescription")}
      submitLabel={realm ? t("common.save") : t("common.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {/* 항목이 많으므로 대화상자 안에서 넘긴다. */}
      <div className="-mx-1 flex max-h-[60svh] flex-col gap-4 overflow-y-auto px-1">
        {/* 종류는 만들 때만 고른다. */}
        {!realm && (
          <Field id="realm-kind" label={t("realms.kind")}>
            <NativeSelect
              id="realm-kind"
              className="w-full"
              value={form.kind}
              onChange={(e) => {
                change({ kind: e.target.value as RealmForm["kind"] });
                setErrors({});
              }}
            >
              <NativeSelectOption value="ldap">{t("realms.kinds.ldap")}</NativeSelectOption>
              <NativeSelectOption value="oidc">{t("realms.kinds.oidc")}</NativeSelectOption>
            </NativeSelect>
          </Field>
        )}
        <Field id="realm-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="realm-name"
              autoComplete="off"
              readOnly={realm !== null}
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        {text("realm-display", "display", t("realms.display"), t("realms.displayHint"))}
        {oidc ? (
          <>
            {text("realm-issuer", "issuer", t("realms.issuer"), t("realms.issuerHint"))}
            {text("realm-client-id", "clientId", t("realms.clientId"))}
            <Field
              id="realm-client-secret"
              label={t("realms.clientSecret")}
              error={errors.clientSecret}
              hint={t(savedSecret ? "realms.clientSecretSaved" : "realms.clientSecretHint")}
            >
              {(a11y) => (
                <Input
                  id="realm-client-secret"
                  type="password"
                  autoComplete="new-password"
                  disabled={form.clearClientSecret}
                  value={form.clientSecret}
                  onChange={(e) => change({ clientSecret: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            {savedSecret && (
              <div className="flex items-center gap-2">
                <Checkbox
                  id="realm-clear-secret"
                  checked={form.clearClientSecret}
                  onCheckedChange={(on) => change({ clearClientSecret: on === true })}
                />
                <Label htmlFor="realm-clear-secret">{t("realms.clearClientSecret")}</Label>
              </div>
            )}
            {text("realm-scopes", "scopes", t("realms.scopes"), t("realms.scopesHint"))}
            {text("realm-username-claim", "usernameClaim", t("realms.usernameClaim"))}
            {text(
              "realm-groups-claim",
              "groupsClaim",
              t("realms.groupsClaim"),
              t("realms.groupsClaimHint"),
            )}
            <Field
              id="realm-redirect-uri"
              label={t("realms.redirectUri")}
              hint={t("realms.redirectUriHint")}
            >
              {(a11y) => (
                <div className="flex items-center gap-2">
                  <Input
                    id="realm-redirect-uri"
                    readOnly
                    className="font-mono text-xs"
                    value={redirectUri()}
                    onFocus={(e) => e.target.select()}
                    {...a11y}
                  />
                  <CopyButton text={redirectUri()} label={t("realms.copyRedirectUri")} />
                </div>
              )}
            </Field>
          </>
        ) : (
          <>
            {text("realm-url", "url", t("realms.url"), t("realms.urlHint"))}
            {text("realm-base-dn", "baseDn", t("realms.baseDn"))}
            {text("realm-bind-dn", "bindDn", t("realms.bindDn"), t("realms.bindDnHint"))}
            <Field
              id="realm-bind-password"
              label={t("realms.bindPassword")}
              error={errors.bindPassword}
              hint={saved ? t("realms.bindPasswordSaved") : undefined}
            >
              {(a11y) => (
                <Input
                  id="realm-bind-password"
                  type="password"
                  autoComplete="new-password"
                  disabled={form.clearBindPassword}
                  value={form.bindPassword}
                  onChange={(e) => change({ bindPassword: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            {saved && (
              <div className="flex items-center gap-2">
                <Checkbox
                  id="realm-clear-password"
                  checked={form.clearBindPassword}
                  onCheckedChange={(on) => change({ clearBindPassword: on === true })}
                />
                <Label htmlFor="realm-clear-password">{t("realms.clearBindPassword")}</Label>
              </div>
            )}
            {text(
              "realm-user-filter",
              "userFilter",
              t("realms.userFilter"),
              t("realms.userFilterHint"),
            )}
            {text("realm-group-attr", "groupAttr", t("realms.groupAttr"))}
          </>
        )}
        <Field
          id="realm-ca"
          label={t("realms.caPem")}
          hint={t(oidc ? "realms.oidcCaPemHint" : "realms.caPemHint")}
        >
          {(a11y) => (
            <textarea
              id="realm-ca"
              rows={3}
              spellCheck={false}
              autoComplete="off"
              value={form.caPem}
              onChange={(e) => change({ caPem: e.target.value })}
              className="w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 font-mono text-xs break-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30"
              {...a11y}
            />
          )}
        </Field>
        <Field id="realm-timeout" label={t("realms.timeout")} error={errors.timeout}>
          {(a11y) => (
            <Input
              id="realm-timeout"
              inputMode="numeric"
              value={form.timeout}
              onChange={(e) => change({ timeout: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1.5 text-sm font-medium">{t("realms.groups")}</legend>
          <p className="text-xs text-muted-foreground">
            {t(oidc ? "realms.oidcGroupsHint" : "realms.groupsHint")}
          </p>
          {form.groups.map((row, i) => {
            const n = i + 1;
            const error = errors[`group-${i}`];
            const errorId = `realm-group-${i}-error`;
            return (
              <div key={i} className="flex flex-col gap-1">
                <div className="flex items-center gap-2">
                  <Input
                    className="min-w-0 flex-1"
                    aria-label={t("realms.groupN", { n })}
                    aria-invalid={error ? true : undefined}
                    aria-describedby={error ? errorId : undefined}
                    spellCheck={false}
                    placeholder={oidc ? "opl-operators" : "cn=ops,ou=groups,dc=example,dc=com"}
                    value={row.group}
                    onChange={(e) =>
                      changeGroups(
                        form.groups.map((g, j) => (j === i ? { ...g, group: e.target.value } : g)),
                      )
                    }
                  />
                  {/* 역할 이름이 다 보이게 내용 너비를 지킨다. */}
                  <NativeSelect
                    className="min-w-28 shrink-0"
                    aria-label={t("realms.groupRoleN", { n })}
                    value={row.role}
                    onChange={(e) =>
                      changeGroups(
                        form.groups.map((g, j) =>
                          j === i ? { ...g, role: e.target.value as Role } : g,
                        ),
                      )
                    }
                  >
                    <RoleOptions />
                  </NativeSelect>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("realms.removeGroupN", { n })}
                    onClick={() => changeGroups(form.groups.filter((_, j) => j !== i))}
                  >
                    <X />
                  </Button>
                </div>
                {error && (
                  <p id={errorId} className="text-xs text-destructive">
                    {t(error)}
                  </p>
                )}
              </div>
            );
          })}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            onClick={() => changeGroups([...form.groups, { group: "", role: "viewer" }])}
          >
            <Plus />
            {t("realms.addGroup")}
          </Button>
        </fieldset>
        <Field id="realm-default-role" label={t("realms.defaultRole")}>
          <NativeSelect
            id="realm-default-role"
            className="w-full"
            value={form.defaultRole}
            onChange={(e) => change({ defaultRole: e.target.value as Role | "" })}
          >
            <NativeSelectOption value="">{t("realms.noDefaultRole")}</NativeSelectOption>
            <RoleOptions />
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}

/**
 * 영역 시험: LDAP은 디렉터리 사용자로 단계마다, OIDC는 입력 없이 IdP의 discovery와 키를 확인한다. 사용자·세션을
 * 만들지 않는다. 결과를 보이도록 시험한 뒤에도 열어 둔다.
 */
function TestDialog({
  realm,
  returnFocus,
  onClose,
}: {
  realm: RealmView;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<RealmTestResult | null>(null);
  const oidc = realm.backend.kind === "oidc";

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      setResult(
        await call(
          api.POST("/realms/{key}/actions/test", {
            params: { path: { key: realm.name } },
            ...(!oidc && { body: { username, password } }),
          }),
        ),
      );
    } catch (err) {
      setResult(null);
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className="sm:max-w-2xl" onCloseAutoFocus={(e) => focusBack(e, returnFocus)}>
        <form onSubmit={(e) => void submit(e)} className="flex min-w-0 flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{t("realms.testTitle", { name: realm.name })}</DialogTitle>
            <DialogDescription>
              {t(oidc ? "realms.oidcTestDescription" : "realms.testDescription")}
            </DialogDescription>
          </DialogHeader>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          {!oidc && (
            <fieldset disabled={busy} className="grid gap-4 sm:grid-cols-2">
              <Field id="realm-test-username" label={t("login.username")}>
                <Input
                  id="realm-test-username"
                  autoComplete="off"
                  required
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                />
              </Field>
              <Field id="realm-test-password" label={t("login.password")}>
                <Input
                  id="realm-test-password"
                  type="password"
                  autoComplete="off"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </Field>
            </fieldset>
          )}
          {result && <TestResult result={result} oidc={oidc} />}
          <DialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
              {t("common.done")}
            </Button>
            <Button type="submit" disabled={busy || (!oidc && !username)}>
              {t("realms.test")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** 단계 표. LDAP이면 찾은 DN·그룹·정해질 역할도 보인다(OIDC는 사용자가 없다). */
function TestResult({ result, oidc }: { result: RealmTestResult; oidc: boolean }) {
  const { t } = useTranslation();
  return (
    <div className="flex min-w-0 flex-col gap-3">
      <Table aria-label={t("realms.steps")}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("realms.step")}</TableHead>
            <TableHead>{t("realms.result")}</TableHead>
            <TableHead>{t("realms.message")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {result.steps.map((step) => (
            <TableRow key={step.name}>
              <TableCell>
                {t(`realms.stepNames.${step.name}`, { defaultValue: step.name })}
              </TableCell>
              <TableCell className={step.ok ? "" : "text-destructive"}>
                {t(step.ok ? "realms.ok" : "realms.failed")}
              </TableCell>
              <TableCell className="whitespace-normal break-words">{step.message}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {!oidc && (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">{t("realms.dn")}</dt>
          <dd className={result.dn ? "font-mono text-xs break-all" : undefined}>
            {result.dn ?? "—"}
          </dd>
          <dt className="text-muted-foreground">{t("realms.groupsFound")}</dt>
          <dd>
            {result.groups.length === 0 ? (
              "—"
            ) : (
              <ul className="font-mono text-xs break-all">
                {result.groups.map((g) => (
                  <li key={g}>{g}</li>
                ))}
              </ul>
            )}
          </dd>
          <dt className="text-muted-foreground">{t("users.role")}</dt>
          <dd>{result.role ? t(`roles.${result.role}`) : t("realms.noRole")}</dd>
        </dl>
      )}
    </div>
  );
}
```

`web/src/realms/realms.ts` 전체:

```ts
import type { components } from "@/api/schema";
import type { Role } from "@/api/types";
import { validName } from "@/vms/form";

type Schemas = components["schemas"];

export type RealmView = Schemas["RealmView"];
export type RealmRequest = Schemas["RealmRequest"];
export type RealmTestResult = Schemas["RealmTestResult"];

/** 서버의 기본값과 같다(`opl_types::realm`). */
export const DEFAULT_USER_FILTER = "(&(objectClass=person)(uid={username}))";
export const DEFAULT_GROUP_ATTR = "memberOf";
export const DEFAULT_TIMEOUT_SECS = 10;
export const DEFAULT_SCOPES = "openid profile email";
export const DEFAULT_USERNAME_CLAIM = "preferred_username";
export const DEFAULT_GROUPS_CLAIM = "groups";

/** IdP에 등록할 이 노드의 리디렉트 URI(`oidc_login::CALLBACK_PATH`) */
export function redirectUri(): string {
  return `${window.location.origin}/api/v1/auth/oidc/callback`;
}

export interface GroupRow {
  group: string;
  role: Role;
}

/** 영역 만들기·고치기 폼. 입력 그대로의 글이다. 종류에 맞지 않는 칸은 쓰지 않는다. */
export interface RealmForm {
  kind: "ldap" | "oidc";
  name: string;
  display: string;
  url: string;
  baseDn: string;
  bindDn: string;
  /** 새 비밀번호. 고칠 때 비우면 저장된 값을 그대로 둔다. */
  bindPassword: string;
  /** 고칠 때 저장된 비밀번호를 지운다. */
  clearBindPassword: boolean;
  userFilter: string;
  groupAttr: string;
  issuer: string;
  clientId: string;
  /** 새 클라이언트 비밀. 고칠 때 비우면 저장된 값을 그대로 둔다. */
  clientSecret: string;
  /** 고칠 때 저장된 클라이언트 비밀을 지운다(공개 클라이언트). */
  clearClientSecret: boolean;
  /** 공백으로 나눈 scope */
  scopes: string;
  usernameClaim: string;
  groupsClaim: string;
  caPem: string;
  timeout: string;
  groups: GroupRow[];
  /** 빈 글이면 기본 역할이 없다(맞는 그룹이 없으면 로그인을 거절한다). */
  defaultRole: Role | "";
}

export function emptyForm(kind: RealmForm["kind"] = "ldap"): RealmForm {
  return {
    kind,
    name: "",
    display: "",
    url: "",
    baseDn: "",
    bindDn: "",
    bindPassword: "",
    clearBindPassword: false,
    userFilter: DEFAULT_USER_FILTER,
    groupAttr: DEFAULT_GROUP_ATTR,
    issuer: "",
    clientId: "",
    clientSecret: "",
    clearClientSecret: false,
    scopes: DEFAULT_SCOPES,
    usernameClaim: DEFAULT_USERNAME_CLAIM,
    groupsClaim: DEFAULT_GROUPS_CLAIM,
    caPem: "",
    timeout: String(DEFAULT_TIMEOUT_SECS),
    groups: [],
    defaultRole: "",
  };
}

export function formOf(realm: RealmView): RealmForm {
  // 다른 종류의 칸은 기본값으로 둔다.
  const ldap = realm.backend.kind === "ldap" ? realm.backend : null;
  const oidc = realm.backend.kind === "oidc" ? realm.backend : null;
  return {
    kind: realm.backend.kind,
    name: realm.name,
    display: realm.display ?? "",
    url: ldap?.url ?? "",
    baseDn: ldap?.base_dn ?? "",
    bindDn: ldap?.bind_dn ?? "",
    bindPassword: "",
    clearBindPassword: false,
    userFilter: ldap?.user_filter ?? DEFAULT_USER_FILTER,
    groupAttr: ldap?.group_attr ?? DEFAULT_GROUP_ATTR,
    issuer: oidc?.issuer ?? "",
    clientId: oidc?.client_id ?? "",
    clientSecret: "",
    clearClientSecret: false,
    scopes: oidc?.scopes.join(" ") ?? DEFAULT_SCOPES,
    usernameClaim: oidc?.username_claim ?? DEFAULT_USERNAME_CLAIM,
    groupsClaim: oidc?.groups_claim ?? DEFAULT_GROUPS_CLAIM,
    caPem: (ldap ?? oidc)?.ca_pem ?? "",
    timeout: String((ldap ?? oidc)?.timeout_secs ?? DEFAULT_TIMEOUT_SECS),
    groups: (realm.roles.groups ?? []).map((g) => ({ ...g })),
    defaultRole: realm.roles.default ?? "",
  };
}

/**
 * 서버의 `RealmSpec::validate`와 PUT의 비밀번호·클라이언트 비밀 규칙과 같은 검사. 값은 번역 키다. 그룹 행의 오류는
 * `group-<번호>`. `realm`은 고칠 때의 지금 영역이다.
 */
export function checkForm(
  form: RealmForm,
  taken: string[] | null,
  realm: RealmView | null = null,
): Record<string, string> {
  const errors: Record<string, string> = {};
  // 만들 때만 이름을 본다(`taken`이 있으면 만들기).
  if (taken) {
    if (!validName(form.name)) {
      errors.name = "forms.name";
    } else if (form.name === "local") {
      errors.name = "realms.errors.local";
    } else if (taken.includes(form.name)) {
      errors.name = "realms.errors.taken";
    }
  }
  if (form.kind === "oidc") {
    checkOidc(form, realm, errors);
  } else {
    checkLdap(form, realm, errors);
  }
  const timeout = Number(form.timeout);
  if (!/^\d+$/.test(form.timeout.trim()) || timeout < 1 || timeout > 30) {
    errors.timeout = "realms.errors.timeout";
  }
  form.groups.forEach((g, i) => {
    if (!g.group.trim()) {
      errors[`group-${i}`] = "realms.errors.group";
    }
  });
  return errors;
}

function checkLdap(form: RealmForm, realm: RealmView | null, errors: Record<string, string>) {
  if (!/^ldaps?:\/\/\[?[^/?:\]]+/.test(form.url.trim())) {
    errors.url = "realms.errors.url";
  }
  if (!form.baseDn.trim()) {
    errors.baseDn = "realms.errors.required";
  }
  if (form.userFilter.split("{username}").length !== 2) {
    errors.userFilter = "realms.errors.userFilter";
  }
  if (!form.groupAttr.trim()) {
    errors.groupAttr = "realms.errors.required";
  }
  const bindDn = form.bindDn.trim();
  const saved = (realm?.backend.kind === "ldap" && realm.backend.has_bind_password) || false;
  if (form.bindPassword && !form.clearBindPassword && !bindDn) {
    errors.bindPassword = "realms.errors.bindDn";
  }
  if (form.clearBindPassword && bindDn) {
    // 바인드 DN에는 비밀번호가 있어야 한다.
    errors.bindDn = "realms.errors.clearNeedsNoBindDn";
  } else if (bindDn && !form.bindPassword && !saved) {
    errors.bindPassword = "realms.errors.bindPasswordNeeded";
  } else if (!bindDn && saved && !form.clearBindPassword && !form.bindPassword) {
    // 바인드 DN을 비우면 익명 검색이므로 저장된 비밀번호를 지워야 한다(다시 넣을 것이 아니다).
    errors.bindPassword = "realms.errors.clearSaved";
  } else if (
    realm?.backend.kind === "ldap" &&
    saved &&
    !form.clearBindPassword &&
    !form.bindPassword
  ) {
    // 저장된 비밀번호를 다른 서버·계정으로 보내지 않는다(서버도 400).
    const ldap = realm.backend;
    if (
      form.url.trim() !== ldap.url ||
      bindDn !== (ldap.bind_dn ?? "") ||
      form.caPem.trim() !== (ldap.ca_pem ?? "")
    ) {
      errors.bindPassword = "realms.errors.bindPasswordAgain";
    }
  }
}

/** `https://`로 시작하고 호스트가 있으며 쿼리·조각·사용자 정보·공백이 없는지(서버의 `https_url_has_host`) */
function validIssuer(issuer: string): boolean {
  if (!issuer.startsWith("https://") || /[?#\s]/.test(issuer)) {
    return false;
  }
  const authority = issuer.slice("https://".length).split("/")[0]!;
  if (authority.includes("@")) {
    return false;
  }
  const host = authority.startsWith("[")
    ? authority.slice(1).split("]")[0]!
    : authority.split(":")[0]!;
  return host !== "";
}

function checkOidc(form: RealmForm, realm: RealmView | null, errors: Record<string, string>) {
  const issuer = form.issuer.trim();
  const clientId = form.clientId.trim();
  if (!validIssuer(issuer)) {
    errors.issuer = "realms.errors.issuer";
  }
  if (!clientId) {
    errors.clientId = "realms.errors.required";
  }
  if (!form.scopes.trim().split(/\s+/).includes("openid")) {
    errors.scopes = "realms.errors.scopes";
  }
  if (!form.usernameClaim.trim()) {
    errors.usernameClaim = "realms.errors.required";
  }
  if (!form.groupsClaim.trim()) {
    errors.groupsClaim = "realms.errors.required";
  }
  // 저장된 비밀을 다른 IdP·클라이언트로 보내지 않는다(서버도 400). 저장된 비밀이 없으면(공개 클라이언트) 해당 없다.
  if (
    realm?.backend.kind === "oidc" &&
    realm.backend.has_client_secret &&
    !form.clearClientSecret &&
    !form.clientSecret &&
    (issuer !== realm.backend.issuer ||
      clientId !== realm.backend.client_id ||
      form.caPem.trim() !== (realm.backend.ca_pem ?? ""))
  ) {
    errors.clientSecret = "realms.errors.clientSecretAgain";
  }
}

/**
 * 폼을 요청 본문으로. 고칠 때(`editing`) 비밀번호·클라이언트 비밀을 비워 두면 그 필드를 빼서 서버가 저장된 값을
 * 두고, 지우기를 골랐으면 빈 글을 보낸다.
 */
export function requestOf(form: RealmForm, editing: boolean): RealmRequest {
  const display = form.display.trim();
  return {
    ...(!editing && { name: form.name }),
    ...(display && { display }),
    backend: form.kind === "oidc" ? oidcBackend(form, editing) : ldapBackend(form, editing),
    roles: {
      groups: form.groups.map((g) => ({ group: g.group.trim(), role: g.role })),
      ...(form.defaultRole && { default: form.defaultRole }),
    },
  };
}

function ldapBackend(form: RealmForm, editing: boolean): RealmRequest["backend"] {
  const bindDn = form.bindDn.trim();
  const password = form.clearBindPassword && editing ? "" : form.bindPassword || undefined;
  return {
    kind: "ldap",
    url: form.url.trim(),
    base_dn: form.baseDn.trim(),
    ...(bindDn && { bind_dn: bindDn }),
    ...(password !== undefined && { bind_password: password }),
    user_filter: form.userFilter.trim(),
    group_attr: form.groupAttr.trim(),
    ...(form.caPem.trim() && { ca_pem: form.caPem.trim() }),
    timeout_secs: Number(form.timeout),
  };
}

function oidcBackend(form: RealmForm, editing: boolean): RealmRequest["backend"] {
  const secret = form.clearClientSecret && editing ? "" : form.clientSecret || undefined;
  return {
    kind: "oidc",
    issuer: form.issuer.trim(),
    client_id: form.clientId.trim(),
    ...(secret !== undefined && { client_secret: secret }),
    scopes: form.scopes.trim().split(/\s+/),
    username_claim: form.usernameClaim.trim(),
    groups_claim: form.groupsClaim.trim(),
    ...(form.caPem.trim() && { ca_pem: form.caPem.trim() }),
    timeout_secs: Number(form.timeout),
  };
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web build`
Expected: 모두 통과

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): OIDC SSO 로그인과 OIDC 영역 화면 — 로그인 화면은 GET /auth/realms의 OIDC 영역마다 아이디·비밀번호 폼 아래 구분선 \"또는\" 뒤에 \"<표시 이름 또는 이름>(으)로 로그인\"을 /api/v1/auth/oidc/<이름>/start로 가는 전체 페이지 링크(버튼 모양)로 보이고, 영역 고르기에는 local·LDAP만(비밀번호 영역이 local뿐이면 고르기 없음). 주소의 ?error=<코드>를 읽어 입력 오류와 따로 알림으로 보이고(sso_failed·too_many_attempts는 errors.*, no_role·realm_unavailable은 IdP 문구 login.sso.*, 모르는 코드는 sso_failed) 주소에서는 error만 지움(react-router setSearchParams replace — next는 둠), 비밀번호로 다시 로그인하면 사라짐. /realms: 만들기 대화상자에서 종류(LDAP/OIDC)를 고르고(제목 \"OIDC 영역 만들기\"), OIDC 폼(issuer, 클라이언트 ID, 클라이언트 비밀 — 고칠 때 비우면 빼서 유지·\"저장된 비밀 지우기(공개 클라이언트)\"면 \"\", scope(공백으로 나눈 한 줄), 사용자 이름·그룹 클레임, CA PEM, 시간 제한, 그룹 → 역할(그룹 값은 클레임 문자열), 기본 역할)과 서버와 같은 검사(https issuer — 쿼리·조각·사용자 정보·공백 없음, openid scope, 빈 값, 1~30초, 저장된 비밀이 있으면 issuer·client_id·CA가 바뀔 때 비밀 다시 넣기), 이 노드의 리디렉트 URI(${location.origin}/api/v1/auth/oidc/callback)를 읽기 전용 칸과 복사 버튼으로. 고치기에서는 종류를 바꾸지 않음. 시험은 OIDC면 입력 없이 본문 없이 불러 discovery·서명 키 단계 표만(DN·그룹·역할 없음). ko·en. 시험: 로그인 화면 SSO 링크·고르기 목록·오류 코드 문구와 주소 정리, SSO 뒤 / 에서 같은 출처 whoami로 세션을 읽음, OIDC 목록·만들기·검사·리디렉트 URI 복사·비밀 유지·바꾸기·지우기·다시 넣기 규칙·공개 클라이언트·시험"
```

---

### Task 6: 문서

README 새 절 "SSO(OIDC)"(IdP 등록 — 리디렉트 URI, Keycloak·Okta·Entra ID 예, 흐름과 쿠키, ID 토큰 확인, 오류, `sub` 묶기, `oplctl login --token-file`, 한계), LDAP·감사 로그 절 갱신, 엔터프라이즈 설계 §4.5 "구현(계획 36)"과 §7·§9, 처음 분해 문서의 하위 프로젝트 10 줄.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-06-enterprise-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
```

을 다음으로 바꾼다:

```markdown
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
```

`README.md`에서

```markdown
인증 영역(`Realm`)을 만들면 로컬 사용자 말고도 바깥 LDAP 디렉터리(OpenLDAP, Active Directory 등)의 계정으로
로그인할 수 있다. 영역은 admin만 보고 만들고 고친다(operator·viewer는 읽기도 **403**). 영역 이름은 리소스 이름
규칙(1~63자의 영문 소문자·숫자·`-`)이고 `local`은 로컬 사용자를 뜻하는 예약 이름이다. UUID로 읽히는 이름(예:
`0192f0c1-5d2e-7000-8000-000000000000`)도 API가 id로 읽어 이름으로 부를 수 없으므로 **400**이다.
```

을 다음으로 바꾼다:

```markdown
인증 영역(`Realm`)을 만들면 로컬 사용자 말고도 바깥 LDAP 디렉터리(OpenLDAP, Active Directory 등)의 계정으로
로그인할 수 있다(OpenID Connect IdP로 로그인하는 OIDC 영역은 다음 절 "SSO(OIDC)"). 영역은 admin만 보고
만들고 고친다(operator·viewer는 읽기도 **403**). 영역 이름은 리소스 이름 규칙(1~63자의 영문 소문자·숫자·`-`)이고
`local`은 로컬 사용자를 뜻하는 예약 이름이다. UUID로 읽히는 이름(예: `0192f0c1-5d2e-7000-8000-000000000000`)도
API가 id로 읽어 이름으로 부를 수 없으므로 **400**이다.
```

`README.md`에서

```markdown
- 사용자 부분이 `a-z`·`0-9`·`.`·`_`·`-`가 아닌 계정(공백·`@`·한글 등)은 로그인할 수 없다. 사용자·그룹 동기화, 외부 사용자의
  2단계 인증, SSO(OIDC)는 없다.
```

을 다음으로 바꾼다:

````markdown
- 사용자 부분이 `a-z`·`0-9`·`.`·`_`·`-`가 아닌 계정(공백·`@`·한글 등)은 로그인할 수 없다. 사용자·그룹 동기화, 외부 사용자의
  2단계 인증은 없다. IdP로 로그인하는 영역은 아래 "SSO(OIDC)".

## SSO(OIDC)

OIDC 영역을 만들면 사용자가 OpenID Connect IdP(Keycloak, Okta, Microsoft Entra ID 등)에서 로그인하고 opLite로
돌아온다(인가 코드 흐름과 PKCE). 영역의 공통 규칙은 "외부 인증(LDAP)"과 같다: admin만 보고 만들고 고치며, 이름 규칙,
외부 사용자 `<사용자>@<영역>`, 로그인할 때마다 그룹 → 역할 매핑(앞뒤 공백을 떼고 ASCII 대소문자 무시, 가장 높은 역할,
기본 역할, 없으면 `no_role`), 역할이 바뀌면 그 사용자의 다른 세션이 끝남, 외부 사용자의 API 토큰, 영역을 지울 때가 모두
같다. 그룹 값은 그룹 클레임의 문자열이다. OIDC 영역 사용자는 비밀번호로 로그인하지 않는다: `POST /auth/login`에 OIDC
영역을 주면 이름 모양과 관계없이(이메일 모양 이름도) 늘 **400** `invalid_request`("realm okta signs in through its
identity provider" — 시도 제한에 세지 않음)다.

- **이름.** OIDC 영역의 사용자 부분은 이름 클레임을 소문자로 바꾼 것이고 1~128자의 `a-z`·`0-9`·`.`·`_`·`+`·`-`와 안쪽
  `@` 하나까지다(이메일 모양 — 앞뒤가 비어 있지 않음, 공백·`/`·`:`·`=` 없음). 그래서 Okta·Entra ID의 `preferred_username`
  `Alice@Corp.com`은 그대로 opLite 사용자 `alice@corp.com@okta`가 된다(영역은 마지막 `@` 뒤). LDAP 영역의 규칙(1~64자,
  `@` 없음)은 그대로다.
- **IdP 사용자에 묶기.** 외부 사용자를 처음 만들 때 IdP의 사용자(`"<issuer> <sub>"`)를 적어 둔다(`subject`). 뒤에 같은
  이름으로 로그인한 사람의 `sub`가 다르면(IdP에서 이름을 다른 사람에게 다시 준 경우 등) `sso_failed`로 거절하고 사용자는
  그대로 둔다(opld 로그에 그 이름과 "subject mismatch"). `subject`가 없는 사용자(이 기능 전에 만들어졌거나 LDAP 영역이었던
  이름)는 다음 OIDC 로그인의 것을 적는다.

- **IdP에 등록.** IdP에 opLite를 웹 애플리케이션(인가 코드 흐름)으로 등록한다.
  - 리디렉트 URI는 `https://<브라우저가 연 노드 주소>/api/v1/auth/oidc/callback`이다(예:
    `https://10.0.0.11:8006/api/v1/auth/oidc/callback`). opld는 로그인을 시작할 때 요청의 `Host` 헤더로 이 주소를 만들므로
    사용자가 들어오는 주소(노드마다, 이름과 IP도 따로)를 모두 등록한다. 리버스 프록시 뒤라면 프록시가 `Host`로 넘기는
    주소를 등록한다. 웹의 영역 대화상자가 지금 노드의 값을 복사 버튼과 함께 보이고, `oplctl realm show`는 노드 주소 뒤에
    붙일 경로(`callback path`)를 보인다.
  - 클라이언트는 기밀 클라이언트(클라이언트 비밀, 토큰 끝점에 `client_secret_basic`)나 공개 클라이언트(비밀 없음)다.
    PKCE(S256)는 둘 다 늘 쓴다.
  - 사용자 이름 클레임(기본 `preferred_username`)은 위 이름 규칙에 맞는 문자열이어야 한다(이메일 모양도 된다). 그룹
    클레임(기본 `groups`)은 문자열 배열이다(없으면 그룹 없음).
  - 노드도 IdP에 직접 닿아야 한다(discovery·JWKS·토큰·userinfo 끝점, https만). 브라우저는 IdP와 노드 둘 다에 닿아야 한다.

```bash
# Keycloak(영역 corp): 클라이언트 oplite(Client authentication 켬 — 끄면 공개 클라이언트), Valid redirect URIs에
# 위 주소. 클라이언트 범위에 "Group Membership" 매퍼(Token Claim Name groups, Add to ID token 켬, Full group path
# 끔 — 켜면 값이 /admins 꼴). preferred_username은 Keycloak 사용자 이름 그대로다.
oplctl realm add-oidc kc --display "Keycloak" \
  --issuer https://kc.example.com/realms/corp --client-id oplite --client-secret-file /root/kc-secret \
  --group admins=admin --group ops=operator --default-role viewer
# Okta(사용자 정의 인가 서버 default): 앱(OIDC Web Application)의 Sign-in redirect URIs에 위 주소. 인가 서버의 Claims에
# groups(Groups 필터, ID Token·Always). preferred_username은 로그인 이메일이라 사용자는 alice@example.com@okta가 된다.
oplctl realm add-oidc okta --display "Okta" \
  --issuer https://example.okta.com/oauth2/default --client-id 0oa1b2c3d4 --client-secret-file /root/okta-secret \
  --group opl-admins=admin --group opl-operators=operator
# Microsoft Entra ID: 앱 등록(웹 플랫폼)의 리디렉트 URI에 위 주소, "인증서 및 비밀"의 클라이언트 비밀, "토큰 구성"의
# 그룹 클레임 추가(보안 그룹 — ID 토큰의 groups에 그룹 개체 ID가 들어간다). preferred_username은 UPN이라 사용자는
# alice@contoso.com@entra가 된다. 그룹이 200개를 넘는 사용자는 groups 클레임이 오지 않아 그룹이 없다.
oplctl realm add-oidc entra --issuer https://login.microsoftonline.com/<테넌트 ID>/v2.0 \
  --client-id <애플리케이션 ID> --client-secret-file /root/entra-secret \
  --group 5f2c9a1e-0000-4000-8000-000000000001=admin
oplctl realm list
# NAME  DISPLAY   KIND  URL
# kc    Keycloak  oidc  https://kc.example.com/realms/corp
oplctl realm show kc       # issuer·client id·client secret: set|none·scopes·클레임·ca·timeout과 callback path
oplctl realm test kc       # OIDC는 사용자·비밀번호 없이 IdP만 확인한다(--user는 경고하고 무시)
# STEP       RESULT  MESSAGE
# discovery  ok      issuer https://kc.example.com/realms/corp, authorization endpoint …, token endpoint …
# jwks       ok      1 RS256 and 0 ES256 signing keys
oplctl realm set-oidc kc --timeout 5                  # 준 것만 바꾼다(읽은 revision으로 — 그 사이 바뀌었으면 412)
oplctl realm set-oidc kc --client-secret-file /root/kc-secret   # 새 클라이언트 비밀
# issuer·client ID·CA를 바꿀 때는 비밀도 다시 준다(주지 않으면 서버가 400)
oplctl realm set-oidc kc --issuer https://sso.example.com/realms/corp --client-secret-file /root/kc-secret
oplctl realm set-oidc kc --clear-client-secret        # 공개 클라이언트로(IdP 쪽도 바꾼다)
oplctl realm set-oidc kc --scope openid --scope profile --scope groups   # scope 목록 전체를 바꾼다
oplctl realm set-oidc kc --clear-ca --clear-default-role --clear-groups
```

- **설정 항목**(API `backend`의 필드 — `"kind": "oidc"`, oplctl 선택):
  - `issuer`(`--issuer`): IdP의 issuer. 호스트가 있는 `https://` URL이고 쿼리·조각·사용자 정보·공백이 없어야 한다.
    discovery 문서(`<issuer>/.well-known/openid-configuration` — issuer 끝의 `/`는 하나로 합친다)의 `issuer`, ID 토큰의
    `iss`와 **글자 그대로** 같아야 하므로 끝 `/`까지 IdP가 알리는 대로 쓴다(Keycloak `https://<호스트>/realms/<영역>` —
    옛 판은 `/auth/realms/<영역>`, Okta `https://<조직>.okta.com/oauth2/default`, Entra ID
    `https://login.microsoftonline.com/<테넌트 ID>/v2.0`).
  - `client_id`(`--client-id`): 비어 있으면 안 된다.
  - `client_secret`(`--client-secret-file <파일|->`): 없으면 공개 클라이언트. API 응답에 없고 `has_client_secret`으로 있는지만
    보인다. 바꾸기(`PUT`)에서 빼면 저장된 값을 두는데 `issuer`·`client_id`·`ca_pem`이 모두 그대로일 때만이다: 하나라도
    바꾸면서 빼면 **400** `invalid_request`("client_secret must be given again when issuer, client_id or ca_pem changes" —
    저장된 비밀을 다른 IdP·클라이언트로 보내지 않는다). 저장된 비밀이 없으면(공개 클라이언트) 이 규칙은 없다. `""`이면
    지운다(`--clear-client-secret`, 웹은 "저장된 비밀 지우기(공개 클라이언트)").
  - 서버는 저장·비교 전에 `issuer`·`client_id`의 앞뒤 공백을 떼고 `ca_pem`은 LDAP처럼 다듬는다(CRLF → LF, 앞뒤 공백).
  - `scopes`(`--scope`, 여러 번, 기본 `openid` `profile` `email`): `openid`가 있어야 하고 값마다 공백 없는 비어 있지 않은 글.
    authorization 요청에 공백으로 이어 보낸다. `set-oidc`에서 주면 목록 전체를 바꾼다.
  - `username_claim`(`--username-claim`, 기본 `preferred_username`), `groups_claim`(`--groups-claim`, 기본 `groups`):
    비어 있으면 안 된다.
  - `ca_pem`(`--ca-file`): IdP 인증서를 확인할 CA(PEM). 있으면 그 인증서들만 믿고, 없으면 시스템 루트를 쓴다. 호스트
    이름도 확인한다.
  - `timeout_secs`(`--timeout`, 기본 10, 1~30): IdP 요청마다의 시간 제한(초).
  - `display`(`--display`): 로그인 화면 버튼의 이름. `roles`: `groups`(`--group <그룹 클레임 값>=<role>`, 여러 번)와
    `default`(`--default-role`).
  - Entra ID 메모: 클레임 변환(예: `ExtractMailPrefix`로 이메일 앞부분만)을 쓰려면 앱 매니페스트에
    `acceptMappedClaims: true`가 필요하고, 앱 전용 서명 키는 discovery 주소에 `?appid=`가 붙어야 하는데 opLite는
    `<issuer>/.well-known/openid-configuration`만 읽으므로 맞지 않는다 — 쓰지 않는다. 이메일 모양 이름을 그대로 쓰므로 보통은
    변환이 필요 없다.
  - `set-oidc`는 영역을 읽어 준 것만 바꾸고, 없애는 선택은 `--clear-client-secret`·`--clear-ca`·`--clear-default-role`·
    `--clear-groups`다(같은 값을 정하는 선택과 함께 줄 수 없다). `set-ldap`·`set-oidc`는 종류가 다른 영역을 거절한다
    ("realm kc is not an LDAP realm (its kind is oidc)"). 나쁜 값은 **400** `invalid_request`다.
- **흐름.**
  1. 로그인 화면의 "<표시 이름>(으)로 로그인"이 `GET /api/v1/auth/oidc/<영역>/start`로 간다(페이지 이동). 노드는 IdP의
     discovery 문서를 읽고(노드 메모리에 10분), 요청의 `Host`로 리디렉트 URI를 만들고, 무작위 `state`·`nonce`·PKCE 검증자와
     그 리디렉트 URI를 로그인 상태 쿠키에 넣은 뒤 IdP의 authorization endpoint로 302한다(`response_type=code`,
     `client_id`, `redirect_uri`, `scope`, `state`, `nonce`, `code_challenge`, `code_challenge_method=S256`).
  2. 사용자가 IdP에서 로그인하면 IdP가 `/api/v1/auth/oidc/callback?code=…&state=…`로 돌려보낸다. IdP는 시작할 때
     보낸 리디렉트 URI(그 주소)로 돌려보내는데, 상태가 쿠키에 있으므로 그 주소 뒤의 어느 노드가 받아도 된다(노드들 앞의
     로드 밸런서·DNS 이름).
  3. 콜백은 쿠키를 확인하고(서명, 10분 안, 쿼리의 `state`와 같음), 인가 응답의 `iss`를 본다(RFC 9207: discovery 문서가
     `authorization_response_iss_parameter_supported: true`면 있어야 하고, 있으면 늘 issuer와 같아야 한다 — 다른 IdP의
     응답을 섞는 공격 막기). 토큰 끝점에 코드·쿠키의 `redirect_uri`·`code_verifier`·`client_id`를 폼으로 보내(기밀
     클라이언트는 `client_secret_basic`) ID 토큰을 받는다. 콜백은 `Host`를 보지 않는다.
  4. ID 토큰을 확인한다: 서명은 RS256(RSA 2048~8192비트)·ES256(P-256)만이고 `none`·`HS256` 등은 거절, 헤더에 `crit`가
     있으면 거절, 키는 IdP의 JWKS(노드 메모리에 10분, `use`가 있으면 `sig`만; 모르는 `kid`면 다시 받는데 마지막으로 받은 지
     10초가 지났을 때만 — IdP 키 교체를 따라감; 다시 받아도 그 `kid`가 없으면 `kid` 없이 내보인 키로 확인), `iss`가 issuer와
     같음, `aud`에 client ID(여럿이거나 `azp`가 있으면 `azp`가 client ID), `exp`·`iat`·(있으면) `nbf`는 5분 오차, `nonce`가
     쿠키의 것과 같음, `sub`가 있음.
  5. 이름·그룹 클레임 가운데 ID 토큰에 없는 것이 있고 IdP에 userinfo 끝점이 있으면 액세스 토큰으로 userinfo를 한 번 읽어
     빈 것만 채운다(userinfo의 `sub`가 ID 토큰과 다르면 실패). 이름 → `<이름>@<영역>`, 그룹 → 역할, 사용자 만들기·고치기
     (LDAP과 같음, IdP 사용자에 묶기) → 세션 쿠키(로컬 로그인과 같은 12시간 세션), 로그인 상태 쿠키 지우기 → `/`로 302.
     로그인 전에 열려던 주소로는 돌아가지 않는다.
- **IdP 요청.** discovery·JWKS를 받지 못하면 그 실패를 같은 영역(같은 revision)에서 15초 동안 기억해 다시 받지 않고 같은
  오류를 낸다(막힌 IdP로 요청을 쏟지 않게). 같은 영역을 동시에 여러 로그인이 물어도 한 번만 받는다. IdP 응답 본문은 1 MiB까지만
  읽는다(넘으면 실패).
- **쿠키.** 로그인 상태 쿠키 `opl_oidc`의 값은 `{영역, state, nonce, PKCE 검증자, 리디렉트 URI, 만든 시각}` JSON의 base64url과 그 HMAC-SHA256
  이다. HMAC 키는 클러스터 CA 비밀 키(모든 노드에 있음)에서 HKDF-SHA256(정보 `opl-oidc-state`)으로 만들므로 새로 저장하는
  값 없이 어느 노드나 같은 키를 얻는다. 속성은 `HttpOnly; Secure; SameSite=Lax; Path=/api/v1/auth/oidc; Max-Age=600`이다
  (IdP에서 돌아오는 최상위 이동에 쿠키가 가도록 `Lax`). 콜백은 성공·실패 모두 이 쿠키를 지운다. 브라우저가 쿠키를 막으면
  SSO 로그인은 늘 실패한다.
- **오류.** 실패하면 `/login?error=<코드>`로 돌아가고 로그인 화면이 문구를 보인다. 원인은 opld 로그에 경고("an OIDC login
  failed" 또는 "cannot start an OIDC login", `journalctl -u opld`)로 남는다.
  - `sso_failed`: 쿠키 없음·서명이 틀림·10분 지남·`state` 다름, IdP가 `error`로 돌려보냄(사용자가 거절 등), 인가 응답의
    `iss`가 다르거나(지원한다는 IdP인데) 없음, 토큰 끝점이 코드를 거절함(4xx `invalid_grant` — 코드가 틀렸거나 이미 썼거나
    만료), ID 토큰 확인 실패, userinfo가 액세스 토큰을 거절함(401·403), 이름 클레임이 없거나 글이 아니거나 이름 규칙에 맞지
    않음, 그룹 클레임이 문자열 배열이 아님, userinfo `sub` 다름, 같은 이름의 사용자가 다른 IdP 사용자에 묶여 있음, 시작할 때
    OIDC 영역이 아니거나 없음, 롤링 업그레이드 중(옛 opld 노드가 있을 때 — LDAP의 409 대신).
  - `no_role`: 매핑되는 역할이 없음.
  - `realm_unavailable`: IdP에 닿지 못함(연결·TLS·믿지 않는 인증서·시간 제한) 또는 IdP 끝점(discovery·JWKS·토큰·userinfo)이
    2xx가 아닌 응답(위의 `invalid_grant`·userinfo 401·403 빼고 — 토큰 끝점의 `invalid_client` 같은 클라이언트 인증 거절도
    여기다), 시작이나 콜백에서 discovery 문서를 읽지 못하거나 맞지 않음(issuer 다름, https가 아닌 끝점).
  - 시작 요청의 `Host` 헤더가 없거나 호스트 글자가 아니면 **400**(JSON)이다. 콜백은 `Host`를 보지 않으므로 늘 302이고
    로그인 상태 쿠키를 지운다.
- **시도 제한.** OIDC 로그인(시작·콜백)은 로그인 시도 제한을 쓰지 않는다. 맞힐 비밀이 없기 때문이다: `state`는 서명된
  쿠키와 같아야 하고, 코드는 IdP가 한 번만 받으며 PKCE 검증자에 묶인다. 같은 NAT·프록시 뒤의 정상 실패(쿠키 만료 등)가
  그 주소의 모든 비밀번호 로그인을 막지 않게 한다.
- **감사 로그.** 콜백 `GET /auth/oidc/callback`을 적는다(다른 `GET`처럼 빼지 않음). `path`는 쿼리를 뺀
  `/auth/oidc/callback`이다. `detail`은 성공이면 `realm=kc username=alice`(사용자는 그 외부 사용자), 실패면
  `realm=kc error=no_role`이고 쿠키를 확인하기 전 실패는 `realm=<unknown>`이다. 실패 원인은 적지 않고 코드만이다(원인은 opld
  로그). 쿼리(코드·`state`)와 토큰은 어디에도 적지 않는다. 시작은 적지 않는다.
- **시험 동작.** `oplctl realm test <영역>`(웹: 영역의 "시험", API `POST /api/v1/realms/{key}/actions/test`를 본문 없이)은
  `discovery`(문서 받기, issuer 같음, 끝점 https)와 `jwks`(쓸 수 있는 RS256·ES256 키 수, 없으면 실패) 단계를 캐시 없이
  돌린다. 아무도 로그인하지 않으므로 클라이언트 비밀·리디렉트 URI·클레임은 확인하지 않는다 — 처음 한 번은 브라우저로
  로그인해 본다. LDAP 영역 시험은 `--user`(API `username`·`password`)가 없으면 **400**이다.
- **웹.** 로그인 화면은 OIDC 영역마다 아이디·비밀번호 폼 아래 "또는" 뒤에 "<표시 이름 또는 이름>(으)로 로그인" 버튼을
  보이고, 영역 고르기에는 로컬과 LDAP 영역만 있다. "인증 영역" 화면은 만들기에서 종류(LDAP/OIDC)를 고르고(고치기에서는
  종류를 바꾸지 않음), OIDC 폼(issuer, 클라이언트 ID, 클라이언트 비밀 — 비워 두면 그대로, issuer·클라이언트 ID·CA를
  바꾸면 다시 넣으라고 알림, scope는 공백으로 나눈 한 줄, 클레임, CA, 시간 제한, 그룹 → 역할, 기본 역할)과 이 노드의
  리디렉트 URI(복사 버튼), 입력 없는 시험(단계 표)이 있다. 목록의 URL 열은 issuer다.
- **API.** 영역 API는 LDAP과 같고(`backend`의 `kind`가 `oidc`, 응답은 `has_client_secret`), `GET /api/v1/auth/realms`의
  `kind`가 `oidc`다. 로그인 흐름은 `GET /api/v1/auth/oidc/{realm}/start`와 `GET /api/v1/auth/oidc/callback`(둘 다 인증
  없음, 302)이다.
- **oplctl.** CLI는 브라우저 SSO를 하지 않는다: `oplctl login --realm <OIDC 영역>`은 비밀번호를 묻기 전에 "sign in to kc in
  the web UI, create an API token there and run `oplctl login --token-file`"로 끝난다(종료 1). SSO 사용자는 웹으로 로그인해
  API 토큰 화면에서 토큰을 만들고 파일에 넣은 뒤 `oplctl login --token-file`로 저장한다:

  ```bash
  oplctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:... --token-file /root/opl-token
  # logged in to https://10.0.0.11:8006 as alice@example.com@okta
  ```

  `--token-file <파일|->`(`-`면 표준 입력)는 `--user`·`--realm`·`--password-file`과 함께 줄 수 없다. 서버 CA는 비밀번호 로그인처럼
  `--ca-fingerprint`로 고정하고(같은 서버에 다시 로그인하면 저장한 CA), 그 토큰으로 `GET /api/v1/auth/whoami`를 불러
  확인한 뒤(받지 않으면 저장하지 않음) 로그인 정보 파일에 넣는다. 다시 로그인할 때 이전 토큰은 새 토큰과 다르고 이 CLI의
  비밀번호 로그인이 만든 토큰(이름 `oplctl-…`)일 때만 서버에서 지운다 — `--token-file`로 넣은 웹 토큰은 두므로 필요 없으면
  웹이나 `oplctl token delete`로 지운다. `oplctl logout`도 같다: login이 만든 토큰(`oplctl-…`)만 서버에서 지우고, 웹에서
  만든 토큰이면 로컬 설정에서만 지운 뒤 "the token stays valid on the server; delete it in the web UI"라고 알린다.

한계:

- 로그아웃은 opLite 세션만 끝낸다(IdP 로그아웃·RP-initiated·back-channel 로그아웃 없음). IdP에 아직 로그인되어 있으면 다시
  누르면 바로 들어온다.
- IdP는 로그인할 때만 본다. IdP에서 계정을 막거나 그룹을 빼도 이미 있는 세션(최대 12시간)과 API 토큰은 남고 역할은 다음
  로그인에 바뀐다. 바로 막으려면 opLite에서 그 사용자를 지운다.
- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 주소 수만큼 등록해야 한다. 리버스 프록시 뒤면 프록시가 넘기는 `Host`의 주소가
  리디렉트 URI가 되고 scheme은 늘 `https`다.
- 외부 사용자는 처음 로그인한 IdP 사용자(`sub`)에 묶인다: IdP에서 계정을 지우고 같은 이름으로 다시 만들면(새 `sub`) 로그인이
  `sso_failed`다 — 같은 사람이면 opLite에서 그 사용자를 지운 뒤(토큰도 지워짐) 다시 로그인한다. 영역의 issuer를 바꾸면
  모든 사용자가 같은 이유로 거절된다. Entra ID는 `sub`가 앱 등록마다 다르므로 앱 등록을 바꾸면(새 `client_id`) `sub`가
  모두 바뀌어 기존 사용자가 모두 `sso_failed`다 — 그 사용자들을 지워야 한다. `subject`가 없는 옛 사용자는 그 이름으로 처음 OIDC 로그인한 사람에게 묶인다.
- discovery·JWKS 받기가 실패하면 IdP가 돌아와도 15초 동안은 같은 오류다.
- discovery·JWKS는 노드마다 10분 둔다: IdP에서 지운 서명 키도 그동안은 그 노드에서 믿는다. 영역을 고치면 바로 새로 받는다.
- 이름 규칙(1~128자, 공백·한글 등 없음)에 맞지 않는 이름 클레임은 쓸 수 없다. 그룹은 클레임 값만 보고(IdP의 그룹 API를
  부르지 않음) Entra ID의 그룹 초과(groups 대신 `_claim_names`)는 그룹 없음이다.
- 클라이언트 인증은 `client_secret_basic`과 공개 클라이언트뿐이다(`client_secret_post`·`private_key_jwt`·mTLS 없음).
  `response_mode=form_post`, 요청 객체·PAR, 서명 알고리즘 RS256·ES256 밖(PS256·EdDSA 등)과 암호화한 ID 토큰은 없다.
- 클라이언트 비밀은 Raft에 평문으로 있다(서비스 계정 비밀번호와 같음). oplctl은 브라우저 SSO를 하지 않는다(웹에서 만든 토큰을
  `oplctl login --token-file`로).
````

`README.md`에서

```markdown
노드마다 그 노드가 받은 바꾸는 API 요청(`POST`·`PUT`·`PATCH`·`DELETE`, 성공·실패와 401·403 포함), 로그인 성공·실패,
로그아웃, VM 콘솔 열기(`GET /api/v1/vms/{key}/console`)를 한 줄 JSON으로 적는다. 다른 `GET`(감사 로그 읽기도),
웹 정적 파일, 노드 사이 RPC, 제어 소켓(`cluster init`·`join`·`leave`), 맞는 라우트가 없는 요청은 적지 않는다.
```

을 다음으로 바꾼다:

```markdown
노드마다 그 노드가 받은 바꾸는 API 요청(`POST`·`PUT`·`PATCH`·`DELETE`, 성공·실패와 401·403 포함), 로그인 성공·실패,
로그아웃, VM 콘솔 열기(`GET /api/v1/vms/{key}/console`), OIDC 로그인 콜백(`GET /api/v1/auth/oidc/callback`)을 한 줄 JSON으로 적는다. 다른 `GET`(감사 로그 읽기도),
웹 정적 파일, 노드 사이 RPC, 제어 소켓(`cluster init`·`join`·`leave`), 맞는 라우트가 없는 요청은 적지 않는다.
```

`README.md`에서

```markdown
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다, 영역 로그인은 뒤에 ` realm=<영역>`), 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
```

을 다음으로 바꾼다:

```markdown
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다, 영역 로그인은 뒤에 ` realm=<영역>`), OIDC 콜백은 `realm=<영역> username=<이름>` 또는 `realm=<영역> error=<코드>`, 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO), 계획 37(원격 클러스터), 계획 38(원격 복제·DR)로 나눴다(`2026-11-06-enterprise-design.md`). 계획 36~38은 남았다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO: `Realm` 종류 `oidc`(issuer·client_id·client_secret(응답·watch에 없음, 공개 클라이언트도 됨)·scope·이름·그룹 클레임·CA), reqwest와 ring으로 직접 만든 인가 코드 흐름(PKCE S256, state·nonce·검증자를 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한 쿠키 `opl_oidc`에 담아 어느 노드나 콜백을 받음), discovery·JWKS 노드 캐시와 모르는 kid 다시 받기, ID 토큰 RS256·ES256 확인과 iss·aud·azp·exp·iat·nbf·nonce, 인가 응답 iss(RFC 9207), userinfo로 빠진 클레임, 그룹 클레임으로 역할과 계획 35의 외부 사용자(이메일 모양 이름, IdP 사용자 sub에 묶음), `/login?error=` 코드와 콜백 감사(시도 제한 없음), IdP 시험 동작, `oplctl realm add-oidc|set-oidc`·`login --token-file`, 웹 로그인 SSO 버튼과 OIDC 영역 화면, 가짜 IdP 시험, 완료), 계획 37(원격 클러스터), 계획 38(원격 복제·DR)로 나눴다(`2026-11-06-enterprise-design.md`). 계획 37·38은 남았다.
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown

## 5. 원격 클러스터 (계획 37)
```

을 다음으로 바꾼다:

```markdown

### 4.5 구현(계획 36)

- 타입(`opl_types::realm`): `RealmBackend::Oidc(OidcRealm)`(`"kind": "oidc"`), `OidcRealm{issuer, client_id, client_secret, scopes, username_claim, groups_claim, ca_pem, timeout_secs}`(`deny_unknown_fields`, `client_secret`·`ca_pem`은 None이면 쓰지 않음, 기본값 `DEFAULT_SCOPES` `["openid","profile","email"]`·`DEFAULT_USERNAME_CLAIM` `preferred_username`·`DEFAULT_GROUPS_CLAIM` `groups`·`timeout_secs` 10, 손으로 쓴 `Debug`가 `client_secret`을 가림). 검증(`RealmError`의 새 변형 `Issuer`·`ClientId`·`Scopes`·`UsernameClaim`·`GroupsClaim`·`OidcTimeout`): issuer는 호스트가 있는 `https://` URL이고 쿼리·조각·사용자 정보·공백이 없음(끝 `/`는 정규화하지 않고 그대로 비교에 씀), `client_id`·두 클레임 이름은 공백만이 아님, `scopes`에 `openid`가 있고 값마다 공백 없는 비어 있지 않은 글, `timeout_secs` 1~30(LDAP과 같게). 계획 35의 반박 불가 패턴(`let RealmBackend::Ldap(l) = …`)은 match로. `opl_types::auth`: `UserSpec.subject: Option<String>`(None이면 쓰지 않음 — 옛 노드가 모르는 필드라 쓰기 전 `require_current`), OIDC 외부 사용자를 처음 만든 IdP 사용자 `"<iss> <sub>"`(공백 하나). 외부 사용자 이름은 마지막 `@`에서 영역을 나누고(`rsplit_once`) 사용자 부분 규칙은 영역 종류에 따라: LDAP은 `validate_external_name`·`external_name`(1~64자 `[a-z0-9._-]`, 그대로), OIDC는 `validate_oidc_external_name`·`external_name_oidc`(1~128자 `[a-z0-9._+-]`와 안쪽 `@` 하나 — 앞뒤 비어 있지 않음; `alice@corp.com@okta`는 사용자 `alice@corp.com`·영역 `okta`). 로컬 사용자(`POST /users`)는 그대로 `@` 불가. 설계와 다른 점: `timeout_secs`가 더해졌고, 사용자 이름 규칙이 OIDC에서 넓어졌다(이메일 모양).
- 영역 API(`opl_api::realms`): `RealmBackendView::Oidc(OidcRealmView{issuer, client_id, scopes, username_claim, groups_claim, ca_pem?, timeout_secs, has_client_secret})`, `RealmKind::Oidc`(`GET /auth/realms`의 `kind` `oidc`). `spec_of`가 종류마다 `ldap_of`·`oidc_of`로 나뉘고 `trim_pem`을 함께 쓴다. `oidc_of`는 `issuer`·`client_id`의 앞뒤 공백을 떼고 `ca_pem`을 다듬은 뒤, `client_secret`을 빼면 저장된 OIDC 영역에 비밀이 있을 때 `issuer`·`client_id`·`ca_pem`이 모두 그대로일 때만 두고 아니면 400 "client_secret must be given again when issuer, client_id or ca_pem changes"(저장된 비밀이 없으면 공개 클라이언트로 남음), `""`는 지움. 종류를 바꾸는 `PUT`은 비밀을 옮기지 않는다. `POST /auth/login`은 영역을 먼저 읽어 OIDC 영역이면 이름 규칙과 관계없이(이메일 모양 이름도) 시도로 세기 전에 400 `invalid_request` "realm <r> signs in through its identity provider". 시험 동작: `RealmTestRequest{username?, password?}`(둘 다 선택), 본문은 없어도·`null`이어도 됨(`crate::error::Json`에 `OptionalFromRequest` — `Content-Type`이 없으면 None), LDAP 영역은 둘 중 하나라도 없으면 400 "username and password are required to test an LDAP realm", OIDC 영역은 본문을 무시하고 `oidc::test`.
- OIDC 모듈(`opl_api::oidc`, pub; opl-api에 `ring`·`base64` 직접 의존과 reqwest `form` 기능 — 새 패키지 없음): `client(&OidcRealm)`(reqwest native-tls, 리다이렉트 없음, `https_only`, 시간 제한 `timeout_secs`, `ca_pem`이 있으면 `tls_certs_only`로 그 인증서만 — 읽지 못하거나 비면 `Invalid`), 응답 본문은 읽으며 1MiB까지(`content_length()`가 넘으면 바로, 아니면 `chunk()`로 읽다가 넘으면 멈춤 — `Invalid`). `OidcError{Unavailable(→realm_unavailable: 연결·TLS·시간 제한·2xx가 아닌 응답 — 오류 본문의 `error`를 붙임), Invalid(→sso_failed: 토큰 끝점 4xx의 `invalid_grant`와 액세스 토큰을 보낸 userinfo의 401·403도)}`. `discovery_url`(issuer 끝 `/`를 하나로 합쳐 `/.well-known/openid-configuration`), `fetch_discovery`(`Discovery{issuer, authorization_endpoint, token_endpoint, jwks_uri, userinfo_endpoint?}`, `issuer`가 설정값과 글자 그대로 같고 끝점은 `https://`), `parse_jwks`(`kty: RSA`(`n`,`e`)·`kty: EC` `crv: P-256`(`x`,`y` → `04||x||y`)만, `alg`가 있으면 RS256·ES256, `use`가 있으면 `sig`, 깨진 키는 건너뜀), `verify_id_token(token, keys, Expected{issuer, client_id, nonce}, now)`(헤더 `alg`는 RS256(`RSA_PKCS1_2048_8192_SHA256`)·ES256(`ECDSA_P256_SHA256_FIXED`)만, 헤더에 `crit`가 있으면 거절, `kid`가 있으면 그 kid의 키(JWKS에 그 kid가 없으면 kid 없는 키 가운데 alg가 맞는 것), 없으면 alg가 맞는 키를 모두; `iss` 같음, `aud`(글 또는 배열)에 client_id, `aud`가 여럿이거나 `azp`가 있으면 `azp` == client_id, `exp` > 지금 − 300초, `iat` ≤ 지금 + 300초(둘 다 필수), `nbf`가 있으면 ≤ 지금 + 300초, `nonce` 같음, `sub`가 비어 있지 않음). `Discovery`는 RFC 9207의 `authorization_response_iss_parameter_supported`(없으면 false)도 읽는다. `OidcCache`(`AppState.oidc`, 노드 메모리, 키 `RealmKey` = (영역 id, revision) — 영역마다 가장 높은 revision 항목만 두어 늦게 끝난 옛 revision 받기가 새 항목을 지우지 않음): `discovery`·`keys`(10분, 모르는 `kid`면 마지막으로 받은 지 `MIN_REFETCH` 10초가 지났을 때만 다시 받음)·`verify_id_token`. 받기 실패는 같은 키에서 `FAILURE_TTL` 15초 동안 기억해 다시 받지 않고 같은 오류를 돌려주고, 같은 키의 받기는 키별 `tokio::sync::Mutex`로 한 번에 하나만(기다린 요청은 그 결과를 씀). `exchange_code`(폼 `grant_type=authorization_code, code, redirect_uri, code_verifier, client_id`, 비밀이 있으면 client_id와 비밀을 각각 form-urlencode한 `client_secret_basic`(RFC 6749 §2.3.1) → `Tokens{id_token, access_token?}`(Debug 가림)). 로그인 상태: `random_value()`(32바이트 base64url), `code_challenge`(S256), `state_key(ca_key_pem)`(HKDF-SHA256, salt 없음, 정보 `opl-oidc-state`, 32바이트 HMAC 키), `LoginState{realm, state, nonce, verifier, redirect_uri, created}::new·seal·open`(base64url(JSON) `.` base64url(HMAC-SHA256); `open`은 서명, `created`가 10분(`STATE_TTL_SECS`) 안이고 미래로 5분 넘지 않음, 쿼리 `state`와 상수 시간 비교). `oidc::test`(캐시 없이 `discovery`·`jwks` 단계 — 키가 없으면 실패, `dn`·`groups`·`role`은 비움). 설계와 다른 점: `azp`는 `aud`가 하나여도 있으면 확인한다.
- 로그인 흐름(`opl_api::oidc_login`, `CALLBACK_PATH`): 리디렉트 URI는 `start`가 `<https|http>://<Host>/api/v1/auth/oidc/callback`으로 만들고(opld는 늘 `secure_cookies`라 https, http는 시험만; `Host`가 없거나 영숫자·`.`·`-`·`:`·`[`·`]` 밖의 글자가 있으면 400) 상태 쿠키에 넣는다. `GET /auth/oidc/{realm}/start`(인증 없음): 영역이 없거나 OIDC가 아니면 `/login?error=sso_failed`, discovery가 어떤 이유로든 실패하면 `realm_unavailable`, 성공이면 `opl_oidc` 쿠키(`HttpOnly; SameSite=Lax; Path=/api/v1/auth/oidc; Max-Age=600`, `secure_cookies`면 `Secure`)와 authorization endpoint로 302(기존 쿼리 뒤에 `response_type=code`·`client_id`·`redirect_uri`·`scope`(공백으로 이음)·`state`·`nonce`·`code_challenge`·`code_challenge_method=S256`). `GET /auth/oidc/callback?code&state[&iss]`(또는 `error`·`error_description`): 쿠키 확인(쿼리가 깨졌거나 쿠키가 없거나 확인 실패면 `sso_failed`) → IdP `error`면 `sso_failed` → `code` → 영역 다시 읽기 → discovery(캐시 — 실패는 `Invalid`도 start처럼 `realm_unavailable`) → RFC 9207(`authorization_response_iss_parameter_supported`면 `iss`가 있어야 하고, 있으면 issuer와 같아야 함 — 아니면 `sso_failed`) → 상태의 `redirect_uri`로 `exchange_code` → `OidcCache::verify_id_token` → 이름·그룹 클레임 가운데 없는 것이 있고 `userinfo_endpoint`와 `access_token`이 있으면 userinfo를 한 번 읽어 없는 것만 채움(`sub`가 ID 토큰과 다르면 실패) → 이름은 글이어야 하고 `external_name_oidc`(소문자) 규칙, 그룹은 문자열 배열(없거나 null이면 빈 것) → `role_for`(없으면 `no_role`) → `require_current`(실패는 `sso_failed`) → 계획 35의 `upsert_external(…, subject)`(pub(crate); OIDC는 `"<iss> <sub>"`를 주어 만들 때 적고, 같은 이름 사용자의 subject가 다르면 거절 — `warn!`에 이름과 "subject mismatch", `sso_failed`, 사용자 그대로 — None(옛 데이터)이면 적음; LDAP은 None) → `users::new_session`(pub(crate), 로컬과 같은 세션 쿠키)과 `opl_oidc` 지우기(`Max-Age=0`), `/`로 302. 실패는 `warn!`(영역·코드·원인)과 `opl_oidc` 지우기, `/login?error=<sso_failed|no_role|realm_unavailable>`. 콜백은 `Host`를 보지 않고(400 경로가 없으므로 쿠키를 지우지 않는 예외도 없음) 시도 제한(`AttemptLimiter`)을 쓰지 않는다: 맞힐 값이 없고(state는 HMAC, 코드는 한 번 쓰고 PKCE에 묶임) NAT·프록시 뒤의 정상 실패가 모두의 비밀번호 로그인을 막지 않게. 클러스터 CA 키는 `cluster::cluster_info`(pub(crate)). 감사 미들웨어 `audit::layer`는 GET 가운데 콘솔과 `/auth/oidc/callback`을 적고(`path`는 쿼리를 뺀 `/auth/oidc/callback` — 코드·state는 남지 않음), 콜백의 `detail`은 성공 `realm=<r> username=<u>`(주체는 그 사용자, `auth` `session`)·실패 `realm=<r> error=<code>`(쿠키 확인 전 실패는 `realm=<unknown>`). `start`는 적지 않는다. 설계와 다른 점: 토큰 끝점의 `invalid_grant`·userinfo 401·403은 `sso_failed`이고 그 밖의 2xx가 아닌 응답(`invalid_client` 등)은 `realm_unavailable`이며, 롤링 업그레이드 중의 OIDC 로그인은 409가 아니라 `sso_failed`다. 콜백 실패를 IP로 세지 않는다(§4.2의 시도 제한 대신). 감사 `detail`에는 원인이 아니라 코드만 남긴다(§4.2 5단계 — 원인은 opld 로그). 쿠키는 확인한 뒤 바로가 아니라 응답에서(성공·실패 모두) 지운다. 외부 사용자를 IdP 사용자(sub)에 묶는 것과 RFC 9207 `iss` 확인이 더해졌다.
- oplctl: `realm add-oidc <이름> --issuer --client-id [--client-secret-file <파일|->] [--scope]... [--username-claim] [--groups-claim] [--ca-file] [--timeout] [--display] [--group <값>=<role>]... [--default-role]`(공통 `OidcArgs`, 준 것만 본문에), `realm set-oidc <이름> (같은 선택, --issuer·--client-id도) [--clear-client-secret] [--clear-ca] [--clear-default-role] [--clear-groups]`(영역을 읽어 준 것만 바꾸고 `If-Match`로 `PUT`, 비밀은 주지 않으면 빼서 옮기기 규칙은 서버가 판단, `--clear-client-secret`은 `""`, `--scope`·`--group`은 목록 전체를 바꿈, 각 `--clear-*`는 같은 값을 정하는 선택과 `conflicts_with`). `set-ldap`·`set-oidc`는 종류가 다른 영역을 거절("realm <r> is not an OIDC realm (its kind is ldap)"). LDAP·OIDC 본문의 역할은 공통 `new_roles`·`changed_roles`. `realm list`의 URL 칸은 OIDC면 issuer, `realm show`는 `issuer`·`client id`·`client secret: set|none`·`scopes`·`username claim`·`groups claim`·`ca`·`timeout`과 `callback path: /api/v1/auth/oidc/callback`. `realm test`의 `--user`는 선택(`--password-file`은 `--user`와 함께): 없으면 묻지 않고 `{}`를 보내 단계 표만(LDAP이면 서버의 400). `realm test`는 `GET /realms/<이름>`으로 종류를 읽어, OIDC에 `--user`를 주면 stderr에 "warning: --user is ignored for an OIDC realm"을 내고 비밀번호를 묻지 않으며, DN·그룹·역할 줄은 LDAP 영역일 때만 보인다. `login --realm`이 `local`이 아니면 비밀번호를 묻기 전에 `GET /auth/realms`를 읽어 OIDC 영역이면 "sign in to <r> in the web UI, create an API token there and run `oplctl login --token-file`"(종료 1, 목록을 읽지 못하면 지나감). `login --token-file <파일|->`(`--user`·`--realm`·`--password-file`과 충돌): 서버 CA는 `--ca-fingerprint`로 고정(`server_ca`), 그 토큰으로 `GET /auth/whoami`를 불러 확인하고 이름을 얻어 저장(받지 않으면 저장하지 않음). 다시 로그인할 때 이전 토큰은 새 토큰과 다르고 이름이 `oplctl-`(비밀번호 `login`이 만든 것 — 이전 토큰으로 `GET /tokens`를 읽어 확인)일 때만 지운다(두 로그인 모두). `logout`도 같은 규칙(`remove_login_token`): `oplctl-` 토큰만 서버에서 지우고, 아니면 로컬 설정만 지운 뒤 "the token stays valid on the server; delete it in the web UI". 설계와 다른 점: `set-oidc`와 `login --token-file`이 더해졌다.
- 웹: 로그인 화면(`LoginPage.tsx`)은 `GET /auth/realms`의 OIDC 영역마다 폼 아래 구분선 "또는" 뒤에 `login.sso.signIn` "<표시 이름 또는 이름>(으)로 로그인"을 `/api/v1/auth/oidc/<이름>/start`로 가는 전체 페이지 링크(버튼 모양)로 보이고, 영역 고르기는 local·LDAP만(비밀번호 영역이 local뿐이면 고르기 없음). 주소의 `?error=<코드>`를 입력 오류와 따로 알림으로 보이고(`sso_failed`·`too_many_attempts`는 `errors.*`, `no_role`·`realm_unavailable`은 IdP 문구 `login.sso.*`, 모르는 코드는 `sso_failed`) 주소에서는 `error`만 지움(react-router `setSearchParams` replace — `next`는 남음), 비밀번호로 다시 로그인하면 사라짐. SSO 로그인은 늘 `/`로 돌아온다(`next`를 잃음) — 세션 쿠키가 `SameSite=Strict`라 IdP에서 이어진 첫 페이지 요청에는 가지 않지만 웹은 같은 출처 `whoami`로 세션을 읽는다. `/realms`: 만들기 대화상자에서 종류(LDAP/OIDC)를 고르고(제목 "OIDC 영역 만들기", 고치기에서는 종류 고정), OIDC 폼(issuer, 클라이언트 ID, 클라이언트 비밀 — 고칠 때 비우면 빼서 유지, "저장된 비밀 지우기(공개 클라이언트)"면 `""`; scope(공백으로 나눈 한 줄), 사용자 이름·그룹 클레임, CA PEM, 시간 제한, 그룹 → 역할, 기본 역할)과 서버와 같은 검사(https issuer, `openid` scope, 빈 값, 1~30초, 저장된 비밀이 있으면 issuer·client_id·CA를 바꿀 때 비밀 다시 넣기), 이 노드의 리디렉트 URI(`${location.origin}/api/v1/auth/oidc/callback`, 대화상자의 읽기 전용 칸과 복사 버튼), OIDC 시험은 입력 없이 본문 없이 불러 `discovery`·`jwks`("서명 키") 단계 표만. 목록의 URL 열은 issuer. `/realms` 안내(`realms.pageHint`)는 LDAP·OIDC 모두, 리디렉트 URI 안내는 리버스 프록시면 프록시가 Host로 넘기는 주소를 등록하라고. ko·en.
- 시험: opl-types 단위(기본값, 태그 JSON 왕복, 검증 각 경우, `Debug` 가림), opl-types `auth` 단위(`subject` 왕복, LDAP·OIDC 이름 규칙과 마지막 `@`, 길이·문자 경계), `oidc.rs` 단위(RS256·ES256 확인, `none`·`HS256` 등 거절, `crit`·`nbf`, 모르는 kid에서 kid 없는 키로, 서명·키 불일치, `iss`·`aud`·`azp`·`exp`·`iat`·`nonce`·`sub` 경우, JWKS 고르기와 EC 점 형식, RFC 7636 부록 B의 PKCE 값, client_secret_basic 인코딩, discovery URL의 `/`, 상태 쿠키 서명·만료·바뀐 바이트·다른 키·state 다름), 시험 견본 RSA 키 `tests/fixtures/oidc-rsa.pk8`(openssl 2048비트 PKCS#8 DER — ring은 RSA 키를 만들지 못함), 가짜 IdP `tests/common/idp.rs`(시험 CA가 서명한 127.0.0.1 HTTPS; discovery·`/authorize`(쿼리를 기록하고 바로 `redirect_uri?code&state`로 302)·`/token`(코드·PKCE S256·client_secret_basic 또는 공개 클라이언트 확인)·`/jwks`·`/userinfo`; `Idp::start()`(`oplite`/`client-secret`)·`start_with(id, None)`(공개), `backend()`, `follow(url)`, `hits`·`authorize_queries`·`token_requests`, `set_discovery`·`add_key`·`publish`·`sign_with`·`set_header_kid`·`set_claims`·`set_userinfo`·`set_authorize_error`·`set_authorize_iss`·`fail_token`·`fail_userinfo`·`pad_jwks`·`sign_raw`; 서명 키는 RS256 `rsa-1`과 시험마다 새로 만드는 ES256 `ec-1`), `tests/oidc.rs`(discovery 확인과 캐시, issuer 다름·http 끝점 거절, 믿는 인증서로만 닿음, 캐시한 키로 ID 토큰 확인, 모르는 kid 뒤 다시 받기와 10초 안 다시 받지 않음, 응답 크기 제한(Content-Length·청크), 받기 실패 기억, 동시 요청 한 번 받기, 가장 높은 revision, 시험 동작 단계), `tests/realms.rs`(만들기·응답에 비밀 없음·`has_client_secret`, 공개 클라이언트, 비밀 유지·바꾸기·지우기·issuer·client_id·CA를 바꾸며 빼면 400, 나쁜 값 400, 권한·옛 노드 409, 로그인 화면 목록 `oidc`와 비밀번호 로그인 400), `tests/oidc_login.rs`(성공 흐름 — start의 Location 쿼리·쿠키 속성 → 가짜 authorize → callback → 세션으로 `whoami`의 `alice@okta`와 역할, 두 번째 로그인은 쓰지 않음; authorization endpoint의 기존 쿼리; 다른 종류·없는 영역·닿지 않는 IdP의 start; start의 Host 없음 400·콜백은 Host 없어도 302; 상태의 리디렉트 URI(다른 Host); 쿠키 없음·state 다름·만료·변조·다른 키; IdP error; RFC 9207 iss(지원·미지원); 콜백의 discovery 실패 → `realm_unavailable`; 토큰 끝점 `invalid_grant` → `sso_failed`·`invalid_client`·`invalid_request`·5xx → `realm_unavailable`; userinfo 401·403·500; nonce; userinfo로 채우기·`sub` 다름; `no_role`; 이름 규칙과 이메일 모양 이름; sub 묶기(다른 sub 거절·같은 sub·subject 없는 옛 사용자); 공개 클라이언트; client_secret_basic 인코딩; 그룹이 바뀌면 역할과 옛 세션; 감사(성공·실패, path, code=·state= 없음); 콜백 실패가 비밀번호 로그인·SSO를 막지 않음), `tests/two_nodes.rs`(A에서 시작해 B가 받은 콜백 — 상태의 A 주소로 교환), oplctl 단위와 `fake_api`(파싱, `add-oidc`·`set-oidc` 본문, `show` 줄, list·add·set·test 요청과 출력(OIDC `--user`), OIDC 로그인 거절, `login --token-file` 충돌·저장·같은 토큰·웹 토큰은 두고 `oplctl-`만 지움·401), 웹 Vitest(`App.test.tsx`: SSO 링크·고르기 목록·오류 코드 문구와 주소 정리·SSO 뒤 `/`에서 `whoami`; `RealmsPage.test.tsx`: OIDC 목록·만들기·검사·리디렉트 URI 복사·비밀 유지·바꾸기·지우기·다시 넣기 규칙·공개 클라이언트·시험). CI에 새 작업은 없다(실제 IdP 시험 없음 — 가짜 IdP만).

## 5. 원격 클러스터 (계획 37)
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
|---|---|---|
| `no_role` | 403 | 외부 사용자의 그룹·클레임에 맞는 역할이 없음 |
| `realm_unavailable` | 503 | LDAP 서버·IdP에 닿지 못함(연결·TLS·믿지 않는 인증서), 서비스 바인드·검색 실패, 사용자 바인드의 49가 아닌 실패 |
| `external_user` | 400 | 외부 사용자의 비밀번호 바꾸기 |
| `invalid_request` | 400 | 모르는 영역으로 로그인, 영역 이름 `local`, 영역 검증 실패(계획 35) |
| `sso_failed` | (리디렉트) | OIDC 콜백 확인 실패 |
| `remote_fingerprint_mismatch` | 400 | 원격 CA 지문이 다름 |
```

을 다음으로 바꾼다:

```markdown
|---|---|---|
| `no_role` | 403 (OIDC는 리디렉트) | 외부 사용자의 그룹·클레임에 맞는 역할이 없음 |
| `realm_unavailable` | 503 (OIDC는 리디렉트) | LDAP 서버·IdP에 닿지 못함(연결·TLS·믿지 않는 인증서), 서비스 바인드·검색 실패, 사용자 바인드의 49가 아닌 실패; IdP 끝점의 2xx가 아닌 응답(토큰 끝점 `invalid_grant`·userinfo 401·403 빼고), OIDC 시작·콜백의 discovery 실패(계획 36) |
| `external_user` | 400 | 외부 사용자의 비밀번호 바꾸기 |
| `invalid_request` | 400 | 모르는 영역으로 로그인, 영역 이름 `local`, 영역 검증 실패(계획 35); OIDC 영역으로 비밀번호 로그인, issuer·client_id·ca_pem을 바꾸며 `client_secret`을 뺌, LDAP 영역 시험에 사용자·비밀번호 없음, `Host` 없는 OIDC 시작(계획 36) |
| `sso_failed` | (리디렉트) | OIDC 로그인 실패: 없는·OIDC가 아닌 영역, 상태 쿠키(없음·서명·10분·state), IdP `error`, 인가 응답 `iss`(RFC 9207), 토큰 끝점 `invalid_grant`, userinfo 401·403, ID 토큰·클레임·이름 규칙·userinfo `sub`, 다른 IdP 사용자(sub)에 묶인 이름, 롤링 업그레이드 중. 콜백 실패는 HTTP 상태 대신 `/login?error=<sso_failed|no_role|realm_unavailable>`로 302 |
| `remote_fingerprint_mismatch` | 400 | 원격 CA 지문이 다름 |
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 노드 수만큼 등록해야 한다.
- 원격 등록의 토큰은 Raft에 평문으로 있다(구독 키·바인드 비밀번호와 같음 — Raft 데이터는 노드의 루트만 읽음).
```

을 다음으로 바꾼다:

```markdown
- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 노드 수만큼 등록해야 한다.
- (계획 36) 리디렉트 URI는 `start` 요청의 `Host`로 만들고 scheme은 늘 https다(프록시 뒤면 프록시가 넘기는 Host). 로그아웃은 opLite 세션만 끝내고(IdP 로그아웃 없음), IdP의 계정·그룹 변화는 다음 로그인에 반영된다(이미 있는 세션 12시간·토큰은 남음). discovery·JWKS는 노드마다 10분 캐시라 IdP에서 지운 서명 키도 그동안 믿는다(영역을 고치면 새로 받음). 받기 실패는 15초 동안 기억하므로 IdP가 돌아와도 그동안은 같은 오류다. 토큰 끝점의 `invalid_grant`가 아닌 4xx(`invalid_client` 등 클라이언트 설정 잘못)도 `realm_unavailable`이라 IdP 장애와 같은 문구다(원인은 opld 로그). 롤링 업그레이드 중의 OIDC 로그인은 `sso_failed`(LDAP은 409). SSO 로그인은 늘 `/`로 돌아온다(`next`를 잃음). 외부 사용자는 처음 로그인한 IdP 사용자(`iss`·`sub`)에 묶이므로 IdP에서 계정을 다시 만들면(새 `sub`) 그 사용자의, 영역의 issuer를 바꾸거나 Entra ID에서 앱 등록을 바꾸면(새 `client_id` — Entra의 `sub`는 앱마다 다름) 모든 기존 사용자의 로그인이 `sso_failed`다(opLite에서 사용자를 지워야 함); `subject`가 없는 옛 사용자는 그 이름으로 처음 OIDC 로그인한 사람에게 묶인다. 이름 규칙(1~128자, 이메일 모양은 됨)에 맞지 않는 이름 클레임은 쓸 수 없고, 그룹은 클레임 값만 본다(Entra ID 그룹 초과 `_claim_names`는 그룹 없음). 클라이언트 인증은 `client_secret_basic`과 공개 클라이언트만, 서명은 RS256·ES256만이다. 클라이언트 비밀은 Raft에 평문이다. oplctl은 브라우저 SSO를 하지 않고 웹에서 만든 API 토큰을 `oplctl login --token-file`로 저장한다. OIDC 로그인은 시도 제한을 쓰지 않는다(맞힐 비밀이 없음). 시험 동작은 discovery·JWKS만 보고 클라이언트 비밀·리디렉트 URI·클레임은 확인하지 못한다.
- 원격 등록의 토큰은 Raft에 평문으로 있다(구독 키·바인드 비밀번호와 같음 — Raft 데이터는 노드의 루트만 읽음).
```

- [ ] **Step 2: 확인**

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: SSO(OIDC) — README 새 절 \"SSO(OIDC)\"(LDAP과 같은 영역 규칙, IdP 등록 방법 — Host로 만드는 리디렉트 URI와 노드 주소마다 등록, 기밀·공개 클라이언트와 PKCE, 이메일 모양 이름 클레임은 쓸 수 없음, Keycloak·Okta·Entra ID의 issuer·그룹 클레임 예, oplctl realm add-oidc|set-oidc|show|test 예, 설정 항목과 client_secret 유지·지우기 규칙, 흐름, opl_oidc 쿠키(HKDF·HMAC, SameSite=Lax), ID 토큰 확인, sso_failed·no_role·realm_unavailable·too_many_attempts와 Host 400, IP 시도 제한, 콜백 감사 detail, 시험 동작, 웹, API, oplctl은 웹에서 만든 토큰, 한계 — 로그아웃은 opLite만 등)와 LDAP 절의 안내·웹 UI 목록·감사 로그의 콜백 기록과 detail 갱신, 엔터프라이즈 설계 §4.5 구현(계획 36)(timeout_secs, azp, too_many_attempts, 토큰 끝점 non-2xx는 realm_unavailable, 롤링 업그레이드 중 sso_failed, set-oidc 등 설계와 다른 점)과 §7 오류·§9 알려진 틈, 노드 코어 설계 §1.1에 계획 36 완료"
```

---

## 완료 기준 (계획 36)

- OIDC 영역을 만들면 로그인 화면에 버튼이 생기고, 가짜 IdP(시험)로 로그인하면 `alice@okta`(그룹으로 정한 역할, IdP `sub`에 묶임)가 세션을 받는다. 다른 노드로 콜백이 돌아와도 된다.
- 쿠키 없음·state 다름·변조·만료, nonce·aud·iss·서명 틀림, `alg: none`은 모두 `sso_failed`이고, 매핑 없음은 `no_role`, IdP에 닿지 못하면 `realm_unavailable`이다.
- client secret·code·state·토큰이 응답·감사 로그·로그에 없다.
- `oplctl login --token-file`로 SSO 사용자가 oplctl을 쓴다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 `typecheck`·`lint`·`test`·`build`가 통과한다.

## 알고 받아들인 제한

- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 노드 수만큼 등록한다. 리버스 프록시 뒤라면 프록시가 Host로 넘기는 주소를 등록한다.
- 로그아웃은 opLite 세션만 끝낸다. IdP에서 사용자를 지워도 이미 있는 세션(최대 12시간)과 API 토큰은 남는다.
- issuer나 (Entra ID에서) 앱 등록을 바꾸면 모든 `sub`가 바뀌어 기존 사용자가 `sso_failed`가 된다 — 사용자를 지우고 다시 들어온다.
- 그 밖의 제한은 설계 문서 §4.5·§9와 README "SSO(OIDC)" 절에 있다.

## 실행 후 기록 (2026-11-08)

- **Task 1~6:** `feat/plan36-oidc`에서 계획대로 적용했다. Task마다 검증한 스테이지 커밋과 트리가 같았다(생성 파일 제외; Task 2의 이진 시험 키도 base64로 같은 바이트).
- **실행 중에 고친 것 (Task 2):** 이 Mac의 저장소 경로에서는 새 시험 CA로 하는 첫 TLS 연결이 15초쯤 걸려, 가짜 IdP 영역의 기본 시간 제한(10초)으로는 OIDC 시험 8개가 시간 초과였다(스크래치 복제본에서는 그렇지 않았다 — 계획 33과 같은 현상). 가짜 IdP의 `timeout_secs`를 30초로 하고 계획 문서를 고쳤다(이 브랜치의 문서 커밋).
- **1차 리뷰에서 고친 것:**
  - 외부 사용자를 IdP의 `sub`에 묶는다(`UserSpec.subject`). 이름 클레임이 다른 사람에게 다시 주어져도 계정을 넘겨받지 못한다.
  - 이메일 모양 이름(`alice@corp.com@okta`, 영역은 마지막 `@`에서 나눔).
  - 리디렉트 URI를 서명한 상태에 넣어 콜백은 Host를 보지 않는다. 콜백은 시도 제한을 쓰지 않는다(NAT 뒤 정상 실패가 모두의 로그인을 막지 않게).
  - `invalid_grant`·userinfo 401은 `sso_failed`, RFC 9207 `iss`, 응답 크기를 읽으며 제한, discovery 실패 15초 기억·한 번만 받기, `crit`·`nbf`, 가장 높은 revision 캐시.
  - `oplctl login --token-file`, `realm test` 출력, 웹 안내 문구.
- **2차 리뷰에서 고친 것:** OIDC 이름에서 `%` 뺌(URL에서 다른 이름으로 풀려 `oplctl user delete`가 다른 사용자를 지울 수 있었다), oplctl 경로 조각 퍼센트 인코딩, 로그아웃은 oplctl이 만든 토큰만 서버에서 지움, OIDC 영역에 비밀번호 로그인은 늘 400.
- **CI 프로브에서 고친 것:** 경로 인코딩이 VM 동작의 `stop?mode=force`까지 인코딩해 `vm stop --force`가 404였다(CI의 실제 KVM 백업 시험이 잡음). 동작 부분은 인코딩하지 않는다.
- **불안정한 시험(부하 때, 다시 돌리면 통과):** opl-api `snapshot_tasks.rs` left_tag_removal_stops_with_the_runner(Task 1), `telemetry_reporter.rs` a_failure_is_recorded_and_retried(Task 3) — 다른 cargo가 같이 돌 때.

**테스트:**
- macOS `cargo test --workspace` 1413개(Task 4 이후)
- 웹 시험 524개와 `typecheck`·`lint`·`build`
- `fmt`·`clippy -D warnings`(모든 기능)

Linux CI(프로브)에서 같은 시험과 linux-kvm·ldap 작업이 통과했다.

## 다음 계획으로 넘길 사항

- 계획 37(원격 클러스터)·38(원격 복제)이 남았다.
- 실제 IdP(Keycloak·Okta·Entra ID) 설정 메모는 각 제품 문서로 확인해야 한다.
- IdP 로그아웃 연동, SAML은 넣지 않았다.
