//! 계획 38: 한 노드짜리 opld 클러스터 둘. prod에 dr을 원격으로 등록하고(dr의 operator 토큰) prod의 꺼진
//! VM 백업을 dr의 백업 풀로 복제한다. dr의 사본은 보통 백업이라 dr에서 복원하면 디스크 내용이 같고, prod에서
//! 원본을 지우면 dr의 사본도 지워진다. 백업과 복원이 `qemu-img`를 쓰므로 `qemu-it` 뒤에 둔다(VM은 켜지
//! 않는다).
//! `cargo test -p opld --features qemu-it --test replicate`
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Daemon, eventually};
use serde_json::{Value, json};

/// 디스크 앞에 써 두는 표식의 길이
const MARK: usize = 64 * 1024;

/// 클러스터 `cluster`를 만들고 로그인해 VM 디스크 풀 `local`과 백업 풀 `bk`(dir)를 만든다.
async fn setup(d: &mut Daemon, cluster: &str) {
    d.start().await;
    d.init(cluster).await;
    d.login().await;
    for (name, extra) in [("local", None), ("bk", Some("--backups"))] {
        let dir = d.path(name);
        std::fs::create_dir(&dir).unwrap();
        let mut args = vec!["pool", "create", name, "--path", dir.to_str().unwrap()];
        args.extend(extra);
        d.ctl(&args).await.unwrap();
        d.wait_pool_ready(name).await;
    }
}

/// 풀 `local`에 있는 볼륨 `name`의 파일
async fn volume_file(d: &Daemon, name: &str) -> PathBuf {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let format = volume["spec"]["format"].as_str().unwrap();
    d.path("local")
        .join(format!("{}.{format}", volume["id"].as_str().unwrap()))
}

/// 이미지의 앞 `MARK` 바이트. raw로 빼서 읽는다.
fn head(d: &Daemon, file: &Path) -> Vec<u8> {
    let raw = d.path("head.raw");
    let out = Command::new("qemu-img")
        .args(["convert", "-O", "raw"])
        .arg(file)
        .arg(&raw)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    let bytes = std::fs::read(&raw).unwrap();
    std::fs::remove_file(&raw).unwrap();
    bytes[..MARK].to_vec()
}

/// dr에 operator 사용자 `repl`을 만들고 그 사용자로 로그인해 만든 API 토큰을 파일에 써 그 경로를 돌려준다.
async fn operator_token(dr: &Daemon, prod: &Daemon) -> String {
    let password = dr.path("repl-password");
    std::fs::write(&password, "repl-password-1").unwrap();
    let password = password.to_str().unwrap();
    dr.ctl(&[
        "user",
        "create",
        "repl",
        "--role",
        "operator",
        "--password-file",
        password,
    ])
    .await
    .unwrap();
    // admin의 로그인 정보(`oplctl.json`)와 따로 둔다.
    let config = dr.path("repl.json");
    let config = config.to_str().unwrap();
    let server = dr.server();
    ctl_with(
        config,
        &[
            "login",
            "--server",
            &server,
            "--user",
            "repl",
            "--password-file",
            password,
            "--ca-fingerprint",
            dr.ca_fingerprint.as_deref().unwrap(),
        ],
    )
    .await;
    let token = ctl_with(config, &["token", "create", "prod"]).await;
    let file = prod.path("dr-token");
    std::fs::write(&file, token).unwrap();
    file.to_str().unwrap().to_string()
}

