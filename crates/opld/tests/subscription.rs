//! 계획 33: 일반 노드 둘과 위트니스로 만든 클러스터에 구독 키를 넣고, 텔레메트리를 켜면 리더가 시험
//! HTTPS 수집 서버(`[telemetry] ca_file`로 믿는 시험 CA)로 한 번만 보낸다. 리더를 kill -9 해도 새 리더는
//! 간격(24시간) 전에 다시 보내지 않는다. 남은 노드를 짧은 간격으로 다시 시작하면 새 리더가 보낸다.
//! QEMU는 쓰지 않는다.

mod common;

#[path = "../../opl-api/tests/common/collector.rs"]
mod collector;

use std::time::Duration;

use collector::Collector;
use common::{Daemon, eventually};
use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
use serde_json::Value;

/// 저장소 개발 키의 비밀 키 견본
const DEV_PRIVATE: &str = include_str!("../../opl-types/tests/fixtures/dev-license.key");
const DAY: u64 = 24 * 3600;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn leader_name(d: &Daemon) -> Option<String> {
    let out = d.ctl(&["-o", "json", "cluster", "status"]).await.ok()?;
    let view: Value = serde_json::from_str(&out).ok()?;
    view["members"]
        .as_array()?
        .iter()
        .find(|m| m["id"] == view["leader"])?["name"]
        .as_str()
        .map(str::to_string)
}

/// `oplctl -o json telemetry show`의 `status.telemetry`
async fn telemetry_status(d: &Daemon) -> Value {
    d.ctl_json(&["telemetry", "show"]).await["status"]["telemetry"].clone()
}

#[tokio::test]
async fn the_leader_reports_once_even_after_a_failover() {
    let collector = Collector::start().await;
    let ca = tempfile::tempdir().unwrap();
    let ca_file = ca.path().join("collector-ca.pem");
    std::fs::write(&ca_file, &collector.ca_pem).unwrap();
    // 틱은 짧게, 간격은 길게: 한 번 보낸 뒤로는 리더가 바뀌어도 다시 보내지 않아야 한다.
    let extra = format!(
        "[telemetry]\ntick = \"1s\"\ninterval = \"24h\"\nca_file = \"{}\"",
        ca_file.display()
    );
    let mut nodes = vec![Daemon::new("node-a", &extra), Daemon::new("node-b", &extra)];
    let mut w = Daemon::witness("wit");
    for d in &mut nodes {
        d.start().await;
    }
    w.start().await;
    nodes[0].init("prod").await;
    let password = nodes[0].admin_password.clone().unwrap();
    let fp = nodes[0].ca_fingerprint.clone().unwrap();
    nodes[0].login().await;
    let url = nodes[0].join_url.clone().unwrap();
    nodes[1].join(&url).await.unwrap();
    nodes[1].login_with(&password, &fp).await;
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();

    // 개발 키로 서명한 구독 키: 위트니스는 세지 않는다.
    let key = sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "sub-e2e".into(),
            customer: "Example Corp".into(),
            level: Level::Standard,
            nodes: 3,
            issued_at: now() - DAY,
            expires_at: now() + 30 * DAY,
        },
    )
    .unwrap();
    // 모든 노드(위트니스 포함)가 버전을 적어야 새 필드를 쓴다.
    eventually("the subscription is accepted", || async {
        nodes[0].ctl(&["subscription", "set", &key]).await.is_ok()
    })
    .await;
    // 다른 노드는 커밋을 조금 늦게 적용할 수 있다.
    eventually("the other node sees the subscription", || async {
        let shown = nodes[1].ctl(&["subscription", "show"]).await.unwrap();
        shown.contains("state: active (development key)") && shown.contains("nodes: 2/3")
    })
    .await;

    nodes[0]
        .ctl(&["telemetry", "enable", "--endpoint", &collector.url])
        .await
        .unwrap();
    eventually("a telemetry report", || async { collector.count() == 1 }).await;
    eventually("last_sent_at", || async {
        telemetry_status(&nodes[1]).await["last_sent_at"].is_u64()
    })
    .await;
    let report = &collector.reports()[0];
    assert_eq!(report["nodes"]["total"], 3, "{report}");
    assert_eq!(report["nodes"]["witness"], 1, "{report}");
    assert_eq!(report["subscription"], "standard", "{report}");
    let sent = telemetry_status(&nodes[1]).await;

    // 리더(일반 노드)를 kill -9 하면 남은 일반 노드가 리더가 된다. 몇 틱이 지나도 다시 보내지 않는다.
    let leader = leader_name(&nodes[0]).await.unwrap();
    let dead = nodes.iter().position(|d| d.name == leader).unwrap();
    nodes[dead].kill();
    let alive = &nodes[1 - dead];
    eventually("the other node leads", || async {
        leader_name(alive).await.as_deref() == Some(alive.name.as_str())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(collector.count(), 1, "{:?}", collector.reports());
    assert_eq!(telemetry_status(alive).await, sent);

    // 남은 노드를 간격 2초로 다시 시작하면(위트니스와 정족수) 새 리더가 다시 보낸다.
    let alive = &mut nodes[1 - dead];
    alive.stop();
    let config = alive.path("opld.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("interval = \"24h\""), "{text}");
    std::fs::write(
        &config,
        text.replace("interval = \"24h\"", "interval = \"2s\""),
    )
    .unwrap();
    alive.start().await;
    eventually("a report from the new leader", || async {
        collector.count() >= 2
    })
    .await;
    let reports = collector.reports();
    assert!(
        reports[1]["sent_at"].as_u64() > reports[0]["sent_at"].as_u64(),
        "{reports:?}"
    );
    assert_eq!(reports[1]["id"], reports[0]["id"], "{reports:?}");
}
