//! VMware 이전 2(계획 72): 저장한 vCenter·ESXi 연결([`VmwareSource`]), 여러 VM을 한 번에 옮기는 이전 계획
//! ([`VmwareMigration`] — 매핑 규칙·동시 수 한도·동기화 주기·전환 정책), 계획 안 VM 하나의 이전 상태([`VmwareVm`] —
//! 디스크마다 CBT `changeId`·복사량, 우리 스냅샷, 바꾼 원본 설정의 옛 값, 전환 걸음).
//!
//! 연결 비밀번호는 Raft 스펙에만 둔다. API 보기·watch 이벤트는 [`redact_source_json`]으로 빼고, 감사는
//! [`VmwareSourceSpec::audit_summary`]만 적고, `Debug`(로그)는 `<redacted>`다. 계획 29·30의 `VmImport`는 그대로이고
//! 원본 VM 모양은 그 [`SourceVm`]을 함께 쓴다.

use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::cpu::CpuModel;
use crate::resource::{Kind, Resource, ResourceSpec, validate_name};
use crate::vm::{DiskBus, Firmware, NicModel};
use crate::vm_import::{SourceVm, normalize_fingerprint};

pub type VmwareSource = Resource<VmwareSourceSpec, VmwareSourceStatus>;
pub type VmwareMigration = Resource<VmwareMigrationSpec, VmwareMigrationStatus>;
pub type VmwareVm = Resource<VmwareVmSpec, VmwareVmStatus>;

/// 계획 하나의 VM 수 상한(넘으면 계획을 나눈다)
pub const MAX_MIGRATION_VMS: usize = 200;
/// 항목 `history`에 남기는 최근 사건 수
pub const MAX_HISTORY: usize = 20;
/// 대상 볼륨에 붙이는 레이블(값은 `vmware-vm` 항목 id). 노드 시작 sweep은 살아 있는 항목의 볼륨을 지우지 않는다.
pub const VMWARE_VM_LABEL: &str = "orbivisor.io/vmware-vm";
/// 원본 VM에 만드는 우리 스냅샷 이름의 접두(`orbivisor-migration-{항목 id 앞 8자}-{n}`). 우리 것인지는 Raft의 moref로만
/// 판단하고 이름은 표시용이다.
pub const SNAPSHOT_PREFIX: &str = "orbivisor-migration-";
/// 동기화가 연달아 이만큼 실패하면 항목이 `failed`다(설계 §9.5).
pub const MAX_SYNC_FAILURES: u32 = 3;

/// 비밀 문자열. JSON은 문자열 그대로(Raft에 저장)이고 `Debug`는 `<redacted>`다. API 보기·watch·감사에는 내지 않는다.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 원본에 로그인할 때만 쓴다(로그·응답에 넣지 않는다).
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

// ---- 원본 연결 ----

/// 저장한 vCenter·ESXi 연결(`vmware-source`). 만들기·바꾸기는 `cluster.admin`이다(관리자 비밀번호를 든다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VmwareSourceSpec {
    /// vCenter·ESXi 호스트 이름 또는 IP(포트·경로 없음)
    pub host: String,
    #[serde(default = "https_port")]
    #[cfg_attr(feature = "openapi", schema(default = 443))]
    pub port: u16,
    pub user: String,
    /// 비밀번호. Raft에만 두고 API 보기·watch·감사·로그에서 뺀다.
    #[cfg_attr(feature = "openapi", schema(value_type = String, write_only))]
    pub password: Secret,
    /// 믿기로 한 서버 인증서의 SHA-256 지문(대문자 콜론 구분 `AB:CD:…`, 계획 30과 같은 꼴)
    pub fingerprint: String,
    /// 디스크를 읽는 길. 없으면 `folder`(데이터스토어 `/folder` 범위 읽기)다.
    #[serde(default, skip_serializing_if = "VmwareTransport::is_folder")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub transport: VmwareTransport,
    /// 데이터센터 이름 힌트(ESXi는 `ha-datacenter`). 없으면 인벤토리에서 찾는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub datacenter_hint: Option<String>,
}

impl ResourceSpec for VmwareSourceSpec {
    const KIND: Kind = Kind::VmwareSource;
    type Status = VmwareSourceStatus;
}

fn https_port() -> u16 {
    443
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_zero32(n: &u32) -> bool {
    *n == 0
}

/// 디스크 읽기 길(설계 §7.4)
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmwareTransport {
    /// 데이터스토어 `/folder`의 HTTPS 범위 읽기(vCenter면 호스트 티켓으로 ESXi 호스트에 바로)
    #[default]
    Folder,
    /// 운영자가 설치한 nbdkit `vddk` 플러그인(vSAN·vVols에 필요)
    Vddk(VddkTransport),
}

impl VmwareTransport {
    pub fn is_folder(&self) -> bool {
        *self == VmwareTransport::Folder
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VddkTransport {
    /// VDDK 라이브러리 디렉터리. 없으면 노드 설정 `[vmware] vddk_libdir`다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub libdir: Option<String>,
    /// 서버 인증서의 SHA-1 썸프린트(VDDK가 받는 꼴, 대문자 콜론 구분). 만들 때 서버가 잰다.
    pub thumbprint_sha1: String,
}

/// 원본의 종류(`about.apiType`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum VmwareApiKind {
    /// `VirtualCenter`
    Vcenter,
    /// `HostAgent`(단독 ESXi)
    Esxi,
}

/// 만들기·"다시 확인"이 채우는 원본 상태
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmwareSourceStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub kind: Option<VmwareApiKind>,
    /// `about.version`(예: `8.0.2`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub version: Option<String>,
    /// `about.build`
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub build: Option<String>,
    /// `about.instanceUuid`(vCenter). 같은 vCenter인지 본다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub instance_uuid: Option<String>,
    /// 마지막으로 확인한 시각(유닉스 초)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub checked_at: Option<u64>,
    /// 마지막 확인이 실패한 이유
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 무료 ESXi(API 읽기 전용 — 스냅샷·설정 바꾸기가 거절되어 차가운 이전만)
    #[serde(skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub restricted: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VmwareError {
    #[error("host {0:?} must be a host name or IP address without scheme, port or path")]
    Host(String),
    #[error("port must not be 0")]
    Port,
    #[error("user must be 1-256 characters without control characters")]
    User,
    #[error("password must be 1-256 characters without control characters")]
    Password,
    #[error("fingerprint must be a SHA-256 certificate fingerprint (64 hex digits)")]
    Fingerprint,
    #[error(
        "transport.vddk.thumbprint_sha1 must be a SHA-1 certificate thumbprint (40 hex digits)"
    )]
    Thumbprint,
    #[error("transport.vddk.libdir must be an absolute path")]
    Libdir,
    #[error("datacenter_hint must be 1-80 characters without control characters")]
    Datacenter,
    #[error("{field} must be between {min} and {max}")]
    Range {
        field: &'static str,
        min: u64,
        max: u64,
    },
    #[error("{0} must be 1-256 characters without control characters")]
    Text(&'static str),
    #[error("rules.{0} maps {1:?} twice")]
    DuplicateRule(&'static str, String),
    #[error("target name: {0}")]
    TargetName(String),
}

/// 제어 문자 없는 1~`max` 바이트
fn plain(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len()) && !s.chars().any(char::is_control)
}

