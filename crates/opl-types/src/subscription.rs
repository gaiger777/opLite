//! 구독 키. 오프라인으로 검증하는 서명된 문자열 `opl1.<payload>.<signature>`이다: payload는 JSON의
//! base64url(패딩 없음), signature는 `opl1.<payload>` 바이트에 대한 Ed25519 서명의 base64url.
//! 구독은 기능을 막지 않는다. 상태([`status`])는 읽을 때 지금 시각과 노드 수로 계산한다.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 키 문자열의 접두사(형식 판)
const PREFIX: &str = "opl1";

/// 만료 시각의 상한(9999-12-31T23:59:59Z)
pub const MAX_EXPIRES_AT: u64 = 253_402_300_799;

/// 저장소의 개발 공개 키. 비밀 키는 시험 견본(`tests/fixtures/dev-license.key`)으로 공개되어 있다.
const DEV_KEY: &str = include_str!("../keys/dev-license.pub");

/// 구독 수준
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Standard,
    Premium,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Standard => "standard",
            Level::Premium => "premium",
        }
    }
}

/// 키에 서명해 담는 내용. 모르는 필드는 무시한다(나중 판이 더할 수 있게).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicensePayload {
    /// 키 id(1~64자, 영문자·숫자·`.`·`_`·`-`, 영문자나 숫자로 시작)
    pub id: String,
    /// 고객 이름(1~200자)
    pub customer: String,
    pub level: Level,
    /// 위트니스가 아닌 노드 수 한도(1 이상)
    pub nodes: u32,
    /// 발급 시각(유닉스 초)
    pub issued_at: u64,
    /// 만료 시각(유닉스 초). 발급 시각보다 뒤다.
    pub expires_at: u64,
}

impl LicensePayload {
    fn validate(&self) -> Result<(), KeyError> {
        // id는 인증 파일·명령 출력에 그대로 들어가므로 안전한 문자만 받는다.
        let id = self.id.as_bytes();
        let safe = id.first().is_some_and(u8::is_ascii_alphanumeric)
            && id.len() <= 64
            && id
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !safe {
            return Err(KeyError::Format(
                "id must be 1-64 letters, digits, '.', '_' or '-' and start with a letter or digit"
                    .into(),
            ));
        }
        let len = self.customer.chars().count();
        if !(1..=200).contains(&len) {
            return Err(KeyError::Format("customer must be 1-200 characters".into()));
        }
        if self.nodes == 0 {
            return Err(KeyError::Format("nodes must be at least 1".into()));
        }
        if self.expires_at > MAX_EXPIRES_AT {
            return Err(KeyError::Format(
                "expires_at must not be after 9999-12-31T23:59:59Z".into(),
            ));
        }
        if self.expires_at <= self.issued_at {
            return Err(KeyError::Format(
                "expires_at must be after issued_at".into(),
            ));
        }
        Ok(())
    }
}

/// 서명이 맞는 키의 내용
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct License {
    pub id: String,
    pub customer: String,
    pub level: Level,
    pub nodes: u32,
    pub issued_at: u64,
    pub expires_at: u64,
    /// 저장소의 개발 키로 검증됨
    pub development: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeyError {
    #[error("malformed subscription key: {0}")]
    Format(String),
    #[error("the subscription key signature does not match this build")]
    Signature,
    #[error("the subscription key expired on {}", date(*.0))]
    Expired(u64),
}

/// 검증에 쓰는 Ed25519 공개 키
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    pub bytes: [u8; 32],
    /// 저장소의 개발 키
    pub development: bool,
}

impl PublicKey {
    /// base64(표준) 32바이트 공개 키
    pub fn from_base64(text: &str, development: bool) -> Option<PublicKey> {
        let bytes = STANDARD.decode(text.trim()).ok()?.try_into().ok()?;
        Some(PublicKey { bytes, development })
    }
}

