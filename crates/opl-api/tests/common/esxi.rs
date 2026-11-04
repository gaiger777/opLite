//! 시험용 가짜 ESXi. 자체 서명 인증서의 HTTPS로 데이터스토어 브라우저(`/folder`)를 흉내 낸다:
//! 데이터센터의 데이터스토어 목록, 디렉터리 목록(ESXi 7/8 모양 HTML), 파일 내려받기, 기본 인증(틀리면
//! 401). 받은 요청을 모두 기록한다. 다른 크레이트의 시험도
//! `#[path = ".../opl-api/tests/common/esxi.rs"] mod esxi;`로 가져다 쓸 수 있다(axum·tokio·opl-api·
//! opl-peer·opl-types·rcgen·rustls·sha2가 필요하다).
//!
//! ```ignore
//! let files = BTreeMap::from([("datastore1/web01/web01.vmx".to_string(), vmx.into_bytes())]);
//! let esxi = MockEsxi::start(files, "root", "secret").await;
//! let conn = esxi.connection("secret", Some(&esxi.fingerprint));
//! ```
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::Response;
use opl_api::vm_imports::esxi::basic_auth;
use opl_types::vm_import::EsxiConnection;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 가짜 ESXi가 받는 데이터센터(ESXi 단독 호스트의 이름)
pub const DATACENTER: &str = "ha-datacenter";

/// 받은 요청 하나
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    /// 퍼센트 인코딩을 푼 경로(`/folder/web01/web01.vmx`)
    pub path: String,
    /// 쿼리 그대로
    pub query: String,
    pub authorization: Option<String>,
}

pub struct MockEsxi {
    pub addr: SocketAddr,
    /// 서버 인증서의 SHA-256 지문(대문자 콜론 구분)
    pub fingerprint: String,
    user: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    server: JoinHandle<()>,
}

struct Site {
    /// `"{데이터스토어}/{경로}"` → 내용
    files: BTreeMap<String, Vec<u8>>,
    authorization: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockEsxi {
    /// `files`의 키는 `"{데이터스토어}/{경로}"`(예: `"datastore1/web01/web01.vmx"`)다. 데이터스토어와
    /// 디렉터리는 파일 경로에서 나온다.
    pub async fn start(files: BTreeMap<String, Vec<u8>>, user: &str, password: &str) -> MockEsxi {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let der = key.cert.der().clone();
        let fingerprint = Sha256::digest(&der)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let private =
            rustls_pki_types::PrivateKeyDer::Pkcs8(key.signing_key.serialize_der().into());
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der], private)
        .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let site = Arc::new(Site {
            files,
            authorization: basic_auth(user, password),
            requests: requests.clone(),
        });
        let app = axum::Router::new().fallback(move |request: Request| {
            let site = site.clone();
            async move { site.handle(request) }
        });
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        MockEsxi {
            addr,
            fingerprint,
            user: user.to_string(),
            requests,
            server,
        }
    }

    /// 지금까지 받은 요청
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// 이 서버로 가는 연결(사용자는 `start`의 것)
    pub fn connection(&self, password: &str, fingerprint: Option<&str>) -> EsxiConnection {
        EsxiConnection {
            host: self.addr.ip().to_string(),
            port: self.addr.port(),
            user: self.user.clone(),
            password: password.into(),
            datacenter: DATACENTER.into(),
            fingerprint: fingerprint.map(str::to_string),
        }
    }
}

