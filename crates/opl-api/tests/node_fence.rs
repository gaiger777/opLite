//! 노드 전원 끄기 펜싱 설정 `/nodes/{key}/fence`와 `fence-test`

mod common;

use std::sync::Arc;

use common::Env;
use opl_api::{FenceStatusSource, Fencer};
use opl_peer::FenceSource;
use opl_types::fence::{FenceKind, FenceMethodStatus, NodeFenceSpec, Power};
use opl_types::node::{FEATURES, Node, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

/// 저장소에만 있는 노드(실행기 없음). `features`가 거짓이면 옛 노드다.
async fn add_node(env: &Env, name: &str, witness: bool, features: bool) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "10.0.0.7:8006".into(),
            maintenance: false,
            witness,
            roles: None,
        },
    );
    node.status = NodeStatus {
        online: false,
        features: if features {
            FEATURES.iter().map(|f| f.to_string()).collect()
        } else {
            vec![]
        },
        ..Default::default()
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

fn ipmi(address: &str, password: Option<&str>) -> Value {
    let mut m = json!({ "kind": "ipmi", "address": address, "user": "admin" });
    if let Some(p) = password {
        m["password"] = json!(p);
    }
    m
}

fn stored(env: &Env, node: Uuid) -> Option<NodeFenceSpec> {
    env.cluster
        .get::<NodeFenceSpec>(node)
        .unwrap()
        .map(|f| f.spec)
}

#[tokio::test]
async fn fencing_is_set_read_and_deleted_without_showing_passwords() {
    let env = Env::start().await;
    let node = add_node(&env, "node2", false, true).await;
    let admin = env.admin().await;
    assert_eq!(
        admin.get("/nodes/node2/fence").await.0,
        StatusCode::NOT_FOUND
    );

    let body = json!({ "methods": [
        ipmi("10.0.0.9", Some("ipmi-s3cret")),
        { "kind": "redfish", "url": "https://bmc", "user": "root", "password": "rf-s3cret", "system_id": "1" },
        { "kind": "command", "off": "pdu off 3", "status": "pdu status 3" },
    ]});
    let (status, view) = admin.put("/nodes/node2/fence", body, None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(!view.to_string().contains("s3cret"), "{view}");
    assert_eq!(view["node"], "node2");
    assert_eq!(
        view["methods"][0],
        json!({ "kind": "ipmi", "address": "10.0.0.9", "port": 623, "user": "admin", "lanplus": true, "has_password": true })
    );
    assert_eq!(view["methods"][1]["has_password"], true);
    assert_eq!(view["methods"][1]["system_id"], "1");
    assert_eq!(view["methods"][2]["off"], "pdu off 3");
    let (status, got) = admin.get(&format!("/nodes/{node}/fence")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got, view);
    // 리소스 id는 노드 id다.
    let spec = stored(&env, node).unwrap();
    assert_eq!(spec.methods.len(), 3);

    // 비밀번호를 빼면 저장된 값을 둔다(순서를 바꿔도 같은 BMC면). 빈 글이면 지운다.
    let (status, view) = admin
        .put(
            "/nodes/node2/fence",
            json!({ "methods": [
                { "kind": "redfish", "url": "https://bmc", "user": "root", "system_id": "1" },
                ipmi("10.0.0.9", Some("")),
            ]}),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["methods"][0]["has_password"], true);
    assert_eq!(view["methods"][1]["has_password"], false);
    let spec = serde_json::to_value(stored(&env, node).unwrap()).unwrap();
    assert_eq!(spec["methods"][0]["password"], "rf-s3cret");
    assert!(spec["methods"][1].get("password").is_none(), "{spec}");

    // 저장된 비밀번호를 다른 BMC로 보내지 않는다: 주소나 사용자를 바꾸면 다시 줘야 한다.
    for changed in [
        json!({ "kind": "redfish", "url": "https://other", "user": "root", "system_id": "1" }),
        json!({ "kind": "redfish", "url": "https://bmc", "user": "other", "system_id": "1" }),
        json!({ "kind": "redfish", "url": "https://bmc", "user": "root", "insecure": true }),
    ] {
        let (status, body) = admin
            .put("/nodes/node2/fence", json!({ "methods": [changed] }), None)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("password must be given again when the address, port or user changes (redfish: url, user, ca_pem or insecure)"),
            "{body}"
        );
    }
    // 형식이 틀리면 400
    for bad in [
        json!({ "methods": [] }),
        json!({ "methods": [{ "kind": "redfish", "url": "bmc", "user": "u" }] }),
        // 자격 증명이 든 URL
        json!({ "methods": [{ "kind": "redfish", "url": "https://root:pw@bmc", "user": "u" }] }),
        // 경로·쿼리가 든 URL, 경로를 바꾸는 system_id
        json!({ "methods": [{ "kind": "redfish", "url": "https://bmc/redfish/v1", "user": "u" }] }),
        json!({ "methods": [{ "kind": "redfish", "url": "https://bmc?a=1", "user": "u" }] }),
        json!({ "methods": [{ "kind": "redfish", "url": "https://bmc", "user": "u", "system_id": ".." }] }),
        json!({ "methods": [{ "kind": "redfish", "url": "https://bmc", "user": "u", "system_id": "1?x" }] }),
        json!({ "methods": [ipmi("10.0.0.9", None)], "extra": 1 }),
    ] {
        let (status, body) = admin.put("/nodes/node2/fence", bad.clone(), None).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {status} {body}"
        );
    }

    assert_eq!(
        admin.delete("/nodes/node2/fence").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(stored(&env, node), None);
    assert_eq!(
        admin.delete("/nodes/node2/fence").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn only_admins_handle_fencing() {
    let env = Env::start().await;
    add_node(&env, "node2", false, true).await;
    let admin = env.admin().await;
    let body = json!({ "methods": [ipmi("10.0.0.9", Some("pw"))] });
    assert_eq!(
        admin.put("/nodes/node2/fence", body.clone(), None).await.0,
        StatusCode::OK
    );
    for role in ["operator", "viewer"] {
        let user = env.user(role, role).await;
        assert_eq!(
            user.get("/nodes/node2/fence").await.0,
            StatusCode::FORBIDDEN,
            "{role}"
        );
        assert_eq!(
            user.put("/nodes/node2/fence", body.clone(), None).await.0,
            StatusCode::FORBIDDEN,
            "{role}"
        );
        assert_eq!(
            user.delete("/nodes/node2/fence").await.0,
            StatusCode::FORBIDDEN,
            "{role}"
        );
        assert_eq!(
            user.post("/nodes/node2/fence-test", json!({})).await.0,
            StatusCode::FORBIDDEN,
            "{role}"
        );
    }
}

#[tokio::test]
async fn witnesses_and_old_clusters_are_refused() {
    let env = Env::start().await;
    add_node(&env, "witness", true, true).await;
    let admin = env.admin().await;
    let body = json!({ "methods": [ipmi("10.0.0.9", Some("pw"))] });
    let (status, err) = admin.put("/nodes/witness/fence", body.clone(), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(
        err["message"].as_str().unwrap().contains("witness"),
        "{err}"
    );

    // `fast-ha`를 모르는 노드가 있으면 쓰지 않는다.
    add_node(&env, "old", false, false).await;
    let (status, err) = admin.put("/nodes/node1/fence", body.clone(), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["code"], "cluster_upgrading");
    assert!(err["message"].as_str().unwrap().contains("old"), "{err}");
    assert_eq!(stored(&env, env.node), None);
}

#[tokio::test]
async fn removing_a_node_removes_its_fencing() {
    let env = Env::start().await;
    let node = add_node(&env, "node2", false, true).await;
    let admin = env.admin().await;
    let body = json!({ "methods": [ipmi("10.0.0.9", Some("pw"))] });
    assert_eq!(
        admin.put("/nodes/node2/fence", body, None).await.0,
        StatusCode::OK
    );
    assert!(stored(&env, node).is_some());
    let (status, body) = admin.delete("/nodes/node2").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(stored(&env, node), None);
}

#[tokio::test]
async fn fence_test_reads_the_power_without_touching_it() {
    let env = Env::start().await;
    add_node(&env, "node2", false, true).await;
    let admin = env.admin().await;
    let dir = TempDir::new().unwrap();
    let d = dir.path().display();
    let command = json!({
        "kind": "command",
        // 시험은 끄기를 부르지 않는다.
        "off": format!("touch {d}/off-called"),
        "status": format!("if [ -f {d}/off-mark ]; then echo off; else echo on; fi; echo \"$ORBIVISOR_NODE_NAME\" > {d}/asked"),
    });
    let broken =
        json!({ "kind": "command", "off": "true", "status": "echo no BMC here >&2; exit 2" });
    assert_eq!(
        admin
            .put(
                "/nodes/node2/fence",
                json!({ "methods": [command, broken] }),
                None
            )
            .await
            .0,
        StatusCode::OK
    );

    // 다른 노드는 이 API 노드가 스스로 읽는다.
    let (status, result) = admin.post("/nodes/node2/fence-test", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["checked_by"], "node1");
    assert_eq!(
        result["methods"][0],
        json!({ "kind": "command", "power": "on" })
    );
    // 명령의 출력은 비밀을 담을 수 있어 답에 담지 않는다(종료 상태만, 출력은 로그에).
    assert_eq!(
        result["methods"][1],
        json!({ "kind": "command", "power": "unknown", "error": "command failed (exit status: 2)" })
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("asked")).unwrap(),
        "node2\n"
    );
    std::fs::write(dir.path().join("off-mark"), "").unwrap();
    let (_, result) = admin.post("/nodes/node2/fence-test", json!({})).await;
    assert_eq!(result["methods"][0]["power"], "off");
    assert!(!dir.path().join("off-called").exists());

    // 펜싱이 없으면 404
    assert_eq!(
        admin.post("/nodes/node1/fence-test", json!({})).await.0,
        StatusCode::NOT_FOUND
    );
    // 이 노드 자신은 이웃에게 부탁한다. 온라인 이웃이 없으면 409.
    assert_eq!(
        admin
            .put(
                "/nodes/node1/fence",
                json!({ "methods": [ipmi("10.0.0.9", Some("pw"))] }),
                None
            )
            .await
            .0,
        StatusCode::OK
    );
    let (status, err) = admin.post("/nodes/node1/fence-test", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["code"], "no_fence_checker");
}

/// 이웃 확인 RPC(`FenceStatus`)의 받는 쪽: 이 노드 저장소의 `NodeFence`로 읽는다.
#[tokio::test]
async fn the_fence_status_source_reads_with_the_stored_methods() {
    let env = Env::start().await;
    let node = add_node(&env, "node2", false, true).await;
    let admin = env.admin().await;
    let source = FenceStatusSource {
        cluster: env.cluster.clone(),
        fence: Arc::new(Fencer::default()),
    };
    let err = source.status(node).await.unwrap_err();
    assert!(err.contains("has no power fencing"), "{err}");
    let err = source.status(Uuid::nil()).await.unwrap_err();
    assert!(err.contains("not found"), "{err}");
    assert_eq!(
        admin
            .put(
                "/nodes/node2/fence",
                json!({ "methods": [{ "kind": "command", "off": "true", "status": "echo off" }] }),
                None
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        source.status(node).await.unwrap(),
        [FenceMethodStatus {
            kind: FenceKind::Command,
            power: Power::Off,
            error: None,
        }]
    );
}
