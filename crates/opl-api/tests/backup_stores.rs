//! 백업 저장소 API(계획 68 스테이지 6): `/backup-stores` 만들기(초기화 Task)·보기(비밀 없음)·바꾸기·지우기, 키 내보내기와
//! escrow(admin), 읽기 전용 붙이기(복구 키·escrow 암호 문구, 지문 확인), 넘겨받기 규칙, 백업 요청의 저장소 대상·라이브
//! 복원·취소·검증·옮기기·복사 API, 저장소 사이 복사 Task, 원격 복제의 없는 청크만 보내기(두 `Env`). 저장소는 가짜 S3다
//! (qemu 도구가 필요 없다 — 백업 스냅샷은 `opl_store::testing`으로 쓴다).

mod common;

use std::sync::Arc;

use common::{Client, Env, TlsServer};
use opl_store::Store;
use opl_store::backend::Backend;
use opl_store::backend::s3::S3Backend;
use opl_store::crypto::MasterKey;
use opl_store::fake_s3::FakeS3;
use opl_store::format::manifest::ConfigBlob;
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus};
use opl_types::backup_store::{BackupStoreSpec, S3Target, StorePhase};
use opl_types::node::{BACKUP_STORE, NodeSpec};
use opl_types::resource::Resource;
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: usize = 1 << 20;

fn s3_backend(target: &S3Target) -> serde_json::Value {
    json!({ "s3": serde_json::to_value(target).unwrap() })
}

fn code(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

async fn create(admin: &Client, body: Value) -> Value {
    let (status, created) = admin.post("/backup-stores", body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let task = admin.wait_task(&created["task"]).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    created
}

fn vm_spec() -> VmSpec {
    serde_json::from_value(json!({
        "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt",
        "disks": [ { "volume": "web-root", "bus": "virtio-blk" } ]
    }))
    .unwrap()
}

/// 저장소 `store`(리소스 이름)에 디스크 하나짜리 `ready` 백업을 쓰고 리소스를 만든다.
async fn store_backup(
    env: &Env,
    store_name: &str,
    target: &S3Target,
    key: Option<&MasterKey>,
    name: &str,
    vm: Uuid,
    image: Vec<u8>,
) -> Backup {
    let backend: Arc<dyn Backend> = Arc::new(S3Backend::new(target, 4).unwrap());
    let store = Store::open(backend, key).await.unwrap();
    let mut backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm,
            vm_name: "web".into(),
            pool: String::new(),
            vm_spec: vm_spec(),
            job: None,
            origin: None,
            incremental: false,
            max_chain: None,
            source: None,
            store: Some(store_name.into()),
        },
    );
    let config = ConfigBlob {
        vm_name: "web".into(),
        vm_spec: vm_spec(),
        uefi_vars: false,
        tpm_state: false,
    };
    let m = opl_store::testing::write_backup_with(
        &store,
        vm,
        backup.id,
        std::slice::from_ref(&image),
        &config,
        1_793_000_000,
    )
    .await
    .unwrap();
    backup.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(m.created_at),
        disks: vec![opl_types::backup::BackupDisk {
            name: "disk0".into(),
            file: "disk0.fidx".into(),
            virtual_bytes: image.len() as u64,
            stored_bytes: 0,
            sha256: None,
        }],
        ..Default::default()
    };
    let status = backup.status.clone();
    env.cluster.create(&backup).await.unwrap();
    env.cluster
        .update_status::<BackupSpec>(backup.id, &status)
        .await
        .unwrap();
    env.cluster.get::<BackupSpec>(backup.id).unwrap().unwrap()
}

