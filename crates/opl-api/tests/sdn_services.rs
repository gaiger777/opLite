//! 계획 67 스테이지 5: SDN 서비스 API — 기능 게이트(옛 노드면 검증보다 먼저 409 `cluster_upgrading`, SDN이 없으면 409
//! `sdn_not_ready`), NIC `qos`의 검사, 부하 분산기(VIP·건강 검사·바깥 VIP IPAM, 라우터 규칙, 멤버 규칙, 보기, `lb_in_use`,
//! 주소 목록의 `lb-vip`·`lb-monitor`, 할당량), 유동 IP를 VM 주인이 떼기, 네트워크 폴더 할당량 `networks`, OVN 잠금의 미리
//! 확인과 `GET /sdn` 보기, 토폴로지.

mod common;

use common::{Env, vm_request};
use opl_types::node::{NodeSpec, SDN_SERVICES};
use opl_types::resource::Resource;
use opl_types::sdn::{SDN_NAME, SdnCentral, SdnPhase, SdnSpec, SdnStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 이 노드가 `sdn-services`를 모르는 것으로 적거나(옛 노드처럼) 되돌린다.
async fn set_services(env: &Env, on: bool) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != SDN_SERVICES);
    if on {
        status.features.push(SDN_SERVICES.to_string());
    }
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 세워진 SDN(중앙 하나)을 저장소에 바로 둔다.
async fn ready_sdn(env: &Env, lockdown: bool) {
    let mut sdn = Resource::new(SDN_NAME, SdnSpec { lockdown });
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

/// 이 노드가 chassis이고 physnet `ext`가 준비된 게이트웨이라고 적는다(SDN 에이전트 대신). `peers`는 중앙에 붙은 상대.
async fn node_sdn(env: &Env, peers: &[&str]) {
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
            "gateway": true,
            "peers": peers,
        }))
        .unwrap(),
    );
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

fn code(status: StatusCode, body: &Value, want_status: StatusCode, want: &str) {
    assert_eq!(status, want_status, "{body}");
    assert_eq!(body["code"], want, "{body}");
}

