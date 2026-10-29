//! 스냅샷 Task 실행: 실행 중인 VM(Mock 하이퍼바이저)의 만들기·삭제, 부모와 현재 위치, 실패 정리,
//! 디스크 없는 VM 되돌리기, 시작할 때의 고아 정리, 실행 중인 VM의 LVM 스냅샷과 크기 늘리기. 디스크 파일을 다루는 경우는 `snapshot_disks`.

mod common;

use std::sync::{Arc, Mutex};

use common::{Client, Env, FakeStorage, vm_request};
use opl_driver::{DriverError, Hypervisor, RuntimeStatus, uefi_vars_path};
use opl_qemu::QmpError;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{Arch, PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 풀 `local`과 바로 Ready인 qcow2 볼륨 `root`를 저장소에 직접 만든다(파일은 없다. Mock
/// 하이퍼바이저는 파일을 열지 않는다).
async fn pool_and_root(env: &Env) {
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir {
                path: env.dir.path().join("pool"),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    let mut volume = Resource::new(
        "root",
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

/// 디스크 `disks`를 붙인 VM을 만들고 켠다.
async fn running_vm(admin: &Client, name: &str, disks: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// 스냅샷을 요청하고 Task가 끝날 때까지 기다린다.
async fn snapshot(admin: &Client, vm: &str, body: Value) -> Value {
    let (status, task) = admin.post(&format!("/vms/{vm}/snapshots"), body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn find(env: &Env, vm: Uuid, name: &str) -> Option<Snapshot> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items
        .into_iter()
        .find(|s| s.spec.vm == vm && s.spec.name == name)
}

fn get(env: &Env, vm: Uuid, name: &str) -> Snapshot {
    find(env, vm, name).unwrap_or_else(|| panic!("snapshot {name} not found"))
}

fn position(env: &Env, vm: Uuid) -> Option<Uuid> {
    env.cluster
        .get::<VmSpec>(vm)
        .unwrap()
        .unwrap()
        .status
        .snapshot
}

fn vars_copy(env: &Env, vm: Uuid, snapshot: Uuid) -> std::path::PathBuf {
    env.dir
        .path()
        .join(format!("state/vm/{vm}/snapshots/{snapshot}.vars"))
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn running_vm_snapshots_chain_and_move_the_position() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    // QEMU 드라이버가 처음 시작할 때 만드는 UEFI 변수 파일
    let vars = uefi_vars_path(&env.dir.path().join("state"), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars-a").unwrap();

    succeeded(&snapshot(&admin, "web", json!({ "name": "a" })).await);
    let a = get(&env, vm, "a");
    assert_eq!(a.status.phase, SnapshotPhase::Ready);
    assert!(a.status.created_at.is_some());
    assert!(a.spec.memory);
    assert_eq!(a.spec.parent, None);
    assert_eq!(env.hv.snapshots(vm), vec![(a.id.to_string(), true)]);
    assert_eq!(position(&env, vm), Some(a.id));
    assert_eq!(std::fs::read(vars_copy(&env, vm, a.id)).unwrap(), b"vars-a");

    // 줄 선 두 만들기: API는 둘 다 a를 부모로 기록하지만, 실행할 때 앞의 것을 부모로 삼는다.
    let (_, tb) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "b", "memory": false }),
        )
        .await;
    let (_, tc) = admin
        .post("/vms/web/snapshots", json!({ "name": "c" }))
        .await;
    succeeded(&admin.wait_task(&tb).await);
    succeeded(&admin.wait_task(&tc).await);
    let (b, c) = (get(&env, vm, "b"), get(&env, vm, "c"));
    assert_eq!(b.spec.parent, Some(a.id));
    assert_eq!(c.spec.parent, Some(b.id));
    assert_eq!(
        env.hv.snapshots(vm),
        vec![
            (a.id.to_string(), true),
            (b.id.to_string(), false),
            (c.id.to_string(), true)
        ]
    );
    assert_eq!(position(&env, vm), Some(c.id));
}

#[tokio::test]
async fn failed_snapshot_is_removed_but_a_timed_out_one_keeps_its_tag() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;

    // 작업이 오류로 끝났다: 만든 태그와 리소스를 지운다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobFailed {
            id: "j".into(),
            error: "no space".into(),
        }));
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "a").is_none());
    assert!(env.hv.snapshots(vm).is_empty());
    assert_eq!(position(&env, vm), None);

    // 시간 초과: 작업이 아직 돌 수 있으므로 태그는 두고 리소스만 지운다. 이름은 다시 쓸 수 있다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobTimeout {
            id: "j".into(),
            timeout: std::time::Duration::from_secs(600),
        }));
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    let id = task["spec"]["op"]["snapshot"].as_str().unwrap();
    assert!(error.contains(id), "{error}");
    assert!(find(&env, vm, "a").is_none());
    assert_eq!(env.hv.snapshots(vm), vec![(id.to_string(), true)]);
    succeeded(&snapshot(&admin, "web", json!({ "name": "a" })).await);
}

