//! 실제 QEMU로 VM 스냅샷(만들기·되돌리기·삭제)과 볼륨 크기 늘리기를 데몬 전체를 거쳐 확인한다.
//! 꺼진 VM은 oplctl로, 실행 중인 VM은 응답을 자세히 보려고 API를 직접 부른다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용이
//! 돌아오는지는 정지한 VM에서 `qemu-io`로 본다.
//! `cargo test -p opld --features qemu-it --test snapshots`
//! QEMU(`qemu-it`와 같다)와 qemu-img, qemu-io가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Daemon;
use oplctl::Client;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const MIB: u64 = 1 << 20;

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

/// 클러스터를 만들고 로그인해 풀 `local`과 qcow2 볼륨 `root`(64 MiB)를 만든다. 볼륨 파일 경로를 돌려준다.
async fn setup(d: &mut Daemon) -> PathBuf {
    d.start().await;
    d.init("snap").await;
    d.login().await;
    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M",
    ])
    .await
    .unwrap();
    let volume = d.ctl_json(&["volume", "get", "root"]).await;
    pool.join(format!("{}.qcow2", volume["id"].as_str().unwrap()))
}

/// Task를 돌려주는 요청을 보내고 Task가 성공할 때까지 기다린다.
async fn run(c: &Client, path: &str, body: Value) {
    let task = c.post(path, &body).await.unwrap();
    c.wait_task(&task).await.unwrap();
}

async fn delete_snapshot(c: &Client, name: &str) {
    let task = c
        .delete(&format!("/vms/web/snapshots/{name}"))
        .await
        .unwrap();
    c.wait_task(&task).await.unwrap();
}

/// 스냅샷 이름 → 리소스
async fn snapshot(c: &Client, name: &str) -> Value {
    c.get(&format!("/vms/web/snapshots/{name}")).await.unwrap()
}

async fn vm(c: &Client) -> Value {
    c.get("/vms/web").await.unwrap()
}

/// 볼륨 파일에 든 스냅샷 태그. 실행 중인 VM이 쥔 파일도 읽는다(`-U`).
fn tags(file: &Path) -> Vec<String> {
    let info = qemu_img_info(file);
    let mut tags: Vec<String> = info["snapshots"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    tags.sort();
    tags
}

/// 태그에 담긴 메모리 상태의 크기(디스크만 담았으면 0)
fn vm_state_size(file: &Path, tag: &str) -> u64 {
    let info = qemu_img_info(file);
    let snapshot = info["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == tag)
        .unwrap_or_else(|| panic!("no tag {tag}: {info}"))
        .clone();
    snapshot["vm-state-size"].as_u64().unwrap()
}

/// QEMU가 디스크 헤더에 쓴 크기
fn virtual_size(file: &Path) -> u64 {
    qemu_img_info(file)["virtual-size"].as_u64().unwrap()
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

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(file: &Path, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", cmd])
        .arg(file)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn running_vm_snapshots_revert_resize_and_delete() {
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    let file = setup(&mut d).await;
    let c = client(&d);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    let pid = vm(&c).await["status"]["pid"].as_u64().unwrap();

    // 실행 중이면 기본으로 메모리까지 담는다. VM은 멈췄다가 계속 돈다.
    run(&c, "/vms/web/snapshots", json!({ "name": "mem" })).await;
    let mem = snapshot(&c, "mem").await;
    let mem_id = mem["id"].as_str().unwrap().to_string();
    assert_eq!(mem["status"]["phase"], "ready", "{mem}");
    assert_eq!(mem["spec"]["memory"], true, "{mem}");
    assert_eq!(mem["spec"]["parent"], Value::Null, "{mem}");
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["pid"].as_u64(), Some(pid), "{v}");
    assert_eq!(v["status"]["snapshot"], mem_id.as_str(), "{v}");
    assert_eq!(tags(&file), vec![mem_id.clone()]);
    assert!(vm_state_size(&file, &mem_id) > 0);

    // 실행 중 크기 늘리기: QEMU가 늘리고(헤더의 크기), 스펙도 그 크기를 받는다.
    let out = d
        .ctl(&["volume", "resize", "root", "--size", "128M"])
        .await
        .unwrap();
    assert!(out.contains("volume root resized to 128.0MiB"), "{out}");
    assert_eq!(virtual_size(&file), 128 * MIB);
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["spec"]["size_bytes"], 128 * MIB, "{volume}");
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["pid"].as_u64(), Some(pid), "{v}");

    // 디스크만 담는 스냅샷은 지금 위치(mem)를 부모로 한다.
    run(
        &c,
        "/vms/web/snapshots",
        json!({ "name": "disk", "memory": false }),
    )
    .await;
    let disk = snapshot(&c, "disk").await;
    let disk_id = disk["id"].as_str().unwrap().to_string();
    assert_eq!(disk["spec"]["memory"], false, "{disk}");
    assert_eq!(disk["spec"]["parent"], mem_id.as_str(), "{disk}");
    assert_eq!(vm(&c).await["status"]["snapshot"], disk_id.as_str());
    assert_eq!(tags(&file), sorted(vec![mem_id.clone(), disk_id.clone()]));
    assert_eq!(vm_state_size(&file, &disk_id), 0);
    let list = c.get("/vms/web/snapshots").await.unwrap();
    let mut names: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["spec"]["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["disk", "mem"], "{list}");

    // 메모리 스냅샷으로 되돌리면 그 메모리로 새로 시작해 실행 상태로 돌아온다. 크기도 그때로 돌아간다.
    run(&c, "/vms/web/snapshots/mem/revert", json!({})).await;
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_ne!(v["status"]["pid"].as_u64(), Some(pid), "{v}");
    assert_eq!(v["status"]["snapshot"], mem_id.as_str(), "{v}");
    assert_eq!(virtual_size(&file), 64 * MIB);
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["spec"]["size_bytes"], 64 * MIB, "{volume}");

    // 디스크만 담은 스냅샷으로 되돌리면 꺼진 채 남는다.
    run(&c, "/vms/web/snapshots/disk/revert", json!({})).await;
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "stopped", "{v}");
    assert_eq!(v["status"]["snapshot"], disk_id.as_str(), "{v}");
    assert_eq!(virtual_size(&file), 128 * MIB);

    // 스냅샷이 남은 VM은 지울 수 없다.
    let resp = c.request(Method::DELETE, "/vms/web").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "vm_has_snapshots", "{body}");

    // 실행 중에 지우면 QEMU가 디스크의 태그를 지우고, 지금 위치는 부모로 옮긴다.
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    // 실행 중인 VM은 지울 수 없으므로 스냅샷도 하나도 지우지 않는다.
    let err = d
        .ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("is not stopped"), "{err}");
    assert_eq!(tags(&file), sorted(vec![mem_id.clone(), disk_id.clone()]));
    delete_snapshot(&c, "disk").await;
    assert_eq!(tags(&file), vec![mem_id.clone()]);
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["snapshot"], mem_id.as_str(), "{v}");

    // 꺼진 뒤에 지우면 스토리지가 지운다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    delete_snapshot(&c, "mem").await;
    assert!(tags(&file).is_empty());
    assert_eq!(vm(&c).await["status"]["snapshot"], Value::Null);
    let list = c.get("/vms/web/snapshots").await.unwrap();
    assert_eq!(list["items"], json!([]), "{list}");

    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}

