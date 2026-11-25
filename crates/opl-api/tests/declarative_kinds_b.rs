//! 계획 73 스테이지 3 선언 종류(HA 묶음·HA 검사·역할·그룹·grant·부하 분산기·ACME 계정·DNS 공급자·인증서): 만들기 →
//! 다시 적용하면 그대로 → 칸 하나 바꾸면 update 하나 → 내보내기 왕복이 모두 그대로, 의존 차례(HA 묶음이 VM보다·역할·그룹이
//! grant보다), grant는 내용이 열쇠(바꾸면 새 grant + 가지치기), 비밀은 계획·묶음·내보내기에 없음, `secretRef`뿐인 새 비밀은
//! 막힘, `DELETE /manifests`.

mod common;

use std::sync::Arc;

use common::acme::seed_account;
use common::{Client, Env, FakeStorage, vm_request};
use opl_types::acme::{
    AcmeAccountSpec, AcmeDnsSpec, CertificateSpec, ExternalAccountBinding, Secret,
};
use opl_types::authz::{AccessRoleSpec, GrantSpec, GroupSpec};
use opl_types::ha_check::HaCheckSpec;
use opl_types::load_balancer::LoadBalancerSpec;
use opl_types::node::NodeSpec;
use opl_types::placement::HaGroupSpec;
use opl_types::resource::Resource;
use opl_types::sdn::{SDN_NAME, SdnCentral, SdnPhase, SdnSpec, SdnStatus};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn start() -> (Env, Client) {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env.start_runner_with(Arc::new(FakeStorage::default()))
        .await;
    let admin = env.admin().await;
    (env, admin)
}

fn doc(kind: &str, name: &str, spec: Value) -> Value {
    json!({ "apiVersion": "orbivisor.io/v1", "kind": kind, "metadata": { "name": name }, "spec": spec })
}

async fn dry_run(client: &Client, body: Value) -> Value {
    let (status, plan) = client.post("/apply?dry_run=true", body).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    plan
}

async fn apply(client: &Client, body: Value) -> Value {
    let (status, started) = client.post("/apply", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let task = client.wait_task(&started["task"]).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    task
}

fn summary(plan: &Value) -> [u64; 5] {
    let s = &plan["summary"];
    ["create", "update", "delete", "unchanged", "blocked"].map(|k| s[k].as_u64().unwrap())
}

fn kinds(plan: &Value) -> Vec<String> {
    plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap().to_string())
        .collect()
}

