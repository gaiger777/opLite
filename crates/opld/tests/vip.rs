//! 계획 46 내장 VIP: opld 둘(과 위트니스)이 같은 VIP를 설정하면 한 노드만 그 VIP를 인터페이스에 붙이고,
//! 그 노드를 멈추면 다른 노드가 붙인다. kill -9로 죽어 남은 VIP는 다시 시작할 때나 `orbivisord --release-vips`
//! (systemd `ExecStopPost`)가 뗀다.
//! `cargo test -p opld --features net-it --test vip`
//! Linux에서 root와 iproute2가 필요하다. 노드마다 따로 만든 dummy 인터페이스에 붙이므로(한 호스트에서 두
//! 노드를 구별하려고) 호스트의 다른 인터페이스는 건드리지 않는다. 주소는 문서용 192.0.2.0/24다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{Daemon, eventually};
use uuid::Uuid;

/// 시험이 만든 dummy 인터페이스. 끝나면 지운다(붙은 주소도 함께 사라진다).
struct Dummy(String);

impl Dummy {
    fn new() -> Self {
        let name = format!("oplv{}", &Uuid::new_v4().simple().to_string()[..8]);
        for args in [
            vec!["link", "add", &name, "type", "dummy"],
            vec!["link", "set", &name, "up"],
        ] {
            assert!(Command::new("ip").args(&args).status().unwrap().success());
        }
        Self(name)
    }

    /// 이 인터페이스에 `ip`가 붙어 있는지
    fn has(&self, ip: &str) -> bool {
        let out = Command::new("ip")
            .args(["-j", "-4", "addr", "show", "dev", &self.0])
            .output()
            .unwrap();
        let links: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        links[0]["addr_info"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a["local"] == ip))
    }
}

impl Drop for Dummy {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
}

fn vip_config(ip: &str, iface: &str) -> String {
    format!("[vip]\naddresses = [\"{ip}/24\"]\ninterface = \"{iface}\"\n")
}

/// 지금 VIP를 가진 노드(0·1). 둘 다면 실패다.
fn holder(ifaces: &[Dummy; 2], ip: &str) -> Option<usize> {
    let held: Vec<usize> = (0..2).filter(|i| ifaces[*i].has(ip)).collect();
    assert!(held.len() <= 1, "both nodes hold {ip}");
    held.first().copied()
}

/// 헬스 체크(`?ready`)가 200이다: 리더를 알고(리더에게서 로그를 받고 있음) 적용이 뒤처지지 않았다.
async fn ready(d: &Daemon) -> bool {
    reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap()
        .get(format!("{}/api/v1/health", d.server()))
        .send()
        .await
        .is_ok_and(|r| r.status() == reqwest::StatusCode::OK)
}

#[tokio::test]
async fn one_node_holds_the_vip_and_it_moves_when_that_node_stops() {
    let ip = format!("192.0.2.{}", 10 + Uuid::new_v4().as_u128() % 240);
    let ifaces = [Dummy::new(), Dummy::new()];
    let mut nodes = [
        Daemon::new("node1", &vip_config(&ip, &ifaces[0].0)),
        Daemon::new("node2", &vip_config(&ip, &ifaces[1].0)),
    ];
    // 한 노드가 멈춰도 과반수가 남게 위트니스를 둔다.
    let mut w = Daemon::witness("wit");
    for d in nodes.iter_mut() {
        d.start().await;
    }
    w.start().await;
    nodes[0].init("prod").await;
    nodes[0].login().await;
    let url = nodes[0].join_url.clone().unwrap();
    nodes[1].join(&url).await.unwrap();
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();

    eventually("a VIP holder", || async { holder(&ifaces, &ip).is_some() }).await;
    // 주인이 살아 있으면 보통 옮기지 않지만, 바쁜 CI에서 실행기 확인이 한 번 늦으면 헬스가 잠깐 503이 되어
    // 옮겨 갈 수 있다. 그래서 주인이 그대로인지 대신 두 노드가 함께 갖는 때가 없는지(`holder`) 본다.
    let watched = Instant::now();
    while watched.elapsed() < Duration::from_secs(3) {
        holder(&ifaces, &ip);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eventually("a VIP holder", || async { holder(&ifaces, &ip).is_some() }).await;
    let first = holder(&ifaces, &ip).unwrap();
    let other = 1 - first;

    // 정상 종료는 VIP를 떼고, 리스가 끝나면 다른 노드가 붙인다(리스 3초).
    let stopped = Instant::now();
    nodes[first].stop();
    assert!(!ifaces[first].has(&ip));
    eventually("the VIP moved", || async {
        holder(&ifaces, &ip) == Some(other)
    })
    .await;
    assert!(
        stopped.elapsed() < Duration::from_secs(15),
        "{:?}",
        stopped.elapsed()
    );

    // kill -9로 죽은 주인의 VIP는 인터페이스에 남는다. 다시 시작하면 리스를 받기 전에 뗀다.
    // 다시 시작한 노드가 멈춰 있던 동안의 로그를 받기 전에 다른 노드를 죽이면, 그 로그를 가진 위트니스가
    // 투표하지 않아(Raft 선거 규칙) 리더가 생기지 않는다. 그래서 준비될(리더에게서 로그를 받을) 때까지 둔다.
    nodes[first].start().await;
    eventually("the first node ready", || ready(&nodes[first])).await;
    nodes[other].kill();
    assert!(ifaces[other].has(&ip));
    eventually("the VIP back on the first node", || async {
        ifaces[first].has(&ip)
    })
    .await;
    nodes[other].start().await;
    eventually("the leftover VIP removed", || async {
        !ifaces[other].has(&ip)
    })
    .await;
    assert!(ifaces[first].has(&ip));

    // 프로세스가 없어도 `--release-vips`가 기록한 VIP를 뗀다(systemd `ExecStopPost`).
    nodes[first].kill();
    assert!(ifaces[first].has(&ip));
    let out = Command::new(env!("CARGO_BIN_EXE_orbivisord"))
        .arg("--config")
        .arg(nodes[first].path("opld.toml"))
        .arg("--release-vips")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(!ifaces[first].has(&ip));

    for d in nodes.iter_mut().filter(|d| d.is_running()) {
        d.stop();
    }
    w.stop();
}
