//! HA 검사의 노드 쪽 probe. 인터페이스·본딩 포트(sysfs·procfs와 `ip -j -d addr show`), 핑, 풀 경로
//! (디렉터리 읽기, iSCSI 세션, multipath 경로, RADOS)를 본다. 파일 읽기는 블로킹 함수이고 명령은 async
//! 함수다. 모두 cfg 없이 컴파일되며 Linux가 아니면 sysfs가 없어 `unknown`·`failing`이 된다.

use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output, Stdio};
use std::time::Duration;

use opl_types::ha_check::CheckState;
use opl_types::network::is_managed_ifname;
use opl_types::node::{
    BondPort, BondState, BondStatus, InterfaceKind, MAX_NODE_INTERFACES, NodeInterface, PortState,
};
use serde::Deserialize;

use crate::net::failure_text;
use crate::storage::RBD_DEFAULT_USER;

/// 핑 하나를 기다리는 시간
pub const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// 디렉터리 풀 probe(`probe_dir`) 하나를 기다리는 시간
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// `rados stat` 하나를 기다리는 시간(안쪽 접속·OSD 시간 제한 3 + 3초보다 넉넉히)
pub const RBD_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// `pvs` 하나를 기다리는 시간
pub const PVS_TIMEOUT: Duration = Duration::from_secs(5);
/// RBD probe가 찾는 객체 이름. 없어도 OSD가 답하면 통과다.
pub const RBD_PROBE_OBJECT: &str = "orbivisor-health-probe";
/// 명령이 스스로 끝낼 시간(핑의 `-W`)에 더해 기다리는 여유
const COMMAND_SLACK: Duration = Duration::from_secs(1);
/// 디렉터리 probe 파일에서 읽는 바이트 수
const PROBE_READ: usize = 64;
/// dm 장치 아래로 따라 내려가는 깊이(파티션 → multipath → 경로)
const DM_DEPTH: usize = 4;

/// sysfs·procfs의 뿌리. 시험은 임시 디렉터리 아래 가짜 트리를 쓴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetFs {
    pub sys: PathBuf,
    pub proc: PathBuf,
}

impl Default for NetFs {
    fn default() -> Self {
        Self {
            sys: PathBuf::from("/sys"),
            proc: PathBuf::from("/proc"),
        }
    }
}

impl NetFs {
    /// `{root}/sys`·`{root}/proc`
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            sys: root.join("sys"),
            proc: root.join("proc"),
        }
    }

    fn net(&self, iface: &str) -> PathBuf {
        self.sys.join("class/net").join(iface)
    }
}

/// `link` 검사 하나의 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkProbe {
    pub state: CheckState,
    pub error: Option<String>,
    /// 본딩이면 그 포트 상태
    pub bond: Option<BondStatus>,
}

/// 파일 내용(앞뒤 공백 없이)
fn read_trim(path: &Path) -> std::io::Result<String> {
    Ok(std::fs::read_to_string(path)?.trim().to_string())
}

/// sysfs `carrier`. 내려간 인터페이스는 읽기가 실패하므로(EINVAL) 없다.
fn carrier(net: &NetFs, iface: &str) -> Option<bool> {
    read_trim(&net.net(iface).join("carrier"))
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .map(|v| v == 1)
}

/// sysfs `speed`(Mbps). 음수(-1, 모름)·읽기 실패는 없다.
fn speed(net: &NetFs, iface: &str) -> Option<u32> {
    read_trim(&net.net(iface).join("speed"))
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v > 0 && v < u32::MAX)
}

fn operstate(net: &NetFs, iface: &str) -> Option<String> {
    read_trim(&net.net(iface).join("operstate")).ok()
}

/// 인터페이스 하나의 `link` 판정. 본딩이면 포트 상태로, 브리지면 아래 포트(업링크)로 본다([`bridge_uplinks`]).
/// 인터페이스가 없으면 `unknown`이다(이 노드에서 이름이 다를 수 있다. 본 적이 있는 인터페이스가 사라졌는지는
/// 부르는 쪽이 안다).
pub fn link_state(net: &NetFs, iface: &str) -> LinkProbe {
    if !net.net(iface).exists() {
        return LinkProbe {
            state: CheckState::Unknown,
            error: Some(format!("interface {iface} not found on this node")),
            bond: None,
        };
    }
    let has_carrier = carrier(net, iface) == Some(true);
    match bond_status(net, iface) {
        Ok(Some(bond)) => {
            let (state, error) = if bond.state == BondState::Down || !has_carrier {
                let error = if bond.state == BondState::Down {
                    format!("bond is down: {}", bond_detail(iface, &bond))
                } else {
                    format!("bond {iface} has no carrier")
                };
                (CheckState::Failing, Some(error))
            } else if bond.state == BondState::Degraded {
                (CheckState::Degraded, Some(bond_detail(iface, &bond)))
            } else {
                (CheckState::Ok, None)
            };
            LinkProbe {
                state,
                error,
                bond: Some(bond),
            }
        }
        // 본딩 정보를 읽지 못하면 일반 링크처럼 본다.
        Ok(None) | Err(_) => {
            let op = operstate(net, iface).unwrap_or_default();
            // dummy·일부 드라이버는 operstate가 `unknown`이다.
            if has_carrier && (op == "up" || op == "unknown") {
                // 브리지는 VM의 tap 포트가 carrier를 지키므로 업링크가 모두 끊겨도 carrier가 있다.
                let error = bridge_uplinks(net, iface)
                    .filter(|u| u.linked.is_empty())
                    .map(|u| {
                        format!(
                            "bridge {iface} has no uplink with a link ({} down)",
                            u.down.join(", ")
                        )
                    });
                LinkProbe {
                    state: if error.is_some() {
                        CheckState::Failing
                    } else {
                        CheckState::Ok
                    },
                    error,
                    bond: None,
                }
            } else {
                LinkProbe {
                    state: CheckState::Failing,
                    error: Some(format!(
                        "{iface} is down (operstate {op}, carrier {})",
                        u8::from(has_carrier)
                    )),
                    bond: None,
                }
            }
        }
    }
}

/// 브리지의 업링크 포트
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Uplinks {
    /// 링크가 있는 포트
    pub linked: Vec<String>,
    /// 링크가 없는 포트
    pub down: Vec<String>,
}

/// 브리지 `bridge`의 업링크(`/sys/class/net/<br>/brif/*`). 물리 장치(`device`), 본딩(`bonding`, 본딩이 `down`이
/// 아니고 carrier가 있어야 링크), 다른 장치 위에 쌓은 장치(VLAN 등 — `lower_*`)만 센다. tap(`tun_flags`)·veth·
/// opld가 만든 장치(`oplx*`)처럼 물리 링크에 닿지 않는 포트는 뺀다. 브리지가 아니거나 업링크가 없으면(내부
/// 브리지) None이다.
pub fn bridge_uplinks(net: &NetFs, bridge: &str) -> Option<Uplinks> {
    let dir = net.net(bridge);
    if !dir.join("bridge").is_dir() {
        return None;
    }
    let mut ports: Vec<String> = std::fs::read_dir(dir.join("brif"))
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ports.sort();
    let mut uplinks = Uplinks::default();
    for port in ports {
        let pdir = net.net(&port);
        if pdir.join("tun_flags").exists() || is_managed_ifname(&port) {
            continue;
        }
        let stacked = std::fs::read_dir(&pdir).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("lower_"))
        });
        let linked = if pdir.join("bonding").is_dir() {
            carrier(net, &port) == Some(true)
                && bond_status(net, &port)
                    .ok()
                    .flatten()
                    .is_none_or(|b| b.state != BondState::Down)
        } else if pdir.join("device").exists() || stacked {
            carrier(net, &port) == Some(true)
        } else {
            continue;
        };
        if linked {
            uplinks.linked.push(port);
        } else {
            uplinks.down.push(port);
        }
    }
    (!uplinks.linked.is_empty() || !uplinks.down.is_empty()).then_some(uplinks)
}

/// `up`이 아닌 포트를 적은 문구(`bond0: eno2 down, eno3 lacp_mismatch`). 모두 `up`이면 이름만이다.
pub fn bond_detail(bond: &str, status: &BondStatus) -> String {
    let bad: Vec<String> = status
        .ports
        .iter()
        .filter(|p| p.state != PortState::Up)
        .map(|p| format!("{} {}", p.name, port_state_name(p.state)))
        .collect();
    if bad.is_empty() {
        bond.to_string()
    } else {
        format!("{bond}: {}", bad.join(", "))
    }
}

/// API에 쓰는 포트 상태 이름
pub fn port_state_name(state: PortState) -> &'static str {
    match state {
        PortState::Up => "up",
        PortState::Down => "down",
        PortState::LacpMismatch => "lacp_mismatch",
        PortState::SpeedMismatch => "speed_mismatch",
        PortState::Unknown => "unknown",
    }
}

