//! Prometheus 끝점 `GET /api/v1/metrics`(계획 60 §10): 기본은 꺼져 404, 토큰이 없으면 401, `PATCH /settings`의
//! `prometheus`로 켜고 끄기(기능 게이트·감사), viewer 토큰으로 긁은 본문이 엄격한 텍스트 형식 파서를 통과하고 이 노드의
//! 노드·VM·풀·클러스터 값을 담는다. 파서 자체가 나쁜 본문을 거절하는지도 본다.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::promtext::{parse, samples};
use common::{Env, WAIT, mark_pool_ready, vm_request};
use opl_peer::{MetricsReply, MetricsSource};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::clock::{Clock, SystemClock};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample, VmSeries};
use opl_types::node::{FEATURES, METRICS_HISTORY, NodeSpec};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

/// 지금 시각의 점 하나를 돌려준다(노드 CPU 50 %, VM CPU 25 %).
struct NowMetrics {
    node: Uuid,
}

impl MetricsSource for NowMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        let t = SystemClock.now_ms() - 1_000;
        match q.scope {
            MetricsScope::Nodes => MetricsReply {
                node: Some(NodeSeries {
                    node: self.node,
                    samples: vec![NodeSample {
                        t,
                        cpu: Some(50.0),
                        mem_used_mib: Some(1024),
                        mem_total_mib: Some(2048),
                        disk_read_bps: None,
                        disk_write_bps: None,
                        net_rx_bps: Some(100.0),
                        net_tx_bps: Some(200.0),
                        fs_used_mib: Some(1),
                        fs_total_mib: Some(2),
                    }],
                }),
                vms: vec![],
            },
            MetricsScope::Vm { id } => MetricsReply {
                node: None,
                vms: vec![VmSeries {
                    vm: id,
                    node: self.node,
                    samples: vec![VmSample {
                        t,
                        cpu: Some(25.0),
                        mem_rss_mib: Some(300),
                        disk_read_bps: None,
                        disk_write_bps: None,
                        disk_read_iops: None,
                        disk_write_iops: None,
                        net_rx_bps: None,
                        net_tx_bps: None,
                    }],
                }],
            },
        }
    }
}

async fn start() -> Env {
    Env::start_with_state(|state| {
        state.metrics = Some(Arc::new(NowMetrics { node: state.node }));
    })
    .await
}

/// 본문, 상태, Content-Type
async fn scrape(base: &str, token: Option<&str>) -> (StatusCode, String, String) {
    let mut r = reqwest::Client::new().get(format!("{base}/api/v1/metrics"));
    if let Some(t) = token {
        r = r.bearer_auth(t);
    }
    let resp = r.send().await.unwrap();
    let status = resp.status();
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    (status, resp.text().await.unwrap(), ct)
}

