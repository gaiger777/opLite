//! HA 검사 probe를 실제 인터페이스로 보는 통합 테스트. `cargo test -p opl-driver --features net-it --test host_check`
//! Linux에서 root(CAP_NET_ADMIN, `ip netns`)와 iproute2가 필요하다. 이름이 `oplh`로 시작하는 장치와
//! 시험용 네트워크 이름공간만 만들고 끝나면 지운다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

use std::process::Command;
use std::time::{Duration, Instant};

use opl_driver::{NetFs, interfaces, link_state};
use opl_types::ha_check::CheckState;
use opl_types::node::{BondState, InterfaceKind};
use uuid::Uuid;

/// 시험이 만든 장치와 네트워크 이름공간. 끝나면 지운다.
#[derive(Default)]
struct Devices {
    links: Vec<String>,
    netns: Vec<String>,
}

impl Drop for Devices {
    fn drop(&mut self) {
        for link in self.links.iter().rev() {
            let _ = Command::new("ip").args(["link", "del", link]).status();
        }
        for ns in &self.netns {
            let _ = Command::new("ip").args(["netns", "del", ns]).status();
        }
    }
}

fn suffix() -> String {
    Uuid::new_v4().simple().to_string()[..6].to_string()
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

/// `iface`의 `link` 판정이 `want`가 되기를 `limit`까지 기다린다.
async fn wait_state(iface: &str, want: CheckState, limit: Duration) {
    let net = NetFs::default();
    let started = Instant::now();
    loop {
        let probe = link_state(&net, iface);
        if probe.state == want {
            return;
        }
        assert!(
            started.elapsed() < limit,
            "{iface}: wanted {want:?}, got {probe:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn sees_real_links_bonds_and_interfaces() {
    let mut dev = Devices::default();
    let s = suffix();

    // veth의 짝을 이름공간 안에 두고 그쪽을 내리면 호스트 쪽 carrier가 0이 된다.
    let ns = format!("oplhc-{s}");
    ip(&["netns", "add", &ns]);
    dev.netns.push(ns.clone());
    let veth = format!("oplhc{s}");
    ip(&[
        "link", "add", &veth, "type", "veth", "peer", "name", "peer0", "netns", &ns,
    ]);
    dev.links.push(veth.clone());
    ip(&["link", "set", &veth, "up"]);
    ip(&["-n", &ns, "link", "set", "peer0", "up"]);
    wait_state(&veth, CheckState::Ok, Duration::from_secs(5)).await;
    ip(&["-n", &ns, "link", "set", "peer0", "down"]);
    wait_state(&veth, CheckState::Failing, Duration::from_secs(2)).await;

    // dummy는 operstate가 unknown이어도 통과다.
    let dummy = format!("oplhd{s}");
    ip(&["link", "add", &dummy, "type", "dummy"]);
    dev.links.push(dummy.clone());
    ip(&["link", "set", &dummy, "up"]);
    wait_state(&dummy, CheckState::Ok, Duration::from_secs(2)).await;

    // active-backup 본딩에 dummy 둘을 묶는다. bonding 모듈이 없는 커널이면 건너뛴다.
    let bond = format!("oplhb{s}");
    let ports = [format!("oplh1{s}"), format!("oplh2{s}")];
    let made = Command::new("ip")
        .args([
            "link",
            "add",
            &bond,
            "type",
            "bond",
            "mode",
            "active-backup",
            "miimon",
            "100",
        ])
        .output()
        .unwrap();
    let have_bond = made.status.success();
    if have_bond {
        dev.links.push(bond.clone());
        for port in &ports {
            ip(&["link", "add", port, "type", "dummy"]);
            dev.links.push(port.clone());
            ip(&["link", "set", port, "master", &bond]);
            ip(&["link", "set", port, "up"]);
        }
        ip(&["link", "set", &bond, "up"]);
        wait_state(&bond, CheckState::Ok, Duration::from_secs(5)).await;
        ip(&["link", "set", &ports[1], "down"]);
        wait_state(&bond, CheckState::Degraded, Duration::from_secs(2)).await;
        let probe = link_state(&NetFs::default(), &bond);
        assert_eq!(probe.bond.unwrap().state, BondState::Degraded);
        ip(&["link", "set", &ports[0], "down"]);
        wait_state(&bond, CheckState::Failing, Duration::from_secs(2)).await;
        let probe = link_state(&NetFs::default(), &bond);
        assert_eq!(probe.bond.unwrap().state, BondState::Down);
    } else {
        eprintln!(
            "cannot create a bond ({}); skipping the bond part",
            String::from_utf8_lossy(&made.stderr).trim()
        );
    }

    let json = ip(&["-j", "-d", "addr", "show"]);
    let list = interfaces(&NetFs::default(), &json).unwrap();
    let find = |name: &str| list.iter().find(|i| i.name == name);
    // 상한(64개)에 걸리지 않을 만큼 장치가 적은 호스트에서만 이름으로 찾는다.
    if list.len() < opl_types::node::MAX_NODE_INTERFACES {
        assert_eq!(find(&veth).unwrap().kind, InterfaceKind::Veth);
        assert_eq!(find(&veth).unwrap().carrier, Some(false));
        assert_eq!(find(&dummy).unwrap().kind, InterfaceKind::Dummy);
        if have_bond {
            let b = find(&bond).unwrap();
            assert_eq!(b.kind, InterfaceKind::Bond);
            assert_eq!(b.bond.as_ref().unwrap().ports.len(), 2);
            assert_eq!(find(&ports[0]).unwrap().master.as_deref(), Some(&*bond));
        }
    }
}
