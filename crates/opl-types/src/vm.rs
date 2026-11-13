use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::runtime::ImageFormat;

pub type Vm = Resource<VmSpec, VmStatus>;

pub const MIN_MEMORY_MIB: u32 = 64;

/// VM의 원하는 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VmSpec {
    pub cpu: CpuSpec,
    pub memory_mib: u32,
    #[serde(default)]
    pub firmware: Firmware,
    pub machine: MachineType,
    #[serde(default)]
    pub disks: Vec<DiskSpec>,
    #[serde(default)]
    pub nics: Vec<NicSpec>,
    #[serde(default)]
    pub power: PowerState,
    #[serde(default)]
    pub restart: RestartPolicy,
    /// 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
    #[serde(default)]
    pub node: Option<Uuid>,
    /// 예약 스냅샷. None이면 예약하지 않는다.
    #[serde(default)]
    pub snapshot_schedule: Option<SnapshotSchedule>,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
    /// JSON은 옛 bool(`false` = 없음, `true` = 기본 정책)이나 정책 객체다.
    #[serde(default, with = "ha_compat")]
    #[cfg_attr(feature = "openapi", schema(value_type = HaSetting))]
    pub ha: Option<VmHa>,
}

/// 재시작·옮기기 횟수의 최댓값.
pub const MAX_HA_COUNT: u8 = 10;

/// VM의 HA 정책.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields, default)]
pub struct VmHa {
    pub state: HaState,
    /// HA 그룹 이름. 없으면 그룹 없이 아무 노드로 복구한다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub group: Option<String>,
    /// 같은 노드에서 다시 켜는 횟수. 넘으면 다른 노드로 옮긴다.
    pub max_restart: u8,
    /// 다른 노드로 옮기는 횟수. 넘으면 `error` 상태로 둔다.
    pub max_relocate: u8,
    /// 복구 순서(0~100, 클수록 먼저). 기본(50)이면 쓰지 않아 계획 50 노드도 정책을 푼다.
    #[serde(skip_serializing_if = "is_default_priority")]
    #[cfg_attr(feature = "openapi", schema(default = 50, minimum = 0, maximum = 100))]
    pub priority: u8,
    /// 게스트 에이전트 감시. 없으면 쓰지 않는다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub guest_watch: Option<GuestWatch>,
}

/// HA 우선순위의 기본값과 최댓값
pub const DEFAULT_HA_PRIORITY: u8 = 50;
pub const MAX_HA_PRIORITY: u8 = 100;

fn is_default_priority(priority: &u8) -> bool {
    *priority == DEFAULT_HA_PRIORITY
}

impl Default for VmHa {
    fn default() -> Self {
        VmHa {
            state: HaState::Started,
            group: None,
            max_restart: 1,
            max_relocate: 1,
            priority: DEFAULT_HA_PRIORITY,
            guest_watch: None,
        }
    }
}

impl VmHa {
    /// 계획 51의 필드(기본이 아닌 우선순위·게스트 감시)를 쓰는지. 그러면 모든 노드가 `fast-ha`를 알아야 한다.
    pub fn uses_fast_ha(&self) -> bool {
        self.priority != DEFAULT_HA_PRIORITY || self.guest_watch.is_some()
    }
}

/// 게스트 감시 시간 제한의 범위와 기본값(초). 최솟값은 놓친 재부팅(QMP `RESET`)에서도 게스트가 다시 뜨기를
/// 기다릴 만큼 둔다.
pub const MIN_GUEST_WATCH_SECS: u32 = 30;
pub const MAX_GUEST_WATCH_SECS: u32 = 3600;
pub const DEFAULT_GUEST_WATCH_SECS: u32 = 60;

/// 게스트 에이전트(QGA) 감시: 한 번 답한 에이전트가 `timeout_secs` 동안 답하지 않으면 VM을 강제로 다시 켠다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct GuestWatch {
    #[serde(default = "default_guest_watch_secs")]
    #[cfg_attr(
        feature = "openapi",
        schema(default = 60, minimum = 30, maximum = 3600)
    )]
    pub timeout_secs: u32,
}

impl Default for GuestWatch {
    fn default() -> Self {
        GuestWatch {
            timeout_secs: DEFAULT_GUEST_WATCH_SECS,
        }
    }
}

fn default_guest_watch_secs() -> u32 {
    DEFAULT_GUEST_WATCH_SECS
}

/// HA가 VM을 어떻게 다루는지.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum HaState {
    /// 노드 장애 때 다른 노드에서 켠다.
    #[default]
    Started,
    /// 노드 장애 때 다른 노드로 옮기기만 하고 켜지 않는다.
    Stopped,
    /// HA를 잠시 끈다(설정은 남김).
    Ignored,
}

/// API 문서의 `ha` 모양: 옛 bool이나 정책 객체. 읽고 쓰기는 `VmSpec.ha`의 손으로 쓴 serde가 한다.
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum HaSetting {
    Enabled(bool),
    Policy(VmHa),
}

/// `ha`의 JSON: 없음은 `false`, 기본 정책은 `true`, 그 밖은 객체로 써서 옛 노드가 옛 VM을 읽는다.
mod ha_compat {
    use std::fmt;

