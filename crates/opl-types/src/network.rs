//! VM NIC가 연결하는 네트워크: 관리자가 미리 만든 Linux 브리지, opld가 만드는 VXLAN 네트워크, OVN 논리 스위치로
//! 만드는 SDN 네트워크(계획 65).

use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ipam;
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::sdn::DEFAULT_SDN_MTU;
use crate::security_group::Cidr;

/// 예약된 네트워크 이름. 리소스가 없고 QEMU user-mode NAT로 연결한다(macOS 개발·데모용).
pub const USER_NETWORK: &str = "user";

/// VXLAN 네트워크의 기본 MTU: underlay 1500에서 VXLAN 머리 50바이트를 뺀 값
pub const DEFAULT_VXLAN_MTU: u16 = 1450;

/// VXLAN UDP 포트(IANA)
pub const VXLAN_PORT: u16 = 4789;

/// SDN NIC의 tap이 붙는 OVS 통합 브리지(ovn-controller가 만든다)
pub const OVN_BRIDGE: &str = "br-int";

/// 서브넷 하나의 DNS 서버 수 상한
pub const MAX_DNS_SERVERS: usize = 4;

/// 서브넷 하나의 할당 풀 수 상한
pub const MAX_POOLS: usize = 16;

/// 서브넷 설명의 길이 상한(글자)
pub const MAX_SUBNET_DESCRIPTION: usize = 200;

pub type Network = Resource<NetworkSpec, NetworkStatus>;

/// `bridge`·`vxlan`·`sdn` 중 정확히 하나를 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bridge: Option<String>,
    /// VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan: Option<u16>,
    /// orbivisord가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vxlan: Option<Vxlan>,
    /// OVN 논리 스위치(Geneve)로 만드는 SDN 네트워크(계획 65). 없으면 쓰지 않아 옛 노드도 네트워크를 푼다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub sdn: Option<SdnNet>,
    /// VXLAN·SDN 네트워크의 MTU(1280~9000, 기본 1450·1442). `vxlan`·`sdn`과만 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub mtu: Option<u16>,
    /// SDN 네트워크의 서브넷(주소 체계마다 하나까지, 계획 65). `sdn`과만 쓰고 비면 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub subnets: Vec<Subnet>,
    /// 든 네트워크 폴더(계획 62, `Folder` id). 없으면 네트워크 트리의 루트이고 쓰지 않아 옛 노드도 네트워크를 푼다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub folder: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Vxlan {
    /// VXLAN 네트워크 식별자(1~16777215). 클러스터 안에서 겹치지 않는다.
    pub vni: u32,
}

/// SDN(OVN) 네트워크. 칸이 없으면 오버레이(Geneve) 네트워크다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SdnNet {
    /// 공급자(외부) 네트워크(계획 66): 노드의 물리 망(physnet)에 붙는 OVN `localnet` 네트워크. 없으면 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub provider: Option<Provider>,
}

/// 공급자 네트워크가 붙는 물리 망(계획 66 §8)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Provider {
    /// 물리 망 이름(`[a-z][a-z0-9-]{0,30}`). 노드마다 `NodeSpec.sdn.physnets`가 이 이름을 그 노드의 Linux 브리지에 잇는다.
    pub physnet: String,
    /// VLAN 태그(1~4094). 없으면 flat(태그 없는 프레임)이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vlan: Option<u16>,
}

/// 공급자 네트워크의 기본 MTU(물리 망)
pub const DEFAULT_PROVIDER_MTU: u16 = 1500;

/// 서브넷 하나의 유동 IP 풀 수 상한
pub const MAX_FLOATING_POOLS: usize = 16;

/// IPv6 서브넷의 주소 알리기(계획 66 §10). 라우터 인터페이스인 네트워크에서는 라우터 포트가 RA로 알린다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Ipv6Mode {
    /// RA의 접두사로 게스트가 주소를 만든다(IPAM은 EUI-64, `/64`만).
    Slaac,
    /// DHCPv6가 주소를 준다(IPAM은 풀에서).
    Dhcpv6Stateful,
    /// 주소는 SLAAC(EUI-64, `/64`만), 나머지(DNS)는 DHCPv6
    Dhcpv6Stateless,
}

impl Ipv6Mode {
    /// 주소를 게스트가 MAC에서 만든다(EUI-64).
    pub fn is_slaac(self) -> bool {
        matches!(self, Ipv6Mode::Slaac | Ipv6Mode::Dhcpv6Stateless)
    }
}

/// SDN 네트워크의 서브넷: 주소를 Orbivisor IPAM(`NicSpec.ips`)이 정하고 OVN 내장 DHCP가 준다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Subnet {
    /// 범위(IPv4 `/8`~`/29`, IPv6 `/48`~`/124`). 접두사 밖 비트가 켜져 있으면 받지 않는다.
    #[cfg_attr(feature = "openapi", schema(value_type = String, example = "10.10.0.0/24"))]
    pub cidr: Cidr,
    /// 게이트웨이 주소. 없으면 첫 호스트 주소다. 라우터가 없어도 늘 예약해 VM에 주지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ip", nullable = false, default)
    )]
    pub gateway: Option<IpAddr>,
    /// OVN DHCP로 주소를 준다. 없으면 IPv4는 참, IPv6는 거짓(IPv6 DHCP는 아직 없어 거짓만 받는다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub dhcp: Option<bool>,
    /// DHCP·cloud-init이 알려 줄 DNS 서버(4개까지)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>, default))]
    pub dns_servers: Vec<IpAddr>,
    /// 자동 할당 범위(16개까지, 겹치지 않게). 없으면 CIDR 전체에서 예약을 뺀 것이다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub pools: Vec<IpRange>,
    /// 라우터 외부 주소와 유동 IP를 고르는 범위(계획 66, 공급자 네트워크의 IPv4 서브넷만, 16개까지, `pools`·게이트웨이와
    /// 겹치지 않게). 없으면 `pools`에서 고른다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub floating_pools: Vec<IpRange>,
    /// IPv6 주소 알리기(계획 66, IPv6 서브넷만). 없으면 cloud-init 고정 주소다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ipv6_mode: Option<Ipv6Mode>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub description: String,
}

/// 주소 범위(양 끝 포함)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct IpRange {
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub start: IpAddr,
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub end: IpAddr,
}

