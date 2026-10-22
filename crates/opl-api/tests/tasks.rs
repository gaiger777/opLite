//! Task 실행기의 재시작 처리.

mod common;

use common::Env;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
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