#[tokio::test]
async fn delete_reparents_children_and_moves_the_position() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    let vars = uefi_vars_path(&env.dir.path().join("state"), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars").unwrap();
    for name in ["a", "b", "c"] {
        succeeded(&snapshot(&admin, "web", json!({ "name": name })).await);
    }
    let (a, b, c) = (get(&env, vm, "a"), get(&env, vm, "b"), get(&env, vm, "c"));

    let (status, task) = admin.delete("/vms/web/snapshots/b").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert!(find(&env, vm, "b").is_none());
    assert!(!vars_copy(&env, vm, b.id).exists());
    assert_eq!(get(&env, vm, "c").spec.parent, Some(a.id));
    assert_eq!(position(&env, vm), Some(c.id));

    // 현재 위치를 지우면 부모로 옮긴다.
    let (_, task) = admin.delete("/vms/web/snapshots/c").await;
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(position(&env, vm), Some(a.id));
    assert_eq!(env.hv.snapshots(vm), vec![(a.id.to_string(), true)]);
    assert!(vars_copy(&env, vm, a.id).exists());

    // 디스크에서 지우지 못하면 리소스와 관계는 그대로다. 다시 하면 된다.
    env.hv.fail_next_snapshot(DriverError::Timeout("qmp"));
    let (_, task) = admin.delete("/vms/web/snapshots/a").await;
    assert_eq!(admin.wait_task(&task).await["status"]["state"], "failed");
    assert_eq!(position(&env, vm), Some(a.id));
    assert!(find(&env, vm, "a").is_some());
    let (_, task) = admin.delete("/vms/web/snapshots/a").await;
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(position(&env, vm), None);
    assert!(env.hv.snapshots(vm).is_empty());
}

#[tokio::test]
async fn revert_restores_the_spec_of_a_diskless_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "bare", &[]).await;
    // 디스크가 없으면 설정만 담는다.
    succeeded(&snapshot(&admin, "bare", json!({ "name": "cfg" })).await);
    let cfg = get(&env, vm, "cfg");
    assert!(!cfg.spec.memory);
    assert!(env.hv.snapshots(vm).is_empty());

    let node = env.cluster.get::<VmSpec>(vm).unwrap().unwrap().spec.node;

    let (_, current) = admin.get("/vms/bare").await;
    let mut spec = current["spec"].clone();
    spec["memory_mib"] = json!(1024);
    admin.put("/vms/bare", json!({ "spec": spec }), None).await;
    let (status, task) = admin
        .post("/vms/bare/snapshots/cfg/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);

    // 메모리가 없는 스냅샷이므로 꺼진 채 남는다. 노드는 그대로다.
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.memory_mib, 256);
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.spec.node, node);
    assert_eq!(got.status.phase, VmPhase::Stopped);
    assert_eq!(got.status.snapshot, Some(cfg.id));
    assert_eq!(env.pid(vm).await, None);
}

