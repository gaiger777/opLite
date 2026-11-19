//! 2단계 인증(계획 62): 사용자의 요소(TOTP·복구 코드·패스키), 대기 로그인, 정책, TOTP(RFC 6238) 계산.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use data_encoding::BASE32_NOPAD;
use ring::hmac;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::auth::Role;
use crate::resource::{Kind, Resource, ResourceSpec, validate_name};

/// 사용자 한 명의 2단계 인증 요소. 리소스 이름은 사용자 id 문자열이다. 계정 종류라 API 응답·watch에 내보내지
/// 않는다(TOTP 비밀은 서버가 HMAC을 다시 셈하므로 해시할 수 없다).
pub type TwoFactor = Resource<TwoFactorSpec, TwoFactorStatus>;

/// TOTP 한 칸의 길이(초)
pub const TOTP_PERIOD: u64 = 30;
/// TOTP 자릿수
pub const TOTP_DIGITS: u32 = 6;
/// 앞뒤로 받는 칸 수(시계 어긋남)
pub const TOTP_SKEW: u64 = 1;
/// 새 TOTP 비밀의 길이(바이트, 160비트 — RFC 4226 권장)
pub const TOTP_SECRET_BYTES: usize = 20;
/// 대기 로그인의 수명(초)
pub const PENDING_LOGIN_SECS: u64 = 300;
/// 한 번에 만드는 복구 코드 수
pub const RECOVERY_CODES: usize = 10;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TwoFactorSpec {
    /// 주인(User id)
    pub user: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totp: Option<Totp>,
    /// 남은 복구 코드의 SHA-256(16진수). 쓰면 지운다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub webauthn: Vec<Passkey>,
}

impl ResourceSpec for TwoFactorSpec {
    const KIND: Kind = Kind::TwoFactor;
    type Status = TwoFactorStatus;
}

impl fmt::Debug for TwoFactorSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TwoFactorSpec")
            .field("user", &self.user)
            .field("totp", &self.totp)
            .field("recovery", &self.recovery.len())
            .field("webauthn", &self.webauthn)
            .finish()
    }
}

