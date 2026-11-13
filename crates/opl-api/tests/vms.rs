//! VM API: 생성 검증, 스케줄링, 동작과 Task, 오류 응답 형식.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm_request};
use opl_cluster::ClusterError;
use opl_driver::DriverError;
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{HaConfig, HaGuard};
use opl_types::node::{FAST_HA, Fencing, HA_PLACEMENT, Node, NodeHa, NodeSpec};
use opl_types::placement::{PlacementRule, PlacementRuleSpec, RuleKind};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{VolumePhase, VolumeSpec};
use opl_types::vm::{Arch, PowerState, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn create_assigns_mac_and_validates_spec() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mac = vm["spec"]["nics"][0]["mac"].as_str().unwrap();
    assert!(mac.starts_with("52:54:00:"), "{mac}");
    // 꺼진 채로 만들면 노드를 고르지 않는다.
    assert_eq!(vm["spec"]["node"], Value::Null);
    assert_eq!(vm["spec"]["power"], "stopped");

    let mut bad = vm_request("bad");
    bad["spec"]["memory_mib"] = json!(1);
    let (status, body) = admin.post("/vms", bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");

    let mut typo = vm_request("typo");
    typo["spec"]["memroy_mib"] = json!(512);
    let (status, body) = admin.post("/vms", typo).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");

    let mut net = vm_request("net");
    net["spec"]["nics"][0]["network"] = json!("lan");
    assert_eq!(admin.post("/vms", net).await.0, StatusCode::BAD_REQUEST);

    let mut dup = vm_request("dup");
    dup["spec"]["nics"][0]["mac"] = json!(mac);
    let (status, body) = admin.post("/vms", dup).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "mac_in_use");

    let (status, body) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "name_taken");

    let (status, body) = admin.get("/vms/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn operators_manage_vms_and_viewers_only_read() {
    let env = Env::start().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    let (status, body) = viewer.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(
        operator.post("/vms", vm_request("web")).await.0,
        StatusCode::CREATED
    );
    assert_eq!(viewer.get("/vms/web").await.0, StatusCode::OK);
    let (status, _) = viewer.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let task = operator.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn disks_must_exist_and_belong_to_one_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let with_disk = |name: &str, volume: &str| {
        let mut req = vm_request(name);
        req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
        req
    };
    // 파일이 아직 없는 볼륨은 붙일 수 없다.
    let (status, body) = admin.post("/vms", with_disk("a", "root")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "volume_not_ready");
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    assert_eq!(
        admin.post("/vms", with_disk("a", "nope")).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        admin.post("/vms", with_disk("a", "root")).await.0,
        StatusCode::CREATED
    );
    let (status, body) = admin.post("/vms", with_disk("b", "root")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "volume_in_use");
}

/// 바로 Ready인 볼륨을 클러스터에 직접 만든다(파일은 만들지 않는다).
async fn ready_volume(env: &Env, admin: &common::Client, name: &str) {
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": "local",
                "spec": { "backend": { "kind": "dir", "path": env.dir.path().join("pool") } }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

#[tokio::test]
async fn disks_of_a_running_vm_cannot_change() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_volume(&env, &admin, "root").await;
    let mut req = vm_request("web");
    req["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    assert_eq!(admin.post("/vms", req).await.0, StatusCode::CREATED);
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["disks"] = json!([]);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running");
}

#[tokio::test]
async fn volume_named_import_is_reserved() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_volume(&env, &admin, "root").await;
    let (status, body) = admin
        .post(
            "/volumes",
            json!({
                "name": "import",
                "spec": { "pool": "local", "size_bytes": 1 << 20, "format": "qcow2" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    let (status, body) = admin
        .upload(
            "/volumes/import?name=import&pool=local&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
}

#[tokio::test]
async fn volume_being_created_cannot_be_deleted() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let (status, body) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_not_ready");
}

#[tokio::test]
async fn start_schedules_and_stop_completes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;

    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-start");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["status"]["phase"], "running");
    let id = vm["id"].as_str().unwrap().parse().unwrap();
    assert!(env.pid(id).await.is_some());

    // 실행 중인 VM은 지우거나 옮길 수 없다.
    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "vm_running");
    let mut spec = vm["spec"].clone();
    spec["node"] = Value::Null;
    spec["power"] = json!("stopped");
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "vm_running");

    // Mock 게스트는 ACPI를 무시하므로 acpi_timeout(300ms) 뒤 강제로 꺼진다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(env.pid(id).await, None);
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn force_stop_does_not_wait_for_acpi() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60), |_| async {}).await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    admin.act("web", "start").await;

    let (status, task) = admin
        .post("/vms/web/actions/stop?mode=force", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-force-stop");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let (status, _) = admin
        .post("/vms/web/actions/start?mode=force", Value::Null)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn force_stop_during_reboot_wins() {
    // 게스트가 ACPI를 무시하므로 reboot은 60초 동안 꺼지기를 기다린다.
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60), |_| async {}).await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    admin.act("web", "start").await;
    let (_, reboot) = admin.post("/vms/web/actions/reboot", Value::Null).await;

    let task = admin.act("web", "stop?mode=force").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let reboot = admin.wait_task(&reboot).await;
    assert_eq!(reboot["status"]["state"], "failed");
    assert_eq!(
        reboot["status"]["error"],
        "superseded by a later request for this VM"
    );
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["power"], "stopped");
    assert_eq!(vm["status"]["phase"], "stopped");
}

