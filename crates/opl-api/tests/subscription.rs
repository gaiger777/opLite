//! 구독 API: 넣기·지우기, 권한, 상태 계산(노드 수, 위트니스 제외), 키를 내보내지 않음, enterprise 채널 조건.

mod common;

use common::Env;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
use opl_types::version::BUILD;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 저장소 개발 키의 비밀 키 견본
const DEV_PRIVATE: &str = include_str!("../../opl-types/tests/fixtures/dev-license.key");
const DAY: u64 = 86_400;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 개발 키로 서명한 키. 노드 `nodes`대까지, 지금부터 `days`일 뒤 만료.
fn key(nodes: u32, days: u64) -> String {
    key_at(nodes, now() - DAY, now() + days * DAY)
}

fn key_at(nodes: u32, issued_at: u64, expires_at: u64) -> String {
    sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "sub-1".into(),
            customer: "Example Corp".into(),
            level: Level::Premium,
            nodes,
            issued_at,
            expires_at,
        },
    )
    .unwrap()
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 지금 빌드를 돌리는 노드를 더한다.
async fn add_node(env: &Env, name: &str, witness: bool, version: Option<&str>) {
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness,
            roles: None,
        },
    );
    node.status.version = version.map(Into::into);
    env.cluster.create(&node).await.unwrap();
}

fn stored_key(env: &Env) -> Option<String> {
    env.cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .and_then(|s| s.spec.subscription)
}

#[tokio::test]
async fn subscription_is_set_shown_and_removed() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, got) = viewer.get("/subscription").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got, json!({ "state": "none", "used_nodes": 1 }));

    // 앞뒤 공백은 다듬어 저장한다.
    let k = key(3, 30);
    let (status, got) = admin
        .put("/subscription", json!({ "key": format!("  {k}\n") }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["state"], "active");
    assert_eq!(got["id"], "sub-1");
    assert_eq!(got["customer"], "Example Corp");
    assert_eq!(got["level"], "premium");
    assert_eq!(got["nodes"], 3);
    assert_eq!(got["used_nodes"], 1);
    assert_eq!(got["development"], true);
    assert!(got["expires_at"].as_u64().unwrap() > now());
    assert_eq!(stored_key(&env).as_deref(), Some(k.as_str()));
    let (status, shown) = viewer.get("/subscription").await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown, got);

    // 설정 응답은 키 문자열을 담지 않는다.
    let (_, settings) = viewer.get("/settings").await;
    assert_eq!(
        settings["spec"],
        json!({ "updates": { "channel": "stable" } })
    );
    let (status, patched) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        patched["spec"],
        json!({ "updates": { "channel": "testing" } })
    );
    assert!(!patched.to_string().contains(&k));
    assert_eq!(stored_key(&env).as_deref(), Some(k.as_str()));

    let (status, got) = admin.delete("/subscription").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got, json!({ "state": "none", "used_nodes": 1 }));
    assert_eq!(stored_key(&env), None);
    let revision = admin.get("/settings").await.1["revision"].clone();
    // 이미 없으면 쓰지 않는다.
    let (status, got) = admin.delete("/subscription").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(admin.get("/settings").await.1["revision"], revision);
}