#[tokio::test]
async fn memory_revert_does_not_place_the_vm_on_a_node_of_another_architecture() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "mem" })).await);
    assert!(get(&env, vm, "mem").spec.memory);
    // 끄고 배치를 푼다. 그 뒤 이 노드가 다른 arch로 기록됐다.
    succeeded(&admin.act("web", "stop").await);
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = Value::Null;
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.arch = Some(Arch::X86_64);
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();

    let (status, task) = admin
        .post("/vms/web/snapshots/mem/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("machine type"),
        "{task}"
    );
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.node, None);
    assert_eq!(got.spec.power, PowerState::Stopped);
}

#[tokio::test]
async fn snapshot_tasks_do_not_supersede_a_reboot() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[]).await;
    let (_, reboot) = admin.post("/vms/web/actions/reboot", Value::Null).await;
    // 재부팅이 끄는 동안(게스트는 ACPI를 무시하고, acpi_timeout 뒤 강제로 꺼진다) 같은 VM에
    // 스냅샷 Task가 만들어진다. 다른 노드에 배정해 여기서는 실행하지 않는다.
    let task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::SnapshotCreate {
                snapshot: Uuid::now_v7(),
            },
            target: vm,
            node: Uuid::now_v7(),
        },
    );
    env.cluster.create(&task).await.unwrap();
    succeeded(&admin.wait_task(&reboot).await);
}

