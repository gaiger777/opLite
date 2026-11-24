//! 웹 VM 빌더의 CLI 미리 보기 교차 시험(계획 71 설계 §7.6): 웹 시험이 쓴 고정 파일
//! `web/src/vmBuilder/model/__fixtures__/cli.json`의 명령(셸을 거치지 않는 인자 배열)을 진짜 CLI(`run`)로 가짜 API에
//! 보내, 받은 `POST /vms`(템플릿은 `POST /vms/{key}/clone`) 본문이 웹이 보낼 본문과 같은지 본다. 플래그나 본문이
//! 바뀌면 한쪽 시험이 깨진다.
//!
//! 맞춰 보기 전에 고르게 하는 것(설계 §18 스테이지 2): CLI는 `power`를 보내지 않고(꺼진 채 만들고 `--start`면
//! `vm start`), HA 정책의 서버 기본값을 적지 않고, `--boot`가 없으면 첫 디스크만 `boot_order: 1`이고(웹은 모두 비워
//! "디스크 차례, CD는 뒤"), `host` 모델은 보내지 않고, 게스트 에이전트 기본은 `{}`(웹은 `{freeze_fs: true}`)이며,
//! 배포의 노드는 준 이름 그대로(웹은 id — 서버가 둘 다 받음)이고, 값이 null인 칸은 서버 기본과 같다.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::fake_api::fake_api;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../web/src/vmBuilder/model/__fixtures__/cli.json"
);

/// 웹 시험의 폴더 id(`web/src/test/fixtures.ts` `folderId`)
fn folder_id(name: &str) -> String {
    format!("019a0f3c-0000-7000-8009-{name:0>12}")
}

/// 고정 파일의 세상을 답하는 가짜 API: 노드(id → 이름), 폴더 `/team/prod`, 템플릿 `tmpl`, 끝난 Task, 만든 것
fn reply(
    nodes: &Map<String, Value>,
    template: &str,
) -> impl Fn(&str, &str) -> (u16, Value) + Send + Sync + use<> {
    let nodes: Vec<Value> = nodes
        .iter()
        .map(|(id, name)| json!({ "id": id, "name": name, "labels": {}, "spec": {}, "status": {} }))
        .collect();
    let folders = json!([
        { "id": folder_id("team"), "name": folder_id("team"), "spec": { "name": "team", "kind": "vm" } },
        { "id": folder_id("prod"), "name": folder_id("prod"),
          "spec": { "name": "prod", "kind": "vm", "parent": folder_id("team") } },
    ]);
    let template = template.to_string();
    let task = json!({ "id": "t1", "spec": { "node": "n1" },
        "status": { "state": "succeeded", "error": null } });
    move |method: &str, path: &str| {
        let path = path.split('?').next().unwrap_or(path);
        match (method, path) {
            ("GET", "/api/v1/nodes") => (200, json!({ "revision": 1, "items": nodes })),
            ("GET", p) if p.starts_with("/api/v1/nodes/") => {
                let key = p.trim_start_matches("/api/v1/nodes/");
                match nodes.iter().find(|n| n["name"] == key || n["id"] == key) {
                    Some(n) => (200, n.clone()),
                    None => (404, json!({ "code": "not_found", "message": key })),
                }
            }
            ("GET", "/api/v1/folders") => (200, json!({ "revision": 1, "items": folders })),
            ("GET", p) if p.starts_with("/api/v1/tasks/") => (200, task.clone()),
            // 웹 시험의 풀은 디렉터리·NFS라 형식은 qcow2다.
            ("GET", p) if p.starts_with("/api/v1/storage-pools/") => (
                200,
                json!({ "name": p.trim_start_matches("/api/v1/storage-pools/"),
                    "spec": { "backend": { "kind": "dir", "path": "/srv" }, "nodes": [] }, "status": {} }),
            ),
            ("GET", p) if p.starts_with("/api/v1/vms/") => {
                let name = p.trim_start_matches("/api/v1/vms/");
                let id = if name == "tmpl" {
                    template.as_str()
                } else {
                    name
                };
                (
                    200,
                    json!({ "id": id, "name": name, "revision": 1, "labels": {},
                        "spec": { "power": "running", "ha": false },
                        "status": { "phase": "running", "node": "n1" } }),
                )
            }
            ("POST", "/api/v1/volumes") | ("POST", "/api/v1/vms/tmpl/clone") => (202, task.clone()),
            ("POST", p) if p.ends_with("/start") => (202, task.clone()),
            ("POST", "/api/v1/placement-rules") | ("POST", "/api/v1/vms") => {
                (201, json!({ "id": "x", "name": "x", "revision": 1 }))
            }
            _ => (
                404,
                json!({ "code": "not_found", "message": format!("{method} {path}") }),
            ),
        }
    }
}

/// 값이 null인 칸을 뺀다(서버 기본과 같다).
fn drop_nulls(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.retain(|_, x| !x.is_null());
            m.values_mut().for_each(drop_nulls);
        }
        Value::Array(a) => a.iter_mut().for_each(drop_nulls),
        _ => {}
    }
}

