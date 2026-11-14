//! 균형 조정기(`Balancer`): 리더가 리스로 받은 노드 부하(시험이 `renew_load`처럼 리더에 직접 넣는다)로 불균형을 재고
//! `recommend`면 추천을 게시판에 두고(`GET /placement`·`/placement/recommendations`), 추천 적용 API, `auto`면
//! 마이그레이션 Task와 감사 `placement.move`, 안전 장치(동시 2개, VM 1시간, 고정·로컬 디스크·백업 중 VM, 유지보수
//! 노드·`placement`를 모르는 노드가 있으면 쉼, HA 검사가 실패한 노드), 모드 off·pack.
//! 이 노드(node1)와 저장소에만 만든 노드 a·b. a·b에는 실행기가 없어 a의 마이그레이션 Task는 끝나지 않는다.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Client, Env, WAIT, vm_request};
use opl_api::{Balancer, BalancerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_types::ha_check::{CheckReport, CheckState, HaCheckRole, HaCheckSpec, HaCheckTarget};
use opl_types::node::{FEATURES, NodeSpec, NodeStatus};
use opl_types::placement::{LoadSummary, VmLoad};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::version::BUILD;
use opl_types::vm::{Arch, PowerState, Vm, VmPhase, VmPlacement, VmSpec, VmStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 회차를 0.5초마다 도는 균형 조정기
fn balancer(env: &Env) -> Balancer {
    balancer_every(env, Duration::from_millis(500))
}

fn balancer_every(env: &Env, interval: Duration) -> Balancer {
    balancer_with(env, interval, Duration::ZERO)
}

/// `settle`: 균형 옮기기를 받는 노드가 살아 있어야 하는 시간
fn balancer_with(env: &Env, interval: Duration, settle: Duration) -> Balancer {
    Balancer::spawn(
        env.state.clone(),
        BalancerConfig {
            tick: Duration::from_millis(100),
            interval,
            settle,
        },
    )
}

/// 노드 `name`(CPU 4, 메모리 8 GiB, 이 빌드의 기능)을 저장소에 직접 만든다.
async fn node(env: &Env, name: &str) -> Uuid {
    let mut node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.status = NodeStatus {
        online: true,
        cpus: 4,
        memory_mib: 8192,
        hypervisor: "mock".into(),
        bridges: vec![],
        arch: Some(Arch::Aarch64),
        pools: Default::default(),
        ha: None,
        underlay: None,
        firewall: false,
        version: Some(BUILD.into()),
        updates: None,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        cluster_addresses: vec![],
        shutdown: None,
        fence_check: None,
        interfaces: vec![],
        checks: vec![],
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 노드의 저장소 기록을 바꾼다.
async fn edit_node(env: &Env, id: Uuid, f: impl FnOnce(&mut Resource<NodeSpec, NodeStatus>)) {
    let mut node = env.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    f(&mut node);
    env.cluster
        .update_spec(id, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    env.cluster
        .update_status::<NodeSpec>(id, &node.status)
        .await
        .unwrap();
}

/// 공유 풀 `nas`와 노드 로컬 풀 `local`
async fn pools(env: &Env) {
    for (name, shared) in [("nas", true), ("local", false)] {
        let pool = Resource::new(
            name,
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: env.dir.path().join(name),
                },
                nodes: vec![],
                shared,
                backups: false,
            },
        );
        env.cluster.create(&pool).await.unwrap();
    }
}

/// 노드 `node`에서 도는 VM(vCPU 1, 메모리 1 GiB, 디스크 `disk-<name>`은 풀 `pool`)을 저장소에 직접 만든다.
async fn vm_on(env: &Env, name: &str, node: Uuid, pool: &str) -> Vm {
    let mut volume: Volume = Resource::new(
        format!("disk-{name}"),
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(node);
    env.cluster.create(&volume).await.unwrap();
    let mut spec: VmSpec = serde_json::from_value(vm_request(name)["spec"].clone()).unwrap();
    spec.memory_mib = 1024;
    spec.nics.clear();
    spec.disks =
        serde_json::from_value(json!([{ "volume": format!("disk-{name}"), "bus": "virtio-blk" }]))
            .unwrap();
    spec.node = Some(node);
    spec.power = PowerState::Running;
    let mut vm: Vm = Resource::new(name, spec);
    vm.status = VmStatus {
        phase: VmPhase::Running,
        node: Some(node),
        pid: Some(4242),
        firmware_node: Some(node),
        ..Default::default()
    };
    vm.revision = env.cluster.create(&vm).await.unwrap();
    vm
}

/// 노드 CPU `permille`‰, 사용 메모리 1 GiB이고 VM마다 vCPU를 다 쓰는(RSS 512 MiB) 부하 요약
fn load(permille: u16, vms: &[&Vm]) -> LoadSummary {
    LoadSummary {
        cpu1: Some(permille),
        cpu5: Some(permille),
        mem: Some(1024),
        vms: vms
            .iter()
            .map(|v| VmLoad {
                id: v.id,
                cpu: Some(1000),
                mem: Some(512),
            })
            .collect(),
    }
}

/// 노드마다 리스 갱신에 실을 부하 요약과 HA 검사 결과
type Loads = Arc<Mutex<BTreeMap<Uuid, (LoadSummary, Vec<CheckReport>)>>>;

/// 노드 측정기 대신 리더에 리스 갱신으로 부하 요약(과 HA 검사 결과)을 계속 넣는다.
struct Loader {
    loads: Loads,
    task: tokio::task::JoinHandle<()>,
}

impl Loader {
    fn start(env: &Env) -> Self {
        let loads: Loads = Arc::default();
        let (cluster, shared) = (env.cluster.clone(), loads.clone());
        let task = tokio::spawn(async move {
            loop {
                let now = shared.lock().unwrap().clone();
                for (node, (load, checks)) in now {
                    let _ = cluster
                        .execute_as_leader(LeaderRequest::RenewLease {
                            node: NodeId(node),
                            checks,
                            load: Some(load),
                        })
                        .await;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        Self { loads, task }
    }

    fn set(&self, node: Uuid, load: LoadSummary) {
        self.set_with(node, load, vec![]);
    }

    fn set_with(&self, node: Uuid, load: LoadSummary, checks: Vec<CheckReport>) {
        self.loads.lock().unwrap().insert(node, (load, checks));
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 바쁜 노드 a와 한가한 b, 이 노드(node1). a에서 `names`의 VM(공유 풀)이 돌고 CPU를 90% 쓴다.
struct Cluster3 {
    env: Env,
    admin: Client,
    a: Uuid,
    b: Uuid,
    vms: Vec<Vm>,
    loader: Loader,
}

async fn cluster3(names: &[&str]) -> Cluster3 {
    let env = Env::start().await;
    let admin = env.admin().await;
    let a = node(&env, "a").await;
    let b = node(&env, "b").await;
    pools(&env).await;
    let mut vms = vec![];
    for name in names {
        vms.push(vm_on(&env, name, a, "nas").await);
    }
    let loader = Loader::start(&env);
    loader.set(a, load(900, &vms.iter().collect::<Vec<_>>()));
    loader.set(b, load(100, &[]));
    loader.set(env.node, load(100, &[]));
    Cluster3 {
        env,
        admin,
        a,
        b,
        vms,
        loader,
    }
}

async fn set_placement(admin: &Client, placement: Value) {
    let (status, body) = admin
        .patch("/settings", json!({ "placement": placement }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn status(admin: &Client) -> Value {
    let (status, body) = admin.get("/placement").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// 조건이 맞는 상태를 기다려 돌려준다.
async fn status_until(admin: &Client, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
    status_within(admin, what, WAIT, ok).await
}

/// `status_until`과 같되 `wait`까지 기다린다.
async fn status_within(
    admin: &Client,
    what: &str,
    wait: Duration,
    ok: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let got = status(admin).await;
        if ok(&got) {
            return got;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out: {what}: {got}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn recommendations(status: &Value) -> Vec<Value> {
    status["recommendations"].as_array().unwrap().clone()
}

/// 마이그레이션 Task(만든 순)
fn migrations(env: &Env) -> Vec<Task> {
    let (_, mut tasks) = env.cluster.list::<TaskSpec>().unwrap();
    tasks.retain(|t| matches!(t.spec.op, TaskOp::VmMigrate { .. }));
    tasks.sort_by_key(|t| t.id);
    tasks
}

async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !ok() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 회차가 여러 번 돌 시간(0.5초 간격)
async fn a_few_runs() {
    tokio::time::sleep(Duration::from_secs(2)).await;
}

/// `recommend`: 바쁜 a의 VM 하나를 한가한 노드로 옮기라는 추천 하나. Task는 만들지 않는다.
#[tokio::test]
async fn recommends_moving_a_vm_off_the_busy_node() {
    let c = cluster3(&["web"]).await;
    let bal = balancer(&c.env);
    let got = status_until(&c.admin, "a recommendation", |s| {
        !recommendations(s).is_empty()
    })
    .await;
    let recs = recommendations(&got);
    assert_eq!(recs.len(), 1, "{got}");
    let rec = &recs[0];
    assert_eq!(rec["vm"], c.vms[0].id.to_string());
    assert_eq!(rec["vm_name"], "web");
    assert_eq!(rec["from"], c.a.to_string());
    assert_eq!(rec["from_name"], "a");
    assert!(
        [c.b.to_string(), c.env.node.to_string()]
            .contains(&rec["to"].as_str().unwrap().to_string()),
        "{rec}"
    );
    assert_eq!(rec["reason"], "balance");
    assert!(rec["gain_pct"].as_u64().unwrap() > 0, "{rec}");
    assert_eq!(rec["cost_mib"], 1024);
    assert!(rec.get("task").is_none(), "{rec}");
    // 불균형(CPU 0.9·0.1·0.1의 표준편차 37.7%, 메모리는 같음)과 노드 표, 마지막·다음 실행
    let imbalance = &got["imbalance"];
    assert_eq!(imbalance["cpu_pct"], 377, "{got}");
    assert_eq!(imbalance["mem_pct"], 0, "{got}");
    assert_eq!(imbalance["total_pct"], 189, "{got}");
    assert!(got.get("paused").is_none(), "{got}");
    assert!(got["last_run"].as_u64().is_some(), "{got}");
    assert!(
        got["next_run"].as_u64() >= got["last_run"].as_u64(),
        "{got}"
    );
    let nodes = got["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 3, "{got}");
    assert!(nodes.iter().all(|n| n["age_secs"].is_u64()), "{got}");
    // 추천 목록 API는 같은 것을 보인다(로그인한 누구나).
    let viewer = c.env.user("v", "viewer").await;
    let (code, list) = viewer.get("/placement/recommendations").await;
    assert_eq!(code, StatusCode::OK, "{list}");
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|r| r["vm"] == rec["vm"]),
        "{list}"
    );
    a_few_runs().await;
    assert!(migrations(&c.env).is_empty());
    bal.shutdown().await;
}

/// 추천 적용: 202 Task(원본 a의 실행기). 같은 추천을 다시 적용하면 VM이 옮기는 중이라 409 `recommendation_stale`, 없는
/// 추천은 404, 그사이 고정한 VM의 추천은 409 `vm_pinned`, 뷰어는 403.
#[tokio::test]
async fn applying_a_recommendation_starts_the_migration() {
    let c = cluster3(&["web-1", "web-2"]).await;
    // 문턱 1%면 두 VM 모두 옮기라고 한다. 회차는 5초마다라 그사이 추천이 바뀌지 않는다.
    set_placement(&c.admin, json!({ "threshold": 1 })).await;
    let bal = balancer_every(&c.env, Duration::from_secs(5));
    let got = status_until(&c.admin, "two recommendations", |s| {
        recommendations(s).len() == 2
    })
    .await;
    let recs = recommendations(&got);
    let (first, second) = (&recs[0], &recs[1]);
    let path = |r: &Value| {
        format!(
            "/placement/recommendations/{}/apply",
            r["id"].as_str().unwrap()
        )
    };

    let viewer = c.env.user("v", "viewer").await;
    let (code, _) = viewer.post(&path(first), Value::Null).await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    let operator = c.env.user("o", "operator").await;
    let (code, task) = operator.post(&path(first), Value::Null).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "vm-migrate", "node": first["to"] })
    );
    assert_eq!(task["spec"]["target"], first["vm"]);
    assert_eq!(task["spec"]["node"], c.a.to_string());
    // 리더의 게시판에도 Task가 적힌다.
    let after = recommendations(&status(&c.admin).await);
    assert_eq!(after[0]["task"], task["id"], "{after:?}");

    let (code, body) = operator.post(&path(first), Value::Null).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "recommendation_stale");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("busy with a task"),
        "{body}"
    );

    let (code, body) = operator
        .post(
            &format!("/placement/recommendations/{}/apply", Uuid::now_v7()),
            Value::Null,
        )
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "recommendation_not_found");
    let (code, body) = operator
        .post("/placement/recommendations/nope/apply", Value::Null)
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND, "{body}");

    let id: Uuid = second["vm"].as_str().unwrap().parse().unwrap();
    let mut vm = c.env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    vm.spec.placement = Some(VmPlacement { pinned: true });
    c.env
        .cluster
        .update_spec(id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    let (code, body) = operator.post(&path(second), Value::Null).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_pinned");
    assert_eq!(migrations(&c.env).len(), 1);
    bal.shutdown().await;
}

/// `auto`: 한 회차에 마이그레이션을 둘까지 만들고 감사에 `placement.move`를 남긴다. 둘이 진행 중이면 쉰다.
#[tokio::test]
async fn auto_starts_at_most_two_migrations_and_audits_them() {
    let c = cluster3(&["v1", "v2", "v3", "v4"]).await;
    set_placement(&c.admin, json!({ "mode": "auto", "threshold": 1 })).await;
    let bal = balancer(&c.env);
    eventually("two migrations", || migrations(&c.env).len() == 2).await;
    let got = status_until(&c.admin, "paused for the migrations", |s| {
        s["paused"] == "2 migrations are in progress"
    })
    .await;
    assert!(recommendations(&got).is_empty(), "{got}");
    assert_eq!(migrations(&c.env).len(), 2);
    for t in migrations(&c.env) {
        assert_eq!(t.spec.node, c.a);
        assert!(c.vms.iter().any(|v| v.id == t.spec.target));
    }
    let (code, audit) = c.admin.get("/audit?action=placement.move").await;
    assert_eq!(code, StatusCode::OK, "{audit}");
    let entries = audit["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{audit}");
    for e in entries {
        assert!(e["user"].is_null(), "{e}");
        assert_eq!(e["target"]["kind"], "vms");
        let detail = e["detail"].as_str().unwrap();
        assert!(detail.starts_with("auto: v"), "{detail}");
        assert!(detail.contains(" a -> "), "{detail}");
        assert!(detail.contains("imbalance "), "{detail}");
        assert!(detail.ends_with("cost 1024 MiB"), "{detail}");
    }
    bal.shutdown().await;
}

/// `auto`로 옮긴 VM은 마이그레이션이 끝난 뒤에도 1시간 안에 다시 옮기지 않는다.
#[tokio::test]
async fn a_moved_vm_is_not_moved_again_within_the_hour() {
    let c = cluster3(&["web"]).await;
    set_placement(&c.admin, json!({ "mode": "auto", "threshold": 1 })).await;
    let bal = balancer(&c.env);
    eventually("a migration", || migrations(&c.env).len() == 1).await;
    // 마이그레이션이 끝난 것처럼 VM을 대상 노드로 옮기고 Task를 끝낸다.
    let task = migrations(&c.env).remove(0);
    let TaskOp::VmMigrate { node: to, .. } = task.spec.op else {
        unreachable!()
    };
    let id = c.vms[0].id;
    let mut vm = c.env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    vm.spec.node = Some(to);
    c.env
        .cluster
        .update_spec(id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    vm.status.node = Some(to);
    c.env
        .cluster
        .update_status::<VmSpec>(id, &vm.status)
        .await
        .unwrap();
    let mut done = task.status.clone();
    done.state = TaskState::Succeeded;
    c.env
        .cluster
        .update_status::<TaskSpec>(task.id, &done)
        .await
        .unwrap();
    // 받은 노드가 바빠져 다시 옮기면 나아지더라도 옮기지 않는다.
    c.loader.set(c.a, load(100, &[]));
    c.loader.set(to, load(900, &[&c.vms[0]]));
    a_few_runs().await;
    assert_eq!(migrations(&c.env).len(), 1);
    let got = status(&c.admin).await;
    assert!(
        got["imbalance"]["total_pct"].as_u64().unwrap() > 10,
        "{got}"
    );
    bal.shutdown().await;
}

/// 고정한 VM, 로컬 풀 디스크 VM, 백업 중인 VM은 옮기지 않는다.
#[tokio::test]
async fn pinned_local_and_busy_vms_stay() {
    let c = cluster3(&[]).await;
    let mut pinned = vm_on(&c.env, "pinned", c.a, "nas").await;
    pinned.spec.placement = Some(VmPlacement { pinned: true });
    c.env
        .cluster
        .update_spec(pinned.id, None, pinned.labels.clone(), &pinned.spec)
        .await
        .unwrap();
    let local = vm_on(&c.env, "local", c.a, "local").await;
    let backing = vm_on(&c.env, "backing", c.a, "nas").await;
    let mut backup: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmBackup {
                backup: Uuid::now_v7(),
            },
            target: backing.id,
            node: c.a,
        },
    );
    backup.name = backup.id.to_string();
    c.env.cluster.create(&backup).await.unwrap();
    c.loader.set(c.a, load(900, &[&pinned, &local, &backing]));
    set_placement(&c.admin, json!({ "mode": "auto", "threshold": 1 })).await;
    let bal = balancer(&c.env);
    let got = status_until(&c.admin, "a run", |s| s["imbalance"].is_object()).await;
    a_few_runs().await;
    assert!(migrations(&c.env).is_empty());
    assert!(recommendations(&got).is_empty(), "{got}");
    assert!(got.get("paused").is_none(), "{got}");
    bal.shutdown().await;
}

/// 유지보수 노드가 있으면 쉬고(옮기지 않음), 유지보수가 끝나면 다시 옮긴다.
#[tokio::test]
async fn pauses_while_a_node_is_in_maintenance() {
    let c = cluster3(&["web"]).await;
    edit_node(&c.env, c.b, |n| n.spec.maintenance = true).await;
    set_placement(&c.admin, json!({ "mode": "auto", "threshold": 1 })).await;
    let bal = balancer(&c.env);
    status_until(&c.admin, "paused for maintenance", |s| {
        s["paused"] == "node \"b\" is in maintenance"
    })
    .await;
    a_few_runs().await;
    assert!(migrations(&c.env).is_empty());
    edit_node(&c.env, c.b, |n| n.spec.maintenance = false).await;
    eventually("a migration", || migrations(&c.env).len() == 1).await;
    bal.shutdown().await;
}

/// 이 VM의 풀을 보는 `storage` 검사가 실패한 노드로는 옮기지 않는다(덜 한가한 다른 노드로).
#[tokio::test]
async fn does_not_move_to_a_node_failing_a_check_on_the_vm() {
    let c = cluster3(&["web"]).await;
    let check = Resource::new(
        "nas",
        HaCheckSpec {
            role: HaCheckRole::Storage,
            target: HaCheckTarget::Pool { name: "nas".into() },
            on_fail: None,
            grace_secs: 10,
            nodes: Default::default(),
            enabled: true,
        },
    );
    c.env.cluster.create(&check).await.unwrap();
    // b가 가장 한가하지만 검사가 실패한다.
    c.loader.set_with(
        c.b,
        load(50, &[]),
        vec![CheckReport::new("nas", CheckState::Failing, None, None)],
    );
    c.loader.set(c.env.node, load(300, &[]));
    set_placement(&c.admin, json!({ "mode": "auto", "threshold": 1 })).await;
    let bal = balancer(&c.env);
    eventually("a migration", || migrations(&c.env).len() == 1).await;
    assert_eq!(
        migrations(&c.env)[0].spec.op,
        TaskOp::VmMigrate {
            node: c.env.node,
            pool: None
        }
    );
    bal.shutdown().await;
}

/// 모드 off면 계산하지 않고, 정책이 pack이면 균형 옮기기를 하지 않는다(쉬는 이유로 보인다).
#[tokio::test]
async fn mode_off_and_the_pack_policy_move_nothing() {
    let c = cluster3(&["web"]).await;
    set_placement(&c.admin, json!({ "mode": "off" })).await;
    let bal = balancer(&c.env);
    let got = status_until(&c.admin, "paused", |s| s["paused"] == "mode is off").await;
    assert!(got.get("imbalance").is_none(), "{got}");
    assert!(recommendations(&got).is_empty(), "{got}");
    set_placement(&c.admin, json!({ "mode": "auto", "policy": "pack" })).await;
    let got = status_until(&c.admin, "pack", |s| s["paused"] == "policy is pack").await;
    assert!(got["imbalance"].is_object(), "{got}");
    assert!(recommendations(&got).is_empty(), "{got}");
    a_few_runs().await;
    assert!(migrations(&c.env).is_empty());
    bal.shutdown().await;
}

/// `placement`를 모르는 노드가 있으면 쉰다.
#[tokio::test]
async fn pauses_while_a_node_does_not_support_placement() {
    let c = cluster3(&["web"]).await;
    edit_node(&c.env, c.b, |n| {
        n.status.features.retain(|f| f != "placement")
    })
    .await;
    let bal = balancer(&c.env);
    let got = status_until(&c.admin, "paused", |s| s["paused"].is_string()).await;
    assert_eq!(
        got["paused"], "some nodes do not support placement yet (b)",
        "{got}"
    );
    assert!(recommendations(&got).is_empty(), "{got}");
    bal.shutdown().await;
}

/// 끝나지 않은 마이그레이션 Task(원본 a)를 저장소에 직접 만든다.
async fn migration_task(env: &Env, vm: Uuid, from: Uuid, to: Uuid) -> Uuid {
    let mut t: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmMigrate {
                node: to,
                pool: None,
            },
            target: vm,
            node: from,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    t.id
}

/// 추천은 회차 때의 상태로 계산했으므로 적용할 때 다시 본다: 마이그레이션이 둘 진행 중이면 409 `placement_busy`, 대상
/// 노드에서 이 VM의 HA 검사가 실패하면 409 `recommendation_stale`.
#[tokio::test]
async fn applying_checks_the_recommendation_again() {
    let c = cluster3(&["web", "x1", "x2"]).await;
    set_placement(&c.admin, json!({ "threshold": 1 })).await;
    // 회차는 5초마다라 그사이 추천이 바뀌지 않는다.
    let bal = balancer_every(&c.env, Duration::from_secs(5));
    let got = status_until(&c.admin, "a recommendation for web", |s| {
        recommendations(s).iter().any(|r| r["vm_name"] == "web")
    })
    .await;
    let rec = recommendations(&got)
        .into_iter()
        .find(|r| r["vm_name"] == "web")
        .unwrap();
    let path = format!(
        "/placement/recommendations/{}/apply",
        rec["id"].as_str().unwrap()
    );
    let to: Uuid = rec["to"].as_str().unwrap().parse().unwrap();

    let busy = [
        migration_task(&c.env, c.vms[1].id, c.a, to).await,
        migration_task(&c.env, c.vms[2].id, c.a, to).await,
    ];
    let (code, body) = c.admin.post(&path, Value::Null).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "placement_busy", "{body}");
    for id in busy {
        c.env.cluster.delete::<TaskSpec>(id, None).await.unwrap();
    }

    // 대상 노드에서 이 VM의 풀을 보는 storage 검사가 실패한다(리더가 리스 갱신으로 받은 뒤).
    let check = Resource::new(
        "nas",
        HaCheckSpec {
            role: HaCheckRole::Storage,
            target: HaCheckTarget::Pool { name: "nas".into() },
            on_fail: None,
            grace_secs: 10,
            nodes: Default::default(),
            enabled: true,
        },
    );
    c.env.cluster.create(&check).await.unwrap();
    let failing = vec![CheckReport::new("nas", CheckState::Failing, None, None)];
    let load = c.loader.loads.lock().unwrap()[&to].0.clone();
    c.loader.set_with(to, load, failing);
    eventually("the failing check reached the leader", || {
        c.env
            .cluster
            .leases()
            .checks
            .get(&NodeId(to))
            .is_some_and(|r| !r.is_empty())
    })
    .await;
    let (code, body) = c.admin.post(&path, Value::Null).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "recommendation_stale", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("a failing HA check affects the VM"),
        "{body}"
    );
    assert!(migrations(&c.env).is_empty());
    bal.shutdown().await;
}

/// 최적 배치 설정(모드·문턱 등)을 바꾸면 리더가 게시판의 추천을 곧 비운다(다음 회차를 기다리지 않는다). 다른 설정을
/// 바꾸면 그대로다.
#[tokio::test]
async fn changing_the_settings_clears_the_recommendations() {
    let c = cluster3(&["web"]).await;
    let bal = balancer_every(&c.env, Duration::from_secs(10));
    // 첫 실행은 10초 뒤다.
    status_within(&c.admin, "a recommendation", WAIT * 2, |s| {
        !recommendations(s).is_empty()
    })
    .await;
    let (code, body) = c
        .admin
        .patch("/settings", json!({ "ha_start_concurrency": 3 }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let got = status(&c.admin).await;
    assert_eq!(recommendations(&got).len(), 1, "{got}");
    set_placement(&c.admin, json!({ "threshold": 50 })).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let got = status(&c.admin).await;
    assert!(recommendations(&got).is_empty(), "{got}");
    bal.shutdown().await;
}

/// 균형 옮기기는 불균형이 문턱(15%)을 넘으면 시작하고, 그 뒤 불균형이 문턱 아래(약 12.5%)로 내려가도 문턱의 0.8배(12%)
/// 아래가 될 때까지 추천한다.
#[tokio::test]
async fn balancing_goes_on_until_four_fifths_of_the_threshold() {
    let c = cluster3(&["web"]).await;
    let bal = balancer(&c.env);
    status_until(&c.admin, "a recommendation", |s| {
        !recommendations(s).is_empty()
    })
    .await;
    // CPU 62%·42%·2%: 불균형 약 12.5%(평균보다 문턱의 두 배 넘게 높은 노드는 없다)
    let lower = |c: &Cluster3| {
        c.loader.set(c.a, load(620, &[&c.vms[0]]));
        c.loader.set(c.b, load(420, &[]));
        c.loader.set(c.env.node, load(20, &[]));
    };
    lower(&c);
    let got = status_until(&c.admin, "a run with the lower load", |s| {
        s["imbalance"]["total_pct"]
            .as_u64()
            .is_some_and(|t| (120..150).contains(&t))
    })
    .await;
    assert_eq!(recommendations(&got).len(), 1, "{got}");
    // 처음부터 12.5%면 시작하지 않는다.
    bal.shutdown().await;
    let d = cluster3(&["web"]).await;
    lower(&d);
    let bal = balancer(&d.env);
    let got = status_until(&d.admin, "a run", |s| s["imbalance"].is_object()).await;
    a_few_runs().await;
    assert!(recommendations(&got).is_empty(), "{got}");
    assert!(recommendations(&status(&d.admin).await).is_empty());
    bal.shutdown().await;
}

/// 리더가 살아 있음을 본 지 `settle`이 지나지 않은 노드는 균형 옮기기를 받지 않는다(돌아온 노드는 HA가 먼저 되돌린다).
#[tokio::test]
async fn nodes_seen_alive_only_briefly_take_no_balance_moves() {
    let c = cluster3(&["web"]).await;
    let bal = balancer_with(
        &c.env,
        Duration::from_millis(500),
        Duration::from_secs(3600),
    );
    let got = status_until(&c.admin, "a run", |s| s["imbalance"].is_object()).await;
    a_few_runs().await;
    assert!(recommendations(&got).is_empty(), "{got}");
    assert!(recommendations(&status(&c.admin).await).is_empty());
    bal.shutdown().await;
}

/// 추천 하나를 기다려 그 추천과 적용 경로를 돌려준다.
async fn one_recommendation(c: &Cluster3) -> (Value, String) {
    let got = status_until(&c.admin, "a recommendation", |s| {
        !recommendations(s).is_empty()
    })
    .await;
    let rec = recommendations(&got).remove(0);
    let path = format!(
        "/placement/recommendations/{}/apply",
        rec["id"].as_str().unwrap()
    );
    (rec, path)
}

/// 리더는 적용할 때 균형 조정기가 쉬는 이유도 본다(쉬면 409 `recommendation_stale`). 균형 조정기를 멈춰 추천이 그대로
/// 남게 한다.
#[tokio::test]
async fn applying_on_the_leader_checks_why_the_balancer_would_pause() {
    let c = cluster3(&["web"]).await;
    let bal = balancer(&c.env);
    let (_, path) = one_recommendation(&c).await;
    bal.shutdown().await;
    edit_node(&c.env, c.b, |n| n.spec.maintenance = true).await;
    let (code, body) = c.admin.post(&path, Value::Null).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "recommendation_stale", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("balancer paused: node \"b\" is in maintenance"),
        "{body}"
    );
    assert!(migrations(&c.env).is_empty());
}

/// 리더는 적용할 때 대상 노드가 살아 있음을 본 시간(`settle`)도 본다: 새 균형 조정기(새 리더처럼 살아 있음을 막 보기
/// 시작함)의 리더에서는 균형 옮기기 추천을 적용하지 않는다.
#[tokio::test]
async fn applying_on_the_leader_waits_until_the_target_settles() {
    let c = cluster3(&["web"]).await;
    let bal = balancer(&c.env);
    let (_, path) = one_recommendation(&c).await;
    let first = status(&c.admin).await["next_run"].as_u64().unwrap();
    bal.shutdown().await;
    // 새 균형 조정기는 첫 회차를 60초 뒤에 돌고 그사이 노드가 살아 있음을 본 때만 적는다.
    let bal = balancer_with(&c.env, Duration::from_secs(60), Duration::from_secs(3600));
    status_until(&c.admin, "the new balancer ticked", |s| {
        s["next_run"].as_u64().is_some_and(|n| n > first + 30)
    })
    .await;
    let (code, body) = c.admin.post(&path, Value::Null).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "recommendation_stale", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("the target node has not been up long enough"),
        "{body}"
    );
    assert!(migrations(&c.env).is_empty());
    bal.shutdown().await;
}

/// 같은 노드의 적용은 하나씩 다시 본다: 마이그레이션 하나가 진행 중일 때 두 추천을 함께 적용하면 하나만 시작하고 다른
/// 하나는 409 `placement_busy`다.
#[tokio::test]
async fn concurrent_applies_are_checked_one_at_a_time() {
    let c = cluster3(&["web-1", "web-2"]).await;
    // 문턱 1%면 두 VM 모두 옮기라고 한다.
    set_placement(&c.admin, json!({ "threshold": 1 })).await;
    let bal = balancer(&c.env);
    let got = status_until(&c.admin, "two recommendations", |s| {
        recommendations(s).len() == 2
    })
    .await;
    bal.shutdown().await;
    let paths: Vec<String> = recommendations(&got)
        .iter()
        .map(|r| {
            format!(
                "/placement/recommendations/{}/apply",
                r["id"].as_str().unwrap()
            )
        })
        .collect();
    // 다른 VM의 마이그레이션 하나가 진행 중이다.
    migration_task(&c.env, Uuid::now_v7(), c.a, c.b).await;
    let ((one, first), (two, second)) = tokio::join!(
        c.admin.post(&paths[0], Value::Null),
        c.admin.post(&paths[1], Value::Null)
    );
    let mut codes = [one, two];
    codes.sort();
    assert_eq!(
        codes,
        [StatusCode::ACCEPTED, StatusCode::CONFLICT],
        "{first} {second}"
    );
    let refused = if one == StatusCode::CONFLICT {
        first
    } else {
        second
    };
    assert_eq!(refused["code"], "placement_busy", "{refused}");
    assert_eq!(migrations(&c.env).len(), 2);
}
