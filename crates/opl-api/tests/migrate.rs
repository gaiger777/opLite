//! VM 라이브 마이그레이션 API: 요청 검사(설계 §4)와 마이그레이션 중 다른 동작 막기.
//! 이 노드 하나만 실제로 돈다(노드 간 클라이언트가 없다). 실제 마이그레이션은 `two_nodes.rs`가 본다.

mod common;

use common::{Client, Env, WAIT, mark_pool_ready, set_pool_state, vm_request};
use opl_types::audit::AuditQuery;
use opl_types::node::{
    FEATURES, Fencing, Node, NodeHa, NodeShutdown, NodeSpec, NodeStatus, PoolState, ShutdownAction,
};
use opl_types::placement::{PlacementRule, PlacementRuleSpec, RuleKind};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotSpec};
use opl_types::storage::{StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState};
use opl_types::vm::{Arch, MigrationStatus, VmHa, VmPhase, VmSpec};
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
        version: None,
        updates: None,
        features: vec![],
        cluster_addresses: vec![],
        shutdown: None,
        fence_check: None,
        interfaces: vec![],
        checks: vec![],
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
    assert_eq!(
        task["status"]["error"],
        "this node cannot reach other nodes"
    );
    // 원본 VM은 그대로 돈다.
    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    assert_eq!(vm.spec.node, Some(env.node));
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(vm.status.migration, None);

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

/// HA VM은 HA 가능 노드(펜싱이 켜진 노드)로만 옮긴다.
#[tokio::test]
async fn ha_vms_migrate_only_to_ha_capable_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup(&env, &admin).await;
    let ha = Some(NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 7,
        fence_after_secs: None,
    });
    edit_node(&env, env.node, |n| n.status.ha = ha).await;
    // API는 실행 중인 VM의 HA를 켜지 않으므로 저장소에 바로 쓴다.
    let mut vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    vm.spec.ha = Some(VmHa::default());
    env.cluster
        .update_spec(vm.id, None, vm.labels, &vm.spec)
        .await
        .unwrap();

    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_eq!(
        body["message"],
        "node \"node2\" does not support HA (fencing is off) and cannot run HA VMs",
        "{body}"
    );
    assert_conflict((status, body), "ha_unsupported");

    edit_node(&env, node2, |n| n.status.ha = ha).await;
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
}

#[tokio::test]
async fn migrate_needs_a_running_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;

    // 꺼진 VM은 옮길 것이 없다.
    create_pool(&env, &admin, "local", false).await;
    ready_volume(&env, "local", "data").await;
    create_vm(&env, &admin, "db", "data").await;
    assert_conflict(migrate(&admin, "db", "node2").await, "vm_not_running");
}

/// 노드 로컬 풀 `local`의 디스크를 쓰는 VM `db`를 이 노드에서 실행한다(공유 풀 VM `web`도 있다).
/// `node2`의 id를 돌려준다. `node2`는 아직 디스크 복사 기능을 알리지 않았다.
async fn setup_local(env: &Env, admin: &Client) -> Uuid {
    let node2 = setup(env, admin).await;
    create_pool(env, admin, "local", false).await;
    ready_volume(env, "local", "data").await;
    create_vm(env, admin, "db", "data").await;
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    node2
}

/// 노드가 이 빌드의 기능을 모두 알린 것으로 둔다.
async fn upgrade(env: &Env, node: Uuid) {
    edit_node(env, node, |n| {
        n.status.features = FEATURES.iter().map(|f| f.to_string()).collect()
    })
    .await;
}

/// 노드 로컬 풀의 디스크는 대상 노드의 같은 이름의 풀에 복사한다. 모든 노드가 그 기능을 알아야 한다.
#[tokio::test]
async fn local_disks_are_copied_to_the_same_named_pool() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup_local(&env, &admin).await;

    // 옛 노드가 있으면 로컬 디스크가 있는 VM은 옮기지 않는다. 공유 풀만의 VM은 그대로 옮긴다.
    let (status, body) = migrate(&admin, "db", "node2").await;
    assert_eq!(
        body["message"],
        "some nodes do not support storage migration yet (node2); upgrade them first",
        "{body}"
    );
    assert_conflict((status, body), "cluster_upgrading");
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;

    upgrade(&env, node2).await;
    // 대상 노드에서 같은 이름의 풀을 쓸 수 없으면 고를 수 없는 노드다.
    set_pool_state(
        &env.cluster,
        node2,
        "local",
        PoolState {
            ready: false,
            ..Default::default()
        },
    )
    .await;
    let (status, body) = migrate(&admin, "db", "node2").await;
    assert_eq!(
        body["message"], "node \"node2\" cannot use storage pool \"local\" for disk \"data\"",
        "{body}"
    );
    assert_conflict((status, body), "no_capacity");

    mark_pool_ready(&env.cluster, node2, "local").await;
    let (status, task) = migrate(&admin, "db", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    // pool을 주지 않으면 Task에도 없다(옛 모양).
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "vm-migrate", "node": node2 })
    );
    admin.wait_task(&task).await;
}