/// 16진 `len`자(콜론·공백 허용, 대소문자 무시)를 대문자 콜론 구분으로
fn colon_hex(text: &str, len: usize) -> Option<String> {
    let hex: Vec<char> = text
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .collect();
    if hex.len() != len || !hex.iter().all(char::is_ascii_hexdigit) {
        return None;
    }
    let pairs: Vec<String> = hex
        .chunks(2)
        .map(|p| p.iter().collect::<String>().to_ascii_uppercase())
        .collect();
    Some(pairs.join(":"))
}

/// SHA-1 썸프린트(16진 40자)를 VDDK가 받는 대문자 콜론 구분으로 바꾼다.
pub fn normalize_thumbprint_sha1(text: &str) -> Option<String> {
    colon_hex(text, 40)
}

impl VmwareSourceSpec {
    /// 검증하고 저장할 모양으로 바꾼다: 호스트는 앞뒤 공백을 떼고 소문자로, 지문·썸프린트는 대문자 콜론 구분으로.
    pub fn normalize(&mut self) -> Result<(), VmwareError> {
        let host = self.host.trim().to_ascii_lowercase();
        let host_ok = (1..=253).contains(&host.len())
            && (host.parse::<IpAddr>().is_ok()
                || host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.'));
        if !host_ok {
            return Err(VmwareError::Host(self.host.clone()));
        }
        self.host = host;
        if self.port == 0 {
            return Err(VmwareError::Port);
        }
        if !plain(&self.user, 256) {
            return Err(VmwareError::User);
        }
        if !plain(self.password.expose(), 256) {
            return Err(VmwareError::Password);
        }
        self.fingerprint =
            normalize_fingerprint(&self.fingerprint).ok_or(VmwareError::Fingerprint)?;
        if let VmwareTransport::Vddk(v) = &mut self.transport {
            v.thumbprint_sha1 =
                normalize_thumbprint_sha1(&v.thumbprint_sha1).ok_or(VmwareError::Thumbprint)?;
            if v.libdir.as_deref().is_some_and(|d| !d.starts_with('/')) {
                return Err(VmwareError::Libdir);
            }
        }
        if self
            .datacenter_hint
            .as_deref()
            .is_some_and(|d| !plain(d, 80))
        {
            return Err(VmwareError::Datacenter);
        }
        Ok(())
    }

    /// 감사 항목의 설명(`vcenter.example.com, user migr@vsphere.local, fingerprint AB:CD:…`). 비밀번호는 없다.
    pub fn audit_summary(&self) -> String {
        let short: String = self.fingerprint.chars().take(5).collect();
        let host = if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        let vddk = if self.transport.is_folder() {
            ""
        } else {
            ", vddk"
        };
        format!("{host}, user {}, fingerprint {short}…{vddk}", self.user)
    }
}

/// 원본 연결 스펙 JSON에서 비밀번호를 뺀다. API 보기·watch 이벤트가 쓴다.
pub fn redact_source_json(spec: &mut serde_json::Value) {
    if let Some(s) = spec.as_object_mut() {
        s.remove("password");
    }
}

// ---- 이전 계획 ----

/// 여러 VM을 같은 규칙으로 옮기는 이전 계획(`vmware-migration`). 진행은 항목([`VmwareVm`])과 Task에 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VmwareMigrationSpec {
    /// 원본 연결(`vmware-source` id)
    pub source: Uuid,
    pub mode: VmwareMigrationMode,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub rules: VmwareRules,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub limits: VmwareLimits,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub sync: VmwareSyncPolicy,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub cutover: VmwareCutoverPolicy,
    /// 계획 리소스가 사는 VM 폴더(권한용). 없으면 루트다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub folder: Option<Uuid>,
}

impl ResourceSpec for VmwareMigrationSpec {
    const KIND: Kind = Kind::VmwareMigration;
    type Status = VmwareMigrationStatus;
}