#[tokio::test]
async fn start_right_after_reboot_leaves_the_vm_running() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    admin.act("web", "start").await;
    // 실행기를 멈춰 두 Task가 함께 시작되게 한다.
    env.stop_runner().await;
    let (_, reboot) = admin.post("/vms/web/actions/reboot", Value::Null).await;
    let (_, start) = admin.post("/vms/web/actions/start", Value::Null).await;
    env.start_runner().await;

    // 켜진 VM의 start는 곧바로 끝난다. 앞의 reboot이 그 뒤에 VM을 끄고 놔두면 안 된다.
    let task = admin.wait_task(&start).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let reboot = admin.wait_task(&reboot).await;
    if reboot["status"]["state"] == "failed" {
        assert_eq!(
            reboot["status"]["error"],
            "superseded by a later request for this VM"
        );
    } else {
        assert_eq!(reboot["status"]["state"], "succeeded", "{reboot}");
    }
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let (_, vm) = admin.get("/vms/web").await;
        assert_eq!(vm["spec"]["power"], "running", "{vm}");
        if vm["status"]["phase"] == "running" {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "VM did not run: {vm}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn reboot_restarts_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    admin.act("web", "start").await;
    let (_, before) = admin.get("/vms/web").await;

    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, after) = admin.get("/vms/web").await;
    assert_eq!(after["spec"]["power"], "running");
    assert_eq!(after["status"]["phase"], "running");
    assert_ne!(after["status"]["pid"], before["status"]["pid"]);

    admin.act("web", "stop").await;
    let (status, body) = admin.post("/vms/web/actions/reboot", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "vm_not_running");
}

#[tokio::test]
async fn failed_start_fails_the_task() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));

    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(
        task["status"]["error"],
        "failed to start: QEMU failed to start: no firmware"
    );
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["power"], "stopped");
}

#[tokio::test]
async fn start_fails_when_no_node_can_run_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    let (_, node) = admin.get(&format!("/nodes/{}", env.node)).await;
    let mut spec = node["spec"].clone();
    spec["maintenance"] = json!(true);
    let (status, _) = admin
        .put(
            &format!("/nodes/{}", env.node),
            json!({ "spec": spec }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "no_node");
}

