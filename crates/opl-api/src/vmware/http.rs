//! 지문을 고정한 HTTPS(계획 30 `esxi.rs`에서 꺼냄, 설계 §7.1). 서버 인증서는 CA로 검증하지 않고 TLS 핸드셰이크
//! 안에서 SHA-256 지문을 비교한다 — 다르면 요청(자격 증명)을 보내기 전에 끊는다. [`HttpClient`]는 연결을 다시 쓰고
//! (HTTP/1.1 keep-alive) POST·`Range`·쿠키를 쓰는 vSphere 클라이언트의 바탕이다.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1::SendRequest;
use hyper::header::HOST;
use hyper::{HeaderMap, Request, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// TCP 연결과 TLS 핸드셰이크의 시간 제한
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 응답 헤더나 본문 조각을 기다리는 최대 시간(설계 §7.1 — 요청마다 60초). 멈춘 서버가 요청을 붙잡지 못하게 한다.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 쉬는 연결을 이만큼만 다시 쓴다(서버가 먼저 닫은 연결에 요청을 보내지 않게).
const IDLE_REUSE: Duration = Duration::from_secs(30);
/// 쉬는 연결을 이만큼까지 둔다(SOAP 하나·파일 읽기 여럿).
const MAX_IDLE: usize = 4;

/// 연결·요청이 실패한 이유
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("{0}")]
    Connect(String),
    /// 믿기로 한 지문이 없다. 서버 인증서의 지문을 알려 준다.
    #[error("the certificate fingerprint {sha256} is not trusted yet")]
    Untrusted { sha256: String, sha1: String },
    #[error("the certificate fingerprint is {actual}, expected {expected}")]
    Fingerprint { expected: String, actual: String },
    /// 응답 본문이 `limit` 바이트를 넘었다.
    #[error("the response is larger than {0} bytes")]
    TooLarge(usize),
}

fn connect_error(e: impl std::fmt::Display) -> HttpError {
    HttpError::Connect(e.to_string())
}

/// 서버 인증서의 지문 둘(TLS 핸드셰이크에서 잼)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// SHA-256(대문자 콜론 구분 — 우리가 고정하는 값)
    pub sha256: String,
    /// SHA-1(대문자 콜론 구분 — VDDK가 받는 썸프린트)
    pub sha1: String,
}

fn colon_upper(digest: &[u8]) -> String {
    let hex = hex::encode_upper(digest);
    hex.as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(":")
}

/// 인증서 DER의 SHA-256 지문(대문자 콜론 구분, ESXi 화면과 같은 모양)
pub fn fingerprint_of(der: &[u8]) -> String {
    colon_upper(&Sha256::digest(der))
}

/// 인증서 DER의 SHA-1 썸프린트(대문자 콜론 구분, VDDK `thumbprint=` 꼴)
pub fn sha1_thumbprint_of(der: &[u8]) -> String {
    colon_upper(&Sha1::digest(der))
}

