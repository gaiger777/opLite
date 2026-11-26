//! 물리 ↔ 가상 네트워크 관계(계획 75 설계 §3.2): 노드마다 포트 → 본딩 → VLAN → 브리지 → 네트워크 → VM NIC 사슬,
//! 포트·본딩 장애의 영향, 네트워크마다 노드별 레일 상태. `GET /topology/physical`과 링크 경보(`NodeLinkDown`·
//! `BondDegraded`)가 같은 계산을 쓴다. 입력은 노드 상태(`interfaces`·`bridges`·`underlay`·`sdn`)와 네트워크·VM 스펙뿐이다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::capacity::placed_on;
use crate::network::{Network, NetworkSpec, is_managed_ifname};
use crate::node::{
    BondPort, BondState, InterfaceKind, LldpNeighbor, Node, NodeInterface, NodeStatus,
    OPERATOR_DASHBOARD, PortState,
};
use crate::sdn::{ChassisState, PhysnetState};
use crate::vm::Vm;

/// 영향 하나에 담는 VM id 수의 상한(수는 `vms`에 다 센다)
pub const MAX_IMPACT_VM_IDS: usize = 50;

/// 사슬을 따라 내려가는 깊이 상한(브리지 → VLAN → 본딩 → 포트는 3, 잘못 적힌 고리를 끊는다)
const MAX_DEPTH: usize = 8;

/// `GET /topology/physical`의 응답
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PhysicalTopology {
    /// 노드마다 하나(이름 순, 위트니스 포함)
    pub nodes: Vec<NodePhysical>,
    /// 네트워크마다 레일 하나(이름 순)
    pub networks: Vec<NetRail>,
}

/// 노드 하나의 물리 네트워크
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodePhysical {
    pub id: Uuid,
    /// 노드 이름
    pub node: String,
    pub online: bool,
    /// 옛 노드(기능 `operator-dashboard`를 알리지 않음): `mtu`·`parent`·`lldp`가 없고 VLAN 아래 장치는 이름 규칙
    /// (`eno1.100`)으로 추정했다. 화면은 "옛 판 — 일부 정보 없음"을 보인다.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub legacy: bool,
    /// 포트(노드가 알린 인터페이스 순서, veth 뺌)
    pub ports: Vec<Port>,
    /// Linux 브리지(이름 순, opld가 만든 장치 뺌)
    pub bridges: Vec<Bridge>,
    /// VXLAN·Geneve 패킷이 오가는 주소와 그 주소를 가진 인터페이스
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub underlay: Option<UnderlayLink>,
    /// SDN physnet 연결(`NodeStatus.sdn.physnets`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub physnets: Vec<PhysnetState>,
    /// 지금 장애가 난 포트·본딩마다 영향(포트 순서). 오프라인 노드는 마지막으로 본 상태의 값이다(`online`이 거짓).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub impact: Vec<Impact>,
    /// 이 노드에서 도는 VM이 NIC를 붙였는데 이 노드에 그 네트워크를 쓸 수단(브리지·physnet·underlay)이 없는 것(이름 순,
    /// 계획 75 리뷰 1). 그 NIC는 끊겨 있다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub unreachable: Vec<Unreachable>,
}

/// 이 노드에 없는 네트워크에 붙은 VM NIC
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Unreachable {
    /// 네트워크 id
    pub network: Uuid,
    /// 이 노드에서 그 네트워크에 NIC가 있는 VM 수
    pub vms: u32,
    /// 그 VM의 id([`MAX_IMPACT_VM_IDS`]개까지, 이름 순)
    pub vm_ids: Vec<Uuid>,
}

/// 포트(인터페이스) 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Port {
    pub name: String,
    pub kind: PortKind,
    /// operstate(`up`, `down`, `unknown`, `lowerlayerdown` …)
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub carrier: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub speed_mbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub mtu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub mac: Option<String>,
    /// 이 포트를 묶은 본딩·브리지
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub master: Option<String>,
    /// VLAN의 아래 장치(노드가 알리지 않았으면 이름 규칙으로 추정)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vlan_id: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub lldp: Option<LldpNeighbor>,
    /// 본딩이면 모드·상태·구성 포트
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bond: Option<PortBond>,
    pub role: PortRole,
    /// 이 포트(또는 그 위)가 가진 주소의 쓰임: 클러스터 링크(`cluster_addresses`)·관리(노드 API 주소)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub uses: Vec<PortUse>,
    /// 이 포트가 가진 전역 주소(CIDR 없이). 주소를 잃었거나 사라진 기억 포트는 마지막으로 가졌던 주소다(계획 75 리뷰 2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub addresses: Vec<String>,
    /// 이 포트에서 아래로 본 상태: 본딩은 본딩 상태, VLAN은 아래 장치, 브리지는 업링크들(관리자가 내렸으면 down).
    /// 주소를 잃은 기억 포트·사라진 기억 포트(`state: missing`)는 down이다(계획 75 리뷰 2).
    pub health: LinkHealth,
}

/// 주소를 가진 포트의 쓰임
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PortUse {
    /// 노드의 클러스터 링크 주소(`NodeStatus.cluster_addresses`)
    Cluster,
    /// 노드의 API 주소(`NodeSpec.address`)
    Management,
}

impl PortUse {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cluster => "cluster link",
            Self::Management => "management",
        }
    }
}

/// 포트 종류(`dummy` 등 그 밖은 `other`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    Ethernet,
    Bond,
    Vlan,
    Bridge,
    /// 그 밖, 그리고 이 빌드가 모르는 값(뒤 버전이 더한 종류 — 노드 상태의 기억 포트를 읽을 수 있게)
    #[serde(other)]
    Other,
}

/// 본딩 포트의 본딩 정보
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PortBond {
    pub mode: String,
    pub state: BondState,
    pub members: Vec<BondPort>,
}

/// 포트가 쓰이는 곳
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PortRole {
    /// 브리지 아래(업링크·그 VLAN 아래 장치·본딩 구성 포트)나 본딩과 그 구성 포트
    Uplink,
    /// VXLAN·Geneve 주소를 가진 인터페이스와 그 아래
    Underlay,
    /// 전역 주소(IPv4·IPv6, 링크 로컬 빼고)를 가진 물리 포트(이더넷·본딩·VLAN·그 밖)와 그 아래 — 스토리지·복제·관리·
    /// 클러스터 링크망(계획 75 리뷰 1). 쓰임은 [`Port::uses`].
    Service,
    /// 어디에도 묶이지 않고 주소도 없는 이더넷
    Unused,
    /// 그 밖(업링크가 아닌 브리지, 어디에도 묶이지 않은 가상 장치 등), 그리고 이 빌드가 모르는 값(뒤 버전이 더한 역할)
    #[serde(other)]
    Other,
}

/// 링크 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum LinkHealth {
    Up,
    /// 이중화를 잃음(본딩 저하, 업링크 일부만 살아 있음)
    Degraded,
    Down,
}

/// 장애의 무게
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FaultSeverity {
    /// 위의 브리지·네트워크가 끊김
    Down,
    /// 위는 살았지만 이중화를 잃음(본딩의 다른 포트로 흐름)
    Degraded,
}

impl FaultSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Degraded => "degraded",
        }
    }
}

/// 노드의 Linux 브리지 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Bridge {
    pub name: String,
    /// `master`가 이 브리지인 인터페이스(tap 빼고)
    pub uplinks: Vec<String>,
    pub vlan_filtering: bool,
    /// 이 노드에서 이 브리지에 붙는 네트워크 id(브리지·VLAN·공급자 네트워크, 이름 순)
    pub networks: Vec<Uuid>,
    /// 이 노드에서 도는 VM의 NIC 가운데 그 네트워크들에 붙은 수
    pub vm_nics: u32,
    pub health: LinkHealth,
}

/// VXLAN·Geneve underlay
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnderlayLink {
    /// `NodeStatus.underlay`, 없으면 SDN `encap_ip`
    pub address: String,
    /// 그 주소를 가진 인터페이스(못 찾으면 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub port: Option<String>,
}

/// 장애 하나와 그 영향
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Impact {
    /// 장애가 난 포트(이더넷·VLAN의 링크 끊김) 또는 본딩(구성 포트에 링크 끊김이 없는 저하·끊김)
    pub port: String,
    pub severity: FaultSeverity,
    /// 끊긴 포트가 든 본딩
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bond: Option<String>,
    /// 포트가 노드에서 사라짐(노드가 기억한 포트, 계획 75 리뷰 2)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub missing: bool,
    /// 위에 있는 브리지(이름 순)
    pub bridges: Vec<String>,
    /// underlay 주소를 가진 인터페이스가 위에 있다(VXLAN·SDN 오버레이가 영향받음).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub underlay: bool,
    /// 끊긴 포트가 실어 나르던 노드 주소의 쓰임(클러스터 링크·관리)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub uses: Vec<PortUse>,
    /// 영향받는 네트워크 id(이름 순)
    pub networks: Vec<Uuid>,
    /// 이 노드에서 그 네트워크에 NIC가 있는 VM 수
    pub vms: u32,
    /// 그 VM의 id([`MAX_IMPACT_VM_IDS`]개까지, 이름 순)
    pub vm_ids: Vec<Uuid>,
}

/// 네트워크 하나의 레일(지도 바닥 판의 가로 띠)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NetRail {
    /// 네트워크 id
    pub id: Uuid,
    pub name: String,
    pub kind: RailKind,
    /// 실어 나르는 것: 브리지 이름, physnet 이름, VXLAN·오버레이는 `underlay`
    pub carrier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vlan: Option<u16>,
    /// 위트니스가 아닌 노드마다(이름 순)
    pub nodes: Vec<RailNode>,
}

/// 레일 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RailKind {
    /// 태그 없는 브리지 네트워크
    Bridge,
    /// VLAN 태그를 단 브리지 네트워크
    Vlan,
    Vxlan,
    SdnOverlay,
    SdnProvider,
}

/// 레일 위 노드 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RailNode {
    /// 노드 이름
    pub node: String,
    pub state: RailState,
}

/// 한 노드에서 본 레일 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RailState {
    /// 사슬이 끝까지 올라와 있음
    Up,
    /// 본딩 저하 등으로 이중화를 잃음
    Degraded,
    /// 사슬이 끊김
    Down,
    /// 이 노드에 이 네트워크를 쓸 수단이 없음(브리지·physnet·underlay 없음)
    Absent,
    /// 모름: 노드가 오프라인이거나, underlay 주소를 가진 인터페이스를 찾지 못했고 전에 본 것도 없음(계획 75 리뷰 1)
    Unknown,
}

impl From<LinkHealth> for RailState {
    fn from(h: LinkHealth) -> Self {
        match h {
            LinkHealth::Up => Self::Up,
            LinkHealth::Degraded => Self::Degraded,
            LinkHealth::Down => Self::Down,
        }
    }
}

/// 링크 경보(`NodeLinkDown`)가 보는 포트
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum LinkRole {
    /// 업링크·underlay·주소를 가진 포트([`PortRole::Uplink`]·[`PortRole::Underlay`]·[`PortRole::Service`])와 관리자가
    /// 내린 브리지
    #[default]
    Uplink,
    /// 그에 더해 이더넷·VLAN 포트 모두(쓰지 않는 포트 포함). 어디에도 묶이지 않은 가상 장치(`ovs-system`·`br-int` 등)는
    /// 보지 않는다.
    Any,
}

/// 링크 경보가 보는 장애 하나(이 노드 상태에서 지금 firing인 것)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LinkFault {
    /// 끊긴 포트(`NodeLinkDown`) 또는 본딩(`BondDegraded`)
    pub port: String,
    pub severity: FaultSeverity,
    /// 끊긴 포트가 든 본딩
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bond: Option<String>,
    pub role: PortRole,
}

/// 노드가 기억하는 포트 하나(`NodeStatus.known_ports`, 계획 75 리뷰 2). 역할이 업링크·underlay·service였던 이더넷·그 밖·
/// VLAN·본딩을 이름으로 기억한다. 장애 순간 주소가 떨어지거나(NetworkManager·networkd가 carrier를 잃은 장치의 주소를 뗌)
/// netdev가 사라져도(PCIe 장애·unbind·핫플러그 제거) 역할이 바뀌어 감시 밖으로 빠지지 않게 한다. 사라짐(`missing`)은
/// 이더넷·그 밖만 남긴다 — VLAN·본딩은 지우지 않는 한 사라지지 않으므로 사라지면 잊는다. 잊는 길은 그 포트가 다시 나타나
/// 감시 밖 역할이 되거나(관리자가 다시 구성), 본딩에 새 구성 포트가 들어오거나(그 본딩의 사라진 구성 포트), 운영자가
/// `forget-ports`로 지우는 것이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct KnownPort {
    pub name: String,
    pub kind: PortKind,
    /// 마지막으로 본 역할(업링크·underlay·service)
    pub role: PortRole,
    /// 이 포트가 스스로 가졌던 전역 주소(CIDR 없이, 마지막으로 본 것). 지금 이 포트도 다른 인터페이스도 이 주소를 갖지
    /// 않으면 주소를 잃은 것이다. 쓰임(클러스터 링크·관리)은 이 주소로 다시 셈한다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub addresses: Vec<String>,
    /// 속했던 본딩
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bond: Option<String>,
    /// 위에 있던 브리지(이름 순) — 본딩 밖 업링크가 사라지면 그 브리지는 그 업링크를 잃은 것이다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub bridges: Vec<String>,
    /// 노드의 인터페이스에서 사라짐
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub missing: bool,
    /// 처음 기억했거나 다시 나타난 때, 사라졌으면 사라진 것을 안 때(유닉스 ms). 매 회차 바꾸지 않는다(노드 상태를
    /// 쓸데없이 다시 쓰지 않게).
    pub last_seen_ms: u64,
}

/// 기억하는 포트의 상한(노드 인터페이스 상한과 같음)
pub const MAX_KNOWN_PORTS: usize = 64;

// ---------------------------------------------------------------------------------------------------------------
// 노드 하나의 그래프
// ---------------------------------------------------------------------------------------------------------------

