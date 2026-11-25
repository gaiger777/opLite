//! 내장 VIP(설계 §4)의 호스트 쪽: 인터페이스에 주소를 붙이고 떼고, gratuitous ARP로 알린다. iproute2(`ip`)와
//! iputils `arping`을 부르므로 Linux에서 root(또는 CAP_NET_ADMIN)로 돌아야 한다.

use std::fmt;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::DriverError;
use crate::net::{netns_args, run};

/// VIP 하나: IPv4 주소와 인터페이스에 붙일 접두사 길이(`192.168.0.200/24`). 접두사가 없으면 `/32`다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct VipAddress {
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

impl FromStr for VipAddress {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || format!("{s:?} is not an IPv4 address with an optional prefix (a.b.c.d/nn)");
        let (ip, prefix) = match s.split_once('/') {
            Some((ip, prefix)) => (ip, Some(prefix)),
            None => (s, None),
        };
        let ip: Ipv4Addr = ip.parse().map_err(|_| err())?;
        let prefix = match prefix {
            None => 32,
            Some(p) if !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) && p.len() <= 2 => {
                p.parse().map_err(|_| err())?
            }
            Some(_) => return Err(err()),
        };
        if !(1..=32).contains(&prefix) {
            return Err(err());
        }
        if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_broadcast() {
            return Err(format!("{s:?} cannot be a VIP"));
        }
        Ok(Self { ip, prefix })
    }
}

impl TryFrom<String> for VipAddress {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl fmt::Display for VipAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.ip, self.prefix)
    }
}

/// VIP를 붙이고 떼는 호스트 조작. 시험에서 바꿔 끼울 수 있게 트레이트로 둔다.
#[async_trait]
pub trait VipNet: Send + Sync {
    /// `interface`에 VIP를 붙인다. 이미 붙어 있으면 아무것도 하지 않는다. 새로 붙였으면 true다.
    async fn add(&self, interface: &str, vip: VipAddress) -> Result<bool, DriverError>;
    /// `interface`에서 VIP를 뗀다. 붙어 있지 않으면 아무것도 하지 않는다.
    async fn remove(&self, interface: &str, vip: VipAddress) -> Result<(), DriverError>;
    /// 이웃의 ARP 캐시가 이 노드를 가리키도록 gratuitous ARP를 보낸다.
    async fn announce(&self, interface: &str, vip: VipAddress) -> Result<(), DriverError>;
}

/// iproute2와 `arping`으로 VIP를 다룬다.
#[derive(Debug, Default)]
pub struct IpVip {
    /// 있으면 이 네트워크 이름공간에서 부른다(시험용).
    netns: Option<String>,
}

impl IpVip {
    pub fn in_netns(name: impl Into<String>) -> Self {
        Self {
            netns: Some(name.into()),
        }
    }
}

/// `ip addr add|del <vip> dev <interface>`의 인자
pub fn addr_args(op: &str, interface: &str, vip: VipAddress) -> Vec<String> {
    ["addr", op, &vip.to_string(), "dev", interface]
        .map(String::from)
        .to_vec()
}

/// `arping -U -c 3 -I <interface> <ip>`의 인자(iputils, 응답을 기다리지 않는 알림 세 번 — 하나를 잃어도
/// 이웃이 바뀐 주인을 안다)
pub fn announce_args(interface: &str, vip: VipAddress) -> Vec<String> {
    ["-U", "-c", "3", "-I", interface, &vip.ip.to_string()]
        .map(String::from)
        .to_vec()
}

/// 붙인 VIP 기록 파일의 이름(`data_dir` 아래)
pub const VIP_RECORD_FILE: &str = "vips";

/// 이 노드가 붙인 VIP 기록(`{data_dir}/vips`, 줄마다 `<인터페이스> <주소/접두 길이>`). 붙이기 전에 적고 뗀
/// 뒤 지운다. 프로세스가 죽어(SIGKILL 등) 떼지 못한 VIP는 다음 시작이나 `orbivisord --release-vips`(systemd
/// `ExecStopPost`)가 [`release_recorded`]로 지금 설정과 관계없이 기록한 그대로(접두 길이까지) 뗀다.
#[derive(Debug, Clone)]
pub struct VipRecord {
    path: PathBuf,
}

/// 기록 파일의 내용을 읽는다(읽지 못하는 줄은 건너뛴다).
pub fn parse_record(text: &str) -> Vec<(String, VipAddress)> {
    text.lines()
        .filter_map(|line| {
            let (iface, vip) = line.trim().split_once(' ')?;
            Some((iface.to_string(), vip.trim().parse().ok()?))
        })
        .collect()
}

/// 기록 파일의 내용
pub fn format_record(entries: &[(String, VipAddress)]) -> String {
    entries
        .iter()
        .map(|(iface, vip)| format!("{iface} {vip}\n"))
        .collect()
}

