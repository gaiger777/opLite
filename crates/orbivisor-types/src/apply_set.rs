//! 적용 묶음(`apply-set`, 계획 73 §6): `orbivisorctl apply --set`이 마지막에 선언한 것을 기억해 세 갈래 비교·가지치기·
//! 드리프트의 기준이 된다. 선언은 status에 둔다(객체 주석·칸 단위 관리자 없이 — §15).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::history::HistoryRef;
use crate::resource::{Kind, Resource, ResourceSpec};

/// 묶음 하나가 기억하는 객체 수 한도
pub const MAX_APPLY_SET_OBJECTS: usize = 1000;
/// 묶음 하나의 `declared` 합(JSON 바이트) 한도
pub const MAX_DECLARED_BYTES: usize = 4 * 1024 * 1024;

pub type ApplySet = Resource<ApplySetSpec, ApplySetStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ApplySetSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub description: Option<String>,
    /// 참이면 가지치기에서 VM·볼륨·백업 저장소를 `--prune-data` 없이는 지우지 않는다. 기본은 참이고 그때는 쓰지 않는다.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub protect_data: bool,
}

impl Default for ApplySetSpec {
    fn default() -> Self {
        Self {
            description: None,
            protect_data: true,
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl ResourceSpec for ApplySetSpec {
    const KIND: Kind = Kind::ApplySet;
    type Status = ApplySetStatus;
}

/// 묶음의 마지막 적용과 드리프트
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct ApplySetStatus {
    /// 마지막 적용을 마친 때(유닉스 밀리초)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub applied_at: Option<u64>,
    /// 마지막으로 적용한 사용자 이름
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub applied_by: Option<String>,
    /// 마지막 적용이 끝난 때의 클러스터 revision(드리프트의 "누가 바꿨나"는 이 뒤 이력에서 찾는다)
    #[serde(skip_serializing_if = "is_zero")]
    pub applied_revision: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<AppliedObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub drift: Option<DriftReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub last_error: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// 묶음이 선언한 객체 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AppliedObject {
    pub kind: Kind,
    pub id: Uuid,
    pub name: String,
    /// 정규화한 선언 칸만(비밀은 지문 없이 `"$secret"`)
    pub declared: Value,
    /// 적용 뒤 객체의 revision
    pub revision: u64,
}

/// 선언과 실제의 차이
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DriftReport {
    /// 견준 때(유닉스 밀리초)
    pub checked_at: u64,
    /// 견준 클러스터 revision
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<DriftItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DriftItem {
    pub kind: Kind,
    pub id: Uuid,
    pub name: String,
    pub state: DriftState,
    /// 다른 칸(JSON 경로)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// 그 칸을 처음 바꾼 이력 줄
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub by: Option<HistoryRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum DriftState {
    /// 선언한 칸이 다름
    Modified,
    /// 밖에서 지움
    Missing,
    /// 묶음 레이블이 떨어짐
    Unmanaged,
}

/// 묶음이 한도를 넘음(400 `apply_set_too_large`)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplySetTooLarge {
    #[error("an apply set holds at most {MAX_APPLY_SET_OBJECTS} objects, not {0}; split the set")]
    Objects(usize),
    #[error("an apply set declares at most {MAX_DECLARED_BYTES} bytes, not {0}; split the set")]
    Bytes(usize),
}

impl ApplySetStatus {
    /// 객체 수와 선언 크기 한도
    pub fn check_limits(&self) -> Result<(), ApplySetTooLarge> {
        if self.objects.len() > MAX_APPLY_SET_OBJECTS {
            return Err(ApplySetTooLarge::Objects(self.objects.len()));
        }
        let bytes: usize = self
            .objects
            .iter()
            .map(|o| serde_json::to_vec(&o.declared).map_or(0, |v| v.len()))
            .sum();
        if bytes > MAX_DECLARED_BYTES {
            return Err(ApplySetTooLarge::Bytes(bytes));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_values_are_not_written() {
        let set: ApplySet = Resource::new("web-prod", ApplySetSpec::default());
        let v = serde_json::to_value(&set).unwrap();
        assert_eq!(v["spec"], json!({}));
        assert_eq!(v["status"], json!({}));
        let back: ApplySet = serde_json::from_value(v).unwrap();
        assert!(back.spec.protect_data);
        let spec: ApplySetSpec = serde_json::from_value(json!({ "protect_data": false })).unwrap();
        assert!(!spec.protect_data);
        assert!(serde_json::from_value::<ApplySetSpec>(json!({ "x": 1 })).is_err());
        assert_eq!(Kind::ApplySet.as_str(), "apply-set");
    }

    #[test]
    fn drift_round_trips() {
        let status = ApplySetStatus {
            applied_at: Some(1),
            applied_by: Some("admin".into()),
            applied_revision: 10,
            objects: vec![AppliedObject {
                kind: Kind::Vm,
                id: Uuid::from_u128(1),
                name: "web01".into(),
                declared: json!({ "vcpus": 4 }),
                revision: 9,
            }],
            drift: Some(DriftReport {
                checked_at: 2,
                revision: 12,
                items: vec![DriftItem {
                    kind: Kind::Vm,
                    id: Uuid::from_u128(1),
                    name: "web01".into(),
                    state: DriftState::Modified,
                    paths: vec!["/vcpus".into()],
                    by: Some(HistoryRef {
                        revision: 11,
                        at_ms: 3,
                        actor: crate::history::Actor::system(),
                        source: crate::history::ChangeSource::Web,
                        reason: None,
                    }),
                }],
            }),
            last_error: None,
        };
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(v["drift"]["items"][0]["state"], "modified");
        assert_eq!(serde_json::from_value::<ApplySetStatus>(v).unwrap(), status);
        assert_eq!(status.check_limits(), Ok(()));
    }

    #[test]
    fn limits() {
        let obj = |declared: Value| AppliedObject {
            kind: Kind::Network,
            id: Uuid::nil(),
            name: "n".into(),
            declared,
            revision: 1,
        };
        let many = ApplySetStatus {
            objects: (0..=MAX_APPLY_SET_OBJECTS)
                .map(|_| obj(json!({})))
                .collect(),
            ..ApplySetStatus::default()
        };
        assert_eq!(
            many.check_limits(),
            Err(ApplySetTooLarge::Objects(MAX_APPLY_SET_OBJECTS + 1))
        );
        let big = ApplySetStatus {
            objects: vec![obj(json!("x".repeat(MAX_DECLARED_BYTES)))],
            ..ApplySetStatus::default()
        };
        assert!(matches!(
            big.check_limits(),
            Err(ApplySetTooLarge::Bytes(_))
        ));
    }
}
