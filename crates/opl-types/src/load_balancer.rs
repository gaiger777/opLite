//! 부하 분산기(계획 67 §7): OVN 내장 `Load_Balancer`로 VIP 하나를 VM NIC(또는 주소) 여럿에 L4(TCP·UDP)로 나눈다.
//! 네트워크 폴더에 산다. VIP 네트워크는 라우터 인터페이스인 오버레이 SDN 네트워크이고, 바깥 VIP는 그 라우터 바깥(공급자
//! 네트워크)의 유동 IP 범위에서 받는다. 원하는 상태만 Raft에 있고 리더 `OvnSync`가 NB의 `Load_Balancer`(프로토콜마다 행
//! 하나)·`Load_Balancer_Health_Check`로 옮겨 쓴다. 멤버 건강은 Raft에 쓰지 않고 보기에서 SB `Service_Monitor`를 읽는다.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::MacAddr;

pub type LoadBalancer = Resource<LoadBalancerSpec, LoadBalancerStatus>;

/// 리스너 수 상한
pub const MAX_LISTENERS: usize = 16;
/// 리스너 하나의 멤버 수 상한
pub const MAX_MEMBERS: usize = 64;
/// 설명의 길이 상한(글자)
pub const MAX_DESCRIPTION: usize = 200;
/// `affinity_secs`의 상한(OVN `options:affinity_timeout`)
pub const MAX_AFFINITY_SECS: u32 = 65535;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct LoadBalancerSpec {
    /// 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub folder: Option<Uuid>,
    /// 200자 이하
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub description: Option<String>,
    /// VIP 네트워크(IPv4 서브넷이 있고 라우터 인터페이스인 오버레이 SDN 네트워크) 이름
    pub network: String,
    /// VIP(IPv4). API가 그 서브넷 풀에서 채운다(직접 주면 풀 안).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ipv4", nullable = false, default)
    )]
    pub vip: Option<Ipv4Addr>,
    /// 바깥 VIP(VIP 네트워크 라우터의 바깥 공급자 네트워크). 없으면 안쪽에서만 닿는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub external: Option<LbExternal>,
    /// 리스너(1~16개, 같은 프로토콜·포트는 한 번)
    pub listeners: Vec<LbListener>,
    /// 같은 클라이언트를 이 시간(초, 1~65535) 동안 같은 멤버로 보낸다. 없으면 연결마다 해시다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub affinity_secs: Option<u32>,
    /// 건강 검사 출발 주소(건강 검사가 있는 NIC 멤버의 네트워크마다 하나). API가 그 서브넷 풀에서 채운다(사용자가 주지
    /// 않는다).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub monitors: Vec<LbMonitor>,
}

impl ResourceSpec for LoadBalancerSpec {
    const KIND: Kind = Kind::LoadBalancer;
    type Status = LoadBalancerStatus;
}

/// 지금은 칸이 없다(멤버 건강·VIP 상태는 보기가 SB·NB에서 읽는다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct LoadBalancerStatus {}

/// 바깥 VIP
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct LbExternal {
    /// 공급자 네트워크 이름(VIP 네트워크 라우터의 바깥)
    pub network: String,
    /// 바깥 VIP(IPv4). API가 그 네트워크의 유동 IP 풀(없으면 풀)에서 채운다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ipv4", nullable = false, default)
    )]
    pub ip: Option<Ipv4Addr>,
}

/// 리스너 하나: 프로토콜·포트와 멤버
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct LbListener {
    pub protocol: LbProtocol,
    /// VIP 포트(1~65535)
    pub port: u16,
    /// 멤버(0~64개). 없으면 연결을 거절한다(TCP RST·ICMP).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub members: Vec<LbMember>,
    /// 건강 검사. 없으면 멤버를 늘 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub health: Option<LbHealth>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum LbProtocol {
    Tcp,
    Udp,
}

impl LbProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            LbProtocol::Tcp => "tcp",
            LbProtocol::Udp => "udp",
        }
    }
}

impl std::fmt::Display for LbProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 멤버 하나: VM NIC(`vm`·`mac` — 주소는 그 NIC의 IPv4) 또는 주소(`ip` — 건강 검사 없음)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct LbMember {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vm: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, nullable = false, default))]
    pub mac: Option<MacAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ipv4", nullable = false, default)
    )]
    pub ip: Option<Ipv4Addr>,
    /// 멤버 포트(1~65535)
    pub port: u16,
}