    use serde::de::{self, MapAccess, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{HaSetting, VmHa};

    pub fn serialize<S: Serializer>(ha: &Option<VmHa>, s: S) -> Result<S::Ok, S::Error> {
        match ha {
            None => HaSetting::Enabled(false),
            Some(ha) if *ha == VmHa::default() => HaSetting::Enabled(true),
            Some(ha) => HaSetting::Policy(ha.clone()),
        }
        .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<VmHa>, D::Error> {
        struct HaVisitor;

        impl<'de> Visitor<'de> for HaVisitor {
            type Value = Option<VmHa>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a boolean or an HA policy object")
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(v.then(VmHa::default))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                VmHa::deserialize(de::value::MapAccessDeserializer::new(map)).map(Some)
            }
        }

        d.deserialize_any(HaVisitor)
    }
}

impl ResourceSpec for VmSpec {
    const KIND: Kind = Kind::Vm;
    type Status = VmStatus;
}

/// VM 하나의 최대 NIC 수. tap 장치 이름(15자)에 순번 두 자리를 넣는다.
pub const MAX_NICS: usize = 16;

/// NIC 하나에 거는 보안 그룹의 최대 수.
pub const MAX_NIC_SECURITY_GROUPS: usize = 8;

/// 예약 스냅샷 보관 개수의 최댓값.
pub const MAX_SCHEDULE_KEEP: u32 = 100;

impl VmSpec {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.cpu.sockets == 0 {
            return Err(ValidationError::ZeroSockets);
        }
        if self.cpu.cores == 0 {
            return Err(ValidationError::ZeroCores);
        }
        if self.memory_mib < MIN_MEMORY_MIB {
            return Err(ValidationError::TooLittleMemory);
        }
        let mut seen = BTreeSet::new();
        for order in self.disks.iter().filter_map(|d| d.boot_order) {
            if !seen.insert(order) {
                return Err(ValidationError::DuplicateBootOrder(order));
            }
        }
        if self.nics.len() > MAX_NICS {
            return Err(ValidationError::TooManyNics);
        }
        let mut macs = BTreeSet::new();
        for mac in self.nics.iter().filter_map(|n| n.mac) {
            if mac.0[0] & 1 == 1 {
                return Err(ValidationError::MulticastMac(mac));
            }
            if !macs.insert(mac) {
                return Err(ValidationError::DuplicateMac(mac));
            }
        }
        for (i, nic) in self.nics.iter().enumerate() {
            if nic.security_groups.len() > MAX_NIC_SECURITY_GROUPS {
                return Err(ValidationError::TooManySecurityGroups(i));
            }
            let mut groups = BTreeSet::new();
            for group in &nic.security_groups {
                if !groups.insert(group) {
                    return Err(ValidationError::DuplicateSecurityGroup(i, group.clone()));
                }
            }
        }
        if let Some(schedule) = &self.snapshot_schedule
            && !(1..=MAX_SCHEDULE_KEEP).contains(&schedule.keep)
        {
            return Err(ValidationError::ScheduleKeep);
        }
        if let Some(ha) = &self.ha
            && (ha.max_restart > MAX_HA_COUNT || ha.max_relocate > MAX_HA_COUNT)
        {
            return Err(ValidationError::HaCount);
        }
        if let Some(ha) = &self.ha {
            if ha.priority > MAX_HA_PRIORITY {
                return Err(ValidationError::HaPriority);
            }
            if ha.guest_watch.is_some_and(|w| {
                !(MIN_GUEST_WATCH_SECS..=MAX_GUEST_WATCH_SECS).contains(&w.timeout_secs)
            }) {
                return Err(ValidationError::GuestWatch);
            }
        }
        Ok(())
    }

    /// HA가 이 VM을 다루는지(정책이 있고 `ignored`가 아님). 펜싱·HA 가능 노드 조건·복구가 이것을 본다.
    pub fn ha_active(&self) -> bool {
        self.ha
            .as_ref()
            .is_some_and(|ha| ha.state != HaState::Ignored)
    }

    /// 보안 그룹을 건(방화벽이 켜진) NIC가 있는지. 이런 VM은 방화벽이 있는 노드에서만 돈다.
    pub fn has_firewall(&self) -> bool {
        self.nics.iter().any(|n| !n.security_groups.is_empty())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("cpu.sockets must be at least 1")]
    ZeroSockets,
    #[error("cpu.cores must be at least 1")]
    ZeroCores,
    #[error("memory_mib must be at least {MIN_MEMORY_MIB}")]
    TooLittleMemory,
    #[error("boot_order {0} is used by more than one disk")]
    DuplicateBootOrder(u8),
    #[error("MAC address {0} is a multicast address")]
    MulticastMac(MacAddr),
    #[error("MAC address {0} is used by more than one NIC")]
    DuplicateMac(MacAddr),
    #[error("a VM can have at most {MAX_NICS} NICs")]
    TooManyNics,
    #[error("snapshot_schedule.keep must be between 1 and {MAX_SCHEDULE_KEEP}")]
    ScheduleKeep,
    #[error("nics[{0}]: a NIC can have at most {MAX_NIC_SECURITY_GROUPS} security groups")]
    TooManySecurityGroups(usize),
    #[error("nics[{0}]: security group {1:?} is listed more than once")]
    DuplicateSecurityGroup(usize, String),
    #[error("ha.max_restart and ha.max_relocate must be between 0 and {MAX_HA_COUNT}")]
    HaCount,
    #[error("ha.priority must be between 0 and {MAX_HA_PRIORITY}")]
    HaPriority,
    #[error(
        "ha.guest_watch.timeout_secs must be between {MIN_GUEST_WATCH_SECS} and {MAX_GUEST_WATCH_SECS}"
    )]
    GuestWatch,
}

