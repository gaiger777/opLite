//! 지표 읽기 API(한 노드): `GET /metrics/nodes`·`/nodes/{key}/metrics`·`/vms/{key}/metrics`의 응답,
//! `since`, 지표 소스가 없는 노드(`unsupported`·409), 위트니스·멤버가 아닌 노드, 권한과 404·400.
//! 다른 노드에서 읽기는 `two_nodes.rs`가 본다.

mod common;

use std::sync::Arc;

use common::{ADMIN_PASSWORD, Env, login_at, vm_request};
use opl_api::{Action, AppState, Authorizer, Principal};
use opl_peer::{MetricsReply, MetricsSource};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample, VmSeries};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::{Kind, Resource};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

/// 점은 `t` 1000·2000. `Vm`은 어떤 VM이든 그 VM의 점으로 돌려준다.
struct FakeMetrics {
    node: Uuid,
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        let times = || {
            [1_000, 2_000]
                .into_iter()
                .filter(|t| q.since.is_none_or(|since| *t > since))
        };
        match q.scope {
            MetricsScope::Nodes => MetricsReply {
                node: Some(NodeSeries {
                    node: self.node,
                    samples: times().map(node_sample).collect(),
                }),
                vms: vec![],
            },
            MetricsScope::Vm { id } => MetricsReply {
                node: None,
                vms: vec![VmSeries {
                    vm: id,
                    node: self.node,
                    samples: times().map(vm_sample).collect(),
                }],
            },
        }
    }
}

fn node_sample(t: i64) -> NodeSample {
    NodeSample {
        t,
        cpu: Some(25.0),
        mem_used_mib: Some(1024),
        mem_total_mib: Some(8192),
        disk_read_bps: None,
        disk_write_bps: None,
        net_rx_bps: Some(10.0),
        net_tx_bps: Some(20.0),
        fs_used_mib: Some(1),
        fs_total_mib: Some(2),
    }
}

fn vm_sample(t: i64) -> VmSample {
    VmSample {
        t,
        cpu: Some(50.0),
        mem_rss_mib: Some(300),
        disk_read_bps: None,
        disk_write_bps: None,
        disk_read_iops: None,
        disk_write_iops: None,
        net_rx_bps: None,
        net_tx_bps: None,
    }
}

/// 모든 것을 거부하는 권한 검사
struct DenyAll;

impl Authorizer for DenyAll {
    fn check(&self, _: &Principal, _: Action, _: Kind) -> bool {
        false
    }
}

/// 같은 노드의 API를 `tweak`한 상태로 하나 더 띄우고 그 주소를 돌려준다.
async fn serve(env: &Env, tweak: impl FnOnce(&mut AppState)) -> String {
    let hv: Arc<dyn opl_driver::Hypervisor> = env.hv.clone();
    let mut state = AppState::new(
        env.cluster.clone(),
        env.node,
        hv,
        env.dir.path().join("uploads2"),
    );
    tweak(&mut state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(Arc::new(state));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

async fn with_metrics(env: &Env) -> String {
    let source = Arc::new(FakeMetrics { node: env.node });
    serve(env, |s| s.metrics = Some(source)).await
}

/// 이름이 `name`인 Node 리소스를 클러스터 멤버로 두지 않고 만든다.
async fn add_node(env: &Env, name: &str, witness: bool) {
    let r: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness,
        },
    );
    env.cluster.create(&r).await.unwrap();
}

