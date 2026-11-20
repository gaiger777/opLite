//! OvnSync의 순수 부분(계획 65 §8.2~8.3): 바라는 NB 행([`NbState`]), NB에서 읽은 우리 행([`NbActual`]), 둘을 견준
//! 트랜잭션 연산([`plan`] — 네트워크마다 한 묶음, 한 트랜잭션), 펜싱 연산([`fence_ops`]).
//!
//! 우리 행은 `external_ids:orbivisor:cluster=<태그>`가 있는 행뿐이다. 그런 행만 읽고 바꾸고 지운다. 논리 스위치는
//! 이름(`opl-<네트워크 id>`), 포트는 이름(`Logical_Switch_Port.name`은 표에서 하나), DHCP_Options는 (네트워크 id,
//! CIDR)로 짝짓는다. 포트는 스위치의 강한 참조로만 살아 있으므로(뿌리 표가 아님) 포트를 넣는 연산과 스위치의 `ports`를
//! 바꾸는 연산은 한 트랜잭션에 둔다 — 그래서 나누는 단위가 네트워크다.
//!
//! 계획 66: 라우터마다 한 묶음(`Logical_Router`·라우터 포트·정적 경로·NAT·`HA_Chassis_Group`). 라우터 포트·경로·NAT·
//! `HA_Chassis`도 뿌리가 아니라 라우터·그룹의 강한 참조로만 산다. SDN NIC 보안 그룹은 그룹 목록마다 `Port_Group`
//! 한 묶음([`acl`]). 묶음 차례는 목록 Port_Group → 네트워크(포트의 Port_Group 멤버 포함) → 라우터 → 지울 스위치 → 지울
//! 라우터(그 그룹과 함께) → 지울 Port_Group → 지울 DHCP 행이다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use opl_driver::ovn::ovsdb::{
    cond, map_value, named_uuid, parse_bool, parse_int, parse_map, parse_strings, parse_uuids,
    set_value, string_set, uuid_value,
};
use opl_types::floating_ip::FloatingIp;
use opl_types::ipam;
use opl_types::network::{Ipv6Mode, Network, Subnet};
use opl_types::node::Node;
use opl_types::router::{Router, RouterExternal};
use opl_types::sdn::{gateway_ranking, router_mac};
use opl_types::security_group::{SecurityGroup, nic_policy};
use opl_types::vm::{PowerState, Vm, VmPhase};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::acl::{self, AclRow, ActualAcl, ActualPortGroup, PortGroupRow};
use super::lb::{self, ActualHealth, ActualLb, LbRow};
use super::qos::{self, ActualQos, QosNbRow};
use opl_types::load_balancer::LoadBalancer;

/// 클러스터 태그(주인 표시)
pub const CLUSTER_KEY: &str = "orbivisor:cluster";
/// Raft 리소스 id
pub const ID_KEY: &str = "orbivisor:id";
/// 네트워크 이름(보기용)
pub const NAME_KEY: &str = "orbivisor:name";
/// 네트워크 폴더 id(보기용)
pub const FOLDER_KEY: &str = "orbivisor:folder";
/// DHCP_Options의 서브넷 CIDR
pub const SUBNET_KEY: &str = "orbivisor:subnet";
/// 라우터 id(라우터 포트·경로·스위치의 라우터 포트 짝)
pub const ROUTER_KEY: &str = "orbivisor:router";
/// 유동 IP id(`dnat_and_snat` NAT)
pub const FIP_KEY: &str = "orbivisor:fip";

/// `NB_Global.external_ids`의 펜싱 토큰 키(`"<term>:<index>"`)
pub fn fence_key(tag: &str) -> String {
    format!("orbivisor:{tag}:fence")
}

/// 펜싱 토큰 `"<term>:<index>"`
pub fn parse_token(s: &str) -> Option<(u64, u64)> {
    let (term, index) = s.split_once(':')?;
    Some((term.parse().ok()?, index.parse().ok()?))
}

/// 네트워크 `id`의 논리 스위치 이름
pub fn switch_name(id: Uuid) -> String {
    format!("opl-{id}")
}

/// 라우터 `id`의 논리 라우터 이름
pub fn router_name(id: Uuid) -> String {
    format!("opl-r-{id}")
}

/// 라우터 인터페이스(네트워크 `network`)의 라우터 포트 이름
pub fn router_port_name(network: Uuid) -> String {
    format!("opl-lrp-{network}")
}

/// 라우터 인터페이스의 스위치 쪽 짝(`type=router`)
pub fn router_switch_port_name(network: Uuid) -> String {
    format!("opl-rsp-{network}")
}

/// 라우터 `router`의 분산 게이트웨이 포트(DGP) 이름
pub fn gateway_port_name(router: Uuid) -> String {
    format!("opl-gw-{router}")
}

/// DGP의 공급자 스위치 쪽 짝
pub fn gateway_switch_port_name(router: Uuid) -> String {
    format!("opl-rgw-{router}")
}

/// 라우터 `router`의 `HA_Chassis_Group` 이름
pub fn ha_group_name(router: Uuid) -> String {
    format!("opl-ha-{router}")
}

/// 공급자 네트워크 `network`의 localnet 포트 이름
pub fn localnet_port_name(network: Uuid) -> String {
    format!("opl-ln-{network}")
}

/// 바라는 NB 행(스위치마다 그 포트와 DHCP 행, 라우터마다 그 포트·경로·NAT·HA 그룹). 같은 입력이면 같은 값이다(스위치는
/// 네트워크 id 순, 라우터는 라우터 id 순).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NbState {
    pub switches: Vec<SwitchRow>,
    pub routers: Vec<RouterRow>,
    /// SDN NIC 보안 그룹의 목록 Port_Group(이름 순, 계획 66 §11)
    pub port_groups: Vec<PortGroupRow>,
    /// 부하 분산의 `Load_Balancer` 행(이름 순, 계획 67 §7.2)
    pub load_balancers: Vec<LbRow>,
}

/// 논리 라우터 하나(Router 하나, 계획 66 §9.1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterRow {
    pub router: Uuid,
    pub name: String,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
    pub ports: Vec<RouterPortRow>,
    pub routes: Vec<RouteRow>,
    pub nat: Vec<NatRow>,
    /// 게이트웨이 후보가 있을 때만
    pub ha: Option<HaGroupRow>,
    /// 붙일 `Load_Balancer` 행 이름(계획 67 — VIP 네트워크가 이 라우터의 인터페이스인 LB, 이름 순)
    pub load_balancers: Vec<String>,
}

/// 라우터 포트 하나(인터페이스 또는 DGP)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterPortRow {
    pub name: String,
    pub mac: String,
    pub networks: Vec<String>,
    pub enabled: Option<bool>,
    /// 라우터의 `HA_Chassis_Group`을 가리킨다(DGP).
    pub gateway: bool,
    pub options: BTreeMap<String, String>,
    pub ipv6_ra_configs: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

/// 정적 경로 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRow {
    pub ip_prefix: String,
    pub nexthop: String,
    pub external_ids: BTreeMap<String, String>,
}

/// NAT 하나(`snat`·`dnat_and_snat`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatRow {
    pub kind: String,
    pub external_ip: String,
    pub logical_ip: String,
    /// 분산 유동 IP의 NIC 논리 포트
    pub logical_port: Option<String>,
    /// 분산 유동 IP의 NIC MAC
    pub external_mac: Option<String>,
    pub external_ids: BTreeMap<String, String>,
}

/// `HA_Chassis_Group` 하나와 그 chassis(이름 = 노드 UUID, 우선순위)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaGroupRow {
    pub name: String,
    pub chassis: Vec<(String, u16)>,
    pub external_ids: BTreeMap<String, String>,
}

/// 논리 스위치 하나(SDN 네트워크 하나)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchRow {
    pub network: Uuid,
    pub name: String,
    pub external_ids: BTreeMap<String, String>,
    pub ports: Vec<PortRow>,
    pub dhcp: Vec<DhcpRow>,
    /// 붙일 `Load_Balancer` 행 이름(계획 67 — 이 네트워크가 인터페이스인 라우터의 LB, 이름 순)
    pub load_balancers: Vec<String>,
    /// 이 스위치 SDN NIC들의 `QoS` 행(계획 67 §8.1, 정렬됨)
    pub qos: Vec<QosNbRow>,
}

/// 논리 스위치 포트 하나(VM NIC 하나, 스테이지 4 — 계획 66부터 localnet·router 포트도)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRow {
    pub name: String,
    /// `type`(VM NIC는 빈 글, `localnet`, `router`)
    pub kind: String,
    pub addresses: Vec<String>,
    pub port_security: Vec<String>,
    /// 이 스위치의 DHCP 행(CIDR)
    pub dhcpv4: Option<String>,
    /// 이 스위치의 DHCPv6 행(CIDR)
    pub dhcpv6: Option<String>,
    pub enabled: Option<bool>,
    /// localnet 포트의 VLAN
    pub tag: Option<u16>,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
    /// 이 포트가 들 목록 Port_Group(보안 그룹을 건 VM NIC만, 계획 66 §11)
    pub port_group: Option<String>,
}

/// DHCP_Options 하나(서브넷 하나, 스테이지 4)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpRow {
    pub cidr: String,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

/// VM id(`orbivisor:vm`, 포트)
pub const VM_KEY: &str = "orbivisor:vm";
/// NIC 순번(`orbivisor:nic`, 포트 — 보기용)
pub const NIC_KEY: &str = "orbivisor:nic";
/// 포트를 바인딩할 수 있는 chassis(쉼표 목록이면 첫 것이 주 chassis — 라이브 마이그레이션)
pub const REQUESTED_CHASSIS: &str = "requested-chassis";
/// 추가 chassis가 포트를 열기 전에 기다릴 패킷
pub const ACTIVATION_STRATEGY: &str = "activation-strategy";
/// DHCP 임대 시간(초)
pub const LEASE_TIME: u32 = 86400;

/// 논리 포트 이름(노드의 OVS 포트 `iface-id`와 같다 — reconciler와 함께 쓰므로 opl-types에 있다)
pub use opl_types::sdn::port_name;

/// IPv4 서브넷 `subnet`(네트워크 `network`)의 DHCP_Options 칸(§8.3): 서버 주소 = 게이트웨이, 서버 MAC, 임대 시간, 망 MTU,
/// DNS 서버(IPv4만, 있을 때). `routed`(라우터 인터페이스이거나 공급자 네트워크 — 계획 66 §9.2)일 때만 `router`(= 게이트웨이)를
/// 넣는다. 라우터가 없는 오버레이 네트워크는 단순 네트워크 NIC를 함께 쓰는 VM의 기본 경로를 빼앗지 않게 넣지 않는다.
fn dhcp_options(network: &Network, subnet: &Subnet, routed: bool) -> BTreeMap<String, String> {
    let mut options = BTreeMap::from([
        ("server_id".to_string(), subnet.gateway().to_string()),
        (
            "server_mac".to_string(),
            ipam::server_mac(network.id).to_string(),
        ),
        ("lease_time".to_string(), LEASE_TIME.to_string()),
    ]);
    if routed {
        options.insert("router".into(), subnet.gateway().to_string());
    }
    if let Some(mtu) = network.spec.mtu() {
        options.insert("mtu".into(), mtu.to_string());
    }
    let dns: Vec<String> = subnet
        .dns_servers
        .iter()
        .filter(|ip| ip.is_ipv4())
        .map(ToString::to_string)
        .collect();
    if !dns.is_empty() {
        options.insert("dns_server".into(), format!("{{{}}}", dns.join(", ")));
    }
    options
}

