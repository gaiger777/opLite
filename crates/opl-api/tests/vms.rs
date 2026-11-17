//! VM API: 생성 검증, 스케줄링, 동작과 Task, 오류 응답 형식.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm_request};
use opl_cluster::ClusterError;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_driver::DriverError;
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{HaConfig, HaGuard};
use opl_types::node::{
    FAST_HA, FEATURES, Fencing, HA_PLACEMENT, HOTPLUG, NODE_REPLICATION, Node, NodeHa, NodeSpec,
    NodeStatus, PLACEMENT,
};
use opl_types::placement::{LoadSummary, PlacementRule, PlacementRuleSpec, RuleKind};
use opl_types::replication::ReplicaStatus;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::SnapshotSpec;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeContent, VolumePhase, VolumeSpec};
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
            content: Default::default(),
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
            content: Default::default(),
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    volume.status.node = Some(env.node);
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
            content: Default::default(),
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

/// 노드 `node`의 이름으로 리더(이 노드)에 리스를 갱신하며 CPU 사용률 `permille`‰의 부하 요약을 싣는다.
async fn renew_load(env: &Env, node: Uuid, permille: u16) {
    let load = LoadSummary {
        cpu1: Some(permille),
        cpu5: Some(permille),
        mem: Some(1024),
        vms: vec![],
    };
    env.cluster
        .execute_as_leader(LeaderRequest::RenewLease {
            node: NodeId(node),
            checks: vec![],
            load: Some(load),
        })
        .await
        .unwrap();
}

/// `spec.node` 없는 VM을 켜면 메모리가 같은 노드 가운데 리더가 아는 CPU 부하가 낮은 노드를 고른다(부하를
/// 모르면 id가 가장 작은 이 노드다).
#[tokio::test]
async fn start_picks_the_less_busy_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let a = add_node(&env, "node2").await;
    let b = add_node(&env, "node3").await;
    let (busy, idle) = (a.min(b), a.max(b));
    renew_load(&env, env.node, 900).await;
    renew_load(&env, busy, 900).await;
    renew_load(&env, idle, 100).await;

    // 그 노드에는 실행기가 없어 Task는 끝나지 않는다.
    let (status, task) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], idle.to_string());
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], idle.to_string());
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

