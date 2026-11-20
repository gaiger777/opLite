//! SDN NIC의 보안 그룹 → OVN `Port_Group`·`ACL`(계획 66 §11). 순수 부분: 그룹 목록마다 Port_Group 하나([`port_group`]),
//! 규칙 하나의 match 글([`acl_match`]), 읽은 행과 견준 Port_Group 묶음([`plan_port_group`]), 네트워크 묶음에 넣는 포트
//! 멤버 연산([`membership_ops`]), 지울 Port_Group([`delete_port_group`]).
//!
//! **뜻**은 단순 NIC(nftables)와 같다: NIC의 그룹 순서 → 그룹 안 순서로 **처음 맞는 규칙**. NIC의 뜻은 그 NIC의 그룹
//! 목록이 정하므로([`NicPolicy::key`]) 같은 목록의 NIC는 Port_Group 하나에 든다. OVN은 한 층(tier) 안에서 가장 높은 맞는
//! ACL 하나로 판정하므로 펼친 규칙 i번째를 우선순위 `32000 − i`에 두면 처음 맞는 규칙이 이긴다.
//!
//! **실패하면 닫힘**(§11.3): Port_Group 묶음(ACL을 만들고 맞춤, `ports`는 건드리지 않음)이 네트워크 묶음보다 먼저 간다.
//! 네트워크 묶음은 포트를 넣거나 바꾸는 같은 트랜잭션에서 그 포트를 목록 Port_Group에 넣는데, 앞에 그 Port_Group이 있는지
//! `wait`한다 — Port_Group이 없으면(그 묶음이 실패) 트랜잭션이 통째로 실패해 포트가 생기지 않는다. ACL 없는 포트가 없다.
//! 쓰이지 않는 Port_Group은 맨 끝에 `ports`가 빈 때만 지운다(포트를 옮기는 네트워크 묶음이 실패했으면 옛 Port_Group이
//! 남아 그 포트를 계속 거른다).

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use opl_driver::ovn::ovsdb::{cond, map_value, named_uuid, set_value, uuid_value};
use opl_types::security_group::{Action, Direction, NicPolicy, Protocol, Rule, SecurityGroup};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::nb::CLUSTER_KEY;

/// 그룹 이름 목록(사람이 보는 칸, `Port_Group.external_ids`)
pub const SG_LIST_KEY: &str = "orbivisor:sg-list";
/// 규칙이 나온 그룹 id(`ACL.external_ids`)
pub const SG_KEY: &str = "orbivisor:sg";
/// 규칙의 자리(`ACL.external_ids`): 그룹 안 순번, 또는 `dhcp`·`default-in`·`default-out`·`missing`
pub const RULE_KEY: &str = "orbivisor:rule";

/// DHCP 응답 늘 허용(물리 망 DHCP 서버 — OVN 자기 응답은 34000이 먼저 보낸다)
pub const DHCP_PRIORITY: i64 = 32767;
/// 펼친 규칙 0번의 우선순위(i번째는 `RULE_TOP − i`, 8 × 200 = 1600개라도 30401 이상)
pub const RULE_TOP: i64 = 32000;
/// 들어오기 기본 막기·나가기 기본 보내기
pub const DEFAULT_PRIORITY: i64 = 1000;

/// 목록 Port_Group 하나(§11.2)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortGroupRow {
    pub name: String,
    pub external_ids: BTreeMap<String, String>,
    pub acls: Vec<AclRow>,
}

/// ACL 하나(층 0)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AclRow {
    pub priority: i64,
    /// `to-lport`(VM이 받음)·`from-lport`(VM이 보냄)
    pub direction: String,
    pub matches: String,
    pub action: String,
    pub external_ids: BTreeMap<String, String>,
}

/// 클러스터 `tag`의 그룹 목록 열쇠 `key`([`NicPolicy::key`])의 Port_Group 이름 `opl_<태그>_<h16>`. OVN 이름 문법
/// `[a-zA-Z_.][a-zA-Z_.0-9]*`에 맞다(태그는 16진수, 하이픈이 없다).
pub fn port_group_name(tag: &str, key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    format!("opl_{tag}_{}", &hex::encode(digest)[..16])
}

