use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_api::SnapshotSchedulerConfig;
use opl_qemu::{Accel, HostProfile, kvm_available};
use serde::Deserialize;

/// `opld --config <path>`로 읽는 TOML 설정.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 노드 이름. 클러스터 안에서 유일해야 한다.
    pub name: String,
    /// 다른 노드와 클라이언트가 이 노드에 닿는 호스트(IP나 DNS 이름). 노드 인증서에 들어가고,
    /// 클러스터에 알리는 주소(`address:포트`)가 된다. 나중에 바꿀 수 없다.
    pub address: String,
    /// API(HTTPS)를 받을 소켓 주소
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// 노드 간 통신(gRPC over mTLS)을 받을 소켓 주소
    #[serde(default = "default_cluster_listen")]
    pub cluster_listen: SocketAddr,
    /// `oplctl cluster init|join|leave`를 받는 유닉스 소켓. 기본은 `{run_dir}/opld.sock`이다.
    /// opld를 실행한 사용자만 쓸 수 있다(0600).
    pub control_socket: Option<PathBuf>,
    /// 클러스터 저장소, 노드 인증서, VM별 영구 파일, 올린 이미지를 둔다.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// QEMU 소켓과 pidfile을 두는 휘발성 디렉터리. 유닉스 소켓 경로 길이 제한(104바이트) 때문에 짧아야 한다.
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
    /// Homebrew QEMU의 펌웨어 디렉터리(macOS). Linux는 Debian 경로를 쓴다(`[hypervisor]`로 바꾼다).
    #[serde(default = "default_qemu_share_dir")]
    pub qemu_share_dir: PathBuf,
    #[serde(default = "default_qemu_img")]
    pub qemu_img: PathBuf,
    #[serde(default)]
    pub hypervisor: HypervisorSection,
    #[serde(default)]
    pub reconciler: ReconcilerSection,
    #[serde(default)]
    pub schedule: ScheduleSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HypervisorSection {
    /// `auto`(macOS는 hvf, Linux는 KVM이 있으면 kvm, 없으면 tcg), `kvm`, `tcg`, `hvf`
    #[serde(default)]
    pub accel: AccelSetting,
    /// QEMU 실행 파일. 기본은 호스트에 맞는 `qemu-system-*`이다.
    pub qemu_binary: Option<PathBuf>,
    /// UEFI 펌웨어 코드와 VM마다 복사하는 변수 저장소 원본
    pub uefi_code: Option<PathBuf>,
    pub uefi_vars: Option<PathBuf>,
    /// QEMU를 `systemd-run --scope`로 감싼다(VM마다 cgroup). Linux에서 기본으로 켠다.
    #[serde(default = "default_systemd_scope")]
    pub systemd_scope: bool,
}

