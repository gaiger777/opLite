//! 계획 71: VM 만들기 미리 확인 `POST /vm-checks` — 만들기와 같은 검사를 만들 볼륨·배치 규칙을 얹어 쓰지 않고 돌린다.
//! 통과(고를 노드·채운 머신·CPU 모델), 아무것도 만들지 않음, 문제를 자리마다 모음(권한·없는 ISO·할당량), 만들 볼륨(없는
//! 풀·권한 없는 풀·같은 이름·풀 용량 경고), 만들 규칙(영역 규칙 → 그 영역 노드, 맞는 노드 없음 → `placement`, 권한),
//! 이름·머신 문제, 폴더 `vm.create` 없음 → 403, 노드 이름을 볼 수 없으면 `node: null`, 기능 `vm-check` → 409, 감사 없음.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Client, Env, FakeStorage, WAIT, set_pool_state};
use orbivisor_types::audit::AuditQuery;
use orbivisor_types::node::{NodeSpec, PoolState, VM_CHECK};
use orbivisor_types::placement::PlacementRuleSpec;
use orbivisor_types::resource::{Resource, ResourceSpec};
use orbivisor_types::storage::VolumeSpec;
use orbivisor_types::task::TaskSpec;
use orbivisor_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};

const GIB: u64 = 1 << 30;

/// 디스크 없는 VM `name`(켜진 채, 노드 없이)의 확인 본문
fn check_body(name: &str) -> Value {
    json!({
        "name": name,
        "spec": {
            "cpu": { "cores": 1 },
            "memory_mib": 256,
            "machine": "virt",
            "power": "running",
            "nics": [{ "network": "user", "model": "virtio" }],
        },
    })
}