#[tokio::test]
async fn unfinished_snapshots_of_this_node_are_removed_at_startup() {
    let ids = Arc::new(Mutex::new(Vec::new()));
    let slot = ids.clone();
    let other = Uuid::now_v7();
    let env = Env::start_with(
        |_| {},
        move |cluster| async move {
            let me = cluster.id().0;
            let mut ids = Vec::new();
            for (name, node) in [("mine", me), ("theirs", other)] {
                let mut vm: opl_types::vm::Vm = Resource::new(name, vm_spec(node));
                vm.spec.power = PowerState::Stopped;
                cluster.create(&vm).await.unwrap();
                // Task를 만들기 전에 API가 있던 opld가 죽었다.
                let s = pending(vm.id, &vm.spec, "orphan");
                cluster.create(&s).await.unwrap();
                ids.push(s.id);
            }
            // 이 노드가 만들다 opld가 죽었다. Task는 실패로 끝나고 리소스를 지운다.
            let vm = cluster.get_by_name::<VmSpec>("mine").unwrap().unwrap();
            let s = pending(vm.id, &vm.spec, "interrupted");
            cluster.create(&s).await.unwrap();
            ids.push(s.id);
            let mut task: Task = Resource::new(
                "",
                TaskSpec {
                    op: TaskOp::SnapshotCreate { snapshot: s.id },
                    target: vm.id,
                    node: me,
                },
            );
            task.name = task.id.to_string();
            task.status = TaskStatus {
                state: TaskState::Running,
                error: None,
            };
            cluster.create(&task).await.unwrap();
            // 이 노드에서 돌던 꺼진 VM을 다른 노드로 옮겼다. 디스크는 그 노드에 있다.
            let mut vm: opl_types::vm::Vm = Resource::new("moved", vm_spec(other));
            vm.spec.power = PowerState::Stopped;
            vm.status.node = Some(me);
            cluster.create(&vm).await.unwrap();
            let s = pending(vm.id, &vm.spec, "orphan");
            cluster.create(&s).await.unwrap();
            ids.push(s.id);
            *slot.lock().unwrap() = ids;
        },
    )
    .await;
    let ids = ids.lock().unwrap().clone();
    // 실행기는 고아를 지운 뒤에 뜬다.
    assert!(env.cluster.get::<SnapshotSpec>(ids[0]).unwrap().is_none());
    assert!(env.cluster.get::<SnapshotSpec>(ids[1]).unwrap().is_some());
    assert!(env.cluster.get::<SnapshotSpec>(ids[3]).unwrap().is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.cluster.get::<SnapshotSpec>(ids[2]).unwrap().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "interrupted snapshot was kept"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 파일 없는 스토리지로 실행기를 다시 띄운다. 볼륨 `root`·`data`는 1 MiB로 알린다.
async fn with_fake_storage(env: &mut Env) -> Arc<FakeStorage> {
    let storage = Arc::new(FakeStorage::default());
    for name in ["root", "data"] {
        if let Some(v) = env.cluster.get_by_name::<VolumeSpec>(name).unwrap() {
            storage.set_size(v.id, v.spec.size_bytes);
        }
    }
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    storage
}

/// 바로 Ready인 qcow2 볼륨을 저장소에 직접 만든다.
async fn ready_volume(env: &Env, name: &str) -> Uuid {
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
    volume.id
}

/// 다른 노드에 배정한 Task를 저장소에 직접 만든다. 여기서는 실행하지 않는다.
async fn foreign_task(env: &Env, op: TaskOp, target: Uuid) {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: Uuid::now_v7(),
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
}

#[tokio::test]
async fn revert_blocks_power_changes_and_stops_if_started_meanwhile() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let data = ready_volume(&env, "data").await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root", "data"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "s" })).await);

    storage.pause();
    let (status, revert) = admin.post("/vms/web/snapshots/s/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{revert}");
    storage.reached().await;

    // 되돌리는 동안에는 전원도 스펙도 바꿀 수 없다(강제 정지 포함).
    for path in ["/vms/web/actions/start", "/vms/web/actions/stop?mode=force"] {
        let (status, body) = admin.post(path, Value::Null).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "vm_busy", "{body}");
    }
    let (_, current) = admin.get("/vms/web").await;
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": current["spec"] }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    // 그 확인을 지나친 요청이 켰다. 남은 디스크는 되돌리지 않고 실패한다.
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    let spec = VmSpec {
        power: PowerState::Running,
        ..current.spec.clone()
    };
    env.cluster
        .update_spec(vm, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    storage.resume();
    let task = admin.wait_task(&revert).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let reverted: Vec<Uuid> = storage
        .calls()
        .into_iter()
        .filter(|(call, _)| *call == "revert")
        .map(|(_, id)| id)
        .collect();
    assert_eq!(reverted.len(), 1);
    assert!(!reverted.contains(&data));
}

#[tokio::test]
async fn memory_revert_yields_to_a_later_stop_and_keeps_volume_labels() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "mem" })).await);
    let launches = env.hv.launches().len();

    storage.pause();
    let (_, revert) = admin
        .post("/vms/web/snapshots/mem/revert", Value::Null)
        .await;
    storage.reached().await;
    // 되돌리는 사이 정지 요청이 들어왔고(다른 노드의 API), 볼륨 라벨이 바뀌었고, 파일 크기는
    // 스냅샷 때의 것이다.
    foreign_task(&env, TaskOp::VmStop, vm).await;
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let labels = std::collections::BTreeMap::from([("tier".to_string(), "gold".to_string())]);
    env.cluster
        .update_spec(root.id, Some(root.revision), labels.clone(), &root.spec)
        .await
        .unwrap();
    storage.set_size(root.id, 2 << 20);
    storage.resume();

    // 메모리를 불러오며 켜지 않는다. 정지 요청을 덮으면 안 된다.
    let task = admin.wait_task(&revert).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("superseded"),
        "{task}"
    );
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.phase, VmPhase::Stopped);
    assert_eq!(env.hv.launches().len(), launches);
    // 디스크는 되돌렸으므로 현재 위치와 크기는 스냅샷 때의 것이고, 라벨은 그대로다.
    assert_eq!(position(&env, vm), Some(get(&env, vm, "mem").id));
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!((root.spec.size_bytes, root.labels), (2 << 20, labels));
}