async fn net(admin: &common::Client, name: &str, spec: Value) {
    let (status, body) = admin
        .post("/networks", json!({ "name": name, "spec": spec }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn vm_on(admin: &common::Client, name: &str, network: &str) -> Value {
    let mut req = vm_request(name);
    req["spec"]["nics"] = json!([{ "network": network, "model": "virtio" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm
}

/// 옛 노드·SDN 없음의 게이트, NIC `qos` 검사, 할당량 `networks`, SDN 경보 조건이 더는 막히지 않음.
#[tokio::test]
async fn gates_qos_and_network_quotas() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let lb = json!({ "name": "web", "spec": {
        "network": "t1", "listeners": [{ "protocol": "tcp", "port": 80 }]
    } });
    let mig = json!({ "name": "m", "spec": { "source": "lan", "target": "t-lan" } });
    let mut qos_vm = vm_request("q");
    qos_vm["spec"]["nics"][0]["qos"] = json!({ "out_mbps": 100 });

    // 옛 노드가 있으면 검증보다 먼저 409
    set_services(&env, false).await;
    for (path, body) in [
        ("/load-balancers", lb.clone()),
        ("/network-migrations", mig.clone()),
        ("/vms", qos_vm.clone()),
        (
            "/folders",
            json!({ "name": "t", "kind": "network", "quota": { "networks": 1 } }),
        ),
        (
            "/alert-rules",
            json!({ "name": "sdn", "spec": { "condition": { "kind": "sdn_chassis_down" }, "severity": "warning", "targets": ["ops"] } }),
        ),
    ] {
        let (status, body) = admin.post(path, body).await;
        code(status, &body, StatusCode::CONFLICT, "cluster_upgrading");
    }
    set_services(&env, true).await;
    // SDN이 없으면 409 `sdn_not_ready`
    for (path, body) in [
        ("/load-balancers", lb),
        ("/network-migrations", mig),
        ("/sdn/lockdown", json!({ "enabled": true })),
    ] {
        let (status, body) = admin.post(path, body).await;
        code(status, &body, StatusCode::CONFLICT, "sdn_not_ready");
    }
    // SDN 경보 조건은 이제 형식 검사로 간다(대상이 없어 400).
    let (status, body) = admin
        .post(
            "/alert-rules",
            json!({ "name": "sdn", "spec": { "condition": { "kind": "sdn_chassis_down" }, "severity": "warning", "targets": ["ops"] } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        !body["message"].as_str().unwrap().contains("not available"),
        "{body}"
    );

    // NIC qos: user NIC는 400 `invalid_qos`, 빈 qos는 400 `invalid_qos`, 단순 NIC의 DSCP는 400 `qos_dscp_needs_sdn`
    let (status, body) = admin.post("/vms", qos_vm).await;
    code(status, &body, StatusCode::BAD_REQUEST, "invalid_qos");
    net(&admin, "lan", json!({ "bridge": "br0" })).await;
    let mut req = vm_request("q");
    req["spec"]["nics"] = json!([{ "network": "lan", "model": "virtio", "qos": {} }]);
    let (status, body) = admin.post("/vms", req.clone()).await;
    code(status, &body, StatusCode::BAD_REQUEST, "invalid_qos");
    req["spec"]["nics"][0]["qos"] = json!({ "out_mbps": 100, "dscp": 46 });
    let (status, body) = admin.post("/vms", req.clone()).await;
    code(status, &body, StatusCode::BAD_REQUEST, "qos_dscp_needs_sdn");
    req["spec"]["nics"][0]["qos"] = json!({ "out_mbps": 100, "in_mbps": 200 });
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["spec"]["nics"][0]["qos"],
        json!({ "out_mbps": 100, "in_mbps": 200 })
    );
    let (_, audit) = admin.get("/audit?limit=5").await;
    assert!(
        audit.to_string().contains("nic-qos 0 out=100 in=200"),
        "{audit}"
    );

    // 네트워크 폴더 할당량 `networks`
    let (status, f) = admin
        .post(
            "/folders",
            json!({ "name": "tenant", "kind": "network", "quota": { "networks": 1, "load_balancers": 1 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{f}");
    let folder = f["id"].clone();
    net(&admin, "n1", json!({ "bridge": "br1", "folder": folder })).await;
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "n2", "spec": { "bridge": "br2", "folder": folder } }),
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "quota_exceeded");
}

/// 부하 분산기 API: 라우터 규칙(400 `lb_network_not_routed`), 멤버 규칙(400 `lb_member_unreachable`), 주소 고르기(VIP·건강
/// 검사·바깥 VIP), 보기, 주소 목록, 같은 주소 409, `lb_in_use`, 할당량, 감사.
#[tokio::test]
async fn load_balancers() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, false).await;
    node_sdn(&env, &[]).await;
    let (status, body) = admin
        .put(
            &format!("/nodes/{}", env.node),
            {
                let (_, n) = admin.get(&format!("/nodes/{}", env.node)).await;
                let mut spec = n["spec"].clone();
                spec["sdn"] = json!({ "physnets": { "ext": "vmbr1" }, "gateway": 100 });
                json!({ "spec": spec })
            },
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    net(
        &admin,
        "ext",
        json!({ "sdn": { "provider": { "physnet": "ext" } }, "subnets": [{
            "cidr": "203.0.113.0/24", "gateway": "203.0.113.1",
            "floating_pools": [{ "start": "203.0.113.100", "end": "203.0.113.110" }]
        }] }),
    )
    .await;
    for (name, cidr) in [
        ("t1", "10.10.0.0/24"),
        ("t2", "10.20.0.0/24"),
        ("t3", "10.30.0.0/24"),
    ] {
        net(
            &admin,
            name,
            json!({ "sdn": {}, "subnets": [{ "cidr": cidr }] }),
        )
        .await;
    }
    let web1 = vm_on(&admin, "web1", "t1").await;
    let web2 = vm_on(&admin, "web2", "t2").await;
    let other = vm_on(&admin, "other", "t3").await;
    let (status, body) = admin
        .post(
            "/routers",
            json!({ "name": "r1", "spec": { "external": { "network": "ext" },
                "interfaces": [{ "network": "t1" }, { "network": "t2" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let member = |vm: &Value, port: u16| json!({ "vm": vm["id"], "mac": vm["spec"]["nics"][0]["mac"], "port": port });
    let health = json!({ "interval_secs": 2, "timeout_secs": 1, "rise": 1, "fall": 2 });

    // 라우터 인터페이스가 아닌 네트워크의 VIP
    let (status, body) = admin
        .post(
            "/load-balancers",
            json!({ "name": "x", "spec": { "network": "t3", "listeners": [{ "protocol": "tcp", "port": 80 }] } }),
        )
        .await;
    code(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "lb_network_not_routed",
    );
    // 라우터 밖의 멤버
    let (status, body) = admin
        .post(
            "/load-balancers",
            json!({ "name": "x", "spec": { "network": "t1",
                "listeners": [{ "protocol": "tcp", "port": 80, "members": [member(&other, 80)] }] } }),
        )
        .await;
    code(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "lb_member_unreachable",
    );
    // 같은 리스너 두 번, 검사 주소를 줌
    let (status, body) = admin
        .post(
            "/load-balancers",
            json!({ "name": "x", "spec": { "network": "t1", "listeners": [
                { "protocol": "tcp", "port": 80 }, { "protocol": "tcp", "port": 80 }] } }),
        )
        .await;
    code(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "invalid_load_balancer",
    );
    let (status, body) = admin
        .post(
            "/load-balancers",
            json!({ "name": "x", "spec": { "network": "t1", "listeners": [{ "protocol": "tcp", "port": 80 }],
                "monitors": [{ "network": "t1", "ip": "10.10.0.200" }] } }),
        )
        .await;
    code(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "invalid_load_balancer",
    );
    // 바깥이 라우터 바깥이 아님
    let (status, body) = admin
        .post(
            "/load-balancers",
            json!({ "name": "x", "spec": { "network": "t1", "external": { "network": "t2" },
                "listeners": [{ "protocol": "tcp", "port": 80 }] } }),
        )
        .await;
    code(
        status,
        &body,
        StatusCode::BAD_REQUEST,
        "lb_network_not_routed",
    );

    // 만들기: VIP는 서브넷에서, 바깥 VIP는 유동 IP 범위에서(라우터 바깥이 .100), 건강 검사 주소는 멤버 네트워크마다
    let (status, lb) = admin
        .post(
            "/load-balancers",
            json!({ "name": "web", "spec": { "network": "t1", "external": { "network": "ext" },
                "listeners": [{ "protocol": "tcp", "port": 80, "members": [member(&web1, 8080)], "health": health }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{lb}");
    let vip = lb["spec"]["vip"].as_str().unwrap().to_string();
    let nic1 = web1["spec"]["nics"][0]["ips"][0].as_str().unwrap();
    assert!(vip.starts_with("10.10.0.") && vip != nic1, "{lb}");
    assert_eq!(lb["spec"]["external"]["ip"], "203.0.113.101", "{lb}");
    let monitors = lb["spec"]["monitors"].as_array().unwrap();
    assert_eq!(monitors.len(), 1, "{lb}");
    assert_eq!(monitors[0]["network"], "t1");
    assert_ne!(monitors[0]["ip"], vip.as_str());
    let (_, audit) = admin.get("/audit?limit=5").await;
    assert!(
        audit.to_string().contains(&format!(
            "lb create web t1 vip {vip} tcp/80 → web1, lb external web ext 203.0.113.101"
        )),
        "{audit}"
    );

    // 보기(OVN을 읽지 않음): VIP·바깥 VIP 준비, 멤버 주소, 건강 검사 결과 없음
    let (status, view) = admin.get("/load-balancers/web?live=false").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["vip_state"], json!({ "state": "ready" }), "{view}");
    assert_eq!(
        view["external_state"],
        json!({ "state": "ready" }),
        "{view}"
    );
    assert_eq!(view["members"][0]["listener"], "tcp/80");
    assert_eq!(view["members"][0]["ip"], nic1);
    assert_eq!(view["members"][0]["health"], "unchecked");
    assert_eq!(view["members"][0]["vm_name"], "web1");
    let (_, list) = admin.get("/load-balancers?live=false").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");

    // 주소 목록의 `lb-vip`·`lb-monitor`
    let (_, ips) = admin.get("/networks/t1/ips?live=false").await;
    let kinds: Vec<&str> = ips["allocations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"lb-vip") && kinds.contains(&"lb-monitor"),
        "{ips}"
    );
    // VIP는 다른 NIC가 쓰지 못한다(Raft `ip`).
    let mut req = vm_request("thief");
    req["spec"]["nics"] = json!([{ "network": "t1", "model": "virtio", "ips": [vip] }]);
    let (status, body) = admin.post("/vms", req).await;
    code(status, &body, StatusCode::CONFLICT, "ip_in_use");

    // 바꾸기: 다른 인터페이스 네트워크의 멤버를 더하면 그 네트워크의 검사 주소가 생긴다.
    let (status, body) = admin
        .patch(
            "/load-balancers/web",
            json!({ "listeners": [{ "protocol": "tcp", "port": 80, "health": health,
                "members": [member(&web1, 8080), member(&web2, 8080)] }], "affinity_secs": 60 }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["monitors"].as_array().unwrap().len(), 2);
    assert_eq!(body["spec"]["vip"], vip.as_str(), "the VIP stays");
    assert_eq!(body["spec"]["affinity_secs"], 60);

    // `lb_in_use`: VIP 네트워크 인터페이스 빼기·네트워크 주인의 떼기·바깥 VIP가 쓰는 라우터 바깥 지우기
    let (status, body) = admin
        .patch(
            "/routers/r1",
            json!({ "interfaces": [{ "network": "t2" }] }),
            None,
        )
        .await;
    code(status, &body, StatusCode::CONFLICT, "lb_in_use");
    let (status, body) = admin.delete("/networks/t1/router").await;
    code(status, &body, StatusCode::CONFLICT, "lb_in_use");
    let (status, body) = admin
        .patch("/routers/r1", json!({ "external": null }), None)
        .await;
    code(status, &body, StatusCode::CONFLICT, "lb_in_use");
    // 네트워크를 쓰는 LB가 있으면 네트워크를 지우지 못한다.
    let (status, body) = admin.delete("/networks/t2").await;
    code(status, &body, StatusCode::CONFLICT, "network_in_use");
    // 바깥 VIP를 떼면 라우터 바깥을 지울 수 있다.
    let (status, body) = admin
        .patch("/load-balancers/web", json!({ "external": null }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin
        .patch("/routers/r1", json!({ "external": null }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 할당량 `load_balancers`
    let (status, f) = admin
        .post(
            "/folders",
            json!({ "name": "tenant", "kind": "network", "quota": { "load_balancers": 1 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{f}");
    for (name, ok) in [("a", true), ("b", false)] {
        let (status, body) = admin
            .post(
                "/load-balancers",
                json!({ "name": name, "spec": { "folder": f["id"], "network": "t2",
                    "listeners": [{ "protocol": "udp", "port": 53 }] } }),
            )
            .await;
        if ok {
            assert_eq!(status, StatusCode::CREATED, "{body}");
        } else {
            code(status, &body, StatusCode::CONFLICT, "quota_exceeded");
        }
    }

    // 지우면 주소가 풀린다.
    let (status, body) = admin.delete("/load-balancers/web").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, ips) = admin.get("/networks/t1/ips?live=false").await;
    assert!(!ips.to_string().contains("lb-vip"), "{ips}");

    // 토폴로지: 네트워크·라우터·LB·VM
    let (status, topo) = admin.get("/network-topology").await;
    assert_eq!(status, StatusCode::OK, "{topo}");
    let names = |k: &str| -> Vec<String> {
        topo[k]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names("routers"), ["r1"]);
    assert_eq!(names("load_balancers"), ["a"]);
    assert!(names("networks").contains(&"ext".to_string()), "{topo}");
    assert_eq!(topo["networks"][0]["kind"], "provider", "{topo}");
    assert!(names("vms").contains(&"web1".to_string()), "{topo}");
    assert_eq!(topo["hidden"]["vms"], 0);
    // 폴더로 거르면 그 폴더의 LB만
    let (_, topo) = admin
        .get(&format!(
            "/network-topology?folder={}",
            f["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(topo["load_balancers"].as_array().unwrap().len(), 1);
    assert!(topo["routers"].as_array().unwrap().is_empty(), "{topo}");
}

/// 유동 IP 떼기는 유동 IP의 `network.config` 또는 붙은 VM의 `vm.config`(떼기만 — 계획 67 §13).
#[tokio::test]
async fn vm_owners_detach_floating_ips() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, false).await;
    node_sdn(&env, &[]).await;
    net(
        &admin,
        "ext",
        json!({ "sdn": { "provider": { "physnet": "ext" } }, "subnets": [{ "cidr": "203.0.113.0/24" }] }),
    )
    .await;
    net(
        &admin,
        "t1",
        json!({ "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] }),
    )
    .await;
    let web = vm_on(&admin, "web", "t1").await;
    let (status, body) = admin
        .post(
            "/routers",
            json!({ "name": "r1", "spec": { "external": { "network": "ext" }, "interfaces": [{ "network": "t1" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, f) = admin
        .post(
            "/floating-ips",
            json!({ "name": "f1", "spec": { "network": "ext",
                "target": { "vm": web["id"], "mac": web["spec"]["nics"][0]["mac"] } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{f}");
    let (status, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "vmcfg", "privileges": ["vm.config", "system.read"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (status, user) = admin
        .post(
            "/users",
            json!({ "name": "bob", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    let (status, body) = admin
        .post(
            "/grants",
            json!({ "principal": { "type": "user", "id": user["id"] },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "object", "kind": "vm", "id": web["id"] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let bob = env.login("bob", "password-1").await;
    // 다른 칸은 바꾸지 못한다(유동 IP를 읽을 수도 없다 — 404).
    let (status, body) = bob
        .patch("/floating-ips/f1", json!({ "description": "x" }), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = bob
        .patch("/floating-ips/f1", json!({ "target": null }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("target").is_none(), "{body}");
    // 떼어진 뒤에는 VM 주인이 아니다(다시 붙이지 못함).
    let (status, _) = bob
        .patch("/floating-ips/f1", json!({ "target": null }), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// OVN 잠금의 미리 확인(`NodeSdn.peers`와 허용 집합), `force`, `GET /sdn`의 잠금 보기.
#[tokio::test]
async fn lockdown_dry_run_and_view() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_sdn(&env, false).await;
    // 이 노드의 주소(127.0.0.1)는 멤버다. 10.9.9.9는 집합 밖이다(NAT·지운 노드).
    node_sdn(&env, &["127.0.0.1", "10.9.9.9", "::ffff:10.9.9.9"]).await;
    let (_, view) = admin.get("/sdn").await;
    assert!(view.get("lockdown").is_none(), "never enabled: {view}");
    let (status, check) = admin
        .post("/sdn/lockdown?dry_run=true", json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{check}");
    assert_eq!(
        check["outside"],
        json!([{ "central": "node1", "peer": "10.9.9.9" }]),
        "{check}"
    );
    assert_eq!(check["blocked"], true);
    let (status, body) = admin
        .post("/sdn/lockdown", json!({ "enabled": true }))
        .await;
    code(
        status,
        &body,
        StatusCode::CONFLICT,
        "lockdown_peers_outside",
    );
    let (status, check) = admin
        .post(
            "/sdn/lockdown?dry_run=true",
            json!({ "enabled": true, "force": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{check}");
    assert_eq!(check["blocked"], false);
    let (status, task) = admin
        .post("/sdn/lockdown", json!({ "enabled": true, "force": true }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    // `sdn-lockdown` Task가 `SdnSpec.lockdown`을 쓴다.
    let deadline = std::time::Instant::now() + common::WAIT;
    let view = loop {
        let (_, view) = admin.get("/sdn").await;
        if view["lockdown"]["enabled"] == true {
            break view;
        }
        assert!(std::time::Instant::now() < deadline, "{view}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(view["lockdown"]["nodes"][0]["firewall"], false, "{view}");
    assert!(
        view["lockdown"]["members"]
            .as_array()
            .unwrap()
            .contains(&json!("127.0.0.1")),
        "{view}"
    );
    let (_, audit) = admin.get("/audit?limit=5").await;
    assert!(
        audit.to_string().contains("sdn lockdown on --force"),
        "{audit}"
    );
}
