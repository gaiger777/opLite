//! `oplctl --version`이 빌드 버전을 보이는지 본다. 기대값은 이 시험을 빌드할 때의 `OPL_VERSION`(비었으면
//! 크레이트 버전)으로 따로 구해, clap 기본 `version`(크레이트 버전)으로 되돌리면 OPL_VERSION을 준 시험이 실패한다.

use std::process::Command;

#[test]
fn version_flag_prints_build_version() {
    let want = match option_env!("OPL_VERSION") {
        Some(v) if !v.is_empty() => v,
        _ => env!("CARGO_PKG_VERSION"),
    };
    let out = Command::new(env!("CARGO_BIN_EXE_oplctl"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("oplctl {want}\n")
    );
}
