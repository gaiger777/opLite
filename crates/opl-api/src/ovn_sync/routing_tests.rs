//! 계획 66 OvnSync(라우팅)의 바라는 행과 가짜 NB 적용: 공급자 localnet, 라우터·포트·경로, 게이트웨이 순위, SNAT, 분산·중앙
//! 유동 IP, DHCP `router`·DHCPv6·RA, 남의 행, 다시 맞추면 연산 0개, 옮기기·지우기.

use std::collections::BTreeMap;

use opl_driver::ovn::ovsdb::{parse_int, parse_map, parse_strings, parse_uuids};
use opl_types::floating_ip::FloatingIp;
use opl_types::network::Network;
use opl_types::node::Node;
use opl_types::resource::Resource;
use opl_types::router::Router;
use opl_types::sdn::{TOP_GATEWAY_PRIORITY, gateway_ranking, router_mac};
use opl_types::vm::{MigrationStatus, PowerState, Vm, VmPhase};
use serde_json::{Value, json};
use uuid::Uuid;

use super::nb::*;
use super::tests::{Nb, TAG, ids, node_res, vm_on};

pub(super) fn net(name: &str, spec: Value) -> Network {
    Resource::new(name, serde_json::from_value(spec).unwrap())
}

/// 게이트웨이 우선순위 `gateway`, physnet `ready`(이름들), 유지보수 `maintenance`인 노드
pub(super) fn gw_node(name: &str, gateway: Option<u16>, ready: &[&str], maintenance: bool) -> Node {
    let mut n = node_res(name);
    n.spec.sdn = gateway.map(|g| opl_types::sdn::NodeSdnConfig {
        physnets: BTreeMap::new(),
        gateway: Some(g),
    });
    n.spec.maintenance = maintenance;
    n.status.sdn = Some(
        serde_json::from_value(json!({
            "chassis": "ready",
            "physnets": ready.iter().map(|p| json!({"name": p, "bridge": "vmbr1", "ready": true}))
                .collect::<Vec<_>>(),
        }))
        .unwrap(),
    );
    n
}

pub(super) fn router(name: &str, spec: Value) -> Router {
    Resource::new(name, serde_json::from_value(spec).unwrap())
}

fn fip(address: &str, target: Option<(&Vm, &str)>) -> FloatingIp {
    let target = target.map(|(vm, mac)| json!({"vm": vm.id, "mac": mac}));
    let mut spec = json!({"network": "ext", "address": address});
    if let Some(t) = target {
        spec["target"] = t;
    }
    Resource::new(
        address.replace('.', "-"),
        serde_json::from_value(spec).unwrap(),
    )
}

pub(super) fn running(mut vm: Vm, node: &Node) -> Vm {
    vm.status.phase = VmPhase::Running;
    vm.status.node = Some(node.id);
    vm.spec.power = PowerState::Running;
    vm
}

pub(super) struct World {
    pub(super) networks: Vec<Network>,
    pub(super) nodes: Vec<Node>,
    pub(super) t1: Network,
    pub(super) t2: Network,
    pub(super) ext: Network,
    /// [`r1`] 하나(계획 67 서비스 시험이 쓴다)
    pub(super) r1: Router,
}

pub(super) fn world() -> World {
    let t1 = net(
        "t1",
        json!({"sdn": {}, "subnets": [
            {"cidr": "10.10.0.0/24"},
            {"cidr": "2001:db8:10::/64", "ipv6_mode": "slaac", "dns_servers": ["2001:db8::53", "2001:db8::54"]}
        ]}),
    );
    let t2 = net(
        "t2",
        json!({"sdn": {}, "subnets": [
            {"cidr": "10.20.0.0/24"},
            {"cidr": "2001:db8:20::/64", "ipv6_mode": "dhcpv6-stateful", "dns_servers": ["2001:db8::53"]}
        ]}),
    );
    let t3 = net(
        "t3",
        json!({"sdn": {}, "subnets": [{"cidr": "10.30.0.0/24"}]}),
    );
    let ext = net(
        "ext",
        json!({"sdn": {"provider": {"physnet": "ext", "vlan": 100}}, "subnets": [
            {"cidr": "203.0.113.0/24", "gateway": "203.0.113.1",
             "floating_pools": [{"start": "203.0.113.100", "end": "203.0.113.199"}]},
            {"cidr": "2001:db8:ff::/64"}
        ]}),
    );
    let nodes = vec![
        gw_node("n1", Some(100), &["ext"], false),
        gw_node("n2", Some(200), &["ext"], false),
        gw_node("n3", Some(300), &["ext"], true),
        gw_node("n4", Some(500), &[], false),
        gw_node("n5", None, &["ext"], false),
    ];
    World {
        networks: vec![t1.clone(), t2.clone(), t3, ext.clone()],
        nodes,
        t1,
        t2,
        ext,
        r1: r1(),
    }
}

