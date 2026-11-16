//! 실제 Ceph 자동 구성(계획 56): Ceph가 없는 깨끗한 호스트에서 orbivisord가 apt로 Ceph 꾸러미를 설치하고 노드
//! 하나에 모니터·매니저를 세운다(`ceph init`). 시험이 만든 루프 장치에 OSD(ceph-volume LVM, BlueStore)를 만들고,
//! Ceph 풀을 만들어 Orbivisor RBD 풀로 등록하고, 그 풀의 볼륨으로 VM을 켠다. 디스크 안전(쓰는 중인 디스크·파일
//! 시스템이 든 디스크 — 지우라고 하면 지우고 그 위에 둘째 OSD)과 두 번째 `ceph init`을 거절하는지도 본다. 끝에 Ceph 명령들의 실제 JSON을 stderr에 찍고
//! opl-driver 해석기로 읽어 본다(시험 고정 JSON과 견주기). 실패하면 `ceph -s`, Ceph 데몬 저널과 opld 로그를 찍는다.
//! `sudo -E env "PATH=$PATH" cargo test -p opld --features ceph-it,qemu-it --test ceph -- --nocapture`
//! Linux(Ubuntu 24.04 = ceph 19.2)에서 root, apt로 Ceph를 받을 수 있는 네트워크, QEMU(`qemu-it`와 같다)와
//! qemu-block-extra가 필요하다. Ceph가 설치·구성되지 않은 호스트여야 하고(`/etc/ceph/ceph.conf`가 없어야 한다),
//! 시험은 이 호스트에 Ceph 꾸러미와 데몬을 남긴다(CI 러너용). 루프 장치는 시험이 만들고 끝에 뗀다.
#![cfg(all(feature = "ceph-it", target_os = "linux"))]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::Daemon;
use opl_driver::ceph::json::parse_pg_ls;
use opl_driver::ceph::{
    LSBLK_COLUMNS, ReportInput, parse_lsblk, parse_mgr_dump, parse_pg_stat, parse_pool_ls,
    parse_version, parse_volume_list, report,
};
use oplctl::Client;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// 노드가 풀을 준비하는 것 같은 느린 일을 기다리는 시간
const SLOW_WAIT: Duration = Duration::from_secs(180);

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

/// 명령을 실행하고, 성공해야 하며 표준 출력을 돌려준다.
fn run(program: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {program}: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?}: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// 명령의 출력을 진단으로 stderr에 찍는다(실패해도 계속).
fn show(program: &str, args: &[&str]) {
    match Command::new(program).args(args).output() {
        Ok(out) => eprintln!(
            "$ {program} {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => eprintln!("$ {program} {}: {e}", args.join(" ")),
    }
}

/// 시험이 실패하면(패닉으로 풀릴 때) Ceph 상태, Ceph 데몬 저널과 opld 로그를 찍는다. opld 로그가 든 임시 디렉터리가
/// 지워지기 전에 풀리도록 데몬 다음에 만든다.
struct Diagnostics {
    opld_log: PathBuf,
}

impl Drop for Diagnostics {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        show("ceph", &["--connect-timeout", "10", "-s"]);
        show(
            "journalctl",
            &[
                "--no-pager",
                "-n",
                "100",
                "-u",
                "ceph-mon@*",
                "-u",
                "ceph-mgr@*",
                "-u",
                "ceph-osd@*",
            ],
        );
        let log = std::fs::read_to_string(&self.opld_log).unwrap_or_default();
        eprintln!("opld log:\n{log}");
    }
}

/// 시험이 만든 루프 장치(10 GiB 성긴 파일). 풀릴 때 떼고 파일을 지운다(OSD의 LVM이 쓰고 있으면 커널이 나중에 뗀다).
struct Loop {
    dev: String,
    file: PathBuf,
}

impl Loop {
    /// `ext4`면 파일에 ext4를 만들어 둔다(지우지 않으면 OSD로 쓸 수 없는 디스크).
    fn new(file: &Path, ext4: bool) -> Self {
        let path = file.to_str().unwrap();
        run("truncate", &["-s", "10G", path]);
        if ext4 {
            run("mkfs.ext4", &["-q", "-F", path]);
        }
        let dev = String::from_utf8(run("losetup", &["--find", "--show", path]))
            .unwrap()
            .trim()
            .to_string();
        // lsblk가 파일 시스템 서명을 읽도록 udev가 새 장치를 살필 때까지 기다린다.
        run("udevadm", &["settle"]);
        Self {
            dev,
            file: file.to_path_buf(),
        }
    }
}

