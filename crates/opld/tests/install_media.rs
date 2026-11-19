//! 설치 매체와 cloud-init(계획 57)의 E2E: CLI로 ISO를 올리고, CD-ROM 드라이브와 cloud-init이 있는 VM을 켜고, 실행
//! 중에 매체를 빼고 넣고, 꽂힌 ISO를 지우지 못하는지와 cloud-init 변경이 다음 시작에 새 씨앗으로 가는지 본다. 게스트
//! OS가 없으므로(UEFI·BIOS 펌웨어만) 실제 QEMU의 매체는 opld를 멈춘 사이 QMP `query-block`으로 본다(opld가 QMP를
//! 쥐고 있다 — VM은 opld 없이 계속 돈다).
//! `cargo test -p opld --features qemu-it --test install_media` (이 Mac·svc01·linux-kvm)
//!
//! `install-media-it`를 함께 켜면 실제 게스트를 띄운다(KVM이 있는 x86_64 Linux, CI `install-media` 작업): 설치 ISO
//! (`OPL_TEST_INSTALL_ISO`, Alpine virt)를 시험의 127.0.0.1 HTTP 서버에서 `iso download`로 받아 BIOS·UEFI VM이 CD에서
//! 부트로더·커널을 읽는지, 클라우드 이미지(`OPL_TEST_CLOUD_IMAGE`, Ubuntu 24.04)를 `volume download`로 받아 cloud-init이
//! 호스트 이름·사용자·비밀번호 해시·SSH 키·고정 IPv4를 적용하고 vendor-data의 runcmd가 그 결과를 시험 서버에 보내는지,
//! 호스트 이름을 바꾸고 다시 켜면 새 instance-id로 다시 적용하는지 본다. 계획 63: 러너 CPU에 있는 플래그를
//! `vm create --cpu-flags=-FLAG`로 숨기면 게스트의 `/proc/cpuinfo`에 없고, 실행 중에 지우면 보류로 남다가 `vm reboot`(전원
//! 껐다 켜기) 뒤에 보이는지 본다. 계획 64: `vm create --sockets 2 --numa 2`로 켜면 게스트가 NUMA 노드 둘(소켓마다
//! CPU 하나, 메모리 반씩)을 본다.
//! `cargo test -p opld --features install-media-it,qemu-it --test install_media -- --nocapture`
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};

use common::Daemon;
use opl_qemu::QmpClient;
use opl_qemu::iso9660::build;
use serde_json::{Value, json};

