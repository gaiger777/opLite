//! 계획 60 지표 기록과 Prometheus E2E(설계 §13):
//! 1. 재시작 뒤 기록: `[test] metrics_flush_secs = 1`로 띄운 노드를 SIGKILL로 끝내고 다시 띄워도
//!    `node stats NODE --range hour -o json`에 재시작 전 점이 그대로 있고, 꺼져 있던 동안은 `null`이다.
//! 2. Prometheus 긁기: 꺼져 있으면 404, 토큰이 없으면 401, `prometheus enable` 뒤 viewer 토큰으로 받은 본문이 엄격한
//!    텍스트 형식 파서(`orbivisor-api/tests/common/promtext.rs`)로 풀린다. `qemu-it`면 VM 줄도 본다. `OPL_TEST_PROMTOOL`이
//!    있으면(GitHub `linux-kvm`) 그 `promtool check metrics`로도 본다.
//! 3. (`qemu-it`) 마이그레이션 뒤 기록: 노드 1에서 켠 디스크 없는 VM을 끄고 정해진 노드를 노드 2로 바꿔 켜면, 짧게 둔
//!    `[test] metrics_push_secs` 뒤 노드 2의 `vm/{id}.rrd`에 노드 1 시절 점이 있고 노드 1의 파일은 없어진다.
//!
//! `cargo test -p orbivisord --features metrics-it --test metrics_history`(QEMU 없음, 1·2),
//! `cargo test -p orbivisord --features qemu-it --test metrics_history`(1·2·3, QEMU 필요)
#![cfg(any(feature = "metrics-it", feature = "qemu-it"))]

mod common;
#[path = "../../orbivisor-api/tests/common/promtext.rs"]
#[allow(dead_code)]
mod promtext;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::{Daemon, eventually};
use serde_json::Value;

/// 기록 답의 한 칸에서 아는 점들(시각, 값)
fn known(reply: &Value, field: &str) -> Vec<(i64, f64)> {
    let start = reply["start"].as_i64().unwrap();
    let step = reply["step_ms"].as_i64().unwrap();
    reply["columns"][field]["avg"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.as_f64().map(|v| (start + i as i64 * step, v)))
        .collect()
}

/// 노드 `node`의 지난 1시간 기록(5초 점 그대로)
async fn node_history(d: &Daemon, node: &str) -> Value {
    d.ctl_json(&["node", "stats", node, "--range", "hour", "--points", "1000"])
        .await
}

#[tokio::test]
async fn history_survives_a_kill_and_restart() {
    let mut d = Daemon::new("node1", "[test]\nmetrics_flush_secs = 1\n");
    d.start().await;
    d.init("hist").await;
    d.login().await;

    // 5초 점이 셋 쌓이면 그 점들을 기억하고, 파일에 쓰일 때까지(1초마다) 기다린 뒤 죽인다.
    eventually("three node samples", || async {
        known(&node_history(&d, "node1").await, "cpu").len() >= 3
    })
    .await;
    let reply = node_history(&d, "node1").await;
    assert_eq!(reply["tier"], "raw5s", "{reply}");
    assert_eq!(reply["step_ms"], 5000, "{reply}");
    let before = known(&reply, "cpu");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let rrd = d.data_dir().join("metrics").join("node.rrd");
    let mode = std::fs::metadata(&rrd).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{}", rrd.display());
    d.kill();
    // 측정 간격 둘보다 길게 꺼 둔다.
    tokio::time::sleep(Duration::from_secs(12)).await;
    d.start().await;

    let last_before = before.last().unwrap().0;
    eventually("a sample after the restart", || async {
        known(&node_history(&d, "node1").await, "cpu")
            .iter()
            .any(|(t, _)| *t > last_before + 10_000)
    })
    .await;
    let reply = node_history(&d, "node1").await;
    let after = known(&reply, "cpu");
    for (t, v) in &before {
        assert!(
            after
                .iter()
                .any(|(t2, v2)| t2 == t && (v2 - v).abs() < 1e-3),
            "point {t} = {v} is gone after the restart: {reply}"
        );
    }
    // 꺼져 있던 동안의 칸은 `null`이다.
    let first_new = after.iter().find(|(t, _)| *t > last_before).unwrap().0;
    let start = reply["start"].as_i64().unwrap();
    let cpu = reply["columns"]["cpu"]["avg"].as_array().unwrap();
    let gap: Vec<&Value> = cpu
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let t = start + *i as i64 * 5000;
            t > last_before && t < first_new
        })
        .map(|(_, v)| v)
        .collect();
    assert!(!gap.is_empty(), "{reply}");
    assert!(gap.iter().all(|v| v.is_null()), "{reply}");
    // 표도 같은 기록이다(지난 1시간은 정확한 백분위).
    let table = d
        .ctl(&["node", "stats", "node1", "--range", "hour"])
        .await
        .unwrap();
    assert!(table.contains("(tier raw5s, step"), "{table}");
    assert!(
        table.lines().nth(1).is_some_and(|l| l
            .split_whitespace()
            .eq(["FIELD", "AVG", "MAX", "MIN", "P50", "P95"])),
        "{table}"
    );
    assert!(table.lines().any(|l| l.starts_with("cpu ")), "{table}");
    // NODE 없이 범위를 주면 클러스터 요약이다.
    let digest = d.ctl(&["node", "stats", "--range", "hour"]).await.unwrap();
    assert!(digest.starts_with("cluster (1 nodes): "), "{digest}");
    assert!(digest.contains("\nnode1 "), "{digest}");
}

