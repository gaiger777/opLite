//! 실제 VXLAN 장치를 네트워크 이름공간 안에 만드는 통합 테스트. `cargo test -p opl-driver --features net-it`
//! Linux에서 root(CAP_NET_ADMIN, `ip netns`)와 iproute2, ping이 필요하다. 노드와 VM을 이름공간으로 흉내
//! 내므로 호스트 네트워크는 건드리지 않는다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::process::Command;
use std::time::Duration;

use opl_driver::{DriverError, HostNet, IpRoute, Overlay};
use serde_json::Value;
use uuid::Uuid;

/// 테스트가 만든 네트워크 이름공간. 끝나면 지운다(안의 장치도 함께 사라진다).
struct Netns(String);

impl Netns {
    fn new() -> Self {
        let name = format!("oplt-{}", random_hex(10));
        ip(&["netns", "add", &name]);
        let ns = Self(name);
        ip(&["-n", &ns.0, "link", "set", "lo", "up"]);
        ns
    }

    fn net(&self, tag: &str) -> IpRoute {
        IpRoute::in_netns(&self.0).with_cluster_tag(tag)
    }
}

impl Drop for Netns {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["netns", "del", &self.0]).status();
    }
}

fn random_hex(n: usize) -> String {
    Uuid::new_v4().simple().to_string()[..n].to_string()
}

fn random_vni() -> u32 {
    1_000_000 + (Uuid::new_v4().as_u128() % 1_000_000) as u32
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

/// 이름공간 `ns`의 장치 `ifname`(`ip -j link show dev`). 없으면 None. Ubuntu iproute2(fan 패치)는
/// VXLAN 장치의 `-j -d` JSON을 깨뜨리므로 `-d`를 쓰지 않는다.
fn link(ns: &Netns, ifname: &str) -> Option<Value> {
    let out = Command::new("ip")
        .args(["-n", &ns.0, "-j", "link", "show", "dev", ifname])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let links: Value = serde_json::from_slice(&out.stdout).unwrap();
    Some(links[0].clone())
}

/// VXLAN 장치의 자세한 텍스트(`ip -d -o link show dev`, 한 줄)
fn link_details(ns: &Netns, ifname: &str) -> String {
    ip(&["-n", &ns.0, "-d", "-o", "link", "show", "dev", ifname])
}

/// VXLAN 장치의 전부 0 FDB 항목의 dst
fn default_fdb(ns: &Netns, dev: &str) -> BTreeSet<IpAddr> {
    let out = Command::new("bridge")
        .args(["-n", &ns.0, "-j", "fdb", "show", "dev", dev])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["mac"] == "00:00:00:00:00:00")
        .filter_map(|e| e["dst"].as_str()?.parse().ok())
        .collect()
}

/// `ns1`의 `if1`과 `ns2`의 `if2`를 veth로 잇는다.
fn veth(ns1: &Netns, if1: &str, ns2: &Netns, if2: &str) {
    ip(&[
        "-n", &ns1.0, "link", "add", if1, "type", "veth", "peer", "name", if2, "netns", &ns2.0,
    ]);
}

