//! OvnSync의 순수 부분을 가짜 NB 서버(`orbivisor_driver::ovn::FakeOvsdb`)에 실제로 적용해 본다: 견주기·넣기·바꾸기·지우기,
//! 다시 견주면 할 일 없음, 남의 행, 펜싱, 나누기, 규모.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use orbivisor_driver::ovn::ovsdb::{DEFAULT_TIMEOUT, parse_map, parse_strings, parse_uuids};
use orbivisor_driver::ovn::{Endpoint, FakeOvsdb, NB_DB, OvsdbClient, Transport};
use orbivisor_types::network::{NetworkSpec, SdnNet, Vxlan};
use orbivisor_types::resource::Resource;
use serde_json::{Value, json};
use uuid::Uuid;

use super::nb::*;

pub(super) const TAG: &str = "abcdef012345";

pub(super) fn ids(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub(super) fn spec() -> NetworkSpec {
    NetworkSpec {
        bridge: None,
        vlan: None,
        vxlan: None,
        sdn: None,
        mtu: None,
        subnets: Vec::new(),
        folder: None,
    }
}

pub(super) fn sdn_net(name: &str) -> orbivisor_types::network::Network {
    Resource::new(
        name,
        NetworkSpec {
            sdn: Some(SdnNet::default()),
            ..spec()
        },
    )
}

fn port(name: &str, ip: &str, dhcp: Option<&str>) -> PortRow {
    PortRow {
        name: name.into(),
        kind: String::new(),
        addresses: vec![format!("02:00:00:00:00:01 {ip}")],
        port_security: vec![format!("02:00:00:00:00:01 {ip}")],
        dhcpv4: dhcp.map(String::from),
        dhcpv6: None,
        enabled: Some(true),
        tag: None,
        options: BTreeMap::new(),
        external_ids: ids(&[(CLUSTER_KEY, TAG)]),
        port_group: None,
    }
}

fn switch(network: Uuid, ports: Vec<PortRow>, dhcp: Vec<DhcpRow>) -> SwitchRow {
    SwitchRow {
        network,
        name: switch_name(network),
        external_ids: ids(&[(CLUSTER_KEY, TAG), (ID_KEY, &network.to_string())]),
        ports,
        dhcp,
        load_balancers: Vec::new(),
        qos: Vec::new(),
    }
}

fn dhcp(network: Uuid, cidr: &str, lease: &str) -> DhcpRow {
    DhcpRow {
        cidr: cidr.into(),
        options: ids(&[("lease_time", lease)]),
        external_ids: ids(&[
            (CLUSTER_KEY, TAG),
            (ID_KEY, &network.to_string()),
            (SUBNET_KEY, cidr),
        ]),
    }
}

pub(super) struct Nb {
    pub(super) fake: FakeOvsdb,
    pub(super) client: OvsdbClient,
}

impl Nb {
    pub(super) async fn new() -> Self {
        let fake = FakeOvsdb::start(NB_DB).await;
        let client =
            OvsdbClient::connect(&Transport::Tcp, &Endpoint::ip(fake.addr()), DEFAULT_TIMEOUT)
                .await
                .unwrap();
        Self { fake, client }
    }

    pub(super) async fn read(&self) -> NbActual {
        parse_actual(&self.client.transact(NB_DB, read_ops(TAG)).await.unwrap())
    }

    /// 견주어 쓰고(펜싱 포함) 쓴 트랜잭션 수를 돌려준다.
    pub(super) async fn sync(&self, want: &NbState, token: &str) -> usize {
        let have = self.read().await;
        let mut global = have.global.clone();
        let txs = plan(want, &have);
        let n = txs.len();
        for b in txs {
            let (mut ops, ext) = fence_ops(global.as_ref(), TAG, token);
            ops.extend(b.ops);
            let results = self.client.transact(NB_DB, ops).await.unwrap();
            global = Some(Global {
                uuid: global
                    .map(|g| g.uuid)
                    .unwrap_or_else(|| parse_uuids(&results[1]["uuid"])[0].clone()),
                external_ids: ext,
                nb_cfg: 0,
                hv_cfg: 0,
            });
        }
        n
    }

    pub(super) fn rows(&self, table: &str) -> Vec<serde_json::Map<String, Value>> {
        self.fake.rows(table)
    }
}

#[test]
fn desired_has_a_switch_per_sdn_network_in_id_order() {
    let a = sdn_net("a");
    let b = sdn_net("b");
    let mut foldered = sdn_net("c");
    let folder = Uuid::now_v7();
    foldered.spec.folder = Some(folder);
    let vxlan = Resource::new(
        "x",
        NetworkSpec {
            vxlan: Some(Vxlan { vni: 5 }),
            ..spec()
        },
    );
    let want = desired(
        &[foldered.clone(), vxlan, b.clone(), a.clone()],
        &[],
        &[],
        TAG,
    );
    let names: Vec<&str> = want.switches.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            switch_name(a.id),
            switch_name(b.id),
            switch_name(foldered.id)
        ]
    );
    assert_eq!(
        want.switches[2].external_ids,
        ids(&[
            (CLUSTER_KEY, TAG),
            (ID_KEY, &foldered.id.to_string()),
            (NAME_KEY, "c"),
            (FOLDER_KEY, &folder.to_string()),
        ])
    );
    assert!(
        want.switches
            .iter()
            .all(|s| s.ports.is_empty() && s.dhcp.is_empty())
    );
    // 같은 입력이면 같은 출력
    assert_eq!(want, desired(&[a, b, foldered], &[], &[], TAG));
}

