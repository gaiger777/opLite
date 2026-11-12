//! 보안 그룹: VM NIC에 거는 방화벽 규칙 묶음. 노드가 nftables bridge 규칙으로 적용한다.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::resource::{Kind, Resource, ResourceSpec};

/// 그룹 하나의 최대 규칙 수
pub const MAX_RULES: usize = 200;

/// 설명의 최대 글자 수
pub const MAX_DESCRIPTION: usize = 200;

pub type SecurityGroup = Resource<SecurityGroupSpec, SecurityGroupStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SecurityGroupSpec {
    /// 200자 이하
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub description: Option<String>,
    /// 적힌 순서대로 보고 처음 맞는 규칙을 따른다(200개 이하).
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl ResourceSpec for SecurityGroupSpec {
    const KIND: Kind = Kind::SecurityGroup;
    type Status = SecurityGroupStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct SecurityGroupStatus {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub direction: Direction,
    pub action: Action,
    /// 없으면 모든 프로토콜
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub protocol: Option<Protocol>,
    /// 목적지 포트 `"22"` 또는 `"8000-8080"`. tcp·udp에서만.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(nullable = false, default, value_type = String, example = "8000-8080")
    )]
    pub ports: Option<PortRange>,
    /// `in`이면 보낸 쪽, `out`이면 받는 쪽 주소(CIDR). 없으면 어디든.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(nullable = false, default, value_type = String, example = "10.0.0.0/8")
    )]
    pub remote: Option<Cidr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// VM으로 들어옴
    In,
    /// VM에서 나감
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Accept,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
    Icmp,
    Icmpv6,
}

impl SecurityGroupSpec {
    pub fn validate(&self) -> Result<(), SecurityGroupError> {
        if let Some(d) = &self.description
            && d.chars().count() > MAX_DESCRIPTION
        {
            return Err(SecurityGroupError::LongDescription);
        }
        if self.rules.len() > MAX_RULES {
            return Err(SecurityGroupError::TooManyRules);
        }
        for (i, rule) in self.rules.iter().enumerate() {
            // 규칙 번호는 1부터(oplctl `rule rm`과 같다)
            rule.validate()
                .map_err(|reason| SecurityGroupError::Rule(i + 1, reason))?;
        }
        Ok(())
    }
}

impl Rule {
    fn validate(&self) -> Result<(), RuleError> {
        if let Some(ports) = self.ports {
            ports.check()?;
            if !matches!(self.protocol, Some(Protocol::Tcp | Protocol::Udp)) {
                return Err(RuleError::PortsNeedTcpOrUdp);
            }
        }
        if let Some(remote) = self.remote {
            remote.check()?;
        }
        match (self.protocol, self.remote.map(|c| c.addr)) {
            (Some(Protocol::Icmp), Some(IpAddr::V6(_))) => Err(RuleError::IcmpWithIpv6),
            (Some(Protocol::Icmpv6), Some(IpAddr::V4(_))) => Err(RuleError::Icmpv6WithIpv4),
            _ => Ok(()),
        }
    }
}

/// 목적지 포트 범위. `"22"` 또는 `"8000-8080"`으로 직렬화한다(1~65535, 시작 ≤ 끝).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

impl PortRange {
    fn check(self) -> Result<(), RuleError> {
        if self.start == 0 || self.start > self.end {
            return Err(RuleError::BadPorts(self.to_string()));
        }
        Ok(())
    }
}

impl FromStr for PortRange {
    type Err = RuleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || RuleError::BadPorts(s.to_string());
        let (start, end) = s.split_once('-').unwrap_or((s, s));
        let range = PortRange {
            start: parse_digits(start).ok_or_else(err)?,
            end: parse_digits(end).ok_or_else(err)?,
        };
        range.check().map_err(|_| err())?;
        Ok(range)
    }
}

impl fmt::Display for PortRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start == self.end {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}-{}", self.start, self.end)
        }
    }
}

