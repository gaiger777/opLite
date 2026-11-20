//! 계획 66 SDN 라우팅 API(스테이지 5): 공급자 네트워크·노드 `sdn`·라우터·유동 IP, 외부 IPAM(유동 IP 풀)과 EUI-64, 네트워크
//! 폴더의 권한·할당량, 기능 게이트, 보안 그룹의 SDN NIC(스테이지 4)와 그 보기의 NIC 수.

mod common;

use common::Env;
use opl_types::node::{NodeSpec, SDN_ROUTING};
use opl_types::resource::Resource;
use opl_types::sdn::{SDN_NAME, SdnCentral, SdnPhase, SdnSpec, SdnStatus};
use opl_types::vm::{VmPhase, VmSpec};
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

fn upgrading(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

fn code(status: StatusCode, body: &Value, want_status: StatusCode, want: &str) {
    assert_eq!(status, want_status, "{body}");
    assert_eq!(body["code"], want, "{body}");
}

/// 이 노드가 physnet `ext`를 준비했다고 적는다(SDN 에이전트 대신).
async fn physnet_ready(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.sdn = Some(
        serde_json::from_value(json!({
            "chassis": "ready",
            "physnets": [{ "name": "ext", "bridge": "vmbr1", "ready": true }],
            "gateway": true
        }))
        .unwrap(),
    );
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 노드 `sdn`을 쓴다(`PUT /nodes/{key}`, 다른 칸은 그대로).
async fn put_node_sdn(
    env: &Env,
    admin: &common::Client,
    sdn: Option<Value>,
) -> (StatusCode, Value) {
    let node = format!("/nodes/{}", env.node);
    let (_, n) = admin.get(&node).await;
    let mut spec = n["spec"].clone();
    match sdn {
        Some(s) => spec["sdn"] = s,
        None => {
            spec.as_object_mut().unwrap().remove("sdn");
        }
    }
    admin.put(&node, json!({ "spec": spec }), None).await
}

fn ext_network() -> Value {
    json!({
        "name": "ext",
        "spec": { "sdn": { "provider": { "physnet": "ext" } }, "subnets": [{
            "cidr": "203.0.113.0/24", "gateway": "203.0.113.1",
            "floating_pools": [{ "start": "203.0.113.100", "end": "203.0.113.102" }]
        }] }
    })
}

/// 이름의 리소스 id
async fn id_of(c: &common::Client, path: &str) -> String {
    let (status, body) = c.get(path).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_string()
}

/// 계획 66 스테이지 5: 라우팅 API는 모든 노드가 `sdn-routing`을 알고 SDN이 세워졌어야 한다. 옛 노드가 남았으면 검증보다
/// 먼저 409 `cluster_upgrading`, SDN이 없으면 409 `sdn_not_ready`. 새 칸을 쓰지 않는 요청과 저장 모양은 그대로다.
#[tokio::test]
async fn routing_needs_every_node_and_a_ready_sdn() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // SDN이 없다.
    let (status, body) = admin
        .post("/routers", json!({ "name": "r1", "spec": {} }))
        .await;
    code(status, &body, StatusCode::CONFLICT, "sdn_not_ready");
    let (status, body) = put_node_sdn(
        &env,
        &admin,
        Some(json!({ "physnets": { "ext": "vmbr1" } })),
    )
    .await;
    code(status, &body, StatusCode::CONFLICT, "sdn_not_ready");
    // 빈 목록
    let (status, body) = admin.get("/routers").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"], json!([]), "{body}");
    ready_sdn(&env).await;
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
    let (status, body) = put_node_sdn(&env, &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("sdn").is_none(), "{body}");

    drop_routing(&env).await;
    let ipv6_mode = json!({
        "name": "v6",
        "spec": { "sdn": {}, "subnets": [{ "cidr": "2001:db8::/64", "ipv6_mode": "slaac" }] }
    });
    for req in [ext_network(), ipv6_mode] {
        let (status, body) = admin.post("/networks", req).await;
        upgrading(status, &body);
    }
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": "tenant", "kind": "network", "quota": { "routers": 2 } }),
        )
        .await;
    upgrading(status, &body);
    let (status, body) = put_node_sdn(&env, &admin, Some(json!({ "gateway": 100 }))).await;
    upgrading(status, &body);
    let (status, body) = admin
        .post("/routers", json!({ "name": "r1", "spec": {} }))
        .await;
    upgrading(status, &body);
    let (status, body) = admin
        .post("/floating-ips", json!({ "spec": { "network": "ext" } }))
        .await;
    upgrading(status, &body);
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
    upgrading(status, &body);
}

