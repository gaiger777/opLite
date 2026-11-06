//! 인증 영역 API: 만들기·읽기·바꾸기·지우기, admin만(읽기도), `local` 거절, 검증, 서비스 계정
//! 비밀번호를 내보내지 않음과 PUT의 bind_password 규칙, 옛 노드 409, 인증 없는 `/auth/realms`, 감사 로그.
//! OIDC 영역: 클라이언트 비밀을 내보내지 않음과 PUT의 client_secret 규칙, 공개 클라이언트, 로그인 400.

mod common;

use std::time::Instant;

use common::{Env, WAIT};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::realm::{RealmBackend, RealmSpec};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 서비스 계정을 쓰는 LDAP 영역 만들기 본문
fn corp() -> Value {
    json!({
        "name": "corp",
        "display": "Corp directory",
        "backend": {
            "kind": "ldap",
            "url": "ldaps://ldap.example.com",
            "bind_dn": "cn=svc,dc=example,dc=com",
            "bind_password": "svc-secret",
            "base_dn": "dc=example,dc=com"
        },
        "roles": {
            "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
            "default": "viewer"
        }
    })
}

/// 저장된 서비스 계정 비밀번호
fn stored_password(env: &Env, name: &str) -> Option<String> {
    let realm = env.cluster.get_by_name::<RealmSpec>(name).unwrap().unwrap();
    let RealmBackend::Ldap(ldap) = realm.spec.backend else {
        panic!("{name} is not an LDAP realm");
    };
    ldap.bind_password
}

/// 기밀 클라이언트 OIDC 영역 만들기 본문
fn okta() -> Value {
    json!({
        "name": "okta",
        "display": "Okta",
        "backend": {
            "kind": "oidc",
            "issuer": "https://corp.okta.com",
            "client_id": "0oa1",
            "client_secret": "client-secret"
        },
        "roles": {
            "groups": [{ "group": "opl-admins", "role": "admin" }],
            "default": "viewer"
        }
    })
}

/// 저장된 클라이언트 비밀
fn stored_secret(env: &Env, name: &str) -> Option<String> {
    let realm = env.cluster.get_by_name::<RealmSpec>(name).unwrap().unwrap();
    let RealmBackend::Oidc(oidc) = realm.spec.backend else {
        panic!("{name} is not an OIDC realm");
    };
    oidc.client_secret
}

#[tokio::test]
async fn oidc_realms_are_created_without_showing_the_client_secret() {
    let env = Env::start().await;
    let admin = env.admin().await;

    let (status, created) = admin.post("/realms", okta()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "okta");
    // 비밀은 응답에 없고, 있는지만 보인다. 빠진 값은 기본값으로 채운다.
    assert_eq!(
        created["backend"],
        json!({
            "kind": "oidc",
            "issuer": "https://corp.okta.com",
            "client_id": "0oa1",
            "scopes": ["openid", "profile", "email"],
            "username_claim": "preferred_username",
            "groups_claim": "groups",
            "timeout_secs": 10,
            "has_client_secret": true
        })
    );
    assert_eq!(created["roles"], okta()["roles"]);
    assert!(!created.to_string().contains("client-secret"));
    assert_eq!(
        stored_secret(&env, "okta").as_deref(),
        Some("client-secret")
    );
    assert_eq!(admin.get("/realms/okta").await.1, created);
    let (_, list) = admin.get("/realms").await;
    assert!(!list.to_string().contains("client-secret"), "{list}");

    // 모든 필드를 주면 그대로(issuer 끝의 /도) 저장한다.
    let mut full = okta();
    full["name"] = json!("full");
    full["backend"] = json!({
        "kind": "oidc",
        "issuer": "https://idp.example.com/realms/corp/",
        "client_id": "oplite",
        "client_secret": "s",
        "scopes": ["openid", "groups"],
        "username_claim": "email",
        "groups_claim": "roles",
        "ca_pem": "-----BEGIN CERTIFICATE-----",
        "timeout_secs": 120
    });
    let (status, got) = admin.post("/realms", full).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(
        got["backend"],
        json!({
            "kind": "oidc",
            "issuer": "https://idp.example.com/realms/corp/",
            "client_id": "oplite",
            "scopes": ["openid", "groups"],
            "username_claim": "email",
            "groups_claim": "roles",
            "ca_pem": "-----BEGIN CERTIFICATE-----",
            "timeout_secs": 120,
            "has_client_secret": true
        })
    );
}

