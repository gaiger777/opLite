//! OIDC 영역의 핵심. IdP HTTP 클라이언트, discovery·JWKS(노드 메모리 캐시 10분), ID 토큰(JWT)
//! 확인(RS256·ES256만), 로그인 상태 쿠키(클러스터 CA 키에서 HKDF로 만든 키의 HMAC)와 PKCE,
//! 영역 시험 동작의 단계(`discovery`, `jwks`). 상태를 쿠키에 담으므로 어느 노드나 로그인을 끝낼 수 있다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use opl_types::realm::OidcRealm;
use ring::rand::SecureRandom;
use ring::{digest, hkdf, hmac, signature};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::realms::{RealmTestResult, RealmTestStep};

/// 시험 동작의 단계 이름
pub const STEP_DISCOVERY: &str = "discovery";
pub const STEP_JWKS: &str = "jwks";

/// 로그인 상태 쿠키 이름
pub const STATE_COOKIE: &str = "opl_oidc";
/// 로그인 상태 쿠키가 유효한 시간(초)
pub const STATE_TTL_SECS: u64 = 600;
/// ID 토큰의 `exp`·`iat`에 허용하는 시계 차(초)
pub const CLOCK_SKEW_SECS: i64 = 300;
/// discovery·JWKS를 노드 메모리에 두는 시간
pub const CACHE_TTL: Duration = Duration::from_secs(600);
/// 모르는 `kid`로 JWKS를 다시 받는 가장 짧은 간격(받기 폭주 방지)
pub const MIN_REFETCH: Duration = Duration::from_secs(10);
/// discovery·JWKS 받기 실패를 기억하는 시간(막힌 IdP로 요청을 쏟지 않게)
pub const FAILURE_TTL: Duration = Duration::from_secs(15);
/// IdP 응답 본문의 최대 크기
const MAX_BODY: usize = 1 << 20;
/// 상태 쿠키 HMAC 키를 만드는 HKDF 정보
const STATE_KEY_INFO: &[u8] = b"opl-oidc-state";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OidcError {
    /// IdP에 닿지 못했다(연결·TLS·시간 제한·2xx가 아닌 응답).
    #[error("{0}")]
    Unavailable(String),
    /// IdP의 응답이나 토큰·쿠키가 맞지 않다.
    #[error("{0}")]
    Invalid(String),
}

fn invalid(message: impl Into<String>) -> OidcError {
    OidcError::Invalid(message.into())
}

/// 캐시 키: 영역 id와 revision. 영역을 바꾸면 새로 받는다.
pub type RealmKey = (Uuid, u64);

/// discovery 문서에서 쓰는 값
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
}

/// JWKS의 서명 키 하나(RS256 또는 ES256)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jwk {
    pub kid: Option<String>,
    pub key: JwkKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwkKey {
    /// RS256: 모듈러스와 지수(빅 엔디언)
    Rsa { n: Vec<u8>, e: Vec<u8> },
    /// ES256: P-256 비압축 점 `04 || x || y`
    Ec { point: Vec<u8> },
}

impl Jwk {
    fn alg(&self) -> &'static str {
        match self.key {
            JwkKey::Rsa { .. } => "RS256",
            JwkKey::Ec { .. } => "ES256",
        }
    }
}

/// IdP 요청용 HTTP 클라이언트. `ca_pem`이 있으면 그 인증서만 루트로 믿고, 없으면 시스템 루트.
/// 리다이렉트를 따르지 않고 https만 쓴다.
pub fn client(realm: &OidcRealm) -> Result<reqwest::Client, OidcError> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(realm.timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true);
    if let Some(pem) = &realm.ca_pem {
        let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
            .map_err(|e| invalid(format!("ca_pem: {}", crate::telemetry::chain(&e))))?;
        if certs.is_empty() {
            return Err(invalid("ca_pem has no certificates"));
        }
        builder = builder.tls_certs_only(certs);
    }
    builder
        .build()
        .map_err(|e| invalid(crate::telemetry::chain(&e)))
}

/// `url`을 GET해 JSON으로 읽는다. 닿지 못했거나 2xx가 아니면 `Unavailable`.
pub(crate) async fn get_json(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
) -> Result<Value, OidcError> {
    let mut request = client.get(url).header("accept", "application/json");
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|e| OidcError::Unavailable(format!("{url}: {}", crate::telemetry::chain(&e))))?;
    let status = response.status();
    if !status.is_success() {
        return Err(OidcError::Unavailable(format!("{url}: HTTP {status}")));
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| OidcError::Unavailable(format!("{url}: {}", crate::telemetry::chain(&e))))?;
    if body.len() > MAX_BODY {
        return Err(invalid(format!("{url}: response is too large")));
    }
    serde_json::from_slice(&body).map_err(|e| invalid(format!("{url}: {e}")))
}

/// discovery 문서 주소. issuer 끝의 `/`는 하나로 합친다.
pub fn discovery_url(issuer: &str) -> String {
    let base = issuer.strip_suffix('/').unwrap_or(issuer);
    format!("{base}/.well-known/openid-configuration")
}

