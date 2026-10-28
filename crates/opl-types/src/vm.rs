use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

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
}

impl ResourceSpec for VmSpec {
    const KIND: Kind = Kind::Vm;
    type Status = VmStatus;
}

/// VM 하나의 최대 NIC 수. tap 장치 이름(15자)에 순번 두 자리를 넣는다.
pub const MAX_NICS: usize = 16;

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
        if let Some(schedule) = &self.snapshot_schedule
            && !(1..=MAX_SCHEDULE_KEEP).contains(&schedule.keep)
        {
            return Err(ValidationError::ScheduleKeep);
        }
        Ok(())
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
    fn status_missing_fields_default() {
        let st: VmStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(st, VmStatus::default());
        assert_eq!(st.snapshot, None);
        assert_eq!(st.schedule_error, None);
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
