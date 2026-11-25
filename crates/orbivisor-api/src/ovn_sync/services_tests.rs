//! 계획 67 OvnSync(서비스)의 바라는 행과 가짜 NB 적용: 부하 분산(프로토콜마다 `Load_Balancer`, 건강 검사·
//! `ip_port_mappings`, 라우터와 모든 인터페이스 스위치에 붙이기, 바깥 VIP, 빠지는 멤버), SDN NIC QoS 행(포트와 한
//! 트랜잭션), 다시 맞추면 연산 0개, 지우기, LB·QoS가 없으면 계획 66 연산 그대로.

use std::collections::BTreeMap;

use orbivisor_driver::ovn::ovsdb::{parse_map, parse_uuids};
use orbivisor_types::load_balancer::LoadBalancer;
use orbivisor_types::resource::Resource;
use orbivisor_types::vm::Vm;
use serde_json::{Value, json};

use super::lb::{self, HealthRow, LbRow};
use super::nb::*;
use super::qos::{PORT_KEY, QosNbRow};
use super::routing_tests::{World, running, world};
use super::tests::{Nb, TAG, ids, vm_on};

fn lb(name: &str, spec: Value) -> LoadBalancer {
    Resource::new(name, serde_json::from_value(spec).unwrap())
}

const MAC_A: &str = "52:54:00:00:00:0a";
const MAC_B: &str = "52:54:00:00:00:0b";

/// 멤버 VM 둘: a(t1, 10.10.0.10), b(t2, 10.20.0.11)
fn members(w: &World) -> (Vm, Vm) {
    let a = running(
        vm_on(
            "a",
            json!([{ "network": "t1", "model": "virtio", "mac": MAC_A, "ips": ["10.10.0.10"] }]),
        ),
        &w.nodes[0],
    );
    let b = vm_on(
        "b",
        json!([{ "network": "t2", "model": "virtio", "mac": MAC_B, "ips": ["10.20.0.11"] }]),
    );
    (a, b)
}

/// t1의 VIP 10.10.0.50, 바깥 VIP 203.0.113.130, tcp/80(건강 검사 — NIC 멤버 둘·라우터 안 주소 하나·라우터 밖 주소
/// 하나·없는 VM 하나)과 udp/53(a만)
fn web_lb(a: &Vm, b: &Vm) -> LoadBalancer {
    lb(
        "web",
        json!({
            "network": "t1", "vip": "10.10.0.50",
            "external": {"network": "ext", "ip": "203.0.113.130"},
            "affinity_secs": 300,
            "listeners": [
                {"protocol": "tcp", "port": 80, "health": {}, "members": [
                    {"vm": a.id, "mac": MAC_A, "port": 8080},
                    {"vm": b.id, "mac": MAC_B, "port": 8080},
                    {"ip": "10.20.0.99", "port": 80},
                    {"ip": "192.168.1.1", "port": 80},
                    {"vm": uuid::Uuid::now_v7(), "mac": "52:54:00:00:00:ff", "port": 80}
                ]},
                {"protocol": "udp", "port": 53, "members": [{"vm": a.id, "mac": MAC_A, "port": 53}]}
            ],
            "monitors": [{"network": "t1", "ip": "10.10.0.200"}, {"network": "t2", "ip": "10.20.0.200"}]
        }),
    )
}

/// 라우터 r1(t1·t2, 바깥 ext)이 있는 바라는 상태. r1은 시험마다 하나(id가 같아야 한다).
fn want(w: &World, vms: &[Vm], lbs: &[LoadBalancer]) -> NbState {
    desired_services(
        &w.networks,
        vms,
        &w.nodes,
        std::slice::from_ref(&w.r1),
        &[],
        &[],
        lbs,
        TAG,
    )
}

