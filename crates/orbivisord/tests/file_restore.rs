//! 파일 단위 복원 E2E(계획 69 설계 §14 opld E2E (1)~(4)): 실제 도우미 이미지와 QEMU로, orbivisorctl `backup files`를 거쳐
//! (1) 고정물 ext4 디스크를 단 꺼진 VM을 로컬 암호화 저장소에 백업해 나열·파일·zip·tar.gz를 고정물 해시와 견주고 닫은 뒤
//! 도우미 프로세스·세션 디렉터리가 남지 않는지, (2) 같은 디스크의 옛 풀 증분 체인(전체 → 디스크를 저널이 더러운 ext4로 바꾼
//! 증분)을 열면 체인을 다 붙인 디스크(재생된 저널)가 보이는지, (3) 도우미 QEMU를 `kill -9`하면 세션이 실패로 끝나고 치워지는지,
//! (4) 세션이 열린 채 opld가 죽었다가 다시 뜨면 남은 도우미를 치우고 세션은 실패로 남는지, (5) 실제 게스트 에이전트(시험
//! 게스트 — `packaging/restore-image/build.sh --test-guest`, Debian qemu-ga)가 도는 VM에 디렉터리·keep·overwrite로 되돌려
//! 쓴 뒤 그 VM을 끄고 다시 백업해 세션으로 열면 쓴 파일이 고정물과 같은지 본다. (5)는 `ORBIVISOR_RESTORE_TEST_GUEST=<시험 게스트
//! 디렉터리>`가 있을 때만 — 시험 게스트 VM은 QEMU 감싸개가 `-kernel`·`-initrd`를 더해 띄운다(VM 스펙에 직접 커널 부팅이 없다).
//! `ORBIVISOR_RESTORE_IMAGE_DIR=<이미지> ORBIVISOR_RESTORE_FIXTURES=<고정물> cargo test -p orbivisord --features restore-it --test file_restore`
//! (QEMU가 PATH 밖이면 `ORBIVISOR_TEST_QEMU=<qemu-system-…>`, 가속 `ORBIVISOR_TEST_ACCEL`, 기본 tcg; 배포판 UEFI 펌웨어가 `-kernel`을
//! 띄우지 못하면 `ORBIVISOR_TEST_UEFI_CODE`·`ORBIVISOR_TEST_UEFI_VARS`)
#![cfg(feature = "restore-it")]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Daemon, eventually};
use serde_json::Value;
use sha2::{Digest, Sha256};

struct Inputs {
    image_dir: PathBuf,
    fixtures: PathBuf,
}

fn inputs() -> Option<Inputs> {
    let (Some(image_dir), Some(fixtures)) = (
        std::env::var_os("ORBIVISOR_RESTORE_IMAGE_DIR"),
        std::env::var_os("ORBIVISOR_RESTORE_FIXTURES"),
    ) else {
        eprintln!("skipping: set ORBIVISOR_RESTORE_IMAGE_DIR and ORBIVISOR_RESTORE_FIXTURES");
        return None;
    };
    Some(Inputs {
        image_dir: image_dir.into(),
        fixtures: fixtures.into(),
    })
}

fn run(cmd: &str, args: &[&str], dir: &Path) {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{cmd} {args:?}: {out:?}");
}

/// 고정물 원본 나무의 일반 파일 해시(`expected.sha256`)
fn expected(fixtures: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(fixtures.join("expected.sha256"))
        .unwrap()
        .lines()
        .filter_map(|l| {
            let (hash, name) = l.split_once("  ")?;
            Some((name.to_string(), hash.to_string()))
        })
        .collect()
}

