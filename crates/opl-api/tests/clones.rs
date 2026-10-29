//! VM 복제: API의 검사, Task 만들기, 복제 중 원본 VM 막기(실행기를 멈춰 두고 Task의 모양만 본다)와
//! 파일 없는 스토리지로 돌리는 복제 Task 실행(새 VM과 볼륨, 실패 정리, 원본 VM의 줄).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage, vm_request};
use opl_driver::uefi_vars_path;
use opl_types::node::PoolState;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
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
    env.pool_ready(name).await;
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
    // 노드 목록에는 있지만 그 노드에서 준비되지 않았다.
    pool(&env, "down", vec![]).await;
    common::set_pool_state(&env.cluster, env.node, "down", Default::default()).await;
    assert_conflict(
        &admin,
        "web",
        json!({ "name": "web-2", "pool": "down" }),
        "pool_unavailable",
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

#[tokio::test]
async fn unfinished_clone_blocks_deleting_the_source() {
    let env = env().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");

    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    finish_tasks(&env).await;
    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// 파일 없는 스토리지로 실행기를 다시 띄운다. `volumes`는 원래 크기로 알린다(나머지는 파일이 없다).
async fn fake_storage(env: &mut Env, volumes: &[Uuid]) -> Arc<FakeStorage> {
    let storage = Arc::new(FakeStorage::default());
    for id in volumes {
        storage.set_size(*id, MIB);
    }
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    storage
}

fn get_vm(env: &Env, name: &str) -> Option<Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

fn get_volume(env: &Env, name: &str) -> Option<opl_types::storage::Volume> {
    env.cluster.get_by_name::<VolumeSpec>(name).unwrap()
}

fn state_dir(env: &Env) -> std::path::PathBuf {
    env.dir.path().join("state")
}

/// VM 상태 디렉터리가 있는 VM들
fn vm_dirs(env: &Env) -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(state_dir(env).join("vm"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// 이름이 `prefix`로 시작하는 볼륨들
fn volumes_named(env: &Env, prefix: &str) -> Vec<String> {
    let (_, volumes) = env.cluster.list::<VolumeSpec>().unwrap();
    volumes
        .into_iter()
        .map(|v| v.name)
        .filter(|n| n.starts_with(prefix))
        .collect()
}

async fn clone_and_wait(admin: &Client, vm: &str, body: Value) -> Value {
    let (status, task) = clone(admin, vm, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

fn failed_with(task: &Value, error: &str) {
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let got = task["status"]["error"].as_str().unwrap();
    assert!(got.contains(error), "{task}");
}

#[tokio::test]
async fn clone_copies_disks_into_a_new_stopped_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "fast", vec![env.node]).await;
    let root = volume(&env, "root").await;
    let data = volume(&env, "data").await;
    let storage = fake_storage(&mut env, &[root, data]).await;
    let id = vm_with_disks(&admin, "web", &["root", "data"]).await;
    // 원본은 노드에 배치됐고 예약이 있다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = json!(env.node);
    spec["snapshot_schedule"] = json!({ "every": "daily", "keep": 3 });
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let vars = uefi_vars_path(&state_dir(&env), id);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars-1").unwrap();

    succeeded(&clone_and_wait(&admin, "web", json!({ "name": "web-2" })).await);
    let source = get_vm(&env, "web").unwrap();
    let copy = get_vm(&env, "web-2").unwrap();
    let disks: Vec<&str> = copy.spec.disks.iter().map(|d| d.volume.as_str()).collect();
    assert_eq!(disks, ["web-2-root", "web-2-data"]);
    assert_eq!(copy.spec.disks[0].bus, source.spec.disks[0].bus);
    assert_eq!(copy.spec.power, PowerState::Stopped);
    assert_eq!(copy.spec.node, None);
    assert_eq!(copy.spec.snapshot_schedule, None);
    assert_eq!(copy.spec.memory_mib, source.spec.memory_mib);
    assert!(copy.spec.nics[0].mac.is_some());
    assert_ne!(copy.spec.nics[0].mac, source.spec.nics[0].mac);
    assert_eq!(copy.status.snapshot, None);
    let mut copied = Vec::new();
    for name in disks {
        let v = get_volume(&env, name).unwrap();
        assert_eq!(v.spec.pool, "local");
        assert_eq!(v.spec.format, ImageFormat::Qcow2);
        assert_eq!(v.spec.size_bytes, MIB);
        assert_eq!(v.status.phase, VolumePhase::Ready);
        assert_eq!(v.status.node, Some(env.node));
        copied.push(("copy", v.id));
    }
    assert_eq!(storage.calls(), copied);
    assert_eq!(
        std::fs::read(uefi_vars_path(&state_dir(&env), copy.id)).unwrap(),
        b"vars-1"
    );

    // 스냅샷에서, 다른 풀로. 이미 있는 이름은 피하고, 변수는 스냅샷 때의 것이다.
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    succeeded(&admin.wait_task(&body).await);
    std::fs::write(&vars, b"vars-2").unwrap();
    volume(&env, "web-3-root").await;
    succeeded(
        &clone_and_wait(
            &admin,
            "web",
            json!({ "name": "web-3", "snapshot": "s1", "pool": "fast" }),
        )
        .await,
    );
    let copy = get_vm(&env, "web-3").unwrap();
    let disks: Vec<&str> = copy.spec.disks.iter().map(|d| d.volume.as_str()).collect();
    assert_eq!(disks, ["web-3-root-2", "web-3-data"]);
    for name in disks {
        assert_eq!(get_volume(&env, name).unwrap().spec.pool, "fast");
    }
    assert_eq!(
        std::fs::read(uefi_vars_path(&state_dir(&env), copy.id)).unwrap(),
        b"vars-1"
    );
}

#[tokio::test]
async fn failed_copy_removes_the_volumes_already_made() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    volume(&env, "data").await;
    // `data`의 파일이 없어 두 번째 복사가 실패한다.
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "web", &["root", "data"]).await;

    let task = clone_and_wait(&admin, "web", json!({ "name": "web-2" })).await;
    failed_with(&task, "data");
    assert_eq!(storage.calls().len(), 2);
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").is_none());
}

#[tokio::test]
async fn name_taken_during_the_copy_removes_volumes_and_uefi_vars() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    let id = vm_with_disks(&admin, "web", &["root"]).await;
    let vars = uefi_vars_path(&state_dir(&env), id);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars").unwrap();

    storage.pause();
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    let (status, body) = admin.post("/vms", vm_request("web-2")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    storage.resume();

    let task = admin.wait_task(&task).await;
    failed_with(&task, "web-2");
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").unwrap().spec.disks.is_empty());
    assert_eq!(vm_dirs(&env), [id.to_string()]);
}

#[tokio::test]
async fn timed_out_clone_removes_the_volumes_it_made() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = Arc::new(FakeStorage::default());
    storage.set_size(root, MIB);
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |c| {
        c.timeout = Duration::from_millis(300);
    })
    .await;
    vm_with_disks(&admin, "web", &["root"]).await;

    // 복사가 끝나지 않는다.
    storage.pause();
    let task = clone_and_wait(&admin, "web", json!({ "name": "web-2" })).await;
    failed_with(&task, "timed out");
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").is_none());
}