pub(super) fn r1() -> Router {
    router(
        "r1",
        json!({
            "interfaces": [{"network": "t2"}, {"network": "t1"}],
            "external": {"network": "ext", "ips": ["203.0.113.50", "2001:db8:ff::50"]},
            "routes": [{"destination": "10.99.0.0/16", "nexthop": "10.10.0.254"}]
        }),
    )
}

fn sw<'a>(want: &'a NbState, n: &Network) -> &'a SwitchRow {
    want.switches.iter().find(|s| s.network == n.id).unwrap()
}

fn port<'a>(s: &'a SwitchRow, name: &str) -> &'a PortRow {
    s.ports
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no port {name} in {:?}", s.ports))
}

#[test]
fn a_router_becomes_router_rows_with_a_ranked_gateway_group_and_snat() {
    let w = world();
    let r = r1();
    let want = desired_routing(
        &w.networks,
        &[],
        &w.nodes,
        std::slice::from_ref(&r),
        &[],
        TAG,
    );

    // 공급자 네트워크: localnet(physnet·VLAN)과 게이트웨이 포트의 짝. DHCP는 기본 꺼짐(공급자).
    let ext = sw(&want, &w.ext);
    let ln = port(ext, &localnet_port_name(w.ext.id));
    assert_eq!(ln.kind, "localnet");
    assert_eq!(ln.addresses, ["unknown"]);
    assert!(ln.port_security.is_empty());
    assert_eq!(ln.tag, Some(100));
    assert_eq!(ln.options, ids(&[("network_name", "ext")]));
    assert_eq!(ln.enabled, None);
    let rgw = port(ext, &gateway_switch_port_name(r.id));
    assert_eq!(rgw.kind, "router");
    assert_eq!(rgw.addresses, ["router"]);
    assert_eq!(
        rgw.options,
        ids(&[
            ("router-port", &gateway_port_name(r.id)),
            ("nat-addresses", "router")
        ])
    );
    assert!(ext.dhcp.is_empty(), "{:?}", ext.dhcp);

    // 인터페이스: 스위치 쪽 짝, DHCP `router`, DHCPv6(상태 있음)
    let t1 = sw(&want, &w.t1);
    let rsp = port(t1, &router_switch_port_name(w.t1.id));
    assert_eq!(
        rsp.options,
        ids(&[("router-port", &router_port_name(w.t1.id))])
    );
    assert_eq!(
        rsp.external_ids,
        ids(&[(CLUSTER_KEY, TAG), (ROUTER_KEY, &r.id.to_string())])
    );
    assert_eq!(t1.dhcp.len(), 1, "slaac has no DHCPv6 row");
    assert_eq!(t1.dhcp[0].options["router"], "10.10.0.1");
    assert_eq!(t1.dhcp[0].options["server_id"], "10.10.0.1");
    let t2 = sw(&want, &w.t2);
    assert_eq!(t2.dhcp.len(), 2);
    let v6 = t2
        .dhcp
        .iter()
        .find(|d| d.cidr == "2001:db8:20::/64")
        .unwrap();
    assert_eq!(
        v6.options,
        ids(&[
            (
                "server_id",
                &opl_types::ipam::server_mac(w.t2.id).to_string()
            ),
            ("dns_server", "{2001:db8::53}"),
        ])
    );

    // 라우터
    assert_eq!(want.routers.len(), 1);
    let lr = &want.routers[0];
    assert_eq!(lr.name, router_name(r.id));
    assert_eq!(
        lr.options,
        ids(&[
            ("dynamic_neigh_routers", "true"),
            ("always_learn_from_arp_request", "false")
        ])
    );
    assert_eq!(
        lr.external_ids,
        ids(&[
            (CLUSTER_KEY, TAG),
            (ID_KEY, &r.id.to_string()),
            (NAME_KEY, "r1")
        ])
    );
    let lrp = |name: String| lr.ports.iter().find(|p| p.name == name).unwrap().clone();
    let p1 = lrp(router_port_name(w.t1.id));
    assert_eq!(p1.networks, ["10.10.0.1/24", "2001:db8:10::1/64"]);
    assert_eq!(p1.mac, router_mac(r.id, w.t1.id).to_string());
    assert_eq!(p1.options, ids(&[("gateway_mtu", "1442")]));
    assert_eq!(
        p1.ipv6_ra_configs,
        ids(&[
            ("address_mode", "slaac"),
            ("send_periodic", "true"),
            ("mtu", "1442"),
            ("rdnss", "2001:db8::53"),
        ])
    );
    assert!(!p1.gateway);
    assert_eq!(
        lrp(router_port_name(w.t2.id)).ipv6_ra_configs["address_mode"],
        "dhcpv6_stateful"
    );
    let gw = lrp(gateway_port_name(r.id));
    assert_eq!(gw.networks, ["203.0.113.50/24", "2001:db8:ff::50/64"]);
    assert_eq!(gw.mac, router_mac(r.id, w.ext.id).to_string());
    assert!(gw.gateway);
    assert_eq!(gw.enabled, None);
    // 경로: 사용자 경로 + 체계마다 기본 경로(외부 서브넷의 게이트웨이)
    let routes: Vec<(&str, &str)> = lr
        .routes
        .iter()
        .map(|r| (r.ip_prefix.as_str(), r.nexthop.as_str()))
        .collect();
    assert_eq!(
        routes,
        [
            ("0.0.0.0/0", "203.0.113.1"),
            ("10.99.0.0/16", "10.10.0.254"),
            ("::/0", "2001:db8:ff::1"),
        ]
    );
    // 게이트웨이: 유지보수(n3)·physnet 없음(n4)·후보 아님(n5)을 빼고 관리자 우선순위 순
    let ha = lr.ha.as_ref().unwrap();
    assert_eq!(ha.name, ha_group_name(r.id));
    assert_eq!(
        ha.chassis,
        [
            (w.nodes[1].id.to_string(), TOP_GATEWAY_PRIORITY),
            (w.nodes[0].id.to_string(), TOP_GATEWAY_PRIORITY - 1),
        ]
    );
    // SNAT: 인터페이스의 IPv4 서브넷마다
    let nat: Vec<(&str, &str, &str)> = lr
        .nat
        .iter()
        .map(|n| {
            (
                n.kind.as_str(),
                n.external_ip.as_str(),
                n.logical_ip.as_str(),
            )
        })
        .collect();
    assert_eq!(
        nat,
        [
            ("snat", "203.0.113.50", "10.10.0.0/24"),
            ("snat", "203.0.113.50", "10.20.0.0/24"),
        ]
    );
    // 같은 입력이면 같은 출력
    assert_eq!(
        want,
        desired_routing(&w.networks, &[], &w.nodes, &[r], &[], TAG)
    );
}

