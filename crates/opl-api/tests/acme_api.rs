//! ACME API(계획 73 §11·§13, CA 없이): 계정·DNS 공급자·인증서의 가린 보기, 검사(HTTP-01 리스너·키 종류·참조), 지우기 막힘,
//! 기능 게이트, 지금 갱신 Task, 설정 `acme`, HTTP-01 리스너의 답·리다이렉트, 끼우기(SNI)·돌아가기, DNS 공급자 셋의 요청 꼴
//! (같은 프로세스의 가짜 Cloudflare·PowerDNS·Route 53 — SigV4 확인).

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::response::IntoResponse;
use common::acme::{TestCa, add_feature, drop_feature, seed_account, served_serial};
use common::{Env, WAIT};
use opl_api::ApiCertAgent;
use opl_api::acme::AcmeNodeConfig;
use opl_api::acme::dns::{DnsEndpoints, Provider};
use opl_types::acme::{AcmeDnsSpec, CertificateSpec, Http01Answer, IssuedCert, Secret};
use opl_types::node::{ACME, HTTP01};
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn accounts_dns_and_certificates_are_redacted_and_checked() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("v", "viewer").await;
    seed_account(&env.cluster, "le", "https://127.0.0.1:1/dir").await;

    // 계정 보기: 키 없음
    let (status, body) = admin.get("/acme-accounts/le").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("key").is_none(), "{body}");
    assert!(!body.to_string().contains("PRIVATE KEY"));

    // DNS 공급자: 비밀은 응답에 없다. 뷰어는 읽기만.
    let dns = json!({ "name": "cf", "provider": { "kind": "cloudflare", "api_token": "tok-secret" },
                      "zones": ["example.com"] });
    let (status, _) = viewer.post("/acme-dns", dns.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = admin.post("/acme-dns", dns).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(!body.to_string().contains("tok-secret"), "{body}");
    let (status, body) = viewer.get("/acme-dns").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["spec"]["provider"]["kind"], "cloudflare");
    assert!(!body.to_string().contains("tok-secret"));
    // 틀린 TSIG 비밀(base64 아님)은 만들 때 거절한다.
    let (status, body) = admin
        .post(
            "/acme-dns",
            json!({ "name": "bind", "provider": { "kind": "rfc2136", "server": "10.0.0.53:53",
                    "key_name": "k", "secret": "not base64!" } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 인증서: 참조·키 종류·HTTP-01 리스너
    let cert = |name: &str, extra: Value| {
        let mut c = json!({ "name": name, "account": "le", "names": ["{node}.example.com"],
                            "challenge": { "kind": "http01" } });
        c.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        c
    };
    let (status, body) = admin
        .post("/certificates", cert("a", json!({ "account": "nope" })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = admin
        .post("/certificates", cert("a", json!({ "key": "rsa2048" })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("rsa2048"));
    let (status, body) = admin.post("/certificates", cert("a", json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "http01_unavailable", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("node1"),
        "{body}"
    );
    add_feature(&env.cluster, env.node, HTTP01).await;
    let (status, body) = admin.post("/certificates", cert("a", json!({}))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 모르는 칸은 거절
    let (status, body) = admin
        .post("/certificates", cert("b", json!({ "bogus": 1 })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"].as_str().unwrap().contains("bogus"),
        "{body}"
    );
    // 공유 + DNS-01: 공유 키는 보기에 없다.
    let (status, body) = admin
        .post(
            "/certificates",
            json!({ "name": "vip", "account": "le", "names": ["*.example.com", "example.com"],
                    "scope": { "kind": "shared" }, "challenge": { "kind": "dns01", "dns": "cf" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let vip = env
        .cluster
        .get_by_name::<CertificateSpec>("vip")
        .unwrap()
        .unwrap();
    let mut st = vip.status.clone();
    st.shared_key = Secret::new("-----BEGIN PRIVATE KEY-----shared");
    env.cluster
        .update_status::<CertificateSpec>(vip.id, &st)
        .await
        .unwrap();
    let (_, body) = viewer.get("/certificates/vip").await;
    assert!(!body.to_string().contains("PRIVATE KEY"), "{body}");
    let (_, body) = viewer.get("/certificates").await;
    assert!(!body.to_string().contains("PRIVATE KEY"), "{body}");

    // 바꾸기: 프로필을 넣고 지운다.
    let (status, body) = admin
        .patch("/certificates/a", json!({ "profile": "shortlived" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["profile"], "shortlived");
    let (_, body) = admin
        .patch("/certificates/a", json!({ "profile": null }), None)
        .await;
    assert!(body["spec"].get("profile").is_none(), "{body}");

    // 지우기 막힘
    let (status, body) = admin.delete("/acme-accounts/le").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "account_in_use");
    let (status, body) = admin.delete("/acme-dns/cf").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "dns_in_use");

    // 지금 갱신: 노드마다 → 이 노드의 Task 하나, 두 번째는 409
    let (status, body) = admin.post("/certificates/a/actions/renew", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "{body}");
    assert_eq!(tasks[0]["spec"]["op"]["kind"], "acme-order", "{body}");
    assert_eq!(
        tasks[0]["spec"]["op"]["node"],
        env.node.to_string(),
        "{body}"
    );
    let (status, body) = admin.post("/certificates/a/actions/renew", json!({})).await;
    // 첫 Task가 이미 끝났을 수도 있다(CA가 없어 곧 실패).
    assert!(
        status == StatusCode::CONFLICT || status == StatusCode::ACCEPTED,
        "{status} {body}"
    );
    // 실패한 주문은 물러서기와 오류를 남긴다.
    let deadline = Instant::now() + WAIT;
    loop {
        let c = env
            .cluster
            .get_by_name::<CertificateSpec>("a")
            .unwrap()
            .unwrap();
        if c.status.failures >= 1 {
            // 노드마다 인증서: 물러서기는 그 노드의 것, 위 칸은 요약(오류에 노드 이름)
            let f = &c.status.node_failures[&env.node];
            assert!(f.next_attempt_at.is_some());
            assert!(c.status.next_attempt_at.is_none());
            let node = env
                .cluster
                .get::<opl_types::node::NodeSpec>(env.node)
                .unwrap()
                .unwrap();
            assert!(
                c.status
                    .error
                    .as_deref()
                    .is_some_and(|e| e.starts_with(&format!("{}: ", node.name))),
                "{:?}",
                c.status.error
            );
            break;
        }
        assert!(Instant::now() < deadline, "the order never failed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 설정 acme
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "acme": { "http_redirect": false } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["acme"],
        json!({ "http_redirect": false, "renew_jitter": true })
    );
    let (_, body) = admin
        .patch(
            "/settings",
            json!({ "acme": { "http_redirect": true } }),
            None,
        )
        .await;
    assert!(body["spec"].get("acme").is_none(), "{body}");

    // 기능 게이트: acme를 모르는 노드가 있으면 쓰기는 409
    drop_feature(&env.cluster, env.node, ACME).await;
    let (status, body) = admin.post("/certificates", cert("c", json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    let (status, _) = admin
        .patch(
            "/settings",
            json!({ "acme": { "renew_jitter": false } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// HTTP-01 리스너: Raft의 답(살아 있는 것만), 그 밖은 https로 308(끄면 404).
#[tokio::test]
async fn http01_listener_answers_from_raft_and_redirects() {
    let env = Env::start().await;
    seed_account(&env.cluster, "le", "https://127.0.0.1:1/dir").await;
    let spec: CertificateSpec = serde_json::from_value(
        json!({ "account": "le", "names": ["a.example.com"], "challenge": { "kind": "http01" } }),
    )
    .unwrap();
    let mut cert: opl_types::acme::Certificate = opl_types::resource::Resource::new("a", spec);
    let now = opl_api_now();
    cert.status.challenges = vec![
        Http01Answer {
            token: "tok_1".into(),
            key_authorization: "tok_1.thumb".into(),
            until: now + 600,
        },
        Http01Answer {
            token: "old".into(),
            key_authorization: "old.thumb".into(),
            until: now - 1,
        },
    ];
    env.cluster.create(&cert).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server =
        opl_api::acme::http01::Http01Server::spawn(listener, env.cluster.clone(), 8006).unwrap();
    let base = format!("http://{}", server.local);
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = http
        .get(format!("{base}/.well-known/acme-challenge/tok_1"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.text().await.unwrap(), "tok_1.thumb");
    for gone in ["old", "missing", "bad%20token"] {
        let r = http
            .get(format!("{base}/.well-known/acme-challenge/{gone}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{gone}");
    }
    let r = http
        .get(format!("{base}/ui/vms?x=1"))
        .header("host", "web.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        r.headers()["location"],
        "https://web.example.com:8006/ui/vms?x=1"
    );
    let r = http.post(format!("{base}/x")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);
    // 리다이렉트를 끄면 404
    let (status, _) = env
        .admin()
        .await
        .patch(
            "/settings",
            json!({ "acme": { "http_redirect": false } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let r = http
        .get(format!("{base}/ui"))
        .header("host", "web.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    server.shutdown().await;
}

fn opl_api_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// 받은 ACME 인증서를 끼운다: 노드마다 인증서가 기본, 공유 인증서는 SNI로, 지우면 내부 인증서로 돌아간다.
#[tokio::test]
async fn acme_certificates_are_installed_and_picked_by_sni() {
    let env = Env::start().await;
    let ca = TestCa::new("Test ACME Root");
    seed_account(&env.cluster, "le", "https://127.0.0.1:1/dir").await;
    let keys = tempfile::TempDir::new().unwrap();
    // 리스너: 노드 인증서로 시작(내부 CA)
    let info = env
        .cluster
        .list::<opl_types::cluster::ClusterSpec>()
        .unwrap()
        .1
        .remove(0);
    let cluster_ca = opl_peer::pki::ClusterCa {
        cert_pem: info.spec.ca_cert_pem.clone(),
        key_pem: info.spec.ca_key_pem,
    };
    let (node_key, csr) = opl_peer::pki::node_key_and_csr().unwrap();
    let tls = opl_peer::pki::NodeTls {
        cert_pem: cluster_ca
            .sign_node(&csr, env.node, &["127.0.0.1".into()])
            .unwrap(),
        ca_pem: cluster_ca.cert_pem.clone(),
        key_pem: node_key,
    };
    let cert = opl_peer::tls::ApiCert::new(&tls.cert_pem, &tls.key_pem).unwrap();
    let agent = ApiCertAgent::spawn_with_acme(
        env.cluster.clone(),
        tls,
        cert.clone(),
        Some(keys.path().to_path_buf()),
    );
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let listener = opl_peer::tls::TlsListener::new(
        tcp,
        opl_peer::tls::api_server_config_with(cert.clone()).unwrap(),
    )
    .unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::Router::new().route("/", axum::routing::get(|| async { "ok" })),
        )
        .await;
    });
    assert!(
        served_serial(addr, "node1.example.com", &ca.pem)
            .await
            .is_none()
    );

    let issued = |names: &[&str], pem: String| IssuedCert {
        serial: opl_api::acme::ari::leaf(&pem).unwrap().serial,
        not_before: 0,
        not_after: opl_api_now() + 86_400,
        names: names.iter().map(|s| s.to_string()).collect(),
        chain_pem: pem,
        ari: None,
        renew_at: opl_api_now() + 3600,
        key: Default::default(),
        error: None,
        ari_checked_at: None,
    };
    // 공유 인증서(키는 Raft)
    let (vip_pem, vip_key) = ca.issue(&["vip.example.com"]);
    let vip_serial = opl_api::acme::ari::leaf(&vip_pem).unwrap().serial;
    let spec: CertificateSpec =
        serde_json::from_value(json!({ "account": "le", "names": ["vip.example.com"],
        "scope": { "kind": "shared" }, "challenge": { "kind": "http01" } }))
        .unwrap();
    let mut shared: opl_types::acme::Certificate = opl_types::resource::Resource::new("vip", spec);
    shared.status.shared = Some(issued(&["vip.example.com"], vip_pem));
    shared.status.shared_key = Secret::new(vip_key);
    env.cluster.create(&shared).await.unwrap();
    // 노드마다 인증서(키는 노드 디스크)
    let (node_pem, node_key) = ca.issue(&["node1.example.com"]);
    let node_serial = opl_api::acme::ari::leaf(&node_pem).unwrap().serial;
    std::fs::write(
        opl_api::acme::order::key_path(keys.path(), uuid::Uuid::nil(), "1"),
        "unused",
    )
    .unwrap();
    let spec: CertificateSpec =
        serde_json::from_value(json!({ "account": "le", "names": ["{node}.example.com"],
        "challenge": { "kind": "http01" } }))
        .unwrap();
    let mut per: opl_types::acme::Certificate = opl_types::resource::Resource::new("per", spec);
    std::fs::write(
        opl_api::acme::order::key_path(keys.path(), per.id, &node_serial),
        node_key,
    )
    .unwrap();
    per.status
        .nodes
        .insert(env.node, issued(&["node1.example.com"], node_pem));
    env.cluster.create(&per).await.unwrap();

    let wait_serial = |name: &'static str, want: Option<String>| {
        let root = ca.pem.clone();
        async move {
            let deadline = Instant::now() + WAIT;
            loop {
                let got = served_serial(addr, name, &root).await;
                if got == want {
                    return;
                }
                assert!(Instant::now() < deadline, "{name}: {got:?} != {want:?}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    wait_serial("node1.example.com", Some(node_serial.clone())).await;
    wait_serial("vip.example.com", Some(vip_serial.clone())).await;
    // SNI 없는(IP) 손맞춤은 기본 — 노드마다 인증서
    assert_eq!(served_serial(addr, "127.0.0.1", &ca.pem).await, None);
    // 지우면 내부 인증서로 돌아간다(노드 이름 127.0.0.1이 다시 통과).
    env.cluster
        .delete::<CertificateSpec>(per.id, None)
        .await
        .unwrap();
    env.cluster
        .delete::<CertificateSpec>(shared.id, None)
        .await
        .unwrap();
    wait_serial("vip.example.com", None).await;
    assert!(
        served_serial(addr, "127.0.0.1", &cluster_ca.cert_pem)
            .await
            .is_some()
    );
    agent.shutdown().await;
}

/// 가짜 공급자 서버가 본 요청
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<SeenRequest>>>);

/// (메서드, 경로와 질의, 머리, 본문)
type SeenRequest = (String, String, Vec<(String, String)>, String);

async fn record(State(seen): State<Seen>, req: Request) -> impl IntoResponse {
    let method = req.method().to_string();
    let uri = req.uri().to_string();
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body).to_string();
    seen.0
        .lock()
        .unwrap()
        .push((method.clone(), uri.clone(), headers, body));
    let path = uri.split('?').next().unwrap_or("").to_string();
    match (method.as_str(), path.as_str()) {
        // Cloudflare
        ("GET", "/cf/zones") => axum::Json(json!({ "success": true, "result": [{ "id": "Z1" }] })).into_response(),
        ("POST", "/cf/zones/Z1/dns_records") => {
            axum::Json(json!({ "success": true, "result": { "id": "R1" } })).into_response()
        }
        ("DELETE", "/cf/zones/Z1/dns_records/R1") => axum::Json(json!({ "success": true, "result": {} })).into_response(),
        // PowerDNS
        ("GET", "/api/v1/servers/localhost/zones") => {
            axum::Json(json!([{ "name": "example.com." }, { "name": "sub.example.com." }])).into_response()
        }
        // 다른 쪽이 둔 TXT 값 "keep"(다른 주문·SPF) — 우리 값만 더하고 빼야 한다.
        ("GET", "/api/v1/servers/localhost/zones/sub.example.com.") => axum::Json(json!({ "rrsets": [
            { "name": "_acme-challenge.a.sub.example.com.", "type": "TXT", "ttl": 120,
              "records": [{ "content": "\"keep\"", "disabled": false }] }] })).into_response(),
        ("PATCH", "/api/v1/servers/localhost/zones/sub.example.com.") => StatusCode::NO_CONTENT.into_response(),
        // Route 53
        ("GET", "/2013-04-01/hostedzonesbyname") => (
            StatusCode::OK,
            "<ListHostedZonesByNameResponse><HostedZones><HostedZone><Id>/hostedzone/H1</Id><Name>example.com.</Name></HostedZone></HostedZones></ListHostedZonesByNameResponse>",
        )
            .into_response(),
        ("GET", "/2013-04-01/hostedzone/H1/rrset") => (
            StatusCode::OK,
            "<ListResourceRecordSetsResponse><ResourceRecordSets><ResourceRecordSet><Name>_acme-challenge.a.sub.example.com.</Name><Type>TXT</Type><TTL>120</TTL><ResourceRecords><ResourceRecord><Value>&quot;keep&quot;</Value></ResourceRecord></ResourceRecords></ResourceRecordSet></ResourceRecordSets></ListResourceRecordSetsResponse>",
        )
            .into_response(),
        ("POST", "/2013-04-01/hostedzone/H1/rrset") => (
            StatusCode::OK,
            "<ChangeResourceRecordSetsResponse><ChangeInfo><Id>/change/C1</Id><Status>PENDING</Status></ChangeInfo></ChangeResourceRecordSetsResponse>",
        )
            .into_response(),
        ("GET", "/2013-04-01/change/C1") => (
            StatusCode::OK,
            "<GetChangeResponse><ChangeInfo><Id>/change/C1</Id><Status>INSYNC</Status></ChangeInfo></GetChangeResponse>",
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::test]
async fn dns_providers_send_the_right_requests() {
    let seen = Seen::default();
    let app = axum::Router::new()
        .fallback(record)
        .with_state(seen.clone());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", tcp.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
    let node = AcmeNodeConfig {
        dns_endpoints: DnsEndpoints {
            cloudflare: format!("{base}/cf"),
            route53: base.clone(),
        },
        ..Default::default()
    };
    let fqdn = "_acme-challenge.a.sub.example.com";
    let values = vec!["v1".to_string(), "v2".to_string()];

    // Cloudflare: 영역 찾기(긴 접미사부터) → 값마다 레코드 → 지우기, Bearer 토큰
    let spec: AcmeDnsSpec =
        serde_json::from_value(json!({ "provider": { "kind": "cloudflare", "api_token": "T" } }))
            .unwrap();
    let p = Provider::new(&spec, &node).unwrap();
    let h = p.add(fqdn, &values, &[]).await.unwrap();
    p.remove(fqdn, &values, &h).await.unwrap();
    {
        let s = seen.0.lock().unwrap();
        assert_eq!(s[0].0, "GET");
        assert!(
            s[0].1.contains("name=_acme-challenge.a.sub.example.com"),
            "{}",
            s[0].1
        );
        assert!(
            s[0].2
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer T")
        );
        let posts: Vec<_> = s.iter().filter(|r| r.0 == "POST").collect();
        assert_eq!(posts.len(), 2);
        let b: Value = serde_json::from_str(&posts[0].3).unwrap();
        assert_eq!(
            b,
            json!({ "type": "TXT", "name": fqdn, "content": "v1", "ttl": 60 })
        );
        assert_eq!(s.iter().filter(|r| r.0 == "DELETE").count(), 2);
    }
    seen.0.lock().unwrap().clear();

    // PowerDNS: 가장 긴 영역, 있던 "keep"에 값 둘을 더해 REPLACE, 지울 때는 "keep"만 남겨 REPLACE
    let spec: AcmeDnsSpec = serde_json::from_value(
        json!({ "provider": { "kind": "power_dns", "url": base, "api_key": "K" } }),
    )
    .unwrap();
    let p = Provider::new(&spec, &node).unwrap();
    let h = p.add(fqdn, &values, &[]).await.unwrap();
    p.remove(fqdn, &values, &h).await.unwrap();
    {
        let s = seen.0.lock().unwrap();
        assert!(
            s.iter()
                .all(|r| r.2.iter().any(|(k, v)| k == "x-api-key" && v == "K"))
        );
        let patches: Vec<Value> = s
            .iter()
            .filter(|r| r.0 == "PATCH")
            .map(|r| serde_json::from_str(&r.3).unwrap())
            .collect();
        assert_eq!(patches[0]["rrsets"][0]["changetype"], "REPLACE");
        assert_eq!(patches[0]["rrsets"][0]["name"], format!("{fqdn}."));
        assert_eq!(patches[0]["rrsets"][0]["ttl"], 120);
        let contents = |p: &Value| -> Vec<String> {
            p["rrsets"][0]["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["content"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(contents(&patches[0]), ["\"keep\"", "\"v1\"", "\"v2\""]);
        // 지우기: 서버는 아직 "keep"만 돌려주므로(가짜) 우리 값이 없어 아무것도 쓰지 않는다 — 여기서는 읽기만 확인
        assert_eq!(patches.len(), 1, "nothing of ours to remove: {patches:?}");
        assert!(
            s.iter()
                .any(|r| r.0 == "GET" && r.1.contains("zones/sub.example.com.?rrset_name="))
        );
    }
    seen.0.lock().unwrap().clear();

    // Route 53: SigV4 서명이 맞는지 같은 비밀로 다시 셈한다.
    let spec: AcmeDnsSpec = serde_json::from_value(json!({ "provider": { "kind": "route53",
        "access_key_id": "AKID", "secret_access_key": "SECRET" }, "zones": ["example.com"] }))
    .unwrap();
    let p = Provider::new(&spec, &node).unwrap();
    let h = p.add(fqdn, &values, &spec.zones).await.unwrap();
    p.remove(fqdn, &values, &h).await.unwrap();
    let s = seen.0.lock().unwrap();
    // 넣기: 있던 rrset("keep", TTL 120)을 그대로 DELETE하고 우리 값을 더한 것을 CREATE(한 묶음)
    let post = s.iter().find(|r| r.0 == "POST").unwrap();
    assert!(
        post.3.contains("<Action>DELETE</Action>")
            && post.3.contains("<Action>CREATE</Action>")
            && post.3.contains("<TTL>120</TTL>")
            && post.3.matches("&quot;keep&quot;").count() == 2
            && post.3.contains("&quot;v2&quot;")
            && !post.3.contains("UPSERT"),
        "{}",
        post.3
    );
    // 지우기: 가짜 서버는 "keep"만 돌려주므로 우리 값이 없어 바꾸지 않는다.
    assert_eq!(s.iter().filter(|r| r.0 == "POST").count(), 1);
    for (method, uri, headers, body) in s.iter() {
        let get = |k: &str| {
            headers
                .iter()
                .find(|(h, _)| h == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        let auth = opl_store::backend::sigv4::parse_authorization(&get("authorization")).unwrap();
        assert_eq!(
            (
                auth.access_key_id.as_str(),
                auth.region.as_str(),
                auth.service.as_str()
            ),
            ("AKID", "us-east-1", "route53")
        );
        let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
        let query: Vec<(String, String)> = query
            .split('&')
            .filter(|q| !q.is_empty())
            .map(|q| {
                let (k, v) = q.split_once('=').unwrap();
                (k.to_string(), v.to_string())
            })
            .collect();
        let signed: Vec<(String, String)> = auth
            .signed_headers
            .iter()
            .map(|h| (h.clone(), get(h)))
            .collect();
        let (sig, _) = opl_store::backend::sigv4::signature(
            &opl_store::backend::sigv4::SignInput {
                method,
                canonical_uri: path,
                query: &query,
                headers: &signed,
                payload_hash: &opl_store::backend::sigv4::sha256_hex(body.as_bytes()),
                amz_date: &get("x-amz-date"),
                region: "us-east-1",
                service: "route53",
            },
            "SECRET",
        );
        assert_eq!(sig, auth.signature, "{method} {uri}");
    }
}
