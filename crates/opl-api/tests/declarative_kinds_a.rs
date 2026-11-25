//! 계획 73 스테이지 3 선언 종류(백업 저장소·원격·백업 작업·영역·알림 대상·경보 규칙·설정): 적용으로 만들고 다시 적용하면
//! 모두 `unchanged`, 칸 하나 바꾸면 update 하나, 내보내기 → 적용 왕복, 비밀은 계획·내보내기·적용 묶음에 없고 내보낸 파일을
//! 다시 적용해도 그대로, 저장한 비밀이 없는 참조는 막힘(`secret_missing`), 바꿀 수 없는 칸은 막힘, 설정은 지우지 않음, 설정
//! `history` PATCH의 기능 게이트와 범위.

mod common;

use common::{Client, Env};
use opl_store::crypto::MasterKey;
use opl_store::fake_s3::FakeS3;
use opl_types::alert::{NotificationTargetSpec, TargetConfig};
use opl_types::backup_store::{BackupStoreSpec, StoreBackend};
use opl_types::node::{CONFIG_HISTORY, NodeSpec};
use opl_types::realm::{RealmBackend, RealmSpec};
use opl_types::remote::RemoteSpec;
use opl_types::resource::Resource;
use opl_types::settings::SettingsSpec;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn doc(kind: &str, name: &str, spec: Value) -> Value {
    json!({ "apiVersion": "orbivisor.io/v1", "kind": kind, "metadata": { "name": name }, "spec": spec })
}

async fn dry_run(client: &Client, body: Value) -> Value {
    let (status, plan) = client.post("/apply?dry_run=true", body).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    plan
}

/// 적용하고 Task가 끝나기를 기다린다(성공해야 한다).
async fn apply(client: &Client, body: Value) {
    let (status, started) = client.post("/apply", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let task = client.wait_task(&started["task"]).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

fn summary(plan: &Value) -> [u64; 5] {
    let s = &plan["summary"];
    ["create", "update", "delete", "unchanged", "blocked"].map(|k| s[k].as_u64().unwrap())
}

/// 이름이 `name`인 단계
fn step<'a>(plan: &'a Value, name: &str) -> &'a Value {
    plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no step {name}: {plan}"))
}

/// 내보낸 묶음을 다시 계획하면 모두 그대로다.
async fn export_round_trip(admin: &Client, set: &str, expect: usize) -> Vec<Value> {
    let (status, exported) = admin.get(&format!("/manifests?apply_set={set}")).await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let docs = exported["documents"].as_array().unwrap().clone();
    assert_eq!(docs.len(), expect, "{exported}");
    let plan = dry_run(admin, json!({ "documents": docs, "apply_set": set })).await;
    assert_eq!(summary(&plan), [0, 0, 0, expect as u64, 0], "{plan}");
    docs
}

fn assert_hidden(text: &str, secrets: &[&str]) {
    for s in secrets {
        assert!(!text.contains(s), "{s} leaked: {text}");
    }
}

/// 알림 대상 둘(webhook·SMTP)·경보 규칙·LDAP 영역·설정
fn ops(severity: &str) -> Vec<Value> {
    vec![
        doc(
            "AlertRule",
            "vm-failed",
            json!({ "condition": { "kind": "vm_failed" }, "severity": severity, "targets": ["hook", "mail"] }),
        ),
        doc(
            "NotificationTarget",
            "hook",
            json!({ "target": { "kind": "webhook", "url": "https://hooks.example.com/T0/HOOKPATHSECRET",
                "format": "ntfy", "token": "HOOKTOKENSECRET", "headers": { "X-Api-Key": "HEADERSECRET" } } }),
        ),
        doc(
            "NotificationTarget",
            "mail",
            json!({ "target": { "kind": "smtp", "host": "127.0.0.1", "port": 2525, "tls": "none", "user": "ops",
                "password": "SMTPPASSWORDSECRET", "from": "o@example.com", "to": ["a@example.com"] } }),
        ),
        doc(
            "Realm",
            "corp",
            json!({ "display": "Corp", "backend": { "kind": "ldap", "url": "ldaps://ldap.example.com",
                "bind_dn": "cn=svc,dc=example,dc=com", "bind_password": "BINDPASSWORDSECRET",
                "base_dn": "dc=example,dc=com" }, "roles": { "default": "viewer" } }),
        ),
        doc(
            "Settings",
            "cluster",
            json!({ "placement": { "mode": "auto" }, "history": { "max_per_resource": 50 } }),
        ),
    ]
}

const OPS_SECRETS: [&str; 5] = [
    "HOOKPATHSECRET",
    "HOOKTOKENSECRET",
    "HEADERSECRET",
    "SMTPPASSWORDSECRET",
    "BINDPASSWORDSECRET",
];

