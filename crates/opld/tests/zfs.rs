//! 실제 ZFS 풀: opld가 관리자가 만든 데이터셋을 확인해 준비됐다고 알리고, 그 풀의 raw 볼륨(zvol)을
//! 블록 장치로 붙인 VM을 띄운다. 실행 중에 디스크 스냅샷(ZFS 스냅샷)을 찍고, 멈춘 뒤 가장 최근
//! 스냅샷으로만 되돌리고, 스냅샷에서 디렉터리 풀로 복제하고, 모두 지운다.
//! 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 꺼진 VM의 zvol 장치를 `qemu-io`로 본다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_ZFS=opltestz/opl cargo test -p opld --features zfs-it --test zfs`
//! Linux에서 root, zfsutils-linux, QEMU(`qemu-it`와 같다), qemu-img, qemu-io가 필요하다.
//! `OPL_TEST_ZFS`는 테스트가 zvol을 만들고 지워도 되는 데이터셋이다.
#![cfg(all(feature = "zfs-it", target_os = "linux"))]

mod common;

use std::path::Path;
use std::process::Command;

use common::Daemon;
use oplctl::Client;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

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

/// Task를 돌려주는 요청을 보내고 Task가 성공할 때까지 기다린다.
async fn run(c: &Client, path: &str, body: Value) {
    let task = c.post(path, &body).await.unwrap();
    c.wait_task(&task).await.unwrap();
}

/// 데이터셋 아래 zvol과 스냅샷 이름(데이터셋 부분을 뺀 것) 중 `prefix`로 시작하는 것
fn zfs_names(dataset: &str, prefix: &str) -> Vec<String> {
    let out = Command::new("zfs")
        .args(["list", "-H", "-o", "name", "-t", "volume,snapshot", "-r"])
        .arg(dataset)
        .output()
        .expect("run zfs list");
    assert!(out.status.success(), "{out:?}");
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().strip_prefix(&format!("{dataset}/")))
        .filter(|l| l.starts_with(prefix))
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

/// QEMU 명령줄(`/proc/{pid}/cmdline`)
fn cmdline(pid: u64) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap()
        .split(|b| *b == 0)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[tokio::test]
