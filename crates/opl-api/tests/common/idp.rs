//! 시험용 가짜 OIDC 제공자(IdP). 시험 CA가 서명한 `localhost`·`127.0.0.1` 인증서의 HTTPS로
//! discovery(`/.well-known/openid-configuration`), `/authorize`(쿼리를 기록하고 곧바로
//! `redirect_uri?code=..&state=..`로 302), `/token`(코드·PKCE S256·클라이언트 인증을 확인하고
//! ID 토큰·액세스 토큰), `/jwks`, `/userinfo`를 낸다. 서명 키는 RS256(저장소 견본
//! `tests/fixtures/oidc-rsa.pk8`, kid `rsa-1`)과 ES256(시험마다 새로, kid `ec-1`)이고 처음엔 RS256으로
//! 서명한다. 시험이 discovery 문서, 클레임, 서명 키·kid·알고리즘, 공개할 키, userinfo, 실패 응답, 인가 응답의
//! `iss`(RFC 9207), 큰 JWKS를 정한다.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use opl_peer::pki::{ClusterCa, NodeTls, first_cert, node_key_and_csr};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair, RsaKeyPair};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 저장소의 RS256 시험 키(openssl로 한 번 만든 2048비트 PKCS#8 DER — ring은 RSA 키를 만들지 못한다)
const RSA_PK8: &[u8] = include_bytes!("../fixtures/oidc-rsa.pk8");

pub const CLIENT_ID: &str = "oplite";
pub const CLIENT_SECRET: &str = "client-secret";
/// 기본 ID 토큰의 `sub`
pub const SUBJECT: &str = "alice-sub";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alg {
    Rs256,
    Es256,
}

#[derive(Clone)]
enum Signer {
    Rsa(Arc<RsaKeyPair>),
    Ec(Arc<EcdsaKeyPair>),
}

#[derive(Clone)]
struct Key {
    kid: String,
    signer: Signer,
    /// JWKS에 내보이는지
    published: bool,
}

impl Key {
    fn new(kid: &str, alg: Alg) -> Key {
        let signer = match alg {
            Alg::Rs256 => Signer::Rsa(Arc::new(RsaKeyPair::from_pkcs8(RSA_PK8).unwrap())),
            Alg::Es256 => {
                let rng = SystemRandom::new();
                let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
                let pk8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
                Signer::Ec(Arc::new(
                    EcdsaKeyPair::from_pkcs8(alg, pk8.as_ref(), &rng).unwrap(),
                ))
            }
        };
        Key {
            kid: kid.into(),
            signer,
            published: true,
        }
    }

    fn alg(&self) -> &'static str {
        match self.signer {
            Signer::Rsa(_) => "RS256",
            Signer::Ec(_) => "ES256",
        }
    }

    fn jwk(&self) -> Value {
        match &self.signer {
            Signer::Rsa(k) => {
                let public: signature::RsaPublicKeyComponents<Vec<u8>> = k.public().into();
                json!({ "kty": "RSA", "kid": self.kid, "use": "sig", "alg": "RS256",
                        "n": b64(public.n), "e": b64(public.e) })
            }
            Signer::Ec(k) => {
                let point = k.public_key().as_ref();
                json!({ "kty": "EC", "kid": self.kid, "crv": "P-256",
                        "x": b64(&point[1..33]), "y": b64(&point[33..]) })
            }
        }
    }

    fn sign(&self, input: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        match &self.signer {
            Signer::Rsa(k) => {
                let mut sig = vec![0; k.public().modulus_len()];
                k.sign(&signature::RSA_PKCS1_SHA256, &rng, input, &mut sig)
                    .unwrap();
                sig
            }
            Signer::Ec(k) => k.sign(&rng, input).unwrap().as_ref().to_vec(),
        }
    }
}

