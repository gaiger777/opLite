//! 실제 OpenLDAP(slapd)로 LDAP 영역 확인: LDAPS와 ldap://+StartTLS로 사용자 찾기·바인드와
//! memberOf 그룹, 틀린 자격 증명, 서비스 계정 실패·믿지 않는 CA, 그리고 API로 영역을 만들어
//! 시험 동작(틀린 서비스 계정 비밀번호는 service_bind에서 멈춤)과 외부 사용자 로그인(대문자 이름도
//! 같은 사용자). 디렉터리는 `.github/scripts/ldap-setup.sh`가 만든다.
//! `cargo test -p opl-api --features ldap-it --test ldap_it`
#![cfg(feature = "ldap-it")]

mod common;

use common::Env;
use opl_api::ldap::{LdapError, authenticate};
use opl_types::realm::{DEFAULT_GROUP_ATTR, DEFAULT_USER_FILTER, LdapRealm};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn var(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} must be set (see .github/scripts/ldap-setup.sh)"))
}

/// 시험 디렉터리. `user`는 `group`에 들고 `other`는 들지 않는다.
struct Directory {
    ldaps_url: String,
    starttls_url: String,
    ca_pem: String,
    bind_dn: String,
    bind_password: String,
    base_dn: String,
    user: (String, String),
    other: (String, String),
    group: String,
}

impl Directory {
    fn from_env() -> Self {
        let ca = var("OPL_TEST_LDAP_CA");
        Self {
            ldaps_url: var("OPL_TEST_LDAP_URL"),
            starttls_url: var("OPL_TEST_LDAP_STARTTLS_URL"),
            ca_pem: std::fs::read_to_string(&ca)
                .unwrap_or_else(|e| panic!("cannot read OPL_TEST_LDAP_CA {ca}: {e}")),
            bind_dn: var("OPL_TEST_LDAP_BIND_DN"),
            bind_password: var("OPL_TEST_LDAP_BIND_PASSWORD"),
            base_dn: var("OPL_TEST_LDAP_BASE_DN"),
            user: (
                var("OPL_TEST_LDAP_USER"),
                var("OPL_TEST_LDAP_USER_PASSWORD"),
            ),
            other: (
                var("OPL_TEST_LDAP_OTHER_USER"),
                var("OPL_TEST_LDAP_OTHER_PASSWORD"),
            ),
            group: var("OPL_TEST_LDAP_GROUP"),
        }
    }

    fn realm(&self, url: &str) -> LdapRealm {
        LdapRealm {
            url: url.into(),
            bind_dn: Some(self.bind_dn.clone()),
            bind_password: Some(self.bind_password.clone()),
            base_dn: self.base_dn.clone(),
            user_filter: DEFAULT_USER_FILTER.into(),
            group_attr: DEFAULT_GROUP_ATTR.into(),
            ca_pem: Some(self.ca_pem.clone()),
            timeout_secs: 10,
        }
    }

    fn in_group(&self, groups: &[String]) -> bool {
        groups.iter().any(|g| g.eq_ignore_ascii_case(&self.group))
    }
}

async fn login(env: &Env, user: &str, password: &str) -> (StatusCode, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&json!({ "username": user, "password": password, "realm": "corp" }))
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

#[tokio::test]
async fn a_real_directory_authenticates_users_and_maps_groups() {
    let dir = Directory::from_env();
    let (user, password) = (&dir.user.0, &dir.user.1);
    let (other, other_password) = (&dir.other.0, &dir.other.1);

    for url in [&dir.ldaps_url, &dir.starttls_url] {
        let realm = dir.realm(url);
        let got = authenticate(&realm, user, password)
            .await
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        assert!(
            got.dn
                .to_ascii_lowercase()
                .starts_with(&format!("uid={user},")),
            "{url}: {got:?}"
        );
        assert!(dir.in_group(&got.groups), "{url}: {got:?}");

        let got = authenticate(&realm, other, other_password)
            .await
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        assert!(!dir.in_group(&got.groups), "{url}: {got:?}");

        for (u, p) in [
            (user.as_str(), "wrong-password"),
            ("nobody", password.as_str()),
            ("*", password.as_str()),
        ] {
            assert_eq!(
                authenticate(&realm, u, p).await,
                Err(LdapError::InvalidCredentials),
                "{url}: {u}"
            );
        }

        // 서비스 계정 비밀번호가 틀림
        let mut bad = realm.clone();
        bad.bind_password = Some("wrong-password".into());
        let got = authenticate(&bad, user, password).await;
        assert!(
            matches!(got, Err(LdapError::Unavailable(_))),
            "{url}: {got:?}"
        );

        // 시스템 루트는 시험 CA를 믿지 않는다.
        let mut untrusted = realm.clone();
        untrusted.ca_pem = None;
        let got = authenticate(&untrusted, user, password).await;
        assert!(
            matches!(got, Err(LdapError::Unavailable(_))),
            "{url}: {got:?}"
        );
    }

    // API로 영역을 만들고 시험 동작과 로그인
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/realms",
            json!({
                "name": "corp",
                "backend": {
                    "kind": "ldap",
                    "url": dir.starttls_url,
                    "bind_dn": dir.bind_dn,
                    "bind_password": dir.bind_password,
                    "base_dn": dir.base_dn,
                    "ca_pem": dir.ca_pem,
                },
                "roles": { "groups": [{ "group": dir.group, "role": "admin" }] }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": user, "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let steps: Vec<(&str, bool)> = result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s["ok"] == true))
        .collect();
    assert_eq!(
        steps,
        [
            ("connect", true),
            ("service_bind", true),
            ("search", true),
            ("user_bind", true),
            ("groups", true)
        ],
        "{result}"
    );
    assert_eq!(result["role"], "admin", "{result}");

    let (status, body) = login(&env, user, password).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], format!("{user}@corp"));
    assert_eq!(body["role"], "admin");
    assert_eq!(body["realm"], "corp");

    // 대문자로 들어와도 같은 외부 사용자다(디렉터리의 uid 비교는 대소문자를 가리지 않는다).
    let (status, body) = login(&env, &user.to_ascii_uppercase(), password).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], format!("{user}@corp"));

    // 매핑되는 그룹이 없다.
    let (status, body) = login(&env, other, other_password).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "no_role");

    let (status, body) = login(&env, user, "wrong-password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "invalid_credentials");

    // 서비스 계정 비밀번호가 틀린 영역의 시험 동작은 service_bind에서 멈춘다.
    let (status, body) = admin
        .post(
            "/realms",
            json!({
                "name": "corp-bad",
                "backend": {
                    "kind": "ldap",
                    "url": dir.ldaps_url,
                    "bind_dn": dir.bind_dn,
                    "bind_password": "wrong-password",
                    "base_dn": dir.base_dn,
                    "ca_pem": dir.ca_pem,
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, result) = admin
        .post(
            "/realms/corp-bad/actions/test",
            json!({ "username": user, "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let last = result["steps"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        (last["name"].as_str(), last["ok"].as_bool()),
        (Some("service_bind"), Some(false)),
        "{result}"
    );
    assert_eq!(result["dn"], Value::Null, "{result}");
}