impl Drop for Loop {
    fn drop(&mut self) {
        let _ = Command::new("losetup").args(["-d", &self.dev]).status();
        let _ = std::fs::remove_file(&self.file);
    }
}

/// 조건이 맞을 때까지 `limit`만큼 다시 확인한다.
async fn wait_for<F, Fut>(what: &str, limit: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + limit;
    while !check().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// 요청을 보내고 실패한 응답의 상태와 오류 코드를 돌려준다.
async fn refused(c: &Client, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = c
        .request(Method::POST, path)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();
    (status, body)
}

/// 노드의 디스크 목록에서 `path`인 디스크
async fn node_disk(d: &Daemon, path: &str) -> Value {
    let disks = d.ctl_json(&["node", "disks", "node1"]).await;
    disks
        .as_array()
        .unwrap()
        .iter()
        .find(|disk| disk["path"] == path)
        .cloned()
        .unwrap_or_else(|| panic!("no disk {path} in {disks}"))
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

/// `ceph --connect-timeout 10 ARGS --format json`의 출력. 비교하도록 stderr에도 찍는다.
fn ceph_json(args: &[&str]) -> Vec<u8> {
    let mut argv = vec!["--connect-timeout", "10"];
    argv.extend_from_slice(args);
    argv.extend_from_slice(&["--format", "json"]);
    let out = run("ceph", &argv);
    eprintln!(
        "ceph {}:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out)
    );
    out
}

/// Ceph 19.2·ceph-volume·lsblk의 실제 JSON을 찍고 opl-driver의 해석기로 읽는다(시험 고정 JSON과 같은 모양인지).
/// OSD는 둘(`loop_dev`와 지우고 만든 디스크)이다.
fn check_real_outputs(loop_dev: &str) {
    let status = ceph_json(&["status"]);
    let quorum = ceph_json(&["quorum_status"]);
    let mgr_dump = ceph_json(&["mgr", "dump"]);
    let osd_tree = ceph_json(&["osd", "tree"]);
    let osd_df = ceph_json(&["osd", "df"]);
    let df = ceph_json(&["df"]);
    let metadata = ceph_json(&["osd", "metadata"]);
    let pool_ls = ceph_json(&["osd", "pool", "ls", "detail"]);
    let input = ReportInput {
        status: &status,
        quorum_status: &quorum,
        mgr_dump: &mgr_dump,
        osd_tree: &osd_tree,
        osd_df: &osd_df,
        df: &df,
        osd_metadata: &metadata,
        pool_ls: &pool_ls,
    };
    let r = report(&input, &[("vmpool".into(), "vmpool".into())]).unwrap();
    assert_eq!(r.quorum, ["node1"], "{r:?}");
    assert_eq!(r.mons.len(), 1, "{r:?}");
    assert_eq!(r.mgr.active.as_deref(), Some("node1"), "{r:?}");
    assert_eq!(r.osds.len(), 2, "{r:?}");
    assert!(
        r.osds.iter().any(|o| o.device.as_deref() == Some(loop_dev)),
        "{r:?}"
    );
    assert!(r.osds.iter().all(|o| o.total_bytes > 0), "{r:?}");
    let pool = r.pools.iter().find(|p| p.name == "vmpool").unwrap();
    assert_eq!(pool.size, Some(1), "{r:?}");
    assert_eq!(pool.orbivisor_pool.as_deref(), Some("vmpool"), "{r:?}");
    assert!(r.usage.total_bytes > 0, "{r:?}");

    let mgr = parse_mgr_dump(&ceph_json(&["mgr", "stat"])).unwrap();
    assert!(mgr.available, "{mgr:?}");
    // 자동 조정기가 PG 수를 바꾸는 중일 수 있으므로 모두 active인지는 보지 않는다.
    let (total, active) = parse_pg_stat(&ceph_json(&["pg", "stat"])).unwrap();
    assert!(total > 0 && active <= total, "{total} {active}");
    let (total, active) = parse_pg_ls(&ceph_json(&["pg", "ls-by-pool", "vmpool"])).unwrap();
    assert!(total > 0 && active <= total, "{total} {active}");
    let pools = parse_pool_ls(&ceph_json(&["osd", "pool", "ls"])).unwrap();
    assert!(pools.iter().any(|p| p.name == "vmpool"), "{pools:?}");
    // 풀 Task가 단 표시(rbd application의 orbivisor-pool)
    let detail = parse_pool_ls(&pool_ls).unwrap();
    let vmpool = detail.iter().find(|p| p.name == "vmpool").unwrap();
    assert_eq!(
        vmpool.orbivisor_pool.as_deref(),
        Some("vmpool"),
        "{detail:?}"
    );

    let volumes = run("ceph-volume", &["lvm", "list", "--format", "json"]);
    eprintln!(
        "ceph-volume lvm list:\n{}",
        String::from_utf8_lossy(&volumes)
    );
    let volumes = parse_volume_list(&volumes).unwrap();
    assert_eq!(volumes.len(), 2, "{volumes:?}");
    assert!(
        volumes
            .iter()
            .any(|v| v.devices.iter().any(|d| d == loop_dev)),
        "{volumes:?}"
    );

    let lsblk = run(
        "lsblk",
        &["--json", "--bytes", "--paths", "--output", LSBLK_COLUMNS],
    );
    eprintln!("lsblk:\n{}", String::from_utf8_lossy(&lsblk));
    parse_lsblk(&lsblk).unwrap();

    let version = String::from_utf8(run("ceph", &["--version"])).unwrap();
    eprintln!("ceph --version: {version}");
    let version = parse_version(version.trim()).unwrap();
    assert!(version.major >= 18, "{version:?}");
}

#[tokio::test]
async fn orbivisord_sets_up_ceph_and_runs_a_vm_on_its_pool() {
    assert!(
        !Path::new("/etc/ceph/ceph.conf").exists(),
        "this test needs a host without Ceph (/etc/ceph/ceph.conf exists)"
    );
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다. 루프 장치를 OSD 디스크로 쓴다(시험용 설정).
    let mut d = Daemon::new(
        "node1",
        "[reconciler]\nacpi_timeout_secs = 5\n[ceph]\nallow_loop_devices = true\n",
    );
    let _diagnostics = Diagnostics {
        opld_log: d.path("opld.log"),
    };
    d.start().await;
    d.init("ceph").await;
    d.login().await;
    let c = client(&d);
    let started = Instant::now();

    // 꾸러미 설치부터 모니터 정족수·매니저까지. 노드 주소(127.0.0.1)는 인터페이스 목록에 없으므로 망을 준다.
    let out = d
        .ctl(&["ceph", "init", "node1", "--public-network", "127.0.0.0/8"])
        .await
        .unwrap();
    assert!(
        out.contains("ready (monitor and manager on node1)"),
        "{out}"
    );
    eprintln!("ceph init took {:?}", started.elapsed());
    let ceph = c.get("/ceph").await.unwrap();
    assert_eq!(ceph["phase"], "ready", "{ceph}");
    assert_eq!(ceph["mons"][0]["name"], "node1", "{ceph}");
    assert_eq!(ceph["mons"][0]["addr"], "127.0.0.1", "{ceph}");
    assert!(
        ceph["release"].as_str().is_some_and(|r| !r.is_empty()),
        "{ceph}"
    );
    // 키는 응답에 없다.
    assert!(ceph.get("keys").is_none(), "{ceph}");
    let conf = std::fs::read_to_string("/etc/opl/ceph/ceph.conf").unwrap();
    assert!(
        conf.contains("mon_host = [v2:127.0.0.1:3300,v1:127.0.0.1:6789]"),
        "{conf}"
    );

    // 다시 init하면 거절한다.
    let (status, body) = refused(&c, "/ceph", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_exists", "{body}");

    // 빈 루프 장치는 OSD로 쓸 수 있다.
    let osd_loop = Loop::new(&d.path("osd.img"), false);
    let disk = node_disk(&d, &osd_loop.dev).await;
    assert_eq!(disk["state"], "available", "{disk}");

    // 쓰는 중인 디스크(마운트·시스템 디스크)는 거절한다.
    let disks = d.ctl_json(&["node", "disks", "node1"]).await;
    let busy = disks
        .as_array()
        .unwrap()
        .iter()
        .find(|disk| disk["state"] == "in-use")
        .unwrap_or_else(|| panic!("no disk in use in {disks}"))
        .clone();
    let (status, body) = refused(
        &c,
        "/ceph/osds",
        json!({ "node": "node1", "device": busy["path"], "wipe": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "disk_in_use", "{body}");

    // 파일 시스템이 든 디스크는 지우라고 해야 쓴다.
    let ext4_loop = Loop::new(&d.path("ext4.img"), true);
    let disk = node_disk(&d, &ext4_loop.dev).await;
    assert_eq!(disk["state"], "wipeable", "{disk}");
    assert!(
        disk["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "ext4 filesystem"),
        "{disk}"
    );
    let (status, body) = refused(
        &c,
        "/ceph/osds",
        json!({ "node": "node1", "device": ext4_loop.dev }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "disk_not_empty", "{body}");

    // OSD를 만들면 Ceph에 up·in으로 보인다.
    let started = Instant::now();
    let out = d
        .ctl(&["ceph", "osd", "add", "node1", &osd_loop.dev])
        .await
        .unwrap();
    assert!(
        out.contains(&format!("on node1 {} is up", osd_loop.dev)),
        "{out}"
    );
    eprintln!("ceph osd add took {:?}", started.elapsed());
    let ceph = c.get("/ceph").await.unwrap();
    assert_eq!(ceph["osd_nodes"].as_array().unwrap().len(), 1, "{ceph}");
    let status = d.ctl_json(&["ceph", "status"]).await;
    let osds = status["osds"].as_array().unwrap();
    assert_eq!(osds.len(), 1, "{status}");
    assert_eq!(osds[0]["up"], true, "{status}");
    assert_eq!(osds[0]["in"], true, "{status}");
    assert_ne!(status["health"]["status"], "HEALTH_ERR", "{status}");
    let disk = node_disk(&d, &osd_loop.dev).await;
    assert_eq!(disk["state"], "in-use", "{disk}");

    // 지우라고 하면(`--wipe --yes`) 파일 시스템이 든 디스크를 지우고(ceph-volume lvm zap) 그 위에 OSD를 만든다.
    let out = d
        .ctl(&[
            "ceph",
            "osd",
            "add",
            "node1",
            &ext4_loop.dev,
            "--wipe",
            "--yes",
        ])
        .await
        .unwrap();
    assert!(
        out.contains(&format!("on node1 {} is up", ext4_loop.dev)),
        "{out}"
    );
    let disk = node_disk(&d, &ext4_loop.dev).await;
    assert_eq!(disk["state"], "in-use", "{disk}");
    let status = d.ctl_json(&["ceph", "status"]).await;
    assert_eq!(status["osds"].as_array().unwrap().len(), 2, "{status}");

    // 호스트가 하나면 size 1을 적어야 한다. 풀은 Orbivisor RBD 풀(사용자 orbivisor, 관리 conf)로 등록된다.
    let (status, body) = refused(&c, "/ceph/pools", json!({ "name": "vmpool" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let out = d
        .ctl(&["ceph", "pool", "create", "vmpool", "--size", "1"])
        .await
        .unwrap();
    assert!(
        out.contains("storage pool vmpool (Ceph pool vmpool, size 1) is ready"),
        "{out}"
    );
    let pool = c.get("/storage-pools/vmpool").await.unwrap();
    let backend = &pool["spec"]["backend"];
    assert_eq!(backend["kind"], "rbd", "{pool}");
    assert_eq!(backend["user"], "orbivisor", "{pool}");
    assert_eq!(backend["conf"], "/etc/opl/ceph/ceph.conf", "{pool}");
    wait_for("pool vmpool ready on node1", SLOW_WAIT, || async {
        let node = c.get("/nodes/node1").await.unwrap();
        node["status"]["pools"]["vmpool"]["ready"] == true
    })
    .await;

    // 그 풀의 볼륨으로 VM을 켠다(QEMU rbd 드라이버, client.orbivisor와 관리 conf).
    d.ctl(&[
        "volume", "create", "root", "--pool", "vmpool", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let image = format!("opl-{}", volume["id"].as_str().unwrap());
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    let args = cmdline(web["status"]["pid"].as_u64().unwrap());
    assert!(
        args.contains(&format!(
            "driver=rbd,pool=vmpool,image={image},user=orbivisor,conf=/etc/opl/ceph/ceph.conf"
        )),
        "{args}"
    );
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();

    // 상태의 풀 표가 Ceph 풀을 Orbivisor 풀에 잇는다.
    let out = d.ctl(&["ceph", "status"]).await.unwrap();
    eprintln!("{out}");
    assert!(out.contains("mon: 1 (quorum node1)"), "{out}");
    assert!(out.contains("mgr: node1 (active)"), "{out}");
    assert!(out.contains("osd: 2 up, 2 in, 1 host"), "{out}");
    let status = d.ctl_json(&["ceph", "status"]).await;
    let pool = status["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "vmpool")
        .unwrap_or_else(|| panic!("no vmpool in {status}"))
        .clone();
    assert_eq!(pool["orbivisor_pool"], "vmpool", "{status}");

    check_real_outputs(&osd_loop.dev);
    d.ctl(&["pool", "delete", "vmpool"]).await.unwrap();
    d.stop();
}