/// 로그인 정보 파일 `config`로 oplctl 명령을 실행한다.
async fn ctl_with(config: &str, args: &[&str]) -> String {
    let mut argv = vec!["oplctl", "--config", config];
    argv.extend_from_slice(args);
    let mut out = Vec::new();
    oplctl::run(argv, &mut out).await.unwrap();
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn a_backup_is_replicated_restored_on_the_remote_and_deleted_with_the_original() {
    let mut prod = Daemon::new("node-a", "");
    let mut dr = Daemon::new("node-b", "");
    setup(&mut prod, "prod").await;
    setup(&mut dr, "dr").await;

    // prod에 dr을 원격으로 등록한다. dr 쪽 토큰 주인은 operator(백업 쓰기)다.
    let token_file = operator_token(&dr, &prod).await;
    let url = dr.server();
    prod.ctl(&[
        "remote",
        "add",
        "dr",
        "--url",
        &url,
        "--ca-fingerprint",
        dr.ca_fingerprint.as_deref().unwrap(),
        "--token-file",
        &token_file,
    ])
    .await
    .unwrap();

    // prod의 꺼진 VM(raw 디스크에 표식)을 백업한다.
    prod.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    {
        use std::io::Write;
        let file = volume_file(&prod, "root").await;
        let mut f = std::fs::OpenOptions::new().write(true).open(file).unwrap();
        f.write_all(&[0xab; MARK]).unwrap();
    }
    prod.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    prod.ctl(&[
        "backup", "create", "web", "--pool", "bk", "--name", "manual",
    ])
    .await
    .unwrap();

    // dr의 백업 풀 bk로 복제한다.
    let out = prod
        .ctl(&[
            "backup",
            "replicate",
            "manual",
            "--remote",
            "dr",
            "--pool",
            "bk",
            "--wait",
        ])
        .await
        .unwrap();
    assert_eq!(out, "backup manual replicated to pool bk of remote dr\n");
    let original = prod.ctl_json(&["backup", "get", "manual"]).await;
    let replica = &original["status"]["replicas"][0];
    assert_eq!(replica["remote"], "dr", "{original}");
    assert_eq!(replica["pool"], "bk", "{original}");
    assert_eq!(replica["state"], "ready", "{original}");
    let list = prod.ctl(&["backup", "list"]).await.unwrap();
    let row: Vec<&str> = list.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(row.last(), Some(&"dr:ready"), "{list}");

    // dr에는 같은 이름의 ready 사본이 있고 출처가 prod의 원본이다.
    let copies = dr.ctl_json(&["backup", "list"]).await;
    let copies = copies.as_array().unwrap();
    assert_eq!(copies.len(), 1, "{copies:?}");
    let copy = &copies[0];
    assert_eq!(copy["id"], replica["backup"], "{copy}");
    assert_eq!(copy["name"], "manual", "{copy}");
    assert_eq!(copy["status"]["phase"], "ready", "{copy}");
    assert_eq!(copy["spec"]["vm_name"], "web", "{copy}");
    let origin = &copy["spec"]["origin"];
    assert_eq!(origin["cluster"], "prod", "{copy}");
    assert_eq!(origin["backup"], original["id"], "{copy}");
    assert_eq!(
        origin["ca_fingerprint"].as_str(),
        prod.ca_fingerprint.as_deref(),
        "{copy}"
    );
    let list = dr.ctl(&["backup", "list"]).await.unwrap();
    let id = original["id"].as_str().unwrap();
    assert!(
        list.lines()
            .nth(1)
            .unwrap()
            .contains(&format!("prod/{}", &id[..8])),
        "{list}"
    );

    // dr에서 사본을 복원하면 디스크 내용이 같다.
    let out = dr
        .ctl(&[
            "backup", "restore", "manual", "--name", "web", "--pool", "local",
        ])
        .await
        .unwrap();
    assert_eq!(out, "vm web restored from backup manual\n");
    let restored = volume_file(&dr, "web-disk0").await;
    assert_eq!(head(&dr, &restored), vec![0xab; MARK]);

    // prod에서 원본을 지우면 dr의 사본도 지워진다.
    let out = prod.ctl(&["backup", "delete", "manual"]).await.unwrap();
    assert_eq!(out, "backup manual deleted\n");
    eventually("the remote copy is deleted", || async {
        dr.ctl_json(&["backup", "list"]).await == json!([])
    })
    .await;
    let copy_dir = dr
        .path("bk")
        .join("backups")
        .join(copy["id"].as_str().unwrap());
    assert!(!copy_dir.exists(), "remote copy files left");
    assert_eq!(
        prod.ctl_json(&["backup", "list"]).await,
        Value::Array(vec![])
    );

    prod.stop();
    dr.stop();
}
