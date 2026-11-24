//! ESXi·vCenter 데이터스토어 HTTPS 클라이언트(`/folder`). 서버 인증서는 CA로 검증하지 않고 TLS
//! 핸드셰이크 안에서 SHA-256 지문을 고정해 비교한다. 지문이 다르면 자격 증명을 보내기 전에 끊는다.
//! 요청마다 새 연결을 연다(HTTP/1.1, 기본 인증). 지문 고정 TLS는 [`crate::vmware::http`]에 있다(계획 72).

use std::collections::HashSet;
use std::io;
use std::path::Path;

use http_body_util::Empty;
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use opl_types::vm_import::{EsxiConnection, normalize_fingerprint};
use tokio::io::AsyncWriteExt;

use crate::vmware::http::{
    HttpError, IDLE_TIMEOUT, connect as tls_connect, host_header, next_data as http_next_data,
    percent_encode,
};
pub use crate::vmware::http::{basic_auth, fingerprint_of};

/// 디렉터리 목록(HTML) 응답의 최대 크기
const MAX_LISTING_BYTES: usize = 1 << 20;
/// HTML 문자 참조 `&…;`의 `&`와 `;` 사이 최대 길이(`#x10FFFF`도 들어간다)
const MAX_ENTITY: usize = 10;
/// 파일로 보는 확장자. 그 밖의 이름은 디렉터리로 보고, 목록을 읽을 때 HTML이 아니면 건너뛴다
/// (`/folder` 목록에는 파일·디렉터리 구분이 없다).
const FILE_EXTENSIONS: [&str; 20] = [
    "vmx",
    "vmdk",
    "nvram",
    "log",
    "lck",
    "vmsd",
    "vmxf",
    "vswp",
    "vmss",
    "vmsn",
    "vmem",
    "vmtx",
    "hlog",
    "iso",
    "flp",
    "ovf",
    "ova",
    "mf",
    "scoreboard",
    "vmx~",
];

