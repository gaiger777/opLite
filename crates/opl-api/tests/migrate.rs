//! VM 라이브 마이그레이션 API: 요청 검사(설계 §4)와 마이그레이션 중 다른 동작 막기.
//! 실제 마이그레이션은 아직 없다. Task 실행기는 "not supported yet"으로 실패시킨다.

mod common;

use common::{Client, Env, mark_pool_ready, vm_request};
use opl_types::node::{Node, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState};
use opl_types::vm::{Arch, MigrationStatus, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 실행기도 reconciler도 없는 두 번째 노드를 저장소에만 만든다(이 노드의 상태를 테스트가 바꾼다).
async fn add_node(env: &Env, name: &str) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
        },
    );
    node.status = NodeStatus {
        online: true,
        cpus: 4,
        memory_mib: 8192,
        hypervisor: "mock".into(),
        bridges: vec![],
        arch: Some(Arch::Aarch64),
        pools: Default::default(),
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 노드의 상태를 바꾼다.
async fn edit_node(env: &Env, id: Uuid, f: impl FnOnce(&mut Node)) {
    let mut node = env.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    f(&mut node);
    env.cluster
        .update_spec(id, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    env.cluster
        .update_status::<NodeSpec>(id, &node.status)
        .await
        .unwrap();
}

/// 풀 `pool`을 만들고 이 노드에서 준비된 것으로 둔다.
async fn create_pool(env: &Env, admin: &Client, pool: &str, shared: bool) {
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": pool,
                "spec": {
                    "backend": { "kind": "dir", "path": env.dir.path().join(pool) },
                    "shared": shared
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready(pool).await;
}

/// 바로 Ready인 볼륨을 클러스터에 직접 만든다(파일은 만들지 않는다).
async fn ready_volume(env: &Env, pool: &str, name: &str) {
    let mut volume: Volume = Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(env.node);
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

/// 이 노드에 고정된, 볼륨 `volume`을 디스크로 쓰는 VM을 만든다.
async fn create_vm(env: &Env, admin: &Client, name: &str, volume: &str) {
    let mut req = vm_request(name);
    req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// 공유 풀 `nas`의 디스크를 쓰는 VM `web`을 이 노드에서 실행하고, 그 풀을 쓸 수 있는 노드
/// `node2`를 더한다. `node2`의 id를 돌려준다.
async fn setup(env: &Env, admin: &Client) -> Uuid {
    let node2 = add_node(env, "node2").await;
    create_pool(env, admin, "nas", true).await;
    mark_pool_ready(&env.cluster, node2, "nas").await;
    ready_volume(env, "nas", "root").await;
    create_vm(env, admin, "web", "root").await;
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    node2
}

async fn migrate(client: &Client, vm: &str, node: &str) -> (StatusCode, Value) {
    client
        .post(&format!("/vms/{vm}/migrate"), json!({ "node": node }))
        .await
}

/// 409와 그 code를 확인한다.
fn assert_conflict((status, body): (StatusCode, Value), code: &str) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], code, "{body}");
}

#[tokio::test]
async fn migrate_creates_a_task_on_the_source_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup(&env, &admin).await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    let (status, _) = migrate(&viewer, "web", "node2").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, task) = migrate(&operator, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "vm-migrate", "node": node2 })
    );
    assert_eq!(task["spec"]["target"], vm["id"]);
    // 원본 노드(VM이 도는 노드)의 실행기가 한다.
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], "VM migration is not supported yet");

    // 노드는 id로도 고른다. 이름이 없으면 404, 지금 노드면 400이다.
    let (status, task) = migrate(&admin, "web", &node2.to_string()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
    let (status, body) = migrate(&admin, "web", "nowhere").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = migrate(&admin, "web", "node1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    let (status, _) = migrate(&admin, "nope", "node2").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn migrate_needs_a_running_vm_on_shared_pools() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;

    // 꺼진 VM은 옮길 것이 없다.
    create_pool(&env, &admin, "local", false).await;
    ready_volume(&env, "local", "data").await;
    create_vm(&env, &admin, "db", "data").await;
    assert_conflict(migrate(&admin, "db", "node2").await, "vm_not_running");

    // 공유가 아닌 풀의 디스크는 대상 노드에서 열 수 없다.
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, body) = migrate(&admin, "db", "node2").await;
    assert_conflict((status, body.clone()), "migration_unsupported");
    assert!(
        body["message"].as_str().unwrap().contains("\"data\""),
        "{body}"
    );
}