pub(super) fn vm_on(name: &str, nics: serde_json::Value) -> orbivisor_types::vm::Vm {
    Resource::new(
        name,
        serde_json::from_value(json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt", "nics": nics
        }))
        .unwrap(),
    )
}

pub(super) fn node_res(name: &str) -> orbivisor_types::node::Node {
    Resource::new(
        name,
        serde_json::from_value(json!({ "address": "127.0.0.1:7443" })).unwrap(),
    )
}

#[test]
fn desired_has_dhcp_rows_and_a_port_per_sdn_nic() {
    use orbivisor_types::vm::{MigrationStatus, PowerState, VmPhase};
    let mut net = sdn_net("tenant");
    net.spec.subnets = serde_json::from_value(json!([
        { "cidr": "10.0.0.0/24", "dns_servers": ["10.0.0.53", "2001:db8::53", "1.1.1.1"] },
        { "cidr": "10.9.0.0/24", "dhcp": false },
        { "cidr": "2001:db8::/64" }
    ]))
    .unwrap();
    let bare = sdn_net("bare");
    let (n1, n2) = (node_res("n1"), node_res("n2"));
    let nodes = [n1.clone(), n2.clone()];
    // 꺼진 VM: 포트는 있고 chassis는 없다. 단순 네트워크 NIC는 포트가 없다.
    let off = vm_on(
        "off",
        json!([
            { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:09" },
            { "network": "tenant", "model": "virtio", "mac": "52:54:00:00:00:01",
              "ips": ["2001:db8::5", "10.0.0.5"] }
        ]),
    );
    // 켜진 VM(서브넷 밖 주소 없음, DHCP 꺼진 서브넷의 주소)
    let mut on = vm_on(
        "on",
        json!([{ "network": "tenant", "model": "virtio", "mac": "52:54:00:00:00:02",
                 "ips": ["10.9.0.7"] }]),
    );
    on.status.phase = VmPhase::Running;
    on.status.node = Some(n1.id);
    // 옮기는 VM: 두 chassis와 rarp
    let mut moving = vm_on(
        "moving",
        json!([{ "network": "bare", "model": "virtio", "mac": "52:54:00:00:00:03" }]),
    );
    moving.status.phase = VmPhase::Migrating;
    moving.status.node = Some(n1.id);
    moving.status.migration = Some(MigrationStatus {
        node: n2.id,
        task: Uuid::now_v7(),
        disks: Vec::new(),
    });
    // 켜는 중(상태가 아직 지난번 노드): 스펙의 노드
    let mut starting = vm_on(
        "starting",
        json!([{ "network": "bare", "model": "virtio", "mac": "52:54:00:00:00:04" }]),
    );
    starting.spec.power = PowerState::Running;
    starting.spec.node = Some(n2.id);
    starting.status.node = Some(n1.id);
    // 템플릿은 포트가 없다. 클러스터에 없는 노드는 적지 않는다.
    let mut template = vm_on(
        "template",
        json!([{ "network": "tenant", "model": "virtio", "mac": "52:54:00:00:00:05" }]),
    );
    template.spec.template = true;
    let mut gone = vm_on(
        "gone",
        json!([{ "network": "bare", "model": "virtio", "mac": "52:54:00:00:00:06" }]),
    );
    gone.status.phase = VmPhase::Running;
    gone.status.node = Some(Uuid::now_v7());

    let vms = [
        off.clone(),
        on.clone(),
        moving.clone(),
        starting.clone(),
        template,
        gone.clone(),
    ];
    let want = desired(&[net.clone(), bare.clone()], &vms, &nodes, TAG);
    let sw = want.switches.iter().find(|s| s.network == net.id).unwrap();
    // DHCP는 DHCP를 켠 IPv4 서브넷만이고 `router`가 없다. DNS는 IPv4만.
    assert_eq!(sw.dhcp.len(), 1);
    let mac = orbivisor_types::ipam::server_mac(net.id).to_string();
    assert_eq!(
        sw.dhcp[0],
        DhcpRow {
            cidr: "10.0.0.0/24".into(),
            options: ids(&[
                ("server_id", "10.0.0.1"),
                ("server_mac", &mac),
                ("lease_time", "86400"),
                ("mtu", "1442"),
                ("dns_server", "{10.0.0.53, 1.1.1.1}"),
            ]),
            external_ids: ids(&[
                (CLUSTER_KEY, TAG),
                (ID_KEY, &net.id.to_string()),
                (SUBNET_KEY, "10.0.0.0/24"),
            ]),
        }
    );
    let port = |sw: &SwitchRow, vm: &orbivisor_types::vm::Vm, mac: &str| {
        let name = port_name(vm.id, mac.parse().unwrap());
        sw.ports.iter().find(|p| p.name == name).cloned().unwrap()
    };
    assert_eq!(sw.ports.len(), 2, "{:?}", sw.ports);
    let p = port(sw, &off, "52:54:00:00:00:01");
    assert_eq!(p.name, format!("opl-{}-525400000001", off.id));
    // IPv4가 먼저다.
    assert_eq!(p.addresses, ["52:54:00:00:00:01 10.0.0.5 2001:db8::5"]);
    assert_eq!(p.port_security, p.addresses);
    assert_eq!(p.dhcpv4.as_deref(), Some("10.0.0.0/24"));
    assert_eq!(p.enabled, Some(true));
    assert!(p.options.is_empty());
    assert_eq!(
        p.external_ids,
        ids(&[
            (CLUSTER_KEY, TAG),
            (VM_KEY, &off.id.to_string()),
            (NIC_KEY, "1"),
        ])
    );
    let p = port(sw, &on, "52:54:00:00:00:02");
    assert_eq!(p.addresses, ["52:54:00:00:00:02 10.9.0.7"]);
    assert_eq!(p.enabled, Some(true));
    assert_eq!(p.dhcpv4, None);
    assert_eq!(p.options, ids(&[(REQUESTED_CHASSIS, &n1.id.to_string())]));

    let sw = want.switches.iter().find(|s| s.network == bare.id).unwrap();
    assert!(sw.dhcp.is_empty());
    let p = port(sw, &moving, "52:54:00:00:00:03");
    assert_eq!(p.addresses, ["52:54:00:00:00:03"]);
    // 서브넷이 없는 네트워크는 L2만이라 MAC만으로 켠다.
    assert_eq!(p.enabled, Some(true));
    assert_eq!(
        p.options,
        ids(&[
            (REQUESTED_CHASSIS, &format!("{},{}", n1.id, n2.id)),
            (ACTIVATION_STRATEGY, "rarp"),
        ])
    );
    let p = port(sw, &starting, "52:54:00:00:00:04");
    assert_eq!(p.options, ids(&[(REQUESTED_CHASSIS, &n2.id.to_string())]));
    assert!(port(sw, &gone, "52:54:00:00:00:06").options.is_empty());
    // 같은 입력이면 같은 출력(VM 순서와 상관없이)
    let mut shuffled = vms.to_vec();
    shuffled.reverse();
    assert_eq!(want, desired(&[bare, net], &shuffled, &nodes, TAG));
}

#[tokio::test]
async fn ports_and_dhcp_reach_the_nb_and_follow_spec_changes() {
    let nb = Nb::new().await;
    let mut net = sdn_net("tenant");
    net.spec.subnets = serde_json::from_value(json!([{ "cidr": "10.0.0.0/24" }])).unwrap();
    let mut vm = vm_on(
        "web",
        json!([{ "network": "tenant", "model": "virtio", "mac": "52:54:00:00:00:01",
                 "ips": ["10.0.0.5"] }]),
    );
    let want = desired(
        std::slice::from_ref(&net),
        std::slice::from_ref(&vm),
        &[],
        TAG,
    );
    assert_eq!(nb.sync(&want, "1:1").await, 1);
    let ports = nb.rows("Logical_Switch_Port");
    assert_eq!(ports.len(), 1);
    let dhcp = nb.rows("DHCP_Options");
    assert_eq!(dhcp.len(), 1);
    assert_eq!(
        parse_uuids(&ports[0]["dhcpv4_options"]),
        parse_uuids(&dhcp[0]["_uuid"])
    );
    assert_eq!(
        parse_strings(&ports[0]["port_security"]),
        ["52:54:00:00:00:01 10.0.0.5"]
    );
    // 다시 견주면 할 일이 없다.
    assert_eq!(nb.sync(&want, "1:2").await, 0);
    // 주소가 바뀌면 포트만 바뀐다. VM을 지우면 포트가 사라지고 DHCP 행은 남는다.
    vm.spec.nics[0].ips = vec!["10.0.0.6".parse().unwrap()];
    let want = desired(
        std::slice::from_ref(&net),
        std::slice::from_ref(&vm),
        &[],
        TAG,
    );
    assert_eq!(nb.sync(&want, "1:3").await, 1);
    assert_eq!(
        parse_strings(&nb.rows("Logical_Switch_Port")[0]["addresses"]),
        ["52:54:00:00:00:01 10.0.0.6"]
    );
    let want = desired(std::slice::from_ref(&net), &[], &[], TAG);
    assert_eq!(nb.sync(&want, "1:4").await, 1);
    assert!(nb.rows("Logical_Switch_Port").is_empty());
    assert_eq!(nb.rows("DHCP_Options").len(), 1);
    // 서브넷을 빼면 DHCP 행도 사라진다.
    net.spec.subnets.clear();
    let want = desired(std::slice::from_ref(&net), &[], &[], TAG);
    assert_eq!(nb.sync(&want, "1:5").await, 1);
    assert!(nb.rows("DHCP_Options").is_empty());
}

#[test]
fn tokens_parse() {
    assert_eq!(parse_token("3:17"), Some((3, 17)));
    assert_eq!(parse_token("3"), None);
    assert_eq!(parse_token("x:1"), None);
    assert_eq!(fence_key(TAG), "orbivisor:abcdef012345:fence");
}

#[tokio::test]
async fn creates_updates_and_removes_switches_ports_and_dhcp() {
    let nb = Nb::new().await;
    let (n1, n2) = (Uuid::now_v7(), Uuid::now_v7());
    let mut want = NbState {
        port_groups: Vec::new(),
        load_balancers: Vec::new(),
        routers: Vec::new(),
        switches: vec![
            switch(
                n1,
                vec![
                    port("opl-vm1-0201", "10.0.0.5", Some("10.0.0.0/24")),
                    port("opl-vm2-0202", "10.0.0.6", None),
                ],
                vec![dhcp(n1, "10.0.0.0/24", "86400")],
            ),
            switch(n2, vec![], vec![]),
        ],
    };
    // 관리자가 만든 태그 없는 스위치와 다른 클러스터의 스위치는 남는다.
    nb.fake.with(|s| {
        s.insert("Logical_Switch", json!({"name": "admin"}));
        s.insert(
            "Logical_Switch",
            json!({"name": "other", "external_ids": ["map", [[CLUSTER_KEY, "000000000000"]]]}),
        );
    });
    // 네트워크마다 한 트랜잭션(리뷰 1 I-6)
    assert_eq!(nb.sync(&want, "1:10").await, 2);
    assert_eq!(nb.rows("Logical_Switch").len(), 4);
    let ports = nb.rows("Logical_Switch_Port");
    assert_eq!(ports.len(), 2);
    let dhcp_rows = nb.rows("DHCP_Options");
    assert_eq!(dhcp_rows.len(), 1);
    let p1 = ports.iter().find(|p| p["name"] == "opl-vm1-0201").unwrap();
    assert_eq!(
        parse_uuids(&p1["dhcpv4_options"]),
        parse_uuids(&dhcp_rows[0]["_uuid"])
    );
    assert_eq!(
        parse_strings(&p1["addresses"]),
        vec!["02:00:00:00:00:01 10.0.0.5"]
    );
    // 펜싱 토큰과 nb_cfg
    let g = &nb.rows("NB_Global")[0];
    assert_eq!(parse_map(&g["external_ids"])[&fence_key(TAG)], "1:10");
    // 트랜잭션마다 1
    assert_eq!(g["nb_cfg"], 2);
    // 다시 견주면 할 일이 없다.
    assert!(plan(&want, &nb.read().await).is_empty());
    assert_eq!(nb.sync(&want, "1:11").await, 0);

    // 바꾸기: DHCP 옵션, 포트 주소, 포트 하나 빼기, 포트를 다른 네트워크로 옮기기, 스위치 칸
    want.switches[0].dhcp[0] = dhcp(n1, "10.0.0.0/24", "3600");
    want.switches[0].ports = vec![port("opl-vm1-0201", "10.0.0.9", Some("10.0.0.0/24"))];
    want.switches[1].ports = vec![port("opl-vm2-0202", "10.0.0.6", None)];
    want.switches[1]
        .external_ids
        .insert(NAME_KEY.into(), "renamed".into());
    let uuid_before = |name: &str| {
        nb.rows("Logical_Switch_Port")
            .iter()
            .find(|p| p["name"] == name)
            .map(|p| p["_uuid"].clone())
    };
    let moved = uuid_before("opl-vm2-0202");
    // 옛 스위치(n1)의 묶음이 먼저 돌아도 옮기는 포트는 새 스위치의 묶음만 옮긴다(리뷰 1 m-3).
    let batches = plan(&want, &nb.read().await);
    assert_eq!(batches[0].key, switch_name(n1));
    assert_eq!(nb.sync(&want, "1:12").await, 2);
    assert!(plan(&want, &nb.read().await).is_empty());
    // 옮긴 포트는 같은 행(같은 uuid)이다.
    assert_eq!(uuid_before("opl-vm2-0202"), moved);
    let have = nb.read().await;
    let s2 = have
        .switches
        .iter()
        .find(|s| s.name == switch_name(n2))
        .unwrap();
    assert_eq!(
        s2.ports,
        vec![parse_uuids(moved.as_ref().unwrap())[0].clone()]
    );
    assert_eq!(
        parse_map(&nb.rows("DHCP_Options")[0]["options"])["lease_time"],
        "3600"
    );

    // 누가 우리 포트를 지우면(`ovn-nbctl lsp-del`) 되살린다.
    nb.fake.with(|s| {
        let p = s.rows("Logical_Switch_Port");
        let u = p.iter().find(|p| p["name"] == "opl-vm1-0201").unwrap()["_uuid"].clone();
        for sw in s.tables.get_mut("Logical_Switch").unwrap().values_mut() {
            let kept: Vec<Value> = parse_uuids(&sw["ports"])
                .into_iter()
                .filter(|x| json!(["uuid", x]) != u)
                .map(|x| json!(["uuid", x]))
                .collect();
            sw.insert("ports".into(), json!(["set", kept]));
        }
        s.delete_where("Logical_Switch_Port", "name", &json!("opl-vm1-0201"));
    });
    assert_eq!(nb.sync(&want, "1:13").await, 1);
    assert!(plan(&want, &nb.read().await).is_empty());

    // 네트워크를 지우면 스위치·포트·DHCP가 사라지고 남의 행은 남는다.
    want.switches.remove(0);
    // 지울 스위치 하나, 지울 DHCP 행 묶음 하나
    assert_eq!(nb.sync(&want, "1:14").await, 2);
    let names: Vec<Value> = nb
        .rows("Logical_Switch")
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&json!("admin")) && names.contains(&json!("other")));
    assert!(nb.rows("DHCP_Options").is_empty());
    assert_eq!(nb.rows("Logical_Switch_Port").len(), 1);
    // 모두 지워도 남의 행은 남는다.
    assert_eq!(nb.sync(&NbState::default(), "1:15").await, 1);
    assert_eq!(nb.rows("Logical_Switch").len(), 2);
    assert!(nb.rows("Logical_Switch_Port").is_empty());
}

