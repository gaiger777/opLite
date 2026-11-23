//! 클라이언트 쪽 암호화의 키 계층(설계 §9). 저장소마다 무작위 주 키(MK) 32바이트에서 HKDF-SHA256으로 쓰임새마다 키를
//! 나눈다: 청크 본문 AES-256-GCM(`data`), 청크 id HMAC(`id`), 결정적 nonce(`nonce`), 매니페스트 인증(`auth`), 지문(`fp`).
//! 복구 키(종이 키, `OPK1-…`)와 저장소 안 escrow(`keys/<지문>.json`, argon2id로 감싼 MK)도 여기 있다.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use opl_types::backup_store::ChunkId;
use ring::{aead, digest, hkdf, hmac};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 주 키 길이
pub const KEY_BYTES: usize = 32;
/// AES-256-GCM nonce 길이
pub const NONCE_BYTES: usize = 12;
/// AES-256-GCM 태그 길이
pub const TAG_BYTES: usize = 16;

const INFO_DATA: &[u8] = b"orbivisor-backup v1 data";
const INFO_ID: &[u8] = b"orbivisor-backup v1 id";
const INFO_NONCE: &[u8] = b"orbivisor-backup v1 nonce";
const INFO_AUTH: &[u8] = b"orbivisor-backup v1 auth";
const INFO_FP: &[u8] = b"orbivisor-backup v1 fingerprint";
const FP_MESSAGE: &[u8] = b"orbivisor key fingerprint";

/// 복구 키의 머리
const RECOVERY_PREFIX: &str = "OPK1-";

/// escrow 기본 argon2id 비용(설계 §9.4): 메모리 64 MiB, 3번, 병렬 1
pub const ESCROW_M_KIB: u32 = 65536;
pub const ESCROW_T: u32 = 3;
pub const ESCROW_P: u32 = 1;
/// 남이 쓴 escrow 파일을 풀 때 받아 주는 비용의 한도(메모리 1 GiB, 16번, 병렬 16) — 저장소가 주는 값으로
/// 노드 메모리를 다 쓰게 하지 않는다.
const ESCROW_MAX_M_KIB: u32 = 1 << 20;
const ESCROW_MAX_T: u32 = 16;
const ESCROW_MAX_P: u32 = 16;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeyError {
    #[error("the encryption key must be 32 bytes in base64")]
    BadKey,
    #[error("the recovery key is not valid (check for typos)")]
    BadRecoveryKey,
    #[error("the passphrase does not unlock the stored key")]
    WrongPassphrase,
    #[error("the escrowed key file is not valid: {0}")]
    BadEscrow(String),
    #[error("the key does not match the store (fingerprint {found}, store has {expected})")]
    Mismatch { expected: String, found: String },
}

/// 저장소의 주 키. `Debug`는 값을 보이지 않는다.
#[derive(Clone, PartialEq, Eq)]
pub struct MasterKey([u8; KEY_BYTES]);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

impl MasterKey {
    /// 새 무작위 키
    pub fn generate() -> MasterKey {
        let mut k = [0u8; KEY_BYTES];
        getrandom::fill(&mut k).expect("system random source");
        MasterKey(k)
    }

    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> MasterKey {
        MasterKey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }

