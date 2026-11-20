//! 계획 66 스테이지 1: SDN 라우팅의 새 칸(공급자 네트워크, 서브넷 `floating_pools`·`ipv6_mode`, 노드 `sdn`, 할당량
//! `routers`·`floating_ips`, 라우터·유동 IP 옮기기)은 API 스테이지(5) 전까지 임시로 막힌다: 옛 노드가 남았으면 409
//! `cluster_upgrading`, 아니면 400. 이 칸을 쓰지 않는 요청과 저장 모양은 지금 그대로다.

mod common;

use common::Env;
use opl_types::node::{NodeSpec, SDN_ROUTING};
use opl_types::resource::Resource;
use opl_types::sdn::{SDN_NAME, SdnCentral, SdnPhase, SdnSpec, SdnStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 이 노드가 `sdn-routing`을 모르는 것으로 적는다(옛 노드처럼).
async fn drop_routing(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != SDN_ROUTING);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 세워진 SDN을 저장소에 바로 둔다.
async fn ready_sdn(env: &Env) {
    let mut sdn = Resource::new(SDN_NAME, SdnSpec {});
    sdn.status = SdnStatus {
        phase: SdnPhase::Ready,
        centrals: vec![SdnCentral {
            node: env.node,
            address: "127.0.0.1".parse().unwrap(),
        }],
        init_node: Some(env.node),
        ..SdnStatus::default()
    };
    env.cluster.create(&sdn).await.unwrap();
}

fn not_yet(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("not available yet"),
        "{body}"
    );
}

fn upgrading(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

#[tokio::test]
async fn routing_fields_are_gated_until_the_api_stage() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env).await;
    let provider = json!({
        "name": "ext",
        "spec": { "sdn": { "provider": { "physnet": "ext" } }, "subnets": [{ "cidr": "203.0.113.0/24" }] }
    });
    let ipv6_mode = json!({
        "name": "v6",
        "spec": { "sdn": {}, "subnets": [{ "cidr": "2001:db8::/64", "ipv6_mode": "slaac" }] }
    });
    let net_quota = json!({ "name": "tenant", "kind": "network", "quota": { "routers": 2 } });
    let node = format!("/nodes/{}", env.node);
    let (_, n) = admin.get(&node).await;
    let mut with_sdn = n["spec"].clone();
    with_sdn["sdn"] = json!({ "physnets": { "ext": "vmbr1" }, "gateway": 100 });

    for req in [&provider, &ipv6_mode] {
        let (status, body) = admin.post("/networks", req.clone()).await;
        not_yet(status, &body);
    }
    let (status, body) = admin.post("/folders", net_quota.clone()).await;
    not_yet(status, &body);
    let (status, body) = admin
        .put(&node, json!({ "spec": with_sdn.clone() }), None)
        .await;
    not_yet(status, &body);

    // 새 칸을 쓰지 않는 요청은 지금 그대로이고 저장 모양에 새 칸이 없다.
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "t1", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["spec"],
        json!({ "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] }),
        "{body}"
    );
    let (status, body) = admin
        .patch(
            "/networks/t1",
            json!({ "subnets": [{
                "cidr": "10.10.0.0/24",
                "floating_pools": [{ "start": "10.10.0.100", "end": "10.10.0.120" }]
            }] }),
            None,
        )
        .await;
    not_yet(status, &body);
    let (status, body) = admin.put(&node, json!({ "spec": n["spec"] }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("sdn").is_none(), "{body}");
    let (status, body) = admin
        .post("/folders", json!({ "name": "tenant", "kind": "network" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["spec"].get("quota").is_none(), "{body}");
    let folder = body["id"].as_str().unwrap().to_string();
    // VM 차원은 네트워크 폴더에 걸 수 없다(계획 66 — VM 폴더에만).
    let (status, body) = admin
        .patch(
            &format!("/folders/{folder}"),
            json!({ "quota": { "vms": 1 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 라우터·유동 IP는 아직 옮길 수 없다(항목의 오류).
    let (status, body) = admin
        .post(
            &format!("/folders/{folder}/members"),
            json!({ "items": [{ "kind": "router", "key": "r1" }, { "kind": "network", "key": "t1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["ok"], false, "{body}");
    assert!(
        body["items"][0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not available yet"),
        "{body}"
    );
    assert_eq!(body["items"][1]["ok"], true, "{body}");

    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_routing(&env).await;
    for req in [&provider, &ipv6_mode] {
        let (status, body) = admin.post("/networks", req.clone()).await;
        upgrading(status, &body);
    }
    let (status, body) = admin.post("/folders", net_quota).await;
    upgrading(status, &body);
    let (status, body) = admin.put(&node, json!({ "spec": with_sdn }), None).await;
    upgrading(status, &body);
}

/// 계획 66 스테이지 4: SDN NIC에 보안 그룹을 건다(OVN ACL). 옛 노드가 남았으면 409 `cluster_upgrading`(계획 65는 400
/// `sdn_security_groups`), 노드 방화벽(nftables)은 단순 NIC만 본다(`firewall_unsupported`가 SDN NIC에는 없다).
#[tokio::test]
async fn sdn_nics_take_security_groups() {
    use opl_types::network::Network;
    let env = Env::start().await;
    let admin = env.admin().await;
    let net: Network = Resource::new(
        "tenant",
        serde_json::from_value(json!({ "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] }))
            .unwrap(),
    );
    env.cluster.create(&net).await.unwrap();
    let rule = json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "22" });
    let (status, body) = admin
        .post(
            "/security-groups",
            json!({ "name": "ssh", "spec": { "rules": [rule] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 노드에 방화벽이 없어도 SDN NIC의 그룹은 된다.
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.firewall = false;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let vm = |name: &str, groups: Value| {
        let mut req = common::vm_request(name);
        req["spec"]["nics"] =
            json!([{ "network": "tenant", "model": "virtio", "security_groups": groups }]);
        req["spec"]["node"] = json!(env.node);
        req
    };
    let (status, body) = admin.post("/vms", vm("a", json!(["ssh"]))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["nics"][0]["security_groups"], json!(["ssh"]));
    // 없는 그룹은 단순 NIC처럼 400이다.
    let (status, body) = admin.post("/vms", vm("b", json!(["nope"]))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    drop_routing(&env).await;
    let (status, body) = admin.post("/vms", vm("c", json!(["ssh"]))).await;
    upgrading(status, &body);
    // 그룹이 없는 SDN NIC는 그대로 된다.
    let (status, body) = admin.post("/vms", vm("c", json!([]))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}