#[tokio::test]
async fn duplicate_switches_of_ours_are_collapsed() {
    let nb = Nb::new().await;
    let n = Uuid::now_v7();
    let want = NbState {
        port_groups: Vec::new(),
        load_balancers: Vec::new(),
        routers: Vec::new(),
        switches: vec![switch(n, vec![], vec![])],
    };
    nb.fake.with(|s| {
        for _ in 0..2 {
            s.insert(
                "Logical_Switch",
                json!({"name": switch_name(n), "external_ids": ["map", [[CLUSTER_KEY, TAG]]]}),
            );
        }
    });
    nb.sync(&want, "1:1").await;
    assert_eq!(nb.rows("Logical_Switch").len(), 1);
    assert!(plan(&want, &nb.read().await).is_empty());
}

#[tokio::test]
async fn fencing_refuses_a_writer_that_read_an_older_token() {
    let nb = Nb::new().await;
    let n = Uuid::now_v7();
    let want = NbState {
        port_groups: Vec::new(),
        load_balancers: Vec::new(),
        routers: Vec::new(),
        switches: vec![switch(n, vec![], vec![])],
    };
    nb.sync(&want, "1:5").await;
    // 옛 리더가 읽은 것
    let stale = nb.read().await;
    // 그 사이 새 리더가 썼다.
    nb.sync(&NbState::default(), "2:9").await;
    let mut stale_want = want.clone();
    stale_want
        .switches
        .push(switch(Uuid::now_v7(), vec![], vec![]));
    let (mut ops, _) = fence_ops(stale.global.as_ref(), TAG, "1:6");
    ops.extend(plan(&stale_want, &stale).remove(0).ops);
    let e = nb.client.transact(NB_DB, ops).await.unwrap_err();
    assert!(e.is_wait_failed(), "{e}");
    // 아무것도 바뀌지 않았다.
    assert!(nb.rows("Logical_Switch").is_empty());
    assert_eq!(
        parse_map(&nb.rows("NB_Global")[0]["external_ids"])[&fence_key(TAG)],
        "2:9"
    );
    // NB_Global이 없는 NB에 둘이 함께 넣으려 하면 하나만 된다.
    let fresh = Nb::new().await;
    let (a, _) = fence_ops(None, TAG, "1:1");
    let (b, _) = fence_ops(None, TAG, "2:1");
    fresh.client.transact(NB_DB, a).await.unwrap();
    assert!(
        fresh
            .client
            .transact(NB_DB, b)
            .await
            .unwrap_err()
            .is_wait_failed()
    );
    assert_eq!(fresh.rows("NB_Global").len(), 1);
}

