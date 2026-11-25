//! 지표 기록 API(한 노드, 계획 60): `GET /nodes/{key}/metrics/history`·`/vms/{key}/metrics/history`·
//! `/storage-pools/{key}/metrics/history`·`/metrics/cluster`의 조회 만들기(범위·점 수), 주인 노드 고르기, 오류
//! (400·404·409·503), 클러스터 모으기의 합과 10초 캐시, 권한. 다른 노드에 묻기는 `two_nodes.rs`가 본다.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use common::{Env, mark_pool_ready, vm_request};
use orbivisor_api::{Action, Authorizer, Principal};
use orbivisor_peer::{HistoryError, MetricsReply, MetricsSource};
use orbivisor_types::metrics::{
    Cf, HistoryQuery, HistoryReply, HistoryScope, MetricsQuery, Tier, TopVm,
};
use orbivisor_types::node::{METRICS_HISTORY, NodeSpec, NodeStatus};
use orbivisor_types::resource::{Kind, Resource};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

/// 받은 조회를 적고, 범위 시작을 `start`로 돌려준다. `Digest`면 상위 VM `top`을 붙인다.
#[derive(Default)]
struct FakeHistory {
    asked: Mutex<Vec<HistoryQuery>>,
    top: Mutex<Vec<TopVm>>,
    error: Mutex<Option<HistoryError>>,
}

impl FakeHistory {
    fn asked(&self) -> Vec<HistoryQuery> {
        self.asked.lock().unwrap().clone()
    }
}

impl MetricsSource for FakeHistory {
    fn read(&self, _: &MetricsQuery) -> MetricsReply {
        MetricsReply {
            node: None,
            vms: vec![],
        }
    }

    fn history(&self, q: &HistoryQuery) -> Result<HistoryReply, HistoryError> {
        self.asked.lock().unwrap().push(q.clone());
        if let Some(e) = self.error.lock().unwrap().clone() {
            return Err(e);
        }
        let digest = matches!(q.scope, HistoryScope::Digest { .. });
        let cf = |v: Vec<Option<f32>>| Cf {
            max: if digest { vec![] } else { v.clone() },
            min: if digest { vec![] } else { v.clone() },
            avg: v,
        };
        Ok(HistoryReply {
            tier: Tier::Raw5s,
            step_ms: 5_000,
            start: q.from - q.from.rem_euclid(5_000),
            columns: BTreeMap::from([
                ("cpu".to_string(), cf(vec![Some(20.0), None])),
                ("mem_used".to_string(), cf(vec![Some(512.0), Some(1024.0)])),
            ]),
            stats: BTreeMap::new(),
            pct: BTreeMap::new(),
            covered_from: Some(q.from + 5_000),
            top: if digest {
                self.top.lock().unwrap().clone()
            } else {
                vec![]
            },
        })
    }
}

async fn start() -> (Env, Arc<FakeHistory>) {
    let source = Arc::new(FakeHistory::default());
    let s = source.clone();
    let env = Env::start_with_state(move |state| state.metrics = Some(s)).await;
    (env, source)
}

/// 이름이 `name`인 Node 리소스를 클러스터 멤버로 두지 않고 만든다.
async fn add_node(env: &Env, name: &str, witness: bool, status: NodeStatus) -> Uuid {
    let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            sdn: None,
            host_resources: None,
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness,
            roles: None,
        },
    );
    r.status = status;
    env.cluster.create(&r).await.unwrap();
    r.id
}

fn upgraded(online: bool) -> NodeStatus {
    NodeStatus {
        online,
        cpus: 2,
        features: vec![METRICS_HISTORY.to_string()],
        ..NodeStatus::default()
    }
}

