//! 업로드 사전 확인(`POST /volumes/import/check`)과 업로드가 같은 확인을 쓰는지.
//! 확인에서 거절되면 Task를 만들지 않으므로 qemu-img 없이 돈다.

mod common;

use common::{Client, Env};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use reqwest::StatusCode;
use serde_json::Value;
use uuid::Uuid;

/// 풀(노드 `nodes`)을 저장소에 직접 만든다.
async fn pool(env: &Env, name: &str, nodes: Vec<Uuid>) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes,
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 풀 `local`에 볼륨을 저장소에 직접 만든다.
async fn volume(env: &Env, name: &str) -> Uuid {
    let volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.id
}

async fn check(client: &Client, name: &str, pool: &str, size: u64) -> (StatusCode, Value) {
    client
        .post(
            &format!(
                "/volumes/import/check?name={name}&pool={pool}&source_format=vmdk&size={size}"
            ),
            Value::Null,
        )
        .await
}

#[tokio::test]
async fn check_rejects_with_the_upload_codes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "remote", vec![Uuid::now_v7()]).await;
    volume(&env, "taken").await;

    let (status, body) = check(&admin, "disk", "local", 1 << 20).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    // 확인만 하고 볼륨은 만들지 않는다.
    assert_eq!(admin.get("/volumes/disk").await.0, StatusCode::NOT_FOUND);

    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = check(&viewer, "disk", "local", 1 << 20).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    for (name, pool, size, want, code) in [
        (
            "Bad_Name",
            "local",
            1 << 20,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "import",
            "local",
            1 << 20,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "disk",
            "missing",
            1 << 20,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "disk",
            "remote",
            1 << 20,
            StatusCode::CONFLICT,
            "pool_not_on_node",
        ),
        (
            "taken",
            "local",
            1 << 20,
            StatusCode::CONFLICT,
            "name_taken",
        ),
        // 이름 중복은 공간보다 먼저 본다.
        (
            "taken",
            "local",
            u64::MAX,
            StatusCode::CONFLICT,
            "name_taken",
        ),
        (
            "disk",
            "local",
            u64::MAX,
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
        ),
    ] {
        let (status, body) = check(&admin, name, pool, size).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (want, Some(code)),
            "{name} {pool} {body}"
        );
    }

    // 크기가 없으면 확인할 수 없다.
    let (status, body) = admin
        .post(
            "/volumes/import/check?name=disk&pool=local&source_format=vmdk",
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn upload_with_a_taken_name_is_rejected_before_creating_a_volume() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let id = volume(&env, "taken").await;

    let (status, body) = admin
        .upload(
            "/volumes/import?name=taken&pool=local&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
    // 저장소에 만들기 전에 거절하므로 받은 파일도 없다.
    assert_eq!(body["message"], "volume \"taken\" already exists");
    let (_, volumes) = admin.get("/volumes").await;
    let volumes = volumes["items"].as_array().unwrap();
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0]["id"], id.to_string());
    let uploads = env.dir.path().join("uploads");
    assert!(!uploads.exists() || std::fs::read_dir(&uploads).unwrap().count() == 0);
}

#[tokio::test]
async fn upload_without_content_length_is_rejected_before_other_checks() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 크기를 모르면 확인할 수 없으므로 없는 풀이나 잘못된 이름보다 411이 먼저다.
    for query in ["name=x&pool=missing", "name=Bad&pool=missing"] {
        let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>(vec![0u8; 512])]);
        let r = admin
            .http
            .post(format!(
                "{}/api/v1/volumes/import?{query}&source_format=raw",
                admin.base
            ))
            .bearer_auth(admin.token.as_ref().unwrap())
            .body(reqwest::Body::wrap_stream(chunks));
        let (status, body) = admin.send(r).await;
        assert_eq!(status, StatusCode::LENGTH_REQUIRED, "{query}: {body}");
        assert_eq!(body["code"], "length_required");
    }
}
