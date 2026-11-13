//! HA 그룹 API: 관리자만 만들고 바꾸고 지우며, VM이 `ha.group`으로 쓰고, 쓰는 동안 지울 수 없다.

mod common;

use common::{Env, vm_request};
use opl_types::node::{HA_PLACEMENT, NodeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn group(name: &str, spec: Value) -> Value {
    json!({ "name": name, "spec": spec })
}

/// 이 노드의 `ha-placement` 기능 표시를 지우거나 되살린다(옛 노드 흉내).
async fn set_ha_placement(env: &Env, on: bool) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != HA_PLACEMENT);
    if on {
        status.features.push(HA_PLACEMENT.into());
    }
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn admins_manage_ha_groups_and_others_read() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, body) = admin
        .post(
            "/ha-groups",
            group("rack-a", json!({ "nodes": [{ "node": "node1" }] })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 빠진 값은 기본값이다.
    assert_eq!(
        body["spec"],
        json!({ "nodes": [{ "node": "node1", "priority": 0 }], "restricted": false, "nofailback": false })
    );

    // 운영자는 읽기만 한다.
    let spec = json!({ "nodes": [{ "node": "node1", "priority": 5 }], "restricted": true });
    assert_eq!(
        operator
            .post("/ha-groups", group("other", spec.clone()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator
            .put("/ha-groups/rack-a", json!({ "spec": spec.clone() }), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator.delete("/ha-groups/rack-a").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, list) = viewer.get("/ha-groups").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"][0]["name"], "rack-a");

    // 통째로 바꾼다.
    let (status, body) = admin
        .put("/ha-groups/rack-a", json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["nodes"][0]["priority"], 5);
    assert_eq!(body["spec"]["restricted"], true);
    let (status, body) = viewer.get("/ha-groups/rack-a").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["restricted"], true);

    assert_eq!(
        admin.delete("/ha-groups/rack-a").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        viewer.get("/ha-groups/rack-a").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn invalid_groups_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for (spec, message) in [
        (json!({ "nodes": [] }), "nodes must not be empty"),
        (
            json!({ "nodes": [{ "node": "node1" }, { "node": "node1" }] }),
            "node \"node1\" is listed more than once",
        ),
        (
            json!({ "nodes": [{ "node": "node1", "priority": 1001 }] }),
            "the priority of node \"node1\" must be between 0 and 1000",
        ),
        (
            json!({ "nodes": [{ "node": "ghost" }] }),
            "node \"ghost\" not found",
        ),
    ] {
        let (status, body) = admin.post("/ha-groups", group("g", spec.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{spec}: {body}");
        assert_eq!(body["message"], message, "{spec}");
    }
    let (status, body) = admin
        .post(
            "/ha-groups",
            group(
                "g",
                json!({ "nodes": [{ "node": "node1" }], "failback": true }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// VM은 있는 그룹만 쓰고, VM이 쓰는 그룹은 지울 수 없다.
#[tokio::test]
async fn groups_in_use_cannot_be_deleted() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut req = vm_request("web");
    req["spec"]["ha"] = json!({ "group": "rack-a" });
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "HA group \"rack-a\" not found");

    let (status, body) = admin
        .post(
            "/ha-groups",
            group("rack-a", json!({ "nodes": [{ "node": "node1" }] })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["ha"]["group"], "rack-a");

    let (status, body) = admin.delete("/ha-groups/rack-a").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "group_in_use");
    assert_eq!(body["message"], "HA group \"rack-a\" is used by VM \"web\"");

    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(false);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        admin.delete("/ha-groups/rack-a").await.0,
        StatusCode::NO_CONTENT
    );
}

/// 옛 노드가 있으면 새 종류를 쓰지 않는다.
#[tokio::test]
async fn groups_wait_for_all_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let spec = json!({ "nodes": [{ "node": "node1" }] });
    let (status, _) = admin.post("/ha-groups", group("g", spec.clone())).await;
    assert_eq!(status, StatusCode::CREATED);

    set_ha_placement(&env, false).await;
    let (status, body) = admin.post("/ha-groups", group("h", spec.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    let (status, body) = admin
        .put("/ha-groups/g", json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    // 지우기는 된다.
    assert_eq!(admin.delete("/ha-groups/g").await.0, StatusCode::NO_CONTENT);
}
