//! OIDC 로그인 흐름(가짜 IdP): `start`의 리디렉트와 상태 쿠키, 콜백의 쿠키 확인(없음·state 다름·만료·
//! 변조), 상태에 담은 리디렉트 URI, IdP 오류, 인가 응답의 iss(RFC 9207), 토큰 끝점·userinfo 실패, ID
//! 토큰과 userinfo의 클레임, 그룹 → 역할, 이름 규칙(이메일 모양), sub 묶기, 공개 클라이언트,
//! client_secret_basic, 두 번째 로그인과 역할 바뀜, 감사 로그, 시도 제한을 쓰지 않음.

mod common;

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::idp::{CLIENT_ID, Idp, SUBJECT};
use common::{ADMIN_PASSWORD, Env, WAIT};
use opl_api::oidc::{LoginState, state_key, unix_now};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};

const CALLBACK: &str = "/api/v1/auth/oidc/callback";

/// opl-admins는 admin, opl-viewers는 viewer
fn realm_body(name: &str, idp: &Idp) -> Value {
    json!({
        "name": name,
        "display": "Okta",
        "backend": idp.backend(),
        "roles": {
            "groups": [
                { "group": "opl-admins", "role": "admin" },
                { "group": "opl-viewers", "role": "viewer" }
            ]
        }
    })
}

async fn add_realm(env: &Env, name: &str, idp: &Idp) {
    let (status, body) = env
        .admin()
        .await
        .post("/realms", realm_body(name, idp))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// 가짜 IdP와 그것을 쓰는 영역 `okta`
async fn setup_with(idp: Idp) -> (Env, Idp) {
    let env = Env::start().await;
    add_realm(&env, "okta", &idp).await;
    (env, idp)
}

async fn setup() -> (Env, Idp) {
    setup_with(Idp::start().await).await
}

/// 리다이렉트를 따르지 않는 브라우저의 응답 하나
struct Hop {
    status: StatusCode,
    location: String,
    /// Set-Cookie 줄 전체
    cookies: Vec<String>,
}

impl Hop {
    /// `name=`으로 시작하는 Set-Cookie 줄
    fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|c| c.starts_with(&format!("{name}=")))
            .map(String::as_str)
    }

    /// `name=value` 부분
    fn pair(&self, name: &str) -> Option<String> {
        self.cookie(name)
            .map(|c| c.split(';').next().unwrap().to_string())
    }
}

async fn open(url: &str, cookie: Option<&str>) -> Hop {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut req = http.get(url);
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    let resp = req.send().await.unwrap();
    Hop {
        status: resp.status(),
        location: resp
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default(),
        cookies: resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect(),
    }
}

async fn start(env: &Env, realm: &str) -> Hop {
    open(
        &format!("{}/api/v1/auth/oidc/{realm}/start", env.base),
        None,
    )
    .await
}

/// `start` → 가짜 `/authorize` → 콜백 주소와 상태 쿠키(`opl_oidc=…`)
async fn authorize(env: &Env, idp: &Idp) -> (String, String) {
    let started = start(env, "okta").await;
    assert_eq!(started.status, StatusCode::FOUND);
    let callback = idp.follow(&started.location).await;
    assert!(
        callback.starts_with(&format!("{}{CALLBACK}?", env.base)),
        "{callback}"
    );
    (callback, started.pair("opl_oidc").unwrap())
}

/// 처음부터 끝까지 로그인한다.
async fn sign_in(env: &Env, idp: &Idp) -> Hop {
    let (callback, cookie) = authorize(env, idp).await;
    open(&callback, Some(&cookie)).await
}

/// 로그인에 성공해 세션 쿠키(`opl_session=…`)를 얻는다.
async fn signed_in(env: &Env, idp: &Idp) -> String {
    let hop = sign_in(env, idp).await;
    assert_eq!(
        (hop.status, hop.location.as_str()),
        (StatusCode::FOUND, "/")
    );
    hop.pair("opl_session").unwrap()
}