/// 시험 ISO 이미지(ISO 9660 + Joliet)를 `d`의 임시 디렉터리에 쓴다.
fn write_iso(d: &Daemon, file: &str, text: &[u8]) -> PathBuf {
    let bytes = build("INSTALL", &[("readme.txt", text)]).unwrap();
    let path = d.path(file);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// 볼륨 `name`의 파일 경로(디렉터리 풀 `pool` 아래 `{id}.raw`)
async fn volume_file(d: &Daemon, pool: &Path, name: &str) -> PathBuf {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    pool.join(format!("{}.raw", volume["id"].as_str().unwrap()))
}

/// opld를 멈추고(VM은 계속 돈다) VM `id`의 QEMU에 QMP로 붙어 CD-ROM·씨앗 드라이브마다의 매체 파일을 읽은 뒤 opld를
/// 다시 띄운다.
async fn qemu_media(d: &mut Daemon, id: &str) -> Vec<(String, Option<PathBuf>)> {
    d.kill();
    let socket = d.path("run").join("vm").join(id).join("qmp.sock");
    let media = {
        let qmp = QmpClient::connect(&socket).await.unwrap();
        qmp.query_block_media().await.unwrap()
    };
    d.start().await;
    let mut media: Vec<_> = media
        .into_iter()
        .filter(|(dev, _)| dev.starts_with("cd") || dev.starts_with("cidata"))
        .collect();
    media.sort();
    media
}

#[tokio::test]
async fn cdroms_and_cloud_init_through_the_cli() {
    // 디스크에 OS가 없는 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("media").await;
    d.login().await;
    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;

    // ISO 올리기. ISO 9660·UDF가 아닌 파일은 가져오기 Task가 거절하고 볼륨을 지운다.
    for (name, text) in [("inst", &b"installer"[..]), ("tools", b"tools")] {
        let file = write_iso(&d, &format!("{name}.iso"), text);
        let out = d
            .ctl(&[
                "iso",
                "upload",
                name,
                "--pool",
                "local",
                "--file",
                file.to_str().unwrap(),
            ])
            .await
            .unwrap();
        assert_eq!(out, format!("iso {name} uploaded\n"));
    }
    let junk = d.path("junk.iso");
    std::fs::write(&junk, vec![0u8; 64 << 10]).unwrap();
    let err = d
        .ctl(&[
            "iso",
            "upload",
            "junk",
            "--pool",
            "local",
            "--file",
            junk.to_str().unwrap(),
        ])
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not an ISO 9660 or UDF image"),
        "{err}"
    );
    let isos = d.ctl_json(&["iso", "list"]).await;
    let names: Vec<&str> = isos
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["inst", "tools"], "{isos}");
    assert_eq!(isos[0]["spec"]["content"], "iso");
    assert_eq!(isos[0]["spec"]["format"], "raw");

    // CD 둘(하나는 빈 드라이브)과 cloud-init이 있는 VM을 켠다.
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M",
    ])
    .await
    .unwrap();
    let password = d.path("ci-password");
    std::fs::write(&password, "s3cret-pass\n").unwrap();
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPr0beOrbivisorInstallMediaTestKey0000000 it@orbivisor";
    let out = d
        .ctl(&[
            "vm",
            "create",
            "web",
            "--memory",
            "256",
            "--disk",
            "root",
            "--cdrom",
            "inst",
            "--cdrom",
            "none",
            "--ci-user",
            "orbi",
            "--ci-password-file",
            password.to_str().unwrap(),
            "--ci-ssh-key",
            key,
            "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    let id = vm["id"].as_str().unwrap().to_string();
    assert_eq!(vm["spec"]["cdroms"], json!([{ "iso": "inst" }, {}]), "{vm}");
    assert_eq!(vm["spec"]["cloud_init"]["password"], "********", "{vm}");
    let hw = &vm["status"]["hardware"];
    assert_eq!(hw["cdroms"], vm["spec"]["cdroms"], "{vm}");
    let first_instance = hw["cloud_init"].as_str().unwrap().to_string();
    assert!(first_instance.starts_with("iid-"), "{vm}");

    // 씨앗은 VM 실행 디렉터리의 ISO이고 해시만 담는다(평문 비밀번호 없음).
    let seed_path = d.path("run").join("vm").join(&id).join("cidata.iso");
    let seed = std::fs::read(&seed_path).unwrap();
    assert_eq!(&seed[16 * 2048 + 1..16 * 2048 + 6], b"CD001");
    let text = String::from_utf8_lossy(&seed);
    assert!(text.contains(&format!("\"instance-id\": \"{first_instance}\"")));
    assert!(text.contains("\"local-hostname\": \"web\""));
    assert!(text.contains("\"user\": \"orbi\""));
    assert!(text.contains("\"password\": \"$6$"));
    assert!(text.contains(key));
    assert!(!text.contains("s3cret-pass"));

    // ISO 목록에 쓰는 VM이 보인다.
    let table = d.ctl(&["iso", "list"]).await.unwrap();
    let row = table.lines().find(|l| l.starts_with("inst ")).unwrap();
    assert!(row.ends_with("node1  web"), "{table}");

    // 실행 중 빼기와 넣기
    let out = d.ctl(&["vm", "cdrom", "eject", "web"]).await.unwrap();
    assert_eq!(
        out,
        "vm web: cdrom0 ejected\nvm web: applied while running\n"
    );
    let out = d
        .ctl(&["vm", "cdrom", "insert", "web", "tools", "--drive", "1"])
        .await
        .unwrap();
    assert_eq!(
        out,
        "vm web: tools inserted in cdrom1\nvm web: applied while running\n"
    );
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(
        vm["spec"]["cdroms"],
        json!([{}, { "iso": "tools" }]),
        "{vm}"
    );
    assert_eq!(vm["status"]["hardware"]["cdroms"], vm["spec"]["cdroms"]);
    let pid = vm["status"]["pid"].as_u64().unwrap();
    let tools = volume_file(&d, &pool, "tools").await;
    assert_eq!(
        qemu_media(&mut d, &id).await,
        [
            ("cd0-dev".to_string(), None),
            ("cd1-dev".to_string(), Some(tools)),
            ("cidata-dev".to_string(), Some(seed_path.clone())),
        ]
    );
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["pid"].as_u64(), Some(pid), "{vm}");

    // 꽂힌 ISO는 지우지 못하고, 실행 중에는 드라이브를 더하지 못한다.
    let err = d.ctl(&["iso", "delete", "tools"]).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("is in the CD-ROM drive of VM \"web\""),
        "{err}"
    );
    let err = d.ctl(&["vm", "cdrom", "add", "web"]).await.unwrap_err();
    assert!(err.to_string().ends_with("(vm_running)"), "{err}");

    // 실행 중 cloud-init 변경은 다음 QEMU 시작 때 적용된다.
    let out = d
        .ctl(&["vm", "set", "web", "--ci-hostname", "web2"])
        .await
        .unwrap();
    assert!(
        out.starts_with(&format!(
            "vm web: cloud-init updated\nvm web: pending until restart:\n  cloud_init: {first_instance} -> iid-"
        )),
        "{out}"
    );
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "start", "web"]).await.unwrap();
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    let instance = vm["status"]["hardware"]["cloud_init"].as_str().unwrap();
    assert_ne!(instance, first_instance, "{vm}");
    let pending = vm["status"]["hardware"]["pending"].as_array();
    assert!(pending.is_none_or(Vec::is_empty), "{vm}");
    let seed = std::fs::read(&seed_path).unwrap();
    assert!(String::from_utf8_lossy(&seed).contains("\"local-hostname\": \"web2\""));

    // 멈춘 VM: 드라이브 빼기, 부팅 순서 바꾸기, cloud-init 끄기
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let out = d.ctl(&["vm", "cdrom", "remove", "web"]).await.unwrap();
    assert_eq!(out, "vm web: cdrom1 removed\n");
    let out = d
        .ctl(&[
            "vm",
            "set",
            "web",
            "--boot",
            "cdrom,disk",
            "--no-cloud-init",
        ])
        .await
        .unwrap();
    assert_eq!(
        out,
        "vm web: boot order cdrom,disk\nvm web: cloud-init off\n"
    );
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["spec"]["cdroms"], json!([{ "boot_order": 1 }]), "{vm}");
    assert_eq!(vm["spec"]["disks"][0]["boot_order"], 2, "{vm}");
    assert_eq!(vm["spec"]["cloud_init"], Value::Null, "{vm}");

    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    for name in ["inst", "tools"] {
        let out = d.ctl(&["iso", "delete", name]).await.unwrap();
        assert_eq!(out, format!("iso {name} deleted\n"));
    }
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
    d.stop();
}

