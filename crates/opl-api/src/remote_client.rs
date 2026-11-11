//! 원격 Orbivisor 클러스터의 HTTPS 클라이언트. TLS는 원격 클러스터 CA 하나만 믿고 호스트 이름은 확인하지
//! 않는다(노드 인증서의 이름은 노드마다 다르다). CA는 등록할 때 받아 지문을 확인하고 저장한 인증서
//! 하나다. 요청마다 새 연결을 연다(HTTP/1.1). 리다이렉트는 따르지 않는다(3xx는 그대로 응답).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http_body_util::{BodyExt, Empty};
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{ACCEPT, AUTHORIZATION, HOST, HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, Uri};
use hyper_util::rt::TokioIo;
use opl_types::remote::normalize_url;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{Instant, Sleep};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// TCP 연결과 TLS 핸드셰이크의 시간 제한
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 요청을 다 보낸 뒤 응답 헤더를 기다리는 기본 시간
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// 본문(보내는 것과 받는 것) 조각 사이의 최대 시간
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// `GET /cluster/ca` 본문의 최대 크기
const MAX_CA_BYTES: usize = 64 << 10;
/// [`RemoteClient::get_json`] 본문의 최대 크기
const MAX_JSON_BYTES: usize = 4 << 20;
/// 오류 응답에서 메시지를 읽을 본문의 최대 크기
const MAX_ERROR_BYTES: usize = 64 << 10;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 원격 클러스터에 요청하지 못한 이유. 메시지는 사용자에게 그대로 보인다.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("cannot reach the remote cluster: {0}")]
    Unreachable(String),
    #[error("TLS with the remote cluster failed: {0}")]
    Tls(String),
    /// 원격이 2xx가 아닌 상태로 답했다(오류 본문의 `message`, 없으면 상태 이름)
    #[error("the remote cluster answered HTTP {0}: {1}")]
    Status(u16, String),
    #[error("{0}")]
    Invalid(String),
}

fn unreachable(e: impl std::fmt::Display) -> RemoteError {
    RemoteError::Unreachable(e.to_string())
}

fn invalid(e: impl std::fmt::Display) -> RemoteError {
    RemoteError::Invalid(e.to_string())
}

/// 원격 클러스터의 CA 인증서(PEM, 정확히 하나). 서버 인증서를 검증하지 않고 `GET /api/v1/cluster/ca`를
/// 읽으므로, 부르는 쪽이 지문을 비교해야 한다. 아무 주소에나 연결하므로 오류에 원격이 준 말(오류 본문,
/// 연결·TLS 오류의 자세한 내용)을 옮기지 않는다.
pub async fn fetch_ca(url: &str) -> Result<String, RemoteError> {
    let target = Target::parse(url)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(AnyCert {
        algorithms: provider.signature_verification_algorithms,
    });
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(invalid)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let request = Request::get(target.uri("/cluster/ca")?)
        .header(HOST, &target.authority)
        .body(Empty::<Bytes>::new())
        .map_err(invalid)?;
    let response = send(&target, Arc::new(config), request, HEADER_TIMEOUT)
        .await
        .map_err(plain)?;
    let status = response.status();
    if !status.is_success() {
        let reason = status.canonical_reason().unwrap_or("error");
        return Err(RemoteError::Status(status.as_u16(), reason.to_string()));
    }
    let body = read_limited(&mut response.into_body(), MAX_CA_BYTES)
        .await
        .map_err(plain)?
        .ok_or_else(|| {
            invalid(format!(
                "the CA certificate is larger than {MAX_CA_BYTES} bytes"
            ))
        })?;
    let pem = String::from_utf8(body)
        .ok()
        .filter(|pem| opl_peer::pki::certs(pem).is_ok())
        .ok_or_else(|| invalid("the address is not an Orbivisor cluster (no CA certificate)"))?;
    single_cert(&pem)?;
    Ok(pem)
}