/// 공개 클라이언트는 비밀이 없다(PKCE만). 빈 글도 없는 것이다.
#[tokio::test]
async fn public_oidc_clients_have_no_secret() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for (name, secret) in [("public", None), ("empty", Some(""))] {
        let mut body = okta();
        body["name"] = json!(name);
        match secret {
            Some(s) => body["backend"]["client_secret"] = json!(s),
            None => {
                body["backend"]
                    .as_object_mut()
                    .unwrap()
                    .remove("client_secret");
            }
        }
        let (status, got) = admin.post("/realms", body).await;
        assert_eq!(status, StatusCode::CREATED, "{got}");
        assert_eq!(got["backend"]["has_client_secret"], false, "{name}");
        assert_eq!(stored_secret(&env, name), None);
    }

    // 저장된 비밀이 없으면 생략은 issuer를 바꿔도 그대로 공개 클라이언트다.
    let mut body = okta();
    body.as_object_mut().unwrap().remove("name");
    body["backend"]
        .as_object_mut()
        .unwrap()
        .remove("client_secret");
    body["backend"]["issuer"] = json!("https://other.example.com");
    body["backend"]["client_id"] = json!("other");
    let (status, got) = admin.put("/realms/public", body, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["issuer"], "https://other.example.com");
    assert_eq!(got["backend"]["has_client_secret"], false);
    assert_eq!(stored_secret(&env, "public"), None);
}

/// PUT에서 client_secret을 빼면 issuer·client_id·ca_pem이 모두 그대로일 때만 저장된 값을 두고(하나라도
/// 바뀌면 400 — 비밀을 다른 IdP로 보내지 않게), 빈 글이면 지우고, 값이면 바꾼다. 서버는 issuer·client_id의
/// 앞뒤 공백과 CA의 CRLF를 다듬은 뒤 비교·저장한다.
#[tokio::test]
async fn the_client_secret_is_kept_replaced_or_cleared() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let ca = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
    let mut created = okta();
    created["backend"]["ca_pem"] = json!(format!("{}\r\n", ca.replace('\n', "\r\n")));
    created["backend"]["issuer"] = json!(" https://corp.okta.com\n");
    created["backend"]["client_id"] = json!(" 0oa1 ");
    let (status, got) = admin.post("/realms", created).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(got["backend"]["issuer"], "https://corp.okta.com");
    assert_eq!(got["backend"]["client_id"], "0oa1");
    assert_eq!(got["backend"]["ca_pem"], ca);

    let without_secret = |f: &dyn Fn(&mut Value)| {
        let mut b = okta();
        b.as_object_mut().unwrap().remove("name");
        b["backend"]["ca_pem"] = json!(ca);
        b["backend"]
            .as_object_mut()
            .unwrap()
            .remove("client_secret");
        f(&mut b);
        b
    };

    // 1. 연결 대상이 같으면 생략은 그대로 둔다(다듬은 같은 CA, 끝 줄바꿈이 있는 CA도).
    for b in [
        without_secret(&|b| b["display"] = json!("Okta SSO")),
        without_secret(&|b| b["backend"]["ca_pem"] = json!(format!("{ca}\n"))),
    ] {
        let (status, got) = admin.put("/realms/okta", b, None).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["backend"]["has_client_secret"], true);
    }
    assert_eq!(
        stored_secret(&env, "okta").as_deref(),
        Some("client-secret")
    );

    // 2~4. issuer·client_id·ca_pem 가운데 하나라도 바꾸면서 생략하면 400이고 아무것도 바뀌지 않는다.
    for changed in [
        without_secret(&|b| b["backend"]["issuer"] = json!("https://evil.example.com")),
        without_secret(&|b| b["backend"]["issuer"] = json!("https://corp.okta.com/")),
        without_secret(&|b| b["backend"]["client_id"] = json!("0oa2")),
        without_secret(&|b| b["backend"]["ca_pem"] = json!("-----BEGIN CERTIFICATE-----")),
        without_secret(&|b| {
            b["backend"].as_object_mut().unwrap().remove("ca_pem");
        }),
    ] {
        let got = admin.put("/realms/okta", changed.clone(), None).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "client_secret must be given again when issuer, client_id or ca_pem changes",
            "{changed}"
        );
    }
    let (_, got) = admin.get("/realms/okta").await;
    assert_eq!(got["backend"]["issuer"], "https://corp.okta.com");
    assert_eq!(
        stored_secret(&env, "okta").as_deref(),
        Some("client-secret")
    );

    // 바꾸면서 새 비밀을 주면 된다.
    let mut moved = without_secret(&|b| b["backend"]["issuer"] = json!("https://new.okta.com"));
    moved["backend"]["client_secret"] = json!("new-secret");
    let (status, got) = admin.put("/realms/okta", moved.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["issuer"], "https://new.okta.com");
    assert_eq!(got["backend"]["has_client_secret"], true);
    assert!(!got.to_string().contains("new-secret"));
    assert_eq!(stored_secret(&env, "okta").as_deref(), Some("new-secret"));

    // 5. 빈 글은 지운다(공개 클라이언트가 된다).
    moved["backend"]["client_secret"] = json!("");
    let (status, got) = admin.put("/realms/okta", moved, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["has_client_secret"], false);
    assert_eq!(stored_secret(&env, "okta"), None);
}

