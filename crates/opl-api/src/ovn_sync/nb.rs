//! OvnSync의 순수 부분(계획 65 §8.2~8.3): 바라는 NB 행([`NbState`]), NB에서 읽은 우리 행([`NbActual`]), 둘을 견준
//! 트랜잭션 연산([`plan`]), 펜싱 연산([`fence_ops`]), 네트워크 단위로 나누기([`batches`]).
//!
//! 우리 행은 `external_ids:orbivisor:cluster=<태그>`가 있는 행뿐이다. 그런 행만 읽고 바꾸고 지운다. 논리 스위치는
//! 이름(`opl-<네트워크 id>`), 포트는 이름(`Logical_Switch_Port.name`은 표에서 하나), DHCP_Options는 (네트워크 id,
//! CIDR)로 짝짓는다. 포트는 스위치의 강한 참조로만 살아 있으므로(뿌리 표가 아님) 포트를 넣는 연산과 스위치의 `ports`를
//! 바꾸는 연산은 한 트랜잭션에 둔다 — 그래서 나누는 단위가 네트워크다.

use std::collections::{BTreeMap, BTreeSet};

use opl_driver::ovn::ovsdb::{
    cond, map_value, named_uuid, parse_bool, parse_int, parse_map, parse_strings, parse_uuids,
    set_value, string_set, uuid_value,
};
use opl_types::network::Network;
use serde_json::{Map, Value, json};
use uuid::Uuid;

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
/// 한 트랜잭션의 연산 수가 이보다 많으면 네트워크 단위로 나눈다.
pub const MAX_OPS: usize = 2000;

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

/// 바라는 NB 행(스위치마다 그 포트와 DHCP 행). 같은 입력이면 같은 값이다(스위치는 네트워크 id 순).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NbState {
    pub switches: Vec<SwitchRow>,
}

/// 논리 스위치 하나(SDN 네트워크 하나)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchRow {
    pub network: Uuid,
    pub name: String,
    pub external_ids: BTreeMap<String, String>,
    pub ports: Vec<PortRow>,
    pub dhcp: Vec<DhcpRow>,
}

/// 논리 스위치 포트 하나(VM NIC 하나, 스테이지 4)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRow {
    pub name: String,
    pub addresses: Vec<String>,
    pub port_security: Vec<String>,
    /// 이 스위치의 DHCP 행(CIDR)
    pub dhcpv4: Option<String>,
    pub enabled: Option<bool>,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

/// DHCP_Options 하나(서브넷 하나, 스테이지 4)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpRow {
    pub cidr: String,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

/// 클러스터 `tag`의 바라는 NB 상태. 스테이지 3은 SDN 네트워크마다 논리 스위치이고, 포트·DHCP는 스테이지 4가 채운다.
pub fn desired(networks: &[Network], tag: &str) -> NbState {
    let mut switches: Vec<SwitchRow> = networks
        .iter()
        .filter(|n| n.spec.is_sdn())
        .map(|n| {
            let mut external_ids = BTreeMap::from([
                (CLUSTER_KEY.to_string(), tag.to_string()),
                (ID_KEY.to_string(), n.id.to_string()),
                (NAME_KEY.to_string(), n.name.clone()),
            ]);
            if let Some(folder) = n.spec.folder {
                external_ids.insert(FOLDER_KEY.into(), folder.to_string());
            }
            SwitchRow {
                network: n.id,
                name: switch_name(n.id),
                external_ids,
                ports: Vec::new(),
                dhcp: Vec::new(),
            }
        })
        .collect();
    switches.sort_by_key(|s| s.network);
    NbState { switches }
}

// ---- NB에서 읽은 것 ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualSwitch {
    pub uuid: String,
    pub name: String,
    pub external_ids: BTreeMap<String, String>,
    pub ports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualPort {
    pub uuid: String,
    pub name: String,
    pub addresses: Vec<String>,
    pub port_security: Vec<String>,
    pub dhcpv4: Option<String>,
    pub enabled: Option<bool>,
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

/// NB에서 읽은 우리 행과 `NB_Global`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NbActual {
    pub switches: Vec<ActualSwitch>,
    pub ports: Vec<ActualPort>,
    pub dhcp: Vec<ActualDhcp>,
    pub global: Option<Global>,
}

/// 우리 행을 읽는 연산들(한 트랜잭션): 스위치·포트·DHCP(태그 조건)와 `NB_Global`
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
               "columns": ["_uuid", "name", "external_ids", "ports"]}),
        json!({"op": "select", "table": "Logical_Switch_Port", "where": ours(),
               "columns": ["_uuid", "name", "addresses", "port_security", "dhcpv4_options",
                           "enabled", "options", "external_ids"]}),
        json!({"op": "select", "table": "DHCP_Options", "where": ours(),
               "columns": ["_uuid", "cidr", "options", "external_ids"]}),
        json!({"op": "select", "table": "NB_Global", "where": [],
               "columns": ["_uuid", "external_ids", "nb_cfg", "hv_cfg"]}),
    ]
}

