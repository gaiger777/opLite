//! 원격 클러스터 클라이언트: CA 하나만 믿고 이름은 보지 않는 TLS, 검증 없는 CA 받기, 본문 한도,
//! 리다이렉트 안 따름, 시간 제한, 흘려 보내기. 시험 CA가 `node-x` 이름으로 서명한 인증서를
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
use opl_api::remote_client::{RemoteClient, RemoteError, fetch_ca};
use opl_peer::pki::{ClusterCa, NodeTls, fingerprint, node_key_and_csr};
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