fn is_https(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty() && !rest.starts_with('/') && !url.chars().any(char::is_whitespace)
    })
}

/// discovery 문서를 받아 확인한다: `issuer`가 설정값과 글자 그대로 같고 끝점이 https다.
pub async fn fetch_discovery(
    client: &reqwest::Client,
    realm: &OidcRealm,
) -> Result<Discovery, OidcError> {
    let url = discovery_url(&realm.issuer);
    let doc: Discovery = serde_json::from_value(get_json(client, &url, None).await?)
        .map_err(|e| invalid(format!("{url}: {e}")))?;
    if doc.issuer != realm.issuer {
        return Err(invalid(format!(
            "the discovery document is for issuer {}, not {}",
            doc.issuer, realm.issuer
        )));
    }
    let endpoints = [
        ("authorization_endpoint", Some(&doc.authorization_endpoint)),
        ("token_endpoint", Some(&doc.token_endpoint)),
        ("jwks_uri", Some(&doc.jwks_uri)),
        ("userinfo_endpoint", doc.userinfo_endpoint.as_ref()),
    ];
    for (name, url) in endpoints {
        if let Some(url) = url
            && !is_https(url)
        {
            return Err(invalid(format!("{name} {url} is not an https:// URL")));
        }
    }
    Ok(doc)
}

/// JWKS를 받아 쓸 수 있는 서명 키만 고른다.
pub async fn fetch_jwks(client: &reqwest::Client, jwks_uri: &str) -> Result<Vec<Jwk>, OidcError> {
    Ok(parse_jwks(&get_json(client, jwks_uri, None).await?))
}

/// JWKS의 `keys`에서 RS256(`kty: RSA`)·ES256(`kty: EC`, `crv: P-256`) 서명 키만 고른다. `alg`가 있으면
/// 그 알고리즘이어야 하고 `use`가 있으면 `sig`여야 한다. 나머지와 깨진 키는 건너뛴다.
pub fn parse_jwks(doc: &Value) -> Vec<Jwk> {
    let Some(keys) = doc.get("keys").and_then(Value::as_array) else {
        return Vec::new();
    };
    keys.iter().filter_map(parse_jwk).collect()
}

fn parse_jwk(k: &Value) -> Option<Jwk> {
    let text = |name: &str| k.get(name).and_then(Value::as_str);
    let bytes = |name: &str| text(name).and_then(|v| URL_SAFE_NO_PAD.decode(v).ok());
    if text("use").is_some_and(|u| u != "sig") {
        return None;
    }
    let alg = text("alg");
    let key = match text("kty")? {
        "RSA" if alg.is_none_or(|a| a == "RS256") => JwkKey::Rsa {
            n: bytes("n")?,
            e: bytes("e")?,
        },
        "EC" if text("crv") == Some("P-256") && alg.is_none_or(|a| a == "ES256") => {
            let (x, y) = (bytes("x")?, bytes("y")?);
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            JwkKey::Ec {
                point: [&[0x04][..], &x, &y].concat(),
            }
        }
        _ => return None,
    };
    Some(Jwk {
        kid: text("kid").map(str::to_string),
        key,
    })
}

/// ID 토큰에서 확인할 값
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
}

/// JWT의 헤더
#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
    /// 꼭 이해해야 하는 확장(RFC 7515 §4.1.11). 아무것도 지원하지 않으므로 있으면 거절한다.
    #[serde(default)]
    crit: Option<Value>,
}

fn decode_part(part: &str, what: &str) -> Result<Vec<u8>, OidcError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| invalid(format!("the ID token {what} is not base64url")))
}

/// ID 토큰 헤더의 `kid`
pub fn token_kid(token: &str) -> Option<String> {
    let header = token.split('.').next()?;
    serde_json::from_slice::<Header>(&URL_SAFE_NO_PAD.decode(header).ok()?)
        .ok()?
        .kid
}