/// 묶음을 내보내 그대로 적용하면 모두 그대로다.
async fn round_trip(admin: &Client, set: &str, kinds: &str) -> Vec<Value> {
    let (status, exported) = admin
        .get(&format!("/manifests?apply_set={set}&kinds={kinds}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let docs = exported["documents"].as_array().unwrap().clone();
    let plan = dry_run(admin, json!({ "documents": docs, "apply_set": set })).await;
    assert_eq!(
        plan["summary"]["unchanged"].as_u64().unwrap(),
        plan["steps"].as_array().unwrap().len() as u64,
        "{plan}"
    );
    docs
}

#[tokio::test]
async fn ha_and_access_kinds_apply_in_order_and_round_trip() {
    let (env, admin) = start().await;
    env.user("alice", "viewer").await;
    let mut vm = vm_request("web01");
    vm["spec"]["ha"] = json!({ "group": "pair" });
    let mut vm_doc = doc("Vm", "web01", vm["spec"].clone());
    vm_doc["state"] = json!("stopped");
    let mut folder = doc("Folder", "prod", json!({ "kind": "vm" }));
    folder["metadata"]["folder"] = json!("/");
    let docs = |privileges: Value, target: &str| {
        vec![
            doc(
                "Grant",
                "ops-prod",
                json!({ "group": "ops", "role": "web-admin", "target": target }),
            ),
            doc(
                "Grant",
                "alice-root",
                json!({ "user": "alice", "role": "viewer", "target": "/", "propagate": false }),
            ),
            vm_doc.clone(),
            doc(
                "HaCheck",
                "vmnet",
                json!({ "role": "vm", "target": { "kind": "link", "iface": "bond0" } }),
            ),
            doc(
                "Group",
                "ops",
                json!({ "members": ["alice"], "description": "ops" }),
            ),
            doc(
                "AccessRole",
                "web-admin",
                json!({ "privileges": privileges }),
            ),
            doc(
                "HaGroup",
                "pair",
                json!({ "nodes": [{ "node": "node1", "priority": 2 }] }),
            ),
            folder.clone(),
        ]
    };
    let body = |privileges: Value, target: &str| json!({ "documents": docs(privileges, target), "apply_set": "acl", "prune": true });

    let plan = dry_run(&admin, body(json!(["vm.power"]), "vm:/prod")).await;
    assert_eq!(summary(&plan), [8, 0, 0, 0, 0], "{plan}");
    let order = kinds(&plan);
    let at = |k: &str| order.iter().position(|x| x == k).unwrap();
    assert!(at("HaGroup") < at("Vm"), "{order:?}");
    assert!(
        at("AccessRole") < at("Grant") && at("Group") < at("Grant"),
        "{order:?}"
    );
    apply(&admin, body(json!(["vm.power"]), "vm:/prod")).await;

    let vm = env.cluster.get_by_name::<VmSpec>("web01").unwrap().unwrap();
    assert_eq!(vm.spec.ha.as_ref().unwrap().group.as_deref(), Some("pair"));
    let group = env
        .cluster
        .get_by_name::<GroupSpec>("ops")
        .unwrap()
        .unwrap();
    assert_eq!(group.spec.members.len(), 1);
    assert_eq!(
        group
            .labels
            .get("orbivisor.io/apply-set")
            .map(String::as_str),
        Some("acl")
    );
    let role = env
        .cluster
        .get_by_name::<AccessRoleSpec>("web-admin")
        .unwrap()
        .unwrap();
    assert_eq!(
        role.labels
            .get("orbivisor.io/apply-set")
            .map(String::as_str),
        Some("acl")
    );
    let (_, grants) = env.cluster.list::<GrantSpec>().unwrap();
    assert_eq!(grants.len(), 2);
    assert!(
        grants
            .iter()
            .all(|g| g.labels.contains_key("orbivisor.io/apply-set"))
    );
    assert!(
        env.cluster
            .get_by_name::<HaGroupSpec>("pair")
            .unwrap()
            .is_some()
    );
    assert!(
        env.cluster
            .get_by_name::<HaCheckSpec>("vmnet")
            .unwrap()
            .is_some()
    );

    // 다시 적용하면 모두 그대로
    let plan = dry_run(&admin, body(json!(["vm.power"]), "vm:/prod")).await;
    assert_eq!(summary(&plan), [0, 0, 0, 8, 0], "{plan}");
    // 역할 권한 하나 더 → update 하나
    let plan = dry_run(&admin, body(json!(["vm.power", "vm.console"]), "vm:/prod")).await;
    assert_eq!(summary(&plan), [0, 1, 0, 7, 0], "{plan}");
    apply(&admin, body(json!(["vm.power", "vm.console"]), "vm:/prod")).await;
    let role = env
        .cluster
        .get_by_name::<AccessRoleSpec>("web-admin")
        .unwrap()
        .unwrap();
    assert_eq!(role.spec.privileges, ["vm.power", "vm.console"]);

    // 내보내기 왕복(grant 이름은 내용에서 — UUID가 아니다)
    let exported = round_trip(&admin, "acl", "Grant,Group,AccessRole,HaGroup,HaCheck").await;
    assert_eq!(exported.len(), 6, "{exported:?}");
    let grant = exported
        .iter()
        .find(|d| d["kind"] == "Grant" && d["spec"]["group"] == "ops")
        .unwrap();
    assert_eq!(grant["spec"]["target"], "vm:/prod", "{grant}");
    assert!(
        grant["metadata"]["name"]
            .as_str()
            .unwrap()
            .starts_with("grant-")
    );
    let ops = exported.iter().find(|d| d["kind"] == "Group").unwrap();
    assert_eq!(ops["spec"]["members"], json!(["alice"]));

    // grant 대상을 바꾸면 다른 grant다: 새로 주고 옛 것은 가지치기
    let plan = dry_run(&admin, body(json!(["vm.power", "vm.console"]), "/")).await;
    assert_eq!(summary(&plan), [1, 0, 1, 7, 0], "{plan}");
    apply(&admin, body(json!(["vm.power", "vm.console"]), "/")).await;
    let (_, grants) = env.cluster.list::<GrantSpec>().unwrap();
    assert_eq!(grants.len(), 2);
    assert!(
        grants
            .iter()
            .any(|g| g.spec.target == opl_types::authz::GrantTarget::Root
                && matches!(
                    g.spec.principal,
                    opl_types::authz::PrincipalRef::Group { .. }
                ))
    );

    // 없는 사용자는 막힘
    let plan = dry_run(
        &admin,
        json!({ "documents": [doc("Group", "ghosts", json!({ "members": ["nobody"] }))] }),
    )
    .await;
    assert_eq!(summary(&plan), [0, 0, 0, 0, 1], "{plan}");
    assert!(plan.to_string().contains("nobody"), "{plan}");

    // 하나 지우기
    let (status, body) = admin.delete("/manifests/HaCheck/vmnet").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        env.cluster
            .get_by_name::<HaCheckSpec>("vmnet")
            .unwrap()
            .is_none()
    );
    let id = grants[0].id;
    let (status, body) = admin.delete(&format!("/manifests/Grant/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(env.cluster.get::<GrantSpec>(id).unwrap().is_none());
}

#[tokio::test]
async fn acme_kinds_keep_secrets_out_and_round_trip() {
    let (env, admin) = start().await;
    seed_account(&env.cluster, "le", "https://127.0.0.1:1/dir").await;
    // EAB가 있는 계정(만들기는 CA에 등록하므로 저장소에 바로 둔다)
    let mut zs: Resource<AcmeAccountSpec, _> = Resource::new(
        "zs",
        AcmeAccountSpec {
            directory: "https://127.0.0.1:1/zs".into(),
            email: Some("ops@example.com".into()),
            agree_tos: true,
            eab: Some(ExternalAccountBinding {
                kid: "kid-1".into(),
                hmac_key: Secret::new("hmac-secret"),
            }),
            ca_bundle: None,
            key: Secret::new("-----BEGIN PRIVATE KEY-----zs"),
        },
    );
    zs.status = Default::default();
    env.cluster.create(&zs).await.unwrap();

    let dns = |token: Value| {
        doc(
            "AcmeDns",
            "cf",
            json!({ "provider": { "kind": "cloudflare", "api_token": token }, "zones": ["example.com"] }),
        )
    };
    let cert = |names: Value| {
        doc(
            "Certificate",
            "vip",
            json!({ "account": "le", "names": names, "scope": { "kind": "shared" },
                    "challenge": { "kind": "dns01", "dns": "cf" } }),
        )
    };
    let body = |names: Value| json!({ "documents": [cert(names), dns(json!("tok-secret"))], "apply_set": "certs" });
    let plan = dry_run(&admin, body(json!(["example.com"]))).await;
    assert_eq!(summary(&plan), [2, 0, 0, 0, 0], "{plan}");
    assert_eq!(kinds(&plan), ["AcmeDns", "Certificate"]);
    assert!(!plan.to_string().contains("tok-secret"), "{plan}");
    apply(&admin, body(json!(["example.com"]))).await;
    let stored = env
        .cluster
        .get_by_name::<AcmeDnsSpec>("cf")
        .unwrap()
        .unwrap();
    assert!(
        serde_json::to_string(&stored.spec)
            .unwrap()
            .contains("tok-secret")
    );
    let (_, set) = admin.get("/apply-sets/certs").await;
    assert!(!set.to_string().contains("tok-secret"), "{set}");

    // 다시 적용(평문 그대로) → 그대로, 이름 하나 더 → update 하나
    let plan = dry_run(&admin, body(json!(["example.com"]))).await;
    assert_eq!(summary(&plan), [0, 0, 0, 2, 0], "{plan}");
    let plan = dry_run(&admin, body(json!(["example.com", "*.example.com"]))).await;
    assert_eq!(summary(&plan), [0, 1, 0, 1, 0], "{plan}");
    apply(&admin, body(json!(["example.com", "*.example.com"]))).await;
    let c = env
        .cluster
        .get_by_name::<CertificateSpec>("vip")
        .unwrap()
        .unwrap();
    assert_eq!(c.spec.names.len(), 2);
    // 토큰이 바뀌면 update(값은 보이지 않는다)
    let plan = dry_run(
        &admin,
        json!({ "documents": [dns(json!("tok-new"))], "apply_set": "certs" }),
    )
    .await;
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");
    assert!(
        plan.to_string().contains("(changed)") && !plan.to_string().contains("tok-"),
        "{plan}"
    );

    // 내보내기: 계정 키·EAB·DNS 토큰 없음, 그대로 적용하면 모두 그대로
    let (status, exported) = admin
        .get("/manifests?kinds=AcmeAccount,AcmeDns,Certificate")
        .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let text = exported.to_string();
    for secret in ["tok-secret", "hmac-secret", "PRIVATE KEY"] {
        assert!(!text.contains(secret), "{secret}: {exported}");
    }
    let docs = exported["documents"].as_array().unwrap().clone();
    assert_eq!(docs.len(), 4, "{exported}");
    let zs_doc = docs.iter().find(|d| d["metadata"]["name"] == "zs").unwrap();
    assert_eq!(
        zs_doc["spec"]["eab"]["hmac_key"]["secretRef"],
        "AcmeAccount/zs/eab.hmac_key"
    );
    assert!(zs_doc["spec"].get("key").is_none());
    let plan = dry_run(&admin, json!({ "documents": docs })).await;
    assert_eq!(summary(&plan), [0, 0, 0, 4, 0], "{plan}");
    let stored = env
        .cluster
        .get_by_name::<AcmeAccountSpec>("zs")
        .unwrap()
        .unwrap();
    assert_eq!(stored.spec.eab.unwrap().hmac_key.expose(), "hmac-secret");

    // 계정 연락처 → update(적용은 CA에 등록하므로 미리 확인만), 디렉터리 → 막힘
    let mut changed = zs_doc.clone();
    changed["spec"]["email"] = json!("sec@example.com");
    let plan = dry_run(&admin, json!({ "documents": [changed.clone()] })).await;
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");
    changed["spec"]["directory"] = json!("https://127.0.0.1:1/other");
    let plan = dry_run(&admin, json!({ "documents": [changed] })).await;
    assert_eq!(summary(&plan), [0, 0, 0, 0, 1], "{plan}");
    assert_eq!(
        plan["steps"][0]["problems"][0]["code"], "immutable",
        "{plan}"
    );

    // 새 계정 미리 확인: 키 없음. 비밀 참조뿐인 새 DNS 공급자 → 막힘 `secret_missing`
    let plan = dry_run(
        &admin,
        json!({ "documents": [
            doc("AcmeAccount", "new", json!({ "directory": "letsencrypt-staging", "agree_tos": true })),
            doc("AcmeDns", "pdns", json!({ "provider": { "kind": "power_dns", "url": "http://127.0.0.1:1", "api_key": { "secretRef": "AcmeDns/pdns/provider.api_key" } } })),
        ] }),
    )
    .await;
    assert_eq!(summary(&plan), [1, 0, 0, 0, 1], "{plan}");
    assert!(!plan.to_string().contains("\"key\""), "{plan}");
    let blocked = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["action"] == "blocked")
        .unwrap();
    assert_eq!(
        blocked["problems"][0]["code"], "secret_missing",
        "{blocked}"
    );

    // 공급자 갈래를 바꾸면 그 칸이 통째로 바뀐다(옛 갈래의 칸이 남지 않음)
    let plan = dry_run(
        &admin,
        json!({ "documents": [doc("AcmeDns", "cf", json!({ "provider": { "kind": "power_dns", "url": "http://127.0.0.1:1", "api_key": "k" } }))] }),
    )
    .await;
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");

    // 지우기(인증서 → 공급자)
    let (status, body) = admin.delete("/manifests/Certificate/vip").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin.delete("/manifests/AcmeDns/cf").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        env.cluster
            .get_by_name::<AcmeDnsSpec>("cf")
            .unwrap()
            .is_none()
    );
}

/// 세워진 SDN(중앙 하나)과 이 노드의 chassis·게이트웨이(계획 67 시험 꼴)
async fn sdn_ready(env: &Env, admin: &Client) {
    let mut sdn = Resource::new(SDN_NAME, SdnSpec { lockdown: false });
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
            "peers": [],
        }))
        .unwrap(),
    );
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (_, n) = admin.get(&format!("/nodes/{}", env.node)).await;
    let mut spec = n["spec"].clone();
    spec["sdn"] = json!({ "physnets": { "ext": "vmbr1" }, "gateway": 100 });
    let (status, body) = admin
        .put(
            &format!("/nodes/{}", env.node),
            json!({ "spec": spec }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for (name, spec) in [
        (
            "ext",
            json!({ "sdn": { "provider": { "physnet": "ext" } }, "subnets": [{
                "cidr": "203.0.113.0/24", "gateway": "203.0.113.1",
                "floating_pools": [{ "start": "203.0.113.100", "end": "203.0.113.110" }] }] }),
        ),
        (
            "t1",
            json!({ "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] }),
        ),
    ] {
        let (status, body) = admin
            .post("/networks", json!({ "name": name, "spec": spec }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = admin
        .post(
            "/routers",
            json!({ "name": "r1", "spec": { "external": { "network": "ext" }, "interfaces": [{ "network": "t1" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn load_balancers_name_their_member_vms_and_keep_server_addresses() {
    let (env, admin) = start().await;
    sdn_ready(&env, &admin).await;
    let mut req = vm_request("web1");
    req["spec"]["nics"] = json!([{ "network": "t1", "model": "virtio" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mac = vm["spec"]["nics"][0]["mac"].clone();
    let lb = |port: u16| {
        doc(
            "LoadBalancer",
            "web",
            json!({ "network": "t1", "listeners": [{ "protocol": "tcp", "port": port,
                "members": [{ "vm": "web1", "mac": mac, "port": 8080 }] }] }),
        )
    };
    let body = |port: u16| json!({ "documents": [lb(port)], "apply_set": "lb" });
    let plan = dry_run(&admin, body(80)).await;
    assert_eq!(summary(&plan), [1, 0, 0, 0, 0], "{plan}");
    apply(&admin, body(80)).await;
    let stored = env
        .cluster
        .get_by_name::<LoadBalancerSpec>("web")
        .unwrap()
        .unwrap();
    assert!(stored.spec.vip.is_some(), "the server picked a VIP");
    assert_eq!(
        stored.spec.listeners[0].members[0].vm,
        Some(vm["id"].as_str().unwrap().parse().unwrap())
    );
    // 서버가 고른 VIP가 있어도 그대로
    let plan = dry_run(&admin, body(80)).await;
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");
    let exported = round_trip(&admin, "lb", "LoadBalancer").await;
    assert_eq!(
        exported[0]["spec"]["listeners"][0]["members"][0]["vm"],
        "web1"
    );
    assert!(exported[0]["spec"].get("monitors").is_none());
    // 포트 → update 하나(VIP 그대로)
    let plan = dry_run(&admin, body(81)).await;
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");
    apply(&admin, body(81)).await;
    let after = env
        .cluster
        .get_by_name::<LoadBalancerSpec>("web")
        .unwrap()
        .unwrap();
    assert_eq!(after.spec.listeners[0].port, 81);
    assert_eq!(after.spec.vip, stored.spec.vip);
    let (status, body) = admin.delete("/manifests/LoadBalancer/web").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        env.cluster
            .get_by_name::<LoadBalancerSpec>("web")
            .unwrap()
            .is_none()
    );
}
