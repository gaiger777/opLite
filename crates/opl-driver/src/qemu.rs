use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::{HostProfile, QmpClient, QmpError, QmpEvent, RunState, RuntimePaths, build_cmdline};
use opl_types::network::tap_prefix;
use opl_types::runtime::{NicBackend, VmRuntimeSpec};
use opl_types::vm::Firmware;
use rustix::process::{Flock, FlockOffsetType, FlockType, Pid, Signal, fcntl_getlk, kill_process};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
use crate::net::{HostNet, Tap};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
/// QMP 명령 하나의 응답을 기다리는 한도. 멈춘(SIGSTOP 등) QEMU에서 호출자가 묶이지 않게 한다.
const QMP_TIMEOUT: Duration = Duration::from_secs(5);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// SIGKILL을 보낸 뒤 프로세스가 사라지기를 기다리는 한도
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// SHUTDOWN 없이 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 기다리는 한도.
/// 넘기면 QMP만 끊긴 것으로 본다.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// SHUTDOWN 뒤 연결이 끊겼을 때 런타임 디렉터리를 지우려고 프로세스 종료를 잠깐 기다리는 한도.
/// QEMU는 소켓을 닫고 곧 끝나지만, 플러시가 길면 디렉터리는 다음 launch/new()에 맡기고 종료를 바로 알린다.
const SHUTDOWN_EXIT_WAIT: Duration = Duration::from_secs(1);
/// 내부 스냅샷(저장·불러오기·삭제) 하나를 기다리는 한도. 메모리 스냅샷은 메모리 크기만큼 걸린다.
/// Task 한도(600초)보다 짧아야 Task가 먼저 끊기지 않고 이 오류(작업이 남아 있을 수 있음)를 받는다.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(540);

/// VM별 상태 디렉터리 `{state_dir}/vm/{id}`. `state_dir`은 [`QemuConfig::state_dir`]이다.
pub fn vm_state_dir(state_dir: &Path, id: Uuid) -> PathBuf {
    state_dir.join("vm").join(id.to_string())
}

/// VM의 UEFI 변수 파일. UEFI VM을 처음 띄울 때 템플릿을 복사해 만든다(BIOS VM에는 없다).
pub fn uefi_vars_path(state_dir: &Path, id: Uuid) -> PathBuf {
    vm_state_dir(state_dir, id).join("uefi-vars.fd")
}

#[derive(Debug, Clone)]
pub struct QemuConfig {
    pub host: HostProfile,
    /// 소켓과 pidfile을 두는 휘발성 디렉터리 (Linux: `/run/opl`)
    pub run_dir: PathBuf,
    /// UEFI 변수처럼 재부팅 후에도 남아야 하는 VM별 파일 (Linux: `/var/lib/opl`)
    pub state_dir: PathBuf,
    /// 브리지 NIC의 tap을 만들고 지운다. 없으면(macOS) 브리지 NIC를 가진 VM은 시작하지 않는다.
    pub net: Option<Arc<dyn HostNet>>,
    /// QEMU를 `systemd-run --scope`로 감싸 VM마다 cgroup(`opl-vm-<id>.scope`)으로 분리한다.
    /// systemd가 있는 Linux에서만 켠다.
    pub systemd_scope: bool,
}

