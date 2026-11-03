//! VM NIC의 방화벽(보안 그룹). 클러스터의 방화벽 NIC를 nftables bridge family 규칙 글 하나로 만들고
//! `nft -f -`로 한 번에(원자적으로) 넣는다. opld가 root(또는 CAP_NET_ADMIN)로 돌아야 한다.

use std::net::IpAddr;

use async_trait::async_trait;
use opl_types::security_group::{Action, Direction, Protocol, Rule};
use opl_types::vm::MacAddr;
use tokio::io::AsyncWriteExt;

use crate::error::DriverError;
use crate::net::failure_text;

/// 방화벽이 켜진 NIC 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirewallNic {
    /// NIC의 tap 장치 이름
    pub tap: String,
    /// NIC의 MAC. 나가는 패킷의 보낸 MAC이 이것이 아니면 막는다.
    pub mac: MacAddr,
    /// NIC 그룹 순서, 그룹 안 순서로 펼친 규칙
    pub rules: Vec<Rule>,
}

/// 클러스터 태그 `tag`의 테이블 `bridge opl_<tag>`를 `nics`로 바꾸는 nft 글. 테이블을 (없으면) 만들고
/// 지운 뒤 새로 정의하므로 한 번에 넣으면 원자적으로 바뀐다. 방화벽 NIC가 없으면 지우기만 한다.
/// 사용자 글자(이름·설명)는 넣지 않고, tap 이름이 `[a-z0-9]`가 아닌 NIC와 앞 NIC와 tap 이름이 같은
/// NIC는 건너뛴다.
/// NIC를 tap 이름 순으로 정렬하므로 같은 입력이면 같은 글이다.
pub fn ruleset(tag: &str, nics: &[FirewallNic]) -> String {
    let table = format!("bridge opl_{tag}");
    let mut out = format!("table {table} {{}}\ndelete table {table}\n");
    let mut nics: Vec<&FirewallNic> = nics
        .iter()
        .filter(|nic| {
            let ok = safe_ifname(&nic.tap);
            if !ok {
                tracing::warn!(tap = %nic.tap, "skipping a firewall NIC with an unexpected tap name");
            }
            ok
        })
        .collect();
    if nics.is_empty() {
        return out;
    }
    // 같은 tap 이름이 둘이면(맵 원소가 겹쳐 글 전체가 실패한다) 첫 것만 쓴다. 정렬은 안정적이다.
    nics.sort_by(|a, b| a.tap.cmp(&b.tap));
    nics.dedup_by(|later, first| {
        let same = later.tap == first.tap;
        if same {
            tracing::warn!(tap = %later.tap, "skipping a firewall NIC with a duplicate tap name");
        }
        same
    });

    out.push_str(&format!("table {table} {{\n"));
    // 맵이 가리키는 체인, 훅 체인이 가리키는 맵을 먼저 적는다.
    for nic in &nics {
        out.push_str(&format!("\tchain o_{} {{\n", nic.tap));
        for line in [
            &format!("ether saddr != {} drop", nic.mac),
            "meta protocol arp return",
            "ct state established,related return",
            "ct state invalid drop",
        ] {
            out.push_str(&format!("\t\t{line}\n"));
        }
        for rule in nic.rules.iter().filter(|r| r.direction == Direction::Out) {
            out.push_str(&format!("\t\t{}\n", rule_line(rule)));
        }
        out.push_str("\t}\n");

        out.push_str(&format!("\tchain i_{} {{\n", nic.tap));
        for line in [
            "meta protocol arp accept",
            "icmpv6 type { nd-neighbor-solicit, nd-neighbor-advert, nd-router-advert } accept",
            "udp sport 67 udp dport 68 accept",
            "udp sport 547 udp dport 546 accept",
            "ct state established,related accept",
            "ct state invalid drop",
        ] {
            out.push_str(&format!("\t\t{line}\n"));
        }
        for rule in nic.rules.iter().filter(|r| r.direction == Direction::In) {
            out.push_str(&format!("\t\t{}\n", rule_line(rule)));
        }
        out.push_str("\t\tdrop\n\t}\n");
    }
    for (map, prefix) in [("out_nics", "o_"), ("in_nics", "i_")] {
        let elements: Vec<String> = nics
            .iter()
            .map(|nic| format!("\"{}\" : jump {prefix}{}", nic.tap, nic.tap))
            .collect();
        out.push_str(&format!(
            "\tmap {map} {{\n\t\ttype ifname : verdict\n\t\telements = {{ {} }}\n\t}}\n",
            elements.join(", ")
        ));
    }
    for (hook, lines) in [
        (
            "forward",
            &["iifname vmap @out_nics", "oifname vmap @in_nics"][..],
        ),
        ("input", &["iifname vmap @out_nics"][..]),
        ("output", &["oifname vmap @in_nics"][..]),
    ] {
        out.push_str(&format!(
            "\tchain {hook} {{\n\t\ttype filter hook {hook} priority 0; policy accept;\n"
        ));
        for line in lines {
            out.push_str(&format!("\t\t{line}\n"));
        }
        out.push_str("\t}\n");
    }
    out.push_str("}\n");
    out
}

