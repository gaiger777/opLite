//! VM마다 하나인 swtpm(TPM 2.0 에뮬레이터, 계획 59)과 노드 로컬 TPM 상태 파일.
//!
//! swtpm은 QEMU처럼 opld에서 떼어 띄운다(`--daemon`). `--terminate` 덕분에 QEMU가 제어 채널을 닫으면(VM 끝,
//! 마이그레이션 원본 끝) 스스로 끝난다. 주인 확인은 pid 파일의 pid가 살아 있고 그 프로세스의 명령줄에 이 VM의 제어
//! 소켓 경로가 있는지로 한다. swtpm의 `.lock`(TPM 디렉터리의 fcntl 잠금)은 TPM을 초기화할 때 잡고 마이그레이션
//! 옵션으로 놓을 수도 있어 프로세스 수명과 같다고 믿지 않는다(설계 §16).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use opl_qemu::RuntimePaths;
use opl_types::backup::TPM_STATE_FILE;
use opl_types::runtime::RuntimeTpm;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use uuid::Uuid;

use crate::error::DriverError;
use crate::qemu::vm_state_dir;

/// swtpm이 제어 소켓을 만들기를 기다리는 시간
pub const SWTPM_START_TIMEOUT: Duration = Duration::from_secs(5);
/// `swtpm_setup`(EK·인증서 만들기)을 기다리는 시간
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
/// 능력 확인 명령(`--print-capabilities`, `-tpmdev help`)을 기다리는 시간
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// swtpm을 죽인 뒤 끝나기를 기다리는 시간
const KILL_TIMEOUT: Duration = Duration::from_secs(10);
/// 노드의 펌웨어 초기화 회차 파일 이름(`{state}/vm/{id}/firmware.epoch`)
const EPOCH_FILE: &str = "firmware.epoch";
/// swtpm 로그 파일 이름(실행 디렉터리)
const LOG_FILE: &str = "swtpm.log";

/// swtpm 실행 파일. 이름만 주면 `PATH`에서 찾는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwtpmConfig {
    pub swtpm: PathBuf,
    pub swtpm_setup: PathBuf,
}

impl Default for SwtpmConfig {
    fn default() -> Self {
        Self {
            swtpm: PathBuf::from("swtpm"),
            swtpm_setup: PathBuf::from("swtpm_setup"),
        }
    }
}

/// VM의 TPM 상태 디렉터리 `{state_dir}/vm/{id}/tpm`(0700)
pub fn vm_tpm_dir(state_dir: &Path, id: Uuid) -> PathBuf {
    vm_state_dir(state_dir, id).join("tpm")
}

/// VM의 TPM 영구 상태 파일 `{state_dir}/vm/{id}/tpm/tpm2-00.permall`(swtpm이 0600으로 쓴다)
pub fn tpm_state_path(state_dir: &Path, id: Uuid) -> PathBuf {
    vm_tpm_dir(state_dir, id).join(TPM_STATE_FILE)
}

