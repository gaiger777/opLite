//! 실제 브리지와 tap을 만드는 통합 테스트. `cargo test -p opl-driver --features net-it`
//! Linux에서 root(CAP_NET_ADMIN)와 iproute2가 필요하다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

use std::process::Command;

use opl_driver::{DriverError, HostNet, IpRoute, Tap};
use opl_types::network::tap_name;
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
    let net = IpRoute;

    let bridges = net.bridges().await.unwrap();
    let find = |name: &str| bridges.iter().find(|b| b.name == name).cloned();
    assert!(find(&aware.0).unwrap().vlan_filtering);
    assert!(!find(&plain.0).unwrap().vlan_filtering);

    let vm = Uuid::now_v7();
    let tagged = Tap {
        ifname: tap_name(vm, 0),
        bridge: aware.0.clone(),
        vlan: Some(10),
    };
    let untagged = Tap {
        ifname: tap_name(vm, 1),
        bridge: plain.0.clone(),
        vlan: None,
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
    let net = IpRoute;
    let vm = Uuid::now_v7();

    let missing = Tap {
        ifname: tap_name(vm, 0),
        bridge: "oplt-missing".into(),
        vlan: None,
    };
    let err = net.create_tap(&missing).await.unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    let vlan_on_plain = Tap {
        ifname: tap_name(vm, 0),
        bridge: plain.0.clone(),
        vlan: Some(10),
    };
    let err = net.create_tap(&vlan_on_plain).await.unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    // 거부하면 tap을 남기지 않는다.
    assert_eq!(master_of(&missing.ifname), None);
}