impl VmwareMigrationSpec {
    pub fn validate(&self) -> Result<(), VmwareError> {
        self.rules.validate()?;
        self.limits.validate()?;
        self.sync.validate()?;
        self.cutover.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmwareMigrationMode {
    /// 켜진 원본을 스냅샷 위에서 복사하고 CBT 델타를 반복해 옮긴 뒤 전환 때만 끈다.
    Warm,
    /// 꺼진 원본을 스냅샷 없이 통째로 복사한다.
    Cold,
}

/// 매핑 규칙(설계 §8.4 — VM 덮어쓰기 > 규칙 > 제안 `propose`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default, deny_unknown_fields)]
pub struct VmwareRules {
    /// 원본 네트워크(이름 또는 `moref:dvportgroup-12`) → 우리 네트워크
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<VmwareMapping>,
    /// 원본 데이터스토어 이름 → 우리 풀(디스크마다)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub datastores: Vec<VmwareMapping>,
    /// 규칙에 없는 데이터스토어의 디스크를 둘 풀
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub default_pool: Option<String>,
    pub folders: VmwareFolderRule,
    /// 대상 노드. 없으면 시작할 때 고른다(풀이 보이고 메모리 여유).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub node: Option<Uuid>,
    /// 디스크 버스·NIC 모델을 모든 VM에 고정한다. 없으면 게스트 OS로 고른다(`propose`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub devices: Option<VmwareDevices>,
    /// 원본 MAC 주소를 그대로 쓴다.
    pub keep_macs: bool,
    /// 대상 네트워크가 SDN이면 VMware Tools가 알린 IP를 그 주소로 예약한다.
    pub reserve_ips: bool,
    /// Windows VM에 CD로 붙일 드라이버 ISO 볼륨(virtio-win)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub driver_iso: Option<String>,
    /// 전환 끝에 대상 VM을 켠다.
    pub start_after: bool,
    /// 대상 VM의 CPU 모델. 없으면 `host`다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub cpu_model: Option<CpuModel>,
}

impl Default for VmwareRules {
    fn default() -> Self {
        Self {
            networks: Vec::new(),
            datastores: Vec::new(),
            default_pool: None,
            folders: VmwareFolderRule::default(),
            node: None,
            devices: None,
            keep_macs: true,
            reserve_ips: true,
            driver_iso: None,
            start_after: true,
            cpu_model: None,
        }
    }
}

impl VmwareRules {
    pub fn validate(&self) -> Result<(), VmwareError> {
        for (field, list) in [
            ("networks", &self.networks),
            ("datastores", &self.datastores),
        ] {
            let mut seen = std::collections::BTreeSet::new();
            for m in list {
                if !plain(&m.from, 256) || !plain(&m.to, 256) {
                    return Err(VmwareError::Text(field));
                }
                if !seen.insert(m.from.as_str()) {
                    return Err(VmwareError::DuplicateRule(field, m.from.clone()));
                }
            }
        }
        for (field, v) in [
            ("rules.default_pool", &self.default_pool),
            ("rules.driver_iso", &self.driver_iso),
        ] {
            if v.as_deref().is_some_and(|s| !plain(s, 256)) {
                return Err(VmwareError::Text(field));
            }
        }
        Ok(())
    }

    /// 원본 네트워크의 대상(규칙에 없으면 None)
    pub fn network_for(&self, from: &str) -> Option<&str> {
        self.networks
            .iter()
            .find(|m| m.from == from)
            .map(|m| m.to.as_str())
    }

    /// 원본 데이터스토어의 풀(규칙 → `default_pool`)
    pub fn pool_for(&self, datastore: &str) -> Option<&str> {
        self.datastores
            .iter()
            .find(|m| m.from == datastore)
            .map(|m| m.to.as_str())
            .or(self.default_pool.as_deref())
    }
}

/// 규칙 한 줄: 원본 이름 → 우리 이름
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VmwareMapping {
    pub from: String,
    pub to: String,
}

/// 만든 VM을 둘 폴더
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum VmwareFolderRule {
    /// 모두 한 폴더에(없으면 VM 트리 루트)
    Flat {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        into: Option<Uuid>,
    },
    /// VMware VM 폴더 경로를 이 폴더(없으면 루트) 아래에 만들어 그대로 둔다.
    Mirror {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        under: Option<Uuid>,
    },
}

impl Default for VmwareFolderRule {
    fn default() -> Self {
        VmwareFolderRule::Flat { into: None }
    }
}

/// 고정한 장치
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VmwareDevices {
    pub disk_bus: DiskBus,
    pub nic_model: NicModel,
}

/// 동시 수·대역 한도(설계 §9.3·§9.4)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default, deny_unknown_fields)]
pub struct VmwareLimits {
    /// 계획의 끝나지 않은 동기화·전환 Task 수(1~64)
    pub max_parallel: u16,
    /// 같은 ESXi 호스트의 VM(1~64)
    pub max_per_host: u16,
    /// 같은 대상 노드(1~64)
    pub max_per_node: u16,
    /// 디스크마다 동시 범위 읽기(1~16)
    pub streams_per_disk: u8,
    /// 계획 전체 대역(MiB/s). 없으면 한도 없음.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub bandwidth_mib_s: Option<u32>,
}

impl Default for VmwareLimits {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            max_per_host: 2,
            max_per_node: 2,
            streams_per_disk: 4,
            bandwidth_mib_s: None,
        }
    }
}

fn range(field: &'static str, v: u64, min: u64, max: u64) -> Result<(), VmwareError> {
    if (min..=max).contains(&v) {
        Ok(())
    } else {
        Err(VmwareError::Range { field, min, max })
    }
}

impl VmwareLimits {
    pub fn validate(&self) -> Result<(), VmwareError> {
        range("limits.max_parallel", self.max_parallel.into(), 1, 64)?;
        range("limits.max_per_host", self.max_per_host.into(), 1, 64)?;
        range("limits.max_per_node", self.max_per_node.into(), 1, 64)?;
        range(
            "limits.streams_per_disk",
            self.streams_per_disk.into(),
            1,
            16,
        )?;
        if let Some(b) = self.bandwidth_mib_s {
            range("limits.bandwidth_mib_s", b.into(), 1, 100_000)?;
        }
        Ok(())
    }
}

/// 따뜻한 이전의 동기화 주기
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default, deny_unknown_fields)]
pub struct VmwareSyncPolicy {
    /// 델타 회차 사이(분, 5~1440)
    pub interval_min: u32,
    /// 이 날 수가 지나도록 전환하지 않으면 동기화를 멈춘다(원본에 스냅샷이 오래 남지 않게, 0은 멈추지 않음, 최대 365).
    pub stop_after_days: u32,
}

impl Default for VmwareSyncPolicy {
    fn default() -> Self {
        Self {
            interval_min: 60,
            stop_after_days: 14,
        }
    }
}

impl VmwareSyncPolicy {
    pub fn validate(&self) -> Result<(), VmwareError> {
        range("sync.interval_min", self.interval_min.into(), 5, 1440)?;
        range("sync.stop_after_days", self.stop_after_days.into(), 0, 365)
    }
}

/// 전환 정책(설계 §10.1)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default, deny_unknown_fields)]
pub struct VmwareCutoverPolicy {
    /// 예약 전환 시각(유닉스 초). 그 시각에 `ready`·`syncing` 항목을 모두 전환한다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub at: Option<u64>,
    pub shutdown: VmwareShutdown,
    /// 게스트 종료를 기다리는 시간(초, 30~3600)
    pub shutdown_timeout_s: u32,
    /// 원본 NIC를 켤 때 끊긴 채로(`startConnected=false`) 두고 설명을 단다(실수로 켜도 같은 MAC·IP가 붙지 않게).
    pub protect_source: bool,
    /// 마지막 델타가 이보다 크면 원본을 끄기 전에 멈추고 알린다(GiB).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub max_final_delta_gib: Option<u32>,
}

impl Default for VmwareCutoverPolicy {
    fn default() -> Self {
        Self {
            at: None,
            shutdown: VmwareShutdown::Guest,
            shutdown_timeout_s: 600,
            protect_source: true,
            max_final_delta_gib: None,
        }
    }
}