/// 노드 `sdn`(계획 66 §7): 칸이 없으면 그대로, `{}`면 지움, 검증 400, 빠지는 physnet을 쓰는 VM이 이 노드에서 돌면 409
/// `physnet_in_use`.
#[tokio::test]
async fn node_sdn_settings() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env).await;
    let set = json!({ "physnets": { "ext": "vmbr1" }, "gateway": 100 });
    let (status, body) = put_node_sdn(&env, &admin, Some(set.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["sdn"], set, "{body}");
    // 칸이 없으면 그대로(이 칸을 모르는 클라이언트의 PUT)
    let (_, body) = put_node_sdn(&env, &admin, None).await;
    assert_eq!(body["spec"]["sdn"], set, "{body}");
    for bad in [
        json!({ "physnets": { "Ext": "vmbr1" } }),
        json!({ "physnets": { "ext": "br-int" } }),
        json!({ "gateway": 0 }),
    ] {
        let (status, body) = put_node_sdn(&env, &admin, Some(bad)).await;
        code(status, &body, StatusCode::BAD_REQUEST, "invalid_node_sdn");
    }
    // 이 노드에서 도는 VM이 physnet ext의 공급자 네트워크를 쓴다.
    physnet_ready(&env).await;
    let (status, body) = admin.post("/networks", ext_network()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut req = common::vm_request("web");
    req["spec"]["nics"] = json!([{ "network": "ext", "model": "virtio" }]);
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // NIC는 유동 IP 풀(.100-.102)을 건너뛰고 풀 밖의 가장 낮은 빈 주소를 받는다.
    assert_eq!(
        body["spec"]["nics"][0]["ips"],
        json!(["203.0.113.2"]),
        "{body}"
    );
    let id: uuid::Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let mut vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    vm.status.node = Some(env.node);
    vm.status.phase = VmPhase::Running;
    env.cluster
        .update_status::<VmSpec>(id, &vm.status)
        .await
        .unwrap();
    let (status, body) = put_node_sdn(&env, &admin, Some(json!({ "gateway": 100 }))).await;
    code(status, &body, StatusCode::CONFLICT, "physnet_in_use");
    assert_eq!(body["details"]["vms"], json!(["web"]), "{body}");
    // 게이트웨이만 바꾸는 것은 된다. `{}`는 지운다(VM을 멈춘 뒤).
    let (status, body) = put_node_sdn(
        &env,
        &admin,
        Some(json!({ "physnets": { "ext": "vmbr1" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    vm.status.phase = VmPhase::Stopped;
    env.cluster
        .update_status::<VmSpec>(id, &vm.status)
        .await
        .unwrap();
    let (status, body) = put_node_sdn(&env, &admin, Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("sdn").is_none(), "{body}");
}

/// 공급자 네트워크·라우터·유동 IP(계획 66 §8·§9): 외부 IPAM(유동 IP 풀 먼저, 세 종류가 한 주소 공간), 검사·409들, 보기,
/// VM 지우기의 유동 IP 떼기, 네트워크 지우기 막기.
#[tokio::test]
async fn provider_networks_routers_and_floating_ips() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env).await;
    // physnet이 준비된 노드가 없으면 만들고 경고한다.
    let (status, headers, body) = admin
        .send_headers(reqwest::Method::POST, "/networks", ext_network())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let warning = headers["orbivisor-warning"].to_str().unwrap().to_string();
    assert!(warning.contains("physnet ext"), "{warning}");
    // 공급자 서브넷의 DHCP는 기본 꺼짐(저장은 그대로), MTU 기본 1500(저장하지 않음)
    assert_eq!(body["spec"]["subnets"][0].get("dhcp"), None, "{body}");
    // 같은 physnet·VLAN은 하나
    let mut dup = ext_network();
    dup["name"] = json!("ext2");
    let (status, body) = admin.post("/networks", dup).await;
    code(status, &body, StatusCode::CONFLICT, "physnet_conflict");
    for (name, cidr) in [
        ("t1", "10.10.0.0/24"),
        ("t2", "10.20.0.0/24"),
        ("t3", "10.10.0.0/25"),
    ] {
        let (status, body) = admin
            .post(
                "/networks",
                json!({ "name": name, "spec": { "sdn": {}, "subnets": [{ "cidr": cidr }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let mut req = common::vm_request("web");
    req["spec"]["nics"] =
        json!([{ "network": "t1", "model": "virtio" }, { "network": "t2", "model": "virtio" }]);
    let (status, web) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{web}");
    let web_id = web["id"].as_str().unwrap().to_string();
    let mac1 = web["spec"]["nics"][0]["mac"].clone();
    let mac2 = web["spec"]["nics"][1]["mac"].clone();

    // 라우터: 외부 주소는 유동 IP 풀에서, 게이트웨이 후보가 없으면 경고
    let (status, headers, r1) = admin
        .send_headers(
            reqwest::Method::POST,
            "/routers",
            json!({
                "name": "r1",
                "spec": { "external": { "network": "ext" }, "interfaces": [{ "network": "t1" }] }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{r1}");
    assert_eq!(
        r1["spec"]["external"]["ips"],
        json!(["203.0.113.100"]),
        "{r1}"
    );
    assert!(
        headers["orbivisor-warning"]
            .to_str()
            .unwrap()
            .contains("no gateway node"),
        "{headers:?}"
    );
    let (_, view) = admin.get("/routers/r1").await;
    assert_eq!(
        view["gateway_error"], "no gateway node has physnet ext ready",
        "{view}"
    );
    // 이 노드는 OVN을 읽지 않는다(시험 환경): 보기는 까닭과 함께
    assert!(view["live_error"].is_string(), "{view}");
    let (_, view) = admin.get("/routers/r1?live=false").await;
    assert!(view.get("live_error").is_none(), "{view}");
    // 게이트웨이 노드가 생기면 순위가 보인다.
    let (status, body) = put_node_sdn(
        &env,
        &admin,
        Some(json!({ "physnets": { "ext": "vmbr1" }, "gateway": 150 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    physnet_ready(&env).await;
    let (_, view) = admin.get("/routers/r1?live=false").await;
    assert_eq!(view["gateways"][0]["node_priority"], 150, "{view}");
    assert_eq!(view["gateways"][0]["priority"], 32767, "{view}");
    assert!(view.get("gateway_error").is_none(), "{view}");
    // 다른 라우터는 이미 인터페이스인 네트워크를 갖지 못한다. 한 라우터의 서브넷은 겹치지 않는다.
    let (status, body) = admin
        .post(
            "/routers",
            json!({ "name": "r2", "spec": { "interfaces": [{ "network": "t1" }] } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "network_in_use");
    let (status, body) = admin
        .patch(
            "/routers/r1",
            json!({ "interfaces": [{ "network": "t1" }, { "network": "t3" }] }),
            None,
        )
        .await;
    code(status, &body, StatusCode::BAD_REQUEST, "invalid_router");
    for bad in [
        json!({ "external": { "network": "t2" } }),
        json!({ "interfaces": [{ "network": "ext" }] }),
        json!({ "routes": [{ "destination": "10.99.0.0/16", "nexthop": "10.50.0.1" }] }),
    ] {
        let (status, body) = admin.patch("/routers/r1", bad.clone(), None).await;
        code(status, &body, StatusCode::BAD_REQUEST, "invalid_router");
    }
    let (status, body) = admin
        .patch(
            "/routers/r1",
            json!({ "routes": [{ "destination": "10.99.0.0/16", "nexthop": "10.10.0.254" }] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 외부 주소는 그대로(같은 네트워크에 주소를 주지 않음)
    assert_eq!(
        body["spec"]["external"]["ips"],
        json!(["203.0.113.100"]),
        "{body}"
    );

    // 유동 IP: 다음 유동 IP 풀 주소, 이름은 주소에서
    let (status, fip) = admin
        .post(
            "/floating-ips",
            json!({ "spec": { "network": "ext", "target": { "vm": web_id, "mac": mac1 } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{fip}");
    assert_eq!(fip["name"], "203-0-113-101", "{fip}");
    assert_eq!(fip["spec"]["address"], "203.0.113.101", "{fip}");
    let (_, view) = admin.get("/floating-ips/203-0-113-101").await;
    assert_eq!(view["state"]["kind"], "centralized", "{view}");
    assert_eq!(view["vm_name"], "web", "{view}");
    // 같은 NIC에 둘은 안 되고, 라우터가 없는 네트워크의 NIC는 라우팅되지 않는다.
    let (status, body) = admin
        .post(
            "/floating-ips",
            json!({ "name": "f2", "spec": { "network": "ext", "target": { "vm": web_id, "mac": mac1 } } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "fip_in_use");
    let (status, body) = admin
        .post(
            "/floating-ips",
            json!({ "name": "f2", "spec": { "network": "ext", "target": { "vm": web_id, "mac": mac2 } } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "fip_not_routable");
    // 공급자가 아닌 네트워크·IPv6 주소·쓰인 주소
    for (spec, want) in [
        (json!({ "network": "t1" }), StatusCode::BAD_REQUEST),
        (
            json!({ "network": "ext", "address": "2001:db8::5" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "network": "ext", "address": "203.0.113.100" }),
            StatusCode::CONFLICT,
        ),
    ] {
        let (status, body) = admin
            .post("/floating-ips", json!({ "name": "f3", "spec": spec }))
            .await;
        assert_eq!(status, want, "{body}");
    }
    // 유동 IP 풀이 다 쓰였다(.102 하나 남음 → 하나 더 받고 그다음은 409).
    let (status, body) = admin
        .post(
            "/floating-ips",
            json!({ "name": "f4", "spec": { "network": "ext" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            "/floating-ips",
            json!({ "name": "f5", "spec": { "network": "ext" } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "ip_exhausted");
    // VM NIC는 라우터·유동 IP의 주소를 받을 수 없다(한 주소 공간).
    let mut req = common::vm_request("direct");
    req["spec"]["nics"] =
        json!([{ "network": "ext", "model": "virtio", "ips": ["203.0.113.101"] }]);
    let (status, body) = admin.post("/vms", req).await;
    code(status, &body, StatusCode::CONFLICT, "ip_in_use");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("router or floating IP"),
        "{body}"
    );

    // 주소 목록에 라우터·유동 IP 행과 유동 IP 풀 사용
    let (status, ips) = admin.get("/networks/ext/ips?live=false").await;
    assert_eq!(status, StatusCode::OK, "{ips}");
    assert_eq!(
        ips["subnets"][0]["floating"],
        json!({ "size": 3, "used": 3, "free": 0 }),
        "{ips}"
    );
    let kinds: Vec<(&str, &str)> = ips["allocations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["kind"].as_str().unwrap_or("nic"),
                a["vm_name"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(kinds.contains(&("router", "r1")), "{ips}");
    assert!(kinds.contains(&("floating-ip", "203-0-113-101")), "{ips}");

    // 유동 IP가 이 라우터로 라우팅되는 인터페이스·외부는 뺄 수 없다.
    let (status, body) = admin
        .patch("/routers/r1", json!({ "interfaces": [] }), None)
        .await;
    code(status, &body, StatusCode::CONFLICT, "fip_in_use");
    let (status, body) = admin
        .patch("/routers/r1", json!({ "external": null }), None)
        .await;
    code(status, &body, StatusCode::CONFLICT, "fip_in_use");
    // 쓰이는 네트워크는 지울 수 없다.
    let (status, body) = admin.delete("/networks/ext").await;
    code(status, &body, StatusCode::CONFLICT, "network_in_use");
    // 라우터 외부 주소의 서브넷은 뺄 수 없다.
    let (status, body) = admin
        .patch("/networks/ext", json!({ "subnets": [] }), None)
        .await;
    code(status, &body, StatusCode::CONFLICT, "subnet_in_use");
    // 인터페이스가 남은 라우터는 지울 수 없다.
    let (status, body) = admin.delete("/routers/r1").await;
    code(status, &body, StatusCode::CONFLICT, "router_in_use");

    // VM을 지우면 유동 IP가 떨어진다.
    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, view) = admin.get("/floating-ips/203-0-113-101").await;
    assert!(view["spec"].get("target").is_none(), "{view}");
    assert_eq!(view["state"]["kind"], "unattached", "{view}");
    let (status, body) = admin
        .patch(
            "/routers/r1",
            json!({ "interfaces": [], "external": null, "routes": [] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin.delete("/routers/r1").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    for f in ["203-0-113-101", "f4"] {
        let (status, body) = admin.delete(&format!("/floating-ips/{f}")).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }
    let (status, body) = admin.delete("/networks/ext").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// 네트워크 폴더의 권한과 할당량(계획 66 §12): 라우터는 폴더의 `network.config`와 잇는 네트워크의 `network.assign`, 공급자
/// 네트워크는 루트 `cluster.admin`, 할당량 `routers`·`floating_ips`.
#[tokio::test]
async fn routers_follow_network_folders() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env).await;
    physnet_ready(&env).await;
    let (status, body) = admin.post("/networks", ext_network()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, folder) = admin
        .post(
            "/folders",
            json!({ "name": "tenant", "kind": "network", "quota": { "routers": 1, "floating_ips": 1 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{folder}");
    let folder = folder["id"].as_str().unwrap().to_string();
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "t1", "spec": { "folder": folder, "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "netops", "privileges": ["network.config", "network.assign"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (status, user) = admin
        .post(
            "/users",
            json!({ "name": "alice", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    let grant = |target: Value| {
        json!({
            "principal": { "type": "user", "id": user["id"] },
            "role": { "type": "custom", "id": role["id"] },
            "target": target,
        })
    };
    let (status, body) = admin
        .post("/grants", grant(json!({ "type": "folder", "id": folder })))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let alice = env.login("alice", "password-1").await;
    // 공급자 네트워크는 루트 cluster.admin
    let mut other = ext_network();
    other["name"] = json!("ext-b");
    other["spec"]["sdn"]["provider"]["vlan"] = json!(20);
    other["spec"]["folder"] = json!(folder);
    let (status, body) = alice.post("/networks", other).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 외부 네트워크(루트)의 network.assign이 없다.
    let router = |name: &str, external: bool| {
        let mut spec = json!({ "folder": folder, "interfaces": [{ "network": "t1" }] });
        if external {
            spec["external"] = json!({ "network": "ext" });
        }
        json!({ "name": name, "spec": spec })
    };
    let (status, body) = alice.post("/routers", router("r1", true)).await;
    assert!(
        matches!(status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{status} {body}"
    );
    // 폴더 밖(루트)에는 만들 수 없다.
    let (status, body) = alice
        .post(
            "/routers",
            json!({ "name": "r0", "spec": { "interfaces": [{ "network": "t1" }] } }),
        )
        .await;
    code(status, &body, StatusCode::BAD_REQUEST, "folder_required");
    let (status, body) = alice.post("/routers", router("r1", false)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 외부 네트워크에 network.assign을 주면 외부를 정할 수 있다(객체 grant).
    let ext_id = id_of(&admin, "/networks/ext").await;
    let (status, body) = admin
        .post(
            "/grants",
            grant(json!({ "type": "object", "kind": "network", "id": ext_id })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = alice
        .patch(
            "/routers/r1",
            json!({ "external": { "network": "ext" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 할당량 routers 1
    let (status, body) = admin
        .post(
            "/routers",
            json!({ "name": "r2", "spec": { "folder": folder } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "quota_exceeded");
    let (status, body) = alice
        .post(
            "/floating-ips",
            json!({ "name": "f1", "spec": { "network": "ext", "folder": folder } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = alice
        .post(
            "/floating-ips",
            json!({ "name": "f2", "spec": { "network": "ext", "folder": folder } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "quota_exceeded");
    // 사용량
    let (_, usage) = admin.get(&format!("/folders/{folder}/usage")).await;
    assert_eq!(usage["usage"]["routers"], 1, "{usage}");
    assert_eq!(usage["usage"]["floating_ips"], 1, "{usage}");
    // 목록은 볼 수 있는 것만: 루트에 만든 라우터는 alice에게 보이지 않는다.
    let (status, body) = admin
        .post("/routers", json!({ "name": "root-r", "spec": {} }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, list) = alice.get("/routers?live=false").await;
    let names: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["r1"], "{list}");
    let (status, _) = alice.get("/routers/root-r").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 라우터 객체 grant: 루트 라우터 하나에만
    let rr = id_of(&admin, "/routers/root-r?live=false").await;
    let (status, body) = admin
        .post(
            "/grants",
            grant(json!({ "type": "object", "kind": "router", "id": rr })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = alice
        .patch("/routers/root-r", json!({ "description": "mine" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 폴더 옮기기(할당량 1이 찼다)
    let (status, body) = admin
        .post(
            &format!("/folders/{folder}/members"),
            json!({ "items": [{ "kind": "router", "key": "root-r" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["ok"], false, "{body}");
    assert_eq!(
        body["items"][0]["error"]["code"], "quota_exceeded",
        "{body}"
    );
    let (status, body) = admin
        .post(
            "/folders/root/members",
            json!({ "items": [{ "kind": "router", "key": "r1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["ok"], true, "{body}");
}

/// EUI-64 IPAM(계획 66 §10): SLAAC 서브넷의 NIC 주소는 MAC에서, 다른 주소는 400, 모드를 바꾸면 쓰는 NIC가 있을 때 409.
#[tokio::test]
async fn slaac_subnets_give_eui64_addresses() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env).await;
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "t6", "spec": { "sdn": {}, "subnets": [
                { "cidr": "10.60.0.0/24" },
                { "cidr": "fd00:60::/64", "ipv6_mode": "slaac" }
            ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut req = common::vm_request("v6");
    req["spec"]["nics"] =
        json!([{ "network": "t6", "model": "virtio", "mac": "52:54:00:12:34:56" }]);
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["spec"]["nics"][0]["ips"],
        json!(["10.60.0.2", "fd00:60::5054:ff:fe12:3456"]),
        "{body}"
    );
    req["name"] = json!("v6b");
    req["spec"]["nics"] = json!([{ "network": "t6", "model": "virtio", "ips": ["fd00:60::5"] }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("EUI-64"),
        "{body}"
    );
    let (status, body) = admin
        .patch(
            "/networks/t6",
            json!({ "subnets": [
                { "cidr": "10.60.0.0/24" },
                { "cidr": "fd00:60::/64", "ipv6_mode": "dhcpv6-stateful" }
            ] }),
            None,
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "subnet_in_use");
    // 새 서브넷을 SLAAC로 더하면 주소 없는 NIC가 아니어도(IPv4가 있음) 그대로 — 채우기는 주소 없는 NIC만
}

/// 보안 그룹 보기의 NIC 수(계획 66 §11.4): 단순 NIC와 SDN NIC
#[tokio::test]
async fn security_groups_count_simple_and_sdn_nics() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let net: opl_types::network::Network = Resource::new(
        "tenant",
        serde_json::from_value(json!({ "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] }))
            .unwrap(),
    );
    env.cluster.create(&net).await.unwrap();
    let (status, body) = admin
        .post(
            "/security-groups",
            json!({ "name": "ssh", "spec": { "rules": [] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut req = common::vm_request("a");
    req["spec"]["nics"] = json!([
        { "network": "tenant", "model": "virtio", "security_groups": ["ssh"] }
    ]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, group) = admin.get("/security-groups/ssh").await;
    assert_eq!(
        group["status"]["nics"],
        json!({ "simple": 0, "sdn": 1 }),
        "{group}"
    );
    let (_, list) = admin.get("/security-groups").await;
    assert_eq!(list["items"][0]["status"]["nics"]["sdn"], 1, "{list}");
    // 저장소에는 쓰지 않는다.
    let stored = env
        .cluster
        .get_by_name::<opl_types::security_group::SecurityGroupSpec>("ssh")
        .unwrap()
        .unwrap();
    assert_eq!(stored.status.nics, None);
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
