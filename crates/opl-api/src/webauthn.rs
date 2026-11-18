//! WebAuthn(패스키) 최소 검증기(계획 62 설계 §10.1). `webauthn-rs`(MPL-2.0, openssl) 대신 필요한 만큼만 짠다.
//!
//! - 등록은 `attestation: "none"`으로 요청하고 증명문(`attStmt`)은 보지 않는다. 인증기 데이터의 공개 키만 받는다.
//! - 알고리즘은 ES256(P-256)·EdDSA(Ed25519)·RS256(RSA PKCS#1 v1.5)이고 서명 확인은 ring이 한다. CBOR는 `ciborium`.
//! - 확인하는 것: `clientDataJSON`의 `type`·`challenge`·`origin`(허용 목록)·`crossOrigin`, 인증기 데이터의 RP ID
//!   해시·UP(사용자 있음) 플래그·요구하면 UV(사용자 확인) 플래그, 로그인의 서명 횟수(되감기 거절).
//!
//! IO가 없는 순수 함수다. 상태(challenge·저장된 키·서명 횟수)는 부르는 쪽(`two_factor.rs`)이 지킨다.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ciborium::Value;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// COSE 알고리즘 ES256
pub const ES256: i64 = -7;
/// COSE 알고리즘 EdDSA
pub const EDDSA: i64 = -8;
/// COSE 알고리즘 RS256
pub const RS256: i64 = -257;
/// 받는 알고리즘(등록 옵션 `pubKeyCredParams`의 차례)
pub const ALGORITHMS: [i64; 3] = [ES256, EDDSA, RS256];

/// 인증기 데이터 플래그
const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;
const FLAG_ED: u8 = 0x80;

/// 자격 증명 id의 최대 길이(WebAuthn Level 3: 1023바이트)
const MAX_CREDENTIAL_ID: usize = 1023;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WebauthnError {
    #[error("malformed client data")]
    ClientData,
    #[error("unexpected client data type")]
    Type,
    #[error("challenge mismatch")]
    Challenge,
    #[error("origin {0:?} is not allowed")]
    Origin(String),
    #[error("cross-origin ceremonies are not allowed")]
    CrossOrigin,
    #[error("malformed authenticator data")]
    AuthData,
    #[error("malformed attestation object")]
    Attestation,
    #[error("RP ID hash mismatch")]
    RpId,
    #[error("the user was not present")]
    UserPresent,
    #[error("the user was not verified")]
    UserVerified,
    #[error("unsupported or malformed public key")]
    Key,
    #[error("bad signature")]
    Signature,
    #[error("sign count went backwards (cloned authenticator?)")]
    Counter,
}

/// 한 번의 등록·로그인에서 기대하는 값
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    /// 서버가 낸 challenge(원래 바이트)
    pub challenge: &'a [u8],
    /// RP ID(도메인)
    pub rp_id: &'a str,
    /// 받는 origin(`https://opl.example.com`, 포트 포함). 하나라도 같아야 한다.
    pub origins: &'a [String],
    /// UV(사용자 확인 — PIN·생체) 플래그를 요구한다.
    pub user_verification: bool,
}

/// 확인한 등록
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub credential_id: Vec<u8>,
    /// COSE 공개 키(인증기가 준 CBOR 바이트 그대로)
    pub cose_key: Vec<u8>,
    pub algorithm: i64,
    pub sign_count: u32,
    pub user_verified: bool,
}

/// 확인한 로그인
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assertion {
    /// 새 서명 횟수(저장한다)
    pub sign_count: u32,
    pub user_verified: bool,
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(default, rename = "crossOrigin")]
    cross_origin: bool,
}

/// 길이가 같은 두 바이트열을 내용과 관계없이 같은 시간에 비교한다.
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// base64url(패딩은 있어도 된다)
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')).ok()
}

pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// `clientDataJSON`을 확인하고 그 SHA-256을 돌려준다(서명 대상의 뒷부분).
fn check_client_data(
    json: &[u8],
    kind: &str,
    expected: &Expected<'_>,
) -> Result<[u8; 32], WebauthnError> {
    let data: ClientData = serde_json::from_slice(json).map_err(|_| WebauthnError::ClientData)?;
    if data.kind != kind {
        return Err(WebauthnError::Type);
    }
    let challenge = b64url_decode(&data.challenge).ok_or(WebauthnError::Challenge)?;
    if !same_bytes(&challenge, expected.challenge) {
        return Err(WebauthnError::Challenge);
    }
    if !expected.origins.contains(&data.origin) {
        return Err(WebauthnError::Origin(data.origin));
    }
    if data.cross_origin {
        return Err(WebauthnError::CrossOrigin);
    }
    Ok(Sha256::digest(json).into())
}

