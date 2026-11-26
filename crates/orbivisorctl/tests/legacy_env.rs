//! 옛 환경 변수 이름(`OPL_*`)을 한 릴리스 동안 받는지 본다(계획 74): main이 clap 전에 `ORBIVISOR_*`로 옮기고
//! 이름마다 경고를 한 줄 낸다. 둘 다 있으면 새 것을 쓰고 경고하지 않는다. 주소는 닫힌 포트라 연결 오류에 그 주소가 보인다.

use std::process::Command;

fn run(envs: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_orbivisorctl"));
    cmd.args(["vm", "list"]);
    for k in ["CONFIG", "SERVER"] {
        cmd.env_remove(format!("ORBIVISOR_{k}"))
            .env_remove(format!("OPL_{k}"));
    }
    cmd.envs(envs.iter().copied());
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "{out:?}");
    (
        String::from_utf8(out.stderr).unwrap(),
        String::from_utf8(out.stdout).unwrap(),
    )
}

#[test]
fn old_names_are_used_with_a_warning() {
    let (stderr, _) = run(&[
        ("OPL_SERVER", "http://127.0.0.1:1"),
        ("OPL_CONFIG", "/nonexistent/orbivisorctl.json"),
    ]);
    assert!(
        stderr.contains(
            "warning: environment variable OPL_SERVER is deprecated; use ORBIVISOR_SERVER\n"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "warning: environment variable OPL_CONFIG is deprecated; use ORBIVISOR_CONFIG\n"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("http://127.0.0.1:1"), "{stderr}");
}

#[test]
fn new_names_win_without_a_warning() {
    let (stderr, _) = run(&[
        ("ORBIVISOR_SERVER", "http://127.0.0.1:2"),
        ("OPL_SERVER", "http://127.0.0.1:1"),
        ("ORBIVISOR_CONFIG", "/nonexistent/orbivisorctl.json"),
    ]);
    assert!(!stderr.contains("deprecated"), "{stderr}");
    assert!(stderr.contains("http://127.0.0.1:2"), "{stderr}");
    assert!(!stderr.contains("127.0.0.1:1/"), "{stderr}");
}

/// `logout`은 옛 로그인 파일(`~/.config/opl/oplctl.json`)을 고치지 못해도 경고만 하고 로그아웃으로 끝난다 — 서버
/// 토큰과 새 파일은 이미 지웠다(리뷰 2, login과 같다).
#[test]
fn logout_warns_when_the_old_login_file_cannot_be_cleared() {
    let home = tempfile::tempdir().unwrap();
    let new = home.path().join(".config/orbivisor/orbivisorctl.json");
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    // login이 만들지 않은 토큰이라 서버에는 묻지 않는다.
    std::fs::write(
        &new,
        r#"{"server":"http://127.0.0.1:1","token":"web-ui-token"}"#,
    )
    .unwrap();
    let old = home.path().join(".config/opl/oplctl.json");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "not json").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_orbivisorctl"));
    for k in ["CONFIG", "SERVER", "SOCKET"] {
        cmd.env_remove(format!("ORBIVISOR_{k}"))
            .env_remove(format!("OPL_{k}"));
    }
    let out = cmd.env("HOME", home.path()).arg("logout").output().unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "logged out\n");
    assert!(
        stderr.contains("warning: cannot clear the token in the old login file"),
        "{stderr}"
    );
    assert!(
        !std::fs::read_to_string(&new)
            .unwrap()
            .contains("web-ui-token")
    );
}