#[test]
fn equal_priority_gateways_are_spread_by_hrw() {
    let w = world();
    let nodes = vec![
        gw_node("a", Some(100), &["ext"], false),
        gw_node("b", Some(100), &["ext"], false),
    ];
    let r = r1();
    let want = desired_routing(&w.networks, &[], &nodes, std::slice::from_ref(&r), &[], TAG);
    let expected: Vec<(String, u16)> =
        gateway_ranking(r.id, &[(nodes[0].id, 100), (nodes[1].id, 100)])
            .into_iter()
            .map(|(n, p)| (n.to_string(), p))
            .collect();
    assert_eq!(want.routers[0].ha.as_ref().unwrap().chassis, expected);
}

#[test]
fn without_gateway_candidates_the_gateway_port_is_closed_and_nat_is_not_written() {
    let w = world();
    let r = r1();
    let nodes = vec![gw_node("n4", Some(500), &[], false)];
    let want = desired_routing(&w.networks, &[], &nodes, std::slice::from_ref(&r), &[], TAG);
    let lr = &want.routers[0];
    let gw = lr
        .ports
        .iter()
        .find(|p| p.name == gateway_port_name(r.id))
        .unwrap();
    assert_eq!(gw.enabled, Some(false));
    assert!(!gw.gateway);
    assert!(lr.ha.is_none());
    assert!(lr.nat.is_empty());
    // 물리 망에는 라우터를 보이지 않는다.
    assert!(
        !sw(&want, &w.ext)
            .ports
            .iter()
            .any(|p| p.name == gateway_switch_port_name(r.id))
    );
    // 기본 경로는 그대로(포트가 열리면 곧바로 쓴다)
    assert!(lr.routes.iter().any(|r| r.ip_prefix == "0.0.0.0/0"));
    // SNAT를 끈 라우터도 NAT가 없다.
    let mut no_snat = r1();
    no_snat.spec.external.as_mut().unwrap().snat = Some(false);
    let want = desired_routing(&w.networks, &[], &w.nodes, &[no_snat], &[], TAG);
    assert!(want.routers[0].nat.is_empty());
    assert!(want.routers[0].ha.is_some());
}

