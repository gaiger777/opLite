//! SDN 네트워크 서브넷의 순수 IPAM 계산(계획 65 §9). 주소의 출처는 Raft의 VM 스펙(`NicSpec.ips`)이고, 여기서는 그
//! 쓴 주소 집합과 서브넷만으로 예약·풀·다음 빈 주소를 정한다.

use std::collections::BTreeSet;
use std::net::IpAddr;

use uuid::Uuid;

use crate::network::{NetworkSpec, Subnet};
use crate::security_group::Cidr;
use crate::vm::MacAddr;

/// 주소의 비트(IPv4는 아래 32비트)
fn bits(ip: IpAddr) -> u128 {
    Cidr::bits(ip)
}

/// 비트를 `v4`이면 IPv4, 아니면 IPv6 주소로
fn addr(v4: bool, b: u128) -> IpAddr {
    if v4 {
        IpAddr::V4((b as u32).into())
    } else {
        IpAddr::V6(b.into())
    }
}

/// 범위의 첫 주소(망 주소)와 마지막 주소의 비트
pub(crate) fn span(cidr: &Cidr) -> (u128, u128) {
    let first = bits(cidr.addr);
    (first, first | cidr.host_mask())
}

/// 첫 호스트 주소(망 주소 다음). 게이트웨이를 주지 않았을 때의 게이트웨이다.
pub fn first_host(cidr: &Cidr) -> IpAddr {
    addr(cidr.addr.is_ipv4(), span(cidr).0 + 1)
}

/// 망 주소와(IPv4면) 브로드캐스트 주소. 게이트웨이·풀로 쓸 수 없다.
pub(crate) fn edges(cidr: &Cidr) -> Vec<IpAddr> {
    let v4 = cidr.addr.is_ipv4();
    let (first, last) = span(cidr);
    let mut out = vec![addr(v4, first)];
    if v4 {
        out.push(addr(v4, last));
    }
    out
}

/// 할당하지 않는 주소: 망 주소, 브로드캐스트(IPv4), 게이트웨이(라우터가 없어도 늘 — 계획 66의 라우터 포트 자리)
pub fn reserved(subnet: &Subnet) -> Vec<IpAddr> {
    let mut out = edges(&subnet.cidr);
    out.push(subnet.gateway());
    out.sort();
    out.dedup();
    out
}

/// 할당 범위(비트, 시작 순). `pools`가 없으면 CIDR에서 망 주소·브로드캐스트를 뺀 전체다.
fn ranges(subnet: &Subnet) -> Vec<(u128, u128)> {
    let mut out: Vec<(u128, u128)> = if subnet.pools.is_empty() {
        let (first, last) = span(&subnet.cidr);
        let last = if subnet.is_ipv4() { last - 1 } else { last };
        vec![(first + 1, last)]
    } else {
        subnet
            .pools
            .iter()
            .map(|p| (bits(p.start), bits(p.end)))
            .collect()
    };
    out.sort();
    out
}

/// 할당할 수 있는 주소를 낮은 것부터(예약은 뺌). 게으르게 돌아 `/64`에도 쓸 수 있다.
pub fn pool_iter(subnet: &Subnet) -> impl Iterator<Item = IpAddr> {
    let v4 = subnet.is_ipv4();
    let reserved = reserved(subnet);
    ranges(subnet)
        .into_iter()
        .flat_map(|(start, end)| start..=end)
        .map(move |b| addr(v4, b))
        .filter(move |ip| !reserved.contains(ip))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IpamError {
    #[error("subnet {0} has no free addresses")]
    Exhausted(Cidr),
    #[error("address {0} is not in subnet {1}")]
    OutsideSubnet(IpAddr, Cidr),
    #[error("address {0} is reserved (network, broadcast or gateway address)")]
    Reserved(IpAddr),
    #[error("address {0} is already in use")]
    InUse(IpAddr),
    #[error(
        "address {0} is not the EUI-64 address {1} of the NIC's MAC; addresses in a SLAAC subnet come from the MAC"
    )]
    NotEui64(IpAddr, IpAddr),
}

