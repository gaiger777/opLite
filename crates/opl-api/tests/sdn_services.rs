//! 계획 67 스테이지 1: SDN 서비스의 새 칸(NIC `qos`, SDN 경보 조건, 할당량 `networks`·`load_balancers`, 부하 분산기·
//! 네트워크 옮기기의 폴더 옮기기)은 API 스테이지(5) 전까지 임시로 막힌다: 옛 노드가 남았으면 409 `cluster_upgrading`,
//! 아니면 400. 이 칸을 쓰지 않는 요청과 저장 모양은 지금 그대로다.

mod common;

use common::{Env, vm_request};
use opl_types::node::{NodeSpec, SDN_SERVICES};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 이 노드가 `sdn-services`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_services(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != SDN_SERVICES);
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
            .contains("not available yet (SDN services)"),
        "{body}"
    );
}

fn upgrading(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

#[tokio::test]
async fn sdn_service_fields_are_gated_until_the_api_stage() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut qos_vm = vm_request("qos");
    qos_vm["spec"]["nics"][0]["qos"] = json!({ "out_mbps": 100 });
    let sdn_rule = json!({
        "name": "sdn",
        "spec": { "condition": { "kind": "sdn_chassis_down" }, "severity": "warning", "targets": ["ops"] }
    });
    let net_quota = json!({ "name": "tenant", "kind": "network", "quota": { "networks": 2 } });

    let (status, body) = admin.post("/vms", qos_vm.clone()).await;
    not_yet(status, &body);
    let (status, body) = admin.post("/alert-rules", sdn_rule.clone()).await;
    not_yet(status, &body);
    let (status, body) = admin.post("/folders", net_quota.clone()).await;
    not_yet(status, &body);

    // 새 칸을 쓰지 않는 요청은 지금 그대로이고 저장 모양에 새 칸이 없다.
    let (status, body) = admin.post("/vms", vm_request("plain")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["spec"]["nics"][0].get("qos").is_none(), "{body}");
    // 있는 VM에 QoS를 더하는 바꾸기도 막힌다.
    let mut spec = body["spec"].clone();
    spec["nics"][0]["qos"] = json!({ "in_mbps": 10 });
    let (status, body) = admin
        .put(
            "/vms/plain",
            json!({ "spec": spec }),
            body["revision"].as_u64(),
        )
        .await;
    not_yet(status, &body);
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": "tenant", "kind": "network", "quota": { "routers": 1 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["quota"], json!({ "routers": 1 }), "{body}");
    let folder = body["id"].as_str().unwrap().to_string();
    let (status, body) = admin
        .patch(
            &format!("/folders/{folder}"),
            json!({ "quota": { "load_balancers": 1 } }),
            None,
        )
        .await;
    not_yet(status, &body);
    // 부하 분산기·네트워크 옮기기는 아직 옮길 수 없다(항목의 오류).
    let (status, body) = admin
        .post(
            &format!("/folders/{folder}/members"),
            json!({ "items": [
                { "kind": "load-balancer", "key": "lb1" },
                { "kind": "network-migration", "key": "m1" }
            ] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for item in body["items"].as_array().unwrap() {
        assert_eq!(item["ok"], false, "{body}");
        assert!(
            item["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not available yet"),
            "{body}"
        );
    }

    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_services(&env).await;
    let (status, body) = admin.post("/vms", qos_vm).await;
    upgrading(status, &body);
    let (status, body) = admin.post("/alert-rules", sdn_rule).await;
    upgrading(status, &body);
    let (status, body) = admin.post("/folders", net_quota).await;
    upgrading(status, &body);
}
