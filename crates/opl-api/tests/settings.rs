//! 클러스터 설정 API: 없으면 기본값, 처음 PATCH가 만듦, 요청 검사와 권한.

mod common;

use common::Env;
use opl_types::node::{FAST_HA, NodeSpec};
use opl_types::resource::Resource;
use opl_types::settings::{Channel, HaReserve, SETTINGS_NAME, SettingsSpec, ShutdownPolicy};
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
                roles: None,
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

/// 투표 멤버 상한은 3·5·7만 받고, 준 값을 적는다(없으면 적지 않는다).
#[tokio::test]
async fn voters_are_three_five_or_seven() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for bad in [json!(0), json!(4), json!(9), json!("5"), json!(-1)] {
        let got = admin
            .patch("/settings", json!({ "voters": bad }), None)
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let (status, got) = admin.patch("/settings", json!({ "voters": 3 }), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["voters"], 3);
    let stored = env
        .cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(stored.spec.voter_limit(), 3);
    // 다른 키만 바꾸면 그대로다.
    let (_, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(got["spec"]["voters"], 3);
}

/// 종료 정책은 네 값만 받고, 기본(`conditional`)은 적지 않는다. `ha-placement`를 모르는 노드가 있으면 409다.
#[tokio::test]
async fn shutdown_policy_is_written_unless_default_and_waits_for_the_feature() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for bad in [json!("Freeze"), json!("ignore"), json!(1)] {
        let got = admin
            .patch("/settings", json!({ "shutdown_policy": bad }), None)
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let (status, got) = admin
        .patch("/settings", json!({ "shutdown_policy": "migrate" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["shutdown_policy"], "migrate");
    let stored = || {
        env.cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
            .unwrap()
            .spec
    };
    assert_eq!(stored().shutdown_policy(), ShutdownPolicy::Migrate);
    // 다른 키만 바꾸면 그대로다.
    let (_, got) = admin.patch("/settings", json!({ "voters": 3 }), None).await;
    assert_eq!(got["spec"]["shutdown_policy"], "migrate");
    // 기본으로 돌리면 필드가 빠진다(옛 모양).
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "shutdown_policy": "conditional" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(got["spec"].get("shutdown_policy").is_none(), "{got}");
    assert_eq!(stored().shutdown_policy, None);

    // 버전은 같지만 기능을 적지 않은 노드(옛 위트니스)가 있으면 거절한다.
    let mut old: opl_types::node::Node = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
            roles: None,
        },
    );
    old.status.version = Some(opl_types::version::BUILD.into());
    env.cluster.create(&old).await.unwrap();
    let got = admin
        .patch("/settings", json!({ "shutdown_policy": "freeze" }), None)
        .await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    assert!(
        got.1["message"].as_str().unwrap().contains("ha placement"),
        "{}",
        got.1
    );
    assert_eq!(stored().shutdown_policy, None);
}

/// HA 용량 예약과 동시 시작: 범위 밖은 400, 기본값·`"none"`은 적지 않고, `fast-ha`를 모르는 노드가 있으면 409다.
#[tokio::test]
async fn ha_reserve_and_start_concurrency_are_checked_and_wait_for_the_feature() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for bad in [
        json!({ "ha_reserve": { "nodes": 0 } }),
        json!({ "ha_reserve": { "nodes": 9 } }),
        json!({ "ha_reserve": { "percent": 0 } }),
        json!({ "ha_reserve": { "percent": 91 } }),
        json!({ "ha_reserve": "all" }),
        json!({ "ha_reserve": { "hosts": 1 } }),
        json!({ "ha_start_concurrency": 0 }),
        json!({ "ha_start_concurrency": 33 }),
    ] {
        let got = admin.patch("/settings", bad.clone(), None).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let stored = || {
        env.cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
            .unwrap()
            .spec
    };
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "ha_reserve": { "nodes": 1 }, "ha_start_concurrency": 2 }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["ha_reserve"], json!({ "nodes": 1 }));
    assert_eq!(got["spec"]["ha_start_concurrency"], 2);
    assert_eq!(stored().ha_reserve, Some(HaReserve::Nodes(1)));
    assert_eq!(stored().ha_start_concurrency(), 2);
    let (_, got) = admin
        .patch(
            "/settings",
            json!({ "ha_reserve": { "percent": 30 } }),
            None,
        )
        .await;
    assert_eq!(got["spec"]["ha_reserve"], json!({ "percent": 30 }));
    assert_eq!(got["spec"]["ha_start_concurrency"], 2);
    // `"none"`과 기본값(4)은 필드를 뺀다.
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "ha_reserve": "none", "ha_start_concurrency": 4 }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(got["spec"].get("ha_reserve").is_none(), "{got}");
    assert!(got["spec"].get("ha_start_concurrency").is_none(), "{got}");
    assert_eq!(stored().ha_reserve, None);
    assert_eq!(stored().ha_start_concurrency, None);

    // `fast-ha`를 모르는 노드가 있으면 거절한다.
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != FAST_HA);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    for body in [
        json!({ "ha_reserve": { "nodes": 1 } }),
        json!({ "ha_start_concurrency": 8 }),
    ] {
        let got = admin.patch("/settings", body, None).await;
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
        assert!(
            got.1["message"].as_str().unwrap().contains("fast ha"),
            "{}",
            got.1
        );
    }
    assert_eq!(stored().ha_reserve, None);
}