fn b64(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// `/authorize`가 낸 코드
#[derive(Clone)]
struct Grant {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    nonce: Option<String>,
}

struct Inner {
    issuer: String,
    client_id: String,
    client_secret: Option<String>,
    discovery: Value,
    keys: Vec<Key>,
    /// 서명할 키의 kid
    signing: String,
    /// ID 토큰 헤더에 kid를 넣는지
    header_kid: bool,
    /// 기본 클레임에 덮어쓸 값(null이면 뺀다)
    claims: Map<String, Value>,
    userinfo: Value,
    /// `/authorize`가 코드 대신 돌려보낼 오류
    authorize_error: Option<String>,
    /// `/token`의 실패 응답
    token_failure: Option<(u16, Value)>,
    /// `/userinfo`의 실패 상태
    userinfo_failure: Option<u16>,
    /// `/authorize`가 돌려보낼 때 붙일 `iss`
    authorize_iss: Option<String>,
    /// JWKS 문서에 더할 바이트 수와 청크로 보낼지(본문 크기 제한 시험)
    jwks_padding: Option<(usize, bool)>,
    codes: HashMap<String, Grant>,
    access_tokens: Vec<String>,
    authorize_queries: Vec<HashMap<String, String>>,
    token_requests: Vec<(Option<String>, HashMap<String, String>)>,
    hits: HashMap<&'static str, usize>,
}

#[derive(Clone)]
struct Site(Arc<Mutex<Inner>>);

impl Site {
    fn hit(&self, path: &'static str) -> std::sync::MutexGuard<'_, Inner> {
        let mut inner = self.0.lock().unwrap();
        *inner.hits.entry(path).or_default() += 1;
        inner
    }
}

pub struct Idp {
    /// `https://127.0.0.1:<port>`
    pub issuer: String,
    /// 서버 인증서에 서명한 CA(PEM)
    pub ca_pem: String,
    /// 같은 CA(DER)
    pub ca_der: Vec<u8>,
    site: Site,
    server: JoinHandle<()>,
}

impl Idp {
    /// 기밀 클라이언트(`oplite`/`client-secret`)의 IdP
    pub async fn start() -> Idp {
        Idp::start_with(CLIENT_ID, Some(CLIENT_SECRET)).await
    }

    /// `client_secret`이 없으면 공개 클라이언트(PKCE만)
    pub async fn start_with(client_id: &str, client_secret: Option<&str>) -> Idp {
        let ca = ClusterCa::generate("idp").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let sans = ["localhost".to_string(), "127.0.0.1".to_string()];
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, uuid::Uuid::new_v4(), &sans).unwrap(),
            key_pem,
        };
        let config = opl_peer::tls::api_server_config(&tls).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("https://{}", tcp.local_addr().unwrap());
        let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
        let site = Site(Arc::new(Mutex::new(Inner {
            discovery: json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "jwks_uri": format!("{issuer}/jwks"),
                "userinfo_endpoint": format!("{issuer}/userinfo"),
                "response_types_supported": ["code"],
                "id_token_signing_alg_values_supported": ["RS256", "ES256"],
            }),
            issuer: issuer.clone(),
            client_id: client_id.into(),
            client_secret: client_secret.map(str::to_string),
            keys: vec![Key::new("rsa-1", Alg::Rs256), Key::new("ec-1", Alg::Es256)],
            signing: "rsa-1".into(),
            header_kid: true,
            claims: Map::new(),
            userinfo: json!({
                "sub": SUBJECT,
                "preferred_username": "Alice",
                "groups": ["opl-admins"],
            }),
            authorize_error: None,
            token_failure: None,
            userinfo_failure: None,
            authorize_iss: None,
            jwks_padding: None,
            codes: HashMap::new(),
            access_tokens: Vec::new(),
            authorize_queries: Vec::new(),
            token_requests: Vec::new(),
            hits: HashMap::new(),
        })));
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get(discovery),
            )
            .route("/authorize", axum::routing::get(authorize))
            .route("/token", axum::routing::post(token))
            .route("/jwks", axum::routing::get(jwks))
            .route("/userinfo", axum::routing::get(userinfo))
            .with_state(site.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Idp {
            issuer,
            ca_der: first_cert(&ca.cert_pem).unwrap().to_vec(),
            ca_pem: ca.cert_pem,
            site,
            server,
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.site.0.lock().unwrap()
    }

    /// 이 IdP를 쓰는 영역의 `backend`(시험 CA를 믿는다)
    pub fn backend(&self) -> Value {
        let inner = self.inner();
        let mut backend = json!({
            "kind": "oidc",
            "issuer": inner.issuer,
            "client_id": inner.client_id,
            "ca_pem": self.ca_pem,
            // macOS에서는 새 시험 CA로 하는 첫 TLS 연결의 인증서 검증이 15초쯤 걸리기도 한다.
            "timeout_secs": 30,
        });
        if let Some(secret) = &inner.client_secret {
            backend["client_secret"] = json!(secret);
        }
        backend
    }

    /// 경로(`discovery`, `authorize`, `token`, `jwks`, `userinfo`)가 불린 횟수
    pub fn hits(&self, path: &str) -> usize {
        self.inner().hits.get(path).copied().unwrap_or(0)
    }

    /// discovery 문서를 고친다.
    pub fn set_discovery(&self, f: impl FnOnce(&mut Value)) {
        f(&mut self.inner().discovery);
    }

    /// 서명 키를 더한다(`published`면 JWKS에도). 서명 키는 [`Idp::sign_with`]로 고른다.
    pub fn add_key(&self, kid: &str, alg: Alg, published: bool) {
        let mut key = Key::new(kid, alg);
        key.published = published;
        self.inner().keys.push(key);
    }

    /// JWKS에 키를 내보일지
    pub fn publish(&self, kid: &str, published: bool) {
        for key in self.inner().keys.iter_mut().filter(|k| k.kid == kid) {
            key.published = published;
        }
    }

    /// 앞으로 `kid`의 키로 서명한다.
    pub fn sign_with(&self, kid: &str) {
        self.inner().signing = kid.into();
    }

    /// ID 토큰 헤더에 kid를 넣을지(기본 넣음)
    pub fn set_header_kid(&self, on: bool) {
        self.inner().header_kid = on;
    }

    /// 앞으로 낼 ID 토큰의 클레임을 덮어쓴다(null이면 뺀다). 앞서 정한 것에 더한다.
    pub fn set_claims(&self, claims: Value) {
        let mut inner = self.inner();
        for (k, v) in claims.as_object().unwrap() {
            inner.claims.insert(k.clone(), v.clone());
        }
    }

    pub fn set_userinfo(&self, userinfo: Value) {
        self.inner().userinfo = userinfo;
    }

    /// `/authorize`가 코드 대신 `error`로 돌려보낸다(None이면 되돌림).
    pub fn set_authorize_error(&self, error: Option<&str>) {
        self.inner().authorize_error = error.map(str::to_string);
    }

    /// `/token`이 `status`와 `body`로 실패한다(None이면 되돌림).
    pub fn fail_token(&self, failure: Option<(u16, Value)>) {
        self.inner().token_failure = failure;
    }

    /// `/userinfo`가 `status`로 실패한다(None이면 되돌림).
    pub fn fail_userinfo(&self, status: Option<u16>) {
        self.inner().userinfo_failure = status;
    }

    /// `/authorize`가 돌려보내는 주소에 `iss`를 붙인다(None이면 붙이지 않음).
    pub fn set_authorize_iss(&self, iss: Option<&str>) {
        self.inner().authorize_iss = iss.map(str::to_string);
    }

    /// JWKS 문서에 `bytes`바이트를 더한다. `chunked`면 Content-Length 없이 청크로 보낸다.
    pub fn pad_jwks(&self, bytes: usize, chunked: bool) {
        self.inner().jwks_padding = Some((bytes, chunked));
    }

    /// `/authorize`가 받은 쿼리(받은 순서)
    pub fn authorize_queries(&self) -> Vec<HashMap<String, String>> {
        self.inner().authorize_queries.clone()
    }

    /// `/token`이 받은 (Authorization 헤더, 폼 본문)(받은 순서)
    pub fn token_requests(&self) -> Vec<(Option<String>, HashMap<String, String>)> {
        self.inner().token_requests.clone()
    }

    /// 기본 클레임(`iss`, `sub`, `aud`, `exp` 5분 뒤, `iat` 지금, `nonce`, `preferred_username` Alice,
    /// `groups` [opl-admins])에 [`Idp::set_claims`]를 덮어쓴 것
    pub fn claims(&self, nonce: Option<&str>) -> Value {
        default_claims(&self.inner(), nonce)
    }

    /// 지금 서명 키로 `claims`에 서명한 ID 토큰
    pub fn id_token(&self, claims: &Value) -> String {
        sign(&self.inner(), claims)
    }

    /// 브라우저처럼 `/authorize` 주소(opLite `start`의 Location)를 열어 돌려보내는 곳(콜백 주소)을 얻는다.
    pub async fn follow(&self, authorize_url: &str) -> String {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .tls_certs_only([reqwest::Certificate::from_pem(self.ca_pem.as_bytes()).unwrap()])
            .build()
            .unwrap();
        let resp = http.get(authorize_url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::FOUND, "{authorize_url}");
        resp.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string()
    }

    /// 헤더까지 정해 지금 서명 키로 서명한다(`alg` 거짓말 시험).
    pub fn sign_raw(&self, header: &Value, claims: &Value) -> String {
        let inner = self.inner();
        let key = inner.keys.iter().find(|k| k.kid == inner.signing).unwrap();
        signed(key, header, claims)
    }
}

