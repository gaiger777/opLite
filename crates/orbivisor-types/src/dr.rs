//! DR 오케스트레이션(계획 76 설계 §3): 복구 계획(`RecoveryPlan`), 그 보호 상태(RPO), 실행(`DrRun`)과 실행 상태 기계.
//! 계획과 실행은 복구하는 클러스터(DR 쪽)에 있고 운영 쪽은 원격(`source`)으로만 가리킨다. 복구 지점은 DR 쪽에 있는
//! 사본 백업(`origin`이 그 원격)이다. 상태 기계 [`advance`]는 순수 함수다: 리더 스케줄러는 5초마다 계획·지금 실행·관찰을
//! 넣어 다음 실행 상태와 할 일([`DrAction`])을 받고, 할 일을 Task·API 부름으로 바꾼 뒤 그 결과를 [`DrRunStatus::record`]로
//! 적어 저장하기만 한다. 같은 입력이면 같은 출력이라 리더가 바뀌어도 기록에서 그대로 이어 간다.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::backup::{Backup, BackupPhase};
use crate::cloud_init::{CloudInit, CloudInitDns, CloudInitNet, IpConfig, MAX_DNS_SERVERS};
use crate::folder::parse_folder_path;
use crate::resource::{Kind, NameError, Resource, ResourceSpec, validate_name};
use crate::vm::MAX_NICS;

pub type RecoveryPlan = Resource<RecoveryPlanSpec, RecoveryPlanStatus>;
pub type DrRun = Resource<DrRunSpec, DrRunStatus>;

/// 계획 하나의 VM 수 상한
pub const MAX_PLAN_VMS: usize = 500;
/// 계획 하나의 그룹 수 상한
pub const MAX_GROUPS: usize = 32;
/// `rpo_target_secs` 기본(1일)과 상한(1년)
pub const DEFAULT_RPO_SECS: u64 = 86_400;
pub const MAX_RPO_SECS: u64 = 365 * 86_400;
/// 그룹 `timeout_secs` 기본과 범위
pub const DEFAULT_TIMEOUT_SECS: u32 = 600;
pub const MIN_TIMEOUT_SECS: u32 = 10;
pub const MAX_TIMEOUT_SECS: u32 = 86_400;
/// 그룹 `delay_secs` 상한(1시간)
pub const MAX_DELAY_SECS: u32 = 3600;
/// VM 하나의 IP 규칙 수 상한
pub const MAX_IP_RULES: usize = 2 * MAX_NICS;
/// 실행 하나에서 동시에 진행하는 복원·시작 수(설정 `max_parallel`) 기본과 상한
pub const DEFAULT_MAX_PARALLEL: u32 = 4;
pub const MAX_PARALLEL: u32 = 32;
/// 노드 하나에서 동시에 진행하는 복원·시작 수
pub const PER_NODE_PARALLEL: usize = 2;
/// 계획마다 남기는 실행 기록 수
pub const MAX_RUNS_PER_PLAN: usize = 20;
/// 계획된 이전: 운영 쪽 VM에 shutdown을 보내고 이만큼 지나면 stop(초)
pub const SHUTDOWN_GRACE_SECS: u64 = 300;
/// 답이 없는 원격 부름(stop·백업)을 다시 보내는 간격(초)
pub const RETRY_SECS: u64 = 60;

/// 장애 조치로 만든 VM의 라벨(값 = 계획 이름) — 설계의 태그 `dr:<plan>`
pub const DR_LABEL: &str = "orbivisor.io/dr";
/// 시험 장애 조치로 만든 VM의 라벨(값 = 계획 이름) — 설계의 태그 `dr-test:<plan>`
pub const DR_TEST_LABEL: &str = "orbivisor.io/dr-test";
/// 시험 VM 이름의 꼬리(`<vm>-drt`)
pub const TEST_VM_SUFFIX: &str = "-drt";
/// 시험 VM을 두는 폴더의 윗 폴더(`dr-test/<plan>`)
pub const TEST_FOLDER: &str = "dr-test";
/// 시험 거품 네트워크 이름의 머리(`drt-<plan>-<n>`)
pub const BUBBLE_PREFIX: &str = "drt-";

/// API 오류 코드(설계 §3.1·§3.2). API는 이 문자열을 그대로 쓴다.
pub const ERR_SOURCE_MISSING: &str = "dr_source_missing";
pub const ERR_RUN_ACTIVE: &str = "dr_run_active";
pub const ERR_CLEANUP_REQUIRED: &str = "dr_cleanup_required";
pub const ERR_TEST_NETWORK_REQUIRED: &str = "dr_test_network_required";
pub const ERR_NAME_TAKEN: &str = "dr_name_taken";

// ---- 계획 ----

/// 복구 계획(DR 쪽 리소스). 운영 클러스터(`source` 원격)의 VM을 그룹 차례로 이 클러스터에 되살린다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RecoveryPlanSpec {
    /// 운영 클러스터(원격 이름). 사본의 `origin`이 이 원격의 CA 지문인 것으로 VM을 맞춘다.
    pub source: String,
    /// 복구 지점이 이보다 오래되면 경보 `dr_rpo_exceeded`(초, 0 = 끔)
    #[serde(default = "default_rpo")]
    #[cfg_attr(feature = "openapi", schema(default = 86400, maximum = 31536000))]
    pub rpo_target_secs: u64,
    /// 기본 복원 풀(VM마다 덮어씀)
    pub pool: String,
    /// 장애 조치로 만든 VM을 둘 폴더(id나 경로). 없으면 백업한 VM의 폴더(복원 규칙)다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub folder: Option<String>,
    /// 원본 네트워크 → DR 네트워크. 없는 이름은 같은 이름이다.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub networks: BTreeMap<String, String>,
    /// 원본 보안 그룹 → DR 보안 그룹. 없는 이름은 같은 이름이다.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub security_groups: BTreeMap<String, String>,
    /// 시험 때 원본 네트워크 → DR 네트워크. 없는 원본 네트워크는 시험 때 거품 SDN 네트워크(`drt-<plan>-<n>`)다.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub test_networks: BTreeMap<String, String>,
    /// VM이 실패하거나 시간을 넘기면 계속할지(기본) 멈출지
    #[serde(default, skip_serializing_if = "OnTimeout::is_continue")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub on_timeout: OnTimeout,
    /// 차례대로 되살리는 그룹(1~32)
    pub groups: Vec<DrGroup>,
}

fn default_rpo() -> u64 {
    DEFAULT_RPO_SECS
}

impl ResourceSpec for RecoveryPlanSpec {
    const KIND: Kind = Kind::RecoveryPlan;
    type Status = RecoveryPlanStatus;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum OnTimeout {
    /// 그 VM만 `failed`·`timed_out`으로 두고 계속한다.
    #[default]
    Continue,
    /// 남은 VM을 `skipped`로 두고 실행을 `partial`로 끝낸다(진행 중인 것은 끝까지).
    Stop,
}

impl OnTimeout {
    fn is_continue(&self) -> bool {
        *self == Self::Continue
    }
}

/// 부팅 그룹 하나. 그룹 안의 VM은 함께 되살리고, 모두 끝나면(`ready`·`failed`·`timed_out`) `delay_secs` 뒤 다음 그룹이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DrGroup {
    pub name: String,
    /// VM이 준비됐다고 보는 때
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub wait: DrWait,
    /// 시작 Task가 끝난 뒤 이만큼(초) 지나도 준비되지 않으면 `timed_out`
    #[serde(default = "default_timeout")]
    #[cfg_attr(
        feature = "openapi",
        schema(default = 600, minimum = 10, maximum = 86400)
    )]
    pub timeout_secs: u32,
    /// 이 그룹이 끝난 뒤 다음 그룹 앞 쉼(초)
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default = 0, maximum = 3600))]
    pub delay_secs: u32,
    pub vms: Vec<DrVm>,
}

