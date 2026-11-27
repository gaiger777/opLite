//! DR 오케스트레이션 API(계획 76 3단계): 복구 계획 CRUD·권한·기능·검사, 보호 상태(리더 반복 작업 — 사본 있음·없음·
//! 나이·다른 클러스터, 바뀔 때만 쓰기), 보고, 실행 기록 읽기(직접 넣은 실행), 실행 끝점의 검사·409들·실행 리소스, 계획된
//! 이전의 원격 확인(원격 역할의 두 번째 `Env`), reprotect. 실행을 진행시키는 엔진은 4단계라 Task는 돌지 않는다.

mod common;

use std::time::Duration;

use common::{Env, WAIT};
use orbivisor_api::{DrProtection, DrProtectionConfig};
use orbivisor_cluster::ClusterError;
use orbivisor_cluster::types::Rejection;
use orbivisor_types::backup::{Backup, BackupOrigin, BackupPhase, BackupSpec, BackupStatus};
use orbivisor_types::backup_job::{BackupJobSpec, VmSelector};
use orbivisor_types::dr::{
    DR_LABEL, DR_TEST_LABEL, DrRun, DrRunKind, DrRunSpec, DrRunState, DrRunStatus, RecoveryPlanSpec,
};
use orbivisor_types::node::{DR_PLANS, NodeSpec};
use orbivisor_types::remote::RemoteSpec;
use orbivisor_types::resource::Resource;
use orbivisor_types::storage::{PoolBackend, StoragePoolSpec};
use orbivisor_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const CA: &str = "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const OTHER_CA: &str = "sha256:ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 이 노드가 `dr-plans`를 알리는지 바꾼다(4단계가 `FEATURES`에 넣기 전까지 시험이 켠다).
async fn set_dr(env: &Env, on: bool) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != DR_PLANS);
    if on {
        status.features.push(DR_PLANS.into());
    }
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

async fn pool(env: &Env, name: &str, backups: bool) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes: vec![],
            shared: false,
            backups,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 저장소에 바로 넣은 원격(닿지 않는 주소)
async fn remote(env: &Env, name: &str, ca: &str) -> Uuid {
    let r = Resource::new(
        name,
        RemoteSpec {
            url: "https://127.0.0.1:9".into(),
            ca_fingerprint: ca.into(),
            token: "opl_x".into(),
            ca_pem: String::new(),
        },
    );
    env.cluster.create(&r).await.unwrap();
    r.id
}

fn vm_spec(networks: &[&str]) -> VmSpec {
    let nics: Vec<_> = networks
        .iter()
        .map(|n| json!({ "network": n, "model": "virtio" }))
        .collect();
    serde_json::from_value(json!({
        "cpu": { "cores": 1 },
        "memory_mib": 256,
        "machine": "virt",
        "nics": nics
    }))
    .unwrap()
}

/// 운영 클러스터(`ca`)에서 복제된 `ready` 사본(`age`초 전에 뜬 것)
async fn copy(env: &Env, name: &str, vm: &str, ca: &str, age: u64) -> Uuid {
    let mut b: Backup = Resource::new(
        name,
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: vm.into(),
            pool: "bk".into(),
            store: None,
            source: None,
            vm_spec: vm_spec(&["lan"]),
            job: None,
            origin: Some(BackupOrigin {
                cluster: "prod".into(),
                ca_fingerprint: ca.into(),
                backup: Uuid::now_v7(),
                job: None,
            }),
            incremental: false,
            max_chain: None,
        },
    );
    b.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(now() - age),
        ..Default::default()
    };
    env.cluster.create(&b).await.unwrap();
    b.id
}

