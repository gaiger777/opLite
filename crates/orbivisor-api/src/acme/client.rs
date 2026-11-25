//! 손으로 짠 ACME 클라이언트(RFC 8555, ARI RFC 9773, 프로필): 디렉터리, nonce 통(`badNonce`면 새 nonce로 3번까지), JWS
//! POST·POST-as-GET, 계정 등록(EAB), 주문·인가·도전·물음(`Retry-After`), finalize, 사슬 받기(`Link: rel="alternate"`의
//! 다른 사슬 중 고른 루트), 갱신 창. HTTP는 reqwest(native-tls)이고 `ca_bundle`이 있으면 그 루트만 믿는다. 응답 본문은
//! 1 MiB까지만 읽는다.

use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::jws::AccountKey;

/// 응답 본문 한도
const MAX_BODY: usize = 1 << 20;
/// `badNonce`를 받고 다시 보내는 수
const NONCE_RETRIES: usize = 3;
/// 요청 하나의 시간 한도
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// ACME 실패. 메시지는 사용자에게 그대로 보인다(비밀은 들지 않는다).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcmeError {
    /// CA의 속도 한도. `retry_after`(초)가 지나야 다시 한다.
    #[error("the CA rate-limited this request{}: {detail}", retry_after.map(|s| format!(" (retry after {s}s)")).unwrap_or_default())]
    RateLimited {
        retry_after: Option<u64>,
        detail: String,
    },
    #[error("the CA kept rejecting the request nonce: {0}")]
    BadNonce(String),
    #[error("the CA refused the authorization: {0}")]
    Unauthorized(String),
    #[error("the CA could not resolve the name: {0}")]
    Dns(String),
    #[error("the CA could not connect: {0}")]
    Connection(String),
    #[error("the CA got a wrong challenge answer: {0}")]
    Incorrect(String),
    #[error("a CAA record forbids this CA: {0}")]
    Caa(String),
    #[error("the CA does not issue for this name: {0}")]
    RejectedIdentifier(String),
    #[error("the CA requires external account binding (EAB): {0}")]
    ExternalAccountRequired(String),
    /// CA 쪽 오류(5xx·serverInternal·모르는 문제 종류)
    #[error("the CA failed: {0}")]
    Server(String),
    /// 닿지 않음·TLS·본문 읽기
    #[error("cannot reach the ACME server: {0}")]
    Http(String),
    /// 응답 꼴이 틀림·우리 쪽 준비 실패(키·CSR·DNS 공급자)
    #[error("{0}")]
    Invalid(String),
}

impl AcmeError {
    /// ACME 문제 문서(RFC 7807, `type` urn:ietf:params:acme:error:…)
    pub fn from_problem(problem: &Value, status: u16, retry_after: Option<u64>) -> Self {
        let kind = problem["type"].as_str().unwrap_or_default();
        let mut detail = problem["detail"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}"));
        // 하위 문제(이름마다)를 붙인다.
        if let Some(subs) = problem["subproblems"].as_array() {
            for s in subs.iter().take(5) {
                if let Some(d) = s["detail"].as_str() {
                    detail.push_str(&format!("; {}: {d}", s["identifier"]["value"]));
                }
            }
        }
        let short = kind.strip_prefix("urn:ietf:params:acme:error:");
        match short {
            Some("rateLimited") => Self::RateLimited {
                retry_after,
                detail,
            },
            Some("badNonce") => Self::BadNonce(detail),
            Some("unauthorized") => Self::Unauthorized(detail),
            Some("dns") => Self::Dns(detail),
            Some("connection") | Some("tls") => Self::Connection(detail),
            Some("incorrectResponse") => Self::Incorrect(detail),
            Some("caa") => Self::Caa(detail),
            Some("rejectedIdentifier") | Some("unsupportedIdentifier") => {
                Self::RejectedIdentifier(detail)
            }
            Some("externalAccountRequired") => Self::ExternalAccountRequired(detail),
            Some("malformed")
            | Some("badCSR")
            | Some("badPublicKey")
            | Some("badSignatureAlgorithm")
            | Some("invalidContact")
            | Some("unsupportedContact")
            | Some("accountDoesNotExist")
            | Some("userActionRequired")
            | Some("badRevocationReason")
            | Some("orderNotReady") => {
                Self::Invalid(format!("{}: {detail}", short.unwrap_or(kind)))
            }
            _ => Self::Server(detail),
        }
    }

