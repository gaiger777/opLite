//! 청크 객체 `OPC1`(설계 §7.2). 블롭(`config`·`vars.fd`·TPM 상태)도 같은 형식이다.
//!
//! ```text
//! 0  "OPC1"   4 flags u8 (bit0 lz4, bit1 암호화)   5 예약 3바이트(0)   8 raw_len u32 LE   12 본문
//! 평문:   payload(lz4 블록이거나 그대로)
//! 암호화: nonce(12) ‖ AES-256-GCM(payload) ‖ tag(16), AAD = 앞 12바이트 ‖ 자리 id(32)
//! ```

use orbivisor_types::backup_store::{CHUNK_BYTES, ChunkId};
use ring::digest;

use super::FormatError;
use crate::crypto::{NONCE_BYTES, StoreKeys, TAG_BYTES};

pub const MAGIC: &[u8; 4] = b"OPC1";
pub const HEADER_BYTES: usize = 12;
pub const FLAG_LZ4: u8 = 1;
pub const FLAG_ENCRYPTED: u8 = 2;
/// 블롭의 최대 크기(풀었을 때). aarch64 UEFI 변수 저장소(64 MiB)보다 넉넉하게.
pub const MAX_BLOB_BYTES: usize = 256 << 20;

/// 인코딩한 청크: id와 저장할 바이트
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedChunk {
    pub id: ChunkId,
    pub data: Vec<u8>,
}

/// 내용이 모두 0인가(0 청크는 저장하지 않는다 — §7.1)
pub fn is_zero(raw: &[u8]) -> bool {
    // 8바이트씩 보면 4 MiB에서도 충분히 빠르다
    let (words, rest) = raw.as_chunks::<8>();
    words.iter().map(|c| u64::from_ne_bytes(*c)).all(|w| w == 0) && rest.iter().all(|b| *b == 0)
}

/// 청크 id: 평문 저장소는 SHA-256(raw), 암호화 저장소는 HMAC-SHA256(id_key, raw)
pub fn chunk_id(raw: &[u8], keys: Option<&StoreKeys>) -> ChunkId {
    match keys {
        Some(k) => k.chunk_id(raw),
        None => sha256(raw),
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    digest::digest(&digest::SHA256, data)
        .as_ref()
        .try_into()
        .expect("SHA-256 is 32 bytes")
}

/// 청크를 인코딩한다(id 계산·압축·암호화). 길이는 [`CHUNK_BYTES`] 이하여야 한다.
pub fn encode(raw: &[u8], keys: Option<&StoreKeys>) -> EncodedChunk {
    assert!(raw.len() as u64 <= CHUNK_BYTES, "chunk larger than 4 MiB");
    let id = chunk_id(raw, keys);
    let nonce = keys.map(|k| k.nonce_for(&id));
    EncodedChunk {
        id,
        data: seal(raw, keys, &id, nonce),
    }
}

/// 객체를 만든다. `aad_id`는 AAD의 자리 id, `nonce`는 암호화일 때만.
fn seal(
    raw: &[u8],
    keys: Option<&StoreKeys>,
    aad_id: &[u8; 32],
    nonce: Option<[u8; NONCE_BYTES]>,
) -> Vec<u8> {
    let compressed = lz4_flex::block::compress(raw);
    let use_lz4 = !raw.is_empty() && compressed.len() <= raw.len() - raw.len() / 8;
    let payload: &[u8] = if use_lz4 { &compressed } else { raw };
    let mut flags = if use_lz4 { FLAG_LZ4 } else { 0 };
    if keys.is_some() {
        flags |= FLAG_ENCRYPTED;
    }
    let mut out = Vec::with_capacity(HEADER_BYTES + payload.len() + NONCE_BYTES + TAG_BYTES);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[flags, 0, 0, 0]);
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    match (keys, nonce) {
        (Some(k), Some(nonce)) => {
            let aad = aad(&out[..HEADER_BYTES], aad_id);
            out.extend_from_slice(&nonce);
            out.extend_from_slice(&k.seal(nonce, &aad, payload));
        }
        _ => out.extend_from_slice(payload),
    }
    out
}

fn aad(header: &[u8], id: &[u8; 32]) -> Vec<u8> {
    let mut a = Vec::with_capacity(HEADER_BYTES + 32);
    a.extend_from_slice(header);
    a.extend_from_slice(id);
    a
}

