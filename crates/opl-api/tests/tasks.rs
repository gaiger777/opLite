//! Task 실행기의 재시작 처리.

mod common;

use common::Env;
use opl_types::resource::Resource;
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
async fn tasks_created_while_runner_is_down_run_when_it_starts() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", common::vm_request("web")).await;
    env.stop_runner().await;

    let (_, task) = admin
        .post("/vms/web/actions/start", serde_json::Value::Null)
        .await;
    assert_eq!(task["status"]["state"], "pending");
    env.start_runner();

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}
