//! 실제 라이브 마이그레이션: 한 호스트의 opld 둘로 클러스터를 만들고, 공유 풀 디스크를 가진 UEFI VM을
//! 실행 중에 다른 노드로 옮긴다. 확정 뒤 상태, 원본 QEMU 정리, UEFI 변수 파일, 디스크 내용, 마이그레이션
//! 중의 `vm_busy`, 대상 포트가 막혀 실패하면 원본이 계속 도는지를 본다. 멈춘 UEFI VM을 옮겨 켜면 변수
//! 파일이 따라가는지도 본다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 정지한 VM에서
//! `qemu-io`로 본다. 노드 비우기(`oplctl node drain --wait`)가 실행 중인 VM을 다른 노드로 옮기는지도 본다.
//! 노드 로컬 디렉터리 풀(노드마다 다른 경로)의 디스크를 복사하며 옮기는지(`--pool`, 진행률, 실패 정리), 같은
//! 이름의 로컬 풀이 같은 경로라 비우기가 그 VM을 남기는지도 본다. 균형 조정기가 hard 규칙을 어기는 VM을 추천·적용과
//! `auto`로 옮기는지도 본다. 로컬 디스크를 다른 노드로 복제하고(전체 뒤 증분) 그 복제본이 있는 노드로 바뀐 것만 보내며
//! 옮기는지도 본다. 공유 풀 ISO를 CD-ROM에 넣고 cloud-init이 있는 VM이 옮겨지는지, 노드 로컬 풀 ISO면 거절하는지도
//! 본다.
//! `cargo test -p orbivisord --features migration-it --test migration`
//! QEMU(`qemu-it`와 같다), qemu-img, qemu-io가 필요하다. 이 Mac에서도 돈다(TCG).
//!
//! 공유 풀 시험은 그 풀의 기능을 함께 켜고 그 시험과 같은 환경 변수가 있어야 한다(Linux, root):
//! `nfs-it`(`ORBIVISOR_TEST_NFS_EXPORT`), `rbd-it`(`ORBIVISOR_TEST_RBD`, `client.admin`), `lvm-shared-it`
//! (`ORBIVISOR_TEST_LVM_SHARED`, 있으면 `ORBIVISOR_TEST_ISCSI`). `lvm-it`(`ORBIVISOR_TEST_LVM`)·`zfs-it`(`ORBIVISOR_TEST_ZFS`)를 함께
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
use orbivisorctl::Client;
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
    let info = std::fs::read_to_string(orbivisor_driver::mounts::MOUNTINFO).unwrap();
    let mut left = false;
    for point in orbivisor_driver::mounts::nfs_mount_points(&info)
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
    assert_vm_on(p, &target).await;
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
    let share = orbivisor_types::legacy::env("QEMU_SHARE")
        .unwrap_or_else(|| "/opt/homebrew/share/qemu".into());
    let template = orbivisor_qemu::HostProfile::native(Path::new(&share)).uefi_vars_template;
    let src_vars = orbivisor_driver::uefi_vars_path(&p.d[0].data_dir(), uuid);
    let dst_vars = orbivisor_driver::uefi_vars_path(&p.d[1].data_dir(), uuid);
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
    let src_vars = orbivisor_driver::uefi_vars_path(&p.d[0].data_dir(), uuid);
    let dst_vars = orbivisor_driver::uefi_vars_path(&p.d[1].data_dir(), uuid);
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
    assert_vm_on(&p, &target).await;
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

