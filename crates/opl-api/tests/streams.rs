//! WebSocket: 변경 구독과 VNC 콘솔 중계.

mod common;

use std::time::Duration;

use common::{Client, Env, vm_request};
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(client: &Client, path: &str) -> Socket {
    let url = format!("{}/api/v1{path}", client.base.replace("http://", "ws://"));
    let mut req = url.into_client_request().unwrap();
    let token = client.token.as_ref().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_json(ws: &mut Socket) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

#[tokio::test]
async fn watch_streams_changes_after_revision() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let (_, list) = viewer.get("/vms").await;
    let since = list["revision"].as_u64().unwrap();
    let mut ws = connect(&viewer, &format!("/watch?kinds=vm,task&since={since}")).await;

    admin.post("/vms", vm_request("web")).await;
    let ev = next_json(&mut ws).await;
    assert_eq!(ev["type"], "put");
    assert_eq!(ev["kind"], "vm");
    assert_eq!(ev["resource"]["name"], "web");

    // 시작하면 Task와 VM 변경이 함께 온다.
    admin.post("/vms/web/actions/start", Value::Null).await;
    let mut kinds = Vec::new();
    while kinds.len() < 3 {
        kinds.push(
            next_json(&mut ws).await["kind"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert!(kinds.contains(&"task".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"vm".to_string()), "{kinds:?}");
}

#[tokio::test]
async fn watch_rejects_secret_kinds_and_bad_queries() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin.get("/watch?kinds=user&since=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("user"), "{body}");
    let (status, body) = admin.get("/watch?kinds=vm").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"].as_str().unwrap().contains("since"),
        "{body}"
    );
    // 검증을 통과해도 WebSocket 요청이 아니면 거부한다.
    let (status, body) = admin.get("/watch?kinds=vm&since=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
}

#[tokio::test]
async fn console_bridges_websocket_to_vnc_socket() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    // 유닉스 소켓 경로 길이 제한 때문에 /tmp 아래 짧은 경로를 쓴다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let consoles = tmp.path().to_path_buf();
    env.hv.set_console_dir(&consoles);

    admin.post("/vms", vm_request("web")).await;
    let (status, body) = admin.get("/vms/web/console").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_not_running");

    admin.act("web", "start").await;
    let (_, vm) = admin.get("/vms/web").await;
    let id = vm["id"].as_str().unwrap();
    // 가짜 VNC 서버: 인사말을 보내고, 받은 것을 그대로 돌려준다.
    let listener = tokio::net::UnixListener::bind(consoles.join(format!("{id}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    assert_eq!(
        viewer.get("/vms/web/console").await.0,
        StatusCode::FORBIDDEN
    );
    let mut ws = connect(&admin, "/vms/web/console").await;
    let greeting = ws.next().await.unwrap().unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::binary(b"ping".to_vec())
    );
}
