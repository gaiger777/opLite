//! 백업 작업 API: 만들기·목록·읽기·바꾸기·지우기·지금 실행 요청의 검사와 기록되는 상태.
//! 스케줄러 없이 API가 남긴 리소스의 모양만 확인한다.

mod common;

use common::{Client, Env, vm_request};
use opl_types::backup::{Backup, BackupSpec};
use opl_types::backup_job::{BackupJobSpec, JobRun, JobSchedule, Weekday, next_run};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::remote::{Remote, RemoteSpec};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 디렉터리 풀을 저장소에 직접 만든다.
async fn dir_pool(env: &Env, name: &str, backups: bool) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes: vec![],
            shared: false,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 백업 풀 `backups`와 디스크 풀 `local`
async fn setup(env: &Env) {
    dir_pool(env, "backups", true).await;
    dir_pool(env, "local", false).await;
}

fn job(name: &str, spec: Value) -> Value {
    json!({ "name": name, "spec": spec })
}

/// `tier=prod` VM을 매일 03:00에 백업하고 최근 3개를 남기는 작업 스펙
fn nightly() -> Value {
    json!({
        "vms": { "labels": { "tier": "prod" } },
        "pool": "backups",
        "schedule": { "at": "03:00" },
        "keep": { "last": 3 },
    })
}

async fn create(admin: &Client, name: &str, spec: Value) -> Value {
    let (status, body) = admin.post("/backup-jobs", job(name, spec)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

fn schedule(at: &str, days: &[Weekday]) -> JobSchedule {
    JobSchedule {
        at: at.into(),
        days: days.to_vec(),
    }
}

#[tokio::test]
async fn create_records_the_job_with_its_next_run() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;

    let before = now();
    let body = create(&admin, "nightly", nightly()).await;
    let after = now();
    assert_eq!(body["spec"]["vms"], json!({ "labels": { "tier": "prod" } }));
    assert_eq!(
        body["spec"]["schedule"],
        json!({ "at": "03:00", "days": [] })
    );
    assert_eq!(
        body["spec"]["keep"],
        json!({ "last": 3, "daily": 0, "weekly": 0, "monthly": 0 })
    );
    // 적지 않으면 사용한다.
    assert_eq!(body["spec"]["enabled"], true);
    // 다음 실행은 만든 시각 뒤의 첫 03:00(UTC)이다.
    let next = body["status"]["next_run"].as_u64().unwrap();
    let sched = schedule("03:00", &[]);
    assert!(
        next_run(&sched, before) <= next && next <= next_run(&sched, after),
        "{body}"
    );
    assert_eq!(body["status"]["run"], Value::Null);
    assert_eq!(body["status"]["last_run"], Value::Null);

    let (status, got) = admin.get("/backup-jobs/nightly").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], body["id"]);
    assert_eq!(got["status"]["next_run"], next);
    let (status, got) = admin
        .get(&format!("/backup-jobs/{}", body["id"].as_str().unwrap()))
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (status, list) = admin.get("/backup-jobs").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(list["revision"].as_u64().is_some());

    // 모든 VM, 이름으로 고르기, 요일과 꺼 두기
    let all = json!({
        "vms": { "all": true },
        "pool": "backups",
        "schedule": { "at": "23:59", "days": ["sat", "sun"] },
        "enabled": false,
    });
    let body = create(&admin, "weekend", all).await;
    assert_eq!(body["spec"]["vms"], json!({ "all": true }));
    assert_eq!(body["spec"]["schedule"]["days"], json!(["sat", "sun"]));
    assert_eq!(body["spec"]["enabled"], false);
    let names = json!({
        "vms": { "names": ["web", "db"] },
        "pool": "backups",
        "schedule": { "at": "00:00" },
    });
    let body = create(&admin, "named", names).await;
    assert_eq!(body["spec"]["vms"], json!({ "names": ["web", "db"] }));
}

