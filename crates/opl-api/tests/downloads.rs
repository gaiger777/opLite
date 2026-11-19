//! URL에서 이미지를 받아 볼륨을 만든다(`POST /volumes/download`, 계획 57). 127.0.0.1의 시험 HTTP 서버에서 받고
//! qemu-img로 가져온다. 계획 58: 체크섬 파일(`checksum` 단계)·SHA-512, 받은 볼륨으로 템플릿 VM 만들기.
//! `cargo test -p opl-api --features storage-it --test downloads`
#![cfg(feature = "storage-it")]

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::CONTENT_LENGTH;
use axum::routing::get;
use common::{Client, Env, WAIT};
use opl_types::audit::AuditQuery;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{SourceFormat, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::template::TemplateDraft;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};
use tokio::sync::{Notify, mpsc};

const CHUNK: usize = 64 * 1024;

/// 시험 HTTP 서버가 내놓는 것
struct Files {
    iso: Vec<u8>,
    qcow2: Vec<u8>,
    /// `/slow.iso`가 둘째·셋째 조각 뒤에 기다리는 문
    gates: [Notify; 2],
}

/// 시험 ISO(ISO 9660 + Joliet). 조각 넷으로 나눠 보낼 만큼 크다.
fn iso_image() -> Vec<u8> {
    let filler = vec![b'x'; 3 * CHUNK];
    opl_qemu::iso9660::build("TEST", &[("filler.bin", &filler)]).unwrap()
}

fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn sha512(data: &[u8]) -> String {
    hex::encode(Sha512::digest(data))
}

/// 본문을 `chunks` 채널에서 받는 스트림 응답
fn streamed(rx: mpsc::Receiver<Bytes>) -> Body {
    Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|c| (Ok::<_, std::io::Error>(c), rx))
    }))
}

async fn slow_iso(
    State(files): State<Arc<Files>>,
) -> ([(axum::http::HeaderName, String); 1], Body) {
    let (tx, rx) = mpsc::channel(4);
    let len = files.iso.len();
    let sender = files.clone();
    tokio::spawn(async move {
        let iso = &sender.iso;
        let cuts = [CHUNK, 2 * CHUNK, 3 * CHUNK, len];
        let mut from = 0;
        for (i, to) in cuts.into_iter().enumerate() {
            // 진행률은 1초가 지난 뒤 오는 조각에서 쓴다.
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(1100)).await;
            }
            if tx
                .send(Bytes::copy_from_slice(&iso[from..to]))
                .await
                .is_err()
            {
                return;
            }
            from = to;
            if i == 1 || i == 2 {
                sender.gates[i - 1].notified().await;
            }
        }
    });
    ([(CONTENT_LENGTH, len.to_string())], streamed(rx))
}

async fn chunked_iso(State(files): State<Arc<Files>>) -> Body {
    let (tx, rx) = mpsc::channel(8);
    for chunk in files.iso.chunks(CHUNK) {
        tx.send(Bytes::copy_from_slice(chunk)).await.unwrap();
    }
    streamed(rx)
}

