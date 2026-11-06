//! 시험용 가짜 LDAP 서버. `lber`로 BER을 읽고 쓴다. 단순 바인드, 검색(필터는 and·등호·있음만
//! 해석하고 나머지는 맞는 것이 없음), 언바인드, StartTLS 확장 동작만 안다. 시험 CA가 서명한
//! `127.0.0.1` 인증서로 처음부터 TLS(LDAPS)이거나, 평문으로 받아 StartTLS 뒤 TLS로 바꾼다.
//! StartTLS 모드에서 TLS 전에 온 바인드·검색은 13(confidentialityRequired)으로 거절한다.
//! 검색의 크기 제한은 기본으로 지키지 않고(모든 항목을 보냄) 받은 값만 기록한다.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use lber::common::TagClass;
use lber::structure::{PL, StructureTag};
use lber::structures::{ASNTag, Enumerated, Integer, OctetString, Sequence, Set, Tag};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

const STARTTLS_OID: &str = "1.3.6.1.4.1.1466.20037";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 처음부터 TLS(`ldaps://`)
    Ldaps,
    /// 평문으로 받아 StartTLS 뒤 TLS(`ldap://`)
    StartTls,
    /// 평문으로 받고 StartTLS를 2(protocolError)로 거절한다(`ldap://`).
    StartTlsRefused,
}

/// 디렉터리의 사용자 항목(`objectClass: person`)
#[derive(Debug, Clone)]
pub struct Person {
    pub uid: String,
    pub dn: String,
    pub password: String,
    /// `memberOf` 값들
    pub groups: Vec<String>,
}

impl Person {
    /// `uid=<uid>,ou=people,dc=example,dc=com`
    pub fn new(uid: &str, password: &str, groups: &[&str]) -> Self {
        Person {
            uid: uid.into(),
            dn: format!("uid={uid},ou=people,{BASE_DN}"),
            password: password.into(),
            groups: groups.iter().map(|g| g.to_string()).collect(),
        }
    }
}

pub const BASE_DN: &str = "dc=example,dc=com";
pub const SERVICE_DN: &str = "cn=svc,dc=example,dc=com";
pub const SERVICE_PASSWORD: &str = "svc-secret";

#[derive(Debug, Clone)]
pub struct Directory {
    /// 서비스 계정 (DN, 비밀번호)
    pub service: Option<(String, String)>,
    pub people: Vec<Person>,
}

impl Directory {
    /// [`SERVICE_DN`] 서비스 계정과 사람들
    pub fn with(people: Vec<Person>) -> Self {
        Directory {
            service: Some((SERVICE_DN.into(), SERVICE_PASSWORD.into())),
            people,
        }
    }
}

#[derive(Default)]
struct Stats {
    connections: AtomicUsize,
    binds: AtomicUsize,
    searches: AtomicUsize,
    /// 검색 필터의 등호 조건(`attr=value`, 받은 그대로)
    assertions: Mutex<Vec<String>>,
    /// 검색 요청의 크기 제한
    size_limits: Mutex<Vec<i64>>,
    abandons: AtomicUsize,
}

/// 시험이 고르는 서버의 이상한 동작
#[derive(Default)]
struct Quirks {
    /// 검색 결과를 이만큼만 보내고 4(sizeLimitExceeded)로 끝낸다(서버 쪽 제한).
    truncate_at: Mutex<Option<usize>>,
    /// 검색에 항목 대신 참조를 끝없이 천천히 보낸다.
    endless_references: std::sync::atomic::AtomicBool,
}

struct Site {
    mode: Mode,
    /// 시험이 도중에 바꿀 수 있다([`FakeLdap::set_groups`]).
    directory: Mutex<Directory>,
    acceptor: TlsAcceptor,
    stats: Stats,
    quirks: Quirks,
}

pub struct FakeLdap {
    /// `ldaps://127.0.0.1:<port>` 또는 `ldap://127.0.0.1:<port>`
    pub url: String,
    /// 서버 인증서에 서명한 CA(PEM)
    pub ca_pem: String,
    site: Arc<Site>,
    server: JoinHandle<()>,
}

