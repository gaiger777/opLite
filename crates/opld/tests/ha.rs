//! 실제 HA 복구: 한 호스트의 opld 둘(`[ha] fencing = "process"`)과 위트니스로 클러스터를 만들고, 노드 A에서
//! 도는 HA VM과 HA 아닌 VM을 둔 채 A의 opld와 그 QEMU들을 kill -9 한다. 리더가 A의 리스가 끝난 것을 보고
//! HA VM만 노드 B로 옮겨 켜는지(기록 Task `vm-ha-recover`), A를 다시 띄워도 그 VM을 건드리지 않는지 본다.
//! HA 그룹 {A:1, B:2}의 VM이 B에 놓이고, B가 죽으면 A로 복구됐다가 B가 돌아오면 라이브 마이그레이션으로
//! 되돌아가는지도 본다(이 Mac에서는 TCG). A에서 두 번 크래시한(`max_restart` 1을 넘은) HA VM이 B로 옮겨
//! 켜지는지도 본다. 호스트 종료 정책: 끄기 + `failover`면 A가 끝나기 전에 HA VM을 끄고 B에서 켜는지, 재부팅 +
//! 기본(`conditional`)이면 얼려 두었다가 A가 돌아와 켜는지, 끄기 + `migrate`면 비우기로 미리 옮기는지 본다
//! (`[test] host_shutdown`). 기본(`fast`) 프로필에서 kill -9·망 끊기·SIGSTOP 뒤 복구 시간이 25초 이하인지 잰다.
//! `cargo test -p opld --features ha-it --test ha`
//! QEMU(`qemu-it`와 같다)와 qemu-img가 필요하다. 이 Mac에서도 돈다.
//!
//! 한 호스트의 두 노드는 systemd 스코프 이름(`opl-vm-{id}.scope`)이 겹치므로 스코프를 끈다(`migration.rs`).
#![cfg(feature = "ha-it")]

mod common;

use std::future::Future;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Daemon, eventually};
use oplctl::Client;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use serde_json::Value;

/// 복구를 기다리는 시간. A의 recover_after(fence_after 2초 + 여유 2초 = 4초), 리더 선거, 대상 노드의
/// QEMU 시작(CI의 KVM, 이 Mac의 HVF·TCG)을 넉넉히 덮는다.
const RECOVER_WAIT: Duration = Duration::from_secs(180);

/// 두 노드의 설정: 프로세스 펜싱, 리스가 2초 끊기면 HA VM을 끈다. UEFI만 있는 게스트는 ACPI 종료에
/// 응하지 않을 수 있다.
const CONFIG: &str =
    "[reconciler]\nacpi_timeout_secs = 5\n[ha]\nfencing = \"process\"\nfence_after_secs = 2\n";

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

/// 조건이 맞을 때까지 `wait` 동안 다시 확인한다.
async fn eventually_within<F, Fut>(what: &str, wait: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + wait;
    while !check().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(500)).await;
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

/// 이 시험이 띄운 QEMU 하나를 pid로 kill -9 하고 사라질 때까지 기다린다.
async fn kill_qemu(pid: u64) {
    let _ = kill_process(Pid::from_raw(pid as i32).unwrap(), Signal::KILL);
    eventually(&format!("QEMU {pid} gone"), || async { !alive(pid) }).await;
}

