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

/// 이 노드의 VM 펌웨어 파일 회차를 적는다(임시 파일에 쓰고 fsync한 뒤 rename, 디렉터리도 fsync — 리뷰 2 #5: 전원이
/// 나가도 빈 파일이 남지 않게). 다른 노드에서 받아 온 UEFI 변수·TPM 파일을 둔 뒤에도 VM의 `firmware_epoch`로 적어야
/// 켤 때 지워지지 않는다.
pub fn write_firmware_epoch(state_dir: &Path, id: Uuid, epoch: u32) -> std::io::Result<()> {
    use std::io::Write;
    let dir = vm_state_dir(state_dir, id);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{EPOCH_FILE}.tmp"));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(epoch.to_string().as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, dir.join(EPOCH_FILE))?;
    std::fs::File::open(&dir)?.sync_all()
}

/// 이 노드의 회차가 `epoch`와 다른지(이 노드의 펌웨어 파일이 초기화 전의 것인지). 회차 파일을 읽지 못하면(깨짐, 입출력
/// 오류) 모르는 것이라 다르지 않다고 본다(리뷰 2 #5): 경고만 남기고 아무것도 지우지 않는다 — 지금 파일일 수 있다.
pub fn firmware_epoch_differs(state_dir: &Path, id: Uuid, epoch: u32) -> bool {
    match read_firmware_epoch(state_dir, id) {
        Ok(on_node) => on_node != epoch,
        Err(e) => {
            tracing::warn!(%id, error = %e, "cannot read the firmware epoch on this node; keeping its firmware files");
            false
        }
    }
}

/// [`adopt_firmware_epoch`]가 남길 이 노드의 펌웨어 파일
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepFirmware {
    /// UEFI 변수(막 받았거나 되돌렸다)
    pub vars: bool,
    /// TPM 상태(막 받았거나 되돌렸거나, 지금 것이다 — 이 노드가 `tpm_node`이거나 해시가 VM의 `tpm_sha256`과 같다)
    pub tpm: bool,
}