/// 저장소의 스냅샷 디스크 하나를 다시 읽는다.
async fn read_disk(target: &S3Target, key: Option<&MasterKey>, vm: Uuid, backup: Uuid) -> Vec<u8> {
    let backend: Arc<dyn Backend> = Arc::new(S3Backend::new(target, 4).unwrap());
    let store = Store::open(backend, key).await.unwrap();
    let m = store.read_manifest(vm, backup).await.unwrap();
    let ix = store.read_index(&m, &m.disks[0]).await.unwrap();
    let mut out = Vec::new();
    for (i, id) in ix.ids.iter().enumerate() {
        out.extend(store.get_chunk(id, ix.chunk_len(i) as usize).await.unwrap());
    }
    out
}

/// 이 노드에서 준비된 dir 풀 `name`(복원 대상)
async fn local_pool(env: &Env, name: &str) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        opl_types::storage::StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

fn chunk_objects(s3: &FakeS3, prefix: &str) -> usize {
    s3.objects()
        .keys()
        .iter()
        .filter(|k| k.starts_with(&format!("{prefix}/chunks/")))
        .count()
}

fn image(seed: u8, chunks: usize) -> Vec<u8> {
    (0..chunks * 4 * MIB)
        .map(|i| {
            let chunk = (i >> 22) as u8;
            ((i >> 12) as u8).wrapping_mul(31) ^ chunk.wrapping_mul(97) ^ seed ^ (i as u8)
        })
        .collect()
}

