//! 계획 65 API(스테이지 6): `/sdn`(첫 세우기·중앙·chassis·상태)의 검사와 자리, SDN 네트워크 만들기·서브넷 바꾸기·
//! 주소 목록, 노드 지우기의 `sdn_central_on_node`, 복제의 주소 지정, 기능 게이트. OVN 꾸러미·Task 실행은 보지 않는다
//! (Task 실행기를 멈추고 Task가 만들어졌는지만 본다 — 실행은 `sdn_tasks.rs`와 실제 `ovn` 작업).

mod common;

use common::{Env, vm_request};
use opl_types::node::{Node, NodeSpec, SDN};
use opl_types::resource::Resource;
use opl_types::sdn::{SDN_NAME, SdnCentral, SdnPhase, SdnSpec, SdnStatus};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

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

/// 이 노드에 Geneve 주소를 준다(시험 노드는 underlay가 없다).
async fn give_underlay(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.underlay = Some("127.0.0.1".parse().unwrap());
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 오프라인 노드 하나를 저장소에 둔다(underlay 있음).
async fn other_node(env: &Env, name: &str, online: bool) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: false,
            roles: None,
            host_resources: None,
        },
    );
    node.status.online = online;
    node.status.underlay = Some("10.0.0.9".parse().unwrap());
    node.status.features = vec![SDN.to_string()];
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 세워진 SDN(중앙 `centrals`)을 저장소에 바로 둔다.
async fn ready_sdn(env: &Env, centrals: &[Uuid]) {
    let status = SdnStatus {
        phase: SdnPhase::Ready,
        centrals: centrals
            .iter()
            .map(|n| SdnCentral {
                node: *n,
                address: "127.0.0.1".parse().unwrap(),
            })
            .collect(),
        init_node: centrals.first().copied(),
        ..SdnStatus::default()
    };
    match env.cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap() {
        Some(sdn) => {
            env.cluster
                .update_status::<SdnSpec>(sdn.id, &status)
                .await
                .unwrap();
        }
        None => {
            let mut sdn = Resource::new(SDN_NAME, SdnSpec {});
            sdn.status = status;
            env.cluster.create(&sdn).await.unwrap();
        }
    }
}

