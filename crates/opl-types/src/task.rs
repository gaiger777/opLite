use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::storage::SourceFormat;
use crate::vm_import::ImportRun;

/// 오래 걸리는 작업. API가 만들고 `node`의 작업 실행기가 실행해 status를 기록한다.
/// 리소스 이름은 id 문자열이다.
pub type Task = Resource<TaskSpec, TaskStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제·백업 Task는 (원본) VM, 백업 지우기는 백업,
    /// 복원과 VM 가져오기는 새 VM, 가져오기 지우기는 가져오기다.
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
        /// 예약 정리가 만든 삭제면 true. 이 삭제의 실패만 `schedule_error`에 남긴다.
        #[serde(default)]
        scheduled: bool,
    },
    /// 볼륨 크기를 늘린다(줄이지 않는다).
    VolumeResize {
        size_bytes: u64,
    },
    /// 대상 VM을 `name`으로 복제한다. `snapshot`이 없으면 지금 상태(꺼진 VM)에서,
    /// `pool`이 없으면 원본 볼륨의 풀에 디스크를 복사한다.
    VmClone {
        name: String,
        snapshot: Option<Uuid>,
        pool: Option<String>,
    },
    /// 실행 중인 대상 VM을 `node`로 라이브 마이그레이션한다. 원본 노드(VM의 `spec.node`)에서 돈다.
    VmMigrate {
        node: Uuid,
    },
    /// 리더의 HA 복구 기록: 노드 `from`이 죽어 대상 VM을 Task의 `node`로 옮겼다. 처음부터 끝난
    /// 상태(Succeeded)로 만들고 실행기는 실행하지 않는다.
    VmHaRecover {
        from: Uuid,
    },
    /// 대상 VM을 백업 `backup`(`pending`으로 먼저 만든 Backup 리소스)에 담는다. VM의 노드에서 돈다.
    VmBackup {
        backup: Uuid,
    },
    /// 대상 백업의 파일과 리소스를 지운다.
    BackupDelete,
    /// 백업 `backup`을 대상 id(미리 정한 새 VM id)의 새 VM `name`으로 되살린다. 디스크는 `pool`에 만든다.
    BackupRestore {
        backup: Uuid,
        name: String,
        pool: String,
    },
    /// 가져오기 `import`를 `run`의 값으로 대상 id(미리 정한 새 VM id)의 새 VM으로 만든다. 가져오기의
    /// 노드(원본이 있는 노드)에서 돈다.
    VmImport {
        import: Uuid,
        run: ImportRun,
    },
    /// 대상 가져오기의 원본 디렉터리와 리소스를 지운다.
    VmImportDelete,
    /// 대상 노드(Task의 `node`와 같다)의 APT 업데이트를 확인해 Node `status.updates`에 쓴다.
    NodeUpdateCheck,
    /// 대상 노드(Task의 `node`와 같다)의 패키지를 `apt-get dist-upgrade`로 올린다. opld 밖의 일시
    /// systemd 유닛에서 돌므로 opld가 다시 시작해도 이어서 지켜본다.
    NodeUpgrade,
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
        ] {
            let json = serde_json::to_value(&op).unwrap();
            assert_eq!(
                json,
                serde_json::json!({ "kind": kind, "snapshot": snapshot })
            );
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        }
        let op = TaskOp::SnapshotDelete {
            snapshot,
            scheduled: true,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "snapshot-delete", "snapshot": snapshot, "scheduled": true })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        // 옛 Task에는 scheduled가 없다. 사용자 삭제로 읽는다.
        assert_eq!(
            serde_json::from_value::<TaskOp>(
                serde_json::json!({ "kind": "snapshot-delete", "snapshot": snapshot })
            )
            .unwrap(),
            TaskOp::SnapshotDelete {
                snapshot,
                scheduled: false
            }
        );
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
    fn clone_op_is_tagged_by_kind() {
        let snapshot = Uuid::now_v7();
        let op = TaskOp::VmClone {
            name: "web-02".into(),
            snapshot: Some(snapshot),
            pool: Some("fast".into()),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "vm-clone",
                "name": "web-02",
                "snapshot": snapshot,
                "pool": "fast"
            })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        let op = TaskOp::VmClone {
            name: "web-02".into(),
            snapshot: None,
            pool: None,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn migrate_op_is_tagged_by_kind() {
        let node = Uuid::now_v7();
        let op = TaskOp::VmMigrate { node };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "vm-migrate", "node": node })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn ha_recover_op_is_tagged_by_kind() {
        let from = Uuid::now_v7();
        let op = TaskOp::VmHaRecover { from };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "vm-ha-recover", "from": from })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn backup_ops_are_tagged_by_kind() {
        let backup = Uuid::now_v7();
        for (op, json) in [
            (
                TaskOp::VmBackup { backup },
                serde_json::json!({ "kind": "vm-backup", "backup": backup }),
            ),
            (
                TaskOp::BackupDelete,
                serde_json::json!({ "kind": "backup-delete" }),
            ),
            (
                TaskOp::BackupRestore {
                    backup,
                    name: "web-2".into(),
                    pool: "local".into(),
                },
                serde_json::json!({
                    "kind": "backup-restore",
                    "backup": backup,
                    "name": "web-2",
                    "pool": "local"
                }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&op).unwrap(), json);
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        }
    }

    #[test]
    fn vm_import_ops_are_tagged_by_kind() {
        let import = Uuid::now_v7();
        let run: ImportRun = serde_json::from_value(serde_json::json!({
            "name": "web01",
            "pool": "local",
            "networks": { "VM Network": "lan" }
        }))
        .unwrap();
        for (op, json) in [
            (
                TaskOp::VmImport { import, run },
                serde_json::json!({
                    "kind": "vm-import",
                    "import": import,
                    "run": {
                        "name": "web01",
                        "pool": "local",
                        "networks": { "VM Network": "lan" },
                        "disk_bus": null,
                        "nic_model": null,
                        "firmware": null,
                        "sockets": null,
                        "cores": null,
                        "memory_mib": null,
                        "keep_macs": true
                    }
                }),
            ),
            (
                TaskOp::VmImportDelete,
                serde_json::json!({ "kind": "vm-import-delete" }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&op).unwrap(), json);
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        }
    }

    #[test]
    fn node_update_check_op_is_tagged_by_kind() {
        let json = serde_json::json!({ "kind": "node-update-check" });
        assert_eq!(serde_json::to_value(TaskOp::NodeUpdateCheck).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<TaskOp>(json).unwrap(),
            TaskOp::NodeUpdateCheck
        );
    }

    #[test]
    fn node_upgrade_op_is_tagged_by_kind() {
        let json = serde_json::json!({ "kind": "node-upgrade" });
        assert_eq!(serde_json::to_value(TaskOp::NodeUpgrade).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<TaskOp>(json).unwrap(),
            TaskOp::NodeUpgrade
        );
    }

    #[test]
    fn new_task_is_pending() {
        assert_eq!(TaskStatus::default().state, TaskState::Pending);
        assert!(!TaskState::Running.is_finished());
        assert!(TaskState::Failed.is_finished());
    }
}