impl LbMember {
    /// NIC 멤버의 `(vm, mac)`
    pub fn nic(&self) -> Option<(Uuid, MacAddr)> {
        self.vm.zip(self.mac)
    }
}

/// 건강 검사(OVN 서비스 모니터 — TCP SYN·UDP)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct LbHealth {
    /// 검사 간격(초, 1~300)
    #[serde(default = "default_interval")]
    #[cfg_attr(feature = "openapi", schema(default = 5))]
    pub interval_secs: u16,
    /// 답을 기다리는 시간(초, 1~간격)
    #[serde(default = "default_timeout")]
    #[cfg_attr(feature = "openapi", schema(default = 3))]
    pub timeout_secs: u16,
    /// 이만큼 이어 성공하면 다시 넣는다(1~10).
    #[serde(default = "default_rise")]
    #[cfg_attr(feature = "openapi", schema(default = 2))]
    pub rise: u8,
    /// 이만큼 이어 실패하면 뺀다(1~10).
    #[serde(default = "default_fall")]
    #[cfg_attr(feature = "openapi", schema(default = 3))]
    pub fall: u8,
}

fn default_interval() -> u16 {
    5
}
fn default_timeout() -> u16 {
    3
}
fn default_rise() -> u8 {
    2
}
fn default_fall() -> u8 {
    3
}

impl Default for LbHealth {
    fn default() -> Self {
        LbHealth {
            interval_secs: default_interval(),
            timeout_secs: default_timeout(),
            rise: default_rise(),
            fall: default_fall(),
        }
    }
}

/// 건강 검사 출발 주소 하나(그 네트워크 서브넷 안 — Raft `ip`로 다른 NIC·라우터·유동 IP와 겹치지 않음)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct LbMonitor {
    /// 멤버 네트워크 이름
    pub network: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ipv4"))]
    pub ip: Ipv4Addr,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LoadBalancerError {
    #[error("description must be at most {MAX_DESCRIPTION} characters")]
    LongDescription,
    #[error("give 1-{MAX_LISTENERS} listeners, not {0}")]
    Listeners(usize),
    #[error("listener {0}/{1} is given twice")]
    DuplicateListener(LbProtocol, u16),
    #[error("port 0 is not a valid port (listener {0}/{1})")]
    ZeroPort(LbProtocol, u16),
    #[error("listener {0}/{1} can have at most {MAX_MEMBERS} members")]
    TooManyMembers(LbProtocol, u16),
    #[error("listener {0}/{1}: a member is either a VM NIC (vm and mac) or an address (ip)")]
    MemberShape(LbProtocol, u16),
    #[error("listener {0}/{1}: a member is given twice")]
    DuplicateMember(LbProtocol, u16),
    #[error(
        "listener {0}/{1}: health check needs interval_secs 1-300, timeout_secs 1-interval_secs, rise and fall 1-10"
    )]
    Health(LbProtocol, u16),
    #[error("affinity_secs {0} is out of range: use 1-{MAX_AFFINITY_SECS}")]
    Affinity(u32),
    #[error("monitor network {0:?} is given twice")]
    DuplicateMonitor(String),
}

