//! 다른 클러스터가 보내는 백업 사본 받기: `POST /backups/receive`가 받는 중인 백업을 만들고,
//! `PUT /backups/{key}/files/{file}`이 파일을 받아 SHA-256과 크기를 돌려주며, `actions/complete`가
//! 매니페스트(받는 쪽 id)를 쓰고 `ready`로 바꾼다. 권한, 풀·매니페스트·파일 이름 검사, 같은 원본의
//! 사본, 받는 중 지우기, 옛 노드, 증분 사본의 부모 사본.

mod common;

use common::{Client, Env, vm_request};
use opl_types::backup::{
    Backup, BackupManifest, BackupPhase, BackupSpec, MANIFEST_FILE, MANIFEST_VERSION,
    MANIFEST_VERSION_INCREMENTAL,
};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const FINGERPRINT: &str = "sha256:abababababababababababababababababababababababababababababababab";

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 이 노드에서 준비된 dir 풀을 저장소에 직접 만든다. `nodes`가 비면 모든 노드의 것이다.
async fn dir_pool(env: &Env, name: &str, backups: bool, nodes: Vec<Uuid>) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes,
            shared: false,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 백업 풀 `dr`과 백업이 아닌 풀 `local`
async fn setup(env: &Env) {
    dir_pool(env, "dr", true, vec![]).await;
    dir_pool(env, "local", false, vec![]).await;
}

/// 디스크 둘(크기 3, 5바이트)과 UEFI 변수가 있는 원본 백업의 매니페스트
fn manifest() -> Value {
    let mut spec = vm_request("web")["spec"].clone();
    spec["disks"] = json!([
        { "volume": "web-root", "bus": "virtio-blk" },
        { "volume": "web-data", "bus": "virtio-blk" }
    ]);
    json!({
        "version": 1,
        "backup": Uuid::now_v7(),
        "vm": Uuid::now_v7(),
        "vm_name": "web",
        "vm_spec": spec,
        "disks": [
            { "name": "disk0", "file": "disk0.qcow2", "virtual_bytes": 1 << 20, "stored_bytes": 3 },
            { "name": "disk1", "file": "disk1.qcow2", "virtual_bytes": 2 << 20, "stored_bytes": 5 }
        ],
        "uefi_vars": true,
        // 2026-10-27 05:07:09 UTC
        "created_at": 1_793_077_629u64
    })
}

fn origin(backup: &Value) -> Value {
    json!({ "cluster": "main", "ca_fingerprint": FINGERPRINT, "backup": backup, "job": "nightly" })
}

fn receive_body(manifest: &Value) -> Value {
    json!({ "pool": "dr", "manifest": manifest, "origin": origin(&manifest["backup"]) })
}

/// 받기를 시작하고 받는 쪽 백업 id를 돌려준다.
async fn receive_ok(client: &Client, body: Value) -> Uuid {
    let (status, got) = client.post("/backups/receive", body).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    got["id"].as_str().unwrap().parse().unwrap()
}

/// 파일 하나를 보낸다(`Content-Length`가 붙는다).
async fn put_file(client: &Client, backup: Uuid, file: &str, data: &[u8]) -> (StatusCode, Value) {
    let r = client
        .http
        .put(format!(
            "{}/api/v1/backups/{backup}/files/{file}",
            client.base
        ))
        .bearer_auth(client.token.as_ref().unwrap())
        .body(data.to_vec());
    client.send(r).await
}

async fn complete(client: &Client, backup: Uuid) -> (StatusCode, Value) {
    client
        .post(&format!("/backups/{backup}/actions/complete"), Value::Null)
        .await
}

fn get_backup(env: &Env, id: Uuid) -> Option<Backup> {
    env.cluster.get::<BackupSpec>(id).unwrap()
}

/// 풀 `dr`에 있는 백업 디렉터리
fn dir_of(env: &Env, id: Uuid) -> std::path::PathBuf {
    opl_types::backup::backup_dir(&env.dir.path().join("dr"), id)
}