#[tokio::test]
async fn node_history_is_read_with_a_range_or_a_window() {
    let (env, source) = start().await;
    let viewer = env.user("vera", "viewer").await;

    let (status, body) = viewer.get("/nodes/node1/metrics/history").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tier"], "raw5s");
    assert_eq!(body["step_ms"], 5_000);
    assert_eq!(body["columns"]["cpu"]["avg"], json!([20.0, null]));
    assert_eq!(body["columns"]["cpu"]["max"], json!([20.0, null]));
    let q = source.asked().pop().unwrap();
    assert_eq!(q.scope, HistoryScope::Node);
    // 기본: 지난 1시간, 300점
    assert_eq!((q.until - q.from, q.points), (HOUR, 300));
    assert_eq!(body["covered_from"], json!(q.from + 5_000));

    let (status, _) = viewer
        .get(&format!(
            "/nodes/{}/metrics/history?range=week&points=50",
            env.node
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let q = source.asked().pop().unwrap();
    assert_eq!((q.until - q.from, q.points), (7 * DAY, 50));

    let (status, _) = viewer
        .get("/nodes/node1/metrics/history?from=1000&until=2000")
        .await;
    assert_eq!(status, StatusCode::OK);
    let q = source.asked().pop().unwrap();
    assert_eq!((q.from, q.until), (1_000, 2_000));

    for bad in [
        "range=day&from=1",
        "until=5",
        "from=10&until=10",
        "from=0&until=40000000000",
        "points=0",
        "points=1001",
        "range=decade",
        "since=5",
    ] {
        let (status, body) = viewer
            .get(&format!("/nodes/node1/metrics/history?{bad}"))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert_eq!(body["code"], "invalid_request", "{bad}");
    }
    let (status, _) = viewer.get("/nodes/nope/metrics/history").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn node_history_says_why_a_node_cannot_answer() {
    let (env, source) = start().await;
    let admin = env.admin().await;

    add_node(&env, "w1", true, upgraded(true)).await;
    let (status, body) = admin.get("/nodes/w1/metrics/history").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");

    // 기능을 알리지 않은 옛 노드는 묻지 않고 409
    add_node(&env, "old", false, NodeStatus::default()).await;
    let (status, body) = admin.get("/nodes/old/metrics/history").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert_eq!(
        body["message"],
        "node \"old\" does not keep metrics history yet; upgrade it first"
    );

    add_node(&env, "off", false, upgraded(false)).await;
    let (status, body) = admin.get("/nodes/off/metrics/history").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "node_unreachable");
    assert_eq!(
        body["message"],
        "node \"off\" did not answer: the node is offline"
    );

    add_node(&env, "stranger", false, upgraded(true)).await;
    let (status, body) = admin.get("/nodes/stranger/metrics/history").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        body["message"],
        "node \"stranger\" did not answer: no node-to-node connection"
    );

    // 이 노드의 소스가 기록을 모르면(옛 opld) 409, 파일 오류는 500(문구를 드러내지 않음)
    *source.error.lock().unwrap() = Some(HistoryError::Unsupported);
    let (status, body) = admin.get("/nodes/node1/metrics/history").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    *source.error.lock().unwrap() = Some(HistoryError::Io("/var/lib/opl: EIO".into()));
    let (status, body) = admin.get("/nodes/node1/metrics/history").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(!body.to_string().contains("EIO"), "{body}");
}

#[tokio::test]
async fn a_node_without_a_metrics_source_is_upgrading() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin.get("/nodes/node1/metrics/history").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    let (status, body) = admin.get("/metrics/cluster").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["unsupported"], json!(["node1"]));
    assert_eq!(body["node_count"], 0);
}

#[tokio::test]
async fn vm_history_asks_the_owner_node() {
    let (env, source) = start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let id: Uuid = serde_json::from_value(vm["id"].clone()).unwrap();

    // 아직 놓이지 않은 VM은 이 노드에 묻는다.
    let (status, body) = admin.get("/vms/web/metrics/history?range=day").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let q = source.asked().pop().unwrap();
    assert_eq!(q.scope, HistoryScope::Vm { id });
    assert_eq!(q.until - q.from, DAY);

    // 주인이 다른 노드면 그 노드에만 묻는다(여기서는 노드 사이 연결이 없어 503).
    let other = add_node(&env, "node2", false, upgraded(true)).await;
    let mut r = env
        .cluster
        .get::<orbivisor_types::vm::VmSpec>(id)
        .unwrap()
        .unwrap();
    r.status.node = Some(other);
    env.cluster
        .update_status::<orbivisor_types::vm::VmSpec>(id, &r.status)
        .await
        .unwrap();
    let asked = source.asked().len();
    let (status, body) = admin.get(&format!("/vms/{id}/metrics/history")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        body["message"],
        "node \"node2\" did not answer: no node-to-node connection"
    );
    assert_eq!(source.asked().len(), asked);
}

