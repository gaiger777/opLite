//! VM 스냅샷. 디스크(qcow2 내부 스냅샷), 실행 중이었으면 메모리, VM 스펙을 함께 저장한다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::{RunningHardware, VmSpec};

/// 리소스 이름은 id 문자열이다(클러스터 전체에서 고유). 사용자 이름은 `spec.name`이고 VM 안에서 고유하다.
/// 디스크 안의 qcow2 태그도 id 문자열이다.
pub type Snapshot = Resource<SnapshotSpec, SnapshotStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SnapshotSpec {
    /// VM id
    pub vm: Uuid,
    /// 사용자가 붙인 이름. VM 안에서 고유하다.
    pub name: String,
    /// 부모 스냅샷 id. VM의 첫 스냅샷이면 없다.
    #[serde(default)]
    pub parent: Option<Uuid>,
    /// 메모리 상태를 담았는지. 담았으면 VM의 첫 디스크에 있다.
    pub memory: bool,
    #[serde(default)]
    pub description: String,
    /// 예약으로 만들었는지. 보관 개수 정리는 이것만 지운다.
    #[serde(default)]
    pub scheduled: bool,
    /// 만든 순간의 VM 스펙
    pub vm_spec: VmSpec,
    /// 메모리 스냅샷이면 그때 도는 QEMU의 하드웨어(계획 54, 모든 노드가 `hotplug`일 때만). 메모리를 불러오려면 같은
    /// 장치로 켜야 하므로 되돌리기는 이 모양(핫플러그 배치 있음·없음, 상한)으로 켠다. 없으면 배치 없이 켠다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub hardware: Option<RunningHardware>,
}

impl ResourceSpec for SnapshotSpec {
    const KIND: Kind = Kind::Snapshot;
    type Status = SnapshotStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct SnapshotStatus {
    pub phase: SnapshotPhase,
    /// 만든 시각(유닉스 초)
    pub created_at: Option<u64>,
    /// 메모리 상태 크기. 메모리 스냅샷만 있다.
    pub vmstate_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotPhase {
    /// 만드는 중
    #[default]
    Pending,
    Ready,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm_spec() -> serde_json::Value {
        serde_json::json!({
            "cpu": { "cores": 1 },
            "memory_mib": 512,
            "machine": "virt",
            "disks": [ { "volume": "web-root", "bus": "virtio-blk" } ]
        })
    }

    #[test]
    fn spec_deserializes_with_defaults_and_round_trips() {
        let vm = Uuid::now_v7();
        let spec: SnapshotSpec = serde_json::from_value(serde_json::json!({
            "vm": vm,
            "name": "before-upgrade",
            "memory": true,
            "vm_spec": vm_spec()
        }))
        .unwrap();
        assert_eq!(spec.vm, vm);
        assert_eq!(spec.parent, None);
        assert_eq!(spec.description, "");
        assert!(!spec.scheduled);
        assert_eq!(spec.vm_spec.disks[0].volume, "web-root");

        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(serde_json::from_value::<SnapshotSpec>(json).unwrap(), spec);
    }

    /// 하드웨어 기록이 없으면 옛 모양 그대로 쓰고, 있으면 왕복한다.
    #[test]
    fn hardware_is_written_only_when_recorded() {
        let mut spec: SnapshotSpec = serde_json::from_value(serde_json::json!({
            "vm": Uuid::now_v7(),
            "name": "a",
            "memory": true,
            "vm_spec": vm_spec()
        }))
        .unwrap();
        assert_eq!(spec.hardware, None);
        assert!(
            serde_json::to_value(&spec)
                .unwrap()
                .get("hardware")
                .is_none()
        );

        let (hw, _) = RunningHardware::at_start(
            uuid::Uuid::nil(),
            "web",
            &spec.vm_spec,
            Default::default(),
            true,
        );
        spec.hardware = Some(hw);
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["hardware"]["layout"]["ports"], 8);
        assert_eq!(serde_json::from_value::<SnapshotSpec>(json).unwrap(), spec);
    }

    #[test]
    fn spec_rejects_unknown_fields() {
        let res = serde_json::from_value::<SnapshotSpec>(serde_json::json!({
            "vm": Uuid::now_v7(),
            "name": "a",
            "memory": false,
            "vm_spec": vm_spec(),
            "memroy": true
        }));
        assert!(res.is_err());
    }

    #[test]
    fn status_defaults_to_pending_and_serializes_phase() {
        let st: SnapshotStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(st, SnapshotStatus::default());
        assert_eq!(st.phase, SnapshotPhase::Pending);

        let ready = SnapshotStatus {
            phase: SnapshotPhase::Ready,
            created_at: Some(1_790_000_000),
            vmstate_bytes: Some(1 << 20),
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({
                "phase": "ready",
                "created_at": 1_790_000_000u64,
                "vmstate_bytes": 1u64 << 20
            })
        );
    }
}