#[tokio::test]
async fn bad_oidc_realms_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let with = |f: &dyn Fn(&mut Value)| {
        let mut body = okta();
        f(&mut body);
        body
    };
    for (body, message) in [
        (
            with(&|b| b["backend"]["issuer"] = json!("http://corp.okta.com")),
            "issuer must be an https:// URL with a host and no query or fragment",
        ),
        (
            with(&|b| b["backend"]["issuer"] = json!("https://corp.okta.com?x=1")),
            "issuer must be an https:// URL with a host and no query or fragment",
        ),
        (
            with(&|b| b["backend"]["client_id"] = json!(" ")),
            "client_id must not be empty",
        ),
        (
            with(&|b| b["backend"]["scopes"] = json!(["profile"])),
            "scopes must include openid, and each scope must be non-empty without spaces",
        ),
        (
            with(&|b| b["backend"]["username_claim"] = json!("")),
            "username_claim must not be empty",
        ),
        (
            with(&|b| b["backend"]["groups_claim"] = json!("")),
            "groups_claim must not be empty",
        ),
        (
            with(&|b| b["backend"]["timeout_secs"] = json!(121)),
            "timeout_secs must be between 1 and 120",
        ),
        // 모르는 필드와 다른 종류의 필드
        (with(&|b| b["backend"]["bogus"] = json!(1)), ""),
        (with(&|b| b["backend"]["base_dn"] = json!("dc=x")), ""),
    ] {
        let got = admin.post("/realms", body.clone()).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        if !message.is_empty() {
            assert_eq!(got.1["message"], message, "{body}");
        }
    }
    assert_eq!(admin.get("/realms").await.1["items"], json!([]));
}

