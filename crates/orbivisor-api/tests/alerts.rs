//! 경보 규칙·알림 대상 API와 경보 엔진(계획 60 §8·§9). 엔진은 손으로 맞추는 시계(`ManualClock`)와 짧은 tick으로 돌리고,
//! 알림은 127.0.0.1의 시험 HTTP 수신기(webhook)와 시험 SMTP 서버로 받는다.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode as AxumStatus, Uri};
use common::{Env, WAIT};
use orbivisor_api::{AlertBook, AlertEngine, AlertEngineConfig, AppState, SenderConfig};
use orbivisor_driver::Hypervisor;
use orbivisor_peer::{MetricsReply, MetricsSource};
use orbivisor_types::alert::RESOLVE_AFTER_MS;
use orbivisor_types::clock::{Clock, ManualClock};
use orbivisor_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries};
use orbivisor_types::node::{NOTIFICATIONS, NodeSpec, NodeStatus};
use orbivisor_types::resource::Resource;
use orbivisor_types::task::{TaskOp, TaskSpec, TaskState, TaskStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

const T0: i64 = 1_700_000_000_000;

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{:?}", got.1);
    assert_eq!(got.1["code"], code, "{:?}", got.1);
}

// ---- 시험 webhook 수신기 ----

#[derive(Debug, Clone)]
struct Hit {
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Hit {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

#[derive(Clone)]
struct Hook {
    base: String,
    hits: Arc<Mutex<Vec<Hit>>>,
    /// 이만큼은 500으로 답한다.
    fail: Arc<AtomicUsize>,
}

/// 0보다 크면 하나 줄이고 true다(`fetch_update`는 새 툴체인에서 deprecated이고 `try_update`는 옛 툴체인에 없다).
fn take_one(n: &AtomicUsize) -> bool {
    let mut cur = n.load(Ordering::SeqCst);
    while cur > 0 {
        match n.compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(now) => cur = now,
        }
    }
    false
}

impl Hook {
    async fn start() -> Self {
        let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
        let fail: Arc<AtomicUsize> = Arc::default();
        let (h, f) = (hits.clone(), fail.clone());
        let app = axum::Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
            let (h, f) = (h.clone(), f.clone());
            async move {
                if take_one(&f) {
                    return AxumStatus::INTERNAL_SERVER_ERROR;
                }
                h.lock().unwrap().push(Hit {
                    path: uri.path().to_string(),
                    headers: headers
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                        .collect(),
                    body: body.to_vec(),
                });
                AxumStatus::OK
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { base, hits, fail }
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    async fn wait(&self, n: usize) -> Vec<Hit> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let hits = self.hits();
            if hits.len() >= n {
                return hits;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "expected {n} notifications, got {hits:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

// ---- 가짜 지표 ----

#[derive(Default)]
struct FakeMetrics {
    node: Mutex<Option<NodeSample>>,
}

impl FakeMetrics {
    fn cpu(&self, t: i64, cpu: f32) {
        let mut values = [None; 9];
        values[0] = Some(cpu);
        *self.node.lock().unwrap() = Some(NodeSample::from_values(t, &values));
    }
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        let node = match q.scope {
            MetricsScope::Nodes => {
                let s = self.node.lock().unwrap().clone();
                Some(NodeSeries {
                    node: Uuid::nil(),
                    samples: s
                        .into_iter()
                        .filter(|s| q.since.is_none_or(|since| s.t > since))
                        .collect(),
                })
            }
            _ => None,
        };
        MetricsReply { node, vms: vec![] }
    }
}

// ---- 엔진 ----

struct Rig {
    env: Env,
    hook: Hook,
    clock: Arc<ManualClock>,
    metrics: Arc<FakeMetrics>,
    /// 장부 디렉터리
    dir: TempDir,
    book: AlertBook,
}

impl Rig {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let book = AlertBook::open(dir.path().join("alerts"), "node1").unwrap();
        let metrics = Arc::new(FakeMetrics::default());
        let (b, m) = (book.clone(), metrics.clone());
        let env = Env::start_with_state(move |s| {
            s.alerts = Some(b);
            s.metrics = Some(m);
        })
        .await;
        Self {
            env,
            hook: Hook::start().await,
            clock: Arc::new(ManualClock::new(T0)),
            metrics,
            dir,
            book,
        }
    }

    /// `book`을 장부로 쓰는 엔진(API 서버와 따로 둔 상태)
    fn engine_with(&self, book: AlertBook, sender: SenderConfig) -> AlertEngine {
        let hv: Arc<dyn Hypervisor> = self.env.hv.clone();
        let mut state = AppState::new(
            self.env.cluster.clone(),
            self.env.node,
            hv,
            self.env.dir.path().join("uploads"),
        );
        state.alerts = Some(book);
        state.metrics = Some(self.metrics.clone());
        let config = AlertEngineConfig {
            tick: Duration::from_millis(20),
            resolve_after_ms: RESOLVE_AFTER_MS,
            clock: self.clock.clone(),
            sender,
        };
        AlertEngine::spawn(Arc::new(state), config)
    }

    fn engine(&self) -> AlertEngine {
        self.engine_with(self.book.clone(), fast_sender())
    }

    async fn target(&self, name: &str, format: &str) {
        let (status, body) = self
            .env
            .admin()
            .await
            .post(
                "/notification-targets",
                json!({"name": name, "target": {"kind": "webhook", "url": format!("{}/{name}", self.hook.base), "format": format}}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    async fn rule(&self, name: &str, spec: Value) {
        let (status, body) = self
            .env
            .admin()
            .await
            .post("/alert-rules", json!({"name": name, "spec": spec}))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    /// 시계를 `ms` 옮기고 그 시각의 CPU 점을 넣는다.
    fn step(&self, ms: i64, cpu: f32) {
        self.clock.advance(ms);
        self.metrics.cpu(self.clock.now_ms(), cpu);
    }

    async fn alerts(&self) -> Value {
        let (status, body) = self.env.admin().await.get("/alerts").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// `/alerts`가 `pred`를 만족할 때까지 기다린다.
    async fn wait_alerts(&self, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let a = self.alerts().await;
            if pred(&a) {
                return a;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "alerts never matched: {a}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn fast_sender() -> SenderConfig {
    SenderConfig {
        timeout: Duration::from_secs(5),
        retries: vec![Duration::from_millis(50)],
        ..SenderConfig::default()
    }
}

/// 엔진이 여러 번 돌 시간
async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

fn state_of(a: &Value) -> Vec<String> {
    a["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["state"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_metric_alert_goes_pending_firing_repeats_and_resolves() {
    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    rig.rule(
        "cpu-hot",
        json!({"condition": {"kind": "node_metric", "metric": "cpu", "op": "gt", "value": 90},
               "for_secs": 60, "repeat_secs": 600, "severity": "critical", "targets": ["ops"]}),
    )
    .await;
    rig.metrics.cpu(T0, 95.0);
    let engine = rig.engine();

    // 지속 시간 전에는 pending이고 알리지 않는다.
    let a = rig.wait_alerts(|a| state_of(a) == ["pending"]).await;
    assert_eq!(a["alerts"][0]["rule_name"], "cpu-hot");
    assert_eq!(a["alerts"][0]["severity"], "critical");
    assert_eq!(a["alerts"][0]["threshold"], 90.0);
    assert_eq!(a["alerts"][0]["subject"]["name"], "node1");
    assert_eq!(a["unreachable"], json!([]));
    assert_eq!(a["unsupported"], json!([]));
    rig.step(30_000, 96.0);
    settle().await;
    assert!(rig.hook.hits().is_empty());

    // 60초가 지나면 firing으로 알린다.
    rig.step(31_000, 97.0);
    let hits = rig.hook.wait(1).await;
    let v = hits[0].json();
    assert_eq!(hits[0].path, "/ops");
    assert_eq!(v["state"], "firing");
    assert_eq!(v["rule"], "cpu-hot");
    assert_eq!(v["severity"], "critical");
    assert_eq!(v["value"], 97.0);
    assert_eq!(v["threshold"], 90.0);
    assert_eq!(v["since"], T0);
    assert_eq!(v["subject"]["kind"], "node");
    rig.wait_alerts(|a| state_of(a) == ["firing"]).await;

    // `repeat_secs`(600초) 전에는 다시 보내지 않고, 지나면 다시 보낸다.
    rig.step(300_000, 98.0);
    settle().await;
    assert_eq!(rig.hook.hits().len(), 1);
    rig.step(301_000, 98.0);
    let hits = rig.hook.wait(2).await;
    assert_eq!(hits[1].json()["state"], "firing");

    // 거짓이 되어도 60초 동안은 firing으로 남고(깜박임 막기), 지나면 해제를 알린다.
    rig.step(1_000, 50.0);
    rig.wait_alerts(|a| a["alerts"][0]["clear_since"].is_i64())
        .await;
    rig.step(30_000, 50.0);
    settle().await;
    assert_eq!(rig.hook.hits().len(), 2);
    rig.step(31_000, 40.0);
    let hits = rig.hook.wait(3).await;
    let v = hits[2].json();
    assert_eq!(v["state"], "resolved");
    assert_eq!(v["value"], 50.0);
    rig.wait_alerts(|a| a["alerts"] == json!([])).await;

    // 보낸 기록: 새 것부터
    let (status, body) = rig
        .env
        .admin()
        .await
        .get(&format!(
            "/alerts/history?since=0&until={}",
            T0 + 86_400_000
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let states: Vec<&str> = body["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["resolved", "firing", "firing"]);
    assert_eq!(body["records"][0]["node"], "node1");
    assert_eq!(body["records"][0]["target"], "ops");
    assert_eq!(body["records"][0]["rule"], "cpu-hot");
    assert_eq!(body["records"][0]["ok"], true);
    assert_eq!(body["truncated"], false);
    let (_, body) = rig
        .env
        .admin()
        .await
        .get(&format!(
            "/alerts/history?since=0&until={}&limit=1",
            T0 + 86_400_000
        ))
        .await;
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
    assert_eq!(body["truncated"], true);
    engine.shutdown().await;
}

/// 다시 시작한 엔진은 장부 파일에서 활성 경보와 보낸 이벤트를 되살려 같은 알림을 다시 보내지 않는다.
#[tokio::test]
async fn a_restart_does_not_resend() {
    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    rig.rule(
        "cpu-hot",
        json!({"condition": {"kind": "node_metric", "metric": "cpu", "op": "gt", "value": 90},
               "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    rig.rule(
        "task-failed",
        json!({"condition": {"kind": "task_failed"}, "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    rig.metrics.cpu(T0, 95.0);
    let engine = rig.engine();
    rig.hook.wait(1).await;
    let task = failed_task(&rig.env, TaskOp::VmStart).await;
    let hits = rig.hook.wait(2).await;
    let event = hits[1].json();
    assert_eq!(event["state"], "event");
    assert_eq!(event["rule"], "task-failed");
    assert_eq!(event["subject"]["kind"], "task");
    assert_eq!(event["subject"]["id"], task.to_string());
    assert!(
        event["detail"].as_str().unwrap().contains("disk on fire"),
        "{event}"
    );
    settle().await;
    engine.shutdown().await;

    // 처리한 리비전을 그 Task 앞으로 되돌려 다시 시작하면 그 Task를 다시 보지만 보내지 않는다.
    let path = rig.dir.path().join("alerts/state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(state["active"][0]["state"], "firing");
    let task_rev = rig
        .env
        .cluster
        .get::<TaskSpec>(task)
        .unwrap()
        .unwrap()
        .revision;
    state["revision"] = json!(task_rev - 1);
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let reopened = AlertBook::open(rig.dir.path().join("alerts"), "node1").unwrap();
    let engine = rig.engine_with(reopened, fast_sender());
    rig.step(5_000, 96.0);
    settle().await;
    rig.step(5_000, 97.0);
    settle().await;
    assert_eq!(rig.hook.hits().len(), 2, "{:?}", rig.hook.hits());
    engine.shutdown().await;
}

async fn failed_task(env: &Env, op: TaskOp) -> Uuid {
    let mut task: Resource<TaskSpec, TaskStatus> = Resource::new(
        format!("t-{}", Uuid::now_v7()),
        TaskSpec {
            op,
            target: Uuid::now_v7(),
            node: env.node,
        },
    );
    task.status = TaskStatus {
        state: TaskState::Failed,
        error: Some("disk on fire".into()),
        progress: None,
    };
    env.cluster.create(&task).await.unwrap();
    task.id
}

/// 이름이 `name`인 Node 리소스를 클러스터 멤버로 두지 않고 만든다.
async fn add_node(env: &Env, name: &str, status: NodeStatus) -> Uuid {
    let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            sdn: None,
            host_resources: None,
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    r.status = status;
    env.cluster.create(&r).await.unwrap();
    r.id
}

/// 리더 경보(`node_offline`)는 Raft에 상태를 두므로 새 리더(메모리가 빈 다른 엔진)가 이어받아 다시 보내지 않는다.
#[tokio::test]
async fn a_leader_change_does_not_resend_leader_alerts() {
    let rig = Rig::start().await;
    rig.target("ops", "slack").await;
    let down = add_node(
        &rig.env,
        "node2",
        NodeStatus {
            online: false,
            features: vec![NOTIFICATIONS.into()],
            ..NodeStatus::default()
        },
    )
    .await;
    rig.rule(
        "node-down",
        json!({"condition": {"kind": "node_offline"}, "severity": "critical", "targets": ["ops"]}),
    )
    .await;
    let engine = rig.engine();
    let hits = rig.hook.wait(1).await;
    let text = hits[0].json()["text"].as_str().unwrap().to_string();
    assert!(
        text.starts_with("*[Orbivisor test] FIRING critical: node-down on node2*"),
        "{text}"
    );
    let admin = rig.env.admin().await;
    let (_, rule) = admin.get("/alert-rules/node-down").await;
    assert_eq!(rule["status"]["active"][0]["state"], "firing");
    assert_eq!(
        rule["status"]["active"][0]["subject"]["id"],
        down.to_string()
    );
    // 리더 경보도 `/alerts`에 보인다(node2는 꺼져 있어 묻지 않는다).
    let a = rig.alerts().await;
    assert_eq!(a["alerts"][0]["rule_name"], "node-down");
    assert_eq!(a["unreachable"], json!(["node2"]));
    engine.shutdown().await;

    let other = rig.engine_with(AlertBook::memory("node1"), fast_sender());
    settle().await;
    rig.step(20_000, 1.0);
    settle().await;
    assert_eq!(rig.hook.hits().len(), 1);

    // 다시 켜지면 60초 뒤 새 리더가 해제를 알린다.
    let mut node = rig.env.cluster.get::<NodeSpec>(down).unwrap().unwrap();
    node.status.online = true;
    rig.env
        .cluster
        .update_status::<NodeSpec>(down, &node.status)
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (_, rule) = admin.get("/alert-rules/node-down").await;
        if rule["status"]["active"][0]["clear_since"].is_i64() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{rule}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rig.step(RESOLVE_AFTER_MS + 1_000, 1.0);
    let hits = rig.hook.wait(2).await;
    assert!(
        hits[1].json()["text"]
            .as_str()
            .unwrap()
            .contains("RESOLVED critical: node-down on node2")
    );
    let (_, rule) = admin.get("/alert-rules/node-down").await;
    assert!(rule["status"].get("active").is_none(), "{rule}");
    other.shutdown().await;
}

/// 대상마다 분당 상한을 넘는 알림은 보내지 않고 모았다가 "N건 더" 알림 하나로 알린다. 실패한 보내기는 다시 한다.
#[tokio::test]
async fn sends_are_retried_and_rate_limited() {
    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    rig.rule(
        "task-failed",
        json!({"condition": {"kind": "task_failed", "ops": ["vm-start"]}, "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    let sender = SenderConfig {
        per_window: 2,
        // 바쁜 기계에서도 뒤의 넷이 처음 것과 같은 창에 들게 넉넉히 둔다(500ms면 가끔 새 창이 열렸다).
        window: Duration::from_secs(5),
        ..fast_sender()
    };
    let engine = rig.engine_with(rig.book.clone(), sender);
    settle().await;
    // 처음 보내기는 실패하고 50ms 뒤 다시 한다.
    rig.hook.fail.store(1, Ordering::SeqCst);
    failed_task(&rig.env, TaskOp::VmStart).await;
    rig.hook.wait(1).await;
    // 거르기에 맞지 않는 Task는 알리지 않는다.
    failed_task(&rig.env, TaskOp::VmStop).await;
    settle().await;
    assert_eq!(rig.hook.hits().len(), 1);
    for _ in 0..4 {
        failed_task(&rig.env, TaskOp::VmStart).await;
    }
    // 창(5초)에 둘까지: 처음 것과 하나 더, 그다음 창에 "N건 더"
    let hits = rig.hook.wait(3).await;
    let last = hits.last().unwrap().json();
    assert_eq!(last["state"], "firing");
    assert!(
        last["detail"]
            .as_str()
            .unwrap()
            .contains("3 more notifications were held back"),
        "{last}"
    );
    settle().await;
    assert_eq!(rig.hook.hits().len(), 3);
    engine.shutdown().await;
}

// ---- 대상 API ----

fn target_body(name: &str, target: Value) -> Value {
    json!({"name": name, "target": target})
}

#[tokio::test]
async fn targets_are_admin_only_and_never_show_secrets() {
    let rig = Rig::start().await;
    let admin = rig.env.admin().await;
    let url = format!("{}/hook/SECRETPATH?key=SECRETQUERY", rig.hook.base);
    let (status, body) = admin
        .post(
            "/notification-targets",
            target_body(
                "ntfy",
                json!({"kind": "webhook", "url": url, "format": "ntfy", "token": "SECRETTOKEN",
                       "headers": {"X-Api-Key": "SECRETHEADER"}}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let port = rig.hook.base.rsplit(':').next().unwrap();
    assert_eq!(body["target"]["url"], format!("http://127.0.0.1:{port}/…"));
    assert_eq!(body["target"]["has_token"], true);
    assert_eq!(body["target"]["header_names"], json!(["X-Api-Key"]));
    assert_eq!(body["enabled"], true);
    let (status, body) = admin
        .post(
            "/notification-targets",
            target_body(
                "mail",
                json!({"kind": "smtp", "host": "127.0.0.1", "port": 2525, "tls": "none",
                       "user": "ops", "password": "SECRETPASSWORD", "from": "o@example.com", "to": ["a@example.com"]}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["target"]["has_password"], true);
    let (status, list) = admin.get("/notification-targets").await;
    assert_eq!(status, StatusCode::OK);
    let (_, one) = admin.get("/notification-targets/ntfy").await;
    for text in [list.to_string(), one.to_string(), body.to_string()] {
        for secret in [
            "SECRETPATH",
            "SECRETQUERY",
            "SECRETTOKEN",
            "SECRETHEADER",
            "SECRETPASSWORD",
        ] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }

    // viewer·operator는 대상을 읽지도 쓰지도 못한다. 규칙은 모두 읽고 admin만 쓴다.
    for role in ["viewer", "operator"] {
        let user = rig.env.user(&format!("u-{role}"), role).await;
        assert_eq!(
            user.get("/notification-targets").await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            user.get("/notification-targets/ntfy").await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            user.post("/notification-targets/ntfy/test", json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(user.get("/alert-rules").await.0, StatusCode::OK);
        assert_eq!(user.get("/alerts").await.0, StatusCode::OK);
        assert_eq!(user.get("/alerts/history").await.0, StatusCode::OK);
        let got = user
            .post(
                "/alert-rules",
                json!({"name": "x", "spec": {"condition": {"kind": "vm_failed"}, "severity": "info", "targets": ["ntfy"]}}),
            )
            .await;
        assert_eq!(got.0, StatusCode::FORBIDDEN);
    }

    // 바꾸기: 줄인 URL·빈 헤더 값·빠진 토큰은 저장된 비밀을 지킨다. 시험 보내기로 확인한다.
    let (status, body) = admin
        .put(
            "/notification-targets/ntfy",
            json!({"target": {"kind": "webhook", "url": format!("http://127.0.0.1:{port}/…"), "format": "ntfy",
                              "headers": {"X-Api-Key": ""}}}),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["target"]["has_token"], true);
    let (status, body) = admin
        .post("/notification-targets/ntfy/test", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"ok": true}));
    let hit = rig.hook.wait(1).await.remove(0);
    assert_eq!(hit.path, "/hook/SECRETPATH");
    assert_eq!(hit.headers["x-api-key"], "SECRETHEADER");
    assert_eq!(hit.headers["authorization"], "Bearer SECRETTOKEN");
    assert!(
        hit.headers["title"].starts_with("[Orbivisor test] TEST:"),
        "{hit:?}"
    );
    // 보낸 기록: admin이 아닌 사용자에게는 대상 이름과 오류 문구를 보이지 않는다(리뷰 1 #9).
    let (_, history) = admin.get("/alerts/history?since=0").await;
    assert_eq!(history["records"][0]["target"], "ntfy", "{history}");
    let viewer = rig.env.user("u-history", "viewer").await;
    let (status, history) = viewer.get("/alerts/history?since=0").await;
    assert_eq!(status, StatusCode::OK);
    let records = history["records"].as_array().unwrap();
    assert!(!records.is_empty(), "{history}");
    for r in records {
        assert!(r.get("target").is_none() && r.get("error").is_none(), "{r}");
    }
    // 다른 호스트나 같은 호스트의 다른 경로로 바꾸면 비밀을 다시 줘야 한다(리뷰 1 #8).
    for url in [
        "http://localhost:9/x".to_string(),
        format!("http://127.0.0.1:{port}/hook/OTHERPATH"),
    ] {
        let got = admin
            .put(
                "/notification-targets/ntfy",
                json!({"target": {"kind": "webhook", "url": url, "format": "ntfy"}}),
                None,
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert!(
            got.1["message"].as_str().unwrap().contains("token"),
            "{:?}",
            got.1
        );
    }
    let got = admin
        .put(
            "/notification-targets/mail",
            json!({"target": {"kind": "smtp", "host": "mail.example.com", "port": 2525, "tls": "none",
                              "user": "ops", "from": "o@example.com", "to": ["a@example.com"]}}),
            None,
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    // TLS 방식만 바꿔도 비밀번호를 다시 줘야 한다(리뷰 1 #4).
    let got = admin
        .put(
            "/notification-targets/mail",
            json!({"target": {"kind": "smtp", "host": "127.0.0.1", "port": 2525, "tls": "starttls",
                              "user": "ops", "from": "o@example.com", "to": ["a@example.com"]}}),
            None,
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"].as_str().unwrap().contains("tls"),
        "{:?}",
        got.1
    );
    // 이름은 바꾸지 못한다.
    let got = admin
        .put(
            "/notification-targets/mail",
            json!({"name": "other", "target": {"kind": "smtp", "host": "127.0.0.1", "port": 2525, "tls": "none",
                              "from": "o@example.com", "to": ["a@example.com"]}}),
            None,
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");

    // 규칙이 쓰는 대상은 지우지 못한다. 없는 대상은 규칙에 넣지 못한다.
    let got = admin
        .post(
            "/alert-rules",
            json!({"name": "r", "spec": {"condition": {"kind": "vm_failed"}, "severity": "info", "targets": ["nope"]}}),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    let got = admin
        .post(
            "/alert-rules",
            json!({"name": "r", "spec": {"condition": {"kind": "vm_failed"}, "severity": "info", "targets": ["ntfy"]}}),
        )
        .await;
    assert_eq!(got.0, StatusCode::CREATED, "{:?}", got.1);
    let got = admin.delete("/notification-targets/ntfy").await;
    assert_error(&got, StatusCode::CONFLICT, "in_use");
    assert_eq!(
        admin.delete("/alert-rules/r").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        admin.delete("/notification-targets/ntfy").await.0,
        StatusCode::NO_CONTENT
    );

    // 감사 로그에는 설명만 있고 비밀은 없다.
    let (_, audit) = admin
        .get(&format!("/audit?since=0&until={}", u64::MAX / 2))
        .await;
    let details: Vec<String> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["detail"].as_str().map(str::to_string))
        .collect();
    for want in [
        "notification-target create ntfy",
        "notification-target update ntfy",
        "notification-target test ntfy",
        "notification-target delete ntfy",
        "alert-rule create r",
        "alert-rule delete r",
    ] {
        assert!(
            details.iter().any(|d| d == want),
            "{want} not in {details:?}"
        );
    }
    let log = std::fs::read_to_string(rig.env.dir.path().join("audit/audit.log")).unwrap();
    // 보낸 기록에도 비밀이 없다.
    let history = std::fs::read_to_string(rig.dir.path().join("alerts/history.log")).unwrap();
    for text in [log, history, audit.to_string()] {
        for secret in [
            "SECRETPATH",
            "SECRETTOKEN",
            "SECRETHEADER",
            "SECRETPASSWORD",
        ] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
}

/// 시험 보내기는 다섯 webhook 형식의 본문과 헤더로 보내고, 실패는 줄인 URL만 담은 오류로 돌려준다.
#[tokio::test]
async fn test_sends_reach_the_webhook_in_each_format() {
    let rig = Rig::start().await;
    let admin = rig.env.admin().await;
    for format in ["json", "slack", "teams", "ntfy", "gotify"] {
        let mut target = json!({"kind": "webhook", "url": format!("{}/{format}", rig.hook.base), "format": format});
        if matches!(format, "ntfy" | "gotify") {
            target["token"] = json!("tok");
        }
        let (status, body) = admin
            .post("/notification-targets", target_body(format, target))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, body) = admin
            .post(&format!("/notification-targets/{format}/test"), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], true, "{body}");
    }
    let hits = rig.hook.wait(5).await;
    let by = |p: &str| {
        hits.iter()
            .find(|h| h.path == format!("/{p}"))
            .unwrap()
            .clone()
    };
    let json_hit = by("json");
    assert_eq!(json_hit.headers["content-type"], "application/json");
    let v = json_hit.json();
    assert_eq!(v["version"], 1);
    assert_eq!(v["state"], "test");
    assert_eq!(v["cluster"], "test");
    assert_eq!(v["severity"], "info");
    assert_eq!(v["rule"], Value::Null);
    assert!(
        by("slack").json()["text"]
            .as_str()
            .unwrap()
            .contains("TEST")
    );
    assert_eq!(
        by("teams").json()["attachments"][0]["content"]["type"],
        "AdaptiveCard"
    );
    let ntfy = by("ntfy");
    assert!(ntfy.headers["content-type"].starts_with("text/plain"));
    assert_eq!(ntfy.headers["authorization"], "Bearer tok");
    assert_eq!(ntfy.headers["priority"], "3");
    assert_eq!(ntfy.headers["tags"], "test_tube");
    let gotify = by("gotify");
    assert_eq!(gotify.headers["x-gotify-key"], "tok");
    assert_eq!(gotify.json()["priority"], 2);

    // 닿지 않는 대상
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let (status, _) = admin
        .post(
            "/notification-targets",
            target_body(
                "dead",
                json!({"kind": "webhook", "url": format!("http://127.0.0.1:{port}/SECRET")}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = admin
        .post("/notification-targets/dead/test", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], false);
    let error = body["error"].as_str().unwrap();
    assert!(
        error.starts_with(&format!("http://127.0.0.1:{port}/…")),
        "{error}"
    );
    assert!(!error.contains("SECRET"), "{error}");

    // 시험 보내기도 보낸 기록에 남는다.
    let (_, body) = admin.get("/alerts/history").await;
    let records = body["records"].as_array().unwrap();
    assert_eq!(records.len(), 6);
    assert!(records.iter().all(|r| r["state"] == "test"));
    assert_eq!(records[0]["target"], "dead");
    assert_eq!(records[0]["ok"], false);
}

/// 시험 SMTP 서버: 대화를 적고 메일을 받는다.
async fn fake_smtp() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = lines.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut r = BufReader::new(r);
                w.write_all(b"220 fake ESMTP\r\n").await.unwrap();
                let mut data = false;
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    seen.lock().unwrap().push(line.trim_end().to_string());
                    if data {
                        if line == ".\r\n" {
                            data = false;
                            w.write_all(b"250 queued\r\n").await.unwrap();
                        }
                        continue;
                    }
                    let upper = line.to_ascii_uppercase();
                    let reply: &[u8] = if upper.starts_with("EHLO") {
                        b"250-fake\r\n250 AUTH PLAIN LOGIN\r\n"
                    } else if upper.starts_with("AUTH") {
                        b"235 ok\r\n"
                    } else if upper.starts_with("DATA") {
                        data = true;
                        b"354 go\r\n"
                    } else if upper.starts_with("QUIT") {
                        w.write_all(b"221 bye\r\n").await.unwrap();
                        return;
                    } else {
                        b"250 ok\r\n"
                    };
                    w.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, lines)
}

#[tokio::test]
async fn test_sends_reach_an_smtp_server() {
    let rig = Rig::start().await;
    let admin = rig.env.admin().await;
    let (port, lines) = fake_smtp().await;
    let (status, body) = admin
        .post(
            "/notification-targets",
            target_body(
                "mail",
                json!({"kind": "smtp", "host": "127.0.0.1", "port": port, "tls": "none",
                       "user": "ops", "password": "pw", "from": "orbivisor@example.com",
                       "to": ["a@example.com", "b@example.com"]}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post("/notification-targets/mail/test", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"ok": true}));
    let lines = lines.lock().unwrap().clone();
    assert!(lines.iter().any(|l| l.starts_with("AUTH ")), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l == "MAIL FROM:<orbivisor@example.com>"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l == "RCPT TO:<b@example.com>"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Subject: [Orbivisor test] TEST:")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("test notification for target mail")),
        "{lines:?}"
    );
}

/// 쓰기는 모든 노드가 `notifications`를 알 때만 받는다. 읽기는 옛 노드를 `unsupported`로 보인다.
#[tokio::test]
async fn writes_wait_for_every_node_to_know_notifications() {
    let rig = Rig::start().await;
    let admin = rig.env.admin().await;
    rig.target("ops", "json").await;
    add_node(
        &rig.env,
        "old",
        NodeStatus {
            online: true,
            ..NodeStatus::default()
        },
    )
    .await;
    // 기능 검사가 검증보다 먼저다.
    let got = admin
        .post(
            "/alert-rules",
            json!({"name": "r", "spec": {"condition": {"kind": "vm_failed"}, "severity": "info", "targets": []}}),
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    let got = admin
        .post(
            "/notification-targets",
            target_body(
                "t2",
                json!({"kind": "webhook", "url": "http://127.0.0.1:1/"}),
            ),
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    let a = rig.alerts().await;
    assert_eq!(a["unsupported"], json!(["old"]));
    let (_, h) = admin.get("/alerts/history").await;
    assert_eq!(h["unsupported"], json!(["old"]));
    // 나쁜 값
    let got = admin.get("/alerts/history?limit=0").await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    let got = admin.get("/alerts/history?since=yesterday").await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
}

#[tokio::test]
async fn rules_are_validated() {
    let rig = Rig::start().await;
    let admin = rig.env.admin().await;
    rig.target("ops", "json").await;
    for spec in [
        // 이벤트 조건은 지속 시간이 없다.
        json!({"condition": {"kind": "task_failed"}, "for_secs": 10, "severity": "info", "targets": ["ops"]}),
        // 대상이 없다.
        json!({"condition": {"kind": "vm_failed"}, "severity": "info", "targets": []}),
        // 모르는 칸
        json!({"condition": {"kind": "vm_failed"}, "severity": "info", "targets": ["ops"], "bogus": 1}),
    ] {
        let got = admin
            .post("/alert-rules", json!({"name": "r", "spec": spec}))
            .await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{:?}", got.1);
    }
    rig.rule(
        "pool-full",
        json!({"condition": {"kind": "pool_usage", "percent": 85}, "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    let (status, body) = admin
        .put(
            "/alert-rules/pool-full",
            json!({"spec": {"condition": {"kind": "pool_usage", "percent": 90}, "severity": "critical", "targets": ["ops"], "enabled": false}}),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["enabled"], false);
    assert_eq!(body["spec"]["condition"]["percent"], 90.0);
    let (_, list) = admin.get("/alert-rules").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
}

/// 로컬 풀 사용률 경보는 그 노드가 평가하고, 유지 보수에 들어간 노드의 노드 지표 경보는 알림 없이 뺀다.
#[tokio::test]
async fn pool_usage_and_maintenance() {
    use orbivisor_types::node::PoolState;
    let rig = Rig::start().await;
    let admin = rig.env.admin().await;
    rig.target("ops", "json").await;
    let (status, pool) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": "/srv/pool" } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{pool}");
    common::set_pool_state(
        &rig.env.cluster,
        rig.env.node,
        "local",
        PoolState {
            ready: true,
            total_bytes: 100 << 30,
            available_bytes: 10 << 30,
            ..Default::default()
        },
    )
    .await;
    rig.rule(
        "pool-full",
        json!({"condition": {"kind": "pool_usage", "percent": 85}, "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    rig.rule(
        "cpu-hot",
        json!({"condition": {"kind": "node_metric", "metric": "cpu", "op": "gt", "value": 90},
               "severity": "critical", "targets": ["ops"]}),
    )
    .await;
    rig.metrics.cpu(T0, 99.0);
    let engine = rig.engine();
    let hits = rig.hook.wait(2).await;
    let pool_hit = hits
        .iter()
        .map(Hit::json)
        .find(|v| v["rule"] == "pool-full")
        .unwrap();
    assert_eq!(pool_hit["subject"]["kind"], "pool");
    assert_eq!(pool_hit["subject"]["name"], "local");
    assert_eq!(pool_hit["value"], 90.0);
    assert_eq!(pool_hit["threshold"], 85.0);
    let a = rig
        .wait_alerts(|a| a["alerts"].as_array().unwrap().len() == 2)
        .await;
    // 심각도 높은 것 먼저
    assert_eq!(a["alerts"][0]["rule_name"], "cpu-hot");

    // 유지 보수: 노드 지표 경보는 알림 없이 빠지고 풀 경보는 남는다.
    let mut node = rig
        .env
        .cluster
        .get::<NodeSpec>(rig.env.node)
        .unwrap()
        .unwrap();
    node.spec.maintenance = true;
    rig.env
        .cluster
        .update_spec(rig.env.node, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    let a = rig
        .wait_alerts(|a| a["alerts"].as_array().unwrap().len() == 1)
        .await;
    assert_eq!(a["alerts"][0]["rule_name"], "pool-full");
    rig.step(5_000, 99.0);
    settle().await;
    assert_eq!(rig.hook.hits().len(), 2);

    // 규칙을 끄면 그 경보도 알림 없이 빠진다.
    let (status, body) = admin
        .put(
            "/alert-rules/pool-full",
            json!({"spec": {"condition": {"kind": "pool_usage", "percent": 85}, "severity": "warning",
                            "targets": ["ops"], "enabled": false}}),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.wait_alerts(|a| a["alerts"] == json!([])).await;
    settle().await;
    assert_eq!(rig.hook.hits().len(), 2);
    engine.shutdown().await;
}

/// SDN 경보(계획 67 §11): "SDN 경보 켜기"가 권하는 여덟 규칙을 만들고(다시 부르면 그대로 둠), 리더 엔진이 chassis 꺼짐을
/// `for_secs` 뒤 알리고(까닭 문구), OvnSync 관찰의 게이트웨이 주인이 바뀌면 넘김 이벤트를 한 번 보낸다(같은 임기의 앞
/// 관찰과 견줌). 다른 임기의 관찰은 쓰지 않는다. chassis가 돌아오면 해제한다.
#[tokio::test]
async fn sdn_alerts_fire_from_node_state_and_observations() {
    use orbivisor_types::alert::sdn::SdnObservations;
    use orbivisor_types::node::FEATURES;
    use orbivisor_types::router::RouterSpec;
    use orbivisor_types::sdn::{
        CentralHealth, ChassisState, NodeSdn, RaftRole, SDN_NAME, SdnCentral, SdnPhase, SdnSpec,
        SdnStatus,
    };

    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    let admin = rig.env.admin().await;
    let (status, body) = admin
        .post("/alert-rules/sdn-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"].as_array().unwrap().len(), 8, "{body}");
    let (_, again) = admin
        .post("/alert-rules/sdn-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(again["created"], json!([]), "{again}");
    assert_eq!(again["existing"].as_array().unwrap().len(), 8, "{again}");
    let missing = admin
        .post("/alert-rules/sdn-defaults", json!({"targets": ["nobody"]}))
        .await;
    assert_error(&missing, StatusCode::BAD_REQUEST, "invalid_request");

    // SDN(중앙 = 이 노드)과 chassis가 꺼진 node2, 라우터 하나
    let mut sdn = Resource::new(SDN_NAME, SdnSpec::default());
    sdn.status = SdnStatus {
        phase: SdnPhase::Ready,
        centrals: vec![SdnCentral {
            node: rig.env.node,
            address: "127.0.0.1".parse().unwrap(),
        }],
        ..SdnStatus::default()
    };
    rig.env.cluster.create(&sdn).await.unwrap();
    // 이 노드는 건강한 중앙(정족수 경보가 뜨지 않게)
    let mut me = rig
        .env
        .cluster
        .get::<NodeSpec>(rig.env.node)
        .unwrap()
        .unwrap();
    me.status.sdn = Some(NodeSdn {
        chassis: ChassisState::Ready,
        central: Some(CentralHealth {
            nb: RaftRole::Leader,
            sb: RaftRole::Leader,
            northd_active: true,
        }),
        ..NodeSdn::default()
    });
    rig.env
        .cluster
        .update_status::<NodeSpec>(rig.env.node, &me.status)
        .await
        .unwrap();
    let node2 = add_node(
        &rig.env,
        "node2",
        NodeStatus {
            online: true,
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
            sdn: Some(NodeSdn {
                chassis: ChassisState::Down,
                ..NodeSdn::default()
            }),
            ..NodeStatus::default()
        },
    )
    .await;
    let router: Resource<RouterSpec, orbivisor_types::router::RouterStatus> =
        Resource::new("r1", RouterSpec::default());
    rig.env.cluster.create(&router).await.unwrap();

    // 관찰을 나누는 OvnSync 설정을 단 엔진
    let ovn = orbivisor_api::OvnSyncConfig::new("t", orbivisor_api::OvnTransport::Tcp);
    let observations = ovn.observations.clone();
    let hv: Arc<dyn Hypervisor> = rig.env.hv.clone();
    let mut state = AppState::new(
        rig.env.cluster.clone(),
        rig.env.node,
        hv,
        rig.env.dir.path().join("uploads"),
    );
    state.alerts = Some(rig.book.clone());
    state.ovn = Some(ovn);
    let engine = AlertEngine::spawn(
        Arc::new(state),
        AlertEngineConfig {
            tick: Duration::from_millis(20),
            resolve_after_ms: RESOLVE_AFTER_MS,
            clock: rig.clock.clone(),
            sender: fast_sender(),
        },
    );
    settle().await;
    // 60초 전에는 pending
    let (_, rule) = admin.get("/alert-rules/sdn-chassis-down").await;
    assert_eq!(rule["status"]["active"][0]["state"], "pending", "{rule}");
    assert_eq!(
        rule["status"]["active"][0]["subject"]["id"],
        node2.to_string()
    );
    assert!(rig.hook.hits().is_empty());
    rig.step(61_000, 1.0);
    let hits = rig.hook.wait(1).await;
    let fired = hits[0].json();
    assert_eq!(fired["rule"], "sdn-chassis-down", "{fired}");
    assert_eq!(fired["state"], "firing", "{fired}");
    assert_eq!(fired["subject"]["kind"], "node", "{fired}");
    assert!(
        fired["detail"]
            .as_str()
            .unwrap()
            .contains("OVN chassis node2 is down: ovn-controller is not running"),
        "{fired}"
    );

    // 게이트웨이 넘김: 다른 임기의 관찰은 쓰지 않고, 같은 임기의 두 관찰이 다르면 이벤트 하나
    let term = rig.env.cluster.leader_term().unwrap();
    let now = rig.clock.now_ms();
    let obs = |term: u64, at: i64, host: Uuid| SdnObservations {
        term,
        at: now + at,
        hosting: [(router.id, host.to_string())].into(),
        monitors: vec![],
    };
    observations.set(obs(term + 1, 1, rig.env.node));
    settle().await;
    observations.set(obs(term + 1, 2, node2));
    settle().await;
    assert_eq!(rig.hook.hits().len(), 1);
    observations.set(obs(term, 3, rig.env.node));
    settle().await;
    observations.set(obs(term, 4, node2));
    let hits = rig.hook.wait(2).await;
    let moved = hits[1].json();
    assert_eq!(moved["rule"], "sdn-gateway-failover", "{moved}");
    assert_eq!(moved["subject"]["kind"], "router", "{moved}");
    assert_eq!(
        moved["detail"], "The gateway of router r1 moved from node1 to node2",
        "{moved}"
    );
    settle().await;
    assert_eq!(rig.hook.hits().len(), 2, "the failover was sent once");
    // 리뷰 2 m5: 주인이 없던 관찰을 지나 되돌아와도(node2 → 없음 → node1) 넘김으로 보고, 오가는 BFD(다시 node2 →
    // node1)는 `repeat_secs` 안에 한 번만 보낸다.
    let none = |at: i64| SdnObservations {
        term,
        at: now + at,
        hosting: Default::default(),
        monitors: vec![],
    };
    observations.set(none(5));
    settle().await;
    observations.set(obs(term, 6, rig.env.node));
    let hits = rig.hook.wait(3).await;
    assert_eq!(
        hits[2].json()["detail"],
        "The gateway of router r1 moved from node2 to node1"
    );
    for (at, host) in [(7, node2), (8, rig.env.node)] {
        observations.set(obs(term, at, host));
        settle().await;
    }
    assert_eq!(rig.hook.hits().len(), 3, "a flapping gateway is damped");

    // chassis가 돌아오면 60초 뒤 해제
    let mut n2 = rig.env.cluster.get::<NodeSpec>(node2).unwrap().unwrap();
    n2.status.sdn.as_mut().unwrap().chassis = ChassisState::Ready;
    rig.env
        .cluster
        .update_status::<NodeSpec>(node2, &n2.status)
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (_, rule) = admin.get("/alert-rules/sdn-chassis-down").await;
        if rule["status"]["active"][0]["clear_since"].is_i64() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{rule}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rig.step(RESOLVE_AFTER_MS + 1_000, 1.0);
    let hits = rig.hook.wait(4).await;
    assert_eq!(hits[3].json()["state"], "resolved", "{:?}", hits[3].json());
    engine.shutdown().await;
}

/// 백업 경보 둘(계획 68 §10.5·§6): 검증이 실패한 저장소 백업마다 `backup_verify_failed`, 노드가 닿지 않는다고 보고한
/// 저장소마다 `backup_store_unavailable`. 다시 검증해 통과하거나 노드가 다시 닿으면 풀린다. 규칙은 API 스테이지 전이라
/// 클러스터에 바로 쓴다.
#[tokio::test]
async fn backup_verify_and_store_alerts_follow_backup_and_node_state() {
    use orbivisor_types::alert::AlertRuleSpec;
    use orbivisor_types::backup::{Backup, BackupSpec, BackupVerify, VerifyState};
    use orbivisor_types::backup_store::{BackupStoreSpec, NodeStoreState, StorePhase};

    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    for (name, kind) in [
        ("verify", "backup_verify_failed"),
        ("store", "backup_store_unavailable"),
    ] {
        let spec: AlertRuleSpec = serde_json::from_value(
            json!({"condition": {"kind": kind}, "severity": "critical", "targets": ["ops"]}),
        )
        .unwrap();
        rig.env
            .cluster
            .create(&Resource::new(name, spec))
            .await
            .unwrap();
    }
    let mut store: orbivisor_types::backup_store::BackupStore = Resource::new(
        "st",
        serde_json::from_value::<BackupStoreSpec>(json!({"backend": {"pool": "bk"}})).unwrap(),
    );
    store.status.phase = StorePhase::Ready;
    rig.env.cluster.create(&store).await.unwrap();
    let spec: BackupSpec = serde_json::from_value(json!({
        "vm": Uuid::now_v7(), "vm_name": "web", "store": "st",
        "vm_spec": {"cpu": {"cores": 1}, "memory_mib": 256, "machine": "virt"}
    }))
    .unwrap();
    let mut backup: Backup = Resource::new("web-1", spec);
    backup.status.phase = orbivisor_types::backup::BackupPhase::Ready;
    backup.status.verify = Some(BackupVerify {
        at: 1,
        state: VerifyState::Failed,
        bad_chunks: 2,
        error: None,
    });
    rig.env.cluster.create(&backup).await.unwrap();
    let mut me = rig
        .env
        .cluster
        .get::<NodeSpec>(rig.env.node)
        .unwrap()
        .unwrap();
    me.status.backup_stores = vec![NodeStoreState {
        store: store.id,
        ok: false,
        error: Some("connection refused".into()),
        checked_at: 1,
    }];
    rig.env
        .cluster
        .update_status::<NodeSpec>(rig.env.node, &me.status)
        .await
        .unwrap();
    let _engine = rig.engine();
    let hits = rig.hook.wait(2).await;
    let texts: Vec<String> = hits.iter().map(|h| h.json().to_string()).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("web-1") && t.contains("2 bad chunks")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("connection refused")),
        "{texts:?}"
    );
    let active = |name: &str| {
        rig.env
            .cluster
            .get_by_name::<AlertRuleSpec>(name)
            .unwrap()
            .unwrap()
            .status
            .active
            .len()
    };
    assert_eq!((active("verify"), active("store")), (1, 1));
    // 다시 검증해 통과, 노드가 다시 닿음 → 풀림
    let b = rig
        .env
        .cluster
        .get::<BackupSpec>(backup.id)
        .unwrap()
        .unwrap();
    let mut status = b.status.clone();
    status.verify = Some(BackupVerify {
        at: 2,
        state: VerifyState::Ok,
        bad_chunks: 0,
        error: None,
    });
    rig.env
        .cluster
        .update_status::<BackupSpec>(backup.id, &status)
        .await
        .unwrap();
    let mut me = rig
        .env
        .cluster
        .get::<NodeSpec>(rig.env.node)
        .unwrap()
        .unwrap();
    me.status.backup_stores[0].ok = true;
    me.status.backup_stores[0].error = None;
    rig.env
        .cluster
        .update_status::<NodeSpec>(rig.env.node, &me.status)
        .await
        .unwrap();
    for _ in 0..200 {
        rig.clock.advance(RESOLVE_AFTER_MS + 1);
        if active("verify") == 0 && active("store") == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("backup alerts did not resolve");
}

/// 인증서 경보(계획 73 §11.6·§11.2): 노드가 내부 노드 인증서의 남은 날로 `certificate_expiring`을, 리더가 이어진 주문
/// 실패로 `certificate_renew_failed`를 알린다.
#[tokio::test]
async fn certificate_alerts_follow_expiry_and_renew_failures() {
    use orbivisor_types::acme::{Certificate, CertificateSpec};

    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    rig.rule(
        "expiring",
        json!({"condition": {"kind": "certificate_expiring", "days": 45}, "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    rig.rule(
        "renewal",
        json!({"condition": {"kind": "certificate_renew_failed", "failures": 2}, "severity": "critical", "targets": ["ops"]}),
    )
    .await;
    common::acme::seed_account(&rig.env.cluster, "le", "https://127.0.0.1:1/dir").await;
    let spec: CertificateSpec =
        serde_json::from_value(json!({ "account": "le", "names": ["api.example.com"],
        "scope": { "kind": "shared" }, "challenge": { "kind": "http01" } }))
        .unwrap();
    let mut cert: Certificate = Resource::new("api", spec);
    cert.status.failures = 3;
    cert.status.error = Some("the CA could not connect: refused".into());
    rig.env.cluster.create(&cert).await.unwrap();
    // 노드마다 인증서: 노드마다 센 실패 — 알림이 그 노드를 적는다(리뷰 1).
    let spec: CertificateSpec =
        serde_json::from_value(json!({ "account": "le", "names": ["{node}.example.com"],
        "challenge": { "kind": "http01" } }))
        .unwrap();
    let mut per: Certificate = Resource::new("per", spec);
    for _ in 0..2 {
        per.status.record_failure(
            Some(rig.env.node),
            "node1: no answer at port 80".into(),
            0,
            |_| 300,
        );
    }
    rig.env.cluster.create(&per).await.unwrap();
    // 30일 남은 내부 노드 인증서
    let (node_pem, _) = common::acme::TestCa::new("node CA").issue(&["node1"]);
    let hv: Arc<dyn Hypervisor> = rig.env.hv.clone();
    let mut state = AppState::new(
        rig.env.cluster.clone(),
        rig.env.node,
        hv,
        rig.env.dir.path().join("uploads"),
    );
    state.alerts = Some(rig.book.clone());
    state.acme.node_cert_pem = Some(node_pem);
    let _engine = AlertEngine::spawn(
        Arc::new(state),
        AlertEngineConfig {
            tick: Duration::from_millis(20),
            resolve_after_ms: RESOLVE_AFTER_MS,
            clock: rig.clock.clone(),
            sender: fast_sender(),
        },
    );
    let hits = rig.hook.wait(3).await;
    let texts: Vec<String> = hits.iter().map(|h| h.json().to_string()).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("\"rule\":\"renewal\"") && t.contains("failed 3 ACME orders")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("on 1 node(s)")
                && t.contains("2 in a row: node1: no answer at port 80")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("\"rule\":\"expiring\"") && t.contains("node1")),
        "{texts:?}"
    );
}

/// 적용 묶음 드리프트 경보(계획 73 §12.5): 리더가 드리프트 보고에 객체가 있는 묶음마다 알린다(대상은 묶음).
#[tokio::test]
async fn apply_set_drift_alerts_name_the_set() {
    use orbivisor_types::apply_set::{ApplySet, ApplySetSpec, DriftItem, DriftReport, DriftState};

    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    rig.rule(
        "drift",
        json!({"condition": {"kind": "apply_set_drift"}, "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    let mut set: ApplySet = Resource::new("web", ApplySetSpec::default());
    set.status.drift = Some(DriftReport {
        checked_at: 1,
        revision: 1,
        items: vec![DriftItem {
            kind: orbivisor_types::resource::Kind::SecurityGroup,
            id: Uuid::now_v7(),
            name: "web-sg".into(),
            state: DriftState::Modified,
            paths: vec!["description".into()],
            by: None,
        }],
    });
    rig.env.cluster.create(&set).await.unwrap();
    let _engine = rig.engine();
    let hits = rig.hook.wait(1).await;
    let text = hits[0].json().to_string();
    assert!(text.contains("\"rule\":\"drift\""), "{text}");
    assert!(text.contains("Apply set web drifted: 1 object"), "{text}");
}

/// 링크 경보(계획 75 §3.4, 리뷰 1·2): 본딩 구성 포트 하나가 끊기면(본딩은 흐름) `bond_degraded`(warning) 하나만, 본딩이
/// 통째로 끊기면 본딩 이름으로 `node_link_down`(critical) 하나 — 새 알림이고, 저하 경보는 "해소"를 보내지 않고 그대로다.
/// 문구는 본딩과 구성 포트 상태, 영향(위 브리지·네트워크·VM). 역할 거르기(`uplink`는 쓰지 않는 포트를 보지 않음)와 포트
/// 글롭, 포트가 돌아오면 해제.
#[tokio::test]
async fn link_alerts_fire_per_port_with_the_impact_and_resolve() {
    use orbivisor_types::network::Network;
    use orbivisor_types::node::{FEATURES, NodeInterface};
    use orbivisor_types::vm::{Vm, VmPhase};

    let interfaces = |eno1_up: bool, eno2_up: bool| -> Vec<NodeInterface> {
        let st = |up: bool| if up { "up" } else { "down" };
        let bond = match (eno1_up, eno2_up) {
            (true, true) => "up",
            (false, false) => "down",
            _ => "degraded",
        };
        serde_json::from_value(json!([
            { "name": "eno1", "kind": "ethernet", "state": st(eno1_up), "carrier": eno1_up, "master": "bond0" },
            { "name": "eno2", "kind": "ethernet", "state": st(eno2_up), "carrier": eno2_up, "master": "bond0" },
            { "name": "bond0", "kind": "bond", "state": st(eno1_up || eno2_up), "carrier": eno1_up || eno2_up,
              "master": "vmbr0",
              "bond": { "mode": "active-backup", "state": bond,
                        "ports": [ { "name": "eno1", "state": st(eno1_up) }, { "name": "eno2", "state": st(eno2_up) } ] } },
            { "name": "vmbr0", "kind": "bridge", "state": "up", "carrier": true },
            { "name": "eno3", "kind": "ethernet", "state": "down", "carrier": false }
        ]))
        .unwrap()
    };
    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    let svc01 = add_node(
        &rig.env,
        "svc01",
        NodeStatus {
            online: true,
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
            interfaces: interfaces(true, false),
            ..NodeStatus::default()
        },
    )
    .await;
    let lan: Network = Resource::new(
        "lan",
        serde_json::from_value(json!({ "bridge": "vmbr0" })).unwrap(),
    );
    rig.env.cluster.create(&lan).await.unwrap();
    let mut spec = common::vm_request("web")["spec"].clone();
    spec["nics"] = json!([{ "network": "lan", "model": "virtio" }]);
    let mut vm: Vm = Resource::new("web", serde_json::from_value(spec).unwrap());
    vm.status.phase = VmPhase::Running;
    vm.status.node = Some(svc01);
    rig.env.cluster.create(&vm).await.unwrap();

    rig.rule(
        "uplinks",
        json!({"condition": {"kind": "node_link_down"}, "severity": "critical", "for_secs": 0, "targets": ["ops"]}),
    )
    .await;
    rig.rule(
        "bonds",
        json!({"condition": {"kind": "bond_degraded"}, "severity": "warning", "for_secs": 0, "targets": ["ops"]}),
    )
    .await;
    rig.rule(
        "spare",
        json!({"condition": {"kind": "node_link_down", "ports": "eno3", "role": "any"}, "severity": "info",
               "for_secs": 0, "targets": ["ops"]}),
    )
    .await;
    let engine = rig.engine();
    let hits = rig.hook.wait(2).await;
    settle().await;
    // 구성 포트 하나: 본딩 저하 하나뿐(critical 포트 경보는 없음)
    assert_eq!(rig.hook.hits().len(), 2, "{:?}", rig.hook.hits());
    let by_rule = |hits: &[Hit], rule: &str, state: &str| -> Value {
        hits.iter()
            .map(Hit::json)
            .find(|h| h["rule"] == rule && h["state"] == state)
            .unwrap_or_else(|| panic!("no {state} {rule} in {hits:?}"))
    };
    let bond = by_rule(&hits, "bonds", "firing");
    assert_eq!(
        bond["subject"],
        json!({ "kind": "node", "id": svc01, "name": "svc01", "node": "svc01", "port": "bond0",
                "map": "/cluster/map?focus=port:svc01/bond0" }),
        "{bond}"
    );
    assert_eq!(
        bond["detail"], "bond0 degraded on svc01 (eno2 down) — vmbr0, 1 network, 1 VM",
        "{bond}"
    );
    assert!(
        bond["title"]
            .as_str()
            .unwrap()
            .ends_with("bonds on svc01/bond0"),
        "{bond}"
    );
    // 쓰지 않는 포트는 `role: any`와 글롭으로만 본다(영향 없음).
    let spare = by_rule(&hits, "spare", "firing");
    assert_eq!(spare["subject"]["port"], "eno3", "{spare}");
    assert_eq!(spare["detail"], "eno3 down on svc01", "{spare}");

    // 본딩이 통째로 끊김: 본딩 이름으로 critical 하나(새 알림), 저하 경보는 풀리지 않는다(리뷰 2).
    let set = |eno1: bool, eno2: bool| {
        let env = &rig.env;
        async move {
            let mut n = env.cluster.get::<NodeSpec>(svc01).unwrap().unwrap();
            n.status.interfaces = interfaces(eno1, eno2);
            env.cluster
                .update_status::<NodeSpec>(svc01, &n.status)
                .await
                .unwrap();
        }
    };
    set(false, false).await;
    let hits = rig.hook.wait(3).await;
    let up = by_rule(&hits, "uplinks", "firing");
    assert_eq!(up["subject"]["port"], "bond0", "{up}");
    assert_eq!(up["severity"], "critical", "{up}");
    assert_eq!(
        up["detail"], "bond0 down on svc01 (eno1 down, eno2 down) — vmbr0, 1 network, 1 VM",
        "{up}"
    );
    settle().await;
    rig.step(RESOLVE_AFTER_MS + 1_000, 1.0);
    settle().await;
    let a = rig.alerts().await;
    assert!(
        a["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["clear_since"].is_null()),
        "{a}"
    );
    assert_eq!(rig.hook.hits().len(), 3, "{:?}", rig.hook.hits());
    // 경보 목록도 포트마다
    let a = rig.alerts().await;
    let mut ports: Vec<(String, String)> = a["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["rule_name"].as_str().unwrap_or_default().to_string(),
                a["subject"]["port"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    ports.sort();
    assert_eq!(ports.len(), 3, "{a}");

    // 모두 돌아오면 60초 뒤 두 경보가 풀린다(eno3는 그대로).
    set(true, true).await;
    rig.wait_alerts(|a| {
        a["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["clear_since"].is_i64())
            .count()
            == 2
    })
    .await;
    rig.step(RESOLVE_AFTER_MS + 1_000, 1.0);
    let hits = rig.hook.wait(5).await;
    for rule in ["uplinks", "bonds"] {
        by_rule(&hits, rule, "resolved");
    }
    settle().await;
    assert_eq!(rig.hook.hits().len(), 5, "{:?}", rig.hook.hits());
    let a = rig.alerts().await;
    assert_eq!(a["alerts"].as_array().unwrap().len(), 1, "{a}");
    assert_eq!(a["alerts"][0]["subject"]["port"], "eno3", "{a}");
    engine.shutdown().await;
}

/// 리뷰 1·2: 주소만 가진 NIC(스토리지·클러스터 링크망)도 기본 규칙이 본다. 노드가 기억하는 포트(`known_ports`)는 링크가
/// 끊겨 주소가 떨어져도, netdev가 사라져도 그 역할 그대로 장애다 — pending이 firing으로 이어지고 firing은 풀리지 않는다
/// (다시 나타나 살아 있으면 풀림). 오프라인 노드의 경보는 되풀이하지 않는다. 운영자가 잊으면(`forget-ports`) 풀린다.
#[tokio::test]
async fn link_alerts_hold_vanished_ports_and_stay_quiet_offline() {
    use orbivisor_types::node::{FEATURES, NodeInterface};
    use orbivisor_types::physical::KnownPort;

    // `addressed`: 주소가 붙어 있음(NM이 떼면 거짓)
    let storage = |present: bool, up: bool, addressed: bool| -> Vec<NodeInterface> {
        let st = if up { "up" } else { "down" };
        let mut list =
            vec![json!({ "name": "eno1", "kind": "ethernet", "state": "up", "carrier": true })];
        if present {
            let addresses: Vec<&str> = if addressed {
                vec!["10.10.0.11/24"]
            } else {
                vec![]
            };
            list.push(
                json!({ "name": "ens1f0", "kind": "ethernet", "state": st, "carrier": up,
                              "addresses": addresses }),
            );
        }
        serde_json::from_value(Value::Array(list)).unwrap()
    };
    let known = |missing: bool| -> Vec<KnownPort> {
        serde_json::from_value(
            json!([{ "name": "ens1f0", "kind": "ethernet", "role": "service",
            "addresses": ["10.10.0.11"], "missing": missing, "last_seen_ms": 1 }]),
        )
        .unwrap()
    };
    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    let svc01 = add_node(
        &rig.env,
        "svc01",
        NodeStatus {
            online: true,
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
            cluster_addresses: vec!["10.10.0.11:8007".into()],
            // 링크가 끊기자마자 주소가 떨어짐: 기억한 역할로 본다.
            interfaces: storage(true, false, false),
            known_ports: known(false),
            ..NodeStatus::default()
        },
    )
    .await;
    rig.rule(
        "uplinks",
        json!({"condition": {"kind": "node_link_down"}, "severity": "critical", "for_secs": 0,
               "repeat_secs": 60, "targets": ["ops"]}),
    )
    .await;
    let engine = rig.engine();
    let hits = rig.hook.wait(1).await;
    let first = hits[0].json();
    assert_eq!(first["subject"]["port"], "ens1f0", "{first}");
    assert_eq!(
        first["detail"], "ens1f0 down on svc01 — cluster link",
        "{first}"
    );

    let set = |f: &dyn Fn(&mut NodeStatus)| {
        let mut n = rig.env.cluster.get::<NodeSpec>(svc01).unwrap().unwrap();
        f(&mut n.status);
        n.status
    };
    // 사라짐(노드가 missing으로 기억): 풀리지 않고(해제 시작 없음) 그대로
    let status = set(&|s| {
        s.interfaces = storage(false, false, false);
        s.known_ports = known(true);
    });
    rig.env
        .cluster
        .update_status::<NodeSpec>(svc01, &status)
        .await
        .unwrap();
    settle().await;
    rig.step(RESOLVE_AFTER_MS + 1_000, 1.0);
    settle().await;
    let a = rig.alerts().await;
    assert_eq!(a["alerts"].as_array().unwrap().len(), 1, "{a}");
    assert!(a["alerts"][0]["clear_since"].is_null(), "{a}");
    // 되풀이 때가 지나면 "사라짐"으로 다시 알린다.
    let hits = rig.hook.wait(2).await;
    assert_eq!(
        hits[1].json()["detail"],
        "ens1f0 missing on svc01 — cluster link"
    );

    // 오프라인: 그대로 두되 되풀이하지 않는다.
    let status = set(&|s| s.online = false);
    rig.env
        .cluster
        .update_status::<NodeSpec>(svc01, &status)
        .await
        .unwrap();
    settle().await;
    rig.step(120_000, 1.0);
    settle().await;
    assert_eq!(rig.hook.hits().len(), 2, "{:?}", rig.hook.hits());
    assert_eq!(rig.alerts().await["alerts"].as_array().unwrap().len(), 1);

    // 다시 켜졌는데 여전히 사라져 있음: 운영자가 잊으면(NIC를 뺌) 풀린다.
    let status = set(&|s| s.online = true);
    rig.env
        .cluster
        .update_status::<NodeSpec>(svc01, &status)
        .await
        .unwrap();
    let admin = rig.env.admin().await;
    let (st, node) = admin
        .post("/nodes/svc01/forget-ports", json!({ "ports": ["ens1f0"] }))
        .await;
    assert_eq!(st, StatusCode::OK, "{node}");
    assert!(node["status"].get("known_ports").is_none(), "{node}");
    // 다시 잊으려 하면 기억에 없음
    let (st, body) = admin
        .post("/nodes/svc01/forget-ports", json!({ "ports": ["ens1f0"] }))
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
    rig.wait_alerts(|a| a["alerts"][0]["clear_since"].is_i64())
        .await;
    rig.step(RESOLVE_AFTER_MS + 1_000, 1.0);
    let hits = rig.hook.wait(3).await;
    assert_eq!(hits[2].json()["state"], "resolved", "{:?}", hits[2].json());
    engine.shutdown().await;
}

/// 리뷰 2: pending 중에 포트가 사라져도(PCIe 장애로 carrier가 먼저 떨어지고 netdev가 없어짐) 기억 포트라 `for_secs` 뒤
/// firing이 된다.
#[tokio::test]
async fn a_port_that_vanishes_while_pending_still_fires() {
    use orbivisor_types::node::{FEATURES, NodeInterface};
    use orbivisor_types::physical::KnownPort;

    let rig = Rig::start().await;
    rig.target("ops", "json").await;
    let list: Vec<NodeInterface> = serde_json::from_value(json!([
        { "name": "eno1", "kind": "ethernet", "state": "down", "carrier": false, "master": "vmbr0" },
        { "name": "vmbr0", "kind": "bridge", "state": "down", "carrier": false }
    ]))
    .unwrap();
    let known: Vec<KnownPort> =
        serde_json::from_value(json!([{ "name": "eno1", "kind": "ethernet",
        "role": "uplink", "bridges": ["vmbr0"], "last_seen_ms": 1 }]))
        .unwrap();
    let svc01 = add_node(
        &rig.env,
        "svc01",
        NodeStatus {
            online: true,
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
            interfaces: list,
            known_ports: known.clone(),
            ..NodeStatus::default()
        },
    )
    .await;
    rig.rule(
        "uplinks",
        json!({"condition": {"kind": "node_link_down"}, "severity": "critical", "for_secs": 30,
               "targets": ["ops"]}),
    )
    .await;
    let engine = rig.engine();
    rig.wait_alerts(|a| a["alerts"].as_array().is_some_and(|l| l.len() == 1))
        .await;
    let mut n = rig.env.cluster.get::<NodeSpec>(svc01).unwrap().unwrap();
    n.status.interfaces.retain(|i| i.name != "eno1");
    n.status.known_ports[0].missing = true;
    rig.env
        .cluster
        .update_status::<NodeSpec>(svc01, &n.status)
        .await
        .unwrap();
    settle().await;
    rig.step(31_000, 1.0);
    let hits = rig.hook.wait(1).await;
    let h = hits[0].json();
    assert_eq!(
        (h["state"].as_str(), h["detail"].as_str()),
        (Some("firing"), Some("eno1 missing on svc01 — vmbr0")),
        "{h}"
    );
    engine.shutdown().await;
}

/// 계획 76: DR 조건은 모든 노드가 `dr-plans`를 알 때만 받는다. 1단계에는 아무 노드도 알리지 않으므로 늘 409다.
#[tokio::test]
async fn dr_rules_wait_for_dr_plans() {
    let rig = Rig::start().await;
    withdraw_dr(&rig.env).await;
    let admin = rig.env.admin().await;
    rig.target("ops", "json").await;
    for condition in [
        json!({"kind": "dr_rpo_exceeded"}),
        json!({"kind": "dr_run_failed", "plans": ["web-tier"]}),
    ] {
        let got = admin
            .post(
                "/alert-rules",
                json!({"name": "dr", "spec": {"condition": condition, "severity": "warning", "targets": ["ops"]}}),
            )
            .await;
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    }
}

/// 링크 조건은 모든 노드가 `link-alerts`를 알 때만 받는다(409 `cluster_upgrading`). "링크 경보 켜기"(`link-defaults`)도
/// 그동안 409이고, SDN 묶음은 링크 규칙 없이 여덟 그대로다. 모두 알면 묶음이 둘을 만들고 다시 부르면 그대로 둔다.
#[tokio::test]
async fn link_rules_wait_for_every_node_to_know_link_alerts() {
    use orbivisor_types::node::{FEATURES, LINK_ALERTS};

    let rig = Rig::start().await;
    let admin = rig.env.admin().await;
    rig.target("ops", "json").await;
    add_node(
        &rig.env,
        "old",
        NodeStatus {
            online: true,
            features: FEATURES
                .iter()
                .filter(|f| **f != LINK_ALERTS)
                .map(|f| f.to_string())
                .collect(),
            ..NodeStatus::default()
        },
    )
    .await;
    for condition in [
        json!({"kind": "node_link_down"}),
        json!({"kind": "bond_degraded"}),
    ] {
        let got = admin
            .post(
                "/alert-rules",
                json!({"name": "link", "spec": {"condition": condition, "severity": "warning", "targets": ["ops"]}}),
            )
            .await;
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    }
    let got = admin
        .post("/alert-rules/link-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    // 다른 조건은 된다.
    rig.rule(
        "off",
        json!({"condition": {"kind": "node_offline"}, "severity": "critical", "targets": ["ops"]}),
    )
    .await;
    let names = |body: &serde_json::Value, key: &str| -> Vec<String> {
        body[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().or(r["name"].as_str()).unwrap().to_string())
            .collect()
    };
    let (status, body) = admin
        .post("/alert-rules/sdn-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let created = names(&body, "created");
    assert_eq!(created.len(), 8, "{body}");
    assert!(created.iter().all(|n| n.starts_with("sdn-")), "{body}");
    // 나쁜 글롭은 400
    let mut n = rig
        .env
        .cluster
        .get_by_name::<NodeSpec>("old")
        .unwrap()
        .unwrap();
    n.status.features = FEATURES.iter().map(|f| f.to_string()).collect();
    rig.env
        .cluster
        .update_status::<NodeSpec>(n.id, &n.status)
        .await
        .unwrap();
    let got = admin
        .post(
            "/alert-rules",
            json!({"name": "link", "spec": {"condition": {"kind": "node_link_down", "ports": "eno 1"},
                   "severity": "warning", "targets": ["ops"]}}),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    // SDN 묶음은 여전히 링크 규칙을 만들지 않는다.
    let (_, again) = admin
        .post("/alert-rules/sdn-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(again["created"], json!([]), "{again}");
    let (status, body) = admin
        .post("/alert-rules/link-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        names(&body, "created"),
        ["node-link-down", "bond-degraded"],
        "{body}"
    );
    let rule = &body["created"][0]["spec"];
    assert_eq!(rule["severity"], "critical", "{body}");
    assert_eq!(rule["for_secs"], 30, "{body}");
    assert_eq!(rule["condition"]["role"], "uplink", "{body}");
    // 멱등: 다시 부르면 만들지 않고 있던 것을 알린다.
    let (status, again) = admin
        .post("/alert-rules/link-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["created"], json!([]), "{again}");
    assert_eq!(
        names(&again, "existing"),
        ["node-link-down", "bond-degraded"],
        "{again}"
    );
    let missing = admin
        .post("/alert-rules/link-defaults", json!({"targets": ["nobody"]}))
        .await;
    assert_error(&missing, StatusCode::BAD_REQUEST, "invalid_request");
    // admin만 쓴다(SDN 묶음과 같은 권한).
    let operator = rig.env.user("u-operator", "operator").await;
    let got = operator
        .post("/alert-rules/link-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(got.0, StatusCode::FORBIDDEN);
}

// ---- DR 경보(계획 76 §3.4) ----

/// 이 노드가 `dr-plans`를 알린다(4단계가 `FEATURES`에 넣기 전까지 시험이 켠다).
async fn announce_dr(env: &Env) {
    let mut n = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    n.status
        .features
        .retain(|f| f != orbivisor_types::node::DR_PLANS);
    n.status
        .features
        .push(orbivisor_types::node::DR_PLANS.into());
    env.cluster
        .update_status::<NodeSpec>(n.id, &n.status)
        .await
        .unwrap();
}

/// 이 노드가 `dr-plans`를 알리지 않게 한다(옛 노드가 섞인 판 — 기능은 `FEATURES`에 있다).
async fn withdraw_dr(env: &Env) {
    let mut n = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    n.status
        .features
        .retain(|f| f != orbivisor_types::node::DR_PLANS);
    env.cluster
        .update_status::<NodeSpec>(n.id, &n.status)
        .await
        .unwrap();
}

/// 저장소에 바로 넣은 복구 계획(보호 상태 `protection`)
async fn dr_plan(env: &Env, name: &str, rpo: u64, protection: Value) -> Uuid {
    use orbivisor_types::dr::RecoveryPlanSpec;
    let spec: RecoveryPlanSpec = serde_json::from_value(json!({
        "source": "prod", "pool": "dr", "rpo_target_secs": rpo,
        "groups": [ { "name": "all", "vms": [ { "vm": "web3" }, { "vm": "db1" } ] } ]
    }))
    .unwrap();
    let mut plan: Resource<RecoveryPlanSpec, _> = Resource::new(name, spec);
    plan.status =
        serde_json::from_value(json!({ "protection": protection, "checked_at": 1 })).unwrap();
    env.cluster.create(&plan).await.unwrap();
    plan.id
}

async fn set_protection(env: &Env, plan: Uuid, protection: Value) {
    use orbivisor_types::dr::RecoveryPlanSpec;
    let status =
        serde_json::from_value(json!({ "protection": protection, "checked_at": 2 })).unwrap();
    env.cluster
        .update_status::<RecoveryPlanSpec>(plan, &status)
        .await
        .unwrap();
}

fn bad_protection() -> Value {
    json!([
        { "vm": "web3", "backup": "web3-a", "point_at": 1, "age_secs": 34 * 3600, "ok": false, "issue": "over-rpo" },
        { "vm": "db1", "ok": false, "issue": "no-recovery-point" }
    ])
}

fn good_protection() -> Value {
    json!([
        { "vm": "web3", "backup": "web3-b", "point_at": 1, "age_secs": 600, "ok": true },
        { "vm": "db1", "backup": "db1-b", "point_at": 1, "age_secs": 600, "ok": true }
    ])
}

/// `dr_rpo_exceeded`: 계획마다 경보 하나(문구는 설계 §3.4, 계획 보고 경로). RPO 목표가 0(끔)인 계획과 `plans`에 없는
/// 계획은 울리지 않는다. 모두 지키면 해제로 알린다.
#[tokio::test]
async fn dr_rpo_alerts_fire_per_plan_and_resolve() {
    let rig = Rig::start().await;
    announce_dr(&rig.env).await;
    rig.target("ops", "json").await;
    let web = dr_plan(&rig.env, "web-tier", 86400, bad_protection()).await;
    dr_plan(&rig.env, "off", 0, bad_protection()).await;
    dr_plan(&rig.env, "other", 86400, bad_protection()).await;
    rig.rule(
        "rpo",
        json!({"condition": {"kind": "dr_rpo_exceeded", "plans": ["web-tier", "off"]},
               "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    let engine = rig.engine();
    let hits = rig.hook.wait(1).await;
    let v = hits[0].json();
    assert_eq!(v["state"], "firing", "{v}");
    assert_eq!(v["rule"], "rpo");
    assert_eq!(v["subject"]["kind"], "recovery_plan");
    assert_eq!(v["subject"]["name"], "web-tier");
    assert_eq!(v["subject"]["id"], web.to_string());
    assert_eq!(
        v["detail"],
        "plan web-tier: 2 of 2 VMs over RPO 24h (web3 34h, db1 no recovery point)\nPlan: /api/v1/dr-plans/web-tier/report"
    );
    let a = rig.wait_alerts(|a| state_of(a) == ["firing"]).await;
    assert_eq!(a["alerts"][0]["subject"]["name"], "web-tier");
    settle().await;
    assert_eq!(rig.hook.hits().len(), 1);

    // 모두 지키면 60초 뒤 해제
    set_protection(&rig.env, web, good_protection()).await;
    settle().await;
    rig.clock.advance(RESOLVE_AFTER_MS + 1000);
    let hits = rig.hook.wait(2).await;
    assert_eq!(hits[1].json()["state"], "resolved", "{:?}", hits[1].json());
    rig.wait_alerts(|a| state_of(a).is_empty()).await;
    engine.shutdown().await;
}

/// 시험이 바로 넣는 끝난 DR 실행
async fn dr_run(env: &Env, plan: Uuid, plan_name: &str, name: &str, state: &str, finished_at: u64) {
    use orbivisor_types::dr::{DrRunSpec, RecoveryPlanSpec};
    let spec: RecoveryPlanSpec = serde_json::from_value(json!({
        "source": "prod", "pool": "dr",
        "groups": [ { "name": "all", "vms": [ { "vm": "web3" }, { "vm": "db1" } ] } ]
    }))
    .unwrap();
    let mut run: Resource<DrRunSpec, _> = Resource::new(
        name,
        DrRunSpec {
            plan,
            plan_name: plan_name.into(),
            kind: orbivisor_types::dr::DrRunKind::Test,
            plan_spec: spec,
            max_parallel: 4,
            cleanup: None,
        },
    );
    run.status = serde_json::from_value(json!({
        "state": state, "started_at": finished_at - 100, "finished_at": finished_at,
        "steps": [
            { "group": "all", "vm": "web3", "phase": "ready", "at": 1 },
            { "group": "all", "vm": "db1", "phase": "failed", "error": "restore failed: no space", "at": 1 }
        ]
    }))
    .unwrap();
    env.cluster.create(&run).await.unwrap();
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// `dr_run_failed`: `failed`·`partial`로 끝난 실행마다 한 번(이벤트, 계획·실행 경로). 규칙 전에 끝난 실행, 성공한 실행,
/// `plans`에 없는 계획의 실행은 알리지 않는다.
#[tokio::test]
async fn dr_run_failures_are_sent_once() {
    let rig = Rig::start().await;
    announce_dr(&rig.env).await;
    rig.target("ops", "json").await;
    let web = dr_plan(&rig.env, "web-tier", 86400, good_protection()).await;
    let other = dr_plan(&rig.env, "other", 86400, good_protection()).await;
    // 규칙보다 먼저 끝난 실행
    dr_run(
        &rig.env,
        web,
        "web-tier",
        "old",
        "failed",
        unix_now() - 3600,
    )
    .await;
    rig.rule(
        "runs",
        json!({"condition": {"kind": "dr_run_failed", "plans": ["web-tier"]},
               "severity": "warning", "targets": ["ops"]}),
    )
    .await;
    let engine = rig.engine();
    settle().await;
    let later = unix_now() + 5;
    dr_run(&rig.env, web, "web-tier", "ok", "succeeded", later).await;
    dr_run(&rig.env, other, "other", "theirs", "failed", later).await;
    let run_id = {
        dr_run(&rig.env, web, "web-tier", "t2", "partial", later).await;
        rig.env
            .cluster
            .get_by_name::<orbivisor_types::dr::DrRunSpec>("t2")
            .unwrap()
            .unwrap()
            .id
    };
    let hits = rig.hook.wait(1).await;
    let v = hits[0].json();
    assert_eq!(v["state"], "event", "{v}");
    assert_eq!(v["rule"], "runs");
    assert_eq!(v["severity"], "warning");
    assert_eq!(v["subject"]["kind"], "recovery_plan");
    assert_eq!(v["subject"]["id"], web.to_string());
    assert_eq!(
        v["detail"],
        format!(
            "DR test failover t2 of plan web-tier finished partially: 1 of 2 VMs failed\n- db1: restore failed: no space\nPlan: /api/v1/dr-plans/web-tier/report\nRun: /api/v1/dr-runs/{run_id}"
        )
    );
    settle().await;
    assert_eq!(rig.hook.hits().len(), 1, "{:?}", rig.hook.hits());
    engine.shutdown().await;
}

/// "DR 경보 켜기"(`dr-defaults`): 모든 노드가 `dr-plans`를 알아야 하고(409), 규칙 둘을 만들고 다시 부르면 그대로 둔다.
#[tokio::test]
async fn dr_default_rules_are_made_once() {
    let rig = Rig::start().await;
    withdraw_dr(&rig.env).await;
    let admin = rig.env.admin().await;
    let operator = rig.env.user("operator", "operator").await;
    rig.target("ops", "json").await;
    assert_error(
        &admin
            .post("/alert-rules/dr-defaults", json!({"targets": ["ops"]}))
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    announce_dr(&rig.env).await;
    assert_eq!(
        operator
            .post("/alert-rules/dr-defaults", json!({"targets": ["ops"]}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_error(
        &admin
            .post("/alert-rules/dr-defaults", json!({"targets": ["nope"]}))
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    let (status, body) = admin
        .post("/alert-rules/dr-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let created: Vec<&str> = body["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(created, ["dr-rpo-exceeded", "dr-run-failed"], "{body}");
    assert_eq!(body["created"][1]["spec"]["severity"], "warning");
    assert_eq!(
        body["created"][1]["spec"]["condition"],
        json!({"kind": "dr_run_failed"})
    );
    let (status, again) = admin
        .post("/alert-rules/dr-defaults", json!({"targets": ["ops"]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["created"], json!([]));
    assert_eq!(
        again["existing"],
        json!(["dr-rpo-exceeded", "dr-run-failed"])
    );
}
