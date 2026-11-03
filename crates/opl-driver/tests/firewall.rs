//! 실제 nftables 규칙을 네트워크 이름공간 안의 브리지에 넣는 통합 테스트.
//! `cargo test -p opl-driver --features net-it`. Linux에서 root(CAP_NET_ADMIN, `ip netns`)와 iproute2,
//! nftables(커널 5.3 이상), ping, python3가 필요하다. 노드와 VM을 이름공간으로 흉내 내므로 호스트
//! 네트워크와 호스트의 nft 규칙은 건드리지 않는다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use opl_driver::{Firewall, FirewallNic, Nft, ruleset};
use opl_types::network::tap_name;
use opl_types::security_group::Rule;
use opl_types::vm::MacAddr;
use uuid::Uuid;

/// 테스트가 만든 네트워크 이름공간. 끝나면 지운다(안의 장치와 nft 테이블도 함께 사라진다).
struct Netns(String);

impl Netns {
    fn new() -> Self {
        let name = format!("oplt-{}", random_hex(10));
        ip(&["netns", "add", &name]);
        let ns = Self(name);
        ip(&["-n", &ns.0, "link", "set", "lo", "up"]);
        ns
    }

    fn exec(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("ip");
        cmd.args(["netns", "exec", &self.0]).args(args);
        cmd
    }

    /// 성공했는지. 출력은 버린다.
    fn ok(&self, args: &[&str]) -> bool {
        self.exec(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .status
            .success()
    }
}

impl Drop for Netns {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["netns", "del", &self.0]).status();
    }
}

/// 테스트가 띄운 프로세스. 끝나면 멈춘다.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn random_hex(n: usize) -> String {
    Uuid::new_v4().simple().to_string()[..n].to_string()
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

/// 주어진 포트들에서 연결을 받자마자 닫는 TCP 서버
const SERVER: &str = "
import socket, sys, threading
def serve(port):
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(('0.0.0.0', port))
    s.listen(16)
    while True:
        c, _ = s.accept()
        c.close()
for p in sys.argv[1:]:
    threading.Thread(target=serve, args=(int(p),)).start()
";

/// 1초 안에 TCP로 연결되면 0으로 끝난다.
const CLIENT: &str = "import socket, sys; socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=1).close()";

/// "VM" 하나: 이름공간 안의 `eth0`, 브리지 쪽 끝은 tap 이름의 veth
struct Vm {
    ns: Netns,
    tap: String,
    mac: MacAddr,
}

impl Vm {
    fn new(host: &Netns, addr: &str) -> Self {
        let ns = Netns::new();
        let tap = tap_name(Uuid::now_v7(), 0);
        let mac = MacAddr::generate();
        ip(&[
            "-n", &host.0, "link", "add", &tap, "type", "veth", "peer", "name", "eth0", "netns",
            &ns.0,
        ]);
        ip(&["-n", &host.0, "link", "set", &tap, "master", "br0", "up"]);
        ip(&[
            "-n",
            &ns.0,
            "link",
            "set",
            "eth0",
            "address",
            &mac.to_string(),
        ]);
        ip(&["-n", &ns.0, "addr", "add", addr, "dev", "eth0"]);
        ip(&["-n", &ns.0, "link", "set", "eth0", "up"]);
        Self { ns, tap, mac }
    }

    /// 방화벽 NIC. `rules`는 `"in accept tcp 22"`처럼 방향·동작·[프로토콜]·[포트]·[CIDR].
    fn nic(&self, rules: &[&str]) -> FirewallNic {
        FirewallNic {
            tap: self.tap.clone(),
            mac: self.mac,
            rules: rules.iter().map(|r| rule(r)).collect(),
        }
    }

    /// 한 번 ping해서 답이 오는지
    fn ping(&self, to: &str) -> bool {
        self.ns.ok(&["ping", "-c1", "-W1", to])
    }

