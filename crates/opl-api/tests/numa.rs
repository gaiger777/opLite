//! 계획 64 API: NUMA·전용 CPU·hugepages의 기능 게이트(스테이지 1 — API를 열기 전에는 VM·노드의 새 칸을 받지 않는다).

mod common;

use common::{Env, vm_request};
use opl_types::node::{NUMA, NodeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 이 노드가 `numa`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_numa(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != NUMA);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 새 칸 하나씩 든 VM 만들기 요청
fn tries() -> Vec<Value> {
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut req = vm_request("new");
        edit(&mut req["spec"]);
        req
    };
    vec![
        with(&|s| s["cpu"]["policy"] = json!("dedicated")),
        with(&|s| {
            s["cpu"]["policy"] = json!("dedicated");
            s["cpu"]["thread_policy"] = json!("isolate");
            s["cpu"]["emulator"] = json!("isolate");
        }),
        with(&|s| s["numa"] = json!({ "nodes": 1 })),
        with(&|s| s["numa"] = json!({ "nodes": "auto", "memory": "strict" })),
        with(&|s| s["hugepages"] = json!("2M")),
        with(&|s| s["hugepages"] = json!("any")),
    ]
}

#[tokio::test]
async fn numa_fields_are_gated() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 쓰지 않는 VM은 지금처럼 되고 새 칸이 없다.
    let (status, body) = admin.post("/vms", vm_request("plain")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    for k in ["numa", "hugepages"] {
        assert!(body["spec"].get(k).is_none(), "{body}");
    }
    for k in ["policy", "thread_policy", "emulator"] {
        assert!(body["spec"]["cpu"].get(k).is_none(), "{body}");
    }
    // 모든 노드가 알려도 스테이지 5 전에는 400이다.
    for req in tries() {
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{req}: {body}");
    }
    // 바꾸기도 같다.
    let (_, vm) = admin.get("/vms/plain").await;
    let mut spec = vm["spec"].clone();
    spec["hugepages"] = json!("1G");
    let (status, body) = admin.put("/vms/plain", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 노드 호스트 자원 설정
    let node = format!("/nodes/{}", env.node);
    let (_, n) = admin.get(&node).await;
    let mut nspec = n["spec"].clone();
    let (status, body) = admin
        .put(&node, json!({ "spec": nspec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("host_resources").is_none(), "{body}");
    nspec["host_resources"] = json!({ "dedicated_cpus": "2-3" });
    let (status, body) = admin
        .put(&node, json!({ "spec": nspec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 폴더 할당량 dedicated_vcpus는 기능만 본다(상태 머신이 이미 센다).
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": "db", "kind": "vm", "quota": { "dedicated_vcpus": 8 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["quota"]["dedicated_vcpus"], 8, "{body}");
    let folder = body["id"].as_str().unwrap().to_string();

    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_numa(&env).await;
    for req in tries() {
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{req}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
    let (status, body) = admin.put(&node, json!({ "spec": nspec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": "db2", "kind": "vm", "quota": { "dedicated_vcpus": 8 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = admin
        .patch(
            &format!("/folders/{folder}"),
            json!({ "quota": { "vcpus": 4, "dedicated_vcpus": 2 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // 옛 칸만이면 된다.
    let (status, body) = admin
        .patch(
            &format!("/folders/{folder}"),
            json!({ "quota": { "vcpus": 4 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin.post("/vms", vm_request("plain2")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 문법이 틀린 칸은 JSON을 풀지 못한다.
    for bad in [
        json!({ "cpu": { "cores": 1, "policy": "exclusive" } }),
        json!({ "hugepages": "4K" }),
        json!({ "numa": { "nodes": "two" } }),
    ] {
        let mut req = vm_request("bad");
        for (k, v) in bad.as_object().unwrap() {
            req["spec"][k] = v.clone();
        }
        let (status, body) = admin.post("/vms", req).await;
        assert!(status.is_client_error(), "{bad}: {status} {body}");
    }
}
