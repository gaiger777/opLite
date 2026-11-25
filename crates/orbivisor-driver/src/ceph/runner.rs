//! Ceph 구성 명령(`apt-get`, `ceph`, `ceph-mon`, `ceph-volume`, `systemctl` …)을 돌리는 실행기. 실제
//! [`SystemRunner`]와 시험의 [`FakeRunner`](부른 명령을 남기고 정해 둔 답을 돌려줌)를 바꿔 끼운다.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;

use crate::error::DriverError;

/// 따로 정하지 않은 명령의 시간 한도
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// 돌릴 명령 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    /// 더하는 환경 변수(기본 `LANG=C` 위에)
    pub env: Vec<(String, String)>,
    /// 표준 입력에 줄 내용. 없으면 빈 입력
    pub stdin: Option<Vec<u8>>,
    /// 이 시간이 지나면 명령을 죽이고 `Err`
    pub timeout: Duration,
}

impl Cmd {
    pub fn new<S: AsRef<str>>(program: &str, args: &[S]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|a| a.as_ref().to_string()).collect(),
            env: Vec::new(),
            stdin: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    pub fn stdin(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(input.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 프로그램과 인자를 빈칸으로 이은 한 줄(`ceph --connect-timeout 10 quorum_status --format json`)
    pub fn line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// 끝난 명령의 결과. 0이 아닌 종료도 결과다(부르는 쪽이 문구를 만든다).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CmdOutput {
    /// 종료 코드. 시그널로 끝났으면 -1
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CmdOutput {
    /// 종료 0과 표준 출력 `stdout`
    pub fn ok(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 0,
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }

    /// 종료 `status`와 stderr `stderr`
    pub fn fail(status: i32, stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            stdout: Vec::new(),
            stderr: stderr.into(),
        }
    }

    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// 명령 실행기. 시간 초과와 실행하지 못함만 `Err`다.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError>;
}

/// 노드에서 실제로 명령을 돌린다(`LANG=C`, 시간이 지나거나 취소되면 죽인다).
pub struct SystemRunner;

#[async_trait]
impl CommandRunner for SystemRunner {
    async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
        let mut command = tokio::process::Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .env("LANG", "C")
            .envs(cmd.env.iter().map(|(k, v)| (k, v)))
            .stdin(if cmd.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| {
            DriverError::Io(std::io::Error::new(
                e.kind(),
                format!("cannot run {}: {e}", cmd.program),
            ))
        })?;
        let input = cmd.stdin.clone();
        let stdin = child.stdin.take();
        let run = async move {
            if let (Some(mut pipe), Some(input)) = (stdin, input) {
                // 입력을 다 읽지 않고 끝나는 명령도 있다(닫힌 파이프는 오류가 아니다).
                let _ = pipe.write_all(&input).await;
            }
            child.wait_with_output().await
        };
        let out = tokio::time::timeout(cmd.timeout, run)
            .await
            .map_err(|_| DriverError::Timeout("a Ceph setup command"))??;
        Ok(CmdOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// 시험의 실행기: 부른 명령을 남기고, 명령 줄의 앞부분이 맞는 첫 규칙의 답을 돌려준다. 규칙이 없으면
/// 종료 0·빈 출력이다.
#[derive(Default)]
pub struct FakeRunner {
    calls: Mutex<Vec<Cmd>>,
    rules: Mutex<Vec<Rule>>,
}

struct Rule {
    prefix: String,
    /// 차례로 돌려줄 답. 마지막 답은 그 뒤로도 되풀이한다.
    replies: VecDeque<CmdOutput>,
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// 줄이 `prefix`로 시작하는 명령에 늘 `out`을 돌려준다. 같은 `prefix`의 규칙이 있으면 바꾼다.
    pub fn reply(&self, prefix: &str, out: CmdOutput) {
        self.reply_seq(prefix, vec![out]);
    }

    /// 줄이 `prefix`로 시작하는 명령에 `outs`를 차례로 돌려준다(다 쓰면 마지막을 되풀이). 같은
    /// `prefix`의 규칙이 있으면 바꾼다.
    pub fn reply_seq(&self, prefix: &str, outs: Vec<CmdOutput>) {
        assert!(!outs.is_empty(), "reply_seq needs at least one reply");
        let mut rules = self.rules.lock().unwrap();
        let replies = VecDeque::from(outs);
        match rules.iter_mut().find(|r| r.prefix == prefix) {
            Some(rule) => rule.replies = replies,
            None => rules.push(Rule {
                prefix: prefix.to_string(),
                replies,
            }),
        }
    }

    /// 부른 명령 줄([`Cmd::line`])들, 부른 차례로
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(Cmd::line).collect()
    }

    /// 부른 명령들(환경·입력·시간 한도까지), 부른 차례로
    pub fn commands(&self) -> Vec<Cmd> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
        self.calls.lock().unwrap().push(cmd.clone());
        let line = cmd.line();
        let mut rules = self.rules.lock().unwrap();
        let Some(rule) = rules.iter_mut().find(|r| line.starts_with(&r.prefix)) else {
            return Ok(CmdOutput::default());
        };
        let out = if rule.replies.len() > 1 {
            rule.replies.pop_front()
        } else {
            rule.replies.front().cloned()
        };
        Ok(out.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_joins_program_and_arguments() {
        let cmd = Cmd::new(
            "ceph",
            &[
                "--connect-timeout",
                "10",
                "quorum_status",
                "--format",
                "json",
            ],
        );
        assert_eq!(
            cmd.line(),
            "ceph --connect-timeout 10 quorum_status --format json"
        );
        assert_eq!(cmd.timeout, DEFAULT_TIMEOUT);
        assert_eq!(Cmd::new::<&str>("true", &[]).line(), "true");
    }

    /// 앞부분이 맞는 첫 규칙의 답, 차례 답(다 쓰면 마지막을 되풀이), 규칙이 없으면 종료 0·빈 출력. 부른
    /// 명령은 환경·입력과 함께 남는다.
    #[tokio::test]
    async fn fake_runner_answers_by_prefix_and_records_calls() {
        let fake = FakeRunner::new();
        fake.reply("ceph mgr stat", CmdOutput::ok(r#"{"available":true}"#));
        fake.reply("ceph", CmdOutput::fail(1, "no quorum"));
        fake.reply_seq(
            "systemctl is-active",
            vec![CmdOutput::fail(3, "activating"), CmdOutput::ok("active\n")],
        );

        let stat = Cmd::new("ceph", &["mgr", "stat", "--format", "json"]);
        assert_eq!(
            fake.run(&stat).await.unwrap().stdout,
            br#"{"available":true}"#
        );
        let out = fake.run(&Cmd::new("ceph", &["status"])).await.unwrap();
        assert_eq!(
            (out.status, out.stderr.as_slice()),
            (1, b"no quorum".as_slice())
        );
        assert!(!out.success());

        let active = Cmd::new("systemctl", &["is-active", "ceph-osd@0"]);
        let seq: Vec<i32> = [
            fake.run(&active).await.unwrap().status,
            fake.run(&active).await.unwrap().status,
            fake.run(&active).await.unwrap().status,
        ]
        .into();
        assert_eq!(seq, [3, 0, 0]);

        let apt = Cmd::new("apt-get", &["install", "-y", "ceph-mon"])
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdin("keyring");
        assert_eq!(fake.run(&apt).await.unwrap(), CmdOutput::default());

        // 같은 접두사로 다시 정하면 그 규칙을 바꾼다(시험 중간에 답 고치기).
        fake.reply("ceph", CmdOutput::ok("fixed"));
        let out = fake.run(&Cmd::new("ceph", &["status"])).await.unwrap();
        assert_eq!(out.stdout, b"fixed");

        assert_eq!(
            fake.calls(),
            [
                "ceph mgr stat --format json",
                "ceph status",
                "systemctl is-active ceph-osd@0",
                "systemctl is-active ceph-osd@0",
                "systemctl is-active ceph-osd@0",
                "apt-get install -y ceph-mon",
                "ceph status",
            ]
        );
        let recorded = &fake.commands()[5];
        assert_eq!(
            recorded.env,
            [("DEBIAN_FRONTEND".to_string(), "noninteractive".to_string())]
        );
        assert_eq!(recorded.stdin.as_deref(), Some(b"keyring".as_slice()));
    }

    /// 실제 실행기: 표준 입력·환경(`LANG=C`)을 주고, 0이 아닌 종료도 결과다. 시간 초과와 없는 프로그램만
    /// 오류다.
    #[tokio::test]
    async fn system_runner_runs_commands() {
        let out = SystemRunner
            .run(
                &Cmd::new(
                    "/bin/sh",
                    &["-c", "echo \"$LANG $X\"; cat; echo err >&2; exit 3"],
                )
                .env("X", "y")
                .stdin("input"),
            )
            .await
            .unwrap();
        assert_eq!(out.status, 3);
        assert_eq!(out.stdout, b"C y\ninput");
        assert_eq!(out.stderr, b"err\n");

        let ok = SystemRunner
            .run(&Cmd::new("/bin/sh", &["-c", "cat"]))
            .await
            .unwrap();
        assert!(ok.success() && ok.stdout.is_empty());

        let err = SystemRunner
            .run(&Cmd::new("/bin/sh", &["-c", "sleep 5"]).timeout(Duration::from_millis(100)))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");

        let err = SystemRunner
            .run(&Cmd::new::<&str>("/nonexistent/opl-ceph-test", &[]))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::Io(e) if e.kind() == std::io::ErrorKind::NotFound
                && e.to_string().contains("cannot run /nonexistent/opl-ceph-test")),
            "{err:?}"
        );
    }
}
