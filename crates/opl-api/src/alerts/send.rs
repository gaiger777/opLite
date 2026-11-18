//! 알림 보내기(설계 §9): SMTP(`lettre`)와 webhook 다섯 형식(일반 JSON·Slack·Teams·ntfy·Gotify, `reqwest`).
//! 경보 엔진은 [`Sender`]의 대기열(노드마다 1000개)에 넣기만 하고 기다리지 않는다. 보내기는 동시에 4개까지, 한 번에
//! 10초까지이고, 실패하면 30초·2분·10분 뒤 다시 한다. 대상마다 분당 20개를 넘는 알림은 보내지 않고 모았다가 다음 분에
//! "N건 더" 알림 하나로 알린다. 결과는 경보 장부의 보낸 기록에 남긴다. 오류 문구에는 비밀(비밀번호·토큰·헤더 값·URL의
//! 경로와 질의)을 넣지 않는다.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::Cluster;
use opl_types::alert::{
    AlertSubject, NotificationRecord, NotificationState, NotificationTargetSpec, Severity,
    SmtpTarget, SmtpTls, TargetConfig, WebhookFormat, WebhookTarget, redact_url,
};
use opl_types::clock::Clock;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::{Semaphore, mpsc, oneshot};
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
            (Some(rule), Some(s)) => format!("{head} {}: {rule} on {}", self.severity, s.name),
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
            let subject = n.subject.as_ref().map(
                |s| json!({ "kind": s.kind.as_str(), "id": s.id, "name": s.name, "node": s.node }),
            );
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
        subject: n.subject.as_ref().map(|s| s.name.clone()),
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
    /// 대기열 크기. 넘치면 버리고 기록에 남긴다.
    pub queue: usize,
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
}

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
        let job = Job {
            target: target.into(),
            n,
            attempt: 0,
            summary: false,
        };
        if let Err(e) = self.tx.try_send(job) {
            let job = match e {
                mpsc::error::TrySendError::Full(j) | mpsc::error::TrySendError::Closed(j) => j,
            };
            tracing::warn!(target = %job.target, "the notification queue is full; dropping a notification");
            let rec = record_of(
                &job.n,
                &job.target,
                self.clock.now_ms(),
                Err("dropped: the send queue is full".into()),
            );
            let book = self.book.clone();
            tokio::task::spawn_blocking(move || book.record(rec));
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

/// 보내기 작업이 끝난 뒤 할 일
enum Next {
    Done,
    /// 기다린 뒤 다시 한다.
    Retry(Job),
    /// 기다림이 끝났다.
    Again(Job),
}

async fn run(ctx: Arc<Ctx>, mut rx: mpsc::Receiver<Job>, mut stop: oneshot::Receiver<()>) {
    let permits = Arc::new(Semaphore::new(ctx.config.concurrency.max(1)));
    let mut windows: HashMap<String, Window> = HashMap::new();
    let mut work: JoinSet<Next> = JoinSet::new();
    let mut flush = tokio::time::interval(Duration::from_secs(1).min(ctx.config.window));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let job = tokio::select! {
            _ = &mut stop => break,
            job = rx.recv() => match job {
                Some(job) => job,
                None => break,
            },
            Some(done) = work.join_next(), if !work.is_empty() => match done {
                Ok(Next::Done) | Err(_) => continue,
                Ok(Next::Retry(job)) => {
                    let wait = ctx.config.retries[job.attempt - 1];
                    work.spawn(async move {
                        tokio::time::sleep(wait).await;
                        Next::Again(job)
                    });
                    continue;
                }
                Ok(Next::Again(job)) => job,
            },
            _ = flush.tick() => {
                for (target, n) in held_over(&mut windows, ctx.config.window, ctx.clock.now_ms()) {
                    let job = Job { target, n: Arc::new(n), attempt: 0, summary: true };
                    work.spawn(attempt(ctx.clone(), permits.clone(), job));
                }
                continue;
            }
        };
        if job.attempt == 0 && !job.summary && !admit(&mut windows, &job, &ctx.config) {
            continue;
        }
        work.spawn(attempt(ctx.clone(), permits.clone(), job));
    }
    // 남은 보내기·기다림은 `work`를 버리며 끊는다.
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

/// 한 번 보낸다. 대상을 Raft에서 읽고(없으면 기록하고 끝, 꺼졌으면 조용히 끝), 실패하면 남은 다시 하기가 있으면
/// [`Next::Retry`], 없으면 실패를 기록한다.
async fn attempt(ctx: Arc<Ctx>, permits: Arc<Semaphore>, job: Job) -> Next {
    let Ok(_permit) = permits.acquire_owned().await else {
        return Next::Done;
    };
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

    use opl_types::alert::SubjectKind;
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
        let job = Job {
            target: "ops".into(),
            n: Arc::new(firing()),
            attempt: 0,
            summary: false,
        };
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
}
