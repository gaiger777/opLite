//! 알림 보내기(설계 §9): SMTP(`lettre`)와 webhook 다섯 형식(일반 JSON·Slack·Teams·ntfy·Gotify, `reqwest`).
//! 경보 엔진은 [`Sender`]의 대기열(노드마다 1000개)에 넣기만 하고 기다리지 않는다. 보내기는 동시에 4개까지(한 대상은
//! 1개까지), 한 번에 10초까지이고, 실패하면 30초·2분·10분 뒤 다시 한다. 대기열에서는 보낼 허가가 남았을 때만 꺼내므로
//! 보내기가 밀리면 대기열이 차고 넘친 것은 버린다. 바쁜 대상의 일은 대상마다 줄을 서고, 줄 전체가 대기열 크기를 넘으면
//! 가장 긴 줄의 가장 오래된 것을 버린다(응답 없는 대상 하나가 다른 대상의 알림을 늦추지 않게). 줄은 돌아가며 꺼낸다.
//! 대상마다 분당 20개를 넘는 알림은 보내지 않고 모았다가 다음 분에 "N건 더" 알림 하나로 알린다. 한 대상의 줄도 20개까지이고
//! 넘친 것은 "N건 더"에 넣는다. 처음 넣은 뒤 15분 안에 보내지 못한 알림과, 같은 경보의 `RESOLVED`를 받은 뒤의 `FIRING`
//! 다시 하기는 버린다(`dropped`). 결과는 경보 장부의 보낸 기록에 남긴다. 오류 문구에는 비밀(비밀번호·토큰·헤더 값·URL의
//! 경로와 질의)을 넣지 않는다.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use orbivisor_cluster::Cluster;
use orbivisor_types::alert::{
    AlertSubject, NotificationRecord, NotificationState, NotificationTargetSpec, Severity,
    SmtpTarget, SmtpTls, TargetConfig, WebhookFormat, WebhookTarget, redact_url,
};
use orbivisor_types::clock::Clock;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use super::book::AlertBook;

/// 보낼 알림 하나
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Notification {
    /// 클러스터 이름
    pub cluster: String,
    pub state: NotificationState,
    pub severity: Severity,
    /// 규칙 이름(시험 보내기·"N건 더"는 없음)
    pub rule: Option<String>,
    /// 조건 종류(`node_metric` …)
    pub condition: Option<String>,
    pub subject: Option<AlertSubject>,
    pub value: Option<f64>,
    pub threshold: Option<f64>,
    /// 조건이 참이 된 때(ms)
    pub since: Option<i64>,
    /// 알린 때(ms)
    pub at: i64,
    /// 더 알릴 것(Task 오류, 시험 보내기 설명 …)
    pub detail: Option<String>,
}

impl Notification {
    fn word(&self) -> &'static str {
        match self.state {
            NotificationState::Firing => "FIRING",
            NotificationState::Resolved => "RESOLVED",
            NotificationState::Event => "EVENT",
            NotificationState::Test => "TEST",
        }
    }

    /// `[Orbivisor {클러스터}] FIRING critical: {규칙} on {대상}`
    pub fn title(&self) -> String {
        let head = format!("[Orbivisor {}] {}", self.cluster, self.word());
        match (&self.rule, &self.subject) {
            (Some(rule), Some(s)) => format!("{head} {}: {rule} on {}", self.severity, s.label()),
            (Some(rule), None) => format!("{head} {}: {rule}", self.severity),
            _ => match &self.detail {
                Some(d) => format!("{head}: {d}"),
                None => head,
            },
        }
    }

    /// 일반 텍스트 본문(제목 아래 줄들)
    pub fn text(&self) -> String {
        let mut lines = Vec::new();
        if let Some(rule) = &self.rule {
            match &self.condition {
                Some(c) => lines.push(format!("Rule: {rule} ({c})")),
                None => lines.push(format!("Rule: {rule}")),
            }
            lines.push(format!("Severity: {}", self.severity));
        }
        if let Some(s) = &self.subject {
            let mut line = format!("Subject: {} {}", s.kind.as_str(), s.name);
            if let Some(node) = &s.node
                && *node != s.name
            {
                line.push_str(&format!(" on node {node}"));
            }
            if let Some(port) = &s.port {
                line.push_str(&format!(" port {port}"));
            }
            lines.push(line);
        }
        match (self.value, self.threshold) {
            (Some(v), Some(t)) => {
                lines.push(format!("Value: {} (threshold {})", number(v), number(t)))
            }
            (Some(v), None) => lines.push(format!("Value: {}", number(v))),
            _ => {}
        }
        if let Some(since) = self.since {
            lines.push(format!("Since: {}", rfc3339(since)));
        }
        lines.push(format!("At: {}", rfc3339(self.at)));
        if let Some(d) = &self.detail {
            lines.push(d.clone());
        }
        lines.join("\n")
    }
}

/// 소수점 둘째 자리까지, 정수면 소수점 없이
fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

fn rfc3339(ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| ms.to_string())
}

/// webhook 요청 하나(시험이 형식을 본다)
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WebhookRequest {
    pub content_type: &'static str,
    /// 형식이 붙이는 헤더와 대상의 헤더
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// ntfy 우선순위(1~5)
fn ntfy_priority(n: &Notification) -> u8 {
    match (n.state, n.severity) {
        (NotificationState::Firing | NotificationState::Event, Severity::Critical) => 5,
        (NotificationState::Firing | NotificationState::Event, Severity::Warning) => 4,
        _ => 3,
    }
}

/// Gotify 우선순위(0~10)
fn gotify_priority(n: &Notification) -> u8 {
    match (n.state, n.severity) {
        (NotificationState::Firing | NotificationState::Event, Severity::Critical) => 8,
        (NotificationState::Firing | NotificationState::Event, Severity::Warning) => 5,
        _ => 2,
    }
}

/// ntfy 태그(이모지 이름)
fn ntfy_tags(n: &Notification) -> &'static str {
    match (n.state, n.severity) {
        (NotificationState::Resolved, _) => "white_check_mark",
        (NotificationState::Test, _) => "test_tube",
        (_, Severity::Critical) => "rotating_light",
        (_, Severity::Warning) => "warning",
        (_, Severity::Info) => "information_source",
    }
}