#[tokio::test]
async fn machine_must_match_the_node_architecture() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node = env.node.to_string();
    // 테스트 노드는 aarch64다. q35(x86_64)를 이 노드에 두면 거절한다.
    let mut x86 = vm_request("x86");
    x86["spec"]["machine"] = json!("q35");
    x86["spec"]["node"] = json!(node);
    let (status, body) = admin.post("/vms", x86.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "machine_arch_mismatch");

    // 고칠 때도 같은 검증이다.
    let mut arm = vm_request("arm");
    arm["spec"]["node"] = json!(node);
    let (status, vm) = admin.post("/vms", arm).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["machine"] = json!("q35");
    let (status, body) = admin.put("/vms/arm", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "machine_arch_mismatch");

    // 노드를 정하지 않은 q35 VM은 만들 수 있지만 맞는 노드가 없어 시작하지 못한다.
    let mut free = vm_request("free");
    free["spec"]["machine"] = json!("q35");
    assert_eq!(admin.post("/vms", free).await.0, StatusCode::CREATED);
    let (status, body) = admin.post("/vms/free/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node");

    // arch를 모르는 노드(옛 버전)는 지정을 막지 않는다.
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.arch = None;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (code, body) = admin.post("/vms", x86).await;
    assert_eq!(code, StatusCode::CREATED, "{body}");

    // 그 노드가 arch를 기록한 뒤 시작하면 지정 노드 검증과 같은 400이다.
    status.arch = Some(Arch::Aarch64);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (code, body) = admin.post("/vms/x86/actions/start", Value::Null).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "machine_arch_mismatch");
}

/// 위트니스는 VM을 돌리지 않는다. VM의 노드로 지정하면 만들 때도 고칠 때도 409다.
#[tokio::test]
async fn a_witness_cannot_hold_vms() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut witness: Node = Resource::new(
        "w",
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: true,
            roles: None,
        },
    );
    witness.status.online = true;
    witness.status.arch = Some(Arch::Aarch64);
    env.cluster.create(&witness).await.unwrap();

    let mut req = vm_request("web");
    req["spec"]["node"] = json!(witness.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert_eq!(
        body["message"],
        "node \"w\" is a witness node and cannot run VMs"
    );

    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["node"] = json!(witness.id);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");

    // 노드를 정하지 않고 켜면 위트니스가 아닌 이 노드로 간다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
}

/// 실행기도 reconciler도 없는 노드를 저장소에만 만든다(온라인, aarch64).
async fn add_node(env: &Env, name: &str) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.status.online = true;
    node.status.memory_mib = 8192;
    node.status.arch = Some(Arch::Aarch64);
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 노드의 유지보수 모드를 바꾼다.
async fn set_maintenance(env: &Env, id: Uuid, maintenance: bool) {
    let mut node = env.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    node.spec.maintenance = maintenance;
    env.cluster
        .update_spec(id, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
}

/// 유지보수 노드에 고정된 꺼진 VM을 켜면 스케줄러가 다른 노드를 고른다.
#[tokio::test]
async fn a_vm_pinned_to_a_maintenance_node_starts_elsewhere() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_node(&env, "node2").await;
    let mut req = vm_request("web");
    req["spec"]["node"] = json!(node2);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    set_maintenance(&env, node2, true).await;

    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["status"]["phase"], "running");
}

/// 다른 노드를 고를 수 없으면 409 `node_unavailable`이고 VM은 그대로다.
#[tokio::test]
async fn start_fails_when_only_the_maintenance_node_can_run_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut req = vm_request("web");
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    set_maintenance(&env, env.node, true).await;

    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert_eq!(
        body["message"],
        "node \"node1\" is in maintenance and no other node can run this VM; end maintenance first"
    );
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["spec"]["power"], "stopped");
}

/// 만들거나 고칠 때 유지보수 노드를 고르면 409 `node_unavailable`이다. 이미 그 노드에 있는 VM의 다른
/// 고치기는 막지 않고, 그 노드에서 도는 VM을 다시 켜도 옮기지 않는다.
#[tokio::test]
async fn a_maintenance_node_cannot_be_chosen() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_node(&env, "node2").await;
    set_maintenance(&env, node2, true).await;

    let mut req = vm_request("db");
    req["spec"]["node"] = json!(node2);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert_eq!(
        body["message"],
        "node \"node2\" is in maintenance and cannot take VMs; end maintenance first"
    );

    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["node"] = json!(node2);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");

    // 이 노드에서 도는 VM: 이 노드가 유지보수가 되어도 고칠 수 있고, start는 옮기지 않는다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    set_maintenance(&env, env.node, true).await;
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["memory_mib"] = json!(512);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["status"]["phase"], "running");

    // 꺼진 뒤에도 그 노드에 고정된 채로 고칠 수 있다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["memory_mib"] = json!(256);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// HA VM은 HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. HA는 멈춘 VM에서만 켤 수 있다(끄기는 언제든).
