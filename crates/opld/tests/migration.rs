//! 실제 라이브 마이그레이션: 한 호스트의 opld 둘로 클러스터를 만들고, 공유 풀 디스크를 가진 UEFI VM을
//! 실행 중에 다른 노드로 옮긴다. 확정 뒤 상태, 원본 QEMU 정리, UEFI 변수 파일, 디스크 내용, 마이그레이션
//! 중의 `vm_busy`, 대상 포트가 막혀 실패하면 원본이 계속 도는지를 본다. 멈춘 UEFI VM을 옮겨 켜면 변수
//! 파일이 따라가는지도 본다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 정지한 VM에서
//! `qemu-io`로 본다. 노드 비우기(`oplctl node drain --wait`)가 실행 중인 VM을 다른 노드로 옮기는지도 본다.
//! 노드 로컬 디렉터리 풀(노드마다 다른 경로)의 디스크를 복사하며 옮기는지(`--pool`, 진행률, 실패 정리), 같은
//! 이름의 로컬 풀이 같은 경로라 비우기가 그 VM을 남기는지도 본다.
//! `cargo test -p opld --features migration-it --test migration`
//! QEMU(`qemu-it`와 같다), qemu-img, qemu-io가 필요하다. 이 Mac에서도 돈다(TCG).
//!
//! 공유 풀 시험은 그 풀의 기능을 함께 켜고 그 시험과 같은 환경 변수가 있어야 한다(Linux, root):
//! `nfs-it`(`OPL_TEST_NFS_EXPORT`), `rbd-it`(`OPL_TEST_RBD`, `client.admin`), `lvm-shared-it`
//! (`OPL_TEST_LVM_SHARED`, 있으면 `OPL_TEST_ISCSI`). `lvm-it`(`OPL_TEST_LVM`)·`zfs-it`(`OPL_TEST_ZFS`)를 함께
//! 켜면 로컬 디스크를 디렉터리 풀 → 그 블록 풀(노드 2만) → 디렉터리 풀로 옮긴다(qcow2 → raw → raw).
//!
//! 한 호스트의 두 노드는 systemd 스코프 이름(`opl-vm-{id}.scope`)과 브리지 NIC의 탭 이름이 겹치므로
//! 스코프를 끄고 NIC 없이 띄운다. 공유 LVM의 잠금(lvmlockd)은 호스트 단위이므로 두 노드가 같은 잠금을
//! 쓴다 — 원본이 확정 뒤 LV를 끄려 하면 대상 QEMU가 열고 있어 실패하고 경고만 남는다.
#![cfg(feature = "migration-it")]

mod common;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{Daemon, eventually, free_port};
use oplctl::Client;
use reqwest::{Method, StatusCode};
use rustix::process::{Pid, test_kill_process};
use serde_json::{Value, json};

/// 원본 UEFI 변수 파일 끝에 남기는 표식
const MARKER: &[u8] = b"opl-migration-it";

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

/// 두 노드의 `[hypervisor]`. HVF(macOS)에서는 보내는 QEMU가 메모리 변경 추적을 켜다 가끔 죽고 API도
/// `qemu-hvf` 노드의 마이그레이션을 거절하므로 이 Mac에서는 TCG로 돌린다. Linux는 KVM(없으면 TCG)이다.
fn hypervisor() -> &'static str {
    if cfg!(target_os = "macos") {
        "systemd_scope = false\naccel = \"tcg\""
    } else {
        "systemd_scope = false"
    }
}

/// 한 클러스터의 두 노드. 0번이 처음 VM을 띄우는 노드다.
struct Pair {
    d: [Daemon; 2],
    c: [Client; 2],
}

impl Pair {
    /// 노드 `node1`·`node2`를 띄워 클러스터를 만들고 둘 다 로그인한다. `ports`는 각 노드의 마이그레이션
    /// 받기 포트 범위다(같은 파일의 시험이 함께 돌므로 시험마다 다르게 준다).
    async fn new(cluster: &str, ports: [&str; 2]) -> Self {
        Self::with_migration(cluster, ports, "").await
    }

    /// `new`와 같고 두 노드의 `[migration]`에 `migration`을 더한다.
    async fn with_migration(cluster: &str, ports: [&str; 2], migration: &str) -> Self {
        // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
        let daemon = |name: &str, ports: &str| {
            Daemon::with_hypervisor(
                name,
                &format!(
                    "[reconciler]\nacpi_timeout_secs = 5\n[migration]\nports = \"{ports}\"\n{migration}"
                ),
                hypervisor(),
            )
        };
        let mut d = [daemon("node1", ports[0]), daemon("node2", ports[1])];
        for d in &mut d {
            d.start().await;
        }
        d[0].init(cluster).await;
        d[0].login().await;
        let url = d[0].join_url.clone().unwrap();
        d[1].join(&url).await.unwrap();
        let (password, fp) = (
            d[0].admin_password.clone().unwrap(),
            d[0].ca_fingerprint.clone().unwrap(),
        );
        d[1].login_with(&password, &fp).await;
        let c = [client(&d[0]), client(&d[1])];
        eventually("both nodes online", || async {
            let nodes = c[0].get("/nodes").await.unwrap();
            let nodes = nodes["items"].as_array().unwrap();
            nodes.len() == 2 && nodes.iter().all(|n| n["status"]["online"] == true)
        })
        .await;
        Self { d, c }
    }

