//! 외부 API의 HTTPS. 노드 인증서로 TLS를 열고 axum에 넘긴다.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rustls::ServerConfig;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::pki::{NodeTls, PkiError, certs};

/// TLS 핸드셰이크를 기다리는 최대 시간. 느린 클라이언트가 연결을 붙잡고 있지 못하게 한다.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 동시에 진행하는 TLS 핸드셰이크의 최대 수. 다 차면 새 연결을 받지 않고 기다려, 끝나지 않는
/// 연결이 몰려도 파일 디스크립터를 다 쓰지 않는다.
const MAX_HANDSHAKES: usize = 256;

/// 외부 API가 내보이는 인증서. 바꿔 끼우면 다음 핸드셰이크부터 새 인증서를 쓴다(열린 연결은 그대로).
pub struct ApiCert {
    current: RwLock<Arc<CertifiedKey>>,
}

impl fmt::Debug for ApiCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiCert").finish_non_exhaustive()
    }
}

impl ApiCert {
    /// 인증서와 키(PEM)로 만든다.
    pub fn new(cert_pem: &str, key_pem: &str) -> Result<Arc<Self>, PkiError> {
        Ok(Arc::new(Self {
            current: RwLock::new(certified_key(cert_pem, key_pem)?),
        }))
    }

    /// 인증서와 키(PEM)를 바꾼다. 읽지 못하거나 짝이 맞지 않으면 바꾸지 않는다.
    pub fn replace(&self, cert_pem: &str, key_pem: &str) -> Result<(), PkiError> {
        let key = certified_key(cert_pem, key_pem)?;
        *self.current.write().unwrap() = key;
        Ok(())
    }
}

impl ResolvesServerCert for ApiCert {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.read().unwrap().clone())
    }
}

fn certified_key(cert_pem: &str, key_pem: &str) -> Result<Arc<CertifiedKey>, PkiError> {
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|e| PkiError::Pem(e.to_string()))?;
    let key = CertifiedKey::from_der(
        certs(cert_pem)?,
        key,
        &rustls::crypto::ring::default_provider(),
    )
    .map_err(|e| PkiError::Pem(e.to_string()))?;
    Ok(Arc::new(key))
}

/// 외부 API용 서버 설정. 클라이언트 인증서는 요구하지 않는다(사용자 인증은 API가 한다).
pub fn api_server_config(tls: &NodeTls) -> Result<ServerConfig, PkiError> {
    api_server_config_with(ApiCert::new(&tls.cert_pem, &tls.key_pem)?)
}

/// [`api_server_config`]와 같되 `cert`가 내보일 인증서를 정한다(바꿔 끼울 수 있음).
pub fn api_server_config_with(cert: Arc<ApiCert>) -> Result<ServerConfig, PkiError> {
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| PkiError::Pem(e.to_string()))?
            .with_no_client_auth()
            .with_cert_resolver(cert);
    // WebSocket을 HTTP/1.1로 받으므로 h2는 알리지 않는다.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// TLS로 감싼 TCP 리스너. `axum::serve`에 넘긴다. 핸드셰이크는 연결마다 따로 돌아 느린 클라이언트가
/// 다른 연결을 막지 않는다.
pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
    accept: JoinHandle<()>,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, config: ServerConfig) -> io::Result<Self> {
        let local = tcp.local_addr()?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let (tx, rx) = mpsc::channel(64);
        let handshakes = Arc::new(Semaphore::new(MAX_HANDSHAKES));
        let accept = tokio::spawn(async move {
            loop {
                let permit = match handshakes.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => return,
                };
                let (stream, addr) = match tcp.accept().await {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot accept a connection");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => {
                            let _ = tx.send((tls, addr)).await;
                        }
                        Ok(Err(e)) => tracing::debug!(%addr, error = %e, "TLS handshake failed"),
                        Err(_) => tracing::debug!(%addr, "TLS handshake timed out"),
                    }
                });
            }
        });
        Ok(Self { rx, local, accept })
    }
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        // 포트를 놓는다.
        self.accept.abort();
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(conn) => conn,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pki::{ClusterCa, api_cert, first_cert, node_key_and_csr};
    use rustls_pki_types::ServerName;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn serves_axum_over_tls_with_the_node_certificate() {
        let ca = ClusterCa::generate("test").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca
                .sign_node(&csr, uuid::Uuid::now_v7(), &["127.0.0.1".into()])
                .unwrap(),
            key_pem,
        };
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener = TlsListener::new(tcp, api_server_config(&tls).unwrap()).unwrap();
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "hello" }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // 핸드셰이크를 끝내지 않는 연결이 있어도 다른 연결은 받는다.
        let _stalled = TcpStream::connect(addr).await.unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(first_cert(&ca.cert_pem).unwrap()).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = ServerName::try_from("127.0.0.1").unwrap();
        let mut stream = connector.connect(name, tcp).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("hello"), "{response}");
    }

    /// 이름 `name`으로 서버 인증서를 확인하며 TLS로 연결한다.
    async fn handshake(
        ca_pem: &str,
        addr: SocketAddr,
        name: &str,
    ) -> io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(first_cert(ca_pem).unwrap()).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let tcp = TcpStream::connect(addr).await?;
        connector
            .connect(ServerName::try_from(name.to_string()).unwrap(), tcp)
            .await
    }

    /// 인증서를 바꿔 끼우면 새 핸드셰이크는 새 이름으로 확인되고, 열려 있던 연결은 그대로 쓴다.
    #[tokio::test]
    async fn api_cert_is_replaced_without_restarting_the_listener() {
        let ca = ClusterCa::generate("test").unwrap();
        let node = uuid::Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let cert_pem = ca.sign_node(&csr, node, &["127.0.0.1".into()]).unwrap();
        let cert = ApiCert::new(&cert_pem, &key_pem).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener =
            TlsListener::new(tcp, api_server_config_with(cert.clone()).unwrap()).unwrap();
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "hello" }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut before = handshake(&ca.cert_pem, addr, "127.0.0.1").await.unwrap();
        assert!(
            handshake(&ca.cert_pem, addr, "orbivisor.example.com")
                .await
                .is_err()
        );
        // 짝이 맞지 않는 키는 받지 않는다.
        let (other_key, _) = node_key_and_csr().unwrap();
        assert!(cert.replace(&cert_pem, &other_key).is_err());

        let (api_pem, api_key) =
            api_cert(&ca, &cert_pem, &["orbivisor.example.com".into()]).unwrap();
        cert.replace(&api_pem, &api_key).unwrap();
        handshake(&ca.cert_pem, addr, "orbivisor.example.com")
            .await
            .unwrap();
        handshake(&ca.cert_pem, addr, "127.0.0.1").await.unwrap();

        before
            .write_all(b"GET / HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        before.read_to_string(&mut response).await.unwrap();
        assert!(response.ends_with("hello"), "{response}");
    }
}