/// ID 토큰(JWS compact)을 `keys`로 확인하고 클레임을 돌려준다. `alg`는 RS256·ES256만이고 `crit`가 있으면
/// 거절한다. `kid`가 있으면 그 키로(JWKS에 그 kid가 없으면 kid 없는 키 가운데 alg가 맞는 것으로), 없으면
/// alg가 맞는 키를 모두 시도한다. 클레임: `iss`, `aud`(client_id 포함, 여럿이면 `azp`), `exp`·`iat`·`nbf`
/// (5분 오차), `nonce`, `sub`. `now`는 유닉스 초.
pub fn verify_id_token(
    token: &str,
    keys: &[Jwk],
    expected: &Expected<'_>,
    now: i64,
) -> Result<Map<String, Value>, OidcError> {
    let parts: Vec<&str> = token.split('.').collect();
    let [header_b64, payload_b64, signature_b64] = parts[..] else {
        return Err(invalid("the ID token is not a signed JWT"));
    };
    let header: Header = serde_json::from_slice(&decode_part(header_b64, "header")?)
        .map_err(|e| invalid(format!("the ID token header: {e}")))?;
    if header.alg != "RS256" && header.alg != "ES256" {
        return Err(invalid(format!(
            "the ID token is signed with {}, not RS256 or ES256",
            header.alg
        )));
    }
    if header.crit.is_some() {
        return Err(invalid("the ID token header has crit extensions"));
    }
    let same_alg = || keys.iter().filter(|k| k.alg() == header.alg);
    let mut candidates: Vec<&Jwk> = same_alg()
        .filter(|k| header.kid.is_none() || k.kid == header.kid)
        .collect();
    // kid가 JWKS에 없으면 kid 없이 내보인 키를 시도한다.
    if candidates.is_empty() && header.kid.is_some() {
        candidates = same_alg().filter(|k| k.kid.is_none()).collect();
    }
    if candidates.is_empty() {
        return Err(invalid(match &header.kid {
            Some(kid) => format!("no {} key with kid {kid} in the JWKS", header.alg),
            None => format!("no {} key in the JWKS", header.alg),
        }));
    }
    let signature = decode_part(signature_b64, "signature")?;
    let input = &token[..header_b64.len() + 1 + payload_b64.len()];
    let verified = candidates.iter().any(|k| match &k.key {
        JwkKey::Rsa { n, e } => signature::RsaPublicKeyComponents { n, e }
            .verify(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                input.as_bytes(),
                &signature,
            )
            .is_ok(),
        JwkKey::Ec { point } => {
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                .verify(input.as_bytes(), &signature)
                .is_ok()
        }
    });
    if !verified {
        return Err(invalid("the ID token signature is not valid"));
    }
    let claims: Map<String, Value> = serde_json::from_slice(&decode_part(payload_b64, "payload")?)
        .map_err(|e| invalid(format!("the ID token claims: {e}")))?;
    check_claims(&claims, expected, now)?;
    Ok(claims)
}

fn check_claims(
    claims: &Map<String, Value>,
    expected: &Expected<'_>,
    now: i64,
) -> Result<(), OidcError> {
    let text = |name: &str| claims.get(name).and_then(Value::as_str);
    let time = |name: &str| {
        claims
            .get(name)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .ok_or_else(|| invalid(format!("the ID token has no {name}")))
    };
    if text("iss") != Some(expected.issuer) {
        return Err(invalid(format!(
            "the ID token issuer is {}, not {}",
            claims.get("iss").unwrap_or(&Value::Null),
            expected.issuer
        )));
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !audiences.contains(&expected.client_id) {
        return Err(invalid("the ID token is not for this client (aud)"));
    }
    // 여러 대상에게 발급된 토큰은 이 클라이언트가 받은 것이어야 한다.
    let azp = text("azp");
    if (audiences.len() > 1 || azp.is_some()) && azp != Some(expected.client_id) {
        return Err(invalid("the ID token was not issued to this client (azp)"));
    }
    if time("exp")? <= now - CLOCK_SKEW_SECS {
        return Err(invalid("the ID token has expired"));
    }
    if time("iat")? > now + CLOCK_SKEW_SECS {
        return Err(invalid("the ID token was issued in the future (iat)"));
    }
    if claims.contains_key("nbf") && time("nbf")? > now + CLOCK_SKEW_SECS {
        return Err(invalid("the ID token is not valid yet (nbf)"));
    }
    if text("nonce") != Some(expected.nonce) {
        return Err(invalid("the ID token nonce does not match"));
    }
    if text("sub").is_none_or(str::is_empty) {
        return Err(invalid("the ID token has no sub"));
    }
    Ok(())
}

/// 지금 유닉스 초
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 영역마다 받은 값(받은 시각)과 최근 실패, 같은 영역의 받기를 하나로 줄이는 잠금
struct Cached<T> {
    entries: Mutex<HashMap<RealmKey, Entry<T>>>,
}

struct Entry<T> {
    value: Option<(Instant, T)>,
    failure: Option<(Instant, OidcError)>,
    fetching: Arc<tokio::sync::Mutex<()>>,
}

impl<T> Default for Cached<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
        }
    }
}

impl<T: Clone> Cached<T> {
    /// 키의 항목. 같은 영역의 더 높은 revision 항목이 있으면 None이다(늦게 끝난 옛 revision 받기가 새
    /// 항목을 지우지 않게). 낮은 revision 항목은 지운다.
    fn entry(map: &mut HashMap<RealmKey, Entry<T>>, key: RealmKey) -> Option<&mut Entry<T>> {
        if map.keys().any(|k| k.0 == key.0 && k.1 > key.1) {
            return None;
        }
        map.retain(|k, _| k.0 != key.0 || k.1 == key.1);
        Some(map.entry(key).or_insert_with(|| Entry {
            value: None,
            failure: None,
            fetching: Arc::default(),
        }))
    }

    /// 받은 값을 `usable`이 쓸 수 있다고 하면 그것, 최근(`FAILURE_TTL` 안)에 실패했으면 그 오류
    fn ready(
        &self,
        key: RealmKey,
        usable: &impl Fn(Instant, &T) -> bool,
    ) -> Option<Result<T, OidcError>> {
        let map = self.entries.lock().unwrap();
        let entry = map.get(&key)?;
        if let Some((at, value)) = &entry.value
            && usable(*at, value)
        {
            return Some(Ok(value.clone()));
        }
        entry
            .failure
            .as_ref()
            .filter(|(at, _)| at.elapsed() < FAILURE_TTL)
            .map(|(_, e)| Err(e.clone()))
    }