impl TwoFactorSpec {
    /// 로그인 때 둘째 단계를 물을 요소가 있다(확인된 TOTP 또는 패스키). 복구 코드만으로는 아니다.
    pub fn has_factor(&self) -> bool {
        self.totp.as_ref().is_some_and(|t| t.confirmed) || !self.webauthn.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TwoFactorStatus {
    /// 마지막으로 받은 TOTP 칸. 이 이하의 칸은 다시 받지 않는다(revision CAS로 올린다).
    pub last_totp_step: u64,
    /// 마지막으로 둘째 단계를 통과한 시각(유닉스 초)
    pub last_used: u64,
    /// 통과하지 못한 둘째 단계 시도 수(클러스터 전체, 계획 62 리뷰 1). 시도마다 확인 전에 올리고 통과하면 0이다.
    #[serde(skip_serializing_if = "is_zero")]
    pub failures: u32,
    /// 이 시각(유닉스 초)까지 둘째 단계를 받지 않는다(`failures`가 [`SECOND_STEP_ATTEMPTS`] 이상일 때 두 배씩).
    #[serde(skip_serializing_if = "is_zero")]
    pub blocked_until: u64,
}

fn is_zero<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// 둘째 단계를 막기 전에 틀릴 수 있는 횟수(사용자마다, 모든 노드를 합쳐). 이만큼 틀리면 대기 세션도 끝난다.
pub const SECOND_STEP_ATTEMPTS: u32 = 5;
/// 처음 막는 시간(초). 더 틀릴 때마다 두 배, [`SECOND_STEP_MAX_BLOCK`]까지.
pub const SECOND_STEP_BLOCK: u64 = 30;
pub const SECOND_STEP_MAX_BLOCK: u64 = 15 * 60;

impl TwoFactorStatus {
    /// 시도 하나를 센 상태. `failures`가 상한 이상이면 `now`부터 막는다(30초부터 두 배, 15분까지).
    pub fn count_attempt(&self, now: u64) -> TwoFactorStatus {
        let failures = self.failures.saturating_add(1);
        let blocked_until = if failures >= SECOND_STEP_ATTEMPTS {
            let doublings = (failures - SECOND_STEP_ATTEMPTS).min(16);
            now + (SECOND_STEP_BLOCK << doublings).min(SECOND_STEP_MAX_BLOCK)
        } else {
            self.blocked_until
        };
        TwoFactorStatus {
            failures,
            blocked_until,
            ..self.clone()
        }
    }

    /// 지금 막혀 있으면 남은 초
    pub fn blocked_for(&self, now: u64) -> Option<u64> {
        (self.failures >= SECOND_STEP_ATTEMPTS && self.blocked_until > now)
            .then(|| self.blocked_until - now)
    }
}

/// TOTP 요소
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Totp {
    /// 비밀(base32, 패딩 없음). `Debug`·API에서 가린다.
    pub secret: String,
    /// 첫 코드로 확인했는지. 거짓이면 로그인에 쓰지 않는다(등록 중).
    #[serde(default)]
    pub confirmed: bool,
    /// 만든 시각(유닉스 초)
    pub created_at: u64,
}

impl fmt::Debug for Totp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Totp")
            .field("secret", &"***")
            .field("confirmed", &self.confirmed)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// 등록한 패스키(WebAuthn 자격 증명). 공개 키만 둔다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Passkey {
    /// 자격 증명 id(base64url, 패딩 없음)
    pub id: String,
    /// 사용자가 붙인 이름
    pub name: String,
    /// 등록할 때의 RP ID
    pub rp_id: String,
    /// COSE 공개 키. JSON은 base64url(패딩 없음)이다.
    #[serde(with = "b64")]
    pub cose_key: Vec<u8>,
    /// 인증기의 서명 횟수. 0이 아니고 줄면 복제된 인증기로 보고 거절한다.
    #[serde(default)]
    pub sign_count: u32,
    /// 만든 시각(유닉스 초)
    pub created_at: u64,
}

mod b64 {
    use super::*;

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        URL_SAFE_NO_PAD
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// 둘째 단계를 기다리는 로그인(`SessionSpec.pending`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingLogin {
    /// 대기가 끝나는 시각(유닉스 초, 보통 5분). 세션의 `expires_at`보다 앞선다.
    pub expires_at: u64,
    /// 정책이 요구하는데 요소가 없어 등록부터 해야 한다(등록 경로만 쓴다).
    #[serde(default, skip_serializing_if = "is_false")]
    pub enroll: bool,
    /// WebAuthn 로그인 challenge(base64url). 패스키가 없으면 None이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// 2단계 인증 정책(`SettingsSpec.two_factor`)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TwoFactorPolicy {
    /// 이 전역 역할의 사용자에게 요구한다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub roles: Vec<Role>,
    /// 이 영역의 사용자에게 요구한다. 로컬 사용자는 `local`이다. OIDC 영역은 받지 않는다(IdP가 MFA를 맡는다).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub realms: Vec<String>,
    /// 패스키의 RP ID(도메인). 없으면 요청 호스트다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub rp_id: Option<String>,
}

/// 로컬 사용자의 영역 이름(정책의 `realms`에 쓴다)
pub const LOCAL_REALM: &str = "local";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TwoFactorError {
    #[error("invalid realm {0:?} in the two-factor policy")]
    BadRealm(String),
    #[error("invalid rp_id {0:?}: use a domain name without scheme, port or path")]
    BadRpId(String),
}

impl TwoFactorPolicy {
    /// 스펙만으로 보는 검사. 영역이 있는지·OIDC인지는 API가 본다.
    pub fn validate(&self) -> Result<(), TwoFactorError> {
        for r in &self.realms {
            if r != LOCAL_REALM && validate_name(r).is_err() {
                return Err(TwoFactorError::BadRealm(r.clone()));
            }
        }
        if let Some(rp) = &self.rp_id
            && !valid_rp_id(rp)
        {
            return Err(TwoFactorError::BadRpId(rp.clone()));
        }
        Ok(())
    }

    /// 전역 역할 `role`, 영역 `realm`(로컬 사용자는 None)인 사용자에게 2단계 인증을 요구하는가
    pub fn requires(&self, role: Role, realm: Option<&str>) -> bool {
        let realm = realm.unwrap_or(LOCAL_REALM);
        self.roles.contains(&role) || self.realms.iter().any(|r| r == realm)
    }
}

/// RP ID: 소문자 도메인 이름(점으로 나눈 DNS 레이블, 253자까지). IP 주소는 WebAuthn이 받지 않으므로 거절한다.
fn valid_rp_id(rp: &str) -> bool {
    let label = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    rp.len() <= 253
        && rp.split('.').all(label)
        && rp.parse::<std::net::IpAddr>().is_err()
        && !rp
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.bytes().all(|c| c.is_ascii_digit()))
}

/// RFC 6238 TOTP(HMAC-SHA-1): `step` 칸의 코드를 `digits` 자리(앞 0 포함 정수)로 낸다.
pub fn totp(secret: &[u8], step: u64, digits: u32) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let mac = hmac::sign(&key, &step.to_be_bytes());
    let h = mac.as_ref();
    let off = (h[h.len() - 1] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[off], h[off + 1], h[off + 2], h[off + 3]]) & 0x7fff_ffff;
    bin % 10u32.pow(digits)
}