#[tokio::test]
async fn ha_vms_need_an_ha_capable_node() {
    // reconciler는 `process` 펜싱으로 HA VM을 켠다(리스는 늘 갱신된다).
    struct Renewed;
    #[async_trait]
    impl LeaseRenewer for Renewed {
        async fn renew_lease(&self, _timeout: Duration) -> Result<(), ClusterError> {
            Ok(())
        }
    }
    let env = Env::start_with(
        |c| {
            c.ha = Some(HaGuard::spawn(
                Arc::new(Renewed),
                HaConfig {
                    fence_after: Duration::from_secs(30),
                    renew_interval: Duration::from_secs(1),
                    watchdog: None,
                },
            ))
        },
        |_| async {},
    )
    .await;
    let admin = env.admin().await;
    // 노드를 정하지 않은 HA VM은 만들 수 있지만, HA 가능 노드가 없으면 켤 수 없다.
    let mut req = vm_request("web");
    req["spec"]["ha"] = json!(true);
    let (status, vm) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["ha"], true);
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node");

    // 펜싱이 꺼진 노드에 두면 만들 때도 고칠 때도 409다.
    req["name"] = json!("db");
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_unsupported");
    assert_eq!(
        body["message"],
        "node \"node1\" does not support HA (fencing is off) and cannot run HA VMs"
    );
    let mut spec = vm["spec"].clone();
    spec["node"] = json!(env.node);
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_unsupported");

    // 이 노드에 펜싱을 켜면 스케줄러가 이 노드를 고른다.
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.ha = Some(NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 7,
        fence_after_secs: None,
    });
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());

    // 실행 중인 VM의 HA는 끌 수 있지만 켤 수는 없다. 멈춘 뒤에는 켤 수 있다.
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(false);
    let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["ha"], false);
    let mut spec = body["spec"].clone();
    spec["ha"] = json!(true);
    let (code, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running");
    assert_eq!(
        body["message"],
        "cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped"
    );
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(true);
    let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["ha"], true);
    // 노드의 펜싱이 꺼진 뒤에는 HA로 바꿀 수 없다.
    status.ha = None;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(false);
    let (code, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    spec["ha"] = json!(true);
    let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_unsupported");
}

/// 배치 규칙을 저장소에 바로 만든다(시험 노드는 `ha-placement`를 알리지 않는다).
async fn add_rule(env: &Env, name: &str, kind: RuleKind, vms: &[&str], nodes: &[&str]) {
    let rule: PlacementRule = Resource::new(
        name,
        PlacementRuleSpec {
            kind,
            vms: vms.iter().map(|v| v.to_string()).collect(),
            vm_tag: None,
            nodes: nodes.iter().map(|n| n.to_string()).collect(),
            node_tag: None,
            weight: None,
            enabled: true,
            ha_may_violate: false,
        },
    );
    env.cluster.create(&rule).await.unwrap();
}

/// 409 `placement_rule`과 `details.rules`를 확인한다.
fn assert_placement_rule((status, body): (StatusCode, Value), rules: &[&str]) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "placement_rule", "{body}");
    assert_eq!(body["details"]["rules"], json!(rules), "{body}");
}

