//! 노드 비우기 API(`POST /nodes/{key}/drain`): 권한, 노드 확인, 끝나지 않은 비우기, 유지보수로 바꾸기,
//! 옛 노드. 이 노드 하나만 실제로 돌므로 비우기 Task는 옮기지 못하는 이유만 본다. 실제로 옮기는 것은
//! `two_nodes.rs`가 본다.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, MOUNT_ROOT, set_pool_state, vm_request};
use opl_driver::DirStorage;
use opl_types::node::{FEATURES, Node, NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotSpec};
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::version::BUILD;
use opl_types::vm::Arch;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 실행기도 reconciler도 없는 노드를 저장소에만 만든다. `edit`로 상태를 바꾼다.
async fn add_node(env: &Env, name: &str, edit: impl FnOnce(&mut Node)) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
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
        ha: None,
        underlay: None,
        firewall: false,
        version: Some(BUILD.into()),
        updates: None,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
    };
    edit(&mut node);
    env.cluster.create(&node).await.unwrap();
    node.id
}

async fn drain(client: &Client, node: &str) -> (StatusCode, Value) {
    client
        .post(&format!("/nodes/{node}/drain"), Value::Null)
        .await
}

/// 409와 그 code를 확인한다.
fn assert_conflict((status, body): (StatusCode, Value), code: &str) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], code, "{body}");
}

fn maintenance(env: &Env, id: Uuid) -> bool {
    env.cluster
        .get::<NodeSpec>(id)
        .unwrap()
        .unwrap()
        .spec
        .maintenance
}

/// 풀 `pool`의 Ready 볼륨 `volume`을 디스크로 쓰는 VM `name`을 이 노드에서 실행한다.
async fn running_vm(env: &Env, admin: &Client, name: &str, pool: &str, shared: bool) {
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
    let mut volume: Volume = Resource::new(
        format!("{name}-root"),
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(env.node);
    volume.status.phase = VolumePhase::Ready;
    env.cluster.create(&volume).await.unwrap();
    let mut req = vm_request(name);
    req["spec"]["disks"] = json!([{ "volume": volume.name, "bus": "virtio-blk" }]);
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn drain_puts_the_node_in_maintenance_and_creates_a_task() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    // 실행기가 없는 노드라 Task가 끝나지 않는다.
    let node2 = add_node(&env, "node2", |_| {}).await;

    for client in [&viewer, &operator] {
        let (status, body) = drain(client, "node2").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    assert!(!maintenance(&env, node2));
    let (status, body) = drain(&admin, "nowhere").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, task) = drain(&admin, "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"], json!({ "kind": "node-drain" }));
    assert_eq!(task["spec"]["target"], node2.to_string());
    assert_eq!(task["spec"]["node"], node2.to_string());
    assert!(maintenance(&env, node2));

    // 끝나지 않은 비우기가 있으면(id로 골라도) 409 node_busy다.
    assert_conflict(drain(&admin, &node2.to_string()).await, "node_busy");
}

#[tokio::test]
async fn drain_needs_an_online_node_that_is_not_a_witness() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let witness = add_node(&env, "witness", |n| n.spec.witness = true).await;
    let offline = add_node(&env, "offline", |n| n.status.online = false).await;

    let (status, body) = drain(&admin, "witness").await;
    assert_eq!(
        body["message"], "node \"witness\" is a witness node and runs no VMs",
        "{body}"
    );
    assert_conflict((status, body), "node_unavailable");
    let (status, body) = drain(&admin, "offline").await;
    assert_eq!(
        body["message"], "node \"offline\" is offline; HA recovers the HA VMs of an offline node",
        "{body}"
    );
    assert_conflict((status, body), "node_unavailable");
    assert!(!maintenance(&env, witness));
    assert!(!maintenance(&env, offline));
}

