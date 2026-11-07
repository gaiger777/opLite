//! 원격 클러스터 API: 원격 역할을 하는 두 번째 `Env`(그 클러스터 CA가 서명한 인증서로 HTTPS 서비스)를
//! 등록하고 상태를 모으는지, 등록 확인 순서와 오류 코드(지문 틀림·자기 자신·토큰 틀림·꺼진 원격),
//! 토큰을 내보내지 않음과 PUT의 token 규칙, 권한, 옛 노드 409, `probe`, `refresh`, 상태 수집(바뀔 때만
//! 쓰기, 원격이 꺼지면 reachable false).

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Env, TlsServer, vm_request};
use opl_api::{RemotePoller, RemotePollerConfig};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::remote::{RemoteSpec, token_id};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 원격 역할을 하는 클러스터와 그 HTTPS 서버, 그곳 admin의 API 토큰
struct Remote {
    env: Env,
    server: TlsServer,
    token: String,
}

async fn remote() -> Remote {
    let env = Env::start().await;
    let server = env.serve_tls().await;
    let token = env.admin().await.token.unwrap();
    Remote { env, server, token }
}

fn body(name: &str, r: &Remote) -> Value {
    json!({
        "name": name,
        "url": r.server.url,
        "ca_fingerprint": r.env.ca_fingerprint(),
        "token": r.token,
    })
}

/// 서버가 `GET /cluster/ca`를 받은 수
fn hits(server: &TlsServer) -> usize {
    server.ca_hits.load(std::sync::atomic::Ordering::SeqCst)
}

fn stored(env: &Env, name: &str) -> RemoteSpec {
    env.cluster
        .get_by_name::<RemoteSpec>(name)
        .unwrap()
        .unwrap()
        .spec
}

