# opLite 계획 37: 원격 클러스터 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 한 opLite 클러스터에서 다른 클러스터들을 등록해 상태(버전, 노드·VM 수, 자원)를 한눈에 보고, 원격 클러스터의 VM을 보고 켜고 끈다. 별도 관리 서버 없이 각 클러스터가 대등하게 서로를 등록한다. 하위 프로젝트 10의 네 번째 계획이며, 계획 38(원격 복제)이 이 연결을 쓴다.

**Architecture:**
- **신뢰:** 원격은 URL, 클러스터 CA 지문, 그 클러스터에서 만든 API 토큰으로 등록한다. 등록할 때 CA를 받아 지문을 확인하고 그 인증서 하나를 저장한다. 이후 연결은 그 CA만 믿고(노드마다 다른 인증서 이름은 확인하지 않음) 검증 없이 CA를 다시 받지 않는다.
- **권한:** 이쪽에서는 읽기 viewer 이상·쓰기 admin, 원격 쪽에서는 토큰 주인의 역할이 한 번 더 정한다. 중계로 viewer가 읽을 수 있는 경로는 자원 목록으로 제한한다(계정·감사는 로컬 admin).
- **상태:** 리더의 `RemotePoller`가 원격마다 상태를 모아 바뀔 때만 Raft에 쓴다.
- **중계:** `/api/v1/remotes/{key}/api/…`가 원격 API를 흘려 보낸다. 원격의 401은 502로 바꿔 로컬 로그아웃과 헷갈리지 않게 하고, 응답에 `nosniff`·CSP를 붙인다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum·utoipa, hyper·tokio-rustls·rustls(이미 쓰는 판), tokio, clap. 웹은 React·TypeScript·react-query·vitest·msw. 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-06-enterprise-design.md` (§5와 §5.6 "구현(계획 37)")
**선행 계획:** 계획 1~36 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `Remote` 리소스, CA 지문 고정 TLS 클라이언트, 상태 수집 | 원격 클러스터의 전체 웹 화면, 콘솔·watch 중계 |
| API 중계(허용 목록), oplctl `remote`, 웹 원격 화면 | 클러스터 사이 라이브 마이그레이션 |
| | 원격 복제(계획 38) |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 Rust·웹 변경과 시험. 원격은 시험 안의 두 번째 클러스터를 HTTPS로 띄워 쓴다(127.0.0.1). opld E2E는 opld 클러스터 둘.

## 설계 결정

- **CA PEM을 spec에 저장:** 지문만 저장하고 연결 때마다 CA를 받으면, 그 응답에 인증서를 덧붙여 신뢰를 넓히는 공격이 된다. 등록 때 확인한 인증서 하나만 저장해 쓴다.
- **토큰 옮기기:** 바꾸기에서 토큰을 생략하면 URL·지문이 그대로일 때만 남긴다.
- **oplctl `remote vm stop`:** 로컬 `vm stop`과 같은 뜻(ACPI, `--force`는 강제).

## 파일 구조

```
crates/opl-types/src/{remote.rs, resource.rs, lib.rs}
crates/opl-api/src/{remote_client.rs, remotes.rs, remote_proxy.rs, lib.rs, vm_imports/esxi.rs}, crates/opl-api/tests/{remote_client.rs, remotes.rs, common/mod.rs, openapi.rs}
crates/opld/src/{config.rs, member.rs}, crates/opld/tests/remotes.rs, crates/oplctl/src/cli.rs
web/src/remotes/{RemotesPage.tsx, RemotePage.tsx, remotes.ts, …}, web/src/{App.tsx, layout/AppLayout.tsx, auth/permissions.ts, i18n/*.json}
README.md, docs/superpowers/specs/…
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

`web/src/api/schema.d.ts`는 싣지 않는다. 각 Task의 "생성 파일" 단계에서 `pnpm -C web gen:api`로 만든다.

---

### Task 1: 원격 클러스터 타입과 원격 클라이언트

새 리소스 `Remote`(`Kind::Remote`): `RemoteSpec{url, ca_fingerprint, ca_pem, token}`(URL·지문 정규화, 인증서 하나짜리 CA, 토큰은 Debug에서 가림)와 `RemoteStatus`(도달 여부, 클러스터 이름, 버전, 노드·VM 수, CPU·메모리). `opl_api::remote_client`: 원격 CA를 검증 없이 받는 `fetch_ca`(인증서 하나만, 원격의 메시지를 옮기지 않음)와, 그 CA 하나만 믿고 노드 이름은 확인하지 않는 `RemoteClient`(연결마다 새로, 시간 제한, 리다이렉트 없음, 스트리밍).

**Files:**
- Create: `crates/opl-api/src/remote_client.rs`
- Create: `crates/opl-api/tests/remote_client.rs`
- Create: `crates/opl-types/src/remote.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/resource.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
pub mod realms;
pub mod resources;
```

을 다음으로 바꾼다:

```rust
pub mod realms;
pub mod remote_client;
pub mod resources;
```

`crates/opl-api/src/remote_client.rs`를 만든다:

```rust
//! 원격 opLite 클러스터의 HTTPS 클라이언트. TLS는 원격 클러스터 CA 하나만 믿고 호스트 이름은 확인하지
//! 않는다(노드 인증서의 이름은 노드마다 다르다). CA는 등록할 때 받아 지문을 확인하고 저장한 인증서
//! 하나다. 요청마다 새 연결을 연다(HTTP/1.1). 리다이렉트는 따르지 않는다(3xx는 그대로 응답).

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use http_body_util::{BodyExt, Empty};
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{ACCEPT, AUTHORIZATION, HOST, HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, Uri};
use hyper_util::rt::TokioIo;
use opl_types::remote::{Remote, normalize_url};
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
use uuid::Uuid;

/// TCP 연결과 TLS 핸드셰이크의 시간 제한
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 요청을 다 보낸 뒤 응답 헤더를 기다리는 시간
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
    let response = send(&target, Arc::new(config), request)
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
        .ok_or_else(|| invalid("the address is not an opLite cluster (no CA certificate)"))?;
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
    /// 보내고, 보내는 본문과 받는 본문 모두 조각 사이가 60초를 넘으면 끊는다. `headers`에 더해
    /// `Host`와 `Authorization`(저장한 토큰)을 이 클라이언트가 채운다.
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
        send(&self.target, self.config.clone(), request).await
    }
}

/// [`RemoteClient::request`] 응답의 본문(조각 사이 60초 제한)
pub type RemoteBody = IdleTimeout<Incoming>;

/// 원격마다 받은 CA PEM을 노드 메모리에 둔다. 원격의 revision이 바뀌면 다시 받는다. 지문이 맞는
/// 것만 둔다.
#[derive(Default)]
pub struct CaCache {
    inner: Mutex<HashMap<Uuid, (u64, String)>>,
}

impl CaCache {
    /// 원격의 CA PEM. 없으면 [`fetch_ca`]로 받고 저장한 지문과 비교한다(다르면 [`RemoteError::Tls`]).
    pub async fn ca_pem(&self, remote: &Remote) -> Result<String, RemoteError> {
        if let Some(pem) = self.cached(remote) {
            return Ok(pem);
        }
        let pem = fetch_ca(&remote.spec.url).await?;
        let actual = opl_peer::pki::fingerprint(&pem).map_err(invalid)?;
        if actual != remote.spec.ca_fingerprint {
            return Err(RemoteError::Tls(format!(
                "the remote CA fingerprint is {actual}, expected {}",
                remote.spec.ca_fingerprint
            )));
        }
        self.lock()
            .insert(remote.id, (remote.revision, pem.clone()));
        Ok(pem)
    }

    /// 확인한 CA로 만든 원격의 클라이언트
    pub async fn client(&self, remote: &Remote) -> Result<RemoteClient, RemoteError> {
        let pem = self.ca_pem(remote).await?;
        RemoteClient::new(&remote.spec.url, &pem, &remote.spec.token)
    }

    fn cached(&self, remote: &Remote) -> Option<String> {
        match self.lock().get(&remote.id) {
            Some((revision, pem)) if *revision == remote.revision => Some(pem.clone()),
            _ => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, (u64, String)>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

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

/// 새 연결을 열어 요청 하나를 보내고 응답 헤더를 기다린다.
async fn send<B>(
    target: &Target,
    config: Arc<ClientConfig>,
    request: Request<B>,
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
    // 응답 헤더는 본문을 다 보낸 뒤(또는 보내기를 그만둔 뒤) 10초까지 기다린다.
    let header_deadline = async {
        let _ = sent_rx.await;
        tokio::time::sleep(HEADER_TIMEOUT).await;
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
    // 오류 본문의 `message`(opLite 오류 응답), 없으면 상태 이름
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
```

`crates/opl-api/tests/remote_client.rs`를 만든다:

```rust
//! 원격 클러스터 클라이언트: CA 하나만 믿고 이름은 보지 않는 TLS, 검증 없는 CA 받기, 본문 한도,
//! 리다이렉트 안 따름, 시간 제한, 흘려 보내기, CA 캐시. 시험 CA가 `node-x` 이름으로 서명한 인증서를
//! 내는 127.0.0.1 HTTPS 서버를 상대로 한다.

use std::convert::Infallible;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use http_body_util::{BodyExt, Empty, StreamBody};
use hyper::Method;
use hyper::body::{Bytes, Frame};
use opl_api::remote_client::{CaCache, RemoteClient, RemoteError, fetch_ca};
use opl_peer::pki::{ClusterCa, NodeTls, fingerprint, node_key_and_csr};
use opl_types::remote::{Remote, RemoteSpec};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const TOKEN: &str = "opl_0192f0c15d2e70008000000000000000_0123456789abcdef";

#[derive(Clone, Default)]
struct Site {
    ca_body: Arc<Mutex<String>>,
    /// `GET /cluster/ca`의 상태(0이면 200)
    ca_status: Arc<AtomicU16>,
    ca_hits: Arc<AtomicUsize>,
    info_hits: Arc<AtomicUsize>,
    /// `GET /info`가 받은 헤더
    headers: Arc<Mutex<Vec<HeaderMap>>>,
}

/// 시험 CA가 `node-x`(127.0.0.1이 아님)로 서명한 인증서의 HTTPS 서버
struct Server {
    url: String,
    ca_pem: String,
    site: Site,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start() -> Server {
    let ca = ClusterCa::generate("remote").unwrap();
    let (key_pem, csr) = node_key_and_csr().unwrap();
    let tls = NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: ca
            .sign_node(&csr, uuid::Uuid::new_v4(), &["node-x".to_string()])
            .unwrap(),
        key_pem,
    };
    let config = opl_peer::tls::api_server_config(&tls).unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
    let site = Site::default();
    *site.ca_body.lock().unwrap() = ca.cert_pem.clone();
    let app = axum::Router::new()
        .route("/api/v1/cluster/ca", get(ca_pem))
        .route("/api/v1/info", get(info))
        .route("/api/v1/big", get(big))
        .route("/api/v1/missing", get(missing))
        .route("/api/v1/moved", get(moved))
        .route("/api/v1/hang", get(hang))
        .route("/api/v1/echo", post(echo))
        .with_state(site.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Server {
        url: format!("https://{addr}"),
        ca_pem: ca.cert_pem,
        site,
        task,
    }
}

async fn ca_pem(State(site): State<Site>) -> Response {
    site.ca_hits.fetch_add(1, Ordering::SeqCst);
    let status = match site.ca_status.load(Ordering::SeqCst) {
        0 => StatusCode::OK,
        code => StatusCode::from_u16(code).unwrap(),
    };
    let body = site.ca_body.lock().unwrap().clone();
    (
        status,
        [(header::CONTENT_TYPE, "application/x-pem-file")],
        body,
    )
        .into_response()
}

async fn info(State(site): State<Site>, headers: HeaderMap) -> Json<Value> {
    site.info_hits.fetch_add(1, Ordering::SeqCst);
    site.headers.lock().unwrap().push(headers);
    Json(json!({ "name": "dr" }))
}

async fn big() -> Vec<u8> {
    vec![b' '; (4 << 20) + 1]
}

async fn missing() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "code": "not_found", "message": "vm \"web\" not found" })),
    )
        .into_response()
}

async fn moved() -> Response {
    (
        StatusCode::TEMPORARY_REDIRECT,
        [(header::LOCATION, "/api/v1/info")],
    )
        .into_response()
}

async fn hang() -> Response {
    tokio::time::sleep(Duration::from_secs(3600)).await;
    StatusCode::OK.into_response()
}

/// 받는 본문을 조각마다 그대로 돌려준다.
async fn echo(body: Body) -> Response {
    Body::from_stream(body.into_data_stream()).into_response()
}

fn client(server: &Server) -> RemoteClient {
    RemoteClient::new(&server.url, &server.ca_pem, TOKEN).unwrap()
}

async fn read_all(body: opl_api::remote_client::RemoteBody) -> Vec<u8> {
    body.collect().await.unwrap().to_bytes().to_vec()
}

#[tokio::test]
async fn trusts_the_remote_ca_whatever_the_node_name() {
    let server = start().await;
    let value: Value = client(&server).get_json("/info").await.unwrap();
    assert_eq!(value, json!({ "name": "dr" }));

    // 저장한 토큰과 원격의 Host를 보낸다.
    let headers = server.site.headers.lock().unwrap()[0].clone();
    assert_eq!(headers[header::AUTHORIZATION], format!("Bearer {TOKEN}"));
    assert_eq!(headers[header::HOST], server.url["https://".len()..]);
    assert_eq!(headers[header::ACCEPT], "application/json");
}

#[tokio::test]
async fn rejects_a_server_signed_by_another_ca() {
    let server = start().await;
    let other = ClusterCa::generate("other").unwrap();
    let client = RemoteClient::new(&server.url, &other.cert_pem, TOKEN).unwrap();
    let err = client.get_json::<Value>("/info").await.unwrap_err();
    assert!(matches!(err, RemoteError::Tls(_)), "{err:?}");
    assert_eq!(server.site.info_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn new_checks_its_inputs() {
    let server = start().await;
    for (url, ca, token) in [
        ("http://127.0.0.1:1", server.ca_pem.as_str(), TOKEN),
        (server.url.as_str(), "not a pem", TOKEN),
        (server.url.as_str(), server.ca_pem.as_str(), "opl_a\nb"),
    ] {
        let err = RemoteClient::new(url, ca, token).unwrap_err();
        assert!(matches!(err, RemoteError::Invalid(_)), "{url} {err:?}");
    }
    // Debug에 토큰이 없다.
    assert!(!format!("{:?}", client(&server)).contains("0123456789abcdef"));
}

#[tokio::test]
async fn fetch_ca_reads_the_ca_without_verifying() {
    let server = start().await;
    let pem = fetch_ca(&format!("{}/", server.url)).await.unwrap();
    assert_eq!(pem, server.ca_pem);
    assert_eq!(
        fingerprint(&pem).unwrap(),
        fingerprint(&server.ca_pem).unwrap()
    );

    // 64 KiB를 넘거나 PEM이 아니면 받지 않는다.
    *server.site.ca_body.lock().unwrap() = "x".repeat((64 << 10) + 1);
    let err = fetch_ca(&server.url).await.unwrap_err();
    assert!(
        matches!(&err, RemoteError::Invalid(m) if m.contains("larger than 65536")),
        "{err:?}"
    );
    *server.site.ca_body.lock().unwrap() = "hello".into();
    let err = fetch_ca(&server.url).await.unwrap_err();
    assert!(
        matches!(&err, RemoteError::Invalid(m) if m.contains("not an opLite cluster")),
        "{err:?}"
    );
}

/// 검증하지 않고 받으므로 CA는 정확히 인증서 하나여야 한다(덧붙인 CA를 믿게 되지 않게).
#[tokio::test]
async fn the_ca_must_be_a_single_certificate() {
    let server = start().await;
    let other = start().await;
    let bundle = format!("{}{}", server.ca_pem, other.ca_pem);
    *server.site.ca_body.lock().unwrap() = bundle.clone();
    let err = fetch_ca(&server.url).await.unwrap_err();
    assert!(
        matches!(&err, RemoteError::Invalid(m) if m.contains("single certificate")),
        "{err:?}"
    );
    // 첫 인증서의 지문이 맞아도 덧붙인 CA가 서명한 서버는 믿지 않는다.
    let err = RemoteClient::new(&other.url, &bundle, TOKEN).unwrap_err();
    assert!(
        matches!(&err, RemoteError::Invalid(m) if m.contains("single certificate")),
        "{err:?}"
    );
    assert_eq!(other.site.info_hits.load(Ordering::SeqCst), 0);
}

/// 아무 주소에나 검증 없이 연결하므로 원격이 준 말은 옮기지 않는다.
#[tokio::test]
async fn fetch_ca_errors_do_not_repeat_what_the_remote_said() {
    let server = start().await;
    server.site.ca_status.store(404, Ordering::SeqCst);
    *server.site.ca_body.lock().unwrap() =
        json!({ "code": "not_found", "message": "s3cret detail" }).to_string();
    let err = fetch_ca(&server.url).await.unwrap_err();
    assert!(matches!(err, RemoteError::Status(404, _)), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("HTTP 404"), "{text}");
    assert!(!text.contains("s3cret"), "{text}");

    // TLS가 아닌 서버
    let plain = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", plain.local_addr().unwrap());
    let answer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Ok((mut stream, _)) = plain.accept().await {
            let _ = stream
                .write_all(b"HTTP/1.1 400 s3cret\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    let err = fetch_ca(&url).await.unwrap_err();
    answer.abort();
    assert!(
        matches!(&err, RemoteError::Tls(m) if m == "TLS handshake failed"),
        "{err:?}"
    );
}

#[tokio::test]
async fn closed_ports_are_unreachable() {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", tcp.local_addr().unwrap());
    drop(tcp);
    let err = fetch_ca(&url).await.unwrap_err();
    assert!(
        matches!(&err, RemoteError::Unreachable(m) if m == "cannot connect"),
        "{err:?}"
    );
}

#[tokio::test]
async fn limits_json_bodies_and_reports_error_messages() {
    let server = start().await;
    let client = client(&server);
    let err = client.get_json::<Value>("/big").await.unwrap_err();
    assert!(
        matches!(&err, RemoteError::Invalid(m) if m.contains("larger than 4194304")),
        "{err:?}"
    );
    let err = client.get_json::<Value>("/missing").await.unwrap_err();
    assert!(
        matches!(&err, RemoteError::Status(404, m) if m == "vm \"web\" not found"),
        "{err:?}"
    );
    // opLite 오류 본문이 아니면 상태 이름
    let err = client.get_json::<Value>("/nothing-here").await.unwrap_err();
    assert!(
        matches!(&err, RemoteError::Status(404, m) if m == "Not Found"),
        "{err:?}"
    );
}

#[tokio::test]
async fn does_not_follow_redirects() {
    let server = start().await;
    let client = client(&server);
    let response = client
        .request(
            Method::GET,
            "/moved",
            HeaderMap::new(),
            Empty::<Bytes>::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(response.headers()[header::LOCATION], "/api/v1/info");
    let err = client.get_json::<Value>("/moved").await.unwrap_err();
    assert!(matches!(err, RemoteError::Status(307, _)), "{err:?}");
    assert_eq!(server.site.info_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn streams_uploads_and_downloads_both_ways() {
    let server = start().await;
    let (tx, rx) = mpsc::channel::<Bytes>(1);
    let frames = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|data| (Ok::<_, Infallible>(Frame::data(data)), rx))
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/octet-stream".parse().unwrap(),
    );
    // 본문을 하나도 보내기 전에 응답 헤더가 온다.
    let response = client(&server)
        .request(
            Method::POST,
            "/echo",
            headers,
            StreamBody::new(Box::pin(frames)),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    for i in 0..4u8 {
        let chunk = Bytes::from(vec![i; 256 << 10]);
        tx.send(chunk.clone()).await.unwrap();
        // 보낸 조각이 돌아온 다음에 다음 조각을 보낸다.
        let mut got = Vec::new();
        while got.len() < chunk.len() {
            got.extend_from_slice(&body.next().await.unwrap().unwrap());
        }
        assert_eq!(got, chunk);
    }
    drop(tx);
    assert!(body.next().await.is_none());
}

#[tokio::test]
async fn request_returns_any_status_with_its_body() {
    let server = start().await;
    let response = client(&server)
        .request(
            Method::GET,
            "/missing",
            HeaderMap::new(),
            Empty::<Bytes>::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_slice(&read_all(response.into_body()).await).unwrap();
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn stalled_servers_time_out() {
    let server = start().await;
    // TCP 연결만 받고 TLS를 시작하지 않는 서버
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_url = format!("https://{}", silent.local_addr().unwrap());
    let hold = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent.accept().await {
            held.push(stream);
        }
    });

    let start = Instant::now();
    let client = client(&server);
    let (handshake, headers) =
        tokio::join!(fetch_ca(&silent_url), client.get_json::<Value>("/hang"));
    let elapsed = start.elapsed();
    hold.abort();
    for err in [handshake.unwrap_err(), headers.unwrap_err()] {
        assert!(
            matches!(&err, RemoteError::Unreachable(m) if m.contains("timed out")),
            "{err:?}"
        );
    }
    assert!(
        (Duration::from_secs(9)..Duration::from_secs(30)).contains(&elapsed),
        "{elapsed:?}"
    );
}

fn remote(server: &Server, ca_fingerprint: String) -> Remote {
    let mut r = Remote::new(
        "dr",
        RemoteSpec {
            url: server.url.clone(),
            ca_fingerprint,
            token: TOKEN.into(),
            ca_pem: server.ca_pem.clone(),
        },
    );
    r.revision = 7;
    r
}

#[tokio::test]
async fn ca_cache_keeps_a_matching_ca_per_revision() {
    let server = start().await;
    let cache = CaCache::default();
    let mut r = remote(&server, fingerprint(&server.ca_pem).unwrap());

    for _ in 0..2 {
        let client = cache.client(&r).await.unwrap();
        let _: Value = client.get_json("/info").await.unwrap();
    }
    assert_eq!(server.site.ca_hits.load(Ordering::SeqCst), 1);

    // revision이 바뀌면 다시 받는다.
    r.revision = 8;
    assert_eq!(cache.ca_pem(&r).await.unwrap(), server.ca_pem);
    assert_eq!(server.site.ca_hits.load(Ordering::SeqCst), 2);

    // 지문이 다르면 쓰지 않고 두지도 않는다.
    let other = ClusterCa::generate("other").unwrap();
    let mut wrong = remote(&server, fingerprint(&other.cert_pem).unwrap());
    wrong.id = uuid::Uuid::now_v7();
    for _ in 0..2 {
        let err = cache.client(&wrong).await.unwrap_err();
        assert!(
            matches!(&err, RemoteError::Tls(m) if m.contains("fingerprint")),
            "{err:?}"
        );
    }
    assert_eq!(server.site.ca_hits.load(Ordering::SeqCst), 4);
}
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod realm;
pub mod resource;
```

을 다음으로 바꾼다:

```rust
pub mod realm;
pub mod remote;
pub mod resource;
```

`crates/opl-types/src/remote.rs`를 만든다:

```rust
//! 원격 클러스터. 다른 opLite 클러스터의 주소, 그 클러스터 CA의 지문과 인증서, 그 클러스터에서 만든 API
//! 토큰을 두고, 리더가 상태를 모으며 API가 요청을 중계한다. 두 클러스터는 대등하다(원격 쪽 권한은 토큰 주인이 정한다).

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

pub type Remote = Resource<RemoteSpec, RemoteStatus>;

/// API 토큰의 접두(`opl_<id>_<비밀>`)
const TOKEN_PREFIX: &str = "opl_";
/// CA 지문의 접두
const FINGERPRINT_PREFIX: &str = "sha256:";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RemoteSpec {
    /// `https://<호스트>[:포트]`(경로 없음)
    pub url: String,
    /// 원격 클러스터 CA 인증서의 지문 `sha256:<소문자 16진수 64자>`. TLS는 이 CA 하나만 믿는다.
    pub ca_fingerprint: String,
    /// 원격 클러스터에서 만든 API 토큰. API는 내보내지 않는다.
    pub token: String,
    /// 등록하거나 바꿀 때 서버가 받아 `ca_fingerprint`와 맞는지 확인한 원격 클러스터 CA 인증서(PEM, 정확히
    /// 하나). 노드는 연결할 때 CA를 다시 받지 않고 이것만 믿는다.
    pub ca_pem: String,
}

impl ResourceSpec for RemoteSpec {
    const KIND: Kind = Kind::Remote;
    type Status = RemoteStatus;
}

// 토큰이 로그에 나가지 않게 가리고, 긴 CA PEM은 뺀다.
impl std::fmt::Debug for RemoteSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSpec")
            .field("url", &self.url)
            .field("ca_fingerprint", &self.ca_fingerprint)
            .field("token", &"***")
            .finish_non_exhaustive()
    }
}

/// 리더가 주기적으로 모은 원격 클러스터의 상태. 모으지 못하면 `reachable`이 false이고 `error`에 이유가 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RemoteStatus {
    pub reachable: bool,
    /// 상태가 마지막으로 바뀐 시각(유닉스 초). 같은 상태면 다시 쓰지 않으므로 마지막 확인 시각이 아니다.
    pub checked_at: Option<u64>,
    /// 닿지 못한 이유
    pub error: Option<String>,
    pub cluster_name: Option<String>,
    /// 노드 버전 가운데 가장 낮은 것
    pub version: Option<String>,
    /// 위트니스를 뺀 노드 수
    pub nodes: Option<NodeCounts>,
    pub vms: Option<VmCounts>,
    /// 위트니스를 뺀 노드의 CPU 합
    pub cpus: Option<u64>,
    /// 위트니스를 뺀 노드의 메모리 합(GiB)
    pub memory_gib: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeCounts {
    pub total: u64,
    pub online: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmCounts {
    pub total: u64,
    pub running: u64,
}

impl RemoteSpec {
    /// 검증하고 저장할 모양으로 바꾼다: url의 앞뒤 공백과 끝 `/`를 떼고, 지문을 `sha256:<소문자>`로.
    pub fn normalize(&mut self) -> Result<(), RemoteSpecError> {
        self.url = normalize_url(&self.url)?;
        self.ca_fingerprint =
            normalize_ca_fingerprint(&self.ca_fingerprint).ok_or(RemoteSpecError::Fingerprint)?;
        if token_id(&self.token).is_none() {
            return Err(RemoteSpecError::Token);
        }
        Ok(())
    }
}

/// `https://<호스트>[:포트]`(경로는 없거나 `/`, 쿼리·조각·사용자 정보 없음)를 끝 `/` 없이 돌려준다.
pub fn normalize_url(url: &str) -> Result<String, RemoteSpecError> {
    let url = url.trim();
    let rest = url.strip_prefix("https://").ok_or(RemoteSpecError::Url)?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@'])
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(RemoteSpecError::Url);
    }
    let (host_ok, port) = match authority.strip_prefix('[') {
        // [IPv6]:포트
        Some(v6) => {
            let (host, after) = v6.split_once(']').ok_or(RemoteSpecError::Url)?;
            let port = match after {
                "" => None,
                _ => Some(after.strip_prefix(':').ok_or(RemoteSpecError::Url)?),
            };
            (host.parse::<std::net::Ipv6Addr>().is_ok(), port)
        }
        None => {
            let (host, port) = match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            };
            (!host.is_empty() && !host.contains(['[', ']']), port)
        }
    };
    let port_ok = port.is_none_or(|p| {
        p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|p| p != 0)
    });
    if !host_ok || !port_ok {
        return Err(RemoteSpecError::Url);
    }
    Ok(format!("https://{authority}"))
}

/// CA 지문을 `sha256:<소문자 16진수 64자>`로 바꾼다. 접두 `sha256:`은 없어도 되고, 대문자와 `:` 구분
/// (`AB:CD:…`)도 받는다. 그 모양이 아니면 None.
pub fn normalize_ca_fingerprint(text: &str) -> Option<String> {
    let text = text.trim();
    let hex = match text.get(..FINGERPRINT_PREFIX.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(FINGERPRINT_PREFIX) => {
            &text[FINGERPRINT_PREFIX.len()..]
        }
        _ => text,
    };
    let hex: String = hex.chars().filter(|c| *c != ':').collect();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{FINGERPRINT_PREFIX}{}", hex.to_ascii_lowercase()))
}

/// API 토큰(`opl_<id>_<비밀>`)의 id. 그 모양이 아니면 None. 비밀은 HTTP 헤더에 넣을 수 있는
/// 공백 없는 ASCII여야 한다.
pub fn token_id(token: &str) -> Option<Uuid> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    if secret.is_empty() || !secret.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    Uuid::try_parse(id).ok()
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteSpecError {
    #[error(
        "url must be https://<host>[:port] without a path, query, fragment or user information"
    )]
    Url,
    #[error("ca_fingerprint must be sha256: followed by 64 hexadecimal digits")]
    Fingerprint,
    #[error("token must be an opLite API token (opl_<id>_<secret>)")]
    Token,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const FP: &str = "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const TOKEN: &str =
        "opl_0192f0c15d2e70008000000000000000_0123456789abcdef0123456789abcdef0123456789abcdef";
    const CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";

    fn spec() -> RemoteSpec {
        RemoteSpec {
            url: "https://dr.example.com:8443".into(),
            ca_fingerprint: FP.into(),
            token: TOKEN.into(),
            ca_pem: CA_PEM.into(),
        }
    }

    #[test]
    fn kind_is_remote() {
        assert_eq!(RemoteSpec::KIND, Kind::Remote);
        assert_eq!(serde_json::to_value(Kind::Remote).unwrap(), "remote");
    }

    #[test]
    fn spec_round_trips_and_rejects_unknown_fields() {
        let full = json!({
            "url": "https://dr.example.com:8443",
            "ca_fingerprint": FP,
            "token": TOKEN,
            "ca_pem": CA_PEM
        });
        let s: RemoteSpec = serde_json::from_value(full.clone()).unwrap();
        assert_eq!(s, spec());
        assert_eq!(serde_json::to_value(&s).unwrap(), full);

        let mut extra = full.clone();
        extra["bogus"] = json!(1);
        assert!(serde_json::from_value::<RemoteSpec>(extra).is_err());
        for field in ["token", "ca_pem"] {
            let mut missing = full.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<RemoteSpec>(missing).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn debug_hides_the_token() {
        let text = format!("{:?}", spec());
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!text.contains("0123456789abcdef"), "{text}");
        assert!(text.contains("***"), "{text}");
        assert!(text.contains("dr.example.com"), "{text}");
    }

    #[test]
    fn status_defaults_and_round_trips() {
        let empty: RemoteStatus = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, RemoteStatus::default());
        assert!(!empty.reachable);

        let full = json!({
            "reachable": true,
            "checked_at": 1_700_000_000u64,
            "error": null,
            "cluster_name": "dr",
            "version": "0.9.0",
            "nodes": { "total": 3, "online": 2 },
            "vms": { "total": 10, "running": 4 },
            "cpus": 48,
            "memory_gib": 192
        });
        let s: RemoteStatus = serde_json::from_value(full.clone()).unwrap();
        assert_eq!(
            s.nodes,
            Some(NodeCounts {
                total: 3,
                online: 2
            })
        );
        assert_eq!(
            s.vms,
            Some(VmCounts {
                total: 10,
                running: 4
            })
        );
        assert_eq!(serde_json::to_value(&s).unwrap(), full);
    }

    #[test]
    fn normalize_trims_the_url_and_lowers_the_fingerprint() {
        let mut s = RemoteSpec {
            url: " https://dr.example.com:8443/ ".into(),
            ca_fingerprint: format!("SHA256:{}", FP[7..].to_ascii_uppercase()),
            ..spec()
        };
        assert_eq!(s.normalize(), Ok(()));
        assert_eq!(s, spec());
    }

    #[test]
    fn validates_urls() {
        for (url, normal) in [
            ("https://h", "https://h"),
            ("https://h/", "https://h"),
            ("https://h:8443", "https://h:8443"),
            ("https://10.0.0.1:8443/", "https://10.0.0.1:8443"),
            ("https://[::1]", "https://[::1]"),
            ("https://[fe80::1]:8443/", "https://[fe80::1]:8443"),
        ] {
            assert_eq!(normalize_url(url).as_deref(), Ok(normal), "{url}");
        }
        for url in [
            "",
            "h",
            "http://h",
            "HTTPS://h",
            "https://",
            "https:///",
            "https://h/api",
            "https://h//",
            "https://h?x=1",
            "https://h/?x=1",
            "https://h#x",
            "https://u@h",
            "https://u:p@h",
            "https://h :8443",
            "https://:8443",
            "https://h:",
            "https://h:0",
            "https://h:65536",
            "https://h:+1",
            "https://h:x",
            "https://h:1:2",
            "https://[]:8443",
            "https://[::1",
            "https://[::1]x",
            "https://[h]:8443",
            "https://::1",
        ] {
            assert_eq!(normalize_url(url), Err(RemoteSpecError::Url), "{url}");
        }
    }

    #[test]
    fn normalizes_fingerprints() {
        let hex = &FP[7..];
        let colons = hex
            .as_bytes()
            .chunks(2)
            .map(|p| std::str::from_utf8(p).unwrap().to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(":");
        for text in [
            FP.to_string(),
            hex.to_string(),
            hex.to_ascii_uppercase(),
            colons.clone(),
            format!("sha256:{colons}"),
            format!(" SHA256:{hex} "),
        ] {
            assert_eq!(
                normalize_ca_fingerprint(&text).as_deref(),
                Some(FP),
                "{text}"
            );
        }
        for text in [
            "",
            "sha256:",
            &FP[..FP.len() - 1],
            &format!("{FP}0"),
            &format!("sha1:{hex}"),
            &format!("sha256:{}g", &hex[1..]),
            &format!("sha256:{} ", &hex[..32]),
        ] {
            assert_eq!(normalize_ca_fingerprint(text), None, "{text}");
        }
        let mut s = RemoteSpec {
            ca_fingerprint: "sha256:abc".into(),
            ..spec()
        };
        assert_eq!(s.normalize(), Err(RemoteSpecError::Fingerprint));
    }

    #[test]
    fn validates_tokens() {
        assert_eq!(
            token_id(TOKEN),
            Some(Uuid::try_parse("0192f0c15d2e70008000000000000000").unwrap())
        );
        for bad in [
            "",
            "opl_",
            "opl_0192f0c15d2e70008000000000000000",
            "opl_0192f0c15d2e70008000000000000000_",
            "opl_xyz_abc",
            "0192f0c15d2e70008000000000000000_abc",
            "OPL_0192f0c15d2e70008000000000000000_abc",
            "opl_0192f0c15d2e70008000000000000000_a b",
            "opl_0192f0c15d2e70008000000000000000_abc\n",
            "opl_0192f0c15d2e70008000000000000000_ab\u{e9}",
        ] {
            assert_eq!(token_id(bad), None, "{bad:?}");
            let mut s = RemoteSpec {
                token: bad.into(),
                ..spec()
            };
            assert_eq!(s.normalize(), Err(RemoteSpecError::Token), "{bad:?}");
        }
        // 검사 순서: url, 지문, 토큰
        let mut s = RemoteSpec {
            url: "http://h".into(),
            ca_fingerprint: "x".into(),
            token: "x".into(),
            ca_pem: String::new(),
        };
        assert_eq!(s.normalize(), Err(RemoteSpecError::Url));
    }
}
```

`crates/opl-types/src/resource.rs`에서

```rust
    Realm,
}
```

을 다음으로 바꾼다:

```rust
    Realm,
    Remote,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Realm => "realm",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::Realm => "realm",
            Kind::Remote => "remote",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Realm,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::Realm,
            Kind::Remote,
        ] {
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1436개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): 원격 클러스터 타입과 클라이언트 — opl-types remote.rs: Remote = Resource<RemoteSpec, RemoteStatus>, Kind::Remote(\"remote\"). RemoteSpec{url, ca_fingerprint, token}(deny_unknown_fields, Debug는 token을 가림), normalize()가 url(https://<호스트>[:포트] — 경로는 없거나 /, 쿼리·조각·사용자 정보·공백 없음, 포트 1~65535, IPv6는 [주소], 앞뒤 공백과 끝 / 뗌), ca_fingerprint(sha256: 접두는 선택·대소문자 무시, AB:CD 구분 받음 → sha256:<소문자 64자>), token(opl_<uuid>_<공백 없는 ASCII 비밀>)을 이 순서로 검사하고 RemoteSpecError{Url, Fingerprint, Token}. 따로 쓰는 normalize_url·normalize_ca_fingerprint·token_id(토큰의 id). RemoteStatus{reachable, checked_at, error, cluster_name, version, nodes: NodeCounts{total, online}, vms: VmCounts{total, running}, cpus, memory_gib}(serde default). opl-api remote_client.rs: fetch_ca(url)(인증서를 검증하지 않고 GET /api/v1/cluster/ca, 2xx·64 KiB·PEM 확인), RemoteClient::new(url, ca_pem, token)(그 CA만 루트로 한 WebPkiServerVerifier를 감싸 NotValidForName·NotValidForNameContext만 받아들임 — 서명·기간은 그대로 확인), get_json(path)(Accept JSON, 2xx, 4 MiB), request(method, path_and_query, headers, body)(Host·Authorization: Bearer <token>을 채우고 응답을 상태와 관계없이 그대로, 보내고 받는 본문을 흘려 보내며 조각 사이 60초 — Response<RemoteBody>), 경로는 /api/v1 아래, 요청마다 새 HTTP/1.1 연결, TCP·TLS 10초, 본문을 다 보낸 뒤 응답 헤더 10초, 리다이렉트 안 따름. RemoteError{Unreachable, Tls, Status(코드, 오류 본문의 message 또는 상태 이름), Invalid}. CaCache(원격 id별 (revision, PEM), 지문이 맞을 때만 두고 다르면 Tls 오류, client(remote)). 시험: 타입 단위(검증·정규화·JSON·Debug), tests/remote_client.rs(시험 CA가 node-x 이름으로 서명한 127.0.0.1 HTTPS 서버 — 이름이 달라도 연결·다른 CA 거절·fetch_ca 검증 없음과 한도·닫힌 포트·JSON 한도와 오류 메시지·3xx 그대로·양방향 스트리밍 왕복·TLS와 응답 헤더 멈춤 10초·CA 캐시)"
```

---

### Task 2: 원격 클러스터 API와 상태 수집

`/api/v1/remotes` 목록·등록·읽기·바꾸기·지우기(읽기는 viewer 이상, 쓰기는 admin), `POST /remotes/probe`(지문 보기), `POST /remotes/{key}/actions/refresh`. 등록은 CA를 받아 지문을 확인하고(다르면 400, 자기 클러스터면 400) 그 PEM을 저장한 뒤 토큰으로 `whoami`(30초 마감). 토큰은 응답에 없고 `token_id`만. 리더의 `RemotePoller`가 원격마다 상태를 모아 바뀔 때만 쓴다. opld `[remotes] interval·tick`.

**Files:**
- Create: `crates/opl-api/src/remotes.rs`
- Create: `crates/opl-api/tests/remotes.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
pub mod remote_client;
pub mod resources;
```

을 다음으로 바꾼다:

```rust
pub mod remote_client;
mod remotes;
pub mod resources;
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use janitor::{ClusterJanitor, ClusterJanitorConfig};
pub use schedule::{SnapshotScheduler, SnapshotSchedulerConfig};
```

을 다음으로 바꾼다:

```rust
pub use janitor::{ClusterJanitor, ClusterJanitorConfig};
pub use remotes::{RemotePoller, RemotePollerConfig};
pub use schedule::{SnapshotScheduler, SnapshotSchedulerConfig};
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(realms::test_realm))
        .routes(routes!(r::list_nodes))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(realms::test_realm))
        .routes(routes!(remotes::list_remotes, remotes::create_remote))
        .routes(routes!(remotes::probe_remote))
        .routes(routes!(
            remotes::get_remote,
            remotes::update_remote,
            remotes::delete_remote
        ))
        .routes(routes!(remotes::refresh_remote))
        .routes(routes!(r::list_nodes))
```

`crates/opl-api/src/remotes.rs`를 만든다:

```rust
//! 원격 클러스터 `/api/v1/remotes`와 리더가 원격의 상태를 모으는 [`RemotePoller`]. 원격은 누구나
//! 읽고 admin만 바꾼다. 저장한 API 토큰은 응답에 넣지 않고 토큰의 id(`token_id`)만 보인다. 원격은
//! watch로도 내보내지 않는다(토큰을 담고 있다).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::remote::{
    NodeCounts, Remote, RemoteSpec, RemoteStatus, VmCounts, normalize_url, token_id,
};
use opl_types::resource::{Resource, ResourceSpec};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
use crate::remote_client::{RemoteClient, RemoteError, fetch_ca};
use crate::resources::{List, check_name, if_match, lookup};
use crate::state::AppState;

/// 원격 하나의 상태를 모으는 시간 제한
const COLLECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 만들기·바꾸기 전에 원격을 확인하는 시간 제한(전체)
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// 원격 만들기·바꾸기 본문. 바꾸기는 통째로 바꾸되, `token`을 빼면 저장된 토큰을 그대로 둔다
/// (`url`·`ca_fingerprint`가 모두 그대로일 때만 — 하나라도 바뀌면 400). 저장하기 전에 원격에 연결해
/// CA 지문과 토큰을 확인한다(30초 제한). 확인한 CA 인증서는 저장해 두고 연결할 때 다시 받지 않으며,
/// `url`이나 `ca_fingerprint`가 바뀔 때만 다시 받는다.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteRequest {
    /// 만들 때 필요하다. 바꿀 때 주면 지금 이름과 같아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub name: Option<String>,
    /// `https://<호스트>[:포트]`
    pub url: String,
    /// 원격 클러스터 CA의 지문(`sha256:<16진수>` — 대문자·`:` 구분도 받는다)
    pub ca_fingerprint: String,
    /// 원격 클러스터에서 만든 API 토큰. 만들 때 필요하다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub token: Option<String>,
}

// 토큰이 로그에 나가지 않게 가린다.
impl std::fmt::Debug for RemoteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteRequest")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("ca_fingerprint", &self.ca_fingerprint)
            .field("token", &"***")
            .finish()
    }
}

/// 응답의 원격. 토큰 대신 토큰의 id만 보인다(원격에서 어느 토큰인지 알아보게).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RemoteView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub url: String,
    pub ca_fingerprint: String,
    /// 저장한 API 토큰의 id
    #[schema(required = true)]
    pub token_id: Option<Uuid>,
    pub status: RemoteStatus,
}

impl From<Remote> for RemoteView {
    fn from(r: Remote) -> Self {
        Self {
            id: r.id,
            name: r.name,
            revision: r.revision,
            token_id: token_id(&r.spec.token),
            url: r.spec.url,
            ca_fingerprint: r.spec.ca_fingerprint,
            status: r.status,
        }
    }
}

fn remote_unreachable(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "remote_unreachable", e.to_string())
}

/// 저장하기 전에 원격을 확인한다(전체 30초, 넘으면 502 `remote_unreachable`). `spec.ca_pem`이 비었으면
/// CA를 받아(실패 502 `remote_unreachable`) 지문을 비교하고(다르면 400 `remote_fingerprint_mismatch`),
/// 이 클러스터 자신이 아닌지 보고(400 `remote_is_local`), `spec.ca_pem`에 둔다. 그 CA로 토큰을 써
/// `GET /auth/whoami`를 읽는다(401이면 400 `remote_unauthorized`, 그 밖의 실패는 502).
async fn verify(state: &AppState, spec: &mut RemoteSpec) -> Result<(), ApiError> {
    tokio::time::timeout(VERIFY_TIMEOUT, verify_now(state, spec))
        .await
        .unwrap_or_else(|_| Err(remote_unreachable("timed out checking the remote cluster")))
}

async fn verify_now(state: &AppState, spec: &mut RemoteSpec) -> Result<(), ApiError> {
    if spec.ca_pem.is_empty() {
        let pem = fetch_ca(&spec.url).await.map_err(remote_unreachable)?;
        let actual = opl_peer::pki::fingerprint(&pem).map_err(remote_unreachable)?;
        if actual != spec.ca_fingerprint {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "remote_fingerprint_mismatch",
                format!(
                    "the remote CA fingerprint is {actual}, not {}",
                    spec.ca_fingerprint
                ),
            ));
        }
        let local = crate::cluster::cluster_info(state)?;
        let local =
            opl_peer::pki::fingerprint(&local.spec.ca_cert_pem).map_err(ApiError::internal)?;
        if actual == local {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "remote_is_local",
                "the remote is this cluster",
            ));
        }
        spec.ca_pem = pem;
    }
    let client =
        RemoteClient::new(&spec.url, &spec.ca_pem, &spec.token).map_err(remote_unreachable)?;
    match client.get_json::<serde_json::Value>("/auth/whoami").await {
        Ok(_) => Ok(()),
        Err(RemoteError::Status(401, message)) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "remote_unauthorized",
            format!("the remote cluster rejected the token: {message}"),
        )),
        Err(e) => Err(remote_unreachable(e)),
    }
}

/// 요청을 저장할 spec으로. `stored`는 바꿀 때의 지금 값이다(빠진 토큰을 채운다). 연결 대상(`url`·
/// `ca_fingerprint`)이 그대로면 확인한 CA도 그대로 두고, 아니면 비워 [`verify`]가 다시 받게 한다.
fn spec_of(req: &RemoteRequest, stored: Option<&RemoteSpec>) -> Result<RemoteSpec, ApiError> {
    let bad = |e: opl_types::remote::RemoteSpecError| ApiError::bad_request(e.to_string());
    let url = normalize_url(&req.url).map_err(bad)?;
    let ca_fingerprint = opl_types::remote::normalize_ca_fingerprint(&req.ca_fingerprint)
        .ok_or(opl_types::remote::RemoteSpecError::Fingerprint)
        .map_err(bad)?;
    let same = stored.filter(|old| old.url == url && old.ca_fingerprint == ca_fingerprint);
    let token = match (&req.token, stored) {
        (Some(token), _) => token.clone(),
        // 저장된 토큰을 다른 곳으로 보내지 않는다.
        (None, Some(old)) if same.is_some() => old.token.clone(),
        (None, Some(_)) => {
            return Err(ApiError::bad_request(
                "token must be given again when url or ca_fingerprint changes",
            ));
        }
        (None, None) => return Err(ApiError::bad_request("token is required")),
    };
    let mut spec = RemoteSpec {
        url,
        ca_fingerprint,
        token,
        ca_pem: same.map(|old| old.ca_pem.clone()).unwrap_or_default(),
    };
    spec.normalize().map_err(bad)?;
    Ok(spec)
}

#[utoipa::path(get, path = "/remotes", tag = "remotes",
    responses((status = 200, body = List<RemoteView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_remotes(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<RemoteView>>, ApiError> {
    state.authorize(&principal, Action::Read, RemoteSpec::KIND)?;
    let (revision, remotes) = state.cluster.list::<RemoteSpec>()?;
    Ok(Json(List {
        revision,
        items: remotes.into_iter().map(RemoteView::from).collect(),
    }))
}

/// 원격을 확인하고([`verify`]) 저장한 뒤 곧 상태를 한 번 모은다. 나쁜 값은 400 `invalid_request`.
#[utoipa::path(post, path = "/remotes", tag = "remotes",
    request_body = RemoteRequest,
    responses((status = 201, body = RemoteView), (status = "4XX", body = crate::error::ErrorBody),
              (status = 502, body = crate::error::ErrorBody)))]
pub async fn create_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<RemoteRequest>,
) -> Result<(StatusCode, Json<RemoteView>), ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let name = req
        .name
        .clone()
        .ok_or_else(|| ApiError::bad_request("name is required"))?;
    check_name(&name)?;
    // `/remotes/probe`는 경로가 따로 있어 이 이름으로는 읽을 수 없다.
    if name == "probe" {
        return Err(ApiError::bad_request("a remote cannot be named \"probe\""));
    }
    let mut spec = spec_of(&req, None)?;
    // 옛 노드는 새 종류를 풀지 못한다.
    crate::cluster::require_current(&state.cluster)?;
    verify(&state, &mut spec).await?;
    let mut remote: Remote = Resource::new(name, spec);
    remote.revision = state.cluster.create(&remote).await?;
    let remote = refresh(&state, remote).await;
    Ok((StatusCode::CREATED, Json(remote.into())))
}

#[utoipa::path(get, path = "/remotes/{key}", tag = "remotes",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = RemoteView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<RemoteView>, ApiError> {
    state.authorize(&principal, Action::Read, RemoteSpec::KIND)?;
    Ok(Json(lookup::<RemoteSpec>(&state.cluster, &key)?.into()))
}

/// 원격을 통째로 바꾼다(`token` 규칙은 [`RemoteRequest`]). 바뀌면 다시 확인하고 상태를 곧 모은다.
/// 확인하는 사이에 상태만 바뀌었으면(수집기 따위) 다시 확인하지 않고 새 revision에 쓴다(`If-Match`도
/// 그대로 통과). spec이 바뀌었으면 `If-Match`가 있으면 412, 없으면 다시 읽어 바꾼다.
#[utoipa::path(put, path = "/remotes/{key}", tag = "remotes",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = RemoteRequest,
    responses((status = 200, body = RemoteView), (status = "4XX", body = crate::error::ErrorBody),
              (status = 502, body = crate::error::ErrorBody)))]
pub async fn update_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RemoteRequest>,
) -> Result<Json<RemoteView>, ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let expected = if_match(&headers)?;
    // 확인하기 전에 읽은 spec과 확인한 spec(쓰지 못했을 때)
    let mut verified: Option<(RemoteSpec, RemoteSpec)> = None;
    for _ in 0..10 {
        let remote = lookup::<RemoteSpec>(&state.cluster, &key)?;
        let spec = match verified.take() {
            Some((read, spec)) if read == remote.spec => spec,
            _ => {
                if let Some(name) = &req.name
                    && *name != remote.name
                {
                    return Err(ApiError::bad_request(
                        "the name of a remote cannot be changed",
                    ));
                }
                if let Some(expected) = expected
                    && expected != remote.revision
                {
                    return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                        expected,
                        actual: remote.revision,
                    })
                    .into());
                }
                let mut spec = spec_of(&req, Some(&remote.spec))?;
                if spec == remote.spec {
                    return Ok(Json(remote.into()));
                }
                crate::cluster::require_current(&state.cluster)?;
                verify(&state, &mut spec).await?;
                spec
            }
        };
        let revision = match state
            .cluster
            .update_spec(
                remote.id,
                Some(remote.revision),
                remote.labels.clone(),
                &spec,
            )
            .await
        {
            Ok(revision) => revision,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                verified = Some((remote.spec, spec));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let updated = Remote {
            revision,
            spec,
            ..remote
        };
        return Ok(Json(refresh(&state, updated).await.into()));
    }
    Err(ApiError::conflict(
        "busy",
        "the remote kept changing while being updated; try again",
    ))
}

#[utoipa::path(delete, path = "/remotes/{key}", tag = "remotes",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let remote = lookup::<RemoteSpec>(&state.cluster, &key)?;
    crate::cluster::require_current(&state.cluster)?;
    state
        .cluster
        .delete::<RemoteSpec>(remote.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 원격 상태를 곧 다시 모아 적고 돌려준다.
#[utoipa::path(post, path = "/remotes/{key}/actions/refresh", tag = "remotes",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = RemoteView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn refresh_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<RemoteView>, ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let remote = lookup::<RemoteSpec>(&state.cluster, &key)?;
    crate::cluster::require_current(&state.cluster)?;
    Ok(Json(refresh(&state, remote).await.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteProbeRequest {
    /// `https://<호스트>[:포트]`
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RemoteProbe {
    /// 정규화한 URL
    pub url: String,
    /// 받은 CA 인증서의 지문. 원격 클러스터에서 보이는 지문과 같은지 사람이 확인해야 한다.
    pub ca_fingerprint: String,
}

/// 원격의 CA를 인증서를 검증하지 않고 받아 지문을 돌려준다(웹이 등록 전에 보여 주고 확인받는다).
/// 30초 안에 받지 못하면 502 `remote_unreachable`.
#[utoipa::path(post, path = "/remotes/probe", tag = "remotes",
    request_body = RemoteProbeRequest,
    responses((status = 200, body = RemoteProbe), (status = "4XX", body = crate::error::ErrorBody),
              (status = 502, body = crate::error::ErrorBody)))]
pub async fn probe_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<RemoteProbeRequest>,
) -> Result<Json<RemoteProbe>, ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let url = normalize_url(&req.url).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let pem = tokio::time::timeout(VERIFY_TIMEOUT, fetch_ca(&url))
        .await
        .map_err(|_| remote_unreachable("timed out checking the remote cluster"))?
        .map_err(remote_unreachable)?;
    let ca_fingerprint = opl_peer::pki::fingerprint(&pem).map_err(remote_unreachable)?;
    Ok(Json(RemoteProbe {
        url,
        ca_fingerprint,
    }))
}

/// 상태를 모아(10초 제한) 바뀌었으면 적고 원격을 돌려준다.
async fn refresh(state: &AppState, remote: Remote) -> Remote {
    let collected = collect_remote(&remote).await;
    recorded(&state.cluster, remote, collected).await
}

/// 모은 상태를 [`record`]하고 원격을 돌려준다. 적지 않았으면(같거나 그 사이 다른 쓰기가 있었으면) 저장된
/// 것을 다시 읽어 돌려준다(지워졌으면 가진 것). 적지 못하면 로그만 남긴다.
async fn recorded(cluster: &Cluster, mut remote: Remote, collected: RemoteStatus) -> Remote {
    match record(cluster, &remote, collected).await {
        Ok(Some((revision, status))) => {
            remote.revision = revision;
            remote.status = status;
            remote
        }
        Ok(None) => cluster
            .get::<RemoteSpec>(remote.id)
            .ok()
            .flatten()
            .unwrap_or(remote),
        Err(e) => {
            tracing::warn!(remote = %remote.name, error = %e, "cannot record the remote status");
            remote
        }
    }
}

/// 저장한 CA로 연결해 상태를 모은다(10초 제한).
async fn collect_remote(remote: &Remote) -> RemoteStatus {
    let work = async {
        match RemoteClient::new(&remote.spec.url, &remote.spec.ca_pem, &remote.spec.token) {
            Ok(client) => collect(&client).await,
            Err(e) => unreachable_status(&e),
        }
    };
    tokio::time::timeout(COLLECT_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| unreachable_status(&"timed out collecting the status"))
}

/// 모은 상태가 저장된 것과 다르면 `checked_at`을 지금으로 해 적는다(같으면 쓰지 않는다 — Raft 부하).
/// 그 사이 원격이 바뀌었으면 쓰지 않는다. 적었으면 새 revision과 상태.
async fn record(
    cluster: &Cluster,
    remote: &Remote,
    mut status: RemoteStatus,
) -> Result<Option<(u64, RemoteStatus)>, ClusterError> {
    status.checked_at = remote.status.checked_at;
    if status == remote.status {
        return Ok(None);
    }
    status.checked_at = Some(now());
    match cluster
        .update_status_checked::<RemoteSpec>(remote.id, remote.revision, &status)
        .await
    {
        Ok(revision) => Ok(Some((revision, status))),
        Err(ClusterError::Rejected(
            Rejection::RevisionMismatch { .. } | Rejection::NotFound { .. },
        )) => Ok(None),
        Err(e) => Err(e),
    }
}

fn unreachable_status(e: &dyn std::fmt::Display) -> RemoteStatus {
    RemoteStatus {
        reachable: false,
        error: Some(e.to_string()),
        ..RemoteStatus::default()
    }
}

/// 원격이 돌려주는 목록(필요한 필드만 읽는다 — 원격의 버전이 달라도 읽히게)
#[derive(Deserialize)]
struct Items<T> {
    items: Vec<T>,
}

#[derive(Deserialize)]
struct ClusterName {
    name: String,
}

#[derive(Deserialize)]
struct RemoteNode {
    #[serde(default)]
    spec: RemoteNodeSpec,
    #[serde(default)]
    status: RemoteNodeStatus,
}

#[derive(Default, Deserialize)]
struct RemoteNodeSpec {
    #[serde(default)]
    witness: bool,
}

#[derive(Default, Deserialize)]
struct RemoteNodeStatus {
    #[serde(default)]
    online: bool,
    #[serde(default)]
    cpus: u64,
    #[serde(default)]
    memory_mib: u64,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Deserialize)]
struct RemoteVm {
    #[serde(default)]
    status: RemoteVmStatus,
}

#[derive(Default, Deserialize)]
struct RemoteVmStatus {
    #[serde(default)]
    phase: String,
}

/// `GET /cluster`, `/nodes`, `/vms`를 읽어 상태를 계산한다(`checked_at`은 비운다).
async fn collect(client: &RemoteClient) -> RemoteStatus {
    let read = async {
        let cluster: ClusterName = client.get_json("/cluster").await?;
        let nodes: Items<RemoteNode> = client.get_json("/nodes").await?;
        let vms: Items<RemoteVm> = client.get_json("/vms").await?;
        Ok::<_, RemoteError>(summarize(cluster.name, &nodes.items, &vms.items))
    };
    read.await.unwrap_or_else(|e| unreachable_status(&e))
}

/// 노드 수·CPU·메모리는 위트니스를 빼고, 버전은 위트니스를 포함한 노드 가운데 가장 낮은 것이다.
fn summarize(cluster_name: String, nodes: &[RemoteNode], vms: &[RemoteVm]) -> RemoteStatus {
    let version = nodes
        .iter()
        .filter_map(|n| n.status.version.as_deref())
        .min_by(|a, b| opl_types::version::compare(a, b))
        .map(str::to_string);
    let hosts: Vec<&RemoteNode> = nodes.iter().filter(|n| !n.spec.witness).collect();
    RemoteStatus {
        reachable: true,
        checked_at: None,
        error: None,
        cluster_name: Some(cluster_name),
        version,
        nodes: Some(NodeCounts {
            total: hosts.len() as u64,
            online: hosts.iter().filter(|n| n.status.online).count() as u64,
        }),
        vms: Some(VmCounts {
            total: vms.len() as u64,
            running: vms.iter().filter(|v| v.status.phase == "running").count() as u64,
        }),
        cpus: Some(hosts.iter().map(|n| n.status.cpus).sum()),
        memory_gib: Some(hosts.iter().map(|n| n.status.memory_mib).sum::<u64>() / 1024),
    }
}

/// 원격 상태 수집 설정
#[derive(Debug, Clone)]
pub struct RemotePollerConfig {
    /// 모을 때가 됐는지 확인하는 간격
    pub tick: Duration,
    /// 원격마다 상태를 모으는 간격
    pub interval: Duration,
}

impl Default for RemotePollerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(5),
            interval: Duration::from_secs(30),
        }
    }
}

/// 원격 상태 수집 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 모은다.
pub struct RemotePoller {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RemotePoller {
    pub fn spawn(state: Arc<AppState>, config: RemotePollerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    state: Arc<AppState>,
    config: RemotePollerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 원격마다 마지막으로 모은 때(리더의 메모리에만)
    let mut polled: HashMap<Uuid, Instant> = HashMap::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config, &mut polled) => {}
        }
    }
}

async fn tick(state: &AppState, config: &RemotePollerConfig, polled: &mut HashMap<Uuid, Instant>) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        // 다시 리더가 되면 곧 모은다.
        polled.clear();
        return;
    }
    // 옛 노드는 원격 종류를 풀지 못한다.
    if crate::cluster::require_current(cluster).is_err() {
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok((_, remotes)) = cluster.list::<RemoteSpec>() else {
        return;
    };
    polled.retain(|id, _| remotes.iter().any(|r| r.id == *id));
    let due: Vec<Remote> = remotes
        .into_iter()
        .filter(|r| {
            polled
                .get(&r.id)
                .is_none_or(|t| t.elapsed() >= config.interval)
        })
        .collect();
    let started = Instant::now();
    for remote in &due {
        polled.insert(remote.id, started);
    }
    // 원격들을 동시에 모은다(각각 10초 제한).
    futures_util::future::join_all(due.into_iter().map(|remote| async move {
        let status = collect_remote(&remote).await;
        if let Err(e) = record(cluster, &remote, status).await {
            tracing::warn!(remote = %remote.name, error = %e, "cannot record the remote status");
        }
    }))
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(witness: bool, online: bool, version: Option<&str>) -> RemoteNode {
        RemoteNode {
            spec: RemoteNodeSpec { witness },
            status: RemoteNodeStatus {
                online,
                cpus: 8,
                memory_mib: 16 * 1024 + 512,
                version: version.map(str::to_string),
            },
        }
    }

    fn vm(phase: &str) -> RemoteVm {
        RemoteVm {
            status: RemoteVmStatus {
                phase: phase.into(),
            },
        }
    }

    #[test]
    fn summaries_leave_witnesses_out_and_take_the_lowest_version() {
        let nodes = [
            node(false, true, Some("0.37.0")),
            node(false, false, None),
            node(true, true, Some("0.36.1")),
        ];
        let vms = [vm("running"), vm("stopped"), vm("running")];
        let status = summarize("dr".into(), &nodes, &vms);
        assert_eq!(
            status,
            RemoteStatus {
                reachable: true,
                checked_at: None,
                error: None,
                cluster_name: Some("dr".into()),
                version: Some("0.36.1".into()),
                nodes: Some(NodeCounts {
                    total: 2,
                    online: 1
                }),
                vms: Some(VmCounts {
                    total: 3,
                    running: 2
                }),
                cpus: Some(16),
                memory_gib: Some(33),
            }
        );
        assert_eq!(summarize("dr".into(), &[], &[]).version, None);
    }

    #[test]
    fn remote_lists_ignore_fields_this_build_does_not_know() {
        let nodes: Items<RemoteNode> = serde_json::from_value(serde_json::json!({
            "revision": 9,
            "items": [{ "id": "x", "spec": { "witness": false, "new": 1 },
                        "status": { "online": true, "cpus": 2, "memory_mib": 2048, "later": [] } }]
        }))
        .unwrap();
        assert_eq!(summarize("c".into(), &nodes.items, &[]).cpus, Some(2));
    }

    #[test]
    fn the_token_is_kept_only_for_the_same_url_and_ca() {
        let fp = format!("sha256:{}", "ab".repeat(32));
        let stored = RemoteSpec {
            url: "https://dr:8443".into(),
            ca_fingerprint: fp.clone(),
            token: "opl_0192f0c15d2e70008000000000000000_secret".into(),
            ca_pem: "pem".into(),
        };
        let req = |url: &str, ca: &str, token: Option<&str>| RemoteRequest {
            name: None,
            url: url.into(),
            ca_fingerprint: ca.into(),
            token: token.map(str::to_string),
        };
        // 정규화한 뒤 비교한다. 확인한 CA도 그대로 둔다.
        let same = spec_of(
            &req("https://dr:8443/", &fp.to_uppercase(), None),
            Some(&stored),
        )
        .unwrap();
        assert_eq!(same, stored);
        // 연결 대상이 바뀌면 CA를 비워 다시 받게 한다.
        let new_token = "opl_0192f0c15d2e70008000000000000000_other";
        let moved = spec_of(&req("https://dr2", &fp, Some(new_token)), Some(&stored)).unwrap();
        assert_eq!(moved.ca_pem, "");
        let token_only = spec_of(&req("https://dr:8443", &fp, Some(new_token)), Some(&stored));
        assert_eq!(token_only.unwrap().ca_pem, "pem");
        for changed in [
            req("https://other:8443", &fp, None),
            req(
                "https://dr:8443",
                &format!("sha256:{}", "cd".repeat(32)),
                None,
            ),
        ] {
            let e = spec_of(&changed, Some(&stored)).unwrap_err();
            assert!(
                e.message.contains("token must be given again"),
                "{}",
                e.message
            );
        }
        assert!(spec_of(&req("https://dr", &fp, None), None).is_err());
        let debug = format!("{:?}", req("https://dr", &fp, Some("opl_x_s3cret")));
        assert!(!debug.contains("s3cret"), "{debug}");
    }

    /// 노드 하나짜리 클러스터
    async fn one_node_cluster(dir: &std::path::Path) -> Cluster {
        use opl_cluster::ClusterConfig;
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::NodeId;

        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, &cluster);
        cluster
            .initialize(std::collections::BTreeMap::from([(
                id,
                openraft::BasicNode::new(id.to_string()),
            )]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == openraft::ServerState::Leader, "leader")
            .await
            .unwrap();
        cluster
    }

    /// 모으는 사이 다른 쓰기가 있어 적지 못하면 모은 것이 아니라 저장된 것을 돌려준다.
    #[tokio::test]
    async fn a_lost_status_write_returns_the_stored_remote() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut remote: Remote = Resource::new(
            "dr",
            RemoteSpec {
                url: "https://dr".into(),
                ca_fingerprint: format!("sha256:{}", "ab".repeat(32)),
                token: "opl_0192f0c15d2e70008000000000000000_secret".into(),
                ca_pem: "pem".into(),
            },
        );
        remote.revision = cluster.create(&remote).await.unwrap();
        // 모으는 사이 수집기가 다른 상태를 적었다.
        let meanwhile = RemoteStatus {
            reachable: true,
            checked_at: Some(1),
            cluster_name: Some("dr".into()),
            ..RemoteStatus::default()
        };
        let revision = cluster
            .update_status::<RemoteSpec>(remote.id, &meanwhile)
            .await
            .unwrap();
        let got = recorded(&cluster, remote.clone(), unreachable_status(&"down")).await;
        assert_eq!((got.revision, &got.status), (revision, &meanwhile));

        // 그대로 적을 수 있으면 모은 것을 적는다.
        let got = recorded(&cluster, got, unreachable_status(&"down")).await;
        assert!(got.revision > revision);
        assert_eq!(got.status.error.as_deref(), Some("down"));
        let stored = cluster.get::<RemoteSpec>(remote.id).unwrap().unwrap();
        assert_eq!((stored.revision, stored.status), (got.revision, got.status));
    }
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
```

을 다음으로 바꾼다:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::serve::ListenerExt;
use openraft::{BasicNode, ServerState};
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
```

을 다음으로 바꾼다:

```rust
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        self.hv.inspect(id).await.unwrap().map(|s| s.pid)
    }
```

을 다음으로 바꾼다:

```rust
        self.hv.inspect(id).await.unwrap().map(|s| s.pid)
    }

    /// 이 클러스터 CA 인증서(PEM)
    pub fn ca_pem(&self) -> String {
        let info = self.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
        info.spec.ca_cert_pem
    }

    /// 이 클러스터 CA 인증서의 지문(`sha256:…`)
    pub fn ca_fingerprint(&self) -> String {
        opl_peer::pki::fingerprint(&self.ca_pem()).unwrap()
    }

    /// 같은 라우터를 이 클러스터 CA가 `node-x`(127.0.0.1이 아님)로 서명한 인증서로 HTTPS 서비스한다.
    /// 다른 클러스터가 원격으로 등록할 때 쓴다. `GET /cluster/ca`를 받은 수를 센다. 돌려준 서버를 버리면
    /// 멈춘다.
    pub async fn serve_tls(&self) -> TlsServer {
        let info = self.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
        let ca = opl_peer::pki::ClusterCa {
            cert_pem: info.spec.ca_cert_pem,
            key_pem: info.spec.ca_key_pem,
        };
        let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
        let tls = opl_peer::pki::NodeTls {
            cert_pem: ca.sign_node(&csr, self.node, &["node-x".into()]).unwrap(),
            ca_pem: ca.cert_pem,
            key_pem,
        };
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", tcp.local_addr().unwrap());
        let config = opl_peer::tls::api_server_config(&tls).unwrap();
        // opld처럼 연결 주소를 넘긴다.
        let listener = opl_peer::tls::TlsListener::new(tcp, config)
            .unwrap()
            .tap_io(|_| {});
        let ca_hits = Arc::new(AtomicUsize::new(0));
        let hits = ca_hits.clone();
        let app = opl_api::router(self.state.clone()).layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                if req.uri().path() == "/api/v1/cluster/ca" {
                    hits.fetch_add(1, Ordering::SeqCst);
                }
                next.run(req)
            },
        ));
        let task = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });
        TlsServer { url, ca_hits, task }
    }
}

/// [`Env::serve_tls`]의 HTTPS 서버. 버리면 새 연결을 받지 않는다.
pub struct TlsServer {
    /// `https://127.0.0.1:<포트>`
    pub url: String,
    /// `GET /api/v1/cluster/ca`를 받은 수
    pub ca_hits: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/realms/{key}/actions/test",
        "/version",
```

을 다음으로 바꾼다:

```rust
        "/realms/{key}/actions/test",
        "/remotes",
        "/remotes/probe",
        "/remotes/{key}",
        "/remotes/{key}/actions/refresh",
        "/version",
```

`crates/opl-api/tests/remotes.rs`를 만든다:

```rust
//! 원격 클러스터 API: 원격 역할을 하는 두 번째 `Env`(그 클러스터 CA가 서명한 인증서로 HTTPS 서비스)를
//! 등록하고 상태를 모으는지, 등록 확인 순서와 오류 코드(지문 틀림·자기 자신·토큰 틀림·꺼진 원격),
//! 토큰을 내보내지 않음과 PUT의 token 규칙, 권한, 옛 노드 409, `probe`, `refresh`, 상태 수집(바뀔 때만
//! 쓰기, 원격이 꺼지면 reachable false).

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Env, TlsServer, vm_request};
use opl_api::{RemotePoller, RemotePollerConfig};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::remote::{RemoteSpec, token_id};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 원격 역할을 하는 클러스터와 그 HTTPS 서버, 그곳 admin의 API 토큰
struct Remote {
    env: Env,
    server: TlsServer,
    token: String,
}

async fn remote() -> Remote {
    let env = Env::start().await;
    let server = env.serve_tls().await;
    let token = env.admin().await.token.unwrap();
    Remote { env, server, token }
}

fn body(name: &str, r: &Remote) -> Value {
    json!({
        "name": name,
        "url": r.server.url,
        "ca_fingerprint": r.env.ca_fingerprint(),
        "token": r.token,
    })
}

/// 서버가 `GET /cluster/ca`를 받은 수
fn hits(server: &TlsServer) -> usize {
    server.ca_hits.load(std::sync::atomic::Ordering::SeqCst)
}

fn stored(env: &Env, name: &str) -> RemoteSpec {
    env.cluster
        .get_by_name::<RemoteSpec>(name)
        .unwrap()
        .unwrap()
        .spec
}

/// 등록하면 원격을 확인하고 곧 상태를 모은다. 응답에는 토큰 대신 토큰의 id만 있다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remotes_are_checked_registered_and_read_without_the_token() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    // 끝 `/`와 대문자 지문은 정규화한다.
    let mut req = body("dr", &r);
    req["url"] = json!(format!("{}/", r.server.url));
    req["ca_fingerprint"] = json!(r.env.ca_fingerprint().to_uppercase());
    let (status, created) = admin.post("/remotes", req).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "dr");
    assert_eq!(created["url"], r.server.url);
    assert_eq!(created["ca_fingerprint"], r.env.ca_fingerprint());
    assert_eq!(created["token_id"], token_id(&r.token).unwrap().to_string());
    let st = &created["status"];
    assert_eq!(st["reachable"], true, "{created}");
    assert_eq!(st["error"], Value::Null, "{created}");
    assert_eq!(st["cluster_name"], "test");
    assert_eq!(st["version"], BUILD);
    assert_eq!(st["nodes"], json!({ "total": 1, "online": 1 }));
    assert_eq!(st["vms"], json!({ "total": 0, "running": 0 }));
    assert_eq!(st["cpus"], 4);
    assert_eq!(st["memory_gib"], 8);
    assert!(st["checked_at"].as_u64().is_some(), "{created}");
    assert_eq!(stored(&local, "dr").token, r.token);
    // 확인한 CA를 저장하고, 상태를 모을 때는 다시 받지 않는다.
    assert_eq!(stored(&local, "dr").ca_pem, r.env.ca_pem());
    assert_eq!(hits(&r.server), 1);

    let (status, list) = admin.get("/remotes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    let (status, got) = admin.get("/remotes/dr").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["revision"], created["revision"]);
    let (_, by_id) = admin
        .get(&format!("/remotes/{}", created["id"].as_str().unwrap()))
        .await;
    assert_eq!(by_id["name"], "dr");
    let secret = r.token.rsplit('_').next().unwrap();
    for shown in [&created, &list, &got] {
        assert!(shown.get("token").is_none(), "{shown}");
        assert!(!shown.to_string().contains(secret), "{shown}");
        assert!(!shown.to_string().contains("BEGIN CERTIFICATE"), "{shown}");
    }

    let (status, _) = admin.delete("/remotes/dr").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_error(
        &admin.get("/remotes/dr").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// 확인 순서: 검증 400 → CA를 받지 못하면 502 → 지문 다름 400 → 자기 자신 400 → 토큰 거절 400.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remotes_that_fail_a_check_are_not_kept() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;

    let mut bad_url = body("dr", &r);
    bad_url["url"] = json!(r.server.url.replace("https", "http"));
    let mut no_token = body("dr", &r);
    no_token.as_object_mut().unwrap().remove("token");
    let mut bad_token = body("dr", &r);
    bad_token["token"] = json!("not-a-token");
    for (req, why) in [
        (bad_url, "url"),
        (no_token, "token is required"),
        (bad_token, "token"),
        (body("probe", &r), "probe"),
        (body("0192f0c1-5d2e-7000-8000-000000000000", &r), "id"),
    ] {
        let got = admin.post("/remotes", req).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert!(
            got.1["message"].as_str().unwrap().contains(why),
            "{why}: {}",
            got.1
        );
    }

    // 닫힌 포트
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("https://{}", l.local_addr().unwrap())
    };
    let mut off = body("dr", &r);
    off["url"] = json!(closed);
    assert_error(
        &admin.post("/remotes", off).await,
        StatusCode::BAD_GATEWAY,
        "remote_unreachable",
    );

    let mut other_ca = body("dr", &r);
    other_ca["ca_fingerprint"] = json!(local.ca_fingerprint());
    let got = admin.post("/remotes", other_ca).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "remote_fingerprint_mismatch");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains(&r.env.ca_fingerprint()),
        "{}",
        got.1
    );

    // 이 클러스터 자신
    let me = local.serve_tls().await;
    let itself = json!({
        "name": "me",
        "url": me.url,
        "ca_fingerprint": local.ca_fingerprint(),
        "token": admin.token.clone().unwrap(),
    });
    assert_error(
        &admin.post("/remotes", itself).await,
        StatusCode::BAD_REQUEST,
        "remote_is_local",
    );

    // 모양은 맞지만 원격에 없는 토큰
    let mut unknown = body("dr", &r);
    unknown["token"] = json!("opl_0192f0c15d2e70008000000000000000_0123456789abcdef");
    assert_error(
        &admin.post("/remotes", unknown).await,
        StatusCode::BAD_REQUEST,
        "remote_unauthorized",
    );

    assert_eq!(admin.get("/remotes").await.1["items"], json!([]));
}

/// PUT은 통째로 바꾼다. token을 빼면 url·ca_fingerprint가 그대로일 때만 저장된 토큰을 둔다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_token_is_kept_only_while_the_url_and_ca_stay() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (status, created) = admin.post("/remotes", body("dr", &r)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let revision = created["revision"].as_u64().unwrap();

    // 같은 값(정규화한 모양만 다름)이면 쓰지 않는다.
    let same = json!({
        "url": format!("{}/", r.server.url),
        "ca_fingerprint": r.env.ca_fingerprint().to_uppercase(),
    });
    let (status, got) = admin.put("/remotes/dr", same, Some(revision)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["revision"], revision);
    assert_eq!(stored(&local, "dr").token, r.token);
    assert_eq!(hits(&r.server), 1);

    // 다른 주소(같은 원격의 다른 서버)로는 토큰을 다시 줘야 한다.
    let second = r.env.serve_tls().await;
    let moved = json!({ "url": second.url, "ca_fingerprint": r.env.ca_fingerprint() });
    let got = admin.put("/remotes/dr", moved.clone(), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("token must be given again"),
        "{}",
        got.1
    );
    let other = r.env.admin().await.token.unwrap();
    let mut with_token = moved.clone();
    with_token["token"] = json!(other);
    with_token["name"] = json!("dr");
    let (status, got) = admin.put("/remotes/dr", with_token, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["url"], second.url);
    assert_eq!(got["token_id"], token_id(&other).unwrap().to_string());
    assert_eq!(got["status"]["reachable"], true, "{got}");
    assert_eq!(stored(&local, "dr").token, other);
    // 주소가 바뀌면 CA를 다시 받아 확인한다.
    assert_eq!(hits(&second), 1);
    assert_eq!(stored(&local, "dr").ca_pem, r.env.ca_pem());

    // 토큰만 바뀌면 저장한 CA로 토큰만 확인한다.
    let mut token_only = moved.clone();
    token_only["token"] = json!(r.token);
    let (status, got) = admin.put("/remotes/dr", token_only, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["token_id"], token_id(&r.token).unwrap().to_string());
    assert_eq!(hits(&second), 1);

    // 바뀐 값은 다시 확인한다.
    let mut wrong = moved.clone();
    wrong["token"] = json!("opl_0192f0c15d2e70008000000000000000_0123456789abcdef");
    assert_error(
        &admin.put("/remotes/dr", wrong, None).await,
        StatusCode::BAD_REQUEST,
        "remote_unauthorized",
    );
    let mut renamed = moved.clone();
    renamed["name"] = json!("other");
    assert_error(
        &admin.put("/remotes/dr", renamed, None).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    let mut back = body("dr", &r);
    back.as_object_mut().unwrap().remove("name");
    assert_error(
        &admin.put("/remotes/dr", back, Some(revision)).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    assert_eq!(stored(&local, "dr").url, second.url);
}

/// 누구나 읽고 admin만 바꾼다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn everyone_reads_remotes_and_only_admins_change_them() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    assert_eq!(
        admin.post("/remotes", body("dr", &r)).await.0,
        StatusCode::CREATED
    );
    let mut update = body("dr", &r);
    update.as_object_mut().unwrap().remove("name");
    for role in ["operator", "viewer"] {
        let client = local.user(role, role).await;
        assert_eq!(client.get("/remotes").await.0, StatusCode::OK);
        let (status, got) = client.get("/remotes/dr").await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert!(got.get("token").is_none(), "{got}");
        for got in [
            client
                .post("/remotes", body(&format!("by-{role}"), &r))
                .await,
            client.put("/remotes/dr", update.clone(), None).await,
            client
                .post("/remotes/dr/actions/refresh", Value::Null)
                .await,
            client
                .post("/remotes/probe", json!({ "url": r.server.url }))
                .await,
            client.delete("/remotes/dr").await,
        ] {
            assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
        }
    }
    assert_error(
        &local.anonymous().get("/remotes").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    assert_eq!(
        admin.get("/remotes").await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// 옛 노드가 남아 있으면 만들지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remotes_wait_until_every_node_runs_this_build() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    local.cluster.create(&node).await.unwrap();
    assert_error(
        &admin.post("/remotes", body("dr", &r)).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    assert_eq!(admin.get("/remotes").await.1["items"], json!([]));
}

/// probe는 인증서를 검증하지 않고 CA를 받아 지문을 돌려준다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probes_show_the_remote_ca_fingerprint() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (status, got) = admin
        .post(
            "/remotes/probe",
            json!({ "url": format!("  {}/", r.server.url) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!({ "url": r.server.url, "ca_fingerprint": r.env.ca_fingerprint() })
    );
    assert_error(
        &admin
            .post("/remotes/probe", json!({ "url": "https://h/path" }))
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    let url = r.server.url.clone();
    drop(r);
    assert_error(
        &admin.post("/remotes/probe", json!({ "url": url })).await,
        StatusCode::BAD_GATEWAY,
        "remote_unreachable",
    );
}

/// `ca`가 `node-x`로 서명한 인증서로 `app`을 HTTPS 서비스한다. 주소와 서버 작업.
async fn serve_app(
    ca: &opl_peer::pki::ClusterCa,
    app: axum::Router,
) -> (String, tokio::task::JoinHandle<()>) {
    let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
    let tls = opl_peer::pki::NodeTls {
        cert_pem: ca
            .sign_node(&csr, uuid::Uuid::now_v7(), &["node-x".into()])
            .unwrap(),
        ca_pem: ca.cert_pem.clone(),
        key_pem,
    };
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", tcp.local_addr().unwrap());
    let config = opl_peer::tls::api_server_config(&tls).unwrap();
    let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, task)
}

/// `GET /cluster/ca`의 본문을 1초에 한 바이트씩 끝없이 보내는 HTTPS 서버(조각 사이 제한에는 걸리지 않음)
async fn trickling_server() -> (String, tokio::task::JoinHandle<()>) {
    let ca = opl_peer::pki::ClusterCa::generate("slow").unwrap();
    let app = axum::Router::new().route(
        "/api/v1/cluster/ca",
        axum::routing::get(|| async {
            let bytes = futures_util::stream::unfold((), |()| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Some((Ok::<_, std::convert::Infallible>(vec![b'-']), ()))
            });
            axum::body::Body::from_stream(bytes)
        }),
    );
    serve_app(&ca, app).await
}

/// 등록할 때의 확인과 probe는 모두 30초 안에 끝나야 한다(넘으면 502).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checking_a_slow_remote_times_out() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (url, server) = trickling_server().await;
    let mut req = body("slow", &r);
    req["url"] = json!(url);
    let timed = |path: &'static str, req: Value| {
        let admin = &admin;
        async move {
            let start = Instant::now();
            // 시간 제한이 없으면 끝나지 않는다.
            let got = tokio::time::timeout(Duration::from_secs(60), admin.post(path, req))
                .await
                .unwrap_or_else(|_| panic!("{path} did not time out"));
            (got, start.elapsed())
        }
    };
    let (created, probed) = tokio::join!(
        timed("/remotes", req),
        timed("/remotes/probe", json!({ "url": url })),
    );
    server.abort();
    for (got, elapsed) in [created, probed] {
        assert_error(&got, StatusCode::BAD_GATEWAY, "remote_unreachable");
        assert!(
            got.1["message"].as_str().unwrap().contains("timed out"),
            "{}",
            got.1
        );
        assert!(
            (Duration::from_secs(29)..Duration::from_secs(45)).contains(&elapsed),
            "{elapsed:?}"
        );
    }
    assert_eq!(admin.get("/remotes").await.1["items"], json!([]));
}

/// [`interjecting_server`]가 `GET /auth/whoami`를 받으면 이 클러스터의 원격 `dr`에 끼워 넣는 쓰기
enum Interject {
    /// 상태만 바꾼다(수집기처럼).
    Status,
    /// spec의 토큰을 바꾼다.
    Token(String),
}

/// `GET /cluster/ca`에 CA를 주고, `GET /auth/whoami`에 답하기 전에 `next`에 둔 쓰기를 `cluster`에 하는
/// HTTPS 서버(확인하는 사이에 다른 쓰기가 끼어든 것처럼). CA 지문, 주소와 서버 작업.
async fn interjecting_server(
    cluster: Arc<opl_cluster::Cluster>,
    next: Arc<Mutex<Option<Interject>>>,
) -> (String, String, tokio::task::JoinHandle<()>) {
    let ca = opl_peer::pki::ClusterCa::generate("dr").unwrap();
    let pem = ca.cert_pem.clone();
    let whoami = move || {
        let (cluster, next) = (cluster.clone(), next.clone());
        async move {
            let interject = next.lock().unwrap().take();
            if let Some(interject) = interject {
                let r = cluster.get_by_name::<RemoteSpec>("dr").unwrap().unwrap();
                match interject {
                    Interject::Status => {
                        let mut status = r.status.clone();
                        status.error = Some("interjected".into());
                        cluster
                            .update_status::<RemoteSpec>(r.id, &status)
                            .await
                            .unwrap();
                    }
                    Interject::Token(token) => {
                        let spec = RemoteSpec { token, ..r.spec };
                        cluster
                            .update_spec(r.id, None, r.labels, &spec)
                            .await
                            .unwrap();
                    }
                }
            }
            axum::Json(json!({}))
        }
    };
    let app = axum::Router::new()
        .route(
            "/api/v1/cluster/ca",
            axum::routing::get(move || {
                let pem = pem.clone();
                async move { pem }
            }),
        )
        .route("/api/v1/auth/whoami", axum::routing::get(whoami));
    let fingerprint = opl_peer::pki::fingerprint(&ca.cert_pem).unwrap();
    let (url, task) = serve_app(&ca, app).await;
    (fingerprint, url, task)
}

/// If-Match가 맞아 확인을 시작한 뒤 그 사이 상태만 바뀌었으면(수집기 따위) 새 revision에 써 200, spec이
/// 바뀌었으면 412.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_status_write_during_the_check_does_not_fail_if_match() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let next = Arc::new(Mutex::new(None));
    let (fingerprint, url, server) = interjecting_server(local.cluster.clone(), next.clone()).await;
    let token = |n: u8| format!("opl_0192f0c15d2e7000800000000000000{n}_secret");
    let (status, created) = admin
        .post(
            "/remotes",
            json!({ "name": "dr", "url": url, "ca_fingerprint": fingerprint, "token": token(1) }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let put = |n: u8| json!({ "url": url, "ca_fingerprint": fingerprint, "token": token(n) });

    let revision = admin.get("/remotes/dr").await.1["revision"]
        .as_u64()
        .unwrap();
    *next.lock().unwrap() = Some(Interject::Status);
    let (status, got) = admin.put("/remotes/dr", put(2), Some(revision)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(next.lock().unwrap().is_none(), "the check did not run");
    assert!(got["revision"].as_u64().unwrap() > revision + 1, "{got}");
    assert_eq!(stored(&local, "dr").token, token(2));

    let revision = admin.get("/remotes/dr").await.1["revision"]
        .as_u64()
        .unwrap();
    *next.lock().unwrap() = Some(Interject::Token(token(3)));
    assert_error(
        &admin.put("/remotes/dr", put(4), Some(revision)).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    assert!(next.lock().unwrap().is_none(), "the check did not run");
    assert_eq!(stored(&local, "dr").token, token(3));
    server.abort();
}

/// refresh는 곧 다시 모으고, 바뀌었을 때만 적는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_collects_now_and_writes_only_changes() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (_, created) = admin.post("/remotes", body("dr", &r)).await;
    let revision = created["revision"].as_u64().unwrap();

    let (status, same) = admin.post("/remotes/dr/actions/refresh", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same["revision"], revision, "{same}");

    let remote_admin = r.env.admin().await;
    let (status, vm) = remote_admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let (_, changed) = admin.post("/remotes/dr/actions/refresh", Value::Null).await;
    assert_eq!(changed["status"]["vms"]["total"], 1, "{changed}");
    assert!(changed["revision"].as_u64().unwrap() > revision);
    assert_eq!(
        admin.get("/remotes/dr").await.1["revision"],
        changed["revision"]
    );
}

/// 리더의 수집기는 원격 상태가 바뀔 때만 적고, 원격이 꺼지면 reachable false와 이유를 적는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_poller_records_changes_only_and_unreachable_remotes() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (status, created) = admin.post("/remotes", body("dr", &r)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let poller = RemotePoller::spawn(
        local.state.clone(),
        RemotePollerConfig {
            tick: Duration::from_millis(50),
            interval: Duration::from_millis(100),
        },
    );
    let get = || {
        local
            .cluster
            .get_by_name::<RemoteSpec>("dr")
            .unwrap()
            .unwrap()
    };

    // 원격이 바뀌면 적는다.
    let remote_admin = r.env.admin().await;
    let (status, vm) = remote_admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    until("the new VM", || {
        get().status.vms.is_some_and(|v| v.total == 1)
    })
    .await;

    // 그대로면 여러 번 모아도 쓰지 않는다.
    let revision = get().revision;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(get().revision, revision);
    assert!(get().status.reachable);

    let Remote { env, server, .. } = r;
    drop(server);
    until("an unreachable remote", || !get().status.reachable).await;
    let status = get().status;
    assert!(
        status.error.as_deref().is_some_and(|e| e.contains("reach")),
        "{status:?}"
    );
    assert_eq!(status.vms, None);
    poller.shutdown().await;
    drop(env);
}

async fn until(why: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
```

`crates/opld/src/config.rs`에서

```rust
use opl_api::{
    AuditConfig, BackupJobSchedulerConfig, ClusterJanitorConfig, SnapshotSchedulerConfig,
    TelemetryConfig, UpdatesConfig,
};
```

을 다음으로 바꾼다:

```rust
use opl_api::{
    AuditConfig, BackupJobSchedulerConfig, ClusterJanitorConfig, RemotePollerConfig,
    SnapshotSchedulerConfig, TelemetryConfig, UpdatesConfig,
};
```

`crates/opld/src/config.rs`에서

```rust
    pub audit: AuditSection,
}
```

을 다음으로 바꾼다:

```rust
    pub audit: AuditSection,
    #[serde(default)]
    pub remotes: RemotesSection,
}
```

`crates/opld/src/config.rs`에서

```rust
            ..TelemetryConfig::default()
        })
    }
}

/// 감사 로그(`{data_dir}/audit/audit.log`). API를 여는 노드만 쓴다.
```

을 다음으로 바꾼다:

```rust
            ..TelemetryConfig::default()
        })
    }
}

/// 원격 클러스터 상태 수집(리더). 원격은 API(`/remotes`)로 등록한다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotesSection {
    /// 원격마다 상태를 모으는 간격
    #[serde(
        default = "default_remotes_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub interval: Duration,
    /// 모을 때가 됐는지 확인하는 간격
    #[serde(
        default = "default_remotes_tick",
        deserialize_with = "deserialize_duration"
    )]
    pub tick: Duration,
}

impl Default for RemotesSection {
    fn default() -> Self {
        Self {
            interval: default_remotes_interval(),
            tick: default_remotes_tick(),
        }
    }
}

impl RemotesSection {
    pub fn poller_config(&self) -> RemotePollerConfig {
        RemotePollerConfig {
            tick: self.tick,
            interval: self.interval,
        }
    }
}

fn default_remotes_interval() -> Duration {
    Duration::from_secs(30)
}
fn default_remotes_tick() -> Duration {
    Duration::from_secs(5)
}

/// 감사 로그(`{data_dir}/audit/audit.log`). API를 여는 노드만 쓴다.
```

`crates/opld/src/config.rs`에서

```rust
    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn remotes_section_defaults_and_overrides() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert_eq!(c.remotes, RemotesSection::default());
        let p = c.remotes.poller_config();
        assert_eq!(p.interval, Duration::from_secs(30));
        assert_eq!(p.tick, Duration::from_secs(5));
        let c = parse("name = \"n\"\naddress = \"h\"\n[remotes]\ninterval = \"2s\"\ntick = \"1s\"");
        let p = c.remotes.poller_config();
        assert_eq!(p.interval, Duration::from_secs(2));
        assert_eq!(p.tick, Duration::from_secs(1));
        for bad in ["interval = \"0s\"", "tick = \"x\"", "url = \"https://h\""] {
            assert!(
                toml::from_str::<Config>(&format!(
                    "name = \"n\"\naddress = \"h\"\n[remotes]\n{bad}"
                ))
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
```

`crates/opld/src/member.rs`에서

```rust
    AppState, AuditLog, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig,
    IncomingConfig, IncomingMigrations, SnapshotScheduler, TaskRunner, TaskRunnerConfig,
    TelemetryReporter, UefiVarsFetcher, UpdateAgent, Updates,
};
```

을 다음으로 바꾼다:

```rust
    AppState, AuditLog, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig,
    IncomingConfig, IncomingMigrations, RemotePoller, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig, TelemetryReporter, UefiVarsFetcher, UpdateAgent, Updates,
};
```

`crates/opld/src/member.rs`에서

```rust
    telemetry: TelemetryReporter,
    /// API가 멈춘 뒤 남은 감사 항목을 쓰고 닫는다.
```

을 다음으로 바꾼다:

```rust
    telemetry: TelemetryReporter,
    /// 리더가 원격 클러스터의 상태를 모은다.
    remotes: RemotePoller,
    /// API가 멈춘 뒤 남은 감사 항목을 쓰고 닫는다.
```

`crates/opld/src/member.rs`에서

```rust
        self.telemetry.shutdown().await;
        self.recovery.shutdown().await;
```

을 다음으로 바꾼다:

```rust
        self.telemetry.shutdown().await;
        self.remotes.shutdown().await;
        self.recovery.shutdown().await;
```

`crates/opld/src/member.rs`에서

```rust
        let telemetry = TelemetryReporter::spawn(state.clone(), telemetry_config);
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
```

을 다음으로 바꾼다:

```rust
        let telemetry = TelemetryReporter::spawn(state.clone(), telemetry_config);
        let remotes = RemotePoller::spawn(state.clone(), config.remotes.poller_config());
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
```

`crates/opld/src/member.rs`에서

```rust
                telemetry,
                audit,
```

을 다음으로 바꾼다:

```rust
                telemetry,
                remotes,
                audit,
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1451개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(api): 원격 클러스터 API와 상태 수집 — remotes.rs: GET·POST /remotes, GET·PUT(If-Match)·DELETE /remotes/{key}(읽기는 Remote 읽기 — viewer 이상, 쓰기는 admin, 쓰기 전 require_current), POST /remotes/probe {url}(admin — 인증서를 검증하지 않고 CA를 받아 정규화한 url과 ca_fingerprint를 돌려줌, 받지 못하면 502 remote_unreachable), POST /remotes/{key}/actions/refresh(admin — 곧 다시 모아 돌려줌). 만들기·바꾸기 확인 순서: 검증 400(url·지문 정규화, token 모양, 이름 규칙, 이름 probe 거절) → 옛 노드 409 → fetch_ca 실패 502 remote_unreachable → 지문 다름 400 remote_fingerprint_mismatch → 자기 클러스터 CA 400 remote_is_local → 토큰으로 GET /auth/whoami 401이면 400 remote_unauthorized(그 밖 502) → 저장하고 곧 상태를 모아 씀. PUT은 통째로 바꾸고 token을 빼면 정규화한 url·ca_fingerprint가 그대로일 때만 유지(아니면 400), 같은 spec이면 쓰지 않음. 응답 RemoteView{id, name, revision, url, ca_fingerprint, token_id, status}(token 없음), watch에 넣지 않음. AppState.remotes(CaCache). RemotePoller(리더만, require_current·read_barrier 뒤, tick마다 interval이 지난 원격을 동시에 각 10초 제한으로): GET /cluster·/nodes·/vms(필요한 필드만 읽음)로 cluster_name, version(위트니스 포함 노드 가운데 가장 낮은 것), nodes total/online·cpus·memory_gib(위트니스 제외), vms total/running(phase running). 실패면 reachable false와 오류만(나머지는 비움). 저장된 것과 같으면 쓰지 않고 다르면 checked_at을 지금으로 update_status_checked(그 사이 바뀌었으면 건너뜀). opld [remotes] interval(기본 30s)·tick(기본 5s), Services에 RemotePoller. schema.d.ts 다시 만듦. 시험: 공통 Env::serve_tls(클러스터 CA가 node-x로 서명한 잎으로 같은 라우터를 HTTPS로)·ca_fingerprint, tests/remotes.rs(등록 성공과 상태·정규화·token 없음, 확인 실패 코드들과 저장 안 됨, PUT token 규칙·다시 확인·이름·If-Match, viewer·operator 읽기 200·쓰기 403, 옛 노드 409, probe, refresh가 바뀔 때만 씀, 수집기가 바뀔 때만 쓰고 원격이 꺼지면 reachable false), 단위(요약 계산, 모르는 필드 무시, token 유지 규칙, Debug 가림), opld 설정, openapi 경로"
```

---

### Task 3: 원격 클러스터 API 중계

`/api/v1/remotes/{key}/api/{*path}`: 저장한 토큰으로 원격의 같은 경로를 부르고 응답을 흘려 돌려준다. viewer는 VM·볼륨·노드 같은 자원의 GET만(사용자·토큰·영역·감사·원격 사슬은 로컬 admin), 쓰기는 admin. `.`·`..`·빈 조각은 400, watch·콘솔은 404. 원격의 401은 502 `remote_unauthorized`, 응답에 `nosniff`·CSP, JSON만 그대로. OpenAPI에는 넣지 않는다.

**Files:**
- Create: `crates/opl-api/src/remote_proxy.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/remote_client.rs`
- Modify: `crates/opl-api/src/remotes.rs`
- Modify: `crates/opl-api/src/vm_imports/esxi.rs`
- Modify: `crates/opl-api/tests/remote_client.rs`
- Modify: `crates/opl-api/tests/remotes.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
pub mod remote_client;
mod remotes;
```

을 다음으로 바꾼다:

```rust
pub mod remote_client;
mod remote_proxy;
mod remotes;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(remotes::refresh_remote))
        .routes(routes!(r::list_nodes))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(remotes::refresh_remote))
        // 모든 메서드를 받는 중계는 API 문서에 넣지 않는다.
        .route(
            "/remotes/{key}/api/{*path}",
            axum::routing::any(remote_proxy::proxy),
        )
        .routes(routes!(r::list_nodes))
```

`crates/opl-api/src/remote_client.rs`에서

```rust

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
```

을 다음으로 바꾼다:

```rust

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
```

`crates/opl-api/src/remote_client.rs`에서

```rust
use hyper_util::rt::TokioIo;
use opl_types::remote::{Remote, normalize_url};
use rustls::client::WebPkiServerVerifier;
```

을 다음으로 바꾼다:

```rust
use hyper_util::rt::TokioIo;
use opl_types::remote::normalize_url;
use rustls::client::WebPkiServerVerifier;
```

`crates/opl-api/src/remote_client.rs`에서

```rust
use tokio_rustls::client::TlsStream;
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use tokio_rustls::client::TlsStream;
```

`crates/opl-api/src/remote_client.rs`에서

```rust
pub type RemoteBody = IdleTimeout<Incoming>;

/// 원격마다 받은 CA PEM을 노드 메모리에 둔다. 원격의 revision이 바뀌면 다시 받는다. 지문이 맞는
/// 것만 둔다.
#[derive(Default)]
pub struct CaCache {
    inner: Mutex<HashMap<Uuid, (u64, String)>>,
}

impl CaCache {
    /// 원격의 CA PEM. 없으면 [`fetch_ca`]로 받고 저장한 지문과 비교한다(다르면 [`RemoteError::Tls`]).
    pub async fn ca_pem(&self, remote: &Remote) -> Result<String, RemoteError> {
        if let Some(pem) = self.cached(remote) {
            return Ok(pem);
        }
        let pem = fetch_ca(&remote.spec.url).await?;
        let actual = opl_peer::pki::fingerprint(&pem).map_err(invalid)?;
        if actual != remote.spec.ca_fingerprint {
            return Err(RemoteError::Tls(format!(
                "the remote CA fingerprint is {actual}, expected {}",
                remote.spec.ca_fingerprint
            )));
        }
        self.lock()
            .insert(remote.id, (remote.revision, pem.clone()));
        Ok(pem)
    }

    /// 확인한 CA로 만든 원격의 클라이언트
    pub async fn client(&self, remote: &Remote) -> Result<RemoteClient, RemoteError> {
        let pem = self.ca_pem(remote).await?;
        RemoteClient::new(&remote.spec.url, &pem, &remote.spec.token)
    }

    fn cached(&self, remote: &Remote) -> Option<String> {
        match self.lock().get(&remote.id) {
            Some((revision, pem)) if *revision == remote.revision => Some(pem.clone()),
            _ => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, (u64, String)>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}
```

을 다음으로 바꾼다:

```rust
pub type RemoteBody = IdleTimeout<Incoming>;
```

`crates/opl-api/src/remote_proxy.rs`를 만든다:

```rust
//! 원격 클러스터 API 중계 `/api/v1/remotes/{key}/api/{*path}`. 그 원격의 `/api/v1/{path}`로 메서드·
//! 쿼리·본문과 몇 헤더만 저장한 토큰으로 보내고, 상태·본문과 몇 헤더를 흘려 돌려준다. 허용 목록의 자원을
//! GET·HEAD로 읽는 것은 원격을 읽을 수 있는 누구나, 그 밖은 admin만(원격 쪽 권한은 토큰 주인의 역할이
//! 다시 정한다). watch와 콘솔(WebSocket)은 중계하지 않는다.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{
    ACCEPT, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG,
    HeaderName, HeaderValue, IF_MATCH, RETRY_AFTER, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use opl_types::remote::RemoteSpec;
use opl_types::resource::ResourceSpec;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::error::ApiError;
use crate::remote_client::RemoteClient;
use crate::remotes::remote_unreachable;
use crate::resources::lookup;
use crate::state::AppState;
use crate::vm_imports::esxi::percent_decode;

/// 원격으로 보내는 요청 헤더(`Authorization`·`Host`는 클라이언트가 채운다 — 쿠키·`Origin` 등은 보내지
/// 않는다)
const REQUEST_HEADERS: [HeaderName; 4] = [CONTENT_TYPE, CONTENT_LENGTH, IF_MATCH, ACCEPT];
/// 돌려주는 응답 헤더(`Content-Type`은 JSON일 때만 그대로)
const RESPONSE_HEADERS: [HeaderName; 4] = [CONTENT_LENGTH, ETAG, RETRY_AFTER, CONTENT_DISPOSITION];
/// admin이 아니어도 GET·HEAD로 읽을 수 있는 첫 경로 조각(그 밖에는 `auth/whoami`만). 사용자·토큰·영역·
/// 감사·텔레메트리와 원격의 원격(중계 사슬)은 원격 토큰이 읽을 수 있어도 이 클러스터의 admin만 읽는다.
const READABLE: [&str; 15] = [
    "vms",
    "volumes",
    "storage-pools",
    "networks",
    "security-groups",
    "snapshots",
    "backups",
    "backup-jobs",
    "vm-imports",
    "nodes",
    "tasks",
    "cluster",
    "version",
    "settings",
    "subscription",
];

/// 원격 API 중계. 경로에 `.`·`..`·빈 조각이 있으면(퍼센트 인코딩 포함) 400, watch·콘솔은 404
/// `remote_path_unsupported`, 원격에 닿지 못하면 502 `remote_unreachable`, 원격이 저장한 토큰을
/// 거절하면(401) 502 `remote_unauthorized`.
pub async fn proxy(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    note: AuditNote,
    Path((key, _)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    let (parts, body) = request.into_parts();
    let (method, uri) = (parts.method, parts.uri);
    let path = remote_path(uri.path());
    let segments = segments(path);
    let action = match method {
        Method::GET | Method::HEAD if readable(&segments) => Action::Read,
        _ => Action::Write,
    };
    state.authorize(&principal, action, RemoteSpec::KIND)?;
    check_path(&segments)?;
    let remote = lookup::<RemoteSpec>(&state.cluster, &key)?;
    note.detail(format!("remote={} {method} /{path}", remote.name));
    let path_and_query = match uri.query() {
        Some(query) => format!("/{path}?{query}"),
        None => format!("/{path}"),
    };
    // 등록할 때 확인해 저장한 CA만 믿는다(연결할 때 CA를 다시 받지 않는다).
    let client = RemoteClient::new(&remote.spec.url, &remote.spec.ca_pem, &remote.spec.token)
        .map_err(remote_unreachable)?;
    let response = client
        .request(
            method,
            &path_and_query,
            copy(&parts.headers, &REQUEST_HEADERS),
            body,
        )
        .await
        .map_err(remote_unreachable)?;
    let (parts, body) = response.into_parts();
    // 원격의 401은 이 클러스터의 로그인 문제가 아니다(본문은 버린다).
    if parts.status == StatusCode::UNAUTHORIZED {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "remote_unauthorized",
            "the remote cluster refused the stored token; update it with `oplctl remote set --token-file`",
        ));
    }
    let mut out = Response::new(Body::new(body));
    *out.status_mut() = parts.status;
    *out.headers_mut() = response_headers(&parts.headers);
    Ok(out)
}

/// 돌려줄 응답 헤더. 원격의 본문을 브라우저가 이 클러스터의 페이지로 실행하지 못하게 JSON이 아니면
/// `application/octet-stream`으로 하고, 늘 `nosniff`와 아무것도 허락하지 않는 CSP를 붙인다.
fn response_headers(from: &HeaderMap) -> HeaderMap {
    let mut to = copy(from, &RESPONSE_HEADERS);
    let json = from
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    let content_type = match from.get(CONTENT_TYPE) {
        Some(v) if json => v.clone(),
        _ => HeaderValue::from_static("application/octet-stream"),
    };
    to.insert(CONTENT_TYPE, content_type);
    to.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    to.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    to
}

/// 요청 경로에서 `/remotes/{key}/api/` 뒤(퍼센트 인코딩 그대로)
fn remote_path(path: &str) -> &str {
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    path.splitn(5, '/').nth(4).unwrap_or_default()
}

/// 디코딩한 경로 조각
fn segments(path: &str) -> Vec<String> {
    path.split('/').map(percent_decode).collect()
}

/// admin이 아니어도 읽을 수 있는 경로인지([`READABLE`]와 `auth/whoami`)
fn readable(segments: &[String]) -> bool {
    match segments {
        [auth, whoami] if auth == "auth" && whoami == "whoami" => true,
        [first, ..] => READABLE.contains(&first.as_str()),
        [] => false,
    }
}

fn check_path(segments: &[String]) -> Result<(), ApiError> {
    if segments
        .iter()
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(ApiError::bad_request(
            "the remote path must not have empty, \".\" or \"..\" segments",
        ));
    }
    let unsupported = match segments {
        [first, ..] if first == "watch" => true,
        [vms, _, console] => vms == "vms" && console == "console",
        _ => false,
    };
    if unsupported {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "remote_path_unsupported",
            "watch and console are not proxied to remote clusters",
        ));
    }
    Ok(())
}

/// `names`의 헤더만 옮긴다.
fn copy(from: &HeaderMap, names: &[HeaderName]) -> HeaderMap {
    let mut to = HeaderMap::new();
    for name in names {
        for value in from.get_all(name) {
            to.append(name.clone(), value.clone());
        }
    }
    to
}

#[cfg(test)]
mod tests {
    use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN, SET_COOKIE};

    use super::*;

    #[test]
    fn only_listed_headers_cross() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            (AUTHORIZATION, "Bearer opl_local"),
            (COOKIE, "opl_session=s3cret"),
            (ORIGIN, "https://here"),
            (HOST, "here"),
            (CONTENT_TYPE, "application/json"),
            (CONTENT_LENGTH, "2"),
            (IF_MATCH, "7"),
            (ACCEPT, "application/json"),
            (SET_COOKIE, "x=y"),
            (ETAG, "\"1\""),
        ] {
            headers.insert(name, HeaderValue::from_static(value));
        }
        let sent = copy(&headers, &REQUEST_HEADERS);
        let mut names: Vec<&str> = sent.keys().map(HeaderName::as_str).collect();
        names.sort();
        assert_eq!(
            names,
            ["accept", "content-length", "content-type", "if-match"]
        );
        let returned = response_headers(&headers);
        let mut names: Vec<&str> = returned.keys().map(HeaderName::as_str).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "content-length",
                "content-security-policy",
                "content-type",
                "etag",
                "x-content-type-options"
            ]
        );
    }

    /// JSON만 그 Content-Type 그대로, 그 밖(없음 포함)은 octet-stream
    #[test]
    fn only_json_keeps_its_content_type() {
        for (given, sent) in [
            (Some("application/json"), "application/json"),
            (
                Some("Application/JSON; charset=utf-8"),
                "Application/JSON; charset=utf-8",
            ),
            (Some("text/html"), "application/octet-stream"),
            (Some("application/json+x"), "application/octet-stream"),
            (Some("image/svg+xml"), "application/octet-stream"),
            (None, "application/octet-stream"),
        ] {
            let mut headers = HeaderMap::new();
            if let Some(given) = given {
                headers.insert(CONTENT_TYPE, HeaderValue::from_static(given));
            }
            let out = response_headers(&headers);
            assert_eq!(out[CONTENT_TYPE], sent, "{given:?}");
            assert_eq!(out[X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert_eq!(out[CONTENT_SECURITY_POLICY], "default-src 'none'; sandbox");
        }
    }

    #[test]
    fn non_admins_read_only_listed_paths() {
        for ok in [
            "vms",
            "vms/web",
            "storage-pools/p/volumes",
            "tasks",
            "auth/whoami",
            "%76ms",
            "subscription",
        ] {
            assert!(readable(&segments(ok)), "{ok}");
        }
        for admin in [
            "users",
            "tokens/x",
            "realms",
            "audit",
            "telemetry",
            "remotes",
            "remotes/dr/api/vms",
            "auth",
            "auth/sessions",
            "auth/whoami/x",
            "%75sers",
            "watch",
            "",
            "unknown",
        ] {
            assert!(!readable(&segments(admin)), "{admin}");
        }
    }

    #[test]
    fn paths_follow_the_api_prefix() {
        for uri in ["/api/v1/remotes/dr/api/vms/web", "/remotes/dr/api/vms/web"] {
            assert_eq!(remote_path(uri), "vms/web", "{uri}");
        }
        assert_eq!(remote_path("/remotes/dr/api/a%2Fb"), "a%2Fb");
    }

    #[test]
    fn dot_empty_watch_and_console_paths_are_refused() {
        for ok in [
            "vms",
            "vms/web",
            "vms/a.b/actions/start",
            "vms/web/console/x",
            "nodes/watch",
        ] {
            assert!(check_path(&segments(ok)).is_ok(), "{ok}");
        }
        for bad in ["vms/..", "%2e%2E/vms", ".", "vms//web", "vms/", ""] {
            assert_eq!(
                check_path(&segments(bad)).unwrap_err().code,
                "invalid_request",
                "{bad}"
            );
        }
        for unsupported in [
            "watch",
            "%77atch",
            "watch/x",
            "vms/web/console",
            "vms/%77eb/console",
        ] {
            assert_eq!(
                check_path(&segments(unsupported)).unwrap_err().code,
                "remote_path_unsupported",
                "{unsupported}"
            );
        }
    }
}
```

`crates/opl-api/src/remotes.rs`에서

```rust

fn remote_unreachable(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "remote_unreachable", e.to_string())
```

을 다음으로 바꾼다:

```rust

pub(crate) fn remote_unreachable(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "remote_unreachable", e.to_string())
```

`crates/opl-api/src/vm_imports/esxi.rs`에서

```rust
/// `%XX`를 푼다(대소문자 무시). 잘못된 것은 그대로 두고, UTF-8이 아니면 손실 변환한다.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
```

을 다음으로 바꾼다:

```rust
/// `%XX`를 푼다(대소문자 무시). 잘못된 것은 그대로 두고, UTF-8이 아니면 손실 변환한다.
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
```

`crates/opl-api/tests/remote_client.rs`에서

```rust
//! 원격 클러스터 클라이언트: CA 하나만 믿고 이름은 보지 않는 TLS, 검증 없는 CA 받기, 본문 한도,
//! 리다이렉트 안 따름, 시간 제한, 흘려 보내기, CA 캐시. 시험 CA가 `node-x` 이름으로 서명한 인증서를
//! 내는 127.0.0.1 HTTPS 서버를 상대로 한다.
```

을 다음으로 바꾼다:

```rust
//! 원격 클러스터 클라이언트: CA 하나만 믿고 이름은 보지 않는 TLS, 검증 없는 CA 받기, 본문 한도,
//! 리다이렉트 안 따름, 시간 제한, 흘려 보내기. 시험 CA가 `node-x` 이름으로 서명한 인증서를
//! 내는 127.0.0.1 HTTPS 서버를 상대로 한다.
```

`crates/opl-api/tests/remote_client.rs`에서

```rust
use hyper::body::{Bytes, Frame};
use opl_api::remote_client::{CaCache, RemoteClient, RemoteError, fetch_ca};
use opl_peer::pki::{ClusterCa, NodeTls, fingerprint, node_key_and_csr};
use opl_types::remote::{Remote, RemoteSpec};
use serde_json::{Value, json};
```

을 다음으로 바꾼다:

```rust
use hyper::body::{Bytes, Frame};
use opl_api::remote_client::{RemoteClient, RemoteError, fetch_ca};
use opl_peer::pki::{ClusterCa, NodeTls, fingerprint, node_key_and_csr};
use serde_json::{Value, json};
```

`crates/opl-api/tests/remote_client.rs`에서

```rust
    ca_status: Arc<AtomicU16>,
    ca_hits: Arc<AtomicUsize>,
    info_hits: Arc<AtomicUsize>,
```

을 다음으로 바꾼다:

```rust
    ca_status: Arc<AtomicU16>,
    info_hits: Arc<AtomicUsize>,
```

`crates/opl-api/tests/remote_client.rs`에서

```rust
async fn ca_pem(State(site): State<Site>) -> Response {
    site.ca_hits.fetch_add(1, Ordering::SeqCst);
    let status = match site.ca_status.load(Ordering::SeqCst) {
```

을 다음으로 바꾼다:

```rust
async fn ca_pem(State(site): State<Site>) -> Response {
    let status = match site.ca_status.load(Ordering::SeqCst) {
```

`crates/opl-api/tests/remote_client.rs`에서

```rust
}

fn remote(server: &Server, ca_fingerprint: String) -> Remote {
    let mut r = Remote::new(
        "dr",
        RemoteSpec {
            url: server.url.clone(),
            ca_fingerprint,
            token: TOKEN.into(),
            ca_pem: server.ca_pem.clone(),
        },
    );
    r.revision = 7;
    r
}

#[tokio::test]
async fn ca_cache_keeps_a_matching_ca_per_revision() {
    let server = start().await;
    let cache = CaCache::default();
    let mut r = remote(&server, fingerprint(&server.ca_pem).unwrap());

    for _ in 0..2 {
        let client = cache.client(&r).await.unwrap();
        let _: Value = client.get_json("/info").await.unwrap();
    }
    assert_eq!(server.site.ca_hits.load(Ordering::SeqCst), 1);

    // revision이 바뀌면 다시 받는다.
    r.revision = 8;
    assert_eq!(cache.ca_pem(&r).await.unwrap(), server.ca_pem);
    assert_eq!(server.site.ca_hits.load(Ordering::SeqCst), 2);

    // 지문이 다르면 쓰지 않고 두지도 않는다.
    let other = ClusterCa::generate("other").unwrap();
    let mut wrong = remote(&server, fingerprint(&other.cert_pem).unwrap());
    wrong.id = uuid::Uuid::now_v7();
    for _ in 0..2 {
        let err = cache.client(&wrong).await.unwrap_err();
        assert!(
            matches!(&err, RemoteError::Tls(m) if m.contains("fingerprint")),
            "{err:?}"
        );
    }
    assert_eq!(server.site.ca_hits.load(Ordering::SeqCst), 4);
}
```

을 다음으로 바꾼다:

```rust
}
```

`crates/opl-api/tests/remotes.rs`에서

```rust
//! 토큰을 내보내지 않음과 PUT의 token 규칙, 권한, 옛 노드 409, `probe`, `refresh`, 상태 수집(바뀔 때만
//! 쓰기, 원격이 꺼지면 reachable false).
```

을 다음으로 바꾼다:

```rust
//! 토큰을 내보내지 않음과 PUT의 token 규칙, 권한, 옛 노드 409, `probe`, `refresh`, 상태 수집(바뀔 때만
//! 쓰기, 원격이 꺼지면 reachable false), 중계(`/remotes/{key}/api/{*path}` — 읽기와 VM 동작, 권한,
//! 원격 쪽 권한, 막힌 경로, 꺼진 원격 502, 감사).
```

`crates/opl-api/tests/remotes.rs`에서

```rust

use common::{Env, TlsServer, vm_request};
use opl_api::{RemotePoller, RemotePollerConfig};
use opl_types::node::{NodeSpec, NodeStatus};
```

을 다음으로 바꾼다:

```rust

use common::{Env, TlsServer, WAIT, vm_request};
use opl_api::{RemotePoller, RemotePollerConfig};
use opl_types::audit::{AuditEntry, AuditQuery, AuditTarget};
use opl_types::node::{NodeSpec, NodeStatus};
```

`crates/opl-api/tests/remotes.rs`에서

```rust
use serde_json::{Value, json};
```

을 다음으로 바꾼다:

```rust
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
```

`crates/opl-api/tests/remotes.rs`에서

```rust
    }
}
```

을 다음으로 바꾼다:

```rust
    }
}

/// 원격 `dr`(admin 토큰)을 등록한 이 클러스터와 그 admin, 원격
async fn registered() -> (Env, common::Client, Remote) {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (status, created) = admin.post("/remotes", body("dr", &r)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    (local, admin, r)
}

/// 중계는 원격의 API를 저장한 토큰으로 부르고 상태·본문·쿼리를 그대로 옮긴다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proxy_reads_and_changes_the_remote() {
    let (local, admin, r) = registered().await;
    let remote_admin = r.env.admin().await;
    let (status, vm) = remote_admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");

    let (status, vms) = admin.get("/remotes/dr/api/vms").await;
    assert_eq!(status, StatusCode::OK, "{vms}");
    let names: Vec<&str> = vms["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["web"]);
    let resp = admin
        .http
        .get(format!("{}/api/v1/remotes/dr/api/vms/web", local.base))
        .bearer_auth(admin.token.as_ref().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "application/json");
    assert!(resp.headers().get("set-cookie").is_none());

    // 쿼리와 원격의 오류가 그대로 온다.
    let got = admin
        .post(
            "/remotes/dr/api/vms/web/actions/start?mode=force",
            Value::Null,
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"].as_str().unwrap().contains("mode=force"),
        "{}",
        got.1
    );
    assert_error(
        &admin.get("/remotes/dr/api/vms/nope").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // VM 동작은 원격에서 실제로 바뀐다.
    let (status, task) = admin
        .post("/remotes/dr/api/vms/web/actions/start", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = remote_admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(
        remote_admin.get("/vms/web").await.1["spec"]["power"],
        "running"
    );

    // 원격은 저장한 토큰의 주인으로 본다.
    let (status, me) = admin.get("/remotes/dr/api/auth/whoami").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["name"], "admin");
    assert_error(
        &admin.get("/remotes/nope/api/vms").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// GET은 누구나, 그 밖은 admin만. 원격 쪽 권한은 토큰 주인의 역할이 다시 정한다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxied_changes_need_an_admin_here_and_the_token_role_there() {
    let (local, admin, r) = registered().await;
    let remote_admin = r.env.admin().await;
    assert_eq!(
        remote_admin.post("/vms", vm_request("web")).await.0,
        StatusCode::CREATED
    );
    for role in ["operator", "viewer"] {
        let client = local.user(role, role).await;
        let (status, vms) = client.get("/remotes/dr/api/vms").await;
        assert_eq!(status, StatusCode::OK, "{vms}");
        for got in [
            client
                .post("/remotes/dr/api/vms/web/actions/start", Value::Null)
                .await,
            client.delete("/remotes/dr/api/vms/web").await,
        ] {
            assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
        }
    }
    assert_eq!(
        remote_admin.get("/vms/web").await.1["spec"]["power"],
        "stopped"
    );

    // 원격의 viewer 토큰으로 등록하면 원격이 403을 돌려준다.
    let reader = r.env.user("reader", "viewer").await.token.unwrap();
    let mut req = body("dr-view", &r);
    req["token"] = json!(reader);
    assert_eq!(admin.post("/remotes", req).await.0, StatusCode::CREATED);
    let (status, me) = admin.get("/remotes/dr-view/api/auth/whoami").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["name"], "reader");
    assert_error(
        &admin
            .post("/remotes/dr-view/api/vms/web/actions/start", Value::Null)
            .await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_eq!(
        remote_admin.get("/vms/web").await.1["spec"]["power"],
        "stopped"
    );
}

/// 요청줄을 그대로 보낸다(reqwest는 `.`·`..` 조각을 정리해 버린다). 상태와 본문.
async fn raw_get(env: &Env, token: &str, path: &str) -> (u16, Value) {
    let addr = env.base.trim_start_matches("http://");
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /api/v1{path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    tcp.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    tcp.read_to_string(&mut response).await.unwrap();
    let status = response[9..12].parse().unwrap();
    let body = response.split_once("\r\n\r\n").unwrap().1;
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// `.`·`..`·빈 조각은 400, watch와 콘솔은 404 `remote_path_unsupported`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proxy_refuses_dot_empty_watch_and_console_paths() {
    let (local, admin, _r) = registered().await;
    let token = admin.token.clone().unwrap();
    for bad in [
        "vms/../nodes",
        "vms/%2e%2E/nodes",
        "./vms",
        "%2E",
        "vms//web",
        "vms/",
    ] {
        let (status, got) = raw_get(&local, &token, &format!("/remotes/dr/api/{bad}")).await;
        assert_eq!(status, 400, "{bad}: {got}");
        assert_eq!(got["code"], "invalid_request", "{bad}: {got}");
    }
    for unsupported in ["watch", "watch?kinds=vm", "%77atch", "vms/web/console"] {
        let (status, got) =
            raw_get(&local, &token, &format!("/remotes/dr/api/{unsupported}")).await;
        assert_eq!(status, 404, "{unsupported}: {got}");
        assert_eq!(
            got["code"], "remote_path_unsupported",
            "{unsupported}: {got}"
        );
    }
    // 이름에 점이 든 조각은 막지 않는다.
    let (status, got) = raw_get(&local, &token, "/remotes/dr/api/vms/a.b").await;
    assert_eq!(status, 404, "{got}");
    assert_eq!(got["code"], "not_found", "{got}");
}

/// 원격에 닿지 못하면 502 `remote_unreachable`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proxy_answers_502_when_the_remote_is_down() {
    let (_local, admin, r) = registered().await;
    let Remote { env, server, .. } = r;
    drop(server);
    for got in [
        admin.get("/remotes/dr/api/vms").await,
        admin
            .post("/remotes/dr/api/vms/web/actions/start", Value::Null)
            .await,
    ] {
        assert_error(&got, StatusCode::BAD_GATEWAY, "remote_unreachable");
    }
    drop(env);
}

/// 중계한 바꾸는 요청은 이 클러스터의 감사 로그에 원격 이름과 경로가 남는다(읽기는 남지 않는다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxied_changes_are_audited_here() {
    let (local, admin, r) = registered().await;
    let remote_admin = r.env.admin().await;
    assert_eq!(
        remote_admin.post("/vms", vm_request("web")).await.0,
        StatusCode::CREATED
    );
    assert_eq!(admin.get("/remotes/dr/api/vms").await.0, StatusCode::OK);
    assert_eq!(
        admin
            .post("/remotes/dr/api/vms/web/actions/start", Value::Null)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let action = "POST /remotes/{key}/api/{*path}";
    let start = Instant::now();
    let entries = loop {
        let entries: Vec<AuditEntry> = local
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
        if entries.iter().any(|e| e.action == action) {
            break entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let entry = entries.iter().find(|e| e.action == action).unwrap();
    assert_eq!(entry.status, 202);
    assert_eq!(entry.user.as_deref(), Some("admin"));
    assert_eq!(entry.path, "/remotes/dr/api/vms/web/actions/start");
    assert_eq!(
        entry.target,
        Some(AuditTarget {
            kind: "remotes".into(),
            key: "dr".into(),
        })
    );
    assert_eq!(
        entry.detail.as_deref(),
        Some("remote=dr POST /vms/web/actions/start")
    );
    assert!(
        !entries.iter().any(|e| e.action.starts_with("GET ")),
        "{entries:#?}"
    );
}

/// 이 클러스터의 admin이 아니면 중계로 읽을 수 있는 것은 허용 목록의 자원과 `auth/whoami`뿐이다(원격
/// 토큰이 읽을 수 있더라도 사용자·토큰·감사 따위와 원격의 원격은 읽지 못한다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_admins_read_only_listed_remote_resources() {
    let (local, admin, _r) = registered().await;
    let viewer = local.user("viewer", "viewer").await;
    for path in ["vms", "nodes", "cluster", "version", "auth/whoami"] {
        let (status, got) = viewer.get(&format!("/remotes/dr/api/{path}")).await;
        assert_eq!(status, StatusCode::OK, "{path}: {got}");
    }
    for path in [
        "users",
        "tokens",
        "realms",
        "audit",
        "telemetry",
        "remotes",
        "remotes/dr/api/vms",
        "auth/sessions",
        "unknown",
    ] {
        let got = viewer.get(&format!("/remotes/dr/api/{path}")).await;
        assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
    }
    // 첫 조각은 디코딩해서 본다.
    let (status, got) = raw_get(
        &local,
        viewer.token.as_ref().unwrap(),
        "/remotes/dr/api/%75sers",
    )
    .await;
    assert_eq!(status, 403, "{got}");
    // HEAD도 GET과 같다.
    for (path, status) in [("users", StatusCode::FORBIDDEN), ("vms", StatusCode::OK)] {
        let resp = viewer
            .http
            .head(format!("{}/api/v1/remotes/dr/api/{path}", local.base))
            .bearer_auth(viewer.token.as_ref().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), status, "HEAD {path}");
    }
    // admin은 원격 토큰이 허락하는 것을 모두 읽는다.
    for path in ["users", "remotes"] {
        let (status, got) = admin.get(&format!("/remotes/dr/api/{path}")).await;
        assert_eq!(status, StatusCode::OK, "{path}: {got}");
    }
}

/// 원격이 저장한 토큰을 거절하면(401) 이 클러스터의 로그인 문제로 보이지 않게 502 `remote_unauthorized`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_remote_token_is_a_502() {
    let (_local, admin, r) = registered().await;
    let remote_admin = r.env.admin().await;
    let id = token_id(&r.token).unwrap();
    let (status, got) = remote_admin.delete(&format!("/tokens/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{got}");
    for got in [
        admin.get("/remotes/dr/api/vms").await,
        admin
            .post("/remotes/dr/api/vms/web/actions/start", Value::Null)
            .await,
    ] {
        assert_error(&got, StatusCode::BAD_GATEWAY, "remote_unauthorized");
        assert!(
            got.1["message"]
                .as_str()
                .unwrap()
                .contains("oplctl remote set --token-file"),
            "{}",
            got.1
        );
    }
}

/// 중계한 응답은 브라우저가 이 클러스터의 페이지로 실행하지 못하게 한다: JSON이 아니면
/// `application/octet-stream`, 늘 `nosniff`와 막는 CSP.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxied_responses_cannot_run_in_the_browser() {
    let (local, admin, r) = registered().await;
    for (path, content_type) in [
        ("vms", "application/json"),
        ("cluster/ca", "application/octet-stream"),
        ("vms/nope", "application/json"),
    ] {
        let resp = admin
            .http
            .get(format!("{}/api/v1/remotes/dr/api/{path}", local.base))
            .bearer_auth(admin.token.as_ref().unwrap())
            .send()
            .await
            .unwrap();
        let headers = resp.headers();
        assert_eq!(headers["content-type"], content_type, "{path}");
        assert_eq!(headers["x-content-type-options"], "nosniff", "{path}");
        assert_eq!(
            headers["content-security-policy"], "default-src 'none'; sandbox",
            "{path}"
        );
    }
    let resp = admin
        .http
        .get(format!("{}/api/v1/remotes/dr/api/cluster/ca", local.base))
        .bearer_auth(admin.token.as_ref().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), r.env.ca_pem());
}

/// 중계와 상태 모으기는 등록 때 확인해 저장한 CA를 쓴다(노드 메모리에 캐시가 없어도 CA를 다시 받지
/// 않는다 — 재시작한 노드도 같다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proxy_uses_the_stored_ca() {
    let (_local, admin, r) = registered().await;
    assert_eq!(hits(&r.server), 1);
    for _ in 0..2 {
        assert_eq!(admin.get("/remotes/dr/api/vms").await.0, StatusCode::OK);
    }
    let (status, got) = admin.post("/remotes/dr/actions/refresh", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(hits(&r.server), 1);
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1464개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): 원격 클러스터 API 중계 — remote_proxy.rs: /api/v1/remotes/{key}/api/{*path}(모든 메서드, API 문서에는 넣지 않음)가 그 원격의 /api/v1/{path}로 메서드·쿼리·본문을 저장한 토큰(Authorization: Bearer)으로 보내고 상태·본문을 그대로 흘려 돌려줌. 권한은 GET·HEAD가 Remote 읽기(viewer 이상), 그 밖은 admin — 원격 쪽 권한은 토큰 주인의 역할이 다시 정함. 경로 조각에 .·..·빈 조각이 있으면(퍼센트 인코딩 포함) 400, watch로 시작하거나 vms/<x>/console이면 404 remote_path_unsupported, 원격에 닿지 못하면(CA 받기·연결 실패 포함) 502 remote_unreachable. 보내는 헤더는 Content-Type·Content-Length·If-Match·Accept만(쿠키·Origin 등은 보내지 않음, Host는 클라이언트가 원격으로), 돌려주는 헤더는 Content-Type·Content-Length·ETag·Retry-After·Content-Disposition만. 감사는 미들웨어가 바꾸는 요청을 적고(action \"<METHOD> /remotes/{key}/api/{*path}\", target remotes/<key>) 핸들러가 detail \"remote=<이름> <METHOD> /<path>\". CaCache는 revision 대신 (원격 id, url, ca_fingerprint)로 키를 잡아 상태만 바뀐 쓰기로는 CA를 다시 받지 않음. esxi percent_decode를 crate 안에서 같이 씀. 시험: tests/remotes.rs(GET /vms·VM 시작이 원격에서 실제로 바뀜·쿼리와 원격 오류 그대로·whoami가 토큰 주인, viewer·operator는 GET만, 원격 viewer 토큰이면 원격의 403, 날 요청줄로 ..·%2e%2E·빈 조각 400과 watch·%77atch·console 404, 꺼진 원격 502, 감사 항목), 단위(헤더 목록·경로 자르기·경로 규칙), CA 캐시(상태만 바뀌면 다시 받지 않고 주소가 바뀌면 다시 받음)"
```

---

### Task 4: oplctl 원격 클러스터 명령과 opld E2E

`oplctl remote list|show|add|set|remove|refresh|vms|vm <이름> start|stop [--force] <VM>`(지문을 보이고 확인, 표준 입력 토큰은 지문이나 `--yes`가 있어야). opld E2E: 클러스터 둘을 띄워 한쪽에 다른 쪽을 등록하고 상태·VM 목록·동작을 본다.

**Files:**
- Create: `crates/opld/tests/remotes.rs`
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust
    Audit(AuditArgs),
}
```

을 다음으로 바꾼다:

```rust
    Audit(AuditArgs),
    /// 원격 클러스터: 다른 opLite 클러스터를 등록해 상태를 모아 보고, 그 클러스터의 API를 이 클러스터를
    /// 거쳐 부른다(저장한 그 클러스터의 API 토큰으로).
    Remote {
        #[command(subcommand)]
        cmd: RemoteCmd,
    },
}
```

`crates/oplctl/src/cli.rs`에서

```rust
}

#[derive(Args)]
struct AuditArgs {
```

을 다음으로 바꾼다:

```rust
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// 원격 클러스터들과 마지막으로 모은 상태(이름, 클러스터, 버전, 노드 online/total, VM running/total)
    List,
    /// 원격 하나의 설정과 상태를 보인다(토큰은 id만).
    Show { name: String },
    /// 원격을 등록한다(admin). 서버가 그 클러스터에 연결해 CA 지문과 토큰을 확인한다.
    Add(RemoteAdd),
    /// 원격을 바꾼다(admin). 준 것만 바꾼다. 주소나 지문을 바꾸면 토큰도 다시 준다.
    Set(RemoteSet),
    /// 원격을 지운다(admin). 원격 클러스터의 토큰은 그대로 남는다.
    Remove { name: String },
    /// 원격 상태를 곧 다시 모아 보인다(admin).
    Refresh { name: String },
    /// 원격 클러스터의 VM 목록(이 클러스터를 거쳐 읽는다)
    Vms { name: String },
    /// 원격 클러스터의 VM을 시작하거나(start) 정지한다(stop — 로컬 `vm stop`처럼 ACPI, `--force`면
    /// 곧바로 끄기). 원격 쪽 권한은 토큰 주인의 역할이 정한다. Task를 기다리지 않는다.
    Vm {
        name: String,
        #[arg(value_enum)]
        action: RemoteVmAction,
        vm: String,
        /// stop에서 ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct RemoteAdd {
    name: String,
    /// 원격 클러스터 노드의 API 주소(`https://<호스트>[:포트]`)
    #[arg(long)]
    url: String,
    /// 원격 클러스터 CA의 지문(`sha256:...`, 원격 노드의 `oplctl cluster status`). 없으면 CA를 받아
    /// 지문을 보이고 확인을 묻는다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
    /// 원격 클러스터에서 만든 API 토큰이 든 파일(`-`면 표준 입력 — 그때는 확인을 물을 수 없으므로
    /// `--ca-fingerprint`나 `--yes`가 필요하다)
    #[arg(long)]
    token_file: PathBuf,
    /// 받은 CA 지문을 묻지 않고 믿는다.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct RemoteSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    ca_fingerprint: Option<String>,
    /// 새 API 토큰이 든 파일(`-`면 표준 입력). 주지 않으면 저장된 토큰을 그대로 둔다.
    #[arg(long)]
    token_file: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum RemoteVmAction {
    Start,
    Stop,
}

/// 원격 VM 동작이 부르는 원격 API의 동작(`/vms/<VM>/actions/<동작>`)과 보일 이름
fn remote_vm_action(
    action: RemoteVmAction,
    force: bool,
) -> anyhow::Result<(&'static str, &'static str)> {
    Ok(match (action, force) {
        (RemoteVmAction::Start, false) => ("start", "start"),
        (RemoteVmAction::Start, true) => bail!("--force is only for stop"),
        (RemoteVmAction::Stop, false) => ("stop", "stop"),
        (RemoteVmAction::Stop, true) => ("stop?mode=force", "force stop"),
    })
}

/// 토큰 파일(`-`면 표준 입력)의 토큰. 앞뒤 공백을 뗀다.
fn read_remote_token(file: &PathBuf) -> anyhow::Result<String> {
    let token = read_secret_file(file)?.trim().to_string();
    if token.is_empty() {
        bail!("the token file is empty");
    }
    Ok(token)
}

/// 받은 CA 지문을 보이고 확인을 받는다. `--yes`면 묻지 않고, 터미널이 아니면 묻지 못하므로
/// `--ca-fingerprint`로 다시 실행하게 한다.
fn confirm_fingerprint(
    actual: &str,
    yes: bool,
    terminal: bool,
    input: &mut dyn std::io::BufRead,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    writeln!(out, "the remote cluster CA fingerprint is {actual}")?;
    if yes {
        return Ok(());
    }
    if !terminal {
        bail!(
            "check the fingerprint (`oplctl cluster status` on a node of the remote cluster) and \
             run again with --ca-fingerprint {actual}"
        );
    }
    write!(out, "Is this the CA of the remote cluster? [y/N] ")?;
    out.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        bail!("cancelled");
    }
    Ok(())
}

/// `<a>/<b>` 모양의 수(`online/total` 등). 모르면 `-`.
fn remote_counts(v: &Value, part: &str) -> String {
    if v.is_null() {
        return "-".into();
    }
    format!("{}/{}", cell(&v[part]), cell(&v["total"]))
}

/// 원격의 상태 칸: 닿으면 `reachable`, 아니면 `unreachable: <이유>`
fn remote_state(status: &Value) -> String {
    if status["reachable"] == true {
        "reachable".into()
    } else {
        format!("unreachable: {}", cell(&status["error"]))
    }
}

/// `remote list`의 열
const REMOTE_HEADERS: &[&str] = &["NAME", "CLUSTER", "VERSION", "NODES", "VMS", "STATUS"];

/// `remote list`의 한 행
fn remote_row(r: &Value) -> Vec<String> {
    let s = &r["status"];
    vec![
        cell(&r["name"]),
        cell(&s["cluster_name"]),
        cell(&s["version"]),
        remote_counts(&s["nodes"], "online"),
        remote_counts(&s["vms"], "running"),
        remote_state(s),
    ]
}

/// `remote show`의 `키: 값` 줄
fn remote_lines(r: &Value) -> Vec<String> {
    let s = &r["status"];
    let checked = match s["checked_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    let memory = match s["memory_gib"].as_u64() {
        Some(gib) => format!("{gib} GiB"),
        None => "-".into(),
    };
    vec![
        format!("name: {}", cell(&r["name"])),
        format!("url: {}", cell(&r["url"])),
        format!("ca fingerprint: {}", cell(&r["ca_fingerprint"])),
        format!("token id: {}", cell(&r["token_id"])),
        format!("status: {}", remote_state(s)),
        format!("changed at: {checked}"),
        format!("cluster: {}", cell(&s["cluster_name"])),
        format!("version: {}", cell(&s["version"])),
        format!("nodes online: {}", remote_counts(&s["nodes"], "online")),
        format!("vms running: {}", remote_counts(&s["vms"], "running")),
        format!("cpus: {}", cell(&s["cpus"])),
        format!("memory: {memory}"),
    ]
}

/// `remote set`의 본문: 지금 원격에 준 것만 바꾼다. 토큰을 주지 않으면 빼서 저장된 토큰을 둔다.
fn remote_set_body(current: &Value, args: &RemoteSet, token: Option<String>) -> Value {
    let mut body = json!({
        "url": args.url.as_deref().map_or_else(|| current["url"].clone(), |u| json!(u)),
        "ca_fingerprint": args
            .ca_fingerprint
            .as_deref()
            .map_or_else(|| current["ca_fingerprint"].clone(), |f| json!(f)),
    });
    if let Some(token) = token {
        body["token"] = json!(token);
    }
    body
}

async fn remote(
    client: &Client,
    cmd: RemoteCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let show = |out: &mut (dyn Write + Send), r: &Value| -> anyhow::Result<()> {
        if json_out {
            print_json(out, r)?;
        } else {
            for line in remote_lines(r) {
                writeln!(out, "{line}")?;
            }
        }
        Ok(())
    };
    match cmd {
        RemoteCmd::List => {
            let list = client.get("/remotes").await?;
            print_list(out, json_out, &list, REMOTE_HEADERS, remote_row)?;
        }
        RemoteCmd::Show { name } => {
            show(out, &client.get(&format!("/remotes/{}", seg(&name))).await?)?
        }
        RemoteCmd::Add(args) => {
            // 표준 입력은 토큰을 읽느라 다 썼으므로 지문 확인을 물을 수 없다.
            if args.token_file.as_os_str() == "-" && args.ca_fingerprint.is_none() && !args.yes {
                bail!("reading the token from stdin needs --ca-fingerprint or --yes");
            }
            let token = read_remote_token(&args.token_file)?;
            let fingerprint = match args.ca_fingerprint {
                Some(f) => f,
                None => {
                    let actual = fingerprint(&fetch_ca(&args.url).await?)?;
                    use std::io::IsTerminal;
                    let stdin = std::io::stdin();
                    let terminal = stdin.is_terminal();
                    confirm_fingerprint(&actual, args.yes, terminal, &mut stdin.lock(), out)?;
                    actual
                }
            };
            let body = json!({
                "name": args.name,
                "url": args.url,
                "ca_fingerprint": fingerprint,
                "token": token,
            });
            let created = client.post("/remotes", &body).await?;
            writeln!(
                out,
                "remote {} added ({})",
                args.name,
                remote_state(&created["status"])
            )?;
        }
        RemoteCmd::Set(args) => {
            let token = args
                .token_file
                .as_ref()
                .map(read_remote_token)
                .transpose()?;
            let path = format!("/remotes/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&remote_set_body(&current, &args, token));
            client.send(r).await?;
            writeln!(out, "remote {} updated", args.name)?;
        }
        RemoteCmd::Remove { name } => {
            client.delete(&format!("/remotes/{}", seg(&name))).await?;
            writeln!(out, "remote {name} removed")?;
        }
        RemoteCmd::Refresh { name } => {
            let r = client
                .post(
                    &format!("/remotes/{}/actions/refresh", seg(&name)),
                    &json!({}),
                )
                .await?;
            show(out, &r)?;
        }
        RemoteCmd::Vms { name } => {
            let list = client
                .get(&format!("/remotes/{}/api/vms", seg(&name)))
                .await?;
            print_list(out, json_out, &list, VM_HEADERS, vm_row)?;
        }
        RemoteCmd::Vm {
            name,
            action,
            vm,
            force,
        } => {
            let (action, shown) = remote_vm_action(action, force)?;
            let task = client
                .send(client.request(
                    Method::POST,
                    // 동작은 이 파일이 정한 값이고 쿼리를 담으므로 인코딩하지 않는다.
                    &format!(
                        "/remotes/{}/api/vms/{}/actions/{}",
                        seg(&name),
                        seg(&vm),
                        action
                    ),
                ))
                .await?;
            writeln!(
                out,
                "vm {vm} on remote {name}: {shown} requested (task {})",
                cell(&task["id"])
            )?;
        }
    }
    Ok(())
}

#[derive(Args)]
struct AuditArgs {
```

`crates/oplctl/src/cli.rs`에서

```rust
        Cmd::Audit(args) => audit(&client, args, json_out, out, &mut std::io::stderr()).await?,
    }
```

을 다음으로 바꾼다:

```rust
        Cmd::Audit(args) => audit(&client, args, json_out, out, &mut std::io::stderr()).await?,
        Cmd::Remote { cmd } => remote(&client, cmd, json_out, out).await?,
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"],
                vm_row,
            )?;
        }
```

을 다음으로 바꾼다:

```rust
            let list = client.get("/vms").await?;
            print_list(out, json_out, &list, VM_HEADERS, vm_row)?;
        }
```

`crates/oplctl/src/cli.rs`에서

```rust
    ]
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
```

을 다음으로 바꾼다:

```rust
    ]
}

/// `vm list`의 열
const VM_HEADERS: &[&str] = &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"];

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
```

`crates/oplctl/src/cli.rs`에서

```rust
        Cli::command().debug_assert();
    }
}
```

을 다음으로 바꾼다:

```rust
        Cli::command().debug_assert();
    }

    fn remote_cmd(args: &[&str]) -> Result<RemoteCmd, clap::Error> {
        let mut all = vec!["oplctl", "remote"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Remote { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `remote add`는 주소와 토큰 파일이 필요하다. `remote vm`의 stop은 로컬 `vm stop`처럼 ACPI이고
    /// `--force`면 곧바로 끈다.
    #[test]
    fn parses_remote_commands() {
        let RemoteCmd::Add(add) = remote_cmd(&[
            "add",
            "dr",
            "--url",
            "https://10.0.0.5:8443",
            "--token-file",
            "-",
            "--yes",
        ])
        .unwrap() else {
            panic!("not add");
        };
        assert_eq!(add.url, "https://10.0.0.5:8443");
        assert_eq!(add.token_file, PathBuf::from("-"));
        assert!(add.yes && add.ca_fingerprint.is_none());
        assert!(remote_cmd(&["add", "dr", "--url", "https://h"]).is_err());
        assert!(remote_cmd(&["add", "dr", "--token-file", "t"]).is_err());
        assert!(remote_cmd(&["vm", "dr", "reboot", "web"]).is_err());
        assert!(remote_cmd(&["vm", "dr", "shutdown", "web"]).is_err());
        let actions: Vec<_> = [
            &["start"][..],
            &["stop"][..],
            &["stop", "--force"][..],
            &["start", "--force"][..],
        ]
        .iter()
        .map(|a| {
            let mut args = vec!["vm", "dr"];
            args.extend_from_slice(&a[..1]);
            args.push("web");
            args.extend_from_slice(&a[1..]);
            match remote_cmd(&args).unwrap() {
                RemoteCmd::Vm { action, force, .. } => {
                    remote_vm_action(action, force).map_err(|e| e.to_string())
                }
                _ => unreachable!(),
            }
        })
        .collect();
        assert_eq!(
            actions,
            [
                Ok(("start", "start")),
                Ok(("stop", "stop")),
                Ok(("stop?mode=force", "force stop")),
                Err("--force is only for stop".to_string()),
            ]
        );
    }

    /// 토큰을 표준 입력으로 받으면 지문 확인을 물을 수 없으므로 `--ca-fingerprint`나 `--yes`가 없으면 곧바로
    /// 실패한다(표준 입력을 읽거나 원격에 연결하지 않고).
    #[tokio::test]
    async fn remote_add_from_stdin_needs_a_fingerprint_or_yes() {
        let client = Client::new("http://127.0.0.1:1", None, None).unwrap();
        let args = [
            "add",
            "dr",
            "--url",
            "https://10.0.0.5:8443",
            "--token-file",
            "-",
        ];
        let err = remote(&client, remote_cmd(&args).unwrap(), false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "reading the token from stdin needs --ca-fingerprint or --yes"
        );
    }

    /// 원격이 저장한 토큰을 거절하면 중계가 502 `remote_unauthorized`를 주므로 그 메시지를 보이고, 이
    /// 클러스터에 다시 로그인하라고 하지 않는다.
    #[tokio::test]
    async fn a_refused_remote_token_is_not_a_login_problem() {
        let message = "the remote cluster refused the stored token; update it with `oplctl remote set --token-file`";
        let (client, _) = fake_api_status(move |_| {
            (
                502,
                json!({ "code": "remote_unauthorized", "message": message }),
            )
        })
        .await;
        let err = remote(
            &client,
            remote_cmd(&["vms", "dr"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(err, format!("{message} (remote_unauthorized)"));
        assert!(!err.contains("oplctl login"), "{err}");
    }

    /// 받은 CA 지문은 보이고, `--yes`가 아니면 터미널에서 묻는다. 터미널이 아니면 지문을 달라고 한다.
    #[test]
    fn remote_fingerprint_needs_a_confirmation() {
        let fp = "sha256:ab";
        let confirm = |yes: bool, terminal: bool, answer: &str| {
            let mut out = Vec::new();
            let r = confirm_fingerprint(fp, yes, terminal, &mut answer.as_bytes(), &mut out);
            (
                r.map_err(|e| e.to_string()),
                String::from_utf8(out).unwrap(),
            )
        };
        let shown = "the remote cluster CA fingerprint is sha256:ab\n";
        assert_eq!(confirm(true, false, ""), (Ok(()), shown.to_string()));
        let (r, out) = confirm(false, false, "y\n");
        assert_eq!(out, shown);
        assert!(
            r.unwrap_err()
                .contains("run again with --ca-fingerprint sha256:ab"),
            "not a terminal"
        );
        let prompt = format!("{shown}Is this the CA of the remote cluster? [y/N] ");
        assert_eq!(confirm(false, true, "Yes\n"), (Ok(()), prompt.clone()));
        assert_eq!(
            confirm(false, true, "\n"),
            (Err("cancelled".to_string()), prompt)
        );
    }

    fn sample_remote() -> Value {
        json!({
            "id": "0190b9d4-0000-7000-8000-000000000001",
            "name": "dr",
            "revision": 9,
            "url": "https://10.0.0.5:8443",
            "ca_fingerprint": "sha256:aa",
            "token_id": "0190b9d4-0000-7000-8000-000000000002",
            "status": {
                "reachable": true,
                "checked_at": 1_793_000_000,
                "error": null,
                "cluster_name": "dr-site",
                "version": "0.37.0",
                "nodes": { "total": 3, "online": 2 },
                "vms": { "total": 5, "running": 4 },
                "cpus": 48,
                "memory_gib": 192
            }
        })
    }

    /// 목록의 노드·VM 칸은 `online/total`·`running/total`이고, 닿지 않는 원격은 이유를 보인다.
    #[test]
    fn remote_rows_and_lines() {
        assert_eq!(
            remote_row(&sample_remote()),
            ["dr", "dr-site", "0.37.0", "2/3", "4/5", "reachable"]
        );
        let down = json!({
            "name": "old",
            "status": { "reachable": false, "error": "connection refused" }
        });
        assert_eq!(
            remote_row(&down),
            ["old", "-", "-", "-", "-", "unreachable: connection refused"]
        );
        let lines = remote_lines(&sample_remote());
        for line in [
            "url: https://10.0.0.5:8443",
            "ca fingerprint: sha256:aa",
            "token id: 0190b9d4-0000-7000-8000-000000000002",
            "status: reachable",
            "nodes online: 2/3",
            "vms running: 4/5",
            "cpus: 48",
            "memory: 192 GiB",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
        assert!(
            remote_lines(&down).contains(&"changed at: never".to_string()),
            "{:?}",
            remote_lines(&down)
        );
    }

    /// 원격 명령이 부르는 API: `add`는 토큰 파일과 준 지문을, `set`은 읽은 revision을 If-Match로 하고
    /// 토큰을 주지 않으면 빼서 보낸다. `vms`·`vm`은 중계 경로로 원격 API를 부른다.
    #[tokio::test]
    async fn remote_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/remotes/dr/api/vms" => json!({
                "revision": 3,
                "items": [{
                    "name": "web",
                    "spec": { "power": "running", "ha": false },
                    "status": { "phase": "running", "node": "n1", "pid": 42 }
                }]
            }),
            "/api/v1/remotes/dr/api/vms/web/actions/stop?mode=force" => {
                json!({ "id": "t1", "status": { "state": "pending" } })
            }
            _ => sample_remote(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        let secret = format!("opl_{}_s3cret", uuid::Uuid::now_v7().simple());
        std::fs::write(&token, format!("{secret}\n")).unwrap();
        let token = token.to_str().unwrap();

        let mut out = Vec::new();
        for args in [
            &[
                "add",
                "dr",
                "--url",
                "https://10.0.0.5:8443",
                "--ca-fingerprint",
                "sha256:aa",
                "--token-file",
                token,
            ][..],
            &["set", "dr", "--url", "https://10.0.0.6:8443"][..],
            &["set", "dr", "--token-file", token][..],
            &["remove", "dr"][..],
            &["vms", "dr"][..],
            &["vm", "dr", "stop", "web", "--force"][..],
        ] {
            remote(&client, remote_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "POST /api/v1/remotes HTTP/1.1",
                "GET /api/v1/remotes/dr HTTP/1.1",
                "PUT /api/v1/remotes/dr HTTP/1.1",
                "GET /api/v1/remotes/dr HTTP/1.1",
                "PUT /api/v1/remotes/dr HTTP/1.1",
                "DELETE /api/v1/remotes/dr HTTP/1.1",
                "GET /api/v1/remotes/dr/api/vms HTTP/1.1",
                "POST /api/v1/remotes/dr/api/vms/web/actions/stop?mode=force HTTP/1.1",
            ]
        );
        assert_eq!(
            seen[0].body,
            json!({
                "name": "dr",
                "url": "https://10.0.0.5:8443",
                "ca_fingerprint": "sha256:aa",
                "token": secret,
            })
        );
        assert_eq!(seen[2].if_match.as_deref(), Some("9"));
        assert_eq!(
            seen[2].body,
            json!({ "url": "https://10.0.0.6:8443", "ca_fingerprint": "sha256:aa" })
        );
        assert_eq!(
            seen[4].body,
            json!({
                "url": "https://10.0.0.5:8443",
                "ca_fingerprint": "sha256:aa",
                "token": secret,
            })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "remote dr added (reachable)\n\
             remote dr updated\n\
             remote dr updated\n\
             remote dr removed\n\
             NAME  POWER    PHASE    NODE  HA     PID  ERROR\n\
             web   running  running  n1    false  42   -\n\
             vm web on remote dr: force stop requested (task t1)\n"
        );
    }

    /// `remote list`는 원격마다 한 줄, `show`·`refresh`는 `키: 값` 줄이다.
    #[tokio::test]
    async fn remote_list_and_refresh() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/remotes" => json!({ "revision": 9, "items": [sample_remote()] }),
            _ => sample_remote(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        remote(&client, remote_cmd(&["list"]).unwrap(), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  CLUSTER  VERSION  NODES  VMS  STATUS\n\
             dr    dr-site  0.37.0   2/3    4/5  reachable\n"
        );
        let mut out = Vec::new();
        remote(
            &client,
            remote_cmd(&["refresh", "dr"]).unwrap(),
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            remote_lines(&sample_remote()).join("\n") + "\n"
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.line.clone())
            .collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/remotes HTTP/1.1",
                "POST /api/v1/remotes/dr/actions/refresh HTTP/1.1",
            ]
        );
    }
}
```

`crates/opld/tests/remotes.rs`를 만든다:

```rust
//! 계획 37: 한 노드짜리 opld 클러스터 둘. 한쪽(prod)에 다른 쪽(dr)을 원격으로 등록하면(dr에서 만든 API
//! 토큰, 받은 CA 지문을 `--yes`로 믿음) prod의 리더가 dr의 상태를 모으고, `oplctl remote vms`가 prod를
//! 거쳐 dr의 VM을 읽는다. dr이 멈추면 상태가 닿지 않음으로 바뀐다. QEMU는 쓰지 않는다.

mod common;

use common::{Daemon, eventually};
use serde_json::Value;

/// 수집 틱과 간격을 짧게
const REMOTES: &str = "[remotes]\ntick = \"1s\"\ninterval = \"1s\"";

/// `oplctl -o json remote list`의 첫 원격 상태
async fn remote_status(d: &Daemon) -> Value {
    d.ctl_json(&["remote", "list"]).await[0]["status"].clone()
}

#[tokio::test]
async fn a_remote_cluster_is_registered_polled_and_proxied() {
    let mut prod = Daemon::new("node-a", REMOTES);
    let mut dr = Daemon::new("node-b", REMOTES);
    prod.start().await;
    dr.start().await;
    prod.init("prod").await;
    dr.init("dr").await;
    prod.login().await;
    dr.login().await;

    // dr에 꺼진 VM 하나와 prod가 쓸 API 토큰
    dr.ctl(&["vm", "create", "web", "--memory", "256"])
        .await
        .unwrap();
    let token = dr.ctl(&["token", "create", "prod"]).await.unwrap();
    let token_file = prod.path("dr-token");
    std::fs::write(&token_file, token).unwrap();
    let token_file = token_file.to_str().unwrap();

    // 자기 클러스터는 등록하지 못한다.
    let own = prod.server();
    let e = prod
        .ctl(&[
            "remote",
            "add",
            "self",
            "--url",
            &own,
            "--ca-fingerprint",
            prod.ca_fingerprint.as_deref().unwrap(),
            "--token-file",
            token_file,
        ])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("remote_is_local"), "{e}");

    // 지문을 주지 않으면 받은 지문을 보이고 `--yes`로 믿는다.
    let url = dr.server();
    let out = prod
        .ctl(&[
            "remote",
            "add",
            "dr",
            "--url",
            &url,
            "--token-file",
            token_file,
            "--yes",
        ])
        .await
        .unwrap();
    let fp = dr.ca_fingerprint.as_deref().unwrap();
    assert_eq!(
        out,
        format!("the remote cluster CA fingerprint is {fp}\nremote dr added (reachable)\n")
    );

    let status = remote_status(&prod).await;
    assert_eq!(status["reachable"], true, "{status}");
    assert_eq!(status["cluster_name"], "dr", "{status}");
    assert!(status["version"].is_string(), "{status}");
    assert_eq!(status["nodes"]["total"], 1, "{status}");
    assert_eq!(status["nodes"]["online"], 1, "{status}");
    assert_eq!(status["vms"]["total"], 1, "{status}");
    assert_eq!(status["vms"]["running"], 0, "{status}");
    let list = prod.ctl(&["remote", "list"]).await.unwrap();
    let row = list.lines().nth(1).unwrap();
    let cells: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cells[..2], ["dr", "dr"], "{list}");
    assert_eq!(cells[3..], ["1/1", "0/1", "reachable"], "{list}");

    // prod를 거쳐 dr의 VM을 읽고 동작을 요청한다.
    let vms = prod.ctl(&["remote", "vms", "dr"]).await.unwrap();
    assert!(vms.starts_with("NAME"), "{vms}");
    let web = vms.lines().find(|l| l.starts_with("web ")).unwrap();
    assert!(web.contains("stopped"), "{vms}");
    let out = prod
        .ctl(&["remote", "vm", "dr", "stop", "web"])
        .await
        .unwrap();
    assert!(
        out.starts_with("vm web on remote dr: stop requested (task "),
        "{out}"
    );

    // dr이 멈추면 리더의 수집기가 닿지 않음을 적는다.
    dr.stop();
    eventually("the remote becomes unreachable", || async {
        remote_status(&prod).await["reachable"] == false
    })
    .await;
    let list = prod.ctl(&["remote", "list"]).await.unwrap();
    assert!(list.contains("unreachable: "), "{list}");
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1472개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): 원격 클러스터 명령과 opld E2E — oplctl remote list(NAME·CLUSTER·VERSION·NODES online/total·VMS running/total·STATUS reachable|unreachable: <이유>), show(키: 값 줄 — 토큰은 id만, 상태가 바뀐 시각, CPU·메모리), add <이름> --url --token-file <파일|-> [--ca-fingerprint] [--yes](지문이 없으면 인증서를 검증하지 않고 원격의 CA를 받아 지문을 보이고, --yes면 믿고, 터미널이면 [y/N]로 묻고, 아니면 --ca-fingerprint <지문>으로 다시 실행하라고 실패), set <이름> [--url] [--ca-fingerprint] [--token-file](읽은 원격에 준 것만 바꿔 If-Match로 PUT, 토큰을 주지 않으면 빼서 보냄 — 옮기기 규칙은 서버가 판단), remove, refresh(곧 다시 모은 상태를 show처럼), vms <이름>(중계 GET /vms — vm list와 같은 표, VM_HEADERS를 같이 씀), vm <이름> start|stop|shutdown <VM>(중계 POST /vms/<VM>/actions/start·stop?mode=force·stop — stop은 곧바로 끄기, shutdown은 ACPI, Task를 기다리지 않고 id를 보임). 시험: 명령 파싱과 동작 대응, 지문 확인(--yes·터미널 아님·y/빈 답), 목록 행과 show 줄, fake_api로 add·set·remove·vms·vm 요청과 본문·If-Match·출력, list·refresh. opld tests/remotes.rs: 한 노드 클러스터 둘([remotes] tick·interval 1s), 자기 클러스터 등록은 remote_is_local, dr에서 만든 토큰으로 remote add --yes → 지문 출력과 reachable, remote list의 cluster dr·노드 1/1·VM 0/1, remote vms와 remote vm shutdown, dr을 멈추면 수집기가 unreachable로 적음"
```

---

### Task 5: 웹: 원격 클러스터 화면

메뉴 "원격 클러스터": 카드 목록(상태, 버전, 노드·VM 수, CPU·메모리, 10초마다), admin의 등록(CA 지문 받아 확인)·고치기·지우기·새로고침, 카드를 누르면 그 클러스터의 VM 목록과 시작·정지·강제 정지(중계로). OpenAPI 스키마 이름 `RemoteNodeCounts`·`RemoteVmCounts`. ko·en.

**Files:**
- Create: `web/src/remotes/RemotePage.test.tsx`
- Create: `web/src/remotes/RemotePage.tsx`
- Create: `web/src/remotes/RemotesPage.test.tsx`
- Create: `web/src/remotes/RemotesPage.tsx`
- Create: `web/src/remotes/remotes.ts`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-types/src/remote.rs`
- Modify: `web/src/App.tsx`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/auth/permissions.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/layout/AppLayout.tsx`
- Modify: `web/src/test/fixtures.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/tests/openapi.rs`에서

```rust
    );
    // 백업 작업의 VM 고르기는 세 객체 중 하나다.
```

을 다음으로 바꾼다:

```rust
    );
    // 원격 상태의 노드·VM 수는 텔레메트리 보고의 것과 이름이 겹치지 않는다.
    let schemas = &doc["components"]["schemas"];
    let remote_status = &schemas["RemoteStatus"]["properties"];
    assert!(
        remote_status["nodes"]
            .to_string()
            .contains("RemoteNodeCounts"),
        "{remote_status}"
    );
    assert!(
        remote_status["vms"].to_string().contains("RemoteVmCounts"),
        "{remote_status}"
    );
    assert!(schemas["RemoteNodeCounts"]["properties"]["online"].is_object());
    assert!(schemas["RemoteVmCounts"]["properties"]["ha"].is_null());
    assert!(schemas["NodeCounts"]["properties"]["witness"].is_object());
    // 백업 작업의 VM 고르기는 세 객체 중 하나다.
```

`crates/opl-types/src/remote.rs`에서

```rust

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeCounts {
```

을 다음으로 바꾼다:

```rust

// 텔레메트리 보고의 `NodeCounts`와 API 문서의 이름이 겹치지 않게 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(as = RemoteNodeCounts))]
pub struct NodeCounts {
```

`crates/opl-types/src/remote.rs`에서

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmCounts {
```

을 다음으로 바꾼다:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(as = RemoteVmCounts))]
pub struct VmCounts {
```

`web/src/App.tsx`에서

```tsx
import { RealmsPage } from "@/realms/RealmsPage";
import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
```

을 다음으로 바꾼다:

```tsx
import { RealmsPage } from "@/realms/RealmsPage";
import { RemotePage } from "@/remotes/RemotePage";
import { RemotesPage } from "@/remotes/RemotesPage";
import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "cluster", element: <ClusterPage /> },
            { path: "settings", element: <SettingsPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "cluster", element: <ClusterPage /> },
            { path: "remotes", element: <RemotesPage /> },
            { path: "remotes/:name", element: <RemotePage /> },
            { path: "settings", element: <SettingsPage /> },
```

`web/src/auth/permissions.test.ts`에서

```ts
  expect(can("admin", "write", "settings")).toBe(true);
});
```

을 다음으로 바꾼다:

```ts
  expect(can("admin", "write", "settings")).toBe(true);
  // 원격 클러스터도 누구나 읽고 admin만 바꾼다(중계로 원격을 바꾸는 요청도).
  expect(can("viewer", "read", "remote")).toBe(true);
  expect(can("operator", "write", "remote")).toBe(false);
  expect(can("admin", "write", "remote")).toBe(true);
});
```

`web/src/auth/permissions.ts`에서

```ts
/** 권한을 따지는 리소스 종류. 이름은 서버의 `Kind`와 같다. */
export type Kind = WatchKind | "settings" | "user" | "api-token" | "join-token" | "realm";
```

을 다음으로 바꾼다:

```ts
/** 권한을 따지는 리소스 종류. 이름은 서버의 `Kind`와 같다. */
export type Kind =
  WatchKind | "settings" | "user" | "api-token" | "join-token" | "realm" | "remote";
```

`web/src/i18n/en.json`에서

```json
    "external_user": "This user signs in through a realm. Change the password in the directory.",
    "sso_failed": "Single sign-on failed. Try again, or ask an administrator to check the realm."
  },
