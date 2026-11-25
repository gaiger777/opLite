//! 계획 72 스테이지 3: VMware 원본 연결·인벤토리 찾아보기·이전 계획·VM마다 미리 확인 API를 가짜 vSphere(기능 `fake-vsphere`)에
//! 대고 본다 — 지문 확인 흐름, 비밀번호가 보기·watch에 없음, 세션·인벤토리 캐시, 계획 만들기(없는 VM·같은 VM 두 계획 →
//! 409 `vm_in_migration`), 미리 확인(우리 쪽 검사 + VMware 쪽 표·게스트 힌트, 숨은 풀은 거절만), 시작·멈춤·이어 가기·취소의
//! 상태, 지우기, 권한(원본은 `cluster.admin`, 계획은 루트 `vm.create`), 옛 노드 → 409 `cluster_upgrading`. qemu-img 없이 돈다.
//! `cargo test -p opl-api --features fake-vsphere --test vmware_api`

mod common;

use std::time::Duration;

use common::{Client, Env};
use opl_api::vmware::fake::{FakeConfig, FakeVsphere};
use opl_api::vmware_sources::VsphereCache;
use opl_types::node::{NodeSpec, VMWARE_MIGRATE};
use opl_types::vmware::{VmwareMigrationSpec, VmwareVmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 이 노드가 `feature`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_feature(env: &Env, feature: &str) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != feature);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