impl fmt::Display for IpRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

impl Subnet {
    pub fn is_ipv4(&self) -> bool {
        self.cidr.addr.is_ipv4()
    }

    /// 게이트웨이(주지 않았으면 첫 호스트 주소)
    pub fn gateway(&self) -> IpAddr {
        self.gateway.unwrap_or_else(|| ipam::first_host(&self.cidr))
    }

    /// OVN DHCP를 쓰는지(없으면 IPv4만 참)
    pub fn dhcp(&self) -> bool {
        self.dhcp.unwrap_or(self.is_ipv4())
    }

    pub fn validate(&self) -> Result<(), SubnetError> {
        let prefix = self.cidr.prefix;
        if self.is_ipv4() && !(8..=29).contains(&prefix) {
            return Err(SubnetError::PrefixV4(prefix));
        }
        if !self.is_ipv4() && !(48..=124).contains(&prefix) {
            return Err(SubnetError::PrefixV6(prefix));
        }
        let edges = ipam::edges(&self.cidr);
        if let Some(gw) = self.gateway {
            if !self.cidr.contains(gw) || gw.is_ipv4() != self.is_ipv4() {
                return Err(SubnetError::GatewayOutside(gw));
            }
            if edges.contains(&gw.to_canonical()) {
                return Err(SubnetError::GatewayReserved(gw));
            }
        }
        if !self.is_ipv4() && self.dhcp == Some(true) {
            return Err(SubnetError::Dhcpv6);
        }
        if self.dns_servers.len() > MAX_DNS_SERVERS {
            return Err(SubnetError::TooManyDns);
        }
        if self.pools.len() > MAX_POOLS {
            return Err(SubnetError::TooManyPools);
        }
        let bits = |ip: IpAddr| crate::security_group::Cidr::bits(ip);
        for pool in &self.pools {
            self.check_range(pool)?;
        }
        let mut pools = self.pools.clone();
        pools.sort_by_key(|p| bits(p.start));
        for pair in pools.windows(2) {
            if bits(pair[1].start) <= bits(pair[0].end) {
                return Err(SubnetError::PoolsOverlap(pair[0], pair[1]));
            }
        }
        if !self.floating_pools.is_empty() && !self.is_ipv4() {
            return Err(SubnetError::FloatingPoolsOnIpv6);
        }
        if self.floating_pools.len() > MAX_FLOATING_POOLS {
            return Err(SubnetError::TooManyFloatingPools);
        }
        for pool in &self.floating_pools {
            self.check_range(pool)?;
        }
        // 유동 IP 풀끼리, 유동 IP 풀과 할당 풀은 겹치지 않는다(같은 주소를 두 쓰임이 고르지 않게).
        for (i, a) in self.floating_pools.iter().enumerate() {
            let others = self.floating_pools[i + 1..].iter().chain(&self.pools);
            for b in others {
                if bits(a.start) <= bits(b.end) && bits(b.start) <= bits(a.end) {
                    return Err(SubnetError::FloatingPoolOverlap(*a, *b));
                }
            }
        }
        if let Some(mode) = self.ipv6_mode {
            if self.is_ipv4() {
                return Err(SubnetError::Ipv6ModeOnIpv4);
            }
            if mode.is_slaac() && prefix != 64 {
                return Err(SubnetError::SlaacNeeds64(prefix));
            }
        }
        if self.description.chars().count() > MAX_SUBNET_DESCRIPTION {
            return Err(SubnetError::LongDescription);
        }
        Ok(())
    }

    /// 할당 범위 하나(풀·유동 IP 풀): 서브넷 안 같은 체계, 시작 ≤ 끝, 망 주소·브로드캐스트와 게이트웨이를 품지 않음
    fn check_range(&self, pool: &IpRange) -> Result<(), SubnetError> {
        let bits = |ip: IpAddr| crate::security_group::Cidr::bits(ip);
        let gateway = self.gateway();
        let (first, last) = ipam::span(&self.cidr);
        let inside = |ip: IpAddr| ip.is_ipv4() == self.is_ipv4() && self.cidr.contains(ip);
        if !inside(pool.start) || !inside(pool.end) {
            return Err(SubnetError::PoolOutside(*pool));
        }
        let (start, end) = (bits(pool.start), bits(pool.end));
        if start > end {
            return Err(SubnetError::PoolReversed(*pool));
        }
        if start == first || (self.is_ipv4() && end == last) {
            return Err(SubnetError::PoolReserved(*pool));
        }
        if (start..=end).contains(&bits(gateway)) {
            return Err(SubnetError::GatewayInPool(*pool, gateway));
        }
        Ok(())
    }
}

impl ResourceSpec for NetworkSpec {
    const KIND: Kind = Kind::Network;
    type Status = NetworkStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NetworkStatus {}

impl NetworkSpec {
    pub fn validate(&self) -> Result<(), NetworkError> {
        match (&self.bridge, &self.vxlan, &self.sdn) {
            (Some(bridge), None, None) => {
                if !valid_ifname(bridge) {
                    return Err(NetworkError::BadBridge(bridge.clone()));
                }
                if is_managed_ifname(bridge) {
                    return Err(NetworkError::ReservedBridge(bridge.clone()));
                }
                if bridge == OVN_BRIDGE {
                    return Err(NetworkError::OvnBridge);
                }
                if self.mtu.is_some() {
                    return Err(NetworkError::MtuWithBridge);
                }
                if let Some(vlan) = self.vlan
                    && !(1..=4094).contains(&vlan)
                {
                    return Err(NetworkError::BadVlan(vlan));
                }
            }
            (None, Some(vxlan), None) => {
                if self.vlan.is_some() {
                    return Err(NetworkError::VlanWithVxlan);
                }
                if !(1..=16_777_215).contains(&vxlan.vni) {
                    return Err(NetworkError::BadVni(vxlan.vni));
                }
                if let Some(mtu) = self.mtu
                    && !(1280..=9000).contains(&mtu)
                {
                    return Err(NetworkError::BadMtu(mtu));
                }
            }
            (None, None, Some(sdn)) => {
                if self.vlan.is_some() {
                    return Err(NetworkError::VlanWithSdn);
                }
                if let Some(provider) = &sdn.provider {
                    provider.validate()?;
                }
                if let Some(mtu) = self.mtu
                    && !(1280..=9000).contains(&mtu)
                {
                    return Err(NetworkError::BadMtu(mtu));
                }
            }
            _ => return Err(NetworkError::KindRequired),
        }
        if self.sdn.is_none() && !self.subnets.is_empty() {
            return Err(NetworkError::SubnetsWithoutSdn);
        }
        let mut families = BTreeSet::new();
        for (i, subnet) in self.subnets.iter().enumerate() {
            subnet
                .validate()
                .map_err(|e| NetworkError::Subnet(i + 1, e))?;
            if !subnet.floating_pools.is_empty() && !self.is_provider() {
                return Err(NetworkError::Subnet(
                    i + 1,
                    SubnetError::FloatingPoolsWithoutProvider,
                ));
            }
            if !families.insert(subnet.is_ipv4()) {
                return Err(NetworkError::DuplicateFamily);
            }
        }
        Ok(())
    }