/// 시간 제한 밖의 연결·TLS 오류를 정해진 말로 바꾼다(원격이 준 말을 옮기지 않게).
fn plain(e: RemoteError) -> RemoteError {
    match e {
        RemoteError::Unreachable(m) if m.starts_with("timed out") => RemoteError::Unreachable(m),
        RemoteError::Unreachable(_) => unreachable("cannot connect"),
        RemoteError::Tls(_) => RemoteError::Tls("TLS handshake failed".into()),
        e => e,
    }
}

/// PEM의 인증서 하나. 덧붙인 다른 CA를 믿게 되지 않게 둘 이상이면 받지 않는다.
fn single_cert(pem: &str) -> Result<CertificateDer<'static>, RemoteError> {
    let mut certs = opl_peer::pki::certs(pem).map_err(invalid)?;
    if certs.len() != 1 {
        return Err(invalid("the remote CA must be a single certificate"));
    }
    Ok(certs.remove(0))
}

/// 원격 클러스터 하나의 클라이언트. 경로는 `/api/v1` 아래 경로다(예: `/vms`).
pub struct RemoteClient {
    target: Target,
    config: Arc<ClientConfig>,
    authorization: HeaderValue,
}

// 토큰이 로그에 나가지 않게 주소만 보인다.
impl std::fmt::Debug for RemoteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteClient")
            .field("authority", &self.target.authority)
            .finish_non_exhaustive()
    }
}

impl RemoteClient {
    /// `ca_pem`(정확히 인증서 하나)이 서명한 인증서만 믿는다(호스트 이름은 보지 않는다). `token`은
    /// 원격의 API 토큰이다.
    pub fn new(url: &str, ca_pem: &str, token: &str) -> Result<Self, RemoteError> {
        let target = Target::parse(url)?;
        let mut roots = RootCertStore::empty();
        roots.add(single_cert(ca_pem)?).map_err(invalid)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(invalid)?;
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(invalid)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyName(webpki)))
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| invalid("the API token is not a valid header value"))?;
        authorization.set_sensitive(true);
        Ok(Self {
            target,
            config: Arc::new(config),
            authorization,
        })
    }

    /// `GET <path>`의 JSON 본문(4 MiB까지). 2xx가 아니면 [`RemoteError::Status`].
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, RemoteError> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let response = self
            .request(Method::GET, path, headers, Empty::<Bytes>::new())
            .await?;
        let body = success(response, MAX_JSON_BYTES, path).await?;
        serde_json::from_slice(&body).map_err(|e| {
            invalid(format!(
                "the remote cluster answered invalid JSON for {path}: {e}"
            ))
        })
    }

    /// 요청 하나를 보내고 응답을 그대로 돌려준다(상태를 보지 않고 본문 한도도 없다). 본문은 흘려
    /// 보내고, 보내는 본문과 받는 본문 모두 조각 사이가 60초를 넘으면 끊는다. 응답 헤더는 본문을 다
    /// 보낸 뒤 10초까지 기다린다. `headers`에 더해 `Host`와 `Authorization`(저장한 토큰)을 이
    /// 클라이언트가 채운다.
    pub async fn request<B>(
        &self,
        method: Method,
        path_and_query: &str,
        headers: HeaderMap,
        body: B,
    ) -> Result<Response<RemoteBody>, RemoteError>
    where
        B: Body<Data = Bytes> + Send + Unpin + 'static,
        B::Error: Into<BoxError>,
    {
        self.request_with_timeout(method, path_and_query, headers, body, HEADER_TIMEOUT)
            .await
    }

    /// [`Self::request`]와 같되 응답 헤더를 본문을 다 보낸 뒤 `header_timeout`까지 기다린다. 받는 쪽이
    /// 큰 파일을 내려 쓰고 답하는 업로드가 쓴다.
    pub async fn request_with_timeout<B>(
        &self,
        method: Method,
        path_and_query: &str,
        headers: HeaderMap,
        body: B,
        header_timeout: Duration,
    ) -> Result<Response<RemoteBody>, RemoteError>
    where
        B: Body<Data = Bytes> + Send + Unpin + 'static,
        B::Error: Into<BoxError>,
    {
        let mut request = Request::builder()
            .method(method)
            .uri(self.target.uri(path_and_query)?)
            .body(body)
            .map_err(invalid)?;
        *request.headers_mut() = headers;
        request.headers_mut().insert(
            HOST,
            HeaderValue::from_str(&self.target.authority).map_err(invalid)?,
        );
        request
            .headers_mut()
            .insert(AUTHORIZATION, self.authorization.clone());
        send(&self.target, self.config.clone(), request, header_timeout).await
    }
}