    /// 쓸 수 있는 값이나 최근 실패가 없으면 받는다. 같은 영역의 받기는 한 번에 하나만 하고, 기다린
    /// 요청은 그 결과를 쓴다.
    async fn get_or_fetch<F>(
        &self,
        key: RealmKey,
        usable: impl Fn(Instant, &T) -> bool,
        fetch: impl FnOnce() -> F,
    ) -> Result<T, OidcError>
    where
        F: Future<Output = Result<T, OidcError>>,
    {
        if let Some(ready) = self.ready(key, &usable) {
            return ready;
        }
        let lock = Self::entry(&mut self.entries.lock().unwrap(), key)
            .map_or_else(Arc::default, |e| e.fetching.clone());
        let _fetching = lock.lock().await;
        // 기다리는 동안 다른 요청이 받았을 수 있다.
        if let Some(ready) = self.ready(key, &usable) {
            return ready;
        }
        let result = fetch().await;
        if let Some(entry) = Self::entry(&mut self.entries.lock().unwrap(), key) {
            match &result {
                Ok(value) => {
                    entry.value = Some((Instant::now(), value.clone()));
                    entry.failure = None;
                }
                Err(e) => entry.failure = Some((Instant::now(), e.clone())),
            }
        }
        result
    }
}

/// 노드 메모리의 discovery·JWKS 캐시. 키는 (영역 id, revision)이고 영역마다 가장 높은 revision만 둔다.
/// 받기 실패는 `FAILURE_TTL` 동안 기억해 같은 오류를 돌려준다.
pub struct OidcCache {
    ttl: Duration,
    min_refetch: Duration,
    discovery: Cached<Arc<Discovery>>,
    jwks: Cached<Arc<Vec<Jwk>>>,
}

impl Default for OidcCache {
    fn default() -> Self {
        Self::with_times(CACHE_TTL, MIN_REFETCH)
    }
}

impl OidcCache {
    /// 캐시 시간(`ttl`)과 모르는 `kid`로 다시 받는 가장 짧은 간격(`min_refetch`)을 정한다(시험용).
    pub fn with_times(ttl: Duration, min_refetch: Duration) -> Self {
        Self {
            ttl,
            min_refetch,
            discovery: Cached::default(),
            jwks: Cached::default(),
        }
    }

    /// 영역의 discovery 문서. 캐시에 없거나 오래됐으면 받는다.
    pub async fn discovery(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
    ) -> Result<Arc<Discovery>, OidcError> {
        self.discovery
            .get_or_fetch(
                key,
                |at, _| at.elapsed() < self.ttl,
                || async { Ok(Arc::new(fetch_discovery(&client(realm)?, realm).await?)) },
            )
            .await
    }

    /// 영역의 서명 키. 캐시에 없거나 오래됐으면 받는다. `missing_kid`가 있고 캐시에 그 키가 없으면,
    /// 마지막으로 받은 지 `min_refetch`가 지났을 때만 다시 받는다.
    pub async fn keys(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
        jwks_uri: &str,
        kid: Option<&str>,
    ) -> Result<Arc<Vec<Jwk>>, OidcError> {
        let usable = |at: Instant, keys: &Arc<Vec<Jwk>>| {
            let known = kid.is_none_or(|kid| keys.iter().any(|k| k.kid.as_deref() == Some(kid)));
            at.elapsed() < self.ttl && (known || at.elapsed() < self.min_refetch)
        };
        self.jwks
            .get_or_fetch(key, usable, || async {
                Ok(Arc::new(fetch_jwks(&client(realm)?, jwks_uri).await?))
            })
            .await
    }

    /// 영역의 ID 토큰을 확인한다. 모르는 `kid`면 JWKS를 한 번 다시 받는다([`OidcCache::keys`]).
    pub async fn verify_id_token(
        &self,
        key: RealmKey,
        realm: &OidcRealm,
        discovery: &Discovery,
        token: &str,
        nonce: &str,
    ) -> Result<Map<String, Value>, OidcError> {
        let kid = token_kid(token);
        let keys = self
            .keys(key, realm, &discovery.jwks_uri, kid.as_deref())
            .await?;
        let expected = Expected {
            issuer: &realm.issuer,
            client_id: &realm.client_id,
            nonce,
        };
        verify_id_token(token, &keys, &expected, unix_now())
    }
}