/// `GET /api/v1/metrics`(인증서는 시험 CA라 확인하지 않는다)
async fn scrape(d: &Daemon, token: Option<&str>) -> (reqwest::StatusCode, String) {
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let mut r = http.get(format!("{}/api/v1/metrics", d.server()));
    if let Some(token) = token {
        r = r.bearer_auth(token);
    }
    let resp = r.send().await.unwrap();
    (resp.status(), resp.text().await.unwrap())
}

/// viewer 사용자 `alice`를 만들어 로그인하고 그 API 토큰을 돌려준다.
async fn viewer_token(d: &Daemon) -> String {
    let pw = d.path("alice-password");
    std::fs::write(&pw, "alice-password-1").unwrap();
    let pw = pw.to_str().unwrap();
    d.ctl(&[
        "user",
        "create",
        "alice",
        "--role",
        "viewer",
        "--password-file",
        pw,
    ])
    .await
    .unwrap();
    let config = d.path("alice.json");
    let server = d.server();
    let mut out = Vec::new();
    orbivisorctl::run(
        [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "login",
            "--server",
            &server,
            "--user",
            "alice",
            "--password-file",
            pw,
            "--ca-fingerprint",
            d.ca_fingerprint.as_deref().unwrap(),
        ],
        &mut out,
    )
    .await
    .unwrap();
    let config: Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    config["token"].as_str().unwrap().to_string()
}

/// 표본 하나의 값(레이블이 모두 맞는 줄)
fn sample(families: &[promtext::Family], name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    promtext::samples(families, name)
        .iter()
        .find(|(l, _)| {
            labels
                .iter()
                .all(|(k, v)| l.get(*k).map(String::as_str) == Some(*v))
        })
        .map(|(_, v)| *v)
}