/// 비우기를 모르는 노드(위트니스 포함)가 남았으면 409 `cluster_upgrading`이다. 버전은 보지 않으므로
/// 롤링 업그레이드 중(버전이 섞여도) 모든 노드가 비우기를 알면 된다. 끝나지 않은 비우기는 먼저 본다.
#[tokio::test]
async fn drain_needs_every_node_to_support_it() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 실행기가 없는 노드라 비우기 Task가 끝나지 않는다.
    add_node(&env, "node2", |_| {}).await;
    let (status, task) = drain(&admin, "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let old = add_node(&env, "old", |n| {
        n.spec.witness = true;
        n.status.version = Some(BUILD.into());
        n.status.features.clear();
    })
    .await;

    let (status, body) = drain(&admin, "node1").await;
    assert_eq!(
        body["message"], "some nodes do not support node drain yet (old); upgrade them first",
        "{body}"
    );
    assert_conflict((status, body), "cluster_upgrading");
    assert!(!maintenance(&env, env.node));
    assert_conflict(drain(&admin, "node2").await, "node_busy");

    // 옛 버전이어도 비우기를 알면 된다.
    let mut node = env.cluster.get::<NodeSpec>(old).unwrap().unwrap();
    node.status.version = Some("0.0.1".into());
    node.status.features = vec!["node-drain".into()];
    env.cluster
        .update_status::<NodeSpec>(old, &node.status)
        .await
        .unwrap();
    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

/// 이미 유지보수인 노드도 비운다. 옮길 VM이 없으면 바로 성공한다.
#[tokio::test]
async fn drain_of_an_empty_node_succeeds() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, node) = admin.get("/nodes/node1").await;
    let mut spec = node["spec"].clone();
    spec["maintenance"] = json!(true);
    let revision = node["revision"].as_u64();
    let (status, body) = admin
        .put("/nodes/node1", json!({ "spec": spec }), revision)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(maintenance(&env, env.node));
}

/// 옮기지 못한 VM은 이름 순으로 이유와 함께 실패 메시지에 남고, 노드는 유지보수로 남는다.
#[tokio::test]
async fn vms_that_cannot_move_are_reported() {
    let env = Env::start().await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "web", "nas", true).await;
    running_vm(&env, &admin, "db", "local", false).await;
    // 다른 노드에는 VM(256 MiB)이 들어갈 메모리가 없다.
    add_node(&env, "node2", |n| n.status.memory_mib = 128).await;

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "2 VM(s) were not moved: db (no node with capacity), web (no node with capacity)"
    );
    assert!(maintenance(&env, env.node));

    // 이 노드가 HVF(macOS)면 옮기지 못한다.
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.hypervisor = "qemu-hvf".into();
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();
    let (_, task) = drain(&admin, "node1").await;
    let task = admin.wait_task(&task).await;
    assert_eq!(
        task["status"]["error"],
        "2 VM(s) were not moved: db (hvf node), web (hvf node)"
    );
}

/// 대상은 VM을 받을 수 있는 노드 가운데 남은 메모리가 가장 많은 노드다. HVF(macOS) 노드는 메모리가
/// 더 많아도 고르지 않는다.
#[tokio::test]
async fn drain_skips_nodes_that_cannot_take_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "web", "nas", true).await;
    add_node(&env, "mac", |n| {
        n.status.hypervisor = "qemu-hvf".into();
        n.status.memory_mib = 65536;
    })
    .await;
    let small = add_node(&env, "small", |_| {}).await;

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    // 시험 노드는 다른 노드에 닿지 못해 마이그레이션 Task가 실패한다.
    assert_eq!(
        task["status"]["error"],
        "1 VM(s) were not moved: web (this node cannot reach other nodes)"
    );
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let migrations: Vec<&TaskOp> = tasks
        .iter()
        .map(|t| &t.spec.op)
        .filter(|op| matches!(op, TaskOp::VmMigrate { .. }))
        .collect();
    assert_eq!(
        migrations,
        [&TaskOp::VmMigrate {
            node: small,
            pool: None
        }]
    );
}

/// 로컬 디스크 VM의 대상은 같은 이름의 풀을 쓸 수 있는 노드 가운데 남은 메모리가 가장 많은 노드다.
/// 풀을 지정하지 않는다(같은 이름 규칙).
#[tokio::test]
async fn local_disk_vms_go_to_a_node_that_can_use_the_same_pool() {
    let env = Env::start().await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "db", "local", false).await;
    // 메모리가 더 많아도 그 풀이 준비되지 않은 노드는 고르지 않는다.
    let big = add_node(&env, "big", |n| n.status.memory_mib = 65536).await;
    set_pool_state(&env.cluster, big, "local", PoolState::default()).await;
    let small = add_node(&env, "small", |_| {}).await;

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let migrations: Vec<&TaskOp> = tasks
        .iter()
        .map(|t| &t.spec.op)
        .filter(|op| matches!(op, TaskOp::VmMigrate { .. }))
        .collect();
    assert_eq!(
        migrations,
        [&TaskOp::VmMigrate {
            node: small,
            pool: None
        }]
    );
}