/// 등록하면 원격을 확인하고 곧 상태를 모은다. 응답에는 토큰 대신 토큰의 id만 있다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remotes_are_checked_registered_and_read_without_the_token() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    // 끝 `/`와 대문자 지문은 정규화한다.
    let mut req = body("dr", &r);
    req["url"] = json!(format!("{}/", r.server.url));
    req["ca_fingerprint"] = json!(r.env.ca_fingerprint().to_uppercase());
    let (status, created) = admin.post("/remotes", req).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "dr");
    assert_eq!(created["url"], r.server.url);
    assert_eq!(created["ca_fingerprint"], r.env.ca_fingerprint());
    assert_eq!(created["token_id"], token_id(&r.token).unwrap().to_string());
    let st = &created["status"];
    assert_eq!(st["reachable"], true, "{created}");
    assert_eq!(st["error"], Value::Null, "{created}");
    assert_eq!(st["cluster_name"], "test");
    assert_eq!(st["version"], BUILD);
    assert_eq!(st["nodes"], json!({ "total": 1, "online": 1 }));
    assert_eq!(st["vms"], json!({ "total": 0, "running": 0 }));
    assert_eq!(st["cpus"], 4);
    assert_eq!(st["memory_gib"], 8);
    assert!(st["checked_at"].as_u64().is_some(), "{created}");
    assert_eq!(stored(&local, "dr").token, r.token);
    // 확인한 CA를 저장하고, 상태를 모을 때는 다시 받지 않는다.
    assert_eq!(stored(&local, "dr").ca_pem, r.env.ca_pem());
    assert_eq!(hits(&r.server), 1);

    let (status, list) = admin.get("/remotes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    let (status, got) = admin.get("/remotes/dr").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["revision"], created["revision"]);
    let (_, by_id) = admin
        .get(&format!("/remotes/{}", created["id"].as_str().unwrap()))
        .await;
    assert_eq!(by_id["name"], "dr");
    let secret = r.token.rsplit('_').next().unwrap();
    for shown in [&created, &list, &got] {
        assert!(shown.get("token").is_none(), "{shown}");
        assert!(!shown.to_string().contains(secret), "{shown}");
        assert!(!shown.to_string().contains("BEGIN CERTIFICATE"), "{shown}");
    }

    let (status, _) = admin.delete("/remotes/dr").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_error(
        &admin.get("/remotes/dr").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// 확인 순서: 검증 400 → CA를 받지 못하면 502 → 지문 다름 400 → 자기 자신 400 → 토큰 거절 400.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remotes_that_fail_a_check_are_not_kept() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;

    let mut bad_url = body("dr", &r);
    bad_url["url"] = json!(r.server.url.replace("https", "http"));
    let mut no_token = body("dr", &r);
    no_token.as_object_mut().unwrap().remove("token");
    let mut bad_token = body("dr", &r);
    bad_token["token"] = json!("not-a-token");
    for (req, why) in [
        (bad_url, "url"),
        (no_token, "token is required"),
        (bad_token, "token"),
        (body("probe", &r), "probe"),
        (body("0192f0c1-5d2e-7000-8000-000000000000", &r), "id"),
    ] {
        let got = admin.post("/remotes", req).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert!(
            got.1["message"].as_str().unwrap().contains(why),
            "{why}: {}",
            got.1
        );
    }

    // 닫힌 포트
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("https://{}", l.local_addr().unwrap())
    };
    let mut off = body("dr", &r);
    off["url"] = json!(closed);
    assert_error(
        &admin.post("/remotes", off).await,
        StatusCode::BAD_GATEWAY,
        "remote_unreachable",
    );

    let mut other_ca = body("dr", &r);
    other_ca["ca_fingerprint"] = json!(local.ca_fingerprint());
    let got = admin.post("/remotes", other_ca).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "remote_fingerprint_mismatch");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains(&r.env.ca_fingerprint()),
        "{}",
        got.1
    );

    // 이 클러스터 자신
    let me = local.serve_tls().await;
    let itself = json!({
        "name": "me",
        "url": me.url,
        "ca_fingerprint": local.ca_fingerprint(),
        "token": admin.token.clone().unwrap(),
    });
    assert_error(
        &admin.post("/remotes", itself).await,
        StatusCode::BAD_REQUEST,
        "remote_is_local",
    );

    // 모양은 맞지만 원격에 없는 토큰
    let mut unknown = body("dr", &r);
    unknown["token"] = json!("opl_0192f0c15d2e70008000000000000000_0123456789abcdef");
    assert_error(
        &admin.post("/remotes", unknown).await,
        StatusCode::BAD_REQUEST,
        "remote_unauthorized",
    );

    assert_eq!(admin.get("/remotes").await.1["items"], json!([]));
}

