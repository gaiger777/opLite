//! 실제 ACME 흐름(계획 73 §14, 기능 `acme-it`): Pebble(Let's Encrypt 시험 CA, `-strict`, 무작위 badNonce 그대로)과
//! pebble-challtestsrv(DNS — 모든 이름이 127.0.0.1, TXT는 관리 API)를 띄우고 우리 클라이언트·HTTP-01 리스너·DNS-01 훅·
//! 끼우기를 본다. 실행 파일 자리는 `OPL_TEST_PEBBLE_DIR`(`pebble`·`pebble-challtestsrv`), RFC 2136은 `OPL_TEST_BIND`
//! (`ip:port` — TSIG 키 `OPL_TEST_BIND_KEY`(base64, hmac-sha256, 키 이름 `acme-key`), 영역 `bind.test`)가 있을 때만.

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Env;
use common::acme::{TestCa, add_feature, served_serial};
use opl_api::ApiCertAgent;
use opl_api::acme::AcmeNodeConfig;
use opl_types::acme::CertificateSpec;
use opl_types::node::HTTP01;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

const LONG: Duration = Duration::from_secs(120);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn free_udp_port() -> u16 {
    // DNS는 UDP와 TCP를 함께 쓴다 — 둘 다 빈 것
    loop {
        let p = free_port();
        if std::net::UdpSocket::bind(("127.0.0.1", p)).is_ok() {
            return p;
        }
    }
}

/// `OPL_TEST_PEBBLE_LOG`가 있으면 Pebble·challtestsrv의 출력을 그대로 보인다.
fn log() -> Stdio {
    if std::env::var_os("OPL_TEST_PEBBLE_LOG").is_some() {
        Stdio::inherit()
    } else {
        Stdio::null()
    }
}

fn bin(name: &str) -> PathBuf {
    let dir = std::env::var("OPL_TEST_PEBBLE_DIR")
        .expect("set OPL_TEST_PEBBLE_DIR to the Pebble binaries");
    Path::new(&dir).join(name)
}

/// Pebble 한 벌
struct Pebble {
    directory: String,
    /// Pebble WFE의 TLS를 믿을 시험 CA(계정 `ca_bundle`)
    wfe_ca: String,
    /// Pebble이 서명하는 루트(받은 사슬을 확인)
    root: String,
    dns: SocketAddr,
    cts_mgmt: SocketAddr,
    _children: Vec<Child>,
    _dir: tempfile::TempDir,
}

