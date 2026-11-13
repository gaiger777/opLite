//! 배치 규칙 API: 관리자만 만들고 바꾸고 지우며, 켜진 hard 규칙끼리의 모순은 422로 거절한다.

mod common;

use std::time::Duration;

use common::{Client, Env};
use futures_util::StreamExt;
use opl_types::node::{HA_PLACEMENT, NodeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn rule(name: &str, spec: Value) -> Value {
    json!({ "name": name, "spec": spec })
}

/// 이 노드의 `ha-placement` 기능 표시를 지운다(옛 노드 흉내).
async fn drop_ha_placement(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != HA_PLACEMENT);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn admins_manage_rules_and_others_read() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, body) = admin
        .post(
            "/placement-rules",
            rule(
                "apart",
                json!({ "kind": "anti-affinity", "vms": ["web-1", "web-2"] }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 아직 없는 VM이어도 된다. 빠진 값은 기본값이다.
    assert_eq!(body["spec"]["enabled"], true);
    assert_eq!(body["spec"]["ha_may_violate"], false);
    assert_eq!(body["spec"]["nodes"], json!([]));

    let spec =
        json!({ "kind": "soft-node-affinity", "vms": ["db"], "node_tag": "zone=a", "weight": 20 });
    assert_eq!(
        operator
            .post("/placement-rules", rule("db-zone", spec.clone()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator
            .put(
                "/placement-rules/apart",
                json!({ "spec": spec.clone() }),
                None
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator.delete("/placement-rules/apart").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, list) = viewer.get("/placement-rules").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"][0]["name"], "apart");

    // 통째로 바꾸고, `enabled`로 끈다.
    let (status, body) = admin
        .put("/placement-rules/apart", json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["kind"], "soft-node-affinity");
    assert_eq!(body["spec"]["weight"], 20);
    let mut off = body["spec"].clone();
    off["enabled"] = json!(false);
    let (status, body) = admin
        .put(
            "/placement-rules/apart",
            json!({ "spec": off }),
            body["revision"].as_u64(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        viewer.get("/placement-rules/apart").await.1["spec"]["enabled"],
        false
    );

    assert_eq!(
        admin.delete("/placement-rules/apart").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        viewer.get("/placement-rules/apart").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn invalid_rules_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for (spec, message) in [
        (
            json!({ "kind": "affinity", "vms": ["a", "b"], "vm_tag": "app=web" }),
            "give exactly one of vms and vm_tag",
        ),
        (
            json!({ "kind": "anti-affinity", "vms": ["a"] }),
            "anti-affinity rules need at least two VMs",
        ),
        (
            json!({ "kind": "affinity", "vms": ["a", "b"], "nodes": ["node1"] }),
            "affinity rules are between VMs; they take no nodes or node_tag",
        ),
        (
            json!({ "kind": "node-affinity", "vms": ["a"] }),
            "node-affinity rules need exactly one of nodes and node_tag",
        ),
        (
            json!({ "kind": "anti-affinity", "vms": ["a", "b"], "weight": 10 }),
            "weight is only for soft rules, not anti-affinity",
        ),
        (
            json!({ "kind": "soft-affinity", "vms": ["a", "b"], "ha_may_violate": true }),
            "ha_may_violate is only for hard rules, not soft-affinity",
        ),
        (
            json!({ "kind": "soft-anti-affinity", "vm_tag": "app", }),
            "tag \"app\" must be key=value",
        ),
        (
            json!({ "kind": "node-anti-affinity", "vms": ["a"], "nodes": ["ghost"] }),
            "node \"ghost\" not found",
        ),
    ] {
        let (status, body) = admin
            .post("/placement-rules", rule("r", spec.clone()))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{spec}: {body}");
        assert_eq!(body["message"], message, "{spec}");
    }
    let (status, body) = admin
        .post(
            "/placement-rules",
            rule(
                "r",
                json!({ "kind": "affinity", "vms": ["a", "b"], "priority": 1 }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// 켜진 hard 규칙끼리 모순이면 422 `rule_conflict`로 거절하고 관련 규칙을 `details.rules`로 알린다.
#[tokio::test]
async fn contradicting_rules_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let post = |name: &str, spec: Value| admin.post("/placement-rules", rule(name, spec));
    assert_eq!(
        post("ab", json!({ "kind": "affinity", "vms": ["a", "b"] }))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        post("bc", json!({ "kind": "affinity", "vms": ["b", "c"] }))
            .await
            .0,
        StatusCode::CREATED
    );
    // a와 c는 b를 거쳐 함께 있어야 한다.
    let apart = json!({ "kind": "anti-affinity", "vms": ["a", "c"] });
    let (status, body) = post("apart", apart.clone()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "rule_conflict");
    assert_eq!(body["details"]["rules"], json!(["ab", "apart", "bc"]));
    assert_eq!(
        body["message"],
        "placement rule \"apart\" contradicts other rules: ab, apart, bc"
    );
    // soft 규칙과 꺼진 규칙은 모순이 아니다. 켜면 422다.
    let mut off = apart.clone();
    off["enabled"] = json!(false);
    assert_eq!(post("apart", off).await.0, StatusCode::CREATED);
    let (status, body) = admin
        .put("/placement-rules/apart", json!({ "spec": apart }), None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        post(
            "spread",
            json!({ "kind": "soft-anti-affinity", "vms": ["a", "c"] })
        )
        .await
        .0,
        StatusCode::CREATED
    );

    // 묶음의 허용 노드가 비면 모순이다.
    assert_eq!(
        post(
            "a-here",
            json!({ "kind": "node-affinity", "vms": ["a"], "nodes": ["node1"] })
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (status, body) = post(
        "c-not-here",
        json!({ "kind": "node-anti-affinity", "vms": ["c"], "nodes": ["node1"] }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["details"]["rules"],
        json!(["a-here", "ab", "bc", "c-not-here"])
    );
}

/// 함께 들어온 모순인 규칙 둘은 하나만 저장된다(검사와 쓰기가 잠금 안에서).
#[tokio::test]
async fn contradicting_rules_created_together_are_not_both_stored() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for round in 0..5 {
        let (a, b) = (format!("a{round}"), format!("b{round}"));
        let vms = json!([a, b]);
        let (together, apart) = tokio::join!(
            admin.post(
                "/placement-rules",
                rule(
                    &format!("together-{round}"),
                    json!({ "kind": "affinity", "vms": vms })
                )
            ),
            admin.post(
                "/placement-rules",
                rule(
                    &format!("apart-{round}"),
                    json!({ "kind": "anti-affinity", "vms": vms })
                )
            ),
        );
        let mut got = [together.0, apart.0];
        got.sort();
        assert_eq!(
            got,
            [StatusCode::CREATED, StatusCode::UNPROCESSABLE_ENTITY],
            "{} {}",
            together.1,
            apart.1
        );
    }
}

#[tokio::test]
async fn rules_wait_for_all_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let spec = json!({ "kind": "affinity", "vms": ["a", "b"] });
    assert_eq!(
        admin
            .post("/placement-rules", rule("ab", spec.clone()))
            .await
            .0,
        StatusCode::CREATED
    );
    drop_ha_placement(&env).await;
    let (status, body) = admin
        .post("/placement-rules", rule("cd", spec.clone()))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    let (status, body) = admin
        .put("/placement-rules/ab", json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert_eq!(
        admin.delete("/placement-rules/ab").await.0,
        StatusCode::NO_CONTENT
    );
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(client: &Client, path: &str) -> Socket {
    let url = format!("{}/api/v1{path}", client.base.replace("http://", "ws://"));
    let mut req = url.into_client_request().unwrap();
    let token = client.token.as_ref().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_json(ws: &mut Socket) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

/// 규칙과 그룹의 변경을 구독할 수 있다.
#[tokio::test]
async fn rules_and_groups_are_watched() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let since = viewer.get("/placement-rules").await.1["revision"]
        .as_u64()
        .unwrap();
    let mut ws = connect(
        &viewer,
        &format!("/watch?kinds=placement-rule,ha-group&since={since}"),
    )
    .await;
    admin
        .post(
            "/placement-rules",
            rule("ab", json!({ "kind": "affinity", "vms": ["a", "b"] })),
        )
        .await;
    let ev = next_json(&mut ws).await;
    assert_eq!(ev["kind"], "placement-rule");
    assert_eq!(ev["resource"]["name"], "ab");
    admin
        .post(
            "/ha-groups",
            json!({ "name": "g", "spec": { "nodes": [{ "node": "node1" }] } }),
        )
        .await;
    let ev = next_json(&mut ws).await;
    assert_eq!(ev["kind"], "ha-group");
    assert_eq!(ev["resource"]["name"], "g");
}