#[tokio::test]
async fn revert_checks_volumes_attached_meanwhile_before_stopping() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "s" })).await);
    succeeded(&admin.act("web", "stop").await);
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["disks"] = json!([]);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    succeeded(&admin.act("web", "start").await);
    let vm = env
        .cluster
        .get_by_name::<VmSpec>("web")
        .unwrap()
        .unwrap()
        .id;
    let pid = env.pid(vm).await;

    // API가 확인한 뒤 Task가 돌기 전에 다른 VM이 그 볼륨을 붙였다.
    env.stop_runner().await;
    let (status, revert) = admin.post("/vms/web/snapshots/s/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{revert}");
    let mut req = vm_request("other");
    req["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.start_runner_with(storage).await;

    let task = admin.wait_task(&revert).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    // 끄기 전에 그만뒀다.
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(env.pid(vm).await, pid);
}

#[tokio::test]
async fn delete_does_not_treat_a_volume_of_another_node_as_gone() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    // 이 노드에 배치된 꺼진 VM
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "stop").await);
    succeeded(&snapshot(&admin, "web", json!({ "name": "s" })).await);

    // 볼륨 파일이 다른 노드에 있다. 여기서 없는 것으로 보고 지우면 그 노드에 태그가 남는다.
    let mut root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    root.status.node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VolumeSpec>(root.id, &root.status)
        .await
        .unwrap();
    let (status, task) = admin.delete("/vms/web/snapshots/s").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "s").is_some());
    assert!(!storage.calls().iter().any(|(call, _)| *call == "delete"));
}

#[tokio::test]
async fn revert_checks_volumes_of_another_node_before_stopping() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "s" })).await);
    let pid = env.pid(vm).await;
    let before = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();

    // 볼륨 파일이 다른 노드에 있다. 이 노드에서는 되돌릴 수 없다.
    let mut root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    root.status.node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VolumeSpec>(root.id, &root.status)
        .await
        .unwrap();
    let (status, revert) = admin.post("/vms/web/snapshots/s/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{revert}");
    let task = admin.wait_task(&revert).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("is on node"),
        "{task}"
    );
    // 끄지도, 스펙을 바꾸지도 않았다.
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec, before.spec);
    assert_eq!(env.pid(vm).await, pid);
    assert!(!storage.calls().iter().any(|(call, _)| *call == "revert"));
}

#[tokio::test]
async fn resize_keeps_volume_labels_written_meanwhile() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;

    storage.pause();
    let (status, task) = admin
        .post("/volumes/root/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let labels = std::collections::BTreeMap::from([("tier".to_string(), "gold".to_string())]);
    env.cluster
        .update_spec(root.id, Some(root.revision), labels.clone(), &root.spec)
        .await
        .unwrap();
    storage.resume();

    succeeded(&admin.wait_task(&task).await);
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!((root.spec.size_bytes, root.labels), (2 << 20, labels));
}

#[tokio::test]
async fn task_failed_by_someone_else_before_it_starts_is_not_run() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;

    storage.pause();
    let (status, first) = admin
        .post("/volumes/root/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    storage.reached().await;
    // 같은 줄에서 앞의 Task를 기다리는 Task를, 시작하기 전에 다른 쪽(리더의 정리 작업)이 실패로 끝낸다.
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let mut second: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeResize {
                size_bytes: 3 << 20,
            },
            target: root.id,
            node: env.node,
        },
    );
    second.name = second.id.to_string();
    env.cluster.create(&second).await.unwrap();
    let failed = TaskStatus {
        state: TaskState::Failed,
        error: Some(format!("node {} is offline", env.node)),
    };
    env.cluster
        .update_status::<TaskSpec>(second.id, &failed)
        .await
        .unwrap();
    storage.resume();

    succeeded(&admin.wait_task(&first).await);
    // 뒤의 Task가 돌았다면 이미 크기를 바꾸고 끝났을 시간을 기다린다.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let task = env.cluster.get::<TaskSpec>(second.id).unwrap().unwrap();
    assert_eq!(task.status, failed);
    let resized = storage
        .calls()
        .iter()
        .filter(|(c, _)| *c == "resize")
        .count();
    assert_eq!(resized, 1);
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!(root.spec.size_bytes, 2 << 20);
}