fn default_timeout() -> u32 {
    DEFAULT_TIMEOUT_SECS
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum DrWait {
    /// QEMU가 시작됨
    #[default]
    Running,
    /// 게스트 에이전트가 답함
    Guest,
    /// 게스트 에이전트가 기대 IP(IP 규칙의 고정 주소, 없으면 아무 전역 주소)를 보고함
    Ip,
}

/// 계획의 VM 하나(원본 VM 이름)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DrVm {
    /// 원본 VM 이름(사본의 `vm_name`)
    pub vm: String,
    /// 복원 풀. 없으면 계획의 `pool`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub pool: Option<String>,
    /// 복원 노드(이름). 없으면 복원 API가 고른다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub node: Option<String>,
    /// 라이브 복원(저장소 백업만 — 디스크를 다 받기 전에 켬)
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub live: bool,
    /// 되살린 VM의 cloud-init 주소(NIC마다). 비면 백업의 cloud-init 그대로
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub ip: Vec<DrIpRule>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// NIC 하나의 주소 규칙: 고정 주소(`address`, IPv4나 IPv6 — 주소 체계마다 하나) 또는 `dhcp: true`(IPv4 DHCP)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DrIpRule {
    /// NIC 순번(0부터). 되살린 VM에 그 NIC가 있는지는 복원 때 본다.
    pub nic: u32,
    /// `주소/접두 길이`(예: `10.20.0.5/24`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub gateway: Option<String>,
    /// DNS 서버(VM 전체로 모아 cloud-init `dns.servers`, 겹치지 않게 3개까지)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>, default))]
    pub dns: Vec<IpAddr>,
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub dhcp: bool,
}

impl DrIpRule {
    /// 이 규칙이 IPv6 칸인지(DHCP는 IPv4)
    fn is_ipv6(&self) -> bool {
        self.address
            .as_deref()
            .and_then(|a| a.split('/').next())
            .and_then(|a| a.parse::<IpAddr>().ok())
            .is_some_and(|a| a.is_ipv6())
    }

    /// 고정 주소의 IP(접두 길이 없이)
    fn expected_ip(&self) -> Option<IpAddr> {
        self.address
            .as_deref()
            .and_then(|a| a.split('/').next())
            .and_then(|a| a.parse().ok())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DrPlanError {
    #[error("{field}: {error}")]
    Name { field: String, error: NameError },
    #[error("folder {0:?} is not a folder id or path")]
    Folder(String),
    #[error("give 1-{MAX_GROUPS} groups, not {0}")]
    Groups(usize),
    #[error("give 1-{MAX_PLAN_VMS} VMs in all, not {0}")]
    Vms(usize),
    #[error("group {0:?} has no VMs")]
    EmptyGroup(String),
    #[error("group {0:?} is given twice")]
    DuplicateGroup(String),
    #[error("VM {0:?} is given twice")]
    DuplicateVm(String),
    #[error("{field} {value} is out of range: use {min}-{max}")]
    Range {
        field: String,
        value: u64,
        min: u64,
        max: u64,
    },
    #[error("VM {vm:?} ip[{index}]: {reason}")]
    IpRule {
        vm: String,
        index: usize,
        reason: String,
    },
}

fn name(field: impl Into<String>, value: &str) -> Result<(), DrPlanError> {
    validate_name(value).map_err(|error| DrPlanError::Name {
        field: field.into(),
        error,
    })
}

fn range(field: impl Into<String>, value: u64, min: u64, max: u64) -> Result<(), DrPlanError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(DrPlanError::Range {
            field: field.into(),
            value,
            min,
            max,
        })
    }
}

impl RecoveryPlanSpec {
    /// 형식만 본다(원격·풀·네트워크·폴더가 있는지는 API가 본다 — 원격이 없으면 409 [`ERR_SOURCE_MISSING`]).
    pub fn validate(&self) -> Result<(), DrPlanError> {
        name("source", &self.source)?;
        name("pool", &self.pool)?;
        range("rpo_target_secs", self.rpo_target_secs, 0, MAX_RPO_SECS)?;
        if let Some(folder) = &self.folder
            && Uuid::try_parse(folder).is_err()
            && parse_folder_path(folder).is_err()
        {
            return Err(DrPlanError::Folder(folder.clone()));
        }
        for (field, map) in [
            ("networks", &self.networks),
            ("security_groups", &self.security_groups),
            ("test_networks", &self.test_networks),
        ] {
            for (from, to) in map {
                name(field, from)?;
                name(field, to)?;
            }
        }
        if self.groups.is_empty() || self.groups.len() > MAX_GROUPS {
            return Err(DrPlanError::Groups(self.groups.len()));
        }
        let total: usize = self.groups.iter().map(|g| g.vms.len()).sum();
        if total > MAX_PLAN_VMS {
            return Err(DrPlanError::Vms(total));
        }
        let mut groups = BTreeSet::new();
        let mut vms = BTreeSet::new();
        for (gi, g) in self.groups.iter().enumerate() {
            name(format!("groups[{gi}].name"), &g.name)?;
            if !groups.insert(g.name.as_str()) {
                return Err(DrPlanError::DuplicateGroup(g.name.clone()));
            }
            if g.vms.is_empty() {
                return Err(DrPlanError::EmptyGroup(g.name.clone()));
            }
            range(
                format!("groups[{gi}].timeout_secs"),
                g.timeout_secs.into(),
                MIN_TIMEOUT_SECS.into(),
                MAX_TIMEOUT_SECS.into(),
            )?;
            range(
                format!("groups[{gi}].delay_secs"),
                g.delay_secs.into(),
                0,
                MAX_DELAY_SECS.into(),
            )?;
            for (vi, v) in g.vms.iter().enumerate() {
                name(format!("groups[{gi}].vms[{vi}].vm"), &v.vm)?;
                if !vms.insert(v.vm.as_str()) {
                    return Err(DrPlanError::DuplicateVm(v.vm.clone()));
                }
                if let Some(pool) = &v.pool {
                    name(format!("groups[{gi}].vms[{vi}].pool"), pool)?;
                }
                if let Some(node) = &v.node {
                    name(format!("groups[{gi}].vms[{vi}].node"), node)?;
                }
                validate_ip_rules(&v.vm, &v.ip)?;
            }
        }
        Ok(())
    }

    /// 계획의 VM(그룹 차례)
    pub fn vms(&self) -> impl Iterator<Item = (&DrGroup, &DrVm)> {
        self.groups
            .iter()
            .flat_map(|g| g.vms.iter().map(move |v| (g, v)))
    }
}

/// IP 규칙 검사: 고정 주소와 `dhcp` 중 정확히 하나, DHCP에는 게이트웨이 없음, NIC·주소 체계마다 하나, NIC 순번이
/// VM NIC 상한 안, DNS 서버는 모아 3개까지, 주소·게이트웨이 모양은 cloud-init 검사 그대로.
fn validate_ip_rules(vm: &str, rules: &[DrIpRule]) -> Result<(), DrPlanError> {
    let err = |index: usize, reason: String| DrPlanError::IpRule {
        vm: vm.to_string(),
        index,
        reason,
    };
    if rules.len() > MAX_IP_RULES {
        return Err(err(
            rules.len() - 1,
            format!("at most {MAX_IP_RULES} rules"),
        ));
    }
    let mut seen = BTreeSet::new();
    for (i, r) in rules.iter().enumerate() {
        if r.nic as usize >= MAX_NICS {
            return Err(err(
                i,
                format!("nic {} is out of range: use 0-{}", r.nic, MAX_NICS - 1),
            ));
        }
        match (&r.address, r.dhcp) {
            (Some(_), true) | (None, false) => {
                return Err(err(i, "give exactly one of address and dhcp".into()));
            }
            (None, true) if r.gateway.is_some() => {
                return Err(err(i, "a dhcp rule takes no gateway".into()));
            }
            _ => {}
        }
        if !seen.insert((r.nic, r.is_ipv6())) {
            let family = if r.is_ipv6() { "ipv6" } else { "ipv4" };
            return Err(err(i, format!("nic {} has two {family} rules", r.nic)));
        }
        // 주소 모양은 cloud-init 검사가 본다(그 NIC 칸 하나로).
        let (networks, _) = cloud_init_networks(&[], std::slice::from_ref(r));
        let ci = CloudInit {
            networks,
            ..Default::default()
        };
        if let Err(e) = ci.validate(MAX_NICS) {
            return Err(err(i, e.to_string()));
        }
    }
    let dns = merged_dns(rules);
    if dns.len() > MAX_DNS_SERVERS {
        return Err(err(
            rules.len() - 1,
            format!("at most {MAX_DNS_SERVERS} dns servers in all"),
        ));
    }
    Ok(())
}

fn merged_dns(rules: &[DrIpRule]) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for a in rules.iter().flat_map(|r| &r.dns) {
        if !out.contains(a) {
            out.push(*a);
        }
    }
    out
}

