//! API 이름(클러스터 설정 `api_names`): 관리자가 바꾸면 노드가 API 인증서를 다시 서명해 리스너를 멈추지
//! 않고 바꿔 끼운다. 그 이름으로 TLS 확인이 통과한다.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use common::{Env, WAIT};
use opl_api::ApiCertAgent;
use opl_peer::pki::{NodeTls, first_cert, node_key_and_csr};
use opl_peer::tls::{ApiCert, TlsListener, api_server_config_with};
use opl_types::cluster::ClusterSpec;
use reqwest::StatusCode;
use rustls_pki_types::ServerName;
use serde_json::json;
use tokio::net::TcpStream;

/// 이 클러스터 CA로 `127.0.0.1` 노드 인증서를 만든다(합류 때처럼).
fn node_tls(env: &Env) -> NodeTls {
    let info = env.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
    let ca = opl_peer::pki::ClusterCa {
        cert_pem: info.spec.ca_cert_pem,
        key_pem: info.spec.ca_key_pem,
    };
    let (key_pem, csr) = node_key_and_csr().unwrap();
    NodeTls {
        cert_pem: ca.sign_node(&csr, env.node, &["127.0.0.1".into()]).unwrap(),
        ca_pem: ca.cert_pem,
        key_pem,
    }
}

/// opld처럼 노드 인증서로 리스너를 열고 API 인증서 작업을 띄운다.
async fn serve(env: &Env) -> (SocketAddr, ApiCertAgent) {
    let tls = node_tls(env);
    let cert = ApiCert::new(&tls.cert_pem, &tls.key_pem).unwrap();
    let agent = ApiCertAgent::spawn(env.cluster.clone(), tls, cert.clone());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let listener = TlsListener::new(tcp, api_server_config_with(cert).unwrap()).unwrap();
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, agent)
}

/// 서버 인증서를 클러스터 CA와 이름 `name`으로 확인하며 연결되는지
async fn verifies(env: &Env, addr: SocketAddr, name: &str) -> bool {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(first_cert(&env.ca_pem()).unwrap()).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = TcpStream::connect(addr).await.unwrap();
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(name.to_string()).unwrap(), tcp)
        .await
        .is_ok()
}

/// `name`의 확인 결과가 `expected`가 될 때까지 기다린다.
async fn wait_verifies(env: &Env, addr: SocketAddr, name: &str, expected: bool) {
    let deadline = Instant::now() + WAIT;
    while verifies(env, addr, name).await != expected {
        assert!(Instant::now() < deadline, "{name}: never became {expected}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn api_names_are_added_to_the_api_certificate() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (addr, agent) = serve(&env).await;
    assert!(verifies(&env, addr, "127.0.0.1").await);
    assert!(!verifies(&env, addr, "orbivisor.example.com").await);

    // 관리자만, 이름이나 IP만
    let viewer = env.user("v", "viewer").await;
    let (status, _) = viewer
        .patch(
            "/settings",
            json!({ "api_names": ["orbivisor.example.com"] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = admin
        .patch("/settings", json!({ "api_names": ["*.example.com"] }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "api_names": ["Orbivisor.Example.com", "192.168.0.200"] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["api_names"],
        json!(["orbivisor.example.com", "192.168.0.200"])
    );
    wait_verifies(&env, addr, "orbivisor.example.com", true).await;
    assert!(verifies(&env, addr, "192.168.0.200").await);
    // 노드 이름은 그대로 통과한다.
    assert!(verifies(&env, addr, "127.0.0.1").await);
    assert!(!verifies(&env, addr, "other.example.com").await);

    // 다시 시작한 노드는 리스너를 열기 전에 API 이름을 더한다.
    let (restarted, second) = serve(&env).await;
    assert!(verifies(&env, restarted, "orbivisor.example.com").await);
    second.shutdown().await;

    // 비우면 노드 인증서로 돌아간다.
    let (status, body) = admin
        .patch("/settings", json!({ "api_names": [] }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("api_names").is_none(), "{body}");
    wait_verifies(&env, addr, "orbivisor.example.com", false).await;
    assert!(verifies(&env, addr, "127.0.0.1").await);
    agent.shutdown().await;
}