```

을 다음으로 바꾼다:

```json
    "external_user": "This user signs in through a realm. Change the password in the directory.",
    "sso_failed": "Single sign-on failed. Try again, or ask an administrator to check the realm.",
    "remote_unreachable": "Cannot reach the remote cluster ({{message}}).",
    "remote_fingerprint_mismatch": "The remote cluster's CA fingerprint is different. Get the CA again and check it.",
    "remote_is_local": "That address is this cluster. Add a different cluster.",
    "remote_unauthorized": "The remote cluster rejected the API token. Check that the token still exists there."
  },
```

`web/src/i18n/en.json`에서

```json
    "cluster": "Cluster",
    "settings": "Settings"
```

을 다음으로 바꾼다:

```json
    "cluster": "Cluster",
    "remotes": "Remote clusters",
    "settings": "Settings"
```

`web/src/i18n/en.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "remotes": {
    "pageHint": "Other opLite clusters this cluster can see. The status is collected every 30 seconds. What you can do on a remote cluster also depends on the role of the user who owns its API token there.",
    "empty": "No remote clusters.",
    "add": "Add",
    "addTitle": "Add a remote cluster",
    "addDescription": "Register another opLite cluster with its address, the fingerprint of its CA and an API token created on it.",
    "editTitle": "Edit remote cluster {{name}}",
    "editNamed": "Edit {{name}}",
    "refreshNamed": "Refresh {{name}}",
    "url": "URL",
    "urlHint": "https://host[:port] of a node of the remote cluster.",
    "getCa": "Get the CA",
    "fingerprint": "CA fingerprint",
    "confirmFingerprint": "This matches the CA fingerprint on the Cluster page of the remote cluster.",
    "token": "API token",
    "tokenHint": "Create it on the API tokens page of the remote cluster. Its user's role there limits what this cluster can do.",
    "tokenSaved": "A token is saved. Leave this empty to keep it.",
    "cluster": "Cluster",
    "version": "Version",
    "nodes": "Nodes",
    "nodesValue": "{{online}} / {{total}} online",
    "vms": "VMs",
    "vmsValue": "{{running}} / {{total}} running",
    "cpus": "CPUs",
    "memory": "Memory",
    "memoryValue": "{{gib}} GiB",
    "reachable": "Reachable",
    "unreachable": "Unreachable",
    "created": "Added remote cluster {{name}}.",
    "saved": "Saved remote cluster {{name}}.",
    "deleted": "Deleted remote cluster {{name}}.",
    "refreshFailed": "Could not refresh {{name}}: {{error}}",
    "deleteTitle": "Delete remote cluster {{name}}?",
    "deleteDescription": "This cluster stops showing it. Nothing changes on the remote cluster, and its API token stays there until you delete it.",
    "backToList": "Back to remote clusters",
    "vmsTitle": "Virtual machines on {{name}}",
    "actionRequested": "{{action}} requested for {{name}} on {{remote}}.",
    "errors": {
      "probe": "Could not get the CA: {{error}}",
      "probeFirst": "Get the CA of this URL first.",
      "confirm": "Check the fingerprint against the remote cluster and confirm it.",
      "token": "Enter an API token of the remote cluster.",
      "tokenAgain": "Enter the API token again: the URL or CA changed."
    }
  },
  "vmForm": {
```

`web/src/i18n/ko.json`에서

```json
    "external_user": "영역으로 로그인하는 사용자입니다. 비밀번호는 디렉터리에서 바꾸세요.",
    "sso_failed": "SSO 로그인에 실패했습니다. 다시 시도하거나 관리자에게 영역 확인을 요청하세요."
  },
