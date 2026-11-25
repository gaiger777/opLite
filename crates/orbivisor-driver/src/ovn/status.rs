//! OVN·OVS 명령 출력 해석(계획 65): `ovs-appctl cluster/status`, 판(`--version`), `ovs-vsctl --format=json` 표,
//! `ovs-vsctl get-ssl`. 고정물(`fixtures/`)은 Ubuntu 24.04의 OVN 24.03.8·OVS 3.3.9에서 받은 실제 출력이다.

use std::collections::BTreeMap;

use orbivisor_types::sdn::RaftRole;
use serde_json::Value;
use uuid::Uuid;

/// `ovs-appctl -t CTL cluster/status DB`의 내용
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterStatus {
    /// DB 이름(`OVN_Northbound`)
    pub name: String,
    /// 클러스터 id. 합류 중이면 아직 모른다(`not yet known`).
    pub cid: Option<Uuid>,
    /// 이 서버의 id
    pub sid: Option<Uuid>,
    /// 이 서버의 Raft 주소(`ssl:10.0.0.1:6643`)
    pub address: String,
    /// `cluster member`·`joining cluster`·`leaving cluster` …
    pub status: String,
    /// `leader`·`follower`·`candidate`
    pub role: String,
    pub term: u64,
    /// 리더의 짧은 id(`self`면 이 서버, `unknown`이면 None)
    pub leader: Option<String>,
    /// 클러스터의 서버들(이 서버 포함)
    pub servers: Vec<ClusterServer>,
    /// 선거 시간(`Election timer:`, ms)
    pub election_timer_ms: Option<u64>,
}

/// `cluster/status`의 `Servers:` 줄 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterServer {
    /// 짧은 서버 id(`4f28`)
    pub sid: String,
    /// Raft 주소
    pub address: String,
    pub is_self: bool,
    /// 그 서버에게서 마지막으로 받은 지(`last msg N ms ago` — 리더는 모든 팔로어, 팔로어는 리더만 적는다)
    pub last_msg_ms: Option<u64>,
}

impl ClusterStatus {
    /// 클러스터 멤버다(합류·떠나는 중이 아님).
    pub fn is_member(&self) -> bool {
        self.status == "cluster member"
    }

    /// 이 서버가 리더다.
    pub fn is_leader(&self) -> bool {
        self.is_member() && self.role == "leader"
    }

    /// 잠금 방화벽을 건 뒤에도 Raft가 이어졌는지(리뷰 2 I1): `before`(표를 걸기 전의 이 DB)에 견준다. 건 뒤(`self`)
    /// 멤버이고 리더나 팔로어이며 리더를 알아야 한다. 리더면 걸기 전에 선거 시간 두 배 안에 들었던 다른 서버 가운데
    /// `judged`(지금의 중앙 주소)인 것마다, 팔로어면 리더에게서 `since_ms`(표를 건 뒤 지난 시간) 안에 받았어야 한다 — 표
    /// 앞의 소식은 세지 않는다. 걸기 전부터 끊겼던 서버(꺼진 중앙, 내쫓지 못한 서버)는 보지 않는다. 걸기 전에도 온전하지
    /// 않았으면(리더를 모름) 판단하지 않는다(Ok). 아니면 그 까닭
    pub fn raft_kept(
        &self,
        before: &ClusterStatus,
        judged: impl Fn(&str) -> bool,
        since_ms: u64,
    ) -> Result<(), String> {
        let db = &self.name;
        let live = |s: &ClusterStatus| {
            s.is_member() && matches!(s.role.as_str(), "leader" | "follower") && s.leader.is_some()
        };
        if !live(before) {
            return Ok(());
        }
        if !live(self) {
            let leader = if self.leader.is_some() {
                ""
            } else {
                ", no leader"
            };
            return Err(format!("{db} is {} ({}{leader})", self.status, self.role));
        }
        let after_apply = |s: &ClusterServer| s.last_msg_ms.is_some_and(|ms| ms <= since_ms);
        let stale: Vec<&str> = if self.leader.as_deref() == Some("self") {
            let max_age = 2 * before.election_timer_ms.unwrap_or(1000);
            before
                .servers
                .iter()
                .filter(|s| !s.is_self && s.last_msg_ms.is_some_and(|ms| ms <= max_age))
                .filter(|s| judged(&s.address))
                .filter(|b| {
                    !self
                        .servers
                        .iter()
                        .find(|s| s.sid == b.sid)
                        .is_some_and(after_apply)
                })
                .map(|s| s.address.as_str())
                .collect()
        } else {
            let leader = self.leader.as_deref();
            self.servers
                .iter()
                .filter(|s| Some(s.sid.as_str()) == leader && judged(&s.address))
                .filter(|s| !after_apply(s))
                .map(|s| s.address.as_str())
                .collect()
        };
        if stale.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{db} has not heard from {} in the {since_ms} ms since the allow-list was applied",
                stale.join(", ")
            ))
        }
    }

    /// 노드 상태(`CentralHealth`)에 쓰는 역할: 멤버면 리더·팔로어·후보, 합류 중이면 `Joining`, 그 밖은 `Unknown`
    pub fn raft_role(&self) -> RaftRole {
        if self.status == "joining cluster" {
            return RaftRole::Joining;
        }
        if !self.is_member() {
            return RaftRole::Unknown;
        }
        match self.role.as_str() {
            "leader" => RaftRole::Leader,
            "follower" => RaftRole::Follower,
            "candidate" => RaftRole::Candidate,
            _ => RaftRole::Unknown,
        }
    }
}