/// 머리를 읽는다: (flags, raw_len). 저장소의 암호화 여부와 플래그가 맞는지, 본문 길이가 말이 되는지 본다.
/// 키 없이 할 수 있는 확인이다(검증 `metadata`·복제 받는 쪽).
pub fn check_header(
    obj: &[u8],
    encrypted: bool,
    max_raw: usize,
) -> Result<(u8, usize), FormatError> {
    if obj.len() < HEADER_BYTES || &obj[..4] != MAGIC {
        return Err(FormatError::Corrupt("not a chunk object".into()));
    }
    let flags = obj[4];
    if flags & !(FLAG_LZ4 | FLAG_ENCRYPTED) != 0 || obj[5..8] != [0, 0, 0] {
        return Err(FormatError::Corrupt("unknown chunk flags".into()));
    }
    if (flags & FLAG_ENCRYPTED != 0) != encrypted {
        return Err(FormatError::Corrupt(if encrypted {
            "chunk is not encrypted in an encrypted store".into()
        } else {
            "chunk is encrypted in a plain store".into()
        }));
    }
    let raw_len = u32::from_le_bytes(obj[8..12].try_into().expect("4 bytes")) as usize;
    if raw_len > max_raw {
        return Err(FormatError::Corrupt("chunk is too large".into()));
    }
    let body = obj.len() - HEADER_BYTES;
    let overhead = if encrypted {
        NONCE_BYTES + TAG_BYTES
    } else {
        0
    };
    if body < overhead {
        return Err(FormatError::Corrupt("chunk is truncated".into()));
    }
    let payload = body - overhead;
    let ok = if flags & FLAG_LZ4 != 0 {
        // lz4 블록은 원본보다 짧게 둘 때만 쓴다
        payload < raw_len
    } else {
        payload == raw_len
    };
    if !ok {
        return Err(FormatError::Corrupt(
            "chunk length does not match its header".into(),
        ));
    }
    Ok((flags, raw_len))
}

/// 저장된 청크의 최소 크기(검증 `metadata`가 `HEAD`로 보는 것)
pub fn min_object_bytes(encrypted: bool) -> u64 {
    (HEADER_BYTES
        + if encrypted {
            NONCE_BYTES + TAG_BYTES
        } else {
            0
        }) as u64
}

/// 청크 객체를 풀고 id를 다시 계산해 맞춰 본다.
pub fn decode(obj: &[u8], id: &ChunkId, keys: Option<&StoreKeys>) -> Result<Vec<u8>, FormatError> {
    let raw = open(obj, keys, id, CHUNK_BYTES as usize)?;
    if chunk_id(&raw, keys) != *id {
        return Err(FormatError::Corrupt(
            "chunk content does not match its id".into(),
        ));
    }
    Ok(raw)
}

fn open(
    obj: &[u8],
    keys: Option<&StoreKeys>,
    aad_id: &[u8; 32],
    max_raw: usize,
) -> Result<Vec<u8>, FormatError> {
    let (flags, raw_len) = check_header(obj, keys.is_some(), max_raw)?;
    let payload = match keys {
        Some(k) => {
            let nonce: [u8; NONCE_BYTES] = obj[HEADER_BYTES..HEADER_BYTES + NONCE_BYTES]
                .try_into()
                .expect("12 bytes");
            let aad = aad(&obj[..HEADER_BYTES], aad_id);
            k.open(nonce, &aad, &obj[HEADER_BYTES + NONCE_BYTES..])
                .ok_or_else(|| FormatError::Corrupt("chunk authentication failed".into()))?
        }
        None => obj[HEADER_BYTES..].to_vec(),
    };
    if flags & FLAG_LZ4 == 0 {
        return Ok(payload);
    }
    let mut raw = vec![0u8; raw_len];
    let n = lz4_flex::block::decompress_into(&payload, &mut raw)
        .map_err(|e| FormatError::Corrupt(format!("chunk does not decompress: {e}")))?;
    if n != raw_len {
        return Err(FormatError::Corrupt(
            "chunk length does not match its header".into(),
        ));
    }
    Ok(raw)
}

/// 블롭의 AAD 자리 id: 이름에 묶어 `config`를 `vars.fd` 자리에 옮겨 놓지 못하게
fn blob_slot(name: &str) -> [u8; 32] {
    sha256(format!("orbivisor blob {name}").as_bytes())
}

/// 블롭을 인코딩한다. 암호화면 nonce = HMAC(nonce_key, 자리 id ‖ 내용 id) — 청크의 nonce(입력 32바이트)와 겹치지 않는다.
pub fn encode_blob(name: &str, raw: &[u8], keys: Option<&StoreKeys>) -> Vec<u8> {
    assert!(raw.len() <= MAX_BLOB_BYTES, "blob too large");
    let slot = blob_slot(name);
    let nonce = keys.map(|k| {
        let mut input = Vec::with_capacity(64);
        input.extend_from_slice(&slot);
        input.extend_from_slice(&k.chunk_id(raw));
        k.nonce_for(&input)
    });
    seal(raw, keys, &slot, nonce)
}

