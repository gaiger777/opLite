//! 스냅샷 API: 검사, 리소스와 Task 만들기, VM·볼륨 삭제 규칙, 볼륨 크기 늘리기.
//! Task 실행기를 멈춰 두고 API가 남긴 리소스와 Task의 모양만 확인한다(실행은 `snapshot_tasks`).

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

/// 남은 Task를 끝난 것으로 기록한다. 끝나지 않은 스냅샷 만들기 Task는 VM 수정을 막는다.
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

/// 풀 `local`(노드 `nodes`)을 저장소에 직접 만든다.
async fn pool(env: &Env, nodes: Vec<Uuid>) {
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir {
                path: env.dir.path().join("pool"),
            },
            nodes,
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 바로 Ready인 볼륨을 저장소에 직접 만든다(파일은 만들지 않는다).
async fn volume(env: &Env, name: &str, format: ImageFormat) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: MIB,
            format,
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
/// `spec.node`가 없는 VM은 reconciler가 다루지 않아 그대로 남는다.
async fn set_vm_status(env: &Env, id: Uuid, phase: VmPhase, node: Option<Uuid>) {
    let power = match phase {
        VmPhase::Starting | VmPhase::Running => PowerState::Running,
        _ => PowerState::Stopped,
    };
    set_vm_power(env, id, power).await;
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
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

/// 원하는 전원 상태만 직접 쓴다(reconciler가 아직 따라가지 않은 상태).
async fn set_vm_power(env: &Env, id: Uuid, power: PowerState) {
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    let spec = VmSpec {
        power,
        ..vm.spec.clone()
    };
    env.cluster
        .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
        .await
        .unwrap();
}

async fn snapshot_of(env: &Env, vm: Uuid, name: &str) -> Resource<SnapshotSpec, SnapshotStatus> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items
        .into_iter()
        .find(|s| s.spec.vm == vm && s.spec.name == name)
        .unwrap_or_else(|| panic!("snapshot {name} not found"))
}

async fn mark_ready(env: &Env, id: Uuid) {
    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(1),
        vmstate_bytes: None,
    };
    env.cluster
        .update_status::<SnapshotSpec>(id, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn create_records_pending_snapshot_and_task() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;

    let (status, task) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "before", "description": "first" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let snap = snapshot_of(&env, vm, "before").await;
    assert_eq!(task["spec"]["op"]["kind"], "snapshot-create");
    assert_eq!(task["spec"]["op"]["snapshot"], snap.id.to_string());
    assert_eq!(task["spec"]["target"], vm.to_string());
    // 배치되지 않은 VM이면 요청을 받은 노드에서 돈다.
    assert_eq!(task["spec"]["node"], env.node.to_string());
    assert_eq!(snap.name, snap.id.to_string());
    assert_eq!(snap.status.phase, SnapshotPhase::Pending);
    assert_eq!(snap.spec.parent, None);
    assert_eq!(snap.spec.description, "first");
    // 꺼진 VM은 디스크만 담는다.
    assert!(!snap.spec.memory);
    assert!(!snap.spec.scheduled);
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(snap.spec.vm_spec, current.spec);

    // 이름이나 id로 조회하고, VM별 목록과 전체 목록에 나온다.
    let (status, got) = admin.get("/vms/web/snapshots/before").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], snap.id.to_string());
    let (status, got) = admin.get(&format!("/vms/{vm}/snapshots/{}", snap.id)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["name"], "before");
    let (status, list) = admin.get("/vms/web/snapshots").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(list["revision"].as_u64().unwrap() > 0);
    let (status, all) = admin.get("/snapshots").await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["items"][0]["spec"]["name"], "before");
    let (status, body) = admin.get("/vms/web/snapshots/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 같은 VM 안에서 이름이 겹치면 409, 다른 VM이면 된다.
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "before" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
    admin.post("/vms", vm_request("db")).await;
    let (status, body) = admin
        .post("/vms/db/snapshots", json!({ "name": "before" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (_, list) = admin.get("/vms/web/snapshots").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // 다음 스냅샷의 부모는 VM의 현재 위치다.
    let mut st = current.status.clone();
    st.snapshot = Some(snap.id);
    env.cluster.update_status::<VmSpec>(vm, &st).await.unwrap();
    admin
        .post("/vms/web/snapshots", json!({ "name": "after" }))
        .await;
    assert_eq!(
        snapshot_of(&env, vm, "after").await.spec.parent,
        Some(snap.id)
    );
}

#[tokio::test]
async fn create_validates_request() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    vm_with_disks(&admin, "web", &["root"]).await;
    let bad = |body: Value| {
        let admin = admin.clone();
        async move {
            let (status, body) = admin.post("/vms/web/snapshots", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["code"], "invalid_request");
        }
    };
    bad(json!({ "name": "Bad_Name" })).await;
    bad(json!({ "name": Uuid::now_v7() })).await;
    bad(json!({ "name": "a", "description": "x".repeat(1025) })).await;
    bad(json!({ "name": "a", "memroy": true })).await;
    // 꺼진 VM에는 메모리가 없다.
    bad(json!({ "name": "a", "memory": true })).await;
    let (status, _) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "a", "description": "가".repeat(1024) }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = admin
        .post("/vms/missing/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn running_vm_snapshots_memory_by_default_on_its_node() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    let elsewhere = Uuid::now_v7();
    set_vm_status(&env, vm, VmPhase::Running, Some(elsewhere)).await;

    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "mem" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], elsewhere.to_string());
    assert!(snapshot_of(&env, vm, "mem").await.spec.memory);
    let (status, _) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "disk", "memory": false }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(!snapshot_of(&env, vm, "disk").await.spec.memory);

    // 시작·정지 중인 VM은 기다려야 한다.
    for phase in [VmPhase::Starting, VmPhase::Stopping] {
        set_vm_status(&env, vm, phase, Some(elsewhere)).await;
        let (status, body) = admin
            .post("/vms/web/snapshots", json!({ "name": "busy" }))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "vm_busy");
    }
}

