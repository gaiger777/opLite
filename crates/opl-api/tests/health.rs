//! 헬스 체크(`GET /health`), 노드 역할(`roles`), API 노드 목록(`GET /cluster/api-endpoints`).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Client, Env, WAIT};
use opl_api::{AppState, RuntimeProbe};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{HaConfig, HaGuard};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 정한 실행기를 늦었다고 알린다.
struct Slow(Vec<&'static str>);

impl RuntimeProbe for Slow {
    fn slow(&self) -> Vec<&'static str> {
        self.0.clone()
    }
}

/// 켜고 끌 수 있는 가짜 리스 갱신
struct FakeRenewer(Mutex<bool>);

#[async_trait::async_trait]
impl LeaseRenewer for FakeRenewer {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), ClusterError> {
        if *self.0.lock().unwrap() {
            Ok(())
        } else {
            Err(ClusterError::Timeout)
        }
    }
}

/// 같은 클러스터를 `tweak`으로 바꾼 상태로 따로 서비스한다(관리자 토큰을 쓰는 클라이언트).
async fn serve(env: &Env, tweak: impl FnOnce(&mut AppState)) -> Client {
    let mut state = AppState::new(
        env.cluster.clone(),
        env.node,
        env.hv.clone(),
        env.dir.path().join("uploads"),
    );
    tweak(&mut state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(Arc::new(state));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Client {
        base,
        http: reqwest::Client::new(),
        token: env.admin().await.token,
    }
}

/// `node1`의 spec 필드 하나를 바꾼다.
async fn set_spec(admin: &Client, field: &str, value: Value) -> (StatusCode, Value) {
    let (_, node) = admin.get("/nodes/node1").await;
    let mut spec = node["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = node.get("labels").cloned().unwrap_or_else(|| json!({}));
    admin
        .put(
            "/nodes/node1",
            json!({ "labels": labels, "spec": spec }),
            None,
        )
        .await
}

fn failed(body: &Value) -> Vec<String> {
    body["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn health_is_ready_on_a_healthy_api_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for path in ["/health", "/health?ready", "/health?live"] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(body["status"], "ok", "{body}");
        assert_eq!(body["node"], env.node.to_string());
        assert_eq!(body["leader"], env.node.to_string());
        assert_eq!(body["quorum"], true);
        assert_eq!(body["apply_lag_ms"], 0);
        // 펜싱이 꺼진 노드
        assert_eq!(body["lease"], "none");
        assert_eq!(body["runtimes"], "ok");
        assert_eq!(body["maintenance"], false);
        assert_eq!(body["api"], true);
        assert_eq!(body["failed"], json!([]));
    }
}

/// 로그인하지 않은 요청에는 `status`·`failed`만 답한다(노드 id·리더 같은 내부 정보를 감춘다).
#[tokio::test]
async fn anonymous_health_is_brief() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let anon = env.anonymous();
    for path in ["/health", "/health?live"] {
        let (status, body) = anon.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(body, json!({ "status": "ok", "failed": [] }), "{path}");
    }
    let (status, body) = set_spec(&admin, "maintenance", json!(true)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = anon.get("/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        body,
        json!({ "status": "unavailable", "failed": ["maintenance"] })
    );
    // 틀린 토큰도 로그인하지 않은 것과 같다.
    let bad = Client {
        token: Some("opl_00000000000000000000000000000000_x".into()),
        ..env.anonymous()
    };
    let (status, body) = bad.get("/health?live").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "status": "ok", "failed": [] }));
}

