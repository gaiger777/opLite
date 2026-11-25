//! 계획 66 §11 SDN NIC 보안 그룹 → OVN Port_Group·ACL: match 글, 그룹 목록마다 Port_Group, 우선순위 띠, 가짜 NB 적용,
//! 실패하면 닫히는 트랜잭션 순서(§11.3), 다시 맞추면 연산 0개, 규모.

use std::time::{Duration, Instant};

use orbivisor_driver::ovn::NB_DB;
use orbivisor_driver::ovn::ovsdb::parse_uuids;
use orbivisor_types::network::Network;
use orbivisor_types::resource::Resource;
use orbivisor_types::security_group::{SecurityGroup, nic_policy};
use orbivisor_types::vm::Vm;
use serde_json::{Value, json};

use super::acl::*;
use super::nb::*;
use super::tests::{Nb, TAG, vm_on};

fn group(name: &str, rules: Value) -> SecurityGroup {
    Resource::new(
        name,
        serde_json::from_value(json!({ "rules": rules })).unwrap(),
    )
}

fn rule(v: Value) -> orbivisor_types::security_group::Rule {
    serde_json::from_value(v).unwrap()
}

fn tenant() -> Network {
    Resource::new(
        "tenant",
        serde_json::from_value(json!({"sdn": {}, "subnets": [{"cidr": "10.0.0.0/24"}]})).unwrap(),
    )
}

/// `tenant` 네트워크의 NIC 하나(MAC 끝 `n`, 주소 10.0.0.`n`)와 그룹 `groups`인 VM
fn vm(name: &str, n: u8, groups: &[&str]) -> Vm {
    vm_on(
        name,
        json!([{ "network": "tenant", "model": "virtio", "mac": format!("52:54:00:00:00:{n:02x}"),
                 "ips": [format!("10.0.0.{n}")], "security_groups": groups }]),
    )
}

fn want(vms: &[Vm], groups: &[SecurityGroup]) -> NbState {
    desired_all(&[tenant()], vms, &[], &[], &[], groups, TAG)
}

fn port_of<'a>(w: &'a NbState, vm: &Vm) -> &'a PortRow {
    w.switches
        .iter()
        .flat_map(|s| &s.ports)
        .find(|p| p.external_ids.get(VM_KEY) == Some(&vm.id.to_string()))
        .unwrap()
}

#[test]
fn rules_become_ovn_matches() {
    let m = |v: Value| acl_match(&rule(v), "pg");
    assert_eq!(
        m(json!({"direction": "in", "action": "accept"})),
        "outport == @pg && ip"
    );
    assert_eq!(
        m(json!({"direction": "out", "action": "drop"})),
        "inport == @pg && ip"
    );
    assert_eq!(
        m(
            json!({"direction": "in", "action": "accept", "protocol": "tcp", "ports": "22", "remote": "10.1.0.0/16"})
        ),
        "outport == @pg && ip4.src == 10.1.0.0/16 && tcp && tcp.dst == 22"
    );
    assert_eq!(
        m(
            json!({"direction": "out", "action": "accept", "protocol": "udp", "ports": "8000-8080", "remote": "2001:db8::/32"})
        ),
        "inport == @pg && ip6.dst == 2001:db8::/32 && udp && udp.dst >= 8000 && udp.dst <= 8080"
    );
    assert_eq!(
        m(json!({"direction": "in", "action": "accept", "protocol": "icmp"})),
        "outport == @pg && icmp4"
    );
    assert_eq!(
        m(json!({"direction": "in", "action": "accept", "protocol": "icmpv6", "remote": "::/0"})),
        "outport == @pg && ip6 && icmp6"
    );
    assert_eq!(
        m(json!({"direction": "in", "action": "drop", "remote": "0.0.0.0/0"})),
        "outport == @pg && ip4"
    );
    assert_eq!(
        m(json!({"direction": "in", "action": "drop", "remote": "192.0.2.7"})),
        "outport == @pg && ip4.src == 192.0.2.7/32"
    );
}

#[test]
fn port_group_names_fit_the_ovn_grammar() {
    let name = port_group_name(TAG, "a,b,missing:x");
    assert_eq!(name.len(), "opl_".len() + 12 + 1 + 16);
    assert!(name.starts_with("opl_abcdef012345_"));
    assert!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    );
    assert_ne!(name, port_group_name(TAG, "b,a,missing:x"));
}