/// 켜진 채 만들면 스케줄러가 hard 규칙을 지킨다: anti-affinity 상대가 도는 노드는 메모리가 더 많아도 피한다.
/// soft 규칙은 어길 수밖에 없으면 어기고 만든다.
#[tokio::test]
async fn running_creation_follows_placement_rules() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_node(&env, "node2").await;
    let mut node = env.cluster.get::<NodeSpec>(node2).unwrap().unwrap();
    node.status.memory_mib = 4096;
    env.cluster
        .update_status::<NodeSpec>(node2, &node.status)
        .await
        .unwrap();
    let mut req = vm_request("web");
    req["spec"]["node"] = json!(env.node);
    admin.post("/vms", req).await;
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let running = |name: &str| {
        let mut req = vm_request(name);
        req["spec"]["power"] = json!("running");
        req
    };
    // 규칙이 없으면 미할당 메모리가 많은 이 노드다.
    let (status, vm) = admin.post("/vms", running("free")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["node"], env.node.to_string());

    add_rule(&env, "apart", RuleKind::AntiAffinity, &["web", "db"], &[]).await;
    let (status, vm) = admin.post("/vms", running("db")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["node"], node2.to_string());

    // 갈 곳이 없으면 409 placement_rule. 노드 조건부터 맞지 않으면 지금처럼 no_node다.
    add_rule(
        &env,
        "apart2",
        RuleKind::AntiAffinity,
        &["db", "cache"],
        &[],
    )
    .await;
    add_rule(
        &env,
        "with-web",
        RuleKind::AntiAffinity,
        &["web", "cache"],
        &[],
    )
    .await;
    assert_placement_rule(
        admin.post("/vms", running("cache")).await,
        &["apart2", "with-web"],
    );
    let mut x86 = running("cache");
    x86["spec"]["machine"] = json!("q35");
    let (status, body) = admin.post("/vms", x86).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node");

    add_rule(
        &env,
        "avoid-all",
        RuleKind::SoftNodeAntiAffinity,
        &["soft"],
        &["node1", "node2"],
    )
    .await;
    let (status, vm) = admin.post("/vms", running("soft")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
}

/// `node-affinity` 밖의 노드를 직접 준 켜진 생성·수정은 409다. 꺼진 VM은 그 노드에 고정할 수 있고, 시작하면
/// 규칙에 맞는 노드로 옮겨 켜며, 그런 노드가 없으면 409다. 이미 그 노드에서 도는 VM은 옮기거나 막지 않는다.
#[tokio::test]
async fn a_node_outside_node_affinity_is_refused_or_replaced_at_start() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_node(&env, "node2").await;
    add_rule(&env, "only-n2", RuleKind::NodeAffinity, &["db"], &["node2"]).await;

    let mut req = vm_request("db");
    req["spec"]["node"] = json!(env.node);
    req["spec"]["power"] = json!("running");
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(
        body["message"],
        "VM \"db\" on node \"node1\" breaks placement rules: only-n2"
    );
    assert_placement_rule((status, body), &["only-n2"]);

    // 꺼진 채로는 고정할 수 있다. 켜는 수정은 409다.
    req["spec"]["power"] = json!("stopped");
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["power"] = json!("running");
    assert_placement_rule(
        admin.put("/vms/db", json!({ "spec": spec }), None).await,
        &["only-n2"],
    );

    // 시작은 규칙에 맞는 노드를 고른다(실행기가 없는 노드라 Task는 끝나지 않는다).
    let (status, task) = admin.post("/vms/db/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], node2.to_string());
    let (_, vm) = admin.get("/vms/db").await;
    assert_eq!(vm["spec"]["node"], node2.to_string());

    // 규칙에 맞는 노드가 없으면 시작은 409이고 VM은 그대로다.
    let mut vm = env.cluster.get_by_name::<VmSpec>("db").unwrap().unwrap();
    vm.spec.node = Some(env.node);
    vm.spec.power = PowerState::Stopped;
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    set_maintenance(&env, node2, true).await;
    let (status, body) = admin.post("/vms/db/actions/start", Value::Null).await;
    assert_eq!(
        body["message"],
        "node \"node1\" breaks placement rules of VM \"db\" and no other node can run it: only-n2"
    );
    assert_placement_rule((status, body), &["only-n2"]);
    let (_, vm) = admin.get("/vms/db").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["spec"]["power"], "stopped");

    // 이미 이 노드에서 도는 VM은 나중에 생긴 규칙을 어겨도 고치고 다시 켤 수 있다.
    let mut req = vm_request("web");
    req["spec"]["node"] = json!(env.node);
    admin.post("/vms", req).await;
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    add_rule(&env, "web-n2", RuleKind::NodeAffinity, &["web"], &["node2"]).await;
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["memory_mib"] = json!(512);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
}

