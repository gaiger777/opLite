//! 계획 37: 한 노드짜리 opld 클러스터 둘. 한쪽(prod)에 다른 쪽(dr)을 원격으로 등록하면(dr에서 만든 API
//! 토큰, 받은 CA 지문을 `--yes`로 믿음) prod의 리더가 dr의 상태를 모으고, `oplctl remote vms`가 prod를
//! 거쳐 dr의 VM을 읽는다. dr이 멈추면 상태가 닿지 않음으로 바뀐다. QEMU는 쓰지 않는다.

mod common;

use common::{Daemon, eventually};
use serde_json::Value;

/// 수집 틱과 간격을 짧게
const REMOTES: &str = "[remotes]\ntick = \"1s\"\ninterval = \"1s\"";

/// `oplctl -o json remote list`의 첫 원격 상태
async fn remote_status(d: &Daemon) -> Value {
    d.ctl_json(&["remote", "list"]).await[0]["status"].clone()
}

#[tokio::test]
async fn a_remote_cluster_is_registered_polled_and_proxied() {
    let mut prod = Daemon::new("node-a", REMOTES);
    let mut dr = Daemon::new("node-b", REMOTES);
    prod.start().await;
    dr.start().await;
    prod.init("prod").await;
    dr.init("dr").await;
    prod.login().await;
    dr.login().await;

    // dr에 꺼진 VM 하나와 prod가 쓸 API 토큰
    dr.ctl(&["vm", "create", "web", "--memory", "256"])
        .await
        .unwrap();
    let token = dr.ctl(&["token", "create", "prod"]).await.unwrap();
    let token_file = prod.path("dr-token");
    std::fs::write(&token_file, token).unwrap();
    let token_file = token_file.to_str().unwrap();

    // 자기 클러스터는 등록하지 못한다.
    let own = prod.server();
    let e = prod
        .ctl(&[
            "remote",
            "add",
            "self",
            "--url",
            &own,
            "--ca-fingerprint",
            prod.ca_fingerprint.as_deref().unwrap(),
            "--token-file",
            token_file,
        ])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("remote_is_local"), "{e}");

    // 지문을 주지 않으면 받은 지문을 보이고 `--yes`로 믿는다.
    let url = dr.server();
    let out = prod
        .ctl(&[
            "remote",
            "add",
            "dr",
            "--url",
            &url,
            "--token-file",
            token_file,
            "--yes",
        ])
        .await
        .unwrap();
    let fp = dr.ca_fingerprint.as_deref().unwrap();
    assert_eq!(
        out,
        format!("the remote cluster CA fingerprint is {fp}\nremote dr added (reachable)\n")
    );

    let status = remote_status(&prod).await;
    assert_eq!(status["reachable"], true, "{status}");
    assert_eq!(status["cluster_name"], "dr", "{status}");
    assert!(status["version"].is_string(), "{status}");
    assert_eq!(status["nodes"]["total"], 1, "{status}");
    assert_eq!(status["nodes"]["online"], 1, "{status}");
    assert_eq!(status["vms"]["total"], 1, "{status}");
    assert_eq!(status["vms"]["running"], 0, "{status}");
    let list = prod.ctl(&["remote", "list"]).await.unwrap();
    let row = list.lines().nth(1).unwrap();
    let cells: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cells[..2], ["dr", "dr"], "{list}");
    assert_eq!(cells[3..], ["1/1", "0/1", "reachable"], "{list}");

    // prod를 거쳐 dr의 VM을 읽고 동작을 요청한다.
    let vms = prod.ctl(&["remote", "vms", "dr"]).await.unwrap();
    assert!(vms.starts_with("NAME"), "{vms}");
    let web = vms.lines().find(|l| l.starts_with("web ")).unwrap();
    assert!(web.contains("stopped"), "{vms}");
    let out = prod
        .ctl(&["remote", "vm", "dr", "stop", "web"])
        .await
        .unwrap();
    assert!(
        out.starts_with("vm web on remote dr: stop requested (task "),
        "{out}"
    );

    // dr이 멈추면 리더의 수집기가 닿지 않음을 적는다.
    dr.stop();
    eventually("the remote becomes unreachable", || async {
        remote_status(&prod).await["reachable"] == false
    })
    .await;
    let list = prod.ctl(&["remote", "list"]).await.unwrap();
    assert!(list.contains("unreachable: "), "{list}");
}
