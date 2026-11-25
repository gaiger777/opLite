//! ACME 인증서 리소스(계획 73 §6·§11): 계정(`acme-account`), DNS-01 공급자(`acme-dns`), 받는 인증서(`certificate`).
//! 비밀(계정 키·EAB HMAC·DNS 공급자 비밀·공유 인증서 키)은 Raft에 평문으로 두고 보기·watch·감사·이력에서 뺀다
//! (`vmware::Secret`과 같은 자리). 노드마다 인증서의 키는 그 노드 디스크에만 있다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, ResourceSpec};
pub use crate::vmware::Secret;

/// 디렉터리 별명
pub const DIRECTORY_ALIASES: [(&str, &str); 4] = [
    (
        "letsencrypt",
        "https://acme-v02.api.letsencrypt.org/directory",
    ),
    (
        "letsencrypt-staging",
        "https://acme-staging-v02.api.letsencrypt.org/directory",
    ),
    ("zerossl", "https://acme.zerossl.com/v2/DV90"),
    ("google", "https://dv.acme-v02.api.pki.goog/directory"),
];

/// 훅 DNS 공급자의 실행 파일이 있어야 하는 디렉터리
pub const HOOK_DIR: &str = "/etc/opl/acme-hooks/";
/// 노드마다 펼치는 이름 자리
pub const NODE_PLACEHOLDER: &str = "{node}";
pub const NODE_ADDRESS_PLACEHOLDER: &str = "{node_address}";

// ---- 계정 ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AcmeAccountSpec {
    /// 디렉터리 URL 또는 별명(`letsencrypt`·`letsencrypt-staging`·`zerossl`·`google`). 만든 뒤 바꿀 수 없다.
    pub directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub email: Option<String>,
    /// CA 약관에 동의함(참이어야 등록한다)
    #[serde(default)]
    pub agree_tos: bool,
    /// 외부 계정 묶기(ZeroSSL·Google·사내 CA)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub eab: Option<ExternalAccountBinding>,
    /// 사내 ACME(step-ca·Pebble)의 루트(PEM). 있으면 이 루트만 믿는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub ca_bundle: Option<String>,
    /// 계정 키(PKCS#8 PEM, ES256). 서버가 만들고 요청에는 없다. 보기·watch·감사·이력에서 뺀다.
    #[serde(default, skip_serializing_if = "Secret::is_empty")]
    #[cfg_attr(feature = "openapi", schema(ignore))]
    pub key: Secret,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ExternalAccountBinding {
    /// EAB 키 id
    pub kid: String,
    /// EAB HMAC 키(base64url). 보기·watch·감사·이력에서 뺀다.
    #[cfg_attr(feature = "openapi", schema(value_type = String, write_only))]
    pub hmac_key: Secret,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct AcmeAccountStatus {
    /// 등록한 계정 URL(`kid`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub url: Option<String>,
    /// 등록한 때(유닉스 초)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub registered_at: Option<i64>,
    /// 동의한 약관 URL
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub terms: Option<String>,
    /// 디렉터리 `meta.profiles`의 이름
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub error: Option<String>,
}

impl ResourceSpec for AcmeAccountSpec {
    const KIND: Kind = Kind::AcmeAccount;
    type Status = AcmeAccountStatus;
}

/// 계정 스펙이 틀림
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcmeSpecError {
    #[error(
        "directory must be an https URL or one of letsencrypt, letsencrypt-staging, zerossl, google"
    )]
    Directory,
    #[error("agree_tos must be true: the CA requires accepting its terms of service")]
    Terms,
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("invalid certificate name {0:?}")]
    Name(String),
    #[error("{0:?}: wildcard names need the dns-01 challenge")]
    Wildcard(String),
    #[error("{0:?}: the {{node}} and {{node_address}} placeholders need a per-node certificate")]
    Placeholder(String),
    #[error("hook path must be an absolute file under {HOOK_DIR}")]
    HookPath,
    #[error("{0}")]
    Range(&'static str),
}