/// 저장소 만들기·보기·바꾸기·키·지우기: 응답·목록에 S3 비밀 키와 주 키가 없고, 새 키면 복구 키가 한 번 오고, escrow가
/// 저장소 안에 생긴다. 키 내보내기는 admin만, 바꾸기는 storage.config만(operator는 403, viewer는 읽기만). 같은 버킷/접두사의
/// 둘째 저장소는 409, 백엔드는 바꾸지 못한다(400), 백업이 남은 저장소는 지우지 못한다(409).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stores_are_created_redacted_and_their_keys_guarded() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let s3 = FakeS3::start("bkt", "AKID", "secret-key-123").await;
    let target = s3.target(Some("prod"));
    let created = create(
        &admin,
        json!({
            "name": "s3",
            "spec": { "backend": s3_backend(&target), "max_streams": 4 },
            "encryption": { "generate": true },
            "escrow_passphrase": "correct horse battery",
        }),
    )
    .await;
    let text = created.to_string();
    assert!(!text.contains("secret-key-123"), "{text}");
    let recovery = created["recovery_key"].as_str().unwrap().to_string();
    assert!(recovery.starts_with("OPK1-"), "{recovery}");
    let mk = MasterKey::from_recovery_key(&recovery).unwrap();
    assert!(!text.contains(&mk.to_base64()), "{text}");
    let fp = created["store"]["spec"]["encryption"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        s3.objects()
            .keys()
            .contains(&format!("prod/keys/{fp}.json"))
    );

    let (status, got) = admin.get("/backup-stores/s3").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["status"]["phase"], "ready", "{got}");
    assert_eq!(got["spec"]["backend"]["s3"]["access_key_id"], "AKID");
    assert!(
        got["spec"]["backend"]["s3"]
            .get("secret_access_key")
            .is_none()
    );
    assert!(got["spec"]["encryption"].get("key").is_none());
    let (_, list) = admin.get("/backup-stores").await;
    assert!(!list.to_string().contains("secret-key-123"));

    // 권한: viewer는 읽기만, operator는 storage.config가 없다, 키는 admin만.
    let viewer = env.user("viewer1", "viewer").await;
    let operator = env.user("op1", "operator").await;
    assert_eq!(viewer.get("/backup-stores/s3").await.0, StatusCode::OK);
    assert_eq!(
        operator
            .patch("/backup-stores/s3", json!({ "max_streams": 2 }), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        operator
            .post("/backup-stores/s3/key/export", json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, exported) = admin.post("/backup-stores/s3/key/export", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    assert_eq!(exported["recovery_key"], recovery);

    // 바꾸기: 일정·동시 요청 수·자격 증명. 백엔드는 받지 않는다.
    let (status, patched) = admin
        .patch(
            "/backup-stores/s3",
            json!({ "max_streams": 2, "verify": { "schedule": { "at": "06:00", "days": ["sun"] } } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["spec"]["max_streams"], 2);
    assert_eq!(patched["spec"]["verify"]["reverify_days"], 30);
    let (status, body) = admin
        .patch(
            "/backup-stores/s3",
            json!({ "backend": { "pool": "bk" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 연결 확인
    let (status, check) = admin
        .post("/backup-stores/s3/actions/check", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{check}");
    assert_eq!(check["ok"], true, "{check}");

    // 같은 곳의 둘째 저장소
    let got = admin
        .post(
            "/backup-stores",
            json!({ "name": "s3b", "spec": { "backend": s3_backend(&target) } }),
        )
        .await;
    code(&got, StatusCode::CONFLICT, "store_target_in_use");
    // 형식
    let got = admin
        .post(
            "/backup-stores",
            json!({ "name": "bad", "spec": { "backend": { "s3": { "endpoint": "ftp://x", "bucket": "bk",
                    "access_key_id": "a", "secret_access_key": "b" } } } }),
        )
        .await;
    code(&got, StatusCode::BAD_REQUEST, "invalid_backup_store");

    // 백업이 남았으면 지우지 못한다.
    let b = store_backup(
        &env,
        "s3",
        &target,
        Some(&mk),
        "web-1",
        Uuid::now_v7(),
        image(1, 1),
    )
    .await;
    let got = admin.delete("/backup-stores/s3").await;
    code(&got, StatusCode::CONFLICT, "store_in_use");
    env.cluster.delete::<BackupSpec>(b.id, None).await.unwrap();
    let (status, body) = admin.delete("/backup-stores/s3").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 저장소 안 데이터는 그대로다.
    assert!(s3.objects().keys().contains(&"prod/store.json".to_string()));

    // 같은 곳을 읽기 전용으로 다시 붙인다: 키가 없으면 400, 틀린 복구 키·암호 문구는 400, escrow 암호 문구로 연다.
    let ro = |enc: Value| json!({ "name": "dr", "spec": { "backend": s3_backend(&target), "read_only": true }, "encryption": enc });
    let got = admin
        .post(
            "/backup-stores",
            json!({ "name": "dr", "spec": { "backend": s3_backend(&target), "read_only": true } }),
        )
        .await;
    code(&got, StatusCode::BAD_REQUEST, "store_key_required");
    let other = MasterKey::generate().recovery_key();
    let got = admin
        .post("/backup-stores", ro(json!({ "recovery_key": other })))
        .await;
    code(&got, StatusCode::BAD_REQUEST, "store_key_mismatch");
    let got = admin
        .post(
            "/backup-stores",
            ro(json!({ "escrow_passphrase": "wrong passphrase" })),
        )
        .await;
    code(&got, StatusCode::BAD_REQUEST, "store_key_mismatch");
    let got = admin
        .post("/backup-stores", ro(json!({ "generate": true })))
        .await;
    code(&got, StatusCode::BAD_REQUEST, "invalid_backup_store");
    let attached = create(
        &admin,
        ro(json!({ "escrow_passphrase": "correct horse battery" })),
    )
    .await;
    assert_eq!(attached["store"]["spec"]["encryption"]["fingerprint"], fp);
    assert!(attached.get("recovery_key").is_none(), "{attached}");
    // 읽기 전용: GC는 409, scan은 Task(이 클러스터의 저장소에서는 400)
    let got = admin.post("/backup-stores/dr/actions/gc", json!({})).await;
    code(&got, StatusCode::CONFLICT, "store_read_only");
    let (status, task) = admin
        .post("/backup-stores/dr/actions/scan", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // 지운 백업 web-1의 스냅샷이 저장소에 남아 있어 scan이 가져온다.
    let (_, backups) = admin.get("/backups").await;
    let items = backups["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{backups}");
    assert_eq!(items[0]["spec"]["store"], "dr");

    // 옛 노드가 있으면 만들기는 검증보다 먼저 409
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != BACKUP_STORE);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let got = admin
        .post(
            "/backup-stores",
            json!({ "name": "x", "spec": { "backend": { "pool": "bk" } } }),
        )
        .await;
    code(&got, StatusCode::CONFLICT, "cluster_upgrading");
}

/// 넘겨받기(§12.6): 다른 클러스터의 하트비트가 새로우면 409, `force`면 주인 표식을 이 클러스터로 바꾸고 읽기 전용을 끈다.
/// 하트비트가 24시간보다 오래면 `force` 없이도 된다. 쓰기 저장소를 만들 때 남의 표식이 있으면 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn take_over_follows_the_heartbeat_rule() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let s3 = FakeS3::start("bkt", "AKID", "secret-key-123").await;
    let target = s3.target(Some("dr"));
    let backend: Arc<dyn Backend> = Arc::new(S3Backend::new(&target, 4).unwrap());
    let other = Uuid::now_v7();
    let store = Store::init(backend.clone(), other, None, 1).await.unwrap();
    let owner = |heartbeat: u64| opl_store::format::OwnerFile {
        cluster: "prod".into(),
        ca_fingerprint: "sha256:00".into(),
        store_resource: other,
        heartbeat,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    store.write_owner(&owner(now)).await.unwrap();

    let got = admin
        .post(
            "/backup-stores",
            json!({ "name": "w", "spec": { "backend": s3_backend(&target) } }),
        )
        .await;
    code(&got, StatusCode::CONFLICT, "store_owned_elsewhere");
    create(
        &admin,
        json!({ "name": "dr", "spec": { "backend": s3_backend(&target), "read_only": true } }),
    )
    .await;
    let got = admin
        .post("/backup-stores/dr/actions/take-over", json!({}))
        .await;
    code(&got, StatusCode::CONFLICT, "store_owned_elsewhere");
    // 하트비트가 이틀 전이면 force 없이
    store.write_owner(&owner(now - 2 * 86400)).await.unwrap();
    let (status, taken) = admin
        .post("/backup-stores/dr/actions/take-over", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{taken}");
    assert!(taken["spec"].get("read_only").is_none(), "{taken}");
    let o = store.read_owner().await.unwrap().unwrap();
    assert_eq!(o.store_resource.to_string(), taken["id"].as_str().unwrap());
    assert_ne!(o.cluster, "prod");
    // 이제 쓰기 저장소: GC가 돈다(주인 표식이 이 클러스터다).
    let (status, task) = admin.post("/backup-stores/dr/actions/gc", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // 다시 넘겨받기는 400(이미 이 클러스터의 것)
    let got = admin
        .post(
            "/backup-stores/dr/actions/take-over",
            json!({ "force": true }),
        )
        .await;
    assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
}

/// 백업 요청: 풀과 저장소 둘 다·둘 다 없음은 400, 준비되지 않은 저장소는 409. 저장소 백업의 라이브 복원은
/// `backup-live-restore` Task, 검증은 저장소의 검증 Task(풀 백업은 400), 복사는 같은 저장소면 400. 라이브 복원 취소는
/// `discard` 없이 400, 라이브 복원 중이 아닌 VM은 409. 저장소 사이 복사 Task는 키가 다른 저장소로 다시 암호화해 쓰고,
/// 두 번째 복사는 409 `backup_exists`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_backup_requests_copy_and_cancel() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let s3 = FakeS3::start("bkt", "AKID", "secret-key-123").await;
    let (ta, tb) = (s3.target(Some("a")), s3.target(Some("b")));
    create(
        &admin,
        json!({ "name": "plain", "spec": { "backend": s3_backend(&ta) } }),
    )
    .await;
    let enc = create(
        &admin,
        json!({ "name": "enc", "spec": { "backend": s3_backend(&tb) }, "encryption": { "generate": true } }),
    )
    .await;
    let key = MasterKey::from_recovery_key(enc["recovery_key"].as_str().unwrap()).unwrap();
    let (status, vm) = admin.post("/vms", common::vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let got = admin
        .post("/vms/web/backups", json!({ "pool": "x", "store": "plain" }))
        .await;
    assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
    let got = admin.post("/vms/web/backups", json!({})).await;
    assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
    // 준비되지 않은 저장소
    let mut st = env
        .cluster
        .get_by_name::<BackupStoreSpec>("plain")
        .unwrap()
        .unwrap();
    st.status.phase = StorePhase::Initializing;
    env.cluster
        .update_status::<BackupStoreSpec>(st.id, &st.status)
        .await
        .unwrap();
    let got = admin
        .post("/vms/web/backups", json!({ "store": "plain" }))
        .await;
    code(&got, StatusCode::CONFLICT, "store_not_ready");
    st.status.phase = StorePhase::Ready;
    env.cluster
        .update_status::<BackupStoreSpec>(st.id, &st.status)
        .await
        .unwrap();

    let vm_id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let mut img = image(7, 2);
    img.extend(vec![0u8; 4 * MIB]);
    let b = store_backup(&env, "plain", &ta, None, "web-1", vm_id, img.clone()).await;

    // 복사: 같은 저장소면 400, 다른 키의 저장소로는 다시 암호화해 쓴다.
    let got = admin
        .post("/backups/web-1/actions/copy", json!({ "store": "plain" }))
        .await;
    assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
    let (status, task) = admin
        .post("/backups/web-1/actions/copy", json!({ "store": "enc" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "backup-copy", "store": "enc" })
    );
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, list) = admin.get("/backups").await;
    let copy = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["spec"]["store"] == "enc")
        .cloned()
        .unwrap();
    assert_eq!(copy["status"]["phase"], "ready", "{copy}");
    assert_eq!(copy["spec"]["source"]["kind"], "copy", "{copy}");
    assert_eq!(copy["spec"]["source"]["backup"], b.id.to_string());
    assert_eq!(copy["status"]["store_stats"]["new_chunks"], 2, "{copy}");
    assert_eq!(copy["status"]["store_stats"]["zero_chunks"], 1, "{copy}");
    let copy_id: Uuid = copy["id"].as_str().unwrap().parse().unwrap();
    assert!(read_disk(&tb, Some(&key), vm_id, copy_id).await == img);
    let got = admin
        .post("/backups/web-1/actions/copy", json!({ "store": "enc" }))
        .await;
    code(&got, StatusCode::CONFLICT, "backup_exists");

    // 검증: 저장소 백업은 검증 Task, 결과가 백업에 적힌다.
    let (status, task) = admin
        .post(&format!("/backups/{copy_id}/actions/verify"), json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, copy) = admin.get(&format!("/backups/{copy_id}")).await;
    assert_eq!(copy["status"]["verify"]["state"], "ok", "{copy}");

    // 라이브 복원 요청은 라이브 복원 Task다(Task는 돌리지 않고 모양만 본다 — 실행은 live_restore_tasks 시험).
    local_pool(&env, "local").await;
    env.stop_runner().await;
    let (status, task) = admin
        .post(
            "/backups/web-1/restore",
            json!({ "name": "web2", "pool": "local", "live": true }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "backup-live-restore", "{task}");
    assert_eq!(task["spec"]["op"]["backup"], b.id.to_string(), "{task}");
    // 라이브 복원은 새 폴더의 vm.power도 든다: operator(루트 vm.create·vm.power)는 된다, viewer는 403.
    let viewer = env.user("viewer2", "viewer").await;
    let got = viewer
        .post(
            "/backups/web-1/restore",
            json!({ "name": "web3", "pool": "local", "live": true }),
        )
        .await;
    assert_eq!(got.0, StatusCode::FORBIDDEN, "{}", got.1);
    // 취소: discard 없이 400, 라이브 복원 중이 아니면 409
    let got = admin.post("/vms/web/live-restore/cancel", json!({})).await;
    assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
    let got = admin
        .post("/vms/web/live-restore/cancel", json!({ "discard": true }))
        .await;
    code(&got, StatusCode::CONFLICT, "vm_not_restoring");
}

/// 원격 역할을 하는 클러스터와 그 HTTPS 서버
struct Remote {
    env: Env,
    server: TlsServer,
}

/// 저장소 백업의 원격 복제(§12.4): 원격의 같은 키 저장소로 원격에 없는 청크만 보낸다 — 두 번째 백업(청크 하나만 다름)은
/// 청크 하나만 보낸다. 사본은 `ready`이고 원격에서 같은 내용으로 읽힌다. 키가 다른 원격 저장소는 Task가 실패한다
/// (`store_key_mismatch`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_backups_replicate_only_missing_chunks() {
    let s3 = FakeS3::start("bkt", "AKID", "secret-key-123").await;
    let (tp, tr, tx) = (
        s3.target(Some("prod")),
        s3.target(Some("dr")),
        s3.target(Some("other")),
    );
    // 원격: 같은 키의 저장소 `dr`과 다른 키의 저장소 `other`
    let remote = Env::start().await;
    let server = remote.serve_tls().await;
    let r = Remote {
        env: remote,
        server,
    };
    let local = Env::start().await;
    let admin = local.admin().await;
    let created = create(
        &admin,
        json!({ "name": "prod", "spec": { "backend": s3_backend(&tp) }, "encryption": { "generate": true } }),
    )
    .await;
    let recovery = created["recovery_key"].as_str().unwrap().to_string();
    let key = MasterKey::from_recovery_key(&recovery).unwrap();
    let radmin = r.env.admin().await;
    create(
        &radmin,
        json!({ "name": "dr", "spec": { "backend": s3_backend(&tr) }, "encryption": { "recovery_key": recovery } }),
    )
    .await;
    create(
        &radmin,
        json!({ "name": "other", "spec": { "backend": s3_backend(&tx) }, "encryption": { "generate": true } }),
    )
    .await;
    let token = r.env.user("sender", "admin").await.token.unwrap();
    let (status, body) = admin
        .post(
            "/remotes",
            json!({ "name": "dr", "url": r.server.url, "ca_fingerprint": r.env.ca_fingerprint(), "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let vm = Uuid::now_v7();
    let first = image(3, 3);
    store_backup(&local, "prod", &tp, Some(&key), "web-1", vm, first.clone()).await;
    let replicate = |backup: &'static str, store: &'static str| {
        let admin = &admin;
        async move {
            let (status, task) = admin
                .post(
                    &format!("/backups/{backup}/actions/replicate"),
                    json!({ "remote": "dr", "pool": store }),
                )
                .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{task}");
            admin.wait_task(&task).await
        }
    };
    let task = replicate("web-1", "dr").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(chunk_objects(&s3, "dr"), 3);
    let (_, copies) = radmin.get("/backups").await;
    let copy = copies["items"][0].clone();
    assert_eq!(copy["status"]["phase"], "ready", "{copy}");
    assert_eq!(copy["spec"]["store"], "dr");
    let copy_id: Uuid = copy["id"].as_str().unwrap().parse().unwrap();
    assert!(read_disk(&tr, Some(&key), vm, copy_id).await == first);

    // 둘째 백업: 청크 하나만 바뀌었다 → 그 청크만 보낸다.
    let mut second = first.clone();
    second[5 * MIB] ^= 0xff;
    store_backup(&local, "prod", &tp, Some(&key), "web-2", vm, second.clone()).await;
    let task = replicate("web-2", "dr").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(chunk_objects(&s3, "dr"), 4);
    let (_, local_b2) = admin.get("/backups/web-2").await;
    assert_eq!(
        local_b2["status"]["replicas"][0]["state"], "ready",
        "{local_b2}"
    );

    // 키가 다른 저장소
    let task = replicate("web-1", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"].as_str().unwrap().contains("key"),
        "{task}"
    );
}