/// 한 네트워크의 트랜잭션이 실패해도(남의 포트와 이름이 겹침) 다른 네트워크는 쓰인다(리뷰 1 I-6).
#[tokio::test]
async fn one_failing_network_does_not_block_the_others() {
    let nb = Nb::new().await;
    let (n1, n2) = (Uuid::now_v7(), Uuid::now_v7());
    let want = NbState {
        port_groups: Vec::new(),
        load_balancers: Vec::new(),
        routers: Vec::new(),
        switches: vec![
            switch(n1, vec![port("opl-taken", "10.0.0.5", None)], vec![]),
            switch(n2, vec![port("opl-free", "10.0.1.5", None)], vec![]),
        ],
    };
    // 관리자가 넣은 태그 없는 포트가 이름을 쓰고 있다.
    nb.fake.with(|s| {
        let p = s.insert("Logical_Switch_Port", json!({"name": "opl-taken"}));
        s.insert(
            "Logical_Switch",
            json!({"name": "admin", "ports": ["set", [["uuid", p]]]}),
        );
    });
    let have = nb.read().await;
    let batches = plan(&want, &have);
    assert_eq!(batches.len(), 2);
    let mut global = have.global.clone();
    let mut failed = Vec::new();
    for b in batches {
        let (mut ops, ext) = fence_ops(global.as_ref(), TAG, "1:1");
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
            Err(e) => failed.push((b.key, e.to_string())),
        }
    }
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].0, switch_name(n1));
    let names: Vec<Value> = nb
        .rows("Logical_Switch")
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert!(names.contains(&json!(switch_name(n2))), "{names:?}");
    assert!(!names.contains(&json!(switch_name(n1))), "{names:?}");
}