#[tokio::test]
async fn create_checks_the_pool_and_the_spec() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let with = |field: &str, value: Value| {
        let mut spec = nightly();
        spec[field] = value;
        job("nightly", spec)
    };

    // 풀은 있어야 하고 백업 풀이어야 한다.
    for (pool, msg) in [("nope", "not found"), ("local", "backups")] {
        let got = admin.post("/backup-jobs", with("pool", json!(pool))).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert!(
            got.1["message"].as_str().unwrap().contains(msg),
            "{}",
            got.1
        );
    }
    let bad = [
        with("vms", json!({ "names": [] })),
        with("vms", json!({ "labels": {} })),
        with("vms", json!({ "all": false })),
        with("vms", json!({ "all": true, "names": ["web"] })),
        with("vms", json!({})),
        with("schedule", json!({ "at": "24:00" })),
        with("schedule", json!({ "at": "3:00" })),
        with("schedule", json!({ "at": "03:00", "days": ["mon", "mon"] })),
        with("schedule", json!({ "at": "03:00", "days": ["monday"] })),
        with("keep", json!({ "last": -1 })),
        job("0190f5a4-3e2b-7c1d-8e9f-a0b1c2d3e4f5", nightly()),
        job("Bad Name", nightly()),
    ];
    for req in bad {
        let got = admin.post("/backup-jobs", req.clone()).await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{req} -> {}", got.1);
    }
    let got = admin
        .post(
            "/backup-jobs",
            with("schedule", json!({ "at": "03:00", "days": ["mon", "mon"] })),
        )
        .await;
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("more than once"),
        "{}",
        got.1
    );
    let (_, list) = admin.get("/backup-jobs").await;
    assert_eq!(list["items"], json!([]));

    create(&admin, "nightly", nightly()).await;
    assert_error(
        &admin.post("/backup-jobs", job("nightly", nightly())).await,
        StatusCode::CONFLICT,
        "name_taken",
    );
}

/// 원격 복제 대상은 이름 규칙을 따라야 하고(400), 옛 노드가 남아 있으면 쓰지 않는다(409).
#[tokio::test]
async fn replicate_is_checked_and_waits_for_every_node_to_run_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let with = |target: Value| {
        let mut spec = nightly();
        spec["replicate"] = target;
        job("nightly", spec)
    };
    for bad in [
        json!({ "remote": "DR", "pool": "backups" }),
        json!({ "remote": "dr", "pool": "" }),
        json!({ "remote": "dr" }),
    ] {
        let got = admin.post("/backup-jobs", with(bad.clone())).await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{bad} -> {}", got.1);
    }

    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    let target = json!({ "remote": "dr", "pool": "backups" });
    assert_error(
        &admin.post("/backup-jobs", with(target.clone())).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    // 복제가 없는 작업은 옛 노드가 읽는 모양 그대로라 그대로 만든다.
    let body = create(&admin, "plain", nightly()).await;
    assert!(body["spec"].get("replicate").is_none(), "{body}");

    let mut node = env.cluster.get_by_name::<NodeSpec>("old").unwrap().unwrap();
    node.status.version = Some(opl_types::version::BUILD.into());
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();
    // 등록하지 않은 원격은 400이다.
    let got = admin.post("/backup-jobs", with(target.clone())).await;
    assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
    assert!(
        got.1["message"].as_str().unwrap().contains("remote"),
        "{}",
        got.1
    );
    remote(&env, "dr").await;
    let body = create(&admin, "nightly", with(target.clone())["spec"].clone()).await;
    assert_eq!(body["spec"]["replicate"], target);
}

/// `full_every`: 만들기와 바꾸기에 쓰고 0이면 빠진다. 원격 복제와 함께 쓸 수 있고, 증분 백업을 모르는
/// 노드가 있으면 쓰지 못한다(409 `cluster_upgrading`).
#[tokio::test]
async fn full_every_is_checked_and_needs_every_node_to_know_incremental_backups() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    remote(&env, "dr").await;
    let mut spec = nightly();
    spec["full_every"] = json!(6);
    let body = create(&admin, "nightly", spec.clone()).await;
    assert_eq!(body["spec"]["full_every"], 6);
    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "full_every": 0 }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("full_every").is_none(), "{body}");
    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "full_every": 3 }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["full_every"], 3);

    // 증분 백업을 만드는 작업도 원격으로 복제한다.
    let target = json!({ "remote": "dr", "pool": "backups" });
    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "replicate": target }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["full_every"], 3);
    assert_eq!(body["spec"]["replicate"], target);
    let mut with = spec.clone();
    with["replicate"] = target.clone();
    let body = create(&admin, "both", with).await;
    assert_eq!(body["spec"]["full_every"], 6);
    assert_eq!(body["spec"]["replicate"], target);

    // 기능 목록이 빈 옛 노드(이 빌드의 버전)
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some(opl_types::version::BUILD.into());
    env.cluster.create(&node).await.unwrap();
    assert_error(
        &admin.post("/backup-jobs", job("weekly", spec)).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    assert_error(
        &admin
            .patch("/backup-jobs/nightly", json!({ "full_every": 6 }), None)
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    // 늘 전체로 되돌리기는 된다.
    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "full_every": 0 }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 원격 `name`을 저장소에 직접 등록한다(연결하지 않는다).
async fn remote(env: &Env, name: &str) {
    let remote: Remote = Resource::new(
        name,
        RemoteSpec {
            url: "https://10.0.0.9:8006".into(),
            ca_fingerprint: format!("sha256:{}", "0".repeat(64)),
            token: "opl_secret".into(),
            ca_pem: String::new(),
        },
    );
    env.cluster.create(&remote).await.unwrap();
}

/// PATCH의 `replicate`: 주면 바꾸고(등록한 원격만), `null`이면 끄고, 없으면 그대로 둔다. 옛 노드가
/// 남아 있으면 켜지 못한다(409).
#[tokio::test]
async fn patch_turns_replication_on_and_off() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    remote(&env, "dr").await;
    create(&admin, "nightly", nightly()).await;
    let target = json!({ "remote": "dr", "pool": "backups" });

    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "replicate": target }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["replicate"], target);
    // 다른 값을 바꾸면 복제는 그대로다.
    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "enabled": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["replicate"], target);

    for bad in [
        json!({ "remote": "nope", "pool": "backups" }),
        json!({ "remote": "dr", "pool": "Bad" }),
        json!({ "remote": "dr" }),
    ] {
        let got = admin
            .patch("/backup-jobs/nightly", json!({ "replicate": bad }), None)
            .await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{bad} -> {}", got.1);
    }

    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "replicate": null }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("replicate").is_none(), "{body}");
    let job = env
        .cluster
        .get_by_name::<BackupJobSpec>("nightly")
        .unwrap()
        .unwrap();
    assert_eq!(job.spec.replicate, None);

    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    assert_error(
        &admin
            .patch("/backup-jobs/nightly", json!({ "replicate": target }), None)
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
}

