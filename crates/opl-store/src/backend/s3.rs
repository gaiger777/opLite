//! S3 호환 객체 저장소 백엔드(설계 §7.7). hyper 1(HTTP/1.1, 연결을 `max_streams`개까지 다시 씀) + tokio-rustls(시스템
//! 루트 또는 사용자 CA) + SigV4. 쓰는 연산은 여섯: `PutObject`·`GetObject`·`HeadObject`·`DeleteObject`·
//! `ListObjectsV2`·`CopyObject`. 5xx·`SlowDown`·429·연결 오류·시간 넘김은 지수 대기(0.2초부터 30초까지)로 60초 동안
//! 다시 하고, 4xx는 곧바로 실패한다.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::client::conn::http1::SendRequest;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use opl_types::backup_store::S3Target;
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio_rustls::TlsConnector;

use super::sigv4::{self, EMPTY_SHA256, SignInput};
use super::{Backend, ListPage, ObjectInfo, ObjectMeta, StoreError, check_key, check_prefix};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 요청 하나(보내기·응답 받기)의 시간 제한 — 4 MiB 청크를 느린 회선으로도
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// 다시 하기를 이어 가는 시간(설계 §7.7)
pub const RETRY_BUDGET: Duration = Duration::from_secs(60);
const BACKOFF_START: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 받는 본문의 한도(인덱스가 가장 크다 — 64 TiB 디스크 = 512 MiB)
const MAX_BODY: usize = 1 << 30;
const LIST_KEYS: usize = 1000;

type Body = Full<Bytes>;

/// 끝점의 모양
#[derive(Clone, Debug, PartialEq, Eq)]
struct Endpoint {
    https: bool,
    /// 연결하는 호스트(가상 호스트 방식이면 `버킷.호스트`)
    host: String,
    port: u16,
    /// `Host` 헤더(기본 포트가 아니면 `:포트`)
    authority: String,
    /// 경로 앞부분(경로 방식이면 `/버킷`, 아니면 빈 문자열)
    base_path: String,
}

