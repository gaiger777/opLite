//! 계획 62: 폴더·grant·할당량·범위 토큰·2단계 인증을 opld 프로세스와 oplctl로 끝까지 본다. QEMU는 쓰지 않는다(꺼진 VM,
//! 디스크 없음).
//!
//! - 노드 하나: admin이 `/teamA`·`/teamB`와 VM을 만들고 그룹 `team-a`(로컬 멤버 alice, 전역 역할 `none`)에 `/teamA`
//!   operator를 준다. alice는 teamA만 보고 다룬다. `--vms 2` 할당량에서 셋째 VM은 409 `quota_exceeded`. 범위 토큰은
//!   범위 밖 권한을 쓰지 못한다. alice가 TOTP를 켜면 `login`에 둘째 단계(TOTP·복구 코드)가 필요하고, 관리자 초기화 뒤에는
//!   필요 없다.
//! - 노드 둘: 두 노드의 API로 같은 폴더에 동시에 만들어도 할당량을 넘지 않는다(Raft 안 확인).

mod common;

use common::{Daemon, eventually};
use orbivisor_types::two_factor::{TOTP_DIGITS, decode_secret, totp, totp_step};
use serde_json::Value;

/// `config`(이 데몬 디렉터리의 파일 이름) 로그인 정보로 oplctl을 실행한다(admin이 아닌 사용자).
async fn ctl_as(d: &Daemon, config: &str, args: &[&str]) -> anyhow::Result<String> {
    let config = d.path(config);
    let mut argv = vec!["oplctl", "--config", config.to_str().unwrap()];
    argv.extend_from_slice(args);
    let mut out = Vec::new();
    // 명령 future가 커서(디버그 빌드) 시험 스레드의 스택 대신 힙에 둔다.
    Box::pin(orbivisorctl::run(argv, &mut out)).await?;
    Ok(String::from_utf8(out).unwrap())
}

async fn json_as(d: &Daemon, config: &str, args: &[&str]) -> Value {
    let mut argv = vec!["-o", "json"];
    argv.extend_from_slice(args);
    serde_json::from_str(&ctl_as(d, config, &argv).await.unwrap()).unwrap()
}

/// 이 호스트의 기본 기계 종류. 폴더 사용자는 노드 목록을 읽지 못해 `vm create`에 `--machine`을 준다.
fn machine() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "virt"
    } else {
        "q35"
    }
}

/// 목록 JSON의 이름들(정렬)
fn names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

/// API 오류의 코드로 끝나는지(`메시지 (코드)`)
fn fails_with(r: anyhow::Result<String>, code: &str) {
    let e = r.expect_err("the command should fail");
    assert!(format!("{e:#}").ends_with(&format!("({code})")), "{e:#}");
}

fn step_now() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    totp_step(now)
}

/// 칸 `step`의 TOTP 코드
fn code_at(secret: &[u8], step: u64) -> String {
    format!(
        "{:0width$}",
        totp(secret, step, TOTP_DIGITS),
        width = TOTP_DIGITS as usize
    )
}

/// 지금(+`ahead` 칸) TOTP 코드
fn code(secret: &[u8], ahead: u64) -> String {
    code_at(secret, step_now() + ahead)
}

async fn create_in(d: &Daemon, name: String) -> anyhow::Result<String> {
    d.ctl(&[
        "vm", "create", &name, "--memory", "256", "--folder", "/team",
    ])
    .await
}

/// alice로 로그인한다(`extra`는 `--code` 등).
async fn alice_login(d: &Daemon, password_file: &str, extra: &[&str]) -> anyhow::Result<String> {
    let server = d.server();
    let fp = d.ca_fingerprint.clone().unwrap();
    let mut args = vec![
        "login",
        "--server",
        &server,
        "--user",
        "alice",
        "--password-file",
        password_file,
        "--ca-fingerprint",
        &fp,
    ];
    args.extend_from_slice(extra);
    ctl_as(d, "alice.json", &args).await
}

