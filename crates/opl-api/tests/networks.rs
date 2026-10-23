//! 네트워크 API: 관리자만 만들고 지우며, VM의 NIC가 쓰고, 브리지가 있는 노드에만 배치된다.

mod common;

use common::{Env, vm_request};
use opl_types::node::{BridgeInfo, NodeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn network(name: &str, bridge: &str, vlan: Option<u16>) -> Value {
    json!({ "name": name, "spec": { "bridge": bridge, "vlan": vlan } })
}

#[tokio::test]
async fn admins_manage_networks_and_others_read() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, body) = admin
        .post("/networks", network("lan", "vmbr0", Some(10)))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["vlan"], 10);
    assert_eq!(
        operator
            .post("/networks", network("dmz", "vmbr0", None))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, list) = viewer.get("/networks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"][0]["name"], "lan");
    assert_eq!(viewer.get("/networks/lan").await.0, StatusCode::OK);

    for (bad, why) in [
        (network("user", "vmbr0", None), "reserved name"),
        (network("bad-bridge", "a/b", None), "bridge name"),
        (network("bad-vlan", "vmbr0", Some(4095)), "VLAN tag"),
    ] {
        let (status, body) = admin.post("/networks", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request");
    }
    let (status, body) = admin.post("/networks", network("lan", "vmbr1", None)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "name_taken");
}

#[tokio::test]
async fn vms_use_existing_networks_which_cannot_be_deleted_while_used() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut vm = vm_request("web");
    vm["spec"]["nics"][0]["network"] = json!("lan");
    let (status, body) = admin.post("/vms", vm.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("lan"), "{body}");

    admin.post("/networks", network("lan", "vmbr0", None)).await;
    assert_eq!(admin.post("/vms", vm).await.0, StatusCode::CREATED);
    let (status, body) = admin.delete("/networks/lan").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "network_in_use");

    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        admin.delete("/networks/lan").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(admin.get("/networks/lan").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn vms_are_placed_on_nodes_with_the_bridge() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin
        .post("/networks", network("dmz", "vmbr0", Some(20)))
        .await;
    let mut vm = vm_request("web");
    vm["spec"]["nics"][0]["network"] = json!("dmz");
    admin.post("/vms", vm).await;

    // 이 노드에는 브리지가 없다.
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node");

    // VLAN-aware 브리지가 생기면 배치된다.
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status.clone();
    status.bridges = vec![BridgeInfo {
        name: "vmbr0".into(),
        vlan_filtering: true,
    }];
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
}
