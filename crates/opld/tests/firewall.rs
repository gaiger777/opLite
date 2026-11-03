//! 계획 26 완료 기준: 보안 그룹을 건 NIC로 VM을 켜면 opld가 nftables 테이블 `bridge opl_<태그>`에 그
//! tap의 체인과 맵 원소를 넣고, 그룹 규칙을 바꾸면 곧바로 체인이 바뀐다. 그룹이 없는 NIC는 맵에 없다.
//! VM과 그룹을 지우면 테이블이 사라진다.
//! `cargo test -p opld --features qemu-it,net-it --test firewall`
//! Linux에서 root, iproute2, nftables, QEMU가 필요하다(KVM이 없으면 TCG).
#![cfg(all(feature = "qemu-it", feature = "net-it", target_os = "linux"))]

mod common;

use std::process::Command;

use common::{Daemon, eventually};
use opl_types::network::tap_name;
use serde_json::Value;
use uuid::Uuid;

/// 관리자가 미리 만든 브리지. 같은 호스트에서 병렬로 도는 다른 시험과 겹치지 않는 이름이다. 끝나면 지운다.
struct TestBridge(String);

impl TestBridge {
    fn new() -> Self {
        let name = format!("oplf{}", &Uuid::now_v7().simple().to_string()[22..]);
        for args in [
            vec!["link", "add", &name, "type", "bridge"],
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

/// 시험이 실패해 남은 이 클러스터의 테이블을 지운다. 데몬보다 먼저 만들어 데몬(과 그 에이전트)이 멈춘
/// 뒤에 지워지게 한다.
struct Table(Option<String>);

impl Drop for Table {
    fn drop(&mut self) {
        if let Some(tag) = &self.0 {
            let table = format!("opl_{tag}");
            let _ = Command::new("nft")
                .args(["delete", "table", "bridge", &table])
                .status();
        }
    }
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

/// `nft -j list table bridge opl_<tag>`의 항목들(테이블이 없으면 None)
fn table(tag: &str) -> Option<Vec<Value>> {
    let out = Command::new("nft")
        .args(["-j", "list", "table", "bridge", &format!("opl_{tag}")])
        .output()
        .unwrap();
    out.status.success().then(|| {
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["nftables"].as_array().unwrap().clone()
    })
}

/// 테이블의 체인 `name`의 규칙들. 체인이 없으면 None.
fn chain_rules(items: &[Value], name: &str) -> Option<Vec<Value>> {
    items.iter().any(|i| i["chain"]["name"] == name).then(|| {
        items
            .iter()
            .filter(|i| i["rule"]["chain"] == name)
            .map(|i| i["rule"]["expr"].clone())
            .collect()
    })
}

/// 맵 `map`의 원소에 장치 `tap`이 있는지
fn map_has(items: &[Value], map: &str, tap: &str) -> bool {
    items
        .iter()
        .filter(|i| i["map"]["name"] == map)
        .any(|i| i["map"]["elem"].to_string().contains(&format!("\"{tap}\"")))
}

/// 들어오기 체인 `i_<tap>`에 `th dport port` 규칙이 있는지
fn allows_port(items: &[Value], tap: &str, port: u16) -> bool {
    chain_rules(items, &format!("i_{tap}")).is_some_and(|rules| {
        rules.iter().any(|expr| {
            let text = expr.to_string();
            text.contains("\"dport\"") && text.contains(&format!("\"right\":{port}}}"))
        })
    })
}

#[tokio::test]
async fn security_groups_become_nftables_rules_for_vm_taps() {
    let br = TestBridge::new();
    let mut leftover = Table(None);
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("p26").await;
    d.login().await;
    let tag = tag_of(&d);
    leftover.0 = Some(tag.clone());

    // root로 도는 Linux opld는 nft가 있으면 방화벽을 알린다.
    let node = d.ctl_json(&["node", "get", "node1"]).await;
    assert_eq!(node["status"]["firewall"], true, "{node}");

    d.ctl(&["network", "create", "lan", "--bridge", &br.0])
        .await
        .unwrap();
    d.ctl(&[
        "security-group",
        "create",
        "web",
        "--rule",
        "in accept tcp 22",
    ])
    .await
    .unwrap();
    let out = d
        .ctl(&[
            "vm",
            "create",
            "web",
            "--memory",
            "256",
            "--nic",
            "lan,sg=web",
            "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    let web_tap = tap_name(vm["id"].as_str().unwrap().parse().unwrap(), 0);
    // 그룹이 없는 NIC(켜지 않아도 된다 — 에이전트는 배치와 상관없이 모든 VM을 본다)
    d.ctl(&["vm", "create", "plain", "--memory", "256", "--nic", "lan"])
        .await
        .unwrap();
    let plain = d.ctl_json(&["vm", "get", "plain"]).await;
    let plain_tap = tap_name(plain["id"].as_str().unwrap().parse().unwrap(), 0);

    // 워커가 켜기 직전에 규칙을 넣었다.
    let items = table(&tag).expect("the firewall table");
    for chain in [format!("o_{web_tap}"), format!("i_{web_tap}")] {
        assert!(chain_rules(&items, &chain).is_some(), "{chain}: {items:?}");
    }
    assert!(map_has(&items, "out_nics", &web_tap), "{items:?}");
    assert!(map_has(&items, "in_nics", &web_tap), "{items:?}");
    assert!(allows_port(&items, &web_tap, 22), "{items:?}");
    assert!(!map_has(&items, "out_nics", &plain_tap), "{items:?}");
    assert!(!map_has(&items, "in_nics", &plain_tap), "{items:?}");
    assert!(chain_rules(&items, &format!("i_{plain_tap}")).is_none());

    // 규칙을 바꾸면(PATCH) 실행 중인 VM의 체인이 곧바로 바뀐다(60초 주기를 기다리지 않는다).
    d.ctl(&["security-group", "rule", "add", "web", "in accept tcp 8080"])
        .await
        .unwrap();
    eventually("the web chain to allow tcp 8080", || async {
        table(&tag).is_some_and(|items| allows_port(&items, &web_tap, 8080))
    })
    .await;
    d.ctl(&["security-group", "rule", "rm", "web", "1"])
        .await
        .unwrap();
    eventually("the web chain to drop the tcp 22 rule", || async {
        table(&tag).is_some_and(|items| {
            !allows_port(&items, &web_tap, 22) && allows_port(&items, &web_tap, 8080)
        })
    })
    .await;

    // 쓰고 있는 그룹은 지울 수 없다.
    let err = d
        .ctl(&["security-group", "delete", "web"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("security_group_in_use"), "{err}");

    let out = d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(out.contains("vm web: stop done"), "{out}");
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["vm", "delete", "plain"]).await.unwrap();
    d.ctl(&["security-group", "delete", "web"]).await.unwrap();
    // 방화벽 NIC가 없으면 테이블을 지운다.
    eventually("the firewall table to go away", || async {
        table(&tag).is_none()
    })
    .await;
    d.ctl(&["network", "delete", "lan"]).await.unwrap();
    d.stop();
}