/// 노드 상태에서 읽은 인터페이스 그래프(VLAN 아래 장치는 없으면 이름으로 추정)와 노드가 기억하는 포트
struct Host<'a> {
    status: &'a NodeStatus,
    /// veth를 뺀 인터페이스
    ifs: Vec<&'a NodeInterface>,
    by_name: BTreeMap<&'a str, usize>,
    /// `ifs`마다 아래 장치·VLAN id(추정 포함)
    parents: Vec<Option<String>>,
    vlan_ids: Vec<Option<u16>>,
    /// 브리지 이름 → `vlan_filtering`(opld가 만든 장치 뺌)
    bridges: BTreeMap<String, bool>,
    /// 노드 주소의 쓰임(클러스터 링크·관리)
    uses: Vec<(IpAddr, PortUse)>,
    /// 사라진 기억 포트(이더넷·그 밖, 목록에 없음, 계획 75 리뷰 2). 포트 목록에서 `ifs` 뒤에 이 순서로 온다.
    missing: Vec<&'a KnownPort>,
    /// 주소를 잃은 기억 포트(목록에 있음): 지금 전역 주소가 없고, 기억한 주소를 어느 인터페이스도 갖지 않는다.
    lost: BTreeSet<&'a str>,
    /// 기억한 역할을 쓰는 포트: 주소를 잃은 포트와 그 아래
    recalled: BTreeSet<&'a str>,
}

/// `host:port`·`[v6]:port`·주소만 가운데 IP 주소(이름이면 없음)
fn ip_of(addr: &str) -> Option<IpAddr> {
    addr.parse::<SocketAddr>()
        .map(|a| a.ip())
        .or_else(|_| addr.parse::<IpAddr>())
        .ok()
}

/// 전역 주소인지(IPv4 링크 로컬 169.254/16·IPv6 fe80::/10·루프백 빼고)
fn is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_link_local() && !v4.is_loopback() && !v4.is_unspecified(),
        IpAddr::V6(v6) => {
            (v6.segments()[0] & 0xffc0) != 0xfe80 && !v6.is_loopback() && !v6.is_unspecified()
        }
    }
}

/// 인터페이스의 주소(CIDR에서 앞)
fn addrs(i: &NodeInterface) -> impl Iterator<Item = IpAddr> + '_ {
    i.addresses
        .iter()
        .filter_map(|a| a.split('/').next().and_then(|a| a.parse::<IpAddr>().ok()))
}

/// 기억한 주소
fn known_ips(k: &KnownPort) -> impl Iterator<Item = IpAddr> + '_ {
    k.addresses.iter().filter_map(|a| a.parse::<IpAddr>().ok())
}

/// 기본 링크 경보가 보는 역할
fn watched_role(role: PortRole) -> bool {
    matches!(
        role,
        PortRole::Uplink | PortRole::Underlay | PortRole::Service
    )
}

impl<'a> Host<'a> {
    fn of(node: &'a Node) -> Host<'a> {
        Host::new(&node.status, &node.spec.address)
    }

    /// `address`는 노드의 API 주소(`NodeSpec.address` — 쓰임 `management`)
    fn new(status: &'a NodeStatus, address: &str) -> Host<'a> {
        let ifs: Vec<&NodeInterface> = status
            .interfaces
            .iter()
            .filter(|i| i.kind != InterfaceKind::Veth)
            .collect();
        let by_name: BTreeMap<&str, usize> = ifs
            .iter()
            .enumerate()
            .map(|(n, i)| (i.name.as_str(), n))
            .collect();
        let mut parents = Vec::with_capacity(ifs.len());
        let mut vlan_ids = Vec::with_capacity(ifs.len());
        for i in &ifs {
            let guess = (i.kind == InterfaceKind::Vlan)
                .then(|| i.name.rsplit_once('.'))
                .flatten()
                .filter(|(lower, _)| by_name.contains_key(lower));
            parents.push(
                i.parent
                    .clone()
                    .or_else(|| guess.map(|(lower, _)| lower.to_string())),
            );
            vlan_ids.push(
                i.vlan_id
                    .or_else(|| guess.and_then(|(_, id)| id.parse().ok())),
            );
        }
        let mut bridges: BTreeMap<String, bool> = status
            .bridges
            .iter()
            .map(|b| (b.name.clone(), b.vlan_filtering))
            .collect();
        for i in &ifs {
            if i.kind == InterfaceKind::Bridge {
                bridges.entry(i.name.clone()).or_insert(false);
            }
        }
        bridges.retain(|name, _| !is_managed_ifname(name));
        let uses = status
            .cluster_addresses
            .iter()
            .filter_map(|a| ip_of(a))
            .map(|ip| (ip, PortUse::Cluster))
            .chain(ip_of(address).map(|ip| (ip, PortUse::Management)))
            .collect();
        let missing = status
            .known_ports
            .iter()
            .filter(|k| {
                k.missing
                    && matches!(k.kind, PortKind::Ethernet | PortKind::Other)
                    && !by_name.contains_key(k.name.as_str())
            })
            .collect();
        let mut host = Host {
            status,
            ifs,
            by_name,
            parents,
            vlan_ids,
            bridges,
            uses,
            missing,
            lost: BTreeSet::new(),
            recalled: BTreeSet::new(),
        };
        let lost: BTreeSet<&str> = status
            .known_ports
            .iter()
            .filter(|k| {
                !k.missing
                    && k.addresses.iter().any(|a| a.parse::<IpAddr>().is_ok())
                    && host.get(&k.name).is_some_and(|i| !addrs(i).any(is_global))
                    && known_ips(k).all(|ip| host.holder(ip).is_none())
            })
            .map(|k| k.name.as_str())
            .collect();
        host.recalled = lost.iter().flat_map(|l| host.with_lower(l)).collect();
        host.lost = lost;
        host
    }

    fn get(&self, name: &str) -> Option<&'a NodeInterface> {
        self.by_name.get(name).map(|n| self.ifs[*n])
    }

    fn parent(&self, name: &str) -> Option<&str> {
        self.by_name
            .get(name)
            .and_then(|n| self.parents[*n].as_deref())
    }

    /// 노드가 기억하는 포트
    fn known(&self, name: &str) -> Option<&'a KnownPort> {
        self.status.known_ports.iter().find(|k| k.name == name)
    }