/// IPv6 서브넷 `subnet`의 DHCPv6 칸(계획 66 §10, `ipv6_mode`가 `dhcpv6-*`일 때): 서버 id(DUID-LL) = 서버 MAC, DNS 서버(IPv6만),
/// 상태 없음이면 `dhcpv6_stateless`(주소는 주지 않음)
fn dhcpv6_options(network: &Network, subnet: &Subnet, mode: Ipv6Mode) -> BTreeMap<String, String> {
    let mut options = BTreeMap::from([(
        "server_id".to_string(),
        ipam::server_mac(network.id).to_string(),
    )]);
    let dns: Vec<String> = subnet
        .dns_servers
        .iter()
        .filter(|ip| !ip.is_ipv4())
        .map(ToString::to_string)
        .collect();
    if !dns.is_empty() {
        options.insert("dns_server".into(), format!("{{{}}}", dns.join(", ")));
    }
    if mode == Ipv6Mode::Dhcpv6Stateless {
        options.insert("dhcpv6_stateless".into(), "true".into());
    }
    options
}

/// DHCPv6 행을 두는 서브넷의 모드(`dhcpv6-stateful`·`dhcpv6-stateless`)
fn dhcpv6_mode(subnet: &Subnet) -> Option<Ipv6Mode> {
    subnet
        .ipv6_mode
        .filter(|m| matches!(m, Ipv6Mode::Dhcpv6Stateful | Ipv6Mode::Dhcpv6Stateless))
        .filter(|_| !subnet.is_ipv4())
}

/// 라우터 포트의 RA 칸(계획 66 §10): 주소 모드, 주기적으로 보냄, MTU(1280 이상만 — OVN이 그보다 작은 값을 넣지 않는다),
/// `rdnss`(서브넷 IPv6 DNS 서버의 첫 하나 — OVN은 하나만 받는다)
fn ra_configs(network: &Network, subnet: &Subnet, mode: Ipv6Mode) -> BTreeMap<String, String> {
    let address_mode = match mode {
        Ipv6Mode::Slaac => "slaac",
        Ipv6Mode::Dhcpv6Stateful => "dhcpv6_stateful",
        Ipv6Mode::Dhcpv6Stateless => "dhcpv6_stateless",
    };
    let mut ra = BTreeMap::from([
        ("address_mode".to_string(), address_mode.to_string()),
        ("send_periodic".to_string(), "true".to_string()),
    ]);
    if let Some(mtu) = network.spec.mtu().filter(|m| *m >= 1280) {
        ra.insert("mtu".into(), mtu.to_string());
    }
    if let Some(dns) = subnet.dns_servers.iter().find(|ip| !ip.is_ipv4()) {
        ra.insert("rdnss".into(), dns.to_string());
    }
    ra
}

/// VM의 포트를 바인딩할 chassis(노드)들: 노드에서 돌거나 켜지는 중이면 그 노드, 라이브 마이그레이션 중이면 원본과 대상,
/// 꺼져 있으면 없다. 클러스터에 없는 노드는 적지 않는다.
pub(crate) fn chassis_nodes(vm: &Vm, nodes: &BTreeSet<Uuid>) -> Vec<Uuid> {
    let active = matches!(
        vm.status.phase,
        VmPhase::Starting | VmPhase::Running | VmPhase::Stopping | VmPhase::Migrating
    );
    let here = if active {
        vm.status.node
    } else if vm.spec.power == PowerState::Running {
        // 켜는 중이지만 노드의 reconciler가 아직 상태를 쓰지 않았다(`status.node`는 지난번 노드일 수 있다).
        vm.spec.node
    } else {
        None
    };
    let Some(here) = here.filter(|n| nodes.contains(n)) else {
        return Vec::new();
    };
    match vm
        .status
        .migration
        .as_ref()
        .filter(|m| active && nodes.contains(&m.node))
    {
        Some(m) if m.node != here => vec![here, m.node],
        _ => vec![here],
    }
}

/// VM의 포트를 바인딩할 chassis(노드 UUID)와 추가 chassis가 열릴 때를 기다릴 패킷. 노드에서 돌거나 켜지는 중이면 그
/// 노드, 라이브 마이그레이션 중이면 `"원본,대상"`과 `rarp`(QEMU가 끝에 보내는 RARP가 대상을 연다), 꺼져 있으면 없다.
fn chassis(vm: &Vm, nodes: &BTreeSet<Uuid>) -> (Option<String>, Option<&'static str>) {
    match chassis_nodes(vm, nodes).as_slice() {
        [] => (None, None),
        [here] => (Some(here.to_string()), None),
        [here, to, ..] => (Some(format!("{here},{to}")), Some("rarp")),
    }
}

/// 공급자 네트워크(physnet `physnet`)로 나가는 라우터의 게이트웨이 후보(계획 66 §9.3): 게이트웨이 우선순위가 있고, 유지보수가
/// 아니며, 그 physnet이 `ready`인 노드와 그 우선순위. chassis 살아 있음은 보지 않는다(OVN이 BFD로 고른다).
pub(crate) fn gateway_candidates(nodes: &[Node], physnet: &str) -> Vec<(Uuid, u16)> {
    nodes
        .iter()
        .filter(|n| !n.spec.maintenance)
        .filter_map(|n| {
            let priority = n.spec.sdn.as_ref()?.gateway?;
            n.status.sdn.as_ref().filter(|s| s.physnet_ready(physnet))?;
            Some((n.id, priority))
        })
        .collect()
}

/// 쓸 수 있는 라우터 하나: 인터페이스(오버레이 SDN, 서브넷 있음, 다른 라우터가 먼저 갖지 않음), 외부(공급자 네트워크와
/// 그 서브넷 안의 외부 주소 `"주소/접두사"`), 게이트웨이 순위
struct RouterPlan<'a> {
    router: &'a Router,
    interfaces: Vec<&'a Network>,
    external: Option<External<'a>>,
}

struct External<'a> {
    network: &'a Network,
    spec: &'a RouterExternal,
    /// `"주소/접두사"`(DGP의 `networks`)
    addresses: Vec<String>,
    /// `(노드, HA_Chassis.priority)`. 비면 DGP를 닫고 NAT를 쓰지 않는다.
    gateways: Vec<(Uuid, u16)>,
}

impl External<'_> {
    fn enabled(&self) -> bool {
        !self.gateways.is_empty()
    }

    /// 체계가 IPv4인 외부 주소
    fn ipv4(&self) -> Option<IpAddr> {
        self.spec
            .ips
            .iter()
            .copied()
            .find(|ip| ip.is_ipv4() && ipam::nic_subnet(&self.network.spec, *ip).is_some())
    }
}

/// 클러스터 `tag`의 바라는 NB 상태(§8.3): 라우터·유동 IP가 없는 [`desired_routing`]이다(계획 65의 행 그대로).
pub fn desired(networks: &[Network], vms: &[Vm], nodes: &[Node], tag: &str) -> NbState {
    desired_routing(networks, vms, nodes, &[], &[], tag)
}

/// 클러스터 `tag`의 바라는 NB 상태(계획 65 §8.3, 계획 66 §8~10): SDN 네트워크마다 논리 스위치, 그 네트워크의 DHCP를 켠 IPv4
/// 서브넷([`NetworkSpec::subnet_dhcp`])과 `dhcpv6-*` IPv6 서브넷마다 DHCP_Options, 템플릿이 아닌 VM의 그 네트워크 NIC마다
/// (전원과 상관없이) 논리 포트(주소·포트 보안 = `"MAC IP4 IP6"`, 주소가 없으면 `"MAC"` — 그때 네트워크에 서브넷이 있으면
/// `enabled = false`, 주소의 서브넷이 DHCP면 그 DHCP 행, `requested-chassis`), 공급자 네트워크면 `localnet` 포트, 라우터
/// 인터페이스면 `router` 포트. 라우터마다 `Logical_Router`(인터페이스 포트, 외부가 있으면 분산 게이트웨이 포트와
/// `HA_Chassis_Group`, 정적 경로와 기본 경로, SNAT·유동 IP NAT). `nodes`는 클러스터의 노드다. 라우터·유동 IP·공급자
/// 네트워크·`ipv6_mode`가 없으면 계획 65와 같은 행이다.
pub fn desired_routing(
    networks: &[Network],
    vms: &[Vm],
    nodes: &[Node],
    routers: &[Router],
    floating_ips: &[FloatingIp],
    tag: &str,
) -> NbState {
    desired_all(networks, vms, nodes, routers, floating_ips, &[], tag)
}

/// [`desired_routing`]에 SDN NIC의 보안 그룹(계획 66 §11)을 더한 것: 그룹을 건 VM NIC마다 그 그룹 목록의 Port_Group
/// ([`acl::port_group`] — 같은 목록은 하나)과 포트의 `port_group`. `security_groups`는 클러스터의 그룹이다. 그룹을 건
/// SDN NIC가 없으면 [`desired_routing`]과 같은 행이다.
pub fn desired_all(
    networks: &[Network],
    vms: &[Vm],
    nodes: &[Node],
    routers: &[Router],
    floating_ips: &[FloatingIp],
    security_groups: &[SecurityGroup],
    tag: &str,
) -> NbState {
    desired_services(
        networks,
        vms,
        nodes,
        routers,
        floating_ips,
        security_groups,
        &[],
        tag,
    )
}

