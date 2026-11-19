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

/// SDN(OVN) 네트워크. 지금은 칸이 없다(계획 66이 공급자 네트워크 `provider`를 더한다).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SdnNet {}

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
        let gateway = self.gateway();
        let (first, last) = ipam::span(&self.cidr);
        let bits = |ip: IpAddr| crate::security_group::Cidr::bits(ip);
        for pool in &self.pools {
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
        }
        let mut pools = self.pools.clone();
        pools.sort_by_key(|p| bits(p.start));
        for pair in pools.windows(2) {
            if bits(pair[1].start) <= bits(pair[0].end) {
                return Err(SubnetError::PoolsOverlap(pair[0], pair[1]));
            }
        }
        if self.description.chars().count() > MAX_SUBNET_DESCRIPTION {
            return Err(SubnetError::LongDescription);
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
            (None, None, Some(_)) => {
                if self.vlan.is_some() {
                    return Err(NetworkError::VlanWithSdn);
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

    /// VM NIC가 붙을 브리지 이름: `bridge`, `oplv<vni>`, SDN이면 OVS `br-int`. 검증을 통과한 스펙에서만 뜻이 있다.
    pub fn bridge_name(&self) -> String {
        match (&self.bridge, self.vni()) {
            (Some(bridge), _) => bridge.clone(),
            (None, Some(vni)) => vxlan_bridge(vni),
            (None, None) if self.is_sdn() => OVN_BRIDGE.to_string(),
            (None, None) => String::new(),
        }
    }

    /// VXLAN·SDN 네트워크의 MTU(없으면 기본값). 브리지 네트워크는 None.
    pub fn mtu(&self) -> Option<u16> {
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

/// opld가 관리하는 장치 이름 모양(`^opl[vx][0-9]+$`)인지
pub fn is_managed_ifname(name: &str) -> bool {
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
    #[error("subnets can only be used with sdn")]
    SubnetsWithoutSdn,
    #[error("a network can have at most one IPv4 and one IPv6 subnet")]
    DuplicateFamily,
    #[error("subnet {0}: {1}")]
    Subnet(usize, SubnetError),
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
            sdn: Some(SdnNet {}),
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
            assert_eq!((s.sdn, s.subnets.len()), (None, 0));
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
}
