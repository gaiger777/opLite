//! 계획 25 완료 기준: VXLAN 네트워크를 만들면 opld가 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를
//! 만들어 브리지 목록에 알리고, 그 네트워크를 쓰는 VM의 NIC가 그 브리지에 VXLAN MTU로 붙는다. 네트워크를
//! 지우면 두 장치가 사라진다. 같은 호스트의 다른 클러스터 장치는 건드리지 않는다.
//! `cargo test -p opld --features qemu-it,net-it --test vxlan`
//! Linux에서 root, iproute2, QEMU가 필요하다(KVM이 없으면 TCG).
#![cfg(all(feature = "qemu-it", feature = "net-it", target_os = "linux"))]

mod common;

use std::process::Command;

use common::{Daemon, eventually};
use opl_types::network::{tap_name, vxlan_bridge, vxlan_device};
use serde_json::Value;
use uuid::Uuid;

/// 같은 호스트에서 병렬로 도는 다른 시험과 겹치지 않게 무작위로 고른 VNI
fn random_vni() -> u32 {
    1_000_000 + (Uuid::new_v4().as_u128() % 15_000_000) as u32
}

/// `ip -j link show dev NAME`(없으면 None). VXLAN 장치에 `-d`를 붙이면 Ubuntu의 iproute2(fan 패치)가
/// 깨진 JSON을 내므로 붙이지 않는다.
fn link(name: &str) -> Option<Value> {
    let out = Command::new("ip")
        .args(["-j", "link", "show", "dev", name])
        .output()
        .unwrap();
    out.status.success().then(|| {
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v[0].clone()
    })
}

/// `ip -d -o link show dev NAME`의 글(VXLAN의 id·local·dstport를 본다)
fn link_details(name: &str) -> String {
    let out = Command::new("ip")
        .args(["-d", "-o", "link", "show", "dev", name])
        .output()
        .unwrap();
    assert!(out.status.success(), "{name}");
    String::from_utf8(out.stdout).unwrap()
}

/// 클러스터 태그: CA 지문(`sha256:…`)의 16진수 앞 12자(opld의 `overlay::cluster_tag`와 같다)
fn tag_of(d: &Daemon) -> String {
    let fingerprint = d.ca_fingerprint.as_deref().unwrap();
    let hex = fingerprint.rsplit(':').next().unwrap();
    hex.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .take(12)
        .collect()
}

/// 시험이 실패해 opld가 지우지 못한 이 VNI의 장치를 지운다. 태그가 이 시험의 클러스터 것일 때만.
/// 데몬보다 먼저 만들어 데몬(과 그 에이전트)이 멈춘 뒤에 지워지게 한다.
struct Overlay {
    vni: u32,
    tag: Option<String>,
}

impl Drop for Overlay {
    fn drop(&mut self) {
        let Some(tag) = &self.tag else { return };
        for name in [vxlan_device(self.vni), vxlan_bridge(self.vni)] {
            if link(&name).is_some_and(|l| l["ifalias"] == format!("opl:{tag}").as_str()) {
                let _ = Command::new("ip").args(["link", "del", &name]).status();
            }
        }
    }
}