/// IP 규칙을 복원 요청의 cloud-init 칸(`cloud_init_networks`·`cloud_init_dns`)으로 바꾼다. `base`는 백업한 VM의
/// cloud-init `networks`이고, 규칙이 있는 NIC·주소 체계 칸만 바꾼다(그 사이 NIC는 빈 항목 = IPv4 DHCP). DNS는 규칙의
/// 서버를 모은 것이고 없으면 `None`(백업의 것 그대로).
pub fn cloud_init_networks(
    base: &[CloudInitNet],
    rules: &[DrIpRule],
) -> (Vec<CloudInitNet>, Option<CloudInitDns>) {
    let mut nets = base.to_vec();
    for r in rules {
        let i = r.nic as usize;
        if nets.len() <= i {
            nets.resize(i + 1, CloudInitNet::default());
        }
        let config = match &r.address {
            Some(address) => IpConfig::Static {
                address: address.clone(),
                gateway: r.gateway.clone(),
            },
            None => IpConfig::Dhcp,
        };
        if r.is_ipv6() {
            nets[i].ipv6 = Some(config);
        } else {
            nets[i].ipv4 = Some(config);
        }
    }
    let servers = merged_dns(rules);
    let dns = (!servers.is_empty()).then(|| CloudInitDns {
        servers,
        search: Vec::new(),
    });
    (nets, dns)
}

// ---- 보호 상태 ----

/// 리더가 1분마다 다시 세는 보호 상태와 마지막 시험·지금 실행
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RecoveryPlanStatus {
    /// 계획의 VM마다(그룹 차례)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub protection: Vec<VmProtection>,
    /// 보호 상태를 센 시각(유닉스 초)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub checked_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_test: Option<LastTest>,
    /// 지금 실행(`DrRun` id)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub run: Option<Uuid>,
}

/// VM 하나의 복구 지점
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmProtection {
    /// 원본 VM 이름
    pub vm: String,
    /// 복구 지점(DR 사본 이름)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub backup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub backup_id: Option<Uuid>,
    /// 복구 지점의 시각(원본 백업을 만든 때, 유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub point_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub age_secs: Option<u64>,
    pub ok: bool,
    /// `ok`가 아닌 까닭
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub issue: Option<ProtectionIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ProtectionIssue {
    /// 쓸 수 있는 사본이 없음
    NoRecoveryPoint,
    /// 가장 새 사본이 RPO 목표보다 오래됨
    OverRpo,
}

/// 마지막 시험 장애 조치
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LastTest {
    pub run: Uuid,
    /// 끝난 시각(유닉스 초)
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub rto_secs: Option<u64>,
    pub result: DrRunState,
}

/// `vm`(원본 VM 이름)의 복구 지점: DR의 `ready` 사본 가운데 `origin.ca_fingerprint`가 운영 클러스터(원격)의 CA 지문이고
/// 만든 때의 VM 이름이 같은 가장 새 것. 증분 사본은 부모 사슬(`status.parent`)이 모두 `ready`로 있어야 쓴다. 원격의
/// 클러스터는 CA 지문으로 알아본다 — 이름(`origin.cluster`·원격 상태의 `cluster_name`)은 바뀌거나 겹칠 수 있다.
pub fn recovery_point<'a>(vm: &str, backups: &'a [Backup], source_ca: &str) -> Option<&'a Backup> {
    let by_id: HashMap<Uuid, &Backup> = backups.iter().map(|b| (b.id, b)).collect();
    best_point(vm, backups, source_ca, &by_id)
}

fn best_point<'a>(
    vm: &str,
    backups: &'a [Backup],
    source_ca: &str,
    by_id: &HashMap<Uuid, &'a Backup>,
) -> Option<&'a Backup> {
    backups
        .iter()
        .filter(|b| from_source(b, source_ca) && b.spec.vm_name == vm)
        .filter(|b| b.status.created_at.is_some() && chain_ready(b, by_id))
        .max_by_key(|b| (b.status.created_at, b.id))
}

fn from_source(b: &Backup, source_ca: &str) -> bool {
    b.spec
        .origin
        .as_ref()
        .is_some_and(|o| o.ca_fingerprint == source_ca)
}

/// 백업과 그 부모 사슬이 모두 `ready`로 있는지(고리는 실패)
fn chain_ready(b: &Backup, by_id: &HashMap<Uuid, &Backup>) -> bool {
    let mut cur = b;
    for _ in 0..=by_id.len() {
        if cur.status.phase != BackupPhase::Ready {
            return false;
        }
        match cur.status.parent {
            None => return true,
            Some(p) => match by_id.get(&p) {
                Some(parent) => cur = parent,
                None => return false,
            },
        }
    }
    false
}

/// 계획의 보호 상태(설계 §3.1). `source_ca`는 원격의 CA 지문이고 원격이 없으면 `None`(모든 VM이 복구 지점 없음).
/// 원격에 닿지 않아도 DR 쪽 자료만으로 센다.
pub fn protection(
    plan: &RecoveryPlanSpec,
    backups: &[Backup],
    source_ca: Option<&str>,
    now: u64,
) -> Vec<VmProtection> {
    let by_id: HashMap<Uuid, &Backup> = backups.iter().map(|b| (b.id, b)).collect();
    plan.vms()
        .map(|(_, v)| {
            let point = source_ca.and_then(|ca| best_point(&v.vm, backups, ca, &by_id));
            match point {
                None => VmProtection {
                    vm: v.vm.clone(),
                    backup: None,
                    backup_id: None,
                    point_at: None,
                    age_secs: None,
                    ok: false,
                    issue: Some(ProtectionIssue::NoRecoveryPoint),
                },
                Some(b) => {
                    let at = b.status.created_at.unwrap_or(0);
                    let age = now.saturating_sub(at);
                    let over = plan.rpo_target_secs > 0 && age > plan.rpo_target_secs;
                    VmProtection {
                        vm: v.vm.clone(),
                        backup: Some(b.name.clone()),
                        backup_id: Some(b.id),
                        point_at: Some(at),
                        age_secs: Some(age),
                        ok: !over,
                        issue: over.then_some(ProtectionIssue::OverRpo),
                    }
                }
            }
        })
        .collect()
}

/// 경보 `dr_rpo_exceeded`의 문구(설계 §3.4): `plan web-tier: 2 of 12 VMs over RPO 24h (web3 34h, db1 no recovery point)`.
/// 모두 `ok`면 `None`. 이름은 다섯까지 적고 나머지는 수로.
pub fn rpo_summary(
    plan: &str,
    rpo_target_secs: u64,
    protection: &[VmProtection],
) -> Option<String> {
    let bad: Vec<&VmProtection> = protection.iter().filter(|p| !p.ok).collect();
    if bad.is_empty() {
        return None;
    }
    let mut items: Vec<String> = bad
        .iter()
        .take(5)
        .map(|p| match (p.issue, p.age_secs) {
            (Some(ProtectionIssue::OverRpo), Some(age)) => format!("{} {}", p.vm, short_secs(age)),
            _ => format!("{} no recovery point", p.vm),
        })
        .collect();
    if bad.len() > 5 {
        items.push(format!("+{} more", bad.len() - 5));
    }
    let what = if rpo_target_secs > 0 {
        format!("over RPO {}", short_secs(rpo_target_secs))
    } else {
        "unprotected".to_string()
    };
    Some(format!(
        "plan {plan}: {} of {} VMs {what} ({})",
        bad.len(),
        protection.len(),
        items.join(", ")
    ))
}

