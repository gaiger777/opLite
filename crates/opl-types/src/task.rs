use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fence::FenceKind;
use crate::ha_check::HaCheckAction;
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::storage::SourceFormat;
use crate::template::{CloneOverrides, TemplateDraft};
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
    /// `url`에서 받아(`sha256`이 있으면 확인) 볼륨을 채운다(계획 57). 받은 파일은 작업 노드의 업로드 디렉터리에
    /// 두었다가 가져오기처럼 변환하고 지운다.
    VolumeDownload {
        url: String,
        /// 받은 파일의 SHA-256(소문자 16진 64자). 없으면 확인하지 않는다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        sha256: Option<String>,
        source_format: SourceFormat,
        /// 받은 파일의 SHA-512(소문자 16진 128자, 계획 58). `sha256`·`checksum_url`과 함께 쓰지 않는다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        sha512: Option<String>,
        /// 체크섬 파일 URL(계획 58). 받기 전에 이 파일에서 이미지 파일 이름의 해시를 찾아 확인한다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        checksum_url: Option<String>,
        /// 가져오기가 끝나면 이 볼륨으로 만들 템플릿 VM(계획 58)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        template: Option<Box<TemplateDraft>>,
    },
    VolumeDelete,
    /// 연결 복제 볼륨(대상)을 기반과 떼어 독립 볼륨으로 만들고 `base`를 지운다(계획 58). 볼륨을 쓰는 VM은 꺼져 있다.
    VolumeFlatten,
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
        /// 연결 복제(계획 58, 템플릿에서만): 없음 = 되는 디스크는 연결하고 안 되는 디스크는 전체 복사, 참 = 모두
        /// 연결(안 되면 실패), 거짓 = 전체 복사. 없으면 쓰지 않는다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        linked: Option<bool>,
        /// 배포 때 바꾸는 값(계획 58). 없으면 쓰지 않는다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        overrides: Option<Box<CloneOverrides>>,
    },
    /// 실행 중인 대상 VM을 `node`로 라이브 마이그레이션한다. 원본 노드(VM의 `spec.node`)에서 돈다.
    /// 노드 로컬 풀의 디스크는 `node`에 새로 만들어 복사한다.
    VmMigrate {
        node: Uuid,
        /// 로컬 디스크를 둘 `node`의 풀. 없으면 디스크마다 같은 이름의 풀이다. 없으면 빼고 쓰므로
        /// 옛 노드도 읽는다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        pool: Option<String>,
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
        /// 새 VM의 폴더(계획 62). API가 정한다: 요청의 폴더, 없으면 백업한 VM의 폴더(아직 있으면). 없으면 루트이고
        /// 쓰지 않아 옛 노드도 푼다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        folder: Option<Uuid>,
    },
    /// 대상 백업(`ready`)을 원격 `remote`의 백업 풀 `pool`에 사본으로 보낸다. 백업 풀을 읽을 수 있는
    /// 노드에서 돈다.
    BackupReplicate {
        remote: String,
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
    /// 대상 노드(Task의 `node`와 같다)의 패키지를 `apt-get dist-upgrade`로 올린다. orbivisord 밖의 일시
    /// systemd 유닛에서 돌므로 orbivisord가 다시 시작해도 이어서 지켜본다.
    NodeUpgrade,
    /// 대상 노드(Task의 `node`와 같다)를 비운다: 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션한다.
    /// 노드는 API가 먼저 유지보수로 바꾼다.
    NodeDrain,
    /// 리더의 전원 끄기 펜싱 기록: 대상 노드(Task의 `node`와 같다)를 `reason` 때문에 껐다(`succeeded`, 끈 방법
    /// `method`) 또는 끄지 못했다(`failed`, 방법마다의 이유는 status `error`). 끝난 상태로 만들고 실행기는
    /// 실행하지 않는다.
    NodePowerOff {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        method: Option<FenceKind>,
        reason: String,
    },
    /// 리더의 HA 검사 기록: 대상 노드(Task의 `node`와 같다)에서 검사 `check`가 실패해 `action`대로 대응했다
    /// (`succeeded`) 또는 대응하지 못했다(`failed`). `vms`는 대응한 VM의 이름이다. 끝난 상태로 만들고 실행기는
    /// 실행하지 않는다.
    HaCheck {
        check: String,
        action: HaCheckAction,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "openapi", schema(default))]
        vms: Vec<String>,
        detail: String,
    },
    /// 노드의 본딩 알림 기록: 대상 노드(Task의 `node`와 같다)의 본딩 `bond`가 `degraded`나 `down`이 되었다.
    /// `ports`는 `up`이 아닌 포트(`"eno2: down"`)다. 끝난 상태로 만들고 실행기는 실행하지 않는다.
    BondDegraded {
        bond: String,
        ports: Vec<String>,
    },
    /// 실행 중인 대상 VM의 하드웨어(vCPU·메모리·디스크·NIC)를 지금 스펙에 맞춘다(핫플러그). 실행 중에 할 수
    /// 없는 것은 VM `status.hardware.pending`에 남긴다. VM의 노드에서 돈다.
    VmHotplug,
    /// 실행 중인 대상 VM의 로컬 디스크를 복제 대상 노드 `node`로 복제한다(한 회차). VM의 노드에서 돈다.
    VmReplicate {
        node: Uuid,
    },
    /// 이 노드에 Ceph를 설치하고 첫 mon·mgr을 세운다(부트스트랩). 대상은 `Ceph` 리소스다.
    CephInit,
    /// 이 노드에 Ceph mon을 더한다. 대상은 `Ceph` 리소스다.
    CephMonAdd,
    /// 이 노드에 Ceph mgr을 더한다. 대상은 `Ceph` 리소스다.
    CephMgrAdd,
    /// 이 노드의 디스크 `device`에 OSD를 만든다. `wipe`면 디스크를 먼저 지운다. 대상은 `Ceph` 리소스다.
    CephOsdAdd {
        device: String,
        #[serde(default, skip_serializing_if = "is_false")]
        #[cfg_attr(feature = "openapi", schema(default))]
        wipe: bool,
    },
    /// Ceph 풀 `pool`을 복제 수 `size`로 만들어 Orbivisor RBD 풀 `name`으로 등록한다. mon 노드에서 돈다.
    /// 대상은 `Ceph` 리소스다.
    CephPoolCreate {
        name: String,
        pool: String,
        size: u8,
    },
    /// 이 노드에 Ceph 클라이언트 꾸러미를 설치한다. 대상은 `Ceph` 리소스다.
    CephClientInstall,
    /// 템플릿(대상 VM)을 보통 VM으로 되돌린다(계획 58): 디스크마다 기반 스냅샷을 풀고 마지막에 `template: false`를
    /// 쓴다. VM의 노드에서 돈다.
    VmUntemplate,
    /// 꺼진 대상 VM의 펌웨어 상태를 초기화한다(계획 59): `uefi_vars`면 UEFI 변수, `tpm`이면 TPM 상태를 지우고
    /// `firmware_epoch`를 올린다. VM의 `firmware_node`(없거나 오프라인이면 요청 노드)에서 돈다.
    VmFirmwareReset {
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(default))]
        uefi_vars: bool,
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(default))]
        tpm: bool,
        /// 있으면 Task가 VM 스펙의 `secure_boot`도 이 값으로 바꾼다(UEFI 변수 초기화와 함께, 리뷰 1 M1). 스펙과 초기화를
        /// 한 Task가 쓰므로 API가 실패해도 둘 중 하나만 적용되지 않는다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secure_boot: Option<bool>,
    },
}