/// 온라인인 다른 노드를 저장소에 만든다.
async fn other_node(env: &Env, name: &str) -> Uuid {
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status.online = true;
    node.status.version = Some(opl_types::version::BUILD.into());
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 받기는 받는 중인 백업을 만든다: VM·스펙·디스크·UEFI 변수·만든 시각은 매니페스트, 출처는 요청,
/// 파일을 둘 노드는 이 노드(공유가 아닌 풀)다. 이름이 없으면 원본 이름 규칙이고 겹치면 `-2`…를 붙인다.
#[tokio::test]
async fn receive_records_a_receiving_backup_with_its_origin() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let m = manifest();
    let (status, got) = admin.post("/backups/receive", receive_body(&m)).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(got["name"], "web-20261027-050709");
    let id: Uuid = got["id"].as_str().unwrap().parse().unwrap();
    let b = get_backup(&env, id).unwrap();
    assert_eq!(b.name, "web-20261027-050709");
    assert_eq!(b.status.phase, BackupPhase::Receiving);
    assert_eq!(b.spec.vm.to_string(), m["vm"].as_str().unwrap());
    assert_eq!(b.spec.vm_name, "web");
    assert_eq!(b.spec.pool, "dr");
    assert_eq!(b.spec.job, None);
    assert_eq!(
        b.spec.vm_spec,
        serde_json::from_value::<VmSpec>(m["vm_spec"].clone()).unwrap()
    );
    let o = b.spec.origin.as_ref().unwrap();
    assert_eq!(
        (o.cluster.as_str(), o.ca_fingerprint.as_str()),
        ("main", FINGERPRINT)
    );
    assert_eq!(o.backup.to_string(), m["backup"].as_str().unwrap());
    assert_eq!(o.job.as_deref(), Some("nightly"));
    assert_eq!(serde_json::to_value(&b.status.disks).unwrap(), m["disks"]);
    assert!(b.status.uefi_vars);
    assert_eq!(b.status.created_at, Some(1_793_077_629));
    assert_eq!(b.status.node, Some(env.node));
    assert!(dir_of(&env, id).is_dir());
    // 목록과 보기에 출처가 보인다.
    let (_, list) = admin.get("/backups").await;
    assert_eq!(list["items"][0]["spec"]["origin"]["cluster"], "main");

    // 다른 원본이 같은 이름을 원하면 뒤에 번호를 붙인다. 이름을 주면 그것이다.
    let second = receive_ok(&admin, receive_body(&manifest())).await;
    assert_eq!(
        get_backup(&env, second).unwrap().name,
        "web-20261027-050709-2"
    );
    let mut body = receive_body(&manifest());
    body["name"] = json!("copy");
    let named = receive_ok(&admin, body.clone()).await;
    assert_eq!(get_backup(&env, named).unwrap().name, "copy");
    body["origin"]["backup"] = json!(Uuid::now_v7());
    let again = receive_ok(&admin, body).await;
    assert_eq!(get_backup(&env, again).unwrap().name, "copy-2");
}

