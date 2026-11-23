//! 시험용 가짜 S3(기능 `fake-s3`, 설계 §14). 경로 방식 버킷 하나를 메모리에 두고 [`S3Backend`](crate::backend::s3::S3Backend)가
//! 쓰는 연산 여섯을 받는다: `PutObject`(조건부 `If-None-Match: *` 포함)·`GetObject`·`HeadObject`·`DeleteObject`·
//! `ListObjectsV2`·`CopyObject`. SigV4 서명과 본문 해시를 확인하고, 실패(5xx·느림·응답 도중 끊음·조건부 쓰기 모름)를
//! 주입하며, `LastModified` 시계를 조종한다.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use opl_types::backup_store::S3Target;

use crate::backend::Backend;
use crate::backend::mem::MemBackend;
use crate::backend::sigv4::{self, SignInput};

/// 조건부 쓰기(`If-None-Match: *`)를 어떻게 다루나
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conditional {
    /// AWS(2024-08)·MinIO처럼 지킨다(있으면 412)
    Supported,
    /// 모르는 구현처럼 헤더를 무시한다
    Ignored,
    /// 501 `NotImplemented`
    NotImplemented,
}

#[derive(Debug)]
struct Faults {
    /// 다음 n개 요청에 줄 상태
    fail: Vec<u16>,
    /// 다음 n개 요청은 응답 본문 도중 끊음
    cut: u32,
    delay: Duration,
    conditional: Conditional,
    page: usize,
}

struct Shared {
    bucket: String,
    access_key_id: String,
    secret: String,
    objects: MemBackend,
    faults: Mutex<Faults>,
    counts: Mutex<BTreeMap<&'static str, u64>>,
}