/// PUT은 통째로 바꾼다. token을 빼면 url·ca_fingerprint가 그대로일 때만 저장된 토큰을 둔다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_token_is_kept_only_while_the_url_and_ca_stay() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (status, created) = admin.post("/remotes", body("dr", &r)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let revision = created["revision"].as_u64().unwrap();

    // 같은 값(정규화한 모양만 다름)이면 쓰지 않는다.
    let same = json!({
        "url": format!("{}/", r.server.url),
        "ca_fingerprint": r.env.ca_fingerprint().to_uppercase(),
    });
    let (status, got) = admin.put("/remotes/dr", same, Some(revision)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["revision"], revision);
    assert_eq!(stored(&local, "dr").token, r.token);
    assert_eq!(hits(&r.server), 1);

    // 다른 주소(같은 원격의 다른 서버)로는 토큰을 다시 줘야 한다.
    let second = r.env.serve_tls().await;
    let moved = json!({ "url": second.url, "ca_fingerprint": r.env.ca_fingerprint() });
    let got = admin.put("/remotes/dr", moved.clone(), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("token must be given again"),
        "{}",
        got.1
    );
    let other = r.env.admin().await.token.unwrap();
    let mut with_token = moved.clone();
    with_token["token"] = json!(other);
    with_token["name"] = json!("dr");
    let (status, got) = admin.put("/remotes/dr", with_token, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["url"], second.url);
    assert_eq!(got["token_id"], token_id(&other).unwrap().to_string());
    assert_eq!(got["status"]["reachable"], true, "{got}");
    assert_eq!(stored(&local, "dr").token, other);
    // 주소가 바뀌면 CA를 다시 받아 확인한다.
    assert_eq!(hits(&second), 1);
    assert_eq!(stored(&local, "dr").ca_pem, r.env.ca_pem());

    // 토큰만 바뀌면 저장한 CA로 토큰만 확인한다.
    let mut token_only = moved.clone();
    token_only["token"] = json!(r.token);
    let (status, got) = admin.put("/remotes/dr", token_only, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["token_id"], token_id(&r.token).unwrap().to_string());
    assert_eq!(hits(&second), 1);

    // 바뀐 값은 다시 확인한다.
    let mut wrong = moved.clone();
    wrong["token"] = json!("opl_0192f0c15d2e70008000000000000000_0123456789abcdef");
    assert_error(
        &admin.put("/remotes/dr", wrong, None).await,
        StatusCode::BAD_REQUEST,
        "remote_unauthorized",
    );
    let mut renamed = moved.clone();
    renamed["name"] = json!("other");
    assert_error(
        &admin.put("/remotes/dr", renamed, None).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    let mut back = body("dr", &r);
    back.as_object_mut().unwrap().remove("name");
    assert_error(
        &admin.put("/remotes/dr", back, Some(revision)).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    assert_eq!(stored(&local, "dr").url, second.url);
}

/// 누구나 읽고 admin만 바꾼다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn everyone_reads_remotes_and_only_admins_change_them() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    assert_eq!(
        admin.post("/remotes", body("dr", &r)).await.0,
        StatusCode::CREATED
    );
    let mut update = body("dr", &r);
    update.as_object_mut().unwrap().remove("name");
    for role in ["operator", "viewer"] {
        let client = local.user(role, role).await;
        assert_eq!(client.get("/remotes").await.0, StatusCode::OK);
        let (status, got) = client.get("/remotes/dr").await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert!(got.get("token").is_none(), "{got}");
        for got in [
            client
                .post("/remotes", body(&format!("by-{role}"), &r))
                .await,
            client.put("/remotes/dr", update.clone(), None).await,
            client
                .post("/remotes/dr/actions/refresh", Value::Null)
                .await,
            client
                .post("/remotes/probe", json!({ "url": r.server.url }))
                .await,
            client.delete("/remotes/dr").await,
        ] {
            assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
        }
    }
    assert_error(
        &local.anonymous().get("/remotes").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    assert_eq!(
        admin.get("/remotes").await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// 옛 노드가 남아 있으면 만들지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remotes_wait_until_every_node_runs_this_build() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    local.cluster.create(&node).await.unwrap();
    assert_error(
        &admin.post("/remotes", body("dr", &r)).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    assert_eq!(admin.get("/remotes").await.1["items"], json!([]));
}

/// probe는 인증서를 검증하지 않고 CA를 받아 지문을 돌려준다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probes_show_the_remote_ca_fingerprint() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (status, got) = admin
        .post(
            "/remotes/probe",
            json!({ "url": format!("  {}/", r.server.url) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!({ "url": r.server.url, "ca_fingerprint": r.env.ca_fingerprint() })
    );
    assert_error(
        &admin
            .post("/remotes/probe", json!({ "url": "https://h/path" }))
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    let url = r.server.url.clone();
    drop(r);
    assert_error(
        &admin.post("/remotes/probe", json!({ "url": url })).await,
        StatusCode::BAD_GATEWAY,
        "remote_unreachable",
    );
}

/// `ca`가 `node-x`로 서명한 인증서로 `app`을 HTTPS 서비스한다. 주소와 서버 작업.
async fn serve_app(
    ca: &opl_peer::pki::ClusterCa,
    app: axum::Router,
) -> (String, tokio::task::JoinHandle<()>) {
    let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
    let tls = opl_peer::pki::NodeTls {
        cert_pem: ca
            .sign_node(&csr, uuid::Uuid::now_v7(), &["node-x".into()])
            .unwrap(),
        ca_pem: ca.cert_pem.clone(),
        key_pem,
    };
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", tcp.local_addr().unwrap());
    let config = opl_peer::tls::api_server_config(&tls).unwrap();
    let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, task)
}

/// `GET /cluster/ca`의 본문을 1초에 한 바이트씩 끝없이 보내는 HTTPS 서버(조각 사이 제한에는 걸리지 않음)
async fn trickling_server() -> (String, tokio::task::JoinHandle<()>) {
    let ca = opl_peer::pki::ClusterCa::generate("slow").unwrap();
    let app = axum::Router::new().route(
        "/api/v1/cluster/ca",
        axum::routing::get(|| async {
            let bytes = futures_util::stream::unfold((), |()| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Some((Ok::<_, std::convert::Infallible>(vec![b'-']), ()))
            });
            axum::body::Body::from_stream(bytes)
        }),
    );
    serve_app(&ca, app).await
}

/// 등록할 때의 확인과 probe는 모두 30초 안에 끝나야 한다(넘으면 502).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checking_a_slow_remote_times_out() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (url, server) = trickling_server().await;
    let mut req = body("slow", &r);
    req["url"] = json!(url);
    let timed = |path: &'static str, req: Value| {
        let admin = &admin;
        async move {
            let start = Instant::now();
            // 시간 제한이 없으면 끝나지 않는다.
            let got = tokio::time::timeout(Duration::from_secs(60), admin.post(path, req))
                .await
                .unwrap_or_else(|_| panic!("{path} did not time out"));
            (got, start.elapsed())
        }
    };
    let (created, probed) = tokio::join!(
        timed("/remotes", req),
        timed("/remotes/probe", json!({ "url": url })),
    );
    server.abort();
    for (got, elapsed) in [created, probed] {
        assert_error(&got, StatusCode::BAD_GATEWAY, "remote_unreachable");
        assert!(
            got.1["message"].as_str().unwrap().contains("timed out"),
            "{}",
            got.1
        );
        assert!(
            (Duration::from_secs(29)..Duration::from_secs(45)).contains(&elapsed),
            "{elapsed:?}"
        );
    }
    assert_eq!(admin.get("/remotes").await.1["items"], json!([]));
}