/// 새 디스크 하나(`<name>-disk0`, 풀 `local`, 1 GiB)를 만들어 붙이는 확인 본문
fn with_new_disk(name: &str, pool: &str) -> Value {
    let mut body = check_body(name);
    let volume = format!("{name}-disk0");
    body["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
    body["volumes"] = json!([{ "name": volume,
        "spec": { "pool": pool, "size_bytes": GIB, "format": "qcow2" } }]);
    body
}

/// 파일 없는 스토리지로 실행기를 띄운 시험 환경 — 볼륨 만들기가 `qemu-img`에 기대지 않게(CI `check`에는 없다).
async fn start() -> Env {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env.start_runner_with(Arc::new(FakeStorage::default()))
        .await;
    env
}

async fn local_pool(env: &Env, admin: &Client, name: &str, folder: Option<&str>) {
    let dir = env.dir.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": name, "spec": { "backend": { "kind": "dir", "path": dir }, "folder": folder } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready(name).await;
}

async fn folder(admin: &Client, name: &str, kind: &str, quota: Value) -> String {
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": name, "kind": kind, "quota": quota }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

/// 전역 역할 `none`인 사용자 `name`에게 폴더마다 operator를 주고 로그인한다.
async fn folder_user(env: &Env, admin: &Client, name: &str, folders: &[&str]) -> Client {
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": name, "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let user = body["id"].as_str().unwrap().to_string();
    for f in folders {
        let (status, body) = admin
            .post(
                "/grants",
                json!({
                    "principal": { "type": "user", "id": user },
                    "role": { "type": "builtin", "role": "operator" },
                    "target": { "type": "folder", "id": f },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    env.login(name, "password-1").await
}

/// 확인을 보내 200과 답을 받는다.
async fn check(client: &Client, body: Value) -> Value {
    let (status, answer) = client.post("/vm-checks", body).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    answer
}

/// 답의 문제를 (자리, 코드, 무게)로
fn problems(answer: &Value) -> Vec<(String, String, String)> {
    answer["problems"]
        .as_array()
        .unwrap_or_else(|| panic!("{answer}"))
        .iter()
        .map(|p| {
            (
                p["at"].as_str().unwrap().to_string(),
                p["code"].as_str().unwrap().to_string(),
                p["level"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn problem(at: &str, code: &str, level: &str) -> (String, String, String) {
    (at.into(), code.into(), level.into())
}

fn count<S: ResourceSpec>(env: &Env) -> usize {
    env.cluster.list::<S>().unwrap().1.len()
}

/// VM·볼륨·배치 규칙·Task 수
fn counts(env: &Env) -> [usize; 4] {
    [
        count::<VmSpec>(env),
        count::<VolumeSpec>(env),
        count::<PlacementRuleSpec>(env),
        count::<TaskSpec>(env),
    ]
}

#[tokio::test]
async fn a_passing_check_names_the_node_and_creates_nothing() {
    let env = start().await;
    let admin = env.admin().await;
    local_pool(&env, &admin, "local", None).await;
    let before = counts(&env);
    let mut body = with_new_disk("web", "local");
    body["spec"].as_object_mut().unwrap().remove("machine");
    let answer = check(&admin, body.clone()).await;
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["problems"], json!([]));
    assert_eq!(answer["node"], "node1");
    assert_eq!(answer["machine"], "virt");
    assert!(answer["cpu_model"].is_string(), "{answer}");
    assert_eq!(answer["ips"], json!([[]]));
    // 미리 확인은 아무것도 만들지 않는다(볼륨·VM·Task 그대로) — 그리고 같은 요청으로 정말 만들 수 있다.
    assert_eq!(counts(&env), before);
    let (status, task) = admin.post("/volumes", body["volumes"][0].clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
    let (status, created) = admin
        .post("/vms", json!({ "name": "web", "spec": body["spec"] }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["spec"]["node"], env.node.to_string(), "{created}");

    // 꺼진 채 노드 없이 만들면 고를 노드가 없다.
    let mut off = check_body("off");
    off["spec"]["power"] = json!("stopped");
    let answer = check(&admin, off).await;
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["node"], Value::Null);
}

#[tokio::test]
async fn independent_problems_are_collected_with_their_slots() {
    let env = start().await;
    let admin = env.admin().await;
    let team = folder(&admin, "team", "vm", json!({ "memory_mib": 128 })).await;
    let net = folder(&admin, "net", "network", json!(null)).await;
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "lan-net", "spec": { "bridge": "vmbr0", "folder": net } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let alice = folder_user(&env, &admin, "alice", &[&team]).await;
    let mut body = check_body("Bad_Name!");
    body["spec"]["folder"] = json!(team);
    body["spec"]["nics"] = json!([
        { "network": "user", "model": "virtio" },
        { "network": "lan-net", "model": "virtio" }]);
    body["spec"]["cdroms"] = json!([{ "iso": "nope.iso" }]);
    let answer = check(&alice, body.clone()).await;
    assert_eq!(answer["ok"], false, "{answer}");
    let found = problems(&answer);
    // 이름 문제와 권한 없는 네트워크(읽을 수도 없으면 404). 권한 거절이 있으면 만들기처럼 거기서 멈춘다(할당량·만들기
    // 검사를 돌리지 않는다 — 리뷰 1 1).
    assert_eq!(found.len(), 2, "{answer}");
    assert_eq!(found[0].0, "name", "{answer}");
    assert_eq!(found[1].0, "nics.1", "{answer}");
    assert!(
        ["forbidden", "not_found"].contains(&found[1].1.as_str()),
        "{answer}"
    );
    assert_eq!(answer["node"], Value::Null);
    assert_eq!(answer["machine"], Value::Null);
    // 권한 거절이 없으면 할당량 넘음과 없는 ISO(만들기 검사의 첫 오류)가 한 답에 함께
    body["spec"]["nics"] = json!([{ "network": "user", "model": "virtio" }]);
    let answer = check(&alice, body).await;
    let found = problems(&answer);
    assert_eq!(found.len(), 3, "{answer}");
    assert_eq!(found[0].0, "name", "{answer}");
    assert_eq!(found[1], problem("quota", "quota_exceeded", "error"));
    assert_eq!(found[2], problem("cdroms.0", "invalid_request", "error"));
}

/// 관리자가 폴더 `theirs`에 볼륨 `secret-disk`를 붙인 VM `payroll`(켜짐)과 그 폴더의 네트워크 폴더에 SDN 네트워크
/// `hidden`(10.20.0.0/29)을 만든다. 폴더 `mine`의 operator `alice`를 돌려준다(둘 다 볼 수 없다).
async fn hidden_payroll(env: &Env, admin: &Client) -> (Client, String) {
    local_pool(env, admin, "local", None).await;
    let mine = folder(admin, "mine", "vm", json!(null)).await;
    let theirs = folder(admin, "theirs", "vm", json!(null)).await;
    let nets = folder(admin, "nets", "network", json!(null)).await;
    let (status, task) = admin
        .post(
            "/volumes",
            json!({ "name": "secret-disk", "spec": { "pool": "local", "size_bytes": GIB, "format": "qcow2" } }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
    let mut payroll = check_body("payroll");
    payroll["spec"]["folder"] = json!(theirs);
    payroll["spec"]["disks"] = json!([{ "volume": "secret-disk", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", payroll).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let net: orbivisor_types::network::Network = Resource::new(
        "hidden",
        serde_json::from_value(
            json!({ "sdn": {}, "subnets": [{ "cidr": "10.20.0.0/29" }], "folder": nets }),
        )
        .unwrap(),
    );
    env.cluster.create(&net).await.unwrap();
    let alice = folder_user(env, admin, "alice", &[&mine]).await;
    (alice, mine)
}

#[tokio::test]
async fn refused_attachments_do_not_run_the_deeper_checks() {
    let env = start().await;
    let admin = env.admin().await;
    let (alice, mine) = hidden_payroll(&env, &admin).await;

    // 볼 수 없는 볼륨: 만들기는 404, 미리 확인도 그 거절 하나뿐 — 볼륨을 쓰는 VM 이름이 드러나지 않는다(리뷰 1 1).
    let mut body = check_body("web");
    body["spec"]["folder"] = json!(mine);
    body["spec"]["disks"] = json!([{ "volume": "secret-disk", "bus": "virtio-blk" }]);
    let (status, created) = alice.post("/vms", body.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{created}");
    let answer = check(&alice, body).await;
    assert_eq!(
        problems(&answer),
        [problem("disks.0", "not_found", "error")],
        "{answer}"
    );
    assert_eq!(answer["problems"][0]["message"], created["message"]);
    let text = answer.to_string();
    assert!(
        !text.contains("payroll") && !text.contains("volume_in_use"),
        "{answer}"
    );
    assert_eq!(answer["ok"], false);
    assert_eq!(answer["node"], Value::Null);
    assert_eq!(answer["ips"], json!([]));

    // 볼 수 없는 SDN 네트워크: 주소를 고르지도(빈 주소), 준 주소를 검사하지도 않는다.
    for nic in [
        json!({ "network": "hidden", "model": "virtio" }),
        json!({ "network": "hidden", "model": "virtio", "ips": ["10.20.0.2"] }),
        json!({ "network": "hidden", "model": "virtio", "ips": ["10.99.0.2"] }),
    ] {
        let mut body = check_body("web");
        body["spec"]["folder"] = json!(mine);
        body["spec"]["nics"] = json!([nic]);
        let answer = check(&alice, body).await;
        assert_eq!(
            problems(&answer),
            [problem("nics.0", "not_found", "error")],
            "{answer}"
        );
        assert_eq!(answer["ips"], json!([]), "{answer}");
        assert!(!answer.to_string().contains("10.20"), "{answer}");
    }
}

#[tokio::test]
async fn rules_the_caller_may_not_create_do_not_steer_the_check() {
    let env = start().await;
    let admin = env.admin().await;
    let (alice, mine) = hidden_payroll(&env, &admin).await;
    let probe = |folder: Option<&str>| {
        let mut body = check_body("web2");
        body["spec"]["node"] = json!(env.node.to_string());
        if let Some(folder) = folder {
            body["spec"]["folder"] = json!(folder);
        }
        body["rules"] = json!([{ "name": "probe",
            "spec": { "kind": "anti-affinity", "vms": ["payroll", "web2"] } }]);
        body
    };
    // 관리자에게는 규칙이 얹혀 payroll과 같은 노드라고 알린다 — 권한 없는 주체에게 이것이 드러나면 안 된다.
    let answer = check(&admin, probe(None)).await;
    assert!(
        problems(&answer)
            .iter()
            .any(|p| p.0 == "placement" && p.1 == "placement_rule"),
        "{answer}"
    );
    // 규칙을 만들 수 없는 alice: 거절 하나뿐(리뷰 1 2)
    let answer = check(&alice, probe(Some(&mine))).await;
    assert_eq!(
        problems(&answer),
        [problem("placement", "forbidden", "error")],
        "{answer}"
    );
    assert!(!answer.to_string().contains("payroll"), "{answer}");
    assert_eq!(answer["node"], Value::Null);
}

/// 같은 이름 사용자에게 폴더 `folder`의 내장 역할 `role`을 준다.
async fn grant(admin: &Client, user: &str, role: &str, folder: &str) {
    let (status, users) = admin.get("/users").await;
    assert_eq!(status, StatusCode::OK, "{users}");
    let id = users
        .as_array()
        .or_else(|| users["items"].as_array())
        .unwrap()
        .iter()
        .find(|u| u["name"] == user)
        .unwrap()["id"]
        .clone();
    let (status, body) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": id },
                "role": { "type": "builtin", "role": role },
                "target": { "type": "folder", "id": folder },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn creates_keep_the_first_refusal_by_volume_name() {
    let env = start().await;
    let admin = env.admin().await;
    let mine = folder(&admin, "mine", "vm", json!(null)).await;
    let seen = folder(&admin, "seen", "storage", json!(null)).await;
    let hid = folder(&admin, "hid", "storage", json!(null)).await;
    local_pool(&env, &admin, "ro", Some(&seen)).await;
    local_pool(&env, &admin, "secret", Some(&hid)).await;
    for (name, pool) in [("a", "ro"), ("b", "secret")] {
        let (status, task) = admin
            .post(
                "/volumes",
                json!({ "name": name, "spec": { "pool": pool, "size_bytes": GIB, "format": "qcow2" } }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        admin.wait_task(&task).await;
    }
    let alice = folder_user(&env, &admin, "alice", &[&mine]).await;
    grant(&admin, "alice", "viewer", &seen).await;
    // 디스크 [b, a]: a는 볼 수 있지만 할당할 수 없어 403, b는 볼 수 없어 404. 만들기·고치기는 계획 71 앞처럼 이름 차례의
    // 첫 거절(a의 403 — 디스크 차례면 b의 404)이다(리뷰 1 3). 미리 확인은 둘 다 칸마다 낸다.
    let mut body = check_body("web");
    body["spec"]["power"] = json!("stopped");
    body["spec"]["folder"] = json!(mine);
    body["spec"]["disks"] = json!([
        { "volume": "b", "bus": "virtio-blk" }, { "volume": "a", "bus": "virtio-blk" }]);
    let (status, refused) = alice.post("/vms", body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    let answer = check(&alice, body.clone()).await;
    assert_eq!(
        problems(&answer),
        [
            problem("disks.0", "not_found", "error"),
            problem("disks.1", "forbidden", "error")
        ],
        "{answer}"
    );
    let disks = std::mem::replace(&mut body["spec"]["disks"], json!([]));
    let (status, created) = alice.post("/vms", body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let mut spec = created["spec"].clone();
    spec["disks"] = disks;
    let (status, refused) = alice.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
}

#[tokio::test]
async fn planned_lists_are_capped() {
    let env = start().await;
    let admin = env.admin().await;
    let volume = |i: usize| json!({ "name": format!("v{i}"), "spec": { "pool": "local", "size_bytes": GIB, "format": "qcow2" } });
    let rule = |i: usize| json!({ "name": format!("r{i}"), "spec": { "kind": "anti-affinity", "vms": ["web", "x"] } });
    let mut body = check_body("web");
    body["volumes"] = (0..16).map(volume).collect();
    body["rules"] = (0..16).map(rule).collect();
    check(&admin, body.clone()).await;
    for (key, item) in [("volumes", volume(16)), ("rules", rule(16))] {
        let mut over = body.clone();
        over[key].as_array_mut().unwrap().push(item);
        let (status, answer) = admin.post("/vm-checks", over).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key}: {answer}");
    }
}

#[tokio::test]
async fn names_and_machines_are_checked_too() {
    let env = start().await;
    let admin = env.admin().await;
    let (status, body) = admin.post("/vms", check_body("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let answer = check(&admin, check_body("web")).await;
    assert_eq!(problems(&answer), [problem("name", "name_taken", "error")]);
    let answer = check(&admin, check_body("Bad_Name!")).await;
    assert_eq!(problems(&answer)[0].0, "name", "{answer}");
    assert_eq!(answer["ok"], false);

    // 아키텍처가 섞인 클러스터에서 머신을 빼면 고를 수 없다(계산 칸).
    let mut other: orbivisor_types::node::Node = Resource::new(
        "x86",
        serde_json::from_value(json!({ "address": "127.0.0.2:0" })).unwrap(),
    );
    other.status.arch = Some(orbivisor_types::vm::Arch::X86_64);
    other.status.features = orbivisor_types::node::FEATURES
        .iter()
        .map(|f| f.to_string())
        .collect();
    env.cluster.create(&other).await.unwrap();
    env.cluster
        .update_status::<NodeSpec>(other.id, &other.status)
        .await
        .unwrap();
    let mut body = check_body("db");
    body["spec"].as_object_mut().unwrap().remove("machine");
    let answer = check(&admin, body).await;
    assert_eq!(
        problems(&answer),
        [problem("compute", "invalid_request", "error")]
    );
    assert_eq!(answer["ok"], false);
}

#[tokio::test]
async fn planned_volumes_are_checked_like_volume_creates() {
    let env = start().await;
    let admin = env.admin().await;
    local_pool(&env, &admin, "local", None).await;
    let team = folder(&admin, "team", "vm", json!(null)).await;
    let alice = folder_user(&env, &admin, "alice", &[&team]).await;

    // 없는 풀: 그 볼륨을 가리키는 디스크 칸. 거절처럼 거기서 멈춘다(리뷰 2 — 준비 단계를 돌리지 않음).
    let answer = check(&admin, with_new_disk("web", "nope")).await;
    assert_eq!(
        problems(&answer),
        [problem("disks.0", "invalid_request", "error")]
    );
    assert_eq!(answer["node"], Value::Null, "{answer}");
    assert_eq!(answer["machine"], Value::Null, "{answer}");

    // 이 풀에 할당할 권한이 없는 폴더 사용자(루트 풀)
    let mut body = with_new_disk("web", "local");
    body["spec"]["folder"] = json!(team);
    let answer = check(&alice, body).await;
    let found = problems(&answer);
    assert_eq!(found.len(), 1, "{answer}");
    assert_eq!(found[0].0, "disks.0");
    assert!(
        ["forbidden", "not_found"].contains(&found[0].1.as_str()),
        "{answer}"
    );

    // 같은 이름 볼륨이 이미 있음
    let (status, task) = admin
        .post(
            "/volumes",
            json!({ "name": "web-disk0", "spec": { "pool": "local", "size_bytes": GIB, "format": "qcow2" } }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let answer = check(&admin, with_new_disk("web", "local")).await;
    assert!(
        problems(&answer).contains(&problem("disks.0", "name_taken", "error")),
        "{answer}"
    );

    // 풀 남은 용량이 새 디스크보다 적으면 경고(만들기는 받는다)
    set_pool_state(
        &env.cluster,
        env.node,
        "local",
        PoolState {
            ready: true,
            total_bytes: 10 * GIB,
            available_bytes: GIB / 2,
            ..Default::default()
        },
    )
    .await;
    let answer = check(&admin, with_new_disk("db", "local")).await;
    assert_eq!(
        problems(&answer),
        [problem("disks.0", "pool_space", "warn")]
    );
    assert_eq!(answer["ok"], true, "{answer}");
}

/// 리뷰 2: 만들 볼륨의 400 검사(크기·이름)가 풀 권한보다 앞서 실패해도, 할당할 수 없는(볼 수 없는) 풀의 볼륨은 얹지 않고
/// 거절만 돌려준다 — 준비 단계가 그 풀의 성질(노드 로컬인지·어느 노드인지)을 드러내지 않게. 없는 풀도 거기서 멈춘다.
#[tokio::test]
async fn planned_volumes_in_hidden_pools_are_refused_before_other_checks() {
    let env = start().await;
    let admin = env.admin().await;
    local_pool(&env, &admin, "local", None).await;
    let team = folder(&admin, "team", "vm", json!(null)).await;
    let alice = folder_user(&env, &admin, "alice", &[&team]).await;

    let node = env.node.to_string();
    for (volume, pool, size) in [
        ("web-disk0", "local", 0),
        ("Bad Name!", "local", GIB),
        ("web-disk0", "nope", 0),
    ] {
        let mut body = check_body("web");
        body["spec"]["folder"] = json!(team);
        body["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
        body["spec"]["replication"] = json!([{ "node": node }]);
        body["volumes"] = json!([{ "name": volume,
            "spec": { "pool": pool, "size_bytes": size, "format": "qcow2" } }]);
        let answer = check(&alice, body).await;
        let found = problems(&answer);
        assert_eq!(found.len(), 1, "{volume} {pool} {size}: {answer}");
        assert_eq!(found[0].0, "disks.0", "{answer}");
        let expected: &[&str] = if pool == "nope" {
            &["invalid_request"]
        } else {
            &["forbidden", "not_found"]
        };
        assert!(expected.contains(&found[0].1.as_str()), "{answer}");
        for key in ["node", "machine", "cpu_model"] {
            assert_eq!(answer[key], Value::Null, "{key}: {answer}");
        }
        assert_eq!(answer["ips"], json!([]), "{answer}");
        assert!(!answer.to_string().contains("node-local"), "{answer}");
    }
}

#[tokio::test]
async fn planned_zone_rules_steer_the_node() {
    let env = start().await;
    let admin = env.admin().await;
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.labels.insert("zone".into(), "a".into());
    env.cluster
        .update_spec(env.node, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    let zone = |value: &str| {
        let mut body = check_body("web");
        body["rules"] = json!([{ "name": "web-zone",
            "spec": { "kind": "node-affinity", "vms": ["web"], "node_tag": format!("zone={value}") } }]);
        body
    };
    let answer = check(&admin, zone("a")).await;
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["node"], "node1");
    // 영역에 노드가 없으면 만들 규칙 때문에 놓을 곳이 없다(배치 칸).
    // 규칙 만들기가 422 `rule_conflict`(맞는 노드가 없는 규칙)로 거절하고, 만들기 검사도 놓을 노드가 없다.
    let answer = check(&admin, zone("b")).await;
    assert_eq!(answer["ok"], false, "{answer}");
    assert_eq!(
        problems(&answer),
        [
            problem("placement", "rule_conflict", "error"),
            problem("placement", "placement_rule", "error")
        ]
    );
    assert_eq!(count::<PlacementRuleSpec>(&env), 0);

    // 규칙을 쓸 권한(admin)이 없으면 배치 칸
    let team = folder(&admin, "team", "vm", json!(null)).await;
    let alice = folder_user(&env, &admin, "alice", &[&team]).await;
    let mut body = zone("a");
    body["spec"]["folder"] = json!(team);
    let answer = check(&alice, body).await;
    assert_eq!(
        problems(&answer)[0],
        problem("placement", "forbidden", "error")
    );
    // 노드 이름을 볼 수 없는 폴더 사용자에게는 고를 노드를 알리지 않는다.
    let mut body = check_body("web");
    body["spec"]["folder"] = json!(team);
    let answer = check(&alice, body).await;
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["node"], Value::Null);
    assert_eq!(answer["machine"], "virt");
}

#[tokio::test]
async fn a_folder_without_vm_create_is_refused() {
    let env = start().await;
    let admin = env.admin().await;
    let mine = folder(&admin, "mine", "vm", json!(null)).await;
    let theirs = folder(&admin, "theirs", "vm", json!(null)).await;
    let alice = folder_user(&env, &admin, "alice", &[&mine]).await;
    let mut body = check_body("web");
    body["spec"]["folder"] = json!(theirs);
    let (status, answer) = alice.post("/vm-checks", body).await;
    assert!(
        [StatusCode::FORBIDDEN, StatusCode::NOT_FOUND].contains(&status),
        "{status} {answer}"
    );
    // 폴더를 주지 않으면 루트에 만들 권한이 없어 400 `folder_required`(만들기와 같다)
    let (status, answer) = alice.post("/vm-checks", check_body("web")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["code"], "folder_required");
    // 풀 수 없는 본문은 400
    let (status, answer) = admin
        .post(
            "/vm-checks",
            json!({ "name": "web", "spec": {}, "extra": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    let (status, _) = env.anonymous().post("/vm-checks", check_body("web")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn old_nodes_make_the_check_wait_for_the_upgrade() {
    let env = start().await;
    let admin = env.admin().await;
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != VM_CHECK);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (code, body) = admin.post("/vm-checks", check_body("web")).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    // 만들기는 그대로 된다.
    let (code, body) = admin.post("/vms", check_body("web")).await;
    assert_eq!(code, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn checks_are_not_audited() {
    let env = start().await;
    let admin = env.admin().await;
    check(&admin, check_body("web")).await;
    let (status, body) = admin.post("/vms", check_body("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 쓰기 스레드가 만들기를 적을 때까지 기다린 뒤 미리 확인이 없는지 본다(확인이 먼저 왔다).
    let start = Instant::now();
    let entries = loop {
        let entries = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap();
        if entries.iter().any(|e| e.action == "POST /vms") {
            break entries;
        }
        assert!(start.elapsed() < WAIT, "{entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        entries.iter().all(|e| e.action != "POST /vm-checks"),
        "{entries:#?}"
    );
}

#[tokio::test]
async fn the_check_is_in_the_api_document() {
    let env = start().await;
    let (status, doc) = env.anonymous().get("/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    let post = &doc["paths"]["/vm-checks"]["post"];
    assert_eq!(
        post["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/VmCheckRequest"
    );
    assert_eq!(
        post["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/VmCheck"
    );
    let schemas = &doc["components"]["schemas"];
    for name in [
        "VmCheck",
        "VmCheckProblem",
        "VmCheckLevel",
        "VmCheckRequest",
    ] {
        assert!(schemas[name].is_object(), "{name}");
    }
}
