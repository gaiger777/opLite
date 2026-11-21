//! 계획 67 §9.5 옮기기 다리의 바라는 행과 가짜 NB 적용: 대상 스위치의 `l2gateway` 포트(다리 노드 chassis·physnet·원본
//! VLAN)와 다리 동안 IPv4 DHCP의 `router`, 다시 맞추면 연산 0개, 다리를 걷거나 끝내면 계획 66 행 그대로.

use opl_types::network::Network;
use opl_types::network_migration::{MigrationBridge, MigrationPhase, NetworkMigration};
use opl_types::resource::Resource;
use serde_json::json;
use uuid::Uuid;

use super::migration::{add_bridges, l2gateway_port_name};
use super::nb::*;
use super::routing_tests::net;
use super::tests::{Nb, TAG};

fn networks() -> Vec<Network> {
    vec![
        net("lan", json!({ "bridge": "vmbr0", "vlan": 20 })),
        net(
            "t-lan",
            json!({ "sdn": {}, "subnets": [{ "cidr": "10.66.0.0/24", "gateway": "10.66.0.254" }, { "cidr": "2001:db8:66::/64" }] }),
        ),
    ]
}

fn migration(bridge: Option<Uuid>) -> NetworkMigration {
    let mut m: NetworkMigration = Resource::new(
        "m",
        serde_json::from_value(json!({ "source": "lan", "target": "t-lan" })).unwrap(),
    );
    m.status.phase = MigrationPhase::Running;
    m.status.bridge = bridge.map(|node| MigrationBridge {
        node,
        physnet: "mig0123abcd".into(),
    });
    m
}

fn state(nets: &[Network], migrations: &[NetworkMigration]) -> NbState {
    let mut s = desired(nets, &[], &[], TAG);
    add_bridges(&mut s, migrations, nets, TAG);
    s
}

#[test]
fn a_laid_bridge_is_an_l2gateway_port_and_a_dhcp_router() {
    let nets = networks();
    let node = Uuid::now_v7();
    let m = migration(Some(node));
    let plain = state(&nets, &[]);
    let with = state(&nets, std::slice::from_ref(&m));
    let sw = &with.switches[0];
    let port = sw
        .ports
        .iter()
        .find(|p| p.name == l2gateway_port_name(m.id))
        .unwrap();
    assert_eq!(port.kind, "l2gateway");
    assert_eq!(port.addresses, ["unknown"]);
    assert_eq!(port.tag, Some(20));
    assert_eq!(port.options["network_name"], "mig0123abcd");
    assert_eq!(port.options["l2gateway-chassis"], node.to_string());
    assert_eq!(port.external_ids[CLUSTER_KEY], TAG);
    assert_eq!(port.external_ids[ID_KEY], m.id.to_string());
    let v4 = sw.dhcp.iter().find(|d| d.cidr == "10.66.0.0/24").unwrap();
    assert_eq!(v4.options["router"], "10.66.0.254");
    let plain_v4 = plain.switches[0]
        .dhcp
        .iter()
        .find(|d| d.cidr == "10.66.0.0/24")
        .unwrap();
    assert!(!plain_v4.options.contains_key("router"));
    // 다리가 없거나 끝난 옮기기는 행이 그대로다.
    let mut done = m.clone();
    done.status.phase = MigrationPhase::Finished;
    assert_eq!(state(&nets, &[migration(None), done]), plain);
    // 원본이 없어지면 다리를 그리지 않는다.
    assert_eq!(
        state(&nets[1..], std::slice::from_ref(&m)),
        state(&nets[1..], &[])
    );
}

#[tokio::test]
async fn the_bridge_is_written_once_and_taken_away() {
    let nb = Nb::new().await;
    let nets = networks();
    let m = migration(Some(Uuid::now_v7()));
    nb.sync(&state(&nets, &[]), "1:1").await;
    let with = state(&nets, std::slice::from_ref(&m));
    assert!(nb.sync(&with, "1:2").await > 0);
    assert!(plan(&with, &nb.read().await).is_empty());
    let ports = nb.rows("Logical_Switch_Port");
    let l2 = ports
        .iter()
        .find(|p| p["name"] == l2gateway_port_name(m.id).as_str())
        .unwrap();
    assert_eq!(l2["type"], "l2gateway");
    assert_eq!(l2["tag"], 20);
    // 걷으면 포트와 DHCP router가 사라진다.
    let plain = state(&nets, &[]);
    nb.sync(&plain, "1:3").await;
    assert!(plan(&plain, &nb.read().await).is_empty());
    assert!(
        !nb.rows("Logical_Switch_Port")
            .iter()
            .any(|p| p["name"] == l2gateway_port_name(m.id).as_str())
    );
}