#[test]
fn floating_ips_are_distributed_only_when_every_bound_node_has_the_physnet() {
    let w = world();
    let r = r1();
    let (n1, n5) = (&w.nodes[0], &w.nodes[4]);
    let no_physnet = gw_node("n6", None, &[], false);
    let mut nodes = w.nodes.clone();
    nodes.push(no_physnet.clone());
    let on_t1 = |name: &str, mac: &str, ip: &str| {
        vm_on(
            name,
            json!([{ "network": "t1", "model": "virtio", "mac": mac, "ips": [ip, "2001:db8:10::9"] }]),
        )
    };
    let a = running(on_t1("a", "52:54:00:00:00:0a", "10.10.0.10"), n1);
    let off = on_t1("off", "52:54:00:00:00:0b", "10.10.0.11");
    let away = running(
        on_t1("away", "52:54:00:00:00:0c", "10.10.0.12"),
        &no_physnet,
    );
    let mut moving = running(on_t1("moving", "52:54:00:00:00:0d", "10.10.0.13"), n1);
    moving.status.phase = VmPhase::Migrating;
    moving.status.migration = Some(MigrationStatus {
        node: n5.id,
        task: Uuid::now_v7(),
        disks: Vec::new(),
    });
    let mut moving_out = moving.clone();
    moving_out.id = Uuid::now_v7();
    moving_out.spec.nics[0].mac = Some("52:54:00:00:00:0e".parse().unwrap());
    moving_out.spec.nics[0].ips = vec!["10.10.0.14".parse().unwrap()];
    moving_out.status.migration.as_mut().unwrap().node = no_physnet.id;
    let unrouted = vm_on(
        "unrouted",
        json!([{ "network": "t3", "model": "virtio", "mac": "52:54:00:00:00:0f", "ips": ["10.30.0.5"] }]),
    );
    let vms = vec![
        a.clone(),
        off.clone(),
        away.clone(),
        moving.clone(),
        moving_out.clone(),
        unrouted.clone(),
    ];
    let fips = vec![
        fip("203.0.113.120", Some((&a, "52:54:00:00:00:0a"))),
        fip("203.0.113.121", Some((&off, "52:54:00:00:00:0b"))),
        fip("203.0.113.122", Some((&away, "52:54:00:00:00:0c"))),
        fip("203.0.113.123", Some((&moving, "52:54:00:00:00:0d"))),
        fip("203.0.113.124", Some((&moving_out, "52:54:00:00:00:0e"))),
        fip("203.0.113.125", Some((&unrouted, "52:54:00:00:00:0f"))),
        fip("203.0.113.126", None),
        // 없는 NIC
        fip("203.0.113.127", Some((&a, "52:54:00:00:00:99"))),
    ];
    let want = desired_routing(&w.networks, &vms, &nodes, &[r], &fips, TAG);
    let nat: BTreeMap<&str, (&str, Option<&str>, Option<&str>)> = want.routers[0]
        .nat
        .iter()
        .filter(|n| n.kind == "dnat_and_snat")
        .map(|n| {
            (
                n.external_ip.as_str(),
                (
                    n.logical_ip.as_str(),
                    n.logical_port.as_deref(),
                    n.external_mac.as_deref(),
                ),
            )
        })
        .collect();
    let port_a = port_name(a.id, "52:54:00:00:00:0a".parse().unwrap());
    let port_m = port_name(moving.id, "52:54:00:00:00:0d".parse().unwrap());
    assert_eq!(
        nat,
        BTreeMap::from([
            (
                "203.0.113.120",
                (
                    "10.10.0.10",
                    Some(port_a.as_str()),
                    Some("52:54:00:00:00:0a")
                )
            ),
            ("203.0.113.121", ("10.10.0.11", None, None)),
            ("203.0.113.122", ("10.10.0.12", None, None)),
            (
                "203.0.113.123",
                (
                    "10.10.0.13",
                    Some(port_m.as_str()),
                    Some("52:54:00:00:00:0d")
                )
            ),
            ("203.0.113.124", ("10.10.0.14", None, None)),
        ])
    );
    let a_nat = want.routers[0]
        .nat
        .iter()
        .find(|n| n.external_ip == "203.0.113.120")
        .unwrap();
    assert_eq!(
        a_nat.external_ids,
        ids(&[(CLUSTER_KEY, TAG), (FIP_KEY, &fips[0].id.to_string())])
    );
}

