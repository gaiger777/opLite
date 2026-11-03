# opLite 계획 26: VM 방화벽 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 보안 그룹으로 VM NIC마다 들어오고 나가는 트래픽을 거른다. 하위 프로젝트 6(네트워크·SDN)의 마지막 계획이다.

**Architecture:** `SecurityGroup` 리소스(순서 있는 허용·차단 규칙)를 NIC에 건다. 각 노드 opld의 방화벽 에이전트가 클러스터의 모든 방화벽 NIC를 tap 이름으로 모아 nftables bridge family 테이블 하나(`bridge opl_<클러스터 태그>`)를 원자적으로 통째로 바꾼다. tap 이름이 VM id에서 나오므로 어느 노드에서든 같은 규칙이 되어, 배치·마이그레이션·HA 복구 중에도 따질 것이 없다. 워커는 방화벽 NIC VM을 켜기 직전에 규칙을 넣고, 넣지 못하면 켜지 않는다(fail closed).

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9, axum 0.8, utoipa, clap, nftables(`nft`); 웹은 계획 8~25와 같다. 새 외부 의존성은 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-03-network-sdn-design.md` (§3과 §4·§5의 방화벽 부분, "구현(계획 26)" 메모 포함)
**선행 계획:** 계획 1~25 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `SecurityGroup` 리소스·API·검증, NIC마다 그룹(8개까지) | 노드(호스트) 방화벽, 클러스터 전체 규칙 |
| nft 규칙 글(상태 추적, MAC 위조 차단, ARP·ND·DHCP 허용, 기본 정책)과 원자적 적용 | IP 위조 차단(IPAM 필요), 다른 그룹을 원격으로 가리키기, 규칙 로그 |
| 노드 방화벽 능력·스케줄러, 에이전트, 켜기 직전 확인(시작·다시 붙이기·다시 돌리기·마이그레이션 받기) | 시작 때 nft 시험이 실패한 노드의 자동 재시험 |
| oplctl·웹, Linux 실제 시험(이름공간 트래픽, opld E2E), README | |

## 이전 계획에서 이어받는 사항

- 계획 25: 클러스터 태그(`overlay::cluster_tag`), `OverlayAgent`의 watch·debounce·종료·leave 방식, `IpRoute::in_netns`와 Linux 전용 시험 방식, `Host::from_config(config, tag)`, `failure_text`.
- 1차 네트워크: tap 이름 `tap_name(vm, i)`, NIC MAC은 VM을 만들 때 정해진다.
- 계획 23·24: 워커의 켜기 직전 확인(HA)과 `IncomingMigrations`, 스케줄러 `node_fits`.

## 실행 환경

계획 25와 같다. nft 규칙을 실제로 적용하는 시험(`opl-driver` `net-it` `tests/firewall.rs`, `opld` `qemu-it,net-it` `tests/firewall.rs`)은 Linux root에서만 돈다 — CI `linux-kvm`이 이 기능들을 돌리고, 이 계획이 그 작업에 `nftables` 꾸러미를 더한다. macOS에서는 이 파일들이 cfg로 빠지므로, 임시로 cfg 줄을 뺀 복사본을 `cargo clippy --features …`로 검사하고 지운다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`).

## 설계 결정

- **모든 노드에 같은 규칙:** 규칙은 tap 이름으로 걸고 클러스터의 모든 방화벽 NIC를 넣는다. 판정 맵으로 tap에서 NIC 체인으로 바로 뛰므로 패킷당 비용은 늘지 않는다.
- **순서 있는 규칙과 기본 정책:** 처음 맞는 규칙을 따르고, 들어오기 기본 차단·나가기 기본 허용. 나가기 체인의 허용은 `return`이라 같은 브리지의 방화벽 VM 둘 사이 패킷은 양쪽 검사를 모두 거친다.
- **fail closed:** 켜기 직전 확인이 실패하거나 노드에 방화벽이 없으면 켜지 않는다. 없는 그룹을 가리키는 NIC는 양방향을 막는다. 시작 때의 능력 시험은 기존 테이블을 건드리지 않는다.
- **도는 tap과 spec을 어긋나게 하지 않는다:** 실행 중인 VM은 NIC 개수·순서·MAC을 바꿀 수 없고, 방화벽 VM은 실행 중에 그룹만 바꿀 수 있다.
- **`meta protocol`로 맞춘다:** VLAN-aware 브리지의 태그 포트에서는 `ether type`이 0x8100으로 보일 수 있어 주소 규칙과 ARP를 `meta protocol ip|ip6|arp`로 쓴다.
- **사용자 글자는 규칙 글에 넣지 않는다:** 이름·설명 대신 tap 이름(검사함)과 검증된 값만.

## 파일 구조

```
crates/opl-types/src/{security_group.rs, resource.rs, vm.rs, node.rs}        # 타입, NicSpec.security_groups, NodeStatus.firewall
crates/opl-api/src/{security_groups.rs, resources.rs, vms.rs, snapshots.rs, scheduler.rs, incoming.rs, watch.rs}, tests/security_groups.rs
crates/opl-driver/src/firewall.rs, tests/firewall.rs, .github/workflows/ci.yml  # 규칙 글·nft 실행기
crates/opl-reconciler/src/{firewall.rs, worker.rs, config.rs}, tests/firewall.rs   # FirewallGate
crates/opld/src/{firewall.rs, host.rs, bootstrap.rs, member.rs}, tests/firewall.rs  # 에이전트, 능력 시험, E2E
crates/oplctl/src/cli.rs, web/src/{security-groups, vms, layout, api, i18n}/...
README.md, docs/superpowers/specs/{2026-11-03-network-sdn-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 보안 그룹 타입

opl-types `security_group.rs`: `SecurityGroupSpec { description, rules }`, `Rule { direction(in|out), action(accept|drop), protocol(tcp|udp|icmp|icmpv6), ports, remote }`. `PortRange`(`"22"`, `"8000-8080"`)와 `Cidr`(`"10.0.0.0/8"`, 호스트 비트 거절)는 JSON 문자열. 검증: 규칙 200개 이하, ports는 tcp·udp에서만, icmp는 IPv4·icmpv6는 IPv6 remote와만, 설명 200자 이하(오류는 "rule N: …"). `Kind::SecurityGroup`(`security-group`). `NicSpec.security_groups`(비면 직렬화에서 뺌)와 `VmSpec::validate`의 NIC당 8개·중복 검사.

**Files:**
- Create: `crates/opl-types/src/security_group.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/resource.rs`
- Modify: `crates/opl-types/src/vm.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/scheduler.rs`에서

```rust
            mac: None,
        };
```

을 다음으로 바꾼다:

```rust
            mac: None,
            security_groups: Vec::new(),
        };
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            mac: None,
        }];
```

을 다음으로 바꾼다:

```rust
            mac: None,
            security_groups: Vec::new(),
        }];
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            mac: None,
        });
```

을 다음으로 바꾼다:

```rust
            mac: None,
            security_groups: Vec::new(),
        });
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
        mac,
    }
```

을 다음으로 바꾼다:

```rust
        mac,
        security_groups: Vec::new(),
    }
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod runtime;
pub mod snapshot;
```

을 다음으로 바꾼다:

```rust
pub mod runtime;
pub mod security_group;
pub mod snapshot;
```

`crates/opl-types/src/resource.rs`에서

```rust
    Snapshot,
}
```

을 다음으로 바꾼다:

```rust
    Snapshot,
    SecurityGroup,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Snapshot => "snapshot",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::Snapshot => "snapshot",
            Kind::SecurityGroup => "security-group",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Snapshot,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::Snapshot,
            Kind::SecurityGroup,
        ] {
```

`crates/opl-types/src/security_group.rs`를 만든다:

```rust
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

    fn check(self) -> Result<(), RuleError> {
        let max = Self::max_prefix(self.addr);
        if self.prefix > max {
            return Err(RuleError::BadCidr(self.to_string()));
        }
        // 접두사 밖(호스트) 비트의 마스크. 128비트를 모두 밀면 넘치므로 그때는 전부.
        let host_mask = 1u128
            .checked_shl(u32::from(max - self.prefix))
            .map_or(u128::MAX, |b| b - 1);
        let bits = match self.addr {
            IpAddr::V4(a) => u128::from(u32::from(a)),
            IpAddr::V6(a) => u128::from(a),
        };
        if bits & host_mask != 0 {
            return Err(RuleError::HostBits(self.to_string()));
        }
        Ok(())
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
```

`crates/opl-types/src/vm.rs`에서

```rust

/// 예약 스냅샷 보관 개수의 최댓값.
```

을 다음으로 바꾼다:

```rust

/// NIC 하나에 거는 보안 그룹의 최대 수.
pub const MAX_NIC_SECURITY_GROUPS: usize = 8;

/// 예약 스냅샷 보관 개수의 최댓값.
```

`crates/opl-types/src/vm.rs`에서

```rust
        }
        if let Some(schedule) = &self.snapshot_schedule
```

을 다음으로 바꾼다:

```rust
        }
        for (i, nic) in self.nics.iter().enumerate() {
            if nic.security_groups.len() > MAX_NIC_SECURITY_GROUPS {
                return Err(ValidationError::TooManySecurityGroups(i));
            }
            let mut groups = BTreeSet::new();
            for group in &nic.security_groups {
                if !groups.insert(group) {
                    return Err(ValidationError::DuplicateSecurityGroup(i, group.clone()));
                }
            }
        }
        if let Some(schedule) = &self.snapshot_schedule
```

`crates/opl-types/src/vm.rs`에서

```rust
    ScheduleKeep,
}
```

을 다음으로 바꾼다:

```rust
    ScheduleKeep,
    #[error("nics[{0}]: a NIC can have at most {MAX_NIC_SECURITY_GROUPS} security groups")]
    TooManySecurityGroups(usize),
    #[error("nics[{0}]: security group {1:?} is listed more than once")]
    DuplicateSecurityGroup(usize, String),
}
```

`crates/opl-types/src/vm.rs`에서

```rust
    pub mac: Option<MacAddr>,
}
```

을 다음으로 바꾼다:

```rust
    pub mac: Option<MacAddr>,
    /// 이 NIC에 거는 보안 그룹 이름(8개 이하, 겹치지 않게). 비어 있지 않으면 방화벽이 켜진다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_groups: Vec<String>,
}
```

`crates/opl-types/src/vm.rs`에서

```rust
    #[test]
    fn status_missing_fields_default() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn validate_checks_nic_security_groups() {
        let with_groups = |groups: &[&str]| {
            let mut s = sample();
            let mut second = s.nics[0].clone();
            second.network = "wan".into();
            second.security_groups = groups.iter().map(|g| g.to_string()).collect();
            s.nics.push(second);
            s.validate()
        };
        assert_eq!(with_groups(&[]), Ok(()));
        let eight: Vec<String> = (0..MAX_NIC_SECURITY_GROUPS)
            .map(|i| format!("g{i}"))
            .collect();
        let eight: Vec<&str> = eight.iter().map(String::as_str).collect();
        assert_eq!(with_groups(&eight), Ok(()));
        let mut nine = eight.clone();
        nine.push("g8");
        assert_eq!(
            with_groups(&nine),
            Err(ValidationError::TooManySecurityGroups(1))
        );
        assert_eq!(
            with_groups(&["web", "ssh", "web"]),
            Err(ValidationError::DuplicateSecurityGroup(1, "web".into()))
        );
        assert_eq!(
            ValidationError::DuplicateSecurityGroup(1, "web".into()).to_string(),
            "nics[1]: security group \"web\" is listed more than once"
        );
        // 다른 NIC에 같은 그룹을 거는 것은 된다.
        let mut s = sample();
        s.nics[0].security_groups = vec!["web".into()];
        let mut second = s.nics[0].clone();
        second.network = "wan".into();
        s.nics.push(second);
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn nic_security_groups_default_empty_and_are_omitted() {
        // 옛 NIC JSON은 그대로 읽고 그대로 쓴다.
        let old = serde_json::json!({ "network": "lan", "model": "virtio", "mac": null });
        let nic: NicSpec = serde_json::from_value(old.clone()).unwrap();
        assert!(nic.security_groups.is_empty());
        assert_eq!(serde_json::to_value(&nic).unwrap(), old);

        let json = serde_json::json!({
            "network": "lan",
            "model": "virtio",
            "mac": null,
            "security_groups": ["web", "ssh"]
        });
        let nic: NicSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(nic.security_groups, ["web", "ssh"]);
        assert_eq!(serde_json::to_value(&nic).unwrap(), json);
    }

    #[test]
    fn status_missing_fields_default() {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 897개, 실패 0

Run: `cargo test -p opl-types`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 330개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(opl-types): 보안 그룹 타입 — SecurityGroup·Rule·PortRange·Cidr와 검증, NicSpec.security_groups"
```

---

### Task 2: 보안 그룹 API

`/security-groups` 목록·만들기·읽기·지우기와 `PATCH`(보낸 필드만, `If-Match`; 쓰기는 admin). VM이 쓰는 그룹은 지울 수 없다(409 `security_group_in_use`, VM 쓰기와 같은 `vm_lock`). VM 만들기·바꾸기·복제에서 없는 그룹과 `user` 네트워크 NIC의 그룹은 400. 실행 중인 VM은 NIC 개수·순서·MAC을 바꿀 수 없고(409 `vm_running`), 방화벽 NIC가 있는(전이나 뒤) 실행 중 VM은 `security_groups` 말고는 NIC를 바꿀 수 없다 — 규칙이 spec의 NIC 순번·MAC으로 tap을 정하므로 도는 tap과 어긋나지 않게. 그룹은 실행 중에도 바꿀 수 있다. watch 종류에 `security-group`.

**Files:**
- Create: `crates/opl-api/src/security_groups.rs`
- Create: `crates/opl-api/tests/security_groups.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/src/watch.rs`
- Modify: `crates/opl-api/tests/clones.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
pub mod scheduler;
mod snapshots;
```

을 다음으로 바꾼다:

```rust
pub mod scheduler;
mod security_groups;
mod snapshots;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(r::get_network, r::delete_network))
        .routes(routes!(r::list_tasks))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(r::get_network, r::delete_network))
        .routes(routes!(
            r::list_security_groups,
            security_groups::create_security_group
        ))
        .routes(routes!(
            r::get_security_group,
            security_groups::patch_security_group,
            security_groups::delete_security_group
        ))
        .routes(routes!(r::list_tasks))
```

`crates/opl-api/src/resources.rs`에서

```rust
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use opl_types::storage::{
```

을 다음으로 바꾼다:

```rust
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use opl_types::security_group::{SecurityGroupSpec, SecurityGroupStatus};
use opl_types::storage::{
```

`crates/opl-api/src/resources.rs`에서

```rust
    "networks"
);
```

을 다음으로 바꾼다:

```rust
    "networks"
);
list_and_get!(
    list_security_groups,
    get_security_group,
    SecurityGroupSpec,
    SecurityGroupStatus,
    "/security-groups",
    "/security-groups/{key}",
    "security-groups"
);
```

`crates/opl-api/src/security_groups.rs`를 만든다:

```rust
//! 보안 그룹 만들기·바꾸기·지우기. 목록과 조회는 `resources`의 공통 핸들러다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::security_group::{Rule, SecurityGroup, SecurityGroupSpec, SecurityGroupStatus};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, check_name, if_match, lookup};
use crate::state::AppState;

fn validate(spec: &SecurityGroupSpec) -> Result<(), ApiError> {
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

/// 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다.
#[utoipa::path(post, path = "/security-groups", tag = "security-groups",
    request_body = CreateRequest<SecurityGroupSpec>,
    responses((status = 201, body = Resource<SecurityGroupSpec, SecurityGroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_security_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<SecurityGroupSpec>>,
) -> Result<(StatusCode, Json<SecurityGroup>), ApiError> {
    state.authorize(&principal, Action::Write, SecurityGroupSpec::KIND)?;
    check_name(&req.name)?;
    validate(&req.spec)?;
    let mut group = Resource::new(req.name, req.spec);
    group.labels = req.labels;
    group.revision = state.cluster.create(&group).await?;
    Ok((StatusCode::CREATED, Json(group)))
}

/// 보안 그룹에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecurityGroupPatch {
    /// 빈 글이면 설명을 지운다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub description: Option<String>,
    /// 규칙 전체를 이것으로 바꾼다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub rules: Option<Vec<Rule>>,
}

/// 설명이나 규칙을 바꾼다. 쓰는 VM의 노드가 곧바로 적용한다. `If-Match`가 없으면 읽은 revision에
/// 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
#[utoipa::path(patch, path = "/security-groups/{key}", tag = "security-groups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = SecurityGroupPatch,
    responses((status = 200, body = Resource<SecurityGroupSpec, SecurityGroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_security_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SecurityGroupPatch>,
) -> Result<Json<SecurityGroup>, ApiError> {
    state.authorize(&principal, Action::Write, SecurityGroupSpec::KIND)?;
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let group = lookup::<SecurityGroupSpec>(&state.cluster, &key)?;
        if let Some(expected) = expected
            && expected != group.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: group.revision,
            })
            .into());
        }
        let mut spec = group.spec.clone();
        if let Some(description) = &req.description {
            spec.description = Some(description.clone()).filter(|d| !d.is_empty());
        }
        if let Some(rules) = &req.rules {
            spec.rules = rules.clone();
        }
        validate(&spec)?;
        if spec == group.spec {
            return Ok(Json(group));
        }
        match state
            .cluster
            .update_spec(group.id, Some(group.revision), group.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        return Ok(Json(lookup::<SecurityGroupSpec>(
            &state.cluster,
            &group.id.to_string(),
        )?));
    }
    Err(ApiError::conflict(
        "busy",
        "the security group kept changing while being updated; try again",
    ))
}

/// VM의 NIC가 쓰고 있으면 409 `security_group_in_use`
#[utoipa::path(delete, path = "/security-groups/{key}", tag = "security-groups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_security_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, SecurityGroupSpec::KIND)?;
    // VM 만들기·바꾸기와 같은 잠금: 검사와 지우기 사이에 그룹을 거는 VM이 생기지 않게.
    let _lock = state.vm_lock.lock().await;
    let group = lookup::<SecurityGroupSpec>(&state.cluster, &key)?;
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    if let Some(vm) = vms.iter().find(|vm| {
        vm.spec
            .nics
            .iter()
            .any(|n| n.security_groups.contains(&group.name))
    }) {
        return Err(ApiError::conflict(
            "security_group_in_use",
            format!(
                "security group {:?} is used by VM {:?}",
                group.name, vm.name
            ),
        ));
    }
    state
        .cluster
        .delete::<SecurityGroupSpec>(group.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
```

`crates/opl-api/src/snapshots.rs`에서

```rust
use crate::tasks::create_task;
use crate::vms::{is_stopped, prepare};
use crate::volumes::{check_pool_on, usable_node};
```

을 다음으로 바꾼다:

```rust
use crate::tasks::create_task;
use crate::vms::{check_security_groups, is_stopped, prepare};
use crate::volumes::{check_pool_on, usable_node};
```

`crates/opl-api/src/snapshots.rs`에서

```rust
    }
    let op = TaskOp::VmClone {
```

을 다음으로 바꾼다:

```rust
    }
    check_security_groups(&state, &spec)?;
    let op = TaskOp::VmClone {
```

`crates/opl-api/src/vms.rs`에서

```rust
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::{VolumePhase, is_shared};
```

을 다음으로 바꾼다:

```rust
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::security_group::SecurityGroupSpec;
use opl_types::storage::{VolumePhase, is_shared};
```

`crates/opl-api/src/vms.rs`에서

```rust
    }
    let mut disks = BTreeSet::new();
```

을 다음으로 바꾼다:

```rust
    }
    check_security_groups(state, spec)?;
    let mut disks = BTreeSet::new();
```

`crates/opl-api/src/vms.rs`에서

```rust
        );
    }
```

을 다음으로 바꾼다:

```rust
        );
    }
    Ok(())
}

/// NIC의 보안 그룹이 모두 있고, 그룹을 건 NIC가 `user` 네트워크가 아닌지. 아니면 400.
pub(crate) fn check_security_groups(state: &AppState, spec: &VmSpec) -> Result<(), ApiError> {
    if spec.nics.iter().all(|n| n.security_groups.is_empty()) {
        return Ok(());
    }
    let (_, groups) = state.cluster.list::<SecurityGroupSpec>()?;
    for (i, nic) in spec.nics.iter().enumerate() {
        if nic.security_groups.is_empty() {
            continue;
        }
        // user 네트워크는 QEMU 안의 NAT라 노드의 방화벽 규칙이 닿지 않는다.
        if nic.network == USER_NETWORK {
            return Err(ApiError::bad_request(format!(
                "NIC {i}: security groups cannot be used on the {USER_NETWORK:?} network"
            )));
        }
        for name in &nic.security_groups {
            if !groups.iter().any(|g| &g.name == name) {
                return Err(ApiError::bad_request(format!(
                    "security group {name:?} not found"
                )));
            }
        }
    }
```

`crates/opl-api/src/vms.rs`에서

```rust

/// spec 전체를 바꾼다. 실행 중인 VM의 `node`와 `disks`는 바꿀 수 없고 `ha`를 켤 수 없다. 다른
/// 변경은 다음 시작부터 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 검증한다.
#[utoipa::path(put, path = "/vms/{key}", tag = "vms",
```

을 다음으로 바꾼다:

```rust

/// spec 전체를 바꾼다. 실행 중인 VM의 `node`와 `disks`는 바꿀 수 없고 `ha`를 켤 수 없다. 보안 그룹을
/// 건 NIC가 있으면 실행 중에는 NIC의 `security_groups`만 바꿀 수 있다. NIC의
/// `security_groups`는 곧바로, 다른 변경은 다음 시작부터 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 검증한다.
#[utoipa::path(put, path = "/vms/{key}", tag = "vms",
```

`crates/opl-api/src/vms.rs`에서

```rust
            return Err(vm_running(&current, "change the disks of"));
        }
```

을 다음으로 바꾼다:

```rust
            return Err(vm_running(&current, "change the disks of"));
        }
        // 방화벽 규칙은 spec의 NIC 순번·MAC으로 tap을 정한다. 실행 중에 NIC 구성이 바뀌면 도는 tap과
        // 어긋나 막히지 않은 채가 되므로, 방화벽 NIC가 있으면(전이나 뒤) 그룹만 바꿀 수 있다.
        let firewalled = |s: &VmSpec| s.nics.iter().any(|n| !n.security_groups.is_empty());
        let without_groups = |s: &VmSpec| {
            let mut nics = s.nics.clone();
            nics.iter_mut().for_each(|n| n.security_groups.clear());
            nics
        };
        if (firewalled(&spec) || firewalled(&current.spec))
            && without_groups(&spec) != without_groups(&current.spec)
            && !is_stopped(&current)
        {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "cannot change the NICs of VM {:?} while it is running; only security groups can change",
                    current.name
                ),
            ));
        }
        // 방화벽이 없어도 NIC 개수·순서·MAC은 그대로 둔다. 그룹 없이 바꾼 뒤 그룹을 걸면 위 검사를 비켜
        // 규칙의 tap이 도는 tap과 어긋난다.
        let macs = |s: &VmSpec| s.nics.iter().map(|n| n.mac).collect::<Vec<_>>();
        if macs(&spec) != macs(&current.spec) && !is_stopped(&current) {
            return Err(vm_running(
                &current,
                "change the number, order or MAC addresses of the NICs of",
            ));
        }
```

`crates/opl-api/src/watch.rs`에서

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 7] = [
    Kind::Vm,
```

을 다음으로 바꾼다:

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 8] = [
    Kind::Vm,
```

`crates/opl-api/src/watch.rs`에서

```rust
    Kind::Snapshot,
];
```

을 다음으로 바꾼다:

```rust
    Kind::Snapshot,
    Kind::SecurityGroup,
];
```

`crates/opl-api/src/watch.rs`에서

```rust
        assert!(parse_kinds("user").is_err());
    }
}
```

을 다음으로 바꾼다:

```rust
        assert!(parse_kinds("user").is_err());
    }

    #[test]
    fn security_groups_are_watchable() {
        let kinds = parse_kinds("security-group").unwrap();
        assert!(kinds.contains(&Kind::SecurityGroup));
    }
}
```

`crates/opl-api/tests/clones.rs`에서

```rust
            .is_some_and(|m| m.contains("network \"lan\" not found")),
        "{body}"
```

을 다음으로 바꾼다:

```rust
            .is_some_and(|m| m.contains("network \"lan\" not found")),
        "{body}"
    );
}