fn rows(v: &Value) -> Vec<Map<String, Value>> {
    v["rows"]
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
        })
        .collect();
    let ports = rows(&results[1])
        .iter()
        .map(|r| ActualPort {
            uuid: uuid_of(r),
            name: text(r, "name"),
            addresses: sorted(parse_strings(&r["addresses"])),
            port_security: sorted(parse_strings(&r["port_security"])),
            dhcpv4: parse_uuids(&r["dhcpv4_options"]).into_iter().next(),
            enabled: parse_bool(&r["enabled"]),
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
    NbActual {
        switches,
        ports,
        dhcp,
        global,
    }
}

// ---- 견주기 ----

/// 네트워크 하나(또는 지울 스위치·DHCP 행 하나)의 연산 묶음. 묶음끼리는 named-uuid가 겹치지 않는다.
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

/// 포트 행의 칸(넣기·바꾸기). `dhcp`는 DHCP 행의 참조(있는 행의 uuid나 넣는 행의 named-uuid)다.
fn port_columns(p: &PortRow, dhcp: Option<Value>) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("name".into(), json!(p.name));
    row.insert("addresses".into(), string_set(&p.addresses));
    row.insert("port_security".into(), string_set(&p.port_security));
    row.insert(
        "dhcpv4_options".into(),
        set_value(dhcp.into_iter().collect()),
    );
    row.insert("enabled".into(), opt_bool(p.enabled));
    row.insert("options".into(), map_value(&p.options));
    row.insert("external_ids".into(), map_value(&p.external_ids));
    row
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
    let wanted_networks: BTreeSet<String> = want
        .switches
        .iter()
        .map(|s| s.network.to_string())
        .collect();

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
            match ports_by_name.get(p.name.as_str()) {
                Some(a) => {
                    let wanted_dhcp =
                        dhcp.as_ref()
                            .map(|d| match d.as_array().map(Vec::as_slice) {
                                Some([_, Value::String(u)]) if d[0] == "uuid" => Some(u.clone()),
                                _ => None,
                            });
                    let same_dhcp = match wanted_dhcp {
                        None => a.dhcpv4.is_none(),
                        Some(Some(u)) => a.dhcpv4.as_deref() == Some(u.as_str()),
                        // 새로 넣는 DHCP 행을 가리켜야 한다.
                        Some(None) => false,
                    };
                    let same = a.addresses == sorted(p.addresses.clone())
                        && a.port_security == sorted(p.port_security.clone())
                        && same_dhcp
                        && a.enabled == p.enabled
                        && a.options == p.options
                        && a.external_ids == p.external_ids;
                    if !same {
                        ops.push(json!({"op": "update", "table": "Logical_Switch_Port",
                            "where": by_uuid(&a.uuid), "row": port_columns(p, dhcp)}));
                    }
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
                        "row": port_columns(p, dhcp)}),
                    );
                    add.push(named_uuid(&name));
                }
            }
        }
        if let Some(c) = current {
            for u in &c.ports {
                // 우리 포트가 아닌 것(관리자가 넣은 포트)은 둔다.
                if our_ports.contains(u.as_str()) && !kept_ports.contains(u.as_str()) {
                    remove.push(uuid_value(u));
                }
            }
        }

        match current {
            None => {
                ops.push(
                    json!({"op": "insert", "table": "Logical_Switch", "uuid-name": format!("s{n}"),
                    "row": {"name": sw.name, "external_ids": map_value(&sw.external_ids),
                            "ports": set_value(add)}}),
                );
            }
            Some(c) => {
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

/// 묶음들을 트랜잭션으로: 모두 합쳐 [`MAX_OPS`] 이하면 하나, 넘으면 묶음마다 하나(한 묶음이 넘어도 나누지 않는다 —
/// 강한 참조가 한 번에 맞아야 한다).
pub fn batches(plan: Vec<Batch>) -> Vec<Batch> {
    let total: usize = plan.iter().map(|b| b.ops.len()).sum();
    if total <= MAX_OPS || plan.len() <= 1 {
        if plan.is_empty() {
            return plan;
        }
        let key = if plan.len() == 1 {
            plan[0].key.clone()
        } else {
            format!("{} networks", plan.len())
        };
        return vec![Batch {
            key,
            ops: plan.into_iter().flat_map(|b| b.ops).collect(),
        }];
    }
    plan
}

/// 트랜잭션 앞의 펜싱 연산: `NB_Global.external_ids`가 읽은 값 그대로인지 `wait`하고, 토큰 `token`을 쓰고 `nb_cfg`를
/// 1 올린다. `NB_Global`이 없으면(northd 전) 없음을 `wait`하고 넣는다. 돌려주는 맵은 쓴 뒤의 `external_ids`다.
pub fn fence_ops(
    global: Option<&Global>,
    tag: &str,
    token: &str,
) -> (Vec<Value>, BTreeMap<String, String>) {
    let mut ids = global.map(|g| g.external_ids.clone()).unwrap_or_default();
    ids.insert(fence_key(tag), token.into());
    let ops = match global {
        Some(g) => vec![
            json!({"op": "wait", "table": "NB_Global", "where": by_uuid(&g.uuid), "columns": ["external_ids"],
                   "until": "==", "rows": [{"external_ids": map_value(&g.external_ids)}], "timeout": 0}),
            json!({"op": "update", "table": "NB_Global", "where": by_uuid(&g.uuid),
                   "row": {"external_ids": map_value(&ids)}}),
            json!({"op": "mutate", "table": "NB_Global", "where": by_uuid(&g.uuid),
                   "mutations": [["nb_cfg", "+=", 1]]}),
        ],
        None => vec![
            json!({"op": "wait", "table": "NB_Global", "where": [], "columns": ["external_ids"],
                   "until": "==", "rows": [], "timeout": 0}),
            json!({"op": "insert", "table": "NB_Global",
                   "row": {"external_ids": map_value(&ids), "nb_cfg": 1}}),
        ],
    };
    (ops, ids)
}