/// OIDC 영역은 admin만 만들고, 옛 노드가 있으면 만들지 않는다.
#[tokio::test]
async fn oidc_realms_need_admins_and_nodes_that_run_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        assert_error(
            &client.post("/realms", okta()).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
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
    assert_error(
        &admin.post("/realms", okta()).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    assert_eq!(admin.get("/realms").await.1["items"], json!([]));
}

/// OIDC 영역은 IdP에서 로그인한다. 로그인 화면 목록에는 kind oidc로 보이고, 비밀번호 로그인은 400이다.
#[tokio::test]
async fn oidc_realms_are_listed_for_the_login_screen_but_reject_password_logins() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", okta()).await.0, StatusCode::CREATED);
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let anonymous = env.anonymous();
    let (status, got) = anonymous.get("/auth/realms").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!([
            { "name": "local", "display": null, "kind": "local" },
            { "name": "corp", "display": "Corp directory", "kind": "ldap" },
            { "name": "okta", "display": "Okta", "kind": "oidc" },
        ])
    );

    // 시도 제한에 세지 않는다(몇 번이든 같은 400).
    for _ in 0..21 {
        let got = anonymous
            .post(
                "/auth/login",
                json!({ "username": "alice", "password": "pw", "realm": "okta" }),
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "realm okta signs in through its identity provider"
        );
    }
    assert!(
        env.cluster
            .get_by_name::<opl_types::auth::UserSpec>("alice@okta")
            .unwrap()
            .is_none()
    );
    // 같은 IP의 로컬 로그인은 막히지 않았다.
    let got = anonymous
        .post(
            "/auth/login",
            json!({ "username": "admin", "password": common::ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(got.0, StatusCode::OK, "{}", got.1);
}

#[tokio::test]
async fn realms_are_created_read_updated_and_deleted() {
    let env = Env::start().await;
    let admin = env.admin().await;

    let (status, created) = admin.post("/realms", corp()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "corp");
    assert_eq!(created["display"], "Corp directory");
    // 비밀번호는 응답에 없고, 있는지만 보인다. 빠진 값은 기본값으로 채운다.
    assert_eq!(
        created["backend"],
        json!({
            "kind": "ldap",
            "url": "ldaps://ldap.example.com",
            "bind_dn": "cn=svc,dc=example,dc=com",
            "base_dn": "dc=example,dc=com",
            "user_filter": "(&(objectClass=person)(uid={username}))",
            "group_attr": "memberOf",
            "timeout_secs": 10,
            "has_bind_password": true
        })
    );
    assert_eq!(created["roles"], corp()["roles"]);
    assert!(!created.to_string().contains("svc-secret"));
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));

    let (status, got) = admin.get("/realms/corp").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got, created);
    let id = created["id"].as_str().unwrap();
    assert_eq!(admin.get(&format!("/realms/{id}")).await.1, created);

    let (status, list) = admin.get("/realms").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"], json!([created]));
    assert!(list["revision"].as_u64().unwrap() >= created["revision"].as_u64().unwrap());

    // PUT은 통째로 바꾼다(비밀번호는 빼면 그대로).
    let mut body = corp();
    body.as_object_mut().unwrap().remove("name");
    body["display"] = json!("Corp");
    body["backend"]["timeout_secs"] = json!(5);
    body["backend"]
        .as_object_mut()
        .unwrap()
        .remove("bind_password");
    body["roles"] = json!({ "groups": [] });
    let revision = created["revision"].as_u64();
    let (status, updated) = admin.put("/realms/corp", body.clone(), revision).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["display"], "Corp");
    assert_eq!(updated["backend"]["timeout_secs"], 5);
    assert_eq!(updated["backend"]["has_bind_password"], true);
    assert_eq!(updated["roles"], json!({ "groups": [] }));
    assert!(updated["revision"].as_u64() > revision);
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));

    // 옛 revision으로는 바꾸지 않는다.
    assert_error(
        &admin.put("/realms/corp", body.clone(), revision).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );

    // 같은 이름을 주는 것은 괜찮고, 다른 이름은 400이다.
    body["name"] = json!("corp");
    assert_eq!(
        admin.put("/realms/corp", body.clone(), None).await.0,
        StatusCode::OK
    );
    body["name"] = json!("other");
    assert_error(
        &admin.put("/realms/corp", body, None).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );

    // 이미 있는 이름
    let got = admin.post("/realms", corp()).await;
    assert_eq!(got.0, StatusCode::CONFLICT, "{}", got.1);

    let (status, body) = admin.delete("/realms/corp").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_error(
        &admin.get("/realms/corp").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_eq!(admin.get("/realms").await.1["items"], json!([]));
    assert_error(
        &admin.delete("/realms/corp").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// PUT에서 bind_password를 빼면 url·bind_dn·ca_pem이 모두 그대로일 때만 저장된 값을 두고(하나라도
/// 바뀌면 400 — 비밀번호를 다른 서버로 보내지 않게), 빈 글이면 지우고, 값이면 바꾼다. 서비스 계정
/// DN에는 비밀번호가 있어야 하고, 익명 검색은 둘 다 없을 때만이다.
#[tokio::test]
async fn the_bind_password_is_kept_replaced_or_cleared() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let mut body = corp();
    body.as_object_mut().unwrap().remove("name");
    let without_password = |f: &dyn Fn(&mut Value)| {
        let mut b = body.clone();
        b["backend"]
            .as_object_mut()
            .unwrap()
            .remove("bind_password");
        f(&mut b);
        b
    };

    // 연결 대상이 같으면 생략은 그대로 둔다.
    let (status, got) = admin
        .put(
            "/realms/corp",
            without_password(&|b| b["display"] = json!("Corp")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["has_bind_password"], true);
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));

    // url·bind_dn·ca_pem 가운데 하나라도 바꾸면서 생략하면 400이고 아무것도 바뀌지 않는다.
    for changed in [
        without_password(&|b| b["backend"]["url"] = json!("ldaps://evil.example.com")),
        without_password(&|b| b["backend"]["bind_dn"] = json!("cn=other,dc=example,dc=com")),
        without_password(&|b| b["backend"]["ca_pem"] = json!("-----BEGIN CERTIFICATE-----")),
        without_password(&|b| {
            b["backend"].as_object_mut().unwrap().remove("bind_dn");
        }),
    ] {
        let got = admin.put("/realms/corp", changed.clone(), None).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "bind_password must be given again when url, bind_dn or ca_pem changes",
            "{changed}"
        );
    }
    let (_, got) = admin.get("/realms/corp").await;
    assert_eq!(got["backend"]["url"], "ldaps://ldap.example.com");
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));

    // 바꾸면서 새 비밀번호를 주면 된다.
    let mut moved = body.clone();
    moved["backend"]["url"] = json!("ldaps://ldap2.example.com");
    moved["backend"]["bind_password"] = json!("new-secret");
    let (status, got) = admin.put("/realms/corp", moved.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["url"], "ldaps://ldap2.example.com");
    assert_eq!(got["backend"]["has_bind_password"], true);
    assert!(!got.to_string().contains("new-secret"));
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("new-secret"));

    // 빈 글은 지우는 것이라 bind_dn이 남아 있으면 400이다.
    moved["backend"]["bind_password"] = json!("");
    let got = admin.put("/realms/corp", moved.clone(), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "bind_dn needs bind_password; remove bind_dn for an anonymous search"
    );
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("new-secret"));

    // bind_dn도 빼면 익명 검색이 된다.
    moved["backend"].as_object_mut().unwrap().remove("bind_dn");
    let (status, got) = admin.put("/realms/corp", moved.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["has_bind_password"], false);
    assert!(got["backend"].get("bind_dn").is_none(), "{got}");
    assert_eq!(stored_password(&env, "corp"), None);

    // 저장된 비밀번호가 없으면 생략은 그대로 익명이다(url을 바꿔도).
    moved["backend"]
        .as_object_mut()
        .unwrap()
        .remove("bind_password");
    moved["backend"]["url"] = json!("ldaps://ldap3.example.com");
    let (status, got) = admin.put("/realms/corp", moved, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["has_bind_password"], false);

    // 만들 때도 bind_dn에는 비밀번호가 있어야 하고, 둘 다 없으면 익명이다.
    let mut anon = corp();
    anon["name"] = json!("anon");
    anon["backend"]["bind_password"] = json!("");
    assert_error(
        &admin.post("/realms", anon.clone()).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    anon["backend"].as_object_mut().unwrap().remove("bind_dn");
    let (status, got) = admin.post("/realms", anon).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(got["backend"]["has_bind_password"], false);
    assert_eq!(stored_password(&env, "anon"), None);
}