impl LoadBalancerSpec {
    /// 스펙만으로 보는 검사. 네트워크를 알아야 하는 검사(VIP 네트워크가 라우터 인터페이스인 오버레이, 바깥이 그
    /// 라우터의 바깥, 멤버가 닿는지, 주소가 서브넷 안)는 API가 한다.
    pub fn validate(&self) -> Result<(), LoadBalancerError> {
        if self
            .description
            .as_ref()
            .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION)
        {
            return Err(LoadBalancerError::LongDescription);
        }
        if self.listeners.is_empty() || self.listeners.len() > MAX_LISTENERS {
            return Err(LoadBalancerError::Listeners(self.listeners.len()));
        }
        for (i, l) in self.listeners.iter().enumerate() {
            let (p, port) = (l.protocol, l.port);
            if self.listeners[..i]
                .iter()
                .any(|o| o.protocol == p && o.port == port)
            {
                return Err(LoadBalancerError::DuplicateListener(p, port));
            }
            if port == 0 || l.members.iter().any(|m| m.port == 0) {
                return Err(LoadBalancerError::ZeroPort(p, port));
            }
            if l.members.len() > MAX_MEMBERS {
                return Err(LoadBalancerError::TooManyMembers(p, port));
            }
            for (j, m) in l.members.iter().enumerate() {
                let nic = m.vm.is_some() && m.mac.is_some() && m.ip.is_none();
                let addr = m.vm.is_none() && m.mac.is_none() && m.ip.is_some();
                if !nic && !addr {
                    return Err(LoadBalancerError::MemberShape(p, port));
                }
                if l.members[..j]
                    .iter()
                    .any(|o| o.nic() == m.nic() && o.ip == m.ip && o.port == m.port)
                {
                    return Err(LoadBalancerError::DuplicateMember(p, port));
                }
            }
            if let Some(h) = l.health
                && !((1..=300).contains(&h.interval_secs)
                    && (1..=h.interval_secs).contains(&h.timeout_secs)
                    && (1..=10).contains(&h.rise)
                    && (1..=10).contains(&h.fall))
            {
                return Err(LoadBalancerError::Health(p, port));
            }
        }
        if let Some(a) = self.affinity_secs
            && !(1..=MAX_AFFINITY_SECS).contains(&a)
        {
            return Err(LoadBalancerError::Affinity(a));
        }
        for (i, m) in self.monitors.iter().enumerate() {
            if self.monitors[..i].iter().any(|o| o.network == m.network) {
                return Err(LoadBalancerError::DuplicateMonitor(m.network.clone()));
            }
        }
        Ok(())
    }

    /// 쓰는 프로토콜(차례대로, 겹치지 않게) — NB `Load_Balancer` 행 하나씩
    pub fn protocols(&self) -> Vec<LbProtocol> {
        let mut out: Vec<LbProtocol> = self.listeners.iter().map(|l| l.protocol).collect();
        out.sort();
        out.dedup();
        out
    }

    /// 안 VIP와(있으면) 바깥 VIP
    fn vip_addrs(&self) -> Vec<Ipv4Addr> {
        self.vip
            .into_iter()
            .chain(self.external.as_ref().and_then(|e| e.ip))
            .collect()
    }

    /// 멤버 네트워크 `network`의 건강 검사 출발 주소
    pub fn monitor_for(&self, network: &str) -> Option<Ipv4Addr> {
        self.monitors
            .iter()
            .find(|m| m.network == network)
            .map(|m| m.ip)
    }
}

/// NB `Load_Balancer` 행 이름 `opl-lb-<LB id>-<프로토콜>`
pub fn nb_name(id: Uuid, protocol: LbProtocol) -> String {
    format!("opl-lb-{id}-{protocol}")
}

/// NIC 멤버의 지금 모습(OvnSync가 VM 스펙에서 찾는다): 그 NIC의 IPv4와 네트워크 이름
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberNic {
    pub ip: Ipv4Addr,
    pub network: String,
}

/// 멤버의 백엔드 주소. NIC 멤버는 `resolve`가 찾지 못하면(NIC를 뺌, VM을 지움, IPv4 없음) 빠진다.
fn backend(
    m: &LbMember,
    resolve: &impl Fn(Uuid, MacAddr) -> Option<MemberNic>,
) -> Option<Ipv4Addr> {
    match m.nic() {
        Some((vm, mac)) => resolve(vm, mac).map(|n| n.ip),
        None => m.ip,
    }
}

/// 프로토콜 `protocol` 행의 `vips`: 안 VIP와 바깥 VIP의 그 프로토콜 리스너마다 `"VIP:포트"` → `"B:멤버포트,…"`(멤버
/// 차례, 같은 백엔드는 한 번). 찾지 못한 NIC 멤버는 빠지고, 남은 멤버가 없으면 빈 값이다(`options:reject`로 거절).
/// VIP를 아직 고르지 않았으면 그 줄이 없다.
pub fn vips(
    spec: &LoadBalancerSpec,
    protocol: LbProtocol,
    resolve: impl Fn(Uuid, MacAddr) -> Option<MemberNic>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for l in spec.listeners.iter().filter(|l| l.protocol == protocol) {
        let mut backends: Vec<String> = Vec::new();
        for m in &l.members {
            if let Some(ip) = backend(m, &resolve) {
                let b = format!("{ip}:{}", m.port);
                if !backends.contains(&b) {
                    backends.push(b);
                }
            }
        }
        let backends = backends.join(",");
        for vip in spec.vip_addrs() {
            out.insert(format!("{vip}:{}", l.port), backends.clone());
        }
    }
    out
}

