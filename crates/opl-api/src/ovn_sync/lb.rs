//! OvnSync의 부하 분산(계획 67 §7.2): LB 리소스의 프로토콜마다 NB `Load_Balancer` 행 하나(`opl-lb-<id>-<proto>`)와
//! 그 `Load_Balancer_Health_Check` 행(강한 참조 — 뿌리가 아님), 그리고 붙이기 — VIP 네트워크 라우터의
//! `Logical_Router.load_balancer`와 그 라우터의 **모든 인터페이스 스위치**의 `Logical_Switch.load_balancer`(둘 다 약한
//! 참조라 LB 행은 혼자 산다).
//!
//! 묶음: LB마다 한 묶음(행·건강 검사 행과, 새로 넣는 행을 이미 있는 스위치·라우터에 붙이기)이 네트워크 묶음보다 먼저
//! 간다. 이미 있는 LB 행의 붙이기·떼기는 스위치·라우터 묶음이 한다(읽은 uuid로). 새 LB와 새 스위치가 한 차례에 함께
//! 생기면 어느 쪽도 상대 uuid를 몰라 그 붙이기는 다음 차례다([`deferred`] — OvnSync가 곧바로 한 번 더 맞춘다). 바라지
//! 않는 LB의 행은 라우터 묶음 뒤에 지운다(약한 참조는 OVSDB가 비운다).

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};

use opl_driver::ovn::ovsdb::{map_value, named_uuid, set_value, string_set, uuid_value};
use opl_types::ipam;
use opl_types::load_balancer::{
    LbMember, LoadBalancer, MemberNic, health_rows, ip_port_mappings, nb_name, vips,
};
use opl_types::network::Network;
use opl_types::vm::{MacAddr, Vm};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::nb::{CLUSTER_KEY, ID_KEY, NAME_KEY, NbActual, NbState};

/// 바라는 `Load_Balancer` 행 하나(LB 하나의 프로토콜 하나)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LbRow {
    /// LB 리소스 id
    pub lb: Uuid,
    /// `opl-lb-<id>-<proto>`
    pub name: String,
    pub protocol: String,
    pub vips: BTreeMap<String, String>,
    pub ip_port_mappings: BTreeMap<String, String>,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
    /// `vip` 순
    pub health: Vec<HealthRow>,
}

/// 바라는 `Load_Balancer_Health_Check` 행 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthRow {
    pub vip: String,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

/// NB에서 읽은 우리 `Load_Balancer` 행
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualLb {
    pub uuid: String,
    pub name: String,
    pub protocol: Option<String>,
    pub vips: BTreeMap<String, String>,
    pub ip_port_mappings: BTreeMap<String, String>,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
    pub health_check: Vec<String>,
}

/// NB에서 읽은 우리 `Load_Balancer_Health_Check` 행
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualHealth {
    pub uuid: String,
    pub vip: String,
    pub options: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

/// LB 하나를 붙일 곳: VIP 네트워크를 가진 라우터와 그 인터페이스 네트워크들
pub(super) struct Attach<'a> {
    pub router: Uuid,
    /// 라우터의 인터페이스(오버레이 SDN 네트워크, id 순)
    pub interfaces: &'a [&'a Network],
    /// 라우터 바깥 공급자 네트워크 이름(있으면)
    pub external: Option<&'a str>,
}

