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
use opl_api::{AlertBook, AlertEngine, AlertEngineConfig, AppState, SenderConfig};
use opl_driver::Hypervisor;
use opl_peer::{MetricsReply, MetricsSource};
use opl_types::alert::RESOLVE_AFTER_MS;
use opl_types::clock::{Clock, ManualClock};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries};
use opl_types::node::{NOTIFICATIONS, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::task::{TaskOp, TaskSpec, TaskState, TaskStatus};
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

impl Hook {
    async fn start() -> Self {
        let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
        let fail: Arc<AtomicUsize> = Arc::default();
        let (h, f) = (hits.clone(), fail.clone());
        let app = axum::Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
            let (h, f) = (h.clone(), f.clone());
            async move {
                if f.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
                {
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
    use opl_types::node::PoolState;
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