/// 형식에 맞는 본문과 헤더
pub(crate) fn webhook_request(w: &WebhookTarget, n: &Notification) -> WebhookRequest {
    let title = n.title();
    let text = n.text();
    let mut headers: Vec<(String, String)> = Vec::new();
    let (content_type, body) = match w.format {
        WebhookFormat::Json => {
            let subject = n.subject.as_ref().map(|s| {
                let mut v =
                    json!({ "kind": s.kind.as_str(), "id": s.id, "name": s.name, "node": s.node });
                // 링크 경보(계획 75)만 포트를 단다(다른 경보의 본문은 그대로).
                if let Some(port) = &s.port {
                    v["port"] = json!(port);
                }
                v
            });
            let body = json!({
                "version": 1,
                "cluster": n.cluster,
                "state": state_str(n.state),
                "severity": n.severity.as_str(),
                "rule": n.rule,
                "subject": subject,
                "value": n.value,
                "threshold": n.threshold,
                "since": n.since,
                "at": n.at,
                "title": title,
                "detail": n.detail,
            });
            ("application/json", body.to_string().into_bytes())
        }
        WebhookFormat::Slack => (
            "application/json",
            json!({ "text": format!("*{title}*\n{text}") })
                .to_string()
                .into_bytes(),
        ),
        WebhookFormat::Teams => {
            let card = json!({
                "type": "message",
                "attachments": [{
                    "contentType": "application/vnd.microsoft.card.adaptive",
                    "contentUrl": Value::Null,
                    "content": {
                        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                        "type": "AdaptiveCard",
                        "version": "1.4",
                        "body": [
                            { "type": "TextBlock", "text": title, "weight": "Bolder", "size": "Medium", "wrap": true },
                            { "type": "TextBlock", "text": text, "wrap": true },
                        ],
                    },
                }],
            });
            ("application/json", card.to_string().into_bytes())
        }
        WebhookFormat::Ntfy => {
            headers.push(("Title".into(), title));
            headers.push(("Priority".into(), ntfy_priority(n).to_string()));
            headers.push(("Tags".into(), ntfy_tags(n).into()));
            if let Some(token) = &w.token {
                headers.push(("Authorization".into(), format!("Bearer {token}")));
            }
            ("text/plain; charset=utf-8", text.into_bytes())
        }
        WebhookFormat::Gotify => {
            if let Some(token) = &w.token {
                headers.push(("X-Gotify-Key".into(), token.clone()));
            }
            let body = json!({ "title": title, "message": text, "priority": gotify_priority(n) });
            ("application/json", body.to_string().into_bytes())
        }
    };
    headers.extend(w.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
    WebhookRequest {
        content_type,
        headers,
        body,
    }
}

fn state_str(s: NotificationState) -> &'static str {
    match s {
        NotificationState::Firing => "firing",
        NotificationState::Resolved => "resolved",
        NotificationState::Event => "event",
        NotificationState::Test => "test",
    }
}

/// webhook으로 보낸다. 2xx가 아니면 실패다. 오류 문구에는 줄인 URL만 넣는다.
async fn send_webhook(
    w: &WebhookTarget,
    n: &Notification,
    timeout: Duration,
) -> Result<(), String> {
    let shown = redact_url(&w.url);
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    if let Some(pem) = &w.ca_pem {
        let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
            .map_err(|_| "ca_pem holds no usable certificate".to_string())?;
        builder = builder.tls_certs_only(certs);
    }
    let client = builder
        .build()
        .map_err(|e| crate::telemetry::chain(&e.without_url()))?;
    let req = webhook_request(w, n);
    let mut request = client
        .post(w.url.trim())
        .header(reqwest::header::CONTENT_TYPE, req.content_type)
        .body(req.body);
    for (name, value) in &req.headers {
        // 제목에 헤더에 쓸 수 없는 글자가 있으면 그 헤더만 뺀다.
        let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) else {
            continue;
        };
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("{shown}: {}", crate::telemetry::chain(&e.without_url())))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{shown}: HTTP {status}"));
    }
    Ok(())
}

/// 메일 한 통(일반 텍스트)
pub(crate) fn smtp_message(s: &SmtpTarget, n: &Notification) -> Result<lettre::Message, String> {
    use lettre::message::Mailbox;
    use lettre::message::header::ContentType;
    let from: Mailbox = s
        .from
        .parse()
        .map_err(|_| format!("invalid from address {:?}", s.from))?;
    let mut builder = lettre::Message::builder()
        .from(from)
        .subject(n.title())
        .header(ContentType::TEXT_PLAIN);
    for to in &s.to {
        let to: Mailbox = to
            .parse()
            .map_err(|_| format!("invalid recipient {to:?}"))?;
        builder = builder.to(to);
    }
    builder
        .body(format!("{}\n\n{}\n", n.title(), n.text()))
        .map_err(|e| e.to_string())
}

/// SMTP로 보낸다. 오류 문구에 비밀번호는 들어가지 않는다(서버 응답과 연결 오류뿐).
async fn send_mail(s: &SmtpTarget, n: &Notification, timeout: Duration) -> Result<(), String> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::transport::smtp::client::{Tls, TlsParameters};
    use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
    let message = smtp_message(s, n)?;
    let params = || TlsParameters::new(s.host.clone()).map_err(|e| crate::telemetry::chain(&e));
    let tls = match s.tls {
        SmtpTls::None => Tls::None,
        SmtpTls::Starttls => Tls::Required(params()?),
        SmtpTls::Tls => Tls::Wrapper(params()?),
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(s.host.clone())
        .port(s.port)
        .tls(tls)
        .timeout(Some(timeout));
    if let Some(user) = &s.user {
        builder = builder.credentials(Credentials::new(
            user.clone(),
            s.password.clone().unwrap_or_default(),
        ));
    }
    let transport = builder.build();
    let where_ = format!("smtp {}:{}", s.host, s.port);
    match tokio::time::timeout(timeout, transport.send(message)).await {
        Err(_) => Err(format!("{where_}: timed out")),
        Ok(Err(e)) => Err(format!("{where_}: {}", crate::telemetry::chain(&e))),
        Ok(Ok(_)) => Ok(()),
    }
}

/// 대상 하나로 한 번 보낸다(다시 하지 않는다). 시험 보내기와 [`Sender`]가 쓴다.
pub(crate) async fn deliver(
    target: &TargetConfig,
    n: &Notification,
    timeout: Duration,
) -> Result<(), String> {
    match target {
        TargetConfig::Smtp(s) => send_mail(s, n, timeout).await,
        TargetConfig::Webhook(w) => send_webhook(w, n, timeout).await,
    }
}

