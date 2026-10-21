//! QEMU 없이 opld를 띄워 CLI로 다루고, 다시 띄워도 상태가 남는지 본다.

mod common;

use common::Daemon;

#[tokio::test]
async fn opld_serves_cli_and_keeps_state_across_restarts() {
    let mut d = Daemon::new("");
    d.start().await;
    // 첫 시작에서 관리자 비밀번호를 파일로 남긴다(0600).
    let password_file = d.data_dir().join("initial-admin-password");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&password_file).unwrap().permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);
    d.login().await;
    // 같은 서버에 다시 로그인하면(끝의 `/`는 무시) 이전 토큰은 지워진다.
    let password = password_file.to_str().unwrap();
    let server = format!("{}/", d.server());
    d.ctl(&["login", "--server", &server, "--password-file", password])
        .await
        .unwrap();
    let tokens = d.ctl_json(&["token", "list"]).await;
    assert_eq!(tokens.as_array().unwrap().len(), 1, "{tokens}");

    let nodes = d.ctl_json(&["node", "list"]).await;
    assert_eq!(nodes[0]["name"], "node1");
    assert_eq!(nodes[0]["status"]["online"], true);
    assert!(nodes[0]["status"]["memory_mib"].as_u64().unwrap() > 0);

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

    let password = std::fs::read_to_string(&password_file).unwrap();
    d.stop();
    d.start().await;
    // 비밀번호는 다시 만들지 않고, 저장된 토큰도 계속 쓸 수 있다.
    assert_eq!(std::fs::read_to_string(&password_file).unwrap(), password);
    let vms = d.ctl_json(&["vm", "list"]).await;
    assert_eq!(vms[0]["name"], "web");
    let pools = d.ctl(&["pool", "list"]).await.unwrap();
    assert!(pools.contains("local"), "{pools}");

    d.ctl(&["logout"]).await.unwrap();
    let err = d.ctl(&["vm", "list"]).await.unwrap_err();
    assert!(err.to_string().contains("oplctl login"), "{err}");
    d.stop();
}

#[tokio::test]
async fn opld_fails_on_taken_port_before_starting_workers() {
    let d = Daemon::new("");
    let _taken = std::net::TcpListener::bind(("127.0.0.1", d.port)).unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_opld"))
        .arg("--config")
        .arg(d.path("opld.toml"))
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    // 하이퍼바이저 드라이버는 run_dir를 만든다. 포트를 먼저 열면 거기까지 가지 않는다.
    assert!(!d.path("run").exists());
}