/// [`desired_all`]에 계획 67의 서비스를 더한 것: LB마다 프로토콜별 `Load_Balancer` 행([`lb::desired_rows`])과 그 붙이기
/// (VIP 네트워크 라우터와 그 모든 인터페이스 스위치), SDN NIC `qos`의 `QoS` 행([`qos::rows`]). LB와 `qos`가 없으면
/// [`desired_all`]과 같은 행이다.
#[allow(clippy::too_many_arguments)]
pub fn desired_services(
    networks: &[Network],
    vms: &[Vm],
    nodes: &[Node],
    routers: &[Router],
    floating_ips: &[FloatingIp],
    security_groups: &[SecurityGroup],
    load_balancers: &[LoadBalancer],
    tag: &str,
) -> NbState {
    let lists: std::cell::RefCell<BTreeMap<String, PortGroupRow>> = Default::default();
    let node_ids: BTreeSet<Uuid> = nodes.iter().map(|n| n.id).collect();
    let sdn: BTreeMap<&str, &Network> = networks
        .iter()
        .filter(|n| n.spec.is_sdn())
        .map(|n| (n.name.as_str(), n))
        .collect();

    // 라우터(id 순). 한 네트워크는 라우터 하나에만 붙는다(Raft 겹침 키가 지키고, 여기서는 먼저 온 라우터가 갖는다).
    let mut sorted: Vec<&Router> = routers.iter().collect();
    sorted.sort_by_key(|r| r.id);
    let mut owner: BTreeMap<&str, Uuid> = BTreeMap::new();
    let plans: Vec<RouterPlan> = sorted
        .into_iter()
        .map(|r| {
            let mut interfaces = Vec::new();
            for i in &r.spec.interfaces {
                let Some(n) = sdn.get(i.network.as_str()).copied() else {
                    continue;
                };
                if n.spec.is_provider()
                    || n.spec.subnets.is_empty()
                    || owner.contains_key(n.name.as_str())
                {
                    continue;
                }
                owner.insert(n.name.as_str(), r.id);
                interfaces.push(n);
            }
            interfaces.sort_by_key(|n| n.id);
            let external = r.spec.external.as_ref().and_then(|e| {
                let network = sdn.get(e.network.as_str()).copied()?;
                let physnet = network.spec.physnet()?;
                let addresses: Vec<String> = e
                    .ips
                    .iter()
                    .filter_map(|ip| {
                        let s = ipam::nic_subnet(&network.spec, *ip)?;
                        Some(format!("{ip}/{}", s.cidr.prefix))
                    })
                    .collect();
                (!addresses.is_empty()).then(|| External {
                    network,
                    spec: e,
                    addresses,
                    gateways: gateway_ranking(r.id, &gateway_candidates(nodes, physnet)),
                })
            });
            RouterPlan {
                router: r,
                interfaces,
                external,
            }
        })
        .collect();

    // 부하 분산(계획 67 §7.2): VIP 네트워크를 인터페이스로 가진 라우터에 붙인다(LB id 순).
    let mut lbs: Vec<&LoadBalancer> = load_balancers.iter().collect();
    lbs.sort_by_key(|l| l.id);
    let mut lb_rows: Vec<LbRow> = Vec::new();
    let mut router_lbs: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
    for l in lbs {
        let plan = owner
            .get(l.spec.network.as_str())
            .and_then(|r| plans.iter().find(|p| p.router.id == *r));
        let attach = plan.map(|p| lb::Attach {
            router: p.router.id,
            interfaces: &p.interfaces,
            external: p.external.as_ref().map(|e| e.network.name.as_str()),
        });
        let rows = lb::desired_rows(l, attach.as_ref(), vms, tag);
        if let Some(a) = &attach {
            router_lbs
                .entry(a.router)
                .or_default()
                .extend(rows.iter().map(|r| r.name.clone()));
        }
        lb_rows.extend(rows);
    }
    lb_rows.sort_by(|a, b| a.name.cmp(&b.name));
    for names in router_lbs.values_mut() {
        names.sort();
    }
    let mut switches: Vec<SwitchRow> =
        sdn.values()
            .map(|n| {
                let mut external_ids = BTreeMap::from([
                    (CLUSTER_KEY.to_string(), tag.to_string()),
                    (ID_KEY.to_string(), n.id.to_string()),
                    (NAME_KEY.to_string(), n.name.clone()),
                ]);
                if let Some(folder) = n.spec.folder {
                    external_ids.insert(FOLDER_KEY.into(), folder.to_string());
                }
                let dhcp_ids = |s: &Subnet| {
                    BTreeMap::from([
                        (CLUSTER_KEY.to_string(), tag.to_string()),
                        (ID_KEY.to_string(), n.id.to_string()),
                        (SUBNET_KEY.to_string(), s.cidr.to_string()),
                    ])
                };
                let routed = owner.contains_key(n.name.as_str()) || n.spec.is_provider();
                let mut dhcp: Vec<DhcpRow> = n
                    .spec
                    .subnets
                    .iter()
                    .filter(|s| s.is_ipv4() && n.spec.subnet_dhcp(s))
                    .map(|s| DhcpRow {
                        cidr: s.cidr.to_string(),
                        options: dhcp_options(n, s, routed),
                        external_ids: dhcp_ids(s),
                    })
                    .collect();
                dhcp.extend(n.spec.subnets.iter().filter_map(|s| {
                    Some(DhcpRow {
                        cidr: s.cidr.to_string(),
                        options: dhcpv6_options(n, s, dhcpv6_mode(s)?),
                        external_ids: dhcp_ids(s),
                    })
                }));
                let mut ports: Vec<PortRow> =
                    vms.iter()
                        .filter(|v| !v.spec.template)
                        .flat_map(|v| {
                            v.spec
                                .nics
                                .iter()
                                .enumerate()
                                .filter(|(_, nic)| nic.network == n.name)
                                .filter_map(move |(i, nic)| Some((v, i, nic, nic.mac?)))
                        })
                        .map(|(v, i, nic, mac)| {
                            let mut ips = nic.ips.clone();
                            ips.sort_by_key(|ip| !ip.is_ipv4());
                            let address = std::iter::once(mac.to_string())
                                .chain(ips.iter().map(ToString::to_string))
                                .collect::<Vec<_>>()
                                .join(" ");
                            let dhcpv4 = ips
                                .iter()
                                .filter(|ip| ip.is_ipv4())
                                .find_map(|ip| ipam::nic_subnet(&n.spec, *ip))
                                .filter(|s| n.spec.subnet_dhcp(s))
                                .map(|s| s.cidr.to_string());
                            let dhcpv6 = ips
                                .iter()
                                .filter(|ip| !ip.is_ipv4())
                                .find_map(|ip| ipam::nic_subnet(&n.spec, *ip))
                                .filter(|s| dhcpv6_mode(s).is_some())
                                .map(|s| s.cidr.to_string());
                            let mut options = BTreeMap::new();
                            let (requested, activation) = chassis(v, &node_ids);
                            if let Some(c) = requested {
                                options.insert(REQUESTED_CHASSIS.to_string(), c);
                            }
                            if let Some(a) = activation {
                                options.insert(ACTIVATION_STRATEGY.to_string(), a.to_string());
                            }
                            // 서브넷이 있는 네트워크에서 주소가 없는 NIC(서브넷을 나중에 더함, 다 쓰인 서브넷의 템플릿 되돌리기,
                            // 복원 실패)는 끈다: MAC만인 포트 보안은 어느 IP든 보내게 둔다(리뷰 1 I-4). `network ips --fill`이나
                            // VM 편집으로 주소를 받으면 켠다. 서브넷이 없는 네트워크는 L2만이라 MAC만이 뜻한 것이다.
                            let enabled = !ips.is_empty() || n.spec.subnets.is_empty();
                            // 보안 그룹을 건 NIC: 그룹 목록의 Port_Group(같은 목록의 NIC는 하나를 함께 쓴다, §11.1)
                            let port_group = (!nic.security_groups.is_empty()).then(|| {
                                let policy = nic_policy(&nic.security_groups, security_groups);
                                let key = policy.key();
                                let name = acl::port_group_name(tag, &key);
                                lists.borrow_mut().entry(name.clone()).or_insert_with(|| {
                                    acl::port_group(tag, &policy, security_groups)
                                });
                                name
                            });
                            PortRow {
                                name: port_name(v.id, mac),
                                kind: String::new(),
                                addresses: vec![address.clone()],
                                port_security: vec![address],
                                dhcpv4,
                                dhcpv6,
                                enabled: Some(enabled),
                                tag: None,
                                options,
                                external_ids: BTreeMap::from([
                                    (CLUSTER_KEY.to_string(), tag.to_string()),
                                    (VM_KEY.to_string(), v.id.to_string()),
                                    (NIC_KEY.to_string(), i.to_string()),
                                ]),
                                port_group,
                            }
                        })
                        .collect();
                // 공급자 네트워크: 노드 physnet(OVS 브리지 매핑)으로 나가는 localnet 포트(계획 66 §8)
                if let Some(p) = n.spec.provider() {
                    ports.push(PortRow {
                        name: localnet_port_name(n.id),
                        kind: "localnet".into(),
                        addresses: vec!["unknown".into()],
                        port_security: Vec::new(),
                        dhcpv4: None,
                        dhcpv6: None,
                        enabled: None,
                        tag: p.vlan,
                        options: BTreeMap::from([("network_name".to_string(), p.physnet.clone())]),
                        external_ids: BTreeMap::from([
                            (CLUSTER_KEY.to_string(), tag.to_string()),
                            (ID_KEY.to_string(), n.id.to_string()),
                        ]),
                        port_group: None,
                    });
                }
                // 라우터 인터페이스: 라우터 포트의 짝
                if let Some(r) = owner.get(n.name.as_str()) {
                    ports.push(router_switch_port(
                        router_switch_port_name(n.id),
                        router_port_name(n.id),
                        *r,
                        false,
                        tag,
                    ));
                }
                // 이 공급자 네트워크로 나가는 라우터의 게이트웨이 포트 짝(게이트웨이가 있을 때만 — 없으면 물리 망에 라우터를 보이지
                // 않는다). `nat-addresses=router`: 게이트웨이 chassis와 분산 유동 IP의 chassis가 GARP를 보낸다.
                for plan in &plans {
                    if plan
                        .external
                        .as_ref()
                        .is_some_and(|e| e.network.id == n.id && e.enabled())
                    {
                        let r = plan.router.id;
                        ports.push(router_switch_port(
                            gateway_switch_port_name(r),
                            gateway_port_name(r),
                            r,
                            true,
                            tag,
                        ));
                    }
                }
                ports.sort_by(|a, b| a.name.cmp(&b.name));
                // SDN NIC의 QoS(계획 67 §8.1)
                let mut qos_rows: Vec<QosNbRow> = vms
                    .iter()
                    .filter(|v| !v.spec.template)
                    .flat_map(|v| {
                        v.spec
                            .nics
                            .iter()
                            .filter(|nic| nic.network == n.name)
                            .filter_map(move |nic| {
                                Some(qos::rows(
                                    &port_name(v.id, nic.mac?),
                                    nic.qos.as_ref()?,
                                    tag,
                                ))
                            })
                    })
                    .flatten()
                    .collect();
                qos_rows.sort();
                SwitchRow {
                    network: n.id,
                    name: switch_name(n.id),
                    external_ids,
                    ports,
                    dhcp,
                    load_balancers: owner
                        .get(n.name.as_str())
                        .and_then(|r| router_lbs.get(r))
                        .cloned()
                        .unwrap_or_default(),
                    qos: qos_rows,
                }
            })
            .collect();
    switches.sort_by_key(|s| s.network);

    let by_id: BTreeMap<Uuid, &Node> = nodes.iter().map(|n| (n.id, n)).collect();
    let mut fips: Vec<&FloatingIp> = floating_ips.iter().collect();
    fips.sort_by_key(|f| f.id);
    let routers = plans
        .iter()
        .map(|plan| {
            let mut row = router_row(plan, vms, &fips, &by_id, &node_ids, tag);
            row.load_balancers = router_lbs.get(&plan.router.id).cloned().unwrap_or_default();
            row
        })
        .collect();
    NbState {
        switches,
        routers,
        port_groups: lists.into_inner().into_values().collect(),
        load_balancers: lb_rows,
    }
}

/// 라우터 포트와 짝인 스위치 포트(`type=router`)
fn router_switch_port(
    name: String,
    peer: String,
    router: Uuid,
    gateway: bool,
    tag: &str,
) -> PortRow {
    let mut options = BTreeMap::from([("router-port".to_string(), peer)]);
    if gateway {
        options.insert("nat-addresses".into(), "router".into());
    }
    PortRow {
        name,
        kind: "router".into(),
        addresses: vec!["router".into()],
        port_security: Vec::new(),
        dhcpv4: None,
        dhcpv6: None,
        enabled: None,
        tag: None,
        options,
        external_ids: BTreeMap::from([
            (CLUSTER_KEY.to_string(), tag.to_string()),
            (ROUTER_KEY.to_string(), router.to_string()),
        ]),
        port_group: None,
    }
}