#[tokio::test]
async fn folder_grants_quotas_scoped_tokens_and_two_factor_on_one_node() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("perm").await;
    d.login().await;

    // admin: 폴더 둘, 각 폴더에 VM 하나, 루트에 VM 하나
    for f in ["/teamA", "/teamB"] {
        d.ctl(&["folder", "create", f]).await.unwrap();
    }
    for (vm, folder) in [("a1", Some("/teamA")), ("b1", Some("/teamB")), ("r1", None)] {
        let mut args = vec!["vm", "create", vm, "--memory", "256"];
        if let Some(f) = folder {
            args.extend(["--folder", f]);
        }
        d.ctl(&args).await.unwrap();
    }
    let tree = d.ctl(&["folder", "tree", "--kind", "vm"]).await.unwrap();
    assert_eq!(tree, "vm:/\n  teamA\n  teamB\n");
    assert_eq!(
        names(&d.ctl_json(&["vm", "list", "--folder", "/teamA"]).await),
        ["a1"]
    );
    assert_eq!(
        names(&d.ctl_json(&["vm", "list", "--folder", "/"]).await),
        ["r1"]
    );

    // alice(전역 역할 none)는 그룹 team-a로 /teamA operator를 받는다.
    let pw = d.path("alice-password");
    std::fs::write(&pw, "alice-password-1").unwrap();
    let pw = pw.to_str().unwrap().to_string();
    d.ctl(&[
        "user",
        "create",
        "alice",
        "--role",
        "none",
        "--password-file",
        &pw,
    ])
    .await
    .unwrap();
    d.ctl(&["group", "create", "team-a", "--member", "alice"])
        .await
        .unwrap();
    // 레이블 없는 그룹을 읽어 다시 쓴다(`labels: null`을 보내 400이던 것, 계획 64 리뷰 1).
    d.ctl(&["group", "set", "team-a", "--description", "team A"])
        .await
        .unwrap();
    d.ctl(&[
        "grant", "add", "--group", "team-a", "--role", "operator", "--on", "/teamA",
    ])
    .await
    .unwrap();
    let grants = d.ctl(&["grant", "list", "--on", "/teamA"]).await.unwrap();
    assert!(
        grants
            .lines()
            .nth(1)
            .is_some_and(|l| l.split_whitespace().skip(1).eq([
                "group:team-a",
                "operator",
                "vm:/teamA",
                "true"
            ])),
        "{grants}"
    );
    let who = d.ctl(&["perm", "who", "vm/a1"]).await.unwrap();
    // grant(그룹)와 전역 역할 사용자(admin). alice는 전역 역할이 none이라 그룹 grant로만 닿는다.
    assert!(
        who.contains("group:team-a") && who.contains("\nadmin ") && !who.contains("alice"),
        "{who}"
    );

    let out = alice_login(&d, &pw, &[]).await.unwrap();
    assert!(out.starts_with("logged in to "), "{out}");

    // alice는 teamA만 본다. teamB VM은 없는 것과 같다(404).
    assert_eq!(
        names(&json_as(&d, "alice.json", &["vm", "list"]).await),
        ["a1"]
    );
    let folders = json_as(&d, "alice.json", &["folder", "list"]).await;
    assert_eq!(folders.as_array().unwrap().len(), 1, "{folders}");
    fails_with(
        ctl_as(&d, "alice.json", &["vm", "get", "b1"]).await,
        "not_found",
    );
    fails_with(
        ctl_as(&d, "alice.json", &["vm", "get", "r1"]).await,
        "not_found",
    );
    let tasks = json_as(&d, "alice.json", &["task", "list"]).await;
    assert!(
        tasks
            .as_array()
            .unwrap()
            .iter()
            .all(|t| !t.to_string().contains("b1")),
        "{tasks}"
    );
    let mine = json_as(&d, "alice.json", &["perm", "mine"]).await;
    assert_eq!(mine["all"], false, "{mine}");
    let check = ctl_as(&d, "alice.json", &["perm", "check", "vm/a1"])
        .await
        .unwrap();
    assert!(
        check.starts_with("alice (role none) on vm/a1: ") && check.contains("vm.power"),
        "{check}"
    );

    // 만들기: 폴더를 주지 않으면 400 folder_required, 볼 수 없는 폴더는 찾지 못한다.
    fails_with(
        ctl_as(
            &d,
            "alice.json",
            &[
                "vm",
                "create",
                "a0",
                "--memory",
                "256",
                "--machine",
                machine(),
            ],
        )
        .await,
        "folder_required",
    );
    let e = ctl_as(
        &d,
        "alice.json",
        &[
            "vm",
            "create",
            "x",
            "--machine",
            machine(),
            "--folder",
            "/teamB",
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "folder /teamB not found");
    ctl_as(
        &d,
        "alice.json",
        &[
            "vm",
            "create",
            "a2",
            "--machine",
            machine(),
            "--folder",
            "/teamA",
        ],
    )
    .await
    .unwrap();
    // 폴더 할당량은 부모(루트)의 inventory.quota — alice는 자기 상한을 바꾸지 못한다.
    fails_with(
        ctl_as(
            &d,
            "alice.json",
            &["folder", "quota", "set", "/teamA", "--vms", "9"],
        )
        .await,
        "forbidden",
    );

    // 할당량 VM 둘: 셋째는 거절된다.
    let out = d
        .ctl(&[
            "folder", "quota", "set", "/teamA", "--vms", "2", "--vcpus", "16",
        ])
        .await
        .unwrap();
    assert_eq!(out, "folder /teamA: quota 16 vCPUs, 2 VMs\n");
    fails_with(
        ctl_as(
            &d,
            "alice.json",
            &[
                "vm",
                "create",
                "a3",
                "--machine",
                machine(),
                "--folder",
                "/teamA",
            ],
        )
        .await,
        "quota_exceeded",
    );
    let usage = json_as(&d, "alice.json", &["folder", "usage", "/teamA"]).await;
    assert_eq!(usage["usage"]["vms"], 2, "{usage}");
    assert_eq!(usage["limits"][0]["quota"]["vms"], 2, "{usage}");
    // 옮겨 넣기도 할당량을 지킨다(admin이 root의 r1을 teamA로).
    let e = d
        .ctl(&["folder", "members", "move", "/teamA", "vm/r1"])
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), "1 of 1 items were not moved");

    // 범위 토큰: system.read만. 목록은 보이고 지우기는 403, 토큰 만들기도 403.
    let secret = ctl_as(
        &d,
        "alice.json",
        &["token", "create", "ro", "--scope", "system.read@/teamA"],
    )
    .await
    .unwrap();
    let token_file = d.path("ro-token");
    std::fs::write(&token_file, secret.trim()).unwrap();
    let server = d.server();
    let fp = d.ca_fingerprint.clone().unwrap();
    ctl_as(
        &d,
        "ro.json",
        &[
            "login",
            "--server",
            &server,
            "--token-file",
            token_file.to_str().unwrap(),
            "--ca-fingerprint",
            &fp,
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        names(&json_as(&d, "ro.json", &["vm", "list"]).await),
        ["a1", "a2"]
    );
    fails_with(
        ctl_as(&d, "ro.json", &["vm", "delete", "a2"]).await,
        "forbidden",
    );
    fails_with(
        ctl_as(&d, "ro.json", &["token", "create", "wider"]).await,
        "forbidden",
    );
    let tokens = ctl_as(&d, "alice.json", &["token", "list"]).await.unwrap();
    assert!(tokens.contains("system.read@1 target"), "{tokens}");

    // 2단계 인증: alice가 TOTP를 켠다(시험이 코드를 셈한다).
    let start = json_as(&d, "alice.json", &["2fa", "totp", "enable", "--no-confirm"]).await;
    let secret = decode_secret(start["secret"].as_str().unwrap()).unwrap();
    let done = json_as(
        &d,
        "alice.json",
        &["2fa", "totp", "confirm", &code(&secret, 0)],
    )
    .await;
    let recovery: Vec<String> = done["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(recovery.len(), 10, "{done}");
    let status = json_as(&d, "alice.json", &["2fa", "status"]).await;
    assert_eq!(status["totp"], true, "{status}");
    assert_eq!(status["recovery_codes"], 10, "{status}");
    let users = d.ctl(&["user", "list"]).await.unwrap();
    assert!(
        users
            .lines()
            .any(|l| l.split_whitespace().eq(["alice", "none", "on"])),
        "{users}"
    );

    // 로그인에 둘째 단계가 필요하다. 틀린 코드는 거절된다(확인에 쓴 칸은 다시 쓸 수 없어 다음 칸으로 로그인한다).
    let step = step_now();
    let near: Vec<String> = (step - 1..step + 4).map(|s| code_at(&secret, s)).collect();
    let wrong = (0..1_000_000u32)
        .map(|n| format!("{n:06}"))
        .find(|c| !near.contains(c))
        .unwrap();
    fails_with(
        alice_login(&d, &pw, &["--code", &wrong]).await,
        "invalid_second_factor",
    );
    let out = alice_login(&d, &pw, &["--code", &code(&secret, 1)])
        .await
        .unwrap();
    assert!(out.starts_with("logged in to "), "{out}");
    assert_eq!(
        names(&json_as(&d, "alice.json", &["vm", "list"]).await),
        ["a1", "a2"]
    );
    // 복구 코드도 한 번 쓴다.
    alice_login(&d, &pw, &["--code", &recovery[0]])
        .await
        .unwrap();
    let status = json_as(&d, "alice.json", &["2fa", "status"]).await;
    assert_eq!(status["recovery_codes"], 9, "{status}");
    fails_with(
        alice_login(&d, &pw, &["--code", &recovery[0]]).await,
        "invalid_second_factor",
    );

    // 관리자 초기화: 요소가 없어지고 둘째 단계 없이 로그인한다. 자기 자신은 초기화할 수 없다.
    let out = d.ctl(&["user", "2fa-reset", "alice"]).await.unwrap();
    assert_eq!(out, "two-factor authentication of alice reset\n");
    fails_with(
        d.ctl(&["user", "2fa-reset", "admin"]).await,
        "cannot_reset_self",
    );
    alice_login(&d, &pw, &[]).await.unwrap();
    let status = json_as(&d, "alice.json", &["2fa", "status"]).await;
    assert_eq!(status["totp"], false, "{status}");

    // 정책: 영역 local이면 요구한다(설정만 보고 되돌린다 — CLI 강제 등록은 터미널이 필요하다).
    d.ctl(&[
        "settings",
        "set",
        "two_factor.realms=local",
        "two_factor.roles=admin",
    ])
    .await
    .unwrap();
    let settings = d.ctl_json(&["settings", "get"]).await;
    assert_eq!(
        settings["spec"]["two_factor"],
        serde_json::json!({ "roles": ["admin"], "realms": ["local"] })
    );
    let e = alice_login(&d, &pw, &["--code", "123456"])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("without --code"), "{e:#}");
    d.ctl(&["settings", "set", "two_factor=off"]).await.unwrap();
    let settings = d.ctl_json(&["settings", "get"]).await;
    assert_eq!(settings["spec"]["two_factor"], Value::Null, "{settings}");

    // 지우기: grant를 빼면 alice는 아무것도 보지 못한다. 빈 폴더만 지운다.
    let grants = d.ctl_json(&["grant", "list"]).await;
    let id = grants[0]["id"].as_str().unwrap().to_string();
    d.ctl(&["grant", "remove", &id]).await.unwrap();
    assert!(
        json_as(&d, "alice.json", &["vm", "list"])
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    fails_with(
        d.ctl(&["folder", "delete", "/teamB"]).await,
        "folder_not_empty",
    );
    d.ctl(&["vm", "delete", "b1"]).await.unwrap();
    d.ctl(&["folder", "delete", "/teamB"]).await.unwrap();
    d.ctl(&["group", "delete", "team-a"]).await.unwrap();
}

#[tokio::test]
async fn concurrent_creates_on_two_nodes_keep_the_folder_quota() {
    let mut a = Daemon::new("node-a", "");
    let mut b = Daemon::new("node-b", "");
    a.start().await;
    b.start().await;
    a.init("perm2").await;
    a.login().await;
    let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
    b.join(url.trim()).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;

    a.ctl(&["folder", "create", "/team", "--vms", "3"])
        .await
        .unwrap();
    eventually("node-b sees the folder", || async {
        b.ctl(&["folder", "usage", "/team"]).await.is_ok()
    })
    .await;

    // 두 노드에 넷씩 동시에
    let mut runs = Vec::new();
    for i in 0..4 {
        runs.push(create_in(&a, format!("va{i}")));
        runs.push(create_in(&b, format!("vb{i}")));
    }
    let results = futures_util::future::join_all(runs).await;
    let ok = results.iter().filter(|r| r.is_ok()).count();
    for r in results.into_iter().filter(|r| r.is_err()) {
        fails_with(r, "quota_exceeded");
    }
    assert_eq!(ok, 3);
    for d in [&a, &b] {
        eventually(&format!("{} lists 3 VMs", d.name), || async {
            d.ctl_json(&["vm", "list", "--folder", "/team"])
                .await
                .as_array()
                .unwrap()
                .len()
                == 3
        })
        .await;
    }
    let usage = b.ctl_json(&["folder", "usage", "/team"]).await;
    assert_eq!(usage["usage"]["vms"], 3, "{usage}");
}
