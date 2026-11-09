//! 백업 API: 만들기·목록·읽기·지우기·복원 요청의 검사와 Task 만들기, 백업이 있는 풀 지키기.
//! Task 실행기를 멈춰 두고 API가 남긴 리소스와 Task의 모양만 확인한다.

mod common;

use common::{Client, Env, mark_pool_ready, set_pool_state, vm_request};
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
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

/// 풀을 저장소에 직접 만들고 이 노드에서 준비됐다고 기록한다.
async fn pool_with(env: &Env, name: &str, backend: PoolBackend, shared: bool, backups: bool) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend,
            nodes: vec![],
            shared,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 디렉터리 풀
async fn dir_pool(env: &Env, name: &str, shared: bool, backups: bool) {
    let backend = PoolBackend::Dir {
        path: env.dir.path().join(name),
    };
    pool_with(env, name, backend, shared, backups).await;
}

/// 이 노드에 파일이 있는 바로 Ready인 볼륨을 저장소에 직접 만든다(파일은 만들지 않는다).
async fn volume(env: &Env, pool: &str, name: &str, format: ImageFormat) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: MIB,
            format,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    volume.status.node = Some(env.node);
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

/// VM의 실제 상태와 그에 맞는 원하는 전원 상태를 직접 쓴다. 노드는 `status.node`에만 쓴다
/// (이 노드의 reconciler가 VM을 건드리지 않게).
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

/// 온라인인 다른 노드를 저장소에 만든다.
async fn other_node(env: &Env, name: &str) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status.online = true;
    env.cluster.create(&node).await.unwrap();
    node.id
}

async fn backup(admin: &Client, vm: &str, body: Value) -> (StatusCode, Value) {
    admin.post(&format!("/vms/{vm}/backups"), body).await
}