    /// 한 번 TCP 연결해 보아 되는지
    fn connect(&self, to: &str, port: u16) -> bool {
        self.ns
            .ok(&["python3", "-c", CLIENT, to, &port.to_string()])
    }

    /// `eth0`의 MAC을 바꾼다(위조).
    fn set_mac(&self, mac: MacAddr) {
        ip(&["-n", &self.ns.0, "link", "set", "eth0", "down"]);
        ip(&[
            "-n",
            &self.ns.0,
            "link",
            "set",
            "eth0",
            "address",
            &mac.to_string(),
        ]);
        ip(&["-n", &self.ns.0, "link", "set", "eth0", "up"]);
    }

    /// 이웃(ARP) 캐시를 비운다.
    fn flush_neigh(&self) {
        ip(&["-n", &self.ns.0, "neigh", "flush", "all"]);
    }
}

/// `"in accept tcp 22"`, `"out drop 10.0.0.1/32"`처럼 방향·동작 뒤에 [프로토콜] [포트] [CIDR]
fn rule(text: &str) -> Rule {
    let mut words = text.split(' ');
    let mut json = serde_json::json!({ "direction": words.next(), "action": words.next() });
    for word in words {
        let key = if ["tcp", "udp", "icmp", "icmpv6"].contains(&word) {
            "protocol"
        } else if word.contains(['.', ':']) {
            "remote"
        } else {
            "ports"
        };
        json[key] = word.into();
    }
    serde_json::from_value(json).unwrap()
}

/// `check`가 참이 될 때까지 몇 번 해 본다.
async fn wait_for(what: &str, check: impl Fn() -> bool) {
    for _ in 0..20 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("timed out waiting until {what}");
}

/// 규칙 글을 넣는다. 실패하면 글과 함께 멈춘다.
async fn apply(nft: &Nft, tag: &str, nics: Vec<FirewallNic>) {
    let text = ruleset(tag, &nics);
    nft.apply(&text)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{text}"));
}