impl Pebble {
    async fn start(http_port: u16, eab: bool) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let ca = TestCa::new("Pebble WFE Test CA");
        let (cert, key) = ca.issue(&["localhost", "127.0.0.1"]);
        std::fs::write(dir.path().join("cert.pem"), cert).unwrap();
        std::fs::write(dir.path().join("key.pem"), key).unwrap();
        let (dns_port, cts_port) = (free_udp_port(), free_port());
        let cts = Command::new(bin("pebble-challtestsrv"))
            .args([
                "-defaultIPv4",
                "127.0.0.1",
                "-defaultIPv6",
                "",
                "-dnsserver",
                &format!("127.0.0.1:{dns_port}"),
                "-management",
                &format!("127.0.0.1:{cts_port}"),
                "-http01",
                "",
                "-https01",
                "",
                "-tlsalpn01",
                "",
                "-doh",
                "",
            ])
            .stdout(log())
            .stderr(log())
            .kill_on_drop(true)
            .spawn()
            .expect("start pebble-challtestsrv");
        let (wfe, mgmt) = (free_port(), free_port());
        let mut config = json!({ "pebble": {
            "listenAddress": format!("127.0.0.1:{wfe}"),
            "managementListenAddress": format!("127.0.0.1:{mgmt}"),
            "certificate": dir.path().join("cert.pem"),
            "privateKey": dir.path().join("key.pem"),
            "httpPort": http_port,
            "tlsPort": free_port(),
            "ocspResponderURL": "",
            "externalAccountBindingRequired": eab,
            "retryAfter": { "authz": 1, "order": 1 },
            "keyAlgorithm": "ecdsa",
            "profiles": {
                "default": { "description": "default", "validityPeriod": 7_776_000 },
                "shortlived": { "description": "short", "validityPeriod": 3600 }
            }
        }});
        if eab {
            config["pebble"]["externalAccountMACKeys"] = json!({ "kid-1": "zWNDZM6eQGHWpSRTPal5eIUYFTu7EajVIoguysqZ9wG44nMEtx3MUAsUDkMTQ12W" });
        }
        let path = dir.path().join("pebble.json");
        std::fs::write(&path, config.to_string()).unwrap();
        let pebble = Command::new(bin("pebble"))
            .args(["-strict", "-config"])
            .arg(&path)
            .args(["-dnsserver", &format!("127.0.0.1:{dns_port}")])
            .env("PEBBLE_VA_NOSLEEP", "1")
            .stdout(log())
            .stderr(log())
            .kill_on_drop(true)
            .spawn()
            .expect("start pebble");
        let http = reqwest::Client::builder()
            .user_agent("opl-acme-test")
            .tls_certs_only(reqwest::Certificate::from_pem_bundle(ca.pem.as_bytes()).unwrap())
            .build()
            .unwrap();
        let directory = format!("https://127.0.0.1:{wfe}/dir");
        let deadline = Instant::now() + Duration::from_secs(20);
        let root = loop {
            let ready = http
                .get(&directory)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            if ready
                && let Ok(r) = http
                    .get(format!("https://127.0.0.1:{mgmt}/roots/0"))
                    .send()
                    .await
                && r.status().is_success()
            {
                break r.text().await.unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "Pebble did not start: {:?}",
                http.get(&directory).send().await.map(|r| r.status())
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        Pebble {
            directory,
            wfe_ca: ca.pem,
            root,
            dns: format!("127.0.0.1:{dns_port}").parse().unwrap(),
            cts_mgmt: format!("127.0.0.1:{cts_port}").parse().unwrap(),
            _children: vec![cts, pebble],
            _dir: dir,
        }
    }
}

/// Pebble 하나·HTTP-01 리스너·노드 하나
struct Rig {
    env: Env,
    pebble: Pebble,
    keys: tempfile::TempDir,
    hooks: tempfile::TempDir,
    _http01: opl_api::acme::http01::Http01Server,
}

impl Rig {
    async fn start(eab: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pebble = Pebble::start(port, eab).await;
        let (keys, hooks) = (
            tempfile::TempDir::new().unwrap(),
            tempfile::TempDir::new().unwrap(),
        );
        let config = AcmeNodeConfig {
            key_dir: Some(keys.path().to_path_buf()),
            dns_resolver: Some(pebble.dns),
            hook_root: hooks.path().to_path_buf(),
            poll_limit: Duration::from_secs(60),
            ..Default::default()
        };
        let env = Env::start_with_state(move |s| s.acme = config).await;
        add_feature(&env.cluster, env.node, HTTP01).await;
        let http01 =
            opl_api::acme::http01::Http01Server::spawn(listener, env.cluster.clone(), 8006)
                .unwrap();
        Rig {
            env,
            pebble,
            keys,
            hooks,
            _http01: http01,
        }
    }

    fn account_body(&self) -> Value {
        json!({ "name": "le", "directory": self.pebble.directory, "email": "ops@example.com",
                "agree_tos": true, "ca_bundle": self.pebble.wfe_ca })
    }