/// `ns`에서 `to`로 ping이 통할 때까지 몇 번 해 본다.
async fn wait_ping(ns: &Netns, to: &str) {
    for _ in 0..10 {
        let ok = Command::new("ip")
            .args(["netns", "exec", &ns.0, "ping", "-c1", "-W1", to])
            .output()
            .unwrap()
            .status
            .success();
        if ok {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{} cannot ping {to}", ns.0);
}

fn overlay(vni: u32, local: IpAddr, peers: &[IpAddr]) -> Overlay {
    Overlay {
        vni,
        mtu: 1450,
        local,
        peers: peers.to_vec(),
    }
}

#[tokio::test]
async fn overlay_connects_vms_across_nodes() {
    let (node_a, node_b) = (Netns::new(), Netns::new());
    let (vm_a, vm_b) = (Netns::new(), Netns::new());
    let tag = random_hex(12);
    let alias = format!("opl:{tag}");
    let vni = random_vni();
    let (br, vx) = (format!("oplv{vni}"), format!("oplx{vni}"));

    // underlay: 두 노드를 veth로 잇는다.
    let bytes = Uuid::new_v4().into_bytes();
    let subnet = format!("10.{}.{}", bytes[0], bytes[1]);
    let (ua, ub): (IpAddr, IpAddr) = (
        format!("{subnet}.1").parse().unwrap(),
        format!("{subnet}.2").parse().unwrap(),
    );
    veth(&node_a, "u0", &node_b, "u0");
    for (ns, addr) in [(&node_a, ua), (&node_b, ub)] {
        ip(&[
            "-n",
            &ns.0,
            "addr",
            "add",
            &format!("{addr}/24"),
            "dev",
            "u0",
        ]);
        ip(&["-n", &ns.0, "link", "set", "u0", "up"]);
    }
    wait_ping(&node_a, &ub.to_string()).await;

    let (net_a, net_b) = (node_a.net(&tag), node_b.net(&tag));
    let changes = net_a
        .sync_overlays(&tag, &[overlay(vni, ua, &[ub])])
        .await
        .unwrap();
    assert!(changes.devices_changed);
    let changes = net_b
        .sync_overlays(&tag, &[overlay(vni, ub, &[ua])])
        .await
        .unwrap();
    assert!(changes.devices_changed);

    for ns in [&node_a, &node_b] {
        let bridge = link(ns, &br).unwrap();
        assert_eq!(bridge["ifalias"], alias.as_str());
        assert_eq!(bridge["mtu"], 1450);
        let dev = link(ns, &vx).unwrap();
        assert_eq!(dev["ifalias"], alias.as_str());
        assert_eq!(dev["mtu"], 1450);
        assert_eq!(dev["master"], br.as_str());
        // fan 패치가 있으면 "vxlan id N fan-map local …"
        let details = link_details(ns, &vx);
        assert!(details.contains(&format!(" vxlan id {vni} ")), "{details}");
        assert!(details.contains(" dstport 4789 "), "{details}");
    }
    assert_eq!(default_fdb(&node_a, &vx), BTreeSet::from([ub]));
    // 새 브리지가 브리지 목록에 보인다. 다른 클러스터(태그)의 브리지 목록에는 보이지 않는다.
    let bridges = net_a.bridges().await.unwrap();
    assert!(bridges.iter().any(|b| b.name == br && !b.vlan_filtering));
    let bridges = node_a.net(&random_hex(12)).bridges().await.unwrap();
    assert!(!bridges.iter().any(|b| b.name == br));

    // "VM"을 각 노드의 브리지에 붙이고 서로 ping한다.
    for (node, vm, addr) in [
        (&node_a, &vm_a, "192.168.77.1/24"),
        (&node_b, &vm_b, "192.168.77.2/24"),
    ] {
        veth(node, "vm0", vm, "eth0");
        ip(&["-n", &node.0, "link", "set", "vm0", "master", &br, "up"]);
        ip(&["-n", &vm.0, "addr", "add", addr, "dev", "eth0"]);
        ip(&["-n", &vm.0, "link", "set", "eth0", "mtu", "1450", "up"]);
    }
    wait_ping(&vm_a, "192.168.77.2").await;

    // 다시 맞춰도 바뀌는 장치가 없다.
    let changes = net_a
        .sync_overlays(&tag, &[overlay(vni, ua, &[ub])])
        .await
        .unwrap();
    assert!(!changes.devices_changed);
    assert_eq!(default_fdb(&node_a, &vx), BTreeSet::from([ub]));

    // 피어를 빼면 FDB 항목만 사라진다.
    let changes = net_a
        .sync_overlays(&tag, &[overlay(vni, ua, &[])])
        .await
        .unwrap();
    assert!(!changes.devices_changed);
    assert!(default_fdb(&node_a, &vx).is_empty());
    assert!(link(&node_a, &vx).is_some());

    // "VM"이 붙어 있는 동안에는 바라는 것을 비워도 브리지와 VXLAN 장치를 남기고 오류다.
    match net_a.sync_overlays(&tag, &[]).await.unwrap_err() {
        DriverError::Overlay(msg) => {
            assert!(
                msg.contains(&format!("bridge {br} still has ports: vm0")),
                "{msg}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(link(&node_a, &vx).is_some());
    assert!(link(&node_a, &br).is_some());

    // "VM"을 떼고 바라는 것을 비우면 두 장치가 사라진다.
    ip(&["-n", &node_a.0, "link", "del", "vm0"]);
    let changes = net_a.sync_overlays(&tag, &[]).await.unwrap();
    assert!(changes.devices_changed);
    assert!(link(&node_a, &vx).is_none());
    assert!(link(&node_a, &br).is_none());
    // 다른 노드는 그대로다.
    assert!(link(&node_b, &vx).is_some());
}

#[tokio::test]
async fn devices_of_others_are_left_alone() {
    let node = Netns::new();
    let tag = random_hex(12);
    let local: IpAddr = "10.255.0.1".parse().unwrap();
    let (foreign, mine) = (random_vni(), random_vni() + 1_000_000);
    let half = mine + 1;
    let foreign_br = format!("oplv{foreign}");
    // 다른 클러스터(또는 관리자)가 만든 같은 모양 이름의 장치
    ip(&[
        "-n",
        &node.0,
        "link",
        "add",
        "name",
        &foreign_br,
        "type",
        "bridge",
    ]);
    ip(&[
        "-n",
        &node.0,
        "link",
        "set",
        "dev",
        &foreign_br,
        "alias",
        "opl:other",
    ]);

    // 만들다 멈춘 내 장치(alias도 포트도 없음)는 다시 만든다.
    let half_br = format!("oplv{half}");
    ip(&[
        "-n", &node.0, "link", "add", "name", &half_br, "type", "bridge",
    ]);

    let net = node.net(&tag);
    let desired = [
        overlay(foreign, local, &[]),
        overlay(mine, local, &[]),
        overlay(half, local, &[]),
    ];
    let err = net.sync_overlays(&tag, &desired).await.unwrap_err();
    // 그 네트워크만 실패하고 다른 네트워크는 만든다.
    match err {
        DriverError::Overlay(msg) => {
            assert!(msg.contains(&foreign.to_string()), "{msg}");
            assert!(!msg.contains(&mine.to_string()), "{msg}");
            assert!(!msg.contains(&half.to_string()), "{msg}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(link(&node, &foreign_br).unwrap()["ifalias"], "opl:other");
    assert!(link(&node, &format!("oplx{foreign}")).is_none());
    assert!(link(&node, &format!("oplv{mine}")).is_some());
    assert!(link(&node, &format!("oplx{mine}")).is_some());
    let alias = format!("opl:{tag}");
    assert_eq!(link(&node, &half_br).unwrap()["ifalias"], alias.as_str());
    assert!(link(&node, &format!("oplx{half}")).is_some());
    // 브리지 목록에는 내 브리지만 보인다.
    let names: Vec<String> = net
        .bridges()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(names.contains(&format!("oplv{mine}")), "{names:?}");
    assert!(!names.contains(&foreign_br), "{names:?}");

    // 정리는 내 장치만 지운다.
    let changes = net.sync_overlays(&tag, &[]).await.unwrap();
    assert!(changes.devices_changed);
    assert!(link(&node, &format!("oplv{mine}")).is_none());
    assert!(link(&node, &format!("oplx{mine}")).is_none());
    assert!(link(&node, &half_br).is_none());
    assert_eq!(link(&node, &foreign_br).unwrap()["ifalias"], "opl:other");
}