/// 같은 목록의 NIC 둘은 Port_Group 하나, 순서가 다른 목록은 다른 Port_Group. 우선순위 띠는 펼친 차례대로 내려가고
/// 기본 막기·보내기와 DHCP 허용이 있다. 없는 그룹이 낀 목록은 띠 끝에 나가기 막기.
#[test]
fn each_distinct_group_list_is_one_port_group_with_a_priority_band() {
    let web = group(
        "web",
        json!([
            {"direction": "in", "action": "drop", "protocol": "tcp", "ports": "8080", "remote": "203.0.113.0/24"},
            {"direction": "in", "action": "accept", "protocol": "tcp", "ports": "8080"}
        ]),
    );
    let ssh = group(
        "ssh",
        json!([{"direction": "in", "action": "accept", "protocol": "tcp", "ports": "22"}]),
    );
    let groups = [web.clone(), ssh.clone()];
    let a = vm("a", 2, &["web", "ssh"]);
    let b = vm("b", 3, &["web", "ssh"]);
    let c = vm("c", 4, &["ssh", "web"]);
    let d = vm("d", 5, &["ssh", "gone"]);
    let e = vm("e", 6, &[]);
    let w = want(
        &[a.clone(), b.clone(), c.clone(), d.clone(), e.clone()],
        &groups,
    );
    assert_eq!(w.port_groups.len(), 3);
    let pa = port_of(&w, &a).port_group.clone().unwrap();
    assert_eq!(port_of(&w, &b).port_group.as_ref(), Some(&pa));
    let pc = port_of(&w, &c).port_group.clone().unwrap();
    assert_ne!(pa, pc);
    assert_eq!(port_of(&w, &e).port_group, None);
    let names: Vec<&str> = w.port_groups.iter().map(|g| g.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "port groups are in name order");

    let g = w.port_groups.iter().find(|g| g.name == pa).unwrap();
    assert_eq!(g.external_ids[SG_LIST_KEY], "web,ssh");
    assert_eq!(g.external_ids[CLUSTER_KEY], TAG);
    let rows: Vec<(i64, &str, &str, &str)> = g
        .acls
        .iter()
        .map(|a| {
            (
                a.priority,
                a.direction.as_str(),
                a.matches.as_str(),
                a.action.as_str(),
            )
        })
        .collect();
    let at = |m: &str| m.replace("PG", &pa);
    assert_eq!(
        rows,
        [
            (
                32767,
                "to-lport",
                at("outport == @PG && udp && ((udp.src == 67 && udp.dst == 68) || (udp.src == 547 && udp.dst == 546))").as_str(),
                "allow-related"
            ),
            (
                32000,
                "to-lport",
                at("outport == @PG && ip4.src == 203.0.113.0/24 && tcp && tcp.dst == 8080").as_str(),
                "drop"
            ),
            (
                31999,
                "to-lport",
                at("outport == @PG && tcp && tcp.dst == 8080").as_str(),
                "allow-related"
            ),
            (
                31998,
                "to-lport",
                at("outport == @PG && tcp && tcp.dst == 22").as_str(),
                "allow-related"
            ),
            (1000, "to-lport", at("outport == @PG && ip").as_str(), "drop"),
            (1000, "from-lport", at("inport == @PG && ip").as_str(), "allow-related"),
        ]
    );
    // 규칙이 나온 그룹과 자리
    assert_eq!(g.acls[1].external_ids[SG_KEY], web.id.to_string());
    assert_eq!(g.acls[2].external_ids[RULE_KEY], "1");
    assert_eq!(g.acls[3].external_ids[SG_KEY], ssh.id.to_string());
    assert_eq!(g.acls[3].external_ids[RULE_KEY], "0");

    // 순서가 다른 목록: ssh가 먼저
    let g = w.port_groups.iter().find(|g| g.name == pc).unwrap();
    assert_eq!(g.external_ids[SG_LIST_KEY], "ssh,web");
    assert_eq!(g.acls[1].priority, 32000);
    assert!(g.acls[1].matches.ends_with("tcp.dst == 22"));

    // 없는 그룹: 찾은 그룹의 규칙 뒤 나가기 막기
    let pd = port_of(&w, &d).port_group.clone().unwrap();
    let g = w.port_groups.iter().find(|g| g.name == pd).unwrap();
    assert_eq!(g.external_ids[SG_LIST_KEY], "ssh,gone");
    assert_eq!(
        pd,
        port_group_name(
            TAG,
            &nic_policy(&["ssh".into(), "gone".into()], &groups).key()
        )
    );
    let missing = &g.acls[2];
    assert_eq!(
        (
            missing.priority,
            missing.direction.as_str(),
            missing.action.as_str()
        ),
        (31999, "from-lport", "drop")
    );
    assert_eq!(missing.matches, format!("inport == @{pd} && ip"));
    assert_eq!(missing.external_ids[RULE_KEY], "missing");
}

