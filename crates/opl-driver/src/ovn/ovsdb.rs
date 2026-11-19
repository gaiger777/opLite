//! OVSDB JSON-RPC(RFC 7047) 클라이언트(계획 65 §8.1). 리더 `OvnSync`가 OVN NB에 쓰고 SB의 지운 노드 chassis를
//! 지우며, API가 NB를 읽는다. `ovn-nbctl`을 부르지 않는다 — 리더 노드에 OVN 꾸러미가 없을 수 있고(macOS 리더도),
//! "다른 것만 한 트랜잭션에 + 펜싱 `wait`"는 JSON으로 만드는 편이 낫다.
//!
//! - **전송**: 클러스터 CA mTLS([`OvsdbTls`]: 노드 인증서로 붙고 서버 인증서를 클러스터 CA와 중앙의 이름(SAN)으로
//!   확인한다). 시험은 평문 TCP([`Transport::Tcp`])도 쓴다.
//! - **메시지**: 연결마다 읽기·쓰기 작업 하나가 돌며, 답을 `id`로 짝짓고 서버의 `echo`(비활성 탐침)에 곧바로 답한다
//!   — 답하지 않으면 서버가 끊는다. 쓰는 메서드는 `transact`·`get_schema`·`echo`·`list_dbs`뿐이다.
//! - **리더**: ovsdb 클러스터는 리더만 트랜잭션을 처리하고 팔로어는 넘긴다. 팔로어에서 읽은 행은 낡았을 수 있으므로
//!   쓰는 쪽(`leader_only`)은 붙을 때 `_Server` DB의 `Database` 행을 읽어 리더에 붙고, 아니면 다른 중앙으로 옮긴다
//!   (`ovn-nbctl`의 기본 `--leader-only`와 같다 — 팔로어의 로컬 소켓을 거절하는 그것). 읽기만 하는 쪽은 팔로어도 쓴다.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;

/// OVN northbound DB 이름
pub const NB_DB: &str = "OVN_Northbound";
/// OVN southbound DB 이름
pub const SB_DB: &str = "OVN_Southbound";
/// 서버 상태 DB(클러스터 리더 찾기)
pub const SERVER_DB: &str = "_Server";
/// 연결·요청 하나의 기본 시간 제한
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// 받는 버퍼의 상한. 이보다 큰 메시지 하나는 프로토콜 오류로 끊는다(5천 포트 select가 수 MiB다).
const MAX_MESSAGE: usize = 256 << 20;

#[derive(Debug, thiserror::Error)]
pub enum OvsdbError {
    #[error("cannot connect to OVSDB {endpoint}: {error}")]
    Connect { endpoint: String, error: String },
    #[error("OVSDB TLS setup: {0}")]
    Tls(String),
    #[error("the OVSDB connection to {0} is closed")]
    Closed(String),
    #[error("OVSDB {method} timed out after {secs}s")]
    Timeout { method: String, secs: u64 },
    #[error("OVSDB protocol error: {0}")]
    Protocol(String),
    #[error("OVSDB {method} failed: {error}")]
    Rpc { method: String, error: String },
    /// 트랜잭션의 `index`번째 연산(0부터, 연산 수와 같으면 커밋)이 실패했다. 트랜잭션 전체가 되돌려졌다.
    #[error("OVSDB operation {index} failed: {error}{}", details.as_deref().map(|d| format!(" ({d})")).unwrap_or_default())]
    Op {
        index: usize,
        error: String,
        details: Option<String>,
    },
    #[error("no OVSDB server of {db} is usable{}: {tried}", if *.leader_only { " as the leader" } else { "" })]
    NoServer {
        db: String,
        leader_only: bool,
        tried: String,
    },
}

impl OvsdbError {
    /// 펜싱 `wait`이 맞지 않아 트랜잭션이 되돌려졌는지(RFC 7047의 `"timed out"`)
    pub fn is_wait_failed(&self) -> bool {
        matches!(self, OvsdbError::Op { error, .. } if error == "timed out")
    }
}

/// 서버(중앙) 하나: TCP 주소와 서버 인증서에서 확인할 이름들(하나라도 맞으면 된다 — 중앙 주소 IP와 노드 주소의
/// 호스트 이름)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub addr: SocketAddr,
    pub names: Vec<String>,
}

