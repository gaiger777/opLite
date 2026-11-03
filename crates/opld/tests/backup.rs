//! 실제 QEMU로 VM 백업(실행 중·꺼진 VM)과 새 VM으로의 복원, 백업 지우기를 데몬 전체를 거쳐 확인한다.
//! dir 풀 둘을 쓴다: VM 디스크용 `local`과 백업 풀 `bk`. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크
//! 내용은 VM을 켜기 전에 써 두고, 복원한 볼륨을 `qemu-img convert`로 raw로 빼서 같은지 본다.
//! `cargo test -p opld --features qemu-it --test backup`
//! QEMU(`qemu-it`와 같다)와 qemu-img, qemu-io가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Daemon;
use oplctl::Client;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 디스크 앞에 써 두는 표식의 길이
const MARK: usize = 64 * 1024;

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

/// 클러스터를 만들고 로그인해 VM 디스크 풀 `local`과 백업 풀 `bk`를 만든다.
async fn setup(d: &mut Daemon) {
    d.start().await;
    d.init("backup").await;
    d.login().await;
    for (name, extra) in [("local", None), ("bk", Some("--backups"))] {
        let dir = d.path(name);
        std::fs::create_dir(&dir).unwrap();
        let mut args = vec!["pool", "create", name, "--path", dir.to_str().unwrap()];
        args.extend(extra);
        d.ctl(&args).await.unwrap();
        d.wait_pool_ready(name).await;
    }
}

/// 풀 `local`에 볼륨 `name`(64 MiB)을 만들고 그 파일 경로를 돌려준다.
async fn create_volume(d: &Daemon, name: &str, format: &str) -> PathBuf {
    d.ctl(&[
        "volume", "create", name, "--pool", "local", "--size", "64M", "--format", format,
    ])
    .await
    .unwrap();
    volume_file(d, name).await
}

/// 볼륨 `name`의 파일(풀 `local`)
async fn volume_file(d: &Daemon, name: &str) -> PathBuf {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let format = volume["spec"]["format"].as_str().unwrap();
    d.path("local")
        .join(format!("{}.{format}", volume["id"].as_str().unwrap()))
}

/// qcow2 파일 앞에 `byte`를 `MARK`만큼 쓴다.
fn mark_qcow2(file: &Path, byte: u8) {
    let ok = Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", &format!("write -P {byte:#x} 0 {MARK}")])
        .arg(file)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success();
    assert!(ok, "qemu-io write to {}", file.display());
}

/// raw 파일 앞에 `byte`를 `MARK`만큼 쓴다.
fn mark_raw(file: &Path, byte: u8) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).open(file).unwrap();
    f.write_all(&[byte; MARK]).unwrap();
}

/// 이미지(`qemu-img`가 형식을 알아본다)의 앞 `MARK` 바이트. raw로 빼서 읽는다. 실행 중인 VM이 쥔 파일도
/// 읽는다(`-U`).
fn head(d: &Daemon, file: &Path) -> Vec<u8> {
    let raw = d.path("head.raw");
    let out = Command::new("qemu-img")
        .args(["convert", "-U", "-O", "raw"])
        .arg(file)
        .arg(&raw)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    let bytes = std::fs::read(&raw).unwrap();
    std::fs::remove_file(&raw).unwrap();
    bytes[..MARK].to_vec()
}