/// 새 VM은 원본 스펙의 NIC 보안 그룹도 가지므로 그 그룹이 모두 있어야 한다(VM 만들기와 같은 검사).
#[tokio::test]
async fn clone_needs_the_security_groups_of_the_source_spec() {
    let env = env().await;
    let admin = env.admin().await;
    admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    let (status, body) = admin
        .post("/security-groups", json!({ "name": "web", "spec": {} }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut req = vm_request("web");
    req["spec"]["nics"] = json!([
        { "network": "lan", "model": "virtio", "security_groups": ["web"] }
    ]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    finish_tasks(&env).await;

    // API는 쓰는 그룹을 지우지 않으므로 저장소에서 바로 지운다(스냅샷 스펙이 옛 그룹을 가진 때와 같다).
    let web = env
        .cluster
        .get_by_name::<opl_types::security_group::SecurityGroupSpec>("web")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<opl_types::security_group::SecurityGroupSpec>(web.id, None)
        .await
        .unwrap();
    let (status, body) = clone(&admin, "web", json!({ "name": "web-3" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|m| m.contains("security group \"web\" not found")),
        "{body}"
```

`crates/opl-api/tests/security_groups.rs`를 만든다:

```rust
//! 보안 그룹 API: 관리자만 만들고 바꾸고 지우며, VM의 브리지 네트워크 NIC가 쓰고, 쓰는 동안 지울 수 없다.

mod common;

use common::{Client, Env, vm_request};
use opl_types::node::{BridgeInfo, NodeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn group(name: &str, spec: Value) -> Value {
    json!({ "name": name, "spec": spec })
}

fn ssh() -> Value {
    json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "22", "remote": "10.0.0.0/8" })
}

/// `lan` 브리지 네트워크 NIC 하나에 그룹을 건 VM 요청
fn vm_with_groups(name: &str, groups: Value) -> Value {
    let mut vm = vm_request(name);
    vm["spec"]["nics"][0] =
        json!({ "network": "lan", "model": "virtio", "security_groups": groups });
    vm
}

#[tokio::test]
async fn admins_manage_security_groups_and_others_read() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, body) = admin
        .post(
            "/security-groups",
            group(
                "web",
                json!({ "description": "web servers", "rules": [ssh()] }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["rules"][0], ssh());
    assert_eq!(body["spec"]["description"], "web servers");
    // 규칙이 없어도 된다(들어오기 모두 차단).
    let (status, body) = admin
        .post("/security-groups", group("closed", json!({})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"], json!({ "rules": [] }));

    assert_eq!(
        operator
            .post("/security-groups", group("db", json!({})))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator.delete("/security-groups/web").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, list) = viewer.get("/security-groups").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(
        names.contains(&"web") && names.contains(&"closed"),
        "{list}"
    );
    let (status, body) = viewer.get("/security-groups/web").await;
    assert_eq!(status, StatusCode::OK);
    let id = body["id"].as_str().unwrap().to_owned();
    assert_eq!(
        viewer.get(&format!("/security-groups/{id}")).await.1["name"],
        "web"
    );
    assert_eq!(
        viewer.get("/security-groups/nope").await.0,
        StatusCode::NOT_FOUND
    );

    let icmp_ports =
        json!({ "direction": "in", "action": "accept", "protocol": "icmp", "ports": "22" });
    for (bad, why) in [
        (group("Bad_Name", json!({})), "name"),
        (
            group("long", json!({ "description": "x".repeat(201) })),
            "description",
        ),
        (
            group("many", json!({ "rules": vec![ssh(); 201] })),
            "rule count",
        ),
        (
            group("ports", json!({ "rules": [ssh(), icmp_ports] })),
            "rule 2",
        ),
        (
            group(
                "cidr",
                json!({ "rules": [{ "direction": "in", "action": "drop", "remote": "10.0.0.1/8" }] }),
            ),
            "host bits",
        ),
        (group("extra", json!({ "colour": "red" })), "unknown field"),
    ] {
        let (status, body) = admin.post("/security-groups", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request", "{why}: {body}");
        if why == "rule 2" {
            assert!(
                body["message"].as_str().unwrap().contains("rule 2"),
                "{body}"
            );
        }
    }
    let (status, body) = admin
        .post("/security-groups", group("web", json!({})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "name_taken");

    assert_eq!(
        admin.delete("/security-groups/closed").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        admin.get("/security-groups/closed").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn patch_changes_rules_and_description() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let (_, created) = admin
        .post(
            "/security-groups",
            group(
                "web",
                json!({ "description": "web servers", "rules": [ssh()] }),
            ),
        )
        .await;
    let revision = created["revision"].as_u64().unwrap();

    // 주어진 필드만 바꾼다.
    let http = json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "80" });
    let (status, body) = admin
        .patch(
            "/security-groups/web",
            json!({ "rules": [ssh(), http] }),
            Some(revision),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["rules"], json!([ssh(), http]));
    assert_eq!(body["spec"]["description"], "web servers");
    let changed = body["revision"].as_u64().unwrap();
    assert!(changed > revision);
    assert_eq!(admin.get("/security-groups/web").await.1, body);

    // 옛 revision이면 412
    let (status, body) = admin
        .patch(
            "/security-groups/web",
            json!({ "rules": [] }),
            Some(revision),
        )
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_eq!(body["code"], "revision_mismatch");

    // 빈 설명은 설명을 지운다. If-Match가 없으면 지금 revision에 쓴다.
    let (status, body) = admin
        .patch(
            "/security-groups/web",
            json!({ "description": "", "rules": [] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"], json!({ "rules": [] }));

    for (bad, why) in [
        (json!({ "description": "x".repeat(201) }), "description"),
        (
            json!({ "rules": [{ "direction": "out", "action": "drop", "protocol": "udp", "ports": "9-8" }] }),
            "ports",
        ),
        (json!({ "name": "other" }), "unknown field"),
    ] {
        let (status, body) = admin.patch("/security-groups/web", bad, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request", "{why}: {body}");
    }
    assert_eq!(
        admin
            .patch("/security-groups/nope", json!({ "rules": [] }), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        viewer
            .patch("/security-groups/web", json!({ "rules": [] }), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn vms_use_existing_groups_which_cannot_be_deleted_while_used() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    admin
        .post(
            "/security-groups",
            group("web", json!({ "rules": [ssh()] })),
        )
        .await;

    // 없는 그룹
    let (status, body) = admin
        .post("/vms", vm_with_groups("vm1", json!(["web", "db"])))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"].as_str().unwrap().contains("\"db\""),
        "{body}"
    );
    // user 네트워크에는 걸 수 없다.
    let mut user = vm_request("vm1");
    user["spec"]["nics"][0]["security_groups"] = json!(["web"]);
    let (status, body) = admin.post("/vms", user).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(body["message"].as_str().unwrap().contains("user"), "{body}");
    // 개수·중복은 spec 검증에서 걸린다.
    let (status, body) = admin
        .post("/vms", vm_with_groups("vm1", json!(["web", "web"])))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = admin
        .post("/vms", vm_with_groups("vm1", json!(["web"])))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["nics"][0]["security_groups"], json!(["web"]));

    // VM을 바꿀 때도 같은 검사를 한다.
    let mut spec = body["spec"].clone();
    spec["nics"][0]["security_groups"] = json!(["web", "db"]);
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("\"db\""),
        "{body}"
    );

    let (status, body) = admin.delete("/security-groups/web").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "security_group_in_use");
    assert_eq!(
        body["message"],
        "security group \"web\" is used by VM \"vm1\""
    );

    assert_eq!(admin.delete("/vms/vm1").await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        admin.delete("/security-groups/web").await.0,
        StatusCode::NO_CONTENT
    );
}

/// `lan` 브리지 네트워크(노드에 그 브리지가 있다)와 규칙 없는 그룹 `web`을 만든다.
async fn lan_and_web(env: &Env, admin: &Client) {
    admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    admin
        .post("/security-groups", group("web", json!({})))
        .await;
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status.clone();
    status.bridges = vec![BridgeInfo {
        name: "vmbr0".into(),
        vlan_filtering: false,
    }];
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn groups_of_a_running_vm_can_change() {
    let env = Env::start().await;
    let admin = env.admin().await;
    lan_and_web(&env, &admin).await;
    assert_eq!(
        admin.post("/vms", vm_with_groups("vm1", json!([]))).await.0,
        StatusCode::CREATED
    );
    let task = admin.act("vm1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let (_, vm) = admin.get("/vms/vm1").await;
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["security_groups"] = json!(["web"]);
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["nics"][0]["security_groups"], json!(["web"]));
    assert_eq!(body["spec"]["power"], "running");
}

/// 실행 중인 VM은 방화벽 NIC가 없어도 NIC 개수·순서·MAC을 바꿀 수 없다. 그룹 없이 NIC 순서를 바꾼 뒤
/// 그룹을 거는 두 번의 바꾸기로 규칙의 tap(순번·MAC)이 도는 tap과 어긋나지 않게.
#[tokio::test]
async fn running_vms_keep_the_number_order_and_macs_of_their_nics() {
    let env = Env::start().await;
    let admin = env.admin().await;
    lan_and_web(&env, &admin).await;
    let mut req = vm_with_groups("vm1", json!([]));
    req["spec"]["nics"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "network": "lan", "model": "virtio" }));
    assert_eq!(admin.post("/vms", req).await.0, StatusCode::CREATED);
    let task = admin.act("vm1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/vm1").await;
    let nics = vm["spec"]["nics"].as_array().unwrap().clone();

    let mut swapped = vm["spec"].clone();
    swapped["nics"] = json!([nics[1], nics[0]]);
    let mut removed = vm["spec"].clone();
    removed["nics"] = json!([nics[0]]);
    let mut added = vm["spec"].clone();
    added["nics"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "network": "user", "model": "virtio" }));
    let mut mac = vm["spec"].clone();
    mac["nics"][1]["mac"] = json!("52:54:00:aa:bb:cc");
    let mut no_mac = vm["spec"].clone();
    no_mac["nics"][1].as_object_mut().unwrap().remove("mac");
    for (spec, why) in [
        (swapped, "order"),
        (removed, "removed"),
        (added, "added"),
        (mac, "mac"),
        (no_mac, "mac left out"),
    ] {
        let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{why}: {body}");
        assert_eq!(body["code"], "vm_running", "{why}: {body}");
        assert_eq!(
            body["message"],
            "cannot change the number, order or MAC addresses of the NICs of VM \"vm1\" while it is running; stop it first",
            "{why}"
        );
    }
    // 그 뒤에 그룹을 걸어도 NIC는 그대로다.
    let (_, vm) = admin.get("/vms/vm1").await;
    assert_eq!(vm["spec"]["nics"], json!(nics));

    // model·network는 지금처럼 바꾼다(다음 시작부터).
    let mut spec = vm["spec"].clone();
    spec["nics"][1]["model"] = json!("e1000e");
    spec["nics"][1]["network"] = json!("user");
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 방화벽 NIC가 있는(바꾸기 전이나 뒤) 실행 중인 VM은 NIC의 그룹만 바꿀 수 있다. 규칙이 NIC 순번·MAC으로
/// tap을 정하므로 다른 NIC 변경은 도는 tap과 어긋난다.
#[tokio::test]
async fn running_vms_with_groups_change_only_the_groups_of_their_nics() {
    let env = Env::start().await;
    let admin = env.admin().await;
    lan_and_web(&env, &admin).await;
    let mut req = vm_with_groups("vm1", json!([]));
    req["spec"]["nics"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "network": "lan", "model": "virtio" }));
    assert_eq!(admin.post("/vms", req).await.0, StatusCode::CREATED);
    let task = admin.act("vm1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // 켠 뒤 첫 NIC에 그룹을 건다.
    let (_, vm) = admin.get("/vms/vm1").await;
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["security_groups"] = json!(["web"]);
    let (status, vm) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{vm}");
    let nics = vm["spec"]["nics"].as_array().unwrap().clone();

    let mut swapped = vm["spec"].clone();
    swapped["nics"] = json!([nics[1], nics[0]]);
    let mut model = vm["spec"].clone();
    model["nics"][1]["model"] = json!("e1000e");
    let mut ungrouped_model = model.clone();
    ungrouped_model["nics"][0]["security_groups"] = json!([]);
    for (spec, why) in [
        (swapped, "order"),
        (model, "model"),
        (ungrouped_model, "groups removed with another change"),
    ] {
        let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{why}: {body}");
        assert_eq!(body["code"], "vm_running", "{why}: {body}");
        assert_eq!(
            body["message"],
            "cannot change the NICs of VM \"vm1\" while it is running; only security groups can change"
        );
    }

    // 그룹만 옮기면 된다.
    let mut moved = vm["spec"].clone();
    moved["nics"][0]["security_groups"] = json!([]);
    moved["nics"][1]["security_groups"] = json!(["web"]);
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": moved }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 방화벽 NIC가 없는 VM은 지금처럼 실행 중에도 NIC의 model·network를 바꾼다(다음 시작부터). 그룹을
    // 걸면서는 안 된다.
    assert_eq!(
        admin.post("/vms", vm_with_groups("vm2", json!([]))).await.0,
        StatusCode::CREATED
    );
    let task = admin.act("vm2", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/vm2").await;
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["model"] = json!("e1000e");
    let (status, body) = admin
        .put("/vms/vm2", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut mac = spec.clone();
    mac["nics"][0]["mac"] = json!("52:54:00:aa:bb:cc");
    let (status, body) = admin.put("/vms/vm2", json!({ "spec": mac }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running", "{body}");
    spec["nics"][0]["model"] = json!("virtio");
    spec["nics"][0]["security_groups"] = json!(["web"]);
    let (status, body) = admin.put("/vms/vm2", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running", "{body}");
}

#[tokio::test]
async fn security_groups_are_documented() {
    let env = Env::start().await;
    let (_, doc) = env.anonymous().get("/openapi.json").await;
    let paths = &doc["paths"];
    for method in ["get", "post"] {
        assert!(paths["/security-groups"][method].is_object(), "{method}");
    }
    for method in ["get", "patch", "delete"] {
        assert!(
            paths["/security-groups/{key}"][method].is_object(),
            "{method}"
        );
    }
    assert_eq!(
        paths["/security-groups/{key}"]["patch"]["requestBody"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/SecurityGroupPatch"
    );
    // 포트와 CIDR은 문자열로 보인다.
    let rule = &doc["components"]["schemas"]["Rule"]["properties"];
    assert_eq!(rule["ports"]["type"], "string", "{rule}");
    assert_eq!(rule["remote"]["type"], "string", "{rule}");
    assert!(
        doc["components"]["schemas"]["Resource_SecurityGroupSpec_SecurityGroupStatus"].is_object()
    );
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 906개, 실패 0

Run: `cargo test -p opl-api --test security_groups --test clones`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 330개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(opl-api): 보안 그룹 API — /security-groups 만들기·목록·읽기·PATCH·지우기, VM NIC 그룹 검사"
```

---

### Task 3: nft 규칙 글과 실행기

opl-driver `firewall.rs`: `ruleset(tag, nics)`가 테이블 `bridge opl_<tag>`를 지우고 다시 만드는 글 하나를 만든다(같은 입력이면 같은 글, tap 이름 순, 같은 tap은 첫 것만). NIC마다 나가기 체인 `o_<tap>`(MAC 위조 차단, ARP·이미 맺은 연결 return, invalid drop, out 규칙 — accept는 `return`) 과 들어오기 체인 `i_<tap>`(ARP·IPv6 이웃 찾기·DHCP 응답·이미 맺은 연결 허용, invalid drop, in 규칙, 끝 drop), 판정 맵 `out_nics`·`in_nics`, 훅 체인 `forward`·`input`·`output`. 주소 규칙과 ARP는 `meta protocol ip|ip6|arp`로 맞춘다(VLAN-aware 브리지의 태그 포트에서 `ether type`이 0x8100으로 보이는 경우). `Firewall::apply`, `Nft`(`nft -f -`, 시험용 `Nft::in_netns`). Linux `net-it` 시험 `tests/firewall.rs`: 이름공간 브리지 위 "VM" 둘 사이에서 허용·차단 TCP, ping, 나가기 차단, MAC 위조, 허용 목록 + 끝 drop에서도 ARP, 곧바로 반영, 잘못된 글은 옛 규칙 유지, VLAN 브리지의 주소 규칙. CI `linux-kvm`에 `nftables`, iSCSI by-path 링크 기다리기.

**Files:**
- Create: `crates/opl-driver/src/firewall.rs`
- Create: `crates/opl-driver/tests/firewall.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opl-driver/Cargo.toml`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/net.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
      - uses: dtolnay/rust-toolchain@stable
      - name: KVM, QEMU, UEFI firmware, iproute2
        run: |
```

을 다음으로 바꾼다:

```yaml
      - uses: dtolnay/rust-toolchain@stable
      - name: KVM, QEMU, UEFI firmware, iproute2, nftables
        run: |
```

`.github/workflows/ci.yml`에서

```yaml
          sudo apt-get update
          sudo apt-get install -y libssl-dev pkg-config qemu-system-x86 ovmf qemu-utils iproute2
          ls -l /dev/kvm
```

을 다음으로 바꾼다:

```yaml
          sudo apt-get update
          sudo apt-get install -y libssl-dev pkg-config qemu-system-x86 ovmf qemu-utils iproute2 nftables
          ls -l /dev/kvm
```

`.github/workflows/ci.yml`에서

```yaml
          DEV="/dev/disk/by-path/ip-127.0.0.1:3260-iscsi-$IQN-lun-0"
          ls -l "$DEV"
```

을 다음으로 바꾼다:

```yaml
          DEV="/dev/disk/by-path/ip-127.0.0.1:3260-iscsi-$IQN-lun-0"
          # udev가 by-path 링크를 settle 뒤에 조금 늦게 만들 때가 있다.
          for _ in $(seq 30); do [ -e "$DEV" ] && break; sleep 1; done
          ls -l "$DEV"
```

`crates/opl-driver/Cargo.toml`에서

```toml
qemu-it = []
# 실제 tap과 브리지를 만드는 통합 테스트. Linux에서 root(CAP_NET_ADMIN)와 iproute2가 필요하다.
net-it = []
```

을 다음으로 바꾼다:

```toml
qemu-it = []
# 실제 tap과 브리지, nft 규칙을 만드는 통합 테스트. Linux에서 root(CAP_NET_ADMIN)와 iproute2, nftables, python3가 필요하다.
net-it = []
```

`crates/opl-driver/src/firewall.rs`를 만든다:

```rust
//! VM NIC의 방화벽(보안 그룹). 클러스터의 방화벽 NIC를 nftables bridge family 규칙 글 하나로 만들고
//! `nft -f -`로 한 번에(원자적으로) 넣는다. opld가 root(또는 CAP_NET_ADMIN)로 돌아야 한다.

use std::net::IpAddr;

use async_trait::async_trait;
use opl_types::security_group::{Action, Direction, Protocol, Rule};
use opl_types::vm::MacAddr;
use tokio::io::AsyncWriteExt;

use crate::error::DriverError;
use crate::net::failure_text;

/// 방화벽이 켜진 NIC 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirewallNic {
    /// NIC의 tap 장치 이름
    pub tap: String,
    /// NIC의 MAC. 나가는 패킷의 보낸 MAC이 이것이 아니면 막는다.
    pub mac: MacAddr,
    /// NIC 그룹 순서, 그룹 안 순서로 펼친 규칙
    pub rules: Vec<Rule>,
}

/// 클러스터 태그 `tag`의 테이블 `bridge opl_<tag>`를 `nics`로 바꾸는 nft 글. 테이블을 (없으면) 만들고
/// 지운 뒤 새로 정의하므로 한 번에 넣으면 원자적으로 바뀐다. 방화벽 NIC가 없으면 지우기만 한다.
/// 사용자 글자(이름·설명)는 넣지 않고, tap 이름이 `[a-z0-9]`가 아닌 NIC와 앞 NIC와 tap 이름이 같은
/// NIC는 건너뛴다.
/// NIC를 tap 이름 순으로 정렬하므로 같은 입력이면 같은 글이다.
pub fn ruleset(tag: &str, nics: &[FirewallNic]) -> String {
    let table = format!("bridge opl_{tag}");
    let mut out = format!("table {table} {{}}\ndelete table {table}\n");
    let mut nics: Vec<&FirewallNic> = nics
        .iter()
        .filter(|nic| {
            let ok = safe_ifname(&nic.tap);
            if !ok {
                tracing::warn!(tap = %nic.tap, "skipping a firewall NIC with an unexpected tap name");
            }
            ok
        })
        .collect();
    if nics.is_empty() {
        return out;
    }
    // 같은 tap 이름이 둘이면(맵 원소가 겹쳐 글 전체가 실패한다) 첫 것만 쓴다. 정렬은 안정적이다.
    nics.sort_by(|a, b| a.tap.cmp(&b.tap));
    nics.dedup_by(|later, first| {
        let same = later.tap == first.tap;
        if same {
            tracing::warn!(tap = %later.tap, "skipping a firewall NIC with a duplicate tap name");
        }
        same
    });

    out.push_str(&format!("table {table} {{\n"));
    // 맵이 가리키는 체인, 훅 체인이 가리키는 맵을 먼저 적는다.
    for nic in &nics {
        out.push_str(&format!("\tchain o_{} {{\n", nic.tap));
        for line in [
            &format!("ether saddr != {} drop", nic.mac),
            "meta protocol arp return",
            "ct state established,related return",
            "ct state invalid drop",
        ] {
            out.push_str(&format!("\t\t{line}\n"));
        }
        for rule in nic.rules.iter().filter(|r| r.direction == Direction::Out) {
            out.push_str(&format!("\t\t{}\n", rule_line(rule)));
        }
        out.push_str("\t}\n");

        out.push_str(&format!("\tchain i_{} {{\n", nic.tap));
        for line in [
            "meta protocol arp accept",
            "icmpv6 type { nd-neighbor-solicit, nd-neighbor-advert, nd-router-advert } accept",
            "udp sport 67 udp dport 68 accept",
            "udp sport 547 udp dport 546 accept",
            "ct state established,related accept",
            "ct state invalid drop",
        ] {
            out.push_str(&format!("\t\t{line}\n"));
        }
        for rule in nic.rules.iter().filter(|r| r.direction == Direction::In) {
            out.push_str(&format!("\t\t{}\n", rule_line(rule)));
        }
        out.push_str("\t\tdrop\n\t}\n");
    }
    for (map, prefix) in [("out_nics", "o_"), ("in_nics", "i_")] {
        let elements: Vec<String> = nics
            .iter()
            .map(|nic| format!("\"{}\" : jump {prefix}{}", nic.tap, nic.tap))
            .collect();
        out.push_str(&format!(
            "\tmap {map} {{\n\t\ttype ifname : verdict\n\t\telements = {{ {} }}\n\t}}\n",
            elements.join(", ")
        ));
    }
    for (hook, lines) in [
        (
            "forward",
            &["iifname vmap @out_nics", "oifname vmap @in_nics"][..],
        ),
        ("input", &["iifname vmap @out_nics"][..]),
        ("output", &["oifname vmap @in_nics"][..]),
    ] {
        out.push_str(&format!(
            "\tchain {hook} {{\n\t\ttype filter hook {hook} priority 0; policy accept;\n"
        ));
        for line in lines {
            out.push_str(&format!("\t\t{line}\n"));
        }
        out.push_str("\t}\n");
    }
    out.push_str("}\n");
    out
}

/// nft 글에 그대로 넣어도 되는 장치 이름(tap 이름 모양 `op…`, 1~15자 `[a-z0-9]`)
fn safe_ifname(name: &str) -> bool {
    (1..=15).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// 규칙 하나 → nft 규칙 한 줄. 나가기 체인의 허용은 `return`(받는 쪽 들어오기 검사가 남는다).
fn rule_line(rule: &Rule) -> String {
    let mut parts = Vec::new();
    if let Some(remote) = rule.remote {
        let field = match rule.direction {
            Direction::In => "saddr",
            Direction::Out => "daddr",
        };
        // 주소 체계는 `ether type`(nft가 `ip daddr`에 저절로 붙이는 조건) 대신 `meta protocol`로 본다.
        // VLAN-aware 브리지에서 태그가 붙은 프레임은 `ether type`이 802.1Q로 보일 수 있다. 접두사
        // 0(`0.0.0.0/0`, `::/0`)은 그 주소 체계의 모든 패킷이다.
        parts.push(match (remote.addr, remote.prefix) {
            (IpAddr::V4(_), 0) => "meta protocol ip".to_string(),
            (IpAddr::V6(_), 0) => "meta protocol ip6".to_string(),
            (IpAddr::V4(_), _) => format!("meta protocol ip ip {field} {remote}"),
            (IpAddr::V6(_), _) => format!("meta protocol ip6 ip6 {field} {remote}"),
        });
    }
    if let Some(protocol) = rule.protocol {
        parts.push(
            match protocol {
                Protocol::Tcp => "meta l4proto tcp",
                Protocol::Udp => "meta l4proto udp",
                Protocol::Icmp => "meta l4proto icmp",
                Protocol::Icmpv6 => "meta l4proto ipv6-icmp",
            }
            .to_string(),
        );
    }
    if let Some(ports) = rule.ports {
        parts.push(format!("th dport {ports}"));
    }
    parts.push(
        match (rule.direction, rule.action) {
            (Direction::In, Action::Accept) => "accept",
            (Direction::Out, Action::Accept) => "return",
            (_, Action::Drop) => "drop",
        }
        .to_string(),
    );
    parts.join(" ")
}

/// 방화벽 규칙 글을 넣는다. 테스트에서 바꿔 끼울 수 있게 트레이트로 둔다.
#[async_trait]
pub trait Firewall: Send + Sync + std::fmt::Debug {
    /// `ruleset`이 만든 글을 한 번에 넣는다.
    async fn apply(&self, ruleset: &str) -> Result<(), DriverError>;
}

/// `nft -f -`로 규칙 글을 넣는다.
#[derive(Debug, Default)]
pub struct Nft {
    /// 있으면 이 네트워크 이름공간에서 `nft`를 부른다(`ip netns exec NAME`, 시험용).
    netns: Option<String>,
}

impl Nft {
    pub fn in_netns(name: impl Into<String>) -> Self {
        Self {
            netns: Some(name.into()),
        }
    }
}

#[async_trait]
impl Firewall for Nft {
    async fn apply(&self, ruleset: &str) -> Result<(), DriverError> {
        let mut cmd = match &self.netns {
            Some(ns) => {
                let mut cmd = tokio::process::Command::new("ip");
                cmd.args(["netns", "exec", ns, "nft", "-f", "-"]);
                cmd
            }
            None => {
                let mut cmd = tokio::process::Command::new("nft");
                cmd.args(["-f", "-"]);
                cmd
            }
        };
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        // 쓰다 실패하면(nft가 먼저 끝남) 종료 상태와 stderr가 까닭을 말한다.
        let _ = stdin.write_all(ruleset.as_bytes()).await;
        drop(stdin);
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(DriverError::ToolFailed {
                tool: "nft",
                stderr: failure_text(&output.stderr, output.status),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "table bridge opl_abc {}\ndelete table bridge opl_abc\n";

    /// 내장 체인(나가기 앞부분)
    fn out_head(mac: &str) -> String {
        format!(
            "\t\tether saddr != {mac} drop\n\
             \t\tmeta protocol arp return\n\
             \t\tct state established,related return\n\
             \t\tct state invalid drop\n"
        )
    }

    const IN_HEAD: &str = "\t\tmeta protocol arp accept\n\
        \t\ticmpv6 type { nd-neighbor-solicit, nd-neighbor-advert, nd-router-advert } accept\n\
        \t\tudp sport 67 udp dport 68 accept\n\
        \t\tudp sport 547 udp dport 546 accept\n\
        \t\tct state established,related accept\n\
        \t\tct state invalid drop\n";

    const HOOKS: &str = "\tchain forward {\n\
        \t\ttype filter hook forward priority 0; policy accept;\n\
        \t\tiifname vmap @out_nics\n\
        \t\toifname vmap @in_nics\n\
        \t}\n\
        \tchain input {\n\
        \t\ttype filter hook input priority 0; policy accept;\n\
        \t\tiifname vmap @out_nics\n\
        \t}\n\
        \tchain output {\n\
        \t\ttype filter hook output priority 0; policy accept;\n\
        \t\toifname vmap @in_nics\n\
        \t}\n";

    fn rule(json: &str) -> Rule {
        serde_json::from_str(json).unwrap()
    }

    fn nic(tap: &str, mac: &str, rules: Vec<Rule>) -> FirewallNic {
        FirewallNic {
            tap: tap.into(),
            mac: mac.parse().unwrap(),
            rules,
        }
    }

    #[test]
    fn no_nics_only_deletes_the_table() {
        assert_eq!(ruleset("abc", &[]), HEAD);
    }

    #[test]
    fn one_nic_without_rules_drops_everything_in() {
        let text = ruleset("abc", &[nic("op0123456789n0", "52:54:00:aa:bb:cc", vec![])]);
        let expected = format!(
            "{HEAD}table bridge opl_abc {{\n\
             \tchain o_op0123456789n0 {{\n{out}\t}}\n\
             \tchain i_op0123456789n0 {{\n{IN_HEAD}\t\tdrop\n\t}}\n\
             \tmap out_nics {{\n\
             \t\ttype ifname : verdict\n\
             \t\telements = {{ \"op0123456789n0\" : jump o_op0123456789n0 }}\n\
             \t}}\n\
             \tmap in_nics {{\n\
             \t\ttype ifname : verdict\n\
             \t\telements = {{ \"op0123456789n0\" : jump i_op0123456789n0 }}\n\
             \t}}\n\
             {HOOKS}}}\n",
            out = out_head("52:54:00:aa:bb:cc"),
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn two_nics_are_sorted_by_tap_name() {
        let a = nic(
            "opbbbbbbbbbbn0",
            "52:54:00:00:00:02",
            vec![rule(
                r#"{"direction":"in","action":"accept","protocol":"tcp","ports":"22"}"#,
            )],
        );
        let b = nic(
            "opaaaaaaaaaan1",
            "52:54:00:00:00:01",
            vec![rule(
                r#"{"direction":"out","action":"drop","protocol":"tcp","ports":"25"}"#,
            )],
        );
        let text = ruleset("abc", &[a.clone(), b.clone()]);
        // 입력 순서가 달라도 같은 글
        assert_eq!(text, ruleset("abc", &[b, a]));
        let expected = format!(
            "{HEAD}table bridge opl_abc {{\n\
             \tchain o_opaaaaaaaaaan1 {{\n{out1}\t\tmeta l4proto tcp th dport 25 drop\n\t}}\n\
             \tchain i_opaaaaaaaaaan1 {{\n{IN_HEAD}\t\tdrop\n\t}}\n\
             \tchain o_opbbbbbbbbbbn0 {{\n{out2}\t}}\n\
             \tchain i_opbbbbbbbbbbn0 {{\n{IN_HEAD}\t\tmeta l4proto tcp th dport 22 accept\n\t\tdrop\n\t}}\n\
             \tmap out_nics {{\n\
             \t\ttype ifname : verdict\n\
             \t\telements = {{ \"opaaaaaaaaaan1\" : jump o_opaaaaaaaaaan1, \"opbbbbbbbbbbn0\" : jump o_opbbbbbbbbbbn0 }}\n\
             \t}}\n\
             \tmap in_nics {{\n\
             \t\ttype ifname : verdict\n\
             \t\telements = {{ \"opaaaaaaaaaan1\" : jump i_opaaaaaaaaaan1, \"opbbbbbbbbbbn0\" : jump i_opbbbbbbbbbbn0 }}\n\
             \t}}\n\
             {HOOKS}}}\n",
            out1 = out_head("52:54:00:00:00:01"),
            out2 = out_head("52:54:00:00:00:02"),
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn rules_keep_their_order_within_a_direction() {
        let rules = vec![
            rule(r#"{"direction":"in","action":"drop","remote":"10.0.0.5"}"#),
            rule(r#"{"direction":"out","action":"accept","protocol":"udp","ports":"53"}"#),
            rule(r#"{"direction":"in","action":"accept","remote":"10.0.0.0/8"}"#),
            rule(r#"{"direction":"out","action":"drop"}"#),
        ];
        let text = ruleset("abc", &[nic("op0n0", "52:54:00:aa:bb:cc", rules)]);
        let out = format!(
            "\tchain o_op0n0 {{\n{}\t\tmeta l4proto udp th dport 53 return\n\t\tdrop\n\t}}\n",
            out_head("52:54:00:aa:bb:cc")
        );
        assert!(text.contains(&out), "{text}");
        let inn = format!(
            "\tchain i_op0n0 {{\n{IN_HEAD}\t\tmeta protocol ip ip saddr 10.0.0.5/32 drop\n\
             \t\tmeta protocol ip ip saddr 10.0.0.0/8 accept\n\t\tdrop\n\t}}\n"
        );
        assert!(text.contains(&inn), "{text}");
    }

    #[test]
    fn each_rule_shape() {
        let cases = [
            (r#"{"direction":"in","action":"accept"}"#, "accept"),
            (r#"{"direction":"in","action":"drop"}"#, "drop"),
            (r#"{"direction":"out","action":"accept"}"#, "return"),
            (r#"{"direction":"out","action":"drop"}"#, "drop"),
            (
                r#"{"direction":"in","action":"accept","protocol":"tcp","ports":"8000-8080"}"#,
                "meta l4proto tcp th dport 8000-8080 accept",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"udp"}"#,
                "meta l4proto udp accept",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"icmp"}"#,
                "meta l4proto icmp accept",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"icmpv6"}"#,
                "meta l4proto ipv6-icmp accept",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"tcp","ports":"22","remote":"10.0.0.0/8"}"#,
                "meta protocol ip ip saddr 10.0.0.0/8 meta l4proto tcp th dport 22 accept",
            ),
            (
                r#"{"direction":"out","action":"drop","protocol":"udp","ports":"53","remote":"192.168.1.1"}"#,
                "meta protocol ip ip daddr 192.168.1.1/32 meta l4proto udp th dport 53 drop",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"icmpv6","remote":"2001:db8::/32"}"#,
                "meta protocol ip6 ip6 saddr 2001:db8::/32 meta l4proto ipv6-icmp accept",
            ),
            (
                r#"{"direction":"out","action":"accept","remote":"fd00::1"}"#,
                "meta protocol ip6 ip6 daddr fd00::1/128 return",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"tcp","ports":"80","remote":"0.0.0.0/0"}"#,
                "meta protocol ip meta l4proto tcp th dport 80 accept",
            ),
            (
                r#"{"direction":"out","action":"drop","remote":"::/0"}"#,
                "meta protocol ip6 drop",
            ),
        ];
        for (json, line) in cases {
            assert_eq!(rule_line(&rule(json)), line, "{json}");
        }
    }

    #[test]
    fn only_the_first_nic_of_a_tap_name_is_used() {
        let first = nic(
            "op0n0",
            "52:54:00:00:00:01",
            vec![rule(r#"{"direction":"in","action":"accept"}"#)],
        );
        let second = nic("op0n0", "52:54:00:00:00:02", vec![]);
        let text = ruleset(
            "abc",
            &[
                nic("op1n0", "52:54:00:00:00:03", vec![]),
                first.clone(),
                second,
            ],
        );
        assert_eq!(
            text,
            ruleset("abc", &[nic("op1n0", "52:54:00:00:00:03", vec![]), first])
        );
        assert_eq!(text.matches("chain o_op0n0 {").count(), 1, "{text}");
        assert!(
            text.contains("ether saddr != 52:54:00:00:00:01 drop"),
            "{text}"
        );
        assert!(!text.contains("52:54:00:00:00:02"), "{text}");
    }

    #[test]
    fn nics_with_unsafe_tap_names_are_skipped() {
        for tap in ["", "op0123456789n0xy", "Op0n0", "op0n0\"", "op0 n0", "op-0"] {
            assert_eq!(
                ruleset("abc", &[nic(tap, "52:54:00:aa:bb:cc", vec![])]),
                HEAD,
                "{tap:?}"
            );
        }
        let text = ruleset(
            "abc",
            &[
                nic("op;drop", "52:54:00:aa:bb:cc", vec![]),
                nic("op0n0", "52:54:00:aa:bb:cd", vec![]),
            ],
        );
        assert!(!text.contains("op;"), "{text}");
        assert!(text.contains("\"op0n0\" : jump o_op0n0 }"), "{text}");
    }
}
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub mod error;
pub mod hypervisor;
```

을 다음으로 바꾼다:

```rust
pub mod error;
pub mod firewall;
pub mod hypervisor;
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub use error::DriverError;
pub use hypervisor::{
```

을 다음으로 바꾼다:

```rust
pub use error::DriverError;
pub use firewall::{Firewall, FirewallNic, Nft, ruleset};
pub use hypervisor::{
```

`crates/opl-driver/src/net.rs`에서

```rust
/// 실패한 명령의 stderr. 비어 있으면(세그폴트 따위) 종료 상태를 쓴다.
fn failure_text(stderr: &[u8], status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
```

을 다음으로 바꾼다:

```rust
/// 실패한 명령의 stderr. 비어 있으면(세그폴트 따위) 종료 상태를 쓴다.
pub(crate) fn failure_text(stderr: &[u8], status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
```

`crates/opl-driver/tests/firewall.rs`를 만든다:

```rust
//! 실제 nftables 규칙을 네트워크 이름공간 안의 브리지에 넣는 통합 테스트.
//! `cargo test -p opl-driver --features net-it`. Linux에서 root(CAP_NET_ADMIN, `ip netns`)와 iproute2,
//! nftables(커널 5.3 이상), ping, python3가 필요하다. 노드와 VM을 이름공간으로 흉내 내므로 호스트
//! 네트워크와 호스트의 nft 규칙은 건드리지 않는다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use opl_driver::{Firewall, FirewallNic, Nft, ruleset};
use opl_types::network::tap_name;
use opl_types::security_group::Rule;
use opl_types::vm::MacAddr;
use uuid::Uuid;

/// 테스트가 만든 네트워크 이름공간. 끝나면 지운다(안의 장치와 nft 테이블도 함께 사라진다).
struct Netns(String);

impl Netns {
    fn new() -> Self {
        let name = format!("oplt-{}", random_hex(10));
        ip(&["netns", "add", &name]);
        let ns = Self(name);
        ip(&["-n", &ns.0, "link", "set", "lo", "up"]);
        ns
    }

    fn exec(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("ip");
        cmd.args(["netns", "exec", &self.0]).args(args);
        cmd
    }

    /// 성공했는지. 출력은 버린다.
    fn ok(&self, args: &[&str]) -> bool {
        self.exec(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .status
            .success()
    }
}

impl Drop for Netns {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["netns", "del", &self.0]).status();
    }
}

/// 테스트가 띄운 프로세스. 끝나면 멈춘다.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn random_hex(n: usize) -> String {
    Uuid::new_v4().simple().to_string()[..n].to_string()
}

fn ip(args: &[&str]) -> String {
    let out = Command::new("ip").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "ip {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// 주어진 포트들에서 연결을 받자마자 닫는 TCP 서버
const SERVER: &str = "
import socket, sys, threading
def serve(port):
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(('0.0.0.0', port))
    s.listen(16)
    while True:
        c, _ = s.accept()
        c.close()
for p in sys.argv[1:]:
    threading.Thread(target=serve, args=(int(p),)).start()
";

/// 1초 안에 TCP로 연결되면 0으로 끝난다.
const CLIENT: &str = "import socket, sys; socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=1).close()";

/// "VM" 하나: 이름공간 안의 `eth0`, 브리지 쪽 끝은 tap 이름의 veth
struct Vm {
    ns: Netns,
    tap: String,
    mac: MacAddr,
}

impl Vm {
    fn new(host: &Netns, addr: &str) -> Self {
        let ns = Netns::new();
        let tap = tap_name(Uuid::now_v7(), 0);
        let mac = MacAddr::generate();
        ip(&[
            "-n", &host.0, "link", "add", &tap, "type", "veth", "peer", "name", "eth0", "netns",
            &ns.0,
        ]);
        ip(&["-n", &host.0, "link", "set", &tap, "master", "br0", "up"]);
        ip(&[
            "-n",
            &ns.0,
            "link",
            "set",
            "eth0",
            "address",
            &mac.to_string(),
        ]);
        ip(&["-n", &ns.0, "addr", "add", addr, "dev", "eth0"]);
        ip(&["-n", &ns.0, "link", "set", "eth0", "up"]);
        Self { ns, tap, mac }
    }

    /// 방화벽 NIC. `rules`는 `"in accept tcp 22"`처럼 방향·동작·[프로토콜]·[포트]·[CIDR].
    fn nic(&self, rules: &[&str]) -> FirewallNic {
        FirewallNic {
            tap: self.tap.clone(),
            mac: self.mac,
            rules: rules.iter().map(|r| rule(r)).collect(),
        }
    }

    /// 한 번 ping해서 답이 오는지
    fn ping(&self, to: &str) -> bool {
        self.ns.ok(&["ping", "-c1", "-W1", to])
    }

    /// 한 번 TCP 연결해 보아 되는지
    fn connect(&self, to: &str, port: u16) -> bool {
        self.ns
            .ok(&["python3", "-c", CLIENT, to, &port.to_string()])
    }

    /// `eth0`의 MAC을 바꾼다(위조).
    fn set_mac(&self, mac: MacAddr) {
        ip(&["-n", &self.ns.0, "link", "set", "eth0", "down"]);
        ip(&[
            "-n",
            &self.ns.0,
            "link",
            "set",
            "eth0",
            "address",
            &mac.to_string(),
        ]);
        ip(&["-n", &self.ns.0, "link", "set", "eth0", "up"]);
    }

    /// 이웃(ARP) 캐시를 비운다.
    fn flush_neigh(&self) {
        ip(&["-n", &self.ns.0, "neigh", "flush", "all"]);
    }
}

/// `"in accept tcp 22"`, `"out drop 10.0.0.1/32"`처럼 방향·동작 뒤에 [프로토콜] [포트] [CIDR]
fn rule(text: &str) -> Rule {
    let mut words = text.split(' ');
    let mut json = serde_json::json!({ "direction": words.next(), "action": words.next() });
    for word in words {
        let key = if ["tcp", "udp", "icmp", "icmpv6"].contains(&word) {
            "protocol"
        } else if word.contains(['.', ':']) {
            "remote"
        } else {
            "ports"
        };
        json[key] = word.into();
    }
    serde_json::from_value(json).unwrap()
}

/// `check`가 참이 될 때까지 몇 번 해 본다.
async fn wait_for(what: &str, check: impl Fn() -> bool) {
    for _ in 0..20 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("timed out waiting until {what}");
}

/// 규칙 글을 넣는다. 실패하면 글과 함께 멈춘다.
async fn apply(nft: &Nft, tag: &str, nics: Vec<FirewallNic>) {
    let text = ruleset(tag, &nics);
    nft.apply(&text)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{text}"));
}

#[tokio::test]
async fn security_group_rules_filter_bridged_traffic() {
    let host = Netns::new();
    ip(&["-n", &host.0, "link", "add", "br0", "type", "bridge"]);
    ip(&["-n", &host.0, "link", "set", "br0", "up"]);
    let a = Vm::new(&host, "192.168.88.1/24");
    let b = Vm::new(&host, "192.168.88.2/24");
    let to_b = "192.168.88.2";
    let _server = Proc(
        b.ns.exec(&["python3", "-c", SERVER, "22", "23"])
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // 방화벽 없이는 모두 통한다.
    wait_for("A can ping B", || a.ping(to_b)).await;
    wait_for("A can connect to B:22", || a.connect(to_b, 22)).await;
    wait_for("A can connect to B:23", || a.connect(to_b, 23)).await;

    let tag = random_hex(12);
    let table = format!("opl_{tag}");
    let nft = Nft::in_netns(&host.0);

    // (1) B는 tcp 22만 받는다.
    apply(&nft, &tag, vec![b.nic(&["in accept tcp 22"])]).await;
    let tables = String::from_utf8(
        host.exec(&["nft", "list", "tables"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        tables.contains(&format!("table bridge {table}")),
        "{tables}"
    );
    wait_for("A can connect to B:22", || a.connect(to_b, 22)).await;
    assert!(!a.connect(to_b, 23), "tcp 23 must be blocked");
    assert!(!a.ping(to_b), "ping must be blocked");

    // (2) 규칙을 더하면 곧바로 반영된다(테이블을 통째로 바꾼다).
    apply(
        &nft,
        &tag,
        vec![b.nic(&["in accept tcp 22", "in accept icmp"])],
    )
    .await;
    wait_for("A can ping B", || a.ping(to_b)).await;
    assert!(!a.connect(to_b, 23), "tcp 23 must stay blocked");

    // (3) A의 나가기에서 icmp를 막는다. 나머지 나가기는 허용이다.
    let b_rules = ["in accept tcp 22", "in accept icmp"];
    apply(&nft, &tag, vec![a.nic(&["out drop icmp"]), b.nic(&b_rules)]).await;
    assert!(!a.ping(to_b), "outgoing ping from A must be blocked");
    wait_for("A can connect to B:22", || a.connect(to_b, 22)).await;

    // 허용 목록 + 끝 drop: ARP는 규칙 앞에서 늘 통하므로 이웃 캐시를 비워도 허용한 주소로는 나간다.
    apply(
        &nft,
        &tag,
        vec![
            a.nic(&["out accept 192.168.88.2/32", "out drop"]),
            b.nic(&b_rules),
        ],
    )
    .await;
    a.flush_neigh();
    b.flush_neigh();
    wait_for("A can ping the allowed B", || a.ping(to_b)).await;

    // (4) A가 MAC을 바꾸면(위조) A에서 나가는 것이 모두 막힌다.
    apply(&nft, &tag, vec![a.nic(&[]), b.nic(&b_rules)]).await;
    wait_for("A can ping B", || a.ping(to_b)).await;
    let spoofed = MacAddr::generate();
    a.set_mac(spoofed);
    a.flush_neigh();
    b.flush_neigh();
    for _ in 0..3 {
        assert!(!a.ping(to_b), "A with a spoofed MAC must be blocked");
    }
    // 그 MAC을 NIC의 MAC으로 넣으면 다시 통한다(막힌 까닭이 MAC 검사였다).
    let mut a_nic = a.nic(&[]);
    a_nic.mac = spoofed;
    apply(&nft, &tag, vec![a_nic, b.nic(&b_rules)]).await;
    wait_for("A can ping B with the NIC's MAC", || a.ping(to_b)).await;

    // (5) 방화벽 NIC가 없으면 테이블이 사라지고 모두 통한다. 두 번 해도 된다.
    apply(&nft, &tag, vec![]).await;
    apply(&nft, &tag, vec![]).await;
    let tables = String::from_utf8(
        host.exec(&["nft", "list", "tables"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(!tables.contains(&table), "{tables}");
    wait_for("A can connect to B:23", || a.connect(to_b, 23)).await;
}

/// VLAN-aware 브리지: VM tap은 VLAN 10 untagged(pvid), 업링크는 VLAN 10 tagged로 다른 이름공간의
/// VLAN 하위 인터페이스와 잇는다. 주소 규칙(`meta protocol ip ip daddr …`)이 태그가 붙는 쪽으로
/// 나가거나 태그를 달고 들어오는 패킷에도 맞는지 본다.
#[tokio::test]
async fn address_rules_match_on_a_vlan_aware_bridge() {
    let host = Netns::new();
    ip(&[
        "-n",
        &host.0,
        "link",
        "add",
        "br0",
        "type",
        "bridge",
        "vlan_filtering",
        "1",
    ]);
    ip(&["-n", &host.0, "link", "set", "br0", "up"]);
    let bridge_vlan = |args: &[&str]| {
        let out = host
            .exec(&[&["bridge", "vlan"][..], args].concat())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "bridge vlan {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let a = Vm::new(&host, "192.168.89.1/24");
    bridge_vlan(&["add", "dev", &a.tap, "vid", "10", "pvid", "untagged"]);
    bridge_vlan(&["del", "dev", &a.tap, "vid", "1"]);
    // 업링크 건너편: VLAN 10 하위 인터페이스 `trunk.10`에 주소
    let up = Netns::new();
    ip(&[
        "-n", &host.0, "link", "add", "upl0", "type", "veth", "peer", "name", "trunk", "netns",
        &up.0,
    ]);
    ip(&["-n", &host.0, "link", "set", "upl0", "master", "br0", "up"]);
    bridge_vlan(&["add", "dev", "upl0", "vid", "10"]);
    bridge_vlan(&["del", "dev", "upl0", "vid", "1"]);
    ip(&["-n", &up.0, "link", "set", "dev", "trunk", "up"]);
    ip(&[
        "-n", &up.0, "link", "add", "link", "trunk", "name", "trunk.10", "type", "vlan", "id", "10",
    ]);
    ip(&[
        "-n",
        &up.0,
        "addr",
        "add",
        "192.168.89.2/24",
        "dev",
        "trunk.10",
    ]);
    ip(&["-n", &up.0, "link", "set", "dev", "trunk.10", "up"]);
    let (to_up, to_a) = ("192.168.89.2", "192.168.89.1");
    let up_pings_a = || up.ok(&["ping", "-c1", "-W1", to_a]);

    // 방화벽 없이는 통한다.
    wait_for("A can ping the uplink", || a.ping(to_up)).await;
    wait_for("the uplink can ping A", up_pings_a).await;

    let tag = random_hex(12);
    let nft = Nft::in_netns(&host.0);
    // 나가기: 업링크 주소 하나, IPv4 전체
    apply(&nft, &tag, vec![a.nic(&["out drop 192.168.89.2/32"])]).await;
    assert!(
        !a.ping(to_up),
        "ping to the dropped address must be blocked"
    );
    apply(&nft, &tag, vec![a.nic(&["out drop 0.0.0.0/0"])]).await;
    assert!(!a.ping(to_up), "IPv4 out must be blocked");
    // 허용 목록 + 끝 drop: 이웃 캐시를 비워도 ARP는 통해 허용한 주소로는 나간다.
    apply(
        &nft,
        &tag,
        vec![a.nic(&["out accept 192.168.89.2/32", "out drop"])],
    )
    .await;
    a.flush_neigh();
    ip(&["-n", &up.0, "neigh", "flush", "all"]);
    wait_for("A can ping the allowed uplink", || a.ping(to_up)).await;

    // 들어오기: 규칙이 없으면 막히고(A가 연 연결의 응답은 들어온다), 주소·IPv4 전체를 허용하면 통한다.
    apply(&nft, &tag, vec![a.nic(&[])]).await;
    wait_for("A can ping the uplink", || a.ping(to_up)).await;
    assert!(!up_pings_a(), "in must be blocked without rules");
    apply(&nft, &tag, vec![a.nic(&["in accept 192.168.89.2"])]).await;
    wait_for("the uplink can ping A", up_pings_a).await;
    apply(&nft, &tag, vec![a.nic(&["in accept 192.168.89.0/25"])]).await;
    wait_for("the uplink can ping A", up_pings_a).await;
    apply(
        &nft,
        &tag,
        vec![a.nic(&["in drop 192.168.89.2", "in accept 0.0.0.0/0"])],
    )
    .await;
    assert!(!up_pings_a(), "the dropped address must stay blocked");
    apply(&nft, &tag, vec![a.nic(&["in accept 0.0.0.0/0"])]).await;
    wait_for("the uplink can ping A", up_pings_a).await;
    apply(&nft, &tag, vec![]).await;
}

#[tokio::test]
async fn bad_ruleset_fails_and_keeps_the_old_rules() {
    let host = Netns::new();
    let tag = random_hex(12);
    let nft = Nft::in_netns(&host.0);
    let nic = FirewallNic {
        tap: tap_name(Uuid::now_v7(), 0),
        mac: MacAddr::generate(),
        rules: vec![rule("in accept tcp 22")],
    };
    let good = ruleset(&tag, std::slice::from_ref(&nic));
    nft.apply(&good).await.unwrap();
    // 원자적: 앞부분(테이블 지우기)도 반영되지 않는다.
    let err = nft
        .apply(&format!("{good}this is not nft\n"))
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("nft failed: "), "{err}");
    let out = host
        .exec(&["nft", "list", "table", "bridge", &format!("opl_{tag}")])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&format!("chain i_{}", nic.tap)), "{text}");
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 913개, 실패 0

Run: `cargo test -p opl-driver firewall`
Expected: PASS (net-it 시험은 Linux root에서 `--features net-it`)

- [ ] **Step 3: 커밋**

```bash
git add .github crates
git commit -m "feat(opl-driver): nft 방화벽 규칙 글과 실행기 — ruleset·Firewall·Nft"
```

---

### Task 4: 노드 방화벽

`NodeStatus.firewall`: opld가 시작 때 이 클러스터 태그의 빈 테이블 정의를 실제로 적용해 보고(기존 규칙은 건드리지 않음) 성공하면 true. 스케줄러·마이그레이션 대상·HA 복구는 방화벽 NIC VM을 이런 노드에만, VM의 `spec.node`가 정해져 있으면 그 노드도 방화벽이 있어야 한다(409 `firewall_unsupported`). opld `firewall.rs` 에이전트: 클러스터의 모든 VM(배치 무관)에서 방화벽 NIC를 모아(`firewall_nics`, 없는 그룹이면 양방향 차단) 시작·VM·보안 그룹 watch(0.5초 모아)·60초마다 적용, `cluster leave` 때 테이블을 지운다. reconciler `FirewallGate`: 워커가 방화벽 NIC VM을 켜기·다시 붙이기·멈춘 VM 다시 돌리기 직전에, opl-api가 마이그레이션을 받기 직전에 규칙을 넣고, 넣지 못하면 켜지 않는다.

**Files:**
- Create: `crates/opl-reconciler/src/firewall.rs`
- Create: `crates/opl-reconciler/tests/firewall.rs`
- Create: `crates/opld/src/firewall.rs`
- Modify: `crates/opl-api/src/incoming.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/ha.rs`
- Modify: `crates/opl-api/tests/incoming.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/security_groups.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-reconciler/src/config.rs`
- Modify: `crates/opl-reconciler/src/lib.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opl-types/src/vm.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/host.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/incoming.rs`에서

```rust
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_reconciler::resolve::incoming_spec;
```

을 다음으로 바꾼다:

```rust
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_reconciler::FirewallGate;
use opl_reconciler::resolve::incoming_spec;
```

`crates/opl-api/src/incoming.rs`에서

```rust
    config: IncomingConfig,
}
```

을 다음으로 바꾼다:

```rust
    config: IncomingConfig,
    /// 이 노드의 방화벽(reconciler와 같다). 없으면 보안 그룹을 건 NIC가 있는 VM을 받지 않는다.
    firewall: Option<Arc<dyn FirewallGate>>,
}
```

`crates/opl-api/src/incoming.rs`에서

```rust
            config,
        }
    }
```

을 다음으로 바꾼다:

```rust
            config,
            firewall: None,
        }
    }

    /// 보안 그룹을 건 NIC가 있는 VM을 받기 직전에 이 노드의 방화벽 규칙을 넣는다.
    pub fn with_firewall(mut self, firewall: Option<Arc<dyn FirewallGate>>) -> Self {
        self.firewall = firewall;
        self
    }
```

`crates/opl-api/src/incoming.rs`에서

```rust
impl MigrationReceiver for IncomingMigrations {
    /// QEMU 버전을 견주고, VM이 이 Task로 이 노드에 오는 중인지 본 뒤 디스크를 공유로 켜고(원본이
    /// 아직 쓴다) 받는 QEMU를 띄운다. 앞선 마이그레이션(다른 Task)의 받는 QEMU가 남아 있으면 먼저
    /// 끈다. 실패하면 켠 디스크를 끈다(띄운 QEMU는 드라이버가 끈다).
```

을 다음으로 바꾼다:

```rust
impl MigrationReceiver for IncomingMigrations {
    /// QEMU 버전을 견주고, VM이 이 Task로 이 노드에 오는 중인지 본 뒤 (보안 그룹이 있으면 방화벽
    /// 규칙을 넣고) 디스크를 공유로 켜고(원본이 아직 쓴다) 받는 QEMU를 띄운다. 앞선 마이그레이션(다른 Task)의 받는 QEMU가 남아 있으면 먼저
    /// 끈다. 실패하면 켠 디스크를 끈다(띄운 QEMU는 드라이버가 끈다).
```

`crates/opl-api/src/incoming.rs`에서

```rust
            self.hv.stop(req.vm, StopMode::Force).await.map_err(fail)?;
        }
        let c = &self.config;
```

을 다음으로 바꾼다:

```rust
            self.hv.stop(req.vm, StopMode::Force).await.map_err(fail)?;
        }
        // 보안 그룹을 건 NIC가 있으면 받는 QEMU(tap)를 띄우기 전에 방화벽 규칙을 넣는다. 넣지 못하면
        // 받지 않는다.
        if vm.spec.has_firewall() {
            let Some(gate) = &self.firewall else {
                return Err(IncomingError::Failed(
                    "firewall: this node has no firewall (nftables)".into(),
                ));
            };
            gate.sync()
                .await
                .map_err(|e| IncomingError::Failed(format!("firewall: {e}")))?;
        }
        let c = &self.config;
```

`crates/opl-api/src/scheduler.rs`에서

```rust
#[error(
    "no node can run this VM (needs an online node that is not a witness or in maintenance, has the architecture of its machine type, can reach its storage pools, has the bridges of its networks and, for an HA VM, has HA fencing)"
)]
```

을 다음으로 바꾼다:

```rust
#[error(
    "no node can run this VM (needs an online node that is not a witness or in maintenance, has the architecture of its machine type, can reach its storage pools, has the bridges of its networks, has a firewall (nftables) for VMs with security groups and, for an HA VM, has HA fencing)"
)]
```

`crates/opl-api/src/scheduler.rs`에서

```rust
/// 위트니스가 아니고 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// (HA VM이면 HA 가능 노드) 가운데 미할당 메모리가 가장 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은
/// 범위 밖). arch를 모르는 노드(옛 버전)는 고르지 않는다.
```

을 다음으로 바꾼다:

```rust
/// 위트니스가 아니고 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// (HA VM이면 HA 가능 노드, 보안 그룹을 건 NIC가 있으면 방화벽이 있는 노드) 가운데 미할당 메모리가 가장 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은
/// 범위 밖). arch를 모르는 노드(옛 버전)는 고르지 않는다.
```

`crates/opl-api/src/scheduler.rs`에서

```rust
    }
    Ok(())
```

을 다음으로 바꾼다:

```rust
    }
    if spec.has_firewall() && !node.status.firewall {
        return Err(format!(
            "node {name:?} does not have a firewall (nftables) for the VM's security groups"
        ));
    }
    Ok(())
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            underlay: None,
        };
```

을 다음으로 바꾼다:

```rust
            underlay: None,
            firewall: false,
        };
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            Err("node \"n\" does not support HA (fencing is off)".into())
        );