impl Endpoint {
    /// 이름이 주소의 IP 하나인 서버
    pub fn ip(addr: SocketAddr) -> Self {
        Self {
            addr,
            names: vec![addr.ip().to_string()],
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.addr)
    }
}

/// 클러스터 CA mTLS 자료: 믿는 CA, 내보일 노드 인증서와 키
#[derive(Clone)]
pub struct OvsdbTls {
    roots: Arc<RootCertStore>,
    chain: Vec<CertificateDer<'static>>,
    key: Arc<PrivateKeyDer<'static>>,
}

impl fmt::Debug for OvsdbTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OvsdbTls").finish_non_exhaustive()
    }
}

impl OvsdbTls {
    /// PEM에서 만든다(`ca_pem`의 모든 인증서를 믿는다).
    pub fn from_pem(ca_pem: &str, cert_pem: &str, key_pem: &str) -> Result<Self, OvsdbError> {
        let tls = |e: &dyn fmt::Display| OvsdbError::Tls(e.to_string());
        let mut roots = RootCertStore::empty();
        for cert in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
            roots.add(cert.map_err(|e| tls(&e))?).map_err(|e| tls(&e))?;
        }
        if roots.is_empty() {
            return Err(OvsdbError::Tls("no CA certificate".into()));
        }
        let chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| tls(&e))?;
        if chain.is_empty() {
            return Err(OvsdbError::Tls("no node certificate".into()));
        }
        let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| tls(&e))?;
        Ok(Self {
            roots: Arc::new(roots),
            chain,
            key: Arc::new(key),
        })
    }

    /// 노드 TLS 디렉터리(`{data_dir}/pki`의 `ca.crt`·`node.crt`·`node.key`)에서 읽는다. 붙을 때마다 읽으면 다시 서명한
    /// 인증서를 곧바로 쓴다.
    pub fn from_dir(dir: &Path) -> Result<Self, OvsdbError> {
        let read = |f: &str| {
            std::fs::read_to_string(dir.join(f))
                .map_err(|e| OvsdbError::Tls(format!("{}: {e}", dir.join(f).display())))
        };
        Self::from_pem(&read("ca.crt")?, &read("node.crt")?, &read("node.key")?)
    }

    /// `names` 가운데 하나에 맞는 서버 인증서만 믿는 설정
    fn config(&self, names: &[String]) -> Result<ClientConfig, OvsdbError> {
        let tls = |e: &dyn fmt::Display| OvsdbError::Tls(e.to_string());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let webpki =
            WebPkiServerVerifier::builder_with_provider(self.roots.clone(), provider.clone())
                .build()
                .map_err(|e| tls(&e))?;
        let names = names
            .iter()
            .map(|n| ServerName::try_from(n.clone()).map_err(|e| tls(&e)))
            .collect::<Result<Vec<_>, _>>()?;
        ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| tls(&e))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyOf { webpki, names }))
            .with_client_auth_cert(self.chain.clone(), self.key.clone_key())
            .map_err(|e| tls(&e))
    }
}

/// 클러스터 CA가 서명했고 `names` 가운데 하나에 맞는 서버 인증서를 믿는다. 연결할 때 준 이름은 보지 않는다(`names`의
/// 첫 것이다).
#[derive(Debug)]
struct AnyOf {
    webpki: Arc<WebPkiServerVerifier>,
    names: Vec<ServerName<'static>>,
}