/// 그룹을 건 SDN NIC가 없으면 Port_Group이 없고 포트는 계획 65 그대로다(그룹이 클러스터에 있어도).
#[test]
fn no_groups_on_sdn_nics_means_no_port_groups() {
    let groups = [group("web", json!([]))];
    let a = vm("a", 2, &[]);
    let net = tenant();
    let w = desired_all(
        std::slice::from_ref(&net),
        std::slice::from_ref(&a),
        &[],
        &[],
        &[],
        &groups,
        TAG,
    );
    assert!(w.port_groups.is_empty());
    assert_eq!(w, desired_routing(&[net], &[a], &[], &[], &[], TAG));
}

/// 묶음 차례: 목록 Port_Group → 네트워크 → … → 지울 Port_Group
#[tokio::test]
async fn port_groups_reach_the_nb_before_their_ports_and_follow_changes() {
    let nb = Nb::new().await;
    let ssh = group(
        "ssh",
        json!([{"direction": "in", "action": "accept", "protocol": "tcp", "ports": "22"}]),
    );
    let icmp = group(
        "icmp",
        json!([{"direction": "in", "action": "accept", "protocol": "icmp"}]),
    );
    let mut groups = vec![ssh, icmp];
    let mut a = vm("a", 2, &["icmp", "ssh"]);
    let b = vm("b", 3, &[]);
    let net = tenant();
    let world = |vms: &[Vm], groups: &[SecurityGroup]| {
        desired_all(std::slice::from_ref(&net), vms, &[], &[], &[], groups, TAG)
    };
    let w = world(&[a.clone(), b.clone()], &groups);
    let pg1 = port_of(&w, &a).port_group.clone().unwrap();
    let batches = plan(&w, &nb.read().await);
    let keys: Vec<&str> = batches.iter().map(|b| b.key.as_str()).collect();
    assert_eq!(keys, [pg1.as_str(), switch_name(net.id).as_str()]);
    // Port_Group 묶음은 포트를 건드리지 않는다. 네트워크 묶음은 Port_Group이 있는지 기다린 뒤 넣는다.
    assert!(
        !batches[0]
            .ops
            .iter()
            .any(|o| o.to_string().contains("ports")),
        "{:?}",
        batches[0].ops
    );
    let net_ops: Vec<&str> = batches[1]
        .ops
        .iter()
        .filter(|o| o["table"] == "Port_Group")
        .map(|o| o["op"].as_str().unwrap())
        .collect();
    assert_eq!(net_ops, ["wait", "mutate"]);

    assert_eq!(nb.sync(&w, "1:1").await, 2);
    let pgs = nb.rows("Port_Group");
    assert_eq!(pgs.len(), 1);
    assert_eq!(pgs[0]["name"], json!(pg1));
    let ports = nb.rows("Logical_Switch_Port");
    let uuid_of = |name: &str| {
        ports
            .iter()
            .find(|p| p["name"] == json!(name))
            .map(|p| parse_uuids(&p["_uuid"])[0].clone())
            .unwrap()
    };
    let pa = port_of(&w, &a).name.clone();
    assert_eq!(parse_uuids(&pgs[0]["ports"]), [uuid_of(&pa)]);
    assert_eq!(nb.rows("ACL").len(), 5);
    // 다시 맞추면 할 일이 없다.
    assert!(plan(&w, &nb.read().await).is_empty());

    // 관리자가 우리 Port_Group에 넣은 ACL(표시 없음)은 남는다.
    let foreign = nb.fake.with(|s| {
        s.insert(
            "ACL",
            json!({"priority": 5, "direction": "to-lport", "match": "1", "action": "allow"}),
        )
    });
    nb.fake.with(|s| {
        let t = s.tables.get_mut("Port_Group").unwrap();
        let row = t.values_mut().next().unwrap();
        let mut acls = parse_uuids(&row["acls"]);
        acls.push(foreign.clone());
        row.insert(
            "acls".into(),
            json!([
                "set",
                acls.iter().map(|u| json!(["uuid", u])).collect::<Vec<_>>()
            ]),
        );
    });
    assert!(plan(&w, &nb.read().await).is_empty());

    // 그룹의 규칙을 바꾸면 Port_Group 묶음 하나만(같은 목록 — 포트는 그대로, 한 트랜잭션)
    groups[0].spec.rules[0].ports = Some("2222".parse().unwrap());
    let w = world(&[a.clone(), b.clone()], &groups);
    let batches = plan(&w, &nb.read().await);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].key, pg1);
    nb.sync(&w, "1:2").await;
    assert!(plan(&w, &nb.read().await).is_empty());
    let acls = nb.rows("ACL");
    assert_eq!(acls.len(), 6, "5 ours + the foreign one");
    assert!(
        acls.iter()
            .any(|r| r["match"].as_str().unwrap().ends_with("tcp.dst == 2222"))
    );
    assert!(
        !acls
            .iter()
            .any(|r| r["match"].as_str().unwrap().ends_with("tcp.dst == 22"))
    );

    // 켜진 VM의 그룹 순서를 바꾸면: 새 Port_Group 묶음 → 네트워크 묶음 하나(옛 것에서 빼고 새 것에 넣음) → 옛 Port_Group
    // 지우기
    a.spec.nics[0].security_groups = vec!["ssh".into(), "icmp".into()];
    let w = world(&[a.clone(), b.clone()], &groups);
    let pg2 = port_of(&w, &a).port_group.clone().unwrap();
    assert_ne!(pg1, pg2);
    let batches = plan(&w, &nb.read().await);
    let keys: Vec<&str> = batches.iter().map(|b| b.key.as_str()).collect();
    assert_eq!(
        keys,
        [pg2.as_str(), switch_name(net.id).as_str(), pg1.as_str()]
    );
    let membership: Vec<(String, String)> = batches[1]
        .ops
        .iter()
        .filter(|o| o["table"] == "Port_Group")
        .map(|o| {
            (
                o["op"].as_str().unwrap().to_string(),
                o["mutations"][0][1].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(
        membership,
        [
            ("mutate".to_string(), "delete".to_string()),
            ("wait".to_string(), String::new()),
            ("mutate".to_string(), "insert".to_string())
        ]
    );
    // 옛 Port_Group은 비었을 때만 지운다.
    assert!(batches[2].ops[0].to_string().contains("\"ports\",\"==\""));
    nb.sync(&w, "1:3").await;
    let pgs = nb.rows("Port_Group");
    assert_eq!(pgs.len(), 1);
    assert_eq!(pgs[0]["name"], json!(pg2));
    assert_eq!(parse_uuids(&pgs[0]["ports"]), [uuid_of(&pa)]);
    // 옛 목록의 우리 ACL은 Port_Group과 함께 사라졌다(남의 ACL도 참조를 잃어 사라진다 — 뿌리가 아님).
    assert_eq!(nb.rows("ACL").len(), 5);
    assert!(plan(&w, &nb.read().await).is_empty());

    // 그룹을 모두 빼면 Port_Group에서 빠지고 Port_Group도 지운다. 포트는 그대로다.
    a.spec.nics[0].security_groups.clear();
    let w = world(&[a.clone(), b.clone()], &groups);
    nb.sync(&w, "1:4").await;
    assert!(nb.rows("Port_Group").is_empty());
    assert!(nb.rows("ACL").is_empty());
    assert_eq!(uuid_of(&pa), {
        let ports = nb.rows("Logical_Switch_Port");
        ports
            .iter()
            .find(|p| p["name"] == json!(pa))
            .map(|p| parse_uuids(&p["_uuid"])[0].clone())
            .unwrap()
    });
    assert!(plan(&w, &nb.read().await).is_empty());
}

/// 견준 묶음을 하나씩 쓰고(펜싱 포함) 실패한 묶음의 이름을 돌려준다.
async fn sync_lenient(nb: &Nb, want: &NbState, token: &str) -> Vec<String> {
    let have = nb.read().await;
    let mut global = have.global.clone();
    let mut failed = Vec::new();
    for b in plan(want, &have) {
        let (mut ops, ext) = fence_ops(global.as_ref(), TAG, token);
        ops.extend(b.ops);
        match nb.client.transact(NB_DB, ops).await {
            Ok(results) => {
                global = Some(Global {
                    uuid: global
                        .map(|g| g.uuid)
                        .unwrap_or_else(|| parse_uuids(&results[1]["uuid"])[0].clone()),
                    external_ids: ext,
                    nb_cfg: 0,
                    hv_cfg: 0,
                });
            }
            Err(e) => failed.push(format!("{}: {e}", b.key)),
        }
    }
    failed
}

/// §11.3 실패하면 닫힘: 목록 Port_Group을 만들지 못하면 그 목록을 쓰는 포트도 생기지 않는다(다른 네트워크는 간다). 켜진
/// VM의 목록을 바꾸다 새 Port_Group을 만들지 못하면 포트는 옛 Port_Group에 남아 계속 걸러지고, 옛 Port_Group도 지우지
/// 않는다.
#[tokio::test]
async fn a_failing_port_group_keeps_its_ports_out_or_on_the_old_rules() {
    let nb = Nb::new().await;
    let ssh = group(
        "ssh",
        json!([{"direction": "in", "action": "accept", "protocol": "tcp", "ports": "22"}]),
    );
    let web = group(
        "web",
        json!([{"direction": "in", "action": "accept", "protocol": "tcp", "ports": "80"}]),
    );
    let groups = [ssh, web];
    let net = tenant();
    let other: Network = Resource::new(
        "other",
        serde_json::from_value(json!({"sdn": {}, "subnets": [{"cidr": "10.1.0.0/24"}]})).unwrap(),
    );
    let mut a = vm("a", 2, &["ssh"]);
    let c = vm_on(
        "c",
        json!([{ "network": "other", "model": "virtio", "mac": "52:54:00:00:01:02", "ips": ["10.1.0.2"] }]),
    );
    let world = |vms: &[Vm]| {
        desired_all(
            &[net.clone(), other.clone()],
            vms,
            &[],
            &[],
            &[],
            &groups,
            TAG,
        )
    };
    let w = world(&[a.clone(), c.clone()]);
    let pg_ssh = port_of(&w, &a).port_group.clone().unwrap();
    // 관리자의 표시 없는 Port_Group이 그 이름을 쓰고 있다 → 우리 Port_Group 묶음이 실패한다.
    nb.fake
        .with(|s| s.insert("Port_Group", json!({"name": pg_ssh})));
    let failed = sync_lenient(&nb, &w, "1:1").await;
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(failed[0].starts_with(&pg_ssh), "{failed:?}");
    assert!(failed[1].starts_with(&switch_name(net.id)), "{failed:?}");
    // ACL 없는 포트가 생기지 않았고(스위치째 없음), 남의 Port_Group에 들지도 않았다. 다른 네트워크는 갔다.
    let ports = nb.rows("Logical_Switch_Port");
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0]["name"], json!(port_of(&w, &c).name));
    assert!(
        parse_uuids(&nb.rows("Port_Group")[0]["ports"]).is_empty(),
        "the squatter got no ports"
    );

    // 막던 것이 사라지면 다음 차례에 둘 다 된다.
    nb.fake
        .with(|s| s.delete_where("Port_Group", "name", &json!(pg_ssh)));
    assert!(sync_lenient(&nb, &w, "1:2").await.is_empty());
    assert!(plan(&w, &nb.read().await).is_empty());
    let pa = port_of(&w, &a).name.clone();
    let port_uuid = nb
        .rows("Logical_Switch_Port")
        .iter()
        .find(|p| p["name"] == json!(pa))
        .map(|p| parse_uuids(&p["_uuid"])[0].clone())
        .unwrap();

    // 목록을 [web]로 바꾸는데 새 Port_Group 이름을 남이 쓰고 있다.
    a.spec.nics[0].security_groups = vec!["web".into()];
    let w = world(&[a.clone(), c.clone()]);
    let pg_web = port_of(&w, &a).port_group.clone().unwrap();
    nb.fake
        .with(|s| s.insert("Port_Group", json!({"name": pg_web})));
    let failed = sync_lenient(&nb, &w, "1:3").await;
    assert_eq!(failed.len(), 2, "{failed:?}");
    // 포트는 옛 [ssh] Port_Group에 남아 그 ACL로 걸러지고, 옛 Port_Group은 지워지지 않았다.
    let have = nb.read().await;
    let old = have
        .port_groups
        .iter()
        .find(|g| g.name == pg_ssh)
        .expect("the old port group stays");
    assert_eq!(old.ports, std::slice::from_ref(&port_uuid));
    assert_eq!(old.acls.len(), 4);

    nb.fake
        .with(|s| s.delete_where("Port_Group", "name", &json!(pg_web)));
    assert!(sync_lenient(&nb, &w, "1:4").await.is_empty());
    let have = nb.read().await;
    assert_eq!(have.port_groups.len(), 1);
    assert_eq!(have.port_groups[0].name, pg_web);
    assert_eq!(have.port_groups[0].ports, [port_uuid]);
}