```

을 다음으로 바꾼다:

```json
    "external_user": "영역으로 로그인하는 사용자입니다. 비밀번호는 디렉터리에서 바꾸세요.",
    "sso_failed": "SSO 로그인에 실패했습니다. 다시 시도하거나 관리자에게 영역 확인을 요청하세요.",
    "remote_unreachable": "원격 클러스터에 닿지 못했습니다({{message}}).",
    "remote_fingerprint_mismatch": "원격 클러스터의 CA 지문이 다릅니다. CA를 다시 받아 확인하세요.",
    "remote_is_local": "그 주소는 이 클러스터입니다. 다른 클러스터를 추가하세요.",
    "remote_unauthorized": "원격 클러스터가 API 토큰을 거절했습니다. 그곳에 토큰이 아직 있는지 확인하세요."
  },
```

`web/src/i18n/ko.json`에서

```json
    "cluster": "클러스터",
    "settings": "설정"
```

을 다음으로 바꾼다:

```json
    "cluster": "클러스터",
    "remotes": "원격 클러스터",
    "settings": "설정"
```

`web/src/i18n/ko.json`에서

```json
  },
  "vmForm": {
```

을 다음으로 바꾼다:

```json
  },
  "remotes": {
    "pageHint": "이 클러스터가 보는 다른 opLite 클러스터입니다. 상태는 30초마다 모읍니다. 원격 클러스터에서 할 수 있는 일은 그곳 API 토큰 주인의 역할로도 정해집니다.",
    "empty": "원격 클러스터가 없습니다.",
    "add": "추가",
    "addTitle": "원격 클러스터 추가",
    "addDescription": "다른 opLite 클러스터를 주소, CA 지문, 그 클러스터에서 만든 API 토큰으로 등록합니다.",
    "editTitle": "원격 클러스터 {{name}} 고치기",
    "editNamed": "{{name}} 고치기",
    "refreshNamed": "{{name}} 새로고침",
    "url": "URL",
    "urlHint": "원격 클러스터 노드의 https://호스트[:포트]",
    "getCa": "CA 받기",
    "fingerprint": "CA 지문",
    "confirmFingerprint": "원격 클러스터의 클러스터 화면에 보이는 CA 지문과 같습니다.",
    "token": "API 토큰",
    "tokenHint": "원격 클러스터의 API 토큰 화면에서 만듭니다. 그곳에서 토큰 주인의 역할이 이 클러스터가 할 수 있는 일을 정합니다.",
    "tokenSaved": "토큰이 저장돼 있습니다. 그대로 두려면 비워 두세요.",
    "cluster": "클러스터",
    "version": "버전",
    "nodes": "노드",
    "nodesValue": "{{total}}개 중 {{online}}개 온라인",
    "vms": "VM",
    "vmsValue": "{{total}}개 중 {{running}}개 실행 중",
    "cpus": "CPU",
    "memory": "메모리",
    "memoryValue": "{{gib}} GiB",
    "reachable": "연결됨",
    "unreachable": "연결 안 됨",
    "created": "원격 클러스터 {{name}}을(를) 추가했습니다.",
    "saved": "원격 클러스터 {{name}}을(를) 저장했습니다.",
    "deleted": "원격 클러스터 {{name}}을(를) 삭제했습니다.",
    "refreshFailed": "{{name}}을(를) 새로 읽지 못했습니다: {{error}}",
    "deleteTitle": "원격 클러스터 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 클러스터에서 보이지 않게 됩니다. 원격 클러스터는 바뀌지 않고, 그곳의 API 토큰도 지우기 전까지 남습니다.",
    "backToList": "원격 클러스터 목록으로",
    "vmsTitle": "{{name}}의 가상 머신",
    "actionRequested": "{{remote}}의 {{name}}에 {{action}} 요청을 보냈습니다.",
    "errors": {
      "probe": "CA를 받지 못했습니다: {{error}}",
      "probeFirst": "먼저 이 URL의 CA를 받으세요.",
      "confirm": "지문을 원격 클러스터에서 확인하고 체크하세요.",
      "token": "원격 클러스터의 API 토큰을 넣으세요.",
      "tokenAgain": "URL이나 CA가 바뀌었으니 API 토큰을 다시 넣으세요."
    }
  },
  "vmForm": {
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute("href", "/settings");
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute("href", "/settings");
  // 원격 클러스터도 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Remote clusters" })).toHaveAttribute("href", "/remotes");
});
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  Database,
  HardDrive,