async fn zfs_pool_runs_snapshots_reverts_and_clones_a_vm() {
    let dataset = std::env::var("OPL_TEST_ZFS").expect("OPL_TEST_ZFS must name POOL/DATASET");
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("zfs").await;
    d.login().await;
    let c = client(&d);

    // 마운터가 데이터셋을 확인하면 준비됐다고 알리고 데이터셋의 용량을 보인다.
    let backend = json!({ "kind": "zfs", "dataset": dataset });
    c.post(
        "/storage-pools",
        &json!({ "name": "zfs", "spec": { "backend": backend } }),
    )
    .await
    .unwrap();
    d.wait_pool_ready("zfs").await;
    let node = c.get("/nodes/node1").await.unwrap();
    let state = &node["status"]["pools"]["zfs"];
    assert!(state["total_bytes"].as_u64().unwrap() > 0, "{node}");
    assert_eq!(state["error"], Value::Null, "{node}");

    // 볼륨은 zvol `opl-{id}`이고 장치 노드로 쓴다.
    d.ctl(&[
        "volume", "create", "root", "--pool", "zfs", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let zvol = format!("opl-{}", volume["id"].as_str().unwrap());
    let dev = Path::new("/dev/zvol").join(&dataset).join(&zvol);
    assert!(dev.exists(), "{}", dev.display());
    assert!(qemu_io(&dev, "raw", "write -P 0xab 0 64k"));

    // VM은 zvol을 host_device로 연다.
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    let pid = web["status"]["pid"].as_u64().unwrap();
    let args = cmdline(pid);
    assert!(
        args.contains(&format!("driver=host_device,filename={}", dev.display())),
        "{args}"
    );

    // ZFS 디스크는 메모리를 담을 수 없다.
    let resp = c
        .request(Method::POST, "/vms/web/snapshots")
        .json(&json!({ "name": "mem", "memory": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "snapshot_unsupported", "{body}");

    // 실행 중 스냅샷은 디스크만 담는다. VM은 잠깐 멈췄다가 같은 프로세스로 계속 돈다.
    run(&c, "/vms/web/snapshots", json!({ "name": "before" })).await;
    let before = c.get("/vms/web/snapshots/before").await.unwrap();
    assert_eq!(before["status"]["phase"], "ready", "{before}");
    assert_eq!(before["spec"]["memory"], false, "{before}");
    let before_snapshot = format!("{zvol}@{}", before["id"].as_str().unwrap());
    assert_eq!(
        zfs_names(&dataset, &zvol),
        vec![zvol.clone(), before_snapshot.clone()]
    );
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    assert_eq!(web["status"]["pid"].as_u64(), Some(pid), "{web}");

    // 멈춘 뒤 디스크를 바꾸고 스냅샷을 하나 더 찍으면 앞 스냅샷으로는 되돌릴 수 없다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&dev, "raw", "write -P 0xcd 0 64k"));
    d.ctl(&["vm", "snapshot", "create", "web", "after"])
        .await
        .unwrap();
    let resp = c
        .request(Method::POST, "/vms/web/snapshots/before/revert")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "snapshot_unsupported", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("latest snapshot"),
        "{body}"
    );
    assert!(qemu_io(&dev, "raw", "read -P 0xcd 0 64k"));

    // 최근 스냅샷을 지우면 앞 스냅샷이 가장 최근이라 되돌릴 수 있고, 그때 내용이 같은 장치에 돌아온다.
    d.ctl(&["vm", "snapshot", "delete", "web", "after"])
        .await
        .unwrap();
    assert_eq!(
        zfs_names(&dataset, &zvol),
        vec![zvol.clone(), before_snapshot.clone()]
    );
    let out = d
        .ctl(&["vm", "snapshot", "revert", "web", "before"])
        .await
        .unwrap();
    assert!(out.contains("vm web: reverted to snapshot before"), "{out}");
    assert!(qemu_io(&dev, "raw", "read -P 0xab 0 64k"));
    assert_eq!(
        zfs_names(&dataset, &zvol),
        vec![zvol.clone(), before_snapshot.clone()]
    );
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "stopped", "{web}");

    // 되돌린 디스크로 다시 켜진다.
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();

    // 스냅샷에서 디렉터리 풀로 복제하면 새 디스크는 qcow2 파일이고 스냅샷 때 내용을 담는다.
    let dir = d.path("pool");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    assert!(qemu_io(&dev, "raw", "write -P 0xcd 0 64k"));
    d.ctl(&[
        "vm",
        "clone",
        "web",
        "copy",
        "--snapshot",
        "before",
        "--pool",
        "local",
    ])
    .await
    .unwrap();
    let copy_volume = c.get("/volumes/copy-root").await.unwrap();
    assert_eq!(copy_volume["status"]["phase"], "ready", "{copy_volume}");
    assert_eq!(copy_volume["spec"]["pool"], "local", "{copy_volume}");
    assert_eq!(copy_volume["spec"]["format"], "qcow2", "{copy_volume}");
    let copy_file = dir.join(format!("{}.qcow2", copy_volume["id"].as_str().unwrap()));
    assert!(qemu_io(&copy_file, "qcow2", "read -P 0xab 0 64k"));

    // 스냅샷과 함께 VM을 지우면 ZFS 스냅샷이, 볼륨을 지우면 zvol이 없어진다.
    d.ctl(&["vm", "delete", "copy"]).await.unwrap();
    d.ctl(&["volume", "delete", "copy-root"]).await.unwrap();
    assert!(!copy_file.exists());
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert_eq!(zfs_names(&dataset, &zvol), vec![zvol.clone()]);
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(zfs_names(&dataset, &zvol).is_empty());
    d.ctl(&["pool", "delete", "local"]).await.unwrap();
    d.ctl(&["pool", "delete", "zfs"]).await.unwrap();
    d.stop();
}
