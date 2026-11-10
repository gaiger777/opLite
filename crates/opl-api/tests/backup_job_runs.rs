//! 백업 작업 스케줄러: 짧은 tick으로 실행을 시작하고 대상 VM을 고르며, 노드당 동시 백업 Task 수를
//! 지키고, 끝나면 `last_run`을 적고 보존 규칙대로 지우는지. 실패한 백업은 더 새 성공 뒤 지우고, 백업이
//! 진행 중인 VM과 거절된 VM, 없는 이름은 오류로 남기며, 놓친 실행은 한 번만 하고, 꺼 둔 작업은 돌지
//! 않는지. 백업을 채우지 못하고 끝난 Task, 실행 도중 없어진 VM, 상태에 적지 못한 이번 실행의 백업도.
//! Mock 하이퍼바이저와 dir 풀로 실제 백업 Task를 돌리므로 qemu-img가 필요하다.
//! `cargo test -p opl-api --features storage-it --test backup_job_runs`
#![cfg(feature = "storage-it")]

mod common;

use std::time::{Duration, Instant};

use common::{Client, Env, WAIT, vm_request};
use opl_api::{BackupJobScheduler, BackupJobSchedulerConfig};
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus};
use opl_types::backup_job::{BackupJob, BackupJobSpec, BackupJobStatus, JobError, JobRun};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::task::{TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 50ms마다 보고 노드마다 백업 Task를 `max_concurrent`개까지 돌리는 스케줄러
fn scheduler(env: &Env, max_concurrent: usize) -> BackupJobScheduler {
    BackupJobScheduler::spawn(
        env.state.clone(),
        BackupJobSchedulerConfig {
            tick: Duration::from_millis(50),
            max_concurrent,
        },
    )
}

/// 이 노드에서 준비된 dir 풀을 저장소에 직접 만든다.
async fn create_pool(env: &Env, name: &str, backups: bool) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 풀 `local`과 백업 풀 `bk`, 그리고 `{이름}-root` qcow2 디스크를 붙인 꺼진 VM들. `labels`가 있는 VM은
/// 그 라벨을 단다.
async fn setup(env: &Env, admin: &Client, vms: &[(&str, Value)]) {
    create_pool(env, "local", false).await;
    create_pool(env, "bk", true).await;
    for (name, labels) in vms {
        let volume = format!("{name}-root");
        let (status, task) = admin
            .post(
                "/volumes",
                json!({
                    "name": volume,
                    "spec": { "pool": "local", "size_bytes": MIB, "format": "qcow2" }
                }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        let task = admin.wait_task(&task).await;
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
        let mut req = vm_request(name);
        req["labels"] = labels.clone();
        req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
        let (status, vm) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{vm}");
    }
}

/// 지금부터 열두 시간 뒤의 `HH:MM`(UTC). 시험 도중 예약 시각이 오지 않게 한다.
fn far_at() -> String {
    let t = (now() + 12 * 3600) % 86400;
    format!("{:02}:{:02}", t / 3600, t / 60 % 60)
}

async fn create_job(admin: &Client, name: &str, vms: Value, keep: Value, enabled: bool) -> Uuid {
    let (status, body) = admin
        .post(
            "/backup-jobs",
            json!({
                "name": name,
                "spec": {
                    "vms": vms,
                    "pool": "bk",
                    "schedule": { "at": far_at() },
                    "keep": keep,
                    "enabled": enabled,
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().parse().unwrap()
}

fn job(env: &Env, id: Uuid) -> BackupJob {
    env.cluster.get::<BackupJobSpec>(id).unwrap().unwrap()
}

/// 작업이 `done`을 만족할 때까지 기다린다.
async fn wait_job(env: &Env, id: Uuid, done: impl Fn(&BackupJob) -> bool) -> BackupJob {
    let deadline = Instant::now() + WAIT;
    loop {
        let job = job(env, id);
        if done(&job) {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "backup job did not settle: {job:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 실행이 없고 다음 실행이 앞으로 잡혔다(지금 실행 요청은 다음 실행을 지금으로 당긴다).
fn idle(job: &BackupJob) -> bool {
    job.status.run.is_none() && job.status.next_run.is_some_and(|t| t > now())
}

/// 지금 실행을 요청하고 그 실행이 끝날 때까지 기다린다.
async fn run_now(env: &Env, admin: &Client, id: Uuid) -> BackupJob {
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_job(env, id, idle).await
}

/// 작업 `id`가 만든 백업(만든 순서)
fn job_backups(env: &Env, id: Uuid) -> Vec<Backup> {
    let mut backups: Vec<Backup> = env
        .cluster
        .list::<BackupSpec>()
        .unwrap()
        .1
        .into_iter()
        .filter(|b| b.spec.job == Some(id))
        .collect();
    backups.sort_by_key(|b| b.id);
    backups
}

/// 백업들이 모두 끝날(ready나 failed) 때까지 기다린다.
async fn wait_backups_done(env: &Env, id: Uuid) -> Vec<Backup> {
    let deadline = Instant::now() + WAIT;
    loop {
        let backups = job_backups(env, id);
        if backups
            .iter()
            .all(|b| b.status.phase != BackupPhase::Pending)
        {
            return backups;
        }
        assert!(
            Instant::now() < deadline,
            "backups did not finish: {backups:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 백업 작업이 아닌 것을 포함한 끝나지 않은 VM 백업 Task 수
fn unfinished_backup_tasks(env: &Env) -> usize {
    env.cluster
        .list::<TaskSpec>()
        .unwrap()
        .1
        .iter()
        .filter(|t| matches!(t.spec.op, TaskOp::VmBackup { .. }) && !t.status.state.is_finished())
        .count()
}

/// 작업의 다음 실행 시각을 `at`으로 직접 바꾼다.
async fn set_next_run(env: &Env, id: Uuid, at: u64) {
    let job = job(env, id);
    let status = BackupJobStatus {
        next_run: Some(at),
        ..job.status
    };
    env.cluster
        .update_status_checked::<BackupJobSpec>(id, job.revision, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_run_backs_up_the_selected_vms_and_records_the_result() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(
        &env,
        &admin,
        &[("web", json!({ "tier": "prod" })), ("db", json!({}))],
    )
    .await;
    let id = create_job(
        &admin,
        "nightly",
        json!({ "labels": { "tier": "prod" } }),
        json!({}),
        true,
    )
    .await;
    let sched = scheduler(&env, 2);

    // 예약 시각 전에는 돌지 않는다.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(job(&env, id).status.last_run, None);
    assert!(job_backups(&env, id).is_empty());

    let before = now();
    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.clone().unwrap();
    assert_eq!((last.ok, last.failed), (1, 0), "{done:?}");
    assert!(before <= last.started_at && last.started_at <= last.finished_at);
    assert_eq!(done.status.errors, []);
    // 라벨이 맞는 VM만 백업했고, 백업에 작업을 적고 기본 이름 규칙을 따른다.
    let backups = job_backups(&env, id);
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(backups[0].spec.vm_name, "web");
    assert_eq!(backups[0].status.phase, BackupPhase::Ready);
    assert!(backups[0].name.starts_with("web-"), "{}", backups[0].name);
    let all = env.cluster.list::<BackupSpec>().unwrap().1;
    assert!(all.iter().all(|b| b.spec.vm_name != "db"), "{all:?}");
    // 다음 실행은 실행을 시작한 시각 뒤의 예약 시각이다.
    assert_eq!(
        done.status.next_run,
        Some(opl_types::backup_job::next_run(
            &done.spec.schedule,
            last.started_at
        ))
    );
    sched.shutdown().await;
}

/// `full_every`가 있는 작업은 증분으로(체인 한도 `full_every`) 백업을 요청한다. 꺼진 VM은 전체다.
#[tokio::test]
async fn jobs_with_full_every_ask_for_incremental_backups() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(&admin, "nightly", json!({ "all": true }), json!({}), true).await;
    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "full_every": 2 }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sched = scheduler(&env, 2);
    run_now(&env, &admin, id).await;
    let backups = wait_backups_done(&env, id).await;
    assert_eq!(backups.len(), 1, "{backups:?}");
    let b = &backups[0];
    assert!(b.spec.incremental);
    assert_eq!(b.spec.max_chain, Some(2));
    assert_eq!(b.status.phase, BackupPhase::Ready, "{b:?}");
    assert_eq!(b.status.full_reason.as_deref(), Some("VM is stopped"));

    // 0이면 지금처럼 늘 전체다.
    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "full_every": 0 }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    run_now(&env, &admin, id).await;
    let backups = wait_backups_done(&env, id).await;
    assert_eq!(backups.len(), 2, "{backups:?}");
    assert!(!backups[1].spec.incremental);
    assert_eq!(backups[1].spec.max_chain, None);
    assert_eq!(backups[1].status.full_reason, None);
    sched.shutdown().await;
}

#[tokio::test]
async fn limits_backup_tasks_per_node() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({})), ("db", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 1);

    // Task가 끝나지 않게 실행기를 멈춘다. 노드당 하나만 돌리므로 첫 VM(이름 순 db)만 꺼낸다.
    env.stop_runner().await;
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let running = wait_job(&env, id, |j| {
        j.status.run.as_ref().is_some_and(|r| r.backups.len() == 1)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still = job(&env, id);
    assert_eq!(still.status.run, running.status.run);
    let run = still.status.run.unwrap();
    assert_eq!(run.queue.len(), 1);
    assert_eq!(unfinished_backup_tasks(&env), 1);
    assert_eq!(job_backups(&env, id)[0].spec.vm_name, "db");
    // 실행 중에는 지금 실행을 다시 요청할 수 없다.
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // 실행기가 돌면 나머지도 백업하고 실행이 끝난다.
    env.start_runner().await;
    let done = wait_job(&env, id, idle).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (2, 0));
    let names: Vec<String> = job_backups(&env, id)
        .into_iter()
        .map(|b| b.spec.vm_name)
        .collect();
    assert_eq!(names, ["db", "web"]);
    sched.shutdown().await;
}

#[tokio::test]
async fn retention_deletes_backups_beyond_keep() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    // 손으로 만든 백업은 보존 규칙이 건드리지 않는다.
    let (status, task) = admin
        .post(
            "/vms/web/backups",
            json!({ "pool": "bk", "name": "manual" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
    let id = create_job(
        &admin,
        "keep2",
        json!({ "names": ["web"] }),
        json!({ "last": 2 }),
        true,
    )
    .await;
    let sched = scheduler(&env, 2);

    let mut made = Vec::new();
    for _ in 0..3 {
        run_now(&env, &admin, id).await;
        let new: Vec<Uuid> = job_backups(&env, id)
            .into_iter()
            .map(|b| b.id)
            .filter(|b| !made.contains(b))
            .collect();
        made.extend(new);
    }
    assert_eq!(made.len(), 3);
    // 세 번째 실행이 끝나면 가장 오래된 것을 지운다.
    let deadline = Instant::now() + WAIT;
    while job_backups(&env, id).len() > 2 {
        assert!(Instant::now() < deadline, "{:?}", job_backups(&env, id));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let left: Vec<Uuid> = job_backups(&env, id).into_iter().map(|b| b.id).collect();
    assert_eq!(left, made[1..]);
    // 이름은 겹치지 않는다(같은 초에 만들면 `-2`…를 붙인다).
    let backups = job_backups(&env, id);
    assert_ne!(backups[0].name, backups[1].name);
    assert!(
        env.cluster
            .get_by_name::<BackupSpec>("manual")
            .unwrap()
            .is_some()
    );
    sched.shutdown().await;
}

#[tokio::test]
async fn failed_backups_count_and_go_after_a_newer_success() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    // 실행 중인 VM의 백업은 임시 스냅샷을 읽는데, Mock 하이퍼바이저는 파일에 스냅샷을 만들지 않으므로
    // 실패한다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 2);

    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (0, 1));
    assert_eq!(done.status.errors.len(), 1, "{:?}", done.status.errors);
    assert_eq!(done.status.errors[0].vm, "web");
    assert!(
        done.status.errors[0]
            .error
            .contains("cannot back up volume"),
        "{:?}",
        done.status.errors
    );
    let failed = job_backups(&env, id);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].status.phase, BackupPhase::Failed);

    // 다시 실패해도 더 새 성공이 없으므로 남는다.
    run_now(&env, &admin, id).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(job_backups(&env, id).len(), 2);

    // 꺼진 VM은 백업된다. 성공하면 앞의 실패한 백업들을 지운다(보존 규칙이 없어도).
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 0));
    assert_eq!(done.status.errors, []);
    let deadline = Instant::now() + WAIT;
    while job_backups(&env, id).len() > 1 {
        assert!(Instant::now() < deadline, "{:?}", job_backups(&env, id));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(job_backups(&env, id)[0].status.phase, BackupPhase::Ready);
    sched.shutdown().await;
}

#[tokio::test]
async fn skips_vms_being_backed_up_and_records_refusals() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    // 실행 중이고 raw 디스크(dir 풀에서 스냅샷 불가)인 VM은 백업 요청이 거절된다.
    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": "db-root",
                "spec": { "pool": "local", "size_bytes": MIB, "format": "raw" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
    let mut req = vm_request("db");
    req["spec"]["disks"] = json!([{ "volume": "db-root", "bus": "virtio-blk" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // web은 손으로 요청한 백업이 끝나지 않았다.
    env.stop_runner().await;
    let (status, task) = admin
        .post(
            "/vms/web/backups",
            json!({ "pool": "bk", "name": "manual" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 2);

    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (0, 2));
    assert_eq!(done.status.errors.len(), 2, "{:?}", done.status.errors);
    assert_eq!(done.status.errors[0].vm, "db");
    assert!(
        done.status.errors[0]
            .error
            .contains("cannot be snapshotted"),
        "{:?}",
        done.status.errors
    );
    assert_eq!(
        done.status.errors[1],
        JobError {
            vm: "web".into(),
            error: "a backup of this VM is already running".into(),
        }
    );
    assert!(job_backups(&env, id).is_empty());
    assert_eq!(unfinished_backup_tasks(&env), 1);
    sched.shutdown().await;
}

#[tokio::test]
async fn missed_runs_run_once() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    // 사흘 동안 리더가 없었던 것처럼 다음 실행이 한참 지났다.
    set_next_run(&env, id, now() - 3 * 86400).await;
    let sched = scheduler(&env, 2);

    let done = wait_job(&env, id, |j| j.status.last_run.is_some() && idle(j)).await;
    wait_backups_done(&env, id).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(job(&env, id).status.last_run, done.status.last_run);
    assert_eq!(job_backups(&env, id).len(), 1);
    assert_eq!(done.status.last_run.unwrap().ok, 1);
    sched.shutdown().await;
}

#[tokio::test]
async fn disabled_jobs_do_not_start_and_stop_taking_vms() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({})), ("db", json!({}))]).await;
    let off = create_job(&admin, "off", json!({ "all": true }), json!({}), false).await;
    set_next_run(&env, off, now() - 60).await;
    let on = create_job(&admin, "on", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 1);

    // 꺼 둔 작업은 다음 실행이 지나도 돌지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let job_off = job(&env, off);
    assert_eq!(job_off.status.run, None);
    assert_eq!(job_off.status.last_run, None);
    assert!(job_backups(&env, off).is_empty());

    // 실행 도중 끄면 이미 시작한 백업만 기다리고 남은 VM은 꺼내지 않는다.
    env.stop_runner().await;
    let (status, body) = admin
        .post(&format!("/backup-jobs/{on}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_job(&env, on, |j| {
        j.status.run.as_ref().is_some_and(|r| r.backups.len() == 1)
    })
    .await;
    let (status, body) = admin
        .patch("/backup-jobs/on", json!({ "enabled": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_job(&env, on, |j| {
        j.status.run.as_ref().is_some_and(|r| r.queue.is_empty())
    })
    .await;
    env.start_runner().await;
    let done = wait_job(&env, on, |j| j.status.run.is_none()).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 0));
    assert_eq!(job_backups(&env, on).len(), 1);
    sched.shutdown().await;
}

#[tokio::test]
async fn a_backup_whose_task_ended_unfinished_fails_the_run() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 2);

    // 실행기가 멈춘 사이 janitor가 죽은 노드의 Task를 실패로 처리한 것처럼 Task만 끝낸다.
    env.stop_runner().await;
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let running = wait_job(&env, id, |j| {
        j.status.run.as_ref().is_some_and(|r| r.backups.len() == 1)
    })
    .await;
    let backup = running.status.run.unwrap().backups[0];
    let task = env
        .cluster
        .list::<TaskSpec>()
        .unwrap()
        .1
        .into_iter()
        .find(|t| matches!(t.spec.op, TaskOp::VmBackup { backup: b } if b == backup))
        .unwrap();
    let failed = TaskStatus {
        state: TaskState::Failed,
        error: Some("node lost".into()),
        progress: None,
    };
    env.cluster
        .update_status::<TaskSpec>(task.id, &failed)
        .await
        .unwrap();

    let done = wait_job(&env, id, idle).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (0, 1));
    assert_eq!(
        done.status.errors,
        [JobError {
            vm: "web".into(),
            error: "backup task ended without finishing the backup".into(),
        }]
    );
    assert_eq!(job_backups(&env, id)[0].status.phase, BackupPhase::Pending);
    sched.shutdown().await;
}

#[tokio::test]
async fn takes_in_a_backup_of_this_run_left_out_of_the_status() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let web = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    // 실행이 web의 백업을 만들었지만(이전 리더) 상태에 적기 전에 멈췄다.
    let job = job(&env, id);
    let status = BackupJobStatus {
        run: Some(JobRun {
            started_at: now() - 5,
            queue: vec![web.id],
            backups: vec![],
        }),
        ..job.status
    };
    env.cluster
        .update_status_checked::<BackupJobSpec>(id, job.revision, &status)
        .await
        .unwrap();
    let mut made: Backup = Resource::new(
        "web-made",
        BackupSpec {
            vm: web.id,
            vm_name: web.name.clone(),
            pool: "bk".into(),
            vm_spec: web.spec.clone(),
            job: Some(id),
            origin: None,
            incremental: false,
            max_chain: None,
        },
    );
    made.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(now()),
        ..Default::default()
    };
    env.cluster.create(&made).await.unwrap();
    let sched = scheduler(&env, 2);

    // 새 백업을 만들지 않고 그 백업을 이번 실행의 것으로 받아들인다.
    let done = wait_job(&env, id, |j| j.status.run.is_none()).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 0));
    assert_eq!(done.status.errors, []);
    let ids: Vec<Uuid> = job_backups(&env, id).into_iter().map(|b| b.id).collect();
    assert_eq!(ids, [made.id]);
    assert_eq!(unfinished_backup_tasks(&env), 0);
    sched.shutdown().await;
}

#[tokio::test]
async fn names_without_a_vm_are_errors() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(
        &admin,
        "named",
        json!({ "names": ["web", "ghost"] }),
        json!({}),
        true,
    )
    .await;
    let sched = scheduler(&env, 2);

    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 1));
    assert_eq!(
        done.status.errors,
        [JobError {
            vm: "ghost".into(),
            error: "no VM named \"ghost\"".into(),
        }]
    );
    sched.shutdown().await;
}

#[tokio::test]
async fn vms_deleted_during_a_run_are_skipped() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({})), ("db", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 1);

    // 노드당 하나만 돌리므로 db를 백업하는 동안 web은 큐에 남는다. 그사이 web이 없어진다.
    env.stop_runner().await;
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_job(&env, id, |j| {
        j.status.run.as_ref().is_some_and(|r| r.backups.len() == 1)
    })
    .await;
    let web = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    env.cluster.delete::<VmSpec>(web.id, None).await.unwrap();
    env.start_runner().await;

    let done = wait_job(&env, id, idle).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 0));
    assert_eq!(done.status.errors, []);
    let names: Vec<String> = job_backups(&env, id)
        .into_iter()
        .map(|b| b.spec.vm_name)
        .collect();
    assert_eq!(names, ["db"]);
    sched.shutdown().await;
}
