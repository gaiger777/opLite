//! ACME API(계획 73 §13): `/acme-accounts`·`/acme-dns`·`/certificates`. 루트 종류다 — 읽기 `cluster.read`(가린 보기 — 계정
//! 키·EAB HMAC·DNS 비밀·공유 키는 응답·watch·감사에 없다), 쓰기 `cluster.admin`(경로표). 모든 쓰기는 모든 노드가 `acme`를
//! 알아야 한다(409 `cluster_upgrading`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_types::acme::{
    AcmeAccount, AcmeAccountSpec, AcmeAccountStatus, AcmeDns, AcmeDnsSpec, AcmeDnsStatus,
    Certificate, CertificateInstall, CertificateKey, CertificateScope, CertificateSpec,
    CertificateStatus, ChallengeKind, DnsProvider, ExternalAccountBinding, Propagation,
    RenewPolicy, Secret, redact_account_json, redact_certificate_status_json, redact_dns_json,
};
use opl_types::node::{ACME, HTTP01, NodeSpec};
use opl_types::resource::{Kind, Resource};
use opl_types::task::{Task, TaskOp, TaskSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::client::{AcmeClient, AcmeError};
use super::jws::AccountKey;
use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::resources::{List, check_name, if_match, lookup};
use crate::state::AppState;
use crate::tasks::create_task;

/// CA와 이야기하다 실패함 → API 오류
pub fn acme_api_error(e: AcmeError) -> ApiError {
    let message = e.to_string();
    match e {
        AcmeError::Http(_) | AcmeError::Server(_) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "acme_unreachable", message)
        }
        AcmeError::RateLimited { .. } => {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "acme_rate_limited", message)
        }
        _ => ApiError::bad_request(message).with_code("acme_rejected"),
    }
}

fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(e.to_string()).with_code("invalid_acme")
}

fn redacted<T: Serialize>(r: &T, redact: impl FnOnce(&mut Value)) -> Result<Json<Value>, ApiError> {
    let mut v = serde_json::to_value(r).map_err(ApiError::internal)?;
    redact(&mut v);
    Ok(Json(v))
}

pub(crate) fn account_view(a: &AcmeAccount) -> Result<Value, ApiError> {
    redacted(a, |v| redact_account_json(&mut v["spec"])).map(|j| j.0)
}

pub(crate) fn dns_view(d: &AcmeDns) -> Result<Value, ApiError> {
    redacted(d, |v| redact_dns_json(&mut v["spec"])).map(|j| j.0)
}

pub(crate) fn certificate_view(c: &Certificate) -> Result<Value, ApiError> {
    redacted(c, |v| redact_certificate_status_json(&mut v["status"])).map(|j| j.0)
}

fn list_view<S, F>(
    state: &AppState,
    principal: &Principal,
    kind: Kind,
    view: F,
) -> Result<Json<Value>, ApiError>
where
    S: opl_types::resource::ResourceSpec,
    F: Fn(&Resource<S, S::Status>) -> Result<Value, ApiError>,
{
    state.authorize(principal, Action::Read, kind)?;
    let (revision, items) = state.cluster.list::<S>()?;
    let items = items.iter().map(view).collect::<Result<Vec<_>, _>>()?;
    Ok(Json(
        serde_json::json!({ "revision": revision, "items": items }),
    ))
}

// ---- 계정 ----

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EabRequest {
    pub kid: String,
    /// base64url HMAC 키. 저장한다(Raft) — 응답·watch·감사에는 없다.
    #[schema(write_only)]
    pub hmac_key: String,
}

/// ACME 계정 만들기 본문
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAccountRequest {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// 디렉터리 URL 또는 별명(`letsencrypt`·`letsencrypt-staging`·`zerossl`·`google`)
    pub directory: String,
    #[serde(default)]
    #[schema(nullable = false)]
    pub email: Option<String>,
    /// CA 약관에 동의함(참이어야 한다)
    pub agree_tos: bool,
    #[serde(default)]
    #[schema(nullable = false)]
    pub eab: Option<EabRequest>,
    /// 사내 ACME·Pebble의 루트(PEM)
    #[serde(default)]
    #[schema(nullable = false)]
    pub ca_bundle: Option<String>,
}