/// 본딩의 포트 상태. 본딩이 아니면 `None`이다.
pub fn bond_status(net: &NetFs, bond: &str) -> std::io::Result<Option<BondStatus>> {
    let dir = net.net(bond).join("bonding");
    if !dir.is_dir() {
        return Ok(None);
    }
    let slaves = read_trim(&dir.join("slaves"))?;
    let mode = read_trim(&dir.join("mode"))?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    // 802.3ad면 aggregator와 상대(LACP partner)를 본다. 읽지 못하면 그 판정만 건너뛴다.
    let lacp = (mode == "802.3ad")
        .then(|| std::fs::read_to_string(net.proc.join("net/bonding").join(bond)).ok())
        .flatten()
        .map(|text| parse_proc_bonding(&text));
    struct Raw {
        name: String,
        down: bool,
        mismatch: bool,
        speed: Option<u32>,
        aggregator: Option<u32>,
    }
    let raw: Vec<Raw> = slaves
        .split_whitespace()
        .map(|name| {
            let proc_port = lacp
                .as_ref()
                .and_then(|l| l.ports.iter().find(|p| p.name == name));
            let down = carrier(net, name) != Some(true)
                || operstate(net, name).as_deref() == Some("down")
                || proc_port.is_some_and(|p| p.mii_up == Some(false));
            let aggregator = proc_port.and_then(|p| p.aggregator);
            let active = lacp.as_ref().and_then(|l| l.active_aggregator);
            let mismatch = proc_port.is_some_and(|p| {
                matches!((p.aggregator, active), (Some(a), Some(b)) if a != b)
                    || p.partner_mac.as_deref() == Some("00:00:00:00:00:00")
            });
            Raw {
                name: name.to_string(),
                down,
                mismatch,
                speed: speed(net, name),
                aggregator,
            }
        })
        .collect();
    let fastest = raw.iter().filter(|p| !p.down).filter_map(|p| p.speed).max();
    let ports: Vec<BondPort> = raw
        .into_iter()
        .map(|p| {
            let state = if p.down {
                PortState::Down
            } else if p.mismatch {
                PortState::LacpMismatch
            } else if matches!((p.speed, fastest), (Some(s), Some(f)) if s < f) {
                PortState::SpeedMismatch
            } else {
                PortState::Up
            };
            BondPort {
                name: p.name,
                state,
                speed_mbps: p.speed,
                aggregator: p.aggregator,
            }
        })
        .collect();
    let up = ports.iter().filter(|p| p.state == PortState::Up).count();
    // 링크가 있는 포트가 하나라도 있으면 트래픽은 지나간다(LACP 상대가 없는 802.3ad도 한 포트로 보낸다).
    let linked = ports.iter().any(|p| p.state != PortState::Down);
    let state = if !linked {
        BondState::Down
    } else if up < ports.len() {
        BondState::Degraded
    } else {
        BondState::Up
    };
    Ok(Some(BondStatus { mode, state, ports }))
}

/// `/proc/net/bonding/<bond>`에서 읽은 것
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcBonding {
    /// "Active Aggregator Info"의 Aggregator ID(802.3ad)
    pub active_aggregator: Option<u32>,
    pub ports: Vec<ProcPort>,
}

/// `/proc/net/bonding/<bond>`의 "Slave Interface" 하나
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcPort {
    pub name: String,
    /// "MII Status"
    pub mii_up: Option<bool>,
    /// "Aggregator ID"(802.3ad)
    pub aggregator: Option<u32>,
    /// "details partner lacp pdu"의 "system mac address"(802.3ad)
    pub partner_mac: Option<String>,
}