/// 멈춘 실행기 대신 Task를 실패로 끝낸다.
async fn finish(env: &Env, task: &Value) {
    let id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    let status = opl_types::task::TaskStatus {
        state: opl_types::task::TaskState::Failed,
        error: Some("test".into()),
        progress: None,
    };
    env.cluster
        .update_status::<opl_types::task::TaskSpec>(id, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn sdn_fields_are_gated_and_need_a_ready_sdn() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 쓰지 않는 네트워크는 지금처럼 되고 새 칸이 없다.
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "overlay", "spec": { "vxlan": { "vni": 10 } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"], json!({ "vxlan": { "vni": 10 } }), "{body}");
    // 설정 전: 상태는 단계가 없고, SDN 네트워크는 409 `sdn_not_ready`
    let (status, view) = admin.get("/sdn").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view.get("phase").is_none(), "{view}");
    assert_eq!(view["chassis"][0]["name"], "node1", "{view}");
    let sdn_req =
        json!({ "name": "tenant", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] } });
    let (status, body) = admin.post("/networks", sdn_req.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_not_ready", "{body}");
    // `br-int`는 브리지 네트워크가 가리킬 수 없다.
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "ovs", "spec": { "bridge": "br-int" } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("OVN integration bridge"),
        "{body}"
    );
    // 세운 뒤에는 된다. 서브넷 오류는 400 `invalid_subnet`.
    ready_sdn(&env, &[env.node]).await;
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "bad", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/30" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_subnet", "{body}");
    let (status, body) = admin.post("/networks", sdn_req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["sdn"], json!({}), "{body}");

    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_sdn(&env).await;
    let mut again = sdn_req;
    again["name"] = json!("tenant2");
    let (status, body) = admin.post("/networks", again).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    let (status, body) = admin.post("/sdn", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    let (status, body) = admin
        .patch("/networks/tenant", json!({ "subnets": [] }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

#[tokio::test]
async fn init_checks_the_node_and_holds_the_slot() {
    let mut env = Env::start().await;
    // Task는 만들어지기만 한다(실행하지 않는다).
    env.stop_runner().await;
    let admin = env.admin().await;
    let (status, body) = admin.post("/sdn", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("underlay"),
        "{body}"
    );
    give_underlay(&env).await;
    let (status, body) = admin.post("/sdn", json!({ "node": "nope" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 노드 인증서 밖의 주소
    let (status, body) = admin
        .post("/sdn", json!({ "node": "node1", "address": "192.0.2.7" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("not in the certificate"),
        "{body}"
    );
    let offline = other_node(&env, "node2", false).await;
    let (status, body) = admin.post("/sdn", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_offline", "{body}");

    let (status, task) = admin
        .post("/sdn", json!({ "node": "node1", "address": "127.0.0.1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "sdn-init", "address": "127.0.0.1" })
    );
    let sdn = env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(task["spec"]["target"], sdn.id.to_string());
    assert_eq!(sdn.status.init_node, Some(env.node));
    assert_eq!(
        sdn.status.pending.as_ref().unwrap().task.to_string(),
        task["id"].as_str().unwrap()
    );
    // 도는 동안 다시 하기·다른 노드는 409
    let (status, body) = admin.post("/sdn", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_busy", "{body}");
    let (_, view) = admin.get("/sdn").await;
    assert_eq!(view["phase"], "bootstrapping", "{view}");
    assert_eq!(view["tasks"], json!([task["id"]]), "{view}");
    // 중앙 더하기는 세운 뒤에만
    let (status, body) = admin
        .post("/sdn/centrals", json!({ "node": "node1" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_not_ready", "{body}");
    let _ = offline;
}

#[tokio::test]
async fn init_moves_off_a_dead_node_before_any_central() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    give_underlay(&env).await;
    let admin = env.admin().await;
    let dead = other_node(&env, "node2", false).await;
    // node2에서 세우다 멈춘 SDN(중앙 없음)
    let mut sdn = Resource::new(SDN_NAME, SdnSpec {});
    sdn.status = SdnStatus {
        init_node: Some(dead),
        ..SdnStatus::default()
    };
    env.cluster.create(&sdn).await.unwrap();
    let (status, task) = admin.post("/sdn", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let sdn = env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(sdn.status.init_node, Some(env.node));
    // 세운 SDN에서는 첫 세우기 노드만, 아직 중앙일 때만 다시 한다.
    ready_sdn(&env, &[dead]).await;
    let (status, body) = admin.post("/sdn", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_exists", "{body}");
}

#[tokio::test]
async fn centrals_and_chassis() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    give_underlay(&env).await;
    let admin = env.admin().await;
    let n2 = other_node(&env, "node2", true).await;
    ready_sdn(&env, &[n2]).await;
    // 더하기: 중앙이 둘(짝수)이 되면 경고
    let (status, headers, task) = admin
        .send_headers(Method::POST, "/sdn/centrals", json!({ "node": "node1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "sdn-central-add");
    assert!(
        headers["orbivisor-warning"]
            .to_str()
            .unwrap()
            .contains("2 central nodes"),
        "{headers:?}"
    );
    // 자리를 잡은 Task가 끝나지 않았다.
    other_node(&env, "node3", true).await;
    let (status, body) = admin
        .post("/sdn/centrals", json!({ "node": "node3" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_busy");
    finish(&env, &task).await;
    // 자리를 비운 뒤: 이미 중앙이면 409, 마지막 중앙은 409, 중앙이 아니면 400
    ready_sdn(&env, &[n2]).await;
    let (status, body) = admin
        .post("/sdn/centrals", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_exists", "{body}");
    let (status, body) = admin.delete("/sdn/centrals/node2").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_last_central", "{body}");
    let (status, body) = admin.delete("/sdn/centrals/node1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    ready_sdn(&env, &[n2, env.node]).await;
    let (status, task) = admin.delete("/sdn/centrals/node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "sdn-central-remove");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    finish(&env, &task).await;

    // chassis: 노드마다 Task(대상 = 노드). 둘 다·둘 다 아님은 400
    let (status, tasks) = admin.post("/sdn/chassis", json!({ "all": true })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{tasks}");
    let nodes: Vec<&str> = tasks
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["spec"]["target"].as_str().unwrap())
        .collect();
    assert_eq!(nodes.len(), 3, "{tasks}");
    let (status, _) = admin
        .post("/sdn/chassis", json!({ "all": true, "nodes": ["node1"] }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, tasks) = admin
        .post("/sdn/chassis", json!({ "nodes": ["node1", "node1"] }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{tasks}");
    assert_eq!(tasks.as_array().unwrap().len(), 1);

    // 중앙 노드는 클러스터에서 지우기 전에 중앙에서 빼야 한다.
    let mut node2 = env.cluster.get::<NodeSpec>(n2).unwrap().unwrap();
    node2.status.online = false;
    env.cluster
        .update_status::<NodeSpec>(n2, &node2.status)
        .await
        .unwrap();
    let (status, body) = admin.delete("/nodes/node2").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_central_on_node", "{body}");

    // 읽기만 하는 사용자는 상태를 보지만 바꾸지 못한다.
    let viewer = env.user("viewer", "viewer").await;
    let (status, _) = viewer.get("/sdn").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = viewer.post("/sdn/chassis", json!({ "all": true })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn subnets_change_and_addresses_are_listed() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, &[env.node]).await;
    let (status, net) = admin
        .post(
            "/networks",
            json!({ "name": "tenant", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/29" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{net}");
    let mut req = vm_request("a");
    req["spec"]["nics"] = json!([{ "network": "tenant", "model": "virtio" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["nics"][0]["ips"], json!(["10.10.0.2"]));

    // 주소 목록: OVN을 읽지 않는 노드면 live가 null이고 까닭이 있다.
    let (status, ips) = admin.get("/networks/tenant/ips").await;
    assert_eq!(status, StatusCode::OK, "{ips}");
    assert_eq!(
        ips["subnets"],
        json!([{ "cidr": "10.10.0.0/29", "size": 5, "used": 1, "free": 4 }])
    );
    assert_eq!(ips["allocations"][0]["vm_name"], "a");
    assert_eq!(ips["allocations"][0]["ips"], json!(["10.10.0.2"]));
    assert!(ips["allocations"][0]["live"].is_null(), "{ips}");
    assert!(ips["live_error"].is_string(), "{ips}");

    // 서브넷 더하기·바꾸기(리비전 확인). 있던 NIC에는 주소를 저절로 주지 않는다.
    let rev = net["revision"].as_u64().unwrap();
    let (status, body) = admin
        .patch(
            "/networks/tenant",
            json!({ "subnets": [
                { "cidr": "10.10.0.0/29", "dns_servers": ["10.10.0.6"] },
                { "cidr": "fd00::/64", "dhcp": false }
            ] }),
            Some(rev),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["subnets"][1]["cidr"], "fd00::/64");
    let (_, vm) = admin.get("/vms/a").await;
    assert_eq!(vm["spec"]["nics"][0]["ips"], json!(["10.10.0.2"]));
    // 낡은 리비전은 409
    let (status, _) = admin
        .patch("/networks/tenant", json!({ "subnets": [] }), Some(rev))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // 게이트웨이는 바꿀 수 없다.
    let (status, body) = admin
        .patch(
            "/networks/tenant",
            json!({ "subnets": [{ "cidr": "10.10.0.0/29", "gateway": "10.10.0.6" }] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_subnet", "{body}");
    // 쓰는 서브넷은 뺄 수 없고, 쓰지 않는 서브넷은 뺀다.
    let (status, body) = admin
        .patch(
            "/networks/tenant",
            json!({ "subnets": [{ "cidr": "fd00::/64", "dhcp": false }] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "subnet_in_use", "{body}");
    assert!(body["message"].as_str().unwrap().contains('a'), "{body}");
    let (status, body) = admin
        .patch(
            "/networks/tenant",
            json!({ "subnets": [{ "cidr": "10.10.0.0/29" }] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 단순 네트워크는 바꾸지 못하고 주소 목록이 없다.
    admin
        .post(
            "/networks",
            json!({ "name": "ov", "spec": { "vxlan": { "vni": 5 } } }),
        )
        .await;
    let (status, body) = admin
        .patch("/networks/ov", json!({ "subnets": [] }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "network_immutable", "{body}");
    let (status, _) = admin.get("/networks/ov/ips").await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn clones_take_requested_addresses() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, &[env.node]).await;
    admin
        .post(
            "/networks",
            json!({ "name": "tenant", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] } }),
        )
        .await;
    let mut req = vm_request("src");
    req["spec"]["nics"] = json!([{ "network": "tenant", "model": "virtio" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    // 쓰는 주소·서브넷 밖은 미리 거절한다(Task를 만들지 않는다).
    let (status, body) = admin
        .post(
            "/vms/src/clone",
            json!({ "name": "copy", "ips": [["10.10.0.2"]] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ip_in_use", "{body}");
    let (status, body) = admin
        .post(
            "/vms/src/clone",
            json!({ "name": "copy", "ips": [["10.99.0.2"]] }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = admin
        .post(
            "/vms/src/clone",
            json!({ "name": "copy", "ips": [[], ["10.10.0.9"]] }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 준 주소로 복제한다.
    let (status, task) = admin
        .post(
            "/vms/src/clone",
            json!({ "name": "copy", "ips": [["10.10.0.50"]] }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    let (_, copy) = admin.get("/vms/copy").await;
    assert_eq!(
        copy["spec"]["nics"][0]["ips"],
        json!(["10.10.0.50"]),
        "{copy}"
    );
    let _: Value = copy;
}
