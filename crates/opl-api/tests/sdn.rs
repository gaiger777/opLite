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
            sdn: None,
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
            let mut sdn = Resource::new(SDN_NAME, SdnSpec::default());
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
    let mut sdn = Resource::new(SDN_NAME, SdnSpec::default());
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
    // `live=false`면 OVN을 읽지 않는다(까닭도 없다).
    let (status, quick) = admin.get("/networks/tenant/ips?live=false").await;
    assert_eq!(status, StatusCode::OK, "{quick}");
    assert!(quick.get("live_error").is_none(), "{quick}");
    assert_eq!(quick["subnets"], ips["subnets"]);

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

async fn folder(admin: &common::Client, name: &str, kind: &str) -> String {
    let (status, body) = admin
        .post("/folders", json!({ "name": name, "kind": kind }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

/// 폴더 둘(teamA·teamB)이 함께 쓰는 SDN 네트워크 `shared`(네트워크 폴더 netA)와 각 폴더의 VM a1·b1. alice는 teamA·netA의
/// operator다.
async fn shared_world(env: &Env) -> (common::Client, common::Client, String, String) {
    let admin = env.admin().await;
    ready_sdn(env, &[env.node]).await;
    let a = folder(&admin, "teamA", "vm").await;
    let b = folder(&admin, "teamB", "vm").await;
    let net = folder(&admin, "netA", "network").await;
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "shared", "spec": { "sdn": {}, "folder": net,
                    "subnets": [{ "cidr": "10.10.0.0/24" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    for (name, f) in [("a1", &a), ("b1", &b)] {
        let mut req = vm_request(name);
        req["spec"]["folder"] = json!(f);
        req["spec"]["nics"] = json!([{ "network": "shared", "model": "virtio" }]);
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": "alice", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let alice_id = body["id"].as_str().unwrap().to_string();
    for target in [&a, &net] {
        let (status, body) = admin
            .post(
                "/grants",
                json!({
                    "principal": { "type": "user", "id": alice_id },
                    "role": { "type": "builtin", "role": "operator" },
                    "target": { "type": "folder", "id": target },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let alice = env.login("alice", "password-1").await;
    (admin, alice, a, b)
}

/// 리뷰 1 I-1·I-2: 네트워크를 함께 쓰는 다른 폴더의 VM은 주소 목록에도 `ip_in_use` 문구에도 드러나지 않고, 복제는 권한을
/// 본 뒤에야 주소를 본다.
#[tokio::test]
async fn folder_users_see_only_their_own_addresses() {
    let env = Env::start().await;
    let (admin, alice, team_a, team_b) = shared_world(&env).await;
    let (_, b1) = admin.get("/vms/b1").await;
    let taken = b1["spec"]["nics"][0]["ips"][0]
        .as_str()
        .unwrap()
        .to_string();

    // 주소 목록: alice는 a1만 보고, b1은 수로만 센다. 사용 수는 모두다.
    let (status, ips) = alice.get("/networks/shared/ips").await;
    assert_eq!(status, StatusCode::OK, "{ips}");
    let names: Vec<&str> = ips["allocations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["vm_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a1"], "{ips}");
    assert_eq!(ips["hidden"], 1, "{ips}");
    assert_eq!(ips["subnets"][0]["used"], 2, "{ips}");
    assert!(!ips.to_string().contains(&taken), "{ips}");
    let (_, all) = admin.get("/networks/shared/ips").await;
    assert_eq!(all["allocations"].as_array().unwrap().len(), 2, "{all}");
    assert!(all.get("hidden").is_none(), "{all}");

    // 쓰는 주소: 409지만 읽을 수 없는 VM의 이름은 대지 않는다(관리자에게는 댄다).
    let mut req = vm_request("a2");
    req["spec"]["folder"] = json!(team_a);
    req["spec"]["nics"] = json!([{ "network": "shared", "model": "virtio", "ips": [taken] }]);
    let (status, body) = alice.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ip_in_use", "{body}");
    assert!(!body["message"].as_str().unwrap().contains("b1"), "{body}");
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["message"].as_str().unwrap().contains("b1"), "{body}");

    // 복제: 새 폴더에 만들 권한이 없으면 주소를 보기 전에 거절한다(쓰는 주소인지 캐 볼 수 없다).
    let (status, body) = alice
        .post(
            "/vms/a1/clone",
            json!({ "name": "probe", "folder": team_b, "ips": [[taken]] }),
        )
        .await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
        "{status}: {body}"
    );
    let (status, body) = alice
        .post(
            "/vms/a1/clone",
            json!({ "name": "probe", "ips": [[taken]] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(!body["message"].as_str().unwrap().contains("b1"), "{body}");
}

/// 리뷰 1 I-3: IPv4 대응 IPv6는 IPv4로 저장되고, IPv4와 함께 주면 한 NIC에 IPv4 둘이라 400이다.
#[tokio::test]
async fn mapped_ipv6_addresses_count_as_ipv4() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, &[env.node]).await;
    admin
        .post(
            "/networks",
            json!({ "name": "tenant", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] } }),
        )
        .await;
    let mut req = vm_request("two");
    req["spec"]["nics"] = json!([{ "network": "tenant", "model": "virtio",
                                   "ips": ["::ffff:10.10.0.5", "10.10.0.6"] }]);
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    req["spec"]["nics"][0]["ips"] = json!(["::ffff:10.10.0.5"]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["nics"][0]["ips"], json!(["10.10.0.5"]), "{vm}");
    let (status, body) = admin
        .post(
            "/vms/two/clone",
            json!({ "name": "copy", "ips": [["::ffff:10.10.0.7", "10.10.0.8"]] }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// 리뷰 1 웹 #2: 복제가 NIC를 SDN 네트워크로 옮기면 그 NIC의 cloud-init 고정 주소가 빠진다(경고). 보안 그룹은 계획 66부터
/// SDN NIC에서도 OVN ACL이라 그대로 따라간다(모든 노드가 `sdn-routing`을 알 때). 새 VM은 그대로 다시 저장할 수 있다.
#[tokio::test]
async fn clones_onto_sdn_keep_groups_and_drop_static_cloud_init() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, &[env.node]).await;
    for body in [
        json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        json!({ "name": "tenant", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] } }),
    ] {
        let (status, b) = admin.post("/networks", body).await;
        assert_eq!(status, StatusCode::CREATED, "{b}");
    }
    let (status, b) = admin
        .post("/security-groups", json!({ "name": "ssh", "spec": {} }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let mut req = vm_request("src");
    req["spec"]["nics"] =
        json!([{ "network": "lan", "model": "virtio", "security_groups": ["ssh"] }]);
    req["spec"]["cloud_init"] = json!({ "user": "orbi",
        "networks": [{ "ipv4": { "mode": "static", "address": "192.168.0.5/24" } }] });
    let (status, b) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    // 주소를 주어도 미리 보기가 그룹 때문에 거절하지 않는다.
    let (status, task) = admin
        .post(
            "/vms/src/clone",
            json!({ "name": "copy", "nics": ["tenant"], "ips": [["10.10.0.9"]] }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    let warning = done["status"]["error"].as_str().unwrap_or_default();
    assert!(!warning.contains("security groups"), "{done}");
    assert!(warning.contains("cloud-init"), "{done}");
    let (_, copy) = admin.get("/vms/copy").await;
    let nic = &copy["spec"]["nics"][0];
    assert_eq!(nic["network"], "tenant", "{copy}");
    assert_eq!(nic["security_groups"], json!(["ssh"]), "{copy}");
    assert_eq!(nic["ips"], json!(["10.10.0.9"]), "{copy}");
    assert_eq!(
        copy["spec"]["cloud_init"]["networks"],
        json!([{}]),
        "{copy}"
    );
    let rev = copy["revision"].as_u64().unwrap();
    let mut spec = copy["spec"].clone();
    spec["memory_mib"] = json!(512);
    let (status, body) = admin
        .put("/vms/copy", json!({ "spec": spec }), Some(rev))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 리뷰 1 I-5·I-7: 루프백이 노드 주소가 아니면 중앙 주소가 될 수 없고, 끝나지 않은 첫 세우기를 주소 없이 다시 하면
/// 지난번 주소로 한다.
#[tokio::test]
async fn init_retries_keep_the_address() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    give_underlay(&env).await;
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.cluster_addresses = vec!["10.1.0.5:7444".into()];
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let admin = env.admin().await;
    for bad in ["::1", "0.0.0.0", "169.254.1.1"] {
        let (status, body) = admin
            .post("/sdn", json!({ "node": "node1", "address": bad }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, task) = admin
        .post("/sdn", json!({ "node": "node1", "address": "10.1.0.5" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    finish(&env, &task).await;
    let (status, again) = admin.post("/sdn", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{again}");
    assert_eq!(
        again["spec"]["op"],
        json!({ "kind": "sdn-init", "address": "10.1.0.5" })
    );
    let sdn = env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(sdn.status.init_address, Some("10.1.0.5".parse().unwrap()));
}

/// 리뷰 1 I-7: 중앙 목록에 없지만 이 SDN의 DB를 가진 노드(끝나지 않은 중앙 더하기)는 중앙 빼기로 떠나게 한다. 꺼진 채
/// 지우면 기록해 두고 다른 중앙이 내쫓는다(리뷰 2 N-2).
#[tokio::test]
async fn half_joined_centrals_can_be_removed() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let n2 = other_node(&env, "node2", true).await;
    ready_sdn(&env, &[env.node]).await;
    let (status, body) = admin.delete("/sdn/centrals/node2").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let mut node2 = env.cluster.get::<NodeSpec>(n2).unwrap().unwrap();
    node2.status.sdn = Some(opl_types::sdn::NodeSdn {
        central: Some(opl_types::sdn::CentralHealth {
            nb: opl_types::sdn::RaftRole::Follower,
            sb: opl_types::sdn::RaftRole::Follower,
            northd_active: false,
        }),
        ..Default::default()
    });
    env.cluster
        .update_status::<NodeSpec>(n2, &node2.status)
        .await
        .unwrap();
    // 마지막(하나뿐인) 중앙이 있어도 반쯤 합류한 노드는 뺄 수 있고, 중앙 수가 그대로라 경고가 없다.
    let (status, headers, task) = admin
        .send_headers(Method::DELETE, "/sdn/centrals/node2", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "sdn-central-remove");
    assert!(headers.get("orbivisor-warning").is_none(), "{headers:?}");
    // 떠나기 전에는 클러스터에서 지울 수 없다.
    let mut node2 = env.cluster.get::<NodeSpec>(n2).unwrap().unwrap();
    node2.status.online = false;
    env.cluster
        .update_status::<NodeSpec>(n2, &node2.status)
        .await
        .unwrap();
    // 꺼진 채 지우기는 막지 않는다(리뷰 2 N-2 — 지운 뒤 기록하고, 다른 중앙 Task가 돌면 나중에 빼라고 알린다).
    let (status, body) = admin.delete("/nodes/node2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["warnings"]
            .to_string()
            .contains("sdn central remove node2"),
        "{body}"
    );
}

/// 리뷰 2 N-4: 서브넷을 더하면 주소가 하나도 없는 NIC(켜진 VM 포함)가 새 서브넷의 주소를 받는다. 모두 줄 수 없으면 409
/// `ip_exhausted`(수와 함께)이고 아무것도 바뀌지 않는다. 주소가 있는 NIC는 그대로다.
#[tokio::test]
async fn adding_subnets_fills_interfaces_without_addresses() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, &[env.node]).await;
    let (status, net) = admin
        .post("/networks", json!({ "name": "l2", "spec": { "sdn": {} } }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{net}");
    for name in ["a", "b"] {
        let mut req = vm_request(name);
        req["spec"]["nics"] = json!([{ "network": "l2", "model": "virtio" }]);
        let (status, vm) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{vm}");
        assert!(vm["spec"]["nics"][0].get("ips").is_none(), "{vm}");
    }
    // 풀이 주소 하나뿐: 둘에게 줄 수 없다.
    let rev = net["revision"].as_u64().unwrap();
    let (status, body) = admin
        .patch(
            "/networks/l2",
            json!({ "subnets": [{ "cidr": "10.20.0.0/29",
                                  "pools": [{ "start": "10.20.0.2", "end": "10.20.0.2" }] }] }),
            Some(rev),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ip_exhausted", "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("2 interface(s)"), "{message}");
    assert!(message.contains("10.20.0.0/29 (1 free)"), "{message}");
    let (_, now) = admin.get("/networks/l2").await;
    assert_eq!(now["revision"], net["revision"], "{now}");

    let (status, body) = admin
        .patch(
            "/networks/l2",
            json!({ "subnets": [{ "cidr": "10.20.0.0/29" }, { "cidr": "fd20::/64", "dhcp": false }] }),
            Some(rev),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, a) = admin.get("/vms/a").await;
    let (_, b) = admin.get("/vms/b").await;
    assert_eq!(a["spec"]["nics"][0]["ips"], json!(["10.20.0.2", "fd20::2"]));
    assert_eq!(b["spec"]["nics"][0]["ips"], json!(["10.20.0.3", "fd20::3"]));

    // 주소가 있는 NIC는 새 체계를 저절로 받지 않는다(`--fill`). 주소가 없는 NIC만 받는다.
    let mut req = vm_request("c");
    req["spec"]["nics"] = json!([{ "network": "l2", "model": "virtio" }]);
    let (status, c) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{c}");
    assert_eq!(c["spec"]["nics"][0]["ips"], json!(["10.20.0.4", "fd20::4"]));
}

/// 리뷰 2 N-4: 켜진 VM에서도 주소가 없던 NIC에는 주소를 줄 수 있다(있던 주소 바꾸기는 여전히 409 `vm_running`).
#[tokio::test]
async fn running_vms_may_give_an_address_to_an_interface_without_one() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, &[env.node]).await;
    let (status, net) = admin
        .post("/networks", json!({ "name": "l2", "spec": { "sdn": {} } }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{net}");
    let mut req = vm_request("a");
    req["spec"]["nics"] = json!([{ "network": "l2", "model": "virtio" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    // 서브넷만 저장소에 바로 더한다(PATCH는 저절로 채운다).
    let mut network = env
        .cluster
        .get_by_name::<opl_types::network::NetworkSpec>("l2")
        .unwrap()
        .unwrap();
    network.spec.subnets = vec![serde_json::from_value(json!({ "cidr": "10.30.0.0/24" })).unwrap()];
    env.cluster
        .update_spec(network.id, None, network.labels.clone(), &network.spec)
        .await
        .unwrap();
    let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    me.status.sdn = Some(opl_types::sdn::NodeSdn {
        chassis: opl_types::sdn::ChassisState::Ready,
        ..Default::default()
    });
    env.cluster
        .update_status::<NodeSpec>(env.node, &me.status)
        .await
        .unwrap();
    let started = admin.act("a", "start").await;
    assert_eq!(started["status"]["state"], "succeeded", "{started}");
    let (_, vm) = admin.get("/vms/a").await;
    let rev = vm["revision"].as_u64().unwrap();
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["ips"] = json!(["10.30.0.7"]);
    let (status, body) = admin
        .put("/vms/a", json!({ "spec": spec }), Some(rev))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rev = body["revision"].as_u64().unwrap();
    spec["nics"][0]["ips"] = json!(["10.30.0.8"]);
    let (status, body) = admin
        .put("/vms/a", json!({ "spec": spec }), Some(rev))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running", "{body}");
}

/// 리뷰 2 N-4: 다 쓰인 서브넷의 템플릿 되돌리기는 주소 없이 끝나고 Task 경고로 알린다.
#[tokio::test]
async fn untemplating_onto_a_full_subnet_warns() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, &[env.node]).await;
    let (status, b) = admin
        .post(
            "/networks",
            json!({ "name": "tiny", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.40.0.0/29",
                "pools": [{ "start": "10.40.0.2", "end": "10.40.0.2" }] }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let mut req = vm_request("tmpl");
    req["spec"]["nics"] = json!([{ "network": "tiny", "model": "virtio" }]);
    let (status, b) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let (status, b) = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{b}");
    // 템플릿이 놓은 하나뿐인 주소를 다른 VM이 잡는다.
    let mut req = vm_request("other");
    req["spec"]["nics"] = json!([{ "network": "tiny", "model": "virtio" }]);
    let (status, b) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let (status, task) = admin.delete("/vms/tmpl/template").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    let warning = done["status"]["error"].as_str().unwrap_or_default();
    assert!(warning.contains("no free addresses"), "{done}");
    assert!(warning.contains("--fill"), "{done}");
    let (_, vm) = admin.get("/vms/tmpl").await;
    assert!(vm["spec"]["nics"][0].get("ips").is_none(), "{vm}");
}

/// 리뷰 2 m-1: 쓰는 서브넷을 빼려 할 때 읽을 수 없는 VM은 이름 없이 수로만 댄다.
#[tokio::test]
async fn subnet_in_use_hides_other_folders() {
    let env = Env::start().await;
    let (admin, alice, _, _) = shared_world(&env).await;
    // alice가 네트워크 폴더의 admin(network.config)이다.
    let (_, users) = admin.get("/users").await;
    let list = if users.is_array() {
        users.clone()
    } else {
        users["items"].clone()
    };
    let alice_id = list
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["name"] == "alice")
        .map(|u| u["id"].clone())
        .unwrap();
    let (_, net) = admin.get("/networks/shared").await;
    let (status, body) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": alice_id },
                "role": { "type": "builtin", "role": "admin" },
                "target": { "type": "folder", "id": net["spec"]["folder"] },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = alice
        .patch("/networks/shared", json!({ "subnets": [] }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "subnet_in_use", "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("a1"), "{message}");
    assert!(!message.contains("b1"), "{message}");
    assert!(message.contains("1 VM(s) you cannot see"), "{message}");
    let (_, body) = admin
        .patch("/networks/shared", json!({ "subnets": [] }), None)
        .await;
    assert!(body["message"].as_str().unwrap().contains("b1"), "{body}");
}

/// 리뷰 2 N-1·N-2·m-4: 꺼진 중앙은 다른 중앙의 `sdn-central-kick`으로 빼고, 꺼진 반쯤 합류한 노드는 지울 수 있다(지운
/// 뒤 기록하고 내쫓는다). 목록 밖 노드를 뺄 때는 짝수 경고가 없다.
#[tokio::test]
async fn dead_centrals_are_kicked_out_by_a_live_one() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let n2 = other_node(&env, "node2", false).await;
    let n3 = other_node(&env, "node3", true).await;
    ready_sdn(&env, &[env.node, n2, n3]).await;
    // 꺼진 목록의 중앙: 지우기는 409, 빼기는 살아 있는 중앙(이 노드)이 내쫓는 Task
    let (status, body) = admin.delete("/nodes/node2").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_central_on_node", "{body}");
    let (status, headers, task) = admin
        .send_headers(Method::DELETE, "/sdn/centrals/node2", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "sdn-central-kick", "{task}");
    assert_eq!(task["spec"]["op"]["node"], n2.to_string());
    assert_eq!(task["spec"]["op"]["address"], "127.0.0.1");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    // 셋에서 하나를 빼면 짝수가 된다.
    assert!(headers.get("orbivisor-warning").is_some(), "{headers:?}");
    finish(&env, &task).await;

    // 꺼진 반쯤 합류한 노드(목록 밖, DB를 알림): 지우기는 막지 않고, 지운 뒤 기록하고 내쫓는 Task를 만든다.
    ready_sdn(&env, &[env.node, n3]).await;
    let n4 = other_node(&env, "node4", false).await;
    let mut node4 = env.cluster.get::<NodeSpec>(n4).unwrap().unwrap();
    node4.status.sdn = Some(opl_types::sdn::NodeSdn {
        central: Some(opl_types::sdn::CentralHealth {
            nb: opl_types::sdn::RaftRole::Follower,
            sb: opl_types::sdn::RaftRole::Follower,
            northd_active: false,
        }),
        ..Default::default()
    });
    env.cluster
        .update_status::<NodeSpec>(n4, &node4.status)
        .await
        .unwrap();
    let (_, view) = admin.get("/sdn").await;
    assert_eq!(
        view["strays"],
        json!([{ "node": n4, "name": "node4" }]),
        "{view}"
    );
    let (status, body) = admin.delete("/nodes/node4").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let warning = body["warnings"].to_string();
    assert!(warning.contains("kicks it out"), "{body}");
    let sdn = env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(sdn.status.departed.len(), 1);
    assert_eq!(sdn.status.departed[0].address.to_string(), "10.0.0.9");
    let task_id = sdn.status.pending.as_ref().unwrap().task;
    let task = env
        .cluster
        .get::<opl_types::task::TaskSpec>(task_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        task.spec.op,
        opl_types::task::TaskOp::SdnCentralKick {
            node: n4,
            address: "10.0.0.9".parse().unwrap()
        }
    );
    let (_, view) = admin.get("/sdn").await;
    assert_eq!(
        view["strays"],
        json!([{ "node": n4, "name": "node4", "gone": true }]),
        "{view}"
    );
    finish(&env, &json!({ "id": task_id })).await;
    // 지운 노드는 그 이름으로 다시 뺄 수 있다(목록 밖이라 짝수 경고 없음).
    let (status, headers, task) = admin
        .send_headers(Method::DELETE, "/sdn/centrals/node4", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "sdn-central-kick", "{task}");
    assert!(headers.get("orbivisor-warning").is_none(), "{headers:?}");
    let _ = n3;
}

/// 계획 67 §10.4: 새 클러스터의 `sdn init`은 잠금을 켠 채 세운다(옛 노드가 있으면 끈 채). `POST /sdn/lockdown`은 admin,
/// 기능 `sdn-services`, 준비된 SDN이 필요하고 중앙 Task와 같은 자리를 잡는다(요청 받은 노드에서 돈다).
#[tokio::test]
async fn lockdown_defaults_on_for_new_clusters_and_takes_the_slot() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    give_underlay(&env).await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post("/sdn/lockdown", json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_not_ready", "{body}");
    let (status, task) = admin.post("/sdn", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let sdn = env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert!(sdn.spec.lockdown);
    finish(&env, &task).await;

    ready_sdn(&env, &[env.node]).await;
    let viewer = env.user("viewer", "viewer").await;
    let (status, _) = viewer
        .post("/sdn/lockdown", json!({ "enabled": false }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = admin.post("/sdn/lockdown", json!({ "on": true })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, task) = admin
        .post("/sdn/lockdown", json!({ "enabled": false }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "sdn-lockdown", "enabled": false })
    );
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let sdn = env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(
        sdn.status.pending.as_ref().unwrap().task.to_string(),
        task["id"].as_str().unwrap()
    );
    // 도는 동안 다른 잠금·중앙 Task는 409 `sdn_busy`
    let (status, body) = admin
        .post("/sdn/lockdown", json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "sdn_busy", "{body}");
    let node2 = other_node(&env, "node2", true).await;
    let (status, body) = admin
        .post("/sdn/centrals", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // node2는 `sdn-services`를 모른다: 잠금 요청은 409 `cluster_upgrading`
    finish(&env, &task).await;
    let (status, body) = admin
        .post("/sdn/lockdown", json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    let _ = node2;

    // 옛 노드가 있는 클러스터의 첫 세우기는 잠금을 끈 채다.
    let mut env = Env::start().await;
    env.stop_runner().await;
    give_underlay(&env).await;
    other_node(&env, "old", true).await;
    let admin = env.admin().await;
    let (status, task) = admin.post("/sdn", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let sdn = env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert!(!sdn.spec.lockdown);
}