```

을 다음으로 바꾼다:

```rust
            Err("node \"n\" does not support HA (fencing is off)".into())
        );
        inv.nodes.pop();
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }

    #[test]
    fn vms_with_security_groups_go_only_to_nodes_with_a_firewall() {
        // 메모리가 가장 많은 노드도 방화벽이 없으면 보안 그룹 NIC가 있는 VM을 받지 않는다.
        let bare = node(64000);
        let mut guarded = node(1024);
        guarded.status.firewall = true;
        let mut inv = Inventory {
            nodes: vec![bare.clone(), guarded.clone()],
            networks: vec![Resource::new(
                "lan",
                NetworkSpec {
                    bridge: Some("vmbr0".into()),
                    vlan: None,
                    vxlan: None,
                    mtu: None,
                },
            )],
            ..Default::default()
        };
        for n in &mut inv.nodes {
            n.status.bridges = vec![BridgeInfo {
                name: "vmbr0".into(),
                vlan_filtering: false,
            }];
        }
        let mut me = spec(512, None, PowerState::Running);
        me.nics = vec![NicSpec {
            network: "lan".into(),
            model: NicModel::Virtio,
            mac: None,
            security_groups: Vec::new(),
        }];
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(bare.id));
        me.nics[0].security_groups = vec!["web".into()];
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(guarded.id));
        assert_eq!(
            node_fits(&me, &inv, &inv.nodes[0]),
            Err(
                "node \"n\" does not have a firewall (nftables) for the VM's security groups"
                    .into()
            )
        );
```

`crates/opl-api/src/vms.rs`에서

```rust
        check_node_arch(node, spec)?;
    }
```

을 다음으로 바꾼다:

```rust
        check_node_arch(node, spec)?;
        check_firewall(node, spec)?;
    }
```

`crates/opl-api/src/vms.rs`에서

```rust
                "node {:?} does not support HA (fencing is off) and cannot run HA VMs",
                node.name
```

을 다음으로 바꾼다:

```rust
                "node {:?} does not support HA (fencing is off) and cannot run HA VMs",
                node.name
            ),
        ));
    }
    Ok(())
}

/// 보안 그룹을 건 NIC가 있는데 노드에 방화벽(nftables)이 없으면 409 `firewall_unsupported`.
fn check_firewall(node: &Node, spec: &VmSpec) -> Result<(), ApiError> {
    if spec.has_firewall() && !node.status.firewall {
        return Err(ApiError::conflict(
            "firewall_unsupported",
            format!(
                "node {:?} has no firewall (nftables) for security groups",
                node.name
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        ha: None,
    }
```

을 다음으로 바꾼다:

```rust
        ha: None,
        firewall: None,
    }
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            underlay: None,
        };
```

을 다음으로 바꾼다:

```rust
            underlay: None,
            firewall: false,
        };
```

`crates/opl-api/tests/ha.rs`에서

```rust
        underlay: None,
    };
```

을 다음으로 바꾼다:

```rust
        underlay: None,
        firewall: false,
    };
```

`crates/opl-api/tests/incoming.rs`에서

```rust
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_types::node::{Node, NodeSpec};
```

을 다음으로 바꾼다:

```rust
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_reconciler::FirewallGate;
use opl_types::network::NetworkSpec;
use opl_types::node::{Node, NodeSpec};
```

`crates/opl-api/tests/incoming.rs`에서

```rust
    s.wait_stopped(db.id).await;
    assert_eq!(s.deactivations(), 0);
}
```

을 다음으로 바꾼다:

```rust
    s.wait_stopped(db.id).await;
    assert_eq!(s.deactivations(), 0);
}

/// 부를 때마다 `fail`이면 실패하는 방화벽
struct Gate {
    fail: bool,
}

#[async_trait::async_trait]
impl FirewallGate for Gate {
    async fn sync(&self) -> Result<(), String> {
        if self.fail {
            Err("nft: boom".into())
        } else {
            Ok(())
        }
    }
}

/// 보안 그룹을 건 NIC가 있는 VM은 방화벽 규칙을 넣은 뒤에만 받는다. 넣지 못하면(방화벽이 없는 노드
/// 포함) 디스크도 켜지 않고 거절한다.
#[tokio::test]
async fn a_vm_with_security_groups_needs_the_firewall() {
    let s = setup().await;
    let lan = Resource::new(
        "lan",
        NetworkSpec {
            bridge: Some("vmbr0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    s.env.cluster.create(&lan).await.unwrap();
    let mut vm = vm_named(&s.env, "web");
    vm.spec.nics[0].network = "lan".into();
    vm.spec.nics[0].security_groups = vec!["web".into()];
    s.env
        .cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;
    let receiver = |gate: Option<Gate>| {
        IncomingMigrations::new(
            s.env.cluster.clone(),
            s.env.hv.clone() as Arc<dyn Hypervisor>,
            IncomingConfig {
                node: s.env.node,
                mount_root: MOUNT_ROOT.into(),
                storage: s.storage.clone() as Arc<dyn Storage>,
                listen_host: "127.0.0.1".into(),
                ports: 61000..=61009,
            },
        )
        .with_firewall(gate.map(|g| Arc::new(g) as Arc<dyn FirewallGate>))
    };

    for (gate, why) in [
        (None, "firewall: this node has no firewall (nftables)"),
        (Some(Gate { fail: true }), "firewall: nft: boom"),
    ] {
        let err = receiver(gate).incoming(s.request(task)).await.unwrap_err();
        assert!(
            matches!(&err, IncomingError::Failed(e) if e == why),
            "{err:?}"
        );
        assert!(!s.env.hv.is_incoming(s.vm));
        assert_eq!(s.count("share"), 0);
    }
    let port = receiver(Some(Gate { fail: false }))
        .incoming(s.request(task))
        .await
        .unwrap();
    assert_eq!(port, 61000);
    assert!(s.env.hv.is_incoming(s.vm));
}
```

`crates/opl-api/tests/migrate.rs`에서

```rust
        underlay: None,
    };
```

을 다음으로 바꾼다:

```rust
        underlay: None,
        firewall: false,
    };
```

`crates/opl-api/tests/security_groups.rs`에서

```rust

/// `lan` 브리지 네트워크(노드에 그 브리지가 있다)와 규칙 없는 그룹 `web`을 만든다.
async fn lan_and_web(env: &Env, admin: &Client) {
```

을 다음으로 바꾼다:

```rust

/// `lan` 브리지 네트워크(노드에 그 브리지와 방화벽이 있다)와 규칙 없는 그룹 `web`을 만든다.
async fn lan_and_web(env: &Env, admin: &Client) {
```

`crates/opl-api/tests/security_groups.rs`에서

```rust
    }];
    env.cluster
```

을 다음으로 바꾼다:

```rust
    }];
    status.firewall = true;
    env.cluster
```

`crates/opl-api/tests/security_groups.rs`에서

```rust
}

#[tokio::test]
async fn security_groups_are_documented() {
```

을 다음으로 바꾼다:

```rust
}

/// 노드를 정한 VM에 그룹을 걸려면 그 노드에 방화벽(nftables)이 있어야 한다.
#[tokio::test]
async fn pinned_vms_with_groups_need_a_node_with_a_firewall() {
    let env = Env::start().await;
    let admin = env.admin().await;
    lan_and_web(&env, &admin).await;
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status.clone();
    status.firewall = false;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();

    let mut req = vm_with_groups("vm1", json!(["web"]));
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "firewall_unsupported");
    assert_eq!(
        body["message"],
        format!(
            "node {:?} has no firewall (nftables) for security groups",
            node.name
        )
    );
    // 그룹이 없으면 되고, 그 뒤 그룹을 걸 때 같은 검사를 한다.
    let mut plain = vm_with_groups("vm1", json!([]));
    plain["spec"]["node"] = json!(env.node);
    let (status, vm) = admin.post("/vms", plain).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["security_groups"] = json!(["web"]);
    let (status, body) = admin
        .put("/vms/vm1", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "firewall_unsupported");

    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status.clone();
    status.firewall = true;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn security_groups_are_documented() {
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            underlay: None,
        };
```

을 다음으로 바꾼다:

```rust
            underlay: None,
            firewall: false,
        };
```

`crates/opl-reconciler/src/config.rs`에서

```rust

use crate::firmware::FirmwareFetcher;
```

을 다음으로 바꾼다:

```rust

use crate::firewall::FirewallGate;
use crate::firmware::FirmwareFetcher;
```

`crates/opl-reconciler/src/config.rs`에서

```rust
    pub ha: Option<Arc<HaGuard>>,
}
```

을 다음으로 바꾼다:

```rust
    pub ha: Option<Arc<HaGuard>>,
    /// 이 노드의 방화벽. None이면 방화벽이 없어 보안 그룹을 건 NIC가 있는 VM을 켜지 않는다.
    pub firewall: Option<Arc<dyn FirewallGate>>,
}
```

`crates/opl-reconciler/src/config.rs`에서

```rust
            ha: None,
        }
```

을 다음으로 바꾼다:

```rust
            ha: None,
            firewall: None,
        }
```

`crates/opl-reconciler/src/firewall.rs`를 만든다:

```rust
//! VM을 켜기 직전의 방화벽 확인. 규칙을 넣는 일은 opld의 방화벽 에이전트가 [`FirewallGate`]로 넣는다
//! (reconciler는 nftables를 모른다).

use async_trait::async_trait;

/// 보안 그룹을 건 NIC가 있는 VM을 켜기(마이그레이션 받기 포함) 직전에 부른다. 지금 클러스터 상태로
/// 이 노드의 방화벽 규칙을 넣고, 넣지 못하면 그 이유를 돌려준다(VM을 켜지 않는다).
#[async_trait]
pub trait FirewallGate: Send + Sync {
    async fn sync(&self) -> Result<(), String>;
}
```

`crates/opl-reconciler/src/lib.rs`에서

```rust
mod config;
mod firmware;
```

을 다음으로 바꾼다:

```rust
mod config;
mod firewall;
mod firmware;
```

`crates/opl-reconciler/src/lib.rs`에서

```rust
pub use config::{ReconcilerConfig, RestartBackoff};
pub use firmware::{FirmwareError, FirmwareFetcher};
```

을 다음으로 바꾼다:

```rust
pub use config::{ReconcilerConfig, RestartBackoff};
pub use firewall::FirewallGate;
pub use firmware::{FirmwareError, FirmwareFetcher};
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
const IMAGE_LOCKED: &str = "disk image is locked by another process; retrying";
```

을 다음으로 바꾼다:

```rust
const IMAGE_LOCKED: &str = "disk image is locked by another process; retrying";
/// 방화벽이 없는 노드(nftables 없음, macOS)에서 보안 그룹을 건 NIC가 있는 VM을 켜려 할 때의 오류
const NO_FIREWALL: &str = "failed to start: firewall: this node has no firewall (nftables)";
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
                    } else {
                        match self.hv.resume_left_stopped(self.id).await {
                            Ok(true) => Ok(()),
                            Ok(false) => self.reattach(vm).await,
                            Err(e) => Err(unresponsive(e)),
                        }
```

을 다음으로 바꾼다:

```rust
                    } else {
                        // 멈춘 VM을 이어서 실행하거나 다시 붙기 전에 방화벽 규칙을 넣는다.
                        match self.sync_firewall(vm).await {
                            Err(e) => Err(e),
                            Ok(()) => match self.hv.resume_left_stopped(self.id).await {
                                Ok(true) => Ok(()),
                                Ok(false) => self.reattach(vm).await,
                                Err(e) => Err(unresponsive(e)),
                            },
                        }
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
        }
        // 실패해도 다시 쓰지 않는다.
```

을 다음으로 바꾼다:

```rust
        }
        // 보안 그룹을 건 NIC가 있으면 켜기 직전에 지금 클러스터 상태로 이 노드의 방화벽 규칙을 넣는다.
        // 넣지 못하면 켜지 않는다(방화벽 없이 도는 순간이 없게).
        if vm.spec.has_firewall() {
            let Some(gate) = &self.config.firewall else {
                self.deactivate_disks(vm).await;
                self.restores.lock().unwrap().remove(&self.id);
                self.power_off = Some(Some(NO_FIREWALL.into()));
                return Ok(Start::NotStarted);
            };
            if let Err(e) = gate.sync().await {
                tracing::warn!(vm = %self.id, error = %e, "cannot apply the firewall rules; not starting");
                self.deactivate_disks(vm).await;
                self.record_failure(format!("failed to start: firewall: {e}"));
                return Ok(Start::NotStarted);
            }
        }
        // 실패해도 다시 쓰지 않는다.
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
    /// 살아 있는 VM에 다시 붙거나 이어서 실행한다(`start`는 멱등이다). 참조 해석 오류도 돌려준다.
    /// QEMU가 이미 디스크를 열었으므로 크기 늘리기 Task를 기다리지 않는다. HA VM은 디스크를 해석한 뒤
```

을 다음으로 바꾼다:

```rust
    /// 살아 있는 VM에 다시 붙거나 이어서 실행한다(`start`는 멱등이다). 참조 해석 오류도 돌려준다.
    /// 방화벽 규칙은 부르는 쪽이 먼저 넣는다([`Worker::sync_firewall`]).
    /// QEMU가 이미 디스크를 열었으므로 크기 늘리기 Task를 기다리지 않는다. HA VM은 디스크를 해석한 뒤
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            .map_err(|e| unresponsive(&e))
    }
```

을 다음으로 바꾼다:

```rust
            .map_err(|e| unresponsive(&e))
    }

    /// 보안 그룹을 건 NIC가 있으면 멈춘 채 남은 VM을 이어서 실행하거나 다시 붙기 전에 방화벽 규칙을
    /// 넣는다. 넣지 못하면 이어서 실행하지 않고 다음 차례에 다시 본다.
    async fn sync_firewall(&self, vm: &Vm) -> Result<(), ContinueError> {
        if !vm.spec.has_firewall() {
            return Ok(());
        }
        let unresponsive = |e: &dyn std::fmt::Display| ContinueError::Unresponsive(e.to_string());
        let Some(gate) = &self.config.firewall else {
            return Err(unresponsive(
                &"firewall: this node has no firewall (nftables)",
            ));
        };
        gate.sync()
            .await
            .map_err(|e| unresponsive(&format!("firewall: {e}")))
    }
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
        ha: None,
    }
```

을 다음으로 바꾼다:

```rust
        ha: None,
        firewall: None,
    }
```

`crates/opl-reconciler/tests/firewall.rs`를 만든다:

```rust
//! 보안 그룹을 건 NIC가 있는 VM을 켜기 직전의 방화벽 확인([`FirewallGate`]).

mod common;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm};
use opl_reconciler::{FirewallGate, ReconcilerConfig};
use opl_types::network::NetworkSpec;
use opl_types::resource::Resource;
use opl_types::vm::{MacAddr, NicModel, NicSpec, PowerState, Vm, VmPhase};

/// 부른 횟수를 세고, `fail`에 남은 오류를 앞에서부터 하나씩 돌려준다.
#[derive(Default)]
struct Gate {
    calls: AtomicUsize,
    fail: Mutex<Vec<String>>,
}

#[async_trait]
impl FirewallGate for Gate {
    async fn sync(&self) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut fail = self.fail.lock().unwrap();
        if fail.is_empty() {
            Ok(())
        } else {
            Err(fail.remove(0))
        }
    }
}

async fn env_with(gate: Option<Arc<Gate>>) -> Env {
    env_with_config(gate, |_| {}).await
}

async fn env_with_config(
    gate: Option<Arc<Gate>>,
    tweak: impl FnOnce(&mut ReconcilerConfig),
) -> Env {
    let env = Env::start_with(|c| {
        c.firewall = gate.map(|g| g as Arc<dyn FirewallGate>);
        tweak(c);
    })
    .await;
    let lan = Resource::new(
        "lan",
        NetworkSpec {
            bridge: Some("vmbr0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    env.cluster.create(&lan).await.unwrap();
    env
}

/// 브리지 네트워크 NIC 하나짜리 VM. `groups`가 있으면 방화벽이 켜진다.
fn vm_with_groups(env: &Env, groups: &[&str]) -> Vm {
    let mut vm = vm("web", env.node);
    vm.spec.nics = vec![NicSpec {
        network: "lan".into(),
        model: NicModel::Virtio,
        mac: Some(MacAddr([0x52, 0x54, 0, 1, 2, 3])),
        security_groups: groups.iter().map(|g| g.to_string()).collect(),
    }];
    vm
}

#[tokio::test]
async fn starts_after_the_firewall_rules_are_in() {
    let gate = Arc::new(Gate::default());
    let env = env_with(Some(gate.clone())).await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    assert_eq!(env.hv.launches().len(), 1);
}

/// 규칙을 넣지 못하면 켜지 않고 시작 실패로 센다(일시 실패처럼 다시 시도하고, 계속 실패하면 포기).
#[tokio::test]
async fn a_failing_firewall_keeps_the_vm_off() {
    let gate = Arc::new(Gate::default());
    *gate.fail.lock().unwrap() = vec!["nft: boom".into()];
    let env = env_with(Some(gate.clone())).await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    // 한 번 실패한 뒤 다시 시도해 켠다.
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 2);
    assert_eq!(env.hv.launches().len(), 1);

    // 하네스는 3번 실패하면 포기한다.
    let gate = Arc::new(Gate::default());
    *gate.fail.lock().unwrap() = vec!["nft: boom".into(); 10];
    let env = env_with(Some(gate.clone())).await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to start: firewall: nft: boom; gave up after 3 failures")
    );
    assert_eq!(gate.calls.load(Ordering::SeqCst), 3);
    assert!(env.hv.launches().is_empty());
}

/// 방화벽이 없는 노드는 보안 그룹 NIC가 있는 VM을 켜지 않는다(다시 시도하지 않는다).
#[tokio::test]
async fn a_node_without_a_firewall_does_not_start_the_vm() {
    let env = env_with(None).await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to start: firewall: this node has no firewall (nftables)")
    );
    assert!(env.hv.launches().is_empty());
}

/// 보안 그룹이 없는 VM은 방화벽 확인 없이 켠다(방화벽이 없는 노드에서도).
#[tokio::test]
async fn vms_without_groups_skip_the_gate() {
    let gate = Arc::new(Gate::default());
    *gate.fail.lock().unwrap() = vec!["nft: boom".into(); 10];
    let env = env_with(Some(gate.clone())).await;
    let vm = vm_with_groups(&env, &[]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 0);

    let env = env_with(None).await;
    let vm = vm_with_groups(&env, &[]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
}

/// 살아 있는 VM에 다시 붙을 때(opld를 다시 시작한 뒤 등)도 이어서 실행하기 전에 규칙을 넣고, 넣지
/// 못하면 이어서 실행하지 않고 다시 본다.
#[tokio::test]
async fn reattaching_applies_the_firewall_rules_first() {
    let gate = Arc::new(Gate::default());
    // 응답하지 않는 VM은 재동기화로 알아채고 다시 붙는다.
    let env = env_with_config(Some(gate.clone()), |c| {
        c.resync_interval = Duration::from_millis(100)
    })
    .await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);

    *gate.fail.lock().unwrap() = vec!["nft: boom".into(); 1000];
    env.hv.set_unresponsive(vm.id, true);
    let got = env
        .wait(vm.id, "reported the firewall", |v| {
            v.status.last_error.is_some()
        })
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("VM is unresponsive: firewall: nft: boom")
    );
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(env.hv.launches().len(), 1);
}

/// 이전 opld가 멈춘 채 남긴 VM을 이어서 실행할 때도 먼저 규칙을 넣고, 넣지 못하면 멈춘 채 두고 다시 본다.
#[tokio::test]
async fn resuming_a_vm_left_paused_applies_the_firewall_rules_first() {
    let gate = Arc::new(Gate::default());
    let env = env_with_config(Some(gate.clone()), |c| {
        c.resync_interval = Duration::from_millis(100)
    })
    .await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);

    *gate.fail.lock().unwrap() = vec!["nft: boom".into(); 1000];
    env.hv.leave_paused(vm.id);
    let got = env
        .wait(vm.id, "reported the firewall", |v| {
            v.status.last_error.is_some()
        })
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("VM is unresponsive: firewall: nft: boom")
    );
    assert!(env.hv.is_paused(vm.id), "must stay paused");

    // 규칙을 넣을 수 있게 되면 이어서 실행한다.
    gate.fail.lock().unwrap().clear();
    env.wait(vm.id, "resumed", |v| v.status.last_error.is_none())
        .await;
    assert!(!env.hv.is_paused(vm.id));
    assert_eq!(env.hv.launches().len(), 1);
}
```

`crates/opl-types/src/node.rs`에서

```rust
    pub underlay: Option<IpAddr>,
}
```

을 다음으로 바꾼다:

```rust
    pub underlay: Option<IpAddr>,
    /// 이 노드에 VM 방화벽(nftables)이 있다. 보안 그룹을 건 NIC가 있는 VM은 이런 노드에서만 돈다
    /// (위트니스는 늘 false).
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub firewall: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}
```

`crates/opl-types/src/node.rs`에서

```rust
    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn status_firewall_round_trips_and_is_omitted_when_false() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("firewall").is_none(), "{none}");
        let status = NodeStatus {
            firewall: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["firewall"], true);
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

`crates/opl-types/src/vm.rs`에서

```rust
        Ok(())
    }
```

을 다음으로 바꾼다:

```rust
        Ok(())
    }

    /// 보안 그룹을 건(방화벽이 켜진) NIC가 있는지. 이런 VM은 방화벽이 있는 노드에서만 돈다.
    pub fn has_firewall(&self) -> bool {
        self.nics.iter().any(|n| !n.security_groups.is_empty())
    }
```

`crates/opl-types/src/vm.rs`에서

```rust
    #[test]
    fn nic_security_groups_default_empty_and_are_omitted() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn has_firewall_when_any_nic_has_groups() {
        let mut s = sample();
        assert!(!s.has_firewall());
        let mut second = s.nics[0].clone();
        second.security_groups = vec!["web".into()];
        s.nics.push(second);
        assert!(s.has_firewall());
    }

    #[test]
    fn nic_security_groups_default_empty_and_are_omitted() {
```

`crates/opld/src/bootstrap.rs`에서

```rust
/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
```

을 다음으로 바꾼다:

```rust
/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay, 방화벽 여부를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
```

`crates/opld/src/bootstrap.rs`에서

```rust
            underlay: underlay.or(status.underlay),
        };
```

을 다음으로 바꾼다:

```rust
            underlay: underlay.or(status.underlay),
            firewall: host.firewall.is_some(),
        };
```

`crates/opld/src/bootstrap.rs`에서

```rust
    use opl_cluster::memnet::MemRouter;
    use opl_driver::{DriverError, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, PoolState};
```

을 다음으로 바꾼다:

```rust
    use opl_cluster::memnet::MemRouter;
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, PoolState};
```

`crates/opld/src/bootstrap.rs`에서

```rust
        ) -> Result<OverlayChanges, DriverError> {
            unimplemented!()
        }
    }
```

을 다음으로 바꾼다:

```rust
        ) -> Result<OverlayChanges, DriverError> {
            unimplemented!()
        }
    }

    #[derive(Debug)]
    struct FakeFirewall;

    #[async_trait]
    impl Firewall for FakeFirewall {
        async fn apply(&self, _: &str) -> Result<(), DriverError> {
            Ok(())
        }
    }
```

`crates/opld/src/bootstrap.rs`에서

```rust
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        cluster.shutdown().await.unwrap();
```

을 다음으로 바꾼다:

```rust
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        cluster.shutdown().await.unwrap();
    }

    /// 방화벽(nftables)이 있는지 기록하고, 다시 등록하면 그때의 값으로 바꾼다.
    #[tokio::test]
    async fn registration_records_the_firewall() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let firewall = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .firewall
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        host.firewall = Some(Arc::new(FakeFirewall));
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(firewall());
        host.firewall = None;
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        cluster.shutdown().await.unwrap();
```

`crates/opld/src/firewall.rs`를 만든다:

```rust
//! 방화벽 에이전트. 클러스터의 모든 VM에서 보안 그룹을 건 NIC를 모아 이 노드의 nftables 테이블
//! `bridge opl_<태그>` 하나로 넣는다(VM이 어느 노드에 있든 — 마이그레이션·복구로 언제 올지 모른다).
//! 워커가 VM을 켜기 직전에 부르는 확인([`FirewallGate`])도 같은 일을 한다.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_driver::{Firewall, FirewallNic, ruleset};
use opl_reconciler::FirewallGate;
use opl_types::network::tap_name;
use opl_types::resource::Kind;
use opl_types::security_group::{Action, Direction, Rule, SecurityGroup, SecurityGroupSpec};
use opl_types::vm::{Vm, VmSpec};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::host::Host;

/// 바뀐 것이 없어도 규칙을 다시 넣는 간격(누가 테이블을 지웠거나 바꿨을 때)
const FIREWALL_REFRESH: Duration = Duration::from_secs(60);
/// VM·SecurityGroup이 바뀌면 이만큼 모아 한 번에 넣는다.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// 클러스터의 방화벽 NIC(보안 그룹을 건 NIC). tap은 그 NIC의 tap 이름, 규칙은 NIC의 그룹 순서·그룹
/// 안 순서로 펼친 것이다. MAC이 없는 NIC는 건너뛰고(아직 켤 수 없다), 없는 그룹은 규칙이 없는 것으로
/// 보되 그 NIC의 규칙 끝에 나가기 차단을 더한다(들어오기·나가기 모두 앞 규칙에 맞는 것만 통한다).
pub fn firewall_nics(vms: &[Vm], groups: &[SecurityGroup]) -> Vec<FirewallNic> {
    let mut nics = Vec::new();
    for vm in vms {
        for (i, nic) in vm.spec.nics.iter().enumerate() {
            if nic.security_groups.is_empty() {
                continue;
            }
            let Some(mac) = nic.mac else {
                continue;
            };
            let found: Vec<_> = nic
                .security_groups
                .iter()
                .filter_map(|name| groups.iter().find(|g| &g.name == name))
                .collect();
            let mut rules: Vec<Rule> = found
                .iter()
                .flat_map(|g| g.spec.rules.iter().cloned())
                .collect();
            if found.len() < nic.security_groups.len() {
                rules.push(Rule {
                    direction: Direction::Out,
                    action: Action::Drop,
                    protocol: None,
                    ports: None,
                    remote: None,
                });
            }
            nics.push(FirewallNic {
                tap: tap_name(vm.id, i),
                mac,
                rules,
            });
        }
    }
    nics
}

/// 지금 클러스터 상태로 규칙 글을 만들어 넣는다. 에이전트 루프와 워커의 시작 직전 확인이 함께 쓰며
/// 한 번에 하나씩만 넣는다.
pub struct FirewallRules {
    cluster: Arc<Cluster>,
    firewall: Arc<dyn Firewall>,
    tag: String,
    /// 마지막으로 넣은 글. 이 잠금이 넣기를 직렬화한다.
    last: Mutex<Option<String>>,
}

impl FirewallRules {
    /// 위트니스가 아니고 이 노드에 방화벽(nftables)이 있을 때만 있다.
    pub fn new(
        cluster: Arc<Cluster>,
        config: &Config,
        host: &Host,
        tag: String,
    ) -> Option<Arc<Self>> {
        if config.witness {
            return None;
        }
        let firewall = host.firewall.clone()?;
        Some(Arc::new(Self {
            cluster,
            firewall,
            tag,
            last: Mutex::new(None),
        }))
    }

    /// 규칙 글을 만들어 넣는다. `force`가 아니면 마지막으로 넣은 글과 같을 때 건너뛴다. 넣었으면 true.
    async fn apply(&self, force: bool) -> anyhow::Result<bool> {
        let mut last = self.last.lock().await;
        const READ: &str = "cannot read cluster state for the firewall";
        let (_, vms) = self.cluster.list::<VmSpec>().context(READ)?;
        let (_, groups) = self.cluster.list::<SecurityGroupSpec>().context(READ)?;
        let text = ruleset(&self.tag, &firewall_nics(&vms, &groups));
        if !force && last.as_deref() == Some(text.as_str()) {
            return Ok(false);
        }
        // 실패하면 다음 차례에 같은 글이어도 다시 넣는다.
        *last = None;
        self.firewall
            .apply(&text)
            .await
            .context("cannot apply the firewall rules")?;
        *last = Some(text);
        Ok(true)
    }

    /// 이 클러스터의 테이블을 지운다(클러스터에서 빠질 때).
    async fn clear(&self) -> anyhow::Result<()> {
        let mut last = self.last.lock().await;
        *last = None;
        self.firewall.apply(&ruleset(&self.tag, &[])).await?;
        Ok(())
    }
}

#[async_trait]
impl FirewallGate for FirewallRules {
    /// 바뀐 것이 없어 보여도 다시 넣는다(누가 테이블을 지웠어도 켜기 전에 되살린다).
    async fn sync(&self) -> Result<(), String> {
        self.apply(true)
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    }
}

/// 방화벽 에이전트 작업
pub struct FirewallAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    rules: Arc<FirewallRules>,
}

impl FirewallAgent {
    pub fn spawn(rules: Arc<FirewallRules>) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_firewall(rules.clone(), FIREWALL_REFRESH, rx)),
            rules,
        }
    }

    /// 멈춘다. 규칙은 그대로 둔다(VM이 계속 돈다). 다만 클러스터에서 빠졌으면(`left`, 이 노드에 배정된
    /// VM이 없다) 테이블을 지운다. 지우지 못하면 경고만 남긴다.
    pub async fn shutdown(self, left: bool) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
        if !left {
            return;
        }
        if let Err(e) = self.rules.clear().await {
            tracing::warn!(error = %e, "cannot remove the firewall rules after leaving the cluster");
        }
    }
}

async fn run_firewall(
    rules: Arc<FirewallRules>,
    refresh: Duration,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(refresh);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watch = None;
    loop {
        // 주기 차례(시작 포함)에는 바뀌지 않았어도 다시 넣는다.
        let force = tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => true,
            () = firewall_changed(&mut watch) => {
                // 잇따른 변경을 모은다. 구독이 끊긴 때도 같이 기다려 다시 구독하기를 몰아치지 않는다.
                tokio::select! {
                    _ = &mut shutdown => return,
                    () = tokio::time::sleep(DEBOUNCE) => {}
                }
                watch = None;
                false
            }
        };
        // 이번에 읽을 목록 뒤의 변경을 놓치지 않게 먼저 구독한다.
        if watch.is_none() {
            watch = rules
                .cluster
                .list::<VmSpec>()
                .ok()
                .and_then(|(revision, _)| rules.cluster.watch(revision).ok());
        }
        // 하던 적용은 끊지 않는다. 종료는 차례 사이에서만 본다.
        if let Err(e) = rules.apply(force).await {
            tracing::warn!(error = %e.root_cause(), "{e}");
        }
    }
}

/// VM이나 SecurityGroup 리소스가 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을
/// 버리고 돌아가 넣은 뒤 다시 구독하게 한다. 구독이 없으면 기다리기만 한다.
async fn firewall_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return std::future::pending().await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Vm | Kind::SecurityGroup,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Vm | Kind::SecurityGroup,
                    ..
                },
            ) => return,
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;
    use opl_cluster::types::NodeId;
    use opl_driver::DriverError;
    use opl_types::resource::Resource;
    use opl_types::vm::{CpuSpec, MacAddr, MachineType, NicModel, NicSpec, PowerState};
    use uuid::Uuid;

    use crate::bootstrap::tests::single_node;

    /// 받은 글을 기록한다. `fail`이면 실패한다.
    #[derive(Debug, Default)]
    struct RecordingFirewall {
        applied: StdMutex<Vec<String>>,
        fail: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl Firewall for RecordingFirewall {
        async fn apply(&self, ruleset: &str) -> Result<(), DriverError> {
            self.applied.lock().unwrap().push(ruleset.to_string());
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(DriverError::ToolFailed {
                    tool: "nft",
                    stderr: "boom".into(),
                });
            }
            Ok(())
        }
    }

    fn rule(json: serde_json::Value) -> Rule {
        serde_json::from_value(json).unwrap()
    }

    fn group(name: &str, rules: Vec<Rule>) -> SecurityGroup {
        Resource::new(
            name,
            SecurityGroupSpec {
                description: None,
                rules,
            },
        )
    }

    fn mac(last: u8) -> MacAddr {
        MacAddr([0x52, 0x54, 0, 0, 0, last])
    }

    fn nic(mac: Option<MacAddr>, groups: &[&str]) -> NicSpec {
        NicSpec {
            network: "lan".into(),
            model: NicModel::Virtio,
            mac,
            security_groups: groups.iter().map(|g| g.to_string()).collect(),
        }
    }

    fn vm(name: &str, node: Option<Uuid>, nics: Vec<NicSpec>) -> Vm {
        Resource::new(
            name,
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 1,
                    model: Default::default(),
                },
                memory_mib: 256,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![],
                nics,
                power: PowerState::Stopped,
                restart: Default::default(),
                node,
                snapshot_schedule: None,
                ha: false,
            },
        )
    }

    #[test]
    fn firewall_nics_expand_the_groups_of_each_nic() {
        let ssh = rule(
            serde_json::json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "22" }),
        );
        let web = rule(
            serde_json::json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "80" }),
        );
        let smtp = rule(
            serde_json::json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }),
        );
        let out_drop = rule(serde_json::json!({ "direction": "out", "action": "drop" }));
        let groups = vec![
            group("ssh", vec![ssh.clone()]),
            group("web", vec![web.clone(), smtp.clone()]),
            group("unused", vec![ssh.clone()]),
        ];
        // 노드와 상관없이 모든 VM을 본다.
        let a = vm(
            "a",
            Some(Uuid::now_v7()),
            vec![
                // 그룹 없는 NIC는 방화벽이 없다.
                nic(Some(mac(1)), &[]),
                // NIC의 그룹 순서대로 펼친다.
                nic(Some(mac(2)), &["web", "ssh"]),
                // 없는 그룹은 규칙이 없는 것이고 나가기도 막는다.
                nic(Some(mac(3)), &["gone"]),
            ],
        );
        let b = vm(
            "b",
            None,
            vec![
                // MAC이 없는 NIC는 건너뛴다.
                nic(None, &["ssh"]),
                nic(Some(mac(5)), &["ssh", "gone", "web"]),
            ],
        );
        let c = vm("c", None, vec![nic(Some(mac(6)), &[])]);
        assert_eq!(
            firewall_nics(&[a.clone(), b.clone(), c], &groups),
            vec![
                FirewallNic {
                    tap: tap_name(a.id, 1),
                    mac: mac(2),
                    rules: vec![web.clone(), smtp.clone(), ssh.clone()],
                },
                FirewallNic {
                    tap: tap_name(a.id, 2),
                    mac: mac(3),
                    rules: vec![out_drop.clone()],
                },
                FirewallNic {
                    tap: tap_name(b.id, 1),
                    mac: mac(5),
                    rules: vec![ssh, web, smtp, out_drop],
                },
            ]
        );
        assert_eq!(firewall_nics(&[], &groups), vec![]);
    }

    fn test_config(witness: bool) -> Config {
        toml::from_str(&format!(
            "name = \"n\"\naddress = \"h\"\nwitness = {witness}\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false"
        ))
        .unwrap()
    }

    fn test_host(firewall: Option<Arc<RecordingFirewall>>) -> Host {
        let mut host = Host::from_config(&test_config(false), "t").unwrap();
        host.net = None;
        host.firewall = firewall.map(|f| f as Arc<dyn Firewall>);
        host
    }

    #[tokio::test]
    async fn runs_only_on_a_non_witness_node_with_a_firewall() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let host = test_host(None);
        assert!(
            FirewallRules::new(cluster.clone(), &test_config(false), &host, "t".into()).is_none()
        );
        let host = test_host(Some(Arc::default()));
        assert!(
            FirewallRules::new(cluster.clone(), &test_config(true), &host, "t".into()).is_none()
        );
        assert!(
            FirewallRules::new(cluster.clone(), &test_config(false), &host, "t".into()).is_some()
        );
        cluster.shutdown().await.unwrap();
    }

    fn rules_for(cluster: &Arc<Cluster>, firewall: &Arc<RecordingFirewall>) -> Arc<FirewallRules> {
        FirewallRules::new(
            cluster.clone(),
            &test_config(false),
            &test_host(Some(firewall.clone())),
            "abc".into(),
        )
        .unwrap()
    }

    /// 같은 글이면 다시 넣지 않고, 넣기가 실패했거나 강제면(주기 차례, 시작 직전 확인) 다시 넣는다.
    #[tokio::test]
    async fn skips_the_same_ruleset_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let firewall = Arc::new(RecordingFirewall::default());
        let rules = rules_for(&cluster, &firewall);
        let count = || firewall.applied.lock().unwrap().len();

        assert!(rules.apply(false).await.unwrap());
        assert_eq!(
            *firewall.applied.lock().unwrap(),
            ["table bridge opl_abc {}\ndelete table bridge opl_abc\n"]
        );
        assert!(!rules.apply(false).await.unwrap());
        assert_eq!(count(), 1);
        assert!(rules.apply(true).await.unwrap());
        assert_eq!(count(), 2);
        rules.sync().await.unwrap();
        assert_eq!(count(), 3);

        // 실패하면 같은 글이어도 다음 차례에 다시 넣는다.
        firewall
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err = rules.sync().await.unwrap_err();
        assert!(
            err.contains("cannot apply the firewall rules") && err.contains("boom"),
            "{err}"
        );
        firewall
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(rules.apply(false).await.unwrap());
        assert_eq!(count(), 5);
        assert!(!rules.apply(false).await.unwrap());
        cluster.shutdown().await.unwrap();
    }

    /// 조건이 맞을 때까지 기다린다.
    async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(tokio::time::Instant::now() < deadline, "{what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// VM·그룹이 바뀌면 주기를 기다리지 않고 다시 넣는다. 규칙 글이 같으면(방화벽과 상관없는 변경)
    /// 다시 넣지 않는다.
    #[tokio::test]
    async fn reapplies_on_vm_and_group_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(me)) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let firewall = Arc::new(RecordingFirewall::default());
        let rules = rules_for(&cluster, &firewall);
        let (stop, rx) = oneshot::channel();
        // 주기는 길게 둔다. 시작할 때 한 번, 그 뒤로는 바뀔 때마다 넣는다.
        let task = tokio::spawn(run_firewall(rules, Duration::from_secs(3600), rx));
        let applied = || firewall.applied.lock().unwrap().clone();
        let last = || applied().last().cloned().unwrap_or_default();

        wait_for("first apply", || applied().len() == 1).await;
        let web = group(
            "web",
            vec![rule(
                serde_json::json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "80" }),
            )],
        );
        cluster.create(&web).await.unwrap();
        let guarded = vm("guarded", Some(me), vec![nic(Some(mac(1)), &["web"])]);
        cluster.create(&guarded).await.unwrap();
        let tap = tap_name(guarded.id, 0);
        wait_for("apply after the VM is created", || {
            last().contains(&format!("chain i_{tap} {{")) && last().contains("th dport 80 accept")
        })
        .await;

        // 그룹 규칙을 바꾸면 곧바로 다시 넣는다.
        let mut spec = web.spec.clone();
        spec.rules = vec![rule(
            serde_json::json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" }),
        )];
        cluster
            .update_spec(web.id, None, web.labels.clone(), &spec)
            .await
            .unwrap();
        wait_for("apply after the group changes", || {
            last().contains("th dport 443 accept") && !last().contains("th dport 80 accept")
        })
        .await;

        // 방화벽과 상관없는 VM은 글을 바꾸지 않으므로 다시 넣지 않는다.
        let before = applied().len();
        let plain = vm("plain", Some(me), vec![nic(Some(mac(2)), &[])]);
        cluster.create(&plain).await.unwrap();
        tokio::time::sleep(DEBOUNCE * 3).await;
        assert_eq!(applied().len(), before);

        // VM을 지우면 테이블을 지우기만 하는 글이 된다.
        cluster.delete::<VmSpec>(guarded.id, None).await.unwrap();
        wait_for("apply after the VM is deleted", || {
            last() == "table bridge opl_abc {}\ndelete table bridge opl_abc\n"
        })
        .await;
        stop.send(()).unwrap();
        task.await.unwrap();
        cluster.shutdown().await.unwrap();
    }

    /// 클러스터에서 빠질 때만 테이블을 지운다.
    #[tokio::test]
    async fn leaving_removes_the_table() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(me)) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        cluster.create(&group("web", vec![])).await.unwrap();
        cluster
            .create(&vm("guarded", Some(me), vec![nic(Some(mac(1)), &["web"])]))
            .await
            .unwrap();
        let firewall = Arc::new(RecordingFirewall::default());
        let applied = || firewall.applied.lock().unwrap().clone();
        let empty = "table bridge opl_abc {}\ndelete table bridge opl_abc\n";

        let agent = FirewallAgent::spawn(rules_for(&cluster, &firewall));
        wait_for("first apply", || applied().len() == 1).await;
        agent.shutdown(false).await;
        assert_eq!(applied().len(), 1);
        assert!(applied()[0].contains("chain i_"));

        let agent = FirewallAgent::spawn(rules_for(&cluster, &firewall));
        wait_for("first apply", || applied().len() == 2).await;
        agent.shutdown(true).await;
        assert_eq!(applied().len(), 3);
        assert_eq!(applied()[2], empty);
        cluster.shutdown().await.unwrap();
    }
}
```

