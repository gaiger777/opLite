//! 노드 업데이트 확인: 다시 확인 Task(가짜 apt), 요청 검사, 소스 파일과 주기 확인 에이전트.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_apt::FakeApt;
use common::{Env, MOUNT_ROOT, WAIT};
use opl_api::{UpdateAgent, Updates};
use opl_driver::DirStorage;
use opl_types::node::{NodeSpec, NodeUpdates};
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
         Inst oplite [0.2.0] (0.2.1 opLite:trixie [amd64])\n\
         Conf oplite (0.2.1 opLite:trixie [amd64])\n\
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
            { "name": "oplite", "current": "0.2.0", "candidate": "0.2.1" },
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
        "Inst oplite [0.2.0] (0.2.1 opLite:trixie [amd64])\n",
    );
    let env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    let admin = env.admin().await;
    let mut config = apt.config();
    config.apt_url = "https://apt.example.com/oplite".into();
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
        text.contains("URIs: https://apt.example.com/oplite\n"),
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