```

을 다음으로 바꾼다:

```tsx
  Database,
  Globe,
  HardDrive,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/cluster", key: "nav.cluster", icon: Boxes, end: false },
  { to: "/settings", key: "nav.settings", icon: Settings, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/cluster", key: "nav.cluster", icon: Boxes, end: false },
  { to: "/remotes", key: "nav.remotes", icon: Globe, end: false, kind: "remote" },
  { to: "/settings", key: "nav.settings", icon: Settings, end: false },
```

`web/src/remotes/RemotePage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ApiError, setUnauthorizedHandler } from "@/api/client";
import { RemotePage } from "@/remotes/RemotePage";
import { proxy } from "@/remotes/remotes";
import { remote, task, vm } from "@/test/fixtures";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const web = vm("web", "running");
const db = vm("db", "stopped");

function mockRemote(
  vms: Record<string, unknown> = { revision: 1, items: [web, db] },
  status = 200,
) {
  server.use(
    http.get(apiUrl("/remotes/dr"), () => HttpResponse.json(remote("dr"))),
    http.get(apiUrl("/remotes/dr/api/vms"), () => HttpResponse.json(vms, { status })),
  );
}

function render(role: "admin" | "viewer" = "admin") {
  return renderPage(<RemotePage />, {
    path: "/remotes/:name",
    url: "/remotes/dr",
    as: { ...ADMIN, role },
  });
}