/// 로컬 디스크 VM을 옮기지 못하는 이유: 같은 이름의 풀을 쓸 수 있는 노드가 없음, 디스크 복사를 모르는
/// 노드(위트니스 포함), 스냅샷.
#[tokio::test]
async fn local_disk_vms_that_cannot_move_are_reported() {
    let env = Env::start().await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "db", "local", false).await;
    let node2 = add_node(&env, "node2", |_| {}).await;
    set_pool_state(&env.cluster, node2, "local", PoolState::default()).await;

    let error = |task: Value| task["status"]["error"].as_str().unwrap().to_string();
    let (_, task) = drain(&admin, "node1").await;
    assert_eq!(
        error(admin.wait_task(&task).await),
        "1 VM(s) were not moved: db (no node can take its local disks)"
    );

    let old = add_node(&env, "old", |n| {
        n.spec.witness = true;
        n.status.features.retain(|f| f != "storage-migration");
    })
    .await;
    let (_, task) = drain(&admin, "node1").await;
    assert_eq!(
        error(admin.wait_task(&task).await),
        "1 VM(s) were not moved: db (disks on a local pool (some nodes do not support storage migration yet))"
    );
    env.cluster.delete::<NodeSpec>(old, None).await.unwrap();

    let vm = env
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("db")
        .unwrap()
        .unwrap();
    let mut snapshot: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm: vm.id,
            name: "s1".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: vm.spec.clone(),
        },
    );
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();
    let (_, task) = drain(&admin, "node1").await;
    assert_eq!(
        error(admin.wait_task(&task).await),
        "1 VM(s) were not moved: db (disks on a local pool and snapshots)"
    );
}

/// "no node can take its local disks"는 풀 때문일 때만이다: 옮길 디스크를 빼면 받을 노드가 있을 때.
/// 메모리가 모자란 노드뿐이면 풀과 상관없이 "no node with capacity"다.
#[tokio::test]
async fn local_disk_vms_without_room_anywhere_lack_capacity() {
    let env = Env::start().await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "db", "local", false).await;
    let small = add_node(&env, "small", |n| n.status.memory_mib = 1).await;
    let error = |task: Value| task["status"]["error"].as_str().unwrap().to_string();

    // 풀은 쓸 수 있지만 메모리가 모자란다.
    let (_, task) = drain(&admin, "node1").await;
    assert_eq!(
        error(admin.wait_task(&task).await),
        "1 VM(s) were not moved: db (no node with capacity)"
    );
    // 풀도 쓸 수 없지만 메모리부터 모자란다.
    set_pool_state(&env.cluster, small, "local", PoolState::default()).await;
    let (_, task) = drain(&admin, "node1").await;
    assert_eq!(
        error(admin.wait_task(&task).await),
        "1 VM(s) were not moved: db (no node with capacity)"
    );
}

/// 끝나지 않은 Task가 있는 VM은 `migration_timeout`까지 기다린 뒤 `busy`로 남긴다.
#[tokio::test]
async fn a_vm_that_stays_busy_is_reported() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.migration_timeout = Duration::from_millis(500)
    })
    .await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "web", "nas", true).await;
    add_node(&env, "node2", |_| {}).await;
    let vm = env
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    // 실행기가 없는 노드의 Task라 끝나지 않는다.
    let mut stuck: Task = Resource::new(
        "stuck",
        TaskSpec {
            op: TaskOp::VmStop,
            target: vm.id,
            node: Uuid::now_v7(),
        },
    );
    stuck.status.state = TaskState::Running;
    env.cluster.create(&stuck).await.unwrap();

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(
        task["status"]["error"],
        "1 VM(s) were not moved: web (busy)"
    );
}
