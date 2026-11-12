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
    // API 노드 목록에 위트니스는 없고, 두 일반 노드의 API는 준비됐다.
    let endpoints = nodes[0].ctl_json(&["cluster", "api-endpoints"]).await;
    let names: Vec<&str> = endpoints
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["node-a", "node-b"]);
    for d in &nodes {
        eventually("ready", || async { health(d, "").await.0 == 200 }).await;
    }

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

/// `GET /api/v1/health`의 상태 코드와 본문(로그인했으면 그 토큰으로 — 전체 본문)
async fn health(d: &Daemon, query: &str) -> (u16, Value) {
    let config: Value =
        serde_json::from_slice(&std::fs::read(d.path("oplctl.json")).unwrap()).unwrap();
    let client = oplctl::Client::new(
        &d.server(),
        config["token"].as_str().map(str::to_string),
        config["ca_pem"].as_str().map(str::to_string),
    )
    .unwrap();
    let resp = client
        .request(reqwest::Method::GET, &format!("/health{query}"))
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

/// 계획 46: 헬스 체크는 HA 리스(`process` 펜싱)·실행기 확인·역할·유지보수를 보고, `?live`는 늘 200이다.
/// API 노드 목록과 HAProxy `server` 줄은 `api` 역할 노드만 담는다.
#[tokio::test]
async fn health_and_api_endpoints_follow_roles_and_maintenance() {
    let mut d = Daemon::new(
        "node1",
        "[ha]\nfencing = \"process\"\nfence_after_secs = 10\n",
    );
    d.start().await;
    d.init("prod").await;
    d.login().await;
    eventually("ready", || async { health(&d, "").await.0 == 200 }).await;
    let (_, body) = health(&d, "?ready").await;
    assert_eq!(body["lease"], "fresh", "{body}");
    assert_eq!(body["runtimes"], "ok", "{body}");
    assert_eq!(body["quorum"], true, "{body}");
    assert_eq!(body["failed"], serde_json::json!([]), "{body}");

    let haproxy = d
        .ctl(&["-o", "haproxy", "cluster", "api-endpoints"])
        .await
        .unwrap();
    assert_eq!(
        haproxy,
        format!(
            "server node1 127.0.0.1:{} check ssl verify required ca-file /etc/haproxy/orbivisor-ca.pem\n",
            d.port
        )
    );

    // 역할을 비우면 앞단에서 빠진다.
    d.ctl(&["node", "roles", "node1", "none"]).await.unwrap();
    let (status, body) = health(&d, "").await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["failed"], serde_json::json!(["role"]), "{body}");
    assert_eq!(health(&d, "?live").await.0, 200);
    let endpoints = d.ctl_json(&["cluster", "api-endpoints"]).await;
    assert_eq!(endpoints, serde_json::json!([]));
    let list = d.ctl(&["node", "list"]).await.unwrap();
    assert!(list.contains("ROLES"), "{list}");
    d.ctl(&["node", "roles", "node1", "default"]).await.unwrap();
    assert_eq!(health(&d, "").await.0, 200);

    // 유지보수 중이면 준비되지 않았다.
    d.ctl(&["node", "maintenance", "node1", "on"])
        .await
        .unwrap();
    let (status, body) = health(&d, "").await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["failed"], serde_json::json!(["maintenance"]), "{body}");
    d.ctl(&["node", "maintenance", "node1", "off"])
        .await
        .unwrap();
    assert_eq!(health(&d, "").await.0, 200);
    d.stop();
}