/// 라우터 하나의 NB 행(§9.1~9.5)
fn router_row(
    plan: &RouterPlan,
    vms: &[Vm],
    fips: &[&FloatingIp],
    nodes: &BTreeMap<Uuid, &Node>,
    node_ids: &BTreeSet<Uuid>,
    tag: &str,
) -> RouterRow {
    let r = plan.router;
    let ours = || {
        BTreeMap::from([
            (CLUSTER_KEY.to_string(), tag.to_string()),
            (ROUTER_KEY.to_string(), r.id.to_string()),
        ])
    };
    let mut external_ids = BTreeMap::from([
        (CLUSTER_KEY.to_string(), tag.to_string()),
        (ID_KEY.to_string(), r.id.to_string()),
        (NAME_KEY.to_string(), r.name.clone()),
    ]);
    if let Some(folder) = r.spec.folder {
        external_ids.insert(FOLDER_KEY.into(), folder.to_string());
    }
    let mut ports: Vec<RouterPortRow> = plan
        .interfaces
        .iter()
        .map(|n| {
            let mut options = BTreeMap::new();
            if let Some(mtu) = n.spec.mtu() {
                options.insert("gateway_mtu".to_string(), mtu.to_string());
            }
            let ipv6_ra_configs = n
                .spec
                .subnets
                .iter()
                .filter(|s| !s.is_ipv4())
                .find_map(|s| Some(ra_configs(n, s, s.ipv6_mode?)))
                .unwrap_or_default();
            RouterPortRow {
                name: router_port_name(n.id),
                mac: router_mac(r.id, n.id).to_string(),
                networks: n
                    .spec
                    .subnets
                    .iter()
                    .map(|s| format!("{}/{}", s.gateway(), s.cidr.prefix))
                    .collect(),
                enabled: None,
                gateway: false,
                options,
                ipv6_ra_configs,
                external_ids: ours(),
            }
        })
        .collect();
    let mut routes: Vec<RouteRow> = r
        .spec
        .routes
        .iter()
        .map(|s| RouteRow {
            ip_prefix: s.destination.to_string(),
            nexthop: s.nexthop.to_string(),
            external_ids: ours(),
        })
        .collect();
    let mut nat = Vec::new();
    let mut ha = None;
    if let Some(e) = &plan.external {
        // 게이트웨이 후보가 없으면 포트를 닫는다: 그룹 없는 포트는 모든 chassis가 같은 라우터 MAC으로 물리 망에 답한다.
        ports.push(RouterPortRow {
            name: gateway_port_name(r.id),
            mac: router_mac(r.id, e.network.id).to_string(),
            networks: e.addresses.clone(),
            enabled: (!e.enabled()).then_some(false),
            gateway: e.enabled(),
            options: BTreeMap::new(),
            ipv6_ra_configs: BTreeMap::new(),
            external_ids: ours(),
        });
        // 외부 주소의 체계마다 기본 경로 → 외부 서브넷의 게이트웨이(물리 라우터)
        for ip in &e.spec.ips {
            if let Some(s) = ipam::nic_subnet(&e.network.spec, *ip) {
                routes.push(RouteRow {
                    ip_prefix: if ip.is_ipv4() { "0.0.0.0/0" } else { "::/0" }.into(),
                    nexthop: s.gateway().to_string(),
                    external_ids: ours(),
                });
            }
        }
        if e.enabled() {
            ha = Some(HaGroupRow {
                name: ha_group_name(r.id),
                chassis: e
                    .gateways
                    .iter()
                    .map(|(node, priority)| (node.to_string(), *priority))
                    .collect(),
                external_ids: BTreeMap::from([
                    (CLUSTER_KEY.to_string(), tag.to_string()),
                    (ID_KEY.to_string(), r.id.to_string()),
                ]),
            });
            nat = router_nat(plan, e, vms, fips, nodes, node_ids, tag);
        }
    }
    ports.sort_by(|a, b| a.name.cmp(&b.name));
    routes.sort_by(|a, b| (&a.ip_prefix, &a.nexthop).cmp(&(&b.ip_prefix, &b.nexthop)));
    routes.dedup();
    RouterRow {
        router: r.id,
        name: router_name(r.id),
        options: BTreeMap::from([
            ("dynamic_neigh_routers".to_string(), "true".to_string()),
            (
                "always_learn_from_arp_request".to_string(),
                "false".to_string(),
            ),
        ]),
        external_ids,
        ports,
        routes,
        nat,
        ha,
        load_balancers: Vec::new(),
    }
}

/// 라우터의 NAT(§9.4·9.5): SNAT(인터페이스의 IPv4 서브넷마다, `snat`이 켜져 있고 IPv4 외부 주소가 있을 때), 이 라우터로
/// 라우팅되는 NIC에 붙은 유동 IP의 `dnat_and_snat`. VM이 켜진 노드(마이그레이션이면 둘 다)가 모두 외부 physnet을 `ready`로
/// 알리면 분산(`logical_port`·`external_mac` = NIC MAC), 아니면 중앙(게이트웨이 chassis)이다.
fn router_nat(
    plan: &RouterPlan,
    e: &External,
    vms: &[Vm],
    fips: &[&FloatingIp],
    nodes: &BTreeMap<Uuid, &Node>,
    node_ids: &BTreeSet<Uuid>,
    tag: &str,
) -> Vec<NatRow> {
    let mut nat = Vec::new();
    if let Some(ext) = e.ipv4().filter(|_| e.spec.snat()) {
        for n in &plan.interfaces {
            for s in n.spec.subnets.iter().filter(|s| s.is_ipv4()) {
                nat.push(NatRow {
                    kind: "snat".into(),
                    external_ip: ext.to_string(),
                    logical_ip: s.cidr.to_string(),
                    logical_port: None,
                    external_mac: None,
                    external_ids: BTreeMap::from([(CLUSTER_KEY.to_string(), tag.to_string())]),
                });
            }
        }
    }
    let physnet = e.network.spec.physnet().unwrap_or_default();
    let mut taken = BTreeSet::new();
    for f in fips.iter().filter(|f| f.spec.network == e.network.name) {
        let (Some(address), Some(target)) = (f.spec.address.filter(IpAddr::is_ipv4), f.spec.target)
        else {
            continue;
        };
        let Some(vm) = vms.iter().find(|v| v.id == target.vm && !v.spec.template) else {
            continue;
        };
        let Some(nic) = vm.spec.nics.iter().find(|n| n.mac == Some(target.mac)) else {
            continue;
        };
        let Some(n) = plan.interfaces.iter().find(|n| n.name == nic.network) else {
            continue;
        };
        let Some(logical) = nic
            .ips
            .iter()
            .copied()
            .find(|ip| ip.is_ipv4() && ipam::nic_subnet(&n.spec, *ip).is_some())
        else {
            continue;
        };
        // 같은 NIC 주소에 유동 IP 둘(Raft가 막는다)이면 먼저 온 것만
        if !taken.insert(logical) {
            continue;
        }
        let on = chassis_nodes(vm, node_ids);
        let distributed = !on.is_empty()
            && on.iter().all(|id| {
                nodes
                    .get(id)
                    .and_then(|n| n.status.sdn.as_ref())
                    .is_some_and(|s| s.physnet_ready(physnet))
            });
        nat.push(NatRow {
            kind: "dnat_and_snat".into(),
            external_ip: address.to_string(),
            logical_ip: logical.to_string(),
            logical_port: distributed.then(|| port_name(vm.id, target.mac)),
            external_mac: distributed.then(|| target.mac.to_string()),
            external_ids: BTreeMap::from([
                (CLUSTER_KEY.to_string(), tag.to_string()),
                (FIP_KEY.to_string(), f.id.to_string()),
            ]),
        });
    }
    nat.sort_by(|a, b| {
        (&a.kind, &a.external_ip, &a.logical_ip).cmp(&(&b.kind, &b.external_ip, &b.logical_ip))
    });
    nat
}

