//! LDAP 클라이언트(`opl_api::ldap`)와 영역 시험 동작: 가짜 LDAP 서버(LDAPS와 ldap://+StartTLS)로
//! 성공, 잘못된 자격 증명, 디렉터리에 닿지 못함, 필터 이스케이프, 시험 동작의 단계 결과와 권한.

mod common;

use std::time::{Duration, Instant};

use common::Env;
use common::ldap::{BASE_DN, Directory, FakeLdap, Mode, Person, SERVICE_DN, SERVICE_PASSWORD};
use opl_api::ldap::{LdapError, LdapUser, authenticate};
use opl_peer::pki::ClusterCa;
use opl_types::realm::{DEFAULT_GROUP_ATTR, DEFAULT_USER_FILTER, LdapRealm};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ADMINS: &str = "cn=admins,ou=groups,dc=example,dc=com";
const STAFF: &str = "cn=staff,ou=groups,dc=example,dc=com";

fn alice() -> Person {
    Person::new("alice", "alice-pw", &[ADMINS, STAFF])
}

fn bob() -> Person {
    Person::new("bob", "bob-pw", &[STAFF])
}

/// 서비스 계정으로 찾는 영역
fn realm(ldap: &FakeLdap) -> LdapRealm {
    LdapRealm {
        url: ldap.url.clone(),
        bind_dn: Some(SERVICE_DN.into()),
        bind_password: Some(SERVICE_PASSWORD.into()),
        base_dn: BASE_DN.into(),
        user_filter: DEFAULT_USER_FILTER.into(),
        group_attr: DEFAULT_GROUP_ATTR.into(),
        ca_pem: Some(ldap.ca_pem.clone()),
        timeout_secs: 5,
    }
}

fn alice_user() -> LdapUser {
    LdapUser {
        dn: format!("uid=alice,ou=people,{BASE_DN}"),
        groups: vec![ADMINS.into(), STAFF.into()],
    }
}

fn unavailable(got: Result<LdapUser, LdapError>) -> String {
    match got {
        Err(LdapError::Unavailable(message)) => message,
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn a_user_is_found_and_bound_over_ldaps_and_starttls() {
    for mode in [Mode::Ldaps, Mode::StartTls] {
        let ldap = FakeLdap::start(mode, Directory::with(vec![alice(), bob()])).await;
        let got = authenticate(&realm(&ldap), "alice", "alice-pw").await;
        assert_eq!(got, Ok(alice_user()), "{mode:?}");
        // 서비스 계정 바인드, 검색, 사용자 바인드
        assert_eq!((ldap.binds(), ldap.searches()), (2, 1), "{mode:?}");
        assert_eq!(
            ldap.assertions(),
            ["objectClass=person", "uid=alice"],
            "{mode:?}"
        );
    }
}

#[tokio::test]
async fn without_a_service_account_the_search_is_anonymous() {
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![bob()])).await;
    let mut realm = realm(&ldap);
    realm.bind_dn = None;
    realm.bind_password = None;
    let got = authenticate(&realm, "bob", "bob-pw").await.unwrap();
    assert_eq!(got.groups, [STAFF]);
    // 사용자 바인드만
    assert_eq!(ldap.binds(), 1);
}

#[tokio::test]
async fn wrong_credentials_are_invalid() {
    let mut directory = Directory::with(vec![alice(), bob()]);
    // 같은 uid의 항목 둘
    directory.people.push(Person {
        dn: format!("uid=carol,ou=other,{BASE_DN}"),
        ..Person::new("carol", "carol-pw", &[])
    });
    directory.people.push(Person {
        dn: format!("uid=carol,ou=more,{BASE_DN}"),
        ..Person::new("carol", "carol-pw", &[])
    });
    let ldap = FakeLdap::start(Mode::Ldaps, directory).await;
    let realm = realm(&ldap);

    for (user, password) in [
        ("alice", "wrong"),
        ("nobody", "alice-pw"),
        ("carol", "carol-pw"),
    ] {
        assert_eq!(
            authenticate(&realm, user, password).await,
            Err(LdapError::InvalidCredentials),
            "{user}"
        );
    }

    // 빈 비밀번호는 서버에 닿지 않는다.
    let connections = ldap.connections();
    let binds = ldap.binds();
    assert_eq!(
        authenticate(&realm, "alice", "").await,
        Err(LdapError::InvalidCredentials)
    );
    assert_eq!((ldap.connections(), ldap.binds()), (connections, binds));
}