/// `/proc/net/bonding/<bond>`를 읽는다. 모르는 줄은 건너뛴다.
pub fn parse_proc_bonding(text: &str) -> ProcBonding {
    #[derive(PartialEq)]
    enum Part {
        Top,
        ActiveAggregator,
        Port,
        Actor,
        Partner,
    }
    let mut out = ProcBonding::default();
    let mut part = Part::Top;
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "Active Aggregator Info" => part = Part::ActiveAggregator,
            "Slave Interface" => {
                part = Part::Port;
                out.ports.push(ProcPort {
                    name: value.to_string(),
                    ..ProcPort::default()
                });
            }
            "details actor lacp pdu" if part != Part::Top => part = Part::Actor,
            "details partner lacp pdu" if part != Part::Top => part = Part::Partner,
            "Aggregator ID" if part == Part::ActiveAggregator => {
                out.active_aggregator = value.parse().ok();
            }
            _ => {
                let Some(port) = out.ports.last_mut() else {
                    continue;
                };
                match (&part, key) {
                    (Part::Port, "MII Status") => port.mii_up = Some(value == "up"),
                    (Part::Port, "Aggregator ID") => port.aggregator = value.parse().ok(),
                    (Part::Partner, "system mac address") => {
                        port.partner_mac = Some(value.to_ascii_lowercase());
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// `ip -j -d addr show`의 인터페이스 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpLink {
    pub name: String,
    /// operstate(소문자)
    pub state: String,
    pub master: Option<String>,
    pub mac: Option<String>,
    pub link_type: Option<String>,
    /// `linkinfo.info_kind`(`bond`, `bridge`, `vlan`, `tun` …)
    pub info_kind: Option<String>,
    /// IP 주소(CIDR)
    pub addresses: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AddrLink {
    ifname: String,
    #[serde(default)]
    operstate: Option<String>,
    #[serde(default)]
    master: Option<String>,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    link_type: Option<String>,
    #[serde(default)]
    linkinfo: Option<AddrLinkInfo>,
    #[serde(default)]
    addr_info: Vec<AddrInfo>,
}

#[derive(Debug, Deserialize)]
struct AddrLinkInfo {
    #[serde(default)]
    info_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddrInfo {
    #[serde(default)]
    local: Option<String>,
    #[serde(default)]
    prefixlen: Option<u8>,
}

/// `ip -j -d addr show`의 출력. 모르는 필드는 무시한다.
pub fn parse_ip_addr(json: &str) -> Result<Vec<IpLink>, String> {
    let links: Vec<AddrLink> =
        serde_json::from_str(json).map_err(|e| format!("unexpected ip output: {e}"))?;
    Ok(links
        .into_iter()
        .map(|l| IpLink {
            name: l.ifname,
            state: l.operstate.unwrap_or_default().to_ascii_lowercase(),
            master: l.master,
            mac: l.address,
            link_type: l.link_type,
            info_kind: l.linkinfo.and_then(|i| i.info_kind),
            addresses: l
                .addr_info
                .into_iter()
                .filter_map(|a| Some(format!("{}/{}", a.local?, a.prefixlen?)))
                .collect(),
        })
        .collect())
}

/// 노드 상태에 보일 인터페이스인지: `lo`, tap(`tun`), opld가 만든 장치(`oplv*`·`oplx*`)는 뺀다.
fn reported(link: &IpLink) -> bool {
    link.name != "lo"
        && link.link_type.as_deref() != Some("loopback")
        && link.info_kind.as_deref() != Some("tun")
        && !is_managed_ifname(&link.name)
}

fn interface_kind(link: &IpLink) -> InterfaceKind {
    match link.info_kind.as_deref() {
        Some("bond") => InterfaceKind::Bond,
        Some("bridge") => InterfaceKind::Bridge,
        Some("vlan") => InterfaceKind::Vlan,
        Some("dummy") => InterfaceKind::Dummy,
        Some("veth") => InterfaceKind::Veth,
        Some(_) => InterfaceKind::Other,
        None if matches!(link.link_type.as_deref(), None | Some("ether")) => {
            InterfaceKind::Ethernet
        }
        None => InterfaceKind::Other,
    }
}

/// 이 노드의 인터페이스 목록(모두, 노드 상태에는 [`reported_interfaces`]로 줄여 쓴다). `ip_json`은
/// `ip -j -d addr show`의 출력이고 carrier·속도·본딩은 sysfs에서 읽는다.
pub fn interfaces(net: &NetFs, ip_json: &str) -> Result<Vec<NodeInterface>, String> {
    let mut list: Vec<NodeInterface> = parse_ip_addr(ip_json)?
        .into_iter()
        .filter(reported)
        .map(|l| NodeInterface {
            kind: interface_kind(&l),
            name: l.name,
            state: l.state,
            carrier: None,
            speed_mbps: None,
            mac: l.mac,
            master: l.master,
            addresses: l.addresses,
            bond: None,
        })
        .collect();
    refresh_interfaces(net, &mut list);
    Ok(list)
}

/// 노드 상태에 쓸 목록: [`MAX_NODE_INTERFACES`]개까지, 넘치면 주소가 있는 인터페이스를 먼저 남긴다(순서는
/// 그대로).
pub fn reported_interfaces(list: &[NodeInterface]) -> Vec<NodeInterface> {
    if list.len() <= MAX_NODE_INTERFACES {
        return list.to_vec();
    }
    let addressed = list.iter().filter(|i| !i.addresses.is_empty()).count();
    let mut plain = MAX_NODE_INTERFACES.saturating_sub(addressed);
    let mut kept = 0;
    list.iter()
        .filter(|i| {
            let keep = if i.addresses.is_empty() {
                let room = plain > 0;
                plain = plain.saturating_sub(1);
                room
            } else {
                true
            };
            keep && {
                kept += 1;
                kept <= MAX_NODE_INTERFACES
            }
        })
        .cloned()
        .collect()
}

/// operstate·carrier·속도·본딩 포트를 sysfs에서 다시 읽는다(`ip`를 부르지 않는 짧은 회차).
/// 읽지 못한 operstate는 그대로 둔다.
pub fn refresh_interfaces(net: &NetFs, list: &mut [NodeInterface]) {
    for iface in list {
        if let Some(state) = operstate(net, &iface.name) {
            iface.state = state;
        }
        iface.carrier = carrier(net, &iface.name);
        iface.speed_mbps = speed(net, &iface.name);
        iface.bond = if iface.kind == InterfaceKind::Bond {
            bond_status(net, &iface.name).ok().flatten()
        } else {
            None
        };
    }
}

/// 핑 명령의 인자. 0.2초 간격으로 둘을 보내 하나라도 답하면 통과다(한 번의 손실로 실패하지 않게 — Linux는
/// 마지막 요청 뒤 `-W`초, macOS는 모두 `-t`초 안에 끝난다). Linux는 `-I`로 보낼 인터페이스를 고를 수 있고, macOS는
/// `via`·IPv6를 받지 않는다.
pub fn ping_args(
    addr: IpAddr,
    via: Option<&str>,
    timeout: Duration,
    linux: bool,
) -> Result<Vec<String>, String> {
    let secs = timeout.as_secs().max(1).to_string();
    let mut args: Vec<String> = ["-n", "-q", "-c", "2", "-i", "0.2"]
        .map(String::from)
        .to_vec();
    if linux {
        args.extend(["-W".to_string(), secs]);
        if let Some(via) = via {
            args.extend(["-I".to_string(), via.to_string()]);
        }
    } else {
        if via.is_some() {
            return Err("ping via an interface is not supported on this OS".into());
        }
        if addr.is_ipv6() {
            return Err("IPv6 ping is not supported on this OS".into());
        }
        args.extend(["-t".to_string(), secs]);
    }
    args.push(addr.to_string());
    Ok(args)
}

/// 명령으로 하는 probe(핑, `rados`)의 실패
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// 검사 명령이 없거나 실행할 수 없다(설치·권한 문제). 대상의 실패가 아니므로 검사는 `unknown`이다.
    Unavailable(String),
    /// 대상이 답하지 않았다(`failing`).
    Failed(String),
}

impl ProbeError {
    /// 이 실패의 검사 결과
    pub fn state(&self) -> CheckState {
        match self {
            Self::Unavailable(_) => CheckState::Unknown,
            Self::Failed(_) => CheckState::Failing,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable(m) | Self::Failed(m) => m,
        }
    }
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// 명령을 `limit` 안에 돌린다. 시간을 넘기면 프로세스를 끝낸다(`kill_on_drop`). 명령이 없거나 실행 권한이
/// 없으면 [`ProbeError::Unavailable`]이고 없을 때의 문구는 설치할 패키지(`package`)를 말한다.
async fn run_limited(
    bin: &Path,
    args: &[String],
    env: &[(&str, &str)],
    limit: Duration,
    package: &str,
) -> Result<Output, ProbeError> {
    let name = bin
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| bin.display().to_string());
    let run = tokio::process::Command::new(bin)
        .args(args)
        .env("LC_ALL", "C")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(limit, run).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(ProbeError::Unavailable(
            format!("{name} not found: install {package}"),
        )),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(ProbeError::Unavailable(format!("cannot run {name}: {e}")))
        }
        Ok(Err(e)) => Err(ProbeError::Failed(format!("cannot run {name}: {e}"))),
        Err(_) => Err(ProbeError::Failed(format!(
            "{name} timed out after {limit:?}"
        ))),
    }
}

/// 실패한 명령의 stderr 첫 줄. 비어 있으면 종료 상태다.
fn first_line(stderr: &[u8], status: ExitStatus) -> String {
    failure_text(stderr, status)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// 시스템 핑(`bin`)으로 주소 하나에 묻는다([`ping_args`]). ICMP 권한은 핑 실행 파일의 것을 쓴다. 핑이 없거나 실행할 수
/// 없으면 [`ProbeError::Unavailable`]이다.
pub async fn ping(
    bin: &Path,
    addr: IpAddr,
    via: Option<&str>,
    timeout: Duration,
) -> Result<(), ProbeError> {
    let args =
        ping_args(addr, via, timeout, cfg!(target_os = "linux")).map_err(ProbeError::Failed)?;
    let out = match run_limited(bin, &args, &[], timeout + COMMAND_SLACK, "iputils-ping").await {
        Ok(out) => out,
        Err(ProbeError::Unavailable(e)) => return Err(ProbeError::Unavailable(e)),
        Err(ProbeError::Failed(e)) => return Err(ProbeError::Failed(format!("ping {addr}: {e}"))),
    };
    if out.status.success() {
        Ok(())
    } else {
        Err(ProbeError::Failed(format!(
            "ping {addr}: {}",
            first_line(&out.stderr, out.status)
        )))
    }
}

/// 디렉터리 풀 probe(블로킹): 디렉터리를 `stat`하고 `probe` 파일을 새로 열어 앞부분을 읽는다.
/// 파일이 없으면 한 번 만든다. NFS는 새로 열 때마다(close-to-open) 서버에 묻는다.
pub fn probe_dir(dir: &Path, probe: &str) -> std::io::Result<()> {
    if !std::fs::metadata(dir)?.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    let path = dir.join(probe);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&path, b"orbivisor health probe\n")?;
            std::fs::File::open(&path)?
        }
        Err(e) => return Err(e),
    };
    let mut buf = Vec::with_capacity(PROBE_READ);
    file.take(PROBE_READ as u64).read_to_end(&mut buf)?;
    Ok(())
}

/// 이 타깃(IQN)에 로그인된 iSCSI 세션이 있는지(`/sys/class/iscsi_session/session*`)
pub fn iscsi_logged_in(net: &NetFs, target: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(net.sys.join("class/iscsi_session")) else {
        return false;
    };
    entries.flatten().any(|e| {
        let dir = e.path();
        read_trim(&dir.join("targetname")).is_ok_and(|t| t == target)
            && read_trim(&dir.join("state")).is_ok_and(|s| s == "LOGGED_IN")
    })
}

/// PV 하나 아래의 경로 수
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paths {
    pub total: usize,
    pub running: usize,
}

/// 블록 장치 하나가 쓸 수 있는지. 장치 상태(`device/state`, 파티션은 부모의 것)가 있으면 SCSI는 `running`,
/// NVMe(컨트롤러 상태)는 `live`여야 하고, 없으면(virtio) 장치가 있는 것으로 충분하다.
fn block_running(net: &NetFs, name: &str) -> bool {
    let dir = net.sys.join("class/block").join(name);
    if !dir.exists() {
        return false;
    }
    [dir.join("device/state"), dir.join("../device/state")]
        .iter()
        .find_map(|p| read_trim(p).ok())
        .is_none_or(|state| state == "running" || state == "live")
}

/// device-mapper 이름(`/dev/mapper/<name>`)의 `dm-N`
fn dm_by_name(net: &NetFs, name: &str) -> Option<String> {
    std::fs::read_dir(net.sys.join("class/block"))
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("dm-"))
        .find(|dm| {
            read_trim(&net.sys.join("class/block").join(dm).join("dm/name"))
                .is_ok_and(|n| n == name)
        })
}

/// `/dev/disk/by-id/…`처럼 장치 노드를 가리키는 링크를 따라간 `/dev/<장치>`. 링크가 아니거나 따라가지 못하면
/// 그대로다.
fn resolve_dev(dev: &str) -> String {
    match std::fs::canonicalize(dev) {
        Ok(real) if real != Path::new(dev) => real.file_name().map_or_else(
            || dev.to_string(),
            |n| format!("/dev/{}", n.to_string_lossy()),
        ),
        _ => dev.to_string(),
    }
}

