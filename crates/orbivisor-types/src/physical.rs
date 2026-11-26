//! 물리 ↔ 가상 네트워크 관계(계획 75 설계 §3.2): 노드마다 포트 → 본딩 → VLAN → 브리지 → 네트워크 → VM NIC 사슬,
//! 포트·본딩 장애의 영향, 네트워크마다 노드별 레일 상태. `GET /topology/physical`과 링크 경보(`NodeLinkDown`·
//! `BondDegraded`)가 같은 계산을 쓴다. 입력은 노드 상태(`interfaces`·`bridges`·`underlay`·`sdn`)와 네트워크·VM 스펙뿐이다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::capacity::placed_on;
use crate::network::{Network, NetworkSpec, is_managed_ifname};
use crate::node::{
    BondPort, BondState, InterfaceKind, LldpNeighbor, Node, NodeInterface, NodeStatus,
    OPERATOR_DASHBOARD,
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
    /// 지금 장애가 난 포트·본딩마다 영향(포트 순서)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub impact: Vec<Impact>,
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
    /// 이 포트에서 아래로 본 상태: 본딩은 본딩 상태, VLAN은 아래 장치, 브리지는 업링크들
    pub health: LinkHealth,
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
    /// 어디에도 묶이지 않고 주소도 없는 이더넷
    Unused,
    /// 그 밖(관리 주소만 가진 포트, 업링크가 아닌 브리지 등)
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
    /// 위에 있는 브리지(이름 순)
    pub bridges: Vec<String>,
    /// underlay 주소를 가진 인터페이스가 위에 있다(VXLAN·SDN 오버레이가 영향받음).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub underlay: bool,
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
    /// 사슬이 끊김(노드 오프라인 포함)
    Down,
    /// 이 노드에 이 네트워크를 쓸 수단이 없음(브리지·physnet·underlay 없음)
    Absent,
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
    /// 업링크·underlay 포트만([`PortRole::Uplink`]·[`PortRole::Underlay`])
    #[default]
    Uplink,
    /// 모든 이더넷 포트(쓰지 않는 포트 포함)
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

// ---------------------------------------------------------------------------------------------------------------
// 노드 하나의 그래프
// ---------------------------------------------------------------------------------------------------------------

/// 노드 상태에서 읽은 인터페이스 그래프(VLAN 아래 장치는 없으면 이름으로 추정)
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
}

impl<'a> Host<'a> {
    fn new(status: &'a NodeStatus) -> Host<'a> {
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
        Host {
            status,
            ifs,
            by_name,
            parents,
            vlan_ids,
            bridges,
        }
    }

    fn get(&self, name: &str) -> Option<&'a NodeInterface> {
        self.by_name.get(name).map(|n| self.ifs[*n])
    }

    fn parent(&self, name: &str) -> Option<&str> {
        self.by_name
            .get(name)
            .and_then(|n| self.parents[*n].as_deref())
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

    /// 이 인터페이스에서 아래로 본 상태. 없는 인터페이스는 모르므로 `up`으로 본다.
    fn health(&self, name: &str) -> LinkHealth {
        self.health_at(name, 0)
    }

    fn health_at(&self, name: &str, depth: usize) -> LinkHealth {
        let Some(i) = self.get(name) else {
            return LinkHealth::Up;
        };
        if depth >= MAX_DEPTH {
            return LinkHealth::Up;
        }
        let below = || -> Vec<LinkHealth> {
            self.children(name)
                .into_iter()
                .map(|c| self.health_at(c, depth + 1))
                .collect()
        };
        match i.kind {
            InterfaceKind::Bond => {
                if link_down(i) {
                    return LinkHealth::Down;
                }
                match i.bond.as_ref().map(|b| b.state) {
                    Some(BondState::Up) => LinkHealth::Up,
                    Some(BondState::Degraded) => LinkHealth::Degraded,
                    Some(BondState::Down) => LinkHealth::Down,
                    // 모르는 상태·본딩 정보 없음: 구성 포트로 본다.
                    _ => redundant(&below()),
                }
            }
            // 브리지는 업링크가 없으면(내부 브리지) carrier가 없어도 끊김이 아니다.
            InterfaceKind::Bridge => redundant(&below()),
            _ if link_down(i) => LinkHealth::Down,
            _ => below().into_iter().max().unwrap_or(LinkHealth::Up),
        }
    }

    /// 주소 `ip`를 가진 인터페이스
    fn holder(&self, ip: IpAddr) -> Option<&'a str> {
        self.ifs
            .iter()
            .find(|i| {
                i.addresses
                    .iter()
                    .any(|a| a.split('/').next().and_then(|a| a.parse::<IpAddr>().ok()) == Some(ip))
            })
            .map(|i| i.name.as_str())
    }