/// [`interjecting_server`]가 `GET /auth/whoami`를 받으면 이 클러스터의 원격 `dr`에 끼워 넣는 쓰기
enum Interject {
    /// 상태만 바꾼다(수집기처럼).
    Status,
    /// spec의 토큰을 바꾼다.
    Token(String),
}

/// `GET /cluster/ca`에 CA를 주고, `GET /auth/whoami`에 답하기 전에 `next`에 둔 쓰기를 `cluster`에 하는
/// HTTPS 서버(확인하는 사이에 다른 쓰기가 끼어든 것처럼). CA 지문, 주소와 서버 작업.
async fn interjecting_server(
    cluster: Arc<opl_cluster::Cluster>,
    next: Arc<Mutex<Option<Interject>>>,
) -> (String, String, tokio::task::JoinHandle<()>) {
    let ca = opl_peer::pki::ClusterCa::generate("dr").unwrap();
    let pem = ca.cert_pem.clone();
    let whoami = move || {
        let (cluster, next) = (cluster.clone(), next.clone());
        async move {
            let interject = next.lock().unwrap().take();
            if let Some(interject) = interject {
                let r = cluster.get_by_name::<RemoteSpec>("dr").unwrap().unwrap();
                match interject {
                    Interject::Status => {
                        let mut status = r.status.clone();
                        status.error = Some("interjected".into());
                        cluster
                            .update_status::<RemoteSpec>(r.id, &status)
                            .await
                            .unwrap();
                    }
                    Interject::Token(token) => {
                        let spec = RemoteSpec { token, ..r.spec };
                        cluster
                            .update_spec(r.id, None, r.labels, &spec)
                            .await
                            .unwrap();
                    }
                }
            }
            axum::Json(json!({}))
        }
    };
    let app = axum::Router::new()
        .route(
            "/api/v1/cluster/ca",
            axum::routing::get(move || {
                let pem = pem.clone();
                async move { pem }
            }),
        )
        .route("/api/v1/auth/whoami", axum::routing::get(whoami));
    let fingerprint = opl_peer::pki::fingerprint(&ca.cert_pem).unwrap();
    let (url, task) = serve_app(&ca, app).await;
    (fingerprint, url, task)
}

