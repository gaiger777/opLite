//! 실제 HA 복구: 한 호스트의 opld 둘(`[ha] fencing = "process"`)과 위트니스로 클러스터를 만들고, 노드 A에서
//! 도는 HA VM과 HA 아닌 VM을 둔 채 A의 opld와 그 QEMU들을 kill -9 한다. 리더가 A의 리스가 끝난 것을 보고
//! HA VM만 노드 B로 옮겨 켜는지(기록 Task `vm-ha-recover`), A를 다시 띄워도 그 VM을 건드리지 않는지 본다.
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

/// 복구를 기다리는 시간. A의 recover_after(fence_after 2초 + 여유 5초 = 7초), 리더 선거, 대상 노드의
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
