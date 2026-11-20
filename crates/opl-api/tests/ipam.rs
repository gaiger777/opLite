//! 계획 65 §9 API: SDN NIC 주소(IPAM) — 새 NIC의 자동 할당, 준 주소의 검사, 있던 NIC의 주소 유지, 템플릿, 오류 코드.
//! SDN 네트워크는 저장소에 바로 만든다(`POST /networks`는 세운 OVN이 있어야 한다 — `tests/sdn.rs`).

mod common;

use common::{Env, vm_request};
use opl_types::network::Network;
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn sdn_network(env: &Env, name: &str, subnets: Value) {
    let net: Network = Resource::new(
        name,
        serde_json::from_value(json!({ "sdn": {}, "subnets": subnets })).unwrap(),
    );
    env.cluster.create(&net).await.unwrap();
}

fn vm_on(name: &str, nics: Value) -> Value {
    let mut req = vm_request(name);
    req["spec"]["nics"] = nics;
    req
}

#[tokio::test]
async fn sdn_nics_get_addresses_from_the_subnets() {
    let env = Env::start().await;
    let admin = env.admin().await;
    sdn_network(
        &env,
        "tenant",
        json!([{ "cidr": "10.10.0.0/29" }, { "cidr": "2001:db8::/64" }]),
    )
    .await;
    sdn_network(&env, "other", json!([{ "cidr": "10.10.0.0/29" }])).await;

    let (status, a) = admin
        .post(
            "/vms",
            vm_on("a", json!([{ "network": "tenant", "model": "virtio" }])),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{a}");
    assert_eq!(
        a["spec"]["nics"][0]["ips"],
        json!(["10.10.0.2", "2001:db8::2"])
    );
    // 다음 VM은 다음 빈 주소다. 폴더(네트워크)가 다르면 같은 CIDR의 같은 주소를 쓴다.
    let (_, b) = admin
        .post(
            "/vms",
            vm_on(
                "b",
                json!([
                    { "network": "tenant", "model": "virtio" },
                    { "network": "other", "model": "virtio" }
                ]),
            ),
        )
        .await;
    assert_eq!(
        b["spec"]["nics"][0]["ips"],
        json!(["10.10.0.3", "2001:db8::3"])
    );
    assert_eq!(b["spec"]["nics"][1]["ips"], json!(["10.10.0.2"]));

    // 준 주소: 쓰는 주소는 409, 서브넷 밖·예약은 400, 단순 네트워크는 400.
    let given = |name: &str, ip: &str| {
        vm_on(
            name,
            json!([{ "network": "tenant", "model": "virtio", "ips": [ip] }]),
        )
    };
    let (status, body) = admin.post("/vms", given("c", "10.10.0.3")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ip_in_use");
    assert!(
        body["message"].as_str().unwrap().contains("\"b\""),
        "{body}"
    );
    for bad in ["10.10.0.1", "10.20.0.5"] {
        let (status, body) = admin.post("/vms", given("c", bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, body) = admin
        .post(
            "/vms",
            vm_on(
                "c",
                json!([{ "network": "user", "model": "virtio", "ips": ["10.0.2.15"] }]),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "ips_without_sdn");
    let (status, c) = admin.post("/vms", given("c", "10.10.0.6")).await;
    assert_eq!(status, StatusCode::CREATED, "{c}");
    assert_eq!(
        c["spec"]["nics"][0]["ips"],
        json!(["10.10.0.6", "2001:db8::4"])
    );

    // SDN NIC의 cloud-init 주소는 400이다(보안 그룹은 계획 66부터 받는다 — sdn_routing.rs).
    let mut req = vm_on("d", json!([{ "network": "tenant", "model": "virtio" }]));
    req["spec"]["cloud_init"] = json!({ "networks": [{ "ipv4": { "mode": "dhcp" } }] });
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "cloud_init_sdn_nic");

    // /29에는 다섯(.2~.6)이 있다: .4, .5를 쓰면 다 찬다.
    for name in ["e", "f"] {
        let (status, body) = admin
            .post(
                "/vms",
                vm_on(name, json!([{ "network": "tenant", "model": "virtio" }])),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = admin
        .post(
            "/vms",
            vm_on("g", json!([{ "network": "tenant", "model": "virtio" }])),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ip_exhausted");
}

#[tokio::test]
async fn updates_keep_or_check_the_addresses() {
    let env = Env::start().await;
    let admin = env.admin().await;
    sdn_network(&env, "tenant", json!([{ "cidr": "10.10.0.0/24" }])).await;
    let (_, a) = admin
        .post(
            "/vms",
            vm_on("a", json!([{ "network": "tenant", "model": "virtio" }])),
        )
        .await;
    assert_eq!(a["spec"]["nics"][0]["ips"], json!(["10.10.0.2"]));
    // 주소를 모르는 PUT(칸 없음)은 주소를 지우지 않는다.
    let mut spec = a["spec"].clone();
    spec["nics"][0].as_object_mut().unwrap().remove("ips");
    spec["memory_mib"] = json!(512);
    let (status, body) = admin.put("/vms/a", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["nics"][0]["ips"], json!(["10.10.0.2"]));
    // 꺼진 VM의 주소는 바꿀 수 있다(검사한다).
    let mut spec = body["spec"].clone();
    spec["nics"][0]["ips"] = json!(["10.10.0.9"]);
    let (status, body) = admin.put("/vms/a", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["nics"][0]["ips"], json!(["10.10.0.9"]));
    // 옛 주소는 풀렸다: 다음 새 NIC가 받는다.
    let (_, b) = admin
        .post(
            "/vms",
            vm_on("b", json!([{ "network": "tenant", "model": "virtio" }])),
        )
        .await;
    assert_eq!(b["spec"]["nics"][0]["ips"], json!(["10.10.0.2"]));
    let mut spec = b["spec"].clone();
    spec["nics"][0]["ips"] = json!(["10.10.0.9"]);
    let (status, body) = admin.put("/vms/b", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ip_in_use");

    // 템플릿으로 바꾸면 주소를 지운다(주소가 풀린다).
    let (status, body) = admin.post("/vms/a/template", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"]["nics"][0].get("ips").is_none(), "{body}");
    let (_, c) = admin
        .post(
            "/vms",
            vm_on(
                "c",
                json!([{ "network": "tenant", "model": "virtio", "ips": ["10.10.0.9"] }]),
            ),
        )
        .await;
    assert_eq!(c["spec"]["nics"][0]["ips"], json!(["10.10.0.9"]), "{c}");
}
