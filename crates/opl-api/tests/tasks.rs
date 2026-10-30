//! Task 실행기의 재시작 처리와 실행 중에 지워진 Task.

mod common;

use std::sync::Arc;

use common::{Env, FakeStorage};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{DiskBus, DiskSpec, PowerState, Vm, VmSpec};
use reqwest::StatusCode;
use uuid::Uuid;

#[tokio::test]
async fn tasks_left_running_by_a_previous_opld_are_failed() {
    let target = Uuid::now_v7();
    let id = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = id.clone();
    let env = Env::start_with(
        |_| {},
        |cluster| async move {
            let node = cluster.id().0;
            let mut task: Task = Resource::new(
                "t",
                TaskSpec {
                    op: TaskOp::VmStop,
                    target,
                    node,
                },
            );
            task.status = TaskStatus {
                state: TaskState::Running,
                error: None,
            };
            cluster.create(&task).await.unwrap();
            *slot.lock().unwrap() = Some(task.id);
        },
    )
    .await;
    let admin = env.admin().await;
    let id = id.lock().unwrap().unwrap();

    let task = admin
        .wait_task(&serde_json::json!({ "id": id.to_string() }))
        .await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(
        task["status"]["error"],
        "interrupted: opld restarted while the task was running"
    );
}

#[tokio::test]
async fn orphans_of_a_previous_opld_are_removed() {
    let env = Env::start_with(
        |_| {},
        |cluster| async move {
            // 볼륨을 만든 뒤 Task를 만들기 전에(또는 업로드 도중) opld가 죽었다.
            let volume = Resource::new(
                "half",
                VolumeSpec {
                    pool: "local".into(),
                    size_bytes: 1,
                    format: ImageFormat::Raw,
                },
            );
            cluster.create(&volume).await.unwrap();
        },
    )
    .await;
    let admin = env.admin().await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while admin.get("/volumes/half").await.0 != reqwest::StatusCode::NOT_FOUND {
        assert!(
            std::time::Instant::now() < deadline,
            "orphan volume was kept"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 풀이 아직 마운트되지 않아(NFS) 지우지 못한 고아 볼륨은 남겨 두고 나중에 다시 지운다.
#[tokio::test]
async fn orphans_on_an_unmounted_pool_are_removed_later() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
        "half",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1,
            format: ImageFormat::Raw,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let storage = Arc::new(FakeStorage::default());
    storage.fail_deletes_unmounted(2);
    env.start_runner_config(storage, |config| {
        config.orphan_retry = std::time::Duration::from_millis(50);
    })
    .await;
    // 실행기가 뜰 때 지우지 못했다.
    assert!(env.cluster.get::<VolumeSpec>(volume.id).unwrap().is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.cluster.get::<VolumeSpec>(volume.id).unwrap().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "orphan volume was kept"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn tasks_created_while_runner_is_down_run_when_it_starts() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", common::vm_request("web")).await;
    env.stop_runner().await;

    let (_, task) = admin
        .post("/vms/web/actions/start", serde_json::Value::Null)
        .await;
    assert_eq!(task["status"]["state"], "pending");
    env.start_runner().await;

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn orphans_are_removed_only_by_the_node_that_made_them() {
    let other = Uuid::now_v7();
    let env = Env::start_with(
        |_| {},
        move |cluster| async move {
            let me = cluster.id().0;
            for (name, node) in [("mine", Some(me)), ("theirs", Some(other))] {
                let mut volume: Volume = Resource::new(
                    name,
                    VolumeSpec {
                        pool: "local".into(),
                        size_bytes: 1,
                        format: ImageFormat::Raw,
                    },
                );
                volume.status.node = node;
                cluster.create(&volume).await.unwrap();
            }
        },
    )
    .await;
    // 실행기는 고아를 지운 뒤에 뜬다.
    assert!(
        env.cluster
            .get_by_name::<VolumeSpec>("mine")
            .unwrap()
            .is_none()
    );
    assert!(
        env.cluster
            .get_by_name::<VolumeSpec>("theirs")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_task_deleted_while_running_does_not_block_its_queue() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    let mut volume: Volume = Resource::new(
        "data",
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
    let storage = Arc::new(FakeStorage::default());
    storage.set_size(volume.id, 1 << 20);
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;

    // 크기 늘리기가 도는 사이에 Task가 지워졌다. 결과를 쓸 곳이 없으므로 끝난 것으로 본다.
    storage.pause();
    let resize = |size: u64| {
        admin.post(
            "/volumes/data/resize",
            serde_json::json!({ "size_bytes": size }),
        )
    };
    let (status, first) = resize(2 << 20).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    storage.reached().await;
    let id: Uuid = first["id"].as_str().unwrap().parse().unwrap();
    env.cluster.delete::<TaskSpec>(id, None).await.unwrap();
    storage.resume();

    // 같은 볼륨의 줄에 선 다음 Task가 돈다.
    let (status, second) = resize(3 << 20).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{second}");
    let second = admin.wait_task(&second).await;
    assert_eq!(second["status"]["state"], "succeeded", "{second}");
}

/// 디스크 `disks`를 쓰는 VM을 저장소에 바로 만든다(이 노드에 놓인다).
async fn vm_with_disks(env: &Env, name: &str, power: PowerState, disks: &[&str]) -> Vm {
    let mut spec: VmSpec =
        serde_json::from_value(common::vm_request(name)["spec"].clone()).unwrap();
    spec.node = Some(env.node);
    spec.power = power;
    spec.nics.clear();
    spec.disks = disks
        .iter()
        .map(|volume| DiskSpec {
            volume: (*volume).into(),
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        })
        .collect();
    let vm: Vm = Resource::new(name, spec);
    env.cluster.create(&vm).await.unwrap();
    vm
}

/// 이 노드에서 `state`로 도는 Task를 저장소에 바로 만든다. 실행 중(running)으로 만든 Task는 실행기가
/// 다시 시작하지 않는다.
async fn task_on(env: &Env, op: TaskOp, target: Uuid, node: Uuid, state: TaskState) {
    let mut task: Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    task.status = TaskStatus { state, error: None };
    env.cluster.create(&task).await.unwrap();
}

/// 실행기는 시작할 때와 `tag_sweep`마다 이 노드에서 켜진 채 남은 볼륨을 끈다(공유 LVM의 배타
/// 활성화가 남으면 다른 노드가 그 디스크를 쓸 수 없다). 이 노드의 실행 중이거나 켜려는 VM의 디스크,
/// 이 노드에서 끝나지 않은 Task의 볼륨(VM Task면 그 VM의 디스크), 아직 만드는 중인 볼륨은 그대로 둔다.
#[tokio::test]
async fn volumes_left_active_are_deactivated() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let storage = Arc::new(FakeStorage::default());
    env.start_runner_config(storage.clone(), |c| {
        c.tag_sweep = std::time::Duration::from_millis(50)
    })
    .await;
    for (name, backend) in [
        (
            "local",
            PoolBackend::Dir {
                path: env.dir.path().join("local"),
            },
        ),
        (
            "san",
            PoolBackend::LvmShared {
                vg: "san".into(),
                iscsi: None,
            },
        ),
    ] {
        let pool = Resource::new(
            name,
            StoragePoolSpec {
                shared: matches!(backend, PoolBackend::LvmShared { .. }),
                backend,
                nodes: vec![],
            },
        );
        env.cluster.create(&pool).await.unwrap();
    }
    let mut ids = std::collections::HashMap::new();
    for (name, pool, ready) in [
        ("boot", "local", true),
        ("idle", "san", true),
        ("stopped", "san", true),
        ("theirs", "san", true),
        ("resizing", "san", true),
        ("snapping", "san", true),
        ("making", "san", false),
    ] {
        let mut volume: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1 << 20,
                format: ImageFormat::Raw,
            },
        );
        if ready {
            volume.status.phase = VolumePhase::Ready;
        }
        env.cluster.create(&volume).await.unwrap();
        ids.insert(name, (pool, volume.id));
    }
    let web = vm_with_disks(&env, "web", PowerState::Running, &["boot"]).await;
    vm_with_disks(&env, "off", PowerState::Stopped, &["stopped"]).await;
    let snap = vm_with_disks(&env, "snap", PowerState::Stopped, &["snapping"]).await;
    let running = TaskState::Running;
    let resize = TaskOp::VolumeResize {
        size_bytes: 2 << 20,
    };
    task_on(&env, resize.clone(), ids["resizing"].1, env.node, running).await;
    let other = Uuid::now_v7();
    task_on(&env, resize, ids["theirs"].1, other, TaskState::Pending).await;
    let op = TaskOp::SnapshotCreate {
        snapshot: Uuid::now_v7(),
    };
    task_on(&env, op, snap.id, env.node, running).await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.pid(web.id).await.is_none() {
        assert!(std::time::Instant::now() < deadline, "web was not started");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let orphan = Uuid::now_v7();
    storage.set_active("san", orphan);
    for (pool, id) in ids.values() {
        storage.set_active(pool, *id);
    }
    let kept = |names: &[&str]| -> Vec<Uuid> {
        let mut v: Vec<Uuid> = names.iter().map(|n| ids[n].1).collect();
        v.sort();
        v
    };
    let want = kept(&["boot", "resizing", "snapping", "making"]);
    let active = || {
        let mut v = storage.active();
        v.sort();
        v
    };
    let deadline = std::time::Instant::now() + common::WAIT;
    while active() != want {
        assert!(
            std::time::Instant::now() < deadline,
            "active volumes: {:?}",
            storage.active()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // 여러 번 돌아도 쓰이는 볼륨은 끄지 않고, 끈 볼륨을 다시 끄지 않는다.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(active(), want);
    let mut deactivated: Vec<Uuid> = storage
        .calls()
        .into_iter()
        .filter(|(call, _)| *call == "deactivate")
        .map(|(_, id)| id)
        .collect();
    deactivated.sort();
    let mut gone = kept(&["idle", "stopped", "theirs"]);
    gone.push(orphan);
    gone.sort();
    assert_eq!(deactivated, gone);
}

/// 남은 활성화를 정리하는 동안 이미 켜진 볼륨을 쓰기 시작하면(그 볼륨을 디스크로 쓰는 VM의 Task)
/// 그 볼륨은 끄지 않는다(볼륨마다 끄기 직전에 VM·Task를 다시 읽는다).
#[tokio::test]
async fn a_volume_taken_up_during_the_sweep_is_not_deactivated() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let storage = Arc::new(FakeStorage::default());
    env.start_runner_config(storage.clone(), |c| {
        c.tag_sweep = std::time::Duration::from_millis(50)
    })
    .await;
    let pool = Resource::new(
        "san",
        StoragePoolSpec {
            shared: true,
            backend: PoolBackend::LvmShared {
                vg: "san".into(),
                iscsi: None,
            },
            nodes: vec![],
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let mut ids = Vec::new();
    for name in ["first", "second"] {
        let mut volume: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: "san".into(),
                size_bytes: 1 << 20,
                format: ImageFormat::Raw,
            },
        );
        volume.status.phase = VolumePhase::Ready;
        env.cluster.create(&volume).await.unwrap();
        ids.push(volume.id);
    }
    let (first, second) = (ids[0], ids[1]);
    storage.slow_deactivate(first, std::time::Duration::from_millis(500));
    storage.set_active("san", first);
    storage.set_active("san", second);

    // 첫 볼륨을 끄는 동안 이 노드에서 둘째 볼륨을 디스크로 쓰는 VM의 Task가 시작한다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while !storage.calls().contains(&("deactivate", first)) {
        assert!(
            std::time::Instant::now() < deadline,
            "first was not deactivated"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let vm = vm_with_disks(&env, "late", PowerState::Stopped, &["second"]).await;
    let op = TaskOp::SnapshotCreate {
        snapshot: Uuid::now_v7(),
    };
    task_on(&env, op, vm.id, env.node, TaskState::Running).await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while storage.active().contains(&first) {
        assert!(std::time::Instant::now() < deadline, "first stayed active");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(storage.active(), [second]);
    assert!(!storage.calls().contains(&("deactivate", second)));
}