/// 127.0.0.1의 시험 서버를 띄우고 `http://127.0.0.1:{port}`를 돌려준다.
async fn serve(files: Arc<Files>) -> String {
    let app = Router::new()
        .route(
            "/a.iso",
            get(|State(f): State<Arc<Files>>| async move { f.iso.clone() }),
        )
        .route(
            "/disk.qcow2",
            get(|State(f): State<Arc<Files>>| async move { f.qcow2.clone() }),
        )
        .route("/chunked.iso", get(chunked_iso))
        // 체크섬 파일(계획 58): GNU SHA-256, BSD SHA-512, 틀린 해시, disk.qcow2 줄이 없는 것, 1 MiB 넘는 것
        .route(
            "/SHA256SUMS",
            get(|State(f): State<Arc<Files>>| async move {
                format!(
                    "{}  a.iso\n{} *disk.qcow2\n",
                    sha256(&f.iso),
                    sha256(&f.qcow2)
                )
            }),
        )
        .route(
            "/SHA512SUMS",
            get(|State(f): State<Arc<Files>>| async move {
                format!(
                    "# Fedora-like\nSHA512 (disk.qcow2) = {}\n",
                    sha512(&f.qcow2)
                )
            }),
        )
        .route(
            "/BADSUMS",
            get(|| async { format!("{}  disk.qcow2\n", "0".repeat(64)) }),
        )
        .route(
            "/OTHERSUMS",
            get(|| async { format!("{}  other.qcow2\n", "0".repeat(64)) }),
        )
        .route("/HUGESUMS", get(|| async { vec![b'#'; 2 << 20] }))
        .route("/slow.iso", get(slow_iso))
        .with_state(files);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn qcow2_image(dir: &Path) -> Vec<u8> {
    let raw = dir.join("disk.raw");
    std::fs::write(&raw, vec![7u8; 2 << 20]).unwrap();
    let qcow2 = dir.join("disk.qcow2");
    let out = Command::new("qemu-img")
        .args(["convert", "-q", "-f", "raw", "-O", "qcow2"])
        .arg(&raw)
        .arg(&qcow2)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    std::fs::read(&qcow2).unwrap()
}

/// dir 풀 `local`과 시험 서버를 띄운다.
async fn setup(env: &Env, admin: &Client) -> (Arc<Files>, String) {
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": pool } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
    let files = Arc::new(Files {
        iso: iso_image(),
        qcow2: qcow2_image(env.dir.path()),
        gates: [Notify::new(), Notify::new()],
    });
    let base = serve(files.clone()).await;
    (files, base)
}

async fn download(admin: &Client, body: Value) -> Value {
    let (status, task) = admin.post("/volumes/download", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    task
}

/// 실패한 받기는 볼륨 리소스·풀 파일·받은 파일을 남기지 않는다.
async fn assert_cleaned(env: &Env, admin: &Client, name: &str) {
    assert_eq!(
        admin.get(&format!("/volumes/{name}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        std::fs::read_dir(env.dir.path().join("pool"))
            .unwrap()
            .count(),
        0
    );
    let uploads = std::fs::read_dir(env.dir.path().join("uploads")).unwrap();
    assert_eq!(uploads.count(), 0);
}

/// 이 API의 감사 로그에서 `POST /volumes/download`가 202로 끝난 가장 새 항목의 설명
async fn audit_detail(env: &Env) -> Option<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        let entries = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: Some("POST /volumes/download".into()),
                limit: 100,
            })
            .await
            .unwrap();
        if let Some(e) = entries.into_iter().find(|e| e.status == 202) {
            return e.detail;
        }
        assert!(Instant::now() < deadline, "no audit entry");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn downloads_an_iso_and_checks_its_sha256() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (files, base) = setup(&env, &admin).await;
    let sum = sha256(&files.iso);
    // 사용자 정보·쿼리가 든 URL: 감사 설명과 Task 응답에서 가린다(저장은 그대로). 대문자 체크섬도 받는다.
    let url = base.replace("http://", "http://user:secret@") + "/a.iso?token=t0k";
    let task = download(
        &admin,
        json!({
            "name": "installer", "pool": "local", "url": url, "content": "iso",
            "sha256": sum.to_uppercase()
        }),
    )
    .await;
    assert_eq!(task["spec"]["op"]["kind"], "volume-download");
    assert_eq!(task["spec"]["op"]["sha256"], sum);
    assert_eq!(task["spec"]["op"]["source_format"], "raw");
    let host = base.trim_start_matches("http://");
    let shown = format!("http://***@{host}/a.iso?***");
    assert_eq!(task["spec"]["op"]["url"], shown);
    let id: uuid::Uuid = task["id"].as_str().unwrap().parse().unwrap();
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["op"]["url"], shown);
    let (_, tasks) = admin.get("/tasks").await;
    let text = tasks.to_string();
    assert!(!text.contains("secret") && !text.contains("t0k"), "{text}");
    let stored = env
        .cluster
        .get::<opl_types::task::TaskSpec>(id)
        .unwrap()
        .unwrap();
    assert!(
        matches!(&stored.spec.op, opl_types::task::TaskOp::VolumeDownload { url: u, .. } if *u == url)
    );

    let (_, volume) = admin.get("/volumes/installer").await;
    assert_eq!(volume["spec"]["content"], "iso", "{volume}");
    assert_eq!(volume["spec"]["format"], "raw");
    assert_eq!(volume["spec"]["size_bytes"], files.iso.len() as u64);
    assert_eq!(volume["status"]["phase"], "ready");
    let file = env
        .dir
        .path()
        .join("pool")
        .join(format!("{}.raw", volume["id"].as_str().unwrap()));
    assert_eq!(std::fs::read(file).unwrap(), files.iso);
    // 받은 파일은 가져온 뒤 지운다.
    let uploads = std::fs::read_dir(env.dir.path().join("uploads")).unwrap();
    assert_eq!(uploads.count(), 0);

    assert_eq!(
        audit_detail(&env).await.as_deref(),
        Some(
            format!(
                "volume download installer from http://***@{host}/a.iso?*** (sha256 {}…)",
                &sum[..4]
            )
            .as_str()
        )
    );

    // 같은 이름은 409다.
    let (status, body) = admin
        .post(
            "/volumes/download",
            json!({ "name": "installer", "pool": "local", "url": url, "content": "iso" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
}

#[tokio::test]
async fn shows_download_progress_and_takes_bodies_without_a_length() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (files, base) = setup(&env, &admin).await;
    let task = download(
        &admin,
        json!({ "name": "slow", "pool": "local", "url": format!("{base}/slow.iso"), "content": "iso" }),
    )
    .await;
    let id = task["id"].as_str().unwrap();
    let total = files.iso.len() as u64;
    // 서버는 둘째 조각 뒤, 셋째 조각 뒤에 시험을 기다린다. 그때마다 받은 양이 진행률에 있다.
    for (gate, done) in [(0, 2 * CHUNK as u64), (1, 3 * CHUNK as u64)] {
        let deadline = Instant::now() + WAIT;
        loop {
            let (_, task) = admin.get(&format!("/tasks/{id}")).await;
            let progress = &task["status"]["progress"];
            if progress["done_bytes"] == done {
                assert_eq!(progress["stage"], "downloading", "{task}");
                assert_eq!(progress["total_bytes"], total, "{task}");
                break;
            }
            assert_ne!(task["status"]["state"], "failed", "{task}");
            assert!(Instant::now() < deadline, "no progress {done}: {task}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        files.gates[gate].notify_one();
    }
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    // `Content-Length` 없는 응답(chunked)도 받는다.
    let task = download(
        &admin,
        json!({ "name": "chunked", "pool": "local", "url": format!("{base}/chunked.iso"), "content": "iso" }),
    )
    .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/chunked").await;
    assert_eq!(volume["spec"]["size_bytes"], total);
}

#[tokio::test]
async fn failed_downloads_leave_nothing_behind() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (files, base) = setup(&env, &admin).await;

    let task = download(
        &admin,
        json!({ "name": "gone", "pool": "local", "url": format!("{base}/missing.iso"), "content": "iso" }),
    )
    .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(
        task["status"]["error"],
        format!("GET {base}/missing.iso: HTTP 404 Not Found")
    );
    assert_cleaned(&env, &admin, "gone").await;

    let wrong = "0".repeat(64);
    let task = download(
        &admin,
        json!({
            "name": "bad", "pool": "local", "url": format!("{base}/a.iso"), "content": "iso",
            "sha256": wrong
        }),
    )
    .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(
        task["status"]["error"],
        format!(
            "checksum mismatch: expected {wrong}, got {}",
            sha256(&files.iso)
        )
    );
    assert_cleaned(&env, &admin, "bad").await;

    // ISO로 받은 것이 설치 매체가 아니면 가져오지 않는다.
    let task = download(
        &admin,
        json!({ "name": "notiso", "pool": "local", "url": format!("{base}/disk.qcow2"), "content": "iso" }),
    )
    .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["error"], "not an ISO 9660 or UDF image");
    assert_cleaned(&env, &admin, "notiso").await;
}

#[tokio::test]
async fn downloads_disk_images() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, base) = setup(&env, &admin).await;
    let task = download(
        &admin,
        json!({
            "name": "cloud", "pool": "local", "url": format!("{base}/disk.qcow2"),
            "source_format": "qcow2"
        }),
    )
    .await;
    assert!(task["spec"]["op"].get("sha256").is_none(), "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/cloud").await;
    assert!(volume["spec"].get("content").is_none(), "{volume}");
    assert_eq!(volume["spec"]["format"], "qcow2");
    assert_eq!(volume["spec"]["size_bytes"], 2u64 << 20);
}