    /// underlay 주소들(VXLAN `underlay`, SDN `encap_ip`)을 가진 인터페이스
    fn underlay_holders(&self) -> BTreeSet<&'a str> {
        [self.status.underlay, encap_ip(self.status)]
            .into_iter()
            .flatten()
            .filter_map(|ip| self.holder(ip))
            .collect()
    }

    fn role(
        &self,
        i: &NodeInterface,
        uplinks: &BTreeSet<&str>,
        underlay: &BTreeSet<&str>,
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
        let used = i.master.is_some()
            || !i.addresses.is_empty()
            || self.ifs.iter().any(|o| self.parent(&o.name) == Some(name));
        if matches!(i.kind, InterfaceKind::Ethernet) && !used {
            PortRole::Unused
        } else {
            PortRole::Other
        }
    }

    fn ports(&self) -> Vec<Port> {
        let uplinks: BTreeSet<&str> = self.bridges.keys().flat_map(|b| self.lower(b)).collect();
        let underlay: BTreeSet<&str> = self
            .underlay_holders()
            .into_iter()
            .flat_map(|h| {
                let mut s = self.lower(h);
                s.insert(h);
                s
            })
            .collect();
        self.ifs
            .iter()
            .enumerate()
            .map(|(n, i)| Port {
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
                bond: i.bond.as_ref().map(|b| PortBond {
                    mode: b.mode.clone(),
                    state: b.state,
                    members: b.ports.clone(),
                }),
                role: self.role(i, &uplinks, &underlay),
                health: self.health(&i.name),
            })
            .collect()
    }

    /// 이 인터페이스를 아래에 둔 브리지(이름 순)와 underlay 주인
    fn tops(&self, name: &str) -> (Vec<&str>, Vec<&'a str>) {
        let bridges = self
            .bridges
            .keys()
            .filter(|b| self.lower(b).contains(name))
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

    /// 링크가 끊긴 포트의 무게: 위의 브리지·underlay 가운데 하나라도 끊겼으면 `down`, 다 살았으면 `degraded`. 위에
    /// 아무것도 없으면 본딩(살았으면 `degraded`), 그것도 없으면 `down`이다.
    fn link_severity(&self, i: &NodeInterface) -> FaultSeverity {
        let (bridges, underlay) = self.tops(&i.name);
        let mut tops: Vec<&str> = bridges.into_iter().chain(underlay).collect();
        if tops.is_empty()
            && let Some(b) = self.bond_of(i)
        {
            tops.push(b.name.as_str());
        }
        if tops.is_empty() || tops.iter().any(|t| self.health(t) == LinkHealth::Down) {
            FaultSeverity::Down
        } else {
            FaultSeverity::Degraded
        }
    }

    /// 링크가 끊긴 포트(이더넷·그 밖·VLAN — VLAN은 아래 장치가 살았을 때만)와 그 무게
    fn link_faults(&self, ports: &[Port]) -> Vec<(usize, FaultSeverity)> {
        self.ifs
            .iter()
            .enumerate()
            .filter(|(n, i)| {
                link_down(i)
                    && match ports[*n].kind {
                        PortKind::Ethernet | PortKind::Other => true,
                        PortKind::Vlan => self
                            .parent(&i.name)
                            .is_none_or(|p| self.health(p) != LinkHealth::Down),
                        PortKind::Bond | PortKind::Bridge => false,
                    }
            })
            .map(|(n, i)| (n, self.link_severity(i)))
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

/// 노드 상태 하나의 포트(역할·상태 포함). 네트워크·VM은 보지 않는다.
pub fn ports(status: &NodeStatus) -> Vec<Port> {
    Host::new(status).ports()
}

/// 링크 경보 `NodeLinkDown`: 이 노드 상태에서 지금 링크가 끊긴 포트. `role`이 `uplink`면 업링크·underlay 포트만,
/// `ports`(글롭)가 있으면 그 이름만이다.
pub fn link_faults(status: &NodeStatus, role: LinkRole, ports: Option<&str>) -> Vec<LinkFault> {
    let host = Host::new(status);
    let list = host.ports();
    host.link_faults(&list)
        .into_iter()
        .map(|(n, severity)| (&list[n], severity))
        .filter(|(p, _)| {
            role == LinkRole::Any || matches!(p.role, PortRole::Uplink | PortRole::Underlay)
        })
        .filter(|(p, _)| ports.is_none_or(|g| glob_match(g, &p.name)))
        .map(|(p, severity)| LinkFault {
            port: p.name.clone(),
            severity,
            bond: host
                .bond_of(host.ifs[host.by_name[p.name.as_str()]])
                .map(|b| b.name.clone()),
            role: p.role,
        })
        .collect()
}

/// 링크 경보 `BondDegraded`: 이 노드 상태에서 지금 저하·끊긴 본딩
pub fn bond_faults(status: &NodeStatus) -> Vec<LinkFault> {
    let host = Host::new(status);
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
        let host = Host::new(&node.status);
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
                        .collect(),
                    vlan_filtering: *filtering,
                    networks: nets.iter().map(|i| networks[*i].id).collect(),
                    vm_nics: nics_on(&nets),
                    health: host.health(name),
                }
            })
            .collect();

        // 영향: 끊긴 포트, 그리고 구성 포트에 끊긴 것이 없는 저하·끊긴 본딩
        let mut faults = host.link_faults(&ports);
        let linked: BTreeSet<&str> = faults
            .iter()
            .filter_map(|(n, _)| host.bond_of(host.ifs[*n]).map(|b| b.name.as_str()))
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
            if !matches!(p.role, PortRole::Uplink | PortRole::Underlay) {
                continue;
            }
            let (above, holders) = host.tops(&p.name);
            let underlay_ips: Vec<IpAddr> = [node.status.underlay, encap_ip(&node.status)]
                .into_iter()
                .flatten()
                .filter(|ip| host.holder(*ip).is_some_and(|h| holders.contains(&h)))
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
                bond: host.bond_of(host.ifs[n]).map(|b| b.name.clone()),
                bridges: above.iter().map(|b| b.to_string()).collect(),
                underlay: !holders.is_empty(),
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
                port: host.holder(ip).map(str::to_string),
            });

        if !node.spec.witness {
            for (i, rail) in rails.iter_mut().enumerate() {
                let state = if !node.status.online {
                    RailState::Down
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
                                host.holder(*ip)
                                    .map_or(LinkHealth::Up, |h| host.health(h))
                                    .into()
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
        });
    }
    out.networks = rails;
    out
}

