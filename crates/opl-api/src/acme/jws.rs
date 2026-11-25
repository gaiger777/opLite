//! ACME의 JWS(RFC 8555 §6.2·RFC 7515 평평한 JSON 꼴): 계정 키(ES256 — P-256, ring `ECDSA_P256_SHA256_FIXED_SIGNING`이
//! 내는 r‖s 64바이트가 JWS 서명 그대로), JWK·지문(RFC 7638), 외부 계정 묶기(EAB — HS256).

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};

/// base64url(채움 없음)
pub fn b64(data: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

/// DER를 PEM으로(64자 줄)
pub fn pem(label: &str, der: &[u8]) -> String {
    let body = STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in body.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).expect("base64 is ascii"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// PEM 하나의 DER(첫 `label` 덩어리)
pub fn pem_der(text: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin)? + begin.len();
    let stop = start + text[start..].find(&end)?;
    let body: String = text[start..stop]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    STANDARD.decode(body).ok()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("the ACME account key is not a P-256 PKCS#8 key")]
    Invalid,
    #[error("cannot generate a key")]
    Generate,
    #[error("the EAB HMAC key is not base64url")]
    EabKey,
}

/// ACME 계정 키(ES256)
pub struct AccountKey {
    pair: EcdsaKeyPair,
    pkcs8: Vec<u8>,
}

impl std::fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountKey")
            .field("thumbprint", &self.thumbprint())
            .finish_non_exhaustive()
    }
}

impl AccountKey {
    /// 새 키
    pub fn generate() -> Result<Self, KeyError> {
        let rng = SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| KeyError::Generate)?;
        Self::from_pkcs8(doc.as_ref().to_vec())
    }

    fn from_pkcs8(pkcs8: Vec<u8>) -> Result<Self, KeyError> {
        let pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            &pkcs8,
            &SystemRandom::new(),
        )
        .map_err(|_| KeyError::Invalid)?;
        Ok(Self { pair, pkcs8 })
    }

    /// PKCS#8 PEM(`PRIVATE KEY`)에서
    pub fn from_pem(text: &str) -> Result<Self, KeyError> {
        Self::from_pkcs8(pem_der(text, "PRIVATE KEY").ok_or(KeyError::Invalid)?)
    }

    /// PKCS#8 PEM — Raft 스펙 `key`(비밀)에 둔다.
    pub fn to_pem(&self) -> String {
        pem("PRIVATE KEY", &self.pkcs8)
    }

    /// 공개 JWK. 멤버 차례는 RFC 7638 사전 차례(`crv`·`kty`·`x`·`y`)라 그대로 지문에 쓴다.
    pub fn jwk(&self) -> Value {
        // 압축하지 않은 점: 0x04 ‖ x(32) ‖ y(32)
        let point = self.pair.public_key().as_ref();
        json!({
            "crv": "P-256",
            "kty": "EC",
            "x": b64(&point[1..33]),
            "y": b64(&point[33..65]),
        })
    }

    /// JWK 지문(RFC 7638): base64url(SHA-256(사전 차례·공백 없는 JWK))
    pub fn thumbprint(&self) -> String {
        thumbprint_of(&self.jwk())
    }

    /// HTTP-01 답·DNS-01 값의 바탕(`token.thumbprint`)
    pub fn key_authorization(&self, token: &str) -> String {
        format!("{token}.{}", self.thumbprint())
    }

    /// 평평한 JWS. `kid`가 없으면 `jwk`를 싣는다(`newAccount`). `payload`가 `None`이면 POST-as-GET(빈 문자열).
    pub fn sign(
        &self,
        url: &str,
        nonce: &str,
        kid: Option<&str>,
        payload: Option<&Value>,
    ) -> Value {
        let mut protected = json!({ "alg": "ES256", "nonce": nonce, "url": url });
        match kid {
            Some(kid) => protected["kid"] = json!(kid),
            None => protected["jwk"] = self.jwk(),
        }
        let protected = b64(protected.to_string());
        let payload = payload.map(|p| b64(p.to_string())).unwrap_or_default();
        let input = format!("{protected}.{payload}");
        let signature = self
            .pair
            .sign(&SystemRandom::new(), input.as_bytes())
            .expect("ECDSA signing does not fail");
        json!({ "protected": protected, "payload": payload, "signature": b64(signature.as_ref()) })
    }

    /// `newAccount`의 외부 계정 묶기(RFC 8555 §7.3.4): 보호 머리 `{alg: HS256, kid, url}`, 내용 = 계정 공개 JWK, EAB HMAC
    /// 키로 서명.
    pub fn eab(&self, kid: &str, hmac_key_b64url: &str, url: &str) -> Result<Value, KeyError> {
        let key = decode_b64url(hmac_key_b64url).ok_or(KeyError::EabKey)?;
        let protected = b64(json!({ "alg": "HS256", "kid": kid, "url": url }).to_string());
        let payload = b64(self.jwk().to_string());
        let tag = ring::hmac::sign(
            &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key),
            format!("{protected}.{payload}").as_bytes(),
        );
        Ok(json!({ "protected": protected, "payload": payload, "signature": b64(tag.as_ref()) }))
    }
}