    async fn node_id(&self, i: usize) -> String {
        let node = self.c[0]
            .get(&format!("/nodes/{}", self.d[i].name))
            .await
            .unwrap();
        node["id"].as_str().unwrap().to_string()
    }

    /// 풀이 두 노드에서 준비될 때까지 기다린다.
    async fn wait_pool_ready(&self, pool: &str) {
        for d in &self.d {
            d.wait_pool_ready(pool).await;
        }
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        // NFS 풀이 다시 마운트되지 않게 opld를 먼저 멈추고, 임시 디렉터리를 지우기 전에 마운트를
        // 푼다(마운트된 채로 지우면 내보내기의 파일까지 지운다). 풀지 못하면 디렉터리를 남긴다.
        for d in &mut self.d {
            if d.is_running() {
                d.kill();
            }
            #[cfg(target_os = "linux")]
            unmount_nfs_pools(d);
        }
    }
}

/// 데몬의 `{data_dir}/pools` 아래 NFS 마운트를 푼다.
#[cfg(target_os = "linux")]
fn unmount_nfs_pools(d: &mut Daemon) {
    let Ok(pools) = std::fs::canonicalize(d.data_dir().join("pools")) else {
        return;
    };
    let info = std::fs::read_to_string(opl_driver::mounts::MOUNTINFO).unwrap();
    let mut left = false;
    for point in opl_driver::mounts::nfs_mount_points(&info)
        .into_iter()
        .filter(|p| p.starts_with(&pools))
    {
        if !Command::new("umount")
            .arg(&point)
            .status()
            .is_ok_and(|s| s.success())
        {
            eprintln!("cannot unmount {}", point.display());
            left = true;
        }
    }
    if left {
        let dir = std::mem::replace(&mut d.dir, tempfile::TempDir::new().unwrap());
        eprintln!(
            "left {} in place because nfs pools are still mounted under it",
            dir.keep().display()
        );
    }
}

/// 프로세스가 살아 있는지
fn alive(pid: u64) -> bool {
    test_kill_process(Pid::from_raw(pid as i32).unwrap()).is_ok()
}

/// 명령줄에 `pattern`이 든 프로세스가 있는지(그 노드의 VM 실행 디렉터리로 그 노드의 QEMU를 찾는다)
fn process_with(pattern: &Path) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run pgrep")
        .success()
}