#[tokio::test]
async fn download_requests_are_checked() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, base) = setup(&env, &admin).await;
    let url = format!("{base}/a.iso");
    for (body, message) in [
        (
            json!({ "name": "x", "pool": "local", "url": "ftp://127.0.0.1/a.iso" }),
            "url must be an http or https URL",
        ),
        (
            json!({ "name": "x", "pool": "local", "url": "not a url" }),
            "url must be an http or https URL",
        ),
        (
            json!({ "name": "x", "pool": "local", "url": url, "sha256": "abc" }),
            "sha256 must be 64 hex digits",
        ),
        (
            json!({ "name": "x", "pool": "local", "url": url, "sha256": "g".repeat(64) }),
            "sha256 must be 64 hex digits",
        ),
        (
            json!({ "name": "x", "pool": "local", "url": url, "content": "iso", "format": "qcow2" }),
            "ISO volumes are raw",
        ),
        (
            json!({ "name": "x", "pool": "local", "url": url, "content": "iso", "source_format": "vmdk" }),
            "download ISO images as raw",
        ),
        (
            json!({ "name": "x", "pool": "nowhere", "url": url }),
            "storage pool \"nowhere\" not found",
        ),
    ] {
        let (status, got) = admin.post("/volumes/download", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {got}");
        assert_eq!(got["message"], message, "{body}");
    }
    assert_eq!(admin.get("/volumes/x").await.0, StatusCode::NOT_FOUND);

    // 받기는 새 Task라 모든 노드가 알아야 한다.
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            host_resources: None,
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
            roles: None,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    let (status, body) = admin
        .post(
            "/volumes/download",
            json!({ "name": "x", "pool": "local", "url": url }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    // ISO 업로드도 같다. 디스크 업로드는 옛 노드가 읽는 모양이라 그대로다.
    let (status, body) = admin
        .upload(
            "/volumes/import?name=x&pool=local&source_format=raw&content=iso",
            iso_image(),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
}

/// 시작 때의 업로드 디렉터리 정리는 끝나지 않은 받기 Task의 파일을 남기고 Task 없는 파일을 지운다.
#[tokio::test]
async fn startup_sweep_keeps_files_of_unfinished_downloads() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let uploads = env.dir.path().join("uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    let volume = uuid::Uuid::now_v7();
    // 다른 노드의 Task라 이 실행기가 돌리지 않는다.
    let mut task: opl_types::task::Task = Resource::new(
        "",
        opl_types::task::TaskSpec {
            op: opl_types::task::TaskOp::VolumeDownload {
                url: "http://127.0.0.1:9/a.iso".into(),
                sha256: None,
                source_format: opl_types::storage::SourceFormat::Raw,
                sha512: None,
                checksum_url: None,
                template: None,
            },
            target: volume,
            node: uuid::Uuid::now_v7(),
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    let kept = uploads.join(format!("{volume}.download"));
    std::fs::write(&kept, b"x").unwrap();
    let stray = uploads.join(format!("{}.download", uuid::Uuid::now_v7()));
    std::fs::write(&stray, b"x").unwrap();

    env.start_runner().await;

    assert!(kept.exists());
    assert!(!stray.exists());
}

/// `[download] allow_private`가 꺼져 있으면(배포 기본) 루프백·사설 주소에서 받지 않는다: IP로 적은 주소도, 그런 주소로
/// 풀리는 이름도 Task가 실패하고 아무것도 남기지 않는다.
#[tokio::test]
async fn private_addresses_are_refused_unless_allowed() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (_files, base) = setup(&env, &admin).await;
    env.stop_runner().await;
    env.start_runner_config(
        Arc::new(opl_driver::DirStorage::new("qemu-img", common::MOUNT_ROOT)),
        |c| c.download_allow_private = false,
    )
    .await;

    let task = download(
        &admin,
        json!({ "name": "inside", "pool": "local", "url": format!("{base}/a.iso"), "content": "iso" }),
    )
    .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(
        task["status"]["error"],
        format!(
            "GET {base}/a.iso: address 127.0.0.1 is loopback, link-local or private; set [download] allow_private = true to download from it"
        )
    );
    assert_cleaned(&env, &admin, "inside").await;

    let by_name = base.replace("127.0.0.1", "localhost");
    let task = download(
        &admin,
        json!({ "name": "named", "pool": "local", "url": format!("{by_name}/a.iso"), "content": "iso" }),
    )
    .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.contains("is loopback, link-local or private"),
        "{error}"
    );
    assert_cleaned(&env, &admin, "named").await;
}

/// 저장소에 받기 볼륨(풀 `local`, qcow2 디스크)과 새 칸을 쓴 받기 Task를 바로 만들고 끝나기를 기다린다(이 칸을 받는 API는
/// 계획 58 S5가 연다).
async fn run_download(env: &Env, admin: &Client, name: &str, op: TaskOp) -> Value {
    let mut volume: opl_types::storage::Volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 0,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    volume.status.node = Some(env.node);
    env.cluster.create(&volume).await.unwrap();
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target: volume.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    admin.wait_task(&json!({ "id": task.id })).await
}

fn disk_download(
    url: String,
    sha512: Option<String>,
    checksum_url: Option<String>,
    template: Option<TemplateDraft>,
) -> TaskOp {
    TaskOp::VolumeDownload {
        url,
        sha256: None,
        source_format: SourceFormat::Qcow2,
        sha512,
        checksum_url,
        template: template.map(Box::new),
    }
}

/// 체크섬 파일에서 이미지 파일 이름의 줄을 찾아 그 알고리즘(SHA-256·SHA-512)으로 확인한다. 고정 SHA-512도 받는다.
#[tokio::test]
async fn checksum_files_and_sha512_check_downloads() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (files, base) = setup(&env, &admin).await;
    let url = format!("{base}/disk.qcow2");
    for (name, sha, sums) in [
        ("gnu", None, Some(format!("{base}/SHA256SUMS"))),
        ("bsd", None, Some(format!("{base}/SHA512SUMS"))),
        ("fixed", Some(sha512(&files.qcow2).to_uppercase()), None),
    ] {
        let task = run_download(
            &env,
            &admin,
            name,
            disk_download(url.clone(), sha, sums, None),
        )
        .await;
        assert_eq!(task["status"]["state"], "succeeded", "{name}: {task}");
        let (_, volume) = admin.get(&format!("/volumes/{name}")).await;
        assert_eq!(volume["status"]["phase"], "ready", "{volume}");
        assert_eq!(volume["spec"]["size_bytes"], 2u64 << 20);
    }
}

/// 체크섬 파일에 줄이 없거나 해시가 틀리거나 파일이 너무 크면 실패하고 아무것도 남기지 않는다.
#[tokio::test]
async fn bad_checksum_files_fail_downloads() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (files, base) = setup(&env, &admin).await;
    let url = format!("{base}/disk.qcow2");
    let zeros = "0".repeat(64);
    let got = sha256(&files.qcow2);
    for (name, sha, sums, error) in [
        (
            "noentry",
            None,
            Some(format!("{base}/OTHERSUMS")),
            format!("checksum file {base}/OTHERSUMS has no entry for disk.qcow2"),
        ),
        (
            "wrong",
            None,
            Some(format!("{base}/BADSUMS")),
            format!("checksum mismatch: expected {zeros} (from {base}/BADSUMS), got {got}"),
        ),
        (
            "huge",
            None,
            Some(format!("{base}/HUGESUMS")),
            format!("checksum file {base}/HUGESUMS is larger than 1024 KiB"),
        ),
        (
            "nosums",
            None,
            Some(format!("{base}/MISSINGSUMS")),
            format!("GET {base}/MISSINGSUMS: HTTP 404 Not Found"),
        ),
        (
            "wrong512",
            Some("0".repeat(128)),
            None,
            format!(
                "checksum mismatch: expected {}, got {}",
                "0".repeat(128),
                sha512(&files.qcow2)
            ),
        ),
    ] {
        let task = run_download(
            &env,
            &admin,
            name,
            disk_download(url.clone(), sha, sums, None),
        )
        .await;
        assert_eq!(task["status"]["state"], "failed", "{name}: {task}");
        assert_eq!(task["status"]["error"], error, "{name}");
        assert_cleaned(&env, &admin, name).await;
    }
}

fn draft(name: &str, size_bytes: Option<u64>) -> TemplateDraft {
    serde_json::from_value(json!({
        "name": name,
        "network": "user",
        "size_bytes": size_bytes,
        "cloud_init": { "user": "orbi" },
    }))
    .unwrap()
}

/// 템플릿 초안이 있으면 받은 볼륨을 늘리고 그것을 첫 부팅 디스크로 쓰는 꺼진 템플릿 VM을 만든다.
#[tokio::test]
async fn a_download_with_a_draft_makes_a_template() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, base) = setup(&env, &admin).await;
    let op = disk_download(
        format!("{base}/disk.qcow2"),
        None,
        Some(format!("{base}/SHA256SUMS")),
        Some(draft("ubuntu-tmpl", Some(8 << 20))),
    );
    let task = run_download(&env, &admin, "ubuntu-24-04", op).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/ubuntu-24-04").await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    assert_eq!(volume["spec"]["size_bytes"], 8u64 << 20);
    let (status, vm) = admin.get("/vms/ubuntu-tmpl").await;
    assert_eq!(status, StatusCode::OK, "{vm}");
    let spec = &vm["spec"];
    assert_eq!(spec["template"], true, "{vm}");
    assert_eq!(spec["power"], "stopped");
    assert_eq!(spec["cpu"]["cores"], 2);
    assert_eq!(spec["memory_mib"], 2048);
    assert_eq!(spec["disks"][0]["volume"], "ubuntu-24-04");
    assert_eq!(spec["disks"][0]["bus"], "virtio-blk");
    assert_eq!(spec["disks"][0]["boot_order"], 1);
    assert_eq!(spec["nics"][0]["network"], "user");
    assert!(spec["nics"][0]["mac"].is_string(), "{vm}");
    assert_eq!(spec["cloud_init"]["user"], "orbi");
}