/// PV 장치(`pvs`의 `pv_name`)의 경로 수. dm(multipath)이면 그 아래 장치들, 아니면 장치 하나다. 링크 이름
/// (`/dev/disk/by-id/…`)은 먼저 따라간다.
pub fn pv_paths(net: &NetFs, dev: &str) -> Paths {
    let dev = &resolve_dev(dev);
    let dm = if let Some(name) = dev.strip_prefix("/dev/mapper/") {
        match dm_by_name(net, name) {
            Some(dm) => Some(dm),
            None => {
                return Paths {
                    total: 0,
                    running: 0,
                };
            }
        }
    } else {
        dev.strip_prefix("/dev/")
            .filter(|n| n.starts_with("dm-"))
            .map(str::to_string)
    };
    let Some(dm) = dm else {
        let name = dev.strip_prefix("/dev/").unwrap_or(dev);
        return Paths {
            total: 1,
            running: usize::from(block_running(net, name)),
        };
    };
    let mut paths = Vec::new();
    dm_paths(net, &dm, DM_DEPTH, &mut paths);
    Paths {
        total: paths.len(),
        running: paths.iter().filter(|s| block_running(net, s)).count(),
    }
}

/// dm 장치 `dm`의 아래 장치(`slaves`)를 `paths`에 더한다. 아래 장치가 또 dm이면(multipath 위의 파티션)
/// `depth`단까지 따라 내려가 실제 경로를 센다.
fn dm_paths(net: &NetFs, dm: &str, depth: usize, paths: &mut Vec<String>) {
    let slaves: Vec<String> =
        std::fs::read_dir(net.sys.join("class/block").join(dm).join("slaves"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
    for slave in slaves {
        if slave.starts_with("dm-") && depth > 1 {
            dm_paths(net, &slave, depth - 1, paths);
        } else {
            paths.push(slave);
        }
    }
}

#[derive(Debug, Deserialize)]
struct PvsReport {
    report: Vec<PvsEntry>,
}

#[derive(Debug, Deserialize)]
struct PvsEntry {
    #[serde(default)]
    pv: Vec<PvsRow>,
}

#[derive(Debug, Deserialize)]
struct PvsRow {
    pv_name: String,
    #[serde(default)]
    vg_name: String,
}

/// `pvs --reportformat json -o pv_name,vg_name`에서 VG `vg`의 PV 장치
pub fn parse_pvs(json: &[u8], vg: &str) -> Result<Vec<String>, String> {
    let report: PvsReport =
        serde_json::from_slice(json).map_err(|e| format!("unexpected pvs output: {e}"))?;
    Ok(report
        .report
        .into_iter()
        .flat_map(|r| r.pv)
        .filter(|r| r.vg_name == vg)
        .map(|r| r.pv_name)
        .collect())
}

/// VG `vg`의 PV 장치를 `pvs`(`bin`)로 읽는다.
pub async fn pvs(bin: &Path, vg: &str) -> Result<Vec<String>, String> {
    let args = ["--reportformat", "json", "-o", "pv_name,vg_name"].map(String::from);
    let out = run_limited(
        bin,
        &args,
        &[("LVM_SUPPRESS_FD_WARNINGS", "1")],
        PVS_TIMEOUT,
        "lvm2",
    )
    .await
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("pvs: {}", first_line(&out.stderr, out.status)));
    }
    parse_pvs(&out.stdout, vg)
}

/// RBD 풀 probe(`rados stat`)의 인자. 접속·OSD 응답을 3초로 묶는다.
pub fn rados_args(
    pool: &str,
    namespace: Option<&str>,
    user: Option<&str>,
    conf: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "--id".to_string(),
        user.unwrap_or(RBD_DEFAULT_USER).to_string(),
    ];
    if let Some(conf) = conf {
        args.extend(["--conf".to_string(), conf.to_string()]);
    }
    args.extend(["--pool".to_string(), pool.to_string()]);
    if let Some(ns) = namespace {
        args.extend(["--namespace".to_string(), ns.to_string()]);
    }
    args.extend(
        [
            "--client_mount_timeout",
            "3",
            "--rados_osd_op_timeout",
            "3",
            "stat",
            RBD_PROBE_OBJECT,
        ]
        .map(String::from),
    );
    args
}

/// `rados stat`의 결과. `stat` 자체가 객체가 없다고 답했으면(`error stat-ing …: (2) …`) OSD가 답한 것이므로
/// 통과다. 그 밖의 오류는 ENOENT여도(지워진 풀, 없는 keyring) 실패다. 오류 문구는 stderr의 마지막 줄에서 앞의
/// 시각·스레드 id를 뗀 것이다(회차마다 문구가 바뀌어 노드 상태를 다시 쓰지 않게).
pub fn rados_result(status: ExitStatus, stderr: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(stderr);
    let no_object = text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("error stat-ing") && line.contains("(2)")
    });
    if status.success() || no_object {
        return Ok(());
    }
    let last = failure_text(stderr, status)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(|l| strip_log_prefix(l).to_string())
        .unwrap_or_default();
    Err(format!("rados: {last}"))
}

/// Ceph 로그 줄 앞의 시각(`2026-11-14T10:00:00.123+0000`, 옛 형식은 날짜와 시각 두 낱말)과 스레드 id(16진수)를
/// 뗀다. 로그 줄이 아니면 그대로다.
fn strip_log_prefix(line: &str) -> &str {
    let is_date = |t: &str| {
        let b = t.as_bytes();
        b.len() >= 10 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[7] == b'-'
    };
    let Some((first, mut rest)) = line.split_once(' ') else {
        return line;
    };
    if !is_date(first) {
        return line;
    }
    if !first.contains('T')
        && let Some((time, after)) = rest.split_once(' ')
        && time.contains(':')
    {
        rest = after;
    }
    if let Some((thread, after)) = rest.split_once(' ')
        && !thread.is_empty()
        && thread.bytes().all(|b| b.is_ascii_hexdigit())
    {
        rest = after;
    }
    rest.trim_start()
}