impl FakeLdap {
    pub async fn start(mode: Mode, directory: Directory) -> FakeLdap {
        let ca = ClusterCa::generate("ldap").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let sans = ["127.0.0.1".to_string()];
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, uuid::Uuid::new_v4(), &sans).unwrap(),
            key_pem,
        };
        let config = opl_peer::tls::api_server_config(&tls).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let site = Arc::new(Site {
            mode,
            directory: Mutex::new(directory),
            acceptor: TlsAcceptor::from(Arc::new(config)),
            stats: Stats::default(),
            quirks: Quirks::default(),
        });
        let s = site.clone();
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = tcp.accept().await {
                tokio::spawn(connection(stream, s.clone()));
            }
        });
        let scheme = match mode {
            Mode::Ldaps => "ldaps",
            Mode::StartTls | Mode::StartTlsRefused => "ldap",
        };
        FakeLdap {
            url: format!("{scheme}://{addr}"),
            ca_pem: ca.cert_pem,
            site,
            server,
        }
    }

    pub fn connections(&self) -> usize {
        self.site.stats.connections.load(Ordering::SeqCst)
    }

    pub fn binds(&self) -> usize {
        self.site.stats.binds.load(Ordering::SeqCst)
    }

    pub fn searches(&self) -> usize {
        self.site.stats.searches.load(Ordering::SeqCst)
    }

    /// 지금까지 받은 검색 요청의 크기 제한(0은 제한 없음)
    pub fn size_limits(&self) -> Vec<i64> {
        self.site.stats.size_limits.lock().unwrap().clone()
    }

    pub fn abandons(&self) -> usize {
        self.site.stats.abandons.load(Ordering::SeqCst)
    }

    /// 검색 결과를 `n`개만 보내고 4(sizeLimitExceeded)로 끝낸다.
    pub fn truncate_at(&self, n: usize) {
        *self.site.quirks.truncate_at.lock().unwrap() = Some(n);
    }

    /// 검색에 참조를 끝없이 보낸다(하나하나는 시간 제한 안에 온다).
    pub fn send_endless_references(&self) {
        self.site
            .quirks
            .endless_references
            .store(true, Ordering::SeqCst);
    }

    /// 지금까지 받은 검색 필터의 등호 조건(`attr=value`)
    pub fn assertions(&self) -> Vec<String> {
        self.site.stats.assertions.lock().unwrap().clone()
    }

    /// `uid` 사람의 그룹을 바꾼다.
    pub fn set_groups(&self, uid: &str, groups: &[&str]) {
        let mut directory = self.site.directory.lock().unwrap();
        let person = directory
            .people
            .iter_mut()
            .find(|p| p.uid == uid)
            .expect("person");
        person.groups = groups.iter().map(|g| g.to_string()).collect();
    }
}