#[test]
fn dhcpv6_ports_and_provider_dhcp() {
    let mut w = world();
    // 공급자 서브넷에 DHCP를 켜면 `router`(물리 라우터)가 있다.
    w.ext.spec.subnets[0].dhcp = Some(true);
    w.networks[3] = w.ext.clone();
    let vm = vm_on(
        "v6",
        json!([
            { "network": "t2", "model": "virtio", "mac": "52:54:00:00:00:01",
              "ips": ["10.20.0.5", "2001:db8:20::5"] },
            { "network": "ext", "model": "virtio", "mac": "52:54:00:00:00:02",
              "ips": ["203.0.113.10"] }
        ]),
    );
    let want = desired_routing(&w.networks, std::slice::from_ref(&vm), &[], &[], &[], TAG);
    let p = port(
        sw(&want, &w.t2),
        &port_name(vm.id, "52:54:00:00:00:01".parse().unwrap()),
    );
    assert_eq!(p.dhcpv4.as_deref(), Some("10.20.0.0/24"));
    assert_eq!(p.dhcpv6.as_deref(), Some("2001:db8:20::/64"));
    // 라우터가 없는 오버레이는 `router`가 없다.
    assert!(!sw(&want, &w.t2).dhcp[0].options.contains_key("router"));
    let ext = sw(&want, &w.ext);
    assert_eq!(ext.dhcp.len(), 1);
    assert_eq!(ext.dhcp[0].options["router"], "203.0.113.1");
    assert_eq!(ext.dhcp[0].options["mtu"], "1500");
    let p = port(ext, &port_name(vm.id, "52:54:00:00:00:02".parse().unwrap()));
    assert_eq!(p.dhcpv4.as_deref(), Some("203.0.113.0/24"));
    // 라우터가 없으면 라우터 행·게이트웨이 포트가 없다.
    assert!(want.routers.is_empty());
    assert!(!ext.ports.iter().any(|p| p.kind == "router"));
}

/// 한 네트워크는 라우터 하나에만(먼저 온 id), 공급자·서브넷 없는 네트워크는 인터페이스가 되지 않는다.
#[test]
fn invalid_interfaces_are_skipped() {
    let w = world();
    let bare = net("bare", json!({"sdn": {}}));
    let mut networks = w.networks.clone();
    networks.push(bare);
    let first = router(
        "a",
        json!({"interfaces": [{"network": "t1"}, {"network": "ext"}, {"network": "bare"}, {"network": "nope"}]}),
    );
    let second = router(
        "b",
        json!({"interfaces": [{"network": "t1"}, {"network": "t2"}]}),
    );
    let want = desired_routing(
        &networks,
        &[],
        &w.nodes,
        &[second.clone(), first.clone()],
        &[],
        TAG,
    );
    let names = |r: &Router| -> Vec<String> {
        want.routers
            .iter()
            .find(|x| x.router == r.id)
            .unwrap()
            .ports
            .iter()
            .map(|p| p.name.clone())
            .collect()
    };
    assert_eq!(names(&first), [router_port_name(w.t1.id)]);
    assert_eq!(names(&second), [router_port_name(w.t2.id)]);
}

fn nb_router<'a>(
    rows: &'a [serde_json::Map<String, Value>],
    name: &str,
) -> &'a serde_json::Map<String, Value> {
    rows.iter().find(|r| r["name"] == name).unwrap()
}