#[tokio::test]
async fn patch_changes_given_fields_and_recomputes_the_next_run() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    dir_pool(&env, "more-backups", true).await;
    let created = create(&admin, "nightly", nightly()).await;
    let next = created["status"]["next_run"].as_u64().unwrap();

    // 보존만 바꾸면 다른 값과 다음 실행은 그대로다.
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "keep": { "daily": 7 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["keep"],
        json!({ "last": 0, "daily": 7, "weekly": 0, "monthly": 0 })
    );
    assert_eq!(body["spec"]["vms"], created["spec"]["vms"]);
    assert_eq!(body["spec"]["schedule"], created["spec"]["schedule"]);
    assert_eq!(body["status"]["next_run"], next);

    // 일정을 바꾸면 다음 실행을 다시 계산한다.
    let before = now();
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({
                "schedule": { "at": "12:30", "days": ["wed"] },
                "vms": { "all": true },
                "pool": "more-backups",
            }),
            None,
        )
        .await;
    let after = now();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["vms"], json!({ "all": true }));
    assert_eq!(body["spec"]["pool"], "more-backups");
    let next = body["status"]["next_run"].as_u64().unwrap();
    let sched = schedule("12:30", &[Weekday::Wed]);
    assert!(
        next_run(&sched, before) <= next && next <= next_run(&sched, after),
        "{body}"
    );
    let (_, got) = admin.get("/backup-jobs/nightly").await;
    assert_eq!(got["status"]["next_run"], next);
    assert_eq!(got["revision"], body["revision"]);

    // 검사: 백업 풀이 아닌 풀, 잘못된 일정, 모르는 필드, 빈 요청
    for req in [
        json!({ "pool": "local" }),
        json!({ "schedule": { "at": "25:00" } }),
        json!({ "vms": { "names": [] } }),
        json!({ "name": "other" }),
        json!({}),
    ] {
        let got = admin.patch("/backup-jobs/nightly", req.clone(), None).await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{req} -> {}", got.1);
    }

    // If-Match가 맞지 않으면 412, 맞으면 쓴다.
    let rev = got["revision"].as_u64().unwrap();
    assert_error(
        &admin
            .patch(
                "/backup-jobs/nightly",
                json!({ "enabled": false }),
                Some(rev + 100),
            )
            .await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "enabled": false }),
            Some(rev),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["enabled"], false);
    assert_error(
        &admin
            .patch("/backup-jobs/nope", json!({ "enabled": true }), None)
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn days_are_stored_in_week_order() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let mut spec = nightly();
    spec["schedule"] = json!({ "at": "03:00", "days": ["sun", "wed", "mon"] });
    let body = create(&admin, "nightly", spec).await;
    assert_eq!(
        body["spec"]["schedule"]["days"],
        json!(["mon", "wed", "sun"])
    );
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "schedule": { "at": "03:00", "days": ["fri", "tue"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["schedule"]["days"], json!(["tue", "fri"]));
    // 순서만 다른 요일은 바꾼 것이 아니다.
    let rev = body["revision"].clone();
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "schedule": { "at": "03:00", "days": ["tue", "fri"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], rev);
}

#[tokio::test]
async fn changing_the_schedule_keeps_a_requested_run() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    create(&admin, "nightly", nightly()).await;
    let (status, body) = admin.post("/backup-jobs/nightly/run", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let requested = body["status"]["next_run"].as_u64().unwrap();
    assert!(requested <= now(), "{body}");

    // 지금 실행 요청이 기다리는 동안 일정을 바꿔도 그 요청은 남는다.
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "schedule": { "at": "12:30" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["schedule"]["at"], "12:30");
    assert_eq!(body["status"]["next_run"], requested);
}

#[tokio::test]
async fn enabling_a_job_skips_runs_missed_while_disabled() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let mut spec = nightly();
    spec["enabled"] = json!(false);
    let created = create(&admin, "nightly", spec).await;
    // 꺼 둔 사이 다음 실행 시각이 지났다.
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let mut status = env
        .cluster
        .get::<BackupJobSpec>(id)
        .unwrap()
        .unwrap()
        .status;
    status.next_run = Some(1);
    env.cluster
        .update_status::<BackupJobSpec>(id, &status)
        .await
        .unwrap();

    let before = now();
    let (code, body) = admin
        .patch("/backup-jobs/nightly", json!({ "enabled": true }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let next = body["status"]["next_run"].as_u64().unwrap();
    assert!(next >= next_run(&schedule("03:00", &[]), before), "{body}");
}

#[tokio::test]
async fn run_asks_for_a_run_now_unless_one_is_going() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let created = create(&admin, "nightly", nightly()).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    let before = now();
    let (status, body) = admin.post("/backup-jobs/nightly/run", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let next = body["status"]["next_run"].as_u64().unwrap();
    assert!(before <= next && next <= now(), "{body}");
    let (_, got) = admin.get("/backup-jobs/nightly").await;
    assert_eq!(got["status"]["next_run"], next);

    // 실행이 진행 중이면 409 job_running
    let mut status = env
        .cluster
        .get::<BackupJobSpec>(id)
        .unwrap()
        .unwrap()
        .status;
    status.run = Some(JobRun {
        started_at: next,
        queue: vec![],
        backups: vec![],
    });
    env.cluster
        .update_status::<BackupJobSpec>(id, &status)
        .await
        .unwrap();
    assert_error(
        &admin.post("/backup-jobs/nightly/run", Value::Null).await,
        StatusCode::CONFLICT,
        "job_running",
    );

    // 꺼 둔 작업은 실행하지 않는다.
    let mut spec = nightly();
    spec["enabled"] = json!(false);
    create(&admin, "off", spec).await;
    assert_error(
        &admin.post("/backup-jobs/off/run", Value::Null).await,
        StatusCode::CONFLICT,
        "job_disabled",
    );
    assert_error(
        &admin.post("/backup-jobs/nope/run", Value::Null).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn deleting_a_job_keeps_its_backups() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let created = create(&admin, "nightly", nightly()).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let vm_spec: VmSpec = serde_json::from_value(vm_request("web")["spec"].clone()).unwrap();
    let backup: Backup = Resource::new(
        "web-20261104-030000",
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            pool: "backups".into(),
            vm_spec,
            job: Some(id),
            origin: None,
            incremental: false,
            max_chain: None,
        },
    );
    env.cluster.create(&backup).await.unwrap();

    // If-Match가 맞지 않으면 지우지 않는다.
    let rev = created["revision"].as_u64().unwrap();
    let r = admin
        .http
        .delete(format!("{}/api/v1/backup-jobs/nightly", admin.base))
        .bearer_auth(admin.token.as_ref().unwrap())
        .header("if-match", (rev + 100).to_string());
    assert_error(
        &admin.send(r).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );

    let (status, body) = admin.delete("/backup-jobs/nightly").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        admin.get("/backup-jobs/nightly").await.0,
        StatusCode::NOT_FOUND
    );
    let (status, got) = admin.get(&format!("/backups/{}", backup.id)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["job"], id.to_string());
}

#[tokio::test]
async fn viewers_read_jobs_and_operators_manage_them() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    create(&admin, "nightly", nightly()).await;

    assert_eq!(viewer.get("/backup-jobs").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/backup-jobs/nightly").await.0, StatusCode::OK);
    for got in [
        viewer.post("/backup-jobs", job("other", nightly())).await,
        viewer
            .patch("/backup-jobs/nightly", json!({ "enabled": false }), None)
            .await,
        viewer.post("/backup-jobs/nightly/run", Value::Null).await,
        viewer.delete("/backup-jobs/nightly").await,
    ] {
        assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
    }

    let (status, body) = operator.post("/backup-jobs", job("other", nightly())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = operator
        .patch("/backup-jobs/other", json!({ "keep": { "last": 1 } }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = operator.post("/backup-jobs/other/run", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = operator.delete("/backup-jobs/other").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}