#[tokio::test]
async fn prometheus_scrapes_parse_with_the_strict_parser() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("prom").await;
    d.login().await;
    let viewer = viewer_token(&d).await;

    let (status, _) = scrape(&d, None).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    let (status, body) = scrape(&d, Some(&viewer)).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{body}");
    let out = d.ctl(&["prometheus", "show"]).await.unwrap();
    assert_eq!(out, "prometheus: disabled\n");
    let out = d.ctl(&["prometheus", "enable"]).await.unwrap();
    assert!(
        out.starts_with("prometheus: enabled\nscrape: https://"),
        "{out}"
    );

    #[cfg(feature = "qemu-it")]
    d.ctl(&["vm", "create", "plain", "--memory", "256", "--start"])
        .await
        .unwrap();

    // 노드 점(15초 안)이 생기고, qemu-it면 VM 줄도 생길 때까지(응답은 5초 캐시)
    let deadline = std::time::Instant::now() + common::WAIT;
    let text = loop {
        let (status, body) = scrape(&d, Some(&viewer)).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{body}");
        let families = promtext::parse(&body).unwrap_or_else(|e| panic!("{e}:\n{body}"));
        let node = sample(
            &families,
            "orbivisor_node_cpu_usage_ratio",
            &[("node", "node1")],
        );
        let vm = !cfg!(feature = "qemu-it")
            || (sample(
                &families,
                "orbivisor_vm_info",
                &[("vm", "plain"), ("node", "node1")],
            ) == Some(1.0)
                && !promtext::samples(&families, "orbivisor_vm_cpu_usage_ratio").is_empty());
        if node.is_some() && vm {
            break body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no node (or VM) lines in the scrape:\n{body}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let families = promtext::parse(&text).unwrap();
    assert_eq!(
        sample(&families, "orbivisor_cluster_leader_present", &[]),
        Some(1.0)
    );
    assert_eq!(
        sample(&families, "orbivisor_cluster_nodes", &[("state", "online")]),
        Some(1.0)
    );
    assert_eq!(
        sample(
            &families,
            "orbivisor_node_is_raft_leader",
            &[("node", "node1")]
        ),
        Some(1.0)
    );
    for severity in ["info", "warning", "critical"] {
        assert_eq!(
            sample(
                &families,
                "orbivisor_alerts_firing",
                &[("severity", severity)]
            ),
            Some(0.0),
            "{text}"
        );
    }
    let cpu = sample(
        &families,
        "orbivisor_node_cpu_usage_ratio",
        &[("node", "node1")],
    )
    .unwrap();
    assert!((0.0..=1.0).contains(&cpu), "{cpu}");
    assert!(
        sample(
            &families,
            "orbivisor_node_memory_total_bytes",
            &[("node", "node1")]
        )
        .is_some_and(|b| b > 0.0),
        "{text}"
    );

    // GitHub linux-kvm: promtool의 형식·이름 검사(`promtool check metrics`는 표준 입력을 본다)
    if let Some(promtool) = std::env::var_os("OPL_TEST_PROMTOOL") {
        use std::io::Write;
        let mut child = std::process::Command::new(promtool)
            .args(["check", "metrics"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run promtool");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "promtool check metrics failed:\n{}{}\n{text}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(feature = "qemu-it")]
    d.ctl(&["vm", "stop", "plain", "--force"]).await.unwrap();
    let out = d.ctl(&["prometheus", "off"]).await.unwrap();
    assert_eq!(out, "prometheus: disabled\n");
    let (status, _) = scrape(&d, Some(&viewer)).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}

/// VM 기록이 정해진 노드를 바꾼(멈춘 채 옮긴) VM을 따라 새 노드로 넘어간다.
#[cfg(feature = "qemu-it")]
#[tokio::test]
async fn vm_history_follows_the_vm_to_its_new_node() {
    let config = "[test]\nmetrics_flush_secs = 1\nmetrics_push_secs = 1\n";
    let mut a = Daemon::new("node1", config);
    let mut b = Daemon::new("node2", config);
    a.start().await;
    b.start().await;
    a.init("hist-move").await;
    a.login().await;
    let url = a.join_url.clone().unwrap();
    b.join(&url).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;
    let c = client(&a);
    eventually("both nodes online", || async {
        let nodes = c.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap();
        nodes.len() == 2 && nodes.iter().all(|n| n["status"]["online"] == true)
    })
    .await;

    a.ctl(&[
        "vm", "create", "plain", "--memory", "256", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    let vm = c.get("/vms/plain").await.unwrap();
    let id = vm["id"].as_str().unwrap().to_string();
    let history = || async {
        a.ctl_json(&[
            "vm", "stats", "plain", "--range", "hour", "--points", "1000",
        ])
        .await
    };
    eventually("two VM samples on node1", || async {
        known(&history().await, "cpu").len() >= 2
    })
    .await;
    a.ctl(&["vm", "stop", "plain", "--force"]).await.unwrap();
    // 멈춘 뒤 마지막 점까지 파일에 쓰인다.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let before = known(&history().await, "cpu");
    let old_file = a
        .data_dir()
        .join("metrics")
        .join("vm")
        .join(format!("{id}.rrd"));
    let new_file = b
        .data_dir()
        .join("metrics")
        .join("vm")
        .join(format!("{id}.rrd"));
    assert!(old_file.exists());

    // 정해진 노드를 바꿔 노드 2에서 켠다.
    let node2 = c.get("/nodes/node2").await.unwrap()["id"].clone();
    let mut spec = c.get("/vms/plain").await.unwrap()["spec"].clone();
    spec["node"] = node2;
    c.put("/vms/plain", &serde_json::json!({ "spec": spec }))
        .await
        .unwrap();
    let out = a.ctl(&["vm", "start", "plain"]).await.unwrap();
    assert!(out.contains("vm plain: start done"), "{out}");

    eventually("the history file moved to node2", || async {
        new_file.exists() && !old_file.exists()
    })
    .await;
    let log = a.log();
    assert!(
        log.contains("pushed metrics history to the VM's node"),
        "{log}"
    );
    // 기록 조회는 VM의 노드(노드 2)에 묻고, 노드 1 시절 점이 있다.
    let reply = history().await;
    let after = known(&reply, "cpu");
    for (t, v) in &before {
        assert!(
            after
                .iter()
                .any(|(t2, v2)| t2 == t && (v2 - v).abs() < 1e-3),
            "point {t} = {v} did not follow the VM: {reply}"
        );
    }
    a.ctl(&["vm", "stop", "plain", "--force"]).await.unwrap();
}

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다.
#[cfg(feature = "qemu-it")]
fn client(d: &Daemon) -> orbivisorctl::Client {
    let config: Value =
        serde_json::from_slice(&std::fs::read(d.path("oplctl.json")).unwrap()).unwrap();
    orbivisorctl::Client::new(
        config["server"].as_str().unwrap(),
        config["token"].as_str().map(str::to_string),
        config["ca_pem"].as_str().map(str::to_string),
    )
    .unwrap()
}