/// If-Match가 맞아 확인을 시작한 뒤 그 사이 상태만 바뀌었으면(수집기 따위) 새 revision에 써 200, spec이
/// 바뀌었으면 412.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_status_write_during_the_check_does_not_fail_if_match() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let next = Arc::new(Mutex::new(None));
    let (fingerprint, url, server) = interjecting_server(local.cluster.clone(), next.clone()).await;
    let token = |n: u8| format!("opl_0192f0c15d2e7000800000000000000{n}_secret");
    let (status, created) = admin
        .post(
            "/remotes",
            json!({ "name": "dr", "url": url, "ca_fingerprint": fingerprint, "token": token(1) }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let put = |n: u8| json!({ "url": url, "ca_fingerprint": fingerprint, "token": token(n) });

    let revision = admin.get("/remotes/dr").await.1["revision"]
        .as_u64()
        .unwrap();
    *next.lock().unwrap() = Some(Interject::Status);
    let (status, got) = admin.put("/remotes/dr", put(2), Some(revision)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(next.lock().unwrap().is_none(), "the check did not run");
    assert!(got["revision"].as_u64().unwrap() > revision + 1, "{got}");
    assert_eq!(stored(&local, "dr").token, token(2));

    let revision = admin.get("/remotes/dr").await.1["revision"]
        .as_u64()
        .unwrap();
    *next.lock().unwrap() = Some(Interject::Token(token(3)));
    assert_error(
        &admin.put("/remotes/dr", put(4), Some(revision)).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    assert!(next.lock().unwrap().is_none(), "the check did not run");
    assert_eq!(stored(&local, "dr").token, token(3));
    server.abort();
}

/// refresh는 곧 다시 모으고, 바뀌었을 때만 적는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_collects_now_and_writes_only_changes() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (_, created) = admin.post("/remotes", body("dr", &r)).await;
    let revision = created["revision"].as_u64().unwrap();

    let (status, same) = admin.post("/remotes/dr/actions/refresh", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same["revision"], revision, "{same}");

    let remote_admin = r.env.admin().await;
    let (status, vm) = remote_admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let (_, changed) = admin.post("/remotes/dr/actions/refresh", Value::Null).await;
    assert_eq!(changed["status"]["vms"]["total"], 1, "{changed}");
    assert!(changed["revision"].as_u64().unwrap() > revision);
    assert_eq!(
        admin.get("/remotes/dr").await.1["revision"],
        changed["revision"]
    );
}

/// 리더의 수집기는 원격 상태가 바뀔 때만 적고, 원격이 꺼지면 reachable false와 이유를 적는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_poller_records_changes_only_and_unreachable_remotes() {
    let local = Env::start().await;
    let admin = local.admin().await;
    let r = remote().await;
    let (status, created) = admin.post("/remotes", body("dr", &r)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let poller = RemotePoller::spawn(
        local.state.clone(),
        RemotePollerConfig {
            tick: Duration::from_millis(50),
            interval: Duration::from_millis(100),
        },
    );
    let get = || {
        local
            .cluster
            .get_by_name::<RemoteSpec>("dr")
            .unwrap()
            .unwrap()
    };

    // 원격이 바뀌면 적는다.
    let remote_admin = r.env.admin().await;
    let (status, vm) = remote_admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    until("the new VM", || {
        get().status.vms.is_some_and(|v| v.total == 1)
    })
    .await;

    // 그대로면 여러 번 모아도 쓰지 않는다.
    let revision = get().revision;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(get().revision, revision);
    assert!(get().status.reachable);

    let Remote { env, server, .. } = r;
    drop(server);
    until("an unreachable remote", || !get().status.reachable).await;
    let status = get().status;
    assert!(
        status.error.as_deref().is_some_and(|e| e.contains("reach")),
        "{status:?}"
    );
    assert_eq!(status.vms, None);
    poller.shutdown().await;
    drop(env);
}

async fn until(why: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
