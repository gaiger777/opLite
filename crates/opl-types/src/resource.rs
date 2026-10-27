use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

/// 클러스터 저장소가 구분하는 리소스 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Vm,
    StoragePool,
    Volume,
    Node,
    Task,
    User,
    ApiToken,
    Cluster,
    JoinToken,
    Network,
    Snapshot,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Vm => "vm",
            Kind::StoragePool => "storage-pool",
            Kind::Volume => "volume",
            Kind::Node => "node",
            Kind::Task => "task",
            Kind::User => "user",
            Kind::ApiToken => "api-token",
            Kind::Cluster => "cluster",
            Kind::JoinToken => "join-token",
            Kind::Network => "network",
            Kind::Snapshot => "snapshot",
        }
    }
}

/// 리소스 이름 규칙: DNS 레이블처럼 1~63자의 영문 소문자, 숫자, `-`이고 `-`로 시작하거나 끝나지 않는다.
/// 저장소는 이름의 유일성만 보므로 API가 생성할 때 확인한다.
pub fn validate_name(name: &str) -> Result<(), NameError> {
    let ok = (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(NameError(name.to_string()))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "invalid name {0:?}: use 1-63 lowercase letters, digits and '-', not starting or ending with '-'"
)]
pub struct NameError(String);

/// spec 타입과 리소스 종류, status 타입을 묶는다. 클러스터 저장소의 타입 있는 API가 쓴다.
pub trait ResourceSpec: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    const KIND: Kind;
    type Status: Serialize + DeserializeOwned + Clone + Default + Send + Sync + 'static;
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
    fn kind_serializes_as_its_str() {
        for kind in [
            Kind::Vm,
            Kind::StoragePool,
            Kind::Volume,
            Kind::Node,
            Kind::Task,
            Kind::User,
            Kind::ApiToken,
            Kind::Cluster,
            Kind::JoinToken,
            Kind::Network,
            Kind::Snapshot,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }

    #[test]
    fn validates_names() {
        for ok in ["a", "web-01", "0", &"a".repeat(63)] {
            assert_eq!(validate_name(ok), Ok(()), "{ok}");
        }
        for bad in ["", "-a", "a-", "Web", "a_b", "a.b", "a b", &"a".repeat(64)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
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
