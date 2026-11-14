//! HA 검사 API: 관리자만 만들고 바꾸고 지우며, 모두 읽는다. 노드 덮기·풀·이름·개수를 확인한다.

mod common;

use common::Env;
use opl_types::ha_check::{HaCheckRole, HaCheckSpec, HaCheckTarget, MAX_HA_CHECKS};
use opl_types::node::{HA_CHECKS, NodeSpec};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn check(name: &str, spec: Value) -> Value {
    json!({ "name": name, "spec": spec })
}

/// 이 노드의 `ha-checks` 기능 표시를 지운다(옛 노드 흉내).
async fn drop_ha_checks(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != HA_CHECKS);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn admins_manage_ha_checks_and_others_read() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let spec = json!({ "role": "vm", "target": { "kind": "link", "iface": "bond0" } });
    let (status, body) = admin.post("/ha-checks", check("vmnet", spec)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 기본값은 쓰지 않는다.
    assert_eq!(
        body["spec"],
        json!({ "role": "vm", "target": { "kind": "link", "iface": "bond0" } })
    );

    // 운영자는 읽기만 한다.
    let other = json!({ "role": "management", "target": { "kind": "link", "iface": "eth0" } });
    assert_eq!(
        operator
            .post("/ha-checks", check("other", other.clone()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator
            .put("/ha-checks/vmnet", json!({ "spec": other }), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator.delete("/ha-checks/vmnet").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, list) = viewer.get("/ha-checks").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"][0]["name"], "vmnet");
    let (status, list) = operator.get("/ha-checks").await;
    assert_eq!(status, StatusCode::OK, "{list}");

    // 통째로 바꾼다. 노드 덮기가 저장되고 그대로 읽힌다.
    let spec = json!({
        "role": "vm",
        "target": { "kind": "ping", "addresses": ["10.0.0.1", "10.0.0.2"], "need": 2 },
        "on_fail": "warn",
        "grace_secs": 30,
        "nodes": { "node1": { "kind": "ping", "addresses": ["10.1.0.1"], "via": "bond1", "need": 1 } },
        "enabled": false
    });
    let (status, body) = admin
        .put("/ha-checks/vmnet", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"], spec);
    let (status, body) = viewer.get("/ha-checks/vmnet").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"], spec);
    let stored = env
        .cluster
        .get_by_name::<HaCheckSpec>("vmnet")
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.spec.target_for("node1"),
        &HaCheckTarget::Ping {
            addresses: vec!["10.1.0.1".parse().unwrap()],
            via: Some("bond1".into()),
            need: 1,
        }
    );

    assert_eq!(
        admin.delete("/ha-checks/vmnet").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        viewer.get("/ha-checks/vmnet").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn invalid_checks_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = |name: &str, backend: Value, shared: bool| json!({ "name": name, "spec": { "backend": backend, "shared": shared } });
    let dir = env.dir.path().to_str().unwrap();
    for body in [
        pool("nas", json!({ "kind": "dir", "path": dir }), true),
        pool("local", json!({ "kind": "dir", "path": dir }), false),
        pool(
            "thin",
            json!({ "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" }),
            false,
        ),
    ] {
        let (status, body) = admin.post("/storage-pools", body).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let shared_pool_message = "pool checks need a shared pool (nfs, lvm-shared, rbd or shared dir)";
    for (name, spec, message) in [
        (
            "c",
            json!({ "role": "vm", "target": { "kind": "link", "iface": "eth0" }, "grace_secs": 9 }),
            "grace_secs 9 is out of range: use 10-600",
        ),
        (
            "c",
            json!({ "role": "vm", "target": { "kind": "link", "iface": "eth0" }, "on_fail": "fence",
                    "nodes": { "ghost": { "kind": "link", "iface": "eth1" } } }),
            "node \"ghost\" not found",
        ),
        (
            "c",
            json!({ "role": "storage", "target": { "kind": "pool", "name": "missing" } }),
            "storage pool \"missing\" not found",
        ),
        (
            "c",
            json!({ "role": "storage", "target": { "kind": "pool", "name": "thin" } }),
            shared_pool_message,
        ),
        (
            "c",
            json!({ "role": "storage", "target": { "kind": "pool", "name": "local" } }),
            shared_pool_message,
        ),
        (
            "c",
            json!({ "role": "storage", "target": { "kind": "pool", "name": "nas" },
                    "nodes": { "node1": { "kind": "pool", "name": "local" } } }),
            shared_pool_message,
        ),
        (
            "cluster-link-1",
            json!({ "role": "management", "target": { "kind": "link", "iface": "eth0" } }),
            "names starting with \"cluster-link-\" are reserved for the automatic cluster link checks",
        ),
    ] {
        let (status, body) = admin.post("/ha-checks", check(name, spec.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{spec}: {body}");
        assert_eq!(body["message"], message, "{spec}");
    }
    // 모르는 필드
    let (status, body) = admin
        .post(
            "/ha-checks",
            check(
                "c",
                json!({ "role": "vm", "target": { "kind": "link", "iface": "eth0", "x": 1 } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 공유 풀은 받는다.
    let (status, body) = admin
        .post(
            "/ha-checks",
            check(
                "stor",
                json!({ "role": "storage", "target": { "kind": "pool", "name": "nas" } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 바꾸기도 같은 검사를 한다.
    let (status, body) = admin
        .put(
            "/ha-checks/stor",
            json!({ "spec": { "role": "storage", "target": { "kind": "pool", "name": "thin" } } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], shared_pool_message);
}

/// 검사 수 상한(리스 갱신 크기)
#[tokio::test]
async fn at_most_64_checks() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for i in 1..MAX_HA_CHECKS {
        let c = Resource::new(
            format!("c{i}"),
            HaCheckSpec {
                role: HaCheckRole::Management,
                target: HaCheckTarget::Link {
                    iface: "eth0".into(),
                },
                on_fail: None,
                grace_secs: 10,
                nodes: Default::default(),
                enabled: true,
            },
        );
        env.cluster.create(&c).await.unwrap();
    }
    let spec = json!({ "role": "management", "target": { "kind": "link", "iface": "eth0" } });
    let (status, body) = admin.post("/ha-checks", check("last", spec.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin.post("/ha-checks", check("extra", spec.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "a cluster can have at most 64 HA checks; delete one first"
    );
    // 있는 검사는 바꿀 수 있다.
    let (status, body) = admin
        .put("/ha-checks/last", json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 옛 노드가 있으면 새 종류를 쓰지 않는다.
#[tokio::test]
async fn checks_wait_for_all_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let spec = json!({ "role": "vm", "target": { "kind": "link", "iface": "eth0" } });
    let (status, _) = admin.post("/ha-checks", check("a", spec.clone())).await;
    assert_eq!(status, StatusCode::CREATED);

    drop_ha_checks(&env).await;
    let (status, body) = admin.post("/ha-checks", check("b", spec.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    let (status, body) = admin
        .put("/ha-checks/a", json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    // 지우기는 된다.
    assert_eq!(admin.delete("/ha-checks/a").await.0, StatusCode::NO_CONTENT);
}
