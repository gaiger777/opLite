//! 실제 라이브 마이그레이션: 한 호스트의 opld 둘로 클러스터를 만들고, 공유 풀 디스크를 가진 UEFI VM을
//! 실행 중에 다른 노드로 옮긴다. 확정 뒤 상태, 원본 QEMU 정리, UEFI 변수 파일, 디스크 내용, 마이그레이션
//! 중의 `vm_busy`, 대상 포트가 막혀 실패하면 원본이 계속 도는지를 본다. 멈춘 UEFI VM을 옮겨 켜면 변수
//! 파일이 따라가는지도 본다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 정지한 VM에서
//! `qemu-io`로 본다.
//! `cargo test -p opld --features migration-it --test migration`
//! QEMU(`qemu-it`와 같다), qemu-img, qemu-io가 필요하다. 이 Mac에서도 돈다(TCG).
//!
//! 공유 풀 시험은 그 풀의 기능을 함께 켜고 그 시험과 같은 환경 변수가 있어야 한다(Linux, root):
//! `nfs-it`(`OPL_TEST_NFS_EXPORT`), `rbd-it`(`OPL_TEST_RBD`, `client.admin`), `lvm-shared-it`
//! (`OPL_TEST_LVM_SHARED`, 있으면 `OPL_TEST_ISCSI`).
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
        // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
        let daemon = |name: &str, ports: &str| {
            Daemon::with_hypervisor(
                name,
                &format!("[reconciler]\nacpi_timeout_secs = 5\n[migration]\nports = \"{ports}\"\n"),
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
