//! opld 프로세스를 띄우고 oplctl(라이브러리)로 조작하는 하네스.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub const WAIT: Duration = Duration::from_secs(30);

pub struct Daemon {
    pub name: String,
    pub dir: TempDir,
    pub port: u16,
    pub cluster_port: u16,
    child: Option<Child>,
    /// `cluster init`이 출력한 것
    pub admin_password: Option<String>,
    pub ca_fingerprint: Option<String>,
    pub join_url: Option<String>,
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
    pub fn new(name: &str, extra_config: &str) -> Self {
        // systemd가 있는 Linux(CI)에서만 `OPL_TEST_SYSTEMD=1`로 QEMU를 스코프로 감싼다.
        let systemd_scope = std::env::var_os("OPL_TEST_SYSTEMD").is_some();
        Self::with_hypervisor(
            name,
            extra_config,
            &format!("systemd_scope = {systemd_scope}"),
        )
    }

    /// `[hypervisor]` 절의 내용을 정해 만든다.
    pub fn with_hypervisor(name: &str, extra_config: &str, hypervisor: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("opl")
            .tempdir_in("/tmp")
            .unwrap();
        let port = free_port();
        let cluster_port = free_port();
        let share =
            std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
        let config = format!(
            "name = \"{name}\"\naddress = \"127.0.0.1\"\nlisten = \"127.0.0.1:{port}\"\n\
             cluster_listen = \"127.0.0.1:{cluster_port}\"\ncontrol_socket = \"{sock}\"\n\
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}\n\
             [hypervisor]\n{hypervisor}\n",
            sock = dir.path().join("opld.sock").display(),
            data = dir.path().join("data").display(),
            run = dir.path().join("run").display(),
        );
        std::fs::write(dir.path().join("opld.toml"), config).unwrap();
        Self {
            name: name.into(),
            dir,
            port,
            cluster_port,
            child: None,
            admin_password: None,
            ca_fingerprint: None,
            join_url: None,
        }
    }

    pub fn server(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn socket(&self) -> String {
        self.path("opld.sock").display().to_string()
    }

    /// opld를 띄우고 제어 소켓이 응답할 때까지 기다린다. 멤버였던 노드면 API까지 기다린다.
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
        let member = self.data_dir().join("pki").exists();
        self.wait_until(|status| status.is_some_and(|s| !member || s["state"] == "member"))
            .await;
        if member {
            self.wait_api().await;
        }
    }

    /// 제어 소켓의 상태(`GET /v1/status`)가 `ready`를 만족할 때까지 기다린다.
    async fn wait_until(&mut self, ready: impl Fn(Option<&serde_json::Value>) -> bool) {
        let http = reqwest::Client::builder()
            .unix_socket(self.path("opld.sock"))
            .build()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let status = match http.get("http://opld/v1/status").send().await {
                Ok(resp) => resp.json::<serde_json::Value>().await.ok(),
                Err(_) => None,
            };
            if ready(status.as_ref()) {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("opld exited with {status}:\n{}", self.log());
            }
            assert!(
                Instant::now() < deadline,
                "opld did not get ready:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// API가 HTTPS로 응답할 때까지 기다린다.
    async fn wait_api(&mut self) {
        let http = reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        let url = format!("{}/api/v1/auth/whoami", self.server());
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(resp) = http.get(&url).send().await
                && resp.status() == reqwest::StatusCode::UNAUTHORIZED
            {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("opld exited with {status}:\n{}", self.log());
            }
            assert!(
                Instant::now() < deadline,
                "API did not start:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// `oplctl cluster init`으로 새 클러스터를 만들고 출력(비밀번호, CA 지문, 조인 URL)을 기억한다.
    pub async fn init(&mut self, cluster: &str) -> String {
        let socket = self.socket();
        let out = self
            .ctl(&["cluster", "init", "--name", cluster, "--socket", &socket])
            .await
            .unwrap();
        let field = |prefix: &str| {
            out.lines()
                .find_map(|l| l.trim().strip_prefix(prefix).map(str::to_string))
                .unwrap_or_else(|| panic!("no {prefix:?} in {out}"))
        };
        self.admin_password = Some(field("admin password (shown only once): "));
        self.ca_fingerprint = Some(field("CA fingerprint: "));
        self.join_url = Some(format!("opl-join://{}", field("opl-join://")));
        self.wait_api().await;
        out
    }

    /// `oplctl cluster join`
    pub async fn join(&mut self, url: &str) -> anyhow::Result<String> {
        let socket = self.socket();
        let out = self
            .ctl(&["cluster", "join", url, "--socket", &socket])
            .await?;
        self.wait_api().await;
        Ok(out)
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

    /// `start`한 뒤 아직 멈추지 않았는지
    pub fn is_running(&self) -> bool {
        self.child.is_some()
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

    /// oplctl 명령을 실행한다. 로그인 정보는 이 데몬의 임시 디렉터리에 둔다.
    pub async fn ctl(&self, args: &[&str]) -> anyhow::Result<String> {
        let config = self.path("oplctl.json");
        let mut argv = vec!["oplctl", "--config", config.to_str().unwrap()];
        argv.extend_from_slice(args);
        let mut out = Vec::new();
        oplctl::run(argv, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }

    /// 이 데몬의 API에 admin으로 로그인한다. 비밀번호와 CA 지문은 `init`한 데몬의 것을 쓴다.
    pub async fn login_with(&self, password: &str, ca_fingerprint: &str) {
        let file = self.path("admin-password");
        std::fs::write(&file, password).unwrap();
        let server = self.server();
        self.ctl(&[
            "login",
            "--server",
            &server,
            "--user",
            "admin",
            "--password-file",
            file.to_str().unwrap(),
            "--ca-fingerprint",
            ca_fingerprint,
        ])
        .await
        .unwrap();
    }

    pub async fn login(&self) {
        self.login_with(
            self.admin_password.as_deref().unwrap(),
            self.ca_fingerprint.as_deref().unwrap(),
        )
        .await;
    }

    /// `oplctl -o json`의 결과
    pub async fn ctl_json(&self, args: &[&str]) -> serde_json::Value {
        let mut argv = vec!["-o", "json"];
        argv.extend_from_slice(args);
        serde_json::from_str(&self.ctl(&argv).await.unwrap()).unwrap()
    }

    /// 이 노드가 풀 `pool`을 준비됐다고 알릴 때까지 기다린다. 볼륨 만들기와 배치는 준비된 노드만 고른다.
    pub async fn wait_pool_ready(&self, pool: &str) {
        eventually(&format!("pool {pool} ready on {}", self.name), || async {
            let nodes = self.ctl_json(&["node", "list"]).await;
            nodes.as_array().unwrap().iter().any(|n| {
                n["name"] == self.name.as_str() && n["status"]["pools"][pool]["ready"] == true
            })
        })
        .await;
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // 테스트가 중간에 실패해도 이 데몬이 띄운 QEMU(데몬과 따로 돈다)를 남기지 않는다.
        // 명령줄에 이 임시 디렉터리의 경로가 든 프로세스만 맞는다.
        let _ = Command::new("pkill")
            .arg("-KILL")
            .arg("-f")
            .arg(self.path("run").join("vm").display().to_string())
            .status();
    }
}

/// 조건이 맞을 때까지 다시 확인한다.
pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT;
    while !check().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