/// DR 쪽: 기능 켬, 복원 풀 `dr-pool`, 백업 풀 `bk`, 원격 `prod`, 시험 네트워크 `dr-test-lan`
async fn dr_env() -> Env {
    let env = Env::start().await;
    set_dr(&env, true).await;
    pool(&env, "dr-pool", false).await;
    pool(&env, "bk", true).await;
    remote(&env, "prod", CA).await;
    let (status, body) = env
        .admin()
        .await
        .post(
            "/networks",
            json!({ "name": "dr-test-lan", "spec": { "bridge": "vmbr9" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env
}

/// 그룹 둘(`db` 다음 `web`)의 계획
fn plan(name: &str) -> Value {
    json!({
        "name": name,
        "spec": {
            "source": "prod",
            "pool": "dr-pool",
            "rpo_target_secs": 86400,
            "test_networks": { "lan": "dr-test-lan" },
            "groups": [
                { "name": "db", "wait": "guest", "vms": [ { "vm": "db1" } ] },
                { "name": "web", "delay_secs": 30, "vms": [ { "vm": "web1", "ip": [ { "nic": 0, "address": "10.20.0.5/24", "gateway": "10.20.0.1" } ] } ] }
            ]
        }
    })
}

async fn create_plan(env: &Env, name: &str) -> Value {
    let (status, body) = env.admin().await.post("/dr-plans", plan(name)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

#[tokio::test]
async fn admins_manage_plans_and_viewers_read_them() {
    let env = dr_env().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let viewer = env.user("viewer", "viewer").await;
    copy(&env, "web1-copy", "web1", CA, 3600).await;

    // 만들면 곧 보호 상태를 센다: web1은 사본(1시간), db1은 없음.
    let created = create_plan(&env, "web-tier").await;
    assert_eq!(
        created["spec"]["groups"][0]["timeout_secs"], 600,
        "{created}"
    );
    assert_eq!(created["spec"]["groups"][1]["wait"], "running");
    let p = &created["status"]["protection"];
    assert_eq!(p[0]["vm"], "db1", "{created}");
    assert_eq!(p[0]["ok"], false);
    assert_eq!(p[0]["issue"], "no-recovery-point");
    assert_eq!(p[1]["vm"], "web1");
    assert_eq!(p[1]["ok"], true);
    assert_eq!(p[1]["backup"], "web1-copy");
    assert!((3600..3700).contains(&p[1]["age_secs"].as_u64().unwrap()));
    assert!(created["status"]["checked_at"].as_u64().is_some());

    assert_error(
        &admin.post("/dr-plans", plan("web-tier")).await,
        StatusCode::CONFLICT,
        "name_taken",
    );
    // viewer는 읽고 operator는 쓰지 못한다.
    for path in [
        "/dr-plans",
        "/dr-plans/web-tier",
        "/dr-plans/web-tier/report",
        "/dr-plans/web-tier/runs",
    ] {
        let (status, body) = viewer.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    let (_, list) = viewer.get("/dr-plans").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(list["revision"].as_u64().is_some());
    let id = created["id"].as_str().unwrap();
    let (_, by_id) = viewer.get(&format!("/dr-plans/{id}")).await;
    assert_eq!(by_id["name"], "web-tier");
    for client in [&operator, &viewer] {
        assert_eq!(
            client.post("/dr-plans", plan("other")).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .put(
                    "/dr-plans/web-tier",
                    json!({ "spec": plan("x")["spec"] }),
                    None
                )
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client.delete("/dr-plans/web-tier").await.0,
            StatusCode::FORBIDDEN
        );
        for action in ["test", "cleanup", "cancel"] {
            assert_eq!(
                client
                    .post(&format!("/dr-plans/web-tier/actions/{action}"), json!({}))
                    .await
                    .0,
                StatusCode::FORBIDDEN,
                "{action}"
            );
        }
        assert_eq!(
            client
                .post(
                    "/dr-plans/web-tier/actions/failover",
                    json!({ "mode": "disaster", "confirm": "web-tier" })
                )
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert!(env.cluster.list::<DrRunSpec>().unwrap().1.is_empty());

    // 통째로 바꾸기(If-Match)
    let mut spec = plan("web-tier")["spec"].clone();
    spec["rpo_target_secs"] = json!(3600);
    let rev = created["revision"].as_u64().unwrap();
    let (status, updated) = admin
        .put(
            "/dr-plans/web-tier",
            json!({ "labels": { "tier": "web" }, "spec": spec }),
            Some(rev),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["spec"]["rpo_target_secs"], 3600);
    assert_eq!(updated["labels"]["tier"], "web");
    assert_error(
        &admin
            .put("/dr-plans/web-tier", json!({ "spec": spec }), Some(rev))
            .await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );

    let (status, _) = admin.delete("/dr-plans/web-tier").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_error(
        &viewer.get("/dr-plans/web-tier").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn plans_are_checked() {
    let env = dr_env().await;
    let admin = env.admin().await;
    let post = |body: Value| {
        let admin = &admin;
        async move { admin.post("/dr-plans", body).await }
    };
    let with = |f: &dyn Fn(&mut Value)| {
        let mut p = plan("p");
        f(&mut p["spec"]);
        p
    };
    // 형식(400)
    for (why, body) in [
        ("no groups", with(&|s| s["groups"] = json!([]))),
        (
            "twice",
            with(&|s| s["groups"][1]["vms"][0]["vm"] = json!("db1")),
        ),
        (
            "bad ip",
            with(&|s| s["groups"][1]["vms"][0]["ip"][0]["dhcp"] = json!(true)),
        ),
        (
            "timeout",
            with(&|s| s["groups"][0]["timeout_secs"] = json!(1)),
        ),
        ("bad pool name", with(&|s| s["pool"] = json!("Bad Pool"))),
        ("unknown pool", with(&|s| s["pool"] = json!("nope"))),
        (
            "unknown vm pool",
            with(&|s| s["groups"][0]["vms"][0]["pool"] = json!("nope")),
        ),
        (
            "unknown node",
            with(&|s| s["groups"][0]["vms"][0]["node"] = json!("node9")),
        ),
        (
            "unknown network",
            with(&|s| s["networks"] = json!({ "lan": "dr-lan" })),
        ),
        (
            "unknown test network",
            with(&|s| s["test_networks"] = json!({ "lan": "nope" })),
        ),
        (
            "unknown security group",
            with(&|s| s["security_groups"] = json!({ "web": "dr-web" })),
        ),
        ("unknown field", with(&|s| s["extra"] = json!(1))),
    ] {
        let (status, body) = post(body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    }
    let (status, body) = post(with(&|s| s["groups"] = json!([]))).await;
    assert_eq!(body["code"], "invalid_request", "{status} {body}");
    assert!(
        body["message"].as_str().unwrap().contains("groups"),
        "{body}"
    );
    // 원격이 없으면 409
    assert_error(
        &post(with(&|s| s["source"] = json!("elsewhere"))).await,
        StatusCode::CONFLICT,
        "dr_source_missing",
    );
    // 있는 이름은 된다(노드·보안 그룹).
    let (status, body) = admin
        .post("/security-groups", json!({ "name": "dr-web", "spec": {} }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = post(with(&|s| {
        s["security_groups"] = json!({ "web": "dr-web" });
        s["groups"][0]["vms"][0]["node"] = json!("node1");
    }))
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 모든 노드가 dr-plans를 알아야 쓴다(검증보다 먼저).
    set_dr(&env, false).await;
    for body in [plan("q"), with(&|s| s["groups"] = json!([]))] {
        assert_error(&post(body).await, StatusCode::CONFLICT, "cluster_upgrading");
    }
    assert_error(
        &admin
            .put("/dr-plans/p", json!({ "spec": plan("p")["spec"] }), None)
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    assert_error(
        &admin.post("/dr-plans/p/actions/test", json!({})).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    // 읽기는 된다.
    assert_eq!(admin.get("/dr-plans/p/report").await.0, StatusCode::OK);
}

fn protection(env: &Env, plan: &str) -> (u64, Value) {
    let p = env
        .cluster
        .get_by_name::<RecoveryPlanSpec>(plan)
        .unwrap()
        .unwrap();
    (p.revision, serde_json::to_value(&p.status).unwrap())
}

async fn until(why: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_leader_recounts_protection_when_copies_change_and_writes_only_changes() {
    let env = dr_env().await;
    create_plan(&env, "web-tier").await;
    let looper = DrProtection::spawn(
        env.state.clone(),
        DrProtectionConfig {
            tick: Duration::from_millis(20),
            interval: Duration::from_millis(100),
        },
    );
    // 다른 클러스터의 사본과 `ready`가 아닌 사본은 복구 지점이 아니다.
    copy(&env, "web1-elsewhere", "web1", OTHER_CA, 60).await;
    let failed = copy(&env, "web1-failed", "web1", CA, 60).await;
    let mut b = env.cluster.get::<BackupSpec>(failed).unwrap().unwrap();
    b.status.phase = BackupPhase::Failed;
    env.cluster
        .update_status::<BackupSpec>(failed, &b.status)
        .await
        .unwrap();
    // 새 사본이 오면 곧 센다: web1은 1시간(ok), db1은 30시간(RPO 24시간 넘음).
    copy(&env, "web1-a", "web1", CA, 3600).await;
    copy(&env, "db1-a", "db1", CA, 30 * 3600).await;
    until("protection never counted the new copies", || {
        let p = protection(&env, "web-tier").1;
        p["protection"][1]["backup"] == "web1-a" && p["protection"][0]["backup"] == "db1-a"
    })
    .await;
    let (rev, st) = protection(&env, "web-tier");
    let p = &st["protection"];
    assert_eq!(p[0]["backup"], "db1-a", "{st}");
    assert_eq!(p[0]["ok"], false);
    assert_eq!(p[0]["issue"], "over-rpo");
    assert_eq!(p[1]["ok"], true);
    assert!(p[1].get("issue").is_none(), "{st}");
    // 바뀐 것이 없으면(나이만 흐름) 여러 번 다시 세어도 쓰지 않는다.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(protection(&env, "web-tier").0, rev);

    // 더 새 사본은 곧 바꾼다.
    copy(&env, "db1-b", "db1", CA, 600).await;
    until("the newer copy was not taken", || {
        protection(&env, "web-tier").1["protection"][0]["backup"] == "db1-b"
    })
    .await;
    assert_eq!(protection(&env, "web-tier").1["protection"][0]["ok"], true);
    // 원격을 지우면 복구 지점이 없다(원격이 닿지 않는 것과 다르다 — 저장된 지문만 쓴다).
    let prod = env
        .cluster
        .get_by_name::<RemoteSpec>("prod")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<RemoteSpec>(prod.id, None)
        .await
        .unwrap();
    until("removing the remote did not clear the points", || {
        protection(&env, "web-tier").1["protection"][1]["issue"] == "no-recovery-point"
    })
    .await;
    looper.shutdown().await;
}

/// 시험이 바로 넣는 끝난 실행
async fn finished_run(
    env: &Env,
    plan: &Value,
    name: &str,
    kind: DrRunKind,
    state: DrRunState,
    started_at: u64,
    steps: Value,
) -> DrRun {
    let spec: RecoveryPlanSpec = serde_json::from_value(plan["spec"].clone()).unwrap();
    let mut run: DrRun = Resource::new(
        name,
        DrRunSpec {
            plan: plan["id"].as_str().unwrap().parse().unwrap(),
            plan_name: plan["name"].as_str().unwrap().into(),
            kind,
            plan_spec: spec,
            max_parallel: 4,
            cleanup: None,
        },
    );
    run.status = serde_json::from_value(json!({
        "state": state,
        "started_at": started_at,
        "finished_at": started_at + 300,
        "rto_secs": 240,
        "steps": steps,
    }))
    .unwrap();
    env.cluster.create(&run).await.unwrap();
    run
}

fn step(vm: &str, phase: &str) -> Value {
    json!({ "group": "web", "vm": vm, "phase": phase, "at": 1, "new_vm": format!("{vm}-drt") })
}

#[tokio::test]
async fn the_report_and_run_records_show_tests_and_failovers() {
    let env = dr_env().await;
    let viewer = env.user("viewer", "viewer").await;
    copy(&env, "web1-a", "web1", CA, 7200).await;
    let p = create_plan(&env, "web-tier").await;
    let t0 = now() - 100_000;
    for (i, state) in [
        DrRunState::Succeeded,
        DrRunState::Succeeded,
        DrRunState::Partial,
        DrRunState::Succeeded,
    ]
    .into_iter()
    .enumerate()
    {
        let steps = if state == DrRunState::Partial {
            json!([step("db1", "failed"), step("web1", "ready")])
        } else {
            json!([step("db1", "ready"), step("web1", "ready")])
        };
        finished_run(
            &env,
            &p,
            &format!("t{i}"),
            DrRunKind::Test,
            state,
            t0 + i as u64 * 1000,
            steps,
        )
        .await;
    }
    let fo = finished_run(
        &env,
        &p,
        "f1",
        DrRunKind::FailoverDisaster,
        DrRunState::Succeeded,
        t0 + 500,
        json!([step("db1", "ready"), step("web1", "timed_out")]),
    )
    .await;

    let (status, r) = viewer.get("/dr-plans/web-tier/report").await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["plan"], "web-tier");
    assert_eq!(r["source"], "prod");
    assert_eq!(r["source_registered"], true);
    assert_eq!(r["rpo_target_secs"], 86400);
    assert_eq!(
        (r["total"].as_u64(), r["protected"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(r["protection"][1]["backup"], "web1-a");
    assert!((7200..7300).contains(&r["protection"][1]["age_secs"].as_u64().unwrap()));
    // 최근 시험 셋(새것부터), 실패한 VM
    let tests: Vec<&str> = r["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tests, ["t3", "t2", "t1"], "{r}");
    assert_eq!(r["tests"][1]["state"], "partial");
    assert_eq!(r["tests"][1]["failed_vms"], json!(["db1"]));
    assert_eq!(r["tests"][0]["rto_secs"], 240);
    assert_eq!(r["last_failover"]["name"], "f1");
    assert_eq!(r["last_failover"]["kind"], "failover-disaster");
    assert_eq!(r["last_failover"]["failed_vms"], json!(["web1"]));
    assert!(r.get("running").is_none(), "{r}");

    let (status, runs) = viewer.get("/dr-plans/web-tier/runs").await;
    assert_eq!(status, StatusCode::OK, "{runs}");
    let names: Vec<&str> = runs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["t3", "t2", "t1", "f1", "t0"]);
    for key in [fo.id.to_string(), "f1".to_string()] {
        let (status, run) = viewer.get(&format!("/dr-runs/{key}")).await;
        assert_eq!(status, StatusCode::OK, "{run}");
        assert_eq!(run["spec"]["kind"], "failover-disaster");
        assert_eq!(run["status"]["steps"][1]["phase"], "timed_out");
    }
    assert_error(
        &viewer.get("/dr-runs/nope").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    // 계획을 지우면 실행 기록도 지운다.
    let (status, _) = env.admin().await.delete("/dr-plans/web-tier").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(env.cluster.list::<DrRunSpec>().unwrap().1.is_empty());
}

async fn finish(env: &Env, id: &Value, state: DrRunState) {
    let id: Uuid = id.as_str().unwrap().parse().unwrap();
    let mut run = env.cluster.get::<DrRunSpec>(id).unwrap().unwrap();
    run.status.state = state;
    run.status.finished_at = Some(now());
    env.cluster
        .update_status::<DrRunSpec>(id, &run.status)
        .await
        .unwrap();
}

/// 라벨이 붙은 VM(시험·장애 조치가 만든 것처럼)
async fn labelled_vm(env: &Env, name: &str, label: &str, plan: &str) -> Uuid {
    let mut vm = Resource::new(name, vm_spec(&["user"]));
    vm.labels.insert(label.into(), plan.into());
    env.cluster.create(&vm).await.unwrap();
    vm.id
}

#[tokio::test]
async fn actions_check_the_request_and_make_run_records() {
    let env = dr_env().await;
    let admin = env.admin().await;
    copy(&env, "web1-a", "web1", CA, 600).await;
    let p = create_plan(&env, "web-tier").await;

    // 시험: 실행 리소스(계획 사본·첫 상태)를 만들고 계획에 적는다. db1은 복구 지점이 없어 곧 failed다.
    let (status, run) = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    assert_eq!(run["spec"]["kind"], "test");
    assert_eq!(run["spec"]["plan"], p["id"]);
    assert_eq!(run["spec"]["plan_name"], "web-tier");
    assert_eq!(run["spec"]["plan_spec"], p["spec"]);
    assert_eq!(run["spec"]["max_parallel"], 4);
    assert_eq!(run["status"]["state"], "running");
    assert!(
        run["name"].as_str().unwrap().starts_with("web-tier-test-"),
        "{run}"
    );
    let steps = &run["status"]["steps"];
    assert_eq!(steps[0]["vm"], "db1");
    assert_eq!(steps[0]["phase"], "failed");
    assert_eq!(steps[0]["error"], "no recovery point");
    assert_eq!(steps[1]["vm"], "web1");
    assert_eq!(steps[1]["phase"], "pending");
    assert_eq!(steps[1]["backup"], "web1-a");
    // `lan`은 시험 네트워크로 맵이 있어 거품 네트워크가 없다.
    assert!(run["status"].get("bubbles").is_none(), "{run}");
    let (_, got) = admin.get("/dr-plans/web-tier").await;
    assert_eq!(got["status"]["run"], run["id"]);

    // 도는 실행이 있으면 다른 실행은 409
    for (path, body) in [
        ("test", json!({})),
        ("cleanup", json!({})),
        (
            "failover",
            json!({ "mode": "disaster", "confirm": "web-tier" }),
        ),
    ] {
        assert_error(
            &admin
                .post(&format!("/dr-plans/web-tier/actions/{path}"), body)
                .await,
            StatusCode::CONFLICT,
            "dr_run_active",
        );
    }
    assert_error(
        &admin.delete("/dr-plans/web-tier").await,
        StatusCode::CONFLICT,
        "dr_run_active",
    );
    // 상태 머신도 계획 하나에 도는 실행 하나만 받는다.
    let mut twin: DrRun = Resource::new(
        "twin",
        env.cluster
            .get::<DrRunSpec>(run["id"].as_str().unwrap().parse().unwrap())
            .unwrap()
            .unwrap()
            .spec,
    );
    twin.status = DrRunStatus::default();
    match env.cluster.create(&twin).await {
        Err(ClusterError::Rejected(Rejection::InUse { field, .. })) => assert_eq!(field, "dr-run"),
        other => panic!("{other:?}"),
    }
    // 취소는 표시만 한다(엔진이 멈춘다). 두 번 해도 같다.
    for _ in 0..2 {
        let (status, cancelled) = admin
            .post("/dr-plans/web-tier/actions/cancel", json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{cancelled}");
        assert_eq!(cancelled["id"], run["id"]);
        assert_eq!(cancelled["status"]["cancel_requested"], true);
    }

    // 시험이 끝났는데 시험 VM이 남았으면 다음 시험은 409, 뒷정리가 그것을 지운다.
    finish(&env, &run["id"], DrRunState::Cancelled).await;
    assert_error(
        &admin
            .post("/dr-plans/web-tier/actions/cancel", json!({}))
            .await,
        StatusCode::CONFLICT,
        "dr_no_active_run",
    );
    labelled_vm(&env, "web1-drt", DR_TEST_LABEL, "web-tier").await;
    labelled_vm(&env, "other-drt", DR_TEST_LABEL, "other").await;
    labelled_vm(&env, "web1", DR_LABEL, "web-tier").await;
    let got = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    assert_error(&got, StatusCode::CONFLICT, "dr_cleanup_required");
    assert!(
        got.1["message"].as_str().unwrap().contains("VM web1-drt"),
        "{}",
        got.1
    );
    let (status, cleanup) = admin
        .post("/dr-plans/web-tier/actions/cleanup", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{cleanup}");
    assert_eq!(cleanup["spec"]["kind"], "cleanup");
    assert_eq!(
        cleanup["spec"]["cleanup"],
        json!({ "run": run["id"], "vms": ["web1-drt"] })
    );
    assert_eq!(cleanup["status"]["steps"][0]["new_vm"], "web1-drt");
    assert_eq!(cleanup["status"]["steps"][0]["phase"], "pending");
    finish(&env, &cleanup["id"], DrRunState::Succeeded).await;
    let drt = env
        .cluster
        .get_by_name::<VmSpec>("web1-drt")
        .unwrap()
        .unwrap();
    env.cluster.delete::<VmSpec>(drt.id, None).await.unwrap();
    assert_error(
        &admin
            .post("/dr-plans/web-tier/actions/cleanup", json!({}))
            .await,
        StatusCode::CONFLICT,
        "dr_nothing_to_clean",
    );

    // 장애 조치: 확인 이름이 다르면 400, 운영 쪽에 닿지 않으면 planned는 409, disaster는 곧장
    assert_error(
        &admin
            .post(
                "/dr-plans/web-tier/actions/failover",
                json!({ "mode": "disaster", "confirm": "web" }),
            )
            .await,
        StatusCode::BAD_REQUEST,
        "dr_confirm_mismatch",
    );
    assert_eq!(
        admin
            .post(
                "/dr-plans/web-tier/actions/failover",
                json!({ "mode": "now", "confirm": "web-tier" })
            )
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_error(
        &admin
            .post(
                "/dr-plans/web-tier/actions/failover",
                json!({ "mode": "planned", "confirm": "web-tier" }),
            )
            .await,
        StatusCode::CONFLICT,
        "dr_source_unreachable",
    );
    let (status, fo) = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "disaster", "confirm": "web-tier" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{fo}");
    assert_eq!(fo["spec"]["kind"], "failover-disaster");
    assert_eq!(fo["status"]["steps"][1]["phase"], "pending");
    finish(&env, &fo["id"], DrRunState::Partial).await;
    let (_, report) = admin.get("/dr-plans/web-tier/report").await;
    assert_eq!(report["last_failover"]["id"], fo["id"], "{report}");
    assert_eq!(report["tests"][0]["state"], "cancelled");

    // 원격이 없으면 시험·장애 조치는 409 `dr_source_missing`, 뒷정리는 원격이 필요 없다.
    let prod = env
        .cluster
        .get_by_name::<RemoteSpec>("prod")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<RemoteSpec>(prod.id, None)
        .await
        .unwrap();
    for (path, body) in [
        ("test", json!({})),
        (
            "failover",
            json!({ "mode": "disaster", "confirm": "web-tier" }),
        ),
    ] {
        assert_error(
            &admin
                .post(&format!("/dr-plans/web-tier/actions/{path}"), body)
                .await,
            StatusCode::CONFLICT,
            "dr_source_missing",
        );
    }
}

/// SDN이 없는 클러스터에서 시험 네트워크 맵이 빠진 원본 네트워크가 있으면 시험은 400이다(거품을 만들 수 없음).
#[tokio::test]
async fn tests_without_sdn_need_every_test_network() {
    let env = dr_env().await;
    let admin = env.admin().await;
    copy(&env, "web1-a", "web1", CA, 600).await;
    let mut body = plan("web-tier");
    body["spec"]["test_networks"] = json!({});
    let (status, created) = admin.post("/dr-plans", body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let got = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "dr_test_network_required");
    assert!(
        got.1["message"].as_str().unwrap().contains("lan"),
        "{}",
        got.1
    );
    assert!(env.cluster.list::<DrRunSpec>().unwrap().1.is_empty());
    // 복구 지점이 없는 VM의 네트워크는 모른다 — 사본이 없으면 거품도 필요 없다.
    let b = env
        .cluster
        .get_by_name::<BackupSpec>("web1-a")
        .unwrap()
        .unwrap();
    env.cluster.delete::<BackupSpec>(b.id, None).await.unwrap();
    let (status, run) = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
}

/// 계획된 이전은 운영 쪽(원격 역할의 두 번째 클러스터)에 닿아야 한다. 닿으면 VM마다 운영 쪽 준비부터다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn planned_failovers_start_when_the_source_answers() {
    let env = Env::start().await;
    set_dr(&env, true).await;
    pool(&env, "dr-pool", false).await;
    let prod = Env::start().await;
    let server = prod.serve_tls().await;
    let token = prod.admin().await.token.unwrap();
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/remotes",
            json!({ "name": "prod", "url": server.url, "ca_fingerprint": prod.ca_fingerprint(), "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut p = plan("web-tier");
    p["spec"]["test_networks"] = json!({});
    let (status, body) = admin.post("/dr-plans", p).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, run) = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "planned", "confirm": "web-tier" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    assert_eq!(run["spec"]["kind"], "failover-planned");
    for s in run["status"]["steps"].as_array().unwrap() {
        assert_eq!(s["phase"], "preparing", "{run}");
        assert_eq!(s["prepare"]["stage"], "shutdown", "{run}");
    }
}

#[tokio::test]
async fn reprotect_makes_a_replicating_backup_job_for_recovered_vms() {
    let env = dr_env().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    create_plan(&env, "web-tier").await;
    let body = json!({ "remote": "prod", "pool": "prod-bk", "schedule": { "at": "03:00" }, "backup_pool": "bk" });
    assert_error(
        &admin
            .post("/dr-plans/web-tier/actions/reprotect", body.clone())
            .await,
        StatusCode::CONFLICT,
        "dr_no_recovered_vms",
    );
    // 시험 VM은 되살린 VM이 아니다.
    labelled_vm(&env, "web1-drt", DR_TEST_LABEL, "web-tier").await;
    assert_error(
        &admin
            .post("/dr-plans/web-tier/actions/reprotect", body.clone())
            .await,
        StatusCode::CONFLICT,
        "dr_no_recovered_vms",
    );
    labelled_vm(&env, "web1", DR_LABEL, "web-tier").await;
    assert_eq!(
        operator
            .post("/dr-plans/web-tier/actions/reprotect", body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // 원격·풀은 백업 작업처럼 본다(계획의 복원 풀은 백업 풀이 아니다).
    for bad in [
        json!({ "remote": "nope", "pool": "prod-bk", "schedule": { "at": "03:00" }, "backup_pool": "bk" }),
        json!({ "remote": "prod", "pool": "prod-bk", "schedule": { "at": "03:00" } }),
        json!({ "remote": "prod", "pool": "prod-bk", "schedule": { "at": "25:00" }, "backup_pool": "bk" }),
    ] {
        let (status, got) = admin
            .post("/dr-plans/web-tier/actions/reprotect", bad)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    }
    let (status, job) = admin
        .post("/dr-plans/web-tier/actions/reprotect", body.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    assert_eq!(job["name"], "dr-reprotect-web-tier");
    assert_eq!(job["labels"][DR_LABEL], "web-tier");
    assert_eq!(
        job["spec"]["vms"],
        json!({ "labels": { DR_LABEL: "web-tier" } })
    );
    assert_eq!(job["spec"]["pool"], "bk");
    assert_eq!(
        job["spec"]["replicate"],
        json!({ "remote": "prod", "pool": "prod-bk" })
    );
    assert!(job["status"]["next_run"].as_u64().is_some(), "{job}");
    let stored = env
        .cluster
        .get_by_name::<BackupJobSpec>("dr-reprotect-web-tier")
        .unwrap()
        .unwrap();
    let vm = env.cluster.get_by_name::<VmSpec>("web1").unwrap().unwrap();
    let drt = env
        .cluster
        .get_by_name::<VmSpec>("web1-drt")
        .unwrap()
        .unwrap();
    assert!(matches!(stored.spec.vms, VmSelector::Labels(_)));
    assert!(stored.spec.vms.matches(&vm) && !stored.spec.vms.matches(&drt));
    // 다시 부르면 같은 작업을 이 값으로 바꾼다.
    let (status, again) = admin
        .post("/dr-plans/web-tier/actions/reprotect", body.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["id"], job["id"]);
    assert_eq!(again["revision"], job["revision"]);
    let mut changed = body.clone();
    changed["schedule"] = json!({ "at": "04:30" });
    let (status, again) = admin
        .post("/dr-plans/web-tier/actions/reprotect", changed)
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["id"], job["id"]);
    assert_eq!(again["spec"]["schedule"]["at"], "04:30");
    assert_eq!(env.cluster.list::<BackupJobSpec>().unwrap().1.len(), 1);
}
