//! Task 실행기의 재시작 처리와 실행 중에 지워진 Task.

mod common;

use std::sync::Arc;

use common::{Env, FakeStorage};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
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
