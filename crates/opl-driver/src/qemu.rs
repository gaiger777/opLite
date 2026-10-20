use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::{HostProfile, QmpClient, QmpError, QmpEvent, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::Firmware;
use rustix::fs::{FlockOperation, fcntl_lock};
use rustix::process::{Pid, Signal, kill_process};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
/// QMP 명령 하나의 응답을 기다리는 한도. 멈춘(SIGSTOP 등) QEMU에서 호출자가 묶이지 않게 한다.
const QMP_TIMEOUT: Duration = Duration::from_secs(5);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// SIGKILL을 보낸 뒤 프로세스가 사라지기를 기다리는 한도
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// QMP 연결이 끊긴 뒤 프로세스가 끝나기를 기다리는 한도. QEMU는 종료 중에 소켓을 먼저 닫는다.
const EXIT_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct QemuConfig {
    pub host: HostProfile,
    /// 소켓과 pidfile을 두는 휘발성 디렉터리 (Linux: `/run/opl`)
    pub run_dir: PathBuf,
    /// UEFI 변수처럼 재부팅 후에도 남아야 하는 VM별 파일 (Linux: `/var/lib/opl`)
    pub state_dir: PathBuf,
}

/// QEMU 프로세스를 `-daemonize`로 띄워 opld와 분리한다. opld가 재시작해도 VM은 계속 돈다.
///
/// QEMU의 생존 여부는 QMP 응답이 아니라 pidfile의 fcntl 잠금으로 판정한다([`qemu_alive`]).
/// 잠금이 걸린 런타임 디렉터리는 절대 지우지 않는다.
pub struct QemuHypervisor {
    config: QemuConfig,
    vms: Arc<Mutex<HashMap<Uuid, Arc<VmHandle>>>>,
    events: broadcast::Sender<HypervisorEvent>,
}

struct VmHandle {
    qmp: QmpClient,
    pid: u32,
    paths: RuntimePaths,
    monitor: JoinHandle<()>,
}

impl Drop for VmHandle {
    fn drop(&mut self) {
        // 드라이버가 내려갈 때 모니터가 "연결 끊김 = 크래시"로 오판해 런타임 파일을 지우지 않게 한다.
        self.monitor.abort();
    }
}

impl QemuHypervisor {
    /// 런타임 디렉터리를 훑어 살아 있는 VM에 다시 연결하고, 죽은 VM의 흔적은 지운다.
    pub async fn new(config: QemuConfig) -> Result<Self, DriverError> {
        let vm_run_dir = config.run_dir.join("vm");
        std::fs::create_dir_all(&vm_run_dir)?;
        std::fs::create_dir_all(config.state_dir.join("vm"))?;
        let hv = Self {
            config,
            vms: Arc::new(Mutex::new(HashMap::new())),
            events: broadcast::channel(64).0,
        };
        for entry in std::fs::read_dir(&vm_run_dir)? {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let paths = hv.paths(id)?;
            if !qemu_alive(&paths.pidfile) {
                tracing::warn!(%id, "removing stale runtime dir");
                if let Err(e) = std::fs::remove_dir_all(&paths.dir) {
                    tracing::error!(%id, error = %e, "failed to remove stale runtime dir");
                }
                continue;
            }
            // 살아 있는 VM이므로 붙지 못해도 아무것도 지우지 않는다. 이후 start()가 다시 시도한다.
            match tokio::time::timeout(ATTACH_TIMEOUT, hv.attach(id, paths, false)).await {
                Ok(Ok(())) => tracing::info!(%id, "reattached to running VM"),
                Ok(Err(e)) => tracing::error!(%id, error = %e, "could not reattach to running VM"),
                Err(_) => tracing::error!(%id, "timed out reattaching to running VM"),
            }
        }
        Ok(hv)
    }

    fn paths(&self, id: Uuid) -> Result<RuntimePaths, DriverError> {
        let mut paths = RuntimePaths::new(&self.config.run_dir, id)
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        paths.uefi_vars = self.state_dir(id).join("uefi-vars.fd");
        Ok(paths)
    }

    fn state_dir(&self, id: Uuid) -> PathBuf {
        self.config.state_dir.join("vm").join(id.to_string())
    }

    fn handle(&self, id: Uuid) -> Option<Arc<VmHandle>> {
        self.vms.lock().unwrap().get(&id).cloned()
    }

    /// QMP에 연결하고 모니터 태스크를 붙여 등록한다. `launched`면 `Started`를 알린다.
    async fn attach(
        &self,
        id: Uuid,
        paths: RuntimePaths,
        launched: bool,
    ) -> Result<(), DriverError> {
        let qmp = QmpClient::connect(&paths.qmp_socket).await?;
        let pid = read_pid(&paths.pidfile)?;
        let rx = qmp.subscribe();
        // 잠금을 쥔 채로 모니터를 띄우고 등록한다. 모니터가 곧바로 연결 끊김을 보더라도
        // 등록이 끝난 뒤에야 해제할 수 있으므로 죽은 핸들이 남지 않는다.
        // 같은 이유로 Started도 이 인스턴스의 Stopped/Crashed보다 항상 먼저 나간다.
        let mut vms = self.vms.lock().unwrap();
        let monitor = tokio::spawn(monitor(
            id,
            rx,
            Arc::downgrade(&self.vms),
            self.events.clone(),
            paths.clone(),
        ));
        vms.insert(
            id,
            Arc::new(VmHandle {
                qmp,
                pid,
                paths,
                monitor,
            }),
        );
        if launched {
            let _ = self.events.send(HypervisorEvent {
                id,
                kind: EventKind::Started,
            });
        }
        Ok(())
    }

    async fn launch(&self, vm: &VmRuntimeSpec, paths: &RuntimePaths) -> Result<(), DriverError> {
        std::fs::create_dir_all(&paths.dir)?;
        std::fs::create_dir_all(self.state_dir(vm.id))?;
        if vm.firmware == Firmware::Uefi && !paths.uefi_vars.exists() {
            std::fs::copy(&self.config.host.uefi_vars_template, &paths.uefi_vars)?;
        }
        // 호출자가 pidfile 잠금이 풀린 것(= QEMU 없음)을 확인했으므로 남은 소켓은 지워도 된다.
        for sock in [&paths.qmp_socket, &paths.vnc_socket] {
            remove_if_exists(sock)?;
        }

        let mut args = build_cmdline(vm, &self.config.host, paths)
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        args.push("-daemonize".into());
        let mut cmd = tokio::process::Command::new(&self.config.host.qemu_binary);
        cmd.args(&args)
            .stdin(Stdio::null())
            // 타임아웃으로 기다림을 그만두면 부모 프로세스도 끝낸다. 이미 분리된 자식은
            // 실패 정리(cleanup_failed_launch)가 pidfile로 찾아 끝낸다.
            .kill_on_drop(true);
        if cfg!(target_os = "macos") {
            // macOS에서는 -daemonize의 fork 뒤 자식이 Objective-C 런타임을 초기화하다
            // "fork() was called ... Crashing instead"로 죽는다. 디스플레이/VNC 장치가 있으면
            // 우연히 피하지만, 이 변수를 켜야 장치 구성과 무관하게 안정적으로 뜬다.
            cmd.env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES");
        }
        // -daemonize: 초기화가 끝나면 부모 프로세스가 종료 코드로 성공 여부를 알려 주고, 자식은 세션에서 분리된다.
        let output = tokio::time::timeout(START_TIMEOUT, cmd.output())
            .await
            .map_err(|_| DriverError::Timeout("QEMU to start"))??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DriverError::StartFailed(stderr));
        }
        tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths.clone(), true))
            .await
            .map_err(|_| DriverError::Timeout("QMP of new QEMU"))??;
        let Some(handle) = self.handle(vm.id) else {
            // 그새 끝났다. 모니터가 종료 이벤트를 보냈다.
            return Ok(());
        };
        let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
        if let Err(e) = flatten(cont, "QMP cont") {
            let _ = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.quit()).await;
            return Err(e);
        }
        Ok(())
    }

    /// 실패한 기동의 흔적을 지운다. `-daemonize`로 분리된 QEMU가 남아 있으면 먼저 끝낸다.
    /// 끝내지 못하면 살아 있는 VM의 파일이므로 아무것도 지우지 않는다.
    async fn cleanup_failed_launch(&self, id: Uuid, paths: &RuntimePaths) {
        if qemu_alive(&paths.pidfile) {
            match read_pid(&paths.pidfile) {
                Ok(pid) => kill_qemu(pid),
                Err(e) => {
                    tracing::error!(%id, error = %e, "orphan QEMU has no readable pid; keeping runtime dir");
                    return;
                }
            }
            if !wait_until(KILL_TIMEOUT, || !qemu_alive(&paths.pidfile)).await {
                tracing::error!(%id, "orphan QEMU did not exit; keeping runtime dir");
                return;
            }
        }
        if let Err(e) = std::fs::remove_dir_all(&paths.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%id, error = %e, "failed to remove runtime dir");
        }
    }

    /// opld가 `-daemonize`와 `cont` 사이에 죽으면 VM이 `-S`(prelaunch) 상태로 남는다. 이어서 실행한다.
    async fn resume_if_prelaunch(&self, id: Uuid) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(());
        };
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        if flatten(status, "QMP query-status")? == RunState::Prelaunch {
            tracing::info!(%id, "continuing VM left in prelaunch");
            let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
            flatten(cont, "QMP cont")?;
        }
        Ok(())
    }

    async fn query_state(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(None);
        };
        let status = match tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await {
            Err(_) => return Err(DriverError::Timeout("QMP query-status")),
            Ok(Ok(RunState::Running)) => RuntimeStatus::Running,
            Ok(Ok(RunState::Prelaunch | RunState::Paused)) => RuntimeStatus::Paused,
            Ok(Ok(RunState::Shutdown)) => RuntimeStatus::Other("shutdown".into()),
            Ok(Ok(RunState::Other(s))) => RuntimeStatus::Other(s),
            // 조회하는 사이에 프로세스가 끝났다.
            Ok(Err(QmpError::Disconnected)) => return Ok(None),
            Ok(Err(e)) => return Err(e.into()),
        };
        Ok(Some(ActualState {
            id,
            pid: handle.pid,
            status,
        }))
    }
}

