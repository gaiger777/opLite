//! M6 완료 기준: opld 프로세스 3개로 클러스터를 만들고, 아무 노드로나 쓰고, 리더가 죽어도 쓰기가
//! 이어지고, 죽은 노드를 빼고, 노드가 떠났다가 다시 합류한다. QEMU는 쓰지 않는다.

mod common;

use common::{Daemon, eventually};
use serde_json::Value;

/// `oplctl cluster status`의 멤버
async fn members(d: &Daemon) -> Vec<Value> {
    d.ctl_json(&["cluster", "status"]).await["members"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn voters(d: &Daemon) -> usize {
    members(d)
        .await
        .iter()
        .filter(|m| m["voter"] == true)
        .count()
}

#[tokio::test]
async fn three_processes_form_a_cluster_that_survives_losing_a_node() {
    let mut nodes = vec![
        Daemon::new("node1", ""),
        Daemon::new("node2", ""),
        Daemon::new("node3", ""),
    ];
    for d in &mut nodes {
        d.start().await;
    }
    nodes[0].init("prod").await;
    let password = nodes[0].admin_password.clone().unwrap();
    let fp = nodes[0].ca_fingerprint.clone().unwrap();
    nodes[0].login().await;

    // node2는 init이 준 URL로, node3은 새로 만든 URL로 합류한다.
    let url = nodes[0].join_url.clone().unwrap();
    let out = nodes[1].join(&url).await.unwrap();
    assert!(out.contains("node node2 joined cluster prod"), "{out}");
    // 쓴 조인 URL은 다시 쓸 수 없다.
    let err = nodes[2].join(&url).await.unwrap_err();
    assert!(err.to_string().contains("invalid or expired"), "{err}");
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    nodes[2].join(url.trim()).await.unwrap();
    for d in &nodes[1..] {
        d.login_with(&password, &fp).await;
    }
    assert_eq!(voters(&nodes[0]).await, 3);
    eventually("all nodes online", || async {
        members(&nodes[0]).await.iter().all(|m| m["online"] == true)
    })
    .await;

    // 아무 노드로나 쓰고 다른 노드에서 읽는다(팔로워는 리더로 전달한다).
    for (i, d) in nodes.iter().enumerate() {
        let pool = d.path("pool");
        std::fs::create_dir_all(&pool).unwrap();
        d.ctl(&[
            "pool",
            "create",
            &format!("pool{i}"),
            "--path",
            pool.to_str().unwrap(),
        ])
        .await
        .unwrap();
    }
    let pools = nodes[2].ctl_json(&["pool", "list"]).await;
    assert_eq!(pools.as_array().unwrap().len(), 3, "{pools}");

    // 리더를 kill -9 해도 남은 두 노드로 쓰기가 이어진다.
    let view = nodes[0].ctl_json(&["cluster", "status"]).await;
    let leader = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == view["leader"])
        .unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string();
    let dead = nodes.iter().position(|d| d.name == leader).unwrap();
    nodes[dead].kill();
    let alive: Vec<usize> = (0..3).filter(|i| *i != dead).collect();
    let (a, b) = (alive[0], alive[1]);
    let pool = nodes[a].path("pool-after");
    std::fs::create_dir_all(&pool).unwrap();
    nodes[a]
        .ctl(&["pool", "create", "after", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    // 다른 노드에는 조금 늦게 반영될 수 있다(각 노드는 자기 상태에서 읽는다).
    eventually("node b sees the pool", || async {
        nodes[b].ctl(&["pool", "get", "after"]).await.is_ok()
    })
    .await;

    // 죽은 노드는 오프라인이 되고, 그 뒤에야 뺄 수 있다.
    let err = nodes[a]
        .ctl(&["cluster", "remove-node", &leader])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("--force"), "{err}");
    eventually("the killed node is offline", || async {
        members(&nodes[a])
            .await
            .iter()
            .any(|m| m["name"] == leader.as_str() && m["online"] == false)
    })
    .await;
    nodes[a]
        .ctl(&["cluster", "remove-node", &leader, "--force"])
        .await
        .unwrap();
    assert_eq!(voters(&nodes[a]).await, 2);
    let status = nodes[a].ctl(&["cluster", "status"]).await.unwrap();
    assert!(status.contains("two-node cluster"), "{status}");

    // 남은 노드 하나가 떠났다가 새 토큰으로 다시 합류한다.
    let socket = nodes[b].socket();
    let out = nodes[b]
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap();
    assert!(out.contains("left the cluster"), "{out}");
    assert_eq!(voters(&nodes[a]).await, 1);
    assert!(!nodes[b].data_dir().join("pki").exists());
    let url = nodes[a].ctl(&["cluster", "join-token"]).await.unwrap();
    nodes[b].join(url.trim()).await.unwrap();
    assert_eq!(voters(&nodes[a]).await, 2);
    // 다시 합류한 노드도 그동안의 기록을 받는다.
    nodes[b].login_with(&password, &fp).await;
    nodes[b].ctl(&["pool", "get", "after"]).await.unwrap();

    nodes[a].stop();
    nodes[b].stop();
}