/// 이 빌드가 믿는 공개 키. 빌드 값 `OPL_LICENSE_KEYS`(base64 표준 공개 키를 쉼표로)가 있으면 그것만,
/// 없거나 공백뿐이면 저장소의 개발 키 하나다. 그 밖에 풀리는 키가 없으면(`","` 같은 값) 빈 목록이다.
pub fn license_keys() -> Vec<PublicKey> {
    keys_from(option_env!("OPL_LICENSE_KEYS"))
}

/// [`license_keys`]의 본체. 풀리지 않는 항목은 건너뛴다(그 키로 서명한 키는 invalid가 된다).
/// 목록에 든 개발 공개 키도 개발 키로 표시한다.
fn keys_from(list: Option<&str>) -> Vec<PublicKey> {
    let dev = PublicKey::from_base64(DEV_KEY, true);
    match list {
        Some(list) if !list.trim().is_empty() => list
            .split(',')
            .filter(|k| !k.trim().is_empty())
            .filter_map(|k| PublicKey::from_base64(k, false))
            .map(|mut k| {
                k.development = dev.as_ref().is_some_and(|d| d.bytes == k.bytes);
                k
            })
            .collect(),
        _ => dev.into_iter().collect(),
    }
}

/// 서명만 확인하고 내용을 푼다(만료는 보지 않는다). 공개 키 중 하나라도 맞으면 된다.
pub fn verify(key: &str, keys: &[PublicKey]) -> Result<License, KeyError> {
    let key = key.trim();
    let parts: Vec<&str> = key.split('.').collect();
    let [prefix, payload, sig] = parts[..] else {
        return Err(KeyError::Format(
            "expected opl1.<payload>.<signature>".into(),
        ));
    };
    if prefix != PREFIX {
        return Err(KeyError::Format(format!("unknown prefix {prefix:?}")));
    }
    let json = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| KeyError::Format("payload is not base64url".into()))?;
    let sig = URL_SAFE_NO_PAD
        .decode(sig)
        .map_err(|_| KeyError::Format("signature is not base64url".into()))?;
    let signed = &key[..PREFIX.len() + 1 + payload.len()];
    let signer = keys
        .iter()
        .find(|k| {
            UnparsedPublicKey::new(&ED25519, &k.bytes)
                .verify(signed.as_bytes(), &sig)
                .is_ok()
        })
        .ok_or(KeyError::Signature)?;
    let p: LicensePayload = serde_json::from_slice(&json)
        .map_err(|e| KeyError::Format(format!("payload is not valid JSON: {e}")))?;
    p.validate()?;
    Ok(License {
        id: p.id,
        customer: p.customer,
        level: p.level,
        nodes: p.nodes,
        issued_at: p.issued_at,
        expires_at: p.expires_at,
        development: signer.development,
    })
}

/// 키를 풀고 만료(`now >= expires_at`)까지 확인한다. 새로 넣는 키에 쓴다.
pub fn parse(key: &str, keys: &[PublicKey], now: u64) -> Result<License, KeyError> {
    let license = verify(key, keys)?;
    if now >= license.expires_at {
        return Err(KeyError::Expired(license.expires_at));
    }
    Ok(license)
}

/// PKCS#8(v2) Ed25519 비밀 키로 키 문자열을 만든다. 내용 검사는 [`verify`]와 같다.
pub fn sign(pkcs8: &[u8], license: &LicensePayload) -> Result<String, KeyError> {
    license.validate()?;
    let json = serde_json::to_vec(license).map_err(|e| KeyError::Format(e.to_string()))?;
    sign_bytes(pkcs8, &json)
}

fn sign_bytes(pkcs8: &[u8], json: &[u8]) -> Result<String, KeyError> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8)
        .map_err(|_| KeyError::Format("not a PKCS#8 Ed25519 private key".into()))?;
    let signed = format!("{PREFIX}.{}", URL_SAFE_NO_PAD.encode(json));
    let sig = pair.sign(signed.as_bytes());
    Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref())))
}