/// 노드 이름 → id
async fn node_id(c: &Client, name: &str) -> String {
    c.get(&format!("/nodes/{name}")).await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// 노드 `node`에서 도는 VM의 QEMU pid
async fn running_pid(c: &Client, vm: &str, node: &str) -> u64 {
    let deadline = Instant::now() + RECOVER_WAIT;
    loop {
        if let Ok(v) = c.get(&format!("/vms/{vm}")).await
            && v["status"]["phase"] == "running"
            && v["status"]["node"] == node
            && let Some(pid) = v["status"]["pid"].as_u64()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for vm {vm} running on {node}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
async fn the_leader_restarts_an_ha_vm_of_a_dead_node_on_another_node() {
    let daemon = |name: &str| Daemon::with_hypervisor(name, CONFIG, "systemd_scope = false");
    let mut a = daemon("node-a");
    let mut b = daemon("node-b");
    let mut w = Daemon::witness("wit");
    a.start().await;
    b.start().await;
    w.start().await;
    a.init("ha").await;
    a.login().await;
    let url = a.join_url.clone().unwrap();
    b.join(&url).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;
    let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();
    let (ca, cb) = (client(&a), client(&b));
    // 두 일반 노드가 온라인이고 HA를 돌릴 수 있다(펜싱 설정을 등록했다).
    eventually("both nodes online and HA capable", || async {
        let nodes = ca.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap().clone();
        nodes.len() == 3
            && nodes.iter().all(|n| n["status"]["online"] == true)
            && nodes
                .iter()
                .filter(|n| n["spec"]["witness"] != true)
                .all(|n| n["status"]["ha"]["fencing"] == "process")
    })
    .await;
    let (id_a, id_b) = (node_id(&ca, "node-a").await, node_id(&ca, "node-b").await);

    // 두 노드가 같은 경로로 여는 공유 디렉터리 풀과 그 위의 디스크
    let shared = tempfile::Builder::new()
        .prefix("oplha")
        .tempdir_in("/tmp")
        .unwrap();
    a.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    a.wait_pool_ready("shared").await;
    b.wait_pool_ready("shared").await;
    a.ctl(&[
        "volume", "create", "db-root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();

    // 실행 중인 VM의 HA는 켤 수 없으므로 만들 때 켠다. HA 아닌 VM은 디스크 없이 둔다.
    a.ctl(&[
        "vm",
        "create",
        "db",
        "--memory",
        "256",
        "--disk",
        "db-root",
        "--node",
        "node-a",
        "--ha",
        "--restart",
        "on-failure",
        "--start",
    ])
    .await
    .unwrap();
    a.ctl(&[
        "vm", "create", "plain", "--memory", "256", "--node", "node-a", "--start",
    ])
    .await
    .unwrap();
    let db_pid = running_pid(&ca, "db", &id_a).await;
    let plain_pid = running_pid(&ca, "plain", &id_a).await;

    // 노드 A가 죽는다: opld를 먼저(QEMU를 다시 띄우지 않게), 그 다음 이 시험이 띄운 A의 QEMU들
    a.kill();
    kill_qemu(db_pid).await;
    kill_qemu(plain_pid).await;

    // 리더(B, 위트니스와 과반수)가 A의 리스가 끝난 것을 보고 HA VM을 B로 옮겨 켠다.
    eventually_within("HA VM running on node B", RECOVER_WAIT, || async {
        let Ok(vm) = cb.get("/vms/db").await else {
            return false;
        };
        vm["spec"]["node"] == id_b.as_str()
            && vm["status"]["phase"] == "running"
            && vm["status"]["node"] == id_b.as_str()
    })
    .await;
    let recovered_pid = running_pid(&cb, "db", &id_b).await;
    assert_ne!(recovered_pid, db_pid);
    assert!(alive(recovered_pid));

    // 기록 Task: A에서 B로, 처음부터 성공
    let vm_db = cb.get("/vms/db").await.unwrap();
    let db_id = vm_db["id"].as_str().unwrap().to_string();
    let tasks = cb.get("/tasks").await.unwrap();
    let records: Vec<&Value> = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["spec"]["op"]["kind"] == "vm-ha-recover")
        .collect();
    assert_eq!(records.len(), 1, "{tasks}");
    let record = records[0];
    assert_eq!(record["spec"]["op"]["from"], id_a.as_str(), "{record}");
    assert_eq!(record["spec"]["target"], vm_db["id"], "{record}");
    assert_eq!(record["spec"]["node"], id_b.as_str(), "{record}");
    assert_eq!(record["status"]["state"], "succeeded", "{record}");

    // HA 아닌 VM은 A에 남는다.
    let plain = cb.get("/vms/plain").await.unwrap();
    assert_eq!(plain["spec"]["node"], id_a.as_str(), "{plain}");

    // A를 다시 띄워도 HA VM은 B에서 그대로 돈다(A는 그 VM의 QEMU를 띄우지 않는다).
    a.start().await;
    eventually_within("node A back online", RECOVER_WAIT, || async {
        let Ok(node) = cb.get("/nodes/node-a").await else {
            return false;
        };
        node["status"]["online"] == true
    })
    .await;
    // A의 워커가 자리를 잡을 동안 몇 번 본다. A가 옛 상태로 잠깐 쓴 status("waiting for the cluster
    // majority …", 노드 A)는 B의 워커가 곧 다시 쓰므로 status는 끝에 한 번 본다.
    for _ in 0..20 {
        let vm = cb.get("/vms/db").await.unwrap();
        assert_eq!(vm["spec"]["node"], id_b.as_str(), "{vm}");
        assert!(alive(recovered_pid));
        assert!(!process_with(&a.path("run").join("vm").join(&db_id)));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(running_pid(&cb, "db", &id_b).await, recovered_pid);
    let plain = cb.get("/vms/plain").await.unwrap();
    assert_eq!(plain["spec"]["node"], id_a.as_str(), "{plain}");

    b.ctl(&["vm", "stop", "db", "--force"]).await.unwrap();
    b.ctl(&["vm", "stop", "plain", "--force"]).await.unwrap();
    b.ctl(&["vm", "delete", "db"]).await.unwrap();
    b.ctl(&["vm", "delete", "plain"]).await.unwrap();
    b.ctl(&["volume", "delete", "db-root"]).await.unwrap();
    b.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// 되돌아가기 시험 노드의 `[hypervisor]`. HVF(macOS)는 마이그레이션을 거절하므로 이 Mac에서는 TCG다.
fn migrating_hypervisor() -> &'static str {
    if cfg!(target_os = "macos") {
        "systemd_scope = false\naccel = \"tcg\""
    } else {
        "systemd_scope = false"
    }
}

#[tokio::test]
async fn an_ha_vm_recovers_to_the_next_group_node_and_fails_back() {
    // 그룹 {A:1, B:2}의 HA VM은 B에 놓인다. B가 죽으면 A로 복구되고, B가 돌아와 리더가 30초 넘게 리스를
    // 본 뒤 B로 라이브 마이그레이션해 되돌아간다.
    let daemon = |name: &str, ports: &str| {
        Daemon::with_hypervisor(
            name,
            &format!("{CONFIG}[migration]\nports = \"{ports}\"\n"),
            migrating_hypervisor(),
        )
    };
    let mut a = daemon("node-a", "49630-49639");
    let mut b = daemon("node-b", "49640-49649");
    let mut w = Daemon::witness("wit");
    a.start().await;
    b.start().await;
    w.start().await;
    a.init("ha-fb").await;
    a.login().await;
    let url = a.join_url.clone().unwrap();
    b.join(&url).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;
    let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();
    let ca = client(&a);
    eventually("both nodes online and HA capable", || async {
        let nodes = ca.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap().clone();
        nodes.len() == 3
            && nodes.iter().all(|n| n["status"]["online"] == true)
            && nodes
                .iter()
                .filter(|n| n["spec"]["witness"] != true)
                .all(|n| n["status"]["ha"]["fencing"] == "process")
    })
    .await;
    let (id_a, id_b) = (node_id(&ca, "node-a").await, node_id(&ca, "node-b").await);

    let shared = tempfile::Builder::new()
        .prefix("oplhafb")
        .tempdir_in("/tmp")
        .unwrap();
    a.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    a.wait_pool_ready("shared").await;
    b.wait_pool_ready("shared").await;
    a.ctl(&[
        "volume", "create", "db-root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    a.ctl(&[
        "ha-group", "create", "g", "--node", "node-a:1", "--node", "node-b:2",
    ])
    .await
    .unwrap();
    // 노드를 주지 않으면 스케줄러가 그룹 우선순위가 높은 B를 고른다.
    a.ctl(&[
        "vm",
        "create",
        "db",
        "--memory",
        "256",
        "--disk",
        "db-root",
        "--ha-group",
        "g",
        "--restart",
        "on-failure",
        "--start",
    ])
    .await
    .unwrap();
    let db_pid = running_pid(&ca, "db", &id_b).await;

    // B가 죽는다: A(위트니스와 과반수)가 A로 복구한다.
    b.kill();
    kill_qemu(db_pid).await;
    eventually_within("HA VM running on node A", RECOVER_WAIT, || async {
        let Ok(vm) = ca.get("/vms/db").await else {
            return false;
        };
        vm["spec"]["node"] == id_a.as_str()
            && vm["status"]["phase"] == "running"
            && vm["status"]["node"] == id_a.as_str()
    })
    .await;
    let recovered_pid = running_pid(&ca, "db", &id_a).await;

    // B가 돌아오면 30초 뒤 리더가 B로 라이브 마이그레이션한다.
    b.start().await;
    eventually_within("HA VM failed back to node B", RECOVER_WAIT, || async {
        let Ok(vm) = ca.get("/vms/db").await else {
            return false;
        };
        vm["spec"]["node"] == id_b.as_str()
            && vm["status"]["phase"] == "running"
            && vm["status"]["node"] == id_b.as_str()
    })
    .await;
    let tasks = ca.get("/tasks").await.unwrap();
    let migrations: Vec<&Value> = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["spec"]["op"]["kind"] == "vm-migrate")
        .collect();
    assert_eq!(migrations.len(), 1, "{tasks}");
    assert_eq!(migrations[0]["spec"]["node"], id_a.as_str());
    assert_eq!(migrations[0]["spec"]["op"]["node"], id_b.as_str());
    // 원본 QEMU는 마이그레이션 뒤 끝난다.
    eventually("source QEMU gone", || async { !alive(recovered_pid) }).await;

    a.ctl(&["vm", "stop", "db", "--force"]).await.unwrap();
    a.ctl(&["vm", "delete", "db"]).await.unwrap();
    a.ctl(&["volume", "delete", "db-root"]).await.unwrap();
    a.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

#[tokio::test]
async fn an_ha_vm_crashing_past_max_restart_is_relocated_to_another_node() {
    // 기본 정책(`max_restart` 1): A의 QEMU가 한 번 죽으면 A에서 다시 켜고, 두 번째에는 A의 reconciler가 옮겨
    // 달라고 해 리더가 B로 옮겨 켠다. 노드 장애가 아니므로 A는 살아 있다.
    let daemon = |name: &str| Daemon::with_hypervisor(name, CONFIG, "systemd_scope = false");
    let mut a = daemon("node-a");
    let mut b = daemon("node-b");
    let mut w = Daemon::witness("wit");
    a.start().await;
    b.start().await;
    w.start().await;
    a.init("ha-rl").await;
    a.login().await;
    let url = a.join_url.clone().unwrap();
    b.join(&url).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;
    let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();
    let ca = client(&a);
    eventually("both nodes online and HA capable", || async {
        let nodes = ca.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap().clone();
        nodes.len() == 3
            && nodes.iter().all(|n| n["status"]["online"] == true)
            && nodes
                .iter()
                .filter(|n| n["spec"]["witness"] != true)
                .all(|n| n["status"]["ha"]["fencing"] == "process")
    })
    .await;
    let (id_a, id_b) = (node_id(&ca, "node-a").await, node_id(&ca, "node-b").await);

    let shared = tempfile::Builder::new()
        .prefix("oplharl")
        .tempdir_in("/tmp")
        .unwrap();
    a.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    a.wait_pool_ready("shared").await;
    b.wait_pool_ready("shared").await;
    a.ctl(&[
        "volume", "create", "db-root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    a.ctl(&[
        "vm", "create", "db", "--memory", "256", "--disk", "db-root", "--node", "node-a", "--ha",
        "--start",
    ])
    .await
    .unwrap();
    let first = running_pid(&ca, "db", &id_a).await;

    // 첫 크래시: A에서 다시 켠다.
    kill_qemu(first).await;
    let deadline = Instant::now() + RECOVER_WAIT;
    let second = loop {
        let pid = running_pid(&ca, "db", &id_a).await;
        if pid != first {
            break pid;
        }
        assert!(Instant::now() < deadline, "db not restarted on node A");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    // 두 번째 크래시: B로 옮겨 켜고 옮긴 기록을 하나 남긴다.
    kill_qemu(second).await;
    eventually_within("HA VM relocated to node B", RECOVER_WAIT, || async {
        let Ok(vm) = ca.get("/vms/db").await else {
            return false;
        };
        vm["spec"]["node"] == id_b.as_str()
            && vm["status"]["phase"] == "running"
            && vm["status"]["node"] == id_b.as_str()
    })
    .await;
    let vm = ca.get("/vms/db").await.unwrap();
    assert_eq!(vm["spec"]["power"], "running", "{vm}");
    assert_eq!(
        vm["status"]["ha"]["relocations"].as_array().map(Vec::len),
        Some(1),
        "{vm}"
    );
    assert!(vm["status"]["ha"].get("relocate_from").is_none(), "{vm}");
    assert!(vm["status"]["ha"].get("error").is_none(), "{vm}");
    let pid = running_pid(&ca, "db", &id_b).await;
    assert!(alive(pid));

    a.ctl(&["vm", "stop", "db", "--force"]).await.unwrap();
    a.ctl(&["vm", "delete", "db"]).await.unwrap();
    a.ctl(&["volume", "delete", "db-root"]).await.unwrap();
    a.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// 노드 A(설정 `extra_a`가 더해짐)·B와 위트니스로 클러스터를 만들고 두 노드가 HA를 돌릴 수 있을 때까지
/// 기다린다. A가 처음 노드이고 로그인돼 있다.
async fn ha_cluster(name: &str, extra_a: &str) -> (Daemon, Daemon, Daemon) {
    ha_cluster_with(name, CONFIG, extra_a).await
}

/// [`ha_cluster`]와 같고 두 노드의 설정이 `config`다.
async fn ha_cluster_with(name: &str, config: &str, extra_a: &str) -> (Daemon, Daemon, Daemon) {
    let mut a = Daemon::with_hypervisor(
        "node-a",
        &format!("{config}{extra_a}"),
        "systemd_scope = false",
    );
    let mut b = Daemon::with_hypervisor("node-b", config, "systemd_scope = false");
    let mut w = Daemon::witness("wit");
    a.start().await;
    b.start().await;
    w.start().await;
    a.init(name).await;
    a.login().await;
    let url = a.join_url.clone().unwrap();
    b.join(&url).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;
    let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();
    let ca = client(&a);
    eventually("both nodes online and HA capable", || async {
        let nodes = ca.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap().clone();
        nodes.len() == 3
            && nodes.iter().all(|n| n["status"]["online"] == true)
            && nodes
                .iter()
                .filter(|n| n["spec"]["witness"] != true)
                .all(|n| n["status"]["ha"]["fencing"] == "process")
    })
    .await;
    (a, b, w)
}

#[tokio::test]
async fn a_host_poweroff_with_failover_stops_the_ha_vm_and_another_node_runs_it() {
    // 호스트 끄기(`[test] host_shutdown`) + `failover`: A가 끝나기 전에 HA VM만 끄고, 리더가 B에서 켠다. HA 아닌
    // VM은 그대로 돈다.
    let (mut a, b, _w) = ha_cluster("ha-off", "[test]\nhost_shutdown = \"poweroff\"\n").await;
    let (ca, cb) = (client(&a), client(&b));
    let (id_a, id_b) = (node_id(&ca, "node-a").await, node_id(&ca, "node-b").await);
    a.ctl(&["settings", "set", "shutdown_policy=failover"])
        .await
        .unwrap();
    let shared = tempfile::Builder::new()
        .prefix("oplhaoff")
        .tempdir_in("/tmp")
        .unwrap();
    a.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    a.wait_pool_ready("shared").await;
    b.wait_pool_ready("shared").await;
    a.ctl(&[
        "volume", "create", "db-root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    a.ctl(&[
        "vm", "create", "db", "--memory", "256", "--disk", "db-root", "--node", "node-a", "--ha",
        "--start",
    ])
    .await
    .unwrap();
    a.ctl(&[
        "vm", "create", "plain", "--memory", "256", "--node", "node-a", "--start",
    ])
    .await
    .unwrap();
    let db_pid = running_pid(&ca, "db", &id_a).await;
    let plain_pid = running_pid(&ca, "plain", &id_a).await;

    // SIGTERM: A가 끝났을 때 HA VM의 QEMU는 이미 꺼졌다.
    a.stop();
    assert!(!alive(db_pid), "the HA VM still runs after node A stopped");
    assert!(alive(plain_pid));
    eventually("node A marked failover and offline", || async {
        let node = cb.get("/nodes/node-a").await.unwrap();
        node["status"]["shutdown"]["action"] == "failover" && node["status"]["online"] == false
    })
    .await;

    eventually_within("HA VM running on node B", RECOVER_WAIT, || async {
        let Ok(vm) = cb.get("/vms/db").await else {
            return false;
        };
        vm["spec"]["node"] == id_b.as_str()
            && vm["status"]["phase"] == "running"
            && vm["status"]["node"] == id_b.as_str()
    })
    .await;
    let plain = cb.get("/vms/plain").await.unwrap();
    assert_eq!(plain["spec"]["node"], id_a.as_str(), "{plain}");

    kill_qemu(plain_pid).await;
    b.ctl(&["vm", "stop", "db", "--force"]).await.unwrap();
    b.ctl(&["vm", "delete", "db"]).await.unwrap();
    b.ctl(&["volume", "delete", "db-root"]).await.unwrap();
    b.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

#[tokio::test]
async fn a_host_reboot_with_the_default_policy_freezes_the_ha_vm_on_its_node() {
    // 호스트 재부팅 + 기본 정책(`conditional` → `freeze`): A가 HA VM을 끄고 얼린 표시를 남긴다. 리더는
    // `recover_after`가 지나도 옮기지 않고, A가 돌아오면 A에서 다시 켠다.
    let (mut a, b, _w) = ha_cluster("ha-reboot", "[test]\nhost_shutdown = \"reboot\"\n").await;
    let (ca, cb) = (client(&a), client(&b));
    let id_a = node_id(&ca, "node-a").await;
    a.ctl(&[
        "vm", "create", "db", "--memory", "256", "--node", "node-a", "--ha", "--start",
    ])
    .await
    .unwrap();
    let db_pid = running_pid(&ca, "db", &id_a).await;

    a.stop();
    assert!(!alive(db_pid), "the HA VM still runs after node A stopped");
    eventually("node A frozen and offline", || async {
        let node = cb.get("/nodes/node-a").await.unwrap();
        node["status"]["shutdown"]["action"] == "freeze" && node["status"]["online"] == false
    })
    .await;

    // recover_after(4초)와 리더 선거를 넉넉히 넘겨도 그대로다.
    let until = Instant::now() + Duration::from_secs(25);
    while Instant::now() < until {
        let vm = cb.get("/vms/db").await.unwrap();
        assert_eq!(vm["spec"]["node"], id_a.as_str(), "{vm}");
        assert_eq!(vm["spec"]["power"], "running", "{vm}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let tasks = cb.get("/tasks").await.unwrap();
    assert!(
        !tasks["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["spec"]["op"]["kind"] == "vm-ha-recover"),
        "{tasks}"
    );

    // A가 돌아오면 표시를 지우고 A에서 다시 켠다.
    // (꺼질 때 reconciler가 멈춰 있어 status는 옛 pid를 담고 있다.)
    a.start().await;
    let deadline = Instant::now() + RECOVER_WAIT;
    let pid = loop {
        let pid = running_pid(&cb, "db", &id_a).await;
        if pid != db_pid {
            break pid;
        }
        assert!(Instant::now() < deadline, "db not started again on node A");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert!(alive(pid));
    let node = cb.get("/nodes/node-a").await.unwrap();
    assert!(node["status"].get("shutdown").is_none(), "{node}");

    a.ctl(&["vm", "stop", "db", "--force"]).await.unwrap();
    a.ctl(&["vm", "delete", "db"]).await.unwrap();
}

#[tokio::test]
async fn a_host_poweroff_with_migrate_drains_the_node_before_stopping() {
    // 호스트 끄기 + `migrate`: A가 끝나기 전에 유지보수로 바꾸고 비우기로 HA VM을 B로 라이브 마이그레이션한다.
    // A가 돌아오면 정책이 켠 유지보수를 끈다(이 Mac에서는 TCG).
    let daemon = |name: &str, extra: &str, ports: &str| {
        Daemon::with_hypervisor(
            name,
            &format!("{CONFIG}{extra}[migration]\nports = \"{ports}\"\n"),
            migrating_hypervisor(),
        )
    };
    let mut a = daemon(
        "node-a",
        "[test]\nhost_shutdown = \"poweroff\"\n",
        "49650-49659",
    );
    let mut b = daemon("node-b", "", "49660-49669");
    let mut w = Daemon::witness("wit");
    a.start().await;
    b.start().await;
    w.start().await;
    a.init("ha-mig").await;
    a.login().await;
    let url = a.join_url.clone().unwrap();
    b.join(&url).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;
    let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();
    let (ca, cb) = (client(&a), client(&b));
    eventually("both nodes online and HA capable", || async {
        let nodes = ca.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap().clone();
        nodes.len() == 3
            && nodes.iter().all(|n| n["status"]["online"] == true)
            && nodes
                .iter()
                .filter(|n| n["spec"]["witness"] != true)
                .all(|n| n["status"]["ha"]["fencing"] == "process")
    })
    .await;
    let (id_a, id_b) = (node_id(&ca, "node-a").await, node_id(&ca, "node-b").await);
    a.ctl(&["settings", "set", "shutdown_policy=migrate"])
        .await
        .unwrap();
    let shared = tempfile::Builder::new()
        .prefix("oplhamig")
        .tempdir_in("/tmp")
        .unwrap();
    a.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    a.wait_pool_ready("shared").await;
    b.wait_pool_ready("shared").await;
    a.ctl(&[
        "volume", "create", "db-root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    a.ctl(&[
        "vm", "create", "db", "--memory", "256", "--disk", "db-root", "--node", "node-a", "--ha",
        "--start",
    ])
    .await
    .unwrap();
    let db_pid = running_pid(&ca, "db", &id_a).await;

    // SIGTERM: A가 끝나기 전에 HA VM이 B로 옮겨 가 돈다(복구가 아니라 마이그레이션).
    a.stop();
    assert!(!alive(db_pid));
    eventually("HA VM assigned to node B", || async {
        cb.get("/vms/db").await.unwrap()["spec"]["node"] == id_b.as_str()
    })
    .await;
    let pid = running_pid(&cb, "db", &id_b).await;
    assert!(alive(pid));
    let tasks = cb.get("/tasks").await.unwrap();
    let kinds: Vec<&Value> = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["spec"]["op"]["kind"])
        .collect();
    assert!(kinds.iter().any(|k| *k == "node-drain"), "{tasks}");
    assert!(kinds.iter().any(|k| *k == "vm-migrate"), "{tasks}");
    assert!(!kinds.iter().any(|k| *k == "vm-ha-recover"), "{tasks}");
    eventually("node A marked migrate and in maintenance", || async {
        let node = cb.get("/nodes/node-a").await.unwrap();
        node["status"]["shutdown"]["action"] == "migrate"
            && node["status"]["shutdown"]["maintenance"] == true
            && node["spec"]["maintenance"] == true
            && node["status"]["online"] == false
    })
    .await;

    // A가 돌아오면 표시를 지우고 정책이 켠 유지보수를 끈다.
    a.start().await;
    eventually("node A back without maintenance", || async {
        let node = cb.get("/nodes/node-a").await.unwrap();
        node["status"]["online"] == true
            && node["spec"]["maintenance"] == false
            && node["status"].get("shutdown").is_none()
    })
    .await;

    b.ctl(&["vm", "stop", "db", "--force"]).await.unwrap();
    b.ctl(&["vm", "delete", "db"]).await.unwrap();
    b.ctl(&["volume", "delete", "db-root"]).await.unwrap();
    b.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// 기본(`fast`) 프로필의 노드: 프로세스 펜싱만 정한다(리스 10초, recover_after 10 + 2 = 12초, 갱신 1초).
const FAST_CONFIG: &str = "[reconciler]\nacpi_timeout_secs = 5\n[ha]\nfencing = \"process\"\n";

/// 기본 프로필에서 노드 장애부터 다른 노드에서 VM이 켜지기까지의 한도(설계 §8.1)
const FAST_RECOVERY: Duration = Duration::from_secs(25);

/// 제어 소켓의 시험 고리(`POST /v1/test/…`)를 부른다.
async fn test_hook(d: &Daemon, path: &str) {
    let http = reqwest::Client::builder()
        .unix_socket(d.path("opld.sock"))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = http
        .post(format!("http://opld/v1/test/{path}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT, "{path}");
}

/// pid에 신호를 보낸다.
fn signal(pid: u64, sig: Signal) {
    kill_process(Pid::from_raw(pid as i32).unwrap(), sig).unwrap();
}

#[tokio::test]
async fn recovery_takes_at_most_25s_with_the_default_profile() {
    // 리더가 아닌 노드에서 도는 HA VM을 세 가지 장애에서 리더가 다른 노드로 옮겨 켜기까지 잰다. 리스 갱신이 끊긴
    // 뒤 recover_after(12초) + 리더의 틱 + QEMU 시작이다. 디스크 없는 VM을 쓴다: 같은 호스트의 공유 디렉터리
    // 디스크는 SIGSTOP한 QEMU가 이미지 잠금을 쥐고 있어 다른 노드가 켤 수 없다(실제로는 다른 호스트라 펜싱된
    // 노드의 잠금이 남지 않는다). 장애 뒤 다시 이어진 노드의 리더가 바뀔 수 있으므로 회차마다 리더가 아닌
    // 노드를 장애 노드로 고른다(위트니스는 리더가 되지 않는다).
    let (a, b, _w) = ha_cluster_with("ha-fast", FAST_CONFIG, "").await;
    let mut nodes = [a, b];
    let clients = [client(&nodes[0]), client(&nodes[1])];
    let ids = [
        node_id(&clients[0], "node-a").await,
        node_id(&clients[0], "node-b").await,
    ];
    let mut took = Vec::new();
    for (round, fault) in ["kill -9", "isolate", "SIGSTOP"].into_iter().enumerate() {
        let deadline = Instant::now() + RECOVER_WAIT;
        let leader = loop {
            if let Ok(c) = clients[0].get("/cluster").await
                && let Some(leader) = c["leader"].as_str()
            {
                break leader.to_string();
            }
            assert!(Instant::now() < deadline, "no leader");
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let survivor = ids.iter().position(|id| *id == leader).unwrap();
        let victim = 1 - survivor;
        let (cs, sid, vid) = (&clients[survivor], &ids[survivor], &ids[victim]);
        let vname = nodes[victim].name.clone();
        let vm = format!("fast{round}");
        nodes[survivor]
            .ctl(&[
                "vm", "create", &vm, "--memory", "256", "--node", &vname, "--ha", "--start",
            ])
            .await
            .unwrap();
        let qemu = running_pid(cs, &vm, vid).await;
        let opld = u64::from(nodes[victim].pid().unwrap());

        let start = Instant::now();
        match fault {
            // 노드가 죽는다: opld를 먼저(QEMU를 다시 띄우지 않게), 그 다음 그 QEMU
            "kill -9" => {
                nodes[victim].kill();
                kill_qemu(qemu).await;
            }
            // 노드 간 통신만 끊긴다: 장애 노드가 리스를 잃고 스스로 QEMU를 끈다.
            "isolate" => test_hook(&nodes[victim], "isolate").await,
            // 노드가 멈춘다(opld와 QEMU 모두)
            _ => {
                signal(opld, Signal::STOP);
                signal(qemu, Signal::STOP);
            }
        }
        let deadline = start + RECOVER_WAIT;
        loop {
            if let Ok(v) = cs.get(&format!("/vms/{vm}")).await
                && v["spec"]["node"] == sid.as_str()
                && v["status"]["phase"] == "running"
                && v["status"]["node"] == sid.as_str()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{fault}: {vm} did not start on the other node"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let elapsed = start.elapsed();
        eprintln!("HA recovery after {fault} of {vname}: {elapsed:?}");
        took.push((fault, elapsed));

        // 장애 노드를 되살린다. 리스를 잃은 노드는 이어진 뒤에도 옛 QEMU를 끈 채 둔다.
        match fault {
            "kill -9" => nodes[victim].start().await,
            "isolate" => {
                eventually("the isolated node fenced its QEMU", || async {
                    !alive(qemu)
                })
                .await;
                test_hook(&nodes[victim], "heal").await;
            }
            _ => {
                signal(opld, Signal::CONT);
                signal(qemu, Signal::CONT);
                eventually_within("the resumed node fenced its QEMU", RECOVER_WAIT, || async {
                    !alive(qemu)
                })
                .await;
            }
        }
        eventually_within("the failed node back online", RECOVER_WAIT, || async {
            let Ok(node) = cs.get(&format!("/nodes/{vname}")).await else {
                return false;
            };
            node["status"]["online"] == true && node["status"]["ha"]["fencing"] == "process"
        })
        .await;
        assert!(!alive(qemu));
        let v = cs.get(&format!("/vms/{vm}")).await.unwrap();
        assert_eq!(v["spec"]["node"], sid.as_str(), "{v}");
        nodes[survivor]
            .ctl(&["vm", "stop", &vm, "--force"])
            .await
            .unwrap();
        nodes[survivor].ctl(&["vm", "delete", &vm]).await.unwrap();
    }
    for (fault, elapsed) in took {
        assert!(
            elapsed <= FAST_RECOVERY,
            "recovery after {fault} took {elapsed:?}"
        );
    }
}