/// 실패해 `/login?error=<code>`로 돌아가고 상태 쿠키를 지운다.
fn assert_failed(hop: &Hop, code: &str) {
    assert_eq!(hop.status, StatusCode::FOUND);
    assert_eq!(hop.location, format!("/login?error={code}"));
    assert!(hop.cookie("opl_session").is_none(), "{:?}", hop.cookies);
    assert_eq!(
        hop.cookie("opl_oidc"),
        Some("opl_oidc=; Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age=0")
    );
}

async fn whoami(env: &Env, cookie: &str) -> (StatusCode, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/auth/whoami", env.base))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

fn stored(env: &Env, name: &str) -> Option<Resource> {
    env.cluster.get_by_name::<UserSpec>(name).unwrap()
}

type Resource = opl_types::resource::Resource<UserSpec, opl_types::auth::UserStatus>;

fn query(url: &str) -> std::collections::HashMap<String, String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[tokio::test]
async fn a_login_goes_through_the_identity_provider() {
    let (env, idp) = setup().await;
    let started = start(&env, "okta").await;
    assert_eq!(started.status, StatusCode::FOUND);
    // IdP의 authorization endpoint로, PKCE S256과 state·nonce
    assert!(
        started
            .location
            .starts_with(&format!("{}/authorize?", idp.issuer)),
        "{}",
        started.location
    );
    let q = query(&started.location);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], CLIENT_ID);
    assert_eq!(q["redirect_uri"], format!("{}{CALLBACK}", env.base));
    assert_eq!(q["scope"], "openid profile email");
    assert_eq!(q["code_challenge_method"], "S256");
    for name in ["state", "nonce", "code_challenge"] {
        assert_eq!(q[name].len(), 43, "{name}: {}", q[name]);
    }
    assert_ne!(q["state"], q["nonce"]);
    // 상태 쿠키는 OIDC 경로에만, 10분
    let cookie = started.cookie("opl_oidc").unwrap();
    let (pair, attrs) = cookie.split_once(';').unwrap();
    assert_eq!(
        attrs,
        " Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age=600"
    );

    let callback = idp.follow(&started.location).await;
    let hop = open(&callback, Some(pair)).await;
    assert_eq!(
        (hop.status, hop.location.as_str()),
        (StatusCode::FOUND, "/")
    );
    assert_eq!(
        hop.cookie("opl_oidc"),
        Some("opl_oidc=; Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age=0")
    );
    let session = hop.cookie("opl_session").unwrap();
    assert!(
        session.ends_with("; Path=/; HttpOnly; SameSite=Strict; Max-Age=43200"),
        "{session}"
    );
    let (status, me) = whoami(&env, &hop.pair("opl_session").unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(
        (me["name"].as_str(), me["role"].as_str()),
        (Some("alice@okta"), Some("admin"))
    );
    let user = stored(&env, "alice@okta").unwrap();
    assert_eq!(user.spec.realm.as_deref(), Some("okta"));
    // IdP의 사용자(iss와 sub)에 묶인다.
    assert_eq!(user.spec.subject, Some(format!("{} {SUBJECT}", idp.issuer)));
    assert_eq!(user.spec.password_hash, "");
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 0));

    // 토큰 끝점: 코드와 검증자, client_secret_basic. 클레임이 ID 토큰에 있으므로 userinfo는 읽지 않는다.
    let requests = idp.token_requests();
    assert_eq!(requests.len(), 1);
    let (authorization, form) = &requests[0];
    assert_eq!(
        authorization.as_deref(),
        Some(format!("Basic {}", STANDARD.encode("oplite:client-secret")).as_str())
    );
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["redirect_uri"], format!("{}{CALLBACK}", env.base));
    assert_eq!(form["client_id"], CLIENT_ID);
    assert_eq!(form["code_verifier"].len(), 43);
    assert_eq!(idp.hits("userinfo"), 0);

    // 두 번째 로그인은 쓰지 않는다. discovery·JWKS는 캐시에서.
    signed_in(&env, &idp).await;
    assert_eq!(stored(&env, "alice@okta").unwrap().revision, user.revision);
    assert_eq!((idp.hits("discovery"), idp.hits("jwks")), (1, 1));
}

