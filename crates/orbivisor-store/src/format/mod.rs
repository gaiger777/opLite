//! 저장소 형식 v1(설계 §7): 청크 객체, 고정 인덱스, 매니페스트 v3, `store.json`·`owner.json`, 객체 배치.

pub mod chunk;
pub mod index;
pub mod manifest;

use orbivisor_types::backup_store::ChunkId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FormatError {
    #[error("{0}")]
    Corrupt(String),
    #[error("the key does not match the store (fingerprint {found}, expected {expected})")]
    KeyMismatch { expected: String, found: String },
}

/// `store.json`의 `format`
pub const STORE_FORMAT: &str = "orbivisor-backup-store";
/// 저장소 형식 버전
pub const STORE_VERSION: u32 = 1;
pub const STORE_FILE: &str = "store.json";
pub const OWNER_FILE: &str = "owner.json";

/// `store.json` — 형식·버전·저장소 id·청크 크기·압축·암호화 지문(만든 뒤 바뀌지 않는다)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreInfo {
    pub format: String,
    pub version: u32,
    pub id: Uuid,
    pub chunk_bytes: u32,
    /// 늘 `lz4`
    pub compression: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<manifest::ManifestEncryption>,
    pub created_at: u64,
}

impl StoreInfo {
    pub fn new(id: Uuid, fingerprint: Option<&str>, created_at: u64) -> StoreInfo {
        StoreInfo {
            format: STORE_FORMAT.into(),
            version: STORE_VERSION,
            id,
            chunk_bytes: orbivisor_types::backup_store::CHUNK_BYTES as u32,
            compression: "lz4".into(),
            encryption: fingerprint.map(|f| manifest::ManifestEncryption {
                fingerprint: f.into(),
            }),
            created_at,
        }
    }

    pub fn fingerprint(&self) -> Option<&str> {
        self.encryption.as_ref().map(|e| e.fingerprint.as_str())
    }

    pub fn from_bytes(b: &[u8]) -> Result<StoreInfo, FormatError> {
        let s: StoreInfo = serde_json::from_slice(b)
            .map_err(|e| FormatError::Corrupt(format!("store.json: {e}")))?;
        if s.format != STORE_FORMAT {
            return Err(FormatError::Corrupt(
                "store.json: not an Orbivisor backup store".into(),
            ));
        }
        if s.version != STORE_VERSION {
            return Err(FormatError::Corrupt(format!(
                "store.json: unsupported store version {}",
                s.version
            )));
        }
        if s.chunk_bytes as u64 != orbivisor_types::backup_store::CHUNK_BYTES
            || s.compression != "lz4"
        {
            return Err(FormatError::Corrupt(
                "store.json: unsupported chunk size or compression".into(),
            ));
        }
        Ok(s)
    }
}

/// `owner.json` — 주인 클러스터(설계 §12.6)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerFile {
    pub cluster: String,
    pub ca_fingerprint: String,
    /// 주인 클러스터의 `BackupStore` 리소스 id
    pub store_resource: Uuid,
    /// 마지막 하트비트(유닉스 초 — 쓴 노드의 시계)
    pub heartbeat: u64,
}

/// 청크 객체 이름 `chunks/<앞 4자>/<64자>`
pub fn chunk_key(id: &ChunkId) -> String {
    let h = hex::encode(id);
    format!("chunks/{}/{h}", &h[..4])
}

/// 청크 객체 이름에서 id. 청크가 아닌 이름(임시 파일 따위)은 `None`.
pub fn parse_chunk_key(key: &str) -> Option<ChunkId> {
    let rest = key.strip_prefix("chunks/")?;
    let (dir, name) = rest.split_once('/')?;
    if dir.len() != 4 || name.len() != 64 || !name.starts_with(dir) {
        return None;
    }
    if !name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex::decode(name).ok()?.try_into().ok()
}

/// 스냅샷 디렉터리 `snapshots/<vm>/<backup>/`
pub fn snapshot_prefix(vm: Uuid, backup: Uuid) -> String {
    format!("snapshots/{vm}/{backup}/")
}

/// `snapshots/<vm>/<backup>/<name>`에서 (vm, backup, name)
pub fn parse_snapshot_key(key: &str) -> Option<(Uuid, Uuid, &str)> {
    let rest = key.strip_prefix("snapshots/")?;
    let mut it = rest.splitn(3, '/');
    let vm = Uuid::parse_str(it.next()?).ok()?;
    let backup = Uuid::parse_str(it.next()?).ok()?;
    let name = it.next()?;
    (!name.is_empty() && !name.contains('/')).then_some((vm, backup, name))
}

/// 깨진 청크를 옮겨 둘 이름 `bad/<id>.<유닉스 초>`
pub fn bad_key(id: &ChunkId, at: u64) -> String {
    format!("bad/{}.{at}", hex::encode(id))
}

/// `bad/<id hex>.<옮긴 시각>`의 청크 id
pub fn parse_bad_key(key: &str) -> Option<ChunkId> {
    let rest = key.strip_prefix("bad/")?;
    let (name, _) = rest.split_once('.')?;
    if name.len() != 64 || !name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex::decode(name).ok()?.try_into().ok()
}

/// GC 기준 표식 `gc/<run>.start`
pub fn gc_marker_key(run: Uuid) -> String {
    format!("gc/{run}.start")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let id = [0xab; 32];
        let k = chunk_key(&id);
        assert_eq!(k, format!("chunks/abab/{}", "ab".repeat(32)));
        assert_eq!(parse_chunk_key(&k), Some(id));
        assert_eq!(parse_chunk_key(&format!("{k}.tmp.123")), None);
        assert_eq!(parse_chunk_key("chunks/abcd/abcd"), None);
        assert_eq!(
            parse_chunk_key(&format!("chunks/0000/{}", "ab".repeat(32))),
            None
        );
        assert_eq!(
            parse_chunk_key(&format!("chunks/ABAB/{}", "AB".repeat(32))),
            None
        );
        let (vm, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let p = snapshot_prefix(vm, b);
        assert_eq!(
            parse_snapshot_key(&format!("{p}disk0.fidx")),
            Some((vm, b, "disk0.fidx"))
        );
        assert_eq!(parse_snapshot_key(&format!("{p}a/b")), None);
        assert_eq!(parse_snapshot_key("snapshots/x/y/z"), None);
        assert_eq!(bad_key(&[1; 32], 5), format!("bad/{}.5", "01".repeat(32)));
        assert_eq!(parse_bad_key(&bad_key(&[0xab; 32], 7)), Some([0xab; 32]));
        assert_eq!(parse_bad_key("bad/xyz.1"), None);
        assert_eq!(parse_bad_key(&chunk_key(&[1; 32])), None);
    }

    #[test]
    fn store_info_roundtrip() {
        let s = StoreInfo::new(Uuid::from_u128(9), Some("ab:cd"), 10);
        let b = serde_json::to_vec(&s).unwrap();
        assert_eq!(StoreInfo::from_bytes(&b).unwrap(), s);
        assert_eq!(s.fingerprint(), Some("ab:cd"));
        let p = StoreInfo::new(Uuid::from_u128(9), None, 10);
        assert!(
            !String::from_utf8(serde_json::to_vec(&p).unwrap())
                .unwrap()
                .contains("encryption")
        );
        let mut bad = s.clone();
        bad.version = 2;
        assert!(StoreInfo::from_bytes(&serde_json::to_vec(&bad).unwrap()).is_err());
        assert!(StoreInfo::from_bytes(b"{}").is_err());
    }
}