/// 계정에서 바꿀 값(디렉터리·EAB는 바꿀 수 없다 — 새 계정을 만든다)
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountPatch {
    /// `null`이면 연락처를 지운다.
    #[serde(default, deserialize_with = "crate::backup_jobs::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub email: Option<Option<String>>,
    /// `null`이면 시스템 루트를 믿는다.
    #[serde(default, deserialize_with = "crate::backup_jobs::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub ca_bundle: Option<Option<String>>,
}

/// 계정을 CA에 등록한다(이미 있으면 같은 계정). status를 채운다.
async fn register(spec: &AcmeAccountSpec) -> Result<AcmeAccountStatus, AcmeError> {
    let url = spec
        .directory_url()
        .ok_or_else(|| AcmeError::Invalid("bad directory".into()))?;
    let key =
        AccountKey::from_pem(spec.key.expose()).map_err(|e| AcmeError::Invalid(e.to_string()))?;
    let mut client = AcmeClient::connect(url, spec.ca_bundle.as_deref(), key).await?;
    if client.directory.meta.external_account_required && spec.eab.is_none() {
        return Err(AcmeError::ExternalAccountRequired(
            "this CA needs eab.kid and eab.hmac_key".into(),
        ));
    }
    let eab = spec
        .eab
        .as_ref()
        .map(|e| (e.kid.as_str(), e.hmac_key.expose()));
    let account = client.register(spec.email.as_deref(), eab).await?;
    if let Some(email) = &spec.email {
        // 이미 있던 계정이면 연락처를 지금 값으로 맞춘다.
        client
            .post(
                &account,
                Some(&serde_json::json!({ "contact": [format!("mailto:{email}")] })),
            )
            .await?;
    }
    Ok(AcmeAccountStatus {
        url: Some(account),
        registered_at: Some(crate::backups::now() as i64),
        terms: client.directory.meta.terms_of_service.clone(),
        profiles: client.profiles(),
        error: None,
    })
}