#[test]
fn a_load_balancer_becomes_a_row_per_protocol_attached_to_the_router_and_its_switches() {
    let w = world();
    let (a, b) = members(&w);
    let web = web_lb(&a, &b);
    let state = want(&w, &[a.clone(), b.clone()], std::slice::from_ref(&web));
    let tcp = orbivisor_types::load_balancer::nb_name(
        web.id,
        orbivisor_types::load_balancer::LbProtocol::Tcp,
    );
    let udp = orbivisor_types::load_balancer::nb_name(
        web.id,
        orbivisor_types::load_balancer::LbProtocol::Udp,
    );
    let backends = "10.10.0.10:8080,10.20.0.11:8080,10.20.0.99:80";
    let pa = port_name(a.id, MAC_A.parse().unwrap());
    let pb = port_name(b.id, MAC_B.parse().unwrap());
    let ours = |extra: &[(&str, &str)]| {
        let mut m = ids(&[(CLUSTER_KEY, TAG)]);
        m.extend(ids(extra));
        m
    };
    let health_options = ids(&[
        ("interval", "5"),
        ("timeout", "3"),
        ("success_count", "2"),
        ("failure_count", "3"),
    ]);
    let id = web.id.to_string();
    assert_eq!(
        state.load_balancers,
        vec![
            LbRow {
                lb: web.id,
                name: tcp.clone(),
                protocol: "tcp".into(),
                vips: ids(&[("10.10.0.50:80", backends), ("203.0.113.130:80", backends)]),
                ip_port_mappings: ids(&[
                    ("10.10.0.10", &format!("{pa}:10.10.0.200")),
                    ("10.20.0.11", &format!("{pb}:10.20.0.200")),
                ]),
                options: ids(&[("reject", "true"), ("affinity_timeout", "300")]),
                external_ids: ours(&[(ID_KEY, &id), (NAME_KEY, "web")]),
                health: ["10.10.0.50:80", "203.0.113.130:80"]
                    .iter()
                    .map(|vip| HealthRow {
                        vip: vip.to_string(),
                        options: health_options.clone(),
                        external_ids: ours(&[(ID_KEY, &id)]),
                    })
                    .collect(),
            },
            LbRow {
                lb: web.id,
                name: udp.clone(),
                protocol: "udp".into(),
                vips: ids(&[
                    ("10.10.0.50:53", "10.10.0.10:53"),
                    ("203.0.113.130:53", "10.10.0.10:53")
                ]),
                ip_port_mappings: BTreeMap::new(),
                options: ids(&[("reject", "true"), ("affinity_timeout", "300")]),
                external_ids: ours(&[(ID_KEY, &id), (NAME_KEY, "web")]),
                health: Vec::new(),
            },
        ]
    );
    let names = vec![tcp.clone(), udp.clone()];
    for s in &state.switches {
        let expect = if s.network == w.t1.id || s.network == w.t2.id {
            names.clone()
        } else {
            Vec::new()
        };
        assert_eq!(s.load_balancers, expect, "{}", s.name);
    }
    assert_eq!(state.routers[0].load_balancers, names);

    // 라우터 바깥이 다른 네트워크면 바깥 VIP 줄이 없다. VIP 네트워크가 라우터 인터페이스가 아니면 행이 없다.
    let mut other = web.clone();
    other.spec.external.as_mut().unwrap().network = "t3".into();
    let s = want(&w, &[a.clone(), b.clone()], std::slice::from_ref(&other));
    assert_eq!(
        s.load_balancers[0].vips,
        ids(&[("10.10.0.50:80", backends)])
    );
    let mut stray = web.clone();
    stray.spec.network = "t3".into();
    let s = want(&w, &[a, b], std::slice::from_ref(&stray));
    assert!(s.load_balancers.is_empty());
    assert!(s.switches.iter().all(|s| s.load_balancers.is_empty()));
}