fn qemu_img_info(file: &Path) -> Value {
    let out = Command::new("qemu-img")
        .args(["info", "-U", "--output=json"])
        .arg(file)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// 풀 `bk` 안의 백업 디렉터리
fn backup_dir(d: &Daemon, backup: &Value) -> PathBuf {
    d.path("bk")
        .join("backups")
        .join(backup["id"].as_str().unwrap())
}

/// 디렉터리의 파일 이름(정렬)
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// VM의 UEFI 변수 파일(BIOS VM이면 없다)
fn vars_file(d: &Daemon, vm: &Value) -> PathBuf {
    d.data_dir()
        .join("vm")
        .join(vm["id"].as_str().unwrap())
        .join("uefi-vars.fd")
}

#[tokio::test]
async fn running_vm_backup_restores_to_a_new_vm() {
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    setup(&mut d).await;
    let c = client(&d);
    let root = create_volume(&d, "root", "qcow2").await;
    mark_qcow2(&root, 0xab);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--nic", "user", "--start",
    ])
    .await
    .unwrap();
    let web = c.get("/vms/web").await.unwrap();
    let pid = web["status"]["pid"].as_u64().unwrap();

    // 실행 중에 백업한다. 이름을 주지 않으면 `{VM 이름}-{UTC 시각}`이다.
    let out = d
        .ctl(&["backup", "create", "web", "--pool", "bk"])
        .await
        .unwrap();
    assert!(
        out.starts_with("backup web-") && out.ends_with(" of vm web created\n"),
        "{out}"
    );
    let list = d.ctl_json(&["backup", "list", "--vm", "web"]).await;
    let backups = list.as_array().unwrap();
    assert_eq!(backups.len(), 1, "{list}");
    let backup = backups[0].clone();
    let name = backup["name"].as_str().unwrap().to_string();
    assert_eq!(backup["status"]["phase"], "ready", "{backup}");
    assert_eq!(backup["spec"]["vm"], web["id"], "{backup}");
    assert_eq!(backup["spec"]["vm_name"], "web", "{backup}");
    assert_eq!(backup["spec"]["pool"], "bk", "{backup}");
    assert_eq!(backup["status"]["disks"][0]["name"], "disk0", "{backup}");
    assert_eq!(
        backup["status"]["disks"][0]["virtual_bytes"],
        64u64 << 20,
        "{backup}"
    );
    // VM은 그대로 돌고, 임시 스냅샷은 지워 디스크에 태그가 없다.
    let v = c.get("/vms/web").await.unwrap();
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["pid"].as_u64(), Some(pid), "{v}");
    assert!(
        qemu_img_info(&root)["snapshots"].is_null(),
        "temporary snapshot left"
    );

    // 백업 디렉터리: 압축 qcow2 디스크, 매니페스트, UEFI 변수(있으면).
    let dir = backup_dir(&d, &backup);
    let uefi = backup["status"]["uefi_vars"] == true;
    let mut expected = vec!["disk0.qcow2", "manifest.json"];
    if uefi {
        expected.push("vars.fd");
    }
    assert_eq!(files(&dir), expected);
    let disk = dir.join("disk0.qcow2");
    let info = qemu_img_info(&disk);
    assert_eq!(info["format"], "qcow2", "{info}");
    assert!(info["backing-filename"].is_null(), "{info}");
    assert_eq!(
        backup["status"]["disks"][0]["stored_bytes"],
        std::fs::metadata(&disk).unwrap().len(),
        "{backup}"
    );
    assert_eq!(head(&d, &disk), vec![0xab; MARK]);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], 1, "{manifest}");
    assert_eq!(manifest["backup"], backup["id"], "{manifest}");
    assert_eq!(manifest["vm_name"], "web", "{manifest}");
    assert_eq!(
        manifest["vm_spec"]["disks"][0]["volume"], "root",
        "{manifest}"
    );

    // 원본 VM과 디스크를 지워도 백업은 남는다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    let out = d.ctl(&["backup", "list"]).await.unwrap();
    assert!(out.lines().nth(1).unwrap().starts_with(&name), "{out}");

    // 새 VM으로 복원한다: 디스크는 `{새 이름}-disk{i}`, 꺼진 채, 노드 없음, 새 MAC.
    let out = d
        .ctl(&[
            "backup", "restore", &name, "--name", "copy", "--pool", "local",
        ])
        .await
        .unwrap();
    assert_eq!(out, format!("vm copy restored from backup {name}\n"));
    let copy = c.get("/vms/copy").await.unwrap();
    assert_ne!(copy["id"], web["id"], "{copy}");
    assert_eq!(copy["spec"]["disks"][0]["volume"], "copy-disk0", "{copy}");
    assert_eq!(copy["spec"]["memory_mib"], 256, "{copy}");
    assert_eq!(copy["spec"]["power"], "stopped", "{copy}");
    assert_eq!(copy["spec"]["node"], Value::Null, "{copy}");
    let mac = &copy["spec"]["nics"][0]["mac"];
    assert!(mac.is_string(), "{copy}");
    assert_ne!(mac, &web["spec"]["nics"][0]["mac"], "{copy} {web}");
    let copy_file = volume_file(&d, "copy-disk0").await;
    assert_eq!(head(&d, &copy_file), vec![0xab; MARK]);
    assert!(qemu_img_info(&copy_file)["snapshots"].is_null());
    // UEFI 변수는 백업 때의 사본을 받는다.
    let copy_vars = vars_file(&d, &copy);
    assert_eq!(copy_vars.exists(), uefi);
    if uefi {
        assert_eq!(
            std::fs::read(dir.join("vars.fd")).unwrap(),
            std::fs::read(&copy_vars).unwrap()
        );
    }

    // 복원한 VM이 켜진다.
    let out = d.ctl(&["vm", "start", "copy"]).await.unwrap();
    assert!(out.contains("vm copy: start done"), "{out}");
    let copy = c.get("/vms/copy").await.unwrap();
    assert_eq!(copy["status"]["phase"], "running", "{copy}");
    d.ctl(&["vm", "stop", "copy", "--force"]).await.unwrap();

    // 백업을 지우면 디렉터리가 사라진다.
    let out = d.ctl(&["backup", "delete", &name]).await.unwrap();
    assert_eq!(out, format!("backup {name} deleted\n"));
    assert!(!dir.exists());
    assert_eq!(d.ctl_json(&["backup", "list"]).await, json!([]));

    d.ctl(&["vm", "delete", "copy"]).await.unwrap();
    d.ctl(&["volume", "delete", "copy-disk0"]).await.unwrap();
    d.stop();
}