/// [`RemoteClient::request`] 응답의 본문(조각 사이 60초 제한)
pub type RemoteBody = IdleTimeout<Incoming>;

/// 연결할 곳
struct Target {
    /// 연결할 호스트(IPv6 주소는 괄호 없이)
    host: String,
    port: u16,
    /// `Host` 헤더 값(URL의 `<호스트>[:포트]` 그대로)
    authority: String,
}

impl Target {
    fn parse(url: &str) -> Result<Self, RemoteError> {
        let url = normalize_url(url).map_err(invalid)?;
        let authority = url["https://".len()..].to_string();
        let (host, port) = match authority.strip_prefix('[') {
            Some(v6) => {
                let (host, after) = v6.split_once(']').unwrap_or((v6, ""));
                (host, after.strip_prefix(':'))
            }
            None => match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority.as_str(), None),
            },
        };
        let port = match port {
            Some(port) => port.parse().map_err(invalid)?,
            None => 443,
        };
        Ok(Self {
            host: host.to_string(),
            port,
            authority,
        })
    }

    /// `/api/v1` 아래 경로(`/`로 시작)의 요청 URI
    fn uri(&self, path_and_query: &str) -> Result<Uri, RemoteError> {
        if !path_and_query.starts_with('/') {
            return Err(invalid(format!("invalid remote path {path_and_query:?}")));
        }
        format!("/api/v1{path_and_query}")
            .parse()
            .map_err(|_| invalid(format!("invalid remote path {path_and_query:?}")))
    }
}

/// 새 연결을 열어 요청 하나를 보내고 응답 헤더를 본문을 다 보낸 뒤 `header_timeout`까지 기다린다.
async fn send<B>(
    target: &Target,
    config: Arc<ClientConfig>,
    request: Request<B>,
    header_timeout: Duration,
) -> Result<Response<RemoteBody>, RemoteError>
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<BoxError>,
{
    let (parts, body) = request.into_parts();
    let (sent_tx, sent_rx) = oneshot::channel();
    // 본문이 없으면 곧바로 응답 헤더 시간 제한을 센다.
    let sent_tx = (!body.is_end_stream()).then_some(sent_tx);
    let request = Request::from_parts(parts, IdleTimeout::new(body, sent_tx));
    let tls = connect(target, config).await?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .map_err(unreachable)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    // 응답 헤더는 본문을 다 보낸 뒤(또는 보내기를 그만둔 뒤) `header_timeout`까지 기다린다.
    let header_deadline = async {
        let _ = sent_rx.await;
        tokio::time::sleep(header_timeout).await;
    };
    let response = tokio::select! {
        response = sender.send_request(request) => response.map_err(unreachable)?,
        () = header_deadline => return Err(unreachable("timed out waiting for a response")),
    };
    Ok(response.map(|body| IdleTimeout::new(body, None)))
}

/// TCP와 TLS를 연다.
async fn connect(
    target: &Target,
    config: Arc<ClientConfig>,
) -> Result<TlsStream<TcpStream>, RemoteError> {
    // IP 주소면 SNI를 보내지 않는다(rustls가 알아서 뺀다).
    let name = ServerName::try_from(target.host.clone())
        .map_err(|_| invalid(format!("invalid host name {}", target.host)))?;
    let connector = TlsConnector::from(config);
    let result = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp = TcpStream::connect((target.host.as_str(), target.port))
            .await
            .map_err(unreachable)?;
        connector
            .connect(name, tcp)
            .await
            .map_err(|e| RemoteError::Tls(e.to_string()))
    })
    .await;
    match result {
        Err(_) => Err(unreachable("timed out connecting")),
        Ok(result) => result,
    }
}