impl VipRecord {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `data_dir` 아래의 기록
    pub fn in_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join(VIP_RECORD_FILE))
    }

    /// 기록한 VIP. 파일이 없으면 비어 있다.
    pub fn load(&self) -> std::io::Result<Vec<(String, VipAddress)>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(parse_record(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// 새 파일에 쓰고 바꿔 끼운다(쓰다 죽어도 옛 기록이나 새 기록 중 하나가 남는다).
    fn save(&self, entries: &[(String, VipAddress)]) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, format_record(entries).as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }

    /// VIP를 붙이기 전에 적는다.
    pub fn add(&self, interface: &str, vip: VipAddress) -> std::io::Result<()> {
        let mut entries = self.load()?;
        let entry = (interface.to_string(), vip);
        if !entries.contains(&entry) {
            entries.push(entry);
            self.save(&entries)?;
        }
        Ok(())
    }

    /// VIP를 뗀 뒤 지운다.
    pub fn remove(&self, interface: &str, vip: VipAddress) -> std::io::Result<()> {
        let mut entries = self.load()?;
        let before = entries.len();
        entries.retain(|(i, v)| !(i == interface && *v == vip));
        if entries.len() != before {
            self.save(&entries)?;
        }
        Ok(())
    }
}

/// 기록한 VIP를 모두 떼고 뗀 것을 기록에서 지운다. 떼지 못한 것은 남긴다(다음에 다시 뗀다). 인터페이스가
/// 없어진 것은 붙어 있을 수 없으므로 경고하고 기록에서 뺀다. 뗀 개수를 돌려준다.
pub async fn release_recorded(record: &VipRecord, net: &dyn VipNet) -> Result<usize, DriverError> {
    let mut released = 0;
    let mut first_error = None;
    for (iface, vip) in record.load()? {
        match net.remove(&iface, vip).await {
            Ok(()) => {
                record.remove(&iface, vip)?;
                released += 1;
            }
            Err(DriverError::ToolFailed { stderr, .. }) if no_device(&stderr) => {
                tracing::warn!(interface = %iface, vip = %vip, "the interface is gone; forgetting the recorded VIP");
                record.remove(&iface, vip)?;
            }
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(released),
    }
}

/// `ip addr add`가 이미 붙은 주소라서 실패했다.
fn already_added(stderr: &str) -> bool {
    stderr.contains("File exists") || stderr.contains("already assigned")
}

/// `ip addr`이 인터페이스가 없어서 실패했다.
fn no_device(stderr: &str) -> bool {
    stderr.contains("Cannot find device") || stderr.contains("Device does not exist")
}

/// `ip addr del`이 붙지 않은 주소라서 실패했다.
fn not_added(stderr: &str) -> bool {
    stderr.contains("Cannot assign requested address") || stderr.contains("Address not found")
}

#[async_trait]
impl VipNet for IpVip {
    async fn add(&self, interface: &str, vip: VipAddress) -> Result<bool, DriverError> {
        let args = netns_args(self.netns.as_deref(), &addr_args("add", interface, vip));
        match run("ip", &args).await {
            Err(DriverError::ToolFailed { stderr, .. }) if already_added(&stderr) => Ok(false),
            other => other.map(|_| true),
        }
    }

    async fn remove(&self, interface: &str, vip: VipAddress) -> Result<(), DriverError> {
        let args = netns_args(self.netns.as_deref(), &addr_args("del", interface, vip));
        match run("ip", &args).await {
            Err(DriverError::ToolFailed { stderr, .. }) if not_added(&stderr) => Ok(()),
            other => other.map(|_| ()),
        }
    }

    async fn announce(&self, interface: &str, vip: VipAddress) -> Result<(), DriverError> {
        let args = announce_args(interface, vip);
        match &self.netns {
            Some(ns) => {
                let mut all = ["netns", "exec", ns, "arping"].map(String::from).to_vec();
                all.extend(args);
                run("ip", &all).await
            }
            None => run("arping", &args).await,
        }
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vip(s: &str) -> VipAddress {
        s.parse().unwrap()
    }

    #[test]
    fn parses_vip_addresses() {
        assert_eq!(
            vip("192.168.0.200/24"),
            VipAddress {
                ip: Ipv4Addr::new(192, 168, 0, 200),
                prefix: 24
            }
        );
        assert_eq!(vip("10.0.0.5").prefix, 32);
        assert_eq!(vip("10.0.0.5/32").to_string(), "10.0.0.5/32");
        for bad in [
            "",
            "eth0",
            "fd00::1/64",
            "10.0.0.5/0",
            "10.0.0.5/33",
            "10.0.0.5/",
            "10.0.0.5/+8",
            "10.0.0.5/024",
            "0.0.0.0/24",
            "127.0.0.1/8",
            "224.0.0.1/24",
            "255.255.255.255/32",
        ] {
            assert!(bad.parse::<VipAddress>().is_err(), "{bad}");
        }
        let toml: VipAddress = serde_json::from_value(serde_json::json!("10.0.0.5/24")).unwrap();
        assert_eq!(toml, vip("10.0.0.5/24"));
        assert!(serde_json::from_value::<VipAddress>(serde_json::json!("x")).is_err());
    }

    #[test]
    fn builds_ip_and_arping_arguments() {
        let v = vip("192.168.0.200/24");
        assert_eq!(
            addr_args("add", "eth0", v),
            ["addr", "add", "192.168.0.200/24", "dev", "eth0"]
        );
        assert_eq!(
            netns_args(Some("ns"), &addr_args("del", "eth0", v)),
            ["-n", "ns", "addr", "del", "192.168.0.200/24", "dev", "eth0"]
        );
        assert_eq!(
            announce_args("eth0", v),
            ["-U", "-c", "3", "-I", "eth0", "192.168.0.200"]
        );
    }

    #[test]
    fn the_record_keeps_exact_prefixes() {
        let entries = vec![
            ("eth0".to_string(), vip("192.168.0.200/24")),
            ("vmbr0".to_string(), vip("10.0.0.5/32")),
        ];
        let text = format_record(&entries);
        assert_eq!(text, "eth0 192.168.0.200/24\nvmbr0 10.0.0.5/32\n");
        assert_eq!(parse_record(&text), entries);
        // 읽지 못하는 줄은 건너뛴다.
        assert_eq!(
            parse_record("garbage\neth0 x\n\neth0 192.168.0.200/24\n"),
            entries[..1]
        );

        let dir = tempfile::tempdir().unwrap();
        let record = VipRecord::in_dir(dir.path());
        assert!(record.load().unwrap().is_empty());
        record.add("eth0", vip("192.168.0.200/24")).unwrap();
        record.add("eth0", vip("192.168.0.200/24")).unwrap();
        record.add("vmbr0", vip("10.0.0.5/32")).unwrap();
        assert_eq!(record.load().unwrap(), entries);
        // 접두 길이가 다르면 다른 항목이다.
        record.remove("eth0", vip("192.168.0.200/25")).unwrap();
        assert_eq!(record.load().unwrap(), entries);
        record.remove("eth0", vip("192.168.0.200/24")).unwrap();
        assert_eq!(record.load().unwrap(), entries[1..]);
    }

    /// 떼는 것을 남기는 가짜. `fail`인 인터페이스는 떼지 못하고, `gone`으로 시작하는 인터페이스는 없다.
    struct Removals(std::sync::Mutex<Vec<String>>, &'static str);

    #[async_trait]
    impl VipNet for Removals {
        async fn add(&self, _: &str, _: VipAddress) -> Result<bool, DriverError> {
            unreachable!()
        }

        async fn remove(&self, iface: &str, vip: VipAddress) -> Result<(), DriverError> {
            if iface == self.1 {
                return Err(DriverError::ToolFailed {
                    tool: "ip",
                    stderr: "RTNETLINK answers: Operation not permitted".into(),
                });
            }
            if iface.starts_with("gone") {
                return Err(DriverError::ToolFailed {
                    tool: "ip",
                    stderr: "Cannot find device \"gone0\"".into(),
                });
            }
            self.0.lock().unwrap().push(format!("{iface} {vip}"));
            Ok(())
        }

        async fn announce(&self, _: &str, _: VipAddress) -> Result<(), DriverError> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn recorded_vips_are_released_as_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let record = VipRecord::in_dir(dir.path());
        record.add("eth0", vip("192.168.0.200/24")).unwrap();
        record.add("eth1", vip("10.0.0.5/32")).unwrap();
        record.add("gone0", vip("10.0.0.6/32")).unwrap();
        let net = Removals(Default::default(), "eth1");
        assert!(release_recorded(&record, &net).await.is_err());
        assert_eq!(*net.0.lock().unwrap(), ["eth0 192.168.0.200/24"]);
        // 떼지 못한 것만 남는다. 없어진 인터페이스의 것은 뺀다.
        assert_eq!(
            record.load().unwrap(),
            [("eth1".into(), vip("10.0.0.5/32"))]
        );
        let net = Removals(Default::default(), "none");
        assert_eq!(release_recorded(&record, &net).await.unwrap(), 1);
        assert!(record.load().unwrap().is_empty());
        assert_eq!(release_recorded(&record, &net).await.unwrap(), 0);
    }

    #[test]
    fn already_added_and_not_added_errors_are_ignored() {
        // iproute2 판에 따라 메시지가 다르다.
        assert!(already_added("RTNETLINK answers: File exists"));
        assert!(already_added("Error: ipv4: Address already assigned."));
        assert!(not_added(
            "RTNETLINK answers: Cannot assign requested address"
        ));
        assert!(not_added("Error: ipv4: Address not found."));
        assert!(!already_added("Cannot find device \"eth9\""));
        assert!(!not_added("Cannot find device \"eth9\""));
        assert!(no_device("Cannot find device \"eth9\""));
        assert!(!no_device("RTNETLINK answers: Operation not permitted"));
    }
}