/// 보낸 기록 한 줄
pub(crate) fn record_of(
    n: &Notification,
    target: &str,
    at: i64,
    result: Result<(), String>,
) -> NotificationRecord {
    NotificationRecord {
        at,
        // 장부가 이 노드 이름으로 채운다.
        node: String::new(),
        rule: n.rule.clone(),
        subject: n.subject.as_ref().map(|s| s.label()),
        state: n.state,
        target: target.into(),
        ok: result.is_ok(),
        error: result.err(),
    }
}

/// 보내기 설정
#[derive(Debug, Clone)]
pub struct SenderConfig {
    /// 동시에 보내는 수
    pub concurrency: usize,
    /// 한 번 보내기의 제한 시간
    pub timeout: Duration,
    /// 실패한 뒤 다시 하기까지 기다리는 시간(이 수만큼 다시 한다)
    pub retries: Vec<Duration>,
    /// 대상마다 `window`에 보내는 최대 수
    pub per_window: u32,
    pub window: Duration,
    /// 대기열 크기. 넘치면 버리고 기록에 남긴다. 바쁜 대상의 줄(다시 하기를 기다린 것 포함)도 모두 합쳐 이만큼이다.
    pub queue: usize,
    /// 한 대상에 동시에 보내는 수(응답 없는 대상이 허가를 다 쓰지 못하게)
    pub per_target: usize,
    /// 처음 넣은 뒤 이만큼 지나도록 보내지 못한 일은 버리고 `dropped`로 기록한다(살아난 대상에 묵은 알림을 몰아
    /// 보내지 않게).
    pub max_age: Duration,
}

impl Default for SenderConfig {
    fn default() -> Self {
        Self {
            concurrency: 4,
            timeout: Duration::from_secs(10),
            retries: vec![
                Duration::from_secs(30),
                Duration::from_secs(120),
                Duration::from_secs(600),
            ],
            per_window: 20,
            window: Duration::from_secs(60),
            queue: 1000,
            per_target: 1,
            max_age: Duration::from_secs(15 * 60),
        }
    }
}

/// 대기열의 일 하나
#[derive(Debug, Clone)]
struct Job {
    /// 알림 대상 이름
    target: String,
    n: Arc<Notification>,
    /// 몇 번째 다시 하기인지(0 = 처음)
    attempt: usize,
    /// "N건 더" 알림(분당 상한을 세지 않는다)
    summary: bool,
    /// 처음 대기열에 넣은 때
    born: Instant,
}

impl Job {
    fn new(target: String, n: Arc<Notification>, summary: bool) -> Self {
        Self {
            target,
            n,
            attempt: 0,
            summary,
            born: Instant::now(),
        }
    }

    /// 같은 경보의 같은 대상 알림을 가리는 열쇠: (대상, 규칙, 경보 대상 id, 포트). 규칙 경보가 아니면 None이다.
    fn alert(&self) -> Option<AlertKey> {
        let (rule, subject) = (self.n.rule.as_ref()?, self.n.subject.as_ref()?);
        Some((
            self.target.clone(),
            rule.clone(),
            subject.id,
            subject.port.clone(),
        ))
    }

    /// 다시 하기를 기다리는 `FIRING`인데 그 뒤에 같은 경보의 `RESOLVED`를 받았다(리뷰 2 #5).
    fn superseded(&self, resolved: &HashMap<AlertKey, Instant>) -> bool {
        self.attempt > 0
            && self.n.state == NotificationState::Firing
            && (self.alert()).is_some_and(|k| resolved.get(&k).is_some_and(|at| *at >= self.born))
    }
}

/// (대상, 규칙 이름, 경보 대상 id, 링크 경보의 포트)
type AlertKey = (String, String, uuid::Uuid, Option<String>);

/// 대상 하나의 분당 창
struct Window {
    start: Instant,
    sent: u32,
    /// 상한을 넘어 보내지 않은 수와 그 가운데 가장 높은 심각도
    held: u32,
    worst: Severity,
    /// 마지막으로 본 클러스터 이름("N건 더" 알림에 씀)
    cluster: String,
}