impl Default for HypervisorSection {
    fn default() -> Self {
        Self {
            accel: AccelSetting::default(),
            qemu_binary: None,
            uefi_code: None,
            uefi_vars: None,
            systemd_scope: default_systemd_scope(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccelSetting {
    #[default]
    Auto,
    Kvm,
    Tcg,
    Hvf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcilerSection {
    /// ACPI 종료를 요청한 뒤 강제 종료하기까지 기다리는 시간(초)
    #[serde(default = "default_acpi_timeout")]
    pub acpi_timeout_secs: u64,
}

impl Default for ReconcilerSection {
    fn default() -> Self {
        Self {
            acpi_timeout_secs: default_acpi_timeout(),
        }
    }
}

impl ReconcilerSection {
    pub fn acpi_timeout(&self) -> Duration {
        Duration::from_secs(self.acpi_timeout_secs)
    }
}

/// 예약 스냅샷 작업. 테스트만 줄인다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSection {
    /// 예약을 확인하는 간격(초)
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// 주기의 한 시간(초). 매일은 24배, 매주는 168배다.
    #[serde(default = "default_hour_secs")]
    pub hour_secs: u64,
}

impl Default for ScheduleSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            hour_secs: default_hour_secs(),
        }
    }
}

impl ScheduleSection {
    pub fn scheduler_config(&self) -> SnapshotSchedulerConfig {
        SnapshotSchedulerConfig {
            tick: Duration::from_secs(self.tick_secs),
            hour: Duration::from_secs(self.hour_secs),
        }
    }
}

fn default_listen() -> SocketAddr {
    "0.0.0.0:8006".parse().unwrap()
}
fn default_cluster_listen() -> SocketAddr {
    "0.0.0.0:8007".parse().unwrap()
}
fn default_data_dir() -> PathBuf {
    "/var/lib/opl".into()
}
fn default_run_dir() -> PathBuf {
    "/run/opl".into()
}
fn default_qemu_share_dir() -> PathBuf {
    "/opt/homebrew/share/qemu".into()
}
fn default_systemd_scope() -> bool {
    cfg!(target_os = "linux")
}
fn default_qemu_img() -> PathBuf {
    "qemu-img".into()
}
fn default_acpi_timeout() -> u64 {
    60
}
fn default_tick_secs() -> u64 {
    60
}
fn default_hour_secs() -> u64 {
    3600
}

/// `host:port`. IPv6 주소는 대괄호로 감싼다.
fn with_port(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        opl_types::resource::validate_name(&self.name)?;
        let address = &self.address;
        let ok = !address.is_empty()
            && (address.parse::<IpAddr>().is_ok()
                || address
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.'));
        if !ok {
            anyhow::bail!(
                "address must be an IP address or a host name without a port (got {address:?})"
            );
        }
        // 다른 노드가 닿을 주소이므로 0.0.0.0이나 ::은 안 된다.
        if address
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified())
        {
            anyhow::bail!("address must not be an unspecified address (got {address:?})");
        }
        for (key, addr) in [
            ("listen", self.listen),
            ("cluster_listen", self.cluster_listen),
        ] {
            if addr.port() == 0 {
                anyhow::bail!("{key} must have a port other than 0 (got {addr})");
            }
        }
        let linux = cfg!(target_os = "linux");
        match self.hypervisor.accel {
            AccelSetting::Kvm if !linux => anyhow::bail!("accel = \"kvm\" needs Linux"),
            AccelSetting::Hvf if linux => anyhow::bail!("accel = \"hvf\" needs macOS"),
            _ => {}
        }
        if self.hypervisor.systemd_scope && !linux {
            anyhow::bail!("systemd_scope needs Linux; set it to false");
        }
        if self.schedule.tick_secs == 0 || self.schedule.hour_secs == 0 {
            anyhow::bail!("schedule.tick_secs and schedule.hour_secs must be at least 1");
        }
        // 알리는 주소는 `address`에 포트만 붙이므로 포트가 같으면 둘을 구별할 수 없다.
        if self.listen.port() == self.cluster_listen.port() {
            anyhow::bail!(
                "listen and cluster_listen must use different ports (got {} for both)",
                self.listen.port()
            );
        }
        Ok(())
    }

    /// 이 호스트에서 QEMU를 실행하는 방법. `accel = "kvm"`인데 `/dev/kvm`을 열 수 없으면 오류다.
    pub fn host_profile(&self) -> anyhow::Result<HostProfile> {
        let h = &self.hypervisor;
        let mut profile = HostProfile::native(&self.qemu_share_dir);
        match h.accel {
            AccelSetting::Auto => {}
            AccelSetting::Kvm if !kvm_available() => {
                anyhow::bail!("accel = \"kvm\" but /dev/kvm cannot be opened")
            }
            AccelSetting::Kvm => profile.accel = Accel::Kvm,
            AccelSetting::Tcg => profile.accel = Accel::Tcg,
            AccelSetting::Hvf => profile.accel = Accel::Hvf,
        }
        if let Some(binary) = &h.qemu_binary {
            profile.qemu_binary = binary.clone();
        }
        if let Some(code) = &h.uefi_code {
            profile.uefi_code = code.clone();
        }
        if let Some(vars) = &h.uefi_vars {
            profile.uefi_vars_template = vars.clone();
        }
        Ok(profile)
    }

    /// 클러스터에 알리는 API 주소
    pub fn api_address(&self) -> String {
        with_port(&self.address, self.listen.port())
    }

    /// 클러스터에 알리는 노드 간 통신 주소(Raft 멤버십의 노드 주소)
    pub fn cluster_address(&self) -> String {
        with_port(&self.address, self.cluster_listen.port())
    }

    /// 노드 인증서의 이름: 알리는 호스트와 같은 기계에서 쓰는 이름
    pub fn cert_names(&self) -> Vec<String> {
        let mut names = vec![self.address.clone()];
        for local in ["localhost", "127.0.0.1", "::1"] {
            if self.address != local {
                names.push(local.into());
            }
        }
        names
    }