impl TryFrom<String> for PortRange {
    type Error = RuleError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<PortRange> for String {
    fn from(p: PortRange) -> String {
        p.to_string()
    }
}

/// IP 주소 범위. `"10.0.0.0/8"`, `"2001:db8::/32"`로 직렬화한다.
/// 접두사를 빼면 주소 하나(/32, /128)이고, 접두사 밖의 비트가 켜져 있으면 받지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    fn max_prefix(addr: IpAddr) -> u8 {
        match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        }
    }

    /// 접두사 밖(호스트) 비트의 마스크. 128비트를 모두 밀면 넘치므로 그때는 전부.
    fn host_mask(self) -> u128 {
        let max = Self::max_prefix(self.addr);
        1u128
            .checked_shl(u32::from(max - self.prefix))
            .map_or(u128::MAX, |b| b - 1)
    }

    fn bits(addr: IpAddr) -> u128 {
        match addr {
            IpAddr::V4(a) => u128::from(u32::from(a)),
            IpAddr::V6(a) => u128::from(a),
        }
    }

    fn check(self) -> Result<(), RuleError> {
        if self.prefix > Self::max_prefix(self.addr) {
            return Err(RuleError::BadCidr(self.to_string()));
        }
        if Self::bits(self.addr) & self.host_mask() != 0 {
            return Err(RuleError::HostBits(self.to_string()));
        }
        Ok(())
    }

    /// `ip`가 이 범위 안에 있다. IPv4에 대응한 IPv6 주소(`::ffff:a.b.c.d`)는 IPv4로 본다.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        ip.is_ipv4() == self.addr.is_ipv4()
            && Self::bits(ip) & !self.host_mask() == Self::bits(self.addr)
    }
}