/// 템플릿을 만들지 못하면(이름이 그새 쓰임) Task는 실패하지만 받은 볼륨은 남기고 손수 만드는 길을 알린다.
#[tokio::test]
async fn a_failed_template_keeps_the_downloaded_volume() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, base) = setup(&env, &admin).await;
    let (status, body) = admin.post("/vms", common::vm_request("taken")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let op = disk_download(
        format!("{base}/disk.qcow2"),
        None,
        None,
        Some(draft("taken", None)),
    );
    let task = run_download(&env, &admin, "kept", op).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("cannot create VM \"taken\""), "{error}");
    assert!(
        error.ends_with(
            "; volume \"kept\" is ready; create the template from it with vm create and template convert"
        ),
        "{error}"
    );
    let (_, volume) = admin.get("/volumes/kept").await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let file = env
        .dir
        .path()
        .join("pool")
        .join(format!("{}.qcow2", volume["id"].as_str().unwrap()));
    assert!(file.exists());
    let uploads = std::fs::read_dir(env.dir.path().join("uploads")).unwrap();
    assert_eq!(uploads.count(), 0);
    let (_, vm) = admin.get("/vms/taken").await;
    assert_ne!(vm["spec"]["template"], true, "{vm}");

    // 체크섬이 틀려 가져오기 전에 실패하면 초안이 있어도 볼륨을 지운다.
    let op = disk_download(
        format!("{base}/disk.qcow2"),
        None,
        Some(format!("{base}/BADSUMS")),
        Some(draft("other", None)),
    );
    let task = run_download(&env, &admin, "dropped", op).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(admin.get("/volumes/dropped").await.0, StatusCode::NOT_FOUND);
    assert_eq!(admin.get("/vms/other").await.0, StatusCode::NOT_FOUND);
}

/// 가져오기를 마친 뒤(볼륨 Ready) 재시작으로 끊긴 받기 → 템플릿은 볼륨을 남기고, 오류에 손수 템플릿을 만드는 길을 붙인다.
#[tokio::test]
async fn an_interrupted_download_to_a_template_tells_how_to_finish() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (_, base) = setup(&env, &admin).await;
    let mut volume = Resource::new(
        "kept",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = opl_types::storage::VolumePhase::Ready;
    volume.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();

    env.stop_runner().await;
    let op = disk_download(
        format!("{base}/disk.qcow2"),
        None,
        None,
        Some(draft("tmpl", None)),
    );
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target: volume.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    task.status.state = opl_types::task::TaskState::Running;
    env.cluster.create(&task).await.unwrap();
    env.start_runner().await;
    let task = admin.wait_task(&json!({ "id": task.id })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "interrupted: orbivisord restarted while the task was running; volume \"kept\" is ready; \
         create the template from it with vm create and template convert"
    );
    let (_, volume) = admin.get("/volumes/kept").await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
}
