//! 단순 → SDN 네트워크 옮기기(계획 67 §9): 단순 네트워크(브리지·VLAN·VXLAN) S의 VM NIC들을 SDN 네트워크 T로 옮긴다.
//! 주소를 그대로 가져가고, VM마다 끈 채(`offline`)·다시 켜며(`restart`)·라이브(`live` — tap만 옮김)로, 대상이 오버레이면
//! 옮기는 동안 OVN `l2gateway` 다리가 두 쪽 L2를 잇는다. `network-migrate` Task가 실행하고 VM마다 상태를 적어 이어서 한다.
//! 미리 보기(dry-run)와 Task가 같은 순수 함수 [`plan`]을 쓴다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cloud_init::{CloudInitNet, IpConfig};
use crate::guest::GuestInfo;
use crate::network::NetworkSpec;
use crate::node::Node;
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::sdn::ChassisState;
use crate::vm::{MacAddr, Vm};

pub type NetworkMigration = Resource<NetworkMigrationSpec, NetworkMigrationStatus>;

/// `vms`의 수 상한
pub const MAX_VMS: usize = 1000;
/// 한 번에 옮기는 VM 수의 상한
pub const MAX_PARALLEL: u8 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NetworkMigrationSpec {
    /// 든 네트워크 폴더(`Folder` id — 원본 네트워크의 폴더). 없으면 네트워크 트리의 루트다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub folder: Option<Uuid>,
    /// 원본 단순 네트워크(브리지·VLAN·VXLAN) 이름
    pub source: String,
    /// 대상 SDN 네트워크(서브넷이 있는 오버레이 또는 공급자) 이름
    pub target: String,
    /// 옮길 VM. 비면 계획할 때 원본에 NIC가 있는 모든 VM이다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub vms: Vec<Uuid>,
    /// VM마다 방식. 없으면 `auto`(전원·MTU·주소로 고름)
    #[serde(default, skip_serializing_if = "MigrationMode::is_auto")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub mode: MigrationMode,
    /// 사용자가 주는 NIC 주소(게스트 에이전트·cloud-init보다 앞선다)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub addresses: Vec<MigrationAddress>,
    /// 옮기는 동안 L2 다리를 놓는지. 없으면 대상이 오버레이면 참이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bridge: Option<bool>,
    /// 다리 노드. 없으면 원본 브리지가 있는 chassis 가운데 옮길 VM이 가장 많은 노드다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bridge_node: Option<Uuid>,
    /// 게스트가 볼 MTU가 줄어도 `live`로 옮긴다(아니면 `restart`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub allow_mtu_shrink: bool,
    /// 한 번에 옮기는 VM 수(1~8). 없으면 1이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub parallel: Option<u8>,
}