`crates/opld/src/host.rs`에서

```rust

use opl_driver::{HostNet, IpRoute, QemuConfig};
use opl_qemu::{Accel, HostProfile};
```

을 다음으로 바꾼다:

```rust

use opl_driver::{Firewall, HostNet, IpRoute, Nft, QemuConfig};
use opl_qemu::{Accel, HostProfile};
```

`crates/opld/src/host.rs`에서

```rust
    pub net: Option<Arc<dyn HostNet>>,
    pub systemd_scope: bool,
```

을 다음으로 바꾼다:

```rust
    pub net: Option<Arc<dyn HostNet>>,
    /// VM 방화벽(nftables)을 넣는다. Linux에서 규칙을 넣을 수 있을 때만 있다([`Host::detect_firewall`]).
    pub firewall: Option<Arc<dyn Firewall>>,
    pub systemd_scope: bool,
```

`crates/opld/src/host.rs`에서

```rust
            net,
            systemd_scope: config.hypervisor.systemd_scope,
        })
    }
```

을 다음으로 바꾼다:

```rust
            net,
            firewall: None,
            systemd_scope: config.hypervisor.systemd_scope,
        })
    }

    /// Linux에서 클러스터 태그 `tag`의 테이블에 실제로 규칙을 넣을 수 있으면 VM 방화벽을 켠다. 안
    /// 되면(nftables가 없음, root가 아님) 보안 그룹을 건 VM이 이 노드에서 돌지 않는다고 알린다.
    pub async fn detect_firewall(&mut self, tag: &str) {
        if !cfg!(target_os = "linux") {
            return;
        }
        self.firewall = probe_firewall(Arc::new(Nft::default()), tag).await;
    }
```

`crates/opld/src/host.rs`에서

```rust
            }
        }
```

을 다음으로 바꾼다:

```rust
            }
        }
    }
}

/// `firewall`로 테이블 `bridge opl_<tag>`를 (없으면) 만드는 글을 넣어 본다. 되면 `firewall`을 돌려준다.
/// 있는 테이블과 규칙은 그대로 둔다(opld를 다시 시작해도 도는 VM을 계속 지킨다).
async fn probe_firewall(firewall: Arc<dyn Firewall>, tag: &str) -> Option<Arc<dyn Firewall>> {
    match firewall
        .apply(&format!("table bridge opl_{tag} {{}}\n"))
        .await
    {
        Ok(()) => Some(firewall),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "cannot apply nftables rules; VMs with security groups cannot run on this node"
            );
            None
        }
```

`crates/opld/src/host.rs`에서

```rust
mod tests {
    #[test]
```

을 다음으로 바꾼다:

```rust
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use opl_driver::{DriverError, Firewall};

    /// 받은 글을 기록하고 `ok`가 아니면 실패한다.
    #[derive(Debug)]
    struct FakeFirewall {
        ok: bool,
        applied: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Firewall for FakeFirewall {
        async fn apply(&self, ruleset: &str) -> Result<(), DriverError> {
            self.applied.lock().unwrap().push(ruleset.to_string());
            if self.ok {
                Ok(())
            } else {
                Err(DriverError::ToolFailed {
                    tool: "nft",
                    stderr: "Operation not permitted".into(),
                })
            }
        }
    }

    /// 방화벽은 이 클러스터의 테이블을 (지우지 않고) 만들어 보아 될 때만 켠다.
    #[tokio::test]
    async fn the_firewall_is_on_only_when_a_probe_applies() {
        for ok in [true, false] {
            let fake = Arc::new(FakeFirewall {
                ok,
                applied: Mutex::default(),
            });
            let got = super::probe_firewall(fake.clone(), "abc").await;
            assert_eq!(got.is_some(), ok);
            assert_eq!(*fake.applied.lock().unwrap(), ["table bridge opl_abc {}\n"]);
        }
    }

    #[test]
```

`crates/opld/src/main.rs`에서

```rust
mod control;
mod host;
```

을 다음으로 바꾼다:

```rust
mod control;
mod firewall;
mod host;
```

`crates/opld/src/member.rs` 전체:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태, VXLAN 장치, VM 방화벽. 위트니스는
//! 노드 간 서버와 클러스터 저장소, 승격, Node 등록만 띄운다.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::serve::ListenerExt;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{
    AppState, ClusterJanitor, HaManager, HaManagerConfig, IncomingConfig, IncomingMigrations,
    SnapshotScheduler, TaskRunner, TaskRunnerConfig, UefiVarsFetcher,
};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError, Typed};
use opl_driver::{
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage, migration_tls_dir, write_migration_tls,
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirewallGate, FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::firewall::{FirewallAgent, FirewallRules};
use crate::host::Host;
use crate::overlay::{OverlayAgent, cluster_tag};
use crate::pools::PoolMounter;

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록을 다시 확인하는 간격. 관리자가 브리지를 만들면 이만큼 뒤에 스케줄러가 안다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
/// 다시 시작한 learner가 승격을 다시 요청하는 간격. 실패할 때마다 두 배로 늘려 최대 시간까지 둔다.
const PROMOTE_RETRY_MIN: Duration = Duration::from_secs(5);
const PROMOTE_RETRY_MAX: Duration = Duration::from_secs(60);

pub fn pki_dir(config: &Config) -> PathBuf {
    config.data_dir.join(PKI_DIR)
}

pub fn cluster_dir(config: &Config) -> PathBuf {
    config.data_dir.join(CLUSTER_DIR)
}

/// 멤버가 되는 방법
pub enum Bootstrap {
    /// 이미 멤버다(다시 시작).
    Restart,
    /// 이 노드 하나로 새 클러스터를 만든다.
    Init { name: String, ca: ClusterCa },
    /// 합류가 받아들여졌다(이 노드는 이미 learner다). 인증서를 저장한 뒤 `via`(받아 준 노드의
    /// 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 위트니스가 `oplctl cluster init`을 받았을 때의 답
pub const WITNESS_CANNOT_INIT: &str =
    "a witness cannot start a cluster; join an existing cluster instead";

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config(witness: bool) -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        // 위트니스는 투표는 하지만 후보가 되지 않는다. 리더 일은 늘 일반 노드가 한다.
        enable_elect: !witness,
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        // 스냅샷 조각마다 걸리는 제한 시간(밀리초). 기본 200ms로는 JSON으로 부푼 1MiB 조각(3.5MiB쯤)이
        // 바쁜 노드에서 끝나지 못할 수 있다.
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

struct Server<E> {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), E>>,
}

/// 위트니스가 띄우지 않는 것: HTTPS API와 VM·풀을 다스리는 일, 리더의 일
struct Services {
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    schedule: SnapshotScheduler,
    janitor: ClusterJanitor,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
    recovery: HaManager,
    pools: PoolMounter,
    /// VXLAN 네트워크의 장치를 맞춘다(호스트 네트워크가 없으면 없다).
    overlay: Option<OverlayAgent>,
    /// VM 방화벽 규칙을 맞춘다(방화벽이 없으면 없다).
    firewall: Option<FirewallAgent>,
    /// HA 리스와 watchdog(`[ha] fencing = "off"`면 없다)
    ha: Option<Arc<HaGuard>>,
}

impl Services {
    /// `left`면 클러스터에서 빠진 것이다(VXLAN 장치와 방화벽 테이블을 지운다).
    async fn shutdown(mut self, left: bool) {
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.monitor.shutdown().await;
        self.schedule.shutdown().await;
        self.janitor.shutdown().await;
        self.recovery.shutdown().await;
        self.pools.shutdown().await;
        if let Some(overlay) = self.overlay {
            overlay.shutdown(left).await;
        }
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        // 작업자가 모두 멈춘 뒤에 멈춰야 테이블을 지운 뒤 시작 직전 확인이 다시 넣지 않는다.
        if let Some(firewall) = self.firewall {
            firewall.shutdown(left).await;
        }
        // 작업자가 모두 멈춘 뒤에 닫아야 도는 HA VM 목록이 맞다.
        if let Some(ha) = &self.ha {
            ha.close();
        }
    }
}

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    /// 위트니스면 없다.
    services: Option<Services>,
    register: JoinHandle<()>,
    promotion: JoinHandle<()>,
    /// 서버 태스크가 끝나면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 인증서가 있으면 멤버로 보므로, 처음 멤버가 되는 경우 `Init`은 마지막에
    /// 노드 인증서를 저장해 도중에 실패하면 이 노드는 계속 미초기화 상태다. `Join`은 이미 learner로
    /// 더해졌으므로 승격을 요청하기 전에 저장한다. 그 뒤에 실패하거나 죽어도 클러스터가 투표 멤버로
    /// 세는 노드가 미초기화로 남지 않고, 다시 시작하면(`Restart`) 승격을 다시 요청한다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        let id = NodeId(node);
        let peers = Arc::new(Peers::new(&tls));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config(config.witness);
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let cluster = Arc::new(
            Cluster::open(cluster_config, GrpcNetwork::new(peers.clone()))
                .await?
                .with_forwarder(peers.clone()),
        );
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
        if matches!(bootstrap, Bootstrap::Restart)
            && let Err(e) = bootstrap::check_role(&cluster, config)
        {
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        if config.witness {
            let member = Self::start_witness(config, cluster, peers, tls, bootstrap).await?;
            return Ok((member, None));
        }
        // 포트가 이미 쓰이고 있으면 아무것도 띄우기 전에 실패한다.
        let ports = async {
            let api = TcpListener::bind(config.listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.listen))?;
            let peer = TcpListener::bind(config.cluster_listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.cluster_listen))?;
            anyhow::Ok((api, peer))
        };
        let (api_listener, peer_listener) = match ports.await {
            Ok(listeners) => listeners,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 이 클러스터가 만든 VXLAN 장치의 표시. 남의 관리 이름 브리지는 브리지 목록에서 뺀다.
        let host = cluster_tag(&tls.ca_pem).and_then(|tag| {
            let host = Host::from_config(config, &tag)?;
            Ok((host, tag))
        });
        let (mut host, tag) = match host {
            Ok(host) => host,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 등록 때 방화벽 여부를 기록하므로 등록 전에 찾는다.
        host.detect_firewall(&tag).await;
        // 워커와 마이그레이션 받기가 VM을 켜기 직전에 규칙을 넣는다. 에이전트는 아래에서 띄운다.
        let firewall = FirewallRules::new(cluster.clone(), config, &host, tag.clone());
        let gate = firewall.clone().map(|f| f as Arc<dyn FirewallGate>);
        let hv: Arc<dyn Hypervisor> = match QemuHypervisor::new(host.qemu_config(config)).await {
            Ok(hv) => Arc::new(hv),
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e.into());
            }
        };
        // QEMU가 마이그레이션 TLS에 쓰는 노드 인증서. 드라이버의 state_dir은 데이터 디렉터리다(host.rs).
        let tls_dir = migration_tls_dir(&config.data_dir);
        if let Err(e) = write_migration_tls(&tls_dir, &tls.ca_pem, &tls.cert_pem, &tls.key_pem) {
            let _ = cluster.shutdown().await;
            return Err(
                anyhow::Error::new(e).context(format!("cannot write {}", tls_dir.display()))
            );
        }
        // 계획 20 전에 받은 노드 인증서는 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가
        // 실패한다. 노드가 클러스터를 나갔다 다시 들어오면 새 인증서를 받는다.
        if let Ok(false) = opl_peer::pki::allows_key_encipherment(&tls.cert_pem) {
            tracing::warn!(
                "this node's certificate lacks the keyEncipherment key usage; live migration over TLS \
                 will fail with QEMU 8.2 or older until the node leaves and rejoins the cluster"
            );
        }
        // NFS 풀의 마운트 지점은 `{data_dir}/pools/{풀 id}`다.
        let mount_root = config.data_dir.join("pools");
        // 풀 백엔드 종류로 나눠 보내는 저장소 하나를 reconciler와 Task 실행기가 함께 쓴다.
        let files: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, &mount_root));
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let lvm_shared = LvmSharedStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd))
                .with_lvm_shared(Arc::new(lvm_shared)),
        );
        let (exited_tx, exited) = mpsc::channel(2);
        // 받는 VM은 원본 노드에 배정된 채이므로 reconciler 없이 받는다.
        let incoming = IncomingMigrations::new(
            cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node,
                mount_root: mount_root.clone(),
                storage: storage.clone(),
                listen_host: config.migration_listen_host(),
                ports: config.migration.ports.clone(),
            },
        );
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming.with_firewall(gate.clone())))
            .with_uefi_vars(config.data_dir.clone());
        let peer = serve_peer(peer_listener, &tls, service, exited_tx.clone());

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let init = matches!(bootstrap, Bootstrap::Init { .. });
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password =
                        Some(bootstrap::create_cluster(&cluster, config, &host, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_node(&cluster, config, &host).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
            let created = match password {
                Some(admin_password) => {
                    let token = issue_join_token(&state, DEFAULT_JOIN_TTL)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.message))?;
                    Some(Created {
                        admin_password,
                        join_url: token.url,
                        ca_fingerprint: fingerprint(&tls.ca_pem)?,
                    })
                }
                None => None,
            };
            let api_tls = api_server_config(&tls)?;
            if init {
                tls.save(&pki_dir(config))?;
            }
            anyhow::Ok((state, created, api_tls))
        };
        let (state, created, api_tls) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = peer.stop.send(());
                let _ = peer.task.await;
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };

        // 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스는 뒤에서 기록한다.
        let promotion = spawn_promotion(first_time, &cluster, &peers);
        // 풀 마운터가 QEMU의 rbd 블록 드라이버를 확인한다.
        let qemu_binary = host.profile.qemu_binary.clone();
        // 받은 UEFI 변수 파일의 크기를 이 노드의 템플릿과 견준다.
        let uefi_vars_template = host.profile.uefi_vars_template.clone();
        // 장치를 만들거나 지우면 곧바로 브리지 목록을 기록한다(아래 주기 갱신을 기다리지 않는다).
        let overlay = OverlayAgent::spawn(cluster.clone(), config, &host, tag);
        let firewall = firewall.map(FirewallAgent::spawn);
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_node(&cluster, &config, &host).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_bridges(&cluster, &host).await {
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다(시작, 스냅샷, 복제).
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
            uefi_vars_template,
        ));
        reconciler_config.firmware = Some(firmware.clone());
        // 펜싱이 켜져 있으면 지금부터 리스를 갱신한다(지금이 stale을 세는 기준점이다).
        let ha = config
            .ha
            .guard_config()
            .map(|ha| HaGuard::spawn(cluster.clone(), ha));
        reconciler_config.ha = ha.clone();
        reconciler_config.firewall = gate;
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let pools = PoolMounter::spawn(
            cluster.clone(),
            mount_root.clone(),
            qemu_binary,
            config.pools.refresh(),
        );
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        // QEMU 드라이버의 state_dir과 같다(host.rs).
        runner_config.state_dir = Some(config.data_dir.clone());
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
        runner_config.peers = Some(peers.clone());
        runner_config.firmware = Some(firmware);
        runner_config.migration_bandwidth =
            (config.migration.bandwidth_mib > 0).then_some(config.migration.bandwidth_mib << 20);
        runner_config.migration_timeout = config.migration.timeout;
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            hv,
            reconciler.handle(),
            runner_config,
        )
        .await;
        let monitor = NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default());

        let listener = TlsListener::new(api_listener, api_tls)?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "opld is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let state = Arc::new(state);
        let schedule = SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config());
        let janitor = ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config());
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
        let app = opl_api::router(state).merge(crate::web::router());
        let task = tokio::spawn(async move {
            // 연결 주소(클라이언트 IP)를 시도 제한에 넘긴다. axum은 `TapIo`로 감싼 리스너에
            // `ConnectInfo<SocketAddr>`를 만들어 주므로 아무 일도 하지 않는 tap을 붙인다.
            let listener = listener.tap_io(|_| {});
            let r = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
            let _ = exited_tx.send(format!("API server stopped: {r:?}")).await;
            r
        });
        let member = Member {
            config: config.clone(),
            cluster,
            peer,
            services: Some(Services {
                api: Server { stop, task },
                reconciler,
                runner,
                monitor,
                schedule,
                janitor,
                recovery,
                pools,
                overlay,
                firewall,
                ha,
            }),
            register,
            promotion,
            exited,
        };
        Ok((member, created))
    }

    /// 위트니스로 시작한다. 노드 간 서버(하이퍼바이저 없음)와 승격, Node 등록만 띄우고 QEMU를 찾지
    /// 않는다. 위트니스는 클러스터를 만들 수 없다(`Init`은 main이 먼저 거부한다).
    async fn start_witness(
        config: &Config,
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<Self> {
        let peer_listener = match TcpListener::bind(config.cluster_listen)
            .await
            .with_context(|| format!("cannot listen on {}", config.cluster_listen))
        {
            Ok(listener) => listener,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        // 마이그레이션과 UEFI 변수는 받지 않는다.
        let service = PeerService::new(cluster.clone(), None);
        let peer = serve_peer(peer_listener, &tls, service, exited_tx);
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { .. } => anyhow::bail!(WITNESS_CANNOT_INIT),
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_witness(&cluster, config).await?;
                }
            }
            anyhow::Ok(())
        };
        if let Err(e) = prepared.await {
            let _ = peer.stop.send(());
            let _ = peer.task.await;
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        let promotion = spawn_promotion(first_time, &cluster, &peers);
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
            })
        };
        tracing::info!(
            node = %cluster.id(),
            cluster = %config.cluster_listen,
            "opld is ready as a witness"
        );
        Ok(Member {
            config: config.clone(),
            cluster,
            peer,
            services: None,
            register,
            promotion,
            exited,
        })
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// API 서버나 노드 간 서버가 스스로 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나, 남는 투표 멤버가 없거나 모두 위트니스면
    /// 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let (_, nodes) = self.cluster.list::<NodeSpec>()?;
        check_leave(me, &self.cluster.members().voters, &nodes)?;
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        if let Some(vm) = vms
            .iter()
            .find(|vm| vm.spec.node == Some(me.0) || vm.status.node == Some(me.0))
        {
            anyhow::bail!(
                "VM {:?} is still assigned to this node; delete it or move it to another node first",
                vm.name
            );
        }
        // 멤버십에서 빠지면 다른 노드가 이 노드의 요청을 받지 않으므로 Node 리소스부터 지운다.
        match self.cluster.delete::<NodeSpec>(me.0, None).await {
            Ok(_) | Err(ClusterError::Rejected(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.cluster
            .request(LeaderRequest::Remove { id: me })
            .await?;
        Ok(())
    }

    /// 모든 것을 멈춘다. `left`면 클러스터에서 빠진 것이므로 클러스터 저장소와 노드 인증서를 지운다.
    /// 아니면 오프라인으로 기록한다. 실행 중인 VM은 그대로 둔다.
    pub async fn close(mut self, left: bool) -> anyhow::Result<()> {
        if let Some(services) = self.services.take() {
            services.shutdown(left).await;
        }
        self.register.abort();
        self.promotion.abort();
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        // 오래 가는 콘솔 중계 스트림이 있으면 서버가 끝나지 않으므로 기다리는 시간을 제한한다.
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.peer.task)
            .await
            .is_err()
        {
            self.peer.task.abort();
        }
        let shutdown = self.cluster.shutdown().await;
        if left {
            // 이미 클러스터에서 빠졌으므로 저장소를 닫지 못했어도 지운다. 남기면 빠진 멤버로 다시 뜬다.
            if let Err(e) = &shutdown {
                tracing::warn!(error = %e, "cannot shut down the cluster store; removing it anyway");
            }
            match std::fs::remove_dir_all(pki_dir(&self.config)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            }
            remove_cluster_data(&self.config)?;
        }
        shutdown?;
        Ok(())
    }
}

/// 이 노드가 빠진 뒤에도 리더가 될 수 있는 투표 멤버가 남는지 본다. 위트니스는 후보가 되지 않으므로
/// 위트니스만 남으면 클러스터에 리더가 영영 생기지 않는다. Node 리소스가 없는 투표 멤버는 일반 노드로
/// 본다(`get_cluster`처럼).
fn check_leave(
    me: NodeId,
    voters: &BTreeMap<NodeId, String>,
    nodes: &[Typed<NodeSpec>],
) -> anyhow::Result<()> {
    let mut rest = voters.keys().filter(|&&id| id != me).peekable();
    if rest.peek().is_none() {
        anyhow::bail!(
            "this node is the only voting member; stop opld and remove its data directory instead"
        );
    }
    let witness = |id: &NodeId| nodes.iter().any(|n| n.id == id.0 && n.spec.witness);
    if rest.all(witness) {
        anyhow::bail!(
            "the remaining voting members are all witnesses, which cannot lead; add another \
             non-witness node first"
        );
    }
    Ok(())
}

/// 노드 간 서버를 띄운다. 서버가 스스로 멈추면 `exited`로 알린다.
fn serve_peer(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    exited: mpsc::Sender<String>,
) -> Server<opl_peer::ServeError> {
    let (stop, rx) = oneshot::channel::<()>();
    let tls = tls.clone();
    let task = tokio::spawn(async move {
        let r = opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await;
        let _ = exited.send(format!("cluster server stopped: {r:?}")).await;
        r
    });
    Server { stop, task }
}

/// 다시 시작한 노드는 투표 멤버가 될 때까지 승격을 다시 요청한다. 처음 멤버가 된 노드는 이미 했다.
fn spawn_promotion(first_time: bool, cluster: &Arc<Cluster>, peers: &Arc<Peers>) -> JoinHandle<()> {
    if first_time {
        return tokio::spawn(async {});
    }
    let (cluster, peers) = (cluster.clone(), peers.clone());
    tokio::spawn(async move { promote_until_voter(&cluster, &peers).await })
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류하다 투표 멤버가 되기 전에 멈춘 노드는 learner로 남아 있다. 다시 시작하면 투표 멤버가 될
/// 때까지, 또는 클러스터에서 빠진 것을 알 때까지 승격을 다시 요청한다.
async fn promote_until_voter(cluster: &Cluster, peers: &Peers) {
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지 모른다.
    while cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .nodes()
                    .any(|(n, _)| *n == id)
            },
            "this node is in the membership",
        )
        .await
        .is_err()
    {
        tracing::info!("this node is not in the cluster membership it knows yet; still waiting");
    }
    let mut retry = PROMOTE_RETRY_MIN;
    let mut asked = false;
    loop {
        let members = cluster.members();
        if members.voters.contains_key(&id) {
            if asked {
                tracing::info!("this node became a voting member");
            } else {
                tracing::info!("this node is a voting member");
            }
            return;
        }
        if !members.learners.contains_key(&id) {
            tracing::info!("this node was removed from the cluster; not asking to be promoted");
            return;
        }
        tracing::info!("this node is not a voting member yet; asking to be promoted");
        asked = true;
        match promote(cluster, peers, None).await {
            Ok(()) => {}
            Err(e) if matches!(e.downcast_ref(), Some(ClusterError::Rejected(_))) => {
                tracing::info!(
                    error = format!("{e:#}"),
                    "this node was removed from the cluster; not asking to be promoted"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "cannot become a voting member; will retry"
                );
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(PROMOTE_RETRY_MAX);
            }
        }
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다. `via`가
/// 없으면(다시 시작한 노드) 이 노드가 아는 리더나 투표 멤버에게 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: Option<&str>) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut redirect: Option<String> = None;
    loop {
        // 다시 시작한 직후에는 리더를 아직 모를 수 있다. 알 때까지 기다린다.
        let Some(addr) = redirect
            .take()
            .or_else(|| via.map(str::to_string))
            .or_else(|| voter_addr(cluster))
        else {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("cannot become a voting member: no voting member is known");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => redirect = Some(leader),
            // 리더의 멤버십에 learner로 없다(클러스터에서 빠졌다). 다시 시도해도 소용없다.
            Err(e @ ClusterError::Rejected(_)) => {
                return Err(e).context("this node is not in the cluster membership");
            }
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::info!(error = %e, "waiting to become a voting member");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e).context("cannot become a voting member"),
        }
    }
    cluster
        .raft()
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .voter_ids()
                    .any(|v| v == id)
            },
            "this node is a voter",
        )
        .await?;
    Ok(())
}

/// 이 노드가 아는 리더의 주소, 모르면 아무 투표 멤버의 주소
fn voter_addr(cluster: &Cluster) -> Option<String> {
    let members = cluster.members();
    members
        .leader
        .and_then(|leader| members.voters.get(&leader))
        .or_else(|| members.voters.values().next())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::resource::Resource;

    #[test]
    fn a_witness_does_not_stand_for_election() {
        assert!(raft_config(false).enable_elect);
        assert!(!raft_config(true).enable_elect);
    }

    /// 남는 투표 멤버가 없거나 모두 위트니스면(리더가 될 수 없다) 빠지지 않는다.
    #[test]
    fn leaving_keeps_a_voter_that_can_lead() {
        let node = |witness: bool| {
            let mut node = Resource::new(
                "n",
                NodeSpec {
                    address: "h:8006".into(),
                    maintenance: false,
                    witness,
                },
            );
            node.id = Uuid::now_v7();
            node
        };
        let (a, b, w) = (node(false), node(false), node(true));
        let id = |n: &Typed<NodeSpec>| NodeId(n.id);
        let voters = |nodes: &[&Typed<NodeSpec>]| {
            nodes
                .iter()
                .map(|n| (id(n), String::new()))
                .collect::<BTreeMap<_, _>>()
        };
        let nodes = [a.clone(), b.clone(), w.clone()];

        // 혼자 남은 투표 멤버
        let only = check_leave(id(&a), &voters(&[&a]), &nodes).unwrap_err();
        assert!(only.to_string().contains("only voting member"), "{only}");
        // 위트니스만 남는다.
        let err = check_leave(id(&a), &voters(&[&a, &w]), &nodes).unwrap_err();
        assert!(err.to_string().contains("all witnesses"), "{err}");
        // 일반 노드가 남으면 누구든 빠질 수 있다. 위트니스도.
        check_leave(id(&a), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &w]), &nodes).unwrap();
        // Node 리소스가 없는 투표 멤버는 일반 노드로 본다(get_cluster처럼).
        check_leave(id(&a), &voters(&[&a, &b, &w]), &[a.clone(), w.clone()]).unwrap();
    }
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 931개, 실패 0

Run: `cargo test -p opl-reconciler --test firewall`
Expected: PASS

Run: `cargo test -p opld --bin opld firewall`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 330개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat: 노드 방화벽 — NodeStatus.firewall, 스케줄러 조건, opld 방화벽 에이전트, 켜기 직전 FirewallGate"
```

---

### Task 5: oplctl·웹에서 보안 그룹

oplctl `security-group list|get|create|delete`, `security-group rule add NAME RULE [--at N]`·`rule rm NAME N`(RULE은 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`), `vm create --nic NET[,sg=A+B]`, `vm nic-sg VM INDEX [GROUP]...`. 웹: 보안 그룹 목록·만들기·지우기, 규칙 표 편집 페이지(순서 바꾸기, PATCH `If-Match`), 사이드바, VM NIC마다 그룹 고르기(실행 중이면 NIC 추가·삭제·MAC 편집을 막고 이유), 오류 문구(`security_group_in_use`, `firewall_unsupported`), en·ko.

**Files:**
- Create: `web/src/security-groups/SecurityGroupPage.test.tsx`
- Create: `web/src/security-groups/SecurityGroupPage.tsx`
- Create: `web/src/security-groups/SecurityGroupsPage.test.tsx`
- Create: `web/src/security-groups/SecurityGroupsPage.tsx`
- Create: `web/src/security-groups/rules.test.ts`
- Create: `web/src/security-groups/rules.ts`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/api/LiveProvider.test.tsx`
- Modify: `web/src/api/live.ts`
- Modify: `web/src/api/types.ts`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/i18n.test.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/layout/AppLayout.tsx`
- Modify: `web/src/test/fixtures.ts`
- Modify: `web/src/test/render.tsx`
- Modify: `web/src/vms/VmDetailPage.test.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmFormPage.test.tsx`
- Modify: `web/src/vms/VmFormPage.tsx`
- Modify: `web/src/vms/form.test.ts`
- Modify: `web/src/vms/form.ts`
- Modify: `web/src/vms/form/NicsSection.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, MIGRATE_WAIT, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지나 opld가 만드는 VXLAN 네트워크)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    /// VM NIC에 거는 방화벽 규칙 묶음. nftables가 있는 노드가 적용한다.
    SecurityGroup {
        #[command(subcommand)]
        cmd: SecurityGroupCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin")]
    user: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get { node: String },
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀, 공유 LVM 풀을 만든다.
    Create(Box<PoolCreate>),
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
struct PoolSet {
    pool: String,
    #[command(flatten)]
    sharing: Sharing,
}

/// `pool set`의 공유 여부. 둘 중 하나를 꼭 준다.
#[derive(Args)]
#[group(required = true, multiple = false)]
struct Sharing {
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long)]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`)
fn pool_set_body(sharing: &Sharing) -> Value {
    json!({ "shared": sharing.shared })
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd", "lvm_shared"])))]
struct PoolCreate {
    name: String,
    /// 풀 디렉터리(절대 경로)
    #[arg(long, conflicts_with = "nfs")]
    path: Option<PathBuf>,
    /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
    #[arg(long, value_parser = parse_nfs)]
    nfs: Option<NfsSource>,
    /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
    #[arg(long, requires = "nfs", conflicts_with = "path")]
    nfs_options: Option<String>,
    /// 관리자가 준비한 VG와 thin pool `VG/THINPOOL`. 노드 로컬 풀이고 볼륨은 raw만 담는다.
    #[arg(long, value_parser = parse_lvm_thin, conflicts_with_all = ["path", "nfs", "nfs_options", "shared"])]
    lvm_thin: Option<LvmThinSource>,
    /// 관리자가 준비한 ZFS 데이터셋 `POOL/DATASET`. 노드 로컬 풀이고 볼륨은 raw zvol만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "shared"])]
    zfs: Option<String>,
    /// 관리자가 준비한 Ceph 풀과 RBD 네임스페이스 `POOL[/NAMESPACE]`. 늘 공유 풀이고 볼륨은 raw
    /// 이미지만 담는다.
    #[arg(long, value_parser = parse_rbd, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "shared"])]
    rbd: Option<RbdSource>,
    /// RBD 풀에 쓸 Ceph 클라이언트 id(`client.` 없이). 생략하면 admin.
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_user: Option<String>,
    /// RBD 풀에 쓸 ceph.conf 절대 경로. 생략하면 Ceph 기본(/etc/ceph/ceph.conf).
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_conf: Option<String>,
    /// 관리자가 준비한 공유 VG(lvmlockd). 늘 공유 풀이고 볼륨은 raw 두꺼운 LV만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "rbd", "rbd_user", "rbd_conf", "shared"])]
    lvm_shared: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃의 포털 `HOST[:PORT]`(IPv6는 `[ADDR][:PORT]`). 노드가 로그인한다.
    #[arg(long, requires_all = ["lvm_shared", "iscsi_target"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_portal: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃 이름(IQN)
    #[arg(long, requires_all = ["lvm_shared", "iscsi_portal"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_target: Option<String>,
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD·공유 LVM 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
}

/// `pool create`의 요청 본문. NFS·RBD·공유 LVM 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
fn pool_create_body(args: PoolCreate) -> Value {
    let PoolCreate {
        name,
        path,
        nfs,
        nfs_options,
        lvm_thin,
        zfs,
        rbd,
        rbd_user,
        rbd_conf,
        lvm_shared,
        iscsi_portal,
        iscsi_target,
        shared,
        nodes,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd, lvm_shared) {
        (Some(n), _, _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _, _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r), _) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None, Some(vg)) => {
            // 포털과 타깃은 clap이 함께 받게 한다.
            let iscsi = iscsi_portal
                .zip(iscsi_target)
                .map(|(portal, target)| json!({ "portal": portal, "target": target }));
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": iscsi })
        }
        (None, None, None, None, None) => json!({ "kind": "dir", "path": path }),
    };
    let mut spec = json!({ "backend": backend, "nodes": nodes });
    if shared {
        spec["shared"] = json!(true);
    }
    json!({ "name": name, "spec": spec })
}

/// `--nfs`로 받은 NFS 서버와 내보내기 경로
#[derive(Debug, Clone, PartialEq, Eq)]
struct NfsSource {
    server: String,
    export: String,
}

/// `nas:/srv/vm`, `[fd00::1]:/srv/vm`처럼 쓴다. 서버 이름의 나머지 검사는 API가 한다.
fn parse_nfs(s: &str) -> Result<NfsSource, String> {
    let split = match s.strip_prefix('[') {
        Some(rest) => rest.split_once("]:"),
        None => s.split_once(':'),
    };
    match split {
        Some((server, export)) if !server.is_empty() && export.starts_with('/') => Ok(NfsSource {
            server: server.into(),
            export: export.into(),
        }),
        _ => Err(format!(
            "invalid nfs source {s:?}: use SERVER:/EXPORT ([ADDR]:/EXPORT for IPv6)"
        )),
    }
}

/// `--lvm-thin`으로 받은 VG와 thin pool 이름
#[derive(Debug, Clone, PartialEq, Eq)]
struct LvmThinSource {
    vg: String,
    thinpool: String,
}

/// `vg0/tp`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_lvm_thin(s: &str) -> Result<LvmThinSource, String> {
    match s.split_once('/') {
        Some((vg, thinpool))
            if !vg.is_empty() && !thinpool.is_empty() && !thinpool.contains('/') =>
        {
            Ok(LvmThinSource {
                vg: vg.into(),
                thinpool: thinpool.into(),
            })
        }
        _ => Err(format!("invalid lvm-thin pool {s:?}: use VG/THINPOOL")),
    }
}