#[tokio::test]
async fn only_admins_change_the_subscription() {
    let env = Env::start().await;
    let k = key(3, 30);
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        assert_eq!(client.get("/subscription").await.0, StatusCode::OK);
        assert_error(
            &client.put("/subscription", json!({ "key": k }), None).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
        assert_error(
            &client.delete("/subscription").await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    assert_error(
        &env.anonymous().get("/subscription").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    assert_eq!(stored_key(&env), None);
}

#[tokio::test]
async fn bad_keys_are_rejected_with_the_reason() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let k = key(3, 30);
    // 서명 가운데 글자 하나를 바꾼다(마지막 글자는 남는 비트가 있어 형식 오류가 될 수 있다).
    let at = k.len() - 10;
    let swapped = if &k[at..=at] == "A" { "B" } else { "A" };
    let tampered = format!("{}{swapped}{}", &k[..at], &k[at + 1..]);
    // 2023-11-14T22:13:20Z에 발급, 하루 뒤 만료
    let expired = key_at(3, 1_700_000_000, 1_700_000_000 + DAY);
    for (body, message) in [
        (
            json!({ "key": "opl1.x" }),
            "malformed subscription key: expected opl1.<payload>.<signature>".to_string(),
        ),
        (
            json!({ "key": tampered }),
            "the subscription key signature does not match this build".to_string(),
        ),
        (
            json!({ "key": expired }),
            "the subscription key expired on 2023-11-15".to_string(),
        ),
    ] {
        let got = admin.put("/subscription", body, None).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_subscription_key");
        assert_eq!(got.1["message"], message);
    }
    assert_error(
        &admin
            .put("/subscription", json!({ "key": k, "x": 1 }), None)
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_eq!(stored_key(&env), None);
}

/// 노드 수는 위트니스를 빼고 센다. 한도와 같으면 active, 넘으면 over_limit.
#[tokio::test]
async fn nodes_over_the_limit_are_reported() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, got) = admin
        .put("/subscription", json!({ "key": key(2, 30) }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["state"], "active");

    add_node(&env, "node2", false, Some(BUILD)).await;
    add_node(&env, "witness", true, Some(BUILD)).await;
    let (_, got) = admin.get("/subscription").await;
    assert_eq!(got["state"], "active", "{got}");
    assert_eq!(got["used_nodes"], 2);

    add_node(&env, "node3", false, Some(BUILD)).await;
    let (_, got) = admin.get("/subscription").await;
    assert_eq!(got["state"], "over_limit", "{got}");
    assert_eq!(got["used_nodes"], 3);
    assert_eq!(got["nodes"], 2);
}

/// 이 빌드보다 낮거나 버전을 적지 않은 노드(위트니스 포함)가 있으면 키를 쓰지 않는다.
#[tokio::test]
async fn subscription_waits_until_every_node_runs_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    add_node(&env, "old", true, Some("0.0.1~old")).await;
    add_node(&env, "ancient", false, None).await;
    let k = key(3, 30);
    for got in [
        admin.put("/subscription", json!({ "key": k }), None).await,
        admin.delete("/subscription").await,
    ] {
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
        assert_eq!(
            got.1["message"],
            "some nodes run an older Orbivisor (ancient, old); finish the upgrade on every node first"
        );
    }
    assert_eq!(stored_key(&env), None);
    // 읽기는 된다.
    assert_eq!(admin.get("/subscription").await.0, StatusCode::OK);
}

/// 설정 변경 구독(watch)도 키 문자열을 내보내지 않는다.
#[tokio::test]
async fn watch_does_not_send_the_key() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let since = viewer.get("/vms").await.1["revision"].as_u64().unwrap();
    let url = format!(
        "{}/api/v1/watch?kinds=settings&since={since}",
        viewer.base.replace("http://", "ws://")
    );
    let mut req = url.into_client_request().unwrap();
    let token = viewer.token.as_ref().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let mut ws = tokio_tungstenite::connect_async(req).await.unwrap().0;

    let k = key(3, 30);
    let (status, got) = admin.put("/subscription", json!({ "key": k }), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let ev: Value = loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
            .await
            .expect("no message")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = msg {
            break serde_json::from_str(&text).unwrap();
        }
    };
    assert_eq!(ev["kind"], "settings", "{ev}");
    assert_eq!(
        ev["resource"]["spec"],
        json!({ "updates": { "channel": "stable" } })
    );
    assert!(!ev.to_string().contains(&k));
}

fn stored_channel(env: &Env) -> Option<String> {
    env.cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .map(|s| s.spec.updates.channel.as_str().to_string())
}

/// enterprise 채널은 구독이 active일 때만 고른다. 이미 enterprise여도 다시 보내면 같이 검사한다.
#[tokio::test]
async fn the_enterprise_channel_needs_an_active_subscription() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let enterprise = json!({ "updates": { "channel": "enterprise" } });
    let got = admin.patch("/settings", enterprise.clone(), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "subscription_required");
    assert_eq!(
        got.1["message"],
        "an active subscription is required for the enterprise channel"
    );
    assert_eq!(stored_channel(&env), None);

    let k = key(1, 30);
    assert_eq!(
        admin
            .put("/subscription", json!({ "key": k }), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, got) = admin.patch("/settings", enterprise.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["spec"],
        json!({ "updates": { "channel": "enterprise" } })
    );
    assert_eq!(stored_channel(&env).as_deref(), Some("enterprise"));

    // 노드가 한도를 넘으면(over_limit) 다시 enterprise를 보내도 거절한다. 다른 채널은 고른다.
    add_node(&env, "node2", false, Some(BUILD)).await;
    let got = admin.patch("/settings", enterprise.clone(), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "subscription_required");
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");

    // 구독을 지워도 채널은 그대로 두지만, enterprise로는 바꾸지 못한다.
    assert_eq!(
        admin
            .put("/subscription", json!({ "key": key(2, 30) }), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        admin.patch("/settings", enterprise.clone(), None).await.0,
        StatusCode::OK
    );
    assert_eq!(admin.delete("/subscription").await.0, StatusCode::OK);
    assert_eq!(stored_channel(&env).as_deref(), Some("enterprise"));
    let got = admin.patch("/settings", enterprise, None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "subscription_required");
}

/// 이 빌드보다 낮은 노드가 있으면 구독이 있어도 enterprise를 쓰지 않는다(옛 노드는 풀지 못한다).
/// 구독을 넣은 뒤 옛 노드가 생기는 이 조합은 합류 버전 검사 때문에 실제로는 생기지 않는 인위적
/// 상황이다. 여기서는 PATCH의 `require_current` 게이트만 확인한다.
#[tokio::test]
async fn the_enterprise_channel_waits_until_every_node_runs_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(
        admin
            .put("/subscription", json!({ "key": key(3, 30) }), None)
            .await
            .0,
        StatusCode::OK
    );
    add_node(&env, "old", true, Some("0.0.1~old")).await;
    let got = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "enterprise" } }),
            None,
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    assert_eq!(stored_channel(&env).as_deref(), Some("stable"));
    // 옛 노드가 푸는 채널은 그대로 고른다.
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
}