/// 경보 문구(설계 §3.4): `eno1 down on svc01: bond0 degraded — vmbr0, 3 networks, 12 VMs`
pub fn describe(node: &str, impact: &Impact) -> String {
    let mut head = match &impact.bond {
        Some(bond) => format!(
            "{} down on {node}: {bond} {}",
            impact.port,
            impact.severity.as_str()
        ),
        None => format!("{} {} on {node}", impact.port, impact.severity.as_str()),
    };
    let mut parts: Vec<String> = impact.bridges.clone();
    if impact.underlay {
        parts.push("underlay".into());
    }
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
    head
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
                bridges: vec!["vmbr0".into()],
                underlay: false,
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
        let faults = link_faults(&n.status, LinkRole::Uplink, None);
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
        assert!(link_faults(&n.status, LinkRole::Uplink, None).is_empty());
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
            bond_faults(&n.status),
            vec![LinkFault {
                port: "bond0".into(),
                severity: FaultSeverity::Degraded,
                bond: None,
                role: PortRole::Uplink,
            }]
        );
        assert_eq!(
            link_faults(&n.status, LinkRole::Uplink, None)[0]
                .bond
                .as_deref(),
            Some("bond0")
        );

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
        assert_eq!(bond_faults(&n.status)[0].severity, FaultSeverity::Down);
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
        assert!(link_faults(&n.status, LinkRole::Any, None).is_empty());
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

    /// 브리지 없음: 네트워크의 브리지가 없는 노드는 absent, 영향 없음. 오프라인 노드는 down, 위트니스는 레일에 없다.
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
                ("c".to_string(), RailState::Down),
            ]
        );
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
            link_faults(&a.status, role, None)
                .into_iter()
                .map(|f| f.port)
                .collect()
        };
        assert_eq!(names(LinkRole::Uplink), ["eno4"]);
        assert_eq!(names(LinkRole::Any), ["eno4", "eno1"]);
        assert!(
            link_faults(&a.status, LinkRole::Any, Some("eno1"))
                .iter()
                .all(|f| f.port == "eno1")
        );
        assert!(link_faults(&a.status, LinkRole::Any, Some("eth*")).is_empty());
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