#[tokio::test]
async fn force_stop_during_a_memory_snapshot_fails_it_and_removes_the_tag() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();

    env.hv.hang_next_snapshot_save();
    let (status, create) = admin
        .post("/vms/web/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{create}");
    tokio::time::timeout(common::WAIT, env.hv.snapshot_save_reached())
        .await
        .expect("snapshot save");

    // 만들기 중에도 강제 정지는 받는다(일반 정지는 그대로 막는다). 강제 정지 Task는 같은 VM의
    // 만들기 Task를 기다리지 않는다. 기다렸다면 VM이 꺼지지 않아 저장도 끝나지 않는다.
    let (status, body) = admin.post("/vms/web/actions/stop", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    succeeded(&admin.act("web", "stop?mode=force").await);

    // QMP가 끊겨 만들기가 실패한다. 꺼진 VM이므로 디스크의 태그는 스토리지로 지우고 리소스도 지운다.
    let task = admin.wait_task(&create).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "s").is_none());
    assert!(storage.calls().contains(&("delete", root.id)));
    assert!(env.hv.snapshots(vm).is_empty());
    assert_eq!(position(&env, vm), None);
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(current.status.phase, VmPhase::Stopped);
}

fn vm_spec(node: Uuid) -> VmSpec {
    let mut spec: VmSpec = serde_json::from_value(vm_request("x")["spec"].clone()).unwrap();
    spec.node = Some(node);
    spec
}

fn pending(vm: Uuid, spec: &VmSpec, name: &str) -> Snapshot {
    let mut s: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm,
            name: name.into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: spec.clone(),
        },
    );
    s.name = s.id.to_string();
    s
}

