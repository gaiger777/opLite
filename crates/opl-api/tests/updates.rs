//! 노드 업데이트 확인과 업그레이드: 다시 확인 Task(가짜 apt), 요청 검사, 소스 파일과 주기 확인 에이전트,
//! 업그레이드 Task(가짜 systemd-run·systemctl·dpkg).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_apt::FakeApt;
use common::{Env, MOUNT_ROOT, WAIT};
use opl_api::{UpdateAgent, Updates};
use opl_driver::DirStorage;
use opl_types::node::{NodeSpec, NodeUpdates};
use opl_types::resource::Resource;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 노드의 status를 `change`로 고쳐 쓴다.
async fn set_node_status(env: &Env, change: impl FnOnce(&mut opl_types::node::NodeStatus)) {
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status;
    change(&mut status);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 업데이트가 켜진 노드(등록 때처럼 빈 `updates`)와 가짜 apt를 쓰는 Task 실행기
async fn env_with_updates(apt: &FakeApt) -> Env {
    let mut env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    env.stop_runner().await;
    let updates = Arc::new(Updates::new(apt.config()));
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.updates = Some(updates)
    })
    .await;
    env
}

async fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn refresh_runs_a_task_that_records_the_updates() {
    let apt = FakeApt::new();
    apt.set(
        "simulate.txt",
        "Reading package lists...\n\
         Inst orbivisor [0.2.0] (0.2.1 opLite:trixie [amd64])\n\
         Conf orbivisor (0.2.1 opLite:trixie [amd64])\n\
         Inst libnew1 (1.0-1 Debian:13.2/stable [amd64])\n",
    );
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (status, task) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"], json!({ "kind": "node-update-check" }));
    assert_eq!(task["spec"]["target"], env.node.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");

    assert_eq!(
        apt.calls(),
        [
            "LANG=C update -q -o DPkg::Lock::Timeout=60",
            "LANG=C -s -q -o DPkg::Lock::Timeout=60 dist-upgrade"
        ]
    );
    let (_, node) = admin.get("/nodes/node1").await;
    let updates = &node["status"]["updates"];
    assert!(updates["checked_at"].as_u64().is_some(), "{updates}");
    assert_eq!(
        updates["packages"],
        json!([
            { "name": "orbivisor", "current": "0.2.0", "candidate": "0.2.1" },
            { "name": "libnew1", "current": null, "candidate": "1.0-1" }
        ])
    );
    assert_eq!(updates["error"], Value::Null);
    assert!(updates.get("truncated").is_none(), "{updates}");
}

#[tokio::test]
async fn a_failed_apt_command_fails_the_task_and_records_the_error() {
    let apt = FakeApt::new();
    apt.set("update.exit", "100");
    apt.set(
        "update.stderr",
        "W: line 1\nW: line 2\nE: line 3\nE: line 4\nE: line 5\nE: line 6\n\
         E: Could not get lock /var/lib/apt/lists/lock. It is held by process 4242 (apt-get)\n",
    );
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (status, task) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    let error = done["status"]["error"].as_str().unwrap();
    assert!(
        error.starts_with("apt-get update -q -o DPkg::Lock::Timeout=60 failed ("),
        "{error}"
    );
    // stderr의 끝 5줄만 담는다.
    assert!(
        error.ends_with(
            "): E: line 3\nE: line 4\nE: line 5\nE: line 6\n\
             E: Could not get lock /var/lib/apt/lists/lock. It is held by process 4242 (apt-get)"
        ),
        "{error}"
    );
    // 시뮬레이션까지 가지 않는다.
    assert_eq!(apt.calls(), ["LANG=C update -q -o DPkg::Lock::Timeout=60"]);

    let (_, node) = admin.get("/nodes/node1").await;
    let updates = &node["status"]["updates"];
    assert_eq!(updates["error"], error, "{updates}");
    assert_eq!(updates["packages"], json!([]));
    assert!(updates["checked_at"].as_u64().is_some(), "{updates}");
}

#[tokio::test]
async fn refresh_checks_the_node_and_the_caller() {
    let apt = FakeApt::new();
    let env = Env::start().await;
    let admin = env.admin().await;

    // 등록 때 업데이트가 꺼졌던 노드(위트니스, macOS 등)
    assert_error(
        &admin.post("/nodes/node1/updates/refresh", json!({})).await,
        StatusCode::BAD_REQUEST,
        "updates_unsupported",
    );
    assert_error(
        &admin.post("/nodes/nope/updates/refresh", json!({})).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    for role in ["operator", "viewer"] {
        let user = env.user(role, role).await;
        assert_error(
            &user.post("/nodes/node1/updates/refresh", json!({})).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    set_node_status(&env, |s| s.online = false).await;
    assert_error(
        &admin.post("/nodes/node1/updates/refresh", json!({})).await,
        StatusCode::CONFLICT,
        "node_offline",
    );
    // 실행기에 업데이트가 없으면(설정과 등록이 어긋남) Task가 실패한다.
    set_node_status(&env, |s| s.online = true).await;
    let (status, task) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        "updates are not enabled on this node"
    );
    assert!(apt.calls().is_empty());
}

#[tokio::test]
async fn refresh_returns_the_unfinished_check() {
    let apt = FakeApt::new();
    let mut env = env_with_updates(&apt).await;
    let admin = env.admin().await;
    // 실행기가 멈춰 있으면 Task는 pending으로 남는다.
    env.stop_runner().await;
    let (status, first) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    let (status, again) = admin
        .post(&format!("/nodes/{}/updates/refresh", env.node), json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{again}");
    assert_eq!(again["id"], first["id"]);

    let updates = Arc::new(Updates::new(apt.config()));
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.updates = Some(updates)
    })
    .await;
    let done = admin.wait_task(&first).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    // 끝났으면 새로 만든다.
    let (_, next) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_ne!(next["id"], first["id"]);
    assert!(Uuid::try_parse(next["id"].as_str().unwrap()).is_ok());
}

/// 버전을 적지 않은 옛 노드가 있으면 옛 노드가 풀지 못하는 확인 Task를 만들지 않는다.
#[tokio::test]
async fn refresh_waits_until_every_node_is_upgraded() {
    let apt = FakeApt::new();
    let env = env_with_updates(&apt).await;
    let old = opl_types::resource::Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    env.cluster.create(&old).await.unwrap();
    assert_error(
        &env.admin()
            .await
            .post("/nodes/node1/updates/refresh", json!({}))
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    let (_, tasks) = env.cluster.list::<opl_types::task::TaskSpec>().unwrap();
    assert!(tasks.is_empty());
}

/// 이 필드를 모르는 옛 리더가 지운 버전과 `updates`를 에이전트가 시작할 때와 확인할 때 되살린다.
#[tokio::test]
async fn the_agent_restores_the_version_and_updates() {
    let apt = FakeApt::new();
    let env = Env::start().await;
    set_node_status(&env, |s| {
        s.version = None;
        s.updates = None;
    })
    .await;
    let mut config = apt.config();
    config.first_check = Duration::from_millis(300);
    let agent = UpdateAgent::spawn(
        env.cluster.clone(),
        env.node,
        Arc::new(Updates::new(config)),
    );
    let status = || {
        env.cluster
            .get::<NodeSpec>(env.node)
            .unwrap()
            .unwrap()
            .status
    };
    eventually("the restored status", || {
        let s = status();
        (s.version.as_deref() == Some(opl_types::version::BUILD)
            && s.updates == Some(NodeUpdates::default()))
        .then_some(())
    })
    .await;
    // 확인할 때도 되살린다.
    set_node_status(&env, |s| {
        s.version = None;
        s.updates = None;
    })
    .await;
    eventually("the first check", || {
        let s = status();
        (s.version.as_deref() == Some(opl_types::version::BUILD)
            && s.updates.is_some_and(|u| u.checked_at.is_some()))
        .then_some(())
    })
    .await;
    agent.shutdown().await;
}

#[tokio::test]
async fn the_agent_writes_the_sources_for_the_channel_and_checks() {
    let apt = FakeApt::new();
    apt.set(
        "simulate.txt",
        "Inst orbivisor [0.2.0] (0.2.1 opLite:trixie [amd64])\n",
    );
    let env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    let admin = env.admin().await;
    let mut config = apt.config();
    config.apt_url = "https://apt.example.com/orbivisor".into();
    config.first_check = Duration::from_millis(50);
    let sources = config.sources_file.clone();
    let agent = UpdateAgent::spawn(
        env.cluster.clone(),
        env.node,
        Arc::new(Updates::new(config)),
    );

    let read = || std::fs::read_to_string(&sources).ok();
    let text = eventually("the sources file", read).await;
    assert!(
        text.starts_with("# managed by opld; edits are overwritten\n"),
        "{text}"
    );
    assert!(
        text.contains("URIs: https://apt.example.com/orbivisor\n"),
        "{text}"
    );
    assert!(text.contains("Components: stable\n"), "{text}");

    // 첫 확인
    eventually("the first check", || {
        let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
        node.status
            .updates
            .filter(|u| u.checked_at.is_some())
            .map(|u| assert_eq!(u.packages[0].candidate, "0.2.1"))
    })
    .await;

    // 채널을 바꾸면 소스 파일이 따라간다.
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    eventually("the testing channel", || {
        read().filter(|t| t.contains("Components: testing\n"))
    })
    .await;
    // 새 채널은 다음 주기(한 시간 뒤)를 기다리지 않고 곧 확인한다.
    eventually("a check of the new channel", || {
        (apt.calls().len() == 4).then_some(())
    })
    .await;
    agent.shutdown().await;
}

/// 개발 키로 서명한 구독 키(노드 3대, 30일)
fn subscription_key() -> String {
    use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    sign(
        &decode_private_key(include_str!(
            "../../opl-types/tests/fixtures/dev-license.key"
        ))
        .unwrap(),
        &LicensePayload {
            id: "sub-1".into(),
            customer: "Example Corp".into(),
            level: Level::Standard,
            nodes: 3,
            issued_at: now - 60,
            expires_at: now + 30 * 86_400,
        },
    )
    .unwrap()
}

/// enterprise 채널이고 구독이 있으면 APT 인증 파일을 쓰고, 구독을 지우거나 채널을 바꾸면 지운다.
#[tokio::test]
async fn the_agent_writes_the_apt_credentials_for_the_enterprise_channel() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    let apt = FakeApt::new();
    let env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    let admin = env.admin().await;
    let mut config = apt.config();
    config.apt_url = "https://apt.example.com:8443/orbivisor".into();
    let sources = config.sources_file.clone();
    let auth = config.auth_file.clone();
    let agent = UpdateAgent::spawn(
        env.cluster.clone(),
        env.node,
        Arc::new(Updates::new(config)),
    );
    let read_sources = || std::fs::read_to_string(&sources).ok();
    let read_auth = || std::fs::read_to_string(&auth).ok();
    eventually("the sources file", read_sources).await;
    assert_eq!(read_auth(), None);

    let k = subscription_key();
    let (status, got) = admin.put("/subscription", json!({ "key": k }), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "enterprise" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    eventually("the enterprise channel", || {
        read_sources().filter(|t| t.contains("Components: enterprise\n"))
    })
    .await;
    let expected = format!(
        "# managed by opld; edits are overwritten\n\
         machine apt.example.com:8443 login sub-1 password {}\n",
        hex::encode(Sha256::digest(k.as_bytes()))
    );
    let text = eventually("the APT credentials", read_auth).await;
    assert_eq!(text, expected);
    let mode = std::fs::metadata(&auth).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // 구독을 지우면 지우고(채널은 그대로), 다시 넣으면 다시 쓴다.
    assert_eq!(admin.delete("/subscription").await.0, StatusCode::OK);
    eventually("the removed credentials", || {
        read_auth().is_none().then_some(())
    })
    .await;
    let (status, got) = admin.put("/subscription", json!({ "key": k }), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        eventually("the credentials again", read_auth).await,
        expected
    );

    // 다른 채널로 바꾸면 지운다.
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "stable" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    eventually("the stable channel", || {
        read_sources().filter(|t| t.contains("Components: stable\n"))
    })
    .await;
    eventually("the removed credentials", || {
        read_auth().is_none().then_some(())
    })
    .await;
    agent.shutdown().await;
}

/// 실행기가 업그레이드 유닛을 지켜보기 시작할 때까지(가짜 `systemctl`이 불릴 때까지) 기다린다.
async fn wait_for_polling(apt: &FakeApt) {
    eventually("the unit to be polled", || {
        (!apt.lines("systemctl.calls").is_empty()).then_some(())
    })
    .await;
}

/// 업그레이드 Task의 유닛 이름과 로그 파일
fn unit_and_log(apt: &FakeApt, task: &Value) -> (String, std::path::PathBuf) {
    let id = task["id"].as_str().unwrap();
    (
        format!("opl-upgrade-{}", id.replace('-', "")),
        apt.path("log").join(format!("upgrade-{id}.log")),
    )
}

/// 업그레이드 Task `id`의 로그를 쓴다(이전 opld 때 유닛이 남긴 것처럼).
fn write_log(apt: &FakeApt, id: Uuid, text: &str) {
    std::fs::create_dir_all(apt.path("log")).unwrap();
    std::fs::write(apt.path("log").join(format!("upgrade-{id}.log")), text).unwrap();
}

#[tokio::test]
async fn upgrade_runs_apt_in_a_transient_unit_and_checks_afterwards() {
    let apt = FakeApt::new();
    apt.set(
        "simulate.txt",
        "Inst libnew1 (1.0-1 Debian:13.2/stable [amd64])\n",
    );
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (status, task) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"], json!({ "kind": "node-upgrade" }));
    assert_eq!(task["spec"]["target"], env.node.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
    wait_for_polling(&apt).await;

    let (unit, log) = unit_and_log(&apt, &task);
    let apt_get = apt.path("apt-get").display().to_string();
    assert_eq!(
        apt.lines("systemd-run.args"),
        [
            format!("--unit={unit}"),
            "--collect".into(),
            "--property=Type=exec".into(),
            format!("--property=StandardOutput=append:{}", log.display()),
            format!("--property=StandardError=append:{}", log.display()),
            "--setenv=DEBIAN_FRONTEND=noninteractive".into(),
            "--setenv=LANG=C".into(),
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "echo \"opl-upgrade: start\"; \
                 '{apt_get}' update -q -o DPkg::Lock::Timeout=600 && \
                 '{apt_get}' -y -q -o DPkg::Lock::Timeout=600 \
                 -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade; \
                 s=$?; echo \"opl-upgrade: exit $s\"; exit $s"
            ),
        ]
    );
    // 로그 디렉터리를 만든다(systemd가 로그 파일을 그 안에 만든다).
    assert!(apt.path("log").is_dir());
    assert_eq!(
        apt.lines("systemctl.calls")[0],
        format!("show -p ActiveState -p Result -p LoadState {unit}.service")
    );
    // 유닛이 도는 동안은 끝나지 않는다. 업데이트 확인도 아직 하지 않는다.
    let (_, now) = admin
        .get(&format!("/tasks/{}", task["id"].as_str().unwrap()))
        .await;
    assert_eq!(now["status"]["state"], "running", "{now}");
    assert!(apt.calls().is_empty());
    // 업그레이드가 apt 잠금을 잡고 있는 동안은 다시 확인하지 않는다.
    let got = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_error(&got, StatusCode::CONFLICT, "upgrade_in_progress");
    assert_eq!(
        got.1["message"],
        "node \"node1\" is being upgraded; it checks for updates when the upgrade ends"
    );

    apt.set("unit.state", "success");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    // 끝나면 업데이트를 다시 확인한다.
    assert_eq!(
        apt.calls(),
        [
            "LANG=C update -q -o DPkg::Lock::Timeout=60",
            "LANG=C -s -q -o DPkg::Lock::Timeout=60 dist-upgrade"
        ]
    );
    let (_, node) = admin.get("/nodes/node1").await;
    assert_eq!(
        node["status"]["updates"]["packages"],
        json!([{ "name": "libnew1", "current": null, "candidate": "1.0-1" }])
    );
    assert!(apt.lines("dpkg.calls").is_empty());
}

/// 업그레이드가 확인 잠금을 잡은 동안 (막히기 전에 만들어진) 다시 확인 Task는 기다리지 않고 성공으로
/// 끝난다. 확인은 업그레이드가 끝날 때 한 번 한다.
#[tokio::test]
async fn a_check_task_during_an_upgrade_succeeds_without_checking() {
    let apt = FakeApt::new();
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;
    let (status, upgrade) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{upgrade}");
    wait_for_polling(&apt).await;

    let mut check: Task = Resource::new(
        "check",
        TaskSpec {
            op: TaskOp::NodeUpdateCheck,
            target: env.node,
            node: env.node,
        },
    );
    check.status.state = TaskState::Pending;
    env.cluster.create(&check).await.unwrap();
    let done = admin
        .wait_task(&json!({ "id": check.id.to_string() }))
        .await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert!(apt.calls().is_empty(), "{:?}", apt.calls());

    apt.set("unit.state", "success");
    let done = admin.wait_task(&upgrade).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert_eq!(apt.calls().len(), 2);
}

#[tokio::test]
async fn a_failed_upgrade_fails_the_task_with_the_last_log_lines() {
    let apt = FakeApt::new();
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (status, task) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    wait_for_polling(&apt).await;
    let (_, log) = unit_and_log(&apt, &task);
    let lines: Vec<String> = (1..=25).map(|i| format!("line {i}")).collect();
    std::fs::write(&log, lines.join("\n") + "\n").unwrap();
    apt.set("unit.state", "failed");

    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        format!(
            "the upgrade failed; last log lines:\n{}",
            lines[5..].join("\n")
        )
    );
    // 실패해도 업데이트를 다시 확인한다.
    assert_eq!(
        apt.calls(),
        [
            "LANG=C update -q -o DPkg::Lock::Timeout=60",
            "LANG=C -s -q -o DPkg::Lock::Timeout=60 dist-upgrade"
        ]
    );
}

#[tokio::test]
async fn upgrade_fails_when_systemd_run_fails() {
    let apt = FakeApt::new();
    apt.set("systemd-run.exit", "1");
    apt.set(
        "systemd-run.stderr",
        "Failed to start transient service unit: Access denied\n",
    );
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (_, task) = admin.post("/nodes/node1/upgrade", json!({})).await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    let error = done["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("systemd-run failed ("), "{error}");
    assert!(
        error.ends_with("): Failed to start transient service unit: Access denied"),
        "{error}"
    );
    assert!(apt.lines("systemctl.calls").is_empty());
}

#[tokio::test]
async fn upgrade_checks_the_node_the_caller_and_other_upgrades() {
    let apt = FakeApt::new();
    let mut env = Env::start().await;
    let admin = env.admin().await;

    assert_error(
        &admin.post("/nodes/node1/upgrade", json!({})).await,
        StatusCode::BAD_REQUEST,
        "updates_unsupported",
    );
    assert_error(
        &admin.post("/nodes/nope/upgrade", json!({})).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    for role in ["operator", "viewer"] {
        let user = env.user(role, role).await;
        assert_error(
            &user.post("/nodes/node1/upgrade", json!({})).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    set_node_status(&env, |s| s.online = false).await;
    assert_error(
        &admin.post("/nodes/node1/upgrade", json!({})).await,
        StatusCode::CONFLICT,
        "node_offline",
    );
    set_node_status(&env, |s| s.online = true).await;

    // 한 번에 한 노드: 이 노드의 업그레이드가 끝나지 않았으면 409
    env.stop_runner().await;
    let (status, first) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    let (status, body) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_error(
        &(status, body.clone()),
        StatusCode::CONFLICT,
        "upgrade_in_progress",
    );
    assert_eq!(
        body["message"],
        "node \"node1\" is still being upgraded; upgrade one node at a time"
    );
    // 다른 노드의 업그레이드도 막는다.
    let id = first["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    env.cluster.delete::<TaskSpec>(id, None).await.unwrap();
    let other = Uuid::now_v7();
    let mut task: Task = Resource::new(
        "other",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: other,
            node: other,
        },
    );
    task.status.state = TaskState::Running;
    env.cluster.create(&task).await.unwrap();
    assert_error(
        &admin.post("/nodes/node1/upgrade", json!({})).await,
        StatusCode::CONFLICT,
        "upgrade_in_progress",
    );
    // 끝난 업그레이드는 막지 않는다.
    env.cluster
        .update_status::<TaskSpec>(
            task.id,
            &TaskStatus {
                state: TaskState::Succeeded,
                error: None,
                progress: None,
            },
        )
        .await
        .unwrap();
    // 버전을 적지 않은 옛 노드가 있으면 옛 노드가 풀지 못하는 Task를 만들지 않는다.
    let old = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    env.cluster.create(&old).await.unwrap();
    assert_error(
        &admin.post("/nodes/node1/upgrade", json!({})).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    env.cluster.delete::<NodeSpec>(old.id, None).await.unwrap();
    let (status, body) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(apt.lines("systemd-run.args").is_empty());
}

/// 두 요청이 함께 지나가 업그레이드 Task가 둘 생겨도, 다른 업그레이드가 running이면 실행기가 새
/// 업그레이드를 시작하지 않는다.
#[tokio::test]
async fn the_runner_refuses_an_upgrade_while_another_runs() {
    let apt = FakeApt::new();
    let mut env = env_with_updates(&apt).await;
    env.stop_runner().await;
    let other = Uuid::now_v7();
    let mut running: Task = Resource::new(
        "other",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: other,
            node: other,
        },
    );
    running.status.state = TaskState::Running;
    env.cluster.create(&running).await.unwrap();
    let mut task: Task = Resource::new(
        "mine",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: env.node,
            node: env.node,
        },
    );
    task.status.state = TaskState::Pending;
    env.cluster.create(&task).await.unwrap();
    let updates = Arc::new(Updates::new(apt.config()));
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.updates = Some(updates)
    })
    .await;
    let done = env
        .admin()
        .await
        .wait_task(&json!({ "id": task.id.to_string() }))
        .await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        format!("upgrade_in_progress: node \"{other}\" is being upgraded")
    );
    assert!(apt.lines("systemd-run.args").is_empty());
}

/// 이전 opld가 지켜보던(running) 업그레이드 Task를 만들고, `prepare(Task id)` 뒤 가짜 apt를 쓰는
/// 실행기를 띄운다.
async fn resume_upgrade(apt: &FakeApt, prepare: impl FnOnce(Uuid)) -> (Env, Value) {
    let mut env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    env.stop_runner().await;
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: env.node,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    task.status.state = TaskState::Running;
    env.cluster.create(&task).await.unwrap();
    prepare(task.id);
    let updates = Arc::new(Updates::new(apt.config()));
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.updates = Some(updates)
    })
    .await;
    let task = json!({ "id": task.id.to_string() });
    (env, task)
}

#[tokio::test]
async fn a_restarted_opld_keeps_watching_the_upgrade_unit() {
    let apt = FakeApt::new();
    apt.set("unit.state", "active");
    let (env, task) = resume_upgrade(&apt, |_| {}).await;
    let admin = env.admin().await;
    wait_for_polling(&apt).await;
    let (unit, _) = unit_and_log(&apt, &task);
    assert_eq!(
        apt.lines("systemctl.calls")[0],
        format!("show -p ActiveState -p Result -p LoadState {unit}.service")
    );
    // 실패로 정리하지 않고, 유닛을 다시 띄우지도 않는다.
    let (_, now) = admin
        .get(&format!("/tasks/{}", task["id"].as_str().unwrap()))
        .await;
    assert_eq!(now["status"]["state"], "running", "{now}");
    assert!(!apt.path("systemd-run.args").exists());

    apt.set("unit.state", "success");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert_eq!(apt.calls().len(), 2);
}

/// 셸이 시작했는데 종료 상태 줄 없이 사라졌으면(apt가 도중에 끊겼다) dpkg가 깨끗해도 실패다.
#[tokio::test]
async fn a_vanished_unit_without_an_exit_status_was_interrupted() {
    let apt = FakeApt::new();
    let (env, task) = resume_upgrade(&apt, |id| {
        write_log(
            &apt,
            id,
            "opl-upgrade: start\nUnpacking orbivisor (0.2.1) ...\n",
        )
    })
    .await;
    let admin = env.admin().await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        "the upgrade was interrupted; start it again"
    );
    assert_eq!(apt.lines("dpkg.calls"), ["--audit"]);
    let (_, node) = admin.get("/nodes/node1").await;
    assert!(
        node["status"]["updates"]["checked_at"].as_u64().is_some(),
        "{node}"
    );
}