/// 인증기 데이터의 앞부분
struct AuthData<'a> {
    rp_id_hash: &'a [u8],
    flags: u8,
    sign_count: u32,
    /// 플래그 뒤의 나머지(자격 증명 데이터·확장)
    rest: &'a [u8],
}

fn parse_auth_data(bytes: &[u8]) -> Result<AuthData<'_>, WebauthnError> {
    if bytes.len() < 37 {
        return Err(WebauthnError::AuthData);
    }
    Ok(AuthData {
        rp_id_hash: &bytes[..32],
        flags: bytes[32],
        sign_count: u32::from_be_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]),
        rest: &bytes[37..],
    })
}

/// RP ID 해시와 UP·UV 플래그
fn check_flags(auth: &AuthData<'_>, expected: &Expected<'_>) -> Result<(), WebauthnError> {
    let want: [u8; 32] = Sha256::digest(expected.rp_id.as_bytes()).into();
    if !same_bytes(auth.rp_id_hash, &want) {
        return Err(WebauthnError::RpId);
    }
    if auth.flags & FLAG_UP == 0 {
        return Err(WebauthnError::UserPresent);
    }
    if expected.user_verification && auth.flags & FLAG_UV == 0 {
        return Err(WebauthnError::UserVerified);
    }
    Ok(())
}

/// CBOR 값 하나를 읽고 남은 바이트를 돌려준다.
fn read_cbor(bytes: &[u8]) -> Option<(Value, &[u8])> {
    let mut rest = bytes;
    let value: Value = ciborium::from_reader(&mut rest).ok()?;
    Some((value, rest))
}

fn map_get<'a>(map: &'a [(Value, Value)], key: &Value) -> Option<&'a Value> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn int(v: &Value) -> Option<i128> {
    v.as_integer().map(i128::from)
}

/// 확인한 COSE 공개 키
enum PublicKey {
    /// 압축하지 않은 P-256 점(0x04 ‖ x ‖ y)
    Es256(Vec<u8>),
    Ed25519(Vec<u8>),
    Rs256 {
        n: Vec<u8>,
        e: Vec<u8>,
    },
}

impl PublicKey {
    fn algorithm(&self) -> i64 {
        match self {
            PublicKey::Es256(_) => ES256,
            PublicKey::Ed25519(_) => EDDSA,
            PublicKey::Rs256 { .. } => RS256,
        }
    }

    fn verify(&self, message: &[u8], sig: &[u8]) -> Result<(), WebauthnError> {
        let ok = match self {
            PublicKey::Es256(point) => {
                UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, point)
                    .verify(message, sig)
            }
            PublicKey::Ed25519(x) => {
                UnparsedPublicKey::new(&signature::ED25519, x).verify(message, sig)
            }
            PublicKey::Rs256 { n, e } => RsaPublicKeyComponents { n, e }.verify(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                message,
                sig,
            ),
        };
        ok.map_err(|_| WebauthnError::Signature)
    }
}

/// COSE_Key(RFC 9053) 가운데 받는 세 모양만 푼다.
fn parse_cose_key(value: &Value) -> Result<PublicKey, WebauthnError> {
    let map = value.as_map().ok_or(WebauthnError::Key)?;
    let get = |k: i64| map_get(map, &Value::Integer(k.into()));
    let bytes = |k: i64| get(k).and_then(Value::as_bytes).cloned();
    let kty = get(1).and_then(int).ok_or(WebauthnError::Key)?;
    let alg = get(3).and_then(int).ok_or(WebauthnError::Key)?;
    match (kty, alg) {
        // EC2, ES256, crv P-256
        (2, a) if a == i128::from(ES256) => {
            let (Some(1), Some(x), Some(y)) = (get(-1).and_then(int), bytes(-2), bytes(-3)) else {
                return Err(WebauthnError::Key);
            };
            if x.len() != 32 || y.len() != 32 {
                return Err(WebauthnError::Key);
            }
            let mut point = Vec::with_capacity(65);
            point.push(0x04);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            Ok(PublicKey::Es256(point))
        }
        // OKP, EdDSA, crv Ed25519
        (1, a) if a == i128::from(EDDSA) => {
            let (Some(6), Some(x)) = (get(-1).and_then(int), bytes(-2)) else {
                return Err(WebauthnError::Key);
            };
            if x.len() != 32 {
                return Err(WebauthnError::Key);
            }
            Ok(PublicKey::Ed25519(x))
        }
        // RSA, RS256
        (3, a) if a == i128::from(RS256) => {
            let (Some(n), Some(e)) = (bytes(-1), bytes(-2)) else {
                return Err(WebauthnError::Key);
            };
            // ring이 받는 2048~8192비트
            let bits = n.iter().skip_while(|b| **b == 0).count() * 8;
            if !(2048..=8192).contains(&bits) || e.is_empty() || e.len() > 8 {
                return Err(WebauthnError::Key);
            }
            Ok(PublicKey::Rs256 { n, e })
        }
        _ => Err(WebauthnError::Key),
    }
}