/// authorization endpoint에 쿼리가 있으면 `&`로 잇는다.
#[tokio::test]
async fn the_authorization_endpoint_keeps_its_query() {
    let (env, idp) = setup().await;
    idp.set_discovery(|d| {
        d["authorization_endpoint"] = json!(format!("{}/authorize?tenant=t1", idp.issuer))
    });
    let started = start(&env, "okta").await;
    assert!(
        started.location.starts_with(&format!(
            "{}/authorize?tenant=t1&response_type=code&",
            idp.issuer
        )),
        "{}",
        started.location
    );
}

#[tokio::test]
async fn starting_fails_for_other_realms_and_unreachable_providers() {
    let (env, idp) = setup().await;
    // 없는 영역, LDAP 영역
    assert_failed_start(&start(&env, "nowhere").await, "sso_failed");
    let (status, body) = env
        .admin()
        .await
        .post(
            "/realms",
            json!({ "name": "corp", "backend": {
                "kind": "ldap", "url": "ldap://127.0.0.1:1", "base_dn": "dc=example,dc=com" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_failed_start(&start(&env, "corp").await, "sso_failed");
    // discovery 실패(issuer 다름, 닿지 않음)
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    assert_failed_start(&start(&env, "okta").await, "realm_unavailable");
    drop(idp);
    assert_failed_start(&start(&env, "okta").await, "realm_unavailable");
}

fn assert_failed_start(hop: &Hop, code: &str) {
    assert_eq!(hop.status, StatusCode::FOUND);
    assert_eq!(hop.location, format!("/login?error={code}"));
    assert!(hop.cookie("opl_oidc").is_none(), "{:?}", hop.cookies);
}

/// Host 헤더가 없거나 이상하면 `start`가 리디렉트 URI를 만들 수 없다. 콜백은 Host를 보지 않는다
/// (리디렉트 URI는 상태 쿠키에 있다).
#[tokio::test]
async fn start_needs_a_valid_host_and_the_callback_does_not() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (env, _idp) = setup().await;
    let addr = env.base.strip_prefix("http://").unwrap();
    for (request, status) in [
        ("GET /api/v1/auth/oidc/okta/start HTTP/1.0\r\n\r\n", "400"),
        (
            "GET /api/v1/auth/oidc/okta/start HTTP/1.0\r\nHost: evil.example.com/x?\r\n\r\n",
            "400",
        ),
        (
            "GET /api/v1/auth/oidc/callback?code=c&state=s HTTP/1.0\r\n\r\n",
            "302",
        ),
    ] {
        let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        tcp.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tcp.read_to_string(&mut response).await.unwrap();
        assert_eq!(
            response.split_whitespace().nth(1),
            Some(status),
            "{request}: {response}"
        );
    }
}

/// 콜백은 `start`가 만든 리디렉트 URI(상태 쿠키 안)로 코드를 바꾼다. 다른 Host로 와도 같다.
#[tokio::test]
async fn the_callback_uses_the_redirect_uri_from_the_state() {
    let (env, idp) = setup().await;
    let (callback, cookie) = authorize(&env, &idp).await;
    let resp = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(&callback)
        .header("host", "other.example.com:8443")
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()["location"], "/");
    let (_, form) = &idp.token_requests()[0];
    assert_eq!(form["redirect_uri"], format!("{}{CALLBACK}", env.base));
}

#[tokio::test]
async fn the_state_cookie_must_be_present_signed_fresh_and_match() {
    let (env, idp) = setup().await;
    let (callback, cookie) = authorize(&env, &idp).await;
    // 쿠키 없음
    assert_failed(&open(&callback, None).await, "sso_failed");
    // state 다름
    let q = query(&callback);
    let other = format!("{}{CALLBACK}?code={}&state=other", env.base, q["code"]);
    assert_failed(&open(&other, Some(&cookie)).await, "sso_failed");
    // 바뀐 바이트
    let mut bytes = cookie.clone().into_bytes();
    let i = bytes.len() - 5;
    bytes[i] = if bytes[i] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(bytes).unwrap();
    assert_failed(&open(&callback, Some(&tampered)).await, "sso_failed");
    // 다른 클러스터의 키로 서명한 쿠키, 10분이 지난 쿠키
    let ca_key = env.cluster.list::<ClusterSpec>().unwrap().1[0]
        .spec
        .ca_key_pem
        .clone();
    let forged = |key: &str, created: i64| {
        let state = LoginState {
            realm: "okta".into(),
            state: q["state"].clone(),
            nonce: "n".into(),
            verifier: "v".into(),
            redirect_uri: format!("{}{CALLBACK}", env.base),
            created,
        };
        format!("opl_oidc={}", state.seal(&state_key(key)))
    };
    assert_failed(
        &open(&callback, Some(&forged("other", unix_now()))).await,
        "sso_failed",
    );
    assert_failed(
        &open(&callback, Some(&forged(&ca_key, unix_now() - 601))).await,
        "sso_failed",
    );
    // 여기까지는 토큰 끝점에 가지 않았다.
    assert_eq!(idp.hits("token"), 0);
    assert!(stored(&env, "alice@okta").is_none());
    // 같은 키로 서명한 새 쿠키는 확인을 지난다(검증자가 달라 IdP가 invalid_grant로 거절한다).
    assert_failed(
        &open(&callback, Some(&forged(&ca_key, unix_now()))).await,
        "sso_failed",
    );
    assert_eq!(idp.hits("token"), 1);
    // 그 코드는 IdP가 이미 썼다. 새로 시작하면 된다.
    let (callback, cookie) = authorize(&env, &idp).await;
    let hop = open(&callback, Some(&cookie)).await;
    assert_eq!(hop.location, "/");
}

#[tokio::test]
async fn an_error_from_the_identity_provider_fails_the_login() {
    let (env, idp) = setup().await;
    idp.set_authorize_error(Some("access_denied"));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert_eq!(idp.hits("token"), 0);
}

/// 토큰 끝점의 invalid_grant(코드가 틀림·씀·만료)는 sso_failed, 그 밖의 4xx·5xx·연결 오류는
/// realm_unavailable
#[tokio::test]
async fn token_endpoint_failures() {
    let (env, idp) = setup().await;
    idp.fail_token(Some((500, json!({ "error": "server_error" }))));
    assert_failed(&sign_in(&env, &idp).await, "realm_unavailable");
    idp.fail_token(Some((400, json!({ "error": "invalid_grant" }))));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    idp.fail_token(Some((401, json!({ "error": "invalid_client" }))));
    assert_failed(&sign_in(&env, &idp).await, "realm_unavailable");
    idp.fail_token(Some((400, json!({ "error": "invalid_request" }))));
    assert_failed(&sign_in(&env, &idp).await, "realm_unavailable");
    // 콜백 전에 IdP가 사라졌다.
    idp.fail_token(None);
    let (callback, cookie) = authorize(&env, &idp).await;
    drop(idp);
    assert_failed(&open(&callback, Some(&cookie)).await, "realm_unavailable");
}

#[tokio::test]
async fn the_id_token_must_carry_the_nonce() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "nonce": "someone-elses" }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    idp.set_claims(json!({ "nonce": null }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert!(stored(&env, "alice@okta").is_none());
}

/// userinfo가 액세스 토큰을 받지 않으면(401·403) sso_failed, 그 밖의 실패는 realm_unavailable
#[tokio::test]
async fn userinfo_failures() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "preferred_username": null }));
    for (status, code) in [
        (401, "sso_failed"),
        (403, "sso_failed"),
        (500, "realm_unavailable"),
    ] {
        idp.fail_userinfo(Some(status));
        assert_failed(&sign_in(&env, &idp).await, code);
    }
    assert!(stored(&env, "alice@okta").is_none());
}

