//! 시험용 가짜 `apt-get`·`systemd-run`·`systemctl`·`dpkg`. 임시 디렉터리에 셸 스크립트를 두고 상태를
//! 파일로 흉내 낸다:
//!
//! - `apt-get`: 부를 때마다 `LANG=<값> <인자…>` 한 줄을 `apt-get.calls`에 더한다. 인자에 `-s`가
//!   있으면 `simulate.txt`를 출력한다. `update`면 `update.stderr`(있으면)를 표준 오류로 내고
//!   `update.exit`(없으면 0)로 끝난다.
//! - `systemd-run`: 인자를 한 줄에 하나씩 `systemd-run.args`에 쓴다. `systemd-run.exit`가 있으면
//!   `systemd-run.stderr`를 표준 오류로 내고 그 코드로 끝나고, 없으면 `unit.state`에 `active`를 쓴다.
//!   명령은 돌리지 않는다.
//! - `systemctl`: 인자를 `systemctl.calls`에 더하고 `unit.state`(`active`·`success`·`failed`, 없거나
//!   `gone`이면 사라진 유닛)에 맞는 `ActiveState=`·`Result=`·`LoadState=` 줄을 출력한다.
//! - `dpkg`: 인자를 `dpkg.calls`에 더하고 `audit.txt`(있으면)를 출력한다.
//!
//! ```ignore
//! let apt = FakeApt::new();
//! apt.set("simulate.txt", "Inst orbivisor [0.2.0] (0.2.1 opLite:trixie [amd64])\n");
//! let updates = Arc::new(Updates::new(apt.config()));
//! ```
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_api::UpdatesConfig;
use tempfile::TempDir;

const APT_GET: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "LANG=$LANG $*" >> "$d/apt-get.calls"
for a; do
    if [ "$a" = -s ]; then
        cat "$d/simulate.txt"
        exit 0
    fi
done
if [ "$1" = update ]; then
    if [ -f "$d/update.stderr" ]; then cat "$d/update.stderr" >&2; fi
    if [ -f "$d/update.exit" ]; then exit "$(cat "$d/update.exit")"; fi
fi
exit 0
"#;

const SYSTEMD_RUN: &str = r#"#!/bin/sh
d=$(dirname "$0")
: > "$d/systemd-run.args"
for a; do printf '%s\n' "$a" >> "$d/systemd-run.args"; done
if [ -f "$d/systemd-run.exit" ]; then
    if [ -f "$d/systemd-run.stderr" ]; then cat "$d/systemd-run.stderr" >&2; fi
    exit "$(cat "$d/systemd-run.exit")"
fi
echo active > "$d/unit.state"
"#;

const SYSTEMCTL: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/systemctl.calls"
state=gone
if [ -f "$d/unit.state" ]; then state=$(cat "$d/unit.state"); fi
case $state in
active) printf 'ActiveState=active\nResult=success\nLoadState=loaded\n' ;;
success) printf 'ActiveState=inactive\nResult=success\nLoadState=loaded\n' ;;
failed) printf 'ActiveState=failed\nResult=exit-code\nLoadState=loaded\n' ;;
*) printf 'ActiveState=inactive\nResult=\nLoadState=not-found\n' ;;
esac
"#;

const DPKG: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/dpkg.calls"
if [ -f "$d/audit.txt" ]; then cat "$d/audit.txt"; fi
"#;

pub struct FakeApt {
    pub dir: TempDir,
}

impl FakeApt {
    pub fn new() -> Self {
        let apt = Self {
            dir: TempDir::new().unwrap(),
        };
        write_script(&apt.path("apt-get"), APT_GET);
        write_script(&apt.path("systemd-run"), SYSTEMD_RUN);
        write_script(&apt.path("systemctl"), SYSTEMCTL);
        write_script(&apt.path("dpkg"), DPKG);
        apt.set("simulate.txt", "");
        apt.set("keyring.gpg", "");
        apt
    }

    /// 디렉터리 안의 파일 경로
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// 상태 파일을 쓴다.
    pub fn set(&self, name: &str, contents: &str) {
        std::fs::write(self.path(name), contents).unwrap();
    }

    /// 가짜 `apt-get`이 받은 호출(`LANG=<값> <인자…>`)
    pub fn calls(&self) -> Vec<String> {
        self.lines("apt-get.calls")
    }

    /// 파일의 줄(없으면 빈 목록). `systemd-run.args`, `systemctl.calls`, `dpkg.calls` 등
    pub fn lines(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.path(name))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// 가짜 명령을 쓰는 업데이트 설정. 주기 확인은 시험 중에 돌지 않을 만큼 길다. 소스 파일은
    /// 쓰지 않는다(`apt_url`이 비었다). 저장소 공개 키는 빈 `keyring.gpg`다.
    pub fn config(&self) -> UpdatesConfig {
        UpdatesConfig {
            check_interval: Duration::from_secs(3600),
            first_check: Duration::from_secs(3600),
            apt_url: String::new(),
            sources_file: self.path("oplite.sources"),
            keyring: self.path("keyring.gpg"),
            auth_file: self.path("oplite.conf"),
            apt_get: self.path("apt-get"),
            systemd_run: self.path("systemd-run"),
            systemctl: self.path("systemctl"),
            dpkg: self.path("dpkg"),
            log_dir: self.path("log"),
            poll: Duration::from_millis(20),
        }
    }
}

impl Default for FakeApt {
    fn default() -> Self {
        Self::new()
    }
}

/// 실행 파일 셸 스크립트를 쓴다. 이 프로세스가 연 쓰기 핸들이 다른 스레드의 fork에 새어 실행이
/// `ETXTBSY`로 실패하지 않도록 `sh`가 쓴다.
fn write_script(path: &Path, text: &str) {
    use std::io::Write;
    let mut sh = std::process::Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    sh.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    assert!(sh.wait().unwrap().success(), "cannot write {path:?}");
}
