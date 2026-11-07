//! 백업을 원격 클러스터로 복제하기: `POST /backups/{key}/actions/replicate`가 만든 `backup-replicate`
//! Task가 원격(두 번째 `Env`, 그 CA가 서명한 HTTPS)의 받기 API로 파일을 보내 사본을 만들고 원본
//! `status.replicas`에 적는다. 해시가 다른 응답, 꺼진 원격, 원격 쪽 권한, 같은 사본 이어 가기, API 검사.
//! 백업 작업의 복제(스케줄러가 ready 백업을 보내고 실패한 사본은 실행마다 한 번 더)와, 원본을 지우면
//! 원격 사본도 지우는 것(사용자·보존 규칙, 404는 성공, 실패는 경고, `force`는 원격을 건드리지 않음).

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Method;
use common::{Client, Env, TlsServer, WAIT, vm_request};
use opl_api::{BackupJobScheduler, BackupJobSchedulerConfig};
use opl_types::backup::{
    Backup, BackupManifest, BackupPhase, BackupSpec, BackupStatus, MANIFEST_FILE, ReplicaState,
    backup_dir,
};
use opl_types::backup_job::{BackupJobSpec, JobRun, JobRunResult};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 이 노드에서 준비된 백업 dir 풀 `name`을 저장소에 직접 만든다.
async fn backup_pool(env: &Env, name: &str) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups: true,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 원본 백업의 파일: 디스크 둘(하나는 여러 조각으로 갈 만큼 크다)과 UEFI 변수
fn files() -> Vec<(&'static str, Vec<u8>)> {
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    vec![
        ("disk0.qcow2", b"disk-zero".to_vec()),
        ("disk1.qcow2", big),
        ("vars.fd", b"uefi-vars".to_vec()),
    ]
}

/// 풀 `backups`에 `ready` 백업 `name`과 그 파일·매니페스트를 직접 만든다. `job`은 원본 작업 id다.
async fn ready_backup(env: &Env, name: &str, job: Option<Uuid>) -> Backup {
    let mut spec = vm_request("web")["spec"].clone();
    spec["disks"] = json!([
        { "volume": "web-root", "bus": "virtio-blk" },
        { "volume": "web-data", "bus": "virtio-blk" }
    ]);
    let files = files();
    let mut backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            pool: "backups".into(),
            vm_spec: serde_json::from_value(spec).unwrap(),
            job,
            origin: None,
        },
    );
    let manifest: BackupManifest = serde_json::from_value(json!({
        "version": 1,
        "backup": backup.id,
        "vm": backup.spec.vm,
        "vm_name": "web",
        "vm_spec": backup.spec.vm_spec,
        "disks": [
            { "name": "disk0", "file": "disk0.qcow2", "virtual_bytes": 1 << 20,
              "stored_bytes": files[0].1.len() },
            { "name": "disk1", "file": "disk1.qcow2", "virtual_bytes": 2 << 20,
              "stored_bytes": files[1].1.len() }
        ],
        "uefi_vars": true,
        "created_at": 1_793_077_629u64
    }))
    .unwrap();
    backup.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(manifest.created_at),
        disks: manifest.disks.clone(),
        uefi_vars: true,
        error: None,
        node: Some(env.node),
        replicas: vec![],
        received_at: None,
    };
    let dir = source_dir(env, backup.id);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, data) in &files {
        std::fs::write(dir.join(file), data).unwrap();
    }
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    env.cluster.create(&backup).await.unwrap();
    backup
}

fn source_dir(env: &Env, id: Uuid) -> PathBuf {
    backup_dir(&env.dir.path().join("backups"), id)
}

fn replica_dir(env: &Env, id: Uuid) -> PathBuf {
    backup_dir(&env.dir.path().join("dr"), id)
}

fn get_backup(env: &Env, id: Uuid) -> Backup {
    env.cluster.get::<BackupSpec>(id).unwrap().unwrap()
}

/// 원격 역할을 하는 클러스터(백업 풀 `dr`)와 그 HTTPS 서버
struct Remote {
    env: Env,
    server: TlsServer,
}

async fn remote_with(tweak: impl FnOnce(axum::Router) -> axum::Router) -> Remote {
    let env = Env::start().await;
    backup_pool(&env, "dr").await;
    let server = env.serve_tls_with(tweak).await;
    Remote { env, server }
}