    /// 사라진 기억 포트
    fn missing_named(&self, name: &str) -> Option<&'a KnownPort> {
        self.missing.iter().find(|k| k.name == name).copied()
    }

    /// 링크가 끊겼거나 주소를 잃었는지
    fn is_down(&self, i: &NodeInterface) -> bool {
        link_down(i) || self.lost.contains(i.name.as_str())
    }

    /// 바로 아래: `master`가 이것인 인터페이스와 아래 장치
    fn children(&self, name: &str) -> Vec<&'a str> {
        let mut out: Vec<&str> = self
            .ifs
            .iter()
            .filter(|i| i.master.as_deref() == Some(name))
            .map(|i| i.name.as_str())
            .collect();
        if let Some(p) = self.parent(name)
            && let Some(i) = self.get(p)
        {
            out.push(i.name.as_str());
        }
        out
    }

    /// 아래 사슬 전체(자기 빼고)
    fn lower(&self, name: &str) -> BTreeSet<&'a str> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![(name.to_string(), 0)];
        while let Some((n, depth)) = todo.pop() {
            if depth >= MAX_DEPTH {
                continue;
            }
            for c in self.children(&n) {
                if c != name && seen.insert(c) {
                    todo.push((c.to_string(), depth + 1));
                }
            }
        }
        seen
    }

    /// 사라진 기억 포트 가운데 이 본딩의 구성 포트였던 것
    fn missing_members<'b>(&'b self, bond: &'b str) -> impl Iterator<Item = &'a KnownPort> + 'b {
        self.missing
            .iter()
            .copied()
            .filter(move |k| k.bond.as_deref() == Some(bond))
    }

    /// 이 인터페이스에서 아래로 본 상태. 사라진 기억 포트는 끊김, 그 밖에 없는 인터페이스는 모르므로 `up`으로 본다.
    fn health(&self, name: &str) -> LinkHealth {
        self.health_at(name, 0)
    }

    fn health_at(&self, name: &str, depth: usize) -> LinkHealth {
        let Some(i) = self.get(name) else {
            return if self.missing_named(name).is_some() {
                LinkHealth::Down
            } else {
                LinkHealth::Up
            };
        };
        if depth >= MAX_DEPTH {
            return LinkHealth::Up;
        }
        // 아래 장치와 사라진 아래 포트(본딩은 그 구성 포트, 브리지는 본딩 밖 업링크)
        let below = || -> Vec<LinkHealth> {
            let gone = self
                .missing
                .iter()
                .filter(|k| match i.kind {
                    InterfaceKind::Bond => k.bond.as_deref() == Some(name),
                    InterfaceKind::Bridge => {
                        k.bond.is_none() && k.bridges.iter().any(|b| b == name)
                    }
                    _ => false,
                })
                .map(|_| LinkHealth::Down);
            self.children(name)
                .into_iter()
                .map(|c| self.health_at(c, depth + 1))
                .chain(gone)
                .collect()
        };
        match i.kind {
            InterfaceKind::Bond => {
                if self.is_down(i) {
                    return LinkHealth::Down;
                }
                match i.bond.as_ref().map(|b| b.state) {
                    // 커널은 사라진 구성 포트를 `slaves`에서도 빼므로 "모두 up"으로 보인다.
                    Some(BondState::Up) if self.missing_members(name).next().is_some() => {
                        LinkHealth::Degraded
                    }
                    Some(BondState::Up) => LinkHealth::Up,
                    Some(BondState::Degraded) => LinkHealth::Degraded,
                    Some(BondState::Down) => LinkHealth::Down,
                    // 모르는 상태·본딩 정보 없음: 구성 포트로 본다.
                    _ => redundant(&below()),
                }
            }
            // 관리자가 내린 브리지는 끊김이다. 올라와 있으면 업링크가 없을 때(내부 브리지) carrier가 없어도 끊김이 아니다.
            // 업링크가 사라졌으면(기억 포트) 그 업링크는 끊김이다.
            InterfaceKind::Bridge if i.admin_up == Some(false) => LinkHealth::Down,
            InterfaceKind::Bridge => redundant(&below()),
            _ if self.is_down(i) => LinkHealth::Down,
            _ => below().into_iter().max().unwrap_or(LinkHealth::Up),
        }
    }

    /// 주소 `ip`를 지금 가진 인터페이스
    fn holder(&self, ip: IpAddr) -> Option<&'a str> {
        self.ifs
            .iter()
            .find(|i| addrs(i).any(|a| a == ip))
            .map(|i| i.name.as_str())
    }

    /// 주소 `ip`를 마지막으로 가졌던 기억 포트
    fn remembered_holder(&self, ip: IpAddr) -> Option<&'a str> {
        self.status
            .known_ports
            .iter()
            .find(|k| known_ips(k).any(|a| a == ip))
            .map(|k| k.name.as_str())
    }

    /// underlay 주소 `ip`의 인터페이스: 지금 가진 것, 없으면 마지막으로 가졌던 기억 포트(링크가 끊겨 주소가 떨어졌거나
    /// 인터페이스가 사라졌음). 둘 다 없으면 모른다.
    fn underlay_owner(&self, ip: IpAddr) -> Option<&'a str> {
        self.holder(ip).or_else(|| self.remembered_holder(ip))
    }

    /// underlay 주소의 상태: 가진 인터페이스의 상태, 아무도 갖지 않고 기억한 주인만 있으면 링크가 살아 있어도 끊김
    /// (VTEP이 보낼 원본 주소가 없다 — 계획 75 리뷰 2), 그것도 없으면 모름(None)
    fn underlay_health(&self, ip: IpAddr) -> Option<LinkHealth> {
        match self.holder(ip) {
            Some(h) => Some(self.health(h)),
            None => self.remembered_holder(ip).map(|_| LinkHealth::Down),
        }
    }

    /// underlay 주소들(VXLAN `underlay`, SDN `encap_ip`)의 인터페이스
    fn underlay_holders(&self) -> BTreeSet<&'a str> {
        [self.status.underlay, encap_ip(self.status)]
            .into_iter()
            .flatten()
            .filter_map(|ip| self.underlay_owner(ip))
            .collect()
    }

    /// 이름과 그 아래 사슬
    fn with_lower(&self, name: &'a str) -> BTreeSet<&'a str> {
        let mut s = self.lower(name);
        s.insert(name);
        s
    }

    /// 전역 주소를 가진 브리지 아닌 인터페이스와 그 아래(역할 `service`의 후보)
    fn addressed(&self) -> BTreeSet<&'a str> {
        self.ifs
            .iter()
            .filter(|i| i.kind != InterfaceKind::Bridge && addrs(i).any(is_global))
            .flat_map(|i| self.with_lower(&i.name))
            .collect()
    }

    /// 포트의 전역 주소: 지금 가진 것, 주소를 잃었거나 사라진 기억 포트는 마지막으로 가졌던 것
    fn addresses_of(&self, name: &str) -> Vec<String> {
        let now: Vec<String> = self
            .get(name)
            .map(|i| {
                addrs(i)
                    .filter(|ip| is_global(*ip))
                    .map(|ip| ip.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !now.is_empty() || !(self.lost.contains(name) || self.missing_named(name).is_some()) {
            return now;
        }
        self.known(name)
            .map(|k| k.addresses.clone())
            .unwrap_or_default()
    }

    /// 인터페이스 `name`(또는 그 위)이 가진 노드 주소의 쓰임. 아무도 갖지 않은 주소는 마지막으로 가졌던 기억 포트로 본다.
    fn uses_of(&self, name: &str) -> Vec<PortUse> {
        let mut out: Vec<PortUse> = self
            .uses
            .iter()
            .filter(|(ip, _)| {
                self.holder(*ip)
                    .or_else(|| self.remembered_holder(*ip))
                    .is_some_and(|h| h == name || self.lower(h).contains(name))
            })
            .map(|(_, u)| *u)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn role(
        &self,
        i: &NodeInterface,
        uplinks: &BTreeSet<&str>,
        underlay: &BTreeSet<&str>,
        service: &BTreeSet<&str>,
    ) -> PortRole {
        let name = i.name.as_str();
        let master_is_bond = i
            .master
            .as_deref()
            .and_then(|m| self.get(m))
            .is_some_and(|m| m.kind == InterfaceKind::Bond);
        if i.kind != InterfaceKind::Bridge
            && (uplinks.contains(name) || master_is_bond || i.kind == InterfaceKind::Bond)
        {
            return PortRole::Uplink;
        }
        if underlay.contains(name) {
            return PortRole::Underlay;
        }
        if i.kind != InterfaceKind::Bridge && service.contains(name) {
            return PortRole::Service;
        }
        // 주소를 잃은 포트와 그 아래는 기억한 역할 그대로다(감시 밖으로 빠지지 않게 — 계획 75 리뷰 2).
        if self.recalled.contains(name)
            && let Some(k) = self.known(name)
            && watched_role(k.role)
        {
            return k.role;
        }
        let used = i.master.is_some()
            || !i.addresses.is_empty()
            || self.ifs.iter().any(|o| self.parent(&o.name) == Some(name));
        if matches!(i.kind, InterfaceKind::Ethernet) && !used {
            PortRole::Unused
        } else {
            PortRole::Other
        }
    }

    /// 포트 목록: 노드가 알린 인터페이스(veth 뺌) 뒤에 사라진 기억 포트(`state: missing`)
    fn ports(&self) -> Vec<Port> {
        let uplinks: BTreeSet<&str> = self.bridges.keys().flat_map(|b| self.lower(b)).collect();
        let underlay: BTreeSet<&str> = self
            .underlay_holders()
            .into_iter()
            .flat_map(|h| self.with_lower(h))
            .collect();
        let service = self.addressed();
        let present = self.ifs.iter().enumerate().map(|(n, i)| Port {
            name: i.name.clone(),
            kind: port_kind(i.kind),
            state: i.state.clone(),
            carrier: i.carrier,
            speed_mbps: i.speed_mbps,
            mtu: i.mtu,
            mac: i.mac.clone(),
            master: i.master.clone(),
            parent: self.parents[n].clone(),
            vlan_id: self.vlan_ids[n],
            lldp: i.lldp.clone(),
            bond: i.bond.as_ref().map(|b| {
                let mut members = b.ports.clone();
                for k in self.missing_members(&i.name) {
                    if !members.iter().any(|m| m.name == k.name) {
                        members.push(BondPort {
                            name: k.name.clone(),
                            state: PortState::Missing,
                            speed_mbps: None,
                            aggregator: None,
                        });
                    }
                }
                let state = if b.state == BondState::Up
                    && members.iter().any(|m| m.state == PortState::Missing)
                {
                    BondState::Degraded
                } else {
                    b.state
                };
                PortBond {
                    mode: b.mode.clone(),
                    state,
                    members,
                }
            }),
            role: self.role(i, &uplinks, &underlay, &service),
            uses: self.uses_of(&i.name),
            addresses: self.addresses_of(&i.name),
            health: self.health(&i.name),
        });
        let gone = self.missing.iter().map(|k| Port {
            name: k.name.clone(),
            kind: k.kind,
            state: "missing".into(),
            carrier: None,
            speed_mbps: None,
            mtu: None,
            mac: None,
            master: k.bond.clone(),
            parent: None,
            vlan_id: None,
            lldp: None,
            bond: None,
            role: k.role,
            uses: self.uses_of(&k.name),
            addresses: k.addresses.clone(),
            health: LinkHealth::Down,
        });
        present.chain(gone).collect()
    }

    /// 이 인터페이스를 아래에 둔 브리지(이름 순)와 underlay 주인. 사라진 기억 포트는 기억한 브리지다.
    fn tops(&self, name: &str) -> (Vec<&str>, Vec<&'a str>) {
        let gone = self.missing_named(name);
        let bridges = self
            .bridges
            .keys()
            .filter(|b| {
                self.lower(b).contains(name)
                    || gone.is_some_and(|k| k.bridges.iter().any(|x| x == *b))
            })
            .map(String::as_str)
            .collect();
        let underlay = self
            .underlay_holders()
            .into_iter()
            .filter(|h| *h == name || self.lower(h).contains(name))
            .collect();
        (bridges, underlay)
    }

    /// 이 인터페이스의 본딩(`master`가 본딩)
    fn bond_of(&self, i: &NodeInterface) -> Option<&'a NodeInterface> {
        i.master
            .as_deref()
            .and_then(|m| self.get(m))
            .filter(|m| m.kind == InterfaceKind::Bond)
    }

    /// 포트 목록 `n`번째의 본딩 이름(사라진 기억 포트는 기억한 본딩이 아직 있을 때)
    fn bond_name(&self, n: usize) -> Option<&'a str> {
        match self.ifs.get(n) {
            Some(i) => self.bond_of(i).map(|b| b.name.as_str()),
            None => self.missing[n - self.ifs.len()]
                .bond
                .as_deref()
                .filter(|b| self.get(b).is_some_and(|m| m.kind == InterfaceKind::Bond)),
        }
    }

    /// 링크가 끊긴 포트의 무게: 위의 브리지·underlay 가운데 하나라도 끊겼으면 `down`, 다 살았으면 `degraded`. 위에
    /// 아무것도 없으면 본딩(살았으면 `degraded`), 그것도 없으면 `down`이다.
    fn link_severity(&self, name: &str, bond: Option<&'a str>) -> FaultSeverity {
        let (bridges, underlay) = self.tops(name);
        let mut tops: Vec<&str> = bridges.into_iter().chain(underlay).collect();
        if tops.is_empty()
            && let Some(b) = bond
        {
            tops.push(b);
        }
        // underlay 주인이 끊긴 그 포트 자신이면 그 주소는 끊김이다.
        if tops.is_empty() || tops.iter().any(|t| self.health(t) == LinkHealth::Down) {
            FaultSeverity::Down
        } else {
            FaultSeverity::Degraded
        }
    }

    /// 링크가 끊긴 포트(이더넷·그 밖·VLAN — VLAN은 아래 장치가 살았을 때만 —, 주소를 잃은 포트, 사라진 기억 포트,
    /// 관리자가 내린 브리지 가운데 네트워크가 실리는 것 `used`)와 그 무게. 순번은 [`Host::ports`]의 것이다.
    fn link_faults(&self, ports: &[Port], used: &BTreeSet<String>) -> Vec<(usize, FaultSeverity)> {
        let present = self
            .ifs
            .iter()
            .enumerate()
            .filter(|(n, i)| {
                self.is_down(i)
                    && match ports[*n].kind {
                        PortKind::Ethernet | PortKind::Other => true,
                        PortKind::Vlan => self
                            .parent(&i.name)
                            .is_none_or(|p| self.health(p) != LinkHealth::Down),
                        PortKind::Bridge => i.admin_up == Some(false) && used.contains(&i.name),
                        PortKind::Bond => false,
                    }
            })
            .map(|(n, _)| n)
            .chain(self.ifs.len()..self.ifs.len() + self.missing.len());
        present
            .map(|n| (n, self.link_severity(&ports[n].name, self.bond_name(n))))
            .collect()
    }

    /// 저하·끊긴 본딩
    fn bond_faults(&self, ports: &[Port]) -> Vec<(usize, FaultSeverity)> {
        ports
            .iter()
            .enumerate()
            .filter(|(_, p)| p.kind == PortKind::Bond)
            .filter_map(|(n, p)| match p.health {
                LinkHealth::Down => Some((n, FaultSeverity::Down)),
                LinkHealth::Degraded => Some((n, FaultSeverity::Degraded)),
                LinkHealth::Up => None,
            })
            .collect()
    }

    /// 네트워크가 실리는 브리지(관리자가 내린 브리지는 이것만 장애로 본다 — 쓰지 않는 브리지는 영향이 없다)
    fn used_bridges(&self, node: &Node, networks: &[&Network]) -> BTreeSet<String> {
        networks
            .iter()
            .filter_map(|n| match self.carrier(node, &n.spec) {
                Some(Carrier::Bridge(b)) => Some(b),
                _ => None,
            })
            .collect()
    }
    /// 네트워크가 이 노드에서 실리는 곳. 없으면 이 노드에 쓸 수단이 없다.
    fn carrier(&self, node: &Node, spec: &NetworkSpec) -> Option<Carrier> {
        if let Some(bridge) = &spec.bridge {
            let filtering = self.bridges.get(bridge)?;
            if spec.vlan.is_some() && !filtering {
                return None;
            }
            return Some(Carrier::Bridge(bridge.clone()));
        }
        if let Some(physnet) = spec.physnet() {
            let bridge = node.spec.sdn.as_ref()?.physnets.get(physnet)?;
            return Some(Carrier::Bridge(bridge.clone()));
        }
        if spec.vxlan.is_some() {
            return self.status.underlay.map(Carrier::Underlay);
        }
        if spec.is_sdn() {
            let sdn = self.status.sdn.as_ref()?;
            if sdn.chassis == ChassisState::Missing {
                return None;
            }
            return encap_ip(self.status).map(Carrier::Underlay);
        }
        None
    }
}

/// 네트워크가 실리는 곳
#[derive(Debug, Clone, PartialEq, Eq)]
enum Carrier {
    Bridge(String),
    Underlay(IpAddr),
}

/// 링크가 끊겼는지: carrier가 없거나 operstate가 `down`·`lowerlayerdown`·`notpresent`(`unknown`은 dummy 등이 늘
/// 내므로 살아 있다고 본다)
fn link_down(i: &NodeInterface) -> bool {
    i.carrier == Some(false) || matches!(i.state.as_str(), "down" | "lowerlayerdown" | "notpresent")
}

/// 여럿 가운데 하나만 살아도 흐르는 것(브리지 업링크·본딩 구성 포트): 모두 끊기면 `down`, 일부·저하면 `degraded`
fn redundant(below: &[LinkHealth]) -> LinkHealth {
    if below.is_empty() {
        LinkHealth::Up
    } else if below.iter().all(|h| *h == LinkHealth::Down) {
        LinkHealth::Down
    } else if below.iter().any(|h| *h != LinkHealth::Up) {
        LinkHealth::Degraded
    } else {
        LinkHealth::Up
    }
}

fn port_kind(kind: InterfaceKind) -> PortKind {
    match kind {
        InterfaceKind::Ethernet => PortKind::Ethernet,
        InterfaceKind::Bond => PortKind::Bond,
        InterfaceKind::Vlan => PortKind::Vlan,
        InterfaceKind::Bridge => PortKind::Bridge,
        _ => PortKind::Other,
    }
}

/// SDN Geneve 주소(없으면 VXLAN underlay)
fn encap_ip(status: &NodeStatus) -> Option<IpAddr> {
    status
        .sdn
        .as_ref()
        .and_then(|s| s.encap_ip)
        .or(status.underlay)
}

fn rail_kind(spec: &NetworkSpec) -> (RailKind, String, Option<u16>) {
    if let Some(bridge) = &spec.bridge {
        let kind = if spec.vlan.is_some() {
            RailKind::Vlan
        } else {
            RailKind::Bridge
        };
        return (kind, bridge.clone(), spec.vlan);
    }
    if let Some(p) = spec.provider() {
        return (RailKind::SdnProvider, p.physnet.clone(), p.vlan);
    }
    if spec.vxlan.is_some() {
        return (RailKind::Vxlan, "underlay".into(), None);
    }
    (RailKind::SdnOverlay, "underlay".into(), None)
}

// ---------------------------------------------------------------------------------------------------------------
// 공개 함수
// ---------------------------------------------------------------------------------------------------------------

/// 노드 하나의 포트(역할·쓰임·상태 포함, 사라진 기억 포트도). 네트워크·VM은 보지 않는다.
pub fn ports(node: &Node) -> Vec<Port> {
    Host::of(node).ports()
}

/// 기본 링크 경보가 보는 포트인지: 업링크·underlay·주소를 가진 포트, 관리자가 내린 브리지(네트워크가 실리는 것 —
/// [`Host::link_faults`]가 거름)
fn watched(p: &Port) -> bool {
    watched_role(p.role) || p.kind == PortKind::Bridge
}

/// 링크 경보 `NodeLinkDown`: 이 노드에서 지금 링크가 끊긴 포트(주소를 잃은 기억 포트·사라진 기억 포트 포함, 계획 75
/// 리뷰 2). 기본(글롭 없음)은 본딩 구성 포트를 따로 세지 않는다(리뷰 1): 본딩이 살아 있으면 이중화만 잃은 것이라
/// `BondDegraded`가 맡고, 본딩이 끊기면 본딩 이름 하나로 낸다. `ports`(글롭)가 있으면 그 이름에 맞는 것만 보되 본딩
/// 구성 포트도 본다(리뷰 2 — `eno*`처럼 물리 NIC만 보는 규칙이 본딩 장애를 놓치지 않게). `role`이 `uplink`면 업링크·
/// underlay·주소를 가진 포트와 네트워크가 실리는데 관리자가 내린 브리지만, `any`면 이더넷·VLAN 포트도 모두(어디에도
/// 묶이지 않은 가상 장치는 빼고)다. `networks`는 관리자가 내린 브리지가 쓰이는지 보는 데만 쓴다.
pub fn link_faults(
    node: &Node,
    role: LinkRole,
    ports: Option<&str>,
    networks: &[Network],
) -> Vec<LinkFault> {
    let host = Host::of(node);
    let list = host.ports();
    let networks: Vec<&Network> = networks.iter().collect();
    let used = host.used_bridges(node, &networks);
    let members = host
        .link_faults(&list, &used)
        .into_iter()
        .filter(|(n, _)| ports.is_some() || host.bond_name(*n).is_none());
    let bonds = list
        .iter()
        .enumerate()
        .filter(|(_, p)| p.kind == PortKind::Bond && p.health == LinkHealth::Down)
        .map(|(n, _)| (n, FaultSeverity::Down));
    let mut faults: Vec<(usize, FaultSeverity)> = members.chain(bonds).collect();
    faults.sort_by_key(|(n, _)| *n);
    faults
        .into_iter()
        .map(|(n, severity)| (n, &list[n], severity))
        .filter(|(_, p, _)| match role {
            LinkRole::Uplink => watched(p),
            LinkRole::Any => watched(p) || matches!(p.kind, PortKind::Ethernet | PortKind::Vlan),
        })
        .filter(|(_, p, _)| ports.is_none_or(|g| glob_match(g, &p.name)))
        .map(|(n, p, severity)| LinkFault {
            port: p.name.clone(),
            severity,
            bond: host.bond_name(n).map(str::to_string),
            role: p.role,
        })
        .collect()
}

/// 링크 경보 `BondDegraded`: 이 노드에서 지금 저하된(`degraded`, 아직 흐르는) 본딩과 끊긴(`down`) 본딩. 끊긴 본딩은
/// `NodeLinkDown`이 본딩 이름으로 새로 내고, 경보 엔진은 이미 울린 `BondDegraded`만 끊긴 동안 그대로 둔다(계획 75
/// 리뷰 2 — 저하가 끊김으로 커질 때 "해소"를 보내지 않게).
pub fn bond_faults(node: &Node) -> Vec<LinkFault> {
    let host = Host::of(node);
    let list = host.ports();
    host.bond_faults(&list)
        .into_iter()
        .map(|(n, severity)| LinkFault {
            port: list[n].name.clone(),
            severity,
            bond: None,
            role: list[n].role,
        })
        .collect()
}

/// 노드 수집(계획 75 리뷰 2): 지금 인터페이스(`status.interfaces` — 노드 상태에 쓸 목록)와 지난 기억(`status.known_ports`)
/// 으로 새 기억을 셈한다. `address`는 노드 API 주소, `present`는 지금 이 노드에 있는 모든 인터페이스 이름(64개 상한 밖
/// 포함 — 상한 때문에 거짓으로 사라지지 않게), `now_ms`는 지금(유닉스 ms)이다.
/// - 지금 역할이 업링크·underlay·service인 이더넷·그 밖·VLAN·본딩은 기억한다(주소는 스스로 가진 전역 주소, 없으면 지난 값).
/// - 주소를 잃은 포트와 그 아래는 기억한 역할 그대로 남는다. 그 밖에 감시 밖 역할이 된 포트는 잊는다(관리자가 다시 구성).
/// - 목록에서 사라진 이더넷·그 밖은 `missing`으로 남긴다(한 번 본딩 상태를 못 읽어도 지우지 않음). VLAN·본딩은 잊는다.
/// - 본딩에 지난번 기억에 없던 구성 포트가 들어왔으면(다른 이름의 새 NIC로 다시 묶음) 그 본딩의 사라진 구성 포트를 잊는다.
pub fn remember_ports(
    status: &NodeStatus,
    address: &str,
    present: &BTreeSet<&str>,
    now_ms: u64,
) -> Vec<KnownPort> {
    let host = Host::new(status, address);
    let list = host.ports();
    let old = |name: &str| status.known_ports.iter().find(|k| k.name == name);
    let mut out: Vec<KnownPort> = Vec::new();
    for (n, i) in host.ifs.iter().enumerate() {
        let p = &list[n];
        if !matches!(
            p.kind,
            PortKind::Ethernet | PortKind::Other | PortKind::Vlan | PortKind::Bond
        ) || !watched_role(p.role)
        {
            continue;
        }
        let prev = old(&i.name);
        let own: Vec<String> = addrs(i)
            .filter(|ip| is_global(*ip))
            .map(|ip| ip.to_string())
            .collect();
        let (bridges, _) = host.tops(&i.name);
        out.push(KnownPort {
            name: i.name.clone(),
            kind: p.kind,
            role: p.role,
            addresses: if own.is_empty() && host.lost.contains(i.name.as_str()) {
                prev.map(|k| k.addresses.clone()).unwrap_or_default()
            } else {
                own
            },
            bond: host.bond_of(i).map(|b| b.name.clone()),
            bridges: bridges.into_iter().map(str::to_string).collect(),
            missing: false,
            last_seen_ms: prev
                .filter(|k| !k.missing)
                .map_or(now_ms, |k| k.last_seen_ms),
        });
    }
    // 본딩마다 지난번에 기억한 구성 포트
    let had = |bond: &str, name: &str| {
        status
            .known_ports
            .iter()
            .any(|k| k.name == name && k.bond.as_deref() == Some(bond))
    };
    let rebonded: BTreeSet<&str> = host
        .ifs
        .iter()
        .filter_map(|i| {
            let b = host.bond_of(i)?;
            let known_bond = status
                .known_ports
                .iter()
                .any(|k| k.bond.as_deref() == Some(b.name.as_str()));
            (known_bond && !had(&b.name, &i.name)).then_some(b.name.as_str())
        })
        .collect();
    for k in &status.known_ports {
        if host.get(&k.name).is_some() || !matches!(k.kind, PortKind::Ethernet | PortKind::Other) {
            continue;
        }
        if present.contains(k.name.as_str()) {
            // 상한 밖에 있음: 그대로
            out.push(k.clone());
            continue;
        }
        if k.bond.as_deref().is_some_and(|b| rebonded.contains(b)) {
            continue;
        }
        out.push(KnownPort {
            missing: true,
            last_seen_ms: if k.missing { k.last_seen_ms } else { now_ms },
            ..k.clone()
        });
    }
    out.truncate(MAX_KNOWN_PORTS);
    out
}

/// 노드의 본딩 감시(감사 `node.bond-degraded`)용: 사라진 기억 포트를 그 본딩의 `missing` 구성 포트로 넣고 `up`인 본딩은
/// `degraded`로 바꾼다(커널은 사라진 netdev를 `slaves`에서도 빼서 본딩이 "모두 up"으로 보인다). 물리 관계와 같은 판단이다.
pub fn mark_missing_members(known: &[KnownPort], list: &mut [NodeInterface]) {
    let present: BTreeSet<String> = list.iter().map(|i| i.name.clone()).collect();
    for iface in list.iter_mut() {
        let Some(bond) = iface.bond.as_mut() else {
            continue;
        };
        for k in known.iter().filter(|k| {
            k.missing
                && k.bond.as_deref() == Some(iface.name.as_str())
                && !present.contains(&k.name)
        }) {
            if !bond.ports.iter().any(|p| p.name == k.name) {
                bond.ports.push(BondPort {
                    name: k.name.clone(),
                    state: PortState::Missing,
                    speed_mbps: None,
                    aggregator: None,
                });
            }
        }
        if bond.state == BondState::Up && bond.ports.iter().any(|p| p.state == PortState::Missing) {
            bond.state = BondState::Degraded;
        }
    }
}

/// 클러스터의 물리 관계(설계 §3.2). `nodes`·`networks`·`vms`는 상태 머신의 목록 그대로다.
pub fn build(nodes: &[Node], networks: &[Network], vms: &[Vm]) -> PhysicalTopology {
    let mut nodes: Vec<&Node> = nodes.iter().collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let mut networks: Vec<&Network> = networks.iter().collect();
    networks.sort_by(|a, b| a.name.cmp(&b.name));
    let mut vms: Vec<&Vm> = vms.iter().collect();
    vms.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = PhysicalTopology::default();
    let mut rails: Vec<NetRail> = networks
        .iter()
        .map(|n| {
            let (kind, carrier, vlan) = rail_kind(&n.spec);
            NetRail {
                id: n.id,
                name: n.name.clone(),
                kind,
                carrier,
                vlan,
                nodes: Vec::new(),
            }
        })
        .collect();
    for node in nodes {
        let host = Host::of(node);
        let carriers: Vec<Option<Carrier>> = networks
            .iter()
            .map(|n| host.carrier(node, &n.spec))
            .collect();
        // 이 노드에 놓인 VM과 NIC가 붙은 네트워크(순번)
        let here: Vec<(&Vm, BTreeSet<usize>)> = vms
            .iter()
            .filter(|vm| placed_on(vm) == Some(node.id))
            .map(|vm| {
                let nets = vm
                    .spec
                    .nics
                    .iter()
                    .filter_map(|nic| networks.iter().position(|n| n.name == nic.network))
                    .collect();
                (*vm, nets)
            })
            .collect();
        let nics_on = |nets: &BTreeSet<usize>| -> u32 {
            here.iter()
                .map(|(vm, _)| {
                    vm.spec
                        .nics
                        .iter()
                        .filter(|nic| {
                            networks
                                .iter()
                                .position(|n| n.name == nic.network)
                                .is_some_and(|i| nets.contains(&i))
                        })
                        .count() as u32
                })
                .sum()
        };

        let ports = host.ports();
        let bridges = host
            .bridges
            .iter()
            .map(|(name, filtering)| {
                let nets: BTreeSet<usize> = carriers
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| matches!(c, Some(Carrier::Bridge(b)) if b == name))
                    .map(|(i, _)| i)
                    .collect();
                Bridge {
                    name: name.clone(),
                    uplinks: host
                        .ifs
                        .iter()
                        .filter(|i| i.master.as_deref() == Some(name.as_str()))
                        .map(|i| i.name.clone())
                        .chain(
                            host.missing
                                .iter()
                                .filter(|k| k.bond.is_none() && k.bridges.contains(name))
                                .map(|k| k.name.clone()),
                        )
                        .collect(),
                    vlan_filtering: *filtering,
                    networks: nets.iter().map(|i| networks[*i].id).collect(),
                    vm_nics: nics_on(&nets),
                    health: host.health(name),
                }
            })
            .collect();

        // 영향: 끊긴 포트, 그리고 구성 포트에 끊긴 것이 없는 저하·끊긴 본딩
        let used: BTreeSet<String> = carriers
            .iter()
            .filter_map(|c| match c {
                Some(Carrier::Bridge(b)) => Some(b.clone()),
                _ => None,
            })
            .collect();
        let mut faults = host.link_faults(&ports, &used);
        let linked: BTreeSet<&str> = faults
            .iter()
            .filter_map(|(n, _)| host.bond_name(*n))
            .collect();
        faults.extend(
            host.bond_faults(&ports)
                .into_iter()
                .filter(|(n, _)| !linked.contains(ports[*n].name.as_str())),
        );
        faults.sort_by_key(|(n, _)| *n);
        let mut impact = Vec::new();
        for (n, severity) in faults {
            let p = &ports[n];
            if !watched(p) {
                continue;
            }
            let (mut above, holders) = host.tops(&p.name);
            if p.kind == PortKind::Bridge {
                above.insert(0, p.name.as_str());
            }
            let underlay_ips: Vec<IpAddr> = [node.status.underlay, encap_ip(&node.status)]
                .into_iter()
                .flatten()
                .filter(|ip| {
                    host.underlay_owner(*ip)
                        .is_some_and(|h| holders.contains(&h))
                })
                .collect();
            let nets: BTreeSet<usize> = carriers
                .iter()
                .enumerate()
                .filter(|(_, c)| match c {
                    Some(Carrier::Bridge(b)) => above.contains(&b.as_str()),
                    Some(Carrier::Underlay(ip)) => underlay_ips.contains(ip),
                    None => false,
                })
                .map(|(i, _)| i)
                .collect();
            let hit: Vec<&Vm> = here
                .iter()
                .filter(|(_, v)| !v.is_disjoint(&nets))
                .map(|(vm, _)| *vm)
                .collect();
            impact.push(Impact {
                port: p.name.clone(),
                severity,
                bond: host.bond_name(n).map(str::to_string),
                missing: n >= host.ifs.len(),
                bridges: above.iter().map(|b| b.to_string()).collect(),
                underlay: !holders.is_empty(),
                uses: p.uses.clone(),
                networks: nets.iter().map(|i| networks[*i].id).collect(),
                vms: hit.len() as u32,
                vm_ids: hit.iter().take(MAX_IMPACT_VM_IDS).map(|vm| vm.id).collect(),
            });
        }

        let underlay = [node.status.underlay, encap_ip(&node.status)]
            .into_iter()
            .flatten()
            .next()
            .map(|ip| UnderlayLink {
                address: ip.to_string(),
                port: host.underlay_owner(ip).map(str::to_string),
            });

        // 도는 VM의 NIC가 붙었는데 이 노드에 쓸 수단이 없는 네트워크
        let unreachable = networks
            .iter()
            .enumerate()
            .filter(|(i, _)| carriers[*i].is_none())
            .filter_map(|(i, net)| {
                let hit: Vec<&Vm> = here
                    .iter()
                    .filter(|(_, v)| v.contains(&i))
                    .map(|(vm, _)| *vm)
                    .collect();
                (!hit.is_empty()).then(|| Unreachable {
                    network: net.id,
                    vms: hit.len() as u32,
                    vm_ids: hit.iter().take(MAX_IMPACT_VM_IDS).map(|vm| vm.id).collect(),
                })
            })
            .collect();

        if !node.spec.witness {
            for (i, rail) in rails.iter_mut().enumerate() {
                // 오프라인 노드의 상태는 낡았으므로 쓸 수단이 있던 네트워크는 "모름"이다(끊김이 아니라).
                let state = if !node.status.online {
                    match &carriers[i] {
                        None => RailState::Absent,
                        Some(_) => RailState::Unknown,
                    }
                } else {
                    match &carriers[i] {
                        None => RailState::Absent,
                        Some(Carrier::Bridge(b)) => {
                            let physnet_down = networks[i].spec.physnet().is_some_and(|name| {
                                node.status.sdn.as_ref().is_some_and(|s| {
                                    s.physnets.iter().any(|p| p.name == name && !p.ready)
                                })
                            });
                            if physnet_down {
                                RailState::Down
                            } else {
                                host.health(b).into()
                            }
                        }
                        Some(Carrier::Underlay(ip)) => {
                            let chassis_down = networks[i].spec.is_sdn()
                                && node
                                    .status
                                    .sdn
                                    .as_ref()
                                    .is_some_and(|s| s.chassis == ChassisState::Down);
                            if chassis_down {
                                RailState::Down
                            } else {
                                // 주소를 아무도 갖지 않으면 기억한 주인의 링크가 살아도 끊김, 기억도 없으면 모른다(정상으로
                                // 보이지 않게).
                                host.underlay_health(*ip)
                                    .map_or(RailState::Unknown, RailState::from)
                            }
                        }
                    }
                };
                rail.nodes.push(RailNode {
                    node: node.name.clone(),
                    state,
                });
            }
        }

        out.nodes.push(NodePhysical {
            id: node.id,
            node: node.name.clone(),
            online: node.status.online,
            legacy: !node.status.features.iter().any(|f| f == OPERATOR_DASHBOARD),
            ports,
            bridges,
            underlay,
            physnets: node
                .status
                .sdn
                .as_ref()
                .map(|s| s.physnets.clone())
                .unwrap_or_default(),
            impact,
            unreachable,
        });
    }
    out.networks = rails;
    out
}

