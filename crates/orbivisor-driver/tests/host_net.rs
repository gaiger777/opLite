//! 실제 브리지와 tap을 만드는 통합 테스트. `cargo test -p orbivisor-driver --features net-it`
//! Linux에서 root(CAP_NET_ADMIN)와 iproute2가 필요하다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

use std::process::Command;

use orbivisor_driver::{DriverError, HostNet, IpRoute, Tap};
use orbivisor_types::network::tap_name;
use serde_json::Value;
use uuid::Uuid;

/// 테스트가 만든 브리지. 끝나면 지운다.
struct TestBridge(String);

impl TestBridge {
    fn new(vlan_filtering: bool) -> Self {
        // 테스트마다 다른 이름(15자 이하)
        let name = format!("oplt{}", &Uuid::now_v7().simple().to_string()[22..]);
        let filtering = if vlan_filtering { "1" } else { "0" };
        ip(&[
            "link",
            "add",
            &name,
            "type",
            "bridge",
            "vlan_filtering",
            filtering,
        ]);
        ip(&["link", "set", &name, "up"]);
        Self(name)
    }
}

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
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

fn master_of(ifname: &str) -> Option<String> {
    let out = Command::new("ip")
        .args(["-j", "link", "show", "dev", ifname])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let links: Value = serde_json::from_slice(&out.stdout).unwrap();
    links[0]["master"].as_str().map(str::to_string)
}

/// `bridge -j vlan show dev <tap>`의 (vid, pvid이면서 untagged인지)
fn vlans(ifname: &str) -> Vec<(u64, bool)> {
    let out = Command::new("bridge")
        .args(["-j", "vlan", "show", "dev", ifname])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v[0]["vlans"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            let flags: Vec<&str> = e["flags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            (
                e["vlan"].as_u64().unwrap(),
                flags.contains(&"PVID") && flags.contains(&"Egress Untagged"),
            )
        })
        .collect()
}

#[tokio::test]
async fn taps_join_bridges_with_vlans_and_are_removed_per_vm() {
    let aware = TestBridge::new(true);
    let plain = TestBridge::new(false);
    let net = IpRoute::default();

    let bridges = net.bridges().await.unwrap();
    let find = |name: &str| bridges.iter().find(|b| b.name == name).cloned();
    assert!(find(&aware.0).unwrap().vlan_filtering);
    assert!(!find(&plain.0).unwrap().vlan_filtering);

    let vm = Uuid::now_v7();
    let tagged = Tap {
        ifname: tap_name(vm, 0),
        bridge: aware.0.clone(),
        vlan: Some(10),
        ovn: None,
        qos: None,
    };
    let untagged = Tap {
        ifname: tap_name(vm, 1),
        bridge: plain.0.clone(),
        vlan: None,
        ovn: None,
        qos: None,
    };
    net.create_tap(&tagged).await.unwrap();
    net.create_tap(&untagged).await.unwrap();
    assert_eq!(master_of(&tagged.ifname), Some(aware.0.clone()));
    assert_eq!(master_of(&untagged.ifname), Some(plain.0.clone()));
    // 기본 VLAN 1은 빠지고 10만 태그 없이 주고받는다.
    assert_eq!(vlans(&tagged.ifname), vec![(10, true)]);

    // 이전 인스턴스가 남긴 tap이 있어도 다시 만든다.
    net.create_tap(&tagged).await.unwrap();
    assert_eq!(vlans(&tagged.ifname), vec![(10, true)]);

    // 다른 VM의 tap은 건드리지 않는다.
    let other = Uuid::now_v7();
    let other_tap = Tap {
        ifname: tap_name(other, 0),
        bridge: plain.0.clone(),
        vlan: None,
        ovn: None,
        qos: None,
    };
    net.create_tap(&other_tap).await.unwrap();
    net.delete_vm_taps(vm).await.unwrap();
    assert_eq!(master_of(&tagged.ifname), None);
    assert_eq!(master_of(&untagged.ifname), None);
    assert_eq!(master_of(&other_tap.ifname), Some(plain.0.clone()));
    // 없는 tap을 지워도 성공이다.
    net.delete_vm_taps(vm).await.unwrap();
    net.delete_vm_taps(other).await.unwrap();
    assert_eq!(master_of(&other_tap.ifname), None);
}

