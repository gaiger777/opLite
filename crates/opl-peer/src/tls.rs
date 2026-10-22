//! 외부 API의 HTTPS. 노드 인증서로 TLS를 열고 axum에 넘긴다.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::pki::{NodeTls, PkiError, first_cert};

/// TLS 핸드셰이크를 기다리는 최대 시간. 느린 클라이언트가 연결을 붙잡고 있지 못하게 한다.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 외부 API용 서버 설정. 클라이언트 인증서는 요구하지 않는다(사용자 인증은 API가 한다).
pub fn api_server_config(tls: &NodeTls) -> Result<ServerConfig, PkiError> {
    let key = PrivateKeyDer::from_pem_slice(tls.key_pem.as_bytes())
        .map_err(|e| PkiError::Pem(e.to_string()))?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| PkiError::Pem(e.to_string()))?
            .with_no_client_auth()
            .with_single_cert(vec![first_cert(&tls.cert_pem)?], key)
            .map_err(|e| PkiError::Pem(e.to_string()))?;
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
        let accept = tokio::spawn(async move {
            loop {
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
    use crate::pki::{ClusterCa, node_key_and_csr};
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
}