/// `rados`(`bin`)로 RBD 풀에 묻는다. `args`는 [`rados_args`]다. `rados`가 없거나 실행할 수 없으면
/// [`ProbeError::Unavailable`]이다.
pub async fn rados(bin: &Path, args: &[String]) -> Result<(), ProbeError> {
    let out = run_limited(bin, args, &[], RBD_PROBE_TIMEOUT, "ceph-common").await?;
    rados_result(out.status, &out.stderr).map_err(ProbeError::Failed)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    /// 가짜 sysfs의 인터페이스 하나
    #[derive(Default)]
    struct FakeLink<'a> {
        name: &'a str,
        operstate: &'a str,
        /// 없으면 `carrier` 파일을 만들지 않는다(내려간 인터페이스처럼 읽기 실패).
        carrier: Option<u8>,
        speed: Option<i64>,
        /// 본딩이면 (모드 줄, 포트)
        bond: Option<(&'a str, &'a [&'a str])>,
    }

    fn link<'a>(name: &'a str, operstate: &'a str, carrier: u8) -> FakeLink<'a> {
        FakeLink {
            name,
            operstate,
            carrier: Some(carrier),
            ..FakeLink::default()
        }
    }

    /// `root` 아래에 sysfs·procfs 트리를 만든다.
    fn fake_net(root: &Path, links: &[FakeLink]) -> NetFs {
        let net = NetFs::under(root);
        std::fs::create_dir_all(net.proc.join("net/bonding")).unwrap();
        for l in links {
            let dir = net.net(l.name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("operstate"), format!("{}\n", l.operstate)).unwrap();
            if let Some(c) = l.carrier {
                std::fs::write(dir.join("carrier"), format!("{c}\n")).unwrap();
            }
            if let Some(s) = l.speed {
                std::fs::write(dir.join("speed"), format!("{s}\n")).unwrap();
            }
            if let Some((mode, ports)) = l.bond {
                std::fs::create_dir_all(dir.join("bonding")).unwrap();
                std::fs::write(dir.join("bonding/mode"), format!("{mode}\n")).unwrap();
                std::fs::write(dir.join("bonding/slaves"), format!("{}\n", ports.join(" ")))
                    .unwrap();
            }
        }
        net
    }

    fn write_proc_bonding(net: &NetFs, bond: &str, text: &str) {
        std::fs::write(net.proc.join("net/bonding").join(bond), text).unwrap();
    }

    const OK_8023AD: &str = include_str!("../tests/fixtures/bonding/8023ad-ok.txt");
    const PORT_DOWN_8023AD: &str = include_str!("../tests/fixtures/bonding/8023ad-port-down.txt");
    const AGG_MISMATCH_8023AD: &str =
        include_str!("../tests/fixtures/bonding/8023ad-aggregator-mismatch.txt");
    const NO_PARTNER_8023AD: &str = include_str!("../tests/fixtures/bonding/8023ad-no-partner.txt");
    const ACTIVE_BACKUP: &str = include_str!("../tests/fixtures/bonding/active-backup.txt");

    #[test]
    fn plain_links() {
        let tmp = tempfile::tempdir().unwrap();
        let net = fake_net(
            tmp.path(),
            &[
                link("eth0", "up", 1),
                link("eth1", "up", 0),
                link("eth2", "down", 1),
                link("dummy0", "unknown", 1),
                FakeLink {
                    name: "eth3",
                    operstate: "down",
                    ..FakeLink::default()
                },
            ],
        );
        let ok = link_state(&net, "eth0");
        assert_eq!((ok.state, ok.error, ok.bond), (CheckState::Ok, None, None));
        assert_eq!(bridge_uplinks(&net, "eth0"), None);
        // dummy는 operstate가 unknown이지만 carrier가 있으면 통과다.
        assert_eq!(link_state(&net, "dummy0").state, CheckState::Ok);
        let no_carrier = link_state(&net, "eth1");
        assert_eq!(no_carrier.state, CheckState::Failing);
        assert_eq!(
            no_carrier.error.as_deref(),
            Some("eth1 is down (operstate up, carrier 0)")
        );
        assert_eq!(link_state(&net, "eth2").state, CheckState::Failing);
        // carrier를 읽지 못하면 0으로 본다.
        assert_eq!(link_state(&net, "eth3").state, CheckState::Failing);
        // 없는 인터페이스는 이 노드에 해당 없음이다(본 적이 있는지는 검사기가 안다).
        let missing = link_state(&net, "nope0");
        assert_eq!(missing.state, CheckState::Unknown);
        assert_eq!(
            missing.error.as_deref(),
            Some("interface nope0 not found on this node")
        );
    }

    /// 브리지 `br` 아래 포트를 만든다: 이름과 종류 표시(`device`·`tun_flags`·`lower_eth0`·`bonding` 등 sysfs 항목).
    fn bridge(net: &NetFs, br: &str, ports: &[(&str, &str)]) {
        let dir = net.net(br);
        std::fs::create_dir_all(dir.join("bridge")).unwrap();
        std::fs::create_dir_all(dir.join("brif")).unwrap();
        for (port, marker) in ports {
            std::fs::create_dir_all(dir.join("brif").join(port)).unwrap();
            if !marker.is_empty() {
                std::fs::create_dir_all(net.net(port).join(marker)).unwrap();
            }
        }
    }

    /// 브리지의 링크는 업링크로 본다: tap(carrier를 지킴)·veth·관리 장치는 빼고 물리·VLAN·본딩 포트 가운데 링크가
    /// 있는 것이 하나도 없으면 실패다. 업링크가 없는 내부 브리지는 브리지 자신의 carrier로 본다.
    #[test]
    fn bridges_are_judged_by_their_uplinks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut links = vec![
            link("br0", "up", 1),
            link("eno1", "up", 0),
            link("eno1.20", "lowerlayerdown", 0),
            link("op0123456789n0", "up", 1),
            link("veth1", "up", 1),
            link("oplx5", "unknown", 1),
            link("br1", "up", 1),
            link("op9876543210n0", "up", 1),
            link("br2", "up", 1),
            link("eno3", "up", 1),
        ];
        links.extend(bond_links(
            "active-backup 1",
            &["eno7", "eno8"],
            [false, false],
        ));
        let net = fake_net(tmp.path(), &links);
        let tap = ("op0123456789n0", "tun_flags");
        bridge(
            &net,
            "br0",
            &[
                ("eno1", "device"),
                ("eno1.20", "lower_eno1"),
                ("bond0", ""),
                tap,
                ("veth1", ""),
                ("oplx5", ""),
            ],
        );
        let down = link_state(&net, "br0");
        assert_eq!(down.state, CheckState::Failing);
        assert_eq!(
            down.error.as_deref(),
            Some("bridge br0 has no uplink with a link (bond0, eno1, eno1.20 down)")
        );
        // 업링크 하나가 살아나면 통과다.
        std::fs::write(net.net("eno1.20").join("carrier"), "1\n").unwrap();
        assert_eq!(link_state(&net, "br0").state, CheckState::Ok);
        assert_eq!(
            bridge_uplinks(&net, "br0").unwrap(),
            Uplinks {
                linked: vec!["eno1.20".into()],
                down: vec!["bond0".into(), "eno1".into()],
            }
        );
        // 내부 브리지(tap만)는 브리지 자신의 carrier다.
        bridge(&net, "br1", &[("op9876543210n0", "tun_flags")]);
        assert_eq!(bridge_uplinks(&net, "br1"), None);
        assert_eq!(link_state(&net, "br1").state, CheckState::Ok);
        bridge(&net, "br2", &[("eno3", "device")]);
        assert_eq!(link_state(&net, "br2").state, CheckState::Ok);
    }

    fn bond_links<'a>(
        mode: &'a str,
        ports: &'a [&'a str],
        port_up: [bool; 2],
    ) -> Vec<FakeLink<'a>> {
        let mut links = vec![FakeLink {
            name: "bond0",
            operstate: if port_up.contains(&true) {
                "up"
            } else {
                "down"
            },
            carrier: Some(u8::from(port_up.contains(&true))),
            bond: Some((mode, ports)),
            ..FakeLink::default()
        }];
        for (name, up) in ports.iter().zip(port_up) {
            links.push(FakeLink {
                name,
                operstate: if up { "up" } else { "down" },
                carrier: Some(u8::from(up)),
                speed: Some(if up { 10000 } else { -1 }),
                ..FakeLink::default()
            });
        }
        links
    }

    #[test]
    fn active_backup_bond() {
        let ports: &[&str] = &["eno1", "eno2"];
        for (port_up, bond_state, check, detail) in [
            ([true, true], BondState::Up, CheckState::Ok, None),
            (
                [true, false],
                BondState::Degraded,
                CheckState::Degraded,
                Some("bond0: eno2 down"),
            ),
            (
                [false, false],
                BondState::Down,
                CheckState::Failing,
                Some("bond is down: bond0: eno1 down, eno2 down"),
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let net = fake_net(tmp.path(), &bond_links("active-backup 1", ports, port_up));
            let bond = bond_status(&net, "bond0").unwrap().unwrap();
            assert_eq!(bond.mode, "active-backup");
            assert_eq!(bond.state, bond_state, "{port_up:?}");
            assert_eq!(bond.ports.len(), 2);
            let probe = link_state(&net, "bond0");
            assert_eq!(probe.state, check, "{port_up:?}");
            assert_eq!(probe.error.as_deref(), detail);
            assert_eq!(probe.bond, Some(bond));
        }
        // 본딩이 아니면 None이다.
        let tmp = tempfile::tempdir().unwrap();
        let net = fake_net(tmp.path(), &[link("eth0", "up", 1)]);
        assert_eq!(bond_status(&net, "eth0").unwrap(), None);
    }

    #[test]
    fn bond_without_carrier_fails_even_with_ports_up() {
        let tmp = tempfile::tempdir().unwrap();
        let mut links = bond_links("active-backup 1", &["eno1", "eno2"], [true, true]);
        links[0].carrier = Some(0);
        let net = fake_net(tmp.path(), &links);
        let probe = link_state(&net, "bond0");
        assert_eq!(probe.state, CheckState::Failing);
        assert_eq!(probe.error.as_deref(), Some("bond bond0 has no carrier"));
    }

    #[test]
    fn parses_proc_bonding() {
        let ok = parse_proc_bonding(OK_8023AD);
        assert_eq!(ok.active_aggregator, Some(1));
        assert_eq!(
            ok.ports,
            vec![
                ProcPort {
                    name: "eno1".into(),
                    mii_up: Some(true),
                    aggregator: Some(1),
                    partner_mac: Some("00:11:22:33:44:55".into()),
                },
                ProcPort {
                    name: "eno2".into(),
                    mii_up: Some(true),
                    aggregator: Some(1),
                    partner_mac: Some("00:11:22:33:44:55".into()),
                },
            ]
        );
        let down = parse_proc_bonding(PORT_DOWN_8023AD);
        assert_eq!(down.ports[1].mii_up, Some(false));
        assert_eq!(down.ports[1].aggregator, Some(2));
        let mismatch = parse_proc_bonding(AGG_MISMATCH_8023AD);
        assert_eq!(mismatch.ports[1].aggregator, Some(2));
        let alone = parse_proc_bonding(NO_PARTNER_8023AD);
        assert_eq!(
            alone.ports[1].partner_mac.as_deref(),
            Some("00:00:00:00:00:00")
        );
        // active-backup에는 aggregator·상대가 없다.
        let ab = parse_proc_bonding(ACTIVE_BACKUP);
        assert_eq!(ab.active_aggregator, None);
        assert_eq!(
            ab.ports
                .iter()
                .map(|p| (
                    p.name.as_str(),
                    p.mii_up,
                    p.aggregator,
                    p.partner_mac.is_some()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("eno1", Some(true), None, false),
                ("eno2", Some(false), None, false)
            ]
        );
    }

    #[test]
    fn lacp_port_states() {
        let ports: &[&str] = &["eno1", "eno2"];
        for (text, port_up, eno2, bond_state) in [
            (OK_8023AD, [true, true], PortState::Up, BondState::Up),
            (
                PORT_DOWN_8023AD,
                [true, false],
                PortState::Down,
                BondState::Degraded,
            ),
            // sysfs로는 올라와 있어도 MII down이면 down이다.
            (
                PORT_DOWN_8023AD,
                [true, true],
                PortState::Down,
                BondState::Degraded,
            ),
            (
                AGG_MISMATCH_8023AD,
                [true, true],
                PortState::LacpMismatch,
                BondState::Degraded,
            ),
            (
                NO_PARTNER_8023AD,
                [true, true],
                PortState::LacpMismatch,
                BondState::Degraded,
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let net = fake_net(tmp.path(), &bond_links("802.3ad 4", ports, port_up));
            write_proc_bonding(&net, "bond0", text);
            let bond = bond_status(&net, "bond0").unwrap().unwrap();
            assert_eq!(bond.mode, "802.3ad");
            assert_eq!(bond.ports[0].state, PortState::Up);
            assert_eq!(bond.ports[0].aggregator, Some(1));
            assert_eq!(bond.ports[1].state, eno2, "{text}");
            assert_eq!(bond.state, bond_state);
        }
    }

    /// LACP 상대가 없는 802.3ad(스위치에 LACP 설정이 없음): 모든 포트가 `lacp_mismatch`여도 링크가 있으면
    /// 트래픽은 지나가므로 본딩은 `degraded`(통과)다. 링크가 있는 포트가 없을 때만 `down`이다.
    #[test]
    fn a_bond_without_any_lacp_partner_is_degraded_not_down() {
        let ports: &[&str] = &["eno1", "eno2"];
        let alone = OK_8023AD.replace("00:11:22:33:44:55", "00:00:00:00:00:00");
        let tmp = tempfile::tempdir().unwrap();
        let net = fake_net(tmp.path(), &bond_links("802.3ad 4", ports, [true, true]));
        write_proc_bonding(&net, "bond0", &alone);
        let bond = bond_status(&net, "bond0").unwrap().unwrap();
        assert_eq!(
            bond.ports.iter().map(|p| p.state).collect::<Vec<_>>(),
            [PortState::LacpMismatch, PortState::LacpMismatch]
        );
        assert_eq!(bond.state, BondState::Degraded);
        let probe = link_state(&net, "bond0");
        assert_eq!(probe.state, CheckState::Degraded);
        assert_eq!(
            probe.error.as_deref(),
            Some("bond0: eno1 lacp_mismatch, eno2 lacp_mismatch")
        );

        let tmp = tempfile::tempdir().unwrap();
        let net = fake_net(tmp.path(), &bond_links("802.3ad 4", ports, [false, false]));
        write_proc_bonding(&net, "bond0", &alone);
        assert_eq!(
            bond_status(&net, "bond0").unwrap().unwrap().state,
            BondState::Down
        );
        assert_eq!(link_state(&net, "bond0").state, CheckState::Failing);
    }

    #[test]
    fn slower_port_is_a_speed_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let mut links = bond_links("balance-xor 2", &["eno1", "eno2"], [true, true]);
        links[2].speed = Some(1000);
        let net = fake_net(tmp.path(), &links);
        let bond = bond_status(&net, "bond0").unwrap().unwrap();
        assert_eq!(
            bond.ports
                .iter()
                .map(|p| (p.state, p.speed_mbps))
                .collect::<Vec<_>>(),
            vec![
                (PortState::Up, Some(10000)),
                (PortState::SpeedMismatch, Some(1000))
            ]
        );
        assert_eq!(bond.state, BondState::Degraded);
        assert_eq!(bond_detail("bond0", &bond), "bond0: eno2 speed_mismatch");
        // 내려간 포트의 속도는 비교에 넣지 않는다.
        let tmp = tempfile::tempdir().unwrap();
        let mut links = bond_links("balance-xor 2", &["eno1", "eno2"], [false, true]);
        links[1].speed = Some(25000);
        links[2].speed = Some(1000);
        let net = fake_net(tmp.path(), &links);
        let bond = bond_status(&net, "bond0").unwrap().unwrap();
        assert_eq!(bond.ports[1].state, PortState::Up);
    }

    const IP_ADDR: &str = r#"[
      {"ifindex":1,"ifname":"lo","flags":["LOOPBACK","UP","LOWER_UP"],"mtu":65536,"operstate":"UNKNOWN","link_type":"loopback","address":"00:00:00:00:00:00","addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8,"scope":"host"}]},
      {"ifindex":2,"ifname":"eno1","flags":["BROADCAST","MULTICAST","SLAVE","UP","LOWER_UP"],"mtu":1500,"master":"bond0","operstate":"UP","link_type":"ether","address":"52:54:00:aa:bb:01","linkinfo":{"info_slave_kind":"bond","info_slave_data":{"state":"ACTIVE"}},"addr_info":[]},
      {"ifindex":3,"ifname":"bond0","flags":["BROADCAST","MULTICAST","MASTER","UP","LOWER_UP"],"mtu":1500,"master":"br0","operstate":"UP","link_type":"ether","address":"52:54:00:aa:bb:01","linkinfo":{"info_kind":"bond","info_data":{"mode":"active-backup"},"info_slave_kind":"bridge"},"addr_info":[]},
      {"ifindex":4,"ifname":"br0","operstate":"UP","link_type":"ether","address":"52:54:00:aa:bb:01","linkinfo":{"info_kind":"bridge","info_data":{"stp_state":0}},"addr_info":[{"family":"inet","local":"10.0.0.5","prefixlen":24},{"family":"inet6","local":"fd00::5","prefixlen":64}]},
      {"ifindex":5,"ifname":"br0.100","link":"br0","operstate":"UP","link_type":"ether","linkinfo":{"info_kind":"vlan","info_data":{"protocol":"802.1Q","id":100}},"addr_info":[]},
      {"ifindex":6,"ifname":"dummy0","operstate":"UNKNOWN","link_type":"ether","linkinfo":{"info_kind":"dummy"}},
      {"ifindex":7,"ifname":"veth1","operstate":"LOWERLAYERDOWN","link_type":"ether","linkinfo":{"info_kind":"veth"}},
      {"ifindex":8,"ifname":"op0123456789n0","master":"br0","operstate":"UP","link_type":"ether","linkinfo":{"info_kind":"tun","info_data":{"type":"tap"}}},
      {"ifindex":9,"ifname":"oplv1","operstate":"UP","link_type":"ether","linkinfo":{"info_kind":"bridge"}},
      {"ifindex":10,"ifname":"oplx1","master":"oplv1","operstate":"UNKNOWN","link_type":"ether","linkinfo":{"info_kind":"vxlan"}},
      {"ifindex":11,"ifname":"vxlan9","operstate":"UNKNOWN","link_type":"ether","linkinfo":{"info_kind":"vxlan"}},
      {"ifindex":12,"ifname":"wg0","operstate":"UNKNOWN","link_type":"none"}
    ]"#;

    #[test]
    fn lists_interfaces_from_ip_and_sysfs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut links = bond_links("active-backup 1", &["eno1", "eno2"], [true, false]);
        links.push(link("br0", "up", 1));
        let net = fake_net(tmp.path(), &links);
        let list = interfaces(&net, IP_ADDR).unwrap();
        assert_eq!(
            list.iter()
                .map(|i| (i.name.as_str(), i.kind))
                .collect::<Vec<_>>(),
            vec![
                ("eno1", InterfaceKind::Ethernet),
                ("bond0", InterfaceKind::Bond),
                ("br0", InterfaceKind::Bridge),
                ("br0.100", InterfaceKind::Vlan),
                ("dummy0", InterfaceKind::Dummy),
                ("veth1", InterfaceKind::Veth),
                ("vxlan9", InterfaceKind::Other),
                ("wg0", InterfaceKind::Other),
            ]
        );
        let eno1 = &list[0];
        assert_eq!(eno1.master.as_deref(), Some("bond0"));
        assert_eq!(eno1.mac.as_deref(), Some("52:54:00:aa:bb:01"));
        assert_eq!((eno1.carrier, eno1.speed_mbps), (Some(true), Some(10000)));
        assert_eq!(eno1.state, "up");
        let bond0 = &list[1];
        assert_eq!(bond0.master.as_deref(), Some("br0"));
        assert_eq!(bond0.bond.as_ref().unwrap().state, BondState::Degraded);
        let br0 = &list[2];
        assert_eq!(br0.addresses, ["10.0.0.5/24", "fd00::5/64"]);
        assert_eq!(br0.bond, None);
        // sysfs에 없는 인터페이스는 ip의 operstate를 소문자로 쓰고 carrier·속도는 모른다.
        let veth = &list[5];
        assert_eq!(veth.state, "lowerlayerdown");
        assert_eq!((veth.carrier, veth.speed_mbps), (None, None));
        assert!(veth.addresses.is_empty());

        // 짧은 회차는 sysfs만 다시 읽는다.
        std::fs::write(net.net("eno1").join("carrier"), "0\n").unwrap();
        std::fs::write(net.net("eno1").join("operstate"), "down\n").unwrap();
        let mut list = list;
        refresh_interfaces(&net, &mut list);
        assert_eq!(
            (list[0].state.as_str(), list[0].carrier),
            ("down", Some(false))
        );
        assert_eq!(list[1].bond.as_ref().unwrap().state, BondState::Down);

        assert!(interfaces(&net, "not json").is_err());
    }

    /// 목록은 모두 읽고(자동 검사가 쓴다), 노드 상태에는 64개까지 쓰되 주소가 있는 인터페이스를 먼저 남긴다.
    #[test]
    fn interface_list_is_capped_for_the_node_status() {
        let many: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                let addr: Vec<serde_json::Value> = if i == 90 {
                    vec![serde_json::json!({"local": "10.0.0.90", "prefixlen": 24})]
                } else {
                    vec![]
                };
                serde_json::json!({"ifname": format!("eth{i}"), "operstate": "UP", "addr_info": addr})
            })
            .collect();
        let json = serde_json::to_string(&many).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let list = interfaces(&NetFs::under(tmp.path()), &json).unwrap();
        assert_eq!(list.len(), 100);
        let reported = reported_interfaces(&list);
        assert_eq!(reported.len(), MAX_NODE_INTERFACES);
        let names: Vec<&str> = reported.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names[62], "eth62");
        assert_eq!(names[63], "eth90");
        assert_eq!(reported_interfaces(&list[..10]), list[..10]);
    }

    #[test]
    fn ping_arguments() {
        let v4: IpAddr = "192.0.2.1".parse().unwrap();
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        let t = Duration::from_secs(2);
        assert_eq!(
            ping_args(v4, None, t, true).unwrap(),
            ["-n", "-q", "-c", "2", "-i", "0.2", "-W", "2", "192.0.2.1"]
        );
        assert_eq!(
            ping_args(v4, Some("bond1"), t, true).unwrap(),
            [
                "-n",
                "-q",
                "-c",
                "2",
                "-i",
                "0.2",
                "-W",
                "2",
                "-I",
                "bond1",
                "192.0.2.1"
            ]
        );
        assert_eq!(
            ping_args(v6, None, t, true).unwrap(),
            ["-n", "-q", "-c", "2", "-i", "0.2", "-W", "2", "2001:db8::1"]
        );
        assert_eq!(
            ping_args(v4, None, Duration::from_millis(300), false).unwrap(),
            ["-n", "-q", "-c", "2", "-i", "0.2", "-t", "1", "192.0.2.1"]
        );
        let via = ping_args(v4, Some("eth0"), t, false).unwrap_err();
        assert!(via.contains("not supported on this OS"), "{via}");
        let v6_mac = ping_args(v6, None, t, false).unwrap_err();
        assert!(v6_mac.contains("not supported on this OS"), "{v6_mac}");
    }

    /// 받은 인자를 `args`에 적고 `body`를 실행하는 가짜 명령
    fn fake_command(dir: &Path, name: &str, body: &str) -> (PathBuf, PathBuf) {
        let bin = dir.join(name);
        let args = dir.join(format!("{name}.args"));
        crate::storage::tests::write_script(
            &bin,
            &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {args:?}\n{body}\n"),
        );
        (bin, args)
    }

    #[tokio::test]
    async fn ping_runs_the_configured_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let addr: IpAddr = "192.0.2.7".parse().unwrap();
        let (ok, args) = fake_command(tmp.path(), "ping-ok", "exit 0");
        ping(&ok, addr, None, PING_TIMEOUT).await.unwrap();
        let want = ping_args(addr, None, PING_TIMEOUT, cfg!(target_os = "linux")).unwrap();
        assert_eq!(
            std::fs::read_to_string(args)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            want
        );

        let (fail, _) = fake_command(
            tmp.path(),
            "ping-fail",
            "echo 'ping: unreachable' >&2\nexit 1",
        );
        assert_eq!(
            ping(&fail, addr, None, PING_TIMEOUT).await.unwrap_err(),
            ProbeError::Failed("ping 192.0.2.7: ping: unreachable".into())
        );
        let (silent, _) = fake_command(tmp.path(), "ping-silent", "exit 1");
        assert_eq!(
            ping(&silent, addr, None, PING_TIMEOUT).await.unwrap_err(),
            ProbeError::Failed("ping 192.0.2.7: exit status 1".into())
        );

        // 답하지 않는 명령은 시간 제한에 끝난다.
        let (slow, _) = fake_command(tmp.path(), "ping-slow", "exec sleep 30");
        let started = std::time::Instant::now();
        let err = ping(&slow, addr, None, Duration::from_millis(100))
            .await
            .unwrap_err();
        assert_eq!(err.state(), CheckState::Failing);
        assert!(err.message().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));

        // 핑이 없거나 실행할 수 없으면 대상의 실패가 아니다(`unknown`).
        let err = ping(&tmp.path().join("ping"), addr, None, PING_TIMEOUT)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            ProbeError::Unavailable("ping not found: install iputils-ping".into())
        );
        assert_eq!(err.state(), CheckState::Unknown);
        let not_executable = tmp.path().join("ping-noexec");
        std::fs::write(&not_executable, "#!/bin/sh\nexit 0\n").unwrap();
        let err = ping(&not_executable, addr, None, PING_TIMEOUT)
            .await
            .unwrap_err();
        assert_eq!(err.state(), CheckState::Unknown, "{err}");
        assert!(
            err.message().starts_with("cannot run ping-noexec: "),
            "{err}"
        );
    }

    #[tokio::test]
    async fn real_ping_to_localhost() {
        // 시스템 핑이 없으면(최소 이미지) 건너뛴다. 127.0.0.1에만 보낸다.
        if std::process::Command::new("ping")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("no ping binary; skipping");
            return;
        }
        let r = ping(
            Path::new("ping"),
            "127.0.0.1".parse().unwrap(),
            None,
            PING_TIMEOUT,
        )
        .await;
        // 컨테이너는 ICMP 권한이 없을 수 있다. 그때는 실패 문구만 확인한다.
        if let Err(e) = r {
            assert!(e.message().starts_with("ping 127.0.0.1: "), "{e}");
            eprintln!("ping to 127.0.0.1 failed here: {e}");
        }
    }

    #[test]
    fn probes_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let probe = ".orbivisor-health-n1";
        probe_dir(tmp.path(), probe).unwrap();
        let file = tmp.path().join(probe);
        let first = std::fs::read(&file).unwrap();
        assert!(!first.is_empty());
        // 다음 probe는 만들지 않고 다시 읽는다.
        std::fs::write(&file, b"kept").unwrap();
        probe_dir(tmp.path(), probe).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"kept");

        let missing = probe_dir(&tmp.path().join("gone"), probe).unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
        assert!(probe_dir(&file, probe).is_err());
    }

    fn iscsi_session(net: &NetFs, n: u32, target: &str, state: &str) {
        let dir = net.sys.join(format!("class/iscsi_session/session{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("targetname"), format!("{target}\n")).unwrap();
        std::fs::write(dir.join("state"), format!("{state}\n")).unwrap();
    }

    #[test]
    fn iscsi_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let net = NetFs::under(tmp.path());
        let target = "iqn.2026-01.example:store";
        assert!(!iscsi_logged_in(&net, target));
        iscsi_session(&net, 1, target, "FAILED");
        iscsi_session(&net, 2, "iqn.2026-01.example:other", "LOGGED_IN");
        assert!(!iscsi_logged_in(&net, target));
        iscsi_session(&net, 3, target, "LOGGED_IN");
        assert!(iscsi_logged_in(&net, target));
    }

    fn block(net: &NetFs, name: &str, state: Option<&str>) -> PathBuf {
        let dir = net.sys.join("class/block").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(state) = state {
            std::fs::create_dir_all(dir.join("device")).unwrap();
            std::fs::write(dir.join("device/state"), format!("{state}\n")).unwrap();
        }
        dir
    }

    fn dm(net: &NetFs, n: u32, name: &str, slaves: &[&str]) {
        let dir = block(net, &format!("dm-{n}"), None);
        std::fs::create_dir_all(dir.join("dm")).unwrap();
        std::fs::write(dir.join("dm/name"), format!("{name}\n")).unwrap();
        std::fs::create_dir_all(dir.join("slaves")).unwrap();
        for s in slaves {
            std::fs::write(dir.join("slaves").join(s), "").unwrap();
        }
    }

    #[test]
    fn multipath_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let net = NetFs::under(tmp.path());
        block(&net, "sdb", Some("running"));
        block(&net, "sdc", Some("offline"));
        block(&net, "sdd", Some("running"));
        block(&net, "sde", Some("transport-offline"));
        block(&net, "vdb", None);
        dm(&net, 0, "vg0-root", &[]);
        dm(&net, 3, "mpatha", &["sdb", "sdc", "sdd", "sde"]);
        let two_of_four = Paths {
            total: 4,
            running: 2,
        };
        assert_eq!(pv_paths(&net, "/dev/mapper/mpatha"), two_of_four);
        assert_eq!(pv_paths(&net, "/dev/dm-3"), two_of_four);
        assert_eq!(
            pv_paths(&net, "/dev/mapper/nope"),
            Paths {
                total: 0,
                running: 0
            }
        );
        // multipath 위의 파티션(kpartx, dm 위의 dm)은 그 아래 경로를 센다.
        dm(&net, 4, "mpatha1", &["dm-3"]);
        assert_eq!(pv_paths(&net, "/dev/mapper/mpatha1"), two_of_four);
        assert_eq!(pv_paths(&net, "/dev/dm-4"), two_of_four);
        // 깊이 제한: 스스로를 가리키는 dm도 끝난다(마지막 단은 장치 하나로 센다).
        dm(&net, 5, "loop", &["dm-5"]);
        assert_eq!(pv_paths(&net, "/dev/dm-5").total, 1);
        // dm이 아니면 경로 하나다. SCSI 상태가 없는 장치(virtio)는 있으면 돈다.
        let one = |running| Paths { total: 1, running };
        assert_eq!(pv_paths(&net, "/dev/sdb"), one(1));
        assert_eq!(pv_paths(&net, "/dev/sdc"), one(0));
        assert_eq!(pv_paths(&net, "/dev/vdb"), one(1));
        assert_eq!(pv_paths(&net, "/dev/sdz"), one(0));
        // NVMe는 컨트롤러 상태가 `live`면 돈다.
        block(&net, "nvme0n1", Some("live"));
        block(&net, "nvme1n1", Some("connecting"));
        assert_eq!(pv_paths(&net, "/dev/nvme0n1"), one(1));
        assert_eq!(pv_paths(&net, "/dev/nvme1n1"), one(0));
        // `/dev/disk/by-id/…` 같은 링크는 따라간 장치로 센다.
        let dev = tmp.path().join("dev");
        std::fs::create_dir_all(dev.join("disk/by-id")).unwrap();
        std::fs::write(dev.join("dm-3"), "").unwrap();
        std::os::unix::fs::symlink("../../dm-3", dev.join("disk/by-id/dm-uuid-mpath-36001"))
            .unwrap();
        let by_id = dev.join("disk/by-id/dm-uuid-mpath-36001");
        assert_eq!(pv_paths(&net, by_id.to_str().unwrap()), two_of_four);
    }

    #[test]
    fn parses_pvs() {
        let json = br#"  {
      "report": [
          {
              "pv": [
                  {"pv_name":"/dev/mapper/mpatha", "vg_name":"shared"},
                  {"pv_name":"/dev/sda3", "vg_name":"ubuntu-vg"},
                  {"pv_name":"/dev/mapper/mpathb", "vg_name":"shared"},
                  {"pv_name":"/dev/sdx", "vg_name":""}
              ]
          }
      ]
  }
