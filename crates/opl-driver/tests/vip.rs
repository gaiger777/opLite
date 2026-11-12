//! 내장 VIP를 실제 인터페이스에 붙이고 떼는 통합 테스트. `cargo test -p opl-driver --features net-it --test vip`
//! Linux에서 root(CAP_NET_ADMIN, `ip netns`)와 iproute2가 필요하다. 네트워크 이름공간 안의 dummy
//! 인터페이스만 쓰므로 호스트 네트워크는 건드리지 않는다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

use std::process::Command;

use opl_driver::{IpVip, VipAddress, VipNet};
use uuid::Uuid;

/// 시험이 만든 네트워크 이름공간과 그 안의 dummy 인터페이스 `vip0`. 끝나면 지운다.
struct Netns(String);

impl Netns {
    fn new() -> Self {
        let name = format!("oplt-{}", &Uuid::new_v4().simple().to_string()[..10]);
        ip(&["netns", "add", &name]);
        let ns = Self(name);
        ip(&["-n", &ns.0, "link", "add", "vip0", "type", "dummy"]);
        ip(&["-n", &ns.0, "link", "set", "vip0", "up"]);
        ns
    }

    /// `vip0`의 IPv4 주소(`a.b.c.d/nn`). 주소가 없으면 `ip -j`가 `addr_info`를 빼므로 빈 목록이다.
    fn addrs(&self) -> Vec<String> {
        let out = ip(&["-n", &self.0, "-j", "-4", "addr", "show", "dev", "vip0"]);
        let links: serde_json::Value = serde_json::from_str(&out).unwrap();
        links[0]["addr_info"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| format!("{}/{}", a["local"].as_str().unwrap(), a["prefixlen"]))
            .collect()
    }
}

impl Drop for Netns {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["netns", "del", &self.0]).status();
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

#[tokio::test]
async fn adds_and_removes_a_vip_idempotently() {
    let ns = Netns::new();
    let net = IpVip::in_netns(&ns.0);
    let vip: VipAddress = "192.0.2.200/24".parse().unwrap();
    assert!(ns.addrs().is_empty());

    assert!(net.add("vip0", vip).await.unwrap());
    assert_eq!(ns.addrs(), ["192.0.2.200/24"]);
    // 이미 붙어 있으면 그대로 성공한다(다시 시작한 노드, 놓친 응답, 주기적인 다시 붙이기).
    assert!(!net.add("vip0", vip).await.unwrap());
    assert_eq!(ns.addrs(), ["192.0.2.200/24"]);

    net.remove("vip0", vip).await.unwrap();
    assert!(ns.addrs().is_empty());
    // 없는 주소를 떼도 성공한다(시작할 때 남은 VIP 정리).
    net.remove("vip0", vip).await.unwrap();

    // 없는 인터페이스는 오류다.
    assert!(net.add("nope0", vip).await.is_err());
}
