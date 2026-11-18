//! 2단계 인증 API(계획 62 설계 §10): TOTP 등록·확인, 복구 코드, 패스키(WebAuthn) 등록, 로그인의 둘째 단계,
//! 관리자 초기화.
//!
//! - 비밀(TOTP 비밀·복구 코드·챌린지·쿠키)은 로그·감사·오류 메시지에 넣지 않는다. TOTP 비밀은 등록 응답에서 한 번,
//!   복구 코드는 만들 때 한 번만 보인다. 저장은 `TwoFactor`(계정 종류, watch 불가) 하나다.
//! - 코드 확인은 로그인과 같은 시도 제한([`crate::limit::AttemptLimiter`], 키 `2fa:<사용자 id>`)을 받는다.
//! - TOTP 재사용 막기는 `last_totp_step`의 revision CAS(`update_status_checked`), 복구 코드는 스펙 CAS로 지운다.
//!   그래서 두 노드에 같은 코드를 동시에 내도 하나만 통과한다.
//! - 쓰기는 모든 노드가 `two-factor`를 알 때만 받는다(옛 노드는 대기 세션을 정식 세션으로 받는다).

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::header::HOST;
use axum::http::request::Parts;
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::auth::{ApiTokenSpec, User, UserSpec};
use opl_types::node::TWO_FACTOR;
use opl_types::realm::{RealmBackend, RealmSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::two_factor::{
    PENDING_LOGIN_SECS, Passkey, PendingLogin, RECOVERY_CODES, TOTP_DIGITS, TOTP_PERIOD,
    TOTP_SECRET_BYTES, Totp, TwoFactor, TwoFactorPolicy, TwoFactorSpec, TwoFactorStatus,
    decode_secret, encode_secret, verify_totp,
};
use ring::hmac;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{
    Action, PendingPrincipal, Principal, constant_time_eq, sha256_hex, unix_now, verify_password,
};
use crate::error::{ApiError, Json};
use crate::limit::ClientIp;
use crate::resources::lookup;
use crate::state::AppState;
use crate::webauthn::{self, ALGORITHMS, Expected, b64url, b64url_decode};

/// 인증 앱과 패스키에 보이는 이름
const ISSUER: &str = "Orbivisor";
/// 브라우저가 패스키를 기다리는 시간(밀리초)
const WEBAUTHN_TIMEOUT_MS: u64 = 120_000;
/// 패스키 등록 토큰의 수명(초)
const REGISTRATION_SECS: u64 = 300;
/// 사용자 한 명의 패스키 상한
const MAX_PASSKEYS: usize = 16;
const MAX_PASSKEY_NAME: usize = 64;
/// 등록 토큰 HMAC 키의 HKDF info(OIDC 상태 쿠키와 다른 키)
const REGISTRATION_KEY_INFO: &[u8] = b"orbivisor webauthn registration v1";
/// 다른 쓰기와 엇갈린 CAS를 다시 해 보는 횟수
const CAS_RETRIES: usize = 5;

/// 둘째 단계 방법
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SecondFactorMethod {
    Totp,
    Recovery,
    Webauthn,
}

/// 로그인 응답의 둘째 단계 안내. 쿠키는 대기 세션이고 [`PENDING_LOGIN_SECS`] 안에 끝내야 한다.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SecondFactorPrompt {
    /// 쓸 수 있는 방법. `enroll`이면 비어 있다.
    pub methods: Vec<SecondFactorMethod>,
    /// 패스키 로그인 옵션(`navigator.credentials.get({publicKey})`). 패스키가 없거나 이 접속(IP 주소)에서 쓸 수 없으면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub webauthn: Option<RequestOptions>,
    /// 정책이 요구하는데 요소가 없다: `/auth/2fa/totp`·`/auth/2fa/webauthn/register`로 등록하면 로그인이 끝난다.
    pub enroll: bool,
    /// 대기 세션이 끝나는 시각(유닉스 초)
    pub expires_at: u64,
}

/// 패스키 자격 증명 설명자
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CredentialDescriptor {
    #[serde(rename = "type")]
    pub kind: String,
    /// 자격 증명 id(base64url)
    pub id: String,
}

fn descriptor(p: &Passkey) -> CredentialDescriptor {
    CredentialDescriptor {
        kind: "public-key".into(),
        id: p.id.clone(),
    }
}

