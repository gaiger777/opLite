//! 앞단 프록시와 따로 띄운 웹 포털: 믿는 프록시(`trusted_proxies`)의 `X-Forwarded-*` 헤더(요청 주소, 같은
//! 출처 검사), 허용 출처(`web_origins`)의 CORS와 세션 쿠키.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{ADMIN_PASSWORD, Env, WAIT};
use opl_api::{AppState, AuditConfig, AuditLog};
use opl_types::audit::{AuditEntry, AuditQuery};
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::json;

const PORTAL: &str = "https://portal.example.com";

/// 같은 클러스터를 따로 서비스한다. `proxies`는 믿는 프록시, 감사 로그는 `{dir}/audit-<이름>`이다.
async fn serve(env: &Env, name: &str, proxies: &[&str]) -> (String, Arc<AuditLog>) {
    let mut state = AppState::new(
        env.cluster.clone(),
        env.node,
        env.hv.clone(),
        env.dir.path().join("uploads"),
    );
    state.trusted_proxies = proxies.iter().map(|p| p.parse().unwrap()).collect();
    let audit = Arc::new(
        AuditLog::open(AuditConfig {
            dir: env.dir.path().join(format!("audit-{name}")),
            node: name.into(),
            max_file_size: 1 << 20,
            max_files: 2,
        })
        .unwrap(),
    );
    state.audit = audit.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(Arc::new(state));
    tokio::spawn(async move {
        // opld처럼 연결 주소를 넘긴다.
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (base, audit)
}

/// `base`에 헤더 `headers`를 붙여 로그인한다.
async fn login(base: &str, password: &str, headers: &[(&'static str, &str)]) -> reqwest::Response {
    let mut r = reqwest::Client::new()
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": password }));
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    r.send().await.unwrap()
}

fn set_cookie(headers: &HeaderMap) -> String {
    headers["set-cookie"].to_str().unwrap().to_string()
}

fn cookie_of(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

/// 쿠키 `cookie`와 헤더 `headers`로 whoami를 부른다.
async fn whoami(base: &str, cookie: &str, headers: &[(&'static str, &str)]) -> reqwest::Response {
    let mut r = reqwest::Client::new()
        .get(format!("{base}/api/v1/auth/whoami"))
        .header("cookie", cookie);
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    r.send().await.unwrap()
}

/// 실패한 로그인의 감사 항목들이 적히기를 기다린다.
async fn failed_logins(audit: &AuditLog, n: usize) -> Vec<AuditEntry> {
    let start = Instant::now();
    loop {
        let entries: Vec<AuditEntry> = audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.action == "POST /auth/login" && e.status == 401)
            .collect();
        if entries.len() >= n {
            return entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 믿는 프록시가 보낸 `X-Forwarded-For`의 주소가 요청 주소(감사 로그, 로그인 제한)다. 믿지 않는 연결은
/// 헤더를 무시한다.
#[tokio::test]
async fn forwarded_for_is_the_client_only_behind_a_trusted_proxy() {
    let env = Env::start().await;
    let (trusting, trusting_audit) = serve(&env, "trusting", &["127.0.0.0/8"]).await;
    let (plain, plain_audit) = serve(&env, "plain", &[]).await;
    // 클라이언트가 지어낸 왼쪽 항목은 믿지 않는다.
    let xff = [("x-forwarded-for", "198.51.100.1, 203.0.113.7")];
    for base in [&trusting, &plain] {
        let resp = login(base, "wrong-password", &xff).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(failed_logins(&trusting_audit, 1).await[0].ip, "203.0.113.7");
    assert_eq!(failed_logins(&plain_audit, 1).await[0].ip, "127.0.0.1");

    // 로그인 제한도 그 주소로 센다: 한 클라이언트가 막혀도 같은 프록시 뒤의 다른 클라이언트는 된다.
    for _ in 0..4 {
        login(&trusting, "wrong-password", &xff).await;
    }
    let resp = login(&trusting, ADMIN_PASSWORD, &xff).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let other = [("x-forwarded-for", "203.0.113.8")];
    let resp = login(&trusting, ADMIN_PASSWORD, &other).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 믿는 프록시가 알린 `X-Forwarded-Host`·`X-Forwarded-Proto`로 같은 출처를 검사한다.
#[tokio::test]
async fn forwarded_host_and_proto_decide_the_same_origin() {
    let env = Env::start().await;
    let (trusting, _) = serve(&env, "trusting", &["127.0.0.1"]).await;
    let (plain, _) = serve(&env, "plain", &[]).await;
    let resp = login(&trusting, ADMIN_PASSWORD, &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = cookie_of(&set_cookie(resp.headers()));
    let behind = |proto| {
        [
            ("origin", "https://orbivisor.example.com"),
            ("x-forwarded-host", "orbivisor.example.com"),
            ("x-forwarded-proto", proto),
        ]
    };
    assert_eq!(
        whoami(&trusting, &cookie, &behind("https")).await.status(),
        StatusCode::OK
    );
    // 프록시가 http로 받았다고 알렸으면 https 출처는 다른 출처다.
    assert_eq!(
        whoami(&trusting, &cookie, &behind("http")).await.status(),
        StatusCode::FORBIDDEN
    );
    // 믿지 않는 연결의 헤더는 보지 않는다(Host는 127.0.0.1:포트).
    assert_eq!(
        whoami(&plain, &cookie, &behind("https")).await.status(),
        StatusCode::FORBIDDEN
    );
}

/// 허용 출처: 관리자가 설정하고, 그 출처의 preflight와 쿠키 요청에 CORS로 답하며, 그 출처에서 로그인하면
/// 세션 쿠키가 `SameSite=None; Secure`다. 다른 출처는 여전히 막는다.
#[tokio::test]
async fn web_origins_accept_cookie_requests_from_the_portal() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let portal = [("origin", PORTAL)];
    let evil = [("origin", "https://evil.example")];

    // 설정 전에는 포털도 다른 출처다.
    let resp = login(&env.base, ADMIN_PASSWORD, &portal).await;
    assert!(set_cookie(resp.headers()).contains("SameSite=Strict"));
    assert!(resp.headers().get("access-control-allow-origin").is_none());
    let cookie = cookie_of(&set_cookie(resp.headers()));
    assert_eq!(
        whoami(&env.base, &cookie, &portal).await.status(),
        StatusCode::FORBIDDEN
    );

    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "web_origins": ["https://portal.example.com/ui"] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let viewer = env.user("v", "viewer").await;
    let (status, _) = viewer
        .patch("/settings", json!({ "web_origins": [PORTAL] }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "web_origins": ["HTTPS://Portal.example.com:443"] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["web_origins"], json!([PORTAL]));

    // preflight
    let preflight = |origin: &'static str| {
        reqwest::Client::new()
            .request(reqwest::Method::OPTIONS, format!("{}/api/v1/vms", env.base))
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .send()
    };
    let resp = preflight(PORTAL).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let h = resp.headers();
    assert_eq!(h["access-control-allow-origin"], PORTAL);
    assert_eq!(h["access-control-allow-credentials"], "true");
    assert!(
        h["access-control-allow-methods"]
            .to_str()
            .unwrap()
            .contains("POST")
    );
    assert_eq!(h["access-control-allow-headers"], "content-type");
    let resp = preflight("https://evil.example").await.unwrap();
    assert!(resp.headers().get("access-control-allow-origin").is_none());

    // 포털에서 로그인하면 쿠키가 다른 사이트의 요청에도 붙는다.
    let resp = login(&env.base, ADMIN_PASSWORD, &portal).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["access-control-allow-origin"], PORTAL);
    let portal_cookie = set_cookie(resp.headers());
    assert!(portal_cookie.contains("SameSite=None"), "{portal_cookie}");
    assert!(portal_cookie.contains("Secure"), "{portal_cookie}");
    let cookie = cookie_of(&portal_cookie);
    let resp = whoami(&env.base, &cookie, &portal).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["access-control-allow-origin"], PORTAL);
    assert_eq!(resp.headers()["access-control-allow-credentials"], "true");
    let resp = whoami(&env.base, &cookie, &evil).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(resp.headers().get("access-control-allow-origin").is_none());
    // 같은 출처의 로그인은 지금처럼 Strict다.
    let resp = login(&env.base, ADMIN_PASSWORD, &[]).await;
    assert!(set_cookie(resp.headers()).contains("SameSite=Strict"));

    // 포털의 로그아웃은 같은 속성으로 쿠키를 지운다.
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/logout", env.base))
        .header("cookie", &cookie)
        .header("origin", PORTAL)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let cleared = set_cookie(resp.headers());
    assert!(
        cleared.contains("Max-Age=0") && cleared.contains("SameSite=None"),
        "{cleared}"
    );

    // 지우면 다시 같은 출처만
    let cookie = cookie_of(&set_cookie(
        login(&env.base, ADMIN_PASSWORD, &portal).await.headers(),
    ));
    let (status, body) = admin
        .patch("/settings", json!({ "web_origins": [] }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("web_origins").is_none(), "{body}");
    assert_eq!(
        whoami(&env.base, &cookie, &portal).await.status(),
        StatusCode::FORBIDDEN
    );
}

/// 쿠키 `cookie`와 헤더 `headers`로 `method path`를 부른 상태와 오류 코드(`code`, 없으면 빈 문자열)
async fn call(
    base: &str,
    method: reqwest::Method,
    path: &str,
    cookie: &str,
    headers: &[(&'static str, &str)],
) -> (StatusCode, String) {
    let mut r = reqwest::Client::new()
        .request(method, format!("{base}/api/v1{path}"))
        .header("cookie", cookie);
    if path == "/settings" {
        r = r.json(&json!({}));
    }
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    let resp = r.send().await.unwrap();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    (
        status,
        body["code"].as_str().unwrap_or_default().to_string(),
    )
}

/// 로그아웃도 쿠키 요청처럼 출처를 본다. 허용 출처를 쓰면(쿠키가 `SameSite=None`일 수 있다) `Origin`이 없는
/// 쓰기 요청과 WebSocket 업그레이드는 `Sec-Fetch-Site`가 다른 사이트라고 알리면 받지 않는다.
#[tokio::test]
async fn cross_site_cookie_writes_and_logouts_are_refused() {
    use reqwest::Method;
    let env = Env::start().await;
    let admin = env.admin().await;
    let cookie = cookie_of(&set_cookie(
        login(&env.base, ADMIN_PASSWORD, &[]).await.headers(),
    ));
    let cross = (StatusCode::FORBIDDEN, "cross_origin".to_string());

    // 다른 출처의 로그아웃은 세션을 끝내지 않는다.
    let evil = [("origin", "https://evil.example")];
    assert_eq!(
        call(&env.base, Method::POST, "/auth/logout", &cookie, &evil).await,
        cross
    );
    assert_eq!(
        whoami(&env.base, &cookie, &[]).await.status(),
        StatusCode::OK
    );

    // 허용 출처가 없으면 `Origin` 없는 쓰기는 지금처럼 받는다(브라우저 밖의 클라이언트).
    let (status, code) = call(&env.base, Method::PATCH, "/settings", &cookie, &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{code}");
    let (status, body) = admin
        .patch("/settings", json!({ "web_origins": [PORTAL] }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 두 헤더가 다 없으면 브라우저 밖의 클라이언트다.
    let (status, code) = call(&env.base, Method::PATCH, "/settings", &cookie, &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{code}");
    let cross_site = [("sec-fetch-site", "cross-site")];
    assert_eq!(
        call(&env.base, Method::PATCH, "/settings", &cookie, &cross_site).await,
        cross
    );
    for site in ["same-origin", "same-site", "none"] {
        let (status, code) = call(
            &env.base,
            Method::PATCH,
            "/settings",
            &cookie,
            &[("sec-fetch-site", site)],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{site}: {code}");
    }
    // 읽기는 받고, WebSocket 업그레이드는 막는다.
    assert_eq!(
        whoami(&env.base, &cookie, &[]).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        call(
            &env.base,
            Method::GET,
            "/watch",
            &cookie,
            &[
                ("upgrade", "websocket"),
                ("connection", "upgrade"),
                ("sec-fetch-site", "cross-site")
            ]
        )
        .await,
        cross
    );
    // `Origin` 없는 로그아웃도 같다.
    assert_eq!(
        call(
            &env.base,
            Method::POST,
            "/auth/logout",
            &cookie,
            &cross_site
        )
        .await,
        cross
    );
    let (status, _) = call(
        &env.base,
        Method::POST,
        "/auth/logout",
        &cookie,
        &[("sec-fetch-site", "same-origin")],
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        whoami(&env.base, &cookie, &[]).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// 허용 출처를 둔 채로도 orbivisorctl 방식(쿠키 로그인 → `POST /tokens` → 로그아웃, `Origin`·
/// `Sec-Fetch-Site` 없음)이 된다.
#[tokio::test]
async fn cli_login_works_with_web_origins() {
    use reqwest::Method;
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .patch("/settings", json!({ "web_origins": [PORTAL] }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let resp = login(&env.base, ADMIN_PASSWORD, &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = cookie_of(&set_cookie(resp.headers()));
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/tokens", env.base))
        .header("cookie", &cookie)
        .json(&json!({ "name": "orbivisorctl-test" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let secret = resp.json::<serde_json::Value>().await.unwrap()["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = call(&env.base, Method::POST, "/auth/logout", &cookie, &[]).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        whoami(&env.base, &cookie, &[]).await.status(),
        StatusCode::UNAUTHORIZED
    );
    // 만든 토큰으로 이어서 부른다.
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/auth/whoami", env.base))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