/// 규칙 하나의 ACL match 글. 사용자 글은 들어가지 않는다(모두 검증된 타입에서 만든다). `in`은 Port_Group 포트로 나가는
/// 패킷(`outport`, 원격 = 보낸 주소), `out`은 Port_Group 포트에서 들어오는 패킷(`inport`, 원격 = 받는 주소). 접두사 0이면
/// 그 주소 체계 전체, 프로토콜도 원격도 없으면 모든 IP.
pub fn acl_match(rule: &Rule, pg: &str) -> String {
    let mut parts = vec![match rule.direction {
        Direction::In => format!("outport == @{pg}"),
        Direction::Out => format!("inport == @{pg}"),
    }];
    if let Some(remote) = rule.remote {
        let field = match rule.direction {
            Direction::In => "src",
            Direction::Out => "dst",
        };
        parts.push(match (remote.addr, remote.prefix) {
            (IpAddr::V4(_), 0) => "ip4".to_string(),
            (IpAddr::V6(_), 0) => "ip6".to_string(),
            (IpAddr::V4(_), _) => format!("ip4.{field} == {remote}"),
            (IpAddr::V6(_), _) => format!("ip6.{field} == {remote}"),
        });
    }
    let l4 = match rule.protocol {
        Some(Protocol::Tcp) => Some("tcp"),
        Some(Protocol::Udp) => Some("udp"),
        Some(Protocol::Icmp) => Some("icmp4"),
        Some(Protocol::Icmpv6) => Some("icmp6"),
        None => None,
    };
    match l4 {
        Some(p) => parts.push(p.to_string()),
        None if rule.remote.is_none() => parts.push("ip".to_string()),
        None => {}
    }
    if let Some(ports) = rule.ports {
        let range = |p: &str| {
            if ports.start == ports.end {
                format!("{p}.dst == {}", ports.start)
            } else {
                format!("{p}.dst >= {} && {p}.dst <= {}", ports.start, ports.end)
            }
        };
        parts.push(match rule.protocol {
            Some(Protocol::Tcp) => range("tcp"),
            Some(Protocol::Udp) => range("udp"),
            // 검증이 막는다(포트는 tcp·udp만). nftables의 `th dport`처럼 둘 다 본다.
            _ => format!("(({}) || ({}))", range("tcp"), range("udp")),
        });
    }
    parts.join(" && ")
}

/// 그룹 목록 `policy`(이름 순서 그대로 펼친 것, `groups`는 클러스터의 그룹)의 Port_Group(§11.2 표): DHCP 응답 허용,
/// 펼친 규칙의 우선순위 띠, 들어오기 기본 막기, 나가기 기본 보내기. 없는 그룹이 낀 목록의 "나가기 막기"는 [`nic_policy`]가
/// 규칙 끝에 더한 것이 띠의 마지막 자리가 된다(nftables가 그 NIC 규칙 끝에 더하는 `out drop`과 같은 자리).
///
/// [`nic_policy`]: opl_types::security_group::nic_policy
pub fn port_group(tag: &str, policy: &NicPolicy, groups: &[SecurityGroup]) -> PortGroupRow {
    let name = port_group_name(tag, &policy.key());
    let ids = |rule: &str, sg: Option<String>| {
        let mut m = BTreeMap::from([
            (CLUSTER_KEY.to_string(), tag.to_string()),
            (RULE_KEY.to_string(), rule.to_string()),
        ]);
        if let Some(sg) = sg {
            m.insert(SG_KEY.to_string(), sg);
        }
        m
    };
    let acl =
        |priority: i64, direction: &str, matches: String, action: &str, external_ids| AclRow {
            priority,
            direction: direction.into(),
            matches,
            action: action.into(),
            external_ids,
        };
    // 펼친 규칙마다 (그룹 id, 그룹 안 순번) — nic_policy와 같은 차례(찾은 그룹 순서 → 그룹 안 순서, 끝에 없는 그룹 막기)
    let mut origin: Vec<(Option<String>, String)> = Vec::new();
    let mut names = Vec::new();
    for id in &policy.groups {
        if let Some(g) = groups.iter().find(|g| g.id == *id) {
            names.push(g.name.clone());
            origin.extend((0..g.spec.rules.len()).map(|i| (Some(id.to_string()), i.to_string())));
        }
    }
    names.extend(policy.missing.iter().cloned());
    let mut acls = vec![acl(
        DHCP_PRIORITY,
        "to-lport",
        format!(
            "outport == @{name} && udp && ((udp.src == 67 && udp.dst == 68) || (udp.src == 547 && udp.dst == 546))"
        ),
        "allow-related",
        ids("dhcp", None),
    )];
    for (i, rule) in policy.rules.iter().enumerate() {
        let (sg, at) = origin
            .get(i)
            .cloned()
            .unwrap_or((None, "missing".to_string()));
        acls.push(acl(
            RULE_TOP - i as i64,
            match rule.direction {
                Direction::In => "to-lport",
                Direction::Out => "from-lport",
            },
            acl_match(rule, &name),
            match (rule.direction, rule.action) {
                // 나가기 허용은 보낸 쪽 판정만 끝낸다 — 받는 VM 포트의 to-lport ACL이 따로 본다(nftables의 `return`).
                (_, Action::Accept) => "allow-related",
                (_, Action::Drop) => "drop",
            },
            ids(&at, sg),
        ));
    }
    acls.push(acl(
        DEFAULT_PRIORITY,
        "to-lport",
        format!("outport == @{name} && ip"),
        "drop",
        ids("default-in", None),
    ));
    // `NB_Global options:default_acl_drop`을 관리자가 켜도 같은 뜻이게 명시한다.
    acls.push(acl(
        DEFAULT_PRIORITY,
        "from-lport",
        format!("inport == @{name} && ip"),
        "allow-related",
        ids("default-out", None),
    ));
    PortGroupRow {
        external_ids: BTreeMap::from([
            (CLUSTER_KEY.to_string(), tag.to_string()),
            (SG_LIST_KEY.to_string(), names.join(",")),
        ]),
        name,
        acls,
    }
}