#[tokio::test]
async fn clone_fails_cleanly_when_the_source_is_gone() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    let id = vm_with_disks(&admin, "web", &["root"]).await;
    let (_, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    succeeded(&admin.wait_task(&body).await);

    // 스냅샷 삭제가 먼저 줄 섰다. 복제는 그 뒤에 돌아 스냅샷이 없는 것을 본다.
    env.stop_runner().await;
    let (status, delete) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{delete}");
    let (status, from_snapshot) =
        clone(&admin, "web", json!({ "name": "web-2", "snapshot": "s1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{from_snapshot}");
    env.start_runner_with(storage.clone()).await;
    succeeded(&admin.wait_task(&delete).await);
    failed_with(
        &admin.wait_task(&from_snapshot).await,
        "snapshot was deleted",
    );

    // API를 거치지 않고 원본 VM이 지워졌다.
    env.stop_runner().await;
    let (status, from_vm) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{from_vm}");
    env.cluster.delete::<VmSpec>(id, None).await.unwrap();
    env.start_runner_with(storage.clone()).await;
    failed_with(&admin.wait_task(&from_vm).await, "VM was deleted");

    assert!(!storage.calls().iter().any(|(call, _)| *call == "copy"));
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").is_none());
}

#[tokio::test]
async fn clone_fails_when_the_target_pool_is_no_longer_ready() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "nas", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "web", &["root"]).await;

    // API가 받은 뒤 Task가 돌기 전에 대상 풀이 이 노드에서 준비되지 않게 됐다(NFS 서버가 멈춤).
    env.stop_runner().await;
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2", "pool": "nas" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let down = PoolState {
        ready: false,
        error: Some("nfs server not responding".into()),
        ..Default::default()
    };
    common::set_pool_state(&env.cluster, env.node, "nas", down).await;
    env.start_runner_with(storage.clone()).await;
    failed_with(&admin.wait_task(&task).await, "nfs server not responding");
    assert!(!storage.calls().iter().any(|(call, _)| *call == "copy"));
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").is_none());
}

#[tokio::test]
async fn snapshot_delete_waits_for_a_clone_of_the_same_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "web", &["root"]).await;
    let (_, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    succeeded(&admin.wait_task(&body).await);

    // 복사하는 동안 그 스냅샷을 지우라는 요청이 온다. 복제가 끝난 뒤에 지운다.
    storage.pause();
    let (status, copy) = clone(&admin, "web", json!({ "name": "web-2", "snapshot": "s1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{copy}");
    storage.reached().await;
    let (status, delete) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{delete}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let path = format!("/tasks/{}", delete["id"].as_str().unwrap());
    let (_, current) = admin.get(&path).await;
    assert_eq!(current["status"]["state"], "pending", "{current}");

    storage.resume();
    succeeded(&admin.wait_task(&copy).await);
    succeeded(&admin.wait_task(&delete).await);
    let calls: Vec<&str> = storage.calls().into_iter().map(|(c, _)| c).collect();
    assert_eq!(calls, ["create", "copy", "delete"]);
}

#[tokio::test]
async fn clone_between_lvm_and_file_pools_takes_the_target_format() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let thin = Resource::new(
        "thin",
        StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&thin).await.unwrap();
    env.pool_ready("thin").await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "web", &["root"]).await;

    // qcow2 디스크를 LVM-thin 풀로: raw
    succeeded(&clone_and_wait(&admin, "web", json!({ "name": "lv", "pool": "thin" })).await);
    let copy = get_volume(&env, "lv-root").unwrap();
    assert_eq!(copy.spec.format, ImageFormat::Raw);
    assert_eq!(storage.copy_formats(), [(copy.id, ImageFormat::Raw)]);

    // 그 LVM 디스크를 파일 풀로: 스냅샷을 담을 수 있는 qcow2
    succeeded(&clone_and_wait(&admin, "lv", json!({ "name": "back", "pool": "local" })).await);
    let back = get_volume(&env, "back-lv-root").unwrap();
    assert_eq!(back.spec.format, ImageFormat::Qcow2);
    assert_eq!(storage.copy_formats()[1], (back.id, ImageFormat::Qcow2));
}