impl VmwareCutoverPolicy {
    pub fn validate(&self) -> Result<(), VmwareError> {
        range(
            "cutover.shutdown_timeout_s",
            self.shutdown_timeout_s.into(),
            30,
            3600,
        )?;
        if let Some(g) = self.max_final_delta_gib {
            range("cutover.max_final_delta_gib", g.into(), 1, 1 << 20)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmwareShutdown {
    /// `ShutdownGuest`(VMware Tools가 있어야 한다)
    #[default]
    Guest,
    /// `PowerOffVM_Task`
    PowerOff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmwareMigrationStatus {
    pub phase: VmwareMigrationPhase,
    /// 항목 상태(`VmwareVmState::as_str`)별 VM 수
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub counts: BTreeMap<String, u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmwareMigrationPhase {
    /// 만들었다(미리 확인만, 시작 전)
    #[default]
    Draft,
    Running,
    Paused,
    /// 모든 항목이 끝났다.
    Finished,
    Cancelled,
}

impl VmwareMigrationPhase {
    /// 끝난 계획(지울 수 있다)
    pub fn is_done(self) -> bool {
        matches!(
            self,
            VmwareMigrationPhase::Finished | VmwareMigrationPhase::Cancelled
        )
    }
}

// ---- 계획 안 VM 하나 ----

/// 이전 계획 안의 VM 하나(`vmware-vm`, 이름 [`item_name`]). 동시 Task가 계획 리소스 하나를 서로 고쳐 revision이
/// 부딪치지 않게, 권한(대상 폴더)과 watch가 VM마다 오게 따로 둔다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VmwareVmSpec {
    /// 계획(`vmware-migration` id)
    pub migration: Uuid,
    /// 원본 VM의 managed object id(`vm-123`)
    pub source_ref: String,
    /// 원본 VM의 `config.instanceUuid`(moref가 바뀌어도 같은 VM인지 — 끝나지 않은 항목은 클러스터에 하나)
    pub instance_uuid: String,
    /// 만들 VM 이름
    pub target_name: String,
    /// 이 VM만의 덮어쓰기(규칙보다 앞선다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub overrides: Option<VmwareOverrides>,
    /// 대상 노드(미리 확인·시작 때 정한다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub node: Option<Uuid>,
}

impl ResourceSpec for VmwareVmSpec {
    const KIND: Kind = Kind::VmwareVm;
    type Status = VmwareVmStatus;
}

impl VmwareVmSpec {
    pub fn validate(&self) -> Result<(), VmwareError> {
        if !plain(&self.source_ref, 80) {
            return Err(VmwareError::Text("source_ref"));
        }
        if !plain(&self.instance_uuid, 80) {
            return Err(VmwareError::Text("instance_uuid"));
        }
        validate_name(&self.target_name).map_err(|e| VmwareError::TargetName(e.to_string()))
    }
}

/// VM 하나의 덮어쓰기(`ImportRun`의 부분 값). 비운 칸은 규칙·제안을 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default, deny_unknown_fields)]
pub struct VmwareOverrides {
    /// 디스크 순번별 풀(비면 데이터스토어 규칙)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pools: Vec<String>,
    /// 원본 네트워크 → 우리 네트워크
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub networks: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub disk_bus: Option<DiskBus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub nic_model: Option<NicModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub firmware: Option<Firmware>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub sockets: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub cores: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub memory_mib: Option<u32>,
    /// 원본 NIC 순번별 SDN 주소
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Vec<String>>))]
    pub ips: Vec<Vec<IpAddr>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub keep_macs: Option<bool>,
    /// 만든 VM을 둘 폴더(폴더 규칙보다 앞선다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub folder: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub cpu_model: Option<CpuModel>,
}

/// VM 하나의 이전 상태(설계 §6). 진행 걸음마다 Raft에 먼저 적고 한다(이어 가기·되돌리기).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmwareVmStatus {
    pub state: VmwareVmState,
    /// `state`가 `cutting-over`일 때 지금 걸음
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cutover_step: Option<VmwareCutoverStep>,
    /// 원본 VM(인벤토리에서 읽은 것)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub source: Option<VmwareSourceVm>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<VmwareSyncDisk>,
    /// 원본에 있는 우리 스냅샷(기반 flat 파일이 이 시점에 얼어 있다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub snapshot: Option<VmwareSnapshotRef>,
    /// 만들었지만 옛 스냅샷을 아직 지우지 못한 새 스냅샷(회차 1·2단계 사이 — 다음 시도가 먼저 옛 것을 지운다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub pending_snapshot: Option<VmwareSnapshotRef>,
    /// 원본의 CBT를 우리가 켰다(마무리·취소가 끈다).
    #[serde(skip_serializing_if = "is_false")]
    pub cbt_enabled_by_us: bool,
    /// 끝낸 회차 수
    #[serde(skip_serializing_if = "is_zero32")]
    pub rounds: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_sync_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub next_sync_at: Option<u64>,
    /// 예상 마지막 델타 시간(초 — 마지막 델타 바이트 ÷ 잰 속도)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub estimate_s: Option<u64>,
    /// 만든 VM
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub target_vm: Option<Uuid>,
    /// 바꾼 원본 설정(되돌림·취소가 되돌린다)
    #[serde(skip_serializing_if = "VmwareSourceChanges::is_empty")]
    pub source_changes: VmwareSourceChanges,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 최근 사건([`MAX_HISTORY`]개까지, 오래된 것부터)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<VmwareHistory>,
    /// 첫 동기화를 시작한 시각(유닉스 초 — `sync.stop_after_days`를 센다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub started_at: Option<u64>,
    /// 연달아 실패한 동기화 수(성공하면 0 — [`MAX_SYNC_FAILURES`]번이면 `failed`)
    #[serde(skip_serializing_if = "is_zero32")]
    pub sync_failures: u32,
    /// 전환이 원본을 끄기 시작한 시각(유닉스 초 — 내린 시간을 센다, 설계 §10.1)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub source_stopped_at: Option<u64>,
    /// 마지막 전환의 내린 시간(초 — 원본 끄기 시작 → 대상 VM 켜짐)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub downtime_s: Option<u64>,
    /// 마지막 전환 요청 시각(유닉스 초 — 예약 전환은 그 시각 뒤 한 번만 한다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cutover_requested_at: Option<u64>,
}

impl VmwareVmStatus {
    /// 사건을 더하고 오래된 것을 [`MAX_HISTORY`]개까지 버린다.
    pub fn push_history(&mut self, at: u64, event: impl Into<String>) {
        self.history.push(VmwareHistory {
            at,
            event: event.into(),
        });
        let extra = self.history.len().saturating_sub(MAX_HISTORY);
        self.history.drain(..extra);
    }
}