/// 서브넷이 있는 네트워크에서 주소가 없는 NIC의 포트는 꺼진다(MAC만인 포트 보안은 IP 위장을 막지 못한다, 리뷰 1 I-4).
#[test]
fn addressless_nics_on_subnetted_networks_are_disabled() {
    let mut net = sdn_net("tenant");
    net.spec.subnets = serde_json::from_value(json!([{ "cidr": "10.0.0.0/24" }])).unwrap();
    let vm = vm_on(
        "late",
        json!([
            { "network": "tenant", "model": "virtio", "mac": "52:54:00:00:00:01" },
            { "network": "tenant", "model": "virtio", "mac": "52:54:00:00:00:02", "ips": ["10.0.0.9"] }
        ]),
    );
    let want = desired(&[net], &[vm], &[], TAG);
    let ports = &want.switches[0].ports;
    let by_mac = |mac: &str| {
        ports
            .iter()
            .find(|p| p.addresses[0].starts_with(mac))
            .unwrap()
    };
    let bare = by_mac("52:54:00:00:00:01");
    assert_eq!(bare.addresses, ["52:54:00:00:00:01"]);
    assert_eq!(bare.enabled, Some(false));
    assert_eq!(by_mac("52:54:00:00:00:02").enabled, Some(true));
}