/// 콜백에서도 discovery 실패(문서가 맞지 않음 포함)는 start처럼 realm_unavailable이다.
#[tokio::test]
async fn discovery_failures_at_the_callback_mean_the_realm_is_unavailable() {
    let (env, idp) = setup().await;
    let (callback, cookie) = authorize(&env, &idp).await;
    // 영역을 고쳐(revision) 콜백이 discovery를 새로 받게 하고, IdP의 문서가 다른 issuer다.
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    let admin = env.admin().await;
    let (_, realm) = admin.get("/realms/okta").await;
    let mut body = realm_body("okta", &idp);
    body["display"] = json!("Okta 2");
    let (status, updated) = admin
        .put("/realms/okta", body, realm["revision"].as_u64())
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_failed(&open(&callback, Some(&cookie)).await, "realm_unavailable");
    assert_eq!(idp.hits("token"), 0);
}

/// RFC 9207: IdP가 인가 응답의 iss를 지원한다고 하면 콜백의 iss가 있어야 하고 issuer와 같아야 한다.
/// 지원하지 않아도 iss가 오면 같아야 한다.
#[tokio::test]
async fn the_authorization_response_issuer_is_checked() {
    let (env, idp) = setup().await;
    // 지원하지 않음: 없으면 된다, 다르면 거절
    signed_in(&env, &idp).await;
    idp.set_authorize_iss(Some("https://evil.example.com"));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    let issuer = idp.issuer.clone();
    idp.set_authorize_iss(Some(&issuer));
    signed_in(&env, &idp).await;

    // 지원함(새 영역 — discovery를 새로 받음)
    let (env, idp) = setup().await;
    idp.set_discovery(|d| d["authorization_response_iss_parameter_supported"] = json!(true));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    idp.set_authorize_iss(Some("https://evil.example.com"));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    let issuer = idp.issuer.clone();
    idp.set_authorize_iss(Some(&issuer));
    signed_in(&env, &idp).await;
    // 토큰 끝점에 가기 전에 거절했다.
    assert_eq!(idp.token_requests().len(), 1);
}

