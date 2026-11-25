//! OVN 접근 잠금의 주소 허용 목록(계획 67 §10.3): nftables 표 `inet opl_ovn_<태그>`(입력 훅, 우선순위 `filter - 10`,
//! 정책 accept — OVN 포트만 거른다). 중앙은 NB 6641·SB 역할 6642를 멤버 집합에, Raft 6643·6644와 SB 특권 6645를 중앙
//! 집합에만 열고, chassis는 Geneve(UDP 6081)를 멤버의 터널 주소에만 연다. 그 밖의 출발 주소에서 이 포트로 오는 것은
//! 맺은 연결이라도 버린다(클러스터에서 지운 노드는 인증서가 살아 있어도 붙지 못한다). 루프백 주소는 늘 받는다(노드 자기
//! 주소는 멤버 집합에 있다). 글 전체를 `nft -f -`로 한 번에(원자적으로) 넣는다 — 계획 26 VM 방화벽(`bridge opl_<태그>`)과
//! 다른 표·계열이다.

use std::net::IpAddr;

use orbivisor_types::sdn::{
    AllowSets, GENEVE_PORT, NB_PORT, NB_RAFT_PORT, SB_ADMIN_PORT, SB_PORT, SB_RAFT_PORT,
};

use super::host::{OvnError, OvnHost};
use crate::ceph::runner::Cmd;

/// 표 이름(`inet opl_ovn_<태그>`)
pub fn table(tag: &str) -> String {
    format!("inet opl_ovn_{tag}")
}

/// 이 노드에서 거를 포트: 중앙이면 TCP 포트들, chassis면 Geneve
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockdownRole {
    pub central: bool,
    pub chassis: bool,
}

/// 표를 지우는 글(없어도 실패하지 않는다)
pub fn delete_ruleset(tag: &str) -> String {
    let table = table(tag);
    format!("table {table} {{}}\ndelete table {table}\n")
}

/// 표를 (없으면) 만들고 지운 뒤 새로 정의하는 글. 같은 입력이면 같은 글이다(집합은 이미 이름 순).
/// Geneve는 멤버 노드의 터널 주소와 멤버 집합을 함께 받는다(새 chassis가 터널 주소를 알리기 전에도 막히지 않게).
pub fn ruleset(tag: &str, sets: &AllowSets, role: LockdownRole) -> String {
    let mut out = delete_ruleset(tag);
    out.push_str(&format!("table {} {{\n", table(tag)));
    let mut geneve: Vec<IpAddr> = sets.encaps.iter().chain(&sets.members).copied().collect();
    geneve.sort();
    geneve.dedup();
    let mut groups: Vec<(&str, &str, String, &[IpAddr])> = Vec::new();
    if role.central {
        groups.push((
            "members",
            "tcp",
            format!("{{ {NB_PORT}, {SB_PORT} }}"),
            &sets.members,
        ));
        groups.push((
            "centrals",
            "tcp",
            format!("{{ {NB_RAFT_PORT}, {SB_RAFT_PORT}, {SB_ADMIN_PORT} }}"),
            &sets.centrals,
        ));
    }
    if role.chassis {
        groups.push(("geneve", "udp", GENEVE_PORT.to_string(), &geneve));
    }
    for (name, _, _, addrs) in &groups {
        for (suffix, kind, v6) in [("4", "ipv4_addr", false), ("6", "ipv6_addr", true)] {
            let elements: Vec<String> = addrs
                .iter()
                .filter(|ip| ip.is_ipv6() == v6)
                .map(IpAddr::to_string)
                .collect();
            out.push_str(&format!("\tset {name}{suffix} {{\n\t\ttype {kind}\n"));
            if !elements.is_empty() {
                out.push_str(&format!("\t\telements = {{ {} }}\n", elements.join(", ")));
            }
            out.push_str("\t}\n");
        }
    }
    out.push_str(
        "\tchain input {\n\t\ttype filter hook input priority filter - 10; policy accept;\n\
         \t\tip saddr 127.0.0.1 accept\n\t\tip6 saddr ::1 accept\n",
    );
    for (name, proto, ports, _) in &groups {
        out.push_str(&format!(
            "\t\t{proto} dport {ports} ip saddr @{name}4 accept\n\
             \t\t{proto} dport {ports} ip6 saddr @{name}6 accept\n\
             \t\t{proto} dport {ports} drop\n"
        ));
    }
    out.push_str("\t}\n}\n");
    out
}

/// `ss -tnH state established …`의 상대 주소들(마지막 칸 `주소:포트`, IPv6는 `[…]`, IPv4 대응 IPv6는 IPv4로, 이름
/// 순·겹치지 않게)
pub fn parse_ss_peers(text: &str) -> Vec<IpAddr> {
    let mut peers: Vec<IpAddr> = text
        .lines()
        .filter_map(|l| l.split_whitespace().last())
        .filter_map(|peer| {
            let (host, _port) = peer.rsplit_once(':')?;
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let host = host.split('%').next().unwrap_or(host);
            host.parse::<IpAddr>().ok().map(|ip| ip.to_canonical())
        })
        .collect();
    peers.sort();
    peers.dedup();
    peers
}