/// 유지보수 중이거나 `api` 역할이 아니면 `?ready`(기본)는 503, `?live`는 200이다.
#[tokio::test]
async fn maintenance_and_roles_take_a_node_out_of_rotation() {
    let env = Env::start().await;
    let admin = env.admin().await;

    let (status, body) = set_spec(&admin, "maintenance", json!(true)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin.get("/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["status"], "unavailable");
    assert_eq!(failed(&body), ["maintenance"]);
    let (status, body) = admin.get("/health?live").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["maintenance"], true);
    assert_eq!(body["failed"], json!([]));
    set_spec(&admin, "maintenance", json!(false)).await;

    // 역할을 비우면 API 노드가 아니다. 겹친 역할은 하나로 적는다.
    let (status, body) = set_spec(&admin, "roles", json!([])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["roles"], json!([]));
    let (status, body) = admin.get("/health?ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(failed(&body), ["role"]);
    assert_eq!(body["api"], false);
    assert_eq!(admin.get("/health?live").await.0, StatusCode::OK);
    let (status, body) = set_spec(&admin, "roles", json!(["api", "api"])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["roles"], json!(["api"]));
    assert_eq!(admin.get("/health").await.0, StatusCode::OK);
    // 모르는 역할은 거절한다.
    let (status, body) = set_spec(&admin, "roles", json!(["observer"])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // null이면 기본 역할로 돌아가고 저장소에 쓰지 않는다.
    let (status, body) = set_spec(&admin, "roles", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("roles").is_none(), "{body}");
    assert_eq!(admin.get("/health").await.0, StatusCode::OK);
}

/// HA 펜싱 노드는 리스가 fresh여야 준비됐다.
#[tokio::test]
async fn a_fencing_node_needs_a_fresh_lease() {
    let env = Env::start().await;
    let renewer = Arc::new(FakeRenewer(Mutex::new(false)));
    let guard = HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after: Duration::from_millis(300),
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    );
    let client = serve(&env, |s| s.ha = Some(guard.clone())).await;
    let (status, body) = client.get("/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["lease"], "expired");
    assert_eq!(failed(&body), ["lease"]);
    assert_eq!(client.get("/health?live").await.0, StatusCode::OK);

    *renewer.0.lock().unwrap() = true;
    tokio::time::timeout(WAIT, guard.subscribe().wait_for(|s| s.fresh))
        .await
        .unwrap()
        .unwrap();
    let (status, body) = client.get("/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["lease"], "fresh");
    guard.close();
}

/// 마지막 실행기 확인에서 `core`나 `api`가 늦었으면 준비되지 않았다. `work`만 늦으면 괜찮다.
#[tokio::test]
async fn slow_core_or_api_runtimes_fail_readiness() {
    let env = Env::start().await;
    let work = serve(&env, |s| s.runtimes = Some(Arc::new(Slow(vec!["work"])))).await;
    let (status, body) = work.get("/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["runtimes"], "ok");
    for slow in ["core", "api"] {
        let client = serve(&env, |s| {
            s.runtimes = Some(Arc::new(Slow(vec![slow, "work"])))
        })
        .await;
        let (status, body) = client.get("/health").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{slow}: {body}");
        assert_eq!(body["runtimes"], "slow");
        assert_eq!(failed(&body), ["runtimes"]);
        assert_eq!(client.get("/health?live").await.0, StatusCode::OK);
    }
}

/// 다른 노드 하나를 Node 리소스로 더한다(Raft 멤버는 아니다).
async fn add_node(env: &Env, name: &str, address: &str, witness: bool, features: bool) -> Uuid {
    let mut node = Resource::new(
        name,
        NodeSpec {
            address: address.into(),
            maintenance: false,
            witness,
            roles: None,
        },
    );
    node.status = NodeStatus {
        online: true,
        features: if features {
            opl_types::node::FEATURES
                .iter()
                .map(|f| f.to_string())
                .collect()
        } else {
            vec![]
        },
        ..Default::default()
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

#[tokio::test]
async fn api_endpoints_list_api_role_nodes() {
    let env = Env::start().await;
    add_node(&env, "node0", "10.0.0.10:8006", false, true).await;
    add_node(&env, "wit", "10.0.0.20:8006", true, true).await;
    let anon = env.anonymous();
    assert_eq!(
        anon.get("/cluster/api-endpoints").await.0,
        StatusCode::UNAUTHORIZED
    );
    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = viewer.get("/cluster/api-endpoints").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 위트니스는 빠지고 이름 순이다. zone 레이블이 없으면 null이다.
    assert_eq!(
        body["items"],
        json!([
            { "name": "node0", "address": "10.0.0.10:8006", "zone": null },
            { "name": "node1", "address": "127.0.0.1:0", "zone": null },
        ])
    );

    // 노드 레이블 `zone`을 보이고, `api` 역할을 뺀 노드는 빠진다.
    let admin = env.admin().await;
    let (_, node) = admin.get("/nodes/node0").await;
    let mut spec = node["spec"].clone();
    spec["roles"] = json!([]);
    let (status, body) = admin
        .put("/nodes/node0", json!({ "labels": {}, "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, node) = admin.get("/nodes/node1").await;
    let (status, body) = admin
        .put(
            "/nodes/node1",
            json!({ "labels": { "zone": "rack-a" }, "spec": node["spec"] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = viewer.get("/cluster/api-endpoints").await;
    assert_eq!(
        body["items"],
        json!([{ "name": "node1", "address": "127.0.0.1:0", "zone": "rack-a" }])
    );
}

#[tokio::test]
async fn roles_are_checked_and_need_every_node_to_know_them() {
    let env = Env::start().await;
    add_node(&env, "wit", "10.0.0.20:8006", true, true).await;
    let admin = env.admin().await;
    // 위트니스는 API가 없다.
    let (_, node) = admin.get("/nodes/wit").await;
    let mut spec = node["spec"].clone();
    spec["roles"] = json!(["api"]);
    let (status, body) = admin
        .put("/nodes/wit", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    spec["roles"] = json!([]);
    let (status, body) = admin.put("/nodes/wit", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // `node-roles`를 모르는 노드가 있으면 역할을 적지 않는다(기본 역할로 바꾸는 것은 된다).
    add_node(&env, "old", "10.0.0.30:8006", false, false).await;
    let (status, body) = set_spec(&admin, "roles", json!(["api"])).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    let (status, body) = set_spec(&admin, "maintenance", json!(true)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 역할을 바꾸는 것은 관리자·운영자만(노드 쓰기)이다.
    let viewer = env.user("viewer", "viewer").await;
    assert_eq!(
        set_spec(&viewer, "roles", Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
}

/// 관찰 노드를 더한다. 실제 길처럼 learner로 가입해 따라잡은 뒤 클러스터 정보의 관찰 노드 목록에 적히고 멤버십에서
/// 빠진다. 같은 저장소로 관찰 노드로 다시 열려 투표 멤버(`env`의 노드)를 따르고, Node 리소스(`api` 역할)가 있다.
async fn start_observer(env: &Env) -> Arc<Cluster> {
    let id = NodeId(Uuid::now_v7());
    let addr = format!("observer-{id}");
    let mut config = ClusterConfig::new(id, env.dir.path().join("observer"));
    config.raft = openraft::Config {
        heartbeat_interval: 50,
        election_timeout_min: 300,
        election_timeout_max: 600,
        ..Default::default()
    };
    let learner = Cluster::open(config.clone(), env.router.network(id))
        .await
        .unwrap();
    env.router.register(id, &learner);
    env.cluster
        .add_learner(id, OrbNode::single(&addr))
        .await
        .unwrap();
    let mut info = env.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
    info.status.observers.insert(id.0, addr);
    env.cluster
        .update_status::<ClusterSpec>(info.id, &info.status)
        .await
        .unwrap();
    // 리더의 키퍼처럼 목록에 적은 뒤 `RemoveNodes`로 뺀다(`LeaderRequest::Remove`는 노드를 지우는 것이라 목록에서도
    // 뺀다).
    env.cluster
        .raft()
        .unwrap()
        .change_membership(openraft::ChangeMembers::RemoveNodes([id].into()), false)
        .await
        .unwrap();
    env.router.unregister(id);
    learner.shutdown().await.unwrap();
    drop(learner);
    let deadline = std::time::Instant::now() + WAIT;
    let observer = loop {
        match Cluster::open_observer(
            config.clone(),
            env.router.network(id),
            env.router.follower(id),
        )
        .await
        {
            Err(ClusterError::Store(_)) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => break other.unwrap(),
        }
    };
    let observer = Arc::new(observer.with_forwarder(env.router.forwarder(id)));
    env.router.register(id, &observer);
    let mut node = Resource::new(
        "observer",
        NodeSpec {
            address: "127.0.0.1:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.id = id.0;
    let revision = env.cluster.create(&node).await.unwrap();
    observer.wait_revision(revision, WAIT).await.unwrap();
    observer
}

/// `cluster`(다른 노드)로 API를 따로 서비스한다(관리자 토큰을 쓰는 클라이언트).
async fn serve_node(env: &Env, cluster: Arc<Cluster>) -> Client {
    let node = cluster.id().0;
    let state = AppState::new(
        cluster,
        node,
        env.hv.clone(),
        env.dir.path().join(format!("uploads-{node}")),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(Arc::new(state));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Client {
        base,
        http: reqwest::Client::new(),
        token: env.admin().await.token,
    }
}

/// 관찰 노드는 붙은 투표 멤버의 진행이 5초 안에 오고 리더를 알면 준비됐고(200), 끊기면 `quorum`으로 503이다.
/// 관찰 노드도 클러스터 멤버로(`role: observer`) 보인다.
#[tokio::test]
async fn an_observer_is_ready_while_it_follows_a_voter() {
    let env = Env::start().await;
    let observer = start_observer(&env).await;
    let id = observer.id();
    let client = serve_node(&env, observer.clone()).await;
    // 관리자 토큰이 관찰 노드에 복제될 때까지
    observer
        .wait_revision(env.cluster.revision(), WAIT)
        .await
        .unwrap();
    let (status, body) = client.get("/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["node"], id.0.to_string());
    assert_eq!(body["leader"], env.node.to_string());
    assert_eq!(body["quorum"], true);

    for c in [&client, &env.admin().await] {
        let (status, view) = c.get("/cluster").await;
        assert_eq!(status, StatusCode::OK, "{view}");
        let me = view["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id.0.to_string())
            .unwrap_or_else(|| panic!("the observer is not listed: {view}"))
            .clone();
        assert_eq!(me["role"], "observer", "{view}");
        assert_eq!(me["voter"], false);
        assert_eq!(me["name"], "observer");
        assert_eq!(me["cluster_address"], format!("observer-{id}"));
        assert_eq!(me["cluster_addresses"], json!([format!("observer-{id}")]));
        assert_eq!(view["members"][0]["role"], "voter", "{view}");
    }

    env.router.isolate(id);
    let anon = Client {
        token: None,
        ..client
    };
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let (status, body) = anon.get("/health").await;
        if status == StatusCode::SERVICE_UNAVAILABLE {
            assert_eq!(failed(&body), ["quorum"], "{body}");
            break;
        }
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(std::time::Instant::now() < deadline, "still ready: {body}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    observer.shutdown().await.unwrap();
}

/// `Orbivisor-Min-Revision`은 다른 노드에서 만든 자격 증명에도 쓰인다: 이 노드가 아직 적용하지 않은 토큰도 그
/// 리비전을 주면 적용한 뒤 다시 인증해 받는다(관찰 노드는 리더가 돌려준 인덱스로 그 리비전이 있음을 안다). 기다리는
/// 사이 지운 토큰은 받지 않는다. 이 노드가 있다고 모르는 리비전이면 인증되지 않은 요청은 기다리지 않는다.
#[tokio::test]
async fn min_revision_covers_credentials_made_on_another_node() {
    let env = Env::start().await;
    let observer = start_observer(&env).await;
    let client = serve_node(&env, observer.clone()).await;
    let doomed = env.admin().await.token.unwrap();
    observer
        .wait_revision(env.cluster.revision(), WAIT)
        .await
        .unwrap();
    let get = |token: &str, min: Option<u64>| {
        let mut r = client
            .http
            .get(format!("{}/api/v1/volumes", client.base))
            .bearer_auth(token);
        if let Some(min) = min {
            r = r.header("Orbivisor-Min-Revision", min.to_string());
        }
        r.send()
    };
    assert_eq!(get(&doomed, None).await.unwrap().status(), StatusCode::OK);

    // 관찰 노드가 프레임을 받지 못하는 동안 리더에서 토큰 하나를 만들고 하나를 지운다.
    env.router.pause_follow(observer.id());
    let fresh = env.admin().await;
    let doomed_id = doomed
        .strip_prefix("opl_")
        .and_then(|t| t.split('_').next())
        .unwrap();
    let (status, body) = fresh.delete(&format!("/tokens/{doomed_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let fresh = fresh.token.unwrap();
    let revision = env.cluster.revision();
    assert_eq!(
        get(&fresh, None).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(get(&doomed, None).await.unwrap().status(), StatusCode::OK);
    // 관찰 노드가 그 리비전을 모르면 인증되지 않은 요청은 기다리지 않는다.
    let started = std::time::Instant::now();
    let resp = get(&fresh, Some(revision)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(started.elapsed() < Duration::from_secs(2));

    // 리스 갱신이 리더의 인덱스를 알려 관찰 노드가 그 리비전이 있음을 안다.
    let renewal = {
        let observer = observer.clone();
        tokio::spawn(async move { observer.renew_lease(WAIT).await })
    };
    let deadline = std::time::Instant::now() + WAIT;
    while observer.known_revision() < revision {
        assert!(
            std::time::Instant::now() < deadline,
            "the revision stayed unknown"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(observer.revision() < revision);
    let resume = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        env.router.heal(observer.id());
    };
    let (made, deleted, ()) = tokio::join!(
        get(&fresh, Some(revision)),
        get(&doomed, Some(revision)),
        resume
    );
    assert_eq!(made.unwrap().status(), StatusCode::OK);
    assert_eq!(deleted.unwrap().status(), StatusCode::UNAUTHORIZED);
    renewal.await.unwrap().unwrap();
    observer.shutdown().await.unwrap();
}