impl ServerCertVerifier for AnyOf {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut last = rustls::Error::InvalidCertificate(CertificateError::NotValidForName);
        for name in &self.names {
            match self.webpki.verify_server_cert(
                end_entity,
                intermediates,
                name,
                ocsp_response,
                now,
            ) {
                Ok(v) => return Ok(v),
                // 이름만 맞지 않으면 다음 이름을 본다. 서명·기간 오류는 이름과 상관없다.
                Err(
                    e @ rustls::Error::InvalidCertificate(
                        CertificateError::NotValidForName
                        | CertificateError::NotValidForNameContext { .. },
                    ),
                ) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

/// 연결 방법
#[derive(Debug, Clone)]
pub enum Transport {
    /// 클러스터 CA mTLS(OVN 중앙의 `pssl`)
    Tls(OvsdbTls),
    /// 평문 TCP. 시험(가짜 서버)만 쓴다 — Orbivisor 중앙은 평문 포트를 열지 않는다.
    Tcp,
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// `_Server` DB가 알려 주는 한 DB의 상태(이 서버에서 본 것)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDb {
    /// `clustered`·`standalone`·`relay`
    pub model: String,
    /// 클러스터에 붙어 있는지(독립 DB는 늘 참)
    pub connected: bool,
    /// 이 서버가 Raft 리더인지(독립 DB는 늘 참)
    pub leader: bool,
    /// Raft 클러스터 id(클러스터 DB만)
    pub cid: Option<String>,
}

impl ServerDb {
    /// 쓰는 쪽이 붙어도 되는 서버인지: 독립 DB이거나 클러스터의 연결된 리더
    pub fn is_leader(&self) -> bool {
        self.model != "clustered" || (self.leader && self.connected)
    }
}

type Reply = Result<Value, OvsdbError>;

#[derive(Default)]
struct Calls {
    waiting: HashMap<u64, oneshot::Sender<Reply>>,
    /// 연결이 닫혔으면 그 까닭
    closed: Option<String>,
}

/// OVSDB 서버 하나와의 연결. 놓으면 연결을 닫는다.
pub struct OvsdbClient {
    endpoint: Endpoint,
    out: mpsc::UnboundedSender<Vec<u8>>,
    calls: Arc<Mutex<Calls>>,
    next_id: AtomicU64,
    timeout: Duration,
    task: JoinHandle<()>,
}

impl fmt::Debug for OvsdbClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OvsdbClient")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Drop for OvsdbClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OvsdbClient {
    /// `endpoint`에 붙는다(TCP 연결과 TLS 핸드셰이크에 `timeout`, 요청마다도 `timeout`).
    pub async fn connect(
        transport: &Transport,
        endpoint: &Endpoint,
        timeout: Duration,
    ) -> Result<Self, OvsdbError> {
        let failed = |error: String| OvsdbError::Connect {
            endpoint: endpoint.to_string(),
            error,
        };
        let open = async {
            let tcp = TcpStream::connect(endpoint.addr)
                .await
                .map_err(|e| failed(e.to_string()))?;
            let _ = tcp.set_nodelay(true);
            let io: Box<dyn Io> = match transport {
                Transport::Tcp => Box::new(tcp),
                Transport::Tls(tls) => {
                    let config = tls.config(&endpoint.names)?;
                    let name = endpoint
                        .names
                        .first()
                        .cloned()
                        .unwrap_or_else(|| endpoint.addr.ip().to_string());
                    let name =
                        ServerName::try_from(name).map_err(|e| OvsdbError::Tls(e.to_string()))?;
                    let stream = TlsConnector::from(Arc::new(config))
                        .connect(name, tcp)
                        .await
                        .map_err(|e| failed(format!("TLS: {e}")))?;
                    Box::new(stream)
                }
            };
            Ok::<_, OvsdbError>(io)
        };
        let io = tokio::time::timeout(timeout, open)
            .await
            .map_err(|_| failed(format!("timed out after {}s", timeout.as_secs())))??;
        Ok(Self::start(io, endpoint.clone(), timeout))
    }

    fn start(io: Box<dyn Io>, endpoint: Endpoint, timeout: Duration) -> Self {
        let (out, rx) = mpsc::unbounded_channel();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let task = tokio::spawn(io_loop(io, rx, out.clone(), calls.clone()));
        Self {
            endpoint,
            out,
            calls,
            next_id: AtomicU64::new(1),
            timeout,
            task,
        }
    }

    /// `endpoints`를 차례로 붙어 보아 쓸 수 있는 첫 서버에 붙는다. `leader_only`면 `db`의 클러스터 리더(또는 독립 DB)만
    /// 받는다 — 팔로어에서 읽은 행은 낡았을 수 있다. 아니면 클러스터에 붙은 서버면 된다.
    pub async fn connect_any(
        transport: &Transport,
        endpoints: &[Endpoint],
        db: &str,
        leader_only: bool,
        timeout: Duration,
    ) -> Result<Self, OvsdbError> {
        let mut tried = Vec::new();
        for endpoint in endpoints {
            let client = match Self::connect(transport, endpoint, timeout).await {
                Ok(c) => c,
                Err(e) => {
                    tried.push(e.to_string());
                    continue;
                }
            };
            match client.server_db(db).await {
                Ok(Some(s))
                    if if leader_only {
                        s.is_leader()
                    } else {
                        s.connected
                    } =>
                {
                    return Ok(client);
                }
                Ok(Some(s)) => tried.push(format!(
                    "{endpoint}: {}",
                    if s.connected {
                        "not the cluster leader"
                    } else {
                        "not connected to the cluster"
                    }
                )),
                Ok(None) => tried.push(format!("{endpoint}: does not serve {db}")),
                Err(e) => tried.push(format!("{endpoint}: {e}")),
            }
        }
        Err(OvsdbError::NoServer {
            db: db.into(),
            leader_only,
            tried: if tried.is_empty() {
                "no servers".into()
            } else {
                tried.join("; ")
            },
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// 연결이 닫혔는지(서버가 끊었거나 오류)
    pub fn is_closed(&self) -> bool {
        self.calls.lock().unwrap().closed.is_some() || self.task.is_finished()
    }

    /// JSON-RPC 요청 하나를 보내고 답(`result`)을 기다린다.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, OvsdbError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut calls = self.calls.lock().unwrap();
            if calls.closed.is_some() {
                return Err(OvsdbError::Closed(self.endpoint.to_string()));
            }
            calls.waiting.insert(id, tx);
        }
        let msg = json!({"method": method, "params": params, "id": id});
        if self.out.send(msg.to_string().into_bytes()).is_err() {
            self.calls.lock().unwrap().waiting.remove(&id);
            return Err(OvsdbError::Closed(self.endpoint.to_string()));
        }
        let reply = match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(OvsdbError::Closed(self.endpoint.to_string())),
            Err(_) => {
                self.calls.lock().unwrap().waiting.remove(&id);
                Err(OvsdbError::Timeout {
                    method: method.into(),
                    secs: self.timeout.as_secs(),
                })
            }
        }?;
        match reply.get("error") {
            None | Some(Value::Null) => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
            Some(e) => Err(OvsdbError::Rpc {
                method: method.into(),
                error: error_text(e),
            }),
        }
    }

    /// 서버가 살아 있는지(`echo`)
    pub async fn echo(&self) -> Result<(), OvsdbError> {
        self.call("echo", json!(["opl"])).await.map(|_| ())
    }

    /// 서버가 내는 DB 이름들
    pub async fn list_dbs(&self) -> Result<Vec<String>, OvsdbError> {
        let v = self.call("list_dbs", json!([])).await?;
        serde_json::from_value(v).map_err(|e| OvsdbError::Protocol(e.to_string()))
    }

    /// DB의 스키마(JSON)
    pub async fn get_schema(&self, db: &str) -> Result<Value, OvsdbError> {
        self.call("get_schema", json!([db])).await
    }

    /// 트랜잭션 하나: 연산들을 한꺼번에 하고 연산마다의 결과를 돌려준다. 어느 연산이나 커밋이 실패하면 아무것도
    /// 바뀌지 않고 [`OvsdbError::Op`]이다.
    pub async fn transact(&self, db: &str, ops: Vec<Value>) -> Result<Vec<Value>, OvsdbError> {
        let n = ops.len();
        let mut params = Vec::with_capacity(n + 1);
        params.push(Value::String(db.into()));
        params.extend(ops);
        let result = self.call("transact", Value::Array(params)).await?;
        let Value::Array(results) = result else {
            return Err(OvsdbError::Protocol(format!(
                "transact result is not an array: {result}"
            )));
        };
        if let Some((index, e)) = results
            .iter()
            .enumerate()
            .find_map(|(i, r)| r.get("error").filter(|e| !e.is_null()).map(|e| (i, e)))
        {
            return Err(OvsdbError::Op {
                index,
                error: error_text(e),
                details: results[index]
                    .get("details")
                    .and_then(Value::as_str)
                    .map(String::from),
            });
        }
        if results.len() < n {
            return Err(OvsdbError::Protocol(format!(
                "transact returned {} results for {n} operations",
                results.len()
            )));
        }
        Ok(results)
    }

    /// `table`에서 `conditions`에 맞는 행의 `columns`(비면 모두)를 읽는다.
    pub async fn select(
        &self,
        db: &str,
        table: &str,
        conditions: Vec<Value>,
        columns: &[&str],
    ) -> Result<Vec<Map<String, Value>>, OvsdbError> {
        let mut op = json!({"op": "select", "table": table, "where": conditions});
        if !columns.is_empty() {
            op["columns"] = json!(columns);
        }
        let mut results = self.transact(db, vec![op]).await?;
        rows(results.swap_remove(0))
    }

    /// 이 서버에서 본 `db`의 상태(`_Server` DB). 서버가 그 DB를 내지 않으면 None
    pub async fn server_db(&self, db: &str) -> Result<Option<ServerDb>, OvsdbError> {
        let rows = self
            .select(
                SERVER_DB,
                "Database",
                vec![cond("name", "==", json!(db))],
                &["name", "model", "connected", "leader", "cid"],
            )
            .await?;
        Ok(rows.first().map(|r| ServerDb {
            model: r
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            connected: r.get("connected").and_then(Value::as_bool).unwrap_or(false),
            leader: r.get("leader").and_then(Value::as_bool).unwrap_or(false),
            cid: r.get("cid").and_then(|v| parse_uuids(v).into_iter().next()),
        }))
    }
}

/// `select` 결과의 `rows`
pub fn rows(result: Value) -> Result<Vec<Map<String, Value>>, OvsdbError> {
    match result.get("rows") {
        Some(Value::Array(rows)) => {
            Ok(rows.iter().filter_map(|r| r.as_object().cloned()).collect())
        }
        _ => Err(OvsdbError::Protocol(format!(
            "select result has no rows: {result}"
        ))),
    }
}

fn error_text(e: &Value) -> String {
    e.as_str().map_or_else(|| e.to_string(), String::from)
}

/// 연결 하나의 읽기·쓰기. 보낼 메시지를 쓰고, 받은 메시지를 짝짓거나(`id`) 서버의 `echo`에 답한다. 끊기면 기다리는
/// 요청을 모두 실패로 끝낸다.
async fn io_loop(
    io: Box<dyn Io>,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    calls: Arc<Mutex<Calls>>,
) {
    let (mut reader, mut writer) = tokio::io::split(io);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 64 << 10];
    let reason = loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break "closed by the client".to_string() };
                if let Err(e) = async {
                    writer.write_all(&msg).await?;
                    writer.flush().await
                }.await {
                    break e.to_string();
                }
            }
            n = reader.read(&mut chunk) => {
                let n = match n {
                    Ok(0) => break "closed by the server".to_string(),
                    Ok(n) => n,
                    Err(e) => break e.to_string(),
                };
                buf.extend_from_slice(&chunk[..n]);
                match take_messages(&mut buf) {
                    Ok(messages) => {
                        for m in messages {
                            dispatch(m, &out, &calls);
                        }
                    }
                    Err(e) => break e,
                }
            }
        }
    };
    let mut calls = calls.lock().unwrap();
    calls.closed = Some(reason);
    calls.waiting.clear();
}