/// 백업을 요청하고 그 백업 id를 돌려준다.
async fn backup_ok(admin: &Client, vm: &str, body: Value) -> Uuid {
    let (status, task) = backup(admin, vm, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    task["spec"]["op"]["backup"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn set_phase(env: &Env, id: Uuid, phase: BackupPhase) {
    let status = BackupStatus {
        phase,
        created_at: Some(1),
        ..Default::default()
    };
    env.cluster
        .update_status::<BackupSpec>(id, &status)
        .await
        .unwrap();
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 백업 풀 `backups`, 디스크 풀 `local`, 디스크 하나의 꺼진 VM `web`
async fn setup(env: &Env, admin: &Client) -> Uuid {
    dir_pool(env, "backups", false, true).await;
    dir_pool(env, "local", false, false).await;
    volume(env, "local", "root", ImageFormat::Qcow2).await;
    vm_with_disks(admin, "web", &["root"]).await
}

#[tokio::test]
async fn backup_records_a_pending_backup_and_a_task_on_the_vm() {
    let env = env().await;
    let admin = env.admin().await;
    let vm = setup(&env, &admin).await;

    let (status, task) = backup(&admin, "web", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-backup");
    assert_eq!(task["spec"]["target"], vm.to_string());
    // 꺼져 있고 배치되지 않은 VM은 디스크와 백업 풀을 함께 쓸 수 있는 노드에서 돈다.
    assert_eq!(task["spec"]["node"], env.node.to_string());
    assert_eq!(task["status"]["state"], "pending");
    let id = task["spec"]["op"]["backup"].as_str().unwrap();

    let (status, got) = admin.get(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["status"]["phase"], "pending");
    assert_eq!(got["spec"]["vm"], vm.to_string());
    assert_eq!(got["spec"]["vm_name"], "web");
    assert_eq!(got["spec"]["pool"], "backups");
    assert_eq!(got["spec"]["vm_spec"]["disks"][0]["volume"], "root");
    assert!(got["spec"].get("job").is_none(), "{got}");
    // 기본 이름은 `{VM 이름}-{yyyymmdd-HHMMSS}`다.
    let name = got["name"].as_str().unwrap();
    let stamp = name.strip_prefix("web-").expect(name);
    assert_eq!(stamp.len(), 15, "{name}");
    assert_eq!(&stamp[8..9], "-", "{name}");
    assert!(
        stamp
            .chars()
            .filter(|c| *c != '-')
            .all(|c| c.is_ascii_digit()),
        "{name}"
    );
    finish_tasks(&env).await;

    // 이름을 주면 그 이름이다. 이름이나 id로 읽는다.
    let named = backup_ok(
        &admin,
        "web",
        json!({ "pool": "backups", "name": "before" }),
    )
    .await;
    let (status, got) = admin.get("/backups/before").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], named.to_string());
    assert_error(
        &admin.get("/backups/nope").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    finish_tasks(&env).await;

    // 같은 이름은 백업마다 하나다(VM이 달라도).
    volume(&env, "local", "db-root", ImageFormat::Qcow2).await;
    let db = vm_with_disks(&admin, "db", &["db-root"]).await;
    assert_error(
        &backup(&admin, "db", json!({ "pool": "backups", "name": "before" })).await,
        StatusCode::CONFLICT,
        "name_taken",
    );
    backup_ok(&admin, "db", json!({ "pool": "backups", "name": "db-1" })).await;

    // 목록은 전부, `vm`(id나 이름)으로 거른다.
    let (status, list) = admin.get("/backups").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
    assert!(list["revision"].as_u64().unwrap() > 0);
    for key in ["web".to_string(), vm.to_string()] {
        let (_, list) = admin.get(&format!("/backups?vm={key}")).await;
        let names: Vec<_> = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["spec"]["vm"].clone())
            .collect();
        assert_eq!(names, vec![json!(vm), json!(vm)], "{list}");
    }
    let (_, list) = admin.get(&format!("/backups?vm={db}")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");

    // 지운 VM의 백업은 그 VM의 이름으로도 찾는다.
    finish_tasks(&env).await;
    assert_eq!(admin.delete("/vms/db").await.0, StatusCode::NO_CONTENT);
    let (_, list) = admin.get("/backups?vm=db").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list["items"][0]["name"], "db-1");
    let (_, list) = admin.get("/backups?vm=nobody").await;
    assert_eq!(list["items"], json!([]));
}

#[tokio::test]
async fn backup_validates_the_request() {
    let env = env().await;
    let admin = env.admin().await;
    let vm = setup(&env, &admin).await;
    dir_pool(&env, "plain", false, false).await;

    assert_error(
        &backup(&admin, "nope", json!({ "pool": "backups" })).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    for body in [
        json!({ "pool": "missing" }),
        // 백업 풀이 아니다.
        json!({ "pool": "plain" }),
        json!({ "pool": "backups", "name": "Bad Name" }),
        json!({ "pool": "backups", "name": Uuid::now_v7().to_string() }),
        json!({ "pool": "backups", "pol": "x" }),
    ] {
        let got = backup(&admin, "web", body).await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
    }
    let got = backup(&admin, "web", json!({ "pool": "plain" })).await;
    assert!(
        got.1["message"].as_str().unwrap().contains("backups"),
        "{}",
        got.1
    );

    // 시작 중인 VM은 받지 않는다.
    set_vm_status(&env, vm, VmPhase::Starting, Some(env.node)).await;
    assert_error(
        &backup(&admin, "web", json!({ "pool": "backups" })).await,
        StatusCode::CONFLICT,
        "vm_busy",
    );
    // 실행 중이면 VM의 노드에서 돈다.
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let (status, task) = backup(&admin, "web", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    finish_tasks(&env).await;

    // 백업 풀이 VM의 노드에 없다.
    let n2 = other_node(&env, "n2").await;
    let far = Resource::new(
        "far",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("far"),
            },
            nodes: vec![n2],
            shared: false,
            backups: true,
        },
    );
    env.cluster.create(&far).await.unwrap();
    mark_pool_ready(&env.cluster, n2, "far").await;
    assert_error(
        &backup(&admin, "web", json!({ "pool": "far" })).await,
        StatusCode::CONFLICT,
        "pool_not_on_node",
    );
}

/// `incremental`은 백업 스펙에 남고(없으면 빠진다), 증분 백업을 모르는 노드가 있으면 409
/// `cluster_upgrading`이다. 전체 백업은 그대로 받는다.
#[tokio::test]
async fn incremental_backups_need_every_node_to_support_them() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;

    let id = backup_ok(
        &admin,
        "web",
        json!({ "pool": "backups", "incremental": true }),
    )
    .await;
    let (_, got) = admin.get(&format!("/backups/{id}")).await;
    assert_eq!(got["spec"]["incremental"], true, "{got}");
    finish_tasks(&env).await;
    let id = backup_ok(&admin, "web", json!({ "pool": "backups", "name": "full" })).await;
    let (_, got) = admin.get(&format!("/backups/{id}")).await;
    assert!(got["spec"].get("incremental").is_none(), "{got}");
    finish_tasks(&env).await;

    // 기능 목록이 빈 옛 노드
    other_node(&env, "old").await;
    let got = backup(
        &admin,
        "web",
        json!({ "pool": "backups", "name": "incr", "incremental": true }),
    )
    .await;
    assert_eq!(
        got.1["message"],
        "some nodes do not support incremental backup yet (old); upgrade them first",
        "{}",
        got.1
    );
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    let (_, list) = admin.get("/backups").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2, "{list}");
    backup_ok(
        &admin,
        "web",
        json!({ "pool": "backups", "name": "full2", "incremental": false }),
    )
    .await;
}

#[tokio::test]
async fn running_vms_need_disks_that_can_be_snapshotted() {
    let env = env().await;
    let admin = env.admin().await;
    dir_pool(&env, "backups", false, true).await;
    dir_pool(&env, "local", false, false).await;
    let san = PoolBackend::LvmShared {
        vg: "san".into(),
        iscsi: None,
    };
    pool_with(&env, "san", san, true, false).await;
    volume(&env, "local", "root", ImageFormat::Qcow2).await;
    volume(&env, "local", "raw", ImageFormat::Raw).await;
    volume(&env, "san", "lun", ImageFormat::Raw).await;
    let mixed = vm_with_disks(&admin, "mixed", &["root", "raw"]).await;
    let shared = vm_with_disks(&admin, "shared", &["lun"]).await;

    for (vm, name, disk) in [(mixed, "mixed", "raw"), (shared, "shared", "lun")] {
        set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
        let got = backup(&admin, name, json!({ "pool": "backups" })).await;
        assert_error(&got, StatusCode::CONFLICT, "vm_running");
        let message = got.1["message"].as_str().unwrap();
        assert!(message.contains(disk), "{message}");
        assert!(message.contains("stop it to back it up"), "{message}");
        // 멈춘 VM은 된다.
        set_vm_status(&env, vm, VmPhase::Stopped, None).await;
        backup_ok(&admin, name, json!({ "pool": "backups" })).await;
    }
}

#[tokio::test]
async fn stopped_vms_without_a_node_run_where_the_disks_and_the_backup_pool_are() {
    let env = env().await;
    let admin = env.admin().await;
    // 이름 순으로 이 노드(`node1`)보다 앞선 노드
    let a = other_node(&env, "a-node").await;
    dir_pool(&env, "backups", true, true).await;
    dir_pool(&env, "shared", true, false).await;
    dir_pool(&env, "local", false, false).await;
    mark_pool_ready(&env.cluster, a, "backups").await;
    mark_pool_ready(&env.cluster, a, "shared").await;
    volume(&env, "shared", "s0", ImageFormat::Qcow2).await;
    volume(&env, "local", "l0", ImageFormat::Qcow2).await;
    vm_with_disks(&admin, "on-shared", &["s0"]).await;
    vm_with_disks(&admin, "on-local", &["l0"]).await;

    // 공유 풀의 디스크는 두 노드 모두에서 읽힌다. 이름이 앞선 노드다.
    let (status, task) = backup(&admin, "on-shared", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], a.to_string());
    // 로컬 풀의 디스크는 파일이 있는 노드에서만 읽힌다.
    let (status, task) = backup(&admin, "on-local", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    finish_tasks(&env).await;

    // 디스크가 있는 노드에서 백업 풀을 쓸 수 없으면 돌 노드가 없다.
    let only_a = Resource::new(
        "only-a",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("only-a"),
            },
            nodes: vec![a],
            shared: true,
            backups: true,
        },
    );
    env.cluster.create(&only_a).await.unwrap();
    mark_pool_ready(&env.cluster, a, "only-a").await;
    assert_error(
        &backup(&admin, "on-local", json!({ "pool": "only-a" })).await,
        StatusCode::CONFLICT,
        "no_node",
    );
    backup_ok(
        &admin,
        "on-shared",
        json!({ "pool": "only-a", "name": "on-a" }),
    )
    .await;
}

