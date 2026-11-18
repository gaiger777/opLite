//! vTPM·Secure Boot·게스트 에이전트(계획 59)의 E2E.
//!
//! `qemu-it`(이 Mac·svc01·linux-kvm): CLI로 게스트 에이전트 채널이 있는 UEFI VM을 켜 `vm guest`가 에이전트가 답하지
//! 않았다고 알리는지, 실행 중 펌웨어 초기화가 거절되는지, 끈 뒤 UEFI 변수가 있는 VM의 Secure Boot 켜기가
//! `firmware_reset_required`인지, `vm set --secure-boot on --reset-uefi-vars`가 변수 파일을 지우고 회차를 올리는지, 노드
//! 능력(`firmware_caps`)이 없으면 Secure Boot·TPM VM의 시작을 거절하는지 본다.
//! `cargo test -p opld --features qemu-it --test vtpm`
//!
//! `vtpm-it`(실제 swtpm·swtpm-tools, Linux CI `linux-kvm`): TPM VM을 켜면 swtpm_setup이 상태를 만들고 해시가 기록되며
//! swtpm이 QEMU와 함께 끝나는지, `vm firmware reset --tpm`이 상태를 지워 다음 시작이 새 TPM인지 본다.
//! `cargo test -p opld --features vtpm-it --test vtpm`
//!
//! `vtpm-it`과 `install-media-it`를 함께 켜면 실제 게스트를 띄운다(KVM이 있는 x86_64 Linux, Secure Boot 키를 넣은
//! ovmf, CI `install-media` 작업): 받아 둔 Ubuntu 24.04 클라우드 이미지(`OPL_TEST_CLOUD_IMAGE`)를 q35·Secure Boot·TPM·
//! 게스트 에이전트로 켜고 cloud-init이 qemu-guest-agent·tpm2-tools를 설치한 뒤 게스트가 `/dev/tpm0`, Secure Boot 상태,
//! EK 공개 키 해시를 보고하는지, `vm guest`가 OS·호스트 이름·IP를 보이는지, 실행 중 디스크 스냅샷에 freeze 경고가
//! 없는지, 에이전트로 끈 뒤 다시 켜도 EK가 같은지, 복제한 VM은 EK가 다른지 본다.
//! `cargo test -p opld --features vtpm-it,install-media-it -p opld --test vtpm -- --nocapture`
#![cfg(feature = "qemu-it")]

mod common;

use std::path::PathBuf;

use common::Daemon;
#[cfg(feature = "vtpm-it")]
use common::eventually;
use serde_json::Value;

/// 노드 하나. 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
async fn daemon(cluster: &str, extra: &str) -> Daemon {
    let mut d = Daemon::new(
        "node1",
        &format!("[reconciler]\nacpi_timeout_secs = 5\n{extra}"),
    );
    d.start().await;
    d.init(cluster).await;
    d.login().await;
    d
}

/// VM 상태 디렉터리(`{data_dir}/vm/{id}`)
fn vm_dir(d: &Daemon, id: &str) -> PathBuf {
    d.data_dir().join("vm").join(id)
}

/// 이 노드의 펌웨어 능력(`status.firmware_caps`)
async fn caps(d: &Daemon) -> Value {
    let node = d.ctl_json(&["node", "get", "node1"]).await;
    let caps = node["status"]["firmware_caps"].clone();
    assert!(
        caps.is_object(),
        "the node did not report firmware caps: {node}"
    );
    caps
}

/// 켜기가 실패했는지 보고 그 이유(명령 오류와 VM의 `last_error`)를 돌려준다.
async fn refused(d: &Daemon, started: anyhow::Result<String>) -> String {
    let err = started.expect_err("the VM started");
    let vm = d.ctl_json(&["vm", "get", "fw"]).await;
    format!("{err:#} / {}", vm["status"]["last_error"])
}

