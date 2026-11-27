//! VM 백업. 백업 풀(`backups`를 켠 dir·NFS 풀)의 `{루트}/backups/{백업 id}/`에 디스크마다 압축
//! qcow2(`disk{i}.qcow2`), UEFI 변수(`vars.fd`), 매니페스트(`manifest.json`)를 둔다. 백업 저장소(계획 68,
//! [`crate::backup_store`])의 백업은 `spec.store`를 쓰고 데이터는 저장소의 청크·인덱스·매니페스트 v3에 있다.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cloud_init::{CloudInitDns, CloudInitNet};
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::VmSpec;

/// 리소스 이름은 사용자가 붙인 이름(클러스터 전체에서 고유)이다. 기본은 [`default_backup_name`].
pub type Backup = Resource<BackupSpec, BackupStatus>;

/// 매니페스트 형식 버전. 다른 버전의 백업은 복원하지 않는다.
pub const MANIFEST_VERSION: u32 = 1;

/// 증분 백업(부모가 있는 백업)의 매니페스트 형식 버전. 전체 백업은 옛 노드도 읽게 [`MANIFEST_VERSION`]으로 쓴다.
pub const MANIFEST_VERSION_INCREMENTAL: u32 = 2;

/// 백업 디렉터리 안의 매니페스트 파일 이름
pub const MANIFEST_FILE: &str = "manifest.json";

/// 증분 백업이 디스크에 두는 dirty bitmap 이름의 접두사. 이 접두사의 비트맵은 opl이 만들고 지운다.
pub const BITMAP_PREFIX: &str = "opl-";

/// 백업 `backup`의 시점 뒤에 바뀐 블록을 따라가는 dirty bitmap 이름 `opl-{id simple}`
pub fn bitmap_name(backup: Uuid) -> String {
    format!("{BITMAP_PREFIX}{}", backup.simple())
}