    /// SDN(OVN) 네트워크인지
    pub fn is_sdn(&self) -> bool {
        self.sdn.is_some()
    }

    /// 공급자(외부) SDN 네트워크인지(계획 66)
    pub fn is_provider(&self) -> bool {
        self.provider().is_some()
    }

    /// 공급자 네트워크의 물리 망
    pub fn provider(&self) -> Option<&Provider> {
        self.sdn.as_ref().and_then(|s| s.provider.as_ref())
    }

    /// 공급자 네트워크의 physnet 이름
    pub fn physnet(&self) -> Option<&str> {
        self.provider().map(|p| p.physnet.as_str())
    }

    /// 서브넷 `subnet`이 OVN DHCP를 쓰는지. 칸이 없으면 오버레이의 IPv4 서브넷만 참이다(공급자 네트워크는 물리 망에
    /// DHCP 서버가 있을 수 있어 기본 거짓 — 계획 66 §8). 계획 66부터 [`Subnet::dhcp`] 대신 이것을 쓴다.
    pub fn subnet_dhcp(&self, subnet: &Subnet) -> bool {
        subnet
            .dhcp
            .unwrap_or(subnet.is_ipv4() && !self.is_provider())
    }

    /// VM NIC가 붙을 브리지 이름: `bridge`, `oplv<vni>`, SDN이면 OVS `br-int`. 검증을 통과한 스펙에서만 뜻이 있다.
    pub fn bridge_name(&self) -> String {
        match (&self.bridge, self.vni()) {
            (Some(bridge), _) => bridge.clone(),
            (None, Some(vni)) => vxlan_bridge(vni),
            (None, None) if self.is_sdn() => OVN_BRIDGE.to_string(),
            (None, None) => String::new(),
        }
    }

    /// VXLAN·SDN 네트워크의 MTU(없으면 기본값 — 공급자 네트워크는 물리 망의 1500). 브리지 네트워크는 None.
    pub fn mtu(&self) -> Option<u16> {
        if self.is_provider() {
            return Some(self.mtu.unwrap_or(DEFAULT_PROVIDER_MTU));
        }
        if self.is_sdn() {
            return Some(self.mtu.unwrap_or(DEFAULT_SDN_MTU));
        }
        self.vxlan.map(|_| self.mtu.unwrap_or(DEFAULT_VXLAN_MTU))
    }

    pub fn vni(&self) -> Option<u32> {
        self.vxlan.map(|v| v.vni)
    }
}

/// VXLAN 네트워크의 브리지 이름(`oplv<vni>`, 최대 12자)
pub fn vxlan_bridge(vni: u32) -> String {
    format!("oplv{vni}")
}

/// VXLAN 네트워크의 VXLAN 장치 이름(`oplx<vni>`, 최대 12자)
pub fn vxlan_device(vni: u32) -> String {
    format!("oplx{vni}")
}

/// opld가 관리하는 장치 이름 모양(`^opl[vx][0-9]+$`, 계획 66의 physnet 장치 `^oplp[0-9a-f]{8}[obx]$`)인지
pub fn is_managed_ifname(name: &str) -> bool {
    if crate::sdn::is_physnet_ifname(name) {
        return true;
    }
    let Some(rest) = name
        .strip_prefix("oplv")
        .or_else(|| name.strip_prefix("oplx"))
    else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_digit())
}

/// VM의 `index`번째 NIC가 쓰는 tap 장치 이름. 노드 안에서 VM마다 겹치지 않고 15자를 넘지 않는다.
/// UUIDv7의 앞부분은 생성 시각이므로 무작위인 뒷부분 10자리를 쓴다.
pub fn tap_name(vm: Uuid, index: usize) -> String {
    format!("{}{index}", tap_prefix(vm))
}

/// 한 VM의 tap 장치 이름이 모두 이것으로 시작한다(`op<10자리>n`).
pub fn tap_prefix(vm: Uuid) -> String {
    let hex = vm.simple().to_string();
    format!("op{}n", &hex[hex.len() - 10..])
}

/// Linux 인터페이스 이름 규칙: 1~15바이트, `/`·공백·`:` 없음, `.`·`..`가 아님.
pub fn valid_ifname(name: &str) -> bool {
    (1..=15).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|c| c.is_ascii_graphic() && c != b'/' && c != b':')
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NetworkError {
    #[error("invalid bridge name {0:?}: use 1-15 characters without '/', ':' or spaces")]
    BadBridge(String),
    #[error("invalid VLAN tag {0}: use 1-4094")]
    BadVlan(u16),
    #[error("set exactly one of bridge, vxlan or sdn")]
    KindRequired,
    #[error("vlan can only be used with bridge, not with vxlan")]
    VlanWithVxlan,
    #[error("vlan can only be used with bridge, not with sdn")]
    VlanWithSdn,
    #[error("mtu can only be used with vxlan or sdn; a bridge network uses the bridge's MTU")]
    MtuWithBridge,
    #[error("invalid VNI {0}: use 1-16777215")]
    BadVni(u32),
    #[error("invalid MTU {0}: use 1280-9000")]
    BadMtu(u16),
    #[error("bridge name {0:?} is reserved for Orbivisor VXLAN networks")]
    ReservedBridge(String),
    #[error(
        "bridge name {OVN_BRIDGE:?} is the OVN integration bridge; create an SDN network (sdn) instead"
    )]
    OvnBridge,
    #[error("subnets can only be used with sdn")]
    SubnetsWithoutSdn,
    #[error("a network can have at most one IPv4 and one IPv6 subnet")]
    DuplicateFamily,
    #[error("subnet {0}: {1}")]
    Subnet(usize, SubnetError),
    #[error(
        "invalid physnet {0:?}: use 1-31 lowercase letters, digits and '-', starting with a letter"
    )]
    BadPhysnet(String),
}