#[tokio::test]
async fn security_group_rules_filter_bridged_traffic() {
    let host = Netns::new();
    ip(&["-n", &host.0, "link", "add", "br0", "type", "bridge"]);
    ip(&["-n", &host.0, "link", "set", "br0", "up"]);
    let a = Vm::new(&host, "192.168.88.1/24");
    let b = Vm::new(&host, "192.168.88.2/24");
    let to_b = "192.168.88.2";
    let _server = Proc(
        b.ns.exec(&["python3", "-c", SERVER, "22", "23"])
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // 방화벽 없이는 모두 통한다.
    wait_for("A can ping B", || a.ping(to_b)).await;
    wait_for("A can connect to B:22", || a.connect(to_b, 22)).await;
    wait_for("A can connect to B:23", || a.connect(to_b, 23)).await;

    let tag = random_hex(12);
    let table = format!("opl_{tag}");
    let nft = Nft::in_netns(&host.0);

    // (1) B는 tcp 22만 받는다.
    apply(&nft, &tag, vec![b.nic(&["in accept tcp 22"])]).await;
    let tables = String::from_utf8(
        host.exec(&["nft", "list", "tables"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        tables.contains(&format!("table bridge {table}")),
        "{tables}"
    );
    wait_for("A can connect to B:22", || a.connect(to_b, 22)).await;
    assert!(!a.connect(to_b, 23), "tcp 23 must be blocked");
    assert!(!a.ping(to_b), "ping must be blocked");

    // (2) 규칙을 더하면 곧바로 반영된다(테이블을 통째로 바꾼다).
    apply(
        &nft,
        &tag,
        vec![b.nic(&["in accept tcp 22", "in accept icmp"])],
    )
    .await;
    wait_for("A can ping B", || a.ping(to_b)).await;
    assert!(!a.connect(to_b, 23), "tcp 23 must stay blocked");

    // (3) A의 나가기에서 icmp를 막는다. 나머지 나가기는 허용이다.
    let b_rules = ["in accept tcp 22", "in accept icmp"];
    apply(&nft, &tag, vec![a.nic(&["out drop icmp"]), b.nic(&b_rules)]).await;
    assert!(!a.ping(to_b), "outgoing ping from A must be blocked");
    wait_for("A can connect to B:22", || a.connect(to_b, 22)).await;

    // 허용 목록 + 끝 drop: ARP는 규칙 앞에서 늘 통하므로 이웃 캐시를 비워도 허용한 주소로는 나간다.
    apply(
        &nft,
        &tag,
        vec![
            a.nic(&["out accept 192.168.88.2/32", "out drop"]),
            b.nic(&b_rules),
        ],
    )
    .await;
    a.flush_neigh();
    b.flush_neigh();
    wait_for("A can ping the allowed B", || a.ping(to_b)).await;

    // (4) A가 MAC을 바꾸면(위조) A에서 나가는 것이 모두 막힌다.
    apply(&nft, &tag, vec![a.nic(&[]), b.nic(&b_rules)]).await;
    wait_for("A can ping B", || a.ping(to_b)).await;
    let spoofed = MacAddr::generate();
    a.set_mac(spoofed);
    a.flush_neigh();
    b.flush_neigh();
    for _ in 0..3 {
        assert!(!a.ping(to_b), "A with a spoofed MAC must be blocked");
    }
    // 그 MAC을 NIC의 MAC으로 넣으면 다시 통한다(막힌 까닭이 MAC 검사였다).
    let mut a_nic = a.nic(&[]);
    a_nic.mac = spoofed;
    apply(&nft, &tag, vec![a_nic, b.nic(&b_rules)]).await;
    wait_for("A can ping B with the NIC's MAC", || a.ping(to_b)).await;

    // (5) 방화벽 NIC가 없으면 테이블이 사라지고 모두 통한다. 두 번 해도 된다.
    apply(&nft, &tag, vec![]).await;
    apply(&nft, &tag, vec![]).await;
    let tables = String::from_utf8(
        host.exec(&["nft", "list", "tables"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(!tables.contains(&table), "{tables}");
    wait_for("A can connect to B:23", || a.connect(to_b, 23)).await;
}

/// VLAN-aware 브리지: VM tap은 VLAN 10 untagged(pvid), 업링크는 VLAN 10 tagged로 다른 이름공간의
/// VLAN 하위 인터페이스와 잇는다. 주소 규칙(`meta protocol ip ip daddr …`)이 태그가 붙는 쪽으로
/// 나가거나 태그를 달고 들어오는 패킷에도 맞는지 본다.
#[tokio::test]
async fn address_rules_match_on_a_vlan_aware_bridge() {
    let host = Netns::new();
    ip(&[
        "-n",
        &host.0,
        "link",
        "add",
        "br0",
        "type",
        "bridge",
        "vlan_filtering",
        "1",
    ]);
    ip(&["-n", &host.0, "link", "set", "br0", "up"]);
    let bridge_vlan = |args: &[&str]| {
        let out = host
            .exec(&[&["bridge", "vlan"][..], args].concat())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "bridge vlan {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let a = Vm::new(&host, "192.168.89.1/24");
    bridge_vlan(&["add", "dev", &a.tap, "vid", "10", "pvid", "untagged"]);
    bridge_vlan(&["del", "dev", &a.tap, "vid", "1"]);
    // 업링크 건너편: VLAN 10 하위 인터페이스 `trunk.10`에 주소
    let up = Netns::new();
    ip(&[
        "-n", &host.0, "link", "add", "upl0", "type", "veth", "peer", "name", "trunk", "netns",
        &up.0,
    ]);
    ip(&["-n", &host.0, "link", "set", "upl0", "master", "br0", "up"]);
    bridge_vlan(&["add", "dev", "upl0", "vid", "10"]);
    bridge_vlan(&["del", "dev", "upl0", "vid", "1"]);
    ip(&["-n", &up.0, "link", "set", "dev", "trunk", "up"]);
    ip(&[
        "-n", &up.0, "link", "add", "link", "trunk", "name", "trunk.10", "type", "vlan", "id", "10",
    ]);
    ip(&[
        "-n",
        &up.0,
        "addr",
        "add",
        "192.168.89.2/24",
        "dev",
        "trunk.10",
    ]);
    ip(&["-n", &up.0, "link", "set", "dev", "trunk.10", "up"]);
    let (to_up, to_a) = ("192.168.89.2", "192.168.89.1");
    let up_pings_a = || up.ok(&["ping", "-c1", "-W1", to_a]);

    // 방화벽 없이는 통한다.
    wait_for("A can ping the uplink", || a.ping(to_up)).await;
    wait_for("the uplink can ping A", up_pings_a).await;

    let tag = random_hex(12);
    let nft = Nft::in_netns(&host.0);
    // 나가기: 업링크 주소 하나, IPv4 전체
    apply(&nft, &tag, vec![a.nic(&["out drop 192.168.89.2/32"])]).await;
    assert!(
        !a.ping(to_up),
        "ping to the dropped address must be blocked"
    );
    apply(&nft, &tag, vec![a.nic(&["out drop 0.0.0.0/0"])]).await;
    assert!(!a.ping(to_up), "IPv4 out must be blocked");
    // 허용 목록 + 끝 drop: 이웃 캐시를 비워도 ARP는 통해 허용한 주소로는 나간다.
    apply(
        &nft,
        &tag,
        vec![a.nic(&["out accept 192.168.89.2/32", "out drop"])],
    )
    .await;
    a.flush_neigh();
    ip(&["-n", &up.0, "neigh", "flush", "all"]);
    wait_for("A can ping the allowed uplink", || a.ping(to_up)).await;

    // 들어오기: 규칙이 없으면 막히고(A가 연 연결의 응답은 들어온다), 주소·IPv4 전체를 허용하면 통한다.
    apply(&nft, &tag, vec![a.nic(&[])]).await;
    wait_for("A can ping the uplink", || a.ping(to_up)).await;
    assert!(!up_pings_a(), "in must be blocked without rules");
    apply(&nft, &tag, vec![a.nic(&["in accept 192.168.89.2"])]).await;
    wait_for("the uplink can ping A", up_pings_a).await;
    apply(&nft, &tag, vec![a.nic(&["in accept 192.168.89.0/25"])]).await;
    wait_for("the uplink can ping A", up_pings_a).await;
    apply(
        &nft,
        &tag,
        vec![a.nic(&["in drop 192.168.89.2", "in accept 0.0.0.0/0"])],
    )
    .await;
    assert!(!up_pings_a(), "the dropped address must stay blocked");
    apply(&nft, &tag, vec![a.nic(&["in accept 0.0.0.0/0"])]).await;
    wait_for("the uplink can ping A", up_pings_a).await;
    apply(&nft, &tag, vec![]).await;
}

#[tokio::test]
async fn bad_ruleset_fails_and_keeps_the_old_rules() {
    let host = Netns::new();
    let tag = random_hex(12);
    let nft = Nft::in_netns(&host.0);
    let nic = FirewallNic {
        tap: tap_name(Uuid::now_v7(), 0),
        mac: MacAddr::generate(),
        rules: vec![rule("in accept tcp 22")],
    };
    let good = ruleset(&tag, std::slice::from_ref(&nic));
    nft.apply(&good).await.unwrap();
    // 원자적: 앞부분(테이블 지우기)도 반영되지 않는다.
    let err = nft
        .apply(&format!("{good}this is not nft\n"))
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("nft failed: "), "{err}");
    let out = host
        .exec(&["nft", "list", "table", "bridge", &format!("opl_{tag}")])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&format!("chain i_{}", nic.tap)), "{text}");
}