/// `--rbd`로 받은 Ceph 풀과 RBD 네임스페이스
#[derive(Debug, Clone, PartialEq, Eq)]
struct RbdSource {
    pool: String,
    namespace: Option<String>,
}

/// `rbd`, `rbd/opl`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_rbd(s: &str) -> Result<RbdSource, String> {
    let (pool, namespace) = match s.split_once('/') {
        Some((pool, namespace)) => (pool, Some(namespace)),
        None => (s, None),
    };
    let bad = |n: &str| n.is_empty() || n.contains('/');
    if bad(pool) || namespace.is_some_and(bad) {
        return Err(format!(
            "invalid rbd pool {s:?}: use POOL or POOL/NAMESPACE"
        ));
    }
    Ok(RbdSource {
        pool: pool.into(),
        namespace: namespace.map(Into::into),
    })
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크나 VXLAN 네트워크를 만든다.
    Create(NetworkCreate),
    Delete {
        network: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["bridge", "vxlan"])))]
struct NetworkCreate {
    name: String,
    /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
    #[arg(long)]
    bridge: Option<String>,
    /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
    #[arg(long, conflicts_with = "vxlan")]
    vlan: Option<u16>,
    /// VXLAN 네트워크 식별자(VNI, 1-16777215). opld가 모든 노드에 브리지 oplv<VNI>를 만든다.
    #[arg(long, value_name = "VNI")]
    vxlan: Option<u32>,
    /// VXLAN 네트워크의 MTU(1280-9000, 기본 1450)
    #[arg(long, conflicts_with = "bridge")]
    mtu: Option<u16>,
}

/// `network create`의 요청 본문(`POST /networks`). 주지 않은 값은 보내지 않는다.
fn network_create_body(c: &NetworkCreate) -> Value {
    let mut spec = serde_json::Map::new();
    if let Some(bridge) = &c.bridge {
        spec.insert("bridge".into(), json!(bridge));
    }
    if let Some(vlan) = c.vlan {
        spec.insert("vlan".into(), json!(vlan));
    }
    if let Some(vni) = c.vxlan {
        spec.insert("vxlan".into(), json!({ "vni": vni }));
    }
    if let Some(mtu) = c.mtu {
        spec.insert("mtu".into(), json!(mtu));
    }
    json!({ "name": c.name, "spec": spec })
}

#[derive(Subcommand)]
enum SecurityGroupCmd {
    List,
    Get {
        group: String,
    },
    /// 보안 그룹을 만든다. 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다. 맞는 규칙이 없으면
    /// 들어오는 것은 막고 나가는 것은 보낸다.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 규칙 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`
        /// (예: "in accept tcp 22 10.0.0.0/8"). 여러 번 줄 수 있다.
        #[arg(long = "rule", value_name = "RULE", value_parser = parse_rule)]
        rules: Vec<Value>,
    },
    /// 보안 그룹을 지운다. 그룹을 쓰는 VM NIC가 없어야 한다.
    Delete {
        group: String,
    },
    /// 규칙을 더하거나 뺀다. 그룹을 쓰는 노드가 곧바로 적용한다.
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// 규칙 하나를 더한다(RULE은 `security-group create --rule`과 같다).
    Add {
        group: String,
        #[arg(value_parser = parse_rule)]
        rule: Value,
        /// 이 번호(1부터) 자리에 넣는다. 생략하면 맨 끝이다.
        #[arg(long, value_name = "N")]
        at: Option<usize>,
    },
    /// N번 규칙(1부터, `security-group get`의 순서)을 뺀다.
    Rm { group: String, n: usize },
}

/// `in accept tcp 22 10.0.0.0/8`처럼 쓴다: 방향과 동작, 그 뒤로 프로토콜·포트·원격 CIDR을 이 차례로
/// (모두 생략할 수 있다). 값의 나머지 검사(포트 범위, 프로토콜과 포트·주소의 짝)는 API가 한다.
fn parse_rule(s: &str) -> Result<Value, String> {
    let bad = || {
        format!("invalid rule {s:?}: use in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]")
    };
    let mut words = s.split_whitespace().peekable();
    let direction = words
        .next()
        .filter(|w| matches!(*w, "in" | "out"))
        .ok_or_else(bad)?;
    let action = words
        .next()
        .filter(|w| matches!(*w, "accept" | "drop"))
        .ok_or_else(bad)?;
    let mut rule = json!({ "direction": direction, "action": action });
    if let Some(protocol) = words.next_if(|w| matches!(*w, "tcp" | "udp" | "icmp" | "icmpv6")) {
        rule["protocol"] = json!(protocol);
    }
    if let Some(ports) = words.next_if(|w| is_ports(w)) {
        rule["ports"] = json!(ports);
    }
    if let Some(remote) = words.next_if(|w| w.contains(['.', ':'])) {
        rule["remote"] = json!(remote);
    }
    if words.next().is_some() {
        return Err(bad());
    }
    Ok(rule)
}

/// `22`나 `8000-8080` 모양인지
fn is_ports(word: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match word.split_once('-') {
        Some((start, end)) => digits(start) && digits(end),
        None => digits(word),
    }
}

/// `security-group create`의 요청 본문(`POST /security-groups`)
fn security_group_create_body(name: &str, description: Option<&str>, rules: &[Value]) -> Value {
    let mut spec = json!({ "rules": rules });
    if let Some(description) = description {
        spec["description"] = json!(description);
    }
    json!({ "name": name, "spec": spec })
}

/// 그룹의 규칙에 `rule`을 `at`번(1부터, 생략하면 맨 끝) 자리에 넣은 목록
fn rules_with(group: &Value, rule: Value, at: Option<usize>) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let at = at.unwrap_or(rules.len() + 1);
    if at == 0 || at > rules.len() + 1 {
        bail!(
            "cannot add a rule at {at}: security group {} has {} rules (use 1 to {})",
            cell(&group["name"]),
            rules.len(),
            rules.len() + 1
        );
    }
    rules.insert(at - 1, rule);
    Ok(rules)
}

/// 그룹의 규칙에서 `n`번(1부터)을 뺀 목록
fn rules_without(group: &Value, n: usize) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if n == 0 || n > rules.len() {
        bail!(
            "security group {} has no rule {n} ({} rules, numbered from 1)",
            cell(&group["name"]),
            rules.len()
        );
    }
    rules.remove(n - 1);
    Ok(rules)
}

/// 그룹을 읽어 `edit`으로 만든 규칙 목록으로 바꾼다. 읽은 revision을 `If-Match`로 보내므로 그 사이
/// 다른 사람이 바꿨으면 실패한다(규칙 번호가 달라졌을 수 있다).
async fn patch_rules(
    client: &Client,
    group: &str,
    edit: impl FnOnce(&Value) -> anyhow::Result<Vec<Value>>,
) -> anyhow::Result<()> {
    let path = format!("/security-groups/{group}");
    let current = client.get(&path).await?;
    let rules = edit(&current)?;
    let r = client
        .request(Method::PATCH, &path)
        .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
        .json(&json!({ "rules": rules }));
    client.send(r).await?;
    Ok(())
}

/// `security-group list`의 열
const SECURITY_GROUP_HEADERS: &[&str] = &["NAME", "RULES", "DESCRIPTION"];

/// `security-group list`의 한 행
fn security_group_row(g: &Value) -> Vec<String> {
    vec![
        cell(&g["name"]),
        g["spec"]["rules"]
            .as_array()
            .map_or(0, Vec::len)
            .to_string(),
        cell(&g["spec"]["description"]),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
    },
    Delete {
        volume: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    /// VM 설정을 바꾼다. 실행 중이어도 된다.
    Set(VmSet),
    /// NIC의 보안 그룹을 바꾼다. 실행 중이어도 곧바로 적용된다. 그룹을 주지 않으면 모두 뺀다.
    NicSg {
        vm: String,
        /// NIC 번호(0부터, `vm get`의 nics 순서)
        index: usize,
        groups: Vec<String>,
    },
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(디스크가 모두 공유 풀에 있어야 한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
    },
    Delete {
        vm: String,
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다. `,sg=A+B`를 붙이면 그 NIC에
    /// 보안 그룹을 건다(예: `lan,sg=web+db`).
    #[arg(long = "nic", value_name = "NETWORK[,sg=GROUP+...]", value_parser = parse_nic)]
    nics: Vec<NicArg>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. 펜싱이 켜진 노드에서만 돈다.
    #[arg(long)]
    ha: bool,
}

/// `--nic`으로 받은 네트워크와 보안 그룹
#[derive(Debug, Clone, PartialEq, Eq)]
struct NicArg {
    network: String,
    security_groups: Vec<String>,
}

/// `lan`, `lan,sg=web+db`처럼 쓴다. 그룹이 있는지는 API가 본다.
fn parse_nic(s: &str) -> Result<NicArg, String> {
    let bad = || format!("invalid nic {s:?}: use NETWORK or NETWORK,sg=GROUP+GROUP");
    let (network, groups) = match s.split_once(',') {
        Some((network, option)) => {
            let groups = option.strip_prefix("sg=").ok_or_else(bad)?;
            let groups: Vec<String> = groups.split('+').map(str::to_string).collect();
            if groups.iter().any(|g| g.is_empty() || g.contains(',')) {
                return Err(bad());
            }
            (network, groups)
        }
        None => (s, Vec::new()),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NicArg {
        network: network.into(),
        security_groups: groups,
    })
}

/// `vm create`의 NIC 하나. 그룹이 없으면 `security_groups`를 보내지 않는다.
fn nic_body(nic: &NicArg) -> Value {
    let mut body = json!({ "network": nic.network, "model": "virtio" });
    if !nic.security_groups.is_empty() {
        body["security_groups"] = json!(nic.security_groups);
    }
    body
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).args(["ha", "no_ha"])))]
struct VmSet {
    vm: String,
    /// HA VM으로 만든다. VM이 멈춰 있어야 하고 VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
    ha: bool,
    /// HA를 끈다.
    #[arg(long)]
    no_ha: bool,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 볼륨 형식. 주지 않았으면 풀을 읽어 그 종류의 기본값으로 정한다.
async fn volume_format(
    client: &Client,
    pool: &str,
    format: Option<Format>,
) -> anyhow::Result<Format> {
    match format {
        Some(f) => Ok(f),
        None => {
            let read = client.get(&format!("/storage-pools/{pool}")).await;
            let pool = read.with_context(|| {
                format!(
                    "cannot read storage pool {pool:?} to choose a volume format; pass --format"
                )
            })?;
            Ok(pool_default_format(&pool))
        }
    }
}

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw(스냅샷은 블록 스냅샷,
/// 공유 LVM은 스냅샷 없음), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" || kind == "lvm-shared" {
        Format::Raw
    } else {
        Format::Qcow2
    }
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let token = login(&server, args, ca_pem.clone()).await?;
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        if let Some((old, old_server)) = previous
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(token), ca_pem)?;
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        writeln!(out, "logged in to {server} as {}", args.user)?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            if let Some(id) = config.token.as_deref().and_then(token_id) {
                // 이미 지워진 토큰이어도 로컬에서는 지운다.
                let _ = client.delete(&format!("/tokens/{id}")).await;
            }
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                let nodes = if json_out {
                    Vec::new()
                } else {
                    items(&client.get("/nodes").await?)
                };
                print_list(out, json_out, &list, POOL_HEADERS, |p| pool_row(p, &nodes))?;
            }
            PoolCmd::Get { pool } => {
                let pool = client.get(&format!("/storage-pools/{pool}")).await?;
                if json_out {
                    print_json(out, &pool)?;
                } else {
                    let nodes = items(&client.get("/nodes").await?);
                    write!(out, "{}", describe_pool(&pool))?;
                    table(out, POOL_NODE_HEADERS, &pool_node_rows(&pool, &nodes))?;
                }
            }
            PoolCmd::Create(args) => {
                let name = args.name.clone();
                client
                    .post("/storage-pools", &pool_create_body(*args))
                    .await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Set(PoolSet { pool, sharing }) => {
                client
                    .patch(&format!("/storage-pools/{pool}"), &pool_set_body(&sharing))
                    .await?;
                let state = if sharing.shared {
                    "shared"
                } else {
                    "node-local"
                };
                writeln!(out, "storage pool {pool}: {state}")?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, NETWORK_HEADERS, network_row)?;
            }
            NetworkCmd::Get { network } => {
                print_json(out, &client.get(&format!("/networks/{network}")).await?)?
            }
            NetworkCmd::Create(c) => {
                client.post("/networks", &network_create_body(&c)).await?;
                writeln!(out, "network {} created", c.name)?;
            }
            NetworkCmd::Delete { network } => {
                client.delete(&format!("/networks/{network}")).await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::SecurityGroup { cmd } => security_group(&client, cmd, json_out, out).await?,
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => {
                print_json(out, &client.get(&format!("/volumes/{volume}")).await?)?
            }
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{volume}/resize"), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{volume}")).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    task_row,
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{task}")).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{name}"), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{name}")).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{token}")).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
    }
    Ok(())
}

async fn security_group(
    client: &Client,
    cmd: SecurityGroupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SecurityGroupCmd::List => {
            let list = client.get("/security-groups").await?;
            print_list(
                out,
                json_out,
                &list,
                SECURITY_GROUP_HEADERS,
                security_group_row,
            )?;
        }
        SecurityGroupCmd::Get { group } => print_json(
            out,
            &client.get(&format!("/security-groups/{group}")).await?,
        )?,
        SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } => {
            let body = security_group_create_body(&name, description.as_deref(), &rules);
            client.post("/security-groups", &body).await?;
            writeln!(out, "security group {name} created")?;
        }
        SecurityGroupCmd::Delete { group } => {
            client.delete(&format!("/security-groups/{group}")).await?;
            writeln!(out, "security group {group} deleted")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Add { group, rule, at },
        } => {
            patch_rules(client, &group, |g| rules_with(g, rule, at)).await?;
            writeln!(out, "security group {group}: rule added")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Rm { group, n },
        } => {
            patch_rules(client, &group, |g| rules_without(g, n)).await?;
            writeln!(out, "security group {group}: rule {n} removed")?;
        }
    }
    Ok(())
}

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            print_cluster_status(out, &view)?;
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{node}")).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"],
                vm_row,
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{vm}")).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args.nics.iter().map(nic_body).collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                    "ha": args.ha,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Set(args) => {
            put_spec_field(client, &args.vm, "ha", json!(args.ha)).await?;
            let state = if args.ha { "on" } else { "off" };
            writeln!(out, "vm {}: HA {state}", args.vm)?;
        }
        VmCmd::NicSg { vm, index, groups } => {
            put_spec(client, &vm, |current| {
                nic_groups_update(current, index, &groups)
            })
            .await?;
            if groups.is_empty() {
                writeln!(out, "vm {vm}: NIC {index} has no security groups")?;
            } else {
                writeln!(
                    out,
                    "vm {vm}: NIC {index} security groups {}",
                    groups.join(", ")
                )?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Migrate { vm, node } => {
            let task = client
                .post(&format!("/vms/{vm}/migrate"), &json!({ "node": node }))
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{vm}")).await?;
                let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{vm}")).await?;
                    let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{vm}/snapshots/{}", cell(&s["id"]));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client.post(&format!("/vms/{vm}/clone"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{vm}")).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// `task list`의 한 행. HA 복구면 OP에 죽은 노드를 붙인다(`vm-ha-recover (from A)`).
fn task_row(t: &Value) -> Vec<String> {
    let op = &t["spec"]["op"];
    let mut kind = cell(&op["kind"]);
    if op["kind"] == "vm-ha-recover" {
        kind = format!("{kind} (from {})", cell(&op["from"]));
    }
    vec![
        cell(&t["id"]),
        kind,
        cell(&t["spec"]["target"]),
        cell(&t["status"]["state"]),
        cell(&t["status"]["error"]),
    ]
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        // 옛 서버는 보내지 않는다 — HA VM이 아니다.
        (v["spec"]["ha"] == true).to_string(),
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client.post(&format!("/vms/{vm}/snapshots"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current = client.get(&format!("/vms/{vm}")).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{vm}/snapshots/{name}/revert");
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{vm}/snapshots/{name}"))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", args.vm);
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다).
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    put_spec_field(
        client,
        vm,
        "snapshot_schedule",
        schedule.unwrap_or(Value::Null),
    )
    .await
}

/// VM `vm`의 spec 필드 `field`를 `value`로 바꾼다.
async fn put_spec_field(
    client: &Client,
    vm: &str,
    field: &str,
    value: Value,
) -> anyhow::Result<()> {
    put_spec(client, vm, |current| {
        Ok(spec_update(current, field, value.clone()))
    })
    .await
}

/// VM `vm`을 읽어 `body`(읽은 VM → 수정 본문)로 만든 본문을 그 revision으로(`If-Match`) 다시 쓴다.
async fn put_spec(
    client: &Client,
    vm: &str,
    body: impl Fn(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<()> {
    let path = format!("/vms/{vm}");
    let mut retries = SpecRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&body(&current)?);
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// VM 수정 PUT을 다시 할 횟수
#[derive(Default)]
struct SpecRetries {
    mismatch: u32,
    busy: u32,
}

impl SpecRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 spec 필드 `field`만 `value`로 바꾼다.
fn spec_update(vm: &Value, field: &str, value: Value) -> Value {
    let mut spec = vm["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// VM 수정 본문: NIC `index`(0부터)의 보안 그룹만 `groups`로 바꾼다.
fn nic_groups_update(vm: &Value, index: usize, groups: &[String]) -> anyhow::Result<Value> {
    let mut nics = vm["spec"]["nics"].as_array().cloned().unwrap_or_default();
    let count = nics.len();
    let Some(nic) = nics.get_mut(index) else {
        bail!(
            "VM {} has no NIC {index} ({count} NICs, numbered from 0)",
            cell(&vm["name"])
        );
    };
    nic["security_groups"] = json!(groups);
    Ok(spec_update(vm, "nics", Value::Array(nics)))
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(Method::POST, &format!("/vms/{vm}/actions/{action}")))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "UNDERLAY",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
    "FENCING",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
        cell(&n["status"]["underlay"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
        // 펜싱이 꺼진 노드(옛 서버도)는 `ha`가 없다.
        cell(&n["status"]["ha"]["fencing"]),
    ]
}

/// `network list`의 열
const NETWORK_HEADERS: &[&str] = &["NAME", "TYPE", "BRIDGE", "VLAN/VNI", "MTU"];

/// VXLAN 네트워크의 기본 MTU(서버의 `DEFAULT_VXLAN_MTU`)
const DEFAULT_VXLAN_MTU: u64 = 1450;

/// `network list`의 한 행. VXLAN 네트워크의 브리지는 opld가 만드는 `oplv<vni>`다.
fn network_row(n: &Value) -> Vec<String> {
    let spec = &n["spec"];
    match spec["vxlan"]["vni"].as_u64() {
        Some(vni) => vec![
            cell(&n["name"]),
            "vxlan".into(),
            format!("oplv{vni}"),
            vni.to_string(),
            spec["mtu"]
                .as_u64()
                .unwrap_or(DEFAULT_VXLAN_MTU)
                .to_string(),
        ],
        None => vec![
            cell(&n["name"]),
            "bridge".into(),
            cell(&spec["bridge"]),
            cell(&spec["vlan"]),
            "-".into(),
        ],
    }
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
        .iter()
        .map(|m| {
            vec![
                cell(&m["name"]),
                cell(&m["id"]),
                cell(&m["address"]),
                if m["voter"] == true {
                    "voter"
                } else {
                    "learner"
                }
                .into(),
                cell(&m["online"]),
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
                cell(&m["fencing"]),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS", "FENCING",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
    Ok(())
}

/// 목록 응답의 항목들
fn items(list: &Value) -> Vec<Value> {
    list["items"].as_array().cloned().unwrap_or_default()
}

/// `pool list`의 열
const POOL_HEADERS: &[&str] = &["NAME", "KIND", "LOCATION", "NODES", "READY"];

/// `pool list`의 한 행. READY는 이 풀을 쓸 수 있는 노드(없는 노드 포함) 가운데 온라인이고 준비된
/// 노드 수다.
fn pool_row(pool: &Value, nodes: &[Value]) -> Vec<String> {
    let listed = pool["spec"]["nodes"].as_array().map_or(0, Vec::len);
    let usable = pool_nodes(pool, nodes);
    let ready = usable
        .iter()
        .filter(|n| n.is_ok_and(|n| node_online(n) && pool_state(pool, n)["ready"] == true))
        .count();
    vec![
        cell(&pool["name"]),
        cell(&pool["spec"]["backend"]["kind"]),
        pool_location(pool),
        if listed == 0 {
            "all".into()
        } else {
            listed.to_string()
        },
        format!("{ready}/{}", usable.len()),
    ]
}

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`, 공유 LVM 풀은 `vg`(iSCSI면
/// `vg @ portal target`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    if b["kind"] == "lvm-shared" {
        let iscsi = &b["iscsi"];
        if iscsi.is_object() {
            return format!(
                "{} @ {} {}",
                cell(&b["vg"]),
                cell(&iscsi["portal"]),
                cell(&iscsi["target"])
            );
        }
        return cell(&b["vg"]);
    }
    if b["kind"] == "rbd" {
        return match b["namespace"].as_str() {
            Some(namespace) => format!("{}/{namespace}", cell(&b["pool"])),
            None => cell(&b["pool"]),
        };
    }
    if let Some(vg) = b["vg"].as_str() {
        return format!("{vg}/{}", cell(&b["thinpool"]));
    }
    if let Some(dataset) = b["dataset"].as_str() {
        return dataset.into();
    }
    match b["server"].as_str() {
        Some(server) if server.contains(':') => format!("[{server}]:{}", cell(&b["export"])),
        Some(server) => format!("{server}:{}", cell(&b["export"])),
        None => cell(&b["path"]),
    }
}

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
    }
}

fn node_online(node: &Value) -> bool {
    node["status"]["online"] == true
}

/// 노드가 알린 풀 상태. 알리지 않았거나 같은 이름의 다른 풀(`pool_id`가 다름)의 상태면 `null`이다.
fn pool_state<'a>(pool: &Value, node: &'a Value) -> &'a Value {
    let name = pool["name"].as_str().unwrap_or_default();
    let state = &node["status"]["pools"][name];
    if state["pool_id"] == pool["id"] {
        state
    } else {
        &Value::Null
    }
}

/// `pool get`의 머리 줄들
fn describe_pool(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    let mut text = format!(
        "pool: {}\nkind: {}\nlocation: {}\n",
        cell(&pool["name"]),
        cell(&b["kind"]),
        pool_location(pool)
    );
    if let Some(options) = b["options"].as_str() {
        text.push_str(&format!("options: {options}\n"));
    }
    // RBD 풀의 Ceph 사용자·conf(생략하면 Ceph 기본이라 적지 않는다)
    for key in ["user", "conf"] {
        if let Some(value) = b[key].as_str() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!("shared: {}\n", cell(&pool["spec"]["shared"])));
    text
}

/// `pool get`의 노드별 상태 열
const POOL_NODE_HEADERS: &[&str] = &["NODE", "READY", "SIZE", "AVAILABLE", "ERROR"];

/// 풀을 쓸 수 있는 노드마다 상태·용량·오류. 용량은 온라인이고 준비된 노드만 보인다.
fn pool_node_rows(pool: &Value, nodes: &[Value]) -> Vec<Vec<String>> {
    pool_nodes(pool, nodes)
        .into_iter()
        .map(|n| {
            let n = match n {
                Ok(n) => n,
                Err(id) => {
                    return vec![
                        cell(id),
                        "false".into(),
                        "-".into(),
                        "-".into(),
                        "node not found".into(),
                    ];
                }
            };
            let state = pool_state(pool, n);
            let online = node_online(n);
            let ready = online && state["ready"] == true;
            let size = |v: &Value| if ready { bytes(v) } else { "-".into() };
            let error = if !online {
                "node offline".into()
            } else if state.is_null() {
                "no status reported yet".into()
            } else {
                cell(&state["error"])
            };
            vec![
                cell(&n["name"]),
                ready.to_string(),
                size(&state["total_bytes"]),
                size(&state["available_bytes"]),
                error,
            ]
        })
        .collect()
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다.
async fn login(server: &str, args: &LoginArgs, ca_pem: Option<String>) -> anyhow::Result<String> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None, ca_pem)?;
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&json!({ "username": args.user, "password": password }))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let name = format!("oplctl-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", schedule.clone()),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", Value::Null),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
        // `vm set --ha`는 ha만 바꾼다.
        assert_eq!(
            spec_update(&vm, "ha", json!(true))["spec"],
            json!({ "memory_mib": 256, "snapshot_schedule": vm["spec"]["snapshot_schedule"], "ha": true })
        );
    }

    #[test]
    fn parses_vm_ha_commands() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web", "--ha"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if !c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Set(VmSet { vm, ha: true, no_ha: false }) } if vm == "web"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--no-ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Set(VmSet {
                    ha: false,
                    no_ha: true,
                    ..
                })
            }
        ));
        // 바꿀 것이 하나는 있어야 하고, 둘을 함께 줄 수 없다.
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha", "--no-ha"]).is_err());
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node } } if vm == "web" && node == "n2"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            [
                "web",
                "running",
                "migrating",
                "id-a -> id-b",
                "false",
                "42",
                "-"
            ]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "false", "42", "-"]
        );
        // HA VM
        vm["spec"]["ha"] = json!(true);
        assert_eq!(vm_row(&vm)[4], "true");
    }

    #[test]
    fn task_row_shows_where_ha_moved_the_vm_from() {
        let task = json!({
            "id": "t1",
            "spec": { "op": { "kind": "vm-ha-recover", "from": "id-a" }, "target": "vm-1", "node": "id-b" },
            "status": { "state": "succeeded", "error": null },
        });
        assert_eq!(
            task_row(&task),
            ["t1", "vm-ha-recover (from id-a)", "vm-1", "succeeded", "-"]
        );
        // 다른 종류는 이름만
        let mut task = task;
        task["spec"]["op"] = json!({ "kind": "vm-start" });
        assert_eq!(task_row(&task)[1], "vm-start");
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn spec_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = SpecRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = SpecRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
        // 실행 중인 VM의 HA 켜기는 서버의 409 문구를 그대로 보인다.
        assert_eq!(
            r.next("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped (vm_running)"),
            None
        );
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool, pool_set_body(&s.sharing)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }))
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            ("nas".into(), json!({ "shared": false }))
        );
        // 둘 중 하나만, 꼭 하나
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
    }

    #[test]
    fn parses_pool_create_commands() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.path, c.nfs, c.nfs_options, c.shared),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None, false)
        );
        // 디렉터리 풀은 공유 스토리지(밖에서 마운트한 공유 디렉터리)라고 적을 수 있다.
        assert!(create(&["--path", "/mnt/shared", "--shared"]).unwrap().3);
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into()),
                false
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로, NFS 풀은 늘 공유라 --shared가 없다
        for args in [
            &[][..],
            &["--nfs", "nas:/srv/vm", "--shared"],
            &["--shared"],
            &["--path", "/srv/pool", "--nfs", "nas:/srv/vm"],
            &["--path", "/srv/pool", "--nfs-options", "hard"],
            &["--nfs", "nas"],
            &["--nfs", "nas:srv"],
            &["--nfs", ":/srv"],
            &["--nfs", "fd00::1:/srv"],
            &["--nfs", "[fd00::1/srv"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 브리지와 VXLAN 중 정확히 하나. VLAN은 브리지와, MTU는 VXLAN과만. 주지 않은 값은 보내지 않는다.
    #[test]
    fn parses_network_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "network", "create", "lan"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Network {
                    cmd: NetworkCmd::Create(c),
                } => network_create_body(&c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--bridge", "vmbr0"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } })
        );
        assert_eq!(
            create(&["--bridge", "vmbr0", "--vlan", "10"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0", "vlan": 10 } })
        );
        assert_eq!(
            create(&["--vxlan", "100"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 } } })
        );
        assert_eq!(
            create(&["--vxlan", "100", "--mtu", "1400"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 }, "mtu": 1400 } })
        );
        for args in [
            &[][..],
            &["--bridge", "vmbr0", "--vxlan", "100"],
            &["--vxlan", "100", "--vlan", "10"],
            &["--bridge", "vmbr0", "--mtu", "1400"],
            &["--vlan", "10"],
            &["--mtu", "1400"],
            &["--vxlan", "-1"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 종류, 붙는 브리지(VXLAN은 관리 이름), VLAN이나 VNI, VXLAN MTU(없으면 기본값)
    #[test]
    fn network_list_shows_kind_bridge_and_mtu() {
        let rows: Vec<_> = [
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
            json!({ "name": "dmz", "spec": { "bridge": "vmbr0", "vlan": 20 } }),
            json!({ "name": "ov", "spec": { "vxlan": { "vni": 100 } } }),
            json!({ "name": "jumbo", "spec": { "vxlan": { "vni": 7 }, "mtu": 8950 } }),
        ]
        .iter()
        .map(network_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, NETWORK_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   TYPE    BRIDGE   VLAN/VNI  MTU\n\
             lan    bridge  vmbr0    -         -\n\
             dmz    bridge  vmbr0    20        -\n\
             ov     vxlan   oplv100  100       1450\n\
             jumbo  vxlan   oplv7    7         8950\n"
        );
    }

    #[test]
    fn parses_lvm_thin_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.lvm_thin,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-thin", "vg0/tp"]).unwrap(),
            Some(LvmThinSource {
                vg: "vg0".into(),
                thinpool: "tp".into()
            })
        );
        // 다른 종류·--shared와 함께 못 쓰고, VG와 thin pool이 모두 있어야 한다
        for args in [
            &["--lvm-thin", "vg0/tp", "--path", "/srv/pool"][..],
            &["--lvm-thin", "vg0/tp", "--nfs", "nas:/srv/vm"],
            &["--lvm-thin", "vg0/tp", "--shared"],
            &["--lvm-thin", "vg0/tp", "--nfs-options", "hard"],
            &["--lvm-thin", "vg0"],
            &["--lvm-thin", "/tp"],
            &["--lvm-thin", "vg0/"],
            &["--lvm-thin", "vg0/tp/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd, --lvm-shared) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in [
            "--path",
            "--nfs",
            "--lvm-thin",
            "--zfs",
            "--rbd",
            "--lvm-shared",
        ] {
            assert!(msg.contains(kind), "{msg}");
        }
        assert!(create(&["--node", "a"]).is_err());
        assert!(create(&["--path", "/srv/p"]).is_ok());
        assert!(create(&["--nfs", "nas:/srv/vm"]).is_ok());
    }

    #[test]
    fn parses_zfs_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.zfs,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--zfs", "tank/opl"]).unwrap(),
            Some("tank/opl".to_string())
        );
        assert_eq!(
            create(&["--zfs", "tank"]).unwrap(),
            Some("tank".to_string())
        );
        // 다른 종류·--shared와 함께 못 쓴다. 데이터셋 이름의 검사는 API가 한다.
        for args in [
            &["--zfs", "tank/opl", "--path", "/srv/pool"][..],
            &["--zfs", "tank/opl", "--nfs", "nas:/srv/vm"],
            &["--zfs", "tank/opl", "--nfs-options", "hard"],
            &["--zfs", "tank/opl", "--lvm-thin", "vg0/tp"],
            &["--zfs", "tank/opl", "--shared"],
            &["--zfs"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_rbd_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.rbd, c.rbd_user, c.rbd_conf),
                _ => unreachable!(),
            })
        };
        let rbd = |pool: &str, namespace: Option<&str>| RbdSource {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
        };
        assert_eq!(
            create(&["--rbd", "rbd"]).unwrap(),
            (Some(rbd("rbd", None)), None, None)
        );
        assert_eq!(
            create(&[
                "--rbd",
                "rbd/opl",
                "--rbd-user",
                "opl",
                "--rbd-conf",
                "/etc/ceph/opl.conf"
            ])
            .unwrap(),
            (
                Some(rbd("rbd", Some("opl"))),
                Some("opl".into()),
                Some("/etc/ceph/opl.conf".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 사용자·conf는 --rbd와 함께만 쓴다.
        // 이름 규칙의 나머지 검사는 API가 한다.
        for args in [
            &["--rbd", "rbd", "--path", "/srv/pool"][..],
            &["--rbd", "rbd", "--nfs", "nas:/srv/vm"],
            &["--rbd", "rbd", "--nfs-options", "hard"],
            &["--rbd", "rbd", "--lvm-thin", "vg0/tp"],
            &["--rbd", "rbd", "--zfs", "tank/opl"],
            &["--rbd", "rbd", "--shared"],
            &["--path", "/srv/pool", "--rbd-user", "opl"],
            &["--path", "/srv/pool", "--rbd-conf", "/etc/ceph/ceph.conf"],
            &["--rbd"],
            &["--rbd", ""],
            &["--rbd", "/opl"],
            &["--rbd", "rbd/"],
            &["--rbd", "rbd/opl/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// `pool create`가 보내는 요청 본문. RBD 풀은 네임스페이스·사용자·conf를 담고 `shared`는 적지
    /// 않는다.
    #[test]
    fn pool_create_sends_the_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        let args = [
            "--rbd",
            "rbd/opl",
            "--rbd-user",
            "opl",
            "--rbd-conf",
            "/etc/ceph/x.conf",
        ];
        assert_eq!(
            body(&args),
            json!({
                "name": "ceph",
                "spec": {
                    "backend": {
                        "kind": "rbd", "pool": "rbd", "namespace": "opl",
                        "user": "opl", "conf": "/etc/ceph/x.conf"
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--rbd", "rbd", "--node", "n1"])["spec"],
            json!({
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": null, "user": null, "conf": null
                },
                "nodes": ["n1"]
            })
        );
    }

    #[test]
    fn parses_lvm_shared_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.lvm_shared, c.iscsi_portal, c.iscsi_target),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-shared", "san"]).unwrap(),
            (Some("san".into()), None, None)
        );
        assert_eq!(
            create(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "10.0.0.5:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ])
            .unwrap(),
            (
                Some("san".into()),
                Some("10.0.0.5:3260".into()),
                Some("iqn.2003-01.org.example:san".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 포털·타깃은 --lvm-shared와 함께, 둘이 함께
        // 쓴다. 이름 규칙의 검사는 API가 한다.
        for args in [
            &["--lvm-shared", "san", "--path", "/srv/pool"][..],
            &["--lvm-shared", "san", "--nfs", "nas:/srv/vm"],
            &["--lvm-shared", "san", "--nfs-options", "hard"],
            &["--lvm-shared", "san", "--lvm-thin", "vg0/tp"],
            &["--lvm-shared", "san", "--zfs", "tank/opl"],
            &["--lvm-shared", "san", "--rbd", "rbd"],
            &["--lvm-shared", "san", "--rbd-user", "opl"],
            &["--lvm-shared", "san", "--shared"],
            &["--lvm-shared", "san", "--iscsi-portal", "nas"],
            &[
                "--lvm-shared",
                "san",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &[
                "--path",
                "/srv/pool",
                "--iscsi-portal",
                "nas",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &["--lvm-shared"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 공유 LVM 풀은 VG와 iSCSI 타깃(없으면 null)을 담고 `shared`는 적지 않는다.
    #[test]
    fn pool_create_sends_the_lvm_shared_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            body(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "[fd00::5]:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ]),
            json!({
                "name": "san",
                "spec": {
                    "backend": {
                        "kind": "lvm-shared", "vg": "san",
                        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.example:san" }
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--lvm-shared", "san", "--node", "n1"])["spec"],
            json!({
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": ["n1"]
            })
        );
    }

    /// 풀을 읽지 못하면 --format을 주라고 알린다. 주면 풀을 읽지 않는다.
    #[tokio::test]
    async fn volume_format_explains_an_unreadable_pool() {
        // 아무도 듣지 않는 포트
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        let err = volume_format(&client, "fast", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.starts_with(
                "cannot read storage pool \"fast\" to choose a volume format; pass --format: "
            ),
            "{msg}"
        );
        let format = volume_format(&client, "fast", Some(Format::Raw)).await;
        assert_eq!(format.unwrap(), Format::Raw);
    }

    #[test]
    fn volume_format_defaults_to_pool_kind() {
        let pool = |kind: &str| json!({ "spec": { "backend": { "kind": kind } } });
        assert_eq!(pool_default_format(&pool("lvm-thin")), Format::Raw);
        assert_eq!(pool_default_format(&pool("zfs")), Format::Raw);
        assert_eq!(pool_default_format(&pool("rbd")), Format::Raw);
        assert_eq!(pool_default_format(&pool("lvm-shared")), Format::Raw);
        assert_eq!(pool_default_format(&pool("dir")), Format::Qcow2);
        assert_eq!(pool_default_format(&pool("nfs")), Format::Qcow2);
        // --format를 주지 않으면 풀을 읽어 정하고, 주면 그대로 쓴다.
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "create", "root", "--pool", "fast", "--size", "1G",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Create { format: None, .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "import", "root", "--pool", "fast", "--file", "a.vmdk", "--format",
            "raw",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Import {
                    format: Some(Format::Raw),
                    ..
                }
            }
        ));
    }

    #[test]
    fn pool_list_and_details_show_each_node() {
        let node = |name: &str, online: bool, pools: Value| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "status": { "online": online, "pools": pools },
            })
        };
        let gib = 1u64 << 30;
        let state = |ready: bool, error: Value, total: u64, available: u64| json!({ "pool_id": "id-nas", "ready": ready, "error": error, "total_bytes": total, "available_bytes": available });
        let nodes = [
            node(
                "a",
                true,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            node(
                "b",
                true,
                json!({ "nas": state(false, json!("mount failed"), 0, 0) }),
            ),
            node("c", true, json!({})),
            // 준비됐다고 알렸지만 꺼진 노드는 준비로 세지 않는다.
            node(
                "d",
                false,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
            node(
                "e",
                true,
                json!({ "nas": { "pool_id": "id-old", "ready": true, "error": null } }),
            ),
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
        ];
        let nas = json!({
            "id": "id-nas",
            "name": "nas",
            "spec": {
                "backend": { "kind": "nfs", "server": "fd00::1", "export": "/srv/vm", "options": "hard" },
                "nodes": [],
                "shared": true,
            },
        });
        // 노드 목록의 노드를 그 순서대로 본다. 없는 노드도 센다.
        let local = json!({
            "id": "id-local",
            "name": "local",
            "spec": {
                "backend": { "kind": "dir", "path": "/srv/pool" },
                "nodes": ["id-gone", "id-b"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&nas, &nodes),
            ["nas", "nfs", "[fd00::1]:/srv/vm", "all", "1/5"]
        );
        assert_eq!(
            pool_row(&local, &nodes),
            ["local", "dir", "/srv/pool", "2", "0/2"]
        );

        let mut out = describe_pool(&nas).into_bytes();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&nas, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "pool: nas\n\
             kind: nfs\n\
             location: [fd00::1]:/srv/vm\n\
             options: hard\n\
             shared: true\n\
             NODE  READY  SIZE      AVAILABLE  ERROR\n\
             a     true   100.0GiB  40.0GiB    -\n\
             b     false  -         -          mount failed\n\
             c     false  -         -          no status reported yet\n\
             d     false  -         -          node offline\n\
             e     false  -         -          no status reported yet\n"
        );
        assert_eq!(
            describe_pool(&local),
            "pool: local\nkind: dir\nlocation: /srv/pool\nshared: false\n"
        );
        let fast = json!({
            "id": "id-fast",
            "name": "fast",
            "spec": {
                "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&fast, &nodes),
            ["fast", "lvm-thin", "vg0/tp", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&fast),
            "pool: fast\nkind: lvm-thin\nlocation: vg0/tp\nshared: false\n"
        );
        let tank = json!({
            "id": "id-tank",
            "name": "tank",
            "spec": {
                "backend": { "kind": "zfs", "dataset": "tank/opl" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&tank, &nodes),
            ["tank", "zfs", "tank/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&tank),
            "pool: tank\nkind: zfs\nlocation: tank/opl\nshared: false\n"
        );
        // RBD 풀의 위치는 `POOL[/NAMESPACE]`, 상세는 사용자·conf가 있으면 보인다.
        let ceph = json!({
            "id": "id-ceph",
            "name": "ceph",
            "spec": {
                "backend": { "kind": "rbd", "pool": "rbd" },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&ceph, &nodes),
            ["ceph", "rbd", "rbd", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&ceph),
            "pool: ceph\nkind: rbd\nlocation: rbd\nshared: true\n"
        );
        let opl = json!({
            "id": "id-opl",
            "name": "opl",
            "spec": {
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                    "conf": "/etc/ceph/opl.conf"
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&opl, &nodes),
            ["opl", "rbd", "rbd/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&opl),
            "pool: opl\nkind: rbd\nlocation: rbd/opl\nuser: opl\nconf: /etc/ceph/opl.conf\n\
             shared: true\n"
        );
        // 공유 LVM 풀의 위치는 `VG`, iSCSI면 `VG @ PORTAL TARGET`
        let fc = json!({
            "id": "id-fc",
            "name": "fc",
            "spec": {
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&fc, &nodes),
            ["fc", "lvm-shared", "san", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&fc),
            "pool: fc\nkind: lvm-shared\nlocation: san\nshared: true\n"
        );
        let san = json!({
            "id": "id-san",
            "name": "san",
            "spec": {
                "backend": {
                    "kind": "lvm-shared", "vg": "san",
                    "iscsi": { "portal": "10.0.0.5:3260", "target": "iqn.2003-01.org.example:san" }
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&san, &nodes),
            [
                "san",
                "lvm-shared",
                "san @ 10.0.0.5:3260 iqn.2003-01.org.example:san",
                "1",
                "0/1"
            ]
        );
        assert_eq!(
            describe_pool(&san),
            "pool: san\nkind: lvm-shared\nlocation: san @ 10.0.0.5:3260 \
             iqn.2003-01.org.example:san\nshared: true\n"
        );
        let mut out = Vec::new();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&local, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE     READY  SIZE  AVAILABLE  ERROR\n\
             id-gone  false  -     -          node not found\n\
             b        false  -     -          no status reported yet\n"
        );
    }

    #[test]
    fn node_list_shows_witnesses_and_fencing() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let mut a = node("a", Some(false));
        a["status"]["ha"] = json!({ "fencing": "watchdog", "recover_after_secs": 65 });
        a["status"]["underlay"] = json!("10.0.0.1");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
            .iter()
            .map(node_row)
            .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다. 펜싱이 꺼진 노드는 `ha`가 없다.
        // underlay가 없는 노드(위트니스, 옛 서버)는 `-`다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -         true    4     1.0GiB  false        false    -\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다. 펜싱이 꺼졌으면(null) 또는 옛
        // 서버라 `fencing`이 없으면 `-`다.
        let mut a = member("a", true, false);
        a["fencing"] = json!("process");
        let mut b = member("b", true, false);
        b["fencing"] = Value::Null;
        assert_eq!(
            status(vec![a, b, member("w", true, true)]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS  FENCING\n\
             a     id-a  a:8006   voter  true    true    false    process\n\
             b     id-b  b:8006   voter  true    false   false    -\n\
             w     id-w  w:8006   voter  true    false   true     -\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 방향과 동작은 꼭, 프로토콜·포트·CIDR은 이 차례로 생략할 수 있다.
    #[test]
    fn parses_rules() {
        assert_eq!(
            parse_rule("in accept tcp 22 10.0.0.0/8"),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "tcp",
                "ports": "22", "remote": "10.0.0.0/8"
            }))
        );
        assert_eq!(
            parse_rule("out drop tcp 25"),
            Ok(json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }))
        );
        assert_eq!(
            parse_rule("in accept icmp"),
            Ok(json!({ "direction": "in", "action": "accept", "protocol": "icmp" }))
        );
        assert_eq!(
            parse_rule("  in   accept udp 8000-8080 "),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "udp", "ports": "8000-8080"
            }))
        );
        assert_eq!(
            parse_rule("out accept 2001:db8::/32"),
            Ok(json!({ "direction": "out", "action": "accept", "remote": "2001:db8::/32" }))
        );
        assert_eq!(
            parse_rule("in drop"),
            Ok(json!({ "direction": "in", "action": "drop" }))
        );
        // 포트 범위와 프로토콜·포트의 짝은 서버가 본다.
        assert_eq!(
            parse_rule("in accept 99999"),
            Ok(json!({ "direction": "in", "action": "accept", "ports": "99999" }))
        );
        for bad in [
            "",
            "in",
            "accept in",
            "up accept",
            "in allow",
            "in accept sctp",
            "in accept tcp 22 10.0.0.0/8 extra",
            "in accept 10.0.0.0/8 tcp",
            "in accept tcp 10.0.0.0/8 22",
            "in accept tcp ssh",
            "in accept tcp 22-",
            "in accept tcp -22",
            "IN accept",
        ] {
            let err = parse_rule(bad).unwrap_err();
            assert!(err.contains("in|out accept|drop"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parses_security_group_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "security-group"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::SecurityGroup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&[
            "create",
            "web",
            "--description",
            "web servers",
            "--rule",
            "in accept tcp 443",
            "--rule",
            "in accept icmp",
        ])
        .unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [
                    { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" },
                    { "direction": "in", "action": "accept", "protocol": "icmp" },
                ]
            }})
        );
        // 설명이 없으면 보내지 않는다. 규칙 없는 그룹도 된다.
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&["create", "empty"]).unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "empty", "spec": { "rules": [] } })
        );
        assert!(parse(&["create", "web", "--rule", "in maybe"]).is_err());
        assert!(matches!(
            parse(&["rule", "add", "web", "out drop tcp 25", "--at", "2"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Add { group, rule, at: Some(2) } }
                if group == "web" && rule["ports"] == "25"
        ));
        assert!(matches!(
            parse(&["rule", "rm", "web", "3"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Rm { group, n: 3 } } if group == "web"
        ));
        assert!(parse(&["rule", "add", "web", "sideways accept"]).is_err());
        assert!(parse(&["rule", "rm", "web", "first"]).is_err());
        assert!(matches!(
            parse(&["get", "web"]).unwrap(),
            SecurityGroupCmd::Get { group } if group == "web"
        ));
        assert!(matches!(
            parse(&["delete", "web"]).unwrap(),
            SecurityGroupCmd::Delete { group } if group == "web"
        ));
    }

    /// 규칙 번호는 1부터다. 범위 밖이면 아무것도 바꾸지 않고 실패한다.
    #[test]
    fn adds_and_removes_rules_by_number() {
        let rule =
            |n: u16| json!({ "direction": "in", "action": "accept", "ports": n.to_string() });
        let group = json!({ "name": "web", "spec": { "rules": [rule(1), rule(2)] } });
        assert_eq!(
            rules_with(&group, rule(9), None).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(1)).unwrap(),
            vec![rule(9), rule(1), rule(2)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(3)).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        for at in [0, 4] {
            let err = rules_with(&group, rule(9), Some(at))
                .unwrap_err()
                .to_string();
            assert!(err.contains("use 1 to 3"), "{err}");
        }
        assert_eq!(rules_without(&group, 1).unwrap(), vec![rule(2)]);
        assert_eq!(rules_without(&group, 2).unwrap(), vec![rule(1)]);
        for n in [0, 3] {
            let err = rules_without(&group, n).unwrap_err().to_string();
            assert!(err.contains(&format!("has no rule {n}")), "{err}");
        }
        // 규칙이 없는 그룹은 응답에 rules가 비어 있다.
        let empty = json!({ "name": "e", "spec": { "rules": [] } });
        assert_eq!(rules_with(&empty, rule(1), None).unwrap(), vec![rule(1)]);
        assert!(rules_without(&empty, 1).is_err());
    }

    #[test]
    fn security_group_list_shows_rule_counts() {
        let rows: Vec<_> = [
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [{ "direction": "in", "action": "accept" }, { "direction": "out", "action": "drop" }]
            }}),
            json!({ "name": "empty", "spec": { "rules": [] } }),
        ]
        .iter()
        .map(security_group_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, SECURITY_GROUP_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   RULES  DESCRIPTION\n\
             web    2      web servers\n\
             empty  0      -\n"
        );
    }

    /// `--nic NETWORK[,sg=A+B]`. 그룹이 없으면 `security_groups`를 보내지 않는다.
    #[test]
    fn parses_nics_with_security_groups() {
        let nics = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "create", "web"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Vm {
                    cmd: VmCmd::Create(c),
                } => c.nics.iter().map(nic_body).collect::<Vec<_>>(),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            nics(&[
                "--nic",
                "user",
                "--nic",
                "lan,sg=web+db",
                "--nic",
                "dmz,sg=web"
            ])
            .unwrap(),
            vec![
                json!({ "network": "user", "model": "virtio" }),
                json!({ "network": "lan", "model": "virtio", "security_groups": ["web", "db"] }),
                json!({ "network": "dmz", "model": "virtio", "security_groups": ["web"] }),
            ]
        );
        for bad in [
            "",
            ",sg=web",
            "lan,",
            "lan,sg=",
            "lan,sg=web+",
            "lan,sg=+web",
            "lan,group=web",
            "lan,sg=web,sg=db",
        ] {
            assert!(nics(&["--nic", bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_vm_nic_sg() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "1", "a", "b"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { vm, index: 1, groups } }
                if vm == "web" && groups == ["a", "b"]
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "0"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { index: 0, groups, .. } } if groups.is_empty()
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "-1"]).is_err());
    }

    /// 그 NIC의 그룹만 바꾸고 나머지(다른 NIC, 다른 필드, 라벨)는 그대로 둔다.
    #[test]
    fn nic_groups_update_changes_one_nic() {
        let vm = json!({
            "name": "web",
            "revision": 4,
            "labels": { "team": "a" },
            "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["old"] },
                ],
            },
        });
        let groups = ["web".to_string(), "db".to_string()];
        assert_eq!(
            nic_groups_update(&vm, 1, &groups).unwrap(),
            json!({ "labels": { "team": "a" }, "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["web", "db"] },
                ],
            }})
        );
        assert_eq!(
            nic_groups_update(&vm, 1, &[]).unwrap()["spec"]["nics"][1]["security_groups"],
            json!([])
        );
        let err = nic_groups_update(&vm, 2, &groups).unwrap_err().to_string();
        assert_eq!(err, "VM web has no NIC 2 (2 NICs, numbered from 0)");
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

`web/src/App.test.tsx`에서

```tsx
    }),
    ...["/vms", "/volumes", "/storage-pools", "/nodes", "/tasks", "/networks", "/snapshots"].map(
      (path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] })),
    ),
    ws.link(`ws://${window.location.host}/api/v1/watch`).addEventListener("connection", () => {}),
```

을 다음으로 바꾼다:

```tsx
    }),
    ...[
      "/vms",
      "/volumes",
      "/storage-pools",
      "/nodes",
      "/tasks",
      "/networks",
      "/snapshots",
      "/security-groups",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
    ws.link(`ws://${window.location.host}/api/v1/watch`).addEventListener("connection", () => {}),
```

`web/src/App.tsx`에서

```tsx
import { NotFoundPage } from "@/pages/NotFoundPage";
import { PoolsPage } from "@/storage/PoolsPage";
```

을 다음으로 바꾼다:

```tsx
import { NotFoundPage } from "@/pages/NotFoundPage";
import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
import { SecurityGroupsPage } from "@/security-groups/SecurityGroupsPage";
import { PoolsPage } from "@/storage/PoolsPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "networks", element: <NetworksPage /> },
            { path: "nodes", element: <NodesPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "networks", element: <NetworksPage /> },
            { path: "security-groups", element: <SecurityGroupsPage /> },
            { path: "security-groups/:name", element: <SecurityGroupPage /> },
            { path: "nodes", element: <NodesPage /> },
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
    http.get(apiUrl("/snapshots"), empty),
  ];
```

을 다음으로 바꾼다:

```tsx
    http.get(apiUrl("/snapshots"), empty),
    http.get(apiUrl("/security-groups"), empty),
  ];
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
  expect(await screen.findByText("live: web=running")).toBeInTheDocument();
  expect(urls[0]).toContain("kinds=vm,volume,storage-pool,node,task,network,snapshot&since=7");
});
```

을 다음으로 바꾼다:

```tsx
  expect(await screen.findByText("live: web=running")).toBeInTheDocument();
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group&since=7",
  );
});
```

`web/src/api/live.ts`에서

```ts
  "snapshot",
];
```

을 다음으로 바꾼다:

```ts
  "snapshot",
  "security-group",
];
```

`web/src/api/live.ts`에서

```ts
        return call(api.GET("/snapshots"));
    }