impl TaskOp {
    /// Ceph 자동 구성 Task(대상이 `Ceph` 리소스)
    pub fn is_ceph(&self) -> bool {
        matches!(
            self,
            TaskOp::CephInit
                | TaskOp::CephMonAdd
                | TaskOp::CephMgrAdd
                | TaskOp::CephOsdAdd { .. }
                | TaskOp::CephPoolCreate { .. }
                | TaskOp::CephClientInstall
        )
    }

    /// 템플릿 기능(계획 58, `node::TEMPLATES`)을 모르는 노드가 풀지 못하는 Task인지: 새 Task 종류와, 새 칸을 쓴
    /// `vm-clone`·`volume-download`
    pub fn needs_templates(&self) -> bool {
        match self {
            TaskOp::VolumeFlatten | TaskOp::VmUntemplate => true,
            TaskOp::VmClone {
                linked, overrides, ..
            } => linked.is_some() || overrides.is_some(),
            TaskOp::VolumeDownload {
                sha512,
                checksum_url,
                template,
                ..
            } => sha512.is_some() || checksum_url.is_some() || template.is_some(),
            _ => false,
        }
    }

    /// vTPM 기능(계획 59, `node::VTPM`)을 모르는 노드가 풀지 못하는 Task인지
    pub fn needs_vtpm(&self) -> bool {
        matches!(self, TaskOp::VmFirmwareReset { .. })
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct TaskStatus {
    pub state: TaskState,
    pub error: Option<String>,
    /// 실행 중인 Task의 진행률(지금은 로컬 디스크를 복사하는 마이그레이션만). Task가 끝나면 지운다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub progress: Option<TaskProgress>,
}

/// Task의 진행률: 단계(`copying-disks`, `migrating-memory`)와 복사한 바이트
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskProgress {
    pub stage: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
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
    fn volume_download_is_tagged_and_omits_a_missing_checksum() {
        let json = serde_json::json!({
            "kind": "volume-download",
            "url": "https://x/a.iso",
            "source_format": "raw"
        });
        let op: TaskOp = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            op,
            TaskOp::VolumeDownload {
                url: "https://x/a.iso".into(),
                sha256: None,
                source_format: SourceFormat::Raw,
                sha512: None,
                checksum_url: None,
                template: None,
            }
        );
        assert_eq!(serde_json::to_value(&op).unwrap(), json);

        let sum = "ab".repeat(32);
        let op = TaskOp::VolumeDownload {
            url: "https://x/a.qcow2".into(),
            sha256: Some(sum.clone()),
            source_format: SourceFormat::Qcow2,
            sha512: None,
            checksum_url: None,
            template: None,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(json["sha256"], sum);
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
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
            linked: None,
            overrides: None,
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
            linked: None,
            overrides: None,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn migrate_op_is_tagged_by_kind() {
        let node = Uuid::now_v7();
        // pool이 없으면 옛 노드가 읽는 지금 모양 그대로다.
        let op = TaskOp::VmMigrate { node, pool: None };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "vm-migrate", "node": node })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        let op = TaskOp::VmMigrate {
            node,
            pool: Some("fast".into()),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "vm-migrate", "node": node, "pool": "fast" })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn status_progress_is_written_only_while_known() {
        let json = serde_json::to_value(TaskStatus::default()).unwrap();
        assert!(json.get("progress").is_none(), "{json}");
        // 옛 노드가 쓴 status(진행률 없음)도 읽는다.
        let st: TaskStatus =
            serde_json::from_value(serde_json::json!({ "state": "running", "error": null }))
                .unwrap();
        assert_eq!(st.progress, None);
        let st = TaskStatus {
            state: TaskState::Running,
            error: None,
            progress: Some(TaskProgress {
                stage: "copying-disks".into(),
                done_bytes: 1 << 30,
                total_bytes: 4 << 30,
            }),
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(
            json["progress"],
            serde_json::json!({
                "stage": "copying-disks",
                "done_bytes": 1u64 << 30,
                "total_bytes": 4u64 << 30
            })
        );
        assert_eq!(serde_json::from_value::<TaskStatus>(json).unwrap(), st);
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
    fn node_power_off_op_is_tagged_by_kind() {
        let op = TaskOp::NodePowerOff {
            method: Some(FenceKind::Ipmi),
            reason: "lease expired".into(),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "node-power-off", "method": "ipmi", "reason": "lease expired" })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        // 끄지 못했으면 방법이 없다.
        let failed = TaskOp::NodePowerOff {
            method: None,
            reason: "lease expired".into(),
        };
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "node-power-off", "reason": "lease expired" })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), failed);
    }

    #[test]
    fn ha_check_and_bond_ops_are_tagged_by_kind() {
        let op = TaskOp::HaCheck {
            check: "stor".into(),
            action: HaCheckAction::RestartElsewhere,
            vms: vec!["web-1".into(), "db-1".into()],
            detail: "also affects 1 VMs without HA".into(),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "ha-check", "check": "stor", "action": "restart_elsewhere",
                                "vms": ["web-1", "db-1"], "detail": "also affects 1 VMs without HA" })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        // VM이 없으면 쓰지 않는다.
        let warn = TaskOp::HaCheck {
            check: "cluster-link-1".into(),
            action: HaCheckAction::Warn,
            vms: vec![],
            detail: "no interface has address 10.0.0.5".into(),
        };
        let json = serde_json::to_value(&warn).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "ha-check", "check": "cluster-link-1", "action": "warn",
                                "detail": "no interface has address 10.0.0.5" })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), warn);

        let op = TaskOp::BondDegraded {
            bond: "bond0".into(),
            ports: vec!["eno2: down".into(), "eno3: lacp_mismatch".into()],
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "bond-degraded", "bond": "bond0",
                                "ports": ["eno2: down", "eno3: lacp_mismatch"] })
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
                    folder: None,
                },
                serde_json::json!({
                    "kind": "backup-restore",
                    "backup": backup,
                    "name": "web-2",
                    "pool": "local"
                }),
            ),
            (
                TaskOp::BackupReplicate {
                    remote: "dr".into(),
                    pool: "backups".into(),
                },
                serde_json::json!({
                    "kind": "backup-replicate",
                    "remote": "dr",
                    "pool": "backups"
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
    fn node_drain_op_is_tagged_by_kind() {
        let json = serde_json::json!({ "kind": "node-drain" });
        assert_eq!(serde_json::to_value(TaskOp::NodeDrain).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<TaskOp>(json).unwrap(),
            TaskOp::NodeDrain
        );
    }

    #[test]
    fn vm_hotplug_op_is_tagged_by_kind() {
        let json = serde_json::json!({ "kind": "vm-hotplug" });
        assert_eq!(serde_json::to_value(TaskOp::VmHotplug).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<TaskOp>(json).unwrap(),
            TaskOp::VmHotplug
        );
    }

    /// Ceph Task 여섯 가지. OSD 더하기의 `wipe`는 false면 쓰지 않는다.
    #[test]
    fn ceph_ops_are_tagged_by_kind() {
        for (op, json) in [
            (TaskOp::CephInit, serde_json::json!({ "kind": "ceph-init" })),
            (
                TaskOp::CephMonAdd,
                serde_json::json!({ "kind": "ceph-mon-add" }),
            ),
            (
                TaskOp::CephMgrAdd,
                serde_json::json!({ "kind": "ceph-mgr-add" }),
            ),
            (
                TaskOp::CephOsdAdd {
                    device: "/dev/sdb".into(),
                    wipe: false,
                },
                serde_json::json!({ "kind": "ceph-osd-add", "device": "/dev/sdb" }),
            ),
            (
                TaskOp::CephOsdAdd {
                    device: "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4".into(),
                    wipe: true,
                },
                serde_json::json!({ "kind": "ceph-osd-add",
                                    "device": "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4",
                                    "wipe": true }),
            ),
            (
                TaskOp::CephPoolCreate {
                    name: "vmpool".into(),
                    pool: "rbd-vm".into(),
                    size: 3,
                },
                serde_json::json!({ "kind": "ceph-pool-create", "name": "vmpool",
                                    "pool": "rbd-vm", "size": 3 }),
            ),
            (
                TaskOp::CephClientInstall,
                serde_json::json!({ "kind": "ceph-client-install" }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&op).unwrap(), json);
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
            assert!(op.is_ceph(), "{op:?}");
        }
        assert!(!TaskOp::VmHotplug.is_ceph());
        assert!(!TaskOp::NodeUpgrade.is_ceph());
    }

    #[test]
    fn vm_replicate_op_is_tagged_by_kind() {
        let node = Uuid::now_v7();
        let json = serde_json::json!({ "kind": "vm-replicate", "node": node });
        assert_eq!(
            serde_json::to_value(TaskOp::VmReplicate { node }).unwrap(),
            json
        );
        assert_eq!(
            serde_json::from_value::<TaskOp>(json).unwrap(),
            TaskOp::VmReplicate { node }
        );
    }

    #[test]
    fn new_task_is_pending() {
        assert_eq!(TaskStatus::default().state, TaskState::Pending);
        assert!(!TaskState::Running.is_finished());
        assert!(TaskState::Failed.is_finished());
    }

    #[test]
    fn firmware_reset_round_trips_and_needs_vtpm() {
        let op = TaskOp::VmFirmwareReset {
            uefi_vars: true,
            tpm: false,
            secure_boot: None,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "vm-firmware-reset", "uefi_vars": true, "tpm": false })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        let tpm_only: TaskOp =
            serde_json::from_value(serde_json::json!({ "kind": "vm-firmware-reset", "tpm": true }))
                .unwrap();
        assert_eq!(
            tpm_only,
            TaskOp::VmFirmwareReset {
                uefi_vars: false,
                tpm: true,
                secure_boot: None,
            }
        );
        let sb = TaskOp::VmFirmwareReset {
            uefi_vars: true,
            tpm: false,
            secure_boot: Some(true),
        };
        assert_eq!(
            serde_json::to_value(&sb).unwrap()["secure_boot"],
            serde_json::json!(true)
        );
        assert!(op.needs_vtpm());
        assert!(!op.needs_templates());
        assert!(!TaskOp::VmUntemplate.needs_vtpm());
        assert!(!TaskOp::VolumeDelete.needs_vtpm());
    }

    #[test]
    fn plain_clone_and_download_stay_byte_identical_and_need_no_templates() {
        // 계획 57이 만든 모양 그대로 읽고 쓴다.
        for old in [
            r#"{"kind":"vm-clone","name":"web-02","snapshot":null,"pool":null}"#,
            r#"{"kind":"volume-download","url":"https://x/a.iso","sha256":"ab","source_format":"raw"}"#,
        ] {
            let op: TaskOp = serde_json::from_str(old).unwrap();
            assert_eq!(serde_json::to_string(&op).unwrap(), old);
            assert!(!op.needs_templates());
        }
        assert!(!TaskOp::VolumeDelete.needs_templates());
    }

    #[test]
    fn template_fields_round_trip_and_need_templates() {
        let clone = TaskOp::VmClone {
            name: "web-1".into(),
            snapshot: None,
            pool: None,
            linked: Some(true),
            overrides: Some(Box::new(CloneOverrides {
                vcpus: Some(4),
                start: true,
                ..CloneOverrides::default()
            })),
        };
        let json = serde_json::to_value(&clone).unwrap();
        assert_eq!(json["linked"], true);
        assert_eq!(
            json["overrides"],
            serde_json::json!({ "vcpus": 4, "start": true })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), clone);
        assert!(clone.needs_templates());
        let full = TaskOp::VmClone {
            name: "web-1".into(),
            snapshot: None,
            pool: None,
            linked: Some(false),
            overrides: None,
        };
        assert!(full.needs_templates());

        let download = TaskOp::VolumeDownload {
            url: "https://x/a.qcow2".into(),
            sha256: None,
            source_format: SourceFormat::Qcow2,
            sha512: None,
            checksum_url: Some("https://x/SHA256SUMS".into()),
            template: Some(Box::new(
                serde_json::from_value(serde_json::json!({ "name": "t", "network": "lan" }))
                    .unwrap(),
            )),
        };
        let json = serde_json::to_value(&download).unwrap();
        assert_eq!(json["checksum_url"], "https://x/SHA256SUMS");
        assert_eq!(json["template"]["name"], "t");
        assert!(json.get("sha512").is_none());
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), download);
        assert!(download.needs_templates());
        let sha512 = TaskOp::VolumeDownload {
            url: "https://x/a.qcow2".into(),
            sha256: None,
            source_format: SourceFormat::Qcow2,
            sha512: Some("cd".repeat(64)),
            checksum_url: None,
            template: None,
        };
        assert!(sha512.needs_templates());

        for (op, kind) in [
            (TaskOp::VolumeFlatten, "volume-flatten"),
            (TaskOp::VmUntemplate, "vm-untemplate"),
        ] {
            let json = serde_json::to_value(&op).unwrap();
            assert_eq!(json, serde_json::json!({ "kind": kind }));
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
            assert!(op.needs_templates());
        }
    }
}