/// 노드의 보내기 대기열과 그것을 비우는 작업
pub(crate) struct Sender {
    tx: mpsc::Sender<Job>,
    book: AlertBook,
    clock: Arc<dyn Clock>,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Sender {
    pub fn spawn(
        cluster: Arc<Cluster>,
        book: AlertBook,
        clock: Arc<dyn Clock>,
        config: SenderConfig,
    ) -> Self {
        let (tx, rx) = mpsc::channel(config.queue.max(1));
        let (shutdown, stop) = oneshot::channel();
        let ctx = Arc::new(Ctx {
            cluster,
            book: book.clone(),
            clock: clock.clone(),
            config,
        });
        Self {
            tx,
            book,
            clock,
            shutdown,
            task: tokio::spawn(run(ctx, rx, stop)),
        }
    }

    /// 대기열에 넣는다(기다리지 않음). 가득 찼으면 버리고 기록에 `dropped`로 남긴다.
    pub fn enqueue(&self, target: &str, n: Arc<Notification>) {
        let job = Job::new(target.into(), n, false);
        if let Err(e) = self.tx.try_send(job) {
            let job = match e {
                mpsc::error::TrySendError::Full(j) | mpsc::error::TrySendError::Closed(j) => j,
            };
            dropped(&self.book, self.clock.as_ref(), &job, DropReason::Full);
        }
    }

    /// 대기열을 비우는 작업을 멈춘다(보내던 것과 다시 하려던 것은 버린다).
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

struct Ctx {
    cluster: Arc<Cluster>,
    book: AlertBook,
    clock: Arc<dyn Clock>,
    config: SenderConfig,
}

/// 알림을 버린 까닭
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropReason {
    /// 대기열이나 줄이 넘쳤다.
    Full,
    /// `max_age` 안에 보내지 못했다.
    Stale,
    /// 다시 하기를 기다리는 `FIRING`인데 같은 경보의 `RESOLVED`가 왔다.
    Resolved,
}

/// 버린 알림을 경고하고 기록에 `dropped: …`로 남긴다.
fn dropped(book: &AlertBook, clock: &dyn Clock, job: &Job, why: DropReason) {
    let error = match why {
        DropReason::Full => {
            tracing::warn!(target = %job.target, "the notification queue is full; dropping a notification");
            "dropped: the send queue is full"
        }
        DropReason::Stale => {
            tracing::warn!(target = %job.target, "a notification waited too long to be sent; dropping it");
            "dropped: not sent in time"
        }
        DropReason::Resolved => {
            tracing::info!(target = %job.target, "the alert was resolved before a retry; dropping the FIRING notification");
            "dropped: the alert was resolved before the retry"
        }
    };
    let rec = record_of(&job.n, &job.target, clock.now_ms(), Err(error.into()));
    let book = book.clone();
    tokio::task::spawn_blocking(move || book.record(rec));
}

/// 보내기 작업이 끝난 뒤 할 일
enum Next {
    Done,
    /// 기다린 뒤 다시 한다.
    Retry(Job),
    /// 기다림이 끝났다.
    Again(Job),
}

/// 보낼 차례를 기다리는 일들: 대상마다 줄과 그 대상의 동시 보내기 몫. 줄은 돌아가며 꺼낸다.
#[derive(Default)]
struct Lanes {
    lanes: Vec<Lane>,
    /// 다음에 먼저 볼 줄(리뷰 2 #4: 앞 줄이 늘 이기지 않게)
    turn: usize,
    /// 모든 줄의 일 수
    waiting: usize,
}

struct Lane {
    target: String,
    jobs: VecDeque<Job>,
    /// 한 대상의 동시 보내기 몫(`per_target`개). 보내는 작업이 끝나면 돌려준다.
    slots: Arc<Semaphore>,
}

/// 줄에 세우다 넘친 일
#[derive(Debug)]
enum Overflow {
    /// 그 대상의 줄이 `per_lane`을 넘어 가장 오래된 일을 "N건 더"로 접는다.
    Fold(Job),
    /// 줄 전체가 `cap`을 넘어 가장 긴 줄의 가장 오래된 일을 버린다.
    Shed(Job),
}

impl Lanes {
    /// 일을 대상의 줄 끝에 세운다. 그 대상의 줄("N건 더"는 빼고 셈)이 `per_lane`을 넘으면 가장 오래된 일을 접어
    /// 돌려주고, 줄 전체가 `cap`을 넘으면 가장 긴 줄의 가장 오래된 일을 버려 돌려준다.
    fn push(
        &mut self,
        job: Job,
        per_target: usize,
        per_lane: usize,
        cap: usize,
    ) -> Option<Overflow> {
        let i = match self.lanes.iter().position(|l| l.target == job.target) {
            Some(i) => i,
            None => {
                self.lanes.push(Lane {
                    target: job.target.clone(),
                    jobs: VecDeque::new(),
                    slots: Arc::new(Semaphore::new(per_target.max(1))),
                });
                self.lanes.len() - 1
            }
        };
        let lane = &mut self.lanes[i];
        lane.jobs.push_back(job);
        if lane.jobs.iter().filter(|j| !j.summary).count() > per_lane.max(1)
            && let Some(k) = lane.jobs.iter().position(|j| !j.summary)
        {
            return lane.jobs.remove(k).map(Overflow::Fold);
        }
        self.waiting += 1;
        if self.waiting <= cap.max(1) {
            return None;
        }
        let longest = self.lanes.iter_mut().max_by_key(|l| l.jobs.len())?;
        self.waiting -= 1;
        longest.jobs.pop_front().map(Overflow::Shed)
    }

    /// 처음 넣은 뒤 `max_age`가 지난 일과 `RESOLVED`에 가려진 다시 하기를 줄에서 빼 돌려준다.
    fn expire(
        &mut self,
        now: Instant,
        max_age: Duration,
        resolved: &HashMap<AlertKey, Instant>,
    ) -> Vec<(Job, DropReason)> {
        let mut out = Vec::new();
        for lane in &mut self.lanes {
            let mut keep = VecDeque::with_capacity(lane.jobs.len());
            for job in lane.jobs.drain(..) {
                if now.duration_since(job.born) >= max_age {
                    out.push((job, DropReason::Stale));
                } else if job.superseded(resolved) {
                    out.push((job, DropReason::Resolved));
                } else {
                    keep.push_back(job);
                }
            }
            lane.jobs = keep;
        }
        self.waiting -= out.len();
        out
    }

    /// 대상 몫과 보낼 허가가 둘 다 있는 일 하나. 허가가 없으면 `None`이다. 지난번에 꺼낸 줄 다음 줄부터 보고, 빈 줄은
    /// 정리한다.
    fn next(
        &mut self,
        permits: &Arc<Semaphore>,
    ) -> Option<(Job, OwnedSemaphorePermit, OwnedSemaphorePermit)> {
        // 기다리는 일도 보내는 중인 일(몫을 쥔 작업)도 없는 줄
        self.lanes
            .retain(|l| !l.jobs.is_empty() || Arc::strong_count(&l.slots) > 1);
        let n = self.lanes.len();
        for k in 0..n {
            let i = (self.turn + k) % n;
            let lane = &mut self.lanes[i];
            if lane.jobs.is_empty() {
                continue;
            }
            let Ok(slot) = lane.slots.clone().try_acquire_owned() else {
                continue;
            };
            let permit = permits.clone().try_acquire_owned().ok()?;
            let job = lane.jobs.pop_front()?;
            self.waiting -= 1;
            self.turn = i + 1;
            return Some((job, permit, slot));
        }
        None
    }
}

async fn run(ctx: Arc<Ctx>, mut rx: mpsc::Receiver<Job>, mut stop: oneshot::Receiver<()>) {
    let permits = Arc::new(Semaphore::new(ctx.config.concurrency.max(1)));
    let mut windows: HashMap<String, Window> = HashMap::new();
    let mut lanes = Lanes::default();
    let mut work: JoinSet<Next> = JoinSet::new();
    // 받은 `RESOLVED`(경보마다 마지막으로 받은 때): 그 전에 넣은 `FIRING`의 다시 하기는 버린다.
    let mut resolved: HashMap<AlertKey, Instant> = HashMap::new();
    let mut flush = tokio::time::interval(Duration::from_secs(1).min(ctx.config.window));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let config = &ctx.config;
    let (per_target, per_lane, cap) = (
        config.per_target,
        usize::try_from(config.per_window).unwrap_or(usize::MAX),
        config.queue,
    );
    let drop_job = |job: &Job, why: DropReason| dropped(&ctx.book, ctx.clock.as_ref(), job, why);
    loop {
        // 대상 몫과 허가가 있는 만큼 보낸다.
        while let Some((job, permit, slot)) = lanes.next(&permits) {
            work.spawn(attempt(ctx.clone(), job, permit, slot));
        }
        let job = tokio::select! {
            _ = &mut stop => break,
            // 허가가 남았을 때만 대기열에서 꺼낸다(보내기가 밀리면 대기열이 차서 넘친 것을 버린다).
            job = rx.recv(), if permits.available_permits() > 0 => match job {
                Some(job) => {
                    if job.n.state == NotificationState::Resolved
                        && let Some(key) = job.alert()
                    {
                        resolved.insert(key, job.born);
                        for (old, why) in lanes.expire(Instant::now(), config.max_age, &resolved) {
                            drop_job(&old, why);
                        }
                    }
                    if !job.summary && !admit(&mut windows, &job, config) {
                        continue;
                    }
                    job
                }
                None => break,
            },
            Some(done) = work.join_next(), if !work.is_empty() => match done {
                Ok(Next::Done) | Err(_) => continue,
                Ok(Next::Retry(job)) if job.superseded(&resolved) => {
                    drop_job(&job, DropReason::Resolved);
                    continue;
                }
                Ok(Next::Retry(job)) => {
                    let wait = config.retries[job.attempt - 1];
                    work.spawn(async move {
                        tokio::time::sleep(wait).await;
                        Next::Again(job)
                    });
                    continue;
                }
                Ok(Next::Again(job)) if job.superseded(&resolved) => {
                    drop_job(&job, DropReason::Resolved);
                    continue;
                }
                Ok(Next::Again(job)) => job,
            },
            _ = flush.tick() => {
                let now = Instant::now();
                for (old, why) in lanes.expire(now, config.max_age, &resolved) {
                    drop_job(&old, why);
                }
                // 이보다 오래된 `FIRING`은 줄에서 이미 버린다.
                resolved.retain(|_, at| now.duration_since(*at) < config.max_age);
                for (target, n) in held_over(&mut windows, config.window, ctx.clock.now_ms()) {
                    let job = Job::new(target, Arc::new(n), true);
                    queue(&mut lanes, &mut windows, job, (per_target, per_lane, cap), &drop_job);
                }
                continue;
            }
        };
        queue(
            &mut lanes,
            &mut windows,
            job,
            (per_target, per_lane, cap),
            &drop_job,
        );
    }
    // 남은 보내기·기다림은 `work`를 버리며 끊는다.
}

/// 일을 줄에 세운다. 대상 줄이 넘쳐 접은 일은 "N건 더"로 세고, 줄 전체가 넘쳐 버린 일은 `dropped`로 기록한다.
fn queue(
    lanes: &mut Lanes,
    windows: &mut HashMap<String, Window>,
    job: Job,
    (per_target, per_lane, cap): (usize, usize, usize),
    drop_job: &impl Fn(&Job, DropReason),
) {
    match lanes.push(job, per_target, per_lane, cap) {
        None => {}
        Some(Overflow::Fold(old)) => hold(windows, &old),
        Some(Overflow::Shed(old)) => drop_job(&old, DropReason::Full),
    }
}

/// 대상의 분당 창에 넣을 수 있으면 센다. 넘으면 모아 둔다.
fn admit(windows: &mut HashMap<String, Window>, job: &Job, config: &SenderConfig) -> bool {
    let now = Instant::now();
    let w = windows.entry(job.target.clone()).or_insert_with(|| Window {
        start: now,
        sent: 0,
        held: 0,
        worst: Severity::Info,
        cluster: String::new(),
    });
    if now.duration_since(w.start) >= config.window && w.held == 0 {
        w.start = now;
        w.sent = 0;
    }
    w.cluster = job.n.cluster.clone();
    if w.sent < config.per_window {
        w.sent += 1;
        return true;
    }
    w.held += 1;
    w.worst = w.worst.max(job.n.severity);
    false
}

/// 보내지 않고 모아 둔 것으로 센다(다음 "N건 더" 알림에 들어간다).
fn hold(windows: &mut HashMap<String, Window>, job: &Job) {
    let w = windows.entry(job.target.clone()).or_insert_with(|| Window {
        start: Instant::now(),
        sent: 0,
        held: 0,
        worst: Severity::Info,
        cluster: String::new(),
    });
    w.cluster = job.n.cluster.clone();
    w.held += 1;
    w.worst = w.worst.max(job.n.severity);
}

/// 창이 끝난 대상 가운데 모아 둔 것이 있는 대상의 "N건 더" 알림. 그 창은 새로 시작한다.
fn held_over(
    windows: &mut HashMap<String, Window>,
    window: Duration,
    at: i64,
) -> Vec<(String, Notification)> {
    let now = Instant::now();
    let mut out = Vec::new();
    windows.retain(|target, w| {
        if now.duration_since(w.start) < window {
            return true;
        }
        if w.held > 0 {
            out.push((
                target.clone(),
                Notification {
                    cluster: w.cluster.clone(),
                    state: NotificationState::Firing,
                    severity: w.worst,
                    rule: None,
                    condition: None,
                    subject: None,
                    value: None,
                    threshold: None,
                    since: None,
                    at,
                    detail: Some(format!(
                        "{} more notifications were held back by the rate limit of {target}",
                        w.held
                    )),
                },
            ));
        }
        // 다음 창은 다음 알림이 열고, 비어 있는 창은 버린다.
        false
    });
    out
}

/// 한 번 보낸다(보낼 허가와 대상 몫을 쥐고, 끝나면 돌려준다). 대상을 Raft에서 읽고(없으면 기록하고 끝, 꺼졌으면
/// 조용히 끝), 실패하면 남은 다시 하기가 있으면 [`Next::Retry`], 없으면 실패를 기록한다.
async fn attempt(
    ctx: Arc<Ctx>,
    job: Job,
    _permit: OwnedSemaphorePermit,
    _slot: OwnedSemaphorePermit,
) -> Next {
    let result = match ctx
        .cluster
        .get_by_name::<NotificationTargetSpec>(&job.target)
    {
        Ok(Some(t)) if !t.spec.enabled => return Next::Done,
        Ok(Some(t)) => deliver(&t.spec.target, &job.n, ctx.config.timeout).await,
        Ok(None) => {
            let e = format!("notification target {:?} not found", job.target);
            finish(&ctx, &job, Err(e)).await;
            return Next::Done;
        }
        Err(e) => Err(format!("cannot read the notification target: {e}")),
    };
    match result {
        Ok(()) => {
            finish(&ctx, &job, Ok(())).await;
            Next::Done
        }
        Err(e) if job.attempt < ctx.config.retries.len() => {
            tracing::info!(target = %job.target, error = %e, attempt = job.attempt + 1, "sending a notification failed; trying again later");
            Next::Retry(Job {
                attempt: job.attempt + 1,
                ..job
            })
        }
        Err(e) => {
            let tries = job.attempt + 1;
            tracing::warn!(target = %job.target, error = %e, "sending a notification failed");
            let e = if tries > 1 {
                format!("{e} (after {tries} attempts)")
            } else {
                e
            };
            finish(&ctx, &job, Err(e)).await;
            Next::Done
        }
    }
}

async fn finish(ctx: &Ctx, job: &Job, result: Result<(), String>) {
    let rec = record_of(&job.n, &job.target, ctx.clock.now_ms(), result);
    let book = ctx.book.clone();
    let _ = tokio::task::spawn_blocking(move || book.record(rec)).await;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use orbivisor_types::alert::SubjectKind;
    use uuid::Uuid;

    use super::*;

    fn firing() -> Notification {
        Notification {
            cluster: "lab".into(),
            state: NotificationState::Firing,
            severity: Severity::Critical,
            rule: Some("cpu-hot".into()),
            condition: Some("node_metric".into()),
            subject: Some(AlertSubject {
                kind: SubjectKind::Vm,
                id: Uuid::from_u128(7),
                name: "web1".into(),
                node: Some("node1".into()),
                port: None,
            }),
            value: Some(97.456),
            threshold: Some(90.0),
            since: Some(1_700_000_000_000),
            at: 1_700_000_060_000,
            detail: None,
        }
    }

    fn webhook(format: WebhookFormat) -> WebhookTarget {
        WebhookTarget {
            url: "https://hooks.example.com/services/T000/B000/SECRETPATH".into(),
            format,
            headers: BTreeMap::from([("X-Extra".into(), "extra-secret".into())]),
            token: format.uses_token().then(|| "tok-secret".into()),
            ca_pem: None,
        }
    }

    fn body(req: &WebhookRequest) -> Value {
        serde_json::from_slice(&req.body).unwrap()
    }

    #[test]
    fn titles_and_text_name_the_rule_and_subject() {
        let n = firing();
        assert_eq!(
            n.title(),
            "[Orbivisor lab] FIRING critical: cpu-hot on web1"
        );
        let text = n.text();
        assert!(text.contains("Rule: cpu-hot (node_metric)"), "{text}");
        assert!(text.contains("Subject: vm web1 on node node1"), "{text}");
        assert!(text.contains("Value: 97.46 (threshold 90)"), "{text}");
        assert!(text.contains("Since: 2023-11-14T22:13:20Z"), "{text}");
        let resolved = Notification {
            state: NotificationState::Resolved,
            ..firing()
        };
        assert!(resolved.title().contains("RESOLVED critical"));
        let test = Notification {
            state: NotificationState::Test,
            rule: None,
            subject: None,
            detail: Some("test notification for target ops".into()),
            ..firing()
        };
        assert_eq!(
            test.title(),
            "[Orbivisor lab] TEST: test notification for target ops"
        );
    }

    #[test]
    fn json_format_has_the_documented_fields() {
        let req = webhook_request(&webhook(WebhookFormat::Json), &firing());
        assert_eq!(req.content_type, "application/json");
        let v = body(&req);
        assert_eq!(v["version"], 1);
        assert_eq!(v["cluster"], "lab");
        assert_eq!(v["state"], "firing");
        assert_eq!(v["severity"], "critical");
        assert_eq!(v["rule"], "cpu-hot");
        assert_eq!(
            v["subject"],
            json!({"kind": "vm", "id": Uuid::from_u128(7), "name": "web1", "node": "node1"})
        );
        assert_eq!(v["value"], 97.456);
        assert_eq!(v["threshold"], 90.0);
        assert_eq!(v["since"], 1_700_000_000_000i64);
        assert_eq!(v["at"], 1_700_000_060_000i64);
        // 대상의 헤더를 붙인다.
        assert_eq!(
            req.headers,
            [("X-Extra".to_string(), "extra-secret".to_string())]
        );
    }

    #[test]
    fn slack_and_teams_bodies() {
        let v = body(&webhook_request(&webhook(WebhookFormat::Slack), &firing()));
        let text = v["text"].as_str().unwrap();
        assert!(
            text.starts_with("*[Orbivisor lab] FIRING critical: cpu-hot on web1*\n"),
            "{text}"
        );
        assert_eq!(v.as_object().unwrap().len(), 1);

        let v = body(&webhook_request(&webhook(WebhookFormat::Teams), &firing()));
        assert_eq!(v["type"], "message");
        let a = &v["attachments"][0];
        assert_eq!(a["contentType"], "application/vnd.microsoft.card.adaptive");
        assert_eq!(a["content"]["type"], "AdaptiveCard");
        assert_eq!(
            a["content"]["body"][0]["text"],
            "[Orbivisor lab] FIRING critical: cpu-hot on web1"
        );
    }

    #[test]
    fn ntfy_sends_text_with_headers_and_gotify_json_with_its_key() {
        let req = webhook_request(&webhook(WebhookFormat::Ntfy), &firing());
        assert_eq!(req.content_type, "text/plain; charset=utf-8");
        let text = String::from_utf8(req.body.clone()).unwrap();
        assert!(text.contains("Rule: cpu-hot"), "{text}");
        let header = |name: &str| {
            req.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            header("Title").as_deref(),
            Some("[Orbivisor lab] FIRING critical: cpu-hot on web1")
        );
        assert_eq!(header("Priority").as_deref(), Some("5"));
        assert_eq!(header("Tags").as_deref(), Some("rotating_light"));
        assert_eq!(
            header("Authorization").as_deref(),
            Some("Bearer tok-secret")
        );

        let resolved = Notification {
            state: NotificationState::Resolved,
            ..firing()
        };
        let req = webhook_request(&webhook(WebhookFormat::Gotify), &resolved);
        let v = body(&req);
        assert_eq!(
            v["title"],
            "[Orbivisor lab] RESOLVED critical: cpu-hot on web1"
        );
        assert_eq!(v["priority"], 2);
        assert!(v["message"].as_str().unwrap().contains("Subject: vm web1"));
        assert!(
            req.headers
                .contains(&("X-Gotify-Key".to_string(), "tok-secret".to_string()))
        );
        let v = body(&webhook_request(&webhook(WebhookFormat::Gotify), &firing()));
        assert_eq!(v["priority"], 8);
    }

    #[test]
    fn mail_is_plain_text_with_the_title_as_subject() {
        let s = SmtpTarget {
            host: "mail.example.com".into(),
            port: 587,
            tls: SmtpTls::Starttls,
            user: Some("ops".into()),
            password: Some("hunter2-secret".into()),
            from: "orbivisor@example.com".into(),
            to: vec!["a@example.com".into(), "b@example.com".into()],
        };
        let m = smtp_message(&s, &firing()).unwrap();
        let raw = String::from_utf8(m.formatted()).unwrap();
        assert!(
            raw.contains("Subject: [Orbivisor lab] FIRING critical: cpu-hot on web1"),
            "{raw}"
        );
        assert!(raw.contains("From: orbivisor@example.com"), "{raw}");
        assert!(raw.contains("To: a@example.com, b@example.com"), "{raw}");
        assert!(raw.contains("Content-Type: text/plain"), "{raw}");
        assert!(!raw.contains("hunter2-secret"));
        let bad = SmtpTarget {
            from: "not an address".into(),
            ..s
        };
        assert!(smtp_message(&bad, &firing()).is_err());
    }

    #[tokio::test]
    async fn webhook_errors_name_only_the_redacted_url() {
        // 닫힌 포트로 보내면 연결 오류다. 문구에는 경로·질의가 없다.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let w = WebhookTarget {
            url: format!("http://127.0.0.1:{port}/hook/SECRETPATH?key=SECRETQUERY"),
            ..webhook(WebhookFormat::Json)
        };
        let e = deliver(&TargetConfig::Webhook(w), &firing(), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(e.starts_with(&format!("http://127.0.0.1:{port}/…")), "{e}");
        assert!(!e.contains("SECRET"), "{e}");
    }

    #[test]
    fn the_rate_limit_holds_over_and_summarizes() {
        let config = SenderConfig {
            per_window: 2,
            window: Duration::from_secs(60),
            ..SenderConfig::default()
        };
        let mut windows = HashMap::new();
        let job = Job::new("ops".into(), Arc::new(firing()), false);
        assert!(admit(&mut windows, &job, &config));
        assert!(admit(&mut windows, &job, &config));
        assert!(!admit(&mut windows, &job, &config));
        assert!(!admit(&mut windows, &job, &config));
        // 창이 끝나기 전에는 모은 채 둔다.
        assert!(held_over(&mut windows, config.window, 5).is_empty());
        let out = held_over(&mut windows, Duration::ZERO, 5);
        assert_eq!(out.len(), 1);
        let (target, n) = &out[0];
        assert_eq!(target, "ops");
        assert_eq!(n.severity, Severity::Critical);
        assert_eq!(n.cluster, "lab");
        assert!(n.title().contains("2 more notifications"), "{}", n.title());
        assert!(windows.is_empty());
    }

    /// 노드 하나짜리 클러스터
    async fn one_node_cluster(dir: &std::path::Path) -> Arc<Cluster> {
        use orbivisor_cluster::ClusterConfig;
        use orbivisor_cluster::memnet::MemRouter;
        use orbivisor_cluster::types::{NodeId, OrbNode};

        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, &cluster);
        cluster
            .initialize(BTreeMap::from([(id, OrbNode::single(id.to_string()))]))
            .await
            .unwrap();
        cluster
            .raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == openraft::ServerState::Leader, "leader")
            .await
            .unwrap();
        Arc::new(cluster)
    }