/// 프로토콜 `protocol` 행의 `Load_Balancer_Health_Check`: 건강 검사가 있는 리스너마다 안 VIP와 바깥 VIP에 하나씩
/// `("VIP:포트", options)`(northd가 `vip` 글을 `vips`의 열쇠와 그대로 맞춘다).
pub fn health_rows(
    spec: &LoadBalancerSpec,
    protocol: LbProtocol,
) -> Vec<(String, BTreeMap<String, String>)> {
    let mut out = Vec::new();
    for l in spec.listeners.iter().filter(|l| l.protocol == protocol) {
        let Some(h) = l.health else { continue };
        let options: BTreeMap<String, String> = [
            ("interval", h.interval_secs.to_string()),
            ("timeout", h.timeout_secs.to_string()),
            ("success_count", h.rise.to_string()),
            ("failure_count", h.fall.to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        for vip in spec.vip_addrs() {
            out.push((format!("{vip}:{}", l.port), options.clone()));
        }
    }
    out.sort();
    out
}

/// 프로토콜 `protocol` 행의 `ip_port_mappings`: 건강 검사가 있는 리스너의 NIC 멤버마다 `"멤버 IPv4"` →
/// `"<NIC 논리 포트>:<그 네트워크의 검사 출발 주소>"`. 찾지 못한 NIC와 검사 주소가 없는 네트워크의 멤버는 빠진다(northd는
/// 매핑이 없는 백엔드를 검사하지 않고 늘 쓴다). 주소 멤버는 검사하지 않는다.
pub fn ip_port_mappings(
    spec: &LoadBalancerSpec,
    protocol: LbProtocol,
    resolve: impl Fn(Uuid, MacAddr) -> Option<MemberNic>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for l in spec
        .listeners
        .iter()
        .filter(|l| l.protocol == protocol && l.health.is_some())
    {
        for (vm, mac) in l.members.iter().filter_map(LbMember::nic) {
            let Some(nic) = resolve(vm, mac) else {
                continue;
            };
            let Some(src) = spec.monitor_for(&nic.network) else {
                continue;
            };
            out.insert(
                nic.ip.to_string(),
                format!("{}:{src}", crate::sdn::port_name(vm, mac)),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(v: serde_json::Value) -> LoadBalancerSpec {
        serde_json::from_value(v).unwrap()
    }

    const VM1: &str = "00000000-0000-0000-0000-000000000001";
    const VM2: &str = "00000000-0000-0000-0000-000000000002";

    fn sample() -> LoadBalancerSpec {
        spec(serde_json::json!({
            "network": "t1",
            "vip": "10.10.0.50",
            "external": { "network": "ext", "ip": "203.0.113.130" },
            "listeners": [
                {
                    "protocol": "tcp",
                    "port": 80,
                    "members": [
                        { "vm": VM1, "mac": "52:54:00:00:00:01", "port": 8080 },
                        { "vm": VM2, "mac": "52:54:00:00:00:02", "port": 8080 },
                        { "ip": "10.20.0.9", "port": 80 }
                    ],
                    "health": {}
                },
                { "protocol": "udp", "port": 53, "members": [{ "ip": "10.10.0.7", "port": 53 }] },
                { "protocol": "tcp", "port": 443 }
            ],
            "affinity_secs": 600,
            "monitors": [{ "network": "t1", "ip": "10.10.0.60" }, { "network": "t2", "ip": "10.20.0.60" }]
        }))
    }

    /// VM1은 t1, VM2는 t2에 NIC가 있다.
    fn resolve(vm: Uuid, _mac: MacAddr) -> Option<MemberNic> {
        match vm.to_string().as_str() {
            VM1 => Some(MemberNic {
                ip: "10.10.0.11".parse().unwrap(),
                network: "t1".into(),
            }),
            VM2 => Some(MemberNic {
                ip: "10.20.0.12".parse().unwrap(),
                network: "t2".into(),
            }),
            _ => None,
        }
    }

    #[test]
    fn storage_format_round_trips_and_validates() {
        let s = sample();
        assert_eq!(s.validate(), Ok(()));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            serde_json::from_value::<LoadBalancerSpec>(json.clone()).unwrap(),
            s
        );
        // 건강 검사 기본값은 쓸 때 칸으로 나온다.
        assert_eq!(
            json["listeners"][0]["health"],
            serde_json::json!({ "interval_secs": 5, "timeout_secs": 3, "rise": 2, "fall": 3 })
        );
        let bare = spec(
            serde_json::json!({ "network": "t1", "listeners": [{ "protocol": "tcp", "port": 80 }] }),
        );
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"network":"t1","listeners":[{"protocol":"tcp","port":80}]}"#
        );
        assert!(
            serde_json::from_value::<LoadBalancerSpec>(
                serde_json::json!({ "network": "t1", "listeners": [], "l7": true })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<LoadBalancerSpec>(
                serde_json::json!({ "network": "t1", "listeners": [{ "protocol": "sctp", "port": 1 }] })
            )
            .is_err()
        );
        assert_eq!(LoadBalancerSpec::KIND.as_str(), "load-balancer");
        assert_eq!(s.protocols(), vec![LbProtocol::Tcp, LbProtocol::Udp]);
    }

    #[test]
    fn validates() {
        let listener = |extra: serde_json::Value| {
            let mut l = serde_json::json!({ "protocol": "tcp", "port": 80 });
            l.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            l
        };
        let with = |listeners: Vec<serde_json::Value>, extra: serde_json::Value| {
            let mut v = serde_json::json!({ "network": "t1", "listeners": listeners });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            spec(v).validate()
        };
        let none = serde_json::json!({});
        let tcp80 = LbProtocol::Tcp;
        assert_eq!(
            with(vec![], none.clone()),
            Err(LoadBalancerError::Listeners(0))
        );
        let many = (1..=17)
            .map(|p| serde_json::json!({ "protocol": "tcp", "port": p }))
            .collect();
        assert_eq!(
            with(many, none.clone()),
            Err(LoadBalancerError::Listeners(17))
        );
        assert_eq!(
            with(
                vec![listener(none.clone()), listener(none.clone())],
                none.clone()
            ),
            Err(LoadBalancerError::DuplicateListener(tcp80, 80))
        );
        // 같은 포트라도 프로토콜이 다르면 된다.
        assert_eq!(
            with(
                vec![
                    listener(none.clone()),
                    serde_json::json!({ "protocol": "udp", "port": 80 })
                ],
                none.clone()
            ),
            Ok(())
        );
        assert_eq!(
            with(
                vec![serde_json::json!({ "protocol": "tcp", "port": 0 })],
                none.clone()
            ),
            Err(LoadBalancerError::ZeroPort(tcp80, 0))
        );
        assert_eq!(
            with(
                vec![listener(
                    serde_json::json!({ "members": [{ "ip": "10.0.0.1", "port": 0 }] })
                )],
                none.clone()
            ),
            Err(LoadBalancerError::ZeroPort(tcp80, 80))
        );
        let members: Vec<_> = (0..65)
            .map(|i| serde_json::json!({ "ip": format!("10.0.0.{}", i + 1), "port": 80 }))
            .collect();
        assert_eq!(
            with(
                vec![listener(serde_json::json!({ "members": members }))],
                none.clone()
            ),
            Err(LoadBalancerError::TooManyMembers(tcp80, 80))
        );
        for bad in [
            serde_json::json!({ "port": 80 }),
            serde_json::json!({ "vm": VM1, "port": 80 }),
            serde_json::json!({ "mac": "52:54:00:00:00:01", "port": 80 }),
            serde_json::json!({ "vm": VM1, "mac": "52:54:00:00:00:01", "ip": "10.0.0.1", "port": 80 }),
        ] {
            assert_eq!(
                with(
                    vec![listener(serde_json::json!({ "members": [bad.clone()] }))],
                    none.clone()
                ),
                Err(LoadBalancerError::MemberShape(tcp80, 80)),
                "{bad}"
            );
        }
        let m = serde_json::json!({ "vm": VM1, "mac": "52:54:00:00:00:01", "port": 80 });
        assert_eq!(
            with(
                vec![listener(serde_json::json!({ "members": [m.clone(), m] }))],
                none.clone()
            ),
            Err(LoadBalancerError::DuplicateMember(tcp80, 80))
        );
        for bad in [
            serde_json::json!({ "interval_secs": 0 }),
            serde_json::json!({ "interval_secs": 301 }),
            serde_json::json!({ "interval_secs": 2, "timeout_secs": 3 }),
            serde_json::json!({ "timeout_secs": 0 }),
            serde_json::json!({ "rise": 0 }),
            serde_json::json!({ "fall": 11 }),
        ] {
            assert_eq!(
                with(
                    vec![listener(serde_json::json!({ "health": bad.clone() }))],
                    none.clone()
                ),
                Err(LoadBalancerError::Health(tcp80, 80)),
                "{bad}"
            );
        }
        assert_eq!(
            with(
                vec![listener(none.clone())],
                serde_json::json!({ "affinity_secs": 0 })
            ),
            Err(LoadBalancerError::Affinity(0))
        );
        assert_eq!(
            with(
                vec![listener(none.clone())],
                serde_json::json!({ "affinity_secs": 65536 })
            ),
            Err(LoadBalancerError::Affinity(65536))
        );
        assert_eq!(
            with(
                vec![listener(none.clone())],
                serde_json::json!({ "monitors": [
                    { "network": "t1", "ip": "10.0.0.2" }, { "network": "t1", "ip": "10.0.0.3" }
                ] })
            ),
            Err(LoadBalancerError::DuplicateMonitor("t1".into()))
        );
        assert_eq!(
            with(
                vec![listener(none)],
                serde_json::json!({ "description": "x".repeat(201) })
            ),
            Err(LoadBalancerError::LongDescription)
        );
    }

    #[test]
    fn vips_cover_both_vips_per_protocol() {
        let s = sample();
        let tcp = vips(&s, LbProtocol::Tcp, resolve);
        assert_eq!(
            tcp,
            [
                (
                    "10.10.0.50:80",
                    "10.10.0.11:8080,10.20.0.12:8080,10.20.0.9:80"
                ),
                ("10.10.0.50:443", ""),
                (
                    "203.0.113.130:80",
                    "10.10.0.11:8080,10.20.0.12:8080,10.20.0.9:80"
                ),
                ("203.0.113.130:443", ""),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
        );
        let udp = vips(&s, LbProtocol::Udp, resolve);
        assert_eq!(udp.len(), 2);
        assert_eq!(udp["10.10.0.50:53"], "10.10.0.7:53");
        // 찾지 못한 NIC 멤버는 빠진다(스펙은 그대로).
        let gone = vips(&s, LbProtocol::Tcp, |vm, mac| {
            (vm.to_string() != VM2).then(|| resolve(vm, mac)).flatten()
        });
        assert_eq!(gone["10.10.0.50:80"], "10.10.0.11:8080,10.20.0.9:80");
        // VIP를 아직 고르지 않았으면 줄이 없다. 바깥 VIP가 없으면 안 VIP만.
        let mut no_vip = s.clone();
        no_vip.vip = None;
        no_vip.external = None;
        assert!(vips(&no_vip, LbProtocol::Tcp, resolve).is_empty());
        let mut inner = s.clone();
        inner.external = None;
        assert_eq!(vips(&inner, LbProtocol::Tcp, resolve).len(), 2);
        // 포트를 바꾸면 열쇠가 바뀐다.
        let mut moved = s;
        moved.listeners[0].port = 8000;
        assert!(vips(&moved, LbProtocol::Tcp, resolve).contains_key("10.10.0.50:8000"));
        assert_eq!(
            nb_name(Uuid::from_u128(5), LbProtocol::Udp),
            "opl-lb-00000000-0000-0000-0000-000000000005-udp"
        );
    }

    #[test]
    fn health_rows_and_mappings() {
        let s = sample();
        let rows = health_rows(&s, LbProtocol::Tcp);
        assert_eq!(
            rows.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(),
            ["10.10.0.50:80", "203.0.113.130:80"]
        );
        assert_eq!(rows[0].1["interval"], "5");
        assert_eq!(rows[0].1["timeout"], "3");
        assert_eq!(rows[0].1["success_count"], "2");
        assert_eq!(rows[0].1["failure_count"], "3");
        assert!(health_rows(&s, LbProtocol::Udp).is_empty());
        let maps = ip_port_mappings(&s, LbProtocol::Tcp, resolve);
        let port1 =
            crate::sdn::port_name(VM1.parse().unwrap(), "52:54:00:00:00:01".parse().unwrap());
        let port2 =
            crate::sdn::port_name(VM2.parse().unwrap(), "52:54:00:00:00:02".parse().unwrap());
        assert_eq!(
            maps,
            [
                ("10.10.0.11".to_string(), format!("{port1}:10.10.0.60")),
                ("10.20.0.12".to_string(), format!("{port2}:10.20.0.60")),
            ]
            .into_iter()
            .collect()
        );
        // 검사 주소가 없는 네트워크의 멤버와 검사 없는 리스너는 매핑이 없다.
        let mut one = s.clone();
        one.monitors.retain(|m| m.network == "t1");
        assert_eq!(ip_port_mappings(&one, LbProtocol::Tcp, resolve).len(), 1);
        let mut unchecked = s;
        unchecked.listeners[0].health = None;
        assert!(ip_port_mappings(&unchecked, LbProtocol::Tcp, resolve).is_empty());
    }
}
