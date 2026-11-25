//! 2단계 인증 API(계획 62 설계 §10·§14): TOTP 등록·로그인·재사용 거절, 복구 코드 한 번만, 대기 세션이 모든 경로에서
//! 401, 정책의 강제 등록, 관리자 초기화가 세션을 끊음, API 토큰은 거치지 않음, 패스키 등록·로그인, 시도 제한.

mod common;

use std::time::SystemTime;

use common::authenticator::{Alg, Authenticator};
use common::{ADMIN_PASSWORD, Client, Env};
use orbivisor_types::two_factor::{decode_secret, totp, totp_step};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// 쿠키로 말하는 브라우저 흉내. `host`가 있으면 `Host` 헤더를 바꾼다(패스키는 IP 주소에서 쓰지 못한다).
struct Browser {
    base: String,
    http: reqwest::Client,
    cookie: Option<String>,
    host: Option<String>,
}

impl Browser {
    fn new(env: &Env) -> Self {
        Self::at(&env.base)
    }

    /// 다른 API 주소(`serve_again`)
    fn at(base: &str) -> Self {
        Self {
            base: base.to_string(),
            http: reqwest::Client::new(),
            cookie: None,
            host: None,
        }
    }

    /// `localhost:<포트>`로 접속한 것처럼(RP ID `localhost`)
    fn on_localhost(env: &Env) -> Self {
        let port = env.base.rsplit(':').next().unwrap().to_string();
        Self {
            host: Some(format!("localhost:{port}")),
            ..Self::new(env)
        }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.host.as_deref().unwrap())
    }

    async fn send(
        &mut self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut r = self
            .http
            .request(method, format!("{}/api/v1{path}", self.base));
        if let Some(c) = &self.cookie {
            r = r.header("cookie", c);
        }
        if let Some(h) = &self.host {
            r = r.header("host", h);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        if let Some(set) = resp.headers().get("set-cookie") {
            let c = set.to_str().unwrap().split(';').next().unwrap().to_string();
            self.cookie = (c != "opl_session=").then_some(c);
        }
        let status = resp.status();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn login(&mut self, user: &str, password: &str) -> Value {
        let (status, body) = self
            .send(
                Method::POST,
                "/auth/login",
                Some(json!({ "username": user, "password": password })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn get(&mut self, path: &str) -> (StatusCode, Value) {
        self.send(Method::GET, path, None).await
    }

    async fn post(&mut self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::POST, path, Some(body)).await
    }

    async fn delete(&mut self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::DELETE, path, Some(body)).await
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 지금 칸에서 `offset`칸 뒤의 코드(±1칸은 받는다)
fn code(secret: &str, offset: u64) -> String {
    let secret = decode_secret(secret).unwrap();
    format!("{:06}", totp(&secret, totp_step(now()) + offset, 6))
}

/// 지금 TOTP 칸(30초)이 적어도 20초 남을 때까지 기다린다. 칸을 다시 쓰지 못함을 보는 시험이 도중에 칸이 넘어가
/// "같은 코드"가 새 칸의 코드가 되어 받아들여지지 않게(느린 러너에서 본 깜빡임).
async fn wait_for_fresh_totp_step() {
    let left = 30 - now() % 30;
    if left < 20 {
        tokio::time::sleep(std::time::Duration::from_secs(left + 1)).await;
    }
}

/// `name` 사용자(비밀번호 `password-1`)를 만든다.
async fn create_user(admin: &Client, name: &str, role: &str) {
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": name, "password": "password-1", "role": role }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// 정식 세션으로 TOTP를 켜고 (비밀, 복구 코드)
async fn enable_totp(b: &mut Browser) -> (String, Vec<String>) {
    let (status, enrol) = b.post("/auth/2fa/totp", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{enrol}");
    let secret = enrol["secret"].as_str().unwrap().to_string();
    let uri = enrol["uri"].as_str().unwrap();
    assert!(uri.starts_with("otpauth://totp/Orbivisor:"), "{uri}");
    assert!(uri.contains(&format!("secret={secret}")), "{uri}");
    let (status, codes) = b
        .post(
            "/auth/2fa/totp/confirm",
            json!({ "code": code(&secret, 0) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{codes}");
    let codes: Vec<String> = codes["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(codes.len(), 10);
    (secret, codes)
}

#[tokio::test]
async fn totp_login_rejects_reuse_and_recovery_codes_work_once() {
    let env = Env::start().await;
    wait_for_fresh_totp_step().await;
    let admin = env.admin().await;
    create_user(&admin, "alice", "operator").await;
    let mut b = Browser::new(&env);
    let body = b.login("alice", "password-1").await;
    assert!(body.get("second_factor").is_none(), "{body}");
    assert_eq!(body["name"], "alice");
    let (_, view) = b.get("/auth/2fa").await;
    assert_eq!(view["totp"], false);
    assert_eq!(view["required"], false);

    // 첫 코드가 틀리면 켜지지 않는다.
    let (status, enrol) = b.post("/auth/2fa/totp", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let secret = enrol["secret"].as_str().unwrap().to_string();
    let wrong = format!(
        "{:06}",
        (code(&secret, 0).parse::<u32>().unwrap() + 1) % 1_000_000
    );
    let (status, err) = b
        .post("/auth/2fa/totp/confirm", json!({ "code": wrong }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{err}");
    assert_eq!(err["code"], "invalid_second_factor");
    let (_, view) = b.get("/auth/2fa").await;
    assert_eq!(
        (view["totp"].clone(), view["totp_pending"].clone()),
        (json!(false), json!(true))
    );
    let (secret, recovery) = enable_totp(&mut b).await;
    let (_, view) = b.get("/auth/2fa").await;
    assert_eq!(view["totp"], true);
    assert_eq!(view["recovery_codes"], 10);
    // 비밀은 어디에도 다시 나오지 않는다.
    assert!(!view.to_string().contains(&secret));
    let (status, _) = b.post("/auth/2fa/totp", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, users) = admin.get("/users").await;
    let alice = users["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["name"] == "alice")
        .unwrap();
    assert_eq!(alice["two_factor"], true);

    // 다시 로그인하면 대기 세션이다.
    let mut b = Browser::new(&env);
    let body = b.login("alice", "password-1").await;
    assert_eq!(
        body["second_factor"]["methods"],
        json!(["totp", "recovery"])
    );
    assert_eq!(body["second_factor"]["enroll"], false);
    let (status, err) = b.get("/auth/whoami").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(err["code"], "second_factor_required");
    // 확인에 쓴 칸은 다시 받지 않는다.
    let (status, err) = b
        .post(
            "/auth/login/second-factor",
            json!({ "totp": code(&secret, 0) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{err}");
    assert_eq!(err["code"], "invalid_second_factor");
    let next = code(&secret, 1);
    let (status, who) = b
        .post("/auth/login/second-factor", json!({ "totp": next }))
        .await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert_eq!(who["two_factor"], true);
    let (status, who) = b.get("/auth/whoami").await;
    assert_eq!(
        (status, who["name"].clone()),
        (StatusCode::OK, json!("alice"))
    );
    // 이미 끝난 세션으로 둘째 단계를 다시 하지 않는다.
    let (status, _) = b
        .post("/auth/login/second-factor", json!({ "totp": next }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // 같은 코드로 다른 로그인을 끝내지 못한다(재사용).
    let mut c = Browser::new(&env);
    c.login("alice", "password-1").await;
    let (status, _) = c
        .post("/auth/login/second-factor", json!({ "totp": next }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // 복구 코드는 한 번만(대소문자·공백 무시)
    let (status, _) = c
        .post(
            "/auth/login/second-factor",
            json!({ "recovery": recovery[0].to_uppercase().replace('-', " ") }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut d = Browser::new(&env);
    d.login("alice", "password-1").await;
    let (status, _) = d
        .post(
            "/auth/login/second-factor",
            json!({ "recovery": recovery[0] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = d
        .post(
            "/auth/login/second-factor",
            json!({ "recovery": recovery[1], "totp": "123456" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = d
        .post(
            "/auth/login/second-factor",
            json!({ "recovery": recovery[1] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, view) = d.get("/auth/2fa").await;
    assert_eq!(view["recovery_codes"], 8);

    // 복구 코드 다시 만들기·TOTP 끄기는 본인 확인이 필요하다.
    let (status, _) = d.post("/auth/2fa/recovery", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = d
        .post(
            "/auth/2fa/recovery",
            json!({ "password": "wrong-password" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, fresh) = d
        .post("/auth/2fa/recovery", json!({ "password": "password-1" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    assert_eq!(fresh["codes"].as_array().unwrap().len(), 10);
    let fresh = fresh["codes"][0].as_str().unwrap().to_string();
    let (status, _) = d
        .delete("/auth/2fa/totp", json!({ "code": recovery[2] }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "old recovery codes are gone");
    let (status, _) = d.delete("/auth/2fa/totp", json!({ "code": fresh })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, view) = d.get("/auth/2fa").await;
    assert_eq!(
        (view["totp"].clone(), view["recovery_codes"].clone()),
        (json!(false), json!(0))
    );
    let body = Browser::new(&env).login("alice", "password-1").await;
    assert!(body.get("second_factor").is_none(), "{body}");
}

#[tokio::test]
async fn a_pending_session_is_rejected_on_every_route() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_user(&admin, "alice", "admin").await;
    let mut b = Browser::new(&env);
    b.login("alice", "password-1").await;
    enable_totp(&mut b).await;
    let mut pending = Browser::new(&env);
    pending.login("alice", "password-1").await;
    // 인증 없이 받는 경로와 둘째 단계 자체
    let open = [
        "POST /auth/login",
        "POST /auth/logout",
        "GET /auth/oidc/callback",
        "GET /auth/oidc/{realm}/start",
        "GET /auth/realms",
        "GET /cluster/ca",
        "GET /version",
        "GET /health",
        "POST /cluster/join",
        "POST /auth/login/second-factor",
    ];
    let (_, doc) = env.anonymous().get("/openapi.json").await;
    let mut checked = 0;
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in ["get", "post", "put", "patch", "delete"] {
            if item.get(method).is_none() {
                continue;
            }
            let m = method.to_uppercase();
            if open.contains(&format!("{m} {path}").as_str()) {
                continue;
            }
            let concrete: String = path
                .split('/')
                .map(|s| if s.starts_with('{') { "x" } else { s })
                .collect::<Vec<_>>()
                .join("/");
            let (status, body) = pending
                .send(
                    Method::from_bytes(m.as_bytes()).unwrap(),
                    &concrete,
                    Some(json!({})),
                )
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{m} {path}: {body}");
            assert_eq!(body["code"], "second_factor_required", "{m} {path}");
            checked += 1;
        }
    }
    assert!(checked > 150, "{checked}");
}

#[tokio::test]
async fn the_policy_forces_enrollment_and_keeps_the_last_factor() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_user(&admin, "bob", "operator").await;
    for bad in [
        json!({ "realms": ["nope"] }),
        json!({ "realms": ["Bad Name"] }),
        json!({ "rp_id": "10.0.0.1" }),
    ] {
        let (status, body) = admin
            .patch("/settings", json!({ "two_factor": bad }), None)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let (status, settings) = admin
        .patch(
            "/settings",
            json!({ "two_factor": { "roles": ["operator"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(
        settings["spec"]["two_factor"],
        json!({ "roles": ["operator"] })
    );

    let mut b = Browser::new(&env);
    let body = b.login("bob", "password-1").await;
    assert_eq!(body["second_factor"]["enroll"], true, "{body}");
    assert_eq!(body["second_factor"]["methods"], json!([]));
    let (status, _) = b.get("/vms").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, err) = b
        .post("/auth/login/second-factor", json!({ "totp": "123456" }))
        .await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::FORBIDDEN, json!("enrollment_required"))
    );
    let (status, view) = b.get("/auth/2fa").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        (view["required"].clone(), view["enrolling"].clone()),
        (json!(true), json!(true))
    );
    // 강제 등록 중에는 요소 빼기·토큰 같은 다른 경로를 쓰지 못한다.
    let (status, _) = b
        .post("/auth/2fa/recovery", json!({ "password": "password-1" }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = b.post("/tokens", json!({ "name": "t" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (secret, _) = enable_totp(&mut b).await;
    // 등록을 마치면 같은 쿠키로 로그인이 끝난다.
    let (status, who) = b.get("/auth/whoami").await;
    assert_eq!(
        (status, who["name"].clone()),
        (StatusCode::OK, json!("bob"))
    );
    // 정책이 요구하므로 마지막 요소는 빼지 못한다.
    let (status, err) = b
        .delete("/auth/2fa/totp", json!({ "code": code(&secret, 1) }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["code"], "two_factor_required");
    // 정책에 없는 admin은 그대로 들어온다.
    let body = Browser::new(&env).login("admin", ADMIN_PASSWORD).await;
    assert!(body.get("second_factor").is_none(), "{body}");
    // 빈 정책은 지운다.
    let (status, settings) = admin
        .patch("/settings", json!({ "two_factor": {} }), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(settings["spec"].get("two_factor").is_none(), "{settings}");
}

#[tokio::test]
async fn an_admin_reset_ends_sessions_and_tokens_bypass_two_factor() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_user(&admin, "alice", "operator").await;
    // 2FA 전에 만든 토큰과 범위 토큰
    let token = env.login("alice", "password-1").await;
    let (status, scoped) = token
        .post(
            "/tokens",
            json!({ "name": "scoped", "scope": { "privileges": ["vm.power"], "targets": [{ "type": "root" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{scoped}");
    let scoped = Client {
        token: Some(scoped["secret"].as_str().unwrap().to_string()),
        ..env.anonymous()
    };
    let mut b = Browser::new(&env);
    b.login("alice", "password-1").await;
    let (secret, _) = enable_totp(&mut b).await;
    // 토큰은 2FA를 거치지 않는다(자동화). 범위 토큰은 2FA를 바꾸지 못한다.
    assert_eq!(token.get("/auth/whoami").await.0, StatusCode::OK);
    let (status, view) = token.get("/auth/2fa").await;
    assert_eq!(
        (status, view["totp"].clone()),
        (StatusCode::OK, json!(true))
    );
    let (status, _) = scoped.post("/auth/2fa/totp", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let mut c = Browser::new(&env);
    c.login("alice", "password-1").await;
    let (status, _) = c
        .post(
            "/auth/login/second-factor",
            json!({ "totp": code(&secret, 1) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut pending = Browser::new(&env);
    pending.login("alice", "password-1").await;

    // 관리자만, 자기 자신은 안 된다.
    let (status, _) = token.delete("/users/alice/two-factor").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, err) = admin.delete("/users/admin/two-factor").await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::CONFLICT, json!("cannot_reset_self"))
    );
    let (status, _) = admin.delete("/users/alice/two-factor").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 정식 세션과 대기 세션이 끊기고 토큰은 남는다.
    assert_eq!(c.get("/auth/whoami").await.0, StatusCode::UNAUTHORIZED);
    let (status, _) = pending
        .post(
            "/auth/login/second-factor",
            json!({ "totp": code(&secret, 1) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(token.get("/auth/whoami").await.0, StatusCode::OK);
    let body = Browser::new(&env).login("alice", "password-1").await;
    assert!(body.get("second_factor").is_none(), "{body}");
    let (_, users) = admin.get("/users").await;
    assert!(
        users["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u.get("two_factor").is_none()),
        "{users}"
    );
}

/// 리뷰 1: 틀린 둘째 단계는 사용자마다 Raft에 센다(노드·IP와 관계없이). 다섯 번 틀리면 대기 세션이 끝나고, 다른
/// 노드(메모리 제한이 비어 있는 API)에서 새로 로그인해도 막힌 동안은 맞는 코드도 받지 않는다.
#[tokio::test]
async fn wrong_codes_are_rate_limited_across_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_user(&admin, "alice", "viewer").await;
    let mut b = Browser::new(&env);
    b.login("alice", "password-1").await;
    let (secret, _) = enable_totp(&mut b).await;
    let mut p = Browser::new(&env);
    p.login("alice", "password-1").await;
    let right = code(&secret, 1);
    let wrong = format!(
        "{:06}",
        (right.parse::<u32>().unwrap() + 500_000) % 1_000_000
    );
    for i in 0..5 {
        let (status, err) = p
            .post("/auth/login/second-factor", json!({ "totp": wrong }))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{err}");
        assert_eq!(err["code"], "invalid_second_factor");
        assert_eq!(
            err["message"].as_str().unwrap().contains("sign in again"),
            i == 4,
            "{err}"
        );
    }
    // 대기 세션이 끝났다: 맞는 코드도 쓸 곳이 없다.
    let (status, err) = p
        .post("/auth/login/second-factor", json!({ "totp": right }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{err}");
    assert_ne!(err["code"], "invalid_second_factor", "{err}");
    let tf = env
        .cluster
        .list::<orbivisor_types::two_factor::TwoFactorSpec>()
        .unwrap()
        .1
        .remove(0);
    assert_eq!(tf.status.failures, 5);
    assert!(tf.status.blocked_until > now(), "{:?}", tf.status);

    // 다른 노드: 이 노드의 메모리 시도 제한을 모른다.
    let other = serve_again(&env).await;
    let mut q = Browser::at(&other);
    let body = q.login("alice", "password-1").await;
    assert!(body.get("second_factor").is_some(), "{body}");
    let (status, err) = q
        .post("/auth/login/second-factor", json!({ "totp": right }))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{err}");
    assert_eq!(err["code"], "too_many_attempts");

    // 막힘이 풀리면(시각을 지난 것으로 둔다) 맞는 코드로 들어오고 센 것이 지워진다.
    let mut tf = env
        .cluster
        .list::<orbivisor_types::two_factor::TwoFactorSpec>()
        .unwrap()
        .1
        .remove(0);
    tf.status.blocked_until = 1;
    env.cluster
        .update_status::<orbivisor_types::two_factor::TwoFactorSpec>(tf.id, &tf.status)
        .await
        .unwrap();
    let mut r = Browser::at(&other);
    r.login("alice", "password-1").await;
    let (status, who) = r
        .post("/auth/login/second-factor", json!({ "totp": right }))
        .await;
    assert_eq!(status, StatusCode::OK, "{who}");
    let tf = env
        .cluster
        .list::<orbivisor_types::two_factor::TwoFactorSpec>()
        .unwrap()
        .1
        .remove(0);
    assert_eq!((tf.status.failures, tf.status.blocked_until), (0, 0));
}

/// 같은 클러스터를 새 `AppState`(새 메모리 시도 제한·세션)로 한 번 더 서비스한다(다른 노드의 API 흉내).
async fn serve_again(env: &Env) -> String {
    let state = orbivisor_api::AppState::new(
        env.cluster.clone(),
        env.node,
        env.hv.clone(),
        env.dir.path().join("uploads"),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = orbivisor_api::router(std::sync::Arc::new(state));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    base
}

#[tokio::test]
async fn passkeys_register_and_sign_in() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_user(&admin, "carol", "viewer").await;
    let mut b = Browser::on_localhost(&env);
    b.login("carol", "password-1").await;
    let (_, view) = b.get("/auth/2fa").await;
    assert_eq!(view["rp_id"], "localhost");

    // IP 주소로 접속하면 패스키를 쓸 수 없다.
    let mut ip = Browser::new(&env);
    ip.login("carol", "password-1").await;
    let (status, err) = ip.post("/auth/2fa/webauthn/register", json!({})).await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("webauthn_unavailable"))
    );

    let mut key = Authenticator::new(Alg::Es256);
    let (status, start) = b.post("/auth/2fa/webauthn/register", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let options = &start["options"];
    assert_eq!(options["rp"]["id"], "localhost");
    assert_eq!(options["attestation"], "none");
    assert_eq!(
        options["pubKeyCredParams"],
        json!([{ "type": "public-key", "alg": -7 }, { "type": "public-key", "alg": -8 }, { "type": "public-key", "alg": -257 }])
    );
    let challenge = options["challenge"].as_str().unwrap().to_string();
    // 틀린 origin
    let (status, err) = b
        .post(
            "/auth/2fa/webauthn/register/finish",
            json!({ "token": start["token"], "name": "laptop",
                    "credential": key.register("localhost", &challenge, "http://evil.example") }),
        )
        .await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("invalid_passkey"))
    );
    // 위조한 토큰
    let (status, _) = b
        .post(
            "/auth/2fa/webauthn/register/finish",
            json!({ "token": "e30.AAAA", "name": "laptop",
                    "credential": key.register("localhost", &challenge, &b.origin()) }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, added) = b
        .post(
            "/auth/2fa/webauthn/register/finish",
            json!({ "token": start["token"], "name": "laptop",
                    "credential": key.register("localhost", &challenge, &b.origin()) }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{added}");
    assert_eq!(added["passkey"]["id"], key.id());
    let recovery = added["recovery_codes"][0].as_str().unwrap().to_string();
    // 리뷰 1: 요소가 있으면 더하기에 본인 확인이 필요하다(훔친 세션이 요소를 더하지 못하게).
    let (status, err) = b
        .post(
            "/auth/2fa/webauthn/register/finish",
            json!({ "token": start["token"], "name": "again",
                    "credential": key.register("localhost", &challenge, &b.origin()) }),
        )
        .await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("proof_required")),
        "{err}"
    );
    // 같은 패스키를 두 번 넣지 않는다.
    let (status, _) = b
        .post(
            "/auth/2fa/webauthn/register/finish",
            json!({ "token": start["token"], "name": "again",
                    "credential": key.register("localhost", &challenge, &b.origin()),
                    "proof": { "password": "password-1" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // 인증 앱 더하기도 본인 확인을 받는다.
    let (status, enrol) = b.post("/auth/2fa/totp", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{enrol}");
    let secret = enrol["secret"].as_str().unwrap().to_string();
    let (status, err) = b
        .post(
            "/auth/2fa/totp/confirm",
            json!({ "code": code(&secret, 0) }),
        )
        .await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("proof_required")),
        "{err}"
    );
    // 리뷰 2 I3: 새 앱의 코드가 틀리면 본인 확인(복구 코드)을 쓰지 않는다.
    let right = code(&secret, 0);
    let wrong = format!(
        "{:06}",
        (right.parse::<u32>().unwrap() + 500_000) % 1_000_000
    );
    // 다른 노드에서(이 노드의 메모리 시도 제한을 아래 확인들에 남겨 둔다)
    let mut other = Browser::at(&serve_again(&env).await);
    other.cookie = b.cookie.clone();
    other.host = b.host.clone();
    let (status, err) = other
        .post(
            "/auth/2fa/totp/confirm",
            json!({ "code": wrong, "proof": { "code": recovery } }),
        )
        .await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::FORBIDDEN, json!("invalid_second_factor")),
        "{err}"
    );
    let tf = env
        .cluster
        .list::<orbivisor_types::two_factor::TwoFactorSpec>()
        .unwrap()
        .1
        .remove(0);
    assert_eq!(tf.spec.recovery.len(), 10, "the recovery code is kept");
    let (status, err) = b
        .post(
            "/auth/2fa/totp/confirm",
            json!({ "code": code(&secret, 0), "proof": { "password": "wrong-password" } }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{err}");
    let (status, codes) = b
        .post(
            "/auth/2fa/totp/confirm",
            json!({ "code": code(&secret, 0), "proof": { "password": "password-1" } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{codes}");
    // 첫 요소가 아니라 새 복구 코드는 없다.
    assert_eq!(codes["codes"], json!([]));
    let (status, _) = b
        .delete("/auth/2fa/totp", json!({ "password": "password-1" }))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 패스키로 로그인
    let mut l = Browser::on_localhost(&env);
    let body = l.login("carol", "password-1").await;
    let prompt = &body["second_factor"];
    assert_eq!(prompt["methods"], json!(["recovery", "webauthn"]), "{body}");
    let request = &prompt["webauthn"];
    assert_eq!(request["rpId"], "localhost");
    assert_eq!(request["allowCredentials"][0]["id"], key.id());
    let login_challenge = request["challenge"].as_str().unwrap().to_string();
    let assertion = key.assert("localhost", &login_challenge, &l.origin());
    let (status, who) = l
        .post(
            "/auth/login/second-factor",
            json!({ "webauthn": assertion }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{who}");
    // 같은 응답을 다른 로그인에 다시 내지 못한다(새 챌린지, 횟수).
    let mut r = Browser::on_localhost(&env);
    r.login("carol", "password-1").await;
    let (status, _) = r
        .post(
            "/auth/login/second-factor",
            json!({ "webauthn": assertion }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // IP 주소 로그인은 복구 코드만 쓸 수 있다.
    let body = Browser::new(&env).login("carol", "password-1").await;
    assert_eq!(body["second_factor"]["methods"], json!(["recovery"]));
    assert!(body["second_factor"].get("webauthn").is_none());

    // 지우기는 본인 확인이 필요하고, 마지막 요소면 복구 코드도 지운다.
    let path = format!("/auth/2fa/webauthn/{}", key.id());
    let (status, _) = l.delete(&path, json!({ "password": "nope-nope" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = l.delete(&path, json!({ "code": recovery })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, view) = l.get("/auth/2fa").await;
    assert_eq!(view["passkeys"], json!([]));
    assert_eq!(view["recovery_codes"], 0);
    let body = Browser::on_localhost(&env)
        .login("carol", "password-1")
        .await;
    assert!(body.get("second_factor").is_none(), "{body}");
}

/// 기능 게이트(설계 §11)와 닫힌 실패(리뷰 1): 어느 노드가 `two-factor`를 적지 않았어도(합류 중인 노드) 요소가 있는
/// 사용자는 둘째 단계를 거친다(대기 세션은 이 노드의 메모리에만). 정책이 요구하는데 요소가 없는 사용자는 등록할 수
/// 없으므로 로그인이 409 `cluster_upgrading`이다. 2FA 쓰기·정책은 409 `cluster_upgrading`이다.
#[tokio::test]
async fn two_factor_fails_closed_while_a_node_lacks_the_feature() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_user(&admin, "alice", "viewer").await;
    create_user(&admin, "bob", "operator").await;
    let mut b = Browser::new(&env);
    b.login("alice", "password-1").await;
    let (secret, _) = enable_totp(&mut b).await;
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "two_factor": { "roles": ["operator"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut node = env
        .cluster
        .get::<orbivisor_types::node::NodeSpec>(env.node)
        .unwrap()
        .unwrap();
    node.status
        .features
        .retain(|f| f != orbivisor_types::node::TWO_FACTOR);
    env.cluster
        .update_status::<orbivisor_types::node::NodeSpec>(env.node, &node.status)
        .await
        .unwrap();
    let mut c = Browser::new(&env);
    let body = c.login("alice", "password-1").await;
    assert!(body.get("second_factor").is_some(), "{body}");
    let (status, _) = c.get("/vms").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // 대기 세션은 Raft에 쓰지 않는다(그 노드가 정식 세션으로 읽지 못하게).
    assert!(
        env.cluster
            .list::<orbivisor_types::auth::SessionSpec>()
            .unwrap()
            .1
            .iter()
            .all(|s| s.spec.pending.is_none())
    );
    let (status, who) = c
        .post(
            "/auth/login/second-factor",
            json!({ "totp": code(&secret, 1) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{who}");
    let (status, err) = c
        .post("/auth/2fa/recovery", json!({ "password": "password-1" }))
        .await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::CONFLICT, json!("cluster_upgrading"))
    );
    let (status, view) = c.get("/auth/2fa").await;
    assert_eq!(
        (status, view["available"].clone()),
        (StatusCode::OK, json!(false))
    );
    // 정책이 요구하는데 요소가 없다: 등록할 수 없으니 들이지 않는다.
    let (status, err) = Browser::new(&env)
        .send(
            Method::POST,
            "/auth/login",
            Some(json!({ "username": "bob", "password": "password-1" })),
        )
        .await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::CONFLICT, json!("cluster_upgrading")),
        "{err}"
    );
    let (status, err) = admin
        .patch(
            "/settings",
            json!({ "two_factor": { "roles": ["admin"] } }),
            None,
        )
        .await;
    assert_eq!(
        (status, err["code"].clone()),
        (StatusCode::CONFLICT, json!("cluster_upgrading"))
    );
}

/// 리뷰 1: 정책의 `admin`은 grant로 관리 권한(`cluster.admin`, 어디서든 `permission.modify`)을 받은 사용자도 덮는다.
#[tokio::test]
async fn the_admin_policy_covers_admins_by_grant() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for name in ["root-admin", "delegator", "plain"] {
        create_user(&admin, name, "none").await;
    }
    let (status, folder) = admin
        .post("/folders", json!({ "name": "team", "kind": "vm" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{folder}");
    let (status, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "lead", "privileges": ["permission.modify", "vm.power"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let id_of = |name: &str| {
        env.cluster
            .get_by_name::<orbivisor_types::auth::UserSpec>(name)
            .unwrap()
            .unwrap()
            .id
    };
    for (user, role, target) in [
        (
            id_of("root-admin"),
            json!({ "type": "builtin", "role": "admin" }),
            json!({ "type": "root" }),
        ),
        (
            id_of("delegator"),
            json!({ "type": "custom", "id": role["id"] }),
            json!({ "type": "folder", "id": folder["id"] }),
        ),
        (
            id_of("plain"),
            json!({ "type": "builtin", "role": "operator" }),
            json!({ "type": "folder", "id": folder["id"] }),
        ),
    ] {
        let (status, body) = admin
            .post(
                "/grants",
                json!({ "principal": { "type": "user", "id": user }, "role": role, "target": target }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "two_factor": { "roles": ["admin"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for (name, required) in [("root-admin", true), ("delegator", true), ("plain", false)] {
        let body = Browser::new(&env).login(name, "password-1").await;
        assert_eq!(
            body["second_factor"]["enroll"].as_bool().unwrap_or(false),
            required,
            "{name}: {body}"
        );
    }
}

/// 리뷰 2 N10: 요소를 빼거나 더할 때의 본인 확인 코드도 둘째 단계와 같은 사용자별 Raft 시도 수로 세고 막힘을 따른다
/// (다른 노드의 메모리 제한을 피해도).
#[tokio::test]
async fn proof_codes_count_toward_the_lockout() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_user(&admin, "alice", "viewer").await;
    let mut b = Browser::new(&env);
    b.login("alice", "password-1").await;
    let (secret, _) = enable_totp(&mut b).await;
    let right = code(&secret, 1);
    let wrong = format!(
        "{:06}",
        (right.parse::<u32>().unwrap() + 500_000) % 1_000_000
    );
    for _ in 0..5 {
        let (status, err) = b.delete("/auth/2fa/totp", json!({ "code": wrong })).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{err}");
    }
    let tf = env
        .cluster
        .list::<orbivisor_types::two_factor::TwoFactorSpec>()
        .unwrap()
        .1
        .remove(0);
    assert_eq!(tf.status.failures, 5);
    assert!(tf.status.blocked_until > now(), "{:?}", tf.status);
    // 다른 노드(새 메모리 제한)에서 맞는 코드도 막힌 동안은 429다.
    let other = serve_again(&env).await;
    let mut c = Browser::at(&other);
    c.cookie = b.cookie.clone();
    let (status, err) = c.delete("/auth/2fa/totp", json!({ "code": right })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{err}");
    assert_eq!(err["code"], "too_many_attempts");
    let tf = env
        .cluster
        .list::<orbivisor_types::two_factor::TwoFactorSpec>()
        .unwrap()
        .1
        .remove(0);
    assert!(tf.spec.has_factor(), "the factor is still there");
}