#[tokio::test]
async fn pool_history_asks_a_node_where_the_pool_is_ready() {
    let (env, source) = start().await;
    let admin = env.admin().await;
    let (status, pool) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": "/srv/pool" } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{pool}");
    let id: Uuid = serde_json::from_value(pool["id"].clone()).unwrap();

    let (status, body) = admin.get("/storage-pools/local/metrics/history").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_unavailable");

    mark_pool_ready(&env.cluster, env.node, "local").await;
    let (status, body) = admin
        .get("/storage-pools/local/metrics/history?range=month")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let q = source.asked().pop().unwrap();
    assert_eq!(q.scope, HistoryScope::Pool { id });
    assert_eq!(q.until - q.from, 30 * DAY);
}

#[tokio::test]
async fn the_cluster_digest_sums_nodes_and_is_cached_for_ten_seconds() {
    let (env, source) = start().await;
    let viewer = env.user("vera", "viewer").await;
    let admin = env.admin().await;
    let (_, vm) = admin.post("/vms", vm_request("web")).await;
    let id: Uuid = serde_json::from_value(vm["id"].clone()).unwrap();
    *source.top.lock().unwrap() = vec![
        TopVm {
            vm: id,
            cpu: Some(42.0),
            mem_rss: Some(300.0),
        },
        // 지워진 VM은 빠진다.
        TopVm {
            vm: Uuid::now_v7(),
            cpu: Some(99.0),
            mem_rss: None,
        },
    ];
    add_node(&env, "old", false, NodeStatus::default()).await;
    add_node(&env, "off", false, upgraded(false)).await;
    add_node(&env, "w1", true, upgraded(true)).await;

    let (status, body) = viewer.get("/metrics/cluster?range=day").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let asked = source.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].scope, HistoryScope::Digest { top: 10 });
    assert_eq!(
        (asked[0].until - asked[0].from, asked[0].points),
        (DAY, 120)
    );
    assert_eq!(body["tier"], "raw5s");
    assert_eq!(body["columns"]["cpu"], json!({ "avg": [20.0, null] }));
    assert_eq!(body["node_count"], 1);
    assert_eq!(body["nodes"][0]["name"], "node1");
    assert_eq!(body["nodes"][0]["cpus"], 4);
    assert_eq!(body["nodes"][0]["avg"]["cpu"], 20.0);
    assert_eq!(body["nodes"][0]["avg"]["mem_used"], 768.0);
    assert_eq!(body["nodes"][0]["cpu"], json!([20.0, null]));
    assert_eq!(
        body["top"],
        json!([{ "vm": id, "name": "web", "cpu": 42.0, "mem_rss": 300.0 }])
    );
    assert_eq!(body["unsupported"], json!(["old"]));
    assert_eq!(body["unreachable"], json!(["off"]));
    assert!(body.get("skewed").is_none(), "{body}");
    assert_eq!(body["pools"], json!([]));

    // 같은 조건은 10초 동안 다시 묻지 않고, 다른 조건은 묻는다.
    let (_, again) = viewer.get("/metrics/cluster?range=day").await;
    assert_eq!(again, body);
    assert_eq!(source.asked().len(), 1);
    let (status, _) = viewer.get("/metrics/cluster?range=day&points=60").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(source.asked().len(), 2);
    let (status, body) = viewer.get("/metrics/cluster?points=2000").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// 모든 것을 거부하는 권한 검사
struct DenyAll;

impl Authorizer for DenyAll {
    fn check(&self, _: &Principal, _: Action, _: Kind) -> bool {
        false
    }
}

#[tokio::test]
async fn history_needs_read_permission() {
    let source = Arc::new(FakeHistory::default());
    let s = source.clone();
    let env = Env::start_with_state(move |state| {
        state.metrics = Some(s);
        state.authorizer = Arc::new(DenyAll);
    })
    .await;
    let admin = env.admin().await;
    // 플러그인은 루트 확인만 정한다(VM·풀 기록은 폴더·grant 색인을 따른다, 계획 62).
    for path in ["/nodes/node1/metrics/history", "/metrics/cluster"] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
    }
    assert!(source.asked().is_empty());
    let (status, _) = env.anonymous().get("/metrics/cluster").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