    /// `StoreEncryption.key`(base64 32바이트)에서
    pub fn from_base64(s: &str) -> Result<MasterKey, KeyError> {
        let v = STANDARD.decode(s.trim()).map_err(|_| KeyError::BadKey)?;
        let arr: [u8; KEY_BYTES] = v.try_into().map_err(|_| KeyError::BadKey)?;
        Ok(MasterKey(arr))
    }

    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }

    /// 복구 키: `OPK1-` + base32(MK ‖ SHA-256(MK) 앞 2바이트)를 네 자씩 `-`로 묶음
    pub fn recovery_key(&self) -> String {
        let mut buf = Vec::with_capacity(KEY_BYTES + 2);
        buf.extend_from_slice(&self.0);
        buf.extend_from_slice(&digest::digest(&digest::SHA256, &self.0).as_ref()[..2]);
        let enc = data_encoding::BASE32_NOPAD.encode(&buf);
        let groups: Vec<&str> = enc
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).expect("base32 is ASCII"))
            .collect();
        format!("{RECOVERY_PREFIX}{}", groups.join("-"))
    }

    /// 복구 키를 푼다. 대소문자·공백·`-`는 가리지 않는다. 체크섬이 틀리면 거절한다.
    pub fn from_recovery_key(s: &str) -> Result<MasterKey, KeyError> {
        let s = s.trim();
        let rest = s
            .get(..RECOVERY_PREFIX.len())
            .filter(|p| p.eq_ignore_ascii_case(RECOVERY_PREFIX))
            .map(|_| &s[RECOVERY_PREFIX.len()..])
            .ok_or(KeyError::BadRecoveryKey)?;
        let cleaned: String = rest
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let raw = data_encoding::BASE32_NOPAD
            .decode(cleaned.as_bytes())
            .map_err(|_| KeyError::BadRecoveryKey)?;
        if raw.len() != KEY_BYTES + 2 {
            return Err(KeyError::BadRecoveryKey);
        }
        let (key, check) = raw.split_at(KEY_BYTES);
        if digest::digest(&digest::SHA256, key).as_ref()[..2] != *check {
            return Err(KeyError::BadRecoveryKey);
        }
        Ok(MasterKey(key.try_into().expect("32 bytes")))
    }
}

/// HKDF 출력 길이
struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn derive(mk: &MasterKey, info: &'static [u8]) -> [u8; 32] {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(&mk.0);
    let infos = [info];
    let okm = prk
        .expand(&infos, Len(32))
        .expect("32 bytes is a valid HKDF length");
    let mut out = [0u8; 32];
    okm.fill(&mut out).expect("same length");
    out
}

/// 주 키에서 나눈 키들. 저장소를 여는 동안 들고 다닌다.
pub struct StoreKeys {
    data: aead::LessSafeKey,
    id: hmac::Key,
    nonce: hmac::Key,
    auth: hmac::Key,
    fingerprint: String,
}

impl std::fmt::Debug for StoreKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreKeys")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl StoreKeys {
    pub fn new(mk: &MasterKey) -> StoreKeys {
        let data = aead::UnboundKey::new(&aead::AES_256_GCM, &derive(mk, INFO_DATA))
            .expect("32-byte AES key");
        let fp_key = hmac::Key::new(hmac::HMAC_SHA256, &derive(mk, INFO_FP));
        let fp = hmac::sign(&fp_key, FP_MESSAGE);
        StoreKeys {
            data: aead::LessSafeKey::new(data),
            id: hmac::Key::new(hmac::HMAC_SHA256, &derive(mk, INFO_ID)),
            nonce: hmac::Key::new(hmac::HMAC_SHA256, &derive(mk, INFO_NONCE)),
            auth: hmac::Key::new(hmac::HMAC_SHA256, &derive(mk, INFO_AUTH)),
            fingerprint: format_fingerprint(&fp.as_ref()[..16]),
        }
    }

    /// 키 지문 `ab:cd:…`(16바이트). 키를 알려 주지 않는다.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// 청크 id = HMAC-SHA256(id_key, raw)
    pub fn chunk_id(&self, raw: &[u8]) -> ChunkId {
        hmac::sign(&self.id, raw)
            .as_ref()
            .try_into()
            .expect("HMAC-SHA256 is 32 bytes")
    }

    /// 결정적 nonce = HMAC-SHA256(nonce_key, input) 앞 12바이트(설계 §9.2). 청크는 input = id다.
    pub fn nonce_for(&self, input: &[u8]) -> [u8; NONCE_BYTES] {
        hmac::sign(&self.nonce, input).as_ref()[..NONCE_BYTES]
            .try_into()
            .expect("12 bytes")
    }

    /// AES-256-GCM 암호화. 결과는 암호문 ‖ 태그(16).
    pub fn seal(&self, nonce: [u8; NONCE_BYTES], aad: &[u8], plain: &[u8]) -> Vec<u8> {
        let mut buf = plain.to_vec();
        self.data
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut buf,
            )
            .expect("AES-GCM seal");
        buf
    }

    /// AES-256-GCM 풀기. 태그가 틀리면 `None`.
    pub fn open(&self, nonce: [u8; NONCE_BYTES], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        let mut buf = sealed.to_vec();
        let n = self
            .data
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut buf,
            )
            .ok()?
            .len();
        buf.truncate(n);
        Some(buf)
    }

    /// 매니페스트 인증 HMAC(16진)
    pub fn auth_tag(&self, msg: &[u8]) -> String {
        hex::encode(hmac::sign(&self.auth, msg).as_ref())
    }

    /// 매니페스트 인증 확인(상수 시간)
    pub fn auth_verify(&self, msg: &[u8], tag_hex: &str) -> bool {
        match hex::decode(tag_hex) {
            Ok(tag) => hmac::verify(&self.auth, msg, &tag).is_ok(),
            Err(_) => false,
        }
    }

    /// 지문이 기대한 것과 같은지
    pub fn check_fingerprint(&self, expected: &str) -> Result<(), KeyError> {
        if self.fingerprint == expected {
            Ok(())
        } else {
            Err(KeyError::Mismatch {
                expected: expected.to_string(),
                found: self.fingerprint.clone(),
            })
        }
    }
}

