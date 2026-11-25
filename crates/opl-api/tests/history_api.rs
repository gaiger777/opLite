//! 계획 73 설정 이력 API·되돌리기·드리프트(§12·§14): 타임라인(바뀐 칸·거르기·출처), 줄 비교(앞 줄·지금 값), 하나 되돌리기
//! (선언형 계획 — 미리 보기·Task·`restored_from`), 지운 것 살리기(같은 id·이름 겹침·데이터 종류 막힘), 폴더를 한 시점으로
//! (바꾼 것·지운 것·그 뒤 만든 것), 숨은 리소스의 이력, 드리프트(누가 바꿨나·경보·받아들이기·지운 것), Terraform 관리 충돌.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage};
use opl_types::alert::{NotificationTargetSpec, TargetConfig};
use opl_types::manifest::{APPLY_SET_LABEL, MANAGED_BY_LABEL};
use opl_types::security_group::SecurityGroupSpec;
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};

const MIB: u64 = 1 << 20;

/// 파일 없는 스토리지로 실행기를 띄우고(CI `check`에는 `qemu-img`가 없다) 풀 `local`을 만든다.
async fn start() -> (Env, Client) {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env.start_runner_with(Arc::new(FakeStorage::default()))
        .await;
    let admin = env.admin().await;
    let dir = env.dir.path().join("local");
    std::fs::create_dir_all(&dir).unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": dir } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
    (env, admin)
}

fn doc(kind: &str, name: &str, spec: Value) -> Value {
    json!({ "apiVersion": "orbivisor.io/v1", "kind": kind, "metadata": { "name": name }, "spec": spec })
}

fn vm(name: &str, folder: &str, memory: u32) -> Value {
    let mut d = doc(
        "Vm",
        name,
        json!({
            "cpu": { "cores": 1 },
            "memory_mib": memory,
            "machine": "virt",
            "disks": [{ "volume": format!("{name}-root"), "bus": "virtio-blk" }],
            "nics": [{ "network": "user", "model": "virtio" }],
        }),
    );
    d["metadata"]["folder"] = json!(folder);
    d["state"] = json!("stopped");
    d
}

fn volume(name: &str) -> Value {
    doc(
        "Volume",
        name,
        json!({ "pool": "local", "size_bytes": MIB }),
    )
}