/// QEMU 프로세스를 `-daemonize`로 띄워 opld와 분리한다. opld가 재시작해도 VM은 계속 돈다.
///
/// QEMU의 생존 여부는 QMP 응답이 아니라 pidfile의 fcntl 잠금으로 판정한다([`liveness`]).
/// 잠금이 걸렸거나 판정할 수 없는 런타임 디렉터리는 절대 지우지 않고, 잠금 소유자로 확인한
/// pid만 죽인다.
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
            match liveness(&paths.pidfile) {
                Liveness::Alive(_) => {}
                Liveness::Dead => {
                    tracing::warn!(%id, "cleaning up stale runtime dir");
                    // 런타임 디렉터리가 정리할 것이 남았다는 유일한 기록이므로 tap부터 지운다.
                    // 지우지 못하면 디렉터리를 남겨 다음 new()가 다시 시도한다.
                    if delete_taps(hv.config.net.as_deref(), id).await
                        && let Err(e) = std::fs::remove_dir_all(&paths.dir)
                    {
                        tracing::error!(%id, error = %e, "failed to remove stale runtime dir");
                    }
                    continue;
                }
                Liveness::Unknown => {
                    tracing::error!(%id, "cannot tell whether QEMU is running; leaving runtime dir");
                    continue;
                }
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
        paths.uefi_vars = uefi_vars_path(&self.config.state_dir, id);
        Ok(paths)
    }

    fn state_dir(&self, id: Uuid) -> PathBuf {
        vm_state_dir(&self.config.state_dir, id)
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
            self.config.net.clone(),
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

    async fn launch(
        &self,
        vm: &VmRuntimeSpec,
        paths: &RuntimePaths,
        restore: Option<&str>,
    ) -> Result<(), DriverError> {
        std::fs::create_dir_all(&paths.dir)?;
        std::fs::create_dir_all(self.state_dir(vm.id))?;
        if vm.firmware == Firmware::Uefi && !paths.uefi_vars.exists() {
            std::fs::copy(&self.config.host.uefi_vars_template, &paths.uefi_vars)?;
        }
        // 호출자가 pidfile 잠금이 풀린 것(= QEMU 없음)을 확인했으므로 남은 소켓은 지워도 된다.
        // 이전 인스턴스가 남긴 pidfile은 새 QEMU가 잠그고 다시 쓴다.
        for sock in [&paths.qmp_socket, &paths.vnc_socket] {
            remove_if_exists(sock)?;
        }

        let mut args = build_cmdline(vm, &self.config.host, paths)
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        args.push("-daemonize".into());
        self.create_taps(vm).await?;
        let mut cmd = if self.config.systemd_scope {
            clear_stale_scope(vm.id).await;
            // 스코프는 QEMU가 끝날 때까지 남는다. --collect: 실패로 끝나도 남기지 않는다.
            let mut cmd = tokio::process::Command::new("systemd-run");
            cmd.args(["--scope", "--quiet", "--collect"])
                .arg(format!("--unit=opl-vm-{}", vm.id))
                .arg("--")
                .arg(&self.config.host.qemu_binary);
            cmd
        } else {
            tokio::process::Command::new(&self.config.host.qemu_binary)
        };
        cmd.args(&args)
            .stdin(Stdio::null())
            // 타임아웃으로 기다림을 그만두면 부모 프로세스도 끝낸다. 이미 분리된 자식은
            // 실패 정리(kill_and_cleanup)가 pidfile 잠금으로 찾아 끝낸다.
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
        if let Some(tag) = restore {
            // -S로 멈춘 채 불러온 뒤 cont로 이어서 실행한다. 메모리 상태는 첫 디스크(disk0)에 있다.
            let devices: Vec<String> = (0..vm.disks.len()).map(|i| format!("disk{i}")).collect();
            let load = job(
                handle
                    .qmp
                    .snapshot_load(tag, "disk0", &devices, SNAPSHOT_TIMEOUT),
                "QMP snapshot-load",
            )
            .await;
            if let Err(e) = load {
                drop(handle);
                // 작업이 끝나지 않았으면 QMP가 답하지 않을 수 있으므로 끝날 때까지 기다리고 필요하면 죽인다.
                if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                    tracing::error!(id = %vm.id, error = %stop, "failed to stop QEMU after snapshot load failed");
                }
                return Err(e);
            }
        }
        let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
        if let Err(e) = flatten(cont, "QMP cont") {
            let _ = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.quit()).await;
            return Err(e);
        }
        Ok(())
    }

    /// 브리지 NIC마다 tap을 만들어 브리지에 붙인다. 하나라도 실패하면 만든 것을 지운다.
    async fn create_taps(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        let taps: Vec<Tap> = vm
            .nics
            .iter()
            .filter_map(|nic| match &nic.backend {
                NicBackend::Tap {
                    ifname,
                    bridge,
                    vlan,
                } => Some(Tap {
                    ifname: ifname.clone(),
                    bridge: bridge.clone(),
                    vlan: *vlan,
                }),
                NicBackend::User => None,
            })
            .collect();
        if taps.is_empty() {
            return Ok(());
        }
        // create_tap은 같은 이름의 장치를 먼저 지우므로, 이 VM의 이름이 아니면 호스트 인터페이스를 지울 수 있다.
        let prefix = tap_prefix(vm.id);
        if let Some(tap) = taps.iter().find(|t| !t.ifname.starts_with(&prefix)) {
            return Err(DriverError::InvalidSpec(format!(
                "tap name {:?} does not start with {prefix:?}",
                tap.ifname
            )));
        }
        let Some(net) = &self.config.net else {
            return Err(DriverError::InvalidSpec(
                "bridge networks are not supported on this node".into(),
            ));
        };
        for tap in &taps {
            if let Err(e) = net.create_tap(tap).await {
                delete_taps(Some(net.as_ref()), vm.id).await;
                return Err(e);
            }
        }
        Ok(())
    }

    /// 등록되지 않은 QEMU(기동 실패로 남은 데몬, 붙지 못한 VM)를 SIGKILL로 끝내고 런타임
    /// 디렉터리를 지운다. 끝냈음을 확인하지 못하면 살아 있는 VM의 파일이므로 아무것도 지우지
    /// 않고 false를 돌려준다.
    async fn kill_and_cleanup(&self, id: Uuid, paths: &RuntimePaths) -> bool {
        if liveness(&paths.pidfile) != Liveness::Dead {
            let pid = match read_pid(&paths.pidfile) {
                Ok(pid) => pid,
                Err(e) => {
                    tracing::error!(%id, error = %e, "QEMU has no readable pid; keeping runtime dir");
                    return false;
                }
            };
            kill_if_owner(&paths.pidfile, pid);
            if !wait_until(KILL_TIMEOUT, || liveness(&paths.pidfile) == Liveness::Dead).await {
                tracing::error!(%id, pid, "QEMU did not exit; keeping runtime dir");
                return false;
            }
        }
        // tap부터 지운다. 지우지 못하면 런타임 디렉터리를 남겨 다음 new()가 다시 시도한다.
        if delete_taps(self.config.net.as_deref(), id).await
            && let Err(e) = std::fs::remove_dir_all(&paths.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%id, error = %e, "failed to remove runtime dir");
        }
        true
    }

    /// QMP로 다룰 수 있는 실행 중인 VM의 핸들
    fn running_handle(&self, id: Uuid) -> Result<Arc<VmHandle>, DriverError> {
        if let Some(handle) = self.handle(id) {
            return Ok(handle);
        }
        match liveness(&self.paths(id)?.pidfile) {
            Liveness::Dead => Err(DriverError::NotRunning(id)),
            // 살아 있지만 QMP로 다룰 수 없다. 꺼졌다고 알리면 호출자가 쓰고 있는 디스크를 건드린다.
            // 요청을 보내지 않았으므로 시간 초과(작업이 남아 있을 수 있음)와 구별한다.
            Liveness::Alive(_) | Liveness::Unknown => Err(DriverError::NotAttached(id)),
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
            // 살아 있지만 QMP에 붙지 못한 VM도 드러내야 reconciler가 놓치지 않는다.
            return match liveness(&self.paths(id)?.pidfile) {
                Liveness::Dead => Ok(None),
                Liveness::Alive(pid) => Ok(Some(ActualState {
                    id,
                    pid,
                    status: RuntimeStatus::Other("unresponsive".into()),
                })),
                Liveness::Unknown => Err(DriverError::Io(std::io::Error::other(format!(
                    "cannot tell whether QEMU of VM {id} is running"
                )))),
            };
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
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        // 이미 살아 있는 VM에는 restore를 쓰지 않는다. -S로 남은 VM도 그대로 이어서 실행한다.
        if self.handle(vm.id).is_some() {
            return self.resume_if_prelaunch(vm.id).await;
        }
        let paths = self.paths(vm.id)?;
        if liveness(&paths.pidfile) != Liveness::Dead {
            // 살아 있지만 등록되지 않은 VM(재시작 때 붙지 못함, QMP 연결 유실). 새로 띄우면
            // pidfile 잠금에 걸려 실패하고 살아 있는 VM의 파일을 지우게 되므로 다시 붙기만 한다.
            // 생존을 판정할 수 없을 때도 같은 이유로 새로 띄우지 않는다.
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
        if restore.is_some() && vm.disks.is_empty() {
            return Err(DriverError::InvalidSpec(
                "a snapshot to restore needs a disk for its memory state".into(),
            ));
        }
        if let Err(e) = self.launch(vm, &paths, restore).await {
            if self.handle(vm.id).is_none() {
                self.kill_and_cleanup(vm.id, &paths).await;
            }
            return Err(e);
        }
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            let paths = self.paths(id)?;
            if liveness(&paths.pidfile) == Liveness::Dead {
                return Ok(());
            }
            // 살아 있지만 QMP로 다룰 수 없는 VM
            return match mode {
                StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                StopMode::Force if self.kill_and_cleanup(id, &paths).await => Ok(()),
                StopMode::Force => Err(DriverError::Timeout("QEMU to exit")),
            };
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
                let gone = || self.handle(id).is_none() && liveness(&pidfile) == Liveness::Dead;
                if !wait_until(FORCE_STOP_TIMEOUT, gone).await {
                    tracing::warn!(%id, pid, "QEMU did not quit; sending SIGKILL");
                    kill_if_owner(&pidfile, pid);
                    if !wait_until(KILL_TIMEOUT, gone).await {
                        return Err(DriverError::Timeout("QEMU to exit"));
                    }
                }
            }
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        match self.query_state(id).await {
            // 멈춘 QEMU도 list()처럼 살아 있는 VM으로 드러내야 reconciler가 강제 종료로 넘어갈 수 있다.
            Err(DriverError::Timeout(what)) => match self.handle(id) {
                Some(handle) => Ok(Some(ActualState {
                    id,
                    pid: handle.pid,
                    status: RuntimeStatus::Other("unresponsive".into()),
                })),
                None => Err(DriverError::Timeout(what)),
            },
            other => other,
        }
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let mut ids: Vec<Uuid> = self.vms.lock().unwrap().keys().copied().collect();
        // 등록되지 않았지만 살아 있는 VM도 포함하도록 런타임 디렉터리를 함께 훑는다.
        for entry in std::fs::read_dir(self.config.run_dir.join("vm"))? {
            if let Some(id) = entry?
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.query_state(id).await {
                Ok(Some(st)) => out.push(st),
                Ok(None) => {}
                Err(DriverError::Io(e)) if self.handle(id).is_none() => {
                    tracing::error!(%id, error = %e, "skipping VM whose state is unknown");
                }
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

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        let Some(vmstate) = devices.first() else {
            return Err(DriverError::InvalidSpec(format!("VM {id} has no disks")));
        };
        if memory {
            let save = handle
                .qmp
                .snapshot_save(tag, vmstate, &devices, SNAPSHOT_TIMEOUT);
            job(save, "QMP snapshot-save").await
        } else {
            let r =
                tokio::time::timeout(SNAPSHOT_TIMEOUT, handle.qmp.snapshot_disks(tag, &devices))
                    .await;
            flatten(r, "QMP transaction")
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        if devices.is_empty() {
            return Ok(());
        }
        let delete = handle.qmp.snapshot_delete(tag, &devices, SNAPSHOT_TIMEOUT);
        job(delete, "QMP snapshot-delete").await
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let node = format!("disk{disk}");
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.block_resize(&node, size_bytes)).await;
        flatten(r, "QMP block_resize")
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

/// 실행 중인 VM의 디스크 노드 이름(`disk{i}`)을 디스크 순서로 읽는다. 다시 붙은 VM은 스펙을
/// 모르므로 QEMU에 묻는다.
async fn query_disk_nodes(qmp: &QmpClient) -> Result<Vec<String>, DriverError> {
    let r = tokio::time::timeout(QMP_TIMEOUT, qmp.execute("query-named-block-nodes", None)).await;
    Ok(disk_nodes(&flatten(r, "QMP query-named-block-nodes")?))
}

/// 블록 노드 목록에서 cmdline이 디스크 포맷 노드에 붙인 `disk{i}`만 골라 i 순서로 돌려준다.
fn disk_nodes(nodes: &serde_json::Value) -> Vec<String> {
    let mut disks: Vec<(usize, String)> = nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["node-name"].as_str())
        .filter_map(|name| {
            let i = name.strip_prefix("disk")?.parse().ok()?;
            Some((i, name.to_string()))
        })
        .collect();
    disks.sort();
    disks.into_iter().map(|(_, name)| name).collect()
}

/// QMP 작업 명령을 기다린다. 작업의 제한 시간은 이벤트를 기다리는 동안에만 걸리므로, QEMU가
/// 명령 자체에 답하지 않아도(SIGSTOP 등) 묶이지 않게 한 번 더 감싼다.
async fn job(
    fut: impl Future<Output = Result<(), QmpError>>,
    what: &'static str,
) -> Result<(), DriverError> {
    flatten(
        tokio::time::timeout(SNAPSHOT_TIMEOUT + QMP_TIMEOUT, fut).await,
        what,
    )
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
    net: Option<Arc<dyn HostNet>>,
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
    let dead = || liveness(&paths.pidfile) == Liveness::Dead;
    // (종료 종류, 프로세스가 끝나 런타임 디렉터리를 지워도 되는지)
    let (kind, exited) = match guest_initiated {
        // SHUTDOWN을 받았으면 종료는 확정이다. QEMU는 소켓을 닫고 곧 끝나므로 잠깐 기다려
        // 디렉터리를 지우되, 플러시 등으로 늦어지면 잠금이 걸린 디렉터리는 남겨 두고
        // (다음 launch/new()가 잠금 해제를 확인하고 정리한다) 종료는 바로 알린다.
        Some(guest_initiated) => {
            let exited = wait_until(SHUTDOWN_EXIT_WAIT, dead).await;
            if !exited {
                tracing::info!(%id, "QEMU is still exiting; leaving runtime dir");
            }
            (EventKind::Stopped { guest_initiated }, exited)
        }
        None => {
            if !wait_until(EXIT_GRACE, dead).await {
                // SHUTDOWN 없이 QMP만 끊기고 QEMU는 살아 있다. 파일을 지우거나 종료로 알리지
                // 않고 등록만 푼다. inspect/list에 "unresponsive"로 드러나고, 이후 start()가
                // 다시 붙거나 stop(Force)가 끝낸다.
                tracing::error!(%id, "lost QMP connection to running QEMU");
                unregister();
                return;
            }
            (EventKind::Crashed, true)
        }
    };
    // 등록을 풀기 전에 tap을 지운다. QEMU가 아직 끝나는 중이어도 tap은 곧바로 지워진다(남은 fd는
    // 오류를 받는다). 게스트는 이미 꺼졌으므로 괜찮다. 런타임 디렉터리는 정리할 것이 남았다는
    // 유일한 기록이므로 tap을 지운 뒤에 지운다. 그 전에 끊기거나(드라이버가 내려가 이 태스크가
    // abort됨) tap을 지우지 못하면 디렉터리가 남아 다음 launch/new()가 다시 정리한다.
    if delete_taps(net.as_deref(), id).await
        && exited
        && let Err(e) = std::fs::remove_dir_all(&paths.dir)
    {
        tracing::warn!(%id, error = %e, "failed to remove runtime dir");
    }
    // 이벤트를 받은 쪽이 곧바로 start를 불러도 "이미 실행 중"으로 오판하지 않도록 등록부터 해제한다.
    // 핸들이 drop되면서 이 태스크에 abort가 걸리지만, 아래에는 await가 없으므로 끝까지 실행된다.
    unregister();
    let _ = events.send(HypervisorEvent { id, kind });
}

/// VM의 tap을 지운다. 지웠거나 지울 것이 없으면 true. 실패하면 기록하고 false를 돌려주며,
/// 호출자는 런타임 디렉터리를 남겨 다음 new()가 다시 지우게 한다.
async fn delete_taps(net: Option<&dyn HostNet>, id: Uuid) -> bool {
    if let Some(net) = net
        && let Err(e) = net.delete_vm_taps(id).await
    {
        tracing::warn!(%id, error = %e, "failed to delete tap devices");
        return false;
    }
    true
}

/// 끝난 QEMU의 `--collect` 스코프는 systemd가 비동기로 회수하므로, 곧바로 다시 띄우면
/// systemd-run이 "Unit ... already exists"로 실패한다. 호출자가 이전 QEMU가 없음을 확인했으므로
/// 남은 스코프를 멈추고 실패 기록을 지운다. 오류와 출력은 무시한다.
async fn clear_stale_scope(id: Uuid) {
    let unit = format!("opl-vm-{id}.scope");
    for verb in ["stop", "reset-failed"] {
        let status = tokio::process::Command::new("systemctl")
            .args([verb, "--quiet", &unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        let _ = tokio::time::timeout(KILL_TIMEOUT, status).await;
    }
}

/// pidfile로 본 QEMU의 생존 여부
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    /// 이 pid가 pidfile 잠금을 쥐고 있다.
    Alive(u32),
    Dead,
    /// 판정할 수 없다. 살아 있을 수 있으므로 파일을 지우거나 프로세스를 죽이지 않는다.
    Unknown,
}

/// QEMU는 수명 내내 `-pidfile`에 POSIX 잠금(lockf)을 쥐고 있다(SIGSTOP 상태에서도).
/// `F_GETLK`로 잠금을 잡지 않고 소유자를 조회한다. 잠금이 없으면 QEMU는 없다.
/// 같은 프로세스가 쥔 잠금은 보이지 않지만, opld는 pidfile을 잠그지 않는다.
fn liveness(pidfile: &Path) -> Liveness {
    let file = match std::fs::File::open(pidfile) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Liveness::Dead,
        Err(e) => {
            tracing::warn!(?pidfile, error = %e, "cannot open pidfile");
            return Liveness::Unknown;
        }
    };
    // 파일 전체(길이 0 = 끝까지)에 대한 쓰기 잠금을 막는 잠금이 있는지 묻는다.
    let probe = Flock {
        start: 0,
        length: 0,
        pid: None,
        typ: FlockType::WriteLock,
        offset_type: FlockOffsetType::Set,
    };
    match fcntl_getlk(&file, &probe) {
        Ok(None) => Liveness::Dead,
        Ok(Some(lock)) => match lock.pid {
            Some(pid) => Liveness::Alive(pid.as_raw_nonzero().get() as u32),
            None => {
                tracing::warn!(?pidfile, "pidfile lock has no owner pid");
                Liveness::Unknown
            }
        },
        Err(e) => {
            tracing::warn!(?pidfile, error = %e, "cannot query pidfile lock");
            Liveness::Unknown
        }
    }
}

/// `pid`가 지금 pidfile 잠금의 소유자일 때만 SIGKILL을 보낸다. pid 재사용이나 잘못된 pidfile로
/// 엉뚱한 프로세스를 죽이지 않기 위해서다.
fn kill_if_owner(pidfile: &Path, pid: u32) {
    match liveness(pidfile) {
        Liveness::Alive(owner) if owner == pid => {}
        Liveness::Dead => return,
        other => {
            tracing::error!(
                ?pidfile,
                pid,
                ?other,
                "pid does not own the pidfile lock; not killing"
            );
            return;
        }
    }
    let Some(target) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    match kill_process(target, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(e) => tracing::warn!(pid, error = %e, "failed to kill QEMU"),
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
    use opl_types::runtime::RuntimeNic;
    use opl_types::vm::{CpuModel, CpuSpec, MachineType, NicModel};

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
            net: None,
            systemd_scope: false,
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

        let err = hv.start(&vm, None).await.unwrap_err();
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
        /// 받은 명령의 인자(`commands`와 같은 순서)
        arguments: Arc<Mutex<Vec<serde_json::Value>>>,
        /// 보내면 서버가 (주어진 이벤트를 먼저 보낸 뒤) 연결을 끊는다.
        hangup: tokio::sync::oneshot::Sender<Option<serde_json::Value>>,
    }

    /// QEMU 대신 QMP 소켓을 여는 가짜 서버. greeting과 capabilities 협상까지 처리하고,
    /// `query-status`에는 `status`를 답한다.
    async fn fake_qmp(dir: &Path, status: &'static str) -> FakeQmp {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        std::fs::create_dir_all(dir).unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let (hangup, mut hangup_rx) = tokio::sync::oneshot::channel::<Option<serde_json::Value>>();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        let args = Arc::new(Mutex::new(Vec::new()));
        let arguments = args.clone();
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
                    ev = &mut hangup_rx => {
                        if let Ok(Some(ev)) = ev {
                            let _ = write.write_all(format!("{ev}\n").as_bytes()).await;
                        }
                        break;
                    }
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
                    // 디스크 두 개를 가진 VM의 블록 노드
                    "query-named-block-nodes" => serde_json::json!([
                        { "node-name": "disk1" },
                        { "node-name": "disk1-file" },
                        { "node-name": "disk0" },
                        { "node-name": "disk0-file" },
                    ]),
                    _ => serde_json::json!({}),
                };
                recorded.lock().unwrap().push(name);
                arguments.lock().unwrap().push(cmd["arguments"].clone());
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
            arguments: args,
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
        let id = Uuid::now_v7();
        let dir = cfg.run_dir.join("vm").join(id.to_string());
        // pidfile이 잠겨 있으니 QEMU는 살아 있다. 소켓은 QMP가 아니어서 붙을 수 없지만 지우면 안 된다.
        let _qemu = hold_pidfile(&dir).await;
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"{\"hello\":1}\n").await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.handle(id).is_none());
        // 등록하지 않지만 살아 있는 VM으로 드러난다.
        let listed = hv.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].status,
            RuntimeStatus::Other("unresponsive".into())
        );
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

        hv.start(&vm, None).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
    }

    #[test]
    fn disk_nodes_are_format_nodes_in_disk_order() {
        let nodes = serde_json::json!([
            { "node-name": "disk10" },
            { "node-name": "disk2-file" },
            { "node-name": "disk2" },
            { "node-name": "#block123" },
            { "node-name": "diskx" },
            { "node-name": "disk0" },
        ]);
        assert_eq!(disk_nodes(&nodes), ["disk0", "disk2", "disk10"]);
    }

    #[tokio::test]
    async fn snapshot_ops_use_disk_node_names() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        // 디스크만 스냅샷은 모든 디스크를 transaction 하나로 찍는다.
        hv.snapshot_save(vm.id, "s1", false).await.unwrap();
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        let commands = fake.commands.lock().unwrap().clone();
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        let actions = &arguments[at("transaction")]["actions"];
        assert_eq!(actions[0]["data"]["device"], "disk0");
        assert_eq!(actions[1]["data"]["device"], "disk1");
        assert_eq!(actions[1]["data"]["name"], "s1");
        assert_eq!(
            arguments[at("block_resize")],
            serde_json::json!({ "node-name": "disk1", "size": 2u64 << 30 })
        );
    }

    #[tokio::test]
    async fn snapshot_ops_on_vm_that_is_not_attached() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let err = hv
            .snapshot_save(Uuid::now_v7(), "s", true)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::NotRunning(_)), "{err:?}");

        // 살아 있지만 QMP로 다룰 수 없는 VM을 꺼진 것으로 알리면 호출자가 쓰고 있는 디스크를
        // qemu-img로 건드린다. 다시 시도할 수 있는 오류로 끝낸다.
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = unresponsive_vm(&dir).await;
        for err in [
            hv.snapshot_save(vm.id, "s", true).await.unwrap_err(),
            hv.snapshot_delete(vm.id, "s").await.unwrap_err(),
            hv.block_resize(vm.id, 0, 1 << 30).await.unwrap_err(),
        ] {
            assert!(matches!(err, DriverError::NotAttached(_)), "{err:?}");
            assert!(err.is_transient());
        }
    }

    #[tokio::test]
    async fn restore_without_disks_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        // 메모리 상태는 첫 디스크에 있으므로 디스크가 없으면 띄우기 전에 거부한다.
        let err = hv.start(&spec(), Some("s1")).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }

    #[tokio::test]
    async fn start_does_not_touch_live_vm_it_cannot_attach_to() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // QEMU는 살아 있지만(SIGSTOP 등) QMP에 답하지 않는다.
        let qemu = unresponsive_vm(&dir).await;

        let err = hv.start(&vm, None).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(dir.join("qemu.pid").exists());
        assert!(dir.join("qmp.sock").exists());
        // 등록되지 않았어도 살아 있는 VM은 조회에 드러나야 reconciler가 놓치지 않는다.
        let expected = ActualState {
            id: vm.id,
            pid: qemu.id().unwrap(),
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(expected.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![expected]);
    }

    /// QEMU는 살아 있지만(SIGSTOP 등) QMP에 답하지 않는 런타임 디렉터리를 만든다.
    async fn unresponsive_vm(dir: &Path) -> tokio::process::Child {
        let qemu = hold_pidfile(dir).await;
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let _conn = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        qemu
    }

    #[tokio::test]
    async fn liveness_reports_lock_owner() {
        let tmp = short_tempdir();
        let dir = tmp.path().join("vm");
        assert_eq!(liveness(&dir.join("qemu.pid")), Liveness::Dead);
        let mut qemu = hold_pidfile(&dir).await;
        assert_eq!(
            liveness(&dir.join("qemu.pid")),
            Liveness::Alive(qemu.id().unwrap())
        );
        qemu.kill().await.unwrap();
        // kill -9 뒤에도 pidfile은 남지만 잠금은 풀린다.
        assert!(dir.join("qemu.pid").exists());
        assert_eq!(liveness(&dir.join("qemu.pid")), Liveness::Dead);
    }

    #[tokio::test]
    async fn force_stop_kills_live_unregistered_vm() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let mut qemu = unresponsive_vm(&dir).await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.inspect(vm.id).await.unwrap().is_some());

        // ACPI 요청은 QMP 없이 보낼 수 없다. 다시 시도할 수 있는 오류다.
        let err = hv.stop(vm.id, StopMode::Acpi).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(dir.exists());

        hv.stop(vm.id, StopMode::Force).await.unwrap();
        let status = tokio::time::timeout(Duration::from_secs(2), qemu.wait())
            .await
            .expect("QEMU was not killed")
            .unwrap();
        assert!(!status.success());
        assert!(!dir.exists());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn force_stop_never_kills_a_pid_that_does_not_own_the_lock() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = unresponsive_vm(&dir).await;
        // pidfile에 적힌 pid가 잠금 소유자와 다르다(pid 재사용, 다른 프로세스가 덮어씀 등).
        let mut bystander = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let bystander_pid = bystander.id().unwrap();
        std::fs::write(dir.join("qemu.pid"), format!("{bystander_pid}\n")).unwrap();
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let err = hv.stop(vm.id, StopMode::Force).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "bystander was killed"
        );
        assert!(dir.join("qemu.pid").exists());
    }

    #[tokio::test]
    async fn guest_shutdown_is_reported_even_if_qemu_exits_slowly() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // QEMU가 SHUTDOWN을 보내고 QMP를 닫았지만 아직 플러시 중이라 잠금을 쥐고 있다.
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mut rx = hv.subscribe();

        let shutdown = serde_json::json!({
            "event": "SHUTDOWN",
            "data": { "guest": true, "reason": "guest-shutdown" },
            "timestamp": { "seconds": 0, "microseconds": 0 }
        });
        fake.hangup.send(Some(shutdown)).unwrap();
        let ev = tokio::time::timeout(EXIT_GRACE - Duration::from_secs(2), rx.recv())
            .await
            .expect("Stopped was not reported promptly")
            .unwrap();
        assert_eq!(
            ev,
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.handle(vm.id).is_none());
        // 잠금이 남아 있으니 파일은 지우지 않는다. 다음 launch/new()가 정리한다.
        assert!(dir.join("qemu.pid").exists());
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

        fake.hangup.send(None).unwrap();
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
    async fn bridge_nic_without_host_net_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let mut vm = spec();
        vm.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: opl_types::network::tap_name(vm.id, 0),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        let err = hv.start(&vm, None).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }

    #[tokio::test]
    async fn tap_name_of_another_vm_or_host_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let mut vm = spec();
        // create_tap이 같은 이름을 먼저 지우므로 호스트 인터페이스 이름은 받으면 안 된다.
        vm.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: "eth0".into(),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        match hv.start(&vm, None).await.unwrap_err() {
            DriverError::InvalidSpec(msg) => assert!(msg.contains("eth0"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
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
            hv.start(&vm, None).await,
            Err(DriverError::InvalidSpec(_))
        ));
    }
}
