//! 실제 공유 LVM 풀: opld가 iSCSI 타깃에 로그인하고(있으면) 공유 VG의 잠금을 시작해 준비됐다고 알리고,
//! 그 풀의 raw 볼륨(두꺼운 LV)을 블록 장치로 붙인 VM을 띄운다. VM이 도는 동안에만 LV가 이 노드에서
//! 켜져 있고, opld가 다시 뜰 때 켜진 채 남은 LV는 끄되 실행 중인 VM의 LV는 그대로 둔다. 디렉터리
//! 풀로 복제하고 모두 지운다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 LV 장치를 직접 본다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_LVM_SHARED=oplshared OPL_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared cargo test -p opld --features lvm-shared-it --test lvm_shared`
//! Linux에서 root, lvm2-lockd와 sanlock(또는 dlm), QEMU(`qemu-it`와 같다), qemu-img, qemu-io가 필요하다.
//! `OPL_TEST_LVM_SHARED`는 테스트가 LV를 만들고 지워도 되는 공유 VG다. `OPL_TEST_ISCSI`가 있으면 VG가
//! 그 타깃의 LUN 위에 있다고 보고, 시험 전에 VG 잠금을 멈추고 로그아웃해 opld가 다시 로그인하게 한다.
#![cfg(all(feature = "lvm-shared-it", target_os = "linux"))]

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::Daemon;
use oplctl::Client;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// 잠금 시작(sanlock 임대)과 iSCSI 로그인을 기다리는 시간
const WAIT: Duration = Duration::from_secs(180);

/// `OPL_TEST_LVM_SHARED`의 공유 VG
fn vg() -> String {
    std::env::var("OPL_TEST_LVM_SHARED").expect("OPL_TEST_LVM_SHARED must name a shared VG")
}