/// 서브넷 하나의 문제. `NetworkError::Subnet`이 서브넷 번호(1부터)를 붙인다.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SubnetError {
    #[error("an IPv4 subnet must be /8 to /29, not /{0}")]
    PrefixV4(u8),
    #[error("an IPv6 subnet must be /48 to /124, not /{0}")]
    PrefixV6(u8),
    #[error("gateway {0} is not in the subnet")]
    GatewayOutside(IpAddr),
    #[error("gateway {0} is the subnet's network or broadcast address")]
    GatewayReserved(IpAddr),
    #[error(
        "dhcp is not available for IPv6 subnets yet; set dhcp to false (cloud-init sets the address)"
    )]
    Dhcpv6,
    #[error("a subnet can have at most {MAX_DNS_SERVERS} dns_servers")]
    TooManyDns,
    #[error("a subnet can have at most {MAX_POOLS} pools")]
    TooManyPools,
    #[error("pool {0} is not in the subnet")]
    PoolOutside(IpRange),
    #[error("pool {0} starts after it ends")]
    PoolReversed(IpRange),
    #[error("pool {0} includes the subnet's network or broadcast address")]
    PoolReserved(IpRange),
    #[error("pool {0} includes the gateway {1}")]
    GatewayInPool(IpRange, IpAddr),
    #[error("pools {0} and {1} overlap")]
    PoolsOverlap(IpRange, IpRange),
    #[error("description must be at most {MAX_SUBNET_DESCRIPTION} characters")]
    LongDescription,
    #[error("floating_pools can only be used on a provider network (sdn.provider)")]
    FloatingPoolsWithoutProvider,
    #[error("floating_pools can only be used on an IPv4 subnet")]
    FloatingPoolsOnIpv6,
    #[error("a subnet can have at most {MAX_FLOATING_POOLS} floating_pools")]
    TooManyFloatingPools,
    #[error("floating pool {0} overlaps {1}")]
    FloatingPoolOverlap(IpRange, IpRange),
    #[error("ipv6_mode can only be used on an IPv6 subnet")]
    Ipv6ModeOnIpv4,
    #[error("ipv6_mode slaac and dhcpv6-stateless need a /64 subnet (EUI-64), not /{0}")]
    SlaacNeeds64(u8),
}

impl Provider {
    pub fn validate(&self) -> Result<(), NetworkError> {
        if !crate::sdn::valid_physnet(&self.physnet) {
            return Err(NetworkError::BadPhysnet(self.physnet.clone()));
        }
        if let Some(vlan) = self.vlan
            && !(1..=4094).contains(&vlan)
        {
            return Err(NetworkError::BadVlan(vlan));
        }
        Ok(())
    }

