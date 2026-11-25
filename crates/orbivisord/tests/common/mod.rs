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
    /// 위트니스(설정 `witness = true`)는 HTTPS API를 열지 않는다.
    pub witness: bool,
    child: Option<Child>,
    /// `cluster init`이 출력한 것
    pub admin_password: Option<String>,
    pub ca_fingerprint: Option<String>,
    pub join_url: Option<String>,
    /// 있으면 opld에 `NOTIFY_SOCKET`으로 준다(systemd 알림 시험).
    pub notify_socket: Option<PathBuf>,
}

/// 비어 있는 포트를 하나 고른다. 운영체제의 임시 포트 범위(Linux 32768~60999, macOS 49152~65535)
/// 밖인 20000~31999에서 시험 프로세스마다 다른 구간을 차례로 나눠 준다 — 임시 포트로 받은 번호는
/// 돌려준 뒤 다른 시험의 나가는 연결이 다시 가져갈 수 있어, opld가 그 포트를 열 때 가끔
/// "Address already in use"가 났다.
pub fn free_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let base = 20000 + (std::process::id() % 24) as u16 * 500;
    loop {
        let port = base + NEXT.fetch_add(1, Ordering::Relaxed) % 500;
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
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
             [hypervisor]\n{hypervisor}\n[updates]\nenabled = false\n",
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
            witness: false,
            child: None,
            admin_password: None,
            ca_fingerprint: None,
            join_url: None,
            notify_socket: None,
        }
    }

    /// 위트니스. QEMU가 없어도 돌아야 하므로 `qemu_binary`는 없는 파일을 가리킨다.
    pub fn witness(name: &str) -> Self {
        let mut d = Self::with_hypervisor(
            name,
            "witness = true",
            "qemu_binary = \"/nonexistent/qemu-system\"",
        );
        d.witness = true;
        d
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

    /// opld를 띄우고 제어 소켓이 응답할 때까지 기다린다. 멤버였던 노드면 API까지 기다린다(위트니스는
    /// 멤버가 될 때까지만).
    pub async fn start(&mut self) {
        assert!(self.child.is_none());
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("opld.log"))
            .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_orbivisord"));
        command
            .arg("--config")
            .arg(self.path("opld.toml"))
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        match &self.notify_socket {
            Some(path) => command.env("NOTIFY_SOCKET", path),
            None => command.env_remove("NOTIFY_SOCKET"),
        };
        let child = command.spawn().unwrap();
        self.child = Some(child);
        let member = self.data_dir().join("pki").exists();
        self.wait_until(|status| status.is_some_and(|s| !member || s["state"] == "member"))
            .await;
        if member && !self.witness {
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

    /// `oplctl cluster join`. 위트니스는 API가 없으니 제어 소켓이 멤버라고 알릴 때까지 기다린다.
    pub async fn join(&mut self, url: &str) -> anyhow::Result<String> {
        let socket = self.socket();
        let out = self
            .ctl(&["cluster", "join", url, "--socket", &socket])
            .await?;
        if self.witness {
            self.wait_until(|status| status.is_some_and(|s| s["state"] == "member"))
                .await;
        } else {
            self.wait_api().await;
        }
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

    /// 돌고 있는 opld의 pid
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
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
        orbivisorctl::run(argv, &mut out).await?;
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

/// 템플릿과 연결 복제의 한 흐름(계획 58, 풀마다 같은 단계): 풀 `pool`에 디스크 하나인 VM을 만들어 템플릿으로 바꾸고,
/// 켜기가 거절되는지 본 뒤, 디스크를 늘려 배포하고 켠다. `linked`(풀이 연결 복제를 만듦)면 새 디스크의 `base`가
/// 템플릿 디스크이고, 템플릿 디스크 지우기·되돌리기·실행 중 평탄화가 거절되며, 끈 뒤 평탄화하면 `base`가 없어진다.
/// 아니면(공유 LVM) 전체 복사이고 평탄화는 400이다. 평탄화한 디스크로 다시 켠 뒤 모두 지운다(템플릿 디스크 볼륨까지).
/// 볼륨·VM 이름은 `pool`로 시작한다.
pub async fn linked_clone_flow(d: &Daemon, pool: &str, linked: bool) {
    let (base, tmpl, web) = (
        format!("{pool}-tmpl-root"),
        format!("{pool}-tmpl"),
        format!("{pool}-web"),
    );
    d.ctl(&["volume", "create", &base, "--pool", pool, "--size", "64M"])
        .await
        .unwrap();
    d.ctl(&["vm", "create", &tmpl, "--memory", "256", "--disk", &base])
        .await
        .unwrap();
    let out = d.ctl(&["template", "convert", &tmpl]).await.unwrap();
    assert_eq!(out, format!("vm {tmpl} is now a template\n"));
    let err = d.ctl(&["vm", "start", &tmpl]).await.unwrap_err();
    assert!(err.to_string().ends_with("(vm_is_template)"), "{err}");

    let out = d
        .ctl(&[
            "template",
            "deploy",
            &tmpl,
            "--name",
            &web,
            "--disk-size",
            "disk0=128M",
            "--start",
        ])
        .await
        .unwrap();
    assert_eq!(
        out,
        format!(
            "vm {web} deployed from {tmpl}\nvm {web}: running on {}\n",
            d.name
        )
    );
    let vm = d.ctl_json(&["vm", "get", &web]).await;
    assert_eq!(vm["spec"]["template"], serde_json::Value::Null, "{vm}");
    assert_eq!(vm["status"]["phase"], "running", "{vm}");
    let disk = vm["spec"]["disks"][0]["volume"]
        .as_str()
        .unwrap()
        .to_string();
    let volume = d.ctl_json(&["volume", "get", &disk]).await;
    let base_volume = d.ctl_json(&["volume", "get", &base]).await;
    assert_eq!(volume["spec"]["pool"], pool, "{volume}");
    assert_eq!(volume["spec"]["size_bytes"], 128 << 20, "{volume}");
    if linked {
        assert_eq!(volume["spec"]["base"], base_volume["id"], "{volume}");
        let table = d.ctl(&["template", "list"]).await.unwrap();
        assert!(
            table
                .lines()
                .any(|l| l.split_whitespace().eq([tmpl.as_str(), &base, pool, "1"])),
            "{table}"
        );
        let err = d.ctl(&["volume", "delete", &base]).await.unwrap_err();
        assert!(err.to_string().ends_with("(volume_is_template)"), "{err}");
        let err = d.ctl(&["template", "unconvert", &tmpl]).await.unwrap_err();
        assert!(err.to_string().ends_with("(template_in_use)"), "{err}");
        let err = d.ctl(&["volume", "flatten", &disk]).await.unwrap_err();
        assert!(err.to_string().ends_with("(vm_running)"), "{err}");
    } else {
        assert_eq!(volume["spec"]["base"], serde_json::Value::Null, "{volume}");
    }

    d.ctl(&["vm", "stop", &web, "--force"]).await.unwrap();
    if linked {
        let out = d.ctl(&["volume", "flatten", &disk]).await.unwrap();
        assert_eq!(out, format!("volume {disk} flattened\n"));
        let volume = d.ctl_json(&["volume", "get", &disk]).await;
        assert_eq!(volume["spec"]["base"], serde_json::Value::Null, "{volume}");
    } else {
        let err = d.ctl(&["volume", "flatten", &disk]).await.unwrap_err();
        assert!(err.to_string().contains("is not a linked clone"), "{err}");
    }
    let out = d.ctl(&["vm", "start", &web]).await.unwrap();
    assert!(out.contains(&format!("vm {web}: start done")), "{out}");
    d.ctl(&["vm", "stop", &web, "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", &web]).await.unwrap();
    d.ctl(&["volume", "delete", &disk]).await.unwrap();
    let out = d.ctl(&["template", "delete", &tmpl]).await.unwrap();
    assert_eq!(
        out,
        format!("template {tmpl} deleted (its disk volumes are kept)\n")
    );
    d.ctl(&["volume", "delete", &base]).await.unwrap();
}
