use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource<S, T> {
    pub id: Uuid,
    pub name: String,
    /// 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
    pub revision: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub spec: S,
    pub status: T,
}

impl<S, T: Default> Resource<S, T> {
    pub fn new(name: impl Into<String>, spec: S) -> Self {
        Self {
            id: Uuid::now_v7(),
            name: name.into(),
            revision: 0,
            labels: BTreeMap::new(),
            spec,
            status: T::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
    struct DummyStatus {
        ready: bool,
    }

    #[test]
    fn new_resource_has_v7_id_zero_revision_and_default_status() {
        let r: Resource<u32, DummyStatus> = Resource::new("web-01", 7);
        assert_eq!(r.id.get_version_num(), 7);
        assert_eq!(r.name, "web-01");
        assert_eq!(r.revision, 0);
        assert!(r.labels.is_empty());
        assert_eq!(r.spec, 7);
        assert_eq!(r.status, DummyStatus::default());
    }

    #[test]
    fn empty_labels_are_omitted_and_round_trip() {
        let r: Resource<u32, DummyStatus> = Resource::new("a", 1);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("labels").is_none());
        let back: Resource<u32, DummyStatus> = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }
}
