//! VM 백업. 백업 풀(`backups`를 켠 dir·NFS 풀)의 `{루트}/backups/{백업 id}/`에 디스크마다 압축
//! qcow2(`disk{i}.qcow2`), UEFI 변수(`vars.fd`), 매니페스트(`manifest.json`)를 둔다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::VmSpec;

/// 리소스 이름은 사용자가 붙인 이름(클러스터 전체에서 고유)이다. 기본은 [`default_backup_name`].
pub type Backup = Resource<BackupSpec, BackupStatus>;

/// 매니페스트 형식 버전. 다른 버전의 백업은 복원하지 않는다.
pub const MANIFEST_VERSION: u32 = 1;

/// 백업 디렉터리 안의 매니페스트 파일 이름
pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BackupSpec {
    /// VM id. VM을 지워도 백업은 남는다.
    pub vm: Uuid,
    /// 만든 때의 VM 이름(표시용)
    pub vm_name: String,
    /// 백업 풀 이름
    pub pool: String,
    /// 만든 때의 VM 스펙
    pub vm_spec: VmSpec,
    /// 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<Uuid>,
}

impl ResourceSpec for BackupSpec {
    const KIND: Kind = Kind::Backup;
    type Status = BackupStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct BackupStatus {
    pub phase: BackupPhase,
    /// 만든 시각(유닉스 초)
    pub created_at: Option<u64>,
    pub disks: Vec<BackupDisk>,
    /// UEFI 변수 파일(`vars.fd`)을 담았는지
    pub uefi_vars: bool,
    /// 실패한 이유
    pub error: Option<String>,
    /// 파일을 쓴 노드. 공유가 아닌 풀의 백업은 이 노드에만 있으므로 지우기·복원도 이 노드에서 한다.
    /// 공유 풀이면 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub node: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BackupPhase {
    /// 만드는 중
    #[default]
    Pending,
    Ready,
    Failed,
}

/// 백업에 담은 디스크 하나. VM 스펙의 디스크 순서를 따른다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupDisk {
    /// 디스크 이름(`disk{i}`)
    pub name: String,
    /// 백업 디렉터리 안의 파일 이름(`disk{i}.qcow2`)
    pub file: String,
    /// 디스크 크기
    pub virtual_bytes: u64,
    /// 백업 파일 크기
    pub stored_bytes: u64,
}

/// 백업 디렉터리의 `manifest.json`. opLite 없이도 백업을 알아볼 수 있게 VM 스펙과 디스크 목록을 담는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// 형식 버전([`MANIFEST_VERSION`])
    pub version: u32,
    /// 백업 id
    pub backup: Uuid,
    pub vm: Uuid,
    pub vm_name: String,
    pub vm_spec: VmSpec,
    pub disks: Vec<BackupDisk>,
    pub uefi_vars: bool,
    /// 만든 시각(유닉스 초)
    pub created_at: u64,
}