#[tokio::test]
async fn special_characters_in_the_user_name_are_escaped() {
    // 사람이 alice 하나뿐이라, 이스케이프하지 않으면 `*`가 alice와 맞는다.
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    let realm = realm(&ldap);
    for user in ["*", "alice)(uid=*", "al*"] {
        assert_eq!(
            authenticate(&realm, user, "alice-pw").await,
            Err(LdapError::InvalidCredentials),
            "{user}"
        );
    }
    // 서버는 이름을 글자 그대로 받았다.
    let assertions = ldap.assertions();
    assert!(assertions.contains(&"uid=*".to_string()), "{assertions:?}");
    assert!(
        assertions.contains(&"uid=alice)(uid=*".to_string()),
        "{assertions:?}"
    );
    // 사용자 바인드는 없었다(서비스 계정만).
    assert_eq!(ldap.binds(), 3);
}

#[tokio::test]
async fn an_unreachable_directory_is_unavailable() {
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;

    // 서비스 계정 비밀번호가 틀림
    let mut bad = realm(&ldap);
    bad.bind_password = Some("wrong".into());
    let message = unavailable(authenticate(&bad, "alice", "alice-pw").await);
    assert!(message.contains("service account"), "{message}");

    // 믿지 않는 CA
    let mut other = realm(&ldap);
    other.ca_pem = Some(ClusterCa::generate("other").unwrap().cert_pem);
    unavailable(authenticate(&other, "alice", "alice-pw").await);

    // 닫힌 포트
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut closed = realm(&ldap);
    closed.url = format!("ldaps://127.0.0.1:{port}");
    unavailable(authenticate(&closed, "alice", "alice-pw").await);

    // 받기만 하고 답하지 않는 서버는 시간 제한으로 끝난다.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut slow = realm(&ldap);
    slow.url = format!("ldaps://{}", silent.local_addr().unwrap());
    slow.timeout_secs = 1;
    let started = Instant::now();
    unavailable(authenticate(&slow, "alice", "alice-pw").await);
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(silent);
}

/// 같은 uid의 항목 `n`개
fn many_carols(n: usize) -> Vec<Person> {
    (0..n)
        .map(|i| Person {
            dn: format!("uid=carol,ou=p{i},{BASE_DN}"),
            ..Person::new("carol", "carol-pw", &[])
        })
        .collect()
}

#[tokio::test]
async fn the_search_stops_at_two_entries() {
    // 서버가 크기 제한을 지키지 않고 많이 보내도 둘에서 멈추고 검색을 버린다.
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(many_carols(50))).await;
    let mut realm = realm(&ldap);
    assert_eq!(
        authenticate(&realm, "carol", "carol-pw").await,
        Err(LdapError::InvalidCredentials)
    );
    assert_eq!(ldap.size_limits(), [2]);
    // 서비스 계정만 바인드했고 검색은 버렸다.
    assert_eq!(ldap.binds(), 1);
    let started = Instant::now();
    while ldap.abandons() == 0 {
        assert!(started.elapsed() < Duration::from_secs(5), "no abandon");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 서버 쪽 제한으로 4(sizeLimitExceeded)가 오면 여럿과 같다.
    ldap.truncate_at(1);
    assert_eq!(
        authenticate(&realm, "carol", "carol-pw").await,
        Err(LdapError::InvalidCredentials)
    );
    assert_eq!(ldap.binds(), 2);

    // 한 명이면 그대로 된다(크기 제한이 결과를 바꾸지 않음).
    realm.user_filter = "(uid={username})".into();
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    realm.url = ldap.url.clone();
    realm.ca_pem = Some(ldap.ca_pem.clone());
    ldap.truncate_at(1);
    assert_eq!(
        authenticate(&realm, "alice", "alice-pw").await,
        Ok(alice_user())
    );
}

#[tokio::test]
async fn the_whole_check_has_a_deadline() {
    // 참조를 끝없이 보내는 서버: 하나하나는 작업 시간 제한 안에 오지만 전체 마감에 걸린다.
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    ldap.send_endless_references();
    let mut realm = realm(&ldap);
    realm.timeout_secs = 1;
    let started = Instant::now();
    let message = unavailable(authenticate(&realm, "alice", "alice-pw").await);
    assert_eq!(message, "timed out");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(8),
        "{elapsed:?}"
    );

    let spec = opl_types::realm::RealmSpec {
        display: None,
        backend: opl_types::realm::RealmBackend::Ldap(realm),
        roles: Default::default(),
    };
    let result = opl_api::ldap::test(&spec, "alice", "alice-pw").await;
    let last = result.steps.last().unwrap();
    assert_eq!((last.name.as_str(), last.ok), ("search", false));
    assert_eq!(last.message, "timed out");
}