#[tokio::test]
async fn disks_must_be_qcow2_and_memory_needs_a_disk() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    volume(&env, "data", ImageFormat::Raw).await;
    vm_with_disks(&admin, "web", &["root", "data"]).await;
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");

    // 디스크 없는 실행 중 VM: 메모리는 담을 곳이 없다. 기본은 설정만 담는다.
    let (_, created) = admin.post("/vms", vm_request("bare")).await;
    let bare: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    set_vm_status(&env, bare, VmPhase::Running, Some(env.node)).await;
    let (status, body) = admin
        .post(
            "/vms/bare/snapshots",
            json!({ "name": "a", "memory": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = admin
        .post("/vms/bare/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(!snapshot_of(&env, bare, "a").await.spec.memory);
}

#[tokio::test]
async fn revert_and_delete_need_a_ready_snapshot() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    let snap = snapshot_of(&env, vm, "s1").await;

    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_not_ready");
    let (status, body) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_not_ready");

    mark_ready(&env, snap.id).await;
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "snapshot-revert", "snapshot": snap.id })
    );
    assert_eq!(task["spec"]["target"], vm.to_string());
    let (status, task) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "snapshot-delete", "snapshot": snap.id, "scheduled": false })
    );
    // Task가 실행되기 전이므로 리소스는 남아 있다.
    assert_eq!(admin.get("/vms/web/snapshots/s1").await.0, StatusCode::OK);
    assert_eq!(
        admin.delete("/vms/web/snapshots/nope").await.0,
        StatusCode::NOT_FOUND
    );

    // 시작·정지 중에는 되돌리거나 지우지 않는다.
    set_vm_status(&env, vm, VmPhase::Stopping, Some(env.node)).await;
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
    let (status, body) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
}

/// 만들기·되돌리기·삭제 요청이 모두 `vm_busy`로 거절되는지.
async fn assert_snapshot_ops_busy(admin: &Client, why: &str) {
    let (status, body) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "busy", "memory": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}: {body}");
    assert_eq!(body["code"], "vm_busy", "{why}: {body}");
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}: {body}");
    assert_eq!(body["code"], "vm_busy", "{why}: {body}");
    let (status, body) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}: {body}");
    assert_eq!(body["code"], "vm_busy", "{why}: {body}");
}

#[tokio::test]
async fn snapshot_ops_wait_for_power_changes() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    mark_ready(&env, snapshot_of(&env, vm, "s1").await.id).await;
    finish_tasks(&env).await;

    // 켜라고 했지만 아직 켜지지 않았다.
    set_vm_power(&env, vm, PowerState::Running).await;
    assert_snapshot_ops_busy(&admin, "start pending").await;
    // 끄라고 했지만 아직 돌고 있다.
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    set_vm_power(&env, vm, PowerState::Stopped).await;
    assert_snapshot_ops_busy(&admin, "stop pending").await;

    // 실행 중으로 자리 잡은 VM이라도 재시작 Task가 끝나지 않았으면 기다린다.
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let (status, task) = admin.post("/vms/web/actions/reboot", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_snapshot_ops_busy(&admin, "reboot task").await;

    finish_tasks(&env).await;
    let (status, body) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "s2", "memory": false }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;
    let (status, body) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