    pub fn control_socket(&self) -> PathBuf {
        self.control_socket
            .clone()
            .unwrap_or_else(|| self.run_dir.join("opld.sock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn fills_defaults() {
        let c = parse("name = \"node1\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.listen, "0.0.0.0:8006".parse().unwrap());
        assert_eq!(c.api_address(), "10.0.0.5:8006");
        assert_eq!(c.cluster_address(), "10.0.0.5:8007");
        assert_eq!(c.data_dir, Path::new("/var/lib/opl"));
        assert_eq!(c.control_socket(), Path::new("/run/opl/opld.sock"));
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(60));
        let schedule = c.schedule.scheduler_config();
        assert_eq!(schedule.tick, Duration::from_secs(60));
        assert_eq!(schedule.hour, Duration::from_secs(3600));
        assert_eq!(c.hypervisor.accel, AccelSetting::Auto);
        assert_eq!(c.hypervisor.systemd_scope, cfg!(target_os = "linux"));
        assert_eq!(
            c.cert_names(),
            ["10.0.0.5", "localhost", "127.0.0.1", "::1"]
        );
    }

    #[test]
    fn rejects_unknown_keys_and_missing_address() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\nlisten_addr = \"x\"").is_err()
        );
        assert!(toml::from_str::<Config>("name = \"n\"").is_err());
        let c = parse("name = \"n\"\naddress = \"h\"\n[reconciler]\nacpi_timeout_secs = 5");
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn validates_name_and_address() {
        let with = |name: &str, address: &str| Config {
            name: name.into(),
            address: address.into(),
            ..parse("name = \"n\"\naddress = \"h\"")
        };
        assert!(with("node1", "10.0.0.5").validate().is_ok());
        assert!(with("node1", "node1.lan").validate().is_ok());
        assert!(with("node1", "::1").validate().is_ok());
        assert!(with("Node_1", "10.0.0.5").validate().is_err());
        for bad in ["", "10.0.0.5:8006", "a b", "[::1]"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
        assert_eq!(with("n", "::1").api_address(), "[::1]:8006");
        for bad in ["0.0.0.0", "::"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_ports() {
        let ok = parse("name = \"n\"\naddress = \"h\"");
        assert!(ok.validate().is_ok());
        let same = Config {
            cluster_listen: "127.0.0.1:8006".parse().unwrap(),
            ..ok.clone()
        };
        assert!(same.validate().is_err());
        let zero = Config {
            listen: "0.0.0.0:0".parse().unwrap(),
            ..ok.clone()
        };
        assert!(zero.validate().is_err());
        let zero = Config {
            cluster_listen: "0.0.0.0:0".parse().unwrap(),
            ..ok
        };
        assert!(zero.validate().is_err());
    }

    #[test]
    fn schedule_section_shortens_periods_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[schedule]\ntick_secs = 1\nhour_secs = 2");
        assert!(c.validate().is_ok());
        let schedule = c.schedule.scheduler_config();
        assert_eq!(schedule.tick, Duration::from_secs(1));
        assert_eq!(schedule.hour, Duration::from_secs(2));
        for bad in ["tick_secs = 0", "hour_secs = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[schedule]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[schedule]\ntick = 1")
                .is_err()
        );
    }

    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\n\
             qemu_binary = \"/opt/qemu/bin/qemu-system-x86_64\"\nsystemd_scope = false",
        );
        assert!(c.validate().is_ok());
        let p = c.host_profile().unwrap();
        assert_eq!(p.accel, Accel::Tcg);
        assert_eq!(p.qemu_binary, Path::new("/opt/qemu/bin/qemu-system-x86_64"));

        // 다른 OS의 가속기와 macOS의 systemd 스코프는 거부한다.
        let other = if cfg!(target_os = "linux") {
            "hvf"
        } else {
            "kvm"
        };
        let c = parse(&format!(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"{other}\"\nsystemd_scope = false"
        ));
        assert!(c.validate().is_err());
        if !cfg!(target_os = "linux") {
            let c = parse("name = \"n\"\naddress = \"h\"\n[hypervisor]\nsystemd_scope = true");
            assert!(c.validate().is_err());
        }
        assert!(
            toml::from_str::<Config>(
                "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"xen\""
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unknown_keys_in_hypervisor_section() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[hypervisor]\nacel = \"kvm\"")
                .is_err()
        );
    }

    #[test]
    fn explicit_kvm_needs_dev_kvm() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"kvm\"");
        // `/dev/kvm`이 있는 호스트(Linux CI)에서는 KVM을 쓰고, 없으면 오류다.
        match c.host_profile() {
            Ok(p) => {
                assert!(kvm_available());
                assert_eq!(p.accel, Accel::Kvm);
            }
            Err(e) => {
                assert!(!kvm_available());
                assert!(e.to_string().contains("/dev/kvm"), "{e}");
            }
        }
    }
}