```

을 다음으로 바꾼다:

```ts
        return call(api.GET("/snapshots"));
      case "security-group":
        return call(api.GET("/security-groups"));
    }
```

`web/src/api/types.ts`에서

```ts
export type Snapshot = Schemas["Resource_SnapshotSpec_SnapshotStatus"];
export type Principal = Schemas["Principal"];
```

을 다음으로 바꾼다:

```ts
export type Snapshot = Schemas["Resource_SnapshotSpec_SnapshotStatus"];
export type SecurityGroup = Schemas["Resource_SecurityGroupSpec_SecurityGroupStatus"];
export type Rule = Schemas["Rule"];
export type Principal = Schemas["Principal"];
```

`web/src/api/types.ts`에서

```ts
/** watch로 받는 리소스 종류. 이름은 API의 `kinds`와 같다. */
export type WatchKind = "vm" | "volume" | "storage-pool" | "node" | "task" | "network" | "snapshot";
```

을 다음으로 바꾼다:

```ts
/** watch로 받는 리소스 종류. 이름은 API의 `kinds`와 같다. */
export type WatchKind =
  "vm" | "volume" | "storage-pool" | "node" | "task" | "network" | "snapshot" | "security-group";
```

`web/src/api/types.ts`에서

```ts
  snapshot: Snapshot;
}
```

을 다음으로 바꾼다:

```ts
  snapshot: Snapshot;
  "security-group": SecurityGroup;
}
```

`web/src/auth/permissions.test.ts`에서

```ts
  expect(can("viewer", "read", "snapshot")).toBe(true);
});
```

을 다음으로 바꾼다:

```ts
  expect(can("viewer", "read", "snapshot")).toBe(true);
  // 보안 그룹은 누구나 읽고 admin만 바꾼다.
  expect(can("viewer", "read", "security-group")).toBe(true);
  expect(can("operator", "write", "security-group")).toBe(false);
  expect(can("admin", "write", "security-group")).toBe(true);
});
```

`web/src/i18n/en.json`에서

```json
    "internal": "The server failed: {{message}}",
    "ha_unsupported": "The node has fencing off and cannot run HA VMs."
  },
```

을 다음으로 바꾼다:

```json
    "internal": "The server failed: {{message}}",
    "ha_unsupported": "The node has fencing off and cannot run HA VMs.",
    "firewall_unsupported": "The node has no firewall (nftables) and cannot run VMs with security groups.",
    "security_group_in_use": "A VM interface uses this security group. Remove it from the VM first."
  },
```

`web/src/i18n/en.json`에서

```json
    "networks": "Networks",
    "nodes": "Nodes",
```

을 다음으로 바꾼다:

```json
    "networks": "Networks",
    "securityGroups": "Security groups",
    "nodes": "Nodes",
```

`web/src/i18n/en.json`에서

```json
  },
  "nodes": {
```

을 다음으로 바꾼다:

```json
  },
  "securityGroups": {
    "note": "Firewall rules for VM interfaces. Nodes with nftables apply them right away, also to running VMs.",
    "rules": "Rules",
    "description": "Description",
    "descriptionOptional": "Description (optional)",
    "vms": "VMs",
    "empty": "No security groups.",
    "inUse": "Remove it from the VM interfaces first.",
    "createTitle": "Create a security group",
    "createDescription": "Creates an empty group. Add its rules on the next page.",
    "created": "Created security group {{name}}.",
    "deleteTitle": "Delete security group {{name}}?",
    "deleteDescription": "No VM interface uses it, so no traffic changes.",
    "deleted": "Deleted security group {{name}}.",
    "notFound": "Security group {{name}} not found",
    "saved": "Saved security group {{name}}.",
    "order": "Rules are checked from the top, and the first match decides. An interface with groups drops incoming traffic that no rule accepts and sends outgoing traffic that no rule drops. With several groups, the first group's rules come first.",
    "always": "Always allowed: replies to allowed traffic, ARP, IPv6 neighbor discovery and DHCP replies. A VM can only send from its own MAC address.",
    "direction": "Direction",
    "action": "Action",
    "protocol": "Protocol",
    "ports": "Ports",
    "remote": "Remote CIDR",
    "directionN": "Direction of rule {{n}}",
    "actionN": "Action of rule {{n}}",
    "protocolN": "Protocol of rule {{n}}",
    "portsN": "Ports of rule {{n}}",
    "remoteN": "Remote CIDR of rule {{n}}",
    "in": "In (to the VM)",
    "out": "Out (from the VM)",
    "accept": "Accept",
    "drop": "Drop",
    "any": "Any",
    "anyPort": "Any port",
    "anywhere": "Anywhere",
    "moveUp": "Move rule {{n}} up",
    "moveDown": "Move rule {{n}} down",
    "removeRule": "Remove rule {{n}}",
    "addRule": "Add a rule",
    "noRules": "No rules: incoming traffic is dropped and outgoing traffic is sent.",
    "errors": {
      "description": "Use at most 200 characters.",
      "ports": "Use a port like 22 or a range like 8000-8080 (1–65535).",
      "remote": "Use a CIDR like 10.0.0.0/8 or 2001:db8::/32.",
      "icmpRemote": "ICMP rules take only IPv4 addresses.",
      "icmpv6Remote": "ICMPv6 rules take only IPv6 addresses."
    }
  },
  "nodes": {
```

`web/src/i18n/en.json`에서

```json
    "editTitle": "Edit {{name}}",
    "runningNote": "The VM is running: its node and disks cannot change. Other changes apply from the next start.",
    "general": "General",
```

을 다음으로 바꾼다:

```json
    "editTitle": "Edit {{name}}",
    "runningNote": "The VM is running: its node, disks, and the number, order and MAC addresses of its interfaces cannot change, and if an interface has security groups, only the groups of the interfaces can change. Security groups apply right away; other changes apply from the next start.",
    "general": "General",
```

`web/src/i18n/en.json`에서

```json
    "addNic": "Add an interface",
    "startAfter": "Start after creating",
```

을 다음으로 바꾼다:

```json
    "addNic": "Add an interface",
    "nicsRunning": "Interfaces cannot be added or removed, and their MAC addresses cannot change, while the VM is running.",
    "securityGroups": "Security groups",
    "nicGroups": "Security groups of interface {{n}}",
    "noSecurityGroups": "No security groups. Create them on the Security groups page.",
    "startAfter": "Start after creating",
```

`web/src/i18n/en.json`에서

```json
      "macDuplicate": "Each interface needs a different MAC address.",
      "keep": "From 1 to 100."
    },
```

을 다음으로 바꾼다:

```json
      "macDuplicate": "Each interface needs a different MAC address.",
      "keep": "From 1 to 100.",
      "securityGroups": "Choose at most 8 security groups."
    },
```

`web/src/i18n/i18n.test.ts`에서

```ts
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

을 다음으로 바꾼다:

```ts
  );
  expect(
    errorMessage(t, new ApiError(409, "firewall_unsupported", 'node "n1" has no firewall')),
  ).toBe("The node has no firewall (nftables) and cannot run VMs with security groups.");
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

`web/src/i18n/ko.json`에서

```json
    "internal": "서버 오류: {{message}}",
    "ha_unsupported": "노드의 펜싱이 꺼져 있어 HA VM을 돌릴 수 없습니다."
  },
```

을 다음으로 바꾼다:

```json
    "internal": "서버 오류: {{message}}",
    "ha_unsupported": "노드의 펜싱이 꺼져 있어 HA VM을 돌릴 수 없습니다.",
    "firewall_unsupported": "노드에 방화벽(nftables)이 없어 보안 그룹을 건 VM을 돌릴 수 없습니다.",
    "security_group_in_use": "이 보안 그룹을 쓰는 VM 인터페이스가 있습니다. 먼저 VM에서 빼세요."
  },
```

`web/src/i18n/ko.json`에서

```json
    "networks": "네트워크",
    "nodes": "노드",
```

을 다음으로 바꾼다:

```json
    "networks": "네트워크",
    "securityGroups": "보안 그룹",
    "nodes": "노드",
```

`web/src/i18n/ko.json`에서

```json
  },
  "nodes": {
```

을 다음으로 바꾼다:

```json
  },
  "securityGroups": {
    "note": "VM 인터페이스에 거는 방화벽 규칙입니다. nftables가 있는 노드가 실행 중인 VM에도 곧바로 적용합니다.",
    "rules": "규칙",
    "description": "설명",
    "descriptionOptional": "설명(선택)",
    "vms": "VM",
    "empty": "보안 그룹이 없습니다.",
    "inUse": "먼저 VM 인터페이스에서 빼세요.",
    "createTitle": "보안 그룹 만들기",
    "createDescription": "빈 그룹을 만듭니다. 규칙은 다음 화면에서 적습니다.",
    "created": "보안 그룹 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "보안 그룹 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 그룹을 쓰는 VM 인터페이스가 없으므로 트래픽은 바뀌지 않습니다.",
    "deleted": "보안 그룹 {{name}}을(를) 삭제했습니다.",
    "notFound": "보안 그룹 {{name}}을(를) 찾을 수 없습니다",
    "saved": "보안 그룹 {{name}}을(를) 저장했습니다.",
    "order": "규칙은 위에서부터 보고 처음 맞는 것을 따릅니다. 그룹을 건 인터페이스는 맞는 규칙이 없으면 들어오는 것은 막고 나가는 것은 보냅니다. 그룹이 여럿이면 앞 그룹의 규칙을 먼저 봅니다.",
    "always": "늘 허용: 허용한 연결의 응답, ARP, IPv6 이웃 탐색, DHCP 응답. VM은 자기 MAC 주소로만 보낼 수 있습니다.",
    "direction": "방향",
    "action": "동작",
    "protocol": "프로토콜",
    "ports": "포트",
    "remote": "원격 CIDR",
    "directionN": "규칙 {{n}}의 방향",
    "actionN": "규칙 {{n}}의 동작",
    "protocolN": "규칙 {{n}}의 프로토콜",
    "portsN": "규칙 {{n}}의 포트",
    "remoteN": "규칙 {{n}}의 원격 CIDR",
    "in": "들어옴(VM으로)",
    "out": "나감(VM에서)",
    "accept": "허용",
    "drop": "차단",
    "any": "모두",
    "anyPort": "모든 포트",
    "anywhere": "어디든",
    "moveUp": "규칙 {{n}}을(를) 위로",
    "moveDown": "규칙 {{n}}을(를) 아래로",
    "removeRule": "규칙 {{n}} 빼기",
    "addRule": "규칙 더하기",
    "noRules": "규칙이 없습니다: 들어오는 것은 막고 나가는 것은 보냅니다.",
    "errors": {
      "description": "200자 이하로 입력하세요.",
      "ports": "22 같은 포트나 8000-8080 같은 범위로 입력하세요(1–65535).",
      "remote": "10.0.0.0/8이나 2001:db8::/32 같은 CIDR로 입력하세요.",
      "icmpRemote": "ICMP 규칙에는 IPv4 주소만 씁니다.",
      "icmpv6Remote": "ICMPv6 규칙에는 IPv6 주소만 씁니다."
    }
  },
  "nodes": {
```

`web/src/i18n/ko.json`에서

```json
    "editTitle": "{{name}} 편집",
    "runningNote": "VM이 실행 중이라 노드와 디스크는 바꿀 수 없습니다. 다른 변경은 다음 시작부터 적용됩니다.",
    "general": "일반",
```

을 다음으로 바꾼다:

```json
    "editTitle": "{{name}} 편집",
    "runningNote": "VM이 실행 중이라 노드·디스크와 인터페이스의 개수·순서·MAC은 바꿀 수 없고, 보안 그룹이 걸린 인터페이스가 있으면 인터페이스는 그룹만 바꿀 수 있습니다. 보안 그룹은 곧바로, 다른 변경은 다음 시작부터 적용됩니다.",
    "general": "일반",
```

`web/src/i18n/ko.json`에서

```json
    "addNic": "인터페이스 추가",
    "startAfter": "만든 뒤 시작",
```

을 다음으로 바꾼다:

```json
    "addNic": "인터페이스 추가",
    "nicsRunning": "VM이 실행 중이라 인터페이스를 더하거나 빼거나 MAC을 바꿀 수 없습니다.",
    "securityGroups": "보안 그룹",
    "nicGroups": "인터페이스 {{n}}의 보안 그룹",
    "noSecurityGroups": "보안 그룹이 없습니다. 보안 그룹 화면에서 만드세요.",
    "startAfter": "만든 뒤 시작",
```

`web/src/i18n/ko.json`에서

```json
      "macDuplicate": "인터페이스마다 다른 MAC 주소여야 합니다.",
      "keep": "1에서 100 사이여야 합니다."
    },
```

을 다음으로 바꾼다:

```json
      "macDuplicate": "인터페이스마다 다른 MAC 주소여야 합니다.",
      "keep": "1에서 100 사이여야 합니다.",
      "securityGroups": "보안 그룹은 8개까지 고를 수 있습니다."
    },
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 보안 그룹은 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
    "href",
    "/security-groups",
  );
});
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  Server,
  Sun,
```

을 다음으로 바꾼다:

```tsx
  Server,
  Shield,
  Sun,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
  { to: "/nodes", key: "nav.nodes", icon: Cpu, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
  { to: "/security-groups", key: "nav.securityGroups", icon: Shield, end: false },
  { to: "/nodes", key: "nav.nodes", icon: Cpu, end: false },
```

`web/src/security-groups/SecurityGroupPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
import { securityGroup } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const ssh = securityGroup(
  "ssh",
  [
    { direction: "in", action: "accept", protocol: "tcp", ports: "22", remote: "10.0.0.0/8" },
    { direction: "out", action: "drop" },
  ],
  "SSH",
);

function open(as = ADMIN) {
  return renderPage(<SecurityGroupPage />, {
    path: "/security-groups/:name",
    url: "/security-groups/ssh",
    as,
  });
}

it("shows the rules in order", async () => {
  mockLists({ "security-group": [ssh] });
  open();
  expect(await screen.findByRole("heading", { name: "ssh" })).toBeInTheDocument();
  expect(screen.getByLabelText("Description")).toHaveValue("SSH");
  expect(screen.getByLabelText("Direction of rule 1")).toHaveValue("in");
  expect(screen.getByLabelText("Protocol of rule 1")).toHaveValue("tcp");
  expect(screen.getByLabelText("Ports of rule 1")).toHaveValue("22");
  expect(screen.getByLabelText("Remote CIDR of rule 1")).toHaveValue("10.0.0.0/8");
  expect(screen.getByLabelText("Direction of rule 2")).toHaveValue("out");
  expect(screen.getByLabelText("Action of rule 2")).toHaveValue("drop");
  // 프로토콜이 없으면 모두이고 포트는 쓸 수 없다.
  expect(screen.getByLabelText("Protocol of rule 2")).toHaveValue("any");
  expect(screen.getByLabelText("Ports of rule 2")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Move rule 1 up" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Move rule 2 down" })).toBeDisabled();
});

