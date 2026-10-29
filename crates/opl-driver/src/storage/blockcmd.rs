//! 블록 스토리지 명령(LVM, ZFS)을 돌리는 도우미. 읽기만 하는 명령은 시간이 지나면 죽이고, 바꾸는
//! 명령은 죽이지 않고 뒤에서 끝나게 둔다(대상마다 하나씩).

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use uuid::Uuid;

use crate::error::DriverError;

/// 명령 도구 하나(LVM, ZFS)의 설정과, 바꾸는 명령이 돌고 있는 대상들
pub(super) struct BlockTool {
    /// 명령이 시간을 넘겼을 때 `Timeout`에 담는 말
    timeout: &'static str,
    /// 같은 대상을 바꾸는 앞 명령이 아직 돌고 있을 때 `Timeout`에 담는 말
    busy: &'static str,
    /// 명령에 더하는 환경 변수
    env: &'static [(&'static str, &'static str)],
    /// 실패한 명령의 오류(stderr 첫 줄을 담는다)
    failure: fn(String) -> DriverError,
    /// 바꾸는 명령이 돌고 있는 대상
    changing: Mutex<BTreeSet<String>>,
}

impl BlockTool {
    pub(super) const fn new(
        timeout: &'static str,
        busy: &'static str,
        env: &'static [(&'static str, &'static str)],
        failure: fn(String) -> DriverError,
    ) -> Self {
        Self {
            timeout,
            busy,
            env,
            failure,
            changing: Mutex::new(BTreeSet::new()),
        }
    }

    fn command(&self, program: &str, args: &[&str]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .envs(self.env.iter().copied())
            .stdin(Stdio::null());
        cmd
    }

    /// 실패한 명령의 오류: stderr 첫 줄(없으면 종료 상태)
    fn failure(
        &self,
        program: &str,
        status: std::process::ExitStatus,
        stderr: &[u8],
    ) -> DriverError {
        let stderr = String::from_utf8_lossy(stderr);
        let line = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map_or_else(|| status.to_string(), str::to_string);
        (self.failure)(format!("{program}: {line}"))
    }

    /// 읽기만 하는 명령을 돌려 표준 출력을 돌려준다. `timeout`이 지나면 `Timeout`이고, 시간이
    /// 지나거나 취소되면 명령을 죽인다.
    pub(super) async fn run_read(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<Vec<u8>, DriverError> {
        let mut cmd = self.command(program, args);
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let out = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| DriverError::Timeout(self.timeout))??;
        if !out.status.success() {
            return Err(self.failure(program, out.status, &out.stderr));
        }
        Ok(out.stdout)
    }

    /// 대상 `targets`를 바꾸는 명령을 돌린다. `timeout`이 지나면 `Timeout`이지만 명령은 죽이지
    /// 않는다(중간에 죽이면 메타데이터 갱신이 반쯤 끝나 남을 수 있다). 이 future가 버려져도 끝날
    /// 때까지 기다리는 작업이 프로세스를 거둔다. 그동안 같은 대상을 바꾸는 명령은 띄우지 않고 일시
    /// 오류다. 기다리는 쪽이 떠난 뒤에도 출력 때문에 막히거나 죽지 않도록(SIGPIPE) 표준 출력은
    /// 버리고 stderr는 지운 임시 파일에 받는다.
    pub(super) async fn run_change(
        &'static self,
        program: &str,
        args: &[&str],
        targets: &[&str],
        timeout: Duration,
    ) -> Result<(), DriverError> {
        let changing = Changing::begin(self, targets)?;
        let mut stderr = stderr_file()?;
        let mut child = self
            .command(program, args)
            .stdout(Stdio::null())
            .stderr(stderr.try_clone()?)
            .kill_on_drop(false)
            .spawn()?;
        let waited = tokio::spawn(async move {
            let status = child.wait().await;
            drop(changing);
            status
        });
        let status = tokio::time::timeout(timeout, waited)
            .await
            .map_err(|_| DriverError::Timeout(self.timeout))?
            .map_err(|e| DriverError::Io(std::io::Error::other(e)))??;
        if status.success() {
            return Ok(());
        }
        let mut text = Vec::new();
        stderr.seek(SeekFrom::Start(0))?;
        stderr.read_to_end(&mut text)?;
        Err(self.failure(program, status, &text))
    }

    /// 대상 `target`을 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    pub(super) fn changing(&self, target: &str) -> bool {
        self.changing.lock().unwrap().contains(target)
    }
}

/// 바꾸는 명령이 돌고 있다는 대상들의 표시. 명령이 끝나 버려지면 지운다.
struct Changing {
    tool: &'static BlockTool,
    targets: Vec<String>,
}

impl Changing {
    /// 대상 중 하나라도 앞 명령이 돌고 있으면(시간을 넘겨 뒤에서 도는 중) 일시 오류다.
    fn begin(tool: &'static BlockTool, targets: &[&str]) -> Result<Self, DriverError> {
        let mut changing = tool.changing.lock().unwrap();
        if targets.iter().any(|t| changing.contains(*t)) {
            return Err(DriverError::Timeout(tool.busy));
        }
        let targets: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
        changing.extend(targets.iter().cloned());
        Ok(Self { tool, targets })
    }
}

