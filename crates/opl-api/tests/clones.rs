//! VM 복제 API: 검사, Task 만들기, 복제 중 원본 VM 막기.
//! Task 실행기를 멈춰 두고 API가 남긴 Task의 모양만 확인한다.

mod common;

use common::{Client, Env, vm_request};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, VmPhase, VmSpec, VmStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// Task 실행기 없이 띄운다. 만든 Task는 pending으로 남는다.
async fn env() -> Env {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env
}

/// 남은 Task를 끝난 것으로 기록한다.
async fn finish_tasks(env: &Env) {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let done = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
    };
    for task in tasks {
        env.cluster
            .update_status::<TaskSpec>(task.id, &done)
            .await
            .unwrap();
    }
}

/// 풀(노드 `nodes`)을 저장소에 직접 만든다.
async fn pool(env: &Env, name: &str, nodes: Vec<Uuid>) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes,
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 풀 `local`에 바로 Ready인 qcow2 볼륨을 저장소에 직접 만든다(파일은 만들지 않는다).
async fn volume(env: &Env, name: &str) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: MIB,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    volume.id
}

/// 볼륨을 붙인 꺼진 VM을 만든다.
async fn vm_with_disks(admin: &Client, name: &str, volumes: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = volumes
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// VM의 실제 상태와 그에 맞는 원하는 전원 상태(시작 중·실행 중이면 켜짐)를 직접 쓴다.
async fn set_vm_status(env: &Env, id: Uuid, phase: VmPhase, node: Option<Uuid>) {
    let power = match phase {
        VmPhase::Starting | VmPhase::Running => PowerState::Running,
        _ => PowerState::Stopped,
    };
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    let spec = VmSpec {
        power,
        ..vm.spec.clone()
    };
    env.cluster
        .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
        .await
        .unwrap();
    let status = VmStatus {
        phase,
        node,
        ..vm.status
    };
    env.cluster
        .update_status::<VmSpec>(id, &status)
        .await
        .unwrap();
}

/// 스냅샷을 만들어 Ready로 기록하고 그 id를 돌려준다.
async fn ready_snapshot(env: &Env, admin: &Client, vm: &str, name: &str) -> Uuid {
    let (status, body) = admin
        .post(
            &format!("/vms/{vm}/snapshots"),
            json!({ "name": name, "memory": false }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id: Uuid = body["spec"]["op"]["snapshot"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(1),
        vmstate_bytes: None,
    };
    env.cluster
        .update_status::<SnapshotSpec>(id, &status)
        .await
        .unwrap();
    finish_tasks(env).await;
    id
}

async fn clone(admin: &Client, vm: &str, body: Value) -> (StatusCode, Value) {
    admin.post(&format!("/vms/{vm}/clone"), body).await
}

async fn assert_conflict(admin: &Client, vm: &str, body: Value, code: &str) {
    let (status, got) = clone(admin, vm, body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    assert_eq!(got["code"], code, "{got}");
}

#[tokio::test]
async fn clone_records_a_task_on_the_source_vm() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "fast", vec![env.node]).await;
    volume(&env, "root").await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;

    // 꺼진 VM의 지금 상태에서. 배치되지 않은 VM이면 요청을 받은 노드에서 돈다.
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "vm-clone", "name": "web-2", "snapshot": null, "pool": null })
    );
    assert_eq!(task["spec"]["target"], vm.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
    assert_eq!(task["status"]["state"], "pending");
    finish_tasks(&env).await;

    // 스냅샷(이름이나 id)에서, 다른 풀로
    let snap = ready_snapshot(&env, &admin, "web", "s1").await;
    for key in ["s1".to_string(), snap.to_string()] {
        let (status, task) = clone(
            &admin,
            &vm.to_string(),
            json!({ "name": "web-3", "snapshot": key, "pool": "fast" }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        assert_eq!(
            task["spec"]["op"],
            json!({ "kind": "vm-clone", "name": "web-3", "snapshot": snap, "pool": "fast" })
        );
        finish_tasks(&env).await;
    }
}

#[tokio::test]
async fn clone_validates_request() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "remote", vec![Uuid::now_v7()]).await;
    volume(&env, "root").await;
    vm_with_disks(&admin, "web", &["root"]).await;
    admin.post("/vms", vm_request("db")).await;

    for body in [
        json!({ "name": "Bad_Name" }),
        json!({ "name": "web-2", "pol": "local" }),
        json!({ "name": "web-2", "pool": "missing" }),
    ] {
        let (status, got) = clone(&admin, "web", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    }
    assert_conflict(&admin, "web", json!({ "name": "db" }), "name_taken").await;
    // 풀이 이 VM의 디스크 노드에서 쓸 수 없다.
    assert_conflict(
        &admin,
        "web",
        json!({ "name": "web-2", "pool": "remote" }),
        "pool_not_on_node",
    )
    .await;
    let (status, _) = clone(&admin, "missing", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = clone(
        &admin,
        "web",
        json!({ "name": "web-2", "snapshot": "missing" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 아직 만들고 있는 스냅샷에서는 복제하지 않는다.
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;
    assert_conflict(
        &admin,
        "web",
        json!({ "name": "web-2", "snapshot": "s1" }),
        "snapshot_not_ready",
    )
    .await;

    // 원본 스펙의 볼륨이 없어졌으면 복사할 수 없다.
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<VolumeSpec>(root.id, None)
        .await
        .unwrap();
    let (status, body) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn clone_needs_the_networks_of_the_source_spec() {
    let env = env().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut req = vm_request("web");
    req["spec"]["nics"] = json!([
        { "network": "user", "model": "virtio" },
        { "network": "lan", "model": "virtio" }
    ]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    finish_tasks(&env).await;

    // 원본 스펙의 네트워크가 없어졌으면 새 VM을 만들 수 없다(`user`는 늘 있다).
    let lan = env
        .cluster
        .get_by_name::<opl_types::network::NetworkSpec>("lan")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<opl_types::network::NetworkSpec>(lan.id, None)
        .await
        .unwrap();
    let (status, body) = clone(&admin, "web", json!({ "name": "web-3" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|m| m.contains("network \"lan\" not found")),
        "{body}"
    );
}

#[tokio::test]
async fn running_vm_clones_only_from_a_snapshot() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    volume(&env, "root").await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    ready_snapshot(&env, &admin, "web", "s1").await;
    let elsewhere = Uuid::now_v7();
    set_vm_status(&env, vm, VmPhase::Running, Some(elsewhere)).await;

    assert_conflict(&admin, "web", json!({ "name": "web-2" }), "vm_running").await;
    // 스냅샷에서는 된다. Task는 VM이 도는 노드에서 돈다.
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2", "snapshot": "s1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], elsewhere.to_string());
    finish_tasks(&env).await;

    // 시작·정지 중이거나 전원 Task가 끝나지 않았으면 기다린다.
    for phase in [VmPhase::Starting, VmPhase::Stopping] {
        set_vm_status(&env, vm, phase, Some(elsewhere)).await;
        assert_conflict(
            &admin,
            "web",
            json!({ "name": "web-2", "snapshot": "s1" }),
            "vm_busy",
        )
        .await;
    }
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let (status, task) = admin.post("/vms/web/actions/reboot", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_conflict(
        &admin,
        "web",
        json!({ "name": "web-2", "snapshot": "s1" }),
        "vm_busy",
    )
    .await;
}

#[tokio::test]
async fn unfinished_clone_blocks_power_and_changes_of_the_source() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    volume(&env, "root").await;
    vm_with_disks(&admin, "web", &["root"]).await;
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");

    // 복사하는 동안 켜면 디스크 잠금 때문에 시작이 실패한다.
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    let (_, current) = admin.get("/vms/web").await;
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": current["spec"] }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    finish_tasks(&env).await;
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

#[tokio::test]
async fn cloning_needs_vm_and_volume_write() {
    let env = env().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    admin.post("/vms", vm_request("web")).await;

    let (status, body) = clone(&viewer, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = clone(&operator, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}