/// 받기·파일·완료는 백업 쓰기 권한이다(operator 이상).
#[tokio::test]
async fn receiving_needs_backup_write() {
    let env = Env::start().await;
    setup(&env).await;
    let viewer = env.user("vic", "viewer").await;
    let operator = env.user("olga", "operator").await;
    assert_error(
        &viewer
            .post("/backups/receive", receive_body(&manifest()))
            .await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let id = receive_ok(&operator, receive_body(&manifest())).await;
    assert_error(
        &put_file(&viewer, id, "disk0.qcow2", b"abc").await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_error(
        &complete(&viewer, id).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_eq!(
        put_file(&operator, id, "disk0.qcow2", b"abc").await.0,
        StatusCode::OK
    );
    // 받는 중인 백업은 viewer도 읽는다.
    assert_eq!(
        viewer.get(&format!("/backups/{id}")).await.0,
        StatusCode::OK
    );
}

/// 받는 풀은 이 노드에서 쓸 수 있는 백업 풀이어야 한다. 매니페스트는 이 노드가 읽는 버전이고, 디스크
/// 파일은 `disk{i}.qcow2`, 디스크 수는 VM 스펙과 같아야 한다.
#[tokio::test]
async fn receive_checks_the_pool_and_the_manifest() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let post = |body: Value| {
        let admin = admin.clone();
        async move { admin.post("/backups/receive", body).await }
    };

    let mut body = receive_body(&manifest());
    body["pool"] = json!("local");
    assert_error(
        &post(body.clone()).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    body["pool"] = json!("nope");
    assert_error(
        &post(body).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    // 다른 노드의 dir 풀: 등록 URL을 그 노드로 하거나 NFS 백업 풀을 쓴다.
    let other = other_node(&env, "node2").await;
    dir_pool(&env, "far", true, vec![other]).await;
    let mut body = receive_body(&manifest());
    body["pool"] = json!("far");
    let got = post(body).await;
    assert_error(&got, StatusCode::CONFLICT, "pool_not_on_node");
    assert!(
        got.1["message"].as_str().unwrap().contains("NFS"),
        "{}",
        got.1
    );

    let bad = |edit: &dyn Fn(&mut Value)| {
        let mut m = manifest();
        edit(&mut m);
        receive_body(&m)
    };
    for body in [
        bad(&|m| m["version"] = json!(2)),
        bad(&|m| m["disks"][1]["file"] = json!("../disk1.qcow2")),
        bad(&|m| m["disks"][1]["file"] = json!("manifest.json")),
        bad(&|m| m["disks"][0]["file"] = json!("disk1.qcow2")),
        bad(&|m| m["disks"][1]["name"] = json!("x")),
        bad(&|m| {
            m["disks"].as_array_mut().unwrap().pop();
        }),
        bad(&|m| m["vm_name"] = json!("Web/..")),
        bad(&|m| m["vm_spec"]["memory_mib"] = json!(0)),
    ] {
        let got = post(body.clone()).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let mut body = receive_body(&manifest());
    body["name"] = json!("Bad Name");
    assert_error(
        &post(body).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert!(env.cluster.list::<BackupSpec>().unwrap().1.is_empty());
}

/// 같은 원본(CA 지문과 원본 id)의 사본이 있으면 409 `replica_exists`이고 그 id를 알려 준다.
#[tokio::test]
async fn a_second_copy_of_the_same_origin_is_refused() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let m = manifest();
    let id = receive_ok(&admin, receive_body(&m)).await;
    let got = admin.post("/backups/receive", receive_body(&m)).await;
    assert_error(&got, StatusCode::CONFLICT, "replica_exists");
    assert_eq!(got.1["details"]["backup"], id.to_string());
    // 다른 클러스터의 같은 id는 다른 원본이다.
    let mut body = receive_body(&m);
    body["origin"]["ca_fingerprint"] = json!(format!("sha256:{}", "cd".repeat(32)));
    receive_ok(&admin, body).await;
    // 같은 원본이라도 다른 풀에 받으면 새 사본이다.
    dir_pool(&env, "dr2", true, vec![]).await;
    let mut body = receive_body(&m);
    body["pool"] = json!("dr2");
    let other = receive_ok(&admin, body.clone()).await;
    assert_ne!(other, id);
    let got = admin.post("/backups/receive", body).await;
    assert_error(&got, StatusCode::CONFLICT, "replica_exists");
    assert_eq!(got.1["details"]["backup"], other.to_string());
}

/// `Content-Length`만 `length`로 알리고 본문은 보내지 않은 PUT의 응답 상태와 본문. 서버가 본문을 읽기 전에
/// 거절하는지 본다.
async fn put_claiming(client: &Client, backup: Uuid, file: &str, length: u64) -> (u16, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = client.base.trim_start_matches("http://");
    let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "PUT /api/v1/backups/{backup}/files/{file} HTTP/1.1\r\nHost: {addr}\r\n\
         Authorization: Bearer {}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n",
        client.token.as_ref().unwrap()
    );
    conn.write_all(request.as_bytes()).await.unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = conn.read(&mut buf).await.unwrap();
        got.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&got);
        if n == 0 || text.trim_end().ends_with('}') {
            break;
        }
    }
    let text = String::from_utf8_lossy(&got).to_string();
    let status = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .and_then(|(_, b)| serde_json::from_str(b).ok())
        .unwrap_or(Value::Null);
    (status, body)
}

/// 디스크 파일은 매니페스트의 크기로만, `vars.fd`는 64 MiB까지만 받는다(400, 본문을 읽기 전에). 풀에
/// 매니페스트의 파일을 받을 자리가 없으면 받기를 시작하지 않는다(507 `insufficient_storage`).
#[tokio::test]
async fn sizes_are_limited_by_the_manifest_and_the_pool() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    for data in [&b"ab"[..], b"abcd"] {
        let got = put_file(&admin, id, "disk0.qcow2", data).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let (status, body) = put_claiming(&admin, id, "vars.fd", (64 << 20) + 1).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["code"], "invalid_request", "{body}");
    assert!(!dir_of(&env, id).join("vars.fd").exists());
    let got = put_file(&admin, id, "vars.fd", b"uefi").await;
    assert_eq!(got.0, StatusCode::OK, "{}", got.1);

    let mut m = manifest();
    m["disks"][0]["stored_bytes"] = json!(1u64 << 62);
    let got = admin.post("/backups/receive", receive_body(&m)).await;
    assert_error(
        &got,
        StatusCode::INSUFFICIENT_STORAGE,
        "insufficient_storage",
    );
    assert_eq!(env.cluster.list::<BackupSpec>().unwrap().1.len(), 1);
}

/// 같은 파일을 받는 중이면 그 파일의 다른 PUT은 409 `backup_busy`다. 다른 파일과 끝난 뒤는 받는다.
#[tokio::test]
async fn the_same_file_is_received_once_at_a_time() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let dir = dir_of(&env, id);
    // 세 바이트 가운데 하나만 보내고 멈춘 PUT
    let addr = admin.base.trim_start_matches("http://").to_string();
    let mut slow = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let request = format!(
        "PUT /api/v1/backups/{id}/files/disk0.qcow2 HTTP/1.1\r\nHost: {addr}\r\n\
         Authorization: Bearer {}\r\nContent-Length: 3\r\nConnection: close\r\n\r\na",
        admin.token.as_ref().unwrap()
    );
    slow.write_all(request.as_bytes()).await.unwrap();
    let deadline = std::time::Instant::now() + common::WAIT;
    while !dir.join("disk0.qcow2.part").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the upload did not start"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_error(
        &put_file(&admin, id, "disk0.qcow2", b"xyz").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    assert_eq!(
        put_file(&admin, id, "disk1.qcow2", b"abcde").await.0,
        StatusCode::OK
    );

    slow.write_all(b"bc").await.unwrap();
    let mut answer = Vec::new();
    slow.read_to_end(&mut answer).await.unwrap();
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert_eq!(std::fs::read(dir.join("disk0.qcow2")).unwrap(), b"abc");
    assert_eq!(
        put_file(&admin, id, "disk0.qcow2", b"xyz").await.0,
        StatusCode::OK
    );
}