/// 백업 비트맵([`bitmap_name`] 모양: `opl-` 뒤가 소문자 16진 32자)인지. 같은 `opl-` 무리의 복제·마이그레이션
/// 비트맵(`opl-repl-…`, `opl-mig-…`)은 백업 비트맵이 아니다.
pub fn is_backup_bitmap(name: &str) -> bool {
    name.strip_prefix(BITMAP_PREFIX)
        .and_then(crate::replication::parse_simple)
        .is_some()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BackupSpec {
    /// VM id. VM을 지워도 백업은 남는다.
    pub vm: Uuid,
    /// 만든 때의 VM 이름(표시용)
    pub vm_name: String,
    /// 백업 풀 이름. 저장소 백업(`store`, 계획 68)이면 비우고 쓰지 않는다(옛 JSON에는 늘 있으므로 바이트 그대로다).
    /// `pool`과 `store` 중 정확히 하나다([`BackupSpec::target`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub pool: String,
    /// 백업 저장소 이름(계획 68). 없으면 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub store: Option<String>,
    /// 옛 백업을 옮기거나(`convert`) 다른 저장소에서 복사해(`copy`) 만든 저장소 백업의 원본(계획 68 §12.2·§12.5). 없으면
    /// 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub source: Option<BackupSource>,
    /// 만든 때의 VM 스펙
    pub vm_spec: VmSpec,
    /// 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<Uuid>,
    /// 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub origin: Option<BackupOrigin>,
    /// 할 수 있으면 증분으로 백업한다(바로 앞 백업을 부모로 바뀐 블록만). 할 수 없으면 전체로 하고 그
    /// 이유를 `status.full_reason`에 적는다. false면 뺀다.
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub incremental: bool,
    /// 증분 체인에 둘 증분 백업 수의 한도(백업 작업의 `full_every`). 부모의 체인에 이미 이만큼 증분이
    /// 있으면 전체로 한다. 없으면 한도가 없다(손으로 만든 백업).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub max_chain: Option<u32>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// 백업이 사는 곳: 백업 풀(계획 27) 또는 백업 저장소(계획 68)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupTarget<'a> {
    Pool(&'a str),
    Store(&'a str),
}

/// `pool`과 `store`가 둘 다 있거나 둘 다 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a backup needs exactly one of pool and store")]
pub struct BackupTargetError;

impl BackupSpec {
    /// 백업이 사는 곳. `pool`과 `store` 중 정확히 하나가 아니면 오류다.
    pub fn target(&self) -> Result<BackupTarget<'_>, BackupTargetError> {
        match (self.pool.is_empty(), &self.store) {
            (false, None) => Ok(BackupTarget::Pool(&self.pool)),
            (true, Some(store)) => Ok(BackupTarget::Store(store)),
            _ => Err(BackupTargetError),
        }
    }
}

/// 저장소 백업의 원본(계획 68): 그 백업 id와 만든 방법
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BackupSource {
    /// 원본 백업 id
    pub backup: Uuid,
    pub kind: BackupSourceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BackupSourceKind {
    /// 옛 풀 백업(qcow2)을 저장소로 옮김(`backup convert`). 원본은 남는다.
    Convert,
    /// 같은 클러스터의 다른 저장소에서 복사함(계층)
    Copy,
}

/// 저장소 백업 하나가 읽고 쓴 양(계획 68 §8.4)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct StoreBackupStats {
    /// 디스크 가상 크기 합
    pub logical_bytes: u64,
    /// 이번에 읽은 바이트(증분이면 바뀐 청크만)
    pub read_bytes: u64,
    /// 저장소에 처음 올린 청크 수
    pub new_chunks: u64,
    /// 이 백업이 저장소에 처음 쓴 저장 바이트(압축·암호화 뒤). 할당량 `backup_bytes`가 센다.
    pub new_bytes: u64,
    /// 이미 저장소에 있어 다시 쓰지 않은 청크 수
    pub reused_chunks: u64,
    /// 내용이 모두 0이라 저장하지 않은 청크 수
    pub zero_chunks: u64,
}

/// 백업 하나의 마지막 검증 결과(계획 68 §10.5)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupVerify {
    /// 검증한 시각(유닉스 초)
    pub at: u64,
    pub state: VerifyState,
    /// 이 백업이 가리키는 깨진 청크 수
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub bad_chunks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VerifyState {
    Ok,
    Failed,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
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
    /// 증분 백업의 부모(같은 풀의 바로 앞 백업). 전체 백업이면 뺀다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub parent: Option<Uuid>,
    /// 증분을 요청했지만 전체로 한 이유. 없으면 뺀다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub full_reason: Option<String>,
    /// TPM 상태 파일(`tpm2-00.permall`)을 담았는지(계획 59). 거짓이면 뺀다.
    #[serde(skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub tpm_state: bool,
    /// 저장소 백업(계획 68)에서 바뀌지 않은 청크를 가져온 앞 백업. 계획 40의 `parent`와 달리 복원은 이것을 따라가지
    /// 않는다(인덱스가 늘 완전하다). 없으면 쓰지 않는다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub base: Option<Uuid>,
    /// 저장소 백업이 읽고 쓴 양(계획 68). 없으면 쓰지 않는다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub store_stats: Option<StoreBackupStats>,
    /// 마지막 검증 결과(계획 68). 검증한 적이 없으면 쓰지 않는다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub verify: Option<BackupVerify>,
}

impl BackupStatus {
    /// 할당량 `backup_bytes`(계획 68 §13)에 드는 바이트: 저장소 백업은 처음 쓴 바이트(`store_stats.new_bytes`), 풀
    /// 백업은 디스크 파일 크기 합
    pub fn backup_bytes(&self) -> u64 {
        match &self.store_stats {
            Some(s) => s.new_bytes,
            None => self
                .disks
                .iter()
                .fold(0u64, |sum, d| sum.saturating_add(d.stored_bytes)),
        }
    }

    /// [`BackupStatus::backup_bytes`]의 JSON 판(상태 머신 — 모르는 칸이 있어도 같은 답)
    pub fn backup_bytes_value(status: &serde_json::Value) -> u64 {
        match status["store_stats"]["new_bytes"].as_u64() {
            Some(n) => n,
            None => status["disks"]
                .as_array()
                .into_iter()
                .flatten()
                .fold(0u64, |sum, d| {
                    sum.saturating_add(d["stored_bytes"].as_u64().unwrap_or(0))
                }),
        }
    }
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

/// 백업 디렉터리의 `manifest.json`. Orbivisor 없이도 백업을 알아볼 수 있게 VM 스펙과 디스크 목록을 담는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupManifest {
    /// 형식 버전([`MANIFEST_VERSION`], 증분 백업은 [`MANIFEST_VERSION_INCREMENTAL`])
    pub version: u32,
    /// 백업 id
    pub backup: Uuid,
    /// 증분 백업의 부모 백업 id. 디스크 파일은 바뀐 블록만 담고, 부모는 헤더가 아니라 여기로만 잇는다.
    /// 전체 백업이면 뺀다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub parent: Option<Uuid>,
    pub vm: Uuid,
    pub vm_name: String,
    pub vm_spec: VmSpec,
    pub disks: Vec<BackupDisk>,
    pub uefi_vars: bool,
    /// 만든 시각(유닉스 초)
    pub created_at: u64,
    /// TPM 상태 파일(`tpm2-00.permall`, 0600)을 담았는지(계획 59). 거짓이면 빼고 써서 형식 버전은 그대로다. 옛 노드는
    /// 이 필드를 무시한다.
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub tpm_state: bool,
}

/// 백업 디렉터리 안 TPM 상태 파일의 이름(swtpm의 TPM 2.0 영구 상태와 같은 이름)
pub const TPM_STATE_FILE: &str = "tpm2-00.permall";

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

/// 복원 맵(계획 76 §3.3): 복원 Task가 새 VM을 만들기 전에 백업의 VM 스펙에 쓴다. 복원 요청의 같은 이름 칸에서 오고
/// Task(`backup-restore`·`backup-live-restore`)에 실린다. 모든 노드가 `restore-remap`을 알 때만 쓴다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RestoreRemap {
    /// 백업의 네트워크 이름 → 새 VM의 네트워크 이름. 맵에 없는 이름은 그대로다.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub networks: BTreeMap<String, String>,
    /// 백업의 보안 그룹 이름 → 새 VM의 보안 그룹 이름. 맵에 없는 이름은 그대로다.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub security_groups: BTreeMap<String, String>,
    /// 주면 새 VM의 cloud-init `networks`를 이것으로 바꾼다(cloud-init이 없던 백업이면 새로 붙인다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cloud_init_networks: Option<Vec<CloudInitNet>>,
    /// 주면 새 VM의 cloud-init `dns`를 이것으로 바꾼다(cloud-init이 없던 백업이면 새로 붙인다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cloud_init_dns: Option<CloudInitDns>,
    /// 새 VM에 처음부터 달 라벨(계획 76 리뷰 2 — DR 엔진의 계획·실행 라벨: VM이 생기는 때부터 라벨이 있어 틈이 없다).
    /// 복원 API 요청에는 없다(엔진만 쓴다).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub labels: BTreeMap<String, String>,
}

impl RestoreRemap {
    /// 아무것도 바꾸지 않는지
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// 백업의 VM 스펙 `spec`에 맞는지: 맵의 원본 이름은 그 VM의 NIC가 쓰는 네트워크·보안 그룹이어야 하고, cloud-init
    /// 주소 설정은 NIC 수를 넘지 않아야 한다. 대상 이름이 이 클러스터에 있는지는 부르는 쪽이 바꾼 스펙으로 본다.
    pub fn check(&self, spec: &VmSpec) -> Result<(), String> {
        for from in self.networks.keys() {
            if !spec.nics.iter().any(|n| &n.network == from) {
                return Err(format!(
                    "networks: the backed-up VM has no NIC on network {from:?}"
                ));
            }
        }
        for from in self.security_groups.keys() {
            if !spec.nics.iter().any(|n| n.security_groups.contains(from)) {
                return Err(format!(
                    "security_groups: the backed-up VM has no NIC in security group {from:?}"
                ));
            }
        }
        if let Some(nets) = &self.cloud_init_networks
            && nets.len() > spec.nics.len()
        {
            return Err(format!(
                "cloud_init_networks: {} entries but the backed-up VM has {} NICs",
                nets.len(),
                spec.nics.len()
            ));
        }
        Ok(())
    }

    /// 스펙 `spec`의 NIC 네트워크·보안 그룹을 맵대로 바꾸고 cloud-init 주소·DNS를 넣는다. 두 원본 그룹이 같은 대상으로
    /// 가면 NIC에 한 번만 둔다.
    pub fn apply(&self, spec: &mut VmSpec) {
        for nic in &mut spec.nics {
            if let Some(to) = self.networks.get(&nic.network) {
                nic.network.clone_from(to);
            }
            let mut groups: Vec<String> = Vec::with_capacity(nic.security_groups.len());
            for g in &nic.security_groups {
                let g = self.security_groups.get(g).unwrap_or(g);
                if !groups.contains(g) {
                    groups.push(g.clone());
                }
            }
            nic.security_groups = groups;
        }
        if self.cloud_init_networks.is_some() || self.cloud_init_dns.is_some() {
            let ci = spec.cloud_init.get_or_insert_with(Default::default);
            if let Some(nets) = &self.cloud_init_networks {
                ci.networks.clone_from(nets);
            }
            if let Some(dns) = &self.cloud_init_dns {
                ci.dns = Some(dns.clone());
            }
        }
    }
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
            parent: None,
            full_reason: None,
            tpm_state: false,
            base: None,
            store_stats: None,
            verify: None,
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
            parent: None,
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
            tpm_state: false,
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

    /// 증분 요청·부모·전체로 한 이유는 없으면 빼서 옛 노드가 읽는 모양 그대로다.
    #[test]
    fn incremental_fields_round_trip_and_are_omitted_when_unset() {
        let spec: BackupSpec = serde_json::from_value(serde_json::json!({
            "vm": Uuid::now_v7(),
            "vm_name": "web",
            "pool": "backups",
            "vm_spec": vm_spec()
        }))
        .unwrap();
        assert!(!spec.incremental);
        assert!(
            serde_json::to_value(&spec)
                .unwrap()
                .get("incremental")
                .is_none()
        );
        let incremental = BackupSpec {
            incremental: true,
            ..spec
        };
        let json = serde_json::to_value(&incremental).unwrap();
        assert_eq!(json["incremental"], true);
        assert_eq!(
            serde_json::from_value::<BackupSpec>(json).unwrap(),
            incremental
        );

        let json = serde_json::to_value(BackupStatus::default()).unwrap();
        assert!(json.get("parent").is_none(), "{json}");
        assert!(json.get("full_reason").is_none(), "{json}");
        let parent = Uuid::now_v7();
        let child = BackupStatus {
            parent: Some(parent),
            ..Default::default()
        };
        let json = serde_json::to_value(&child).unwrap();
        assert_eq!(json["parent"], serde_json::json!(parent));
        assert_eq!(serde_json::from_value::<BackupStatus>(json).unwrap(), child);
        let full = BackupStatus {
            full_reason: Some("VM is stopped".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["full_reason"], "VM is stopped");
        assert_eq!(serde_json::from_value::<BackupStatus>(json).unwrap(), full);
    }

    /// 체인 한도는 없으면 뺀다(손으로 만든 백업).
    #[test]
    fn max_chain_round_trips_and_is_omitted_when_unset() {
        let spec: BackupSpec = serde_json::from_value(serde_json::json!({
            "vm": Uuid::now_v7(),
            "vm_name": "web",
            "pool": "backups",
            "vm_spec": vm_spec()
        }))
        .unwrap();
        assert_eq!(spec.max_chain, None);
        assert!(
            serde_json::to_value(&spec)
                .unwrap()
                .get("max_chain")
                .is_none()
        );
        let limited = BackupSpec {
            incremental: true,
            max_chain: Some(6),
            ..spec
        };
        let json = serde_json::to_value(&limited).unwrap();
        assert_eq!(json["max_chain"], 6);
        assert_eq!(serde_json::from_value::<BackupSpec>(json).unwrap(), limited);
    }

    /// 증분 백업의 매니페스트는 버전 2에 부모를 담는다. 전체 백업은 부모를 빼고 버전 1 그대로다.
    #[test]
    fn manifests_of_incremental_backups_carry_their_parent() {
        let full = BackupManifest {
            version: MANIFEST_VERSION,
            backup: Uuid::now_v7(),
            parent: None,
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            vm_spec: vm_spec(),
            disks: vec![],
            uefi_vars: false,
            created_at: 1_790_000_000,
            tpm_state: false,
        };
        let json = serde_json::to_value(&full).unwrap();
        assert!(json.get("parent").is_none(), "{json}");
        assert_eq!(
            serde_json::from_value::<BackupManifest>(json).unwrap(),
            full
        );

        let parent = full.backup;
        let child = BackupManifest {
            version: MANIFEST_VERSION_INCREMENTAL,
            backup: Uuid::now_v7(),
            parent: Some(parent),
            ..full
        };
        let json = serde_json::to_value(&child).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["parent"], serde_json::json!(parent));
        assert_eq!(
            serde_json::from_value::<BackupManifest>(json).unwrap(),
            child
        );
    }

    /// 계획 68: 옛 풀 백업의 스펙·상태 JSON은 바이트 그대로이고, 저장소 백업은 `pool`을 쓰지 않는다.
    #[test]
    fn store_backups_leave_out_the_pool_and_old_json_stays_byte_identical() {
        let vm = Uuid::now_v7();
        // 이 빌드 전 노드가 쓴 모양(칸 차례 그대로)
        let vm_spec_json = serde_json::to_string(&vm_spec()).unwrap();
        let spec_json = format!(
            r#"{{"vm":"{vm}","vm_name":"web","pool":"backups","vm_spec":{vm_spec_json},"incremental":true}}"#
        );
        let old: BackupSpec = serde_json::from_str(&spec_json).unwrap();
        assert_eq!(old.store, None);
        assert_eq!(old.source, None);
        assert_eq!(old.target(), Ok(BackupTarget::Pool("backups")));
        assert_eq!(serde_json::to_string(&old).unwrap(), spec_json);

        let source = Uuid::now_v7();
        let json = serde_json::json!({
            "vm": vm, "vm_name": "web", "store": "local-st", "vm_spec": vm_spec(),
            "source": { "backup": source, "kind": "convert" }
        });
        let st: BackupSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(st.pool, "");
        assert_eq!(st.target(), Ok(BackupTarget::Store("local-st")));
        assert_eq!(
            st.source,
            Some(BackupSource {
                backup: source,
                kind: BackupSourceKind::Convert
            })
        );
        assert_eq!(serde_json::to_value(&st).unwrap(), json);
        assert_eq!(
            serde_json::to_value(BackupSourceKind::Copy).unwrap(),
            "copy"
        );

        let both = BackupSpec {
            pool: "backups".into(),
            ..st.clone()
        };
        assert_eq!(both.target(), Err(BackupTargetError));
        let neither = BackupSpec {
            store: None,
            ..st.clone()
        };
        assert_eq!(neither.target(), Err(BackupTargetError));
        let bad = serde_json::json!({ "backup": source, "kind": "move" });
        assert!(serde_json::from_value::<BackupSource>(bad).is_err());

        let status_json = r#"{"phase":"ready","created_at":1790000000,"disks":[{"name":"disk0","file":"disk0.qcow2","virtual_bytes":1073741824,"stored_bytes":1048576}],"uefi_vars":false,"error":null,"parent":"0190a2b3-c4d5-7e6f-8a9b-0c1d2e3f4a5b"}"#;
        let status: BackupStatus = serde_json::from_str(status_json).unwrap();
        assert_eq!(serde_json::to_string(&status).unwrap(), status_json);
        assert_eq!(status.backup_bytes(), 1 << 20);
        assert_eq!(
            BackupStatus::backup_bytes_value(&serde_json::from_str(status_json).unwrap()),
            1 << 20
        );
    }

    #[test]
    fn store_backup_status_round_trips() {
        let base = Uuid::now_v7();
        let status = BackupStatus {
            phase: BackupPhase::Ready,
            disks: vec![BackupDisk {
                name: "disk0".into(),
                file: "disk0.fidx".into(),
                virtual_bytes: 20 << 30,
                stored_bytes: 300 << 20,
                sha256: None,
            }],
            base: Some(base),
            store_stats: Some(StoreBackupStats {
                logical_bytes: 20 << 30,
                read_bytes: 1 << 30,
                new_chunks: 90,
                new_bytes: 300 << 20,
                reused_chunks: 1000,
                zero_chunks: 3901,
            }),
            verify: Some(BackupVerify {
                at: 1_790_000_000,
                state: VerifyState::Failed,
                bad_chunks: 2,
                error: Some("2 chunks are damaged".into()),
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["base"], serde_json::json!(base));
        assert_eq!(json["store_stats"]["new_bytes"], 300u64 << 20);
        assert_eq!(
            json["verify"],
            serde_json::json!({ "at": 1_790_000_000u64, "state": "failed", "bad_chunks": 2, "error": "2 chunks are damaged" })
        );
        assert_eq!(status.backup_bytes(), 300 << 20);
        assert_eq!(BackupStatus::backup_bytes_value(&json), 300 << 20);
        assert_eq!(
            serde_json::from_value::<BackupStatus>(json).unwrap(),
            status
        );
        let ok = BackupVerify {
            at: 1,
            state: VerifyState::Ok,
            bad_chunks: 0,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            serde_json::json!({ "at": 1, "state": "ok" })
        );
        // 새 칸이 없으면 쓰지 않는다.
        let json = serde_json::to_value(BackupStatus::default()).unwrap();
        for k in ["base", "store_stats", "verify"] {
            assert!(json.get(k).is_none(), "{k}: {json}");
        }
    }

    #[test]
    fn bitmap_names_are_the_prefix_and_the_simple_backup_id() {
        let id: Uuid = "0190a2b3-c4d5-7e6f-8a9b-0c1d2e3f4a5b".parse().unwrap();
        assert_eq!(bitmap_name(id), "opl-0190a2b3c4d57e6f8a9b0c1d2e3f4a5b");
        assert!(bitmap_name(id).starts_with(BITMAP_PREFIX));
    }

    /// 백업 비트맵만 참이다. 같은 `opl-` 무리의 복제·마이그레이션 비트맵은 백업이 지우거나 부모로 쓰지 않는다.
    #[test]
    fn only_backup_bitmaps_are_backup_bitmaps() {
        let id = Uuid::now_v7();
        assert!(is_backup_bitmap(&bitmap_name(id)));
        let replica = crate::replication::replica_bitmap(Uuid::now_v7(), id);
        assert!(!is_backup_bitmap(&replica));
        assert!(!is_backup_bitmap(&crate::replication::migration_bitmap(
            id, 1
        )));
        for name in [
            "opl-x",
            "opl-",
            "user-bitmap",
            &bitmap_name(id).to_uppercase(),
        ] {
            assert!(!is_backup_bitmap(name), "{name}");
        }
        assert!(!is_backup_bitmap(&format!("{}0", bitmap_name(id))));
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

    /// TPM 상태(계획 59)는 담았을 때만 쓴다. 옛 매니페스트·상태도 그대로 읽고, 형식 버전은 바뀌지 않는다.
    #[test]
    fn tpm_state_is_written_only_when_backed_up() {
        let manifest = BackupManifest {
            version: MANIFEST_VERSION,
            backup: Uuid::now_v7(),
            parent: None,
            vm: Uuid::now_v7(),
            vm_name: "win".into(),
            vm_spec: vm_spec(),
            disks: vec![],
            uefi_vars: true,
            created_at: 1_790_000_000,
            tpm_state: false,
        };
        let json = serde_json::to_value(&manifest).unwrap();
        assert!(json.get("tpm_state").is_none(), "{json}");
        assert_eq!(
            serde_json::from_value::<BackupManifest>(json).unwrap(),
            manifest
        );

        let with_tpm = BackupManifest {
            tpm_state: true,
            ..manifest
        };
        let json = serde_json::to_value(&with_tpm).unwrap();
        assert_eq!(json["tpm_state"], true);
        assert_eq!(json["version"], MANIFEST_VERSION);
        assert_eq!(
            serde_json::from_value::<BackupManifest>(json).unwrap(),
            with_tpm
        );

        let status = BackupStatus::default();
        assert!(
            serde_json::to_value(&status)
                .unwrap()
                .get("tpm_state")
                .is_none()
        );
        let status = BackupStatus {
            tpm_state: true,
            ..status
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["tpm_state"], true);
        assert_eq!(
            serde_json::from_value::<BackupStatus>(json).unwrap(),
            status
        );
        assert_eq!(TPM_STATE_FILE, "tpm2-00.permall");
    }

    /// 계획 76 §3.3: 복원 맵은 NIC 네트워크·보안 그룹을 바꾸고(맵에 없는 이름은 그대로, 같은 대상은 한 번), cloud-init
    /// 주소·DNS를 넣는다(없던 cloud-init은 새로). 원본 이름이 백업에 없거나 주소 설정이 NIC보다 많으면 거절한다.
    #[test]
    fn restore_remap_rewrites_nics_and_cloud_init() {
        let mut spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 },
            "memory_mib": 512,
            "machine": "virt",
            "nics": [
                { "network": "lan", "model": "virtio", "security_groups": ["web", "ssh"] },
                { "network": "db", "model": "virtio", "security_groups": ["ssh"] }
            ]
        }))
        .unwrap();
        let remap: RestoreRemap = serde_json::from_value(serde_json::json!({
            "networks": { "lan": "dr-lan" },
            "security_groups": { "web": "dr-ssh", "ssh": "dr-ssh" },
            "cloud_init_networks": [ { "ipv4": { "mode": "static", "address": "10.0.0.5/24" } } ],
            "cloud_init_dns": { "servers": ["1.1.1.1"] }
        }))
        .unwrap();
        assert!(!remap.is_empty());
        assert!(RestoreRemap::default().is_empty());
        remap.check(&spec).unwrap();
        assert!(spec.cloud_init.is_none());
        remap.apply(&mut spec);
        let nets: Vec<&str> = spec.nics.iter().map(|n| n.network.as_str()).collect();
        assert_eq!(nets, ["dr-lan", "db"]);
        assert_eq!(spec.nics[0].security_groups, ["dr-ssh"]);
        assert_eq!(spec.nics[1].security_groups, ["dr-ssh"]);
        let ci = spec.cloud_init.as_ref().unwrap();
        assert_eq!(ci.networks, remap.cloud_init_networks.clone().unwrap());
        assert_eq!(ci.dns, remap.cloud_init_dns);
        // 빈 칸은 쓰지 않는다(Task JSON).
        assert_eq!(
            serde_json::to_value(RestoreRemap::default()).unwrap(),
            serde_json::json!({})
        );

        let spec = vm_spec();
        for (remap, want) in [
            (
                serde_json::json!({ "networks": { "nope": "x" } }),
                "no NIC on network \"nope\"",
            ),
            (
                serde_json::json!({ "security_groups": { "nope": "x" } }),
                "no NIC in security group \"nope\"",
            ),
            (
                serde_json::json!({ "cloud_init_networks": [{}, {}] }),
                "2 entries but the backed-up VM has 0 NICs",
            ),
        ] {
            let remap: RestoreRemap = serde_json::from_value(remap).unwrap();
            let err = remap.check(&spec).unwrap_err();
            assert!(err.contains(want), "{err}");
        }
    }
}
