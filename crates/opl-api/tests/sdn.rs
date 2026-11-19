//! 계획 65 API: SDN(OVN)의 기능 게이트(스테이지 1 — API를 열기 전에는 네트워크 `sdn`·`subnets`를 받지 않는다. NIC
//! `ips`는 옛 노드가 있으면 409, SDN 네트워크가 아니면 400 — 스테이지 4).

mod common;

use common::{Env, vm_request};
use opl_types::node::{NodeSpec, SDN};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 이 노드가 `sdn`을 모르는 것으로 적는다(옛 노드처럼).
async fn drop_sdn(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != SDN);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 새 칸 하나씩 든 네트워크 만들기 요청
fn network_tries() -> Vec<Value> {
    vec![
        json!({ "name": "tenant", "spec": { "sdn": {} } }),
        json!({ "name": "tenant", "spec": {
            "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }]
        } }),
        // 서브넷만 있어도(검증에서 막힐 것이라도) 게이트가 먼저다.
        json!({ "name": "tenant", "spec": {
            "vxlan": { "vni": 77 }, "subnets": [{ "cidr": "10.10.0.0/24" }]
        } }),
    ]
}

/// NIC 주소가 든 VM 만들기 요청
fn vm_try() -> Value {
    let mut req = vm_request("addr");
    req["spec"]["nics"][0]["ips"] = json!(["10.0.2.15"]);
    req
}

#[tokio::test]
async fn sdn_fields_are_gated() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 쓰지 않는 네트워크·VM은 지금처럼 되고 새 칸이 없다.
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "overlay", "spec": { "vxlan": { "vni": 10 } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"], json!({ "vxlan": { "vni": 10 } }), "{body}");
    let (status, body) = admin.post("/vms", vm_request("plain")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["spec"]["nics"][0].get("ips").is_none(), "{body}");

    // 모든 노드가 알려도 SDN 네트워크는 스테이지 6 전에는 400이다. `user` 네트워크 NIC의 주소는 400
    // `ips_without_sdn`(스테이지 4)이다.
    for req in network_tries() {
        let (status, body) = admin.post("/networks", req.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{req}: {body}");
        assert_eq!(
            body["message"], "SDN (OVN) networks are not available yet",
            "{body}"
        );
    }
    let (status, body) = admin.post("/vms", vm_try()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "ips_without_sdn", "{body}");
    // 바꾸기도 같다.
    let (_, vm) = admin.get("/vms/plain").await;
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["ips"] = json!(["10.0.2.15"]);
    let (status, body) = admin
        .put("/vms/plain", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (_, after) = admin.get("/vms/plain").await;
    assert!(after["spec"]["nics"][0].get("ips").is_none(), "{after}");

    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_sdn(&env).await;
    for req in network_tries() {
        let (status, body) = admin.post("/networks", req.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{req}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
    let (status, body) = admin.post("/vms", vm_try()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    let (status, body) = admin.put("/vms/plain", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    // 새 칸이 없으면 옛 노드가 있어도 그대로다.
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "overlay2", "spec": { "vxlan": { "vni": 11 } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}