/// 본문을 보내다 멈춘 PUT은 조각을 기다리는 시간(기본 60초, 여기서는 줄인 것)이 지나면 400
/// `invalid_request`로 끝나고 `.part`를 지우며 그 파일의 표시를 푼다. 그래서 같은 파일을 다시 받는다.
#[tokio::test]
async fn a_stalled_upload_is_cut_off() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let env = Env::start().await;
    setup(&env).await;
    // 같은 저장소를 쓰고 조각을 300ms만 기다리는 API 서버
    let hv: std::sync::Arc<dyn opl_driver::Hypervisor> = env.hv.clone();
    let mut state = opl_api::AppState::new(
        env.cluster.clone(),
        env.node,
        hv,
        env.dir.path().join("uploads"),
    );
    state.receive_idle_timeout = std::time::Duration::from_millis(300);
    let app = opl_api::router(std::sync::Arc::new(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let admin = common::login_at(&format!("http://{addr}"), "admin", common::ADMIN_PASSWORD).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let dir = dir_of(&env, id);

    // 세 바이트 가운데 하나만 보내고 멈춘 PUT
    let mut slow = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let request = format!(
        "PUT /api/v1/backups/{id}/files/disk0.qcow2 HTTP/1.1\r\nHost: {addr}\r\n\
         Authorization: Bearer {}\r\nContent-Length: 3\r\nConnection: close\r\n\r\na",
        admin.token.as_ref().unwrap()
    );
    slow.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(common::WAIT, slow.read_to_end(&mut answer))
        .await
        .expect("the stalled upload was not cut off")
        .unwrap();
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    assert!(answer.contains("the upload stalled"), "{answer}");
    assert!(!dir.join("disk0.qcow2.part").exists());
    assert!(!dir.join("disk0.qcow2").exists());

    let got = put_file(&admin, id, "disk0.qcow2", b"abc").await;
    assert_eq!(got.0, StatusCode::OK, "{}", got.1);
    assert_eq!(std::fs::read(dir.join("disk0.qcow2")).unwrap(), b"abc");
}

/// 받은 파일마다 디스크의 SHA-256과 마지막 활동 시각(`received_at`)을 적는다.
#[tokio::test]
async fn received_files_record_their_hash_and_the_time() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let b = get_backup(&env, id).unwrap();
    assert_eq!(b.status.received_at, None);
    assert!(b.status.disks.iter().all(|d| d.sha256.is_none()));

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    put_file(&admin, id, "disk1.qcow2", b"abcde").await;
    put_file(&admin, id, "vars.fd", b"uefi").await;
    let b = get_backup(&env, id).unwrap();
    assert!(b.status.received_at.unwrap() >= before, "{:?}", b.status);
    assert_eq!(b.status.disks[0].sha256, None);
    assert_eq!(
        b.status.disks[1].sha256.as_deref(),
        Some(hex::encode(Sha256::digest(b"abcde")).as_str())
    );
    // 다시 보내면 해시도 바뀐다. 완료한 뒤에도 남는다.
    put_file(&admin, id, "disk0.qcow2", b"abc").await;
    put_file(&admin, id, "disk1.qcow2", b"vwxyz").await;
    assert_eq!(complete(&admin, id).await.0, StatusCode::OK);
    let b = get_backup(&env, id).unwrap();
    let hashes: Vec<Option<String>> = b.status.disks.iter().map(|d| d.sha256.clone()).collect();
    assert_eq!(
        hashes,
        [
            Some(hex::encode(Sha256::digest(b"abc"))),
            Some(hex::encode(Sha256::digest(b"vwxyz")))
        ]
    );
}