/// 기본 백업 이름 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC). 리소스 이름 길이(63자)를 넘지 않게 VM 이름을
/// 47자까지 줄인다(끝의 `-`는 뺀다).
pub fn default_backup_name(vm_name: &str, at: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(at as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let prefix = vm_name[..vm_name.len().min(47)].trim_end_matches('-');
    format!(
        "{prefix}-{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// 풀 루트 아래 백업 하나의 디렉터리 `{root}/backups/{id}`
pub fn backup_dir(pool_root: &Path, backup: Uuid) -> PathBuf {
    pool_root.join("backups").join(backup.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::validate_name;

    fn vm_spec() -> VmSpec {
        serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 },
            "memory_mib": 512,
            "machine": "virt",
            "disks": [ { "volume": "web-root", "bus": "virtio-blk" } ]
        }))
        .unwrap()
    }

    #[test]
    fn spec_round_trips_and_leaves_out_job() {
        let vm = Uuid::now_v7();
        let spec: BackupSpec = serde_json::from_value(serde_json::json!({
            "vm": vm,
            "vm_name": "web",
            "pool": "backups",
            "vm_spec": vm_spec()
        }))
        .unwrap();
        assert_eq!(spec.vm, vm);
        assert_eq!(spec.job, None);
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("job").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<BackupSpec>(json).unwrap(), spec);

        let job = Uuid::now_v7();
        let with = BackupSpec {
            job: Some(job),
            ..spec
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["job"], serde_json::json!(job));
        assert_eq!(serde_json::from_value::<BackupSpec>(json).unwrap(), with);
    }

    #[test]
    fn spec_rejects_unknown_fields() {
        let res = serde_json::from_value::<BackupSpec>(serde_json::json!({
            "vm": Uuid::now_v7(),
            "vm_name": "web",
            "pool": "backups",
            "vm_spec": vm_spec(),
            "poll": "x"
        }));
        assert!(res.is_err());
    }

    #[test]
    fn status_defaults_to_pending_and_serializes_phase() {
        let st: BackupStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(st, BackupStatus::default());
        assert_eq!(st.phase, BackupPhase::Pending);

        let ready = BackupStatus {
            phase: BackupPhase::Ready,
            created_at: Some(1_790_000_000),
            disks: vec![BackupDisk {
                name: "disk0".into(),
                file: "disk0.qcow2".into(),
                virtual_bytes: 10 << 30,
                stored_bytes: 1 << 30,
            }],
            uefi_vars: true,
            error: None,
            node: None,
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({
                "phase": "ready",
                "created_at": 1_790_000_000u64,
                "disks": [{
                    "name": "disk0",
                    "file": "disk0.qcow2",
                    "virtual_bytes": 10u64 << 30,
                    "stored_bytes": 1u64 << 30
                }],
                "uefi_vars": true,
                "error": null
            })
        );
        let failed: BackupStatus =
            serde_json::from_value(serde_json::json!({ "phase": "failed", "error": "disk full" }))
                .unwrap();
        assert_eq!(failed.phase, BackupPhase::Failed);
        assert_eq!(failed.error.as_deref(), Some("disk full"));

        // 공유가 아닌 풀의 백업은 파일을 쓴 노드를 적는다. 없으면 뺀다.
        let node = Uuid::now_v7();
        let local = BackupStatus {
            node: Some(node),
            ..ready
        };
        let json = serde_json::to_value(&local).unwrap();
        assert_eq!(json["node"], serde_json::json!(node));
        assert_eq!(serde_json::from_value::<BackupStatus>(json).unwrap(), local);
    }

    #[test]
    fn manifest_round_trips() {
        let manifest = BackupManifest {
            version: MANIFEST_VERSION,
            backup: Uuid::now_v7(),
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            vm_spec: vm_spec(),
            disks: vec![BackupDisk {
                name: "disk0".into(),
                file: "disk0.qcow2".into(),
                virtual_bytes: 1 << 30,
                stored_bytes: 1 << 20,
            }],
            uefi_vars: false,
            created_at: 1_790_000_000,
        };
        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["vm_name"], "web");
        assert_eq!(json["disks"][0]["file"], "disk0.qcow2");
        assert_eq!(json["vm_spec"]["disks"][0]["volume"], "web-root");
        let text = serde_json::to_string_pretty(&manifest).unwrap();
        assert_eq!(
            serde_json::from_str::<BackupManifest>(&text).unwrap(),
            manifest
        );
    }

    #[test]
    fn default_names_are_utc_seconds() {
        // 2026-10-27 05:07:09 UTC
        assert_eq!(
            default_backup_name("web", 1_793_077_629),
            "web-20261027-050709"
        );
        assert_eq!(default_backup_name("db", 0), "db-19700101-000000");
        // 긴 VM 이름은 줄여서 이름 규칙에 맞춘다.
        let long = "a".repeat(63);
        let name = default_backup_name(&long, 0);
        assert_eq!(name.len(), 63);
        assert_eq!(validate_name(&name), Ok(()));
        let dash = format!("{}-b", "a".repeat(46));
        assert_eq!(
            default_backup_name(&dash, 0),
            format!("{}-19700101-000000", "a".repeat(46))
        );
    }

    #[test]
    fn kind_is_backup() {
        assert_eq!(serde_json::to_value(Kind::Backup).unwrap(), "backup");
        assert_eq!(Kind::Backup.as_str(), "backup");
    }

    #[test]
    fn backup_dir_is_under_backups() {
        let id = Uuid::now_v7();
        assert_eq!(
            backup_dir(Path::new("/srv/backups"), id),
            PathBuf::from(format!("/srv/backups/backups/{id}"))
        );
    }
}
