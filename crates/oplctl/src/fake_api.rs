//! 시험용 작은 HTTP API 서버(`metrics`·`alerts` 모듈의 시험). 요청 줄·`if-match`·JSON 본문을 남기고, 메서드와 경로에
//! 맞는 상태 코드와 JSON으로 답한다.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::client::Client;

/// 받은 요청 하나
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    /// 요청 줄(`GET /api/v1/... HTTP/1.1`)
    pub line: String,
    pub if_match: Option<String>,
    /// JSON 본문(없으면 null)
    pub body: Value,
}

pub(crate) type SeenLog = Arc<Mutex<Vec<Seen>>>;

/// `reply(method, path)`가 정한 상태와 JSON으로 답하는 서버를 띄우고 그 서버로 가는 클라이언트를 돌려준다.
pub(crate) async fn fake_api(
    reply: impl Fn(&str, &str) -> (u16, Value) + Send + Sync + 'static,
) -> (Client, SeenLog) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: SeenLog = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = vec![0; 4096];
            // 머리와 content-length만큼의 본문을 다 읽는다.
            let (head, body) = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                assert!(n > 0, "connection closed mid-request");
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let length = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break (head.to_string(), body.to_string());
                }
            };
            let line = head.lines().next().unwrap_or("").to_string();
            let mut parts = line.split(' ');
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let if_match = head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("if-match")
                    .then(|| v.trim().to_string())
            });
            log.lock().unwrap().push(Seen {
                line,
                if_match,
                body: serde_json::from_str(&body).unwrap_or(Value::Null),
            });
            let (status, body) = reply(&method, &path);
            let body = if body.is_null() {
                String::new()
            } else {
                body.to_string()
            };
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        }
    });
    (Client::new(&base, None, None).unwrap(), seen)
}
