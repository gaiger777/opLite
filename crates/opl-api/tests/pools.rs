//! 스토리지 풀 API: 백엔드별 값 검사.

mod common;

use common::Env;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn pool(name: &str, backend: Value) -> Value {
    json!({ "name": name, "spec": { "backend": backend } })
}

#[tokio::test]
async fn rejects_bad_backend_values() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let nfs =
        |server: &str, export: &str| json!({ "kind": "nfs", "server": server, "export": export });
    for (bad, why) in [
        (
            pool("rel", json!({ "kind": "dir", "path": "pool" })),
            "relative path",
        ),
        (pool("srv", nfs("nas:2049", "/srv/vm")), "server with ':'"),
        (pool("exp", nfs("nas", "srv/vm")), "relative export"),
        (
            pool(
                "opt",
                json!({ "kind": "nfs", "server": "nas", "export": "/srv/vm", "options": "vers=4.2, hard" }),
            ),
            "options with space",
        ),
    ] {
        let (status, body) = admin.post("/storage-pools", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request", "{why}");
    }
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool("dir", json!({ "kind": "dir", "path": env.dir.path() })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn nfs_pools_are_not_supported_yet() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool(
                "nas",
                json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("nfs"), "{body}");
    assert_eq!(
        admin.get("/storage-pools/nas").await.0,
        StatusCode::NOT_FOUND
    );
}
