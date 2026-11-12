//! 텔레메트리 리포터: 시험 HTTPS 수집 서버(시험 CA)로 한 번 보내고 `last_sent_at`을 적는지, 간격 전에는
//! 다시 보내지 않고 간격이 지나면 다시 보내는지, 실패하면 `last_error`를 적고 retry 뒤 다시 보내는지,
//! 꺼져 있거나 옛 노드가 남아 있으면 보내지 않는지, 리다이렉트를 따르지 않는지, 끝점이 없으면 오류를
//! 적는지. 짧은 tick으로 돌린다.

mod common;

use std::time::{Duration, Instant};

use common::Env;
use common::collector::Collector;
use opl_api::{TelemetryConfig, TelemetryReporter};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::settings::{
    SETTINGS_NAME, Settings, SettingsSpec, TelemetrySettings, TelemetryStatus,
};
use opl_types::version::TELEMETRY_URL;
use reqwest::StatusCode;
use serde_json::json;

/// 기다리는 한도. macOS에서는 새 CA로 하는 첫 TLS 연결의 인증서 검증이 15초쯤 걸리기도 한다.
const REPORT_WAIT: Duration = Duration::from_secs(60);

/// 50ms마다 보는 리포터. 수집 서버의 CA를 믿는다. 시각은 초 단위라 retry 3초는 적어도 2초 뒤다.
fn reporter(env: &Env, collector: &Collector, interval: Duration) -> TelemetryReporter {
    TelemetryReporter::spawn(
        env.state.clone(),
        TelemetryConfig {
            tick: Duration::from_millis(50),
            interval,
            retry: Duration::from_secs(3),
            timeout: Duration::from_secs(30),
            extra_roots: vec![collector.ca_der.clone()],
        },
    )
}

/// 수집 서버로 텔레메트리를 켠다(끄면 끝점만 둔다).
async fn configure(env: &Env, collector: &Collector, enabled: bool) {
    let admin = env.admin().await;
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": enabled, "endpoint": collector.url } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn telemetry(env: &Env) -> TelemetryStatus {
    env.cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .and_then(|s| s.status.telemetry)
        .unwrap_or_default()
}

async fn until(why: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + REPORT_WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 켜면 한 번 보내고 `last_sent_at`을 적는다. 간격(24시간) 전에는 다시 보내지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_once_and_records_the_time() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    configure(&env, &collector, true).await;
    let id = telemetry(&env).id.unwrap();
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    until("a report", || collector.count() == 1).await;
    until("last_sent_at", || telemetry(&env).last_sent_at.is_some()).await;
    let report = &collector.reports()[0];
    assert_eq!(report["schema"], 1, "{report}");
    assert_eq!(report["id"], id.to_string(), "{report}");
    assert_eq!(report["nodes"]["total"], 1, "{report}");
    let status = telemetry(&env);
    assert_eq!(status.id, Some(id));
    assert_eq!(status.last_sent_at, report["sent_at"].as_u64());
    assert_eq!(status.last_error, None);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(collector.count(), 1);
    reporter.shutdown().await;
}

/// 간격이 지나면 다시 보낸다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_again_after_the_interval() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    configure(&env, &collector, true).await;
    let reporter = reporter(&env, &collector, Duration::from_secs(1));

    until("two reports", || collector.count() >= 2).await;
    let reports = collector.reports();
    assert!(
        reports[1]["sent_at"].as_u64() > reports[0]["sent_at"].as_u64(),
        "{reports:?}"
    );
    reporter.shutdown().await;
}

/// 수집 서버가 실패하면 `last_error`를 적고 retry 전에는 다시 보내지 않으며, 그 뒤 다시 보낸다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_is_recorded_and_retried() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    collector.respond_with(500);
    configure(&env, &collector, true).await;
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    until("last_error", || telemetry(&env).last_error.is_some()).await;
    let status = telemetry(&env);
    let error = status.last_error.unwrap();
    assert!(error.contains("500"), "{error}");
    assert_eq!(status.last_sent_at, None);
    assert_eq!(collector.count(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(collector.count(), 1);

    collector.respond_with(200);
    until("a retried report", || {
        telemetry(&env).last_sent_at.is_some()
    })
    .await;
    assert_eq!(collector.count(), 2);
    assert_eq!(telemetry(&env).last_error, None);
    reporter.shutdown().await;
}

/// 꺼져 있으면 보내지 않고, 켜면 보낸다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_sent_while_disabled() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    configure(&env, &collector, false).await;
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(collector.count(), 0);
    assert_eq!(telemetry(&env).last_sent_at, None);

    configure(&env, &collector, true).await;
    until("a report", || collector.count() == 1).await;
    reporter.shutdown().await;
}

/// 이 빌드보다 낮은 노드가 남아 있으면(설정의 새 필드를 풀지 못한다) 보내지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_sent_while_an_old_node_remains() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    configure(&env, &collector, true).await;
    let mut old: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.9.8.7:8006".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    old.status.version = Some("0.0.1".into());
    env.cluster.create(&old).await.unwrap();
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(collector.count(), 0);
    assert_eq!(telemetry(&env).last_sent_at, None);

    env.cluster.delete::<NodeSpec>(old.id, None).await.unwrap();
    until("a report", || collector.count() == 1).await;
    reporter.shutdown().await;
}

/// 3xx는 따르지 않고 실패로 적는다(끝점을 바꾼 응답이 보고를 다른 곳으로 보내지 못하게).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_are_not_followed() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    // 따른다면 같은 수집 서버로 다시 보내 요청 수가 늘어난다.
    collector.redirect_to(&collector.url);
    configure(&env, &collector, true).await;
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    until("last_error", || telemetry(&env).last_error.is_some()).await;
    let error = telemetry(&env).last_error.unwrap();
    assert!(error.starts_with("HTTP 307"), "{error}");
    assert_eq!(telemetry(&env).last_sent_at, None);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(collector.count(), 1);
    reporter.shutdown().await;
}

/// 켜져 있는데 쓸 끝점이 없으면(설정도 빌드 값도 없음) 그 이유를 last_error에 한 번 적는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_endpoint_is_recorded() {
    if !TELEMETRY_URL.is_empty() {
        return;
    }
    let env = Env::start().await;
    let collector = Collector::start().await;
    // API는 이런 설정을 받지 않는다. 다른 빌드(빌드 기본 끝점이 있는)가 쓴 설정을 흉내 낸다.
    let settings: Settings = Settings::new(
        SETTINGS_NAME,
        SettingsSpec {
            telemetry: TelemetrySettings {
                enabled: true,
                endpoint: None,
            },
            ..SettingsSpec::default()
        },
    );
    env.cluster.create(&settings).await.unwrap();
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    until("last_error", || telemetry(&env).last_error.is_some()).await;
    assert_eq!(
        telemetry(&env).last_error.as_deref(),
        Some("no telemetry endpoint: set telemetry.endpoint")
    );
    let revision = |env: &Env| {
        env.cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
            .unwrap()
            .revision
    };
    let written = revision(&env);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        revision(&env),
        written,
        "the same error is not written again"
    );
    assert_eq!(collector.count(), 0);
    reporter.shutdown().await;
}
