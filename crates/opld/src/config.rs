use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// `opld --config <path>`로 읽는 TOML 설정.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 노드 이름. 클러스터 안에서 유일해야 한다.
    pub name: String,
    /// API를 받을 소켓 주소(`127.0.0.1:8006`, `[::1]:8006`). TLS는 클러스터 부트스트랩(계획 6)에서
    /// 더하므로 그때까지는 루프백 주소만 받는다. 호스트 이름은 받지 않는다.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// 클러스터 저장소, VM별 영구 파일, 올린 이미지를 둔다.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// QEMU 소켓과 pidfile을 두는 휘발성 디렉터리. 유닉스 소켓 경로 길이 제한(104바이트) 때문에 짧아야 한다.
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
    /// Homebrew QEMU의 펌웨어 디렉터리. 지금은 Apple Silicon(hvf)만 지원한다(Linux/KVM은 M7).
    #[serde(default = "default_qemu_share_dir")]
    pub qemu_share_dir: PathBuf,
    #[serde(default = "default_qemu_img")]
    pub qemu_img: PathBuf,
    #[serde(default)]
    pub reconciler: ReconcilerSection,
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

fn default_listen() -> String {
    "127.0.0.1:8006".into()
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
fn default_qemu_img() -> PathBuf {
    "qemu-img".into()
}
fn default_acpi_timeout() -> u64 {
    60
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
        let listen = &self.listen;
        match listen.parse::<SocketAddr>() {
            Ok(addr) if addr.ip().is_loopback() => Ok(()),
            _ => anyhow::bail!(
                "listen must be a loopback address until TLS is supported (got {listen})"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_defaults() {
        let c: Config = toml::from_str(r#"name = "node1""#).unwrap();
        assert_eq!(c.listen, "127.0.0.1:8006");
        assert_eq!(c.data_dir, Path::new("/var/lib/opl"));
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(60));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("name = \"n\"\nlisten_addr = \"x\"").is_err());
        let c: Config =
            toml::from_str("name = \"n\"\n[reconciler]\nacpi_timeout_secs = 5").unwrap();
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn listen_must_be_loopback() {
        let with = |listen: &str| Config {
            listen: listen.into(),
            ..toml::from_str(r#"name = "n""#).unwrap()
        };
        assert!(with("127.0.0.1:8006").validate().is_ok());
        assert!(with("[::1]:8006").validate().is_ok());
        for bad in ["0.0.0.0:8006", "10.0.0.1:8006", "localhost:8006"] {
            assert!(with(bad).validate().is_err(), "{bad}");
        }
    }
}