/// 버퍼 앞의 다 받은 JSON 메시지들을 꺼낸다(OVSDB는 메시지를 구분자 없이 잇는다). 끝의 덜 받은 메시지는 남긴다.
fn take_messages(buf: &mut Vec<u8>) -> Result<Vec<Value>, String> {
    let mut messages = Vec::new();
    let mut stream = serde_json::Deserializer::from_slice(buf).into_iter::<Value>();
    let mut used = 0;
    loop {
        match stream.next() {
            Some(Ok(v)) => {
                messages.push(v);
                used = stream.byte_offset();
            }
            Some(Err(e)) if e.is_eof() => break,
            Some(Err(e)) => return Err(format!("invalid JSON from the server: {e}")),
            None => {
                used = stream.byte_offset();
                break;
            }
        }
    }
    buf.drain(..used);
    if buf.len() > MAX_MESSAGE {
        return Err(format!("a message is larger than {MAX_MESSAGE} bytes"));
    }
    Ok(messages)
}

fn dispatch(m: Value, out: &mpsc::UnboundedSender<Vec<u8>>, calls: &Mutex<Calls>) {
    match m.get("method").and_then(Value::as_str) {
        // 서버의 비활성 탐침: 같은 id와 params로 답한다.
        Some("echo") => {
            let reply = json!({
                "id": m.get("id").cloned().unwrap_or(Value::Null),
                "result": m.get("params").cloned().unwrap_or(json!([])),
                "error": null,
            });
            let _ = out.send(reply.to_string().into_bytes());
        }
        // 알림(update·locked 따위)은 쓰지 않는다.
        Some(_) => {}
        None => {
            let Some(id) = m.get("id").and_then(Value::as_u64) else {
                return;
            };
            if let Some(tx) = calls.lock().unwrap().waiting.remove(&id) {
                let _ = tx.send(Ok(m));
            }
        }
    }
}