/// 계획 46: `[api] tls_cert`·`tls_key`(관리자 인증서)가 있으면 API 리스너가 그것을 그대로 쓴다(클러스터
/// CA로 다시 발급하지 않는다). 파일을 읽지 못하면 opld가 시작하지 못한다.
#[tokio::test]
async fn an_admin_certificate_is_served_as_is() {
    const NAME: &str = "api.corp.example";
    let files = tempfile::tempdir().unwrap();
    let corp = opl_peer::pki::ClusterCa::generate("corp").unwrap();
    let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
    let cert_pem = corp.sign_api(&csr, &[NAME.into()]).unwrap();
    let (cert, key) = (files.path().join("api.crt"), files.path().join("api.key"));
    std::fs::write(&cert, cert_pem).unwrap();
    std::fs::write(&key, key_pem).unwrap();
    let mut d = Daemon::new(
        "node1",
        &format!(
            "[api]\ntls_cert = {:?}\ntls_key = {:?}",
            cert.display().to_string(),
            key.display().to_string()
        ),
    );
    d.start().await;
    d.init("prod").await;
    let resp = live_by_name(&d, &corp.cert_pem, NAME).await.unwrap();
    assert_eq!(resp.status(), 200);
    d.stop();

    std::fs::remove_file(&key).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_orbivisord"))
        .arg("--config")
        .arg(d.path("opld.toml"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + common::WAIT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("opld started without its API key");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert!(!status.success());
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert!(err.contains("api.key"), "{err}");
}

/// 클러스터 CA로 확인하며 `https://<name>:<포트>`(127.0.0.1로 풂)의 `?live` 헬스 체크를 부른다.
async fn live_by_name(d: &Daemon, ca_pem: &str, name: &str) -> reqwest::Result<reqwest::Response> {
    reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(ca_pem.as_bytes()).unwrap()])
        .resolve(name, ([127, 0, 0, 1], d.port).into())
        .build()
        .unwrap()
        .get(format!("https://{name}:{}/api/v1/health?live", d.port))
        .send()
        .await
}

/// 계획 46: 클러스터 설정 `api_names`를 바꾸면 노드가 API 인증서를 다시 서명해 다시 시작하지 않고 그
/// 이름으로 확인되게 한다. 다시 시작해도 그 이름을 쓴다.
#[tokio::test]
async fn api_names_are_served_by_the_api_listener() {
    const NAME: &str = "orbivisor.example.com";
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("prod").await;
    d.login().await;
    let ca = d.ctl(&["cluster", "ca"]).await.unwrap();
    assert!(live_by_name(&d, &ca, NAME).await.is_err());

    d.ctl(&["settings", "set", &format!("api_names={NAME}")])
        .await
        .unwrap();
    eventually("the API name", || async {
        live_by_name(&d, &ca, NAME).await.is_ok()
    })
    .await;
    // 노드 주소는 그대로 확인된다.
    assert_eq!(health(&d, "?live").await.0, 200);

    d.stop();
    d.start().await;
    eventually("the API name after a restart", || async {
        live_by_name(&d, &ca, NAME).await.is_ok()
    })
    .await;
    d.stop();
}

/// `/`(웹 UI 자리)의 상태 코드와 CSP 헤더 유무. 클러스터 CA로 확인한다.
async fn root_page(d: &Daemon, ca_pem: &str) -> (u16, bool) {
    let resp = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(ca_pem.as_bytes()).unwrap()])
        .build()
        .unwrap()
        .get(format!("{}/", d.server()))
        .send()
        .await
        .unwrap();
    (
        resp.status().as_u16(),
        resp.headers().contains_key("content-security-policy"),
    )
}

/// 계획 46: `[web] enabled = false`면 API만 열고 내장 웹 UI(시험 빌드는 안내 페이지)를 주지 않는다.
#[tokio::test]
async fn the_built_in_web_ui_can_be_turned_off() {
    let mut on = Daemon::new("node1", "");
    on.start().await;
    on.init("prod").await;
    on.login().await;
    let ca = on.ctl(&["cluster", "ca"]).await.unwrap();
    // 웹 UI를 넣은 빌드(`web-ui`, CI의 `web-it`)는 UI를, 아니면 보안 헤더를 붙인 안내 페이지(404)를 준다.
    let (status, csp) = root_page(&on, &ca).await;
    assert!(csp);
    if !cfg!(feature = "web-ui") {
        assert_eq!(status, 404);
    }
    on.stop();

    let mut off = Daemon::new("node1", "[web]\nenabled = false\n");
    off.start().await;
    off.init("prod").await;
    off.login().await;
    let ca = off.ctl(&["cluster", "ca"]).await.unwrap();
    assert_eq!(root_page(&off, &ca).await, (404, false));
    assert_eq!(health(&off, "").await.0, 200);
    off.stop();
}