#[tokio::test]
async fn alerting_realms_and_settings_apply_round_trip() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let body = json!({ "documents": ops("warning"), "apply_set": "ops" });

    let plan = dry_run(&admin, body.clone()).await;
    assert_eq!(summary(&plan), [5, 0, 0, 0, 0], "{plan}");
    // 비밀 칸(토큰·비밀번호)은 계획에 없다(webhook URL·헤더 값은 비밀 칸이 아니다 — 내보내기만 줄인다).
    assert_hidden(
        &plan.to_string(),
        &[
            "HOOKTOKENSECRET",
            "SMTPPASSWORDSECRET",
            "BINDPASSWORDSECRET",
        ],
    );
    // 알림 대상이 규칙보다 먼저
    assert!(step(&plan, "hook")["n"].as_u64() < step(&plan, "vm-failed")["n"].as_u64());
    apply(&admin, body.clone()).await;

    let settings = env
        .cluster
        .get_by_name::<SettingsSpec>("cluster")
        .unwrap()
        .unwrap();
    assert_eq!(settings.spec.history().max_per_resource, 50);
    assert_eq!(
        settings.spec.placement().mode,
        opl_types::settings::PlacementMode::Auto
    );
    let realm = env
        .cluster
        .get_by_name::<RealmSpec>("corp")
        .unwrap()
        .unwrap();
    assert_eq!(
        realm
            .labels
            .get("orbivisor.io/apply-set")
            .map(String::as_str),
        Some("ops")
    );

    // 다시 적용하면 모두 그대로, 칸 하나 바꾸면 update 하나
    let plan = dry_run(&admin, body.clone()).await;
    assert_eq!(summary(&plan), [0, 0, 0, 5, 0], "{plan}");
    let changed = json!({ "documents": ops("critical"), "apply_set": "ops" });
    let plan = dry_run(&admin, changed.clone()).await;
    assert_eq!(summary(&plan), [0, 1, 0, 4, 0], "{plan}");
    assert_eq!(step(&plan, "vm-failed")["action"], "update");
    apply(&admin, changed).await;

    // 비밀 하나 바꾸기: 계획에는 "(changed)"만
    let mut docs = ops("critical");
    docs[3]["spec"]["backend"]["bind_password"] = json!("NEWBINDSECRET");
    let plan = dry_run(&admin, json!({ "documents": docs, "apply_set": "ops" })).await;
    assert_eq!(summary(&plan), [0, 1, 0, 4, 0], "{plan}");
    assert_hidden(&plan.to_string(), &["NEWBINDSECRET", "BINDPASSWORDSECRET"]);

    // 내보내기: 비밀은 참조, webhook URL은 줄이고 헤더 값은 비움. 그대로 다시 적용하면 모두 그대로, 비밀도 그대로
    let docs = export_round_trip(&admin, "ops", 5).await;
    let text = Value::Array(docs.clone()).to_string();
    assert_hidden(&text, &OPS_SECRETS);
    let realm_doc = docs.iter().find(|d| d["kind"] == "Realm").unwrap();
    assert_eq!(
        realm_doc["spec"]["backend"]["bind_password"],
        json!({ "secretRef": "Realm/corp/backend.bind_password" })
    );
    let settings_doc = docs.iter().find(|d| d["kind"] == "Settings").unwrap();
    assert!(settings_doc["spec"].get("subscription").is_none());
    apply(&admin, json!({ "documents": docs, "apply_set": "ops" })).await;
    let realm = env
        .cluster
        .get_by_name::<RealmSpec>("corp")
        .unwrap()
        .unwrap();
    let RealmBackend::Ldap(ldap) = &realm.spec.backend else {
        panic!("ldap")
    };
    assert_eq!(ldap.bind_password.as_deref(), Some("BINDPASSWORDSECRET"));
    let hook = env
        .cluster
        .get_by_name::<NotificationTargetSpec>("hook")
        .unwrap()
        .unwrap();
    let TargetConfig::Webhook(w) = &hook.spec.target else {
        panic!("webhook")
    };
    assert_eq!(w.url, "https://hooks.example.com/T0/HOOKPATHSECRET");
    assert_eq!(w.token.as_deref(), Some("HOOKTOKENSECRET"));
    assert_eq!(w.headers["X-Api-Key"], "HEADERSECRET");
    // 적용 묶음 status에도 비밀이 없다.
    let (_, set) = admin.get("/apply-sets/ops").await;
    assert_hidden(&set.to_string(), &OPS_SECRETS);

    // 참조만 준 새 영역은 막힘(저장한 비밀이 없다)
    let mut fresh = realm_doc.clone();
    fresh["metadata"]["name"] = json!("corp2");
    let plan = dry_run(&admin, json!({ "documents": [fresh] })).await;
    assert_eq!(summary(&plan), [0, 0, 0, 0, 1], "{plan}");
    assert_eq!(
        step(&plan, "corp2")["problems"][0]["code"],
        "secret_missing"
    );

    // 설정은 지우지 않는다: 가지치기는 건너뛰고 DELETE /manifests는 400
    let rest: Vec<Value> = ops("critical")
        .into_iter()
        .filter(|d| d["kind"] != "Settings" && d["kind"] != "Realm")
        .collect();
    let plan = dry_run(
        &admin,
        json!({ "documents": rest.clone(), "apply_set": "ops", "prune": true }),
    )
    .await;
    assert_eq!(summary(&plan), [0, 0, 1, 3, 0], "{plan}");
    assert_eq!(step(&plan, "corp")["action"], "delete");
    apply(
        &admin,
        json!({ "documents": rest, "apply_set": "ops", "prune": true }),
    )
    .await;
    assert!(
        env.cluster
            .get_by_name::<RealmSpec>("corp")
            .unwrap()
            .is_none()
    );
    let (status, body) = admin.delete("/manifests/settings/cluster").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        env.cluster
            .get_by_name::<SettingsSpec>("cluster")
            .unwrap()
            .is_some()
    );
    // 지우기: 경보 규칙 → 알림 대상(쓰는 규칙이 없어야 한다)
    let (status, body) = admin.delete("/manifests/alert-rule/vm-failed").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin.delete("/manifests/notification-target/mail").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 이 노드가 `feature`를 모르는 것처럼
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