/// 경보 문구(설계 §3.4): `eno1 down on svc01: bond0 degraded — vmbr0, 3 networks, 12 VMs`. 사라진 포트는 `missing`이다.
pub fn describe(node: &str, impact: &Impact) -> String {
    let what = if impact.missing { "missing" } else { "down" };
    let mut head = match &impact.bond {
        Some(bond) => format!(
            "{} {what} on {node}: {bond} {}",
            impact.port,
            impact.severity.as_str()
        ),
        None if impact.missing => format!("{} missing on {node}", impact.port),
        None => format!("{} {} on {node}", impact.port, impact.severity.as_str()),
    };
    push_tail(&mut head, impact);
    head
}

/// 본딩 경보 문구(계획 75 리뷰 2): `bond0 down on svc01 (eno1 down, eno2 missing) — vmbr0, 3 networks, 12 VMs`.
/// `bond`는 본딩 포트(상태·구성 포트), `impact`는 그 본딩이나 그 구성 포트의 영향(위가 같으므로 하나만 쓴다).
pub fn describe_bond(node: &str, bond: &Port, impact: Option<&Impact>) -> String {
    let state = match bond.health {
        LinkHealth::Down => "down",
        LinkHealth::Degraded => "degraded",
        LinkHealth::Up => "up",
    };
    let mut head = format!("{} {state} on {node}", bond.name);
    let members: Vec<String> = bond
        .bond
        .iter()
        .flat_map(|b| &b.members)
        .filter_map(|m| {
            let s = match m.state {
                PortState::Up => return None,
                PortState::Down => "down",
                PortState::LacpMismatch => "LACP mismatch",
                PortState::SpeedMismatch => "speed mismatch",
                PortState::Missing => "missing",
                PortState::Unknown => "unknown",
            };
            Some(format!("{} {s}", m.name))
        })
        .collect();
    if !members.is_empty() {
        head.push_str(&format!(" ({})", members.join(", ")));
    }
    if let Some(i) = impact {
        push_tail(&mut head, i);
    }
    head
}