#[tokio::test]
async fn missing_claims_are_read_from_userinfo() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "preferred_username": null, "groups": null }));
    idp.set_userinfo(json!({
        "sub": SUBJECT,
        "preferred_username": "Bob",
        "groups": ["opl-viewers"],
    }));
    let session = signed_in(&env, &idp).await;
    assert_eq!(idp.hits("userinfo"), 1);
    let (_, me) = whoami(&env, &session).await;
    assert_eq!(
        (me["name"].as_str(), me["role"].as_str()),
        (Some("bob@okta"), Some("viewer"))
    );
    // 이름만 없으면 userinfo의 이름과 ID 토큰의 그룹
    idp.set_claims(json!({ "groups": ["opl-admins"] }));
    let session = signed_in(&env, &idp).await;
    assert_eq!(whoami(&env, &session).await.1["role"], "admin");
    assert_eq!(idp.hits("userinfo"), 2);
    // userinfo의 sub가 다르면 다른 사람이다.
    idp.set_userinfo(json!({ "sub": "mallory", "preferred_username": "Bob" }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    // userinfo 끝점이 없고 이름이 없다.
    let (env, idp) = setup().await;
    idp.set_discovery(|d| {
        d.as_object_mut().unwrap().remove("userinfo_endpoint");
    });
    idp.set_claims(json!({ "preferred_username": null }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert_eq!(idp.hits("userinfo"), 0);
}

#[tokio::test]
async fn groups_without_a_role_fail_with_no_role() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "groups": ["nobody"] }));
    assert_failed(&sign_in(&env, &idp).await, "no_role");
    // 그룹이 없고 userinfo에도 없다.
    idp.set_claims(json!({ "groups": null }));
    idp.set_userinfo(json!({ "sub": SUBJECT }));
    assert_failed(&sign_in(&env, &idp).await, "no_role");
    // 그룹 클레임이 문자열 배열이 아니다.
    idp.set_claims(json!({ "groups": "opl-admins" }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert!(stored(&env, "alice@okta").is_none());
}

#[tokio::test]
async fn usernames_must_follow_the_external_name_rules() {
    let (env, idp) = setup().await;
    for bad in [
        json!("alice smith"),
        json!("a@b@c"),
        json!("@corp.com"),
        json!("a/b"),
        json!(""),
        json!(7),
    ] {
        idp.set_claims(json!({ "preferred_username": bad }));
        assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    }
    // Okta·Entra ID의 이메일 모양 이름은 그대로(소문자로) 쓴다.
    idp.set_claims(json!({ "preferred_username": "Alice.Smith+ops@Corp.com" }));
    let session = signed_in(&env, &idp).await;
    let (_, me) = whoami(&env, &session).await;
    assert_eq!(me["name"], "alice.smith+ops@corp.com@okta");
    let found = audit_entries(&env, |e| {
        e.iter()
            .any(|e| e.detail.as_deref() == Some("realm=okta username=alice.smith+ops@corp.com"))
    })
    .await;
    assert!(!found.is_empty());
    // 다른 클레임을 이름으로 쓰는 영역
    let (status, realm) = env.admin().await.get("/realms/okta").await;
    assert_eq!(status, StatusCode::OK, "{realm}");
    let mut body = realm_body("okta", &idp);
    body["backend"]["username_claim"] = json!("email_local");
    let (status, updated) = env
        .admin()
        .await
        .put("/realms/okta", body, realm["revision"].as_u64())
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    idp.set_claims(json!({ "email_local": "Alice.S" }));
    let session = signed_in(&env, &idp).await;
    assert_eq!(whoami(&env, &session).await.1["name"], "alice.s@okta");
}

/// 공개 클라이언트는 비밀 없이 PKCE만으로 로그인한다.
#[tokio::test]
async fn public_clients_sign_in_with_pkce_only() {
    let (env, idp) = setup_with(Idp::start_with(CLIENT_ID, None).await).await;
    signed_in(&env, &idp).await;
    let (authorization, form) = &idp.token_requests()[0];
    assert_eq!(*authorization, None);
    assert_eq!(form["client_id"], CLIENT_ID);
    assert!(form.contains_key("code_verifier"));
    assert!(!form.contains_key("client_secret"));
}

/// client_secret_basic은 client_id와 비밀을 각각 form-urlencode한다(RFC 6749 §2.3.1).
#[tokio::test]
async fn client_secret_basic_encodes_the_id_and_the_secret() {
    let (env, idp) = setup_with(Idp::start_with("opl:ite", Some("s3cr:t+/ é")).await).await;
    signed_in(&env, &idp).await;
    let (authorization, _) = &idp.token_requests()[0];
    assert_eq!(
        authorization.as_deref(),
        Some(
            format!(
                "Basic {}",
                STANDARD.encode("opl%3Aite:s3cr%3At%2B%2F+%C3%A9")
            )
            .as_str()
        )
    );
}

#[tokio::test]
async fn a_changed_group_changes_the_role_and_ends_old_sessions() {
    let (env, idp) = setup().await;
    idp.set_claims(json!({ "groups": ["opl-viewers"] }));
    let old = signed_in(&env, &idp).await;
    assert_eq!(whoami(&env, &old).await.1["role"], "viewer");
    idp.set_claims(json!({ "groups": ["opl-viewers", "opl-admins"] }));
    let new = signed_in(&env, &idp).await;
    let user = stored(&env, "alice@okta").unwrap();
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 1));
    assert_eq!(whoami(&env, &new).await.1["role"], "admin");
    assert_eq!(whoami(&env, &old).await.0, StatusCode::UNAUTHORIZED);
}