/// VM 고정(`placement.pinned`)은 저장하고(실행 중에도 바꿈), 고정하지 않음은 적지 않으며, `placement`를 모르는 노드가
/// 있으면 409다.
#[tokio::test]
async fn pinned_vms_are_stored_only_when_pinned_and_wait_for_all_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut req = vm_request("web");
    req["spec"]["placement"] = json!({ "pinned": true });
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["placement"], json!({ "pinned": true }));
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    assert!(
        env.cluster
            .get::<VmSpec>(id)
            .unwrap()
            .unwrap()
            .spec
            .pinned()
    );
    // `pinned: false`와 `{}`은 적지 않는다.
    for (name, placement) in [("a", json!({ "pinned": false })), ("b", json!({}))] {
        req["name"] = json!(name);
        req["spec"]["placement"] = placement;
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body["spec"].get("placement").is_none(), "{body}");
    }
    // 실행 중에도 풀고 다시 고정한다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["placement"] = json!({ "pinned": false });
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("placement").is_none(), "{body}");
    assert_eq!(
        env.cluster
            .get::<VmSpec>(id)
            .unwrap()
            .unwrap()
            .spec
            .placement,
        None
    );

    // 옛 노드 흉내: `placement`를 모르면 고정을 받지 않는다(고정하지 않음은 받는다).
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != PLACEMENT);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    spec["placement"] = json!({ "pinned": true });
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert!(
        body["message"].as_str().unwrap().contains("placement"),
        "{body}"
    );
    spec["placement"] = json!({ "pinned": false });
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 핫플러그 상한은 준 그대로 저장하고(지금 값과 같아도), 범위 밖은 400, `hotplug`를 모르는 노드가 있으면 409다.
#[tokio::test]
async fn hotplug_maximums_are_stored_as_given_and_wait_for_all_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut req = vm_request("web");
    req["spec"]["cpu"] = json!({ "cores": 2, "max_vcpus": 8 });
    req["spec"]["max_memory_mib"] = json!(1024);
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["cpu"]["max_vcpus"], 8);
    assert_eq!(body["spec"]["max_memory_mib"], 1024);
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let stored = env.cluster.get::<VmSpec>(id).unwrap().unwrap().spec;
    assert_eq!(
        (stored.cpu.max_vcpus, stored.max_memory_mib),
        (Some(8), Some(1024))
    );

    // 지금 값과 같은 상한도 적는다(상한까지 늘린 VM이 상한을 잃지 않게).
    req["name"] = json!("same");
    req["spec"]["cpu"] = json!({ "sockets": 2, "cores": 2, "max_vcpus": 4 });
    req["spec"]["max_memory_mib"] = json!(256);
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["cpu"]["max_vcpus"], 4);
    assert_eq!(body["spec"]["max_memory_mib"], 256);

    // 상한을 적지 않으면 옛 VM 모양 그대로다.
    req["name"] = json!("none");
    req["spec"]["cpu"] = json!({ "cores": 2 });
    req["spec"]["max_memory_mib"] = Value::Null;
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["spec"]["cpu"].get("max_vcpus").is_none(), "{body}");
    assert!(body["spec"].get("max_memory_mib").is_none(), "{body}");

    req["name"] = json!("bad");
    for (cpu, memory, max_memory, message) in [
        (
            json!({ "cores": 4, "max_vcpus": 2 }),
            256,
            Value::Null,
            "cpu.max_vcpus must be at least cpu.sockets × cpu.cores (4)",
        ),
        (
            json!({ "sockets": 2, "cores": 1, "max_vcpus": 3 }),
            256,
            Value::Null,
            "cpu.max_vcpus must be a multiple of cpu.sockets",
        ),
        (
            json!({ "cores": 1, "max_vcpus": 257 }),
            256,
            Value::Null,
            "cpu.max_vcpus must be at most 256",
        ),
        (
            json!({ "cores": 1 }),
            512,
            json!(384),
            "max_memory_mib must be at least memory_mib",
        ),
        (
            json!({ "cores": 1 }),
            256,
            json!(524_416),
            "max_memory_mib must be at most 524288",
        ),
        (
            json!({ "cores": 1 }),
            300,
            json!(1024),
            "memory_mib and max_memory_mib must be multiples of 128 when max_memory_mib is set",
        ),
        (
            json!({ "cores": 1 }),
            300,
            json!(300),
            "memory_mib and max_memory_mib must be multiples of 128 when max_memory_mib is set",
        ),
    ] {
        req["spec"]["cpu"] = cpu;
        req["spec"]["memory_mib"] = json!(memory);
        req["spec"]["max_memory_mib"] = max_memory;
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["message"], message);
    }

    // 옛 노드 흉내: `hotplug`를 모르면 상한을 받지 않는다(같은 값도).
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != HOTPLUG);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    req["spec"]["memory_mib"] = json!(256);
    for (cpu, max_memory) in [
        (json!({ "cores": 1, "max_vcpus": 2 }), Value::Null),
        (json!({ "cores": 1 }), json!(512)),
        (json!({ "cores": 1, "max_vcpus": 1 }), Value::Null),
    ] {
        req["spec"]["cpu"] = cpu;
        req["spec"]["max_memory_mib"] = max_memory;
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "cluster_upgrading");
        assert!(
            body["message"].as_str().unwrap().contains("hotplug"),
            "{body}"
        );
    }
    req["spec"]["cpu"] = json!({ "cores": 1 });
    req["spec"]["max_memory_mib"] = Value::Null;
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
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

