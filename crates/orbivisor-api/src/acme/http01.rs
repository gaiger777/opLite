//! HTTP-01 리스너(계획 73 §11.3 — opld `[api] http_listen`, 기본 꺼짐). 하는 일은 둘뿐이다:
//! `GET /.well-known/acme-challenge/{token}` → 인증서 status `challenges`의 `key_authorization`(Raft — 이름이 어느 노드를
//! 가리켜도 답한다, 없으면 404), 그 밖은 설정 `acme.http_redirect`면 `308 https://{Host}:{API 포트}{경로}`, 아니면 404.
//! 손으로 짠 HTTP/1.1이다 — 본문을 읽지 않고, 요청 머리 8 KiB·연결당 한 요청(`Connection: close`)·10초. 초당 100 요청
//! 한도는 도전 경로가 아닌 요청(리다이렉트·404)에만 건다 — 도전 답은 메모리 조회뿐이고, 한도로 막으면 아무나 CA 검증을
//! 503으로 떨어뜨릴 수 있다.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orbivisor_cluster::Cluster;
use orbivisor_types::acme::CertificateSpec;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;

pub const CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";
const MAX_HEAD: usize = 8 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS: usize = 256;
const RATE_PER_SEC: u32 = 100;

/// 도전 경로가 아닌 요청의 초당 한도(노드 전체)
struct Rate {
    window: Instant,
    used: u32,
}

impl Rate {
    fn allow(&mut self) -> bool {
        if self.window.elapsed() >= Duration::from_secs(1) {
            self.window = Instant::now();
            self.used = 0;
        }
        self.used += 1;
        self.used <= RATE_PER_SEC
    }
}

/// 도전 토큰의 답(살아 있는 것만)
pub fn answer(cluster: &Cluster, token: &str) -> Option<String> {
    if token.is_empty()
        || token.len() > 128
        || !token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return None;
    }
    let now = crate::backups::now() as i64;
    let (_, certs) = cluster.list::<CertificateSpec>().ok()?;
    certs.iter().find_map(|c| {
        c.status
            .challenges
            .iter()
            .find(|a| a.token == token && a.until > now)
            .map(|a| a.key_authorization.clone())
    })
}

/// 요청 머리에서 (메서드, 경로, Host)
fn parse_head(head: &str) -> Option<(&str, &str, Option<&str>)> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?;
    let target = first.next()?;
    let version = first.next()?;
    // 경로는 보이는 ASCII만(리다이렉트 `Location`에 그대로 싣는다 — 맨 LF·제어 글자로 머리를 끼워 넣지 못하게)
    if !version.starts_with("HTTP/1.")
        || !target.starts_with('/')
        || !target.bytes().all(|c| (0x21..=0x7e).contains(&c))
    {
        return None;
    }
    let host = lines.find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("host").then(|| v.trim())
    });
    Some((method, target, host))
}

/// Host 머리에서 포트를 뗀 이름(이름·IPv4·[IPv6] 글자만)
fn host_name(host: &str) -> Option<&str> {
    if host.is_empty()
        || host.len() > 255
        || !host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-:[]".contains(&c))
    {
        return None;
    }
    if let Some(rest) = host.strip_prefix('[') {
        let end = rest.find(']')?;
        return Some(&host[..end + 2]);
    }
    Some(host.split(':').next().unwrap_or(host))
}

fn response(status: &str, headers: &[(&str, &str)], body: &str, head_only: bool) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    if !head_only {
        out.push_str(body);
    }
    out.into_bytes()
}

/// 요청 머리 하나에 대한 응답
pub fn respond(cluster: &Cluster, https_port: u16, head: &str) -> Vec<u8> {
    let text = [("Content-Type", "text/plain")];
    let Some((method, target, host)) = parse_head(head) else {
        return response("400 Bad Request", &text, "bad request\n", false);
    };
    let head_only = method == "HEAD";
    if method != "GET" && !head_only {
        return response("405 Method Not Allowed", &text, "only GET\n", false);
    }
    let path = target.split('?').next().unwrap_or(target);
    if let Some(token) = path.strip_prefix(CHALLENGE_PREFIX) {
        return match answer(cluster, token) {
            Some(key_authorization) => response("200 OK", &text, &key_authorization, head_only),
            None => response("404 Not Found", &text, "not found\n", head_only),
        };
    }
    let redirect = crate::settings::current(cluster)
        .map(|s| s.spec.acme().http_redirect)
        .unwrap_or(true);
    match host.and_then(host_name) {
        Some(name) if redirect => {
            let location = format!("https://{name}:{https_port}{target}");
            response(
                "308 Permanent Redirect",
                &[("Location", location.as_str()), text[0]],
                "",
                head_only,
            )
        }
        _ => response("404 Not Found", &text, "not found\n", head_only),
    }
}