    /// 다시 해도 같은 실패(설정을 고쳐야 함) — 물러서기는 같지만 문구가 다르다.
    pub fn retry_after(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    pub new_nonce: String,
    pub new_account: String,
    pub new_order: String,
    #[serde(default)]
    pub renewal_info: Option<String>,
    #[serde(default)]
    pub meta: DirectoryMeta,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryMeta {
    #[serde(default)]
    pub terms_of_service: Option<String>,
    #[serde(default)]
    pub external_account_required: bool,
    /// 프로필 이름 → 설명
    #[serde(default)]
    pub profiles: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Order {
    pub status: String,
    #[serde(default)]
    pub authorizations: Vec<String>,
    pub finalize: String,
    #[serde(default)]
    pub certificate: Option<String>,
    #[serde(default)]
    pub error: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Identifier {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Authorization {
    pub status: String,
    pub identifier: Identifier,
    #[serde(default)]
    pub challenges: Vec<Challenge>,
    #[serde(default)]
    pub wildcard: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Challenge {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error: Option<Value>,
}

/// 갱신 창(ARI) — 유닉스 초
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenewalInfo {
    pub start: i64,
    pub end: i64,
    /// 다시 물을 때까지(초)
    pub retry_after: Option<u64>,
}

/// 응답 하나
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub location: Option<String>,
    pub retry_after: Option<u64>,
    /// `Link` 머리(rel, URL)
    pub links: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, AcmeError> {
        serde_json::from_slice(&self.body)
            .map_err(|e| AcmeError::Invalid(format!("unexpected ACME response: {e}")))
    }
}

/// `ca_bundle`(PEM)이 있으면 그 루트만 믿는 HTTP 클라이언트
pub fn http_client(ca_bundle: Option<&str>) -> Result<reqwest::Client, AcmeError> {
    let mut builder = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("orbivisor-acme/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(3));
    if let Some(pem) = ca_bundle {
        let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
            .map_err(|_| AcmeError::Invalid("ca_bundle holds no usable certificate".into()))?;
        builder = builder.tls_certs_only(certs);
    }
    builder
        .build()
        .map_err(|e| AcmeError::Http(crate::telemetry::chain(&e)))
}

/// 머리 `Retry-After`(초 또는 HTTP 날짜)
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let v = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(s) = v.trim().parse::<u64>() {
        return Some(s);
    }
    let at = time::OffsetDateTime::parse(v.trim(), &time::format_description::well_known::Rfc2822)
        .ok()?
        .unix_timestamp();
    Some(at.saturating_sub(crate::backups::now() as i64).max(0) as u64)
}

/// `Link: <url>;rel="alternate", <url2>;rel="up"`
fn links(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for v in headers.get_all(reqwest::header::LINK) {
        let Ok(v) = v.to_str() else { continue };
        for part in v.split(',') {
            let Some((url, params)) = part.trim().split_once(';') else {
                continue;
            };
            let url = url.trim().trim_start_matches('<').trim_end_matches('>');
            for p in params.split(';') {
                if let Some(rel) = p.trim().strip_prefix("rel=") {
                    out.push((rel.trim_matches('"').to_string(), url.to_string()));
                }
            }
        }
    }
    out
}

async fn read(response: reqwest::Response) -> Result<Reply, AcmeError> {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| AcmeError::Http(crate::telemetry::chain(&e.without_url())))?
    {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(AcmeError::Invalid(
                "the ACME response is larger than 1 MiB".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Reply {
        status,
        location: headers
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        retry_after: retry_after(&headers),
        links: links(&headers),
        body,
    })
}

fn transport(e: reqwest::Error) -> AcmeError {
    AcmeError::Http(crate::telemetry::chain(&e.without_url()))
}

/// 오류 응답이면 문제 문서로
fn check(reply: Reply) -> Result<Reply, AcmeError> {
    if reply.status < 400 {
        return Ok(reply);
    }
    let problem: Value = serde_json::from_slice(&reply.body).unwrap_or(Value::Null);
    Err(AcmeError::from_problem(
        &problem,
        reply.status,
        reply.retry_after,
    ))
}

/// 디렉터리 하나와 계정 키 하나의 연결
pub struct AcmeClient {
    http: reqwest::Client,
    pub directory: Directory,
    key: AccountKey,
    kid: Option<String>,
    nonces: Mutex<Vec<String>>,
}

impl std::fmt::Debug for AcmeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeClient")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl AcmeClient {
    /// 디렉터리를 읽는다.
    pub async fn connect(
        directory_url: &str,
        ca_bundle: Option<&str>,
        key: AccountKey,
    ) -> Result<Self, AcmeError> {
        let http = http_client(ca_bundle)?;
        let reply = check(read(http.get(directory_url).send().await.map_err(transport)?).await?)?;
        let directory: Directory = reply.json()?;
        Ok(Self {
            http,
            directory,
            key,
            kid: None,
            nonces: Mutex::new(Vec::new()),
        })
    }

    pub fn key(&self) -> &AccountKey {
        &self.key
    }

    /// 등록한 계정 URL(그 뒤 JWS는 `kid`)
    pub fn set_account(&mut self, url: String) {
        self.kid = Some(url);
    }

    /// 디렉터리의 프로필 이름
    pub fn profiles(&self) -> Vec<String> {
        self.directory.meta.profiles.keys().cloned().collect()
    }

    fn keep_nonce(&self, reply_nonce: Option<String>) {
        if let Some(n) = reply_nonce {
            let mut pool = self.nonces.lock().unwrap_or_else(|e| e.into_inner());
            pool.push(n);
            // 오래 쓰지 않은 것은 버린다.
            if pool.len() > 16 {
                pool.remove(0);
            }
        }
    }

    async fn nonce(&self) -> Result<String, AcmeError> {
        if let Some(n) = self.nonces.lock().unwrap_or_else(|e| e.into_inner()).pop() {
            return Ok(n);
        }
        let response = self
            .http
            .head(&self.directory.new_nonce)
            .send()
            .await
            .map_err(transport)?;
        response
            .headers()
            .get("replay-nonce")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| AcmeError::Invalid("the ACME server gave no nonce".into()))
    }

    /// JWS POST(`payload`가 `None`이면 POST-as-GET). `badNonce`면 새 nonce로 다시 보낸다.
    pub async fn post(&self, url: &str, payload: Option<&Value>) -> Result<Reply, AcmeError> {
        let mut tries = 0;
        loop {
            let nonce = self.nonce().await?;
            let body = self.key.sign(url, &nonce, self.kid.as_deref(), payload);
            let response = self
                .http
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/jose+json")
                .body(body.to_string())
                .send()
                .await
                .map_err(transport)?;
            self.keep_nonce(
                response
                    .headers()
                    .get("replay-nonce")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            );
            match check(read(response).await?) {
                Err(AcmeError::BadNonce(_)) if tries < NONCE_RETRIES => tries += 1,
                other => return other,
            }
        }
    }

    /// 계정을 등록한다(이미 있으면 같은 URL — 다시 해도 같다). 계정 URL을 돌려주고 그 뒤 JWS에 쓴다.
    pub async fn register(
        &mut self,
        email: Option<&str>,
        eab: Option<(&str, &str)>,
    ) -> Result<String, AcmeError> {
        let mut payload = json!({ "termsOfServiceAgreed": true });
        if let Some(e) = email {
            payload["contact"] = json!([format!("mailto:{e}")]);
        }
        if let Some((kid, hmac)) = eab {
            payload["externalAccountBinding"] = self
                .key
                .eab(kid, hmac, &self.directory.new_account)
                .map_err(|e| AcmeError::Invalid(e.to_string()))?;
        }
        self.kid = None;
        let url = self.directory.new_account.clone();
        let reply = self.post(&url, Some(&payload)).await?;
        let account = reply
            .location
            .ok_or_else(|| AcmeError::Invalid("the CA gave no account URL".into()))?;
        self.kid = Some(account.clone());
        Ok(account)
    }

    /// 새 주문. `profile`은 디렉터리에 프로필이 있을 때만 보낸다. `replaces`는 ARI 인증서 id(갱신).
    pub async fn new_order(
        &self,
        names: &[String],
        profile: Option<&str>,
        replaces: Option<&str>,
    ) -> Result<(String, Order), AcmeError> {
        let identifiers: Vec<Value> = names
            .iter()
            .map(|n| {
                let kind = if n.parse::<std::net::IpAddr>().is_ok() {
                    "ip"
                } else {
                    "dns"
                };
                json!({ "type": kind, "value": n })
            })
            .collect();
        let mut payload = json!({ "identifiers": identifiers });
        if let Some(p) = profile
            && self.directory.meta.profiles.contains_key(p)
        {
            payload["profile"] = json!(p);
        }
        if let Some(r) = replaces
            && self.directory.renewal_info.is_some()
        {
            payload["replaces"] = json!(r);
        }
        let reply = self
            .post(&self.directory.new_order.clone(), Some(&payload))
            .await?;
        let url = reply
            .location
            .clone()
            .ok_or_else(|| AcmeError::Invalid("the CA gave no order URL".into()))?;
        Ok((url, reply.json()?))
    }

    pub async fn order(&self, url: &str) -> Result<(Order, Option<u64>), AcmeError> {
        let reply = self.post(url, None).await?;
        Ok((reply.json()?, reply.retry_after))
    }

    pub async fn authorization(
        &self,
        url: &str,
    ) -> Result<(Authorization, Option<u64>), AcmeError> {
        let reply = self.post(url, None).await?;
        Ok((reply.json()?, reply.retry_after))
    }

    /// 도전에 답했다고 알린다(빈 객체 POST).
    pub async fn respond(&self, challenge_url: &str) -> Result<(), AcmeError> {
        self.post(challenge_url, Some(&json!({}))).await.map(drop)
    }

    /// 인가가 `valid`가 될 때까지 묻는다(`Retry-After` 존중, `limit` 넘으면 실패). `invalid`면 도전의 오류.
    pub async fn wait_authorization(&self, url: &str, limit: Duration) -> Result<(), AcmeError> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let (authz, retry) = self.authorization(url).await?;
            match authz.status.as_str() {
                "valid" => return Ok(()),
                "pending" | "processing" => {}
                _ => {
                    let err = authz
                        .challenges
                        .iter()
                        .find_map(|c| c.error.as_ref())
                        .map(|p| AcmeError::from_problem(p, 400, None))
                        .unwrap_or_else(|| {
                            AcmeError::Unauthorized(format!(
                                "authorization for {} is {}",
                                authz.identifier.value, authz.status
                            ))
                        });
                    return Err(err);
                }
            }
            sleep_until_retry(deadline, retry, "authorization").await?;
        }
    }

    /// CSR(DER)를 보내고 주문이 `valid`가 될 때까지 묻는다. 인증서 URL을 돌려준다.
    pub async fn finalize(
        &self,
        order_url: &str,
        finalize_url: &str,
        csr_der: &[u8],
        limit: Duration,
    ) -> Result<String, AcmeError> {
        let reply = self
            .post(
                finalize_url,
                Some(&json!({ "csr": super::jws::b64(csr_der) })),
            )
            .await?;
        let mut order: Order = reply.json()?;
        let mut retry = reply.retry_after;
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            match order.status.as_str() {
                "valid" => {
                    return order.certificate.ok_or_else(|| {
                        AcmeError::Invalid("the valid order has no certificate URL".into())
                    });
                }
                "processing" | "ready" | "pending" => {}
                _ => {
                    return Err(order
                        .error
                        .as_ref()
                        .map(|p| AcmeError::from_problem(p, 400, None))
                        .unwrap_or_else(|| {
                            AcmeError::Server(format!("the order became {}", order.status))
                        }));
                }
            }
            sleep_until_retry(deadline, retry, "order").await?;
            (order, retry) = self.order(order_url).await?;
        }
    }

    /// 사슬(PEM)을 받는다. `preferred_chain`(루트 CN)이 있으면 `Link: rel="alternate"` 사슬 가운데 맨 위 인증서의 발급자
    /// CN이 같은 것을 고른다(없으면 기본 사슬).
    pub async fn certificate(
        &self,
        url: &str,
        preferred_chain: Option<&str>,
    ) -> Result<String, AcmeError> {
        let reply = self.post(url, None).await?;
        let first = String::from_utf8(reply.body.clone())
            .map_err(|_| AcmeError::Invalid("the certificate chain is not PEM".into()))?;
        let Some(want) = preferred_chain else {
            return Ok(first);
        };
        if super::ari::chain_root_issuer(&first).as_deref() == Some(want) {
            return Ok(first);
        }
        for (rel, alt) in &reply.links {
            if rel != "alternate" {
                continue;
            }
            let reply = self.post(alt, None).await?;
            if let Ok(pem) = String::from_utf8(reply.body)
                && super::ari::chain_root_issuer(&pem).as_deref() == Some(want)
            {
                return Ok(pem);
            }
        }
        Ok(first)
    }

    /// 갱신 창(RFC 9773 — 인증 없는 GET). 디렉터리에 `renewalInfo`가 없으면 `None`.
    pub async fn renewal_info(&self, cert_id: &str) -> Result<Option<RenewalInfo>, AcmeError> {
        let Some(base) = &self.directory.renewal_info else {
            return Ok(None);
        };
        let url = format!("{}/{cert_id}", base.trim_end_matches('/'));
        let reply = check(read(self.http.get(&url).send().await.map_err(transport)?).await?)?;
        let v: Value = reply.json()?;
        let at = |k: &str| {
            v["suggestedWindow"][k]
                .as_str()
                .and_then(|s| {
                    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .map(|t| t.unix_timestamp())
        };
        match (at("start"), at("end")) {
            (Some(start), Some(end)) if start <= end => Ok(Some(RenewalInfo {
                start,
                end,
                retry_after: reply.retry_after,
            })),
            _ => Err(AcmeError::Invalid(
                "the renewal information has no valid suggestedWindow".into(),
            )),
        }
    }
}

/// `Retry-After`(기본 2초, 30초 한도)만큼 쉰다. 마감이 지나면 실패.
async fn sleep_until_retry(
    deadline: tokio::time::Instant,
    retry: Option<u64>,
    what: &str,
) -> Result<(), AcmeError> {
    let wait = Duration::from_secs(retry.unwrap_or(2).clamp(1, 30));
    if tokio::time::Instant::now() + wait > deadline {
        return Err(AcmeError::Server(format!(
            "the {what} did not finish in time"
        )));
    }
    tokio::time::sleep(wait).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_documents_map_to_errors() {
        let p = |t: &str| json!({"type": format!("urn:ietf:params:acme:error:{t}"), "detail": "d"});
        assert_eq!(
            AcmeError::from_problem(&p("rateLimited"), 429, Some(60)),
            AcmeError::RateLimited {
                retry_after: Some(60),
                detail: "d".into()
            }
        );
        assert_eq!(
            AcmeError::from_problem(&p("badNonce"), 400, None),
            AcmeError::BadNonce("d".into())
        );
        assert_eq!(
            AcmeError::from_problem(&p("connection"), 400, None),
            AcmeError::Connection("d".into())
        );
        assert_eq!(
            AcmeError::from_problem(&p("incorrectResponse"), 400, None),
            AcmeError::Incorrect("d".into())
        );
        assert_eq!(
            AcmeError::from_problem(&p("caa"), 403, None),
            AcmeError::Caa("d".into())
        );
        assert_eq!(
            AcmeError::from_problem(&p("externalAccountRequired"), 403, None),
            AcmeError::ExternalAccountRequired("d".into())
        );
        assert_eq!(
            AcmeError::from_problem(&p("malformed"), 400, None),
            AcmeError::Invalid("malformed: d".into())
        );
        assert_eq!(
            AcmeError::from_problem(&json!({}), 503, None),
            AcmeError::Server("HTTP 503".into())
        );
        let sub = json!({"type": "urn:ietf:params:acme:error:rejectedIdentifier", "detail": "bad",
            "subproblems": [{"identifier": {"type": "dns", "value": "x.test"}, "detail": "nope"}]});
        assert_eq!(
            AcmeError::from_problem(&sub, 400, None).to_string(),
            "the CA does not issue for this name: bad; \"x.test\": nope"
        );
    }

    #[test]
    fn link_headers_are_parsed() {
        let mut h = reqwest::header::HeaderMap::new();
        h.append(
            reqwest::header::LINK,
            r#"<https://ca/cert/1/1>;rel="alternate", <https://ca/dir>;rel="index""#
                .parse()
                .unwrap(),
        );
        h.append(
            reqwest::header::LINK,
            "<https://ca/cert/1/2>; rel=alternate".parse().unwrap(),
        );
        assert_eq!(
            links(&h),
            vec![
                ("alternate".to_string(), "https://ca/cert/1/1".to_string()),
                ("index".to_string(), "https://ca/dir".to_string()),
                ("alternate".to_string(), "https://ca/cert/1/2".to_string()),
            ]
        );
        h.insert(reqwest::header::RETRY_AFTER, "120".parse().unwrap());
        assert_eq!(retry_after(&h), Some(120));
    }

    #[test]
    fn directory_parses_meta() {
        let d: Directory = serde_json::from_value(json!({
            "newNonce": "n", "newAccount": "a", "newOrder": "o", "renewalInfo": "r",
            "meta": {"termsOfService": "t", "externalAccountRequired": true,
                     "profiles": {"classic": "x", "shortlived": "y"}}
        }))
        .unwrap();
        assert!(d.meta.external_account_required);
        assert_eq!(
            d.meta.profiles.keys().cloned().collect::<Vec<_>>(),
            ["classic", "shortlived"]
        );
    }
}