/// 영역의 IdP를 시험한다: discovery 문서(issuer 같음·끝점 https)와 JWKS(RS256·ES256 키 수). 캐시를
/// 쓰지 않고 새로 받는다. 실패한 단계에서 멈춘다.
pub async fn test(realm: &OidcRealm) -> RealmTestResult {
    let mut steps = Vec::new();
    let mut step = |name: &str, result: Result<String, OidcError>| {
        let ok = result.is_ok();
        steps.push(RealmTestStep {
            name: name.into(),
            ok,
            message: result.unwrap_or_else(|e| e.to_string()),
        });
        ok
    };
    let discovered = match client(realm) {
        Ok(client) => fetch_discovery(&client, realm)
            .await
            .map(|doc| (client, doc)),
        Err(e) => Err(e),
    };
    let found = discovered.as_ref().map(|(_, doc)| {
        format!(
            "issuer {}, authorization endpoint {}, token endpoint {}",
            doc.issuer, doc.authorization_endpoint, doc.token_endpoint
        )
    });
    if step(STEP_DISCOVERY, found.map_err(Clone::clone))
        && let Ok((client, doc)) = &discovered
    {
        let keys = fetch_jwks(client, &doc.jwks_uri).await.and_then(|keys| {
            let rsa = keys.iter().filter(|k| k.alg() == "RS256").count();
            let ec = keys.len() - rsa;
            if keys.is_empty() {
                Err(invalid(format!(
                    "{} has no RS256 or ES256 signing keys",
                    doc.jwks_uri
                )))
            } else {
                Ok(format!("{rsa} RS256 and {ec} ES256 signing keys"))
            }
        });
        step(STEP_JWKS, keys);
    }
    RealmTestResult {
        steps,
        dn: None,
        groups: Vec::new(),
        role: None,
    }
}

/// 무작위 32바이트(base64url). `state`·`nonce`·PKCE 검증자에 쓴다.
pub fn random_value() -> String {
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("system random");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE S256 `code_challenge` = base64url(SHA-256(verifier))
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}

/// 상태 쿠키의 HMAC 키. 클러스터 CA 개인키(PEM)에서 HKDF-SHA256(salt 없음, 정보 `opl-oidc-state`)으로
/// 만든 32바이트라 모든 노드가 같은 키를 얻는다.
pub fn state_key(ca_key_pem: &str) -> hmac::Key {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(ca_key_pem.as_bytes());
    let okm = prk
        .expand(&[STATE_KEY_INFO], hmac::HMAC_SHA256)
        .expect("32 bytes is a valid HKDF length");
    hmac::Key::from(okm)
}

/// 로그인을 시작할 때 만들어 쿠키에 담는 값
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginState {
    pub realm: String,
    pub state: String,
    pub nonce: String,
    /// PKCE 검증자
    pub verifier: String,
    /// 만든 시각(유닉스 초)
    pub created: i64,
}

// 검증자가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for LoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginState")
            .field("realm", &self.realm)
            .field("created", &self.created)
            .finish_non_exhaustive()
    }
}

