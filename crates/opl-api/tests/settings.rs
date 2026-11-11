//! 클러스터 설정 API: 없으면 기본값, 처음 PATCH가 만듦, 요청 검사와 권한.

mod common;

use common::Env;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::settings::{Channel, SETTINGS_NAME, SettingsSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

#[tokio::test]
async fn settings_default_until_the_first_patch_creates_them() {
    let env = Env::start().await;
    let admin = env.admin().await;

    // 리소스가 없으면 기본값을 리소스 모양으로 보인다.
    let (status, got) = admin.get("/settings").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], "00000000-0000-0000-0000-000000000000");
    assert_eq!(got["name"], SETTINGS_NAME);
    assert_eq!(got["revision"], 0);
    assert_eq!(got["spec"], json!({ "updates": { "channel": "stable" } }));
    assert!(
        env.cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
            .is_none()
    );

    // 처음 PATCH가 만든다.
    let (status, created) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["name"], SETTINGS_NAME);
    assert_eq!(created["spec"]["updates"]["channel"], "testing");
    let stored = env
        .cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(stored.spec.updates.channel, Channel::Testing);
    assert_eq!(created["id"], stored.id.to_string());
    assert_eq!(created["revision"], stored.revision);

    // 다음 PATCH는 같은 리소스를 바꾼다.
    let (status, updated) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "stable" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["id"], created["id"]);
    assert_eq!(updated["spec"]["updates"]["channel"], "stable");
    assert!(updated["revision"].as_u64() > created["revision"].as_u64());
    let (_, got) = admin.get("/settings").await;
    assert_eq!(got, updated);

    // 같은 값이면 쓰지 않는다.
    let (status, same) = admin
        .patch("/settings", json!({ "updates": {} }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same["revision"], updated["revision"]);
}

#[tokio::test]
async fn settings_patch_checks_the_request() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_error(
        &admin.patch("/settings", json!({}), None).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_error(
        &admin
            .patch("/settings", json!({ "telemetry": { "interval": 1 } }), None)
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_error(
        &admin
            .patch("/settings", json!({ "updates": { "mirror": "x" } }), None)
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_error(
        &admin
            .patch(
                "/settings",
                json!({ "updates": { "channel": "nightly" } }),
                None,
            )
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    // If-Match는 읽은 revision과 맞아야 한다(없는 설정은 0).
    let body = json!({ "updates": { "channel": "testing" } });
    assert_error(
        &admin.patch("/settings", body.clone(), Some(5)).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    let (status, created) = admin.patch("/settings", body.clone(), Some(0)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let revision = created["revision"].as_u64().unwrap();
    let body = json!({ "updates": { "channel": "stable" } });
    assert_error(
        &admin
            .patch("/settings", body.clone(), Some(revision - 1))
            .await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    let (status, got) = admin.patch("/settings", body, Some(revision)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["updates"]["channel"], "stable");
}

#[tokio::test]
async fn only_admins_change_settings() {
    let env = Env::start().await;
    let body = json!({ "updates": { "channel": "testing" } });
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        let (status, got) = client.get("/settings").await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["spec"]["updates"]["channel"], "stable");
        assert_error(
            &client.patch("/settings", body.clone(), None).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    assert_error(
        &env.anonymous().get("/settings").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    assert!(
        env.cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
            .is_none()
    );
}

/// 같은 If-Match 0으로 함께 만들면 하나만 만들고 나머지는 412다(409 name_taken이 아니다).
/// 요청이 정말 겹치도록 여러 스레드에서 돈다.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_patches_with_if_match_lose_with_412() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let body = json!({ "updates": { "channel": "testing" } });
    let got = futures_util::future::join_all(
        (0..32).map(|_| admin.patch("/settings", body.clone(), Some(0))),
    )
    .await;
    let created = got.iter().filter(|g| g.0 == StatusCode::OK).count();
    assert_eq!(created, 1, "{got:?}");
    for g in got.iter().filter(|g| g.0 != StatusCode::OK) {
        assert_error(g, StatusCode::PRECONDITION_FAILED, "revision_mismatch");
    }
}

/// 버전을 적지 않은 옛 노드가 있으면 옛 노드가 풀지 못하는 설정을 쓰지 않는다.
#[tokio::test]
async fn settings_wait_until_every_node_is_upgraded() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut olds = Vec::new();
    for name in ["old", "ancient"] {
        let old = Resource::new(
            name,
            NodeSpec {
                address: "10.0.0.9:8006".into(),
                maintenance: false,
                witness: true,
            },
        );
        env.cluster.create(&old).await.unwrap();
        olds.push(old.id);
    }
    let body = json!({ "updates": { "channel": "testing" } });
    let got = admin.patch("/settings", body.clone(), None).await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    assert_eq!(
        got.1["message"],
        "some nodes run an older Orbivisor (ancient, old); finish the upgrade on every node first"
    );

    for old in olds {
        let mut status = env.cluster.get::<NodeSpec>(old).unwrap().unwrap().status;
        status.version = Some("0.1.0".into());
        env.cluster
            .update_status::<NodeSpec>(old, &status)
            .await
            .unwrap();
    }
    let (status, got) = admin.patch("/settings", body, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
}