/// 떠 있는 가짜 S3. 버리면 멈춘다.
pub struct FakeS3 {
    shared: Arc<Shared>,
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeS3 {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeS3 {
    /// `127.0.0.1`의 빈 포트에서 버킷 `bucket`을 연다.
    pub async fn start(bucket: &str, access_key_id: &str, secret: &str) -> FakeS3 {
        let shared = Arc::new(Shared {
            bucket: bucket.into(),
            access_key_id: access_key_id.into(),
            secret: secret.into(),
            objects: MemBackend::new(),
            faults: Mutex::new(Faults {
                fail: Vec::new(),
                cut: 0,
                delay: Duration::ZERO,
                conditional: Conditional::Supported,
                page: crate::backend::LIST_PAGE,
            }),
            counts: Mutex::new(BTreeMap::new()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake S3");
        let addr = listener.local_addr().expect("local address");
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeS3 { shared, addr, task }
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 이 서버를 가리키는 저장소 대상(경로 방식, HTTP)
    pub fn target(&self, prefix: Option<&str>) -> S3Target {
        S3Target {
            endpoint: self.endpoint(),
            region: "us-east-1".into(),
            bucket: self.shared.bucket.clone(),
            prefix: prefix.map(str::to_string),
            path_style: true,
            ca_pem: None,
            access_key_id: self.shared.access_key_id.clone(),
            secret_access_key: self.shared.secret.clone(),
            insecure_http: true,
        }
    }

    /// 버킷 내용(전체 키 — 접두사 포함)과 시계
    pub fn objects(&self) -> &MemBackend {
        &self.shared.objects
    }

    /// 다음 `n`개 요청에 `status`(예: 503 `SlowDown`)
    pub fn fail_next(&self, n: usize, status: u16) {
        let mut f = self.shared.faults.lock().unwrap();
        f.fail.extend(std::iter::repeat_n(status, n));
    }

    /// 다음 `n`개 요청은 응답 본문 도중 연결을 끊는다(본문이 있는 응답에만 보인다)
    pub fn cut_next(&self, n: u32) {
        self.shared.faults.lock().unwrap().cut += n;
    }

    /// 요청마다 늦춤
    pub fn set_delay(&self, d: Duration) {
        self.shared.faults.lock().unwrap().delay = d;
    }

    pub fn set_conditional(&self, c: Conditional) {
        self.shared.faults.lock().unwrap().conditional = c;
    }

    /// `ListObjectsV2` 한 쪽의 최대 크기(쪽 넘김 시험)
    pub fn set_page_size(&self, n: usize) {
        self.shared.faults.lock().unwrap().page = n.max(1);
    }

    /// 연산별 요청 수(`put`·`get`·`head`·`delete`·`list`·`copy`)
    pub fn count(&self, op: &str) -> u64 {
        self.shared
            .counts
            .lock()
            .unwrap()
            .get(op)
            .copied()
            .unwrap_or(0)
    }
}

fn s3_error(status: StatusCode, code: &str, msg: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>{}</Message></Error>",
        xml_escape(msg)
    );
    (status, [("content-type", "application/xml")], body).into_response()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_query(q: &str) -> Option<Vec<(String, String)>> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            Some((percent_decode(k)?, percent_decode(v)?))
        })
        .collect()
}

fn http_date(unix: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(unix as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let wd = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
        [t.weekday().number_days_from_monday() as usize];
    let mo = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][t.month() as usize - 1];
    format!(
        "{wd}, {:02} {mo} {} {:02}:{:02}:{:02} GMT",
        t.day(),
        t.year(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

fn iso8601(unix: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(unix as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// SigV4 확인. 틀리면 403 응답.
fn check_signature(
    sh: &Shared,
    method: &Method,
    path: &str,
    query: &[(String, String)],
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), Box<Response>> {
    let deny = |code: &str, msg: &str| Box::new(s3_error(StatusCode::FORBIDDEN, code, msg));
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    let auth = h("authorization")
        .and_then(sigv4::parse_authorization)
        .ok_or_else(|| deny("AccessDenied", "missing or malformed authorization"))?;
    if auth.access_key_id != sh.access_key_id {
        return Err(deny("InvalidAccessKeyId", "unknown access key"));
    }
    let payload_hash = h("x-amz-content-sha256")
        .ok_or_else(|| deny("AccessDenied", "missing x-amz-content-sha256"))?;
    if payload_hash != sigv4::sha256_hex(body) {
        return Err(Box::new(s3_error(
            StatusCode::BAD_REQUEST,
            "XAmzContentSHA256Mismatch",
            "payload hash does not match",
        )));
    }
    let amz_date = h("x-amz-date").ok_or_else(|| deny("AccessDenied", "missing x-amz-date"))?;
    if !auth.signed_headers.iter().any(|s| s == "host") {
        return Err(deny("AccessDenied", "host is not signed"));
    }
    let signed: Vec<(String, String)> = auth
        .signed_headers
        .iter()
        .map(|n| (n.clone(), h(n).unwrap_or("").to_string()))
        .collect();
    let (sig, _) = sigv4::signature(
        &SignInput {
            method: method.as_str(),
            canonical_uri: path,
            query,
            headers: &signed,
            payload_hash,
            amz_date,
            region: &auth.region,
            service: &auth.service,
        },
        &sh.secret,
    );
    if sig != auth.signature || !amz_date.starts_with(&auth.date) {
        return Err(deny("SignatureDoesNotMatch", "signature does not match"));
    }
    Ok(())
}

fn bump(sh: &Shared, op: &'static str) {
    *sh.counts.lock().unwrap().entry(op).or_default() += 1;
}

async fn handle(State(sh): State<Arc<Shared>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, 1 << 30).await {
        Ok(b) => b,
        Err(_) => return s3_error(StatusCode::BAD_REQUEST, "IncompleteBody", "body"),
    };
    let (delay, fail, cut, conditional, page) = {
        let mut f = sh.faults.lock().unwrap();
        let fail = (!f.fail.is_empty()).then(|| f.fail.remove(0));
        let cut = f.cut > 0;
        if cut {
            f.cut -= 1;
        }
        (f.delay, fail, cut, f.conditional, f.page)
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    if let Some(status) = fail {
        let s = StatusCode::from_u16(status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
        let code = if status == 503 {
            "SlowDown"
        } else {
            "InternalError"
        };
        return s3_error(s, code, "injected failure");
    }
    let path = parts.uri.path().to_string();
    let Some(query) = parse_query(parts.uri.query().unwrap_or("")) else {
        return s3_error(StatusCode::BAD_REQUEST, "InvalidArgument", "query");
    };
    if let Err(r) = check_signature(&sh, &parts.method, &path, &query, &parts.headers, &body) {
        return *r;
    }
    let Some(decoded) = percent_decode(&path) else {
        return s3_error(StatusCode::BAD_REQUEST, "InvalidURI", "path");
    };
    let rest = decoded.trim_start_matches('/');
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    if bucket != sh.bucket {
        return s3_error(StatusCode::NOT_FOUND, "NoSuchBucket", "no such bucket");
    }
    let resp = route(
        &sh,
        &parts.method,
        key,
        &query,
        &parts.headers,
        body.to_vec(),
        conditional,
        page,
    )
    .await;
    if cut {
        // 머리는 보내고 본문 도중 끊는다
        let stream = futures_util::stream::once(async {
            Err::<bytes::Bytes, std::io::Error>(std::io::Error::other("cut"))
        });
        let mut r = Response::new(Body::from_stream(stream));
        *r.status_mut() = StatusCode::OK;
        r.headers_mut().insert("content-length", 1000.into());
        return r;
    }
    resp
}

#[allow(clippy::too_many_arguments)]
async fn route(
    sh: &Shared,
    method: &Method,
    key: &str,
    query: &[(String, String)],
    headers: &HeaderMap,
    body: Vec<u8>,
    conditional: Conditional,
    page: usize,
) -> Response {
    let q = |n: &str| query.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    let store = &sh.objects;
    let not_found = || {
        s3_error(
            StatusCode::NOT_FOUND,
            "NoSuchKey",
            "The specified key does not exist.",
        )
    };
    if key.is_empty() {
        if *method == Method::GET && q("list-type") == Some("2") {
            bump(sh, "list");
            let prefix = q("prefix").unwrap_or("");
            let max = q("max-keys")
                .and_then(|m| m.parse().ok())
                .unwrap_or(1000usize)
                .min(page);
            store.set_page_size(max);
            let token = q("continuation-token").map(str::to_string);
            let listed = match store.list(prefix, token).await {
                Ok(p) => (p.objects, p.next),
                Err(e) => {
                    return s3_error(StatusCode::BAD_REQUEST, "InvalidArgument", &e.to_string());
                }
            };
            let (objects, next) = listed;
            let mut xml = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
            );
            xml.push_str(&format!(
                "<Name>{}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{max}</MaxKeys><IsTruncated>{}</IsTruncated>",
                xml_escape(&sh.bucket),
                xml_escape(prefix),
                objects.len(),
                next.is_some()
            ));
            if let Some(n) = &next {
                xml.push_str(&format!(
                    "<NextContinuationToken>{}</NextContinuationToken>",
                    xml_escape(n)
                ));
            }
            for o in objects {
                xml.push_str(&format!(
                    "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>\"x\"</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                    xml_escape(&o.key),
                    iso8601(o.modified),
                    o.size
                ));
            }
            xml.push_str("</ListBucketResult>");
            return (StatusCode::OK, [("content-type", "application/xml")], xml).into_response();
        }
        return s3_error(
            StatusCode::NOT_IMPLEMENTED,
            "NotImplemented",
            "bucket operation",
        );
    }
    match *method {
        Method::PUT => {
            if let Some(src) = h("x-amz-copy-source") {
                bump(sh, "copy");
                let Some(src) = percent_decode(src) else {
                    return s3_error(StatusCode::BAD_REQUEST, "InvalidArgument", "copy source");
                };
                let src = src.trim_start_matches('/');
                let Some((b, k)) = src.split_once('/') else {
                    return s3_error(StatusCode::BAD_REQUEST, "InvalidArgument", "copy source");
                };
                if b != sh.bucket {
                    return s3_error(StatusCode::NOT_FOUND, "NoSuchBucket", "no such bucket");
                }
                if k == key && h("x-amz-metadata-directive") != Some("REPLACE") {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "This copy request is illegal because it is trying to copy an object to itself without changing the object's metadata.",
                    );
                }
                let data = match store.get(k).await {
                    Ok(d) => d,
                    Err(_) => return not_found(),
                };
                let _ = store.put(key, data).await;
                let at = store.now();
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><CopyObjectResult><LastModified>{}</LastModified><ETag>\"x\"</ETag></CopyObjectResult>",
                    iso8601(at)
                );
                return (StatusCode::OK, xml).into_response();
            }
            bump(sh, "put");
            if h("if-none-match") == Some("*") {
                match conditional {
                    Conditional::Supported => {
                        return match store.put_if_absent(key, body).await {
                            Ok(true) => StatusCode::OK.into_response(),
                            Ok(false) => s3_error(
                                StatusCode::PRECONDITION_FAILED,
                                "PreconditionFailed",
                                "At least one of the pre-conditions you specified did not hold",
                            ),
                            Err(e) => {
                                s3_error(StatusCode::BAD_REQUEST, "InvalidArgument", &e.to_string())
                            }
                        };
                    }
                    Conditional::NotImplemented => {
                        return s3_error(
                            StatusCode::NOT_IMPLEMENTED,
                            "NotImplemented",
                            "A header you provided implies functionality that is not implemented",
                        );
                    }
                    Conditional::Ignored => {}
                }
            }
            match store.put(key, body).await {
                Ok(()) => StatusCode::OK.into_response(),
                Err(e) => s3_error(StatusCode::BAD_REQUEST, "InvalidArgument", &e.to_string()),
            }
        }
        Method::GET => {
            bump(sh, "get");
            match store.get(key).await {
                Ok(d) => (StatusCode::OK, d).into_response(),
                Err(_) => not_found(),
            }
        }
        Method::HEAD => {
            bump(sh, "head");
            match store.head(key).await {
                Ok(Some(m)) => {
                    let mut r = Response::new(Body::empty());
                    r.headers_mut().insert("content-length", m.size.into());
                    r.headers_mut().insert(
                        "last-modified",
                        http_date(m.modified).parse().expect("header value"),
                    );
                    r
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
        Method::DELETE => {
            bump(sh, "delete");
            let _ = store.delete(key).await;
            StatusCode::NO_CONTENT.into_response()
        }
        _ => s3_error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed", "method"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(http_date(1_255_369_830), "Mon, 12 Oct 2009 17:50:30 GMT");
        assert_eq!(iso8601(1_255_369_830), "2009-10-12T17:50:30.000Z");
        assert_eq!(
            crate::backend::s3::parse_http_date(&http_date(1_700_000_000)),
            Some(1_700_000_000)
        );
        assert_eq!(percent_decode("a%2Fb%3A"), Some("a/b:".into()));
        assert_eq!(percent_decode("%zz"), None);
    }
}
