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
    let mut sdn = Resource::new(SDN_NAME, SdnSpec::default());
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

/// VM을 API로 켜거나 끄고, 조정기가 Mock 하이퍼바이저에서 그렇게 만들 때까지 기다린다(켜면 이 노드에서 돈다).
async fn power(env: &Env, admin: &common::Client, name: &str, id: uuid::Uuid, on: bool) {
    let action = if on { "start" } else { "stop" };
    let (status, body) = admin
        .post(&format!("/vms/{name}/actions/{action}"), json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
        let done = if on {
            vm.status.phase == VmPhase::Running && vm.status.node == Some(env.node)
        } else {
            vm.status.phase == VmPhase::Stopped
        };
        if done {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{name} {action}: {:?}",
            vm.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
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
    // 정말 켠다: 상태를 직접 적으면 조정기가 스펙(꺼짐)대로 고쳐 쓴다.
    power(&env, &admin, "web", id, true).await;
    let (status, body) = put_node_sdn(&env, &admin, Some(json!({ "gateway": 100 }))).await;
    code(status, &body, StatusCode::CONFLICT, "physnet_in_use");
    assert_eq!(body["details"]["vms"], json!(["web"]), "{body}");
    // 리뷰 1 #3: 같은 physnet의 브리지를 바꾸는 것도 도는 VM을 다른 L2로 옮기므로 막는다.
    let (status, body) = put_node_sdn(
        &env,
        &admin,
        Some(json!({ "physnets": { "ext": "vmbr2" }, "gateway": 100 })),
    )
    .await;
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
    power(&env, &admin, "web", id, false).await;
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
    // 리뷰 1 웹 M6: 줄바꿈에서 남은 빈칸이 없다.
    assert!(
        warning.contains("until one has (`orbivisorctl node sdn"),
        "{warning}"
    );
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

/// 계획 66 리뷰 1: 라우터 인터페이스는 그 네트워크의 `network.config`(#1 — `network.assign`만으로는 남의 네트워크 기본
/// 경로를 가져가지 못함), 고른 유동 IP·라우터 외부 주소는 유동 IP 풀 안(#2 — 루트 `cluster.admin`만 밖), 루트
/// `cluster.read`가 없으면 게이트웨이 노드 이름을 비움(M-4).
#[tokio::test]
async fn review1_router_interfaces_and_chosen_addresses() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env).await;
    physnet_ready(&env).await;
    let (status, body) = put_node_sdn(
        &env,
        &admin,
        Some(json!({ "physnets": { "ext": "vmbr1" }, "gateway": 100 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin.post("/networks", ext_network()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut folders = Vec::new();
    for name in ["tenant-a", "tenant-b"] {
        let (status, f) = admin
            .post("/folders", json!({ "name": name, "kind": "network" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{f}");
        folders.push(f["id"].as_str().unwrap().to_string());
    }
    for (name, folder, cidr) in [
        ("a-net", &folders[0], "10.10.0.0/24"),
        ("shared", &folders[1], "10.30.0.0/24"),
    ] {
        let (status, body) = admin
            .post(
                "/networks",
                json!({ "name": name, "spec": { "folder": folder, "sdn": {}, "subnets": [{ "cidr": cidr }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let mut roles = Vec::new();
    for (name, privileges) in [
        ("netops", json!(["network.config", "network.assign"])),
        ("assigner", json!(["network.assign"])),
    ] {
        let (status, role) = admin
            .post(
                "/access-roles",
                json!({ "name": name, "privileges": privileges }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{role}");
        roles.push(role["id"].clone());
    }
    let (status, user) = admin
        .post(
            "/users",
            json!({ "name": "alice", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    let shared_id = id_of(&admin, "/networks/shared").await;
    let ext_id = id_of(&admin, "/networks/ext").await;
    for (role, target) in [
        (&roles[0], json!({ "type": "folder", "id": folders[0] })),
        (
            &roles[1],
            json!({ "type": "object", "kind": "network", "id": shared_id }),
        ),
        (
            &roles[1],
            json!({ "type": "object", "kind": "network", "id": ext_id }),
        ),
    ] {
        let (status, body) = admin
            .post(
                "/grants",
                json!({
                    "principal": { "type": "user", "id": user["id"] },
                    "role": { "type": "custom", "id": role },
                    "target": target,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let alice = env.login("alice", "password-1").await;

    // #1: `shared`에는 NIC만 붙일 수 있다(network.assign) — 라우터 인터페이스는 403.
    let (status, body) = alice
        .post(
            "/routers",
            json!({ "name": "r-a", "spec": { "folder": folders[0],
                "interfaces": [{ "network": "a-net" }, { "network": "shared" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = alice
        .post(
            "/routers",
            json!({ "name": "r-a", "spec": { "folder": folders[0], "interfaces": [{ "network": "a-net" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = alice
        .patch(
            "/routers/r-a",
            json!({ "interfaces": [{ "network": "a-net" }, { "network": "shared" }] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // #2: 고른 주소는 유동 IP 풀 안이어야 한다(물리 망의 .53을 가로채지 못함).
    let (status, body) = alice
        .post(
            "/floating-ips",
            json!({ "name": "f-x", "spec": { "network": "ext", "folder": folders[0], "address": "203.0.113.53" } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("only a cluster administrator"),
        "{body}"
    );
    let (status, body) = alice
        .patch(
            "/routers/r-a",
            json!({ "external": { "network": "ext", "ips": ["203.0.113.53"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = alice
        .post(
            "/floating-ips",
            json!({ "name": "f-a", "spec": { "network": "ext", "folder": folders[0], "address": "203.0.113.101" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            "/floating-ips",
            json!({ "name": "f-admin", "spec": { "network": "ext", "address": "203.0.113.53" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = alice
        .patch(
            "/routers/r-a",
            json!({ "external": { "network": "ext" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["external"]["ips"],
        json!(["203.0.113.100"]),
        "{body}"
    );

    // M-4: 게이트웨이 순위는 보이지만 노드 이름은 루트 cluster.read가 있을 때만.
    let (_, view) = alice.get("/routers/r-a?live=false").await;
    assert_eq!(view["gateways"][0]["node"], "", "{view}");
    assert_eq!(view["gateways"][0]["node_priority"], 100, "{view}");
    let (_, view) = admin.get("/routers/r-a?live=false").await;
    assert_ne!(view["gateways"][0]["node"], "", "{view}");
}

/// 사용자 `name`(비밀번호 `password-1`)을 만들고 grant들을 준다. grant id들을 돌려준다.
async fn user_with(admin: &common::Client, name: &str, grants: &[(&Value, Value)]) -> Vec<String> {
    let (status, user) = admin
        .post(
            "/users",
            json!({ "name": name, "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    let mut ids = Vec::new();
    for (role, target) in grants {
        let (status, body) = admin
            .post(
                "/grants",
                json!({
                    "principal": { "type": "user", "id": user["id"] },
                    "role": { "type": "custom", "id": role },
                    "target": target,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        ids.push(body["id"].as_str().unwrap().to_string());
    }
    ids
}

/// 계획 66 리뷰 2: 공급자 NIC 주소도 풀 안에서만(I-1), 경로·외부를 바꾸면 모든 인터페이스 네트워크의 `network.config`와
/// 네트워크 주인의 떼기(I-2), 게이트웨이·분산 유동 IP가 있는 노드의 physnet 브리지 바꾸기(I-3), 남은 이름을 물려받지
/// 않기(m-4), 읽을 수 없는 이름을 숨기기(m-5), 다른 칸만 바꾸는 `PATCH`는 외부를 채우지 않기(m-6), 감사(m-8), 목록의
/// 라우팅된 네트워크(웹 I1)와 게이트웨이의 살아 있음(웹 I2).
#[tokio::test]
async fn review2_provider_nics_router_owners_and_gateway_bridges() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env).await;
    physnet_ready(&env).await;
    let (status, body) = put_node_sdn(
        &env,
        &admin,
        Some(json!({ "physnets": { "ext": "vmbr1" }, "gateway": 100 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut ext = ext_network();
    ext["spec"]["subnets"][0]["pools"] =
        json!([{ "start": "203.0.113.10", "end": "203.0.113.99" }]);
    let (status, body) = admin.post("/networks", ext).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut folders = Vec::new();
    for name in ["tenant-a", "tenant-b"] {
        let (status, f) = admin
            .post("/folders", json!({ "name": name, "kind": "network" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{f}");
        folders.push(f["id"].as_str().unwrap().to_string());
    }
    for (name, folder, cidr) in [
        ("a-net", &folders[0], "10.10.0.0/24"),
        ("shared", &folders[1], "10.30.0.0/24"),
    ] {
        let (status, body) = admin
            .post(
                "/networks",
                json!({ "name": name, "spec": { "folder": folder, "sdn": {}, "subnets": [{ "cidr": cidr }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let mut roles = Vec::new();
    for (name, privileges) in [
        (
            "netops",
            json!(["network.config", "network.assign", "system.read"]),
        ),
        ("assigner", json!(["network.assign"])),
        ("vmuser", json!(["vm.create", "vm.config"])),
    ] {
        let (status, role) = admin
            .post(
                "/access-roles",
                json!({ "name": name, "privileges": privileges }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{role}");
        roles.push(role["id"].clone());
    }
    let shared_id = id_of(&admin, "/networks/shared").await;
    let ext_id = id_of(&admin, "/networks/ext").await;
    let (status, vm_folder) = admin
        .post("/folders", json!({ "name": "vms-a", "kind": "vm" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{vm_folder}");
    let alice_grants = user_with(
        &admin,
        "alice",
        &[
            (&roles[0], json!({ "type": "folder", "id": folders[0] })),
            (
                &roles[0],
                json!({ "type": "object", "kind": "network", "id": shared_id }),
            ),
            (
                &roles[1],
                json!({ "type": "object", "kind": "network", "id": ext_id }),
            ),
            (
                &roles[2],
                json!({ "type": "folder", "id": vm_folder["id"] }),
            ),
        ],
    )
    .await;
    user_with(
        &admin,
        "bob",
        &[(&roles[0], json!({ "type": "folder", "id": folders[1] }))],
    )
    .await;
    let alice = env.login("alice", "password-1").await;
    let bob = env.login("bob", "password-1").await;

    // I-1: 공급자 네트워크 NIC에 고른 주소는 풀 안이고 유동 IP 범위 밖이어야 한다(.53은 물리 망의 DNS).
    for (ip, ok) in [
        ("203.0.113.5", false),
        ("203.0.113.101", false),
        ("203.0.113.20", true),
    ] {
        let mut req = common::vm_request(&format!("p{}", ip.replace('.', "-")));
        req["spec"]["nics"] = json!([{ "network": "ext", "model": "virtio", "ips": [ip] }]);
        req["spec"]["folder"] = vm_folder["id"].clone();
        let (status, body) = alice.post("/vms", req).await;
        if ok {
            assert_eq!(status, StatusCode::CREATED, "{ip}: {body}");
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{ip}: {body}");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap()
                    .contains("only a cluster administrator"),
                "{body}"
            );
        }
    }
    let mut req = common::vm_request("dns-box");
    req["spec"]["nics"] = json!([{ "network": "ext", "model": "virtio", "ips": ["203.0.113.5"] }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 웹 I1: 라우터를 읽을 수 없어도 목록이 라우팅된 네트워크를 알린다(라우터 이름 없이).
    let (status, body) = alice
        .post(
            "/routers",
            json!({ "name": "r-a", "spec": { "folder": folders[0],
                "interfaces": [{ "network": "a-net" }, { "network": "shared" }],
                "external": { "network": "ext" } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["external"]["ips"], json!(["203.0.113.100"]));
    let (_, list) = bob.get("/routers?live=false").await;
    assert_eq!(list["items"], json!([]), "{list}");
    assert_eq!(
        list["routed"],
        json!([{ "network": "shared", "external": "" }]),
        "{list}"
    );
    let (_, list) = alice.get("/routers?live=false").await;
    assert_eq!(
        list["routed"],
        json!([
            { "network": "a-net", "external": "ext" },
            { "network": "shared", "external": "ext" }
        ]),
        "{list}"
    );
    // 웹 I2: 게이트웨이 후보마다 살아 있음
    assert_eq!(list["items"][0]["gateways"][0]["online"], true, "{list}");
    // m-5: bob은 읽을 수 없는 라우터의 이름을 보지 않는다.
    let (status, body) = bob
        .post(
            "/routers",
            json!({ "name": "r-b", "spec": { "folder": folders[1], "interfaces": [{ "network": "shared" }] } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "network_in_use");
    let message = body["message"].as_str().unwrap();
    assert!(!message.contains("r-a"), "{body}");
    assert!(message.contains("a router you cannot see"), "{body}");

    // m-6: 공급자 네트워크에 IPv6 서브넷이 생겨도 설명만 바꾸는 PATCH는 외부 주소를 더하지 않는다.
    let (_, net) = admin.get("/networks/ext").await;
    let mut subnets = net["spec"]["subnets"].clone();
    subnets
        .as_array_mut()
        .unwrap()
        .push(json!({ "cidr": "2001:db8:9::/64" }));
    let (status, body) = admin
        .patch("/networks/ext", json!({ "subnets": subnets }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = alice
        .patch("/routers/r-a", json!({ "description": "edge" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["external"]["ips"], json!(["203.0.113.100"]));

    // m-8: 유동 IP 만들기의 감사에 이름·범위 밖 주소·붙인 VM
    let mut req = common::vm_request("q");
    req["spec"]["nics"] = json!([{ "network": "a-net", "model": "virtio" }]);
    req["spec"]["node"] = json!(env.node);
    let (status, q) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{q}");
    let q_id: uuid::Uuid = q["id"].as_str().unwrap().parse().unwrap();
    let q_mac = q["spec"]["nics"][0]["mac"].as_str().unwrap().to_string();
    let (status, body) = admin
        .post(
            "/floating-ips",
            json!({ "name": "fq", "spec": { "network": "ext", "address": "203.0.113.53",
                "folder": folders[1], "target": { "vm": q_id, "mac": q_mac } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // I-2: 권한을 거두면 경로·외부를 바꾸지 못한다(설명은 된다).
    let (status, body) = admin.delete(&format!("/grants/{}", alice_grants[1])).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    for patch in [
        json!({ "routes": [{ "destination": "10.88.0.0/16", "nexthop": "10.30.0.5" }] }),
        json!({ "external": null }),
    ] {
        // 이제 `shared`를 읽을 수도 없다: 없는 것과 같은 404
        let (status, body) = alice.patch("/routers/r-a", patch.clone(), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{patch}: {body}");
    }
    let (status, body) = alice
        .patch("/routers/r-a", json!({ "description": "still" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 네트워크 주인은 라우터를 읽지 못해도 자기 네트워크를 뗀다.
    let (status, body) = bob.delete("/networks/shared/router").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, r) = admin.get("/routers/r-a?live=false").await;
    assert_eq!(
        r["spec"]["interfaces"],
        json!([{ "network": "a-net" }]),
        "{r}"
    );
    let (status, body) = bob.delete("/networks/shared/router").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    // 남의 네트워크는 못 뗀다(읽을 수도 없다).
    let (status, body) = bob.delete("/networks/a-net/router").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = alice
        .patch(
            "/routers/r-a",
            json!({ "routes": [{ "destination": "10.99.0.0/16", "nexthop": "10.10.0.5" }] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 유동 IP가 이 길로 라우팅되면 떼기도 409(읽을 수 없는 유동 IP의 이름 없이)
    let (status, body) = alice.delete("/networks/a-net/router").await;
    code(status, &body, StatusCode::CONFLICT, "fip_in_use");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("1 floating IP(s) you cannot see"),
        "{body}"
    );

    // I-3: 게이트웨이 후보이고 분산 유동 IP의 VM이 도는 노드는 physnet 브리지를 바꾸지 못한다(빼기는 된다).
    power(&env, &admin, "q", q_id, true).await;
    let (status, body) = put_node_sdn(
        &env,
        &admin,
        Some(json!({ "physnets": { "ext": "vmbr2" }, "gateway": 100 })),
    )
    .await;
    code(status, &body, StatusCode::CONFLICT, "physnet_in_use");
    assert_eq!(body["details"]["vms"], json!([]), "{body}");
    assert_eq!(body["details"]["routers"], json!(["r-a"]), "{body}");
    assert_eq!(body["details"]["floating_ips"], json!(["fq"]), "{body}");
    let (status, body) = put_node_sdn(&env, &admin, Some(json!({ "gateway": 100 }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // m-4: 지운 네트워크 이름을 라우터가 아직 가리키면(라우터 만들기와 지우기가 겹침) 그 이름의 새 네트워크는 409
    let ghost: Resource<opl_types::router::RouterSpec, opl_types::router::RouterStatus> =
        Resource::new(
            "r-ghost",
            serde_json::from_value(
                json!({ "folder": folders[1], "interfaces": [{ "network": "ghost" }] }),
            )
            .unwrap(),
        );
    env.cluster.create(&ghost).await.unwrap();
    let (status, body) = alice
        .post(
            "/networks",
            json!({ "name": "ghost", "spec": { "folder": folders[0], "sdn": {}, "subnets": [{ "cidr": "10.40.0.0/24" }] } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "network_in_use");
    let message = body["message"].as_str().unwrap();
    assert!(!message.contains("r-ghost"), "{body}");
    assert!(message.contains("1 router(s) you cannot see"), "{body}");

    // m-8: 감사 설명
    let (_, audit) = admin
        .get(&format!("/audit?since=0&until={}", u64::MAX / 2))
        .await;
    let details: Vec<String> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["detail"].as_str().map(str::to_string))
        .collect();
    for want in [
        "router create r-a --interface a-net --interface shared --external ext (203.0.113.100)",
        &format!("fip create fq --network ext --ip 203.0.113.53 admin override --attach q/{q_mac}"),
        "router interface remove r-a shared (by the network)",
    ] {
        assert!(details.iter().any(|d| d == want), "{want}: {details:#?}");
    }
}