/// 기본 HA 정책은 옛 `true`로 저장되지만, 기본과 다른 정책 객체는 모든 노드가 `ha-placement`를 알 때만 받는다.
#[tokio::test]
async fn ha_policy_objects_wait_for_all_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 옛 노드 흉내: 이 노드의 `ha-placement` 표시를 지운다.
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != HA_PLACEMENT);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();

    let mut req = vm_request("web");
    req["spec"]["ha"] = json!({});
    let (status, vm) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["ha"], true);

    req["name"] = json!("db");
    req["spec"]["ha"] = json!({ "state": "stopped", "max_restart": 3 });
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");

    // 범위 밖 횟수는 기능보다 먼저 400이다.
    let mut bad = req.clone();
    bad["spec"]["ha"] = json!({ "max_relocate": 11 });
    let (status, body) = admin.post("/vms", bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "ha.max_restart and ha.max_relocate must be between 0 and 10"
    );

    let mut spec = vm["spec"].clone();
    spec["ha"] = json!({ "state": "ignored" });
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    spec["ha"] = json!(false);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["ha"], false);

    // 모든 노드가 알면 정책 객체를 객체로 저장한다.
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.push(HA_PLACEMENT.into());
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["spec"]["ha"],
        json!({ "state": "stopped", "max_restart": 3, "max_relocate": 1 })
    );
}

/// 우선순위·게스트 감시는 모든 노드가 `fast-ha`를 알 때만 받는다. 범위 밖은 기능보다 먼저 400이다.
#[tokio::test]
async fn priority_and_guest_watch_wait_for_all_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let set_fast_ha = |on: bool| {
        let cluster = env.cluster.clone();
        let node = env.node;
        async move {
            let mut status = cluster.get::<NodeSpec>(node).unwrap().unwrap().status;
            status.features.retain(|f| f != FAST_HA);
            if on {
                status.features.push(FAST_HA.into());
            }
            cluster
                .update_status::<NodeSpec>(node, &status)
                .await
                .unwrap();
        }
    };
    set_fast_ha(false).await;

    let mut req = vm_request("web");
    // 계획 50 모양(그룹 없는 기본이 아닌 정책)은 그대로 받는다.
    req["spec"]["ha"] = json!({ "max_restart": 2, "priority": 50 });
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["spec"]["ha"],
        json!({ "state": "started", "max_restart": 2, "max_relocate": 1 })
    );

    for (name, ha) in [
        ("p", json!({ "priority": 80 })),
        ("g", json!({ "guest_watch": { "timeout_secs": 30 } })),
    ] {
        req["name"] = json!(name);
        req["spec"]["ha"] = ha;
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "cluster_upgrading");
        assert!(
            body["message"].as_str().unwrap().contains("fast ha"),
            "{body}"
        );
    }
    for (ha, message) in [
        (
            json!({ "priority": 101 }),
            "ha.priority must be between 0 and 100",
        ),
        (
            json!({ "guest_watch": { "timeout_secs": 5 } }),
            "ha.guest_watch.timeout_secs must be between 30 and 3600",
        ),
    ] {
        req["spec"]["ha"] = ha;
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["message"], message);
    }

    set_fast_ha(true).await;
    req["name"] = json!("db");
    req["spec"]["ha"] = json!({ "priority": 80, "guest_watch": {} });
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["spec"]["ha"],
        json!({
            "state": "started",
            "max_restart": 1,
            "max_relocate": 1,
            "priority": 80,
            "guest_watch": { "timeout_secs": 60 }
        })
    );
}

