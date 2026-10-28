//! 로그인, 세션, API 토큰, 역할 권한.

mod common;

use std::sync::Arc;

use common::{ADMIN_PASSWORD, Env};
use opl_api::AppState;
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
    // 브라우저의 쿠키도 지운다.
    let cleared = resp.headers()["set-cookie"].to_str().unwrap();
    assert!(cleared.starts_with("opl_session=;"), "{cleared}");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
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

/// 로그인을 한 번 시도하고 (상태, Retry-After, 본문)을 돌려준다.
async fn try_login(
    env: &Env,
    user: &str,
    password: &str,
) -> (StatusCode, Option<String>, serde_json::Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&json!({ "username": user, "password": password }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .map(|v| v.to_str().unwrap().to_string());
    (status, retry_after, resp.json().await.unwrap())
}

#[tokio::test]
async fn repeated_login_failures_are_throttled() {
    let env = Env::start().await;
    for i in 0..5 {
        let (status, retry_after, _) = try_login(&env, "admin", "wrong-password").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{i}");
        assert_eq!(retry_after, None);
    }
    let (status, retry_after, body) = try_login(&env, "admin", "wrong-password").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "too_many_attempts");
    let secs: u64 = retry_after.unwrap().parse().unwrap();
    assert!((1..=30).contains(&secs), "{secs}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains(&format!("try again in {secs} seconds")),
        "{body}"
    );
    // 막힌 동안에는 맞는 비밀번호도 받지 않는다.
    let (status, _, _) = try_login(&env, "admin", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // 없는 사용자도 실패로 센다.
    for _ in 0..5 {
        assert_eq!(
            try_login(&env, "nobody", "wrong-password").await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        try_login(&env, "nobody", "wrong-password").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn successful_login_resets_failures() {
    let env = Env::start().await;
    for _ in 0..4 {
        try_login(&env, "admin", "wrong-password").await;
    }
    assert_eq!(
        try_login(&env, "admin", ADMIN_PASSWORD).await.0,
        StatusCode::OK
    );
    for _ in 0..4 {
        try_login(&env, "admin", "wrong-password").await;
    }
    assert_eq!(
        try_login(&env, "admin", ADMIN_PASSWORD).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn concurrent_logins_do_not_pass_the_limit() {
    let env = Env::start().await;
    // 비밀번호 확인을 기다리는 동안 들어온 시도도 앞의 시도를 실패로 보고 막힌다.
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let url = format!("{}/api/v1/auth/login", env.base);
        set.spawn(async move {
            reqwest::Client::new()
                .post(url)
                .json(&json!({ "username": "admin", "password": "wrong-password" }))
                .send()
                .await
                .unwrap()
                .status()
        });
    }
    let statuses = set.join_all().await;
    let throttled = statuses
        .iter()
        .filter(|s| **s == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(throttled, 15, "{statuses:?}");
}

#[tokio::test]
async fn impossible_user_names_count_only_against_the_ip() {
    let env = Env::start().await;
    let long = "a".repeat(10_000);
    for name in ["Admin", "no such user", long.as_str()] {
        let (status, _, body) = try_login(&env, name, "wrong-password").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{name}");
        assert_eq!(body["code"], "invalid_credentials");
    }
    for _ in 3..20 {
        try_login(&env, &long, "wrong-password").await;
    }
    // IP 키는 20번 실패로 막혀 맞는 비밀번호도 받지 않는다.
    assert_eq!(
        try_login(&env, "admin", ADMIN_PASSWORD).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn current_password_check_is_throttled_with_login() {
    let env = Env::start().await;
    let alice = env.user("alice", "viewer").await;
    let change = json!({ "password": "new-password", "current_password": "wrong-password" });
    for i in 0..5 {
        let (status, body) = alice.put("/users/alice", change.clone(), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{i}");
        assert_eq!(body["code"], "invalid_credentials");
    }
    let (status, body) = alice
        .put(
            "/users/alice",
            json!({ "password": "new-password", "current_password": "password-1" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "too_many_attempts");
    // 로그인과 같은 기록을 쓴다.
    assert_eq!(
        try_login(&env, "alice", "password-1").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

/// 같은 클러스터를 쓰는 두 번째 API 서버(다른 노드 역할, 세션 저장소가 따로다)를 띄우고 주소를 돌려준다.
async fn serve_another_node(env: &Env) -> String {
    let state = Arc::new(AppState::new(
        env.cluster.clone(),
        env.node,
        env.hv.clone(),
        env.dir.path().join("uploads-b"),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// `base`에 로그인해 세션 쿠키(`opl_session=...`)를 돌려준다.
async fn cookie_login(base: &str, user: &str, password: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": user, "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn whoami_with_cookie(base: &str, cookie: &str) -> (StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/auth/whoami"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

#[tokio::test]
async fn password_change_ends_sessions_on_other_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let alice = env.user("alice", "viewer").await;
    let b = serve_another_node(&env).await;

    // 본인이 노드 A에서 비밀번호를 바꾸면 노드 B의 옛 세션도 끊긴다.
    let cookie = cookie_login(&b, "alice", "password-1").await;
    assert_eq!(whoami_with_cookie(&b, &cookie).await.0, StatusCode::OK);
    let (status, body) = alice
        .put(
            "/users/alice",
            json!({ "password": "new-password", "current_password": "password-1" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = whoami_with_cookie(&b, &cookie).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
    // API 토큰은 그대로다.
    assert_eq!(alice.get("/auth/whoami").await.0, StatusCode::OK);

    // 관리자가 바꿔도 같다.
    let cookie = cookie_login(&b, "alice", "new-password").await;
    let (status, body) = admin
        .put(
            "/users/alice",
            json!({ "password": "reset-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        whoami_with_cookie(&b, &cookie).await.0,
        StatusCode::UNAUTHORIZED
    );

    // 비밀번호가 아닌 것(역할)을 바꾸면 세션은 남는다.
    let cookie = cookie_login(&b, "alice", "reset-password").await;
    let (status, body) = admin
        .put("/users/alice", json!({ "role": "operator" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = whoami_with_cookie(&b, &cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["role"], "operator");
}