fn source_body(fake: &FakeVsphere, name: &str) -> Value {
    let spec = fake.source_spec();
    json!({
        "name": name,
        "host": spec.host,
        "port": spec.port,
        "user": spec.user,
        "password": spec.password.expose(),
        "fingerprint": spec.fingerprint,
    })
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

/// 가짜 vSphere와 원본 연결 `vc1`, 풀 `local`
async fn setup() -> (Env, Client, FakeVsphere) {
    let env = Env::start().await;
    let admin = env.admin().await;
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let (status, body) = admin
        .post("/vmware-sources", source_body(&fake, "vc1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    local_pool(&env, &admin, "local", None).await;
    (env, admin, fake)
}

/// 두 VM을 `user` 네트워크와 `local` 풀로 옮기는 계획 본문
fn plan_body(name: &str, vms: Value) -> Value {
    json!({
        "name": name,
        "source": "vc1",
        "mode": "warm",
        "rules": {
            "networks": [
                { "from": "VM Network", "to": "user" },
                { "from": "dvpg-web", "to": "user" },
            ],
            "default_pool": "local",
        },
        "vms": vms,
    })
}

/// db01(Windows)은 이 시험 노드(aarch64 `virt`)에서 SATA를 쓸 수 없어 virtio로 덮어쓴다.
fn both() -> Value {
    json!([
        { "ref": "DC1/vm/Prod/web01" },
        { "ref": "db01-placeholder", "overrides": { "disk_bus": "virtio-blk", "nic_model": "virtio" } },
    ])
}

fn with_db(fake: &FakeVsphere, mut vms: Value) -> Value {
    for v in vms.as_array_mut().unwrap() {
        if v["ref"] == "db01-placeholder" {
            v["ref"] = json!(fake.moref("db01"));
        }
    }
    vms
}

fn count(fake: &FakeVsphere, method: &str) -> usize {
    fake.requests()
        .iter()
        .filter(|r| r.method == method)
        .count()
}

fn codes(vm: &Value) -> Vec<String> {
    vm["problems"]
        .as_array()
        .unwrap_or_else(|| panic!("{vm}"))
        .iter()
        .map(|p| {
            format!(
                "{}:{}",
                p["at"].as_str().unwrap(),
                p["code"].as_str().unwrap()
            )
        })
        .collect()
}

fn item<'a>(view: &'a Value, name: &str) -> &'a Value {
    view["vms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name || v["item"] == name)
        .unwrap_or_else(|| panic!("{name} not in {view}"))
}

/// 지문 확인 흐름(지문 없음 → 409와 지문 둘, 틀린 비밀번호 → 400), 비밀번호 없는 보기, 세션 재사용과 인벤토리 60초 캐시,
/// 바꾸기(틀린 비밀번호는 저장하지 않음)·다시 확인·지우기.
#[tokio::test]
async fn sources_check_the_fingerprint_hide_the_password_and_cache_sessions() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let fake = FakeVsphere::start(FakeConfig::default()).await;

    let mut body = source_body(&fake, "vc1");
    body.as_object_mut().unwrap().remove("fingerprint");
    let (status, err) = admin.post("/vmware-sources", body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["code"], "untrusted_certificate");
    assert_eq!(err["details"]["fingerprint"], json!(fake.fingerprint));
    assert_eq!(
        err["details"]["thumbprint_sha1"],
        json!(fake.thumbprint_sha1)
    );
    assert_eq!(count(&fake, "Login"), 0, "no credentials before trust");

    let mut body = source_body(&fake, "vc1");
    body["password"] = json!("wrong");
    let (status, err) = admin.post("/vmware-sources", body).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("vmware_auth")),
        "{err}"
    );
    let (status, err) = admin
        .post("/vmware-sources", json!({ "name": "x", "host": "a/b", "user": "u", "password": "p", "fingerprint": fake.fingerprint }))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_vmware_source")),
        "{err}"
    );

    let password = fake.source_spec().password.expose().to_string();
    let (status, created) = admin
        .post("/vmware-sources", source_body(&fake, "vc1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["status"]["kind"], "vcenter");
    assert_eq!(created["status"]["version"], "8.0.2");
    for (_, got) in [
        ("create", created.clone()),
        ("get", admin.get("/vmware-sources/vc1").await.1),
        ("list", admin.get("/vmware-sources").await.1),
    ] {
        assert!(!got.to_string().contains(&password), "{got}");
        assert!(!got.to_string().contains("\"password\""), "{got}");
    }

    // 만들기의 세션을 찾아보기가 다시 쓰고, 두 번째 찾아보기는 노드 캐시다.
    let logins = count(&fake, "Login");
    let (status, inv) = admin.get("/vmware-sources/vc1/inventory").await;
    assert_eq!(status, StatusCode::OK, "{inv}");
    let names: Vec<&str> = inv["inventory"]["vms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["web01", "db01"]);
    assert_eq!(inv["inventory"]["vms"][0]["path"], "DC1/vm/Prod/web01");
    assert_eq!(inv["migrating"], json!([]));
    let (_, again) = admin.get("/vmware-sources/vc1/inventory").await;
    assert_eq!(again["fetched_at"], inv["fetched_at"]);
    assert_eq!(count(&fake, "CreateContainerView"), 1);
    admin
        .get("/vmware-sources/vc1/inventory?refresh=true")
        .await;
    assert_eq!(count(&fake, "CreateContainerView"), 2);
    assert_eq!(
        count(&fake, "Login"),
        logins,
        "the cached session was reused"
    );

    // 틀린 비밀번호로 바꾸면 저장하지 않는다.
    let (status, err) = admin
        .patch("/vmware-sources/vc1", json!({ "password": "nope" }), None)
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("vmware_auth")),
        "{err}"
    );
    let stored = env
        .cluster
        .get_by_name::<opl_types::vmware::VmwareSourceSpec>("vc1")
        .unwrap()
        .unwrap();
    assert_eq!(stored.spec.password.expose(), password);
    let (status, got) = admin
        .patch(
            "/vmware-sources/vc1",
            json!({ "datacenter_hint": "DC1" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["datacenter_hint"], "DC1");
    assert!(!got.to_string().contains(&password));

    let (status, got) = admin
        .post("/vmware-sources/vc1/actions/check", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(got["status"]["checked_at"].as_u64().is_some());

    let (status, _) = admin.delete("/vmware-sources/vc1").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// 리뷰 1: 저장한 비밀번호는 다른 서버로 가지 않는다 — 호스트·포트·사용자를 바꾸면서 비밀번호를 주지 않으면 400
/// `password_required`이고 새 서버에 로그인(비밀번호)이 가지 않는다. 비밀번호를 같이 주면 바뀐다.
#[tokio::test]
async fn changing_the_endpoint_needs_the_password_again() {
    let (_env, admin, fake) = setup().await;
    let other = FakeVsphere::start(FakeConfig::default()).await;
    let spec = other.source_spec();
    other.clear_requests();
    for body in [
        json!({ "host": "127.0.0.2" }),
        json!({ "port": spec.port, "fingerprint": spec.fingerprint }),
        json!({ "user": "someone-else" }),
    ] {
        let (status, err) = admin.patch("/vmware-sources/vc1", body.clone(), None).await;
        assert_eq!(
            (status, err["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("password_required")),
            "{body}: {err}"
        );
    }
    assert!(other.requests().is_empty(), "{:?}", other.requests());
    // 같은 값을 다시 주는 것은 바꾸는 것이 아니다.
    let mine = fake.source_spec();
    let (status, got) = admin
        .patch(
            "/vmware-sources/vc1",
            json!({ "host": mine.host, "port": mine.port, "user": mine.user }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    // 비밀번호를 같이 주면 새 서버로 바뀐다.
    let (status, got) = admin
        .patch(
            "/vmware-sources/vc1",
            json!({
                "port": spec.port, "fingerprint": spec.fingerprint,
                "password": spec.password.expose(),
            }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["port"], spec.port);
    assert_eq!(count(&other, "Login"), 1);
}

/// 쉰 세션은 닫는다(로그아웃), 다음 요청은 다시 로그인한다.
#[tokio::test]
async fn idle_sessions_are_logged_out() {
    let env = Env::start_with_state(|s| {
        s.vmware_cache = std::sync::Arc::new(VsphereCache::new(
            Duration::from_millis(200),
            Duration::from_millis(1),
        ))
    })
    .await;
    let admin = env.admin().await;
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let (status, body) = admin
        .post("/vmware-sources", source_body(&fake, "vc1"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while count(&fake, "Logout") == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the idle session was not logged out"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (status, _) = admin.get("/vmware-sources/vc1/inventory").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count(&fake, "Login"), 2);
}

/// 계획 만들기·미리 확인·시작·멈춤·빼기·이어 가기·취소·지우기
#[tokio::test]
async fn migrations_are_planned_checked_started_and_cancelled() {
    let (env, admin, fake) = setup().await;

    // 없는 VM은 400, 아무것도 남지 않는다.
    let (status, err) = admin
        .post(
            "/vmware-migrations",
            plan_body("w0", json!([{ "ref": "vm-nope" }])),
        )
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("vmware_vm_not_found")),
        "{err}"
    );
    let (status, err) = admin
        .post("/vmware-migrations", plan_body("w0", json!([])))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

    let (status, made) = admin
        .post(
            "/vmware-migrations",
            plan_body("wave1", with_db(&fake, both())),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["migration"]["status"]["phase"], "draft");
    assert_eq!(made["migration"]["status"]["counts"], json!({ "draft": 2 }));
    let names: Vec<&str> = made["vms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["wave1-db01", "wave1-web01"]);
    let web = item(&made, "wave1-web01");
    assert_eq!(web["spec"]["target_name"], "web01");
    assert_eq!(web["status"]["state"], "draft");
    assert_eq!(web["status"]["source"]["vm"]["name"], "web01");

    // 같은 VM은 끝나지 않은 다른 계획에 들 수 없다(원자적으로 — 앞의 항목도 남지 않는다).
    let (status, err) = admin
        .post(
            "/vmware-migrations",
            plan_body(
                "wave2",
                json!([{ "ref": fake.moref("db01") }, { "ref": "DC1/vm/Prod/web01" }]),
            ),
        )
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("vm_in_migration")),
        "{err}"
    );
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("vmware-migration \"wave1\""),
        "{err}"
    );
    assert_eq!(
        env.cluster.list::<VmwareMigrationSpec>().unwrap().1.len(),
        1
    );
    assert_eq!(env.cluster.list::<VmwareVmSpec>().unwrap().1.len(), 2);
    // 찾아보기가 계획에 든 VM을 알린다.
    let (_, inv) = admin.get("/vmware-sources/vc1/inventory").await;
    assert_eq!(inv["migrating"].as_array().unwrap().len(), 2, "{inv}");
    // 원본을 쓰는 계획이 있으면 지울 수 없다.
    let (status, err) = admin.delete("/vmware-sources/vc1").await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("source_in_use")),
        "{err}"
    );

    // 미리 확인: web01은 우리 쪽을 통과하고(노드·스펙 미리 보기) RHEL initramfs 경고, db01은 Tools 없음 경고.
    let (status, pre) = admin
        .post("/vmware-migrations/wave1/preflight", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{pre}");
    let web = item(&pre, "wave1-web01");
    assert_eq!(web["level"], "warn", "{web}");
    // Tools가 알린 IP가 있고 NIC이 vmxnet3 → virtio로 바뀐다.
    assert_eq!(
        codes(web),
        ["guest:guest_initramfs", "guest:guest_nic_renamed"]
    );
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap().name;
    assert_eq!(web["node"], json!(node));
    assert_eq!(web["spec_preview"]["disks"][1]["volume"], "web01-disk1");
    assert_eq!(web["spec_preview"]["nics"][1]["network"], "user");
    assert_eq!(web["spec_preview"]["machine"], "virt");
    let db = item(&pre, "wave1-db01");
    assert_eq!(db["level"], "warn", "{db}");
    assert!(
        codes(db).contains(&"source:vmware_no_tools".to_string()),
        "{db}"
    );
    assert!(
        codes(db).contains(&"guest:guest_drivers".to_string()),
        "{db}"
    );
    // 아무것도 만들지 않았다.
    assert!(
        env.cluster
            .list::<opl_types::storage::VolumeSpec>()
            .unwrap()
            .1
            .is_empty()
    );

    // 사용자 스냅샷이 생기면 막힌다(다시 읽기).
    fake.add_user_snapshot("web01", "before upgrade");
    let (_, pre) = admin
        .post(
            "/vmware-migrations/wave1/preflight",
            json!({ "vms": ["web01"], "refresh": true }),
        )
        .await;
    assert_eq!(pre["vms"].as_array().unwrap().len(), 1, "{pre}");
    let web = item(&pre, "wave1-web01");
    assert_eq!(web["level"], "blocked");
    assert!(
        codes(web).contains(&"source:vmware_snapshots".to_string()),
        "{web}"
    );

    // 시작: 막힌 web01은 failed, db01은 pending(노드를 적음)
    let (status, started) = admin
        .post("/vmware-migrations/wave1/actions/start", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["migration"]["status"]["phase"], "running");
    assert_eq!(
        started["migration"]["status"]["counts"],
        json!({ "failed": 1, "pending": 1 })
    );
    let web = item(&started, "wave1-web01");
    assert_eq!(web["status"]["state"], "failed");
    assert!(
        web["status"]["error"]
            .as_str()
            .unwrap()
            .contains("vmware_snapshots"),
        "{web}"
    );
    let db = item(&started, "wave1-db01");
    assert_eq!(db["status"]["state"], "pending");
    assert_eq!(db["spec"]["node"], json!(env.node));
    let (status, err) = admin
        .post("/vmware-migrations/wave1/actions/start", json!({}))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("migration_not_draft")),
        "{err}"
    );
    let (status, err) = admin
        .patch(
            "/vmware-migrations/wave1",
            json!({ "remove_vms": ["web01"] }),
            None,
        )
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("migration_running")),
        "{err}"
    );

    // 멈추고, 막힌 VM을 빼고 한도를 바꾸고, 잇는다.
    let (status, got) = admin
        .post("/vmware-migrations/wave1/actions/pause", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["migration"]["status"]["phase"], "paused");
    let (status, got) = admin
        .patch(
            "/vmware-migrations/wave1",
            json!({ "remove_vms": ["web01"], "limits": { "max_parallel": 2 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["vms"].as_array().unwrap().len(), 1);
    assert_eq!(got["migration"]["spec"]["limits"]["max_parallel"], 2);
    let (status, err) = admin
        .patch(
            "/vmware-migrations/wave1",
            json!({ "limits": { "max_parallel": 0 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    let (status, got) = admin
        .post("/vmware-migrations/wave1/actions/resume", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["migration"]["status"]["phase"], "running");
    let (status, err) = admin.delete("/vmware-migrations/wave1").await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("migration_active")),
        "{err}"
    );

    // 항목 목록
    let (status, list) = admin.get("/vmware-vms?migration=wave1").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (status, _) = admin.get("/vmware-vms?migration=nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, got) = admin.get("/vmware-vms/wave1-db01").await;
    assert_eq!(status, StatusCode::OK, "{got}");

    // 취소: 아무것도 하지 않은 항목은 곧 cancelled, 그 VM을 새 계획에 넣을 수 있다.
    let (status, got) = admin
        .post("/vmware-migrations/wave1/actions/cancel", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["migration"]["status"]["phase"], "cancelled");
    assert_eq!(
        got["migration"]["status"]["counts"],
        json!({ "cancelled": 1 })
    );
    let (status, got) = admin
        .post(
            "/vmware-migrations",
            plan_body(
                "wave2",
                json!([{ "ref": fake.moref("db01"), "target_name": "db" }]),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(got["vms"][0]["spec"]["target_name"], "db");
    let (status, _) = admin.delete("/vmware-migrations/wave1").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = admin.delete("/vmware-migrations/wave2").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(env.cluster.list::<VmwareVmSpec>().unwrap().1.is_empty());
    let (status, _) = admin.delete("/vmware-sources/vc1").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// 권한: 원본 만들기는 `cluster.admin`(operator 403), 찾아보기·계획은 루트 `vm.create`(operator 됨, viewer 403), 보기는
/// viewer도 된다. 미리 확인은 할당할 수 없는(볼 수 없는) 풀·네트워크를 거절만 하고 더 깊은 검사(노드·용량)를 돌리지 않는다.
#[tokio::test]
async fn permissions_and_hidden_objects() {
    let (env, admin, fake) = setup().await;
    let operator = env.user("op", "operator").await;
    let viewer = env.user("vi", "viewer").await;
    let (status, _) = operator
        .post("/vmware-sources", source_body(&fake, "vc2"))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, list) = viewer.get("/vmware-sources").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let (status, _) = viewer.get("/vmware-sources/vc1/inventory").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = viewer
        .post(
            "/vmware-migrations",
            plan_body("v", json!([{ "ref": "DC1/vm/Prod/web01" }])),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, made) = operator
        .post(
            "/vmware-migrations",
            plan_body("op1", json!([{ "ref": "DC1/vm/Prod/web01" }])),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let (status, got) = viewer.get("/vmware-migrations/op1").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (status, _) = viewer
        .post("/vmware-migrations/op1/actions/start", json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 루트 `vm.create`만 가진 사용자: 풀 `local`·네트워크를 쓸(볼) 수 없다.
    let (status, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "migrator", "privileges": ["vm.create"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": "mig", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": body["id"] },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "root" },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mig = env.login("mig", "password-1").await;
    let (status, _) = operator
        .post("/vmware-migrations/op1/actions/cancel", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut body = plan_body("mig1", json!([{ "ref": "DC1/vm/Prod/web01" }]));
    body["rules"]["networks"] = json!([]);
    let (status, made) = mig.post("/vmware-migrations", body).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let (status, pre) = mig
        .post("/vmware-migrations/mig1/preflight", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{pre}");
    let web = item(&pre, "mig1-web01");
    assert_eq!(web["level"], "blocked");
    // 매핑 없는 네트워크(제안은 볼 수 있는 네트워크만 고른다)는 매핑 문제
    assert!(
        codes(web).iter().any(|c| c.ends_with(":network_unmapped")),
        "{web}"
    );
    assert!(web.get("node").is_none(), "{web}");

    let rules = plan_body("x", json!([]))["rules"].clone();
    let (status, got) = admin
        .patch("/vmware-migrations/mig1", json!({ "rules": rules }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (_, pre) = mig
        .post("/vmware-migrations/mig1/preflight", json!({}))
        .await;
    let web = item(&pre, "mig1-web01");
    let ours: Vec<String> = codes(web)
        .into_iter()
        .filter(|c| !c.starts_with("source:") && !c.starts_with("guest:"))
        .collect();
    // 볼륨마다 풀 권한 거절만(그 뒤 할당량·배치·용량 검사 없이)
    assert_eq!(ours, ["disks.0:forbidden", "disks.1:forbidden"], "{web}");
    assert!(web.get("spec_preview").is_none(), "{web}");
    assert!(web.get("node").is_none(), "{web}");
    assert!(!web.to_string().contains("pool_space"), "{web}");
}

/// 옛 노드가 있으면 새 경로는 409 `cluster_upgrading`
#[tokio::test]
async fn old_nodes_make_vmware_routes_wait() {
    let (env, admin, fake) = setup().await;
    drop_feature(&env, VMWARE_MIGRATE).await;
    for (status, body) in [
        admin
            .post("/vmware-sources", source_body(&fake, "vc2"))
            .await,
        admin.get("/vmware-sources/vc1/inventory").await,
        admin
            .post(
                "/vmware-migrations",
                plan_body("w", json!([{ "ref": "DC1/vm/Prod/web01" }])),
            )
            .await,
    ] {
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
}