fn sha256(file: &Path) -> String {
    hex(&Sha256::digest(std::fs::read(file).unwrap()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 파일 복원을 켠 노드 하나, 풀 `local`·백업 풀 `bk`
async fn daemon(inputs: &Inputs) -> Daemon {
    daemon_with(inputs, None).await
}

/// `daemon`과 같고 QEMU 실행 파일을 `qemu`로(시험 게스트 감싸개)
async fn daemon_with(inputs: &Inputs, qemu: Option<&Path>) -> Daemon {
    let extra = format!(
        "[reconciler]\nacpi_timeout_secs = 5\n[file_restore]\nimage_dir = \"{}\"\nrun_as = \"\"\n",
        inputs.image_dir.display()
    );
    // QEMU가 PATH 밖이면(svc01의 /opt/qemu) `ORBIVISOR_TEST_QEMU`·`ORBIVISOR_TEST_ACCEL`로 준다.
    let mut d = match qemu
        .map(|q| q.display().to_string())
        .ok_or(())
        .or_else(|()| std::env::var("ORBIVISOR_TEST_QEMU"))
    {
        Ok(qemu) => {
            // 시험 게스트 감싸개만이면(`ORBIVISOR_TEST_QEMU` 없음) 호스트에 맞는 가속(KVM·HVF)
            let fallback = if std::env::var_os("ORBIVISOR_TEST_QEMU").is_some() {
                "tcg"
            } else {
                "auto"
            };
            let accel = std::env::var("ORBIVISOR_TEST_ACCEL").unwrap_or_else(|_| fallback.into());
            let scope = std::env::var_os("ORBIVISOR_TEST_SYSTEMD").is_some();
            // 배포판 펌웨어가 `-kernel`을 띄우지 못하면(svc01 Rocky AAVMF) QEMU에 딸린 edk2를 준다.
            let mut uefi = String::new();
            if let (Ok(code), Ok(vars)) = (
                std::env::var("ORBIVISOR_TEST_UEFI_CODE"),
                std::env::var("ORBIVISOR_TEST_UEFI_VARS"),
            ) {
                uefi = format!("\nuefi_code = \"{code}\"\nuefi_vars = \"{vars}\"");
            }
            Daemon::with_hypervisor(
                "node1",
                &extra,
                &format!(
                    "qemu_binary = \"{qemu}\"\naccel = \"{accel}\"\nsystemd_scope = {scope}{uefi}"
                ),
            )
        }
        Err(_) => Daemon::new("node1", &extra),
    };
    d.start().await;
    d.init("fr-e2e").await;
    d.login().await;
    for (name, extra) in [("local", None), ("bk", Some("--backups"))] {
        let dir = d.path(name);
        std::fs::create_dir(&dir).unwrap();
        let mut args = vec!["pool", "create", name, "--path", dir.to_str().unwrap()];
        args.extend(extra);
        d.ctl(&args).await.unwrap();
        d.wait_pool_ready(name).await;
    }
    // 노드가 이미지를 검사해 알린다.
    eventually("the node reports file restore", || async {
        let nodes = d.ctl_json(&["node", "list"]).await;
        nodes[0]["status"]["file_restore"]["image_version"].is_string()
    })
    .await;
    d
}

/// 풀 `local`에 64 MiB qcow2 볼륨 `name`을 만들고 고정물 `image`(raw)를 담는다. 파일 경로를 돌려준다.
async fn fixture_volume(d: &Daemon, name: &str, image: &Path) -> PathBuf {
    d.ctl(&[
        "volume", "create", name, "--pool", "local", "--size", "64M", "--format", "qcow2",
    ])
    .await
    .unwrap();
    let v = d.ctl_json(&["volume", "get", name]).await;
    let file = d
        .path("local")
        .join(format!("{}.qcow2", v["id"].as_str().unwrap()));
    write_disk(&file, image);
    file
}

/// qcow2 디스크의 내용을 raw 이미지로 바꾼다(qcow2의 persistent 비트맵이 이 쓰기를 적는다).
fn write_disk(file: &Path, image: &Path) {
    run(
        "qemu-img",
        &[
            "convert",
            "-n",
            "-f",
            "raw",
            "-O",
            "qcow2",
            image.to_str().unwrap(),
            file.to_str().unwrap(),
        ],
        Path::new("/"),
    );
}

/// 세션 id(백업의 내 세션)
async fn session_id(d: &Daemon, backup: &str) -> String {
    let out = d
        .ctl(&["backup", "files", "open", backup, "--wait"])
        .await
        .unwrap();
    assert!(out.ends_with("(ready)\n"), "{out}");
    out.split_whitespace().nth(3).unwrap().to_string()
}

/// 세션의 도우미 QEMU pid(명령줄의 `opl-fr-<id>`)
fn helper_pid(id: &str) -> Option<u32> {
    let out = Command::new("pgrep")
        .args(["-f", &format!("opl-fr-{id}")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
}

/// 세션의 도우미·디렉터리가 남지 않았다.
async fn gone(d: &Daemon, id: &str) {
    eventually(&format!("session {id} cleaned up"), || async {
        helper_pid(id).is_none()
            && !d.path("run").join("file-restore").join(id).exists()
            && !d.data_dir().join("file-restore").join(id).exists()
    })
    .await;
}

async fn phase(d: &Daemon, id: &str) -> Value {
    let sessions = d.ctl_json(&["backup", "files", "sessions"]).await;
    sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

/// (1) 암호화 저장소 백업: 볼륨·나열·파일·zip·tar.gz를 고정물과 견주고, 닫으면 도우미와 세션 디렉터리가 남지 않는다.
#[tokio::test]
async fn store_backup_files_are_listed_downloaded_and_cleaned_up() {
    let Some(inputs) = inputs() else { return };
    let d = daemon(&inputs).await;
    fixture_volume(&d, "root", &inputs.fixtures.join("ext4.img")).await;
    d.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    let out = d
        .ctl(&["backup-store", "create", "st", "--pool", "bk", "--encrypt"])
        .await
        .unwrap();
    assert!(out.contains("backup store st created"), "{out}");
    d.ctl(&["backup", "create", "web", "--store", "st", "--name", "sb"])
        .await
        .unwrap();

    let id = session_id(&d, "sb").await;
    assert!(helper_pid(&id).is_some());
    let vols = d.ctl(&["backup", "files", "volumes", "sb"]).await.unwrap();
    assert!(
        vols.lines()
            .any(|l| l.starts_with("disk0 ") && l.contains("ext4") && l.contains("opl-ext4")),
        "{vols}"
    );
    let ls = d
        .ctl(&["backup", "files", "ls", "sb", "disk0:/"])
        .await
        .unwrap();
    for name in [
        "hello.txt",
        "dir/",
        "empty/",
        "link -> hello.txt",
        "big.bin",
    ] {
        assert!(ls.lines().any(|l| l == name), "{name}: {ls}");
    }
    let long = d
        .ctl(&["backup", "files", "ls", "sb", "disk0:/dir/nested", "--long"])
        .await
        .unwrap();
    assert!(long.contains("deep.txt") && long.contains("-rw-"), "{long}");

    let want = expected(&inputs.fixtures);
    let work = d.path("got");
    std::fs::create_dir(&work).unwrap();
    // 파일 하나(raw)
    let file = work.join("big.bin");
    d.ctl(&[
        "backup",
        "files",
        "get",
        "sb",
        "disk0:/big.bin",
        "-O",
        file.to_str().unwrap(),
    ])
    .await
    .unwrap();
    assert_eq!(sha256(&file), want["big.bin"]);
    // 볼륨 전체 → zip(링크는 보고서에)
    let zip = work.join("all.zip");
    d.ctl(&[
        "backup",
        "files",
        "get",
        "sb",
        "disk0:/",
        "--format",
        "zip",
        "-O",
        zip.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let unzipped = work.join("zip");
    std::fs::create_dir(&unzipped).unwrap();
    run("unzip", &["-q", zip.to_str().unwrap()], &unzipped);
    for (name, hash) in &want {
        assert_eq!(&sha256(&unzipped.join(name)), hash, "{name} in zip");
    }
    let report = std::fs::read_to_string(unzipped.join("ORBIVISOR-RESTORE-REPORT.txt")).unwrap();
    assert!(report.contains("link"), "{report}");
    // 여러 항목 → tar.gz(링크·하드 링크 보존)
    let tgz = work.join("some.tar.gz");
    d.ctl(&[
        "backup",
        "files",
        "get",
        "sb",
        "disk0:/dir",
        "disk0:/hello.txt",
        "disk0:/hard.txt",
        "disk0:/link",
        "--format",
        "tar.gz",
        "-O",
        tgz.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let untarred = work.join("tar");
    std::fs::create_dir(&untarred).unwrap();
    run("tar", &["-xzf", tgz.to_str().unwrap()], &untarred);
    for name in ["dir/nested/deep.txt", "hello.txt", "hard.txt"] {
        assert_eq!(sha256(&untarred.join(name)), want[name], "{name} in tar.gz");
    }
    assert_eq!(
        std::fs::read_link(untarred.join("link")).unwrap(),
        Path::new("hello.txt")
    );

    // 닫으면 도우미·덮개·소켓이 남지 않는다.
    let out = d.ctl(&["backup", "files", "close", "sb"]).await.unwrap();
    assert_eq!(out, format!("file restore session {id} closed\n"));
    gone(&d, &id).await;
    eventually("the session is closed", || async {
        phase(&d, &id).await.is_null()
    })
    .await;
    let log = d.log();
    assert!(log.contains("file restore session ended"), "{log}");
}

/// (2) 옛 풀 증분 체인: 전체 백업 뒤 디스크를 바꾼 증분을 열면 체인을 다 붙인 디스크가 보인다(바뀐 블록 — 커밋만 된 저널
/// 트랜잭션 — 이 덮개 위에서 재생된다). 전체 백업은 옛 내용 그대로다.
#[tokio::test]
async fn an_incremental_pool_chain_shows_the_latest_disk() {
    let Some(inputs) = inputs() else { return };
    let d = daemon(&inputs).await;
    let root = fixture_volume(&d, "root", &inputs.fixtures.join("ext4.img")).await;
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    for name in ["full", "incr"] {
        if name == "incr" {
            // 게스트가 없어 VM을 끄고 쓴다(persistent 비트맵이 이 쓰기를 적는다 — backup.rs의 증분 시험과 같다).
            d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
            write_disk(&root, &inputs.fixtures.join("ext4-dirty.img"));
            d.ctl(&["vm", "start", "web"]).await.unwrap();
        }
        let out = d
            .ctl(&[
                "backup",
                "create",
                "web",
                "--pool",
                "bk",
                "--name",
                name,
                "--incremental",
            ])
            .await
            .unwrap();
        assert!(
            out.starts_with(&format!("backup {name} of vm web created")),
            "{out}"
        );
    }
    let incr = d.ctl_json(&["backup", "get", "incr"]).await;
    assert!(incr["status"]["parent"].is_string(), "{incr}");

    let work = d.path("got");
    std::fs::create_dir(&work).unwrap();
    for (backup, starts) in [
        ("incr", "journal replayed"),
        ("full", "hello from orbivisor"),
    ] {
        let file = work.join(format!("{backup}-hello.txt"));
        d.ctl(&[
            "backup",
            "files",
            "get",
            backup,
            "disk0:/hello.txt",
            "-O",
            file.to_str().unwrap(),
        ])
        .await
        .unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with(starts), "{backup}: {text:?}");
    }
    // 세션은 사용자·백업마다 하나 — 둘 다 열려 있다.
    let sessions = d.ctl_json(&["backup", "files", "sessions"]).await;
    assert_eq!(sessions.as_array().unwrap().len(), 2, "{sessions}");
    for backup in ["incr", "full"] {
        d.ctl(&["backup", "files", "close", backup]).await.unwrap();
    }
    eventually("both sessions closed", || async {
        d.ctl_json(&["backup", "files", "sessions"])
            .await
            .as_array()
            .unwrap()
            .is_empty()
    })
    .await;
    // 세션이 닫힌 뒤에는 백업을 지울 수 있다(체인의 자식부터).
    d.ctl(&["backup", "delete", "incr"]).await.unwrap();
}

/// (3) 도우미 QEMU가 죽으면(`kill -9`) 세션은 실패로 끝나고 디렉터리가 치워진다. (4) 세션이 열린 채 opld가 죽었다가 다시
/// 뜨면 시작 때 치우기가 남은 도우미 QEMU·디렉터리를 지우고 세션 Task는 끊긴 것으로 실패한다.
#[tokio::test]
async fn killed_helpers_and_restarts_leave_nothing_behind() {
    let Some(inputs) = inputs() else { return };
    let mut d = daemon(&inputs).await;
    fixture_volume(&d, "root", &inputs.fixtures.join("ext4.img")).await;
    d.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    d.ctl(&["backup", "create", "web", "--pool", "bk", "--name", "b1"])
        .await
        .unwrap();

    // (3)
    let id = session_id(&d, "b1").await;
    let pid = helper_pid(&id).expect("helper QEMU");
    run("kill", &["-9", &pid.to_string()], Path::new("/"));
    gone(&d, &id).await;
    eventually("the session task failed", || async {
        let tasks = d.ctl_json(&["task", "list"]).await;
        tasks.as_array().unwrap().iter().any(|t| {
            t["spec"]["target"] == id.as_str()
                && t["status"]["state"] == "failed"
                && t["status"]["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("exited"))
        })
    })
    .await;
    // 실패한 세션 뒤의 명령은 새 세션을 연다.
    let ls = d
        .ctl(&["backup", "files", "ls", "b1", "disk0:/"])
        .await
        .unwrap();
    assert!(ls.lines().any(|l| l == "hello.txt"), "{ls}");

    // (4) 지금 열린 세션(위의 ls가 연 것)을 두고 opld를 죽인다.
    let sessions = d.ctl_json(&["backup", "files", "sessions"]).await;
    let open = sessions[0]["id"].as_str().unwrap().to_string();
    assert!(helper_pid(&open).is_some());
    d.kill();
    d.start().await;
    gone(&d, &open).await;
    eventually("the interrupted session task failed", || async {
        let tasks = d.ctl_json(&["task", "list"]).await;
        tasks.as_array().unwrap().iter().any(|t| {
            t["spec"]["target"] == open.as_str()
                && t["status"]["state"] == "failed"
                && t["status"]["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("interrupted"))
        })
    })
    .await;
    // 다시 열 수 있다.
    let again = session_id(&d, "b1").await;
    assert_ne!(again, open);
    d.ctl(&["backup", "files", "close", "b1"]).await.unwrap();
    gone(&d, &again).await;
}

/// 이 호스트의 QEMU(`ORBIVISOR_TEST_QEMU`, 없으면 PATH의 `qemu-system-<arch>`) 절대 경로
fn real_qemu() -> String {
    if let Ok(q) = std::env::var("ORBIVISOR_TEST_QEMU") {
        return q;
    }
    let name = format!("qemu-system-{}", std::env::consts::ARCH);
    let out = Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .unwrap();
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!path.is_empty(), "{name} is not on PATH");
    path
}

/// 시험 게스트 VM(`-name guest=tguest,…`)에만 `-kernel`·`-initrd`·`-append`를 더하는 QEMU 감싸개. 도우미·탐침은 그대로.
fn guest_wrapper(dir: &Path, guest: &Path) -> PathBuf {
    let (kernel, console) = if std::env::consts::ARCH == "x86_64" {
        ("vmlinuz", "ttyS0")
    } else {
        ("Image", "ttyAMA0")
    };
    let script = format!(
        "#!/bin/sh\ncase \" $* \" in\n*\" guest=tguest,\"*)\n\texec '{qemu}' \"$@\" -kernel '{k}' -initrd '{i}' -append 'console={console} panic=-1' ;;\nesac\nexec '{qemu}' \"$@\"\n",
        qemu = real_qemu(),
        k = guest.join(kernel).display(),
        i = guest.join("initrd.img").display(),
    );
    let path = dir.join("qemu-wrap");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// 백업 `backup`의 `disk0:<path>`를 받아 SHA-256
async fn got_hash(d: &Daemon, backup: &str, path: &str, work: &Path) -> String {
    let file = work.join("got.bin");
    d.ctl(&[
        "backup",
        "files",
        "get",
        backup,
        &format!("disk0:{path}"),
        "-O",
        file.to_str().unwrap(),
    ])
    .await
    .unwrap();
    sha256(&file)
}

/// (5) 실제 게스트 에이전트로 되돌려 쓰기: 디렉터리(새 자리), keep(있는 hello.txt는 두고 `.restored-…`), overwrite(hard.txt를
/// 임시 파일 → mv), 3 MiB 파일(1 MiB 쓰기 셋). VM을 끄고 다시 백업해 세션으로 열면 쓴 파일이 고정물과 같다.
#[tokio::test]
async fn files_are_restored_into_a_real_guest_agent() {
    let Some(inputs) = inputs() else { return };
    let Some(guest) = std::env::var_os("ORBIVISOR_RESTORE_TEST_GUEST") else {
        eprintln!(
            "skipping (5): set ORBIVISOR_RESTORE_TEST_GUEST (packaging/restore-image/build.sh --test-guest)"
        );
        return;
    };
    let wrap = tempfile::Builder::new()
        .prefix("oplw")
        .tempdir_in("/tmp")
        .unwrap();
    let qemu = guest_wrapper(wrap.path(), Path::new(&guest));
    let d = daemon_with(&inputs, Some(&qemu)).await;
    let want = expected(&inputs.fixtures);
    // 원본: 고정물 ext4 디스크 VM의 저장소 백업
    fixture_volume(&d, "src", &inputs.fixtures.join("ext4.img")).await;
    d.ctl(&["vm", "create", "src", "--memory", "256", "--disk", "src"])
        .await
        .unwrap();
    d.ctl(&["backup-store", "create", "st", "--pool", "bk"])
        .await
        .unwrap();
    d.ctl(&["backup", "create", "src", "--store", "st", "--name", "sb"])
        .await
        .unwrap();
    // 대상: 저널이 더러운 ext4(게스트가 붙일 때 재생 — hello.txt·hard.txt가 "journal replayed…")를 단 시험 게스트
    fixture_volume(&d, "gdisk", &inputs.fixtures.join("ext4-dirty.img")).await;
    d.ctl(&[
        "vm",
        "create",
        "tguest",
        "--memory",
        "512",
        "--disk",
        "gdisk",
        "--guest-agent",
        "on",
        "--start",
    ])
    .await
    .unwrap();
    let started = std::time::Instant::now();
    eventually("the test guest agent answers", || async {
        let g = d.ctl_json(&["vm", "guest", "tguest", "--refresh"]).await;
        g["state"] == "answered"
    })
    .await;
    eprintln!("test guest agent answered after {:?}", started.elapsed());

    let restore = |paths: &'static [&'static str], to: &'static str, conflict: &'static str| {
        let d = &d;
        async move {
            let mut args = vec!["backup", "files", "restore", "sb"];
            args.extend(paths);
            args.extend([
                "--vm",
                "tguest",
                "--to",
                to,
                "--conflict",
                conflict,
                "--wait",
            ]);
            let t = std::time::Instant::now();
            let out = d.ctl(&args).await.unwrap();
            eprintln!(
                "restore {paths:?} → {to} ({conflict}) in {:?}: {out}",
                t.elapsed()
            );
            out
        }
    };
    let out = restore(
        &["disk0:/dir", "disk0:/big.bin"],
        "/target/restored",
        "keep",
    )
    .await;
    assert!(out.contains("restored 2 file(s)"), "{out}");
    assert!(
        out.contains("owners, modes and times were not restored"),
        "{out}"
    );
    let out = restore(&["disk0:/hello.txt"], "/target", "keep").await;
    assert!(
        out.contains("kept existing files, wrote hello.txt as /target/hello.txt.restored-"),
        "{out}"
    );
    let out = restore(&["disk0:/hard.txt"], "/target", "overwrite").await;
    assert!(out.contains("restored 1 file(s)"), "{out}");
    // 쓰기 속도(세션이 이미 열려 있음 — 3 MiB, 1 MiB `guest-file-write` 셋)
    let t = std::time::Instant::now();
    restore(&["disk0:/big.bin"], "/target/speed", "keep").await;
    eprintln!(
        "guest write: {:.1} MB/s",
        3.0 * 1.048_576 / t.elapsed().as_secs_f64()
    );
    let out = restore(&["disk0:/link"], "/target/restored", "keep").await;
    assert!(out.contains("symbolic links are not restored"), "{out}");

    // 게스트가 1초마다 sync한다 — 그 뒤 끄고 다시 백업해 세션으로 본다.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    d.ctl(&["vm", "stop", "tguest", "--force"]).await.unwrap();
    d.ctl(&[
        "backup", "create", "tguest", "--store", "st", "--name", "gb",
    ])
    .await
    .unwrap();
    let work = d.path("got5");
    std::fs::create_dir(&work).unwrap();
    for (path, name) in [
        ("/restored/dir/nested/deep.txt", "dir/nested/deep.txt"),
        ("/restored/big.bin", "big.bin"),
        ("/hard.txt", "hard.txt"),
    ] {
        assert_eq!(got_hash(&d, "gb", path, &work).await, want[name], "{path}");
    }
    let ls = d
        .ctl(&["backup", "files", "ls", "gb", "disk0:/"])
        .await
        .unwrap();
    let kept = ls
        .lines()
        .find(|l| l.starts_with("hello.txt.restored-"))
        .unwrap_or_else(|| panic!("no kept copy: {ls}"))
        .to_string();
    assert_eq!(
        got_hash(&d, "gb", &format!("/{kept}"), &work).await,
        want["hello.txt"]
    );
    assert!(!ls.contains(".orbivisor-"), "{ls}");
    let hello = work.join("hello.txt");
    d.ctl(&[
        "backup",
        "files",
        "get",
        "gb",
        "disk0:/hello.txt",
        "-O",
        hello.to_str().unwrap(),
    ])
    .await
    .unwrap();
    assert!(
        std::fs::read_to_string(&hello)
            .unwrap()
            .starts_with("journal replayed"),
        "the existing hello.txt was replaced"
    );
    let restored = d
        .ctl(&["backup", "files", "ls", "gb", "disk0:/restored"])
        .await
        .unwrap();
    assert!(!restored.contains("link"), "{restored}");
    for b in ["sb", "gb"] {
        d.ctl(&["backup", "files", "close", b]).await.unwrap();
    }
}