/// 게스트 감시(QGA 장치)는 멈춘 VM에서만 켜고 끈다(실행 중에 바꾸면 라이브 마이그레이션이 장치 차이로 실패한다).
/// 시간만 바꾸기는 실행 중에도 된다.
#[tokio::test]
async fn guest_watch_is_turned_on_or_off_only_while_stopped() {
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
    let mut req = vm_request("web");
    req["spec"]["ha"] = json!({ "guest_watch": { "timeout_secs": 60 } });
    let (code, body) = admin.post("/vms", req).await;
    assert_eq!(code, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"]["guest_watch"]["timeout_secs"] = json!(120);
    let (code, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    // 감시를 끄는 것도, HA를 통째로 끄는 것도 장치를 바꾼다.
    for ha in [json!(true), json!(false)] {
        spec["ha"] = ha;
        let (code, body) = admin
            .put("/vms/web", json!({ "spec": spec.clone() }), None)
            .await;
        assert_eq!(code, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "vm_running");
        assert_eq!(
            body["message"],
            "cannot turn guest_watch on or off for VM \"web\" while it is running; guest_watch can be turned on or off only while the VM is stopped"
        );
    }
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(true);
    let (code, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    // 켜진 VM에 감시를 켜는 것도 막는다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!({ "guest_watch": {} });
    let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running");
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

/// 모든 기능을 알리는 노드(실행기 없음)를 저장소에만 만든다. `witness`면 위트니스다.
async fn add_full_node(env: &Env, name: &str, witness: bool) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness,
            roles: None,
        },
    );
    node.status.online = true;
    node.status.memory_mib = 8192;
    node.status.arch = Some(Arch::Aarch64);
    node.status.features = FEATURES.iter().map(|f| f.to_string()).collect();
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 디렉터리 풀 `name`(공유 여부 `shared`, 노드 목록 `nodes`)을 저장소에 만든다.
async fn dir_pool(env: &Env, name: &str, shared: bool, nodes: Vec<Uuid>) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes,
            shared,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 풀 `pool`에 이 노드에 있는 Ready 볼륨 `name`을 저장소에 만든다(파일 없음).
async fn volume_in(env: &Env, name: &str, pool: &str) {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
            content: Default::default(),
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    volume.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

/// 볼륨 `volume`을 붙이고 복제 대상 `replication`을 둔 VM 요청
fn replicated_request(name: &str, volume: &str, replication: Value) -> Value {
    let mut req = vm_request(name);
    req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
    req["spec"]["replication"] = replication;
    req
}

fn assert_bad_request((status, body): (StatusCode, Value), message: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], message, "{body}");
}

/// 복제 대상(계획 55)은 클러스터에 맞아야 한다: 대상 노드가 있고 위트니스가 아니며, VM에 노드 로컬 풀의 디스크가
/// 있고, 대상 노드에서 대상 풀을 쓸 수 있다. 자기 노드(정한 노드나 스케줄러가 고른 노드)는 대상이 될 수 없다.
#[tokio::test]
async fn replication_targets_must_fit_the_cluster() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_full_node(&env, "node2", false).await;
    let witness = add_full_node(&env, "w", true).await;
    // a는 이 노드만, b는 node2만 쓰는 로컬 풀이다.
    dir_pool(&env, "a", false, vec![env.node]).await;
    dir_pool(&env, "b", false, vec![node2]).await;
    dir_pool(&env, "shared", true, vec![]).await;
    volume_in(&env, "root", "a").await;
    volume_in(&env, "nfs-root", "shared").await;

    let missing = Uuid::now_v7();
    assert_bad_request(
        admin
            .post(
                "/vms",
                replicated_request("web", "root", json!([{ "node": missing }])),
            )
            .await,
        &format!("replication target node {missing} not found"),
    );
    assert_bad_request(
        admin
            .post(
                "/vms",
                replicated_request("web", "root", json!([{ "node": witness }])),
            )
            .await,
        "node \"w\" is a witness and cannot hold replicas",
    );
    let mut own = replicated_request("web", "root", json!([{ "node": env.node }]));
    own["spec"]["node"] = json!(env.node);
    assert_bad_request(
        admin.post("/vms", own).await,
        "replication target cannot be the VM's own node",
    );
    // 공유 풀의 디스크는 복제하지 않는다.
    assert_bad_request(
        admin
            .post(
                "/vms",
                replicated_request("web", "nfs-root", json!([{ "node": node2 }])),
            )
            .await,
        "VM \"web\" has no disks on node-local pools; replication applies only to them",
    );
    let mut diskless = vm_request("web");
    diskless["spec"]["replication"] = json!([{ "node": node2 }]);
    assert_bad_request(
        admin.post("/vms", diskless).await,
        "VM \"web\" has no disks on node-local pools; replication applies only to them",
    );
    // node2는 같은 이름의 풀 a를 쓸 수 없다. 풀 b를 주면 된다.
    assert_bad_request(
        admin
            .post(
                "/vms",
                replicated_request("web", "root", json!([{ "node": node2 }])),
            )
            .await,
        "node \"node2\" cannot use storage pool \"a\" for the replica of disk \"root\"",
    );
    assert_bad_request(
        admin
            .post(
                "/vms",
                replicated_request("web", "root", json!([{ "node": node2, "pool": "nope" }])),
            )
            .await,
        "node \"node2\" cannot use storage pool \"nope\" for the replica of disk \"root\"",
    );
    // 복제본은 대상 노드만의 풀에 둔다(공유 풀이면 원본과 같은 볼륨 id가 같은 곳을 가리킨다).
    assert_bad_request(
        admin
            .post(
                "/vms",
                replicated_request("web", "root", json!([{ "node": node2, "pool": "shared" }])),
            )
            .await,
        "replica pool \"shared\" is shared; replication needs a node-local pool",
    );
    // 켜진 채 만들면 스케줄러가 디스크가 있는 이 노드를 고른다: 그 노드가 대상이면 안 된다.
    let mut scheduled = replicated_request("web", "root", json!([{ "node": env.node }]));
    scheduled["spec"]["power"] = json!("running");
    assert_bad_request(
        admin.post("/vms", scheduled).await,
        "replication target cannot be the VM's own node",
    );

    let (status, body) = admin
        .post(
            "/vms",
            replicated_request(
                "web",
                "root",
                json!([{ "node": node2, "schedule": "*/5", "pool": "b" }]),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["spec"]["replication"],
        json!([{ "node": node2, "schedule": "*/5", "pool": "b" }])
    );
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let stored = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    assert_eq!(stored.spec.replication[0].pool.as_deref(), Some("b"));
    // 순수 검증의 400이 먼저다.
    let mut spec = body["spec"].clone();
    spec["replication"] = json!([{ "node": node2, "schedule": "15" }]);
    assert_bad_request(
        admin.put("/vms/web", json!({ "spec": spec }), None).await,
        "replication schedule must look like */N with N minutes from 1 to 1440",
    );
}

/// 저장된 복제 대상은 다시 보지 않아 대상 노드가 사라져도 VM의 다른 고치기는 된다(바꾼 대상은 본다). 죽은 노드를
/// 지우면 VM들의 복제 대상과 상태에서 그 노드가 빠진다.
#[tokio::test]
async fn a_removed_target_node_does_not_block_vm_changes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_full_node(&env, "node2", false).await;
    let node3 = add_full_node(&env, "node3", false).await;
    dir_pool(&env, "a", false, vec![env.node]).await;
    dir_pool(&env, "b", false, vec![node2, node3]).await;
    volume_in(&env, "root", "a").await;
    let targets = json!([{ "node": node2, "pool": "b" }, { "node": node3, "pool": "b" }]);
    let (status, body) = admin
        .post("/vms", replicated_request("web", "root", targets.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let mut vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    vm.status.replication = vec![ReplicaStatus::pending(node2), ReplicaStatus::pending(node3)];
    env.cluster
        .update_status::<VmSpec>(id, &vm.status)
        .await
        .unwrap();

    // node2가 저장소에서 사라졌다(지우기가 대상을 고치기 전에 끊김 등).
    env.cluster.delete::<NodeSpec>(node2, None).await.unwrap();
    let mut spec = body["spec"].clone();
    spec["memory_mib"] = json!(1024);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    spec["replication"] = json!([{ "node": node2, "pool": "a" }, { "node": node3, "pool": "b" }]);
    assert_bad_request(
        admin.put("/vms/web", json!({ "spec": spec }), None).await,
        &format!("replication target node {node2} not found"),
    );

    // 죽은 node3을 지우면 대상과 상태에서 빠진다.
    env.cluster
        .update_status::<NodeSpec>(node3, &NodeStatus::default())
        .await
        .unwrap();
    let (status, body) = admin.delete("/nodes/node3").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    assert_eq!(vm.spec.replication.len(), 1);
    assert_eq!(vm.spec.replication[0].node, node2);
    assert_eq!(vm.spec.memory_mib, 1024);
    let nodes: Vec<Uuid> = vm.status.replication.iter().map(|r| r.node).collect();
    assert_eq!(nodes, [node2]);
}

/// 복제 대상은 모든 노드가 `node-replication`을 알 때만 받는다. 범위 밖은 기능보다 먼저 400이다.
#[tokio::test]
async fn replication_waits_for_all_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_full_node(&env, "node2", false).await;
    dir_pool(&env, "local", false, vec![]).await;
    volume_in(&env, "root", "local").await;
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != NODE_REPLICATION);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();

    let req = replicated_request("web", "root", json!([{ "node": node2 }]));
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("node replication"),
        "{body}"
    );
    let mut bad = req.clone();
    bad["spec"]["replication"] = json!([{ "node": node2, "schedule": "*/0" }]);
    assert_eq!(admin.post("/vms", bad).await.0, StatusCode::BAD_REQUEST);
    // 복제가 없으면 그대로 받는다.
    let (status, body) = admin
        .post("/vms", replicated_request("web", "root", json!([])))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["spec"].get("replication").is_none(), "{body}");
}