/// 예약 스냅샷: 주기마다 스냅샷을 만들고, 예약으로 만든 것을 `keep`개만 남긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SnapshotSchedule {
    pub every: ScheduleEvery,
    /// 1부터 `MAX_SCHEDULE_KEEP`까지
    pub keep: u32,
    /// 실행 중이면 메모리도 저장한다.
    #[serde(default)]
    pub memory: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ScheduleEvery {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CpuSpec {
    #[serde(default = "one")]
    pub sockets: u16,
    pub cores: u16,
    #[serde(default)]
    pub model: CpuModel,
}

fn one() -> u16 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CpuModel {
    /// 호스트 CPU를 그대로 노출한다. TCG에서는 max로 대체된다.
    #[default]
    Host,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Firmware {
    Bios,
    #[default]
    Uefi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum MachineType {
    /// x86_64 전용
    Q35,
    /// aarch64 전용
    Virt,
}

impl MachineType {
    /// 이 머신 타입을 쓸 수 있는 아키텍처
    pub fn arch(self) -> Arch {
        match self {
            MachineType::Q35 => Arch::X86_64,
            MachineType::Virt => Arch::Aarch64,
        }
    }
}

/// CPU 아키텍처. 노드가 등록할 때 기록하고, 머신 타입마다 하나로 정해진다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    /// 이 프로그램이 컴파일된 아키텍처
    pub fn native() -> Self {
        if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else {
            Arch::Aarch64
        }
    }

    /// 이 아키텍처에서 쓸 수 있는 머신 타입
    pub fn machine(self) -> MachineType {
        match self {
            Arch::X86_64 => MachineType::Q35,
            Arch::Aarch64 => MachineType::Virt,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DiskSpec {
    /// Volume 리소스 이름
    pub volume: String,
    pub bus: DiskBus,
    #[serde(default)]
    pub boot_order: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum DiskBus {
    VirtioBlk,
    /// 드라이버 없이 설치하는 Windows 게스트용. q35에서만 쓸 수 있다.
    Sata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NicSpec {
    /// Network 리소스 이름
    pub network: String,
    pub model: NicModel,
    /// None이면 생성 시 자동 할당한다.
    #[serde(default)]
    pub mac: Option<MacAddr>,
    /// 이 NIC에 거는 보안 그룹 이름(8개 이하, 겹치지 않게). 비어 있지 않으면 방화벽이 켜진다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_groups: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum NicModel {
    Virtio,
    E1000e,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum PowerState {
    Running,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
}

/// VM의 실제 상태. reconciler만 기록한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmStatus {
    pub phase: VmPhase,
    pub node: Option<Uuid>,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    /// 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
    pub snapshot: Option<Uuid>,
    /// 마지막 예약 스냅샷이 실패한 이유. 성공하면 지운다.
    pub schedule_error: Option<ScheduleError>,
    /// 진행 중인 라이브 마이그레이션. 원본 노드의 마이그레이션 Task가 쓰고 지운다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    // 없으면 빼고 쓰므로 API 문서에서도 null·기본값이 아니라 선택 필드다.
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub migration: Option<MigrationStatus>,
    /// 지금 UEFI 변수 파일을 가진 노드. UEFI VM을 켠 노드나 마이그레이션의 대상 노드가 쓴다.
    /// 다른 노드에서 켤 때 이 노드에서 변수를 받아 온다. BIOS VM은 늘 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub firmware_node: Option<Uuid>,
    /// 지금 배치가 어기는 배치 규칙의 이름. 리더의 HA 관리자가 쓴다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub rule_violations: Vec<String>,
    /// HA 재시작·옮기기 기록. 기록이 없으면 빼고 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ha: Option<VmHaStatus>,
}

/// HA 재시작·옮기기 기록
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmHaStatus {
    /// 재시작 횟수를 넘겨 이 노드에서 다른 노드로 옮겨 달라는 요청. reconciler가 쓴다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub relocate_from: Option<Uuid>,
    /// 창 안에서 다른 노드로 옮긴 때(유닉스 초).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relocations: Vec<u64>,
    /// 마지막으로 옮겨 떠난 노드. 그 노드로 되돌아가면 옮기기 한 번으로 센다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub relocated_from: Option<Uuid>,
    /// HA가 이 VM을 놓거나 옮기지 못한 이유.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

/// 진행 중인 라이브 마이그레이션: 대상 노드와 그 마이그레이션 Task
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigrationStatus {
    pub node: Uuid,
    pub task: Uuid,
    /// 대상 노드로 복사하는 노드 로컬 풀의 디스크. VM이 대상 노드로 확정되면 적용된 것이 된다.
    /// 비면 빼고 쓰므로 공유 풀만의 마이그레이션은 기록이 옛 모양 그대로다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub disks: Vec<DiskMove>,
}

/// 마이그레이션이 복사하는 디스크 하나: 볼륨 `volume`을 대상 노드의 풀 `pool`에 `format`으로 만든다.
/// 원본 풀·형식은 볼륨 기록에 그대로 있다(확정 전에는 바꾸지 않는다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiskMove {
    pub volume: Uuid,
    pub pool: String,
    pub format: ImageFormat,
}

/// 예약 스냅샷이 실패한 이유. `code`는 거절한 API 오류의 code(`vm_busy` 등)이고, Task가 실패했으면
/// `task_failed`다. 옛 상태의 문자열은 `{ code: "unknown", message: <문자열> }`로 읽는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(from = "ScheduleErrorRepr")]
pub struct ScheduleError {
    pub code: String,
    pub message: String,
}

/// [`ScheduleError`]가 읽는 형식: 지금의 구조나 옛 문자열
#[derive(Deserialize)]
#[serde(untagged)]
enum ScheduleErrorRepr {
    Current { code: String, message: String },
    Old(String),
}

impl From<ScheduleErrorRepr> for ScheduleError {
    fn from(r: ScheduleErrorRepr) -> Self {
        match r {
            ScheduleErrorRepr::Current { code, message } => ScheduleError { code, message },
            ScheduleErrorRepr::Old(message) => ScheduleError {
                code: "unknown".into(),
                message,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmPhase {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
    /// 다른 노드로 라이브 마이그레이션 중이다(원본 QEMU가 아직 돈다).
    Migrating,
}

/// `52:54:00:12:34:56` 형식의 MAC 주소.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(value_type = String))]
#[serde(try_from = "String", into = "String")]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// QEMU/KVM 관례 접두사 `52:54:00`에 난수 3바이트를 붙인 주소를 만든다.
    /// 난수가 24비트라 VM이 많으면 충돌할 수 있다. 호출자(API)는 클러스터 안에서 중복을 확인하고 다시 뽑는다.
    pub fn generate() -> Self {
        let r = Uuid::new_v4().into_bytes();
        MacAddr([0x52, 0x54, 0x00, r[0], r[1], r[2]])
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid MAC address: {0}")]
pub struct MacParseError(String);

impl FromStr for MacAddr {
    type Err = MacParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || MacParseError(s.to_string());
        let mut bytes = [0u8; 6];
        let mut parts = s.split(':');
        for b in &mut bytes {
            let part = parts.next().ok_or_else(err)?;
            if part.len() != 2 || !part.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err(err());
            }
            *b = u8::from_str_radix(part, 16).map_err(|_| err())?;
        }
        if parts.next().is_some() {
            return Err(err());
        }
        Ok(MacAddr(bytes))
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

impl TryFrom<String> for MacAddr {
    type Error = MacParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<MacAddr> for String {
    fn from(m: MacAddr) -> String {
        m.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> serde_json::Value {
        serde_json::json!({
            "cpu": { "cores": 4 },
            "memory_mib": 8192,
            "machine": "q35",
            "disks": [ { "volume": "web-01-root", "bus": "virtio-blk", "boot_order": 1 } ],
            "nics": [ { "network": "lan", "model": "virtio" } ],
            "power": "running",
            "restart": "on-failure"
        })
    }

    fn sample() -> VmSpec {
        serde_json::from_value(sample_json()).unwrap()
    }

    #[test]
    fn arch_serializes_as_lowercase_and_pairs_with_machine() {
        assert_eq!(serde_json::to_value(Arch::X86_64).unwrap(), "x86_64");
        assert_eq!(serde_json::to_value(Arch::Aarch64).unwrap(), "aarch64");
        assert_eq!(
            serde_json::from_value::<Arch>(serde_json::json!("aarch64")).unwrap(),
            Arch::Aarch64
        );
        for arch in [Arch::X86_64, Arch::Aarch64] {
            assert_eq!(arch.machine().arch(), arch);
        }
        assert_eq!(MachineType::Q35.arch(), Arch::X86_64);
        assert_eq!(MachineType::Virt.arch(), Arch::Aarch64);
    }

    #[test]
    fn deserializes_with_defaults() {
        let spec = sample();
        assert_eq!(spec.cpu.sockets, 1);
        assert_eq!(spec.cpu.model, CpuModel::Host);
        assert_eq!(spec.firmware, Firmware::Uefi);
        assert_eq!(spec.disks[0].bus, DiskBus::VirtioBlk);
        assert_eq!(spec.nics[0].mac, None);
        assert_eq!(spec.power, PowerState::Running);
        assert_eq!(spec.restart, RestartPolicy::OnFailure);
        assert_eq!(spec.node, None);
        assert_eq!(spec.snapshot_schedule, None);
        assert_eq!(spec.ha, None);
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut json = sample_json();
        json["memroy_mib"] = 1.into();
        assert!(serde_json::from_value::<VmSpec>(json).is_err());
    }

    #[test]
    fn round_trips_through_json() {
        let spec = sample();
        let back: VmSpec = serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn validate_accepts_sample() {
        assert_eq!(sample().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_bad_values() {
        let mut s = sample();
        s.cpu.cores = 0;
        assert_eq!(s.validate(), Err(ValidationError::ZeroCores));

        let mut s = sample();
        s.cpu.sockets = 0;
        assert_eq!(s.validate(), Err(ValidationError::ZeroSockets));

        let mut s = sample();
        s.memory_mib = MIN_MEMORY_MIB - 1;
        assert_eq!(s.validate(), Err(ValidationError::TooLittleMemory));

        let mut s = sample();
        let mut second = s.disks[0].clone();
        second.volume = "data".into();
        s.disks.push(second);
        assert_eq!(s.validate(), Err(ValidationError::DuplicateBootOrder(1)));

        let mut s = sample();
        let mac: MacAddr = "01:00:5e:00:00:01".parse().unwrap();
        s.nics[0].mac = Some(mac);
        assert_eq!(s.validate(), Err(ValidationError::MulticastMac(mac)));

        let mut s = sample();
        let mac: MacAddr = "52:54:00:12:34:56".parse().unwrap();
        s.nics[0].mac = Some(mac);
        let mut second = s.nics[0].clone();
        second.network = "wan".into();
        s.nics.push(second);
        assert_eq!(s.validate(), Err(ValidationError::DuplicateMac(mac)));

        let mut s = sample();
        s.nics = vec![s.nics[0].clone(); MAX_NICS + 1];
        for nic in &mut s.nics {
            nic.mac = None;
        }
        assert_eq!(s.validate(), Err(ValidationError::TooManyNics));
    }

    #[test]
    fn validate_checks_nic_security_groups() {
        let with_groups = |groups: &[&str]| {
            let mut s = sample();
            let mut second = s.nics[0].clone();
            second.network = "wan".into();
            second.security_groups = groups.iter().map(|g| g.to_string()).collect();
            s.nics.push(second);
            s.validate()
        };
        assert_eq!(with_groups(&[]), Ok(()));
        let eight: Vec<String> = (0..MAX_NIC_SECURITY_GROUPS)
            .map(|i| format!("g{i}"))
            .collect();
        let eight: Vec<&str> = eight.iter().map(String::as_str).collect();
        assert_eq!(with_groups(&eight), Ok(()));
        let mut nine = eight.clone();
        nine.push("g8");
        assert_eq!(
            with_groups(&nine),
            Err(ValidationError::TooManySecurityGroups(1))
        );
        assert_eq!(
            with_groups(&["web", "ssh", "web"]),
            Err(ValidationError::DuplicateSecurityGroup(1, "web".into()))
        );
        assert_eq!(
            ValidationError::DuplicateSecurityGroup(1, "web".into()).to_string(),
            "nics[1]: security group \"web\" is listed more than once"
        );
        // 다른 NIC에 같은 그룹을 거는 것은 된다.
        let mut s = sample();
        s.nics[0].security_groups = vec!["web".into()];
        let mut second = s.nics[0].clone();
        second.network = "wan".into();
        s.nics.push(second);
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn has_firewall_when_any_nic_has_groups() {
        let mut s = sample();
        assert!(!s.has_firewall());
        let mut second = s.nics[0].clone();
        second.security_groups = vec!["web".into()];
        s.nics.push(second);
        assert!(s.has_firewall());
    }

    #[test]
    fn nic_security_groups_default_empty_and_are_omitted() {
        // 옛 NIC JSON은 그대로 읽고 그대로 쓴다.
        let old = serde_json::json!({ "network": "lan", "model": "virtio", "mac": null });
        let nic: NicSpec = serde_json::from_value(old.clone()).unwrap();
        assert!(nic.security_groups.is_empty());
        assert_eq!(serde_json::to_value(&nic).unwrap(), old);

        let json = serde_json::json!({
            "network": "lan",
            "model": "virtio",
            "mac": null,
            "security_groups": ["web", "ssh"]
        });
        let nic: NicSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(nic.security_groups, ["web", "ssh"]);
        assert_eq!(serde_json::to_value(&nic).unwrap(), json);
    }

    #[test]
    fn status_missing_fields_default() {
        let st: VmStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(st, VmStatus::default());
        assert_eq!(st.snapshot, None);
        assert_eq!(st.schedule_error, None);
        assert_eq!(st.migration, None);
        assert_eq!(st.firmware_node, None);
    }

    #[test]
    fn firmware_node_is_written_only_when_known() {
        let json = serde_json::to_value(VmStatus::default()).unwrap();
        assert!(json.get("firmware_node").is_none(), "{json}");
        let node = Uuid::now_v7();
        let st = VmStatus {
            firmware_node: Some(node),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["firmware_node"], serde_json::json!(node));
        assert_eq!(serde_json::from_value::<VmStatus>(json).unwrap(), st);
    }

    #[test]
    fn migration_status_is_written_only_while_migrating() {
        // 마이그레이션이 없으면 필드를 빼고 쓴다.
        let json = serde_json::to_value(VmStatus::default()).unwrap();
        assert!(json.get("migration").is_none(), "{json}");
        let st = VmStatus {
            phase: VmPhase::Migrating,
            migration: Some(MigrationStatus {
                node: Uuid::now_v7(),
                task: Uuid::now_v7(),
                disks: vec![],
            }),
            ..Default::default()
        };
        let m = st.migration.clone().unwrap();
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["phase"], "migrating");
        // 복사할 디스크가 없으면 disks를 빼고 쓴다(옛 모양).
        assert_eq!(
            json["migration"],
            serde_json::json!({ "node": m.node, "task": m.task })
        );
        assert_eq!(serde_json::from_value::<VmStatus>(json).unwrap(), st);
    }

    #[test]
    fn migration_status_carries_the_disk_moves() {
        let volume = Uuid::now_v7();
        let m = MigrationStatus {
            node: Uuid::now_v7(),
            task: Uuid::now_v7(),
            disks: vec![DiskMove {
                volume,
                pool: "fast".into(),
                format: ImageFormat::Raw,
            }],
        };
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "node": m.node,
                "task": m.task,
                "disks": [{ "volume": volume, "pool": "fast", "format": "raw" }]
            })
        );
        assert_eq!(serde_json::from_value::<MigrationStatus>(json).unwrap(), m);
    }

    #[test]
    fn schedule_error_reads_old_string_and_writes_structure() {
        let e = ScheduleError {
            code: "vm_busy".into(),
            message: "VM web is busy".into(),
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "code": "vm_busy", "message": "VM web is busy" })
        );
        assert_eq!(serde_json::from_value::<ScheduleError>(json).unwrap(), e);
        // 옛 상태는 문자열이었다. code를 모르므로 unknown으로 읽는다.
        let st: VmStatus =
            serde_json::from_value(serde_json::json!({ "schedule_error": "VM web is busy" }))
                .unwrap();
        assert_eq!(
            st.schedule_error,
            Some(ScheduleError {
                code: "unknown".into(),
                message: "VM web is busy".into(),
            })
        );
        assert_eq!(
            serde_json::to_value(&st).unwrap()["schedule_error"],
            serde_json::json!({ "code": "unknown", "message": "VM web is busy" })
        );
    }

    // 이 계획 전(bf40f46)에 쓴 VM의 JSON 그대로.
    const OLD_VM_JSON: &str = r#"{"cpu":{"sockets":1,"cores":4,"model":"host"},"memory_mib":8192,"firmware":"uefi","machine":"q35","disks":[{"volume":"web-01-root","bus":"virtio-blk","boot_order":1}],"nics":[{"network":"lan","model":"virtio","mac":"52:54:00:12:34:56"}],"power":"running","restart":"on-failure","node":"0193a6f2-7c4e-7d1a-9b3e-2f4c5d6e7f80","snapshot_schedule":null,"ha":false}"#;

    #[test]
    fn ha_reads_old_bools() {
        let mut json = sample_json();
        json["ha"] = false.into();
        assert_eq!(
            serde_json::from_value::<VmSpec>(json.clone()).unwrap().ha,
            None
        );
        json["ha"] = true.into();
        assert_eq!(
            serde_json::from_value::<VmSpec>(json).unwrap().ha,
            Some(VmHa::default())
        );
        // 필드가 없으면 HA가 아니다.
        assert_eq!(sample().ha, None);
    }

    #[test]
    fn old_vm_json_round_trips_byte_for_byte() {
        let spec: VmSpec = serde_json::from_str(OLD_VM_JSON).unwrap();
        assert_eq!(spec.ha, None);
        assert_eq!(serde_json::to_string(&spec).unwrap(), OLD_VM_JSON);
        let old_ha = OLD_VM_JSON.replace(r#""ha":false"#, r#""ha":true"#);
        let spec: VmSpec = serde_json::from_str(&old_ha).unwrap();
        assert_eq!(spec.ha, Some(VmHa::default()));
        assert_eq!(serde_json::to_string(&spec).unwrap(), old_ha);
    }

    #[test]
    fn ha_writes_none_as_false_and_default_as_true() {
        let mut spec = sample();
        assert_eq!(serde_json::to_value(&spec).unwrap()["ha"], false);
        spec.ha = Some(VmHa::default());
        assert_eq!(serde_json::to_value(&spec).unwrap()["ha"], true);
    }

    #[test]
    fn ha_policy_object_round_trips() {
        let mut spec = sample();
        spec.ha = Some(VmHa {
            state: HaState::Stopped,
            group: Some("rack-a".into()),
            max_restart: 3,
            max_relocate: 0,
            ..Default::default()
        });
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["ha"],
            serde_json::json!({
                "state": "stopped",
                "group": "rack-a",
                "max_restart": 3,
                "max_relocate": 0
            })
        );
        assert_eq!(serde_json::from_value::<VmSpec>(json).unwrap(), spec);

        // 기본과 하나만 달라도 객체로 쓴다. 그룹이 없으면 빼고 쓴다.
        spec.ha = Some(VmHa {
            state: HaState::Ignored,
            ..Default::default()
        });
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["ha"],
            serde_json::json!({ "state": "ignored", "max_restart": 1, "max_relocate": 1 })
        );
        assert_eq!(serde_json::from_value::<VmSpec>(json).unwrap(), spec);
    }

    #[test]
    fn ha_object_fills_missing_fields_and_rejects_unknown() {
        let mut json = sample_json();
        json["ha"] = serde_json::json!({ "group": "g" });
        assert_eq!(
            serde_json::from_value::<VmSpec>(json.clone()).unwrap().ha,
            Some(VmHa {
                group: Some("g".into()),
                ..Default::default()
            })
        );
        // 빈 객체는 기본 정책이고 다시 쓰면 true다.
        json["ha"] = serde_json::json!({});
        let spec: VmSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.ha, Some(VmHa::default()));
        assert_eq!(serde_json::to_value(&spec).unwrap()["ha"], true);

        for bad in [
            serde_json::json!({ "state": "started", "weight": 1 }),
            serde_json::json!({ "guest_watch": { "timeout_secs": 60, "interval": 5 } }),
            serde_json::json!({ "priority": "high" }),
            serde_json::json!({ "state": "paused" }),
            serde_json::json!("yes"),
            serde_json::json!(1),
            serde_json::Value::Null,
        ] {
            json["ha"] = bad.clone();
            assert!(
                serde_json::from_value::<VmSpec>(json.clone()).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    /// 계획 50 노드가 쓴 그룹 정책 객체는 바이트까지 그대로 읽고 쓴다(새 필드가 기본값이면 쓰지 않는다).
    #[test]
    fn plan50_policy_object_round_trips_byte_for_byte() {
        let old = OLD_VM_JSON.replace(
            r#""ha":false"#,
            r#""ha":{"state":"started","group":"rack-a","max_restart":1,"max_relocate":1}"#,
        );
        let spec: VmSpec = serde_json::from_str(&old).unwrap();
        let ha = spec.ha.clone().unwrap();
        assert_eq!(ha.priority, DEFAULT_HA_PRIORITY);
        assert_eq!(ha.guest_watch, None);
        assert!(!ha.uses_fast_ha());
        assert_eq!(serde_json::to_string(&spec).unwrap(), old);
    }

    /// 우선순위·게스트 감시는 기본이 아니면 객체에 나타나고 다시 읽힌다. 기본 정책은 그대로 `true`다.
    #[test]
    fn priority_and_guest_watch_are_written_only_when_set() {
        let mut spec = sample();
        spec.ha = Some(VmHa {
            priority: DEFAULT_HA_PRIORITY,
            guest_watch: None,
            ..Default::default()
        });
        assert_eq!(serde_json::to_value(&spec).unwrap()["ha"], true);

        spec.ha = Some(VmHa {
            priority: 80,
            ..Default::default()
        });
        assert!(spec.ha.as_ref().unwrap().uses_fast_ha());
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["ha"],
            serde_json::json!({ "state": "started", "max_restart": 1, "max_relocate": 1, "priority": 80 })
        );
        assert_eq!(serde_json::from_value::<VmSpec>(json).unwrap(), spec);

        spec.ha = Some(VmHa {
            guest_watch: Some(GuestWatch { timeout_secs: 120 }),
            ..Default::default()
        });
        assert!(spec.ha.as_ref().unwrap().uses_fast_ha());
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["ha"],
            serde_json::json!({
                "state": "started",
                "max_restart": 1,
                "max_relocate": 1,
                "guest_watch": { "timeout_secs": 120 }
            })
        );
        assert_eq!(serde_json::from_value::<VmSpec>(json).unwrap(), spec);

        // 빈 감시 객체는 기본 60초다.
        let mut json = sample_json();
        json["ha"] = serde_json::json!({ "guest_watch": {} });
        let ha = serde_json::from_value::<VmSpec>(json).unwrap().ha.unwrap();
        assert_eq!(ha.guest_watch, Some(GuestWatch { timeout_secs: 60 }));
    }

    #[test]
    fn validate_checks_priority_and_guest_watch() {
        let with = |priority, timeout_secs: Option<u32>| {
            let mut s = sample();
            s.ha = Some(VmHa {
                priority,
                guest_watch: timeout_secs.map(|timeout_secs| GuestWatch { timeout_secs }),
                ..Default::default()
            });
            s.validate()
        };
        assert_eq!(with(0, None), Ok(()));
        assert_eq!(with(MAX_HA_PRIORITY, Some(30)), Ok(()));
        assert_eq!(with(50, Some(3600)), Ok(()));
        assert_eq!(
            with(MAX_HA_PRIORITY + 1, None),
            Err(ValidationError::HaPriority)
        );
        assert_eq!(with(50, Some(29)), Err(ValidationError::GuestWatch));
        assert_eq!(with(50, Some(3601)), Err(ValidationError::GuestWatch));
        assert_eq!(
            ValidationError::GuestWatch.to_string(),
            "ha.guest_watch.timeout_secs must be between 30 and 3600"
        );
    }

    #[test]
    fn validate_checks_ha_counts() {
        let with = |max_restart, max_relocate| {
            let mut s = sample();
            s.ha = Some(VmHa {
                max_restart,
                max_relocate,
                ..Default::default()
            });
            s.validate()
        };
        assert_eq!(with(0, 0), Ok(()));
        assert_eq!(with(MAX_HA_COUNT, MAX_HA_COUNT), Ok(()));
        assert_eq!(with(MAX_HA_COUNT + 1, 1), Err(ValidationError::HaCount));
        assert_eq!(with(1, MAX_HA_COUNT + 1), Err(ValidationError::HaCount));
    }

    #[test]
    fn ha_active_ignores_ignored_state() {
        let mut s = sample();
        assert!(!s.ha_active());
        for (state, active) in [
            (HaState::Started, true),
            (HaState::Stopped, true),
            (HaState::Ignored, false),
        ] {
            s.ha = Some(VmHa {
                state,
                ..Default::default()
            });
            assert_eq!(s.ha_active(), active, "{state:?}");
        }
    }

    #[test]
    fn status_ha_fields_are_written_only_when_set() {
        // 비어 있으면 옛 모양 그대로다.
        let old = serde_json::json!({
            "phase": "running",
            "node": null,
            "pid": 7,
            "last_error": null,
            "snapshot": null,
            "schedule_error": null
        });
        let st: VmStatus = serde_json::from_value(old.clone()).unwrap();
        assert!(st.rule_violations.is_empty());
        assert_eq!(st.ha, None);
        assert_eq!(serde_json::to_value(&st).unwrap(), old);

        let node = Uuid::now_v7();
        let st = VmStatus {
            rule_violations: vec!["keep-apart".into()],
            ha: Some(VmHaStatus {
                relocate_from: Some(node),
                relocations: vec![100, 200],
                relocated_from: Some(node),
                error: Some("gave up after 1 relocations".into()),
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["rule_violations"], serde_json::json!(["keep-apart"]));
        assert_eq!(
            json["ha"],
            serde_json::json!({
                "relocate_from": node,
                "relocations": [100, 200],
                "relocated_from": node,
                "error": "gave up after 1 relocations"
            })
        );
        assert_eq!(serde_json::from_value::<VmStatus>(json).unwrap(), st);
        // 빈 기록의 필드는 빼고 쓴다.
        assert_eq!(
            serde_json::to_value(VmHaStatus::default()).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn snapshot_schedule_defaults_to_none() {
        assert_eq!(sample().snapshot_schedule, None);
    }

    #[test]
    fn snapshot_schedule_round_trips() {
        let mut json = sample_json();
        json["snapshot_schedule"] = serde_json::json!({ "every": "daily", "keep": 7 });
        let spec: VmSpec = serde_json::from_value(json).unwrap();
        assert_eq!(
            spec.snapshot_schedule,
            Some(SnapshotSchedule {
                every: ScheduleEvery::Daily,
                keep: 7,
                memory: false,
            })
        );
        let back = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            back["snapshot_schedule"],
            serde_json::json!({ "every": "daily", "keep": 7, "memory": false })
        );
        for (every, text) in [
            (ScheduleEvery::Hourly, "hourly"),
            (ScheduleEvery::Daily, "daily"),
            (ScheduleEvery::Weekly, "weekly"),
        ] {
            assert_eq!(serde_json::to_value(every).unwrap(), text);
        }

        let mut json = sample_json();
        json["snapshot_schedule"] = serde_json::json!({ "every": "monthly", "keep": 7 });
        assert!(serde_json::from_value::<VmSpec>(json).is_err());
    }

    #[test]
    fn validate_checks_schedule_keep() {
        let with_keep = |keep| {
            let mut s = sample();
            s.snapshot_schedule = Some(SnapshotSchedule {
                every: ScheduleEvery::Hourly,
                keep,
                memory: true,
            });
            s.validate()
        };
        assert_eq!(with_keep(1), Ok(()));
        assert_eq!(with_keep(MAX_SCHEDULE_KEEP), Ok(()));
        assert_eq!(with_keep(0), Err(ValidationError::ScheduleKeep));
        assert_eq!(
            with_keep(MAX_SCHEDULE_KEEP + 1),
            Err(ValidationError::ScheduleKeep)
        );
    }

    #[test]
    fn mac_parses_and_formats() {
        let mac: MacAddr = "52:54:00:AB:cd:0f".parse().unwrap();
        assert_eq!(mac.0, [0x52, 0x54, 0x00, 0xab, 0xcd, 0x0f]);
        assert_eq!(mac.to_string(), "52:54:00:ab:cd:0f");
        assert_eq!(serde_json::to_value(mac).unwrap(), "52:54:00:ab:cd:0f");
    }

    #[test]
    fn generated_mac_is_unicast_with_qemu_prefix() {
        let a = MacAddr::generate();
        let b = MacAddr::generate();
        assert_eq!(&a.0[..3], &[0x52, 0x54, 0x00]);
        assert_eq!(a.0[0] & 1, 0);
        assert_ne!(a, b);
    }

    #[test]
    fn mac_rejects_malformed() {
        for bad in [
            "",
            "52:54:00:ab:cd",
            "52:54:00:ab:cd:0f:11",
            "5:54:00:ab:cd:0f",
            "zz:54:00:ab:cd:0f",
            "+2:54:00:ab:cd:0f",
        ] {
            assert!(bad.parse::<MacAddr>().is_err(), "{bad} should be rejected");
        }
        assert!(serde_json::from_value::<MacAddr>("nope".into()).is_err());
    }
}