/// PEM 인증서(첫 `CERTIFICATE` 블록)의 SHA-256 지문. vCenter가 알려 주는 호스트 인증서
/// (`HostSystem.config.certificate`)를 고정할 때 쓴다.
pub fn pem_fingerprint(pem: &[u8]) -> Option<String> {
    use base64::Engine as _;
    let text = std::str::from_utf8(pem).ok()?;
    let start = text.find("-----BEGIN CERTIFICATE-----")? + "-----BEGIN CERTIFICATE-----".len();
    let end = start + text[start..].find("-----END CERTIFICATE-----")?;
    let b64: String = text[start..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .ok()?;
    Some(fingerprint_of(&der))
}

/// `Authorization: Basic …` 값
pub fn basic_auth(user: &str, password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

/// RFC 3986 unreserved 밖의 바이트를 `%XX`로. `keep_slash`면 `/`는 그대로 둔다.
pub fn percent_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 연결할 호스트: IPv6 주소를 `[::1]`처럼 괄호로 적었으면 괄호를 뗀다.
pub fn bare_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

/// Host 헤더 값(IPv6 주소는 괄호로 감싼다)
pub fn host_header(host: &str, port: u16) -> String {
    let host = bare_host(host);
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// `호스트[:포트]`(IPv6는 `[::1]:443`)를 나눈다. 포트가 없으면 `default`다.
pub fn split_host_port(text: &str, default: u16) -> (String, u16) {
    if let Some(rest) = text.strip_prefix('[')
        && let Some((host, after)) = rest.split_once(']')
    {
        let port = after
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default);
        return (host.to_string(), port);
    }
    match text.rsplit_once(':') {
        // 콜론이 하나뿐이면 포트다(괄호 없는 IPv6 주소는 통째로 호스트).
        Some((host, port)) if !host.contains(':') => match port.parse() {
            Ok(port) => (host.to_string(), port),
            Err(_) => (text.to_string(), default),
        },
        _ => (text.to_string(), default),
    }
}

/// 서버 인증서의 지문만 재고 끊는다(자격 증명은 보내지 않는다).
pub async fn probe(host: &str, port: u16) -> Result<CertInfo, HttpError> {
    match connect(host, port, None).await {
        Err(HttpError::Untrusted { sha256, sha1 }) => Ok(CertInfo { sha256, sha1 }),
        Err(e) => Err(e),
        Ok(_) => Err(connect_error("the TLS handshake did not stop")),
    }
}

/// TCP와 TLS를 연다. `expected`가 없거나 서버 지문과 다르면 핸드셰이크 안에서 끊는다.
pub async fn connect(
    host: &str,
    port: u16,
    expected: Option<String>,
) -> Result<TlsStream<TcpStream>, HttpError> {
    let host = bare_host(host);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinnedCert {
        expected: expected.clone(),
        seen: Mutex::new(None),
        algorithms: provider.signature_verification_algorithms,
    });
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(connect_error)?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    // IP 주소면 SNI를 보내지 않는다(rustls가 알아서 뺀다).
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| connect_error(format!("invalid host name {host}")))?;
    let connector = TlsConnector::from(Arc::new(config));
    let result = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp = TcpStream::connect((host, port)).await?;
        connector.connect(name, tcp).await
    })
    .await;
    match result {
        Err(_) => Err(connect_error("timed out")),
        Ok(Ok(tls)) => Ok(tls),
        Ok(Err(e)) => {
            let seen = verifier
                .seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            Err(match (expected, seen) {
                (None, Some(seen)) => HttpError::Untrusted {
                    sha256: seen.sha256,
                    sha1: seen.sha1,
                },
                (Some(expected), Some(seen)) if expected != seen.sha256 => HttpError::Fingerprint {
                    expected,
                    actual: seen.sha256,
                },
                _ => connect_error(e),
            })
        }
    }
}