// ---- OVSDB 값(RFC 7047 §5.1) ----

/// 조건 `[column, function, value]`
pub fn cond(column: &str, function: &str, value: Value) -> Value {
    json!([column, function, value])
}

/// 문자열 맵 `["map", [[k, v], …]]`
pub fn map_value(m: &BTreeMap<String, String>) -> Value {
    json!([
        "map",
        m.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>()
    ])
}

/// 집합 `["set", […]]`
pub fn set_value(items: Vec<Value>) -> Value {
    json!(["set", items])
}

/// 문자열 집합
pub fn string_set(items: &[String]) -> Value {
    set_value(items.iter().map(|s| json!(s)).collect())
}

/// 행 참조 `["uuid", u]`
pub fn uuid_value(u: &str) -> Value {
    json!(["uuid", u])
}

/// 같은 트랜잭션에서 넣은 행의 참조 `["named-uuid", n]`
pub fn named_uuid(n: &str) -> Value {
    json!(["named-uuid", n])
}

/// 집합의 원소들: `["set", […]]`이나 원소 하나(서버는 원소가 하나인 집합을 그 원소로 쓴다)
pub fn set_items(v: &Value) -> Vec<Value> {
    match v.as_array().map(Vec::as_slice) {
        Some([tag, Value::Array(items)]) if tag == "set" => items.clone(),
        _ => vec![v.clone()],
    }
}