/// 지우는 Task가 걸린 백업은 완료하지 않는다(409 `backup_busy`).
#[tokio::test]
async fn a_backup_being_deleted_is_not_completed() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    for (file, data) in [
        ("disk0.qcow2", &b"abc"[..]),
        ("disk1.qcow2", b"abcde"),
        ("vars.fd", b"uefi"),
    ] {
        assert_eq!(put_file(&admin, id, file, data).await.0, StatusCode::OK);
    }
    env.stop_runner().await;
    let (status, task) = admin.delete(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_error(
        &complete(&admin, id).await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    assert_eq!(
        get_backup(&env, id).unwrap().status.phase,
        BackupPhase::Receiving
    );
    env.start_runner().await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 파일은 `.part`에 받아 이름을 바꾸고 SHA-256과 크기를 돌려준다. 다시 보내면 덮는다. 매니페스트에
/// 없는 이름은 400, `Content-Length`가 없으면 411, 받는 중이 아니면 409.
#[tokio::test]
async fn files_are_hashed_and_overwritten_and_limited_to_the_manifest() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let dir = dir_of(&env, id);

    let (status, got) = put_file(&admin, id, "disk0.qcow2", b"old").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["sha256"], hex::encode(Sha256::digest(b"old")));
    assert_eq!(got["size"], 3);
    assert_eq!(std::fs::read(dir.join("disk0.qcow2")).unwrap(), b"old");
    let (status, got) = put_file(&admin, id, "disk0.qcow2", b"abc").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["sha256"], hex::encode(Sha256::digest(b"abc")));
    assert_eq!(std::fs::read(dir.join("disk0.qcow2")).unwrap(), b"abc");
    assert!(!dir.join("disk0.qcow2.part").exists());
    let (status, _) = put_file(&admin, id, "vars.fd", b"uefi").await;
    assert_eq!(status, StatusCode::OK);

    for file in [
        "disk2.qcow2",
        "manifest.json",
        "..%2Fdisk0.qcow2",
        "disk0.qcow2.part",
    ] {
        let got = put_file(&admin, id, file, b"x").await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    assert!(!dir.join(MANIFEST_FILE).exists());

    // 스트림 본문은 길이를 모르므로 chunked로 간다.
    let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>(b"abc".to_vec())]);
    let r = admin
        .http
        .put(format!(
            "{}/api/v1/backups/{id}/files/disk0.qcow2",
            admin.base
        ))
        .bearer_auth(admin.token.as_ref().unwrap())
        .body(reqwest::Body::wrap_stream(chunks));
    assert_error(
        &admin.send(r).await,
        StatusCode::LENGTH_REQUIRED,
        "length_required",
    );

    // UEFI 변수가 없는 백업은 vars.fd를 받지 않는다.
    let mut m = manifest();
    m["uefi_vars"] = json!(false);
    let bios = receive_ok(&admin, receive_body(&m)).await;
    assert_error(
        &put_file(&admin, bios, "vars.fd", b"x").await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_error(
        &put_file(&admin, Uuid::now_v7(), "disk0.qcow2", b"x").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// 완료는 모든 파일이 매니페스트의 크기로 있어야 한다(409 `backup_incomplete`와 빠진 파일). 쓴
/// 매니페스트의 `backup`은 받는 쪽 id다. 그 뒤로는 파일도 완료도 받지 않는다.
#[tokio::test]
async fn complete_needs_every_file_and_writes_the_manifest_with_the_local_id() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let m = manifest();
    let id = receive_ok(&admin, receive_body(&m)).await;
    let dir = dir_of(&env, id);

    put_file(&admin, id, "disk0.qcow2", b"abc").await;
    let got = complete(&admin, id).await;
    assert_error(&got, StatusCode::CONFLICT, "backup_incomplete");
    assert_eq!(got.1["details"]["files"], json!(["disk1.qcow2", "vars.fd"]));
    // 크기가 다른 파일도 다 받지 못한 것이다(PUT은 크기가 다르면 받지 않으므로 직접 쓴다).
    std::fs::write(dir.join("disk1.qcow2"), b"abcd").unwrap();
    put_file(&admin, id, "vars.fd", b"uefi").await;
    let got = complete(&admin, id).await;
    assert_error(&got, StatusCode::CONFLICT, "backup_incomplete");
    assert_eq!(got.1["details"]["files"], json!(["disk1.qcow2"]));
    assert_eq!(
        get_backup(&env, id).unwrap().status.phase,
        BackupPhase::Receiving
    );
    assert!(!dir.join(MANIFEST_FILE).exists());

    put_file(&admin, id, "disk1.qcow2", b"abcde").await;
    let (status, got) = complete(&admin, id).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["status"]["phase"], "ready");
    let b = get_backup(&env, id).unwrap();
    assert_eq!(b.status.phase, BackupPhase::Ready);
    let written: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(written.backup, id);
    assert_eq!(written.vm_spec, b.spec.vm_spec);
    assert_eq!(written.disks, b.status.disks);
    assert!(written.uefi_vars);
    assert_eq!(written.created_at, 1_793_077_629);
    assert_eq!(written.vm.to_string(), m["vm"].as_str().unwrap());

    assert_error(
        &put_file(&admin, id, "disk0.qcow2", b"abc").await,
        StatusCode::CONFLICT,
        "backup_not_receiving",
    );
    assert_error(
        &complete(&admin, id).await,
        StatusCode::CONFLICT,
        "backup_not_receiving",
    );
}