/// `34h`·`15m`·`40s`(내림)
fn short_secs(secs: u64) -> String {
    match secs {
        s if s >= 3600 => format!("{}h", s / 3600),
        s if s >= 60 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// 시험 때 거품 네트워크가 필요한 원본 네트워크(이름 순): 계획 VM의 복구 지점이 쓰는 네트워크 가운데 `test_networks`에
/// 없는 것. SDN이 없는 클러스터에서 이것이 비지 않으면 API는 시험을 400 [`ERR_TEST_NETWORK_REQUIRED`]로 거절한다.
pub fn bubble_sources(
    plan: &RecoveryPlanSpec,
    backups: &[Backup],
    source_ca: Option<&str>,
) -> Vec<String> {
    let by_id: HashMap<Uuid, &Backup> = backups.iter().map(|b| (b.id, b)).collect();
    let mut out = BTreeSet::new();
    for (_, v) in plan.vms() {
        let Some(b) = source_ca.and_then(|ca| best_point(&v.vm, backups, ca, &by_id)) else {
            continue;
        };
        for nic in &b.spec.vm_spec.nics {
            if !plan.test_networks.contains_key(&nic.network) {
                out.insert(nic.network.clone());
            }
        }
    }
    out.into_iter().collect()
}

// ---- 이름 ----

/// FNV-1a 32비트(이름을 줄일 때 겹치지 않게 붙이는 짧은 표시)
fn fnv1a(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// `head`에 `tail`을 붙인 `max`자 이하 이름. 넘치면 `head`를 줄이고 원래 `head`의 해시 6자리를 끼워
/// (`<줄인 머리>-<해시><꼬리>`) 줄인 이름끼리 겹치지 않게 한다. `-`로 끝나지 않게 줄인다.
fn fit_name(head: &str, tail: &str, max: usize) -> String {
    if head.len() + tail.len() <= max {
        return format!("{head}{tail}");
    }
    let hash = format!("{:06x}", fnv1a(head) & 0x00ff_ffff);
    let keep = max.saturating_sub(tail.len() + hash.len() + 1);
    // 이름 규칙상 ASCII라 바이트로 잘라도 된다.
    let cut = head[..keep.min(head.len())].trim_end_matches('-');
    format!("{cut}-{hash}{tail}")
}

/// 리소스 이름 길이 상한([`validate_name`])
const MAX_NAME: usize = 63;

/// 시험 VM 이름 `<vm>-drt`(이름 규칙 63자에 맞게 줄임)
pub fn test_vm_name(vm: &str) -> String {
    fit_name(vm, TEST_VM_SUFFIX, MAX_NAME)
}

/// 거품 네트워크 이름 `drt-<plan>-<n>`(`n`은 1부터, 63자에 맞게 계획 이름을 줄임)
pub fn bubble_network_name(plan: &str, n: usize) -> String {
    let body = fit_name(plan, &format!("-{n}"), MAX_NAME - BUBBLE_PREFIX.len());
    format!("{BUBBLE_PREFIX}{body}")
}

/// 실행 이름 `<plan>-<what>-<secs>`(`what`은 `test`·`failover`·`cleanup`, `secs`는 시작 시각 — 63자에 맞게 계획 이름을 줄임)
pub fn run_name(plan: &str, what: &str, secs: u64) -> String {
    fit_name(plan, &format!("-{what}-{secs}"), MAX_NAME)
}

/// 되돌리기용 백업 작업 이름(설계 §3.6) `dr-reprotect-<plan>`(63자에 맞게 계획 이름을 줄임)
pub fn reprotect_job_name(plan: &str) -> String {
    const HEAD: &str = "dr-reprotect-";
    format!("{HEAD}{}", fit_name(plan, "", MAX_NAME - HEAD.len()))
}

/// 시험 VM 폴더 경로 `dr-test/<plan>`
pub fn test_folder(plan: &str) -> String {
    format!("{TEST_FOLDER}/{plan}")
}

// ---- 실행 ----

/// 실행 하나(Raft 리소스, 계획마다 최근 [`MAX_RUNS_PER_PLAN`]개). spec은 시작할 때 정하고 바꾸지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DrRunSpec {
    /// 계획 id
    pub plan: Uuid,
    /// 시작한 때의 계획 이름
    pub plan_name: String,
    pub kind: DrRunKind,
    /// 시작한 때의 계획(실행 중에 계획을 바꿔도 이 실행은 그대로 간다)
    pub plan_spec: RecoveryPlanSpec,
    /// 동시에 진행하는 복원·시작 수(설정, 노드마다는 [`PER_NODE_PARALLEL`])
    #[serde(default = "default_max_parallel")]
    #[cfg_attr(feature = "openapi", schema(default = 4, minimum = 1, maximum = 32))]
    pub max_parallel: u32,
    /// 뒷정리가 지울 것(`cleanup` 실행만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cleanup: Option<DrCleanupTarget>,
}

fn default_max_parallel() -> u32 {
    DEFAULT_MAX_PARALLEL
}

impl ResourceSpec for DrRunSpec {
    const KIND: Kind = Kind::DrRun;
    type Status = DrRunStatus;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum DrRunKind {
    /// 시험 장애 조치(격리망, 운영 쪽을 건드리지 않음)
    Test,
    /// 계획된 이전(운영 쪽 정지·마지막 백업·복제 뒤 넘김)
    FailoverPlanned,
    /// 재해 장애 조치(지금 있는 가장 새 사본으로 곧장)
    FailoverDisaster,
    /// 마지막 시험의 VM·거품 네트워크 지우기
    Cleanup,
}

impl DrRunKind {
    pub fn is_failover(self) -> bool {
        matches!(self, Self::FailoverPlanned | Self::FailoverDisaster)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum DrRunState {
    #[default]
    Running,
    /// 모든 VM이 `ready`(뒷정리는 모두 지움)
    Succeeded,
    /// 일부만
    Partial,
    /// 하나도 되지 않음
    Failed,
    Cancelled,
}

impl DrRunState {
    pub fn is_finished(self) -> bool {
        self != Self::Running
    }
}

/// 뒷정리 대상: 시험 실행이 만든 VM과 거품 네트워크([`DrRunStatus::cleanup_target`])
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DrCleanupTarget {
    /// 시험 실행 id
    pub run: Uuid,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub vms: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub networks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct DrRunStatus {
    pub state: DrRunState,
    /// 시작한 시각(유닉스 초)
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub finished_at: Option<u64>,
    /// 시작부터 마지막 `ready`까지(초). `ready`가 없으면 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub rto_secs: Option<u64>,
    /// 계획된 이전: 시작부터 운영 쪽 정지·마지막 복제가 끝날 때까지(초, RTO에 든다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub prepare_secs: Option<u64>,
    /// 지금 그룹 순번
    #[serde(skip_serializing_if = "is_zero_u32")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub group: u32,
    /// 앞 그룹이 끝나 쉬는 중이면 다음 그룹을 시작할 시각
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub delay_until: Option<u64>,
    /// 취소를 받음(API가 적는다): 진행 중인 Task는 끝까지, 새 단계는 없다.
    #[serde(skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub cancel_requested: bool,
    /// `on_timeout: stop`이나 거품 네트워크 실패로 남은 VM을 건너뛰는 중
    #[serde(skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub stopping: bool,
    /// 실행 전체의 실패 까닭(거품 네트워크 등)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub bubbles: Vec<DrBubble>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub steps: Vec<DrStep>,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// 시험의 거품 SDN 네트워크(라우터·업링크 없음, `like` 네트워크의 CIDR 그대로)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DrBubble {
    /// 원본 네트워크 이름(뒷정리는 비움)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub source: String,
    /// 거품 네트워크 이름 `drt-<plan>-<n>`
    pub name: String,
    /// CIDR을 본뜰 DR 네트워크(원본 네트워크의 `networks` 맵 값, 없으면 같은 이름)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub like: String,
    pub state: BubbleState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BubbleState {
    /// 만들어야 함
    Pending,
    /// 있음
    Ready,
    /// 지움(뒷정리)
    Removed,
    Failed,
}

/// VM 하나의 진행
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DrStep {
    /// 그룹 이름(뒷정리는 비움)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub group: String,
    /// 원본 VM 이름(뒷정리는 지울 VM 이름)
    pub vm: String,
    /// 복구 지점(DR 사본 이름)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub backup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub backup_id: Option<Uuid>,
    pub phase: DrPhase,
    /// 되살린(뒷정리는 지울) VM 이름
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub new_vm: Option<String>,
    /// 지금 단계의 Task(복원·시작·지우기). 있으면 다시 내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub task: Option<Uuid>,
    /// 복원·시작하는 노드(계획의 `node`, 없으면 되살린 VM의 노드를 보고 앎)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 지금 단계에 든 시각(유닉스 초). `waiting`은 여기서부터 `timeout_secs`, `ready`는 RTO의 끝이다.
    pub at: u64,
    /// 계획된 이전의 운영 쪽 준비
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub prepare: Option<DrPrepare>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum DrPhase {
    Pending,
    /// 계획된 이전: 운영 쪽 정지·백업·복제
    Preparing,
    Restoring,
    Starting,
    /// 그룹의 `wait`를 기다림
    Waiting,
    Ready,
    Failed,
    TimedOut,
    Skipped,
    /// 뒷정리: VM을 끄고 지우는 중
    Deleting,
    /// 뒷정리: 지움(없었음)
    Deleted,
}

impl DrPhase {
    /// 더 나아가지 않는 단계
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Failed | Self::TimedOut | Self::Skipped | Self::Deleted
        )
    }

    /// Task가 도는 단계(동시성에 든다)
    fn in_flight(self) -> bool {
        matches!(self, Self::Restoring | Self::Starting | Self::Deleting)
    }
}

/// 계획된 이전에서 VM 하나의 운영 쪽 준비
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DrPrepare {
    pub stage: PrepareStage,
    /// 지금 단계의 원격 부름을 보낸 시각
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub requested_at: Option<u64>,
    /// 운영 쪽에 뜬 마지막 백업
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub source_backup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub source_backup_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum PrepareStage {
    /// shutdown을 보내고 꺼지기를 기다림(뒤 그룹부터)
    Shutdown,
    /// [`SHUTDOWN_GRACE_SECS`]가 지나 stop
    Stop,
    /// 운영 쪽 백업
    Backup,
    /// 그 백업을 이 클러스터로 복제하고 사본을 기다림
    Replicate,
    /// 사본이 왔음(단계는 `pending`)
    Done,
}

// ---- 관찰과 할 일 ----

/// 스케줄러가 한 번 본 것. 상태 기계의 입력이다.
#[derive(Debug, Clone, Default)]
pub struct DrObservation {
    /// 지금(유닉스 초)
    pub now: u64,
    /// 단계가 가리키는 Task의 결과. 없으면(지워짐) VM으로 판단한다.
    pub tasks: BTreeMap<Uuid, TaskOutcome>,
    /// 이 클러스터의 VM(이름으로)
    pub vms: BTreeMap<String, VmObservation>,
    /// 이 클러스터의 네트워크 이름
    pub networks: BTreeSet<String>,
    /// 이 클러스터의 백업(계획된 이전에서 새 사본을 찾음)
    pub backups: Vec<Backup>,
    /// 운영 클러스터(원격)의 CA 지문
    pub source_ca: Option<String>,
    /// 운영 클러스터에서 본 것(계획된 이전만). 닿지 않으면 `None`이고 준비는 기다린다.
    pub source: Option<SourceObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    Running,
    Succeeded,
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmObservation {
    /// QEMU가 돈다
    pub running: bool,
    /// 게스트 에이전트가 답한다
    pub guest: bool,
    /// 게스트 에이전트가 보고한 주소
    pub ips: Vec<IpAddr>,
    pub node: Option<String>,
}

/// 운영 클러스터에서 본 것(프록시로 읽음)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceObservation {
    /// VM 이름 → 꺼졌는지
    pub vms: BTreeMap<String, bool>,
    /// 백업 이름 → 상태
    pub backups: BTreeMap<String, SourceBackup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackup {
    pub id: Uuid,
    pub ready: bool,
    /// 백업이 실패한 까닭
    pub error: Option<String>,
    /// 이 클러스터로 보내는 복제가 실패한 까닭
    pub replica_error: Option<String>,
}

/// 스케줄러가 할 일. `step`·`bubble`은 [`DrRunStatus`]의 순번이고 결과는 [`DrRunStatus::record`]로 적는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrAction {
    /// 거품 SDN 네트워크를 만든다(`like`의 서브넷을 본떠 라우터 없이). 이미 있으면 결과는 `Done`이다.
    CreateBubble {
        bubble: usize,
        name: String,
        like: String,
    },
    /// 사본 `backup`을 `name`으로 복원하는 Task(복원 맵·cloud-init 주소·라벨과 함께). 결과는 `Task(id)`.
    Restore {
        step: usize,
        backup: Uuid,
        name: String,
        pool: String,
        node: Option<String>,
        folder: Option<String>,
        networks: BTreeMap<String, String>,
        security_groups: BTreeMap<String, String>,
        /// [`cloud_init_networks`]로 백업의 cloud-init과 합친다.
        ip: Vec<DrIpRule>,
        live: bool,
        label: (String, String),
    },
    /// `vm-start` Task. 결과는 `Task(id)`.
    Start { step: usize, vm: String },
    /// 운영 쪽 `POST /vms/{vm}/actions/shutdown`
    ShutdownSource { step: usize, vm: String },
    /// 운영 쪽 `POST /vms/{vm}/actions/stop`
    StopSource { step: usize, vm: String },
    /// 운영 쪽 `POST /vms/{vm}/backup`. 결과는 `SourceBackup`.
    BackupSource { step: usize, vm: String },
    /// 운영 쪽 `POST /backups/{backup}/actions/replicate`(이 클러스터로)
    ReplicateSource { step: usize, backup: String },
    /// 시험 VM을 끄고 지우는 Task. 결과는 `Task(id)`.
    DeleteVm { step: usize, vm: String },
    /// 거품 네트워크를 지운다. 이미 없으면 결과는 `Done`이다.
    DeleteNetwork { bubble: usize, name: String },
    /// 취소한 시험의 뒷정리 실행을 시작한다(실행이 끝날 때 한 번).
    StartCleanup,
}

/// 할 일의 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrResult {
    Task(Uuid),
    SourceBackup { name: String, id: Uuid },
    Done,
    Failed(String),
}

impl DrRunStatus {
    /// 실행을 시작한 상태: VM마다 단계 하나(그룹 차례). 시험·재해 장애 조치는 지금 있는 가장 새 복구 지점을 고르고(없으면
    /// 그 VM `failed`), 계획된 이전은 운영 쪽 준비부터. 시험은 거품 네트워크([`bubble_sources`])를, 뒷정리는 대상을 둔다.
    pub fn start(spec: &DrRunSpec, backups: &[Backup], source_ca: Option<&str>, now: u64) -> Self {
        let mut s = DrRunStatus {
            started_at: now,
            ..Default::default()
        };
        let plan = &spec.plan_spec;
        if spec.kind == DrRunKind::Cleanup {
            let target = spec.cleanup.clone().unwrap_or(DrCleanupTarget {
                run: Uuid::nil(),
                vms: Vec::new(),
                networks: Vec::new(),
            });
            s.steps = target
                .vms
                .iter()
                .map(|vm| DrStep {
                    group: String::new(),
                    vm: vm.clone(),
                    backup: None,
                    backup_id: None,
                    phase: DrPhase::Pending,
                    new_vm: Some(vm.clone()),
                    task: None,
                    node: None,
                    error: None,
                    at: now,
                    prepare: None,
                })
                .collect();
            s.bubbles = target
                .networks
                .iter()
                .map(|n| DrBubble {
                    source: String::new(),
                    name: n.clone(),
                    like: String::new(),
                    state: BubbleState::Ready,
                    error: None,
                })
                .collect();
            return s;
        }
        let points = protection(plan, backups, source_ca, now);
        for ((g, v), p) in plan.vms().zip(&points) {
            let mut step = DrStep {
                group: g.name.clone(),
                vm: v.vm.clone(),
                backup: None,
                backup_id: None,
                phase: DrPhase::Pending,
                new_vm: None,
                task: None,
                node: v.node.clone(),
                error: None,
                at: now,
                prepare: None,
            };
            if spec.kind == DrRunKind::FailoverPlanned {
                step.phase = DrPhase::Preparing;
                step.prepare = Some(DrPrepare {
                    stage: PrepareStage::Shutdown,
                    requested_at: None,
                    source_backup: None,
                    source_backup_id: None,
                });
            } else if p.backup_id.is_some() {
                step.backup = p.backup.clone();
                step.backup_id = p.backup_id;
            } else {
                step.phase = DrPhase::Failed;
                step.error = Some("no recovery point".into());
            }
            s.steps.push(step);
        }
        if spec.kind == DrRunKind::Test {
            s.bubbles = bubble_sources(plan, backups, source_ca)
                .into_iter()
                .enumerate()
                .map(|(i, source)| DrBubble {
                    name: bubble_network_name(&spec.plan_name, i + 1),
                    like: plan.networks.get(&source).unwrap_or(&source).clone(),
                    source,
                    state: BubbleState::Pending,
                    error: None,
                })
                .collect();
        }
        s
    }

    /// 할 일의 결과를 적는다. 스케줄러는 할 일을 한 뒤 이것을 부르고 상태를 저장한다(저장하기 전에 리더가 바뀌면 새 리더가
    /// 같은 할 일을 다시 받는다).
    pub fn record(&mut self, action: &DrAction, result: DrResult, now: u64) {
        let fail = |step: &mut DrStep, e: String| {
            step.phase = DrPhase::Failed;
            step.error = Some(e);
            step.task = None;
            step.at = now;
        };
        match action {
            DrAction::CreateBubble { bubble, .. } | DrAction::DeleteNetwork { bubble, .. } => {
                let Some(b) = self.bubbles.get_mut(*bubble) else {
                    return;
                };
                match result {
                    DrResult::Failed(e) => {
                        b.state = BubbleState::Failed;
                        b.error = Some(e.clone());
                        self.error.get_or_insert(format!("network {}: {e}", b.name));
                    }
                    _ => {
                        b.state = match action {
                            DrAction::CreateBubble { .. } => BubbleState::Ready,
                            _ => BubbleState::Removed,
                        }
                    }
                }
            }
            DrAction::Restore { step, .. }
            | DrAction::Start { step, .. }
            | DrAction::DeleteVm { step, .. } => {
                let Some(st) = self.steps.get_mut(*step) else {
                    return;
                };
                match result {
                    DrResult::Task(id) => st.task = Some(id),
                    DrResult::Failed(e) => fail(st, e),
                    _ => {}
                }
            }
            DrAction::ShutdownSource { step, .. }
            | DrAction::StopSource { step, .. }
            | DrAction::BackupSource { step, .. }
            | DrAction::ReplicateSource { step, .. } => {
                let Some(st) = self.steps.get_mut(*step) else {
                    return;
                };
                match result {
                    DrResult::SourceBackup { name, id } => {
                        if let Some(p) = &mut st.prepare {
                            p.source_backup = Some(name);
                            p.source_backup_id = Some(id);
                        }
                    }
                    DrResult::Failed(e) => fail(st, e),
                    _ => {}
                }
            }
            DrAction::StartCleanup => {}
        }
    }

    /// 이 (시험) 실행을 치울 뒷정리 대상: 이름을 정한 시험 VM과 거품 네트워크(지운 것 빼고)
    pub fn cleanup_target(&self, run: Uuid) -> DrCleanupTarget {
        DrCleanupTarget {
            run,
            vms: self.steps.iter().filter_map(|s| s.new_vm.clone()).collect(),
            networks: self
                .bubbles
                .iter()
                .filter(|b| matches!(b.state, BubbleState::Ready | BubbleState::Pending))
                .map(|b| b.name.clone())
                .collect(),
        }
    }
}

/// 상태 기계 한 걸음(설계 §3.2 엔진). 끝난 실행은 그대로 돌려준다.
///
/// - 그룹은 차례로: 그룹의 모든 VM이 끝나면(`ready`·`failed`·`timed_out`·`skipped`) `delay_secs` 쉬고 다음 그룹.
/// - 그룹 안은 동시에: 복원·시작 Task가 실행 전체 `max_parallel`개, 노드마다 [`PER_NODE_PARALLEL`]개까지(노드를 아는 것만).
/// - VM은 복원 Task → (라이브 복원이 아니면) `vm-start` Task → 그룹의 `wait` → `ready`. 기다림이 `timeout_secs`를 넘으면
///   `timed_out`. 복원 Task가 도는 시간은 시간 제한에 들지 않는다(디스크 크기에 따름).
/// - `on_timeout: stop`이면 `failed`·`timed_out`이 하나라도 생기면 남은(`pending`) VM은 `skipped`, 진행 중인 것은 끝까지.
/// - 취소: 진행 중인 Task는 끝까지, 그 뒤 단계는 없다(`skipped`). 시험이면 끝날 때 [`DrAction::StartCleanup`].
/// - 단계에 Task id가 있으면 그 Task를 다시 내지 않는다. Task id가 없는 `restoring`·`starting`은 같은 할 일을 다시 낸다.
pub fn advance(
    spec: &DrRunSpec,
    current: &DrRunStatus,
    obs: &DrObservation,
) -> (DrRunStatus, Vec<DrAction>) {
    let mut s = current.clone();
    let mut actions = Vec::new();
    if s.state.is_finished() {
        return (s, actions);
    }
    if spec.kind == DrRunKind::Cleanup {
        advance_cleanup(spec, &mut s, obs, &mut actions);
    } else {
        advance_recovery(spec, &mut s, obs, &mut actions);
    }
    (s, actions)
}

/// Task 결과. Task가 없어졌으면 `done`(그 단계가 바라던 것 — 예: VM이 있음)으로 판단한다.
fn outcome(obs: &DrObservation, task: Uuid, done: bool) -> TaskOutcome {
    match obs.tasks.get(&task) {
        Some(o) => o.clone(),
        None if done => TaskOutcome::Succeeded,
        None => TaskOutcome::Failed(format!("task {task} is gone")),
    }
}

/// 게스트가 보고한 주소 가운데 전역 주소가 있는지(루프백·링크 로컬·미지정·멀티캐스트 빼고)
fn has_global(ips: &[IpAddr]) -> bool {
    ips.iter().any(|ip| match ip {
        IpAddr::V4(a) => {
            !(a.is_loopback() || a.is_link_local() || a.is_unspecified() || a.is_multicast())
        }
        IpAddr::V6(a) => {
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (a.segments()[0] & 0xffc0) == 0xfe80)
        }
    })
}

fn wait_met(wait: DrWait, vm: &VmObservation, rules: &[DrIpRule]) -> bool {
    match wait {
        DrWait::Running => vm.running,
        DrWait::Guest => vm.running && vm.guest,
        DrWait::Ip => {
            let expected: Vec<IpAddr> = rules.iter().filter_map(DrIpRule::expected_ip).collect();
            vm.running
                && if expected.is_empty() {
                    has_global(&vm.ips)
                } else {
                    expected.iter().all(|e| vm.ips.contains(e))
                }
        }
    }
}

fn set_phase(step: &mut DrStep, phase: DrPhase, now: u64) {
    step.phase = phase;
    step.task = None;
    step.at = now;
}

fn advance_recovery(
    spec: &DrRunSpec,
    s: &mut DrRunStatus,
    obs: &DrObservation,
    actions: &mut Vec<DrAction>,
) {
    let plan = &spec.plan_spec;
    let now = obs.now;
    let groups: Vec<&DrGroup> = plan.groups.iter().collect();
    let group_of = |name: &str| groups.iter().position(|g| g.name == name);
    let vm_of = |name: &str| plan.vms().find(|(_, v)| v.vm == name).map(|(_, v)| v);
    let live = |name: &str| vm_of(name).is_some_and(|v| v.live);

    // 1. 진행 중인 단계를 관찰로 옮긴다.
    for step in &mut s.steps {
        let new_vm = step.new_vm.clone().unwrap_or_default();
        let seen = obs.vms.get(&new_vm);
        if let Some(node) = seen.and_then(|v| v.node.clone()) {
            step.node = Some(node);
        }
        match (step.phase, step.task) {
            (DrPhase::Restoring, Some(t)) => match outcome(obs, t, seen.is_some()) {
                TaskOutcome::Running => {}
                TaskOutcome::Succeeded if live(&step.vm) => set_phase(step, DrPhase::Waiting, now),
                TaskOutcome::Succeeded => set_phase(step, DrPhase::Starting, now),
                TaskOutcome::Failed(e) => {
                    set_phase(step, DrPhase::Failed, now);
                    step.error = Some(e);
                }
            },
            (DrPhase::Starting, Some(t)) => {
                match outcome(obs, t, seen.is_some_and(|v| v.running)) {
                    TaskOutcome::Running => {}
                    TaskOutcome::Succeeded => set_phase(step, DrPhase::Waiting, now),
                    TaskOutcome::Failed(e) => {
                        set_phase(step, DrPhase::Failed, now);
                        step.error = Some(e);
                    }
                }
            }
            _ => {}
        }
        // 시작이 끝난 VM은 같은 걸음에 기다림도 본다.
        if step.phase == DrPhase::Waiting {
            let Some(g) = group_of(&step.group).map(|i| groups[i]) else {
                continue;
            };
            let rules = vm_of(&step.vm).map(|v| v.ip.as_slice()).unwrap_or(&[]);
            match seen {
                None => {
                    set_phase(step, DrPhase::Failed, now);
                    step.error = Some(format!("VM {new_vm} is gone"));
                }
                Some(v) if wait_met(g.wait, v, rules) => set_phase(step, DrPhase::Ready, now),
                Some(_) if now >= step.at + u64::from(g.timeout_secs) => {
                    set_phase(step, DrPhase::TimedOut, now);
                    step.error = Some(format!(
                        "not {} after {} s",
                        match g.wait {
                            DrWait::Running => "running",
                            DrWait::Guest => "answering the guest agent",
                            DrWait::Ip => "reporting its address",
                        },
                        g.timeout_secs
                    ));
                }
                Some(_) => {}
            }
        }
    }

    // 2. 계획된 이전은 운영 쪽 준비가 모두 끝나야 그룹을 시작한다.
    if spec.kind == DrRunKind::FailoverPlanned && s.prepare_secs.is_none() && !s.cancel_requested {
        if !advance_prepare(spec, s, obs, actions) {
            check_stop(plan, s);
            if !s.stopping {
                return;
            }
        } else {
            s.prepare_secs = Some(now.saturating_sub(s.started_at));
        }
    }

    // 3. 시험의 거품 네트워크
    for (i, b) in s.bubbles.iter_mut().enumerate() {
        if b.state == BubbleState::Pending {
            if obs.networks.contains(&b.name) {
                b.state = BubbleState::Ready;
            } else {
                actions.push(DrAction::CreateBubble {
                    bubble: i,
                    name: b.name.clone(),
                    like: b.like.clone(),
                });
            }
        }
    }
    if let Some(b) = s.bubbles.iter().find(|b| b.state == BubbleState::Failed) {
        let e = b.error.clone().unwrap_or_default();
        s.error.get_or_insert(format!("network {}: {e}", b.name));
        s.stopping = true;
    }
    let bubbles_ready = s.bubbles.iter().all(|b| b.state == BubbleState::Ready);

    // 4. 취소·멈춤: 새 단계를 내지 않는다.
    check_stop(plan, s);
    if s.cancel_requested || s.stopping {
        actions.retain(|a| !matches!(a, DrAction::CreateBubble { .. }));
        for step in &mut s.steps {
            let skip = match step.phase {
                DrPhase::Pending | DrPhase::Preparing => true,
                // 취소는 진행 중인 Task 뒤의 단계도 없다.
                DrPhase::Starting | DrPhase::Restoring if step.task.is_none() => s.cancel_requested,
                DrPhase::Waiting => s.cancel_requested,
                _ => false,
            };
            if skip {
                set_phase(step, DrPhase::Skipped, now);
                step.error = Some(
                    if s.cancel_requested {
                        "cancelled"
                    } else {
                        "skipped after a failure"
                    }
                    .into(),
                );
            }
        }
    }

    // 5. 그룹 차례
    if !(s.cancel_requested || s.stopping) && bubbles_ready {
        loop {
            let g = s.group as usize;
            let Some(group) = groups.get(g) else { break };
            let done = s
                .steps
                .iter()
                .filter(|st| st.group == group.name)
                .all(|st| st.phase.is_terminal());
            if !done {
                dispatch(spec, s, g, now, true, actions);
                break;
            }
            if g + 1 >= groups.len() {
                s.group = groups.len() as u32;
                break;
            }
            if group.delay_secs > 0 {
                let until = *s
                    .delay_until
                    .get_or_insert(now + u64::from(group.delay_secs));
                if now < until {
                    break;
                }
            }
            s.delay_until = None;
            s.group += 1;
        }
    } else if s.stopping && !s.cancel_requested {
        // 멈춤: 새 VM은 없지만 진행 중인 VM(지금 그룹)은 끝까지 — 복원이 끝난 VM은 시작한다.
        let g = s.group as usize;
        if g < groups.len() {
            dispatch(spec, s, g, now, false, actions);
        }
    }

    // 6. 끝
    if s.steps.iter().all(|st| st.phase.is_terminal()) && actions.is_empty() {
        finish(spec, s, now, actions);
    }
}

/// `on_timeout: stop`이고 실패·시간 초과가 있으면 멈춤
fn check_stop(plan: &RecoveryPlanSpec, s: &mut DrRunStatus) {
    if plan.on_timeout == OnTimeout::Stop
        && s.steps
            .iter()
            .any(|st| matches!(st.phase, DrPhase::Failed | DrPhase::TimedOut))
    {
        s.stopping = true;
    }
}

/// 그룹 `g`의 VM에 복원·시작을 낸다(동시성 안에서, 계획 차례로). `new`가 거짓이면 `pending` VM은 시작하지 않고 진행 중인
/// VM의 다음 Task만 낸다.
fn dispatch(
    spec: &DrRunSpec,
    s: &mut DrRunStatus,
    g: usize,
    now: u64,
    new: bool,
    actions: &mut Vec<DrAction>,
) {
    let plan = &spec.plan_spec;
    let group = &plan.groups[g];
    let mut in_flight = s.steps.iter().filter(|st| st.phase.in_flight()).count();
    let mut per_node: BTreeMap<String, usize> = BTreeMap::new();
    for st in s.steps.iter().filter(|st| st.phase.in_flight()) {
        if let Some(n) = &st.node {
            *per_node.entry(n.clone()).or_default() += 1;
        }
    }
    let max = spec.max_parallel.max(1) as usize;
    let test = spec.kind == DrRunKind::Test;
    let bubble_map: BTreeMap<String, String> = s
        .bubbles
        .iter()
        .map(|b| (b.source.clone(), b.name.clone()))
        .collect();
    for i in 0..s.steps.len() {
        if s.steps[i].group != group.name {
            continue;
        }
        let Some(v) = group.vms.iter().find(|v| v.vm == s.steps[i].vm) else {
            continue;
        };
        let step = &mut s.steps[i];
        match (step.phase, step.task) {
            (DrPhase::Pending, _) if new => {
                if in_flight >= max {
                    continue;
                }
                if let Some(n) = &step.node
                    && per_node.get(n).copied().unwrap_or(0) >= PER_NODE_PARALLEL
                {
                    continue;
                }
                in_flight += 1;
                if let Some(n) = &step.node {
                    *per_node.entry(n.clone()).or_default() += 1;
                }
                step.new_vm = Some(if test {
                    test_vm_name(&v.vm)
                } else {
                    v.vm.clone()
                });
                set_phase(step, DrPhase::Restoring, now);
                actions.push(restore_action(spec, i, step, v, &bubble_map));
            }
            (DrPhase::Restoring, None) => {
                actions.push(restore_action(spec, i, step, v, &bubble_map))
            }
            (DrPhase::Starting, None) => actions.push(DrAction::Start {
                step: i,
                vm: step.new_vm.clone().unwrap_or_default(),
            }),
            _ => {}
        }
    }
}

fn restore_action(
    spec: &DrRunSpec,
    i: usize,
    step: &DrStep,
    v: &DrVm,
    bubbles: &BTreeMap<String, String>,
) -> DrAction {
    let plan = &spec.plan_spec;
    let test = spec.kind == DrRunKind::Test;
    let networks = if test {
        let mut m = plan.test_networks.clone();
        m.extend(bubbles.iter().map(|(k, v)| (k.clone(), v.clone())));
        m
    } else {
        plan.networks.clone()
    };
    DrAction::Restore {
        step: i,
        backup: step.backup_id.unwrap_or_default(),
        name: step.new_vm.clone().unwrap_or_default(),
        pool: v.pool.clone().unwrap_or_else(|| plan.pool.clone()),
        node: v.node.clone(),
        folder: if test {
            Some(test_folder(&spec.plan_name))
        } else {
            plan.folder.clone()
        },
        networks,
        security_groups: plan.security_groups.clone(),
        ip: v.ip.clone(),
        live: v.live,
        label: (
            if test { DR_TEST_LABEL } else { DR_LABEL }.to_string(),
            spec.plan_name.clone(),
        ),
    }
}

/// 계획된 이전의 운영 쪽 준비(설계 §3.2): 그룹 역순으로 shutdown(5분 뒤 stop) → 새 백업 → 이 클러스터로 복제 → 사본이
/// 오면 그 VM은 `pending`. 모두 끝났으면 참. 원격에 닿지 않으면 기다린다(사람이 취소할 수 있다). 뒤 그룹부터 보므로 한
/// 걸음에 뒤 그룹이 꺼진 것을 보고 앞 그룹의 shutdown을 낸다.
fn advance_prepare(
    spec: &DrRunSpec,
    s: &mut DrRunStatus,
    obs: &DrObservation,
    actions: &mut Vec<DrAction>,
) -> bool {
    let plan = &spec.plan_spec;
    let group_index: BTreeMap<&str, usize> = plan
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| (g.name.as_str(), i))
        .collect();
    let gi = |st: &DrStep| group_index.get(st.group.as_str()).copied().unwrap_or(0);
    if let Some(src) = &obs.source {
        let by_id: HashMap<Uuid, &Backup> = obs.backups.iter().map(|b| (b.id, b)).collect();
        let mut order: Vec<usize> = (0..s.steps.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(gi(&s.steps[i])));
        for i in order {
            let g = gi(&s.steps[i]);
            // 뒤 그룹이 모두 꺼졌는지(이 그룹의 shutdown을 시작해도 되는지)
            let later_stopped = s.steps.iter().filter(|st| gi(st) > g).all(|st| {
                st.phase != DrPhase::Preparing
                    || st.prepare.as_ref().is_some_and(|p| {
                        !matches!(p.stage, PrepareStage::Shutdown | PrepareStage::Stop)
                    })
            });
            // 단계가 할 일 없이 넘어가면 같은 걸음에 다음 단계도 본다.
            for _ in 0..4 {
                if !prepare_step(&mut s.steps[i], i, later_stopped, src, obs, &by_id, actions) {
                    break;
                }
            }
        }
    }
    s.steps.iter().all(|st| st.phase != DrPhase::Preparing)
}

/// 준비 중인 VM 하나를 한 단계 본다. 할 일 없이 다음 단계로 넘어갔으면 참.
fn prepare_step(
    step: &mut DrStep,
    i: usize,
    later_stopped: bool,
    src: &SourceObservation,
    obs: &DrObservation,
    by_id: &HashMap<Uuid, &Backup>,
    actions: &mut Vec<DrAction>,
) -> bool {
    let now = obs.now;
    if step.phase != DrPhase::Preparing {
        return false;
    }
    let Some(p) = step.prepare.as_mut() else {
        return false;
    };
    let vm = step.vm.clone();
    let mut failed = None;
    let mut moved = false;
    match p.stage {
        PrepareStage::Shutdown if !later_stopped => {}
        PrepareStage::Shutdown | PrepareStage::Stop => match src.vms.get(&vm) {
            None => failed = Some(format!("VM {vm} is not on the source cluster")),
            Some(true) => {
                p.stage = PrepareStage::Backup;
                p.requested_at = None;
                moved = true;
            }
            Some(false) => match (p.stage, p.requested_at) {
                (PrepareStage::Shutdown, None) => {
                    p.requested_at = Some(now);
                    actions.push(DrAction::ShutdownSource { step: i, vm });
                }
                (PrepareStage::Shutdown, Some(t)) if now >= t + SHUTDOWN_GRACE_SECS => {
                    p.stage = PrepareStage::Stop;
                    p.requested_at = Some(now);
                    actions.push(DrAction::StopSource { step: i, vm });
                }
                (PrepareStage::Stop, Some(t)) if now >= t + RETRY_SECS => {
                    p.requested_at = Some(now);
                    actions.push(DrAction::StopSource { step: i, vm });
                }
                _ => {}
            },
        },
        PrepareStage::Backup => match &p.source_backup {
            None => {
                if p.requested_at.is_none_or(|t| now >= t + RETRY_SECS) {
                    p.requested_at = Some(now);
                    actions.push(DrAction::BackupSource { step: i, vm });
                }
            }
            Some(name) => match src.backups.get(name) {
                Some(b) if b.error.is_some() => {
                    failed = Some(format!(
                        "source backup {name}: {}",
                        b.error.clone().unwrap_or_default()
                    ));
                }
                Some(b) if b.ready => {
                    p.stage = PrepareStage::Replicate;
                    p.requested_at = None;
                    moved = true;
                }
                _ => {}
            },
        },
        PrepareStage::Replicate => {
            let name = p.source_backup.clone().unwrap_or_default();
            let copy = obs.backups.iter().find(|b| {
                b.spec.origin.as_ref().is_some_and(|o| {
                    Some(o.backup) == p.source_backup_id
                        && obs.source_ca.as_deref() == Some(o.ca_fingerprint.as_str())
                }) && chain_ready(b, by_id)
            });
            if let Some(copy) = copy {
                p.stage = PrepareStage::Done;
                p.requested_at = None;
                step.backup = Some(copy.name.clone());
                step.backup_id = Some(copy.id);
                set_phase(step, DrPhase::Pending, now);
                return false;
            }
            match src.backups.get(&name).and_then(|b| b.replica_error.clone()) {
                Some(e) => failed = Some(format!("replicating {name}: {e}")),
                None if p.requested_at.is_none() => {
                    p.requested_at = Some(now);
                    actions.push(DrAction::ReplicateSource {
                        step: i,
                        backup: name,
                    });
                }
                None => {}
            }
        }
        PrepareStage::Done => {}
    }
    if let Some(e) = failed {
        set_phase(step, DrPhase::Failed, now);
        step.error = Some(e);
        return false;
    }
    moved
}

/// 실행을 끝낸다: 결과·끝난 시각·RTO. 취소한 시험은 뒷정리를 시작한다.
fn finish(spec: &DrRunSpec, s: &mut DrRunStatus, now: u64, actions: &mut Vec<DrAction>) {
    let ok = |p: DrPhase| matches!(p, DrPhase::Ready | DrPhase::Deleted);
    let all = s.steps.iter().all(|st| ok(st.phase));
    let any = s.steps.iter().any(|st| ok(st.phase));
    let bubbles_ok = s
        .bubbles
        .iter()
        .all(|b| !matches!(b.state, BubbleState::Failed));
    s.state = if s.cancel_requested {
        DrRunState::Cancelled
    } else if all && bubbles_ok && s.error.is_none() {
        DrRunState::Succeeded
    } else if any {
        DrRunState::Partial
    } else {
        DrRunState::Failed
    };
    s.finished_at = Some(now);
    if spec.kind != DrRunKind::Cleanup {
        s.rto_secs = s
            .steps
            .iter()
            .filter(|st| st.phase == DrPhase::Ready)
            .map(|st| st.at)
            .max()
            .map(|t| t.saturating_sub(s.started_at));
    }
    if spec.kind == DrRunKind::Test && s.cancel_requested {
        actions.push(DrAction::StartCleanup);
    }
}

/// 뒷정리: 시험 VM을 끄고 지운 뒤(동시성 안에서) 거품 네트워크를 지운다.
fn advance_cleanup(
    spec: &DrRunSpec,
    s: &mut DrRunStatus,
    obs: &DrObservation,
    actions: &mut Vec<DrAction>,
) {
    let now = obs.now;
    let max = spec.max_parallel.max(1) as usize;
    for step in &mut s.steps {
        let vm = step.new_vm.clone().unwrap_or_default();
        let present = obs.vms.contains_key(&vm);
        match (step.phase, step.task) {
            (DrPhase::Pending, _) if !present => set_phase(step, DrPhase::Deleted, now),
            (DrPhase::Deleting, Some(t)) => match outcome(obs, t, !present) {
                TaskOutcome::Running => {}
                TaskOutcome::Succeeded if !present => set_phase(step, DrPhase::Deleted, now),
                TaskOutcome::Succeeded => {
                    set_phase(step, DrPhase::Failed, now);
                    step.error = Some(format!("VM {vm} is still there"));
                }
                TaskOutcome::Failed(e) => {
                    set_phase(step, DrPhase::Failed, now);
                    step.error = Some(e);
                }
            },
            _ => {}
        }
    }
    let mut in_flight = s.steps.iter().filter(|st| st.phase.in_flight()).count();
    for (i, step) in s.steps.iter_mut().enumerate() {
        match (step.phase, step.task) {
            (DrPhase::Pending, _) if !s.cancel_requested && in_flight < max => {
                in_flight += 1;
                set_phase(step, DrPhase::Deleting, now);
                actions.push(DrAction::DeleteVm {
                    step: i,
                    vm: step.new_vm.clone().unwrap_or_default(),
                });
            }
            (DrPhase::Pending, _) if s.cancel_requested => {
                set_phase(step, DrPhase::Skipped, now);
                step.error = Some("cancelled".into());
            }
            (DrPhase::Deleting, None) => actions.push(DrAction::DeleteVm {
                step: i,
                vm: step.new_vm.clone().unwrap_or_default(),
            }),
            _ => {}
        }
    }
    if !s.steps.iter().all(|st| st.phase.is_terminal()) {
        return;
    }
    // VM이 모두 끝난 뒤 네트워크(VM이 쓰던 네트워크는 지우지 못한다)
    if !s.cancel_requested {
        for (i, b) in s.bubbles.iter_mut().enumerate() {
            if b.state != BubbleState::Ready {
                continue;
            }
            if obs.networks.contains(&b.name) {
                actions.push(DrAction::DeleteNetwork {
                    bubble: i,
                    name: b.name.clone(),
                });
            } else {
                b.state = BubbleState::Removed;
            }
        }
    }
    if actions.is_empty() {
        finish(spec, s, now, actions);
    }
}

#[cfg(test)]
mod tests;
