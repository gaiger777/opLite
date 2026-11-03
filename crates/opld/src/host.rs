//! 호스트 용량과 가상화 구성. 스케줄러가 노드를 고를 때 쓴다.

use std::sync::Arc;

use opl_driver::{Firewall, HostNet, IpRoute, Nft, QemuConfig};
use opl_qemu::{Accel, HostProfile};
use opl_types::node::BridgeInfo;

use crate::config::{AccelSetting, Config};

/// 이 노드가 VM을 실행하는 방법
#[derive(Debug, Clone)]
pub struct Host {
    pub profile: HostProfile,
    /// 브리지 NIC의 tap을 다룬다. Linux에서만 있다.
    pub net: Option<Arc<dyn HostNet>>,
    /// VM 방화벽(nftables)을 넣는다. Linux에서 규칙을 넣을 수 있을 때만 있다([`Host::detect_firewall`]).
    pub firewall: Option<Arc<dyn Firewall>>,
    pub systemd_scope: bool,
}

impl Host {
    /// `tag`는 이 노드의 클러스터 태그다. 관리 이름 브리지(`oplv…`)는 이 클러스터의 것만 쓴다.
    pub fn from_config(config: &Config, tag: &str) -> anyhow::Result<Self> {
        let profile = config.host_profile()?;
        // 관리자가 `accel = "tcg"`를 고른 때는 알리지 않는다.
        if cfg!(target_os = "linux")
            && config.hypervisor.accel == AccelSetting::Auto
            && profile.accel == Accel::Tcg
        {
            tracing::warn!("KVM is not available; VMs run with TCG emulation (slow)");
        }
        let net: Option<Arc<dyn HostNet>> = if cfg!(target_os = "linux") {
            Some(Arc::new(IpRoute::default().with_cluster_tag(tag)))
        } else {
            None
        };
        Ok(Self {
            profile,
            net,
            firewall: None,
            systemd_scope: config.hypervisor.systemd_scope,
        })
    }

    /// Linux에서 클러스터 태그 `tag`의 테이블에 실제로 규칙을 넣을 수 있으면 VM 방화벽을 켠다. 안
    /// 되면(nftables가 없음, root가 아님) 보안 그룹을 건 VM이 이 노드에서 돌지 않는다고 알린다.
    pub async fn detect_firewall(&mut self, tag: &str) {
        if !cfg!(target_os = "linux") {
            return;
        }
        self.firewall = probe_firewall(Arc::new(Nft::default()), tag).await;
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

    /// 이 노드의 Linux 브리지. 목록을 얻지 못하면 `None`이다(기록된 목록을 지우지 않게).
    pub async fn bridges(&self) -> Option<Vec<BridgeInfo>> {
        let Some(net) = &self.net else {
            return Some(vec![]);
        };
        match net.bridges().await {
            Ok(bridges) => Some(bridges),
            Err(e) => {
                tracing::warn!(error = %e, "cannot list bridges; keeping the recorded ones");
                None
            }
        }
    }
}

/// `firewall`로 테이블 `bridge opl_<tag>`를 (없으면) 만드는 글을 넣어 본다. 되면 `firewall`을 돌려준다.
/// 있는 테이블과 규칙은 그대로 둔다(opld를 다시 시작해도 도는 VM을 계속 지킨다).
async fn probe_firewall(firewall: Arc<dyn Firewall>, tag: &str) -> Option<Arc<dyn Firewall>> {
    match firewall
        .apply(&format!("table bridge opl_{tag} {{}}\n"))
        .await
    {
        Ok(()) => Some(firewall),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "cannot apply nftables rules; VMs with security groups cannot run on this node"
            );
            None
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
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use opl_driver::{DriverError, Firewall};

    /// 받은 글을 기록하고 `ok`가 아니면 실패한다.
    #[derive(Debug)]
    struct FakeFirewall {
        ok: bool,
        applied: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Firewall for FakeFirewall {
        async fn apply(&self, ruleset: &str) -> Result<(), DriverError> {
            self.applied.lock().unwrap().push(ruleset.to_string());
            if self.ok {
                Ok(())
            } else {
                Err(DriverError::ToolFailed {
                    tool: "nft",
                    stderr: "Operation not permitted".into(),
                })
            }
        }
    }

    /// 방화벽은 이 클러스터의 테이블을 (지우지 않고) 만들어 보아 될 때만 켠다.
    #[tokio::test]
    async fn the_firewall_is_on_only_when_a_probe_applies() {
        for ok in [true, false] {
            let fake = Arc::new(FakeFirewall {
                ok,
                applied: Mutex::default(),
            });
            let got = super::probe_firewall(fake.clone(), "abc").await;
            assert_eq!(got.is_some(), ok);
            assert_eq!(*fake.applied.lock().unwrap(), ["table bridge opl_abc {}\n"]);
        }
    }

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
        let host = super::Host::from_config(&config, "t").unwrap();
        assert_eq!(host.hypervisor_name(), "qemu-tcg");
        assert_eq!(host.net.is_some(), cfg!(target_os = "linux"));
    }
}
