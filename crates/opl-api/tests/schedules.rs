//! 예약 스냅샷: 짧은 주기의 예약 작업이 스냅샷을 만들고 보관 개수대로 지우는지, 거절된 요청과 실패한
//! 만들기 Task를 `schedule_error`에 남기고 성공하면 지우는지, 실패한 보관 삭제를 남기고 tick마다 다시
//! 하지 않는지, 되돌리기가 지금 예약을 지키는지.

mod common;

use std::time::{Duration, Instant};

use common::{Client, Env, WAIT, vm_request};
use opl_api::{SnapshotScheduler, SnapshotSchedulerConfig};
use opl_driver::DriverError;
use opl_qemu::QmpError;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::{ScheduleEvery, SnapshotSchedule, Vm, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 50ms마다 보고 한 시간이 1초인 예약 작업
fn fast_scheduler(env: &Env) -> SnapshotScheduler {
    SnapshotScheduler::spawn(
        env.state.clone(),
        SnapshotSchedulerConfig {
            tick: Duration::from_millis(50),
            hour: Duration::from_secs(1),
        },
    )
}

/// 디스크 `disks`를 붙이고 예약 `schedule`을 둔 VM을 만들고 켠다.
async fn running_vm(admin: &Client, name: &str, disks: &[&str], schedule: Value) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    req["spec"]["snapshot_schedule"] = schedule;
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// 풀 `local`과 바로 Ready인 볼륨 `name`을 저장소에 직접 만든다(파일 없음).
async fn ready_volume(env: &Env, name: &str, format: ImageFormat) {
    if env
        .cluster
        .get_by_name::<StoragePoolSpec>("local")
        .unwrap()
        .is_none()
    {
        let pool = Resource::new(
            "local",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: env.dir.path().join("pool"),
                },
                nodes: vec![],
                shared: false,
            },
        );
        env.cluster.create(&pool).await.unwrap();
        env.pool_ready("local").await;
    }
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

fn snapshots(env: &Env, vm: Uuid) -> Vec<Snapshot> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items.into_iter().filter(|s| s.spec.vm == vm).collect()
}

fn get_vm(env: &Env, vm: Uuid) -> Vm {
    env.cluster.get::<VmSpec>(vm).unwrap().unwrap()
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
async fn scheduled_snapshots_are_created_and_pruned_to_keep() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let schedule = json!({ "every": "hourly", "keep": 2, "memory": true });
    let vm = running_vm(&admin, "web", &[], schedule).await;
    // 사용자가 만든 스냅샷은 세지도 지우지도 않는다.
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "mine" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;

    let scheduler = fast_scheduler(&env);
    let mut seen = std::collections::HashSet::new();
    let deadline = Instant::now() + WAIT;
    while seen.len() < 4 {
        assert!(Instant::now() < deadline, "only {} scheduled", seen.len());
        let all = snapshots(&env, vm);
        let auto: Vec<&Snapshot> = all.iter().filter(|s| s.spec.scheduled).collect();
        // 넘친 것은 다음 tick에 지우므로 잠깐 keep+1개일 수 있다.
        assert!(auto.len() <= 3, "{auto:?}");
        assert!(
            all.iter()
                .any(|s| s.spec.name == "mine" && !s.spec.scheduled)
        );
        for s in auto {
            assert!(s.spec.name.starts_with("auto-"), "{}", s.spec.name);
            // 디스크가 없으면 메모리를 담지 않는다.
            assert!(!s.spec.memory);
            seen.insert(s.id);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    scheduler.shutdown().await;
    // 넷 이상 만들었는데 셋 이하만 남았으니 오래된 것을 지웠다.
    assert_eq!(get_vm(&env, vm).status.schedule_error, None);
}

#[tokio::test]
async fn refused_requests_and_failed_tasks_are_recorded_until_a_success() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_volume(&env, "raw", ImageFormat::Raw).await;
    ready_volume(&env, "root", ImageFormat::Qcow2).await;
    // 실패하면 다음 주기(1초)에 다시 한다.
    let schedule = json!({ "every": "hourly", "keep": 3 });
    let raw = running_vm(&admin, "raw-vm", &["raw"], schedule.clone()).await;
    let web = running_vm(&admin, "web", &["root"], schedule).await;

    // 다음 메모리 스냅샷 작업이 실패한다. 예약은 메모리를 담지 않지만 실행 중이면 하이퍼바이저로 만든다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobFailed {
            id: "j".into(),
            error: "no space".into(),
        }));
    let scheduler = fast_scheduler(&env);

    // raw 디스크는 요청이 거절된다. 스냅샷은 생기지 않는다.
    until("raw refusal", || {
        get_vm(&env, raw).status.schedule_error.is_some_and(|e| {
            e.code == "snapshot_unsupported" && e.message.contains("cannot be snapshotted")
        })
    })
    .await;
    assert!(snapshots(&env, raw).is_empty());

    // 실패한 만들기 Task의 오류가 남고, 다음 주기에 다시 만들어 성공하면 지운다.
    until("task failure", || {
        get_vm(&env, web)
            .status
            .schedule_error
            .is_some_and(|e| e.code == "task_failed" && e.message.contains("no space"))
    })
    .await;
    until("success", || {
        get_vm(&env, web).status.schedule_error.is_none()
            && snapshots(&env, web)
                .iter()
                .any(|s| s.spec.scheduled && s.status.phase == SnapshotPhase::Ready)
    })
    .await;
    scheduler.shutdown().await;
}