impl AcmeAccountSpec {
    /// 디렉터리 URL(별명이면 풀어서)
    pub fn directory_url(&self) -> Option<&str> {
        resolve_directory(&self.directory)
    }

    pub fn validate(&self) -> Result<(), AcmeSpecError> {
        self.directory_url().ok_or(AcmeSpecError::Directory)?;
        if !self.agree_tos {
            return Err(AcmeSpecError::Terms);
        }
        if let Some(eab) = &self.eab
            && (eab.kid.is_empty() || eab.hmac_key.is_empty())
        {
            return Err(AcmeSpecError::Empty("eab"));
        }
        Ok(())
    }
}

/// 별명이나 https URL을 디렉터리 URL로. 둘 다 아니면 `None`.
pub fn resolve_directory(directory: &str) -> Option<&str> {
    if let Some((_, url)) = DIRECTORY_ALIASES.iter().find(|(a, _)| *a == directory) {
        return Some(url);
    }
    let rest = directory.strip_prefix("https://")?;
    (!rest.is_empty() && !rest.starts_with('/') && !directory.chars().any(char::is_whitespace))
        .then_some(directory)
}

/// 계정 스펙 JSON에서 비밀(계정 키·EAB HMAC 키)을 뺀다. API 보기·watch 이벤트가 쓴다.
pub fn redact_account_json(spec: &mut serde_json::Value) {
    if let Some(s) = spec.as_object_mut() {
        s.remove("key");
        if let Some(eab) = s.get_mut("eab").and_then(serde_json::Value::as_object_mut) {
            eab.remove("hmac_key");
        }
    }
}

// ---- DNS 공급자 ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AcmeDnsSpec {
    pub provider: DnsProvider,
    #[serde(default, skip_serializing_if = "Propagation::is_default")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub propagation: Propagation,
    /// 이 공급자가 맡는 영역(`example.com`). 인증서가 이름으로 공급자를 고를 때 쓴다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<String>,
}

/// DNS-01 TXT를 쓰는 길
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DnsProvider {
    Cloudflare {
        #[cfg_attr(feature = "openapi", schema(value_type = String, write_only))]
        api_token: Secret,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false))]
        zone_id: Option<String>,
    },
    Route53 {
        access_key_id: String,
        #[cfg_attr(feature = "openapi", schema(value_type = String, write_only))]
        secret_access_key: Secret,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false))]
        hosted_zone_id: Option<String>,
        #[serde(default = "default_region")]
        #[cfg_attr(feature = "openapi", schema(default = "us-east-1"))]
        region: String,
    },
    /// DNS UPDATE(RFC 2136) + TSIG
    Rfc2136 {
        /// `ip:53`
        server: String,
        key_name: String,
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(default))]
        algorithm: TsigAlgorithm,
        /// TSIG 비밀(base64)
        #[cfg_attr(feature = "openapi", schema(value_type = String, write_only))]
        secret: Secret,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false))]
        zone: Option<String>,
    },
    PowerDns {
        url: String,
        #[cfg_attr(feature = "openapi", schema(value_type = String, write_only))]
        api_key: Secret,
        #[serde(default = "default_server_id")]
        #[cfg_attr(feature = "openapi", schema(default = "localhost"))]
        server_id: String,
    },
    /// `{path} add|remove {fqdn} {value}`를 그 노드에서 부른다(acme.sh·lego 플러그인을 잇는 길).
    Hook {
        /// [`HOOK_DIR`] 아래 절대 경로
        path: String,
    },
}

fn default_region() -> String {
    "us-east-1".into()
}

