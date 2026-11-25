//! 템플릿·연결 복제·이미지 라이브러리(계획 58)의 E2E: CLI로 VM을 템플릿으로 바꾸고, 디렉터리 풀의 qcow2 겹층(상대
//! backing)으로 연결 배포해 켜고, 기반 지우기·되돌리기·실행 중 평탄화가 거절되는지, 끈 뒤 평탄화하면 독립 볼륨이 되는지,
//! 여러 VM 배포(`--count`)·전체 복사(`--full`)·템플릿 지우기를 본다. 127.0.0.1 HTTP 서버의 작은 qcow2와 체크섬 파일로
//! `volume download --checksum-url|--sha512`, 관리자 카탈로그 항목, `image get --template`(받기 → 템플릿)을 본다. 게스트
//! OS는 없다(UEFI 펌웨어만).
//! `cargo test -p orbivisord --features qemu-it --test templates` (이 Mac·svc01·linux-kvm). qemu-img·qemu-io가 필요하다.
//!
//! `templates-it`를 함께 켜면 실제 게스트를 띄운다(KVM이 있는 x86_64 Linux, CI `install-media` 작업): 받아 둔 Ubuntu
//! 24.04 클라우드 이미지(`OPL_TEST_CLOUD_IMAGE`)와 그 `SHA256SUMS`(`OPL_TEST_CLOUD_SUMS`, 없으면 시험이 만든다)를 시험의
//! HTTP 서버로 내놓고 관리자 카탈로그 항목(`checksum: sums`)으로 받아 템플릿을 만든 뒤, 디스크를 늘린 연결 복제 둘을
//! cloud-init과 함께 켜 두 게스트가 보고하는지(호스트 이름·사용자·늘어난 루트 파일 시스템), 기반 파일이 그대로인지,
//! 평탄화한 VM이 다시 켜져 보고하는지, 복제를 지운 뒤 템플릿을 되돌릴 수 있는지 본다.
//! `cargo test -p orbivisord --features templates-it,qemu-it --test templates -- --nocapture`
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use common::Daemon;
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};

/// 시험의 127.0.0.1 HTTP 서버: `GET /files/{name}`는 내놓은 파일, `POST /report`는 게스트의 보고
struct Server {
    port: u16,
    #[cfg_attr(not(feature = "templates-it"), allow(dead_code))]
    reports: Arc<Mutex<Vec<Value>>>,
}