fn format_fingerprint(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// 저장소 안 escrow 파일 `keys/<지문>.json`(설계 §9.4). 암호 문구에서 argon2id로 얻은 KEK로 MK를 AES-256-GCM으로 감싼다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowFile {
    /// 늘 `argon2id`
    pub kdf: String,
    /// 메모리(KiB)
    pub m: u32,
    /// 반복 수
    pub t: u32,
    /// 병렬 수
    pub p: u32,
    /// base64 16바이트
    pub salt: String,
    /// base64 12바이트
    pub nonce: String,
    /// base64 AES-256-GCM(KEK, MK) ‖ 태그
    pub wrapped: String,
    pub fingerprint: String,
}

/// escrow 파일의 객체 이름
pub fn escrow_key(fingerprint: &str) -> String {
    format!("keys/{fingerprint}.json")
}

fn kek(passphrase: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<[u8; 32], KeyError> {
    let params = argon2::Params::new(m, t, p, Some(32))
        .map_err(|e| KeyError::BadEscrow(format!("argon2 parameters: {e}")))?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; 32];
    a.hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| KeyError::BadEscrow(format!("argon2: {e}")))?;
    Ok(out)
}

/// AAD: 지문(감싼 키를 다른 지문의 파일로 옮겨 놓지 못하게)
fn escrow_aad(fingerprint: &str) -> Vec<u8> {
    format!("orbivisor escrow {fingerprint}").into_bytes()
}

/// 기본 비용으로 MK를 감싼다.
pub fn escrow_wrap(mk: &MasterKey, passphrase: &str) -> Result<EscrowFile, KeyError> {
    escrow_wrap_with(mk, passphrase, ESCROW_M_KIB, ESCROW_T, ESCROW_P)
}

/// 비용을 골라 감싼다(시험은 작은 값을 쓴다).
pub fn escrow_wrap_with(
    mk: &MasterKey,
    passphrase: &str,
    m: u32,
    t: u32,
    p: u32,
) -> Result<EscrowFile, KeyError> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; NONCE_BYTES];
    getrandom::fill(&mut salt).expect("system random source");
    getrandom::fill(&mut nonce).expect("system random source");
    let k = kek(passphrase, &salt, m, t, p)?;
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, &k).expect("32-byte AES key"),
    );
    let fingerprint = StoreKeys::new(mk).fingerprint;
    let mut buf = mk.0.to_vec();
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(escrow_aad(&fingerprint)),
        &mut buf,
    )
    .expect("AES-GCM seal");
    Ok(EscrowFile {
        kdf: "argon2id".into(),
        m,
        t,
        p,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        wrapped: STANDARD.encode(buf),
        fingerprint,
    })
}

