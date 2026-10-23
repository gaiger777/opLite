//! M7 완료 기준: Linux에서 브리지 네트워크를 만들고, 그 네트워크를 쓰는 VM의 NIC가 VLAN 태그와
//! 함께 브리지에 붙었다가 VM을 끄면 떨어진다.
//! `cargo test -p opld --features qemu-it,net-it --test bridge`
//! Linux에서 root, iproute2, QEMU가 필요하다(KVM이 없으면 TCG).
#![cfg(all(feature = "qemu-it", feature = "net-it", target_os = "linux"))]

mod common;

use std::process::Command;
use std::time::Duration;

use common::{Daemon, WAIT};
use opl_types::network::tap_name;
use serde_json::Value;
use uuid::Uuid;

/// 관리자가 미리 만든 VLAN-aware 브리지. 끝나면 지운다.
struct TestBridge(String);

impl TestBridge {
    fn new() -> Self {
        let name = format!("oplb{}", &Uuid::now_v7().simple().to_string()[22..]);
        for args in [
            vec![
                "link",
                "add",
                &name,
                "type",
                "bridge",
                "vlan_filtering",
                "1",
            ],
            vec!["link", "set", &name, "up"],
        ] {
            assert!(Command::new("ip").args(&args).status().unwrap().success());
        }
        Self(name)
    }
}

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
}

fn json(program: &str, args: &[&str]) -> Option<Value> {
    let out = Command::new(program).args(args).output().unwrap();
    out.status
        .success()
        .then(|| serde_json::from_slice(&out.stdout).unwrap())
}

fn master_of(ifname: &str) -> Option<String> {
    let v = json("ip", &["-j", "link", "show", "dev", ifname])?;
    v[0]["master"].as_str().map(str::to_string)
}

#[tokio::test]
async fn vm_nics_join_the_bridge_with_their_vlan() {
    let br = TestBridge::new();
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("m7").await;
    d.login().await;

    // 노드가 브리지를 알린다.
    let node = d.ctl_json(&["node", "get", "node1"]).await;
    let bridges = node["status"]["bridges"].as_array().unwrap();
    assert!(
        bridges
            .iter()
            .any(|b| b["name"] == br.0.as_str() && b["vlan_filtering"] == true),
        "{node}"
    );
    assert!(
        node["status"]["hypervisor"]
            .as_str()
            .is_some_and(|h| h == "qemu-kvm" || h == "qemu-tcg")
    );

    d.ctl(&[
        "network", "create", "lan", "--bridge", &br.0, "--vlan", "30",
    ])
    .await
    .unwrap();
    let out = d
        .ctl(&[
            "vm", "create", "web", "--memory", "256", "--nic", "lan", "--nic", "user", "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    let id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let tap = tap_name(id, 0);
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    let vlans = json("bridge", &["-j", "vlan", "show", "dev", &tap]).unwrap();
    let vlans = &vlans[0]["vlans"];
    assert_eq!(vlans.as_array().unwrap().len(), 1, "{vlans}");
    assert_eq!(vlans[0]["vlan"], 30);

    // 쓰고 있는 네트워크는 지울 수 없다.
    let err = d.ctl(&["network", "delete", "lan"]).await.unwrap_err();
    assert!(err.to_string().contains("network_in_use"), "{err}");

    let out = d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(out.contains("vm web: stop done"), "{out}");
    let deadline = tokio::time::Instant::now() + WAIT;
    while master_of(&tap).is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "tap {tap} is still there"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["network", "delete", "lan"]).await.unwrap();
    d.stop();
}