impl Drop for Idp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn signed(key: &Key, header: &Value, claims: &Value) -> String {
    let input = format!(
        "{}.{}",
        b64(serde_json::to_vec(header).unwrap()),
        b64(serde_json::to_vec(claims).unwrap())
    );
    let sig = key.sign(input.as_bytes());
    format!("{input}.{}", b64(sig))
}

fn sign(inner: &Inner, claims: &Value) -> String {
    let key = inner.keys.iter().find(|k| k.kid == inner.signing).unwrap();
    let mut header = json!({ "alg": key.alg(), "typ": "JWT" });
    if inner.header_kid {
        header["kid"] = json!(key.kid);
    }
    signed(key, &header, claims)
}

async fn discovery(State(site): State<Site>) -> Response {
    axum::Json(site.hit("discovery").discovery.clone()).into_response()
}

async fn jwks(State(site): State<Site>) -> Response {
    let inner = site.hit("jwks");
    let keys: Vec<Value> = inner
        .keys
        .iter()
        .filter(|k| k.published)
        .map(Key::jwk)
        .collect();
    match inner.jwks_padding {
        None => axum::Json(json!({ "keys": keys })).into_response(),
        Some((bytes, chunked)) => {
            let body = json!({ "keys": keys, "padding": "x".repeat(bytes) }).to_string();
            if !chunked {
                return ([(header::CONTENT_TYPE, "application/json")], body).into_response();
            }
            let (a, b) = body.split_at(body.len() / 2);
            let chunks = [a.to_string(), b.to_string()].map(Ok::<_, std::io::Error>);
            let stream = futures_util::stream::iter(chunks);
            (
                [(header::CONTENT_TYPE, "application/json")],
                axum::body::Body::from_stream(stream),
            )
                .into_response()
        }
    }
}