fn times(samples: &serde_json::Value) -> Vec<i64> {
    samples
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["t"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn a_viewer_reads_node_metrics() {
    let env = Env::start().await;
    let base = with_metrics(&env).await;
    env.user("vera", "viewer").await;
    let viewer = login_at(&base, "vera", "password-1").await;

    let (status, body) = viewer.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["unreachable"], json!([]));
    assert_eq!(body["unsupported"], json!([]));
    let nodes = body["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1, "{body:#}");
    assert_eq!(nodes[0]["node"], json!(env.node));
    assert_eq!(times(&nodes[0]["samples"]), [1_000, 2_000]);
    assert_eq!(nodes[0]["samples"][0]["cpu"], 25.0);
    assert_eq!(
        nodes[0]["samples"][0]["disk_read_bps"],
        serde_json::Value::Null
    );
    let (_, body) = viewer.get("/metrics/nodes?since=1000").await;
    assert_eq!(times(&body["nodes"][0]["samples"]), [2_000]);

    // 노드 하나: 이름이나 id로
    let (status, body) = viewer.get("/nodes/node1/metrics?since=1000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["node"], json!(env.node));
    assert_eq!(times(&body["samples"]), [2_000]);
    let (status, body) = viewer.get(&format!("/nodes/{}/metrics", env.node)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(times(&body["samples"]), [1_000, 2_000]);
}

#[tokio::test]
async fn vm_metrics_are_read_by_vm_key() {
    let env = Env::start().await;
    let base = with_metrics(&env).await;
    let admin = login_at(&base, "admin", ADMIN_PASSWORD).await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");

    let (status, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["vm"], vm["id"]);
    assert_eq!(times(&body["samples"]), [1_000, 2_000]);
    assert_eq!(body["samples"][0]["mem_rss_mib"], 300);
    assert_eq!(body["unreachable"], json!([]));
    assert_eq!(body["unsupported"], json!([]));
    let id = vm["id"].as_str().unwrap();
    let (status, body) = admin.get(&format!("/vms/{id}/metrics?since=1000")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(times(&body["samples"]), [2_000]);
}

#[tokio::test]
async fn a_node_without_metrics_is_unsupported() {
    // 기본 Env의 API에는 지표 소스가 없다(옛 노드처럼).
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["nodes"], json!([]));
    assert_eq!(body["unsupported"], json!(["node1"]));
    assert_eq!(body["unreachable"], json!([]));

    let (status, body) = admin.get("/nodes/node1/metrics").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert_eq!(
        body["message"],
        "node \"node1\" does not report metrics yet"
    );

    admin.post("/vms", vm_request("web")).await;
    let (status, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["samples"], json!([]));
    assert_eq!(body["unsupported"], json!(["node1"]));
}

#[tokio::test]
async fn witness_and_non_member_nodes_are_not_read() {
    let env = Env::start().await;
    let base = with_metrics(&env).await;
    let admin = login_at(&base, "admin", ADMIN_PASSWORD).await;
    add_node(&env, "w1", true).await;
    add_node(&env, "ghost", false).await;

    let (status, body) = admin.get("/nodes/w1/metrics").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert_eq!(
        body["message"],
        "node \"w1\" is a witness node and runs no workloads"
    );
    let (status, body) = admin.get("/nodes/ghost/metrics").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "node_unreachable");
    assert_eq!(
        body["message"],
        "node \"ghost\" did not answer: not a cluster member"
    );
    // 목록은 클러스터 멤버만 묻는다.
    let (_, body) = admin.get("/metrics/nodes").await;
    assert_eq!(body["nodes"].as_array().unwrap().len(), 1, "{body:#}");
    assert_eq!(body["unreachable"], json!([]));
}

#[tokio::test]
async fn bad_requests_and_missing_resources_are_rejected() {
    let env = Env::start().await;
    let base = with_metrics(&env).await;
    let admin = login_at(&base, "admin", ADMIN_PASSWORD).await;
    for path in [
        "/metrics/nodes?since=yesterday",
        "/nodes/node1/metrics?since=1.5",
        "/vms/web/metrics?since=x",
        "/metrics/nodes?until=5",
    ] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(body["code"], "invalid_request", "{path}");
    }
    for path in ["/nodes/nope/metrics", "/vms/nope/metrics"] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        assert_eq!(body["code"], "not_found", "{path}");
    }

    // 로그인하지 않으면 401, 권한이 없으면 403
    let (status, _) = env.anonymous().get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let denied = serve(&env, |s| s.authorizer = Arc::new(DenyAll)).await;
    let admin = login_at(&denied, "admin", ADMIN_PASSWORD).await;
    for path in ["/metrics/nodes", "/nodes/node1/metrics", "/vms/web/metrics"] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
    }
}
