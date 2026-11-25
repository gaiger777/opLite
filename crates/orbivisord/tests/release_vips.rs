//! `orbivisord --release-vips`(systemd `ExecStopPost`)는 설정의 `data_dir`만 읽어 기록된 VIP를 떼고, 설정이나
//! 네트워크 오류가 있어도 0으로 끝난다(systemd 재시작을 막지 않게). 데이터 디렉터리 잠금을 쥔 orbivisord가
//! 돌고 있으면 떼지 않는다. 실제로 떼는 것은 Linux `net-it`의 `vip.rs`가 본다.

use std::path::Path;
use std::process::Command;

fn release_vips(config: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orbivisord"))
        .arg("--config")
        .arg(config)
        .arg("--release-vips")
        .output()
        .unwrap()
}

#[test]
fn release_vips_never_fails() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("opld.toml");
    // 설정 파일이 없다.
    let out = release_vips(&config);
    assert!(out.status.success(), "{out:?}");

    // 다른 설정이 틀려도 `data_dir`만 읽는다. 기록이 없으면 할 일이 없다.
    let data = dir.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(
        &config,
        format!("data_dir = {:?}\nunknown = 1\n", data.display().to_string()),
    )
    .unwrap();
    let out = release_vips(&config);
    assert!(out.status.success(), "{out:?}");

    let log_of = |out: &std::process::Output| {
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
    };
    std::fs::write(data.join("vips"), "oplnone0 192.0.2.1/24\n").unwrap();
    // 잠금을 쥔 프로세스(돌고 있는 orbivisord)가 있으면 아무것도 하지 않는다.
    let lock = std::fs::File::create(data.join("opld.lock")).unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let out = release_vips(&config);
    assert!(out.status.success(), "{out:?}");
    let log = log_of(&out);
    assert!(log.contains("is locked"), "{log}");
    assert!(!log.contains("cannot remove VIPs"), "{log}");
    drop(lock);

    let out = release_vips(&config);
    assert!(out.status.success(), "{out:?}");
    let log = log_of(&out);
    if cfg!(target_os = "linux") {
        // 없는 인터페이스의 것은 붙어 있을 수 없으므로 기록에서 뺀다.
        assert!(log.contains("the interface is gone"), "{log}");
        assert_eq!(std::fs::read_to_string(data.join("vips")).unwrap(), "");
    } else {
        // 떼지 못하면(macOS에는 `ip`가 없음) 경고하고 기록을 남긴다.
        assert!(log.contains("cannot remove VIPs"), "{log}");
        assert_eq!(
            std::fs::read_to_string(data.join("vips")).unwrap(),
            "oplnone0 192.0.2.1/24\n"
        );
    }
}