// ---- NB에서 읽은 것 ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualPortGroup {
    pub uuid: String,
    pub name: String,
    /// 논리 포트(약한 참조) uuid
    pub ports: Vec<String>,
    pub acls: Vec<String>,
    pub external_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualAcl {
    pub uuid: String,
    pub acl: AclRow,
}

/// 읽은 Port_Group·ACL의 찾아보기 표(한 차례에 한 번 만든다 — 포트 5천·ACL 수천에서 포트마다 훑지 않게)
pub struct PortGroupIndex<'a> {
    by_name: BTreeMap<&'a str, &'a ActualPortGroup>,
    /// 포트 uuid → 그 포트를 가진 우리 Port_Group
    members: BTreeMap<&'a str, Vec<&'a ActualPortGroup>>,
    acls: BTreeMap<&'a str, &'a AclRow>,
}

impl<'a> PortGroupIndex<'a> {
    pub fn new(groups: &'a [ActualPortGroup], acls: &'a [ActualAcl]) -> Self {
        let mut members: BTreeMap<&str, Vec<&ActualPortGroup>> = BTreeMap::new();
        for g in groups {
            for p in &g.ports {
                members.entry(p.as_str()).or_default().push(g);
            }
        }
        Self {
            by_name: groups.iter().map(|g| (g.name.as_str(), g)).collect(),
            members,
            acls: acls.iter().map(|a| (a.uuid.as_str(), &a.acl)).collect(),
        }
    }
}

fn by_uuid(uuid: &str) -> Value {
    json!([cond("_uuid", "==", uuid_value(uuid))])
}

fn acl_columns(a: &AclRow) -> Value {
    json!({"priority": a.priority, "direction": a.direction, "match": a.matches, "action": a.action,
           "external_ids": map_value(&a.external_ids)})
}

/// 우리 Port_Group(이름, 태그 조건)을 고르는 where
fn ours(tag: &str, name: &str) -> Value {
    json!([
        cond("name", "==", json!(name)),
        cond(
            "external_ids",
            "includes",
            map_value(&BTreeMap::from([(
                CLUSTER_KEY.to_string(),
                tag.to_string()
            )]))
        ),
    ])
}