/// 상수 시간 비교
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl LoginState {
    /// 영역의 새 로그인 상태(무작위 `state`·`nonce`·검증자)
    pub fn new(realm: &str, now: i64) -> Self {
        Self {
            realm: realm.into(),
            state: random_value(),
            nonce: random_value(),
            verifier: random_value(),
            created: now,
        }
    }

    /// 쿠키 값: base64url(JSON) `.` base64url(HMAC-SHA256)
    pub fn seal(&self, key: &hmac::Key) -> String {
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("serializable"));
        let tag = hmac::sign(key, body.as_bytes());
        format!("{body}.{}", URL_SAFE_NO_PAD.encode(tag))
    }

    /// 쿠키 값을 확인한다: 서명, 만든 지 10분 안, 쿼리의 `state`와 같음.
    pub fn open(
        cookie: &str,
        key: &hmac::Key,
        query_state: &str,
        now: i64,
    ) -> Result<Self, OidcError> {
        let (body, tag) = cookie
            .split_once('.')
            .ok_or_else(|| invalid("the login state cookie is malformed"))?;
        let tag = URL_SAFE_NO_PAD
            .decode(tag)
            .map_err(|_| invalid("the login state cookie is malformed"))?;
        hmac::verify(key, body.as_bytes(), &tag)
            .map_err(|_| invalid("the login state cookie signature is not valid"))?;
        let state: LoginState = URL_SAFE_NO_PAD
            .decode(body)
            .ok()
            .and_then(|json| serde_json::from_slice(&json).ok())
            .ok_or_else(|| invalid("the login state cookie is malformed"))?;
        if now - state.created > STATE_TTL_SECS as i64 || state.created > now + CLOCK_SKEW_SECS {
            return Err(invalid("the login state cookie has expired"));
        }
        if !same(state.state.as_bytes(), query_state.as_bytes()) {
            return Err(invalid("the state does not match the login state cookie"));
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, RsaKeyPair};
    use serde_json::json;

    const RSA_PK8: &[u8] = include_bytes!("../tests/fixtures/oidc-rsa.pk8");
    const ISSUER: &str = "https://idp.example.com/";
    const NOW: i64 = 1_800_000_000;

    fn b64(bytes: impl AsRef<[u8]>) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn rsa() -> RsaKeyPair {
        RsaKeyPair::from_pkcs8(RSA_PK8).unwrap()
    }

    fn rsa_jwk(kid: Option<&str>) -> Jwk {
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = rsa().public().into();
        Jwk {
            kid: kid.map(str::to_string),
            key: JwkKey::Rsa {
                n: public.n,
                e: public.e,
            },
        }
    }

    fn ec() -> EcdsaKeyPair {
        let rng = SystemRandom::new();
        let pk8 = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .unwrap();
        EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pk8.as_ref(),
            &rng,
        )
        .unwrap()
    }

    fn ec_jwk(key: &EcdsaKeyPair, kid: Option<&str>) -> Jwk {
        Jwk {
            kid: kid.map(str::to_string),
            key: JwkKey::Ec {
                point: key.public_key().as_ref().to_vec(),
            },
        }
    }

    fn claims() -> Value {
        json!({
            "iss": ISSUER,
            "sub": "u-1",
            "aud": "oplite",
            "exp": NOW + 60,
            "iat": NOW,
            "nonce": "n-1",
            "preferred_username": "Alice",
        })
    }

    fn input(header: &Value, claims: &Value) -> String {
        format!(
            "{}.{}",
            b64(serde_json::to_vec(header).unwrap()),
            b64(serde_json::to_vec(claims).unwrap())
        )
    }

    fn sign_rs256(header: &Value, claims: &Value) -> String {
        let input = input(header, claims);
        let key = rsa();
        let mut sig = vec![0; key.public().modulus_len()];
        key.sign(
            &signature::RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{input}.{}", b64(sig))
    }

    fn sign_es256(key: &EcdsaKeyPair, header: &Value, claims: &Value) -> String {
        let input = input(header, claims);
        let sig = key.sign(&SystemRandom::new(), input.as_bytes()).unwrap();
        format!("{input}.{}", b64(sig))
    }

    fn expected() -> Expected<'static> {
        Expected {
            issuer: ISSUER,
            client_id: "oplite",
            nonce: "n-1",
        }
    }

    fn verify(token: &str, keys: &[Jwk]) -> Result<Map<String, Value>, OidcError> {
        verify_id_token(token, keys, &expected(), NOW)
    }

    fn err(message: &str) -> Result<Map<String, Value>, OidcError> {
        Err(OidcError::Invalid(message.into()))
    }

    #[test]
    fn rs256_and_es256_tokens_are_verified() {
        let token = sign_rs256(&json!({ "alg": "RS256", "kid": "r1" }), &claims());
        let got = verify(&token, &[rsa_jwk(Some("r1"))]).unwrap();
        assert_eq!(got["preferred_username"], "Alice");
        assert_eq!(token_kid(&token).as_deref(), Some("r1"));

        let key = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256", "kid": "e1" }), &claims());
        let keys = [rsa_jwk(Some("r1")), ec_jwk(&key, Some("e1"))];
        assert_eq!(verify(&token, &keys).unwrap()["sub"], "u-1");

        // kid가 없으면 alg가 맞는 키를 모두 시도한다.
        let other = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256" }), &claims());
        let keys = [ec_jwk(&other, Some("x")), ec_jwk(&key, Some("e1"))];
        assert!(verify(&token, &keys).is_ok());
        assert_eq!(token_kid(&token), None);
    }

    #[test]
    fn other_algorithms_are_rejected() {
        let payload = b64(serde_json::to_vec(&claims()).unwrap());
        // 서명 없는 토큰
        let none = format!("{}.{payload}.", b64(br#"{"alg":"none"}"#));
        assert_eq!(
            verify(&none, &[rsa_jwk(None)]),
            err("the ID token is signed with none, not RS256 or ES256")
        );
        // 공개 키를 HMAC 비밀로 쓰게 하는 공격
        let header = b64(br#"{"alg":"HS256"}"#);
        let input = format!("{header}.{payload}");
        let JwkKey::Rsa { n, .. } = rsa_jwk(None).key else {
            unreachable!()
        };
        let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &n), input.as_bytes());
        let hs256 = format!("{input}.{}", b64(tag));
        assert_eq!(
            verify(&hs256, &[rsa_jwk(None)]),
            err("the ID token is signed with HS256, not RS256 or ES256")
        );
        // 점 둘이 아닌 토큰(JWE 등)
        assert_eq!(
            verify("a.b.c.d.e", &[rsa_jwk(None)]),
            err("the ID token is not a signed JWT")
        );
    }

    #[test]
    fn signatures_and_keys_must_match() {
        let token = sign_rs256(&json!({ "alg": "RS256", "kid": "r1" }), &claims());
        // 클레임을 바꾼 토큰
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        let mut changed = claims();
        changed["preferred_username"] = json!("root");
        parts[1] = b64(serde_json::to_vec(&changed).unwrap());
        assert_eq!(
            verify(&parts.join("."), &[rsa_jwk(Some("r1"))]),
            err("the ID token signature is not valid")
        );
        // 서명 바이트를 바꾼 토큰
        let mut sig = URL_SAFE_NO_PAD
            .decode(token.rsplit('.').next().unwrap())
            .unwrap();
        sig[10] ^= 1;
        let (input, _) = token.rsplit_once('.').unwrap();
        assert_eq!(
            verify(&format!("{input}.{}", b64(sig)), &[rsa_jwk(Some("r1"))]),
            err("the ID token signature is not valid")
        );
        // 다른 키로 서명한 ES256
        let token = sign_es256(&ec(), &json!({ "alg": "ES256", "kid": "e1" }), &claims());
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e1"))]),
            err("the ID token signature is not valid")
        );
        // 모르는 kid, alg가 맞는 키 없음
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e2"))]),
            err("no ES256 key with kid e1 in the JWKS")
        );
        let token = sign_es256(&ec(), &json!({ "alg": "ES256" }), &claims());
        assert_eq!(
            verify(&token, &[rsa_jwk(None)]),
            err("no ES256 key in the JWKS")
        );
    }

    #[test]
    fn claims_are_checked() {
        let keys = [rsa_jwk(None)];
        let check = |f: &dyn Fn(&mut Value)| {
            let mut c = claims();
            f(&mut c);
            verify(&sign_rs256(&json!({ "alg": "RS256" }), &c), &keys).map(|_| ())
        };
        let fail = |m: &str| Err(OidcError::Invalid(m.into()));
        assert_eq!(check(&|_| {}), Ok(()));
        // iss는 글자 그대로(끝 / 포함)
        assert_eq!(
            check(&|c| c["iss"] = json!("https://idp.example.com")),
            fail(
                "the ID token issuer is \"https://idp.example.com\", not https://idp.example.com/"
            )
        );
        let aud = "the ID token is not for this client (aud)";
        assert_eq!(check(&|c| c["aud"] = json!("other")), fail(aud));
        assert_eq!(check(&|c| c["aud"] = json!(["a", "b"])), fail(aud));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("aud");
            }),
            fail(aud)
        );
        assert_eq!(check(&|c| c["aud"] = json!(["oplite"])), Ok(()));
        // 대상이 여럿이면 azp가 이 클라이언트여야 한다.
        let azp = "the ID token was not issued to this client (azp)";
        assert_eq!(check(&|c| c["aud"] = json!(["oplite", "api"])), fail(azp));
        assert_eq!(
            check(&|c| {
                c["aud"] = json!(["oplite", "api"]);
                c["azp"] = json!("api");
            }),
            fail(azp)
        );
        assert_eq!(
            check(&|c| {
                c["aud"] = json!(["oplite", "api"]);
                c["azp"] = json!("oplite");
            }),
            Ok(())
        );
        // exp·iat는 5분 오차
        let expired = "the ID token has expired";
        assert_eq!(check(&|c| c["exp"] = json!(NOW - 300)), fail(expired));
        assert_eq!(check(&|c| c["exp"] = json!(NOW - 299)), Ok(()));
        assert_eq!(
            check(&|c| c["exp"] = json!("soon")),
            fail("the ID token has no exp")
        );
        let future = "the ID token was issued in the future (iat)";
        assert_eq!(check(&|c| c["iat"] = json!(NOW + 301)), fail(future));
        assert_eq!(check(&|c| c["iat"] = json!(NOW + 300)), Ok(()));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("iat");
            }),
            fail("the ID token has no iat")
        );
        let nonce = "the ID token nonce does not match";
        assert_eq!(check(&|c| c["nonce"] = json!("n-2")), fail(nonce));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("nonce");
            }),
            fail(nonce)
        );
        let sub = "the ID token has no sub";
        assert_eq!(check(&|c| c["sub"] = json!("")), fail(sub));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("sub");
            }),
            fail(sub)
        );
    }

    #[test]
    fn crit_headers_and_future_nbf_are_rejected() {
        let keys = [rsa_jwk(None)];
        let token = sign_rs256(&json!({ "alg": "RS256", "crit": ["exp"] }), &claims());
        assert_eq!(
            verify(&token, &keys),
            err("the ID token header has crit extensions")
        );
        let with_nbf = |nbf: Value| {
            let mut c = claims();
            c["nbf"] = nbf;
            verify(&sign_rs256(&json!({ "alg": "RS256" }), &c), &keys).map(|_| ())
        };
        assert_eq!(with_nbf(json!(NOW + 300)), Ok(()));
        assert_eq!(
            with_nbf(json!(NOW + 301)),
            Err(invalid("the ID token is not valid yet (nbf)"))
        );
        assert_eq!(
            with_nbf(json!("later")),
            Err(invalid("the ID token has no nbf"))
        );
    }

    #[test]
    fn an_unknown_kid_falls_back_to_keys_without_a_kid() {
        let key = ec();
        let token = sign_es256(&key, &json!({ "alg": "ES256", "kid": "e9" }), &claims());
        // JWKS에 e9가 없으면 kid 없는 ES256 키를 시도한다.
        assert!(verify(&token, &[rsa_jwk(None), ec_jwk(&key, None)]).is_ok());
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), None)]),
            err("the ID token signature is not valid")
        );
        // kid가 있는 키로는 바꾸지 않는다.
        assert_eq!(
            verify(&token, &[ec_jwk(&key, Some("e1"))]),
            err("no ES256 key with kid e9 in the JWKS")
        );
        // JWKS에 e9가 있으면 그 키만 쓴다.
        assert_eq!(
            verify(&token, &[ec_jwk(&ec(), Some("e9")), ec_jwk(&key, None)]),
            err("the ID token signature is not valid")
        );
    }

    #[test]
    fn jwks_keeps_only_rs256_and_es256_signing_keys() {
        let key = ec();
        let point = key.public_key().as_ref();
        let (x, y) = (b64(&point[1..33]), b64(&point[33..]));
        let JwkKey::Rsa { n, e } = rsa_jwk(None).key else {
            unreachable!()
        };
        let (n, e) = (b64(n), b64(e));
        let doc = json!({ "keys": [
            { "kty": "RSA", "kid": "r1", "n": n, "e": e },
            { "kty": "RSA", "kid": "r2", "alg": "RS256", "use": "sig", "n": n, "e": e },
            { "kty": "EC", "kid": "e1", "crv": "P-256", "x": x, "y": y },
            { "kty": "EC", "kid": "e2", "crv": "P-256", "alg": "ES256", "x": x, "y": y },
            // 쓰지 않는 키
            { "kty": "RSA", "kid": "enc", "use": "enc", "n": n, "e": e },
            { "kty": "RSA", "kid": "ps", "alg": "PS256", "n": n, "e": e },
            { "kty": "EC", "kid": "p384", "crv": "P-384", "x": x, "y": y },
            { "kty": "EC", "kid": "short", "crv": "P-256", "x": b64([1u8; 31]), "y": y },
            { "kty": "oct", "kid": "hs", "k": "c2VjcmV0" },
            { "kty": "RSA", "kid": "broken", "n": "!!", "e": e },
        ]});
        let keys = parse_jwks(&doc);
        let kids: Vec<_> = keys.iter().map(|k| k.kid.as_deref().unwrap()).collect();
        assert_eq!(kids, ["r1", "r2", "e1", "e2"]);
        // EC 점은 비압축 형식 04 || x || y
        assert_eq!(keys[2], ec_jwk(&key, Some("e1")));
        assert_eq!(keys[0], rsa_jwk(Some("r1")));
        assert!(parse_jwks(&json!({})).is_empty());
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        // RFC 7636 부록 B
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let a = random_value();
        assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), 32);
        assert_ne!(a, random_value());
    }

    #[test]
    fn discovery_url_joins_one_slash() {
        assert_eq!(
            discovery_url("https://idp.example.com/realms/corp/"),
            "https://idp.example.com/realms/corp/.well-known/openid-configuration"
        );
        assert_eq!(
            discovery_url("https://idp.example.com"),
            "https://idp.example.com/.well-known/openid-configuration"
        );
    }

    #[test]
    fn state_cookies_are_signed_and_expire() {
        let key = state_key("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        let state = LoginState::new("okta", NOW);
        assert_ne!(state.state, state.nonce);
        let cookie = state.seal(&key);
        assert_eq!(
            LoginState::open(&cookie, &key, &state.state, NOW),
            Ok(state.clone())
        );
        assert_eq!(
            LoginState::open(&cookie, &key, &state.state, NOW + 600),
            Ok(state.clone())
        );
        let open = |cookie: &str, key: &hmac::Key, query: &str, now: i64| {
            LoginState::open(cookie, key, query, now).map(|_| ())
        };
        let fail = |m: &str| Err(OidcError::Invalid(m.into()));
        assert_eq!(
            open(&cookie, &key, &state.state, NOW + 601),
            fail("the login state cookie has expired")
        );
        assert_eq!(
            open(&cookie, &key, "other", NOW),
            fail("the state does not match the login state cookie")
        );
        assert_eq!(
            open(&cookie, &key, &state.state[1..], NOW),
            fail("the state does not match the login state cookie")
        );
        // 다른 클러스터의 키
        let other = state_key("other");
        assert_eq!(
            open(&cookie, &other, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        // 바뀐 바이트(본문을 바꾸면 서명이 맞지 않는다)
        let mut forged = LoginState {
            created: NOW + 3600,
            ..state.clone()
        }
        .seal(&key);
        let (_, tag) = cookie.split_once('.').unwrap();
        let (body, _) = forged.split_once('.').unwrap();
        forged = format!("{body}.{tag}");
        assert_eq!(
            open(&forged, &key, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        let mut bytes = cookie.clone().into_bytes();
        bytes[3] = if bytes[3] == b'A' { b'B' } else { b'A' };
        assert_eq!(
            open(&String::from_utf8(bytes).unwrap(), &key, &state.state, NOW),
            fail("the login state cookie signature is not valid")
        );
        for bad in ["", "nodot", "a.!!"] {
            assert_eq!(
                open(bad, &key, &state.state, NOW),
                fail("the login state cookie is malformed")
            );
        }
        // 같은 CA 키에서는 같은 쿠키 키(다른 노드도 확인할 수 있다)
        let again = state_key("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        assert!(LoginState::open(&cookie, &again, &state.state, NOW).is_ok());
        // 로그에 검증자·state를 내지 않는다.
        let shown = format!("{state:?}");
        assert!(!shown.contains(&state.verifier), "{shown}");
        assert!(!shown.contains(&state.state), "{shown}");
    }
}