impl OvnHost {
    /// 허용 목록 글을 넣는다(`nft -f -`).
    pub async fn apply_lockdown(&self, ruleset: &str) -> Result<(), OvnError> {
        self.run_ok(&Cmd::new("nft", &["-f", "-"]).stdin(ruleset.as_bytes().to_vec()))
            .await
            .map(drop)
    }

    /// 이 클러스터의 허용 목록 표가 있는지(`nft list table`)
    pub async fn lockdown_present(&self) -> Result<bool, OvnError> {
        let table = table(self.tag());
        let mut args = vec!["list", "table"];
        args.extend(table.split(' '));
        Ok(self.exec(&Cmd::new("nft", &args)).await?.success())
    }

    /// 허용 목록 표가 있으면 지운다(잠금 끄기). 지웠으면 참
    pub async fn remove_lockdown(&self) -> Result<bool, OvnError> {
        if !self.lockdown_present().await? {
            return Ok(false);
        }
        self.apply_lockdown(&delete_ruleset(self.tag())).await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn sets() -> AllowSets {
        AllowSets {
            members: vec![ip("10.0.0.1"), ip("10.0.0.2"), ip("fd00::3")],
            centrals: vec![ip("10.0.0.1")],
            encaps: vec![ip("192.168.0.2")],
        }
    }

    /// 중앙이자 chassis: 포트 묶음 셋, 빈 집합은 원소 줄이 없다.
    #[test]
    fn central_and_chassis_ruleset() {
        let text = ruleset(
            "abc",
            &sets(),
            LockdownRole {
                central: true,
                chassis: true,
            },
        );
        assert_eq!(
            text,
            "table inet opl_ovn_abc {}\n\
             delete table inet opl_ovn_abc\n\
             table inet opl_ovn_abc {\n\
             \tset members4 {\n\t\ttype ipv4_addr\n\t\telements = { 10.0.0.1, 10.0.0.2 }\n\t}\n\
             \tset members6 {\n\t\ttype ipv6_addr\n\t\telements = { fd00::3 }\n\t}\n\
             \tset centrals4 {\n\t\ttype ipv4_addr\n\t\telements = { 10.0.0.1 }\n\t}\n\
             \tset centrals6 {\n\t\ttype ipv6_addr\n\t}\n\
             \tset geneve4 {\n\t\ttype ipv4_addr\n\t\telements = { 10.0.0.1, 10.0.0.2, 192.168.0.2 }\n\t}\n\
             \tset geneve6 {\n\t\ttype ipv6_addr\n\t\telements = { fd00::3 }\n\t}\n\
             \tchain input {\n\
             \t\ttype filter hook input priority filter - 10; policy accept;\n\
             \t\tip saddr 127.0.0.1 accept\n\
             \t\tip6 saddr ::1 accept\n\
             \t\ttcp dport { 6641, 6642 } ip saddr @members4 accept\n\
             \t\ttcp dport { 6641, 6642 } ip6 saddr @members6 accept\n\
             \t\ttcp dport { 6641, 6642 } drop\n\
             \t\ttcp dport { 6643, 6644, 6645 } ip saddr @centrals4 accept\n\
             \t\ttcp dport { 6643, 6644, 6645 } ip6 saddr @centrals6 accept\n\
             \t\ttcp dport { 6643, 6644, 6645 } drop\n\
             \t\tudp dport 6081 ip saddr @geneve4 accept\n\
             \t\tudp dport 6081 ip6 saddr @geneve6 accept\n\
             \t\tudp dport 6081 drop\n\
             \t}\n\
             }\n"
        );
    }

    /// chassis만이면 Geneve만 거른다. 같은 입력이면 같은 글이다.
    #[test]
    fn chassis_only_ruleset() {
        let role = LockdownRole {
            central: false,
            chassis: true,
        };
        let text = ruleset("abc", &sets(), role);
        assert!(!text.contains("tcp dport"), "{text}");
        assert!(!text.contains("members4"), "{text}");
        assert!(text.contains("udp dport 6081 drop"), "{text}");
        assert_eq!(text, ruleset("abc", &sets(), role));
        assert_eq!(
            delete_ruleset("abc"),
            "table inet opl_ovn_abc {}\ndelete table inet opl_ovn_abc\n"
        );
    }

    #[test]
    fn ss_peers() {
        let text = "0      0      10.0.0.1:6642      10.0.0.3:51234\n\
                    0      0      10.0.0.1:6641      10.0.0.2:40000\n\
                    0      0      10.0.0.1:6642      10.0.0.2:40002\n\
                    0      0      [fd00::1]:6642     [fd00::9]:3333\n\
                    0      0      [::ffff:10.0.0.1]:6641 [::ffff:10.0.0.7]:1\n\
                    0      0      [fe80::1%eth0]:6642 [fe80::2%eth0]:2\n";
        assert_eq!(
            parse_ss_peers(text),
            [
                ip("10.0.0.2"),
                ip("10.0.0.3"),
                ip("10.0.0.7"),
                ip("fd00::9"),
                ip("fe80::2")
            ]
        );
        assert!(parse_ss_peers("").is_empty());
    }
}
