//! VM API: 생성 검증, 스케줄링, 동작과 Task, 오류 응답 형식.

mod common;

use std::time::Duration;

use common::{Env, vm_request};
use opl_driver::DriverError;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{VolumePhase, VolumeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

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
    let (status, _) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT);

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