/// base64url(채움 있음·없음 모두)
pub fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s.trim().trim_end_matches('=')).ok()
}

/// JWK 지문(RFC 7638). `jwk`는 필요한 멤버만 담은 객체다 — serde_json의 객체는 열쇠 차례(BTreeMap)로 적힌다.
pub fn thumbprint_of(jwk: &Value) -> String {
    b64(ring::digest::digest(&ring::digest::SHA256, jwk.to_string().as_bytes()).as_ref())
}

/// DNS-01 TXT 값: base64url(SHA-256(key authorization))
pub fn dns01_value(key_authorization: &str) -> String {
    b64(ring::digest::digest(&ring::digest::SHA256, key_authorization.as_bytes()).as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};

    #[test]
    fn rfc7638_example_thumbprint() {
        // RFC 7638 §3.1의 RSA 견본: 멤버 사전 차례·공백 없음이면 같은 지문이 나온다.
        let jwk = json!({
            "e": "AQAB",
            "kty": "RSA",
            "n": "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw"
        });
        assert_eq!(
            thumbprint_of(&jwk),
            "NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs"
        );
    }

    #[test]
    fn jws_signature_verifies_with_the_jwk() {
        let key = AccountKey::generate().unwrap();
        let again = AccountKey::from_pem(&key.to_pem()).unwrap();
        assert_eq!(key.thumbprint(), again.thumbprint());
        let jws = key.sign(
            "https://ca/new-order",
            "nonce1",
            Some("https://ca/acct/1"),
            Some(&json!({"identifiers": []})),
        );
        let protected: Value =
            serde_json::from_slice(&decode_b64url(jws["protected"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(
            protected,
            json!({"alg": "ES256", "nonce": "nonce1", "url": "https://ca/new-order", "kid": "https://ca/acct/1"})
        );
        let jwk = key.jwk();
        let mut point = vec![4u8];
        point.extend(decode_b64url(jwk["x"].as_str().unwrap()).unwrap());
        point.extend(decode_b64url(jwk["y"].as_str().unwrap()).unwrap());
        let input = format!(
            "{}.{}",
            jws["protected"].as_str().unwrap(),
            jws["payload"].as_str().unwrap()
        );
        let sig = decode_b64url(jws["signature"].as_str().unwrap()).unwrap();
        assert_eq!(sig.len(), 64);
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &point)
            .verify(input.as_bytes(), &sig)
            .unwrap();
        // POST-as-GET은 빈 내용, kid가 없으면 jwk
        let get = key.sign("https://ca/x", "n", None, None);
        assert_eq!(get["payload"], "");
        let p: Value =
            serde_json::from_slice(&decode_b64url(get["protected"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(p["jwk"], jwk);
        assert!(p.get("kid").is_none());
        assert!(!format!("{key:?}").contains("PRIVATE"));
    }

    #[test]
    fn eab_is_hs256_over_the_account_jwk() {
        let key = AccountKey::generate().unwrap();
        let hmac = b64(b"0123456789abcdef0123456789abcdef");
        let eab = key.eab("kid-1", &hmac, "https://ca/new-acct").unwrap();
        let p: Value =
            serde_json::from_slice(&decode_b64url(eab["protected"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(
            p,
            json!({"alg": "HS256", "kid": "kid-1", "url": "https://ca/new-acct"})
        );
        let payload: Value =
            serde_json::from_slice(&decode_b64url(eab["payload"].as_str().unwrap()).unwrap())
                .unwrap();
        assert_eq!(payload, key.jwk());
        let input = format!(
            "{}.{}",
            eab["protected"].as_str().unwrap(),
            eab["payload"].as_str().unwrap()
        );
        ring::hmac::verify(
            &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"0123456789abcdef0123456789abcdef"),
            input.as_bytes(),
            &decode_b64url(eab["signature"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            key.eab("k", "not base64!", "u").unwrap_err(),
            KeyError::EabKey
        );
    }

    #[test]
    fn dns01_value_and_pem() {
        // RFC 8555 §8.4: base64url(SHA-256(key authorization)) — 43자
        let v = dns01_value("token.thumb");
        assert_eq!(v.len(), 43);
        assert_eq!(
            pem_der(&pem("CERTIFICATE", b"hello"), "CERTIFICATE").unwrap(),
            b"hello"
        );
        assert!(pem_der("junk", "CERTIFICATE").is_none());
    }
}
