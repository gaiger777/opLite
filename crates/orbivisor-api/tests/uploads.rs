//! 조각 업로드(계획 77-8, `/uploads`): 순서·중복·끊긴 뒤 잇기·크기·공간·권한, raw를 풀에 바로 쓰기, 변환하는 원본.
//! 실행기는 파일 없는 스토리지(`FakeStorage`)라 qemu-img 없이 돈다.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{Client, Env, FakeStorage};
use orbivisor_types::resource::Resource;
use orbivisor_types::runtime::ImageFormat;
use orbivisor_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 디렉터리 풀 `local`(디렉터리도 만든다)
async fn pool(env: &Env) -> std::path::PathBuf {
    let dir = env.dir.path().join("local");
    std::fs::create_dir_all(&dir).unwrap();
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir { path: dir.clone() },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    dir
}

async fn fake(env: &mut Env) -> Arc<FakeStorage> {
    let storage = Arc::new(FakeStorage::default());
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    storage
}

async fn open(client: &Client, body: Value) -> (StatusCode, Value) {
    client.post("/uploads", body).await
}

/// 조각 하나를 `offset`에 보낸다.
async fn put(client: &Client, id: &str, offset: u64, bytes: &[u8]) -> (StatusCode, Value) {
    let r = client
        .http
        .put(format!(
            "{}/api/v1/uploads/{id}?offset={offset}",
            client.base
        ))
        .bearer_auth(client.token.as_ref().unwrap())
        .header("content-type", "application/octet-stream")
        .body(bytes.to_vec());
    client.send(r).await
}