it("edits, reorders and saves the rules with the revision it read", async () => {
  mockLists({ "security-group": [ssh] });
  let ifMatch: string | null = null;
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/security-groups/${ssh.id}`), async ({ request }) => {
      ifMatch = request.headers.get("if-match");
      body = await request.json();
      return HttpResponse.json({ ...ssh, revision: 2 });
    }),
  );
  open();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Add a rule" }));
  // 새 규칙은 들어오는 tcp 허용이다.
  expect(screen.getByLabelText("Protocol of rule 3")).toHaveValue("tcp");
  await user.selectOptions(screen.getByLabelText("Protocol of rule 3"), "icmp");
  await user.click(screen.getByRole("button", { name: "Move rule 3 up" }));
  await user.click(screen.getByRole("button", { name: "Move rule 2 up" }));
  // tcp에서 udp로 바꾸면 포트는 그대로다.
  await user.selectOptions(screen.getByLabelText("Protocol of rule 2"), "udp");
  expect(screen.getByLabelText("Ports of rule 2")).toHaveValue("22");
  await user.clear(screen.getByLabelText("Ports of rule 2"));
  await user.type(screen.getByLabelText("Ports of rule 2"), "53");
  await user.click(screen.getByRole("button", { name: "Remove rule 3" }));
  await user.clear(screen.getByLabelText("Description"));
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("Saved security group ssh.")).toBeInTheDocument();
  expect(ifMatch).toBe("1");
  expect(body).toEqual({
    description: "",
    rules: [
      { direction: "in", action: "accept", protocol: "icmp" },
      { direction: "in", action: "accept", protocol: "udp", ports: "53", remote: "10.0.0.0/8" },
    ],
  });
});

it("clears the ports when the protocol cannot have them", async () => {
  mockLists({ "security-group": [ssh] });
  open();
  const user = userEvent.setup();
  await user.selectOptions(await screen.findByLabelText("Protocol of rule 1"), "icmp");
  expect(screen.getByLabelText("Ports of rule 1")).toHaveValue("");
  expect(screen.getByLabelText("Ports of rule 1")).toBeDisabled();
});

it("ties input errors to their rules without calling the API", async () => {
  mockLists({ "security-group": [ssh] });
  let called = false;
  server.use(
    http.patch(apiUrl(`/security-groups/${ssh.id}`), () => {
      called = true;
      return HttpResponse.json(ssh);
    }),
  );
  open();
  const user = userEvent.setup();
  await user.clear(await screen.findByLabelText("Ports of rule 1"));
  await user.type(screen.getByLabelText("Ports of rule 1"), "70000");
  await user.type(screen.getByLabelText("Remote CIDR of rule 2"), "10.0.0/8");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(screen.getByLabelText("Ports of rule 1")).toHaveAccessibleDescription(
    "Use a port like 22 or a range like 8000-8080 (1–65535).",
  );
  expect(screen.getByLabelText("Remote CIDR of rule 2")).toHaveAccessibleDescription(
    "Use a CIDR like 10.0.0.0/8 or 2001:db8::/32.",
  );
  expect(called).toBe(false);
  // 행을 옮기면 행 번호로 단 오류는 지운다.
  await user.click(screen.getByRole("button", { name: "Move rule 2 up" }));
  expect(screen.getByLabelText("Ports of rule 2")).not.toHaveAttribute("aria-invalid");
});

it("offers to reload when someone else changed the group first", async () => {
  mockLists({ "security-group": [ssh] });
  server.use(
    http.patch(apiUrl(`/security-groups/${ssh.id}`), () =>
      HttpResponse.json(
        { code: "revision_mismatch", message: "expected 1, found 2" },
        { status: 412 },
      ),
    ),
  );
  const { queryClient } = open();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Remove rule 2" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Someone else changed it first.");

  const changed = { ...ssh, revision: 2, spec: { ...ssh.spec, rules: [] } };
  queryClient.setQueryData(["list", "security-group"], { revision: 2, items: [changed] });
  await user.click(screen.getByRole("button", { name: "Reload" }));
  expect(screen.getByText(/^No rules/)).toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("shows the rules read-only to operators", async () => {
  mockLists({ "security-group": [ssh] });
  open({ ...ADMIN, role: "operator" });
  expect(await screen.findByLabelText("Ports of rule 1")).toBeDisabled();
  expect(screen.getByLabelText("Direction of rule 1")).toBeDisabled();
  for (const name of ["Save", "Add a rule", "Remove rule 1"]) {
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
  }
});

it("says when the group does not exist", async () => {
  mockLists({ "security-group": [] });
  open();
  expect(
    await screen.findByRole("heading", { name: "Security group ssh not found" }),
  ).toBeInTheDocument();
  expect(within(document.body).queryByRole("table")).not.toBeInTheDocument();
});
```

`web/src/security-groups/SecurityGroupPage.tsx`를 만든다:

```tsx
import { ArrowDown, ArrowUp, Plus, X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { SecurityGroup } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import {
  emptyRule,
  fromRule,
  hasPorts,
  MAX_DESCRIPTION,
  MAX_RULES,
  moveRule,
  type RuleRow,
  toRule,
  validateRules,
} from "@/security-groups/rules";

/** 보안 그룹 하나: 설명과 규칙 표. 쓰기 권한이 없으면 읽기만 한다. */
export function SecurityGroupPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const groups = useList("security-group");
  if (!groups.data) {
    return <ListPending error={groups.error} />;
  }
  const group = groups.data.find((g) => g.name === name);
  if (!group) {
    return <h1 className="text-2xl font-semibold">{t("securityGroups.notFound", { name })}</h1>;
  }
  return <RulesEditor key={group.id} group={group} />;
}

function RulesEditor({ group }: { group: SecurityGroup }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const write = can(me.role, "write", "security-group");
  const [description, setDescription] = useState(group.spec.description ?? "");
  const [rows, setRows] = useState(() => (group.spec.rules ?? []).map(fromRule));
  // 읽은 revision. 그 사이 다른 사람이 바꿨으면 저장이 실패한다(If-Match).
  const [revision, setRevision] = useState(group.revision);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<{ message: string; conflict: boolean } | null>(null);
  const [busy, setBusy] = useState(false);

  // 행을 넣거나 빼거나 옮기면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const changeRows = (next: RuleRow[]) => {
    setRows(next);
    setErrors({});
  };
  const setRow = (i: number, patch: Partial<RuleRow>) =>
    setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  /** 다른 사람이 바꾼 그룹을 다시 읽는다. `group`은 watch로 늘 최신이다. */
  function reload() {
    setDescription(group.spec.description ?? "");
    setRows((group.spec.rules ?? []).map(fromRule));
    setRevision(group.revision);
    setErrors({});
    setFailure(null);
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const found = validateRules(rows);
    if (description.length > MAX_DESCRIPTION) {
      found.description = "securityGroups.errors.description";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    setFailure(null);
    try {
      const saved = await call(
        api.PATCH("/security-groups/{key}", {
          params: { path: { key: group.id }, header: { "If-Match": revision } },
          body: { description, rules: rows.map(toRule) },
        }),
      );
      setRevision(saved.revision);
      toast.success(t("securityGroups.saved", { name: group.name }));
    } catch (err) {
      setFailure({
        message: errorMessage(t, err),
        conflict: err instanceof ApiError && err.code === "revision_mismatch",
      });
    } finally {
      setBusy(false);
    }
  }

  const n = (i: number) => ({ n: i + 1 });
  return (
    <form onSubmit={(e) => void submit(e)} className="flex flex-col gap-4" noValidate>
      <div className="flex flex-col gap-1">
        <Link
          to="/security-groups"
          className="text-sm text-muted-foreground underline-offset-4 hover:underline"
        >
          {t("nav.securityGroups")}
        </Link>
        <h1 className="text-2xl font-semibold">{group.name}</h1>
      </div>
      <p className="text-sm text-muted-foreground">{t("securityGroups.order")}</p>
      <p className="text-sm text-muted-foreground">{t("securityGroups.always")}</p>
      {/* 보내는 동안이나 읽기 권한뿐이면 고치지 못하게 한다. */}
      <fieldset disabled={busy || !write} className="flex min-w-0 flex-col gap-4">
        <div className="max-w-xl">
          <Field
            id="group-description"
            label={t("securityGroups.description")}
            error={errors.description}
          >
            {(a11y) => (
              <Input
                id="group-description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </div>
        <Table aria-label={t("securityGroups.rules")}>
          <TableHeader>
            <TableRow>
              <TableHead>#</TableHead>
              <TableHead>{t("securityGroups.direction")}</TableHead>
              <TableHead>{t("securityGroups.action")}</TableHead>
              <TableHead>{t("securityGroups.protocol")}</TableHead>
              <TableHead>{t("securityGroups.ports")}</TableHead>
              <TableHead>{t("securityGroups.remote")}</TableHead>
              {write && (
                <TableHead>
                  <span className="sr-only">{t("vms.actions")}</span>
                </TableHead>
              )}
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.length === 0 && (
              <TableRow>
                <TableCell colSpan={write ? 7 : 6} className="text-center text-muted-foreground">
                  {t("securityGroups.noRules")}
                </TableCell>
              </TableRow>
            )}
            {rows.map((row, i) => {
              const portsError = errors[`rules.${i}.ports`];
              const remoteError = errors[`rules.${i}.remote`];
              return (
                <TableRow key={i}>
                  <TableCell>{i + 1}</TableCell>
                  <TableCell>
                    <NativeSelect
                      aria-label={t("securityGroups.directionN", n(i))}
                      value={row.direction}
                      onChange={(e) =>
                        setRow(i, { direction: e.target.value as RuleRow["direction"] })
                      }
                    >
                      <NativeSelectOption value="in">{t("securityGroups.in")}</NativeSelectOption>
                      <NativeSelectOption value="out">{t("securityGroups.out")}</NativeSelectOption>
                    </NativeSelect>
                  </TableCell>
                  <TableCell>
                    <NativeSelect
                      aria-label={t("securityGroups.actionN", n(i))}
                      value={row.action}
                      onChange={(e) => setRow(i, { action: e.target.value as RuleRow["action"] })}
                    >
                      <NativeSelectOption value="accept">
                        {t("securityGroups.accept")}
                      </NativeSelectOption>
                      <NativeSelectOption value="drop">
                        {t("securityGroups.drop")}
                      </NativeSelectOption>
                    </NativeSelect>
                  </TableCell>
                  <TableCell>
                    <NativeSelect
                      aria-label={t("securityGroups.protocolN", n(i))}
                      value={row.protocol}
                      onChange={(e) => {
                        const protocol = e.target.value as RuleRow["protocol"];
                        // 포트는 tcp·udp에서만 쓴다.
                        setRow(i, hasPorts(protocol) ? { protocol } : { protocol, ports: "" });
                      }}
                    >
                      <NativeSelectOption value="any">{t("securityGroups.any")}</NativeSelectOption>
                      <NativeSelectOption value="tcp">TCP</NativeSelectOption>
                      <NativeSelectOption value="udp">UDP</NativeSelectOption>
                      <NativeSelectOption value="icmp">ICMP</NativeSelectOption>
                      <NativeSelectOption value="icmpv6">ICMPv6</NativeSelectOption>
                    </NativeSelect>
                  </TableCell>
                  <TableCell>
                    <Input
                      aria-label={t("securityGroups.portsN", n(i))}
                      className="w-28"
                      placeholder={hasPorts(row.protocol) ? t("securityGroups.anyPort") : "—"}
                      disabled={!hasPorts(row.protocol)}
                      value={row.ports}
                      onChange={(e) => setRow(i, { ports: e.target.value })}
                      {...(portsError && {
                        "aria-invalid": true,
                        "aria-describedby": `rule-${i}-ports-error`,
                      })}
                    />
                    {portsError && (
                      <p id={`rule-${i}-ports-error`} className="text-xs text-destructive">
                        {t(portsError)}
                      </p>
                    )}
                  </TableCell>
                  <TableCell>
                    <Input
                      aria-label={t("securityGroups.remoteN", n(i))}
                      className="w-44"
                      placeholder={t("securityGroups.anywhere")}
                      value={row.remote}
                      onChange={(e) => setRow(i, { remote: e.target.value })}
                      {...(remoteError && {
                        "aria-invalid": true,
                        "aria-describedby": `rule-${i}-remote-error`,
                      })}
                    />
                    {remoteError && (
                      <p id={`rule-${i}-remote-error`} className="text-xs text-destructive">
                        {t(remoteError)}
                      </p>
                    )}
                  </TableCell>
                  {write && (
                    <TableCell className="text-right whitespace-nowrap">
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("securityGroups.moveUp", n(i))}
                        disabled={i === 0}
                        onClick={() => changeRows(moveRule(rows, i, -1))}
                      >
                        <ArrowUp />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("securityGroups.moveDown", n(i))}
                        disabled={i === rows.length - 1}
                        onClick={() => changeRows(moveRule(rows, i, 1))}
                      >
                        <ArrowDown />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("securityGroups.removeRule", n(i))}
                        onClick={() => changeRows(rows.filter((_, j) => j !== i))}
                      >
                        <X />
                      </Button>
                    </TableCell>
                  )}
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
        {write && rows.length < MAX_RULES && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => changeRows([...rows, emptyRule()])}
            >
              <Plus />
              {t("securityGroups.addRule")}
            </Button>
          </div>
        )}
      </fieldset>
      {failure && (
        <div role="alert" className="flex items-center gap-3 text-sm text-destructive">
          <span>{failure.message}</span>
          {failure.conflict && (
            <Button type="button" variant="outline" size="sm" onClick={reload}>
              {t("vmForm.reload")}
            </Button>
          )}
        </div>
      )}
      {write && (
        <div>
          <Button type="submit" disabled={busy}>
            {t("common.save")}
          </Button>
        </div>
      )}
    </form>
  );
}
```

`web/src/security-groups/SecurityGroupsPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { SecurityGroupsPage } from "@/security-groups/SecurityGroupsPage";
import { securityGroup, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const ssh = securityGroup(
  "ssh",
  [
    { direction: "in", action: "accept", protocol: "tcp", ports: "22" },
    { direction: "in", action: "accept", protocol: "icmp" },
  ],
  "SSH from anywhere",
);
const web = vm("web", "running", {
  spec: {
    ...vm("web", "running").spec,
    nics: [{ network: "lan", model: "virtio", security_groups: ["ssh"] }],
  },
});

it("lists groups with their rule counts and the VMs that use them", async () => {
  mockLists({ "security-group": [ssh, securityGroup("empty")], vm: [web] });
  renderPage(<SecurityGroupsPage />);
  const table = await screen.findByRole("table", { name: "Security groups" });
  const headers = within(table).getAllByRole("columnheader");
  const cell = (row: HTMLElement, name: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === name)];
  const row = (await within(table).findByRole("link", { name: "ssh" })).closest("tr")!;
  expect(within(row).getByRole("link", { name: "ssh" })).toHaveAttribute(
    "href",
    "/security-groups/ssh",
  );
  expect(cell(row, "Rules")).toHaveTextContent("2");
  expect(cell(row, "Description")).toHaveTextContent("SSH from anywhere");
  expect(cell(row, "VMs")).toHaveTextContent("1");
  expect(row).toHaveTextContent("Remove it from the VM interfaces first.");
  // VM NIC가 쓰는 그룹은 지울 수 없다.
  expect(within(row).getByRole("button", { name: "Delete ssh" })).toBeDisabled();
  const empty = within(table).getByRole("link", { name: "empty" }).closest("tr")!;
  expect(cell(empty, "Rules")).toHaveTextContent("0");
  expect(cell(empty, "Description")).toHaveTextContent("—");
  expect(within(empty).getByRole("button", { name: "Delete empty" })).toBeEnabled();
});

it("judges nothing without the VMs", async () => {
  mockLists({ "security-group": [ssh] });
  server.use(
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<SecurityGroupsPage />);
  const row = (await screen.findByRole("link", { name: "ssh" })).closest("tr")!;
  expect(within(row).getByRole("button", { name: "Delete ssh" })).toBeDisabled();
});

it("creates a group and opens it to add rules", async () => {
  mockLists({});
  let body: unknown;
  server.use(
    http.post(apiUrl("/security-groups"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(securityGroup("web"), { status: 201 });
    }),
  );
  const { router } = renderPage(<SecurityGroupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a security group" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "Web");
  await userEvent.type(within(dialog).getByLabelText("Description (optional)"), "x".repeat(201));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText(/lowercase letters/)).toBeVisible();
  expect(within(dialog).getByText("Use at most 200 characters.")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "web");
  await userEvent.clear(within(dialog).getByLabelText("Description (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created security group web.")).toBeInTheDocument();
  // 설명이 비면 보내지 않는다.
  expect(body).toEqual({ name: "web", spec: { rules: [] } });
  expect(router.state.location.pathname).toBe("/security-groups/web");
});

it("deletes an unused group and shows why a delete failed", async () => {
  const lab = securityGroup("lab");
  mockLists({ "security-group": [lab] });
  server.use(
    http.delete(apiUrl(`/security-groups/${lab.id}`), () =>
      HttpResponse.json(
        { code: "security_group_in_use", message: 'security group "lab" is used by VM "x"' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SecurityGroupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete lab" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A VM interface uses this security group. Remove it from the VM first.",
  );
});

it("hides changes from operators", async () => {
  mockLists({ "security-group": [ssh] });
  renderPage(<SecurityGroupsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("link", { name: "ssh" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete ssh" })).not.toBeInTheDocument();
});
```

`web/src/security-groups/SecurityGroupsPage.tsx`를 만든다:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { SecurityGroup, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { MAX_DESCRIPTION } from "@/security-groups/rules";
import { validName } from "@/vms/form";

/** 그룹을 NIC에 건 VM 수 */
export function groupUsers(group: SecurityGroup, vms: Vm[]): number {
  return vms.filter((vm) => vm.spec.nics?.some((nic) => nic.security_groups?.includes(group.name)))
    .length;
}

export function SecurityGroupsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const groups = useList("security-group");
  const vms = useList("vm");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<SecurityGroup | null>(null);
  const write = can(me.role, "write", "security-group");

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="security-groups-title" className="text-2xl font-semibold">
          {t("nav.securityGroups")}
        </h1>
        {write && (
          <Button onClick={() => setCreating(true)}>
            <Plus />
            {t("common.create")}
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("securityGroups.note")}</p>
      <Table aria-labelledby="security-groups-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("securityGroups.rules")}</TableHead>
            <TableHead>{t("securityGroups.description")}</TableHead>
            <TableHead>{t("securityGroups.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(groups.data ?? []).length === 0 && (
            <TableMessage
              colSpan={write ? 5 : 4}
              query={groups}
              empty={t("securityGroups.empty")}
            />
          )}
          {(groups.data ?? []).map((group) => {
            // VM 목록을 다 읽기 전에는 모른다.
            const used = vms.isSuccess ? groupUsers(group, vms.data) : undefined;
            return (
              <TableRow key={group.id}>
                <TableCell>
                  <Link
                    to={`/security-groups/${encodeURIComponent(group.name)}`}
                    className="font-medium underline-offset-4 hover:underline"
                  >
                    {group.name}
                  </Link>
                </TableCell>
                <TableCell>{group.spec.rules?.length ?? 0}</TableCell>
                <TableCell className="max-w-xs truncate">{group.spec.description ?? "—"}</TableCell>
                <TableCell>
                  {used ?? "—"}
                  {write && used !== undefined && used > 0 && (
                    <div className="text-xs text-muted-foreground">{t("securityGroups.inUse")}</div>
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: group.name })}
                      // VM NIC가 쓰는 그룹은 서버가 지우지 않는다.
                      disabled={used !== 0}
                      onClick={() => setDeleting(group)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateGroupDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("securityGroups.deleteTitle", { name: deleting?.name })}
        description={t("securityGroups.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const group = deleting!;
          await call(api.DELETE("/security-groups/{key}", { params: { path: { key: group.id } } }));
          toast.success(t("securityGroups.deleted", { name: group.name }));
        }}
      />
    </div>
  );
}

/** 이름과 설명으로 빈 그룹을 만들고, 규칙을 적도록 그 그룹의 화면으로 간다. */
function CreateGroupDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    }
    if (description.length > MAX_DESCRIPTION) {
      found.description = "securityGroups.errors.description";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const spec = { rules: [], ...(description !== "" && { description }) };
    await call(api.POST("/security-groups", { body: { name, spec } }));
    toast.success(t("securityGroups.created", { name }));
    void navigate(`/security-groups/${encodeURIComponent(name)}`);
  }

  return (
    <FormDialog
      title={t("securityGroups.createTitle")}
      description={t("securityGroups.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="group-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input id="group-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      <Field
        id="group-description"
        label={t("securityGroups.descriptionOptional")}
        error={errors.description}
      >
        {(a11y) => (
          <Input
            id="group-description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/security-groups/rules.test.ts`를 만든다:

```ts
import {
  cidrFamily,
  fromRule,
  moveRule,
  type RuleRow,
  toRule,
  validateRules,
  validPorts,
} from "@/security-groups/rules";

const row = (patch: Partial<RuleRow>): RuleRow => ({
  direction: "in",
  action: "accept",
  protocol: "tcp",
  ports: "",
  remote: "",
  ...patch,
});

it("round-trips rules and leaves out empty values", () => {
  const full = {
    direction: "in",
    action: "accept",
    protocol: "tcp",
    ports: "22",
    remote: "10.0.0.0/8",
  } as const;
  expect(toRule(fromRule(full))).toEqual(full);
  expect(fromRule({ direction: "out", action: "drop" })).toEqual(
    row({ direction: "out", action: "drop", protocol: "any" }),
  );
  expect(toRule(row({ protocol: "any" }))).toEqual({ direction: "in", action: "accept" });
  // 포트는 tcp·udp에서만 보낸다. 앞뒤 빈칸은 뺀다.
  expect(toRule(row({ protocol: "icmp", ports: "22" }))).toEqual({
    direction: "in",
    action: "accept",
    protocol: "icmp",
  });
  expect(toRule(row({ protocol: "udp", ports: " 53 ", remote: " ::1 " }))).toEqual({
    direction: "in",
    action: "accept",
    protocol: "udp",
    ports: "53",
    remote: "::1",
  });
});

it("follows the server's port range rule", () => {
  for (const ok of ["1", "22", "65535", "8000-8080", "80-80"]) {
    expect(validPorts(ok)).toBe(true);
  }
  for (const bad of ["", "0", "65536", "8080-8000", "22-", "-22", "a", "1 - 2", "0-10"]) {
    expect(validPorts(bad)).toBe(false);
  }
});

it("tells the CIDR family", () => {
  expect(cidrFamily("10.0.0.0/8")).toBe(4);
  expect(cidrFamily("192.168.1.10")).toBe(4);
  expect(cidrFamily("0.0.0.0/0")).toBe(4);
  expect(cidrFamily("2001:db8::/32")).toBe(6);
  expect(cidrFamily("::/0")).toBe(6);
  expect(cidrFamily("fe80::1")).toBe(6);
  for (const bad of ["", "10.0.0/8", "256.0.0.0", "10.0.0.0/33", "::/129", "1::2::3", "a/b/c"]) {
    expect(cidrFamily(bad)).toBeUndefined();
  }
  expect(cidrFamily("10.0.0.0/x")).toBeUndefined();
});

it("checks ports, addresses and ICMP families", () => {
  expect(
    validateRules([
      row({ ports: "22" }),
      row({ ports: "99999" }),
      // tcp·udp가 아니면 포트는 보내지 않으므로 보지 않는다.
      row({ protocol: "any", ports: "x" }),
      row({ remote: "10.0.0.0/40" }),
      row({ protocol: "icmp", remote: "2001:db8::/32" }),
      row({ protocol: "icmpv6", remote: "10.0.0.0/8" }),
      row({ protocol: "icmpv6", remote: "2001:db8::/32" }),
    ]),
  ).toEqual({
    "rules.1.ports": "securityGroups.errors.ports",
    "rules.3.remote": "securityGroups.errors.remote",
    "rules.4.remote": "securityGroups.errors.icmpRemote",
    "rules.5.remote": "securityGroups.errors.icmpv6Remote",
  });
});

it("moves a rule up or down within the list", () => {
  expect(moveRule(["a", "b", "c"], 1, -1)).toEqual(["b", "a", "c"]);
  expect(moveRule(["a", "b", "c"], 1, 1)).toEqual(["a", "c", "b"]);
  expect(moveRule(["a", "b", "c"], 0, -1)).toEqual(["a", "b", "c"]);
  expect(moveRule(["a", "b", "c"], 2, 1)).toEqual(["a", "b", "c"]);
});
```

`web/src/security-groups/rules.ts`를 만든다:

```ts
import type { components } from "@/api/schema";
import type { Rule } from "@/api/types";

type Schemas = components["schemas"];

/** 서버의 `MAX_RULES` */
export const MAX_RULES = 200;
/** 서버의 `MAX_DESCRIPTION` */
export const MAX_DESCRIPTION = 200;
/** 서버의 `MAX_NIC_SECURITY_GROUPS` */
export const MAX_NIC_SECURITY_GROUPS = 8;

/** 규칙 표의 한 행. 프로토콜이 없으면 `any`, 포트와 원격 주소가 없으면 빈 글이다. */
export interface RuleRow {
  direction: Schemas["Direction"];
  action: Schemas["Action"];
  protocol: Schemas["Protocol"] | "any";
  ports: string;
  remote: string;
}

export function emptyRule(): RuleRow {
  return { direction: "in", action: "accept", protocol: "tcp", ports: "", remote: "" };
}

export function fromRule(rule: Rule): RuleRow {
  return {
    direction: rule.direction,
    action: rule.action,
    protocol: rule.protocol ?? "any",
    ports: rule.ports ?? "",
    remote: rule.remote ?? "",
  };
}

/** 포트는 tcp·udp에서만 쓴다. */
export function hasPorts(protocol: RuleRow["protocol"]): boolean {
  return protocol === "tcp" || protocol === "udp";
}

/** 행을 API 규칙으로. 빈 값은 보내지 않는다. */
export function toRule(row: RuleRow): Rule {
  const rule: Rule = { direction: row.direction, action: row.action };
  if (row.protocol !== "any") {
    rule.protocol = row.protocol;
  }
  if (hasPorts(row.protocol) && row.ports.trim() !== "") {
    rule.ports = row.ports.trim();
  }
  if (row.remote.trim() !== "") {
    rule.remote = row.remote.trim();
  }
  return rule;
}

/** 서버의 `PortRange`: `22` 또는 `8000-8080`(1–65535, 시작 ≤ 끝) */
export function validPorts(text: string): boolean {
  const m = /^(\d{1,5})(?:-(\d{1,5}))?$/.exec(text);
  if (!m) {
    return false;
  }
  const start = Number(m[1]);
  const end = m[2] === undefined ? start : Number(m[2]);
  return start >= 1 && end <= 65535 && start <= end;
}

/**
 * CIDR(`10.0.0.0/8`, `2001:db8::/32`, 접두사 생략 가능)의 주소 종류. 모양이 틀리면 undefined. IPv6
 * 주소의 자세한 문법과 호스트 비트는 서버가 본다.
 */
export function cidrFamily(text: string): 4 | 6 | undefined {
  const [addr = "", prefix, ...rest] = text.split("/");
  if (rest.length > 0 || (prefix !== undefined && !/^\d{1,3}$/.test(prefix))) {
    return undefined;
  }
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(addr);
  if (v4) {
    const octets = v4.slice(1).map(Number);
    return octets.every((o) => o <= 255) && (prefix === undefined || Number(prefix) <= 32)
      ? 4
      : undefined;
  }
  const v6 =
    addr.includes(":") &&
    /^[0-9a-f:.]+$/i.test(addr) &&
    addr.split("::").length <= 2 &&
    (prefix === undefined || Number(prefix) <= 128);
  return v6 ? 6 : undefined;
}

/** 규칙 행의 입력 오류: `rules.<i>.ports`·`rules.<i>.remote` → 번역 키. 나머지 검사는 서버가 한다. */
export function validateRules(rows: RuleRow[]): Record<string, string> {
  const errors: Record<string, string> = {};
  rows.forEach((row, i) => {
    if (hasPorts(row.protocol) && row.ports.trim() !== "" && !validPorts(row.ports.trim())) {
      errors[`rules.${i}.ports`] = "securityGroups.errors.ports";
    }
    const remote = row.remote.trim();
    if (remote === "") {
      return;
    }
    const family = cidrFamily(remote);
    if (family === undefined) {
      errors[`rules.${i}.remote`] = "securityGroups.errors.remote";
    } else if (row.protocol === "icmp" && family === 6) {
      errors[`rules.${i}.remote`] = "securityGroups.errors.icmpRemote";
    } else if (row.protocol === "icmpv6" && family === 4) {
      errors[`rules.${i}.remote`] = "securityGroups.errors.icmpv6Remote";
    }
  });
  return errors;
}

/** 규칙 `i`를 `by`(−1 위, +1 아래)만큼 옮긴 목록. 끝을 넘으면 그대로다. */
export function moveRule<T>(rows: T[], i: number, by: -1 | 1): T[] {
  const j = i + by;
  if (j < 0 || j >= rows.length) {
    return rows;
  }
  // 이웃한 두 행을 맞바꾼다.
  const lo = Math.min(i, j);
  return [...rows.slice(0, lo), ...rows.slice(lo, lo + 2).reverse(), ...rows.slice(lo + 2)];
}
```

`web/src/test/fixtures.ts`에서

```ts
import type { Node, Snapshot, StoragePool, Task, Vm, Volume } from "@/api/types";
import type { PoolState } from "@/storage/pools";
```

을 다음으로 바꾼다:

```ts
import type {
  Node,
  Rule,
  SecurityGroup,
  Snapshot,
  StoragePool,
  Task,
  Vm,
  Volume,
} from "@/api/types";
import type { PoolState } from "@/storage/pools";
```

`web/src/test/fixtures.ts`에서

```ts
    status: { phase: "ready", created_at: 1_792_000_000, vmstate_bytes: null, ...extra.status },
  };
}
```

을 다음으로 바꾼다:

```ts
    status: { phase: "ready", created_at: 1_792_000_000, vmstate_bytes: null, ...extra.status },
  };
}

/** 테스트용 보안 그룹 */
export function securityGroup(
  name: string,
  rules: Rule[] = [],
  description?: string,
): SecurityGroup {
  return {
    id: `019a0f3c-0000-7000-8005-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { rules, ...(description && { description }) },
    status: {},
  };
}
```

`web/src/test/render.tsx`에서

```tsx
  snapshot: "/snapshots",
};
```

을 다음으로 바꾼다:

```tsx
  snapshot: "/snapshots",
  "security-group": "/security-groups",
};
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
    disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
    nics: [{ network: "lan", model: "virtio", mac: "52:54:00:12:34:56" }],
  },
```

을 다음으로 바꾼다:

```tsx
    disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
    nics: [
      {
        network: "lan",
        model: "virtio",
        mac: "52:54:00:12:34:56",
        security_groups: ["web", "ssh"],
      },
    ],
  },
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
  expect(screen.getByText("52:54:00:12:34:56")).toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByText("52:54:00:12:34:56")).toBeInTheDocument();
  expect(screen.getByText("web, ssh")).toBeInTheDocument();
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
              <TableHead>{t("vms.mac")}</TableHead>
            </TableRow>
```

을 다음으로 바꾼다:

```tsx
              <TableHead>{t("vms.mac")}</TableHead>
              <TableHead>{t("vmForm.securityGroups")}</TableHead>
            </TableRow>
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
              <TableRow>
                <TableCell colSpan={3} className="text-center text-muted-foreground">
                  {t("vms.noNics")}
```

을 다음으로 바꾼다:

```tsx
              <TableRow>
                <TableCell colSpan={4} className="text-center text-muted-foreground">
                  {t("vms.noNics")}
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
                <TableCell className="font-mono text-xs">{nic.mac ?? "—"}</TableCell>
              </TableRow>
```

을 다음으로 바꾼다:

```tsx
                <TableCell className="font-mono text-xs">{nic.mac ?? "—"}</TableCell>
                <TableCell>
                  {nic.security_groups?.length ? nic.security_groups.join(", ") : "—"}
                </TableCell>
              </TableRow>
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
import type { Task } from "@/api/types";
import { NODE, pool, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import type { Task } from "@/api/types";
import { NODE, pool, securityGroup, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx

it("keeps created volumes when the VM cannot be created, and does not recreate them", async () => {
```

을 다음으로 바꾼다:

```tsx

it("keeps the interfaces and MAC addresses of a running VM", async () => {
  const web = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      nics: [{ network: "user", model: "virtio", mac: "52:54:00:00:00:01" }],
    },
  });
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  expect(await screen.findByLabelText("MAC address")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Remove interface 1" })).toBeDisabled();
  expect(screen.queryByRole("button", { name: "Add an interface" })).not.toBeInTheDocument();
  expect(
    screen.getByText(
      "Interfaces cannot be added or removed, and their MAC addresses cannot change, while the VM is running.",
    ),
  ).toBeInTheDocument();
  // 네트워크와 모델은 바꿀 수 있다(다음 시작부터).
  expect(screen.getByLabelText("Network")).toBeEnabled();
});

it("keeps created volumes when the VM cannot be created, and does not recreate them", async () => {
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
  expect(screen.queryByRole("button", { name: "Add an interface" })).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("button", { name: "Add an interface" })).not.toBeInTheDocument();
});

it("changes the security groups of a running VM's interfaces", async () => {
  const web = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      nics: [
        { network: "lan", model: "virtio", mac: "52:54:00:00:00:01", security_groups: ["ssh"] },
        { network: "user", model: "virtio", mac: "52:54:00:00:00:02" },
      ],
    },
  });
  mockLists({
    vm: [web],
    node: [NODE],
    network: [{ id: "net", name: "lan", revision: 1, labels: {}, spec: { bridge: "br0" } }],
    "security-group": [securityGroup("web"), securityGroup("ssh")],
  });
  let body: { spec: { nics: unknown } } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const groups = await screen.findByRole("group", {
    name: "Security groups of interface 1",
  });
  expect(await within(groups).findByRole("checkbox", { name: "web" })).not.toBeChecked();
  expect(within(groups).getByRole("checkbox", { name: "ssh" })).toBeChecked();
  // user 네트워크(NAT)에는 그룹을 걸 수 없다.
  expect(
    screen.queryByRole("group", { name: "Security groups of interface 2" }),
  ).not.toBeInTheDocument();
  await user.click(within(groups).getByRole("checkbox", { name: "web" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(body).toBeDefined());
  expect(body!.spec.nics).toEqual([
    {
      network: "lan",
      model: "virtio",
      mac: "52:54:00:00:00:01",
      security_groups: ["ssh", "web"],
    },
    { network: "user", model: "virtio", mac: "52:54:00:00:00:02" },
  ]);
});

it("drops the security groups of an interface moved to the user network", async () => {
  mockLists({
    node: [NODE],
    network: [{ id: "net", name: "lan", revision: 1, labels: {}, spec: { bridge: "br0" } }],
    "security-group": [securityGroup("ssh")],
  });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  const nic = await screen.findByRole("group", { name: "Network interface 1" });
  await within(nic).findByRole("option", { name: "lan" });
  await user.selectOptions(within(nic).getByLabelText("Network"), "lan");
  await user.click(await within(nic).findByRole("checkbox", { name: "ssh" }));
  await user.selectOptions(within(nic).getByLabelText("Network"), "user");
  await user.selectOptions(within(nic).getByLabelText("Network"), "lan");
  expect(within(nic).getByRole("checkbox", { name: "ssh" })).not.toBeChecked();
});
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
  const networks = useList("network");
  const volumes = useList("volume");
```

을 다음으로 바꾼다:

```tsx
  const networks = useList("network");
  const securityGroups = useList("security-group");
  const volumes = useList("volume");
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
  const creating = vm === undefined;
  // 멈추지 않은 VM(서버의 `is_stopped`가 아닌)의 노드와 디스크는 서버가 바꾸지 않는다.
  const locked = !creating && !isStopped(vm);
```

을 다음으로 바꾼다:

```tsx
  const creating = vm === undefined;
  // 멈추지 않은 VM(서버의 `is_stopped`가 아닌)의 노드·디스크와 NIC 개수·순서·MAC은 서버가 바꾸지 않는다.
  const locked = !creating && !isStopped(vm);
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
          networks={networks.data}
        />
```

을 다음으로 바꾼다:

```tsx
          networks={networks.data}
          securityGroups={securityGroups.data}
          locked={locked}
        />
```

`web/src/vms/form.test.ts`에서

```ts
    ],
    nics: [{ network: "user", model: "virtio" as const, mac: "52:54:00:12:34" }],
  };
```

을 다음으로 바꾼다:

```ts
    ],
    nics: [
      { network: "user", model: "virtio" as const, mac: "52:54:00:12:34", securityGroups: [] },
    ],
  };
```

`web/src/vms/form.test.ts`에서

```ts
      disks: [{ volume: "web-root", bus: "sata", boot_order: 1 }],
      nics: [{ network: "lan", model: "e1000e", mac: "52:54:00:12:34:56" }],
      snapshot_schedule: { every: "daily", keep: 7, memory: true },
```

을 다음으로 바꾼다:

```ts
      disks: [{ volume: "web-root", bus: "sata", boot_order: 1 }],
      nics: [
        {
          network: "lan",
          model: "e1000e",
          mac: "52:54:00:12:34:56",
          security_groups: ["web", "db"],
        },
        { network: "user", model: "virtio", mac: "52:54:00:12:34:57" },
      ],
      snapshot_schedule: { every: "daily", keep: 7, memory: true },
```

`web/src/vms/form.test.ts`에서

```ts
  expect(toSpec(fromVm(plain), "stopped").snapshot_schedule).toBeNull();
});
```

을 다음으로 바꾼다:

```ts
  expect(toSpec(fromVm(plain), "stopped").snapshot_schedule).toBeNull();
});

it("allows up to eight security groups on an interface", () => {
  const nic = (count: number) => ({
    network: "lan",
    model: "virtio" as const,
    mac: "",
    securityGroups: Array.from({ length: count }, (_, i) => `g${i}`),
  });
  const v = { ...emptyForm("q35"), name: "web", nics: [nic(8), nic(9)] };
  expect(validate(v, true)).toEqual({ "nics.1.securityGroups": "vmForm.errors.securityGroups" });
  // 그룹이 없는 NIC는 `security_groups`를 보내지 않는다.
  expect(toSpec({ ...v, nics: [nic(0), nic(1)] }, "stopped").nics).toEqual([
    { network: "lan", model: "virtio", mac: null },
    { network: "lan", model: "virtio", mac: null, security_groups: ["g0"] },
  ]);
});
```

`web/src/vms/form.test.ts`에서

```ts
    nics: [
      { network: "user", model: "virtio" as const, mac: "01:00:5e:00:00:01" },
      { network: "user", model: "virtio" as const, mac: "52:54:00:12:34:56" },
      { network: "user", model: "virtio" as const, mac: "52:54:00:12:34:56" },
    ],
```

을 다음으로 바꾼다:

```ts
    nics: [
      { network: "user", model: "virtio" as const, mac: "01:00:5e:00:00:01", securityGroups: [] },
      { network: "user", model: "virtio" as const, mac: "52:54:00:12:34:56", securityGroups: [] },
      { network: "user", model: "virtio" as const, mac: "52:54:00:12:34:56", securityGroups: [] },
    ],
```

`web/src/vms/form.ts`에서

```ts
import type { Node, Vm, VmSpec } from "@/api/types";
```

을 다음으로 바꾼다:

```ts
import type { Node, Vm, VmSpec } from "@/api/types";
import { MAX_NIC_SECURITY_GROUPS } from "@/security-groups/rules";
```

`web/src/vms/form.ts`에서

```ts
  mac: string;
}
```

을 다음으로 바꾼다:

```ts
  mac: string;
  /** 거는 보안 그룹 이름. `user` 네트워크는 쓸 수 없다. */
  securityGroups: string[];
}
```

`web/src/vms/form.ts`에서

```ts
    disks: [],
    nics: [{ network: "user", model: "virtio", mac: "" }],
    node: "",
```

을 다음으로 바꾼다:

```ts
    disks: [],
    nics: [{ network: "user", model: "virtio", mac: "", securityGroups: [] }],
    node: "",
```

`web/src/vms/form.ts`에서

```ts
    })),
    nics: (s.nics ?? []).map((n) => ({ network: n.network, model: n.model, mac: n.mac ?? "" })),
    node: s.node ?? "",
```

을 다음으로 바꾼다:

```ts
    })),
    nics: (s.nics ?? []).map((n) => ({
      network: n.network,
      model: n.model,
      mac: n.mac ?? "",
      securityGroups: n.security_groups ?? [],
    })),
    node: s.node ?? "",
```

`web/src/vms/form.ts`에서

```ts
  v.nics.forEach((n, i) => {
    if (n.mac === "") {
```

을 다음으로 바꾼다:

```ts
  v.nics.forEach((n, i) => {
    if (n.securityGroups.length > MAX_NIC_SECURITY_GROUPS) {
      errors[`nics.${i}.securityGroups`] = "vmForm.errors.securityGroups";
    }
    if (n.mac === "") {
```

`web/src/vms/form.ts`에서

```ts
      mac: n.mac === "" ? null : n.mac.toLowerCase(),
    })),
```

을 다음으로 바꾼다:

```ts
      mac: n.mac === "" ? null : n.mac.toLowerCase(),
      ...(n.securityGroups.length > 0 && { security_groups: n.securityGroups }),
    })),
```

`web/src/vms/form/NicsSection.tsx` 전체:

```tsx
import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { Network, SecurityGroup } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { MAX_NICS, type NicRow, type VmFormValues } from "@/vms/form";