/// 새로 만든 키 쌍(둘 다 base64 표준 한 줄)
pub struct GeneratedKey {
    /// PKCS#8(v2) 비밀 키
    pub private_key: String,
    /// 32바이트 공개 키(`OPL_LICENSE_KEYS`의 항목)
    pub public_key: String,
}

/// 발급용 Ed25519 키 쌍을 만든다.
pub fn generate_key() -> Result<GeneratedKey, KeyError> {
    let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| KeyError::Format("generate a key pair".into()))?;
    let pair = Ed25519KeyPair::from_pkcs8(doc.as_ref())
        .map_err(|_| KeyError::Format("generate a key pair".into()))?;
    Ok(GeneratedKey {
        private_key: STANDARD.encode(doc.as_ref()),
        public_key: STANDARD.encode(pair.public_key().as_ref()),
    })
}

/// PKCS#8(v2) Ed25519 비밀 키의 공개 키(base64 표준, `OPL_LICENSE_KEYS`의 항목)
pub fn public_key(pkcs8: &[u8]) -> Result<String, KeyError> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8)
        .map_err(|_| KeyError::Format("not a PKCS#8 Ed25519 private key".into()))?;
    Ok(STANDARD.encode(pair.public_key().as_ref()))
}

/// 비밀 키 파일(base64 표준 한 줄)의 PKCS#8 바이트
pub fn decode_private_key(text: &str) -> Result<Vec<u8>, KeyError> {
    STANDARD
        .decode(text.trim())
        .map_err(|_| KeyError::Format("the private key file is not base64".into()))
}

/// 구독 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionState {
    /// 키가 없다.
    None,
    Active,
    Expired,
    /// 저장된 키가 이 빌드의 공개 키로 검증되지 않는다.
    Invalid,
    /// 위트니스가 아닌 노드가 한도보다 많다.
    OverLimit,
}

/// 읽을 때 계산한 구독 상태. 키 문자열은 담지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubscriptionInfo {
    pub state: SubscriptionState,
    // 아래 Option들은 없으면 빼고 쓰므로 API 문서에서도 null·기본값이 아니라 선택 필드다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub customer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub level: Option<Level>,
    /// 노드 수 한도
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub nodes: Option<u32>,
    /// 위트니스가 아닌 노드 수
    pub used_nodes: u32,
    /// 만료 시각(유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub expires_at: Option<u64>,
    /// 개발 키로 검증됨
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub development: Option<bool>,
}

/// 저장된 키의 상태. 키 없음 → none, 서명·형식 실패 → invalid, 만료 → expired,
/// 노드 수 초과 → over_limit, 그 밖 active.
pub fn status(
    key: Option<&str>,
    keys: &[PublicKey],
    now: u64,
    used_nodes: u32,
) -> SubscriptionInfo {
    let mut info = SubscriptionInfo {
        state: SubscriptionState::None,
        id: None,
        customer: None,
        level: None,
        nodes: None,
        used_nodes,
        expires_at: None,
        development: None,
    };
    let Some(key) = key else {
        return info;
    };
    let Ok(license) = verify(key, keys) else {
        info.state = SubscriptionState::Invalid;
        return info;
    };
    info.state = if now >= license.expires_at {
        SubscriptionState::Expired
    } else if used_nodes > license.nodes {
        SubscriptionState::OverLimit
    } else {
        SubscriptionState::Active
    };
    info.id = Some(license.id);
    info.customer = Some(license.customer);
    info.level = Some(license.level);
    info.nodes = Some(license.nodes);
    info.expires_at = Some(license.expires_at);
    info.development = Some(license.development);
    info
}