/// 스냅샷(또는 예약 스냅샷)이 있는 VM은 복제할 수 없고(409 `replication_unsupported`), 복제하는 VM은 스냅샷을 만들
/// 수 없다(409 `snapshot_unsupported`).
#[tokio::test]
async fn replication_and_snapshots_exclude_each_other() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_full_node(&env, "node2", false).await;
    dir_pool(&env, "local", false, vec![]).await;
    volume_in(&env, "root", "local").await;
    volume_in(&env, "data", "local").await;

    let (status, vm) = admin
        .post("/vms", replicated_request("web", "root", json!([])))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let stored = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    let snapshot = Resource::new(
        "before",
        SnapshotSpec {
            vm: id,
            name: "before".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: stored.spec.clone(),
            hardware: None,
        },
    );
    env.cluster.create(&snapshot).await.unwrap();
    let mut spec = vm["spec"].clone();
    spec["replication"] = json!([{ "node": node2 }]);
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "replication_unsupported");
    assert_eq!(
        body["message"],
        "VM \"web\" has snapshots (or a snapshot schedule); replicas do not keep snapshots, delete them first"
    );

    // 예약 스냅샷도 막는다.
    let mut req = replicated_request("db", "data", json!([{ "node": node2 }]));
    req["spec"]["snapshot_schedule"] = json!({ "every": "hourly", "keep": 2 });
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "replication_unsupported");

    req["spec"]["snapshot_schedule"] = Value::Null;
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post("/vms/db/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");
    assert_eq!(
        body["message"],
        "VM \"db\" is replicated to other nodes; replicas do not keep snapshots, remove the replication first"
    );
}

