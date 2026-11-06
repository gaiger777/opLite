//! OIDC 핵심 모듈과 가짜 IdP: discovery(issuer 다름·http 끝점·믿지 않는 CA 거절, 캐시), JWKS(캐시,
//! 모르는 kid 뒤 다시 받기, 10초 안에는 다시 받지 않음), 받기 실패 기억과 동시 받기 하나로, 가장 높은
//! revision, 캐시를 거친 ID 토큰 확인, 영역 시험 동작의 `discovery`·`jwks` 단계.

mod common;

use std::time::Duration;

use common::Env;
use common::idp::{Alg, Idp};
use opl_api::oidc::{OidcCache, OidcError, RealmKey};
use opl_types::realm::{OidcRealm, RealmBackend};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn realm(idp: &Idp) -> OidcRealm {
    match serde_json::from_value(idp.backend()).unwrap() {
        RealmBackend::Oidc(o) => o,
        RealmBackend::Ldap(_) => unreachable!(),
    }
}

fn key() -> RealmKey {
    (Uuid::new_v4(), 1)
}

fn invalid(message: impl Into<String>) -> OidcError {
    OidcError::Invalid(message.into())
}

#[tokio::test]
async fn discovery_is_checked_and_cached() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let k = key();
    let doc = cache.discovery(k, &realm).await.unwrap();
    assert_eq!(doc.issuer, idp.issuer);
    assert_eq!(doc.jwks_uri, format!("{}/jwks", idp.issuer));
    assert_eq!(
        doc.userinfo_endpoint.as_deref(),
        Some(format!("{}/userinfo", idp.issuer).as_str())
    );
    // 10분 동안 다시 받지 않는다.
    assert_eq!(*cache.discovery(k, &realm).await.unwrap(), *doc);
    assert_eq!(idp.hits("discovery"), 1);
    // 영역이 바뀌면(revision) 새로 받는다.
    cache.discovery((k.0, 2), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 2);
    // 캐시 시간이 지나면 새로 받는다.
    let cache = OidcCache::with_times(Duration::ZERO, Duration::ZERO);
    cache.discovery(k, &realm).await.unwrap();
    cache.discovery(k, &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 4);

    // issuer 끝의 /는 discovery 주소에서 하나로 합치지만, 문서의 issuer와는 글자 그대로 비교한다.
    let slash = OidcRealm {
        issuer: format!("{}/", idp.issuer),
        ..realm.clone()
    };
    assert_eq!(
        cache.discovery(key(), &slash).await,
        Err(invalid(format!(
            "the discovery document is for issuer {0}, not {0}/",
            idp.issuer
        )))
    );
    assert_eq!(idp.hits("discovery"), 5);
}

#[tokio::test]
async fn bad_discovery_documents_are_rejected() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    assert_eq!(
        cache.discovery(key(), &realm).await,
        Err(invalid(format!(
            "the discovery document is for issuer https://evil.example.com, not {}",
            idp.issuer
        )))
    );
    idp.set_discovery(|d| d["issuer"] = json!(idp.issuer));
    for field in [
        "authorization_endpoint",
        "token_endpoint",
        "jwks_uri",
        "userinfo_endpoint",
    ] {
        let old = idp.issuer.clone();
        let http = old.replace("https://", "http://") + "/x";
        idp.set_discovery(|d| d[field] = json!(http));
        assert_eq!(
            cache.discovery(key(), &realm).await,
            Err(invalid(format!("{field} {http} is not an https:// URL")))
        );
        idp.set_discovery(|d| d[field] = json!(format!("{old}/x")));
    }
    // userinfo는 없어도 된다. jwks_uri는 있어야 한다.
    idp.set_discovery(|d| {
        d.as_object_mut().unwrap().remove("userinfo_endpoint");
    });
    assert_eq!(
        cache
            .discovery(key(), &realm)
            .await
            .unwrap()
            .userinfo_endpoint,
        None
    );
    idp.set_discovery(|d| {
        d.as_object_mut().unwrap().remove("jwks_uri");
    });
    let Err(OidcError::Invalid(message)) = cache.discovery(key(), &realm).await else {
        panic!("a document without jwks_uri was accepted");
    };
    assert!(message.contains("missing field `jwks_uri`"), "{message}");
}