/// 외부 사용자는 처음 만든 IdP 사용자(iss·sub)에 묶인다. 같은 이름의 다른 사람은 거절한다.
#[tokio::test]
async fn users_are_bound_to_their_subject() {
    let (env, idp) = setup().await;
    signed_in(&env, &idp).await;
    let user = stored(&env, "alice@okta").unwrap();
    // 이름을 다시 쓴 다른 사람(다른 sub)
    idp.set_claims(json!({ "sub": "mallory-sub", "groups": ["opl-viewers"] }));
    assert_failed(&sign_in(&env, &idp).await, "sso_failed");
    assert_eq!(stored(&env, "alice@okta").unwrap(), user);
    // 같은 sub면 된다.
    idp.set_claims(json!({ "sub": SUBJECT, "groups": ["opl-admins"] }));
    signed_in(&env, &idp).await;
    assert_eq!(stored(&env, "alice@okta").unwrap().revision, user.revision);
}

/// subject 없이 만든 사용자(옛 데이터)는 이번 로그인의 subject를 적는다(역할이 같으면 세션은 그대로).
#[tokio::test]
async fn users_without_a_subject_get_one() {
    let (env, idp) = setup().await;
    let old: Resource = opl_types::resource::Resource::new(
        "alice@okta",
        UserSpec {
            password_hash: String::new(),
            role: Role::Admin,
            session_epoch: 0,
            realm: Some("okta".into()),
            subject: None,
        },
    );
    env.cluster.create(&old).await.unwrap();
    signed_in(&env, &idp).await;
    let user = stored(&env, "alice@okta").unwrap();
    assert_eq!(user.id, old.id);
    assert_eq!(user.spec.subject, Some(format!("{} {SUBJECT}", idp.issuer)));
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 0));
}

