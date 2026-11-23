//! 계획 69 스테이지 1: 파일 복원의 새 권한 `backup.file_restore`과 Task는 API 스테이지(5) 전까지 임시로 막힌다. 역할·
//! 토큰 범위에 그 권한을 넣는 쓰기는 옛 노드가 남았으면 409 `cluster_upgrading`(검증보다 먼저), 아니면 400이다. 내장
//! admin은 그 권한을 가지고 operator는 가지지 않는다. Raft에 직접 쓴 파일 복원 Task는 실행기가 실패로 끝낸다.

mod common;

use common::Env;
use opl_types::file_restore::{EntryPath, GuestConflict};
use opl_types::node::{FILE_RESTORE, NodeSpec};
use opl_types::resource::Resource;
use opl_types::task::{TaskOp, TaskSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 이 노드가 `file-restore`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_feature(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != FILE_RESTORE);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

fn not_yet(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("not available yet (file restore)"),
        "{body}"
    );
}

fn upgrading(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

#[tokio::test]
async fn file_restore_privilege_is_gated_until_the_api_stage() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let role = json!({ "name": "helpdesk", "privileges": ["system.read", "backup.file_restore"] });
    let token = json!({ "name": "ci", "scope": { "privileges": ["backup.file_restore"], "targets": [{ "type": "root" }] } });
    let (status, body) = admin.post("/access-roles", role.clone()).await;
    not_yet(status, &body);
    let (status, body) = admin.post("/tokens", token.clone()).await;
    not_yet(status, &body);

    // 새 권한이 없는 역할은 지금 그대로이고, 바꾸기로 넣는 것은 막힌다.
    let (status, body) = admin
        .post(
            "/access-roles",
            json!({ "name": "helpdesk", "privileges": ["system.read"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .put(
            "/access-roles/helpdesk",
            json!({ "privileges": ["system.read", "backup.file_restore"] }),
            None,
        )
        .await;
    not_yet(status, &body);

    // 내장 admin은 새 권한을 가지고 operator는 가지지 않는다(계획 62 규칙).
    let (_, body) = admin.get("/access-roles/admin").await;
    assert!(
        body["privileges"]
            .as_array()
            .unwrap()
            .contains(&json!("backup.file_restore")),
        "{body}"
    );
    let (_, body) = admin.get("/access-roles/operator").await;
    assert!(
        !body["privileges"]
            .as_array()
            .unwrap()
            .contains(&json!("backup.file_restore")),
        "{body}"
    );

    // 옛 노드가 있으면 검증보다 먼저 409다(모르는 권한 이름과 섞여도).
    drop_feature(&env).await;
    let (status, body) = admin.post("/access-roles", role).await;
    upgrading(status, &body);
    let (status, body) = admin
        .put(
            "/access-roles/helpdesk",
            json!({ "privileges": ["backup.file_restore", "no.such"] }),
            None,
        )
        .await;
    upgrading(status, &body);
    let (status, body) = admin.post("/tokens", token).await;
    upgrading(status, &body);
}

/// 실행 스테이지(5·6) 전에는 파일 복원 Task가 실패로 끝난다(Raft에 직접 쓴 것만 — API는 만들지 않는다).
#[tokio::test]
async fn file_restore_tasks_fail_until_the_run_stages() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let backup = Uuid::now_v7();
    for op in [
        TaskOp::FileRestoreSession { backup },
        TaskOp::FileRestoreToGuest {
            session: Uuid::now_v7(),
            backup,
            volume: "disk0".into(),
            paths: vec![EntryPath::parse("/etc/hosts").unwrap()],
            target_dir: "/tmp".into(),
            conflict: GuestConflict::Keep,
        },
    ] {
        let mut t: opl_types::task::Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: Uuid::now_v7(),
                node: env.node,
            },
        );
        t.name = t.id.to_string();
        env.cluster.create(&t).await.unwrap();
        let done = admin.wait_task(&json!({ "id": t.id })).await;
        assert_eq!(done["status"]["state"], "failed", "{done}");
        assert_eq!(
            done["status"]["error"], "file restore tasks are not available yet",
            "{done}"
        );
    }
}