async fn remote() -> Remote {
    remote_with(|app| app).await
}

/// 보내는 클러스터(백업 풀 `backups`)에 원격 `dr`을 원격 쪽 `role` 사용자의 토큰으로 등록한다.
async fn local_with_remote(r: &Remote, role: &str) -> (Env, Client) {
    let local = Env::start().await;
    backup_pool(&local, "backups").await;
    let admin = local.admin().await;
    let token = r.env.user("sender", role).await.token.unwrap();
    let (status, body) = admin
        .post(
            "/remotes",
            json!({
                "name": "dr",
                "url": r.server.url,
                "ca_fingerprint": r.env.ca_fingerprint(),
                "token": token,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (local, admin)
}

async fn replicate(client: &Client, backup: &str) -> (StatusCode, Value) {
    client
        .post(
            &format!("/backups/{backup}/actions/replicate"),
            json!({ "remote": "dr", "pool": "dr" }),
        )
        .await
}

/// 복제를 요청하고 Task가 끝날 때까지 기다린다.
async fn replicate_done(client: &Client, backup: &str) -> Value {
    let (status, task) = replicate(client, backup).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "backup-replicate", "remote": "dr", "pool": "dr" })
    );
    client.wait_task(&task).await
}

/// 원본에 적힌 원격 `dr`·풀 `dr`의 사본 기록 하나
fn only_replica(env: &Env, id: Uuid) -> opl_types::backup::BackupReplica {
    let replicas = get_backup(env, id).status.replicas;
    assert_eq!(replicas.len(), 1, "{replicas:?}");
    let replica = replicas[0].clone();
    assert_eq!(
        (replica.remote.as_str(), replica.pool.as_str()),
        ("dr", "dr")
    );
    assert!(replica.at > 0);
    replica
}

/// 복제는 원격의 백업 풀에 같은 파일의 `ready` 사본을 만든다: 이름은 원본 이름, 출처는 이 클러스터
/// 이름·CA 지문·원본 id·원본 작업 이름, 매니페스트의 백업 id는 원격 쪽 id. 원본에는 사본이 ready로
/// 적힌다. 원격 토큰은 operator면 된다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replication_makes_a_ready_copy_on_the_remote() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let (status, job) = admin
        .post(
            "/backup-jobs",
            json!({ "name": "nightly", "spec": {
                "vms": { "names": ["web"] }, "pool": "backups",
                "schedule": { "at": "03:00" }, "enabled": false,
            }}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let job: Uuid = job["id"].as_str().unwrap().parse().unwrap();
    let source = ready_backup(&local, "web-nightly", Some(job)).await;

    let task = replicate_done(&admin, "web-nightly").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    assert_eq!(replica.state, ReplicaState::Ready);
    assert_eq!(replica.error, None);
    let id = replica.backup.expect("remote copy id");

    let copy = get_backup(&r.env, id);
    assert_eq!(copy.name, "web-nightly");
    assert_eq!(copy.status.phase, BackupPhase::Ready);
    assert_eq!(copy.spec.vm, source.spec.vm);
    assert_eq!(copy.spec.vm_spec, source.spec.vm_spec);
    let origin = copy.spec.origin.expect("origin");
    assert_eq!(origin.cluster, "test");
    assert_eq!(origin.ca_fingerprint, local.ca_fingerprint());
    assert_eq!(origin.backup, source.id);
    assert_eq!(origin.job.as_deref(), Some("nightly"));
    let dir = replica_dir(&r.env, id);
    for (file, data) in files() {
        assert_eq!(std::fs::read(dir.join(file)).unwrap(), data, "{file}");
    }
    let manifest: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(manifest.backup, id);
}

/// 원격이 보낸 것과 다른 SHA-256을 돌려주면 Task가 실패하고 complete하지 않는다(사본은 받는 중으로
/// 남는다). 원본에는 사본이 failed와 이유로 적힌다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_hash_from_the_remote_fails_the_copy() {
    // 파일을 받은 응답의 해시를 바꾸는 원격
    let r = remote_with(|app| {
        app.layer(axum::middleware::from_fn(
            |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let tamper = req.method() == Method::PUT && req.uri().path().contains("/files/");
                let response = next.run(req).await;
                if !tamper || response.status() != StatusCode::OK {
                    return response;
                }
                let (parts, body) = response.into_parts();
                let bytes = axum::body::to_bytes(body, 1 << 16).await.unwrap();
                let mut got: Value = serde_json::from_slice(&bytes).unwrap();
                got["sha256"] = json!("0".repeat(64));
                let mut response = axum::response::Response::from_parts(
                    parts,
                    Body::from(serde_json::to_vec(&got).unwrap()),
                );
                response
                    .headers_mut()
                    .remove(axum::http::header::CONTENT_LENGTH);
                response
            },
        ))
    })
    .await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("SHA-256"), "{error}");
    let replica = only_replica(&local, source.id);
    assert_eq!(replica.state, ReplicaState::Failed);
    assert_eq!(replica.error.as_deref(), Some(error));
    let id = replica.backup.expect("remote copy id");
    assert_eq!(get_backup(&r.env, id).status.phase, BackupPhase::Receiving);
}

