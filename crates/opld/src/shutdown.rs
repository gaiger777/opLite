//! 노드 정상 종료 정책(설계 §8). 종료 신호를 받았을 때 호스트가 꺼지는 중(재부팅·전원 끄기)인지 systemd의
//! 작업 목록으로 가리고(Proxmox `pve-ha-lrm`과 같은 방식), 클러스터 설정 `shutdown_policy`로 할 일을 정한다.
//! `systemctl stop`·업그레이드 재시작은 호스트 종료가 아니므로 지금처럼 VM을 그대로 둔다.

use std::time::Duration;

use opl_types::node::ShutdownAction;
use opl_types::settings::ShutdownPolicy;
use serde::Deserialize;

/// 호스트가 꺼지는 중인지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostShutdown {
    /// 호스트 종료가 아니다(`systemctl stop`, 업그레이드 재시작, systemd 없음).
    None,
    Reboot,
    Poweroff,
}

/// `systemctl list-jobs`를 기다리는 시간
const LIST_JOBS_TIMEOUT: Duration = Duration::from_secs(5);

/// `systemctl list-jobs --no-legend --plain`의 출력(`JOB UNIT TYPE STATE` 줄)에서 호스트 종료를 찾는다.
/// `reboot.target`·`kexec.target`·`soft-reboot.target`의 start 작업이면 재부팅, `poweroff.target`·
/// `halt.target`이면 끄기, `shutdown.target`만 있으면 끄기다.
pub fn parse_list_jobs(out: &str) -> HostShutdown {
    let starting: Vec<&str> = out
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            (fields.len() >= 3 && fields[2] == "start").then(|| fields[1])
        })
        .collect();
    let has = |units: &[&str]| starting.iter().any(|u| units.contains(u));
    if has(&["reboot.target", "kexec.target", "soft-reboot.target"]) {
        HostShutdown::Reboot
    } else if has(&["poweroff.target", "halt.target", "shutdown.target"]) {
        HostShutdown::Poweroff
    } else {
        HostShutdown::None
    }
}

/// systemd의 작업 목록으로 호스트 종료를 가린다. 명령이 없거나(macOS) 실패하면 호스트 종료가 아니다.
pub async fn detect() -> HostShutdown {
    let run = tokio::process::Command::new("systemctl")
        .args(["list-jobs", "--no-legend", "--plain"])
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(LIST_JOBS_TIMEOUT, run).await {
        Ok(Ok(out)) if out.status.success() => {
            parse_list_jobs(&String::from_utf8_lossy(&out.stdout))
        }
        Ok(Ok(out)) => {
            tracing::info!(status = %out.status, "systemctl list-jobs failed; not a host shutdown");
            HostShutdown::None
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "cannot run systemctl; not a host shutdown");
            HostShutdown::None
        }
        Err(_) => {
            tracing::warn!("systemctl list-jobs timed out; not a host shutdown");
            HostShutdown::None
        }
    }
}

/// 정책 `policy`로 호스트 종료 `host` 때 할 일. 호스트 종료가 아니면 없다. `conditional`은 재부팅이면
/// `freeze`, 끄기면 `failover`다.
pub fn action(policy: ShutdownPolicy, host: HostShutdown) -> Option<ShutdownAction> {
    match (policy, host) {
        (_, HostShutdown::None) => None,
        (ShutdownPolicy::Conditional, HostShutdown::Reboot) => Some(ShutdownAction::Freeze),
        (ShutdownPolicy::Conditional, HostShutdown::Poweroff) => Some(ShutdownAction::Failover),
        (ShutdownPolicy::Freeze, _) => Some(ShutdownAction::Freeze),
        (ShutdownPolicy::Failover, _) => Some(ShutdownAction::Failover),
        (ShutdownPolicy::Migrate, _) => Some(ShutdownAction::Migrate),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_jobs_tells_reboot_from_poweroff() {
        let job = |unit: &str, kind: &str| format!("123 {unit} {kind} waiting\n");
        for (out, want) in [
            (job("reboot.target", "start"), HostShutdown::Reboot),
            (job("kexec.target", "start"), HostShutdown::Reboot),
            // 사용자 공간만 다시 띄우는 재부팅(systemd 254+)도 노드가 곧 돌아온다.
            (job("soft-reboot.target", "start"), HostShutdown::Reboot),
            (job("poweroff.target", "start"), HostShutdown::Poweroff),
            (job("halt.target", "start"), HostShutdown::Poweroff),
            (job("shutdown.target", "start"), HostShutdown::Poweroff),
            // 재부팅은 shutdown.target도 함께 시작한다.
            (
                format!(
                    "{}{}",
                    job("shutdown.target", "start"),
                    job("reboot.target", "start")
                ),
                HostShutdown::Reboot,
            ),
            (String::new(), HostShutdown::None),
            (job("opld.service", "stop"), HostShutdown::None),
            (job("apt-daily.service", "start"), HostShutdown::None),
            // start가 아닌 작업은 호스트 종료가 아니다.
            (job("reboot.target", "stop"), HostShutdown::None),
            ("garbage\n".into(), HostShutdown::None),
        ] {
            assert_eq!(parse_list_jobs(&out), want, "{out:?}");
        }
        // systemd가 붙이는 열이 더 있어도 된다.
        assert_eq!(
            parse_list_jobs("  7  reboot.target   start  running  extra\n"),
            HostShutdown::Reboot
        );
    }

    #[test]
    fn action_follows_the_policy_only_on_a_host_shutdown() {
        use HostShutdown::{None as NoShutdown, Poweroff, Reboot};
        use ShutdownAction::{Failover, Freeze, Migrate};
        let table = [
            (
                ShutdownPolicy::Conditional,
                [Some(Freeze), Some(Failover), None],
            ),
            (ShutdownPolicy::Freeze, [Some(Freeze), Some(Freeze), None]),
            (
                ShutdownPolicy::Failover,
                [Some(Failover), Some(Failover), None],
            ),
            (
                ShutdownPolicy::Migrate,
                [Some(Migrate), Some(Migrate), None],
            ),
        ];
        for (policy, want) in table {
            let got = [Reboot, Poweroff, NoShutdown].map(|host| action(policy, host));
            assert_eq!(got, want, "{policy:?}");
        }
    }

    #[test]
    fn host_shutdown_reads_lowercase() {
        #[derive(Deserialize)]
        struct T {
            h: HostShutdown,
        }
        let t: T = toml::from_str("h = \"reboot\"").unwrap();
        assert_eq!(t.h, HostShutdown::Reboot);
        let t: T = toml::from_str("h = \"poweroff\"").unwrap();
        assert_eq!(t.h, HostShutdown::Poweroff);
        assert!(toml::from_str::<T>("h = \"halt\"").is_err());
    }
}