/// 서버가 url·bind_dn의 앞뒤 공백을 떼고 CA의 CRLF를 LF로 바꿔 앞뒤 공백을 뗀 값을 저장·비교한다.
/// 그래서 끝 줄바꿈이 있는 CA(`oplctl --ca-file`)로 만든 영역을 다듬은 같은 CA(웹)로 바꿔도 비밀번호가 남는다.
#[tokio::test]
async fn urls_dns_and_cas_are_trimmed_before_they_are_kept_or_compared() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let ca = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
    for (name, sent_ca) in [
        ("lf", format!("{ca}\n")),
        ("crlf", format!("{}\r\n", ca.replace('\n', "\r\n"))),
    ] {
        let mut body = corp();
        body["name"] = json!(name);
        body["backend"]["url"] = json!(" ldaps://ldap.example.com\n");
        body["backend"]["bind_dn"] = json!(" cn=svc,dc=example,dc=com ");
        body["backend"]["ca_pem"] = json!(sent_ca);
        let (status, got) = admin.post("/realms", body).await;
        assert_eq!(status, StatusCode::CREATED, "{got}");
        assert_eq!(got["backend"]["url"], "ldaps://ldap.example.com");
        assert_eq!(got["backend"]["bind_dn"], "cn=svc,dc=example,dc=com");
        assert_eq!(got["backend"]["ca_pem"], ca, "{name}");

        let mut body = corp();
        body.as_object_mut().unwrap().remove("name");
        body["backend"]
            .as_object_mut()
            .unwrap()
            .remove("bind_password");
        body["backend"]["ca_pem"] = json!(ca);
        body["display"] = json!("Trimmed");
        let (status, got) = admin.put(&format!("/realms/{name}"), body, None).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["backend"]["has_bind_password"], true);
        assert_eq!(stored_password(&env, name).as_deref(), Some("svc-secret"));
    }
}