impl Drop for FakeLdap {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn connection(tcp: TcpStream, site: Arc<Site>) {
    site.stats.connections.fetch_add(1, Ordering::SeqCst);
    match site.mode {
        Mode::Ldaps => {
            if let Ok(tls) = site.acceptor.accept(tcp).await {
                serve(tls, &site, true).await;
            }
        }
        Mode::StartTls | Mode::StartTlsRefused => {
            if let Some(tcp) = serve(tcp, &site, false).await
                && let Ok(tls) = site.acceptor.accept(tcp).await
            {
                serve(tls, &site, true).await;
            }
        }
    }
}

/// 요청을 처리한다. TLS가 아닐 때 StartTLS를 받으면 성공을 답하고 스트림을 돌려준다.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    site: &Site,
    tls: bool,
) -> Option<S> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let message = loop {
            match lber::parse::parse_tag(&buf) {
                Ok((rest, tag)) => {
                    let used = buf.len() - rest.len();
                    buf.drain(..used);
                    break tag;
                }
                Err(_) => {
                    let mut chunk = [0u8; 4096];
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return None,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
            }
        };
        let mut parts = message.expect_constructed()?.into_iter();
        let id = uint(&parts.next()?.expect_primitive()?);
        let op = parts.next()?;
        let replies = match (op.class, op.id) {
            (TagClass::Application, 0) => vec![bind(site, tls, op)],
            (TagClass::Application, 2) => return None,
            (TagClass::Application, 3) => {
                let replies = search(site, tls, op);
                if tls && site.quirks.endless_references.load(Ordering::SeqCst) {
                    // 연결이 끊길 때까지
                    loop {
                        write(&mut stream, id, reference()).await?;
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    }
                }
                replies
            }
            (TagClass::Application, 16) => {
                site.stats.abandons.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            (TagClass::Application, 23) => {
                let oid = string(op.expect_constructed()?.into_iter().next()?);
                if oid == STARTTLS_OID && !tls && site.mode != Mode::StartTlsRefused {
                    write(&mut stream, id, extended(0, "")).await?;
                    return Some(stream);
                }
                vec![extended(2, "unsupported extended operation")]
            }
            _ => return None,
        };
        for reply in replies {
            write(&mut stream, id, reply).await?;
        }
    }
}

async fn write<S: AsyncWrite + Unpin>(stream: &mut S, id: u64, op: Tag) -> Option<()> {
    let message = Tag::Sequence(Sequence {
        inner: vec![
            Tag::Integer(Integer {
                inner: id as i64,
                ..Default::default()
            }),
            op,
        ],
        ..Default::default()
    });
    let mut out = BytesMut::new();
    lber::write::encode_into(&mut out, message.into_structure()).ok()?;
    stream.write_all(&out).await.ok()?;
    stream.flush().await.ok()
}

fn bind(site: &Site, tls: bool, op: StructureTag) -> Tag {
    site.stats.binds.fetch_add(1, Ordering::SeqCst);
    if !tls && site.mode != Mode::Ldaps {
        return response(1, 13, "TLS confidentiality required");
    }
    let mut parts = op.expect_constructed().unwrap().into_iter();
    let _version = parts.next();
    let name = string(parts.next().unwrap());
    let password = string(parts.next().unwrap());
    let directory = site.directory.lock().unwrap();
    let ok = (name.is_empty() && password.is_empty())
        || directory
            .service
            .as_ref()
            .is_some_and(|(dn, pw)| dn.eq_ignore_ascii_case(&name) && *pw == password)
        || directory
            .people
            .iter()
            .any(|p| p.dn.eq_ignore_ascii_case(&name) && p.password == password);
    if ok {
        response(1, 0, "")
    } else {
        response(1, 49, "invalid credentials")
    }
}

fn search(site: &Site, tls: bool, op: StructureTag) -> Vec<Tag> {
    site.stats.searches.fetch_add(1, Ordering::SeqCst);
    if !tls && site.mode != Mode::Ldaps {
        return vec![response(5, 13, "TLS confidentiality required")];
    }
    let parts: Vec<StructureTag> = op.expect_constructed().unwrap();
    let base = string(parts[0].clone()).to_ascii_lowercase();
    let size_limit = uint(&parts[3].clone().expect_primitive().unwrap()) as i64;
    site.stats.size_limits.lock().unwrap().push(size_limit);
    let filter = parts[6].clone();
    let wanted: Vec<String> = parts[7]
        .clone()
        .expect_constructed()
        .unwrap()
        .into_iter()
        .map(string)
        .collect();
    record(&site.stats, &filter);
    let mut replies: Vec<Tag> = site
        .directory
        .lock()
        .unwrap()
        .people
        .iter()
        .filter(|p| p.dn.to_ascii_lowercase().ends_with(&base))
        .filter(|p| matches(&filter, p))
        .map(|p| entry(p, &wanted))
        .collect();
    match *site.quirks.truncate_at.lock().unwrap() {
        Some(n) if replies.len() > n => {
            replies.truncate(n);
            replies.push(response(5, 4, "size limit exceeded"));
        }
        _ => replies.push(response(5, 0, "")),
    }
    replies
}