/// 바라는 Port_Group `pg`(묶음 번호 `n`)를 읽은 것과 견준 연산(한 트랜잭션). 없으면 ACL과 함께 넣고(`ports`는 비움),
/// 있으면 우리 표시가 있는 ACL만 견주어 같은 것은 두고 다른 것만 넣고 뺀다(빠진 ACL은 참조를 잃고 OVSDB가 지운다 —
/// 관리자가 우리 Port_Group에 넣은 ACL은 둔다). 규칙이 바뀌어도 한 트랜잭션이라 중간에 열리는 순간이 없다.
pub fn plan_port_group(n: usize, pg: &PortGroupRow, have: &PortGroupIndex) -> Vec<Value> {
    let mut ops = Vec::new();
    let current = have.by_name.get(pg.name.as_str()).copied();
    let by_uuid_acl = &have.acls;
    let mut kept = BTreeSet::new();
    let mut add = Vec::new();
    for (i, want) in pg.acls.iter().enumerate() {
        let found = current.and_then(|c| {
            c.acls
                .iter()
                .find(|u| !kept.contains(u.as_str()) && by_uuid_acl.get(u.as_str()) == Some(&want))
        });
        match found {
            Some(u) => {
                kept.insert(u.as_str());
            }
            None => {
                let name = format!("g{n}_{i}");
                ops.push(json!({"op": "insert", "table": "ACL", "uuid-name": name, "row": acl_columns(want)}));
                add.push(named_uuid(&name));
            }
        }
    }
    match current {
        None => {
            ops.push(json!({"op": "insert", "table": "Port_Group", "row": {
            "name": pg.name, "external_ids": map_value(&pg.external_ids), "acls": set_value(add)}}))
        }
        Some(c) => {
            if c.external_ids != pg.external_ids {
                ops.push(
                    json!({"op": "update", "table": "Port_Group", "where": by_uuid(&c.uuid),
                    "row": {"external_ids": map_value(&pg.external_ids)}}),
                );
            }
            let remove: Vec<Value> = c
                .acls
                .iter()
                .filter(|u| by_uuid_acl.contains_key(u.as_str()) && !kept.contains(u.as_str()))
                .map(|u| uuid_value(u))
                .collect();
            let mut mutations = Vec::new();
            if !remove.is_empty() {
                mutations.push(json!(["acls", "delete", set_value(remove)]));
            }
            if !add.is_empty() {
                mutations.push(json!(["acls", "insert", set_value(add)]));
            }
            if !mutations.is_empty() {
                ops.push(
                    json!({"op": "mutate", "table": "Port_Group", "where": by_uuid(&c.uuid),
                    "mutations": mutations}),
                );
            }
        }
    }
    ops
}

/// 네트워크 묶음에 넣는 포트 하나의 멤버 연산: 포트 `port`(있는 행 uuid나 넣는 행의 named-uuid, 읽은 uuid `uuid`)를
/// 바라는 Port_Group `want`에 넣고 우리 다른 Port_Group에서 뺀다. `want`에 넣을 때는 그 Port_Group이 (우리 표시로) 있는지
/// 먼저 `wait`한다 — 없으면 트랜잭션 전체가 실패한다(포트를 만들거나 바꾸지 않는다, §11.3).
pub fn membership_ops(
    tag: &str,
    port: &Value,
    uuid: Option<&str>,
    want: Option<&str>,
    have: &PortGroupIndex,
) -> Vec<Value> {
    let mut ops = Vec::new();
    let member_of: &[&ActualPortGroup] = uuid
        .and_then(|u| have.members.get(u))
        .map_or(&[], Vec::as_slice);
    for g in member_of {
        if Some(g.name.as_str()) != want {
            ops.push(
                json!({"op": "mutate", "table": "Port_Group", "where": by_uuid(&g.uuid),
                "mutations": [["ports", "delete", set_value(vec![port.clone()])]]}),
            );
        }
    }
    if let Some(name) = want {
        let already = member_of.iter().any(|g| g.name == name);
        if !already {
            ops.push(
                json!({"op": "wait", "table": "Port_Group", "where": ours(tag, name),
                "columns": ["name"], "until": "==", "rows": [{"name": name}], "timeout": 0}),
            );
            ops.push(
                json!({"op": "mutate", "table": "Port_Group", "where": ours(tag, name),
                "mutations": [["ports", "insert", set_value(vec![port.clone()])]]}),
            );
        }
    }
    ops
}

/// 쓰이지 않는 우리 Port_Group `g` 지우기. 남의 포트(우리 포트가 아닌 uuid)가 들어 있으면 지우지 않는다(None). 우리
/// 포트가 남아 있으면 앞 네트워크 묶음이 옮길 것이므로 `ports`가 빈 때만 지운다 — 그 묶음이 실패했으면 지우지 않고(그
/// 포트를 계속 거른다) 다음 차례에 다시 본다. 그 ACL은 참조를 잃고 OVSDB가 지운다.
pub fn delete_port_group(g: &ActualPortGroup, our_ports: &BTreeSet<&str>) -> Option<Value> {
    if g.ports.iter().any(|p| !our_ports.contains(p.as_str())) {
        return None;
    }
    Some(json!({"op": "delete", "table": "Port_Group", "where": [
        cond("_uuid", "==", uuid_value(&g.uuid)),
        cond("ports", "==", set_value(Vec::new())),
    ]}))
}