impl ResourceSpec for NetworkMigrationSpec {
    const KIND: Kind = Kind::NetworkMigration;
    type Status = NetworkMigrationStatus;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum MigrationMode {
    #[default]
    Auto,
    Offline,
    Restart,
    Live,
}

impl MigrationMode {
    pub fn is_auto(&self) -> bool {
        *self == MigrationMode::Auto
    }
}

/// 사용자가 주는 NIC 하나의 주소
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct MigrationAddress {
    pub vm: Uuid,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub mac: MacAddr,
    /// 주소 체계마다 하나까지
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub ip: Vec<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NetworkMigrationStatus {
    pub phase: MigrationPhase,
    /// VM NIC마다의 계획과 상태(만들 때 계획을 적고 Task가 고친다)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub vms: Vec<VmMove>,
    /// 놓은 다리
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bridge: Option<MigrationBridge>,
    /// 도는(돈) `network-migrate` Task
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub task: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

impl NetworkMigrationStatus {
    /// 끝난 옮기기(`Finished`·`RolledBack`)는 원본 네트워크·VM을 더 잡지 않는다.
    pub fn is_done(&self) -> bool {
        self.phase.is_done()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum MigrationPhase {
    /// 계획만 했다(시작 전).
    #[default]
    Planned,
    Running,
    Paused,
    /// 옮길 VM을 다 옮겼다(끝내기 전 — 다리가 남아 있다).
    Moved,
    /// 끝냈다(다리를 걷었다). 기록으로 남는다.
    Finished,
    RollingBack,
    RolledBack,
    Failed,
}

impl MigrationPhase {
    pub fn is_done(self) -> bool {
        matches!(self, MigrationPhase::Finished | MigrationPhase::RolledBack)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MigrationPhase::Planned => "planned",
            MigrationPhase::Running => "running",
            MigrationPhase::Paused => "paused",
            MigrationPhase::Moved => "moved",
            MigrationPhase::Finished => "finished",
            MigrationPhase::RollingBack => "rolling-back",
            MigrationPhase::RolledBack => "rolled-back",
            MigrationPhase::Failed => "failed",
        }
    }
}

/// VM NIC 하나의 옮기기
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmMove {
    pub vm: Uuid,
    /// NIC 순번
    pub nic: usize,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub mac: MacAddr,
    /// 정한 방식
    pub mode: PlannedMode,
    /// 옮겨 갈 주소(`ipam`이면 Task가 고른 주소)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>, default))]
    pub ips: Vec<IpAddr>,
    pub address_source: AddressSource,
    /// 되돌리기가 되살리는 원본 NIC
    pub from: NicOrigin,
    #[serde(default)]
    pub state: MoveState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 마지막으로 상태를 바꾼 시각(유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub at: Option<u64>,
}

/// 옮기기 전 NIC(되돌리기가 되살린다)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NicOrigin {
    pub network: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub security_groups: Vec<String>,
    /// 지운 cloud-init NIC 항목
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cloud_init: Option<CloudInitNet>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum PlannedMode {
    Offline,
    Live,
    Restart,
    /// 막는 까닭이 있어 건너뛴다.
    Blocked,
}

/// 옮겨 갈 주소의 출처(앞이 먼저)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum AddressSource {
    /// 사용자 `addresses`
    Given,
    /// 게스트 에이전트가 이 MAC에 보고한, 대상 서브넷 안의 주소
    GuestAgent,
    /// 이 NIC의 cloud-init 고정 주소
    CloudInit,
    /// 대상 풀의 새 주소(주소가 바뀜)
    Ipam,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum MoveState {
    #[default]
    Pending,
    Moving,
    Moved,
    Failed,
    RollingBack,
    RolledBack,
}

/// 놓은 다리(노드와 그 노드의 임시 physnet `mig<h8>`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigrationBridge {
    pub node: Uuid,
    pub physnet: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MigrationError {
    #[error("the source and the target are the same network")]
    SameNetwork,
    #[error("give at most {MAX_VMS} VMs")]
    TooManyVms,
    #[error("VM {0} is given twice")]
    DuplicateVm(Uuid),
    #[error("addresses: VM {0} NIC {1} is given twice")]
    DuplicateAddress(Uuid, MacAddr),
    #[error("addresses: VM {0} NIC {1} needs one or two addresses, at most one per family")]
    AddressFamilies(Uuid, MacAddr),
    #[error("parallel {0} is out of range: use 1-{MAX_PARALLEL}")]
    Parallel(u8),
    #[error("bridge_node needs a bridge")]
    BridgeNodeWithoutBridge,
}

impl NetworkMigrationSpec {
    /// 스펙만으로 보는 검사. 원본이 단순 네트워크·대상이 서브넷 있는 SDN 네트워크인지, VM이 있는지는 API가 본다.
    pub fn validate(&self) -> Result<(), MigrationError> {
        if self.source == self.target {
            return Err(MigrationError::SameNetwork);
        }
        if self.vms.len() > MAX_VMS {
            return Err(MigrationError::TooManyVms);
        }
        for (i, vm) in self.vms.iter().enumerate() {
            if self.vms[..i].contains(vm) {
                return Err(MigrationError::DuplicateVm(*vm));
            }
        }
        for (i, a) in self.addresses.iter().enumerate() {
            if self.addresses[..i]
                .iter()
                .any(|o| o.vm == a.vm && o.mac == a.mac)
            {
                return Err(MigrationError::DuplicateAddress(a.vm, a.mac));
            }
            let v4 = a.ip.iter().filter(|ip| ip.to_canonical().is_ipv4()).count();
            if a.ip.is_empty() || v4 > 1 || a.ip.len() - v4 > 1 {
                return Err(MigrationError::AddressFamilies(a.vm, a.mac));
            }
        }
        if let Some(p) = self.parallel
            && !(1..=MAX_PARALLEL).contains(&p)
        {
            return Err(MigrationError::Parallel(p));
        }
        if self.bridge == Some(false) && self.bridge_node.is_some() {
            return Err(MigrationError::BridgeNodeWithoutBridge);
        }
        Ok(())
    }
}

// ---- 계획(미리 보기와 Task가 같은 함수) ----

/// 계획에 드는 VM 하나(원본에 NIC가 있는 VM)
#[derive(Debug, Clone, Copy)]
pub struct PlanVm<'a> {
    pub vm: &'a Vm,
    /// 실행 중(꺼짐이 아님)
    pub running: bool,
    /// 게스트 에이전트가 답한 것. None이면 에이전트가 없거나 답하지 않았다.
    pub guest: Option<&'a GuestInfo>,
    /// 부르는 사람이 이 VM의 `vm.config`를 가졌다.
    pub allowed: bool,
}

/// [`plan`]의 입력
#[derive(Debug, Clone, Copy)]
pub struct PlanInput<'a> {
    pub spec: &'a NetworkMigrationSpec,
    pub source: &'a NetworkSpec,
    pub target: &'a NetworkSpec,
    pub vms: &'a [PlanVm<'a>],
    pub nodes: &'a [Node],
    /// 대상 네트워크에서 Raft `ip`로 이미 잡힌 주소와 그 주인(VM·라우터·유동 IP·부하 분산기 id)
    pub taken: &'a BTreeMap<IpAddr, Uuid>,
    /// 끝나지 않은 다른 옮기기에 든 VM
    pub busy: &'a BTreeSet<Uuid>,
}

/// NIC 하나의 계획
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlannedNic {
    pub vm: Uuid,
    /// VM 이름
    pub name: String,
    pub nic: usize,
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub mac: Option<MacAddr>,
    pub mode: PlannedMode,
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub ips: Vec<IpAddr>,
    pub address_source: AddressSource,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

/// 다리 계획
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlannedBridge {
    /// 다리를 놓는다.
    pub needed: bool,
    /// 고른 다리 노드
    pub node: Option<Uuid>,
    /// 다리를 놓을 수 없는 까닭(그러면 옮기기를 시작하지 못한다)
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

/// 계획 전체
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigrationPlan {
    pub nics: Vec<PlannedNic>,
    pub bridge: PlannedBridge,
}

/// 게스트가 원본에서 보는 MTU: VXLAN은 그 네트워크 MTU, 브리지·VLAN은 1500(브리지 MTU는 노드마다라 Raft에 없다)
fn source_mtu(source: &NetworkSpec) -> u16 {
    source.mtu().unwrap_or(crate::network::DEFAULT_PROVIDER_MTU)
}

/// cloud-init 고정 주소 하나(`주소/길이`)의 주소
fn static_ip(c: Option<&IpConfig>) -> Option<IpAddr> {
    match c? {
        IpConfig::Static { address, .. } => address.split('/').next()?.parse().ok(),
        _ => None,
    }
}

/// 체계마다 하나만(앞의 것)
fn one_per_family(ips: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for ip in ips {
        let ip = ip.to_canonical();
        if !out.iter().any(|o| o.is_ipv4() == ip.is_ipv4()) {
            out.push(ip);
        }
    }
    out
}

/// NIC(MAC `mac`)가 옮겨 갈 주소와 출처(§9.2): 사용자 `given` → 게스트 에이전트가 이 MAC에 보고한 대상 서브넷 안의 주소
/// → 이 NIC의 cloud-init 고정 주소(대상 서브넷 안) → 없음(`ipam` — 대상 풀의 새 주소). 체계마다 하나다.
pub fn address_for(
    mac: Option<MacAddr>,
    target: &NetworkSpec,
    given: Option<&[IpAddr]>,
    guest: Option<&GuestInfo>,
    cloud_init: Option<&CloudInitNet>,
) -> (Vec<IpAddr>, AddressSource) {
    if let Some(ips) = given.filter(|g| !g.is_empty()) {
        return (one_per_family(ips.iter().copied()), AddressSource::Given);
    }
    let inside = |ip: &IpAddr| {
        target
            .subnets
            .iter()
            .any(|s| s.cidr.contains(ip.to_canonical()))
    };
    if let (Some(mac), Some(guest)) = (mac, guest) {
        let reported = guest
            .interfaces
            .iter()
            .filter(|n| {
                n.mac
                    .as_deref()
                    .and_then(|m| m.parse::<MacAddr>().ok())
                    .is_some_and(|m| m == mac)
            })
            .flat_map(|n| n.addresses.iter().map(|a| a.ip))
            .filter(inside);
        let ips = one_per_family(reported);
        if !ips.is_empty() {
            return (ips, AddressSource::GuestAgent);
        }
    }
    if let Some(ci) = cloud_init {
        let ips = one_per_family(
            [static_ip(ci.ipv4.as_ref()), static_ip(ci.ipv6.as_ref())]
                .into_iter()
                .flatten()
                .filter(inside),
        );
        if !ips.is_empty() {
            return (ips, AddressSource::CloudInit);
        }
    }
    (Vec::new(), AddressSource::Ipam)
}

/// 옮기기 계획(§9.2·§9.4): VM마다 원본 NIC마다 방식·주소·막는 까닭·경고와 다리. 막힌 NIC는 시작해도 건너뛴다.
pub fn plan(input: &PlanInput) -> MigrationPlan {
    let PlanInput {
        spec,
        source,
        target,
        vms,
        nodes,
        taken,
        busy,
    } = *input;
    let chassis = |node: Option<Uuid>| {
        node.and_then(|id| nodes.iter().find(|n| n.id == id))
            .and_then(|n| n.status.sdn.as_ref())
            .is_some_and(|s| s.chassis == ChassisState::Ready)
    };
    let (from_mtu, to_mtu) = (
        source_mtu(source),
        target.mtu().unwrap_or(crate::sdn::DEFAULT_SDN_MTU),
    );
    let shrinks = to_mtu < from_mtu;
    let mut nics: Vec<PlannedNic> = Vec::new();
    for pv in vms {
        let vm = pv.vm;
        let on_target = vm.spec.nics.iter().any(|n| n.network == spec.target);
        for (i, nic) in vm
            .spec
            .nics
            .iter()
            .enumerate()
            .filter(|(_, n)| n.network == spec.source)
        {
            let given = nic.mac.and_then(|mac| {
                spec.addresses
                    .iter()
                    .find(|a| a.vm == vm.id && a.mac == mac)
                    .map(|a| a.ip.as_slice())
            });
            let ci = vm.spec.cloud_init.as_ref().and_then(|c| c.networks.get(i));
            let (ips, address_source) = address_for(nic.mac, target, given, pv.guest, ci);
            let mut blockers = Vec::new();
            let mut warnings = Vec::new();
            if vm.spec.template {
                blockers.push("a template is not migrated (edit it instead)".to_string());
            }
            if !pv.allowed {
                blockers.push("no permission: vm.config on the VM is needed".to_string());
            }
            if busy.contains(&vm.id) {
                blockers.push("the VM is in another network migration".to_string());
            }
            if on_target {
                blockers.push(format!(
                    "another NIC of the VM is already on {:?}",
                    spec.target
                ));
            }
            if nic.mac.is_none() {
                blockers.push("the NIC has no MAC address yet".to_string());
            }
            for ip in &ips {
                match crate::ipam::nic_subnet(target, *ip) {
                    None => blockers.push(format!(
                        "address {ip} is outside the subnets of {:?}",
                        spec.target
                    )),
                    Some(s) if crate::ipam::reserved(s).contains(ip) => blockers.push(format!(
                        "address {ip} is reserved in {:?} (network, broadcast or gateway)",
                        spec.target
                    )),
                    Some(_) => {}
                }
                if let Some(holder) = taken.get(ip).filter(|h| **h != vm.id) {
                    blockers.push(format!("address {ip} is already in use by {holder}"));
                }
            }
            // 방식(§9.4)
            let live_reason = if !chassis(vm.status.node) {
                Some("the VM's node is not an SDN (OVN) chassis")
            } else if shrinks && !spec.allow_mtu_shrink {
                Some("the MTU the guest sees would shrink")
            } else if address_source == AddressSource::Ipam {
                Some("the address is not known")
            } else {
                None
            };
            let mode = if !pv.running {
                PlannedMode::Offline
            } else {
                match spec.mode {
                    MigrationMode::Offline => {
                        blockers.push("the VM is running (mode offline)".to_string());
                        PlannedMode::Offline
                    }
                    MigrationMode::Restart => PlannedMode::Restart,
                    MigrationMode::Live => {
                        if let Some(r) = live_reason {
                            blockers.push(format!("cannot move it live: {r}"));
                        }
                        PlannedMode::Live
                    }
                    MigrationMode::Auto => match live_reason {
                        None => PlannedMode::Live,
                        Some(_) => PlannedMode::Restart,
                    },
                }
            };
            if address_source == AddressSource::Ipam {
                warnings.push(
                    "the address changes: a new address comes from the target's pool".to_string(),
                );
                if pv.guest.is_none() {
                    warnings.push(
                        "no guest agent: the guest's current address is not known".to_string(),
                    );
                }
            }
            if shrinks {
                warnings.push(format!(
                    "the MTU shrinks from {from_mtu} to {to_mtu} (DHCP guests get the new MTU)"
                ));
            }
            if nic.security_groups.is_empty() {
                warnings.push(
                    "the NIC has no security groups: traffic is not filtered on SDN either"
                        .to_string(),
                );
            }
            match mode {
                PlannedMode::Live => {
                    warnings.push("about a second without network".to_string());
                }
                PlannedMode::Restart => {
                    warnings.push("the VM is shut down and started again".to_string());
                }
                _ => {}
            }
            nics.push(PlannedNic {
                vm: vm.id,
                name: vm.name.clone(),
                nic: i,
                mac: nic.mac,
                mode,
                ips,
                address_source,
                blockers,
                warnings,
            });
        }
    }
    // 단순 네트워크에는 IPAM이 없어 두 VM이 같은 주소를 가질 수 있다. 둘 다 막는다.
    let mut seen: BTreeMap<IpAddr, usize> = BTreeMap::new();
    for n in &nics {
        for ip in &n.ips {
            *seen.entry(*ip).or_default() += 1;
        }
    }
    for n in &mut nics {
        for ip in n.ips.clone() {
            if seen[&ip] > 1 {
                n.blockers
                    .push(format!("address {ip} is also planned for another NIC"));
            }
        }
        if !n.blockers.is_empty() {
            n.mode = PlannedMode::Blocked;
        }
    }
    let vm_nodes: BTreeMap<Uuid, Option<Uuid>> =
        vms.iter().map(|v| (v.vm.id, v.vm.status.node)).collect();
    let bridge = plan_bridge(spec, source, target, nodes, &nics, &vm_nodes);
    MigrationPlan { nics, bridge }
}

/// 다리 계획(§9.5)
fn plan_bridge(
    spec: &NetworkMigrationSpec,
    source: &NetworkSpec,
    target: &NetworkSpec,
    nodes: &[Node],
    nics: &[PlannedNic],
    vm_nodes: &BTreeMap<Uuid, Option<Uuid>>,
) -> PlannedBridge {
    let mut b = PlannedBridge::default();
    if target.is_provider() {
        if spec.bridge == Some(true) {
            b.error = Some("a bridge is only laid to an overlay target".to_string());
        } else {
            b.warnings.push(
                "the target is a provider network: no bridge is laid, so moved and remaining VMs reach each other only if both networks share the same L2"
                    .to_string(),
            );
        }
        return b;
    }
    b.needed = spec.bridge.unwrap_or(true);
    if !b.needed {
        b.warnings.push(
            "no bridge: moved VMs cannot reach the VMs that are still on the source".to_string(),
        );
        return b;
    }
    let bridge = source.bridge_name();
    let candidates: Vec<&Node> = nodes
        .iter()
        .filter(|n| {
            n.status
                .sdn
                .as_ref()
                .is_some_and(|s| s.chassis == ChassisState::Ready)
                && n.status.bridges.iter().any(|br| br.name == bridge)
        })
        .collect();
    b.node = match spec.bridge_node {
        Some(id) if candidates.iter().any(|n| n.id == id) => Some(id),
        Some(id) => {
            b.error = Some(format!(
                "node {id} cannot be the bridge node: it is not an SDN chassis with bridge {bridge:?}"
            ));
            None
        }
        None => {
            // 옮길(막히지 않은) VM이 가장 많은 노드, 같으면 id 순
            let count = |node: Uuid| {
                nics.iter()
                    .filter(|n| n.mode != PlannedMode::Blocked)
                    .filter(|n| vm_nodes.get(&n.vm).copied().flatten() == Some(node))
                    .count()
            };
            let best = candidates
                .iter()
                .map(|n| (count(n.id), std::cmp::Reverse(n.id)))
                .max()
                .map(|(_, std::cmp::Reverse(id))| id);
            if best.is_none() {
                b.error = Some(format!(
                    "no SDN chassis has the source bridge {bridge:?} to lay the migration bridge on"
                ));
            }
            best
        }
    };
    b.warnings.push(
        "a DHCP server on the source network also answers moved VMs over the bridge: reserve the same addresses there or turn it off while migrating"
            .to_string(),
    );
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeSpec;
    use crate::vm::VmSpec;
    use serde_json::json;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn spec(v: serde_json::Value) -> NetworkMigrationSpec {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn storage_format_round_trips_and_validates() {
        let bare = spec(json!({ "source": "lan", "target": "t-lan" }));
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"source":"lan","target":"t-lan"}"#
        );
        assert_eq!(bare.validate(), Ok(()));
        let full = json!({
            "folder": id(3),
            "source": "lan",
            "target": "t-lan",
            "vms": [id(1)],
            "mode": "live",
            "addresses": [{ "vm": id(1), "mac": "52:54:00:00:00:01", "ip": ["10.0.0.5", "2001:db8::5"] }],
            "bridge": true,
            "bridge_node": id(9),
            "allow_mtu_shrink": true,
            "parallel": 4
        });
        let s = spec(full.clone());
        assert_eq!(serde_json::to_value(&s).unwrap(), full);
        assert_eq!(s.validate(), Ok(()));
        assert!(
            serde_json::from_value::<NetworkMigrationSpec>(
                json!({ "source": "a", "target": "b", "x": 1 })
            )
            .is_err()
        );
        assert_eq!(NetworkMigrationSpec::KIND.as_str(), "network-migration");
        let mac: MacAddr = "52:54:00:00:00:01".parse().unwrap();
        for (v, err) in [
            (
                json!({ "source": "a", "target": "a" }),
                MigrationError::SameNetwork,
            ),
            (
                json!({ "source": "a", "target": "b", "vms": [id(1), id(1)] }),
                MigrationError::DuplicateVm(id(1)),
            ),
            (
                json!({ "source": "a", "target": "b", "addresses": [
                    { "vm": id(1), "mac": mac.to_string(), "ip": ["10.0.0.1"] },
                    { "vm": id(1), "mac": mac.to_string(), "ip": ["10.0.0.2"] }
                ] }),
                MigrationError::DuplicateAddress(id(1), mac),
            ),
            (
                json!({ "source": "a", "target": "b", "addresses": [{ "vm": id(1), "mac": mac.to_string(), "ip": ["10.0.0.1", "10.0.0.2"] }] }),
                MigrationError::AddressFamilies(id(1), mac),
            ),
            (
                json!({ "source": "a", "target": "b", "addresses": [{ "vm": id(1), "mac": mac.to_string(), "ip": [] }] }),
                MigrationError::AddressFamilies(id(1), mac),
            ),
            (
                json!({ "source": "a", "target": "b", "parallel": 0 }),
                MigrationError::Parallel(0),
            ),
            (
                json!({ "source": "a", "target": "b", "parallel": 9 }),
                MigrationError::Parallel(9),
            ),
            (
                json!({ "source": "a", "target": "b", "bridge": false, "bridge_node": id(2) }),
                MigrationError::BridgeNodeWithoutBridge,
            ),
        ] {
            assert_eq!(spec(v.clone()).validate(), Err(err), "{v}");
        }
        // 상태: 빈 상태는 `planned`만, 끝난 단계
        let st = NetworkMigrationStatus::default();
        assert_eq!(
            serde_json::to_string(&st).unwrap(),
            r#"{"phase":"planned"}"#
        );
        for (phase, done) in [
            (MigrationPhase::Running, false),
            (MigrationPhase::Failed, false),
            (MigrationPhase::Finished, true),
            (MigrationPhase::RolledBack, true),
        ] {
            assert_eq!(phase.is_done(), done);
            assert_eq!(serde_json::to_value(phase).unwrap(), phase.as_str());
        }
    }

    fn net(v: serde_json::Value) -> NetworkSpec {
        serde_json::from_value(v).unwrap()
    }

    fn node(n: u128, chassis: bool, bridges: &[&str]) -> Node {
        let mut node: Node = Resource::new(
            format!("n{n}"),
            serde_json::from_value::<NodeSpec>(json!({ "address": format!("10.0.0.{n}:8006") }))
                .unwrap(),
        );
        node.id = id(n);
        node.status.bridges = bridges
            .iter()
            .map(|b| crate::node::BridgeInfo {
                name: b.to_string(),
                vlan_filtering: false,
            })
            .collect();
        if chassis {
            node.status.sdn = Some(crate::sdn::NodeSdn {
                chassis: ChassisState::Ready,
                ..Default::default()
            });
        }
        node
    }

    fn vm(n: u128, node: Option<u128>, nics: serde_json::Value, extra: serde_json::Value) -> Vm {
        let mut v = json!({ "cpu": { "sockets": 1, "cores": 1 }, "memory_mib": 256, "machine": "q35", "nics": nics });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut vm: Vm = Resource::new(
            format!("vm{n}"),
            serde_json::from_value::<VmSpec>(v).unwrap(),
        );
        vm.id = id(n);
        vm.status.node = node.map(id);
        vm
    }

    fn guest(mac: &str, ips: &[&str]) -> GuestInfo {
        serde_json::from_value(json!({
            "collected_at": 0,
            "interfaces": [{ "name": "eth0", "mac": mac, "addresses": ips.iter().map(|i| json!({ "ip": i, "prefix": 24 })).collect::<Vec<_>>() }]
        }))
        .unwrap()
    }

    #[test]
    fn address_for_follows_the_order() {
        let t = net(json!({ "sdn": {}, "subnets": [{ "cidr": "10.0.0.0/24" }] }));
        let mac: MacAddr = "52:54:00:00:00:01".parse().unwrap();
        let g = guest(
            "52:54:00:00:00:01",
            &["10.0.0.5", "192.168.9.9", "10.0.0.6"],
        );
        let ci: CloudInitNet = serde_json::from_value(
            json!({ "ipv4": { "mode": "static", "address": "10.0.0.7/24" } }),
        )
        .unwrap();
        let given = ["10.0.0.4".parse().unwrap()];
        assert_eq!(
            address_for(Some(mac), &t, Some(&given), Some(&g), Some(&ci)),
            (vec!["10.0.0.4".parse().unwrap()], AddressSource::Given)
        );
        // 게스트가 보고한 대상 서브넷 안의 주소(체계마다 하나)
        assert_eq!(
            address_for(Some(mac), &t, None, Some(&g), Some(&ci)),
            (vec!["10.0.0.5".parse().unwrap()], AddressSource::GuestAgent)
        );
        // 다른 MAC의 보고는 쓰지 않는다(MAC 글자 크기는 상관없다).
        let other = guest("52:54:00:00:00:02", &["10.0.0.5"]);
        assert_eq!(
            address_for(Some(mac), &t, None, Some(&other), Some(&ci)),
            (vec!["10.0.0.7".parse().unwrap()], AddressSource::CloudInit)
        );
        let upper = guest("52:54:00:00:00:0A", &["10.0.0.8"]);
        assert_eq!(
            address_for(
                Some("52:54:00:00:00:0a".parse().unwrap()),
                &t,
                None,
                Some(&upper),
                None
            )
            .1,
            AddressSource::GuestAgent
        );
        // 대상 서브넷 밖의 cloud-init 주소는 쓰지 않는다.
        let out: CloudInitNet = serde_json::from_value(
            json!({ "ipv4": { "mode": "static", "address": "172.16.0.7/24" } }),
        )
        .unwrap();
        assert_eq!(
            address_for(Some(mac), &t, None, None, Some(&out)),
            (vec![], AddressSource::Ipam)
        );
    }

    struct Fixture {
        spec: NetworkMigrationSpec,
        source: NetworkSpec,
        target: NetworkSpec,
        nodes: Vec<Node>,
        vms: Vec<Vm>,
    }

    fn fixture() -> Fixture {
        Fixture {
            spec: spec(json!({ "source": "lan", "target": "t-lan" })),
            source: net(json!({ "bridge": "vmbr0" })),
            target: net(json!({ "sdn": {}, "subnets": [{ "cidr": "10.0.0.0/24" }] })),
            // n1·n2는 chassis이고 원본 브리지가 있다. n3는 chassis가 아니다.
            nodes: vec![
                node(1, true, &["vmbr0"]),
                node(2, true, &["vmbr0"]),
                node(3, false, &["vmbr0"]),
            ],
            vms: vec![
                // 꺼짐
                vm(
                    10,
                    None,
                    json!([{ "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:10" }]),
                    json!({}),
                ),
                // 켜짐, n2, cloud-init 고정 주소, 보안 그룹
                vm(
                    11,
                    Some(2),
                    json!([{ "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:11", "security_groups": ["web"] }]),
                    json!({ "cloud_init": { "networks": [{ "ipv4": { "mode": "static", "address": "10.0.0.11/24" } }] } }),
                ),
                // 켜짐, chassis 아닌 n3, 게스트 에이전트 주소
                vm(
                    12,
                    Some(3),
                    json!([{ "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:12" }]),
                    json!({}),
                ),
                // 템플릿
                vm(
                    13,
                    None,
                    json!([{ "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:13" }]),
                    json!({ "template": true }),
                ),
            ],
        }
    }

    fn run(
        f: &Fixture,
        running: &[u128],
        guests: &[(u128, GuestInfo)],
        taken: &[(&str, u128)],
        busy: &[u128],
    ) -> MigrationPlan {
        let pvs: Vec<PlanVm> = f
            .vms
            .iter()
            .map(|vm| PlanVm {
                vm,
                running: running.contains(&vm.id.as_u128()),
                guest: guests.iter().find(|(n, _)| id(*n) == vm.id).map(|(_, g)| g),
                allowed: true,
            })
            .collect();
        let taken: BTreeMap<IpAddr, Uuid> = taken
            .iter()
            .map(|(ip, h)| (ip.parse().unwrap(), id(*h)))
            .collect();
        let busy: BTreeSet<Uuid> = busy.iter().map(|n| id(*n)).collect();
        plan(&PlanInput {
            spec: &f.spec,
            source: &f.source,
            target: &f.target,
            vms: &pvs,
            nodes: &f.nodes,
            taken: &taken,
            busy: &busy,
        })
    }

    #[test]
    fn plan_picks_modes_addresses_blockers_and_the_bridge() {
        let f = fixture();
        let g12 = guest("52:54:00:00:00:12", &["10.0.0.12"]);
        let p = run(&f, &[11, 12], &[(12, g12.clone())], &[], &[]);
        let by = |n: u128| p.nics.iter().find(|x| x.vm == id(n)).unwrap();
        // 꺼짐 → offline, 주소를 모름 → ipam
        assert_eq!(by(10).mode, PlannedMode::Offline);
        assert_eq!(by(10).address_source, AddressSource::Ipam);
        assert!(
            by(10)
                .warnings
                .iter()
                .any(|w| w.contains("address changes"))
        );
        assert!(by(10).warnings.iter().any(|w| w.contains("no guest agent")));
        // 켜짐 + chassis + cloud-init 주소 + MTU 1500 → 1442가 줄어 restart(auto)
        assert_eq!(by(11).mode, PlannedMode::Restart);
        assert_eq!(by(11).address_source, AddressSource::CloudInit);
        assert_eq!(by(11).ips, vec!["10.0.0.11".parse::<IpAddr>().unwrap()]);
        assert!(
            by(11)
                .warnings
                .iter()
                .any(|w| w.contains("MTU shrinks from 1500 to 1442"))
        );
        assert!(
            !by(11)
                .warnings
                .iter()
                .any(|w| w.contains("security groups"))
        );
        // chassis가 아닌 노드 → restart
        assert_eq!(by(12).mode, PlannedMode::Restart);
        assert_eq!(by(12).address_source, AddressSource::GuestAgent);
        // 템플릿은 막힌다.
        assert_eq!(by(13).mode, PlannedMode::Blocked);
        // 다리: 오버레이 대상, 옮길 VM이 가장 많은 chassis(n2 — VM 11), chassis가 아닌 n3는 후보가 아니다.
        assert!(p.bridge.needed);
        assert_eq!(p.bridge.node, Some(id(2)));
        assert_eq!(p.bridge.error, None);

        // MTU가 줄어도 된다면 live
        let mut f2 = fixture();
        f2.spec.allow_mtu_shrink = true;
        let p = run(&f2, &[11, 12], &[], &[], &[]);
        assert_eq!(
            p.nics.iter().find(|x| x.vm == id(11)).unwrap().mode,
            PlannedMode::Live
        );
        // mode live인데 안 되는 VM은 막힌다(몰래 다시 켜지 않는다).
        f2.spec.mode = MigrationMode::Live;
        let p = run(&f2, &[11, 12], &[], &[], &[]);
        let n12 = p.nics.iter().find(|x| x.vm == id(12)).unwrap();
        assert_eq!(n12.mode, PlannedMode::Blocked);
        assert!(
            n12.blockers
                .iter()
                .any(|b| b.contains("not an SDN (OVN) chassis"))
        );
        // mode offline인데 켜져 있으면 막힌다.
        f2.spec.mode = MigrationMode::Offline;
        let p = run(&f2, &[11], &[], &[], &[]);
        assert_eq!(
            p.nics.iter().find(|x| x.vm == id(11)).unwrap().mode,
            PlannedMode::Blocked
        );
        assert_eq!(
            p.nics.iter().find(|x| x.vm == id(10)).unwrap().mode,
            PlannedMode::Offline
        );
    }

    #[test]
    fn plan_blocks_address_conflicts() {
        let mut f = fixture();
        // VM 10과 12가 같은 주소를 쓴다(단순 네트워크엔 IPAM이 없다).
        f.spec.addresses = vec![MigrationAddress {
            vm: id(10),
            mac: "52:54:00:00:00:10".parse().unwrap(),
            ip: vec!["10.0.0.12".parse().unwrap()],
        }];
        let g12 = guest("52:54:00:00:00:12", &["10.0.0.12"]);
        let p = run(&f, &[], &[(12, g12)], &[("10.0.0.11", 99)], &[12]);
        let by = |n: u128| p.nics.iter().find(|x| x.vm == id(n)).unwrap();
        assert_eq!(by(10).address_source, AddressSource::Given);
        assert!(by(10).blockers.iter().any(|b| b.contains("also planned")));
        assert!(by(12).blockers.iter().any(|b| b.contains("also planned")));
        assert!(
            by(12)
                .blockers
                .iter()
                .any(|b| b.contains("another network migration"))
        );
        // Raft에 이미 있는 주소
        assert!(by(11).blockers.iter().any(|b| b.contains("already in use")));
        // 서브넷 밖·예약 주소
        f.spec.addresses[0].ip = vec!["10.0.0.1".parse().unwrap()];
        let p = run(&f, &[], &[], &[], &[]);
        assert!(p.nics[0].blockers.iter().any(|b| b.contains("reserved")));
        f.spec.addresses[0].ip = vec!["10.9.0.1".parse().unwrap()];
        let p = run(&f, &[], &[], &[], &[]);
        assert!(
            p.nics[0]
                .blockers
                .iter()
                .any(|b| b.contains("outside the subnets"))
        );
        // 같은 VM의 다른 NIC가 대상에 이미 있다.
        let mut f = fixture();
        f.vms = vec![vm(
            20,
            None,
            json!([
                { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:20" },
                { "network": "t-lan", "model": "virtio", "mac": "52:54:00:00:00:21" }
            ]),
            json!({}),
        )];
        let p = run(&f, &[], &[], &[], &[]);
        assert_eq!(p.nics.len(), 1);
        assert_eq!(p.nics[0].mode, PlannedMode::Blocked);
    }

    #[test]
    fn plan_bridge_choices() {
        let mut f = fixture();
        f.spec.bridge_node = Some(id(1));
        assert_eq!(run(&f, &[], &[], &[], &[]).bridge.node, Some(id(1)));
        f.spec.bridge_node = Some(id(3));
        let b = run(&f, &[], &[], &[], &[]).bridge;
        assert_eq!(b.node, None);
        assert!(b.error.unwrap().contains("cannot be the bridge node"));
        f.spec.bridge_node = None;
        f.spec.bridge = Some(false);
        let b = run(&f, &[], &[], &[], &[]).bridge;
        assert!(!b.needed);
        assert!(b.warnings[0].contains("no bridge"));
        // 원본 브리지가 있는 chassis가 없다.
        let mut f = fixture();
        f.nodes = vec![node(3, false, &["vmbr0"])];
        assert!(
            run(&f, &[], &[], &[], &[])
                .bridge
                .error
                .unwrap()
                .contains("no SDN chassis")
        );
        // 공급자 대상: 다리 없음
        let mut f = fixture();
        f.target = net(
            json!({ "sdn": { "provider": { "physnet": "ext" } }, "subnets": [{ "cidr": "10.0.0.0/24" }] }),
        );
        let b = run(&f, &[], &[], &[], &[]).bridge;
        assert!(!b.needed);
        assert!(b.warnings[0].contains("provider network"));
        f.spec.bridge = Some(true);
        assert!(run(&f, &[], &[], &[], &[]).bridge.error.is_some());
    }
}
