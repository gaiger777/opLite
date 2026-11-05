//! 시험용 텔레메트리 수집 서버. 시험 CA가 서명한 `localhost`·`127.0.0.1` 인증서의 HTTPS로
//! `POST /report`를 받아 본문(JSON)을 기록하고, 정한 상태 코드(와 `Location`)로 답한다(기본 204). 다른 크레이트의
//! 시험도 `#[path = ".../opl-api/tests/common/collector.rs"] mod collector;`로 가져다 쓸 수 있다
//! (axum·opl-peer·serde_json·tokio·uuid가 필요하다).
#![allow(dead_code)]

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use opl_peer::pki::{ClusterCa, NodeTls, first_cert, node_key_and_csr};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone)]
struct Site {
    reports: Arc<Mutex<Vec<Value>>>,
    status: Arc<AtomicU16>,
    location: Arc<Mutex<Option<String>>>,
}

pub struct Collector {
    /// 보고를 보낼 주소(`https://127.0.0.1:<port>/report`)
    pub url: String,
    /// 서버 인증서에 서명한 CA(PEM)
    pub ca_pem: String,
    /// 같은 CA(DER)
    pub ca_der: Vec<u8>,
    site: Site,
    server: JoinHandle<()>,
}

impl Collector {
    pub async fn start() -> Collector {
        let ca = ClusterCa::generate("telemetry").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let sans = ["localhost".to_string(), "127.0.0.1".to_string()];
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, uuid::Uuid::new_v4(), &sans).unwrap(),
            key_pem,
        };
        let config = opl_peer::tls::api_server_config(&tls).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
        let site = Site {
            reports: Arc::default(),
            status: Arc::new(AtomicU16::new(204)),
            location: Arc::default(),
        };
        let app = axum::Router::new()
            .route("/report", axum::routing::post(report))
            .with_state(site.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Collector {
            url: format!("https://{addr}/report"),
            ca_der: first_cert(&ca.cert_pem).unwrap().to_vec(),
            ca_pem: ca.cert_pem,
            site,
            server,
        }
    }

    /// 지금까지 받은 보고(받은 순서)
    pub fn reports(&self) -> Vec<Value> {
        self.site.reports.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.site.reports.lock().unwrap().len()
    }

    /// 앞으로 `status`로 답한다(기록은 그대로 한다).
    pub fn respond_with(&self, status: u16) {
        self.site.status.store(status, Ordering::SeqCst);
    }

    /// 앞으로 307과 `Location: location`으로 답한다(기록은 그대로 한다).
    pub fn redirect_to(&self, location: &str) {
        *self.site.location.lock().unwrap() = Some(location.to_string());
        self.respond_with(307);
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn report(State(site): State<Site>, body: Bytes) -> Response {
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    site.reports.lock().unwrap().push(value);
    let status = StatusCode::from_u16(site.status.load(Ordering::SeqCst)).unwrap();
    match site.location.lock().unwrap().clone() {
        Some(location) => (status, [(header::LOCATION, location)]).into_response(),
        None => status.into_response(),
    }
}