/// ESXi에 연결하거나 읽을 수 없는 이유. 메시지는 사용자에게 그대로 보인다.
#[derive(Debug, thiserror::Error)]
pub enum EsxiError {
    #[error("ESXi rejected the user name or password")]
    Auth,
    #[error("{0} was not found on the ESXi host")]
    NotFound(String),
    #[error("the ESXi host answered HTTP {0}")]
    Http(u16),
    #[error("cannot connect to the ESXi host: {0}")]
    Connect(String),
    /// 믿기로 한 지문이 없다. 서버 인증서의 지문을 알려 준다.
    #[error("the ESXi certificate fingerprint {0} is not trusted yet")]
    Untrusted(String),
    #[error("the ESXi certificate fingerprint is {actual}, expected {expected}")]
    Fingerprint { expected: String, actual: String },
    /// 응답이 크기 제한을 넘었다(`path`는 사용자에게 보일 경로)
    #[error("{path} is larger than {limit} bytes")]
    TooLarge { path: String, limit: usize },
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn connect_error(e: impl std::fmt::Display) -> EsxiError {
    EsxiError::Connect(e.to_string())
}

/// `/folder` 목록의 항목
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// 서버 인증서의 지문만 읽고 끊는다(자격 증명은 보내지 않는다).
pub async fn probe(host: &str, port: u16) -> Result<String, EsxiError> {
    crate::vmware::http::probe(host, port)
        .await
        .map(|cert| cert.sha256)
        .map_err(tls_error)
}

/// 지문 고정 TLS 오류를 ESXi 오류로(계획 30과 같은 뜻)
fn tls_error(e: HttpError) -> EsxiError {
    match e {
        HttpError::Untrusted { sha256, .. } => EsxiError::Untrusted(sha256),
        HttpError::Fingerprint { expected, actual } => EsxiError::Fingerprint { expected, actual },
        HttpError::Connect(m) => EsxiError::Connect(m),
        HttpError::TooLarge(limit) => {
            EsxiError::Invalid(format!("response larger than {limit} bytes"))
        }
    }
}

async fn connect(
    host: &str,
    port: u16,
    expected: Option<String>,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, EsxiError> {
    tls_connect(host, port, expected).await.map_err(tls_error)
}

/// 디렉터리 목록. `datastore`가 없으면 데이터센터의 데이터스토어 목록(`dir`은 무시), 있으면 그
/// 데이터스토어 안 `dir`(`""`은 맨 위)의 바로 아래 항목. 응답이 HTML이 아니면(파일) 빈 목록이다.
pub async fn list(
    conn: &EsxiConnection,
    datastore: Option<&str>,
    dir: &str,
) -> Result<Vec<Entry>, EsxiError> {
    let dir = dir.trim_matches('/');
    let path = if datastore.is_some() { dir } else { "" };
    let response = get(conn, datastore, path).await?;
    let html = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("html"));
    if !html {
        return Ok(Vec::new());
    }
    let what = display_path(datastore, path);
    let body = read_limited(response.into_body(), MAX_LISTING_BYTES, &what).await?;
    Ok(parse_listing(
        &String::from_utf8_lossy(&body),
        datastore,
        dir,
    ))
}

/// 작은 텍스트 파일(`.vmx`·디스크립터)을 읽는다. `limit` 바이트를 넘으면 오류다.
pub async fn get_small(
    conn: &EsxiConnection,
    datastore: &str,
    path: &str,
    limit: usize,
) -> Result<String, EsxiError> {
    let what = display_path(Some(datastore), path);
    let response = get(conn, Some(datastore), path).await?;
    let body = read_limited(response.into_body(), limit, &what).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// 파일을 `dest`로 받는다(스트리밍, fsync). 받은 길이가 `expected_len`과 다르거나 실패하면 `dest`를
/// 지운다.
pub async fn download(
    conn: &EsxiConnection,
    datastore: &str,
    path: &str,
    dest: &Path,
    expected_len: u64,
) -> Result<(), EsxiError> {
    let result = download_to(conn, datastore, path, dest, expected_len).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(dest).await;
    }
    result
}

async fn download_to(
    conn: &EsxiConnection,
    datastore: &str,
    path: &str,
    dest: &Path,
    expected_len: u64,
) -> Result<(), EsxiError> {
    let what = display_path(Some(datastore), path);
    let mut body = get(conn, Some(datastore), path).await?.into_body();
    let mut out = tokio::fs::File::create(dest).await?;
    let mut received = 0u64;
    let short = |n: u64| {
        EsxiError::Invalid(format!(
            "downloaded {n} bytes of {what}, expected {expected_len}"
        ))
    };
    while let Some(data) = next_data(&mut body).await? {
        received += data.len() as u64;
        // 더 오면 디스크를 채우기 전에 멈춘다.
        if received > expected_len {
            return Err(short(received));
        }
        out.write_all(&data).await?;
    }
    if received != expected_len {
        return Err(short(received));
    }
    out.flush().await?;
    out.sync_all().await?;
    Ok(())
}

/// 사용자에게 보일 데이터스토어 경로(`[datastore1] web01/web01.vmx`, vSphere 표기)
fn display_path(datastore: Option<&str>, path: &str) -> String {
    match datastore {
        Some(ds) => format!("[{ds}] {path}"),
        None => "the datastore list".into(),
    }
}

/// `/folder` URL(경로와 쿼리)
fn folder_url(datacenter: &str, datastore: Option<&str>, path: &str) -> String {
    let mut url = String::from("/folder");
    if !path.is_empty() {
        url.push('/');
        url.push_str(&percent_encode(path, true));
    }
    url.push_str("?dcPath=");
    url.push_str(&percent_encode(datacenter, false));
    if let Some(ds) = datastore {
        url.push_str("&dsName=");
        url.push_str(&percent_encode(ds, false));
    }
    url
}

/// GET 하나. 200이 아니면 오류다.
async fn get(
    conn: &EsxiConnection,
    datastore: Option<&str>,
    path: &str,
) -> Result<Response<Incoming>, EsxiError> {
    // 모양이 틀린 지문은 어떤 인증서와도 맞지 않는다(그대로 두어 오류에 보인다).
    let expected = conn
        .fingerprint
        .as_ref()
        .map(|f| normalize_fingerprint(f).unwrap_or_else(|| f.clone()));
    let tls = connect(&conn.host, conn.port, expected).await?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .map_err(connect_error)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = Request::get(folder_url(&conn.datacenter, datastore, path))
        .header(HOST, host_header(&conn.host, conn.port))
        .header(AUTHORIZATION, basic_auth(&conn.user, &conn.password))
        .body(Empty::<Bytes>::new())
        .map_err(|e| EsxiError::Invalid(format!("invalid ESXi request: {e}")))?;
    let response = tokio::time::timeout(IDLE_TIMEOUT, sender.send_request(request))
        .await
        .map_err(|_| connect_error("timed out waiting for a response"))?
        .map_err(connect_error)?;
    match response.status() {
        StatusCode::OK => Ok(response),
        StatusCode::UNAUTHORIZED => Err(EsxiError::Auth),
        StatusCode::NOT_FOUND => Err(EsxiError::NotFound(display_path(datastore, path))),
        status => Err(EsxiError::Http(status.as_u16())),
    }
}

/// 본문의 다음 데이터 조각(끝이면 None)
async fn next_data(body: &mut Incoming) -> Result<Option<Bytes>, EsxiError> {
    http_next_data(body).await.map_err(tls_error)
}

async fn read_limited(mut body: Incoming, limit: usize, what: &str) -> Result<Vec<u8>, EsxiError> {
    let mut out = Vec::new();
    while let Some(data) = next_data(&mut body).await? {
        if out.len() + data.len() > limit {
            return Err(EsxiError::TooLarge {
                path: what.to_string(),
                limit,
            });
        }
        out.extend_from_slice(&data);
    }
    Ok(out)
}

/// `/folder` HTML 목록을 읽는다. `<a href="…">`의 href만 본다(링크 글자는 잘리거나 바뀔 수 있다).
/// 데이터스토어 목록은 `dsName`이 있는 `/folder?…` 링크, 디렉터리 목록은 같은 데이터스토어의
/// `/folder/{dir}/{이름}` 링크다. 위로 가는 링크("Parent Directory"·"Parent Datacenter")는 `dir`의
/// 바로 아래가 아니므로 빠진다.
pub fn parse_listing(html: &str, datastore: Option<&str>, dir: &str) -> Vec<Entry> {
    let dir = dir.trim_matches('/');
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = HashSet::new();
    for href in hrefs(html) {
        let href = decode_entities(&href);
        let (path, query) = href.split_once('?').unwrap_or((&href, ""));
        let ds = query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            // 쿼리 값의 `+`는 공백이다.
            key.eq_ignore_ascii_case("dsName")
                .then(|| percent_decode(&value.replace('+', " ")))
        });
        let Some(path) = path.strip_prefix("/folder") else {
            continue;
        };
        let path = percent_decode(path);
        let path = path.trim_matches('/');
        let entry = match datastore {
            None => match ds {
                Some(name) if path.is_empty() && !name.is_empty() => Entry { name, is_dir: true },
                _ => continue,
            },
            Some(want) => {
                if ds.as_deref() != Some(want) {
                    continue;
                }
                let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
                if parent != dir || name.is_empty() {
                    continue;
                }
                Entry {
                    name: name.to_string(),
                    is_dir: !is_file_name(name),
                }
            }
        };
        if seen.insert(entry.name.clone()) {
            entries.push(entry);
        }
    }
    entries
}