/// `pool`은 노드 로컬 풀의 디스크를 둘 대상 노드의 풀이다.
#[tokio::test]
async fn pool_picks_where_local_disks_go() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup_local(&env, &admin).await;
    upgrade(&env, node2).await;
    let migrate_to = |vm: &'static str, pool: &'static str| {
        let admin = &admin;
        async move {
            admin
                .post(
                    &format!("/vms/{vm}/migrate"),
                    json!({ "node": "node2", "pool": pool }),
                )
                .await
        }
    };

    // 로컬 디스크가 없는 VM에는 쓸 데가 없다.
    let (status, body) = migrate_to("web", "local").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(
        body["message"],
        "VM \"web\" has no disks on node-local pools; pool applies only to them"
    );
    let (status, body) = migrate_to("db", "nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // node2가 쓸 수 없는 풀
    create_pool(&env, &admin, "fast", false).await;
    let fast = env
        .cluster
        .get_by_name::<StoragePoolSpec>("fast")
        .unwrap()
        .unwrap();
    let mut spec = fast.spec.clone();
    spec.nodes = vec![env.node];
    env.cluster
        .update_spec(fast.id, None, fast.labels.clone(), &spec)
        .await
        .unwrap();
    let (status, body) = migrate_to("db", "fast").await;
    assert_eq!(
        body["message"], "node \"node2\" cannot use storage pool \"fast\" for disk \"data\"",
        "{body}"
    );
    assert_conflict((status, body), "no_capacity");

    // 공유 풀로도 옮긴다. Task에 그 풀을 적는다.
    let (status, task) = migrate_to("db", "nas").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "vm-migrate", "node": node2, "pool": "nas" })
    );
    admin.wait_task(&task).await;
}

/// 스냅샷은 복사하지 않으므로 스냅샷이 있는 VM의 로컬 디스크는 옮기지 않는다.
#[tokio::test]
async fn local_disks_of_a_vm_with_snapshots_are_not_moved() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup_local(&env, &admin).await;
    upgrade(&env, node2).await;
    for vm in ["db", "web"] {
        let vm = env.cluster.get_by_name::<VmSpec>(vm).unwrap().unwrap();
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
    }

    let (status, body) = migrate(&admin, "db", "node2").await;
    assert_eq!(
        body["message"],
        "VM \"db\" has snapshots and disk \"data\" is on node-local pool \"local\"; snapshots are not copied, delete them first",
        "{body}"
    );
    assert_conflict((status, body), "migration_unsupported");
    // 공유 풀의 디스크는 복사하지 않으므로 스냅샷이 있어도 된다(지금 그대로).
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
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
    // 호스트가 꺼지는 중인 노드(종료 표시)도 받지 않는다.
    edit_node(&env, node2, |n| {
        n.status.shutdown = Some(NodeShutdown {
            action: ShutdownAction::Failover,
            maintenance: false,
            at: 1,
        })
    })
    .await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_eq!(body["message"], "node \"node2\" is shutting down", "{body}");
    assert_conflict((status, body), "node_unavailable");
    edit_node(&env, node2, |n| n.status.shutdown = None).await;
    // 위트니스는 VM을 받지 않는다.
    edit_node(&env, node2, |n| n.spec.witness = true).await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_eq!(
        body["message"], "node \"node2\" is a witness node and cannot run VMs",
        "{body}"
    );
    assert_conflict((status, body), "node_unavailable");
    edit_node(&env, node2, |n| n.spec.witness = false).await;

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
        disks: vec![],
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

/// 이 노드의 감사 로그에서 `action`과 `status`가 같은 항목의 detail(쓰기 스레드가 곧 적는다)
async fn audit_detail(env: &Env, action: &str, status: u16) -> Option<String> {
    let start = std::time::Instant::now();
    loop {
        let entries = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap();
        if let Some(e) = entries
            .iter()
            .find(|e| e.action == action && e.status == status)
        {
            return e.detail.clone();
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
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

/// hard 배치 규칙을 어기는 대상으로는 옮기지 않는다(409 `placement_rule`). 관리자만 `force`로 넘을 수 있고,
/// 넘은 규칙이 감사 기록에 남는다. 노드 조건은 `force`로도 넘지 않는다.
#[tokio::test]
async fn hard_rules_refuse_the_target_unless_an_admin_forces_it() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let node2 = setup(&env, &admin).await;
    add_rule(
        &env,
        "never",
        RuleKind::NodeAntiAffinity,
        &["web"],
        &["node2"],
    )
    .await;

    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_eq!(
        body["message"],
        "moving VM \"web\" to node \"node2\" breaks placement rules: never (VMs kept together by an affinity rule move together only when their node is drained)"
    );
    assert_placement_rule((status, body), &["never"]);
    assert_placement_rule(migrate(&operator, "web", "node2").await, &["never"]);

    let force = json!({ "node": "node2", "force": true });
    let (status, body) = operator.post("/vms/web/migrate", force.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, task) = admin.post("/vms/web/migrate", force.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "vm-migrate", "node": node2 })
    );
    admin.wait_task(&task).await;
    assert_eq!(
        audit_detail(&env, "POST /vms/{key}/migrate", 202).await,
        Some("force rules=never".into())
    );

    // 노드 조건(유지보수)은 force로도 넘지 않는다.
    edit_node(&env, node2, |n| n.spec.maintenance = true).await;
    let (status, body) = admin.post("/vms/web/migrate", force).await;
    assert_conflict((status, body), "node_unavailable");
}

/// 자리 잡은 hard affinity 상대가 있는 VM은 혼자 옮기지 않는다. 상대가 꺼지면 옮길 수 있다.
#[tokio::test]
async fn a_vm_with_a_running_affinity_partner_does_not_move_alone() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let mut req = vm_request("db");
    req["spec"]["node"] = json!(env.node);
    admin.post("/vms", req).await;
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    add_rule(&env, "together", RuleKind::Affinity, &["web", "db"], &[]).await;

    assert_placement_rule(migrate(&admin, "web", "node2").await, &["together"]);
    let task = admin.act("db", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
}
