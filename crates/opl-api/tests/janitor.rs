//! 클러스터 정리 작업: 오프라인 노드의 `pending` 스냅샷 Task가 유예 뒤 실패해 VM 조작이 풀리고
//! `running` Task는 남는지, 제거된 노드의 Task가 곧바로 실패하는지, 만들 Task 없이 남은 `pending`
//! 스냅샷을 지우는지, 끝낸 예약 스냅샷 만들기를 `schedule_error`에 남기는지.

mod common;

use std::time::{Duration, Instant};

use common::{Env, WAIT, vm_request};
use opl_api::{ClusterJanitor, ClusterJanitorConfig};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::snapshot::{Snapshot, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{ScheduleError, VmSpec};
use reqwest::StatusCode;
use serde_json::Value;
use uuid::Uuid;

const GRACE: Duration = Duration::from_millis(500);

/// 50ms마다 보고 유예(오프라인·고아 스냅샷)가 0.5초인 정리 작업
fn fast_janitor(env: &Env) -> ClusterJanitor {
    ClusterJanitor::spawn(
        env.state.clone(),
        ClusterJanitorConfig {
            tick: Duration::from_millis(50),
            offline_grace: GRACE,
            orphan_grace: GRACE,
        },
    )
}

/// 노드 `name`을 저장소에 직접 만든다(멤버십에는 없다).
async fn other_node(env: &Env, name: &str, online: bool) -> Uuid {
    let mut node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    env.cluster.create(&node).await.unwrap();
    node.status = NodeStatus {
        online,
        ..Default::default()
    };
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();
    node.id
}

/// VM `vm`의 `pending` 스냅샷과 노드 `node`에 배정된 만들기 Task를 직접 만든다.
async fn snapshot_task(env: &Env, vm: Uuid, name: &str, node: Uuid, state: TaskState) -> Task {
    snapshot_task_of(env, vm, name, false, node, state).await
}

/// [`snapshot_task`]에 예약 스냅샷인지(`scheduled`)를 더한 것
async fn snapshot_task_of(
    env: &Env,
    vm: Uuid,
    name: &str,
    scheduled: bool,
    node: Uuid,
    state: TaskState,
) -> Task {
    let spec = env.cluster.get::<VmSpec>(vm).unwrap().unwrap().spec;
    let mut snapshot: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm,
            name: name.into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled,
            vm_spec: spec,
        },
    );
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();
    task(
        env,
        TaskOp::SnapshotCreate {
            snapshot: snapshot.id,
        },
        vm,
        node,
        state,
    )
    .await
}

async fn task(env: &Env, op: TaskOp, target: Uuid, node: Uuid, state: TaskState) -> Task {
    let mut t: Task = Resource::new("", TaskSpec { op, target, node });
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    if state != TaskState::Pending {
        t.status = TaskStatus { state, error: None };
        env.cluster
            .update_status::<TaskSpec>(t.id, &t.status)
            .await
            .unwrap();
    }
    t
}

fn get_task(env: &Env, id: Uuid) -> Task {
    env.cluster.get::<TaskSpec>(id).unwrap().unwrap()
}

fn snapshot_ids(env: &Env) -> Vec<Uuid> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items.into_iter().map(|s| s.id).collect()
}

/// 조건이 맞을 때까지 기다린다.
async fn until(why: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn offline_node_tasks_fail_after_the_grace_and_free_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let vm: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let away = other_node(&env, "node2", false).await;
    let pending = snapshot_task(&env, vm, "p", away, TaskState::Pending).await;
    // 네트워크만 끊긴 노드는 아직 실행하고 있을 수 있으므로 `running`은 남는다(다른 VM 대상).
    let running = task(
        &env,
        TaskOp::VmStart,
        Uuid::now_v7(),
        away,
        TaskState::Running,
    )
    .await;

    // 끝나지 않은 스냅샷 Task가 VM 조작을 막는다.
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    let janitor = fast_janitor(&env);
    let started = Instant::now();
    until("offline failure", || {
        get_task(&env, pending.id).status.state == TaskState::Failed
    })
    .await;
    assert!(started.elapsed() >= GRACE, "{:?}", started.elapsed());
    assert_eq!(
        get_task(&env, pending.id).status.error,
        Some(format!("node {away} is offline"))
    );
    // Task 없이 남은 pending 스냅샷도 지운다.
    until("orphan snapshot", || snapshot_ids(&env).is_empty()).await;
    janitor.shutdown().await;
    assert_eq!(get_task(&env, running.id).status.state, TaskState::Running);

    // VM 조작이 풀린다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn removed_node_tasks_fail_at_once() {
    let env = Env::start().await;
    let gone = Uuid::now_v7();
    let target = Uuid::now_v7();
    let pending = task(&env, TaskOp::VmStart, target, gone, TaskState::Pending).await;
    let running = task(&env, TaskOp::VmStop, target, gone, TaskState::Running).await;
    // 유예가 길어도 제거된 노드는 기다리지 않는다.
    let janitor = ClusterJanitor::spawn(
        env.state.clone(),
        ClusterJanitorConfig {
            tick: Duration::from_millis(50),
            offline_grace: Duration::from_secs(3600),
            orphan_grace: Duration::from_secs(3600),
        },
    );
    let error = Some(format!("node {gone} was removed"));
    until("removed", || {
        [&pending, &running].iter().all(|t| {
            let t = get_task(&env, t.id);
            t.status.state == TaskState::Failed && t.status.error == error
        })
    })
    .await;
    janitor.shutdown().await;
}

#[tokio::test]
async fn pending_snapshots_with_a_live_task_stay() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let vm: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    // 온라인 노드(이 노드는 아니다)의 끝나지 않은 만들기 Task가 있으면 지우지 않는다.
    let node = other_node(&env, "node2", true).await;
    let live = snapshot_task(&env, vm, "live", node, TaskState::Pending).await;
    let janitor = fast_janitor(&env);
    tokio::time::sleep(GRACE).await;
    janitor.shutdown().await;
    assert_eq!(snapshot_ids(&env).len(), 1);
    assert_eq!(get_task(&env, live.id).status.state, TaskState::Pending);
}

#[tokio::test]
async fn failed_scheduled_snapshot_creation_is_recorded() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let vm: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let away = other_node(&env, "node2", false).await;
    let pending = snapshot_task_of(&env, vm, "auto", true, away, TaskState::Pending).await;
    let janitor = fast_janitor(&env);
    // 실행기가 실패로 끝낸 예약 만들기처럼 Task 실패로 남긴다.
    let expected = ScheduleError {
        code: "task_failed".into(),
        message: format!("node {away} is offline"),
    };
    until("schedule error", || {
        env.cluster
            .get::<VmSpec>(vm)
            .unwrap()
            .unwrap()
            .status
            .schedule_error
            .as_ref()
            == Some(&expected)
    })
    .await;
    janitor.shutdown().await;
    assert_eq!(get_task(&env, pending.id).status.state, TaskState::Failed);
}