/// LB `lb`의 바라는 행들(프로토콜 순). VIP가 없거나 VIP 네트워크가 라우터 인터페이스가 아니면(`attach`가 None) 없다 —
/// 붙일 곳이 없는 행은 쓰지 않는다(API가 막지만 라우터를 뗀 뒤 남은 LB 따위). 멤버 규칙(§7.1): NIC 멤버는 그 NIC가
/// 라우터 인터페이스 네트워크에 있고 그 서브넷의 IPv4가 있어야 하고, 주소 멤버는 인터페이스 서브넷 안이어야 한다 —
/// 아니면 빠진다(스펙은 그대로). 바깥 VIP는 라우터 바깥이 그 네트워크일 때만 줄이 생긴다.
pub(super) fn desired_rows(
    lb: &LoadBalancer,
    attach: Option<&Attach>,
    vms: &[Vm],
    tag: &str,
) -> Vec<LbRow> {
    let Some(attach) = attach else {
        return Vec::new();
    };
    if lb.spec.vip.is_none() {
        return Vec::new();
    }
    let mut spec = lb.spec.clone();
    if spec
        .external
        .as_ref()
        .is_some_and(|e| attach.external != Some(e.network.as_str()))
    {
        spec.external = None;
    }
    let in_router = |ip: Ipv4Addr| {
        attach
            .interfaces
            .iter()
            .any(|n| ipam::nic_subnet(&n.spec, IpAddr::V4(ip)).is_some())
    };
    for l in &mut spec.listeners {
        l.members
            .retain(|m: &LbMember| m.nic().is_some() || m.ip.is_some_and(in_router));
    }
    let resolve = |vm: Uuid, mac: MacAddr| -> Option<MemberNic> {
        let v = vms.iter().find(|v| v.id == vm && !v.spec.template)?;
        let nic = v.spec.nics.iter().find(|n| n.mac == Some(mac))?;
        let net = attach.interfaces.iter().find(|n| n.name == nic.network)?;
        let ip = nic.ips.iter().find_map(|ip| match ip {
            IpAddr::V4(v4) if ipam::nic_subnet(&net.spec, *ip).is_some() => Some(*v4),
            _ => None,
        })?;
        Some(MemberNic {
            ip,
            network: nic.network.clone(),
        })
    };
    let ours = |extra: &[(&str, String)]| -> BTreeMap<String, String> {
        let mut m = BTreeMap::from([(CLUSTER_KEY.to_string(), tag.to_string())]);
        for (k, v) in extra {
            m.insert((*k).to_string(), v.clone());
        }
        m
    };
    spec.protocols()
        .into_iter()
        .map(|proto| {
            let mut options = BTreeMap::from([("reject".to_string(), "true".to_string())]);
            if let Some(a) = spec.affinity_secs {
                options.insert("affinity_timeout".into(), a.to_string());
            }
            LbRow {
                lb: lb.id,
                name: nb_name(lb.id, proto),
                protocol: proto.as_str().into(),
                vips: vips(&spec, proto, resolve),
                ip_port_mappings: ip_port_mappings(&spec, proto, resolve),
                options,
                external_ids: ours(&[(ID_KEY, lb.id.to_string()), (NAME_KEY, lb.name.clone())]),
                health: health_rows(&spec, proto)
                    .into_iter()
                    .map(|(vip, options)| HealthRow {
                        vip,
                        options,
                        external_ids: ours(&[(ID_KEY, lb.id.to_string())]),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// 바라는 LB 행 이름 → 읽은 행 uuid(이미 있는 것만)
pub(super) fn existing_uuids(have: &NbActual) -> BTreeMap<&str, &str> {
    have.load_balancers
        .iter()
        .map(|l| (l.name.as_str(), l.uuid.as_str()))
        .collect()
}

/// 약한 참조 칸 `load_balancer`(스위치·라우터)를 바라는 LB 이름 `want`에 맞추는 (넣을 것, 뺄 것). 이미 있는 LB 행만
/// 본다 — 이번 차례에 새로 넣는 LB는 LB 묶음이 붙인다. 우리 것이 아닌 LB(관리자가 붙인 것)는 두고 뺀다.
pub(super) fn attach_changes(
    want: &[String],
    current: &[String],
    have: &NbActual,
) -> (Vec<Value>, Vec<Value>) {
    let by_name = existing_uuids(have);
    let ours: BTreeSet<&str> = have
        .load_balancers
        .iter()
        .map(|l| l.uuid.as_str())
        .collect();
    let wanted: BTreeSet<&str> = want
        .iter()
        .filter_map(|n| by_name.get(n.as_str()).copied())
        .collect();
    let add = wanted
        .iter()
        .filter(|u| !current.iter().any(|c| c == *u))
        .map(|u| uuid_value(u))
        .collect();
    let remove = current
        .iter()
        .filter(|c| ours.contains(c.as_str()) && !wanted.contains(c.as_str()))
        .map(|c| uuid_value(c))
        .collect();
    (add, remove)
}

/// 새 스위치·라우터 행의 `load_balancer`(이미 있는 LB 행만)
pub(super) fn attach_insert(want: &[String], have: &NbActual) -> Vec<Value> {
    let by_name = existing_uuids(have);
    let mut out: Vec<&str> = want
        .iter()
        .filter_map(|n| by_name.get(n.as_str()).copied())
        .collect();
    out.sort();
    out.dedup();
    out.into_iter().map(uuid_value).collect()
}

fn by_uuid(uuid: &str) -> Value {
    json!([opl_driver::ovn::ovsdb::cond(
        "_uuid",
        "==",
        uuid_value(uuid)
    )])
}

fn lb_columns(r: &LbRow, health: Value) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("name".into(), json!(r.name));
    row.insert(
        "protocol".into(),
        string_set(std::slice::from_ref(&r.protocol)),
    );
    row.insert("vips".into(), map_value(&r.vips));
    row.insert("ip_port_mappings".into(), map_value(&r.ip_port_mappings));
    row.insert("options".into(), map_value(&r.options));
    row.insert("external_ids".into(), map_value(&r.external_ids));
    row.insert("health_check".into(), health);
    row
}

/// LB `lb`의 묶음(한 트랜잭션): 바라는 행 `rows`(이 LB의 것)를 넣거나 바꾸고, 그 건강 검사 행을 `vip`으로 짝지어
/// 맞추고, 바라지 않는 프로토콜 행을 지운다. 새로 넣는 행은 바라는 스위치·라우터(`want`) 가운데 이미 있는 것에 곧바로
/// 붙인다(이미 있는 행의 붙이기는 그 스위치·라우터 묶음이 한다).
pub(super) fn plan_lb(
    n: usize,
    lb: Uuid,
    rows: &[&LbRow],
    want: &NbState,
    have: &NbActual,
) -> Vec<Value> {
    let id = lb.to_string();
    let mut ops = Vec::new();
    let mine: Vec<&ActualLb> = have
        .load_balancers
        .iter()
        .filter(|l| l.external_ids.get(ID_KEY) == Some(&id))
        .collect();
    let mut kept = BTreeSet::new();
    for (i, r) in rows.iter().enumerate() {
        let current = mine
            .iter()
            .find(|a| a.name == r.name && !kept.contains(a.uuid.as_str()));
        match current {
            Some(a) => {
                kept.insert(a.uuid.as_str());
                let checks: Vec<&ActualHealth> = have
                    .health_checks
                    .iter()
                    .filter(|h| a.health_check.contains(&h.uuid))
                    .collect();
                let (mut kept_hc, mut add) = (BTreeSet::new(), Vec::new());
                for (j, h) in r.health.iter().enumerate() {
                    match checks
                        .iter()
                        .find(|c| c.vip == h.vip && !kept_hc.contains(c.uuid.as_str()))
                    {
                        Some(c) => {
                            kept_hc.insert(c.uuid.as_str());
                            if c.options != h.options || c.external_ids != h.external_ids {
                                ops.push(
                                    json!({"op": "update", "table": "Load_Balancer_Health_Check",
                                    "where": by_uuid(&c.uuid),
                                    "row": {"options": map_value(&h.options),
                                            "external_ids": map_value(&h.external_ids)}}),
                                );
                            }
                        }
                        None => {
                            let name = format!("hc{n}_{i}_{j}");
                            ops.push(health_insert(&name, h));
                            add.push(named_uuid(&name));
                        }
                    }
                }
                // 남의 건강 검사 행(우리 표시 없음)도 우리 LB에 있으면 뺀다 — 이 행의 검사는 우리가 정한다.
                let remove: Vec<Value> = a
                    .health_check
                    .iter()
                    .filter(|u| !kept_hc.contains(u.as_str()))
                    .map(|u| uuid_value(u))
                    .collect();
                let same = a.protocol.as_deref() == Some(r.protocol.as_str())
                    && a.vips == r.vips
                    && a.ip_port_mappings == r.ip_port_mappings
                    && a.options == r.options
                    && a.external_ids == r.external_ids;
                if !same {
                    let mut row = lb_columns(r, Value::Null);
                    row.remove("health_check");
                    ops.push(
                        json!({"op": "update", "table": "Load_Balancer", "where": by_uuid(&a.uuid),
                        "row": row}),
                    );
                }
                for (verb, set) in [("delete", remove), ("insert", add)] {
                    if !set.is_empty() {
                        ops.push(json!({"op": "mutate", "table": "Load_Balancer", "where": by_uuid(&a.uuid),
                            "mutations": [["health_check", verb, set_value(set)]]}));
                    }
                }
            }
            None => {
                let mut checks = Vec::new();
                for (j, h) in r.health.iter().enumerate() {
                    let name = format!("hc{n}_{i}_{j}");
                    ops.push(health_insert(&name, h));
                    checks.push(named_uuid(&name));
                }
                let name = format!("lb{n}_{i}");
                ops.push(
                    json!({"op": "insert", "table": "Load_Balancer", "uuid-name": name,
                    "row": lb_columns(r, set_value(checks))}),
                );
                // 이미 있는 스위치·라우터에 곧바로 붙인다.
                for s in want
                    .switches
                    .iter()
                    .filter(|s| s.load_balancers.contains(&r.name))
                {
                    if let Some(sw) = have.switches.iter().find(|a| a.name == s.name) {
                        ops.push(json!({"op": "mutate", "table": "Logical_Switch", "where": by_uuid(&sw.uuid),
                            "mutations": [["load_balancer", "insert", set_value(vec![named_uuid(&name)])]]}));
                    }
                }
                for w in want
                    .routers
                    .iter()
                    .filter(|w| w.load_balancers.contains(&r.name))
                {
                    if let Some(lr) = have.routers.iter().find(|a| a.name == w.name) {
                        ops.push(json!({"op": "mutate", "table": "Logical_Router", "where": by_uuid(&lr.uuid),
                            "mutations": [["load_balancer", "insert", set_value(vec![named_uuid(&name)])]]}));
                    }
                }
            }
        }
    }
    for a in mine.iter().filter(|a| !kept.contains(a.uuid.as_str())) {
        ops.push(json!({"op": "delete", "table": "Load_Balancer", "where": by_uuid(&a.uuid)}));
    }
    ops
}

fn health_insert(name: &str, h: &HealthRow) -> Value {
    json!({"op": "insert", "table": "Load_Balancer_Health_Check", "uuid-name": name,
        "row": {"vip": h.vip, "options": map_value(&h.options), "external_ids": map_value(&h.external_ids)}})
}

/// 바라지 않는 LB(리소스 id)마다 그 행을 지우는 묶음 (id, 연산)
pub(super) fn stale(want: &NbState, have: &NbActual) -> Vec<(String, Vec<Value>)> {
    let wanted: BTreeSet<String> = want
        .load_balancers
        .iter()
        .map(|r| r.lb.to_string())
        .collect();
    let mut by_id: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for a in &have.load_balancers {
        let id = a.external_ids.get(ID_KEY).map_or("", String::as_str);
        if !wanted.contains(id) {
            by_id
                .entry(id)
                .or_default()
                .push(json!({"op": "delete", "table": "Load_Balancer", "where": by_uuid(&a.uuid)}));
        }
    }
    by_id
        .into_iter()
        .map(|(id, ops)| (format!("opl-lb-{id}"), ops))
        .collect()
}

/// 이번 차례에 새로 넣는 LB 행이 새로 넣는 스위치·라우터에 붙어야 해서(둘 다 uuid를 모름) 다음 차례로 미룬 붙이기가
/// 있는지
pub fn deferred(want: &NbState, have: &NbActual) -> bool {
    let by_name = existing_uuids(have);
    let new_lb = |n: &String| !by_name.contains_key(n.as_str());
    want.switches.iter().any(|s| {
        !have.switches.iter().any(|a| a.name == s.name) && s.load_balancers.iter().any(new_lb)
    }) || want.routers.iter().any(|r| {
        !have.routers.iter().any(|a| a.name == r.name) && r.load_balancers.iter().any(new_lb)
    })
}
