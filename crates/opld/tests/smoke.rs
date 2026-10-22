//! QEMU 없이 opld 하나를 띄워 클러스터를 만들고, HTTPS로 CLI를 쓰고, 다시 띄워도 상태가 남는지 본다.

mod common;

use common::Daemon;

#[tokio::test]
async fn single_node_cluster_serves_cli_over_https_and_survives_restarts() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    // 아직 클러스터가 없으면 API를 열지 않는다.
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", d.port)).is_err(),
        "API is open before the cluster exists"
    );
    let socket = d.socket();
    let err = d
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");

    let out = d.init("prod").await;
    assert!(out.contains("cluster prod created"), "{out}");
    assert!(
        out.contains("oplctl login --server https://127.0.0.1:"),
        "{out}"
    );
    let err = d
        .ctl(&["cluster", "init", "--name", "other", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already a member"), "{err}");
    // 노드 키는 주인만 읽는다.
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(d.data_dir().join("pki/node.key"))
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);
    // 저장소에 CA 개인 키가 있으므로 데이터 디렉터리도 주인만 들어간다. 제어 소켓은 주인만 쓴다.
    for (path, want) in [(d.data_dir(), 0o700), (d.path("opld.sock"), 0o600)] {
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, want, "{}", path.display());
    }

    // CA 지문 없이는 처음 로그인할 수 없다(지문을 알려 준다).
    let password = d.path("admin-password");
    std::fs::write(&password, d.admin_password.as_deref().unwrap()).unwrap();
    let server = d.server();
    let err = d
        .ctl(&[
            "login",
            "--server",
            &server,
            "--password-file",
            password.to_str().unwrap(),
        ])
        .await
        .unwrap_err();
    let fp = d.ca_fingerprint.clone().unwrap();
    assert!(err.to_string().contains(&fp), "{err}");
    let err = d
        .ctl(&[
            "login",
            "--server",
            &server,
            "--password-file",
            password.to_str().unwrap(),
            "--ca-fingerprint",
            "sha256:00",
        ])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");
    d.login().await;
    // 같은 서버에 다시 로그인하면 저장한 CA를 쓰고, 이전 토큰은 지워진다.
    d.ctl(&[
        "login",
        "--server",
        &server,
        "--password-file",
        password.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let tokens = d.ctl_json(&["token", "list"]).await;
    assert_eq!(tokens.as_array().unwrap().len(), 1, "{tokens}");

    let status = d.ctl(&["cluster", "status"]).await.unwrap();
    assert!(status.contains("cluster: prod"), "{status}");
    assert!(status.contains(&fp), "{status}");
    let nodes = d.ctl_json(&["node", "list"]).await;
    assert_eq!(nodes[0]["name"], "node1");
    assert_eq!(nodes[0]["status"]["online"], true);
    assert_eq!(nodes[0]["spec"]["address"], format!("127.0.0.1:{}", d.port));

    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.ctl(&["vm", "create", "web", "--memory", "256", "--nic", "user"])
        .await
        .unwrap();
    let err = d
        .ctl(&["vm", "create", "web", "--memory", "256"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("name_taken"), "{err}");

    d.stop();
    d.start().await;
    // 다시 시작하면 멤버로 바로 뜨고, 저장한 토큰을 계속 쓸 수 있다.
    let vms = d.ctl_json(&["vm", "list"]).await;
    assert_eq!(vms[0]["name"], "web");
    let pools = d.ctl(&["pool", "list"]).await.unwrap();
    assert!(pools.contains("local"), "{pools}");
    // 유일한 투표 멤버는 떠날 수 없다.
    let err = d
        .ctl(&["cluster", "leave", "--socket", &socket])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("only voting member"), "{err}");

    d.ctl(&["logout"]).await.unwrap();
    let err = d.ctl(&["vm", "list"]).await.unwrap_err();
    assert!(err.to_string().contains("oplctl login"), "{err}");
    d.stop();
}

#[tokio::test]
async fn opld_fails_on_taken_port_before_starting_workers() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("prod").await;
    d.stop();
    // 하이퍼바이저 드라이버는 run_dir/vm을 만든다. 포트를 먼저 열면 거기까지 가지 않는다.
    std::fs::remove_dir_all(d.path("run/vm")).unwrap();
    let _taken = std::net::TcpListener::bind(("127.0.0.1", d.port)).unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_opld"))
        .arg("--config")
        .arg(d.path("opld.toml"))
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!d.path("run/vm").exists());
}

#[tokio::test]
async fn a_second_opld_on_the_same_socket_refuses_to_start() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_opld"))
        .arg("--config")
        .arg(d.path("opld.toml"))
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    // 처음 opld는 그대로 응답한다.
    d.init("prod").await;
    d.stop();
}

#[tokio::test]
async fn a_node_killed_right_after_joining_becomes_a_voter_when_restarted() {
    let mut d1 = Daemon::new("node1", "");
    d1.start().await;
    d1.init("prod").await;
    d1.login().await;
    let url = d1.join_url.clone().unwrap();

    let mut d2 = Daemon::new("node2", "");
    d2.start().await;
    let socket = d2.socket();
    // 포트가 쓰이고 있으면 조인 토큰을 쓰기 전에 실패한다(같은 토큰으로 아래에서 합류한다).
    {
        let _taken = std::net::TcpListener::bind(("127.0.0.1", d2.port)).unwrap();
        let err = d2
            .ctl(&["cluster", "join", &url, "--socket", &socket])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot listen"), "{err}");
    }
    // 노드 인증서를 저장하자마자(투표 멤버로 올라가기 전에) kill -9 한다.
    let pki = d2.data_dir().join("pki");
    let join = ["cluster", "join", &url, "--socket", &socket];
    tokio::select! {
        _ = d2.ctl(&join) => {}
        () = async {
            while !pki.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        } => {}
    }
    d2.kill();

    // 다시 띄우면 멤버로 뜨고, 승격을 다시 요청해 투표 멤버가 된다.
    d2.start().await;
    common::eventually("node2 to become a voter", || async {
        let view = d1.ctl_json(&["cluster", "status"]).await;
        let members = view["members"].as_array().unwrap();
        members.len() == 2 && members.iter().all(|m| m["voter"] == true)
    })
    .await;
    // 승격 요청은 노드 2의 응답이 있어야 끝나므로, 위에서 죽인 노드는 learner로 다시 떴다.
    assert!(d2.log().contains("asking to be promoted"), "{}", d2.log());
    d2.stop();
}