/// 원격에 닿지 못하면 Task가 실패하고 원본에 사본이 failed로 적힌다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_remote_fails_the_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    drop(r.server);

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("cannot reach the remote cluster"), "{error}");
    let replica = only_replica(&local, source.id);
    assert_eq!(replica.state, ReplicaState::Failed);
    assert_eq!(replica.error.as_deref(), Some(error));
    assert_eq!(replica.backup, None);
}

/// 원격 쪽 권한은 토큰 주인의 역할이다: viewer 토큰이면 원격이 403으로 거절하고 그 이유가 남는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_token_on_the_remote_is_refused() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "viewer").await;
    let source = ready_backup(&local, "web-1", None).await;

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("HTTP 403"), "{error}");
    let replica = only_replica(&local, source.id);
    assert_eq!(replica.state, ReplicaState::Failed);
    assert_eq!(replica.error.as_deref(), Some(error));
    let (_, list) = r.env.admin().await.get("/backups").await;
    assert_eq!(list["items"], json!([]), "{list}");
}

/// 원격에 같은 원본의 사본이 이미 있으면(409 `replica_exists`) 그 사본으로 이어 간다: 받는 중이면
/// 파일을 (반쯤 보낸 것도 덮어) 다시 보내고 끝내며, 이미 ready면 받은 파일의 해시가 원본과 같을 때 그대로
/// 성공한다. 원격의 사본은 하나다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicating_again_continues_with_the_existing_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    // 앞의 복제가 받기만 하고 멈춘 것처럼 원격에 받는 중인 사본을 만든다.
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(source_dir(&local, source.id).join(MANIFEST_FILE)).unwrap(),
    )
    .unwrap();
    let remote_admin = r.env.admin().await;
    let (status, started) = remote_admin
        .post(
            "/backups/receive",
            json!({
                "name": "partial",
                "pool": "dr",
                "manifest": manifest,
                "origin": {
                    "cluster": "test",
                    "ca_fingerprint": local.ca_fingerprint(),
                    "backup": source.id,
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    let id: Uuid = started["id"].as_str().unwrap().parse().unwrap();
    // 반쯤 보낸 파일: 다른 내용의 디스크 파일과 끝나지 않은 `.part`
    let put = remote_admin
        .http
        .put(format!(
            "{}/api/v1/backups/{id}/files/disk0.qcow2",
            remote_admin.base
        ))
        .bearer_auth(remote_admin.token.as_ref().unwrap())
        .body(b"XXXXXXXXX".to_vec());
    assert_eq!(remote_admin.send(put).await.0, StatusCode::OK);
    std::fs::write(replica_dir(&r.env, id).join("disk1.qcow2.part"), b"half").unwrap();

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    assert_eq!(
        (replica.state, replica.backup),
        (ReplicaState::Ready, Some(id))
    );
    let copy = get_backup(&r.env, id);
    assert_eq!(
        (copy.name.as_str(), copy.status.phase),
        ("partial", BackupPhase::Ready)
    );
    for (file, data) in files() {
        assert_eq!(
            std::fs::read(replica_dir(&r.env, id).join(file)).unwrap(),
            data
        );
    }

    // 한 번 더: 이미 ready인 사본이다.
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    assert_eq!(
        (replica.state, replica.backup),
        (ReplicaState::Ready, Some(id))
    );
    let (_, list) = remote_admin.get("/backups").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");

    // ready 사본의 해시가 원본과 다르면 실패로 알린다(사본은 그대로).
    let mut copy = get_backup(&r.env, id);
    copy.status.disks[1].sha256 = Some("0".repeat(64));
    r.env
        .cluster
        .update_status::<BackupSpec>(id, &copy.status)
        .await
        .unwrap();
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains(&id.to_string()), "{error}");
    assert!(error.contains("delete it there"), "{error}");
    assert_eq!(only_replica(&local, source.id).state, ReplicaState::Failed);
    assert_eq!(get_backup(&r.env, id).status.phase, BackupPhase::Ready);
}

/// 같은 원본의 사본이라고 하지만 풀·VM 스펙·디스크가 다른 원격 백업에는 붙지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_backup_claiming_the_origin_fails_the_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(source_dir(&local, source.id).join(MANIFEST_FILE)).unwrap(),
    )
    .unwrap();
    manifest["vm_spec"]["memory_mib"] = json!(4096);
    let (status, started) = r
        .env
        .admin()
        .await
        .post(
            "/backups/receive",
            json!({
                "pool": "dr",
                "manifest": manifest,
                "origin": {
                    "cluster": "test",
                    "ca_fingerprint": local.ca_fingerprint(),
                    "backup": source.id,
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{started}");

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.contains("a different backup claims to be the copy"),
        "{error}"
    );
    assert_eq!(only_replica(&local, source.id).state, ReplicaState::Failed);
}

/// 원격에서 사본을 지웠으면 다음 복제가 새로 받는다(적어 둔 사본 id를 원격에 물어본다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_deleted_on_the_remote_is_sent_again() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let first = only_replica(&local, source.id).backup.unwrap();
    let remote_admin = r.env.admin().await;
    let (status, task) = remote_admin.delete(&format!("/backups/{first}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    remote_admin.wait_task(&task).await;

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    let second = replica.backup.unwrap();
    assert_ne!(second, first);
    assert_eq!(replica.state, ReplicaState::Ready);
    assert_eq!(get_backup(&r.env, second).status.phase, BackupPhase::Ready);
    for (file, data) in files() {
        assert_eq!(
            std::fs::read(replica_dir(&r.env, second).join(file)).unwrap(),
            data
        );
    }
}

/// 보낸 파일의 해시는 원본 디스크에 적는다. 이미 ready인 사본은 원본에 적힌 해시와 사본에 적힌 해시를
/// 비교할 뿐 파일을 다시 읽지 않는다(원본 파일이 없어도 확인한다). 해시가 적히지 않은 옛 백업은 파일을
/// 읽어 계산하고 적는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ready_copy_is_checked_by_the_recorded_hashes() {
    use sha2::{Digest, Sha256};
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let hashes = |env: &Env, id: Uuid| -> Vec<Option<String>> {
        get_backup(env, id)
            .status
            .disks
            .iter()
            .map(|d| d.sha256.clone())
            .collect()
    };
    let want: Vec<Option<String>> = files()[..2]
        .iter()
        .map(|(_, data)| Some(hex::encode(Sha256::digest(data))))
        .collect();
    assert_eq!(hashes(&local, source.id), want);
    let copy = only_replica(&local, source.id).backup.unwrap();
    assert_eq!(hashes(&r.env, copy), want);

    // 해시가 없는 옛 백업: 파일을 읽어 계산하고 적는다.
    let mut b = get_backup(&local, source.id);
    for disk in &mut b.status.disks {
        disk.sha256 = None;
    }
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &b.status)
        .await
        .unwrap();
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(hashes(&local, source.id), want);

    // 적힌 해시가 있으면 원본 파일을 읽지 않는다.
    let dir = source_dir(&local, source.id);
    for (file, _) in &files()[..2] {
        std::fs::remove_file(dir.join(file)).unwrap();
    }
    let at = only_replica(&local, source.id).at;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    assert_eq!(
        (replica.state, replica.backup, replica.error),
        (ReplicaState::Ready, Some(copy), None)
    );
    assert!(replica.at > at);
}

/// ready 사본을 확인하는 동안 기록은 ready 그대로다. 원격에 닿지 못하면 Task는 실패하지만 기록은 ready로
/// 두고 이유와 시각만 적는다(다음 실행에 다시 확인한다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checking_a_ready_copy_keeps_it_ready() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let before = only_replica(&local, source.id);
    // 확인하는 동안 사본 기록이 바뀌면 남긴다.
    let mut watch = local
        .cluster
        .watch(local.cluster.list::<BackupSpec>().unwrap().0)
        .unwrap();
    drop(r.server);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("cannot reach the remote cluster"), "{error}");
    let replica = only_replica(&local, source.id);
    assert_eq!(
        (replica.state, replica.backup, replica.error.as_deref()),
        (ReplicaState::Ready, before.backup, Some(error))
    );
    assert!(replica.at > before.at);
    let mut written = 0;
    while let Ok(Ok(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(100), watch.next()).await
    {
        if let opl_cluster::watch::WatchEvent::Put { resource, .. } = event
            && resource.id == source.id
        {
            written += 1;
            assert_eq!(
                resource.status["replicas"][0]["state"], "ready",
                "{}",
                resource.status
            );
        }
    }
    assert!(written > 0);
}

/// 요청 검사: 백업 쓰기 권한(viewer 403), 없는 원격 404, 잘못된 풀 이름 400, 준비되지 않은 백업 409
/// `backup_not_ready`, 같은 원격으로 끝나지 않은 복제가 있으면 409 `backup_busy`(복제 중인 백업은
/// 지우지도 못한다), 옛 노드가 있으면 409 `cluster_upgrading`, 파일을 쓴 노드가 오프라인이면 409
/// `node_offline`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicate_requests_are_checked() {
    let r = remote().await;
    let (mut local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;

    let viewer = local.user("watcher", "viewer").await;
    assert_error(
        &replicate(&viewer, "web-1").await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_error(
        &admin
            .post(
                "/backups/web-1/actions/replicate",
                json!({ "remote": "nope", "pool": "dr" }),
            )
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_error(
        &admin
            .post(
                "/backups/web-1/actions/replicate",
                json!({ "remote": "dr", "pool": "Bad Pool" }),
            )
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    let mut failed = get_backup(&local, source.id);
    failed.status.phase = BackupPhase::Failed;
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &failed.status)
        .await
        .unwrap();
    assert_error(
        &replicate(&admin, "web-1").await,
        StatusCode::CONFLICT,
        "backup_not_ready",
    );
    failed.status.phase = BackupPhase::Ready;
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &failed.status)
        .await
        .unwrap();

    // Task가 돌지 않으면 앞의 복제가 끝나지 않은 채 남는다.
    local.stop_runner().await;
    let (status, task) = replicate(&admin, "web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_error(
        &replicate(&admin, "web-1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    assert_error(
        &admin.delete("/backups/web-1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    local.start_runner().await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");

    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.online = true;
    node.status.version = Some("0.0.1~old".into());
    local.cluster.create(&node).await.unwrap();
    assert_error(
        &replicate(&admin, "web-1").await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    node.status.version = Some(opl_types::version::BUILD.into());
    node.status.online = false;
    local
        .cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();

    // 파일을 쓴 노드가 오프라인이면 보낼 수 없다.
    let mut b = get_backup(&local, source.id);
    b.status.node = Some(node.id);
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &b.status)
        .await
        .unwrap();
    assert_error(
        &replicate(&admin, "web-1").await,
        StatusCode::CONFLICT,
        "node_offline",
    );
}

/// `done`이 참이 될 때까지 기다린다.
async fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 지금부터 열두 시간 뒤의 `HH:MM`(UTC). 시험 도중 예약 실행이 시작되지 않게 한다.
fn far_at() -> String {
    let t = (now() + 12 * 3600) % 86400;
    format!("{:02}:{:02}", t / 3600, t / 60 % 60)
}

/// 원격 `dr`의 풀 `dr`로 복제하는 켜 둔 작업 `nightly`(예약 실행은 열두 시간 뒤)의 id
async fn replicating_job(admin: &Client) -> Uuid {
    let (status, job) = admin
        .post(
            "/backup-jobs",
            json!({ "name": "nightly", "spec": {
                "vms": { "names": ["web"] }, "pool": "backups",
                "schedule": { "at": far_at() },
                "replicate": { "remote": "dr", "pool": "dr" },
            }}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    job["id"].as_str().unwrap().parse().unwrap()
}

/// 50ms마다 보는 스케줄러
fn scheduler(env: &Env) -> BackupJobScheduler {
    BackupJobScheduler::spawn(
        env.state.clone(),
        BackupJobSchedulerConfig {
            tick: Duration::from_millis(50),
            max_concurrent: 2,
        },
    )
}

/// 작업 `job`의 상태를 `change`로 바꿔 쓴다.
async fn change_job(
    env: &Env,
    job: Uuid,
    change: impl Fn(&mut opl_types::backup_job::BackupJobStatus),
) {
    let mut got = env.cluster.get::<BackupJobSpec>(job).unwrap().unwrap();
    change(&mut got.status);
    env.cluster
        .update_status::<BackupJobSpec>(job, &got.status)
        .await
        .unwrap();
}

fn replica_state(env: &Env, id: Uuid) -> Option<ReplicaState> {
    get_backup(env, id).status.replicas.first().map(|r| r.state)
}

fn exists(env: &Env, id: Uuid) -> bool {
    env.cluster.get::<BackupSpec>(id).unwrap().is_some()
}

/// 복제를 켠 작업은 스케줄러가 그 작업의 ready 백업을 원격으로 보낸다(손으로 만든 백업은 보내지 않는다).
/// 보존 규칙이 원본을 지우면 원격 사본도 지운다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_replicates_its_backups_and_pruning_deletes_the_copies() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let job = replicating_job(&admin).await;
    let old = ready_backup(&local, "web-old", Some(job)).await;
    let new = ready_backup(&local, "web-new", Some(job)).await;
    // 같은 VM의 백업이고, 오래된 백업은 하루 앞에 만들었다.
    let spec = BackupSpec {
        vm: old.spec.vm,
        ..new.spec.clone()
    };
    local
        .cluster
        .update_spec(new.id, None, new.labels.clone(), &spec)
        .await
        .unwrap();
    let mut earlier = old.status.clone();
    earlier.created_at = earlier.created_at.map(|t| t - 86400);
    local
        .cluster
        .update_status::<BackupSpec>(old.id, &earlier)
        .await
        .unwrap();
    let manual = ready_backup(&local, "web-manual", None).await;
    let sched = scheduler(&local);

    wait_until("both copies", || {
        [old.id, new.id]
            .iter()
            .all(|id| replica_state(&local, *id) == Some(ReplicaState::Ready))
    })
    .await;
    assert!(get_backup(&local, manual.id).status.replicas.is_empty());
    let copy = |id| only_replica(&local, id).backup.unwrap();
    let (old_copy, new_copy) = (copy(old.id), copy(new.id));
    assert_eq!(
        get_backup(&r.env, old_copy).status.phase,
        BackupPhase::Ready
    );
    let origin = get_backup(&r.env, new_copy).spec.origin.unwrap();
    assert_eq!(
        (origin.backup, origin.job.as_deref()),
        (new.id, Some("nightly"))
    );

    // 마지막 하나만 남기는 규칙으로 실행이 끝나면 오래된 원본과 그 사본이 지워진다.
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "keep": { "last": 1 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    change_job(&local, job, |s| {
        s.run = Some(JobRun {
            started_at: now(),
            queue: vec![],
            backups: vec![],
        })
    })
    .await;
    wait_until("the pruned backup and its copy", || {
        !exists(&local, old.id) && !exists(&r.env, old_copy)
    })
    .await;
    assert!(exists(&local, new.id));
    assert!(exists(&r.env, new_copy));
    sched.shutdown().await;
}

/// 실패한 사본은 마지막 실행을 시작하기 전에 실패한 것만 한 번 더 보낸다. 실행 전에 ready였던 사본도
/// 실행마다 한 번 원격에 있는지 확인하므로, 원격에서 지운 사본은 다시 보낸다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_retries_copies_that_failed_before_its_last_run() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let job = replicating_job(&admin).await;
    let started = now() - 100;
    change_job(&local, job, |s| {
        s.last_run = Some(JobRunResult {
            started_at: started,
            finished_at: started + 10,
            ok: 2,
            failed: 0,
        })
    })
    .await;
    let failed = |at: u64| opl_types::backup::BackupReplica {
        remote: "dr".into(),
        pool: "dr".into(),
        backup: None,
        state: ReplicaState::Failed,
        error: Some("down".into()),
        at,
    };
    let before = ready_backup(&local, "web-before", Some(job)).await;
    let since = ready_backup(&local, "web-since", Some(job)).await;
    for (id, at) in [(before.id, started - 1), (since.id, started + 1)] {
        let mut b = get_backup(&local, id);
        b.status.replicas = vec![failed(at)];
        local
            .cluster
            .update_status::<BackupSpec>(id, &b.status)
            .await
            .unwrap();
    }
    let sched = scheduler(&local);

    wait_until("the retried copy", || {
        replica_state(&local, before.id) == Some(ReplicaState::Ready)
    })
    .await;
    // 나중에 생긴 백업이 보내질 때까지 tick이 돌아도(실행 뒤에 실패한 사본이 다시 보낼 것이었다면 만든
    // 순서상 먼저 보냈다) 실행 뒤에 실패한 사본은 그대로다.
    let later = ready_backup(&local, "web-later", Some(job)).await;
    wait_until("the later copy", || {
        replica_state(&local, later.id) == Some(ReplicaState::Ready)
    })
    .await;
    let replica = only_replica(&local, since.id);
    assert_eq!(
        (replica.state, replica.at),
        (ReplicaState::Failed, started + 1)
    );

    // 원격에서 지운 사본은 다음 실행에 다시 보낸다.
    let gone = only_replica(&local, before.id).backup.unwrap();
    let remote_admin = r.env.admin().await;
    let (status, task) = remote_admin.delete(&format!("/backups/{gone}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    remote_admin.wait_task(&task).await;
    let next = now() + 1;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    change_job(&local, job, |s| {
        s.last_run = Some(JobRunResult {
            started_at: next,
            finished_at: next,
            ok: 3,
            failed: 0,
        })
    })
    .await;
    wait_until("the copy sent again", || {
        let replica = only_replica(&local, before.id);
        replica.state == ReplicaState::Ready && replica.backup != Some(gone)
    })
    .await;
    let again = only_replica(&local, before.id).backup.unwrap();
    assert_eq!(get_backup(&r.env, again).status.phase, BackupPhase::Ready);
    sched.shutdown().await;
}

/// 보존 규칙이 지울 백업이 복제 Task(ready 사본 확인) 중이라 실행이 끝날 때 지우지 못하면(`backup_busy`)
/// tick마다 다시 해서 Task가 끝나면 지운다. 지울 백업에는 확인 Task를 더 만들지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pruning_waits_for_a_running_check() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    // 켜 두면 사본 읽기(GET /backups/{id})를 2초 늦추고 그 요청이 왔음을 알리는 원격
    let slow = Arc::new(AtomicBool::new(false));
    let reached = Arc::new(AtomicBool::new(false));
    let (s, got) = (slow.clone(), reached.clone());
    let r = remote_with(move |app| {
        app.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let (slow, reached) = (s.clone(), got.clone());
                async move {
                    if slow.load(Ordering::SeqCst)
                        && req.method() == Method::GET
                        && req.uri().path().starts_with("/api/v1/backups/")
                    {
                        reached.store(true, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    next.run(req).await
                }
            },
        ))
    })
    .await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let job = replicating_job(&admin).await;
    let old = ready_backup(&local, "web-old", Some(job)).await;
    let new = ready_backup(&local, "web-new", Some(job)).await;
    let spec = BackupSpec {
        vm: old.spec.vm,
        ..new.spec.clone()
    };
    local
        .cluster
        .update_spec(new.id, None, new.labels.clone(), &spec)
        .await
        .unwrap();
    for name in ["web-old", "web-new"] {
        let task = replicate_done(&admin, name).await;
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
    }
    let old_copy = only_replica(&local, old.id).backup.unwrap();

    // 오래된 백업의 사본을 확인하는 Task가 원격의 답을 기다리는 동안
    slow.store(true, Ordering::SeqCst);
    let (status, check) = replicate(&admin, "web-old").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{check}");
    wait_until("the check", || reached.load(Ordering::SeqCst)).await;
    // 마지막 하나만 남기는 규칙으로 실행이 끝난다.
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "keep": { "last": 1 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sched = scheduler(&local);
    change_job(&local, job, |s| {
        s.run = Some(JobRun {
            started_at: now(),
            queue: vec![],
            backups: vec![],
        })
    })
    .await;
    wait_until("the end of the run", || {
        let got = local.cluster.get::<BackupJobSpec>(job).unwrap().unwrap();
        got.status.run.is_none()
    })
    .await;
    let (_, task) = admin
        .get(&format!("/tasks/{}", check["id"].as_str().unwrap()))
        .await;
    assert_eq!(task["status"]["state"], "running", "{task}");
    assert!(exists(&local, old.id));

    wait_until("the pruned backup and its copy", || {
        !exists(&local, old.id) && !exists(&r.env, old_copy)
    })
    .await;
    let task = admin.wait_task(&check).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(exists(&local, new.id));
    // 지운 백업의 복제 Task는 손으로 요청한 둘(복제와 확인)뿐이다.
    let (_, tasks) = local.cluster.list::<opl_types::task::TaskSpec>().unwrap();
    let checks = tasks
        .iter()
        .filter(|t| {
            t.spec.target == old.id
                && matches!(t.spec.op, opl_types::task::TaskOp::BackupReplicate { .. })
        })
        .count();
    assert_eq!(checks, 2);
    sched.shutdown().await;
}

/// 원본을 지우면 로컬 파일을 먼저 지우고 원격 사본을 지운다. 원격에서 이미 지운 사본(404)은 성공이다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_backup_deletes_its_remote_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let remote_admin = r.env.admin().await;
    for name in ["web-1", "web-2"] {
        let source = ready_backup(&local, name, None).await;
        let task = replicate_done(&admin, name).await;
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
        let copy = only_replica(&local, source.id).backup.unwrap();
        if name == "web-2" {
            let (status, task) = remote_admin.delete(&format!("/backups/{copy}")).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{task}");
            remote_admin.wait_task(&task).await;
            assert!(!exists(&r.env, copy));
        }

        let (status, task) = admin.delete(&format!("/backups/{name}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        let task = admin.wait_task(&task).await;
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
        assert_eq!(task["status"]["error"], Value::Null, "{task}");
        assert!(!exists(&local, source.id));
        assert!(!source_dir(&local, source.id).exists());
        wait_until("the remote copy", || !exists(&r.env, copy)).await;
    }
}

/// 원격 사본을 지우지 못해도 원본은 지우고, Task는 성공하되 그 경고를 오류로 남긴다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_that_cannot_be_deleted_leaves_a_warning() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = only_replica(&local, source.id).backup.unwrap();
    drop(r.server);

    let (status, task) = admin.delete("/backups/web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("remote copies may be left"), "{error}");
    assert!(error.contains(&copy.to_string()), "{error}");
    assert!(!exists(&local, source.id));
    assert!(exists(&r.env, copy));
}

/// 실패로 끝난 사본이라도 원격에 만든 사본(id)이 있으면 원본을 지울 때 지운다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_backup_deletes_a_failed_copy_too() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let mut b = get_backup(&local, source.id);
    b.status.replicas[0].state = ReplicaState::Failed;
    let copy = b.status.replicas[0].backup.unwrap();
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &b.status)
        .await
        .unwrap();

    let (status, task) = admin.delete("/backups/web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["status"]["error"], Value::Null, "{task}");
    wait_until("the remote copy", || !exists(&r.env, copy)).await;
}

/// 원본 파일을 지우지 못하면 원격 사본은 건드리지 않는다(원본이 남으면 사본도 남는다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_copies_are_deleted_after_the_local_files() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = only_replica(&local, source.id).backup.unwrap();
    // 디렉터리 자리에 파일이 있으면 지우지 못한다.
    let dir = source_dir(&local, source.id);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, b"not a directory").unwrap();

    let (status, task) = admin.delete("/backups/web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(exists(&local, source.id));
    // 원격에 지우기를 요청하지 않았다(지우기 Task가 없다).
    let (_, tasks) = r.env.cluster.list::<opl_types::task::TaskSpec>().unwrap();
    assert!(!tasks.iter().any(|t| t.spec.target == copy), "{tasks:?}");
    assert_eq!(get_backup(&r.env, copy).status.phase, BackupPhase::Ready);
}

/// `force`로 리소스만 지우면 원격 사본은 건드리지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_deleting_leaves_the_remote_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = only_replica(&local, source.id).backup.unwrap();
    // 파일을 쓴 노드가 없어져 파일을 지울 수 없다.
    let mut b = get_backup(&local, source.id);
    b.status.node = Some(Uuid::now_v7());
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &b.status)
        .await
        .unwrap();

    let (status, body) = admin.delete("/backups/web-1?force=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!exists(&local, source.id));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(get_backup(&r.env, copy).status.phase, BackupPhase::Ready);
}