#[tokio::test]
async fn idps_are_reached_only_with_a_trusted_certificate() {
    let idp = Idp::start().await;
    let cache = OidcCache::default();
    // 시스템 루트는 시험 CA를 믿지 않는다.
    let system = OidcRealm {
        ca_pem: None,
        ..realm(&idp)
    };
    assert!(
        matches!(
            cache.discovery(key(), &system).await,
            Err(OidcError::Unavailable(_))
        ),
        "the test CA was trusted without ca_pem"
    );
    // 다른 CA를 주면 시스템 루트도 쓰지 않는다.
    let other = Idp::start().await;
    let wrong = OidcRealm {
        ca_pem: Some(other.ca_pem.clone()),
        ..realm(&idp)
    };
    assert!(matches!(
        cache.discovery(key(), &wrong).await,
        Err(OidcError::Unavailable(_))
    ));
    assert_eq!(idp.hits("discovery"), 0);
    // 닿지 않는 IdP
    let gone = realm(&other);
    drop(other);
    assert!(matches!(
        cache.discovery(key(), &gone).await,
        Err(OidcError::Unavailable(_))
    ));
}

#[tokio::test]
async fn id_tokens_are_verified_with_cached_keys() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let k = key();
    let doc = cache.discovery(k, &realm).await.unwrap();
    let token = idp.id_token(&idp.claims(Some("n-1")));
    let claims = cache
        .verify_id_token(k, &realm, &doc, &token, "n-1")
        .await
        .unwrap();
    assert_eq!(claims["preferred_username"], "Alice");
    assert_eq!(claims["groups"], json!(["opl-admins"]));
    // ES256 키도 같은 JWKS에 있다.
    idp.sign_with("ec-1");
    let token = idp.id_token(&idp.claims(Some("n-1")));
    cache
        .verify_id_token(k, &realm, &doc, &token, "n-1")
        .await
        .unwrap();
    assert_eq!(idp.hits("jwks"), 1);
    // nonce가 다르면 거절
    assert_eq!(
        cache.verify_id_token(k, &realm, &doc, &token, "n-2").await,
        Err(invalid("the ID token nonce does not match"))
    );
    // alg를 none으로 바꾼 토큰
    let none = idp.sign_raw(&json!({ "alg": "none" }), &idp.claims(Some("n-1")));
    assert_eq!(
        cache.verify_id_token(k, &realm, &doc, &none, "n-1").await,
        Err(invalid(
            "the ID token is signed with none, not RS256 or ES256"
        ))
    );
    // aud·iss가 다른 토큰
    idp.set_claims(json!({ "aud": "someone-else" }));
    let token = idp.id_token(&idp.claims(Some("n-1")));
    assert_eq!(
        cache.verify_id_token(k, &realm, &doc, &token, "n-1").await,
        Err(invalid("the ID token is not for this client (aud)"))
    );
    assert_eq!(idp.hits("jwks"), 1);
}