#[tokio::test]
async fn router_rows_reach_the_nb_and_follow_changes() {
    let nb = Nb::new().await;
    let w = world();
    let r = r1();
    let a = running(
        vm_on(
            "a",
            json!([{ "network": "t1", "model": "virtio", "mac": "52:54:00:00:00:0a", "ips": ["10.10.0.10"] }]),
        ),
        &w.nodes[0],
    );
    let f = fip("203.0.113.120", Some((&a, "52:54:00:00:00:0a")));
    // 관리자의 라우터와 HA 그룹은 남는다.
    nb.fake.with(|s| {
        s.insert("Logical_Router", json!({"name": "admin-router"}));
        s.insert("HA_Chassis_Group", json!({"name": "admin-ha"}));
    });
    let mut want = desired_routing(
        &w.networks,
        std::slice::from_ref(&a),
        &w.nodes,
        std::slice::from_ref(&r),
        std::slice::from_ref(&f),
        TAG,
    );
    // 네트워크 넷 + 라우터 하나
    assert_eq!(nb.sync(&want, "1:1").await, 5);
    assert!(plan(&want, &nb.read().await).is_empty());
    assert_eq!(nb.sync(&want, "1:2").await, 0);

    let lr = nb_router(&nb.rows("Logical_Router"), &router_name(r.id)).clone();
    assert_eq!(parse_uuids(&lr["ports"]).len(), 3);
    assert_eq!(parse_uuids(&lr["static_routes"]).len(), 3);
    assert_eq!(parse_uuids(&lr["nat"]).len(), 3);
    let lrps = nb.rows("Logical_Router_Port");
    let dgp = lrps
        .iter()
        .find(|p| p["name"] == gateway_port_name(r.id))
        .unwrap();
    let groups = nb.rows("HA_Chassis_Group");
    let group = groups
        .iter()
        .find(|g| g["name"] == ha_group_name(r.id))
        .unwrap();
    assert_eq!(
        parse_uuids(&dgp["ha_chassis_group"]),
        parse_uuids(&group["_uuid"])
    );
    assert_eq!(parse_uuids(&group["ha_chassis"]).len(), 2);
    let chassis = nb.rows("HA_Chassis");
    let top = chassis
        .iter()
        .find(|c| parse_int(&c["priority"]) == Some(32767))
        .unwrap();
    assert_eq!(top["chassis_name"], w.nodes[1].id.to_string());
    let nats = nb.rows("NAT");
    let dnat = nats.iter().find(|n| n["type"] == "dnat_and_snat").unwrap();
    assert_eq!(dnat["external_mac"], "52:54:00:00:00:0a");
    assert_eq!(parse_map(&dnat["external_ids"])[FIP_KEY], f.id.to_string());
    let ln = nb
        .rows("Logical_Switch_Port")
        .into_iter()
        .find(|p| p["name"] == localnet_port_name(w.ext.id))
        .unwrap();
    assert_eq!(ln["type"], "localnet");
    assert_eq!(ln["tag"], 100);
    assert_eq!(parse_strings(&ln["addresses"]), ["unknown"]);

    // 관리자가 우리 라우터에 넣은 경로(주인 표시 없음)는 남는다.
    nb.fake.with(|s| {
        let route = s.insert(
            "Logical_Router_Static_Route",
            json!({"ip_prefix": "192.0.2.0/24", "nexthop": "10.10.0.253"}),
        );
        for lr in s.tables.get_mut("Logical_Router").unwrap().values_mut() {
            if lr["name"] == router_name(r.id) {
                let mut routes = parse_uuids(&lr["static_routes"]);
                routes.push(route.clone());
                lr.insert(
                    "static_routes".into(),
                    json!([
                        "set",
                        routes
                            .iter()
                            .map(|u| json!(["uuid", u]))
                            .collect::<Vec<_>>()
                    ]),
                );
            }
        }
    });

    // 게이트웨이 순위가 바뀜(n2를 유지보수로): HA chassis만 바뀌고 같은 행(n1)은 우선순위만 바뀐다.
    let n1_row = |nb: &Nb| {
        nb.rows("HA_Chassis")
            .into_iter()
            .find(|c| c["chassis_name"] == w.nodes[0].id.to_string())
            .unwrap()
    };
    let before = n1_row(&nb)["_uuid"].clone();
    let mut nodes = w.nodes.clone();
    nodes[1].spec.maintenance = true;
    // 꺼진 VM이 되면 유동 IP가 중앙으로
    let mut off = a.clone();
    off.status.phase = VmPhase::Stopped;
    off.spec.power = PowerState::Stopped;
    want = desired_routing(
        &w.networks,
        std::slice::from_ref(&off),
        &nodes,
        std::slice::from_ref(&r),
        std::slice::from_ref(&f),
        TAG,
    );
    // 라우터 묶음과 VM 포트(chassis 없음)의 스위치 묶음
    assert_eq!(nb.sync(&want, "1:3").await, 2);
    assert!(plan(&want, &nb.read().await).is_empty());
    let after = n1_row(&nb);
    assert_eq!(after["_uuid"], before);
    assert_eq!(parse_int(&after["priority"]), Some(32767));
    assert_eq!(nb.rows("HA_Chassis").len(), 1);
    let nats = nb.rows("NAT");
    let dnat = nats.iter().find(|n| n["type"] == "dnat_and_snat").unwrap();
    assert_eq!(parse_strings(&dnat["external_mac"]), Vec::<String>::new());
    assert_eq!(nb.rows("Logical_Router_Static_Route").len(), 4);

    // 후보가 없어짐: DGP 닫힘, HA 그룹·NAT·게이트웨이 스위치 포트가 사라진다.
    let none: Vec<Node> = Vec::new();
    want = desired_routing(
        &w.networks,
        std::slice::from_ref(&off),
        &none,
        std::slice::from_ref(&r),
        std::slice::from_ref(&f),
        TAG,
    );
    nb.sync(&want, "1:4").await;
    assert!(plan(&want, &nb.read().await).is_empty());
    assert!(nb.rows("NAT").is_empty());
    assert!(nb.rows("HA_Chassis").is_empty());
    let groups: Vec<Value> = nb
        .rows("HA_Chassis_Group")
        .iter()
        .map(|g| g["name"].clone())
        .collect();
    assert_eq!(groups, [json!("admin-ha")]);
    let dgp = nb
        .rows("Logical_Router_Port")
        .into_iter()
        .find(|p| p["name"] == gateway_port_name(r.id))
        .unwrap();
    assert_eq!(dgp["enabled"], false);
    assert_eq!(parse_uuids(&dgp["ha_chassis_group"]), Vec::<String>::new());

    // 다시 후보가 생기고, 외부를 뗌: DGP·그룹·기본 경로가 사라진다.
    want = desired_routing(
        &w.networks,
        &[],
        &w.nodes,
        std::slice::from_ref(&r),
        &[],
        TAG,
    );
    nb.sync(&want, "1:5").await;
    assert!(plan(&want, &nb.read().await).is_empty());
    let mut internal = r.clone();
    internal.spec.external = None;
    want = desired_routing(
        &w.networks,
        &[],
        &w.nodes,
        std::slice::from_ref(&internal),
        &[],
        TAG,
    );
    nb.sync(&want, "1:6").await;
    assert!(plan(&want, &nb.read().await).is_empty());
    assert_eq!(nb.rows("Logical_Router_Port").len(), 2);
    assert_eq!(nb.rows("HA_Chassis_Group").len(), 1);
    // 사용자 경로 하나 + 관리자 경로
    assert_eq!(nb.rows("Logical_Router_Static_Route").len(), 2);

    // 인터페이스를 다른 라우터로 옮김: t2는 r2로(포트 이름이 같다 — 새 라우터 MAC으로 다시 넣는다).
    let mut only_t1 = internal.clone();
    only_t1.spec.interfaces.retain(|i| i.network == "t1");
    let r2 = router("r2", json!({"interfaces": [{"network": "t2"}]}));
    for order in [[only_t1.clone(), r2.clone()], [r2.clone(), only_t1.clone()]] {
        want = desired_routing(&w.networks, &[], &w.nodes, &order, &[], TAG);
        nb.sync(&want, "1:7").await;
        assert!(plan(&want, &nb.read().await).is_empty());
    }
    let lrp_t2 = nb
        .rows("Logical_Router_Port")
        .into_iter()
        .find(|p| p["name"] == router_port_name(w.t2.id))
        .unwrap();
    let owner = nb_router(&nb.rows("Logical_Router"), &router_name(r2.id)).clone();
    assert_eq!(parse_uuids(&owner["ports"]), parse_uuids(&lrp_t2["_uuid"]));
    assert_eq!(lrp_t2["mac"], router_mac(r2.id, w.t2.id).to_string());

    // 라우터를 모두 지우면 우리 행만 사라진다(라우터 스위치 포트도).
    want = desired_routing(&w.networks, &[], &w.nodes, &[], &[], TAG);
    nb.sync(&want, "1:8").await;
    assert!(plan(&want, &nb.read().await).is_empty());
    let routers: Vec<Value> = nb
        .rows("Logical_Router")
        .iter()
        .map(|r| r["name"].clone())
        .collect();
    assert_eq!(routers, [json!("admin-router")]);
    assert!(nb.rows("Logical_Router_Port").is_empty());
    assert!(
        !nb.rows("Logical_Switch_Port")
            .iter()
            .any(|p| p["type"] == "router")
    );
}