it("lists the VMs of the remote through the proxy", async () => {
  mockRemote();
  render();
  const table = await screen.findByRole("table", { name: "Virtual machines on dr" });
  expect(await within(table).findByText("web")).toBeInTheDocument();
  const row = within(table).getByText("web").closest("tr")!;
  expect(row).toHaveTextContent("Running");
  expect(row).toHaveTextContent("2");
  expect(row).toHaveTextContent("2 GiB");
  expect(screen.getByText("dr · https://dr.example:8006")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Back to remote clusters" })).toHaveAttribute(
    "href",
    "/remotes",
  );
});

it("starts, shuts down and force stops remote VMs", async () => {
  mockRemote();
  const calls: string[] = [];
  server.use(
    http.post(apiUrl("/remotes/dr/api/vms/:id/actions/:action"), ({ request, params }) => {
      const url = new URL(request.url);
      calls.push(`${String(params.id)} ${String(params.action)}${url.search}`);
      return HttpResponse.json(task("t1", "vm-start", "x"), { status: 202 });
    }),
  );
  render();
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  // 멈춘 VM은 정지할 수 없다.
  expect(screen.getByRole("menuitem", { name: "Stop" })).toHaveAttribute("aria-disabled", "true");
  await userEvent.click(screen.getByRole("menuitem", { name: "Start" }));
  expect(await screen.findByText("Start requested for db on dr.")).toBeInTheDocument();

  await userEvent.click(screen.getByRole("button", { name: "Actions for web" }));
  await userEvent.click(screen.getByRole("menuitem", { name: "Stop" }));
  await screen.findByText("Stop requested for web on dr.");

  await userEvent.click(screen.getByRole("button", { name: "Actions for web" }));
  await userEvent.click(screen.getByRole("menuitem", { name: "Force stop" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Force stop web?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Force stop" }));
  await screen.findByText("Force stop requested for web on dr.");
  expect(calls).toEqual([`${db.id} start`, `${web.id} stop`, `${web.id} stop?mode=force`]);
});

it("shows the error of a refused action", async () => {
  mockRemote();
  server.use(
    http.post(apiUrl("/remotes/dr/api/vms/:id/actions/start"), () =>
      HttpResponse.json({ code: "forbidden", message: "viewer" }, { status: 403 }),
    ),
  );
  render();
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(screen.getByRole("menuitem", { name: "Start" }));
  expect(await screen.findByText("db: You do not have permission to do this.")).toBeInTheDocument();
});

it("shows the VMs to viewers without actions", async () => {
  mockRemote();
  render("viewer");
  expect(await screen.findByText("web")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Actions for web" })).not.toBeInTheDocument();
});

it("shows a token the remote refused as an error, not a sign-out", async () => {
  mockRemote(
    { code: "remote_unauthorized", message: "the remote cluster refused the stored token" },
    502,
  );
  render();
  // 서버 오류라 두 번 더 읽어 본 뒤(1초·2초 뒤) 보인다.
  expect(
    await screen.findByText(
      "The remote cluster rejected the API token. Check that the token still exists there.",
      {},
      { timeout: 6000 },
    ),
  ).toBeInTheDocument();
  expect(screen.queryByText("moved")).not.toBeInTheDocument();
}, 10_000);

it("reports a 401 of this cluster like other requests", async () => {
  const unauthorized = vi.fn();
  setUnauthorizedHandler(unauthorized, () => 3);
  onTestFinished(() =>
    setUnauthorizedHandler(
      () => {},
      () => 0,
    ),
  );
  server.use(
    http.get(apiUrl("/remotes/dr/api/vms"), () =>
      HttpResponse.json({ code: "unauthorized", message: "no session" }, { status: 401 }),
    ),
  );
  await expect(proxy("dr", "GET", "/vms")).rejects.toBeInstanceOf(ApiError);
  expect(unauthorized).toHaveBeenCalledWith(3);
});
```

`web/src/remotes/RemotePage.tsx`를 만든다:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, MoreHorizontal } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ListPending } from "@/components/ListPending";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatMib } from "@/lib/format";
import { REMOTE_ACTIONS, remoteVmAction, remoteVms, type RemoteVmAction } from "@/remotes/remotes";
import { availability } from "@/vms/actions";
import { vcpus } from "@/vms/VmListPage";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

/** 원격 상태와 VM 목록을 다시 읽는 간격 */
const REFRESH_MS = 10_000;

/**
 * 원격 클러스터 하나의 VM 목록. 이 클러스터의 중계로 10초마다 읽는다. admin은 시작·종료·강제 정지를
 * 요청한다(원격 쪽에서는 토큰 주인의 역할이 한 번 더 정한다).
 */
export function RemotePage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const me = usePrincipal();
  const admin = can(me.role, "write", "remote");
  const queryClient = useQueryClient();
  const remote = useQuery({
    queryKey: ["remotes", name],
    queryFn: () => call(api.GET("/remotes/{key}", { params: { path: { key: name } } })),
    refetchInterval: REFRESH_MS,
  });
  const vms = useQuery({
    queryKey: ["remotes", name, "vms"],
    queryFn: ({ signal }) => remoteVms(name, signal),
    select: (items) => [...items].sort((a, b) => a.name.localeCompare(b.name)),
    refetchInterval: REFRESH_MS,
    enabled: remote.isSuccess,
  });
  const [forceStopping, setForceStopping] = useState<Vm | null>(null);

  async function run(vm: Vm, action: RemoteVmAction) {
    try {
      await remoteVmAction(name, vm, action);
      toast.success(
        t("remotes.actionRequested", {
          action: t(`vms.action.${action}`),
          name: vm.name,
          remote: name,
        }),
      );
      await queryClient.invalidateQueries({ queryKey: ["remotes", name, "vms"] });
    } catch (err) {
      toast.error(t("vms.actionFailed", { name: vm.name, error: errorMessage(t, err) }));
    }
  }

  const back = (
    <Link
      to="/remotes"
      className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:underline"
    >
      <ArrowLeft className="size-4" />
      {t("remotes.backToList")}
    </Link>
  );
  if (!remote.data) {
    return (
      <div className="flex flex-col gap-4">
        {back}
        <ListPending error={remote.error} />
      </div>
    );
  }
  const { status } = remote.data;
  const list = vms.data ?? [];
  return (
    <div className="flex flex-col gap-4">
      {back}
      <div className="flex flex-col gap-1">
        <h1 id="remote-vms-title" className="text-2xl font-semibold">
          {t("remotes.vmsTitle", { name })}
        </h1>
        <p className="font-mono text-xs text-muted-foreground">
          {status.cluster_name ? `${status.cluster_name} · ` : ""}
          {remote.data.url}
        </p>
      </div>
      {/* 다시 읽지 못하면 마지막으로 읽은 목록을 그대로 보이고 이유를 알린다. */}
      {vms.isError && vms.data && (
        <p role="alert" className="text-sm text-destructive">
          {errorMessage(t, vms.error)}
        </p>
      )}
      <Table aria-labelledby="remote-vms-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("vms.vcpus")}</TableHead>
            <TableHead>{t("vms.memory")}</TableHead>
            {admin && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={admin ? 5 : 4} query={vms} empty={t("vms.empty")} />
          )}
          {list.map((vm) => (
            <TableRow key={vm.id}>
              <TableCell className="font-medium">{vm.name}</TableCell>
              <TableCell>
                <VmPhaseBadge vm={vm} />
              </TableCell>
              <TableCell>{vcpus(vm)}</TableCell>
              <TableCell>{formatMib(vm.spec.memory_mib)}</TableCell>
              {admin && (
                <TableCell className="text-right">
                  <RemoteVmMenu
                    vm={vm}
                    onRun={(action) =>
                      action === "force-stop" ? setForceStopping(vm) : void run(vm, action)
                    }
                  />
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <ConfirmDialog
        open={forceStopping !== null}
        onOpenChange={(open) => !open && setForceStopping(null)}
        title={t("vms.forceStopTitle", { name: forceStopping?.name })}
        description={t("vms.forceStopDescription")}
        confirmLabel={t("vms.action.force-stop")}
        onConfirm={() => run(forceStopping!, "force-stop")}
      />
    </div>
  );
}

/** 원격 VM의 동작 메뉴. 상태로 할 수 없는 동작은 이유와 함께 막는다(원격의 Task는 모른다). */
function RemoteVmMenu({ vm, onRun }: { vm: Vm; onRun(action: RemoteVmAction): void }) {
  const { t } = useTranslation();
  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon-sm" aria-label={t("vms.actionsFor", { name: vm.name })}>
          <MoreHorizontal />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {REMOTE_ACTIONS.map((action) => {
          const able = availability(vm, action, []);
          return (
            <ReasonMenuItem
              key={action}
              reason={able.ok ? null : able.reason}
              onSelect={() => onRun(action)}
            >
              {t(`vms.action.${action}`)}
            </ReasonMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
```

`web/src/remotes/RemotesPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { RemotesPage } from "@/remotes/RemotesPage";
import { remote } from "@/test/fixtures";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const TOKEN = "opl_0192f0c15d2e70008000000000000000_secret";
const NEW_FP = `sha256:${"cd".repeat(32)}`;

function mockRemotes(...items: ReturnType<typeof remote>[]) {
  server.use(http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items })));
}

it("shows a card for each remote with its status", async () => {
  mockRemotes(
    remote("dr"),
    remote("lab", {
      reachable: false,
      error: "connection refused",
      cluster_name: null,
      version: null,
      nodes: null,
      vms: null,
      cpus: null,
      memory_gib: null,
    }),
  );
  renderPage(<RemotesPage />);
  const dr = await screen.findByRole("group", { name: /^dr/ });
  expect(within(dr).getByRole("link", { name: "dr" })).toHaveAttribute("href", "/remotes/dr");
  expect(dr).toHaveTextContent("Reachable");
  expect(dr).toHaveTextContent("https://dr.example:8006");
  expect(dr).toHaveTextContent("Version0.37.0");
  expect(dr).toHaveTextContent("Nodes1 / 1 online");
  expect(dr).toHaveTextContent("VMs1 / 2 running");
  expect(dr).toHaveTextContent("CPUs8");
  expect(dr).toHaveTextContent("Memory32 GiB");
  const lab = screen.getByRole("group", { name: /^lab/ });
  expect(lab).toHaveTextContent("Unreachable");
  expect(lab).toHaveTextContent("connection refused");
  expect(lab).toHaveTextContent("Version—");
  expect(within(lab).getByRole("button", { name: "Refresh lab" })).toBeInTheDocument();
});

it("shows the list to viewers without changes", async () => {
  mockRemotes(remote("dr"));
  renderPage(<RemotesPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByRole("link", { name: "dr" })).toBeInTheDocument();
  for (const name of ["Add", "Refresh dr", "Edit dr", "Delete dr"]) {
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
  }
});

it("adds a remote after the CA fingerprint is fetched and confirmed", async () => {
  mockRemotes();
  let probed: unknown;
  let body: unknown;
  server.use(
    http.post(apiUrl("/remotes/probe"), async ({ request }) => {
      probed = await request.json();
      return HttpResponse.json({ url: "https://dr.example:8006", ca_fingerprint: NEW_FP });
    }),
    http.post(apiUrl("/remotes"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(remote("dr"), { status: 201 });
    }),
  );
  renderPage(<RemotesPage />);
  expect(await screen.findByText("No remote clusters.")).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Add" }));
  const dialog = await screen.findByRole("dialog", { name: "Add a remote cluster" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "dr");
  await userEvent.type(within(dialog).getByLabelText("URL"), "https://dr.example:8006/");
  await userEvent.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(within(dialog).getByText("Get the CA of this URL first.")).toBeVisible();
  expect(within(dialog).getByText("Enter an API token of the remote cluster.")).toBeVisible();

  await userEvent.click(within(dialog).getByRole("button", { name: "Get the CA" }));
  expect(await within(dialog).findByText(NEW_FP)).toBeVisible();
  // 지문은 이름이 붙은 값(dt·dd)이다.
  expect(within(dialog).getByRole("term")).toHaveTextContent("CA fingerprint");
  expect(within(dialog).getByRole("definition")).toHaveTextContent(NEW_FP);
  expect(probed).toEqual({ url: "https://dr.example:8006/" });
  // 서버가 정규화한 URL로 바꾼다.
  expect(within(dialog).getByLabelText("URL")).toHaveValue("https://dr.example:8006");
  await userEvent.type(within(dialog).getByLabelText("API token"), TOKEN);
  await userEvent.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(
    within(dialog).getByText("Check the fingerprint against the remote cluster and confirm it."),
  ).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.click(
    within(dialog).getByRole("checkbox", { name: /matches the CA fingerprint/ }),
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(await screen.findByText("Added remote cluster dr.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "dr",
    url: "https://dr.example:8006",
    ca_fingerprint: NEW_FP,
    token: TOKEN,
  });
});

it("asks for the CA again when the URL changes and shows why it could not be fetched", async () => {
  mockRemotes();
  server.use(
    http.post(apiUrl("/remotes/probe"), () =>
      HttpResponse.json(
        { code: "remote_unreachable", message: "connection refused" },
        { status: 502 },
      ),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Add" }));
  const dialog = await screen.findByRole("dialog");
  await userEvent.type(within(dialog).getByLabelText("URL"), "https://gone.example");
  await userEvent.click(within(dialog).getByRole("button", { name: "Get the CA" }));
  expect(
    await within(dialog).findByText(
      "Could not get the CA: Cannot reach the remote cluster (connection refused).",
    ),
  ).toBeVisible();
});

it("shows why the server refused the remote", async () => {
  mockRemotes();
  server.use(
    http.post(apiUrl("/remotes/probe"), () =>
      HttpResponse.json({ url: "https://dr.example", ca_fingerprint: NEW_FP }),
    ),
    http.post(apiUrl("/remotes"), () =>
      HttpResponse.json(
        { code: "remote_unauthorized", message: "the remote cluster rejected the token" },
        { status: 400 },
      ),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Add" }));
  const dialog = await screen.findByRole("dialog");
  await userEvent.type(within(dialog).getByLabelText("Name"), "dr");
  await userEvent.type(within(dialog).getByLabelText("URL"), "https://dr.example");
  await userEvent.click(within(dialog).getByRole("button", { name: "Get the CA" }));
  await userEvent.click(await within(dialog).findByRole("checkbox"));
  await userEvent.type(within(dialog).getByLabelText("API token"), TOKEN);
  await userEvent.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The remote cluster rejected the API token.",
  );
});

it("edits a remote, keeping the token unless the URL or CA changes", async () => {
  const dr = remote("dr");
  mockRemotes(dr);
  const bodies: unknown[] = [];
  server.use(
    http.put(apiUrl("/remotes/dr"), async ({ request }) => {
      bodies.push(await request.json());
      expect(request.headers.get("If-Match")).toBeNull();
      return HttpResponse.json(dr);
    }),
    http.post(apiUrl("/remotes/probe"), () =>
      HttpResponse.json({ url: "https://dr2.example", ca_fingerprint: dr.ca_fingerprint }),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Edit dr" }));
  let dialog = await screen.findByRole("dialog", { name: "Edit remote cluster dr" });
  expect(within(dialog).queryByLabelText("Name")).not.toBeInTheDocument();
  expect(within(dialog).getByLabelText("URL")).toHaveValue(dr.url);
  expect(within(dialog).getByText(dr.ca_fingerprint)).toBeVisible();
  // 저장된 지문은 이미 확인했다.
  expect(within(dialog).queryByRole("checkbox")).not.toBeInTheDocument();
  expect(within(dialog).getByText("A token is saved. Leave this empty to keep it.")).toBeVisible();
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Saved remote cluster dr.")).toBeInTheDocument();
  expect(bodies).toEqual([{ url: dr.url, ca_fingerprint: dr.ca_fingerprint }]);

  await userEvent.click(screen.getByRole("button", { name: "Edit dr" }));
  dialog = await screen.findByRole("dialog", { name: "Edit remote cluster dr" });
  await userEvent.clear(within(dialog).getByLabelText("URL"));
  await userEvent.type(within(dialog).getByLabelText("URL"), "https://dr2.example");
  expect(within(dialog).queryByText(dr.ca_fingerprint)).not.toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Get the CA" }));
  await userEvent.click(await within(dialog).findByRole("checkbox"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    within(dialog).getByText("Enter the API token again: the URL or CA changed."),
  ).toBeVisible();
  await userEvent.type(within(dialog).getByLabelText("API token"), TOKEN);
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(bodies).toHaveLength(2));
  expect(bodies[1]).toEqual({
    url: "https://dr2.example",
    ca_fingerprint: dr.ca_fingerprint,
    token: TOKEN,
  });
});

it("refreshes and deletes a remote", async () => {
  let refreshed = false;
  let deleted = false;
  server.use(
    http.get(apiUrl("/remotes"), () =>
      HttpResponse.json({
        revision: 1,
        items: deleted ? [] : [refreshed ? remote("dr", { version: "0.38.0" }) : remote("dr")],
      }),
    ),
    http.post(apiUrl("/remotes/dr/actions/refresh"), () => {
      refreshed = true;
      return HttpResponse.json(remote("dr", { version: "0.38.0" }));
    }),
    http.delete(apiUrl("/remotes/dr"), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Refresh dr" }));
  expect(await screen.findByText("0.38.0")).toBeInTheDocument();

  await userEvent.click(screen.getByRole("button", { name: "Delete dr" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete remote cluster dr?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Deleted remote cluster dr.")).toBeInTheDocument();
  expect(await screen.findByText("No remote clusters.")).toBeInTheDocument();
});

it("shows why a refresh failed", async () => {
  mockRemotes(remote("dr"));
  server.use(
    http.post(apiUrl("/remotes/dr/actions/refresh"), () =>
      HttpResponse.json({ code: "not_leader", message: "x" }, { status: 503 }),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Refresh dr" }));
  expect(
    await screen.findByText(
      "Could not refresh dr: The cluster leader is changing. Try again shortly.",
    ),
  ).toBeInTheDocument();
});
```

`web/src/remotes/RemotesPage.tsx`를 만든다:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useId, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/i18n/errors";
import type { RemoteView } from "@/remotes/remotes";
import { validName } from "@/vms/form";

/** 목록을 다시 읽는 간격. 원격은 watch로 받지 않는다(토큰을 담고 있다). */
const REFRESH_MS = 10_000;

/**
 * 원격 클러스터: 카드 목록과 admin의 추가·고치기·지우기·새로고침. 카드를 누르면 그 클러스터의 VM 목록으로
 * 간다. 상태는 리더가 모은 것을 10초마다 다시 읽는다.
 */
export function RemotesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const admin = can(me.role, "write", "remote");
  const queryClient = useQueryClient();
  const remotes = useQuery({
    queryKey: ["remotes"],
    queryFn: () => call(api.GET("/remotes")),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
    refetchInterval: REFRESH_MS,
  });
  const [dialog, setDialog] = useState<{ remote: RemoteView | null } | null>(null);
  const [deleting, setDeleting] = useState<RemoteView | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["remotes"] });

  async function refresh(remote: RemoteView) {
    try {
      await call(
        api.POST("/remotes/{key}/actions/refresh", { params: { path: { key: remote.name } } }),
      );
      await changed();
    } catch (err) {
      toast.error(t("remotes.refreshFailed", { name: remote.name, error: errorMessage(t, err) }));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("nav.remotes")}</h1>
        {admin && (
          <Button
            onClick={(e) => {
              opener.current = e.currentTarget;
              setDialog({ remote: null });
            }}
          >
            <Plus />
            {t("remotes.add")}
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("remotes.pageHint")}</p>
      {!remotes.data ? (
        <ListPending error={remotes.error} />
      ) : remotes.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("remotes.empty")}</p>
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {remotes.data.map((remote) => (
            <RemoteCard
              key={remote.id}
              remote={remote}
              admin={admin}
              onRefresh={() => void refresh(remote)}
              onEdit={(target) => {
                opener.current = target;
                setDialog({ remote });
              }}
              onDelete={(target) => {
                opener.current = target;
                setDeleting(remote);
                setDeleteOpen(true);
              }}
            />
          ))}
        </div>
      )}
      {dialog && (
        <RemoteDialog
          remote={dialog.remote}
          returnFocus={opener}
          onClose={() => setDialog(null)}
          onDone={changed}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("remotes.deleteTitle", { name: deleting?.name })}
        description={t("remotes.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const remote = deleting!;
          await call(api.DELETE("/remotes/{key}", { params: { path: { key: remote.name } } }));
          toast.success(t("remotes.deleted", { name: remote.name }));
          await changed();
        }}
      />
    </div>
  );
}

/** 원격 하나의 카드: 이름(VM 목록으로 가는 링크), 연결 상태, 모은 요약 */
function RemoteCard({
  remote,
  admin,
  onRefresh,
  onEdit,
  onDelete,
}: {
  remote: RemoteView;
  admin: boolean;
  onRefresh(): void;
  onEdit(target: HTMLElement): void;
  onDelete(target: HTMLElement): void;
}) {
  const { t } = useTranslation();
  const id = useId();
  const { status } = remote;
  const rows: [string, string][] = [
    [t("remotes.cluster"), status.cluster_name ?? "—"],
    [t("remotes.version"), status.version ?? "—"],
    [t("remotes.nodes"), status.nodes ? t("remotes.nodesValue", status.nodes) : "—"],
    [t("remotes.vms"), status.vms ? t("remotes.vmsValue", status.vms) : "—"],
    [t("remotes.cpus"), status.cpus != null ? String(status.cpus) : "—"],
    [
      t("remotes.memory"),
      status.memory_gib != null ? t("remotes.memoryValue", { gib: status.memory_gib }) : "—",
    ],
  ];
  return (
    <Card role="group" aria-labelledby={id}>
      <CardHeader>
        <CardTitle id={id} className="flex flex-wrap items-center gap-2">
          <Link
            to={`/remotes/${encodeURIComponent(remote.name)}`}
            className="underline-offset-4 hover:underline"
          >
            {remote.name}
          </Link>
          {status.reachable ? (
            <Badge>{t("remotes.reachable")}</Badge>
          ) : (
            <Badge variant="destructive">{t("remotes.unreachable")}</Badge>
          )}
        </CardTitle>
        <p className="truncate font-mono text-xs text-muted-foreground">{remote.url}</p>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {!status.reachable && status.error && (
          <p className="text-sm break-words text-destructive">{status.error}</p>
        )}
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
          {rows.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-muted-foreground">{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
        {admin && (
          <div className="flex justify-end gap-1">
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("remotes.refreshNamed", { name: remote.name })}
              onClick={onRefresh}
            >
              <RefreshCw />
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("remotes.editNamed", { name: remote.name })}
              onClick={(e) => onEdit(e.currentTarget)}
            >
              <Pencil />
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t("common.deleteNamed", { name: remote.name })}
              onClick={(e) => onDelete(e.currentTarget)}
            >
              <Trash2 />
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** 서버가 CA를 받아 돌려준 정규화한 URL과 지문 */
interface Probe {
  url: string;
  ca_fingerprint: string;
}

/**
 * 원격 추가(`remote`가 null)와 고치기. 브라우저는 다른 출처의 CA를 읽지 못하므로 서버(`/remotes/probe`)가
 * 받아 온 지문을 보이고 사람이 원격 클러스터에서 확인하게 한다. 고칠 때는 저장된 URL·지문으로 시작하고,
 * URL이 바뀌면 CA를 다시 받는다. 토큰은 비우면 저장된 것을 두되, URL이나 지문이 바뀌면 다시 받는다(서버 규칙).
 */
function RemoteDialog({
  remote,
  returnFocus,
  onClose,
  onDone,
}: {
  remote: RemoteView | null;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [url, setUrl] = useState(remote?.url ?? "");
  const [probe, setProbe] = useState<Probe | null>(
    remote && { url: remote.url, ca_fingerprint: remote.ca_fingerprint },
  );
  const [probing, setProbing] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [token, setToken] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [probeError, setProbeError] = useState<string | null>(null);
  // 저장된 URL·지문 그대로면 이미 확인한 것이다.
  const same =
    remote !== null &&
    probe !== null &&
    probe.url === remote.url &&
    probe.ca_fingerprint === remote.ca_fingerprint;

  async function getCa() {
    setProbing(true);
    setProbeError(null);
    setErrors((e) => ({ ...e, url: "" }));
    try {
      const found = await call(api.POST("/remotes/probe", { body: { url } }));
      setProbe(found);
      setUrl(found.url);
      setConfirmed(false);
    } catch (err) {
      setProbeError(errorMessage(t, err));
    } finally {
      setProbing(false);
    }
  }

  async function submit() {
    const found: Record<string, string> = {};
    if (!remote && !validName(name)) {
      found.name = "forms.name";
    }
    if (!probe) {
      found.url = "remotes.errors.probeFirst";
    } else if (!same && !confirmed) {
      found.confirm = "remotes.errors.confirm";
    }
    if (token.trim() === "" && !same) {
      found.token = remote ? "remotes.errors.tokenAgain" : "remotes.errors.token";
    }
    setErrors(found);
    if (Object.keys(found).length > 0 || !probe) {
      return false;
    }
    const body = {
      url: probe.url,
      ca_fingerprint: probe.ca_fingerprint,
      ...(token.trim() !== "" && { token: token.trim() }),
    };
    // 상태를 모을 때마다 revision이 바뀌므로 If-Match 없이 보낸다(서버가 지금 것에 쓴다).
    if (remote) {
      await call(api.PUT("/remotes/{key}", { params: { path: { key: remote.name } }, body }));
      toast.success(t("remotes.saved", { name: remote.name }));
    } else {
      await call(api.POST("/remotes", { body: { name, ...body } }));
      toast.success(t("remotes.created", { name }));
    }
    await onDone();
  }

  return (
    <FormDialog
      title={remote ? t("remotes.editTitle", { name: remote.name }) : t("remotes.addTitle")}
      description={t("remotes.addDescription")}
      submitLabel={remote ? t("common.save") : t("remotes.add")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!remote && (
        <Field id="remote-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="remote-name"
              autoComplete="off"
              value={name}
              onChange={(e) => setName(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field
        id="remote-url"
        label={t("remotes.url")}
        error={probeError ? "remotes.errors.probe" : errors.url}
        errorValues={probeError ? { error: probeError } : undefined}
        hint={t("remotes.urlHint")}
      >
        {(a11y) => (
          <div className="flex gap-2">
            <Input
              id="remote-url"
              autoComplete="off"
              spellCheck={false}
              placeholder="https://"
              value={url}
              onChange={(e) => {
                setUrl(e.target.value);
                // 받은 지문은 그 URL의 것이다.
                setProbe(null);
                setConfirmed(false);
                setProbeError(null);
              }}
              {...a11y}
            />
            <Button
              type="button"
              variant="outline"
              disabled={probing || url.trim() === ""}
              onClick={() => void getCa()}
            >
              {t("remotes.getCa")}
            </Button>
          </div>
        )}
      </Field>
      {probe && (
        <div className="flex flex-col gap-2">
          <dl className="flex flex-col gap-2">
            <dt className="text-sm leading-none font-medium">{t("remotes.fingerprint")}</dt>
            <dd>
              <code className="block rounded-md bg-muted px-2 py-1 text-xs break-all">
                {probe.ca_fingerprint}
              </code>
            </dd>
          </dl>
          {!same && (
            <div className="flex items-start gap-2">
              <Checkbox
                id="remote-confirm"
                checked={confirmed}
                aria-invalid={errors.confirm ? true : undefined}
                aria-describedby={errors.confirm ? "remote-confirm-error" : undefined}
                onCheckedChange={(on) => setConfirmed(on === true)}
              />
              <Label htmlFor="remote-confirm" className="leading-snug font-normal">
                {t("remotes.confirmFingerprint")}
              </Label>
            </div>
          )}
          {errors.confirm && (
            <p id="remote-confirm-error" className="text-xs text-destructive">
              {t(errors.confirm)}
            </p>
          )}
        </div>
      )}
      <Field
        id="remote-token"
        label={t("remotes.token")}
        error={errors.token}
        hint={t(remote ? "remotes.tokenSaved" : "remotes.tokenHint")}
      >
        {(a11y) => (
          <Input
            id="remote-token"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={token}
            onChange={(e) => setToken(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/remotes/remotes.ts`를 만든다:

```ts
import { currentAuthEpoch, NetworkError, reportUnauthorized, toApiError } from "@/api/client";
import type { components } from "@/api/schema";
import type { Task, Vm } from "@/api/types";

export type RemoteView = components["schemas"]["RemoteView"];

/** 원격 클러스터 화면에서 하는 VM 동작. `stop`은 ACPI 종료, `force-stop`은 곧바로 끄기다. */
export type RemoteVmAction = "start" | "stop" | "force-stop";

export const REMOTE_ACTIONS: readonly RemoteVmAction[] = ["start", "stop", "force-stop"];

/** 원격 `remote`의 API `path`(`/vms` 따위, 쿼리 포함)로 가는 중계 주소 */
export function proxyUrl(remote: string, path: string): string {
  return `${window.location.origin}/api/v1/remotes/${encodeURIComponent(remote)}/api${path}`;
}

/**
 * 원격 클러스터의 API를 이 클러스터의 중계로 부른다. 중계는 API 문서에 없어 fetch로 부른다. 오류 응답은
 * `ApiError`, 닿지 못하면 `NetworkError`다. 401은 이 클러스터의 세션이 끝난 것이므로 다른 요청처럼
 * 알린다(원격이 저장된 토큰을 거절하면 중계는 502 `remote_unauthorized`를 준다).
 */
export async function proxy<T>(
  remote: string,
  method: "GET" | "POST",
  path: string,
  signal?: AbortSignal,
): Promise<T> {
  let response: Response;
  let body: unknown = null;
  const sentAt = currentAuthEpoch();
  try {
    response = await globalThis.fetch(proxyUrl(remote, path), {
      method,
      headers: { Accept: "application/json" },
      signal,
    });
    const text = await response.text();
    try {
      body = text === "" ? null : JSON.parse(text);
    } catch {
      // 프록시의 HTML 오류 페이지 따위
    }
  } catch (error) {
    if (signal?.aborted) {
      throw error;
    }
    throw new NetworkError(error);
  }
  if (response.status === 401) {
    reportUnauthorized(sentAt);
  }
  if (!response.ok) {
    throw toApiError(response.status, body);
  }
  return body as T;
}

/** 원격의 VM 목록 */
export async function remoteVms(remote: string, signal?: AbortSignal): Promise<Vm[]> {
  const list = await proxy<{ items: Vm[] }>(remote, "GET", "/vms", signal);
  return list.items;
}

/** 원격 VM의 동작을 요청한다. 원격이 만든 Task를 돌려준다(이 클러스터의 Task가 아니라 지켜보지 않는다). */
export function remoteVmAction(remote: string, vm: Vm, action: RemoteVmAction): Promise<Task> {
  const path =
    action === "force-stop"
      ? `/vms/${vm.id}/actions/stop?mode=force`
      : `/vms/${vm.id}/actions/${action}`;
  return proxy<Task>(remote, "POST", path);
}
```

`web/src/test/fixtures.ts`에서

```ts
} from "@/api/types";
import type { PoolState } from "@/storage/pools";
```

을 다음으로 바꾼다:

```ts
} from "@/api/types";
import type { RemoteView } from "@/remotes/remotes";
import type { PoolState } from "@/storage/pools";
```

`web/src/test/fixtures.ts`에서

```ts
).join(":");
```

을 다음으로 바꾼다:

```ts
).join(":");

/** 테스트용 원격 클러스터. 기본은 닿는 한 노드 클러스터다. */
export function remote(name: string, status: Partial<RemoteView["status"]> = {}): RemoteView {
  return {
    id: `019a0f3c-0000-7000-8002-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    url: `https://${name}.example:8006`,
    ca_fingerprint: `sha256:${"ab".repeat(32)}`,
    token_id: "0192f0c1-5d2e-7000-8000-000000000000",
    status: {
      reachable: true,
      checked_at: 1700000000,
      error: null,
      cluster_name: name,
      version: "0.37.0",
      nodes: { total: 1, online: 1 },
      vms: { total: 2, running: 1 },
      cpus: 8,
      memory_gib: 32,
      ...status,
    },
  };
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web build`
Expected: 모두 통과

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(web): 원격 클러스터 화면 — 메뉴 \"원격 클러스터\"(모두에게, Remote 읽기)와 /remotes: 원격마다 카드(이름 — /remotes/<이름>으로 가는 링크, 연결됨·연결 안 됨, URL, 클러스터 이름, 버전, 노드 online/total, VM running/total, CPU, 메모리 GiB, 닿지 않으면 오류 문구), 10초마다 다시 읽음. admin: 추가 대화상자(이름, URL과 \"CA 받기\" — 브라우저는 다른 출처를 읽지 못하므로 POST /remotes/probe가 받은 지문을 보이고 정규화한 URL로 바꿈, 지문 확인 체크, API 토큰 → POST /remotes), 고치기(저장된 URL·지문으로 시작해 확인이 필요 없고, URL을 바꾸면 CA를 다시 받아 확인, 토큰을 비우면 빼서 보내 유지하되 URL·지문이 바뀌면 다시 요구, 상태 수집이 revision을 올리므로 If-Match 없이 PUT), 지우기, 새로고침(actions/refresh, 실패는 알림). /remotes/<이름>: 그 원격의 VM 목록(중계 GET /vms를 fetch로 10초마다 — 중계는 API 문서에 없음)과 admin 동작 메뉴(시작, 정지 = ACPI stop, 강제 정지 = stop?mode=force 확인 뒤, 상태로 막힌 동작은 로컬 VM과 같은 이유로 막음, 원격 Task는 지켜보지 않고 요청했다고 알림). 중계 응답의 401은 원격이 토큰을 거절한 것이므로 로컬 로그아웃으로 다루지 않고 remote_unauthorized 오류로 보임(로컬 세션 끝은 함께 읽는 GET /remotes/{key}가 알림). permissions의 Kind에 remote, 오류 문구 remote_unreachable·remote_fingerprint_mismatch·remote_is_local·remote_unauthorized, ko·en. API 문서에서 원격 상태의 NodeCounts·VmCounts가 텔레메트리 보고의 같은 이름 스키마에 덮이던 것을 RemoteNodeCounts·RemoteVmCounts로 이름을 따로 줘 고치고 schema.d.ts를 다시 만듦. 시험: 카드와 상태·닿지 않는 원격, viewer는 바꾸기 버튼 없음, 추가(CA 먼저·지문 확인·토큰 요구·본문), probe 실패 문구, 서버 거절 문구, 고치기 토큰 유지·다시 요구, 새로고침·지우기, 새로고침 실패, 원격 VM 목록·시작·정지·강제 정지 요청 경로, 거절된 동작, viewer는 동작 없음, 중계 401은 로그아웃하지 않음, 메뉴, 권한, openapi 스키마 이름"
```

---

### Task 6: 문서

README 새 절 "원격 클러스터"(대등한 두 클러스터, 권한, 등록과 지문, 상태 수집, 중계 허용 목록, oplctl·웹, `[remotes]`, 한계), 엔터프라이즈 설계 §5.6 "구현(계획 37)"과 §7·§9, 처음 분해 문서의 하위 프로젝트 10 줄.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-06-enterprise-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
```

을 다음으로 바꾼다:

```markdown
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 원격 클러스터(상태 카드, 원격 VM 목록과 동작), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
```

`README.md`에서

```markdown
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다, 영역 로그인은 뒤에 ` realm=<영역>`), OIDC 콜백은 `realm=<영역> username=<이름>` 또는 `realm=<영역> error=<코드>`, 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
```

을 다음으로 바꾼다:

```markdown
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다, 영역 로그인은 뒤에 ` realm=<영역>`), OIDC 콜백은 `realm=<영역> username=<이름>` 또는 `realm=<영역> error=<코드>`, 원격 클러스터 중계는 `remote=<이름> <메서드> /<경로>`, 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
```

`README.md`에서

```markdown
  `max_file_mib`는 256까지이고, 읽을 때 파일 하나를 통째로 메모리에 올린다(한 번 읽기에 최대 `max_file_mib`쯤).
```

을 다음으로 바꾼다:

````markdown
  `max_file_mib`는 256까지이고, 읽을 때 파일 하나를 통째로 메모리에 올린다(한 번 읽기에 최대 `max_file_mib`쯤).

## 원격 클러스터

다른 opLite 클러스터를 원격(`Remote`)으로 등록하면 이 클러스터에서 그 클러스터의 상태(노드·VM 수, 버전, CPU·메모리)를
보고, 그 클러스터의 API를 이 클러스터를 거쳐 부를 수 있다(VM 목록, 시작·정지 등). 두 클러스터는 대등하다: 어느 쪽도
다른 쪽의 멤버가 아니고 Raft를 함께 쓰지 않는다. 서로 보려면 양쪽에서 따로 등록한다.

- **권한은 두 번.** 원격은 이 클러스터의 리소스라 로그인한 누구나(viewer 이상) 보고, admin만 등록·고치기·지우기·새로
  모으기를 한다(operator도 **403**). 원격 클러스터 쪽에서 할 수 있는 일은 등록할 때 넣은 그 클러스터의 API 토큰 주인의
  역할이 다시 정한다. 원격 클러스터에서 viewer 사용자로 만든 토큰을 등록하면 이 클러스터의 admin도 그곳에서는 읽기만
  한다(바꾸는 요청은 원격의 **403**이 그대로 온다). API 토큰은 만든 사용자의 역할을 그대로 가지므로, 원격에 필요한 만큼의
  역할(VM을 시작·정지하려면 보통 operator — admin 토큰은 쓰지 않는다)로 사용자를 따로 만들고 그 사용자로 토큰을 만든다.
- **지문으로 믿기.** 원격과의 TLS는 원격 클러스터의 CA 하나만 믿는다(시스템 루트는 쓰지 않는다). 그 CA는 등록할 때
  원격의 `GET /api/v1/cluster/ca`에서 받아 준 지문(`sha256:…`)과 같고 정확히 인증서 하나인지 확인한 뒤 원격과 함께
  저장한다(덧붙인 다른 CA를 믿게 되지 않게 둘 이상이면 받지 않는다). 그 뒤로 노드는 연결할 때 CA를 다시 받지 않고 저장한
  것만 믿는다(재시작한 노드도). 지문은 원격 클러스터 노드의 `oplctl cluster status`(`CA fingerprint:` 줄)나 웹 클러스터
  화면, `cluster init` 출력에서 확인한다. 호스트 이름은 확인하지 않는다(노드 인증서의 이름은 노드마다 다르므로 그 CA가
  서명했는지와 기간만 본다). 원격 클러스터의 CA가 바뀌면(클러스터를 새로 만듦) `oplctl remote set <이름> --ca-fingerprint
  <새 지문> --token-file <파일>`로 고칠 때까지 닿지 않는다.
- **등록할 때의 확인**(이 순서, 하나라도 실패하면 저장하지 않는다): 입력 검사 **400**(`url`은 `https://<호스트>[:포트]` —
  경로·쿼리·조각·사용자 정보가 없고 끝 `/`는 뗀다, 지문은 대소문자와 `AB:CD:…` 꼴도 받아 `sha256:<소문자 64자>`로, 토큰은
  `opl_<id>_<비밀>` 모양, 이름은 리소스 이름 규칙이고 `probe`는 쓸 수 없다) → 롤링 업그레이드 중이면 **409**
  `cluster_upgrading` → 원격의 CA를 받지 못하면 **502** `remote_unreachable` → 지문이 다르면 **400**
  `remote_fingerprint_mismatch`(메시지에 받은 지문) → 받은 CA가 이 클러스터의 CA면 **400** `remote_is_local` → 그 CA로
  연결해 토큰으로 원격의 `GET /api/v1/auth/whoami`를 읽어 **401**이면 **400** `remote_unauthorized`(그 밖의 실패는 **502**
  `remote_unreachable`) → 저장하고 곧 상태를 한 번 모아(10초 제한) 응답에 넣는다. CA 받기부터 whoami까지 모두 30초 안에
  끝나야 한다(넘으면 **502** `remote_unreachable` "timed out checking the remote cluster" — `probe`의 CA 받기도 같다).
  검증 없이 CA를 받는 것(등록·`probe`)은 아무 주소에나 닿으므로 오류에 원격이 준 말을 옮기지 않는다: 2xx가 아니면 HTTP
  상태 코드만(`HTTP 404: Not Found`), 연결 실패는 `cannot connect`, TLS 실패는 `TLS handshake failed`, 2xx지만 PEM
  인증서가 아니면 `not an opLite cluster`.
- **토큰은 나가지 않는다.** 응답에는 토큰 대신 토큰의 id(`token_id` — 원격의 `oplctl token list`·API 토큰 화면에서 어느
  토큰인지 알아보게)만 있고, 원격은 watch로도 내보내지 않는다(웹은 10초마다 다시 읽는다). 바꾸기(`PUT`)에서 `token`을 빼면
  `url`·`ca_fingerprint`(정규화한 값)가 모두 그대로일 때만 저장된 토큰을 두고, 하나라도 바뀌면 **400** "token must be given
  again when url or ca_fingerprint changes"다(저장된 토큰을 다른 곳으로 보내지 않는다). `url`이나 `ca_fingerprint`가 바뀌면
  등록 때와 같은 확인(CA를 다시 받아 저장)을 하고, 토큰만 바뀌면 저장한 CA로 토큰만 확인한다. 이름은 바꿀 수 없다.
  `If-Match`가 다르면 **412**지만, 확인하는 사이에 상태만 바뀌었으면(상태 수집이 revision을 올림) 다시 확인하지 않고 새
  revision에 쓴다 — **412**는 그 사이 spec(주소·지문·토큰)이 바뀐 때만이다. 원격을 지워도 원격 클러스터의 토큰은 남으므로
  그곳에서 따로 지운다.
- **노드에서 원격으로.** 원격에 연결하는 것은 브라우저가 아니라 이 클러스터의 노드다(상태는 리더가, 중계는 요청을 받은
  노드가). 모든 노드가 원격 URL에 닿아야 한다. 연결은 요청마다 새로 열고(HTTP/1.1), 연결·TLS 10초, 요청을 다 보낸 뒤
  응답 헤더 10초, 본문 조각 사이 60초 제한이며 리다이렉트는 따르지 않는다. CA는 등록 때 저장한 것을 쓴다.

```bash
# 원격 클러스터(dr)에서: 이 클러스터가 쓸 사용자(읽기만 시키려면 --role viewer)
oplctl user create prod-link --role operator
# 그 사용자로 로그인해 API 토큰을 만든다(웹이면 그 사용자로 로그인해 API 토큰 화면에서). 토큰은 다시 볼 수 없다.
oplctl login --server https://10.0.1.11:8006 --ca-fingerprint sha256:… --user prod-link
oplctl token create prod > /root/dr-token      # 이 파일을 prod 쪽으로 옮긴다
oplctl cluster status                          # CA fingerprint: sha256:… — 아래에서 보이는 지문과 비교한다

# 이 클러스터(prod)에서(admin)
oplctl remote add dr --url https://10.0.1.11:8006 --token-file /root/dr-token
# the remote cluster CA fingerprint is sha256:3f9a…
# Is this the CA of the remote cluster? [y/N] y
# remote dr added (reachable)
oplctl remote add dr --url https://10.0.1.11:8006 --ca-fingerprint sha256:3f9a… --token-file -   # 지문을 미리 알면
oplctl remote list
# NAME  CLUSTER  VERSION  NODES  VMS    STATUS
# dr    dr       0.37.0   3/3    12/15  reachable
oplctl remote show dr       # url·ca fingerprint·token id·status·changed at·cluster·version·nodes·vms·cpus·memory
oplctl remote refresh dr    # 곧 다시 모아 show처럼 보인다(admin)
oplctl remote vms dr        # 원격의 VM 목록(vm list와 같은 표)
oplctl remote vm dr start web          # 시작
oplctl remote vm dr stop web           # ACPI 종료(로컬 vm stop과 같음)
oplctl remote vm dr stop web --force   # 곧바로 끄기(로컬 vm stop --force와 같음)
oplctl remote set dr --url https://10.0.1.12:8006 --token-file /root/dr-token   # 주소·지문을 바꾸면 토큰도 다시
oplctl remote set dr --token-file /root/new-token    # 새 토큰만(주지 않은 것은 그대로, 읽은 revision으로 — 그 사이 다른 admin이 바꿨으면 412)
oplctl remote remove dr
```

- **oplctl.** `remote add`에 `--ca-fingerprint`가 없으면 oplctl이 직접(인증서를 검증하지 않고) 원격의 CA를 받아 지문을 보이고,
  `--yes`면 그대로 믿고, 터미널이면 `[y/N]`로 묻고(`y`·`yes`만 계속), 터미널이 아니면(스크립트) "check the fingerprint
  (`oplctl cluster status` on a node of the remote cluster) and run again with --ca-fingerprint sha256:…"로 끝난다(종료 1).
  `--token-file -`이면 표준 입력에서 읽고 앞뒤 공백을 뗀다 — 그때는 표준 입력으로 지문 확인을 물을 수 없으므로
  `--ca-fingerprint`나 `--yes`가 없으면 토큰을 읽거나 원격에 연결하기 전에 "reading the token from stdin needs
  --ca-fingerprint or --yes"로 끝난다. `remote vm`은 원격의 Task를 기다리지 않고 "vm web on remote dr: stop requested (task
  <id>)"(`--force`면 `force stop requested`)만 보인다(결과는 `remote vms`로). `-o json`이면 API 응답 그대로다.
- **상태 수집.** 리더가 5초마다(`tick`) 모을 때가 된 원격을 찾아, 원격마다 30초마다(`interval`) 원격의 `GET /cluster`,
  `/nodes`, `/vms`를 읽는다(원격들을 동시에, 각각 10초 제한 — 그래서 원격 토큰은 적어도 viewer여야 한다). 상태는
  `cluster_name`, `version`(위트니스를 포함한 노드 버전 가운데 가장 낮은 것), `nodes{total, online}`·`cpus`·`memory_gib`
  (위트니스를 뺀 노드), `vms{total, running}`이다. 읽지 못하면 `reachable: false`와 `error`(이유)만 남고 나머지는 비운다.
  모은 것이 저장된 것과 같으면 쓰지 않는다(Raft 부하) — 그래서 `checked_at`은 마지막 확인 시각이 아니라 상태가 마지막으로
  **바뀐** 시각이다(oplctl `changed at`). 리더가 바뀌면 새 리더가 곧 모은다. `oplctl remote refresh`(웹 "새로고침", API
  `POST /remotes/{key}/actions/refresh`)는 기다리지 않고 지금 모은다(10초 제한). 모은 상태를 적지 못하면(그 사이 수집기가
  먼저 적음) 모은 것 대신 저장된 원격을 돌려준다(등록·바꾸기의 응답도 같다).
- **중계.** `/api/v1/remotes/{key}/api/{*path}`(모든 메서드)는 그 원격의 `/api/v1/{path}`로 메서드·쿼리·본문을 저장한
  토큰(`Authorization: Bearer`)으로 보내고, 상태와 본문을 그대로 흘려 돌려준다(본문 한도 없음 — 업로드·내려받기도 된다).
  - 권한: 아래 허용 목록의 경로를 `GET`·`HEAD`로 읽는 것은 원격을 읽을 수 있는 누구나(viewer 이상), 그 밖은 메서드와
    관계없이 이 클러스터의 admin이다. 그다음 원격이 토큰 주인의 역할로 다시 확인한다. 원격 토큰이 읽을 수 있더라도 사용자·
    토큰·인증 영역·감사 로그·텔레메트리처럼 계정과 보안에 관한 것과 원격의 원격(`remotes` — 중계 사슬)은 이 클러스터의
    viewer·operator가 읽지 못하게 한다. 첫 조각은 퍼센트 인코딩을 풀어 본다(`%75sers`도 `users`).

    | 경로(`{*path}`) | `GET`·`HEAD` | 그 밖의 메서드 |
    |---|---|---|
    | 첫 조각이 `vms`·`volumes`·`storage-pools`·`networks`·`security-groups`·`snapshots`·`backups`·`backup-jobs`·`vm-imports`·`nodes`·`tasks`·`cluster`·`version`·`settings`·`subscription`, 또는 정확히 `auth/whoami` | viewer 이상 | admin |
    | 그 밖(`users`·`tokens`·`realms`·`audit`·`telemetry`·`remotes`·다른 `auth/*`·모르는 조각) | admin | admin |

  - 보내는 헤더는 `Content-Type`·`Content-Length`·`If-Match`·`Accept`뿐이다(쿠키·`Origin`·원래 `Authorization` 등은 보내지
    않고 `Host`는 원격 주소). 돌려주는 헤더는 `Content-Type`·`Content-Length`·`ETag`·`Retry-After`·`Content-Disposition`뿐이고,
    원격이 보낸 HTML·SVG 따위가 이 클러스터 출처의 페이지로 실행되지 않게 `Content-Type`은 `application/json`(매개변수
    허용)일 때만 그대로, 그 밖(없음 포함)은 `application/octet-stream`으로 하며 늘 `X-Content-Type-Options: nosniff`와
    `Content-Security-Policy: default-src 'none'; sandbox`를 붙인다.
  - 경로 조각에 `.`·`..`·빈 조각이 있으면(퍼센트 인코딩 `%2e` 등도) **400** `invalid_request`, `watch`로 시작하는 경로와 VM
    콘솔(`vms/<VM>/console`, WebSocket)은 **404** `remote_path_unsupported`, 원격에 닿지 못하면(연결·TLS·시간 제한) **502**
    `remote_unreachable`이다. 원격이 저장된 토큰을 거절하면(**401** — 원격에서 토큰을 지움 등) 본문을 버리고 **502**
    `remote_unauthorized` "the remote cluster refused the stored token; update it with `oplctl remote set --token-file`"다
    (이 클러스터의 로그인 문제로 보이지 않게 — oplctl은 이 메시지를 그대로 보이고 웹은 로그아웃하지 않는다). 원격의 그 밖의
    오류 응답(**403** 포함 **4xx**·**5xx**)은 그대로 온다.
  - 감사: 중계한 바꾸는 요청은 이 클러스터의 감사 로그에 `action` `"<METHOD> /remotes/{key}/api/{*path}"`, `target`
    `remotes/<키>`, `detail` `remote=<이름> <METHOD> /<경로>`로 남고, 원격 클러스터의 감사 로그에는 토큰 주인의 요청으로 남는다.
  - 중계는 API 문서(OpenAPI)에 넣지 않는다(원격 API가 그대로 보이므로). 원격 쪽 API는 원격의 `openapi.json`을 본다.
- **웹.** "원격 클러스터" 메뉴(모두에게): 원격마다 카드(이름, 연결됨·연결 안 됨, URL, 클러스터 이름, 버전, 노드
  online/total, VM running/total, CPU, 메모리, 닿지 않으면 오류 문구), 10초마다 다시 읽는다. admin은 추가(이름, URL을 넣고
  "CA 받기" — 브라우저는 다른 출처를 읽지 못하므로 노드가 받은 지문을 보인다, 지문 확인 체크, API 토큰), 고치기(토큰을
  비우면 그대로, URL·CA가 바뀌면 다시 넣으라고 알림), 지우기, 새로고침을 한다. 카드의 이름을 누르면 그 원격의 VM 목록
  (중계 `GET /vms`, 10초마다)이고, admin은 VM마다 시작·정지(ACPI)·강제 정지(확인 뒤)를 요청한다(oplctl `remote vm
  start|stop [--force]`와 같은 이름, 원격 쪽 Task는 지켜보지 않고 요청했다고만 알린다). 중계의 **401**은 이 클러스터의
  세션이 끝난 것이므로 다른 화면처럼 로그인으로 돌아간다.
- **API.** `GET`·`POST /api/v1/remotes`, `GET`·`PUT`(`If-Match` 선택)·`DELETE /api/v1/remotes/{key}`(본문
  `{"name","url","ca_fingerprint","token"}` — `PUT`의 `name`은 생략하거나 지금 이름), `POST /api/v1/remotes/probe {"url"}`
  (admin — 인증서를 검증하지 않고 CA를 받아 정규화한 `url`과 `ca_fingerprint`를 돌려준다, 받지 못하거나 30초를 넘으면
  **502** `remote_unreachable`),
  `POST /api/v1/remotes/{key}/actions/refresh`(admin), 그리고 위의 중계. 응답은 `{"id","name","revision","url",
  "ca_fingerprint","token_id","status":{"reachable","checked_at","error","cluster_name","version","nodes":{"total","online"},
  "vms":{"total","running"},"cpus","memory_gib"}}`이다.

설정(노드마다, 생략하면 아래 기본값 — 리더인 노드의 값을 쓴다):

```toml
[remotes]
interval = "30s"   # 원격마다 상태를 모으는 간격
tick = "5s"        # 모을 때가 된 원격을 찾는 간격
```

- **한계.**
  - 원격 토큰은 Raft에 평문으로 있다(구독 키·LDAP 서비스 계정 비밀번호·OIDC 클라이언트 비밀과 같음 — Raft 데이터는 노드의
    root만 읽는다). 위트니스 노드와 데이터 디렉터리 백업에도 그대로 있다. 이 클러스터의 노드 하나(위트니스 포함)나 그
    백업을 가진 사람은 원격 클러스터를 그 토큰의 역할로 다룰 수 있으므로, 원격에는 필요한 만큼의 역할(보통 operator)
    사용자의 토큰을 등록한다.
  - watch(실시간 갱신)와 VM 콘솔은 중계하지 않는다. 원격 VM은 10초마다 다시 읽어 보이고, 콘솔은 원격 클러스터에 직접 연다.
  - 원격 URL은 노드 하나의 주소다. 그 노드가 꺼지면 원격의 다른 노드가 살아 있어도 `reachable: false`이므로 주소를 바꾼다
    (`remote set --url` — 토큰도 다시).
  - 원격 클러스터의 CA가 바뀌면(클러스터를 새로 만듦) `remote set --ca-fingerprint <새 지문> --token-file`로 고칠 때까지
    닿지 않는다(노드는 저장한 CA만 믿는다).
  - `checked_at`은 상태가 바뀐 시각이라 "마지막 확인"을 알 수 없다(`reachable`·`error`만 믿는다). 원격 Task(시작·정지의
    결과)는 지켜보지 않는다. 원격 쪽 감사 기록은 토큰 주인으로 남아 이 클러스터의 누가 요청했는지는 이 클러스터의 감사
    로그에만 있다.
  - 웹의 고치기는 `If-Match` 없이 보낸다(상태 수집이 revision을 올리므로) — 두 admin이 동시에 고치면 나중 것이 남는다.
  - 중계는 원격의 **401**을 모두 **502** `remote_unauthorized`로 바꾸므로, 토큰 거절이 아닌 401(중계한 `auth/login`에 틀린
    비밀번호 따위)도 그렇게 보인다. `auth/login`은 허용 목록 밖이라 이 클러스터의 admin만 닿으므로 받아들인 한계다.
````

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO: `Realm` 종류 `oidc`(issuer·client_id·client_secret(응답·watch에 없음, 공개 클라이언트도 됨)·scope·이름·그룹 클레임·CA), reqwest와 ring으로 직접 만든 인가 코드 흐름(PKCE S256, state·nonce·검증자를 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한 쿠키 `opl_oidc`에 담아 어느 노드나 콜백을 받음), discovery·JWKS 노드 캐시와 모르는 kid 다시 받기, ID 토큰 RS256·ES256 확인과 iss·aud·azp·exp·iat·nbf·nonce, 인가 응답 iss(RFC 9207), userinfo로 빠진 클레임, 그룹 클레임으로 역할과 계획 35의 외부 사용자(이메일 모양 이름, IdP 사용자 sub에 묶음), `/login?error=` 코드와 콜백 감사(시도 제한 없음), IdP 시험 동작, `oplctl realm add-oidc|set-oidc`·`login --token-file`, 웹 로그인 SSO 버튼과 OIDC 영역 화면, 가짜 IdP 시험, 완료), 계획 37(원격 클러스터), 계획 38(원격 복제·DR)로 나눴다(`2026-11-06-enterprise-design.md`). 계획 37·38은 남았다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO: `Realm` 종류 `oidc`(issuer·client_id·client_secret(응답·watch에 없음, 공개 클라이언트도 됨)·scope·이름·그룹 클레임·CA), reqwest와 ring으로 직접 만든 인가 코드 흐름(PKCE S256, state·nonce·검증자를 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한 쿠키 `opl_oidc`에 담아 어느 노드나 콜백을 받음), discovery·JWKS 노드 캐시와 모르는 kid 다시 받기, ID 토큰 RS256·ES256 확인과 iss·aud·azp·exp·iat·nbf·nonce, 인가 응답 iss(RFC 9207), userinfo로 빠진 클레임, 그룹 클레임으로 역할과 계획 35의 외부 사용자(이메일 모양 이름, IdP 사용자 sub에 묶음), `/login?error=` 코드와 콜백 감사(시도 제한 없음), IdP 시험 동작, `oplctl realm add-oidc|set-oidc`·`login --token-file`, 웹 로그인 SSO 버튼과 OIDC 영역 화면, 가짜 IdP 시험, 완료), 계획 37(원격 클러스터: 새 리소스 `Remote`(URL·CA 지문·원격 클러스터의 API 토큰 — 토큰은 응답·watch에 없고 id만 보임), 원격 CA 하나만 믿고 호스트 이름은 보지 않는 hyper·rustls 클라이언트(CA는 검증 없이 받아 지문으로 확인, 노드 메모리 캐시), 등록 때 지문·자기 클러스터·토큰(`whoami`) 확인과 `remote_*` 오류, 웹용 `probe`, 리더의 `RemotePoller`가 원격의 `/cluster`·`/nodes`·`/vms`로 상태를 모아 바뀔 때만 씀(opld `[remotes]`), API 중계 `/remotes/{key}/api/{*path}`(GET은 viewer 이상·나머지 admin, 원격 쪽은 토큰 주인의 역할, watch·콘솔 404, 스트리밍, OpenAPI 밖, 감사 detail), `oplctl remote`, 웹 `/remotes` 카드와 원격 VM 목록·동작, 두 `Env`(HTTPS)·opld 두 클러스터 E2E 시험, 완료), 계획 38(원격 복제·DR)로 나눴다(`2026-11-06-enterprise-design.md`). 계획 38은 남았다.
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown

## 6. 원격 복제 (계획 38)
```

을 다음으로 바꾼다:

```markdown

### 5.6 구현(계획 37)

- 타입(`opl_types::remote`): `Remote = Resource<RemoteSpec, RemoteStatus>`, `Kind::Remote`(`"remote"`). `RemoteSpec{url, ca_fingerprint, token, ca_pem}`(`deny_unknown_fields`, 손으로 쓴 `Debug`가 `token`을 가리고 `ca_pem`은 뺌). `ca_pem`은 등록·바꾸기 때 서버가 받아 지문을 확인한 원격 CA 인증서(PEM, 정확히 하나)로, 노드는 연결할 때 이것만 믿는다(설계 §5.1과 계획 37 스테이지 결정의 "CA PEM은 spec에 두지 않고 연결할 때마다 `fetch_ca`·노드 캐시"에서 바꿈 — 리뷰: 검증 없는 CA 받기를 등록 때 한 번으로). `normalize()`가 `normalize_url`(`https://<호스트>[:포트]` — 경로는 없거나 `/`, 쿼리·조각·사용자 정보·공백 없음, 포트 1~65535, IPv6는 `[주소]`, 앞뒤 공백과 끝 `/`를 뗌), `normalize_ca_fingerprint`(접두 `sha256:`는 선택·대소문자 무시, `AB:CD:…` 구분을 받아 `sha256:<소문자 64자>`), `token_id`(`opl_<uuid>_<공백 없는 ASCII 비밀>`의 id)를 이 순서로 검사하고 `RemoteSpecError{Url, Fingerprint, Token}`. `RemoteStatus{reachable, checked_at, error, cluster_name, version, nodes: NodeCounts{total, online}, vms: VmCounts{total, running}, cpus, memory_gib}`(`serde(default)`). 두 Counts는 API 문서에서 텔레메트리 보고의 같은 이름 스키마에 덮이지 않게 `RemoteNodeCounts`·`RemoteVmCounts`로 이름을 줌.
- 원격 클라이언트(`opl_api::remote_client`, pub; opl-api가 이미 쓰던 hyper·hyper-util·http-body-util·rustls·tokio-rustls — 새 패키지 없음): `fetch_ca(url)`(인증서를 검증하지 않는 `AnyCert` — 핸드셰이크 서명은 확인 — 로 `GET /api/v1/cluster/ca`, 2xx·64 KiB·정확히 인증서 하나인 PEM 확인; 지문 비교는 부르는 쪽. 아무 주소에나 연결하므로 원격이 준 말을 옮기지 않음: 2xx가 아니면 `Status(코드, 상태 이름)`, 연결 실패는 `Unreachable("cannot connect")`(시간 제한 문구는 그대로), TLS는 `Tls("TLS handshake failed")`, PEM 인증서가 아니면 `Invalid("… not an opLite cluster …")`), `RemoteClient::new(url, ca_pem, token)`(정확히 인증서 하나인 그 CA만 루트로 한 — 둘 이상이면 `Invalid`, 첫 인증서의 지문만 맞고 덧붙인 CA가 서명한 서버를 믿게 되지 않게 — `WebPkiServerVerifier`를 감싼 `AnyName`이 `CertificateError::NotValidForName`·`NotValidForNameContext`만 받아들임 — 서명·기간은 그대로 확인; IP 주소면 SNI 없음), `get_json(path)`(Accept JSON, 2xx가 아니면 `Status`, 4 MiB), `request(method, path_and_query, headers, body)`(`Host`·`Authorization: Bearer`를 채우고 상태와 관계없이 응답을 그대로 `Response<RemoteBody>`로; 보내고 받는 본문을 흘려 보내며 조각 사이 60초 `IdleTimeout`). 경로는 `/api/v1` 아래 상대, 요청마다 새 HTTP/1.1 연결, TCP·TLS 10초, 본문을 다 보낸 뒤 응답 헤더 10초, 리다이렉트를 따르지 않음(3xx 그대로). `RemoteError{Unreachable, Tls, Status(코드, 오류 본문의 message 또는 상태 이름 — `fetch_ca`는 상태 이름만), Invalid}`. 노드 메모리의 CA 캐시는 없다(spec의 `ca_pem`으로 `RemoteClient::new`).
- API(`opl_api::remotes`): `GET`·`POST /remotes`, `GET`·`PUT`(If-Match 선택 — 없으면 읽은 revision으로 쓰고 그 사이 바뀌면 다시 읽어 10번까지; 확인을 마친 뒤 쓸 때 revision이 달라도 저장된 spec이 확인 전에 읽은 것과 같으면(상태만 바뀜 — 수집기 따위) 다시 확인하지 않고 새 revision에 씀 — If-Match가 맞았던 요청도 200, 412는 그 사이 spec이 바뀐 때만)·`DELETE /remotes/{key}`. 읽기는 `Action::Read`(viewer 이상), 쓰기는 `Action::Write`(admin — `RoleAuthorizer`는 그대로, operator 목록에 Remote 없음), 쓰기 전 `require_current`. 본문 `RemoteRequest{name?, url, ca_fingerprint, token?}`(`deny_unknown_fields`, `Debug`가 token을 가림). 확인 순서: 검증 400(url·지문 정규화, token 모양, 이름 규칙, 이름 `probe` 거절 — `/remotes/probe`와 겹침) → 옛 노드 409 → `fetch_ca` 실패 502 `remote_unreachable` → 지문 다름 400 `remote_fingerprint_mismatch`(받은 지문을 메시지에) → 자기 클러스터 CA 400 `remote_is_local` → 받은 CA를 `spec.ca_pem`에 두고 그 CA로 `GET /auth/whoami`가 401이면 400 `remote_unauthorized`(그 밖 502) → 저장하고 곧 상태를 모아 씀. 확인 전체(`verify`)는 30초(`VERIFY_TIMEOUT`, 넘으면 502 `remote_unreachable` "timed out checking the remote cluster"), 만들기·바꾸기 직후의 상태 모으기와 refresh는 `COLLECT_TIMEOUT`(10초 — 넘으면 `reachable: false`). 그 상태를 적지 못하면(그 사이 수집기가 써 revision이 다름) 모은 기본값이 아니라 저장된 행을 다시 읽어 돌려줌(`recorded`). `PUT`은 통째로 바꾸고, `token`을 빼면 정규화한 url·ca_fingerprint가 모두 그대로일 때만 저장된 토큰을 둠(아니면 400 "token must be given again when url or ca_fingerprint changes"), url·ca_fingerprint가 그대로면 저장한 `ca_pem`도 두고 토큰만 확인하며(CA를 다시 받지 않음) 바뀌면 CA를 다시 받아 확인·저장, 이름은 바꾸지 못함, 같은 spec이면 쓰지 않음(확인도 안 함). 응답 `RemoteView{id, name, revision, url, ca_fingerprint, token_id, status}`(token·ca_pem 없음), `WATCH_KINDS`에 넣지 않음. 설계에 없던 끝점 둘: `POST /remotes/probe {url}`(admin — 웹이 등록 전에 지문을 보여 주려고: 브라우저는 다른 출처의 CA를 읽지 못함; 정규화한 `url`과 `ca_fingerprint`, 실패 502 — CA 받기도 `VERIFY_TIMEOUT`(30초, 넘으면 502 `remote_unreachable` "timed out checking the remote cluster")), `POST /remotes/{key}/actions/refresh`(admin — 곧 다시 모아 돌려줌).
- 상태 수집(`RemotePoller`·`RemotePollerConfig{tick, interval}`, `TelemetryReporter` 모양): 모든 노드에서 띄우고 리더만, `require_current`·`read_barrier` 뒤, `tick`마다 리더 메모리에 둔 마지막 수집 시각이 `interval`을 지난 원격을 동시에(각 10초 제한). `GET /cluster`·`/nodes`·`/vms`에서 필요한 필드만 읽어(원격 버전이 달라도 읽히게) cluster_name, version(위트니스를 포함한 노드 가운데 `opl_types::version::compare`로 가장 낮은 것), nodes total/online(위트니스 제외, online은 노드 status의 `online`)·cpus·memory_gib(위트니스 제외, MiB 합 / 1024), vms total/running(phase `running`). 실패면 `reachable: false`와 `error`만(나머지는 비움). 저장된 것과(`checked_at` 빼고) 같으면 쓰지 않고, 다르면 `checked_at`을 지금으로 `update_status_checked`(그 사이 바뀌었거나 지워졌으면 건너뜀). 리더가 바뀌면 기록을 비워 곧 모음.
- 중계(`opl_api::remote_proxy`): `/remotes/{key}/api/{*path}`를 `axum::routing::any`로 — **API 문서(OpenAPI)에 넣지 않음**(원격 API를 그대로 비추는 경로라 스키마가 없음; 웹은 업로드처럼 일반 `fetch`로 부름). 경로는 요청 URI에서 퍼센트 인코딩 그대로 잘라 조각마다 디코드한다. 권한: GET·HEAD이고 첫 조각이 허용 목록(`READABLE` — `vms`·`volumes`·`storage-pools`·`networks`·`security-groups`·`snapshots`·`backups`·`backup-jobs`·`vm-imports`·`nodes`·`tasks`·`cluster`·`version`·`settings`·`subscription`)이거나 경로가 정확히 `auth/whoami`면 `Action::Read`, 그 밖(`users`·`tokens`·`realms`·`audit`·`telemetry`·`remotes`(중계 사슬)·다른 `auth/*`·모르는 조각, 모든 쓰기)은 `Action::Write`(admin) — 설계 §5.3의 "GET은 누구나"에서 바꿈(리뷰: 원격 토큰이 admin이면 이 클러스터의 viewer가 원격의 사용자·토큰·감사 로그를 읽게 됨). 그다음 `.`·`..`·빈 조각이면 400, 첫 조각 `watch` 또는 `vms/<x>/console`(세 조각)이면 404 `remote_path_unsupported`, 원격을 찾고(404 `not_found`), 쿼리를 붙여 spec의 `ca_pem`으로 만든 클라이언트로 보냄(연결 실패 502 `remote_unreachable`). 보내는 헤더 `Content-Type`·`Content-Length`·`If-Match`·`Accept`, 돌려주는 헤더 `Content-Length`·`ETag`·`Retry-After`·`Content-Disposition`과 `Content-Type`(`application/json`(매개변수 허용)일 때만 그대로, 그 밖·없음은 `application/octet-stream`), 늘 `X-Content-Type-Options: nosniff`·`Content-Security-Policy: default-src 'none'; sandbox`(원격 본문이 이 출처의 페이지로 실행되지 않게). 원격의 401은 본문을 버리고 502 `remote_unauthorized` "the remote cluster refused the stored token; update it with `oplctl remote set --token-file`"(이 클러스터의 로그인 문제로 오해되지 않게 — 토큰 거절이 아닌 401, 예를 들어 중계한 `auth/login`의 틀린 비밀번호도 같게 보이지만 `auth/login`은 허용 목록 밖이라 admin만 닿으므로 받아들인 한계), 403 등 그 밖은 그대로. 감사는 설계의 `action: remote.proxy`가 아니라 계획 34 미들웨어 그대로(action `"<METHOD> /remotes/{key}/api/{*path}"`, target `remotes/<키>`, path는 실제 경로)이고 핸들러가 `AuditNote`로 detail `remote=<이름> <METHOD> /<경로>`. `vm_imports::esxi::percent_decode`를 crate 안에서 같이 씀.
- opld: `[remotes] interval`(기본 `"30s"`)·`tick`(기본 `"5s"`, `deny_unknown_fields`, 0 거절), `Services`에 `RemotePoller`(멈출 때 shutdown).
- oplctl: `remote list`(NAME·CLUSTER·VERSION·NODES online/total·VMS running/total·STATUS `reachable`|`unreachable: <이유>`), `show`(`키: 값` 줄 — token id, `changed at`(= `checked_at`), CPU·메모리), `add <이름> --url --token-file <파일|-> [--ca-fingerprint] [--yes]`(지문이 없으면 CLI가 직접 `fetch_ca`(client.rs의 것)로 받아 보이고 `--yes`·터미널 `[y/N]`·아니면 `--ca-fingerprint`로 다시 실행하라고 실패), `set <이름> [--url] [--ca-fingerprint] [--token-file]`(읽은 원격에 준 것만 바꿔 If-Match `PUT`, 토큰을 주지 않으면 빼서 옮기기 규칙은 서버가), `remove`, `refresh`, `vms <이름>`(중계 `GET /vms`, `vm list`와 같은 `VM_HEADERS` 표), `vm <이름> start|stop <VM> [--force]`(로컬 `vm stop`과 같은 이름 — `stop`은 ACPI `stop`, `--force`면 `stop?mode=force` 곧바로 끄기, `start --force`는 오류; 설계 §5.4의 `start|stop|shutdown`에서 바꿈; 원격 Task를 기다리지 않고 id만 보임). `add --token-file -`에 `--ca-fingerprint`도 `--yes`도 없으면 표준 입력을 읽거나 연결하기 전에 "reading the token from stdin needs --ca-fingerprint or --yes"(토큰을 읽은 표준 입력으로 확인을 물을 수 없음). 중계의 원격 거절은 502 `remote_unauthorized`라 서버 메시지를 그대로 보임(login 안내 없음).
- 웹(`web/src/remotes/`): 메뉴 `nav.remotes`(모두에게), `/remotes` 카드(이름 링크, 연결됨·연결 안 됨, URL, 클러스터 이름, 버전, 노드, VM, CPU, 메모리 GiB, 오류 문구, 10초마다 다시 읽음)와 admin 추가(URL → "CA 받기"(`probe`)가 지문을 보이고 URL을 정규화한 값으로 바꿈, 지문 확인 체크, 토큰)·고치기(저장된 URL·지문으로 시작, URL을 바꾸면 CA를 다시 받아 확인, 토큰을 비우면 빼서 보내되 URL·지문이 바뀌면 다시 요구, 상태 수집이 revision을 올리므로 If-Match 없이 `PUT`)·지우기·새로고침. `/remotes/<이름>`: 중계 `GET /vms`를 `proxy<T>()`(일반 `fetch`)로 10초마다, admin 동작 시작·정지(ACPI `stop`)·강제 정지(`stop?mode=force`, 확인 뒤), 상태로 막힌 동작은 로컬 VM과 같은 이유로 막음, 원격 Task는 지켜보지 않음. 중계 응답의 401은 이 클러스터의 세션 끝이므로 업로드처럼 `reportUnauthorized`(보낼 때의 인증 세대)로 알림(원격 거절은 502 `remote_unauthorized`라 기존 오류 문구). 추가 대화상자의 CA 지문은 `<dl>`(dt·dd). 오류 문구 `remote_unreachable`·`remote_fingerprint_mismatch`·`remote_is_local`·`remote_unauthorized`, ko·en.
- 시험: opl-types 단위(검증·정규화·JSON(`ca_pem` 필수)·`Debug` 가림); `tests/remote_client.rs`(시험 CA가 `node-x` 이름으로 서명한 127.0.0.1 HTTPS 서버 — 이름이 달라도 연결, 다른 CA 거절, `fetch_ca`는 검증 없이 받음과 한도, CA 응답에 두 번째 CA를 붙이면 `fetch_ca`·`RemoteClient::new` 거절(덧붙인 CA가 서명한 서버에 연결하지 않음), `fetch_ca` 오류 문구(404 본문의 message 없음·TLS 아닌 서버·닫힌 포트·PEM 아님), JSON 한도와 오류 메시지, 3xx 그대로, 양방향 스트리밍 왕복, TLS·응답 헤더 멈춤 10초); 공통 `Env::serve_tls()`(같은 라우터를 그 클러스터 CA가 `node-x`로 서명한 잎으로 HTTPS 서비스, `GET /cluster/ca` 수를 `ca_hits`로 셈)·`Env::ca_fingerprint()`·`Env::ca_pem()`으로 두 `Env`를 쓰는 `tests/remotes.rs`(등록과 상태·정규화·token·PEM 없음·CA 저장·수집이 CA를 다시 받지 않음, 확인 실패 코드들과 저장 안 됨, 1초에 한 바이트씩 CA를 보내는 서버로 확인·probe 30초 마감 502, 확인하는 사이에 상태 쓰기를 끼워 넣어도 If-Match PUT 200·spec 쓰기면 412(whoami에 답하기 전에 쓰는 시험 서버), PUT token 규칙·주소가 바뀔 때만 CA를 다시 받음·이름·If-Match, viewer·operator 읽기 200·쓰기 403, 옛 노드 409, probe, refresh와 수집기가 바뀔 때만 씀·원격이 꺼지면 reachable false, 중계 GET·VM 시작이 원격에서 실제로 바뀜·쿼리와 원격 오류 그대로, viewer·operator는 GET만, viewer는 허용 목록만(users·tokens·remotes 등·`%75sers`·HEAD users 403, HEAD vms 200), admin은 users·remotes도, 원격 viewer 토큰이면 원격의 403, 원격에서 토큰을 지우면 502 `remote_unauthorized`, 응답 헤더(JSON·octet-stream·nosniff·CSP), 중계·refresh가 CA를 다시 받지 않음, 날 요청줄로 `..`·`%2e%2E`·빈 조각 400과 `watch`·`%77atch`·console 404, 꺼진 원격 502, 감사 항목); 단위(요약 계산, 적기 전에 다른 상태 쓰기를 끼워 넣으면 저장된 것을 돌려줌, 대상이 바뀌면 `ca_pem`을 비움, 헤더 목록·JSON만 Content-Type 유지·허용 목록·경로 규칙); oplctl 파싱(`stop`·`stop --force`·`start --force` 오류)·지문 확인·표와 줄·fake_api 요청·표준 입력 토큰 확인 오류·502 `remote_unauthorized` 메시지; opld E2E `tests/remotes.rs`(한 노드 클러스터 둘, `[remotes]` 1초 — 자기 클러스터는 `remote_is_local`, dr에서 만든 토큰으로 `remote add --yes`, `remote list`의 노드 1/1·VM 0/1, `remote vms`·`remote vm stop`, dr을 멈추면 unreachable); 웹 Vitest·msw(카드·추가·지문 dl·고치기·지우기·새로고침, 원격 VM 목록과 동작, 중계 502 `remote_unauthorized`는 오류 문구, 중계 401은 `reportUnauthorized`, 메뉴·권한), openapi 시험(경로·스키마 이름).

## 6. 원격 복제 (계획 38)
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
| `external_user` | 400 | 외부 사용자의 비밀번호 바꾸기 |
| `invalid_request` | 400 | 모르는 영역으로 로그인, 영역 이름 `local`, 영역 검증 실패(계획 35); OIDC 영역으로 비밀번호 로그인, issuer·client_id·ca_pem을 바꾸며 `client_secret`을 뺌, LDAP 영역 시험에 사용자·비밀번호 없음, `Host` 없는 OIDC 시작(계획 36) |
| `sso_failed` | (리디렉트) | OIDC 로그인 실패: 없는·OIDC가 아닌 영역, 상태 쿠키(없음·서명·10분·state), IdP `error`, 인가 응답 `iss`(RFC 9207), 토큰 끝점 `invalid_grant`, userinfo 401·403, ID 토큰·클레임·이름 규칙·userinfo `sub`, 다른 IdP 사용자(sub)에 묶인 이름, 롤링 업그레이드 중. 콜백 실패는 HTTP 상태 대신 `/login?error=<sso_failed|no_role|realm_unavailable>`로 302 |
| `remote_fingerprint_mismatch` | 400 | 원격 CA 지문이 다름 |
| `remote_unauthorized` | 400 | 원격 토큰이 받아들여지지 않음 |
| `remote_is_local` | 400 | 자기 클러스터를 원격으로 |
| `remote_unreachable` | 502 | 중계·복제 중 원격에 닿지 못함 |
| `remote_path_unsupported` | 404 | watch·콘솔 중계 |
| `replica_exists` | 409 | 같은 출처의 사본이 이미 있음 |
```

을 다음으로 바꾼다:

```markdown
| `external_user` | 400 | 외부 사용자의 비밀번호 바꾸기 |
| `invalid_request` | 400 | 모르는 영역으로 로그인, 영역 이름 `local`, 영역 검증 실패(계획 35); OIDC 영역으로 비밀번호 로그인, issuer·client_id·ca_pem을 바꾸며 `client_secret`을 뺌, LDAP 영역 시험에 사용자·비밀번호 없음, `Host` 없는 OIDC 시작(계획 36); 원격의 url·지문·토큰 모양, 원격 이름 `probe`·이름 바꾸기, url·ca_fingerprint를 바꾸며 `token`을 뺌, 중계 경로의 `.`·`..`·빈 조각(계획 37) |
| `sso_failed` | (리디렉트) | OIDC 로그인 실패: 없는·OIDC가 아닌 영역, 상태 쿠키(없음·서명·10분·state), IdP `error`, 인가 응답 `iss`(RFC 9207), 토큰 끝점 `invalid_grant`, userinfo 401·403, ID 토큰·클레임·이름 규칙·userinfo `sub`, 다른 IdP 사용자(sub)에 묶인 이름, 롤링 업그레이드 중. 콜백 실패는 HTTP 상태 대신 `/login?error=<sso_failed|no_role|realm_unavailable>`로 302 |
| `remote_fingerprint_mismatch` | 400 | 원격 CA 지문이 다름(등록·바꾸기 — 메시지에 받은 지문) |
| `remote_unauthorized` | 400·502 | 원격 토큰이 받아들여지지 않음: 등록·바꾸기의 `GET /auth/whoami`가 401이면 400, 중계에서 원격이 저장된 토큰을 거절(401)하면 502(본문을 버림 — 계획 37) |
| `remote_is_local` | 400 | 자기 클러스터를 원격으로(받은 CA 지문이 이 클러스터의 것) |
| `remote_unreachable` | 502 | 중계·복제 중 원격에 닿지 못함; 등록·바꾸기·`probe`에서 CA를 받지 못함(CA가 인증서 하나가 아님 포함), whoami의 401이 아닌 실패, 확인 30초 넘음, 중계의 연결·TLS·시간 제한(계획 37) |
| `remote_path_unsupported` | 404 | watch·콘솔 중계(첫 조각 `watch`, `vms/<x>/console` — 퍼센트 인코딩을 풀어 봄) |
| `replica_exists` | 409 | 같은 출처의 사본이 이미 있음 |
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
- (계획 36) 리디렉트 URI는 `start` 요청의 `Host`로 만들고 scheme은 늘 https다(프록시 뒤면 프록시가 넘기는 Host). 로그아웃은 opLite 세션만 끝내고(IdP 로그아웃 없음), IdP의 계정·그룹 변화는 다음 로그인에 반영된다(이미 있는 세션 12시간·토큰은 남음). discovery·JWKS는 노드마다 10분 캐시라 IdP에서 지운 서명 키도 그동안 믿는다(영역을 고치면 새로 받음). 받기 실패는 15초 동안 기억하므로 IdP가 돌아와도 그동안은 같은 오류다. 토큰 끝점의 `invalid_grant`가 아닌 4xx(`invalid_client` 등 클라이언트 설정 잘못)도 `realm_unavailable`이라 IdP 장애와 같은 문구다(원인은 opld 로그). 롤링 업그레이드 중의 OIDC 로그인은 `sso_failed`(LDAP은 409). SSO 로그인은 늘 `/`로 돌아온다(`next`를 잃음). 외부 사용자는 처음 로그인한 IdP 사용자(`iss`·`sub`)에 묶이므로 IdP에서 계정을 다시 만들면(새 `sub`) 그 사용자의, 영역의 issuer를 바꾸거나 Entra ID에서 앱 등록을 바꾸면(새 `client_id` — Entra의 `sub`는 앱마다 다름) 모든 기존 사용자의 로그인이 `sso_failed`다(opLite에서 사용자를 지워야 함); `subject`가 없는 옛 사용자는 그 이름으로 처음 OIDC 로그인한 사람에게 묶인다. 이름 규칙(1~128자, 이메일 모양은 됨)에 맞지 않는 이름 클레임은 쓸 수 없고, 그룹은 클레임 값만 본다(Entra ID 그룹 초과 `_claim_names`는 그룹 없음). 클라이언트 인증은 `client_secret_basic`과 공개 클라이언트만, 서명은 RS256·ES256만이다. 클라이언트 비밀은 Raft에 평문이다. oplctl은 브라우저 SSO를 하지 않고 웹에서 만든 API 토큰을 `oplctl login --token-file`로 저장한다. OIDC 로그인은 시도 제한을 쓰지 않는다(맞힐 비밀이 없음). 시험 동작은 discovery·JWKS만 보고 클라이언트 비밀·리디렉트 URI·클레임은 확인하지 못한다.
- 원격 등록의 토큰은 Raft에 평문으로 있다(구독 키·바인드 비밀번호와 같음 — Raft 데이터는 노드의 루트만 읽음).
- 복제는 전체 백업을 매번 보낸다(증분 없음). 원본을 지울 때 원격에 닿지 못하면 사본이 남는다.
```

을 다음으로 바꾼다:

```markdown
- (계획 36) 리디렉트 URI는 `start` 요청의 `Host`로 만들고 scheme은 늘 https다(프록시 뒤면 프록시가 넘기는 Host). 로그아웃은 opLite 세션만 끝내고(IdP 로그아웃 없음), IdP의 계정·그룹 변화는 다음 로그인에 반영된다(이미 있는 세션 12시간·토큰은 남음). discovery·JWKS는 노드마다 10분 캐시라 IdP에서 지운 서명 키도 그동안 믿는다(영역을 고치면 새로 받음). 받기 실패는 15초 동안 기억하므로 IdP가 돌아와도 그동안은 같은 오류다. 토큰 끝점의 `invalid_grant`가 아닌 4xx(`invalid_client` 등 클라이언트 설정 잘못)도 `realm_unavailable`이라 IdP 장애와 같은 문구다(원인은 opld 로그). 롤링 업그레이드 중의 OIDC 로그인은 `sso_failed`(LDAP은 409). SSO 로그인은 늘 `/`로 돌아온다(`next`를 잃음). 외부 사용자는 처음 로그인한 IdP 사용자(`iss`·`sub`)에 묶이므로 IdP에서 계정을 다시 만들면(새 `sub`) 그 사용자의, 영역의 issuer를 바꾸거나 Entra ID에서 앱 등록을 바꾸면(새 `client_id` — Entra의 `sub`는 앱마다 다름) 모든 기존 사용자의 로그인이 `sso_failed`다(opLite에서 사용자를 지워야 함); `subject`가 없는 옛 사용자는 그 이름으로 처음 OIDC 로그인한 사람에게 묶인다. 이름 규칙(1~128자, 이메일 모양은 됨)에 맞지 않는 이름 클레임은 쓸 수 없고, 그룹은 클레임 값만 본다(Entra ID 그룹 초과 `_claim_names`는 그룹 없음). 클라이언트 인증은 `client_secret_basic`과 공개 클라이언트만, 서명은 RS256·ES256만이다. 클라이언트 비밀은 Raft에 평문이다. oplctl은 브라우저 SSO를 하지 않고 웹에서 만든 API 토큰을 `oplctl login --token-file`로 저장한다. OIDC 로그인은 시도 제한을 쓰지 않는다(맞힐 비밀이 없음). 시험 동작은 discovery·JWKS만 보고 클라이언트 비밀·리디렉트 URI·클레임은 확인하지 못한다.
- 원격 등록의 토큰은 Raft에 평문으로 있다(구독 키·바인드 비밀번호와 같음 — Raft 데이터는 노드의 루트만 읽음). 위트니스와 데이터 디렉터리 백업에도 있으므로 원격에는 필요한 만큼의 역할(보통 operator) 사용자의 토큰을 등록한다.
- (계획 37) 이 클러스터의 viewer·operator는 중계로 허용 목록의 자원(VM·볼륨·저장소·네트워크·보안 그룹·스냅숏·백업·가져오기·노드·Task·클러스터·버전·설정·구독과 `auth/whoami`)을 원격 토큰이 읽을 수 있는 만큼 읽는다(사용자·토큰·영역·감사·텔레메트리·원격의 원격은 admin만). 원격 URL은 노드 하나의 주소라 그 노드가 꺼지면 다른 노드가 살아 있어도 닿지 않는다(주소를 바꿈 — 토큰도 다시). 노드는 등록 때 저장한 CA만 믿으므로 원격 클러스터의 CA가 바뀌면 `remote set --ca-fingerprint`(토큰도 다시)로 고칠 때까지 닿지 않는다. `checked_at`은 상태가 바뀐 시각이라 마지막 확인 시각을 알 수 없다. watch·콘솔은 중계하지 않아 원격 VM은 10초마다 다시 읽고 원격 Task는 지켜보지 않는다. 원격 쪽 감사 기록은 토큰 주인으로 남는다(누가 요청했는지는 이 클러스터의 감사 로그에만). 원격을 지워도 원격 클러스터의 토큰은 남는다. 웹의 고치기는 If-Match 없이 보내 동시에 고치면 나중 것이 남는다. 수집 시각은 리더 메모리에만 있어 리더가 바뀌면 곧 다시 모은다.
- 복제는 전체 백업을 매번 보낸다(증분 없음). 원본을 지울 때 원격에 닿지 못하면 사본이 남는다.
```

- [ ] **Step 2: 확인**

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 원격 클러스터 — README 새 절 \"원격 클러스터\"(대등한 두 클러스터, 권한은 두 번 — 여기서는 읽기 viewer·바꾸기 admin, 원격 쪽은 토큰 주인의 역할이라 필요한 역할의 사용자로 토큰을 만듦, CA 하나만 믿고 호스트 이름은 보지 않는 지문 확인, 등록 확인 순서와 400·409·502 코드, token은 응답·watch에 없고 token_id만, PUT의 token 유지 규칙, 노드가 원격에 연결하고 시간 제한·CA 캐시, oplctl remote add|list|show|refresh|vms|vm|set|remove 예와 지문 확인(--yes·터미널·스크립트), remote vm stop은 곧바로 끄기·shutdown은 ACPI, 상태 수집(리더, tick·interval, 위트니스 제외, 바뀔 때만 써 checked_at은 바뀐 시각), 중계(GET·HEAD viewer·나머지 admin, viewer도 토큰이 읽는 것을 모두 읽음, 보내고 돌려주는 헤더, .·..·빈 조각 400, watch·콘솔 404, 502, 감사 action·target·detail, API 문서(OpenAPI)에 넣지 않음), 웹, API, [remotes] 설정, 한계 — 토큰이 Raft에 평문, watch·콘솔 없음, URL은 노드 하나, CA가 바뀌면 다시 등록, 원격 401을 그대로 돌려줘 oplctl이 login으로 안내, If-Match 없는 웹 고치기 등)와 웹 UI 목록·감사 로그 detail 갱신, 엔터프라이즈 설계 §5.6 구현(계획 37)(설계와 다른 점: probe·refresh 끝점, CaCache 키 (id, url, 지문), 감사 action은 remote.proxy가 아니라 미들웨어 라우트 템플릿, 중계는 OpenAPI 밖, RemoteNodeCounts·RemoteVmCounts, oplctl stop·shutdown 뜻)과 §7 오류·§9 알려진 틈, 노드 코어 설계 §1.1에 계획 37 완료"
```

---

## 완료 기준 (계획 37)

- 클러스터 A에 B를 `oplctl remote add`로 등록하면(지문 확인) `remote list`가 B의 클러스터 이름·버전·노드·VM 수를 보이고, B가 꺼지면 unreachable이 된다.
- `oplctl remote vms B`와 웹 원격 화면이 B의 VM을 보이고, admin은 시작·정지할 수 있다. viewer는 중계로 B의 사용자·토큰·감사 로그를 읽지 못한다.
- CA 응답에 인증서를 덧붙이거나 지문이 다르면 등록이 실패하고, 등록 뒤에는 CA를 다시 받지 않는다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 `typecheck`·`lint`·`test`·`build`가 통과한다.

## 알고 받아들인 제한

- 원격 토큰은 Raft에 평문으로 있다(위트니스와 데이터 디렉터리 백업에도). 원격에는 필요한 만큼의 역할(보통 operator)을 가진 사용자의 토큰을 등록한다.
- 원격 URL은 한 노드를 가리킨다. 그 노드가 꺼지면 다른 노드가 살아 있어도 unreachable이다.
- 원격 CA가 바뀌면 `remote set --ca-fingerprint`로 다시 등록한다.
- 그 밖의 제한은 설계 문서 §5.6·§9와 README "원격 클러스터" 절에 있다.