/// Raft 주소(`ssl:10.0.0.1:6643`, `tcp:[fd00::1]:6643`)의 IP
pub fn raft_address_ip(address: &str) -> Option<std::net::IpAddr> {
    let rest = address.split_once(':').map_or(address, |(_, r)| r);
    let (host, _) = rest.rsplit_once(':')?;
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

/// `cluster/status` 출력을 읽는다. `Name:`·`Status:`·`Role:`이 없으면 오류다.
pub fn parse_cluster_status(text: &str) -> Result<ClusterStatus, String> {
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    let mut servers = Vec::new();
    let mut in_servers = false;
    for line in text.lines() {
        if in_servers {
            if let Some(server) = parse_server(line) {
                servers.push(server);
                continue;
            }
            if !line.starts_with(' ') && !line.trim().is_empty() {
                in_servers = false;
            }
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if line.starts_with(' ') {
            continue;
        }
        let key = key.trim();
        if key == "Servers" {
            in_servers = true;
            continue;
        }
        fields.entry(key).or_insert(value.trim());
    }
    let field = |k: &str| {
        fields
            .get(k)
            .map(|v| v.to_string())
            .ok_or_else(|| format!("no {k:?} in the cluster status: {}", first_line(text)))
    };
    // `5f8b (5f8bebd0-7593-4d24-8db5-23678d1f6ce6)` → 괄호 안의 UUID
    let long_id = |k: &str| {
        fields
            .get(k)
            .and_then(|v| v.split_once('('))
            .and_then(|(_, rest)| rest.strip_suffix(')'))
            .and_then(|id| id.parse::<Uuid>().ok())
    };
    let leader = fields
        .get("Leader")
        .filter(|l| **l != "unknown")
        .map(|l| l.to_string());
    Ok(ClusterStatus {
        name: field("Name")?,
        cid: long_id("Cluster ID"),
        sid: long_id("Server ID"),
        address: fields.get("Address").unwrap_or(&"").to_string(),
        status: field("Status")?,
        role: field("Role")?,
        term: fields
            .get("Term")
            .and_then(|t| t.parse().ok())
            .unwrap_or_default(),
        leader,
        servers,
        election_timer_ms: fields.get("Election timer").and_then(|t| t.parse().ok()),
    })
}

/// `    4f28 (4f28 at ssl:127.0.0.1:6643) (self) next_index=2 match_index=2`
fn parse_server(line: &str) -> Option<ClusterServer> {
    if !line.starts_with(' ') {
        return None;
    }
    let line = line.trim();
    let (sid, rest) = line.split_once(' ')?;
    let inner = rest.strip_prefix('(')?.split_once(')')?.0;
    let (_, address) = inner.split_once(" at ")?;
    let last_msg_ms = rest
        .split_once("last msg ")
        .and_then(|(_, t)| t.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    Some(ClusterServer {
        sid: sid.to_string(),
        address: address.to_string(),
        is_self: rest.contains("(self)"),
        last_msg_ms,
    })
}

fn first_line(text: &str) -> &str {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
}

/// `--version` 출력의 첫 줄 마지막 낱말(`ovn-controller 24.03.8` → `24.03.8`, `ovs-vsctl (Open vSwitch) 3.3.9` →
/// `3.3.9`). 숫자로 시작하지 않으면 None
pub fn parse_version(text: &str) -> Option<String> {
    let word = first_line(text).split_whitespace().last()?;
    word.starts_with(|c: char| c.is_ascii_digit())
        .then(|| word.to_string())
}

/// 판의 (주, 부)(`24.03.8` → (24, 3))
pub fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split(['.', '-', '~', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// `ovs-vsctl --format=json …`의 표를 행마다 (열 이름 → 값)으로 읽는다.
pub fn parse_table(json: &[u8]) -> Result<Vec<BTreeMap<String, Value>>, String> {
    let value: Value =
        serde_json::from_slice(json).map_err(|e| format!("cannot read the OVSDB table: {e}"))?;
    let headings: Vec<String> = value["headings"]
        .as_array()
        .ok_or("the OVSDB table has no headings")?
        .iter()
        .map(|h| h.as_str().unwrap_or_default().to_string())
        .collect();
    let rows = value["data"]
        .as_array()
        .ok_or("the OVSDB table has no data")?;
    Ok(rows
        .iter()
        .filter_map(Value::as_array)
        .map(|row| headings.iter().cloned().zip(row.iter().cloned()).collect())
        .collect())
}

/// OVSDB 값 `["map", [[k, v], …]]`의 문자열 쌍(문자열이 아닌 키·값은 뺀다)
pub fn atom_map(value: &Value) -> BTreeMap<String, String> {
    let pairs = match value.as_array().map(Vec::as_slice) {
        Some([tag, pairs]) if tag == "map" => pairs.as_array(),
        _ => None,
    };
    pairs
        .into_iter()
        .flatten()
        .filter_map(|pair| {
            let k = pair.get(0)?.as_str()?;
            let v = pair.get(1)?.as_str()?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// OVSDB 정수 값(빈 집합 `["set", []]`이면 None)
pub fn atom_int(value: &Value) -> Option<i64> {
    value.as_i64()
}

/// `ovs-vsctl --format=json --columns=external_ids list Open_vSwitch`의 `external_ids`
pub fn parse_external_ids(json: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let rows = parse_table(json)?;
    let row = rows.first().ok_or("the Open_vSwitch table is empty")?;
    Ok(row.get("external_ids").map(atom_map).unwrap_or_default())
}

/// OVS `Interface` 행 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvsInterface {
    pub name: String,
    /// OpenFlow 포트 번호. 장치가 없으면 -1, 아직 정하지 않았으면 None
    pub ofport: Option<i64>,
    pub external_ids: BTreeMap<String, String>,
}

/// `ovs-vsctl --format=json --columns=name,ofport,external_ids find Interface …`
pub fn parse_interfaces(json: &[u8]) -> Result<Vec<OvsInterface>, String> {
    Ok(parse_table(json)?
        .into_iter()
        .map(|row| OvsInterface {
            name: row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            ofport: row.get("ofport").and_then(atom_int),
            external_ids: row.get("external_ids").map(atom_map).unwrap_or_default(),
        })
        .collect())
}

/// `ovs-vsctl get-ssl`의 파일들(설정되지 않았으면 모두 None)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SslConfig {
    pub private_key: Option<String>,
    pub certificate: Option<String>,
    pub ca_cert: Option<String>,
}

/// `Private key: …`·`Certificate: …`·`CA Certificate: …` 줄을 읽는다.
pub fn parse_ssl(text: &str) -> SslConfig {
    let mut ssl = SslConfig::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = Some(value.trim().to_string()).filter(|v| !v.is_empty());
        match key.trim() {
            "Private key" => ssl.private_key = value,
            "Certificate" => ssl.certificate = value,
            "CA Certificate" => ssl.ca_cert = value,
            _ => {}
        }
    }
    ssl
}

#[cfg(test)]
mod tests {
    use super::*;

    const NB_LEADER: &str = include_str!("fixtures/cluster-status-nb-leader.txt");
    const SB_LEADER: &str = include_str!("fixtures/cluster-status-sb-leader.txt");
    const JOINING: &str = include_str!("fixtures/cluster-status-joining.txt");
    const FOLLOWER: &str = include_str!("fixtures/cluster-status-follower.txt");
    const LEADER_TWO: &str = include_str!("fixtures/cluster-status-leader-two.txt");

    /// 처음 만든 클러스터의 하나뿐인 서버는 리더다.
    #[test]
    fn a_new_cluster_has_one_leader() {
        let s = parse_cluster_status(NB_LEADER).unwrap();
        assert_eq!(s.name, "OVN_Northbound");
        assert_eq!(
            s.cid,
            Some("5f8bebd0-7593-4d24-8db5-23678d1f6ce6".parse().unwrap())
        );
        assert_eq!(
            s.sid,
            Some("4f28dad5-42cb-46b0-aa86-97bbc5da8166".parse().unwrap())
        );
        assert_eq!(s.address, "ssl:127.0.0.1:6643");
        assert!(s.is_member() && s.is_leader());
        assert_eq!(s.raft_role(), RaftRole::Leader);
        assert_eq!((s.term, s.leader.as_deref()), (1, Some("self")));
        assert_eq!(
            s.servers,
            [ClusterServer {
                sid: "4f28".into(),
                address: "ssl:127.0.0.1:6643".into(),
                is_self: true,
                last_msg_ms: None,
            }]
        );
        assert_eq!(s.election_timer_ms, Some(1000));
        // 혼자인 리더는 늘 온전하다.
        assert_eq!(s.raft_kept(&s, |_| true, 0), Ok(()));
        let sb = parse_cluster_status(SB_LEADER).unwrap();
        assert_eq!(sb.name, "OVN_Southbound");
        assert_eq!(sb.address, "ssl:127.0.0.1:6644");
        assert!(sb.is_leader());
    }

    /// 합류 중인 서버는 클러스터 id를 아직 모르고 서버 목록이 비어 있다. 합류하면 팔로어이고 리더가 둘을 본다.
    #[test]
    fn joining_then_member() {
        let s = parse_cluster_status(JOINING).unwrap();
        assert_eq!((s.cid, s.status.as_str()), (None, "joining cluster"));
        assert_eq!(s.raft_role(), RaftRole::Joining);
        assert!(!s.is_member() && s.leader.is_none() && s.servers.is_empty());

        let f = parse_cluster_status(FOLLOWER).unwrap();
        assert!(f.is_member() && !f.is_leader());
        assert_eq!(f.raft_role(), RaftRole::Follower);
        assert_eq!(f.leader.as_deref(), Some("4f28"));
        assert_eq!(
            f.cid,
            Some("5f8bebd0-7593-4d24-8db5-23678d1f6ce6".parse().unwrap())
        );
        let addresses: Vec<(&str, bool)> = f
            .servers
            .iter()
            .map(|s| (s.address.as_str(), s.is_self))
            .collect();
        assert_eq!(
            addresses,
            [("ssl:127.0.0.2:6643", true), ("ssl:127.0.0.1:6643", false)]
        );
        let l = parse_cluster_status(LEADER_TWO).unwrap();
        assert_eq!(l.servers.len(), 2);
        assert!(l.is_leader());
    }

    /// 잠금 방화벽 뒤 Raft 확인(리뷰 1 I5): 리더는 팔로어에게서, 팔로어는 리더에게서 최근에 받아야 한다.
    #[test]
    fn raft_is_intact_only_with_fresh_messages() {
        let all = |_: &str| true;
        let l = parse_cluster_status(LEADER_TWO).unwrap();
        assert_eq!(l.servers[0].last_msg_ms, Some(331));
        assert_eq!(l.raft_kept(&l, all, 2000), Ok(()));
        // 표 앞의 소식(331 ms 전 — 건 지 300 ms)은 세지 않는다.
        let err = l.raft_kept(&l, all, 300).unwrap_err();
        assert!(err.contains("ssl:127.0.0.2:6643"), "{err}");
        let f = parse_cluster_status(FOLLOWER).unwrap();
        assert_eq!(f.raft_kept(&f, all, 2000), Ok(()));
        let cut = FOLLOWER.replace("last msg 331 ms ago", "last msg 9000 ms ago");
        assert!(
            parse_cluster_status(&cut)
                .unwrap()
                .raft_kept(&f, all, 2000)
                .is_err()
        );
        let lost = FOLLOWER
            .replace("Role: follower", "Role: candidate")
            .replace("Leader: 4f28", "Leader: unknown");
        let lost = parse_cluster_status(&lost).unwrap();
        assert!(lost.raft_kept(&f, all, 2000).is_err());
        // 걸기 전에도 리더를 몰랐으면 판단하지 않는다.
        assert_eq!(lost.raft_kept(&lost, all, 2000), Ok(()));
    }

    /// 리뷰 2 I1: 걸기 전부터 끊겼던 서버(꺼진 중앙·내쫓지 못한 서버)나 중앙이 아닌 주소는 리더가 듣지 못해도 끊김이 아니다.
    #[test]
    fn raft_kept_judges_only_peers_that_were_fresh_before() {
        let down = LEADER_TWO.replace("last msg 331 ms ago", "last msg 9000 ms ago");
        let before = parse_cluster_status(&down).unwrap();
        let after = parse_cluster_status(&down.replace("9000", "12000")).unwrap();
        assert_eq!(after.raft_kept(&before, |_| true, 2500), Ok(()));
        let fresh = parse_cluster_status(LEADER_TWO).unwrap();
        assert!(after.raft_kept(&fresh, |_| true, 2500).is_err());
        let central = |a: &str| raft_address_ip(a) == Some("127.0.0.1".parse().unwrap());
        assert_eq!(after.raft_kept(&fresh, central, 2500), Ok(()));
        assert_eq!(
            raft_address_ip("ssl:127.0.0.2:6643"),
            Some("127.0.0.2".parse().unwrap())
        );
        assert_eq!(
            raft_address_ip("ssl:[fd00::2]:6644"),
            Some("fd00::2".parse().unwrap())
        );
    }

    /// 떠난 서버(`unknown cluster`)나 다른 출력은 오류다.
    #[test]
    fn other_outputs_are_errors() {
        let err = parse_cluster_status("unknown cluster\n").unwrap_err();
        assert!(err.contains("unknown cluster"), "{err}");
        assert!(parse_cluster_status("").is_err());
    }

    #[test]
    fn versions() {
        let ovn = include_str!("fixtures/ovn-controller-version.txt");
        let ovs = include_str!("fixtures/ovs-vsctl-version.txt");
        assert_eq!(parse_version(ovn).as_deref(), Some("24.03.8"));
        assert_eq!(parse_version(ovs).as_deref(), Some("3.3.9"));
        assert_eq!(
            parse_version("ovn-nbctl 24.03.8\n").as_deref(),
            Some("24.03.8")
        );
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(major_minor("24.03.8"), Some((24, 3)));
        assert_eq!(major_minor("25.09.0~rc1"), Some((25, 9)));
        assert_eq!(major_minor("x"), None);
    }

    /// 실제 `find Interface` 출력: 장치가 없는 포트는 ofport -1이다.
    #[test]
    fn interfaces_and_maps() {
        let list = parse_interfaces(include_bytes!("fixtures/find-interfaces.json")).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].name.as_str(), list[0].ofport), ("optest", Some(1)));
        assert_eq!(
            (list[1].name.as_str(), list[1].ofport),
            ("optap0", Some(-1))
        );
        assert_eq!(list[1].external_ids["iface-id"], "x");
        assert_eq!(list[1].external_ids["orbivisor-cluster"], "abcdef012345");
        // 정하지 않은 ofport는 빈 집합이다.
        let unset = br#"{"data":[["tap1",["set",[]],["map",[]]]],"headings":["name","ofport","external_ids"]}"#;
        let list = parse_interfaces(unset).unwrap();
        assert_eq!((list[0].ofport, list[0].external_ids.len()), (None, 0));
        assert!(parse_interfaces(b"not json").is_err());

        let ids = parse_external_ids(include_bytes!("fixtures/external-ids-fresh.json")).unwrap();
        assert_eq!(ids["system-id"], "b72814a1-deac-4c3b-9c43-5719f7683fb6");
        assert!(!ids.contains_key("ovn-remote"));
        assert!(parse_external_ids(br#"{"data":[],"headings":["external_ids"]}"#).is_err());
        // 실제 chassis 구성 뒤(`ovn` 작업의 출력)
        let ids = parse_external_ids(include_bytes!("fixtures/external-ids-chassis.json")).unwrap();
        assert_eq!(ids["orbivisor-cluster"], "3ff07b431d8e");
        assert_eq!(ids["ovn-remote"], "ssl:127.0.0.1:6642");
        assert_eq!(ids["ovn-encap-type"], "geneve");
        assert_eq!(ids["system-id"], "01a1f5c3-80d3-736d-87b4-16dc80b56c67");
    }

    #[test]
    fn ssl_config() {
        let ssl = parse_ssl(
            "Private key: /etc/opl/ovn/node.key\nCertificate: /etc/opl/ovn/node.crt\n\
             CA Certificate: /etc/opl/ovn/ca.crt\nBootstrap: false\n",
        );
        assert_eq!(ssl.private_key.as_deref(), Some("/etc/opl/ovn/node.key"));
        assert_eq!(ssl.certificate.as_deref(), Some("/etc/opl/ovn/node.crt"));
        assert_eq!(ssl.ca_cert.as_deref(), Some("/etc/opl/ovn/ca.crt"));
        assert_eq!(parse_ssl(""), SslConfig::default());
    }
}