/// 셸이 끝까지 돌고 유닛이 치워졌으면 로그의 종료 상태로 성공이다.
#[tokio::test]
async fn a_vanished_unit_that_exited_zero_succeeded() {
    let apt = FakeApt::new();
    let (env, task) = resume_upgrade(&apt, |id| {
        write_log(&apt, id, "opl-upgrade: start\nopl-upgrade: exit 0\n")
    })
    .await;
    let done = env.admin().await.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert!(apt.lines("dpkg.calls").is_empty());
    assert!(!apt.path("systemd-run.args").exists());
}

/// 이전 opld가 유닛을 띄우기 전에 멈췄으면(로그가 없다) 이어 볼 때 유닛을 띄운다.
#[tokio::test]
async fn resuming_an_upgrade_that_never_started_starts_it() {
    let apt = FakeApt::new();
    let (env, task) = resume_upgrade(&apt, |_| {}).await;
    let admin = env.admin().await;
    // 가짜 systemd-run은 인자를 쓴 뒤 `unit.state`에 active를 쓴다.
    eventually("the unit to be started", || {
        let state = std::fs::read_to_string(apt.path("unit.state")).ok()?;
        (state.trim() == "active").then_some(())
    })
    .await;
    let (unit, _) = unit_and_log(&apt, &task);
    assert_eq!(apt.lines("systemd-run.args")[0], format!("--unit={unit}"));
    apt.set("unit.state", "success");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert!(apt.lines("dpkg.calls").is_empty());
}