/** 네트워크 카드 행 */
export function NicsSection({
  values,
  errors,
  onChange,
  onRowsChange,
  networks,
  securityGroups,
  locked,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 행을 넣거나 뺀다. 행 번호로 단 오류가 다른 행을 가리키므로 오류도 지운다. */
  onRowsChange: (nics: NicRow[]) => void;
  networks: Network[] | undefined;
  securityGroups: SecurityGroup[] | undefined;
  /** 실행 중: NIC 개수·순서·MAC은 서버가 바꾸지 않는다. */
  locked: boolean;
}) {
  const { t } = useTranslation();
  const setNic = (i: number, patch: Partial<NicRow>) =>
    onChange({ nics: values.nics.map((n, j) => (j === i ? { ...n, ...patch } : n)) });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vms.nics")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {locked && <p className="text-sm text-muted-foreground">{t("vmForm.nicsRunning")}</p>}
        {values.nics.map((nic, i) => (
          <fieldset
            key={i}
            aria-label={t("vmForm.nicN", { n: i + 1 })}
            className="grid grid-cols-2 items-end gap-2 rounded-md border p-3 sm:grid-cols-4"
          >
            <Field id={`nic-${i}-network`} label={t("vms.network")}>
              <NativeSelect
                id={`nic-${i}-network`}
                className="w-full"
                value={nic.network}
                // `user` 네트워크(QEMU NAT)에는 보안 그룹을 걸 수 없다.
                onChange={(e) =>
                  setNic(
                    i,
                    e.target.value === "user"
                      ? { network: e.target.value, securityGroups: [] }
                      : { network: e.target.value },
                  )
                }
              >
                <NativeSelectOption value="user">{t("vmForm.userNetwork")}</NativeSelectOption>
                {networks?.map((n) => (
                  <NativeSelectOption key={n.id} value={n.name}>
                    {n.name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
            <Field id={`nic-${i}-model`} label={t("vms.model")}>
              <NativeSelect
                id={`nic-${i}-model`}
                className="w-full"
                value={nic.model}
                onChange={(e) => setNic(i, { model: e.target.value as NicRow["model"] })}
              >
                <NativeSelectOption value="virtio">virtio</NativeSelectOption>
                <NativeSelectOption value="e1000e">e1000e</NativeSelectOption>
              </NativeSelect>
            </Field>
            <Field id={`nic-${i}-mac`} label={t("vms.mac")} error={errors[`nics.${i}.mac`]}>
              {(a11y) => (
                <Input
                  id={`nic-${i}-mac`}
                  {...a11y}
                  placeholder={t("vmForm.macAuto")}
                  disabled={locked}
                  value={nic.mac}
                  onChange={(e) => setNic(i, { mac: e.target.value })}
                />
              )}
            </Field>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={t("vmForm.removeNic", { n: i + 1 })}
              disabled={locked}
              onClick={() => onRowsChange(values.nics.filter((_, j) => j !== i))}
            >
              <X />
            </Button>
            {nic.network !== "user" && (
              <NicGroups
                index={i}
                chosen={nic.securityGroups}
                groups={securityGroups}
                error={errors[`nics.${i}.securityGroups`]}
                onChange={(chosen) => setNic(i, { securityGroups: chosen })}
              />
            )}
          </fieldset>
        ))}
        {!locked && values.nics.length < MAX_NICS && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                onRowsChange([
                  ...values.nics,
                  { network: "user", model: "virtio", mac: "", securityGroups: [] },
                ])
              }
            >
              <Plus />
              {t("vmForm.addNic")}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** NIC 하나의 보안 그룹 고르기. 고른 차례대로 규칙을 본다. */
function NicGroups({
  index,
  chosen,
  groups,
  error,
  onChange,
}: {
  index: number;
  chosen: string[];
  groups: SecurityGroup[] | undefined;
  error: string | undefined;
  onChange: (chosen: string[]) => void;
}) {
  const { t } = useTranslation();
  // 목록을 아직 읽지 못했어도 이미 건 그룹은 보인다.
  const names = [...new Set([...(groups ?? []).map((g) => g.name), ...chosen])].sort();
  const errorId = `nic-${index}-groups-error`;
  return (
    <div
      role="group"
      aria-label={t("vmForm.nicGroups", { n: index + 1 })}
      {...(error && { "aria-describedby": errorId })}
      className="col-span-full flex flex-col gap-1.5"
    >
      <span className="text-sm font-medium">{t("vmForm.securityGroups")}</span>
      {names.length === 0 ? (
        <p className="text-xs text-muted-foreground">{t("vmForm.noSecurityGroups")}</p>
      ) : (
        <div className="flex flex-wrap gap-x-4 gap-y-2">
          {names.map((name) => {
            const id = `nic-${index}-group-${name}`;
            return (
              <div key={name} className="flex items-center gap-2">
                <Checkbox
                  id={id}
                  checked={chosen.includes(name)}
                  onCheckedChange={(on) =>
                    onChange(on === true ? [...chosen, name] : chosen.filter((g) => g !== name))
                  }
                />
                <Label htmlFor={id}>{name}</Label>
              </div>
            );
          })}
        </div>
      )}
      {error && (
        <p id={errorId} className="text-xs text-destructive">
          {t(error)}
        </p>
      )}
    </div>
  );
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 938개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 351개

- [ ] **Step 3: 커밋**

```bash
git add crates web
git commit -m "feat: oplctl·웹에서 보안 그룹 — security-group 명령과 규칙 문자열, --nic sg=, vm nic-sg, 보안 그룹 화면과 VM NIC 그룹 고르기"
```

---

### Task 6: E2E와 문서

`crates/opld/tests/firewall.rs`(기능 `qemu-it,net-it`, Linux root): opld 하나, 브리지 네트워크, 보안 그룹 NIC VM을 켜면 `nft -j list table bridge opl_<tag>`에 그 tap의 체인·맵 원소와 규칙이 있고, 그룹 규칙을 바꾸면 곧 반영, 그룹 없는 VM의 tap은 없고, 다 지우면 테이블이 사라지며, 노드는 `firewall: true`. README 방화벽 절(그룹 만들기, 규칙 뜻·순서·기본 정책, 늘 허용되는 것, NIC에 걸기, 실행 중 바꾸기 규칙, 노드 요구, br_netfilter·호스트 iptables, 열리는 틈과 한계, 업그레이드 먼저), 설계 "구현(계획 26)" 메모, 처음 분해 문서(하위 프로젝트 6 완료).

**Files:**
- Create: `crates/opld/tests/firewall.rs`
- Modify: `README.md`
- Modify: `crates/opld/Cargo.toml`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-03-network-sdn-design.md`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

````markdown
```bash
sudo apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils iproute2
# 브리지는 관리자가 미리 만든다. VLAN 태그를 쓰려면 VLAN-aware여야 한다.
````

을 다음으로 바꾼다:

````markdown
```bash
sudo apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils iproute2 nftables   # nftables는 VM 방화벽
# 브리지는 관리자가 미리 만든다. VLAN 태그를 쓰려면 VLAN-aware여야 한다.
````

`README.md`에서

```markdown
  쓰면 서로 방해할 수 있다(장치 이름이 겹쳐 나중 쪽의 그 네트워크는 장치를 만들지 못한다) — 클러스터마다 VNI를 나눈다.
```

을 다음으로 바꾼다:

````markdown
  쓰면 서로 방해할 수 있다(장치 이름이 겹쳐 나중 쪽의 그 네트워크는 장치를 만들지 못한다) — 클러스터마다 VNI를 나눈다.

## VM 방화벽 (보안 그룹)

보안 그룹은 VM NIC에 거는 규칙 목록이다(Proxmox의 VM 방화벽, 클라우드의 security group과 같은 자리). 그룹을 건
NIC는 방화벽이 켜져 규칙에 맞는 것만 들어오고, 노드의 opld가 nftables bridge family 규칙으로 tap에서 거른다.
그룹을 걸지 않은 NIC는 지금처럼 거르지 않는다.

```bash
oplctl security-group create web --description "web servers" \
  --rule "in accept tcp 80" --rule "in accept tcp 22 10.0.0.0/8" --rule "out drop tcp 25"
oplctl security-group rule add web "in accept icmp"          # 맨 끝에 더한다
oplctl security-group rule add web "in drop tcp 22 10.0.9.0/24" --at 1   # 1번 자리에 넣는다
oplctl security-group rule rm web 3                          # 3번 규칙(1부터)을 뺀다
oplctl security-group list
# NAME  RULES  DESCRIPTION
# web   4      web servers
oplctl security-group get web                                # 규칙을 순서대로(JSON)
oplctl vm create web1 --memory 1024 --disk root --nic lan,sg=web+ssh --start   # NIC에 그룹 둘(web, ssh)
oplctl vm nic-sg web1 0 web                                  # 0번 NIC의 그룹을 web 하나로(실행 중에도 된다)
oplctl vm nic-sg web1 0                                      # 0번 NIC의 그룹을 모두 뺀다(방화벽 끔)
oplctl security-group delete web                             # 그룹을 쓰는 NIC가 없어야 한다
```

웹은 "보안 그룹" 메뉴에서 그룹을 만들고 지우며, 그룹 화면의 규칙 표에서 방향·동작·프로토콜·포트·원격 CIDR을
고치고 순서를 바꿔 저장한다. VM 편집의 NIC마다 그룹을 고른다. API는 `/api/v1/security-groups`(목록·만들기·읽기·
지우기)와 `PATCH /api/v1/security-groups/{name}`(`{"description": …, "rules": […]}` 중 보낸 것만, `If-Match`로
revision), VM NIC의 `security_groups: ["web"]`, Node `status.firewall`이다.

- **규칙.** `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`. `in`은 VM으로 들어오는 것, `out`은 VM에서
  나가는 것이다. 프로토콜을 빼면 모든 프로토콜이다. `PORTS`는 목적지 포트 `22`나 `8000-8080`(tcp·udp에서만),
  `CIDR`은 `in`이면 보낸 쪽, `out`이면 받는 쪽 주소다(`10.0.0.0/8`, `2001:db8::/32`, 접두사를 빼면 주소 하나,
  `0.0.0.0/0`은 IPv4 전체). IPv4 CIDR은 IPv4 패킷에만, IPv6 CIDR은 IPv6에만 맞는다. `icmp`는 IPv4 CIDR과만,
  `icmpv6`는 IPv6 CIDR과만 쓴다. 그룹당 규칙 200개, NIC당 그룹 8개(겹치면 안 된다)까지다. 잘못된 규칙은 400
  (`rule 3: …`처럼 번호를 붙인다), VM이 쓰는 그룹 지우기는 409 `security_group_in_use`다.
- **순서와 기본 정책.** NIC의 그룹 순서대로, 그룹 안에서는 적힌 순서대로 보고 **처음 맞는 규칙**을 따른다. 맞는
  규칙이 없으면 **들어오기는 막고 나가기는 보낸다**. 그래서 규칙 없는 그룹 하나만 걸면 "들어오기 모두 막기, 나가기
  모두 허용"이다. 같은 브리지의 방화벽 VM 둘 사이 패킷은 보낸 VM의 `out`과 받는 VM의 `in`을 모두 통과해야 한다
  (`out accept`는 끝이 아니라 받는 쪽 검사로 넘긴다). 노드 자신과 주고받는 것(브리지에 노드 주소가 있을 때)도 같다.
- **늘 허용되는 것.** 이미 맺은 연결의 응답(conntrack `established,related`, 그래서 나가는 연결의 응답은 `in`
  규칙이 없어도 들어온다), ARP, IPv6 이웃 찾기(neighbor solicit·advert, router advert), DHCP 응답(UDP 67→68,
  547→546). 잘못된 연결 상태(`invalid`)의 패킷과 **NIC의 MAC이 아닌 보낸 MAC**(위조)은 늘 막는다.
- **NIC에 걸기.** 브리지·VXLAN 네트워크 NIC에만 건다(`user` 네트워크는 400). 없는 그룹은 400이다(VM 복제도
  원본 스펙의 그룹이 모두 있어야 한다). 노드를 정해 둔 VM에 그룹을 걸면 그 노드에 방화벽이 있어야 한다(409
  `firewall_unsupported`). 실행 중인 VM의 그룹을 바꾸거나(`vm nic-sg`, 웹 VM 편집, `PUT /api/v1/vms/{name}`) 그룹
  규칙을 바꾸면 VM을 다시 켜지 않아도 몇 초 안에 적용된다. 다만 실행 중인 VM은 그룹이 없어도 NIC를 더하거나 빼거나
  순서·MAC을 바꿀 수 없고, 그룹을 건 NIC가 있는(바꾸기 전이나 뒤) VM은 실행 중에 **그룹만** 바꿀 수 있다 — 먼저
  끈다(409 `vm_running`; 규칙은 NIC 순번과 MAC으로 tap을 찾으므로 도는 tap과 어긋나지 않게). 그룹이 없는 VM의
  네트워크·모델 변경은 지금처럼 다음 시작부터 적용된다.
- **누가 바꾸나.** 그룹을 만들고 바꾸고 지우는 것은 admin만이다. VM NIC에 그룹을 걸거나 떼는 것은 VM 쓰기
  권한(operator 포함)이라 operator도 VM의 방화벽을 끌 수 있다(그룹을 모두 떼면 거르지 않는다).
- **실시간 적용.** 노드마다 opld가 클러스터의 모든 방화벽 NIC를 테이블 `bridge opl_<클러스터 태그>`(CA 지문 앞 12자)
  하나로 만들어 `nft -f`로 한 번에 바꾼다(바꾸는 동안 규칙이 빈 순간이 없다). opld 시작, VM·보안 그룹이 바뀔 때
  (0.5초 모아 한 번), 그리고 60초마다(누가 테이블을 지웠어도 되살린다) 넣는다. 그룹을 건 VM은 켜기(마이그레이션
  받기 포함) 직전에 규칙을 넣고, 넣지 못하면 켜지 않는다("failed to start: firewall: …", 방화벽 없이 도는 순간이
  없다). 이미 도는 VM에 다시 붙거나 멈춘 채 남은 VM을 이어서 실행할 때(opld를 다시 시작한 뒤 등)도 먼저 넣는다. 방화벽 NIC가 하나도
  없으면 테이블을 지운다. opld를 멈춰도 규칙은 남고(도는 VM을 계속 지킨다), `cluster leave`로 빠지면 지운다.
  `nft list table bridge opl_<태그>`로 볼 수 있다(NIC마다 체인 `o_<tap>`·`i_<tap>`). 없는 그룹을 건 NIC(그룹을
  저장소에서 바로 지운 때 등)는 들어오기와 나가기를 모두 막는다.
- **비는 때와 실패.** 누가 `nft flush ruleset`을 하거나 `nftables.service`를 다시 읽으면(`systemctl reload
  nftables`, 보통 `flush ruleset`으로 시작한다) 테이블이 사라져 **다음 60초 차례까지 그 노드의 방화벽 VM이 열린다.**
  nftables 설정을 다시 읽은 뒤에는 opld를 다시 시작하면 곧바로 되살린다. 에이전트의 규칙 넣기 실패(VM 켜기 직전
  확인 말고)는 opld 로그에 경고로만 남고 API·웹에는 보이지 않는다 — 그동안은 마지막으로 넣은 규칙이 남는다.
- **노드 요구.** Linux, `nftables`(`nft` 명령), bridge family conntrack이 되는 커널 5.3 이상, root로 도는 opld.
  opld는 시작할 때 이 클러스터의 테이블을 실제로 넣어 보아(`table bridge opl_<태그> {}` — 있는 규칙은 그대로 둔다)
  되면 `node get`의 `status.firewall: true`를 알리고, 안 되면(nftables 없음, root 아님) 경고와 함께 `false`다.
  이 시험은 시작 때 한 번만 하므로, 실패했다면 nftables를 고친 뒤 **opld를 다시 시작해야** 방화벽이 켜진다.
  스케줄러는 그룹을 건 VM을 `true`인 노드에만 둔다. 위트니스와 macOS 노드는 방화벽이 없다.
- **업그레이드 먼저.** 보안 그룹을 VM NIC에 걸기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다.
  예전 opld는 `security_groups`가 있는 NIC를 읽지 못한다.
- **br_netfilter와 호스트 iptables.** 이 규칙은 브리지 단계에서 걸러 br_netfilter가 켜져 있어도 그대로 적용된다.
  다만 br_netfilter가 켜져 있고 `net.bridge.bridge-nf-call-iptables`가 1이면 브리지를 지나는 VM 트래픽이 호스트의
  iptables(`FORWARD`)도 지난다. Docker처럼 `FORWARD` 기본 정책을 DROP으로 두는 프로그램이 있으면 보안 그룹이 허용한
  VM 트래픽도 막힐 수 있다 — 그 sysctl을 0으로 두거나 iptables에서 VM 브리지를 허용한다.
- **한계.** 걸러내는 것은 L3·L4 주소·포트와 보낸 MAC이다. 보낸 IP 위조(MAC은 맞고 IP만 다른 것)와 ARP 위조는
  막지 않는다. 규칙에서 허용을 빼도 이미 맺은 연결은 끊기지 않는다(conntrack 항목이 끝날 때까지 간다). 다른 그룹을
  원격으로 가리키는 규칙, 로그·카운터, 속도 제한, 기본 정책 바꾸기는 없다.
````

`README.md`에서

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

을 다음으로 바꾼다:

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

`README.md`에서

```markdown
# root가 만든 빌드 결과가 ./target에 섞이지 않게 따로 둔다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opl-driver --features qemu-it,net-it   # tap·브리지·VLAN, 이름공간 사이 VXLAN
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test vxlan   # E2E: VXLAN 네트워크
# Linux, root: 실제 NFS 풀. nfs-kernel-server로 127.0.0.1에 rw,no_root_squash로 내보낸 디렉터리가 필요하다.
```

을 다음으로 바꾼다:

```markdown
# root가 만든 빌드 결과가 ./target에 섞이지 않게 따로 둔다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opl-driver --features qemu-it,net-it   # tap·브리지·VLAN, 이름공간 사이 VXLAN, nft 방화벽(nftables·python3 필요)
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test vxlan   # E2E: VXLAN 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test firewall  # E2E: VM 방화벽(nftables 필요)
# Linux, root: 실제 NFS 풀. nfs-kernel-server로 127.0.0.1에 rw,no_root_squash로 내보낸 디렉터리가 필요하다.
```

`crates/opld/Cargo.toml`에서

```toml
qemu-it = []
# VM NIC를 Linux 브리지에 잇는 E2E 테스트(qemu-it와 함께). Linux에서 root와 iproute2가 필요하다.
net-it = []
```

을 다음으로 바꾼다:

```toml
qemu-it = []
# VM NIC를 Linux 브리지에 잇는 E2E 테스트(qemu-it와 함께). Linux에서 root와 iproute2가 필요하다(방화벽
# E2E는 nftables도).
net-it = []
```

`crates/opld/tests/firewall.rs`를 만든다:

```rust
//! 계획 26 완료 기준: 보안 그룹을 건 NIC로 VM을 켜면 opld가 nftables 테이블 `bridge opl_<태그>`에 그
//! tap의 체인과 맵 원소를 넣고, 그룹 규칙을 바꾸면 곧바로 체인이 바뀐다. 그룹이 없는 NIC는 맵에 없다.
//! VM과 그룹을 지우면 테이블이 사라진다.
//! `cargo test -p opld --features qemu-it,net-it --test firewall`
//! Linux에서 root, iproute2, nftables, QEMU가 필요하다(KVM이 없으면 TCG).
#![cfg(all(feature = "qemu-it", feature = "net-it", target_os = "linux"))]

mod common;

use std::process::Command;

use common::{Daemon, eventually};
use opl_types::network::tap_name;
use serde_json::Value;
use uuid::Uuid;

/// 관리자가 미리 만든 브리지. 같은 호스트에서 병렬로 도는 다른 시험과 겹치지 않는 이름이다. 끝나면 지운다.
struct TestBridge(String);

impl TestBridge {
    fn new() -> Self {
        let name = format!("oplf{}", &Uuid::now_v7().simple().to_string()[22..]);
        for args in [
            vec!["link", "add", &name, "type", "bridge"],
            vec!["link", "set", &name, "up"],
        ] {
            assert!(Command::new("ip").args(&args).status().unwrap().success());
        }
        Self(name)
    }
}

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
}

/// 시험이 실패해 남은 이 클러스터의 테이블을 지운다. 데몬보다 먼저 만들어 데몬(과 그 에이전트)이 멈춘
/// 뒤에 지워지게 한다.
struct Table(Option<String>);

impl Drop for Table {
    fn drop(&mut self) {
        if let Some(tag) = &self.0 {
            let table = format!("opl_{tag}");
            let _ = Command::new("nft")
                .args(["delete", "table", "bridge", &table])
                .status();
        }
    }
}

/// 클러스터 태그: CA 지문(`sha256:…`)의 16진수 앞 12자(opld의 `overlay::cluster_tag`와 같다)
fn tag_of(d: &Daemon) -> String {
    let fingerprint = d.ca_fingerprint.as_deref().unwrap();
    let hex = fingerprint.rsplit(':').next().unwrap();
    hex.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .take(12)
        .collect()
}

/// `nft -j list table bridge opl_<tag>`의 항목들(테이블이 없으면 None)
fn table(tag: &str) -> Option<Vec<Value>> {
    let out = Command::new("nft")
        .args(["-j", "list", "table", "bridge", &format!("opl_{tag}")])
        .output()
        .unwrap();
    out.status.success().then(|| {
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["nftables"].as_array().unwrap().clone()
    })
}

/// 테이블의 체인 `name`의 규칙들. 체인이 없으면 None.
fn chain_rules(items: &[Value], name: &str) -> Option<Vec<Value>> {
    items.iter().any(|i| i["chain"]["name"] == name).then(|| {
        items
            .iter()
            .filter(|i| i["rule"]["chain"] == name)
            .map(|i| i["rule"]["expr"].clone())
            .collect()
    })
}

/// 맵 `map`의 원소에 장치 `tap`이 있는지
fn map_has(items: &[Value], map: &str, tap: &str) -> bool {
    items
        .iter()
        .filter(|i| i["map"]["name"] == map)
        .any(|i| i["map"]["elem"].to_string().contains(&format!("\"{tap}\"")))
}

/// 들어오기 체인 `i_<tap>`에 `th dport port` 규칙이 있는지
fn allows_port(items: &[Value], tap: &str, port: u16) -> bool {
    chain_rules(items, &format!("i_{tap}")).is_some_and(|rules| {
        rules.iter().any(|expr| {
            let text = expr.to_string();
            text.contains("\"dport\"") && text.contains(&format!("\"right\":{port}}}"))
        })
    })
}

#[tokio::test]
async fn security_groups_become_nftables_rules_for_vm_taps() {
    let br = TestBridge::new();
    let mut leftover = Table(None);
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("p26").await;
    d.login().await;
    let tag = tag_of(&d);
    leftover.0 = Some(tag.clone());

    // root로 도는 Linux opld는 nft가 있으면 방화벽을 알린다.
    let node = d.ctl_json(&["node", "get", "node1"]).await;
    assert_eq!(node["status"]["firewall"], true, "{node}");

    d.ctl(&["network", "create", "lan", "--bridge", &br.0])
        .await
        .unwrap();
    d.ctl(&[
        "security-group",
        "create",
        "web",
        "--rule",
        "in accept tcp 22",
    ])
    .await
    .unwrap();
    let out = d
        .ctl(&[
            "vm",
            "create",
            "web",
            "--memory",
            "256",
            "--nic",
            "lan,sg=web",
            "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    let web_tap = tap_name(vm["id"].as_str().unwrap().parse().unwrap(), 0);
    // 그룹이 없는 NIC(켜지 않아도 된다 — 에이전트는 배치와 상관없이 모든 VM을 본다)
    d.ctl(&["vm", "create", "plain", "--memory", "256", "--nic", "lan"])
        .await
        .unwrap();
    let plain = d.ctl_json(&["vm", "get", "plain"]).await;
    let plain_tap = tap_name(plain["id"].as_str().unwrap().parse().unwrap(), 0);

    // 워커가 켜기 직전에 규칙을 넣었다.
    let items = table(&tag).expect("the firewall table");
    for chain in [format!("o_{web_tap}"), format!("i_{web_tap}")] {
        assert!(chain_rules(&items, &chain).is_some(), "{chain}: {items:?}");
    }
    assert!(map_has(&items, "out_nics", &web_tap), "{items:?}");
    assert!(map_has(&items, "in_nics", &web_tap), "{items:?}");
    assert!(allows_port(&items, &web_tap, 22), "{items:?}");
    assert!(!map_has(&items, "out_nics", &plain_tap), "{items:?}");
    assert!(!map_has(&items, "in_nics", &plain_tap), "{items:?}");
    assert!(chain_rules(&items, &format!("i_{plain_tap}")).is_none());

    // 규칙을 바꾸면(PATCH) 실행 중인 VM의 체인이 곧바로 바뀐다(60초 주기를 기다리지 않는다).
    d.ctl(&["security-group", "rule", "add", "web", "in accept tcp 8080"])
        .await
        .unwrap();
    eventually("the web chain to allow tcp 8080", || async {
        table(&tag).is_some_and(|items| allows_port(&items, &web_tap, 8080))
    })
    .await;
    d.ctl(&["security-group", "rule", "rm", "web", "1"])
        .await
        .unwrap();
    eventually("the web chain to drop the tcp 22 rule", || async {
        table(&tag).is_some_and(|items| {
            !allows_port(&items, &web_tap, 22) && allows_port(&items, &web_tap, 8080)
        })
    })
    .await;

    // 쓰고 있는 그룹은 지울 수 없다.
    let err = d
        .ctl(&["security-group", "delete", "web"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("security_group_in_use"), "{err}");

    let out = d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(out.contains("vm web: stop done"), "{out}");
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["vm", "delete", "plain"]).await.unwrap();
    d.ctl(&["security-group", "delete", "web"]).await.unwrap();
    // 방화벽 NIC가 없으면 테이블을 지운다.
    eventually("the firewall table to go away", || async {
        table(&tag).is_none()
    })
    .await;
    d.ctl(&["network", "delete", "lan"]).await.unwrap();
    d.stop();
}
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 6은 계획 25(VXLAN 네트워크: opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만들고 다른 노드의 underlay를 FDB에 넣음, 노드 underlay, tap MTU와 `host_mtu`, 완료)와 계획 26(VM 방화벽: 보안 그룹과 nftables bridge 규칙, 남음)으로 나눴다(`2026-11-03-network-sdn-design.md`). 하위 프로젝트 6은 진행 중이다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 6은 계획 25(VXLAN 네트워크: opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만들고 다른 노드의 underlay를 FDB에 넣음, 노드 underlay, tap MTU와 `host_mtu`, 완료)와 계획 26(VM 방화벽: 보안 그룹 `SecurityGroup`을 VM NIC에 걸고 노드마다 nftables `bridge opl_<태그>` 테이블로 tap에서 거름, 노드 능력 `firewall`과 스케줄러 조건, 켜기 직전 규칙 넣기, 완료)으로 나눴다(`2026-11-03-network-sdn-design.md`). 하위 프로젝트 6은 끝났다. IPAM·DHCP·라우팅·SNAT, EVPN·BGP, VXLAN 암호화, 그룹을 원격으로 가리키는 규칙, 방화벽 로그, IP·ARP 위조 막기는 넣지 않았다.
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

## 8. 클러스터 부트스트랩과 보안
```

을 다음으로 바꾼다:

```markdown

구현(계획 26): VM 방화벽은 NIC의 보안 그룹(`NicSpec.security_groups`)으로 켠다. 노드의 opld가 클러스터의 모든 방화벽 NIC를 nftables bridge family 테이블 `bridge opl_<클러스터 태그>` 하나로 만들어 `nft -f -`로 한 번에 바꾸고(tap 이름 → NIC 체인 판정 맵, `forward`·`input`·`output` 훅), 워커는 그런 VM을 켜기 직전에 규칙을 넣지 못하면 켜지 않는다. 노드는 `NodeStatus.firewall`로 능력을 알리고 스케줄러가 쓴다(`2026-11-03-network-sdn-design.md` §3).

## 8. 클러스터 부트스트랩과 보안
```

`docs/superpowers/specs/2026-11-03-network-sdn-design.md`에서

```markdown

넣지 않는 것: 물리 NIC·본딩·업링크 브리지 관리(설치 ISO, 하위 프로젝트 9에서), EVPN·BGP 제어 평면, IPAM·DHCP·SNAT(서브넷), VXLAN 암호화, 노드(호스트) 방화벽과 클러스터 전체 규칙, IP 위조 막기(IPAM이 필요), 원격으로 다른 보안 그룹 가리키기, 규칙 로그, 실행 중 VM의 NIC 추가·제거(지금처럼 다음 시작에 반영).
```

을 다음으로 바꾼다:

```markdown

넣지 않는 것: 물리 NIC·본딩·업링크 브리지 관리(설치 ISO, 하위 프로젝트 9에서), EVPN·BGP 제어 평면, IPAM·DHCP·SNAT(서브넷), VXLAN 암호화, 노드(호스트) 방화벽과 클러스터 전체 규칙, IP 위조 막기(IPAM이 필요), 원격으로 다른 보안 그룹 가리키기, 규칙 로그, 실행 중 VM의 NIC 추가·제거(실행 중에는 막는다 — 멈춘 뒤 바꾼다).
```

`docs/superpowers/specs/2026-11-03-network-sdn-design.md`에서

```markdown

### 3.2 NIC에 걸기
```

을 다음으로 바꾼다:

```markdown

구현(계획 26): `opl_types::security_group`의 `SecurityGroup = Resource<SecurityGroupSpec { description: Option<String>, rules: Vec<Rule> }, SecurityGroupStatus {}>`, `Kind::SecurityGroup`(`security-group`). `Rule { direction: Direction(In|Out), action: Action(Accept|Drop), protocol: Option<Protocol(Tcp|Udp|Icmp|Icmpv6)>, ports: Option<PortRange { start, end }>, remote: Option<Cidr { addr, prefix }> }`는 `deny_unknown_fields`, None은 빼고 쓴다. `PortRange`·`Cidr`는 `FromStr`·`Display`로 JSON 문자열(`"22"`·`"8000-8080"`, `"10.0.0.0/8"`)이고 OpenAPI도 문자열이다. `Cidr`는 새 크레이트 없이 직접 읽는다(접두사를 빼면 /32·/128, 접두사 밖 비트가 켜져 있으면 거절). 상수 `MAX_RULES = 200`, `MAX_DESCRIPTION = 200`, 오류 `SecurityGroupError::{LongDescription, TooManyRules, Rule(n, RuleError)}`(메시지 "rule 3: …", 번호는 1부터). API는 `crates/opl-api/src/security_groups.rs`(만들기·`PATCH`·지우기, 목록·읽기는 다른 리소스와 같은 매크로)이고 쓰기는 admin만이다. `PATCH` 본문 `SecurityGroupPatch { description, rules }`는 보낸 필드만 바꾸고 빈 `description`은 지운다. `If-Match`가 없으면 revision 충돌 때 다시 읽어 되풀이하고, 오래된 `If-Match`는 412다. watch 종류에 `security-group`이 있다.

### 3.2 NIC에 걸기
```

`docs/superpowers/specs/2026-11-03-network-sdn-design.md`에서

```markdown
- 실행 중에도 바꿀 수 있고 곧바로 적용된다(디스크·노드처럼 막지 않는다). 그룹 규칙을 바꿔도 곧바로 적용된다.
```

을 다음으로 바꾼다:

```markdown
- 실행 중에도 바꿀 수 있고 곧바로 적용된다(디스크·노드처럼 막지 않는다). 그룹 규칙을 바꿔도 곧바로 적용된다.

구현(계획 26): `NicSpec.security_groups`(serde 기본, 비면 뺀다), `MAX_NIC_SECURITY_GROUPS = 8`, `ValidationError::{TooManySecurityGroups(nic), DuplicateSecurityGroup(nic, name)}`, `VmSpec::has_firewall()`(그룹을 건 NIC가 있는지). 같은 그룹을 여러 NIC에 거는 것은 된다. 그룹이 있는지와 `user` 네트워크가 아닌지는 API(VM 만들기·바꾸기·되돌리기는 `prepare`의 `check_security_groups`, 복제는 `clone_vm`이 같은 함수로)가 본다(400 "security group \"db\" not found", "NIC 0: security groups cannot be used on the \"user\" network"). VM 바꾸기의 실행 중 금지 목록에 `security_groups`는 없다. 다만 방화벽 NIC가 있는(바꾸기 전이나 뒤) 실행 중 VM은 NIC의 그룹 말고는 바꿀 수 없고(409 `vm_running` "cannot change the NICs of VM … while it is running; only security groups can change"), 방화벽 NIC가 없어도 실행 중 VM의 NIC 개수·순번별 MAC은 바꿀 수 없다(409 `vm_running` "cannot change the number, order or MAC addresses of the NICs of VM … while it is running; stop it first" — 그룹 없이 순서를 바꾼 뒤 그룹을 거는 두 번의 바꾸기도 막는다) — 규칙은 spec의 NIC 순번·MAC으로 tap을 정하므로 도는 tap과 어긋나 열린 채가 되지 않게. 방화벽 NIC가 없는 실행 중 VM의 네트워크·모델 변경은 지금처럼 다음 시작에 반영된다. 웹 VM 폼은 실행 중이면 NIC 더하기·빼기와 MAC 편집을 막는다(`vmForm.nicsRunning`). 노드를 정한(`spec.node`) VM에 그룹을 걸면 그 노드의 `status.firewall`이 참이어야 한다(409 `firewall_unsupported` "node … has no firewall (nftables) for security groups", 계획 26 노드 단계). 그룹 지우기는 VM 쓰기와 같은 `vm_lock`을 쥐고 사용 검사와 지우기를 한다.
```

`docs/superpowers/specs/2026-11-03-network-sdn-design.md`에서

```markdown
- 노드 자신과의 트래픽(브리지에 노드 주소가 있을 때)도 같은 체인을 탄다(bridge `input`·`output` 훅).
```

을 다음으로 바꾼다:

```markdown
- 노드 자신과의 트래픽(브리지에 노드 주소가 있을 때)도 같은 체인을 탄다(bridge `input`·`output` 훅).

구현(계획 26): `opl_driver::firewall`의 `FirewallNic { tap, mac, rules }`와 순수 함수 `ruleset(tag, nics) -> String`. 글은 `table bridge opl_T {}` → `delete table bridge opl_T` → (방화벽 NIC가 있으면) 새 정의 순이고, 정의 안은 NIC 체인(`o_<tap>`·`i_<tap>`) → 판정 맵(`out_nics`·`in_nics`, `type ifname : verdict`) → 훅 체인(`forward`: `iifname vmap @out_nics`·`oifname vmap @in_nics`, `input`: `iifname vmap @out_nics`, `output`: `oifname vmap @in_nics`, 모두 priority 0, policy accept) 순이다. 나가기 체인은 `ether saddr != MAC drop` → `meta protocol arp return` → `ct state established,related return` → `ct state invalid drop` → `out` 규칙(허용은 `return`, 차단은 `drop`), 들어오기 체인은 ARP(`meta protocol arp`)·이웃 찾기·DHCP 응답 `accept` → established·invalid → `in` 규칙 → `drop`이다. 규칙 한 줄은 remote(`meta protocol ip ip saddr|daddr C`/`meta protocol ip6 ip6 …`, 접두사 0이면 그 주소 체계 전체 `meta protocol ip`/`ip6`) → `meta l4proto tcp|udp|icmp|ipv6-icmp` → `th dport N[-M]` → 판정이다. 주소 체계를 `ether type`(nft가 `ip daddr`에 저절로 붙이는 조건) 대신 `meta protocol`로 적는 것(ARP도)은 VLAN-aware 브리지에서 태그가 붙은 프레임의 `ether type`이 802.1Q로 보일 수 있어서다. tap 이름이 1~15자 `[a-z0-9]`가 아닌 NIC는 경고하고 건너뛰고, NIC는 tap 이름 순으로 정렬하며 같은 tap 이름이 또 나오면 첫 것만 쓰고 경고한다(맵 원소가 겹쳐 글 전체가 실패하지 않게). 단위 시험은 글 전체를 고정 기대 문자열로 본다. `net-it` `opl-driver/tests/firewall.rs`는 이름공간 "호스트"의 브리지에 tap 이름 모양 veth로 "VM" 이름공간 둘을 붙여 허용 포트·ICMP, 나가기 차단, 이웃 캐시를 비운 뒤 허용 목록 + 끝 `out drop`(ARP는 늘 통한다), MAC 위조 차단, 테이블 지우기, 잘못된 글이면 옛 규칙이 남는 것을 본다(TCP는 python3로 듣고 잇는다). VLAN-aware 브리지(VM tap은 VLAN 10 pvid untagged, 업링크 veth는 VLAN 10 tagged로 다른 이름공간의 `link.10`과 이음)에서 주소 하나·`0.0.0.0/0`의 나가기 차단, 이웃 캐시를 비운 뒤 허용 목록 + 끝 `out drop`과 주소·접두사의 들어오기 허용도 본다.
```

`docs/superpowers/specs/2026-11-03-network-sdn-design.md`에서

```markdown

### 3.5 API·oplctl·웹
```

을 다음으로 바꾼다:

```markdown

구현(계획 26, 실행기): `#[async_trait] Firewall::apply(&self, ruleset) -> Result<(), DriverError>`, `Nft { netns: Option<String> }`(`Nft::default()`, 시험용 `Nft::in_netns(name)`이 `ip netns exec NAME nft -f -`)는 글을 stdin으로 `nft -f -`에 넣고, 실패는 `DriverError::ToolFailed`(stderr가 비면 종료 상태, 계획 25의 `failure_text`). 노드 능력은 `nft --version`이 아니라 실제 적용으로 정한다(아래 노드·에이전트) — `nft --version`은 root가 아니어도 성공한다. CI linux-kvm 작업의 apt에 `nftables`를 더했다.

구현(계획 26, 노드·에이전트): `NodeStatus.firewall: bool`(false면 뺀다, OpenAPI 선택 필드)은 `register_node`가 시작할 때마다 `host.firewall.is_some()`으로 쓴다. `Host::detect_firewall(tag)`는 Linux에서 `Nft::default()`로 이 클러스터의 테이블을 만드는 글 `table bridge opl_<tag> {}`를 실제로 넣어 보아(있는 테이블·규칙은 그대로 — opld를 다시 시작해도 도는 VM이 열리지 않게) 되면 `Host.firewall = Some(…)`, 안 되면(nftables 없음, root 아님) 경고와 `None`이다. 스케줄러 `node_fits`는 `has_firewall()`인 VM에 `firewall`인 노드만 받는다("node … does not have a firewall (nftables) for the VM's security groups", `NoNode` 메시지에 "has a firewall (nftables) for VMs with security groups"). HA 복구와 마이그레이션 대상 검사도 `node_fits`라 같다. `crates/opld/src/firewall.rs`: 순수 `firewall_nics(vms, groups)`(배치와 상관없이 모든 VM, MAC 없는 NIC는 건너뜀, 없는 그룹은 빈 규칙이고 그 NIC의 규칙 끝에 `out drop`을 더해 들어오기·나가기를 모두 막음), `FirewallRules`(위트니스가 아니고 `host.firewall`이 있을 때만; 지금 클러스터 상태로 글을 만들어 넣고 마지막 글을 기억하는 뮤텍스가 넣기를 직렬화)와 그 `FirewallGate` 구현(`sync`는 늘 다시 넣는다), `FirewallAgent::spawn`·`shutdown(left)`(member의 서비스, 종료 때 규칙은 남기고 `cluster leave`면 빈 글로 테이블을 지운다). 루프는 시작과 60초마다 강제로, VM·SecurityGroup watch(목록을 읽기 전에 구독, 끊기면 다시 구독)는 0.5초 모아 바뀐 글일 때만 넣고, 종료는 차례 사이에서만 본다. 실패는 경고하고 다음 차례에 같은 글이어도 다시 넣는다. reconciler는 `opl_reconciler::FirewallGate`(`async fn sync(&self) -> Result<(), String>`)와 `ReconcilerConfig.firewall: Option<Arc<dyn FirewallGate>>`를 받고, 워커는 `has_firewall()`인 VM의 `hv.start` 직전에 부른다: 게이트가 없으면 다시 시도하지 않는 시작 실패("failed to start: firewall: this node has no firewall (nftables)"), `sync` 실패는 일시적 시작 실패("failed to start: firewall: …", 재시작 백오프 규칙 그대로)다. 살아 있는 VM에 다시 붙는 `reattach`와 멈춘 채 남은 VM을 이어서 실행하는 `resume_left_stopped`도 그 전에(`Worker::sync_firewall`) 부르고, 게이트가 없거나 실패하면 이어서 실행하지 않고 다음 차례에 다시 본다("VM is unresponsive: firewall: …"). 마이그레이션 받기는 opl-api `IncomingMigrations::with_firewall(gate)`가 받는 QEMU를 띄우기 전에 같은 확인을 한다.

### 3.5 API·oplctl·웹
```

`docs/superpowers/specs/2026-11-03-network-sdn-design.md`에서

```markdown
- 웹: 보안 그룹 목록·만들기·규칙 편집 페이지, VM NIC 편집에서 그룹 고르기.
```

을 다음으로 바꾼다:

```markdown
- 웹: 보안 그룹 목록·만들기·규칙 편집 페이지, VM NIC 편집에서 그룹 고르기.

구현(계획 26): oplctl `security-group {list, get, create NAME [--description D] [--rule RULE]..., delete, rule add NAME RULE [--at N], rule rm NAME N}`(설계의 `show`는 다른 리소스처럼 `get`이다; `list` 열 `NAME RULES DESCRIPTION`; `rule add/rm`은 읽은 revision을 `If-Match`로 `PATCH`한다). RULE은 oplctl이 JSON 규칙으로 바꾸고 값 검사는 API가 한다. VM은 `vm create --nic NETWORK[,sg=A+B]`와 `vm nic-sg VM INDEX [GROUP]...`(NIC 번호 0부터, 그룹이 없으면 비운다 — `vm set`에 NIC 지정이 없어 따로 두었다)이다. 웹은 `/security-groups`(목록·만들기·지우기)와 `/security-groups/:name`(규칙 표: 방향·동작·프로토콜·포트·원격 CIDR, 위·아래 순서 바꾸기, 저장은 `If-Match` `PATCH`), 사이드바 메뉴, VM NIC 편집의 그룹 체크박스(8개까지), `security_group_in_use` 문구(ko·en)다. 노드 목록에 `firewall`은 보이지 않는다(`node get`의 `status.firewall`). E2E `crates/opld/tests/firewall.rs`(`qemu-it,net-it`, Linux root): opld 하나에서 `node get`이 `firewall: true`, 보안 그룹 NIC로 VM을 켜면 `nft -j list table bridge opl_<태그>`에 그 tap의 `o_`·`i_` 체인과 두 맵 원소가 있고 그룹이 없는 VM의 tap은 없다. 규칙을 더하고 빼면(`PATCH`) 몇 초 안에 들어오기 체인이 바뀌고, VM과 그룹을 지우면 테이블이 사라진다. 시험 브리지 이름은 무작위이고, 남은 테이블은 데몬이 멈춘 뒤 지운다. CI는 이미 `opld/net-it`를 돌리므로 그 안에서 돈다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 938개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add README.md crates docs
git commit -m "test(opld): VM 방화벽 E2E와 문서 — README 방화벽 절, 설계 "구현(계획 26)" 메모, 하위 프로젝트 6 완료"
```

---

## 완료 기준 (계획 26)

- 보안 그룹을 만들어 NIC에 걸면 그 VM tap의 트래픽이 규칙대로 걸러지고(들어오기 기본 차단, 나가기 기본 허용, 이미 맺은 연결·ARP·ND·DHCP 허용, MAC 위조 차단), 그룹·규칙을 바꾸면 도는 VM에 곧 반영된다.
- 방화벽 NIC VM은 규칙을 넣지 못하면 켜지지 않고(시작·다시 붙이기·다시 돌리기·마이그레이션 받기), 방화벽 없는 노드에는 배치되지 않는다.
- Linux CI(`linux-kvm`)에서 `opl-driver` `net-it`의 `firewall`(이름공간 트래픽, VLAN 브리지)과 `opld`의 `firewall` E2E가 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **`nft flush ruleset`(예: `nftables.service` 재적재) 뒤 최대 60초** 동안 도는 VM이 열린다(에이전트의 60초 다시 넣기까지).
- **적용 실패는 로그로만 알린다.** 실패하면 커널에는 이전 규칙이 남는다.
- **시작 때 nft 시험이 실패한 노드는 opld를 재시작해야** 방화벽이 켜진다.
- **IP 위조는 막지 않는다**(MAC만). 원격으로 다른 보안 그룹을 가리킬 수 없다.
- **업그레이드 먼저:** 옛 opld는 `security_groups`가 있는 NIC를 읽지 못하므로 모든 노드를 올린 뒤 그룹을 건다.
- 그 밖의 제한은 설계 문서 §3·§4와 README 방화벽 절에 있다.

## 실행 후 기록 (2026-11-03)

6개 Task는 계획대로 적용했고, Task마다 검증된 단계와 트리가 같았다(생성 파일 `schema.d.ts` 포함). Task 4의 첫 적용은 `crates/opl-api/src/incoming.rs`의 찾을 블록 하나가 두 번 나와 멈췄다 — 뒤 줄(`let c = &self.config;`)을 더해 한 번만 나오게 이 문서를 고치고, 반쯤 적용된 파일을 되돌린 뒤 다시 적용했다. 계획 문서를 만들기 전에 스테이지 작업에서 리뷰를 세 차례 돌리고 CI에서 실제 Linux로 확인해 고쳤다(위 Task의 코드에 이미 들어 있다).

- 1차: 실행 중 VM의 NIC 순서·MAC이 바뀌면 규칙이 다른 tap에 붙어 열리던 것(방화벽 VM은 실행 중 그룹만 바꿈), VLAN 브리지에서 주소 규칙이 안 맞을 수 있던 것(`meta protocol`), 방화벽 없는 노드의 도는 VM에 그룹을 걸던 것(`firewall_unsupported`), 그룹 지우기 경쟁과 없는 그룹(양방향 차단), 복제의 그룹 검사, root가 아닌 opld가 방화벽 가능으로 보이던 것(시작 때 실제 적용 시험 — 기존 테이블은 건드리지 않음), 같은 tap 이름, 다시 붙이기 전 확인.
- 2차: 그룹 없이 NIC를 바꾼 뒤 그룹을 거는 두 번 PUT으로 1차 규칙을 비켜 가던 것(실행 중 VM은 NIC 개수·순서·MAC을 바꿀 수 없음), VLAN 브리지의 ARP(`meta protocol arp`), 멈춘 VM을 다시 돌리기 전 확인.
- 3차: 남은 중요한 문제 없음.
- CI가 찾은 것: 새 러너 이미지에서 iSCSI by-path 링크가 `udevadm settle` 뒤 늦게 생겨 준비 단계가 실패(기다리기 추가, nftables와 무관함을 진단으로 확인), VLAN 시험의 인터페이스 이름 `link`가 `ip` 낱말과 겹친 시험 오류. 이미 알려진 `running_vm_snapshots_revert_resize_and_delete`의 QMP 시간 초과가 한 번 나 다시 돌렸다.

테스트: macOS `cargo test --workspace` 938개, 웹 Vitest 351개, `fmt`·`clippy -D warnings`(모든 기능). Linux CI(`linux-kvm`)에서 `opl-driver` `net-it` `firewall`(이름공간 트래픽: 허용·차단 TCP, ping, 나가기 차단, MAC 위조, 허용 목록 + 끝 drop의 ARP, 곧바로 반영, 잘못된 글, VLAN 브리지)과 `opld` `firewall` E2E가 통과했다.

## 다음 계획으로 넘길 사항

- 하위 프로젝트 6(네트워크·SDN)을 마쳤다. 다음은 처음 분해(`2026-10-20-opl-node-core-design.md` §1.1)의 하위 프로젝트 7(백업)이다.
- 남은 방화벽 과제: `user` 네트워크 NIC를 실행 중 브리지로 바꾼 뒤 그룹을 걸면 API는 그룹을 보이지만 도는 NIC(NAT)는 거르지 않는다(노출은 없음 — 실행 중 network 변경도 막을지), 마이그레이션 원본 다시 돌리기 경로의 확인(지금은 원본 규칙이 남아 있어 안전 — 주석), 시작 때 nft 시험이 실패한 노드의 재시험, 적용 실패를 노드 상태로 알리기, 노드 목록의 방화벽 표시, IP 위조 막기, 원격 그룹 참조, 규칙 로그, IPv6·호스트 주소 트래픽 시험.