/// 2xx면 본문(`limit`까지), 아니면 [`RemoteError::Status`]
async fn success(
    response: Response<RemoteBody>,
    limit: usize,
    what: &str,
) -> Result<Vec<u8>, RemoteError> {
    let status = response.status();
    let mut body = response.into_body();
    if status.is_success() {
        return read_limited(&mut body, limit)
            .await?
            .ok_or_else(|| invalid(format!("{what} is larger than {limit} bytes")));
    }
    // 오류 본문의 `message`(Orbivisor 오류 응답), 없으면 상태 이름
    let message = read_limited(&mut body, MAX_ERROR_BYTES)
        .await
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string());
    Err(RemoteError::Status(status.as_u16(), message))
}

/// 본문을 `limit`까지 읽는다. 넘으면 None.
async fn read_limited(body: &mut RemoteBody, limit: usize) -> Result<Option<Vec<u8>>, RemoteError> {
    let mut out = Vec::new();
    while let Some(frame) = body.frame().await {
        let Ok(data) = frame.map_err(unreachable)?.into_data() else {
            continue;
        };
        if out.len() + data.len() > limit {
            return Ok(None);
        }
        out.extend_from_slice(&data);
    }
    Ok(Some(out))
}

/// 조각 사이가 60초를 넘으면 오류로 끝나는 본문. `done`은 본문이 끝나거나 버려질 때 닫힌다.
pub struct IdleTimeout<B> {
    inner: B,
    idle: Pin<Box<Sleep>>,
    done: Option<oneshot::Sender<()>>,
}

impl<B> IdleTimeout<B> {
    fn new(inner: B, done: Option<oneshot::Sender<()>>) -> Self {
        Self {
            inner,
            idle: Box::pin(tokio::time::sleep(IDLE_TIMEOUT)),
            done,
        }
    }
}

impl<B> Body for IdleTimeout<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(frame) => {
                this.idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                if !matches!(frame, Some(Ok(_))) {
                    this.done.take();
                }
                Poll::Ready(frame.map(|f| f.map_err(Into::into)))
            }
            Poll::Pending => match this.idle.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    this.done.take();
                    Poll::Ready(Some(Err("timed out waiting for body data".into())))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// 원격 CA가 서명했는지는 확인하고 이름이 맞지 않는 것만 받아들인다.
#[derive(Debug)]
struct AnyName(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for AnyName {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // webpki는 서명·기간을 먼저 확인하고 이름은 마지막에 본다.
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

/// 서버 인증서를 검증하지 않는다(CA를 받을 때만 쓴다). 핸드셰이크 서명은 확인한다.
#[derive(Debug)]
struct AnyCert {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_split_host_and_port() {
        for (url, host, port, authority) in [
            ("https://h", "h", 443, "h"),
            ("https://h:8443/", "h", 8443, "h:8443"),
            ("https://10.0.0.1:8443", "10.0.0.1", 8443, "10.0.0.1:8443"),
            ("https://[::1]", "::1", 443, "[::1]"),
            ("https://[::1]:8443", "::1", 8443, "[::1]:8443"),
        ] {
            let t = Target::parse(url).unwrap();
            assert_eq!(
                (t.host.as_str(), t.port, t.authority.as_str()),
                (host, port, authority),
                "{url}"
            );
        }
        assert!(matches!(
            Target::parse("http://h"),
            Err(RemoteError::Invalid(_))
        ));
    }

    #[test]
    fn paths_go_under_api_v1() {
        let t = Target::parse("https://h").unwrap();
        assert_eq!(t.uri("/vms?x=1").unwrap(), "/api/v1/vms?x=1");
        for bad in ["vms", "", "/a b"] {
            assert!(matches!(t.uri(bad), Err(RemoteError::Invalid(_))), "{bad}");
        }
    }
}