fn is_file_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        FILE_EXTENSIONS
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// `<a … href="…">`의 href 값들(대소문자 무시, 큰·작은따옴표)
fn hrefs(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<a").map(|i| at + i) {
        let end = lower[start..].find('>').map_or(lower.len(), |i| start + i);
        at = end;
        let tag = &lower[start..end];
        // `<abbr>` 같은 다른 태그는 건너뛴다.
        if !tag[2..].starts_with(|c: char| c.is_ascii_whitespace()) {
            continue;
        }
        let Some(h) = tag.find("href") else {
            continue;
        };
        let rest = tag[h + 4..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value_start = end - rest.len();
        let quote = rest.chars().next();
        let value = match quote {
            Some(q @ ('"' | '\'')) => {
                let body = &html[value_start + 1..end];
                body.find(q).map(|i| &body[..i])
            }
            _ => {
                let body = &html[value_start..end];
                Some(body.split_ascii_whitespace().next().unwrap_or(""))
            }
        };
        if let Some(value) = value {
            out.push(value.to_string());
        }
    }
    out
}

/// HTML 문자 참조(`&amp;`, `&#39;`, `&#x2F;` …)를 푼다. 모르는 것은 그대로 둔다. `;`는 `&` 뒤
/// [`MAX_ENTITY`] 바이트 안에서만 찾는다(`;` 없는 `&`가 많아도 한 번만 훑는다).
fn decode_entities(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest
            .bytes()
            .skip(1)
            .take(MAX_ENTITY)
            .position(|b| b == b';')
            .map(|i| i + 1);
        let decoded = end.and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let code = name.strip_prefix('#')?;
                    let n = match code.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => code.parse().ok()?,
                    };
                    char::from_u32(n)
                }
            }?;
            Some((c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `%XX`를 푼다(대소문자 무시). 잘못된 것은 그대로 두고, UTF-8이 아니면 손실 변환한다.
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_urls_encode_paths_and_names() {
        assert_eq!(
            folder_url("ha-datacenter", None, ""),
            "/folder?dcPath=ha-datacenter"
        );
        assert_eq!(
            folder_url("ha-datacenter", Some("datastore1"), ""),
            "/folder?dcPath=ha-datacenter&dsName=datastore1"
        );
        assert_eq!(
            folder_url("DC 1", Some("ssd & co"), "my vm/my vm-flat.vmdk"),
            "/folder/my%20vm/my%20vm-flat.vmdk?dcPath=DC%201&dsName=ssd%20%26%20co"
        );
    }

    #[test]
    fn decodes_entities_and_percent_escapes() {
        assert_eq!(
            decode_entities("a&amp;b&#39;c&#x2F;d&bogus;&"),
            "a&b'c/d&bogus;&"
        );
        // `;`가 `&` 뒤 10바이트를 넘으면 참조가 아니다.
        assert_eq!(decode_entities("&#x00000041;&#x0000041;"), "&#x00000041;A");
        assert_eq!(
            percent_decode("ha%2ddatacenter%20x%zz%E2%82%AC"),
            "ha-datacenter x%zz€"
        );
    }

    #[test]
    fn fingerprints_are_upper_case_colon_separated() {
        let f = fingerprint_of(b"");
        assert_eq!(f.len(), 32 * 3 - 1);
        assert!(f.starts_with("E3:B0:C4:42:"), "{f}");
        assert_eq!(normalize_fingerprint(&f).as_deref(), Some(f.as_str()));
    }
}