fn sessions(env: &Env) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(env.dir.path().join("uploads/sessions"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn chunks_go_in_order_resume_and_complete_into_a_converted_volume() {
    let mut env = Env::start().await;
    let storage = fake(&mut env).await;
    let admin = env.admin().await;
    pool(&env).await;

    let (status, up) = open(
        &admin,
        json!({ "name": "disk", "pool": "local", "source_format": "vmdk", "size": 10 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{up}");
    assert_eq!(up["offset"], 0);
    assert_eq!(up["format"], "qcow2");
    assert_eq!(up["direct"], false);
    assert!(up["chunk_size"].as_u64().unwrap() >= 1 << 20);
    let id = up["id"].as_str().unwrap().to_string();
    // 같은 이름은 하나만 연다.
    let (status, body) = open(
        &admin,
        json!({ "name": "disk", "pool": "local", "source_format": "vmdk", "size": 10 }),
    )
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("name_taken"))
    );

    let (status, body) = put(&admin, &id, 0, b"vmdk").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["offset"], 4);
    // 같은 조각을 다시 보내거나(응답을 잃음) 앞질러 보내면 받은 데를 알린다.
    for offset in [0, 8] {
        let (status, body) = put(&admin, &id, offset, b"vmdk").await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::CONFLICT, Some("upload_offset_mismatch")),
            "{body}"
        );
        assert_eq!(body["details"]["offset"], 4);
    }
    // 크기를 넘는 조각은 받지 않는다.
    let (status, _) = put(&admin, &id, 4, b"1234567").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // 덜 받으면 끝내지 못한다.
    let (status, body) = admin
        .post(&format!("/uploads/{id}/complete"), Value::Null)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("upload_incomplete"))
    );
    // 탭을 닫았다 와도 목록·GET으로 받은 데를 알고 잇는다.
    let (_, list) = admin.get("/uploads").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["offset"], 4);
    let (_, got) = admin.get(&format!("/uploads/{id}")).await;
    assert_eq!(got["offset"], 4);
    let (status, body) = put(&admin, &id, 4, b"-image").await;
    assert_eq!(
        (status, body["offset"].as_u64()),
        (StatusCode::OK, Some(10))
    );

    let (status, task) = admin
        .post(&format!("/uploads/{id}/complete"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "volume-import");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/disk").await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let volume = volume["id"].as_str().unwrap().parse().unwrap();
    // 변환했다(qcow2 볼륨), 받은 파일과 기록은 지웠다.
    assert_eq!(storage.import_formats(), [(volume, ImageFormat::Qcow2)]);
    assert!(sessions(&env).is_empty(), "{:?}", sessions(&env));
    assert_eq!(
        admin.get(&format!("/uploads/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
}

/// 조각이 중간에 끊기면 받은 데까지 남고 거기서 잇는다.
#[tokio::test]
async fn an_interrupted_chunk_keeps_what_arrived() {
    let mut env = Env::start().await;
    fake(&mut env).await;
    let admin = env.admin().await;
    pool(&env).await;
    let (_, up) = open(
        &admin,
        json!({ "name": "disk", "pool": "local", "source_format": "qcow2", "size": 8 }),
    )
    .await;
    let id = up["id"].as_str().unwrap();
    // 3바이트를 보내고 잠시 뒤 끊는다(보낸 것이 서버에 닿게).
    let chunks = futures_util::stream::unfold(0, |step| async move {
        match step {
            0 => Some((Ok::<_, std::io::Error>(b"abc".to_vec()), 1)),
            1 => {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                Some((Err(std::io::Error::other("cable pulled")), 2))
            }
            _ => None,
        }
    });
    let r = admin
        .http
        .put(format!("{}/api/v1/uploads/{id}?offset=0", admin.base))
        .bearer_auth(admin.token.as_ref().unwrap())
        .header("content-length", "8")
        .body(reqwest::Body::wrap_stream(chunks));
    assert!(r.send().await.is_err());
    // 서버가 끊김을 알아챌 때까지
    let mut offset = 0;
    for _ in 0..100 {
        let (_, got) = admin.get(&format!("/uploads/{id}")).await;
        offset = got["offset"].as_u64().unwrap();
        if offset == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(offset, 3);
    // 끊긴 요청을 서버가 아직 정리하는 중이면 `upload_busy` — 잠시 뒤 다시 보낸다(웹도 같다).
    let mut got = put(&admin, id, 3, b"defgh").await;
    for _ in 0..100 {
        if got.1["code"] != "upload_busy" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        got = put(&admin, id, 3, b"defgh").await;
    }
    assert_eq!(
        (got.0, got.1["offset"].as_u64()),
        (StatusCode::OK, Some(8)),
        "{}",
        got.1
    );
}

/// raw 원본을 raw 볼륨으로(ISO도) 디렉터리 풀에 올리면 풀 안에 바로 쓰고, 끝내면 그 파일이 볼륨이다(변환 없음).
#[tokio::test]
async fn raw_uploads_write_straight_into_the_pool() {
    let mut env = Env::start().await;
    let storage = fake(&mut env).await;
    let admin = env.admin().await;
    let dir = pool(&env).await;
    let (status, up) = open(
        &admin,
        json!({
            "name": "plain", "pool": "local", "format": "raw", "source_format": "raw", "size": 6
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{up}");
    assert_eq!(up["direct"], true);
    let id = up["id"].as_str().unwrap();
    assert_eq!(put(&admin, id, 0, b"rawraw").await.0, StatusCode::OK);
    // 받는 중에는 풀 안 임시 이름이다(업로드 디렉터리에는 기록만).
    assert_eq!(files(&dir), [format!(".upload-{id}.raw.part")]);
    assert_eq!(sessions(&env), [format!("{id}.json")]);

    let (status, task) = admin
        .post(&format!("/uploads/{id}/complete"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/plain").await;
    assert_eq!(volume["status"]["phase"], "ready");
    let file = dir.join(format!("{}.raw", volume["id"].as_str().unwrap()));
    assert_eq!(std::fs::read(&file).unwrap(), b"rawraw");
    assert_eq!(files(&dir), [file.file_name().unwrap().to_str().unwrap()]);
    // 바꾸지 않았다(가져오기를 부르지 않음).
    assert!(storage.import_formats().is_empty());
    assert!(sessions(&env).is_empty());

    // ISO도 같다(내용은 실행기가 ISO인지 본다).
    let iso = orbivisor_qemu::iso9660::build("TEST", &[("readme.txt", b"install me")]).unwrap();
    let (status, up) = open(
        &admin,
        json!({
            "name": "media", "pool": "local", "source_format": "raw", "content": "iso",
            "size": iso.len()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{up}");
    assert_eq!(up["direct"], true);
    let id = up["id"].as_str().unwrap();
    assert_eq!(put(&admin, id, 0, &iso).await.0, StatusCode::OK);
    let (_, task) = admin
        .post(&format!("/uploads/{id}/complete"), Value::Null)
        .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // ISO가 아닌 내용은 실행기가 거절하고 볼륨·파일을 지운다.
    let (_, up) = open(
        &admin,
        json!({
            "name": "fake-iso", "pool": "local", "source_format": "raw", "content": "iso",
            "size": 4
        }),
    )
    .await;
    let id = up["id"].as_str().unwrap();
    put(&admin, id, 0, b"junk").await;
    let (_, task) = admin
        .post(&format!("/uploads/{id}/complete"), Value::Null)
        .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["failure"]["code"], "image_unsupported");
    assert_eq!(
        admin.get("/volumes/fake-iso").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn opening_checks_like_the_one_shot_upload() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env).await;
    let taken = Resource::new(
        "taken",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    env.cluster.create(&taken).await.unwrap();
    for (body, want, code) in [
        (
            json!({ "name": "taken", "pool": "local", "source_format": "raw", "size": 1 }),
            StatusCode::CONFLICT,
            "name_taken",
        ),
        (
            json!({ "name": "Bad_Name", "pool": "local", "source_format": "raw", "size": 1 }),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            json!({ "name": "disk", "pool": "missing", "source_format": "raw", "size": 1 }),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        // 공간 부족(업로드 디렉터리)
        (
            json!({ "name": "disk", "pool": "local", "source_format": "raw", "size": u64::MAX }),
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
        ),
        // 공간 부족(바로 쓰는 풀)
        (
            json!({
                "name": "disk", "pool": "local", "format": "raw", "source_format": "raw",
                "size": u64::MAX
            }),
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
        ),
    ] {
        let (status, got) = open(&admin, body.clone()).await;
        assert_eq!(
            (status, got["code"].as_str()),
            (want, Some(code)),
            "{body} {got}"
        );
    }
    let viewer = env.user("viewer", "viewer").await;
    let (status, _) = open(
        &viewer,
        json!({ "name": "disk", "pool": "local", "source_format": "raw", "size": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(sessions(&env).is_empty());

    // 남의 업로드는 보이지 않는다. 버리면 받은 것도 지운다.
    let (_, up) = open(
        &admin,
        json!({ "name": "disk", "pool": "local", "source_format": "raw", "size": 4 }),
    )
    .await;
    let id = up["id"].as_str().unwrap();
    put(&admin, id, 0, b"ab").await;
    let other = env.user("other", "admin").await;
    assert_eq!(
        other.get(&format!("/uploads/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(put(&other, id, 2, b"cd").await.0, StatusCode::NOT_FOUND);
    let (_, list) = other.get("/uploads").await;
    assert_eq!(list["items"], json!([]));
    assert_eq!(
        admin.delete(&format!("/uploads/{id}")).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(sessions(&env).is_empty());
    assert_eq!(
        admin.get(&format!("/uploads/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
}

/// 실행 파일 셸 스크립트를 자식 셸이 쓰게 한다(이 프로세스가 연 채 실행하면 "Text file busy").
fn script(path: &Path, text: &str) {
    use std::io::Write;
    let mut sh = std::process::Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    sh.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    assert!(sh.wait().unwrap().success());
}

/// qcow2 원본은 업로드 디렉터리에 받은 뒤 변환하고, 변환 진행(`qemu-img -p`)을 Task 진행 `converting`으로 알린다.
#[tokio::test]
async fn conversion_progress_shows_on_the_task() {
    let mut env = Env::start().await;
    let tool = env.dir.path().join("fake-qemu-img");
    script(
        &tool,
        "#!/bin/sh\nfor last; do :; done\n\
         if [ \"$1\" = info ]; then echo '{\"virtual-size\": 10, \"actual-size\": 10}'; exit 0; fi\n\
         if [ \"$1\" = convert ]; then printf '    (30.00/100%%)\\r'; sleep 3; printf '    (100.00/100%%)\\r'; : > \"$last\"; exit 0; fi\n\
         exit 1\n",
    );
    env.stop_runner().await;
    env.start_runner_with(Arc::new(orbivisor_driver::DirStorage::new(
        &tool,
        "/nonexistent/opl-pools",
    )))
    .await;
    let admin = env.admin().await;
    pool(&env).await;
    let mut image = b"QFI\xfb".to_vec();
    image.resize(100, 0);
    let (_, up) = open(
        &admin,
        json!({ "name": "disk", "pool": "local", "source_format": "qcow2", "size": 100 }),
    )
    .await;
    let id = up["id"].as_str().unwrap();
    assert_eq!(put(&admin, id, 0, &image).await.0, StatusCode::OK);
    let (status, task) = admin
        .post(&format!("/uploads/{id}/complete"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task_id = task["id"].as_str().unwrap();
    let mut seen = None;
    for _ in 0..300 {
        let (_, t) = admin.get(&format!("/tasks/{task_id}")).await;
        let p = &t["status"]["progress"];
        if p["stage"] == "converting" && p["done_bytes"].as_u64().unwrap_or(0) > 0 {
            seen = Some(p.clone());
            break;
        }
        assert_ne!(t["status"]["state"], "failed", "{t}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        seen,
        Some(json!({ "stage": "converting", "done_bytes": 30, "total_bytes": 100 }))
    );
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(task["status"].get("progress").is_none(), "{task}");
}