/// 서브넷에서 주소 하나를 고른다. `want`가 있으면 그것이 서브넷 안이고 예약·사용 중이 아닌지만 본다(풀 밖이어도
/// 된다 — Neutron의 고정 IP와 같다). 없으면 풀에서 가장 낮은 빈 주소다. `used`는 그 네트워크에서 쓰는 주소(다른
/// 체계의 주소가 섞여도 된다). 드는 시간은 풀 크기가 아니라 앞에서 이어 쓴 주소 수에 비례한다.
pub fn allocate(
    subnet: &Subnet,
    used: &BTreeSet<IpAddr>,
    want: Option<IpAddr>,
) -> Result<IpAddr, IpamError> {
    if let Some(want) = want {
        let reserved = reserved(subnet);
        let want = want.to_canonical();
        if !subnet.cidr.contains(want) {
            return Err(IpamError::OutsideSubnet(want, subnet.cidr));
        }
        if reserved.contains(&want) {
            return Err(IpamError::Reserved(want));
        }
        if used.contains(&want) {
            return Err(IpamError::InUse(want));
        }
        return Ok(want);
    }
    // NIC는 유동 IP 풀(계획 66 §8 — 라우터 외부 주소·유동 IP의 자리)에서 저절로 받지 않는다.
    let skip: Vec<(u128, u128)> = subnet
        .floating_pools
        .iter()
        .map(|p| (bits(p.start), bits(p.end)))
        .collect();
    pick(subnet, ranges(subnet), &skip, used)
}

/// 범위들 `ranges`에서 `skip` 범위·예약·사용 중을 빼고 가장 낮은 주소
fn pick(
    subnet: &Subnet,
    ranges: Vec<(u128, u128)>,
    skip: &[(u128, u128)],
    used: &BTreeSet<IpAddr>,
) -> Result<IpAddr, IpamError> {
    let reserved = reserved(subnet);
    let v4 = subnet.is_ipv4();
    for (start, end) in ranges {
        let mut cur = start;
        while cur <= end {
            if let Some((_, e)) = skip.iter().find(|(s, e)| (*s..=*e).contains(&cur)) {
                match e.checked_add(1) {
                    Some(next) => cur = next,
                    None => break,
                }
                continue;
            }
            let ip = addr(v4, cur);
            if !reserved.contains(&ip) && !used.contains(&ip) {
                return Ok(ip);
            }
            match cur.checked_add(1) {
                Some(next) => cur = next,
                None => break,
            }
        }
    }
    Err(IpamError::Exhausted(subnet.cidr))
}

/// 라우터 외부 주소와 유동 IP를 고른다(계획 66 §8): 서브넷의 유동 IP 풀 `floating_pools`가 있으면 거기서만, 없으면 NIC와
/// 같은 할당 범위(풀, 없으면 CIDR)에서 가장 낮은 빈 주소. `want`는 [`allocate`]와 같다(서브넷 안의 예약이 아닌 어디든).
pub fn allocate_from(
    subnet: &Subnet,
    used: &BTreeSet<IpAddr>,
    want: Option<IpAddr>,
) -> Result<IpAddr, IpamError> {
    if want.is_some() || subnet.floating_pools.is_empty() {
        // 유동 IP 풀이 없으면 건너뛸 것도 없다.
        return allocate(subnet, used, want);
    }
    let mut pools: Vec<(u128, u128)> = subnet
        .floating_pools
        .iter()
        .map(|p| (bits(p.start), bits(p.end)))
        .collect();
    pools.sort();
    pick(subnet, pools, &[], used)
}