/// 노드 `d`의 VM 실행 디렉터리(QEMU 명령줄에 든다)
fn vm_run_dir(d: &Daemon, id: &str) -> PathBuf {
    d.path("run").join("vm").join(id)
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

/// API 오류 응답의 상태와 코드
async fn error(c: &Client, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = c.request(method, path).json(&body).send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

/// 실행 중인 VM `web`의 QEMU pid. 확정은 pid 없이 `running`을 쓰고 대상 워커가 넘겨받으며 채우므로
/// 기다린다.
async fn running_pid(c: &Client) -> u64 {
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let vm = c.get("/vms/web").await.unwrap();
        if vm["status"]["phase"] == "running"
            && let Some(pid) = vm["status"]["pid"].as_u64()
        {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "vm web is not running with a pid: {vm}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// 보낸 마이그레이션 Task와 원본의 VM id·QEMU pid
struct Started {
    task: Value,
    id: String,
    pid: u64,
}

/// 노드 `from`에서 도는 VM `web`을 노드 `to`로 옮기기 시작한다.
async fn start_migration(p: &Pair, from: usize, to: usize) -> Started {
    let c = &p.c[from];
    let pid = running_pid(c).await;
    let vm = c.get("/vms/web").await.unwrap();
    assert_eq!(vm["spec"]["node"].as_str().unwrap(), p.node_id(from).await);
    let task = c
        .post("/vms/web/migrate", &json!({ "node": p.d[to].name }))
        .await
        .unwrap();
    assert_eq!(task["spec"]["op"]["kind"], "vm-migrate", "{task}");
    Started {
        task,
        id: vm["id"].as_str().unwrap().to_string(),
        pid,
    }
}

/// Task가 성공하기를 기다리고 확정 뒤 상태를 확인한다: 두 노드에서 같은 기록, 대상의 새 QEMU가
/// 계속 돌고 원본 QEMU는 없다. 대상 QEMU의 pid를 돌려준다.
async fn finish_migration(p: &Pair, from: usize, to: usize, started: Started) -> u64 {
    p.c[from].wait_task(&started.task).await.unwrap();
    let target = p.node_id(to).await;
    for c in &p.c {
        let vm = c.get("/vms/web").await.unwrap();
        assert_eq!(vm["spec"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["migration"], Value::Null, "{vm}");
    }
    // 넘겨받은 VM은 계속 돈다(워커가 QMP `query-status`로 본다).
    let pid = running_pid(&p.c[to]).await;
    assert_ne!(pid, started.pid);
    assert!(alive(pid));
    assert!(process_with(&vm_run_dir(&p.d[to], &started.id)));
    // 원본 opld가 멈춘(`postmigrate`) QEMU를 끈다.
    eventually("source QEMU stopped", || async {
        !alive(started.pid) && !process_with(&vm_run_dir(&p.d[from], &started.id))
    })
    .await;
    assert_eq!(running_pid(&p.c[to]).await, pid);
    pid
}

/// VM `web`을 노드 `from`에서 `to`로 옮긴다.
async fn migrate(p: &Pair, from: usize, to: usize) -> u64 {
    let started = start_migration(p, from, to).await;
    finish_migration(p, from, to, started).await
}

/// VM `web`을 노드 0에 만들어 켜고 0→1→0으로 옮긴 뒤 강제로 끈다. 공유 풀 시험이 쓴다.
#[cfg(all(any(feature = "nfs-it", feature = "rbd-it"), target_os = "linux"))]
async fn round_trip(p: &Pair, disk: &str) {
    p.d[0]
        .ctl(&[
            "vm", "create", "web", "--memory", "256", "--disk", disk, "--node", "node1", "--start",
        ])
        .await
        .unwrap();
    migrate(p, 0, 1).await;
    migrate(p, 1, 0).await;
    p.d[0].ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
}

/// 공유 디렉터리 풀(두 노드가 같은 경로로 연다)의 qcow2 디스크를 가진 UEFI VM을 옮긴다.
#[tokio::test]
async fn migrates_a_running_vm_between_two_nodes() {
    // 노드 1은 처음에는 원본이고, 나중에 받을 때 쓸 포트 하나를 시험이 막는다.
    let blocked = free_port();
    let p = Pair::new("mig", [&format!("{blocked}-{blocked}"), "60100-60119"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);

    let shared = tempfile::Builder::new()
        .prefix("oplshared")
        .tempdir_in("/tmp")
        .unwrap();
    d.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    p.wait_pool_ready("shared").await;
    d.ctl(&[
        "volume", "create", "root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    let disk = shared
        .path()
        .join(format!("{}.qcow2", volume["id"].as_str().unwrap()));
    assert!(qemu_io(&disk, "qcow2", "write -P 0xab 0 64k"));

    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--node", "node1",
    ])
    .await
    .unwrap();
    let vm = c.get("/vms/web").await.unwrap();
    let id = vm["id"].as_str().unwrap().to_string();
    let uuid = id.parse().unwrap();

    // 원본의 UEFI 변수 파일(템플릿 복사본) 끝에 표식을 남기고 띄운다. 대상은 템플릿으로 시작하므로,
    // 옮긴 뒤 대상 파일에 표식이 있으면 QEMU가 pflash 내용을 옮겨 쓴 것이다.
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    let template = opl_qemu::HostProfile::native(Path::new(&share)).uefi_vars_template;
    let src_vars = opl_driver::uefi_vars_path(&p.d[0].data_dir(), uuid);
    let dst_vars = opl_driver::uefi_vars_path(&p.d[1].data_dir(), uuid);
    let mut content = std::fs::read(&template).unwrap();
    let at = content.len() - MARKER.len();
    content[at..].copy_from_slice(MARKER);
    std::fs::create_dir_all(src_vars.parent().unwrap()).unwrap();
    std::fs::write(&src_vars, &content).unwrap();
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");

    // 마이그레이션 중에는 다른 동작이 409 vm_busy다. 대상 QEMU를 띄우는 동안이므로 아직 끝나지 않았다.
    let started = start_migration(&p, 0, 1).await;
    let (status, body) = error(c, Method::POST, "/vms/web/actions/stop", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    let (status, body) = error(
        c,
        Method::POST,
        "/vms/web/migrate",
        json!({ "node": "node2" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    let pid = finish_migration(&p, 0, 1, started).await;
    let target = p.node_id(1).await;

    // 대상은 템플릿(표식 없음)으로 변수 파일을 만들어 받는다. 대상 QEMU가 이어서 실행하면서 pflash(UEFI
    // 변수)를 자기 파일에 원본 내용으로 쓰므로 표식이 생긴다. 게스트 펌웨어가 대상에서 계속 변수를 바꾸므로
    // 파일 전체를 원본과 견주지는 않는다.
    eventually("UEFI variables written on the target", || async {
        std::fs::read(&dst_vars).is_ok_and(|dst| dst.ends_with(MARKER))
    })
    .await;
    assert!(!std::fs::read(&template).unwrap().ends_with(MARKER));

    // 대상 포트가 막히면 확정 전에 실패하고 VM은 그 노드에서 그대로 돈다. 받으려고 띄운 QEMU는 버린다.
    let listener = TcpListener::bind(("127.0.0.1", blocked)).unwrap();
    let task = c
        .post("/vms/web/migrate", &json!({ "node": "node1" }))
        .await
        .unwrap();
    let err = c.wait_task(&task).await.unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("no free migration port in {blocked}-{blocked}")),
        "{err}"
    );
    for c in &p.c {
        let vm = c.get("/vms/web").await.unwrap();
        assert_eq!(vm["spec"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["phase"], "running", "{vm}");
        assert_eq!(vm["status"]["pid"].as_u64(), Some(pid), "{vm}");
        assert_eq!(vm["status"]["migration"], Value::Null, "{vm}");
    }
    assert!(alive(pid));
    eventually("receiving QEMU discarded", || async {
        !process_with(&vm_run_dir(&p.d[0], &id))
    })
    .await;

    // 포트가 풀리면 되돌아온다.
    drop(listener);
    migrate(&p, 1, 0).await;

    // 디스크 내용은 그대로다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&disk, "qcow2", "read -P 0xab 0 64k"));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(!disk.exists());
    d.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// 멈춘 UEFI VM을 다른 노드로 옮겨(`PUT`의 `node`) 켜면, 그 노드가 앞 노드의 변수 파일을 받아 켜고
/// 변수 노드가 된다. 로컬로 만든 디렉터리 풀을 공유로 바꾸고(`pool set`), VM이 볼륨과 다른 노드에
/// 있는 동안은 다시 로컬로 바꾸지 못하는지도 본다.
#[tokio::test]
async fn uefi_variables_follow_a_stopped_vm() {
    let p = Pair::new("mig-vars", ["60180-60189", "60190-60199"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let (node1, node2) = (p.node_id(0).await, p.node_id(1).await);

    let dir = tempfile::Builder::new()
        .prefix("oplvars")
        .tempdir_in("/tmp")
        .unwrap();
    d.ctl(&[
        "pool",
        "create",
        "vars",
        "--path",
        dir.path().to_str().unwrap(),
    ])
    .await
    .unwrap();
    p.wait_pool_ready("vars").await;
    d.ctl(&[
        "volume",
        "create",
        "vars-root",
        "--pool",
        "vars",
        "--size",
        "64M",
    ])
    .await
    .unwrap();
    let out = d.ctl(&["pool", "set", "vars", "--shared"]).await.unwrap();
    assert!(out.contains("storage pool vars: shared"), "{out}");

    // 노드 1에서 켜면 그 노드가 변수 노드다. 끈 뒤 노드 1의 변수 파일 끝에 표식을 남긴다.
    d.ctl(&[
        "vm",
        "create",
        "web",
        "--memory",
        "256",
        "--disk",
        "vars-root",
        "--node",
        "node1",
        "--start",
    ])
    .await
    .unwrap();
    let vm = c.get("/vms/web").await.unwrap();
    let uuid = vm["id"].as_str().unwrap().parse().unwrap();
    eventually("node 1 has the UEFI variables", || async {
        let vm = c.get("/vms/web").await.unwrap();
        vm["status"]["firmware_node"].as_str() == Some(node1.as_str())
    })
    .await;
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let src_vars = opl_driver::uefi_vars_path(&p.d[0].data_dir(), uuid);
    let dst_vars = opl_driver::uefi_vars_path(&p.d[1].data_dir(), uuid);
    let mut content = std::fs::read(&src_vars).unwrap();
    let at = content.len() - MARKER.len();
    content[at..].copy_from_slice(MARKER);
    std::fs::write(&src_vars, &content).unwrap();
    assert!(!dst_vars.exists());

    // 멈춘 채 노드 2로 옮긴다. VM이 볼륨과 다른 노드에 있으므로 풀을 로컬로 되돌릴 수 없다.
    let mut spec = c.get("/vms/web").await.unwrap()["spec"].clone();
    spec["node"] = json!(node2);
    c.put("/vms/web", &json!({ "spec": spec })).await.unwrap();
    let (status, body) = error(
        c,
        Method::PATCH,
        "/storage-pools/vars",
        json!({ "shared": false }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_in_use", "{body}");

    // 노드 2가 노드 1의 변수 파일을 받아 켜고 변수 노드가 된다. 노드 1의 파일은 남는다.
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    // 받아 온 파일은 펌웨어가 곧바로 고쳐 쓸 수 있다(KVM에서는 OVMF가 시작하면서 빈 영역의 표식을
    // 정리한다). 그래서 파일 내용이 아니라 노드 2가 노드 1에서 받아 왔다는 기록으로 본다.
    let log = p.d[1].log();
    assert!(log.contains("fetched UEFI variables"), "{log}");
    assert!(!log.contains("cannot fetch UEFI variables"), "{log}");
    assert!(dst_vars.exists());
    assert!(src_vars.exists());
    eventually("node 2 has the UEFI variables", || async {
        let vm = c.get("/vms/web").await.unwrap();
        vm["status"]["firmware_node"].as_str() == Some(node2.as_str())
    })
    .await;
    assert!(process_with(&vm_run_dir(
        &p.d[1],
        vm["id"].as_str().unwrap()
    )));

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "vars-root"]).await.unwrap();
    d.ctl(&["pool", "delete", "vars"]).await.unwrap();
}

/// `oplctl node drain --wait`: 노드 1을 유지보수로 바꾸고 그 노드에서 도는 VM을 스케줄러가 고른 노드 2로
/// 라이브 마이그레이션한다. 유지보수를 끈 뒤에도 VM은 노드 2에 남는다.
#[tokio::test]
async fn drain_moves_a_running_vm_to_the_other_node() {
    let p = Pair::new("mig-drain", ["60200-60209", "60210-60219"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let target = p.node_id(1).await;

    let shared = tempfile::Builder::new()
        .prefix("opldrain")
        .tempdir_in("/tmp")
        .unwrap();
    d.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    p.wait_pool_ready("shared").await;
    d.ctl(&[
        "volume", "create", "root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    let pid = running_pid(c).await;
    let id = c.get("/vms/web").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let out = d.ctl(&["node", "drain", "node1", "--wait"]).await.unwrap();
    assert_eq!(out, "node node1 drained\n");
    let node = c.get("/nodes/node1").await.unwrap();
    assert_eq!(node["spec"]["maintenance"], true, "{node}");
    for c in &p.c {
        let vm = c.get("/vms/web").await.unwrap();
        assert_eq!(vm["spec"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["migration"], Value::Null, "{vm}");
    }
    let moved = running_pid(&p.c[1]).await;
    assert_ne!(moved, pid);
    assert!(alive(moved));
    eventually("source QEMU stopped", || async {
        !alive(pid) && !process_with(&vm_run_dir(&p.d[0], &id))
    })
    .await;

    let out = d
        .ctl(&["node", "maintenance", "node1", "off"])
        .await
        .unwrap();
    assert_eq!(out, "node node1: maintenance off\n");
    let node = c.get("/nodes/node1").await.unwrap();
    assert_eq!(node["spec"]["maintenance"], false, "{node}");
    let vm = c.get("/vms/web").await.unwrap();
    assert_eq!(vm["spec"]["node"].as_str(), Some(target.as_str()), "{vm}");

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// `run`(마이그레이션을 기다리는 oplctl)이 끝날 때까지 VM `web`의 `vm-migrate` Task가 보인 진행률
/// (`status.progress`)을 모은다.
async fn watch_progress(
    c: &Client,
    run: impl Future<Output = anyhow::Result<String>>,
) -> (anyhow::Result<String>, Vec<Value>) {
    let vm = c.get("/vms/web").await.unwrap()["id"].clone();
    let mut seen: Vec<Value> = Vec::new();
    tokio::pin!(run);
    loop {
        tokio::select! {
            out = &mut run => return (out, seen),
            () = tokio::time::sleep(Duration::from_millis(200)) => {
                let tasks = c.get("/tasks").await.unwrap();
                for t in tasks["items"].as_array().unwrap() {
                    let progress = &t["status"]["progress"];
                    if t["spec"]["op"]["kind"] == "vm-migrate"
                        && t["spec"]["target"] == vm
                        && !progress.is_null()
                        && !seen.contains(progress)
                    {
                        seen.push(progress.clone());
                    }
                }
            }
        }
    }
}

/// 노드 `d`의 마이그레이션 디스크 기록 파일 수
fn disk_records(d: &Daemon) -> usize {
    std::fs::read_dir(d.data_dir().join("migration-disks")).map_or(0, |dir| dir.count())
}

/// 볼륨 `root`의 기록이 풀 `pool`(형식 `format`)과 노드 `node`에 있는지 두 노드에서 본다.
async fn assert_volume_on(p: &Pair, pool: &str, format: &str, node: &str) {
    for c in &p.c {
        let volume = c.get("/volumes/root").await.unwrap();
        assert_eq!(volume["spec"]["pool"], pool, "{volume}");
        assert_eq!(volume["spec"]["format"], format, "{volume}");
        assert_eq!(volume["status"]["node"].as_str(), Some(node), "{volume}");
    }
}

/// VM `web`이 노드 `node`에 확정되어 돌고 마이그레이션 표시가 없는지 두 노드에서 본다.
async fn assert_vm_on(p: &Pair, node: &str) {
    for c in &p.c {
        let vm = c.get("/vms/web").await.unwrap();
        assert_eq!(vm["spec"]["node"].as_str(), Some(node), "{vm}");
        assert_eq!(vm["status"]["node"].as_str(), Some(node), "{vm}");
        assert_eq!(vm["status"]["migration"], Value::Null, "{vm}");
    }
}

/// 노드마다 다른 경로의 로컬 디렉터리 풀(`a`는 노드 1만, `b`는 노드 2만)에 디스크를 둔 VM을, 디스크를
/// 복사하며 옮긴다(`oplctl vm migrate web node2 --pool b`). 대상의 포트가 모자라 받지 못하면 대상 풀에
/// 파일이 남지 않고 원본이 계속 돈다. 성공하면 볼륨 기록이 대상 풀·노드로 바뀌고 원본 파일이 지워지며,
/// 복사와 메모리 단계의 진행률이 Task에 보인다(복사가 몇 초 걸리도록 대역폭을 줄이고 디스크를 채운다).
/// 그다음 `--pool a`로 되돌아온다. 게스트에 OS가 없으므로 복사 중의 게스트 쓰기는 보지 않고, 마이그레이션
/// 전후에 꺼진 디스크에 쓴 무늬를 `qemu-io`로 본다.
#[tokio::test]
async fn migrates_a_vm_with_local_disks() {
    // 노드 2의 포트는 둘(마이그레이션과 NBD)뿐이다. 시험이 하나를 막으면 받지 못한다.
    let p = Pair::with_migration(
        "mig-local",
        ["60220-60229", "60230-60231"],
        "bandwidth_mib = 16\n",
    )
    .await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let (node1, node2) = (p.node_id(0).await, p.node_id(1).await);

    let dir = |prefix: &str| {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/tmp")
            .unwrap()
    };
    let (a, b) = (dir("oplpoola"), dir("oplpoolb"));
    for (name, path, node) in [("a", &a, &node1), ("b", &b, &node2)] {
        d.ctl(&[
            "pool",
            "create",
            name,
            "--path",
            path.path().to_str().unwrap(),
            "--node",
            node,
        ])
        .await
        .unwrap();
    }
    p.d[0].wait_pool_ready("a").await;
    p.d[1].wait_pool_ready("b").await;
    d.ctl(&["volume", "create", "root", "--pool", "a", "--size", "128M"])
        .await
        .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    let file = format!("{}.qcow2", volume["id"].as_str().unwrap());
    let (src, dst) = (a.path().join(&file), b.path().join(&file));
    // 16 MiB/s로 복사에 몇 초 걸린다.
    assert!(qemu_io(&src, "qcow2", "write -P 0xab 0 96M"));

    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    let pid = running_pid(c).await;
    let id = c.get("/vms/web").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 대상이 NBD 서버에 첫 포트를 쓰면 마이그레이션 포트가 남지 않는다. 대상은 만든 볼륨을 지운다.
    let blocked = TcpListener::bind(("127.0.0.1", 60231)).unwrap();
    let err = d
        .ctl(&["vm", "migrate", "web", "node2", "--pool", "b"])
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("no free migration port in 60230-60231"),
        "{err}"
    );
    assert_vm_on(&p, &node1).await;
    assert_eq!(running_pid(c).await, pid);
    assert!(alive(pid));
    assert_volume_on(&p, "a", "qcow2", &node1).await;
    assert!(src.exists());
    assert!(!dst.exists());
    eventually("receiving QEMU discarded and records removed", || async {
        !process_with(&vm_run_dir(&p.d[1], &id)) && p.d.iter().all(|d| disk_records(d) == 0)
    })
    .await;
    drop(blocked);

    // 포트가 풀리면 옮긴다. 확정 뒤 원본 파일을 지운다.
    let (out, progress) =
        watch_progress(c, d.ctl(&["vm", "migrate", "web", "node2", "--pool", "b"])).await;
    assert_eq!(out.unwrap(), "vm web: migrated to node2\n");
    let stages: Vec<&str> = progress
        .iter()
        .map(|p| p["stage"].as_str().unwrap())
        .collect();
    assert!(stages.contains(&"copying-disks"), "{progress:?}");
    assert_eq!(stages.last(), Some(&"migrating-memory"), "{progress:?}");
    assert!(
        progress
            .iter()
            .all(|p| p["done_bytes"].as_u64() <= p["total_bytes"].as_u64()),
        "{progress:?}"
    );
    assert!(
        progress
            .iter()
            .any(|p| p["total_bytes"].as_u64().unwrap() >= 96 << 20),
        "{progress:?}"
    );
    assert_vm_on(&p, &node2).await;
    assert_volume_on(&p, "b", "qcow2", &node2).await;
    let moved = running_pid(&p.c[1]).await;
    assert_ne!(moved, pid);
    assert!(alive(moved));
    eventually("source QEMU stopped and source disk deleted", || async {
        !alive(pid) && !process_with(&vm_run_dir(&p.d[0], &id)) && !src.exists()
    })
    .await;
    assert!(dst.exists());
    let task = c.get("/tasks").await.unwrap();
    let task = task["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["spec"]["op"]["kind"] == "vm-migrate" && t["status"]["state"] == "succeeded")
        .unwrap()
        .clone();
    assert_eq!(task["spec"]["op"]["pool"], "b", "{task}");
    assert_eq!(task["status"]["progress"], Value::Null, "{task}");

    // 복사한 디스크에 원본 내용이 있다. 꺼진 디스크에 더 써 두고 노드 1로 되돌린다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&dst, "qcow2", "read -P 0xab 0 96M"));
    assert!(qemu_io(&dst, "qcow2", "write -P 0xcd 96M 16M"));
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let out = d
        .ctl(&["vm", "migrate", "web", "node1", "--pool", "a"])
        .await
        .unwrap();
    assert_eq!(out, "vm web: migrated to node1\n");
    assert_vm_on(&p, &node1).await;
    assert_volume_on(&p, "a", "qcow2", &node1).await;
    eventually("disk deleted on node 2", || async { !dst.exists() }).await;
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&src, "qcow2", "read -P 0xab 0 96M"));
    assert!(qemu_io(&src, "qcow2", "read -P 0xcd 96M 16M"));

    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(!src.exists());
    d.ctl(&["pool", "delete", "a"]).await.unwrap();
    d.ctl(&["pool", "delete", "b"]).await.unwrap();
}

/// 한 호스트의 두 노드는 같은 이름의 로컬 디렉터리 풀이 같은 경로이므로, 비우기가 로컬 디스크 VM을 옮기려
/// 하면 대상이 같은 볼륨 파일을 보고 거절한다. VM은 원래 노드에서 계속 돌고 그 파일은 지워지지 않는다
/// (비우기가 로컬 디스크 VM을 옮기는 것은 `opl-api`의 `two_nodes.rs`가 본다).
#[tokio::test]
async fn drain_leaves_a_vm_whose_local_disk_the_target_already_has() {
    let p = Pair::new("mig-drain-local", ["60240-60249", "60250-60259"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let node1 = p.node_id(0).await;

    let local = tempfile::Builder::new()
        .prefix("opldrainl")
        .tempdir_in("/tmp")
        .unwrap();
    d.ctl(&[
        "pool",
        "create",
        "local",
        "--path",
        local.path().to_str().unwrap(),
    ])
    .await
    .unwrap();
    p.wait_pool_ready("local").await;
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    let disk = local
        .path()
        .join(format!("{}.qcow2", volume["id"].as_str().unwrap()));
    assert!(qemu_io(&disk, "qcow2", "write -P 0xab 0 64k"));
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    let pid = running_pid(c).await;

    let err = d
        .ctl(&["node", "drain", "node1", "--wait"])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("1 VM(s) were not moved: web ("), "{err}");
    assert!(
        err.contains("already exists on node \"node2\" in pool \"local\""),
        "{err}"
    );
    assert_vm_on(&p, &node1).await;
    assert_eq!(running_pid(c).await, pid);
    assert!(alive(pid));
    assert_volume_on(&p, "local", "qcow2", &node1).await;
    assert!(disk.exists());

    d.ctl(&["node", "maintenance", "node1", "off"])
        .await
        .unwrap();
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&disk, "qcow2", "read -P 0xab 0 64k"));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.ctl(&["pool", "delete", "local"]).await.unwrap();
}

/// NFS 풀(두 노드가 각자 마운트한다)
#[cfg(all(feature = "nfs-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_a_vm_on_an_nfs_pool() {
    let export = std::env::var("OPL_TEST_NFS_EXPORT")
        .expect("OPL_TEST_NFS_EXPORT must name a directory exported to 127.0.0.1");
    let p = Pair::new("mig-nfs", ["60120-60129", "60130-60139"]).await;
    let d = &p.d[0];
    let source = format!("127.0.0.1:{export}");
    d.ctl(&["pool", "create", "nas", "--nfs", &source])
        .await
        .unwrap();
    p.wait_pool_ready("nas").await;
    d.ctl(&[
        "volume", "create", "mig-root", "--pool", "nas", "--size", "64M",
    ])
    .await
    .unwrap();
    round_trip(&p, "mig-root").await;
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "mig-root"]).await.unwrap();
    d.ctl(&["pool", "delete", "nas"]).await.unwrap();
}

/// Ceph RBD 풀(`client.admin`)
#[cfg(all(feature = "rbd-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_a_vm_on_an_rbd_pool() {
    let pool = std::env::var("OPL_TEST_RBD").expect("OPL_TEST_RBD must name a Ceph pool");
    let p = Pair::new("mig-rbd", ["60140-60149", "60150-60159"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    c.post(
        "/storage-pools",
        &json!({ "name": "ceph", "spec": { "backend": { "kind": "rbd", "pool": pool } } }),
    )
    .await
    .unwrap();
    p.wait_pool_ready("ceph").await;
    d.ctl(&[
        "volume", "create", "mig-root", "--pool", "ceph", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    round_trip(&p, "mig-root").await;
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "mig-root"]).await.unwrap();
    d.ctl(&["pool", "delete", "ceph"]).await.unwrap();
}

/// 공유 LVM 풀(lvmlockd 공유 VG). 이미 배타로 쥔 LV에 `lvchange -aey`를 다시 해도 성공해야 한다
/// (대상이 넘겨받은 뒤 배타로 바꿀 때 기대는 동작).
#[cfg(all(feature = "lvm-shared-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_a_vm_on_a_shared_lvm_pool() {
    let vg =
        std::env::var("OPL_TEST_LVM_SHARED").expect("OPL_TEST_LVM_SHARED must name a shared VG");
    let must = |program: &str, args: &[&str]| {
        let out = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .env("LVM_SUPPRESS_FD_WARNINGS", "1")
            .output()
            .unwrap_or_else(|e| panic!("run {program}: {e}"));
        assert!(out.status.success(), "{program} {args:?}: {out:?}");
    };
    // 두 opld가 한 호스트에서 함께 로그인하고 잠금을 시작하지 않도록, 타깃 로그인과 잠금 시작은 시험이
    // 먼저 하고 풀에는 iSCSI를 적지 않는다(opld는 잠금 시작만 다시 한다 — 이미 했으면 곧바로 끝난다).
    if let Ok(iscsi) = std::env::var("OPL_TEST_ISCSI") {
        let (portal, target) = iscsi
            .split_once(',')
            .expect("OPL_TEST_ISCSI must look like PORTAL,TARGET");
        let sessions = Command::new("iscsiadm")
            .args(["-m", "session"])
            .output()
            .expect("run iscsiadm");
        if !String::from_utf8_lossy(&sessions.stdout).contains(target) {
            must(
                "iscsiadm",
                &["-m", "discovery", "-t", "sendtargets", "-p", portal],
            );
            must(
                "iscsiadm",
                &["-m", "node", "-T", target, "-p", portal, "--login"],
            );
            must("udevadm", &["settle"]);
        }
    }
    must("vgchange", &["--lockstart", &vg]);

    let p = Pair::new("mig-lvm", ["60160-60169", "60170-60179"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    c.post(
        "/storage-pools",
        &json!({ "name": "san", "spec": { "backend": { "kind": "lvm-shared", "vg": vg } } }),
    )
    .await
    .unwrap();
    p.wait_pool_ready("san").await;
    d.ctl(&[
        "volume", "create", "mig-root", "--pool", "san", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/mig-root").await.unwrap();
    let lv = format!("{vg}/opl-{}", volume["id"].as_str().unwrap());

    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "mig-root", "--node", "node1",
        "--start",
    ])
    .await
    .unwrap();
    // 원본 노드가 배타로 쥔 LV
    must("lvchange", &["-aey", &lv]);
    migrate(&p, 0, 1).await;
    must("lvchange", &["-aey", &lv]);
    migrate(&p, 1, 0).await;
    must("lvchange", &["-aey", &lv]);
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "mig-root"]).await.unwrap();
    d.ctl(&["pool", "delete", "san"]).await.unwrap();
}

/// 노드 1만 쓰는 디렉터리 풀 `a`의 qcow2 디스크를 가진 VM을, 노드 2만 쓰는 블록 풀 `blk`(`backend`)로
/// 디스크를 복사하며 옮기고(raw로 바뀐다) 다시 `a`로 되돌린다(raw 그대로). `dev`는 볼륨 id의 블록 장치
/// 경로다. 한 호스트의 두 노드가 같은 VG·데이터셋을 보므로 블록 풀은 노드 2에만 둔다.
#[cfg(all(any(feature = "lvm-it", feature = "zfs-it"), target_os = "linux"))]
async fn local_round_trip(
    cluster: &str,
    ports: [&str; 2],
    backend: Value,
    dev: impl Fn(&str) -> PathBuf,
) {
    let p = Pair::new(cluster, ports).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let (node1, node2) = (p.node_id(0).await, p.node_id(1).await);
    let a = tempfile::Builder::new()
        .prefix("oplpoola")
        .tempdir_in("/tmp")
        .unwrap();
    d.ctl(&[
        "pool",
        "create",
        "a",
        "--path",
        a.path().to_str().unwrap(),
        "--node",
        &node1,
    ])
    .await
    .unwrap();
    c.post(
        "/storage-pools",
        &json!({ "name": "blk", "spec": { "backend": backend, "nodes": [node2] } }),
    )
    .await
    .unwrap();
    p.d[0].wait_pool_ready("a").await;
    p.d[1].wait_pool_ready("blk").await;
    d.ctl(&["volume", "create", "root", "--pool", "a", "--size", "64M"])
        .await
        .unwrap();
    let id = c.get("/volumes/root").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (qcow2, raw, dev) = (
        a.path().join(format!("{id}.qcow2")),
        a.path().join(format!("{id}.raw")),
        dev(&id),
    );
    assert!(qemu_io(&qcow2, "qcow2", "write -P 0xab 0 1M"));
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--node", "node1", "--start",
    ])
    .await
    .unwrap();

    let out = d
        .ctl(&["vm", "migrate", "web", "node2", "--pool", "blk"])
        .await
        .unwrap();
    assert_eq!(out, "vm web: migrated to node2\n");
    assert_vm_on(&p, &node2).await;
    assert_volume_on(&p, "blk", "raw", &node2).await;
    eventually("qcow2 file deleted", || async { !qcow2.exists() }).await;
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&dev, "raw", "read -P 0xab 0 1M"));
    assert!(qemu_io(&dev, "raw", "write -P 0xcd 1M 1M"));

    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let out = d
        .ctl(&["vm", "migrate", "web", "node1", "--pool", "a"])
        .await
        .unwrap();
    assert_eq!(out, "vm web: migrated to node1\n");
    assert_vm_on(&p, &node1).await;
    assert_volume_on(&p, "a", "raw", &node1).await;
    eventually("block device deleted", || async { !dev.exists() }).await;
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&raw, "raw", "read -P 0xab 0 1M"));
    assert!(qemu_io(&raw, "raw", "read -P 0xcd 1M 1M"));

    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(!raw.exists());
    d.ctl(&["pool", "delete", "a"]).await.unwrap();
    d.ctl(&["pool", "delete", "blk"]).await.unwrap();
}

/// 디렉터리 풀 → LVM-thin 풀(`OPL_TEST_LVM`) → 디렉터리 풀
#[cfg(all(feature = "lvm-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_local_disks_through_an_lvm_thin_pool() {
    let target = std::env::var("OPL_TEST_LVM").expect("OPL_TEST_LVM must name VG/THINPOOL");
    let (vg, thinpool) = target
        .split_once('/')
        .expect("OPL_TEST_LVM must look like VG/THINPOOL");
    local_round_trip(
        "mig-lvm-thin",
        ["60260-60269", "60270-60279"],
        json!({ "kind": "lvm-thin", "vg": vg, "thinpool": thinpool }),
        |id| PathBuf::from("/dev").join(vg).join(format!("opl-{id}")),
    )
    .await;
}

/// 디렉터리 풀 → ZFS 풀(`OPL_TEST_ZFS`) → 디렉터리 풀
#[cfg(all(feature = "zfs-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_local_disks_through_a_zfs_pool() {
    let dataset = std::env::var("OPL_TEST_ZFS").expect("OPL_TEST_ZFS must name POOL/DATASET");
    local_round_trip(
        "mig-zfs",
        ["60280-60289", "60290-60299"],
        json!({ "kind": "zfs", "dataset": dataset }),
        |id| {
            Path::new("/dev/zvol")
                .join(&dataset)
                .join(format!("opl-{id}"))
        },
    )
    .await;
}