/// 문구 꼬리: ` — 브리지, underlay, 쓰임, 네트워크 n, VM n`
fn push_tail(head: &mut String, impact: &Impact) {
    let mut parts: Vec<String> = impact.bridges.clone();
    if impact.underlay {
        parts.push("underlay".into());
    }
    parts.extend(impact.uses.iter().map(|u| u.as_str().to_string()));
    let count =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if !impact.networks.is_empty() {
        parts.push(count(impact.networks.len(), "network", "networks"));
    }
    if impact.vms > 0 {
        parts.push(count(impact.vms as usize, "VM", "VMs"));
    }
    if !parts.is_empty() {
        head.push_str(" — ");
        head.push_str(&parts.join(", "));
    }
}

/// `*`(아무 글자 여럿)·`?`(아무 글자 하나)만 아는 글롭
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // 마지막 `*`로 되돌아가는 탐욕 비교
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

/// `NodeLinkDown.ports`로 받는 글롭: 1~64자, 인터페이스 이름에 쓸 수 있는 글자(`/`·공백·`:` 없음)와 `*`·`?`
pub fn valid_port_glob(pattern: &str) -> bool {
    (1..=crate::alert::MAX_PORTS_PATTERN).contains(&pattern.len())
        && pattern
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '/' | ':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{Provider, SdnNet, Vxlan};
    use crate::node::{BondStatus, BridgeInfo, NodeSpec, PortState};
    use crate::resource::Resource;
    use crate::sdn::{NodeSdn, NodeSdnConfig};
    use crate::vm::{CpuSpec, MachineType, NicModel, NicSpec, VmPhase, VmSpec};

    fn iface(name: &str, kind: InterfaceKind) -> NodeInterface {
        NodeInterface {
            name: name.into(),
            kind,
            state: "up".into(),
            carrier: Some(true),
            speed_mbps: Some(10_000),
            mac: None,
            master: None,
            addresses: vec![],
            bond: None,
            mtu: Some(1500),
            parent: None,
            vlan_id: None,
            lldp: None,
            admin_up: None,
        }
    }

    fn eth(name: &str, master: Option<&str>) -> NodeInterface {
        NodeInterface {
            master: master.map(Into::into),
            ..iface(name, InterfaceKind::Ethernet)
        }
    }

    fn down(mut i: NodeInterface) -> NodeInterface {
        i.carrier = Some(false);
        i.state = "down".into();
        i
    }

    fn bond(
        name: &str,
        master: Option<&str>,
        state: BondState,
        ports: &[(&str, PortState)],
    ) -> NodeInterface {
        NodeInterface {
            master: master.map(Into::into),
            bond: Some(BondStatus {
                mode: "802.3ad".into(),
                state,
                ports: ports
                    .iter()
                    .map(|(n, s)| BondPort {
                        name: n.to_string(),
                        state: *s,
                        speed_mbps: Some(10_000),
                        aggregator: Some(1),
                    })
                    .collect(),
            }),
            ..iface(name, InterfaceKind::Bond)
        }
    }

    fn br(name: &str) -> NodeInterface {
        iface(name, InterfaceKind::Bridge)
    }

    fn node(name: &str, interfaces: Vec<NodeInterface>, bridges: &[(&str, bool)]) -> Node {
        let mut n: Node = Resource::new(
            name,
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
                host_resources: None,
                sdn: None,
            },
        );
        n.status.online = true;
        n.status.features = vec![OPERATOR_DASHBOARD.into()];
        n.status.interfaces = interfaces;
        n.status.bridges = bridges
            .iter()
            .map(|(b, f)| BridgeInfo {
                name: b.to_string(),
                vlan_filtering: *f,
            })
            .collect();
        n
    }

    fn net(name: &str, f: impl FnOnce(&mut NetworkSpec)) -> Network {
        let mut spec = NetworkSpec {
            bridge: None,
            vlan: None,
            vxlan: None,
            sdn: None,
            mtu: None,
            subnets: vec![],
            folder: None,
        };
        f(&mut spec);
        Resource::new(name, spec)
    }

    fn bridge_net(name: &str, bridge: &str) -> Network {
        net(name, |s| s.bridge = Some(bridge.into()))
    }

    fn vm(name: &str, node: &Node, phase: VmPhase, nets: &[&str]) -> Vm {
        let mut v: Vm = Resource::new(
            name,
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 2,
                    model: Default::default(),
                    max_vcpus: None,
                    flags: vec![],
                    policy: Default::default(),
                    thread_policy: None,
                    emulator: None,
                },
                memory_mib: 1024,
                max_memory_mib: None,
                firmware: Default::default(),
                machine: MachineType::Q35,
                disks: vec![],
                nics: nets
                    .iter()
                    .map(|n| NicSpec {
                        network: n.to_string(),
                        model: NicModel::Virtio,
                        mac: None,
                        security_groups: vec![],
                        ips: vec![],
                        qos: None,
                    })
                    .collect(),
                power: Default::default(),
                restart: Default::default(),
                node: None,
                snapshot_schedule: None,
                ha: None,
                placement: None,
                replication: vec![],
                cdroms: vec![],
                cloud_init: None,
                template: false,
                tpm: false,
                secure_boot: false,
                guest_agent: None,
                folder: None,
                numa: None,
                hugepages: None,
            },
        );
        v.status.phase = phase;
        v.status.node = Some(node.id);
        v
    }

    fn port<'a>(p: &'a PhysicalTopology, node: &str, port: &str) -> &'a Port {
        p.nodes
            .iter()
            .find(|n| n.node == node)
            .unwrap()
            .ports
            .iter()
            .find(|x| x.name == port)
            .unwrap()
    }

    fn rail(p: &PhysicalTopology, net: &str) -> Vec<(String, RailState)> {
        p.networks
            .iter()
            .find(|r| r.name == net)
            .unwrap()
            .nodes
            .iter()
            .map(|n| (n.node.clone(), n.state))
            .collect()
    }

    /// 브리지에 이더넷이 바로 붙음: 업링크, 다 살았으면 영향 없음, VM NIC를 센다.
    #[test]
    fn bridge_with_a_direct_uplink() {
        let n = node(
            "svc01",
            vec![eth("eno1", Some("vmbr0")), br("vmbr0"), eth("eno2", None)],
            &[("vmbr0", false)],
        );
        let lan = bridge_net("lan", "vmbr0");
        let vms = [
            vm("a", &n, VmPhase::Running, &["lan", "lan"]),
            vm("b", &n, VmPhase::Stopped, &["lan"]),
        ];
        let p = build(std::slice::from_ref(&n), std::slice::from_ref(&lan), &vms);
        let np = &p.nodes[0];
        assert!(!np.legacy && np.impact.is_empty());
        assert_eq!(port(&p, "svc01", "eno1").role, PortRole::Uplink);
        assert_eq!(port(&p, "svc01", "eno2").role, PortRole::Unused);
        assert_eq!(port(&p, "svc01", "vmbr0").role, PortRole::Other);
        assert_eq!(
            np.bridges,
            vec![Bridge {
                name: "vmbr0".into(),
                uplinks: vec!["eno1".into()],
                vlan_filtering: false,
                networks: vec![lan.id],
                vm_nics: 2,
                health: LinkHealth::Up,
            }]
        );
        assert_eq!(p.networks[0].kind, RailKind::Bridge);
        assert_eq!(p.networks[0].carrier, "vmbr0");
        assert_eq!(rail(&p, "lan"), [("svc01".to_string(), RailState::Up)]);
    }

    /// 포트가 끊김(본딩 없음): 브리지·네트워크·이 노드의 VM이 끊김, 레일 down. 다시 올라오면 사라진다.
    #[test]
    fn a_port_down_takes_its_bridge_down() {
        let mut n = node(
            "svc01",
            vec![down(eth("eno1", Some("vmbr0"))), br("vmbr0")],
            &[("vmbr0", false)],
        );
        let lan = bridge_net("lan", "vmbr0");
        let vms = [
            vm("a", &n, VmPhase::Running, &["lan"]),
            vm("b", &n, VmPhase::Failed, &["lan"]),
        ];
        let p = build(std::slice::from_ref(&n), std::slice::from_ref(&lan), &vms);
        let i = &p.nodes[0].impact;
        assert_eq!(
            i,
            &vec![Impact {
                port: "eno1".into(),
                severity: FaultSeverity::Down,
                bond: None,
                missing: false,
                bridges: vec!["vmbr0".into()],
                underlay: false,
                uses: vec![],
                networks: vec![lan.id],
                vms: 1,
                vm_ids: vec![vms[0].id],
            }]
        );
        assert_eq!(
            describe("svc01", &i[0]),
            "eno1 down on svc01 — vmbr0, 1 network, 1 VM"
        );
        assert_eq!(p.nodes[0].bridges[0].health, LinkHealth::Down);
        assert_eq!(rail(&p, "lan")[0].1, RailState::Down);
        let faults = link_faults(&n, LinkRole::Uplink, None, &[]);
        assert_eq!(
            faults,
            vec![LinkFault {
                port: "eno1".into(),
                severity: FaultSeverity::Down,
                bond: None,
                role: PortRole::Uplink,
            }]
        );
        n.status.interfaces[0] = eth("eno1", Some("vmbr0"));
        assert!(link_faults(&n, LinkRole::Uplink, None, &[]).is_empty());
        assert!(build(&[n], &[lan], &vms).nodes[0].impact.is_empty());
    }

    /// 본딩 위 브리지: 포트 하나가 끊기고 본딩은 저하 → `degraded`(이중화 잃음), 레일 degraded. 둘 다 끊기면
    /// 본딩 down → 둘 다 `down`.
    #[test]
    fn bond_member_down_is_degraded_then_down() {
        let lan = bridge_net("lan", "vmbr0");
        let n = node(
            "svc01",
            vec![
                down(eth("eno1", Some("bond0"))),
                eth("eno2", Some("bond0")),
                bond(
                    "bond0",
                    Some("vmbr0"),
                    BondState::Degraded,
                    &[("eno1", PortState::Down), ("eno2", PortState::Up)],
                ),
                br("vmbr0"),
            ],
            &[("vmbr0", false)],
        );
        let vms: Vec<Vm> = (0..12)
            .map(|i| vm(&format!("v{i:02}"), &n, VmPhase::Running, &["lan"]))
            .collect();
        let nets = [
            lan.clone(),
            bridge_net("lan2", "vmbr0"),
            bridge_net("lan3", "vmbr0"),
        ];
        let p = build(std::slice::from_ref(&n), &nets, &vms);
        let i = &p.nodes[0].impact;
        assert_eq!(i.len(), 1, "{i:?}");
        assert_eq!(i[0].port, "eno1");
        assert_eq!(i[0].severity, FaultSeverity::Degraded);
        assert_eq!(i[0].bond.as_deref(), Some("bond0"));
        assert_eq!(i[0].vms, 12);
        assert_eq!(
            describe("svc01", &i[0]),
            "eno1 down on svc01: bond0 degraded — vmbr0, 3 networks, 12 VMs"
        );
        for p in ["eno1", "eno2", "bond0"] {
            assert_eq!(
                port(&build(std::slice::from_ref(&n), &nets, &vms), "svc01", p).role,
                PortRole::Uplink
            );
        }
        assert_eq!(port(&p, "svc01", "bond0").health, LinkHealth::Degraded);
        assert_eq!(
            port(&p, "svc01", "bond0")
                .bond
                .as_ref()
                .unwrap()
                .members
                .len(),
            2
        );
        assert_eq!(rail(&p, "lan")[0].1, RailState::Degraded);
        assert_eq!(
            bond_faults(&n),
            vec![LinkFault {
                port: "bond0".into(),
                severity: FaultSeverity::Degraded,
                bond: None,
                role: PortRole::Uplink,
            }]
        );
        // 본딩이 살아 있으면 구성 포트의 끊김은 `node_link_down`이 아니다(이중화만 잃음 — `bond_degraded` 하나).
        assert!(link_faults(&n, LinkRole::Uplink, None, &[]).is_empty());
        assert!(link_faults(&n, LinkRole::Any, None, &[]).is_empty());

        let mut n = n;
        n.status.interfaces[1] = down(eth("eno2", Some("bond0")));
        n.status.interfaces[2].bond.as_mut().unwrap().state = BondState::Down;
        let p = build(std::slice::from_ref(&n), &nets, &vms);
        let i = &p.nodes[0].impact;
        assert_eq!(
            i.iter()
                .map(|x| (x.port.as_str(), x.severity))
                .collect::<Vec<_>>(),
            [("eno1", FaultSeverity::Down), ("eno2", FaultSeverity::Down)]
        );
        assert_eq!(
            describe("svc01", &i[0]),
            "eno1 down on svc01: bond0 down — vmbr0, 3 networks, 12 VMs"
        );
        assert_eq!(rail(&p, "lan")[0].1, RailState::Down);
        // 본딩이 끊기면 본딩 이름 하나로 `node_link_down`(구성 포트는 따로 없음). `bond_faults`에는 down으로 남는다
        // (엔진이 이미 울린 `bond_degraded`만 이어 감 — 리뷰 2).
        assert_eq!(
            bond_faults(&n)
                .iter()
                .map(|f| f.severity)
                .collect::<Vec<_>>(),
            [FaultSeverity::Down]
        );
        assert_eq!(
            link_faults(&n, LinkRole::Uplink, None, &[]),
            vec![LinkFault {
                port: "bond0".into(),
                severity: FaultSeverity::Down,
                bond: None,
                role: PortRole::Uplink,
            }]
        );
    }

    /// 본딩이 저하인데 끊긴 구성 포트는 없음(LACP 어긋남): 본딩 자체가 영향의 포트다.
    #[test]
    fn bond_degraded_without_a_link_down_is_reported_on_the_bond() {
        let n = node(
            "svc01",
            vec![
                eth("eno1", Some("bond0")),
                eth("eno2", Some("bond0")),
                bond(
                    "bond0",
                    Some("vmbr0"),
                    BondState::Degraded,
                    &[("eno1", PortState::Up), ("eno2", PortState::LacpMismatch)],
                ),
                br("vmbr0"),
            ],
            &[("vmbr0", false)],
        );
        let p = build(std::slice::from_ref(&n), &[bridge_net("lan", "vmbr0")], &[]);
        let i = &p.nodes[0].impact;
        assert_eq!(i.len(), 1);
        assert_eq!(
            (i[0].port.as_str(), i[0].severity, i[0].bond.as_deref()),
            ("bond0", FaultSeverity::Degraded, None)
        );
        assert_eq!(
            describe("svc01", &i[0]),
            "bond0 degraded on svc01 — vmbr0, 1 network"
        );
        assert!(link_faults(&n, LinkRole::Any, None, &[]).is_empty());
    }

    /// 본딩 위 VLAN 위 브리지(`eno1,eno2 → bond0 → bond0.100 → vmbr100`): 사슬 끝의 포트가 업링크이고 영향이
    /// 브리지까지 올라간다. VLAN 아래 장치는 노드가 알린 `parent`로 따른다.
    #[test]
    fn vlan_on_a_bond_under_a_bridge() {
        let mut vlan = iface("bond0.100", InterfaceKind::Vlan);
        vlan.master = Some("vmbr100".into());
        vlan.parent = Some("bond0".into());
        vlan.vlan_id = Some(100);
        let n = node(
            "svc01",
            vec![
                down(eth("eno1", Some("bond0"))),
                eth("eno2", Some("bond0")),
                bond(
                    "bond0",
                    None,
                    BondState::Degraded,
                    &[("eno1", PortState::Down), ("eno2", PortState::Up)],
                ),
                vlan,
                br("vmbr100"),
            ],
            &[("vmbr100", false)],
        );
        let lan = bridge_net("v100", "vmbr100");
        let p = build(std::slice::from_ref(&n), std::slice::from_ref(&lan), &[]);
        assert_eq!(port(&p, "svc01", "bond0.100").role, PortRole::Uplink);
        assert_eq!(port(&p, "svc01", "bond0.100").health, LinkHealth::Degraded);
        assert_eq!(port(&p, "svc01", "bond0").role, PortRole::Uplink);
        let i = &p.nodes[0].impact[0];
        assert_eq!(
            (i.port.as_str(), i.severity),
            ("eno1", FaultSeverity::Degraded)
        );
        assert_eq!(i.bridges, ["vmbr100"]);
        assert_eq!(i.networks, [lan.id]);
        assert_eq!(rail(&p, "v100")[0].1, RailState::Degraded);
    }

    /// VLAN 태그 네트워크는 VLAN-aware 브리지에서만 레일이 있다(아니면 absent). 레일 종류 `vlan`, 태그를 적는다.
    #[test]
    fn vlan_network_needs_a_vlan_aware_bridge() {
        let a = node(
            "a",
            vec![eth("eno1", Some("vmbr0")), br("vmbr0")],
            &[("vmbr0", true)],
        );
        let b = node(
            "b",
            vec![eth("eno1", Some("vmbr0")), br("vmbr0")],
            &[("vmbr0", false)],
        );
        let tagged = net("tagged", |s| {
            s.bridge = Some("vmbr0".into());
            s.vlan = Some(30);
        });
        let p = build(&[b, a], &[tagged], &[]);
        assert_eq!(p.networks[0].kind, RailKind::Vlan);
        assert_eq!(p.networks[0].vlan, Some(30));
        assert_eq!(
            rail(&p, "tagged"),
            [
                ("a".to_string(), RailState::Up),
                ("b".to_string(), RailState::Absent)
            ]
        );
        assert_eq!(p.nodes[0].node, "a");
    }

    /// 브리지 없음: 네트워크의 브리지가 없는 노드는 absent, 영향 없음. 오프라인 노드는 쓸 수단이 있던 레일이 모름(낡은
    /// 상태로 끊김을 그리지 않음), 없던 레일은 absent, 위트니스는 레일에 없다.
    #[test]
    fn missing_bridge_offline_and_witness_nodes() {
        let a = node(
            "a",
            vec![eth("eno1", Some("vmbr0")), br("vmbr0")],
            &[("vmbr0", false)],
        );
        let b = node("b", vec![eth("eno1", None)], &[]);
        let mut c = node(
            "c",
            vec![eth("eno1", Some("vmbr0")), br("vmbr0")],
            &[("vmbr0", false)],
        );
        c.status.online = false;
        let mut w = node("w", vec![], &[]);
        w.spec.witness = true;
        let p = build(&[a, b, c, w], &[bridge_net("lan", "vmbr0")], &[]);
        assert_eq!(
            rail(&p, "lan"),
            [
                ("a".to_string(), RailState::Up),
                ("b".to_string(), RailState::Absent),
                ("c".to_string(), RailState::Unknown),
            ]
        );
        let mut d = node("d", vec![eth("eno1", None)], &[]);
        d.status.online = false;
        let p2 = build(&[d], &[bridge_net("lan", "vmbr0")], &[]);
        assert_eq!(rail(&p2, "lan")[0].1, RailState::Absent);
        assert_eq!(p.nodes.len(), 4);
        assert!(p.nodes.iter().all(|n| n.impact.is_empty()));
    }

    /// 공급자 physnet: 노드의 physnet 브리지로 따라가고, physnet이 준비되지 않았으면 down, 연결이 없으면 absent.
    #[test]
    fn provider_physnet_follows_the_node_bridge() {
        let mut a = node(
            "a",
            vec![down(eth("eno3", Some("vmbr1"))), br("vmbr1")],
            &[("vmbr1", false)],
        );
        a.spec.sdn = Some(NodeSdnConfig {
            physnets: [("ext".to_string(), "vmbr1".to_string())].into(),
            ..Default::default()
        });
        let mut b = node(
            "b",
            vec![eth("eno3", Some("vmbr1")), br("vmbr1")],
            &[("vmbr1", false)],
        );
        b.spec.sdn = a.spec.sdn.clone();
        b.status.sdn = Some(NodeSdn {
            physnets: vec![PhysnetState {
                name: "ext".into(),
                bridge: "vmbr1".into(),
                ready: false,
                error: Some("no bridge".into()),
            }],
            ..Default::default()
        });
        let c = node(
            "c",
            vec![eth("eno3", Some("vmbr1")), br("vmbr1")],
            &[("vmbr1", false)],
        );
        let ext = net("ext-net", |s| {
            s.sdn = Some(SdnNet {
                provider: Some(Provider {
                    physnet: "ext".into(),
                    vlan: Some(7),
                }),
            })
        });
        let p = build(
            &[a.clone(), b, c],
            std::slice::from_ref(&ext),
            &[vm("x", &a, VmPhase::Running, &["ext-net"])],
        );
        assert_eq!(p.networks[0].kind, RailKind::SdnProvider);
        assert_eq!(p.networks[0].carrier, "ext");
        assert_eq!(p.networks[0].vlan, Some(7));
        assert_eq!(
            rail(&p, "ext-net"),
            [
                ("a".to_string(), RailState::Down),
                ("b".to_string(), RailState::Down),
                ("c".to_string(), RailState::Absent),
            ]
        );
        let i = &p.nodes[0].impact[0];
        assert_eq!(
            (i.networks.as_slice(), i.vms),
            (std::slice::from_ref(&ext.id), 1)
        );
        assert_eq!(p.nodes[0].bridges[0].networks, [ext.id]);
        assert_eq!(p.nodes[1].physnets[0].error.as_deref(), Some("no bridge"));
    }

    /// VXLAN underlay: underlay 주소를 가진 인터페이스와 그 아래가 underlay 역할, 그 포트가 끊기면 VXLAN 네트워크가
    /// 영향받는다. underlay가 없는 노드는 absent.
    #[test]
    fn vxlan_follows_the_underlay_interface() {
        let mut ul = eth("eno4", None);
        ul.addresses = vec!["10.1.0.11/24".into()];
        let mut a = node("a", vec![down(ul), eth("eno1", None)], &[]);
        a.status.underlay = Some("10.1.0.11".parse().unwrap());
        let b = node("b", vec![], &[]);
        let vx = net("vx", |s| s.vxlan = Some(Vxlan { vni: 5000 }));
        let vms = [vm("x", &a, VmPhase::Migrating, &["vx"])];
        let p = build(&[a.clone(), b], std::slice::from_ref(&vx), &vms);
        assert_eq!(p.networks[0].kind, RailKind::Vxlan);
        assert_eq!(p.networks[0].carrier, "underlay");
        assert_eq!(
            p.nodes[0].underlay,
            Some(UnderlayLink {
                address: "10.1.0.11".into(),
                port: Some("eno4".into()),
            })
        );
        assert_eq!(port(&p, "a", "eno4").role, PortRole::Underlay);
        let i = &p.nodes[0].impact[0];
        assert!(i.underlay && i.bridges.is_empty());
        assert_eq!(
            (i.networks.as_slice(), i.vms),
            (std::slice::from_ref(&vx.id), 1)
        );
        assert_eq!(
            describe("a", i),
            "eno4 down on a — underlay, 1 network, 1 VM"
        );
        assert_eq!(
            rail(&p, "vx"),
            [
                ("a".to_string(), RailState::Down),
                ("b".to_string(), RailState::Absent)
            ]
        );
        // `uplink`은 underlay 포트를 보고, 쓰지 않는 포트는 `any`에서만 본다.
        a.status.interfaces[1] = down(eth("eno1", None));
        let names = |role| -> Vec<String> {
            link_faults(&a, role, None, &[])
                .into_iter()
                .map(|f| f.port)
                .collect()
        };
        assert_eq!(names(LinkRole::Uplink), ["eno4"]);
        assert_eq!(names(LinkRole::Any), ["eno4", "eno1"]);
        assert!(
            link_faults(&a, LinkRole::Any, Some("eno1"), &[])
                .iter()
                .all(|f| f.port == "eno1")
        );
        assert!(link_faults(&a, LinkRole::Any, Some("eth*"), &[]).is_empty());
    }

    /// SDN 오버레이: Geneve 주소(`encap_ip`)의 인터페이스로 따라가고, ovn-controller가 꺼졌으면 down, chassis가 아니면
    /// absent.
    #[test]
    fn sdn_overlay_follows_the_encap_address() {
        let mut ul = eth("eno4", Some("bond1"));
        ul.addresses = vec![];
        let mut b1 = bond("bond1", None, BondState::Up, &[("eno4", PortState::Up)]);
        b1.addresses = vec!["10.2.0.11/24".into()];
        let mut a = node("a", vec![ul, b1], &[]);
        a.status.sdn = Some(NodeSdn {
            chassis: ChassisState::Ready,
            encap_ip: Some("10.2.0.11".parse().unwrap()),
            ..Default::default()
        });
        let mut b = a.clone();
        b.name = "b".into();
        b.status.sdn.as_mut().unwrap().chassis = ChassisState::Down;
        let mut c = a.clone();
        c.name = "c".into();
        c.status.sdn = None;
        let ov = net("ov", |s| s.sdn = Some(SdnNet::default()));
        let p = build(&[a, b, c], &[ov], &[]);
        assert_eq!(p.networks[0].kind, RailKind::SdnOverlay);
        assert_eq!(
            rail(&p, "ov"),
            [
                ("a".to_string(), RailState::Up),
                ("b".to_string(), RailState::Down),
                ("c".to_string(), RailState::Absent),
            ]
        );
        assert_eq!(
            p.nodes[0].underlay.as_ref().unwrap().port.as_deref(),
            Some("bond1")
        );
        assert_eq!(port(&p, "a", "bond1").role, PortRole::Uplink);
    }

    /// 옛 노드: `parent`·`vlan_id`·`mtu`가 없으면 VLAN 아래 장치를 이름 규칙으로 추정하고 `legacy`로 표시한다.
    #[test]
    fn old_nodes_guess_the_vlan_parent_from_the_name() {
        let old = |mut i: NodeInterface| {
            i.mtu = None;
            i
        };
        let mut vlan = old(iface("eno1.100", InterfaceKind::Vlan));
        vlan.master = Some("vmbr100".into());
        vlan.carrier = Some(false);
        vlan.state = "lowerlayerdown".into();
        let mut n = node(
            "old",
            vec![old(down(eth("eno1", None))), vlan, old(br("vmbr100"))],
            &[("vmbr100", false)],
        );
        n.status.features = vec!["numa".into()];
        let p = build(
            std::slice::from_ref(&n),
            &[bridge_net("v100", "vmbr100")],
            &[],
        );
        assert!(p.nodes[0].legacy);
        let v = port(&p, "old", "eno1.100");
        assert_eq!(
            (v.parent.as_deref(), v.vlan_id, v.mtu),
            (Some("eno1"), Some(100), None)
        );
        assert_eq!(port(&p, "old", "eno1").role, PortRole::Uplink);
        let i = &p.nodes[0].impact;
        assert_eq!(i.len(), 1, "the VLAN is down because its parent is: {i:?}");
        assert_eq!(
            (i[0].port.as_str(), i[0].bridges.as_slice()),
            ("eno1", ["vmbr100".to_string()].as_slice())
        );
        // 인터페이스를 알리지 않은 아주 옛 노드는 브리지 목록만으로 레일이 올라와 있다고 본다.
        let older = node("older", vec![], &[("vmbr100", false)]);
        let p = build(&[older], &[bridge_net("v100", "vmbr100")], &[]);
        assert_eq!(rail(&p, "v100")[0].1, RailState::Up);
        assert_eq!(p.nodes[0].bridges[0].uplinks, Vec::<String>::new());
    }

    /// 업링크가 둘인 브리지에서 하나만 끊기면 `degraded`, 업링크 없는 내부 브리지는 carrier가 없어도 up이다.
    #[test]
    fn bridge_health_needs_every_uplink_down() {
        let mut inner = br("vmbr9");
        inner.carrier = Some(false);
        inner.state = "down".into();
        let n = node(
            "a",
            vec![
                down(eth("eno1", Some("vmbr0"))),
                eth("eno2", Some("vmbr0")),
                br("vmbr0"),
                inner,
            ],
            &[("vmbr0", false), ("vmbr9", false)],
        );
        let p = build(
            std::slice::from_ref(&n),
            &[bridge_net("lan", "vmbr0"), bridge_net("int", "vmbr9")],
            &[],
        );
        assert_eq!(p.nodes[0].bridges[0].health, LinkHealth::Degraded);
        assert_eq!(p.nodes[0].bridges[1].health, LinkHealth::Up);
        assert_eq!(p.nodes[0].impact[0].severity, FaultSeverity::Degraded);
        assert_eq!(rail(&p, "int")[0].1, RailState::Up);
    }

    /// 영향의 VM id는 50개까지(수는 다 센다), 실행하지 않는 VM은 세지 않는다.
    #[test]
    fn impact_caps_vm_ids() {
        let n = node(
            "a",
            vec![down(eth("eno1", Some("vmbr0"))), br("vmbr0")],
            &[("vmbr0", false)],
        );
        let mut vms: Vec<Vm> = (0..60)
            .map(|i| vm(&format!("v{i:02}"), &n, VmPhase::Running, &["lan"]))
            .collect();
        vms.push(vm("stopped", &n, VmPhase::Stopped, &["lan"]));
        let p = build(
            std::slice::from_ref(&n),
            &[bridge_net("lan", "vmbr0")],
            &vms,
        );
        let i = &p.nodes[0].impact[0];
        assert_eq!((i.vms, i.vm_ids.len()), (60, MAX_IMPACT_VM_IDS));
        assert_eq!(i.vm_ids[0], vms[0].id);
        assert_eq!(p.nodes[0].bridges[0].vm_nics, 60);
    }

    /// 주소만 가진 비본딩 NIC(스토리지·클러스터 링크·관리망)는 역할 `service`라 기본 경보·영향에 들고, 노드 주소의 쓰임을
    /// 꼬리표로 단다. 링크 로컬 주소만 가진 포트는 아니다.
    #[test]
    fn addressed_ports_are_service_ports() {
        let mut storage = eth("ens1f0", None);
        storage.addresses = vec!["10.10.0.11/24".into(), "fe80::1/64".into()];
        let mut vlan = iface("eno3.20", InterfaceKind::Vlan);
        vlan.parent = Some("eno3".into());
        vlan.addresses = vec!["192.168.0.181/24".into()];
        let mut ll = eth("eno4", None);
        ll.addresses = vec!["fe80::2/64".into(), "169.254.1.1/16".into()];
        let mut n = node(
            "svc01",
            vec![down(storage), eth("eno3", None), vlan, down(ll)],
            &[],
        );
        n.spec.address = "192.168.0.181:8006".into();
        n.status.cluster_addresses = vec!["10.10.0.11:8007".into()];
        let p = build(std::slice::from_ref(&n), &[], &[]);
        let s = port(&p, "svc01", "ens1f0");
        assert_eq!(
            (s.role, s.uses.as_slice()),
            (PortRole::Service, [PortUse::Cluster].as_slice())
        );
        for name in ["eno3", "eno3.20"] {
            let x = port(&p, "svc01", name);
            assert_eq!(
                (x.role, x.uses.as_slice()),
                (PortRole::Service, [PortUse::Management].as_slice()),
                "{name}"
            );
        }
        assert_eq!(port(&p, "svc01", "eno4").role, PortRole::Other);
        let i = &p.nodes[0].impact;
        assert_eq!(i.len(), 1, "{i:?}");
        assert_eq!(i[0].uses, [PortUse::Cluster]);
        assert_eq!(
            describe("svc01", &i[0]),
            "ens1f0 down on svc01 — cluster link"
        );
        let names: Vec<String> = link_faults(&n, LinkRole::Uplink, None, &[])
            .into_iter()
            .map(|f| f.port)
            .collect();
        assert_eq!(names, ["ens1f0"]);
    }

    fn known(name: &str, role: PortRole, addresses: &[&str]) -> KnownPort {
        KnownPort {
            name: name.into(),
            kind: PortKind::Ethernet,
            role,
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
            bond: None,
            bridges: vec![],
            missing: false,
            last_seen_ms: 1,
        }
    }

    fn gone(mut k: KnownPort) -> KnownPort {
        k.missing = true;
        k
    }

    /// underlay 주소가 떨어진 인터페이스(NetworkManager가 carrier를 잃은 장치의 주소를 뗌): 기억 포트(`known_ports`)로 그
    /// 포트를 계속 본다. 그 포트가 사라졌으면 끊김, 기억도 없으면 레일은 모름이다. 주소만 떨어지고 링크가 살았어도 끊김이다.
    #[test]
    fn underlay_falls_back_to_the_last_seen_port() {
        let mut a = node("a", vec![down(eth("eno4", None))], &[]);
        a.status.underlay = Some("10.1.0.11".parse().unwrap());
        a.status.known_ports = vec![known("eno4", PortRole::Underlay, &["10.1.0.11"])];
        let vx = net("vx", |s| s.vxlan = Some(Vxlan { vni: 5000 }));
        let p = build(std::slice::from_ref(&a), std::slice::from_ref(&vx), &[]);
        assert_eq!(port(&p, "a", "eno4").role, PortRole::Underlay);
        assert_eq!(port(&p, "a", "eno4").addresses, ["10.1.0.11"]);
        assert_eq!(
            p.nodes[0].underlay.as_ref().unwrap().port.as_deref(),
            Some("eno4")
        );
        assert_eq!(rail(&p, "vx")[0].1, RailState::Down);
        assert!(p.nodes[0].impact[0].underlay);
        assert_eq!(link_faults(&a, LinkRole::Uplink, None, &[])[0].port, "eno4");
        // 링크는 살았는데 주소만 없음(DHCP 만료·`ip addr del`) → 그래도 끊김(리뷰 2)
        a.status.interfaces = vec![eth("eno4", None)];
        let p = build(std::slice::from_ref(&a), std::slice::from_ref(&vx), &[]);
        assert_eq!(rail(&p, "vx")[0].1, RailState::Down);
        assert_eq!(port(&p, "a", "eno4").health, LinkHealth::Down);
        assert_eq!(link_faults(&a, LinkRole::Uplink, None, &[])[0].port, "eno4");
        // 그 인터페이스가 사라짐 → 끊김, 포트 목록에 missing으로 남음
        a.status.interfaces.clear();
        a.status.known_ports[0].missing = true;
        let p = build(std::slice::from_ref(&a), std::slice::from_ref(&vx), &[]);
        assert_eq!(rail(&p, "vx")[0].1, RailState::Down);
        assert_eq!(
            (
                port(&p, "a", "eno4").state.as_str(),
                port(&p, "a", "eno4").health
            ),
            ("missing", LinkHealth::Down)
        );
        let i = &p.nodes[0].impact[0];
        assert!(i.missing && i.underlay, "{i:?}");
        assert_eq!(describe("a", i), "eno4 missing on a — underlay, 1 network");
        assert_eq!(link_faults(&a, LinkRole::Uplink, None, &[])[0].port, "eno4");
        // 기억도 없음 → 모름(정상이 아님)
        a.status.known_ports.clear();
        let p = build(std::slice::from_ref(&a), std::slice::from_ref(&vx), &[]);
        assert_eq!(rail(&p, "vx")[0].1, RailState::Unknown);
        assert_eq!(p.nodes[0].underlay.as_ref().unwrap().port, None);
    }

    /// 주소만 가진 스토리지 NIC의 링크가 끊겨 NetworkManager가 주소를 뗌(리뷰 2): 기억한 역할(service)·쓰임·주소 그대로
    /// 끊김이다. 주소가 다른 인터페이스로 옮겨 갔으면 잃은 것이 아니다.
    #[test]
    fn a_service_port_that_lost_its_address_stays_watched() {
        let mut n = node("svc01", vec![down(eth("ens1f0", None))], &[]);
        n.status.cluster_addresses = vec!["10.10.0.11:8007".into()];
        n.status.known_ports = vec![known("ens1f0", PortRole::Service, &["10.10.0.11"])];
        let p = build(std::slice::from_ref(&n), &[], &[]);
        let s = port(&p, "svc01", "ens1f0");
        assert_eq!(
            (s.role, s.uses.as_slice(), s.addresses.as_slice()),
            (
                PortRole::Service,
                [PortUse::Cluster].as_slice(),
                ["10.10.0.11".to_string()].as_slice()
            )
        );
        assert_eq!(
            describe("svc01", &p.nodes[0].impact[0]),
            "ens1f0 down on svc01 — cluster link"
        );
        assert_eq!(
            link_faults(&n, LinkRole::Uplink, None, &[])[0].port,
            "ens1f0"
        );
        // 링크가 살아 있어도 주소를 잃었으면 끊김
        n.status.interfaces = vec![eth("ens1f0", None)];
        assert_eq!(
            link_faults(&n, LinkRole::Uplink, None, &[])[0].port,
            "ens1f0"
        );
        // 주소가 bond1으로 옮겨 감 → 잃은 것이 아니고 감시 밖(쓰지 않는 포트)
        let mut bond1 = bond("bond1", None, BondState::Up, &[]);
        bond1.addresses = vec!["10.10.0.11/24".into()];
        n.status.interfaces = vec![eth("ens1f0", None), bond1];
        let p = build(std::slice::from_ref(&n), &[], &[]);
        assert_eq!(port(&p, "svc01", "ens1f0").role, PortRole::Unused);
        assert!(link_faults(&n, LinkRole::Uplink, None, &[]).is_empty());
    }

    /// 주소를 VLAN에 둔 포트: 주소를 잃으면 VLAN과 그 아래 이더넷이 기억한 역할로 남고, 끊긴 이더넷 하나로 낸다.
    #[test]
    fn a_vlan_service_address_recalls_the_lower_port() {
        let mut vlan = iface("eno3.20", InterfaceKind::Vlan);
        vlan.parent = Some("eno3".into());
        let mut n = node("a", vec![down(eth("eno3", None)), down(vlan)], &[]);
        let mut kv = known("eno3.20", PortRole::Service, &["10.20.0.1"]);
        kv.kind = PortKind::Vlan;
        n.status.known_ports = vec![known("eno3", PortRole::Service, &[]), kv];
        let p = build(std::slice::from_ref(&n), &[], &[]);
        assert_eq!(port(&p, "a", "eno3").role, PortRole::Service);
        assert_eq!(port(&p, "a", "eno3.20").role, PortRole::Service);
        let names: Vec<String> = link_faults(&n, LinkRole::Uplink, None, &[])
            .into_iter()
            .map(|f| f.port)
            .collect();
        assert_eq!(names, ["eno3"]);
    }

    /// 본딩 밖 브리지 업링크의 netdev가 사라짐(리뷰 2): 기억 포트로 브리지가 업링크를 잃어 끊김, 영향·경보가 난다.
    #[test]
    fn a_missing_bridge_uplink_takes_the_bridge_down() {
        let mut n = node("a", vec![br("vmbr0")], &[("vmbr0", false)]);
        let mut k = gone(known("eno1", PortRole::Uplink, &[]));
        k.bridges = vec!["vmbr0".into()];
        n.status.known_ports = vec![k];
        let lan = bridge_net("lan", "vmbr0");
        let vms = [vm("x", &n, VmPhase::Running, &["lan"])];
        let p = build(std::slice::from_ref(&n), std::slice::from_ref(&lan), &vms);
        let b = &p.nodes[0].bridges[0];
        assert_eq!(
            (b.health, b.uplinks.as_slice()),
            (LinkHealth::Down, ["eno1".to_string()].as_slice())
        );
        assert_eq!(rail(&p, "lan")[0].1, RailState::Down);
        let i = &p.nodes[0].impact[0];
        assert_eq!(
            describe("a", i),
            "eno1 missing on a — vmbr0, 1 network, 1 VM"
        );
        assert_eq!(link_faults(&n, LinkRole::Uplink, None, &[])[0].port, "eno1");
    }

    /// 본딩 구성 포트가 노드에서 사라짐(기억 포트가 `missing`): 본딩은 저하이고 사라진 이름이 구성 포트·영향·문구에 보인다.
    #[test]
    fn a_missing_bond_member_degrades_the_bond() {
        let mut n = node(
            "a",
            vec![
                eth("eno1", Some("bond0")),
                bond(
                    "bond0",
                    Some("vmbr0"),
                    BondState::Up,
                    &[("eno1", PortState::Up)],
                ),
                br("vmbr0"),
            ],
            &[("vmbr0", false)],
        );
        let mut k = gone(known("eno2", PortRole::Uplink, &[]));
        k.bond = Some("bond0".into());
        k.bridges = vec!["vmbr0".into()];
        n.status.known_ports = vec![k];
        let p = build(std::slice::from_ref(&n), &[bridge_net("lan", "vmbr0")], &[]);
        let b = port(&p, "a", "bond0");
        assert_eq!(b.health, LinkHealth::Degraded);
        let members = &b.bond.as_ref().unwrap().members;
        assert_eq!(
            (members[1].name.as_str(), members[1].state),
            ("eno2", PortState::Missing)
        );
        assert_eq!(rail(&p, "lan")[0].1, RailState::Degraded);
        assert_eq!(port(&p, "a", "eno2").state, "missing");
        let i = &p.nodes[0].impact;
        assert_eq!(
            (
                i[0].port.as_str(),
                i[0].severity,
                i[0].bond.as_deref(),
                i[0].missing
            ),
            ("eno2", FaultSeverity::Degraded, Some("bond0"), true)
        );
        assert_eq!(
            describe("a", &i[0]),
            "eno2 missing on a: bond0 degraded — vmbr0, 1 network"
        );
        assert_eq!(
            describe_bond("a", b, Some(&i[0])),
            "bond0 degraded on a (eno2 missing) — vmbr0, 1 network"
        );
        assert_eq!(bond_faults(&n)[0].port, "bond0");
        assert!(link_faults(&n, LinkRole::Any, None, &[]).is_empty());
        // 노드의 본딩 감시도 같은 판단
        let mut list = n.status.interfaces.clone();
        mark_missing_members(&n.status.known_ports, &mut list);
        assert_eq!(list[1].bond.as_ref().unwrap().state, BondState::Degraded);
    }

    /// 글롭 규칙(`eno*`)은 본딩 구성 포트도 본다(리뷰 2): 본딩이 통째로 끊기면 구성 포트마다 낸다. 끊긴 본딩은
    /// `bond_faults`에 down으로 남는다(엔진이 이미 울린 저하 경보를 이어 감).
    #[test]
    fn globs_see_bond_members_and_down_bonds_stay_in_bond_faults() {
        let n = node(
            "a",
            vec![
                down(eth("eno1", Some("bond0"))),
                down(eth("eno2", Some("bond0"))),
                bond(
                    "bond0",
                    Some("vmbr0"),
                    BondState::Down,
                    &[("eno1", PortState::Down), ("eno2", PortState::Down)],
                ),
                br("vmbr0"),
            ],
            &[("vmbr0", false)],
        );
        let names = |f: Vec<LinkFault>| f.into_iter().map(|f| f.port).collect::<Vec<_>>();
        assert_eq!(
            names(link_faults(&n, LinkRole::Uplink, Some("eno*"), &[])),
            ["eno1", "eno2"]
        );
        assert_eq!(
            names(link_faults(&n, LinkRole::Uplink, None, &[])),
            ["bond0"]
        );
        let b = bond_faults(&n);
        assert_eq!(
            (b[0].port.as_str(), b[0].severity),
            ("bond0", FaultSeverity::Down)
        );
        let p = build(std::slice::from_ref(&n), &[], &[]);
        assert_eq!(
            describe_bond("a", port(&p, "a", "bond0"), None),
            "bond0 down on a (eno1 down, eno2 down)"
        );
    }

    /// 노드 수집의 기억(리뷰 2): 감시하는 포트를 기억하고, 주소를 잃어도 남고, 사라지면 missing(한 번 시각을 적음),
    /// VLAN은 사라지면 잊고, 상한 밖에 있는 것은 그대로, 감시 밖이 된 포트는 잊고, 본딩에 새 구성 포트가 오면 그 본딩의
    /// 사라진 구성 포트를 잊는다.
    #[test]
    fn remember_ports_tracks_roles_addresses_and_missing_ports() {
        let mut storage = eth("ens1f0", None);
        storage.addresses = vec!["10.10.0.11/24".into()];
        let mut vlan = iface("eno1.100", InterfaceKind::Vlan);
        vlan.parent = Some("eno1".into());
        vlan.master = Some("vmbr100".into());
        let mut n = node(
            "a",
            vec![
                storage,
                eth("eno1", None),
                vlan,
                br("vmbr100"),
                eth("eno2", Some("bond0")),
                eth("eno3", Some("bond0")),
                bond("bond0", None, BondState::Up, &[]),
                eth("eno9", None),
            ],
            &[("vmbr100", false)],
        );
        let all = |n: &Node| -> BTreeSet<String> {
            n.status.interfaces.iter().map(|i| i.name.clone()).collect()
        };
        let remember = |n: &Node, extra: &[&str], now: u64| {
            let names = all(n);
            let mut present: BTreeSet<&str> = names.iter().map(String::as_str).collect();
            present.extend(extra);
            remember_ports(&n.status, &n.spec.address, &present, now)
        };
        n.status.known_ports = remember(&n, &[], 10);
        let k = |n: &Node, name: &str| {
            n.status
                .known_ports
                .iter()
                .find(|k| k.name == name)
                .cloned()
        };
        let names: Vec<&str> = n
            .status
            .known_ports
            .iter()
            .map(|k| k.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["ens1f0", "eno1", "eno1.100", "eno2", "eno3", "bond0"]
        );
        assert_eq!(k(&n, "ens1f0").unwrap().addresses, ["10.10.0.11"]);
        assert_eq!(k(&n, "eno1").unwrap().bridges, ["vmbr100"]);
        assert_eq!(k(&n, "eno2").unwrap().bond.as_deref(), Some("bond0"));
        // 주소를 잃음(링크 끊김) → 그대로 기억, 시각은 바꾸지 않음
        n.status.interfaces[0] = down(eth("ens1f0", None));
        n.status.known_ports = remember(&n, &[], 20);
        let s = k(&n, "ens1f0").unwrap();
        assert_eq!(
            (s.role, s.addresses.as_slice(), s.last_seen_ms),
            (PortRole::Service, ["10.10.0.11".to_string()].as_slice(), 10)
        );
        // eno1·eno1.100과 eno3이 사라짐 → 이더넷은 missing(사라진 것을 안 때), VLAN은 잊음
        n.status
            .interfaces
            .retain(|i| !matches!(i.name.as_str(), "eno1" | "eno1.100" | "eno3"));
        n.status.known_ports = remember(&n, &[], 30);
        let e = k(&n, "eno1").unwrap();
        assert_eq!((e.missing, e.last_seen_ms), (true, 30));
        assert!(k(&n, "eno1.100").is_none());
        assert!(k(&n, "eno3").unwrap().missing);
        // 다음 회차에도 그대로(시각도), 본딩 상태를 한 번 못 읽어도 남음
        n.status.interfaces[3].bond = None;
        n.status.known_ports = remember(&n, &[], 40);
        assert_eq!(k(&n, "eno1").unwrap().last_seen_ms, 30);
        assert!(k(&n, "eno3").unwrap().missing);
        // 상한 밖에 있는 것은 사라진 것이 아님
        let before = k(&n, "eno1").unwrap();
        n.status.known_ports.iter_mut().for_each(|k| {
            if k.name == "eno1" {
                k.missing = false
            }
        });
        n.status.known_ports = remember(&n, &["eno1"], 50);
        assert!(!k(&n, "eno1").unwrap().missing);
        n.status.known_ports.iter_mut().for_each(|k| {
            if k.name == "eno1" {
                *k = before.clone()
            }
        });
        // 새 이름의 NIC로 bond0을 다시 묶음 → bond0의 사라진 eno3을 잊음(eno1은 남음)
        n.status.interfaces.push(eth("ens2f1", Some("bond0")));
        n.status.known_ports = remember(&n, &[], 60);
        assert!(k(&n, "eno3").is_none());
        assert!(k(&n, "eno1").unwrap().missing);
        assert!(k(&n, "ens2f1").is_some());
        // 관리자가 eno2를 본딩에서 뺌(쓰지 않는 포트가 됨) → 잊음
        n.status.interfaces.iter_mut().for_each(|i| {
            if i.name == "eno2" {
                i.master = None
            }
        });
        n.status.known_ports = remember(&n, &[], 70);
        assert!(k(&n, "eno2").is_none());
        assert!(k(&n, "eno9").is_none());
    }

    /// 관리자가 내린 브리지(`ip link set vmbr0 down`)는 업링크가 살아도 끊김이다. 올라와 있는 내부 브리지는 아니다.
    #[test]
    fn an_admin_down_bridge_is_down() {
        let mut vmbr0 = br("vmbr0");
        vmbr0.admin_up = Some(false);
        vmbr0.state = "down".into();
        vmbr0.carrier = Some(false);
        let n = node(
            "a",
            vec![eth("eno1", Some("vmbr0")), vmbr0],
            &[("vmbr0", false)],
        );
        let lan = bridge_net("lan", "vmbr0");
        let vms = [vm("x", &n, VmPhase::Running, &["lan"])];
        let p = build(std::slice::from_ref(&n), std::slice::from_ref(&lan), &vms);
        assert_eq!(p.nodes[0].bridges[0].health, LinkHealth::Down);
        assert_eq!(rail(&p, "lan")[0].1, RailState::Down);
        let i = &p.nodes[0].impact[0];
        assert_eq!(
            (i.port.as_str(), i.bridges.as_slice(), i.vms),
            ("vmbr0", ["vmbr0".to_string()].as_slice(), 1)
        );
        assert_eq!(
            link_faults(&n, LinkRole::Uplink, None, std::slice::from_ref(&lan))[0].port,
            "vmbr0"
        );
        // 네트워크가 실리지 않는 브리지는 내려도 장애가 아니다(계획 75 리뷰 2).
        assert!(link_faults(&n, LinkRole::Uplink, None, &[]).is_empty());
        let p = build(std::slice::from_ref(&n), &[], &[]);
        assert!(p.nodes[0].impact.is_empty());
    }

    /// `role: any`는 이더넷·VLAN과 묶인 포트를 본다. 어디에도 묶이지 않은 가상 장치(`ovs-system`·`br-int`)는 늘 down이라
    /// 보지 않는다.
    #[test]
    fn any_role_skips_unattached_virtual_devices() {
        let n = node(
            "a",
            vec![
                down(iface("ovs-system", InterfaceKind::Other)),
                down(iface("br-int", InterfaceKind::Other)),
                down(eth("eno9", None)),
            ],
            &[],
        );
        let names: Vec<String> = link_faults(&n, LinkRole::Any, None, &[])
            .into_iter()
            .map(|f| f.port)
            .collect();
        assert_eq!(names, ["eno9"]);
        assert!(link_faults(&n, LinkRole::Uplink, None, &[]).is_empty());
    }

    /// 도는 VM의 NIC 네트워크가 이 노드에 없으면 `unreachable`로 센다(멈춘 VM은 세지 않음).
    #[test]
    fn nics_on_networks_missing_from_the_node_are_unreachable() {
        let n = node("a", vec![eth("eno1", None)], &[]);
        let lan = bridge_net("lan", "vmbr0");
        let vms = [
            vm("x", &n, VmPhase::Running, &["lan"]),
            vm("y", &n, VmPhase::Stopped, &["lan"]),
        ];
        let p = build(std::slice::from_ref(&n), std::slice::from_ref(&lan), &vms);
        assert_eq!(
            p.nodes[0].unreachable,
            vec![Unreachable {
                network: lan.id,
                vms: 1,
                vm_ids: vec![vms[0].id],
            }]
        );
    }

    #[test]
    fn globs() {
        for (p, n, ok) in [
            ("eno*", "eno1", true),
            ("eno*", "eno", true),
            ("eno*", "enp1s0", false),
            ("en?1", "eno1", true),
            ("*1", "bond1", true),
            ("*", "x", true),
            ("e*o*1", "eXXoYY1", true),
            ("e*o*1", "eXXoYY2", false),
            ("eno1", "eno1", true),
            ("eno1", "eno10", false),
        ] {
            assert_eq!(glob_match(p, n), ok, "{p} {n}");
        }
        assert!(valid_port_glob("eno*") && valid_port_glob("bond0.100"));
        assert!(!valid_port_glob("") && !valid_port_glob("a b") && !valid_port_glob("a/b"));
    }

    #[test]
    fn serde_names() {
        assert_eq!(
            serde_json::to_value(RailKind::SdnOverlay).unwrap(),
            "sdn-overlay"
        );
        assert_eq!(
            serde_json::to_value(RailKind::SdnProvider).unwrap(),
            "sdn-provider"
        );
        assert_eq!(serde_json::to_value(RailState::Absent).unwrap(), "absent");
        assert_eq!(
            serde_json::to_value(PortRole::Underlay).unwrap(),
            "underlay"
        );
        assert_eq!(
            serde_json::to_value(FaultSeverity::Degraded).unwrap(),
            "degraded"
        );
        assert_eq!(serde_json::to_value(LinkRole::default()).unwrap(), "uplink");
    }
}