#[tokio::test]
async fn migrate_checks_the_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup(&env, &admin).await;

    // HVF(macOS) 노드는 마이그레이션을 하지 못한다. 대상이든 원본이든.
    edit_node(&env, node2, |n| n.status.hypervisor = "qemu-hvf".into()).await;
    assert_conflict(
        migrate(&admin, "web", "node2").await,
        "migration_unsupported",
    );
    edit_node(&env, node2, |n| n.status.hypervisor = "qemu-kvm".into()).await;
    edit_node(&env, env.node, |n| n.status.hypervisor = "qemu-hvf".into()).await;
    assert_conflict(
        migrate(&admin, "web", "node2").await,
        "migration_unsupported",
    );
    edit_node(&env, env.node, |n| n.status.hypervisor = "mock".into()).await;

    edit_node(&env, node2, |n| n.status.online = false).await;
    assert_conflict(migrate(&admin, "web", "node2").await, "node_unavailable");
    edit_node(&env, node2, |n| {
        n.status.online = true;
        n.spec.maintenance = true;
    })
    .await;
    assert_conflict(migrate(&admin, "web", "node2").await, "node_unavailable");
    edit_node(&env, node2, |n| n.spec.maintenance = false).await;

    edit_node(&env, node2, |n| n.status.arch = Some(Arch::X86_64)).await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "machine_arch_mismatch");
    edit_node(&env, node2, |n| n.status.arch = Some(Arch::Aarch64)).await;

    // 풀이 준비되지 않았거나 메모리가 모자라면 스케줄러가 고를 수 없는 노드다.
    edit_node(&env, node2, |n| {
        n.status.pools.get_mut("nas").unwrap().ready = false
    })
    .await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_conflict((status, body.clone()), "no_capacity");
    assert!(
        body["message"].as_str().unwrap().contains("storage pools"),
        "{body}"
    );
    mark_pool_ready(&env.cluster, node2, "nas").await;
    edit_node(&env, node2, |n| n.status.memory_mib = 128).await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_conflict((status, body.clone()), "no_capacity");
    assert!(
        body["message"].as_str().unwrap().contains("memory"),
        "{body}"
    );
    edit_node(&env, node2, |n| n.status.memory_mib = 8192).await;
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

/// 끝나지 않은 마이그레이션 Task가 있으면 VM의 다른 동작이 모두 409 `vm_busy`다.
#[tokio::test]
async fn migrate_task_makes_the_vm_busy() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    // 실행기를 멈춰 Task가 끝나지 않게 한다.
    env.stop_runner().await;
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");

    assert_busy(&admin, "web", "root", "node2").await;
    // 마이그레이션 Task가 끝나면 풀린다.
    env.start_runner().await;
    admin.wait_task(&task).await;
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// `migrating`인 VM은 다른 동작이 모두 409 `vm_busy`다.
#[tokio::test]
async fn migrating_phase_makes_the_vm_busy() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup(&env, &admin).await;
    // 이 노드의 reconciler가 손대지 않도록 node2에서 도는 VM을 둔다.
    ready_volume(&env, "nas", "far-root").await;
    let mut req = vm_request("far");
    req["spec"]["disks"] = json!([{ "volume": "far-root", "bus": "virtio-blk" }]);
    req["spec"]["node"] = json!(node2);
    req["spec"]["power"] = json!("running");
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut vm = env.cluster.get_by_name::<VmSpec>("far").unwrap().unwrap();
    vm.status.phase = VmPhase::Migrating;
    vm.status.node = Some(node2);
    vm.status.migration = Some(MigrationStatus {
        node: env.node,
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &vm.status)
        .await
        .unwrap();

    assert_busy(&admin, "far", "far-root", "node1").await;
}

/// 켜기·끄기·강제 끄기·재시작·`PUT`·지우기·스냅샷·복제·디스크(`volume`) 크기 늘리기·`node`로
/// 마이그레이션이 모두 409 `vm_busy`다.
async fn assert_busy(admin: &Client, vm: &str, volume: &str, node: &str) {
    for action in ["start", "stop", "stop?mode=force", "reboot"] {
        let (status, body) = admin
            .post(&format!("/vms/{vm}/actions/{action}"), Value::Null)
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{action}: {body}");
        assert_eq!(body["code"], "vm_busy", "{action}: {body}");
    }
    let (_, current) = admin.get(&format!("/vms/{vm}")).await;
    let mut spec = current["spec"].clone();
    spec["memory_mib"] = json!(512);
    assert_conflict(
        admin
            .put(&format!("/vms/{vm}"), json!({ "spec": spec }), None)
            .await,
        "vm_busy",
    );
    assert_conflict(admin.delete(&format!("/vms/{vm}")).await, "vm_busy");
    assert_conflict(
        admin
            .post(&format!("/vms/{vm}/snapshots"), json!({ "name": "s1" }))
            .await,
        "vm_busy",
    );
    assert_conflict(
        admin
            .post(&format!("/vms/{vm}/clone"), json!({ "name": "copy" }))
            .await,
        "vm_busy",
    );
    assert_conflict(
        admin
            .post(
                &format!("/volumes/{volume}/resize"),
                json!({ "size_bytes": 2 << 20 }),
            )
            .await,
        "vm_busy",
    );
    assert_conflict(migrate(admin, vm, node).await, "vm_busy");
}

/// 끝나지 않은 스냅샷·크기 늘리기 Task가 있으면 마이그레이션을 시작하지 않는다.
#[tokio::test]
async fn migrate_waits_for_snapshot_and_resize_tasks() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    env.stop_runner().await;

    let (status, resize) = admin
        .post("/volumes/root/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resize}");
    assert_conflict(migrate(&admin, "web", "node2").await, "vm_busy");
    // 크기 늘리기 Task를 끝난 것으로 두고 스냅샷 Task를 본다.
    let id = resize["id"].as_str().unwrap().parse().unwrap();
    let mut task = env.cluster.get::<TaskSpec>(id).unwrap().unwrap();
    task.status.state = TaskState::Failed;
    env.cluster
        .update_status::<TaskSpec>(task.id, &task.status)
        .await
        .unwrap();

    let (status, snapshot) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{snapshot}");
    assert_conflict(migrate(&admin, "web", "node2").await, "vm_busy");
}