/// 다른 노드가 받는(파일이 그 노드에 있는) 백업의 파일은 그 노드로 보내야 한다.
#[tokio::test]
async fn files_of_a_backup_on_another_node_are_refused() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let other = other_node(&env, "node2").await;
    let mut b = get_backup(&env, id).unwrap();
    b.status.node = Some(other);
    env.cluster
        .update_status::<BackupSpec>(id, &b.status)
        .await
        .unwrap();
    assert_error(
        &put_file(&admin, id, "disk0.qcow2", b"abc").await,
        StatusCode::CONFLICT,
        "backup_on_other_node",
    );
    assert_error(
        &complete(&admin, id).await,
        StatusCode::CONFLICT,
        "backup_on_other_node",
    );
}

/// 받는 중인 백업을 지우면 받은 파일도 지운다.
#[tokio::test]
async fn deleting_a_receiving_backup_removes_its_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    put_file(&admin, id, "disk0.qcow2", b"abc").await;
    let dir = dir_of(&env, id);
    assert!(dir.join("disk0.qcow2").exists());
    let (status, task) = admin.delete(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(get_backup(&env, id).is_none());
    assert!(!dir.exists());
}

/// 옛 노드는 받는 중인 백업과 출처를 모르므로 모든 노드가 이 빌드일 때만 받는다.
#[tokio::test]
async fn receive_waits_for_every_node_to_run_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
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
            .post("/backups/receive", receive_body(&manifest()))
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
}

/// 받기 요청 `body`로 받기를 시작해 파일을 모두 보내고 완료한 `ready` 사본의 id
async fn ready_copy(client: &Client, body: Value) -> Uuid {
    let id = receive_ok(client, body).await;
    for (file, data) in [
        ("disk0.qcow2", &b"abc"[..]),
        ("disk1.qcow2", b"abcde"),
        ("vars.fd", b"uefi"),
    ] {
        assert_eq!(put_file(client, id, file, data).await.0, StatusCode::OK);
    }
    let (status, got) = complete(client, id).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    id
}

/// 증분 백업을 아는 온라인인 다른 노드
async fn current_node(env: &Env, name: &str) -> Uuid {
    let id = other_node(env, name).await;
    let mut node = env.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    node.status.features = opl_types::node::FEATURES
        .iter()
        .map(|f| f.to_string())
        .collect();
    env.cluster
        .update_status::<NodeSpec>(id, &node.status)
        .await
        .unwrap();
    id
}

/// 매니페스트 `parent`의 백업을 부모로 둔 증분 백업의 매니페스트(버전 2)
fn incremental_manifest(parent: &Value) -> Value {
    let mut m = manifest();
    m["version"] = json!(2);
    m["parent"] = parent["backup"].clone();
    m["vm"] = parent["vm"].clone();
    m
}

/// 받는 쪽 부모 사본이 `copy`인 증분 백업의 받기 요청
fn incremental_body(m: &Value, copy: Uuid) -> Value {
    let mut body = receive_body(m);
    body["parent"] = json!(copy);
    body
}