/// 실제 게스트(KVM이 있는 x86_64 Linux, 인터넷에서 받아 둔 설치 ISO와 클라우드 이미지)
#[cfg(all(feature = "install-media-it", target_os = "linux"))]
mod guest {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::{Path as UrlPath, State};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use sha2::{Digest, Sha256};

    use super::*;

    /// 설치 ISO가 부트로더·커널·initramfs를 CD에서 읽었다고 볼 양
    const BOOT_READ: u64 = 8 << 20;
    /// 펌웨어 → 부트로더 → 커널까지 기다리는 상한
    const BOOT_WAIT: Duration = Duration::from_secs(180);
    /// 클라우드 이미지가 켜져 cloud-init이 끝나고 보고하기까지 기다리는 상한
    const REPORT_WAIT: Duration = Duration::from_secs(600);

    /// 시험의 127.0.0.1 HTTP 서버: `GET /files/{name}`는 받을 이미지, `POST /report`는 게스트의 보고
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

    /// `files`(이름, 파일)를 내놓는 서버를 띄운다. 이미지는 메모리에 읽어 둔다.
    async fn serve(files: &[(&str, &Path)]) -> Server {
        let files = files
            .iter()
            .map(|(name, path)| (name.to_string(), Bytes::from(std::fs::read(path).unwrap())))
            .collect();
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

    fn sha256(path: &Path) -> String {
        hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
    }

    fn env_path(var: &str) -> PathBuf {
        PathBuf::from(std::env::var(var).unwrap_or_else(|_| panic!("set {var}")))
    }

    /// 실패했을 때 볼 것: VM들(`vm get`), 받은 보고, opld 로그 끝
    async fn diagnose(d: &Daemon, vms: &[&str], server: Option<&Server>) {
        for vm in vms {
            match d.ctl(&["-o", "json", "vm", "get", vm]).await {
                Ok(json) => eprintln!("vm {vm}:\n{json}"),
                Err(e) => eprintln!("vm {vm}: {e}"),
            }
        }
        if let Some(server) = server {
            eprintln!("reports: {:?}", server.reports.lock().unwrap());
        }
        let log = d.log();
        let tail: Vec<&str> = log.lines().rev().take(80).collect();
        eprintln!(
            "opld log (last lines):\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }

    async fn daemon(cluster: &str) -> (Daemon, PathBuf) {
        // 시험 서버는 127.0.0.1이다(배포 기본은 사설·루프백 주소에서 받지 않음).
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

    /// 드라이브 `dev`가 읽은 바이트(`query-blockstats`)
    async fn read_bytes(qmp: &QmpClient, dev: &str) -> u64 {
        let stats = qmp.query_blockstats().await.unwrap();
        stats
            .iter()
            .find(|s| {
                s.qdev.as_deref().is_some_and(|q| {
                    q == dev || q.strip_prefix("/machine/peripheral/") == Some(dev)
                })
            })
            .map_or(0, |s| s.rd_bytes)
    }

    /// 설치 ISO를 받아 빈 디스크와 그 CD로 BIOS·UEFI VM을 켜고, 펌웨어가 CD에서 부트로더와 커널을 읽는지 본다.
    #[tokio::test]
    async fn boots_an_installer_iso_with_bios_and_uefi() {
        let iso = env_path("OPL_TEST_INSTALL_ISO");
        let server = serve(&[("alpine.iso", &iso)]).await;
        let (mut d, _pool) = daemon("installer").await;
        let url = format!("http://127.0.0.1:{}/files/alpine.iso", server.port);
        let out = d
            .ctl(&[
                "iso",
                "download",
                "alpine",
                "--pool",
                "local",
                "--url",
                &url,
                "--sha256",
                &sha256(&iso),
            ])
            .await
            .unwrap();
        assert_eq!(out, "iso alpine downloaded\n");

        let vms = [("bios", "bios"), ("uefi", "uefi")];
        let mut ids = Vec::new();
        for (name, firmware) in vms {
            let root = format!("{name}-root");
            d.ctl(&["volume", "create", &root, "--pool", "local", "--size", "1G"])
                .await
                .unwrap();
            let started = d
                .ctl(&[
                    "vm",
                    "create",
                    name,
                    "--memory",
                    "512",
                    "--machine",
                    "q35",
                    "--firmware",
                    firmware,
                    "--disk",
                    &root,
                    "--cdrom",
                    "alpine",
                    "--start",
                ])
                .await;
            if let Err(e) = started {
                diagnose(&d, &[name], None).await;
                panic!("vm {name} did not start: {e}");
            }
            let vm = d.ctl_json(&["vm", "get", name]).await;
            ids.push((name, vm["id"].as_str().unwrap().to_string()));
        }

        // opld가 QMP를 쥐고 있으므로 멈춘 사이(VM은 계속 돈다) 시험이 QMP로 읽은 양을 본다.
        d.kill();
        let deadline = Instant::now() + BOOT_WAIT;
        let mut read = vec![0; ids.len()];
        for (i, (_, id)) in ids.iter().enumerate() {
            let socket = d.path("run").join("vm").join(id).join("qmp.sock");
            let qmp = QmpClient::connect(&socket).await.unwrap();
            loop {
                read[i] = read_bytes(&qmp, "cd0-dev").await;
                if read[i] > BOOT_READ || Instant::now() > deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let stats = qmp.query_blockstats().await.unwrap();
            eprintln!("vm {}: blockstats {stats:?}", ids[i].0);
        }
        d.start().await;
        let names: Vec<&str> = ids.iter().map(|(n, _)| *n).collect();
        if read.iter().any(|&n| n <= BOOT_READ) {
            diagnose(&d, &names, None).await;
            panic!("the firmware did not boot from the CD: {names:?} read {read:?} bytes");
        }
        for (name, _) in vms {
            d.ctl(&["vm", "stop", name, "--force"]).await.unwrap();
            d.ctl(&["vm", "delete", name]).await.unwrap();
            d.ctl(&["volume", "delete", &format!("{name}-root")])
                .await
                .unwrap();
        }
        d.ctl(&["iso", "delete", "alpine"]).await.unwrap();
        d.stop();
    }

    /// 게스트가 cloud-init의 결과를 보내는 vendor-data: 스크립트를 쓰고 runcmd로 돌린다. 10.0.2.2는 slirp가 호스트의
    /// 127.0.0.1로 잇는 주소다.
    fn vendor_data(port: u16) -> String {
        let script = format!(
            r#"import json, socket, subprocess, urllib.request
def run(*a):
    try:
        return subprocess.run(a, capture_output=True, text=True).stdout
    except Exception as e:
        return str(e)
def read(p):
    try:
        return open(p).read()
    except Exception as e:
        return str(e)
shadow = run("getent", "shadow", "orbi").split(":")
report = {{
    "hostname": socket.gethostname(),
    "user": run("getent", "passwd", "orbi"),
    "shadow": shadow[1] if len(shadow) > 1 else "",
    "keys": read("/home/orbi/.ssh/authorized_keys"),
    "netplan": read("/etc/netplan/50-cloud-init.yaml"),
    "instance": read("/run/cloud-init/.instance-id"),
    "addresses": run("ip", "-o", "addr"),
}}
request = urllib.request.Request("http://10.0.2.2:{port}/report", data=json.dumps(report).encode(),
                                 headers={{"Content-Type": "application/json"}})
urllib.request.urlopen(request, timeout=30)
"#
        );
        let config = json!({
            "write_files": [{ "path": "/run/orbi-report.py", "content": script, "permissions": "0755" }],
            "runcmd": [["python3", "/run/orbi-report.py"]],
        });
        format!(
            "#cloud-config\n{}\n",
            serde_json::to_string_pretty(&config).unwrap()
        )
    }

    /// 보고가 `n`개가 될 때까지 기다려 마지막 보고를 돌려준다.
    async fn wait_report(d: &Daemon, server: &Server, n: usize) -> Value {
        let deadline = Instant::now() + REPORT_WAIT;
        loop {
            if let Some(r) = server.reports.lock().unwrap().get(n - 1) {
                return r.clone();
            }
            if Instant::now() > deadline {
                diagnose(d, &["probe"], Some(server)).await;
                panic!("no report {n} from the guest in {REPORT_WAIT:?}");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// 클라우드 이미지를 받아 cloud-init(호스트 이름·사용자·비밀번호 해시·SSH 키·고정 IPv4·DNS·vendor-data)으로 켜고,
    /// 게스트가 보낸 결과를 본다. 호스트 이름을 바꾸고 다시 켜면 새 instance-id로 다시 적용한다.
    #[tokio::test]
    async fn cloud_init_configures_a_cloud_image() {
        let image = env_path("OPL_TEST_CLOUD_IMAGE");
        let server = serve(&[("noble.img", &image)]).await;
        let (mut d, _pool) = daemon("cloud").await;
        let url = format!("http://127.0.0.1:{}/files/noble.img", server.port);
        let out = d
            .ctl(&[
                "volume",
                "download",
                "noble",
                "--pool",
                "local",
                "--url",
                &url,
                "--sha256",
                &sha256(&image),
                "--source-format",
                "qcow2",
            ])
            .await
            .unwrap();
        assert_eq!(out, "volume noble downloaded\n");

        let hash = opl_types::crypt::sha512_crypt(b"orbi-probe-password", "probesalt12");
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPr0beOrbivisorCloudInitProbeKey00000000 probe@orbivisor";
        let key_file = d.path("probe.pub");
        std::fs::write(&key_file, format!("# probe key\n{key}\n")).unwrap();
        let vendor = d.path("vendor-data");
        std::fs::write(&vendor, vendor_data(server.port)).unwrap();
        let key_arg = format!("@{}", key_file.display());
        let started = d
            .ctl(&[
                "vm",
                "create",
                "probe",
                "--memory",
                "1024",
                "--machine",
                "q35",
                "--disk",
                "noble",
                "--nic",
                "user",
                "--ci-hostname",
                "ci-probe",
                "--ci-user",
                "orbi",
                "--ci-password-hash",
                &hash,
                "--ci-ssh-key",
                &key_arg,
                "--ci-ip",
                "10.0.2.15/24,gw=10.0.2.2",
                "--ci-dns",
                "10.0.2.3",
                "--ci-vendor-data",
                vendor.to_str().unwrap(),
                "--start",
            ])
            .await;
        if let Err(e) = started {
            diagnose(&d, &["probe"], Some(&server)).await;
            panic!("vm probe did not start: {e}");
        }
        let vm = d.ctl_json(&["vm", "get", "probe"]).await;
        let instance = vm["status"]["hardware"]["cloud_init"]
            .as_str()
            .unwrap()
            .to_string();

        let report = wait_report(&d, &server, 1).await;
        eprintln!("report 1: {report}");
        let text = |k: &str| report[k].as_str().unwrap_or_default().to_string();
        assert_eq!(text("hostname"), "ci-probe", "{report}");
        assert!(text("user").starts_with("orbi:"), "{report}");
        assert_eq!(text("shadow"), hash, "{report}");
        assert!(text("keys").contains(key), "{report}");
        assert!(text("netplan").contains("10.0.2.15/24"), "{report}");
        assert!(!text("netplan").contains("dhcp4: true"), "{report}");
        assert_eq!(text("instance").trim(), instance, "{report}");

        // 호스트 이름을 바꾸면 instance-id가 바뀌고, 새 QEMU에서 cloud-init이 다시 적용한다.
        let out = d
            .ctl(&["vm", "set", "probe", "--ci-hostname", "ci-probe2"])
            .await
            .unwrap();
        assert!(out.contains("  cloud_init: "), "{out}");
        d.ctl(&["vm", "reboot", "probe"]).await.unwrap();
        let report = wait_report(&d, &server, 2).await;
        eprintln!("report 2: {report}");
        assert_eq!(report["hostname"], "ci-probe2", "{report}");
        let vm = d.ctl_json(&["vm", "get", "probe"]).await;
        assert_ne!(vm["status"]["hardware"]["cloud_init"], instance, "{vm}");

        d.ctl(&["vm", "stop", "probe", "--force"]).await.unwrap();
        d.ctl(&["vm", "delete", "probe"]).await.unwrap();
        d.ctl(&["volume", "delete", "noble"]).await.unwrap();
        d.stop();
    }

    /// 숨길 플래그(QEMU 이름, `/proc/cpuinfo` 이름): 노드의 CPU 카탈로그(`node cpu` JSON)가 줄 수 있다고 한(`supported`)
    /// 것 가운데 러너 호스트의 `/proc/cpuinfo`에도 있는 앞의 것(리뷰 2 #8 — KVM이 넘기지 않는 기능, 중첩 가상화가
    /// 거르는 AVX-512를 고르지 않게). 없으면 None(시험을 건너뛴다).
    fn pick_flag(node_cpu: &Value) -> Option<(&'static str, &'static str)> {
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let host: Vec<&str> = cpuinfo
            .lines()
            .find(|l| l.starts_with("flags"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, f)| f.split_whitespace().collect())
            .unwrap_or_default();
        let supported = |name: &str| {
            node_cpu["flags"].as_array().is_some_and(|f| {
                f.iter()
                    .any(|x| x["name"] == name && x["supported"] == true)
            })
        };
        [
            ("avx512vbmi", "avx512vbmi"),
            ("avx2", "avx2"),
            ("pcid", "pcid"),
            ("sse4.2", "sse4_2"),
        ]
        .into_iter()
        .find(|(qemu, cpuinfo)| supported(qemu) && host.contains(cpuinfo))
    }

    /// 부팅마다 `/proc/cpuinfo`의 플래그와 boot_id를 보내는 vendor-data(cloud-init per-boot 스크립트, 첫 부팅에도 돈다)
    fn cpu_vendor_data(port: u16) -> String {
        let script = format!(
            r#"#!/usr/bin/env python3
import json, urllib.request
flags = ""
for line in open("/proc/cpuinfo"):
    if line.startswith("flags"):
        flags = line.split(":", 1)[1].strip()
        break
report = {{
    "flags": flags.split(),
    "boot": open("/proc/sys/kernel/random/boot_id").read().strip(),
}}
request = urllib.request.Request("http://10.0.2.2:{port}/report", data=json.dumps(report).encode(),
                                 headers={{"Content-Type": "application/json"}})
urllib.request.urlopen(request, timeout=30)
"#
        );
        let config = json!({
            "write_files": [{
                "path": "/var/lib/cloud/scripts/per-boot/orbi-cpu-report",
                "content": script,
                "permissions": "0755",
            }],
        });
        format!(
            "#cloud-config\n{}\n",
            serde_json::to_string_pretty(&config).unwrap()
        )
    }

    /// 계획 63: 러너 CPU에 있는 플래그를 VM 플래그(`-FLAG`)로 숨겨 클라우드 이미지를 켜면 게스트 `/proc/cpuinfo`에 없다.
    /// 실행 중에 플래그를 지우면 보류 `cpu`로 남고(CPUID는 QEMU를 띄울 때 정해진다), `vm reboot`(전원 껐다 켜기) 뒤 새
    /// 부팅에서 보인다.
    #[tokio::test]
    async fn a_hidden_cpu_flag_is_absent_in_a_real_guest() {
        let image = env_path("OPL_TEST_CLOUD_IMAGE");
        let server = serve(&[("noble.img", &image)]).await;
        let (mut d, _pool) = daemon("cpu-flags").await;
        // 플래그 이름은 노드가 올린 CPU 카탈로그로 본다(첫 검사 회차가 올린다).
        common::eventually("the node CPU catalog", || async {
            d.ctl(&["-o", "json", "node", "cpu", "node1"])
                .await
                .ok()
                .and_then(|out| serde_json::from_str::<Value>(&out).ok())
                .is_some_and(|v| v["flags"].as_array().is_some_and(|f| !f.is_empty()))
        })
        .await;
        let node_cpu = d.ctl_json(&["node", "cpu", "node1"]).await;
        let Some((qemu_name, cpuinfo_name)) = pick_flag(&node_cpu) else {
            eprintln!(
                "skipping: this node's QEMU gives the guest none of avx512vbmi, avx2, pcid, sse4.2: {node_cpu}"
            );
            d.stop();
            return;
        };
        eprintln!("hiding {qemu_name} ({cpuinfo_name} in /proc/cpuinfo)");
        let url = format!("http://127.0.0.1:{}/files/noble.img", server.port);
        d.ctl(&[
            "volume",
            "download",
            "noble-cpu",
            "--pool",
            "local",
            "--url",
            &url,
            "--sha256",
            &sha256(&image),
            "--source-format",
            "qcow2",
        ])
        .await
        .unwrap();
        let vendor = d.path("cpu-vendor-data");
        std::fs::write(&vendor, cpu_vendor_data(server.port)).unwrap();
        let hide = format!("--cpu-flags=-{qemu_name}");
        let started = d
            .ctl(&[
                "vm",
                "create",
                "cpu-probe",
                "--memory",
                "1024",
                "--machine",
                "q35",
                "--disk",
                "noble-cpu",
                "--nic",
                "user",
                &hide,
                "--ci-user",
                "orbi",
                "--ci-vendor-data",
                vendor.to_str().unwrap(),
                "--start",
            ])
            .await;
        if let Err(e) = started {
            diagnose(&d, &["cpu-probe"], Some(&server)).await;
            panic!("vm cpu-probe did not start: {e}");
        }
        let vm = d.ctl_json(&["vm", "get", "cpu-probe"]).await;
        assert_eq!(
            vm["status"]["hardware"]["cpu_resolved"]["flags"],
            json!({ qemu_name: false }),
            "{vm}"
        );
        let has = |report: &Value| {
            report["flags"]
                .as_array()
                .is_some_and(|f| f.iter().any(|x| x == cpuinfo_name))
        };
        let first = wait_report(&d, &server, 1).await;
        assert!(
            first["flags"].as_array().is_some_and(|f| !f.is_empty()),
            "{first}"
        );
        assert!(
            !has(&first),
            "{cpuinfo_name} is visible in the guest: {first}"
        );

        // 실행 중에 지우면 보류로 남는다(도는 QEMU의 CPUID는 그대로).
        d.ctl(&["vm", "set", "cpu-probe", "--no-cpu-flags"])
            .await
            .unwrap();
        let vm = d.ctl_json(&["vm", "get", "cpu-probe"]).await;
        let pending = vm["status"]["hardware"]["pending"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(pending.iter().any(|p| p["field"] == "cpu"), "{vm}");
        d.ctl(&["vm", "reboot", "cpu-probe"]).await.unwrap();
        let second = wait_report(&d, &server, 2).await;
        assert_ne!(second["boot"], first["boot"], "{second}");
        assert!(
            has(&second),
            "{cpuinfo_name} is not back after a power cycle: {second}"
        );

        d.ctl(&["vm", "stop", "cpu-probe", "--force"])
            .await
            .unwrap();
        d.ctl(&["vm", "delete", "cpu-probe"]).await.unwrap();
        d.ctl(&["volume", "delete", "noble-cpu"]).await.unwrap();
        d.stop();
    }
    /// 부팅마다 `/sys/devices/system/node/node*/cpulist`·`meminfo`의 `MemTotal`을 보내는 vendor-data
    fn numa_vendor_data(port: u16) -> String {
        let script = format!(
            r#"#!/usr/bin/env python3
import glob, json, os, urllib.request
nodes = {{}}
for d in sorted(glob.glob("/sys/devices/system/node/node[0-9]*")):
    cpus = open(os.path.join(d, "cpulist")).read().strip()
    total = 0
    for line in open(os.path.join(d, "meminfo")):
        if "MemTotal:" in line:
            total = int(line.split()[-2])
    nodes[os.path.basename(d)] = {{"cpus": cpus, "mem_kib": total}}
request = urllib.request.Request("http://10.0.2.2:{port}/report", data=json.dumps({{"nodes": nodes}}).encode(),
                                 headers={{"Content-Type": "application/json"}})
urllib.request.urlopen(request, timeout=30)
"#
        );
        let config = json!({
            "write_files": [{
                "path": "/var/lib/cloud/scripts/per-boot/orbi-numa-report",
                "content": script,
                "permissions": "0755",
            }],
        });
        format!(
            "#cloud-config\n{}\n",
            serde_json::to_string_pretty(&config).unwrap()
        )
    }

    /// 계획 64: 공유 vNUMA(소켓 둘, 게스트 노드 둘)로 클라우드 이미지를 켜면 게스트가 노드 둘을 보고 소켓마다의 CPU와
    /// 메모리 반씩을 가진다. 기록 `numa`에 모양이 남는다.
    #[tokio::test]
    async fn a_guest_sees_its_numa_nodes() {
        let image = env_path("OPL_TEST_CLOUD_IMAGE");
        let server = serve(&[("noble.img", &image)]).await;
        let (mut d, _pool) = daemon("numa").await;
        let url = format!("http://127.0.0.1:{}/files/noble.img", server.port);
        d.ctl(&[
            "volume",
            "download",
            "noble-numa",
            "--pool",
            "local",
            "--url",
            &url,
            "--sha256",
            &sha256(&image),
            "--source-format",
            "qcow2",
        ])
        .await
        .unwrap();
        let vendor = d.path("numa-vendor-data");
        std::fs::write(&vendor, numa_vendor_data(server.port)).unwrap();
        let started = d
            .ctl(&[
                "vm",
                "create",
                "numa-probe",
                "--memory",
                "2048",
                "--sockets",
                "2",
                "--cores",
                "1",
                "--numa",
                "2",
                "--machine",
                "q35",
                "--disk",
                "noble-numa",
                "--nic",
                "user",
                "--ci-user",
                "orbi",
                "--ci-vendor-data",
                vendor.to_str().unwrap(),
                "--start",
            ])
            .await;
        if let Err(e) = started {
            diagnose(&d, &["numa-probe"], Some(&server)).await;
            panic!("vm numa-probe did not start: {e}");
        }
        let vm = d.ctl_json(&["vm", "get", "numa-probe"]).await;
        assert_eq!(
            vm["status"]["hardware"]["numa"]["nodes"],
            json!([{ "sockets": [0], "memory_mib": 1024 }, { "sockets": [1], "memory_mib": 1024 }]),
            "{vm}"
        );
        let report = wait_report(&d, &server, 1).await;
        let nodes = report["nodes"].as_object().cloned().unwrap_or_default();
        assert_eq!(nodes.len(), 2, "{report}");
        assert_eq!(report["nodes"]["node0"]["cpus"], "0", "{report}");
        assert_eq!(report["nodes"]["node1"]["cpus"], "1", "{report}");
        for n in nodes.values() {
            // 게스트 커널이 쓰는 몫을 빼면 1 GiB보다 조금 적다.
            let kib = n["mem_kib"].as_u64().unwrap();
            assert!((700_000..=1_048_576).contains(&kib), "{report}");
        }
        d.ctl(&["vm", "stop", "numa-probe", "--force"])
            .await
            .unwrap();
        d.ctl(&["vm", "delete", "numa-probe"]).await.unwrap();
        d.ctl(&["volume", "delete", "noble-numa"]).await.unwrap();
        d.stop();
    }
}