#[async_trait]
impl Hypervisor for QemuHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        if self.handle(vm.id).is_some() {
            return self.resume_if_prelaunch(vm.id).await;
        }
        let paths = self.paths(vm.id)?;
        if qemu_alive(&paths.pidfile) {
            // 살아 있지만 등록되지 않은 VM(재시작 때 붙지 못함, QMP 연결 유실). 새로 띄우면
            // pidfile 잠금에 걸려 실패하고 살아 있는 VM의 파일을 지우게 되므로 다시 붙기만 한다.
            match tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths, false)).await {
                Ok(Ok(())) => {
                    tracing::info!(id = %vm.id, "reattached to running VM");
                    return self.resume_if_prelaunch(vm.id).await;
                }
                Ok(Err(e)) => {
                    tracing::error!(id = %vm.id, error = %e, "could not reattach to running VM")
                }
                Err(_) => tracing::error!(id = %vm.id, "timed out reattaching to running VM"),
            }
            return Err(DriverError::Timeout("reattach to running QEMU"));
        }
        if let Err(e) = self.launch(vm, &paths).await {
            if self.handle(vm.id).is_none() {
                self.cleanup_failed_launch(vm.id, &paths).await;
            }
            return Err(e);
        }
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(());
        };
        match mode {
            StopMode::Acpi => {
                let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.system_powerdown()).await;
                flatten(r, "QMP system_powerdown")?;
            }
            StopMode::Force => {
                let pid = handle.pid;
                let pidfile = handle.paths.pidfile.clone();
                match tokio::time::timeout(QMP_TIMEOUT, handle.qmp.quit()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(%id, error = %e, "QMP quit failed"),
                    Err(_) => tracing::warn!(%id, "QMP quit timed out"),
                }
                drop(handle);
                // 등록이 풀렸어도 QMP만 끊기고 프로세스는 살아 있을 수 있으므로 pidfile도 확인한다.
                let gone = || self.handle(id).is_none() && !qemu_alive(&pidfile);
                if !wait_until(FORCE_STOP_TIMEOUT, gone).await {
                    tracing::warn!(%id, pid, "QEMU did not quit; sending SIGKILL");
                    if qemu_alive(&pidfile) {
                        kill_qemu(pid);
                    }
                    if !wait_until(KILL_TIMEOUT, gone).await {
                        return Err(DriverError::Timeout("QEMU to exit"));
                    }
                }
            }
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        self.query_state(id).await
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let ids: Vec<Uuid> = self.vms.lock().unwrap().keys().copied().collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.query_state(id).await {
                Ok(Some(st)) => out.push(st),
                Ok(None) => {}
                // VM 하나가 응답하지 않는다고 전체 조회가 실패하면 안 된다.
                Err(DriverError::Timeout(_)) => {
                    if let Some(handle) = self.handle(id) {
                        out.push(ActualState {
                            id,
                            pid: handle.pid,
                            status: RuntimeStatus::Other("unresponsive".into()),
                        });
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        self.handle(id)
            .map(|h| h.paths.vnc_socket.clone())
            .ok_or(DriverError::NotRunning(id))
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

/// QMP 연결이 끊길 때까지 이벤트를 지켜보다가, 종료 종류를 판정해 알리고 런타임 파일을 정리한다.
async fn monitor(
    id: Uuid,
    mut rx: broadcast::Receiver<QmpEvent>,
    // Weak: 맵이 핸들을, 핸들이 이 태스크를 소유하므로 강한 참조를 쥐면 순환이 생겨
    // 드라이버를 내려도 QMP 연결이 닫히지 않는다.
    vms: Weak<Mutex<HashMap<Uuid, Arc<VmHandle>>>>,
    events: broadcast::Sender<HypervisorEvent>,
    paths: RuntimePaths,
) {
    let mut guest_initiated = None;
    loop {
        match rx.recv().await {
            Ok(ev) if ev.event == "SHUTDOWN" => {
                guest_initiated = Some(ev.data["guest"].as_bool().unwrap_or(false));
            }
            Ok(_) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
    }
    let unregister = || {
        if let Some(vms) = vms.upgrade() {
            let handle = vms.lock().unwrap().remove(&id);
            drop(handle);
        }
    };
    // QEMU는 종료 중에 소켓을 먼저 닫으므로 프로세스가 끝나기를 잠시 기다린다.
    if !wait_until(EXIT_GRACE, || !qemu_alive(&paths.pidfile)).await {
        // QMP만 끊기고 QEMU는 살아 있다. 파일을 지우거나 종료로 알리지 않고 등록만 푼다.
        // 이후 start()가 pidfile 잠금을 보고 다시 붙는다.
        tracing::error!(%id, "lost QMP connection to running QEMU");
        unregister();
        return;
    }
    let kind = match guest_initiated {
        Some(guest_initiated) => EventKind::Stopped { guest_initiated },
        None => EventKind::Crashed,
    };
    if let Err(e) = std::fs::remove_dir_all(&paths.dir) {
        tracing::warn!(%id, error = %e, "failed to remove runtime dir");
    }
    // 이벤트를 받은 쪽이 곧바로 start를 불러도 "이미 실행 중"으로 오판하지 않도록 등록부터 해제한다.
    // 핸들이 drop되면서 이 태스크에 abort가 걸리지만, 아래에는 await가 없으므로 끝까지 실행된다.
    unregister();
    let _ = events.send(HypervisorEvent { id, kind });
}

/// QEMU는 수명 내내 `-pidfile`에 fcntl 잠금을 쥐고 있다(SIGSTOP 상태에서도). 잠금을 잡을 수
/// 있으면 QEMU는 없다. 판단할 수 없으면 살아 있는 것으로 본다(파일을 지우지 않는 쪽).
fn qemu_alive(pidfile: &Path) -> bool {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pidfile)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(e) => {
            tracing::warn!(?pidfile, error = %e, "cannot open pidfile; assuming QEMU is alive");
            return true;
        }
    };
    match fcntl_lock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            // 잡은 잠금은 바로 놓는다. 이 프로세스가 쥐고 있으면 새 QEMU가 pidfile을 잠그지 못한다.
            let _ = fcntl_lock(&file, FlockOperation::Unlock);
            false
        }
        Err(e) => {
            if e != rustix::io::Errno::WOULDBLOCK && e != rustix::io::Errno::ACCESS {
                tracing::warn!(?pidfile, error = %e, "cannot test pidfile lock; assuming QEMU is alive");
            }
            true
        }
    }
}