/// 도전 경로 요청인가(한도에서 뺀다)
fn is_challenge(head: &str) -> bool {
    parse_head(head).is_some_and(|(_, target, _)| target.starts_with(CHALLENGE_PREFIX))
}

async fn serve_one(
    mut stream: TcpStream,
    cluster: Arc<Cluster>,
    https_port: u16,
    rate: Arc<Mutex<Rate>>,
) {
    let read = async {
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                buf.truncate(end);
                return String::from_utf8(buf).ok();
            }
            if buf.len() > MAX_HEAD {
                return Some(String::new());
            }
        }
    };
    let head = match tokio::time::timeout(READ_TIMEOUT, read).await {
        Ok(Some(head)) => head,
        _ => return,
    };
    let reply = if head.is_empty() {
        response("431 Request Header Fields Too Large", &[], "", false)
    } else if !is_challenge(&head) && !rate.lock().unwrap_or_else(|e| e.into_inner()).allow() {
        response(
            "503 Service Unavailable",
            &[("Retry-After", "1")],
            "",
            false,
        )
    } else {
        respond(&cluster, https_port, &head)
    };
    let _ = stream.write_all(&reply).await;
    let _ = stream.shutdown().await;
}

/// 포트 80 리스너
pub struct Http01Server {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    pub local: SocketAddr,
}

impl Http01Server {
    /// `https_port`는 리다이렉트할 API 포트(8006)다.
    pub fn spawn(
        listener: TcpListener,
        cluster: Arc<Cluster>,
        https_port: u16,
    ) -> std::io::Result<Self> {
        let local = listener.local_addr()?;
        let (shutdown, mut rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
            let rate = Arc::new(Mutex::new(Rate {
                window: Instant::now(),
                used: 0,
            }));
            loop {
                let permit = tokio::select! {
                    _ = &mut rx => return,
                    p = slots.clone().acquire_owned() => match p { Ok(p) => p, Err(_) => return },
                };
                let (stream, _) = tokio::select! {
                    _ = &mut rx => return,
                    a = listener.accept() => match a {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::debug!(error = %e, "HTTP-01 accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    },
                };
                let (cluster, rate) = (cluster.clone(), rate.clone());
                tokio::spawn(async move {
                    serve_one(stream, cluster, https_port, rate).await;
                    drop(permit);
                });
            }
        });
        Ok(Self {
            shutdown,
            task,
            local,
        })
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_heads_and_hosts() {
        assert_eq!(
            parse_head(
                "GET /.well-known/acme-challenge/abc HTTP/1.1\r\nHost: a.example.com:80\r\nX: y"
            ),
            Some((
                "GET",
                "/.well-known/acme-challenge/abc",
                Some("a.example.com:80")
            ))
        );
        assert_eq!(parse_head("GET abc HTTP/1.1"), None);
        assert_eq!(parse_head("garbage"), None);
        // 리뷰 1: 맨 LF·제어 글자·공백 아닌 보이지 않는 글자가 든 경로는 400(Location에 실리지 않게)
        assert_eq!(parse_head("GET /a\nSet-Cookie:x HTTP/1.1"), None);
        assert_eq!(parse_head("GET /a\tb HTTP/1.1"), None);
        assert_eq!(parse_head("GET /a\u{7f} HTTP/1.1"), None);
        assert!(parse_head("GET /a?b=%0d%0a HTTP/1.1").is_some());
        assert!(is_challenge(
            "GET /.well-known/acme-challenge/x HTTP/1.1\r\nHost: a"
        ));
        assert!(!is_challenge("GET /other HTTP/1.1\r\nHost: a"));
        assert_eq!(host_name("a.example.com:80"), Some("a.example.com"));
        assert_eq!(host_name("[fd00::1]:80"), Some("[fd00::1]"));
        assert_eq!(host_name("evil.com/x"), None);
        assert_eq!(host_name("a\r\nb"), None);
        let mut r = Rate {
            window: Instant::now(),
            used: 0,
        };
        assert!((0..RATE_PER_SEC).all(|_| r.allow()));
        assert!(!r.allow());
    }
}