impl FromStr for Cidr {
    type Err = RuleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || RuleError::BadCidr(s.to_string());
        let (addr, prefix) = match s.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (s, None),
        };
        let addr: IpAddr = addr.parse().map_err(|_| err())?;
        let prefix = match prefix {
            Some(p) => parse_digits(p).ok_or_else(err)?,
            None => u16::from(Self::max_prefix(addr)),
        };
        let cidr = Cidr {
            addr,
            prefix: u8::try_from(prefix).map_err(|_| err())?,
        };
        cidr.check().map_err(|e| match e {
            RuleError::HostBits(_) => RuleError::HostBits(s.to_string()),
            _ => err(),
        })?;
        Ok(cidr)
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl TryFrom<String> for Cidr {
    type Error = RuleError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Cidr> for String {
    fn from(c: Cidr) -> String {
        c.to_string()
    }
}

/// 10진 숫자만(부호·공백 없이) 받는다. u16을 넘으면 None.
fn parse_digits(s: &str) -> Option<u16> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecurityGroupError {
    #[error("description must be at most {MAX_DESCRIPTION} characters")]
    LongDescription,
    #[error("a security group can have at most {MAX_RULES} rules")]
    TooManyRules,
    #[error("rule {0}: {1}")]
    Rule(usize, RuleError),
}

/// 규칙 하나의 문제. `SecurityGroupError::Rule`이 규칙 번호를 붙인다.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuleError {
    #[error("invalid ports {0:?}: use a port like \"22\" or a range like \"8000-8080\" (1-65535)")]
    BadPorts(String),
    #[error("invalid CIDR {0:?}: use an address like \"10.0.0.0/8\" or \"2001:db8::/32\"")]
    BadCidr(String),
    #[error("CIDR {0:?} has bits set after the prefix length")]
    HostBits(String),
    #[error("ports can only be used with protocol tcp or udp")]
    PortsNeedTcpOrUdp,
    #[error("protocol icmp can only be used with an IPv4 remote; use icmpv6 for IPv6")]
    IcmpWithIpv6,
    #[error("protocol icmpv6 can only be used with an IPv6 remote; use icmp for IPv4")]
    Icmpv6WithIpv4,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(json: serde_json::Value) -> Rule {
        serde_json::from_value(json).unwrap()
    }

    fn group(rules: Vec<Rule>) -> SecurityGroupSpec {
        SecurityGroupSpec {
            description: None,
            rules,
        }
    }

    #[test]
    fn kind_is_security_group() {
        assert_eq!(
            serde_json::to_value(Kind::SecurityGroup).unwrap(),
            "security-group"
        );
        assert_eq!(Kind::SecurityGroup.as_str(), "security-group");
    }

    #[test]
    fn spec_round_trips_and_omits_empty_fields() {
        let json = json!({
            "description": "web servers",
            "rules": [
                { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "80", "remote": "0.0.0.0/0" },
                { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "8000-8080", "remote": "2001:db8::/32" },
                { "direction": "out", "action": "drop", "protocol": "udp", "ports": "53" },
                { "direction": "in", "action": "accept", "protocol": "icmpv6" },
                { "direction": "out", "action": "drop" }
            ]
        });
        let spec: SecurityGroupSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(spec.validate(), Ok(()));
        assert_eq!(serde_json::to_value(&spec).unwrap(), json);
        assert_eq!(
            spec.rules[1],
            Rule {
                direction: Direction::In,
                action: Action::Accept,
                protocol: Some(Protocol::Tcp),
                ports: Some(PortRange {
                    start: 8000,
                    end: 8080
                }),
                remote: Some(Cidr {
                    addr: "2001:db8::".parse().unwrap(),
                    prefix: 32
                }),
            }
        );

        // 빈 그룹: 설명·규칙 없이 읽고, 쓸 때 설명은 빠진다.
        let empty: SecurityGroupSpec = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, group(vec![]));
        assert_eq!(
            serde_json::to_value(&empty).unwrap(),
            json!({ "rules": [] })
        );
        assert_eq!(
            serde_json::to_value(SecurityGroupStatus::default()).unwrap(),
            json!({})
        );
    }

    #[cfg(feature = "openapi")]
    #[test]
    fn openapi_shows_ports_and_remote_as_strings() {
        let schema = serde_json::to_value(<Rule as utoipa::PartialSchema>::schema()).unwrap();
        for field in ["ports", "remote"] {
            assert_eq!(schema["properties"][field]["type"], "string", "{schema}");
        }
        assert_eq!(schema["required"], json!(["direction", "action"]));
    }

    #[test]
    fn rejects_unknown_fields_and_values() {
        for bad in [
            json!({ "rules": [], "policy": "drop" }),
            json!({ "rules": [{ "direction": "in", "action": "accept", "port": "22" }] }),
            json!({ "rules": [{ "direction": "both", "action": "accept" }] }),
            json!({ "rules": [{ "direction": "in", "action": "reject" }] }),
            json!({ "rules": [{ "direction": "in", "action": "accept", "protocol": "sctp" }] }),
            json!({ "rules": [{ "action": "accept" }] }),
        ] {
            assert!(
                serde_json::from_value::<SecurityGroupSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn port_ranges_parse_and_format() {
        for (text, start, end) in [
            ("22", 22, 22),
            ("1", 1, 1),
            ("65535", 65535, 65535),
            ("8000-8080", 8000, 8080),
            ("1-65535", 1, 65535),
            ("5-5", 5, 5),
        ] {
            let p: PortRange = text.parse().unwrap();
            assert_eq!(p, PortRange { start, end }, "{text}");
        }
        // 같은 시작·끝은 한 포트로 쓴다.
        assert_eq!("5-5".parse::<PortRange>().unwrap().to_string(), "5");
        assert_eq!(PortRange { start: 1, end: 2 }.to_string(), "1-2");
        for bad in [
            "", "0", "65536", "0-10", "10-9", "-", "1-", "-1", "+22", " 22", "22 ", "1-2-3", "a",
            "1-65536",
        ] {
            assert_eq!(
                bad.parse::<PortRange>(),
                Err(RuleError::BadPorts(bad.into())),
                "{bad:?}"
            );
        }
        assert!(serde_json::from_value::<PortRange>(json!(22)).is_err());
        assert!(serde_json::from_value::<PortRange>(json!("0")).is_err());
    }

    #[test]
    fn cidrs_contain_addresses_in_their_prefix() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let c: Cidr = "10.0.0.0/8".parse().unwrap();
        assert!(c.contains(ip("10.0.0.0")));
        assert!(c.contains(ip("10.255.255.255")));
        assert!(!c.contains(ip("11.0.0.0")));
        assert!(!c.contains(ip("::a00:1")));
        // IPv6 소켓으로 받은 IPv4 연결
        assert!(c.contains(ip("::ffff:10.1.2.3")));
        let one: Cidr = "192.168.1.5".parse().unwrap();
        assert!(one.contains(ip("192.168.1.5")));
        assert!(!one.contains(ip("192.168.1.4")));
        let all: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains(ip("1.2.3.4")));
        assert!(!all.contains(ip("2001:db8::1")));
        let v6: Cidr = "2001:db8::/32".parse().unwrap();
        assert!(v6.contains(ip("2001:db8:ffff::1")));
        assert!(!v6.contains(ip("2001:db9::1")));
        assert!("::/0".parse::<Cidr>().unwrap().contains(ip("fe80::1")));
    }

    #[test]
    fn cidrs_parse_and_format() {
        for (text, canonical) in [
            ("10.0.0.0/8", "10.0.0.0/8"),
            ("0.0.0.0/0", "0.0.0.0/0"),
            ("192.168.1.5", "192.168.1.5/32"),
            ("192.168.1.5/32", "192.168.1.5/32"),
            ("2001:db8::/32", "2001:db8::/32"),
            ("::/0", "::/0"),
            ("2001:db8::1", "2001:db8::1/128"),
            ("2001:DB8::/32", "2001:db8::/32"),
            ("10.0.0.0/08", "10.0.0.0/8"),
        ] {
            let c: Cidr = text.parse().unwrap();
            assert_eq!(c.to_string(), canonical, "{text}");
            assert_eq!(serde_json::to_value(c).unwrap(), canonical);
        }
        for bad in [
            "",
            "/8",
            "10.0.0.0/",
            "10.0.0.0/33",
            "2001:db8::/129",
            "10.0.0.0/+8",
            "10.0.0.0/-1",
            "10.0.0/8",
            "example.com",
            "10.0.0.0/8/8",
            "10.0.0.0/256",
            "fe80::1%eth0/128",
        ] {
            assert_eq!(
                bad.parse::<Cidr>(),
                Err(RuleError::BadCidr(bad.into())),
                "{bad:?}"
            );
        }
        // 접두사 밖 비트가 켜진 주소는 받지 않는다(경계: 마지막 비트, 첫 비트).
        for bad in [
            "10.0.0.1/8",
            "10.0.0.1/31",
            "128.0.0.0/0",
            "2001:db8::1/127",
            "8000::/0",
        ] {
            assert_eq!(
                bad.parse::<Cidr>(),
                Err(RuleError::HostBits(bad.into())),
                "{bad:?}"
            );
        }
        for ok in ["10.0.0.2/31", "255.255.255.255/32", "2001:db8::2/127"] {
            assert!(ok.parse::<Cidr>().is_ok(), "{ok}");
        }
        assert!(serde_json::from_value::<Cidr>(json!("10.0.0.1/8")).is_err());
    }

    #[test]
    fn validates_description_and_rule_count() {
        let mut s = group(vec![]);
        s.description = Some("가".repeat(MAX_DESCRIPTION));
        assert_eq!(s.validate(), Ok(()));
        s.description = Some("a".repeat(MAX_DESCRIPTION + 1));
        assert_eq!(s.validate(), Err(SecurityGroupError::LongDescription));

        let any = rule(json!({ "direction": "in", "action": "accept" }));
        assert_eq!(group(vec![any.clone(); MAX_RULES]).validate(), Ok(()));
        assert_eq!(
            group(vec![any; MAX_RULES + 1]).validate(),
            Err(SecurityGroupError::TooManyRules)
        );
    }

    #[test]
    fn validates_each_rule_with_its_number() {
        let ok = rule(json!({ "direction": "in", "action": "accept" }));
        let check = |bad: Rule| group(vec![ok.clone(), ok.clone(), bad]).validate();

        for protocol in [None, Some(Protocol::Icmp), Some(Protocol::Icmpv6)] {
            let mut r = rule(json!({ "direction": "in", "action": "accept", "ports": "22" }));
            r.protocol = protocol;
            assert_eq!(
                check(r),
                Err(SecurityGroupError::Rule(3, RuleError::PortsNeedTcpOrUdp))
            );
        }
        for protocol in ["tcp", "udp"] {
            let r = rule(
                json!({ "direction": "out", "action": "drop", "protocol": protocol, "ports": "1-65535" }),
            );
            assert_eq!(check(r), Ok(()));
        }

        let r = rule(
            json!({ "direction": "in", "action": "accept", "protocol": "icmp", "remote": "::/0" }),
        );
        assert_eq!(
            check(r),
            Err(SecurityGroupError::Rule(3, RuleError::IcmpWithIpv6))
        );
        let r = rule(
            json!({ "direction": "in", "action": "accept", "protocol": "icmpv6", "remote": "10.0.0.0/8" }),
        );
        assert_eq!(
            check(r),
            Err(SecurityGroupError::Rule(3, RuleError::Icmpv6WithIpv4))
        );
        for (protocol, remote) in [("icmp", "10.0.0.0/8"), ("icmpv6", "fd00::/8")] {
            let r = rule(
                json!({ "direction": "in", "action": "accept", "protocol": protocol, "remote": remote }),
            );
            assert_eq!(check(r), Ok(()));
        }

        // 필드를 직접 채운 잘못된 값도 검증에서 걸린다.
        let mut r = ok.clone();
        r.protocol = Some(Protocol::Tcp);
        r.ports = Some(PortRange { start: 10, end: 9 });
        assert_eq!(
            check(r),
            Err(SecurityGroupError::Rule(
                3,
                RuleError::BadPorts("10-9".into())
            ))
        );
        let mut r = ok.clone();
        r.remote = Some(Cidr {
            addr: "10.0.0.1".parse().unwrap(),
            prefix: 8,
        });
        assert_eq!(
            check(r),
            Err(SecurityGroupError::Rule(
                3,
                RuleError::HostBits("10.0.0.1/8".into())
            ))
        );
        let mut r = ok.clone();
        r.remote = Some(Cidr {
            addr: "10.0.0.0".parse().unwrap(),
            prefix: 33,
        });
        assert_eq!(
            check(r),
            Err(SecurityGroupError::Rule(
                3,
                RuleError::BadCidr("10.0.0.0/33".into())
            ))
        );

        assert_eq!(
            SecurityGroupError::Rule(3, RuleError::PortsNeedTcpOrUdp).to_string(),
            "rule 3: ports can only be used with protocol tcp or udp"
        );
    }
}