    async fn add_target(cluster: &Cluster, name: &str, url: String) {
        let target: orbivisor_types::alert::NotificationTarget =
            orbivisor_types::resource::Resource::new(
                name,
                NotificationTargetSpec {
                    target: TargetConfig::Webhook(WebhookTarget {
                        url,
                        format: WebhookFormat::Json,
                        headers: BTreeMap::new(),
                        token: None,
                        ca_pem: None,
                    }),
                    enabled: true,
                },
            );
        cluster.create(&target).await.unwrap();
    }

    /// 받으면 200으로 답하고 받은 경로를 알리는 webhook 수신기
    async fn hook() -> (String, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let tx = tx.clone();
            async move {
                let _ = tx.send(uri.path().to_string());
                axum::http::StatusCode::OK
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, rx)
    }

    /// 연결은 받지만 답하지 않는 곳(응답 없는 호스트)
    fn black_hole() -> (std::net::TcpListener, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        (listener, url)
    }

    /// 응답 없는 대상에 알림이 몰려도 그 대상은 허가 하나만 쓰므로 다른 대상의 알림은 바로 간다(리뷰 1 #1).
    #[tokio::test]
    async fn a_black_holed_target_does_not_delay_other_targets() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let (_hole, slow_url) = black_hole();
        let (base, mut hits) = hook().await;
        add_target(&cluster, "slow", slow_url).await;
        add_target(&cluster, "fast", format!("{base}/fast")).await;
        let book = AlertBook::memory("node1");
        let clock: Arc<dyn Clock> = Arc::new(orbivisor_types::clock::SystemClock);
        let sender = Sender::spawn(
            cluster.clone(),
            book,
            clock,
            SenderConfig {
                timeout: Duration::from_secs(5),
                retries: vec![],
                per_window: 100,
                ..SenderConfig::default()
            },
        );
        for _ in 0..8 {
            sender.enqueue("slow", Arc::new(firing()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        sender.enqueue("fast", Arc::new(firing()));
        let got = tokio::time::timeout(Duration::from_secs(2), hits.recv())
            .await
            .expect("the fast target waited behind the black-holed one");
        assert_eq!(got.as_deref(), Some("/fast"));
        assert!(started.elapsed() < Duration::from_secs(2));
        sender.shutdown().await;
        cluster.shutdown().await.unwrap();
    }

    /// 허가가 없으면 대기열에서 꺼내지 않으므로 대기열 크기가 상한이다. 넘친 알림은 `dropped`로 기록한다(리뷰 1 #1).
    #[tokio::test]
    async fn the_queue_bound_holds_while_sends_are_stuck() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let (_hole, slow_url) = black_hole();
        add_target(&cluster, "slow", slow_url).await;
        let book = AlertBook::memory("node1");
        let clock: Arc<dyn Clock> = Arc::new(orbivisor_types::clock::SystemClock);
        let sender = Sender::spawn(
            cluster.clone(),
            book.clone(),
            clock,
            SenderConfig {
                concurrency: 1,
                timeout: Duration::from_secs(30),
                retries: vec![],
                per_window: 100,
                queue: 2,
                ..SenderConfig::default()
            },
        );
        // 처음 것은 꺼내 보내는 중이고(허가 0), 다음 둘은 대기열을 채우고, 넷째부터 버린다.
        sender.enqueue("slow", Arc::new(firing()));
        tokio::time::sleep(Duration::from_millis(200)).await;
        for _ in 0..4 {
            sender.enqueue("slow", Arc::new(firing()));
        }
        let window = orbivisor_types::alert::HistoryWindow {
            since: 0,
            until: i64::MAX,
            limit: 100,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let dropped = loop {
            let recs = book.read_history(window).unwrap();
            let dropped = recs
                .iter()
                .filter(|r| r.error.as_deref().is_some_and(|e| e.starts_with("dropped")))
                .count();
            if dropped >= 2 || Instant::now() > deadline {
                break dropped;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(dropped, 2);
        sender.shutdown().await;
        cluster.shutdown().await.unwrap();
    }

    fn job(target: &str, attempt: usize) -> Job {
        Job {
            attempt,
            ..Job::new(target.into(), Arc::new(firing()), false)
        }
    }

    fn shed(o: Option<Overflow>) -> Job {
        match o {
            Some(Overflow::Shed(j)) => j,
            o => panic!("not shed: {o:?}"),
        }
    }

    /// 줄 전체가 상한을 넘으면 가장 긴 줄의 가장 오래된 일을 버린다.
    #[test]
    fn full_lanes_shed_the_oldest_job_of_the_longest_lane() {
        let mut lanes = Lanes::default();
        for k in 1..=3 {
            assert!(lanes.push(job("slow", k), 1, 20, 4).is_none());
        }
        assert!(lanes.push(job("mail", 1), 1, 20, 4).is_none());
        let shed = shed(lanes.push(job("mail", 2), 1, 20, 4));
        assert_eq!((shed.target.as_str(), shed.attempt), ("slow", 1));
        assert_eq!(lanes.waiting, 4);
        // 대상마다 하나씩만 꺼낸다.
        let permits = Arc::new(Semaphore::new(4));
        let a = lanes.next(&permits).unwrap();
        let b = lanes.next(&permits).unwrap();
        assert_ne!(a.0.target, b.0.target);
        assert!(lanes.next(&permits).is_none());
        drop(a);
        assert!(lanes.next(&permits).is_some());
    }

    /// 한 대상의 줄은 분당 상한까지이고 넘친 것은 "N건 더"로 접는다. 처음 넣은 뒤 `max_age`가 지난 일은 줄에서
    /// 버린다(리뷰 2 #2).
    #[tokio::test(start_paused = true)]
    async fn a_lane_holds_at_most_a_window_and_drops_stale_jobs() {
        let mut lanes = Lanes::default();
        for k in 0..3 {
            assert!(lanes.push(job("slack", k), 1, 3, 1000).is_none());
        }
        // 요약은 세지 않는다.
        let summary = Job::new("slack".into(), Arc::new(firing()), true);
        assert!(lanes.push(summary, 1, 3, 1000).is_none());
        match lanes.push(job("slack", 3), 1, 3, 1000) {
            Some(Overflow::Fold(j)) => assert_eq!(j.attempt, 0),
            o => panic!("not folded: {o:?}"),
        }
        assert_eq!(lanes.waiting, 4);
        let mut windows = HashMap::new();
        queue(
            &mut lanes,
            &mut windows,
            job("slack", 4),
            (1, 3, 1000),
            &|_: &Job, _| panic!("dropped"),
        );
        assert_eq!(windows["slack"].held, 1);
        let out = held_over(&mut windows, Duration::ZERO, 5);
        assert!(
            out[0].1.title().contains("1 more notifications"),
            "{}",
            out[0].1.title()
        );

        let max_age = Duration::from_secs(900);
        tokio::time::advance(Duration::from_secs(600)).await;
        assert!(lanes.push(job("mail", 0), 1, 3, 1000).is_none());
        tokio::time::advance(Duration::from_secs(301)).await;
        let old = lanes.expire(Instant::now(), max_age, &HashMap::new());
        assert_eq!(old.len(), 4);
        assert!(
            old.iter()
                .all(|(j, why)| j.target == "slack" && *why == DropReason::Stale)
        );
        assert_eq!(lanes.waiting, 1);
        let permits = Arc::new(Semaphore::new(4));
        assert_eq!(lanes.next(&permits).unwrap().0.target, "mail");
    }

    /// 몫이 빈 대상 둘이 번갈아 있어도 뒤쪽 줄이 차례를 얻는다(리뷰 2 #4).
    #[test]
    fn lanes_take_turns() {
        let mut lanes = Lanes::default();
        for t in ["a", "b", "c"] {
            for k in 0..3 {
                assert!(lanes.push(job(t, k), 1, 20, 100).is_none());
            }
        }
        // 허가가 하나뿐이라 한 번에 하나씩 보낸다.
        let permits = Arc::new(Semaphore::new(1));
        let mut order = Vec::new();
        for _ in 0..6 {
            let (j, permit, slot) = lanes.next(&permits).unwrap();
            order.push(j.target);
            drop((permit, slot));
        }
        assert_eq!(order, ["a", "b", "c", "a", "b", "c"]);
    }

    /// 다시 하기를 기다리는 `FIRING`은 그 뒤에 받은 같은 경보·같은 대상의 `RESOLVED`가 있으면 버린다. 다른 대상,
    /// 처음 보내는 것, `RESOLVED` 뒤에 다시 넣은 `FIRING`은 둔다(리뷰 2 #5).
    #[tokio::test(start_paused = true)]
    async fn a_resolved_alert_supersedes_pending_firing_retries() {
        let retry = job("ops", 1);
        let first = job("ops", 0);
        let other = job("mail", 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        let resolved_job = Job::new(
            "ops".into(),
            Arc::new(Notification {
                state: NotificationState::Resolved,
                ..firing()
            }),
            false,
        );
        let resolved = HashMap::from([(resolved_job.alert().unwrap(), resolved_job.born)]);
        tokio::time::advance(Duration::from_secs(1)).await;
        let later = job("ops", 1);
        assert!(retry.superseded(&resolved));
        assert!(!first.superseded(&resolved));
        assert!(!other.superseded(&resolved));
        assert!(!later.superseded(&resolved));
        assert!(!resolved_job.superseded(&resolved));
        let mut lanes = Lanes::default();
        for j in [retry, first, other, later] {
            assert!(lanes.push(j, 1, 20, 100).is_none());
        }
        let out = lanes.expire(Instant::now(), Duration::from_secs(900), &resolved);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, DropReason::Resolved);
        assert_eq!(lanes.waiting, 3);
    }
}