/// SIGKILL을 보낸다. 이미 없는 프로세스는 무시한다. pidfile 잠금으로 QEMU임을 확인한 pid에만 쓴다.
fn kill_qemu(pid: u32) {
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        tracing::warn!(pid, "invalid pid");
        return;
    };
    match kill_process(pid, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(e) => {
            tracing::warn!(pid = pid.as_raw_nonzero().get(), error = %e, "failed to kill QEMU")
        }
    }
}

/// `cond`가 참이 될 때까지 짧게 쉬며 확인한다. 한도 안에 참이 되면 true.
async fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    tokio::time::timeout(limit, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

/// QMP_TIMEOUT으로 감싼 명령의 결과를 풀어 낸다.
fn flatten<T>(
    r: Result<Result<T, QmpError>, tokio::time::error::Elapsed>,
    what: &'static str,
) -> Result<T, DriverError> {
    Ok(r.map_err(|_| DriverError::Timeout(what))??)
}

fn read_pid(pidfile: &Path) -> Result<u32, DriverError> {
    let text = std::fs::read_to_string(pidfile)?;
    text.trim().parse().map_err(|_| {
        DriverError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad pidfile {pidfile:?}: {text:?}"),
        ))
    })
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use opl_qemu::{Accel, Arch};
    use opl_types::vm::{CpuModel, CpuSpec, MachineType};

    use super::*;

    fn config(root: &Path, qemu_binary: PathBuf) -> QemuConfig {
        let template = root.join("vars-template.fd");
        std::fs::write(&template, b"vars").unwrap();
        QemuConfig {
            host: HostProfile {
                arch: Arch::Aarch64,
                accel: Accel::Tcg,
                qemu_binary,
                uefi_code: root.join("code.fd"),
                uefi_vars_template: template,
            },
            run_dir: root.join("run"),
            state_dir: root.join("state"),
        }
    }

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
        }
    }

    fn short_tempdir() -> tempfile::TempDir {
        // macOS의 기본 TMPDIR은 소켓 경로 한도를 넘기므로 /tmp를 쓴다.
        tempfile::Builder::new()
            .prefix("opl")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[tokio::test]
    async fn start_failure_reports_qemu_stderr_and_cleans_run_dir() {
        let tmp = short_tempdir();
        let fake = tmp.path().join("fake-qemu");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'qemu: something broke' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hv = QemuHypervisor::new(config(tmp.path(), fake)).await.unwrap();
        let vm = spec();

        let err = hv.start(&vm).await.unwrap_err();
        match &err {
            DriverError::StartFailed(msg) => assert_eq!(msg, "qemu: something broke"),
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(!err.is_transient());
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
        // UEFI 변수 파일은 state_dir에 복사되어 남는다.
        let vars = tmp
            .path()
            .join("state/vm")
            .join(vm.id.to_string())
            .join("uefi-vars.fd");
        assert_eq!(std::fs::read(vars).unwrap(), b"vars");
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn new_removes_stale_runtime_dirs_and_ignores_foreign_entries() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        // pidfile이 없다.
        let stale = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("qmp.sock"), b"not a socket").unwrap();
        // pidfile은 있지만 아무도 잠그고 있지 않다(kill -9 뒤의 모습).
        let unlocked = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        std::fs::create_dir_all(&unlocked).unwrap();
        std::fs::write(unlocked.join("qemu.pid"), b"4242\n").unwrap();
        let foreign = cfg.run_dir.join("vm").join("not-a-uuid");
        std::fs::create_dir_all(&foreign).unwrap();

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!stale.exists());
        assert!(!unlocked.exists());
        assert!(foreign.exists());
        assert!(hv.list().await.unwrap().is_empty());
    }

    /// 살아 있는 QEMU처럼 `dir/qemu.pid`에 fcntl 잠금을 걸고 자기 pid를 써 두는 자식 프로세스.
    /// fcntl 잠금은 프로세스 단위라 테스트 프로세스가 직접 잠그면 드라이버가 잠금을 볼 수 없다.
    /// 반환값을 drop하면 자식이 죽어 잠금이 풀린다.
    async fn hold_pidfile(dir: &Path) -> tokio::process::Child {
        use tokio::io::{AsyncBufReadExt, BufReader};

        const SCRIPT: &str = "import fcntl, os, sys, time
fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o644)
fcntl.lockf(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, b'%d\\n' % os.getpid())
print('locked', flush=True)
time.sleep(60)
";
        std::fs::create_dir_all(dir).unwrap();
        let mut child = tokio::process::Command::new("python3")
            .arg("-c")
            .arg(SCRIPT)
            .arg(dir.join("qemu.pid"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .await
            .unwrap();
        assert_eq!(line.trim(), "locked");
        child
    }

    struct FakeQmp {
        /// 클라이언트가 연결을 닫으면 완료된다.
        closed: tokio::sync::oneshot::Receiver<()>,
        /// 받은 명령 이름
        commands: Arc<Mutex<Vec<String>>>,
        /// 보내면 서버가 연결을 끊는다.
        hangup: tokio::sync::oneshot::Sender<()>,
    }

    /// QEMU 대신 QMP 소켓을 여는 가짜 서버. greeting과 capabilities 협상까지 처리하고,
    /// `query-status`에는 `status`를 답한다.
    async fn fake_qmp(dir: &Path, status: &'static str) -> FakeQmp {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        std::fs::create_dir_all(dir).unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let (hangup, mut hangup_rx) = tokio::sync::oneshot::channel::<()>();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write
                .write_all(b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\n")
                .await
                .unwrap();
            loop {
                let line = tokio::select! {
                    _ = &mut hangup_rx => break,
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => line,
                        _ => break,
                    },
                };
                let cmd: serde_json::Value = serde_json::from_str(&line).unwrap();
                let name = cmd["execute"].as_str().unwrap_or_default().to_string();
                let ret = match name.as_str() {
                    "query-status" => {
                        serde_json::json!({ "running": status == "running", "status": status })
                    }
                    _ => serde_json::json!({}),
                };
                recorded.lock().unwrap().push(name);
                let reply = serde_json::json!({ "return": ret, "id": cmd["id"] });
                if write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let _ = closed_tx.send(());
        });
        FakeQmp {
            closed,
            commands,
            hangup,
        }
    }

    #[tokio::test]
    async fn dropping_the_driver_releases_qmp_connections() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let dir = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert_eq!(hv.list().await.unwrap().len(), 1);
        drop(hv);

        // QEMU는 QMP 클라이언트를 하나만 받으므로, 연결이 남아 있으면 재시작한 opld가 붙을 수 없다.
        tokio::time::timeout(Duration::from_secs(2), fake.closed)
            .await
            .expect("QMP connection was not closed after dropping the driver")
            .unwrap();
        assert!(dir.exists(), "runtime dir of a live VM must not be removed");
    }

    #[tokio::test]
    async fn new_keeps_runtime_dir_when_reattach_fails_for_other_reasons() {
        use tokio::io::AsyncWriteExt;

        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let dir = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        // pidfile이 잠겨 있으니 QEMU는 살아 있다. 소켓은 QMP가 아니어서 붙을 수 없지만 지우면 안 된다.
        let _qemu = hold_pidfile(&dir).await;
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"{\"hello\":1}\n").await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.list().await.unwrap().is_empty());
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn start_continues_vm_left_in_prelaunch() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // opld가 -daemonize와 cont 사이에 죽어 -S 상태로 남은 VM
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "prelaunch").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.inspect(vm.id).await.unwrap().is_some());

        hv.start(&vm).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn start_does_not_touch_live_vm_it_cannot_attach_to() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // QEMU는 살아 있지만(SIGSTOP 등) QMP에 답하지 않는다.
        let _qemu = hold_pidfile(&dir).await;
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let _conn = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let err = hv.start(&vm).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(dir.join("qemu.pid").exists());
        assert!(dir.join("qmp.sock").exists());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn losing_qmp_while_qemu_lives_unregisters_without_cleanup() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mut rx = hv.subscribe();

        fake.hangup.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while hv.handle(vm.id).is_some() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("VM was not unregistered");
        // 살아 있는 VM이므로 파일을 지우거나 종료로 알리면 안 된다. 이후 start()가 다시 붙는다.
        assert!(dir.join("qemu.pid").exists());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn stop_and_console_on_unknown_vm() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let id = Uuid::now_v7();
        hv.stop(id, StopMode::Force).await.unwrap();
        hv.stop(id, StopMode::Acpi).await.unwrap();
        assert!(matches!(
            hv.console_endpoint(id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn incompatible_spec_is_invalid_not_start_failure() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let vm = VmRuntimeSpec {
            machine: MachineType::Q35,
            ..spec()
        };
        assert!(matches!(
            hv.start(&vm).await,
            Err(DriverError::InvalidSpec(_))
        ));
    }
}