#[tokio::test]
async fn missing_or_vlan_unaware_bridges_are_refused() {
    let plain = TestBridge::new(false);
    let net = IpRoute::default();
    let vm = Uuid::now_v7();

    let missing = Tap {
        ifname: tap_name(vm, 0),
        bridge: "oplt-missing".into(),
        vlan: None,
        ovn: None,
        qos: None,
    };
    let err = net.create_tap(&missing).await.unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    let vlan_on_plain = Tap {
        ifname: tap_name(vm, 0),
        bridge: plain.0.clone(),
        vlan: Some(10),
        ovn: None,
        qos: None,
    };
    let err = net.create_tap(&vlan_on_plain).await.unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    // 거부하면 tap을 남기지 않는다.
    assert_eq!(master_of(&missing.ifname), None);
}

fn mtu_of(ifname: &str) -> u64 {
    let links: Value = serde_json::from_str(&ip(&["-j", "link", "show", "dev", ifname])).unwrap();
    links[0]["mtu"].as_u64().unwrap()
}

#[tokio::test]
async fn taps_take_the_bridge_mtu() {
    // VXLAN 브리지처럼 MTU를 낮춘 브리지
    let br = TestBridge::new(false);
    ip(&["link", "set", "dev", &br.0, "mtu", "1450"]);
    let net = IpRoute::default();
    let vm = Uuid::now_v7();
    let tap = Tap {
        ifname: tap_name(vm, 0),
        bridge: br.0.clone(),
        vlan: None,
        ovn: None,
        qos: None,
    };
    net.create_tap(&tap).await.unwrap();
    assert_eq!(master_of(&tap.ifname), Some(br.0.clone()));
    assert_eq!(mtu_of(&tap.ifname), 1450);
    assert_eq!(mtu_of(&br.0), 1450);
    net.delete_vm_taps(vm).await.unwrap();
}

fn tc(args: &[&str]) -> String {
    let out = Command::new("tc").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "tc {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// 계획 67 §8.2: 속도 제한이 있는 단순 tap은 만들 때 `clsact`와 우선순위 49152의 matchall police(보내기 = ingress,
/// 받기 = egress)를 단다. 실제 tc가 바이트 버스트를 받고, 되읽은 상태로 같은 값이면 그대로, 바꾸면 지우고 다시 넣으며,
/// 빼면 `clsact`도 지운다. 탐침은 시험 장치를 남기지 않는다.
#[tokio::test]
async fn tap_rate_limits_use_tc_police() {
    use orbivisor_types::qos::NicQos;
    let br = TestBridge::new(false);
    let net = IpRoute::default();
    assert!(net.probe_qos().await);
    assert!(
        Command::new("ip")
            .args(["link", "show", "oplqosprobe"])
            .output()
            .unwrap()
            .stdout
            .is_empty()
    );
    let vm = Uuid::now_v7();
    let qos = NicQos {
        out_mbps: Some(100),
        in_mbps: Some(50),
        ..Default::default()
    };
    let tap = Tap {
        ifname: tap_name(vm, 0),
        bridge: br.0.clone(),
        vlan: None,
        ovn: None,
        qos: Some(qos),
    };
    net.create_tap(&tap).await.unwrap();
    let dev = tap.ifname.as_str();
    assert!(tc(&["qdisc", "show", "dev", dev]).contains("clsact"));
    let ingress = tc(&["filter", "show", "dev", dev, "ingress"]);
    let egress = tc(&["filter", "show", "dev", dev, "egress"]);
    eprintln!("ingress:\n{ingress}egress:\n{egress}");
    assert!(
        ingress.contains("pref 49152") && ingress.contains("rate 100Mbit"),
        "{ingress}"
    );
    assert!(
        egress.contains("pref 49152") && egress.contains("rate 50Mbit"),
        "{egress}"
    );
    let index = |hook: &str| {
        let out = tc(&["filter", "show", "dev", dev, hook]);
        let words: Vec<String> = out.split_whitespace().map(String::from).collect();
        let i = words.iter().position(|w| w == "police").unwrap();
        words[i + 1].clone()
    };
    let (i0, e0) = (index("ingress"), index("egress"));
    net.set_tap_qos(dev, Some(&qos)).await.unwrap();
    assert_eq!(
        (index("ingress"), index("egress")),
        (i0.clone(), e0.clone())
    );
    let slower = NicQos {
        out_mbps: Some(10),
        ..qos
    };
    net.set_tap_qos(dev, Some(&slower)).await.unwrap();
    assert_ne!(index("ingress"), i0);
    assert_eq!(index("egress"), e0);
    assert!(tc(&["filter", "show", "dev", dev, "ingress"]).contains("rate 10Mbit"));
    net.set_tap_qos(dev, None).await.unwrap();
    assert!(!tc(&["qdisc", "show", "dev", dev]).contains("clsact"));
    net.delete_vm_taps(vm).await.unwrap();
}