async fn audit_entries(env: &Env, enough: impl Fn(&[AuditEntry]) -> bool) -> Vec<AuditEntry> {
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
        if enough(&entries) {
            return entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn callbacks_are_audited() {
    let (env, idp) = setup().await;
    signed_in(&env, &idp).await;
    idp.set_claims(json!({ "groups": ["nobody"] }));
    assert_failed(&sign_in(&env, &idp).await, "no_role");
    assert_failed(
        &open(&format!("{}{CALLBACK}?code=c&state=s", env.base), None).await,
        "sso_failed",
    );
    let callbacks = |entries: &[AuditEntry]| {
        entries
            .iter()
            .filter(|e| e.action == "GET /auth/oidc/callback")
            .cloned()
            .collect::<Vec<_>>()
    };
    let entries = audit_entries(&env, |e| callbacks(e).len() >= 3).await;
    let found = callbacks(&entries);
    let detail = |detail: &str| {
        found
            .iter()
            .find(|e| e.detail.as_deref() == Some(detail))
            .unwrap_or_else(|| panic!("{detail}: {found:#?}"))
    };
    let ok = detail("realm=okta username=alice");
    assert_eq!(ok.user.as_deref(), Some("alice@okta"));
    assert_eq!(ok.status, 302);
    assert_eq!(detail("realm=okta error=no_role").user, None);
    assert_eq!(detail("realm=<unknown> error=sso_failed").user, None);
    // 경로는 쿼리를 뺀 것이다. start는 적지 않는다. 코드·state·쿠키·토큰은 어디에도 남지 않는다.
    assert!(
        found.iter().all(|e| e.path == "/auth/oidc/callback"),
        "{found:#?}"
    );
    assert!(!entries.iter().any(|e| e.action.contains("/start")));
    let text = serde_json::to_string(&entries).unwrap();
    for secret in ["code=", "state=", "client-secret", "opl_oidc"] {
        assert!(!text.contains(secret), "{secret}: {text}");
    }
}

/// 콜백은 시도 제한을 쓰지 않는다(맞힐 비밀이 없다 — state는 서명됐고 코드는 한 번 쓰며 PKCE에 묶인다).
/// 실패한 콜백이 많아도 같은 주소의 비밀번호 로그인과 SSO는 막히지 않는다.
#[tokio::test]
async fn failed_callbacks_do_not_block_logins() {
    let (env, idp) = setup().await;
    let url = format!("{}{CALLBACK}?code=c&state=s", env.base);
    for _ in 0..25 {
        assert_failed(&open(&url, None).await, "sso_failed");
    }
    signed_in(&env, &idp).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&json!({ "username": "admin", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
