use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::storage::SourceFormat;

/// 오래 걸리는 작업. API가 만들고 `node`의 작업 실행기가 실행해 status를 기록한다.
/// 리소스 이름은 id 문자열이다.
pub type Task = Resource<TaskSpec, TaskStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷 Task는 VM이다.
    pub target: Uuid,
    /// 작업을 실행할 노드
    pub node: Uuid,
}

impl ResourceSpec for TaskSpec {
    const KIND: Kind = Kind::Task;
    type Status = TaskStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum TaskOp {
    VmStart,
    VmStop,
    /// ACPI 없이 곧바로 끈다.
    VmForceStop,
    /// ACPI로 끈 뒤 다시 켠다.
    VmReboot,
    VolumeCreate,
    /// `source`(작업 노드의 로컬 파일)를 변환해 볼륨을 채우고, 끝나면 `source`를 지운다.
    VolumeImport {
        #[cfg_attr(feature = "openapi", schema(value_type = String))]
        source: PathBuf,
        source_format: SourceFormat,
    },
    VolumeDelete,
    SnapshotCreate {
        snapshot: Uuid,
    },
    /// 디스크·UEFI 변수·VM 스펙을 스냅샷 것으로 되돌린다.
    SnapshotRevert {
        snapshot: Uuid,
    },
    SnapshotDelete {
        snapshot: Uuid,
    },
    /// 볼륨 크기를 늘린다(줄이지 않는다).
    VolumeResize {
        size_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct TaskStatus {
    pub state: TaskState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl TaskState {
    pub fn is_finished(self) -> bool {
        matches!(self, TaskState::Succeeded | TaskState::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_is_tagged_by_kind() {
        let op = TaskOp::VolumeImport {
            source: "/var/lib/opl/uploads/x".into(),
            source_format: SourceFormat::Vmdk,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "volume-import",
                "source": "/var/lib/opl/uploads/x",
                "source_format": "vmdk"
            })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        assert_eq!(
            serde_json::to_value(TaskOp::VmForceStop).unwrap(),
            serde_json::json!({ "kind": "vm-force-stop" })
        );
    }

    #[test]
    fn snapshot_and_resize_ops_are_tagged_by_kind() {
        let snapshot = Uuid::now_v7();
        for (op, kind) in [
            (TaskOp::SnapshotCreate { snapshot }, "snapshot-create"),
            (TaskOp::SnapshotRevert { snapshot }, "snapshot-revert"),
            (TaskOp::SnapshotDelete { snapshot }, "snapshot-delete"),
        ] {
            let json = serde_json::to_value(&op).unwrap();
            assert_eq!(
                json,
                serde_json::json!({ "kind": kind, "snapshot": snapshot })
            );
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        }
        let op = TaskOp::VolumeResize {
            size_bytes: 20 << 30,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "volume-resize", "size_bytes": 20u64 << 30 })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn new_task_is_pending() {
        assert_eq!(TaskStatus::default().state, TaskState::Pending);
        assert!(!TaskState::Running.is_finished());
        assert!(TaskState::Failed.is_finished());
    }
}