"#;
        assert_eq!(
            parse_pvs(json, "shared").unwrap(),
            ["/dev/mapper/mpatha", "/dev/mapper/mpathb"]
        );
        assert!(parse_pvs(json, "nope").unwrap().is_empty());
        assert!(parse_pvs(b"oops", "shared").is_err());
    }

    #[tokio::test]
    async fn pvs_runs_with_a_time_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let (bin, args) = fake_command(
            tmp.path(),
            "pvs",
            r#"echo '{"report":[{"pv":[{"pv_name":"/dev/sdb","vg_name":"vg1"}]}]}'"#,
        );
        assert_eq!(pvs(&bin, "vg1").await.unwrap(), ["/dev/sdb"]);
        assert_eq!(
            std::fs::read_to_string(args).unwrap(),
            "--reportformat\njson\n-o\npv_name,vg_name\n"
        );
        let (bad, _) = fake_command(tmp.path(), "pvs-bad", "echo 'lvmlockd down' >&2\nexit 5");
        assert_eq!(pvs(&bad, "vg1").await.unwrap_err(), "pvs: lvmlockd down");
    }

    #[test]
    fn rados_arguments_and_results() {
        assert_eq!(
            rados_args("rbd", None, None, None),
            [
                "--id",
                "admin",
                "--pool",
                "rbd",
                "--client_mount_timeout",
                "3",
                "--rados_osd_op_timeout",
                "3",
                "stat",
                "orbivisor-health-probe"
            ]
        );
        assert_eq!(
            rados_args("vms", Some("ns1"), Some("orb"), Some("/etc/ceph/c2.conf"))[..8],
            [
                "--id",
                "orb",
                "--conf",
                "/etc/ceph/c2.conf",
                "--pool",
                "vms",
                "--namespace",
                "ns1"
            ]
        );
        let exit = |code: i32| ExitStatus::from_raw(code << 8);
        assert_eq!(rados_result(exit(0), b""), Ok(()));
        let enoent = b"error stat-ing vms/orbivisor-health-probe: (2) No such file or directory\n";
        assert_eq!(rados_result(exit(2), enoent), Ok(()));
        // `stat`이 아닌 곳의 ENOENT(풀을 열지 못함, keyring이 없음)는 실패다.
        assert_eq!(
            rados_result(
                exit(2),
                b"error opening pool vms: (2) No such file or directory\n"
            ),
            Err("rados: error opening pool vms: (2) No such file or directory".into())
        );
        let keyring = b"2026-11-14T10:00:00.123+0000 7f3a9c1b2640 -1 auth: unable to find a keyring on /etc/ceph/ceph.client.orb.keyring: (2) No such file or directory\n\
2026-11-14T10:00:00.124+0000 7f3a9c1b2640 -1 monclient: keyring not found\n\
failed to fetch mon config (--no-mon-config to skip)\n";
        assert_eq!(
            rados_result(exit(1), keyring),
            Err("rados: failed to fetch mon config (--no-mon-config to skip)".into())
        );
        // 마지막 줄의 시각·스레드 id는 뗀다(회차마다 같은 문구).
        assert_eq!(
            rados_result(
                exit(1),
                b"2026-11-14T10:00:05.001+0000 7f3a9c1b2640 -1 monclient(hunting): authenticate timed out after 3\n"
            ),
            Err("rados: -1 monclient(hunting): authenticate timed out after 3".into())
        );
        assert_eq!(
            rados_result(
                exit(1),
                b"2019-01-01 10:00:05.001 7f3a9c1b2640 -1 monclient: timed out\n\n"
            ),
            Err("rados: -1 monclient: timed out".into())
        );
        assert_eq!(
            rados_result(exit(110), b""),
            Err("rados: exit status 110".into())
        );
    }

    #[tokio::test]
    async fn rados_probe_runs_the_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let (bin, args) = fake_command(
            tmp.path(),
            "rados",
            "echo 'error stat-ing p/orbivisor-health-probe: (2) No such file or directory' >&2\nexit 2",
        );
        let probe = rados_args("p", None, None, None);
        rados(&bin, &probe).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(args)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            probe
        );
        let (bad, _) = fake_command(tmp.path(), "rados-bad", "echo 'timed out' >&2\nexit 1");
        assert_eq!(
            rados(&bad, &probe).await.unwrap_err(),
            ProbeError::Failed("rados: timed out".into())
        );
        assert_eq!(
            rados(&tmp.path().join("none/rados"), &probe)
                .await
                .unwrap_err(),
            ProbeError::Unavailable("rados not found: install ceph-common".into())
        );
    }
}