#[tokio::test]
async fn a_failed_delete_is_recorded_and_not_retried_every_tick() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[], json!({ "every": "hourly", "keep": 1 })).await;
    // 보관 개수를 넘은 예약 스냅샷 둘을 직접 둔다. 한 시간(1분) 안이라 새로 만들지 않는다.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let spec = get_vm(&env, vm).spec;
    let mut ids = vec![];
    for (name, at) in [("old", now - 2), ("new", now - 1)] {
        let mut s: Snapshot = Resource::new(
            "",
            SnapshotSpec {
                vm,
                name: name.into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled: true,
                vm_spec: spec.clone(),
            },
        );
        s.name = s.id.to_string();
        env.cluster.create(&s).await.unwrap();
        s.status.phase = SnapshotPhase::Ready;
        s.status.created_at = Some(at);
        env.cluster
            .update_status::<SnapshotSpec>(s.id, &s.status)
            .await
            .unwrap();
        ids.push(s.id);
    }

    // 실행 중인 VM의 스냅샷은 하이퍼바이저로 지운다. 처음 지우기가 실패한다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobFailed {
            id: "j".into(),
            error: "no space".into(),
        }));
    let scheduler = SnapshotScheduler::spawn(
        env.state.clone(),
        SnapshotSchedulerConfig {
            tick: Duration::from_millis(50),
            hour: Duration::from_secs(60),
        },
    );
    until("delete failure", || {
        get_vm(&env, vm)
            .status
            .schedule_error
            .is_some_and(|e| e.code == "task_failed" && e.message.contains("no space"))
    })
    .await;
    // 실패한 삭제를 tick마다 다시 하지 않는다(한 시간 뒤에 한다).
    tokio::time::sleep(Duration::from_millis(500)).await;
    scheduler.shutdown().await;
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    // 예약 정리의 삭제로 표시한다.
    let deletes: Vec<_> = tasks
        .iter()
        .filter(|t| matches!(t.spec.op, TaskOp::SnapshotDelete { .. }))
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(
        deletes[0].spec.op,
        TaskOp::SnapshotDelete {
            snapshot: ids[0],
            scheduled: true
        }
    );
    assert!(snapshots(&env, vm).iter().any(|s| s.id == ids[0]));
}

#[tokio::test]
async fn a_failed_user_delete_of_a_scheduled_snapshot_is_not_recorded() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[], Value::Null).await;
    let mut s: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm,
            name: "auto".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: true,
            vm_spec: get_vm(&env, vm).spec,
        },
    );
    s.name = s.id.to_string();
    env.cluster.create(&s).await.unwrap();
    s.status.phase = SnapshotPhase::Ready;
    s.status.created_at = Some(1);
    env.cluster
        .update_status::<SnapshotSpec>(s.id, &s.status)
        .await
        .unwrap();

    // 사용자가 지운 예약 스냅샷의 삭제가 실패해도 예약 오류로 남기지 않는다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobFailed {
            id: "j".into(),
            error: "no space".into(),
        }));
    let (status, task) = admin.delete("/vms/web/snapshots/auto").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["scheduled"], false, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(get_vm(&env, vm).status.schedule_error, None);
}

#[tokio::test]
async fn offline_task_node_is_recorded_without_a_request() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[], json!({ "every": "hourly", "keep": 1 })).await;
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.online = false;
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();

    let scheduler = fast_scheduler(&env);
    until("offline", || {
        get_vm(&env, vm)
            .status
            .schedule_error
            .is_some_and(|e| e.code == "node_offline" && e.message.contains("offline"))
    })
    .await;
    assert!(snapshots(&env, vm).is_empty());

    // 노드가 돌아오면 만들고 오류를 지운다.
    node.status.online = true;
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();
    until("recovered", || {
        get_vm(&env, vm).status.schedule_error.is_none() && snapshots(&env, vm).len() == 1
    })
    .await;
    scheduler.shutdown().await;
}

#[tokio::test]
async fn revert_keeps_the_current_schedule() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[], Value::Null).await;
    let (_, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "cfg" }))
        .await;
    admin.wait_task(&task).await;

    // 스냅샷 뒤에 예약을 켠다. 되돌려도 스냅샷 때의 "예약 없음"으로 돌아가지 않는다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["snapshot_schedule"] = json!({ "every": "weekly", "keep": 5 });
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, task) = admin
        .post("/vms/web/snapshots/cfg/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(
        get_vm(&env, vm).spec.snapshot_schedule,
        Some(SnapshotSchedule {
            every: ScheduleEvery::Weekly,
            keep: 5,
            memory: false,
        })
    );
}