/// SDN NIC의 QoS 행(§8.1): 보내기(속도·DSCP)는 `from-lport inport`, 받기는 `to-lport outport`, DSCP만이면 `&& ip`.
#[test]
fn nic_qos_becomes_qos_rows_on_its_switch() {
    let w = world();
    let mut a = vm_on(
        "a",
        json!([{ "network": "t1", "model": "virtio", "mac": MAC_A, "ips": ["10.10.0.10"],
                 "qos": {"out_mbps": 100, "in_mbps": 200, "dscp": 46} }]),
    );
    let b = vm_on(
        "b",
        json!([{ "network": "t2", "model": "virtio", "mac": MAC_B, "ips": ["10.20.0.11"],
                 "qos": {"dscp": 10} }]),
    );
    let state = want(&w, &[a.clone(), b.clone()], &[]);
    let pa = port_name(a.id, MAC_A.parse().unwrap());
    let pb = port_name(b.id, MAC_B.parse().unwrap());
    let row =
        |dir: &str, m: String, action: &[(&str, &str)], bw: &[(&str, &str)], port: &str| QosNbRow {
            direction: dir.into(),
            priority: 2000,
            r#match: m,
            action: ids(action),
            bandwidth: ids(bw),
            external_ids: ids(&[(CLUSTER_KEY, TAG), (PORT_KEY, port)]),
        };
    let t1 = state
        .switches
        .iter()
        .find(|s| s.network == w.t1.id)
        .unwrap();
    let mut expect = vec![
        row(
            "from-lport",
            format!("inport == \"{pa}\""),
            &[("dscp", "46")],
            &[("rate", "100000"), ("burst", "80000")],
            &pa,
        ),
        row(
            "to-lport",
            format!("outport == \"{pa}\""),
            &[],
            &[("rate", "200000"), ("burst", "160000")],
            &pa,
        ),
    ];
    expect.sort();
    assert_eq!(t1.qos, expect);
    let t2 = state
        .switches
        .iter()
        .find(|s| s.network == w.t2.id)
        .unwrap();
    assert_eq!(
        t2.qos,
        [row(
            "from-lport",
            format!("inport == \"{pb}\" && ip"),
            &[("dscp", "10")],
            &[],
            &pb
        )]
    );
    // 템플릿은 포트가 없으니 행도 없다.
    a.spec.template = true;
    let state = want(&w, &[a], &[]);
    assert!(state.switches.iter().all(|s| s.qos.is_empty()));
}

fn lb_rows(nb: &Nb) -> Vec<serde_json::Map<String, Value>> {
    nb.rows("Load_Balancer")
}

/// 스위치·라우터 행 `name`의 `load_balancer` uuid
fn attached(nb: &Nb, table: &str, name: &str) -> Vec<String> {
    let rows = nb.rows(table);
    let r = rows.iter().find(|r| r["name"] == name).unwrap();
    let mut v = parse_uuids(&r["load_balancer"]);
    v.sort();
    v
}

#[tokio::test]
async fn load_balancers_reach_the_nb_attach_and_go() {
    let nb = Nb::new().await;
    let w = world();
    let (a, b) = members(&w);
    let vms = [a.clone(), b.clone()];
    let mut web = web_lb(&a, &b);
    let lr = router_name(w.r1.id);
    let (s1, s2, s3) = (
        switch_name(w.t1.id),
        switch_name(w.t2.id),
        switch_name(w.ext.id),
    );

    // 처음 한 번에 스위치·라우터·LB가 모두 새로 생긴다: LB 행은 들어가지만 붙이기는 다음 차례(미룸)다.
    let first = want(&w, &vms, std::slice::from_ref(&web));
    assert!(lb::deferred(&first, &nb.read().await));
    nb.sync(&first, "1:1").await;
    assert_eq!(lb_rows(&nb).len(), 2);
    assert!(attached(&nb, "Logical_Switch", &s1).is_empty());
    assert!(!lb::deferred(&first, &nb.read().await));
    nb.sync(&first, "1:2").await;
    let uuids: Vec<String> = {
        let mut v: Vec<String> = lb_rows(&nb)
            .iter()
            .map(|r| parse_uuids(&r["_uuid"])[0].clone())
            .collect();
        v.sort();
        v
    };
    assert_eq!(attached(&nb, "Logical_Switch", &s1), uuids);
    assert_eq!(attached(&nb, "Logical_Switch", &s2), uuids);
    assert!(attached(&nb, "Logical_Switch", &s3).is_empty());
    assert_eq!(attached(&nb, "Logical_Router", &lr), uuids);
    assert_eq!(nb.rows("Load_Balancer_Health_Check").len(), 2);
    let tcp = lb_rows(&nb)
        .into_iter()
        .find(|r| r["protocol"] == "tcp")
        .unwrap();
    assert_eq!(parse_map(&tcp["options"])["reject"], "true");
    assert_eq!(parse_map(&tcp["ip_port_mappings"]).len(), 2);
    assert!(plan(&first, &nb.read().await).is_empty());

    // 관리자의 LB를 우리 스위치에 붙여도 둔다.
    let admin = nb
        .fake
        .with(|s| s.insert("Load_Balancer", json!({"name": "admin-lb"})));
    nb.fake.with(|s| {
        let sw = s.tables.get_mut("Logical_Switch").unwrap();
        let row = sw.values_mut().find(|r| r["name"] == s1.as_str()).unwrap();
        let mut set = orbivisor_driver::ovn::ovsdb::set_items(&row["load_balancer"]);
        set.push(json!(["uuid", admin]));
        row.insert("load_balancer".into(), json!(["set", set]));
    });

    // 멤버 VM이 사라지고 udp 리스너를 빼면: tcp 행만 바뀌고 udp 행은 지워지며 약한 참조도 사라진다.
    web.spec.listeners.pop();
    let second = want(&w, std::slice::from_ref(&a), std::slice::from_ref(&web));
    nb.sync(&second, "1:3").await;
    let rows = lb_rows(&nb);
    assert_eq!(rows.iter().filter(|r| r["name"] != "admin-lb").count(), 1);
    let tcp = rows.iter().find(|r| r["protocol"] == "tcp").unwrap();
    assert_eq!(
        parse_map(&tcp["vips"])["10.10.0.50:80"],
        "10.10.0.10:8080,10.20.0.99:80"
    );
    assert_eq!(attached(&nb, "Logical_Switch", &s1).len(), 2);
    assert!(attached(&nb, "Logical_Switch", &s1).contains(&admin));
    assert!(plan(&second, &nb.read().await).is_empty());

    // 건강 검사를 끄면 그 행이 사라진다(뿌리가 아니라 참조를 잃고).
    web.spec.listeners[0].health = None;
    let third = want(&w, std::slice::from_ref(&a), std::slice::from_ref(&web));
    nb.sync(&third, "1:4").await;
    assert!(nb.rows("Load_Balancer_Health_Check").is_empty());

    // LB를 지우면 우리 행만 사라지고 붙은 약한 참조도 비워진다(관리자의 것은 남음).
    let gone = want(&w, std::slice::from_ref(&a), &[]);
    nb.sync(&gone, "1:5").await;
    let rows = lb_rows(&nb);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "admin-lb");
    assert_eq!(
        attached(&nb, "Logical_Switch", &s1),
        std::slice::from_ref(&admin)
    );
    assert!(attached(&nb, "Logical_Router", &lr).is_empty());
    assert!(plan(&gone, &nb.read().await).is_empty());
}