#[tokio::test]
async fn revert_needs_the_snapshot_volumes() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    mark_ready(&env, snapshot_of(&env, vm, "s1").await.id).await;
    finish_tasks(&env).await;

    // 볼륨을 떼어 다른 VM에 붙이면 되돌릴 수 없다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["disks"] = json!([]);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    vm_with_disks(&admin, "other", &["root"]).await;
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_use");

    // 아직 만들고 있는 볼륨은 쓸 수 없다.
    assert_eq!(admin.delete("/vms/other").await.0, StatusCode::NO_CONTENT);
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let mut st = root.status.clone();
    st.phase = VolumePhase::Pending;
    env.cluster
        .update_status::<VolumeSpec>(root.id, &st)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_not_ready");

    // 볼륨이 없어졌으면 되돌릴 수 없다.
    env.cluster
        .delete::<VolumeSpec>(root.id, None)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn snapshots_block_vm_and_volume_deletion() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;

    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_has_snapshots");

    // VM에서 떼어 낸 볼륨도 스냅샷이 가리키면 지울 수 없다.
    finish_tasks(&env).await;
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["disks"] = json!([]);
    admin.put("/vms/web", json!({ "spec": spec }), None).await;
    let (status, body) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_snapshot");

    let snap = snapshot_of(&env, vm, "s1").await;
    env.cluster
        .delete::<SnapshotSpec>(snap.id, None)
        .await
        .unwrap();
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    assert_eq!(admin.delete("/volumes/root").await.0, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn viewers_read_snapshots_and_operators_manage_them() {
    let env = env().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    admin.post("/vms", vm_request("web")).await;

    let (status, body) = viewer
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = operator
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(viewer.get("/vms/web/snapshots").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/vms/web/snapshots/s1").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/snapshots").await.0, StatusCode::OK);
    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    mark_ready(&env, snapshot_of(&env, vm.id, "s1").await.id).await;
    for (status, _) in [
        viewer
            .post("/vms/web/snapshots/s1/revert", Value::Null)
            .await,
        viewer.delete("/vms/web/snapshots/s1").await,
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        operator
            .post("/vms/web/snapshots/s1/revert", Value::Null)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        operator.delete("/vms/web/snapshots/s1").await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn resize_only_grows_and_runs_where_the_volume_is_used() {
    let env = env().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    // 풀은 다른 노드에만 있다(파일 작업은 그 노드에서 한다).
    let pool_node = Uuid::now_v7();
    pool(&env, vec![pool_node]).await;
    let id = volume(&env, "data", ImageFormat::Raw).await;

    // 줄이기·같은 크기와 512 배수가 아닌 크기는 받지 않는다.
    for size in [0, MIB, 2 * MIB + 1] {
        let (status, body) = admin
            .post("/volumes/data/resize", json!({ "size_bytes": size }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_request");
    }
    let (status, _) = viewer
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, task) = operator
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "volume-resize", "size_bytes": 2 * MIB })
    );
    assert_eq!(task["spec"]["target"], id.to_string());
    assert_eq!(task["spec"]["node"], pool_node.to_string());

    // 배치된 적 없는 꺼진 VM이 쓰면 볼륨의 노드, 배치된 노드가 있으면 그 노드에서 한다
    // (그 VM의 스냅샷 Task와 같은 실행기의 줄에 선다).
    let vm = vm_with_disks(&admin, "web", &["data"]).await;
    let resize = || async {
        let (status, task) = admin
            .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        task["spec"]["node"].as_str().unwrap().to_string()
    };
    assert_eq!(resize().await, pool_node.to_string());
    let placed = Uuid::now_v7();
    set_vm_status(&env, vm, VmPhase::Stopped, Some(placed)).await;
    assert_eq!(resize().await, placed.to_string());

    // 실행 중인 VM이 쓰면 그 VM의 노드에서 한다.
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let (status, task) = admin
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());

    // 끈 뒤 다른 노드로 옮겼으면 디스크를 다음에 쓸 그 노드에서 한다.
    set_vm_status(&env, vm, VmPhase::Stopped, Some(env.node)).await;
    let moved = Uuid::now_v7();
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    let spec = VmSpec {
        node: Some(moved),
        ..current.spec.clone()
    };
    env.cluster
        .update_spec(vm, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    assert_eq!(resize().await, moved.to_string());

    let (status, _) = admin
        .post("/volumes/missing/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tasks_run_where_the_vm_disks_will_be_used() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());

    // 꺼진 VM을 A에서 B로 옮겼다. 디스크는 다음에 B에서 쓰이므로 B에서 한다.
    volume(&env, "root", ImageFormat::Qcow2).await;
    let moved = vm_with_disks(&admin, "moved", &["root"]).await;
    set_vm_status(&env, moved, VmPhase::Stopped, Some(a)).await;
    let vm = env.cluster.get::<VmSpec>(moved).unwrap().unwrap();
    let spec = VmSpec {
        node: Some(b),
        ..vm.spec.clone()
    };
    env.cluster
        .update_spec(moved, Some(vm.revision), vm.labels.clone(), &spec)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/vms/moved/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], b.to_string());

    // 배치된 적 없는 VM은 첫 디스크 볼륨이 있는 노드에서 한다.
    let data = volume(&env, "data", ImageFormat::Qcow2).await;
    let mut on_a = env.cluster.get::<VolumeSpec>(data).unwrap().unwrap();
    on_a.status.node = Some(a);
    env.cluster
        .update_status::<VolumeSpec>(data, &on_a.status)
        .await
        .unwrap();
    vm_with_disks(&admin, "new", &["data"]).await;
    let (status, task) = admin
        .post("/vms/new/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], a.to_string());
}
