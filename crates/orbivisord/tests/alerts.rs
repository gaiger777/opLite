//! 계획 60 경보 E2E(설계 §13 3번): 시험 안 webhook 수신기를 127.0.0.1에 띄우고 `notify target create … webhook`으로
//! 대상을 만든다. 시험 보내기가 닿고, 규칙 `--node-metric cpu --gt -1`(for 0)이 다음 평가에서 `firing`을 보내며,
//! `alert rule set --gt 1000`으로 조건이 거짓이 되면 해제 대기(`[test] alert_tick_ms = 200`이면 12 tick = 2.4초) 뒤
//! `resolved`를 보낸다. `alert list`·`alert history`에 그것들이 보인다. 리더 경보(`node_offline`)의 리더 바뀜 뒤 다시
//! 보내지 않음은 orbivisor-api `tests/alerts.rs`가 본다(노드 셋으로는 노드 하나를 끈 채 리더를 바꾸면 정족수가 없다).
//!
//! `cargo test -p orbivisord --features metrics-it --test alerts`(QEMU 없음)
#![cfg(any(feature = "metrics-it", feature = "qemu-it"))]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::StatusCode;
use common::{Daemon, WAIT};
use serde_json::Value;

/// 받은 webhook 본문들
#[derive(Clone, Default)]
struct Hook {
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl Hook {
    /// 127.0.0.1의 빈 포트에 띄우고 URL을 돌려준다.
    async fn start(&self) -> String {
        let bodies = self.bodies.clone();
        let app = axum::Router::new().fallback(move |body: Bytes| {
            let bodies = bodies.clone();
            async move {
                bodies
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                StatusCode::OK
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// `state`인 본문이 올 때까지 기다린다.
    async fn wait(&self, state: &str) -> Value {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(b) = self
                .bodies
                .lock()
                .unwrap()
                .iter()
                .find(|b| b["state"] == state)
            {
                return b.clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {state} notification: {:?}",
                self.bodies.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn count(&self, state: &str) -> usize {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b["state"] == state)
            .count()
    }
}

#[tokio::test]
async fn an_alert_fires_and_resolves_to_a_webhook() {
    let mut d = Daemon::new("node1", "[test]\nalert_tick_ms = 200\n");
    d.start().await;
    d.init("alerts").await;
    d.login().await;
    let hook = Hook::default();
    let url = hook.start().await;

    let out = d
        .ctl(&[
            "notify", "target", "create", "hook", "webhook", "--url", &url,
        ])
        .await
        .unwrap();
    assert_eq!(out, "notification target hook created\n");
    let list = d.ctl(&["notify", "target", "list"]).await.unwrap();
    // 응답의 URL은 줄인 URL이다(경로를 숨긴다).
    assert!(list.contains("hook  webhook  true"), "{list}");
    assert!(!list.contains("/hook"), "{list}");
    let out = d.ctl(&["notify", "target", "test", "hook"]).await.unwrap();
    assert_eq!(out, "notification target hook: test sent\n");
    hook.wait("test").await;

    let out = d
        .ctl(&[
            "alert",
            "rule",
            "create",
            "cpu-any",
            "--node-metric",
            "cpu",
            "--gt",
            "-1",
            "--target",
            "hook",
            "--severity",
            "critical",
        ])
        .await
        .unwrap();
    assert_eq!(out, "alert rule cpu-any created\n");
    let fired = hook.wait("firing").await;
    assert_eq!(fired["rule"], "cpu-any", "{fired}");
    assert_eq!(fired["severity"], "critical", "{fired}");
    assert_eq!(fired["subject"]["kind"], "node", "{fired}");
    assert_eq!(fired["subject"]["name"], "node1", "{fired}");
    assert_eq!(fired["threshold"], -1.0, "{fired}");

    let alerts = d.ctl_json(&["alert", "list"]).await;
    let active = alerts["alerts"].as_array().unwrap();
    assert!(
        active
            .iter()
            .any(|a| a["rule_name"] == "cpu-any" && a["state"] == "firing"),
        "{alerts}"
    );
    let table = d.ctl(&["alert", "list"]).await.unwrap();
    assert!(
        table.lines().any(|l| l.split_whitespace().take(4).eq([
            "firing",
            "critical",
            "cpu-any",
            "node/node1"
        ])),
        "{table}"
    );

    // 문턱만 바꿔 조건을 거짓으로 만든다.
    let out = d
        .ctl(&["alert", "rule", "set", "cpu-any", "--gt", "1000"])
        .await
        .unwrap();
    assert_eq!(out, "alert rule cpu-any updated\n");
    let rule = d.ctl_json(&["alert", "rule", "get", "cpu-any"]).await;
    assert_eq!(rule["spec"]["condition"]["value"], 1000.0, "{rule}");
    let resolved = hook.wait("resolved").await;
    assert_eq!(resolved["rule"], "cpu-any", "{resolved}");
    assert_eq!(hook.count("firing"), 1);
    common::eventually("no active alerts", || async {
        let alerts = d.ctl_json(&["alert", "list"]).await;
        alerts["alerts"].as_array().unwrap().is_empty()
    })
    .await;

    let history = d.ctl_json(&["alert", "history", "--since", "1h"]).await;
    let records = history["records"].as_array().unwrap();
    for state in ["test", "firing", "resolved"] {
        assert!(
            records
                .iter()
                .any(|r| r["state"] == state && r["ok"] == true && r["target"] == "hook"),
            "no {state} record: {history}"
        );
    }
    // 새 것부터
    assert_eq!(records[0]["state"], "resolved", "{history}");
    let table = d.ctl(&["alert", "history"]).await.unwrap();
    assert!(table.starts_with("TIME"), "{table}");
    assert!(
        table
            .lines()
            .any(|l| l.contains("resolved  cpu-any") && l.ends_with("hook    ok")),
        "{table}"
    );

    // 쓰는 규칙이 있으면 대상을 지울 수 없다.
    let err = d
        .ctl(&["notify", "target", "delete", "hook"])
        .await
        .unwrap_err();
    assert!(err.to_string().ends_with("(in_use)"), "{err}");
    d.ctl(&["alert", "rule", "delete", "cpu-any"])
        .await
        .unwrap();
    d.ctl(&["notify", "target", "delete", "hook"])
        .await
        .unwrap();
}