/// 항목 상태(설계 §6). `Ready`는 차가운 이전의 복사 끝, 따뜻한 이전은 첫 델타 뒤 `Syncing`과 같은 뜻의 화면 이름이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmwareVmState {
    /// `Draft` 계획의 항목(미리 확인만)
    #[default]
    Draft,
    /// 미리 확인을 통과해 차례를 기다린다.
    Pending,
    /// 전체 복사
    Copying,
    /// 따뜻한 이전: 델타 주기 사이
    Syncing,
    /// 전환을 기다린다.
    Ready,
    /// 전환 중(걸음은 `cutover_step`)
    CuttingOver,
    /// 대상 VM이 켜지고 원본은 꺼졌다(되돌릴 수 있다).
    Done,
    /// 우리 스냅샷·CBT를 정리했다(되돌릴 수 없다).
    Finalized,
    /// 다시 할 수 있다(이유는 `error`).
    Failed,
    /// 전환이 실패해 원본을 다시 켰다(다음 회차가 잇는다).
    RolledBack,
    /// `Done` 뒤 손으로 되돌렸다.
    Reverted,
    /// 취소했다(우리 스냅샷·대상 볼륨을 지움 — 원본 그대로).
    Cancelled,
}

impl VmwareVmState {
    pub const ALL: [VmwareVmState; 12] = [
        VmwareVmState::Draft,
        VmwareVmState::Pending,
        VmwareVmState::Copying,
        VmwareVmState::Syncing,
        VmwareVmState::Ready,
        VmwareVmState::CuttingOver,
        VmwareVmState::Done,
        VmwareVmState::Finalized,
        VmwareVmState::Failed,
        VmwareVmState::RolledBack,
        VmwareVmState::Reverted,
        VmwareVmState::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            VmwareVmState::Draft => "draft",
            VmwareVmState::Pending => "pending",
            VmwareVmState::Copying => "copying",
            VmwareVmState::Syncing => "syncing",
            VmwareVmState::Ready => "ready",
            VmwareVmState::CuttingOver => "cutting-over",
            VmwareVmState::Done => "done",
            VmwareVmState::Finalized => "finalized",
            VmwareVmState::Failed => "failed",
            VmwareVmState::RolledBack => "rolled-back",
            VmwareVmState::Reverted => "reverted",
            VmwareVmState::Cancelled => "cancelled",
        }
    }

    /// 끝난 항목: 원본 VM을 더 잡지 않는다(같은 VM을 새 계획에 넣을 수 있다).
    pub fn is_final(self) -> bool {
        matches!(
            self,
            VmwareVmState::Finalized | VmwareVmState::Reverted | VmwareVmState::Cancelled
        )
    }
}

/// 전환 걸음(설계 §10.1). 각 걸음은 Raft에 먼저 적고 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmwareCutoverStep {
    Preparing,
    StoppingSource,
    FinalSync,
    CreatingVm,
    StartingVm,
}

impl VmwareCutoverStep {
    /// 이 걸음에서 실패하면 원본을 다시 켜서 되돌린다(원본을 끄기 시작한 뒤).
    pub fn rolls_back(self) -> bool {
        self >= VmwareCutoverStep::FinalSync
    }
}

/// 원본 VM(계획 29 `SourceVm` + 인벤토리의 실행 정보)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmwareSourceVm {
    pub vm: SourceVm,
    /// VM이 있는 ESXi 호스트 이름
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub host: Option<String>,
    /// VMware Tools 상태(`guestToolsRunning` 등)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub tools: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub power: Option<VmwarePower>,
    /// Tools가 알린 MAC → IP
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = BTreeMap<String, Vec<String>>, default))]
    pub guest_ips: BTreeMap<String, Vec<IpAddr>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmwarePower {
    PoweredOn,
    PoweredOff,
    Suspended,
}

/// 디스크 하나의 동기화
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmwareSyncDisk {
    /// 원본 장치 키
    pub key: i32,
    /// 기반 flat 파일(`[ds] web/web-flat.vmdk`)
    pub file: String,
    pub capacity_bytes: u64,
    /// 대상 볼륨(첫 동기화가 만들어 먼저 적는다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub volume: Option<Uuid>,
    /// 마지막 회차 스냅샷의 CBT `changeId`(다음 델타의 시작)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub change_id: Option<String>,
    /// 지금 회차에서 복사한 바이트(체크포인트)
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub copied_bytes: u64,
    #[serde(default, skip_serializing_if = "is_zero32")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub rounds: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub last_delta_bytes: u64,
    /// 차가운 이전: 복사를 시작할 때 flat 파일의 수정 시각(전환이 다시 봐서 그새 켜졌는지 안다 — 설계 §9.2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub source_mtime: Option<String>,
}

/// 원본의 우리 스냅샷
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmwareSnapshotRef {
    /// managed object id(`snapshot-7`). 지우기는 이 값에만 한다.
    pub moref: String,
    /// 표시용 이름([`snapshot_name`])
    pub name: String,
    /// 만든 시각(유닉스 초)
    pub created_at: u64,
}

/// 우리가 바꾼 원본 설정. 바꾸기 전에 옛 값을 먼저 적는다.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmwareSourceChanges {
    /// 전환이 원본을 껐다(되돌림이 켠다).
    #[serde(skip_serializing_if = "is_false")]
    pub powered_off_by_us: bool,
    /// `startConnected=false`로 바꾼 NIC 장치 키
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nics_disconnected: Vec<i32>,
    /// 설명(annotation)을 바꿨다.
    #[serde(skip_serializing_if = "is_false")]
    pub annotation_added: bool,
    /// 바꾸기 전 설명(되돌림이 되살린다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub old_annotation: Option<String>,
}