/// 이 노드에 있는 VM 펌웨어 파일(UEFI 변수·TPM)의 초기화 회차. 파일이 없으면 0이다.
pub fn read_firmware_epoch(state_dir: &Path, id: Uuid) -> std::io::Result<u32> {
    match std::fs::read_to_string(vm_state_dir(state_dir, id).join(EPOCH_FILE)) {
        Ok(text) => text.trim().parse().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad firmware epoch {text:?} of VM {id}"),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

/// 이 노드의 VM 펌웨어 파일 회차를 적는다(임시 파일에 쓰고 rename). 다른 노드에서 받아 온 UEFI 변수·TPM 파일을 둔
/// 뒤에도 VM의 `firmware_epoch`로 적어야 켤 때 지워지지 않는다.
pub fn write_firmware_epoch(state_dir: &Path, id: Uuid, epoch: u32) -> std::io::Result<()> {
    let dir = vm_state_dir(state_dir, id);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{EPOCH_FILE}.tmp"));
    std::fs::write(&tmp, epoch.to_string())?;
    std::fs::rename(&tmp, dir.join(EPOCH_FILE))
}

/// 켜기 전에 TPM 상태를 맞춘다.
///
/// 1. `tpm.epoch`가 이 노드의 회차와 다르면 이 노드의 파일은 초기화 전의 것이다. UEFI 변수(`uefi_vars`)를 지우고,
///    `tpm.fresh`면 TPM 상태도 지운 뒤 회차를 적는다. `fresh`가 아니면 TPM 파일은 해시가 맞는 지금 것이거나 막 받아
///    온 것이라 그대로 둔다.
/// 2. TPM 디렉터리를 0700으로 만든다.
/// 3. 마이그레이션을 받는 쪽(`incoming`)이 아니고 상태 파일이 없으면, `fresh`면 `swtpm_setup`으로 새로 만들고(EK와
///    인증서, 있는 파일은 덮어쓰지 않음) 아니면 몰래 새로 만들지 않고 실패한다.
pub(crate) async fn prepare_state(
    cfg: &SwtpmConfig,
    state_dir: &Path,
    id: Uuid,
    tpm: RuntimeTpm,
    incoming: bool,
    uefi_vars: &Path,
) -> Result<(), DriverError> {
    use std::os::unix::fs::PermissionsExt;

    let dir = vm_tpm_dir(state_dir, id);
    if read_firmware_epoch(state_dir, id)? != tpm.epoch {
        tracing::info!(%id, epoch = tpm.epoch, fresh = tpm.fresh,
            "firmware files on this node are from before a reset; removing them");
        remove_file(uefi_vars)?;
        if tpm.fresh {
            match std::fs::remove_dir_all(&dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        write_firmware_epoch(state_dir, id, tpm.epoch)?;
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    if incoming || dir.join(TPM_STATE_FILE).exists() {
        return Ok(());
    }
    if !tpm.fresh {
        return Err(DriverError::InvalidSpec(format!(
            "the TPM state of VM {id} is missing on this node"
        )));
    }
    setup(cfg, &dir).await
}

/// `swtpm_setup`으로 새 TPM 2.0 상태(EK, EK·플랫폼 인증서, NVRAM 잠금)를 만든다.
async fn setup(cfg: &SwtpmConfig, dir: &Path) -> Result<(), DriverError> {
    let output = tokio::process::Command::new(&cfg.swtpm_setup)
        .args(setup_args(dir))
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(SETUP_TIMEOUT, output)
        .await
        .map_err(|_| DriverError::Timeout("swtpm_setup"))?
        .map_err(|e| DriverError::StartFailed(format!("swtpm_setup: {e}")))?;
    if !output.status.success() {
        let mut msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
        return Err(DriverError::StartFailed(format!("swtpm_setup: {msg}")));
    }
    if !dir.join(TPM_STATE_FILE).exists() {
        return Err(DriverError::StartFailed(format!(
            "swtpm_setup: no {TPM_STATE_FILE} in {}",
            dir.display()
        )));
    }
    Ok(())
}

fn setup_args(dir: &Path) -> Vec<String> {
    [
        "--tpm2",
        "--tpm-state",
        &dir.display().to_string(),
        "--createek",
        "--create-ek-cert",
        "--create-platform-cert",
        "--lock-nvram",
        "--not-overwrite",
    ]
    .map(String::from)
    .to_vec()
}

/// swtpm의 인자. 상태 디렉터리의 쉼표는 두 번 써서 이스케이프한다.
pub(crate) fn swtpm_args(paths: &RuntimePaths, tpm_dir: &Path) -> Vec<String> {
    let esc = |p: &Path| p.display().to_string().replace(',', ",,");
    vec![
        "socket".into(),
        "--tpm2".into(),
        "--tpmstate".into(),
        format!("dir={},mode=0600", esc(tpm_dir)),
        "--ctrl".into(),
        format!("type=unixio,path={},mode=0600", esc(&paths.tpm_socket)),
        "--pid".into(),
        format!("file={}", esc(&paths.tpm_pidfile)),
        "--log".into(),
        format!("file={},level=1", esc(&paths.dir.join(LOG_FILE))),
        "--daemon".into(),
        "--terminate".into(),
    ]
}

/// QEMU를 감싸는 것과 같은 모양의 `systemd-run` 인자(`opl-tpm-{id}.scope`)
fn scope_args(id: Uuid) -> Vec<String> {
    vec![
        "--scope".into(),
        "--quiet".into(),
        "--collect".into(),
        format!("--unit=opl-tpm-{id}"),
        "--property=Before=opld.service".into(),
        "--property=After=network-online.target remote-fs.target".into(),
    ]
}

/// VM `id`의 swtpm을 띄우고 제어 소켓이 생기기를 기다린다([`SWTPM_START_TIMEOUT`]). 남은 swtpm(QEMU 없이 남음)은
/// 먼저 끝낸다. 실패하면 띄운 swtpm을 끝내고 `StartFailed("swtpm: …")`다.
pub(crate) async fn start(
    cfg: &SwtpmConfig,
    systemd_scope: bool,
    id: Uuid,
    paths: &RuntimePaths,
    tpm_dir: &Path,
) -> Result<(), DriverError> {
    if !terminate(paths).await {
        return Err(DriverError::StartFailed(format!(
            "swtpm: an earlier swtpm of VM {id} did not exit"
        )));
    }
    for f in [&paths.tpm_socket, &paths.tpm_pidfile] {
        remove_file(f)?;
    }
    let mut cmd = if systemd_scope {
        crate::qemu::clear_stale_scope(&format!("opl-tpm-{id}.scope")).await;
        let mut cmd = tokio::process::Command::new("systemd-run");
        cmd.args(scope_args(id)).arg("--").arg(&cfg.swtpm);
        cmd
    } else {
        tokio::process::Command::new(&cfg.swtpm)
    };
    cmd.args(swtpm_args(paths, tpm_dir))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    // --daemon: 소켓을 연 뒤 떼어 낸 자식이 남고 부모는 끝난다.
    let output = tokio::time::timeout(SWTPM_START_TIMEOUT, cmd.output())
        .await
        .map_err(|_| DriverError::Timeout("swtpm to start"))?
        .map_err(|e| DriverError::StartFailed(format!("swtpm: {}: {e}", cfg.swtpm.display())))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        terminate(paths).await;
        return Err(DriverError::StartFailed(format!("swtpm: {stderr}")));
    }
    let ready = || paths.tpm_socket.exists() && alive(paths).is_some();
    if !crate::qemu::wait_until(SWTPM_START_TIMEOUT, ready).await {
        terminate(paths).await;
        let log = std::fs::read_to_string(paths.dir.join(LOG_FILE)).unwrap_or_default();
        let last = log.lines().last().unwrap_or("no log").trim().to_string();
        return Err(DriverError::StartFailed(format!(
            "swtpm: the control socket did not appear in {}s ({last})",
            SWTPM_START_TIMEOUT.as_secs()
        )));
    }
    Ok(())
}

/// 이 VM의 swtpm이 살아 있으면 그 pid. pid 파일의 pid가 살아 있고 그 명령줄에 이 VM의 제어 소켓 경로가 있어야
/// 주인이다(pid 재사용이나 다른 프로세스를 죽이지 않기 위해서다).
pub(crate) fn alive(paths: &RuntimePaths) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(&paths.tpm_pidfile)
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let target = Pid::from_raw(i32::try_from(pid).ok()?)?;
    test_kill_process(target).ok()?;
    let cmdline = process_cmdline(pid)?;
    let socket = paths.tpm_socket.display().to_string().replace(',', ",,");
    cmdline.contains(&socket).then_some(pid)
}

/// 프로세스의 명령줄(인자를 공백으로 이은 것). Linux는 `/proc`, 다른 곳은 `ps`로 읽는다.
fn process_cmdline(pid: u32) -> Option<String> {
    if cfg!(target_os = "linux") {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(
            raw.split(|b| *b == 0)
                .map(String::from_utf8_lossy)
                .collect::<Vec<_>>()
                .join(" "),
        )
    } else {
        let out = std::process::Command::new("ps")
            .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// 이 VM의 swtpm이 살아 있으면(주인 확인) SIGKILL로 끝내고 끝나기를 기다린다. 없거나 끝났으면 true, 끝나지 않으면
/// false다.
pub(crate) async fn terminate(paths: &RuntimePaths) -> bool {
    let Some(pid) = alive(paths) else {
        return true;
    };
    tracing::warn!(pid, socket = ?paths.tpm_socket, "stopping a swtpm without its QEMU");
    if let Some(target) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
        match kill_process(target, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(e) => tracing::warn!(pid, error = %e, "failed to kill swtpm"),
        }
    }
    let gone = || alive(paths).is_none();
    let ok = crate::qemu::wait_until(KILL_TIMEOUT, gone).await;
    if !ok {
        tracing::error!(pid, "swtpm did not exit");
    }
    ok
}

/// 이 노드가 TPM 2.0 VM을 켤 수 있는지: `swtpm`·`swtpm_setup`이 있고, `swtpm socket --print-capabilities`에
/// `tpm-2.0`이 있고, `qemu -tpmdev help`에 `emulator`가 있다. 읽지 못하면 못 하는 것이다.
pub(crate) async fn tpm_capable(cfg: &SwtpmConfig, qemu_binary: &Path) -> bool {
    if !installed(&cfg.swtpm) || !installed(&cfg.swtpm_setup) {
        return false;
    }
    let caps = probe(&cfg.swtpm, &["socket", "--print-capabilities"]).await;
    let help = probe(qemu_binary, &["-tpmdev", "help"]).await;
    match (caps, help) {
        (Some(caps), Some(help)) => tpm_caps(&caps, &help),
        _ => false,
    }
}

/// `--print-capabilities`(JSON의 `features`)와 `-tpmdev help` 출력으로 본 TPM 2.0 능력
pub(crate) fn tpm_caps(swtpm_caps: &str, tpmdev_help: &str) -> bool {
    let tpm2 = serde_json::from_str::<serde_json::Value>(swtpm_caps).is_ok_and(|v| {
        v["features"]
            .as_array()
            .is_some_and(|f| f.iter().any(|x| x == "tpm-2.0"))
    });
    let emulator = tpmdev_help
        .lines()
        .any(|l| l.split_whitespace().next() == Some("emulator"));
    tpm2 && emulator
}

/// 실행 파일이 있는지. 이름만 있으면 `PATH`에서 찾는다.
fn installed(binary: &Path) -> bool {
    if binary.components().count() > 1 {
        return binary.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(binary).is_file()))
}

async fn probe(binary: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(PROBE_TIMEOUT, output).await {
        Ok(Ok(out)) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        other => {
            let error = match other {
                Ok(Ok(out)) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
                Ok(Err(e)) => e.to_string(),
                Err(_) => "timed out".into(),
            };
            tracing::info!(?binary, ?args, %error, "TPM probe failed; TPM is off on this node");
            None
        }
    }
}

fn remove_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swtpm_runs_detached_and_terminates_with_its_qemu() {
        let paths = RuntimePaths::new(Path::new("/run/opl"), Uuid::nil()).unwrap();
        let dir = "/run/opl/vm/00000000-0000-0000-0000-000000000000";
        let args = swtpm_args(&paths, Path::new("/var/lib/opl,x/vm/1/tpm"));
        assert_eq!(
            args,
            [
                "socket".to_string(),
                "--tpm2".into(),
                "--tpmstate".into(),
                "dir=/var/lib/opl,,x/vm/1/tpm,mode=0600".into(),
                "--ctrl".into(),
                format!("type=unixio,path={dir}/tpm.sock,mode=0600"),
                "--pid".into(),
                format!("file={dir}/swtpm.pid"),
                "--log".into(),
                format!("file={dir}/swtpm.log,level=1"),
                "--daemon".into(),
                "--terminate".into(),
            ]
        );
        assert_eq!(
            setup_args(Path::new("/s/tpm")),
            [
                "--tpm2",
                "--tpm-state",
                "/s/tpm",
                "--createek",
                "--create-ek-cert",
                "--create-platform-cert",
                "--lock-nvram",
                "--not-overwrite"
            ]
        );
        assert_eq!(
            scope_args(Uuid::nil())[3],
            format!("--unit=opl-tpm-{}", Uuid::nil())
        );
    }

    /// swtpm 0.7~0.10의 `--print-capabilities`와 QEMU 8.2·11.1의 `-tpmdev help` 모양
    #[test]
    fn tpm_caps_need_tpm2_and_the_emulator_backend() {
        let caps = r#"{ "type": "swtpm", "features": [ "tpm-1.2", "tpm-2.0", "tpm-send-command-header", "flags-opt-startup", "cmdarg-seccomp", "cmdarg-key-fd" ], "version": "0.9.0" }"#;
        let help =
            "Supported TPM types (choose only one):\n    emulator   TPM emulator backend driver\n";
        assert!(tpm_caps(caps, help));
        assert!(!tpm_caps(r#"{"features": ["tpm-1.2"]}"#, help));
        assert!(!tpm_caps(
            caps,
            "Supported TPM types (choose only one):\n passthrough  Passthrough TPM backend driver\n"
        ));
        assert!(!tpm_caps("not json", help));
    }

    #[test]
    fn firmware_epoch_is_zero_until_written() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        assert_eq!(read_firmware_epoch(tmp.path(), id).unwrap(), 0);
        write_firmware_epoch(tmp.path(), id, 3).unwrap();
        assert_eq!(read_firmware_epoch(tmp.path(), id).unwrap(), 3);
        std::fs::write(vm_state_dir(tmp.path(), id).join(EPOCH_FILE), "x").unwrap();
        assert!(read_firmware_epoch(tmp.path(), id).is_err());
        assert_eq!(
            tpm_state_path(tmp.path(), id),
            vm_state_dir(tmp.path(), id).join("tpm/tpm2-00.permall")
        );
    }

    /// 가짜 swtpm·swtpm_setup(`tests/fixtures`, python3)
    pub(crate) fn fake_swtpm() -> SwtpmConfig {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        SwtpmConfig {
            swtpm: dir.join("fake-swtpm"),
            swtpm_setup: dir.join("fake-swtpm-setup"),
        }
    }

    fn short_tempdir() -> tempfile::TempDir {
        // macOS의 기본 TMPDIR은 소켓 경로 한도를 넘기므로 /tmp를 쓴다.
        tempfile::Builder::new()
            .prefix("opl")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn fresh(epoch: u32) -> RuntimeTpm {
        RuntimeTpm { fresh: true, epoch }
    }

    fn kept(epoch: u32) -> RuntimeTpm {
        RuntimeTpm {
            fresh: false,
            epoch,
        }
    }

    /// `fresh`면 없는 상태를 만들고(0700 디렉터리) 있는 상태는 그대로 쓴다. `fresh`가 아닌데 없으면 몰래 만들지
    /// 않는다. 받는 쪽은 만들지 않는다.
    #[tokio::test]
    async fn prepare_creates_only_fresh_state() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let (state, cfg, id) = (tmp.path(), fake_swtpm(), Uuid::now_v7());
        let vars = tmp.path().join("vars.fd");
        let err = prepare_state(&cfg, state, id, kept(0), false, &vars)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::InvalidSpec(m) if m == &format!("the TPM state of VM {id} is missing on this node")),
            "{err}"
        );
        prepare_state(&cfg, state, id, fresh(0), false, &vars)
            .await
            .unwrap();
        let file = tpm_state_path(state, id);
        let first = std::fs::read(&file).unwrap();
        assert_eq!(first.len(), 64);
        let mode = std::fs::metadata(vm_tpm_dir(state, id))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // 다시 켜도 같은 TPM이다(fresh여도 덮어쓰지 않는다).
        prepare_state(&cfg, state, id, fresh(0), false, &vars)
            .await
            .unwrap();
        prepare_state(&cfg, state, id, kept(0), false, &vars)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), first);
        // 마이그레이션을 받는 쪽은 빈 디렉터리로 띄운다.
        let other = Uuid::now_v7();
        prepare_state(&cfg, state, other, kept(0), true, &vars)
            .await
            .unwrap();
        assert!(vm_tpm_dir(state, other).is_dir());
        assert!(!tpm_state_path(state, other).exists());
    }

    /// 회차가 다르면 UEFI 변수를 지우고, `fresh`면 TPM도 새로 만든다. `fresh`가 아니면(지금 것·받아 온 것) TPM은 둔다.
    #[tokio::test]
    async fn a_new_epoch_drops_stale_firmware_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, cfg, id) = (tmp.path(), fake_swtpm(), Uuid::now_v7());
        let vars = tmp.path().join("vars.fd");
        prepare_state(&cfg, state, id, fresh(0), false, &vars)
            .await
            .unwrap();
        let file = tpm_state_path(state, id);
        let first = std::fs::read(&file).unwrap();

        std::fs::write(&vars, b"old vars").unwrap();
        prepare_state(&cfg, state, id, kept(1), false, &vars)
            .await
            .unwrap();
        assert!(!vars.exists());
        assert_eq!(std::fs::read(&file).unwrap(), first);
        assert_eq!(read_firmware_epoch(state, id).unwrap(), 1);

        std::fs::write(&vars, b"old vars").unwrap();
        prepare_state(&cfg, state, id, fresh(2), false, &vars)
            .await
            .unwrap();
        assert!(!vars.exists());
        assert_ne!(std::fs::read(&file).unwrap(), first);
        assert_eq!(read_firmware_epoch(state, id).unwrap(), 2);
    }

    #[tokio::test]
    async fn a_failed_setup_is_a_start_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, cfg, id) = (tmp.path(), fake_swtpm(), Uuid::now_v7());
        std::fs::create_dir_all(vm_tpm_dir(state, id)).unwrap();
        std::fs::write(vm_tpm_dir(state, id).join("fail-setup"), b"").unwrap();
        let err = prepare_state(&cfg, state, id, fresh(0), false, Path::new("/nonexistent"))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::StartFailed(m) if m == "swtpm_setup: swtpm_setup: Could not create the EK"),
            "{err}"
        );
        let missing = SwtpmConfig {
            swtpm_setup: "/nonexistent/swtpm_setup".into(),
            ..fake_swtpm()
        };
        std::fs::remove_file(vm_tpm_dir(state, id).join("fail-setup")).unwrap();
        let err = prepare_state(&missing, state, id, fresh(0), false, Path::new("/x"))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::StartFailed(m) if m.starts_with("swtpm_setup: ")),
            "{err}"
        );
    }

    /// 띄우면 제어 소켓과 pid 파일이 생기고 주인이 확인된다. QEMU가 연결했다 끊으면(`--terminate`) 스스로 끝난다.
    #[tokio::test]
    async fn swtpm_starts_detached_and_ends_with_its_client() {
        let tmp = short_tempdir();
        let id = Uuid::now_v7();
        let paths = RuntimePaths::new(tmp.path(), id).unwrap();
        std::fs::create_dir_all(&paths.dir).unwrap();
        let dir = vm_tpm_dir(&tmp.path().join("state"), id);
        std::fs::create_dir_all(&dir).unwrap();
        start(&fake_swtpm(), false, id, &paths, &dir).await.unwrap();
        assert!(paths.tpm_socket.exists());
        let pid = alive(&paths).expect("swtpm runs");
        assert_ne!(pid, std::process::id());
        // 두 번째 swtpm은 남은 것을 끝내고 다시 띄운다.
        start(&fake_swtpm(), false, id, &paths, &dir).await.unwrap();
        let second = alive(&paths).unwrap();
        assert_ne!(second, pid);
        let qemu = std::os::unix::net::UnixStream::connect(&paths.tpm_socket).unwrap();
        drop(qemu);
        assert!(crate::qemu::wait_until(Duration::from_secs(10), || alive(&paths).is_none()).await);
    }

    /// pid 파일의 pid가 이 VM의 swtpm이 아니면(pid 재사용, 다른 프로세스) 주인이 아니라 죽이지 않는다.
    #[tokio::test]
    async fn only_the_owner_is_killed() {
        let tmp = short_tempdir();
        let id = Uuid::now_v7();
        let paths = RuntimePaths::new(tmp.path(), id).unwrap();
        std::fs::create_dir_all(&paths.dir).unwrap();
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        std::fs::write(&paths.tpm_pidfile, sleeper.id().to_string()).unwrap();
        assert_eq!(alive(&paths), None);
        assert!(terminate(&paths).await);
        assert!(
            sleeper.try_wait().unwrap().is_none(),
            "the sleeper was killed"
        );
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();

        let dir = vm_tpm_dir(&tmp.path().join("state"), id);
        std::fs::create_dir_all(&dir).unwrap();
        start(&fake_swtpm(), false, id, &paths, &dir).await.unwrap();
        assert!(alive(&paths).is_some());
        assert!(terminate(&paths).await);
        assert_eq!(alive(&paths), None);
    }

    #[tokio::test]
    async fn a_failing_or_missing_swtpm_is_a_start_failure() {
        let tmp = short_tempdir();
        let id = Uuid::now_v7();
        let paths = RuntimePaths::new(tmp.path(), id).unwrap();
        std::fs::create_dir_all(&paths.dir).unwrap();
        // 상태 디렉터리가 없으면 swtpm이 실패한다.
        let err = start(&fake_swtpm(), false, id, &paths, &tmp.path().join("none"))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::StartFailed(m) if m.starts_with("swtpm: swtpm: Could not open TPM state directory")),
            "{err}"
        );
        let missing = SwtpmConfig {
            swtpm: "/nonexistent/swtpm".into(),
            ..fake_swtpm()
        };
        let err = start(&missing, false, id, &paths, tmp.path())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::StartFailed(m) if m.starts_with("swtpm: /nonexistent/swtpm: ")),
            "{err}"
        );
    }

    /// 가짜 swtpm은 TPM 2.0을 알리고, QEMU의 `-tpmdev help`(이 Mac의 Homebrew QEMU, 없으면 가짜)에는 `emulator`가
    /// 있다. 실행 파일이 없으면 능력이 없다.
    #[tokio::test]
    async fn tpm_capability_needs_both_binaries_and_the_backend() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let qemu = tmp.path().join("fake-qemu");
        std::fs::write(
            &qemu,
            "#!/bin/sh\necho 'Supported TPM types (choose only one):'\necho '    emulator   TPM emulator backend driver'\n",
        )
        .unwrap();
        std::fs::set_permissions(&qemu, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(tpm_capable(&fake_swtpm(), &qemu).await);
        let no_setup = SwtpmConfig {
            swtpm_setup: "opl-no-such-swtpm_setup".into(),
            ..fake_swtpm()
        };
        assert!(!tpm_capable(&no_setup, &qemu).await);
        assert!(!tpm_capable(&fake_swtpm(), Path::new("/nonexistent/qemu")).await);
        // 기본 설정은 PATH의 swtpm을 찾는다(이 Mac에는 없다).
        if !installed(Path::new("swtpm")) {
            assert!(!tpm_capable(&SwtpmConfig::default(), &qemu).await);
        }
    }

    #[test]
    fn installed_looks_in_path_for_bare_names() {
        assert!(installed(Path::new("sh")));
        assert!(installed(Path::new("/bin/sh")));
        assert!(!installed(Path::new("opl-no-such-binary")));
        assert!(!installed(Path::new("/nonexistent/swtpm")));
    }
}