#[tokio::test]
async fn a_vanished_unit_with_a_dpkg_audit_report_was_interrupted() {
    let apt = FakeApt::new();
    apt.set(
        "audit.txt",
        "The following packages are only half configured, probably due to problems\n\
         configuring them the first time.  The configuration should be retried using\n\
         dpkg --configure <package> or the configure menu option in dselect:\n\
         orbivisor            Orbivisor virtualization platform\n",
    );
    let (env, task) = resume_upgrade(&apt, |id| write_log(&apt, id, "opl-upgrade: start\n")).await;
    let admin = env.admin().await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        "the upgrade was interrupted; run \"dpkg --configure -a\" on the node"
    );
}

#[tokio::test]
async fn a_vanished_unit_uses_the_exit_status_in_the_log() {
    let apt = FakeApt::new();
    let (env, task) = resume_upgrade(&apt, |id| {
        write_log(
            &apt,
            id,
            "opl-upgrade: start\nE: Unable to fetch some archives\nopl-upgrade: exit 100\n",
        )
    })
    .await;
    let admin = env.admin().await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        "the upgrade failed; last log lines:\nopl-upgrade: start\n\
         E: Unable to fetch some archives\nopl-upgrade: exit 100"
    );
    // 종료 상태가 있으면 dpkg로 짐작하지 않는다.
    assert!(apt.lines("dpkg.calls").is_empty());
}

#[tokio::test]
async fn the_agent_skips_checks_while_the_node_is_upgrading() {
    let apt = FakeApt::new();
    let mut env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    // 실행기가 없으면 업그레이드 Task는 running으로 남는다.
    env.stop_runner().await;
    let mut task: Task = Resource::new(
        "up",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: env.node,
            node: env.node,
        },
    );
    task.status.state = TaskState::Running;
    env.cluster.create(&task).await.unwrap();
    let mut config = apt.config();
    config.first_check = Duration::from_millis(20);
    config.check_interval = Duration::from_millis(50);
    let agent = UpdateAgent::spawn(
        env.cluster.clone(),
        env.node,
        Arc::new(Updates::new(config)),
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(apt.calls().is_empty(), "{:?}", apt.calls());

    env.cluster
        .update_status::<TaskSpec>(
            task.id,
            &TaskStatus {
                state: TaskState::Succeeded,
                error: None,
                progress: None,
            },
        )
        .await
        .unwrap();
    eventually("a check after the upgrade", || {
        (!apt.calls().is_empty()).then_some(())
    })
    .await;
    agent.shutdown().await;
}