/// 노드가 알린 브리지 목록에 `bridge`가 있는지
async fn node_has_bridge(d: &Daemon, node: &str, bridge: &str) -> bool {
    let node = d.ctl_json(&["node", "get", node]).await;
    node["status"]["bridges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["name"] == bridge)
}

/// 이 VNI의 두 장치가 `tag`로 표시되어 바라는 모양으로 있는지
fn assert_devices(vni: u32, tag: &str) {
    let (br, vx) = (vxlan_bridge(vni), vxlan_device(vni));
    let alias = format!("opl:{tag}");
    let bridge = link(&br).unwrap();
    assert_eq!(bridge["ifalias"], alias.as_str(), "{bridge}");
    assert_eq!(bridge["mtu"], 1450, "{bridge}");
    let device = link(&vx).unwrap();
    assert_eq!(device["ifalias"], alias.as_str(), "{device}");
    assert_eq!(device["master"], br.as_str(), "{device}");
    assert_eq!(device["mtu"], 1450, "{device}");
    let details = link_details(&vx);
    assert!(details.contains(&format!("vxlan id {vni} ")), "{details}");
    assert!(details.contains("local 127.0.0.1 "), "{details}");
    assert!(details.contains("dstport 4789 "), "{details}");
}

#[tokio::test]
async fn vxlan_networks_get_devices_and_vm_nics_join_them() {
    let vni = random_vni();
    let other_vni = vni + 1;
    let (br, vx) = (vxlan_bridge(vni), vxlan_device(vni));
    let (other_br, other_vx) = (vxlan_bridge(other_vni), vxlan_device(other_vni));
    let mut ours = Overlay { vni, tag: None };
    let mut theirs = Overlay {
        vni: other_vni,
        tag: None,
    };
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("p25").await;
    d.login().await;
    let tag = tag_of(&d);
    ours.tag = Some(tag.clone());

    // underlay는 설정의 address(127.0.0.1)다.
    let node = d.ctl_json(&["node", "get", "node1"]).await;
    assert_eq!(node["status"]["underlay"], "127.0.0.1", "{node}");

    let vni_arg = vni.to_string();
    d.ctl(&["network", "create", "ov", "--vxlan", &vni_arg])
        .await
        .unwrap();
    // 에이전트가 watch로 깨어나 장치를 만들고 곧바로 브리지 목록을 기록한다(60초 주기를 기다리지 않는다).
    eventually(&format!("node bridges to have {br}"), || {
        node_has_bridge(&d, "node1", &br)
    })
    .await;
    assert_devices(vni, &tag);

    // 같은 VNI는 다시 쓸 수 없다.
    let err = d
        .ctl(&["network", "create", "ov2", "--vxlan", &vni_arg])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("vni_in_use"), "{err}");

    let out = d
        .ctl(&[
            "vm", "create", "web", "--memory", "256", "--nic", "ov", "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    let id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let tap = tap_name(id, 0);
    let tap_link = link(&tap).unwrap();
    assert_eq!(tap_link["master"], br.as_str(), "{tap_link}");
    assert_eq!(tap_link["mtu"], 1450, "{tap_link}");
    // 게스트도 MTU를 알게 virtio NIC에 host_mtu를 준다.
    let pid = vm["status"]["pid"].as_u64().unwrap();
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
    let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    assert!(cmdline.contains("host_mtu=1450"), "{cmdline}");

    // 같은 호스트의 다른 클러스터(다른 태그)의 에이전트는 이 장치를 지우지 않는다. 그 클러스터의
    // VXLAN 장치가 생겼으면 그 에이전트가 적어도 한 번 이 호스트의 장치를 훑은 것이다.
    let mut o = Daemon::new("other", "");
    o.start().await;
    o.init("p25-other").await;
    o.login().await;
    let other_tag = tag_of(&o);
    assert_ne!(other_tag, tag);
    theirs.tag = Some(other_tag.clone());
    o.ctl(&["network", "create", "ov", "--vxlan", &other_vni.to_string()])
        .await
        .unwrap();
    eventually("the other cluster's VXLAN bridge", || {
        node_has_bridge(&o, "other", &other_br)
    })
    .await;
    assert_devices(other_vni, &other_tag);
    assert_devices(vni, &tag);
    // 다른 클러스터 노드의 브리지 목록에는 이 클러스터의 브리지가 오르지 않는다(alias가 다르다).
    assert!(!node_has_bridge(&o, "other", &br).await);

    let out = d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(out.contains("vm web: stop done"), "{out}");
    eventually(&format!("tap {tap} to go away"), || async {
        link(&tap).is_none()
    })
    .await;
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["network", "delete", "ov"]).await.unwrap();
    eventually(&format!("{vx} and {br} to go away"), || async {
        link(&vx).is_none() && link(&br).is_none()
    })
    .await;
    eventually(&format!("node bridges to drop {br}"), || async {
        !node_has_bridge(&d, "node1", &br).await
    })
    .await;
    // 다른 클러스터의 장치는 남는다.
    assert_devices(other_vni, &other_tag);

    o.ctl(&["network", "delete", "ov"]).await.unwrap();
    eventually("the other cluster's VXLAN devices to go away", || async {
        link(&other_vx).is_none() && link(&other_br).is_none()
    })
    .await;
    o.stop();
    d.stop();
}