/// 풀 `pool`에 이 노드에 있는 ISO 볼륨 `name`(raw, 단계 `phase`)을 저장소에 만든다(파일 없음).
async fn iso_in(env: &Env, name: &str, pool: &str, phase: VolumePhase) {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
            content: VolumeContent::Iso,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = phase;
    volume.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

/// CD-ROM 드라이브 `cdroms`를 둔 VM 요청
fn cdrom_request(name: &str, cdroms: Value) -> Value {
    let mut req = vm_request(name);
    req["spec"]["cdroms"] = cdroms;
    req
}

/// CD 매체는 준비된 ISO 볼륨이어야 하고, 읽기 전용이라 여러 VM이 같은 ISO를 넣는다. ISO는 디스크로 붙이지 못하고
/// 디스크 볼륨은 CD에 넣지 못한다. HA·복제 VM은 공유 풀의 ISO만 쓰고, 지정한 노드는 그 ISO를 읽을 수 있어야 한다.
#[tokio::test]
async fn cdrom_media_must_be_ready_isos_shared_by_vms() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local", false, vec![]).await;
    dir_pool(&env, "nas", true, vec![]).await;
    env.pool_ready("local").await;
    env.pool_ready("nas").await;
    iso_in(&env, "ubuntu", "local", VolumePhase::Ready).await;
    iso_in(&env, "shared-iso", "nas", VolumePhase::Ready).await;
    iso_in(&env, "uploading", "local", VolumePhase::Pending).await;
    volume_in(&env, "root", "local").await;

    let (status, a) = admin
        .post("/vms", cdrom_request("a", json!([{ "iso": "ubuntu" }, {}])))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{a}");
    assert_eq!(a["spec"]["cdroms"], json!([{ "iso": "ubuntu" }, {}]));
    let (status, b) = admin
        .post("/vms", cdrom_request("b", json!([{ "iso": "ubuntu" }])))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");

    let mut req = vm_request("c");
    req["spec"]["disks"] = json!([{ "volume": "ubuntu", "bus": "virtio-blk" }]);
    assert_bad_request(
        admin.post("/vms", req).await,
        "volume \"ubuntu\" is an ISO image; attach it as a CD-ROM",
    );
    assert_bad_request(
        admin
            .post("/vms", cdrom_request("c", json!([{ "iso": "root" }])))
            .await,
        "volume \"root\" is not an ISO image",
    );
    assert_bad_request(
        admin
            .post("/vms", cdrom_request("c", json!([{ "iso": "nope" }])))
            .await,
        "volume \"nope\" not found",
    );
    let (status, body) = admin
        .post("/vms", cdrom_request("c", json!([{ "iso": "uploading" }])))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_not_ready");

    // HA VM은 공유 풀의 ISO만(빈 드라이브는 된다).
    let mut req = cdrom_request("c", json!([{ "iso": "ubuntu" }]));
    req["spec"]["ha"] = json!(true);
    let local_iso = "HA and replicated VMs can use ISOs only from shared pools; ISO \"ubuntu\" is in node-local pool \"local\"";
    assert_bad_request(admin.post("/vms", req.clone()).await, local_iso);
    req["spec"]["cdroms"] = json!([{ "iso": "shared-iso" }, {}]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 복제 VM도 같다(복제본은 다른 노드에서 켠다).
    let mut req = replicated_request("d", "root", json!([{ "node": Uuid::now_v7() }]));
    req["spec"]["cdroms"] = json!([{ "iso": "ubuntu" }]);
    assert_bad_request(admin.post("/vms", req).await, local_iso);

    // 지정한 노드는 노드 로컬 풀의 ISO를 그 파일이 있는 노드에서만 읽는다.
    add_full_node(&env, "node2", false).await;
    let mut req = cdrom_request("e", json!([{ "iso": "ubuntu" }]));
    let node2 = env
        .cluster
        .get_by_name::<NodeSpec>("node2")
        .unwrap()
        .unwrap();
    req["spec"]["node"] = json!(node2.id);
    assert_bad_request(
        admin.post("/vms", req.clone()).await,
        "node \"node2\" cannot use ISO \"ubuntu\"",
    );
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// 지우기 Task가 끝나지 않은 ISO는 CD에 넣지 못한다(409 `volume_in_use` — 지우기 API의 확인 뒤 넣으면 Task가 그 ISO를
/// 지울 수 있다). Task가 끝나면 된다.
#[tokio::test]
async fn an_iso_being_deleted_cannot_be_inserted() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "nas", true, vec![]).await;
    env.pool_ready("nas").await;
    iso_in(&env, "ubuntu", "nas", VolumePhase::Ready).await;
    let iso = env
        .cluster
        .get_by_name::<VolumeSpec>("ubuntu")
        .unwrap()
        .unwrap();
    // 다른 노드의 Task라 이 실행기가 돌리지 않는다.
    let mut task: opl_types::task::Task = Resource::new(
        "",
        opl_types::task::TaskSpec {
            op: opl_types::task::TaskOp::VolumeDelete,
            target: iso.id,
            node: Uuid::now_v7(),
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();

    let (status, body) = admin
        .post("/vms", cdrom_request("a", json!([{ "iso": "ubuntu" }])))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_use");
    assert_eq!(
        body["message"],
        format!("ISO \"ubuntu\" is being deleted (task {})", task.id)
    );
    // 빈 드라이브는 된다.
    let (status, body) = admin.post("/vms", cdrom_request("a", json!([{}]))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post("/vms/a/cdrom", json!({ "drive": 0, "iso": "ubuntu" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_use");

    task.status.state = opl_types::task::TaskState::Failed;
    env.cluster
        .update_status::<opl_types::task::TaskSpec>(task.id, &task.status)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/a/cdrom", json!({ "drive": 0, "iso": "ubuntu" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 스케줄러는 노드 로컬 풀의 ISO가 있는 노드를 고른다(메모리가 더 많은 노드가 있어도). 공유 풀의 ISO는 묶지 않는다.
#[tokio::test]
async fn the_scheduler_keeps_a_local_iso_on_its_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_full_node(&env, "node2", false).await;
    let mut n = env.cluster.get::<NodeSpec>(node2).unwrap().unwrap();
    n.status.memory_mib = 65536;
    env.cluster
        .update_status::<NodeSpec>(node2, &n.status)
        .await
        .unwrap();
    dir_pool(&env, "local", false, vec![]).await;
    dir_pool(&env, "nas", true, vec![]).await;
    for pool in ["local", "nas"] {
        env.pool_ready(pool).await;
        common::mark_pool_ready(&env.cluster, node2, pool).await;
    }
    iso_in(&env, "ubuntu", "local", VolumePhase::Ready).await;
    iso_in(&env, "shared-iso", "nas", VolumePhase::Ready).await;

    let mut req = cdrom_request("a", json!([{ "iso": "ubuntu" }]));
    req["spec"]["power"] = json!("running");
    let (status, a) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{a}");
    assert_eq!(a["spec"]["node"], env.node.to_string());
    req["name"] = json!("b");
    req["spec"]["cdroms"] = json!([{ "iso": "shared-iso" }]);
    let (status, b) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    assert_eq!(b["spec"]["node"], node2.to_string());
}