#[utoipa::path(get, path = "/acme-accounts", tag = "acme",
    responses((status = 200, body = List<Resource<AcmeAccountSpec, AcmeAccountStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_accounts(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Value>, ApiError> {
    list_view::<AcmeAccountSpec, _>(&state, &principal, Kind::AcmeAccount, account_view)
}

#[utoipa::path(get, path = "/acme-accounts/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<AcmeAccountSpec, AcmeAccountStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_account(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::AcmeAccount)?;
    Ok(Json(account_view(&lookup::<AcmeAccountSpec>(
        &state.cluster,
        &key,
    )?)?))
}

/// ACME 계정을 만든다(201): 계정 키(ES256)를 서버가 만들고 CA에 등록한다(EAB가 있으면 묶는다). 약관에 동의해야 한다.
/// CA가 거절하면 400 `acme_rejected`, 닿지 않으면 502 `acme_unreachable`. 계정 키·EAB HMAC 키는 응답·watch·감사에 없다.
#[utoipa::path(post, path = "/acme-accounts", tag = "acme",
    request_body = CreateAccountRequest,
    responses((status = 201, body = Resource<AcmeAccountSpec, AcmeAccountStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn create_account(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateAccountRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_feature(&state.cluster, ACME)?;
    check_name(&req.name)?;
    let key = AccountKey::generate().map_err(ApiError::internal)?;
    let spec = AcmeAccountSpec {
        directory: req.directory,
        email: req.email.filter(|e| !e.is_empty()),
        agree_tos: req.agree_tos,
        eab: req.eab.map(|e| ExternalAccountBinding {
            kid: e.kid,
            hmac_key: Secret::new(e.hmac_key),
        }),
        ca_bundle: req.ca_bundle.filter(|c| !c.trim().is_empty()),
        key: Secret::new(key.to_pem()),
    };
    spec.validate().map_err(invalid)?;
    audit.detail(format!(
        "acme-account create {} ({}, tos agreed{})",
        req.name,
        spec.directory,
        if spec.eab.is_some() { ", eab" } else { "" }
    ));
    let mut account: AcmeAccount = Resource::new(req.name, spec);
    account.labels = req.labels;
    account.status = register(&account.spec).await.map_err(acme_api_error)?;
    state.cluster.create(&account).await?;
    Ok((StatusCode::CREATED, Json(account_view(&account)?)))
}

/// 계정의 연락처·루트를 바꾼다. 바꾸면 CA에 다시 등록해 본다(같은 계정 — 연락처를 맞춘다).
#[utoipa::path(patch, path = "/acme-accounts/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = AccountPatch,
    responses((status = 200, body = Resource<AcmeAccountSpec, AcmeAccountStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn patch_account(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<AccountPatch>,
) -> Result<Json<Value>, ApiError> {
    require_feature(&state.cluster, ACME)?;
    let account = lookup::<AcmeAccountSpec>(&state.cluster, &key)?;
    let expected = if_match(&headers)?.unwrap_or(account.revision);
    let mut spec = account.spec.clone();
    if let Some(e) = patch.email {
        spec.email = e.filter(|e| !e.is_empty());
    }
    if let Some(c) = patch.ca_bundle {
        spec.ca_bundle = c.filter(|c| !c.trim().is_empty());
    }
    spec.validate().map_err(invalid)?;
    audit.detail(format!("acme-account update {}", account.name));
    let status = register(&spec).await.map_err(acme_api_error)?;
    state
        .cluster
        .update_spec::<AcmeAccountSpec>(account.id, Some(expected), account.labels.clone(), &spec)
        .await?;
    state
        .cluster
        .update_status::<AcmeAccountSpec>(account.id, &status)
        .await?;
    Ok(Json(account_view(&lookup::<AcmeAccountSpec>(
        &state.cluster,
        &account.id.to_string(),
    )?)?))
}

/// 계정을 지운다(204). 쓰는 인증서가 있으면 409 `account_in_use`. CA 쪽 계정은 그대로 둔다.
#[utoipa::path(delete, path = "/acme-accounts/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_account(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let account = lookup::<AcmeAccountSpec>(&state.cluster, &key)?;
    audit.detail(format!("acme-account delete {}", account.name));
    let (_, certs) = state.cluster.list::<CertificateSpec>()?;
    if let Some(c) = certs.iter().find(|c| c.spec.account == account.name) {
        return Err(ApiError::conflict(
            "account_in_use",
            format!(
                "acme-account {:?} is used by certificate {:?}",
                account.name, c.name
            ),
        ));
    }
    state
        .cluster
        .delete::<AcmeAccountSpec>(account.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// CA에 다시 묻는다(디렉터리·등록 — 같은 계정). 결과를 status에 적고, 실패면 `status.error`에 적은 뒤 그 오류를 돌려준다.
#[utoipa::path(post, path = "/acme-accounts/{key}/actions/check", tag = "acme",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<AcmeAccountSpec, AcmeAccountStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn check_account(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_feature(&state.cluster, ACME)?;
    let mut account = lookup::<AcmeAccountSpec>(&state.cluster, &key)?;
    audit.detail(format!("acme-account check {}", account.name));
    let result = register(&account.spec).await;
    let status = match &result {
        Ok(s) => s.clone(),
        Err(e) => AcmeAccountStatus {
            error: Some(e.to_string()),
            ..account.status.clone()
        },
    };
    state
        .cluster
        .update_status::<AcmeAccountSpec>(account.id, &status)
        .await?;
    account.status = status;
    result.map_err(acme_api_error)?;
    Ok(Json(account_view(&account)?))
}

// ---- DNS 공급자 ----

/// DNS 공급자 만들기 본문(비밀은 저장 뒤 다시 보이지 않는다)
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateDnsRequest {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub provider: DnsProvider,
    #[serde(default)]
    #[schema(nullable = false)]
    pub propagation: Option<Propagation>,
    #[serde(default)]
    pub zones: Vec<String>,
}

/// DNS 공급자에서 바꿀 값. `provider`는 통째로(비밀도 다시) 준다.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DnsPatch {
    #[serde(default)]
    #[schema(nullable = false)]
    pub provider: Option<DnsProvider>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub propagation: Option<Propagation>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub zones: Option<Vec<String>>,
}

fn provider_kind(p: &DnsProvider) -> &'static str {
    match p {
        DnsProvider::Cloudflare { .. } => "cloudflare",
        DnsProvider::Route53 { .. } => "route53",
        DnsProvider::Rfc2136 { .. } => "rfc2136",
        DnsProvider::PowerDns { .. } => "power_dns",
        DnsProvider::Hook { .. } => "hook",
    }
}

/// 이 노드에서 만들 수 있는지까지 본다(TSIG base64·서버 주소 꼴).
fn check_dns(state: &AppState, spec: &AcmeDnsSpec) -> Result<(), ApiError> {
    spec.validate().map_err(invalid)?;
    super::dns::Provider::new(spec, &state.acme)
        .map(drop)
        .map_err(invalid)
}

#[utoipa::path(get, path = "/acme-dns", tag = "acme",
    responses((status = 200, body = List<Resource<AcmeDnsSpec, AcmeDnsStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_dns(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Value>, ApiError> {
    list_view::<AcmeDnsSpec, _>(&state, &principal, Kind::AcmeDns, dns_view)
}

#[utoipa::path(get, path = "/acme-dns/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<AcmeDnsSpec, AcmeDnsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_dns(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::AcmeDns)?;
    Ok(Json(dns_view(&lookup::<AcmeDnsSpec>(
        &state.cluster,
        &key,
    )?)?))
}

/// DNS-01 공급자를 만든다(201). 비밀(API 토큰·키·TSIG 비밀)은 응답·watch·감사에 없다.
#[utoipa::path(post, path = "/acme-dns", tag = "acme",
    request_body = CreateDnsRequest,
    responses((status = 201, body = Resource<AcmeDnsSpec, AcmeDnsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_dns(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateDnsRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_feature(&state.cluster, ACME)?;
    check_name(&req.name)?;
    let spec = AcmeDnsSpec {
        provider: req.provider,
        propagation: req.propagation.unwrap_or_default(),
        zones: req.zones,
    };
    check_dns(&state, &spec)?;
    audit.detail(format!(
        "acme-dns create {} ({})",
        req.name,
        provider_kind(&spec.provider)
    ));
    let mut dns: AcmeDns = Resource::new(req.name, spec);
    dns.labels = req.labels;
    state.cluster.create(&dns).await?;
    Ok((StatusCode::CREATED, Json(dns_view(&dns)?)))
}

#[utoipa::path(patch, path = "/acme-dns/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = DnsPatch,
    responses((status = 200, body = Resource<AcmeDnsSpec, AcmeDnsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_dns(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<DnsPatch>,
) -> Result<Json<Value>, ApiError> {
    require_feature(&state.cluster, ACME)?;
    let dns = lookup::<AcmeDnsSpec>(&state.cluster, &key)?;
    let expected = if_match(&headers)?.unwrap_or(dns.revision);
    let mut spec = dns.spec.clone();
    if let Some(p) = patch.provider {
        spec.provider = p;
    }
    if let Some(p) = patch.propagation {
        spec.propagation = p;
    }
    if let Some(z) = patch.zones {
        spec.zones = z;
    }
    check_dns(&state, &spec)?;
    audit.detail(format!(
        "acme-dns update {} ({})",
        dns.name,
        provider_kind(&spec.provider)
    ));
    state
        .cluster
        .update_spec::<AcmeDnsSpec>(dns.id, Some(expected), dns.labels.clone(), &spec)
        .await?;
    Ok(Json(dns_view(&lookup::<AcmeDnsSpec>(
        &state.cluster,
        &dns.id.to_string(),
    )?)?))
}

/// DNS 공급자를 지운다(204). 쓰는 인증서가 있으면 409 `dns_in_use`.
#[utoipa::path(delete, path = "/acme-dns/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_dns(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let dns = lookup::<AcmeDnsSpec>(&state.cluster, &key)?;
    audit.detail(format!("acme-dns delete {}", dns.name));
    let (_, certs) = state.cluster.list::<CertificateSpec>()?;
    if let Some(c) = certs
        .iter()
        .find(|c| matches!(&c.spec.challenge, ChallengeKind::Dns01 { dns: d } if *d == dns.name))
    {
        return Err(ApiError::conflict(
            "dns_in_use",
            format!(
                "acme-dns {:?} is used by certificate {:?}",
                dns.name, c.name
            ),
        ));
    }
    state
        .cluster
        .delete::<AcmeDnsSpec>(dns.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- 인증서 ----

/// 인증서 만들기 본문: `name`·`labels`와 인증서 스펙 칸(모르는 칸은 거절한다)
#[derive(Deserialize, ToSchema)]
#[serde(try_from = "serde_json::Map<String, Value>")]
pub struct CreateCertificateRequest {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(flatten)]
    pub spec: CertificateSpec,
}

impl TryFrom<serde_json::Map<String, Value>> for CreateCertificateRequest {
    type Error = String;

    // `flatten`은 안쪽의 `deny_unknown_fields`를 지키지 않으므로 손으로 나눈다.
    fn try_from(mut m: serde_json::Map<String, Value>) -> Result<Self, String> {
        let name = match m.remove("name") {
            Some(Value::String(n)) => n,
            _ => return Err("missing field `name`".into()),
        };
        let labels = match m.remove("labels") {
            None | Some(Value::Null) => BTreeMap::new(),
            Some(v) => serde_json::from_value(v).map_err(|e| format!("labels: {e}"))?,
        };
        let spec = serde_json::from_value(Value::Object(m)).map_err(|e| e.to_string())?;
        Ok(Self { name, labels, spec })
    }
}

/// 인증서에서 바꿀 값(주어진 것만). 이름·범위·도전·키가 바뀌면 리더가 다음 틱에 다시 받는다.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CertificatePatch {
    #[serde(default)]
    #[schema(nullable = false)]
    pub account: Option<String>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub names: Option<Vec<String>>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub scope: Option<CertificateScope>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub challenge: Option<ChallengeKind>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub key: Option<CertificateKey>,
    /// `null`이면 지운다.
    #[serde(default, deserialize_with = "crate::backup_jobs::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub profile: Option<Option<String>>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub renew: Option<RenewPolicy>,
    /// `null`이면 지운다.
    #[serde(default, deserialize_with = "crate::backup_jobs::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub preferred_chain: Option<Option<String>>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub install: Option<CertificateInstall>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub precheck: Option<bool>,
}

/// 스펙 검사: 꼴, 계정·DNS 공급자가 있는지, 키 종류, HTTP-01이면 대상 노드가 포트 80을 듣는지
fn check_certificate(state: &AppState, spec: &CertificateSpec) -> Result<(), ApiError> {
    spec.validate().map_err(invalid)?;
    if spec.key == CertificateKey::Rsa2048 {
        return Err(invalid(
            "rsa2048 keys cannot be generated by this build; use ecdsa-p256 or ecdsa-p384",
        ));
    }
    if state
        .cluster
        .get_by_name::<AcmeAccountSpec>(&spec.account)?
        .is_none()
    {
        return Err(invalid(format!(
            "acme-account {:?} not found",
            spec.account
        )));
    }
    match &spec.challenge {
        ChallengeKind::Dns01 { dns } => {
            if state.cluster.get_by_name::<AcmeDnsSpec>(dns)?.is_none() {
                return Err(invalid(format!("acme-dns {dns:?} not found")));
            }
        }
        ChallengeKind::Http01 => {
            let (_, nodes) = state.cluster.list::<NodeSpec>()?;
            let mut missing: Vec<String> = super::covered_nodes(spec, &nodes)
                .into_iter()
                .filter(|n| !n.status.features.iter().any(|f| f == HTTP01))
                .map(|n| n.name.clone())
                .collect();
            if let CertificateScope::PerNode { nodes: ids } = &spec.scope
                && let Some(id) = ids.iter().find(|id| !nodes.iter().any(|n| n.id == **id))
            {
                return Err(invalid(format!("node {id} not found")));
            }
            missing.sort();
            if !missing.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "HTTP-01 needs the port 80 listener: set [api] http_listen on node {}",
                    missing.join(", ")
                ))
                .with_code("http01_unavailable"));
            }
        }
    }
    Ok(())
}

fn summary(spec: &CertificateSpec) -> String {
    let scope = match spec.scope {
        CertificateScope::Shared => "shared",
        CertificateScope::PerNode { .. } => "per-node",
    };
    let challenge = match &spec.challenge {
        ChallengeKind::Http01 => "http-01".to_string(),
        ChallengeKind::Dns01 { dns } => format!("dns-01 {dns}"),
    };
    format!("{scope}, {challenge}")
}

#[utoipa::path(get, path = "/certificates", tag = "acme",
    responses((status = 200, body = List<Resource<CertificateSpec, CertificateStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_certificates(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Value>, ApiError> {
    list_view::<CertificateSpec, _>(&state, &principal, Kind::Certificate, certificate_view)
}

#[utoipa::path(get, path = "/certificates/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<CertificateSpec, CertificateStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_certificate(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::Certificate)?;
    Ok(Json(certificate_view(&lookup::<CertificateSpec>(
        &state.cluster,
        &key,
    )?)?))
}

/// 인증서를 만든다(201). 리더 `AcmeRenewer`가 다음 틱(60초)에 주문한다(`actions/renew`로 곧바로). HTTP-01이면 대상
/// 노드가 모두 `[api] http_listen`을 켜야 한다(400 `http01_unavailable`). `rsa2048`은 이 빌드가 키를 만들지 못한다(400).
#[utoipa::path(post, path = "/certificates", tag = "acme",
    request_body = CreateCertificateRequest,
    responses((status = 201, body = Resource<CertificateSpec, CertificateStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_certificate(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateCertificateRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_feature(&state.cluster, ACME)?;
    check_name(&req.name)?;
    check_certificate(&state, &req.spec)?;
    audit.detail(format!(
        "certificate create {} ({})",
        req.name,
        summary(&req.spec)
    ));
    let mut cert: Certificate = Resource::new(req.name, req.spec);
    cert.labels = req.labels;
    state.cluster.create(&cert).await?;
    Ok((StatusCode::CREATED, Json(certificate_view(&cert)?)))
}

#[utoipa::path(patch, path = "/certificates/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = CertificatePatch,
    responses((status = 200, body = Resource<CertificateSpec, CertificateStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_certificate(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(p): Json<CertificatePatch>,
) -> Result<Json<Value>, ApiError> {
    require_feature(&state.cluster, ACME)?;
    let cert = lookup::<CertificateSpec>(&state.cluster, &key)?;
    let expected = if_match(&headers)?.unwrap_or(cert.revision);
    let mut spec = cert.spec.clone();
    if let Some(v) = p.account {
        spec.account = v;
    }
    if let Some(v) = p.names {
        spec.names = v;
    }
    if let Some(v) = p.scope {
        spec.scope = v;
    }
    if let Some(v) = p.challenge {
        spec.challenge = v;
    }
    if let Some(v) = p.key {
        spec.key = v;
    }
    if let Some(v) = p.profile {
        spec.profile = v;
    }
    if let Some(v) = p.renew {
        spec.renew = v;
    }
    if let Some(v) = p.preferred_chain {
        spec.preferred_chain = v;
    }
    if let Some(v) = p.install {
        spec.install = v;
    }
    if let Some(v) = p.precheck {
        spec.precheck = v;
    }
    check_certificate(&state, &spec)?;
    audit.detail(format!(
        "certificate update {} ({})",
        cert.name,
        summary(&spec)
    ));
    state
        .cluster
        .update_spec::<CertificateSpec>(cert.id, Some(expected), cert.labels.clone(), &spec)
        .await?;
    Ok(Json(certificate_view(&lookup::<CertificateSpec>(
        &state.cluster,
        &cert.id.to_string(),
    )?)?))
}

/// 인증서를 지운다(204). 끼워 둔 노드는 다음 차례(다른 ACME 인증서·내부 CA)로 돌아간다. CA 쪽 인증서는 취소하지 않는다.
#[utoipa::path(delete, path = "/certificates/{key}", tag = "acme",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_certificate(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let cert = lookup::<CertificateSpec>(&state.cluster, &key)?;
    audit.detail(format!("certificate delete {}", cert.name));
    state
        .cluster
        .delete::<CertificateSpec>(cert.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 만든 주문 Task들
#[derive(Debug, Serialize, ToSchema)]
pub struct RenewStarted {
    pub tasks: Vec<Task>,
}

/// 지금 주문한다(물러서기를 지우고 받을 것마다 `acme-order` Task — 노드마다면 온라인인 덮는 노드마다). 주문 Task가 이미
/// 돌면 409 `acme_order_running`.
#[utoipa::path(post, path = "/certificates/{key}/actions/renew", tag = "acme",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = RenewStarted), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn renew_certificate(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<RenewStarted>), ApiError> {
    require_feature(&state.cluster, ACME)?;
    let cert = lookup::<CertificateSpec>(&state.cluster, &key)?;
    audit.detail(format!("certificate renew {}", cert.name));
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::AcmeOrder { certificate, .. } if certificate == cert.id)
    }) {
        return Err(ApiError::conflict(
            "acme_order_running",
            format!(
                "an ACME order for certificate {:?} is already running",
                cert.name
            ),
        ));
    }
    let (_, nodes) = state.cluster.list::<NodeSpec>()?;
    let ready =
        |n: &&opl_types::node::Node| n.status.online && n.status.features.iter().any(|f| f == ACME);
    let targets: Vec<(Option<uuid::Uuid>, uuid::Uuid)> = match cert.spec.scope {
        CertificateScope::Shared => {
            let leader = state
                .cluster
                .members()
                .leader
                .map(|l| l.0)
                .unwrap_or(state.node);
            super::renewer::shared_runner(&nodes, leader)
                .map(|n| (None, n))
                .into_iter()
                .collect()
        }
        CertificateScope::PerNode { .. } => super::covered_nodes(&cert.spec, &nodes)
            .into_iter()
            .filter(ready)
            .map(|n| (Some(n.id), n.id))
            .collect(),
    };
    if targets.is_empty() {
        return Err(ApiError::conflict(
            "no_node",
            "no online node can order this certificate",
        ));
    }
    super::order::modify_status(&state.cluster, cert.id, |s| s.next_attempt_at = None)
        .await
        .map_err(ApiError::internal)?;
    let mut created = Vec::new();
    for (node, run_on) in targets {
        created.push(
            create_task(
                &state.cluster,
                TaskOp::AcmeOrder {
                    certificate: cert.id,
                    node,
                },
                cert.id,
                run_on,
            )
            .await?,
        );
    }
    Ok((StatusCode::ACCEPTED, Json(RenewStarted { tasks: created })))
}