/// HA 용량 예약: HA VM을 켜는 요청(켜진 채 만들기·켜기)은 예약을 남겨야 하고(409 `ha_capacity`), HA 아닌 VM과
/// 이미 켜진 HA VM의 다른 고치기는 막지 않는다.
#[tokio::test]
async fn ha_vms_start_only_within_the_ha_reserve() {
    struct Renewed;
    #[async_trait]
    impl LeaseRenewer for Renewed {
        async fn renew_lease(&self, _timeout: Duration) -> Result<(), ClusterError> {
            Ok(())
        }
    }
    let env = Env::start_with(
        |c| {
            c.ha = Some(HaGuard::spawn(
                Arc::new(Renewed),
                HaConfig {
                    fence_after: Duration::from_secs(30),
                    renew_interval: Duration::from_secs(1),
                    watchdog: None,
                },
            ))
        },
        |_| async {},
    )
    .await;
    let admin = env.admin().await;
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.ha = Some(NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 12,
        fence_after_secs: Some(10),
    });
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    // 노드 8192 MiB의 절반을 비워 둔다.
    let (code, body) = admin
        .patch(
            "/settings",
            json!({ "ha_reserve": { "percent": 50 } }),
            None,
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");

    let ha_vm = |name: &str, memory: u32, running: bool| {
        let mut req = vm_request(name);
        req["spec"]["ha"] = json!(true);
        req["spec"]["memory_mib"] = json!(memory);
        if running {
            req["spec"]["power"] = json!("running");
        }
        req
    };
    let (code, body) = admin.post("/vms", ha_vm("a", 4096, false)).await;
    assert_eq!(code, StatusCode::CREATED, "{body}");
    let task = admin.act("a", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let details = json!({
        "capacity_mib": 8192,
        "reserve_mib": 4096,
        "used_mib": 4096,
        "need_mib": 256
    });
    let (code, body) = admin.post("/vms", ha_vm("b", 256, true)).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_capacity");
    assert_eq!(body["details"], details);
    let (code, body) = admin.post("/vms", ha_vm("b", 256, false)).await;
    assert_eq!(code, StatusCode::CREATED, "{body}");
    let (code, body) = admin.post("/vms/b/actions/start", Value::Null).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_capacity");
    assert_eq!(body["details"], details);
    assert_eq!(
        body["message"],
        "starting HA VM \"b\" (256 MiB) would use capacity reserved for HA recovery: 0 MiB left of 8192 MiB (4096 MiB reserved, 4096 MiB used)"
    );
    let (_, vm) = admin.get("/vms/b").await;
    assert_eq!(vm["spec"]["power"], "stopped");

    // HA 아닌 VM은 예약을 보지 않지만 자리는 차지한다.
    let mut plain = vm_request("c");
    plain["spec"]["power"] = json!("running");
    let (code, body) = admin.post("/vms", plain).await;
    assert_eq!(code, StatusCode::CREATED, "{body}");
    // 이미 켜진 HA VM의 다른 고치기는 넘친 뒤에도 된다. 메모리를 늘리면 예약을 본다.
    let (_, vm) = admin.get("/vms/a").await;
    let mut spec = vm["spec"].clone();
    spec["restart"] = json!("on-failure");
    let (code, body) = admin
        .put("/vms/a", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    spec["memory_mib"] = json!(4352);
    let (code, body) = admin.put("/vms/a", json!({ "spec": spec }), None).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_capacity");
    // 이미 켜기로 된 VM의 start는 다시 보지 않는다.
    let task = admin.act("a", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    // 예약을 지우면 켤 수 있다.
    let (code, body) = admin
        .patch("/settings", json!({ "ha_reserve": "none" }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let task = admin.act("b", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn update_checks_if_match_revision() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, vm) = admin.post("/vms", vm_request("web")).await;
    let revision = vm["revision"].as_u64().unwrap();
    let mut spec = vm["spec"].clone();
    spec["memory_mib"] = json!(512);

    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec }), Some(revision + 100))
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(body["code"], "revision_mismatch");
    // revision은 spec 검증보다 먼저 확인한다.
    let mut bad = spec.clone();
    bad["memory_mib"] = json!(1);
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": bad }), Some(revision + 100))
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");

    let (status, updated) = admin
        .put(
            "/vms/web",
            json!({ "spec": spec, "labels": { "tier": "web" } }),
            Some(revision),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["spec"]["memory_mib"], 512);
    assert_eq!(updated["labels"]["tier"], "web");
    // 기존 NIC의 MAC은 그대로 둔다.
    assert_eq!(updated["spec"]["nics"], vm["spec"]["nics"]);
}

#[tokio::test]
async fn list_returns_revision_and_items() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("a")).await;
    admin.post("/vms", vm_request("b")).await;
    let (status, list) = admin.get("/vms").await;
    assert_eq!(status, StatusCode::OK);
    assert!(list["revision"].as_u64().unwrap() > 0);
    let mut names: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["a", "b"]);
}