fn parse_endpoint(t: &S3Target) -> Result<Endpoint, StoreError> {
    let bad = || StoreError::Invalid(format!("invalid S3 endpoint {:?}", t.endpoint));
    let url = url::Url::parse(&t.endpoint).map_err(|_| bad())?;
    let https = match url.scheme() {
        "https" => true,
        "http" if t.insecure_http => false,
        _ => return Err(bad()),
    };
    let host = url.host_str().ok_or_else(bad)?.to_ascii_lowercase();
    let port = url.port_or_known_default().ok_or_else(bad)?;
    let default = if https { 443 } else { 80 };
    let (host, base_path) = if t.path_style {
        (host, format!("/{}", t.bucket))
    } else {
        (format!("{}.{host}", t.bucket), String::new())
    };
    let authority = if port == default {
        host.clone()
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    Ok(Endpoint {
        https,
        host: host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
        port,
        authority,
        base_path,
    })
}

/// 저장소 접두사(끝에 `/`, 없으면 빈 문자열)
fn store_prefix(t: &S3Target) -> String {
    match t.prefix.as_deref().map(|p| p.trim_end_matches('/')) {
        Some(p) if !p.is_empty() => format!("{p}/"),
        _ => String::new(),
    }
}

fn tls_config(ca_pem: Option<&str>) -> Result<Arc<ClientConfig>, StoreError> {
    let mut roots = RootCertStore::empty();
    match ca_pem {
        Some(pem) => {
            let certs = CertificateDer::pem_slice_iter(pem.as_bytes())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| StoreError::Invalid(format!("invalid CA certificate: {e}")))?;
            if certs.is_empty() {
                return Err(StoreError::Invalid(
                    "the CA bundle has no certificate".into(),
                ));
            }
            for c in certs {
                roots
                    .add(c)
                    .map_err(|e| StoreError::Invalid(format!("invalid CA certificate: {e}")))?;
            }
        }
        None => {
            let native = rustls_native_certs::load_native_certs();
            for c in native.certs {
                let _ = roots.add(c);
            }
            if roots.is_empty() {
                return Err(StoreError::Invalid(
                    "no system root certificates; give the store a CA certificate".into(),
                ));
            }
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| StoreError::Invalid(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// 요청 하나의 결과
struct Reply {
    status: StatusCode,
    headers: hyper::HeaderMap,
    body: Bytes,
}

/// 다시 할 만한 실패인가
enum Attempt {
    Retry(String),
    Fail(StoreError),
}

pub struct S3Backend {
    endpoint: Endpoint,
    region: String,
    bucket: String,
    prefix: String,
    access_key_id: String,
    secret_access_key: String,
    tls: Option<TlsConnector>,
    idle: Mutex<Vec<SendRequest<Body>>>,
    streams: Semaphore,
    max_streams: usize,
    retry_budget: Duration,
}

impl std::fmt::Debug for S3Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Backend")
            .field("endpoint", &self.endpoint.authority)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl S3Backend {
    pub fn new(target: &S3Target, max_streams: u8) -> Result<S3Backend, StoreError> {
        let endpoint = parse_endpoint(target)?;
        let tls = if endpoint.https {
            Some(TlsConnector::from(tls_config(target.ca_pem.as_deref())?))
        } else {
            None
        };
        let n = max_streams.clamp(1, opl_types::backup_store::MAX_STREAMS) as usize;
        Ok(S3Backend {
            endpoint,
            region: target.region.clone(),
            bucket: target.bucket.clone(),
            prefix: store_prefix(target),
            access_key_id: target.access_key_id.clone(),
            secret_access_key: target.secret_access_key.clone(),
            tls,
            idle: Mutex::new(Vec::new()),
            streams: Semaphore::new(n),
            max_streams: n,
            retry_budget: RETRY_BUDGET,
        })
    }

    /// 다시 하기 시간(시험은 짧게)
    pub fn with_retry_budget(mut self, d: Duration) -> S3Backend {
        self.retry_budget = d;
        self
    }

    fn object_path(&self, key: &str) -> String {
        format!("{}/{}{}", self.endpoint.base_path, self.prefix, key)
    }

    async fn connect(&self) -> Result<SendRequest<Body>, Attempt> {
        let addr = (self.endpoint.host.as_str(), self.endpoint.port);
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| Attempt::Retry("timed out connecting".into()))?
            .map_err(|e| Attempt::Retry(format!("cannot connect: {e}")))?;
        let _ = tcp.set_nodelay(true);
        let io: Box<dyn IoBox> = match &self.tls {
            Some(tls) => {
                let name = ServerName::try_from(self.endpoint.host.clone()).map_err(|e| {
                    Attempt::Fail(StoreError::Invalid(format!("invalid host name: {e}")))
                })?;
                let s = tokio::time::timeout(CONNECT_TIMEOUT, tls.connect(name, tcp))
                    .await
                    .map_err(|_| Attempt::Retry("timed out in the TLS handshake".into()))?
                    .map_err(|e| {
                        let msg = format!("TLS handshake failed: {e}");
                        // 인증서가 틀린 것은 다시 해도 같다
                        let cert = e
                            .get_ref()
                            .and_then(|i| i.downcast_ref::<rustls::Error>())
                            .is_some_and(|r| matches!(r, rustls::Error::InvalidCertificate(_)));
                        if cert {
                            Attempt::Fail(StoreError::Unavailable(msg))
                        } else {
                            Attempt::Retry(msg)
                        }
                    })?;
                Box::new(s)
            }
            None => Box::new(tcp),
        };
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .map_err(|e| Attempt::Retry(format!("HTTP handshake failed: {e}")))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(sender)
    }

    async fn sender(&self) -> Result<SendRequest<Body>, Attempt> {
        loop {
            let s = self.idle.lock().unwrap().pop();
            match s {
                Some(mut s) => {
                    if s.is_closed() {
                        continue;
                    }
                    if s.ready().await.is_ok() {
                        return Ok(s);
                    }
                }
                None => return self.connect().await,
            }
        }
    }

    /// 요청을 한 번 보낸다.
    async fn attempt(
        &self,
        method: &Method,
        path: &str,
        query: &[(String, String)],
        extra: &[(String, String)],
        body: &Bytes,
    ) -> Result<Reply, Attempt> {
        let now = super::unix_now();
        let amz_date = sigv4::amz_date(now);
        let payload_hash = if body.is_empty() {
            EMPTY_SHA256.to_string()
        } else {
            sigv4::sha256_hex(body)
        };
        let canonical_uri = sigv4::uri_encode(path, false);
        let mut headers = vec![
            ("host".to_string(), self.endpoint.authority.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        headers.extend(
            extra
                .iter()
                .filter(|(k, _)| k.starts_with("x-amz-"))
                .cloned(),
        );
        let auth = sigv4::authorization(
            &SignInput {
                method: method.as_str(),
                canonical_uri: &canonical_uri,
                query,
                headers: &headers,
                payload_hash: &payload_hash,
                amz_date: &amz_date,
                region: &self.region,
                service: "s3",
            },
            &self.access_key_id,
            &self.secret_access_key,
        );
        let q = sigv4::canonical_query(query);
        let uri = if q.is_empty() {
            canonical_uri
        } else {
            format!("{canonical_uri}?{q}")
        };
        let mut req = Request::builder().method(method.clone()).uri(uri);
        for (k, v) in headers
            .iter()
            .chain(extra.iter().filter(|(k, _)| !k.starts_with("x-amz-")))
        {
            req = req.header(k.as_str(), v.as_str());
        }
        if method == Method::PUT {
            req = req.header("content-length", body.len());
        }
        let req = req
            .header("authorization", auth)
            .body(Full::new(body.clone()))
            .map_err(|e| Attempt::Fail(StoreError::Invalid(e.to_string())))?;

        let _permit = self
            .streams
            .acquire()
            .await
            .expect("semaphore is never closed");
        let is_head = method == Method::HEAD;
        let exchange = async {
            let mut sender = self.sender().await?;
            let resp = sender
                .send_request(req)
                .await
                .map_err(|e| Attempt::Retry(format!("request failed: {e}")))?;
            let (parts, incoming) = resp.into_parts();
            let body = if is_head {
                Bytes::new()
            } else {
                Limited::new(incoming, MAX_BODY)
                    .collect()
                    .await
                    .map_err(|e| Attempt::Retry(format!("response body failed: {e}")))?
                    .to_bytes()
            };
            if !sender.is_closed() {
                self.idle.lock().unwrap().push(sender);
            }
            Ok(Reply {
                status: parts.status,
                headers: parts.headers,
                body,
            })
        };
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| Attempt::Retry("timed out".into()))??;
        let s = reply.status;
        // 501(모르는 기능)은 다시 해도 같다
        if (s.is_server_error() && s != StatusCode::NOT_IMPLEMENTED)
            || s == StatusCode::TOO_MANY_REQUESTS
        {
            return Err(Attempt::Retry(error_text(&reply)));
        }
        Ok(reply)
    }

    /// 다시 하기를 포함해 보낸다. 2xx가 아닌 4xx도 그대로 돌려준다(연산마다 뜻이 다르다).
    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        extra: &[(String, String)],
        body: Bytes,
    ) -> Result<Reply, StoreError> {
        let start = tokio::time::Instant::now();
        let mut wait = BACKOFF_START;
        loop {
            match self.attempt(&method, path, query, extra, &body).await {
                Ok(r) => return Ok(r),
                Err(Attempt::Fail(e)) => return Err(e),
                Err(Attempt::Retry(msg)) => {
                    if start.elapsed() + wait > self.retry_budget {
                        return Err(StoreError::Unavailable(format!(
                            "{} {}: {msg}",
                            self.endpoint.authority, self.bucket
                        )));
                    }
                    tracing::debug!(%msg, "S3 request failed; retrying");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(BACKOFF_MAX);
                }
            }
        }
    }

    fn fail(&self, what: &str, r: &Reply) -> StoreError {
        let text = format!("{what}: {}", error_text(r));
        match r.status {
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => StoreError::Denied(text),
            _ => StoreError::Rejected(text),
        }
    }

    async fn copy(&self, from: &str, to: &str, replace_metadata: bool) -> Result<(), StoreError> {
        let source = sigv4::uri_encode(&format!("/{}/{}{}", self.bucket, self.prefix, from), false);
        let mut extra = vec![("x-amz-copy-source".to_string(), source)];
        if replace_metadata {
            extra.push((
                "x-amz-metadata-directive".to_string(),
                "REPLACE".to_string(),
            ));
        }
        let r = self
            .request(
                Method::PUT,
                &self.object_path(to),
                &[],
                &extra,
                Bytes::new(),
            )
            .await?;
        if r.status == StatusCode::NOT_FOUND {
            return Err(StoreError::NotFound(from.into()));
        }
        // CopyObject는 200 안에 오류를 담을 수 있다
        if !r.status.is_success() || body_is_error(&r.body) {
            if error_code(&r.body).as_deref() == Some("NoSuchKey") {
                return Err(StoreError::NotFound(from.into()));
            }
            return Err(self.fail("copy failed", &r));
        }
        Ok(())
    }
}

/// `Box<dyn …>`로 TLS·평문 스트림을 한 꼴로
trait IoBox: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> IoBox for T {}

fn body_is_error(b: &[u8]) -> bool {
    let head = &b[..b.len().min(256)];
    std::str::from_utf8(head).is_ok_and(|s| s.contains("<Error>"))
}

fn error_code(b: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(b).ok()?;
    let doc = roxmltree::Document::parse(s).ok()?;
    let root = doc.root_element();
    (root.tag_name().name() == "Error").then_some(())?;
    child_text(root, "Code")
}

/// 오류 응답의 `Code: Message`(XML이 아니면 상태 이름)
fn error_text(r: &Reply) -> String {
    let parsed = std::str::from_utf8(&r.body)
        .ok()
        .and_then(|s| roxmltree::Document::parse(s).ok())
        .and_then(|doc| {
            let root = doc.root_element();
            let code = child_text(root, "Code")?;
            let msg = child_text(root, "Message").unwrap_or_default();
            Some(if msg.is_empty() {
                code
            } else {
                format!("{code}: {msg}")
            })
        });
    match parsed {
        Some(t) => format!("HTTP {} {t}", r.status.as_u16()),
        None => format!(
            "HTTP {} {}",
            r.status.as_u16(),
            r.status.canonical_reason().unwrap_or("")
        ),
    }
}

fn child_text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
        .map(|n| n.text().unwrap_or("").to_string())
}

/// `ListObjectsV2` 응답: (객체들, 다음 쪽 토큰)
pub fn parse_list(xml: &str) -> Result<(Vec<ObjectInfo>, Option<String>), StoreError> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| StoreError::Rejected(format!("invalid list response: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "ListBucketResult" {
        return Err(StoreError::Rejected("invalid list response".into()));
    }
    let mut out = Vec::new();
    for c in root
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "Contents")
    {
        let key = child_text(c, "Key")
            .ok_or_else(|| StoreError::Rejected("list entry without a key".into()))?;
        let size = child_text(c, "Size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let modified = child_text(c, "LastModified")
            .and_then(|s| parse_iso8601(&s))
            .unwrap_or(0);
        out.push(ObjectInfo {
            key,
            size,
            modified,
        });
    }
    let truncated = child_text(root, "IsTruncated").is_some_and(|s| s == "true");
    let next = if truncated {
        Some(child_text(root, "NextContinuationToken").ok_or_else(|| {
            StoreError::Rejected("truncated list without a continuation token".into())
        })?)
    } else {
        None
    };
    Ok((out, next))
}

fn unix(y: i32, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> Option<u64> {
    let date = time::Date::from_calendar_date(y, time::Month::try_from(mo).ok()?, d).ok()?;
    let t = time::Time::from_hms(h, mi, s).ok()?;
    u64::try_from(
        time::PrimitiveDateTime::new(date, t)
            .assume_utc()
            .unix_timestamp(),
    )
    .ok()
}

/// `2009-10-12T17:50:30.000Z`
pub fn parse_iso8601(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<u32>().ok();
    unix(
        n(0..4)? as i32,
        n(5..7)? as u8,
        n(8..10)? as u8,
        n(11..13)? as u8,
        n(14..16)? as u8,
        n(17..19)? as u8,
    )
}

/// `Wed, 12 Oct 2009 17:50:30 GMT`
pub fn parse_http_date(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mo = months.iter().position(|m| *m == parts[2])? as u8 + 1;
    let hms: Vec<u8> = parts[4]
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if hms.len() != 3 {
        return None;
    }
    unix(
        parts[3].parse().ok()?,
        mo,
        parts[1].parse().ok()?,
        hms[0],
        hms[1],
        hms[2],
    )
}

#[async_trait]
impl Backend for S3Backend {
    fn describe(&self) -> String {
        format!(
            "s3 {}://{}/{}/{}",
            if self.endpoint.https { "https" } else { "http" },
            self.endpoint.authority,
            self.bucket,
            self.prefix
        )
    }

    fn max_streams(&self) -> usize {
        self.max_streams
    }

    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), StoreError> {
        check_key(key)?;
        let r = self
            .request(Method::PUT, &self.object_path(key), &[], &[], data.into())
            .await?;
        if !r.status.is_success() {
            return Err(self.fail("upload failed", &r));
        }
        Ok(())
    }

    async fn put_if_absent(&self, key: &str, data: Vec<u8>) -> Result<bool, StoreError> {
        // 조건부 쓰기를 무시하는 구현이 있으므로 먼저 있음을 본다(부르는 쪽이 다시 읽어 견준다 — 설계 §4)
        if self.head(key).await?.is_some() {
            return Ok(false);
        }
        let path = self.object_path(key);
        let body: Bytes = data.into();
        let extra = [("if-none-match".to_string(), "*".to_string())];
        let r = self
            .request(Method::PUT, &path, &[], &extra, body.clone())
            .await?;
        match r.status {
            s if s.is_success() => Ok(true),
            StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => Ok(false),
            StatusCode::NOT_IMPLEMENTED | StatusCode::BAD_REQUEST
                if error_code(&r.body).is_none_or(|c| c == "NotImplemented") =>
            {
                // 조건부 쓰기를 모른다 — 있음 확인으로 대신한다
                let r = self.request(Method::PUT, &path, &[], &[], body).await?;
                if !r.status.is_success() {
                    return Err(self.fail("upload failed", &r));
                }
                Ok(true)
            }
            _ => Err(self.fail("upload failed", &r)),
        }
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        check_key(key)?;
        let r = self
            .request(Method::GET, &self.object_path(key), &[], &[], Bytes::new())
            .await?;
        match r.status {
            s if s.is_success() => Ok(r.body.to_vec()),
            StatusCode::NOT_FOUND => Err(StoreError::NotFound(key.into())),
            _ => Err(self.fail("download failed", &r)),
        }
    }

    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, StoreError> {
        check_key(key)?;
        let r = self
            .request(Method::HEAD, &self.object_path(key), &[], &[], Bytes::new())
            .await?;
        match r.status {
            s if s.is_success() => {
                let h = |n: &str| r.headers.get(n).and_then(|v| v.to_str().ok());
                Ok(Some(ObjectMeta {
                    size: h("content-length")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                    modified: h("last-modified").and_then(parse_http_date).unwrap_or(0),
                }))
            }
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(self.fail("lookup failed", &r)),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        check_key(key)?;
        let r = self
            .request(
                Method::DELETE,
                &self.object_path(key),
                &[],
                &[],
                Bytes::new(),
            )
            .await?;
        if r.status.is_success() || r.status == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(self.fail("delete failed", &r))
        }
    }

    async fn list(&self, prefix: &str, token: Option<String>) -> Result<ListPage, StoreError> {
        check_prefix(prefix)?;
        let mut query = vec![
            ("list-type".to_string(), "2".to_string()),
            ("max-keys".to_string(), LIST_KEYS.to_string()),
            ("prefix".to_string(), format!("{}{prefix}", self.prefix)),
        ];
        if let Some(t) = token {
            query.push(("continuation-token".to_string(), t));
        }
        let path = format!("{}/", self.endpoint.base_path);
        let r = self
            .request(Method::GET, &path, &query, &[], Bytes::new())
            .await?;
        if !r.status.is_success() {
            return Err(self.fail("list failed", &r));
        }
        let xml = std::str::from_utf8(&r.body)
            .map_err(|_| StoreError::Rejected("invalid list response".into()))?;
        let (objects, next) = parse_list(xml)?;
        let objects = objects
            .into_iter()
            .filter_map(|mut o| {
                o.key = o.key.strip_prefix(&self.prefix)?.to_string();
                Some(o)
            })
            .collect();
        Ok(ListPage { objects, next })
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), StoreError> {
        check_key(from)?;
        check_key(to)?;
        self.copy(from, to, false).await?;
        self.delete(from).await
    }

    async fn touch(&self, key: &str) -> Result<(), StoreError> {
        check_key(key)?;
        self.copy(key, key, true).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(endpoint: &str, path_style: bool) -> S3Target {
        S3Target {
            endpoint: endpoint.into(),
            region: "us-east-1".into(),
            bucket: "bk".into(),
            prefix: Some("orbi/".into()),
            path_style,
            ca_pem: None,
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
            insecure_http: true,
        }
    }

    #[test]
    fn endpoints() {
        let e = parse_endpoint(&target("https://s3.example.com", false)).unwrap();
        assert_eq!(e.host, "bk.s3.example.com");
        assert_eq!(e.authority, "bk.s3.example.com");
        assert_eq!(e.port, 443);
        assert_eq!(e.base_path, "");
        let e = parse_endpoint(&target("http://127.0.0.1:9000", true)).unwrap();
        assert_eq!(e.host, "127.0.0.1");
        assert_eq!(e.authority, "127.0.0.1:9000");
        assert_eq!(e.base_path, "/bk");
        let mut t = target("http://minio:9000", true);
        t.insecure_http = false;
        assert!(parse_endpoint(&t).is_err());
        assert_eq!(store_prefix(&target("https://x", true)), "orbi/");
        let mut t = target("https://x", true);
        t.prefix = None;
        assert_eq!(store_prefix(&t), "");
    }

    #[test]
    fn debug_hides_secret() {
        let b = S3Backend::new(&target("http://127.0.0.1:9000", true), 4).unwrap();
        let d = format!("{b:?}");
        assert!(!d.contains("\"SK\"") && !d.contains("secret"), "{d}");
        assert_eq!(b.describe(), "s3 http://127.0.0.1:9000/bk/orbi/");
        assert_eq!(b.object_path("chunks/a"), "/bk/orbi/chunks/a");
    }

    #[test]
    fn list_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>bk</Name><Prefix>orbi/chunks/</Prefix><KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys>
  <IsTruncated>true</IsTruncated><NextContinuationToken>1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=</NextContinuationToken>
  <Contents><Key>orbi/chunks/abcd/x</Key><LastModified>2009-10-12T17:50:30.000Z</LastModified><ETag>"e"</ETag><Size>434234</Size><StorageClass>STANDARD</StorageClass></Contents>
  <Contents><Key>orbi/chunks/abcd/y</Key><LastModified>2009-10-12T17:50:31.000Z</LastModified><Size>1</Size></Contents>
</ListBucketResult>"#;
        let (objs, next) = parse_list(xml).unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].key, "orbi/chunks/abcd/x");
        assert_eq!(objs[0].size, 434234);
        assert_eq!(objs[0].modified, 1_255_369_830);
        assert_eq!(
            next.as_deref(),
            Some("1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=")
        );
        let (objs, next) =
            parse_list("<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>")
                .unwrap();
        assert!(objs.is_empty() && next.is_none());
        assert!(parse_list("<Error><Code>x</Code></Error>").is_err());
        assert!(
            parse_list("<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>")
                .is_err()
        );
    }

    #[test]
    fn error_xml() {
        let r = Reply {
            status: StatusCode::SERVICE_UNAVAILABLE,
            headers: Default::default(),
            body: Bytes::from_static(
                b"<?xml version=\"1.0\"?><Error><Code>SlowDown</Code><Message>Please reduce your request rate.</Message></Error>",
            ),
        };
        assert_eq!(
            error_text(&r),
            "HTTP 503 SlowDown: Please reduce your request rate."
        );
        assert_eq!(error_code(&r.body).as_deref(), Some("SlowDown"));
        assert!(body_is_error(&r.body));
        let r = Reply {
            status: StatusCode::FORBIDDEN,
            headers: Default::default(),
            body: Bytes::new(),
        };
        assert_eq!(error_text(&r), "HTTP 403 Forbidden");
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_iso8601("2009-10-12T17:50:30.000Z"),
            Some(1_255_369_830)
        );
        assert_eq!(parse_iso8601("2009-10-12T17:50:30Z"), Some(1_255_369_830));
        assert_eq!(parse_iso8601("garbage"), None);
        assert_eq!(
            parse_http_date("Mon, 12 Oct 2009 17:50:30 GMT"),
            Some(1_255_369_830)
        );
        assert_eq!(parse_http_date("12 Oct 2009"), None);
    }
}
