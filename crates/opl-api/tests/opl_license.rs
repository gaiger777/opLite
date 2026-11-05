//! 구독 키 발급 도구 opl-license: keygen → issue → 키 풀기 왕복, 비밀 키 파일 권한, 잘못된 인자.

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

use opl_types::subscription::{self, KeyError, Level, PublicKey};

fn opl_license(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_opl-license"))
        .args(args)
        .output()
        .expect("run opl-license")
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn keygen_then_issue_makes_a_key_this_public_key_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("issuer.key");
    let key_path = key_file.to_str().unwrap();

    let public = stdout(&opl_license(&["keygen", "--out", key_path]));
    let mode = std::fs::metadata(&key_file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    // pubkey는 비밀 키 파일에서 같은 공개 키를 낸다.
    assert_eq!(stdout(&opl_license(&["pubkey", "--key", key_path])), public);
    let public = PublicKey::from_base64(public.trim(), false).expect("public key on stdout");

    let issued_at = 1_800_000_000;
    let out = stdout(&opl_license(&[
        "issue",
        "--key",
        key_path,
        "--id",
        "lic-42",
        "--customer",
        "Example Corp",
        "--level",
        "standard",
        "--nodes",
        "4",
        "--days",
        "365",
        "--issued-at",
        &issued_at.to_string(),
    ]));
    assert_eq!(out.lines().count(), 1, "{out}");
    let license = subscription::parse(&out, std::slice::from_ref(&public), issued_at).unwrap();
    assert_eq!(license.id, "lic-42");
    assert_eq!(license.customer, "Example Corp");
    assert_eq!(license.level, Level::Standard);
    assert_eq!(license.nodes, 4);
    assert_eq!(license.issued_at, issued_at);
    assert_eq!(license.expires_at, issued_at + 365 * 86_400);
    assert!(!license.development);
    // 저장소의 개발 키로는 검증되지 않는다.
    assert_eq!(
        subscription::verify(&out, &subscription::license_keys()),
        Err(KeyError::Signature)
    );

    // 이미 있는 파일은 덮어쓰지 않는다.
    let before = std::fs::read(&key_file).unwrap();
    let again = opl_license(&["keygen", "--out", key_path]);
    assert!(!again.status.success());
    assert_eq!(std::fs::read(&key_file).unwrap(), before);
}

#[test]
fn issue_rejects_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("issuer.key");
    let key_path = key_file.to_str().unwrap();
    stdout(&opl_license(&["keygen", "--out", key_path]));

    let issue = |key: &str, nodes: &str, days: &str| {
        opl_license(&[
            "issue",
            "--key",
            key,
            "--id",
            "lic-1",
            "--customer",
            "c",
            "--level",
            "premium",
            "--nodes",
            nodes,
            "--days",
            days,
        ])
    };
    // 노드 0, 기간 0은 키를 만들기 전에 거절한다.
    for (out, want) in [
        (
            issue(key_path, "0", "30"),
            "opl-license: --nodes must be at least 1",
        ),
        (
            issue(key_path, "3", "0"),
            "opl-license: --days must be at least 1",
        ),
    ] {
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(err.trim(), want);
    }
    // 내용이 틀리면 "키를 받았다"가 아니라 발급할 내용이 틀렸다고 한다.
    let out = opl_license(&[
        "issue",
        "--key",
        key_path,
        "--id",
        "Acme 2026",
        "--customer",
        "c",
        "--level",
        "premium",
        "--nodes",
        "1",
        "--days",
        "1",
    ]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("opl-license: invalid license: id must be"),
        "{err}"
    );
    // 비밀 키 파일이 없거나 키가 아님
    let missing = dir.path().join("missing.key");
    assert!(!issue(missing.to_str().unwrap(), "3", "30").status.success());
    let junk = dir.path().join("junk.key");
    std::fs::write(&junk, "AAAA\n").unwrap();
    assert!(!issue(junk.to_str().unwrap(), "3", "30").status.success());
    let out = opl_license(&["pubkey", "--key", junk.to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a PKCS#8 Ed25519 private key"), "{err}");
    // 모르는 수준
    let out = opl_license(&[
        "issue",
        "--key",
        key_path,
        "--id",
        "x",
        "--customer",
        "c",
        "--level",
        "gold",
        "--nodes",
        "1",
        "--days",
        "1",
    ]);
    assert!(!out.status.success());
}
