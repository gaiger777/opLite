//! 계획 63 API: CPU 모델·플래그의 기능 게이트(스테이지 1 — 모델 API를 열기 전에는 `host`·`max` 밖을 받지 않는다).

mod common;

use common::{Env, vm_request};
use opl_types::node::{CPU_MODELS, NodeSpec};
use reqwest::StatusCode;
use serde_json::json;

/// 이 노드가 `cpu-models`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_cpu_models(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != CPU_MODELS);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn cpu_models_and_flags_are_gated() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // host·max는 지금처럼 된다.
    for (name, model) in [("h", "host"), ("m", "max")] {
        let mut req = vm_request(name);
        req["spec"]["cpu"]["model"] = json!(model);
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["spec"]["cpu"]["model"], model);
        assert!(body["spec"]["cpu"].get("flags").is_none(), "{body}");
    }
    let tries = [
        json!({ "model": "x86-64-v3" }),
        json!({ "model": "cluster" }),
        json!({ "model": "Icelake-Server-v7" }),
        json!({ "model": "host", "flags": ["-avx512vbmi"] }),
    ];
    // 모든 노드가 알려도 스테이지 5 전에는 400이다.
    for cpu in &tries {
        let mut req = vm_request("new");
        req["spec"]["cpu"]["model"] = cpu["model"].clone();
        if let Some(f) = cpu.get("flags") {
            req["spec"]["cpu"]["flags"] = f.clone();
        }
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{cpu}: {body}");
    }
    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_cpu_models(&env).await;
    for cpu in &tries {
        let mut req = vm_request("new");
        req["spec"]["cpu"]["model"] = cpu["model"].clone();
        if let Some(f) = cpu.get("flags") {
            req["spec"]["cpu"]["flags"] = f.clone();
        }
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CONFLICT, "{cpu}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
    // 문법이 틀린 모델·플래그는 JSON을 풀지 못한다.
    for (model, flags) in [("base", json!([])), ("host", json!(["avx"]))] {
        let mut req = vm_request("bad");
        req["spec"]["cpu"]["model"] = json!(model);
        req["spec"]["cpu"]["flags"] = flags;
        let (status, body) = admin.post("/vms", req).await;
        assert!(status.is_client_error(), "{model}: {status} {body}");
    }
}