fn default_server_id() -> String {
    "localhost".into()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum TsigAlgorithm {
    #[default]
    HmacSha256,
    HmacSha512,
}

/// TXT가 퍼졌는지 확인하는 방법
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Propagation {
    #[serde(default = "default_propagation_timeout")]
    #[cfg_attr(feature = "openapi", schema(default = 180))]
    pub timeout_s: u32,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub check: PropagationCheck,
}

fn default_propagation_timeout() -> u32 {
    180
}

impl Default for Propagation {
    fn default() -> Self {
        Self {
            timeout_s: default_propagation_timeout(),
            check: PropagationCheck::Authoritative,
        }
    }
}

impl Propagation {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PropagationCheck {
    /// 영역의 권한 서버(NS)에 직접 묻는다
    #[default]
    Authoritative,
    /// 묻지 않는다
    Skip,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct AcmeDnsStatus {}

impl ResourceSpec for AcmeDnsSpec {
    const KIND: Kind = Kind::AcmeDns;
    type Status = AcmeDnsStatus;
}

impl AcmeDnsSpec {
    pub fn validate(&self) -> Result<(), AcmeSpecError> {
        if !(10..=3600).contains(&self.propagation.timeout_s) {
            return Err(AcmeSpecError::Range(
                "propagation.timeout_s must be 10..=3600",
            ));
        }
        match &self.provider {
            DnsProvider::Cloudflare { api_token, .. } if api_token.is_empty() => {
                Err(AcmeSpecError::Empty("api_token"))
            }
            DnsProvider::Route53 {
                access_key_id,
                secret_access_key,
                ..
            } if access_key_id.is_empty() || secret_access_key.is_empty() => {
                Err(AcmeSpecError::Empty("access_key_id/secret_access_key"))
            }
            DnsProvider::Rfc2136 {
                server,
                key_name,
                secret,
                ..
            } if server.is_empty() || key_name.is_empty() || secret.is_empty() => {
                Err(AcmeSpecError::Empty("server/key_name/secret"))
            }
            DnsProvider::PowerDns { url, api_key, .. } if url.is_empty() || api_key.is_empty() => {
                Err(AcmeSpecError::Empty("url/api_key"))
            }
            DnsProvider::Hook { path } if !hook_path_ok(path) => Err(AcmeSpecError::HookPath),
            _ => Ok(()),
        }
    }
}

/// 훅 경로가 [`HOOK_DIR`] 아래 파일인지(`..`·빈 조각 없음). 파일 주인·권한은 실행하는 노드가 본다.
pub fn hook_path_ok(path: &str) -> bool {
    path.strip_prefix(HOOK_DIR).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != "..")
    })
}

/// DNS 공급자 스펙 JSON에서 비밀을 뺀다. API 보기·watch 이벤트가 쓴다.
pub fn redact_dns_json(spec: &mut serde_json::Value) {
    if let Some(p) = spec
        .get_mut("provider")
        .and_then(serde_json::Value::as_object_mut)
    {
        for key in ["api_token", "secret_access_key", "secret", "api_key"] {
            p.remove(key);
        }
    }
}

// ---- 인증서 ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CertificateSpec {
    /// ACME 계정 이름
    pub account: String,
    /// 인증서 이름들. `{node}`·`{node_address}`(노드마다만)와 `*.`(DNS-01만)를 받는다.
    pub names: Vec<String>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub scope: CertificateScope,
    pub challenge: ChallengeKind,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub key: CertificateKey,
    /// ACME 프로필(`classic`·`tlsserver`·`shortlived`). 디렉터리에 프로필이 없으면 보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "RenewPolicy::is_ari")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub renew: RenewPolicy,
    /// 고를 사슬의 루트 CN
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub preferred_chain: Option<String>,
    #[serde(default, skip_serializing_if = "CertificateInstall::is_api")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub install: CertificateInstall,
}

/// 인증서를 노드마다 받나, 한 번 받아 나누나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertificateScope {
    /// 노드마다 따로(키는 그 노드 디스크에만). `nodes`가 비면 모든 노드
    PerNode {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        nodes: Vec<Uuid>,
    },
    /// 한 번 받아 모든 노드가 같은 인증서를 낸다(VIP 이름 — 키는 Raft 비밀)
    Shared,
}