/// 계정 종류처럼 admin만 읽고 바꾼다.
#[tokio::test]
async fn only_admins_read_or_change_realms() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let mut body = corp();
    body.as_object_mut().unwrap().remove("name");
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        let mut other = corp();
        other["name"] = json!(format!("by-{role}"));
        for got in [
            client.get("/realms").await,
            client.get("/realms/corp").await,
            client.post("/realms", other).await,
            client.put("/realms/corp", body.clone(), None).await,
            client.delete("/realms/corp").await,
        ] {
            assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
        }
    }
    assert_error(
        &env.anonymous().get("/realms").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    let (_, list) = admin.get("/realms").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));
}

#[tokio::test]
async fn bad_realms_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let with = |f: &dyn Fn(&mut Value)| {
        let mut body = corp();
        f(&mut body);
        body
    };
    for (body, message) in [
        (
            with(&|b| b["name"] = json!("local")),
            "the realm name \"local\" is reserved for local users",
        ),
        (with(&|b| b["name"] = json!("Corp")), ""),
        (
            with(&|b| b["name"] = json!("0192f0c1-5d2e-7000-8000-000000000000")),
            "",
        ),
        (
            with(&|b| {
                b.as_object_mut().unwrap().remove("name");
            }),
            "name is required",
        ),
        (
            with(&|b| b["backend"]["url"] = json!("https://ldap.example.com")),
            "url must be ldap://host[:port] or ldaps://host[:port]",
        ),
        (
            with(&|b| b["backend"]["base_dn"] = json!(" ")),
            "base_dn must not be empty",
        ),
        (
            with(&|b| b["backend"]["user_filter"] = json!("(uid=alice)")),
            "user_filter must contain {username} exactly once",
        ),
        (
            with(&|b| b["backend"]["timeout_secs"] = json!(0)),
            "timeout_secs must be between 1 and 30",
        ),
        (
            with(&|b| {
                b["backend"].as_object_mut().unwrap().remove("bind_dn");
            }),
            "bind_password needs bind_dn",
        ),
        (
            with(&|b| b["roles"]["groups"][0]["group"] = json!("")),
            "roles.groups[].group must not be empty",
        ),
        // 모르는 필드와 종류
        (with(&|b| b["bogus"] = json!(1)), ""),
        (with(&|b| b["backend"]["kind"] = json!("saml")), ""),
        (with(&|b| b["backend"]["kind"] = json!("oidc")), ""),
        (with(&|b| b["backend"]["bogus"] = json!(1)), ""),
    ] {
        let got = admin.post("/realms", body.clone()).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        if !message.is_empty() {
            assert_eq!(got.1["message"], message, "{body}");
        }
    }
    assert_eq!(admin.get("/realms").await.1["items"], json!([]));

    // 바꿀 때도 검증한다.
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let mut body = corp();
    body["backend"]["url"] = json!("ldap://");
    let got = admin.put("/realms/corp", body, None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_error(
        &admin.put("/realms/nope", corp(), None).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// 이 빌드보다 낮거나 버전을 적지 않은 노드가 있으면 영역을 쓰지 않는다(옛 노드는 새 종류를 풀지 못한다).
#[tokio::test]
async fn realms_wait_until_every_node_runs_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
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

    let mut other = corp();
    other["name"] = json!("other");
    let mut body = corp();
    body["display"] = json!("changed");
    for got in [
        admin.post("/realms", other).await,
        admin.put("/realms/corp", body, None).await,
        admin.delete("/realms/corp").await,
    ] {
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    }
    // 읽기는 된다.
    assert_eq!(
        admin.get("/realms/corp").await.1["display"],
        "Corp directory"
    );
    assert_eq!(
        admin.get("/realms").await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// 로그인 화면이 읽는 목록: 인증 없이, `local`이 먼저, 나머지는 이름 순, 비밀은 없음.
#[tokio::test]
async fn the_login_screen_lists_realms_without_login() {
    let env = Env::start().await;
    let anonymous = env.anonymous();
    let (status, got) = anonymous.get("/auth/realms").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!([{ "name": "local", "display": null, "kind": "local" }])
    );

    let admin = env.admin().await;
    for (name, display) in [("zeta", Some("Zeta")), ("corp", None), ("ad", Some("AD"))] {
        let mut body = corp();
        body["name"] = json!(name);
        match display {
            Some(d) => body["display"] = json!(d),
            None => {
                body.as_object_mut().unwrap().remove("display");
            }
        }
        let (status, got) = admin.post("/realms", body).await;
        assert_eq!(status, StatusCode::CREATED, "{got}");
    }
    let (status, got) = anonymous.get("/auth/realms").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!([
            { "name": "local", "display": null, "kind": "local" },
            { "name": "ad", "display": "AD", "kind": "ldap" },
            { "name": "corp", "display": null, "kind": "ldap" },
            { "name": "zeta", "display": "Zeta", "kind": "ldap" },
        ])
    );
}

/// 영역을 바꾸는 요청은 감사 미들웨어가 적는다(본문의 비밀번호는 적지 않는다).
#[tokio::test]
async fn realm_changes_are_audited() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    assert_eq!(admin.delete("/realms/corp").await.0, StatusCode::NO_CONTENT);
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
        if entries.iter().any(|e| e.action == "DELETE /realms/{key}") {
            break entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let created = entries
        .iter()
        .find(|e| e.action == "POST /realms" && e.status == 201)
        .expect("POST /realms is recorded");
    assert_eq!(created.user.as_deref(), Some("admin"));
    let deleted = entries
        .iter()
        .find(|e| e.action == "DELETE /realms/{key}")
        .unwrap();
    assert_eq!(deleted.status, 204);
    assert_eq!(deleted.target.as_ref().unwrap().key, "corp");
    let text = serde_json::to_string(&entries).unwrap();
    assert!(!text.contains("svc-secret"), "{text}");
}
