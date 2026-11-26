//! `orbivisord backup-serve` 헬퍼 띄우기·지켜보기·멈추기(계획 68 설계 §11.1). 헬퍼는 저장소 백업 디스크를 읽기 전용
//! NBD로 내보낸다(보통 복원의 `qemu-img convert`, 라이브 복원의 QEMU backing). 설정(자격 증명·키)은 표준 입력의
//! JSON으로만 준다. Linux에서 systemd가 있으면 `systemd-run --scope --unit orbivisor-bkserve-<id>`로 띄워 opld가 다시
//! 떠도 산다(swtpm과 같은 꼴). 아니면 opld의 자식이다.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use orbivisor_store::nbd::NbdClient;
use orbivisor_store::nbd::serve::ServeConfig;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::error::DriverError;

/// 헬퍼 하위 명령
pub const BACKUP_SERVE_COMMAND: &str = "backup-serve";
/// 소켓이 답하기를 기다리는 한도(저장소가 S3면 매니페스트·인덱스를 받는 데 다시 하기 60초가 들 수 있다)
pub const BACKUP_SERVE_START_TIMEOUT: Duration = Duration::from_secs(90);
/// SIGTERM 뒤 끝나기를 기다리는 한도
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// 띄운 헬퍼
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeHelper {
    pub pid: u32,
    pub socket: PathBuf,
}

/// 헬퍼의 systemd scope 이름(`orbivisor-bkserve-<id>.scope`)
pub fn backup_serve_unit(id: Uuid) -> String {
    format!("orbivisor-bkserve-{id}")
}

/// 이름 바꾸기(계획 74) 앞의 데몬이 쓰던 scope 이름. 같은 세션의 남은 scope를 치울 때 함께 본다.
fn legacy_backup_serve_unit(id: Uuid) -> String {
    format!("{LEGACY_BACKUP_SERVE_PREFIX}{id}")
}

/// 이름 바꾸기 앞의 scope 이름 앞
const LEGACY_BACKUP_SERVE_PREFIX: &str = "opl-bkserve-";

/// `exe`(`orbivisord`)의 헬퍼를 `cfg`로 띄우고 소켓이 답하기를 기다린다. `cfg.pid_file`은 있어야 한다(헬퍼가 pid를
/// 적는다 — 다시 뜬 opld가 그것으로 찾는다). 표준 오류는 `log`에 덧붙인다. `scope`면 systemd scope(`id`)로 감싼다.
/// 실패하면 띄운 헬퍼를 끝내고 로그의 마지막 줄과 함께 오류다. 같은 소켓의 앞선 헬퍼가 살아 있으면 먼저 끝낸다.
pub async fn start_backup_serve(
    exe: &Path,
    scope: bool,
    id: Uuid,
    cfg: &ServeConfig,
    log: &Path,
) -> Result<ServeHelper, DriverError> {
    let pid_file = cfg
        .pid_file
        .as_deref()
        .ok_or_else(|| DriverError::InvalidSpec("backup-serve needs a pid file".into()))?;
    stop_backup_serve(pid_file, &cfg.socket).await;
    let mut cmd = if scope {
        let unit = backup_serve_unit(id);
        for stale in [&unit, &legacy_backup_serve_unit(id)] {
            crate::qemu::clear_stale_scope(&format!("{stale}.scope")).await;
        }
        let mut cmd = tokio::process::Command::new("systemd-run");
        cmd.args([
            "--scope",
            "--quiet",
            "--collect",
            &format!("--unit={unit}"),
            "--property=Before=orbivisord.service",
            "--property=After=network-online.target remote-fs.target",
            "--",
        ])
        .arg(exe);
        cmd
    } else {
        tokio::process::Command::new(exe)
    };
    if let Some(dir) = log.parent() {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    cmd.arg(BACKUP_SERVE_COMMAND)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(stderr);
    let mut child = cmd
        .spawn()
        .map_err(|e| DriverError::StartFailed(format!("backup-serve: {}: {e}", exe.display())))?;
    let config = serde_json::to_vec(cfg).expect("serve config serializes");
    if let Some(mut stdin) = child.stdin.take() {
        // 다 쓰고 닫는다(헬퍼는 끝까지 읽는다).
        stdin.write_all(&config).await?;
    }
    let deadline = tokio::time::Instant::now() + BACKUP_SERVE_START_TIMEOUT;
    loop {
        if NbdClient::list(&cfg.socket).await.is_ok()
            && let Some(pid) = read_pid(pid_file)
        {
            // 자식을 버려도 헬퍼는 계속 돈다(tokio가 끝난 자식을 거둔다).
            drop(child);
            return Ok(ServeHelper {
                pid,
                socket: cfg.socket.clone(),
            });
        }
        let exited = child.try_wait()?;
        if exited.is_some() || tokio::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            let text = std::fs::read_to_string(log).unwrap_or_default();
            let last = text.lines().last().unwrap_or("no log").trim().to_string();
            let why = match exited {
                Some(status) => format!("exited ({status})"),
                None => format!(
                    "did not answer in {}s",
                    BACKUP_SERVE_START_TIMEOUT.as_secs()
                ),
            };
            return Err(DriverError::StartFailed(format!(
                "backup-serve {why}: {last}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn read_pid(pid_file: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}

/// pid 파일의 헬퍼가 살아 있으면 그 pid(프로세스가 있고 명령줄이 `backup-serve`)
pub fn backup_serve_alive(pid_file: &Path) -> Option<u32> {
    let pid = read_pid(pid_file)?;
    test_kill_process(Pid::from_raw(i32::try_from(pid).ok()?)?).ok()?;
    crate::tpm::process_cmdline(pid)
        .is_some_and(|c| c.contains(BACKUP_SERVE_COMMAND))
        .then_some(pid)
}

/// 헬퍼를 끝낸다(SIGTERM, 끝나지 않으면 SIGKILL). 소켓과 pid 파일을 지운다. 없으면 할 일이 없다.
pub async fn stop_backup_serve(pid_file: &Path, socket: &Path) {
    if let Some(pid) = backup_serve_alive(pid_file)
        && let Some(target) = i32::try_from(pid).ok().and_then(Pid::from_raw)
    {
        let _ = kill_process(target, Signal::TERM);
        let gone = || backup_serve_alive(pid_file).is_none();
        if !crate::qemu::wait_until(STOP_TIMEOUT, gone).await {
            tracing::warn!(pid, "backup-serve did not stop; killing it");
            let _ = kill_process(target, Signal::KILL);
            crate::qemu::wait_until(STOP_TIMEOUT, gone).await;
        }
    }
    for f in [pid_file, socket] {
        if let Err(e) = std::fs::remove_file(f)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(file = %f.display(), error = %e, "cannot remove");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 새로 띄우는 scope는 새 이름이고, 치울 때 함께 보는 옛 이름은 계획 74 앞의 이름이다.
    #[test]
    fn scope_names_use_the_new_prefix_and_remember_the_old_one() {
        let id = Uuid::nil();
        assert_eq!(
            backup_serve_unit(id),
            "orbivisor-bkserve-00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            legacy_backup_serve_unit(id),
            "opl-bkserve-00000000-0000-0000-0000-000000000000"
        );
    }
}