/// 규모(§11.2): 포트 5천(네트워크 50개), 목록 50개 × 규칙 50(ACL 2500 + 띠 밖 150). 다시 맞추는 읽기·견주기 시간을 적는다.
#[tokio::test]
async fn fifty_lists_of_fifty_rules_resync_quickly() {
    let nb = Nb::new().await;
    let groups: Vec<SecurityGroup> = (0..50)
        .map(|g| {
            group(
                &format!("g{g}"),
                Value::Array(
                    (0..50)
                        .map(|r| {
                            json!({"direction": "in", "action": "accept", "protocol": "tcp",
                                   "ports": format!("{}", 1000 + g * 50 + r)})
                        })
                        .collect(),
                ),
            )
        })
        .collect();
    let networks: Vec<Network> = (0..50)
        .map(|i| {
            Resource::new(
                format!("n{i}"),
                serde_json::from_value(
                    json!({"sdn": {}, "subnets": [{"cidr": format!("10.{i}.0.0/16")}]}),
                )
                .unwrap(),
            )
        })
        .collect();
    let vms: Vec<Vm> = (0..5000u32)
        .map(|j| {
            let i = j / 100;
            vm_on(
                &format!("v{j}"),
                json!([{ "network": format!("n{i}"), "model": "virtio",
                         "mac": format!("52:54:00:00:{:02x}:{:02x}", j / 256, j % 256),
                         "ips": [format!("10.{i}.0.{}", j % 100 + 2)],
                         "security_groups": [format!("g{}", j % 50)] }]),
            )
        })
        .collect();
    let started = Instant::now();
    let w = desired_all(&networks, &vms, &[], &[], &[], &groups, TAG);
    eprintln!("desired with 50 lists: {:?}", started.elapsed());
    assert_eq!(w.port_groups.len(), 50);
    assert_eq!(nb.sync(&w, "1:1").await, 100);
    assert_eq!(nb.rows("ACL").len(), 50 * 53);
    let started = Instant::now();
    let have = nb.read().await;
    let read = started.elapsed();
    let todo = plan(&w, &have);
    let took = started.elapsed();
    assert!(todo.is_empty());
    eprintln!("full resync of 5000 ports and 2650 ACLs: read {read:?}, read + diff {took:?}");
    // 읽기는 가짜 서버(디버그 빌드의 JSON)가 대부분이다. 견주기만은 2초 목표(계획 65)보다 훨씬 짧아야 한다.
    assert!(took - read < Duration::from_secs(2), "{took:?}");
    assert!(took < Duration::from_secs(30), "{took:?}");
    assert!(have.port_groups.iter().all(|g| g.ports.len() == 100));
}