/// 웹과 CLI 본문의 문서로 정한 차이를 고르게 한다(맨 위 설명).
fn normalise(web: &mut Value, cli: &mut Value, argv: &[String]) {
    for body in [&mut *web, &mut *cli] {
        if let Some(spec) = body.get_mut("spec").and_then(Value::as_object_mut) {
            spec.remove("power");
            if spec["cpu"]["model"] == "host" {
                spec["cpu"].as_object_mut().unwrap().remove("model");
            }
            if spec.get("guest_agent") == Some(&json!({ "freeze_fs": true })) {
                spec.insert("guest_agent".into(), json!({}));
            }
            // HA 정책의 서버 기본값(웹 `DEFAULT_HA`)은 CLI가 적지 않는다.
            if let Some(ha) = spec.get_mut("ha").and_then(Value::as_object_mut) {
                for (k, v) in [
                    ("state", json!("started")),
                    ("max_restart", json!(1)),
                    ("max_relocate", json!(1)),
                    ("priority", json!(50)),
                ] {
                    if ha.get(k) == Some(&v) {
                        ha.remove(k);
                    }
                }
            }
        }
        drop_nulls(body);
    }
    // 웹은 부팅 순서를 비워 둔다(서버 기본: 디스크 차례, CD는 뒤). CLI는 `--boot`가 없으면 첫 디스크만 1이고, 있으면
    // 그 기본과 같은 차례를 적는다.
    let web_orders = ["disks", "cdroms"]
        .iter()
        .flat_map(|k| web["spec"][k].as_array().cloned().unwrap_or_default())
        .any(|d| d.get("boot_order").is_some());
    if !web_orders && cli.get("spec").is_some() {
        let boot = argv.iter().any(|a| a == "--boot");
        let mut n = 0;
        for k in ["disks", "cdroms"] {
            let list = cli["spec"].get_mut(k).and_then(Value::as_array_mut);
            for d in list.into_iter().flatten() {
                n += 1;
                let want = if boot || n == 1 {
                    json!(n)
                } else {
                    Value::Null
                };
                let got = d.as_object_mut().unwrap().remove("boot_order");
                assert_eq!(got.unwrap_or(Value::Null), want, "CLI boot order of {k}");
            }
        }
    }
}

/// 인자 배열의 파일 이름(고정 파일의 `files`)을 임시 폴더의 경로로 바꾼다.
fn with_files(argv: &[Value], files: &[Value], dir: &Path) -> Vec<String> {
    argv.iter()
        .map(|a| {
            let a = a.as_str().unwrap();
            if files.iter().any(|f| f["name"] == a) {
                dir.join(a).to_string_lossy().to_string()
            } else {
                a.to_string()
            }
        })
        .collect()
}

#[tokio::test]
async fn web_cli_preview_sends_the_same_body_as_the_web() {
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    let nodes = fixture["nodes"].as_object().unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 12, "fixture has {} cases", cases.len());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("oplctl.json");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let files = case["files"].as_array().unwrap();
        for f in files {
            std::fs::write(
                dir.path().join(f["name"].as_str().unwrap()),
                f["content"].as_str().unwrap(),
            )
            .unwrap();
        }
        let web = &case["request"];
        let template = web["key"].as_str().unwrap_or("");
        let (client, seen) = fake_api(reply(nodes, template)).await;
        let mut last = Vec::new();
        for command in case["commands"].as_array().unwrap() {
            let mut argv = with_files(command.as_array().unwrap(), files, dir.path());
            argv.splice(
                1..1,
                [
                    "--server".to_string(),
                    client.base.clone(),
                    "--config".to_string(),
                    config.to_string_lossy().to_string(),
                ],
            );
            let mut out = Vec::new();
            crate::run(&argv, &mut out)
                .await
                .unwrap_or_else(|e| panic!("{name}: {argv:?}: {e:#}"));
            last = argv;
        }
        let seen = seen.lock().unwrap().clone();
        // 웹의 마지막 요청(VM 만들기·배포)과 같은 요청을 CLI가 보냈다.
        let want = match web["path"].as_str().unwrap() {
            "/vms" => "POST /api/v1/vms HTTP/1.1".to_string(),
            "/vms/{key}/clone" => "POST /api/v1/vms/tmpl/clone HTTP/1.1".to_string(),
            other => panic!("{name}: unexpected web request {other}"),
        };
        let sent = seen.iter().find(|s| s.line == want).unwrap_or_else(|| {
            panic!(
                "{name}: no {want} in {:?}",
                seen.iter().map(|s| &s.line).collect::<Vec<_>>()
            )
        });
        let mut cli = sent.body.clone();
        let mut body = web["body"].clone();
        normalise(&mut body, &mut cli, &last);
        // 배포 본문의 `node`는 이름이나 id다(서버가 둘 다 받음): 웹은 id, CLI는 준 이름 그대로다.
        if web["path"] == "/vms/{key}/clone"
            && let Some(id) = body["node"].as_str()
        {
            body["node"] = nodes[id].clone();
        }
        assert_eq!(cli, body, "{name}: CLI body differs from the web body");
    }
}