    /// challtestsrv에 TXT를 넣고 지우는 훅(`/etc/opl/acme-hooks/{name}`)
    fn hook(&self, name: &str, fail: bool) -> String {
        use std::os::unix::fs::PermissionsExt;
        let m = self.pebble.cts_mgmt;
        let body = if fail {
            "echo \"no credentials for $2\" >&2\nexit 1\n".to_string()
        } else {
            format!(
                "case \"$1\" in\n\
                 add) curl -sf -X POST -d \"{{\\\"host\\\":\\\"$2.\\\",\\\"value\\\":\\\"$3\\\"}}\" http://{m}/set-txt ;;\n\
                 remove) curl -sf -X POST -d \"{{\\\"host\\\":\\\"$2.\\\"}}\" http://{m}/clear-txt ;;\n\
                 esac\n"
            )
        };
        let path = self.hooks.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        format!("/etc/opl/acme-hooks/{name}")
    }
}

/// 지금 주문하고 Task들이 끝나기를 기다린다. 마지막 Task 모습들.
async fn renew(admin: &common::Client, cert: &str) -> Vec<Value> {
    let (status, body) = admin
        .post(&format!("/certificates/{cert}/actions/renew"), json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let mut done = Vec::new();
    for t in body["tasks"].as_array().unwrap() {
        let id = t["id"].as_str().unwrap();
        let deadline = Instant::now() + LONG;
        loop {
            let (_, task) = admin.get(&format!("/tasks/{id}")).await;
            let state = task["status"]["state"].as_str().unwrap_or("");
            if state == "succeeded" || state == "failed" {
                done.push(task);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the order did not finish: {task}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    done
}

fn cert(env: &Env, name: &str) -> opl_types::acme::Certificate {
    env.cluster
        .get_by_name::<CertificateSpec>(name)
        .unwrap()
        .unwrap()
}

/// HTTP-01 노드마다: 계정 등록, 주문(Raft의 답을 리스너가 준다), 키는 노드 디스크에만, 끼우기(SNI), ARI 창, 갱신
/// (`replaces`) 뒤 새 일련번호로 바꿔 끼우기 — 열린 연결은 그대로.
#[tokio::test]
async fn http01_per_node_issue_install_and_renew() {
    let rig = Rig::start(false).await;
    let admin = rig.env.admin().await;
    let (status, body) = admin.post("/acme-accounts", rig.account_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        body["status"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://127.0.0.1"),
        "{body}"
    );
    assert!(body["spec"].get("key").is_none());
    // 다시 확인해도 같은 계정
    let (status, again) = admin
        .post("/acme-accounts/le/actions/check", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["status"]["url"], body["status"]["url"]);
    assert_eq!(
        again["status"]["profiles"],
        json!(["default", "shortlived"])
    );

    let (status, body) = admin
        .post(
            "/certificates",
            json!({ "name": "web", "account": "le", "names": ["{node}.acme.test"],
                    "challenge": { "kind": "http01" }, "precheck": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tasks = renew(&admin, "web").await;
    assert_eq!(tasks[0]["status"]["state"], "succeeded", "{}", tasks[0]);
    let c = cert(&rig.env, "web");
    let issued = c.status.nodes.get(&rig.env.node).expect("issued").clone();
    assert_eq!(issued.names, ["node1.acme.test"]);
    assert!(issued.ari.is_some(), "Pebble serves ARI windows");
    assert!(issued.renew_at > issued.not_before && issued.renew_at < issued.not_after);
    assert!(
        c.status.challenges.is_empty(),
        "answers are removed after validation"
    );
    assert!(c.status.error.is_none() && c.status.failures == 0);
    // 키는 노드 디스크(0600)에만 — Raft 객체에는 없다.
    let key = opl_api::acme::order::key_path(rig.keys.path(), c.id);
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(!serde_json::to_string(&c).unwrap().contains("PRIVATE KEY"));

    // 끼우기: SNI 이름으로 Pebble 루트까지 확인된다.
    let info = rig
        .env
        .cluster
        .list::<opl_types::cluster::ClusterSpec>()
        .unwrap()
        .1
        .remove(0);
    let ca = opl_peer::pki::ClusterCa {
        cert_pem: info.spec.ca_cert_pem,
        key_pem: info.spec.ca_key_pem,
    };
    let (node_key, csr) = opl_peer::pki::node_key_and_csr().unwrap();
    let tls = opl_peer::pki::NodeTls {
        cert_pem: ca
            .sign_node(&csr, rig.env.node, &["127.0.0.1".into()])
            .unwrap(),
        ca_pem: ca.cert_pem.clone(),
        key_pem: node_key,
    };
    let api_cert = opl_peer::tls::ApiCert::new(&tls.cert_pem, &tls.key_pem).unwrap();
    let agent = ApiCertAgent::spawn_with_acme(
        rig.env.cluster.clone(),
        tls,
        api_cert.clone(),
        Some(rig.keys.path().to_path_buf()),
    );
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let listener = opl_peer::tls::TlsListener::new(
        tcp,
        opl_peer::tls::api_server_config_with(api_cert).unwrap(),
    )
    .unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::Router::new().route("/", axum::routing::get(|| async { "ok" })),
        )
        .await;
    });
    let wait_serial = |want: String| {
        let root = rig.pebble.root.clone();
        async move {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let got = served_serial(addr, "node1.acme.test", &root).await;
                if got.as_deref() == Some(want.as_str()) {
                    return;
                }
                assert!(Instant::now() < deadline, "served {got:?}, want {want}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    wait_serial(issued.serial.clone()).await;
    // 열어 둔 연결
    let mut roots = rustls::RootCertStore::empty();
    for c in opl_peer::pki::certs(&rig.pebble.root).unwrap() {
        roots.add(c).unwrap();
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let mut open = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(
            rustls_pki_types::ServerName::try_from("node1.acme.test".to_string()).unwrap(),
            tokio::net::TcpStream::connect(addr).await.unwrap(),
        )
        .await
        .unwrap();

    // 갱신: `replaces`로 다시 받아 새 일련번호를 끼운다.
    let tasks = renew(&admin, "web").await;
    assert_eq!(tasks[0]["status"]["state"], "succeeded", "{}", tasks[0]);
    let renewed = cert(&rig.env, "web").status.nodes[&rig.env.node].clone();
    assert_ne!(renewed.serial, issued.serial);
    wait_serial(renewed.serial.clone()).await;
    open.write_all(b"GET / HTTP/1.1\r\nhost: node1.acme.test\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    open.read_to_string(&mut response).await.unwrap();
    assert!(response.ends_with("ok"), "{response}");

    // 리더 루프: 받은 것이 살아 있으면 할 일이 없다.
    opl_api::acme::renewer::tick(&rig.env.state, &Default::default()).await;
    let (_, tasks) = rig.env.cluster.list::<opl_types::task::TaskSpec>().unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|t| matches!(t.spec.op, opl_types::task::TaskOp::AcmeOrder { .. }))
            .count(),
        2
    );
    agent.shutdown().await;
}

/// DNS-01 훅, 공유 인증서(와일드카드 + 기본 이름 — 같은 TXT 이름에 값 둘), 키는 Raft의 비밀(보기에는 없음), 프로필.
#[tokio::test]
async fn dns01_hook_shared_wildcard() {
    let rig = Rig::start(false).await;
    let admin = rig.env.admin().await;
    let (status, body) = admin.post("/acme-accounts", rig.account_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let path = rig.hook("cts.sh", false);
    let (status, body) = admin
        .post(
            "/acme-dns",
            json!({ "name": "cts", "provider": { "kind": "hook", "path": path },
                                    "propagation": { "timeout_s": 30 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            "/certificates",
            json!({ "name": "wild", "account": "le", "names": ["*.wild.test", "wild.test"],
                    "scope": { "kind": "shared" }, "challenge": { "kind": "dns01", "dns": "cts" },
                    "profile": "shortlived" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tasks = renew(&admin, "wild").await;
    assert_eq!(tasks[0]["status"]["state"], "succeeded", "{}", tasks[0]);
    let c = cert(&rig.env, "wild");
    let shared = c.status.shared.clone().expect("shared certificate");
    let leaf = opl_api::acme::ari::leaf(&shared.chain_pem).unwrap();
    let mut names = leaf.names.clone();
    names.sort();
    assert_eq!(names, ["*.wild.test", "wild.test"]);
    // 짧은 프로필(1시간)
    assert!(leaf.not_after - leaf.not_before <= 3700, "{leaf:?}");
    assert!(c.status.shared_key.expose().contains("PRIVATE KEY"));
    let (_, view) = admin.get("/certificates/wild").await;
    assert!(!view.to_string().contains("PRIVATE KEY"), "{view}");
    assert_eq!(view["status"]["shared"]["serial"], shared.serial);
}

/// EAB를 요구하는 CA: 묶지 않으면 거절, 묶으면 등록.
#[tokio::test]
async fn external_account_binding() {
    let rig = Rig::start(true).await;
    let admin = rig.env.admin().await;
    let (status, body) = admin.post("/acme-accounts", rig.account_body()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "acme_rejected", "{body}");
    let mut req = rig.account_body();
    req["eab"] = json!({ "kid": "kid-1", "hmac_key": "zWNDZM6eQGHWpSRTPal5eIUYFTu7EajVIoguysqZ9wG44nMEtx3MUAsUDkMTQ12W" });
    let (status, body) = admin.post("/acme-accounts", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["status"]["url"].is_string());
    assert_eq!(body["spec"]["eab"], json!({ "kid": "kid-1" }));
}

/// 실패한 주문: 오류·실패 수·물러서기를 status에 적고, 리더는 물러서는 동안 다시 주문하지 않는다.
#[tokio::test]
async fn failures_back_off() {
    let rig = Rig::start(false).await;
    let admin = rig.env.admin().await;
    let (status, _) = admin.post("/acme-accounts", rig.account_body()).await;
    assert_eq!(status, StatusCode::CREATED);
    let path = rig.hook("broken.sh", true);
    admin
        .post(
            "/acme-dns",
            json!({ "name": "broken", "provider": { "kind": "hook", "path": path } }),
        )
        .await;
    let (status, body) = admin
        .post("/certificates", json!({ "name": "bad", "account": "le", "names": ["bad.acme.test"],
                                       "scope": { "kind": "shared" }, "challenge": { "kind": "dns01", "dns": "broken" } }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tasks = renew(&admin, "bad").await;
    assert_eq!(tasks[0]["status"]["state"], "failed", "{}", tasks[0]);
    let c = cert(&rig.env, "bad");
    assert_eq!(c.status.failures, 1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(
        c.status.next_attempt_at.unwrap() >= now + 290,
        "{:?}",
        c.status
    );
    assert!(
        c.status
            .error
            .as_deref()
            .unwrap()
            .contains("no credentials for _acme-challenge.bad.acme.test"),
        "{:?}",
        c.status.error
    );
    opl_api::acme::renewer::tick(&rig.env.state, &Default::default()).await;
    let (_, tasks) = rig.env.cluster.list::<opl_types::task::TaskSpec>().unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|t| matches!(t.spec.op, opl_types::task::TaskOp::AcmeOrder { .. }))
            .count(),
        1
    );
}

/// RFC 2136(BIND + TSIG): `OPL_TEST_BIND`가 있을 때만(GitHub `acme` 작업). Pebble의 DNS를 BIND로 둔 두 번째 벌.
#[tokio::test]
async fn rfc2136_with_bind() {
    let (Ok(server), Ok(secret)) = (
        std::env::var("OPL_TEST_BIND"),
        std::env::var("OPL_TEST_BIND_KEY"),
    ) else {
        eprintln!("skipped: OPL_TEST_BIND is not set");
        return;
    };
    let bind: SocketAddr = server.parse().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut pebble = Pebble::start(port, false).await;
    // Pebble을 BIND에 묻게 다시 띄운다.
    pebble._children.pop();
    let config = pebble._dir.path().join("pebble.json");
    let wfe = pebble
        .directory
        .trim_start_matches("https://127.0.0.1:")
        .trim_end_matches("/dir")
        .to_string();
    pebble._children.push(
        Command::new(bin("pebble"))
            .args(["-strict", "-config"])
            .arg(&config)
            .args(["-dnsserver", &server])
            .env("PEBBLE_VA_NOSLEEP", "1")
            .stdout(log())
            .stderr(log())
            .kill_on_drop(true)
            .spawn()
            .unwrap(),
    );
    let http = reqwest::Client::builder()
        .user_agent("opl-acme-test")
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(pebble.wfe_ca.as_bytes()).unwrap())
        .build()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let up = http
            .get(format!("https://127.0.0.1:{wfe}/dir"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if up {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let config = AcmeNodeConfig {
        dns_resolver: Some(bind),
        poll_limit: Duration::from_secs(60),
        ..Default::default()
    };
    let env = Env::start_with_state(move |s| s.acme = config).await;
    let _http01 =
        opl_api::acme::http01::Http01Server::spawn(listener, env.cluster.clone(), 8006).unwrap();
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/acme-accounts",
            json!({ "name": "le", "directory": pebble.directory, "agree_tos": true,
                                         "ca_bundle": pebble.wfe_ca }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post("/acme-dns", json!({ "name": "bind", "provider": { "kind": "rfc2136", "server": server,
                                    "key_name": "acme-key", "secret": secret }, "zones": ["bind.test"] }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post("/certificates", json!({ "name": "b", "account": "le", "names": ["www.bind.test"],
                                       "scope": { "kind": "shared" }, "challenge": { "kind": "dns01", "dns": "bind" } }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tasks = renew(&admin, "b").await;
    assert_eq!(tasks[0]["status"]["state"], "succeeded", "{}", tasks[0]);
    assert!(cert(&env, "b").status.shared.is_some());
    // 지운 뒤 TXT가 남지 않는다.
    assert!(
        !opl_api::acme::dns::query::txt_present(bind, "_acme-challenge.www.bind.test", "x")
            .await
            .unwrap()
    );
}