/// 증분 백업은 받는 쪽 부모 사본(`parent`)과 함께 받는다: 받는 중인 사본의 `status.parent`가 그 사본이고,
/// 완료가 쓴 매니페스트는 버전 2에 자기 id와 부모 사본 id다. 부모 사본은 자식이 있는 동안(받는 중 포함)
/// 지우지 못한다(409 `backup_in_use`).
#[tokio::test]
async fn an_incremental_copy_records_its_parent_copy() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let full = manifest();
    let parent = ready_copy(&admin, receive_body(&full)).await;
    let m = incremental_manifest(&full);
    let id = receive_ok(&admin, incremental_body(&m, parent)).await;
    let b = get_backup(&env, id).unwrap();
    assert_eq!(b.status.phase, BackupPhase::Receiving);
    assert_eq!(b.status.parent, Some(parent));
    assert_eq!(
        b.spec.origin.as_ref().unwrap().backup.to_string(),
        m["backup"].as_str().unwrap()
    );
    assert_error(
        &admin.delete(&format!("/backups/{parent}")).await,
        StatusCode::CONFLICT,
        "backup_in_use",
    );

    for (file, data) in [
        ("disk0.qcow2", &b"abc"[..]),
        ("disk1.qcow2", b"abcde"),
        ("vars.fd", b"uefi"),
    ] {
        assert_eq!(put_file(&admin, id, file, data).await.0, StatusCode::OK);
    }
    let (status, got) = complete(&admin, id).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["status"]["parent"], parent.to_string());
    let written: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir_of(&env, id).join(MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!(written.version, MANIFEST_VERSION_INCREMENTAL);
    assert_eq!(written.backup, id);
    assert_eq!(written.parent, Some(parent));
    // 전체 사본의 매니페스트는 그대로 버전 1에 부모가 없다.
    let written: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir_of(&env, parent).join(MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!((written.version, written.parent), (MANIFEST_VERSION, None));
    assert_error(
        &admin.delete(&format!("/backups/{parent}")).await,
        StatusCode::CONFLICT,
        "backup_in_use",
    );
}

/// 부모 사본이 없거나 맞지 않으면(다른 풀, `ready`가 아님, 다른 CA 지문, 다른 원본, 다른 노드의 파일, 지우는
/// 중) 409 `replica_parent_missing`이고 `details.parent`가 요청한 id다. 요청의 `parent`와 매니페스트의 버전·
/// `parent`가 어긋나면 400이다.
#[tokio::test]
async fn an_incremental_copy_needs_a_matching_parent_copy() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    dir_pool(&env, "dr2", true, vec![]).await;
    let full = manifest();
    let parent = ready_copy(&admin, receive_body(&full)).await;
    let m = incremental_manifest(&full);
    let missing = |got: &(StatusCode, Value), copy: Uuid| {
        assert_error(got, StatusCode::CONFLICT, "replica_parent_missing");
        assert_eq!(got.1["details"]["parent"], copy.to_string(), "{}", got.1);
    };

    // 없는 사본
    let unknown = Uuid::now_v7();
    let got = admin
        .post("/backups/receive", incremental_body(&m, unknown))
        .await;
    missing(&got, unknown);
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("send the parent first"),
        "{}",
        got.1
    );
    // 다른 풀의 사본
    let mut body = incremental_body(&m, parent);
    body["pool"] = json!("dr2");
    missing(&admin.post("/backups/receive", body).await, parent);
    // 받는 중인 사본
    let other = manifest();
    let receiving = receive_ok(&admin, receive_body(&other)).await;
    missing(
        &admin
            .post(
                "/backups/receive",
                incremental_body(&incremental_manifest(&other), receiving),
            )
            .await,
        receiving,
    );
    // 다른 클러스터(CA 지문)의 사본
    let foreign_full = manifest();
    let mut body = receive_body(&foreign_full);
    body["origin"]["ca_fingerprint"] = json!(format!("sha256:{}", "cd".repeat(32)));
    let foreign = ready_copy(&admin, body).await;
    missing(
        &admin
            .post(
                "/backups/receive",
                incremental_body(&incremental_manifest(&foreign_full), foreign),
            )
            .await,
        foreign,
    );
    // 매니페스트의 부모가 사본의 원본이 아니다.
    let mut wrong = m.clone();
    wrong["parent"] = json!(Uuid::now_v7());
    missing(
        &admin
            .post("/backups/receive", incremental_body(&wrong, parent))
            .await,
        parent,
    );
    // 공유가 아닌 풀에서 사본의 파일이 다른 노드에 있다.
    let node2 = current_node(&env, "node2").await;
    let mut b = get_backup(&env, parent).unwrap();
    b.status.node = Some(node2);
    env.cluster
        .update_status::<BackupSpec>(parent, &b.status)
        .await
        .unwrap();
    missing(
        &admin
            .post("/backups/receive", incremental_body(&m, parent))
            .await,
        parent,
    );
    b.status.node = Some(env.node);
    env.cluster
        .update_status::<BackupSpec>(parent, &b.status)
        .await
        .unwrap();

    // 버전과 parent가 어긋난다.
    let bad = |edit: &dyn Fn(&mut Value), copy: Option<Uuid>| {
        let mut m = m.clone();
        edit(&mut m);
        match copy {
            Some(copy) => incremental_body(&m, copy),
            None => receive_body(&m),
        }
    };
    for body in [
        bad(&|m| m["version"] = json!(1), Some(parent)),
        bad(&|m| m["version"] = json!(3), Some(parent)),
        bad(
            &|m| {
                m.as_object_mut().unwrap().remove("parent");
            },
            Some(parent),
        ),
        bad(&|_| {}, None),
        bad(&|m| m["version"] = json!(1), None),
    ] {
        let got = admin.post("/backups/receive", body.clone()).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let before = env.cluster.list::<BackupSpec>().unwrap().1.len();

    // 지우는 중인 사본
    env.stop_runner().await;
    let (status, task) = admin.delete(&format!("/backups/{parent}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    missing(
        &admin
            .post("/backups/receive", incremental_body(&m, parent))
            .await,
        parent,
    );
    assert_eq!(env.cluster.list::<BackupSpec>().unwrap().1.len(), before);
    env.start_runner().await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 증분 사본은 이 클러스터의 모든 노드가 증분 복제(`incremental-replication`)를 알 때만 받는다(409
/// `cluster_upgrading`). 증분 백업만 아는 계획 40 노드도 증분 받기를 모른다. 전체 사본은 그대로 받는다.
#[tokio::test]
async fn incremental_copies_wait_for_every_node_to_know_incremental_replication() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let full = manifest();
    let parent = ready_copy(&admin, receive_body(&full)).await;
    // 증분 백업은 알지만 증분 받기는 모르는 노드(계획 40)
    let old = other_node(&env, "node2").await;
    let mut node = env.cluster.get::<NodeSpec>(old).unwrap().unwrap();
    node.status.features = vec!["node-drain".into(), "incremental-backup".into()];
    env.cluster
        .update_status::<NodeSpec>(old, &node.status)
        .await
        .unwrap();
    let got = admin
        .post(
            "/backups/receive",
            incremental_body(&incremental_manifest(&full), parent),
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("incremental replication"),
        "{}",
        got.1
    );
    receive_ok(&admin, receive_body(&manifest())).await;
}

/// 완료할 때도 부모 사본을 다시 본다: 받는 사이 부모 사본이 `ready`가 아니게 됐거나 없어졌으면 409
/// `replica_parent_missing`(`details.parent`)이고 사본은 받는 중으로 남는다.
#[tokio::test]
async fn completing_an_incremental_copy_checks_its_parent_copy_again() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let full = manifest();
    let parent = ready_copy(&admin, receive_body(&full)).await;
    let id = receive_ok(
        &admin,
        incremental_body(&incremental_manifest(&full), parent),
    )
    .await;
    for (file, data) in [
        ("disk0.qcow2", &b"abc"[..]),
        ("disk1.qcow2", b"abcde"),
        ("vars.fd", b"uefi"),
    ] {
        assert_eq!(put_file(&admin, id, file, data).await.0, StatusCode::OK);
    }
    let missing = |got: &(StatusCode, Value)| {
        assert_error(got, StatusCode::CONFLICT, "replica_parent_missing");
        assert_eq!(got.1["details"]["parent"], parent.to_string(), "{}", got.1);
        assert!(
            got.1["message"]
                .as_str()
                .unwrap()
                .contains("send the parent first"),
            "{}",
            got.1
        );
    };

    // 부모 사본이 ready가 아니다.
    let mut b = get_backup(&env, parent).unwrap();
    b.status.phase = BackupPhase::Failed;
    env.cluster
        .update_status::<BackupSpec>(parent, &b.status)
        .await
        .unwrap();
    missing(&complete(&admin, id).await);
    assert_eq!(
        get_backup(&env, id).unwrap().status.phase,
        BackupPhase::Receiving
    );
    // 부모 사본이 없어졌다.
    env.cluster
        .delete::<BackupSpec>(parent, None)
        .await
        .unwrap();
    missing(&complete(&admin, id).await);
    assert_eq!(
        get_backup(&env, id).unwrap().status.phase,
        BackupPhase::Receiving
    );
}