#[tokio::test]
async fn stopped_vm_backup_with_raw_disk_restores_both_disks() {
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    setup(&mut d).await;
    let c = client(&d);
    let root = create_volume(&d, "root", "qcow2").await;
    let data = create_volume(&d, "data", "raw").await;
    mark_qcow2(&root, 0xab);
    mark_raw(&data, 0xcd);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--disk", "data", "--start",
    ])
    .await
    .unwrap();

    // dir 풀의 raw 디스크는 스냅샷을 찍을 수 없으므로 실행 중에는 백업하지 않는다.
    let resp = c
        .request(reqwest::Method::POST, "/vms/web/backups")
        .json(&json!({ "pool": "bk" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "vm_running", "{body}");
    assert_eq!(d.ctl_json(&["backup", "list"]).await, json!([]));

    // 꺼진 VM은 된다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let out = d
        .ctl(&[
            "backup", "create", "web", "--pool", "bk", "--name", "manual",
        ])
        .await
        .unwrap();
    assert_eq!(out, "backup manual of vm web created\n");
    let backup = c.get("/backups/manual").await.unwrap();
    assert_eq!(backup["status"]["phase"], "ready", "{backup}");
    let dir = backup_dir(&d, &backup);
    assert_eq!(head(&d, &dir.join("disk0.qcow2")), vec![0xab; MARK]);
    assert_eq!(head(&d, &dir.join("disk1.qcow2")), vec![0xcd; MARK]);

    // 백업이 남은 풀은 지울 수 없다.
    let err = d.ctl(&["pool", "delete", "bk"]).await.unwrap_err();
    assert!(err.to_string().contains("holds backups"), "{err}");

    // 원본 디스크를 바꿔도 복원한 디스크는 백업 때의 내용이다. 대상 풀(dir)은 qcow2를 받는다.
    mark_qcow2(&root, 0x11);
    mark_raw(&data, 0x22);
    d.ctl(&[
        "backup", "restore", "manual", "--name", "copy", "--pool", "local",
    ])
    .await
    .unwrap();
    let copy = c.get("/vms/copy").await.unwrap();
    let volumes: Vec<&str> = copy["spec"]["disks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["volume"].as_str().unwrap())
        .collect();
    assert_eq!(volumes, ["copy-disk0", "copy-disk1"], "{copy}");
    for (volume, byte) in [("copy-disk0", 0xab), ("copy-disk1", 0xcd)] {
        let v = d.ctl_json(&["volume", "get", volume]).await;
        assert_eq!(v["spec"]["format"], "qcow2", "{v}");
        assert_eq!(v["spec"]["size_bytes"], 64u64 << 20, "{v}");
        assert_eq!(head(&d, &volume_file(&d, volume).await), vec![byte; MARK]);
    }
    let out = d.ctl(&["vm", "start", "copy"]).await.unwrap();
    assert!(out.contains("vm copy: start done"), "{out}");
    d.ctl(&["vm", "stop", "copy", "--force"]).await.unwrap();

    d.ctl(&["backup", "delete", "manual"]).await.unwrap();
    assert!(!dir.exists());
    d.ctl(&["pool", "set", "bk", "--backups", "false"])
        .await
        .unwrap();
    for vm in ["copy", "web"] {
        d.ctl(&["vm", "delete", vm]).await.unwrap();
    }
    for volume in ["copy-disk0", "copy-disk1", "root", "data"] {
        d.ctl(&["volume", "delete", volume]).await.unwrap();
    }
    d.stop();
}
