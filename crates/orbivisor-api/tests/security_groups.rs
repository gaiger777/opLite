//! 보안 그룹 API: 관리자만 만들고 바꾸고 지우며, VM의 브리지 네트워크 NIC가 쓰고, 쓰는 동안 지울 수 없다.

mod common;

use common::{Client, Env, vm_request};
use orbivisor_types::node::{BridgeInfo, HOTPLUG, NodeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn group(name: &str, spec: Value) -> Value {
    json!({ "name": name, "spec": spec })
}

fn ssh() -> Value {
    json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "22", "remote": "10.0.0.0/8" })
}

/// `lan` 브리지 네트워크 NIC 하나에 그룹을 건 VM 요청
fn vm_with_groups(name: &str, groups: Value) -> Value {
    let mut vm = vm_request(name);
    vm["spec"]["nics"][0] =
        json!({ "network": "lan", "model": "virtio", "security_groups": groups });
    vm
}

#[tokio::test]
async fn admins_manage_security_groups_and_others_read() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, body) = admin
        .post(
            "/security-groups",
            group(
                "web",
                json!({ "description": "web servers", "rules": [ssh()] }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["rules"][0], ssh());
    assert_eq!(body["spec"]["description"], "web servers");
    // 규칙이 없어도 된다(들어오기 모두 차단).
    let (status, body) = admin
        .post("/security-groups", group("closed", json!({})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"], json!({ "rules": [] }));

    assert_eq!(
        operator
            .post("/security-groups", group("db", json!({})))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator.delete("/security-groups/web").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, list) = viewer.get("/security-groups").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(
        names.contains(&"web") && names.contains(&"closed"),
        "{list}"
    );
    let (status, body) = viewer.get("/security-groups/web").await;
    assert_eq!(status, StatusCode::OK);
    let id = body["id"].as_str().unwrap().to_owned();
    assert_eq!(
        viewer.get(&format!("/security-groups/{id}")).await.1["name"],
        "web"
    );
    assert_eq!(
        viewer.get("/security-groups/nope").await.0,
        StatusCode::NOT_FOUND
    );

    let icmp_ports =
        json!({ "direction": "in", "action": "accept", "protocol": "icmp", "ports": "22" });
    for (bad, why) in [
        (group("Bad_Name", json!({})), "name"),
        (
            group("long", json!({ "description": "x".repeat(201) })),
            "description",
        ),
        (
            group("many", json!({ "rules": vec![ssh(); 201] })),
            "rule count",
        ),
        (
            group("ports", json!({ "rules": [ssh(), icmp_ports] })),
            "rule 2",
        ),
        (
            group(
                "cidr",
                json!({ "rules": [{ "direction": "in", "action": "drop", "remote": "10.0.0.1/8" }] }),
            ),
            "host bits",
        ),
        (group("extra", json!({ "colour": "red" })), "unknown field"),
    ] {
        let (status, body) = admin.post("/security-groups", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request", "{why}: {body}");
        if why == "rule 2" {
            assert!(
                body["message"].as_str().unwrap().contains("rule 2"),
                "{body}"
            );
        }
    }
    let (status, body) = admin
        .post("/security-groups", group("web", json!({})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "name_taken");

    assert_eq!(
        admin.delete("/security-groups/closed").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        admin.get("/security-groups/closed").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn patch_changes_rules_and_description() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let (_, created) = admin
        .post(
            "/security-groups",
            group(
                "web",
                json!({ "description": "web servers", "rules": [ssh()] }),
            ),
        )
        .await;
    let revision = created["revision"].as_u64().unwrap();

    // 주어진 필드만 바꾼다.
    let http = json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "80" });
    let (status, body) = admin
        .patch(
            "/security-groups/web",
            json!({ "rules": [ssh(), http] }),
            Some(revision),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["rules"], json!([ssh(), http]));
    assert_eq!(body["spec"]["description"], "web servers");
    let changed = body["revision"].as_u64().unwrap();
    assert!(changed > revision);
    // 조회는 그룹을 건 NIC 수(`status.nics`, 계획 66)를 더 싣는다.
    let mut got = admin.get("/security-groups/web").await.1;
    assert_eq!(got["status"]["nics"], json!({ "simple": 0, "sdn": 0 }));
    got["status"] = json!({});
    assert_eq!(got, body);

    // 옛 revision이면 412
    let (status, body) = admin
        .patch(
            "/security-groups/web",
            json!({ "rules": [] }),
            Some(revision),
        )
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_eq!(body["code"], "revision_mismatch");

    // 빈 설명은 설명을 지운다. If-Match가 없으면 지금 revision에 쓴다.
    let (status, body) = admin
        .patch(
            "/security-groups/web",
            json!({ "description": "", "rules": [] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"], json!({ "rules": [] }));

    for (bad, why) in [
        (json!({ "description": "x".repeat(201) }), "description"),
        (
            json!({ "rules": [{ "direction": "out", "action": "drop", "protocol": "udp", "ports": "9-8" }] }),
            "ports",
        ),
        (json!({ "name": "other" }), "unknown field"),
    ] {
        let (status, body) = admin.patch("/security-groups/web", bad, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request", "{why}: {body}");
    }
    assert_eq!(
        admin
            .patch("/security-groups/nope", json!({ "rules": [] }), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        viewer
            .patch("/security-groups/web", json!({ "rules": [] }), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn vms_use_existing_groups_which_cannot_be_deleted_while_used() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    admin
        .post(
            "/security-groups",
            group("web", json!({ "rules": [ssh()] })),
        )
        .await;

    // 없는 그룹
    let (status, body) = admin
        .post("/vms", vm_with_groups("vm1", json!(["web", "db"])))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"].as_str().unwrap().contains("\"db\""),
        "{body}"
    );
    // user 네트워크에는 걸 수 없다.
    let mut user = vm_request("vm1");
    user["spec"]["nics"][0]["security_groups"] = json!(["web"]);
    let (status, body) = admin.post("/vms", user).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(body["message"].as_str().unwrap().contains("user"), "{body}");
    // 개수·중복은 spec 검증에서 걸린다.
    let (status, body) = admin
        .post("/vms", vm_with_groups("vm1", json!(["web", "web"])))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = admin
        .post("/vms", vm_with_groups("vm1", json!(["web"])))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["nics"][0]["security_groups"], json!(["web"]));

    // VM을 바꿀 때도 같은 검사를 한다.
    let mut spec = body["spec"].clone();
    spec["nics"][0]["security_groups"] = json!(["web", "db"]);
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("\"db\""),
        "{body}"
    );

    let (status, body) = admin.delete("/security-groups/web").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "security_group_in_use");
    assert_eq!(
        body["message"],
        "security group \"web\" is used by VM \"vm1\""
    );

    assert_eq!(admin.delete("/vms/vm1").await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        admin.delete("/security-groups/web").await.0,
        StatusCode::NO_CONTENT
    );
}

/// `lan` 브리지 네트워크(노드에 그 브리지와 방화벽이 있다)와 규칙 없는 그룹 `web`을 만든다.
async fn lan_and_web(env: &Env, admin: &Client) {
    admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    admin
        .post("/security-groups", group("web", json!({})))
        .await;
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status.clone();
    status.bridges = vec![BridgeInfo {
        name: "vmbr0".into(),
        vlan_filtering: false,
    }];
    status.firewall = true;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn groups_of_a_running_vm_can_change() {
    let env = Env::start().await;
    let admin = env.admin().await;
    lan_and_web(&env, &admin).await;
    assert_eq!(
        admin.post("/vms", vm_with_groups("vm1", json!([]))).await.0,
        StatusCode::CREATED
    );
    let task = admin.act("vm1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let (_, vm) = admin.get("/vms/vm1").await;
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["security_groups"] = json!(["web"]);
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["nics"][0]["security_groups"], json!(["web"]));
    assert_eq!(body["spec"]["power"], "running");
}

/// 노드가 `hotplug`를 알리지 않게 한다(롤링 업그레이드 중의 옛 규칙).
async fn without_hotplug(env: &Env) {
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.features.retain(|f| f != HOTPLUG);
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();
}

/// `hotplug`를 모르는 노드가 있으면 실행 중인 VM은 방화벽 NIC가 없어도 NIC 개수·순서·MAC을 바꿀 수 없다. 그룹
/// 없이 NIC 순서를 바꾼 뒤 그룹을 거는 두 번의 바꾸기로 규칙의 tap(순번·MAC)이 도는 tap과 어긋나지 않게.
#[tokio::test]
async fn running_vms_keep_the_number_order_and_macs_of_their_nics() {
    let env = Env::start().await;
    let admin = env.admin().await;
    without_hotplug(&env).await;
    lan_and_web(&env, &admin).await;
    let mut req = vm_with_groups("vm1", json!([]));
    req["spec"]["nics"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "network": "lan", "model": "virtio" }));
    assert_eq!(admin.post("/vms", req).await.0, StatusCode::CREATED);
    let task = admin.act("vm1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/vm1").await;
    let nics = vm["spec"]["nics"].as_array().unwrap().clone();

    let mut swapped = vm["spec"].clone();
    swapped["nics"] = json!([nics[1], nics[0]]);
    let mut removed = vm["spec"].clone();
    removed["nics"] = json!([nics[0]]);
    let mut added = vm["spec"].clone();
    added["nics"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "network": "user", "model": "virtio" }));
    let mut mac = vm["spec"].clone();
    mac["nics"][1]["mac"] = json!("52:54:00:aa:bb:cc");
    let mut no_mac = vm["spec"].clone();
    no_mac["nics"][1].as_object_mut().unwrap().remove("mac");
    for (spec, why) in [
        (swapped, "order"),
        (removed, "removed"),
        (added, "added"),
        (mac, "mac"),
        (no_mac, "mac left out"),
    ] {
        let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{why}: {body}");
        assert_eq!(body["code"], "vm_running", "{why}: {body}");
        assert_eq!(
            body["message"],
            "cannot change the number, order or MAC addresses of the NICs of VM \"vm1\" while it is running; stop it first",
            "{why}"
        );
    }
    // 그 뒤에 그룹을 걸어도 NIC는 그대로다.
    let (_, vm) = admin.get("/vms/vm1").await;
    assert_eq!(vm["spec"]["nics"], json!(nics));

    // 도는 하드웨어 기록이 있으므로(마이그레이션 대상이 그 NIC로 켠다) model·network도 바꾸지 않는다.
    let mut spec = vm["spec"].clone();
    spec["nics"][1]["model"] = json!("e1000e");
    spec["nics"][1]["network"] = json!("user");
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["message"],
        "cannot change the NICs of VM \"vm1\" while it is running; only security groups can change"
    );
}

/// `hotplug`를 모르는 노드가 있으면 방화벽 NIC가 있는(바꾸기 전이나 뒤) 실행 중인 VM은 NIC의 그룹만 바꿀 수
/// 있다. 규칙이 NIC 순번·MAC으로 tap을 정하므로 다른 NIC 변경은 도는 tap과 어긋난다.
#[tokio::test]
async fn running_vms_with_groups_change_only_the_groups_of_their_nics() {
    let env = Env::start().await;
    let admin = env.admin().await;
    without_hotplug(&env).await;
    lan_and_web(&env, &admin).await;
    let mut req = vm_with_groups("vm1", json!([]));
    req["spec"]["nics"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "network": "lan", "model": "virtio" }));
    assert_eq!(admin.post("/vms", req).await.0, StatusCode::CREATED);
    let task = admin.act("vm1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // 켠 뒤 첫 NIC에 그룹을 건다.
    let (_, vm) = admin.get("/vms/vm1").await;
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["security_groups"] = json!(["web"]);
    let (status, vm) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{vm}");
    let nics = vm["spec"]["nics"].as_array().unwrap().clone();

    let mut swapped = vm["spec"].clone();
    swapped["nics"] = json!([nics[1], nics[0]]);
    let mut model = vm["spec"].clone();
    model["nics"][1]["model"] = json!("e1000e");
    let mut ungrouped_model = model.clone();
    ungrouped_model["nics"][0]["security_groups"] = json!([]);
    for (spec, why) in [
        (swapped, "order"),
        (model, "model"),
        (ungrouped_model, "groups removed with another change"),
    ] {
        let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{why}: {body}");
        assert_eq!(body["code"], "vm_running", "{why}: {body}");
        assert_eq!(
            body["message"],
            "cannot change the NICs of VM \"vm1\" while it is running; only security groups can change"
        );
    }

    // 그룹만 옮기면 된다.
    let mut moved = vm["spec"].clone();
    moved["nics"][0]["security_groups"] = json!([]);
    moved["nics"][1]["security_groups"] = json!(["web"]);
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": moved }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 방화벽 NIC가 없는 VM도 도는 하드웨어 기록이 있으면(마이그레이션 대상이 그 NIC로 켠다) 실행 중에 NIC의
    // model·network·MAC을 바꾸지 않고, 그룹을 걸면서도 안 된다. 그룹만 걸기는 된다.
    assert_eq!(
        admin.post("/vms", vm_with_groups("vm2", json!([]))).await.0,
        StatusCode::CREATED
    );
    let task = admin.act("vm2", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/vm2").await;
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["model"] = json!("e1000e");
    let (status, body) = admin
        .put("/vms/vm2", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running", "{body}");
    let mut mac = vm["spec"].clone();
    mac["nics"][0]["mac"] = json!("52:54:00:aa:bb:cc");
    let (status, body) = admin.put("/vms/vm2", json!({ "spec": mac }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running", "{body}");
    spec["nics"][0]["security_groups"] = json!(["web"]);
    let (status, body) = admin
        .put("/vms/vm2", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running", "{body}");
    spec["nics"][0]["model"] = json!("virtio");
    let (status, body) = admin.put("/vms/vm2", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 모든 노드가 `hotplug`면 실행 중인 VM의 NIC 그룹만 바꾸기는 핫플러그 Task 없이 곧바로이고, 도는 NIC의 모델·
/// 네트워크 바꾸기는 방화벽이 없어도 409 `vm_running`이다(전에는 다음 시작부터 — 스펙과 도는 장치가 어긋났다).
#[tokio::test]
async fn running_vms_change_groups_without_a_task_and_keep_their_nic_devices() {
    let env = Env::start().await;
    let admin = env.admin().await;
    lan_and_web(&env, &admin).await;
    let mut req = vm_with_groups("vm1", json!([]));
    req["spec"]["nics"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "network": "lan", "model": "virtio" }));
    assert_eq!(admin.post("/vms", req).await.0, StatusCode::CREATED);
    let task = admin.act("vm1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, mut body) = admin.get("/vms/vm1").await;

    // 첫 NIC에 그룹을 걸고, 둘째 NIC로 옮긴다.
    for nic in [0, 1] {
        let mut moved = body["spec"].clone();
        moved["nics"][0]["security_groups"] = json!([]);
        moved["nics"][nic]["security_groups"] = json!(["web"]);
        let (status, task, got) = admin.put_task("/vms/vm1", json!({ "spec": moved })).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(task, None);
        assert_eq!(got["spec"]["nics"][nic]["security_groups"], json!(["web"]));
        body = got;
    }

    for (field, value) in [("model", "e1000e"), ("network", "user")] {
        let mut spec = body["spec"].clone();
        spec["nics"][0][field] = json!(value);
        spec["nics"][0]["security_groups"] = json!([]);
        let (status, task, got) = admin.put_task("/vms/vm1", json!({ "spec": spec })).await;
        assert_eq!(status, StatusCode::CONFLICT, "{field}: {got}");
        assert_eq!(task, None);
        assert_eq!(got["code"], "vm_running", "{field}: {got}");
        assert_eq!(
            got["message"],
            "cannot change NIC 0 of VM \"vm1\" while it is running; only disks and NICs added at the end can be removed while it runs; stop it first",
            "{field}"
        );
    }
}

/// 노드를 정한 VM에 그룹을 걸려면 그 노드에 방화벽(nftables)이 있어야 한다.
#[tokio::test]
async fn pinned_vms_with_groups_need_a_node_with_a_firewall() {
    let env = Env::start().await;
    let admin = env.admin().await;
    lan_and_web(&env, &admin).await;
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status.clone();
    status.firewall = false;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();

    let mut req = vm_with_groups("vm1", json!(["web"]));
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "firewall_unsupported");
    assert_eq!(
        body["message"],
        format!(
            "node {:?} has no firewall (nftables) for security groups",
            node.name
        )
    );
    // 그룹이 없으면 되고, 그 뒤 그룹을 걸 때 같은 검사를 한다.
    let mut plain = vm_with_groups("vm1", json!([]));
    plain["spec"]["node"] = json!(env.node);
    let (status, vm) = admin.post("/vms", plain).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["nics"][0]["security_groups"] = json!(["web"]);
    let (status, body) = admin
        .put("/vms/vm1", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "firewall_unsupported");

    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status.clone();
    status.firewall = true;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (status, body) = admin.put("/vms/vm1", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn security_groups_are_documented() {
    let env = Env::start().await;
    let (_, doc) = env.anonymous().get("/openapi.json").await;
    let paths = &doc["paths"];
    for method in ["get", "post"] {
        assert!(paths["/security-groups"][method].is_object(), "{method}");
    }
    for method in ["get", "patch", "delete"] {
        assert!(
            paths["/security-groups/{key}"][method].is_object(),
            "{method}"
        );
    }
    assert_eq!(
        paths["/security-groups/{key}"]["patch"]["requestBody"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/SecurityGroupPatch"
    );
    // 포트와 CIDR은 문자열로 보인다.
    let rule = &doc["components"]["schemas"]["Rule"]["properties"];
    assert_eq!(rule["ports"]["type"], "string", "{rule}");
    assert_eq!(rule["remote"]["type"], "string", "{rule}");
    assert!(
        doc["components"]["schemas"]["Resource_SecurityGroupSpec_SecurityGroupStatus"].is_object()
    );
}
