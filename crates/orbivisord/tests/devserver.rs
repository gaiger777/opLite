//! 개발 서버(계획 73 §10.3): 연결 줄 하나(JSON)를 쓰고, 그 토큰·CA로 HTTPS API가 답하며(노드 하나가 온라인, SDN을 세운 것처럼
//! 적어 SDN 네트워크 계획이 막히지 않음, 심은 풀·ACME 계정), 매니페스트를 적용하면 VM이 켜짐까지 가고, 표준 입력을 닫으면
//! 끝나며 임시 디렉터리를 지운다. Terraform 수용 시험(`terraform/`)이 같은 길로 띄운다.
//! `cargo test -p orbivisord --features devserver --test devserver`
#![cfg(feature = "devserver")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn http(ca: &str) -> reqwest::Client {
    reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(ca.as_bytes()).unwrap())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

#[tokio::test]
async fn devserver_prints_a_connection_line_serves_the_api_and_ends_with_stdin() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orbivisor-devserver"))
        .arg("--exit-on-stdin-eof")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let info: Value = serde_json::from_str(&line).unwrap();
    let url = info["url"].as_str().unwrap();
    let token = info["token"].as_str().unwrap();
    let ca = info["ca_pem"].as_str().unwrap();
    let dir = std::path::PathBuf::from(info["dir"].as_str().unwrap());
    assert!(url.starts_with("https://127.0.0.1:"), "{url}");
    assert!(
        info["ca_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(dir.is_dir());
    let http = http(ca);
    let get = |path: &str| {
        http.get(format!("{url}/api/v1{path}"))
            .bearer_auth(token)
            .send()
    };

    let nodes: Value = get("/nodes").await.unwrap().json().await.unwrap();
    let node = &nodes["items"][0];
    assert_eq!(node["name"], "dev1");
    assert_eq!(node["status"]["online"], true);
    for pool in ["local", "backup"] {
        let r = get(&format!("/storage-pools/{pool}")).await.unwrap();
        assert!(r.status().is_success(), "{pool}: {}", r.status());
    }
    assert!(
        get("/acme-accounts/dev")
            .await
            .unwrap()
            .status()
            .is_success()
    );
    // 토큰 없이는 받지 않는다.
    let r = http
        .get(format!("{url}/api/v1/nodes"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let doc = |kind: &str, name: &str, spec: Value| json!({ "apiVersion": "orbivisor.io/v1", "kind": kind, "metadata": { "name": name }, "spec": spec });
    let plan: Value = http
        .post(format!("{url}/api/v1/apply?dry_run=true"))
        .bearer_auth(token)
        .json(&json!({ "documents": [doc("Network", "app", json!({ "sdn": {}, "subnets": [{ "cidr": "10.1.0.0/24" }] }))] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan["steps"][0]["action"], "create", "{plan}");

    let mut vm = doc(
        "Vm",
        "web",
        json!({ "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt",
                "disks": [{ "volume": "web-root", "bus": "virtio-blk" }],
                "nics": [{ "network": "user", "model": "virtio" }] }),
    );
    vm["state"] = json!("running");
    let started: Value = http
        .post(format!("{url}/api/v1/apply"))
        .bearer_auth(token)
        .json(&json!({ "documents": [doc("Volume", "web-root", json!({ "pool": "local", "size_bytes": 1 << 20 })), vm] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task = started["task"]["id"]
        .as_str()
        .expect("apply task")
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let t: Value = get(&format!("/tasks/{task}"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        match t["status"]["state"].as_str() {
            Some("succeeded") => break,
            Some("failed") => panic!("apply failed: {t}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "apply did not finish: {t}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let v: Value = get("/vms/web").await.unwrap().json().await.unwrap();
        if v["status"]["phase"] == "running" {
            break;
        }
        assert!(Instant::now() < deadline, "the VM did not start: {v}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "the dev server did not end");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(status.success(), "{status}");
    assert!(!dir.exists(), "the temporary directory is left");
}