    /// 클러스터에서 하나뿐이어야 하는 열쇠 `<physnet>/<vlan|flat>`(Raft 겹침 `physnet`)
    pub fn key(&self) -> String {
        match self.vlan {
            Some(vlan) => format!("{}/{vlan}", self.physnet),
            None => format!("{}/flat", self.physnet),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(bridge: &str, vlan: Option<u16>) -> NetworkSpec {
        NetworkSpec {
            bridge: Some(bridge.into()),
            vlan,
            vxlan: None,
            sdn: None,
            mtu: None,
            subnets: Vec::new(),
            folder: None,
        }
    }

    fn vxlan(vni: u32, mtu: Option<u16>) -> NetworkSpec {
        NetworkSpec {
            bridge: None,
            vlan: None,
            vxlan: Some(Vxlan { vni }),
            sdn: None,
            mtu,
            subnets: Vec::new(),
            folder: None,
        }
    }

    #[test]
    fn validates_bridge_and_vlan() {
        assert!(spec("vmbr0", None).validate().is_ok());
        assert!(spec("br-lan.10", Some(4094)).validate().is_ok());
        for bad in ["", "a/b", "a b", "a:b", ".", "..", "abcdefghijklmnop"] {
            assert_eq!(
                spec(bad, None).validate(),
                Err(NetworkError::BadBridge(bad.into()))
            );
        }
        assert_eq!(
            spec("vmbr0", Some(0)).validate(),
            Err(NetworkError::BadVlan(0))
        );
        assert_eq!(
            spec("vmbr0", Some(4095)).validate(),
            Err(NetworkError::BadVlan(4095))
        );
    }

    #[test]
    fn tap_names_fit_and_share_the_vm_prefix() {
        let vm = Uuid::now_v7();
        let last = tap_name(vm, crate::vm::MAX_NICS - 1);
        assert!(valid_ifname(&last), "{last}");
        assert_eq!(last.len(), 15);
        assert!(tap_name(vm, 0).starts_with(&tap_prefix(vm)));
        assert_ne!(tap_prefix(vm), tap_prefix(Uuid::now_v7()));
        // 같은 VM의 1번과 10번 NIC가 접두사로 겹치지 않는다(`...n1`과 `...n10`은 다른 이름).
        assert_ne!(tap_name(vm, 1), tap_name(vm, 10));
    }

    #[test]
    fn vlan_is_optional() {
        let s: NetworkSpec =
            serde_json::from_value(serde_json::json!({ "bridge": "vmbr0" })).unwrap();
        assert_eq!(s, spec("vmbr0", None));
    }

    #[test]
    fn old_bridge_networks_read_and_write_the_same_json() {
        for old in [
            serde_json::json!({ "bridge": "vmbr0" }),
            serde_json::json!({ "bridge": "vmbr0", "vlan": 10 }),
        ] {
            let s: NetworkSpec = serde_json::from_value(old.clone()).unwrap();
            assert_eq!(serde_json::to_value(&s).unwrap(), old);
        }
        // 지금까지 쓰던 `"vlan": null`도 읽힌다.
        let s: NetworkSpec =
            serde_json::from_value(serde_json::json!({ "bridge": "vmbr0", "vlan": null })).unwrap();
        assert_eq!(s, spec("vmbr0", None));
    }

    /// 계획 62: 폴더가 없으면 옛 모양 그대로, 있으면 왕복한다.
    #[test]
    fn folder_is_written_only_when_set() {
        let old = serde_json::json!({ "vxlan": { "vni": 1000 }, "mtu": 1400 });
        let s: NetworkSpec = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(s.folder, None);
        assert_eq!(serde_json::to_value(&s).unwrap(), old);
        let folder = Uuid::from_u128(3);
        let s = NetworkSpec {
            folder: Some(folder),
            ..s
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["folder"], folder.to_string());
        assert_eq!(serde_json::from_value::<NetworkSpec>(json).unwrap(), s);
    }

    #[test]
    fn vxlan_networks_round_trip() {
        let json = serde_json::json!({ "vxlan": { "vni": 1000 }, "mtu": 1400 });
        let s: NetworkSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(s, vxlan(1000, Some(1400)));
        assert_eq!(serde_json::to_value(&s).unwrap(), json);
        assert!(
            serde_json::from_value::<NetworkSpec>(
                serde_json::json!({ "vxlan": { "vni": 1, "group": "x" } })
            )
            .is_err()
        );
    }

    #[test]
    fn validates_the_kind_and_its_fields() {
        let both = NetworkSpec {
            bridge: Some("vmbr0".into()),
            ..vxlan(10, None)
        };
        assert_eq!(both.validate(), Err(NetworkError::KindRequired));
        let neither = NetworkSpec {
            vxlan: None,
            ..vxlan(10, None)
        };
        assert_eq!(neither.validate(), Err(NetworkError::KindRequired));
        let tagged = NetworkSpec {
            vlan: Some(10),
            ..vxlan(10, None)
        };
        assert_eq!(tagged.validate(), Err(NetworkError::VlanWithVxlan));
        let sized = NetworkSpec {
            mtu: Some(1450),
            ..spec("vmbr0", None)
        };
        assert_eq!(sized.validate(), Err(NetworkError::MtuWithBridge));

        for ok in [1, 16_777_215] {
            assert_eq!(vxlan(ok, None).validate(), Ok(()));
        }
        for bad in [0, 16_777_216] {
            assert_eq!(vxlan(bad, None).validate(), Err(NetworkError::BadVni(bad)));
        }
        for ok in [1280, 9000] {
            assert_eq!(vxlan(10, Some(ok)).validate(), Ok(()));
        }
        for bad in [1279, 9001] {
            assert_eq!(
                vxlan(10, Some(bad)).validate(),
                Err(NetworkError::BadMtu(bad))
            );
        }
        for reserved in ["oplv10", "oplx7"] {
            assert_eq!(
                spec(reserved, None).validate(),
                Err(NetworkError::ReservedBridge(reserved.into()))
            );
        }
        assert_eq!(spec("oplv", None).validate(), Ok(()));
        // SDN 네트워크의 OVS 브리지는 브리지 네트워크가 가리킬 수 없다(계획 65).
        assert_eq!(
            spec("br-int", None).validate(),
            Err(NetworkError::OvnBridge)
        );
        assert_eq!(spec("br-int0", None).validate(), Ok(()));
    }

    #[test]
    fn names_the_bridge_and_mtu_of_each_kind() {
        let lan = spec("vmbr0", Some(10));
        assert_eq!(lan.bridge_name(), "vmbr0");
        assert_eq!(lan.mtu(), None);
        assert_eq!(lan.vni(), None);
        let overlay = vxlan(16_777_215, None);
        assert_eq!(overlay.bridge_name(), "oplv16777215");
        assert_eq!(overlay.mtu(), Some(DEFAULT_VXLAN_MTU));
        assert_eq!(overlay.vni(), Some(16_777_215));
        assert_eq!(vxlan(7, Some(9000)).mtu(), Some(9000));
        // 가장 큰 VNI로도 인터페이스 이름 규칙에 맞는다.
        assert!(valid_ifname(&vxlan_device(16_777_215)));
        assert_eq!(vxlan_device(7), "oplx7");
    }

    #[test]
    fn recognizes_managed_interface_names() {
        for yes in ["oplv1", "oplx16777215", "oplv007"] {
            assert!(is_managed_ifname(yes), "{yes}");
        }
        for no in [
            "oplv",
            "oplx",
            "opl1",
            "oplz1",
            "oplv1a",
            "xoplv1",
            "vmbr0",
            "op0123456789n0",
        ] {
            assert!(!is_managed_ifname(no), "{no}");
        }
        assert!(is_managed_ifname(&vxlan_bridge(42)));
        // tap 이름과 겹치지 않는다.
        assert!(!is_managed_ifname(&tap_name(Uuid::now_v7(), 0)));
    }

    fn sdn(subnets: Vec<Subnet>) -> NetworkSpec {
        NetworkSpec {
            bridge: None,
            vlan: None,
            vxlan: None,
            sdn: Some(SdnNet::default()),
            mtu: None,
            subnets,
            folder: None,
        }
    }

    fn subnet(v: serde_json::Value) -> Subnet {
        serde_json::from_value(v).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn range(a: &str, b: &str) -> IpRange {
        IpRange {
            start: ip(a),
            end: ip(b),
        }
    }

    /// 계획 65: 새 칸이 없으면 옛 모양 그대로, SDN 네트워크와 서브넷은 왕복한다.
    #[test]
    fn sdn_fields_are_written_only_when_set() {
        for old in [
            r#"{"bridge":"vmbr0","vlan":10}"#,
            r#"{"vxlan":{"vni":1000},"mtu":1400,"folder":"00000000-0000-0000-0000-000000000003"}"#,
        ] {
            let s: NetworkSpec = serde_json::from_str(old).unwrap();
            assert_eq!((s.sdn.clone(), s.subnets.len()), (None, 0));
            assert_eq!(serde_json::to_string(&s).unwrap(), old);
        }
        let json = serde_json::json!({
            "sdn": {},
            "mtu": 1400,
            "subnets": [
                {
                    "cidr": "10.10.0.0/24",
                    "gateway": "10.10.0.254",
                    "dhcp": false,
                    "dns_servers": ["1.1.1.1"],
                    "pools": [{ "start": "10.10.0.10", "end": "10.10.0.99" }],
                    "description": "web"
                },
                { "cidr": "2001:db8::/64" }
            ]
        });
        let s: NetworkSpec = serde_json::from_value(json.clone()).unwrap();
        assert!(s.is_sdn());
        assert_eq!(s.subnets[0].pools, [range("10.10.0.10", "10.10.0.99")]);
        assert_eq!(serde_json::to_value(&s).unwrap(), json);
        assert_eq!(s.validate(), Ok(()));
        for bad in [
            serde_json::json!({ "sdn": { "provider": "physnet1" } }),
            serde_json::json!({ "sdn": {}, "subnets": [{ "cidr": "10.0.0.0/24", "x": 1 }] }),
            serde_json::json!({ "sdn": {}, "subnets": [{ "cidr": "10.0.0.1/24" }] }),
        ] {
            assert!(
                serde_json::from_value::<NetworkSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn validates_the_three_kinds() {
        assert_eq!(sdn(Vec::new()).validate(), Ok(()));
        let two = NetworkSpec {
            vxlan: Some(Vxlan { vni: 5 }),
            ..sdn(Vec::new())
        };
        assert_eq!(two.validate(), Err(NetworkError::KindRequired));
        let two = NetworkSpec {
            bridge: Some("vmbr0".into()),
            ..sdn(Vec::new())
        };
        assert_eq!(two.validate(), Err(NetworkError::KindRequired));
        assert_eq!(
            NetworkError::KindRequired.to_string(),
            "set exactly one of bridge, vxlan or sdn"
        );
        let tagged = NetworkSpec {
            vlan: Some(10),
            ..sdn(Vec::new())
        };
        assert_eq!(tagged.validate(), Err(NetworkError::VlanWithSdn));
        for (mtu, ok) in [(1279, false), (1280, true), (9000, true), (9001, false)] {
            let s = NetworkSpec {
                mtu: Some(mtu),
                ..sdn(Vec::new())
            };
            assert_eq!(s.validate().is_ok(), ok, "{mtu}");
        }
        let plain = subnet(serde_json::json!({ "cidr": "10.0.0.0/24" }));
        let bridged = NetworkSpec {
            subnets: vec![plain.clone()],
            ..spec("vmbr0", None)
        };
        assert_eq!(bridged.validate(), Err(NetworkError::SubnetsWithoutSdn));
        let overlay = NetworkSpec {
            subnets: vec![plain.clone()],
            ..vxlan(10, None)
        };
        assert_eq!(overlay.validate(), Err(NetworkError::SubnetsWithoutSdn));
        let two_v4 = sdn(vec![
            plain.clone(),
            subnet(serde_json::json!({ "cidr": "10.0.1.0/24" })),
        ]);
        assert_eq!(two_v4.validate(), Err(NetworkError::DuplicateFamily));
        let bad = sdn(vec![
            plain,
            subnet(serde_json::json!({ "cidr": "2001:db8::/32" })),
        ]);
        assert_eq!(
            bad.validate(),
            Err(NetworkError::Subnet(2, SubnetError::PrefixV6(32)))
        );
        assert_eq!(
            bad.validate().unwrap_err().to_string(),
            "subnet 2: an IPv6 subnet must be /48 to /124, not /32"
        );
    }

    #[test]
    fn names_the_bridge_and_mtu_of_sdn_networks() {
        let s = sdn(Vec::new());
        assert_eq!(s.bridge_name(), OVN_BRIDGE);
        assert_eq!(s.mtu(), Some(DEFAULT_SDN_MTU));
        assert_eq!(s.vni(), None);
        let s = NetworkSpec {
            mtu: Some(8900),
            ..sdn(Vec::new())
        };
        assert_eq!(s.mtu(), Some(8900));
        assert!(!spec("vmbr0", None).is_sdn());
    }

    #[test]
    fn subnet_defaults() {
        let v4 = subnet(serde_json::json!({ "cidr": "10.0.0.0/24" }));
        assert_eq!(v4.gateway(), ip("10.0.0.1"));
        assert!(v4.dhcp());
        let v6 = subnet(serde_json::json!({ "cidr": "2001:db8::/64", "gateway": "2001:db8::ff" }));
        assert_eq!(v6.gateway(), ip("2001:db8::ff"));
        assert!(!v6.dhcp());
        let off = subnet(serde_json::json!({ "cidr": "10.0.0.0/24", "dhcp": false }));
        assert!(!off.dhcp());
    }

    #[test]
    fn validates_subnets() {
        let check = |v: serde_json::Value| subnet(v).validate();
        for ok in [
            serde_json::json!({ "cidr": "10.0.0.0/8" }),
            serde_json::json!({ "cidr": "10.0.0.0/29" }),
            serde_json::json!({ "cidr": "2001:db8::/48" }),
            serde_json::json!({ "cidr": "2001:db8::/124", "dhcp": false }),
            serde_json::json!({
                "cidr": "10.0.0.0/24",
                "gateway": "10.0.0.254",
                "dns_servers": ["1.1.1.1", "8.8.8.8", "2001:4860::8888", "9.9.9.9"],
                "pools": [
                    { "start": "10.0.0.100", "end": "10.0.0.200" },
                    { "start": "10.0.0.2", "end": "10.0.0.99" },
                    { "start": "10.0.0.201", "end": "10.0.0.201" }
                ]
            }),
        ] {
            assert_eq!(check(ok.clone()), Ok(()), "{ok}");
        }
        let pool = |a: &str, b: &str| serde_json::json!({ "start": a, "end": b });
        let cases = [
            (
                serde_json::json!({ "cidr": "10.0.0.0/7" }),
                SubnetError::PrefixV4(7),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/30" }),
                SubnetError::PrefixV4(30),
            ),
            (
                serde_json::json!({ "cidr": "2001:db8::/47" }),
                SubnetError::PrefixV6(47),
            ),
            (
                serde_json::json!({ "cidr": "2001:db8::/125" }),
                SubnetError::PrefixV6(125),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "gateway": "10.0.1.1" }),
                SubnetError::GatewayOutside(ip("10.0.1.1")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "gateway": "2001:db8::1" }),
                SubnetError::GatewayOutside(ip("2001:db8::1")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "gateway": "10.0.0.255" }),
                SubnetError::GatewayReserved(ip("10.0.0.255")),
            ),
            (
                serde_json::json!({ "cidr": "2001:db8::/64", "gateway": "2001:db8::" }),
                SubnetError::GatewayReserved(ip("2001:db8::")),
            ),
            (
                serde_json::json!({ "cidr": "2001:db8::/64", "dhcp": true }),
                SubnetError::Dhcpv6,
            ),
            (
                serde_json::json!({
                    "cidr": "10.0.0.0/24",
                    "dns_servers": ["1.1.1.1", "1.1.1.2", "1.1.1.3", "1.1.1.4", "1.1.1.5"]
                }),
                SubnetError::TooManyDns,
            ),
            (
                serde_json::json!({
                    "cidr": "10.0.0.0/16",
                    "pools": (0..17).map(|i| pool(&format!("10.0.{i}.10"), &format!("10.0.{i}.20"))).collect::<Vec<_>>()
                }),
                SubnetError::TooManyPools,
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "pools": [pool("10.0.0.10", "10.0.1.5")] }),
                SubnetError::PoolOutside(range("10.0.0.10", "10.0.1.5")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "pools": [pool("10.0.0.10", "2001:db8::5")] }),
                SubnetError::PoolOutside(range("10.0.0.10", "2001:db8::5")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "pools": [pool("10.0.0.20", "10.0.0.10")] }),
                SubnetError::PoolReversed(range("10.0.0.20", "10.0.0.10")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "pools": [pool("10.0.0.0", "10.0.0.10")] }),
                SubnetError::PoolReserved(range("10.0.0.0", "10.0.0.10")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "pools": [pool("10.0.0.200", "10.0.0.255")] }),
                SubnetError::PoolReserved(range("10.0.0.200", "10.0.0.255")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "pools": [pool("10.0.0.1", "10.0.0.10")] }),
                SubnetError::GatewayInPool(range("10.0.0.1", "10.0.0.10"), ip("10.0.0.1")),
            ),
            (
                serde_json::json!({
                    "cidr": "10.0.0.0/24",
                    "pools": [pool("10.0.0.50", "10.0.0.60"), pool("10.0.0.10", "10.0.0.50")]
                }),
                SubnetError::PoolsOverlap(
                    range("10.0.0.10", "10.0.0.50"),
                    range("10.0.0.50", "10.0.0.60"),
                ),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "description": "x".repeat(201) }),
                SubnetError::LongDescription,
            ),
        ];
        for (v, err) in cases {
            assert_eq!(check(v.clone()), Err(err), "{v}");
        }
    }

    /// 계획 66: 계획 65 모양(공급자·유동 IP 풀·IPv6 모드 없음)은 바이트 그대로, 새 칸은 왕복한다. 계획 65의
    /// `{"sdn": {"provider": "physnet1"}}`(문자열)은 여전히 거절한다(모양이 객체).
    #[test]
    fn routing_fields_are_written_only_when_set() {
        for old in [
            r#"{"sdn":{},"mtu":1400,"subnets":[{"cidr":"10.10.0.0/24","gateway":"10.10.0.254","dhcp":false,"dns_servers":["1.1.1.1"],"pools":[{"start":"10.10.0.10","end":"10.10.0.99"}],"description":"web"},{"cidr":"2001:db8::/64"}],"folder":"00000000-0000-0000-0000-000000000003"}"#,
            r#"{"sdn":{}}"#,
        ] {
            let s: NetworkSpec = serde_json::from_str(old).unwrap();
            assert!(!s.is_provider());
            assert_eq!(serde_json::to_string(&s).unwrap(), old);
        }
        let json = serde_json::json!({
            "sdn": { "provider": { "physnet": "ext", "vlan": 100 } },
            "subnets": [
                {
                    "cidr": "203.0.113.0/24",
                    "pools": [{ "start": "203.0.113.10", "end": "203.0.113.99" }],
                    "floating_pools": [{ "start": "203.0.113.100", "end": "203.0.113.199" }]
                },
                { "cidr": "2001:db8::/64", "ipv6_mode": "dhcpv6-stateless" }
            ]
        });
        let s: NetworkSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&s).unwrap(), json);
        assert_eq!(s.validate(), Ok(()));
        assert!(s.is_provider());
        assert_eq!(s.physnet(), Some("ext"));
        assert_eq!(s.provider().unwrap().key(), "ext/100");
        assert_eq!(s.subnets[1].ipv6_mode, Some(Ipv6Mode::Dhcpv6Stateless));
        for (mode, text) in [
            (Ipv6Mode::Slaac, "slaac"),
            (Ipv6Mode::Dhcpv6Stateful, "dhcpv6-stateful"),
            (Ipv6Mode::Dhcpv6Stateless, "dhcpv6-stateless"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), text);
        }
        for bad in [
            serde_json::json!({ "sdn": { "provider": "physnet1" } }),
            serde_json::json!({ "sdn": { "provider": { "physnet": "ext", "x": 1 } } }),
            serde_json::json!({ "sdn": { "provider": { "vlan": 5 } } }),
            serde_json::json!({ "sdn": {}, "subnets": [{ "cidr": "2001:db8::/64", "ipv6_mode": "eui64" }] }),
        ] {
            assert!(
                serde_json::from_value::<NetworkSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    fn provider(physnet: &str, vlan: Option<u16>, subnets: Vec<Subnet>) -> NetworkSpec {
        NetworkSpec {
            sdn: Some(SdnNet {
                provider: Some(Provider {
                    physnet: physnet.into(),
                    vlan,
                }),
            }),
            ..sdn(subnets)
        }
    }

    #[test]
    fn validates_provider_networks() {
        assert_eq!(provider("ext", None, Vec::new()).validate(), Ok(()));
        assert_eq!(provider("a", Some(4094), Vec::new()).validate(), Ok(()));
        assert_eq!(
            provider("ext", None, Vec::new()).provider().unwrap().key(),
            "ext/flat"
        );
        for bad in ["", "Ext", "1ext", "ext:1", "ext,b", "ex t", &"a".repeat(32)] {
            assert_eq!(
                provider(bad, None, Vec::new()).validate(),
                Err(NetworkError::BadPhysnet(bad.into())),
                "{bad:?}"
            );
        }
        assert!(
            provider(&"a".repeat(31), None, Vec::new())
                .validate()
                .is_ok()
        );
        for bad in [0, 4095] {
            assert_eq!(
                provider("ext", Some(bad), Vec::new()).validate(),
                Err(NetworkError::BadVlan(bad))
            );
        }
        // 브리지용 `vlan` 칸은 여전히 sdn과 못 쓴다(공급자 VLAN은 provider.vlan).
        let tagged = NetworkSpec {
            vlan: Some(10),
            ..provider("ext", None, Vec::new())
        };
        assert_eq!(tagged.validate(), Err(NetworkError::VlanWithSdn));
        // MTU 기본은 물리 망의 1500, 바꿀 수 있다. 브리지 이름은 늘 br-int다.
        let p = provider("ext", None, Vec::new());
        assert_eq!(p.mtu(), Some(DEFAULT_PROVIDER_MTU));
        assert_eq!(p.bridge_name(), OVN_BRIDGE);
        let p = NetworkSpec {
            mtu: Some(9000),
            ..p
        };
        assert_eq!(p.mtu(), Some(9000));
        assert_eq!(sdn(Vec::new()).mtu(), Some(DEFAULT_SDN_MTU));
        // 공급자 서브넷의 DHCP 기본은 거짓이다(오버레이 IPv4는 참).
        let v4 = subnet(serde_json::json!({ "cidr": "10.0.0.0/24" }));
        let on = subnet(serde_json::json!({ "cidr": "10.0.0.0/24", "dhcp": true }));
        assert!(!provider("ext", None, Vec::new()).subnet_dhcp(&v4));
        assert!(provider("ext", None, Vec::new()).subnet_dhcp(&on));
        assert!(sdn(Vec::new()).subnet_dhcp(&v4));
        // 유동 IP 풀은 공급자 네트워크에만
        let floating = subnet(serde_json::json!({
            "cidr": "203.0.113.0/24",
            "floating_pools": [{ "start": "203.0.113.100", "end": "203.0.113.199" }]
        }));
        assert_eq!(
            provider("ext", None, vec![floating.clone()]).validate(),
            Ok(())
        );
        assert_eq!(
            sdn(vec![floating]).validate(),
            Err(NetworkError::Subnet(
                1,
                SubnetError::FloatingPoolsWithoutProvider
            ))
        );
    }

    #[test]
    fn validates_floating_pools_and_ipv6_modes() {
        let check = |v: serde_json::Value| subnet(v).validate();
        let pool = |a: &str, b: &str| serde_json::json!({ "start": a, "end": b });
        assert_eq!(
            check(serde_json::json!({
                "cidr": "203.0.113.0/24",
                "pools": [pool("203.0.113.10", "203.0.113.99")],
                "floating_pools": [pool("203.0.113.100", "203.0.113.149"), pool("203.0.113.150", "203.0.113.199")]
            })),
            Ok(())
        );
        for mode in ["slaac", "dhcpv6-stateless", "dhcpv6-stateful"] {
            assert_eq!(
                check(serde_json::json!({ "cidr": "2001:db8::/64", "ipv6_mode": mode })),
                Ok(()),
                "{mode}"
            );
        }
        assert_eq!(
            check(serde_json::json!({ "cidr": "2001:db8::/80", "ipv6_mode": "dhcpv6-stateful" })),
            Ok(())
        );
        let cases = [
            (
                serde_json::json!({ "cidr": "2001:db8::/64", "floating_pools": [pool("2001:db8::10", "2001:db8::20")] }),
                SubnetError::FloatingPoolsOnIpv6,
            ),
            (
                serde_json::json!({
                    "cidr": "10.0.0.0/16",
                    "floating_pools": (0..17).map(|i| pool(&format!("10.0.{i}.10"), &format!("10.0.{i}.20"))).collect::<Vec<_>>()
                }),
                SubnetError::TooManyFloatingPools,
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "floating_pools": [pool("10.0.0.10", "10.0.1.5")] }),
                SubnetError::PoolOutside(range("10.0.0.10", "10.0.1.5")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "floating_pools": [pool("10.0.0.1", "10.0.0.5")] }),
                SubnetError::GatewayInPool(range("10.0.0.1", "10.0.0.5"), ip("10.0.0.1")),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "floating_pools": [pool("10.0.0.200", "10.0.0.255")] }),
                SubnetError::PoolReserved(range("10.0.0.200", "10.0.0.255")),
            ),
            (
                serde_json::json!({
                    "cidr": "10.0.0.0/24",
                    "pools": [pool("10.0.0.10", "10.0.0.100")],
                    "floating_pools": [pool("10.0.0.100", "10.0.0.120")]
                }),
                SubnetError::FloatingPoolOverlap(
                    range("10.0.0.100", "10.0.0.120"),
                    range("10.0.0.10", "10.0.0.100"),
                ),
            ),
            (
                serde_json::json!({
                    "cidr": "10.0.0.0/24",
                    "floating_pools": [pool("10.0.0.10", "10.0.0.20"), pool("10.0.0.15", "10.0.0.30")]
                }),
                SubnetError::FloatingPoolOverlap(
                    range("10.0.0.10", "10.0.0.20"),
                    range("10.0.0.15", "10.0.0.30"),
                ),
            ),
            (
                serde_json::json!({ "cidr": "10.0.0.0/24", "ipv6_mode": "slaac" }),
                SubnetError::Ipv6ModeOnIpv4,
            ),
            (
                serde_json::json!({ "cidr": "2001:db8::/80", "ipv6_mode": "slaac" }),
                SubnetError::SlaacNeeds64(80),
            ),
            (
                serde_json::json!({ "cidr": "2001:db8::/56", "ipv6_mode": "dhcpv6-stateless" }),
                SubnetError::SlaacNeeds64(56),
            ),
            // IPv6 `dhcp: true`는 여전히 400이다(`ipv6_mode`가 그 자리).
            (
                serde_json::json!({ "cidr": "2001:db8::/64", "dhcp": true, "ipv6_mode": "dhcpv6-stateful" }),
                SubnetError::Dhcpv6,
            ),
        ];
        for (v, err) in cases {
            assert_eq!(check(v.clone()), Err(err), "{v}");
        }
    }

    #[test]
    fn physnet_devices_are_managed_names() {
        let (o, b, x) = crate::sdn::physnet_ifnames("0123456789ab", "ext");
        for name in [&o, &b, &x] {
            assert!(is_managed_ifname(name), "{name}");
            assert!(valid_ifname(name), "{name}");
        }
        assert!(!is_managed_ifname("oplp0123abcdz"));
        assert!(!is_managed_ifname("oplp0123ABCDo"));
        assert_eq!(
            spec(&b, None).validate(),
            Err(NetworkError::ReservedBridge(b.clone()))
        );
    }
}