/// 검색 결과 참조(SearchResultReference)
fn reference() -> Tag {
    Tag::Sequence(Sequence {
        class: TagClass::Application,
        id: 19,
        inner: vec![octets("ldaps://elsewhere.example.com/dc=example,dc=com")],
    })
}

/// 사람 항목의 속성 값들
fn values(p: &Person, attr: &str) -> Vec<String> {
    match attr.to_ascii_lowercase().as_str() {
        "objectclass" => vec!["person".into()],
        "uid" => vec![p.uid.clone()],
        "memberof" => p.groups.clone(),
        _ => Vec::new(),
    }
}

fn matches(filter: &StructureTag, p: &Person) -> bool {
    if filter.class != TagClass::Context {
        return false;
    }
    match (filter.id, &filter.payload) {
        // and
        (0, PL::C(inner)) => inner.iter().all(|f| matches(f, p)),
        // 등호
        (3, PL::C(inner)) => {
            let attr = string(inner[0].clone());
            let value = string(inner[1].clone());
            values(p, &attr)
                .iter()
                .any(|v| v.eq_ignore_ascii_case(&value))
        }
        // 있음
        (7, PL::P(attr)) => !values(p, &String::from_utf8_lossy(attr)).is_empty(),
        _ => false,
    }
}

fn record(stats: &Stats, filter: &StructureTag) {
    match (filter.class, filter.id, &filter.payload) {
        (TagClass::Context, 0, PL::C(inner)) => inner.iter().for_each(|f| record(stats, f)),
        (TagClass::Context, 3, PL::C(inner)) => {
            let attr = string(inner[0].clone());
            let value = string(inner[1].clone());
            stats
                .assertions
                .lock()
                .unwrap()
                .push(format!("{attr}={value}"));
        }
        _ => {}
    }
}

fn entry(p: &Person, wanted: &[String]) -> Tag {
    let attrs = ["objectClass", "uid", "memberOf"]
        .into_iter()
        .filter(|a| wanted.is_empty() || wanted.iter().any(|w| w.eq_ignore_ascii_case(a)))
        .map(|a| {
            Tag::Sequence(Sequence {
                inner: vec![
                    octets(a),
                    Tag::Set(Set {
                        inner: values(p, a).iter().map(|v| octets(v)).collect(),
                        ..Default::default()
                    }),
                ],
                ..Default::default()
            })
        })
        .collect();
    Tag::Sequence(Sequence {
        class: TagClass::Application,
        id: 4,
        inner: vec![
            octets(&p.dn),
            Tag::Sequence(Sequence {
                inner: attrs,
                ..Default::default()
            }),
        ],
    })
}

/// 결과 코드만 있는 응답(`LDAPResult`)
fn response(op: u64, rc: i64, text: &str) -> Tag {
    Tag::Sequence(Sequence {
        class: TagClass::Application,
        id: op,
        inner: vec![
            Tag::Enumerated(Enumerated {
                inner: rc,
                ..Default::default()
            }),
            octets(""),
            octets(text),
        ],
    })
}

fn extended(rc: i64, text: &str) -> Tag {
    let Tag::Sequence(mut seq) = response(24, rc, text) else {
        unreachable!()
    };
    if rc == 0 {
        seq.inner.push(Tag::OctetString(OctetString {
            class: TagClass::Context,
            id: 10,
            inner: STARTTLS_OID.as_bytes().to_vec(),
        }));
    }
    Tag::Sequence(seq)
}

fn octets(s: &str) -> Tag {
    Tag::OctetString(OctetString {
        inner: s.as_bytes().to_vec(),
        ..Default::default()
    })
}

fn string(tag: StructureTag) -> String {
    String::from_utf8(tag.expect_primitive().unwrap_or_default()).unwrap()
}

fn uint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |n, b| (n << 8) | u64::from(*b))
}
