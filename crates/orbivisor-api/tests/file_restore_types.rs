//! 계획 69: 파일 복원의 새 권한 `backup.file_restore`을 역할·토큰 범위에 넣는 쓰기는 옛 노드가 남았으면 409
//! `cluster_upgrading`(검증보다 먼저)이고 모든 노드가 알면 된다(스테이지 5). 내장 admin은 그 권한을 가지고 operator는
//! 가지지 않는다. 파일 복원을 차리지 않은 노드의 세션 Task와 되돌려 쓰기 Task는 실패로 끝난다.

mod common;

use common::Env;
use orbivisor_types::file_restore::{EntryPath, GuestConflict};
use orbivisor_types::node::{FILE_RESTORE, NodeSpec};
use orbivisor_types::resource::Resource;
use orbivisor_types::task::{TaskOp, TaskSpec};
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

fn upgrading(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

#[tokio::test]
async fn file_restore_privilege_needs_every_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let role = json!({ "name": "helpdesk", "privileges": ["system.read", "backup.file_restore"] });
    let token = json!({ "name": "ci", "scope": { "privileges": ["backup.file_restore"], "targets": [{ "type": "root" }] } });
    let (status, body) = admin.post("/access-roles", role.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        body["privileges"]
            .as_array()
            .unwrap()
            .contains(&json!("backup.file_restore")),
        "{body}"
    );
    let (status, body) = admin.post("/tokens", token.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .put(
            "/access-roles/helpdesk",
            json!({ "privileges": ["backup.file_restore"] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

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
    let role = json!({ "name": "helpdesk2", "privileges": ["system.read", "backup.file_restore"] });
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

/// 도우미 이미지를 차리지 않은 노드의 세션 Task와 되돌려 쓰기 Task는 실패로 끝난다(Raft에 직접 쓴 것).
#[tokio::test]
async fn file_restore_tasks_fail_where_they_cannot_run() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let backup = Uuid::now_v7();
    for (op, error) in [
        (
            TaskOp::FileRestoreSession { backup },
            "file restore is not set up on this node",
        ),
        (
            TaskOp::FileRestoreToGuest {
                session: Uuid::now_v7(),
                backup,
                volume: "disk0".into(),
                paths: vec![EntryPath::parse("/etc/hosts").unwrap()],
                target_dir: "/tmp".into(),
                conflict: GuestConflict::Keep,
            },
            "file restore is not set up on this node",
        ),
    ] {
        let mut t: orbivisor_types::task::Task = Resource::new(
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
        assert_eq!(done["status"]["error"], error, "{done}");
    }
}
