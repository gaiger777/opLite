//! DR 오케스트레이션 API(계획 76 3단계): 복구 계획 CRUD·권한·기능·검사, 보호 상태(리더 반복 작업 — 사본 있음·없음·
//! 나이·다른 클러스터, 바뀔 때만 쓰기), 보고, 실행 기록 읽기(직접 넣은 실행), 실행 끝점의 검사·409들·실행 리소스, 계획된
//! 이전의 원격 확인(원격 역할의 두 번째 `Env`), reprotect. 4단계 실행 엔진(`DrScheduler`): 시험 끝까지·뒷정리, on_timeout
//! stop, 취소 뒤 뒷정리, 재해 장애 조치(이름·라벨·네트워크 맵·IP, 이름 겹침), 리더 바뀜, 계획된 이전(운영 쪽 정지·백업·복제).

mod common;

use std::time::Duration;

use common::{Env, WAIT};
use orbivisor_api::{DrProtection, DrProtectionConfig};
use orbivisor_cluster::ClusterError;
use orbivisor_cluster::types::Rejection;
use orbivisor_types::backup::{Backup, BackupOrigin, BackupPhase, BackupSpec, BackupStatus};
use orbivisor_types::backup_job::{BackupJobSpec, VmSelector};
use orbivisor_types::dr::{
    DR_LABEL, DR_TEST_LABEL, DrPhase, DrRun, DrRunKind, DrRunSpec, DrRunState, DrRunStatus,
    RecoveryPlanSpec,
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

/// 이 노드가 `dr-plans`를 알리는지 바꾼다(`FEATURES`에 있어 켜져 있다 — 섞인 판 시험이 끈다).
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

    // 모든 노드가 dr-plans를 알아야 쓴다(이름 검사·검증·키 찾기보다 먼저).
    set_dr(&env, false).await;
    for body in [
        plan("q"),
        plan("Bad Name!"),
        with(&|s| s["groups"] = json!([])),
    ] {
        assert_error(&post(body).await, StatusCode::CONFLICT, "cluster_upgrading");
    }
    assert_error(
        &admin
            .put("/dr-plans/nope", json!({ "spec": plan("p")["spec"] }), None)
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
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
            allow_missing: false,
            point_in_time: None,
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

    // 시험망이 브리지(물리망)면 400(격리 아님 — 알고 진행하려면 `allow_routed_test_networks`), db1은 복구 지점이 없어 400
    // (`allow_missing`이면 그 VM은 skipped)
    let got = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    assert_error(
        &got,
        StatusCode::BAD_REQUEST,
        "dr_test_network_not_isolated",
    );
    assert_eq!(got.1["details"]["networks"], json!(["dr-test-lan"]));
    let got = admin
        .post(
            "/dr-plans/web-tier/actions/test",
            json!({ "allow_routed_test_networks": true }),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "dr_missing_recovery_points");
    assert_eq!(got.1["details"]["vms"], json!(["db1"]));
    assert!(env.cluster.list::<DrRunSpec>().unwrap().1.is_empty());
    // 시험: 실행 리소스(계획 사본·첫 상태)를 만들고 계획에 적는다.
    let (status, run) = admin
        .post(
            "/dr-plans/web-tier/actions/test",
            json!({ "allow_routed_test_networks": true, "allow_missing": true }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    assert_eq!(run["spec"]["allow_missing"], true);
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
    assert_eq!(steps[0]["phase"], "skipped");
    assert_eq!(steps[0]["error"], "no recovery point");
    // 장애 조치 준비: db1은 복구 지점이 없고, web1의 `lan`(네트워크 맵 없음 — 같은 이름)은 이 클러스터에 없다.
    let ready = &run["status"]["failover_readiness"];
    assert_eq!(ready[0]["vm"], "db1");
    assert_eq!(ready[0]["issues"], json!([{ "kind": "recovery-point" }]));
    assert_eq!(
        ready[1]["issues"],
        json!([{ "kind": "network", "name": "lan" }])
    );
    assert_eq!(steps[1]["vm"], "web1");
    assert_eq!(steps[1]["phase"], "pending");
    assert_eq!(steps[1]["backup"], "web1-a");
    // `lan`은 시험 네트워크로 맵이 있어 거품 네트워크가 없다.
    assert!(run["status"].get("bubbles").is_none(), "{run}");
    let (_, got) = admin.get("/dr-plans/web-tier").await;
    assert_eq!(got["status"]["run"], run["id"]);

    // 도는 실행이 있으면 다른 실행은 409(장애 조치는 도는 시험을 끊는다 — 따로 시험)
    for (path, body) in [("test", json!({})), ("cleanup", json!({}))] {
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
    // 시험이 만든 VM(실행 기록의 id + 시험 라벨)만 남은 것이다. 라벨만 단 VM, 다른 계획의 시험 VM, 장애 조치 VM은 아니다.
    let made = labelled_vm(&env, "web1-drt", DR_TEST_LABEL, "web-tier").await;
    labelled_vm(&env, "stray-drt", DR_TEST_LABEL, "web-tier").await;
    labelled_vm(&env, "other-drt", DR_TEST_LABEL, "other").await;
    labelled_vm(&env, "web1", DR_LABEL, "web-tier").await;
    let id: Uuid = run["id"].as_str().unwrap().parse().unwrap();
    let mut rec = env.cluster.get::<DrRunSpec>(id).unwrap().unwrap();
    rec.status.steps[1].vm_id = Some(made);
    rec.status.steps[1].new_vm = Some("web1-drt".into());
    env.cluster
        .update_status::<DrRunSpec>(id, &rec.status)
        .await
        .unwrap();
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
        json!({ "run": run["id"], "vms": ["web1-drt"], "vm_ids": [made] })
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
    assert_error(
        &admin
            .post(
                "/dr-plans/web-tier/actions/failover",
                json!({ "mode": "disaster", "confirm": "web-tier" }),
            )
            .await,
        StatusCode::BAD_REQUEST,
        "dr_missing_recovery_points",
    );
    // `point_in_time`: 그 시각 이전의 사본이 없으면 web1도 빠진다.
    let got = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "disaster", "confirm": "web-tier", "point_in_time": now() - 3600 }),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "dr_missing_recovery_points");
    assert_eq!(got.1["details"]["vms"], json!(["db1", "web1"]));
    let (status, fo) = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "disaster", "confirm": "web-tier", "allow_missing": true }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{fo}");
    assert_eq!(fo["spec"]["kind"], "failover-disaster");
    assert_eq!(fo["status"]["steps"][0]["phase"], "skipped");
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
        .post(
            "/dr-plans/web-tier/actions/test",
            json!({ "allow_missing": true }),
        )
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
        .post(
            "/dr-plans/web-tier/actions/test",
            json!({ "allow_missing": true }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
}

/// 계획된 이전은 운영 쪽(원격 역할의 두 번째 클러스터)에 닿아야 하고, 운영 VM을 끄기 전에 사전 검사를 모두 지나야 한다
/// (409 `dr_planned_preflight` — 운영 쪽에 이 클러스터의 원격이 없음, VM이 없음, 토큰이 admin이 아님). 아무것도 끄지 않고
/// 실행도 만들지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn planned_failovers_check_the_source_before_stopping_anything() {
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
    let got = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "planned", "confirm": "web-tier" }),
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "dr_planned_preflight");
    let codes: Vec<(String, Option<String>)> = got.1["details"]["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["code"].as_str().unwrap().to_string(),
                p["vm"].as_str().map(str::to_string),
            )
        })
        .collect();
    assert_eq!(
        codes,
        [
            ("no_remote_here".to_string(), None),
            ("vm_missing".to_string(), Some("db1".to_string())),
            ("vm_missing".to_string(), Some("web1".to_string())),
        ],
        "{}",
        got.1
    );
    assert!(env.cluster.list::<DrRunSpec>().unwrap().1.is_empty());
    // viewer 토큰이면 그것도 문제다(운영 쪽 복제는 admin).
    let viewer = prod.user("viewer", "viewer").await;
    let remote = env
        .cluster
        .get_by_name::<RemoteSpec>("prod")
        .unwrap()
        .unwrap();
    let mut spec = remote.spec.clone();
    spec.token = viewer.token.clone().unwrap();
    env.cluster
        .update_spec(remote.id, None, remote.labels.clone(), &spec)
        .await
        .unwrap();
    let got = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "planned", "confirm": "web-tier" }),
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "dr_planned_preflight");
    assert_eq!(
        got.1["details"]["problems"][0]["code"], "token_role",
        "{}",
        got.1
    );
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
    // 백업 작업 화면에서 보존 규칙을 고친 뒤 다시 보호해도(보존·시각을 주지 않음) 그대로 둔다.
    let mut stored = env
        .cluster
        .get_by_name::<BackupJobSpec>("dr-reprotect-web-tier")
        .unwrap()
        .unwrap();
    stored.spec.keep.last = 7;
    env.cluster
        .update_spec(stored.id, None, stored.labels.clone(), &stored.spec)
        .await
        .unwrap();
    let (status, again) = admin
        .post(
            "/dr-plans/web-tier/actions/reprotect",
            json!({ "remote": "prod", "pool": "prod-bk" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["spec"]["keep"]["last"], 7, "{again}");
    assert_eq!(again["spec"]["schedule"]["at"], "04:30");
    assert_eq!(again["spec"]["pool"], "bk");
}

// ---- 4단계: 실행 엔진 ----

/// 게스트 에이전트: `answer`가 켜져 있으면 모든 VM이 `10.20.0.9`를 알린다.
#[derive(Default)]
struct Agent {
    answer: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl orbivisor_peer::GuestAgentSource for Agent {
    async fn info(&self, _vm: Uuid, _refresh: bool) -> orbivisor_types::guest::GuestInfoResponse {
        let answer = self.answer.load(std::sync::atomic::Ordering::SeqCst);
        serde_json::from_value(if answer {
            json!({ "state": "answered", "info": { "collected_at": 1, "interfaces": [
                { "name": "eth0", "addresses": [ { "ip": "10.20.0.9", "prefix": 24 } ] } ] } })
        } else {
            json!({ "state": "no_agent" })
        })
        .unwrap()
    }

    async fn set_password(
        &self,
        _vm: Uuid,
        _user: &str,
        _password: &str,
    ) -> Result<(), orbivisor_peer::GuestAgentError> {
        Ok(())
    }
}

/// 가짜 OVN 실행기(태그만 쓴다)
fn ovn_host(dir: &std::path::Path) -> std::sync::Arc<orbivisor_driver::ovn::OvnHost> {
    use orbivisor_driver::ovn::{FakeRunner, OvnHost, OvnOptions, OvnPaths};
    let paths = OvnPaths {
        etc: dir.join("etc"),
        var_lib: dir.join("var"),
        run: dir.join("run"),
        log: dir.join("log"),
        systemd: dir.join("systemd"),
        pki: dir.join("pki"),
        schemas: dir.join("schemas"),
        distro_nb_db: dir.join("nb.db"),
        ovs_system_id: dir.join("system-id.conf"),
    };
    std::sync::Arc::new(OvnHost::new(
        std::sync::Arc::new(FakeRunner::new()),
        paths,
        OvnOptions::default(),
        "abcdef012345".into(),
    ))
}

/// 이 노드에서 준비된 dir 풀
async fn ready_pool(env: &Env, name: &str, backups: bool) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 엔진 시험의 DR 쪽: 파일 없는 스토리지·가짜 OVN으로 VM을 켜는 노드(chassis), 세운 SDN, 풀 `dr-pool`·백업 풀 `bk`, 원격
/// `prod`, 같은 이름의 SDN 네트워크 `lan`(10.20.0.0/24)과 `dr-lan`(10.30.0.0/24), 게스트 에이전트 `agent`
async fn engine_env(agent: std::sync::Arc<Agent>) -> Env {
    use orbivisor_types::node::NodeHotplug;
    use orbivisor_types::sdn::{ChassisState, NodeSdn};
    let mut env = Box::pin(Env::start_with_state(move |s| s.guest = Some(agent))).await;
    env.stop_runner().await;
    let host = ovn_host(&env.dir.path().join("ovn"));
    Box::pin(env.start_runner_config(
        std::sync::Arc::new(common::FakeStorage::default()),
        move |c| c.sdn = Some(host),
    ))
    .await;
    let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    me.status.hotplug = Some(NodeHotplug::default());
    me.status.sdn = Some(NodeSdn {
        chassis: ChassisState::Ready,
        ..NodeSdn::default()
    });
    env.cluster
        .update_status::<NodeSpec>(env.node, &me.status)
        .await
        .unwrap();
    let mut sdn: orbivisor_types::sdn::Sdn = Resource::new(
        orbivisor_types::sdn::SDN_NAME,
        orbivisor_types::sdn::SdnSpec::default(),
    );
    sdn.status.phase = orbivisor_types::sdn::SdnPhase::Ready;
    sdn.status.centrals = vec![orbivisor_types::sdn::SdnCentral {
        node: env.node,
        address: "10.1.0.5".parse().unwrap(),
    }];
    env.cluster.create(&sdn).await.unwrap();
    set_dr(&env, true).await;
    ready_pool(&env, "dr-pool", false).await;
    ready_pool(&env, "bk", true).await;
    remote(&env, "prod", CA).await;
    for (name, cidr) in [("lan", "10.20.0.0/24"), ("dr-lan", "10.30.0.0/24")] {
        let net: orbivisor_types::network::Network = Resource::new(
            name,
            serde_json::from_value(json!({ "sdn": {}, "subnets": [ { "cidr": cidr } ] })).unwrap(),
        );
        env.cluster.create(&net).await.unwrap();
    }
    env
}

/// 운영 클러스터(`CA`)에서 온, 되살릴 수 있는 사본(리소스와 백업 풀 `bk`의 매니페스트, 디스크 없음). `agent`면 게스트
/// 에이전트 채널이 있는 VM이다.
async fn restorable(env: &Env, name: &str, vm: &str, agent: bool) -> Uuid {
    use orbivisor_types::backup::{BackupManifest, MANIFEST_FILE, MANIFEST_VERSION, backup_dir};
    let mut vm_spec = vm_spec(&["lan"]);
    if agent {
        vm_spec.guest_agent = Some(serde_json::from_value(json!({})).unwrap());
    }
    let source = Uuid::now_v7();
    let mut b: Backup = Resource::new(
        name,
        BackupSpec {
            vm: source,
            vm_name: vm.into(),
            pool: "bk".into(),
            store: None,
            source: None,
            vm_spec: vm_spec.clone(),
            job: None,
            origin: Some(BackupOrigin {
                cluster: "prod".into(),
                ca_fingerprint: CA.into(),
                backup: Uuid::now_v7(),
                job: None,
            }),
            incremental: false,
            max_chain: None,
        },
    );
    b.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(now() - 600),
        node: Some(env.node),
        ..Default::default()
    };
    env.cluster.create(&b).await.unwrap();
    let dir = backup_dir(&env.dir.path().join("bk"), b.id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = BackupManifest {
        version: MANIFEST_VERSION,
        backup: b.id,
        parent: None,
        vm: source,
        vm_name: vm.into(),
        vm_spec,
        disks: vec![],
        uefi_vars: false,
        created_at: now() - 600,
        tpm_state: false,
    };
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    b.id
}

/// 그룹 둘(`db`는 게스트 에이전트를 기다림, `web`은 고정 IP)의 계획 `spec`
fn engine_plan() -> Value {
    json!({
        "source": "prod",
        "pool": "dr-pool",
        "groups": [
            { "name": "db", "wait": "guest", "timeout_secs": 60, "vms": [ { "vm": "db1" } ] },
            { "name": "web", "delay_secs": 1, "vms": [ { "vm": "web1", "ip": [ { "nic": 0, "address": "10.20.0.5/24", "gateway": "10.20.0.1" } ] } ] }
        ]
    })
}

fn scheduler(env: &Env) -> orbivisor_api::DrScheduler {
    orbivisor_api::DrScheduler::spawn(
        env.state.clone(),
        orbivisor_api::DrSchedulerConfig {
            tick: Duration::from_millis(50),
        },
    )
}

/// 엔진 시험의 기다림(Task·VM 켜기가 여러 걸음이라 길게)
async fn until_long(why: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{why}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn run_of(env: &Env, id: &Value) -> DrRun {
    let id: Uuid = id.as_str().unwrap().parse().unwrap();
    env.cluster.get::<DrRunSpec>(id).unwrap().unwrap()
}

async fn finished(env: &Env, id: &Value) -> DrRun {
    until_long("the DR run did not finish", || {
        run_of(env, id).status.state.is_finished()
    })
    .await;
    run_of(env, id)
}

fn vm_named(env: &Env, name: &str) -> Option<orbivisor_types::vm::Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

/// 계획 상태(끝난 실행을 계획에 적기 — `status.run` 비우기 — 를 기다린 뒤)
async fn settled(env: &Env, name: &str) -> orbivisor_types::dr::RecoveryPlanStatus {
    until_long("the plan still points at the finished run", || {
        plan_status(env, name).run.is_none()
    })
    .await;
    plan_status(env, name)
}

fn plan_status(env: &Env, name: &str) -> orbivisor_types::dr::RecoveryPlanStatus {
    env.cluster
        .get_by_name::<RecoveryPlanSpec>(name)
        .unwrap()
        .unwrap()
        .status
}

/// `name`으로 되살리는 복원 Task들
fn restore_tasks(env: &Env, name: &str) -> Vec<orbivisor_types::task::Task> {
    use orbivisor_types::task::{TaskOp, TaskSpec};
    env.cluster
        .list::<TaskSpec>()
        .unwrap()
        .1
        .into_iter()
        .filter(|t| matches!(&t.spec.op, TaskOp::BackupRestore { name: n, .. } if n == name))
        .collect()
}

fn start_tasks(env: &Env, vm: Uuid) -> usize {
    use orbivisor_types::task::{TaskOp, TaskSpec};
    env.cluster
        .list::<TaskSpec>()
        .unwrap()
        .1
        .iter()
        .filter(|t| t.spec.target == vm && t.spec.op == TaskOp::VmStart)
        .count()
}

/// 시험 장애 조치를 끝까지: 그룹 차례(db가 게스트 에이전트에 답한 뒤 web), 거품 네트워크(DR `lan`의 서브넷)를 만들고 NIC가
/// 거기 붙음(web1의 고정 IP는 그 서브넷의 IPAM 주소), 시험 이름·폴더·라벨, RTO·계획의 `last_test`. 그 뒤 뒷정리가 VM과
/// 거품 네트워크를 지운다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_run_recovers_groups_in_a_bubble_and_cleanup_removes_them() {
    let agent = std::sync::Arc::new(Agent::default());
    let env = engine_env(agent.clone()).await;
    let admin = env.admin().await;
    restorable(&env, "db1-c", "db1", true).await;
    restorable(&env, "web1-c", "web1", false).await;
    let (status, body) = admin
        .post(
            "/dr-plans",
            json!({ "name": "web-tier", "spec": engine_plan() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, run) = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    assert_eq!(
        run["status"]["bubbles"][0]["name"], "drt-web-tier-1",
        "{run}"
    );
    let looper = scheduler(&env);

    // db1-drt가 켜졌어도 게스트 에이전트가 답할 때까지 web은 시작하지 않는다.
    until_long("db1-drt never started", || {
        vm_named(&env, "db1-drt")
            .is_some_and(|v| v.status.phase == orbivisor_types::vm::VmPhase::Running)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        run_of(&env, &run["id"]).status.steps[0].phase,
        DrPhase::Waiting
    );
    assert!(vm_named(&env, "web1-drt").is_none());
    assert!(restore_tasks(&env, "web1-drt").is_empty());
    agent
        .answer
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let done = finished(&env, &run["id"]).await;
    assert_eq!(
        done.status.state,
        DrRunState::Succeeded,
        "{:?}",
        done.status
    );
    let rto = done.status.rto_secs.expect("rto");
    assert!(rto < 60, "{rto}");
    let db_ready = done.status.steps[0].at;
    assert!(done.status.steps.iter().all(|s| s.phase == DrPhase::Ready));
    // web은 db가 준비된 뒤에(1초 쉼) 복원했다.
    let web_restore = restore_tasks(&env, "web1-drt");
    assert_eq!(web_restore.len(), 1);
    let made = web_restore[0].id.get_timestamp().unwrap().to_unix().0;
    assert!(made >= db_ready, "{made} < {db_ready}");
    // 거품 네트워크: SDN, 라우터 없음, DR `lan`의 서브넷, 시험 라벨
    let bubble = env
        .cluster
        .get_by_name::<orbivisor_types::network::NetworkSpec>("drt-web-tier-1")
        .unwrap()
        .expect("bubble");
    assert!(bubble.spec.is_sdn());
    assert_eq!(bubble.spec.subnets[0].cidr.to_string(), "10.20.0.0/24");
    assert_eq!(bubble.labels[DR_TEST_LABEL], "web-tier");
    for name in ["db1-drt", "web1-drt"] {
        let vm = vm_named(&env, name).unwrap();
        assert_eq!(vm.labels[DR_TEST_LABEL], "web-tier", "{name}");
        assert!(!vm.labels.contains_key(DR_LABEL));
        assert_eq!(vm.spec.nics[0].network, "drt-web-tier-1", "{name}");
        assert!(vm.spec.ha.is_none());
        let folder = env
            .cluster
            .get::<orbivisor_types::folder::FolderSpec>(vm.spec.folder.expect("folder"))
            .unwrap()
            .unwrap();
        assert_eq!(folder.spec.name, "web-tier");
        let parent = env
            .cluster
            .get::<orbivisor_types::folder::FolderSpec>(folder.spec.parent.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(parent.spec.name, "dr-test");
    }
    assert_eq!(
        vm_named(&env, "web1-drt").unwrap().spec.nics[0].ips,
        ["10.20.0.5".parse::<std::net::IpAddr>().unwrap()]
    );
    // 계획: 지금 실행 없음, 마지막 시험
    let st = settled(&env, "web-tier").await;
    assert_eq!(st.run, None);
    let last = st.last_test.expect("last_test");
    assert_eq!(last.run, done.id);
    assert_eq!(last.result, DrRunState::Succeeded);
    assert_eq!(last.rto_secs, Some(rto));

    // 뒷정리: 켜진 시험 VM을 끄고 지운 뒤 거품 네트워크
    let (status, cleanup) = admin
        .post("/dr-plans/web-tier/actions/cleanup", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{cleanup}");
    assert_eq!(
        cleanup["spec"]["cleanup"]["networks"],
        json!(["drt-web-tier-1"])
    );
    let done = finished(&env, &cleanup["id"]).await;
    assert_eq!(
        done.status.state,
        DrRunState::Succeeded,
        "{:?}",
        done.status
    );
    assert!(vm_named(&env, "db1-drt").is_none());
    assert!(vm_named(&env, "web1-drt").is_none());
    assert!(
        env.cluster
            .get_by_name::<orbivisor_types::network::NetworkSpec>("drt-web-tier-1")
            .unwrap()
            .is_none()
    );
    assert_eq!(settled(&env, "web-tier").await.run, None);
    // 시험 결과는 뒷정리가 바꾸지 않는다.
    assert_eq!(
        plan_status(&env, "web-tier").last_test.unwrap().result,
        DrRunState::Succeeded
    );
    looper.shutdown().await;
}

async fn make_plan(admin: &common::Client, spec: Value) {
    let (status, body) = admin
        .post("/dr-plans", json!({ "name": "web-tier", "spec": spec }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// `on_timeout: stop`: 앞 그룹의 VM이 실패하면(복구 지점의 파일이 없어 복원 Task 실패) 남은 VM은 `skipped`이고 실행은
/// `failed`(준비된 VM 없음). 시험 VM이 없어도 계획의 `last_test`에 남는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_timeout_stop_skips_the_rest_after_a_failure() {
    let env = engine_env(std::sync::Arc::new(Agent::default())).await;
    let admin = env.admin().await;
    let db = restorable(&env, "db1-c", "db1", true).await;
    restorable(&env, "web1-c", "web1", false).await;
    std::fs::remove_dir_all(orbivisor_types::backup::backup_dir(
        &env.dir.path().join("bk"),
        db,
    ))
    .unwrap();
    let mut spec = engine_plan();
    spec["on_timeout"] = json!("stop");
    spec["test_networks"] = json!({ "lan": "dr-lan" });
    make_plan(&admin, spec).await;
    let (status, run) = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let looper = scheduler(&env);
    let done = finished(&env, &run["id"]).await;
    assert_eq!(done.status.state, DrRunState::Failed, "{:?}", done.status);
    assert_eq!(done.status.steps[0].phase, DrPhase::Failed);
    assert!(done.status.steps[0].error.is_some());
    assert_eq!(done.status.steps[1].phase, DrPhase::Skipped);
    assert!(restore_tasks(&env, "web1-drt").is_empty());
    assert!(done.status.bubbles.is_empty(), "test_networks map lan");
    let st = settled(&env, "web-tier").await;
    assert_eq!(st.run, None);
    assert_eq!(st.last_test.unwrap().result, DrRunState::Failed);
    looper.shutdown().await;
}

/// 시험을 취소하면 진행 중인 단계 뒤에 멈추고(기다리던 db1은 `skipped`, web은 시작하지 않음) 끝난 상태를 저장한 뒤 뒷정리
/// 실행이 시험 VM과 거품 네트워크를 지운다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_test_cleans_up_after_it() {
    let env = engine_env(std::sync::Arc::new(Agent::default())).await;
    let admin = env.admin().await;
    restorable(&env, "db1-c", "db1", true).await;
    restorable(&env, "web1-c", "web1", false).await;
    make_plan(&admin, engine_plan()).await;
    let (_, run) = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    let looper = scheduler(&env);
    // 게스트 에이전트가 답하지 않아 db1은 기다린다.
    until_long("db1 never waited", || {
        run_of(&env, &run["id"]).status.steps[0].phase == DrPhase::Waiting
    })
    .await;
    let (status, body) = admin
        .post("/dr-plans/web-tier/actions/cancel", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let done = finished(&env, &run["id"]).await;
    assert_eq!(
        done.status.state,
        DrRunState::Cancelled,
        "{:?}",
        done.status
    );
    assert!(
        done.status
            .steps
            .iter()
            .all(|s| s.phase == DrPhase::Skipped)
    );
    assert!(vm_named(&env, "web1-drt").is_none());
    // 뒷정리 실행이 생겨 끝까지 간다.
    until_long("no cleanup run", || {
        env.cluster
            .list::<DrRunSpec>()
            .unwrap()
            .1
            .iter()
            .any(|r| r.spec.kind == DrRunKind::Cleanup && r.status.state == DrRunState::Succeeded)
    })
    .await;
    looper.shutdown().await;
    let cleanup = env
        .cluster
        .list::<DrRunSpec>()
        .unwrap()
        .1
        .into_iter()
        .find(|r| r.spec.kind == DrRunKind::Cleanup)
        .unwrap();
    assert_eq!(cleanup.spec.cleanup.as_ref().unwrap().run, done.id);
    assert!(vm_named(&env, "db1-drt").is_none());
    assert!(
        env.cluster
            .get_by_name::<orbivisor_types::network::NetworkSpec>("drt-web-tier-1")
            .unwrap()
            .is_none()
    );
    let st = settled(&env, "web-tier").await;
    assert_eq!(st.run, None);
    assert_eq!(st.last_test.unwrap().result, DrRunState::Cancelled);
}

/// 재해 장애 조치: 원본 이름·라벨 `orbivisor.io/dr`, 네트워크 맵(`lan` → `dr-lan`)과 그 서브넷의 고정 IP. 이름이 이미
/// 쓰인 VM(db1)만 `dr_name_taken`으로 실패하고(그 VM은 건드리지 않음) 실행은 `partial`. 시험이 아니라 `last_test`는 없다.
/// 계획의 폴더가 없으면 복원 기본(백업한 VM의 폴더 — 여기서는 없음, 루트)에 둔다(VM을 실패시키지 않음).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disaster_failover_uses_source_names_and_maps() {
    let env = engine_env(std::sync::Arc::new(Agent::default())).await;
    let admin = env.admin().await;
    restorable(&env, "db1-c", "db1", true).await;
    restorable(&env, "web1-c", "web1", false).await;
    let theirs = labelled_vm(&env, "db1", "owner", "someone").await;
    let mut spec = engine_plan();
    spec["networks"] = json!({ "lan": "dr-lan" });
    spec["folder"] = json!("/dr/missing");
    spec["groups"][0]["wait"] = json!("running");
    spec["groups"][1]["vms"][0]["ip"] =
        json!([ { "nic": 0, "address": "10.30.0.7/24", "gateway": "10.30.0.1" } ]);
    make_plan(&admin, spec).await;
    let (status, run) = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "disaster", "confirm": "web-tier" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let looper = scheduler(&env);
    let done = finished(&env, &run["id"]).await;
    assert_eq!(done.status.state, DrRunState::Partial, "{:?}", done.status);
    let db = &done.status.steps[0];
    assert_eq!(db.phase, DrPhase::Failed);
    assert!(
        db.error.as_deref().unwrap().starts_with("dr_name_taken"),
        "{db:?}"
    );
    let mine = env.cluster.get::<VmSpec>(theirs).unwrap().unwrap();
    assert!(!mine.labels.contains_key(DR_LABEL));
    assert_eq!(done.status.steps[1].phase, DrPhase::Ready);
    let web = vm_named(&env, "web1").unwrap();
    assert_eq!(web.labels[DR_LABEL], "web-tier");
    assert_eq!(web.spec.folder, None);
    assert_eq!(web.spec.nics[0].network, "dr-lan");
    assert_eq!(
        web.spec.nics[0].ips,
        ["10.30.0.7".parse::<std::net::IpAddr>().unwrap()]
    );
    assert!(done.status.rto_secs.is_some());
    assert_eq!(done.status.steps[0].failed_in, Some(DrPhase::Restoring));
    let st = settled(&env, "web-tier").await;
    assert_eq!(st.run, None);
    assert!(st.last_test.is_none());
    assert_eq!(st.source_fence_pending, ["web1"]);
    // 부분 실패 뒤 다시 돌리면 이 계획이 되살린 VM(web1)은 그대로 두고(`skipped`) 나머지만 다시 한다.
    let (status, again) = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "disaster", "confirm": "web-tier" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{again}");
    let again = finished(&env, &again["id"]).await;
    assert_eq!(
        again.status.steps[1].phase,
        DrPhase::Skipped,
        "{:?}",
        again.status
    );
    assert_eq!(
        again.status.steps[1].error.as_deref(),
        Some("already recovered by this plan")
    );
    assert_eq!(restore_tasks(&env, "web1").len(), 1);
    looper.shutdown().await;
}

/// 리더가 바뀜: 첫 스케줄러가 복원 Task를 내고 멈추고, 그 걸음의 기록을 통째로 잃은 것처럼(단계가 아직 `pending`) 둔다.
/// 새 스케줄러는 몇 초 뒤 같은 기록에서 이어 가며(단계를 지금 시각으로 다시 든다) 이미 낸 Task를 찾아 쓴다 — 실행 시작
/// 시각으로 찾으므로 복원·시작 Task는 VM마다 하나다(리뷰 1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_leader_continues_without_issuing_tasks_twice() {
    let mut env = engine_env(std::sync::Arc::new(Agent::default())).await;
    let admin = env.admin().await;
    restorable(&env, "web1-c", "web1", false).await;
    make_plan(
        &admin,
        json!({ "source": "prod", "pool": "dr-pool",
                "groups": [ { "name": "web", "vms": [ { "vm": "web1" } ] } ] }),
    )
    .await;
    // 새 실행은 설정의 동시 수를 가진다.
    let (status, body) = admin
        .patch("/settings", json!({ "dr_max_parallel": 2 }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Task 실행기를 멈춰 복원 Task가 끝나지 않게 한다.
    env.stop_runner().await;
    let (_, run) = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "disaster", "confirm": "web-tier" }),
        )
        .await;
    assert_eq!(run["spec"]["max_parallel"], 2, "{run}");
    let first = scheduler(&env);
    until_long("no restore task", || {
        run_of(&env, &run["id"]).status.steps[0].task.is_some()
    })
    .await;
    first.shutdown().await;
    let issued = run_of(&env, &run["id"]).status.steps[0].task.unwrap();
    // 저장 전에 리더가 바뀐 것처럼 단계를 `pending`으로 되돌린다(Task는 남음).
    let mut lost = run_of(&env, &run["id"]);
    assert_eq!(lost.status.steps[0].phase, DrPhase::Restoring);
    let step = &mut lost.status.steps[0];
    step.phase = DrPhase::Pending;
    step.task = None;
    step.new_vm = None;
    step.vm_id = None;
    env.cluster
        .update_status::<DrRunSpec>(lost.id, &lost.status)
        .await
        .unwrap();
    // 단계 시각으로 찾던 옛 길은 이만큼 지나면 놓쳤다.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let second = scheduler(&env);
    until_long("the new leader did not pick up the task", || {
        run_of(&env, &run["id"]).status.steps[0].task == Some(issued)
    })
    .await;
    assert_eq!(restore_tasks(&env, "web1").len(), 1);
    env.start_runner_with(std::sync::Arc::new(common::FakeStorage::default()))
        .await;
    let done = finished(&env, &run["id"]).await;
    second.shutdown().await;
    assert_eq!(
        done.status.state,
        DrRunState::Succeeded,
        "{:?}",
        done.status
    );
    assert_eq!(restore_tasks(&env, "web1").len(), 1);
    let web = vm_named(&env, "web1").unwrap();
    assert_eq!(start_tasks(&env, web.id), 1);
}

/// 계획된 이전: 운영 쪽(원격 역할의 두 번째 클러스터 — 이 클러스터를 원격 `dr`로 등록함)이 정지·백업·복제를 받는다. 켜진
/// 운영 VM을 끄고, 실행이 정한 이름으로 그 VM의 백업 풀에 백업한 뒤 이 클러스터로 복제하고, 사본이 오면 그것으로 되살린다.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_planned_failover_stops_backs_up_and_replicates_the_source_first() {
    let prod = Env::start().await;
    ready_pool(&prod, "backups", true).await;
    let prod_admin = prod.admin().await;
    let (status, vm) = prod_admin.post("/vms", common::vm_request("web1")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = prod_admin.act("web1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // 운영 쪽의 지난 백업(마지막 백업이 이 풀을 쓴다)
    let (status, task) = prod_admin
        .post("/vms/web1/backups", json!({ "pool": "backups" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = prod_admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let mut env = Env::start().await;
    env.stop_runner().await;
    env.start_runner_with(std::sync::Arc::new(common::FakeStorage::default()))
        .await;
    set_dr(&env, true).await;
    ready_pool(&env, "dr-pool", false).await;
    ready_pool(&env, "bk", true).await;
    let admin = env.admin().await;
    let prod_server = prod.serve_tls().await;
    let dr_server = env.serve_tls().await;
    let (status, body) = admin
        .post(
            "/remotes",
            json!({ "name": "prod", "url": prod_server.url, "ca_fingerprint": prod.ca_fingerprint(),
                    "token": prod_admin.token.clone().unwrap() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = prod_admin
        .post(
            "/remotes",
            json!({ "name": "dr", "url": dr_server.url, "ca_fingerprint": env.ca_fingerprint(),
                    "token": admin.token.clone().unwrap() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    make_plan(
        &admin,
        json!({ "source": "prod", "pool": "dr-pool",
                "groups": [ { "name": "web", "vms": [ { "vm": "web1" } ] } ] }),
    )
    .await;
    let (status, run) = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "planned", "confirm": "web-tier" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    let looper = scheduler(&env);
    let done = finished(&env, &run["id"]).await;
    assert_eq!(
        done.status.state,
        DrRunState::Succeeded,
        "{:?}",
        done.status
    );
    assert!(done.status.prepare_secs.is_some());
    let step = &done.status.steps[0];
    let prepare = step.prepare.as_ref().unwrap();
    let source_backup = prepare.source_backup.clone().unwrap();
    assert!(source_backup.starts_with("web1-dr-"), "{source_backup}");
    // 운영 쪽: VM은 꺼졌고 실행의 백업이 있으며 이 클러스터로 복제됐다.
    let source_vm = prod.cluster.get_by_name::<VmSpec>("web1").unwrap().unwrap();
    assert_eq!(
        source_vm.status.phase,
        orbivisor_types::vm::VmPhase::Stopped
    );
    let b = prod
        .cluster
        .get_by_name::<BackupSpec>(&source_backup)
        .unwrap()
        .unwrap();
    assert_eq!(b.spec.pool, "backups");
    assert_eq!(b.status.replicas[0].remote, "dr");
    // 이 클러스터: 그 사본으로 되살린 원본 이름의 VM
    let copy = env
        .cluster
        .get_by_name::<BackupSpec>(step.backup.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(copy.spec.origin.as_ref().unwrap().backup, b.id);
    assert_eq!(copy.spec.pool, "bk");
    let web = vm_named(&env, "web1").unwrap();
    assert_eq!(web.labels[DR_LABEL], "web-tier");
    assert_eq!(settled(&env, "web-tier").await.run, None);
    looper.shutdown().await;
}

// ---- 리뷰 1 ----

/// 시험 도중 진짜 장애: 장애 조치가 도는 시험을 곧 끊고(`cancelled`·`preempted`, 진행 중 Task는 그대로) 시작한다. 끊은 시험이
/// 만든 VM에도 시험 라벨을 달고, 장애 조치가 끝난 뒤에 그 뒷정리가 시작된다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failover_preempts_a_running_test() {
    let mut env = engine_env(std::sync::Arc::new(Agent::default())).await;
    let admin = env.admin().await;
    restorable(&env, "db1-c", "db1", true).await;
    restorable(&env, "web1-c", "web1", false).await;
    let mut spec = engine_plan();
    spec["groups"][0]["wait"] = json!("running");
    make_plan(&admin, spec).await;
    env.stop_runner().await;
    let (status, test) = admin
        .post("/dr-plans/web-tier/actions/test", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{test}");
    let first = scheduler(&env);
    until_long("the test never issued its restore", || {
        run_of(&env, &test["id"]).status.steps[0].task.is_some()
    })
    .await;
    first.shutdown().await;
    let (status, fo) = admin
        .post(
            "/dr-plans/web-tier/actions/failover",
            json!({ "mode": "disaster", "confirm": "web-tier" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{fo}");
    assert_eq!(fo["status"]["preempted_test"], test["id"]);
    let cut = run_of(&env, &test["id"]);
    assert_eq!(cut.status.state, DrRunState::Cancelled);
    assert!(cut.status.preempted);
    assert!(cut.status.steps.iter().all(|s| s.phase.is_terminal()));
    assert_eq!(
        plan_status(&env, "web-tier").run.map(|r| r.to_string()),
        fo["id"].as_str().map(str::to_string)
    );
    // 끊은 시험의 복원 Task가 끝나 시험 VM이 생긴다.
    env.start_runner_with(std::sync::Arc::new(common::FakeStorage::default()))
        .await;
    until_long("the test VM never appeared", || {
        vm_named(&env, "db1-drt").is_some()
    })
    .await;
    let looper = scheduler(&env);
    let done = finished(&env, &fo["id"]).await;
    assert_eq!(
        done.status.state,
        DrRunState::Succeeded,
        "{:?}",
        done.status
    );
    until_long("no cleanup after the failover", || {
        env.cluster
            .list::<DrRunSpec>()
            .unwrap()
            .1
            .iter()
            .any(|r| r.spec.kind == DrRunKind::Cleanup && r.status.state == DrRunState::Succeeded)
    })
    .await;
    looper.shutdown().await;
    assert!(vm_named(&env, "db1-drt").is_none());
    assert_eq!(vm_named(&env, "db1").unwrap().labels[DR_LABEL], "web-tier");
    // 재해 장애 조치로 되살린 VM은 운영 쪽에서 멈출 때까지 목록에 남는다.
    assert_eq!(
        settled(&env, "web-tier").await.source_fence_pending,
        ["db1", "web1"]
    );
}

/// fence-source: 재해 뒤 돌아온 운영 쪽(두 번째 클러스터)의 그 VM을 강제로 끄고 원하는 상태를 stopped로 둔다. 운영 쪽에 없는
/// VM도 멈춘 것으로 보고 목록에서 뺀다. 목록이 비면 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fence_source_stops_the_recovered_vms_on_the_returned_source() {
    let prod = Env::start().await;
    let prod_admin = prod.admin().await;
    let (status, vm) = prod_admin.post("/vms", common::vm_request("web1")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = prod_admin.act("web1", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let env = Env::start().await;
    set_dr(&env, true).await;
    pool(&env, "dr-pool", false).await;
    let admin = env.admin().await;
    let server = prod.serve_tls().await;
    let (status, body) = admin
        .post(
            "/remotes",
            json!({ "name": "prod", "url": server.url, "ca_fingerprint": prod.ca_fingerprint(),
                    "token": prod_admin.token.clone().unwrap() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut p = plan("web-tier");
    p["spec"]["test_networks"] = json!({});
    let (status, created) = admin.post("/dr-plans", p).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_error(
        &admin
            .post("/dr-plans/web-tier/actions/fence-source", json!({}))
            .await,
        StatusCode::CONFLICT,
        "dr_nothing_to_fence",
    );
    let plan = env
        .cluster
        .get_by_name::<RecoveryPlanSpec>("web-tier")
        .unwrap()
        .unwrap();
    let mut st = plan.status.clone();
    st.source_fence_pending = vec!["db1".into(), "web1".into()];
    env.cluster
        .update_status::<RecoveryPlanSpec>(plan.id, &st)
        .await
        .unwrap();
    let (status, got) = admin
        .post("/dr-plans/web-tier/actions/fence-source", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["pending"], json!([]), "{got}");
    assert_eq!(got["results"][0]["vm"], "db1");
    assert_eq!(got["results"][0]["error"], "not on the source cluster");
    assert_eq!(got["results"][1]["ok"], true);
    let source_vm = prod.cluster.get_by_name::<VmSpec>("web1").unwrap().unwrap();
    assert_eq!(
        source_vm.spec.power,
        orbivisor_types::vm::PowerState::Stopped
    );
    assert!(
        plan_status(&env, "web-tier")
            .source_fence_pending
            .is_empty()
    );
    let (_, report) = admin.get("/dr-plans/web-tier/report").await;
    assert!(report.get("source_fence_pending").is_none(), "{report}");
}

/// 지난 시험이 남긴 시험 VM이 있으면 계획을 지우지 않는다(기록을 지우면 다시 찾지 못한다).
#[tokio::test]
async fn plans_with_test_leftovers_are_not_deleted() {
    let env = dr_env().await;
    let admin = env.admin().await;
    copy(&env, "web1-a", "web1", CA, 600).await;
    create_plan(&env, "web-tier").await;
    let (status, run) = admin
        .post(
            "/dr-plans/web-tier/actions/test",
            json!({ "allow_routed_test_networks": true, "allow_missing": true }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{run}");
    finish(&env, &run["id"], DrRunState::Succeeded).await;
    let made = labelled_vm(&env, "web1-drt", DR_TEST_LABEL, "web-tier").await;
    let id: Uuid = run["id"].as_str().unwrap().parse().unwrap();
    let mut rec = env.cluster.get::<DrRunSpec>(id).unwrap().unwrap();
    rec.status.steps[1].vm_id = Some(made);
    env.cluster
        .update_status::<DrRunSpec>(id, &rec.status)
        .await
        .unwrap();
    assert_error(
        &admin.delete("/dr-plans/web-tier").await,
        StatusCode::CONFLICT,
        "dr_cleanup_required",
    );
    env.cluster.delete::<VmSpec>(made, None).await.unwrap();
    let (status, _) = admin.delete("/dr-plans/web-tier").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