#[tokio::test]
async fn unknown_key_ids_refetch_the_jwks_at_most_every_ten_seconds() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    // 다시 받는 간격을 길게 해 느린 기계에서도 "방금 받음"이 유지되게 한다.
    let cache = OidcCache::with_times(Duration::from_secs(600), Duration::from_secs(3600));
    let k = key();
    let doc = cache.discovery(k, &realm).await.unwrap();
    let check = async |cache: &OidcCache, kid: &str| {
        idp.sign_with(kid);
        let token = idp.id_token(&idp.claims(Some("n")));
        cache.verify_id_token(k, &realm, &doc, &token, "n").await
    };
    check(&cache, "rsa-1").await.unwrap();
    assert_eq!(idp.hits("jwks"), 1);

    // IdP가 새 키로 바꿨다. 방금 받았으므로(10초 안) 다시 받지 않고 거절한다.
    idp.add_key("rsa-2", Alg::Rs256, true);
    assert_eq!(
        check(&cache, "rsa-2").await,
        Err(invalid("no RS256 key with kid rsa-2 in the JWKS"))
    );
    assert_eq!(idp.hits("jwks"), 1);

    // 10초가 지났다면 한 번 다시 받는다.
    let cache = OidcCache::with_times(Duration::from_secs(600), Duration::ZERO);
    check(&cache, "rsa-1").await.unwrap();
    assert_eq!(idp.hits("jwks"), 2);
    idp.add_key("ec-2", Alg::Es256, true);
    check(&cache, "ec-2").await.unwrap();
    assert_eq!(idp.hits("jwks"), 3);
    check(&cache, "ec-2").await.unwrap();
    assert_eq!(idp.hits("jwks"), 3);
    // JWKS에 없는 키는 다시 받아도 없다(한 번만 다시 받는다).
    idp.add_key("hidden", Alg::Es256, false);
    assert_eq!(
        check(&cache, "hidden").await,
        Err(invalid("no ES256 key with kid hidden in the JWKS"))
    );
    assert_eq!(idp.hits("jwks"), 4);
    // 키가 바뀐 뒤 옛 키로 서명한 토큰은 거절된다.
    idp.publish("rsa-1", false);
    idp.add_key("rsa-3", Alg::Rs256, true);
    check(&cache, "rsa-3").await.unwrap();
    assert_eq!(idp.hits("jwks"), 5);
    assert_eq!(
        check(&cache, "rsa-1").await,
        Err(invalid("no RS256 key with kid rsa-1 in the JWKS"))
    );
    assert_eq!(idp.hits("jwks"), 6);
}

/// 받기 실패는 같은 (영역, revision)에서 15초 동안 기억해 다시 받지 않고 같은 오류를 돌려준다.
#[tokio::test]
async fn fetch_failures_are_remembered() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let k = key();
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    let failed = cache.discovery(k, &realm).await;
    assert!(matches!(failed, Err(OidcError::Invalid(_))), "{failed:?}");
    // IdP가 고쳐져도 기억한 동안은 받지 않는다.
    idp.set_discovery(|d| d["issuer"] = json!(idp.issuer));
    assert_eq!(cache.discovery(k, &realm).await, failed);
    assert_eq!(idp.hits("discovery"), 1);
    // 영역이 바뀌면(revision) 새로 받는다.
    cache.discovery((k.0, 2), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 2);

    // JWKS도 같다(액세스 토큰 없는 userinfo는 401).
    let unauthorized = format!("{}/userinfo", idp.issuer);
    let failed = cache.keys(k, &realm, &unauthorized, None).await;
    assert!(
        matches!(failed, Err(OidcError::Unavailable(_))),
        "{failed:?}"
    );
    assert_eq!(cache.keys(k, &realm, &unauthorized, None).await, failed);
    assert_eq!(idp.hits("userinfo"), 1);
}

/// 같은 영역을 동시에 여러 요청이 물어도 IdP에서는 한 번만 받는다(실패도).
#[tokio::test]
async fn concurrent_requests_fetch_once() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let k = key();
    let docs = futures_util::future::join_all((0..8).map(|_| cache.discovery(k, &realm))).await;
    assert!(docs.iter().all(Result::is_ok), "{docs:?}");
    assert_eq!(idp.hits("discovery"), 1);
    let jwks = format!("{}/jwks", idp.issuer);
    let keys =
        futures_util::future::join_all((0..8).map(|_| cache.keys(k, &realm, &jwks, None))).await;
    assert!(keys.iter().all(Result::is_ok), "{keys:?}");
    assert_eq!(idp.hits("jwks"), 1);
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    let k = key();
    let docs = futures_util::future::join_all((0..8).map(|_| cache.discovery(k, &realm))).await;
    assert!(docs.iter().all(Result::is_err), "{docs:?}");
    assert_eq!(idp.hits("discovery"), 2);
}