async fn authorize(
    State(site): State<Site>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut inner = site.hit("authorize");
    inner.authorize_queries.push(query.clone());
    let redirect = query.get("redirect_uri").cloned().unwrap_or_default();
    let Ok(mut location) = reqwest::Url::parse(&redirect) else {
        return (StatusCode::BAD_REQUEST, "bad redirect_uri").into_response();
    };
    {
        let mut pairs = location.query_pairs_mut();
        if let Some(error) = &inner.authorize_error {
            pairs.append_pair("error", error);
            pairs.append_pair("error_description", "the user said no");
        } else {
            let code = b64(uuid::Uuid::new_v4().as_bytes());
            inner.codes.insert(
                code.clone(),
                Grant {
                    client_id: query.get("client_id").cloned().unwrap_or_default(),
                    redirect_uri: redirect.clone(),
                    challenge: query.get("code_challenge").cloned().unwrap_or_default(),
                    nonce: query.get("nonce").cloned(),
                },
            );
            pairs.append_pair("code", &code);
        }
        if let Some(state) = query.get("state") {
            pairs.append_pair("state", state);
        }
        if let Some(iss) = &inner.authorize_iss {
            pairs.append_pair("iss", iss);
        }
    }
    (
        StatusCode::FOUND,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}

/// application/x-www-form-urlencoded 값 하나를 푼다.
fn form_decode(value: &str) -> Option<String> {
    let uri: axum::http::Uri = format!("/?v={value}").parse().ok()?;
    let Query(mut query) = Query::<HashMap<String, String>>::try_from_uri(&uri).ok()?;
    query.remove("v")
}

fn token_error(status: StatusCode, error: &str) -> Response {
    (status, axum::Json(json!({ "error": error }))).into_response()
}

async fn token(
    State(site): State<Site>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> Response {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut inner = site.hit("token");
    inner
        .token_requests
        .push((authorization.clone(), form.clone()));
    if let Some((status, body)) = inner.token_failure.clone() {
        return (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response();
    }
    // 클라이언트 인증: 기밀 클라이언트는 client_secret_basic(RFC 6749 §2.3.1 — client_id와 비밀을 각각
    // form-urlencode한 뒤 `id:secret`), 공개 클라이언트는 폼의 client_id
    let client_ok = match &inner.client_secret {
        Some(secret) => authorization
            .as_deref()
            .and_then(|a| a.strip_prefix("Basic "))
            .and_then(|b| STANDARD.decode(b).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|pair| {
                let (id, s) = pair.split_once(':')?;
                Some((form_decode(id)?, form_decode(s)?))
            })
            .is_some_and(|(id, s)| id == inner.client_id && s == *secret),
        None => authorization.is_none() && form.get("client_id") == Some(&inner.client_id),
    };
    if !client_ok {
        return token_error(StatusCode::UNAUTHORIZED, "invalid_client");
    }
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return token_error(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }
    // 코드는 한 번만 쓴다.
    let Some(grant) = form.get("code").and_then(|c| inner.codes.remove(c)) else {
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    let challenge = b64(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ));
    if grant.client_id != inner.client_id
        || form.get("redirect_uri") != Some(&grant.redirect_uri)
        || challenge != grant.challenge
    {
        return token_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    let access_token = b64(uuid::Uuid::new_v4().as_bytes());
    inner.access_tokens.push(access_token.clone());
    let id_token = sign(&inner, &default_claims(&inner, grant.nonce.as_deref()));
    axum::Json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token,
    }))
    .into_response()
}

fn default_claims(inner: &Inner, nonce: Option<&str>) -> Value {
    let mut claims = json!({
        "iss": inner.issuer,
        "sub": SUBJECT,
        "aud": inner.client_id,
        "exp": now() + 300,
        "iat": now(),
        "preferred_username": "Alice",
        "groups": ["opl-admins"],
    });
    if let Some(nonce) = nonce {
        claims["nonce"] = json!(nonce);
    }
    let map = claims.as_object_mut().unwrap();
    for (k, v) in &inner.claims {
        if v.is_null() {
            map.remove(k);
        } else {
            map.insert(k.clone(), v.clone());
        }
    }
    claims
}

async fn userinfo(State(site): State<Site>, headers: HeaderMap) -> Response {
    let inner = site.hit("userinfo");
    if let Some(status) = inner.userinfo_failure {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|a| a.strip_prefix("Bearer "));
    if !token.is_some_and(|t| inner.access_tokens.iter().any(|a| a == t)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(inner.userinfo.clone()).into_response()
}