/// 스위치·라우터가 이미 있으면 새 LB는 LB 묶음 하나에서 넣고 붙는다(한 트랜잭션). LB 묶음이 실패해도(여기서는 빼고
/// 씀) 네트워크·라우터 묶음은 가고, 다음 차례가 마저 붙인다.
#[tokio::test]
async fn a_new_load_balancer_attaches_in_its_own_batch() {
    let nb = Nb::new().await;
    let w = world();
    let (a, b) = members(&w);
    let vms = [a.clone(), b.clone()];
    nb.sync(&want(&w, &vms, &[]), "1:1").await;
    let web = web_lb(&a, &b);
    let state = want(&w, &vms, std::slice::from_ref(&web));
    let have = nb.read().await;
    assert!(!lb::deferred(&state, &have));
    let batches = plan(&state, &have);
    assert_eq!(
        batches.iter().map(|b| b.key.as_str()).collect::<Vec<_>>(),
        [format!("opl-lb-{}", web.id)]
    );
    let ops = &batches[0].ops;
    let attach = |table: &str| {
        ops.iter()
            .filter(|o| o["op"] == "mutate" && o["table"] == table)
            .count()
    };
    // 프로토콜 둘 × (스위치 둘, 라우터 하나)
    assert_eq!(attach("Logical_Switch"), 4);
    assert_eq!(attach("Logical_Router"), 2);

    // LB 묶음을 빼고 쓴다(실패한 것처럼) — 다른 묶음은 LB 없이 맞다.
    let mut partial = state.clone();
    partial.load_balancers.clear();
    assert!(plan(&partial, &have).is_empty());
    nb.sync(&state, "1:2").await;
    assert!(plan(&state, &nb.read().await).is_empty());
    let s1 = switch_name(w.t1.id);
    assert_eq!(attached(&nb, "Logical_Switch", &s1).len(), 2);
}