/// 서버 끝 인증서의 지문을 고정해 비교한다. 본 지문은 `seen`에 남겨 오류에 쓴다.
#[derive(Debug)]
struct PinnedCert {
    expected: Option<String>,
    seen: Mutex<Option<CertInfo>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let seen = CertInfo {
            sha256: fingerprint_of(end_entity),
            sha1: sha1_thumbprint_of(end_entity),
        };
        let trusted = self.expected.as_deref() == Some(seen.sha256.as_str());
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(seen);
        if trusted {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the certificate fingerprint is not trusted".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// 본문의 다음 데이터 조각(끝이면 None)
pub async fn next_data(body: &mut Incoming) -> Result<Option<Bytes>, HttpError> {
    loop {
        let frame = tokio::time::timeout(IDLE_TIMEOUT, body.frame())
            .await
            .map_err(|_| connect_error("timed out reading the response"))?;
        match frame {
            None => return Ok(None),
            Some(frame) => {
                if let Ok(data) = frame.map_err(connect_error)?.into_data() {
                    return Ok(Some(data));
                }
            }
        }
    }
}

/// 받은 응답(본문을 다 읽음)
#[derive(Debug)]
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

struct Idle {
    sender: SendRequest<Full<Bytes>>,
    since: Instant,
}

/// 한 서버(호스트·포트·고정 지문)로 가는 HTTPS 클라이언트. 본문을 다 읽은 연결은 다시 쓴다(keep-alive).
pub struct HttpClient {
    host: String,
    port: u16,
    fingerprint: String,
    idle: Mutex<Vec<Idle>>,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    /// `fingerprint`는 대문자 콜론 구분 SHA-256이다(정규화한 값).
    pub fn new(host: &str, port: u16, fingerprint: &str) -> Self {
        Self {
            host: bare_host(host).to_string(),
            port,
            fingerprint: fingerprint.to_string(),
            idle: Mutex::new(Vec::new()),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Host 헤더 값
    pub fn authority(&self) -> String {
        host_header(&self.host, self.port)
    }

    fn take_idle(&self) -> Option<SendRequest<Full<Bytes>>> {
        let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        while let Some(i) = idle.pop() {
            if i.since.elapsed() < IDLE_REUSE && !i.sender.is_closed() {
                return Some(i.sender);
            }
        }
        None
    }

    fn put_idle(&self, sender: SendRequest<Full<Bytes>>) {
        let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        if idle.len() < MAX_IDLE && !sender.is_closed() {
            idle.push(Idle {
                sender,
                since: Instant::now(),
            });
        }
    }

    async fn open(&self) -> Result<SendRequest<Full<Bytes>>, HttpError> {
        let tls = connect(&self.host, self.port, Some(self.fingerprint.clone())).await?;
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(connect_error)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(sender)
    }

    /// 요청 하나를 보내고 본문을 `limit` 바이트까지 다 읽는다. Host 헤더는 여기서 넣는다. 다시 쓴 연결이 보내기
    /// 전에 닫혀 있었으면 새 연결로 한 번 더 보낸다(보낸 요청은 다시 보내지 않는다).
    pub async fn send(
        &self,
        mut request: Request<Full<Bytes>>,
        limit: usize,
    ) -> Result<HttpResponse, HttpError> {
        request.headers_mut().insert(
            HOST,
            self.authority()
                .parse()
                .map_err(|_| connect_error("invalid host"))?,
        );
        let (mut sender, reused) = match self.take_idle() {
            Some(s) => (s, true),
            None => (self.open().await?, false),
        };
        if reused && sender.ready().await.is_err() {
            sender = self.open().await?;
        }
        let response = match tokio::time::timeout(IDLE_TIMEOUT, sender.send_request(request)).await
        {
            Err(_) => return Err(connect_error("timed out waiting for a response")),
            Ok(Err(e)) => return Err(connect_error(e)),
            Ok(Ok(r)) => r,
        };
        let (parts, mut body) = response.into_parts();
        let mut out = Vec::new();
        while let Some(data) = next_data(&mut body).await? {
            if out.len() + data.len() > limit {
                return Err(HttpError::TooLarge(limit));
            }
            out.extend_from_slice(&data);
        }
        self.put_idle(sender);
        Ok(HttpResponse {
            status: parts.status,
            headers: parts.headers,
            body: Bytes::from(out),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_is_base64_of_user_and_password() {
        // RFC 7617의 예
        assert_eq!(
            basic_auth("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert_eq!(basic_auth("root", "p"), "Basic cm9vdDpw");
        assert_eq!(basic_auth("ro", "x"), "Basic cm86eA==");
    }

    #[test]
    fn bracketed_ipv6_hosts_connect_without_brackets() {
        assert_eq!(bare_host("[::1]"), "::1");
        assert_eq!(bare_host("::1"), "::1");
        assert_eq!(bare_host("esxi1.lab"), "esxi1.lab");
        assert_eq!(bare_host("10.0.0.5"), "10.0.0.5");
        // Host 헤더는 IPv6면 괄호로 감싼다(한 번만).
        assert_eq!(host_header("[::1]", 443), "[::1]:443");
        assert_eq!(host_header("fe80::1", 8443), "[fe80::1]:8443");
        assert_eq!(host_header("esxi1.lab", 443), "esxi1.lab:443");
    }

    #[test]
    fn splits_host_and_port() {
        assert_eq!(split_host_port("esx1.lab", 443), ("esx1.lab".into(), 443));
        assert_eq!(
            split_host_port("127.0.0.1:8443", 443),
            ("127.0.0.1".into(), 8443)
        );
        assert_eq!(split_host_port("[::1]:9", 443), ("::1".into(), 9));
        assert_eq!(split_host_port("[::1]", 443), ("::1".into(), 443));
        assert_eq!(split_host_port("fe80::1", 443), ("fe80::1".into(), 443));
        assert_eq!(split_host_port("a:b", 443), ("a:b".into(), 443));
    }

    #[test]
    fn fingerprints_are_upper_case_colon_separated() {
        let f = fingerprint_of(b"");
        assert_eq!(f.len(), 32 * 3 - 1);
        assert!(f.starts_with("E3:B0:C4:42:"), "{f}");
        let s = sha1_thumbprint_of(b"");
        assert_eq!(
            s,
            "DA:39:A3:EE:5E:6B:4B:0D:32:55:BF:EF:95:60:18:90:AF:D8:07:09"
        );
    }

    #[test]
    fn pem_fingerprint_hashes_the_der() {
        let key = rcgen::generate_simple_self_signed(vec!["esx1".to_string()]).unwrap();
        let pem = key.cert.pem();
        assert_eq!(
            pem_fingerprint(pem.as_bytes()),
            Some(fingerprint_of(key.cert.der()))
        );
        assert_eq!(pem_fingerprint(b"not a certificate"), None);
    }

    #[test]
    fn percent_encoding_keeps_unreserved() {
        assert_eq!(percent_encode("a b/c&d", true), "a%20b/c%26d");
        assert_eq!(percent_encode("a b/c", false), "a%20b%2Fc");
    }
}