/// `PublicKeyCredentialRequestOptions`(이진 값은 base64url)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestOptions {
    pub challenge: String,
    pub timeout: u64,
    pub rp_id: String,
    pub allow_credentials: Vec<CredentialDescriptor>,
    pub user_verification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RelyingParty {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebauthnUser {
    /// 사용자 id 바이트(base64url)
    pub id: String,
    pub name: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CredentialParameter {
    #[serde(rename = "type")]
    pub kind: String,
    /// COSE 알고리즘(-7 ES256, -8 EdDSA, -257 RS256)
    pub alg: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatorSelection {
    pub resident_key: String,
    pub user_verification: String,
}

/// `PublicKeyCredentialCreationOptions`(이진 값은 base64url)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreationOptions {
    pub challenge: String,
    pub rp: RelyingParty,
    pub user: WebauthnUser,
    pub pub_key_cred_params: Vec<CredentialParameter>,
    pub timeout: u64,
    pub exclude_credentials: Vec<CredentialDescriptor>,
    pub authenticator_selection: AuthenticatorSelection,
    pub attestation: String,
}

/// 패스키 등록 시작: 브라우저에 넘길 옵션과, 끝낼 때 돌려보낼 서명한 토큰(5분)
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegistrationStart {
    pub token: String,
    pub options: CreationOptions,
}

/// `AuthenticatorAttestationResponse`(base64url). 브라우저 `toJSON()`의 다른 칸은 무시한다.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AttestationResponse {
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(rename = "attestationObject")]
    pub attestation_object: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RegistrationCredential {
    /// 자격 증명 id(base64url)
    pub id: String,
    pub response: AttestationResponse,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FinishRegistrationRequest {
    /// 등록 시작이 준 토큰
    pub token: String,
    /// 패스키 이름(1~64자)
    pub name: String,
    pub credential: RegistrationCredential,
}

/// `AuthenticatorAssertionResponse`(base64url)
#[derive(Debug, Deserialize, ToSchema)]
pub struct AssertionResponse {
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(rename = "authenticatorData")]
    pub authenticator_data: String,
    pub signature: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AssertionCredential {
    /// 자격 증명 id(base64url)
    pub id: String,
    pub response: AssertionResponse,
}

/// 둘째 단계: 셋 가운데 하나만
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecondFactorRequest {
    /// 인증 앱의 6자리 코드
    #[serde(default)]
    #[schema(nullable = false)]
    pub totp: Option<String>,
    /// 복구 코드(`xxxx-xxxx-xxxx-xxxx`). 한 번 쓰면 없어진다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub recovery: Option<String>,
    /// 패스키 응답(`navigator.credentials.get`)
    #[serde(default)]
    #[schema(nullable = false)]
    pub webauthn: Option<AssertionCredential>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeRequest {
    pub code: String,
}

/// 요소를 빼거나 복구 코드를 다시 만들 때의 본인 확인: TOTP 코드나 복구 코드(`code`), 또는 로컬 사용자의 비밀번호
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProofRequest {
    #[serde(default)]
    #[schema(nullable = false)]
    pub code: Option<String>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PasskeyView {
    pub id: String,
    pub name: String,
    pub rp_id: String,
    pub created_at: u64,
}

impl From<&Passkey> for PasskeyView {
    fn from(p: &Passkey) -> Self {
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            rp_id: p.rp_id.clone(),
            created_at: p.created_at,
        }
    }
}

/// 자기 2단계 인증 상태(비밀은 없다)
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TwoFactorView {
    /// 확인된 TOTP가 있다
    pub totp: bool,
    /// 확인을 기다리는 TOTP가 있다
    pub totp_pending: bool,
    /// 남은 복구 코드 수
    pub recovery_codes: usize,
    pub passkeys: Vec<PasskeyView>,
    /// 정책이 이 사용자에게 2단계 인증을 요구한다
    pub required: bool,
    /// 이 세션은 강제 등록 중이다(등록을 마치면 로그인이 끝난다)
    pub enrolling: bool,
    /// 이 접속에서 패스키에 쓸 RP ID. 없으면(IP 주소로 접속) 브라우저가 패스키를 막는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub rp_id: Option<String>,
    /// 모든 노드가 2단계 인증을 안다(아니면 등록을 받지 않는다)
    pub available: bool,
}

/// TOTP 등록: 인증 앱에 넣을 비밀과 `otpauth://` URI(QR). 다시 볼 수 없다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TotpEnrollment {
    pub secret: String,
    pub uri: String,
}

/// 새 복구 코드(한 번만 보인다). 이미 남은 코드가 있어 새로 만들지 않았으면 비어 있다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RecoveryCodes {
    pub codes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PasskeyAdded {
    pub passkey: PasskeyView,
    /// 첫 요소라 새로 만든 복구 코드(한 번만 보인다). 아니면 비어 있다.
    pub recovery_codes: Vec<String>,
}

fn invalid_second_factor(status: StatusCode) -> ApiError {
    ApiError::new(
        status,
        "invalid_second_factor",
        "the second factor is wrong or was already used",
    )
}

fn limiter_key(user: Uuid) -> String {
    format!("2fa:{user}")
}

fn load(cluster: &Cluster, user: Uuid) -> Result<Option<TwoFactor>, ApiError> {
    Ok(cluster.get_by_name::<TwoFactorSpec>(&user.to_string())?)
}

fn policy(cluster: &Cluster) -> Result<Option<TwoFactorPolicy>, ApiError> {
    Ok(crate::settings::current(cluster)?.spec.two_factor)
}

fn all_nodes_know(cluster: &Cluster) -> bool {
    crate::cluster::missing_feature(cluster, TWO_FACTOR).is_ok_and(|m| m.is_empty())
}

/// OIDC 영역의 사용자(IdP가 MFA를 맡는다)
fn oidc_user(cluster: &Cluster, user: &User) -> Result<bool, ApiError> {
    let Some(realm) = &user.spec.realm else {
        return Ok(false);
    };
    Ok(cluster
        .get_by_name::<RealmSpec>(realm)?
        .is_some_and(|r| matches!(r.spec.backend, RealmBackend::Oidc(_))))
}

/// 정책이 이 사용자에게 2단계 인증을 요구한다(OIDC 사용자는 아니다).
fn required(cluster: &Cluster, user: &User) -> Result<bool, ApiError> {
    let Some(policy) = policy(cluster)? else {
        return Ok(false);
    };
    Ok(policy.requires(user.spec.role, user.spec.realm.as_deref()) && !oidc_user(cluster, user)?)
}

/// 요청의 호스트와 scheme(믿는 프록시가 알린 것)
#[derive(Debug, Clone)]
pub struct RequestHost {
    host: Option<String>,
    proto: Option<&'static str>,
}

impl<S: Send + Sync> FromRequestParts<S> for RequestHost {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self {
            host: parts
                .headers
                .get(HOST)
                .and_then(|h| h.to_str().ok())
                .map(str::to_string),
            proto: crate::proxy::forwarded_proto(parts),
        })
    }
}

/// `host[:port]`의 호스트 부분(IPv6는 대괄호째)
fn host_name(host: &str) -> &str {
    if host.starts_with('[') {
        return host
            .split_once(']')
            .map_or(host, |(h, _)| &host[..h.len() + 1]);
    }
    host.rsplit_once(':').map_or(host, |(h, _)| h)
}

/// 패스키의 RP ID: 정책의 `rp_id`, 없으면 요청 호스트. IP 주소면 None(브라우저가 WebAuthn을 막는다).
fn rp_id(cluster: &Cluster, host: &RequestHost) -> Result<Option<String>, ApiError> {
    if let Some(rp) = policy(cluster)?.and_then(|p| p.rp_id) {
        return Ok(Some(rp));
    }
    let Some(host) = &host.host else {
        return Ok(None);
    };
    let name = host_name(host).to_ascii_lowercase();
    if name.is_empty() || name.starts_with('[') || name.parse::<IpAddr>().is_ok() {
        return Ok(None);
    }
    Ok(Some(name))
}

/// `clientDataJSON.origin`으로 받는 값: 요청 호스트(scheme을 모르면 둘 다)와 허용 출처 가운데 호스트가 RP ID이거나
/// 그 하위 도메인인 것
fn origins(cluster: &Cluster, host: &RequestHost, rp_id: &str) -> Result<Vec<String>, ApiError> {
    let mut out = Vec::new();
    if let Some(h) = &host.host {
        match host.proto {
            Some(p) => out.push(format!("{p}://{h}")),
            None => {
                out.push(format!("https://{h}"));
                out.push(format!("http://{h}"));
            }
        }
    }
    out.extend(crate::settings::current(cluster)?.spec.web_origins);
    out.retain(|o| {
        let Some((_, rest)) = o.split_once("://") else {
            return false;
        };
        let name = host_name(rest).to_ascii_lowercase();
        name == rp_id || name.ends_with(&format!(".{rp_id}"))
    });
    Ok(out)
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("system random source");
    buf
}