/// 블롭을 푼다(무결성은 매니페스트의 SHA-256과 AEAD 태그가 본다).
pub fn decode_blob(
    name: &str,
    obj: &[u8],
    keys: Option<&StoreKeys>,
) -> Result<Vec<u8>, FormatError> {
    open(obj, keys, &blob_slot(name), MAX_BLOB_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::MasterKey;

    fn keys() -> StoreKeys {
        StoreKeys::new(&MasterKey::from_bytes([3; 32]))
    }

    fn compressible() -> Vec<u8> {
        (0..65536u32).flat_map(|i| (i % 7).to_le_bytes()).collect()
    }

    fn random(n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        getrandom::fill(&mut v).unwrap();
        v
    }

    #[test]
    fn zero_detection() {
        assert!(is_zero(&[0; 4097]));
        assert!(is_zero(&[]));
        let mut v = vec![0u8; 4099];
        v[4098] = 1;
        assert!(!is_zero(&v));
        v[4098] = 0;
        v[3] = 1;
        assert!(!is_zero(&v));
    }

    #[test]
    fn plain_roundtrip_compressed_and_not() {
        let raw = compressible();
        let c = encode(&raw, None);
        assert_eq!(c.id, sha256(&raw));
        assert_eq!(c.data[4], FLAG_LZ4);
        assert!(c.data.len() < raw.len() / 2);
        assert_eq!(decode(&c.data, &c.id, None).unwrap(), raw);

        let raw = random(100_000);
        let c = encode(&raw, None);
        assert_eq!(c.data[4], 0, "random data is stored as is");
        assert_eq!(c.data.len(), raw.len() + HEADER_BYTES);
        assert_eq!(decode(&c.data, &c.id, None).unwrap(), raw);
    }

    #[test]
    fn encrypted_roundtrip_deterministic() {
        let k = keys();
        for raw in [compressible(), random(5000), vec![1u8]] {
            let a = encode(&raw, Some(&k));
            let b = encode(&raw, Some(&k));
            assert_eq!(a, b, "same chunk → same bytes");
            assert_eq!(a.data[4] & FLAG_ENCRYPTED, FLAG_ENCRYPTED);
            assert_eq!(decode(&a.data, &a.id, Some(&k)).unwrap(), raw);
            // 평문이 그대로 보이지 않는다
            if raw.len() > 100 {
                assert!(
                    !a.data
                        .windows(32)
                        .any(|w| raw.windows(32).take(10).any(|r| r == w))
                );
            }
        }
    }

    #[test]
    fn rejects_moved_and_tampered() {
        let k = keys();
        let a = encode(&random(3000), Some(&k));
        let b = encode(&random(3000), Some(&k));
        // 다른 id 자리에 놓인 객체
        assert!(decode(&a.data, &b.id, Some(&k)).is_err());
        // 태그 1비트
        let mut t = a.data.clone();
        let last = t.len() - 1;
        t[last] ^= 1;
        assert!(decode(&t, &a.id, Some(&k)).is_err());
        // 머리 플래그(압축 표시) 바꿈 → AAD가 달라 거절
        let mut t = a.data.clone();
        t[4] ^= FLAG_LZ4;
        assert!(decode(&t, &a.id, Some(&k)).is_err());
        // 평문 저장소: 내용 1바이트 → id 불일치
        let p = encode(&random(3000), None);
        let mut t = p.data.clone();
        t[HEADER_BYTES + 5] ^= 1;
        assert!(decode(&t, &p.id, None).is_err());
        // 평문 객체를 암호화 저장소로, 그 반대
        assert!(decode(&p.data, &p.id, Some(&k)).is_err());
        assert!(decode(&a.data, &a.id, None).is_err());
        // 잘림
        assert!(decode(&a.data[..20], &a.id, Some(&k)).is_err());
        assert!(check_header(b"OPC", false, 10).is_err());
    }

    #[test]
    fn header_checks() {
        let p = encode(&compressible(), None);
        let (flags, len) = check_header(&p.data, false, CHUNK_BYTES as usize).unwrap();
        assert_eq!((flags, len), (FLAG_LZ4, compressible().len()));
        let mut t = p.data.clone();
        t[5] = 1;
        assert!(check_header(&t, false, CHUNK_BYTES as usize).is_err());
        let mut t = p.data.clone();
        t[8..12].copy_from_slice(&(5u32 << 20).to_le_bytes());
        assert!(check_header(&t, false, CHUNK_BYTES as usize).is_err());
        assert_eq!(min_object_bytes(false), 12);
        assert_eq!(min_object_bytes(true), 40);
    }

    #[test]
    fn full_chunk_roundtrip() {
        let raw = random(CHUNK_BYTES as usize);
        let c = encode(&raw, Some(&keys()));
        assert_eq!(decode(&c.data, &c.id, Some(&keys())).unwrap(), raw);
    }

    #[test]
    fn blobs_bind_to_name() {
        let k = keys();
        let raw = b"{\"vm_name\":\"web\"}".to_vec();
        let o = encode_blob("config", &raw, Some(&k));
        assert_eq!(decode_blob("config", &o, Some(&k)).unwrap(), raw);
        assert!(decode_blob("vars.fd", &o, Some(&k)).is_err());
        // 같은 내용의 청크와 nonce가 다르다
        let c = encode(&raw, Some(&k));
        assert_ne!(
            c.data[HEADER_BYTES..HEADER_BYTES + 12],
            o[HEADER_BYTES..HEADER_BYTES + 12]
        );
        let p = encode_blob("config", &raw, None);
        assert_eq!(decode_blob("config", &p, None).unwrap(), raw);
        assert_eq!(
            decode_blob("x", &[], None).unwrap_err().to_string(),
            "not a chunk object"
        );
    }
}