/// 유닉스 초의 TOTP 칸
pub fn totp_step(unix_secs: u64) -> u64 {
    unix_secs / TOTP_PERIOD
}

/// 사용자가 낸 6자리 코드를 확인한다. 지금 칸 ±[`TOTP_SKEW`] 가운데 맞고 `last_step`보다 큰 칸을 돌려준다
/// (부르는 쪽이 `last_totp_step`을 그 값으로 CAS 갱신한다). 공백은 무시한다.
pub fn verify_totp(secret: &[u8], code: &str, unix_secs: u64, last_step: u64) -> Option<u64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != TOTP_DIGITS as usize || !code.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let want: u32 = code.parse().ok()?;
    let now = totp_step(unix_secs);
    (now.saturating_sub(TOTP_SKEW)..=now + TOTP_SKEW)
        .filter(|s| *s > last_step)
        .find(|s| totp(secret, *s, TOTP_DIGITS) == want)
}

/// TOTP 비밀을 base32(대문자, 패딩 없음)로 쓴다(인증 앱의 `otpauth://` URI 형식).
pub fn encode_secret(secret: &[u8]) -> String {
    BASE32_NOPAD.encode(secret)
}

/// base32 TOTP 비밀을 푼다. 공백·`=` 패딩·소문자를 받는다.
pub fn decode_secret(text: &str) -> Option<Vec<u8>> {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let bytes = BASE32_NOPAD.decode(clean.as_bytes()).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_SECRET: &[u8] = b"12345678901234567890";

    /// RFC 6238 부록 B의 SHA-1 벡터(8자리)와 6자리 자르기
    #[test]
    fn totp_matches_rfc6238_vectors() {
        for (t, code8) in [
            (59u64, 94287082u32),
            (1111111109, 7081804),
            (1111111111, 14050471),
            (1234567890, 89005924),
            (2000000000, 69279037),
            (20000000000, 65353130),
        ] {
            let step = totp_step(t);
            assert_eq!(totp(RFC_SECRET, step, 8), code8, "{t}");
            assert_eq!(totp(RFC_SECRET, step, 6), code8 % 1_000_000, "{t}");
        }
    }

    #[test]
    fn verify_totp_accepts_skew_and_rejects_reuse() {
        let now = 1_111_111_111;
        let step = totp_step(now);
        let code = |s: u64| format!("{:06}", totp(RFC_SECRET, s, 6));
        assert_eq!(verify_totp(RFC_SECRET, &code(step), now, 0), Some(step));
        assert_eq!(
            verify_totp(RFC_SECRET, &code(step - 1), now, 0),
            Some(step - 1)
        );
        assert_eq!(
            verify_totp(RFC_SECRET, &code(step + 1), now, 0),
            Some(step + 1)
        );
        assert_eq!(verify_totp(RFC_SECRET, &code(step + 2), now, 0), None);
        assert_eq!(verify_totp(RFC_SECRET, &code(step - 2), now, 0), None);
        // 이미 받은 칸은 다시 받지 않는다.
        assert_eq!(verify_totp(RFC_SECRET, &code(step), now, step), None);
        assert_eq!(
            verify_totp(RFC_SECRET, &code(step + 1), now, step),
            Some(step + 1)
        );
        // 모양
        let c = code(step);
        let spaced = format!("{} {}", &c[..3], &c[3..]);
        assert_eq!(verify_totp(RFC_SECRET, &spaced, now, 0), Some(step));
        for bad in ["", "12345", "1234567", "abcdef", "12345a"] {
            assert_eq!(verify_totp(RFC_SECRET, bad, now, 0), None, "{bad}");
        }
    }

    /// 리뷰 1 I4: 시도는 다섯 번까지, 그 뒤로는 30초부터 두 배씩(15분까지) 막는다. 옛 JSON은 그대로다.
    #[test]
    fn second_step_attempts_block_with_backoff() {
        let mut status = TwoFactorStatus::default();
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"last_totp_step":0,"last_used":0}"#
        );
        for _ in 0..4 {
            status = status.count_attempt(1000);
            assert_eq!(status.blocked_for(1000), None);
        }
        status = status.count_attempt(1000);
        assert_eq!(status.failures, 5);
        assert_eq!(status.blocked_for(1000), Some(30));
        assert_eq!(status.blocked_for(1030), None);
        status = status.count_attempt(1030);
        assert_eq!(status.blocked_for(1030), Some(60));
        for _ in 0..20 {
            status = status.count_attempt(5000);
        }
        assert_eq!(status.blocked_for(5000), Some(SECOND_STEP_MAX_BLOCK));
        let back: TwoFactorStatus =
            serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
        assert_eq!(back, status);
    }

    #[test]
    fn base32_secret_round_trips() {
        let text = encode_secret(RFC_SECRET);
        assert_eq!(text, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(decode_secret(&text).unwrap(), RFC_SECRET);
        assert_eq!(
            decode_secret("gezd gnbv gy3t qojq gezd gnbv gy3t qojq").unwrap(),
            RFC_SECRET
        );
        let short = encode_secret(b"abc");
        assert_eq!(decode_secret(&format!("{short}===")).unwrap(), b"abc");
        assert_eq!(decode_secret(""), None);
        assert_eq!(decode_secret("not base32!"), None);
    }

    fn spec() -> TwoFactorSpec {
        TwoFactorSpec {
            user: Uuid::from_u128(1),
            totp: Some(Totp {
                secret: "JBSWY3DPEHPK3PXP".into(),
                confirmed: true,
                created_at: 10,
            }),
            recovery: vec!["ab".repeat(32)],
            webauthn: vec![Passkey {
                id: "Y3JlZA".into(),
                name: "laptop".into(),
                rp_id: "opl.example.com".into(),
                cose_key: vec![0xa5, 0x01, 0x02],
                sign_count: 3,
                created_at: 11,
            }],
        }
    }

    #[test]
    fn two_factor_storage_format_and_debug() {
        let s = spec();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["totp"]["secret"], "JBSWY3DPEHPK3PXP");
        assert_eq!(json["webauthn"][0]["cose_key"], "pQEC");
        assert_eq!(serde_json::from_value::<TwoFactorSpec>(json).unwrap(), s);
        let shown = format!("{s:?}");
        assert!(!shown.contains("JBSWY3DPEHPK3PXP"), "{shown}");
        assert!(!shown.contains(&"ab".repeat(32)), "{shown}");
        assert!(shown.contains(r#"secret: "***""#), "{shown}");
        let bare: TwoFactorSpec =
            serde_json::from_value(serde_json::json!({ "user": Uuid::from_u128(2) })).unwrap();
        assert!(!bare.has_factor());
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({ "user": Uuid::from_u128(2).to_string() })
        );
        let status: TwoFactorStatus = serde_json::from_str("{}").unwrap();
        assert_eq!(status, TwoFactorStatus::default());
        assert_eq!(TwoFactorSpec::KIND.as_str(), "two-factor");
    }

    #[test]
    fn has_factor_needs_a_confirmed_totp_or_a_passkey() {
        let mut s = spec();
        assert!(s.has_factor());
        s.webauthn.clear();
        assert!(s.has_factor());
        s.totp.as_mut().unwrap().confirmed = false;
        assert!(!s.has_factor(), "등록 중인 TOTP와 복구 코드만으로는 아니다");
    }

    #[test]
    fn pending_login_format() {
        let p = PendingLogin {
            expires_at: 100,
            enroll: false,
            challenge: None,
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({ "expires_at": 100 })
        );
        let p = PendingLogin {
            enroll: true,
            challenge: Some("abc".into()),
            ..p
        };
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "expires_at": 100, "enroll": true, "challenge": "abc" })
        );
        assert_eq!(serde_json::from_value::<PendingLogin>(json).unwrap(), p);
    }

    #[test]
    fn policy_requires_by_role_or_realm() {
        let p = TwoFactorPolicy {
            roles: vec![Role::Admin],
            realms: vec!["local".into(), "corp".into()],
            rp_id: None,
        };
        assert_eq!(p.validate(), Ok(()));
        assert!(p.requires(Role::Admin, Some("okta")));
        assert!(p.requires(Role::Viewer, None));
        assert!(p.requires(Role::None, Some("corp")));
        assert!(!p.requires(Role::Operator, Some("okta")));
        assert!(!TwoFactorPolicy::default().requires(Role::Admin, None));
        assert_eq!(
            serde_json::to_value(TwoFactorPolicy::default()).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn policy_validation() {
        for ok in ["opl.example.com", "localhost", "a-1.b"] {
            let p = TwoFactorPolicy {
                rp_id: Some(ok.into()),
                ..TwoFactorPolicy::default()
            };
            assert_eq!(p.validate(), Ok(()), "{ok}");
        }
        for bad in [
            "https://opl.example.com",
            "opl.example.com:8006",
            "10.0.0.1",
            "::1",
            "Opl.example.com",
            "",
            "a..b",
            "1.2.3",
        ] {
            let p = TwoFactorPolicy {
                rp_id: Some(bad.into()),
                ..TwoFactorPolicy::default()
            };
            assert_eq!(
                p.validate(),
                Err(TwoFactorError::BadRpId(bad.into())),
                "{bad}"
            );
        }
        let p = TwoFactorPolicy {
            realms: vec!["Corp".into()],
            ..TwoFactorPolicy::default()
        };
        assert_eq!(p.validate(), Err(TwoFactorError::BadRealm("Corp".into())));
    }
}