impl Default for CertificateScope {
    fn default() -> Self {
        Self::PerNode { nodes: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChallengeKind {
    /// 포트 80(`[api] http_listen`) — 어느 노드든 Raft의 도전 값으로 답한다
    Http01,
    /// TXT `_acme-challenge` — `dns`는 DNS 공급자 이름
    Dns01 { dns: String },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CertificateKey {
    #[default]
    EcdsaP256,
    EcdsaP384,
    Rsa2048,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RenewPolicy {
    /// CA의 갱신 창(ARI, RFC 9773). 창이 없으면 유효 기간 1/3이 남았을 때
    #[default]
    Ari,
    /// 만료 `days`일 전
    DaysBefore { days: u16 },
}

impl RenewPolicy {
    pub fn is_ari(&self) -> bool {
        *self == Self::Ari
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CertificateInstall {
    /// 공개 API·웹 리스너(8006)에 끼운다
    #[default]
    Api,
    /// 받기만 한다
    None,
}

impl CertificateInstall {
    pub fn is_api(&self) -> bool {
        *self == Self::Api
    }
}

impl CertificateSpec {
    pub fn validate(&self) -> Result<(), AcmeSpecError> {
        if self.account.is_empty() {
            return Err(AcmeSpecError::Empty("account"));
        }
        if self.names.is_empty() {
            return Err(AcmeSpecError::Empty("names"));
        }
        if self.names.len() > 100 {
            return Err(AcmeSpecError::Range(
                "a certificate holds at most 100 names",
            ));
        }
        let per_node = matches!(self.scope, CertificateScope::PerNode { .. });
        let dns01 = matches!(self.challenge, ChallengeKind::Dns01 { .. });
        for name in &self.names {
            let placeholder =
                name.contains(NODE_PLACEHOLDER) || name.contains(NODE_ADDRESS_PLACEHOLDER);
            if placeholder && !per_node {
                return Err(AcmeSpecError::Placeholder(name.clone()));
            }
            // 자리를 그럴듯한 레이블로 바꿔 꼴을 본다.
            let probe = name
                .replace(NODE_PLACEHOLDER, "node")
                .replace(NODE_ADDRESS_PLACEHOLDER, "10.0.0.1");
            let (wild, host) = match probe.strip_prefix("*.") {
                Some(rest) => (true, rest.to_string()),
                None => (false, probe),
            };
            if wild && !dns01 {
                return Err(AcmeSpecError::Wildcard(name.clone()));
            }
            if !dns_name_ok(&host) {
                return Err(AcmeSpecError::Name(name.clone()));
            }
        }
        if let RenewPolicy::DaysBefore { days } = self.renew
            && !(1..=365).contains(&days)
        {
            return Err(AcmeSpecError::Range("renew.days must be 1..=365"));
        }
        Ok(())
    }

    /// 노드 하나에 펼친 이름(`{node}`·`{node_address}` 자리를 채움, 겹친 이름은 하나로)
    pub fn names_for(&self, node: &str, address: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in &self.names {
            let n = n
                .replace(NODE_PLACEHOLDER, node)
                .replace(NODE_ADDRESS_PLACEHOLDER, address);
            if !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }
}

/// DNS 이름(또는 IP — `{node_address}`로 펼친 것) 꼴
fn dns_name_ok(name: &str) -> bool {
    if name.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    name.len() <= 253
        && name.contains('.')
        && name.split('.').all(|l| {
            (1..=63).contains(&l.len())
                && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        })
}

/// 받은 인증서와 갱신 상태
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct CertificateStatus {
    /// 노드마다 받은 인증서(노드 id → 인증서)
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub nodes: BTreeMap<Uuid, IssuedCert>,
    /// 공유 인증서
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub shared: Option<IssuedCert>,
    /// 공유 인증서의 키(PKCS#8 PEM). 보기·watch·감사에서 뺀다(status라 이력에는 없다).
    #[serde(skip_serializing_if = "Secret::is_empty")]
    #[cfg_attr(feature = "openapi", schema(ignore))]
    pub shared_key: Secret,
    /// HTTP-01 답(짧게 산다 — 모든 노드의 포트 80 리스너가 읽는다)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub challenges: Vec<Http01Answer>,
    /// 다음 주문을 해도 되는 때(유닉스 초, 물러서기)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub next_attempt_at: Option<i64>,
    /// 이어진 실패 수
    #[serde(skip_serializing_if = "is_zero")]
    pub failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub error: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Http01Answer {
    pub token: String,
    pub key_authorization: String,
    /// 이때(유닉스 초)가 지나면 답하지 않는다
    pub until: i64,
}

/// 받은 인증서 하나. 시각은 유닉스 초다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct IssuedCert {
    /// 일련번호(16진수)
    pub serial: String,
    pub not_before: i64,
    pub not_after: i64,
    pub names: Vec<String>,
    /// 사슬(PEM, 공개)
    pub chain_pem: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub ari: Option<AriWindow>,
    /// 갱신할 때
    pub renew_at: i64,
    /// 진행 중 주문 URL(Task가 다시 떠도 이어 본다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub order: Option<String>,
    /// 끼우지 못한 까닭(이름 빠짐·키 맞지 않음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub error: Option<String>,
}

/// CA가 알려 준 갱신 창(ARI)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AriWindow {
    pub start: i64,
    pub end: i64,
    pub checked_at: i64,
}

impl ResourceSpec for CertificateSpec {
    const KIND: Kind = Kind::Certificate;
    type Status = CertificateStatus;
}

/// 인증서 status JSON에서 공유 키를 뺀다. API 보기·watch 이벤트가 쓴다.
pub fn redact_certificate_status_json(status: &mut serde_json::Value) {
    if let Some(s) = status.as_object_mut() {
        s.remove("shared_key");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account() -> AcmeAccountSpec {
        AcmeAccountSpec {
            directory: "letsencrypt".into(),
            email: Some("ops@example.com".into()),
            agree_tos: true,
            eab: None,
            ca_bundle: None,
            key: Secret::default(),
        }
    }

    #[test]
    fn account_validation_and_redaction() {
        let mut a = account();
        assert_eq!(a.validate(), Ok(()));
        assert_eq!(
            a.directory_url(),
            Some("https://acme-v02.api.letsencrypt.org/directory")
        );
        a.directory = "https://ca.internal:9000/acme/acme/directory".into();
        assert_eq!(a.validate(), Ok(()));
        for bad in ["http://ca/directory", "ftp://x", "https://", "letsencrypt2"] {
            a.directory = bad.into();
            assert_eq!(a.validate(), Err(AcmeSpecError::Directory), "{bad}");
        }
        a.directory = "zerossl".into();
        a.agree_tos = false;
        assert_eq!(a.validate(), Err(AcmeSpecError::Terms));
        a.agree_tos = true;
        a.eab = Some(ExternalAccountBinding {
            kid: "kid".into(),
            hmac_key: Secret::new("hmac-secret"),
        });
        a.key = Secret::new("-----BEGIN PRIVATE KEY-----");
        assert!(!format!("{a:?}").contains("hmac-secret"));
        let mut v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["eab"]["hmac_key"], "hmac-secret");
        redact_account_json(&mut v);
        assert!(!v.to_string().contains("hmac-secret"));
        assert!(!v.to_string().contains("PRIVATE"));
        assert_eq!(v["eab"]["kid"], "kid");
        // 키가 없으면 쓰지 않는다(요청에도 없다).
        let v = serde_json::to_value(account()).unwrap();
        assert!(v.get("key").is_none());
        assert_eq!(
            serde_json::from_value::<AcmeAccountSpec>(v).unwrap(),
            account()
        );
        assert_eq!(
            serde_json::to_value(AcmeAccountStatus::default()).unwrap(),
            json!({})
        );
    }

    #[test]
    fn dns_providers_round_trip_and_redact() {
        let cases = [
            json!({ "provider": { "kind": "cloudflare", "api_token": "T" } }),
            json!({ "provider": { "kind": "route53", "access_key_id": "A",
                                  "secret_access_key": "S", "region": "us-east-1" } }),
            json!({ "provider": { "kind": "rfc2136", "server": "10.0.0.53:53", "key_name": "k",
                                  "algorithm": "hmac-sha512", "secret": "c2VjcmV0" },
                    "propagation": { "timeout_s": 60, "check": "skip" }, "zones": ["example.com"] }),
            json!({ "provider": { "kind": "power_dns", "url": "http://pdns:8081", "api_key": "K",
                                  "server_id": "localhost" } }),
            json!({ "provider": { "kind": "hook", "path": "/etc/opl/acme-hooks/lego.sh" } }),
        ];
        for doc in cases {
            let spec: AcmeDnsSpec = serde_json::from_value(doc.clone()).unwrap();
            assert_eq!(spec.validate(), Ok(()), "{doc}");
            assert_eq!(serde_json::to_value(&spec).unwrap(), doc);
            let mut v = doc.clone();
            redact_dns_json(&mut v);
            for secret in ["\"T\"", "\"S\"", "c2VjcmV0", "\"K\""] {
                assert!(!v.to_string().contains(secret), "{v}");
            }
            assert!(!format!("{spec:?}").contains("c2VjcmV0"));
        }
        let bad: AcmeDnsSpec = serde_json::from_value(
            json!({ "provider": { "kind": "hook", "path": "/etc/opl/acme-hooks/../../bin/sh" } }),
        )
        .unwrap();
        assert_eq!(bad.validate(), Err(AcmeSpecError::HookPath));
        assert!(!hook_path_ok("/usr/bin/x"));
        assert!(!hook_path_ok("/etc/opl/acme-hooks/"));
        assert!(hook_path_ok("/etc/opl/acme-hooks/a/b.sh"));
    }

    #[test]
    fn certificate_validation_and_names() {
        let mut c: CertificateSpec = serde_json::from_value(json!({
            "account": "le",
            "names": ["{node}.pve.example.com", "{node_address}"],
            "challenge": { "kind": "http01" }
        }))
        .unwrap();
        assert_eq!(c.scope, CertificateScope::PerNode { nodes: vec![] });
        assert_eq!(c.key, CertificateKey::EcdsaP256);
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(
            c.names_for("n1", "10.0.0.1"),
            vec!["n1.pve.example.com".to_string(), "10.0.0.1".to_string()]
        );
        // 기본값은 쓰지 않는다.
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({ "account": "le", "names": ["{node}.pve.example.com", "{node_address}"],
                    "scope": { "kind": "per_node" }, "challenge": { "kind": "http01" },
                    "key": "ecdsa-p256" })
        );
        c.scope = CertificateScope::Shared;
        assert!(matches!(c.validate(), Err(AcmeSpecError::Placeholder(_))));
        c.names = vec!["*.example.com".into()];
        assert!(matches!(c.validate(), Err(AcmeSpecError::Wildcard(_))));
        c.challenge = ChallengeKind::Dns01 { dns: "cf".into() };
        assert_eq!(c.validate(), Ok(()));
        c.names = vec!["bad_name.example.com".into()];
        assert!(matches!(c.validate(), Err(AcmeSpecError::Name(_))));
        c.names = vec!["api.example.com".into()];
        c.renew = RenewPolicy::DaysBefore { days: 0 };
        assert!(matches!(c.validate(), Err(AcmeSpecError::Range(_))));
        assert_eq!(Kind::Certificate.as_str(), "certificate");
    }

    #[test]
    fn certificate_status_hides_the_shared_key() {
        let status = CertificateStatus {
            shared: Some(IssuedCert {
                serial: "0a".into(),
                not_before: 1,
                not_after: 2,
                names: vec!["api.example.com".into()],
                chain_pem: "-----BEGIN CERTIFICATE-----".into(),
                ari: None,
                renew_at: 2,
                order: None,
                error: None,
            }),
            shared_key: Secret::new("-----BEGIN PRIVATE KEY-----"),
            failures: 1,
            ..CertificateStatus::default()
        };
        assert!(!format!("{status:?}").contains("PRIVATE"));
        let mut v = serde_json::to_value(&status).unwrap();
        assert_eq!(
            serde_json::from_value::<CertificateStatus>(v.clone()).unwrap(),
            status
        );
        redact_certificate_status_json(&mut v);
        assert!(!v.to_string().contains("PRIVATE"));
        assert_eq!(v["shared"]["serial"], "0a");
        assert_eq!(
            serde_json::to_value(CertificateStatus::default()).unwrap(),
            json!({})
        );
    }
}