/// LVM-thin 풀 `thin`과 그 풀의 바로 Ready인 1 MiB raw 볼륨 `names`를 저장소에 직접 만들고, 파일
/// 없는 스토리지로 실행기를 다시 띄운다(볼륨은 1 MiB로 알린다).
async fn lvm_volumes(env: &mut Env, names: &[&str]) -> (Vec<Uuid>, Arc<FakeStorage>) {
    let pool = Resource::new(
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
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("thin").await;
    let storage = Arc::new(FakeStorage::default());
    let mut ids = Vec::new();
    for name in names {
        let mut volume = Resource::new(
            *name,
            VolumeSpec {
                pool: "thin".into(),
                size_bytes: 1 << 20,
                format: ImageFormat::Raw,
            },
        );
        env.cluster.create(&volume).await.unwrap();
        volume.status.phase = VolumePhase::Ready;
        env.cluster
            .update_status::<VolumeSpec>(volume.id, &volume.status)
            .await
            .unwrap();
        storage.set_size(volume.id, 1 << 20);
        ids.push(volume.id);
    }
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    (ids, storage)
}

/// 디스크 `disks`를 붙인 VM을 이 노드에서 실행 중으로 만든다. LVM-thin 풀은 macOS에서 쓸 수 없어
/// reconciler가 띄우지 못하므로 Mock 하이퍼바이저에 직접 띄운 뒤 이 노드에 켜진 것으로 배치한다
/// (reconciler는 이미 돌고 있는 VM을 그대로 둔다).
async fn running_lvm_vm(env: &Env, admin: &Client, name: &str, disks: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let current = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    let runtime = VmRuntimeSpec {
        id,
        name: name.into(),
        cpu: current.spec.cpu.clone(),
        memory_mib: current.spec.memory_mib,
        firmware: current.spec.firmware,
        machine: current.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    env.hv.start(&runtime, None).await.unwrap();
    let mut spec = current.spec.clone();
    spec.node = Some(env.node);
    spec.power = PowerState::Running;
    env.cluster
        .update_spec(id, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.cluster.get::<VmSpec>(id).unwrap().unwrap().status.phase != VmPhase::Running {
        assert!(std::time::Instant::now() < deadline, "VM did not run");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    id
}

#[tokio::test]
async fn running_vm_lvm_snapshot_pauses_it_while_the_disks_are_snapshotted() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0", "lv1"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0", "lv1"]).await;

    // 첫 디스크를 찍는 동안 VM은 멈춰 있다.
    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    assert!(env.hv.is_paused(vm));
    storage.resume();
    succeeded(&admin.wait_task(&task).await);
    assert!(!env.hv.is_paused(vm));
    assert_eq!(storage.calls(), [("create", lvs[0]), ("create", lvs[1])]);
    // QEMU의 내부 스냅샷은 쓰지 않는다.
    assert!(env.hv.snapshots(vm).is_empty());
    let a = get(&env, vm, "a");
    assert_eq!(a.status.phase, SnapshotPhase::Ready);
    assert!(!a.spec.memory);
    assert_eq!(position(&env, vm), Some(a.id));

    // 실행 중이어도 스냅샷 LV는 스토리지로 지운다.
    let (status, task) = admin.delete("/vms/web/snapshots/a").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(
        storage.calls()[2..],
        [("delete", lvs[0]), ("delete", lvs[1])]
    );
    assert!(find(&env, vm, "a").is_none());
    assert!(env.pid(vm).await.is_some());
}

#[tokio::test]
async fn failed_lvm_snapshot_resumes_the_vm_and_removes_the_lvs_made() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0", "lv1"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0", "lv1"]).await;

    storage.fail_snapshot_create(lvs[1]);
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!env.hv.is_paused(vm));
    assert_eq!(
        storage.calls(),
        [
            ("create", lvs[0]),
            ("create", lvs[1]),
            ("delete", lvs[0]),
            ("delete", lvs[1]),
        ]
    );
    assert!(find(&env, vm, "a").is_none());
    assert_eq!(position(&env, vm), None);
}

#[tokio::test]
async fn running_vm_lvm_disk_grows_in_storage_before_qemu() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0"]).await;

    // LV를 늘리는 동안 QEMU에는 아직 알리지 않았다.
    storage.pause();
    let (status, task) = admin
        .post("/volumes/lv0/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    assert_eq!(env.hv.disk_size(vm, 0), None);
    storage.resume();
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(storage.calls(), [("resize", lvs[0])]);
    assert_eq!(env.hv.disk_size(vm, 0), Some(2 << 20));
    let lv0 = env.cluster.get::<VolumeSpec>(lvs[0]).unwrap().unwrap();
    assert_eq!(lv0.spec.size_bytes, 2 << 20);
}