/// 유닉스 초의 UTC 날짜(RFC 3339 full-date, `YYYY-MM-DD`)
pub fn date(at: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(at.min(i64::MAX as u64) as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 저장소 개발 키의 비밀 키 견본
    const DEV_PRIVATE: &str = include_str!("../tests/fixtures/dev-license.key");
    const DAY: u64 = 86_400;
    const NOW: u64 = 1_800_000_000;

    fn dev_keys() -> Vec<PublicKey> {
        keys_from(None)
    }

    fn payload() -> LicensePayload {
        LicensePayload {
            id: "lic-0001".into(),
            customer: "Example Corp".into(),
            level: Level::Premium,
            nodes: 3,
            issued_at: NOW - DAY,
            expires_at: NOW + 30 * DAY,
        }
    }

    fn dev_sign(p: &LicensePayload) -> String {
        sign(&decode_private_key(DEV_PRIVATE).unwrap(), p).unwrap()
    }

    fn dev_sign_json(json: &str) -> String {
        sign_bytes(&decode_private_key(DEV_PRIVATE).unwrap(), json.as_bytes()).unwrap()
    }

    fn is_format(r: Result<License, KeyError>) -> bool {
        matches!(r, Err(KeyError::Format(_)))
    }

    #[test]
    fn a_key_signed_with_the_dev_fixture_parses() {
        let key = dev_sign(&payload());
        assert!(key.starts_with("opl1."));
        let license = parse(&format!("  {key}\n"), &dev_keys(), NOW).unwrap();
        assert_eq!(
            license,
            License {
                id: "lic-0001".into(),
                customer: "Example Corp".into(),
                level: Level::Premium,
                nodes: 3,
                issued_at: NOW - DAY,
                expires_at: NOW + 30 * DAY,
                development: true,
            }
        );
    }

    #[test]
    fn the_dev_fixture_matches_the_dev_public_key() {
        let pkcs8 = decode_private_key(DEV_PRIVATE).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).unwrap();
        assert_eq!(dev_keys().len(), 1);
        assert_eq!(pair.public_key().as_ref(), dev_keys()[0].bytes);
        assert!(dev_keys()[0].development);
        assert_eq!(public_key(&pkcs8).unwrap(), DEV_KEY.trim());
        assert!(public_key(b"not a key").is_err());
    }

    #[test]
    fn tampered_keys_are_rejected() {
        let key = dev_sign(&payload());
        let (signed, sig) = key.rsplit_once('.').unwrap();
        // 다른 내용(노드 수 99)에 원래 서명
        let other = dev_sign(&LicensePayload {
            nodes: 99,
            ..payload()
        });
        let other_payload = other.split('.').nth(1).unwrap();
        let swapped = format!("opl1.{other_payload}.{sig}");
        assert_eq!(verify(&swapped, &dev_keys()), Err(KeyError::Signature));
        // 서명 바이트 하나를 바꿈
        let mut bytes = URL_SAFE_NO_PAD.decode(sig).unwrap();
        bytes[10] ^= 1;
        let flipped = format!("{signed}.{}", URL_SAFE_NO_PAD.encode(&bytes));
        assert_eq!(verify(&flipped, &dev_keys()), Err(KeyError::Signature));
        // 다른 공개 키
        let stranger = generate_key().unwrap();
        let keys = vec![PublicKey::from_base64(&stranger.public_key, false).unwrap()];
        assert_eq!(verify(&key, &keys), Err(KeyError::Signature));
        assert_eq!(verify(&key, &[]), Err(KeyError::Signature));
        assert_eq!(
            KeyError::Signature.to_string(),
            "the subscription key signature does not match this build"
        );
    }

    #[test]
    fn any_listed_key_may_match() {
        let stranger = generate_key().unwrap();
        let mut keys = vec![PublicKey::from_base64(&stranger.public_key, false).unwrap()];
        keys.extend(dev_keys());
        let license = verify(&dev_sign(&payload()), &keys).unwrap();
        assert!(license.development);
        // 다른 키로 서명한 키는 development가 아니다.
        let pkcs8 = decode_private_key(&stranger.private_key).unwrap();
        let key = sign(&pkcs8, &payload()).unwrap();
        assert!(!verify(&key, &keys).unwrap().development);
    }

    #[test]
    fn malformed_keys_are_rejected() {
        let key = dev_sign(&payload());
        let (prefix_payload, sig) = key.rsplit_once('.').unwrap();
        let payload_part = prefix_payload.strip_prefix("opl1.").unwrap();
        for bad in [
            String::new(),
            "opl1".into(),
            prefix_payload.to_string(),
            format!("{key}.x"),
            format!("opl2.{payload_part}.{sig}"),
            format!("opl1.{payload_part}!.{sig}"),
            format!("opl1.{payload_part}.{sig}="),
        ] {
            assert!(is_format(verify(&bad, &dev_keys())), "{bad:?}");
        }
        // 서명은 맞지만 내용이 틀림
        for json in [
            "not json",
            r#"{"id":"a","customer":"c","level":"gold","nodes":1,"issued_at":1,"expires_at":2}"#,
            r#"{"id":"","customer":"c","level":"standard","nodes":1,"issued_at":1,"expires_at":2}"#,
            r#"{"id":"a","customer":"","level":"standard","nodes":1,"issued_at":1,"expires_at":2}"#,
            r#"{"id":"a","customer":"c","level":"standard","nodes":0,"issued_at":1,"expires_at":2}"#,
            r#"{"id":"a","customer":"c","level":"standard","nodes":1,"issued_at":2,"expires_at":2}"#,
            r#"{"id":"a","customer":"c","level":"standard","nodes":1,"issued_at":1}"#,
        ] {
            assert!(
                is_format(verify(&dev_sign_json(json), &dev_keys())),
                "{json}"
            );
        }
        let long_id = "x".repeat(65);
        let json = format!(
            r#"{{"id":"{long_id}","customer":"c","level":"standard","nodes":1,"issued_at":1,"expires_at":2}}"#
        );
        assert!(is_format(verify(&dev_sign_json(&json), &dev_keys())));
        let err = verify("x.y", &dev_keys()).unwrap_err().to_string();
        assert!(err.starts_with("malformed subscription key: "), "{err}");
    }

    #[test]
    fn ids_are_limited_to_safe_characters() {
        let pkcs8 = decode_private_key(DEV_PRIVATE).unwrap();
        for id in ["Acme 2026", "a\nb", "-a", ".a", "a/b", "가"] {
            let p = LicensePayload {
                id: id.into(),
                ..payload()
            };
            assert!(
                matches!(sign(&pkcs8, &p), Err(KeyError::Format(_))),
                "{id:?}"
            );
            let json = serde_json::to_string(&p).unwrap();
            assert!(
                is_format(verify(&dev_sign_json(&json), &dev_keys())),
                "{id:?}"
            );
        }
        for id in ["a", "Acme-2026_01.x", "0", &"x".repeat(64)] {
            let p = LicensePayload {
                id: id.to_string(),
                ..payload()
            };
            assert!(verify(&dev_sign(&p), &dev_keys()).is_ok(), "{id:?}");
        }
    }

    #[test]
    fn expiry_is_capped_at_the_year_9999() {
        let max = LicensePayload {
            expires_at: MAX_EXPIRES_AT,
            ..payload()
        };
        assert!(verify(&dev_sign(&max), &dev_keys()).is_ok());
        let json = serde_json::to_string(&LicensePayload {
            expires_at: MAX_EXPIRES_AT + 1,
            ..payload()
        })
        .unwrap();
        assert!(is_format(verify(&dev_sign_json(&json), &dev_keys())));
        assert_eq!(date(MAX_EXPIRES_AT), "9999-12-31");
    }

    #[test]
    fn sign_checks_the_payload_too() {
        let pkcs8 = decode_private_key(DEV_PRIVATE).unwrap();
        let bad = LicensePayload {
            nodes: 0,
            ..payload()
        };
        assert!(matches!(sign(&pkcs8, &bad), Err(KeyError::Format(_))));
        assert!(matches!(
            sign(b"not a key", &payload()),
            Err(KeyError::Format(_))
        ));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{"id":"a","customer":"c","level":"standard","nodes":2,"issued_at":1,"expires_at":2,"features":["x"]}"#;
        let license = verify(&dev_sign_json(json), &dev_keys()).unwrap();
        assert_eq!(license.level, Level::Standard);
        assert_eq!(license.nodes, 2);
    }

    #[test]
    fn expired_keys_fail_to_parse_but_verify() {
        let key = dev_sign(&payload());
        let expires = NOW + 30 * DAY;
        assert!(parse(&key, &dev_keys(), expires - 1).is_ok());
        assert_eq!(
            parse(&key, &dev_keys(), expires),
            Err(KeyError::Expired(expires))
        );
        assert!(verify(&key, &dev_keys()).is_ok());
        assert_eq!(
            KeyError::Expired(1_767_225_600).to_string(),
            "the subscription key expired on 2026-01-01"
        );
    }

    #[test]
    fn status_is_computed_from_time_and_nodes() {
        let key = dev_sign(&payload());
        let keys = dev_keys();

        let none = status(None, &keys, NOW, 2);
        assert_eq!(none.state, SubscriptionState::None);
        assert_eq!(none.used_nodes, 2);
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            serde_json::json!({ "state": "none", "used_nodes": 2 })
        );

        let active = status(Some(&key), &keys, NOW, 2);
        assert_eq!(
            serde_json::to_value(&active).unwrap(),
            serde_json::json!({
                "state": "active", "id": "lic-0001", "customer": "Example Corp",
                "level": "premium", "nodes": 3, "used_nodes": 2,
                "expires_at": NOW + 30 * DAY, "development": true,
            })
        );
        // 경계: 한도와 같으면 active
        assert_eq!(
            status(Some(&key), &keys, NOW, 3).state,
            SubscriptionState::Active
        );
        let over = status(Some(&key), &keys, NOW, 4);
        assert_eq!(over.state, SubscriptionState::OverLimit);
        assert_eq!(over.id.as_deref(), Some("lic-0001"));
        // 만료가 노드 수 초과보다 먼저다. 만료돼도 내용은 보인다.
        let expired = status(Some(&key), &keys, NOW + 30 * DAY, 4);
        assert_eq!(expired.state, SubscriptionState::Expired);
        assert_eq!(expired.level, Some(Level::Premium));

        let invalid = status(Some(&key), &[], NOW, 1);
        assert_eq!(
            serde_json::to_value(&invalid).unwrap(),
            serde_json::json!({ "state": "invalid", "used_nodes": 1 })
        );
        assert_eq!(
            status(Some("garbage"), &keys, NOW, 1).state,
            SubscriptionState::Invalid
        );
        assert_eq!(
            serde_json::to_value(SubscriptionState::OverLimit).unwrap(),
            "over_limit"
        );
    }

    #[test]
    fn build_keys_replace_the_dev_key() {
        let a = generate_key().unwrap();
        let b = generate_key().unwrap();
        let keys = keys_from(Some(&format!(" {} ,,{}", a.public_key, b.public_key)));
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().all(|k| !k.development));
        assert_eq!(
            keys[0],
            PublicKey::from_base64(&a.public_key, false).unwrap()
        );
        // 빈 값은 없는 것과 같다.
        assert_eq!(keys_from(Some("")), dev_keys());
        assert_eq!(keys_from(Some(" ")), dev_keys());
        // 빈 항목만 있으면 개발 키로 떨어지지 않고 어떤 키도 믿지 않는다.
        assert_eq!(keys_from(Some(",")), vec![]);
        assert_eq!(keys_from(Some(" , ,")), vec![]);
        // 개발 공개 키를 목록에 넣어도 개발 키다.
        let keys = keys_from(Some(&format!("{},{}", a.public_key, DEV_KEY.trim())));
        assert_eq!(keys.len(), 2);
        assert!(!keys[0].development);
        assert!(keys[1].development);
        // 풀리지 않는 항목은 건너뛴다.
        assert_eq!(keys_from(Some("not-base64,AAAA")), vec![]);
        let want = match option_env!("OPL_LICENSE_KEYS") {
            Some(v) if !v.trim().is_empty() => keys_from(Some(v)),
            _ => dev_keys(),
        };
        assert_eq!(license_keys(), want);
    }
}