#[tokio::test]
async fn stopped_vm_snapshot_restores_disk_content() {
    let mut d = Daemon::new("node1", "");
    let file = setup(&mut d).await;
    let c = client(&d);
    d.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();

    assert!(qemu_io(&file, "write -P 0xab 0 64k"));
    // 꺼진 VM은 기본으로 디스크만 담는다.
    let out = d
        .ctl(&[
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--description",
            "clean",
        ])
        .await
        .unwrap();
    assert!(out.contains("snapshot before of vm web created"), "{out}");
    let before = snapshot(&c, "before").await;
    let id = before["id"].as_str().unwrap().to_string();
    assert_eq!(before["status"]["phase"], "ready", "{before}");
    assert_eq!(before["spec"]["memory"], false, "{before}");
    assert_eq!(tags(&file), vec![id.clone()]);

    assert!(qemu_io(&file, "write -P 0xcd 0 64k"));
    assert!(!qemu_io(&file, "read -P 0xab 0 64k"));
    let out = d
        .ctl(&["vm", "snapshot", "revert", "web", "before"])
        .await
        .unwrap();
    assert!(out.contains("vm web: reverted to snapshot before"), "{out}");
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "stopped", "{v}");
    assert_eq!(v["status"]["snapshot"], id.as_str(), "{v}");

    // 두 번째 스냅샷은 지금 위치(before) 아래에 붙고 현재 위치가 된다.
    d.ctl(&["vm", "snapshot", "create", "web", "after"])
        .await
        .unwrap();
    let out = d.ctl(&["vm", "snapshot", "list", "web"]).await.unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(
        lines[1].starts_with("   before ") && lines[1].ends_with("clean"),
        "{out}"
    );
    assert!(lines[2].starts_with("*  └─ after "), "{out}");

    let resp = c.request(Method::DELETE, "/vms/web").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    // 스냅샷을 하나씩 지운 뒤 VM을 지운다. 지운 뒤에도 디스크 내용은 그대로다.
    let out = d
        .ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert!(out.contains("snapshot before of vm web deleted"), "{out}");
    assert!(out.contains("snapshot after of vm web deleted"), "{out}");
    assert!(out.contains("vm web deleted"), "{out}");
    assert!(tags(&file).is_empty());
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}
