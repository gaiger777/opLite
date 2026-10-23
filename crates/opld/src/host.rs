//! 호스트 용량과 가상화 구성. 스케줄러가 노드를 고를 때 쓴다.

use std::sync::Arc;

use opl_driver::{HostNet, IpRoute, QemuConfig};
use opl_qemu::{Accel, HostProfile};
use opl_types::node::BridgeInfo;

use crate::config::Config;

/// 이 노드가 VM을 실행하는 방법
#[derive(Debug, Clone)]
pub struct Host {
    pub profile: HostProfile,
    /// 브리지 NIC의 tap을 다룬다. Linux에서만 있다.
    pub net: Option<Arc<dyn HostNet>>,
    pub systemd_scope: bool,
}

impl Host {
    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        let profile = config.host_profile()?;
        if cfg!(target_os = "linux") && profile.accel == Accel::Tcg {
            tracing::warn!("KVM is not available; VMs run with TCG emulation (slow)");
        }
        let net: Option<Arc<dyn HostNet>> = if cfg!(target_os = "linux") {
            Some(Arc::new(IpRoute))
        } else {
            None
        };
        Ok(Self {
            profile,
            net,
            systemd_scope: config.hypervisor.systemd_scope,
        })
    }

    pub fn qemu_config(&self, config: &Config) -> QemuConfig {
        QemuConfig {
            host: self.profile.clone(),
            run_dir: config.run_dir.clone(),
            state_dir: config.data_dir.clone(),
            net: self.net.clone(),
            systemd_scope: self.systemd_scope,
        }
    }

    /// `NodeStatus.hypervisor`: `qemu-kvm`, `qemu-tcg`, `qemu-hvf`
    pub fn hypervisor_name(&self) -> String {
        format!("qemu-{}", self.profile.accel.as_str())
    }

    /// 이 노드의 Linux 브리지. 알 수 없으면 비어 있다.
    pub async fn bridges(&self) -> Vec<BridgeInfo> {
        let Some(net) = &self.net else {
            return vec![];
        };
        match net.bridges().await {
            Ok(bridges) => bridges,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list bridges");
                vec![]
            }
        }
    }
}

/// 물리 메모리(MiB). 알 수 없으면 0이다.
pub fn memory_mib() -> u64 {
    bytes().map_or(0, |b| b >> 20)
}

#[cfg(target_os = "linux")]
fn bytes() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(target_os = "macos")]
fn bytes() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_host_memory() {
        assert!(super::memory_mib() > 0);
    }

    #[test]
    fn names_the_hypervisor_by_accelerator() {
        let config: crate::config::Config = toml::from_str(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap();
        let host = super::Host::from_config(&config).unwrap();
        assert_eq!(host.hypervisor_name(), "qemu-tcg");
        assert_eq!(host.net.is_some(), cfg!(target_os = "linux"));
    }
}