/// 문자열 집합(문자열이 아닌 원소는 뺀다)
pub fn parse_strings(v: &Value) -> Vec<String> {
    set_items(v)
        .iter()
        .filter_map(|a| a.as_str().map(String::from))
        .collect()
}

/// 참조(`["uuid", u]`)들
pub fn parse_uuids(v: &Value) -> Vec<String> {
    set_items(v)
        .iter()
        .filter_map(|a| match a.as_array().map(Vec::as_slice) {
            Some([tag, Value::String(u)]) if tag == "uuid" => Some(u.clone()),
            _ => None,
        })
        .collect()
}

/// 문자열 맵(값이 문자열이 아니면 JSON 글로)
pub fn parse_map(v: &Value) -> BTreeMap<String, String> {
    let Some([tag, Value::Array(pairs)]) = v.as_array().map(Vec::as_slice) else {
        return BTreeMap::new();
    };
    if tag != "map" {
        return BTreeMap::new();
    }
    pairs
        .iter()
        .filter_map(|p| match p.as_array().map(Vec::as_slice) {
            Some([Value::String(k), v]) => Some((
                k.clone(),
                v.as_str().map_or_else(|| v.to_string(), String::from),
            )),
            _ => None,
        })
        .collect()
}

/// 정수(선택 정수 `["set", []]`는 None)
pub fn parse_int(v: &Value) -> Option<i64> {
    set_items(v).first().and_then(Value::as_i64)
}

/// 참거짓(선택 값이 비면 None)
pub fn parse_bool(v: &Value) -> Option<bool> {
    set_items(v).first().and_then(Value::as_bool)
}

#[cfg(test)]
mod tests;
