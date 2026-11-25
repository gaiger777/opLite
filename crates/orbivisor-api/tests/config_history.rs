//! 설정 이력의 출처(계획 73 §7.1): API 미들웨어가 요청마다 주체·출처(`Orbivisor-Client`)·사유(`Orbivisor-Change-Reason`)·
//! 감사 id를 쓰기에 달고, Task 실행기는 Task를 만든 사용자와 Task를 단다. 출처 없는 쓰기는 `System`이다.

mod common;

use std::sync::Arc;

use common::{Env, FakeStorage};
use orbivisor_types::history::{ActorAuth, ChangeSource, HistoryOp};
use orbivisor_types::resource::Kind;
use orbivisor_types::storage::VolumeSpec;
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn id_of(v: &serde_json::Value) -> Uuid {
    v["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn api_writes_carry_actor_client_reason_and_audit_id() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 토큰 + `Orbivisor-Client` + 사유
    let r = admin
        .http
        .post(format!("{}/api/v1/folders", admin.base))
        .bearer_auth(admin.token.as_deref().unwrap())
        .header("orbivisor-client", "cli/0.1.0")
        .header(
            "orbivisor-change-reason",
            reqwest::header::HeaderValue::from_bytes("  INC-42:\t팀 폴더 ".as_bytes()).unwrap(),
        )
        .json(&json!({ "name": "prod", "kind": "vm" }));
    let (status, folder) = admin.send(r).await;
    assert_eq!(status, StatusCode::CREATED, "{folder}");
    let rows = env
        .cluster
        .history_of(Kind::Folder, id_of(&folder))
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row.op, HistoryOp::Create);
    assert_eq!(row.source, ChangeSource::Cli);
    assert_eq!(row.reason.as_deref(), Some("INC-42:팀 폴더"));
    assert_eq!(row.actor.user.as_deref(), Some("admin"));
    assert!(row.actor.user_id.is_some());
    // 토큰 이름(시험 하네스가 `admin-…`으로 만든다)
    assert!(
        matches!(&row.actor.auth, ActorAuth::Token { name } if name.starts_with("admin-")),
        "{:?}",
        row.actor.auth
    );
    assert!(row.at_ms > 0);
    // 감사 줄과 같은 id다.
    let request = row.request.expect("audit id");
    let (status, audit) = admin.get("/audit?limit=50").await;
    assert_eq!(status, StatusCode::OK, "{audit}");
    let entry = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == request.to_string())
        .unwrap_or_else(|| panic!("no audit entry {request}: {audit}"));
    assert_eq!(entry["action"], "POST /folders");

    // 머리 없는 토큰 요청은 `api`, 모르는 도구도 `api`
    let (status, other) = admin
        .post("/folders", json!({ "name": "dev", "kind": "vm" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let rows = env.cluster.history_of(Kind::Folder, id_of(&other)).unwrap();
    assert_eq!(rows[0].source, ChangeSource::Api);
    assert_eq!(rows[0].reason, None);
}

#[tokio::test]
async fn session_writes_come_from_the_web() {
    let env = Env::start().await;
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&json!({ "username": "admin", "password": common::ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let resp = http
        .post(format!("{}/api/v1/folders", env.base))
        .header("cookie", &cookie)
        .header("origin", &env.base)
        .json(&json!({ "name": "web", "kind": "vm" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let folder: serde_json::Value = resp.json().await.unwrap();
    let rows = env
        .cluster
        .history_of(Kind::Folder, id_of(&folder))
        .unwrap();
    assert_eq!(rows[0].source, ChangeSource::Web);
    assert_eq!(rows[0].actor.auth, ActorAuth::Session);
    assert_eq!(rows[0].actor.user.as_deref(), Some("admin"));
}

/// Task가 쓴 spec(볼륨 크기 바꾸기)은 Task를 만든 사용자와 Task id를 단다.
#[tokio::test]
async fn task_writes_carry_the_requester_and_the_task() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": pool } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
    let storage = Arc::new(FakeStorage::default());
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    let (status, task) = admin
        .post(
            "/volumes",
            json!({ "name": "data", "spec": { "pool": "local", "size_bytes": MIB, "format": "qcow2" } }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, task) = admin
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>("data")
        .unwrap()
        .unwrap();
    assert_eq!(volume.spec.size_bytes, 2 * MIB);
    let rows = env.cluster.history_of(Kind::Volume, volume.id).unwrap();
    let resized = rows
        .iter()
        .find(|r| r.op == HistoryOp::Update && r.spec["size_bytes"] == 2 * MIB)
        .unwrap_or_else(|| panic!("{rows:?}"));
    assert_eq!(resized.source, ChangeSource::Task);
    assert_eq!(resized.actor.task, Some(id_of(&task)));
    assert_eq!(resized.actor.user.as_deref(), Some("admin"));
    assert_eq!(rows[0].op, HistoryOp::Create);
    assert_eq!(rows[0].source, ChangeSource::Api);
}
