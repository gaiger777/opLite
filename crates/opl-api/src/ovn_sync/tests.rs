//! OvnSync의 순수 부분을 가짜 NB 서버(`opl_driver::ovn::FakeOvsdb`)에 실제로 적용해 본다: 견주기·넣기·바꾸기·지우기,
//! 다시 견주면 할 일 없음, 남의 행, 펜싱, 나누기, 규모.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use opl_driver::ovn::ovsdb::{DEFAULT_TIMEOUT, parse_map, parse_strings, parse_uuids};
use opl_driver::ovn::{Endpoint, FakeOvsdb, NB_DB, OvsdbClient, Transport};
use opl_types::network::{NetworkSpec, SdnNet, Vxlan};
use opl_types::resource::Resource;
use serde_json::{Value, json};
use uuid::Uuid;

use super::nb::*;

const TAG: &str = "abcdef012345";

fn ids(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn spec() -> NetworkSpec {
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

fn sdn_net(name: &str) -> opl_types::network::Network {
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
        addresses: vec![format!("02:00:00:00:00:01 {ip}")],
        port_security: vec![format!("02:00:00:00:00:01 {ip}")],
        dhcpv4: dhcp.map(String::from),
        enabled: Some(true),
        options: BTreeMap::new(),
        external_ids: ids(&[(CLUSTER_KEY, TAG)]),
    }
}

fn switch(network: Uuid, ports: Vec<PortRow>, dhcp: Vec<DhcpRow>) -> SwitchRow {
    SwitchRow {
        network,
        name: switch_name(network),
        external_ids: ids(&[(CLUSTER_KEY, TAG), (ID_KEY, &network.to_string())]),
        ports,
        dhcp,
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

struct Nb {
    fake: FakeOvsdb,
    client: OvsdbClient,
}

impl Nb {
    async fn new() -> Self {
        let fake = FakeOvsdb::start(NB_DB).await;
        let client =
            OvsdbClient::connect(&Transport::Tcp, &Endpoint::ip(fake.addr()), DEFAULT_TIMEOUT)
                .await
                .unwrap();
        Self { fake, client }
    }

    async fn read(&self) -> NbActual {
        parse_actual(&self.client.transact(NB_DB, read_ops(TAG)).await.unwrap())
    }

    /// 견주어 쓰고(펜싱 포함) 쓴 트랜잭션 수를 돌려준다.
    async fn sync(&self, want: &NbState, token: &str) -> usize {
        let have = self.read().await;
        let mut global = have.global.clone();
        let txs = batches(plan(want, &have));
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

    fn rows(&self, table: &str) -> Vec<serde_json::Map<String, Value>> {
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
    let want = desired(&[foldered.clone(), vxlan, b.clone(), a.clone()], TAG);
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
    assert_eq!(want, desired(&[a, b, foldered], TAG));
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
    assert_eq!(nb.sync(&want, "1:10").await, 1);
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
    assert_eq!(g["nb_cfg"], 1);
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
    assert_eq!(nb.sync(&want, "1:12").await, 1);
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
    assert_eq!(nb.sync(&want, "1:14").await, 1);
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
    ops.extend(batches(plan(&stale_want, &stale)).remove(0).ops);
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

#[test]
fn large_plans_are_split_per_network() {
    let small = vec![
        Batch {
            key: "a".into(),
            ops: vec![json!({}); 10],
        },
        Batch {
            key: "b".into(),
            ops: vec![json!({}); 10],
        },
    ];
    let one = batches(small);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].ops.len(), 20);
    let big = vec![
        Batch {
            key: "a".into(),
            ops: vec![json!({}); 1500],
        },
        Batch {
            key: "b".into(),
            ops: vec![json!({}); 1500],
        },
    ];
    let split = batches(big);
    assert_eq!(split.len(), 2);
    assert!(batches(Vec::new()).is_empty());
}

/// 5천 포트(네트워크 50개)를 빈 NB에 넣고, 다시 맞추는 견주기가 빠른지 잰다(목표 2초 안, 기록).
#[tokio::test]
async fn five_thousand_ports_resync_quickly() {
    let nb = Nb::new().await;
    let networks: Vec<Uuid> = (0..50).map(|_| Uuid::now_v7()).collect();
    let want = NbState {
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
    let first = batches(plan(&want, &nb.read().await));
    // 5천 포트 + 50 스위치 + 50 DHCP는 2000 연산을 넘으므로 네트워크마다 나뉜다.
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
