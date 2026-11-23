//! 매니페스트 v3 `index.json`과 `config` 블롭(설계 §7.4). 스냅샷은 매니페스트가 있을 때만 완전하다(커밋 지점).
//! 암호화 저장소는 `auth` = HMAC-SHA256(auth_key, `auth`를 뺀 정규 JSON)으로 인증한다. 정규 JSON은 키를 정렬한
//! 빈칸 없는 JSON이고, 모르는 칸(계획 69의 `catalog` 따위)도 그대로 들어간다 — 읽는 쪽은 모르는 칸을 무시하되
//! 인증에서는 뺄 수 없다.

use opl_types::vm::VmSpec;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::FormatError;
use crate::crypto::StoreKeys;

pub const VERSION: u32 = 3;
/// 매니페스트의 객체 이름(스냅샷 디렉터리 안)
pub const MANIFEST_NAME: &str = "index.json";
/// VM 이름·스펙·펌웨어 표시를 담은 블롭
pub const CONFIG_BLOB: &str = "config";
/// UEFI 변수 블롭
pub const VARS_BLOB: &str = "vars.fd";
/// TPM 상태 블롭(swtpm의 TPM 2.0 영구 상태와 같은 이름)
pub const TPM_BLOB: &str = opl_types::backup::TPM_STATE_FILE;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// 저장소 id(`store.json`)
    pub store: Uuid,
    pub backup: Uuid,
    pub vm: Uuid,
    /// 만든 시각(유닉스 초)
    pub created_at: u64,
    pub disks: Vec<ManifestDisk>,
    pub blobs: Vec<ManifestBlob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<ManifestEncryption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// 이 판이 모르는 칸(그대로 두어 인증이 맞게)
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDisk {
    /// `disk{i}`
    pub name: String,
    pub virtual_bytes: u64,
    /// 인덱스 객체 이름(`disk{i}.fidx`)
    pub index: String,
    /// 인덱스 파일 전체의 SHA-256(16진)
    pub index_sha256: String,
    pub chunks: u64,
    pub zero_chunks: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestBlob {
    pub name: String,
    /// 저장된 블롭 객체의 SHA-256(16진 — 암호화 저장소면 암호문의 해시)
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEncryption {
    pub fingerprint: String,
}

/// `config` 블롭의 내용(매니페스트 v1·v2에 있던 VM 쪽 칸)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigBlob {
    pub vm_name: String,
    pub vm_spec: VmSpec,
    /// `vars.fd` 블롭이 있는가
    #[serde(default)]
    pub uefi_vars: bool,
    /// TPM 상태 블롭이 있는가
    #[serde(default)]
    pub tpm_state: bool,
}

/// 키를 정렬한 빈칸 없는 JSON
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string"));
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

impl Manifest {
    /// `auth`를 뺀 정규 JSON
    fn signed_bytes(&self) -> Vec<u8> {
        let mut v = serde_json::to_value(self).expect("manifest serializes");
        if let Value::Object(m) = &mut v {
            m.remove("auth");
        }
        canonical_json(&v).into_bytes()
    }

    /// 암호화 저장소면 지문과 `auth`를 채운다. 평문 저장소면 둘 다 비운다.
    pub fn seal(&mut self, keys: Option<&StoreKeys>) {
        self.auth = None;
        match keys {
            Some(k) => {
                self.encryption = Some(ManifestEncryption {
                    fingerprint: k.fingerprint().to_string(),
                });
                self.auth = Some(k.auth_tag(&self.signed_bytes()));
            }
            None => self.encryption = None,
        }
    }

    /// 저장소의 키로 확인한다. 암호화 저장소에서 `auth`가 없거나 틀리면, 평문 저장소에 암호화 매니페스트가 있으면 거절.
    pub fn verify(&self, keys: Option<&StoreKeys>) -> Result<(), FormatError> {
        let bad = |s: &str| FormatError::Corrupt(format!("manifest: {s}"));
        if self.version != VERSION {
            return Err(bad("unknown version"));
        }
        match keys {
            Some(k) => {
                let fp = self
                    .encryption
                    .as_ref()
                    .ok_or_else(|| bad("not encrypted in an encrypted store"))?;
                if fp.fingerprint != k.fingerprint() {
                    return Err(FormatError::KeyMismatch {
                        expected: fp.fingerprint.clone(),
                        found: k.fingerprint().to_string(),
                    });
                }
                let tag = self
                    .auth
                    .as_deref()
                    .ok_or_else(|| bad("authentication is missing"))?;
                if !k.auth_verify(&self.signed_bytes(), tag) {
                    return Err(bad("authentication failed"));
                }
            }
            None => {
                if self.encryption.is_some() || self.auth.is_some() {
                    return Err(bad("encrypted manifest in a plain store"));
                }
            }
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("manifest serializes")
    }

    /// JSON을 읽는다(인증은 [`Manifest::verify`]).
    pub fn from_bytes(b: &[u8]) -> Result<Manifest, FormatError> {
        let m: Manifest = serde_json::from_slice(b)
            .map_err(|e| FormatError::Corrupt(format!("manifest: {e}")))?;
        if m.version != VERSION {
            return Err(FormatError::Corrupt("manifest: unknown version".into()));
        }
        Ok(m)
    }

    pub fn blob(&self, name: &str) -> Option<&ManifestBlob> {
        self.blobs.iter().find(|b| b.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::MasterKey;

    pub(crate) fn sample() -> Manifest {
        Manifest {
            version: VERSION,
            store: Uuid::from_u128(1),
            backup: Uuid::from_u128(2),
            vm: Uuid::from_u128(3),
            created_at: 1_763_870_400,
            disks: vec![ManifestDisk {
                name: "disk0".into(),
                virtual_bytes: 21_474_836_480,
                index: "disk0.fidx".into(),
                index_sha256: "ab".repeat(32),
                chunks: 5120,
                zero_chunks: 3901,
            }],
            blobs: vec![ManifestBlob {
                name: "config".into(),
                sha256: "cd".repeat(32),
            }],
            encryption: None,
            auth: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn canonical_sorts_keys() {
        let v: Value =
            serde_json::from_str(r#"{"b":[{"z":1,"a":"x\"y"}],"a":null,"c":1.5}"#).unwrap();
        assert_eq!(
            canonical_json(&v),
            r#"{"a":null,"b":[{"a":"x\"y","z":1}],"c":1.5}"#
        );
    }

    #[test]
    fn plain_manifest() {
        let mut m = sample();
        m.seal(None);
        assert!(m.auth.is_none());
        m.verify(None).unwrap();
        let back = Manifest::from_bytes(&m.to_bytes()).unwrap();
        assert_eq!(back, m);
        let j = String::from_utf8(m.to_bytes()).unwrap();
        assert!(!j.contains("auth") && !j.contains("encryption"));
    }

    #[test]
    fn encrypted_manifest_auth() {
        let k = StoreKeys::new(&MasterKey::from_bytes([5; 32]));
        let mut m = sample();
        m.seal(Some(&k));
        m.verify(Some(&k)).unwrap();
        let back = Manifest::from_bytes(&m.to_bytes()).unwrap();
        back.verify(Some(&k)).unwrap();
        // 칸 하나 바꿈 → 거절
        let mut t = back.clone();
        t.disks[0].zero_chunks += 1;
        assert!(t.verify(Some(&k)).is_err());
        let mut t = back.clone();
        t.created_at += 1;
        assert!(t.verify(Some(&k)).is_err());
        let mut t = back.clone();
        t.auth = None;
        assert!(t.verify(Some(&k)).is_err());
        // 다른 키
        let k2 = StoreKeys::new(&MasterKey::from_bytes([6; 32]));
        assert!(matches!(
            back.verify(Some(&k2)),
            Err(FormatError::KeyMismatch { .. })
        ));
        // 평문 저장소로 읽으면 거절
        assert!(back.verify(None).is_err());
        // 평문 매니페스트를 암호화 저장소로
        let mut p = sample();
        p.seal(None);
        assert!(p.verify(Some(&k)).is_err());
    }

    #[test]
    fn unknown_fields_are_kept_and_authenticated() {
        let k = StoreKeys::new(&MasterKey::from_bytes([5; 32]));
        let mut v = serde_json::to_value(sample()).unwrap();
        v["catalog"] = serde_json::json!({"disk0": "catalog0"});
        let mut m: Manifest = serde_json::from_value(v).unwrap();
        m.seal(Some(&k));
        let back = Manifest::from_bytes(&m.to_bytes()).unwrap();
        assert!(back.extra.contains_key("catalog"));
        back.verify(Some(&k)).unwrap();
        let mut t = back.clone();
        t.extra.insert("catalog".into(), Value::Null);
        assert!(t.verify(Some(&k)).is_err());
    }

    #[test]
    fn version_must_be_3() {
        let mut v = serde_json::to_value(sample()).unwrap();
        v["version"] = 2.into();
        assert!(Manifest::from_bytes(&serde_json::to_vec(&v).unwrap()).is_err());
    }
}