#[tokio::test]
async fn the_host_name_and_starttls_are_required() {
    // 인증서는 127.0.0.1이라 localhost로는 믿지 않는다.
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    let mut by_name = realm(&ldap);
    let port = ldap.url.rsplit(':').next().unwrap();
    by_name.url = format!("ldaps://localhost:{port}");
    let message = unavailable(authenticate(&by_name, "alice", "alice-pw").await);
    assert!(ldap.connections() >= 1, "{message}");
    assert_eq!(ldap.binds(), 0, "{message}");

    // StartTLS를 거절하는 서버에는 바인드하지 않는다.
    let ldap = FakeLdap::start(Mode::StartTlsRefused, Directory::with(vec![alice()])).await;
    unavailable(authenticate(&realm(&ldap), "alice", "alice-pw").await);
    assert_eq!(ldap.connections(), 1);
    assert_eq!(ldap.binds(), 0);
}

#[tokio::test]
async fn invalid_credentials_take_at_least_300ms() {
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    let realm = realm(&ldap);
    for (user, password) in [("nobody", "x"), ("alice", "wrong"), ("alice", "")] {
        let started = Instant::now();
        assert_eq!(
            authenticate(&realm, user, password).await,
            Err(LdapError::InvalidCredentials),
            "{user}"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "{user}: {:?}",
            started.elapsed()
        );
    }
}

/// 가짜 서버를 쓰는 영역 만들기 본문
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

fn step_names(result: &Value) -> Vec<(String, bool)> {
    result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_string(),
                s["ok"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_test_action_reports_each_step() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let ldap = FakeLdap::start(Mode::StartTls, Directory::with(vec![alice(), bob()])).await;
    let (status, body) = admin.post("/realms", corp(&ldap)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, users) = admin.get("/users").await;

    let (status, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": "alice", "password": "alice-pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let all = ["connect", "service_bind", "search", "user_bind", "groups"];
    assert_eq!(
        step_names(&result),
        all.map(|s| (s.to_string(), true)).to_vec()
    );
    assert_eq!(result["dn"], alice_user().dn);
    assert_eq!(result["groups"], json!([ADMINS, STAFF]));
    assert_eq!(result["role"], "admin");
    assert!(!result.to_string().contains("alice-pw"));
    assert!(!result.to_string().contains(SERVICE_PASSWORD));

    // 틀린 비밀번호는 user_bind에서 멈춘다.
    let (status, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": "bob", "password": "secret-guess" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        step_names(&result),
        [
            ("connect".to_string(), true),
            ("service_bind".to_string(), true),
            ("search".to_string(), true),
            ("user_bind".to_string(), false)
        ]
    );
    assert_eq!(result["dn"], format!("uid=bob,ou=people,{BASE_DN}"));
    assert_eq!(result["groups"], json!([]));
    assert_eq!(result["role"], Value::Null);
    assert!(!result.to_string().contains("secret-guess"));

    // 없는 사용자는 search에서 멈춘다.
    let (_, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": "nobody", "password": "x" }),
        )
        .await;
    assert_eq!(
        step_names(&result).last().unwrap(),
        &("search".to_string(), false)
    );
    assert_eq!(result["dn"], Value::Null);

    // 사용자를 만들지 않는다.
    assert_eq!(admin.get("/users").await.1, users);

    // 닿지 않는 디렉터리는 connect에서 멈춘다.
    drop(ldap);
    let (_, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": "alice", "password": "alice-pw" }),
        )
        .await;
    assert_eq!(step_names(&result), [("connect".to_string(), false)]);

    // LDAP 영역은 사용자와 비밀번호가 있어야 한다.
    for body in [json!({}), json!({ "username": "alice" })] {
        let (status, got) = admin.post("/realms/corp/actions/test", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
        assert_eq!(
            got["message"],
            "username and password are required to test an LDAP realm"
        );
    }

    // 없는 영역
    let (status, _) = admin
        .post(
            "/realms/other/actions/test",
            json!({ "username": "alice", "password": "alice-pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_admins_can_test_a_realm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    let (status, body) = admin.post("/realms", corp(&ldap)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let body = json!({ "username": "alice", "password": "alice-pw" });
    for role in ["operator", "viewer"] {
        let client = env.user(&format!("u-{role}"), role).await;
        let (status, got) = client.post("/realms/corp/actions/test", body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {got}");
    }
    let (status, _) = env
        .anonymous()
        .post("/realms/corp/actions/test", body)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // 거절된 요청은 디렉터리에 닿지 않았다.
    assert_eq!(ldap.connections(), 0);
}