impl VmwareSourceChanges {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmwareHistory {
    /// 유닉스 초
    pub at: u64,
    pub event: String,
}

/// 원본 이름을 리소스 이름 조각으로 다듬는다: 소문자, 영문·숫자가 아니면 `-`, 겹친 `-`와 앞뒤 `-`를 떼고 `max`자까지.
/// 남는 것이 없으면 `vm`이다.
pub fn name_part(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.truncate(max);
    let out = out.trim_matches('-');
    if out.is_empty() {
        "vm".into()
    } else {
        out.to_string()
    }
}

/// 항목 리소스 이름 `{계획}-{원본 이름 다듬음}`(63자까지)
pub fn item_name(migration: &str, source_name: &str) -> String {
    let room = 63usize.saturating_sub(migration.len() + 1).max(1);
    format!("{migration}-{}", name_part(source_name, room))
}

/// 우리 스냅샷 이름 `orbivisor-migration-{항목 id 앞 8자}-{n}`
pub fn snapshot_name(item: Uuid, round: u32) -> String {
    let id = item.simple().to_string();
    format!("{SNAPSHOT_PREFIX}{}-{round}", &id[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm_import::{SourceDisk, SourceNic};
    use serde_json::json;

    /// 시험 경우 하나의 고치기
    type Change<T> = Box<dyn Fn(&mut T)>;

    const FP: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";

    fn source() -> VmwareSourceSpec {
        VmwareSourceSpec {
            host: "vcenter.example.com".into(),
            port: 443,
            user: "migr@vsphere.local".into(),
            password: Secret::new("Sup3r s3cret!"),
            fingerprint: FP.into(),
            transport: VmwareTransport::Folder,
            datacenter_hint: None,
        }
    }

    #[test]
    fn kinds_are_named() {
        assert_eq!(Kind::VmwareSource.as_str(), "vmware-source");
        assert_eq!(Kind::VmwareMigration.as_str(), "vmware-migration");
        assert_eq!(Kind::VmwareVm.as_str(), "vmware-vm");
        assert_eq!(
            serde_json::to_value(Kind::VmwareVm).unwrap(),
            json!("vmware-vm")
        );
    }

    /// 원본 연결: 빈 값은 쓰지 않고, 비밀번호는 Raft JSON에만 있고 `Debug`·가린 JSON·감사 설명에는 없다.
    #[test]
    fn source_round_trips_and_hides_the_password() {
        let spec = source();
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v,
            json!({ "host": "vcenter.example.com", "port": 443, "user": "migr@vsphere.local",
                    "password": "Sup3r s3cret!", "fingerprint": FP })
        );
        assert_eq!(
            serde_json::from_value::<VmwareSourceSpec>(v.clone()).unwrap(),
            spec
        );
        let debug = format!("{spec:?}");
        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        let mut redacted = v;
        redact_source_json(&mut redacted);
        assert!(!redacted.to_string().contains("s3cret"), "{redacted}");
        assert_eq!(redacted["user"], "migr@vsphere.local");
        let summary = spec.audit_summary();
        assert_eq!(
            summary,
            "vcenter.example.com, user migr@vsphere.local, fingerprint AB:CD…"
        );
        // 기본값과 모르는 칸
        let s: VmwareSourceSpec = serde_json::from_value(json!({
            "host": "esx1", "user": "root", "password": "p", "fingerprint": FP
        }))
        .unwrap();
        assert_eq!(
            (s.port, s.transport.clone()),
            (443, VmwareTransport::Folder)
        );
        assert!(
            serde_json::from_value::<VmwareSourceSpec>(json!({
                "host": "esx1", "user": "root", "password": "p", "fingerprint": FP, "x": 1
            }))
            .is_err()
        );
        // 상태: 빈 값은 쓰지 않는다.
        assert_eq!(
            serde_json::to_value(VmwareSourceStatus::default()).unwrap(),
            json!({})
        );
        let st = VmwareSourceStatus {
            kind: Some(VmwareApiKind::Vcenter),
            version: Some("8.0.2".into()),
            restricted: true,
            ..Default::default()
        };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(
            v,
            json!({ "kind": "vcenter", "version": "8.0.2", "restricted": true })
        );
        assert_eq!(serde_json::from_value::<VmwareSourceStatus>(v).unwrap(), st);
    }

    #[test]
    fn vddk_transport_is_tagged() {
        let mut spec = source();
        spec.port = 8443;
        spec.transport = VmwareTransport::Vddk(VddkTransport {
            libdir: None,
            thumbprint_sha1: "ab".repeat(20),
        });
        spec.normalize().unwrap();
        let v = serde_json::to_value(&spec).unwrap();
        let VmwareTransport::Vddk(t) = &spec.transport else {
            panic!()
        };
        assert_eq!(t.thumbprint_sha1, ["AB"; 20].join(":"));
        assert_eq!(
            v["transport"],
            json!({ "vddk": { "thumbprint_sha1": t.thumbprint_sha1 } })
        );
        assert_eq!(serde_json::from_value::<VmwareSourceSpec>(v).unwrap(), spec);
        assert_eq!(
            spec.audit_summary(),
            "vcenter.example.com:8443, user migr@vsphere.local, fingerprint AB:CD…, vddk"
        );
    }

    #[test]
    fn source_normalizes_and_validates() {
        let mut s = source();
        s.host = "  VCenter.Example.com ".into();
        s.fingerprint = FP.replace(':', "").to_ascii_lowercase();
        s.normalize().unwrap();
        assert_eq!(s.host, "vcenter.example.com");
        assert_eq!(s.fingerprint, FP);
        let cases: Vec<(Change<VmwareSourceSpec>, VmwareError)> = vec![
            (
                Box::new(|s| s.host = "https://vc".into()),
                VmwareError::Host("https://vc".into()),
            ),
            (
                Box::new(|s| s.host = "vc:443".into()),
                VmwareError::Host("vc:443".into()),
            ),
            (Box::new(|s| s.port = 0), VmwareError::Port),
            (Box::new(|s| s.user = String::new()), VmwareError::User),
            (
                Box::new(|s| s.password = Secret::new("a\nb")),
                VmwareError::Password,
            ),
            (
                Box::new(|s| s.fingerprint = "AB:CD".into()),
                VmwareError::Fingerprint,
            ),
            (
                Box::new(|s| {
                    s.transport = VmwareTransport::Vddk(VddkTransport {
                        libdir: None,
                        thumbprint_sha1: "ab".into(),
                    })
                }),
                VmwareError::Thumbprint,
            ),
            (
                Box::new(|s| {
                    s.transport = VmwareTransport::Vddk(VddkTransport {
                        libdir: Some("opt/vddk".into()),
                        thumbprint_sha1: "ab".repeat(20),
                    })
                }),
                VmwareError::Libdir,
            ),
            (
                Box::new(|s| s.datacenter_hint = Some(String::new())),
                VmwareError::Datacenter,
            ),
        ];
        for (change, want) in cases {
            let mut s = source();
            change(&mut s);
            assert_eq!(s.normalize(), Err(want));
        }
        let mut v6 = source();
        v6.host = "fd00::10".into();
        assert_eq!(v6.normalize(), Ok(()));
    }

    /// 계획: 기본값을 채워 받고, 쓰는 JSON은 한도·정책을 늘 담고 빈 규칙은 쓰지 않는다.
    #[test]
    fn migration_defaults_and_round_trip() {
        let source = Uuid::now_v7();
        let spec: VmwareMigrationSpec =
            serde_json::from_value(json!({ "source": source, "mode": "warm" })).unwrap();
        assert_eq!(spec.limits, VmwareLimits::default());
        assert_eq!(spec.limits.max_parallel, 4);
        assert_eq!(spec.sync.interval_min, 60);
        assert_eq!(spec.sync.stop_after_days, 14);
        assert_eq!(spec.cutover.shutdown, VmwareShutdown::Guest);
        assert_eq!(spec.cutover.shutdown_timeout_s, 600);
        assert!(spec.cutover.protect_source);
        assert!(spec.rules.keep_macs && spec.rules.reserve_ips && spec.rules.start_after);
        assert_eq!(spec.rules.folders, VmwareFolderRule::Flat { into: None });
        assert_eq!(spec.validate(), Ok(()));
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v,
            json!({
                "source": source, "mode": "warm",
                "rules": { "folders": { "kind": "flat" }, "keep_macs": true, "reserve_ips": true, "start_after": true },
                "limits": { "max_parallel": 4, "max_per_host": 2, "max_per_node": 2, "streams_per_disk": 4 },
                "sync": { "interval_min": 60, "stop_after_days": 14 },
                "cutover": { "shutdown": "guest", "shutdown_timeout_s": 600, "protect_source": true }
            })
        );
        assert_eq!(
            serde_json::from_value::<VmwareMigrationSpec>(v).unwrap(),
            spec
        );

        let folder = Uuid::now_v7();
        let full: VmwareMigrationSpec = serde_json::from_value(json!({
            "source": source, "mode": "cold", "folder": folder,
            "rules": {
                "networks": [{ "from": "VM Network", "to": "lan" }, { "from": "moref:dvportgroup-12", "to": "dmz" }],
                "datastores": [{ "from": "ds1", "to": "ceph" }], "default_pool": "local",
                "folders": { "kind": "mirror", "under": folder },
                "devices": { "disk_bus": "sata", "nic_model": "e1000e" },
                "keep_macs": false, "driver_iso": "virtio-win", "cpu_model": "x86-64-v2"
            },
            "limits": { "max_parallel": 8, "bandwidth_mib_s": 200 },
            "sync": { "interval_min": 30, "stop_after_days": 0 },
            "cutover": { "at": 1_790_000_000u64, "shutdown": "power-off", "max_final_delta_gib": 50 }
        }))
        .unwrap();
        assert_eq!(full.validate(), Ok(()));
        assert_eq!(full.rules.network_for("VM Network"), Some("lan"));
        assert_eq!(full.rules.network_for("x"), None);
        assert_eq!(full.rules.pool_for("ds1"), Some("ceph"));
        assert_eq!(full.rules.pool_for("ds2"), Some("local"));
        assert_eq!(full.limits.max_per_host, 2);
        let v = serde_json::to_value(&full).unwrap();
        assert_eq!(
            v["rules"]["folders"],
            json!({ "kind": "mirror", "under": folder })
        );
        assert_eq!(v["cutover"]["shutdown"], "power-off");
        assert_eq!(
            serde_json::from_value::<VmwareMigrationSpec>(v).unwrap(),
            full
        );
        // 모르는 칸은 어디서든 거절
        for bad in [
            json!({ "source": source, "mode": "warm", "x": 1 }),
            json!({ "source": source, "mode": "warm", "rules": { "x": 1 } }),
            json!({ "source": source, "mode": "warm", "limits": { "x": 1 } }),
            json!({ "source": source, "mode": "hot" }),
        ] {
            assert!(
                serde_json::from_value::<VmwareMigrationSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn migration_validates_ranges_and_rules() {
        let base: VmwareMigrationSpec =
            serde_json::from_value(json!({ "source": Uuid::nil(), "mode": "warm" })).unwrap();
        let cases: Vec<(Change<VmwareMigrationSpec>, &str)> = vec![
            (
                Box::new(|s| s.limits.max_parallel = 0),
                "limits.max_parallel",
            ),
            (
                Box::new(|s| s.limits.max_per_node = 65),
                "limits.max_per_node",
            ),
            (
                Box::new(|s| s.limits.streams_per_disk = 17),
                "limits.streams_per_disk",
            ),
            (
                Box::new(|s| s.limits.bandwidth_mib_s = Some(0)),
                "limits.bandwidth_mib_s",
            ),
            (Box::new(|s| s.sync.interval_min = 4), "sync.interval_min"),
            (
                Box::new(|s| s.sync.stop_after_days = 366),
                "sync.stop_after_days",
            ),
            (
                Box::new(|s| s.cutover.shutdown_timeout_s = 10),
                "cutover.shutdown_timeout_s",
            ),
            (
                Box::new(|s| {
                    s.rules.networks = vec![
                        VmwareMapping {
                            from: "a".into(),
                            to: "x".into(),
                        },
                        VmwareMapping {
                            from: "a".into(),
                            to: "y".into(),
                        },
                    ]
                }),
                "maps \"a\" twice",
            ),
            (
                Box::new(|s| {
                    s.rules.datastores = vec![VmwareMapping {
                        from: String::new(),
                        to: "x".into(),
                    }]
                }),
                "datastores",
            ),
        ];
        for (change, want) in cases {
            let mut s = base.clone();
            change(&mut s);
            let err = s.validate().unwrap_err().to_string();
            assert!(err.contains(want), "{err} / {want}");
        }
    }

    fn source_vm() -> SourceVm {
        SourceVm {
            name: "web01".into(),
            guest_os: "rhel9_64Guest".into(),
            firmware: Firmware::Uefi,
            vcpus: 2,
            cores_per_socket: 1,
            memory_mib: 4096,
            disks: vec![SourceDisk {
                name: "disk0".into(),
                file: "[ds1] web01/web01.vmdk".into(),
                capacity_bytes: 16 << 30,
                controller: "scsi".into(),
            }],
            nics: vec![SourceNic {
                network: "VM Network".into(),
                adapter: "vmxnet3".into(),
                mac: Some("00:50:56:01:02:03".parse().unwrap()),
            }],
        }
    }

    /// 항목: 처음(`Draft`)은 거의 빈 상태이고, 진행 칸은 값이 있을 때만 쓴다.
    #[test]
    fn item_round_trips() {
        let migration = Uuid::now_v7();
        let spec = VmwareVmSpec {
            migration,
            source_ref: "vm-123".into(),
            instance_uuid: "5003a1b2-0000-0000-0000-000000000001".into(),
            target_name: "web01".into(),
            overrides: None,
            node: None,
        };
        assert_eq!(spec.validate(), Ok(()));
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v,
            json!({ "migration": migration, "source_ref": "vm-123",
                    "instance_uuid": "5003a1b2-0000-0000-0000-000000000001", "target_name": "web01" })
        );
        assert_eq!(serde_json::from_value::<VmwareVmSpec>(v).unwrap(), spec);
        let mut bad = spec.clone();
        bad.target_name = "Web 01".into();
        assert!(matches!(bad.validate(), Err(VmwareError::TargetName(_))));

        let over: VmwareOverrides = serde_json::from_value(json!({
            "pools": ["ceph"], "networks": { "VM Network": "lan" }, "disk_bus": "virtio-blk",
            "memory_mib": 8192, "ips": [["10.0.0.5"]], "keep_macs": false
        }))
        .unwrap();
        assert_eq!(over.memory_mib, Some(8192));
        let v = serde_json::to_value(&over).unwrap();
        assert_eq!(serde_json::from_value::<VmwareOverrides>(v).unwrap(), over);
        assert_eq!(
            serde_json::to_value(VmwareOverrides::default()).unwrap(),
            json!({})
        );
        assert!(serde_json::from_value::<VmwareOverrides>(json!({ "pool": "x" })).is_err());

        assert_eq!(
            serde_json::to_value(VmwareVmStatus::default()).unwrap(),
            json!({ "state": "draft" })
        );
        let volume = Uuid::now_v7();
        let mut status = VmwareVmStatus {
            state: VmwareVmState::CuttingOver,
            cutover_step: Some(VmwareCutoverStep::FinalSync),
            source: Some(VmwareSourceVm {
                vm: source_vm(),
                host: Some("esx1".into()),
                tools: Some("guestToolsRunning".into()),
                power: Some(VmwarePower::PoweredOn),
                guest_ips: BTreeMap::from([(
                    "00:50:56:01:02:03".into(),
                    vec!["10.0.0.5".parse().unwrap()],
                )]),
            }),
            disks: vec![VmwareSyncDisk {
                key: 2000,
                file: "[ds1] web01/web01-flat.vmdk".into(),
                capacity_bytes: 16 << 30,
                volume: Some(volume),
                change_id: Some("52 1c 9d/4".into()),
                copied_bytes: 0,
                rounds: 3,
                last_delta_bytes: 1 << 20,
                source_mtime: None,
            }],
            snapshot: Some(VmwareSnapshotRef {
                moref: "snapshot-7".into(),
                name: snapshot_name(volume, 3),
                created_at: 1_790_000_000,
            }),
            cbt_enabled_by_us: true,
            rounds: 3,
            source_changes: VmwareSourceChanges {
                powered_off_by_us: true,
                nics_disconnected: vec![4000],
                annotation_added: true,
                old_annotation: Some(String::new()),
            },
            ..Default::default()
        };
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(v["state"], "cutting-over");
        assert_eq!(v["cutover_step"], "final-sync");
        assert_eq!(v["source"]["power"], "powered-on");
        assert_eq!(
            v["source"]["guest_ips"],
            json!({ "00:50:56:01:02:03": ["10.0.0.5"] })
        );
        assert_eq!(
            v["disks"][0],
            json!({ "key": 2000, "file": "[ds1] web01/web01-flat.vmdk", "capacity_bytes": 16u64 << 30,
                    "volume": volume, "change_id": "52 1c 9d/4", "rounds": 3, "last_delta_bytes": 1 << 20 })
        );
        assert_eq!(
            v["source_changes"],
            json!({ "powered_off_by_us": true, "nics_disconnected": [4000], "annotation_added": true,
                    "old_annotation": "" })
        );
        assert_eq!(serde_json::from_value::<VmwareVmStatus>(v).unwrap(), status);
        // 전환 칸(스테이지 5)은 있을 때만 쓴다.
        let done = VmwareVmStatus {
            state: VmwareVmState::Done,
            source_stopped_at: Some(10),
            downtime_s: Some(252),
            cutover_requested_at: Some(9),
            ..Default::default()
        };
        let v = serde_json::to_value(&done).unwrap();
        assert_eq!(
            v,
            json!({ "state": "done", "source_stopped_at": 10, "downtime_s": 252, "cutover_requested_at": 9 })
        );
        assert_eq!(serde_json::from_value::<VmwareVmStatus>(v).unwrap(), done);
        for i in 0..25u64 {
            status.push_history(i, format!("event {i}"));
        }
        assert_eq!(status.history.len(), MAX_HISTORY);
        assert_eq!(status.history[0].at, 5);
        assert_eq!(status.history.last().unwrap().event, "event 24");
    }

    #[test]
    fn states_and_steps() {
        for s in VmwareVmState::ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
        let finals: Vec<_> = VmwareVmState::ALL
            .into_iter()
            .filter(|s| s.is_final())
            .collect();
        assert_eq!(
            finals,
            vec![
                VmwareVmState::Finalized,
                VmwareVmState::Reverted,
                VmwareVmState::Cancelled
            ]
        );
        assert!(!VmwareCutoverStep::Preparing.rolls_back());
        assert!(!VmwareCutoverStep::StoppingSource.rolls_back());
        assert!(VmwareCutoverStep::FinalSync.rolls_back());
        assert!(VmwareCutoverStep::StartingVm.rolls_back());
        assert!(VmwareMigrationPhase::Cancelled.is_done());
        assert!(!VmwareMigrationPhase::Paused.is_done());
        let st = VmwareMigrationStatus {
            phase: VmwareMigrationPhase::Running,
            counts: BTreeMap::from([("syncing".into(), 3), ("done".into(), 1)]),
            error: None,
        };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(
            v,
            json!({ "phase": "running", "counts": { "done": 1, "syncing": 3 } })
        );
        assert_eq!(
            serde_json::from_value::<VmwareMigrationStatus>(v).unwrap(),
            st
        );
    }

    #[test]
    fn names() {
        assert_eq!(item_name("wave1", "Web01 (prod)"), "wave1-web01-prod");
        assert_eq!(item_name("wave1", "___"), "wave1-vm");
        let long = item_name(&"w".repeat(40), &"a".repeat(80));
        assert_eq!(long.len(), 63);
        assert_eq!(validate_name(&long), Ok(()));
        assert_eq!(validate_name(&item_name("w", "Ünïcode-ВМ")), Ok(()));
        let id = Uuid::parse_str("01937a5c-1234-7000-8000-000000000000").unwrap();
        assert_eq!(snapshot_name(id, 2), "orbivisor-migration-01937a5c-2");
    }
}
