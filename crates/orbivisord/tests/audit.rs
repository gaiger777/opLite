//! 계획 34: 노드 셋으로 만든 클러스터에서 각 노드에 바꾸는 요청을 보내면, 한 노드의
//! `oplctl -o json audit`가 두 노드의 기록을 함께 보인다. 감사 파일은 `{data_dir}/audit/audit.log`(0600)이고,
//! `[audit] enabled = false`인 노드는 파일을 만들지 않는다. QEMU는 쓰지 않는다.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::{Daemon, eventually};
use serde_json::{Value, json};

/// `entries`에 노드 `node`의 성공한 `action` 기록(admin, 토큰)이 있는지. `target`은 대상 JSON
fn has(entries: &Value, node: &str, action: &str, target: Value) -> bool {
    entries.as_array().unwrap().iter().any(|e| {
        e["node"] == node
            && e["action"] == action
            && e["target"] == target
            && e["user"] == "admin"
            && e["auth"] == "token"
            && e["status"].as_u64().is_some_and(|s| s < 300)
    })
}

#[tokio::test]
async fn audit_reads_the_records_of_every_node() {
    let mut a = Daemon::new("node-a", "");
    let mut b = Daemon::new("node-b", "");
    let mut c = Daemon::new("node-c", "[audit]\nenabled = false");
    for d in [&mut a, &mut b, &mut c] {
        d.start().await;
    }
    a.init("prod").await;
    let password = a.admin_password.clone().unwrap();
    let fp = a.ca_fingerprint.clone().unwrap();
    a.login().await;
    for d in [&mut b, &mut c] {
        let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
        d.join(url.trim()).await.unwrap();
        d.login_with(&password, &fp).await;
    }

    // 노드마다 다른 바꾸는 요청(oplctl은 로그인한 노드의 API로 보낸다)
    let pw = a.path("alice-password");
    std::fs::write(&pw, "alice-password-1").unwrap();
    let pw = pw.to_str().unwrap();
    a.ctl(&[
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
    // node-b가 사용자 생성을 적용한 뒤 지운다.
    eventually("node-b deletes alice", || async {
        b.ctl(&["user", "delete", "alice"]).await.is_ok()
    })
    .await;
    c.ctl(&["token", "create", "ci"]).await.unwrap();

    // 쓰기 스레드가 파일에 적은 뒤 읽힌다.
    eventually("records of node-a and node-b", || async {
        let shown = b.ctl_json(&["audit"]).await;
        has(&shown["entries"], "node-a", "POST /users", Value::Null)
            && has(
                &shown["entries"],
                "node-b",
                "DELETE /users/{key}",
                json!({ "kind": "users", "key": "alice" }),
            )
    })
    .await;
    let shown = b.ctl_json(&["audit", "--since", "1h"]).await;
    // 감사가 꺼진 노드는 읽지 못한 노드가 아니라 꺼진 노드다.
    assert_eq!(shown["unreachable"], json!([]), "{shown}");
    assert_eq!(shown["disabled"], json!(["node-c"]), "{shown}");
    assert_eq!(shown["truncated"], false, "{shown}");
    let entries = shown["entries"].as_array().unwrap();
    assert!(entries.iter().all(|e| e["node"] != "node-c"), "{shown}");
    // 새 것부터
    let times: Vec<u64> = entries
        .iter()
        .map(|e| e["time"].as_u64().unwrap())
        .collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "{times:?}");
    // 노드로 거르기
    let only_a = b.ctl_json(&["audit", "--node", "node-a"]).await;
    let only_a = only_a["entries"].as_array().unwrap();
    assert!(!only_a.is_empty() && only_a.iter().all(|e| e["node"] == "node-a"));
    // 표도 두 노드를 보인다.
    let table = a.ctl(&["audit", "--user", "admin"]).await.unwrap();
    assert!(table.starts_with("TIME"), "{table}");
    assert!(
        table.contains("node-a") && table.contains("node-b"),
        "{table}"
    );

    for d in [&a, &b] {
        let file = d.data_dir().join("audit").join("audit.log");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", file.display());
    }
    assert!(!c.data_dir().join("audit").exists());
}