impl Drop for MockEsxi {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Site {
    fn handle(&self, request: Request) -> Response {
        let path = percent_decode(request.uri().path());
        let query = request.uri().query().unwrap_or("").to_string();
        let authorization = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        self.requests.lock().unwrap().push(Recorded {
            method: request.method().to_string(),
            path: path.clone(),
            query: query.clone(),
            authorization: authorization.clone(),
        });
        if authorization.as_deref() != Some(self.authorization.as_str()) {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header(
                    header::WWW_AUTHENTICATE,
                    "Basic realm=\"VMware HTTP server\"",
                )
                .body(Body::empty())
                .unwrap();
        }
        let params: BTreeMap<String, String> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_string(), percent_decode(v)))
            .collect();
        if params.get("dcPath").map(String::as_str) != Some(DATACENTER) {
            return not_found();
        }
        let Some(rest) = path.strip_prefix("/folder") else {
            return not_found();
        };
        let rest = rest.trim_matches('/');
        let Some(ds) = params.get("dsName") else {
            return if rest.is_empty() {
                html(self.datacenter_page())
            } else {
                not_found()
            };
        };
        let key = if rest.is_empty() {
            ds.clone()
        } else {
            format!("{ds}/{rest}")
        };
        if let Some(bytes) = self.files.get(&key) {
            return Response::builder()
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(bytes.clone()))
                .unwrap();
        }
        let prefix = format!("{key}/");
        let children: BTreeSet<&str> = self
            .files
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .map(|r| r.split('/').next().unwrap_or(r))
            .collect();
        if children.is_empty() {
            return not_found();
        }
        html(directory_page(ds, rest, &children))
    }

    fn datastores(&self) -> BTreeSet<&str> {
        self.files
            .keys()
            .filter_map(|k| k.split('/').next())
            .collect()
    }

    fn datacenter_page(&self) -> String {
        let rows: String = self
            .datastores()
            .iter()
            .map(|ds| {
                format!(
                    "<tr><td><a href=\"/folder?dcPath={}&amp;dsName={}\">{}</a></td></tr>\n",
                    encode(DATACENTER),
                    encode(ds),
                    escape(ds)
                )
            })
            .collect();
        page(
            "Datastores",
            &format!(
                "<table>\n<tr><th>Datastore Name</th></tr>\n<tr><td><a href=\"/\">Parent Directory</a></td></tr>\n{rows}</table>\n"
            ),
        )
    }
}

/// ESXi 7/8 데이터스토어 브라우저 모양의 디렉터리 목록. 맨 위면 "Parent Datacenter", 그 아래면
/// "Parent Directory" 링크가 먼저 온다.
fn directory_page(ds: &str, dir: &str, children: &BTreeSet<&str>) -> String {
    let query = format!("dcPath={}&amp;dsName={}", encode(DATACENTER), encode(ds));
    let parent = if dir.is_empty() {
        format!(
            "<a href=\"/folder?dcPath={}\">Parent Datacenter</a>",
            encode(DATACENTER)
        )
    } else {
        let up = dir.rsplit_once('/').map_or("", |(p, _)| p);
        let up = if up.is_empty() {
            String::new()
        } else {
            format!("/{}", encode(up))
        };
        format!("<a href=\"/folder{up}?{query}\">Parent Directory</a>")
    };
    let rows: String = children
        .iter()
        .map(|name| {
            let path = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            format!(
                "<tr><td><a href=\"/folder/{}?{query}\">{}</a></td><td align=\"right\">04-Nov-2026 10:00</td><td align=\"right\">  3 KB</td></tr>\n",
                encode(&path),
                escape(name)
            )
        })
        .collect();
    page(
        &format!("Index of {}", escape(&format!("[{ds}] {dir}"))),
        &format!(
            "<table>\n<tr><th>Name</th><th>Last modified</th><th>Size</th></tr>\n<tr><td colspan=\"3\">{parent}</td></tr>\n{rows}</table>\n"
        ),
    )
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\">\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"en\" lang=\"en\">\n\
         <head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\" /><title>{title}</title></head>\n\
         <body>\n<h1>{title}</h1>\n{body}</body>\n</html>\n"
    )
}

fn html(body: String) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(body))
        .unwrap()
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .unwrap()
}

/// ESXi처럼 영숫자와 `/`·`.`·`_` 밖은 소문자 `%xx`로 쓴다(`ha%2ddatacenter`).
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/._".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02x}")
            }
        })
        .collect()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}
