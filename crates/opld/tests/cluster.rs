//! M6 완료 기준: opld 프로세스 3개로 클러스터를 만들고, 아무 노드로나 쓰고, 리더가 죽어도 쓰기가
//! 이어지고, 죽은 노드를 빼고, 노드가 떠났다가 다시 합류한다. QEMU는 쓰지 않는다.
//! 계획 22: 일반 노드 둘과 위트니스로 만든 클러스터는 리더를 잃어도 남은 일반 노드가 리더가 된다.
//! 위트니스는 다시 시작해도 위트니스이고, 위트니스만 남기는 탈퇴는 거부한다.

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

/// `oplctl cluster status`에서 리더의 이름. 리더가 없거나 답하지 못하면 `None`
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

#[tokio::test]
async fn a_witness_votes_but_never_leads_or_runs_vms() {
    let mut nodes = vec![Daemon::new("node-a", ""), Daemon::new("node-b", "")];
    // 위트니스의 qemu_binary는 없는 파일이다 — QEMU 없이 돈다.
    let mut w = Daemon::witness("wit");
    for d in &mut nodes {
        d.start().await;
    }
    w.start().await;

    // 위트니스는 클러스터를 만들 수 없다.
    let socket = w.socket();
    let err = w
        .ctl(&["cluster", "init", "--name", "prod", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("a witness cannot start a cluster"),
        "{err}"
    );

    nodes[0].init("prod").await;
    let password = nodes[0].admin_password.clone().unwrap();
    let fp = nodes[0].ca_fingerprint.clone().unwrap();
    nodes[0].login().await;
    let url = nodes[0].join_url.clone().unwrap();
    nodes[1].join(&url).await.unwrap();
    nodes[1].login_with(&password, &fp).await;
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    let out = w.join(url.trim()).await.unwrap();
    assert!(out.contains("node wit joined cluster prod"), "{out}");
    // 위트니스는 HTTPS API를 열지 않는다.
    assert!(std::net::TcpStream::connect(("127.0.0.1", w.port)).is_err());

    // 투표 멤버 셋 중 하나가 위트니스다. 일반 노드가 둘이니 경고가 없다.
    assert_eq!(voters(&nodes[0]).await, 3);
    eventually("all members online", || async {
        members(&nodes[0]).await.iter().all(|m| m["online"] == true)
    })
    .await;
    let roles: Vec<(String, bool)> = members(&nodes[0])
        .await
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().to_string(),
                m["witness"] == true,
            )
        })
        .collect();
    assert!(roles.contains(&("wit".into(), true)), "{roles:?}");
    assert_eq!(roles.iter().filter(|(_, w)| *w).count(), 1, "{roles:?}");
    let status = nodes[0].ctl(&["cluster", "status"]).await.unwrap();
    assert!(!status.contains("warning"), "{status}");
    let node = nodes[0].ctl_json(&["node", "get", "wit"]).await;
    assert_eq!(node["spec"]["witness"], true, "{node}");

    // 위트니스에 VM을 둘 수 없다.
    let err = nodes[0]
        .ctl(&["vm", "create", "web", "--memory", "256", "--node", "wit"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("node_unavailable"), "{err}");

    // 리더(일반 노드)를 kill -9 하면 남은 일반 노드가 리더가 되고 쓰기가 이어진다.
    let leader = leader_name(&nodes[0]).await.unwrap();
    assert_ne!(leader, "wit");
    let dead = nodes.iter().position(|d| d.name == leader).unwrap();
    nodes[dead].kill();
    let alive = &nodes[1 - dead];
    eventually("the other node leads", || async {
        leader_name(alive).await.as_deref() == Some(alive.name.as_str())
    })
    .await;
    let pool = alive.path("pool-after");
    std::fs::create_dir_all(&pool).unwrap();
    alive
        .ctl(&["pool", "create", "after", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();

    // 다시 시작한 위트니스는 위트니스 멤버로 돌아와 온라인이 된다(멈춘 동안은 과반수가 없다).
    w.stop();
    w.start().await;
    let witness_online = || async {
        members(alive).await.iter().any(|m| {
            m["name"] == "wit" && m["voter"] == true && m["witness"] == true && m["online"] == true
        })
    };
    eventually("the restarted witness is online", witness_online).await;

    // 죽은 노드를 빼면 투표 멤버는 일반 노드 하나와 위트니스다. 일반 노드는 빠질 수 없다(위트니스만
    // 남으면 리더가 생기지 않는다). 위트니스는 빠질 수 있다.
    eventually("the killed node is offline", || async {
        members(alive)
            .await
            .iter()
            .any(|m| m["name"] == leader.as_str() && m["online"] == false)
    })
    .await;
    alive
        .ctl(&["cluster", "remove-node", &leader, "--force"])
        .await
        .unwrap();
    assert_eq!(voters(alive).await, 2);
    let socket = alive.socket();
    let err = alive
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("all witnesses"), "{err}");
    let socket = w.socket();
    let out = w
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap();
    assert!(out.contains("left the cluster"), "{out}");
    assert_eq!(voters(alive).await, 1);

    nodes[1 - dead].stop();
    w.stop();
}