#[tokio::test]
async fn an_unfinished_backup_holds_the_vm() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    backup_ok(&admin, "web", json!({ "pool": "backups" })).await;

    // 디스크를 복사하는 동안 켜거나 지우지 못한다.
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
    assert!(
        body["message"].as_str().unwrap().contains("backed up"),
        "{body}"
    );
    assert_error(
        &admin.delete("/vms/web").await,
        StatusCode::CONFLICT,
        "vm_busy",
    );
    finish_tasks(&env).await;
    assert_eq!(
        admin.post("/vms/web/actions/start", Value::Null).await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn delete_records_a_task_where_the_backup_pool_is() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let id = backup_ok(&admin, "web", json!({ "pool": "backups", "name": "b1" })).await;

    // 만드는 중에는 지우지 못한다.
    assert_error(
        &admin.delete("/backups/b1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    finish_tasks(&env).await;
    // 실패한 백업도 지운다.
    set_phase(&env, id, BackupPhase::Failed).await;
    let (status, task) = admin.delete(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"], json!({ "kind": "backup-delete" }));
    assert_eq!(task["spec"]["target"], id.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
    // 리소스는 Task가 지운다.
    assert_eq!(admin.get("/backups/b1").await.0, StatusCode::OK);
    // 지우는 중에 다시 지우거나 복원하지 못한다.
    assert_error(
        &admin.delete("/backups/b1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    set_phase(&env, id, BackupPhase::Ready).await;
    assert_error(
        &admin
            .post(
                "/backups/b1/restore",
                json!({ "name": "web-2", "pool": "local" }),
            )
            .await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    assert_error(
        &admin.delete("/backups/nope").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// 꺼진 VM `web`의 Ready 백업 `b1`
async fn ready_backup(env: &Env, admin: &Client) -> Uuid {
    setup(env, admin).await;
    let id = backup_ok(admin, "web", json!({ "pool": "backups", "name": "b1" })).await;
    finish_tasks(env).await;
    set_phase(env, id, BackupPhase::Ready).await;
    id
}

async fn restore(admin: &Client, backup: &str, body: Value) -> (StatusCode, Value) {
    admin
        .post(&format!("/backups/{backup}/restore"), body)
        .await
}

#[tokio::test]
async fn restore_records_a_task_for_a_new_vm() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;
    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();

    let (status, task) = restore(&admin, "b1", json!({ "name": "web-2", "pool": "local" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "backup-restore", "backup": id, "name": "web-2", "pool": "local" })
    );
    assert_eq!(task["spec"]["node"], env.node.to_string());
    // 대상은 미리 정한 새 VM id다. VM은 Task가 만든다.
    let target: Uuid = task["spec"]["target"].as_str().unwrap().parse().unwrap();
    assert_ne!(target, vm.id);
    assert_ne!(target, id);
    assert!(env.cluster.get::<VmSpec>(target).unwrap().is_none());

    // 복원하는 중에는 백업을 지우지 못한다.
    assert_error(
        &admin.delete("/backups/b1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    // 복원은 여럿 겹쳐도 된다. 노드는 이름이나 id로 고른다.
    for node in ["node1".to_string(), env.node.to_string()] {
        let (status, task) = restore(
            &admin,
            &id.to_string(),
            json!({ "name": "web-3", "pool": "backups", "node": node }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        assert_eq!(task["spec"]["node"], env.node.to_string());
    }
    finish_tasks(&env).await;
    // 원래 VM을 지워도 백업에서 되살린다.
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    let (status, task) = restore(&admin, "b1", json!({ "name": "web", "pool": "local" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

#[tokio::test]
async fn restore_validates_the_request() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;

    assert_error(
        &restore(&admin, "nope", json!({ "name": "x", "pool": "local" })).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    // 이름은 지금 VM과 겹치면 안 된다.
    assert_error(
        &restore(&admin, "b1", json!({ "name": "web", "pool": "local" })).await,
        StatusCode::CONFLICT,
        "name_taken",
    );
    for body in [
        json!({ "name": "Bad Name", "pool": "local" }),
        json!({ "name": "web-2", "pool": "missing" }),
        json!({ "name": "web-2" }),
        json!({ "name": "web-2", "pool": "local", "extra": 1 }),
    ] {
        let got = restore(&admin, "b1", body).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    assert_error(
        &restore(
            &admin,
            "b1",
            json!({ "name": "web-2", "pool": "local", "node": "nope" }),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // 만드는 중이거나 실패했거나 아직 받는 중인 백업은 되살리지 못한다.
    for phase in [
        BackupPhase::Pending,
        BackupPhase::Failed,
        BackupPhase::Receiving,
    ] {
        set_phase(&env, id, phase).await;
        assert_error(
            &restore(&admin, "b1", json!({ "name": "web-2", "pool": "local" })).await,
            StatusCode::CONFLICT,
            "backup_not_ready",
        );
    }
    set_phase(&env, id, BackupPhase::Ready).await;

    // 고른 노드에서 백업 풀이나 대상 풀을 쓸 수 없다.
    let n2 = other_node(&env, "n2").await;
    let down = PoolState {
        ready: false,
        error: Some("not mounted".into()),
        ..Default::default()
    };
    set_pool_state(&env.cluster, n2, "backups", down).await;
    assert_error(
        &restore(
            &admin,
            "b1",
            json!({ "name": "web-2", "pool": "local", "node": "n2" }),
        )
        .await,
        StatusCode::CONFLICT,
        "pool_unavailable",
    );
    mark_pool_ready(&env.cluster, n2, "backups").await;
    mark_pool_ready(&env.cluster, n2, "local").await;
    let (status, task) = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": n2 }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], n2.to_string());
    finish_tasks(&env).await;

    // 두 풀을 함께 쓰는 노드가 없다.
    let far = Resource::new(
        "far",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("far"),
            },
            nodes: vec![Uuid::now_v7()],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&far).await.unwrap();
    assert_error(
        &restore(&admin, "b1", json!({ "name": "web-2", "pool": "far" })).await,
        StatusCode::CONFLICT,
        "no_node",
    );
}

#[tokio::test]
async fn restore_needs_the_networks_and_security_groups_of_the_backup() {
    let env = env().await;
    let admin = env.admin().await;
    dir_pool(&env, "backups", false, true).await;
    dir_pool(&env, "local", false, false).await;
    let mut vm_spec: VmSpec = serde_json::from_value(vm_request("web")["spec"].clone()).unwrap();
    vm_spec.nics = serde_json::from_value(json!([
        { "network": "lan", "model": "virtio", "security_groups": ["web"] }
    ]))
    .unwrap();
    let mut backup: Backup = Resource::new(
        "b1",
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            pool: "backups".into(),
            vm_spec,
            job: None,
            origin: None,
            incremental: false,
        },
    );
    backup.status.phase = BackupPhase::Ready;
    env.cluster.create(&backup).await.unwrap();
    env.cluster
        .update_status::<BackupSpec>(backup.id, &backup.status)
        .await
        .unwrap();
    let body = json!({ "name": "web-2", "pool": "local" });

    let got = restore(&admin, "b1", body.clone()).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("network \"lan\""),
        "{}",
        got.1
    );
    let (status, net) = admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{net}");
    let got = restore(&admin, "b1", body.clone()).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("security group \"web\""),
        "{}",
        got.1
    );
    let (status, sg) = admin
        .post("/security-groups", json!({ "name": "web", "spec": {} }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sg}");
    let (status, task) = restore(&admin, "b1", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

/// 백업의 상태를 직접 쓴다: Ready, 파일을 쓴 노드 `node`
async fn set_stored(env: &Env, id: Uuid, node: Option<Uuid>) {
    let status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(1),
        node,
        ..Default::default()
    };
    env.cluster
        .update_status::<BackupSpec>(id, &status)
        .await
        .unwrap();
}

/// 공유가 아닌 백업 풀의 백업은 파일을 쓴 노드(`status.node`)에만 있다. 지우기와 복원은 그 노드에서
/// 돌고, 다른 노드를 고르거나 그 노드에서 대상 풀을 쓸 수 없으면 409다.
#[tokio::test]
async fn backups_in_local_pools_are_handled_on_their_node() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;
    let n2 = other_node(&env, "n2").await;
    mark_pool_ready(&env.cluster, n2, "backups").await;
    mark_pool_ready(&env.cluster, n2, "local").await;
    set_stored(&env, id, Some(n2)).await;

    // 지우기와 복원은 파일이 있는 n2에서 돈다(이 노드가 백업 풀을 쓸 수 있어도).
    let (status, task) = admin.delete("/backups/b1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], n2.to_string());
    finish_tasks(&env).await;
    let (status, task) = restore(&admin, "b1", json!({ "name": "web-2", "pool": "local" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], n2.to_string());
    finish_tasks(&env).await;
    let (status, task) = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": "n2" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    finish_tasks(&env).await;

    // 다른 노드에서는 되살리지 못한다.
    let got = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": "node1" }),
    )
    .await;
    assert_error(&got, StatusCode::CONFLICT, "backup_on_other_node");
    assert_eq!(got.1["message"], "backup \"b1\" is stored on node \"n2\"");

    // 대상 풀이 n2에 없다.
    let here = Resource::new(
        "here",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("here"),
            },
            nodes: vec![env.node],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&here).await.unwrap();
    env.pool_ready("here").await;
    assert_error(
        &restore(&admin, "b1", json!({ "name": "web-2", "pool": "here" })).await,
        StatusCode::CONFLICT,
        "pool_not_on_node",
    );

    // 이 노드에 있는 백업은 이 노드에서 다룬다.
    set_stored(&env, id, Some(env.node)).await;
    let (status, task) = admin.delete("/backups/b1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    finish_tasks(&env).await;
    let got = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": "n2" }),
    )
    .await;
    assert_error(&got, StatusCode::CONFLICT, "backup_on_other_node");
    assert_eq!(
        got.1["message"],
        "backup \"b1\" is stored on node \"node1\""
    );
}

async fn set_online(env: &Env, node: Uuid, online: bool) {
    let mut n = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    n.status.online = online;
    env.cluster
        .update_status::<NodeSpec>(node, &n.status)
        .await
        .unwrap();
}

/// 파일이 있는 노드나 고른 노드가 오프라인이면 409 `node_offline`이다.
#[tokio::test]
async fn offline_nodes_do_not_take_backup_tasks() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;
    let n2 = other_node(&env, "n2").await;
    mark_pool_ready(&env.cluster, n2, "backups").await;
    mark_pool_ready(&env.cluster, n2, "local").await;
    set_online(&env, n2, false).await;

    let got = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": "n2" }),
    )
    .await;
    assert_error(&got, StatusCode::CONFLICT, "node_offline");
    assert_eq!(got.1["message"], "node \"n2\" is offline");

    set_stored(&env, id, Some(n2)).await;
    assert_error(
        &restore(&admin, "b1", json!({ "name": "web-2", "pool": "local" })).await,
        StatusCode::CONFLICT,
        "node_offline",
    );
    assert_error(
        &admin.delete("/backups/b1").await,
        StatusCode::CONFLICT,
        "node_offline",
    );
}

/// 풀이 없어진 백업은 이 노드의 Task가 리소스만 지운다. 풀은 있는데 파일을 지울 노드가 없으면 409이고,
/// `force`면 리소스만 바로 지운다(파일은 남는다).
#[tokio::test]
async fn backups_without_a_usable_pool_can_be_forgotten() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;
    let n2 = other_node(&env, "n2").await;

    // 파일이 있는 노드에서 풀이 준비되지 않았다.
    let down = PoolState {
        ready: false,
        error: Some("not mounted".into()),
        ..Default::default()
    };
    set_pool_state(&env.cluster, n2, "backups", down).await;
    set_stored(&env, id, Some(n2)).await;
    let got = admin.delete("/backups/b1").await;
    assert_error(&got, StatusCode::CONFLICT, "pool_unavailable");
    let got = admin.delete("/backups/b1?force=false").await;
    assert_error(&got, StatusCode::CONFLICT, "pool_unavailable");
    assert!(env.cluster.get::<BackupSpec>(id).unwrap().is_some());
    let (status, body) = admin.delete("/backups/b1?force=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("files are left in storage pool \"backups\""),
        "{message}"
    );
    assert!(env.cluster.get::<BackupSpec>(id).unwrap().is_none());
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    assert!(tasks.iter().all(|t| t.spec.target != id), "{tasks:?}");

    // 풀째 없어졌으면 이 노드의 Task가 리소스만 지운다.
    let b2 = backup_ok(&admin, "web", json!({ "pool": "backups", "name": "b2" })).await;
    finish_tasks(&env).await;
    set_stored(&env, b2, Some(n2)).await;
    let pool = env
        .cluster
        .get_by_name::<StoragePoolSpec>("backups")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<StoragePoolSpec>(pool.id, None)
        .await
        .unwrap();
    let (status, task) = admin.delete("/backups/b2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["target"], b2.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
}

/// 지운 풀에는 백업을 만들지 못한다.
#[tokio::test]
async fn backups_need_a_pool_that_still_exists() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    dir_pool(&env, "gone", false, true).await;
    assert_eq!(
        admin.delete("/storage-pools/gone").await.0,
        StatusCode::NO_CONTENT
    );
    let got = backup(&admin, "web", json!({ "pool": "gone" })).await;
    assert!(got.0.is_client_error(), "{got:?}");
    assert!(env.cluster.list::<BackupSpec>().unwrap().1.is_empty());
}

/// 백업을 먼저 만들면(끝나지 않았어도) 그 풀은 지우지 못한다.
#[tokio::test]
async fn pools_with_a_new_backup_cannot_be_deleted() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    backup_ok(&admin, "web", json!({ "pool": "backups" })).await;
    let got = admin.delete("/storage-pools/backups").await;
    assert_error(&got, StatusCode::CONFLICT, "pool_in_use");
}

/// 실행 중인 VM의 디스크가 서로 다른 스냅샷 방식을 쓰면 스냅샷처럼 거절한다.
#[tokio::test]
async fn running_vms_with_mixed_snapshot_methods_are_refused() {
    let env = env().await;
    let admin = env.admin().await;
    dir_pool(&env, "backups", false, true).await;
    dir_pool(&env, "local", false, false).await;
    let thin = PoolBackend::LvmThin {
        vg: "vg0".into(),
        thinpool: "tp".into(),
    };
    pool_with(&env, "thin", thin, false, false).await;
    volume(&env, "local", "root", ImageFormat::Qcow2).await;
    volume(&env, "thin", "lv", ImageFormat::Raw).await;
    let vm = vm_with_disks(&admin, "web", &["root", "lv"]).await;

    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let got = backup(&admin, "web", json!({ "pool": "backups" })).await;
    assert_error(&got, StatusCode::CONFLICT, "snapshot_unsupported");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("different snapshot methods"),
        "{}",
        got.1
    );
    assert!(env.cluster.list::<BackupSpec>().unwrap().1.is_empty());
    // 멈춘 VM은 된다.
    set_vm_status(&env, vm, VmPhase::Stopped, None).await;
    backup_ok(&admin, "web", json!({ "pool": "backups" })).await;
}

/// `?vm=이름`은 지금 그 이름인 VM의 백업과 그 이름으로 남은(지운 VM의) 백업을 함께 준다.
#[tokio::test]
async fn listing_by_vm_name_includes_deleted_vms_of_that_name() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    backup_ok(&admin, "web", json!({ "pool": "backups", "name": "old" })).await;
    finish_tasks(&env).await;
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    let web = vm_with_disks(&admin, "web", &["root"]).await;
    backup_ok(&admin, "web", json!({ "pool": "backups", "name": "new" })).await;

    let names = |list: &Value| {
        let mut names: Vec<String> = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    };
    let (_, list) = admin.get("/backups?vm=web").await;
    assert_eq!(names(&list), ["new", "old"], "{list}");
    // id는 그 VM의 백업만이다.
    let (_, list) = admin.get(&format!("/backups?vm={web}")).await;
    assert_eq!(names(&list), ["new"], "{list}");
}

#[tokio::test]
async fn pools_holding_backups_stay() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;

    let got = admin.delete("/storage-pools/backups").await;
    assert_error(&got, StatusCode::CONFLICT, "pool_in_use");
    assert_eq!(got.1["message"], "pool \"backups\" holds backups");
    let got = admin
        .patch("/storage-pools/backups", json!({ "backups": false }), None)
        .await;
    assert_error(&got, StatusCode::CONFLICT, "pool_in_use");
    assert_eq!(got.1["message"], "pool \"backups\" holds backups");
    // 공유가 아닌 풀의 백업은 쓴 노드에만 있으므로 공유 여부도 바꾸지 못한다.
    let got = admin
        .patch("/storage-pools/backups", json!({ "shared": true }), None)
        .await;
    assert_error(&got, StatusCode::CONFLICT, "pool_in_use");
    assert_eq!(got.1["message"], "pool \"backups\" holds backups");
    // 그대로 두는 PATCH는 된다.
    let (status, body) = admin
        .patch(
            "/storage-pools/backups",
            json!({ "shared": false, "backups": true }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["backups"], true);

    env.cluster.delete::<BackupSpec>(id, None).await.unwrap();
    let (status, body) = admin
        .patch("/storage-pools/backups", json!({ "backups": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        admin.delete("/storage-pools/backups").await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn viewers_read_backups_and_operators_manage_them() {
    let env = env().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    let id = ready_backup(&env, &admin).await;

    assert_eq!(viewer.get("/backups").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/backups/b1").await.0, StatusCode::OK);
    for got in [
        backup(&viewer, "web", json!({ "pool": "backups" })).await,
        restore(&viewer, "b1", json!({ "name": "web-2", "pool": "local" })).await,
        viewer.delete("/backups/b1").await,
    ] {
        assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
    }

    let (status, body) = backup(&operator, "web", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;
    let (status, body) =
        restore(&operator, "b1", json!({ "name": "web-2", "pool": "local" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;
    let (status, body) = operator.delete(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}
