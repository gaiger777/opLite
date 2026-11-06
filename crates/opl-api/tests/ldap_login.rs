//! LDAP 영역 로그인과 외부 사용자(가짜 LDAP 서버): 첫 로그인에 사용자가 생기고 다음 로그인은
//! 쓰지 않음, 그룹이 바뀌면 역할과 옛 세션, 매핑 없음·디렉터리 없음·모르는 영역, 시도 제한,
//! 외부 사용자의 비밀번호, 영역을 지우면 세션·토큰이 멈춤, 감사 detail, 외부 사용자의 API 토큰.

mod common;

use std::time::{Duration, Instant};

use common::ldap::{BASE_DN, Directory, FakeLdap, Mode, Person, SERVICE_DN, SERVICE_PASSWORD};
use common::{ADMIN_PASSWORD, Client, Env, WAIT};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};

const ADMINS: &str = "cn=admins,ou=groups,dc=example,dc=com";
const STAFF: &str = "cn=staff,ou=groups,dc=example,dc=com";

/// alice는 admin, bob은 viewer, carol은 매핑되는 그룹이 없다.
async fn directory() -> FakeLdap {
    FakeLdap::start(
        Mode::StartTls,
        Directory::with(vec![
            Person::new("alice", "alice-pw", &[ADMINS, STAFF]),
            Person::new("bob", "bob-pw", &[STAFF]),
            Person::new("carol", "carol-pw", &["cn=other,dc=example,dc=com"]),
        ]),
    )
    .await
}

fn corp(ldap: &FakeLdap) -> Value {
    json!({
        "name": "corp",
        "backend": {
            "kind": "ldap",
            "url": ldap.url,
            "bind_dn": SERVICE_DN,
            "bind_password": SERVICE_PASSWORD,
            "base_dn": BASE_DN,
            "ca_pem": ldap.ca_pem,
        },
        "roles": {
            "groups": [
                { "group": STAFF, "role": "viewer" },
                { "group": ADMINS, "role": "admin" }
            ]
        }
    })
}