/// 실행 중 디스크를 붙인(x86_64는 vCPU도 늘린) VM을 옮긴다(계획 54). 대상은 원본의 실행 중 하드웨어 기록으로 같은
/// 장치(루트 포트의 디스크, 붙인 vCPU)를 켜야 마이그레이션이 된다. 옮긴 뒤 기록은 그대로이고, aarch64는 대상에서
/// 그 디스크를 다시 뗀다(x86_64 `q35`는 게스트 OS 없이 떼기가 끝나지 않는다).
#[tokio::test]
async fn a_vm_with_a_disk_added_while_running_migrates() {
    let p = Pair::new("mig-hotplug", ["60300-60309", "60310-60319"]).await;
    let d = &p.d[0];
    let shared = tempfile::Builder::new()
        .prefix("oplhotplug")
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
    for (name, size) in [("root", "64M"), ("data", "16M")] {
        d.ctl(&["volume", "create", name, "--pool", "shared", "--size", size])
            .await
            .unwrap();
    }
    let mut create = vec![
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--node", "node1", "--start",
    ];
    let x86 = cfg!(target_arch = "x86_64");
    if x86 {
        create.extend(["--max-vcpus", "2"]);
    }
    d.ctl(&create).await.unwrap();
    let out = d.ctl(&["vm", "disk", "add", "web", "data"]).await.unwrap();
    assert_eq!(
        out,
        "vm web: disk data added\nvm web: applied while running\n"
    );
    if x86 {
        let out = d.ctl(&["vm", "set", "web", "--cores", "2"]).await.unwrap();
        assert!(out.ends_with("vm web: applied while running\n"), "{out}");
    }
    let before = p.c[0].get("/vms/web").await.unwrap()["status"]["hardware"].clone();
    assert_eq!(before["layout"]["disk_ports"], json!([0]), "{before}");

    migrate(&p, 0, 1).await;
    let vm = p.c[1].get("/vms/web").await.unwrap();
    assert_eq!(vm["status"]["hardware"], before, "{vm}");
    // 대상에서도 붙인 디스크가 루트 포트 하나를 쓰고, x86_64는 붙인 vCPU가 그대로다.
    let layout = &vm["status"]["hardware"]["layout"];
    assert_eq!(layout["vcpus"], if x86 { 2 } else { 1 }, "{vm}");
    assert_eq!(layout["disk_ports"], json!([0]), "{vm}");
    if !x86 {
        let out = p.d[1]
            .ctl(&["vm", "disk", "remove", "web", "data"])
            .await
            .unwrap();
        assert_eq!(
            out,
            "vm web: disk data removed\nvm web: applied while running\n"
        );
    }

    p.d[1].ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    for name in ["root", "data"] {
        d.ctl(&["volume", "delete", name]).await.unwrap();
    }
    d.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// 공유 디렉터리 풀의 ISO를 CD-ROM에 넣고 cloud-init이 있는 VM을 옮기면 대상 QEMU가 같은 ISO를 읽기 전용으로 열고
/// 대상 노드가 씨앗(`cidata.iso`)을 다시 만든다. 노드 로컬 풀의 ISO를 넣으면 옮기지 못한다(409 `cdrom_local`).
#[tokio::test]
async fn a_vm_with_a_cdrom_and_cloud_init_migrates() {
    let p = Pair::new("mig-cdrom", ["60340-60349", "60350-60359"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let node1 = p.node_id(0).await;
    let dir = |prefix: &str| {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/tmp")
            .unwrap()
    };
    let (shared, local) = (dir("oplcdshared"), dir("oplcdlocal"));
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
    d.ctl(&[
        "pool",
        "create",
        "local",
        "--path",
        local.path().to_str().unwrap(),
        "--node",
        &node1,
    ])
    .await
    .unwrap();
    p.wait_pool_ready("shared").await;
    p.d[0].wait_pool_ready("local").await;
    let iso = orbivisor_qemu::iso9660::build("INSTALL", &[("readme.txt", b"installer")]).unwrap();
    let file = d.path("inst.iso");
    std::fs::write(&file, iso).unwrap();
    for (name, pool) in [("inst", "shared"), ("local-inst", "local")] {
        d.ctl(&[
            "iso",
            "upload",
            name,
            "--pool",
            pool,
            "--file",
            file.to_str().unwrap(),
        ])
        .await
        .unwrap();
    }
    d.ctl(&[
        "volume", "create", "root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    d.ctl(&[
        "vm",
        "create",
        "web",
        "--memory",
        "256",
        "--disk",
        "root",
        "--cdrom",
        "inst",
        "--ci-user",
        "orbi",
        "--node",
        "node1",
        "--start",
    ])
    .await
    .unwrap();
    let vm = c.get("/vms/web").await.unwrap();
    let id = vm["id"].as_str().unwrap().to_string();
    let before = vm["status"]["hardware"].clone();
    assert_eq!(before["cdroms"], json!([{ "iso": "inst" }]), "{vm}");

    migrate(&p, 0, 1).await;
    let vm = p.c[1].get("/vms/web").await.unwrap();
    assert_eq!(vm["status"]["hardware"], before, "{vm}");
    let seed = vm_run_dir(&p.d[1], &id).join("cidata.iso");
    let text = String::from_utf8_lossy(&std::fs::read(&seed).unwrap()).into_owned();
    assert!(text.contains("\"local-hostname\": \"web\""), "{text}");
    assert!(text.contains(before["cloud_init"].as_str().unwrap()));
    migrate(&p, 1, 0).await;

    // 노드 로컬 풀의 ISO가 꽂혀 있으면 옮기지 못하고 VM은 그 노드에서 그대로 돈다.
    let out = d
        .ctl(&["vm", "cdrom", "insert", "web", "local-inst"])
        .await
        .unwrap();
    assert!(out.ends_with("vm web: applied while running\n"), "{out}");
    let pid = running_pid(c).await;
    let err = d.ctl(&["vm", "migrate", "web", "node2"]).await.unwrap_err();
    assert!(err.to_string().ends_with("(cdrom_local)"), "{err}");
    assert!(
        err.to_string().contains("from node-local pool \"local\""),
        "{err}"
    );
    assert_vm_on(&p, &node1).await;
    assert_eq!(running_pid(c).await, pid);

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    for iso in ["inst", "local-inst"] {
        d.ctl(&["iso", "delete", iso]).await.unwrap();
    }
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    for pool in ["shared", "local"] {
        d.ctl(&["pool", "delete", pool]).await.unwrap();
    }
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

/// VM `web`이 노드 `node`에 확정되어 돌고 마이그레이션 표시가 없는지 두 노드에서 본다. 대상 노드의 reconciler가
/// 원본보다 먼저 넘겨받으면 표시만 지운 status(원본 노드·pid)가 Task가 끝난 뒤에도 잠깐 보이고 곧 대상의 status로
/// 바뀌므로 기다린다.
async fn assert_vm_on(p: &Pair, node: &str) {
    let deadline = std::time::Instant::now() + common::WAIT;
    for c in &p.c {
        loop {
            let vm = c.get("/vms/web").await.unwrap();
            assert_eq!(vm["spec"]["node"].as_str(), Some(node), "{vm}");
            if vm["status"]["node"].as_str() == Some(node) && vm["status"]["migration"].is_null() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "vm web is not recorded on node {node}: {vm}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
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

    // 포트가 풀리면 옮긴다. 확정 뒤 원본 파일을 지운다. 두 포트가 실제로 bind 될 때까지 기다린다 — 리눅스의
    // 임시 포트 범위(32768-60999) 안이라 다른 연결이 잠깐 쥘 수 있다. 그 사이에 또 잡혀 같은 오류면 몇 번만
    // 다시 한다(받는 QEMU가 치워진 뒤에).
    let ports_free = || (60230..=60231).all(|port| TcpListener::bind(("127.0.0.1", port)).is_ok());
    let mut retries = 0;
    let (out, progress) = loop {
        eventually("migration ports 60230-60231 free", || async {
            ports_free()
        })
        .await;
        let (out, progress) =
            watch_progress(c, d.ctl(&["vm", "migrate", "web", "node2", "--pool", "b"])).await;
        match &out {
            Err(err) if retries < 3 && err.to_string().contains("no free migration port") => {
                retries += 1;
                eventually("receiving QEMU discarded and records removed", || async {
                    !process_with(&vm_run_dir(&p.d[1], &id))
                        && p.d.iter().all(|d| disk_records(d) == 0)
                })
                .await;
            }
            _ => break (out, progress),
        }
    };
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
/// (비우기가 로컬 디스크 VM을 옮기는 것은 `orbivisor-api`의 `two_nodes.rs`가 본다).
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

/// 두 이미지의 게스트가 보는 내용이 같은지(`qemu-img compare`)
fn same_images(a: &Path, b: &Path) -> bool {
    Command::new("qemu-img")
        .arg("compare")
        .args([a, b])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-img")
        .success()
}

/// VM `web`의 `status.replication`에서 노드 `node`(id)의 항목
async fn replica(c: &Client, node: &str) -> Value {
    let vm = c.get("/vms/web").await.unwrap();
    vm["status"]["replication"]
        .as_array()
        .and_then(|r| r.iter().find(|e| e["node"] == node))
        .cloned()
        .unwrap_or(Value::Null)
}

/// VM `web`의 노드 `node`(id) 복제본이 마지막 회차에 보낸 바이트
async fn last_bytes(c: &Client, node: &str) -> u64 {
    replica(c, node).await["last_bytes"].as_u64().unwrap()
}

/// 노드마다 다른 경로의 로컬 디렉터리 풀(`a`는 노드 1만, `b`는 노드 2만)에 디스크를 둔 VM을 노드 2의 풀 `b`로
/// 복제한다(`--replicate node2,*/60,pool=b`, 예약이 끼어들지 않게 스케줄러 틱을 늘림): 처음은 전체, 꺼진 디스크에
/// `qemu-io`로 쓴 뒤(qcow2의 영구 비트맵이 기록한다) 다시 켜서 복제하면 바뀐 블록만 보낸다. 복제본은 원본과 같다.
/// 그다음 노드 2로 옮기면(`--pool` 없음) 복제 뒤 바뀐 것만 보내고(디스크 복사 진행률이 디스크보다 훨씬 작다), 원본
/// 파일이 노드 1의 복제본으로 남으며 복제 방향이 뒤집힌다(`spec.replication`이 노드 1·풀 `a`). 노드 1로 되돌아와도
/// 같고, 두 무늬가 그대로다.
#[tokio::test]
async fn replicates_a_vm_and_migrates_it_to_its_replica() {
    let p = Pair::with_migration(
        "mig-repl",
        ["60320-60329", "60330-60339"],
        "[replication]\ntick_secs = 3600\n",
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
    let (a, b) = (dir("oplrepla"), dir("oplreplb"));
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
    assert!(qemu_io(&src, "qcow2", "write -P 0xab 0 32M"));
    d.ctl(&[
        "vm",
        "create",
        "web",
        "--memory",
        "256",
        "--disk",
        "root",
        "--node",
        "node1",
        "--replicate",
        "node2,*/60,pool=b",
        "--start",
    ])
    .await
    .unwrap();
    let vm = c.get("/vms/web").await.unwrap();
    assert_eq!(
        vm["spec"]["replication"],
        json!([{ "node": node2, "schedule": "*/60", "pool": "b" }]),
        "{vm}"
    );
    running_pid(c).await;

    // 처음은 전체
    let out = d.ctl(&["vm", "replicate", "web"]).await.unwrap();
    assert!(
        out.starts_with("vm web: replicated to node2 (") && out.ends_with(", full)\n"),
        "{out}"
    );
    let entry = replica(c, &node2).await;
    assert_eq!(entry["state"], "ready", "{entry}");
    assert_eq!(entry["last_full"], true, "{entry}");
    let table = d.ctl(&["vm", "replication", "web"]).await.unwrap();
    assert!(table.starts_with("NODE   STATE"), "{table}");
    assert!(table.contains("\nnode2  ready  "), "{table}");
    assert!(dst.exists());

    // 꺼진 디스크에 쓴 것만 다음 회차가 보낸다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&src, "qcow2", "write -P 0xcd 64M 64k"));
    d.ctl(&["vm", "start", "web"]).await.unwrap();
    let out = d.ctl(&["vm", "replicate", "web"]).await.unwrap();
    assert!(out.ends_with(", incremental)\n"), "{out}");
    let sent = last_bytes(c, &node2).await;
    assert!(sent > 0 && sent <= 1 << 20, "{sent}");
    let entry = replica(c, &node2).await;
    assert_eq!(
        (&entry["state"], &entry["last_full"]),
        (&json!("ready"), &json!(false)),
        "{entry}"
    );
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(same_images(&src, &dst));
    d.ctl(&["vm", "start", "web"]).await.unwrap();
    running_pid(c).await;

    // 복제본이 있는 노드 2로 옮기면 바뀐 것만 보낸다.
    let (out, progress) = watch_progress(c, d.ctl(&["vm", "migrate", "web", "node2"])).await;
    assert_eq!(out.unwrap(), "vm web: migrated to node2\n");
    assert!(
        progress
            .iter()
            .all(|p| p["total_bytes"].as_u64().unwrap() <= 4 << 20),
        "{progress:?}"
    );
    assert_vm_on(&p, &node2).await;
    assert_volume_on(&p, "b", "qcow2", &node2).await;
    let vm = c.get("/vms/web").await.unwrap();
    assert_eq!(
        vm["spec"]["replication"],
        json!([{ "node": node1, "schedule": "*/60", "pool": "a" }]),
        "{vm}"
    );
    eventually("node 1 holds a ready replica", || async {
        replica(c, &node1).await["state"] == "ready"
    })
    .await;
    // 원본 파일은 지우지 않고 그 마이그레이션 시점의 복제본이 된다.
    let tasks = c.get("/tasks").await.unwrap();
    let task = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["spec"]["op"]["kind"] == "vm-migrate" && t["status"]["state"] == "succeeded")
        .unwrap()
        .clone();
    assert_eq!(replica(c, &node1).await["generation"], task["id"]);
    assert!(src.exists());
    let line = d.ctl(&["vm", "replication", "web"]).await.unwrap();
    assert!(line.contains("\nnode1  ready  "), "{line}");

    // 노드 1로 되돌아온다. 두 무늬가 그대로다.
    let out = d.ctl(&["vm", "migrate", "web", "node1"]).await.unwrap();
    assert_eq!(out, "vm web: migrated to node1\n");
    assert_vm_on(&p, &node1).await;
    assert_volume_on(&p, "a", "qcow2", &node1).await;
    eventually("node 2 holds a ready replica", || async {
        replica(c, &node2).await["state"] == "ready"
    })
    .await;
    assert!(dst.exists());
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&src, "qcow2", "read -P 0xab 0 32M"));
    assert!(qemu_io(&src, "qcow2", "read -P 0xcd 64M 64k"));
    assert!(same_images(&src, &dst));

    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.ctl(&["pool", "delete", "a"]).await.unwrap();
    d.ctl(&["pool", "delete", "b"]).await.unwrap();
}

/// VM `name`의 배정 노드와 실행 노드의 id
async fn vm_nodes(c: &Client, name: &str) -> (Value, Value) {
    let vm = c.get(&format!("/vms/{name}")).await.unwrap();
    (vm["spec"]["node"].clone(), vm["status"]["node"].clone())
}

/// 균형 조정기가 지금 노드에서 hard 규칙을 어기는 VM을 옮긴다: `recommend`의 추천(이유 `rule`)을 `placement apply`로
/// 적용하고, `auto`면 스스로 옮긴다. 노드 부하는 실제 측정기의 요약이 리스로 리더에 간다. 한 호스트의 두 노드는 실제
/// CPU·메모리 사용이 같아 부하 불균형을 만들 수 없으므로 규칙 바로잡기로 옮기기 경로를 본다(불균형은 orbivisor-api 시험).
#[tokio::test]
async fn the_balancer_moves_a_vm_off_a_node_its_rules_forbid() {
    let p = Pair::with_migration(
        "mig-balance",
        ["60280-60289", "60290-60299"],
        "[test]\nplacement_interval_secs = 5\n",
    )
    .await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let node2 = Value::String(p.node_id(1).await);
    // 두 노드의 측정기 요약이 리더에 닿는다.
    eventually("load summaries of both nodes", || async {
        let status = c.get("/placement").await.unwrap();
        let nodes = status["nodes"].as_array().cloned().unwrap_or_default();
        nodes.len() == 2 && nodes.iter().all(|n| n["age_secs"].is_u64())
    })
    .await;
    let out = d.ctl(&["placement", "status"]).await.unwrap();
    assert!(
        out.starts_with("mode: recommend  policy: balance  threshold: 15%\n"),
        "{out}"
    );
    for line in out.lines().filter(|l| l.starts_with("node")) {
        assert!(line.trim_end().ends_with('s'), "no summary age: {out}");
    }

    let shared = tempfile::Builder::new()
        .prefix("oplbalance")
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
    for vm in ["web1", "web2"] {
        let disk = format!("{vm}-root");
        d.ctl(&[
            "volume", "create", &disk, "--pool", "shared", "--size", "64M",
        ])
        .await
        .unwrap();
        d.ctl(&[
            "vm", "create", vm, "--memory", "256", "--disk", &disk, "--node", "node1", "--start",
        ])
        .await
        .unwrap();
    }
    // 자리 잡은 VM은 규칙을 만들어도 옮기지 않으므로 위반으로 남는다.
    d.ctl(&[
        "placement-rule",
        "create",
        "on-node2",
        "--kind",
        "node-affinity",
        "--vm",
        "web1",
        "--node",
        "node2",
    ])
    .await
    .unwrap();
    eventually("web1 breaks the rule", || async {
        let vm = c.get("/vms/web1").await.unwrap();
        vm["status"]["rule_violations"] == json!(["on-node2"])
    })
    .await;

    // recommend: 이유 rule의 추천 하나를 적용한다.
    eventually("a rule recommendation", || async {
        let list = c.get("/placement/recommendations").await.unwrap();
        list.as_array().is_some_and(|l| l.len() == 1)
    })
    .await;
    let list = c.get("/placement/recommendations").await.unwrap();
    let rec = &list[0];
    assert_eq!(rec["vm_name"], "web1", "{list}");
    assert_eq!(rec["reason"], "rule", "{list}");
    assert_eq!(rec["to"], node2, "{list}");
    assert_eq!(rec["rules"], json!(["on-node2"]), "{list}");
    // 표의 ID는 id 끝 12자이고 `placement apply`가 그것으로 찾는다.
    let id = rec["id"].as_str().unwrap();
    let tail = &id[id.len() - 12..];
    let out = d.ctl(&["placement", "recommendations"]).await.unwrap();
    assert!(
        out.contains(tail) && out.contains(" web1 ") && out.contains(" rule "),
        "{out}"
    );
    let out = d.ctl(&["placement", "apply", tail]).await.unwrap();
    let task = out
        .trim_end()
        .strip_prefix("vm web1: moving from node1 to node2 (task ")
        .and_then(|t| t.strip_suffix(')'))
        .unwrap_or_else(|| panic!("{out}"));
    let task = c.get(&format!("/tasks/{task}")).await.unwrap();
    assert_eq!(task["spec"]["op"]["kind"], "vm-migrate", "{task}");
    let task = c.wait_task(&task).await.unwrap();
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // 대상 노드가 먼저 넘겨받으면 status의 노드는 Task가 끝난 뒤 곧 바뀐다(`assert_vm_on`).
    eventually("web1 recorded on node2", || async {
        vm_nodes(c, "web1").await == (node2.clone(), node2.clone())
    })
    .await;

    // auto: 같은 규칙에 넣은 web2를 스스로 옮긴다.
    d.ctl(&[
        "placement-rule",
        "set",
        "on-node2",
        "--vm",
        "web1",
        "--vm",
        "web2",
    ])
    .await
    .unwrap();
    let out = d
        .ctl(&["placement", "set", "--mode", "auto"])
        .await
        .unwrap();
    assert_eq!(out, "placement settings updated\n");
    eventually("web2 moved to node2", || async {
        vm_nodes(c, "web2").await == (node2.clone(), node2.clone())
    })
    .await;
    assert_eq!(vm_nodes(c, "web1").await, (node2.clone(), node2.clone()));

    d.ctl(&["placement", "set", "--mode", "off"]).await.unwrap();
    d.ctl(&["placement-rule", "delete", "on-node2"])
        .await
        .unwrap();
    for vm in ["web1", "web2"] {
        d.ctl(&["vm", "stop", vm, "--force"]).await.unwrap();
        d.ctl(&["vm", "delete", vm]).await.unwrap();
        d.ctl(&["volume", "delete", &format!("{vm}-root")])
            .await
            .unwrap();
    }
    d.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// NFS 풀(두 노드가 각자 마운트한다)
#[cfg(all(feature = "nfs-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_a_vm_on_an_nfs_pool() {
    let export = std::env::var("ORBIVISOR_TEST_NFS_EXPORT")
        .expect("ORBIVISOR_TEST_NFS_EXPORT must name a directory exported to 127.0.0.1");
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
    let pool =
        std::env::var("ORBIVISOR_TEST_RBD").expect("ORBIVISOR_TEST_RBD must name a Ceph pool");
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
    let vg = std::env::var("ORBIVISOR_TEST_LVM_SHARED")
        .expect("ORBIVISOR_TEST_LVM_SHARED must name a shared VG");
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
    if let Ok(iscsi) = std::env::var("ORBIVISOR_TEST_ISCSI") {
        let (portal, target) = iscsi
            .split_once(',')
            .expect("ORBIVISOR_TEST_ISCSI must look like PORTAL,TARGET");
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

/// 디렉터리 풀 → LVM-thin 풀(`ORBIVISOR_TEST_LVM`) → 디렉터리 풀
#[cfg(all(feature = "lvm-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_local_disks_through_an_lvm_thin_pool() {
    let target =
        std::env::var("ORBIVISOR_TEST_LVM").expect("ORBIVISOR_TEST_LVM must name VG/THINPOOL");
    let (vg, thinpool) = target
        .split_once('/')
        .expect("ORBIVISOR_TEST_LVM must look like VG/THINPOOL");
    local_round_trip(
        "mig-lvm-thin",
        ["60260-60269", "60270-60279"],
        json!({ "kind": "lvm-thin", "vg": vg, "thinpool": thinpool }),
        |id| PathBuf::from("/dev").join(vg).join(format!("opl-{id}")),
    )
    .await;
}

/// 디렉터리 풀 → ZFS 풀(`ORBIVISOR_TEST_ZFS`) → 디렉터리 풀
#[cfg(all(feature = "zfs-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_local_disks_through_a_zfs_pool() {
    let dataset =
        std::env::var("ORBIVISOR_TEST_ZFS").expect("ORBIVISOR_TEST_ZFS must name POOL/DATASET");
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

/// `qemu-img info`의 backing 파일 이름(없으면 Null). 실행 중인 QEMU가 연 파일도 읽는다(`-U`).
fn backing_of(path: &Path) -> Value {
    let out = Command::new("qemu-img")
        .args(["info", "--output=json", "-U"])
        .arg(path)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    let info: Value = serde_json::from_slice(&out.stdout).unwrap();
    info["backing-filename"].clone()
}

/// 노드 1의 로컬 디렉터리 풀 `a`에 둔 템플릿에서 연결 배포한 VM(qcow2 겹층)을 노드 2의 로컬 풀 `b`로 옮긴다(계획 58
/// §9): 디스크 복사(`sync: full`)가 사슬 전체를 읽어 대상은 backing이 없는 독립 볼륨이고, 확정이 볼륨 기록의 `base`를
/// 지운다. 원본 겹층은 지워지고 기반 파일은 그대로이며, 남은 연결 복제가 없으므로 템플릿을 되돌릴 수 있다.
#[tokio::test]
async fn a_linked_clone_migrates_to_a_local_pool_of_another_node() {
    let p = Pair::new("mig-linked", ["60360-60369", "60370-60379"]).await;
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
    d.ctl(&["volume", "create", "base", "--pool", "a", "--size", "64M"])
        .await
        .unwrap();
    let base = c.get("/volumes/base").await.unwrap();
    let base_file = a
        .path()
        .join(format!("{}.qcow2", base["id"].as_str().unwrap()));
    assert!(qemu_io(&base_file, "qcow2", "write -P 0x5a 0 1M"));
    d.ctl(&[
        "vm", "create", "tmpl", "--memory", "256", "--disk", "base", "--node", "node1",
    ])
    .await
    .unwrap();
    d.ctl(&["template", "convert", "tmpl"]).await.unwrap();
    let base_bytes = std::fs::read(&base_file).unwrap();

    let out = d
        .ctl(&[
            "template", "deploy", "tmpl", "--name", "web", "--linked", "--node", "node1", "--start",
        ])
        .await
        .unwrap();
    assert_eq!(
        out,
        "vm web deployed from tmpl (linked)\nvm web: running on node1\n"
    );
    let vm = c.get("/vms/web").await.unwrap();
    let disk = vm["spec"]["disks"][0]["volume"]
        .as_str()
        .unwrap()
        .to_string();
    let volume = c.get(&format!("/volumes/{disk}")).await.unwrap();
    assert_eq!(volume["spec"]["base"], base["id"], "{volume}");
    let file = format!("{}.qcow2", volume["id"].as_str().unwrap());
    let (src, dst) = (a.path().join(&file), b.path().join(&file));
    assert_eq!(
        backing_of(&src),
        base_file.file_name().unwrap().to_str().unwrap()
    );

    let out = d
        .ctl(&["vm", "migrate", "web", "node2", "--pool", "b"])
        .await
        .unwrap();
    assert_eq!(out, "vm web: migrated to node2\n");
    assert_vm_on(&p, &node2).await;
    for c in &p.c {
        let volume = c.get(&format!("/volumes/{disk}")).await.unwrap();
        assert_eq!(volume["spec"]["pool"], "b", "{volume}");
        assert_eq!(volume["spec"]["base"], Value::Null, "{volume}");
    }
    assert_eq!(backing_of(&dst), Value::Null);
    eventually("source overlay deleted", || async { !src.exists() }).await;
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&dst, "qcow2", "read -P 0x5a 0 1M"));
    assert_eq!(std::fs::read(&base_file).unwrap(), base_bytes);

    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", &disk]).await.unwrap();
    let out = d.ctl(&["template", "unconvert", "tmpl"]).await.unwrap();
    assert_eq!(out, "template tmpl converted back to a VM\n");
    d.ctl(&["vm", "delete", "tmpl"]).await.unwrap();
    d.ctl(&["volume", "delete", "base"]).await.unwrap();
    d.ctl(&["pool", "delete", "a"]).await.unwrap();
    d.ctl(&["pool", "delete", "b"]).await.unwrap();
}

/// NFS 공유 풀의 연결 복제는 디스크를 복사하지 않고 옮기므로 사슬(상대 backing)과 `base`가 그대로다(계획 58 §9).
#[cfg(all(feature = "nfs-it", target_os = "linux"))]
#[tokio::test]
async fn a_linked_clone_on_an_nfs_pool_migrates_with_its_chain() {
    let export = std::env::var("ORBIVISOR_TEST_NFS_EXPORT")
        .expect("ORBIVISOR_TEST_NFS_EXPORT must name a directory exported to 127.0.0.1");
    let p = Pair::new("mig-nfs-linked", ["60380-60389", "60390-60399"]).await;
    let d = &p.d[0];
    let source = format!("127.0.0.1:{export}");
    d.ctl(&["pool", "create", "nas", "--nfs", &source])
        .await
        .unwrap();
    p.wait_pool_ready("nas").await;
    d.ctl(&[
        "volume", "create", "mig-base", "--pool", "nas", "--size", "64M",
    ])
    .await
    .unwrap();
    d.ctl(&[
        "vm", "create", "mig-tmpl", "--memory", "256", "--disk", "mig-base",
    ])
    .await
    .unwrap();
    d.ctl(&["template", "convert", "mig-tmpl"]).await.unwrap();
    d.ctl(&[
        "template", "deploy", "mig-tmpl", "--name", "web", "--linked", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    migrate(&p, 0, 1).await;
    migrate(&p, 1, 0).await;
    let c = &p.c[0];
    let base = c.get("/volumes/mig-base").await.unwrap();
    let vm = c.get("/vms/web").await.unwrap();
    let disk = vm["spec"]["disks"][0]["volume"]
        .as_str()
        .unwrap()
        .to_string();
    let volume = c.get(&format!("/volumes/{disk}")).await.unwrap();
    assert_eq!(volume["spec"]["base"], base["id"], "{volume}");
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", &disk]).await.unwrap();
    d.ctl(&["template", "delete", "mig-tmpl"]).await.unwrap();
    d.ctl(&["volume", "delete", "mig-base"]).await.unwrap();
    d.ctl(&["pool", "delete", "nas"]).await.unwrap();
}

/// 노드 `d`의 VM `id` TPM 상태 파일(`{data_dir}/vm/{id}/tpm/tpm2-00.permall`)
#[cfg(feature = "vtpm-it")]
fn tpm_state(d: &Daemon, id: &str) -> PathBuf {
    orbivisor_driver::tpm_state_path(&d.data_dir(), id.parse().unwrap())
}

#[cfg(feature = "vtpm-it")]
fn sha256_file(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}

/// VM `web`의 `firmware_node`가 노드 `i`이고 `tpm_sha256`이 그 노드의 상태 파일 해시와 같아질 때까지 기다려 해시를
/// 돌려준다(작업자가 `[test] firmware_sync_secs`마다 다시 센다).
#[cfg(feature = "vtpm-it")]
async fn tpm_recorded_on(p: &Pair, i: usize, id: &str) -> String {
    let node = p.node_id(i).await;
    let file = tpm_state(&p.d[i], id);
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let vm = p.c[0].get("/vms/web").await.unwrap();
        if vm["status"]["firmware_node"] == node.as_str()
            && let Some(hash) = vm["status"]["tpm_sha256"].as_str()
            && file.exists()
            && sha256_file(&file) == hash
        {
            return hash.to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the TPM state of vm web is not recorded on {}: {vm}",
            p.d[i].name
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 실제 swtpm(`vtpm-it`): TPM VM을 실행 중에 옮기면 QEMU가 swtpm 상태를 마이그레이션 스트림으로 옮기고(받는 쪽은 상태를
/// 만들지 않는다) 대상 노드가 `firmware_node`가 되며 원본 swtpm은 끝난다. 끈 VM을 다른 노드에서 켜면 그 노드가
/// `firmware_node`에서 TPM 상태를 받아 온다.
#[cfg(feature = "vtpm-it")]
#[tokio::test]
async fn a_tpm_vm_migrates_with_its_state() {
    let p = Pair::with_migration(
        "mig-tpm",
        ["60400-60409", "60410-60419"],
        "[test]\nfirmware_sync_secs = 2\n",
    )
    .await;
    let d = &p.d[0];
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--tpm", "on", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    let vm = p.c[0].get("/vms/web").await.unwrap();
    let id = vm["id"].as_str().unwrap().to_string();
    tpm_recorded_on(&p, 0, &id).await;
    let src_ctrl = vm_run_dir(&p.d[0], &id).join("tpm.sock");
    assert!(process_with(&src_ctrl), "no swtpm on node1");
    assert!(!tpm_state(&p.d[1], &id).exists());

    migrate(&p, 0, 1).await;
    eventually("the source swtpm ends", || async {
        !process_with(&src_ctrl)
    })
    .await;
    assert!(process_with(&vm_run_dir(&p.d[1], &id).join("tpm.sock")));
    tpm_recorded_on(&p, 1, &id).await;
    let vm = p.c[0].get("/vms/web").await.unwrap();
    assert_eq!(vm["status"]["firmware_copies"], Value::Null, "{vm}");

    // 끈 뒤 노드 1에서 켜면 노드 2의 상태를 받아 온다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let moved = tpm_recorded_on(&p, 1, &id).await;
    let mut spec = p.c[0].get("/vms/web").await.unwrap()["spec"].clone();
    spec["node"] = json!(p.node_id(0).await);
    p.c[0]
        .put("/vms/web", &json!({ "spec": spec }))
        .await
        .unwrap();
    // 노드 1에 남은 옛 파일이 지금 상태와 같으면(마이그레이션 뒤 TPM이 바뀌지 않음) 받지 않고 그것으로 켠다.
    // 다르면 노드 2에서 받아 온다(켜면 펌웨어가 TPM에 쓰므로 켠 뒤 파일은 견주지 않는다).
    let stale = tpm_state(&p.d[0], &id);
    let current_here = stale.exists() && sha256_file(&stale) == moved;
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let log = p.d[0].log();
    assert!(
        current_here || log.contains("fetched the TPM state"),
        "node1 had a stale TPM file but did not fetch the current one: {log}"
    );
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let back = tpm_recorded_on(&p, 0, &id).await;
    eprintln!("tpm state on node2 {moved}, back on node1 {back}");
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
}

/// 계획 60: VM 지표 기록이 라이브 마이그레이션을 따라간다. 디스크 없는 VM을 노드 1에서 켜 5초 점이 쌓이면 노드 2로
/// 옮기고, 짧게 둔 `[test] metrics_push_secs` 뒤 노드 2의 `vm/{id}.rrd`에 노드 1 시절 점이 있고 노드 1의 파일은 없어진다.
/// 기록 조회(`vm stats --range hour`)는 VM의 노드에 묻고 옮기기 전과 뒤의 점을 함께 보인다. `[test]` 고리는 `qemu-it`
/// 빌드에 있다.
#[cfg(feature = "qemu-it")]
#[tokio::test]
async fn metrics_history_follows_a_live_migration() {
    let p = Pair::with_migration(
        "mig-hist",
        ["60420-60429", "60430-60439"],
        "[test]\nmetrics_flush_secs = 1\nmetrics_push_secs = 1\n",
    )
    .await;
    let d = &p.d[0];
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    let id = p.c[0].get("/vms/web").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // VM 기록 칸 cpu의 아는 점들(시각, 값)
    let cpu = |reply: &Value| -> Vec<(i64, f64)> {
        let start = reply["start"].as_i64().unwrap();
        let step = reply["step_ms"].as_i64().unwrap();
        reply["columns"]["cpu"]["avg"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.as_f64().map(|v| (start + i as i64 * step, v)))
            .collect()
    };
    let history = || d.ctl_json(&["vm", "stats", "web", "--range", "hour", "--points", "1000"]);
    eventually("two VM samples on node1", || async {
        cpu(&history().await).len() >= 2
    })
    .await;
    // 파일에 쓰인(1초마다) 점만 견준다.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let before = cpu(&history().await);
    let file = |i: usize| {
        p.d[i]
            .data_dir()
            .join("metrics")
            .join("vm")
            .join(format!("{id}.rrd"))
    };
    assert!(file(0).exists());

    migrate(&p, 0, 1).await;
    eventually("the history file moved to node2", || async {
        file(1).exists() && !file(0).exists()
    })
    .await;
    let last_before = before.last().unwrap().0;
    eventually("a sample on node2", || async {
        cpu(&history().await).iter().any(|(t, _)| *t > last_before)
    })
    .await;
    let reply = history().await;
    let after = cpu(&reply);
    for (t, v) in &before {
        assert!(
            after
                .iter()
                .any(|(t2, v2)| t2 == t && (v2 - v).abs() < 1e-3),
            "point {t} = {v} did not follow the VM: {reply}"
        );
    }
    p.d[1].ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
}