// ---- NB에서 읽은 것 ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualSwitch {
    pub uuid: String,
    pub name: String,
    pub external_ids: BTreeMap<String, String>,
    pub ports: Vec<String>,
    /// 붙은 `Load_Balancer`(누구 것이든, 계획 67)
    pub load_balancers: Vec<String>,
    /// `QoS` 행(누구 것이든, 계획 67)
    pub qos_rules: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualPort {
    pub uuid: String,
    pub name: String,
    pub kind: String,
    pub addresses: Vec<String>,
    pub port_security: Vec<String>,
    pub dhcpv4: Option<String>,
    pub dhcpv6: Option<String>,
    pub enabled: Option<bool>,
    pub tag: Option<i64>,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualDhcp {
    pub uuid: String,
    pub cidr: String,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

/// `NB_Global` 행(없으면 northd가 아직 만들지 않은 빈 NB)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Global {
    pub uuid: String,
    pub external_ids: BTreeMap<String, String>,
    pub nb_cfg: i64,
    pub hv_cfg: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualRouter {
    pub uuid: String,
    pub name: String,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
    pub ports: Vec<String>,
    pub static_routes: Vec<String>,
    pub nat: Vec<String>,
    /// 붙은 `Load_Balancer`(누구 것이든, 계획 67)
    pub load_balancers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualRouterPort {
    pub uuid: String,
    pub name: String,
    pub mac: String,
    pub networks: Vec<String>,
    pub enabled: Option<bool>,
    pub ha_chassis_group: Option<String>,
    pub options: BTreeMap<String, String>,
    pub ipv6_ra_configs: BTreeMap<String, String>,
    /// northd가 쓰는 칸(`hosting-chassis` — 보기용)
    pub status: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualRoute {
    pub uuid: String,
    pub ip_prefix: String,
    pub nexthop: String,
    pub external_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualNat {
    pub uuid: String,
    pub kind: String,
    pub external_ip: String,
    pub logical_ip: String,
    pub logical_port: Option<String>,
    pub external_mac: Option<String>,
    pub external_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualHaGroup {
    pub uuid: String,
    pub name: String,
    pub ha_chassis: Vec<String>,
    pub external_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualHaChassis {
    pub uuid: String,
    pub chassis_name: String,
    pub priority: i64,
    pub external_ids: BTreeMap<String, String>,
}

/// NB에서 읽은 우리 행과 `NB_Global`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NbActual {
    pub switches: Vec<ActualSwitch>,
    pub ports: Vec<ActualPort>,
    pub dhcp: Vec<ActualDhcp>,
    pub global: Option<Global>,
    pub routers: Vec<ActualRouter>,
    pub router_ports: Vec<ActualRouterPort>,
    pub routes: Vec<ActualRoute>,
    pub nat: Vec<ActualNat>,
    pub ha_groups: Vec<ActualHaGroup>,
    pub ha_chassis: Vec<ActualHaChassis>,
    pub port_groups: Vec<ActualPortGroup>,
    pub acls: Vec<ActualAcl>,
    /// 계획 67: 우리 `Load_Balancer`·`Load_Balancer_Health_Check`·`QoS` 행
    pub load_balancers: Vec<ActualLb>,
    pub health_checks: Vec<ActualHealth>,
    pub qos: Vec<ActualQos>,
}

/// 우리 행을 읽는 연산들(한 트랜잭션): 스위치·포트·DHCP(태그 조건)와 `NB_Global`, 라우터·라우터 포트·경로·NAT·HA 그룹·
/// HA chassis, Port_Group·ACL, 부하 분산·건강 검사·QoS(태그 조건)
pub fn read_ops(tag: &str) -> Vec<Value> {
    let ours = || {
        vec![cond(
            "external_ids",
            "includes",
            map_value(&BTreeMap::from([(
                CLUSTER_KEY.to_string(),
                tag.to_string(),
            )])),
        )]
    };
    vec![
        json!({"op": "select", "table": "Logical_Switch", "where": ours(),
               "columns": ["_uuid", "name", "external_ids", "ports", "load_balancer", "qos_rules"]}),
        json!({"op": "select", "table": "Logical_Switch_Port", "where": ours(),
               "columns": ["_uuid", "name", "type", "addresses", "port_security", "dhcpv4_options",
                           "dhcpv6_options", "enabled", "tag", "options", "external_ids"]}),
        json!({"op": "select", "table": "DHCP_Options", "where": ours(),
               "columns": ["_uuid", "cidr", "options", "external_ids"]}),
        json!({"op": "select", "table": "NB_Global", "where": [],
               "columns": ["_uuid", "external_ids", "nb_cfg", "hv_cfg"]}),
        json!({"op": "select", "table": "Logical_Router", "where": ours(),
               "columns": ["_uuid", "name", "options", "external_ids", "ports", "static_routes", "nat",
                           "load_balancer"]}),
        json!({"op": "select", "table": "Logical_Router_Port", "where": ours(),
               "columns": ["_uuid", "name", "mac", "networks", "enabled", "ha_chassis_group", "options",
                           "ipv6_ra_configs", "status", "external_ids"]}),
        json!({"op": "select", "table": "Logical_Router_Static_Route", "where": ours(),
               "columns": ["_uuid", "ip_prefix", "nexthop", "external_ids"]}),
        json!({"op": "select", "table": "NAT", "where": ours(),
               "columns": ["_uuid", "type", "external_ip", "logical_ip", "logical_port", "external_mac",
                           "external_ids"]}),
        json!({"op": "select", "table": "HA_Chassis_Group", "where": ours(),
               "columns": ["_uuid", "name", "ha_chassis", "external_ids"]}),
        json!({"op": "select", "table": "HA_Chassis", "where": ours(),
               "columns": ["_uuid", "chassis_name", "priority", "external_ids"]}),
        json!({"op": "select", "table": "Port_Group", "where": ours(),
               "columns": ["_uuid", "name", "ports", "acls", "external_ids"]}),
        json!({"op": "select", "table": "ACL", "where": ours(),
               "columns": ["_uuid", "priority", "direction", "match", "action", "external_ids"]}),
        json!({"op": "select", "table": "Load_Balancer", "where": ours(),
               "columns": ["_uuid", "name", "protocol", "vips", "ip_port_mappings", "options", "external_ids",
                           "health_check"]}),
        json!({"op": "select", "table": "Load_Balancer_Health_Check", "where": ours(),
               "columns": ["_uuid", "vip", "options", "external_ids"]}),
        json!({"op": "select", "table": "QoS", "where": ours(),
               "columns": ["_uuid", "priority", "direction", "match", "action", "bandwidth", "external_ids"]}),
    ]
}

/// 라우터 보기의 가벼운 NB 읽기(계획 66 리뷰 1 웹 I1): 우리 `Logical_Router_Port`의 이름·`status`·주인 표시만 고르는
/// 조건과 칸(`router`가 있으면 그 라우터의 게이트웨이 포트 하나). [`read_ops`]처럼 12개 표를 다 읽지 않는다.
pub fn hosting_select(tag: &str, router: Option<Uuid>) -> (Vec<Value>, [&'static str; 3]) {
    let mut conditions = vec![cond(
        "external_ids",
        "includes",
        map_value(&BTreeMap::from([(
            CLUSTER_KEY.to_string(),
            tag.to_string(),
        )])),
    )];
    if let Some(id) = router {
        conditions.push(cond("name", "==", json!(gateway_port_name(id))));
    }
    (conditions, ["name", "status", "external_ids"])
}

/// [`hosting_select`]의 행 → 라우터 id → 게이트웨이 포트를 가진 chassis 이름(`status:hosting-chassis`가 있는 것만)
pub fn parse_hosting(rows: &[Map<String, Value>]) -> BTreeMap<Uuid, String> {
    rows.iter()
        .filter_map(|r| {
            let ids = parse_map(r.get("external_ids")?);
            let id: Uuid = ids.get(ROUTER_KEY)?.parse().ok()?;
            if text(r, "name") != gateway_port_name(id) {
                return None;
            }
            let host = parse_map(r.get("status")?).get("hosting-chassis")?.clone();
            Some((id, host))
        })
        .collect()
}

fn rows(v: &Value) -> Vec<Map<String, Value>> {
    v.get("rows")
        .unwrap_or(&Value::Null)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_object().cloned())
        .collect()
}

fn uuid_of(r: &Map<String, Value>) -> String {
    parse_uuids(&r["_uuid"])
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn text(r: &Map<String, Value>, c: &str) -> String {
    r.get(c).and_then(Value::as_str).unwrap_or_default().into()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// [`read_ops`]의 결과
pub fn parse_actual(results: &[Value]) -> NbActual {
    let switches = rows(&results[0])
        .iter()
        .map(|r| ActualSwitch {
            uuid: uuid_of(r),
            name: text(r, "name"),
            external_ids: parse_map(&r["external_ids"]),
            ports: sorted(parse_uuids(&r["ports"])),
            load_balancers: sorted(r.get("load_balancer").map(parse_uuids).unwrap_or_default()),
            qos_rules: sorted(r.get("qos_rules").map(parse_uuids).unwrap_or_default()),
        })
        .collect();
    let ports = rows(&results[1])
        .iter()
        .map(|r| ActualPort {
            uuid: uuid_of(r),
            name: text(r, "name"),
            kind: text(r, "type"),
            addresses: sorted(parse_strings(&r["addresses"])),
            port_security: sorted(parse_strings(&r["port_security"])),
            dhcpv4: parse_uuids(&r["dhcpv4_options"]).into_iter().next(),
            dhcpv6: r
                .get("dhcpv6_options")
                .and_then(|v| parse_uuids(v).into_iter().next()),
            enabled: parse_bool(&r["enabled"]),
            tag: r.get("tag").and_then(parse_int),
            options: parse_map(&r["options"]),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    let dhcp = rows(&results[2])
        .iter()
        .map(|r| ActualDhcp {
            uuid: uuid_of(r),
            cidr: text(r, "cidr"),
            options: parse_map(&r["options"]),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    // 행이 여럿이면(있을 수 없지만) 첫 것이다. 펜싱 `wait`은 그 행만 본다.
    let global = rows(&results[3]).first().map(|r| Global {
        uuid: uuid_of(r),
        external_ids: parse_map(&r["external_ids"]),
        nb_cfg: parse_int(&r["nb_cfg"]).unwrap_or(0),
        hv_cfg: parse_int(&r["hv_cfg"]).unwrap_or(0),
    });
    let none = Value::Null;
    let at = |i: usize| rows(results.get(i).unwrap_or(&none));
    let opt_text = |r: &Map<String, Value>, c: &str| {
        r.get(c)
            .map(parse_strings)
            .and_then(|v| v.into_iter().next())
    };
    let routers = at(4)
        .iter()
        .map(|r| ActualRouter {
            uuid: uuid_of(r),
            name: text(r, "name"),
            options: parse_map(&r["options"]),
            external_ids: parse_map(&r["external_ids"]),
            ports: sorted(parse_uuids(&r["ports"])),
            static_routes: sorted(parse_uuids(&r["static_routes"])),
            nat: sorted(parse_uuids(&r["nat"])),
            load_balancers: sorted(r.get("load_balancer").map(parse_uuids).unwrap_or_default()),
        })
        .collect();
    let router_ports = at(5)
        .iter()
        .map(|r| ActualRouterPort {
            uuid: uuid_of(r),
            name: text(r, "name"),
            mac: text(r, "mac"),
            networks: sorted(parse_strings(&r["networks"])),
            enabled: parse_bool(&r["enabled"]),
            ha_chassis_group: parse_uuids(&r["ha_chassis_group"]).into_iter().next(),
            options: parse_map(&r["options"]),
            ipv6_ra_configs: parse_map(&r["ipv6_ra_configs"]),
            status: parse_map(&r["status"]),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    let routes = at(6)
        .iter()
        .map(|r| ActualRoute {
            uuid: uuid_of(r),
            ip_prefix: text(r, "ip_prefix"),
            nexthop: text(r, "nexthop"),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    let nat = at(7)
        .iter()
        .map(|r| ActualNat {
            uuid: uuid_of(r),
            kind: text(r, "type"),
            external_ip: text(r, "external_ip"),
            logical_ip: text(r, "logical_ip"),
            logical_port: opt_text(r, "logical_port"),
            external_mac: opt_text(r, "external_mac"),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    let ha_groups = at(8)
        .iter()
        .map(|r| ActualHaGroup {
            uuid: uuid_of(r),
            name: text(r, "name"),
            ha_chassis: sorted(parse_uuids(&r["ha_chassis"])),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    let ha_chassis = at(9)
        .iter()
        .map(|r| ActualHaChassis {
            uuid: uuid_of(r),
            chassis_name: text(r, "chassis_name"),
            priority: parse_int(&r["priority"]).unwrap_or(0),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    let port_groups = at(10)
        .iter()
        .map(|r| ActualPortGroup {
            uuid: uuid_of(r),
            name: text(r, "name"),
            ports: sorted(parse_uuids(&r["ports"])),
            acls: sorted(parse_uuids(&r["acls"])),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    let acls = at(11)
        .iter()
        .map(|r| ActualAcl {
            uuid: uuid_of(r),
            acl: AclRow {
                priority: parse_int(&r["priority"]).unwrap_or(0),
                direction: text(r, "direction"),
                matches: text(r, "match"),
                action: text(r, "action"),
                external_ids: parse_map(&r["external_ids"]),
            },
        })
        .collect();
    let load_balancers = at(12)
        .iter()
        .map(|r| ActualLb {
            uuid: uuid_of(r),
            name: text(r, "name"),
            protocol: r
                .get("protocol")
                .map(parse_strings)
                .and_then(|v| v.into_iter().next()),
            vips: parse_map(&r["vips"]),
            ip_port_mappings: parse_map(&r["ip_port_mappings"]),
            options: parse_map(&r["options"]),
            external_ids: parse_map(&r["external_ids"]),
            health_check: sorted(parse_uuids(&r["health_check"])),
        })
        .collect();
    let health_checks = at(13)
        .iter()
        .map(|r| ActualHealth {
            uuid: uuid_of(r),
            vip: text(r, "vip"),
            options: parse_map(&r["options"]),
            external_ids: parse_map(&r["external_ids"]),
        })
        .collect();
    let qos = at(14)
        .iter()
        .map(|r| ActualQos {
            uuid: uuid_of(r),
            row: QosNbRow {
                direction: text(r, "direction"),
                priority: parse_int(&r["priority"]).unwrap_or(0),
                r#match: text(r, "match"),
                action: parse_map(&r["action"]),
                bandwidth: parse_map(&r["bandwidth"]),
                external_ids: parse_map(&r["external_ids"]),
            },
        })
        .collect();
    NbActual {
        switches,
        ports,
        dhcp,
        global,
        routers,
        router_ports,
        routes,
        nat,
        ha_groups,
        ha_chassis,
        port_groups,
        acls,
        load_balancers,
        health_checks,
        qos,
    }
}

// ---- 견주기 ----

/// 네트워크 하나(또는 지울 스위치·DHCP 행 하나)의 연산 묶음. 묶음 하나가 트랜잭션 하나다(리뷰 1 I-6 — 한 네트워크의
/// 실패가 다른 네트워크를 막지 않게). 묶음끼리는 named-uuid가 겹치지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// 보기용 이름(스위치 이름)
    pub key: String,
    pub ops: Vec<Value>,
}

fn by_uuid(uuid: &str) -> Value {
    json!([cond("_uuid", "==", uuid_value(uuid))])
}

fn opt_bool(b: Option<bool>) -> Value {
    set_value(b.map(|b| json!(b)).into_iter().collect())
}

/// 포트 행의 칸(넣기·바꾸기). `dhcp`·`dhcp6`는 DHCP 행의 참조(있는 행의 uuid나 넣는 행의 named-uuid)다. 계획 66의 칸
/// (`type`·`tag`·`dhcpv6_options`)은 넣을 때 빈 값이면 쓰지 않는다(계획 65 포트의 연산 그대로), 바꿀 때는 늘 쓴다.
fn port_columns(
    p: &PortRow,
    dhcp: Option<Value>,
    dhcp6: Option<Value>,
    update: bool,
) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("name".into(), json!(p.name));
    if update || !p.kind.is_empty() {
        row.insert("type".into(), json!(p.kind));
    }
    row.insert("addresses".into(), string_set(&p.addresses));
    row.insert("port_security".into(), string_set(&p.port_security));
    row.insert(
        "dhcpv4_options".into(),
        set_value(dhcp.into_iter().collect()),
    );
    if update || dhcp6.is_some() {
        row.insert(
            "dhcpv6_options".into(),
            set_value(dhcp6.into_iter().collect()),
        );
    }
    row.insert("enabled".into(), opt_bool(p.enabled));
    if update || p.tag.is_some() {
        row.insert(
            "tag".into(),
            set_value(p.tag.map(|t| json!(t)).into_iter().collect()),
        );
    }
    row.insert("options".into(), map_value(&p.options));
    row.insert("external_ids".into(), map_value(&p.external_ids));
    row
}

/// 포트 `p`(참조 `port`, 읽은 uuid `uuid`)의 목록 Port_Group 멤버 연산([`acl::membership_ops`]). 태그는 포트의 주인
/// 표시다.
fn membership(
    p: &PortRow,
    port: &Value,
    uuid: Option<&str>,
    have: &acl::PortGroupIndex,
) -> Vec<Value> {
    let tag = p
        .external_ids
        .get(CLUSTER_KEY)
        .map(String::as_str)
        .unwrap_or_default();
    acl::membership_ops(tag, port, uuid, p.port_group.as_deref(), have)
}

/// 참조 `want`(있는 행 uuid나 named-uuid, 없음)가 읽은 참조 `have`와 같은지. 넣는 행(named-uuid)은 늘 다르다.
fn same_ref(want: Option<&Value>, have: Option<&str>) -> bool {
    match want {
        None => have.is_none(),
        Some(v) => match v.as_array().map(Vec::as_slice) {
            Some([tag, Value::String(u)]) if tag == "uuid" => have == Some(u.as_str()),
            _ => false,
        },
    }
}

/// 바라는 것과 읽은 것을 견준 묶음들. 같으면 빈 목록이다.
pub fn plan(want: &NbState, have: &NbActual) -> Vec<Batch> {
    let mut out = Vec::new();
    let ports_by_name: BTreeMap<&str, &ActualPort> =
        have.ports.iter().map(|p| (p.name.as_str(), p)).collect();
    let our_ports: BTreeSet<&str> = have.ports.iter().map(|p| p.uuid.as_str()).collect();
    // 포트 uuid → 그 포트를 가진 우리 스위치
    let mut parent: BTreeMap<&str, &ActualSwitch> = BTreeMap::new();
    for s in &have.switches {
        for p in &s.ports {
            parent.insert(p.as_str(), s);
        }
    }
    let wanted: BTreeSet<&str> = want.switches.iter().map(|s| s.name.as_str()).collect();
    // 바라는 포트(이름). 다른 스위치로 옮기는 포트는 새 스위치의 묶음만 옮긴다 — 옛 스위치의 묶음이 먼저 빼면 강한
    // 참조를 잃은 포트가 지워져 새 묶음이 실패한다(리뷰 1 m-3).
    let wanted_ports: BTreeSet<&str> = want
        .switches
        .iter()
        .flat_map(|s| s.ports.iter().map(|p| p.name.as_str()))
        .collect();
    let port_names: BTreeMap<&str, &str> = have
        .ports
        .iter()
        .map(|p| (p.uuid.as_str(), p.name.as_str()))
        .collect();
    let wanted_networks: BTreeSet<String> = want
        .switches
        .iter()
        .map(|s| s.network.to_string())
        .collect();
    let pg_index = acl::PortGroupIndex::new(&have.port_groups, &have.acls);
    // 목록 Port_Group마다 한 묶음(네트워크 묶음보다 먼저 — §11.3: 포트를 넣는 네트워크 묶음은 Port_Group이 있어야 간다)
    for (n, pg) in want.port_groups.iter().enumerate() {
        let ops = acl::plan_port_group(n, pg, &pg_index);
        if !ops.is_empty() {
            out.push(Batch {
                key: pg.name.clone(),
                ops,
            });
        }
    }
    // LB마다 한 묶음(네트워크 묶음보다 먼저 — 새 LB 행을 이미 있는 스위치·라우터에 같은 트랜잭션에서 붙인다, 계획 67 §7.2)
    let mut by_lb: BTreeMap<Uuid, Vec<&LbRow>> = BTreeMap::new();
    for r in &want.load_balancers {
        by_lb.entry(r.lb).or_default().push(r);
    }
    for (n, (id, rows)) in by_lb.iter().enumerate() {
        let ops = lb::plan_lb(n, *id, rows, want, have);
        if !ops.is_empty() {
            out.push(Batch {
                key: format!("opl-lb-{id}"),
                ops,
            });
        }
    }

    for (n, sw) in want.switches.iter().enumerate() {
        let mut ops = Vec::new();
        let net = sw.network.to_string();
        // 같은 이름의 우리 스위치: 첫 것을 쓰고 나머지는 지운다(포트는 따라 사라진다).
        let mut same = have.switches.iter().filter(|s| s.name == sw.name);
        let current = same.next();
        for extra in same {
            ops.push(
                json!({"op": "delete", "table": "Logical_Switch", "where": by_uuid(&extra.uuid)}),
            );
        }

        // DHCP 행: (네트워크, CIDR)로 짝짓는다.
        let mut dhcp_ref: BTreeMap<&str, Value> = BTreeMap::new();
        let mut kept_dhcp = BTreeSet::new();
        for (i, d) in sw.dhcp.iter().enumerate() {
            let found = have.dhcp.iter().find(|a| {
                a.external_ids.get(ID_KEY) == Some(&net)
                    && a.cidr == d.cidr
                    && !kept_dhcp.contains(&a.uuid)
            });
            match found {
                Some(a) => {
                    kept_dhcp.insert(a.uuid.clone());
                    if a.options != d.options || a.external_ids != d.external_ids {
                        ops.push(json!({"op": "update", "table": "DHCP_Options", "where": by_uuid(&a.uuid),
                            "row": {"options": map_value(&d.options), "external_ids": map_value(&d.external_ids)}}));
                    }
                    dhcp_ref.insert(&d.cidr, uuid_value(&a.uuid));
                }
                None => {
                    let name = format!("d{n}_{i}");
                    ops.push(
                        json!({"op": "insert", "table": "DHCP_Options", "uuid-name": name,
                        "row": {"cidr": d.cidr, "options": map_value(&d.options),
                                "external_ids": map_value(&d.external_ids)}}),
                    );
                    dhcp_ref.insert(&d.cidr, named_uuid(&name));
                }
            }
        }
        for a in have
            .dhcp
            .iter()
            .filter(|a| a.external_ids.get(ID_KEY) == Some(&net) && !kept_dhcp.contains(&a.uuid))
        {
            ops.push(json!({"op": "delete", "table": "DHCP_Options", "where": by_uuid(&a.uuid)}));
        }

        // 포트
        let mut add: Vec<Value> = Vec::new();
        let mut remove: Vec<Value> = Vec::new();
        let mut kept_ports = BTreeSet::new();
        for (i, p) in sw.ports.iter().enumerate() {
            let dhcp = p.dhcpv4.as_deref().and_then(|c| dhcp_ref.get(c).cloned());
            let dhcp6 = p.dhcpv6.as_deref().and_then(|c| dhcp_ref.get(c).cloned());
            match ports_by_name.get(p.name.as_str()) {
                Some(a) => {
                    let same = a.kind == p.kind
                        && a.addresses == sorted(p.addresses.clone())
                        && a.port_security == sorted(p.port_security.clone())
                        && same_ref(dhcp.as_ref(), a.dhcpv4.as_deref())
                        && same_ref(dhcp6.as_ref(), a.dhcpv6.as_deref())
                        && a.enabled == p.enabled
                        && a.tag == p.tag.map(i64::from)
                        && a.options == p.options
                        && a.external_ids == p.external_ids;
                    if !same {
                        ops.push(json!({"op": "update", "table": "Logical_Switch_Port",
                            "where": by_uuid(&a.uuid), "row": port_columns(p, dhcp, dhcp6, true)}));
                    }
                    ops.extend(membership(
                        p,
                        &uuid_value(&a.uuid),
                        Some(&a.uuid),
                        &pg_index,
                    ));
                    let in_this = current.is_some_and(|c| c.ports.contains(&a.uuid));
                    if !in_this {
                        // 다른 스위치에서 옮겨 온다(네트워크를 바꾼 NIC — 포트 이름이 같다).
                        if let Some(old) = parent.get(a.uuid.as_str()) {
                            ops.push(json!({"op": "mutate", "table": "Logical_Switch", "where": by_uuid(&old.uuid),
                                "mutations": [["ports", "delete", set_value(vec![uuid_value(&a.uuid)])]]}));
                        }
                        add.push(uuid_value(&a.uuid));
                    }
                    kept_ports.insert(a.uuid.as_str());
                }
                None => {
                    let name = format!("p{n}_{i}");
                    ops.push(
                        json!({"op": "insert", "table": "Logical_Switch_Port", "uuid-name": name,
                        "row": port_columns(p, dhcp, dhcp6, false)}),
                    );
                    ops.extend(membership(p, &named_uuid(&name), None, &pg_index));
                    add.push(named_uuid(&name));
                }
            }
        }
        if let Some(c) = current {
            for u in &c.ports {
                // 우리 포트가 아닌 것(관리자가 넣은 포트)은 둔다.
                let moved = port_names
                    .get(u.as_str())
                    .is_some_and(|name| wanted_ports.contains(name));
                if our_ports.contains(u.as_str()) && !kept_ports.contains(u.as_str()) && !moved {
                    remove.push(uuid_value(u));
                }
            }
        }

        // QoS 행(계획 67 §8.1 — 포트와 한 트랜잭션)
        let (qos_ops, qos_add, qos_remove) = qos::plan(
            n,
            &sw.qos,
            current.map_or(&[][..], |c| c.qos_rules.as_slice()),
            &have.qos,
        );
        ops.extend(qos_ops);
        match current {
            None => {
                let mut row = Map::new();
                row.insert("name".into(), json!(sw.name));
                row.insert("external_ids".into(), map_value(&sw.external_ids));
                row.insert("ports".into(), set_value(add));
                // 계획 67의 칸은 비면 쓰지 않는다(계획 66 스위치의 연산 그대로).
                let lbs = lb::attach_insert(&sw.load_balancers, have);
                if !lbs.is_empty() {
                    row.insert("load_balancer".into(), set_value(lbs));
                }
                if !qos_add.is_empty() {
                    row.insert("qos_rules".into(), set_value(qos_add));
                }
                ops.push(
                    json!({"op": "insert", "table": "Logical_Switch", "uuid-name": format!("s{n}"),
                    "row": row}),
                );
            }
            Some(c) => {
                let (lb_add, lb_remove) =
                    lb::attach_changes(&sw.load_balancers, &c.load_balancers, have);
                mutate_set(
                    &mut ops,
                    "Logical_Switch",
                    &c.uuid,
                    "load_balancer",
                    lb_remove,
                    lb_add,
                );
                mutate_set(
                    &mut ops,
                    "Logical_Switch",
                    &c.uuid,
                    "qos_rules",
                    qos_remove,
                    qos_add,
                );
                if c.external_ids != sw.external_ids {
                    ops.push(
                        json!({"op": "update", "table": "Logical_Switch", "where": by_uuid(&c.uuid),
                        "row": {"external_ids": map_value(&sw.external_ids)}}),
                    );
                }
                if !remove.is_empty() {
                    ops.push(
                        json!({"op": "mutate", "table": "Logical_Switch", "where": by_uuid(&c.uuid),
                        "mutations": [["ports", "delete", set_value(remove)]]}),
                    );
                }
                if !add.is_empty() {
                    ops.push(
                        json!({"op": "mutate", "table": "Logical_Switch", "where": by_uuid(&c.uuid),
                        "mutations": [["ports", "insert", set_value(add)]]}),
                    );
                }
            }
        }
        if !ops.is_empty() {
            out.push(Batch {
                key: sw.name.clone(),
                ops,
            });
        }
    }

    // 라우터마다 한 묶음(스위치 묶음 뒤 — 라우터 포트의 짝인 스위치 포트는 이름으로만 잇는다)
    for (n, r) in want.routers.iter().enumerate() {
        let ops = plan_router(n, r, have);
        if !ops.is_empty() {
            out.push(Batch {
                key: r.name.clone(),
                ops,
            });
        }
    }

    // 바라지 않는 우리 스위치(포트는 따라 사라진다)와 그 네트워크의 DHCP 행
    for s in have
        .switches
        .iter()
        .filter(|s| !wanted.contains(s.name.as_str()))
    {
        out.push(Batch {
            key: s.name.clone(),
            ops: vec![
                json!({"op": "delete", "table": "Logical_Switch", "where": by_uuid(&s.uuid)}),
            ],
        });
    }
    // 바라지 않는 라우터(행이 남은 라우터 id마다 한 묶음 — 라우터와 그 HA 그룹)
    let wanted_routers: BTreeSet<String> =
        want.routers.iter().map(|r| r.router.to_string()).collect();
    let stale_routers: BTreeSet<&str> = have
        .routers
        .iter()
        .filter_map(|r| r.external_ids.get(ID_KEY).map(String::as_str))
        .chain(
            have.ha_groups
                .iter()
                .filter_map(|g| g.external_ids.get(ID_KEY).map(String::as_str)),
        )
        .filter(|id| !wanted_routers.contains(*id))
        .collect();
    for id in stale_routers {
        let ops = delete_router(id, have);
        if !ops.is_empty() {
            out.push(Batch {
                key: format!("opl-r-{id}"),
                ops,
            });
        }
    }
    // 바라지 않는 LB(라우터 묶음 뒤 — 붙인 약한 참조는 OVSDB가 비운다, 계획 67)
    for (key, ops) in lb::stale(want, have) {
        out.push(Batch { key, ops });
    }
    // 쓰이지 않는 목록 Port_Group(포트를 옮기는 네트워크 묶음 뒤, `ports`가 빈 때만 — §11.3)
    let wanted_groups: BTreeSet<&str> = want.port_groups.iter().map(|g| g.name.as_str()).collect();
    for g in have
        .port_groups
        .iter()
        .filter(|g| !wanted_groups.contains(g.name.as_str()))
    {
        if let Some(op) = acl::delete_port_group(g, &our_ports) {
            out.push(Batch {
                key: g.name.clone(),
                ops: vec![op],
            });
        }
    }
    let stale_dhcp: Vec<Value> = have
        .dhcp
        .iter()
        .filter(|a| {
            a.external_ids
                .get(ID_KEY)
                .is_none_or(|id| !wanted_networks.contains(id))
        })
        .map(|a| json!({"op": "delete", "table": "DHCP_Options", "where": by_uuid(&a.uuid)}))
        .collect();
    if !stale_dhcp.is_empty() {
        out.push(Batch {
            key: "DHCP_Options".into(),
            ops: stale_dhcp,
        });
    }
    out
}

/// 라우터 행 하나와 그 자식 행을 견주는 연산(한 트랜잭션, 계획 66 §9). 자식(포트·경로·NAT·HA chassis)은 우리 표시가 있는
/// 것만 본다(관리자가 우리 라우터에 넣은 행은 둔다). 경로·NAT·HA chassis는 내용이 같은 행을 그대로 두고 다른 것만 넣고
/// 뺀다(빠진 행은 참조를 잃고 OVSDB가 지운다). 다른 라우터에 있던 같은 이름의 포트(인터페이스를 옮김)는 이 묶음이 옛 라우터에서
/// 빼고 지운 뒤 새로 넣는다 — 옛 라우터의 묶음과 차례가 어떻든 uuid로만 지우므로 서로 막지 않는다.
fn plan_router(n: usize, r: &RouterRow, have: &NbActual) -> Vec<Value> {
    let mut ops = Vec::new();
    let mut same = have.routers.iter().filter(|a| a.name == r.name);
    let current = same.next();
    let our_ports: BTreeMap<&str, &ActualRouterPort> = have
        .router_ports
        .iter()
        .map(|p| (p.uuid.as_str(), p))
        .collect();
    for extra in same {
        ops.push(json!({"op": "delete", "table": "Logical_Router", "where": by_uuid(&extra.uuid)}));
        for u in extra
            .ports
            .iter()
            .filter(|u| our_ports.contains_key(u.as_str()))
        {
            ops.push(json!({"op": "delete", "table": "Logical_Router_Port", "where": by_uuid(u)}));
        }
    }

    // HA 그룹(뿌리, 이름으로 짝짓기)
    let mut groups = have.ha_groups.iter().filter(|g| {
        r.ha.as_ref().is_some_and(|h| h.name == g.name) || g.name == ha_group_name(r.router)
    });
    let group = groups.next();
    let mut stale_groups: Vec<&ActualHaGroup> = groups.collect();
    let group_ref = match (&r.ha, group) {
        (Some(want), Some(g)) => {
            let chassis: Vec<&ActualHaChassis> = have
                .ha_chassis
                .iter()
                .filter(|c| g.ha_chassis.contains(&c.uuid))
                .collect();
            let (mut kept, mut add) = (BTreeSet::new(), Vec::new());
            for (i, (name, priority)) in want.chassis.iter().enumerate() {
                let ids = ha_chassis_ids(want);
                match chassis
                    .iter()
                    .find(|c| &c.chassis_name == name && !kept.contains(c.uuid.as_str()))
                {
                    Some(c) => {
                        kept.insert(c.uuid.as_str());
                        if c.priority != i64::from(*priority) || c.external_ids != ids {
                            ops.push(json!({"op": "update", "table": "HA_Chassis", "where": by_uuid(&c.uuid),
                                "row": {"priority": priority, "external_ids": map_value(&ids)}}));
                        }
                    }
                    None => {
                        let name_u = format!("h{n}_{i}");
                        ops.push(json!({"op": "insert", "table": "HA_Chassis", "uuid-name": name_u,
                            "row": {"chassis_name": name, "priority": priority, "external_ids": map_value(&ids)}}));
                        add.push(named_uuid(&name_u));
                    }
                }
            }
            let remove: Vec<Value> = chassis
                .iter()
                .filter(|c| !kept.contains(c.uuid.as_str()))
                .map(|c| uuid_value(&c.uuid))
                .collect();
            if g.external_ids != want.external_ids {
                ops.push(
                    json!({"op": "update", "table": "HA_Chassis_Group", "where": by_uuid(&g.uuid),
                    "row": {"external_ids": map_value(&want.external_ids)}}),
                );
            }
            mutate_set(
                &mut ops,
                "HA_Chassis_Group",
                &g.uuid,
                "ha_chassis",
                remove,
                add,
            );
            Some(uuid_value(&g.uuid))
        }
        (Some(want), None) => {
            let mut members = Vec::new();
            for (i, (name, priority)) in want.chassis.iter().enumerate() {
                let name_u = format!("h{n}_{i}");
                ops.push(
                    json!({"op": "insert", "table": "HA_Chassis", "uuid-name": name_u,
                    "row": {"chassis_name": name, "priority": priority,
                            "external_ids": map_value(&ha_chassis_ids(want))}}),
                );
                members.push(named_uuid(&name_u));
            }
            let name_g = format!("g{n}");
            ops.push(
                json!({"op": "insert", "table": "HA_Chassis_Group", "uuid-name": name_g,
                "row": {"name": want.name, "ha_chassis": set_value(members),
                        "external_ids": map_value(&want.external_ids)}}),
            );
            Some(named_uuid(&name_g))
        }
        (None, g) => {
            stale_groups.extend(g);
            None
        }
    };

    // 라우터 포트(이름으로 짝짓기)
    let ports_by_name: BTreeMap<&str, &ActualRouterPort> = have
        .router_ports
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();
    let (mut kept, mut add) = (BTreeSet::new(), Vec::new());
    for (i, p) in r.ports.iter().enumerate() {
        let ha = if p.gateway { group_ref.clone() } else { None };
        let found = ports_by_name.get(p.name.as_str()).copied();
        match found.filter(|a| current.is_some_and(|c| c.ports.contains(&a.uuid))) {
            Some(a) => {
                kept.insert(a.uuid.as_str());
                let same = a.mac == p.mac
                    && a.networks == sorted(p.networks.clone())
                    && a.enabled == p.enabled
                    && same_ref(ha.as_ref(), a.ha_chassis_group.as_deref())
                    && a.options == p.options
                    && a.ipv6_ra_configs == p.ipv6_ra_configs
                    && a.external_ids == p.external_ids;
                if !same {
                    ops.push(json!({"op": "update", "table": "Logical_Router_Port", "where": by_uuid(&a.uuid),
                        "row": router_port_columns(p, ha)}));
                }
            }
            None => {
                if let Some(a) = found {
                    // 다른 라우터에서 옮겨 온다.
                    for old in have.routers.iter().filter(|o| o.ports.contains(&a.uuid)) {
                        ops.push(json!({"op": "mutate", "table": "Logical_Router", "where": by_uuid(&old.uuid),
                            "mutations": [["ports", "delete", set_value(vec![uuid_value(&a.uuid)])]]}));
                    }
                    ops.push(json!({"op": "delete", "table": "Logical_Router_Port", "where": by_uuid(&a.uuid)}));
                }
                let name_p = format!("lp{n}_{i}");
                ops.push(
                    json!({"op": "insert", "table": "Logical_Router_Port", "uuid-name": name_p,
                    "row": router_port_columns(p, ha)}),
                );
                add.push(named_uuid(&name_p));
            }
        }
    }
    let mut remove = Vec::new();
    for u in current.iter().flat_map(|c| &c.ports) {
        if our_ports.contains_key(u.as_str()) && !kept.contains(u.as_str()) {
            remove.push(uuid_value(u));
            // 참조를 잃은 포트는 커밋 때 사라지지만, 그 포트가 지울 HA 그룹을 가리키면 참조 검사가 먼저 막으므로 곧바로 지운다.
            ops.push(json!({"op": "delete", "table": "Logical_Router_Port", "where": by_uuid(u)}));
        }
    }
    for g in stale_groups {
        ops.push(json!({"op": "delete", "table": "HA_Chassis_Group", "where": by_uuid(&g.uuid)}));
    }

    // 정적 경로·NAT(내용으로 짝짓기)
    let routes: Vec<&ActualRoute> = have
        .routes
        .iter()
        .filter(|a| current.is_some_and(|c| c.static_routes.contains(&a.uuid)))
        .collect();
    let (mut kept_routes, mut add_routes) = (BTreeSet::new(), Vec::new());
    for (i, w) in r.routes.iter().enumerate() {
        match routes.iter().find(|a| {
            a.ip_prefix == w.ip_prefix
                && a.nexthop == w.nexthop
                && a.external_ids == w.external_ids
                && !kept_routes.contains(a.uuid.as_str())
        }) {
            Some(a) => {
                kept_routes.insert(a.uuid.as_str());
            }
            None => {
                let name_r = format!("r{n}_{i}");
                ops.push(json!({"op": "insert", "table": "Logical_Router_Static_Route", "uuid-name": name_r,
                    "row": {"ip_prefix": w.ip_prefix, "nexthop": w.nexthop,
                            "external_ids": map_value(&w.external_ids)}}));
                add_routes.push(named_uuid(&name_r));
            }
        }
    }
    let remove_routes: Vec<Value> = routes
        .iter()
        .filter(|a| !kept_routes.contains(a.uuid.as_str()))
        .map(|a| uuid_value(&a.uuid))
        .collect();
    let nats: Vec<&ActualNat> = have
        .nat
        .iter()
        .filter(|a| current.is_some_and(|c| c.nat.contains(&a.uuid)))
        .collect();
    let (mut kept_nat, mut add_nat) = (BTreeSet::new(), Vec::new());
    for (i, w) in r.nat.iter().enumerate() {
        match nats.iter().find(|a| {
            a.kind == w.kind
                && a.external_ip == w.external_ip
                && a.logical_ip == w.logical_ip
                && a.logical_port == w.logical_port
                && a.external_mac == w.external_mac
                && a.external_ids == w.external_ids
                && !kept_nat.contains(a.uuid.as_str())
        }) {
            Some(a) => {
                kept_nat.insert(a.uuid.as_str());
            }
            None => {
                let name_n = format!("n{n}_{i}");
                let opt = |v: &Option<String>| string_set(&v.iter().cloned().collect::<Vec<_>>());
                ops.push(json!({"op": "insert", "table": "NAT", "uuid-name": name_n,
                    "row": {"type": w.kind, "external_ip": w.external_ip, "logical_ip": w.logical_ip,
                            "logical_port": opt(&w.logical_port), "external_mac": opt(&w.external_mac),
                            "external_ids": map_value(&w.external_ids)}}));
                add_nat.push(named_uuid(&name_n));
            }
        }
    }
    let remove_nat: Vec<Value> = nats
        .iter()
        .filter(|a| !kept_nat.contains(a.uuid.as_str()))
        .map(|a| uuid_value(&a.uuid))
        .collect();

    match current {
        None => {
            let mut row = json!({"name": r.name, "options": map_value(&r.options),
                    "external_ids": map_value(&r.external_ids), "ports": set_value(add),
                    "static_routes": set_value(add_routes), "nat": set_value(add_nat)});
            // 계획 67: 비면 쓰지 않는다(계획 66 라우터의 연산 그대로).
            let lbs = lb::attach_insert(&r.load_balancers, have);
            if !lbs.is_empty() {
                row["load_balancer"] = set_value(lbs);
            }
            ops.push(
                json!({"op": "insert", "table": "Logical_Router", "uuid-name": format!("lr{n}"),
                "row": row}),
            );
        }
        Some(c) => {
            if c.options != r.options || c.external_ids != r.external_ids {
                ops.push(json!({"op": "update", "table": "Logical_Router", "where": by_uuid(&c.uuid),
                    "row": {"options": map_value(&r.options), "external_ids": map_value(&r.external_ids)}}));
            }
            mutate_set(&mut ops, "Logical_Router", &c.uuid, "ports", remove, add);
            mutate_set(
                &mut ops,
                "Logical_Router",
                &c.uuid,
                "static_routes",
                remove_routes,
                add_routes,
            );
            mutate_set(
                &mut ops,
                "Logical_Router",
                &c.uuid,
                "nat",
                remove_nat,
                add_nat,
            );
            let (lb_add, lb_remove) =
                lb::attach_changes(&r.load_balancers, &c.load_balancers, have);
            mutate_set(
                &mut ops,
                "Logical_Router",
                &c.uuid,
                "load_balancer",
                lb_remove,
                lb_add,
            );
        }
    }
    ops
}

/// 그룹의 HA chassis 표시(그룹과 같은 주인 표시)
fn ha_chassis_ids(group: &HaGroupRow) -> BTreeMap<String, String> {
    group
        .external_ids
        .iter()
        .filter(|(k, _)| k.as_str() == CLUSTER_KEY)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// 집합 칸 `column`에서 `remove`를 빼고 `add`를 넣는 연산(빈 것은 쓰지 않는다)
fn mutate_set(
    ops: &mut Vec<Value>,
    table: &str,
    uuid: &str,
    column: &str,
    remove: Vec<Value>,
    add: Vec<Value>,
) {
    if !remove.is_empty() {
        ops.push(
            json!({"op": "mutate", "table": table, "where": by_uuid(uuid),
            "mutations": [[column, "delete", set_value(remove)]]}),
        );
    }
    if !add.is_empty() {
        ops.push(
            json!({"op": "mutate", "table": table, "where": by_uuid(uuid),
            "mutations": [[column, "insert", set_value(add)]]}),
        );
    }
}

/// 라우터 포트 행의 칸(넣기·바꾸기). `ha`는 HA 그룹의 참조다.
fn router_port_columns(p: &RouterPortRow, ha: Option<Value>) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("name".into(), json!(p.name));
    row.insert("mac".into(), json!(p.mac));
    row.insert("networks".into(), string_set(&p.networks));
    row.insert("enabled".into(), opt_bool(p.enabled));
    row.insert(
        "ha_chassis_group".into(),
        set_value(ha.into_iter().collect()),
    );
    row.insert("options".into(), map_value(&p.options));
    row.insert("ipv6_ra_configs".into(), map_value(&p.ipv6_ra_configs));
    row.insert("external_ids".into(), map_value(&p.external_ids));
    row
}

/// 바라지 않는 라우터 `id`(라우터 행이나 HA 그룹의 `orbivisor:id`)의 행을 지우는 연산: 라우터, 그 우리 포트(HA 그룹을
/// 가리키는 DGP가 그룹 지우기를 막지 않게 곧바로), HA 그룹
fn delete_router(id: &str, have: &NbActual) -> Vec<Value> {
    let mut ops = Vec::new();
    let ours: BTreeSet<&str> = have.router_ports.iter().map(|p| p.uuid.as_str()).collect();
    for lr in have
        .routers
        .iter()
        .filter(|r| r.external_ids.get(ID_KEY).map(String::as_str) == Some(id))
    {
        ops.push(json!({"op": "delete", "table": "Logical_Router", "where": by_uuid(&lr.uuid)}));
        for u in lr.ports.iter().filter(|u| ours.contains(u.as_str())) {
            ops.push(json!({"op": "delete", "table": "Logical_Router_Port", "where": by_uuid(u)}));
        }
    }
    for g in have
        .ha_groups
        .iter()
        .filter(|g| g.external_ids.get(ID_KEY).map(String::as_str) == Some(id))
    {
        ops.push(json!({"op": "delete", "table": "HA_Chassis_Group", "where": by_uuid(&g.uuid)}));
    }
    ops
}

/// 트랜잭션 앞의 펜싱 연산: `NB_Global.external_ids`가 읽은 값 그대로인지 `wait`하고, 토큰 `token`을 쓰고 `nb_cfg`를
/// 1 올린다. `NB_Global`이 없으면(northd 전) 없음을 `wait`하고 넣는다. 돌려주는 맵은 쓴 뒤의 `external_ids`다.
pub fn fence_ops(
    global: Option<&Global>,
    tag: &str,
    token: &str,
) -> (Vec<Value>, BTreeMap<String, String>) {
    fence(global, tag, token, true)
}

/// [`fence_ops`]와 같되 `bump`가 거짓이면 `nb_cfg`를 올리지 않는다(한 차례의 마지막 트랜잭션만 올린다 — 리뷰 2 m-2:
/// 올릴 때마다 모든 ovn-controller가 SB `Chassis_Private.nb_cfg`를 쓴다). 펜싱 연산의 수는 같다.
pub fn fence(
    global: Option<&Global>,
    tag: &str,
    token: &str,
    bump: bool,
) -> (Vec<Value>, BTreeMap<String, String>) {
    let mut ids = global.map(|g| g.external_ids.clone()).unwrap_or_default();
    ids.insert(fence_key(tag), token.into());
    let step = i64::from(bump);
    let ops = match global {
        Some(g) => vec![
            json!({"op": "wait", "table": "NB_Global", "where": by_uuid(&g.uuid), "columns": ["external_ids"],
                   "until": "==", "rows": [{"external_ids": map_value(&g.external_ids)}], "timeout": 0}),
            json!({"op": "update", "table": "NB_Global", "where": by_uuid(&g.uuid),
                   "row": {"external_ids": map_value(&ids)}}),
            json!({"op": "mutate", "table": "NB_Global", "where": by_uuid(&g.uuid),
                   "mutations": [["nb_cfg", "+=", step]]}),
        ],
        None => vec![
            json!({"op": "wait", "table": "NB_Global", "where": [], "columns": ["external_ids"],
                   "until": "==", "rows": [], "timeout": 0}),
            json!({"op": "insert", "table": "NB_Global",
                   "row": {"external_ids": map_value(&ids), "nb_cfg": step}}),
        ],
    };
    (ops, ids)
}

/// NIC 하나의 칸(네트워크, MAC, 주소, 보안 그룹, QoS — 계획 67)
type NicInputs = (
    String,
    Option<String>,
    Vec<std::net::IpAddr>,
    Vec<String>,
    Option<opl_types::qos::NicQos>,
);

/// 논리 포트([`desired`])에 쓰이는 VM의 칸: 템플릿인지, NIC(네트워크·MAC·주소·보안 그룹), 전원·배정 노드, 실행 단계·노드·
/// 마이그레이션 대상. 이것이 그대로인 VM 쓰기(복제 기록·통계 같은 상태 쓰기)로는 OvnSync가 NB를 다시 읽지 않는다(리뷰 2
/// m-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmInputs {
    template: bool,
    nics: Vec<NicInputs>,
    power: PowerState,
    node: Option<Uuid>,
    phase: VmPhase,
    at: Option<Uuid>,
    migration: Option<Uuid>,
}

impl VmInputs {
    pub fn of(spec: &opl_types::vm::VmSpec, status: &opl_types::vm::VmStatus) -> Self {
        Self {
            template: spec.template,
            nics: spec
                .nics
                .iter()
                .map(|n| {
                    (
                        n.network.clone(),
                        n.mac.map(|m| m.to_string()),
                        n.ips.clone(),
                        n.security_groups.clone(),
                        n.qos,
                    )
                })
                .collect(),
            power: spec.power,
            node: spec.node,
            phase: status.phase,
            at: status.node,
            migration: status.migration.as_ref().map(|m| m.node),
        }
    }
}

/// 라우터 행([`desired_routing`])에 쓰이는 노드의 칸(계획 66 §12): 게이트웨이 우선순위, 유지보수, `ready`인 physnet. 이것이
/// 그대로인 노드 쓰기(5초 검사 루프의 상태 쓰기)로는 OvnSync가 NB를 다시 읽지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInputs {
    gateway: Option<u16>,
    maintenance: bool,
    physnets: Vec<String>,
}

impl NodeInputs {
    pub fn of(spec: &opl_types::node::NodeSpec, status: &opl_types::node::NodeStatus) -> Self {
        Self {
            gateway: spec.sdn.as_ref().and_then(|s| s.gateway),
            maintenance: spec.maintenance,
            physnets: status
                .sdn
                .iter()
                .flat_map(|s| &s.physnets)
                .filter(|p| p.ready)
                .map(|p| p.name.clone())
                .collect(),
        }
    }
}