/// 지운 라우터의 DGP가 HA 그룹을 가리켜도 한 트랜잭션에 지워진다(포트를 곧바로 지움 — 참조 검사).
#[tokio::test]
async fn deleting_a_router_with_a_gateway_group_is_one_transaction() {
    let nb = Nb::new().await;
    let w = world();
    let r = r1();
    let want = desired_routing(
        &w.networks,
        &[],
        &w.nodes,
        std::slice::from_ref(&r),
        &[],
        TAG,
    );
    nb.sync(&want, "1:1").await;
    // 네트워크는 그대로 두고 라우터만 지움
    let want = desired_routing(&w.networks, &[], &w.nodes, &[], &[], TAG);
    let batches = plan(&want, &nb.read().await);
    let router_batch = batches.iter().find(|b| b.key == router_name(r.id)).unwrap();
    assert!(
        router_batch
            .ops
            .iter()
            .any(|o| o["table"] == "HA_Chassis_Group" && o["op"] == "delete")
    );
    nb.sync(&want, "1:2").await;
    assert!(nb.rows("Logical_Router").is_empty());
    assert!(nb.rows("HA_Chassis_Group").is_empty());
    assert!(nb.rows("HA_Chassis").is_empty());
    assert!(nb.rows("NAT").is_empty());
    assert!(nb.rows("Logical_Router_Static_Route").is_empty());
}