/// 저장한 COSE 키 바이트를 푼다.
fn decode_cose_key(cose_key: &[u8]) -> Result<PublicKey, WebauthnError> {
    match read_cbor(cose_key) {
        Some((value, [])) => parse_cose_key(&value),
        _ => Err(WebauthnError::Key),
    }
}

/// 등록 응답(`navigator.credentials.create`)을 확인한다.
pub fn verify_registration(
    expected: &Expected<'_>,
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> Result<Registration, WebauthnError> {
    check_client_data(client_data_json, "webauthn.create", expected)?;
    let (att, rest) = read_cbor(attestation_object).ok_or(WebauthnError::Attestation)?;
    if !rest.is_empty() {
        return Err(WebauthnError::Attestation);
    }
    let map = att.as_map().ok_or(WebauthnError::Attestation)?;
    // 증명 형식은 보지 않는다(`attestation: "none"`으로 요청했고 MDS가 없다). 모양만 맞으면 된다.
    map_get(map, &Value::Text("fmt".into()))
        .and_then(Value::as_text)
        .ok_or(WebauthnError::Attestation)?;
    let auth_bytes = map_get(map, &Value::Text("authData".into()))
        .and_then(Value::as_bytes)
        .ok_or(WebauthnError::Attestation)?;
    let auth = parse_auth_data(auth_bytes)?;
    check_flags(&auth, expected)?;
    if auth.flags & FLAG_AT == 0 {
        return Err(WebauthnError::AuthData);
    }
    // aaguid(16) ‖ 자격 증명 id 길이(2) ‖ id ‖ COSE 키 ‖ (ED면) 확장
    let rest = auth.rest;
    if rest.len() < 18 {
        return Err(WebauthnError::AuthData);
    }
    let id_len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
    if id_len == 0 || id_len > MAX_CREDENTIAL_ID || rest.len() < 18 + id_len {
        return Err(WebauthnError::AuthData);
    }
    let credential_id = rest[18..18 + id_len].to_vec();
    let key_bytes = &rest[18 + id_len..];
    let (key, after) = read_cbor(key_bytes).ok_or(WebauthnError::Key)?;
    let has_extensions = auth.flags & FLAG_ED != 0;
    if has_extensions {
        // 확장 맵 하나가 정확히 남아야 한다.
        match read_cbor(after) {
            Some((Value::Map(_), [])) => {}
            _ => return Err(WebauthnError::AuthData),
        }
    } else if !after.is_empty() {
        return Err(WebauthnError::AuthData);
    }
    let public = parse_cose_key(&key)?;
    let cose_key = key_bytes[..key_bytes.len() - after.len()].to_vec();
    Ok(Registration {
        credential_id,
        cose_key,
        algorithm: public.algorithm(),
        sign_count: auth.sign_count,
        user_verified: auth.flags & FLAG_UV != 0,
    })
}

/// 로그인 응답(`navigator.credentials.get`)을 저장한 키 `cose_key`와 서명 횟수 `stored_count`로 확인한다.
/// 서명 횟수는 둘 중 하나라도 0이 아니면 늘어야 한다(둘 다 0이면 횟수를 세지 않는 인증기다).
pub fn verify_assertion(
    expected: &Expected<'_>,
    cose_key: &[u8],
    stored_count: u32,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    sig: &[u8],
) -> Result<Assertion, WebauthnError> {
    let client_hash = check_client_data(client_data_json, "webauthn.get", expected)?;
    let auth = parse_auth_data(authenticator_data)?;
    check_flags(&auth, expected)?;
    let key = decode_cose_key(cose_key)?;
    let mut message = Vec::with_capacity(authenticator_data.len() + 32);
    message.extend_from_slice(authenticator_data);
    message.extend_from_slice(&client_hash);
    key.verify(&message, sig)?;
    if (auth.sign_count != 0 || stored_count != 0) && auth.sign_count <= stored_count {
        return Err(WebauthnError::Counter);
    }
    Ok(Assertion {
        sign_count: auth.sign_count,
        user_verified: auth.flags & FLAG_UV != 0,
    })
}

/// 저장한 COSE 키의 알고리즘(모양이 틀리면 None)
pub fn key_algorithm(cose_key: &[u8]) -> Option<i64> {
    decode_cose_key(cose_key).ok().map(|k| k.algorithm())
}