/// LVM 디스크의 VM은 예약이 메모리를 원해도 디스크만 찍는다(메모리를 요청하면 거절되어 예약이
/// 실패로 남는다).
#[tokio::test]
async fn scheduled_lvm_snapshot_leaves_out_memory() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0"]).await;
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.snapshot_schedule = Some(opl_types::vm::SnapshotSchedule {
        every: opl_types::vm::ScheduleEvery::Hourly,
        keep: 3,
        memory: true,
    });
    env.cluster
        .update_spec(vm, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();

    let scheduler = opl_api::SnapshotScheduler::spawn(
        env.state.clone(),
        opl_api::SnapshotSchedulerConfig {
            tick: std::time::Duration::from_millis(50),
            hour: std::time::Duration::from_secs(3600),
        },
    );
    let deadline = std::time::Instant::now() + common::WAIT;
    let made = loop {
        let (_, all) = env.cluster.list::<SnapshotSpec>().unwrap();
        if let Some(s) = all
            .into_iter()
            .find(|s| s.spec.vm == vm && s.spec.scheduled && s.status.phase == SnapshotPhase::Ready)
        {
            break s;
        }
        let error = env.cluster.get::<VmSpec>(vm).unwrap().unwrap().status;
        assert_eq!(error.schedule_error, None);
        assert!(
            std::time::Instant::now() < deadline,
            "no scheduled snapshot"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    scheduler.shutdown().await;
    assert!(!made.spec.memory);
    assert_eq!(storage.calls(), [("create", lvs[0])]);
    assert!(env.hv.snapshots(vm).is_empty());
}

/// 멈춘 동안 Task가 시간을 넘겨 버려져도 VM을 다시 돌린다.
#[tokio::test]
async fn abandoned_lvm_snapshot_still_resumes_the_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (_, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0"]).await;
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |c| {
        c.timeout = std::time::Duration::from_millis(500)
    })
    .await;

    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    assert!(env.hv.is_paused(vm));
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.is_paused(vm) {
        assert!(std::time::Instant::now() < deadline, "VM stayed paused");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    storage.resume();
}

/// QEMU가 스스로 멈춘 VM(I/O 오류 등)은 멈추지도 이어서 실행하지도 않고 디스크만 찍는다.
#[tokio::test]
async fn lvm_snapshot_leaves_a_vm_paused_by_qemu_paused() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0"]).await;
    // Mock 하이퍼바이저에서는 멈춘 것처럼 보이게 한다.
    env.hv.pause(vm).await.unwrap();

    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    succeeded(&task);
    assert!(env.hv.is_paused(vm));
    assert_eq!(storage.calls(), [("create", lvs[0])]);
    assert_eq!(get(&env, vm, "a").status.phase, SnapshotPhase::Ready);
}

/// QEMU가 스스로 멈춘 VM(`io-error` 등, 실행 중도 일시정지도 아닌 상태)은 멈추지도 이어서 실행하지도
/// 않고 디스크만 찍는다.
#[tokio::test]
async fn lvm_snapshot_of_a_vm_stopped_on_io_error_neither_pauses_nor_resumes_it() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0"]).await;
    env.hv
        .set_status(vm, Some(RuntimeStatus::Other("io-error".into())));

    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    succeeded(&task);
    assert_eq!(env.hv.pause_calls(vm), Vec::<&str>::new());
    assert_eq!(storage.calls(), [("create", lvs[0])]);
    assert_eq!(get(&env, vm, "a").status.phase, SnapshotPhase::Ready);
}

/// QMP로 다룰 수 없는 VM은 멈출 수 없으므로 디스크를 건드리기 전에 실패한다.
#[tokio::test]
async fn lvm_snapshot_of_an_unresponsive_vm_fails_before_touching_the_disks() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (_, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0"]).await;
    env.hv.set_unresponsive(vm, true);

    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("not responding"),
        "{task}"
    );
    assert_eq!(env.hv.pause_calls(vm), Vec::<&str>::new());
    assert!(storage.calls().is_empty());
    assert!(find(&env, vm, "a").is_none());
}

/// 디스크는 찍었는데 VM을 이어서 실행하지 못하면 스냅샷은 그대로 두고 Task만 실패로 끝낸다.
#[tokio::test]
async fn lvm_snapshot_taken_but_not_resumed_is_kept() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_lvm_vm(&env, &admin, "web", &["lv0"]).await;

    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    // 멈춘 동안 QMP를 잃었다.
    env.hv.set_unresponsive(vm, true);
    storage.resume();
    let task = admin.wait_task(&task).await;
    env.hv.set_unresponsive(vm, false);
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.starts_with("snapshot taken; resuming the VM failed:"),
        "{error}"
    );
    assert_eq!(storage.calls(), [("create", lvs[0])]);
    let a = get(&env, vm, "a");
    assert_eq!(a.status.phase, SnapshotPhase::Ready);
    assert_eq!(position(&env, vm), Some(a.id));
}