/// 계획 65의 바라는 상태(라우터·공급자·`ipv6_mode` 없음)는 [`desired`]와 [`desired_routing`]이 같고, 그 NB에 다시 맞추면
/// 연산이 없다(업그레이드가 NB를 건드리지 않는다).
#[tokio::test]
async fn a_plan65_nb_needs_no_operations() {
    let nb = Nb::new().await;
    let mut t = super::tests::sdn_net("tenant");
    t.spec.subnets = serde_json::from_value(json!([
        { "cidr": "10.0.0.0/24" }, { "cidr": "2001:db8::/64" }
    ]))
    .unwrap();
    let vm = vm_on(
        "web",
        json!([{ "network": "tenant", "model": "virtio", "mac": "52:54:00:00:00:01",
                 "ips": ["10.0.0.5", "2001:db8::5"] }]),
    );
    let nodes = [gw_node("n1", Some(100), &["ext"], false)];
    let old = desired(
        std::slice::from_ref(&t),
        std::slice::from_ref(&vm),
        &nodes,
        TAG,
    );
    nb.sync(&old, "1:1").await;
    // 계획 65가 넣던 연산과 같은 칸만 쓴다(새 칸 `type`·`tag`·`dhcpv6_options` 없음).
    let inserted = nb.fake.with(|s| s.transacts.last().cloned().unwrap());
    let port_insert = inserted
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["table"] == "Logical_Switch_Port")
        .unwrap();
    for c in ["type", "tag", "dhcpv6_options"] {
        assert!(port_insert["row"].get(c).is_none(), "{port_insert}");
    }
    let new = desired_routing(
        std::slice::from_ref(&t),
        std::slice::from_ref(&vm),
        &nodes,
        &[],
        &[],
        TAG,
    );
    assert_eq!(old, new);
    assert!(plan(&new, &nb.read().await).is_empty());
}

/// 리뷰 1 웹 I1: 라우터 보기는 `Logical_Router_Port` 한 표의 세 칸만 읽고 우리 게이트웨이 포트의 `hosting-chassis`만 낸다
/// (관리자의 포트·인터페이스 포트·주인 없는 상태는 빠짐). 라우터 하나를 주면 그 포트 하나만 고른다.
#[tokio::test]
async fn the_hosting_read_selects_only_gateway_ports() {
    use opl_driver::ovn::ovsdb::NB_DB;
    let nb = Nb::new().await;
    let w = world();
    let r = r1();
    let want = desired_routing(
        &w.networks,
        &[],
        &w.nodes,
        std::slice::from_ref(&r),
        &[],
        TAG,
    );
    nb.sync(&want, "1:1").await;
    nb.fake.with(|s| {
        s.insert(
            "Logical_Router_Port",
            json!({"name": "admin-gw", "mac": "02:00:00:00:00:01",
                   "status": ["map", [["hosting-chassis", "admin"]]]}),
        );
    });
    let read = |router: Option<Uuid>| {
        let (conditions, columns) = hosting_select(TAG, router);
        let client = &nb.client;
        async move {
            let rows = client
                .select(NB_DB, "Logical_Router_Port", conditions, &columns)
                .await
                .unwrap();
            assert!(rows.iter().all(|r| r.len() <= 4), "{rows:?}");
            parse_hosting(&rows)
        }
    };
    // 아직 northd가 주인을 적지 않았다.
    assert!(read(None).await.is_empty());
    nb.client
        .transact(
            NB_DB,
            vec![json!({"op": "update", "table": "Logical_Router_Port",
                "where": [["name", "==", gateway_port_name(r.id)]],
                "row": {"status": ["map", [["hosting-chassis", "chassis-2"]]]}})],
        )
        .await
        .unwrap();
    let all = read(None).await;
    assert_eq!(all, BTreeMap::from([(r.id, "chassis-2".to_string())]));
    assert_eq!(read(Some(r.id)).await, all);
    assert!(read(Some(Uuid::new_v4())).await.is_empty());
}