/// nft 글에 그대로 넣어도 되는 장치 이름(tap 이름 모양 `op…`, 1~15자 `[a-z0-9]`)
fn safe_ifname(name: &str) -> bool {
    (1..=15).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// 규칙 하나 → nft 규칙 한 줄. 나가기 체인의 허용은 `return`(받는 쪽 들어오기 검사가 남는다).
fn rule_line(rule: &Rule) -> String {
    let mut parts = Vec::new();
    if let Some(remote) = rule.remote {
        let field = match rule.direction {
            Direction::In => "saddr",
            Direction::Out => "daddr",
        };
        // 주소 체계는 `ether type`(nft가 `ip daddr`에 저절로 붙이는 조건) 대신 `meta protocol`로 본다.
        // VLAN-aware 브리지에서 태그가 붙은 프레임은 `ether type`이 802.1Q로 보일 수 있다. 접두사
        // 0(`0.0.0.0/0`, `::/0`)은 그 주소 체계의 모든 패킷이다.
        parts.push(match (remote.addr, remote.prefix) {
            (IpAddr::V4(_), 0) => "meta protocol ip".to_string(),
            (IpAddr::V6(_), 0) => "meta protocol ip6".to_string(),
            (IpAddr::V4(_), _) => format!("meta protocol ip ip {field} {remote}"),
            (IpAddr::V6(_), _) => format!("meta protocol ip6 ip6 {field} {remote}"),
        });
    }
    if let Some(protocol) = rule.protocol {
        parts.push(
            match protocol {
                Protocol::Tcp => "meta l4proto tcp",
                Protocol::Udp => "meta l4proto udp",
                Protocol::Icmp => "meta l4proto icmp",
                Protocol::Icmpv6 => "meta l4proto ipv6-icmp",
            }
            .to_string(),
        );
    }
    if let Some(ports) = rule.ports {
        parts.push(format!("th dport {ports}"));
    }
    parts.push(
        match (rule.direction, rule.action) {
            (Direction::In, Action::Accept) => "accept",
            (Direction::Out, Action::Accept) => "return",
            (_, Action::Drop) => "drop",
        }
        .to_string(),
    );
    parts.join(" ")
}

/// 방화벽 규칙 글을 넣는다. 테스트에서 바꿔 끼울 수 있게 트레이트로 둔다.
#[async_trait]
pub trait Firewall: Send + Sync + std::fmt::Debug {
    /// `ruleset`이 만든 글을 한 번에 넣는다.
    async fn apply(&self, ruleset: &str) -> Result<(), DriverError>;
}

/// `nft -f -`로 규칙 글을 넣는다.
#[derive(Debug, Default)]
pub struct Nft {
    /// 있으면 이 네트워크 이름공간에서 `nft`를 부른다(`ip netns exec NAME`, 시험용).
    netns: Option<String>,
}

impl Nft {
    pub fn in_netns(name: impl Into<String>) -> Self {
        Self {
            netns: Some(name.into()),
        }
    }
}

#[async_trait]
impl Firewall for Nft {
    async fn apply(&self, ruleset: &str) -> Result<(), DriverError> {
        let mut cmd = match &self.netns {
            Some(ns) => {
                let mut cmd = tokio::process::Command::new("ip");
                cmd.args(["netns", "exec", ns, "nft", "-f", "-"]);
                cmd
            }
            None => {
                let mut cmd = tokio::process::Command::new("nft");
                cmd.args(["-f", "-"]);
                cmd
            }
        };
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        // 쓰다 실패하면(nft가 먼저 끝남) 종료 상태와 stderr가 까닭을 말한다.
        let _ = stdin.write_all(ruleset.as_bytes()).await;
        drop(stdin);
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(DriverError::ToolFailed {
                tool: "nft",
                stderr: failure_text(&output.stderr, output.status),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "table bridge opl_abc {}\ndelete table bridge opl_abc\n";

    /// 내장 체인(나가기 앞부분)
    fn out_head(mac: &str) -> String {
        format!(
            "\t\tether saddr != {mac} drop\n\
             \t\tmeta protocol arp return\n\
             \t\tct state established,related return\n\
             \t\tct state invalid drop\n"
        )
    }

    const IN_HEAD: &str = "\t\tmeta protocol arp accept\n\
        \t\ticmpv6 type { nd-neighbor-solicit, nd-neighbor-advert, nd-router-advert } accept\n\
        \t\tudp sport 67 udp dport 68 accept\n\
        \t\tudp sport 547 udp dport 546 accept\n\
        \t\tct state established,related accept\n\
        \t\tct state invalid drop\n";

    const HOOKS: &str = "\tchain forward {\n\
        \t\ttype filter hook forward priority 0; policy accept;\n\
        \t\tiifname vmap @out_nics\n\
        \t\toifname vmap @in_nics\n\
        \t}\n\
        \tchain input {\n\
        \t\ttype filter hook input priority 0; policy accept;\n\
        \t\tiifname vmap @out_nics\n\
        \t}\n\
        \tchain output {\n\
        \t\ttype filter hook output priority 0; policy accept;\n\
        \t\toifname vmap @in_nics\n\
        \t}\n";

    fn rule(json: &str) -> Rule {
        serde_json::from_str(json).unwrap()
    }

    fn nic(tap: &str, mac: &str, rules: Vec<Rule>) -> FirewallNic {
        FirewallNic {
            tap: tap.into(),
            mac: mac.parse().unwrap(),
            rules,
        }
    }

    #[test]
    fn no_nics_only_deletes_the_table() {
        assert_eq!(ruleset("abc", &[]), HEAD);
    }

    #[test]
    fn one_nic_without_rules_drops_everything_in() {
        let text = ruleset("abc", &[nic("op0123456789n0", "52:54:00:aa:bb:cc", vec![])]);
        let expected = format!(
            "{HEAD}table bridge opl_abc {{\n\
             \tchain o_op0123456789n0 {{\n{out}\t}}\n\
             \tchain i_op0123456789n0 {{\n{IN_HEAD}\t\tdrop\n\t}}\n\
             \tmap out_nics {{\n\
             \t\ttype ifname : verdict\n\
             \t\telements = {{ \"op0123456789n0\" : jump o_op0123456789n0 }}\n\
             \t}}\n\
             \tmap in_nics {{\n\
             \t\ttype ifname : verdict\n\
             \t\telements = {{ \"op0123456789n0\" : jump i_op0123456789n0 }}\n\
             \t}}\n\
             {HOOKS}}}\n",
            out = out_head("52:54:00:aa:bb:cc"),
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn two_nics_are_sorted_by_tap_name() {
        let a = nic(
            "opbbbbbbbbbbn0",
            "52:54:00:00:00:02",
            vec![rule(
                r#"{"direction":"in","action":"accept","protocol":"tcp","ports":"22"}"#,
            )],
        );
        let b = nic(
            "opaaaaaaaaaan1",
            "52:54:00:00:00:01",
            vec![rule(
                r#"{"direction":"out","action":"drop","protocol":"tcp","ports":"25"}"#,
            )],
        );
        let text = ruleset("abc", &[a.clone(), b.clone()]);
        // 입력 순서가 달라도 같은 글
        assert_eq!(text, ruleset("abc", &[b, a]));
        let expected = format!(
            "{HEAD}table bridge opl_abc {{\n\
             \tchain o_opaaaaaaaaaan1 {{\n{out1}\t\tmeta l4proto tcp th dport 25 drop\n\t}}\n\
             \tchain i_opaaaaaaaaaan1 {{\n{IN_HEAD}\t\tdrop\n\t}}\n\
             \tchain o_opbbbbbbbbbbn0 {{\n{out2}\t}}\n\
             \tchain i_opbbbbbbbbbbn0 {{\n{IN_HEAD}\t\tmeta l4proto tcp th dport 22 accept\n\t\tdrop\n\t}}\n\
             \tmap out_nics {{\n\
             \t\ttype ifname : verdict\n\
             \t\telements = {{ \"opaaaaaaaaaan1\" : jump o_opaaaaaaaaaan1, \"opbbbbbbbbbbn0\" : jump o_opbbbbbbbbbbn0 }}\n\
             \t}}\n\
             \tmap in_nics {{\n\
             \t\ttype ifname : verdict\n\
             \t\telements = {{ \"opaaaaaaaaaan1\" : jump i_opaaaaaaaaaan1, \"opbbbbbbbbbbn0\" : jump i_opbbbbbbbbbbn0 }}\n\
             \t}}\n\
             {HOOKS}}}\n",
            out1 = out_head("52:54:00:00:00:01"),
            out2 = out_head("52:54:00:00:00:02"),
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn rules_keep_their_order_within_a_direction() {
        let rules = vec![
            rule(r#"{"direction":"in","action":"drop","remote":"10.0.0.5"}"#),
            rule(r#"{"direction":"out","action":"accept","protocol":"udp","ports":"53"}"#),
            rule(r#"{"direction":"in","action":"accept","remote":"10.0.0.0/8"}"#),
            rule(r#"{"direction":"out","action":"drop"}"#),
        ];
        let text = ruleset("abc", &[nic("op0n0", "52:54:00:aa:bb:cc", rules)]);
        let out = format!(
            "\tchain o_op0n0 {{\n{}\t\tmeta l4proto udp th dport 53 return\n\t\tdrop\n\t}}\n",
            out_head("52:54:00:aa:bb:cc")
        );
        assert!(text.contains(&out), "{text}");
        let inn = format!(
            "\tchain i_op0n0 {{\n{IN_HEAD}\t\tmeta protocol ip ip saddr 10.0.0.5/32 drop\n\
             \t\tmeta protocol ip ip saddr 10.0.0.0/8 accept\n\t\tdrop\n\t}}\n"
        );
        assert!(text.contains(&inn), "{text}");
    }

    #[test]
    fn each_rule_shape() {
        let cases = [
            (r#"{"direction":"in","action":"accept"}"#, "accept"),
            (r#"{"direction":"in","action":"drop"}"#, "drop"),
            (r#"{"direction":"out","action":"accept"}"#, "return"),
            (r#"{"direction":"out","action":"drop"}"#, "drop"),
            (
                r#"{"direction":"in","action":"accept","protocol":"tcp","ports":"8000-8080"}"#,
                "meta l4proto tcp th dport 8000-8080 accept",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"udp"}"#,
                "meta l4proto udp accept",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"icmp"}"#,
                "meta l4proto icmp accept",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"icmpv6"}"#,
                "meta l4proto ipv6-icmp accept",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"tcp","ports":"22","remote":"10.0.0.0/8"}"#,
                "meta protocol ip ip saddr 10.0.0.0/8 meta l4proto tcp th dport 22 accept",
            ),
            (
                r#"{"direction":"out","action":"drop","protocol":"udp","ports":"53","remote":"192.168.1.1"}"#,
                "meta protocol ip ip daddr 192.168.1.1/32 meta l4proto udp th dport 53 drop",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"icmpv6","remote":"2001:db8::/32"}"#,
                "meta protocol ip6 ip6 saddr 2001:db8::/32 meta l4proto ipv6-icmp accept",
            ),
            (
                r#"{"direction":"out","action":"accept","remote":"fd00::1"}"#,
                "meta protocol ip6 ip6 daddr fd00::1/128 return",
            ),
            (
                r#"{"direction":"in","action":"accept","protocol":"tcp","ports":"80","remote":"0.0.0.0/0"}"#,
                "meta protocol ip meta l4proto tcp th dport 80 accept",
            ),
            (
                r#"{"direction":"out","action":"drop","remote":"::/0"}"#,
                "meta protocol ip6 drop",
            ),
        ];
        for (json, line) in cases {
            assert_eq!(rule_line(&rule(json)), line, "{json}");
        }
    }

    #[test]
    fn only_the_first_nic_of_a_tap_name_is_used() {
        let first = nic(
            "op0n0",
            "52:54:00:00:00:01",
            vec![rule(r#"{"direction":"in","action":"accept"}"#)],
        );
        let second = nic("op0n0", "52:54:00:00:00:02", vec![]);
        let text = ruleset(
            "abc",
            &[
                nic("op1n0", "52:54:00:00:00:03", vec![]),
                first.clone(),
                second,
            ],
        );
        assert_eq!(
            text,
            ruleset("abc", &[nic("op1n0", "52:54:00:00:00:03", vec![]), first])
        );
        assert_eq!(text.matches("chain o_op0n0 {").count(), 1, "{text}");
        assert!(
            text.contains("ether saddr != 52:54:00:00:00:01 drop"),
            "{text}"
        );
        assert!(!text.contains("52:54:00:00:00:02"), "{text}");
    }

    #[test]
    fn nics_with_unsafe_tap_names_are_skipped() {
        for tap in ["", "op0123456789n0xy", "Op0n0", "op0n0\"", "op0 n0", "op-0"] {
            assert_eq!(
                ruleset("abc", &[nic(tap, "52:54:00:aa:bb:cc", vec![])]),
                HEAD,
                "{tap:?}"
            );
        }
        let text = ruleset(
            "abc",
            &[
                nic("op;drop", "52:54:00:aa:bb:cc", vec![]),
                nic("op0n0", "52:54:00:aa:bb:cd", vec![]),
            ],
        );
        assert!(!text.contains("op;"), "{text}");
        assert!(text.contains("\"op0n0\" : jump o_op0n0 }"), "{text}");
    }
}