/// 복구 코드 `n`개와 그 SHA-256. 80비트(base32 16자, `xxxx-xxxx-xxxx-xxxx`)라 느린 해시가 필요 없다(토큰과 같다).
fn new_recovery_codes(n: usize) -> (Vec<String>, Vec<String>) {
    let codes: Vec<String> = (0..n)
        .map(|_| {
            let text = encode_secret(&random_bytes(10)).to_ascii_lowercase();
            text.as_bytes()
                .chunks(4)
                .map(|c| std::str::from_utf8(c).expect("base32 is ascii"))
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect();
    let hashes = codes
        .iter()
        .map(|c| sha256_hex(&normalize_recovery(c)))
        .collect();
    (codes, hashes)
}

/// 복구 코드의 비교 형식: 공백·`-`를 빼고 소문자
fn normalize_recovery(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `otpauth://` URI의 이름 부분(RFC 3986 unreserved 밖은 퍼센트 인코딩)
fn percent(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn otpauth_uri(user: &str, secret: &str) -> String {
    format!(
        "otpauth://totp/{issuer}:{user}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits={TOTP_DIGITS}&period={TOTP_PERIOD}",
        issuer = percent(ISSUER),
        user = percent(user),
    )
}

fn retryable(e: &ClusterError) -> bool {
    matches!(
        e,
        ClusterError::Rejected(
            Rejection::RevisionMismatch { .. }
                | Rejection::AlreadyExists { .. }
                | Rejection::NameTaken { .. }
                | Rejection::NotFound { .. }
        )
    )
}

/// 사용자의 `TwoFactor` 스펙을 `change`로 바꿔 쓴다(없으면 만든다, revision CAS, 엇갈리면 다시 읽어 다시 한다).
/// 바꾼 뒤 요소·복구 코드가 모두 없으면 리소스를 지운다. 쓴 리소스(지웠으면 None)를 돌려준다.
async fn write_spec(
    cluster: &Cluster,
    user: Uuid,
    mut change: impl FnMut(&mut TwoFactorSpec) -> Result<(), ApiError>,
) -> Result<Option<TwoFactor>, ApiError> {
    for _ in 0..CAS_RETRIES {
        let current = load(cluster, user)?;
        let mut spec = current.as_ref().map_or_else(
            || TwoFactorSpec {
                user,
                totp: None,
                recovery: vec![],
                webauthn: vec![],
            },
            |t| t.spec.clone(),
        );
        change(&mut spec)?;
        let empty = spec.totp.is_none() && spec.webauthn.is_empty() && spec.recovery.is_empty();
        let written = match (&current, empty) {
            (None, true) => return Ok(None),
            (Some(t), true) => cluster
                .delete::<TwoFactorSpec>(t.id, Some(t.revision))
                .await
                .map(|_| None),
            (Some(t), false) => cluster
                .update_spec(t.id, Some(t.revision), t.labels.clone(), &spec)
                .await
                .map(|revision| {
                    Some(TwoFactor {
                        spec: spec.clone(),
                        revision,
                        ..t.clone()
                    })
                }),
            (None, false) => {
                let mut t: TwoFactor = Resource::new(user.to_string(), spec.clone());
                cluster.create(&t).await.map(|revision| {
                    t.revision = revision;
                    Some(t)
                })
            }
        };
        match written {
            Ok(t) => return Ok(t),
            Err(e) if retryable(&e) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the second factors changed concurrently; try again",
    ))
}

/// 확인된 TOTP 코드를 쓴다. 맞고 아직 쓰지 않은 칸이면 `last_totp_step`을 CAS로 올리고 참이다.
async fn use_totp(cluster: &Cluster, user: Uuid, code: &str) -> Result<bool, ApiError> {
    for _ in 0..CAS_RETRIES {
        let Some(tf) = load(cluster, user)? else {
            return Ok(false);
        };
        let Some(secret) = tf
            .spec
            .totp
            .as_ref()
            .filter(|t| t.confirmed)
            .and_then(|t| decode_secret(&t.secret))
        else {
            return Ok(false);
        };
        let now = unix_now();
        let Some(step) = verify_totp(&secret, code, now, tf.status.last_totp_step) else {
            return Ok(false);
        };
        let status = TwoFactorStatus {
            last_totp_step: step,
            last_used: now,
        };
        match cluster
            .update_status_checked::<TwoFactorSpec>(tf.id, tf.revision, &status)
            .await
        {
            Ok(_) => return Ok(true),
            // 다른 노드가 먼저 썼다: 다시 읽으면 같은 칸은 `last_totp_step` 이하라 거절된다.
            Err(e) if retryable(&e) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(false)
}

/// 복구 코드를 쓴다. 맞으면 스펙에서 CAS로 지우고 참이다.
async fn use_recovery(cluster: &Cluster, user: Uuid, code: &str) -> Result<bool, ApiError> {
    let code = normalize_recovery(code);
    if code.len() != 16 {
        return Ok(false);
    }
    let want = sha256_hex(&code);
    for _ in 0..CAS_RETRIES {
        let Some(tf) = load(cluster, user)? else {
            return Ok(false);
        };
        // 모든 해시와 견준다(어디서 맞았는지 시간으로 드러나지 않게).
        let mut found = None;
        for (i, h) in tf.spec.recovery.iter().enumerate() {
            if constant_time_eq(h, &want) {
                found = Some(i);
            }
        }
        let Some(i) = found else {
            return Ok(false);
        };
        let mut spec = tf.spec.clone();
        spec.recovery.remove(i);
        let now = unix_now();
        match cluster
            .update_spec(tf.id, Some(tf.revision), tf.labels.clone(), &spec)
            .await
        {
            Ok(revision) => {
                let status = TwoFactorStatus {
                    last_used: now,
                    ..tf.status.clone()
                };
                // 마지막 사용 시각은 놓쳐도 된다.
                let _ = cluster
                    .update_status_checked::<TwoFactorSpec>(tf.id, revision, &status)
                    .await;
                return Ok(true);
            }
            Err(e) if retryable(&e) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(false)
}

/// 패스키 로그인 응답을 확인하고 서명 횟수를 CAS로 적는다.
async fn use_passkey(
    cluster: &Cluster,
    user: Uuid,
    credential: &AssertionCredential,
    challenge: &[u8],
    origins: &[String],
) -> Result<bool, ApiError> {
    let decode = |s: &str| b64url_decode(s).ok_or_else(|| ApiError::bad_request("bad base64url"));
    let id = b64url(&decode(&credential.id)?);
    let client_data = decode(&credential.response.client_data_json)?;
    let auth_data = decode(&credential.response.authenticator_data)?;
    let sig = decode(&credential.response.signature)?;
    for _ in 0..CAS_RETRIES {
        let Some(tf) = load(cluster, user)? else {
            return Ok(false);
        };
        let Some(i) = tf.spec.webauthn.iter().position(|p| p.id == id) else {
            return Ok(false);
        };
        let passkey = &tf.spec.webauthn[i];
        let expected = Expected {
            challenge,
            rp_id: &passkey.rp_id,
            origins,
            user_verification: false,
        };
        let assertion = match webauthn::verify_assertion(
            &expected,
            &passkey.cose_key,
            passkey.sign_count,
            &client_data,
            &auth_data,
            &sig,
        ) {
            Ok(a) => a,
            Err(e) => {
                if e == webauthn::WebauthnError::Counter {
                    tracing::warn!(%user, passkey = %passkey.name, "passkey sign count went backwards; the authenticator may be cloned");
                } else {
                    tracing::debug!(%user, error = %e, "passkey assertion rejected");
                }
                return Ok(false);
            }
        };
        if assertion.sign_count == passkey.sign_count {
            // 횟수를 세지 않는 인증기(둘 다 0): 쓸 것이 없다. 챌린지는 대기 세션과 함께 한 번만 쓴다.
            return Ok(true);
        }
        let mut spec = tf.spec.clone();
        spec.webauthn[i].sign_count = assertion.sign_count;
        match cluster
            .update_spec(tf.id, Some(tf.revision), tf.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(true),
            Err(e) if retryable(&e) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(false)
}

/// 로그인 때 둘째 단계가 필요하면 대기 세션 내용과 안내. 모든 노드가 `two-factor`를 알 때만이다(옛 노드는 대기
/// 세션을 정식 세션으로 받는다).
pub(crate) fn login_prompt(
    state: &AppState,
    user: &User,
    host: &RequestHost,
) -> Result<Option<(PendingLogin, SecondFactorPrompt)>, ApiError> {
    if !all_nodes_know(&state.cluster) {
        return Ok(None);
    }
    let tf = load(&state.cluster, user.id)?;
    let has_factor = tf.as_ref().is_some_and(|t| t.spec.has_factor());
    let expires_at = unix_now() + PENDING_LOGIN_SECS;
    if !has_factor {
        if !required(&state.cluster, user)? {
            return Ok(None);
        }
        let pending = PendingLogin {
            expires_at,
            enroll: true,
            challenge: None,
        };
        let prompt = SecondFactorPrompt {
            methods: vec![],
            webauthn: None,
            enroll: true,
            expires_at,
        };
        return Ok(Some((pending, prompt)));
    }
    let spec = &tf.expect("has a factor").spec;
    let mut methods = Vec::new();
    if spec.totp.as_ref().is_some_and(|t| t.confirmed) {
        methods.push(SecondFactorMethod::Totp);
    }
    if !spec.recovery.is_empty() {
        methods.push(SecondFactorMethod::Recovery);
    }
    let mut webauthn = None;
    let mut challenge = None;
    if let Some(rp) = rp_id(&state.cluster, host)? {
        let allow: Vec<CredentialDescriptor> = spec
            .webauthn
            .iter()
            .filter(|p| p.rp_id == rp)
            .map(descriptor)
            .collect();
        if !allow.is_empty() {
            let c = b64url(&random_bytes(32));
            methods.push(SecondFactorMethod::Webauthn);
            webauthn = Some(RequestOptions {
                challenge: c.clone(),
                timeout: WEBAUTHN_TIMEOUT_MS,
                rp_id: rp,
                allow_credentials: allow,
                user_verification: "preferred".into(),
            });
            challenge = Some(c);
        }
    }
    Ok(Some((
        PendingLogin {
            expires_at,
            enroll: false,
            challenge,
        },
        SecondFactorPrompt {
            methods,
            webauthn,
            enroll: false,
            expires_at,
        },
    )))
}

/// 비밀번호 로그인의 둘째 단계. `totp`·`recovery`·`webauthn` 가운데 하나를 낸다. 맞으면 같은 세션 쿠키가 정식 세션이
/// 된다(200 사용자). 틀리면 401 `invalid_second_factor`이고 로그인과 같은 시도 제한을 받는다(429). 강제 등록 중인
/// 세션은 403 `enrollment_required`(등록 경로를 쓴다).
#[utoipa::path(post, path = "/auth/login/second-factor", tag = "auth", security(("session" = [])),
    request_body = SecondFactorRequest,
    responses((status = 200, body = crate::users::UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn second_factor(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    host: RequestHost,
    audit: AuditNote,
    login: PendingPrincipal,
    Json(req): Json<SecondFactorRequest>,
) -> Result<Json<crate::users::UserView>, ApiError> {
    if login.pending.enroll {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "enrollment_required",
            "the two-factor policy requires you to enroll a second factor first",
        ));
    }
    let given = [
        req.totp.is_some(),
        req.recovery.is_some(),
        req.webauthn.is_some(),
    ];
    if given.iter().filter(|g| **g).count() != 1 {
        return Err(ApiError::bad_request(
            "give one of totp, recovery or webauthn",
        ));
    }
    let user = login.user.id;
    let attempt = state.attempts.begin(&limiter_key(user), ip)?;
    let (ok, method) = if let Some(code) = &req.totp {
        (use_totp(&state.cluster, user, code).await?, "totp")
    } else if let Some(code) = &req.recovery {
        (use_recovery(&state.cluster, user, code).await?, "recovery")
    } else {
        let credential = req.webauthn.as_ref().expect("one was given");
        let ok = match &login.pending.challenge {
            Some(c) => {
                let challenge = b64url_decode(c).unwrap_or_default();
                let rp = rp_id(&state.cluster, &host)?.unwrap_or_default();
                let origins = origins(&state.cluster, &host, &rp)?;
                use_passkey(&state.cluster, user, credential, &challenge, &origins).await?
            }
            None => false,
        };
        (ok, "webauthn")
    };
    audit.detail(format!("second factor {method}"));
    if !ok {
        return Err(invalid_second_factor(StatusCode::UNAUTHORIZED));
    }
    state.attempts.success(&limiter_key(user), ip, attempt);
    state.complete_login(&login).await?;
    Ok(Json(crate::users::view(&state.cluster, login.user)?))
}

/// 2단계 인증을 다루는 주체: 정식 세션·API 토큰, 또는 강제 등록 중인 대기 세션
pub struct Enroller {
    principal: Principal,
    /// 강제 등록 중인 대기 세션(등록을 마치면 로그인을 끝낸다)
    pending: Option<PendingPrincipal>,
}

impl FromRequestParts<Arc<AppState>> for Enroller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        match Principal::from_request_parts(parts, state).await {
            Ok(principal) => Ok(Self {
                principal,
                pending: None,
            }),
            Err(e) if e.code == "second_factor_required" => {
                let login = PendingPrincipal::from_request_parts(parts, state).await?;
                if !login.pending.enroll {
                    return Err(e);
                }
                Ok(Self {
                    principal: login.principal(),
                    pending: Some(login),
                })
            }
            Err(e) => Err(e),
        }
    }
}

/// 범위 있는 API 토큰은 2단계 인증을 바꾸지 못한다(범위 밖의 계정 동작).
fn not_scoped(state: &AppState, principal: &Principal) -> Result<(), ApiError> {
    if let Some(id) = principal.token
        && state
            .cluster
            .get::<ApiTokenSpec>(id)?
            .is_some_and(|t| t.spec.scope.is_some())
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a scoped token cannot change second factors",
        ));
    }
    Ok(())
}

fn user_of(state: &AppState, principal: &Principal) -> Result<User, ApiError> {
    state
        .cluster
        .get::<UserSpec>(principal.user)?
        .ok_or_else(ApiError::unauthorized)
}

/// 쓰기 앞: 모든 노드가 `two-factor`를 알고, 범위 토큰이 아니고, OIDC 사용자가 아니다.
fn may_change(state: &AppState, principal: &Principal) -> Result<User, ApiError> {
    crate::cluster::require_feature(&state.cluster, TWO_FACTOR)?;
    not_scoped(state, principal)?;
    let user = user_of(state, principal)?;
    if oidc_user(&state.cluster, &user)? {
        return Err(ApiError::bad_request(
            "users of an OIDC realm use the multi-factor authentication of their identity provider",
        ));
    }
    Ok(user)
}

/// 강제 등록 중이면 등록을 마쳤으니 로그인을 끝낸다.
async fn finish_enrollment(state: &AppState, who: &Enroller) -> Result<(), ApiError> {
    match &who.pending {
        Some(login) => state.complete_login(login).await,
        None => Ok(()),
    }
}

/// 자기 2단계 인증 상태. 강제 등록 중인 대기 세션도 읽는다.
#[utoipa::path(get, path = "/auth/2fa", tag = "auth",
    responses((status = 200, body = TwoFactorView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_two_factor(
    State(state): State<Arc<AppState>>,
    host: RequestHost,
    who: Enroller,
) -> Result<Json<TwoFactorView>, ApiError> {
    let user = user_of(&state, &who.principal)?;
    let spec = load(&state.cluster, user.id)?.map(|t| t.spec);
    let totp = spec.as_ref().and_then(|s| s.totp.as_ref());
    Ok(Json(TwoFactorView {
        totp: totp.is_some_and(|t| t.confirmed),
        totp_pending: totp.is_some_and(|t| !t.confirmed),
        recovery_codes: spec.as_ref().map_or(0, |s| s.recovery.len()),
        passkeys: spec
            .as_ref()
            .map(|s| s.webauthn.iter().map(PasskeyView::from).collect())
            .unwrap_or_default(),
        required: required(&state.cluster, &user)?,
        enrolling: who.pending.is_some(),
        rp_id: rp_id(&state.cluster, &host)?,
        available: all_nodes_know(&state.cluster),
    }))
}

/// TOTP 등록을 시작한다: 새 비밀과 `otpauth://` URI(QR)를 한 번 준다. 첫 코드로 확인(`/auth/2fa/totp/confirm`)해야
/// 쓰인다. 확인 전에 다시 부르면 새 비밀로 바뀐다. 이미 켜져 있으면 409 `totp_enabled`(먼저 끈다).
#[utoipa::path(post, path = "/auth/2fa/totp", tag = "auth",
    responses((status = 200, body = TotpEnrollment), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn enroll_totp(
    State(state): State<Arc<AppState>>,
    who: Enroller,
) -> Result<Json<TotpEnrollment>, ApiError> {
    let user = may_change(&state, &who.principal)?;
    let secret = encode_secret(&random_bytes(TOTP_SECRET_BYTES));
    write_spec(&state.cluster, user.id, |spec| {
        if spec.totp.as_ref().is_some_and(|t| t.confirmed) {
            return Err(ApiError::conflict(
                "totp_enabled",
                "an authenticator app is already enabled; disable it first",
            ));
        }
        spec.totp = Some(Totp {
            secret: secret.clone(),
            confirmed: false,
            created_at: unix_now(),
        });
        Ok(())
    })
    .await?;
    Ok(Json(TotpEnrollment {
        uri: otpauth_uri(&user.name, &secret),
        secret,
    }))
}

/// 등록 중인 TOTP를 첫 코드로 확인해 켠다. 첫 요소면 복구 코드 10개를 만들어 한 번 준다. 강제 등록 중인 대기 세션은
/// 이것으로 로그인이 끝난다. 틀리면 403 `invalid_second_factor`(시도 제한).
#[utoipa::path(post, path = "/auth/2fa/totp/confirm", tag = "auth",
    request_body = CodeRequest,
    responses((status = 200, body = RecoveryCodes), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn confirm_totp(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    audit: AuditNote,
    who: Enroller,
    Json(req): Json<CodeRequest>,
) -> Result<Json<RecoveryCodes>, ApiError> {
    let user = may_change(&state, &who.principal)?;
    let attempt = state.attempts.begin(&limiter_key(user.id), ip)?;
    let (codes, hashes) = new_recovery_codes(RECOVERY_CODES);
    let mut issued = false;
    let mut step = None;
    let written = write_spec(&state.cluster, user.id, |spec| {
        let Some(totp) = spec.totp.as_mut().filter(|t| !t.confirmed) else {
            return Err(ApiError::conflict(
                "no_totp_enrollment",
                "start the authenticator app enrollment first",
            ));
        };
        let secret =
            decode_secret(&totp.secret).ok_or_else(|| ApiError::internal("bad TOTP secret"))?;
        // 확인 코드의 칸도 쓴 것으로 적는다(아래 상태 쓰기). 등록 중 비밀은 로그인에 쓰이지 않았으므로 0부터 본다.
        step = verify_totp(&secret, &req.code, unix_now(), 0);
        if step.is_none() {
            return Err(invalid_second_factor(StatusCode::FORBIDDEN));
        }
        totp.confirmed = true;
        issued = spec.recovery.is_empty();
        if issued {
            spec.recovery = hashes.clone();
        }
        Ok(())
    })
    .await?
    .ok_or_else(|| ApiError::internal("two-factor record vanished"))?;
    state.attempts.success(&limiter_key(user.id), ip, attempt);
    if let Some(step) = step {
        let status = TwoFactorStatus {
            last_totp_step: step.max(written.status.last_totp_step),
            last_used: unix_now(),
        };
        if let Err(e) = state
            .cluster
            .update_status_checked::<TwoFactorSpec>(written.id, written.revision, &status)
            .await
        {
            tracing::debug!(user = %user.name, error = %e, "TOTP confirmation step not recorded");
        }
    }
    audit.detail("2fa totp on");
    finish_enrollment(&state, &who).await?;
    Ok(Json(RecoveryCodes {
        codes: if issued { codes } else { vec![] },
    }))
}

/// 본인 확인: TOTP 코드, 복구 코드(쓰면 없어진다), 또는 로컬 사용자의 비밀번호. 시도 제한을 받는다.
async fn check_proof(
    state: &AppState,
    ip: IpAddr,
    user: &User,
    proof: &ProofRequest,
) -> Result<(), ApiError> {
    let attempt = state.attempts.begin(&limiter_key(user.id), ip)?;
    let ok = match (&proof.code, &proof.password) {
        (Some(code), None) => {
            use_totp(&state.cluster, user.id, code).await?
                || use_recovery(&state.cluster, user.id, code).await?
        }
        (None, Some(password)) => {
            if user.spec.realm.is_some() {
                return Err(ApiError::bad_request(
                    "external users confirm with a code from their authenticator app or a recovery code",
                ));
            }
            verify_password(password.clone(), user.spec.password_hash.clone()).await
        }
        _ => {
            return Err(ApiError::bad_request(
                "give code (authenticator app or recovery code) or password",
            ));
        }
    };
    if !ok {
        return Err(invalid_second_factor(StatusCode::FORBIDDEN));
    }
    state.attempts.success(&limiter_key(user.id), ip, attempt);
    Ok(())
}

/// 요소를 뺀 뒤: 남은 요소가 없으면 복구 코드도 지우고, 정책이 요구하면 거절한다.
fn after_removal(spec: &mut TwoFactorSpec, required: bool) -> Result<(), ApiError> {
    if spec.has_factor() {
        return Ok(());
    }
    if required {
        return Err(ApiError::conflict(
            "two_factor_required",
            "the two-factor policy requires at least one second factor",
        ));
    }
    spec.recovery.clear();
    spec.totp = None;
    Ok(())
}

/// TOTP를 끈다(등록 중인 것도 지운다). 본인 확인(`code` 또는 `password`)이 필요하다. 남은 요소가 없으면 복구 코드도
/// 지우고, 정책이 요구하는 사용자의 마지막 요소면 409 `two_factor_required`.
#[utoipa::path(delete, path = "/auth/2fa/totp", tag = "auth",
    request_body = ProofRequest,
    responses((status = 204, description = "Disabled"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn disable_totp(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    audit: AuditNote,
    principal: Principal,
    Json(req): Json<ProofRequest>,
) -> Result<StatusCode, ApiError> {
    let user = may_change(&state, &principal)?;
    let confirmed = load(&state.cluster, user.id)?
        .and_then(|t| t.spec.totp)
        .is_some_and(|t| t.confirmed);
    // 확인 전 등록만 있으면 본인 확인 없이 지운다(쓰인 적이 없다).
    if confirmed {
        check_proof(&state, ip, &user, &req).await?;
    }
    let required = required(&state.cluster, &user)?;
    write_spec(&state.cluster, user.id, |spec| {
        spec.totp = None;
        after_removal(spec, required)
    })
    .await?;
    audit.detail("2fa totp off");
    Ok(StatusCode::NO_CONTENT)
}

/// 복구 코드 10개를 새로 만든다(남은 옛 코드는 모두 못 쓴다). 요소가 있어야 하고 본인 확인이 필요하다.
#[utoipa::path(post, path = "/auth/2fa/recovery", tag = "auth",
    request_body = ProofRequest,
    responses((status = 200, body = RecoveryCodes), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn regenerate_recovery(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    audit: AuditNote,
    principal: Principal,
    Json(req): Json<ProofRequest>,
) -> Result<Json<RecoveryCodes>, ApiError> {
    let user = may_change(&state, &principal)?;
    if !load(&state.cluster, user.id)?.is_some_and(|t| t.spec.has_factor()) {
        return Err(ApiError::conflict(
            "no_second_factor",
            "enable an authenticator app or a passkey first",
        ));
    }
    check_proof(&state, ip, &user, &req).await?;
    let (codes, hashes) = new_recovery_codes(RECOVERY_CODES);
    write_spec(&state.cluster, user.id, |spec| {
        if !spec.has_factor() {
            return Err(ApiError::conflict(
                "no_second_factor",
                "enable an authenticator app or a passkey first",
            ));
        }
        spec.recovery = hashes.clone();
        Ok(())
    })
    .await?;
    audit.detail("2fa recovery regenerate");
    Ok(Json(RecoveryCodes { codes }))
}

/// 등록 토큰의 내용(서명해 클라이언트에 맡긴다. 어느 노드로 끝내도 된다)
#[derive(Serialize, Deserialize)]
struct RegistrationState {
    user: Uuid,
    challenge: String,
    rp_id: String,
    expires_at: u64,
}

/// 등록 토큰 HMAC 키(클러스터 CA 개인키에서 HKDF, OIDC 상태 쿠키와 다른 info)
fn registration_key(state: &AppState) -> Result<hmac::Key, ApiError> {
    let info = crate::cluster::cluster_info(state)?;
    let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, &[])
        .extract(info.spec.ca_key_pem.as_bytes());
    let okm = prk
        .expand(&[REGISTRATION_KEY_INFO], hmac::HMAC_SHA256)
        .map_err(|_| ApiError::internal("HKDF expand"))?;
    Ok(hmac::Key::from(okm))
}

fn seal(key: &hmac::Key, s: &RegistrationState) -> Result<String, ApiError> {
    let body = b64url(&serde_json::to_vec(s).map_err(ApiError::internal)?);
    let tag = hmac::sign(key, body.as_bytes());
    Ok(format!("{body}.{}", b64url(tag.as_ref())))
}

fn open(key: &hmac::Key, token: &str) -> Option<RegistrationState> {
    let (body, tag) = token.split_once('.')?;
    hmac::verify(key, body.as_bytes(), &b64url_decode(tag)?).ok()?;
    serde_json::from_slice(&b64url_decode(body)?).ok()
}

fn bad_registration(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_passkey", message)
}

/// 패스키 등록을 시작한다: `navigator.credentials.create({publicKey: options})`에 넘길 옵션과 끝낼 때 돌려보낼 토큰.
/// RP ID는 정책의 `rp_id` 또는 요청 호스트이고, IP 주소로 접속했으면 400 `webauthn_unavailable`(브라우저가 막는다).
#[utoipa::path(post, path = "/auth/2fa/webauthn/register", tag = "auth",
    responses((status = 200, body = RegistrationStart), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn start_passkey(
    State(state): State<Arc<AppState>>,
    host: RequestHost,
    who: Enroller,
) -> Result<Json<RegistrationStart>, ApiError> {
    let user = may_change(&state, &who.principal)?;
    let Some(rp) = rp_id(&state.cluster, &host)? else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "webauthn_unavailable",
            "passkeys need a domain name; open the web UI by its DNS name or set the two-factor rp_id",
        ));
    };
    let existing = load(&state.cluster, user.id)?
        .map(|t| t.spec.webauthn)
        .unwrap_or_default();
    if existing.len() >= MAX_PASSKEYS {
        return Err(ApiError::conflict(
            "too_many_passkeys",
            format!("a user can have at most {MAX_PASSKEYS} passkeys"),
        ));
    }
    let challenge = b64url(&random_bytes(32));
    let token = seal(
        &registration_key(&state)?,
        &RegistrationState {
            user: user.id,
            challenge: challenge.clone(),
            rp_id: rp.clone(),
            expires_at: unix_now() + REGISTRATION_SECS,
        },
    )?;
    Ok(Json(RegistrationStart {
        token,
        options: CreationOptions {
            challenge,
            rp: RelyingParty {
                id: rp,
                name: ISSUER.into(),
            },
            user: WebauthnUser {
                id: b64url(user.id.as_bytes()),
                name: user.name.clone(),
                display_name: user.name.clone(),
            },
            pub_key_cred_params: ALGORITHMS
                .iter()
                .map(|alg| CredentialParameter {
                    kind: "public-key".into(),
                    alg: *alg,
                })
                .collect(),
            timeout: WEBAUTHN_TIMEOUT_MS,
            exclude_credentials: existing.iter().map(descriptor).collect(),
            authenticator_selection: AuthenticatorSelection {
                resident_key: "preferred".into(),
                user_verification: "preferred".into(),
            },
            attestation: "none".into(),
        },
    }))
}

fn check_passkey_name(name: &str) -> Result<(), ApiError> {
    let n = name.chars().count();
    if n == 0 || n > MAX_PASSKEY_NAME || name.chars().any(char::is_control) || name.trim() != name {
        return Err(ApiError::bad_request(format!(
            "a passkey name has 1-{MAX_PASSKEY_NAME} characters without control characters or surrounding spaces"
        )));
    }
    Ok(())
}

/// 패스키 등록을 끝낸다(201). 첫 요소면 복구 코드 10개를 만들어 한 번 준다. 강제 등록 중인 대기 세션은 이것으로
/// 로그인이 끝난다. 토큰이 틀렸거나 지났거나 응답이 맞지 않으면 400 `invalid_passkey`.
#[utoipa::path(post, path = "/auth/2fa/webauthn/register/finish", tag = "auth",
    request_body = FinishRegistrationRequest,
    responses((status = 201, body = PasskeyAdded), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn finish_passkey(
    State(state): State<Arc<AppState>>,
    host: RequestHost,
    audit: AuditNote,
    who: Enroller,
    Json(req): Json<FinishRegistrationRequest>,
) -> Result<(StatusCode, Json<PasskeyAdded>), ApiError> {
    let user = may_change(&state, &who.principal)?;
    check_passkey_name(&req.name)?;
    let reg = open(&registration_key(&state)?, &req.token)
        .filter(|r| r.user == user.id && r.expires_at > unix_now())
        .ok_or_else(|| {
            bad_registration("the registration expired or belongs to someone else; start again")
        })?;
    let challenge = b64url_decode(&reg.challenge).unwrap_or_default();
    let origins = origins(&state.cluster, &host, &reg.rp_id)?;
    let decode = |s: &str| b64url_decode(s).ok_or_else(|| bad_registration("bad base64url"));
    let client_data = decode(&req.credential.response.client_data_json)?;
    let attestation = decode(&req.credential.response.attestation_object)?;
    let registration = webauthn::verify_registration(
        &Expected {
            challenge: &challenge,
            rp_id: &reg.rp_id,
            origins: &origins,
            user_verification: false,
        },
        &client_data,
        &attestation,
    )
    .map_err(|e| bad_registration(format!("the passkey response is not valid: {e}")))?;
    if b64url_decode(&req.credential.id).as_deref() != Some(registration.credential_id.as_slice()) {
        return Err(bad_registration(
            "the credential id does not match the authenticator data",
        ));
    }
    let id = b64url(&registration.credential_id);
    let passkey = Passkey {
        id: id.clone(),
        name: req.name.clone(),
        rp_id: reg.rp_id.clone(),
        cose_key: registration.cose_key,
        sign_count: registration.sign_count,
        created_at: unix_now(),
    };
    let (codes, hashes) = new_recovery_codes(RECOVERY_CODES);
    let mut issued = false;
    write_spec(&state.cluster, user.id, |spec| {
        if spec.webauthn.iter().any(|p| p.id == id) {
            return Err(ApiError::conflict(
                "passkey_exists",
                "this passkey is already registered",
            ));
        }
        if spec.webauthn.len() >= MAX_PASSKEYS {
            return Err(ApiError::conflict(
                "too_many_passkeys",
                format!("a user can have at most {MAX_PASSKEYS} passkeys"),
            ));
        }
        spec.webauthn.push(passkey.clone());
        issued = spec.recovery.is_empty();
        if issued {
            spec.recovery = hashes.clone();
        }
        Ok(())
    })
    .await?;
    audit.detail(format!("2fa passkey add {}", req.name));
    finish_enrollment(&state, &who).await?;
    Ok((
        StatusCode::CREATED,
        Json(PasskeyAdded {
            passkey: PasskeyView::from(&passkey),
            recovery_codes: if issued { codes } else { vec![] },
        }),
    ))
}

/// 패스키를 지운다. 본인 확인(`code` 또는 `password`)이 필요하고, 정책이 요구하는 사용자의 마지막 요소면 409
/// `two_factor_required`.
#[utoipa::path(delete, path = "/auth/2fa/webauthn/{id}", tag = "auth",
    params(("id" = String, Path, description = "passkey credential id (base64url)")),
    request_body = ProofRequest,
    responses((status = 204, description = "Removed"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_passkey(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    audit: AuditNote,
    principal: Principal,
    Path(id): Path<String>,
    Json(req): Json<ProofRequest>,
) -> Result<StatusCode, ApiError> {
    let user = may_change(&state, &principal)?;
    let name = load(&state.cluster, user.id)?
        .and_then(|t| t.spec.webauthn.into_iter().find(|p| p.id == id))
        .map(|p| p.name)
        .ok_or_else(|| ApiError::not_found(format!("passkey {id:?} not found")))?;
    check_proof(&state, ip, &user, &req).await?;
    let required = required(&state.cluster, &user)?;
    write_spec(&state.cluster, user.id, |spec| {
        spec.webauthn.retain(|p| p.id != id);
        after_removal(spec, required)
    })
    .await?;
    audit.detail(format!("2fa passkey remove {name}"));
    Ok(StatusCode::NO_CONTENT)
}

/// 관리자 초기화: 사용자의 2단계 인증 요소를 모두 지우고 세션을 끊는다(API 토큰은 남는다). 자기 자신은 409
/// `cannot_reset_self`(자기 요소는 `/auth/2fa`에서 뺀다).
#[utoipa::path(delete, path = "/users/{key}/two-factor", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Reset"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn reset_two_factor(
    State(state): State<Arc<AppState>>,
    audit: AuditNote,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    let user = lookup::<UserSpec>(&state.cluster, &key)?;
    if user.id == principal.user {
        return Err(ApiError::conflict(
            "cannot_reset_self",
            "you cannot reset your own second factors",
        ));
    }
    if let Some(tf) = load(&state.cluster, user.id)? {
        match state.cluster.delete::<TwoFactorSpec>(tf.id, None).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
            Err(e) => return Err(e.into()),
        }
    }
    // 세션 세대를 올려 모든 노드의 세션(대기 세션 포함)을 끊는다.
    let mut current = user.clone();
    for _ in 0..CAS_RETRIES {
        let mut spec = current.spec.clone();
        spec.session_epoch += 1;
        match state
            .cluster
            .update_spec(
                current.id,
                Some(current.revision),
                current.labels.clone(),
                &spec,
            )
            .await
        {
            Ok(_) => break,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                current = lookup::<UserSpec>(&state.cluster, &user.id.to_string())?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    if let Err(e) = state.end_user_sessions(user.id).await {
        tracing::warn!(user = %user.name, error = %e.message, "sessions not deleted");
    }
    audit.detail(format!("user 2fa-reset {}", user.name));
    Ok(StatusCode::NO_CONTENT)
}

/// 사용자 목록에 보일 "2FA 켬"(확인된 TOTP 또는 패스키)
pub(crate) fn enabled_users(
    cluster: &Cluster,
) -> Result<std::collections::HashSet<Uuid>, ApiError> {
    Ok(cluster
        .list::<TwoFactorSpec>()?
        .1
        .into_iter()
        .filter(|t| t.spec.has_factor())
        .map(|t| t.spec.user)
        .collect())
}

/// 정책 검사(설정 PATCH): 스펙 검증과 영역(`local` 말고는 있는 LDAP 영역이어야 한다. OIDC는 IdP가 MFA를 맡는다).
pub(crate) fn check_policy(cluster: &Cluster, policy: &TwoFactorPolicy) -> Result<(), ApiError> {
    policy
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    for realm in &policy.realms {
        if realm == opl_types::two_factor::LOCAL_REALM {
            continue;
        }
        match cluster.get_by_name::<RealmSpec>(realm)? {
            None => {
                return Err(ApiError::bad_request(format!("unknown realm {realm:?}")));
            }
            Some(r) if matches!(r.spec.backend, RealmBackend::Oidc(_)) => {
                return Err(ApiError::bad_request(format!(
                    "realm {realm} is an OIDC realm; its identity provider handles multi-factor authentication"
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_codes_are_80_bits_and_hashed_normalized() {
        let (codes, hashes) = new_recovery_codes(RECOVERY_CODES);
        assert_eq!(codes.len(), 10);
        assert_eq!(hashes.len(), 10);
        for (c, h) in codes.iter().zip(&hashes) {
            assert_eq!(c.len(), 19, "{c}");
            assert_eq!(c.matches('-').count(), 3, "{c}");
            assert!(
                c.bytes()
                    .all(|b| b == b'-' || b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
            );
            assert_eq!(&sha256_hex(&normalize_recovery(&c.to_uppercase())), h);
            assert_eq!(&sha256_hex(&normalize_recovery(&c.replace('-', " "))), h);
        }
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), 10);
    }

    #[test]
    fn otpauth_uri_escapes_the_label() {
        assert_eq!(
            otpauth_uri("alice@corp", "ABC"),
            "otpauth://totp/Orbivisor:alice%40corp?secret=ABC&issuer=Orbivisor&algorithm=SHA1&digits=6&period=30"
        );
    }

    #[test]
    fn host_names_and_rp_ids() {
        assert_eq!(host_name("opl.example.com:8006"), "opl.example.com");
        assert_eq!(host_name("opl.example.com"), "opl.example.com");
        assert_eq!(host_name("[::1]:8006"), "[::1]");
        assert_eq!(host_name("10.0.0.1:8006"), "10.0.0.1");
    }

    #[test]
    fn registration_tokens_are_signed() {
        let key = hmac::Key::new(hmac::HMAC_SHA256, b"k");
        let s = RegistrationState {
            user: Uuid::from_u128(1),
            challenge: "c".into(),
            rp_id: "a.example".into(),
            expires_at: 9,
        };
        let token = seal(&key, &s).unwrap();
        let back = open(&key, &token).unwrap();
        assert_eq!((back.user, back.rp_id.as_str()), (s.user, "a.example"));
        let other = hmac::Key::new(hmac::HMAC_SHA256, b"other");
        assert!(open(&other, &token).is_none());
        let (body, tag) = token.split_once('.').unwrap();
        let forged = b64url(
            &serde_json::to_vec(&RegistrationState {
                user: Uuid::from_u128(2),
                ..s
            })
            .unwrap(),
        );
        assert!(open(&key, &format!("{forged}.{tag}")).is_none());
        assert!(open(&key, body).is_none());
    }

    #[test]
    fn removing_the_last_factor() {
        let mut spec = TwoFactorSpec {
            user: Uuid::nil(),
            totp: None,
            recovery: vec!["h".into()],
            webauthn: vec![],
        };
        assert_eq!(
            after_removal(&mut spec.clone(), true).unwrap_err().code,
            "two_factor_required"
        );
        after_removal(&mut spec, false).unwrap();
        assert!(spec.recovery.is_empty());
    }
}