/// NIC(MAC `mac`)의 주소를 서브넷에서 고른다(계획 66 §10): 주소를 게스트가 MAC에서 만드는 IPv6 서브넷(`ipv6_mode`가
/// `slaac`·`dhcpv6-stateless`)이면 늘 [`eui64`] 주소이고 다른 주소를 원하면 [`IpamError::NotEui64`]다. 아니면
/// [`allocate`]다.
pub fn allocate_v6_mode(
    subnet: &Subnet,
    mac: MacAddr,
    used: &BTreeSet<IpAddr>,
    want: Option<IpAddr>,
) -> Result<IpAddr, IpamError> {
    let Some(prefix) = slaac_prefix(subnet) else {
        return allocate(subnet, used, want);
    };
    let eui = IpAddr::V6(eui64(prefix, mac));
    if let Some(want) = want.map(|w| w.to_canonical())
        && want != eui
    {
        return Err(IpamError::NotEui64(want, eui));
    }
    if used.contains(&eui) {
        return Err(IpamError::InUse(eui));
    }
    Ok(eui)
}

/// 주소를 게스트가 MAC에서 만드는 IPv6 서브넷이면 그 접두사
pub fn slaac_prefix(subnet: &Subnet) -> Option<std::net::Ipv6Addr> {
    match (subnet.cidr.addr, subnet.ipv6_mode) {
        (IpAddr::V6(prefix), Some(mode)) if mode.is_slaac() => Some(prefix),
        _ => None,
    }
}

/// OVN DHCP 서버의 MAC: `02:` + 네트워크 id(16바이트) SHA-256의 앞 5바이트. 로컬 관리 유니캐스트 주소이고 같은
/// 네트워크는 늘 같은 값이다(리더가 바뀌어도).
pub fn server_mac(network: Uuid) -> MacAddr {
    let digest = ring::digest::digest(&ring::digest::SHA256, network.as_bytes());
    let d = digest.as_ref();
    MacAddr([0x02, d[0], d[1], d[2], d[3], d[4]])
}

/// 수정 EUI-64 주소(RFC 4291 부록 A, 계획 66 §10): `prefix`의 위 64비트 + MAC에서 만든 인터페이스 id(가운데 `ff:fe`,
/// 첫 바이트의 U/L 비트 뒤집기). SLAAC 게스트가 스스로 만드는 주소라 IPAM이 같은 주소를 포트 보안에 적는다.
pub fn eui64(prefix: std::net::Ipv6Addr, mac: MacAddr) -> std::net::Ipv6Addr {
    let m = mac.0;
    let iid = u64::from_be_bytes([m[0] ^ 0x02, m[1], m[2], 0xff, 0xfe, m[3], m[4], m[5]]);
    let net = u128::from(prefix) & !u128::from(u64::MAX);
    (net | u128::from(iid)).into()
}