async fn apply(client: &Client, body: Value) -> Value {
    let (status, started) = client.post("/apply", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let task = client.wait_task(&started["task"]).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    task
}

fn summary(plan: &Value) -> [u64; 5] {
    let s = &plan["summary"];
    ["create", "update", "delete", "unchanged", "blocked"].map(|k| s[k].as_u64().unwrap())
}

/// `orbivisor-client`·사유 머리를 단 요청
async fn send_as(
    client: &Client,
    method: reqwest::Method,
    path: &str,
    body: Value,
    agent: &str,
    reason: Option<&str>,
) -> (StatusCode, Value) {
    let mut r = client
        .http
        .request(method, format!("{}/api/v1{path}", client.base))
        .bearer_auth(client.token.as_deref().unwrap())
        .header("orbivisor-client", agent)
        .json(&body);
    if let Some(reason) = reason {
        r = r.header("orbivisor-change-reason", reason);
    }
    client.send(r).await
}

/// 리소스의 이력(새것부터)
async fn rows(client: &Client, kind: &str, key: &str) -> Vec<Value> {
    let (status, list) = client.get(&format!("/history/{kind}/{key}")).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    list["items"].as_array().unwrap().clone()
}

/// 되돌리기 계획을 보고(200) 그 계획을 적용한다.
async fn restore(client: &Client, path: &str, body: Value) -> (Value, Value) {
    let mut dry = body.clone();
    dry["dry_run"] = json!(true);
    let (status, plan) = client.post(path, dry).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let (status, started) = client.post("/apply", json!({ "plan": plan["plan"] })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let task = client.wait_task(&started["task"]).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    (plan, task)
}

fn rules(port: u16) -> Value {
    json!([{ "direction": "in", "action": "accept", "protocol": "tcp", "ports": port.to_string() }])
}

#[tokio::test]
async fn timeline_shows_changed_paths_sources_and_comparisons() {
    let (_env, admin) = start().await;
    let (status, sg) = send_as(
        &admin,
        reqwest::Method::POST,
        "/security-groups",
        json!({ "name": "web-sg", "spec": { "description": "a", "rules": rules(80) } }),
        "cli/0.1.0",
        Some("INC-7 open http"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{sg}");
    let (status, body) = send_as(
        &admin,
        reqwest::Method::PATCH,
        "/security-groups/web-sg",
        json!({ "description": "b" }),
        "web",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let items = rows(&admin, "security-group", "web-sg").await;
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["op"], "update");
    assert_eq!(items[0]["source"], "web");
    assert_eq!(items[0]["changed"], json!(["description"]));
    assert_eq!(items[1]["op"], "create");
    assert_eq!(items[1]["source"], "cli");
    assert_eq!(items[1]["reason"], "INC-7 open http");
    assert_eq!(items[1]["actor"]["user"], "admin");
    assert!(items[1]["request"].is_string(), "{:?}", items[1]);
    // 전체 타임라인의 거르기
    let (_, list) = admin
        .get("/history?kind=SecurityGroup&source=cli&limit=10")
        .await;
    let names: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["kind"].clone(), e["op"].clone()))
        .collect();
    assert_eq!(
        names,
        [(json!("security-group"), json!("create"))],
        "{list}"
    );
    let (_, list) = admin.get("/history?user=nobody").await;
    assert_eq!(list["items"], json!([]));
    // 쪽 나누기
    let (_, page) = admin.get("/history?limit=1").await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let next = page["next_before"].as_u64().expect("more pages");
    let (_, page2) = admin.get(&format!("/history?limit=1&before={next}")).await;
    assert!(page2["items"][0]["revision"].as_u64().unwrap() < next);

    // 줄 비교: 앞 줄 → 이 줄, 이 줄 → 지금 값
    let rev = items[0]["revision"].as_u64().unwrap();
    let (status, row) = admin
        .get(&format!("/history/security-group/web-sg/{rev}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(
        row["diff"],
        json!([{ "path": "description", "op": "replace", "old": "a", "new": "b" }])
    );
    let first = items[1]["revision"].as_u64().unwrap();
    let (_, row) = admin
        .get(&format!(
            "/history/security-group/web-sg/{first}?compare=live"
        ))
        .await;
    assert_eq!(row["diff"][0]["path"], "description", "{row}");
    assert_eq!(row["compare"], "live");
    let (status, _) = admin
        .get("/history/security-group/web-sg/1?compare=prev")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = admin.get("/history/task/x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn restoring_one_object_plans_and_marks_the_row() {
    let (env, admin) = start().await;
    let (status, _) = admin
        .post(
            "/security-groups",
            json!({ "name": "web-sg", "spec": { "description": "a", "rules": rules(80) } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = admin
        .patch(
            "/security-groups/web-sg",
            json!({ "description": "b", "rules": rules(443) }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let items = rows(&admin, "security-group", "web-sg").await;
    let created = items[1]["revision"].as_u64().unwrap();

    let before = env.cluster.revision();
    let (status, plan) = admin
        .post(
            "/history/security-group/web-sg/restore",
            json!({ "revision": created, "dry_run": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");
    let paths: Vec<_> = plan["steps"][0]["diff"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(paths, ["description", "rules"], "{plan}");
    assert_eq!(env.cluster.revision(), before, "a dry run writes nothing");

    let (status, started) = admin
        .post(
            "/history/security-group/web-sg/restore",
            json!({ "revision": created, "reason": "roll back INC-9" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    admin.wait_task(&started["task"]).await;
    let sg = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg")
        .unwrap()
        .unwrap();
    assert_eq!(sg.spec.description.as_deref(), Some("a"));
    assert_eq!(serde_json::to_value(&sg.spec.rules).unwrap(), rules(80));
    let items = rows(&admin, "security-group", "web-sg").await;
    assert_eq!(items[0]["restored_from"], created, "{:?}", items[0]);
    assert_eq!(items[0]["reason"], "roll back INC-9");
    // 요청한 곳의 출처(토큰·머리 없음 = api)를 그대로 둔다 — `apply`가 아니다.
    assert_eq!(items[0]["source"], "api");
    // 같은 값으로 다시 되돌리면 할 일이 없다.
    let (_, plan) = admin
        .post(
            "/history/security-group/web-sg/restore",
            json!({ "revision": created, "dry_run": true }),
        )
        .await;
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");
}

/// 리뷰 1: webhook URL은 줄인 글, 헤더 값은 표시로 남는다. 되돌리기는 저장한 URL·헤더 값을 그대로 두고, 지운 대상은
/// 줄인 URL을 채울 값이 없어 막힌다.
#[tokio::test]
async fn webhook_targets_restore_without_their_hidden_url_and_headers() {
    let (env, admin) = start().await;
    let hook = |format: &str| {
        doc(
            "NotificationTarget",
            "hook",
            json!({ "target": { "kind": "webhook", "url": "https://hooks.example.com/T0/PATHSECRET",
                "format": format, "headers": { "X-Api-Key": "HEADERSECRET" } } }),
        )
    };
    apply(&admin, json!({ "documents": [hook("ntfy")] })).await;
    apply(&admin, json!({ "documents": [hook("slack")] })).await;
    let items = rows(&admin, "notification-target", "hook").await;
    assert_eq!(items.len(), 2, "{items:?}");
    let created = items[1]["revision"].as_u64().unwrap();
    restore(
        &admin,
        "/history/notification-target/hook/restore",
        json!({ "revision": created }),
    )
    .await;
    let now = env
        .cluster
        .get_by_name::<NotificationTargetSpec>("hook")
        .unwrap()
        .unwrap();
    let TargetConfig::Webhook(w) = &now.spec.target else {
        panic!("webhook")
    };
    assert_eq!(w.format, opl_types::alert::WebhookFormat::Ntfy);
    assert_eq!(w.url, "https://hooks.example.com/T0/PATHSECRET");
    assert_eq!(w.headers["X-Api-Key"], "HEADERSECRET");

    let (status, _) = admin.delete("/notification-targets/hook").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let items = rows(&admin, "notification-target", "hook").await;
    assert!(!Value::Array(items.clone()).to_string().contains("SECRET"));
    let (status, plan) = admin
        .post(
            "/history/notification-target/hook/restore",
            json!({ "revision": items[0]["revision"], "dry_run": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(summary(&plan), [0, 0, 0, 0, 1], "{plan}");
    assert_eq!(
        plan["steps"][0]["problems"][0]["code"], "secret_missing",
        "{plan}"
    );
}

/// 리뷰 1 합치기: 되돌리기는 그 줄의 spec 전체다 — 그때 기본값이라 줄에 없던 칸(webhook `enabled: true`, VM
/// `max_memory_mib` 없음)도 되살린다. 그 뒤 넣은 비밀(webhook 토큰)은 되돌리지 않는다(지금 값 그대로).
#[tokio::test]
async fn restore_brings_back_fields_that_were_at_their_default() {
    let (env, admin) = start().await;
    let hook = |extra: Value| {
        let mut target =
            json!({ "kind": "webhook", "url": "https://hooks.example.com/T0", "format": "ntfy" });
        let mut spec = json!({});
        for (k, v) in extra.as_object().unwrap() {
            if k == "enabled" {
                spec[k] = v.clone();
            } else {
                target[k] = v.clone();
            }
        }
        spec["target"] = target;
        doc("NotificationTarget", "hook", spec)
    };
    apply(&admin, json!({ "documents": [hook(json!({}))] })).await;
    apply(
        &admin,
        json!({ "documents": [hook(json!({ "enabled": false, "token": "TOKEN-1" }))] }),
    )
    .await;
    let items = rows(&admin, "notification-target", "hook").await;
    let created = items.last().unwrap()["revision"].as_u64().unwrap();
    let (plan, _) = restore(
        &admin,
        "/history/notification-target/hook/restore",
        json!({ "revision": created }),
    )
    .await;
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");
    let now = env
        .cluster
        .get_by_name::<NotificationTargetSpec>("hook")
        .unwrap()
        .unwrap();
    assert!(now.spec.enabled, "enabled goes back to its default");
    let TargetConfig::Webhook(w) = &now.spec.target else {
        panic!("webhook")
    };
    assert_eq!(
        w.token.as_deref(),
        Some("TOKEN-1"),
        "secrets are not restored"
    );
    // 다시 되돌려도 바뀔 것이 없다.
    let (status, plan) = admin
        .post(
            "/history/notification-target/hook/restore",
            json!({ "revision": created, "dry_run": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");

    // 시점 되돌리기도 같다(VM의 최대 메모리를 그 뒤 넣었다).
    let docs = |max: Option<u32>| {
        let mut web = vm("web01", "/prod", 256);
        if let Some(max) = max {
            web["spec"]["max_memory_mib"] = json!(max);
        }
        json!({ "documents": [doc("Folder", "prod", json!({ "kind": "vm" })), volume("web01-root"), web] })
    };
    apply(&admin, docs(None)).await;
    let point = env.cluster.revision();
    apply(&admin, docs(Some(1024))).await;
    let web01 = env.cluster.get_by_name::<VmSpec>("web01").unwrap().unwrap();
    assert_eq!(web01.spec.max_memory_mib, Some(1024));
    let (plan, _) = restore(
        &admin,
        "/history/restore",
        json!({ "at": { "revision": point }, "scope": { "folder": "/prod" } }),
    )
    .await;
    assert_eq!(summary(&plan)[1], 1, "{plan}");
    let web01 = env.cluster.get_by_name::<VmSpec>("web01").unwrap().unwrap();
    assert_eq!(web01.spec.max_memory_mib, None);
}

#[tokio::test]
async fn deleted_objects_come_back_with_their_id_unless_they_hold_data() {
    let (env, admin) = start().await;
    let (status, sg) = admin
        .post(
            "/security-groups",
            json!({ "name": "web-sg", "spec": { "rules": rules(22) } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = sg["id"].as_str().unwrap().to_string();
    let (status, _) = admin.delete("/security-groups/web-sg").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 지운 것은 이름으로도 이력을 찾는다.
    let items = rows(&admin, "security-group", "web-sg").await;
    assert_eq!(items[0]["op"], "delete");
    let gone = items[0]["revision"].as_u64().unwrap();
    let (plan, _) = restore(
        &admin,
        &format!("/history/security-group/{id}/restore"),
        json!({ "revision": gone }),
    )
    .await;
    assert_eq!(summary(&plan), [1, 0, 0, 0, 0], "{plan}");
    let back = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg")
        .unwrap()
        .unwrap();
    assert_eq!(back.id.to_string(), id, "the same id comes back");
    assert_eq!(serde_json::to_value(&back.spec.rules).unwrap(), rules(22));

    // 이름이 그새 쓰이면 409 — 새 이름이면 새 id로
    let (status, _) = admin.delete("/security-groups/web-sg").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = admin
        .post("/security-groups", json!({ "name": "web-sg", "spec": {} }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, e) = admin
        .post(
            &format!("/history/security-group/{id}/restore"),
            json!({ "revision": gone, "dry_run": true }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["code"], "name_taken");
    let (plan, _) = restore(
        &admin,
        &format!("/history/security-group/{id}/restore"),
        json!({ "revision": gone, "name": "web-sg-old" }),
    )
    .await;
    assert_eq!(summary(&plan), [1, 0, 0, 0, 0], "{plan}");
    let old = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg-old")
        .unwrap()
        .unwrap();
    assert_ne!(old.id.to_string(), id);

    // 볼륨: 데이터가 없다.
    apply(&admin, json!({ "documents": [volume("data")] })).await;
    let (status, body) = admin.delete("/manifests/volume/data").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    if let Some(t) = body["task"].as_str() {
        admin.wait_task(&json!({ "id": t })).await;
    }
    let items = rows(&admin, "volume", "data").await;
    let (status, plan) = admin
        .post(
            "/history/volume/data/restore",
            json!({ "revision": items[0]["revision"], "dry_run": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(summary(&plan), [0, 0, 0, 0, 1], "{plan}");
    assert_eq!(plan["steps"][0]["problems"][0]["code"], "data_lost");
    let (status, e) = admin
        .post(
            "/history/volume/data/restore",
            json!({ "revision": items[0]["revision"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["code"], "plan_blocked");
}

#[tokio::test]
async fn a_folder_goes_back_to_a_point_in_time() {
    let (env, admin) = start().await;
    let docs = |memory| {
        json!({ "documents": [
            doc("Folder", "prod", json!({ "kind": "vm" })),
            volume("web01-root"), volume("web02-root"),
            vm("web01", "/prod", memory), vm("web02", "/prod", 256),
        ] })
    };
    apply(&admin, docs(256)).await;
    let point = env.cluster.revision();
    let web02 = env.cluster.get_by_name::<VmSpec>("web02").unwrap().unwrap();
    // 그 뒤: web01 메모리 바꿈, web02 지움(디스크는 남음), web03 만듦
    apply(&admin, docs(512)).await;
    let (status, body) = admin.delete("/vms/web02").await;
    assert!(status.is_success(), "{body}");
    apply(
        &admin,
        json!({ "documents": [volume("web03-root"), vm("web03", "/prod", 256)] }),
    )
    .await;

    let body = json!({
        "at": { "revision": point },
        "scope": { "folder": "/prod" },
        "delete_created": true,
    });
    // VM 지우기는 데이터를 잃는다 — 따로 허락해야 한다.
    let mut dry = body.clone();
    dry["dry_run"] = json!(true);
    let (status, plan) = admin.post("/history/restore", dry).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(summary(&plan), [1, 1, 0, 0, 1], "{plan}");
    let blocked: Vec<_> = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["action"] == "blocked")
        .map(|s| (s["name"].clone(), s["problems"][0]["code"].clone()))
        .collect();
    assert_eq!(blocked, [(json!("web03"), json!("protected"))], "{plan}");

    let mut allowed = body.clone();
    allowed["allow_data_loss"] = json!(true);
    let (plan, _) = restore(&admin, "/history/restore", allowed).await;
    assert_eq!(summary(&plan), [1, 1, 1, 0, 0], "{plan}");
    let web01 = env.cluster.get_by_name::<VmSpec>("web01").unwrap().unwrap();
    assert_eq!(web01.spec.memory_mib, 256);
    let back = env.cluster.get_by_name::<VmSpec>("web02").unwrap().unwrap();
    assert_eq!(back.id, web02.id, "the deleted VM keeps its id");
    assert!(
        env.cluster
            .get_by_name::<VmSpec>("web03")
            .unwrap()
            .is_none()
    );
    // 범위가 없으면 거절
    let (status, _) = admin
        .post(
            "/history/restore",
            json!({ "at": { "revision": point }, "scope": {}, "dry_run": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn history_hides_objects_the_caller_cannot_read() {
    let (env, admin) = start().await;
    let (_, mine) = admin
        .post("/folders", json!({ "name": "mine", "kind": "vm" }))
        .await;
    admin
        .post("/folders", json!({ "name": "theirs", "kind": "vm" }))
        .await;
    apply(
        &admin,
        json!({ "documents": [volume("a-root"), volume("b-root"), vm("a", "/mine", 256), vm("b", "/theirs", 256)] }),
    )
    .await;
    let (status, user) = admin
        .post(
            "/users",
            json!({ "name": "alice", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    let (status, body) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": user["id"] },
                "role": { "type": "builtin", "role": "operator" },
                "target": { "type": "folder", "id": mine["id"] },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let alice = env.login("alice", "password-1").await;
    let (status, list) = alice.get("/history?kind=vm").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let names: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["a"], "{list}");
    let (status, _) = alice.get("/history/vm/b").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let admin_rows = rows(&admin, "vm", "b").await;
    let (status, _) = alice
        .post(
            "/history/vm/b/restore",
            json!({ "revision": admin_rows[0]["revision"], "dry_run": true }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 있지만 읽을 수 없는 것과 없는 것은 거르기와 관계없이 같은 404다(리뷰 1).
    for q in [
        "kind=vm&key=b&user=nobody",
        "kind=vm&key=no-such-vm&user=nobody",
    ] {
        let (status, body) = alice.get(&format!("/history?{q}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{q}: {body}");
    }
    // 읽을 수 없는 폴더에서 옮겨 온 객체: 그때의 줄은 보이지 않는다(리뷰 1).
    let (status, moved) = admin
        .post(
            &format!("/folders/{}/members", mine["id"].as_str().unwrap()),
            json!({ "items": [{ "kind": "vm", "key": "b" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["items"][0]["ok"], true, "{moved}");
    let all = rows(&admin, "vm", "b").await;
    let seen = rows(&alice, "vm", "b").await;
    assert!(!seen.is_empty() && seen.len() < all.len(), "{seen:?}");
    assert_eq!(seen[0]["revision"], all[0]["revision"]);
    let hidden = all.last().unwrap()["revision"].as_u64().unwrap();
    let (status, _) = alice.get(&format!("/history/vm/b/{hidden}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, row) = alice
        .get(&format!("/history/vm/b/{}", all[0]["revision"]))
        .await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert!(
        row["against"].is_null(),
        "the /theirs row is not compared: {row}"
    );
}

#[tokio::test]
async fn drift_names_who_changed_it_and_adopt_accepts_it() {
    let (env, admin) = start().await;
    let checker = opl_api::DriftChecker::spawn(
        env.state.clone(),
        opl_api::DriftCheckerConfig {
            tick: Duration::from_millis(100),
            full: Duration::from_secs(1),
        },
    );
    let sg = |desc: &str| {
        doc(
            "SecurityGroup",
            "web-sg",
            json!({ "description": desc, "rules": [] }),
        )
    };
    apply(
        &admin,
        json!({ "documents": [sg("a"), doc("SecurityGroup", "db-sg", json!({}))], "apply_set": "web" }),
    )
    .await;
    // 웹에서 설명을 바꾼다.
    let (status, _) = send_as(
        &admin,
        reqwest::Method::PATCH,
        "/security-groups/web-sg",
        json!({ "description": "b" }),
        "web",
        Some("hotfix"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let set = wait_drift(&admin, |items| !items.is_empty()).await;
    let items = set["status"]["drift"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{set}");
    assert_eq!(items[0]["name"], "web-sg");
    assert_eq!(items[0]["state"], "modified");
    assert_eq!(items[0]["paths"], json!(["description"]));
    assert_eq!(items[0]["by"]["actor"]["user"], "admin");
    assert_eq!(items[0]["by"]["source"], "web");
    assert_eq!(items[0]["by"]["reason"], "hotfix");

    // 받아들이기: 묶음 선언만 바뀌고 리소스는 그대로
    let (status, adopted) = admin
        .post(
            "/apply-sets/web/adopt",
            json!({ "objects": ["SecurityGroup/web-sg"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{adopted}");
    let obj = adopted["status"]["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "web-sg")
        .unwrap()
        .clone();
    assert_eq!(obj["declared"]["spec"]["description"], "b", "{obj}");
    assert_eq!(adopted["status"]["drift"]["items"], Value::Null);
    let set = wait_drift(&admin, |items| items.is_empty()).await;
    assert!(set["status"]["drift"]["items"].is_null(), "{set}");
    // 밖에서 지우면 없음
    let (status, _) = admin.delete("/security-groups/db-sg").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let set = wait_drift(&admin, |items| !items.is_empty()).await;
    let item = &set["status"]["drift"]["items"][0];
    assert_eq!(item["state"], "missing", "{set}");
    assert_eq!(item["by"]["actor"]["user"], "admin");
    checker.shutdown().await;
}

/// 리뷰 1(웹 5): 받아들이기 "모두"(`{}`)는 이 주체가 볼 수 있고 바꿀 수 있는 드리프트만 받아들인다.
#[tokio::test]
async fn adopting_all_takes_only_what_the_caller_may_change() {
    let (env, admin) = start().await;
    let checker = opl_api::DriftChecker::spawn(
        env.state.clone(),
        opl_api::DriftCheckerConfig {
            tick: Duration::from_millis(100),
            full: Duration::from_secs(1),
        },
    );
    let (_, mine) = admin
        .post("/folders", json!({ "name": "mine", "kind": "vm" }))
        .await;
    admin
        .post("/folders", json!({ "name": "theirs", "kind": "vm" }))
        .await;
    apply(
        &admin,
        json!({ "documents": [volume("a-root"), volume("b-root"), vm("a", "/mine", 256), vm("b", "/theirs", 256)],
                "apply_set": "web" }),
    )
    .await;
    for name in ["a", "b"] {
        let v = env.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap();
        let mut spec = v.spec.clone();
        spec.memory_mib = 512;
        env.cluster
            .update_spec(v.id, None, v.labels.clone(), &spec)
            .await
            .unwrap();
    }
    wait_drift(&admin, |items| items.len() == 2).await;
    let (_, user) = admin
        .post(
            "/users",
            json!({ "name": "alice", "password": "password-1", "role": "none" }),
        )
        .await;
    let (status, body) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": user["id"] },
                "role": { "type": "builtin", "role": "operator" },
                "target": { "type": "folder", "id": mine["id"] },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let alice = env.login("alice", "password-1").await;
    let (status, adopted) = alice.post("/apply-sets/web/adopt", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{adopted}");
    let (_, set) = admin.get("/apply-sets/web").await;
    let declared = |name: &str| {
        set["status"]["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == name)
            .unwrap()["declared"]["spec"]["memory_mib"]
            .clone()
    };
    assert_eq!(declared("a"), 512, "{set}");
    assert_eq!(declared("b"), 256, "the hidden one stays: {set}");
    let items = set["status"]["drift"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{set}");
    assert_eq!(items[0]["name"], "b");
    checker.shutdown().await;
}

/// 드리프트 보고가 `ok`가 될 때까지 기다린다.
async fn wait_drift(admin: &Client, ok: impl Fn(&[Value]) -> bool) -> Value {
    for _ in 0..100 {
        let (_, set) = admin.get("/apply-sets/web").await;
        let items = set["status"]["drift"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if set["status"]["drift"].is_object() && ok(&items) {
            return set;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the drift report did not settle");
}

#[tokio::test]
async fn terraform_objects_are_not_taken_without_force() {
    let (env, admin) = start().await;
    let sg = |desc: &str| {
        doc(
            "SecurityGroup",
            "web-sg",
            json!({ "description": desc, "rules": [] }),
        )
    };
    apply(
        &admin,
        json!({ "documents": [sg("a")], "manager": "terraform" }),
    )
    .await;
    let (status, plan) = admin
        .post(
            "/apply?dry_run=true",
            json!({ "documents": [sg("b")], "apply_set": "web" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(summary(&plan), [0, 0, 0, 0, 1], "{plan}");
    assert_eq!(plan["steps"][0]["problems"][0]["code"], "managed_elsewhere");
    // 그대로인 것은 충돌이 아니다(Terraform이 다시 적용).
    let (_, plan) = admin
        .post(
            "/apply?dry_run=true",
            json!({ "documents": [sg("a")], "manager": "terraform" }),
        )
        .await;
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");
    // 넘겨받기
    apply(
        &admin,
        json!({ "documents": [sg("b")], "apply_set": "web", "force_conflicts": true }),
    )
    .await;
    let group = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg")
        .unwrap()
        .unwrap();
    assert_eq!(group.spec.description.as_deref(), Some("b"));
    assert_eq!(
        group.labels.get(APPLY_SET_LABEL).map(String::as_str),
        Some("web")
    );
    assert!(!group.labels.contains_key(MANAGED_BY_LABEL));
    // 이제 Terraform이 묶음 객체를 바꾸려 하면 막힌다.
    let (_, plan) = admin
        .post(
            "/apply?dry_run=true",
            json!({ "documents": [sg("c")], "manager": "terraform" }),
        )
        .await;
    assert_eq!(
        plan["steps"][0]["problems"][0]["code"], "managed_elsewhere",
        "{plan}"
    );
}