/// 가짜 디렉터리와 그것을 쓰는 영역 `corp`
async fn setup() -> (Env, FakeLdap) {
    let env = Env::start().await;
    let ldap = directory().await;
    let (status, body) = env.admin().await.post("/realms", corp(&ldap)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (env, ldap)
}

struct Login {
    status: StatusCode,
    body: Value,
    /// `opl_session=…`
    cookie: Option<String>,
}

async fn login(env: &Env, user: &str, password: &str, realm: Option<&str>) -> Login {
    let mut body = json!({ "username": user, "password": password });
    if let Some(realm) = realm {
        body["realm"] = json!(realm);
    }
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    Login {
        status,
        body: resp.json().await.unwrap(),
        cookie,
    }
}

/// 영역 `corp`로 로그인하고 세션 쿠키를 돌려준다.
async fn corp_login(env: &Env, user: &str, password: &str) -> (Value, String) {
    let got = login(env, user, password, Some("corp")).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    (got.body, got.cookie.unwrap())
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

fn assert_error(got: &Login, status: StatusCode, code: &str) {
    assert_eq!(got.status, status, "{}", got.body);
    assert_eq!(got.body["code"], code, "{}", got.body);
    assert!(got.cookie.is_none());
}

fn stored(env: &Env, name: &str) -> Option<Resource<UserSpec, opl_types::auth::UserStatus>> {
    env.cluster.get_by_name::<UserSpec>(name).unwrap()
}

#[tokio::test]
async fn the_first_login_creates_the_external_user_and_later_logins_do_not_write() {
    let (env, _ldap) = setup().await;
    let (body, cookie) = corp_login(&env, "alice", "alice-pw").await;
    assert_eq!(body["name"], "alice@corp");
    assert_eq!(body["role"], "admin");
    assert_eq!(body["realm"], "corp");
    let (status, me) = whoami(&env, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(
        (me["name"].as_str(), me["role"].as_str()),
        (Some("alice@corp"), Some("admin"))
    );

    let user = stored(&env, "alice@corp").unwrap();
    assert_eq!(user.spec.realm.as_deref(), Some("corp"));
    assert_eq!(user.spec.password_hash, "");
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 0));

    // 다시 로그인해도(대문자도 같은 사용자) 쓰지 않는다.
    let (body, _) = corp_login(&env, "Alice", "alice-pw").await;
    assert_eq!(body["name"], "alice@corp");
    assert_eq!(stored(&env, "alice@corp").unwrap().revision, user.revision);

    // 사용자 목록에 영역이 보인다(로컬 사용자에는 없다).
    let (_, users) = env.admin().await.get("/users").await;
    let items = users["items"].as_array().unwrap();
    let alice = items.iter().find(|u| u["name"] == "alice@corp").unwrap();
    assert_eq!(alice["realm"], "corp");
    let admin = items.iter().find(|u| u["name"] == "admin").unwrap();
    assert!(admin.get("realm").is_none(), "{admin}");
}

#[tokio::test]
async fn a_changed_group_changes_the_role_and_ends_old_sessions() {
    let (env, ldap) = setup().await;
    let (body, old) = corp_login(&env, "bob", "bob-pw").await;
    assert_eq!(body["role"], "viewer");
    assert_eq!(whoami(&env, &old).await.0, StatusCode::OK);

    ldap.set_groups("bob", &[ADMINS]);
    let (body, new) = corp_login(&env, "bob", "bob-pw").await;
    assert_eq!(body["role"], "admin");
    let user = stored(&env, "bob@corp").unwrap();
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 1));
    assert_eq!(whoami(&env, &new).await.1["role"], "admin");
    // 옛 역할의 세션은 끝났다.
    assert_eq!(whoami(&env, &old).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn failed_realm_logins_give_distinct_errors() {
    let (env, ldap) = setup().await;

    // 매핑되는 그룹이 없다(사용자를 만들지 않는다).
    let got = login(&env, "carol", "carol-pw", Some("corp")).await;
    assert_error(&got, StatusCode::FORBIDDEN, "no_role");
    assert!(stored(&env, "carol@corp").is_none());

    let got = login(&env, "alice", "wrong", Some("corp")).await;
    assert_error(&got, StatusCode::UNAUTHORIZED, "invalid_credentials");
    assert_eq!(got.body["message"], "wrong username or password");
    // 외부 사용자 이름이 될 수 없다.
    for user in ["no one", "a@b", ""] {
        let got = login(&env, user, "alice-pw", Some("corp")).await;
        assert_error(&got, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    let got = login(&env, "alice", "alice-pw", Some("nowhere")).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(stored(&env, "alice@corp").is_none());

    // `local`은 로컬 사용자다.
    assert_eq!(
        login(&env, "admin", ADMIN_PASSWORD, Some("local"))
            .await
            .status,
        StatusCode::OK
    );

    // 외부 사용자는 로컬 로그인 경로로 들어오지 못한다.
    corp_login(&env, "alice", "alice-pw").await;
    for realm in [None, Some("local")] {
        let got = login(&env, "alice@corp", "alice-pw", realm).await;
        assert_error(&got, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }

    // 디렉터리에 닿지 못한다.
    drop(ldap);
    let got = login(&env, "alice", "alice-pw", Some("corp")).await;
    assert_error(&got, StatusCode::SERVICE_UNAVAILABLE, "realm_unavailable");
    assert_eq!(
        got.body["message"],
        "the directory of realm corp is unavailable"
    );
}

#[tokio::test]
async fn realm_logins_are_throttled_by_the_external_name() {
    let (env, _ldap) = setup().await;
    for i in 0..5 {
        let got = login(&env, "alice", "wrong", Some("corp")).await;
        assert_eq!(got.status, StatusCode::UNAUTHORIZED, "{i}");
    }
    let got = login(&env, "alice", "alice-pw", Some("corp")).await;
    assert_error(&got, StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
    // 대문자로 바꿔도 같은 키다.
    let got = login(&env, "ALICE", "alice-pw", Some("corp")).await;
    assert_error(&got, StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
    // 다른 사용자와 로컬 사용자는 막히지 않는다.
    corp_login(&env, "bob", "bob-pw").await;
    assert_eq!(
        login(&env, "admin", ADMIN_PASSWORD, None).await.status,
        StatusCode::OK
    );
}

/// 매핑되는 역할이 없는 로그인도 실패로 센다.
#[tokio::test]
async fn logins_without_a_role_are_throttled() {
    let (env, _ldap) = setup().await;
    for _ in 0..5 {
        let got = login(&env, "carol", "carol-pw", Some("corp")).await;
        assert_error(&got, StatusCode::FORBIDDEN, "no_role");
    }
    let got = login(&env, "carol", "carol-pw", Some("corp")).await;
    assert_error(&got, StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
    assert!(stored(&env, "carol@corp").is_none());
}

/// 역할이 바뀌면 다른 세션은 끝나고, API 토큰은 남아 새 역할로 동작한다.
#[tokio::test]
async fn a_changed_role_ends_other_sessions_and_tokens_follow_it() {
    let (env, ldap) = setup().await;
    let (_, first) = corp_login(&env, "bob", "bob-pw").await;
    let (_, second) = corp_login(&env, "bob", "bob-pw").await;
    let bob = token_client(&env, &first).await;
    assert_eq!(bob.get("/auth/whoami").await.1["role"], "viewer");
    assert_eq!(bob.get("/realms").await.0, StatusCode::FORBIDDEN);

    ldap.set_groups("bob", &[ADMINS]);
    let (_, third) = corp_login(&env, "bob", "bob-pw").await;
    for old in [&first, &second] {
        assert_eq!(whoami(&env, old).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(whoami(&env, &third).await.1["role"], "admin");
    let (status, me) = bob.get("/auth/whoami").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["role"], "admin");
    assert_eq!(bob.get("/realms").await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_password_of_an_external_user_cannot_be_changed() {
    let (env, _ldap) = setup().await;
    let (_, cookie) = corp_login(&env, "bob", "bob-pw").await;
    let admin = env.admin().await;
    let (status, body) = admin
        .put(
            "/users/bob@corp",
            json!({ "password": "new-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "external_user");

    // 본인도 못 바꾼다.
    let bob = token_client(&env, &cookie).await;
    let (status, body) = bob
        .put(
            "/users/bob@corp",
            json!({ "password": "new-password", "current_password": "bob-pw" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "external_user");

    // 역할은 바꿀 수 있다(다음 로그인에 매핑으로 다시 정해진다).
    let (status, body) = admin
        .put("/users/bob@corp", json!({ "role": "operator" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["role"].as_str(), body["realm"].as_str()),
        (Some("operator"), Some("corp"))
    );
    let (body, _) = corp_login(&env, "bob", "bob-pw").await;
    assert_eq!(body["role"], "viewer");
}

/// 세션 쿠키로 API 토큰을 만들고 그 토큰을 쓰는 클라이언트
async fn token_client(env: &Env, cookie: &str) -> Client {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/tokens", env.base))
        .header("cookie", cookie)
        .json(&json!({ "name": "cli" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.unwrap();
    Client {
        token: body["secret"].as_str().map(str::to_string),
        ..env.anonymous()
    }
}

#[tokio::test]
async fn external_users_use_api_tokens_until_their_realm_is_deleted() {
    let (env, ldap) = setup().await;
    let (_, cookie) = corp_login(&env, "alice", "alice-pw").await;
    let alice = token_client(&env, &cookie).await;
    let (status, me) = alice.get("/auth/whoami").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["name"], "alice@corp");
    // 외부 admin도 admin이다.
    assert_eq!(alice.get("/realms").await.0, StatusCode::OK);

    let admin = env.admin().await;
    assert_eq!(admin.delete("/realms/corp").await.0, StatusCode::NO_CONTENT);
    assert_eq!(whoami(&env, &cookie).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(alice.get("/auth/whoami").await.0, StatusCode::UNAUTHORIZED);
    // 사용자는 남는다.
    assert!(stored(&env, "alice@corp").is_some());

    // 같은 이름으로 다시 만들면 이어 쓴다.
    assert_eq!(
        admin.post("/realms", corp(&ldap)).await.0,
        StatusCode::CREATED
    );
    assert_eq!(whoami(&env, &cookie).await.0, StatusCode::OK);
    assert_eq!(alice.get("/auth/whoami").await.0, StatusCode::OK);
}

#[tokio::test]
async fn concurrent_first_logins_make_one_user() {
    let (env, _ldap) = setup().await;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let url = format!("{}/api/v1/auth/login", env.base);
        set.spawn(async move {
            reqwest::Client::new()
                .post(url)
                .json(&json!({ "username": "bob", "password": "bob-pw", "realm": "corp" }))
                .send()
                .await
                .unwrap()
                .status()
        });
    }
    for status in set.join_all().await {
        assert_eq!(status, StatusCode::OK);
    }
    let (_, users) = env.cluster.list::<UserSpec>().unwrap();
    assert_eq!(users.iter().filter(|u| u.name == "bob@corp").count(), 1);
}

/// 외부 사용자를 쓰는 로그인은 옛 노드가 모르는 필드(`realm`)를 쓰므로 기다린다.
#[tokio::test]
async fn realm_logins_wait_until_every_node_runs_this_build() {
    let (env, _ldap) = setup().await;
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    let got = login(&env, "alice", "alice-pw", Some("corp")).await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    assert!(stored(&env, "alice@corp").is_none());
}

#[tokio::test]
async fn realm_logins_are_audited_with_the_realm() {
    let (env, _ldap) = setup().await;
    corp_login(&env, "alice", "alice-pw").await;
    login(&env, "bob", "wrong", Some("corp")).await;
    login(&env, "bob", "wrong", Some("Bad Realm")).await;
    let start = Instant::now();
    let entries = loop {
        let entries: Vec<AuditEntry> = env
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
        let logins = entries
            .iter()
            .filter(|e| e.action == "POST /auth/login")
            .count();
        // admin 로그인(setup)과 셋
        if logins >= 4 {
            break entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let detail = |status: u16, detail: &str| {
        entries
            .iter()
            .find(|e| e.status == status && e.detail.as_deref() == Some(detail))
            .unwrap_or_else(|| panic!("{status} {detail}: {entries:#?}"))
    };
    let ok = detail(200, "username=alice realm=corp");
    assert_eq!(ok.user.as_deref(), Some("alice@corp"));
    detail(401, "username=bob realm=corp");
    detail(401, "username=bob realm=<invalid>");
    let text = serde_json::to_string(&entries).unwrap();
    assert!(!text.contains("alice-pw"), "{text}");
}
