//! 로그인, 세션, API 토큰, 역할 권한.

mod common;

use common::{ADMIN_PASSWORD, Env};
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn requests_without_credentials_are_rejected() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (status, body) = anon.get("/nodes").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");

    let mut forged = anon.clone();
    forged.token = Some(format!("opl_{}_deadbeef", uuid::Uuid::now_v7().simple()));
    assert_eq!(forged.get("/nodes").await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_checks_password_and_session_cookie_works() {
    let env = Env::start().await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("{}/api/v1{p}", env.base);
    for (user, password) in [("admin", "wrong-password"), ("nobody", ADMIN_PASSWORD)] {
        let resp = http
            .post(url("/auth/login"))
            .json(&json!({ "username": user, "password": password }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["code"], "invalid_credentials");
    }

    let resp = http
        .post(url("/auth/login"))
        .json(&json!({ "username": "admin", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    let cookie = set_cookie.split(';').next().unwrap().to_string();
    let who: serde_json::Value = http
        .get(url("/auth/whoami"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(who["name"], "admin");
    assert_eq!(who["role"], "admin");
    // 다른 출처의 페이지가 쿠키로 보낸 요청은 거부한다.
    let resp = http
        .get(url("/auth/whoami"))
        .header("cookie", &cookie)
        .header("origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let resp = http
        .post(url("/auth/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = http
        .get(url("/auth/whoami"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn roles_limit_what_users_can_do() {
    let env = Env::start().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;

    let pool = json!({
        "name": "local",
        "spec": { "backend": { "kind": "dir", "path": "/tmp/pool" } }
    });
    assert_eq!(viewer.get("/storage-pools").await.0, StatusCode::OK);
    let (status, body) = viewer.post("/storage-pools", pool.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(viewer.get("/users").await.0, StatusCode::FORBIDDEN);

    // 운영자는 VM과 볼륨만 다룬다(VM 테스트에서 확인). 풀과 사용자는 관리자 몫이다.
    assert_eq!(operator.get("/nodes").await.0, StatusCode::OK);
    assert_eq!(
        operator.post("/storage-pools", pool).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(operator.get("/users").await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn users_manage_their_own_tokens_and_deleted_users_lose_access() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let alice = env.user("alice", "viewer").await;

    // 사용자 목록과 토큰 목록에 해시가 드러나지 않는다.
    let (_, users) = admin.get("/users").await;
    assert!(!users.to_string().contains("argon2"), "{users}");
    let (_, mine) = alice.get("/tokens").await;
    let items = mine["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].get("secret_hash").is_none());

    // 관리자 토큰은 alice에게 보이지 않고 지울 수도 없다.
    let (_, all) = admin.get("/tokens").await;
    let admin_token = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"].as_str().unwrap().starts_with("admin-"))
        .unwrap()
        .clone();
    let (status, _) = alice
        .delete(&format!("/tokens/{}", admin_token["id"].as_str().unwrap()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 자기 비밀번호는 현재 비밀번호를 대야 바꿀 수 있고, 역할은 못 바꾼다.
    let (status, body) = alice
        .put("/users/alice", json!({ "password": "new-password" }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "invalid_credentials");
    let (status, _) = alice
        .put(
            "/users/alice",
            json!({ "password": "new-password", "current_password": "password-1" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = alice
        .put("/users/alice", json!({ "role": "admin" }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = admin.delete("/users/admin").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // 마지막 관리자는 역할을 내려놓을 수 없다.
    let (status, body) = admin
        .put("/users/admin", json!({ "role": "viewer" }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "last_admin");
    assert_eq!(admin.delete("/users/alice").await.0, StatusCode::NO_CONTENT);
    assert_eq!(alice.get("/nodes").await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn weak_passwords_and_bad_names_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": "bob", "password": "short", "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    for name in ["Bob!", "0190a5c4-7c1e-7cc3-8a8e-2d1f0b8f4c11"] {
        let (status, _) = admin
            .post(
                "/users",
                json!({ "name": name, "password": "long-enough", "role": "viewer" }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
    }
}

#[tokio::test]
async fn token_with_wrong_secret_is_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let token = admin.token.clone().unwrap();
    let (id, _) = token.rsplit_once('_').unwrap();
    let mut forged = admin.clone();
    forged.token = Some(format!("{id}_{}", "0123456789abcdef".repeat(4)));
    let (status, body) = forged.get("/nodes").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
    assert_eq!(admin.get("/nodes").await.0, StatusCode::OK);
}

#[tokio::test]
async fn changing_own_password_ends_existing_sessions() {
    let env = Env::start().await;
    env.user("alice", "viewer").await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("{}/api/v1{p}", env.base);
    let resp = http
        .post(url("/auth/login"))
        .json(&json!({ "username": "alice", "password": "password-1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let resp = http
        .put(url("/users/alice"))
        .header("cookie", &cookie)
        .json(&json!({ "password": "new-password", "current_password": "password-1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = http
        .get(url("/auth/whoami"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    env.login("alice", "new-password").await;
}

#[tokio::test]
async fn admin_resets_password_without_current_password() {
    let env = Env::start().await;
    let admin = env.admin().await;
    env.user("alice", "viewer").await;
    let (status, body) = admin
        .put(
            "/users/alice",
            json!({ "password": "reset-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    env.login("alice", "reset-password").await;
}

#[tokio::test]
async fn viewers_cannot_probe_other_users() {
    let env = Env::start().await;
    let viewer = env.user("viewer", "viewer").await;
    for key in ["/users/nobody", "/users/admin"] {
        let (status, body) = viewer
            .put(key, json!({ "password": "new-password" }), None)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{key}");
        assert_eq!(body["code"], "forbidden", "{key}");
    }
}