/// 이 노드의 펌웨어 회차를 VM의 `epoch`로 옮기는 단 하나의 규칙(리뷰 1 C2, 리뷰 2 #1): 이 노드의 회차가 다르면 이
/// 노드의 파일은 초기화 전의 것이므로 `keep`에 없는 파일(받거나 되돌리지 않은 변수, 지금 것이 아닌 TPM 상태)을 지운
/// 뒤 회차를 적는다. 회차만 적으면 낡은 파일이 지금 것으로 보여 켤 때 지워지지 않는다(초기화가 되살아난다). 회차 파일을
/// 읽지 못하면 아무것도 지우지 않고 회차만 적는다([`firmware_epoch_differs`]). 받기·사본 받기·초기화·되돌리기·켜기가 모두
/// 이것을 부른다.
pub fn adopt_firmware_epoch(
    state_dir: &Path,
    id: Uuid,
    epoch: u32,
    keep: KeepFirmware,
) -> std::io::Result<()> {
    match read_firmware_epoch(state_dir, id) {
        Ok(on_node) if on_node == epoch => return Ok(()),
        Ok(_) => {
            if !keep.vars {
                remove_file(&crate::qemu::uefi_vars_path(state_dir, id))?;
            }
            if !keep.tpm {
                match std::fs::remove_dir_all(vm_tpm_dir(state_dir, id)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
        }
        Err(e) => {
            tracing::warn!(%id, error = %e, "cannot read the firmware epoch on this node; keeping its firmware files");
        }
    }
    write_firmware_epoch(state_dir, id, epoch)
}

/// TPM이 없는 UEFI VM을 켜기 전에(리뷰 1 I1): 이 노드의 회차가 VM의 `epoch`와 다르면 이 노드의 UEFI 변수는 초기화
/// 전의 것이므로 지우고, TPM 상태도 지금 것이 아니면(`keep_tpm` 거짓) 지운 뒤 회차를 적는다(리뷰 2 #1b — 남기면 TPM을
/// 다시 켤 때 낡은 TPM이 새 회차의 것으로 보인다). 회차 파일이 없고 VM의 회차가 0이면(초기화한 적 없음) 아무것도 쓰지
/// 않는다.
pub(crate) fn prepare_vars(
    state_dir: &Path,
    id: Uuid,
    epoch: u32,
    keep_tpm: bool,
) -> Result<(), DriverError> {
    let keep = KeepFirmware {
        vars: false,
        tpm: keep_tpm,
    };
    adopt_firmware_epoch(state_dir, id, epoch, keep).map_err(Into::into)
}

/// 켜기 전에 TPM 상태를 맞춘다.
///
/// 1. `tpm.epoch`가 이 노드의 회차와 다르면 이 노드의 파일은 초기화 전의 것이다. UEFI 변수를 지우고, TPM 상태는 지금
///    것이 아니면(`keep_tpm` 거짓) 지운 뒤 회차를 적는다([`adopt_firmware_epoch`]). 막 받아 온 TPM은 받을 때 회차를
///    이미 옮겼다.
/// 2. TPM 디렉터리를 0700으로 만든다.
/// 3. 마이그레이션을 받는 쪽(`incoming`)이 아니고 상태 파일이 없으면, `fresh`면 `swtpm_setup`으로 새로 만들고(EK와
///    인증서, 있는 파일은 덮어쓰지 않음) 아니면 몰래 새로 만들지 않고 실패한다.
pub(crate) async fn prepare_state(
    cfg: &SwtpmConfig,
    state_dir: &Path,
    id: Uuid,
    tpm: RuntimeTpm,
    keep_tpm: bool,
    incoming: bool,
) -> Result<(), DriverError> {
    use std::os::unix::fs::PermissionsExt;

    let dir = vm_tpm_dir(state_dir, id);
    let keep = KeepFirmware {
        vars: false,
        tpm: keep_tpm,
    };
    adopt_firmware_epoch(state_dir, id, tpm.epoch, keep)?;
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
    cpus: Option<&opl_types::numa::CpuSet>,
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
    // 계획 64: opld의 친화(예약 CPU)를 물려받지 않게 QEMU의 에뮬레이터 집합에 둔다.
    if let Some(cpus) = cpus {
        crate::qemu::pin_child(&mut cmd, cpus);
    }
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

/// 이 노드가 TPM 2.0 VM을 켤 수 있는지([`tpm_check`]). 아니면 그 이유를 경고로 남긴다(opld 로그에서 볼 수 있게).
pub(crate) async fn tpm_capable(
    cfg: &SwtpmConfig,
    qemu_binary: &Path,
    state_dir: &Path,
    run_dir: &Path,
) -> bool {
    match tpm_check(cfg, qemu_binary, state_dir, run_dir).await {
        Ok(()) => true,
        Err(why) => {
            tracing::warn!(reason = %why, "TPM is off on this node: {why}");
            false
        }
    }
}

/// 이 노드가 TPM 2.0 VM을 켤 수 있는지: `swtpm`·`swtpm_setup`이 있고, `swtpm socket --print-capabilities`에
/// `tpm-2.0`이 있고, `qemu -tpmdev help`에 `emulator`가 있고, 실제 상태·실행 디렉터리(`state_dir`·`run_dir`의
/// `vm/{id}`)에서 swtpm을 한 번 띄워 볼 수 있다([`try_swtpm`]). 마지막 것은 AppArmor(Ubuntu의 swtpm 프로필) 같은 막힘을
/// VM 켜기 실패가 아니라 능력 없음으로 알리려는 것이다(리뷰 1 I6). 못 하면 그 이유다.
///
/// `-tpmdev help`는 끝난 상태를 보지 않는다: QEMU 8.2(Ubuntu 24.04)는 목록을 찍은 뒤 1로 끝나고(`tpm_config_parse`가
/// -1을 돌려 `exit(1)`) 11.1은 0으로 끝난다. 출력은 표준 오류다(GitHub 탐침 2).
pub(crate) async fn tpm_check(
    cfg: &SwtpmConfig,
    qemu_binary: &Path,
    state_dir: &Path,
    run_dir: &Path,
) -> Result<(), String> {
    for binary in [&cfg.swtpm, &cfg.swtpm_setup] {
        if !installed(binary) {
            return Err(format!("{} is not installed", binary.display()));
        }
    }
    let (caps, _) = probe(&cfg.swtpm, &["socket", "--print-capabilities"], false).await?;
    if !tpm2_listed(&caps) {
        return Err(format!(
            "`{} socket --print-capabilities` does not list tpm-2.0: {}",
            cfg.swtpm.display(),
            caps.trim()
        ));
    }
    let (out, err) = probe(qemu_binary, &["-tpmdev", "help"], true).await?;
    let help = format!("{out}{err}");
    if !emulator_listed(&help) {
        return Err(format!(
            "`{} -tpmdev help` does not list the emulator backend: {}",
            qemu_binary.display(),
            help.trim()
        ));
    }
    try_swtpm(cfg, state_dir, run_dir).await.map_err(|e| {
        format!(
            "swtpm cannot run with this node's state and run directories ({e}); on Ubuntu/Debian check the \
             AppArmor include /etc/apparmor.d/abstractions/orbivisor-swtpm"
        )
    })
}

/// 버릴 VM id의 상태·실행 디렉터리(`{state}/vm/{id}/tpm`, `{run}/vm/{id}`)에서 VM과 같은 인자로 swtpm을 띄워 제어
/// 소켓과 pid 파일이 생기는지 본다([`PROBE_TIMEOUT`]까지). 띄운 swtpm은 끝내고 두 디렉터리를 지운다.
async fn try_swtpm(cfg: &SwtpmConfig, state_dir: &Path, run_dir: &Path) -> Result<(), String> {
    let id = Uuid::now_v7();
    let paths = RuntimePaths::new(run_dir, id).map_err(|e| e.to_string())?;
    let state = vm_state_dir(state_dir, id);
    let result = run_swtpm(cfg, &state.join("tpm"), &paths).await;
    for dir in [&state, &paths.dir] {
        if let Err(e) = std::fs::remove_dir_all(dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot remove the swtpm probe directory");
        }
    }
    result
}

async fn run_swtpm(cfg: &SwtpmConfig, tpm_dir: &Path, paths: &RuntimePaths) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(tpm_dir)
        .map_err(|e| format!("cannot create {}: {e}", tpm_dir.display()))?;
    std::fs::set_permissions(tpm_dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&paths.dir)
        .map_err(|e| format!("cannot create {}: {e}", paths.dir.display()))?;
    let mut args = swtpm_args(paths, tpm_dir);
    // 떼어 내지 않고(`--daemon` 빼기) 자식으로 두어 확인한 뒤 제어 소켓에 붙었다 끊어 끝낸다(`--terminate`). opld가 그
    // 사이 죽어도(감시견 SIGKILL) 남지 않게 Linux에서는 부모가 죽으면 SIGKILL을 받는다(리뷰 2 #9).
    args.retain(|a| a != "--daemon");
    args.extend(["--flags".into(), "not-need-init,startup-clear".into()]);
    let mut cmd = tokio::process::Command::new(&cfg.swtpm);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    die_with_parent(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("swtpm: {e}"))?;
    let deadline = tokio::time::Instant::now() + PROBE_TIMEOUT;
    let outcome = loop {
        if paths.tpm_socket.exists() && paths.tpm_pidfile.exists() {
            break Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            let mut msg = String::new();
            if let Some(mut err) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = err.read_to_string(&mut msg).await;
            }
            if let Ok(log) = std::fs::read_to_string(paths.dir.join(LOG_FILE)) {
                msg.push_str(&log);
            }
            break Err(format!("swtpm exited ({status}): {}", msg.trim()));
        }
        if tokio::time::Instant::now() >= deadline {
            break Err("swtpm did not create its control socket in time".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    if outcome.is_ok() {
        // QEMU처럼 붙었다 끊는다: `--terminate`로 스스로 끝난다.
        drop(tokio::net::UnixStream::connect(&paths.tpm_socket).await);
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            tracing::debug!("the probe swtpm did not end on disconnect; killing it");
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(KILL_TIMEOUT, child.wait()).await;
    outcome
}

/// 띄운 프로세스가 부모(opld)가 죽으면 SIGKILL을 받게 한다(Linux `PR_SET_PDEATHSIG`). 다른 곳에서는 아무것도 하지 않는다.
#[cfg(target_os = "linux")]
fn die_with_parent(cmd: &mut tokio::process::Command) {
    // SAFETY: fork 뒤 exec 전에 비동기 신호 안전한 prctl 하나만 부른다.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::set_parent_process_death_signal(Some(Signal::KILL))
                .map_err(std::io::Error::from)
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn die_with_parent(_cmd: &mut tokio::process::Command) {}

/// `--print-capabilities`(JSON의 `features`)와 `-tpmdev help` 출력으로 본 TPM 2.0 능력
#[cfg(test)]
pub(crate) fn tpm_caps(swtpm_caps: &str, tpmdev_help: &str) -> bool {
    tpm2_listed(swtpm_caps) && emulator_listed(tpmdev_help)
}

/// `--print-capabilities`의 JSON `features`에 `tpm-2.0`이 있는지
fn tpm2_listed(swtpm_caps: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(swtpm_caps).is_ok_and(|v| {
        v["features"]
            .as_array()
            .is_some_and(|f| f.iter().any(|x| x == "tpm-2.0"))
    })
}

/// `-tpmdev help` 출력에 `emulator` 줄이 있는지(앞의 공백은 버전마다 다르다)
fn emulator_listed(tpmdev_help: &str) -> bool {
    tpmdev_help
        .lines()
        .any(|l| l.split_whitespace().next() == Some("emulator"))
}

/// 실행 파일이 있는지. 이름만 있으면 `PATH`에서 찾는다.
fn installed(binary: &Path) -> bool {
    if binary.components().count() > 1 {
        return binary.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(binary).is_file()))
}

/// 명령을 돌려 (표준 출력, 표준 오류)를 돌려준다. `any_status`가 아니면 0이 아닌 끝난 상태도 실패다. 띄우지 못했거나
/// 시간이 넘었으면 그 이유다.
async fn probe(binary: &Path, args: &[&str], any_status: bool) -> Result<(String, String), String> {
    let output = tokio::process::Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let shown = || format!("`{} {}`", binary.display(), args.join(" "));
    let out = match tokio::time::timeout(PROBE_TIMEOUT, output).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("{}: {e}", shown())),
        Err(_) => return Err(format!("{} timed out", shown())),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !any_status && !out.status.success() {
        return Err(format!(
            "{} failed ({}): {}",
            shown(),
            out.status,
            stderr.trim()
        ));
    }
    Ok((stdout, stderr))
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

    /// 받은 뒤 회차를 옮기면 받지 않은 낡은 파일을 지운다(리뷰 1 C2). 회차가 같으면 아무것도 지우지 않는다.
    #[test]
    fn adopting_a_new_epoch_removes_files_not_received() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let vars = crate::qemu::uefi_vars_path(tmp.path(), id);
        let permall = tpm_state_path(tmp.path(), id);
        let put = || {
            std::fs::create_dir_all(permall.parent().unwrap()).unwrap();
            std::fs::write(&vars, b"vars").unwrap();
            std::fs::write(&permall, b"tpm").unwrap();
        };
        // 변수만 받았다(TPM 초기화 뒤 새로 켤 차례): 낡은 TPM을 지운다.
        put();
        let only_vars = KeepFirmware {
            vars: true,
            tpm: false,
        };
        adopt_firmware_epoch(tmp.path(), id, 1, only_vars).unwrap();
        assert!(vars.exists());
        assert!(!permall.exists());
        assert_eq!(read_firmware_epoch(tmp.path(), id).unwrap(), 1);
        // 같은 회차면 지우지 않는다.
        put();
        adopt_firmware_epoch(tmp.path(), id, 1, only_vars).unwrap();
        assert!(permall.exists());
        // TPM만 받았다: 낡은 변수를 지운다.
        let only_tpm = KeepFirmware {
            vars: false,
            tpm: true,
        };
        adopt_firmware_epoch(tmp.path(), id, 2, only_tpm).unwrap();
        assert!(!vars.exists());
        assert!(permall.exists());
    }

    /// TPM이 없는 UEFI VM도 회차가 다르면 변수를 지운다(리뷰 1 I1). TPM 상태도 지금 것이 아니면 지운다(리뷰 2 #1b).
    #[test]
    fn vars_of_an_older_epoch_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let vars = crate::qemu::uefi_vars_path(tmp.path(), id);
        let permall = tpm_state_path(tmp.path(), id);
        std::fs::create_dir_all(permall.parent().unwrap()).unwrap();
        std::fs::write(&vars, b"vars").unwrap();
        std::fs::write(&permall, b"tpm").unwrap();
        prepare_vars(tmp.path(), id, 0, false).unwrap();
        assert!(vars.exists() && permall.exists());
        // 회차 파일이 없고 VM의 회차가 0이면(업그레이드 뒤 첫 켜기) 아무것도 쓰지 않는다.
        assert!(!vm_state_dir(tmp.path(), id).join(EPOCH_FILE).exists());
        // 지금 것인 TPM(tpm_node, 해시 같음)은 남긴다.
        prepare_vars(tmp.path(), id, 1, true).unwrap();
        assert!(!vars.exists());
        assert!(permall.exists());
        assert_eq!(read_firmware_epoch(tmp.path(), id).unwrap(), 1);
        std::fs::write(&vars, b"vars").unwrap();
        prepare_vars(tmp.path(), id, 1, false).unwrap();
        assert!(vars.exists() && permall.exists());
        // 초기화 전의 TPM(지금 것이 아님)은 지운다: 다시 TPM을 켜면 새 회차의 것으로 보이지 않게.
        prepare_vars(tmp.path(), id, 2, false).unwrap();
        assert!(!vars.exists());
        assert!(!permall.exists());
    }

    /// 업그레이드: 계획 59 전 UEFI VM(회차 파일 없음, VM 회차 0)은 첫 켜기에 변수를 지키고 회차 파일을 쓰지 않는다.
    #[test]
    fn upgraded_uefi_vms_keep_their_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let vars = crate::qemu::uefi_vars_path(tmp.path(), id);
        std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
        std::fs::write(&vars, b"boot entries").unwrap();
        prepare_vars(tmp.path(), id, 0, false).unwrap();
        assert_eq!(std::fs::read(&vars).unwrap(), b"boot entries");
        let keep = KeepFirmware {
            vars: true,
            tpm: false,
        };
        adopt_firmware_epoch(tmp.path(), id, 0, keep).unwrap();
        prepare_vars(tmp.path(), id, 0, false).unwrap();
        assert_eq!(std::fs::read(&vars).unwrap(), b"boot entries");
    }

    /// 회차 파일이 깨졌으면(리뷰 2 #5) 모르는 것이라 아무것도 지우지 않고 회차를 다시 적는다.
    #[test]
    fn an_unreadable_epoch_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let vars = crate::qemu::uefi_vars_path(tmp.path(), id);
        let permall = tpm_state_path(tmp.path(), id);
        std::fs::create_dir_all(permall.parent().unwrap()).unwrap();
        std::fs::write(&vars, b"vars").unwrap();
        std::fs::write(&permall, b"tpm").unwrap();
        let epoch = vm_state_dir(tmp.path(), id).join(EPOCH_FILE);
        std::fs::write(&epoch, b"").unwrap();
        assert!(!firmware_epoch_differs(tmp.path(), id, 3));
        prepare_vars(tmp.path(), id, 3, false).unwrap();
        assert!(vars.exists() && permall.exists());
        assert_eq!(read_firmware_epoch(tmp.path(), id).unwrap(), 3);
        std::fs::write(&epoch, b"garbage").unwrap();
        let none = KeepFirmware {
            vars: false,
            tpm: false,
        };
        adopt_firmware_epoch(tmp.path(), id, 4, none).unwrap();
        assert!(vars.exists() && permall.exists());
        assert_eq!(read_firmware_epoch(tmp.path(), id).unwrap(), 4);
        assert!(firmware_epoch_differs(tmp.path(), id, 5));
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
        let err = prepare_state(&cfg, state, id, kept(0), false, false)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::InvalidSpec(m) if m == &format!("the TPM state of VM {id} is missing on this node")),
            "{err}"
        );
        prepare_state(&cfg, state, id, fresh(0), false, false)
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
        prepare_state(&cfg, state, id, fresh(0), false, false)
            .await
            .unwrap();
        prepare_state(&cfg, state, id, kept(0), false, false)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), first);
        // 마이그레이션을 받는 쪽은 빈 디렉터리로 띄운다.
        let other = Uuid::now_v7();
        prepare_state(&cfg, state, other, kept(0), false, true)
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
        let vars = crate::qemu::uefi_vars_path(state, id);
        prepare_state(&cfg, state, id, fresh(0), false, false)
            .await
            .unwrap();
        let file = tpm_state_path(state, id);
        let first = std::fs::read(&file).unwrap();

        std::fs::write(&vars, b"old vars").unwrap();
        prepare_state(&cfg, state, id, kept(1), true, false)
            .await
            .unwrap();
        assert!(!vars.exists());
        assert_eq!(std::fs::read(&file).unwrap(), first);
        assert_eq!(read_firmware_epoch(state, id).unwrap(), 1);

        std::fs::write(&vars, b"old vars").unwrap();
        prepare_state(&cfg, state, id, fresh(2), false, false)
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
        let err = prepare_state(&cfg, state, id, fresh(0), false, false)
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
        let err = prepare_state(&missing, state, id, fresh(0), false, false)
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
        start(&fake_swtpm(), false, id, &paths, &dir, None)
            .await
            .unwrap();
        assert!(paths.tpm_socket.exists());
        let pid = alive(&paths).expect("swtpm runs");
        assert_ne!(pid, std::process::id());
        // 두 번째 swtpm은 남은 것을 끝내고 다시 띄운다.
        start(&fake_swtpm(), false, id, &paths, &dir, None)
            .await
            .unwrap();
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
        start(&fake_swtpm(), false, id, &paths, &dir, None)
            .await
            .unwrap();
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
        let err = start(
            &fake_swtpm(),
            false,
            id,
            &paths,
            &tmp.path().join("none"),
            None,
        )
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
        let err = start(&missing, false, id, &paths, tmp.path(), None)
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
            "#!/bin/sh\necho 'Supported TPM types (choose only one):' >&2\necho '    emulator   TPM emulator backend driver' >&2\n",
        )
        .unwrap();
        std::fs::set_permissions(&qemu, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dirs = short_tempdir();
        let (state, run) = (dirs.path().join("state"), dirs.path().join("run"));
        let capable = |cfg: SwtpmConfig, qemu: PathBuf| {
            let (state, run) = (state.clone(), run.clone());
            async move { tpm_capable(&cfg, &qemu, &state, &run).await }
        };
        assert!(capable(fake_swtpm(), qemu.clone()).await);
        // 띄워 본 swtpm은 끝내고 디렉터리를 지운다.
        assert_eq!(std::fs::read_dir(state.join("vm")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(run.join("vm")).unwrap().count(), 0);
        let no_setup = SwtpmConfig {
            swtpm_setup: "opl-no-such-swtpm_setup".into(),
            ..fake_swtpm()
        };
        assert!(!capable(no_setup, qemu.clone()).await);
        assert!(!capable(fake_swtpm(), "/nonexistent/qemu".into()).await);
        // 상태 디렉터리에서 swtpm이 막히면(AppArmor 거절 흉내) 능력이 없다.
        std::fs::write(state.join("fail-swtpm"), b"").unwrap();
        assert!(!capable(fake_swtpm(), qemu.clone()).await);
        assert_eq!(std::fs::read_dir(state.join("vm")).unwrap().count(), 0);
        // 기본 설정은 PATH의 swtpm을 찾는다(이 Mac에는 없다).
        if !installed(Path::new("swtpm")) {
            assert!(!capable(SwtpmConfig::default(), qemu).await);
        }
        // 실제 QEMU(있으면)의 `-tpmdev help`(표준 오류)에서도 emulator를 찾는다.
        if installed(Path::new("qemu-system-x86_64")) {
            std::fs::remove_file(state.join("fail-swtpm")).unwrap();
            assert!(capable(fake_swtpm(), "qemu-system-x86_64".into()).await);
        }
    }

    /// GitHub 탐침 2: Ubuntu 24.04의 QEMU 8.2는 `-tpmdev help`를 표준 오류로 찍고 1로 끝난다(11.1은 0). 끝난 상태와
    /// 상관없이 출력을 읽는다. 능력이 없으면 이유를 돌려준다.
    #[tokio::test]
    async fn tpm_check_reads_the_ubuntu_qemu_help_and_explains_failures() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let ubuntu = script(
            "qemu-8.2",
            "printf 'Supported TPM types (choose only one):\\n passthrough   Passthrough TPM backend driver\\n    emulator   Emulator TPM backend driver\\n' >&2\nexit 1\n",
        );
        let stdout_zero = script(
            "qemu-stdout",
            "printf 'Supported TPM types (choose only one):\\n    emulator   TPM emulator backend driver\\n'\n",
        );
        let none = script(
            "qemu-none",
            "printf 'Supported TPM types (choose only one):\\n passthrough   Passthrough TPM backend driver\\n' >&2\nexit 1\n",
        );
        let dirs = short_tempdir();
        let (state, run) = (dirs.path().join("state"), dirs.path().join("run"));
        let check = |cfg: SwtpmConfig, qemu: PathBuf| {
            let (state, run) = (state.clone(), run.clone());
            async move { tpm_check(&cfg, &qemu, &state, &run).await }
        };
        check(fake_swtpm(), ubuntu).await.unwrap();
        check(fake_swtpm(), stdout_zero).await.unwrap();
        let why = check(fake_swtpm(), none).await.unwrap_err();
        assert!(
            why.contains("-tpmdev help` does not list the emulator backend")
                && why.contains("passthrough"),
            "{why}"
        );
        let missing = SwtpmConfig {
            swtpm: "/nonexistent/swtpm".into(),
            ..fake_swtpm()
        };
        let why = check(missing, "qemu".into()).await.unwrap_err();
        assert_eq!(why, "/nonexistent/swtpm is not installed");
        let why = check(fake_swtpm(), tmp.path().join("no-qemu"))
            .await
            .unwrap_err();
        assert!(
            why.contains("is not installed") || why.contains("-tpmdev help"),
            "{why}"
        );
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("fail-swtpm"), b"").unwrap();
        let why = check(fake_swtpm(), tmp.path().join("qemu-8.2"))
            .await
            .unwrap_err();
        assert!(
            why.starts_with("swtpm cannot run with this node's state"),
            "{why}"
        );
    }

    #[test]
    fn installed_looks_in_path_for_bare_names() {
        assert!(installed(Path::new("sh")));
        assert!(installed(Path::new("/bin/sh")));
        assert!(!installed(Path::new("opl-no-such-binary")));
        assert!(!installed(Path::new("/nonexistent/swtpm")));
    }
}