/// 늦게 끝난 옛 revision 받기가 새 revision 항목을 지우지 않는다.
#[tokio::test]
async fn the_cache_keeps_the_highest_revision() {
    let idp = Idp::start().await;
    let realm = realm(&idp);
    let cache = OidcCache::default();
    let id = Uuid::new_v4();
    cache.discovery((id, 2), &realm).await.unwrap();
    // 옛 revision을 읽은 요청: 받기는 하지만 남기지 않는다.
    cache.discovery((id, 1), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 2);
    cache.discovery((id, 2), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 2);
    // 더 높은 revision은 옛 항목을 바꾼다.
    cache.discovery((id, 3), &realm).await.unwrap();
    cache.discovery((id, 3), &realm).await.unwrap();
    assert_eq!(idp.hits("discovery"), 3);
}

fn steps(result: &Value) -> Vec<(String, bool)> {
    result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap().into(), s["ok"] == true))
        .collect()
}

/// 영역 시험 동작은 OIDC면 사용자·비밀번호 없이 discovery와 jwks만 확인한다.
#[tokio::test]
async fn the_test_action_checks_discovery_and_jwks() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let idp = Idp::start().await;
    let (status, body) = admin
        .post(
            "/realms",
            json!({ "name": "okta", "backend": idp.backend() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let url = format!("{}/api/v1/realms/okta/actions/test", admin.base);
    let token = admin.token.clone().unwrap();
    // 본문 없이
    let (status, result) = admin.send(admin.http.post(&url).bearer_auth(&token)).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        steps(&result),
        [("discovery".to_string(), true), ("jwks".to_string(), true)]
    );
    assert_eq!(
        result["steps"][1]["message"],
        "1 RS256 and 1 ES256 signing keys"
    );
    assert_eq!(result["dn"], Value::Null);
    assert_eq!(result["groups"], json!([]));
    assert_eq!(result["role"], Value::Null);
    assert!(!result.to_string().contains("client-secret"));
    // 본문이 null이어도 된다(OpenAPI의 nullable 본문).
    let (status, result) = admin
        .send(admin.http.post(&url).bearer_auth(&token).json(&Value::Null))
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(steps(&result).len(), 2);
    // 사용자·비밀번호는 무시한다. 시험은 캐시를 쓰지 않는다.
    let (status, result) = admin
        .post(
            "/realms/okta/actions/test",
            json!({ "username": "alice", "password": "pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(steps(&result).len(), 2);
    assert_eq!((idp.hits("discovery"), idp.hits("jwks")), (3, 3));

    // 서명 키가 없으면 jwks에서 실패
    idp.publish("rsa-1", false);
    idp.publish("ec-1", false);
    let (_, result) = admin.post("/realms/okta/actions/test", json!({})).await;
    assert_eq!(
        steps(&result),
        [("discovery".to_string(), true), ("jwks".to_string(), false)]
    );
    assert_eq!(
        result["steps"][1]["message"],
        format!("{}/jwks has no RS256 or ES256 signing keys", idp.issuer)
    );
    // issuer가 다르면 discovery에서 멈춘다.
    idp.set_discovery(|d| d["issuer"] = json!("https://evil.example.com"));
    let (_, result) = admin.post("/realms/okta/actions/test", json!({})).await;
    assert_eq!(steps(&result), [("discovery".to_string(), false)]);
    assert!(
        result["steps"][0]["message"]
            .as_str()
            .unwrap()
            .contains("https://evil.example.com"),
        "{result}"
    );
    // 닿지 않는 IdP
    drop(idp);
    let (_, result) = admin.post("/realms/okta/actions/test", json!({})).await;
    assert_eq!(steps(&result), [("discovery".to_string(), false)]);

    // operator는 시험할 수 없다(LDAP와 같은 권한).
    let operator = env.user("op", "operator").await;
    assert_eq!(
        operator
            .post("/realms/okta/actions/test", json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