/// CLI 흐름(swtpm 없이도 됨): 게스트 정보, 실행 중 초기화 거절, Secure Boot 켜기와 변수 초기화, 능력 없는 노드의 거절
#[tokio::test]
async fn firmware_and_guest_agent_through_the_cli() {
    let mut d = daemon("vtpm-cli", "").await;
    let caps = caps(&d).await;
    let out = d
        .ctl(&[
            "vm",
            "create",
            "fw",
            "--memory",
            "256",
            "--guest-agent",
            "on",
            "--agent-shutdown",
            "acpi",
            "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm fw: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "fw"]).await;
    assert_eq!(vm["spec"]["guest_agent"]["shutdown"], "acpi", "{vm}");
    assert_eq!(vm["status"]["hardware"]["guest_agent"], true, "{vm}");
    let id = vm["id"].as_str().unwrap().to_string();

    // 게스트 OS가 없으니 에이전트가 답한 적이 없다.
    let guest = d.ctl_json(&["vm", "guest", "fw", "--refresh"]).await;
    assert_eq!(guest["state"], "no_agent", "{guest}");
    let out = d.ctl(&["vm", "guest", "fw"]).await.unwrap();
    assert!(out.contains("the guest agent has not answered"), "{out}");

    let err = d
        .ctl(&["vm", "firmware", "reset", "fw", "--uefi-vars"])
        .await
        .unwrap_err();
    assert!(err.to_string().ends_with("(vm_running)"), "{err}");

    d.ctl(&["vm", "stop", "fw", "--force"]).await.unwrap();
    let guest = d.ctl_json(&["vm", "guest", "fw"]).await;
    assert_eq!(guest["state"], "not_running", "{guest}");
    let vars = vm_dir(&d, &id).join("uefi-vars.fd");
    assert!(vars.exists(), "{}", vars.display());
    let vm = d.ctl_json(&["vm", "get", "fw"]).await;
    assert!(vm["status"]["firmware_node"].is_string(), "{vm}");

    // 키 없이 만든 변수가 있으니 함께 초기화해야 한다.
    let err = d
        .ctl(&["vm", "set", "fw", "--secure-boot", "on"])
        .await
        .unwrap_err();
    assert!(
        err.to_string().ends_with("(firmware_reset_required)"),
        "{err}"
    );
    assert!(err.to_string().contains("--reset-uefi-vars"), "{err}");
    let out = d
        .ctl(&[
            "vm",
            "set",
            "fw",
            "--secure-boot",
            "on",
            "--reset-uefi-vars",
        ])
        .await
        .unwrap();
    assert_eq!(out, "vm fw: UEFI variables reset, Secure Boot on\n");
    let vm = d.ctl_json(&["vm", "get", "fw"]).await;
    assert_eq!(vm["spec"]["secure_boot"], true, "{vm}");
    assert_eq!(vm["status"]["firmware_epoch"], 1, "{vm}");
    assert!(!vars.exists(), "the UEFI variables were not removed");
    assert_eq!(
        std::fs::read_to_string(vm_dir(&d, &id).join("firmware.epoch"))
            .unwrap()
            .trim(),
        "1"
    );

    // Secure Boot 펌웨어가 없는 노드(Homebrew의 descriptor에는 enrolled-keys가 없다)는 켜지 않는다. VM의 노드가 이미
    // 정해져 있으면 스케줄러를 거치지 않으므로 노드가 켜다 실패하고 그 이유를 남긴다.
    let started = d.ctl(&["vm", "start", "fw"]).await;
    if caps["secure_boot"] == true {
        assert!(started.is_ok(), "{started:?}");
        d.ctl(&["vm", "stop", "fw", "--force"]).await.unwrap();
        // 새 변수는 MS 키를 넣은 템플릿에서 복사했다.
        assert!(vars.exists());
    } else {
        let why = refused(&d, started).await;
        assert!(why.contains("Secure Boot"), "{why}");
        d.ctl(&["vm", "stop", "fw", "--force"]).await.unwrap();
    }
    let out = d
        .ctl(&[
            "vm",
            "set",
            "fw",
            "--secure-boot",
            "off",
            "--reset-uefi-vars",
        ])
        .await
        .unwrap();
    assert_eq!(out, "vm fw: UEFI variables reset, Secure Boot off\n");

    // TPM을 켠다.
    let out = d
        .ctl(&["vm", "set", "fw", "--tpm", "on", "--guest-agent", "off"])
        .await
        .unwrap();
    assert_eq!(out, "vm fw: tpm on, guest agent off\n");
    let vm = d.ctl_json(&["vm", "get", "fw"]).await;
    assert_eq!(vm["spec"]["tpm"], true, "{vm}");
    assert_eq!(vm["spec"]["guest_agent"], Value::Null, "{vm}");
    // 노드가 알린 능력대로다: swtpm을 쓸 수 있으면 켜지고, 아니면 이유를 남기고 켜지지 않는다.
    let started = d.ctl(&["vm", "start", "fw"]).await;
    if caps["tpm"] == true {
        assert!(started.is_ok(), "{started:?}");
    } else {
        assert!(
            started.is_err(),
            "the node reports no TPM capability ({caps}) but the TPM VM started: the capability probe is wrong"
        );
        let why = refused(&d, started).await;
        assert!(why.contains("swtpm"), "{why}");
    }
    d.ctl(&["vm", "stop", "fw", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "fw"]).await.unwrap();
    d.stop();
}

/// 실제 swtpm: 상태 만들기·해시·swtpm 정리·초기화
#[cfg(feature = "vtpm-it")]
mod swtpm {
    use std::time::Duration;

    use sha2::{Digest, Sha256};

    use super::*;

    fn file_hash(path: &std::path::Path) -> String {
        hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
    }

    /// 명령줄에 `pattern`이 든 프로세스가 있는지
    fn process_with(pattern: &std::path::Path) -> bool {
        std::process::Command::new("pgrep")
            .arg("-f")
            .arg(pattern)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run pgrep")
            .success()
    }

    /// VM `name`의 `tpm_sha256`이 이 노드의 상태 파일 해시와 같아질 때까지 기다려 그 해시를 돌려준다.
    async fn recorded_hash(d: &Daemon, name: &str, permall: &std::path::Path) -> String {
        let deadline = std::time::Instant::now() + common::WAIT;
        loop {
            let vm = d.ctl_json(&["vm", "get", name]).await;
            if let Some(recorded) = vm["status"]["tpm_sha256"].as_str()
                && permall.exists()
                && file_hash(permall) == recorded
            {
                return recorded.to_string();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "tpm_sha256 does not match the state file: {vm}"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    #[tokio::test]
    async fn a_tpm_vm_keeps_its_state_until_reset() {
        let mut d = daemon("vtpm-swtpm", "[test]\nfirmware_sync_secs = 2\n").await;
        assert_eq!(caps(&d).await["tpm"], true, "swtpm is not usable here");
        d.ctl(&[
            "vm", "create", "tpm", "--memory", "256", "--tpm", "on", "--start",
        ])
        .await
        .unwrap();
        let vm = d.ctl_json(&["vm", "get", "tpm"]).await;
        let id = vm["id"].as_str().unwrap().to_string();
        assert_eq!(vm["status"]["hardware"]["tpm"], true, "{vm}");
        let node = d.ctl_json(&["node", "get", "node1"]).await["id"].clone();
        let permall = vm_dir(&d, &id).join("tpm").join("tpm2-00.permall");
        let ctrl = d.path("run").join("vm").join(&id).join("tpm.sock");
        assert!(process_with(&ctrl), "no swtpm for {}", ctrl.display());
        let first = recorded_hash(&d, "tpm", &permall).await;
        let vm = d.ctl_json(&["vm", "get", "tpm"]).await;
        assert_eq!(vm["status"]["firmware_node"], node, "{vm}");

        let err = d
            .ctl(&["vm", "firmware", "reset", "tpm", "--tpm", "--yes"])
            .await
            .unwrap_err();
        assert!(err.to_string().ends_with("(vm_running)"), "{err}");

        // 끄면 swtpm도 끝나고 상태 파일은 남는다.
        d.ctl(&["vm", "stop", "tpm", "--force"]).await.unwrap();
        eventually("swtpm gone", || async { !process_with(&ctrl) }).await;
        let stopped = recorded_hash(&d, "tpm", &permall).await;

        // 다시 켜도 같은 상태 파일로 켠다(swtpm_setup은 있는 상태를 덮지 않는다).
        d.ctl(&["vm", "start", "tpm"]).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        d.ctl(&["vm", "stop", "tpm", "--force"]).await.unwrap();
        recorded_hash(&d, "tpm", &permall).await;

        let out = d
            .ctl(&["vm", "firmware", "reset", "tpm", "--tpm", "--yes"])
            .await
            .unwrap();
        assert_eq!(out, "vm tpm: firmware reset (tpm)\n");
        let vm = d.ctl_json(&["vm", "get", "tpm"]).await;
        assert_eq!(vm["status"]["tpm_sha256"], Value::Null, "{vm}");
        assert_eq!(vm["status"]["firmware_epoch"], 1, "{vm}");
        assert!(!permall.exists(), "the TPM state was not removed");

        // 새 TPM(새 EK·시드)이라 상태가 다르다.
        d.ctl(&["vm", "start", "tpm"]).await.unwrap();
        d.ctl(&["vm", "stop", "tpm", "--force"]).await.unwrap();
        let fresh = recorded_hash(&d, "tpm", &permall).await;
        assert_ne!(fresh, first);
        assert_ne!(fresh, stopped);
        d.ctl(&["vm", "delete", "tpm"]).await.unwrap();
        d.stop();
    }
}

/// 실제 게스트(KVM이 있는 x86_64 Linux, 받아 둔 Ubuntu 24.04 클라우드 이미지, Secure Boot 키를 넣은 ovmf)
#[cfg(all(feature = "vtpm-it", feature = "install-media-it", target_os = "linux"))]
mod guest {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::{Path as UrlPath, State};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use oplctl::Client;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    use super::*;

    /// 클라우드 이미지가 켜져 cloud-init이 꾸러미를 설치하고 보고하기까지 기다리는 상한
    const REPORT_WAIT: Duration = Duration::from_secs(900);
    /// 게스트 에이전트가 답하기까지 기다리는 상한
    const AGENT_WAIT: Duration = Duration::from_secs(300);

    struct Server {
        port: u16,
        reports: Arc<Mutex<Vec<Value>>>,
    }

    #[derive(Clone)]
    struct Shared {
        files: Arc<Vec<(String, Bytes)>>,
        reports: Arc<Mutex<Vec<Value>>>,
    }

    async fn file(
        State(s): State<Shared>,
        UrlPath(name): UrlPath<String>,
    ) -> Result<Bytes, StatusCode> {
        s.files
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, b)| b.clone())
            .ok_or(StatusCode::NOT_FOUND)
    }

    async fn report(State(s): State<Shared>, body: Bytes) -> StatusCode {
        let value = serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into()));
        s.reports.lock().unwrap().push(value);
        StatusCode::NO_CONTENT
    }

    /// 이미지를 `GET /files/{name}`로 내놓고 `POST /report`로 보고를 받는 127.0.0.1 서버
    async fn serve(name: &str, bytes: Bytes) -> Server {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let shared = Shared {
            files: Arc::new(vec![(name.to_string(), bytes)]),
            reports: reports.clone(),
        };
        let app = Router::new()
            .route("/files/{name}", get(file))
            .route("/report", post(report))
            .with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server { port, reports }
    }

    fn env_path(var: &str) -> PathBuf {
        PathBuf::from(std::env::var(var).unwrap_or_else(|_| panic!("set {var}")))
    }

    /// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다.
    fn client(d: &Daemon) -> Client {
        let config: Value =
            serde_json::from_slice(&std::fs::read(d.path("oplctl.json")).unwrap()).unwrap();
        Client::new(
            config["server"].as_str().unwrap(),
            config["token"].as_str().map(str::to_string),
            config["ca_pem"].as_str().map(str::to_string),
        )
        .unwrap()
    }

    /// 실패했을 때 볼 것: VM들, 받은 보고, opld 로그 끝
    async fn diagnose(d: &Daemon, vms: &[&str], server: &Server) {
        for vm in vms {
            match d.ctl(&["-o", "json", "vm", "get", vm]).await {
                Ok(json) => eprintln!("vm {vm}:\n{json}"),
                Err(e) => eprintln!("vm {vm}: {e}"),
            }
        }
        eprintln!("reports: {:?}", server.reports.lock().unwrap());
        let log = d.log();
        let tail: Vec<&str> = log.lines().rev().take(80).collect();
        eprintln!(
            "opld log (last lines):\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }

    /// 게스트가 부팅마다 보고하게 하는 vendor-data. 보고 스크립트는 파이썬 표준 라이브러리만 쓴다(리뷰 1 M4: 게스트의
    /// apt에 기대지 않는다). TPM은 sysfs(`/sys/class/tpm/tpm0`), Secure Boot는 efivar, EK는 `/dev/tpmrm0`에 보낸
    /// `TPM2_ReadPublic`(swtpm_setup이 EK를 0x81010001(RSA 2048)·0x8101001c(RSA 3072)·0x81010016(ECC)에 영구로 둔다)의
    /// 답의 SHA-256이다 — EK는 TPM의 endorsement 시드에서 정해지므로 같은 TPM이면 같다. 게스트 에이전트만 꾸러미
    /// (`qemu-guest-agent`, Ubuntu 클라우드 이미지에 없음)가 필요해 cloud-init `packages`로 넣되, 설치되지 않으면(게스트에서
    /// 저장소에 닿지 못함) 에이전트 검사를 건너뛴다. 첫 부팅에는 final 단계의 꾸러미 설치 뒤에 per-boot 스크립트가 돈다.
    /// 10.0.2.2는 slirp가 호스트의 127.0.0.1로 잇는 주소다.
    fn vendor_data(port: u16) -> String {
        let script = format!(
            r#"#!/usr/bin/env python3
import glob, hashlib, json, os, shutil, struct, subprocess, urllib.request
def run(*a):
    try:
        return subprocess.run(a, capture_output=True, text=True, timeout=60).stdout
    except Exception as e:
        return str(e)
def read(p):
    try:
        return open(p).read().strip()
    except Exception:
        return None
def efivar(prefix):
    for p in glob.glob("/sys/firmware/efi/efivars/" + prefix + "-*"):
        try:
            data = open(p, "rb").read()
            return data[4] if len(data) > 4 else -1
        except Exception:
            return -1
    return None
def ek():
    dev = "/dev/tpmrm0" if os.path.exists("/dev/tpmrm0") else "/dev/tpm0"
    errors = []
    for handle in (0x81010001, 0x8101001C, 0x81010016):
        try:
            fd = os.open(dev, os.O_RDWR)
            try:
                os.write(fd, struct.pack(">HII", 0x8001, 14, 0x173) + struct.pack(">I", handle))
                reply = os.read(fd, 4096)
            finally:
                os.close(fd)
        except Exception as e:
            errors.append(str(e))
            continue
        rc = struct.unpack(">I", reply[6:10])[0] if len(reply) >= 10 else -1
        if rc == 0:
            return hashlib.sha256(reply).hexdigest()
        errors.append(f"{{handle:#x}}: rc {{rc:#x}}")
    return "error: " + "; ".join(errors)
agent = shutil.which("qemu-ga") is not None
if agent:
    subprocess.run(["systemctl", "start", "qemu-guest-agent"], timeout=60)
report = {{
    "boot": open("/proc/sys/kernel/random/boot_id").read().strip(),
    "tpm0": os.path.isdir("/sys/class/tpm/tpm0"),
    "tpm_version": read("/sys/class/tpm/tpm0/tpm_version_major"),
    "secure_boot": efivar("SecureBoot"),
    "ek": ek(),
    "agent_installed": agent,
    "agent": run("systemctl", "is-active", "qemu-guest-agent").strip() if agent else "",
}}
request = urllib.request.Request("http://10.0.2.2:{port}/report", data=json.dumps(report).encode(),
                                 headers={{"Content-Type": "application/json"}})
urllib.request.urlopen(request, timeout=30)
"#
        );
        let config = json!({
            "packages": ["qemu-guest-agent"],
            "write_files": [{
                "path": "/var/lib/cloud/scripts/per-boot/orbi-report",
                "content": script,
                "permissions": "0755",
            }],
        });
        format!(
            "#cloud-config\n{}\n",
            serde_json::to_string_pretty(&config).unwrap()
        )
    }

    /// 보고가 `n`개가 될 때까지 기다려 마지막 보고를 돌려준다.
    async fn wait_report(d: &Daemon, server: &Server, vm: &str, n: usize) -> Value {
        let deadline = Instant::now() + REPORT_WAIT;
        loop {
            if let Some(r) = server.reports.lock().unwrap().get(n - 1) {
                return r.clone();
            }
            if Instant::now() > deadline {
                diagnose(d, &[vm], server).await;
                panic!("no report {n} from the guest in {REPORT_WAIT:?}");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// `vm guest --refresh`가 `answered`이고 10.0.2.x 주소가 있을 때까지 기다린다.
    async fn wait_guest_info(d: &Daemon, server: &Server, vm: &str) -> Value {
        let deadline = Instant::now() + AGENT_WAIT;
        loop {
            let guest = d.ctl_json(&["vm", "guest", vm, "--refresh"]).await;
            let has_ip = guest["info"]["interfaces"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|n| n["addresses"].as_array().cloned().unwrap_or_default())
                .any(|a| a["ip"].as_str().is_some_and(|ip| ip.starts_with("10.0.2.")));
            if guest["state"] == "answered" && has_ip {
                return guest;
            }
            if Instant::now() > deadline {
                eprintln!("guest: {guest}");
                diagnose(d, &[vm], server).await;
                panic!("the guest agent of {vm} did not answer in {AGENT_WAIT:?}");
            }
            // `refresh`는 VM마다 5초에 한 번이다.
            tokio::time::sleep(Duration::from_secs(6)).await;
        }
    }

    /// Secure Boot·TPM·게스트 에이전트로 Ubuntu 클라우드 이미지를 켜서 보고·게스트 정보·스냅샷·에이전트 종료·다시
    /// 켜기의 같은 EK·복제의 다른 EK를 본다.
    #[tokio::test]
    async fn a_cloud_image_boots_with_secure_boot_a_tpm_and_the_guest_agent() {
        let image = env_path("OPL_TEST_CLOUD_IMAGE");
        let bytes = Bytes::from(std::fs::read(&image).unwrap());
        let sha = hex::encode(Sha256::digest(&bytes));
        let server = serve("noble.img", bytes).await;
        // 게스트가 ACPI로도 꺼지므로 에이전트 종료를 보려면 ACPI 한도를 길게 둔다.
        let mut d = Daemon::new(
            "node1",
            "[reconciler]\nacpi_timeout_secs = 300\n[download]\nallow_private = true\n",
        );
        d.start().await;
        d.init("vtpm-guest").await;
        d.login().await;
        let caps = caps(&d).await;
        assert_eq!(
            caps,
            json!({ "tpm": true, "secure_boot": true }),
            "this node needs swtpm and ovmf with enrolled Secure Boot keys"
        );
        let pool = d.path("pool");
        std::fs::create_dir(&pool).unwrap();
        d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
            .await
            .unwrap();
        d.wait_pool_ready("local").await;
        let url = format!("http://127.0.0.1:{}/files/noble.img", server.port);
        d.ctl(&[
            "volume",
            "download",
            "noble",
            "--pool",
            "local",
            "--url",
            &url,
            "--sha256",
            &sha,
            "--source-format",
            "qcow2",
        ])
        .await
        .unwrap();
        let vendor = d.path("vendor-data");
        std::fs::write(&vendor, vendor_data(server.port)).unwrap();
        let started = d
            .ctl(&[
                "vm",
                "create",
                "sb",
                "--memory",
                "2048",
                "--cores",
                "2",
                "--machine",
                "q35",
                "--disk",
                "noble",
                "--nic",
                "user",
                "--tpm",
                "on",
                "--secure-boot",
                "on",
                "--guest-agent",
                "on",
                "--ci-hostname",
                "sb-probe",
                "--ci-user",
                "orbi",
                "--ci-vendor-data",
                vendor.to_str().unwrap(),
                "--start",
            ])
            .await;
        if let Err(e) = started {
            diagnose(&d, &["sb"], &server).await;
            panic!("vm sb did not start: {e:#}");
        }
        let first = wait_report(&d, &server, "sb", 1).await;
        eprintln!("report 1: {first}");
        assert_eq!(first["tpm0"], true, "{first}");
        assert_eq!(first["tpm_version"], "2", "{first}");
        assert_eq!(first["secure_boot"], 1, "{first}");
        let ek = first["ek"].as_str().unwrap_or_default().to_string();
        assert_eq!(ek.len(), 64, "no EK hash: {first}");
        // 게스트 에이전트는 게스트가 꾸러미를 받아야 있다. 없으면 에이전트 검사만 건너뛴다.
        let agent = first["agent"] == "active";
        if !agent {
            eprintln!(
                "SKIP guest agent checks: qemu-guest-agent is not running in the guest \
                 (cloud-init could not install it; the guest may have no route to the Ubuntu archive): {first}"
            );
        }

        let c = client(&d);
        let task = c
            .post(
                "/vms/sb/snapshots",
                &json!({ "name": "frozen", "description": "", "memory": false }),
            )
            .await
            .unwrap();
        let task = c.wait_task(&task).await.unwrap();
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
        if agent {
            // 게스트 에이전트: OS·호스트 이름·slirp 주소
            let guest = wait_guest_info(&d, &server, "sb").await;
            eprintln!("guest info: {guest}");
            let info = &guest["info"];
            assert_eq!(info["hostname"], "sb-probe", "{guest}");
            assert!(
                info["os"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("Ubuntu"),
                "{guest}"
            );
            let out = d.ctl(&["vm", "guest", "sb"]).await.unwrap();
            assert!(out.contains("hostname: sb-probe"), "{out}");
            // 실행 중 디스크 스냅샷은 파일 시스템을 얼려 찍는다(경고 없음).
            assert_eq!(task["status"]["error"], Value::Null, "{task}");
        }

        // 에이전트로 끈다(ACPI 한도 300초보다 훨씬 빨리). 에이전트가 없으면 ACPI로 끄되 기다리지 않는다.
        let at = Instant::now();
        if agent {
            d.ctl(&["vm", "stop", "sb"]).await.unwrap();
            assert!(
                at.elapsed() < Duration::from_secs(120),
                "stopping took {:?}",
                at.elapsed()
            );
        } else {
            d.ctl(&["vm", "stop", "sb", "--force"]).await.unwrap();
        }
        let vm = d.ctl_json(&["vm", "get", "sb"]).await;
        let recorded = vm["status"]["tpm_sha256"].clone();
        assert!(recorded.is_string(), "{vm}");

        // 다시 켜면 같은 TPM(같은 EK)이다.
        d.ctl(&["vm", "start", "sb"]).await.unwrap();
        let second = wait_report(&d, &server, "sb", 2).await;
        eprintln!("report 2: {second}");
        assert_ne!(second["boot"], first["boot"], "{second}");
        assert_eq!(second["ek"], ek.as_str(), "{second}");
        assert_eq!(second["secure_boot"], 1, "{second}");
        d.ctl(&["vm", "stop", "sb", "--force"]).await.unwrap();

        // 복제한 VM은 새 TPM이다(EK가 다름).
        d.ctl(&["vm", "snapshot", "delete", "sb", "frozen"])
            .await
            .unwrap();
        let out = d.ctl(&["vm", "clone", "sb", "sb-clone"]).await.unwrap();
        assert!(out.starts_with("vm sb-clone cloned from sb"), "{out}");
        d.ctl(&["vm", "start", "sb-clone"]).await.unwrap();
        let third = wait_report(&d, &server, "sb-clone", 3).await;
        eprintln!("report 3: {third}");
        assert_eq!(third["tpm0"], true, "{third}");
        let clone_ek = third["ek"].as_str().unwrap_or_default();
        assert_eq!(clone_ek.len(), 64, "{third}");
        assert_ne!(clone_ek, ek, "the clone kept the TPM of its source");

        let clone = d.ctl_json(&["vm", "get", "sb-clone"]).await;
        let clone_disk = clone["spec"]["disks"][0]["volume"]
            .as_str()
            .unwrap()
            .to_string();
        d.ctl(&["vm", "stop", "sb-clone", "--force"]).await.unwrap();
        for vm in ["sb-clone", "sb"] {
            d.ctl(&["vm", "delete", vm]).await.unwrap();
        }
        d.ctl(&["volume", "delete", &clone_disk]).await.unwrap();
        d.ctl(&["volume", "delete", "noble"]).await.unwrap();
        d.stop();
    }
}
