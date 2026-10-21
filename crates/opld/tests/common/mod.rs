//! opld 프로세스를 띄우고 oplctl(라이브러리)로 조작하는 하네스.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub struct Daemon {
    pub dir: TempDir,
    pub port: u16,
    child: Option<Child>,
}

/// 비어 있는 포트를 하나 고른다.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Daemon {
    /// 유닉스 소켓 경로 길이 제한 때문에 /tmp 아래 짧은 경로를 쓴다.
    pub fn new(extra_config: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("opl")
            .tempdir_in("/tmp")
            .unwrap();
        let port = free_port();
        let share =
            std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
        let config = format!(
            "name = \"node1\"\nlisten = \"127.0.0.1:{port}\"\ndata_dir = \"{data}\"\n\
             run_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}",
            data = dir.path().join("data").display(),
            run = dir.path().join("run").display(),
        );
        std::fs::write(dir.path().join("opld.toml"), config).unwrap();
        Self {
            dir,
            port,
            child: None,
        }
    }

    pub fn server(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// opld를 띄우고 API가 응답할 때까지 기다린다.
    pub async fn start(&mut self) {
        assert!(self.child.is_none());
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("opld.log"))
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_opld"))
            .arg("--config")
            .arg(self.path("opld.toml"))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
        let url = format!("{}/api/v1/auth/whoami", self.server());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(resp) = reqwest::get(&url).await
                && resp.status() == reqwest::StatusCode::UNAUTHORIZED
            {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("opld exited with {status}:\n{}", self.log());
            }
            assert!(
                Instant::now() < deadline,
                "opld did not start:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// SIGTERM을 보내고 끝나기를 기다린다.
    pub fn stop(&mut self) {
        let mut child = self.child.take().unwrap();
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&child),
            rustix::process::Signal::TERM,
        )
        .unwrap();
        let status = child.wait().unwrap();
        assert!(
            status.success(),
            "opld exited with {status}:\n{}",
            self.log()
        );
    }

    /// kill -9
    pub fn kill(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.path("opld.log")).unwrap_or_default()
    }

    /// oplctl 명령을 이 데몬에 대고 실행한다. 로그인 정보는 임시 디렉터리에 둔다.
    pub async fn ctl(&self, args: &[&str]) -> anyhow::Result<String> {
        let config = self.path("oplctl.json");
        let mut argv = vec!["oplctl", "--config", config.to_str().unwrap()];
        argv.extend_from_slice(args);
        let mut out = Vec::new();
        oplctl::run(argv, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }

    pub async fn login(&self) {
        let password = self.data_dir().join("initial-admin-password");
        let server = self.server();
        self.ctl(&[
            "login",
            "--server",
            &server,
            "--user",
            "admin",
            "--password-file",
            password.to_str().unwrap(),
        ])
        .await
        .unwrap();
    }

    /// `oplctl -o json`의 결과
    pub async fn ctl_json(&self, args: &[&str]) -> serde_json::Value {
        let mut argv = vec!["-o", "json"];
        argv.extend_from_slice(args);
        serde_json::from_str(&self.ctl(&argv).await.unwrap()).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
