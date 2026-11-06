//! 감사 로그 기록: 바꾸는 요청·로그인·로그아웃·콘솔 열기가 누가·어떻게·무엇을과 함께 적히고,
//! 읽는 요청과 비밀번호는 적히지 않는다.

mod common;

use std::time::Instant;

use common::{ADMIN_PASSWORD, Env, WAIT};
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery, AuditTarget};
use opl_types::auth::Role;
use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
use reqwest::StatusCode;
use serde_json::json;

const DEV_PRIVATE: &str = include_str!("../../opl-types/tests/fixtures/dev-license.key");

/// `found`가 참이 될 때까지 이 노드의 감사 로그를 다시 읽는다(쓰기 스레드가 곧 적는다).
async fn entries_until(env: &Env, found: impl Fn(&[AuditEntry]) -> bool) -> Vec<AuditEntry> {
    let start = Instant::now();
    loop {
        let entries = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap();
        if found(&entries) {
            return entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// `action`과 `status`가 같은 가장 새 항목
async fn entry(env: &Env, action: &str, status: u16) -> AuditEntry {
    let has = |e: &AuditEntry| e.action == action && e.status == status;
    entries_until(env, |es| es.iter().any(has))
        .await
        .into_iter()
        .find(has)
        .unwrap()
}

fn target(kind: &str, key: &str) -> Option<AuditTarget> {
    Some(AuditTarget {
        kind: kind.into(),
        key: key.into(),
    })
}

#[tokio::test]
async fn a_changing_request_is_recorded_with_who_and_what() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": "carol", "password": "password-1", "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created = entry(&env, "POST /users", 201).await;
    assert_eq!(created.user.as_deref(), Some("admin"));
    assert!(created.user_id.is_some());
    assert_eq!(created.role, Some(Role::Admin));
    assert_eq!(created.auth, AuditAuth::Token);
    assert_eq!(created.method, "POST");
    assert_eq!(created.path, "/users");
    assert_eq!(created.target, None);
    assert_eq!(created.ip, "127.0.0.1");
    assert_eq!(created.node, "node1");

    // 쿼리 문자열은 경로에 넣지 않는다. 대상은 템플릿의 첫 조각과 첫 매개변수 값이다.
    let (status, body) = admin.delete("/users/carol?reason=secret").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let deleted = entry(&env, "DELETE /users/{key}", 204).await;
    assert_eq!(deleted.path, "/users/carol");
    assert_eq!(deleted.target, target("users", "carol"));
    assert_eq!(deleted.user.as_deref(), Some("admin"));
}

#[tokio::test]
async fn denied_and_unauthenticated_requests_are_recorded() {
    let env = Env::start().await;
    let viewer = env.user("vic", "viewer").await;
    let (status, _) = viewer
        .post(
            "/users",
            json!({ "name": "x1", "password": "password-1", "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let denied = entry(&env, "POST /users", 403).await;
    assert_eq!(denied.user.as_deref(), Some("vic"));
    assert_eq!(denied.role, Some(Role::Viewer));
    assert_eq!(denied.auth, AuditAuth::Token);

    // 틀린 토큰: 사용자는 모르지만 토큰으로 시도한 것은 남는다.
    let mut forged = env.anonymous();
    forged.token = Some(format!("opl_{}_deadbeef", uuid::Uuid::now_v7().simple()));
    let (status, _) = forged.delete("/users/vic").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let bad = entry(&env, "DELETE /users/{key}", 401).await;
    assert_eq!(bad.user, None);
    assert_eq!(bad.user_id, None);
    assert_eq!(bad.auth, AuditAuth::Token);
    assert_eq!(bad.target, target("users", "vic"));

    // 자격 증명이 없는 요청
    let (status, _) = env
        .anonymous()
        .post("/tokens", json!({ "name": "t" }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let anon = entry(&env, "POST /tokens", 401).await;
    assert_eq!((anon.user, anon.auth), (None, AuditAuth::None));
}

#[tokio::test]
async fn logins_are_recorded_without_the_password() {
    let env = Env::start().await;
    let http = reqwest::Client::new();
    let url = format!("{}/api/v1/auth/login", env.base);
    let wrong = "wrong-pass-7f3a";
    for (user, password, status) in [
        ("admin", wrong, 401),
        ("Not A Name", wrong, 401),
        ("admin", ADMIN_PASSWORD, 200),
    ] {
        let resp = http
            .post(&url)
            .json(&json!({ "username": user, "password": password }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), status, "{user}");
    }
    let entries = entries_until(&env, |es| {
        es.iter().filter(|e| e.action == "POST /auth/login").count() == 3
    })
    .await;
    let logins: Vec<&AuditEntry> = entries
        .iter()
        .filter(|e| e.action == "POST /auth/login")
        .collect();
    // 새 것부터
    let (ok, invalid, failed) = (logins[0], logins[1], logins[2]);
    assert_eq!(failed.status, 401);
    assert_eq!(failed.detail.as_deref(), Some("username=admin"));
    assert_eq!((&failed.user, failed.auth), (&None, AuditAuth::None));
    assert_eq!(invalid.status, 401);
    assert_eq!(invalid.detail.as_deref(), Some("username=<invalid>"));
    assert_eq!(ok.status, 200);
    assert_eq!(ok.detail.as_deref(), Some("username=admin"));
    assert_eq!(ok.user.as_deref(), Some("admin"));
    assert_eq!(ok.role, Some(Role::Admin));
    assert_eq!(ok.auth, AuditAuth::Session);

    env.state.audit.shutdown().await;
    let text = std::fs::read_to_string(env.dir.path().join("audit/audit.log")).unwrap();
    assert!(text.contains("username=admin"), "{text}");
    for secret in [wrong, ADMIN_PASSWORD] {
        assert!(!text.contains(secret), "{text}");
    }
}

#[tokio::test]
async fn session_requests_and_logout_name_the_user() {
    let env = Env::start().await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("{}/api/v1{p}", env.base);
    let resp = http
        .post(url("/auth/login"))
        .json(&json!({ "username": "admin", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    let cookie = resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let resp = http
        .post(url("/tokens"))
        .header("cookie", &cookie)
        .json(&json!({ "name": "t1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let token = entry(&env, "POST /tokens", 201).await;
    assert_eq!(token.user.as_deref(), Some("admin"));
    assert_eq!(token.auth, AuditAuth::Session);

    let resp = http
        .post(url("/auth/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let out = entry(&env, "POST /auth/logout", 204).await;
    assert_eq!(out.user.as_deref(), Some("admin"));
    assert_eq!(out.auth, AuditAuth::Session);

    // 세션이 없으면 사용자는 모른다.
    let resp = http.post(url("/auth/logout")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let entries = entries_until(&env, |es| {
        es.iter()
            .filter(|e| e.action == "POST /auth/logout")
            .count()
            == 2
    })
    .await;
    let anon = entries
        .iter()
        .find(|e| e.action == "POST /auth/logout")
        .unwrap();
    assert_eq!((&anon.user, anon.auth), (&None, AuditAuth::None));
}

#[tokio::test]
async fn reads_are_not_recorded_but_opening_a_console_is() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.get("/users").await.0, StatusCode::OK);
    assert_eq!(admin.get("/vms/web-1").await.0, StatusCode::NOT_FOUND);
    let (status, _) = admin.get("/vms/web-1/console").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let console = entry(&env, "GET /vms/{key}/console", 404).await;
    assert_eq!(console.target, target("vms", "web-1"));
    assert_eq!(console.user.as_deref(), Some("admin"));
    // 콘솔 앞의 GET은 적지 않았다(콘솔 항목은 그 뒤에 적혔다).
    let entries = entries_until(&env, |_| true).await;
    assert!(
        entries
            .iter()
            .all(|e| e.method != "GET" || e.action == "GET /vms/{key}/console"),
        "{entries:#?}"
    );
}

#[tokio::test]
async fn a_subscription_key_is_recorded_by_its_id() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let key = sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "sub-1".into(),
            customer: "Example Corp".into(),
            level: Level::Premium,
            nodes: 3,
            issued_at: now - 86_400,
            expires_at: now + 30 * 86_400,
        },
    )
    .unwrap();
    let (status, body) = admin
        .put("/subscription", json!({ "key": key }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let put = entry(&env, "PUT /subscription", 200).await;
    assert_eq!(put.detail.as_deref(), Some("id=sub-1"));
    // 나쁜 키는 detail이 없다.
    let (status, _) = admin
        .put("/subscription", json!({ "key": "nope" }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(entry(&env, "PUT /subscription", 400).await.detail, None);
}
