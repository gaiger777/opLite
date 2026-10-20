use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::Resource;
use crate::runtime::ImageFormat;

pub type StoragePool = Resource<StoragePoolSpec, StoragePoolStatus>;
pub type Volume = Resource<VolumeSpec, VolumeStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoragePoolSpec {
    pub backend: PoolBackend,
    /// 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드.
    #[serde(default)]
    pub nodes: Vec<Uuid>,
    /// 모든 노드가 같은 데이터를 보는 공유 스토리지인지. 라이브 마이그레이션에서 쓴다.
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum PoolBackend {
    /// 로컬 디렉터리에 이미지 파일을 둔다.
    Dir { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StoragePoolStatus {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    /// StoragePool 리소스 이름
    pub pool: String,
    pub size_bytes: u64,
    pub format: ImageFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct VolumeStatus {
    /// 실제로 디스크에 할당된 크기
    pub allocated_bytes: Option<u64>,
    pub attached_to: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_pool_deserializes_with_defaults() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "dir", "path": "/var/lib/opl/pools/local" }
        }))
        .unwrap();
        assert_eq!(
            spec.backend,
            PoolBackend::Dir {
                path: "/var/lib/opl/pools/local".into()
            }
        );
        assert!(spec.nodes.is_empty());
        assert!(!spec.shared);
    }

    #[test]
    fn rejects_unknown_backend_fields() {
        let res = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "dir", "path": "/p", "pth": "/typo" }
        }));
        assert!(res.is_err());
    }

    #[test]
    fn volume_spec_round_trips() {
        let spec = VolumeSpec {
            pool: "local".into(),
            size_bytes: 10 << 30,
            format: ImageFormat::Qcow2,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["format"], "qcow2");
        assert_eq!(serde_json::from_value::<VolumeSpec>(json).unwrap(), spec);
    }

    #[test]
    fn statuses_default_missing_fields() {
        let p: StoragePoolStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(p, StoragePoolStatus::default());
        let v: VolumeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(v, VolumeStatus::default());
    }
}