impl Server {
    fn url(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/files/{name}", self.port)
    }
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

/// `files`(이름, 내용)를 내놓는 서버를 띄운다.
async fn serve(files: Vec<(String, Bytes)>) -> Server {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let shared = Shared {
        files: Arc::new(files),
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

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 노드(시험 호스트)의 아키텍처 이름(`x86_64`·`aarch64`)
fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// 디렉터리 풀 `local`이 있는 노드 하나. `[download] allow_private`로 시험 서버(127.0.0.1)에서 받는다.
async fn daemon(cluster: &str) -> (Daemon, PathBuf) {
    // 디스크에 OS가 없는 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new(
        "node1",
        "[reconciler]\nacpi_timeout_secs = 5\n[download]\nallow_private = true\n",
    );
    d.start().await;
    d.init(cluster).await;
    d.login().await;
    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    (d, pool)
}

/// 디렉터리 풀의 볼륨 `name` 파일(`{id}.{format}`)
async fn volume_file(d: &Daemon, pool: &Path, name: &str) -> PathBuf {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    pool.join(format!(
        "{}.{}",
        volume["id"].as_str().unwrap(),
        volume["spec"]["format"].as_str().unwrap()
    ))
}

/// `qemu-img info`(JSON)
fn image_info(path: &Path) -> Value {
    let out = Command::new("qemu-img")
        .args(["info", "--output=json", "-U"])
        .arg(path)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 무늬가 다르면 실패한다.
fn qemu_io(path: &Path, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

fn file_hash(path: &Path) -> String {
    sha256(&std::fs::read(path).unwrap())
}

/// 풀마다 같은 흐름(common::linked_clone_flow)과, 디렉터리 풀만의 것: 상대 backing, 복제에 써도 기반 파일이 그대로,
/// `--count` 배포, `vm clone --full`, 이름 겹침과 여러 VM의 같은 호스트 이름 거절, 템플릿의 PHASE 표시
#[tokio::test]
async fn templates_and_linked_clones_on_a_directory_pool() {
    let (mut d, pool) = daemon("templates").await;
    common::linked_clone_flow(&d, "local", true).await;
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);

    // 기반 디스크에 무늬를 쓰고 템플릿을 만든다.
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M",
    ])
    .await
    .unwrap();
    let base = volume_file(&d, &pool, "root").await;
    assert!(qemu_io(&base, "write -P 0x5a 0 1M"));
    d.ctl(&["vm", "create", "tmpl", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    d.ctl(&["template", "convert", "tmpl"]).await.unwrap();
    // 두 번 바꿔도 된다.
    d.ctl(&["template", "convert", "tmpl"]).await.unwrap();
    let base_hash = file_hash(&base);

    let err = d
        .ctl(&[
            "template",
            "deploy",
            "tmpl",
            "--name",
            "web-{n}",
            "--count",
            "2",
            "--ci-hostname",
            "web",
        ])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("--ci-hostname"), "{err}");
    let out = d
        .ctl(&[
            "template", "deploy", "tmpl", "--name", "web-{n}", "--count", "2", "--linked",
            "--memory", "128",
        ])
        .await
        .unwrap();
    assert_eq!(
        out,
        "vm web-1 deployed from tmpl (linked)\nvm web-2 deployed from tmpl (linked)\n"
    );
    let err = d
        .ctl(&["template", "deploy", "tmpl", "--name", "web-1"])
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("cannot deploy vm web-1"),
        "{err:#}"
    );

    let base_id = d.ctl_json(&["volume", "get", "root"]).await["id"].clone();
    for name in ["web-1", "web-2"] {
        let vm = d.ctl_json(&["vm", "get", name]).await;
        assert_eq!(vm["spec"]["memory_mib"], 128, "{vm}");
        assert_eq!(vm["spec"]["power"], "stopped", "{vm}");
        let disk = vm["spec"]["disks"][0]["volume"].as_str().unwrap();
        let volume = d.ctl_json(&["volume", "get", disk]).await;
        assert_eq!(volume["spec"]["base"], base_id, "{volume}");
        assert_eq!(volume["spec"]["format"], "qcow2", "{volume}");
        // 겹층은 같은 디렉터리의 기반을 상대 경로로 가리킨다.
        let file = volume_file(&d, &pool, disk).await;
        let info = image_info(&file);
        assert_eq!(
            info["backing-filename"],
            base.file_name().unwrap().to_str().unwrap(),
            "{info}"
        );
        assert!(qemu_io(&file, "read -P 0x5a 0 1M"));
        assert!(qemu_io(&file, "write -P 0x33 0 512K"));
    }
    assert_eq!(file_hash(&base), base_hash);
    let table = d.ctl(&["template", "list"]).await.unwrap();
    assert_eq!(table, "NAME  DISKS  POOL   CLONES\ntmpl  root   local  2\n");
    let table = d.ctl(&["vm", "list"]).await.unwrap();
    let row = table.lines().find(|l| l.starts_with("tmpl ")).unwrap();
    assert!(row.contains(" template "), "{table}");
    let table = d.ctl(&["volume", "list"]).await.unwrap();
    assert!(
        table
            .lines()
            .any(|l| l.starts_with("web-1-") && l.ends_with("  root")),
        "{table}"
    );

    // 전체 복사는 기반이 없다.
    let out = d
        .ctl(&["vm", "clone", "tmpl", "full", "--full"])
        .await
        .unwrap();
    assert_eq!(out, "vm full cloned from tmpl\n");
    let vm = d.ctl_json(&["vm", "get", "full"]).await;
    let full_disk = vm["spec"]["disks"][0]["volume"]
        .as_str()
        .unwrap()
        .to_string();
    let volume = d.ctl_json(&["volume", "get", &full_disk]).await;
    assert_eq!(volume["spec"]["base"], Value::Null, "{volume}");
    let file = volume_file(&d, &pool, &full_disk).await;
    assert_eq!(image_info(&file)["backing-filename"], Value::Null);
    assert!(qemu_io(&file, "read -P 0x5a 0 1M"));

    // 평탄화한 복제는 기반 없이 같은 내용이다.
    let vm = d.ctl_json(&["vm", "get", "web-1"]).await;
    let disk = vm["spec"]["disks"][0]["volume"]
        .as_str()
        .unwrap()
        .to_string();
    d.ctl(&["volume", "flatten", &disk]).await.unwrap();
    let file = volume_file(&d, &pool, &disk).await;
    assert_eq!(image_info(&file)["backing-filename"], Value::Null);
    assert!(qemu_io(&file, "read -P 0x33 0 512K"));
    assert!(qemu_io(&file, "read -P 0x5a 512K 512K"));
    let err = d.ctl(&["volume", "flatten", &disk]).await.unwrap_err();
    assert!(err.to_string().contains("is not a linked clone"), "{err}");

    // 남은 연결 복제(web-2)가 있으면 되돌리지 못한다. 지우면 된다.
    let err = d.ctl(&["template", "unconvert", "tmpl"]).await.unwrap_err();
    assert!(err.to_string().contains("web-2"), "{err}");
    for name in ["web-1", "web-2", "full"] {
        let vm = d.ctl_json(&["vm", "get", name]).await;
        let disk = vm["spec"]["disks"][0]["volume"]
            .as_str()
            .unwrap()
            .to_string();
        d.ctl(&["vm", "delete", name]).await.unwrap();
        d.ctl(&["volume", "delete", &disk]).await.unwrap();
    }
    let out = d.ctl(&["template", "unconvert", "tmpl"]).await.unwrap();
    assert_eq!(out, "template tmpl converted back to a VM\n");
    let vm = d.ctl_json(&["vm", "get", "tmpl"]).await;
    assert_eq!(vm["spec"]["template"], Value::Null, "{vm}");
    let err = d.ctl(&["template", "delete", "tmpl"]).await.unwrap_err();
    assert!(err.to_string().contains("is not a template"), "{err}");
    assert_eq!(file_hash(&base), base_hash);
    d.ctl(&["vm", "delete", "tmpl"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
    d.stop();
}

/// 작은 qcow2 이미지(64 MiB, 첫 1 MiB에 무늬)
fn tiny_image(d: &Daemon) -> Vec<u8> {
    let path = d.path("tiny.qcow2");
    let status = Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(&path)
        .arg("64M")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(qemu_io(&path, "write -P 0x7e 0 1M"));
    std::fs::read(&path).unwrap()
}

/// 체크섬 파일과 고정 SHA-512로 받기, 관리자 카탈로그 항목, 받기 → 템플릿 → 배포
#[tokio::test]
async fn image_catalog_downloads_a_template() {
    let (mut d, pool) = daemon("images").await;
    let image = tiny_image(&d);
    let sums = format!(
        "{}  other.img\n{}  tiny.qcow2\n",
        "0".repeat(64),
        sha256(&image)
    );
    let sha512 = hex::encode(Sha512::digest(&image));
    let server = serve(vec![
        ("tiny.qcow2".into(), Bytes::from(image)),
        ("SHA256SUMS".into(), Bytes::from(sums)),
        ("EMPTYSUMS".into(), Bytes::from_static(b"# nothing\n")),
    ])
    .await;
    let url = server.url("tiny.qcow2");

    let download = |name: &'static str, check: &'static str, value: String| {
        let url = url.clone();
        let d = &d;
        async move {
            d.ctl(&[
                "volume",
                "download",
                name,
                "--pool",
                "local",
                "--url",
                &url,
                check,
                &value,
                "--source-format",
                "qcow2",
            ])
            .await
        }
    };
    let out = download("by-sums", "--checksum-url", server.url("SHA256SUMS"))
        .await
        .unwrap();
    assert_eq!(out, "volume by-sums downloaded\n");
    let out = download("by-sha512", "--sha512", sha512).await.unwrap();
    assert_eq!(out, "volume by-sha512 downloaded\n");
    let err = download("no-entry", "--checksum-url", server.url("EMPTYSUMS"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("has no entry for tiny.qcow2"),
        "{err}"
    );
    for name in ["by-sums", "by-sha512"] {
        let file = volume_file(&d, &pool, name).await;
        assert!(qemu_io(&file, "read -P 0x7e 0 1M"));
        d.ctl(&["volume", "delete", name]).await.unwrap();
    }

    // 관리자 항목. 내장 항목은 지우지 못한다.
    let out = d
        .ctl(&[
            "image",
            "catalog",
            "add",
            "tiny",
            "--url",
            &url,
            "--checksum-url",
            &server.url("SHA256SUMS"),
            "--arch",
            arch(),
            "--os",
            "tiny",
            "--version",
            "1",
            "--min-size",
            "64M",
        ])
        .await
        .unwrap();
    assert_eq!(out, "image tiny added to the catalog\n");
    let err = d
        .ctl(&["image", "catalog", "remove", "ubuntu-24.04"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("built-in"), "{err}");
    let list = d.ctl_json(&["image", "list", "--arch", arch()]).await;
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"ubuntu-24.04") && ids.contains(&"tiny"),
        "{list}"
    );
    assert!(
        list.as_array().unwrap().iter().all(|i| i["arch"] == arch()),
        "{list}"
    );

    let out = d
        .ctl(&[
            "image",
            "get",
            "tiny",
            "--pool",
            "local",
            "--template",
            "tiny-tmpl",
            "--network",
            "user",
            "--memory",
            "256",
            "--size",
            "128M",
            "--ci-user",
            "orbi",
        ])
        .await
        .unwrap();
    let volume = if arch() == "aarch64" {
        "tiny-arm64"
    } else {
        "tiny"
    };
    assert_eq!(
        out,
        format!("volume {volume} ready\ntemplate tiny-tmpl created\n")
    );
    let vm = d.ctl_json(&["vm", "get", "tiny-tmpl"]).await;
    assert_eq!(vm["spec"]["template"], true, "{vm}");
    assert_eq!(vm["spec"]["disks"][0]["volume"], volume, "{vm}");
    assert_eq!(vm["spec"]["nics"][0]["network"], "user", "{vm}");
    assert_eq!(vm["spec"]["cloud_init"]["user"], "orbi", "{vm}");
    let size = d.ctl_json(&["volume", "get", volume]).await["spec"]["size_bytes"].clone();
    assert_eq!(size, 128 << 20);
    let table = d.ctl(&["image", "list"]).await.unwrap();
    assert!(
        table
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>())
            .any(|row| row[0] == "tiny" && row[5..] == ["custom", volume]),
        "{table}"
    );

    // 받은 템플릿에서 배포해 켠다(cloud-init 씨앗은 배포한 VM의 이름을 호스트 이름으로).
    let out = d
        .ctl(&[
            "template",
            "deploy",
            "tiny-tmpl",
            "--name",
            "tiny-1",
            "--start",
        ])
        .await
        .unwrap();
    assert_eq!(
        out,
        "vm tiny-1 deployed from tiny-tmpl\nvm tiny-1: running on node1\n"
    );
    let vm = d.ctl_json(&["vm", "get", "tiny-1"]).await;
    let id = vm["id"].as_str().unwrap();
    let seed = std::fs::read(d.path("run").join("vm").join(id).join("cidata.iso")).unwrap();
    let seed = String::from_utf8_lossy(&seed);
    assert!(seed.contains("\"local-hostname\": \"tiny-1\""));
    assert!(seed.contains("\"user\": \"orbi\""));
    let disk = vm["spec"]["disks"][0]["volume"]
        .as_str()
        .unwrap()
        .to_string();
    d.ctl(&["vm", "stop", "tiny-1", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "tiny-1"]).await.unwrap();
    d.ctl(&["volume", "delete", &disk]).await.unwrap();
    d.ctl(&["template", "delete", "tiny-tmpl"]).await.unwrap();
    d.ctl(&["volume", "delete", volume]).await.unwrap();
    let out = d
        .ctl(&["image", "catalog", "remove", "tiny"])
        .await
        .unwrap();
    assert_eq!(out, "image tiny removed from the catalog\n");
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
    d.stop();
}

/// 실제 게스트(KVM이 있는 x86_64 Linux, 받아 둔 Ubuntu 24.04 클라우드 이미지)
#[cfg(feature = "templates-it")]
mod guest {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;

    /// 클라우드 이미지가 켜져 cloud-init이 끝나고 보고하기까지 기다리는 상한
    const REPORT_WAIT: Duration = Duration::from_secs(600);

    fn env_path(var: &str) -> PathBuf {
        PathBuf::from(std::env::var(var).unwrap_or_else(|_| panic!("set {var}")))
    }

    /// 실패했을 때 볼 것: VM들(`vm get`), 받은 보고, opld 로그 끝
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

    /// 게스트가 부팅마다 보고하게 하는 vendor-data: cloud-init의 per-boot 스크립트(첫 부팅에도 final 단계에서 돈다).
    /// 10.0.2.2는 slirp가 호스트의 127.0.0.1로 잇는 주소다.
    fn vendor_data(port: u16) -> String {
        let script = format!(
            r#"#!/usr/bin/env python3
import json, os, socket, subprocess, urllib.request
def run(*a):
    try:
        return subprocess.run(a, capture_output=True, text=True).stdout
    except Exception as e:
        return str(e)
def read(p):
    try:
        return open(p).read().strip()
    except Exception as e:
        return str(e)
root = os.statvfs("/")
report = {{
    "hostname": socket.gethostname(),
    "user": run("getent", "passwd", "orbi"),
    "keys": read("/home/orbi/.ssh/authorized_keys"),
    "root_bytes": root.f_blocks * root.f_frsize,
    "boot": read("/proc/sys/kernel/random/boot_id"),
    "instance": read("/run/cloud-init/.instance-id"),
}}
request = urllib.request.Request("http://10.0.2.2:{port}/report", data=json.dumps(report).encode(),
                                 headers={{"Content-Type": "application/json"}})
urllib.request.urlopen(request, timeout=30)
"#
        );
        let config = json!({
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

    /// 호스트 이름이 `host`인 보고가 `n`개가 될 때까지 기다려 마지막 것을 돌려준다.
    async fn wait_report(d: &Daemon, server: &Server, host: &str, n: usize) -> Value {
        let deadline = Instant::now() + REPORT_WAIT;
        loop {
            let reports: Vec<Value> = server
                .reports
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["hostname"] == host)
                .cloned()
                .collect();
            if let Some(r) = reports.get(n - 1) {
                return r.clone();
            }
            if Instant::now() > deadline {
                diagnose(d, &["ci-1", "ci-2"], server).await;
                panic!("no report {n} from {host} in {REPORT_WAIT:?}");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// 관리자 카탈로그 항목(체크섬 파일)으로 받아 템플릿을 만들고, 디스크를 8 GiB로 늘린 연결 복제 둘을 켜 두 게스트의
    /// 보고(호스트 이름, cloud-init 사용자·키, growpart로 6 GiB 넘게 늘어난 루트)를 본다. 기반 파일은 그대로이고 지우지 못한다.
    /// `ci-1`을 끄고 평탄화한 뒤 다시 켜면 새 부팅으로 보고한다. 복제를 지우면 템플릿을 되돌린다.
    #[tokio::test]
    async fn linked_clones_of_a_cloud_image_template_boot_and_flatten() {
        let image = env_path("OPL_TEST_CLOUD_IMAGE");
        let file_name = image.file_name().unwrap().to_str().unwrap().to_string();
        let bytes = Bytes::from(std::fs::read(&image).unwrap());
        let sums = match std::env::var_os("OPL_TEST_CLOUD_SUMS") {
            Some(path) => Bytes::from(std::fs::read(path).unwrap()),
            None => Bytes::from(format!("{} *{file_name}\n", sha256(&bytes))),
        };
        let server = serve(vec![
            (file_name.clone(), bytes),
            ("SHA256SUMS".into(), sums),
        ])
        .await;
        let (mut d, pool) = daemon("guest-templates").await;
        d.ctl(&[
            "image",
            "catalog",
            "add",
            "ci-noble",
            "--url",
            &server.url(&file_name),
            "--checksum-url",
            &server.url("SHA256SUMS"),
            "--os",
            "ubuntu",
            "--version",
            "24.04",
            "--arch",
            "x86_64",
            "--default-user",
            "ubuntu",
        ])
        .await
        .unwrap();
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPr0beOrbivisorTemplatesProbeKey00000000 probe@orbivisor";
        let out = d
            .ctl(&[
                "image",
                "get",
                "ci-noble",
                "--pool",
                "local",
                "--template",
                "ubuntu-tmpl",
                "--network",
                "user",
                "--memory",
                "1024",
                "--ci-user",
                "orbi",
                "--ci-ssh-key",
                key,
            ])
            .await
            .unwrap();
        assert_eq!(out, "volume ci-noble ready\ntemplate ubuntu-tmpl created\n");
        let base = volume_file(&d, &pool, "ci-noble").await;
        let base_hash = file_hash(&base);

        let vendor = d.path("vendor-data");
        std::fs::write(&vendor, vendor_data(server.port)).unwrap();
        let deployed = d
            .ctl(&[
                "template",
                "deploy",
                "ubuntu-tmpl",
                "--linked",
                "--count",
                "2",
                "--name",
                "ci-{n}",
                "--disk-size",
                "disk0=8G",
                "--ci-vendor-data",
                vendor.to_str().unwrap(),
                "--start",
            ])
            .await;
        match deployed {
            Ok(out) => assert_eq!(
                out,
                "vm ci-1 deployed from ubuntu-tmpl (linked)\nvm ci-1: running on node1\n\
                 vm ci-2 deployed from ubuntu-tmpl (linked)\nvm ci-2: running on node1\n"
            ),
            Err(e) => {
                diagnose(&d, &["ci-1", "ci-2"], &server).await;
                panic!("deploy failed: {e:#}");
            }
        }
        let mut first = Vec::new();
        for host in ["ci-1", "ci-2"] {
            let report = wait_report(&d, &server, host, 1).await;
            eprintln!("report from {host}: {report}");
            assert!(
                report["user"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("orbi:"),
                "{report}"
            );
            assert!(
                report["keys"].as_str().unwrap_or_default().contains(key),
                "{report}"
            );
            // 이미지(3.5 GiB, 루트 파일 시스템 2.5 GiB쯤)의 루트 파티션을 growpart가 8 GiB 디스크 끝까지 늘린다. 디스크에는
            // /boot(913 MiB)·EFI 파티션도 있어 루트는 7 GiB가 조금 안 된다.
            assert!(
                report["root_bytes"].as_u64().unwrap_or(0) > 6 << 30,
                "the root file system did not grow: {report}"
            );
            first.push(report);
        }
        assert_ne!(first[0]["instance"], first[1]["instance"]);
        assert_eq!(file_hash(&base), base_hash, "the base file changed");
        let err = d.ctl(&["volume", "delete", "ci-noble"]).await.unwrap_err();
        assert!(err.to_string().ends_with("(volume_is_template)"), "{err}");

        // ci-1을 평탄화하고 다시 켠다.
        let vm = d.ctl_json(&["vm", "get", "ci-1"]).await;
        let disk = vm["spec"]["disks"][0]["volume"]
            .as_str()
            .unwrap()
            .to_string();
        d.ctl(&["vm", "stop", "ci-1", "--force"]).await.unwrap();
        d.ctl(&["volume", "flatten", &disk]).await.unwrap();
        let volume = d.ctl_json(&["volume", "get", &disk]).await;
        assert_eq!(volume["spec"]["base"], Value::Null, "{volume}");
        assert_eq!(
            image_info(&volume_file(&d, &pool, &disk).await)["backing-filename"],
            Value::Null
        );
        d.ctl(&["vm", "start", "ci-1"]).await.unwrap();
        let again = wait_report(&d, &server, "ci-1", 2).await;
        eprintln!("report from ci-1 after flattening: {again}");
        assert_ne!(again["boot"], first[0]["boot"], "{again}");
        assert_eq!(again["instance"], first[0]["instance"], "{again}");
        assert_eq!(file_hash(&base), base_hash, "the base file changed");

        let err = d
            .ctl(&["template", "unconvert", "ubuntu-tmpl"])
            .await
            .unwrap_err();
        assert!(err.to_string().ends_with("(template_in_use)"), "{err}");
        for name in ["ci-1", "ci-2"] {
            let vm = d.ctl_json(&["vm", "get", name]).await;
            let disk = vm["spec"]["disks"][0]["volume"]
                .as_str()
                .unwrap()
                .to_string();
            d.ctl(&["vm", "stop", name, "--force"]).await.unwrap();
            d.ctl(&["vm", "delete", name]).await.unwrap();
            d.ctl(&["volume", "delete", &disk]).await.unwrap();
        }
        let out = d
            .ctl(&["template", "unconvert", "ubuntu-tmpl"])
            .await
            .unwrap();
        assert_eq!(out, "template ubuntu-tmpl converted back to a VM\n");
        d.ctl(&["vm", "delete", "ubuntu-tmpl"]).await.unwrap();
        d.ctl(&["volume", "delete", "ci-noble"]).await.unwrap();
        d.ctl(&["image", "catalog", "remove", "ci-noble"])
            .await
            .unwrap();
        d.stop();
    }
}