/// 5천 포트(네트워크 50개)를 빈 NB에 넣고, 다시 맞추는 견주기가 빠른지 잰다(목표 2초 안, 기록).
#[tokio::test]
async fn five_thousand_ports_resync_quickly() {
    let nb = Nb::new().await;
    let networks: Vec<Uuid> = (0..50).map(|_| Uuid::now_v7()).collect();
    let want = NbState {
        port_groups: Vec::new(),
        load_balancers: Vec::new(),
        routers: Vec::new(),
        switches: networks
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let cidr = format!("10.{i}.0.0/16");
                switch(
                    *n,
                    (0..100)
                        .map(|j| {
                            port(
                                &format!("opl-{n}-{j:012x}"),
                                &format!("10.{i}.0.{j}"),
                                Some(&cidr),
                            )
                        })
                        .collect(),
                    vec![dhcp(*n, &cidr, "86400")],
                )
            })
            .collect(),
    };
    let first = plan(&want, &nb.read().await);
    // 네트워크마다 한 트랜잭션이다.
    assert_eq!(first.len(), 50);
    assert_eq!(nb.sync(&want, "1:1").await, 50);
    assert_eq!(nb.rows("Logical_Switch_Port").len(), 5000);
    let started = Instant::now();
    let have = nb.read().await;
    let todo = plan(&want, &have);
    let took = started.elapsed();
    assert!(todo.is_empty());
    eprintln!("full resync of 5000 ports (read + diff): {took:?}");
    assert!(took < Duration::from_secs(10), "{took:?}");
}