#[tokio::test]
async fn settings_history_is_patched_in_range_behind_its_feature() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "history": { "max_age_days": 30 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["history"],
        json!({ "max_per_resource": 100, "max_age_days": 30, "max_total_mib": 64 })
    );
    for bad in [
        json!({ "max_per_resource": 5 }),
        json!({ "max_age_days": 4000 }),
        json!({ "max_total_mib": 1024 }),
    ] {
        let (status, body) = admin
            .patch("/settings", json!({ "history": bad }), None)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    // 기본값으로 돌리면 칸이 없어진다.
    let (_, body) = admin
        .patch(
            "/settings",
            json!({ "history": { "max_age_days": 365 } }),
            None,
        )
        .await;
    assert!(body["spec"].get("history").is_none(), "{body}");
    drop_feature(&env, CONFIG_HISTORY).await;
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "history": { "max_age_days": 30 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
}

/// 백업 풀(디렉터리)
async fn backup_pool(env: &Env, name: &str) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes: vec![],
            shared: false,
            backups: true,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_stores_and_jobs_apply_round_trip_without_secrets() {
    let env = Env::start().await;
    let admin = env.admin().await;
    backup_pool(&env, "backups").await;
    let s3 = FakeS3::start("bkt", "AKID", "S3SECRETACCESSKEY").await;
    let target = serde_json::to_value(s3.target(Some("prod"))).unwrap();
    let mk = MasterKey::generate();
    let key = mk.to_base64();
    let docs = |streams: u8| {
        vec![
            doc(
                "BackupJob",
                "nightly",
                json!({ "vms": { "labels": { "tier": "prod" } }, "store": "s3",
                        "schedule": { "at": "03:00" }, "keep": { "last": 3 } }),
            ),
            doc(
                "BackupStore",
                "s3",
                json!({ "backend": { "s3": target }, "max_streams": streams, "encryption": { "key": key } }),
            ),
            doc(
                "BackupJob",
                "to-pool",
                json!({ "vms": { "all": true }, "pool": "backups", "schedule": { "at": "04:00" } }),
            ),
        ]
    };
    let body = json!({ "documents": docs(4), "apply_set": "bk" });
    let plan = dry_run(&admin, body.clone()).await;
    assert_eq!(summary(&plan), [3, 0, 0, 0, 0], "{plan}");
    assert!(step(&plan, "s3")["n"].as_u64() < step(&plan, "nightly")["n"].as_u64());
    assert_hidden(&plan.to_string(), &["S3SECRETACCESSKEY", &key]);
    // 초기화 Task를 기다린다(적용이 끝나면 저장소가 준비됐다).
    apply(&admin, body.clone()).await;
    let store = env
        .cluster
        .get_by_name::<BackupStoreSpec>("s3")
        .unwrap()
        .unwrap();
    assert_eq!(store.spec.encryption.as_ref().unwrap().key, key);
    assert_eq!(store.spec.max_streams, 4);

    let plan = dry_run(&admin, body).await;
    assert_eq!(summary(&plan), [0, 0, 0, 3, 0], "{plan}");
    let changed = json!({ "documents": docs(2), "apply_set": "bk" });
    let plan = dry_run(&admin, changed.clone()).await;
    assert_eq!(summary(&plan), [0, 1, 0, 2, 0], "{plan}");
    apply(&admin, changed).await;
    let store = env
        .cluster
        .get_by_name::<BackupStoreSpec>("s3")
        .unwrap()
        .unwrap();
    assert_eq!(store.spec.max_streams, 2);

    // 내보내기 왕복, 비밀 없음, 다시 적용해도 키·S3 비밀 그대로
    let exported = export_round_trip(&admin, "bk", 3).await;
    let text = Value::Array(exported.clone()).to_string();
    assert_hidden(&text, &["S3SECRETACCESSKEY", &key]);
    apply(
        &admin,
        json!({ "documents": exported.clone(), "apply_set": "bk" }),
    )
    .await;
    let store = env
        .cluster
        .get_by_name::<BackupStoreSpec>("s3")
        .unwrap()
        .unwrap();
    assert_eq!(store.spec.encryption.as_ref().unwrap().key, key);
    let StoreBackend::S3(s) = &store.spec.backend else {
        panic!("s3")
    };
    assert_eq!(s.secret_access_key, "S3SECRETACCESSKEY");
    let (_, set) = admin.get("/apply-sets/bk").await;
    assert_hidden(&set.to_string(), &["S3SECRETACCESSKEY", &key]);

    // 바꿀 수 없는 백엔드 칸(버킷)은 막힘, 작업의 저장소 → 풀은 update
    let mut moved = exported.clone();
    let i = moved
        .iter()
        .position(|d| d["kind"] == "BackupStore")
        .unwrap();
    moved[i]["spec"]["backend"]["s3"]["bucket"] = json!("other");
    let plan = dry_run(&admin, json!({ "documents": moved })).await;
    assert_eq!(step(&plan, "s3")["action"], "blocked", "{plan}");
    assert_eq!(step(&plan, "s3")["problems"][0]["code"], "immutable");
    let job = doc(
        "BackupJob",
        "nightly",
        json!({ "vms": { "names": ["web"] }, "pool": "backups" }),
    );
    let plan = dry_run(&admin, json!({ "documents": [job.clone()] })).await;
    assert_eq!(step(&plan, "nightly")["action"], "blocked", "{plan}");
    assert_eq!(
        step(&plan, "nightly")["problems"][0]["code"],
        "missing_reference"
    );
    let job = doc(
        "BackupJob",
        "nightly",
        json!({ "vms": { "all": true }, "pool": "backups" }),
    );
    apply(&admin, json!({ "documents": [job] })).await;
    let j = env
        .cluster
        .get_by_name::<opl_types::backup_job::BackupJobSpec>("nightly")
        .unwrap()
        .unwrap();
    assert_eq!(j.spec.pool, "backups");
    assert!(j.spec.store.is_none());
    assert_eq!(j.spec.vms, opl_types::backup_job::VmSelector::All);

    // 저장소 가지치기는 데이터 종류라 prune_data 없이는 막힘
    let plan = dry_run(
        &admin,
        json!({ "documents": [], "apply_set": "bk", "prune": true }),
    )
    .await;
    assert_eq!(step(&plan, "s3")["problems"][0]["code"], "protected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remotes_apply_and_keep_their_token() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let remote = Env::start().await;
    let server = remote.serve_tls().await;
    let token = remote.admin().await.token.unwrap();
    let body = json!({ "documents": [doc("Remote", "dr", json!({
        "url": server.url, "ca_fingerprint": remote.ca_fingerprint(), "token": token }))], "apply_set": "dr" });
    let plan = dry_run(&admin, body.clone()).await;
    assert_eq!(summary(&plan), [1, 0, 0, 0, 0], "{plan}");
    assert_hidden(&plan.to_string(), &[&token]);
    apply(&admin, body.clone()).await;
    let r = local
        .cluster
        .get_by_name::<RemoteSpec>("dr")
        .unwrap()
        .unwrap();
    assert_eq!(r.spec.token, token);
    assert!(!r.spec.ca_pem.is_empty());
    let plan = dry_run(&admin, body).await;
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");

    let exported = export_round_trip(&admin, "dr", 1).await;
    let text = Value::Array(exported.clone()).to_string();
    assert_hidden(&text, &[&token]);
    assert!(exported[0]["spec"].get("ca_pem").is_none(), "{text}");
    apply(&admin, json!({ "documents": exported, "apply_set": "dr" })).await;
    let r = local
        .cluster
        .get_by_name::<RemoteSpec>("dr")
        .unwrap()
        .unwrap();
    assert_eq!(r.spec.token, token);
    let (status, body) = admin.delete("/manifests/remote/dr").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        local
            .cluster
            .get_by_name::<RemoteSpec>("dr")
            .unwrap()
            .is_none()
    );
}