/// `action`이 같은 감사 항목(`n`개 이상 적힐 때까지 기다림)의 설명, 새것부터
async fn audit_details(env: &Env, action: &str, n: usize) -> Vec<Option<String>> {
    let start = Instant::now();
    loop {
        let entries: Vec<AuditEntry> = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: Some(action.into()),
                limit: 1000,
            })
            .await
            .unwrap();
        if entries.len() >= n {
            return entries.into_iter().map(|e| e.detail).collect();
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_endpoint_is_off_until_turned_on_and_needs_a_token() {
    let env = start().await;
    let admin = env.admin().await;
    let viewer = env.user("vera", "viewer").await;
    let token = viewer.token.clone().unwrap();

    let (status, _, _) = scrape(&env.base, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body, _) = scrape(&env.base, Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("\"code\":\"not_found\""), "{body}");

    // viewer는 설정을 바꾸지 못한다.
    let (status, _) = viewer
        .patch(
            "/settings",
            json!({ "prometheus": { "enabled": true } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "prometheus": { "enabled": true } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["prometheus"], json!({ "enabled": true }));

    let (status, text, ct) = scrape(&env.base, Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(ct, "text/plain; version=0.0.4; charset=utf-8");
    let families = parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let cpu = samples(&families, "orbivisor_node_cpu_usage_ratio");
    assert_eq!(cpu.len(), 1);
    assert_eq!(cpu[0].0["node"], "node1");
    assert_eq!(cpu[0].1, 0.5);
    assert_eq!(
        samples(&families, "orbivisor_node_memory_total_bytes")[0].1,
        2048.0 * 1048576.0
    );
    assert!(samples(&families, "orbivisor_node_disk_read_bytes_per_second").is_empty());
    assert_eq!(
        samples(&families, "orbivisor_cluster_leader_present")[0].1,
        1.0
    );
    assert_eq!(
        samples(&families, "orbivisor_node_is_raft_leader")[0].1,
        1.0
    );
    let nodes = samples(&families, "orbivisor_cluster_nodes");
    assert_eq!(nodes.len(), 3);
    assert_eq!(nodes[0].0["state"], "online");
    assert_eq!(nodes[0].1, 1.0);
    for f in &families {
        assert_eq!(f.kind.as_deref(), Some("gauge"), "{}", f.name);
        assert!(f.help.is_some(), "{}", f.name);
        // 레이블은 낮은 카디널리티의 것뿐이다.
        for (labels, _) in &f.samples {
            for k in labels.keys() {
                assert!(
                    ["node", "vm_id", "vm", "pool", "state", "phase", "severity"]
                        .contains(&k.as_str()),
                    "{}: {k}",
                    f.name
                );
                assert!(k != "vm" || f.name == "orbivisor_vm_info", "{}", f.name);
            }
        }
    }

    // 끄면 다시 404다(캐시와 상관없이).
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "prometheus": { "enabled": false } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("prometheus").is_none(), "{body}");
    let (status, _, _) = scrape(&env.base, Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 새것부터, viewer의 403에는 설명이 없다.
    assert_eq!(
        audit_details(&env, "PATCH /settings", 3).await,
        [
            Some("settings prometheus off".to_string()),
            Some("settings prometheus on".to_string()),
            None
        ]
    );
}

#[tokio::test]
async fn vms_and_pools_of_this_node_are_exported() {
    let env = start().await;
    let admin = env.admin().await;
    let (status, _) = admin
        .patch(
            "/settings",
            json!({ "prometheus": { "enabled": true } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, pool) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": "/srv/pool" } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{pool}");
    mark_pool_ready(&env.cluster, env.node, "local").await;

    let (status, text, _) = scrape(&env.base, admin.token.as_deref()).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let families = parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let id = vm["id"].as_str().unwrap();
    let info = samples(&families, "orbivisor_vm_info");
    assert_eq!(info.len(), 1, "{text}");
    assert_eq!(info[0].0["vm_id"], id);
    assert_eq!(info[0].0["vm"], "web");
    assert_eq!(info[0].0["node"], "node1");
    let cpu = samples(&families, "orbivisor_vm_cpu_usage_ratio");
    assert_eq!(cpu[0].0.len(), 1);
    assert_eq!(cpu[0].0["vm_id"], id);
    assert_eq!(cpu[0].1, 0.25);
    assert_eq!(
        samples(&families, "orbivisor_vm_memory_rss_bytes")[0].1,
        300.0 * 1048576.0
    );
    let running: Vec<_> = samples(&families, "orbivisor_cluster_vms")
        .iter()
        .filter(|(l, _)| l["phase"] == "running")
        .collect();
    assert_eq!(running[0].1, 1.0);
    assert_eq!(
        samples(&families, "orbivisor_pool_total_bytes")[0].0["pool"],
        "local"
    );
}

#[tokio::test]
async fn turning_it_on_needs_every_node_to_know_metrics_history() {
    let env = start().await;
    let admin = env.admin().await;
    let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    me.status.features = FEATURES
        .iter()
        .filter(|f| **f != METRICS_HISTORY)
        .map(|f| f.to_string())
        .collect();
    env.cluster
        .update_status::<NodeSpec>(env.node, &me.status)
        .await
        .unwrap();
    // 모든 노드가 metrics-history를 알아야 켜고 끈다. 모르는 칸은 본문을 풀 때 400이다.
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "prometheus": { "enabled": true } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    let (status, body) = admin
        .patch("/settings", json!({ "prometheus": { "on": true } }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[test]
fn the_parser_rejects_what_prometheus_would_choke_on() {
    let ok = "# HELP a_b x\n# TYPE a_b gauge\na_b{l=\"q\\\"\\\\\\n\"} 1\na_b{l=\"2\"} -0.5 123\n";
    let f = parse(ok).unwrap();
    assert_eq!(f[0].samples[0].0["l"], "q\"\\\n");
    assert_eq!(f[0].samples[1].1, -0.5);
    for bad in [
        "a 1",
        "a 1\n\n",
        "1a 1\n",
        "a{__x=\"1\"} 1\n",
        "a{l=\"1\",l=\"2\"} 1\n",
        "a{l=\"\\t\"} 1\n",
        "a{l=1} 1\n",
        "a{l=\"1\",} 1\n",
        "a one\n",
        "a 1 2 3\n",
        "a 1\na 2\n",
        "a 1\nb 1\na{x=\"1\"} 1\n",
        "a 1\n# TYPE a gauge\n",
        "# TYPE a gauge\n# TYPE a gauge\n",
        "# TYPE a gauges\n",
        "# HELP a x\n# HELP a y\n",
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}