#[tokio::test]
async fn qos_rows_go_with_the_port_and_follow_changes() {
    let nb = Nb::new().await;
    let w = world();
    let vm = |qos: Value| {
        let mut v = vm_on(
            "a",
            json!([{ "network": "t1", "model": "virtio", "mac": MAC_A, "ips": ["10.10.0.10"], "qos": qos }]),
        );
        v.id = uuid::Uuid::from_u128(7);
        v
    };
    let a = vm(json!({"out_mbps": 100, "in_mbps": 200, "dscp": 46}));
    nb.sync(&want(&w, std::slice::from_ref(&a), &[]), "1:1")
        .await;
    // 포트를 넣는 트랜잭션에 QoS 행도 있다.
    let tx = nb.fake.with(|s| {
        s.transacts
            .iter()
            .find(|t| {
                t.as_array().unwrap().iter().any(|o| {
                    o["table"] == "Logical_Switch_Port"
                        && o["row"]["name"] == port_name(a.id, MAC_A.parse().unwrap())
                })
            })
            .cloned()
            .unwrap()
    });
    let qos_inserts = tx
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["op"] == "insert" && o["table"] == "QoS")
        .count();
    assert_eq!(qos_inserts, 2);
    let rows = nb.rows("QoS");
    assert_eq!(rows.len(), 2);
    let from = rows
        .iter()
        .find(|r| r["direction"] == "from-lport")
        .unwrap();
    assert_eq!(from["action"], json!(["map", [["dscp", 46]]]));
    assert_eq!(
        from["bandwidth"],
        json!(["map", [["burst", 80000], ["rate", 100000]]])
    );
    assert!(plan(&want(&w, std::slice::from_ref(&a), &[]), &nb.read().await).is_empty());

    // 관리자의 QoS 행은 둔다.
    let s1 = switch_name(w.t1.id);
    let admin = nb.fake.with(|s| {
        let u = s.insert(
            "QoS",
            json!({"priority": 1, "direction": "to-lport", "match": "1"}),
        );
        let sw = s.tables.get_mut("Logical_Switch").unwrap();
        let row = sw.values_mut().find(|r| r["name"] == s1.as_str()).unwrap();
        let mut set = orbivisor_driver::ovn::ovsdb::set_items(&row["qos_rules"]);
        set.push(json!(["uuid", u]));
        row.insert("qos_rules".into(), json!(["set", set]));
        u
    });
    // 보내기 속도만 바꾸면 그 행 하나만 바뀐다.
    let b = vm(json!({"out_mbps": 50, "in_mbps": 200, "dscp": 46}));
    let state = want(&w, std::slice::from_ref(&b), &[]);
    let batches = plan(&state, &nb.read().await);
    assert_eq!(batches.len(), 1);
    let inserts = batches[0]
        .ops
        .iter()
        .filter(|o| o["op"] == "insert")
        .count();
    assert_eq!(inserts, 1, "{:?}", batches[0].ops);
    nb.sync(&state, "1:2").await;
    assert_eq!(nb.rows("QoS").len(), 3);
    assert!(plan(&state, &nb.read().await).is_empty());
    // QoS를 빼면 우리 행만 사라진다.
    let mut c = b.clone();
    c.spec.nics[0].qos = None;
    let state = want(&w, std::slice::from_ref(&c), &[]);
    nb.sync(&state, "1:3").await;
    let rows = nb.rows("QoS");
    assert_eq!(rows.len(), 1);
    assert_eq!(parse_uuids(&rows[0]["_uuid"]), [admin]);
    assert!(plan(&state, &nb.read().await).is_empty());
}

/// LB·QoS가 없으면 계획 66과 같은 연산이다: 새 스위치·라우터 넣기에 계획 67 칸(`load_balancer`·`qos_rules`)이 없다.
#[tokio::test]
async fn without_services_the_plan66_operations_are_unchanged() {
    let nb = Nb::new().await;
    let w = world();
    let (a, b) = members(&w);
    let vms = [a, b];
    let old = desired_all(
        &w.networks,
        &vms,
        &w.nodes,
        std::slice::from_ref(&w.r1),
        &[],
        &[],
        TAG,
    );
    assert_eq!(old, want(&w, &vms, &[]));
    nb.sync(&old, "1:1").await;
    let inserted = nb.fake.with(|s| s.transacts.clone());
    for tx in inserted {
        for op in tx.as_array().unwrap() {
            if op["op"] == "insert"
                && (op["table"] == "Logical_Switch" || op["table"] == "Logical_Router")
            {
                assert!(op["row"].get("load_balancer").is_none(), "{op}");
                assert!(op["row"].get("qos_rules").is_none(), "{op}");
            }
        }
    }
    assert!(plan(&old, &nb.read().await).is_empty());
}