/// `OPL_TEST_ISCSI`의 포털과 타깃
fn iscsi() -> Option<(String, String)> {
    let value = std::env::var("OPL_TEST_ISCSI").ok()?;
    let (portal, target) = value
        .split_once(',')
        .expect("OPL_TEST_ISCSI must look like PORTAL,TARGET");
    Some((portal.to_string(), target.to_string()))
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

/// 명령을 실행해 결과를 돌려준다(실패해도).
fn output(program: &str, args: &[&str]) -> std::process::Output {
    Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .env("LVM_SUPPRESS_FD_WARNINGS", "1")
        .output()
        .unwrap_or_else(|e| panic!("run {program}: {e}"))
}

/// 실패한 확인을 이해하도록 iSCSI 세션, VG와 그 LV, lvmlockd의 상태를 보인다.
fn state() -> String {
    let vg = vg();
    let mut text = String::new();
    for (program, args) in [
        ("iscsiadm", vec!["-m", "session"]),
        ("ls", vec!["-l", "/dev/disk/by-path"]),
        (
            "vgs",
            vec!["-o", "vg_name,vg_lock_type,vg_size,vg_free", &vg],
        ),
        ("lvs", vec!["-a", "-o", "lv_name,lv_attr,lv_size", &vg]),
        ("lvmlockctl", vec!["--info"]),
    ] {
        let out = output(program, &args);
        text.push_str(&format!(
            "\n$ {program} {} ({})\n{}{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    text
}

/// [`state`]와 opld 로그의 끝
fn diagnostics(d: &Daemon) -> String {
    let log = d.log();
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(200)..].join("\n");
    format!("{}\n--- opld log (tail) ---\n{tail}", state())
}

/// 명령이 성공해야 한다.
fn must(program: &str, args: &[&str]) {
    let out = output(program, args);
    assert!(
        out.status.success(),
        "{program} {args:?}: {out:?}{}",
        state()
    );
}

/// 조건이 맞을 때까지 [`WAIT`]까지 다시 확인한다. 시간이 지나면 진단 정보와 함께 실패한다.
async fn until<F, Fut>(what: &str, d: &Daemon, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT;
    while !check().await {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}{}",
            diagnostics(d)
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 타깃 `target`의 iSCSI 세션이 있는지. 세션이 하나도 없으면 `iscsiadm`은 21로 끝난다.
fn session_present(target: &str) -> bool {
    let out = output("iscsiadm", &["-m", "session"]);
    assert!(
        out.status.success() || out.status.code() == Some(21),
        "iscsiadm -m session: {out:?}"
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.split_whitespace().any(|w| w == target))
}

/// LV `name`이 이 노드에서 켜져 있는지(`lv_attr` 다섯째 글자 `a`)
fn is_active(name: &str) -> bool {
    let target = format!("{}/{name}", vg());
    let out = output("lvs", &["--noheadings", "-o", "lv_attr", &target]);
    assert!(out.status.success(), "{out:?}{}", state());
    String::from_utf8_lossy(&out.stdout).trim().chars().nth(4) == Some('a')
}

/// VG에 있는 LV 이름 중 `prefix`로 시작하는 것
fn lvs(prefix: &str) -> Vec<String> {
    let out = output("lvs", &["--noheadings", "-o", "lv_name", &vg()]);
    assert!(out.status.success(), "{out:?}{}", state());
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with(prefix))
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

/// 볼륨 `name`의 LV 이름(`opl-{id}`)
async fn volume_lv(c: &Client, name: &str) -> String {
    let volume = c.get(&format!("/volumes/{name}")).await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    format!("opl-{}", volume["id"].as_str().unwrap())
}

#[tokio::test]
async fn lvm_shared_pool_runs_and_clones_a_vm_and_deactivates_its_disk() {
    let vg = vg();
    let iscsi = iscsi();
    // opld가 iSCSI 로그인부터 하도록 VG 잠금을 멈추고 로그아웃한다(LV는 모두 끈다).
    if let Some((portal, target)) = &iscsi {
        must("vgchange", &["-an", &vg]);
        must("vgchange", &["--lockstop", &vg]);
        if session_present(target) {
            must(
                "iscsiadm",
                &["-m", "node", "-T", target, "-p", portal, "--logout"],
            );
        }
        assert!(!session_present(target), "{}", state());
    }

    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다. 풀 상태는 자주 새로 고친다.
    let mut d = Daemon::new(
        "node1",
        "[reconciler]\nacpi_timeout_secs = 5\n[pools]\nrefresh_secs = 2\n",
    );
    d.start().await;
    d.init("lvm-shared").await;
    d.login().await;
    let c = client(&d);

    // 마운터가 로그인하고 잠금을 시작해 공유 VG를 확인하면 준비됐다고 알리고 VG의 용량을 보인다.
    // 공유 LVM 풀은 늘 공유다.
    let backend = match &iscsi {
        Some((portal, target)) => {
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": { "portal": portal, "target": target } })
        }
        None => json!({ "kind": "lvm-shared", "vg": vg }),
    };
    let created = c
        .post(
            "/storage-pools",
            &json!({ "name": "san", "spec": { "backend": backend } }),
        )
        .await
        .unwrap();
    assert_eq!(created["spec"]["shared"], true, "{created}");
    until("pool san ready", &d, || async {
        let node = c.get("/nodes/node1").await.unwrap();
        node["status"]["pools"]["san"]["ready"] == true
    })
    .await;
    let node = c.get("/nodes/node1").await.unwrap();
    let state = &node["status"]["pools"]["san"];
    assert!(state["total_bytes"].as_u64().unwrap() > 0, "{node}");
    assert_eq!(state["error"], Value::Null, "{node}");
    if let Some((_, target)) = &iscsi {
        assert!(session_present(target), "{}", diagnostics(&d));
    }

    // 볼륨은 두꺼운 LV `opl-{id}`이고 만든 뒤에는 꺼져 있다.
    d.ctl(&[
        "volume", "create", "root", "--pool", "san", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let lv = volume_lv(&c, "root").await;
    let dev = PathBuf::from("/dev").join(&vg).join(&lv);
    assert!(!is_active(&lv), "{}", diagnostics(&d));
    must("lvchange", &["-aey", &format!("{vg}/{lv}")]);
    assert!(qemu_io(&dev, "raw", "read -P 0 0 64M"));
    assert!(qemu_io(&dev, "raw", "write -P 0xab 0 64k"));
    must("lvchange", &["-an", &format!("{vg}/{lv}")]);

    // VM을 켜면 LV를 이 노드에서 켜고 host_device로 연다.
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
    assert!(is_active(&lv), "{}", diagnostics(&d));

    // 공유 LVM 디스크는 스냅샷이 없다.
    let resp = c
        .request(Method::POST, "/vms/web/snapshots")
        .json(&json!({ "name": "before" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "snapshot_unsupported", "{body}");

    // 켜진 채 남은 LV(쓰는 VM·Task가 없다)는 opld가 다시 뜰 때 끈다. 실행 중인 VM의 LV는 그대로 둔다.
    d.ctl(&[
        "volume", "create", "idle", "--pool", "san", "--size", "8M", "--format", "raw",
    ])
    .await
    .unwrap();
    let idle = volume_lv(&c, "idle").await;
    must("lvchange", &["-aey", &format!("{vg}/{idle}")]);
    d.kill();
    d.start().await;
    until("left-over lv deactivated", &d, || async {
        !is_active(&idle)
    })
    .await;
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    assert_eq!(web["status"]["pid"].as_u64(), Some(pid), "{web}");
    assert!(is_active(&lv), "{}", diagnostics(&d));

    // VM이 멈추면 LV를 끈다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    until("vm disk deactivated", &d, || async { !is_active(&lv) }).await;
    assert!(!dev.exists(), "{}", diagnostics(&d));

    // 디렉터리 풀로 복제하면 새 디스크는 qcow2 파일이고 같은 내용을 담는다. 읽으려고 켠 LV는 끈다.
    let dir = d.path("pool");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&["vm", "clone", "web", "copy", "--pool", "local"])
        .await
        .unwrap();
    let copy_volume = c.get("/volumes/copy-root").await.unwrap();
    assert_eq!(copy_volume["status"]["phase"], "ready", "{copy_volume}");
    assert_eq!(copy_volume["spec"]["format"], "qcow2", "{copy_volume}");
    let copy_file = dir.join(format!("{}.qcow2", copy_volume["id"].as_str().unwrap()));
    assert!(qemu_io(&copy_file, "qcow2", "read -P 0xab 0 64k"));
    assert!(!is_active(&lv), "{}", diagnostics(&d));

    // 같은 풀 안의 복제는 새 LV다.
    d.ctl(&["vm", "clone", "web", "twin"]).await.unwrap();
    let twin = volume_lv(&c, "twin-root").await;
    assert!(!is_active(&lv) && !is_active(&twin), "{}", diagnostics(&d));
    must("lvchange", &["-aey", &format!("{vg}/{twin}")]);
    let twin_dev = PathBuf::from("/dev").join(&vg).join(&twin);
    assert!(qemu_io(&twin_dev, "raw", "read -P 0xab 0 64k"));
    must("lvchange", &["-an", &format!("{vg}/{twin}")]);

    // 다시 켜고 멈춰도 LV는 꺼진다.
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    assert!(is_active(&lv), "{}", diagnostics(&d));
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    until("vm disk deactivated again", &d, || async {
        !is_active(&lv)
    })
    .await;

    for vm in ["copy", "twin", "web"] {
        d.ctl(&["vm", "delete", vm]).await.unwrap();
    }
    for volume in ["copy-root", "twin-root", "root", "idle"] {
        d.ctl(&["volume", "delete", volume]).await.unwrap();
    }
    assert!(!copy_file.exists());
    for name in [&lv, &twin, &idle] {
        assert!(lvs(name).is_empty(), "{}", diagnostics(&d));
    }
    d.ctl(&["pool", "delete", "local"]).await.unwrap();
    d.ctl(&["pool", "delete", "san"]).await.unwrap();
    d.stop();
}
