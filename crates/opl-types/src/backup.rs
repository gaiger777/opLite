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
    /// 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub origin: Option<BackupOrigin>,
}

/// 받은 사본의 원본: 보낸 클러스터와 그 클러스터의 백업
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BackupOrigin {
    /// 보낸 클러스터 이름
    pub cluster: String,
    /// 보낸 클러스터 CA의 지문(`sha256:…`). 같은 원본의 사본을 알아보는 데 쓴다.
    pub ca_fingerprint: String,
    /// 원본 백업 id
    pub backup: Uuid,
    /// 원본을 만든 백업 작업 이름. 손으로 만든 백업이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub job: Option<String>,
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
    /// 원격 클러스터에 보낸 사본. 없으면 뺀다.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = Option<Vec<BackupReplica>>, nullable = false, default)
    )]
    pub replicas: Vec<BackupReplica>,
    /// 받는 중인 사본이 마지막으로 파일을 받은 시각(유닉스 초). 받은 적이 없으면 뺀다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub received_at: Option<u64>,
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
    /// 다른 클러스터에서 파일을 받는 중
    Receiving,
}

/// 원격 클러스터의 백업 풀에 보낸 사본 하나. (remote, pool)마다 하나다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupReplica {
    /// 원격 이름
    pub remote: String,
    /// 원격의 백업 풀 이름
    pub pool: String,
    /// 원격에 만든 사본의 id. 만들기 전이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub backup: Option<Uuid>,
    pub state: ReplicaState,
    /// 실패한 이유
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 상태가 바뀐 시각(유닉스 초)
    pub at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ReplicaState {
    /// 보내는 중
    Copying,
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
    /// 받은 사본이면 받은 파일의 SHA-256(16진수 소문자). 이 클러스터에서 만든 백업이면 뺀다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub sha256: Option<String>,
}

/// 백업 디렉터리의 `manifest.json`. opLite 없이도 백업을 알아볼 수 있게 VM 스펙과 디스크 목록을 담는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
        assert_eq!(spec.origin, None);
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("job").is_none(), "{json}");
        assert!(json.get("origin").is_none(), "{json}");
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
                sha256: None,
            }],
            uefi_vars: true,
            error: None,
            node: None,
            replicas: vec![],
            received_at: None,
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
    fn spec_round_trips_its_origin() {
        let backup = Uuid::now_v7();
        let fp = format!("sha256:{}", "ab".repeat(32));
        let json = serde_json::json!({
            "vm": Uuid::now_v7(),
            "vm_name": "web",
            "pool": "dr",
            "vm_spec": vm_spec(),
            "origin": { "cluster": "main", "ca_fingerprint": fp, "backup": backup, "job": "nightly" }
        });
        let spec: BackupSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            spec.origin,
            Some(BackupOrigin {
                cluster: "main".into(),
                ca_fingerprint: fp.clone(),
                backup,
                job: Some("nightly".into()),
            })
        );
        assert_eq!(serde_json::to_value(&spec).unwrap(), json);

        // 손으로 만든 원본이면 job을 뺀다.
        let manual = BackupSpec {
            origin: Some(BackupOrigin {
                job: None,
                ..spec.origin.clone().unwrap()
            }),
            ..spec
        };
        let json = serde_json::to_value(&manual).unwrap();
        assert!(json["origin"].get("job").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<BackupSpec>(json).unwrap(), manual);

        let bad = serde_json::json!({ "cluster": "main", "ca_fingerprint": fp, "backup": backup, "jobs": "x" });
        assert!(serde_json::from_value::<BackupOrigin>(bad).is_err());
    }

    #[test]
    fn status_round_trips_receiving_and_replicas() {
        let receiving: BackupStatus =
            serde_json::from_value(serde_json::json!({ "phase": "receiving" })).unwrap();
        assert_eq!(receiving.phase, BackupPhase::Receiving);
        assert_eq!(
            serde_json::to_value(BackupPhase::Receiving).unwrap(),
            "receiving"
        );

        // 사본이 없으면 replicas를 빼서 옛 노드가 읽는 모양 그대로다.
        let json = serde_json::to_value(BackupStatus::default()).unwrap();
        assert!(json.get("replicas").is_none(), "{json}");
        assert_eq!(
            json,
            serde_json::json!({
                "phase": "pending",
                "created_at": null,
                "disks": [],
                "uefi_vars": false,
                "error": null
            })
        );

        let copy = Uuid::now_v7();
        let status = BackupStatus {
            phase: BackupPhase::Ready,
            replicas: vec![
                BackupReplica {
                    remote: "dr".into(),
                    pool: "backups".into(),
                    backup: Some(copy),
                    state: ReplicaState::Ready,
                    error: None,
                    at: 1_790_000_000,
                },
                BackupReplica {
                    remote: "far".into(),
                    pool: "backups".into(),
                    backup: None,
                    state: ReplicaState::Failed,
                    error: Some("remote unreachable".into()),
                    at: 1_790_000_001,
                },
            ],
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["replicas"],
            serde_json::json!([
                { "remote": "dr", "pool": "backups", "backup": copy, "state": "ready", "at": 1_790_000_000u64 },
                { "remote": "far", "pool": "backups", "state": "failed", "error": "remote unreachable", "at": 1_790_000_001u64 }
            ])
        );
        assert_eq!(
            serde_json::from_value::<BackupStatus>(json).unwrap(),
            status
        );
        assert_eq!(
            serde_json::to_value(ReplicaState::Copying).unwrap(),
            "copying"
        );
    }

    #[test]
    fn received_copies_keep_file_hashes_and_the_last_activity() {
        let hash = "ab".repeat(32);
        let json = serde_json::json!({
            "phase": "receiving",
            "created_at": 1_790_000_000u64,
            "disks": [{
                "name": "disk0",
                "file": "disk0.qcow2",
                "virtual_bytes": 1u64 << 20,
                "stored_bytes": 3u64,
                "sha256": hash
            }],
            "uefi_vars": false,
            "error": null,
            "received_at": 1_790_000_100u64
        });
        let status: BackupStatus = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(status.received_at, Some(1_790_000_100));
        assert_eq!(status.disks[0].sha256.as_deref(), Some(hash.as_str()));
        assert_eq!(serde_json::to_value(&status).unwrap(), json);

        // 없으면 빼서 옛 노드가 읽는 모양 그대로다.
        let plain = BackupStatus {
            disks: vec![BackupDisk {
                sha256: None,
                ..status.disks[0].clone()
            }],
            received_at: None,
            ..status
        };
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("received_at").is_none(), "{json}");
        assert!(json["disks"][0].get("sha256").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<BackupStatus>(json).unwrap(), plain);
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
                sha256: None,
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