/// escrow 파일을 암호 문구로 푼다. 풀린 키의 지문이 파일의 지문과 다르면 거절한다.
pub fn escrow_unwrap(file: &EscrowFile, passphrase: &str) -> Result<MasterKey, KeyError> {
    let bad = |s: &str| KeyError::BadEscrow(s.to_string());
    if file.kdf != "argon2id" {
        return Err(bad("unknown key derivation"));
    }
    if file.m > ESCROW_MAX_M_KIB || file.t > ESCROW_MAX_T || file.p > ESCROW_MAX_P {
        return Err(bad("key derivation cost is too high"));
    }
    let salt = STANDARD.decode(&file.salt).map_err(|_| bad("salt"))?;
    let nonce: [u8; NONCE_BYTES] = STANDARD
        .decode(&file.nonce)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| bad("nonce"))?;
    let mut buf = STANDARD
        .decode(&file.wrapped)
        .map_err(|_| bad("wrapped key"))?;
    if buf.len() != KEY_BYTES + TAG_BYTES {
        return Err(bad("wrapped key"));
    }
    let k = kek(passphrase, &salt, file.m, file.t, file.p)?;
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, &k).expect("32-byte AES key"),
    );
    let plain = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(escrow_aad(&file.fingerprint)),
            &mut buf,
        )
        .map_err(|_| KeyError::WrongPassphrase)?;
    let mk = MasterKey(plain.try_into().map_err(|_| bad("wrapped key"))?);
    StoreKeys::new(&mk).check_fingerprint(&file.fingerprint)?;
    Ok(mk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed() -> MasterKey {
        MasterKey::from_bytes(std::array::from_fn(|i| i as u8))
    }

    #[test]
    fn derivation_is_stable() {
        // 고정 벡터: 같은 MK면 언제나 같은 지문·id·nonce(형식이 바뀌면 이 값이 바뀐다 — 바꾸면 안 된다)
        let k = StoreKeys::new(&fixed());
        let k2 = StoreKeys::new(&fixed());
        assert_eq!(k.fingerprint(), k2.fingerprint());
        assert_eq!(k.chunk_id(b"hello"), k2.chunk_id(b"hello"));
        assert_eq!(k.nonce_for(b"x"), k2.nonce_for(b"x"));
        assert!(opl_types::backup_store::valid_fingerprint(k.fingerprint()));
        // 다른 키 → 다른 지문·id
        let other = StoreKeys::new(&MasterKey::from_bytes([7; 32]));
        assert_ne!(other.fingerprint(), k.fingerprint());
        assert_ne!(other.chunk_id(b"hello"), k.chunk_id(b"hello"));
        // id는 평문의 SHA-256이 아니다
        assert_ne!(
            k.chunk_id(b"hello").as_slice(),
            digest::digest(&digest::SHA256, b"hello").as_ref()
        );
        // HKDF-SHA256(salt 없음)을 HMAC으로 따로 계산한 값과 같다
        assert_eq!(k.fingerprint(), fixed_vector_fingerprint());
        assert_eq!(hex::encode(k.chunk_id(b"")), fixed_vector_empty_id());
    }

    /// 지문을 HMAC만으로 따로 계산한다(ring HKDF와 같은 RFC 5869 규칙인지 대조).
    fn fixed_vector_fingerprint() -> String {
        // 독립 계산: HKDF-Expand(HKDF-Extract(zero salt, MK), info, 32) → HMAC(fp_key, msg)[..16]
        let salt = hmac::Key::new(hmac::HMAC_SHA256, &[0u8; 32]);
        let prk = hmac::sign(&salt, fixed().as_bytes());
        let prk = hmac::Key::new(hmac::HMAC_SHA256, prk.as_ref());
        let mut t1 = INFO_FP.to_vec();
        t1.push(1);
        let fp_key = hmac::sign(&prk, &t1);
        let fp = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, fp_key.as_ref()),
            FP_MESSAGE,
        );
        format_fingerprint(&fp.as_ref()[..16])
    }

    fn fixed_vector_empty_id() -> String {
        let salt = hmac::Key::new(hmac::HMAC_SHA256, &[0u8; 32]);
        let prk = hmac::sign(&salt, fixed().as_bytes());
        let prk = hmac::Key::new(hmac::HMAC_SHA256, prk.as_ref());
        let mut t1 = INFO_ID.to_vec();
        t1.push(1);
        let id_key = hmac::sign(&prk, &t1);
        hex::encode(hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, id_key.as_ref()),
            b"",
        ))
    }

    #[test]
    fn seal_open_roundtrip_and_tamper() {
        let k = StoreKeys::new(&fixed());
        let n = k.nonce_for(b"id");
        let s = k.seal(n, b"aad", b"secret data");
        assert_eq!(s.len(), 11 + TAG_BYTES);
        assert_eq!(k.open(n, b"aad", &s).unwrap(), b"secret data");
        assert!(k.open(n, b"aae", &s).is_none());
        let mut t = s.clone();
        t[3] ^= 1;
        assert!(k.open(n, b"aad", &t).is_none());
        let last = t.len() - 1;
        let mut t = s.clone();
        t[last] ^= 0x80;
        assert!(k.open(n, b"aad", &t).is_none());
    }

    #[test]
    fn base64_key() {
        let mk = MasterKey::generate();
        assert_eq!(MasterKey::from_base64(&mk.to_base64()).unwrap(), mk);
        assert_eq!(MasterKey::from_base64("AAAA"), Err(KeyError::BadKey));
        assert_eq!(format!("{mk:?}"), "MasterKey(<redacted>)");
        assert!(!format!("{:?}", StoreKeys::new(&mk)).contains(&mk.to_base64()));
    }

    #[test]
    fn recovery_key_roundtrip_and_checksum() {
        let mk = fixed();
        let rk = mk.recovery_key();
        assert!(rk.starts_with("OPK1-"));
        assert!(rk.len() < 80, "{rk}");
        assert_eq!(MasterKey::from_recovery_key(&rk).unwrap(), mk);
        // 소문자·공백·묶음 없음도 받는다
        let loose = rk.to_lowercase().replace('-', " ");
        let loose = format!("OPK1-{}", &loose[5..]);
        assert_eq!(MasterKey::from_recovery_key(&loose).unwrap(), mk);
        // 한 자 바꿈 → 체크섬(또는 base32) 거절
        let mut chars: Vec<char> = rk.chars().collect();
        let i = 10;
        chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
        let typo: String = chars.into_iter().collect();
        assert_eq!(
            MasterKey::from_recovery_key(&typo),
            Err(KeyError::BadRecoveryKey)
        );
        assert_eq!(
            MasterKey::from_recovery_key("OPK2-AAAA"),
            Err(KeyError::BadRecoveryKey)
        );
        assert_eq!(
            MasterKey::from_recovery_key(""),
            Err(KeyError::BadRecoveryKey)
        );
    }

    #[test]
    fn escrow_roundtrip_wrong_passphrase_and_swap() {
        let mk = MasterKey::generate();
        let f = escrow_wrap_with(&mk, "correct horse", 64, 1, 1).unwrap();
        assert_eq!(f.fingerprint, StoreKeys::new(&mk).fingerprint());
        assert_eq!(escrow_unwrap(&f, "correct horse").unwrap(), mk);
        assert_eq!(
            escrow_unwrap(&f, "wrong horse"),
            Err(KeyError::WrongPassphrase)
        );
        // 지문을 바꿔 놓은 파일 → AAD가 달라 풀리지 않는다
        let mut g = f.clone();
        g.fingerprint = StoreKeys::new(&MasterKey::generate()).fingerprint().into();
        assert_eq!(
            escrow_unwrap(&g, "correct horse"),
            Err(KeyError::WrongPassphrase)
        );
        let mut h = f.clone();
        h.m = u32::MAX;
        assert!(matches!(
            escrow_unwrap(&h, "correct horse"),
            Err(KeyError::BadEscrow(_))
        ));
        // JSON 왕복
        let j = serde_json::to_string(&f).unwrap();
        assert_eq!(serde_json::from_str::<EscrowFile>(&j).unwrap(), f);
        assert_eq!(escrow_key("ab:cd"), "keys/ab:cd.json");
    }
}
