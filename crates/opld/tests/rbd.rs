//! 실제 Ceph RBD 풀: opld가 관리자가 만든 Ceph 풀을 확인해 준비됐다고 알리고, 그 풀의 raw 볼륨(RBD
//! 이미지)을 QEMU `rbd` 블록 드라이버로 붙인 VM을 띄운다. 실행 중에 디스크 스냅샷(RBD 스냅샷)을 찍고,
//! 멈춘 뒤 더 최근 스냅샷이 있어도 앞 스냅샷으로 되돌리고, 스냅샷에서 디렉터리 풀로 복제하고, 모두 지운다.
//! 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 꺼진 VM의 이미지를 `qemu-io`로 본다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_RBD=rbd cargo test -p opld --features rbd-it --test rbd`
//! Linux에서 root, ceph-common, qemu-block-extra, QEMU(`qemu-it`와 같다), qemu-img, qemu-io와
//! `client.admin`으로 접속할 수 있는 `/etc/ceph/ceph.conf`·키링이 필요하다. `OPL_TEST_RBD`는 테스트가
//! 이미지를 만들고 지워도 되는 Ceph 풀(`rbd pool init`을 마친 것)이다.
#![cfg(all(feature = "rbd-it", target_os = "linux"))]

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

/// `rbd` 명령을 실행하고 표준 출력을 돌려준다.
fn rbd(args: &[&str]) -> Vec<u8> {
    let out = Command::new("rbd").args(args).output().expect("run rbd");
    assert!(out.status.success(), "rbd {args:?}: {out:?}");
    out.stdout
}

/// Ceph 풀의 이미지와 스냅샷(`image@tag`) 이름 중 `prefix`로 시작하는 것
fn rbd_names(pool: &str, prefix: &str) -> Vec<String> {
    let images: Vec<String> =
        serde_json::from_slice(&rbd(&["ls", "--format", "json", "--pool", pool])).unwrap();
    let mut names = vec![];
    for name in images.into_iter().filter(|n| n.starts_with(prefix)) {
        let spec = format!("{pool}/{name}");
        let snaps: Value =
            serde_json::from_slice(&rbd(&["snap", "ls", "--format", "json", &spec])).unwrap();
        for s in snaps.as_array().unwrap() {
            names.push(format!("{name}@{}", s["name"].as_str().unwrap()));
        }
        names.push(name);
    }
    names.sort();
    names
}

/// 이미지에 `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io_rbd(pool: &str, image: &str, cmd: &str) -> bool {
    let opts = format!("driver=raw,file.driver=rbd,file.pool={pool},file.image={image}");
    Command::new("qemu-io")
        .args(["--image-opts", "-c", cmd, &opts])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

/// 파일 볼륨에 `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다.
fn qemu_io_file(path: &Path, format: &str, cmd: &str) -> bool {
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
async fn rbd_pool_runs_snapshots_reverts_and_clones_a_vm() {
    let pool = std::env::var("OPL_TEST_RBD").expect("OPL_TEST_RBD must name a Ceph pool");
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("rbd").await;
    d.login().await;
    let c = client(&d);

    // 마운터가 Ceph 풀과 QEMU의 rbd 드라이버를 확인하면 준비됐다고 알리고 풀의 용량을 보인다.
    // RBD 풀은 늘 공유다.
    let backend = json!({ "kind": "rbd", "pool": pool });
    let created = c
        .post(
            "/storage-pools",
            &json!({ "name": "ceph", "spec": { "backend": backend } }),
        )
        .await
        .unwrap();
    assert_eq!(created["spec"]["shared"], true, "{created}");
    d.wait_pool_ready("ceph").await;
    let node = c.get("/nodes/node1").await.unwrap();
    let state = &node["status"]["pools"]["ceph"];
    assert!(state["total_bytes"].as_u64().unwrap() > 0, "{node}");
    assert_eq!(state["error"], Value::Null, "{node}");

    // 볼륨은 RBD 이미지 `opl-{id}`다.
    d.ctl(&[
        "volume", "create", "root", "--pool", "ceph", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let image = format!("opl-{}", volume["id"].as_str().unwrap());
    assert_eq!(rbd_names(&pool, &image), vec![image.clone()]);
    assert!(qemu_io_rbd(&pool, &image, "write -P 0xab 0 64k"));

    // VM은 이미지를 QEMU의 rbd 드라이버로 연다(커널 매핑 없음).
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
        args.contains(&format!(
            "driver=rbd,pool={pool},image={image},user=admin,node-name=disk0-file"
        )),
        "{args}"
    );

    // RBD 디스크는 메모리를 담을 수 없다.
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
    let before_snapshot = format!("{image}@{}", before["id"].as_str().unwrap());
    assert_eq!(
        rbd_names(&pool, &image),
        vec![image.clone(), before_snapshot.clone()]
    );
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    assert_eq!(web["status"]["pid"].as_u64(), Some(pid), "{web}");

    // 멈춘 뒤 디스크를 바꾸고 스냅샷을 하나 더 찍어도 앞 스냅샷으로 되돌릴 수 있다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io_rbd(&pool, &image, "write -P 0xcd 0 64k"));
    d.ctl(&["vm", "snapshot", "create", "web", "after"])
        .await
        .unwrap();
    let after = c.get("/vms/web/snapshots/after").await.unwrap();
    let after_snapshot = format!("{image}@{}", after["id"].as_str().unwrap());
    assert!(qemu_io_rbd(&pool, &image, "write -P 0x5a 0 64k"));
    let out = d
        .ctl(&["vm", "snapshot", "revert", "web", "before"])
        .await
        .unwrap();
    assert!(out.contains("vm web: reverted to snapshot before"), "{out}");
    assert!(qemu_io_rbd(&pool, &image, "read -P 0xab 0 64k"));
    let mut both = vec![image.clone(), before_snapshot.clone(), after_snapshot];
    both.sort();
    assert_eq!(rbd_names(&pool, &image), both);
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "stopped", "{web}");

    // 더 최근 스냅샷으로도 되돌아갈 수 있다.
    d.ctl(&["vm", "snapshot", "revert", "web", "after"])
        .await
        .unwrap();
    assert!(qemu_io_rbd(&pool, &image, "read -P 0xcd 0 64k"));
    d.ctl(&["vm", "snapshot", "delete", "web", "after"])
        .await
        .unwrap();
    assert_eq!(
        rbd_names(&pool, &image),
        vec![image.clone(), before_snapshot.clone()]
    );

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
    assert!(qemu_io_file(&copy_file, "qcow2", "read -P 0xab 0 64k"));

    // 스냅샷과 함께 VM을 지우면 RBD 스냅샷이, 볼륨을 지우면 이미지가 없어진다.
    d.ctl(&["vm", "delete", "copy"]).await.unwrap();
    d.ctl(&["volume", "delete", "copy-root"]).await.unwrap();
    assert!(!copy_file.exists());
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert_eq!(rbd_names(&pool, &image), vec![image.clone()]);
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(rbd_names(&pool, &image).is_empty());
    d.ctl(&["pool", "delete", "local"]).await.unwrap();
    d.ctl(&["pool", "delete", "ceph"]).await.unwrap();
    d.stop();
}