/// 주소 `ip`가 든 네트워크의 서브넷
pub fn nic_subnet(network: &NetworkSpec, ip: IpAddr) -> Option<&Subnet> {
    network.subnets.iter().find(|s| s.cidr.contains(ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::IpRange;
    use std::time::Instant;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn subnet(cidr: &str) -> Subnet {
        Subnet {
            floating_pools: Vec::new(),
            ipv6_mode: None,
            cidr: cidr.parse().unwrap(),
            gateway: None,
            dhcp: None,
            dns_servers: Vec::new(),
            pools: Vec::new(),
            description: String::new(),
        }
    }

    fn used(ips: &[&str]) -> BTreeSet<IpAddr> {
        ips.iter().map(|s| ip(s)).collect()
    }

    #[test]
    fn reserves_network_broadcast_and_gateway() {
        assert_eq!(
            reserved(&subnet("10.0.0.0/24")),
            [ip("10.0.0.0"), ip("10.0.0.1"), ip("10.0.0.255")]
        );
        let mut s = subnet("10.0.0.0/24");
        s.gateway = Some(ip("10.0.0.254"));
        assert_eq!(
            reserved(&s),
            [ip("10.0.0.0"), ip("10.0.0.254"), ip("10.0.0.255")]
        );
        // IPv6에는 브로드캐스트가 없다.
        assert_eq!(
            reserved(&subnet("2001:db8::/64")),
            [ip("2001:db8::"), ip("2001:db8::1")]
        );
    }

    #[test]
    fn pool_iter_walks_pools_without_reserved() {
        let all: Vec<_> = pool_iter(&subnet("10.0.0.0/29")).collect();
        assert_eq!(
            all,
            ["10.0.0.2", "10.0.0.3", "10.0.0.4", "10.0.0.5", "10.0.0.6"].map(ip)
        );
        let mut s = subnet("10.0.0.0/24");
        s.pools = vec![
            IpRange {
                start: ip("10.0.0.20"),
                end: ip("10.0.0.21"),
            },
            IpRange {
                start: ip("10.0.0.10"),
                end: ip("10.0.0.11"),
            },
        ];
        let all: Vec<_> = pool_iter(&s).collect();
        assert_eq!(
            all,
            ["10.0.0.10", "10.0.0.11", "10.0.0.20", "10.0.0.21"].map(ip)
        );
        // /64도 게으르게 돈다.
        assert_eq!(
            pool_iter(&subnet("2001:db8::/64")).nth(1),
            Some(ip("2001:db8::3"))
        );
    }

    #[test]
    fn allocates_the_lowest_free_address() {
        let s = subnet("10.0.0.0/24");
        assert_eq!(allocate(&s, &used(&[]), None), Ok(ip("10.0.0.2")));
        assert_eq!(
            allocate(&s, &used(&["10.0.0.2", "10.0.0.3", "10.0.0.5"]), None),
            Ok(ip("10.0.0.4"))
        );
        // 다른 체계·다른 서브넷의 주소는 상관없다.
        assert_eq!(
            allocate(&s, &used(&["2001:db8::2", "10.0.1.2"]), None),
            Ok(ip("10.0.0.2"))
        );
        // 풀의 틈과 다음 풀
        let mut p = subnet("10.0.0.0/24");
        p.pools = vec![
            IpRange {
                start: ip("10.0.0.10"),
                end: ip("10.0.0.11"),
            },
            IpRange {
                start: ip("10.0.0.20"),
                end: ip("10.0.0.20"),
            },
        ];
        assert_eq!(
            allocate(&p, &used(&["10.0.0.10", "10.0.0.11"]), None),
            Ok(ip("10.0.0.20"))
        );
        assert_eq!(
            allocate(&p, &used(&["10.0.0.10", "10.0.0.11", "10.0.0.20"]), None),
            Err(IpamError::Exhausted(p.cidr))
        );
        // 다 쓴 /29
        let small = subnet("10.0.0.0/29");
        let full: BTreeSet<_> = pool_iter(&small).collect();
        assert_eq!(
            allocate(&small, &full, None),
            Err(IpamError::Exhausted(small.cidr))
        );
        // IPv6
        let v6 = subnet("2001:db8::/120");
        assert_eq!(
            allocate(&v6, &used(&["2001:db8::2"]), None),
            Ok(ip("2001:db8::3"))
        );
    }

    #[test]
    fn checks_a_wanted_address() {
        let s = subnet("10.0.0.0/24");
        let u = used(&["10.0.0.7"]);
        assert_eq!(
            allocate(&s, &u, Some(ip("10.0.0.200"))),
            Ok(ip("10.0.0.200"))
        );
        assert_eq!(
            allocate(&s, &u, Some(ip("::ffff:10.0.0.9"))),
            Ok(ip("10.0.0.9"))
        );
        assert_eq!(
            allocate(&s, &u, Some(ip("10.0.0.7"))),
            Err(IpamError::InUse(ip("10.0.0.7")))
        );
        for r in ["10.0.0.0", "10.0.0.1", "10.0.0.255"] {
            assert_eq!(
                allocate(&s, &u, Some(ip(r))),
                Err(IpamError::Reserved(ip(r)))
            );
        }
        for out in ["10.0.1.1", "2001:db8::5"] {
            assert_eq!(
                allocate(&s, &u, Some(ip(out))),
                Err(IpamError::OutsideSubnet(ip(out), s.cidr))
            );
        }
        // 풀 밖이어도 서브넷 안이면 된다.
        let mut p = subnet("10.0.0.0/24");
        p.pools = vec![IpRange {
            start: ip("10.0.0.10"),
            end: ip("10.0.0.20"),
        }];
        assert_eq!(allocate(&p, &u, Some(ip("10.0.0.50"))), Ok(ip("10.0.0.50")));
    }

    /// 계획 66 §8: NIC는 유동 IP 풀을 건너뛰고, 라우터 외부 주소·유동 IP는 유동 IP 풀(없으면 풀)에서 고른다.
    #[test]
    fn floating_pools_are_kept_for_routers_and_floating_ips() {
        let mut s = subnet("203.0.113.0/24");
        s.floating_pools = vec![IpRange {
            start: ip("203.0.113.2"),
            end: ip("203.0.113.3"),
        }];
        assert_eq!(allocate(&s, &used(&[]), None), Ok(ip("203.0.113.4")));
        assert_eq!(allocate_from(&s, &used(&[]), None), Ok(ip("203.0.113.2")));
        assert_eq!(
            allocate_from(&s, &used(&["203.0.113.2", "203.0.113.3"]), None),
            Err(IpamError::Exhausted(s.cidr))
        );
        // 준 주소는 서브넷 안 어디든(NIC도 유동 IP 풀의 주소를 직접 줄 수는 있다)
        assert_eq!(
            allocate_from(&s, &used(&[]), Some(ip("203.0.113.90"))),
            Ok(ip("203.0.113.90"))
        );
        assert_eq!(
            allocate(&s, &used(&[]), Some(ip("203.0.113.2"))),
            Ok(ip("203.0.113.2"))
        );
        // 유동 IP 풀이 없으면 NIC와 같은 범위
        let plain = subnet("203.0.113.0/24");
        assert_eq!(
            allocate_from(&plain, &used(&["203.0.113.2"]), None),
            Ok(ip("203.0.113.3"))
        );
    }

    /// 계획 66 §10: SLAAC 서브넷의 NIC 주소는 MAC의 EUI-64다.
    #[test]
    fn slaac_subnets_give_eui64_addresses() {
        let mac: MacAddr = "52:54:00:12:34:56".parse().unwrap();
        let mut s = subnet("2001:db8:1::/64");
        s.ipv6_mode = Some(crate::network::Ipv6Mode::Slaac);
        let eui = ip("2001:db8:1::5054:ff:fe12:3456");
        assert_eq!(allocate_v6_mode(&s, mac, &used(&[]), None), Ok(eui));
        assert_eq!(allocate_v6_mode(&s, mac, &used(&[]), Some(eui)), Ok(eui));
        assert_eq!(
            allocate_v6_mode(&s, mac, &used(&[]), Some(ip("2001:db8:1::5"))),
            Err(IpamError::NotEui64(ip("2001:db8:1::5"), eui))
        );
        assert_eq!(
            allocate_v6_mode(&s, mac, &[eui].into(), None),
            Err(IpamError::InUse(eui))
        );
        s.ipv6_mode = Some(crate::network::Ipv6Mode::Dhcpv6Stateless);
        assert_eq!(allocate_v6_mode(&s, mac, &used(&[]), None), Ok(eui));
        // 상태 있는 DHCPv6와 모드 없는 서브넷은 풀에서
        s.ipv6_mode = Some(crate::network::Ipv6Mode::Dhcpv6Stateful);
        assert_eq!(
            allocate_v6_mode(&s, mac, &used(&[]), None),
            Ok(ip("2001:db8:1::2"))
        );
        let v4 = subnet("10.0.0.0/24");
        assert_eq!(
            allocate_v6_mode(&v4, mac, &used(&[]), None),
            Ok(ip("10.0.0.2"))
        );
    }

    /// `/16`에 앞에서부터 1만 개를 썼어도 다음 빈 주소를 곧 찾는다.
    #[test]
    fn allocation_scales_with_used_addresses() {
        let s = subnet("10.1.0.0/16");
        let taken: BTreeSet<IpAddr> = pool_iter(&s).take(10_000).collect();
        let started = Instant::now();
        let got = allocate(&s, &taken, None).unwrap();
        assert_eq!(got, pool_iter(&s).nth(10_000).unwrap());
        assert!(
            started.elapsed().as_millis() < 500,
            "{:?}",
            started.elapsed()
        );
        // 큰 IPv6 서브넷도 같다.
        let v6 = subnet("2001:db8::/64");
        let taken: BTreeSet<IpAddr> = pool_iter(&v6).take(10_000).collect();
        assert_eq!(
            allocate(&v6, &taken, None).unwrap(),
            pool_iter(&v6).nth(10_000).unwrap()
        );
    }

    #[test]
    fn server_mac_is_stable_local_and_unicast() {
        let id = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let mac = server_mac(id);
        assert_eq!(mac, server_mac(id));
        assert_eq!(mac.0[0], 0x02);
        assert_ne!(mac, server_mac(Uuid::from_u128(1)));
        // SHA-256(id 16바이트)의 앞 5바이트
        let d = ring::digest::digest(&ring::digest::SHA256, id.as_bytes());
        assert_eq!(&mac.0[1..], &d.as_ref()[..5]);
    }

    #[test]
    fn finds_the_subnet_of_an_address() {
        let net = NetworkSpec {
            bridge: None,
            vlan: None,
            vxlan: None,
            sdn: Some(crate::network::SdnNet::default()),
            mtu: None,
            subnets: vec![subnet("10.0.0.0/24"), subnet("2001:db8::/64")],
            folder: None,
        };
        assert_eq!(
            nic_subnet(&net, ip("10.0.0.9")).map(|s| s.cidr.to_string()),
            Some("10.0.0.0/24".into())
        );
        assert_eq!(
            nic_subnet(&net, ip("2001:db8::9")).map(|s| s.cidr.to_string()),
            Some("2001:db8::/64".into())
        );
        assert_eq!(nic_subnet(&net, ip("10.0.1.9")), None);
    }

    #[test]
    fn first_host_and_edges() {
        let c: Cidr = "10.0.0.0/24".parse().unwrap();
        assert_eq!(first_host(&c), ip("10.0.0.1"));
        assert_eq!(edges(&c), [ip("10.0.0.0"), ip("10.0.0.255")]);
        let c: Cidr = "2001:db8::/124".parse().unwrap();
        assert_eq!(first_host(&c), ip("2001:db8::1"));
        assert_eq!(edges(&c), [ip("2001:db8::")]);
    }

    /// RFC 4291 부록 A의 예: MAC 34-56-78-9A-BC-DE → 인터페이스 id 3656:78ff:fe9a:bcde(U/L 비트 뒤집기)
    #[test]
    fn eui64_follows_rfc_4291() {
        let mac = MacAddr([0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde]);
        let prefix: std::net::Ipv6Addr = "2001:db8:1:2::".parse().unwrap();
        assert_eq!(
            eui64(prefix, mac),
            "2001:db8:1:2:3656:78ff:fe9a:bcde"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
        );
        // 접두사의 아래 64비트는 무시한다. 로컬 관리 MAC(02:…)은 U/L 비트가 꺼진다.
        let host: std::net::Ipv6Addr = "2001:db8:1:2::99".parse().unwrap();
        assert_eq!(eui64(host, mac), eui64(prefix, mac));
        let local = MacAddr([0x02, 0, 0, 0, 0, 1]);
        assert_eq!(
            eui64(prefix, local),
            "2001:db8:1:2::ff:fe00:1"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
        );
    }
}