impl Drop for Changing {
    fn drop(&mut self) {
        let mut changing = self.tool.changing.lock().unwrap();
        for target in &self.targets {
            changing.remove(target);
        }
    }
}

/// 명령의 stderr를 받을 임시 파일. 만들자마자 지우므로 열린 동안만 있다.
fn stderr_file() -> std::io::Result<std::fs::File> {
    let path = std::env::temp_dir().join(format!("opl-blockcmd-{}.err", Uuid::now_v7()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    static TOOL: BlockTool = BlockTool::new(
        "test command",
        "an earlier test command still running on the same target",
        &[("OPL_BLOCKCMD_TEST", "set")],
        DriverError::Lvm,
    );

    /// 읽기만 하는 명령만 시간이 지나면 죽인다. 바꾸는 명령은 뒤에서 끝나게 둔다.
    #[tokio::test]
    async fn only_reads_are_killed_on_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let short = Duration::from_millis(50);
        let (read, change) = (tmp.path().join("read"), tmp.path().join("change"));
        let script = |done: &Path| format!("sleep 0.3; touch {}", done.display());
        let err = TOOL
            .run_read("/bin/sh", &["-c", &script(&read)], short)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout("test command")),
            "{err:?}"
        );
        let err = TOOL
            .run_change("/bin/sh", &["-c", &script(&change)], &[], short)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout("test command")),
            "{err:?}"
        );
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(!read.exists());
        assert!(change.exists());

        // 읽기는 표준 출력을 돌려준다. 환경 변수가 붙는다. 실패하면 둘 다 stderr 첫 줄이다.
        let long = Duration::from_secs(10);
        let out = TOOL
            .run_read("/bin/sh", &["-c", "echo $OPL_BLOCKCMD_TEST"], long)
            .await;
        assert_eq!(out.unwrap(), b"set\n");
        let fail = ["-c", "echo '  no vg' >&2; echo more >&2; exit 5"];
        for err in [
            TOOL.run_read("/bin/sh", &fail, long).await.unwrap_err(),
            TOOL.run_change("/bin/sh", &fail, &[], long)
                .await
                .unwrap_err(),
        ] {
            assert!(
                matches!(err, DriverError::Lvm(ref m) if m == "/bin/sh: no vg"),
                "{err:?}"
            );
        }
        TOOL.run_change("/bin/sh", &["-c", "exit 0"], &[], long)
            .await
            .unwrap();
    }

    /// 바꾸는 명령의 출력은 파이프가 아니다(표준 출력은 버리고 stderr는 파일). 기다리는 쪽이 떠나
    /// 파이프가 닫혀도 명령이 SIGPIPE로 죽지 않는다.
    #[tokio::test]
    async fn change_output_is_not_a_pipe() {
        let script = "if [ -p /dev/fd/1 ] || [ -p /dev/fd/2 ]; then exit 3; fi";
        TOOL.run_change("/bin/sh", &["-c", script], &[], Duration::from_secs(10))
            .await
            .unwrap();
    }

    /// 같은 대상을 바꾸는 앞 명령이 아직 돌고 있으면 새 명령을 띄우지 않고 일시 오류다. 다른 대상과
    /// 다른 도구는 상관없고, 앞 명령이 끝나면 다시 띄운다.
    #[tokio::test]
    async fn one_change_per_target_at_a_time() {
        static OTHER_TOOL: BlockTool = BlockTool::new("other", "other busy", &[], DriverError::Lvm);
        let tmp = tempfile::tempdir().unwrap();
        let (target, other) = (
            format!("vg0/{}", Uuid::now_v7()),
            format!("vg0/{}", Uuid::now_v7()),
        );
        let (first, second) = (tmp.path().join("first"), tmp.path().join("second"));
        let slow = format!("sleep 0.4; touch {}", first.display());
        let touch = format!("touch {}", second.display());
        let long = Duration::from_secs(10);

        let err = TOOL
            .run_change(
                "/bin/sh",
                &["-c", &slow],
                &[&target],
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
        assert!(TOOL.changing(&target));
        assert!(!TOOL.changing(&other) && !OTHER_TOOL.changing(&target));
        // 이름 바꾸기처럼 여러 대상을 다루는 명령도 그중 하나가 돌고 있으면 띄우지 않는다.
        for targets in [vec![target.as_str()], vec![other.as_str(), target.as_str()]] {
            let err = TOOL
                .run_change("/bin/sh", &["-c", &touch], &targets, long)
                .await
                .unwrap_err();
            assert!(
                matches!(err, DriverError::Timeout(m) if m == TOOL.busy),
                "{err:?}"
            );
            assert!(err.is_transient(), "{err:?}");
            assert!(!second.exists());
        }
        TOOL.run_change("/bin/sh", &["-c", "exit 0"], &[&other], long)
            .await
            .unwrap();
        OTHER_TOOL
            .run_change("/bin/sh", &["-c", "exit 0"], &[&target], long)
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(first.exists());
        assert!(!TOOL.changing(&target));
        TOOL.run_change("/bin/sh", &["-c", &touch], &[&target], long)
            .await
            .unwrap();
        assert!(second.exists());
    }
}
