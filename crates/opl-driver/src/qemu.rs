use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::paths::MAX_SOCKET_PATH;
use opl_qemu::qga::{self, Session};
use opl_qemu::{
    BackupSync, BlockNode, HostProfile, JobInfo, MigrationInfo, MigrationParameters,
    MigrationState, NbdBlockdev, QmpClient, QmpError, QmpEvent, RunState, RuntimePaths,
    TlsEndpoint, TransactionAction, build_cmdline, build_incoming_cmdline,
    build_replica_receiver_cmdline,
};
use opl_types::backup::{BITMAP_PREFIX, is_backup_bitmap};
use opl_types::guest::GuestInfo;
use opl_types::network::{tap_name, tap_prefix};
use opl_types::node::{FirmwareCaps, NodeHotplug};
use opl_types::numa::CpuSet;
use opl_types::replication::{MIGRATION_BITMAP_PREFIX, migration_bitmap};
use opl_types::runtime::{
    DiskKind, ImageFormat, NicBackend, RuntimeDisk, RuntimeMedium, RuntimeNic, RuntimePinning,
    VmRuntimeSpec,
};
use opl_types::vm::{CpuSpec, DiskBus, Firmware, NicModel, RunningHardware};
use rustix::process::{Flock, FlockOffsetType, FlockType, Pid, Signal, fcntl_getlk, kill_process};
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, AddOutcome, BackupTarget, CpuProbe, DiskBitmaps, DiskCounters, EventKind,
    Hypervisor, HypervisorEvent, Quiesced, RuntimeStatus, StopMode, VcpuChange,
};
use crate::migration::{
    CatchUp, CatchUpState, IncomingPorts, MIGRATION_TLS_ID, MigrateOptions, MigrateTarget,
    MirrorOptions, MirrorProgress, MirrorTarget, NBD_TLS_ID, format_duration, migration_tls_dir,
    parse_qemu_version, pick_port, tcp_uri,
};
use crate::net::{HostNet, Tap};
use crate::replication::{REPLICA_TLS_ID, ReceiverExport, ReplicateRequest};
use crate::storage::{DiskLocation, fmt_name};
use crate::tpm::{self, SwtpmConfig, vm_tpm_dir};

mod agent;

use agent::Freezer;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
/// QMP 명령 하나의 응답을 기다리는 한도. 멈춘(SIGSTOP 등) QEMU에서 호출자가 묶이지 않게 한다.
const QMP_TIMEOUT: Duration = Duration::from_secs(5);
/// CD-ROM 매체 바꾸기(QMP 명령 여럿 — 남은 노드 닫기, 노드 열기, 트레이 열기·빼기·넣기·닫기) 전체의 한도. 넘기면
/// 노드가 남을 수 있고 다음 바꾸기가 먼저 닫는다.
const MEDIUM_TIMEOUT: Duration = Duration::from_secs(30);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// SIGKILL을 보낸 뒤 프로세스가 사라지기를 기다리는 한도. 큰 VM은 메모리를 푸는 데 몇 초가 걸린다. HA 검사의
/// 끄기 요청 시간(opl-api `EVICT_TIMEOUT` 20초)보다 짧아야 한다.
const KILL_TIMEOUT: Duration = Duration::from_secs(10);
/// SHUTDOWN 없이 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 기다리는 한도.
/// 넘기면 QMP만 끊긴 것으로 본다.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// SHUTDOWN 뒤 연결이 끊겼을 때 런타임 디렉터리를 지우려고 프로세스 종료를 잠깐 기다리는 한도.
/// QEMU는 소켓을 닫고 곧 끝나지만, 플러시가 길면 디렉터리는 다음 launch/new()에 맡기고 종료를 바로 알린다.
const SHUTDOWN_EXIT_WAIT: Duration = Duration::from_secs(1);
/// 게스트 에이전트의 `guest-ping`을 기다리는 시간
const GUEST_PING_TIMEOUT: Duration = Duration::from_secs(3);
/// 게스트 에이전트 잠금을 기다리는 한도(그 뒤는 바쁨으로 본다)
const AGENT_LOCK_WAIT: Duration = Duration::from_secs(2);
/// 내부 스냅샷(저장·불러오기·삭제) 하나를 기다리는 한도. 메모리 스냅샷은 메모리 크기만큼 걸린다.
/// Task 한도(600초)보다 짧아야 Task가 먼저 끊기지 않고 이 오류(작업이 남아 있을 수 있음)를 받는다.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(540);

/// opl이 스냅샷을 위해 VM을 멈춰 두었다는 표시 파일. VM 실행 디렉터리에 `pause`가 쓰고 `resume`이
/// 지운다. opld가 멈춘 채 죽었으면 다시 붙을 때 이 표시를 보고 이어서 실행한다.
const PAUSED_MARK: &str = "paused-by-opl";

/// QMP를 오래 붙잡는 작업(메모리 스냅샷 등 — [`QemuHypervisor::busy`])이 돈다는 표시 파일(내용은 작업 이름). VM 실행
/// 디렉터리에 작업 전에 쓰고 끝나면 지운다. 그 작업 중 orbivisord가 다시 켜지면 메모리의 표시는 없지만 QEMU는 여전히
/// 답하지 않으므로, 다시 붙지 못한 VM에 이 표시가 있고(`SNAPSHOT_TIMEOUT` 안) QMP가 답하지 않으면 응답 없음이 아니라
/// 바쁨이다(계획 66 리뷰 1 #4 — 그렇지 않으면 멈추라는 요청이 `acpi_timeout` 뒤 저장 중인 VM을 강제로 끈다).
const BUSY_MARK: &str = "busy-op";
/// [`BUSY_MARK`]에 쓰는 작업 이름(표시 파일의 글을 `&'static str`로 되돌린다)
const BUSY_OPS: [&str; 3] = ["snapshot-save", "snapshot-load", "snapshot-delete"];
/// 다시 붙지 못한 VM에 [`BUSY_MARK`]가 있을 때 QMP가 답하는지 보는 한도(답하면 작업이 끝났고 표시는 남은 것이다)
const BUSY_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// 라이브 마이그레이션을 받는 QEMU라는 표시 파일(내용은 마이그레이션 Task id). `start_incoming`이
/// QEMU를 띄우기 전에 실행 디렉터리에 쓰고 `finish_incoming`이 지운다. QEMU가 끝나면 실행 디렉터리와
/// 함께 사라진다.
const INCOMING_MARK: &str = "incoming";
/// 마이그레이션 상태를 다시 묻는 간격(`MIGRATION` 이벤트를 놓쳐도 이만큼 뒤에 안다)
const MIGRATION_POLL: Duration = Duration::from_secs(1);
/// 원본이 끝난 뒤 받는 쪽이 마지막 장치 상태를 다 읽기를 기다리는 한도
const INCOMING_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
/// 받는 쪽이 받는 QEMU를 띄우기 전에 SDN NIC의 OVN 포트가 이 노드에서 켜지기(`ovn-installed`)를 기다리는 시간(계획 65 §10).
/// 넘으면 받기를 거절해 마이그레이션이 시작 전에 실패한다(VM은 원본에 그대로).
const OVN_INCOMING_WAIT: Duration = Duration::from_secs(30);
/// 켤 때는 OVN 바인딩을 기다리지 않는다. 이만큼 지나도 `ovn-installed`가 없으면 경고만 남긴다.
const OVN_START_WARN: Duration = Duration::from_secs(10);
/// 시간 제한으로 `migrate_cancel`한 뒤 마이그레이션이 끝나기를 기다리는 한도
const CANCEL_WAIT: Duration = Duration::from_secs(30);
/// 마이그레이션이 끝난 뒤 원본이 `finish-migrate`를 벗어났는지 다시 묻는 간격
const POSTMIGRATE_POLL: Duration = Duration::from_millis(50);
/// 증분 백업의 대상 노드와 그 작업 id의 접두사(`opl-backup-{i}`)
const BACKUP_JOB_PREFIX: &str = "opl-backup-";
/// 증분 백업 작업 상태를 다시 묻는 간격(`JOB_STATUS_CHANGE` 이벤트를 놓쳐도 이만큼 뒤에 안다)
const JOB_POLL: Duration = Duration::from_secs(1);
/// 핫플러그 배치의 virtio-mem 장치(`-device virtio-mem-pci,id=vmem0-dev`)
const VIRTIO_MEM_PATH: &str = "/machine/peripheral/vmem0-dev";
/// virtio-mem을 줄일 때 게스트가 내려 준 크기(`size`)를 다시 읽는 간격
const VIRTIO_MEM_POLL: Duration = Duration::from_millis(200);
/// 이 노드가 Linux인지(x86_64 vCPU 핫플러그는 Linux만 — macOS는 HVF)
const LINUX: bool = cfg!(target_os = "linux");
/// 디스크 미러의 작업 id와 그 NBD 대상 노드 이름의 접두사(`opl-mirror-{i}`, i는 디스크 순서 번호)
const MIRROR_JOB_PREFIX: &str = "opl-mirror-";
/// 받는 쪽 NBD 내보내기 id의 접두사(`opl-export-{i}`)
const EXPORT_PREFIX: &str = "opl-export-";
/// 켤 때의 하드웨어 기록을 남기는 실행 디렉터리의 파일([`Hypervisor::save_hardware`])
const HARDWARE_FILE: &str = "hardware.json";
/// 스레드 고정(계획 64)을 남기는 실행 디렉터리의 파일
const PINNING_FILE: &str = "pinning.json";
/// 복제 회차의 작업 id(`opl-repl-{i}`)와 그 NBD 대상 노드(`opl-repl-nbd-{i}`)의 접두사(i는 디스크 순서 번호)
const REPLICA_JOB_PREFIX: &str = "opl-repl-";
/// 복제 회차의 NBD 대상 노드 이름의 접두사(`opl-repl-nbd-{i}`)
const REPLICA_NBD_PREFIX: &str = "opl-repl-nbd-";
/// 플리싱 노드 이름의 접두사: 플리싱 이미지 `opl-fl-tmp-{i}`, copy-before-write 필터 `opl-fl-cbw-{i}`, 회차 시점의
/// 디스크를 읽는 snapshot-access `opl-fl-acc-{i}`(i는 디스크 순서 번호)
const FLEECE_PREFIX: &str = "opl-fl-";
/// snapshot-access 노드에 기준 비트맵을 옮겨 담는 비트맵(노드와 함께 사라진다)
const FLEECE_BITMAP: &str = "opl-fl-base";
/// copy-before-write 필터가 플리싱 이미지에 쓰기를 기다리는 한도(초). 넘기거나 쓰지 못하면 게스트 쓰기는 그대로
/// 하고 회차 시점(스냅샷)만 깨져 그 회차가 실패한다.
const CBW_TIMEOUT_SECS: u64 = 30;
/// 플리싱 이미지를 둘 VM 상태 디렉터리의 남은 공간이 플리싱할 디스크의 가상 크기 합의 이 비율(%)보다 작으면
/// 시작하지 않는다(회차 동안 게스트가 처음 덮어쓰는 블록만큼 자란다).
const FLEECE_SPACE_PERCENT: u64 = 10;
/// 보내는 동안(플리싱 이미지가 게스트가 덮어쓰는 블록만큼 자란다) 그 파일 시스템에 남겨 둘 공간: 이것과 파일 시스템
/// 크기의 [`FLEECE_RESERVE_PERCENT`]% 가운데 큰 쪽. 그보다 줄면 보내기를 멈춘다(실행 실패, 플리싱을 뗀다).
const FLEECE_RESERVE_BYTES: u64 = 1 << 30;
const FLEECE_RESERVE_PERCENT: u64 = 5;
/// 받는 QEMU의 실행 디렉터리 이름의 접두사(`{run_dir}/replica-{회차}`)
const RECEIVER_DIR_PREFIX: &str = "replica-";
/// 받는 QEMU가 연 복제본 블록 노드 이름의 접두사(`opl-repl-{i}`, i는 내보내기 순서)
const RECEIVER_NODE_PREFIX: &str = "opl-repl-";
/// 받는 QEMU의 NBD 내보내기 id의 접두사(`opl-repl-export-{i}`)
const RECEIVER_EXPORT_PREFIX: &str = "opl-repl-export-";
/// 마이그레이션 따라잡기(계획 55)의 작업 id(`opl-catchup-{i}`)와 그 NBD 대상 노드(`opl-catchup-nbd-{i}`)의 접두사(i는
/// 디스크 순서 번호)
const CATCH_UP_PREFIX: &str = "opl-catchup-";
/// 마이그레이션 따라잡기의 NBD 대상 노드 이름의 접두사(`opl-catchup-nbd-{i}`)
const CATCH_UP_NBD_PREFIX: &str = "opl-catchup-nbd-";
/// 받는 QEMU의 블록 노드를 닫을 때 내보내기가 아직 붙들고 있으면 다시 해 보는 한도
const RECEIVER_CLOSE_WAIT: Duration = Duration::from_secs(5);

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
    /// systemd가 있는 Linux에서만 켠다. swtpm도 `opl-tpm-<id>.scope`로 감싼다.
    pub systemd_scope: bool,
    /// TPM VM의 swtpm·swtpm_setup(계획 59)
    pub swtpm: SwtpmConfig,
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
    /// `pause`로 멈추고 아직 `resume`을 부르지 않은 VM. 다시 붙어도 이어서 실행하지 않는다.
    pausing: Mutex<HashSet<Uuid>>,
    /// 받을 포트를 고르고 QEMU가 그 포트를 열 때까지 쥔다. 동시에 받는 두 VM이 같은 포트를 고르지 않게 한다.
    incoming_ports: tokio::sync::Mutex<()>,
    /// QMP로 붙은 복제 받는 QEMU(회차별)
    receivers: Mutex<HashMap<Uuid, Arc<Receiver>>>,
    /// QMP를 오래 붙잡는 작업이 도는 VM과 그 작업 이름([`RuntimeStatus::Busy`], [`Self::busy`]).
    busy: Mutex<HashMap<Uuid, &'static str>>,
}

/// [`QemuHypervisor::busy`]가 돌려주는 표시. 버리면(작업이 끝나거나 실패하거나 취소되면) 메모리의 표시와 표시 파일
/// ([`BUSY_MARK`])을 지운다.
struct BusyGuard<'a> {
    map: &'a Mutex<HashMap<Uuid, &'static str>>,
    id: Uuid,
    mark: Option<PathBuf>,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.map.lock().unwrap().remove(&self.id);
        if let Some(mark) = &self.mark
            && let Err(e) = remove_if_exists(mark)
        {
            tracing::warn!(id = %self.id, error = %e, "cannot remove busy mark");
        }
    }
}

/// 실행 디렉터리 `dir`의 [`BUSY_MARK`]가 `SNAPSHOT_TIMEOUT` 안에 쓰였으면 그 작업 이름
fn busy_mark(dir: &Path) -> Option<&'static str> {
    let path = dir.join(BUSY_MARK);
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    // 시계가 뒤로 갔으면(mtime이 미래) 새 표시로 본다.
    if modified.elapsed().is_ok_and(|age| age >= SNAPSHOT_TIMEOUT) {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    BUSY_OPS.into_iter().find(|op| *op == text.trim())
}

/// QMP 소켓이 곧 답하는지(인사와 `qmp_capabilities`). 메모리 스냅샷 작업 중의 QEMU는 BQL을 쥐어 답하지 않는다.
async fn qmp_answers(socket: &Path) -> bool {
    matches!(
        tokio::time::timeout(BUSY_PROBE_TIMEOUT, QmpClient::connect(socket)).await,
        Ok(Ok(_))
    )
}

/// 복제 회차 하나를 받는 QEMU(`-machine none`)
struct Receiver {
    qmp: QmpClient,
    pid: u32,
    dir: PathBuf,
}

impl Receiver {
    fn pidfile(dir: &Path) -> PathBuf {
        dir.join("qemu.pid")
    }

    fn qmp_socket(dir: &Path) -> PathBuf {
        dir.join("qmp.sock")
    }
}

/// QEMU를 띄우는 방식
#[derive(Clone, Copy)]
enum Launch<'a> {
    /// 띄우고(스냅샷 태그가 있으면 불러온 뒤) 이어서 실행한다.
    Start { restore: Option<&'a str> },
    /// `-incoming defer`로 띄우고 받는 중 표시(마이그레이션 Task)를 남긴다. 멈춘 채 둔다.
    Incoming { task: Uuid },
}

struct VmHandle {
    qmp: QmpClient,
    pid: u32,
    paths: RuntimePaths,
    monitor: JoinHandle<()>,
    /// 게스트 에이전트 채널은 한 번에 한 연결뿐이라 ping·정보·종료·얼리기·비밀번호가 모두 이것을 거친다(계획 59).
    agent: Arc<tokio::sync::Mutex<()>>,
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
            pausing: Mutex::default(),
            incoming_ports: tokio::sync::Mutex::new(()),
            receivers: Mutex::default(),
            busy: Mutex::default(),
        };
        hv.remove_dead_receiver_dirs();
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
                    // 런타임 디렉터리가 정리할 것이 남았다는 유일한 기록이므로 tap과 QEMU 없이 남은 swtpm부터
                    // 지운다. 지우지 못하면 디렉터리를 남겨 다음 new()가 다시 시도한다.
                    if tpm::terminate(&paths).await
                        && delete_taps(hv.config.net.as_deref(), id).await
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
                Ok(Ok(())) => {
                    tracing::info!(%id, "reattached to running VM");
                    // 앞선 opld가 얼린 채 죽었으면 게스트 파일 시스템을 녹인다.
                    agent::thaw_left(id, &hv.paths(id)?).await;
                    // 앞선 opld가 복제 회차 도중에 죽었으면 플리싱이 남아 디스크 장치를 쥐고 있다.
                    if let Err(e) = hv.clear_left_fleecing(id).await {
                        tracing::warn!(%id, error = %e, "cannot remove the fleecing left by an earlier run");
                    }
                }
                Ok(Err(e)) => tracing::error!(%id, error = %e, "could not reattach to running VM"),
                Err(_) => tracing::error!(%id, "timed out reattaching to running VM"),
            }
        }
        Ok(hv)
    }

    /// 실행 중인 VM에 HMP 명령 한 줄을 보내고 그 출력을 돌려준다(QMP `human-monitor-command`). 시험과
    /// 진단용이다. 예: 게스트 OS 없이 디스크에 쓰는 `qemu-io -d /machine/peripheral/disk0-dev/virtio-backend
    /// "write -P 0x44 0 1M"`
    pub async fn human_monitor_command(&self, id: Uuid, line: &str) -> Result<String, DriverError> {
        let handle = self.running_handle(id)?;
        let args = serde_json::json!({ "command-line": line });
        let hmp = handle.qmp.execute("human-monitor-command", Some(args));
        let out = flatten(
            tokio::time::timeout(QMP_TIMEOUT, hmp).await,
            "QMP human-monitor-command",
        )?;
        Ok(out.as_str().unwrap_or_default().to_string())
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
        // 게스트 에이전트 채널을 단 VM(소켓이 있다)만 게스트 이벤트(재부팅·일시 정지)를 알린다.
        let guest_events = paths.qga_socket.exists();
        // 잠금을 쥔 채로 모니터를 띄우고 등록한다. 모니터가 곧바로 연결 끊김을 보더라도
        // 등록이 끝난 뒤에야 해제할 수 있으므로 죽은 핸들이 남지 않는다.
        // 같은 이유로 Started도 이 인스턴스의 Stopped/Crashed보다 항상 먼저 나간다.
        let mut vms = self.vms.lock().unwrap();
        let monitor = tokio::spawn(monitor(
            id,
            rx,
            guest_events,
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
                agent: Arc::new(tokio::sync::Mutex::new(())),
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
        how: Launch<'_>,
    ) -> Result<(), DriverError> {
        std::fs::create_dir_all(&paths.dir)?;
        std::fs::create_dir_all(self.state_dir(vm.id))?;
        // 앞선 QEMU의 복제 회차가 남긴 플리싱 이미지(그 QEMU와 함께 쓸모가 없어졌다)
        remove_fleece_files(&self.state_dir(vm.id))?;
        let incoming = matches!(how, Launch::Incoming { .. });
        if let Some(t) = vm.tpm {
            // 펌웨어 초기화 회차가 다르면 UEFI 변수도 지우므로 변수를 복사하기 전에 맞춘다.
            let state = &self.config.state_dir;
            tpm::prepare_state(&self.config.swtpm, state, vm.id, t, vm.keep_tpm, incoming).await?;
        } else if vm.firmware == Firmware::Uefi {
            tpm::prepare_vars(
                &self.config.state_dir,
                vm.id,
                vm.firmware_epoch,
                vm.keep_tpm,
            )?;
        }
        if vm.firmware == Firmware::Uefi && !paths.uefi_vars.exists() {
            // Secure Boot VM은 MS 키를 등록한 변수 원본에서 만든다.
            let template = match (&self.config.host.secure_boot, vm.secure_boot) {
                (Some(pair), true) => &pair.vars_template,
                (None, true) => {
                    return Err(DriverError::InvalidSpec(
                        opl_qemu::CmdlineError::NoSecureBootFirmware.to_string(),
                    ));
                }
                (_, false) => &self.config.host.uefi_vars_template,
            };
            std::fs::copy(template, &paths.uefi_vars)?;
        }
        // cloud-init 씨앗은 켤 때마다 이 노드에서 런타임 정의로 다시 만든다(어느 노드에서 켜든 같은 바이트).
        write_seed(vm, &paths.cidata)?;
        // 호출자가 pidfile 잠금이 풀린 것(= QEMU 없음)을 확인했으므로 남은 소켓은 지워도 된다.
        // 이전 인스턴스가 남긴 pidfile은 새 QEMU가 잠그고 다시 쓴다.
        for sock in [&paths.qmp_socket, &paths.vnc_socket, &paths.qga_socket] {
            remove_if_exists(sock)?;
        }
        remove_if_exists(&paths.dir.join(agent::FSFROZEN_MARK))?;

        let args = match how {
            Launch::Start { .. } => build_cmdline(vm, &self.config.host, paths),
            Launch::Incoming { .. } => build_incoming_cmdline(vm, &self.config.host, paths),
        };
        let mut args = args.map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        args.push("-daemonize".into());
        if let Launch::Incoming { task } = how {
            // 띄우는 도중에 opld가 죽어도 남은 QEMU가 받는 쪽임을 알 수 있게 먼저 쓴다.
            std::fs::write(paths.dir.join(INCOMING_MARK), task.to_string())?;
        }
        let taps = self.create_taps(vm).await?;
        self.wait_ovn_ports(vm.id, &taps, incoming).await?;
        // 계획 64: QEMU·swtpm의 모든 스레드(나중 일꾼·vhost 포함)가 물려받을 친화. 고정이 있으면 에뮬레이터 집합이고, 없어도
        // opld가 예약 CPU에 묶여 있으면 온라인 CPU 전체로 다시 건다(설정을 지운 직후 등 — 리뷰 1).
        let child_cpus = vm
            .pinning
            .as_ref()
            .map(|p| p.emulator.clone())
            .or_else(crate::affinity::unpinned_child);
        if vm.tpm.is_some() {
            // QEMU가 띄울 때 제어 채널에 연결하므로 먼저 띄운다. QEMU를 띄우지 못하면 kill_and_cleanup이 끝낸다.
            let dir = vm_tpm_dir(&self.config.state_dir, vm.id);
            tpm::start(
                &self.config.swtpm,
                self.config.systemd_scope,
                vm.id,
                paths,
                &dir,
                child_cpus.as_ref(),
            )
            .await?;
        }
        let mut cmd = if self.config.systemd_scope {
            clear_stale_scope(&format!("opl-vm-{}.scope", vm.id)).await;
            let mut cmd = tokio::process::Command::new("systemd-run");
            cmd.args(vm_scope_args(vm.id, vm.pinning.as_ref()))
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
        if let Some(cpus) = &child_cpus {
            pin_child(&mut cmd, cpus);
        }
        if cfg!(target_os = "macos") {
            // macOS에서는 -daemonize의 fork 뒤 자식이 Objective-C 런타임을 초기화하다
            // "fork() was called ... Crashing instead"로 죽는다. 디스플레이/VNC 장치가 있으면
            // 우연히 피하지만, 이 변수를 켜야 장치 구성과 무관하게 안정적으로 뜬다.
            cmd.env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES");
        }
        // -daemonize: 초기화가 끝나면 부모 프로세스가 종료 코드로 성공 여부를 알려 주고, 자식은 세션에서 분리된다.
        // 미리 잡는 메모리(계획 64)는 `-daemonize`의 부모가 끝나기 전에 잡는다.
        let output = tokio::time::timeout(start_timeout(vm), cmd.output())
            .await
            .map_err(|_| DriverError::Timeout("QEMU to start"))??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if image_locked(&stderr) {
                return Err(DriverError::ImageLocked(stderr));
            }
            return Err(DriverError::StartFailed(stderr));
        }
        tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths.clone(), true))
            .await
            .map_err(|_| DriverError::Timeout("QMP of new QEMU"))??;
        let Some(handle) = self.handle(vm.id) else {
            // 그새 끝났다. 모니터가 종료 이벤트를 보냈다.
            return Ok(());
        };
        // vCPU 스레드는 `-S`로 멈춘 동안에도 있다: `cont`(받는 쪽은 마이그레이션) 전에 고정하고 확인한다.
        if let Some(pin) = &vm.pinning {
            let pinned = match pin_threads(&handle, pin).await {
                Ok(_) => write_pinning(&paths.dir, pin),
                Err(e) => Err(e),
            };
            if let Err(e) = pinned {
                drop(handle);
                if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                    tracing::error!(id = %vm.id, error = %stop, "failed to stop QEMU after pinning failed");
                }
                return Err(e);
            }
        }
        let restore = match how {
            Launch::Start { restore } => restore,
            // 받기를 마칠 때까지 멈춰 둔다.
            Launch::Incoming { .. } => return Ok(()),
        };
        remove_inconsistent_bitmaps(&handle.qmp, vm.id).await;
        if let Some(tag) = restore {
            // -S로 멈춘 채 불러온 뒤 cont로 이어서 실행한다. 메모리 상태는 첫 디스크(disk0)에 있다.
            let devices: Vec<String> = (0..vm.disks.len()).map(|i| format!("disk{i}")).collect();
            let busy = self.busy(vm.id, "snapshot-load");
            let load = job(
                handle
                    .qmp
                    .snapshot_load(tag, "disk0", &devices, SNAPSHOT_TIMEOUT),
                "QMP snapshot-load",
            )
            .await;
            drop(busy);
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

    /// 브리지 NIC마다 tap을 만들어 브리지에 붙인다(SDN NIC는 OVS `br-int`에). 하나라도 실패하면 만든 것을 지운다.
    /// 만든 tap을 돌려준다.
    async fn create_taps(&self, vm: &VmRuntimeSpec) -> Result<Vec<Tap>, DriverError> {
        let taps: Vec<Tap> = vm
            .nics
            .iter()
            .filter_map(|nic| Tap::from_nic(vm.id, nic))
            .collect();
        if taps.is_empty() {
            return Ok(taps);
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
        Ok(taps)
    }

    /// SDN tap의 OVN 포트가 이 노드에서 켜지기를(`ovn-installed`) 기다린다(계획 65 §10). 마이그레이션을 받을 때(`incoming`)는
    /// 받는 QEMU를 띄우기 전에 [`OVN_INCOMING_WAIT`]까지 기다리고, 오지 않으면 실패한다(호출자가 tap을 지운다) — 원본 Task가
    /// `requested-chassis`를 "원본,대상"으로 바꿨으므로 이 노드도 포트를 바인딩하고, `activation-strategy=rarp`라 QEMU가
    /// 끝에 보내는 RARP 전까지 막혀 있다. 켤 때는 기다리지 않는다: OvnSync가 `requested-chassis`를 이 노드로 바꾸기 전에는
    /// 바인딩되지 않을 수 있고 게스트 DHCP가 다시 묻는다. [`OVN_START_WARN`] 뒤에도 오지 않으면 경고만 남긴다.
    async fn wait_ovn_ports(
        &self,
        id: Uuid,
        taps: &[Tap],
        incoming: bool,
    ) -> Result<(), DriverError> {
        let ovn: Vec<String> = taps
            .iter()
            .filter(|t| t.ovn.is_some())
            .map(|t| t.ifname.clone())
            .collect();
        let Some(net) = self.config.net.clone().filter(|_| !ovn.is_empty()) else {
            return Ok(());
        };
        if incoming {
            for ifname in &ovn {
                if let Err(e) = net.wait_ovn_installed(ifname, OVN_INCOMING_WAIT).await {
                    return Err(DriverError::Migration(format!(
                        "OVN did not activate the SDN port {ifname} on this node in {}s: {e}",
                        OVN_INCOMING_WAIT.as_secs()
                    )));
                }
            }
            return Ok(());
        }
        tokio::spawn(async move {
            for ifname in &ovn {
                if let Err(e) = net.wait_ovn_installed(ifname, OVN_START_WARN).await {
                    tracing::warn!(%id, tap = %ifname, error = %e, "the SDN port is not bound on this node yet; the guest gets its network once OVN binds it");
                }
            }
        });
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
        // QEMU 없이 남은 swtpm(QEMU를 띄우지 못함)을 끝낸다. 끝내지 못하면 그 pid 파일을 지우지 않는다.
        if !tpm::terminate(paths).await {
            return false;
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

    /// 플리싱을 붙이는 동안 게스트를 멈출 VM `id`([`start_fleecing`])
    fn fleece_halt(&self, id: Uuid, handle: &VmHandle) -> FleeceHalt<'_> {
        FleeceHalt {
            id,
            mark: handle.paths.dir.join(PAUSED_MARK),
            pausing: &self.pausing,
        }
    }

    /// 다시 붙은 VM에 앞선 opld가 남긴 플리싱(`opl-fl-*` 노드)이 있으면 그것을 읽던 복제 작업을 치우고 뗀 뒤, 남은
    /// 플리싱 이미지 파일을 지운다.
    async fn clear_left_fleecing(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let nodes = qmp_call(qmp.query_block_nodes(), "QMP query-named-block-nodes").await?;
        if nodes.iter().any(|n| n.node_name.starts_with(FLEECE_PREFIX)) {
            clear_jobs(qmp, &[REPLICA_JOB_PREFIX, CATCH_UP_PREFIX]).await?;
        }
        clear_fleecing(qmp, &self.state_dir(id)).await
    }

    /// opld가 `-daemonize`와 `cont` 사이에 죽으면 VM이 `-S`(prelaunch) 상태로 남는다. 스냅샷을 위해
    /// 멈춘 채([`PAUSED_MARK`]) opld가 죽었거나 이어서 실행하지 못한 VM도 멈춘 채 남는다. 둘 다 이어서
    /// 실행한다. 지금 `pause`로 멈춰 둔 VM과 QEMU가 스스로 멈춘 VM(표시 없음)은 그대로 둔다.
    async fn continue_if_left_stopped(&self, id: Uuid) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(());
        };
        let mark = handle.paths.dir.join(PAUSED_MARK);
        let ours = !self.pausing.lock().unwrap().contains(&id) && mark.exists();
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        let state = flatten(status, "QMP query-status")?;
        if state == RunState::Prelaunch || (ours && state == RunState::Paused) {
            tracing::info!(%id, ?state, "continuing VM left stopped");
            let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
            flatten(cont, "QMP cont")?;
        }
        if ours {
            remove_if_exists(&mark)?;
        }
        Ok(())
    }

    /// 마이그레이션 TLS 자격 증명(`{state_dir}/migration-tls`)과 능력·매개변수를 정한다. 자격 증명
    /// 객체는 파일을 만들 때 읽으므로, 인증서가 바뀌었을 수 있어 매번 지우고 다시 만든다.
    /// `dirty_bitmaps`면 dirty bitmap도 넘긴다. `pause_before_switchover`면 스위치오버 직전에
    /// `pre-switchover`로 기다린다(능력은 QEMU에 남으므로 늘 정한다).
    async fn setup_migration(
        &self,
        qmp: &QmpClient,
        endpoint: TlsEndpoint,
        tls_hostname: Option<&str>,
        bandwidth: Option<u64>,
        dirty_bitmaps: bool,
        pause_before_switchover: bool,
    ) -> Result<(), DriverError> {
        // 처음이면 없어서 실패한다.
        let _ = tokio::time::timeout(QMP_TIMEOUT, qmp.object_del(MIGRATION_TLS_ID)).await;
        let dir = migration_tls_dir(&self.config.state_dir);
        let add = qmp.add_tls_creds(MIGRATION_TLS_ID, &dir, endpoint);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, add).await,
            "QMP object-add",
        )?;
        // dirty-bitmaps: 증분 백업의 비트맵을 넘긴다. 양쪽 디스크 노드 이름이 같다(`disk{i}`).
        let caps = [
            ("events", true),
            ("auto-converge", true),
            ("dirty-bitmaps", dirty_bitmaps),
            ("pause-before-switchover", pause_before_switchover),
        ];
        let caps = qmp.migrate_set_capabilities(&caps);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, caps).await,
            "QMP migrate-set-capabilities",
        )?;
        let params = MigrationParameters {
            tls_creds: Some(MIGRATION_TLS_ID.into()),
            tls_hostname: tls_hostname.map(str::to_string),
            max_bandwidth: bandwidth,
        };
        let set = qmp.migrate_set_parameters(&params);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, set).await,
            "QMP migrate-set-parameters",
        )
    }

    /// 띄운 받는 QEMU에 마이그레이션 설정을 하고, `exports`가 있으면 빈 포트에서 그 디스크들을 TLS
    /// NBD로 내보낸 뒤, 다른 빈 포트에서 받기 시작한다.
    async fn listen_incoming(
        &self,
        id: Uuid,
        host: &str,
        ports: RangeInclusive<u16>,
        exports: &[usize],
    ) -> Result<IncomingPorts, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        // 받는 쪽은 보내는 쪽이 넘긴 비트맵만 받는다.
        self.setup_migration(qmp, TlsEndpoint::Server, None, None, true, false)
            .await?;
        let no_port = || {
            DriverError::Migration(format!(
                "no free migration port in {}-{} on {host}",
                ports.start(),
                ports.end()
            ))
        };
        let _ports = self.incoming_ports.lock().await;
        let mut nbd = None;
        if !exports.is_empty() {
            let dir = migration_tls_dir(&self.config.state_dir);
            let add = qmp.add_tls_creds(NBD_TLS_ID, &dir, TlsEndpoint::Server);
            flatten(
                tokio::time::timeout(QMP_TIMEOUT, add).await,
                "QMP object-add",
            )?;
            let port = pick_port(host, ports.clone(), &[]).ok_or_else(no_port)?;
            let start = qmp.nbd_server_start(host, port, NBD_TLS_ID);
            flatten(
                tokio::time::timeout(QMP_TIMEOUT, start).await,
                "QMP nbd-server-start",
            )?;
            // `-incoming defer`로 연 이미지는 비활성이지만 내보내기가 그 노드를 활성화해 쓰기를 받는다.
            for i in exports {
                let (export, node) = (format!("{EXPORT_PREFIX}{i}"), format!("disk{i}"));
                let add = qmp.block_export_add_nbd(&export, &node, &node);
                flatten(
                    tokio::time::timeout(QMP_TIMEOUT, add).await,
                    "QMP block-export-add",
                )?;
            }
            nbd = Some(port);
        }
        // NBD 서버가 연 포트는 열 수 없어 건너뛴다(가짜 QMP처럼 열지 않았어도 건너뛴다).
        let except: Vec<u16> = nbd.into_iter().collect();
        let port = pick_port(host, ports.clone(), &except).ok_or_else(no_port)?;
        let uri = tcp_uri(host, port);
        let listen = qmp.migrate_incoming(&uri);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, listen).await,
            "QMP migrate-incoming",
        )?;
        Ok(IncomingPorts {
            migration: port,
            nbd,
        })
    }

    /// 회차 `run`을 받는 QEMU의 실행 디렉터리 `{run_dir}/replica-{run}`
    fn receiver_dir(&self, run: Uuid) -> PathBuf {
        self.config
            .run_dir
            .join(format!("{RECEIVER_DIR_PREFIX}{run}"))
    }

    /// 실행 디렉터리가 있는 받는 QEMU의 회차와 그 생존 여부
    fn receiver_dirs(&self) -> std::io::Result<Vec<(Uuid, Liveness)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.config.run_dir)? {
            let entry = entry?;
            let Some(run) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.strip_prefix(RECEIVER_DIR_PREFIX))
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            out.push((run, liveness(&Receiver::pidfile(&entry.path()))));
        }
        Ok(out)
    }

    /// 끝난 받는 QEMU가 남긴 실행 디렉터리를 지운다(opld가 받는 도중 죽었다).
    fn remove_dead_receiver_dirs(&self) {
        let dirs = match self.receiver_dirs() {
            Ok(dirs) => dirs,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list replica receiver dirs");
                return;
            }
        };
        for (run, live) in dirs {
            if live == Liveness::Dead
                && let Err(e) = std::fs::remove_dir_all(self.receiver_dir(run))
            {
                tracing::warn!(%run, error = %e, "cannot remove replica receiver dir");
            }
        }
    }

    /// 회차 `run`을 받는 QEMU. 붙어 있지 않으면(opld가 다시 켜짐) 살아 있는 QEMU의 QMP에 다시 붙는다.
    async fn receiver(&self, run: Uuid) -> Result<Arc<Receiver>, DriverError> {
        if let Some(r) = self.receivers.lock().unwrap().get(&run).cloned() {
            return Ok(r);
        }
        let dir = self.receiver_dir(run);
        if liveness(&Receiver::pidfile(&dir)) == Liveness::Dead {
            return Err(DriverError::Replication(format!(
                "no replica receiver for run {run} on this node"
            )));
        }
        let socket = Receiver::qmp_socket(&dir);
        let qmp = tokio::time::timeout(ATTACH_TIMEOUT, QmpClient::connect(&socket))
            .await
            .map_err(|_| DriverError::Timeout("QMP of the replica receiver"))??;
        let pid = read_pid(&Receiver::pidfile(&dir))?;
        let r = Arc::new(Receiver { qmp, pid, dir });
        self.receivers.lock().unwrap().insert(run, r.clone());
        Ok(r)
    }

    /// 받는 QEMU를 띄우고 QMP에 붙어 등록한 뒤 복제본을 내보낸다. 실패하면 호출자가 끈다.
    async fn launch_receiver(
        &self,
        run: Uuid,
        dir: &Path,
        exports: &[ReceiverExport],
        host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        let (qmp_socket, pidfile) = (Receiver::qmp_socket(dir), Receiver::pidfile(dir));
        let mut cmd = tokio::process::Command::new(&self.config.host.qemu_binary);
        cmd.args(build_replica_receiver_cmdline(&qmp_socket, &pidfile))
            .arg("-daemonize")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if cfg!(target_os = "macos") {
            // VM을 띄울 때와 같은 까닭(-daemonize의 fork 뒤 Objective-C 런타임)
            cmd.env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES");
        }
        let output = tokio::time::timeout(START_TIMEOUT, cmd.output())
            .await
            .map_err(|_| DriverError::Timeout("replica receiver QEMU to start"))??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DriverError::StartFailed(stderr));
        }
        let connect = QmpClient::connect(&qmp_socket);
        let qmp = tokio::time::timeout(ATTACH_TIMEOUT, connect)
            .await
            .map_err(|_| DriverError::Timeout("QMP of the replica receiver"))??;
        let pid = read_pid(&pidfile)?;
        let receiver = Arc::new(Receiver {
            qmp,
            pid,
            dir: dir.to_path_buf(),
        });
        self.receivers.lock().unwrap().insert(run, receiver.clone());
        let tls = migration_tls_dir(&self.config.state_dir);
        let _ports = self.incoming_ports.lock().await;
        serve_replicas(&receiver.qmp, &tls, exports, host, ports).await
    }

    /// VM `id`에서 QMP를 오래 붙잡는 작업 `op`을 시작한다. QEMU는 메모리 스냅샷 저장·불러오기와 스냅샷
    /// 지우기 작업을 BQL을 쥔 채 주 루프에서 하므로 끝날 때까지 QMP에 아무것도 답하지 않는다. 표시가 있는
    /// 동안 상태 조회는 QMP를 묻지 않고 [`RuntimeStatus::Busy`]를 알린다(응답 없음으로 보이지 않게,
    /// 답하지 않을 요청이 쌓이지 않게). 작업의 제한 시간(`SNAPSHOT_TIMEOUT`)을 넘겨 돌아오면 표시가
    /// 사라지므로 그 뒤로 QEMU가 여전히 답하지 않으면 다시 응답 없음이다.
    ///
    /// 표시는 실행 디렉터리의 [`BUSY_MARK`]에도 남겨 orbivisord가 작업 중 다시 켜져도 바쁨을 안다.
    fn busy(&self, id: Uuid, op: &'static str) -> BusyGuard<'_> {
        debug_assert!(BUSY_OPS.contains(&op), "{op}");
        self.busy.lock().unwrap().insert(id, op);
        let mark = self.paths(id).ok().map(|p| p.dir.join(BUSY_MARK));
        if let Some(path) = &mark
            && let Err(e) = std::fs::write(path, op)
        {
            tracing::warn!(%id, error = %e, "cannot write busy mark");
        }
        BusyGuard {
            map: &self.busy,
            id,
            mark,
        }
    }

    fn busy_with(&self, id: Uuid) -> Option<&'static str> {
        self.busy.lock().unwrap().get(&id).copied()
    }

    async fn query_state(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let Some(handle) = self.handle(id) else {
            // 살아 있지만 QMP에 붙지 못한 VM도 드러내야 reconciler가 놓치지 않는다.
            let paths = self.paths(id)?;
            return match liveness(&paths.pidfile) {
                Liveness::Dead => Ok(None),
                Liveness::Alive(pid) => {
                    // orbivisord가 메모리 스냅샷 작업 중 다시 켜졌다: QEMU가 아직 답하지 않으면 바쁨이다. 답하면 작업은
                    // 끝났고 표시는 옛 프로세스가 남긴 것이다(지우고 보통처럼 다시 붙게 둔다).
                    let status = match busy_mark(&paths.dir) {
                        Some(op) if !qmp_answers(&paths.qmp_socket).await => {
                            RuntimeStatus::Busy(op)
                        }
                        mark => {
                            if mark.is_some()
                                && let Err(e) = remove_if_exists(&paths.dir.join(BUSY_MARK))
                            {
                                tracing::warn!(%id, error = %e, "cannot remove busy mark");
                            }
                            RuntimeStatus::Other("unresponsive".into())
                        }
                    };
                    Ok(Some(ActualState { id, pid, status }))
                }
                Liveness::Unknown => Err(DriverError::Io(std::io::Error::other(format!(
                    "cannot tell whether QEMU of VM {id} is running"
                )))),
            };
        };
        // QMP를 붙잡는 작업 중이다. 물어도 작업이 끝날 때까지 답이 없다. QEMU가 죽으면 모니터가 등록을 푼다.
        if let Some(op) = self.busy_with(id) {
            return Ok(Some(ActualState {
                id,
                pid: handle.pid,
                status: RuntimeStatus::Busy(op),
            }));
        }
        let status = match tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await {
            // 묻는 사이에 작업이 시작됐거나(리뷰 1 M-7) 옛 프로세스의 작업이 아직 돈다(#4).
            Err(_) => {
                return match self.busy_with(id).or_else(|| busy_mark(&handle.paths.dir)) {
                    Some(op) => Ok(Some(ActualState {
                        id,
                        pid: handle.pid,
                        status: RuntimeStatus::Busy(op),
                    })),
                    None => Err(DriverError::Timeout("QMP query-status")),
                };
            }
            Ok(Ok(RunState::Running)) => RuntimeStatus::Running,
            Ok(Ok(RunState::Prelaunch | RunState::Paused)) => RuntimeStatus::Paused,
            Ok(Ok(RunState::Shutdown)) => RuntimeStatus::Other("shutdown".into()),
            Ok(Ok(RunState::InMigrate)) => RuntimeStatus::Other("inmigrate".into()),
            Ok(Ok(RunState::PostMigrate)) => RuntimeStatus::Other("postmigrate".into()),
            Ok(Ok(RunState::Other(s))) => RuntimeStatus::Other(s),
            // 조회하는 사이에 프로세스가 끝났다.
            Ok(Err(QmpError::Disconnected)) => return Ok(None),
            Ok(Err(e)) => return Err(e.into()),
        };
        // QEMU가 답했으니 메모리에 표시가 없는 바쁨 표시 파일은 옛 프로세스가 남긴 것이다.
        if self.busy_with(id).is_none()
            && let Err(e) = remove_if_exists(&handle.paths.dir.join(BUSY_MARK))
        {
            tracing::warn!(%id, error = %e, "cannot remove busy mark");
        }
        // 실행 중인데 지금 멈춰 두지 않았으면 표시는 남은 것이다(이어서 실행했지만 지우지 못했거나
        // 누가 `cont`함). 그대로 두면 나중에 다른 까닭으로 멈춘 VM을 다시 붙을 때 이어서 실행한다.
        if status == RuntimeStatus::Running && !self.pausing.lock().unwrap().contains(&id) {
            let mark = handle.paths.dir.join(PAUSED_MARK);
            if let Err(e) = remove_if_exists(&mark) {
                tracing::warn!(%id, error = %e, "cannot remove paused mark");
            }
        }
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
            return self.continue_if_left_stopped(vm.id).await;
        }
        let paths = self.paths(vm.id)?;
        if liveness(&paths.pidfile) != Liveness::Dead {
            // 살아 있지만 등록되지 않은 VM(재시작 때 붙지 못함, QMP 연결 유실). 새로 띄우면
            // pidfile 잠금에 걸려 실패하고 살아 있는 VM의 파일을 지우게 되므로 다시 붙기만 한다.
            // 생존을 판정할 수 없을 때도 같은 이유로 새로 띄우지 않는다.
            match tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths, false)).await {
                Ok(Ok(())) => {
                    tracing::info!(id = %vm.id, "reattached to running VM");
                    self.continue_if_left_stopped(vm.id).await?;
                    agent::thaw_left(vm.id, &self.paths(vm.id)?).await;
                    return Ok(());
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
        if let Err(e) = self.launch(vm, &paths, Launch::Start { restore }).await {
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
                StopMode::Acpi | StopMode::Agent => {
                    Err(DriverError::Timeout("reattach to running QEMU"))
                }
                StopMode::Force | StopMode::Kill if self.kill_and_cleanup(id, &paths).await => {
                    Ok(())
                }
                StopMode::Force | StopMode::Kill => Err(DriverError::Timeout("QEMU to exit")),
            };
        };
        match mode {
            StopMode::Acpi => {
                let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.system_powerdown()).await;
                flatten(r, "QMP system_powerdown")?;
            }
            StopMode::Agent => {
                if let Err(e) = agent_shutdown(&handle, id).await {
                    tracing::info!(%id, error = %e, "guest agent shutdown failed; sending ACPI power button");
                    let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.system_powerdown()).await;
                    flatten(r, "QMP system_powerdown")?;
                }
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
            StopMode::Kill => {
                let pid = handle.pid;
                let pidfile = handle.paths.pidfile.clone();
                drop(handle);
                tracing::warn!(%id, pid, "killing QEMU without QMP");
                kill_if_owner(&pidfile, pid);
                let gone = || self.handle(id).is_none() && liveness(&pidfile) == Liveness::Dead;
                if !wait_until(KILL_TIMEOUT, gone).await {
                    return Err(DriverError::Timeout("QEMU to exit"));
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

    async fn snapshot_save(
        &self,
        id: Uuid,
        tag: &str,
        memory: bool,
        quiesce: bool,
    ) -> Result<Quiesced, DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        let Some(vmstate) = devices.first() else {
            return Err(DriverError::InvalidSpec(format!("VM {id} has no disks")));
        };
        if memory {
            // 메모리 스냅샷은 그 자체로 게스트의 한 시점이라 얼리지 않는다.
            let save = handle
                .qmp
                .snapshot_save(tag, vmstate, &devices, SNAPSHOT_TIMEOUT);
            let _busy = self.busy(id, "snapshot-save");
            job(save, "QMP snapshot-save").await?;
            Ok(Quiesced::No)
        } else {
            let mut freezer = Freezer::new(quiesce, &handle.paths, &handle.agent);
            freezer.freeze(id).await;
            let r =
                tokio::time::timeout(SNAPSHOT_TIMEOUT, handle.qmp.snapshot_disks(tag, &devices))
                    .await;
            freezer.thaw(id).await;
            flatten(r, "QMP transaction")?;
            Ok(freezer.outcome())
        }
    }

    async fn snapshot_save_with_bitmap(
        &self,
        id: Uuid,
        tag: &str,
        bitmap: &str,
        quiesce: bool,
    ) -> Result<Quiesced, DriverError> {
        let handle = self.running_handle(id)?;
        let disks = query_disks(&handle.qmp).await?;
        let actions: Vec<TransactionAction> = disks
            .iter()
            .flat_map(|disk| {
                [
                    TransactionAction::SnapshotInternal {
                        device: disk.node_name.clone(),
                        name: tag.to_string(),
                    },
                    bitmap_add(disk, bitmap),
                ]
            })
            .collect();
        let mut freezer = Freezer::new(quiesce, &handle.paths, &handle.agent);
        freezer.freeze(id).await;
        let r = tokio::time::timeout(SNAPSHOT_TIMEOUT, handle.qmp.transaction(&actions)).await;
        freezer.thaw(id).await;
        flatten(r, "QMP transaction")?;
        Ok(freezer.outcome())
    }

    async fn backup_bitmaps(&self, id: Uuid) -> Result<Vec<DiskBitmaps>, DriverError> {
        let handle = self.running_handle(id)?;
        let disks = query_disks(&handle.qmp).await?;
        Ok(disks
            .into_iter()
            .map(|disk| DiskBitmaps {
                node: disk.node_name,
                bitmaps: disk.bitmaps,
            })
            .collect())
    }

    async fn add_bitmaps(&self, id: Uuid, name: &str) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let disks = query_disks(&handle.qmp).await?;
        let actions: Vec<TransactionAction> =
            disks.iter().map(|disk| bitmap_add(disk, name)).collect();
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.transaction(&actions)).await;
        flatten(r, "QMP transaction")
    }

    async fn remove_bitmaps(&self, id: Uuid, keep: Option<&str>) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        for disk in query_disks(&handle.qmp).await? {
            for bitmap in &disk.bitmaps {
                if !is_backup_bitmap(&bitmap.name) || keep == Some(bitmap.name.as_str()) {
                    continue;
                }
                let remove = handle
                    .qmp
                    .block_dirty_bitmap_remove(&disk.node_name, &bitmap.name);
                flatten(
                    tokio::time::timeout(QMP_TIMEOUT, remove).await,
                    "QMP block-dirty-bitmap-remove",
                )?;
            }
        }
        Ok(())
    }

    async fn incremental_backup(
        &self,
        id: Uuid,
        parent: &str,
        new: &str,
        targets: &[BackupTarget],
        quiesce: bool,
    ) -> Result<Quiesced, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        clear_backup_leftovers(qmp).await?;
        let disks = query_disks(qmp).await?;
        let nodes: Vec<&str> = disks.iter().map(|d| d.node_name.as_str()).collect();
        let wanted: Vec<&str> = targets.iter().map(|t| t.node.as_str()).collect();
        if nodes != wanted {
            return Err(DriverError::InvalidSpec(format!(
                "backup targets {wanted:?} do not match the disks {nodes:?} of VM {id}"
            )));
        }
        let mut opened = Vec::new();
        let mut freezer = Freezer::new(quiesce, &handle.paths, &handle.agent);
        let mut result = backup_into(
            qmp,
            &disks,
            (parent, new),
            targets,
            &mut opened,
            (id, &mut freezer),
        )
        .await;
        // 대상 노드를 닫는다. 닫을 때 QEMU가 이미지 메타데이터(L2)를 쓰고 플러시하므로, 닫지 못하면
        // 대상 파일이 온전하지 않아 백업 실패다(앞 비트맵을 남겨 다음도 같은 부모에서 증분). 남은 노드는
        // 다음 증분 백업이 앞서 남은 것으로 치운다.
        for node in &opened {
            let del = tokio::time::timeout(SNAPSHOT_TIMEOUT, qmp.blockdev_del(node)).await;
            if let Err(e) = flatten(del, "QMP blockdev-del") {
                tracing::warn!(%id, %node, error = %e, "cannot close backup target");
                result = result.and(Err(e));
            }
        }
        // 성공하면 새 비트맵만, 실패하면 앞 비트맵만 남긴다. 실패했으면 새 비트맵이 없을 수 있다.
        let stale = if result.is_ok() { parent } else { new };
        for disk in &disks {
            let remove = qmp.block_dirty_bitmap_remove(&disk.node_name, stale);
            let removed = flatten(
                tokio::time::timeout(QMP_TIMEOUT, remove).await,
                "QMP block-dirty-bitmap-remove",
            );
            if let (Err(e), true) = (removed, result.is_ok()) {
                tracing::warn!(%id, node = %disk.node_name, bitmap = stale, error = %e,
                    "cannot remove dirty bitmap");
            }
        }
        result.map(|()| freezer.outcome())
    }

    async fn remove_named_bitmaps(&self, id: Uuid, names: &[String]) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        remove_bitmaps_where(&handle.qmp, |name| names.iter().any(|n| n == name)).await
    }

    async fn clear_backup_leftovers(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        clear_backup_leftovers(&handle.qmp).await?;
        clear_fleecing(&handle.qmp, &self.state_dir(id)).await
    }

    async fn replicate_disks(
        &self,
        id: Uuid,
        req: &ReplicateRequest,
    ) -> Result<(u64, Quiesced), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        if req.targets.is_empty() {
            return Err(DriverError::InvalidSpec(format!(
                "nothing to replicate for VM {id}"
            )));
        }
        // 앞선 회차가 남긴 작업과 NBD 노드(TLS 객체를 쥔다)를 먼저 치운다.
        clear_jobs(qmp, &[REPLICA_JOB_PREFIX, CATCH_UP_PREFIX]).await?;
        let fleece_dir = self.state_dir(id);
        clear_fleecing(qmp, &fleece_dir).await?;
        let disks = query_disks(qmp).await?;
        if let Some(t) = req.targets.iter().find(|t| t.disk >= disks.len()) {
            return Err(DriverError::InvalidSpec(format!(
                "VM {id} has no disk{}",
                t.disk
            )));
        }
        let _ = tokio::time::timeout(QMP_TIMEOUT, qmp.object_del(REPLICA_TLS_ID)).await;
        let dir = migration_tls_dir(&self.config.state_dir);
        qmp_call(
            qmp.add_tls_creds(REPLICA_TLS_ID, &dir, TlsEndpoint::Client),
            "QMP object-add",
        )
        .await?;
        let mut opened = Vec::new();
        let mut fleeces = Vec::new();
        let halt = self.fleece_halt(id, &handle);
        let mut freezer = Freezer::new(req.quiesce, &handle.paths, &handle.agent);
        let mut result = send_replicas(
            qmp,
            &disks,
            req,
            (&fleece_dir, &halt, &mut freezer),
            &mut opened,
            &mut fleeces,
        )
        .await;
        // 플리싱을 뗀다. 떼지 못하면 회차 실패다(남은 것은 다음 회차가 뗀다).
        if let Err(e) = stop_fleecing(qmp, &mut fleeces).await {
            result = result.and(Err(e));
        }
        // NBD 노드를 닫는다. 닫을 때 받는 쪽에 쓴 것을 내보내므로(flush) 닫지 못하면 회차 실패다.
        for node in &opened {
            let del = tokio::time::timeout(SNAPSHOT_TIMEOUT, qmp.blockdev_del(node)).await;
            if let Err(e) = flatten(del, "QMP blockdev-del") {
                tracing::warn!(%id, %node, error = %e, "cannot close replication target");
                result = result.and(Err(e));
            }
        }
        if let Err(e) = &result {
            tracing::warn!(%id, error = %e, bitmap = %req.bitmap, "replication failed; dropping the new dirty bitmap");
            // 새 비트맵이 없을 수 있다(transaction 전에 실패). 기준 비트맵은 그대로 둔다.
            for t in &req.targets {
                let remove = qmp.block_dirty_bitmap_remove(&disks[t.disk].node_name, &req.bitmap);
                if let Err(e) = qmp_call(remove, "QMP block-dirty-bitmap-remove").await {
                    tracing::debug!(%id, disk = t.disk, error = %e, "no new dirty bitmap to remove");
                }
            }
        }
        result.map(|sent| (sent, freezer.outcome()))
    }

    async fn start_replica_receiver(
        &self,
        run: Uuid,
        exports: &[ReceiverExport],
        listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        if exports.is_empty() {
            return Err(DriverError::InvalidSpec(format!(
                "nothing to receive for run {run}"
            )));
        }
        let dir = self.receiver_dir(run);
        let socket = Receiver::qmp_socket(&dir);
        if socket.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(DriverError::InvalidSpec(format!(
                "socket path {socket:?} is longer than {MAX_SOCKET_PATH} bytes"
            )));
        }
        if self.receivers.lock().unwrap().contains_key(&run)
            || liveness(&Receiver::pidfile(&dir)) != Liveness::Dead
        {
            return Err(DriverError::Replication(format!(
                "a replica receiver for run {run} is already running"
            )));
        }
        // pid 파일 잠금이 풀린 것(= QEMU 없음)을 보았으므로 남은 소켓은 지워도 된다.
        std::fs::create_dir_all(&dir)?;
        remove_if_exists(&socket)?;
        let result = self
            .launch_receiver(run, &dir, exports, listen_host, ports)
            .await;
        if let Err(e) = &result {
            tracing::warn!(%run, error = %e, "replica receiver failed to start; stopping it");
            if let Err(stop) = self.stop_replica_receiver(run).await {
                tracing::error!(%run, error = %stop, "cannot stop the replica receiver");
            }
        }
        result
    }

    async fn finish_replica_receiver(&self, run: Uuid) -> Result<(), DriverError> {
        let receiver = self.receiver(run).await?;
        close_replicas(&receiver.qmp).await?;
        let pidfile = Receiver::pidfile(&receiver.dir);
        if !wait_until(KILL_TIMEOUT, || liveness(&pidfile) == Liveness::Dead).await {
            return Err(DriverError::Timeout("replica receiver QEMU to exit"));
        }
        self.receivers.lock().unwrap().remove(&run);
        if let Err(e) = std::fs::remove_dir_all(&receiver.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%run, error = %e, "cannot remove replica receiver dir");
        }
        Ok(())
    }

    async fn stop_replica_receiver(&self, run: Uuid) -> Result<(), DriverError> {
        let known = self.receivers.lock().unwrap().remove(&run);
        let dir = self.receiver_dir(run);
        let pidfile = Receiver::pidfile(&dir);
        if liveness(&pidfile) != Liveness::Dead {
            let pid = match &known {
                Some(r) => r.pid,
                None => read_pid(&pidfile)?,
            };
            tracing::info!(%run, pid, "killing replica receiver QEMU");
            kill_if_owner(&pidfile, pid);
            if !wait_until(KILL_TIMEOUT, || liveness(&pidfile) == Liveness::Dead).await {
                return Err(DriverError::Timeout("replica receiver QEMU to exit"));
            }
        }
        drop(known);
        match std::fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    async fn replica_receivers(&self) -> Result<Vec<Uuid>, DriverError> {
        Ok(self
            .receiver_dirs()?
            .into_iter()
            .filter(|(_, live)| *live != Liveness::Dead)
            .map(|(run, _)| run)
            .collect())
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        if devices.is_empty() {
            return Ok(());
        }
        let delete = handle.qmp.snapshot_delete(tag, &devices, SNAPSHOT_TIMEOUT);
        let _busy = self.busy(id, "snapshot-delete");
        job(delete, "QMP snapshot-delete").await
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        remove_inconsistent_bitmaps(&handle.qmp, id).await;
        let node = format!("disk{disk}");
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.block_resize(&node, size_bytes)).await;
        flatten(r, "QMP block_resize")
    }

    /// 멈추기 전에 표시([`PAUSED_MARK`])를 남긴다.
    async fn pause(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        self.pausing.lock().unwrap().insert(id);
        std::fs::write(handle.paths.dir.join(PAUSED_MARK), b"")?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.stop()).await;
        flatten(r, "QMP stop")
    }

    /// 이어서 실행한 뒤 표시를 지운다. 실패하면 표시가 남아 다시 붙을 때(`start`) 이어서 실행한다.
    async fn resume(&self, id: Uuid) -> Result<(), DriverError> {
        self.pausing.lock().unwrap().remove(&id);
        let handle = self.running_handle(id)?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
        flatten(r, "QMP cont")?;
        remove_if_exists(&handle.paths.dir.join(PAUSED_MARK))?;
        Ok(())
    }

    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError> {
        if self.handle(id).is_none() {
            return Ok(false);
        }
        self.continue_if_left_stopped(id).await?;
        Ok(true)
    }

    async fn qemu_version(&self) -> Result<String, DriverError> {
        let output = tokio::process::Command::new(&self.config.host.qemu_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(QMP_TIMEOUT, output)
            .await
            .map_err(|_| DriverError::Timeout("qemu --version"))??;
        let stdout = String::from_utf8_lossy(&output.stdout);
        match parse_qemu_version(&stdout) {
            Some(v) if output.status.success() => Ok(v),
            _ => Err(DriverError::ToolFailed {
                tool: "qemu --version",
                stderr: format!(
                    "{}{}",
                    stdout.trim(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }),
        }
    }

    async fn vcpu_props(
        &self,
        id: Uuid,
        names: &[String],
    ) -> Result<Vec<std::collections::BTreeMap<String, bool>>, DriverError> {
        let handle = self.running_handle(id)?;
        let r =
            tokio::time::timeout(QMP_TIMEOUT, handle.qmp.execute("query-cpus-fast", None)).await;
        let cpus = flatten(r, "QMP query-cpus-fast")?;
        let mut out = Vec::new();
        for cpu in cpus.as_array().into_iter().flatten() {
            let Some(path) = cpu["qom-path"].as_str() else {
                continue;
            };
            let mut props = std::collections::BTreeMap::new();
            for name in names {
                let args = json!({ "path": path, "property": name });
                let r =
                    tokio::time::timeout(QMP_TIMEOUT, handle.qmp.execute("qom-get", Some(args)))
                        .await;
                // 없는 속성은 QMP 오류다(빼고 넘어간다). 시간 초과는 오류다.
                match r {
                    Err(_) => return Err(DriverError::Timeout("QMP qom-get")),
                    Ok(Ok(v)) => {
                        if let Some(b) = v.as_bool() {
                            props.insert(name.clone(), b);
                        }
                    }
                    Ok(Err(QmpError::Command { .. })) => {}
                    Ok(Err(e)) => return Err(e.into()),
                }
            }
            out.push(props);
        }
        Ok(out)
    }

    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError> {
        let handle = self.running_handle(id)?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_version()).await;
        flatten(r, "QMP query-version")
    }

    async fn disk_stats(&self, id: Uuid) -> Result<DiskCounters, DriverError> {
        let handle = self.running_handle(id)?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_blockstats()).await;
        let stats = flatten(r, "QMP query-blockstats")?;
        // 디스크는 장치 id `disk{i}-dev`로 고른다. 맨 위 노드는 블록 작업 중 필터 노드로 바뀐다.
        Ok(stats
            .iter()
            .filter(|s| s.qdev.as_deref().and_then(disk_device_index).is_some())
            .fold(DiskCounters::default(), |sum, s| DiskCounters {
                rd_bytes: sum.rd_bytes + s.rd_bytes,
                wr_bytes: sum.wr_bytes + s.wr_bytes,
                rd_operations: sum.rd_operations + s.rd_operations,
                wr_operations: sum.wr_operations + s.wr_operations,
            }))
    }

    async fn guest_ping(&self, id: Uuid) -> Result<bool, DriverError> {
        let handle = self.running_handle(id)?;
        let sock = agent_socket(&handle, id)?;
        // 얼리는 중이면 답하지 않은 것으로 본다(게스트 감시는 한도 동안 이어진 무응답만 센다).
        let Some(_agent) = agent_lock(&handle).await else {
            return Ok(false);
        };
        agent::thaw_left(id, &handle.paths).await;
        Ok(opl_qemu::qga::ping(&sock, GUEST_PING_TIMEOUT).await?)
    }

    async fn guest_info(&self, id: Uuid) -> Result<GuestInfo, DriverError> {
        let handle = self.running_handle(id)?;
        let sock = agent_socket(&handle, id)?;
        let _agent = agent_lock(&handle).await.ok_or_else(agent_busy)?;
        agent::thaw_left(id, &handle.paths).await;
        let mut session = Session::connect(&sock, qga::SYNC_TIMEOUT)
            .await
            .map_err(|e| DriverError::GuestAgent(e.to_string()))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Ok(qga::guest_info(&mut session, now).await)
    }

    async fn set_user_password(
        &self,
        id: Uuid,
        user: &str,
        password: &str,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let sock = agent_socket(&handle, id)?;
        let _agent = agent_lock(&handle).await.ok_or_else(agent_busy)?;
        let mut session = Session::connect(&sock, qga::SYNC_TIMEOUT)
            .await
            .map_err(|e| DriverError::GuestAgent(e.to_string()))?;
        qga::set_user_password(&mut session, user, password)
            .await
            .map_err(|e| DriverError::GuestAgent(e.to_string()))
    }

    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        task: Uuid,
        listen_host: &str,
        ports: RangeInclusive<u16>,
        exports: &[usize],
    ) -> Result<IncomingPorts, DriverError> {
        let paths = self.paths(vm.id)?;
        if self.handle(vm.id).is_some() || liveness(&paths.pidfile) != Liveness::Dead {
            return Err(DriverError::Migration(format!(
                "VM {} is already running on this node",
                vm.id
            )));
        }
        if let Err(e) = self.launch(vm, &paths, Launch::Incoming { task }).await {
            if self.handle(vm.id).is_none() {
                self.kill_and_cleanup(vm.id, &paths).await;
            } else if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                tracing::error!(id = %vm.id, error = %stop, "failed to stop incoming QEMU");
            }
            return Err(e);
        }
        match self
            .listen_incoming(vm.id, listen_host, ports, exports)
            .await
        {
            Ok(ports) => Ok(ports),
            Err(e) => {
                // QEMU 하나는 한 번만 받을 수 있으므로 버린다.
                if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                    tracing::error!(id = %vm.id, error = %stop, "failed to stop incoming QEMU");
                }
                Err(e)
            }
        }
    }

    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        if !handle.paths.dir.join(INCOMING_MARK).exists() {
            return Err(DriverError::Migration(format!(
                "VM {id} is not receiving a migration"
            )));
        }
        let wait = handle.qmp.wait_migration(MIGRATION_POLL);
        match tokio::time::timeout(INCOMING_WAIT_TIMEOUT, wait).await {
            Err(_) => Err(DriverError::Timeout("incoming migration to complete")),
            // 받기에 실패한 QEMU는 스스로 끝난다.
            Ok(Err(QmpError::Disconnected)) => {
                Err(DriverError::Migration("the receiving QEMU exited".into()))
            }
            Ok(Err(e)) => Err(e.into()),
            Ok(Ok(info)) => migration_result(info),
        }
    }

    async fn finish_incoming(&self, id: Uuid, bitmap: Option<&str>) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let mark = handle.paths.dir.join(INCOMING_MARK);
        if !mark.exists() {
            return Ok(());
        }
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        let state = flatten(status, "QMP query-status")?;
        if matches!(state, RunState::Paused | RunState::Running) {
            // 디스크 내보내기를 닫는다. 내보낸 디스크가 없었으면 서버도 없어 QEMU 오류다.
            let stop = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.nbd_server_stop()).await;
            match flatten(stop, "QMP nbd-server-stop") {
                Ok(()) | Err(DriverError::Qmp(QmpError::Command { .. })) => {}
                Err(e) => return Err(e),
            }
        }
        if let (RunState::Paused, Some(name)) = (&state, bitmap) {
            // 게스트가 돌기 전에 만들어야 이어서 실행한 뒤의 쓰기를 모두 담는다.
            match query_disks(&handle.qmp).await {
                Ok(disks) => {
                    let actions: Vec<TransactionAction> =
                        disks.iter().map(|disk| bitmap_add(disk, name)).collect();
                    let r =
                        tokio::time::timeout(QMP_TIMEOUT, handle.qmp.transaction(&actions)).await;
                    if let Err(e) = flatten(r, "QMP transaction") {
                        tracing::warn!(%id, bitmap = name, error = %e, "cannot add the dirty bitmap of the reversed replication");
                    }
                }
                Err(e) => {
                    tracing::warn!(%id, bitmap = name, error = %e, "cannot add the dirty bitmap of the reversed replication");
                }
            }
        }
        match state {
            RunState::Paused => {
                tracing::info!(%id, "continuing received VM");
                let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
                flatten(cont, "QMP cont")?;
            }
            RunState::Running => {}
            state => {
                return Err(DriverError::Migration(format!(
                    "incoming migration of VM {id} has not completed (QEMU is {state:?})"
                )));
            }
        }
        remove_if_exists(&mark)?;
        Ok(())
    }

    async fn list_incoming(&self) -> Result<Vec<(Uuid, Uuid)>, DriverError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.config.run_dir.join("vm"))? {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let paths = self.paths(id)?;
            let mark = paths.dir.join(INCOMING_MARK);
            if mark.exists() && liveness(&paths.pidfile) != Liveness::Dead {
                // 읽을 수 없으면 어느 마이그레이션과도 맞지 않는 nil로 둔다(정리가 끈다).
                let task = std::fs::read_to_string(&mark)
                    .ok()
                    .and_then(|s| Uuid::parse_str(s.trim()).ok())
                    .unwrap_or_default();
                out.push((id, task));
            }
        }
        Ok(out)
    }

    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        opts: &MigrateOptions,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        // 받는 쪽은 기록대로 켠다. 게스트가 아직 내려 주지 않은 장치 등으로 다르면 보내지 않는다(장치가 달라 실패하거나
        // 옮긴 뒤 장치가 사라진다).
        if let Some(expected) = &opts.devices {
            check_devices(qmp, expected).await?;
        }
        remove_inconsistent_bitmaps(qmp, id).await;
        let catch_up = opts.switchover_catch_up.as_ref();
        let mirrored = if opts.switchover_mirrors {
            let mirrored = mirrored_disks(&query_jobs(qmp).await?);
            if mirrored.is_empty() {
                return Err(DriverError::Migration(
                    "no disk mirrors to switch over".into(),
                ));
            }
            mirrored
        } else {
            Vec::new()
        };
        // qcow2의 persistent 비트맵은 공유 이미지로 넘어간다. 능력을 켜면 그것도 스트림으로 보내고,
        // 마이그레이션이 실패하면 원본 이미지에 in-use로 남아 다음 시작에 inconsistent가 된다.
        // 미러하는 디스크는 대상이 다른 이미지이므로 persistent 비트맵도 스트림으로 보낸다. 따라잡기는 넘기지 않는다:
        // 넘기는 비트맵은 마이그레이션 동안 바쁨이라 마지막 패스가 그 비트맵으로 작업을 시작하지 못한다(다른 복제
        // 대상과 백업의 비트맵은 대상에서 없어 다음이 전체다).
        let dirty_bitmaps =
            catch_up.is_none() && migrated_bitmaps(&query_disks(qmp).await?, &mirrored);
        let pause = opts.switchover_mirrors || catch_up.is_some();
        self.setup_migration(
            qmp,
            TlsEndpoint::Client,
            Some(&target.tls_hostname),
            opts.bandwidth,
            dirty_bitmaps,
            pause,
        )
        .await?;
        let uri = tcp_uri(&target.host, target.port);
        tracing::info!(%id, %uri, mirrors = mirrored.len(), catch_up = catch_up.is_some(), "migrating VM");
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, qmp.migrate(&uri)).await,
            "QMP migrate",
        )?;
        let deadline = tokio::time::Instant::now() + opts.timeout;
        let switchover =
            |s: &MigrationState| s.is_finished() || (pause && *s == MigrationState::PreSwitchover);
        let mut info = wait_or_cancel(qmp, id, deadline, opts.timeout, switchover).await?;
        if info.state == MigrationState::PreSwitchover {
            // 게스트가 멈췄고 원본은 아직 블록 장치를 쥐고 있다. 미러를 끝내거나 마지막 패스를 보내면 대상이 원본과 같다.
            let done = match catch_up {
                Some(state) => {
                    match tokio::time::timeout_at(deadline, last_catch_up(qmp, state)).await {
                        Ok(r) => r,
                        Err(_) => Err(DriverError::Migration(format!(
                            "the last disk copy did not finish within {}",
                            format_duration(opts.timeout)
                        ))),
                    }
                }
                None => complete_mirrors(qmp, &mirrored).await,
            };
            if let Err(e) = done {
                tracing::warn!(%id, error = %e, "disk copies failed at switchover; cancelling");
                cancel_and_wait(qmp).await?;
                if let Err(clear) = clear_jobs(qmp, &[MIRROR_JOB_PREFIX, CATCH_UP_PREFIX]).await {
                    tracing::warn!(%id, error = %clear, "cannot clear disk copies");
                }
                return Err(e);
            }
            let cont = tokio::time::timeout(QMP_TIMEOUT, qmp.migrate_continue()).await;
            flatten(cont, "QMP migrate-continue")?;
            info = wait_or_cancel(qmp, id, deadline, opts.timeout, MigrationState::is_finished)
                .await?;
        }
        migration_result(info)?;
        wait_postmigrate(qmp, id).await;
        Ok(())
    }

    async fn mirror_disks(
        &self,
        id: Uuid,
        targets: &[MirrorTarget],
        opts: &MirrorOptions,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        // 앞선 마이그레이션이 남긴 미러와 그 NBD 노드(TLS 객체를 쥔다)를 먼저 치운다.
        clear_jobs(qmp, &[MIRROR_JOB_PREFIX]).await?;
        let disks = query_disks(qmp).await?;
        for target in targets {
            let Some(disk) = disks.get(target.disk) else {
                return Err(DriverError::InvalidSpec(format!(
                    "VM {id} has no disk{}",
                    target.disk
                )));
            };
            // raw 이미지는 비트맵을 담을 수 없어 qcow2의 persistent 비트맵은 대상에서 사라진다(`opl-` 무리 모두 —
            // 복제 비트맵도 덮는다).
            if disk.driver == "qcow2" && target.format == ImageFormat::Raw {
                for bitmap in disk
                    .bitmaps
                    .iter()
                    .filter(|b| b.name.starts_with(BITMAP_PREFIX))
                {
                    tracing::info!(%id, node = %disk.node_name, bitmap = %bitmap.name,
                        "removing dirty bitmap of a disk moving to a raw image");
                    let remove = qmp.block_dirty_bitmap_remove(&disk.node_name, &bitmap.name);
                    flatten(
                        tokio::time::timeout(QMP_TIMEOUT, remove).await,
                        "QMP block-dirty-bitmap-remove",
                    )?;
                }
            }
        }
        let _ = tokio::time::timeout(QMP_TIMEOUT, qmp.object_del(NBD_TLS_ID)).await;
        let dir = migration_tls_dir(&self.config.state_dir);
        let add = qmp.add_tls_creds(NBD_TLS_ID, &dir, TlsEndpoint::Client);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, add).await,
            "QMP object-add",
        )?;
        let result = start_mirrors(qmp, targets, opts.speed).await;
        let result = match result {
            Ok(ids) => wait_mirrors_ready(qmp, &ids, opts).await,
            Err(e) => Err(e),
        };
        if let Err(e) = &result {
            tracing::warn!(%id, error = %e, "disk copy failed; clearing mirrors");
            if let Err(clear) = clear_jobs(qmp, &[MIRROR_JOB_PREFIX]).await {
                tracing::warn!(%id, error = %clear, "cannot clear disk mirrors");
            }
        }
        result
    }

    async fn catch_up_disks(&self, id: Uuid, req: &CatchUp) -> Result<CatchUpState, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        if req.targets.is_empty() || req.base.len() != req.targets.len() || req.passes == 0 {
            return Err(DriverError::InvalidSpec(format!(
                "nothing to catch up for VM {id}"
            )));
        }
        let deadline = (tokio::time::Instant::now() + req.timeout, req.timeout);
        // 앞선 따라잡기(와 복제 회차)가 남긴 작업, NBD 노드(TLS 객체를 쥔다)와 플리싱을 먼저 치운다.
        clear_jobs(qmp, &[CATCH_UP_PREFIX, REPLICA_JOB_PREFIX]).await?;
        let fleece_dir = self.state_dir(id);
        clear_fleecing(qmp, &fleece_dir).await?;
        let disks = query_disks(qmp).await?;
        if let Some(t) = req.targets.iter().find(|t| t.disk >= disks.len()) {
            return Err(DriverError::InvalidSpec(format!(
                "VM {id} has no disk{}",
                t.disk
            )));
        }
        let _ = tokio::time::timeout(QMP_TIMEOUT, qmp.object_del(NBD_TLS_ID)).await;
        let dir = migration_tls_dir(&self.config.state_dir);
        qmp_call(
            qmp.add_tls_creds(NBD_TLS_ID, &dir, TlsEndpoint::Client),
            "QMP object-add",
        )
        .await?;
        let mut result = Ok(());
        for t in &req.targets {
            let (_, node) = catch_up_names(t.disk);
            let nbd = NbdBlockdev {
                node_name: node,
                host: t.host.clone(),
                port: t.port,
                export: t.export.clone(),
                tls_creds: NBD_TLS_ID.into(),
                tls_hostname: t.tls_hostname.clone(),
            };
            result = qmp_call(qmp.blockdev_add_nbd(&nbd), "QMP blockdev-add").await;
            if result.is_err() {
                break;
            }
        }
        let halt = self.fleece_halt(id, &handle);
        let result = match result {
            Ok(()) => catch_up_passes(qmp, &disks, req, (&fleece_dir, &halt), deadline).await,
            Err(e) => Err(e),
        };
        if let Err(e) = &result {
            tracing::warn!(%id, error = %e, "catching up the disks failed; clearing the copies");
            if let Err(clear) = clear_jobs(qmp, &[CATCH_UP_PREFIX]).await {
                tracing::warn!(%id, error = %clear, "cannot clear the disk copies");
            }
            // 작업을 치우지 못했어도 떼어 본다(남으면 다음 회차·따라잡기·남은 것 치우기가 뗀다).
            if let Err(clear) = clear_fleecing(qmp, &fleece_dir).await {
                tracing::warn!(%id, error = %clear, "cannot remove the fleecing");
            }
            // 이 Task의 임시 비트맵만 지운다. 복제 비트맵은 그대로 둔다.
            let prefix = format!("{MIGRATION_BITMAP_PREFIX}{}-", req.task.simple());
            if let Err(remove) = remove_bitmaps_where(qmp, |name| name.starts_with(&prefix)).await {
                tracing::warn!(%id, error = %remove, "cannot remove the catch-up dirty bitmaps");
            }
        }
        result
    }

    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(false);
        };
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        if flatten(status, "QMP query-status")? != RunState::PostMigrate {
            return Ok(false);
        }
        drop(handle);
        tracing::info!(%id, "stopping migrated source QEMU");
        self.stop(id, StopMode::Force).await?;
        Ok(true)
    }

    async fn cancel_migration(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = match self.running_handle(id) {
            Ok(handle) => handle,
            Err(DriverError::NotRunning(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        // 받는 QEMU의 마이그레이션은 보내는 쪽이 다룬다.
        if handle.paths.dir.join(INCOMING_MARK).exists() {
            return Ok(());
        }
        let info = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_migrate()).await;
        let info = flatten(info, "QMP query-migrate")?;
        if info.state != MigrationState::None && !info.state.is_finished() {
            tracing::warn!(%id, state = ?info.state, "cancelling an outgoing migration");
            cancel_and_wait(&handle.qmp).await?;
        }
        // 마이그레이션을 멈춘 뒤 미러와 따라잡기를 치운다(디스크 복사 중이거나 실패한 뒤 남은 것).
        clear_jobs(&handle.qmp, &[MIRROR_JOB_PREFIX, CATCH_UP_PREFIX]).await?;
        remove_bitmaps_where(&handle.qmp, |name| {
            name.starts_with(MIGRATION_BITMAP_PREFIX)
        })
        .await
    }

    async fn check_devices(&self, id: Uuid, expected: &[String]) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        check_devices(&handle.qmp, expected).await
    }

    async fn hotplug_caps(&self) -> NodeHotplug {
        let host = &self.config.host;
        let output = tokio::process::Command::new(&host.qemu_binary)
            .args(["-device", "help"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(QMP_TIMEOUT, output).await {
            Ok(Ok(out)) if out.status.success() => {
                let devices = String::from_utf8_lossy(&out.stdout);
                // virtio-mem이 꽂을 때 미리 잡는지(`prealloc`, QEMU 7.1+, 리뷰 2): 없으면 hugepages VM은 메모리
                // 핫플러그 없이 켠다.
                let mem_help = if devices.contains("\"virtio-mem-pci\"") {
                    let help = tokio::process::Command::new(&host.qemu_binary)
                        .args(["-device", "virtio-mem-pci,help"])
                        .stdin(Stdio::null())
                        .kill_on_drop(true)
                        .output();
                    match tokio::time::timeout(QMP_TIMEOUT, help).await {
                        Ok(Ok(out)) if out.status.success() => {
                            String::from_utf8_lossy(&out.stdout).into_owned()
                        }
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };
                opl_qemu::hotplug_caps(host.arch, LINUX, &devices, &mem_help)
            }
            other => {
                let error = match other {
                    Ok(Ok(out)) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
                    Ok(Err(e)) => e.to_string(),
                    Err(_) => "timed out".into(),
                };
                tracing::warn!(binary = ?host.qemu_binary, %error,
                    "cannot list QEMU devices; memory hotplug is off on this node");
                opl_qemu::hotplug_caps(host.arch, LINUX, "", "")
            }
        }
    }

    async fn firmware_caps(&self) -> FirmwareCaps {
        let host = &self.config.host;
        FirmwareCaps {
            tpm: tpm::tpm_capable(
                &self.config.swtpm,
                &host.qemu_binary,
                &self.config.state_dir,
                &self.config.run_dir,
            )
            .await,
            secure_boot: host
                .secure_boot
                .as_ref()
                .is_some_and(|p| p.code.is_file() && p.vars_template.is_file()),
        }
    }

    async fn cpu_catalog(&self) -> Result<CpuProbe, DriverError> {
        let host = &self.config.host;
        opl_qemu::cpu::probe(
            &host.qemu_binary,
            host.arch,
            host.accel,
            &self.config.run_dir,
            opl_qemu::cpu::PROBE_TIMEOUT,
        )
        .await
        .map_err(|e| DriverError::CpuProbe(e.to_string()))
    }

    async fn set_vcpus(
        &self,
        id: Uuid,
        cpu: &CpuSpec,
        from: u16,
        to: u16,
        timeout: Duration,
    ) -> Result<VcpuChange, DriverError> {
        let (boot, max) = (cpu.vcpus(), cpu.max());
        if max <= boot {
            return Err(DriverError::InvalidSpec(format!(
                "VM {id} was started without vCPU hotplug"
            )));
        }
        if !(boot..=max).contains(&u32::from(to)) {
            return Err(DriverError::InvalidSpec(format!(
                "VM {id} can run {boot} to {max} vCPUs, not {to}"
            )));
        }
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let pin = read_pinning(&handle.paths.dir).filter(|p| !p.vcpus.is_empty());
        // 켤 때처럼 소켓마다 `max / sockets`개 코어 자리를 앞에서부터 채운다(cmdline과 같은 자리).
        let per_socket = max / u32::from(cpu.sockets);
        let mut now = from;
        let mut refused = false;
        let stepped = async {
            if to >= from {
                // 켤 때 수부터 본다: 앞서 떼기를 요청한 vCPU를 게스트가 늦게 내려 줬으면 그 자리도 다시 붙인다.
                let slots =
                    qmp_call(qmp.query_hotpluggable_cpus(), "QMP query-hotpluggable-cpus").await?;
                let boot = u16::try_from(boot).unwrap_or(u16::MAX);
                for i in boot..to {
                    let (socket, core) = (u32::from(i) / per_socket, u32::from(i) % per_socket);
                    let slot = slots.iter().find(|s| {
                        s.socket_id == Some(socket)
                            && s.core_id == Some(core)
                            && s.thread_id.unwrap_or(0) == 0
                    });
                    match slot {
                        None => {
                            return Err(DriverError::InvalidSpec(format!(
                                "VM {id} has no vCPU slot at socket {socket} core {core}"
                            )));
                        }
                        // 앞서 붙인 자리(다시 해 보기)
                        Some(s) if s.qom_path.is_some() => {}
                        Some(s) => {
                            let args = json!({
                                "driver": s.kind, "id": format!("cpu{i}"), "socket-id": socket,
                                "core-id": core, "thread-id": s.thread_id.unwrap_or(0),
                            });
                            qmp_call(qmp.device_add(args), "QMP device_add").await?;
                            // 계획 64: 새 vCPU 스레드를 켤 때 잡아 둔 그 자리의 pCPU에 고정한다. 안 되면 되돌린다.
                            if let Some(pin) = &pin
                                && let Err(e) = pin_threads(&handle, pin).await
                            {
                                if let Err(del) = unplug(qmp, &format!("cpu{i}"), timeout).await {
                                    tracing::warn!(%id, vcpu = i, error = %del, "cannot remove the vCPU that could not be pinned");
                                }
                                return Err(e);
                            }
                        }
                    }
                    if i >= from {
                        now = i + 1;
                    }
                }
            } else {
                // 큰 번호부터 뗀다. 켤 때 단 vCPU는 위에서 막았다.
                for i in (to..from).rev() {
                    match unplug(qmp, &format!("cpu{i}"), timeout).await {
                        Ok(true) => now = i,
                        Ok(false) => {
                            tracing::warn!(%id, vcpu = i, ?timeout, "the guest did not release a vCPU");
                            break;
                        }
                        // 게스트가 vCPU 떼기를 켜지 않았다(QEMU 8.2 q35는 OS가 새 ACPI CPU 핫플러그로 바꾸기 전까지 옛
                        // 방식이라 떼기를 거절한다). 요청이 남지 않으므로 떼는 중이 아니다.
                        Err(DriverError::Qmp(QmpError::Command { desc, .. }))
                            if vcpu_unplug_refused(&desc) =>
                        {
                            tracing::warn!(%id, vcpu = i, error = %desc, "QEMU refused to unplug a vCPU");
                            refused = true;
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            Ok(())
        }
        .await;
        match stepped {
            Ok(()) => {}
            Err(e) if now == from => return Err(e),
            // 몇 개를 바꾼 뒤의 오류: 닿은 수를 알린다(호출자가 기록한다).
            Err(e) => {
                tracing::warn!(%id, from, to, reached = now, error = %e, "changing vCPUs stopped")
            }
        }
        Ok(VcpuChange {
            reached: now,
            refused,
        })
    }

    async fn set_memory(
        &self,
        id: Uuid,
        plugged_mib: u32,
        timeout: Duration,
    ) -> Result<u32, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        // vNUMA VM(계획 64)은 게스트 노드마다 장치가 있다.
        let nodes = virtio_mem_nodes(qmp).await?;
        if nodes > 1 {
            return set_memory_per_node(qmp, id, nodes, plugged_mib, timeout).await;
        }
        let size = virtio_mem_size(qmp).await?;
        let requested = u64::from(plugged_mib) << 20;
        let set = qmp.qom_set(VIRTIO_MEM_PATH, "requested-size", json!(requested));
        qmp_call(set, "QMP qom-set").await?;
        if requested >= size {
            return Ok(plugged_mib);
        }
        // 줄이기: 게스트가 블록을 내려 주어 `size`가 요청 이하가 되기를 기다린다.
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let size = virtio_mem_size(qmp).await?;
            if size <= requested {
                return Ok(plugged_mib);
            }
            if tokio::time::Instant::now() >= deadline {
                // 게스트가 더 내려 주지 않게 지금 크기를 요청으로 되돌린다.
                tracing::warn!(%id, size, requested, ?timeout, "the guest did not release memory");
                let set = qmp.qom_set(VIRTIO_MEM_PATH, "requested-size", json!(size));
                qmp_call(set, "QMP qom-set").await?;
                return Ok(u32::try_from(size.div_ceil(1 << 20)).unwrap_or(u32::MAX));
            }
            tokio::time::sleep(VIRTIO_MEM_POLL).await;
        }
    }

    async fn add_disk(
        &self,
        id: Uuid,
        index: usize,
        disk: &RuntimeDisk,
        port: u8,
    ) -> Result<AddOutcome, DriverError> {
        if disk.bus != DiskBus::VirtioBlk {
            return Err(DriverError::InvalidSpec(
                "only virtio-blk disks can be added while the VM is running".into(),
            ));
        }
        let mut source = match disk.kind {
            DiskKind::File => json!({ "driver": "file", "filename": disk.path }),
            DiskKind::Block => json!({ "driver": "host_device", "filename": disk.path }),
            DiskKind::Rbd => {
                let Some(rbd) = &disk.rbd else {
                    return Err(DriverError::InvalidSpec(format!(
                        "disk {index} is an rbd disk without an rbd image"
                    )));
                };
                let mut source = json!({
                    "driver": "rbd", "pool": rbd.pool, "image": rbd.image, "user": rbd.user,
                });
                if let Some(ns) = &rbd.namespace {
                    source["namespace"] = json!(ns);
                }
                if let Some(conf) = &rbd.conf {
                    source["conf"] = json!(conf);
                }
                source
            }
        };
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        // cmdline과 같은 두 층(`disk{i}-file` 위에 형식 노드 `disk{i}`)과 장치 `disk{i}-dev`
        let (file, node) = (format!("disk{index}-file"), format!("disk{index}"));
        // 앞서 중단된 Task가 붙였으면 그대로 둔다. 장치 없이 남은 노드는 닫고 다시 연다.
        let devices = qmp_call(qmp.peripheral_ids(), "QMP qom-list").await?;
        if devices.contains(&format!("disk{index}-dev")) {
            return Ok(AddOutcome::AlreadyPresent);
        }
        let nodes = qmp_call(qmp.query_block_nodes(), "QMP query-named-block-nodes").await?;
        for left in [&node, &file] {
            if nodes.iter().any(|n| n.node_name == *left) {
                tracing::info!(%id, node = %left, "closing a block node left by an earlier disk add");
                qmp_call(qmp.blockdev_del(left), "QMP blockdev-del").await?;
            }
        }
        source["node-name"] = json!(file);
        qmp_call(qmp.blockdev_add(source), "QMP blockdev-add").await?;
        let format = match disk.format {
            ImageFormat::Qcow2 => "qcow2",
            ImageFormat::Raw => "raw",
        };
        let mut opened = vec![file.clone()];
        let mut dev = json!({
            "driver": "virtio-blk-pci", "drive": node, "id": format!("disk{index}-dev"),
            "bus": format!("hp{port}"),
        });
        if let Some(order) = disk.boot_order {
            dev["bootindex"] = json!(order);
        }
        let added = async {
            let layer = json!({ "driver": format, "file": file, "node-name": node });
            qmp_call(qmp.blockdev_add(layer), "QMP blockdev-add").await?;
            opened.push(node.clone());
            qmp_call(qmp.device_add(dev), "QMP device_add").await
        }
        .await;
        if let Err(e) = added {
            // 연 노드를 거꾸로 닫는다(형식 노드가 파일 노드를 쥔다).
            for node in opened.iter().rev() {
                let del = qmp_call(qmp.blockdev_del(node), "QMP blockdev-del").await;
                if let Err(del) = del {
                    tracing::warn!(%id, %node, error = %del, "cannot close the block node of a disk not added");
                }
            }
            return Err(e);
        }
        Ok(AddOutcome::Added)
    }

    async fn remove_disk(
        &self,
        id: Uuid,
        index: usize,
        timeout: Duration,
    ) -> Result<bool, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        if !unplug(qmp, &format!("disk{index}-dev"), timeout).await? {
            return Ok(false);
        }
        // 장치는 빠졌다. 블록 노드를 닫지 못해도 떼기는 된 것이다(경고만 — 남은 노드는 QEMU가 끝날 때 닫힌다).
        // 앞서 닫았으면(다시 해 보기) 남은 노드만 닫는다.
        let nodes = match qmp_call(qmp.query_block_nodes(), "QMP query-named-block-nodes").await {
            Ok(nodes) => nodes,
            Err(e) => {
                tracing::warn!(%id, index, error = %e, "cannot list the block nodes of a removed disk");
                return Ok(true);
            }
        };
        for node in [format!("disk{index}"), format!("disk{index}-file")] {
            if nodes.iter().any(|n| n.node_name == node)
                && let Err(e) = qmp_call(qmp.blockdev_del(&node), "QMP blockdev-del").await
            {
                tracing::warn!(%id, %node, error = %e, "cannot close the block node of a removed disk");
            }
        }
        Ok(true)
    }

    async fn add_nic(
        &self,
        id: Uuid,
        index: usize,
        nic: &RuntimeNic,
        port: u8,
    ) -> Result<AddOutcome, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let netdev = format!("net{index}");
        // 앞서 중단된 Task가 붙였으면 tap도 그대로 둔다(장치 없이 남은 백엔드는 아래 `netdev_add`에서 바꾼다).
        let devices = qmp_call(qmp.peripheral_ids(), "QMP qom-list").await?;
        if devices.contains(&format!("nic{index}")) {
            return Ok(AddOutcome::AlreadyPresent);
        }
        // 브리지 NIC는 켤 때(create_taps)처럼 이 VM 이름의 tap을 먼저 만든다(SDN NIC는 OVS `br-int`에).
        let tap = match Tap::from_nic(id, nic) {
            Some(tap) => {
                let prefix = tap_prefix(id);
                if !tap.ifname.starts_with(&prefix) {
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
                net.create_tap(&tap).await?;
                Some((net, tap.ifname))
            }
            None => None,
        };
        let backend = match &nic.backend {
            NicBackend::User => json!({ "type": "user", "id": netdev }),
            NicBackend::Tap { ifname, .. } => json!({
                "type": "tap", "id": netdev, "ifname": ifname, "script": "no", "downscript": "no",
            }),
        };
        let model = match nic.model {
            NicModel::Virtio => "virtio-net-pci",
            NicModel::E1000e => "e1000e",
        };
        let mut dev = json!({
            "driver": model, "netdev": netdev, "mac": nic.mac.to_string(),
            "id": format!("nic{index}"), "bus": format!("hp{port}"),
        });
        // VXLAN 네트워크의 MTU를 게스트에 알린다(virtio만 받는다).
        if let (NicModel::Virtio, NicBackend::Tap { mtu: Some(mtu), .. }) =
            (nic.model, &nic.backend)
        {
            dev["host_mtu"] = json!(mtu);
        }
        let mut backend_added = false;
        let added = async {
            if let Err(e) = qmp_call(qmp.netdev_add(backend.clone()), "QMP netdev_add").await {
                // 장치 없이 남은 같은 id의 백엔드(앞서 중단된 붙이기)는 지우고 다시 만든다.
                let left = matches!(&e, DriverError::Qmp(QmpError::Command { desc, .. })
                    if desc.contains("Duplicate") || desc.contains("already"));
                if !left {
                    return Err(e);
                }
                tracing::info!(%id, %netdev, "replacing a backend left by an earlier NIC add");
                qmp_call(qmp.netdev_del(&netdev), "QMP netdev_del").await?;
                qmp_call(qmp.netdev_add(backend), "QMP netdev_add").await?;
            }
            backend_added = true;
            qmp_call(qmp.device_add(dev), "QMP device_add").await
        }
        .await;
        if let Err(e) = added {
            if backend_added
                && let Err(del) = qmp_call(qmp.netdev_del(&netdev), "QMP netdev_del").await
            {
                tracing::warn!(%id, %netdev, error = %del, "cannot remove the backend of a NIC not added");
            }
            if let Some((net, ifname)) = tap
                && let Err(del) = net.delete_tap(&ifname).await
            {
                tracing::warn!(%id, %ifname, error = %del, "cannot delete the tap of a NIC not added");
            }
            return Err(e);
        }
        Ok(AddOutcome::Added)
    }

    async fn remove_nic(
        &self,
        id: Uuid,
        index: usize,
        timeout: Duration,
    ) -> Result<bool, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        if !unplug(qmp, &format!("nic{index}"), timeout).await? {
            return Ok(false);
        }
        // 장치는 빠졌다. 백엔드·tap을 지우지 못해도 떼기는 된 것이다(경고만 — VM이 끝날 때 tap을 모두 지운다).
        // 앞서 지웠으면(다시 해 보기) 백엔드가 없다.
        let netdev = format!("net{index}");
        match qmp_call(qmp.netdev_del(&netdev), "QMP netdev_del").await {
            Ok(()) => {}
            Err(DriverError::Qmp(QmpError::Command { class, .. })) if class == "DeviceNotFound" => {
            }
            Err(e) => {
                tracing::warn!(%id, %netdev, error = %e, "cannot remove the backend of a removed NIC")
            }
        }
        if let Some(net) = &self.config.net {
            let tap = tap_name(id, index);
            if let Err(e) = net.delete_tap(&tap).await {
                tracing::warn!(%id, %tap, error = %e, "cannot delete the tap of a removed NIC");
            }
        }
        Ok(true)
    }

    async fn change_medium(
        &self,
        id: Uuid,
        index: usize,
        medium: Option<&RuntimeMedium>,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let dev = format!("cd{index}-dev");
        let media = qmp_call(qmp.query_block_media(), "QMP query-block").await?;
        let Some((_, now)) = media.iter().find(|(d, _)| *d == dev) else {
            return Err(DriverError::InvalidSpec(format!(
                "the VM has no CD-ROM drive {index}"
            )));
        };
        let wanted = medium.map(|m| m.path.as_path());
        if now.as_deref() == wanted {
            // 같은 매체(빈 드라이브)지만 트레이가 열려 있으면(게스트가 열었거나 앞서 실패) 닫아 둔다.
            if qmp_call(qmp.query_tray_open(&dev), "QMP query-block").await? {
                qmp_call(qmp.blockdev_close_tray(&dev), "QMP blockdev-close-tray").await?;
            }
            return Ok(());
        }
        let change = opl_qemu::change_medium(qmp, index, wanted);
        flatten(
            tokio::time::timeout(MEDIUM_TIMEOUT, change).await,
            "QMP CD-ROM medium change",
        )
    }

    fn save_hardware(&self, id: Uuid, hw: &RunningHardware) -> Result<(), DriverError> {
        let dir = self.paths(id)?.dir;
        if !dir.is_dir() {
            return Err(DriverError::NotRunning(id));
        }
        let json = serde_json::to_vec(hw).map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        std::fs::write(dir.join(HARDWARE_FILE), json)?;
        Ok(())
    }

    fn saved_hardware(&self, id: Uuid) -> Option<RunningHardware> {
        let file = self.paths(id).ok()?.dir.join(HARDWARE_FILE);
        let bytes = std::fs::read(&file).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(hw) => Some(hw),
            Err(e) => {
                tracing::warn!(%id, file = %file.display(), error = %e, "cannot read the saved hardware record");
                None
            }
        }
    }

    async fn repin(&self, id: Uuid, pin: &RuntimePinning) -> Result<usize, DriverError> {
        let handle = self.running_handle(id)?;
        // QMP를 붙잡는 작업 중에는 vCPU 스레드를 물을 수 없다. 다음 차례(30초 뒤)에 본다.
        if self.busy_with(id).is_some() {
            return Ok(0);
        }
        let mut changed = 0;
        // 스코프를 먼저: cpuset이 좁아지면 커널이 스레드 친화를 그 안으로 자른다.
        if self.config.systemd_scope
            && let Some(cpus) = &pin.scope
        {
            let unit = format!("opl-vm-{id}.scope");
            if scope_cpus(&unit).await.as_ref() != Some(cpus) {
                set_scope_cpus(&unit, cpus).await?;
                changed += 1;
            }
        }
        changed += pin_threads(&handle, pin).await?;
        // 바뀐 때만 쓴다(리뷰 2 N3 — 30초마다 VM마다 쓰지 않는다).
        if read_pinning(&handle.paths.dir).as_ref() != Some(pin) {
            write_pinning(&handle.paths.dir, pin)?;
        }
        Ok(changed)
    }

    fn forget_pinning(&self, id: Uuid) {
        if let Ok(paths) = self.paths(id) {
            let _ = std::fs::remove_file(paths.dir.join(PINNING_FILE));
        }
    }

    fn saved_pinning(&self, id: Uuid) -> Option<RuntimePinning> {
        read_pinning(&self.paths(id).ok()?.dir)
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

/// QEMU 시작 stderr가 다른 프로세스가 쥔 이미지 잠금 때문인지. QEMU(file-posix, 8.2와 11 같음)는
/// `Failed to get "write" lock`이나 `Failed to get shared "write" lock` 뒤에
/// `Is another process using the image [경로]?`를 붙인다.
/// VM의 게스트 에이전트 채널 소켓. 채널 없이 띄운 VM이면 `InvalidSpec`이다.
fn agent_socket(handle: &VmHandle, id: Uuid) -> Result<PathBuf, DriverError> {
    let sock = handle.paths.qga_socket.clone();
    if !sock.exists() {
        return Err(DriverError::InvalidSpec(format!(
            "VM {id} has no guest agent channel"
        )));
    }
    Ok(sock)
}

/// VM의 에이전트 잠금을 [`AGENT_LOCK_WAIT`]까지 기다려 쥔다. 얼리기 창(그 사이 녹이기 포함)만 잠금을 오래
/// 쥐므로, 넘으면 `None`(에이전트가 바쁨)이다. 작업자의 ping·정보 수집·종료가 그 창 뒤로 밀리지 않는다.
async fn agent_lock(handle: &VmHandle) -> Option<tokio::sync::MutexGuard<'_, ()>> {
    tokio::time::timeout(AGENT_LOCK_WAIT, handle.agent.lock())
        .await
        .ok()
}

fn agent_busy() -> DriverError {
    DriverError::GuestAgent("the guest agent is busy freezing or thawing file systems".into())
}

/// 게스트 에이전트로 끈다(`guest-shutdown`). 채널이 없거나, 동기화되지 않거나, 오류로 답하면 오류다.
async fn agent_shutdown(handle: &VmHandle, id: Uuid) -> Result<(), DriverError> {
    let sock = agent_socket(handle, id)?;
    let _agent = agent_lock(handle).await.ok_or_else(agent_busy)?;
    let mut session = Session::connect(&sock, qga::SYNC_TIMEOUT)
        .await
        .map_err(|e| DriverError::GuestAgent(e.to_string()))?;
    qga::shutdown(&mut session, qga::AGENT_TIMEOUT)
        .await
        .map_err(|e| DriverError::GuestAgent(e.to_string()))
}

fn image_locked(stderr: &str) -> bool {
    stderr.contains("\"write\" lock") || stderr.contains("Is another process using the image")
}

/// 끝난 마이그레이션의 결과
fn migration_result(info: MigrationInfo) -> Result<(), DriverError> {
    match info.state {
        MigrationState::Completed => Ok(()),
        MigrationState::Cancelled => Err(DriverError::Migration("migration was cancelled".into())),
        state => Err(DriverError::Migration(
            info.error
                .unwrap_or_else(|| format!("migration ended as {state:?}")),
        )),
    }
}

/// 끝난 마이그레이션의 원본이 `finish-migrate`를 벗어나기를 QMP_TIMEOUT까지 기다린다. QEMU 8.2는
/// 마이그레이션을 `completed`로 바꾼 뒤에 원본을 `postmigrate`로 바꾼다. 넘기면 경고만 남긴다
/// (마이그레이션은 끝났고 남은 원본은 주기적 정리가 끈다).
async fn wait_postmigrate(qmp: &QmpClient, id: Uuid) {
    let deadline = tokio::time::Instant::now() + QMP_TIMEOUT;
    loop {
        let status = tokio::time::timeout(QMP_TIMEOUT, qmp.query_status()).await;
        match flatten(status, "QMP query-status") {
            Ok(RunState::Other(s)) if s == "finish-migrate" => {}
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(%id, error = %e, "cannot query the migrated source");
                return;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(%id, "migrated source is still in finish-migrate");
            return;
        }
        tokio::time::sleep(POSTMIGRATE_POLL).await;
    }
}

/// 마이그레이션이 `until`인 상태가 되기를 `deadline`까지 기다린다. 넘으면 취소하고 실패다(취소하는
/// 사이에 끝났으면 그대로 성공).
async fn wait_or_cancel(
    qmp: &QmpClient,
    id: Uuid,
    deadline: tokio::time::Instant,
    timeout: Duration,
    until: impl Fn(&MigrationState) -> bool,
) -> Result<MigrationInfo, DriverError> {
    let wait = qmp.wait_migration_until(MIGRATION_POLL, until);
    match tokio::time::timeout_at(deadline, wait).await {
        Ok(r) => Ok(r?),
        Err(_) => {
            tracing::warn!(%id, ?timeout, "migration timed out; cancelling");
            let info = cancel_and_wait(qmp).await?;
            if info.state != MigrationState::Completed {
                return Err(DriverError::Migration(format!(
                    "did not complete within {timeout:?}; cancelled"
                )));
            }
            Ok(info)
        }
    }
}

/// 보내는 중인 마이그레이션을 취소하고 끝나기를 기다린다.
async fn cancel_and_wait(qmp: &QmpClient) -> Result<MigrationInfo, DriverError> {
    let cancel = tokio::time::timeout(QMP_TIMEOUT, qmp.migrate_cancel()).await;
    flatten(cancel, "QMP migrate_cancel")?;
    let wait = qmp.wait_migration(MIGRATION_POLL);
    flatten(
        tokio::time::timeout(CANCEL_WAIT, wait).await,
        "migration to cancel",
    )
}

async fn query_jobs(qmp: &QmpClient) -> Result<Vec<JobInfo>, DriverError> {
    flatten(
        tokio::time::timeout(QMP_TIMEOUT, qmp.query_jobs()).await,
        "QMP query-jobs",
    )
}

/// 미러 작업(`opl-mirror-{i}`)이 있는 디스크 순서 번호
fn mirrored_disks(jobs: &[JobInfo]) -> Vec<usize> {
    let mut disks: Vec<usize> = jobs
        .iter()
        .filter_map(|j| j.id.strip_prefix(MIRROR_JOB_PREFIX)?.parse().ok())
        .collect();
    disks.sort();
    disks
}

/// 마이그레이션에 능력 `dirty-bitmaps`가 필요한지: 이미지에 저장되지 않는 `opl-` 비트맵이 있거나, 미러하는
/// 디스크(`mirrored`, 대상이 다른 이미지다)에 `opl-` 비트맵이 있다(복제 비트맵도 덮는다).
fn migrated_bitmaps(disks: &[BlockNode], mirrored: &[usize]) -> bool {
    volatile_bitmaps(disks)
        || disks.iter().any(|d| {
            disk_index(&d.node_name).is_some_and(|i| mirrored.contains(&i))
                && d.bitmaps.iter().any(|b| b.name.starts_with(BITMAP_PREFIX))
        })
}

/// 미러 작업이 실패했다는 오류
fn mirror_failed(job: &JobInfo) -> DriverError {
    DriverError::Migration(format!(
        "disk mirror {} failed: {}",
        job.id,
        job.error.as_deref().unwrap_or("it ended early")
    ))
}

/// 디스크마다 대상 NBD 노드를 열고 미러 작업을 시작한다. 시작한 작업 id(= 노드 이름)를 돌려준다.
async fn start_mirrors(
    qmp: &QmpClient,
    targets: &[MirrorTarget],
    speed: Option<u64>,
) -> Result<Vec<String>, DriverError> {
    let mut ids = Vec::new();
    for target in targets {
        let node = format!("{MIRROR_JOB_PREFIX}{}", target.disk);
        let nbd = NbdBlockdev {
            node_name: node.clone(),
            host: target.host.clone(),
            port: target.port,
            export: target.export.clone(),
            tls_creds: NBD_TLS_ID.into(),
            tls_hostname: target.tls_hostname.clone(),
        };
        let add = tokio::time::timeout(QMP_TIMEOUT, qmp.blockdev_add_nbd(&nbd)).await;
        flatten(add, "QMP blockdev-add")?;
        let device = format!("disk{}", target.disk);
        let mirror = qmp.blockdev_mirror(&node, &device, &node, speed);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, mirror).await,
            "QMP blockdev-mirror",
        )?;
        ids.push(node);
    }
    Ok(ids)
}

/// 미러 작업 `ids`가 모두 `ready`가 되기를 기다리며 진행을 알린다. 작업이 끝났거나(오류) 사라졌거나,
/// 시간 제한을 넘었거나, 정체하면 실패다(작업은 호출자가 치운다).
async fn wait_mirrors_ready(
    qmp: &QmpClient,
    ids: &[String],
    opts: &MirrorOptions,
) -> Result<(), DriverError> {
    let started = tokio::time::Instant::now();
    let (mut most, mut moved) = (0, started);
    // 조회와 이벤트 사이에 바뀌어도 놓치지 않게 먼저 구독한다.
    let mut events = qmp.subscribe();
    loop {
        let jobs = query_jobs(qmp).await?;
        let mut progress = MirrorProgress::default();
        let mut ready = true;
        for id in ids {
            let job = match jobs.iter().find(|j| &j.id == id) {
                Some(job) if job.is_concluded() => return Err(mirror_failed(job)),
                Some(job) => job,
                None => {
                    return Err(DriverError::Migration(format!(
                        "disk mirror {id} disappeared"
                    )));
                }
            };
            progress.done_bytes += job.current_progress;
            progress.total_bytes += job.total_progress;
            ready &= job.is_ready();
        }
        if let Some(tx) = &opts.progress {
            tx.send_if_modified(|p| std::mem::replace(p, progress) != progress);
        }
        if ready {
            return Ok(());
        }
        let now = tokio::time::Instant::now();
        if progress.done_bytes > most {
            (most, moved) = (progress.done_bytes, now);
        } else if now - moved >= opts.stall {
            return Err(DriverError::Migration(format!(
                "disk copy made no progress for {}",
                format_duration(opts.stall)
            )));
        }
        if now - started >= opts.timeout {
            return Err(DriverError::Migration(format!(
                "disk copy did not finish within {}",
                format_duration(opts.timeout)
            )));
        }
        tokio::select! {
            ev = events.recv() => {
                if let Err(RecvError::Closed) = ev {
                    return Err(QmpError::Disconnected.into());
                }
            }
            _ = tokio::time::sleep(JOB_POLL) => {}
        }
    }
}

/// 스위치오버 직전(게스트가 멈춤)에 디스크 `mirrored`의 ready 미러를 끝낸다: 이 시점까지 맞춘 뒤 오류
/// 없이 끝나기를 기다리고 작업과 대상 노드를 지운다. 미러가 이미 끝났거나(오류) 오류로 끝나면 실패다.
async fn complete_mirrors(qmp: &QmpClient, mirrored: &[usize]) -> Result<(), DriverError> {
    let ids: Vec<String> = mirrored
        .iter()
        .map(|i| format!("{MIRROR_JOB_PREFIX}{i}"))
        .collect();
    let jobs = query_jobs(qmp).await?;
    for id in &ids {
        match jobs.iter().find(|j| &j.id == id) {
            Some(job) if job.is_concluded() => return Err(mirror_failed(job)),
            Some(_) => {}
            None => {
                return Err(DriverError::Migration(format!(
                    "disk mirror {id} disappeared"
                )));
            }
        }
        let cancel = tokio::time::timeout(QMP_TIMEOUT, qmp.block_job_cancel(id, false)).await;
        flatten(cancel, "QMP block-job-cancel")?;
    }
    let limit = CANCEL_WAIT * ids.len() as u32;
    let failed = match tokio::time::timeout(limit, wait_jobs(qmp, &ids)).await {
        Ok(r) => r?,
        Err(_) => return Err(DriverError::Timeout("disk mirrors to complete")),
    };
    if let Some((id, error)) = failed {
        return Err(DriverError::Migration(format!(
            "disk mirror {id} failed: {error}"
        )));
    }
    for id in &ids {
        let dismiss = tokio::time::timeout(QMP_TIMEOUT, qmp.job_dismiss(id)).await;
        flatten(dismiss, "QMP job-dismiss")?;
        let del = tokio::time::timeout(QMP_TIMEOUT, qmp.blockdev_del(id)).await;
        flatten(del, "QMP blockdev-del")?;
    }
    Ok(())
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
        .filter_map(|name| Some((disk_index(name)?, name.to_string())))
        .collect();
    disks.sort();
    disks.into_iter().map(|(_, name)| name).collect()
}

/// 이름이 `disk{i}`이면 i
fn disk_index(name: &str) -> Option<usize> {
    name.strip_prefix("disk")?.parse().ok()
}

/// qdev 경로(`/machine/peripheral/disk{i}-dev`, virtio는 그 아래 `/virtio-backend`)가 디스크 장치면 i
fn disk_device_index(qdev: &str) -> Option<usize> {
    let id = qdev
        .strip_prefix("/machine/peripheral/")?
        .split('/')
        .next()?;
    disk_index(id.strip_suffix("-dev")?)
}

/// 실행 중인 VM의 디스크 노드(`disk{i}`)를 그 비트맵과 함께 i 순서로 읽는다.
async fn query_disks(qmp: &QmpClient) -> Result<Vec<BlockNode>, DriverError> {
    let r = tokio::time::timeout(QMP_TIMEOUT, qmp.query_block_nodes()).await;
    Ok(disk_block_nodes(flatten(r, "QMP query-named-block-nodes")?))
}

/// 블록 노드 중 디스크 포맷 노드 `disk{i}`만 i 순서로 고른다.
fn disk_block_nodes(nodes: Vec<BlockNode>) -> Vec<BlockNode> {
    let mut disks: Vec<(usize, BlockNode)> = nodes
        .into_iter()
        .filter_map(|n| Some((disk_index(&n.node_name)?, n)))
        .collect();
    disks.sort_by_key(|(i, _)| *i);
    disks.into_iter().map(|(_, n)| n).collect()
}

/// 디스크에 이미지에 저장되지 않는(raw 디스크) `opl-` 비트맵이 있는지. 그런 비트맵만 마이그레이션
/// 능력 `dirty-bitmaps`로 넘긴다.
fn volatile_bitmaps(disks: &[BlockNode]) -> bool {
    disks
        .iter()
        .flat_map(|d| &d.bitmaps)
        .any(|b| b.name.starts_with(BITMAP_PREFIX) && !b.persistent)
}

/// inconsistent인 `opl-` 비트맵(QEMU가 비정상으로 끝나 이미지에 in-use로 남은 것, 복제 비트맵도 덮는다)을 지운다. 그런
/// 비트맵이 있으면 마이그레이션과 `block_resize`가 실패한다. 증분 백업은 이미 그 비트맵을 쓰지 않는다
/// (다음은 전체). 실패는 경고만 남긴다.
async fn remove_inconsistent_bitmaps(qmp: &QmpClient, id: Uuid) {
    let disks = match query_disks(qmp).await {
        Ok(disks) => disks,
        Err(e) => {
            tracing::warn!(%id, error = %e, "cannot read dirty bitmaps");
            return;
        }
    };
    for disk in &disks {
        for bitmap in &disk.bitmaps {
            if !bitmap.inconsistent || !bitmap.name.starts_with(BITMAP_PREFIX) {
                continue;
            }
            tracing::warn!(%id, node = %disk.node_name, bitmap = %bitmap.name,
                "removing inconsistent dirty bitmap");
            let remove = qmp.block_dirty_bitmap_remove(&disk.node_name, &bitmap.name);
            if let Err(e) = flatten(
                tokio::time::timeout(QMP_TIMEOUT, remove).await,
                "QMP block-dirty-bitmap-remove",
            ) {
                tracing::warn!(%id, node = %disk.node_name, bitmap = %bitmap.name, error = %e,
                    "cannot remove inconsistent dirty bitmap");
            }
        }
    }
}

/// 디스크에 비트맵 `name`을 만드는 동작. qcow2 디스크의 비트맵만 이미지에 저장한다(raw 이미지는 담을
/// 수 없다).
fn bitmap_add(disk: &BlockNode, name: &str) -> TransactionAction {
    TransactionAction::BitmapAdd {
        node: disk.node_name.clone(),
        name: name.to_string(),
        persistent: disk.driver == "qcow2",
    }
}

/// 대상 노드를 열고(연 것은 `opened`에 적는다) 새 비트맵과 백업 작업을 transaction 하나로 시작한 뒤
/// 작업이 모두 끝나기를 기다려 지운다. 작업이 실패했으면 그 오류다.
async fn backup_into(
    qmp: &QmpClient,
    disks: &[BlockNode],
    (parent, new): (&str, &str),
    targets: &[BackupTarget],
    opened: &mut Vec<String>,
    (id, freezer): (Uuid, &mut Freezer),
) -> Result<(), DriverError> {
    let mut actions = Vec::new();
    for ((i, target), disk) in targets.iter().enumerate().zip(disks) {
        if !target.path.is_absolute() {
            return Err(DriverError::InvalidSpec(format!(
                "backup target must be an absolute path: {:?}",
                target.path
            )));
        }
        let node = format!("{BACKUP_JOB_PREFIX}{i}");
        let add = qmp.blockdev_add_qcow2(&node, &target.path);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, add).await,
            "QMP blockdev-add",
        )?;
        opened.push(node.clone());
        actions.push(bitmap_add(disk, new));
        actions.push(TransactionAction::IncrementalBackup {
            job_id: node.clone(),
            device: disk.node_name.clone(),
            target: node,
            bitmap: parent.to_string(),
        });
    }
    // blockdev-backup transaction은 게스트의 진행 중인 I/O가 끝나기를 기다린다. 얼리기는 그 앞뒤로만 한다.
    freezer.freeze(id).await;
    let r = tokio::time::timeout(SNAPSHOT_TIMEOUT, qmp.transaction(&actions)).await;
    freezer.thaw(id).await;
    flatten(r, "QMP transaction")?;
    let failed = wait_jobs(qmp, opened).await?;
    // 작업을 모두 지운다. 작업이 실패했으면 지우기 오류보다 그 오류다.
    let mut dismissed = Ok(());
    for job in opened.iter() {
        let dismiss = tokio::time::timeout(QMP_TIMEOUT, qmp.job_dismiss(job)).await;
        if let Err(e) = flatten(dismiss, "QMP job-dismiss") {
            tracing::warn!(%job, error = %e, "cannot dismiss backup job");
            dismissed = dismissed.and(Err(e));
        }
    }
    match failed {
        Some((id, error)) => Err(QmpError::JobFailed { id, error }.into()),
        None => dismissed,
    }
}

/// 작업 `ids`가 모두 끝나기(`concluded`)를 기다린다. 하나라도 실패하면 아직 도는 나머지를 취소한다.
/// 먼저 실패한 작업의 (id, 오류)를 돌려준다(취소한 작업도 오류로 끝난다). 시간 제한은 없다(디스크
/// 크기만큼 걸린다). QEMU가 답하지 않으면 그 오류다.
async fn wait_jobs(
    qmp: &QmpClient,
    ids: &[String],
) -> Result<Option<(String, String)>, DriverError> {
    // 조회와 이벤트 사이에 끝나도 놓치지 않게 먼저 구독한다.
    let mut events = qmp.subscribe();
    let mut failed: Option<(String, String)> = None;
    let mut cancelled = false;
    loop {
        let jobs = tokio::time::timeout(QMP_TIMEOUT, qmp.query_jobs()).await;
        let jobs = flatten(jobs, "QMP query-jobs")?;
        let mut running = Vec::new();
        for id in ids {
            let error = match jobs.iter().find(|j| &j.id == id) {
                Some(job) if !job.is_concluded() => {
                    running.push(id);
                    continue;
                }
                Some(job) => job.error.clone(),
                None => Some("job disappeared".to_string()),
            };
            if let (Some(error), None) = (error, &failed) {
                failed = Some((id.clone(), error));
            }
        }
        if running.is_empty() {
            return Ok(failed);
        }
        if failed.is_some() && !cancelled {
            cancelled = true;
            for id in running {
                let cancel = tokio::time::timeout(QMP_TIMEOUT, qmp.job_cancel(id)).await;
                if let Err(e) = flatten(cancel, "QMP job-cancel") {
                    tracing::warn!(job = id, error = %e, "cannot cancel backup job");
                }
            }
        }
        tokio::select! {
            ev = events.recv() => {
                if let Err(RecvError::Closed) = ev {
                    return Err(QmpError::Disconnected.into());
                }
            }
            _ = tokio::time::sleep(JOB_POLL) => {}
        }
    }
}

/// 앞선 증분 백업이 남긴 작업과 대상 노드(opld가 기다리다 죽었다)와 남은 디스크 미러, 복제 회차와 마이그레이션
/// 따라잡기의 작업과 NBD 노드를 치우고, 남은 마이그레이션 따라잡기 비트맵을 지운다.
async fn clear_backup_leftovers(qmp: &QmpClient) -> Result<(), DriverError> {
    clear_jobs(
        qmp,
        &[
            BACKUP_JOB_PREFIX,
            MIRROR_JOB_PREFIX,
            REPLICA_JOB_PREFIX,
            CATCH_UP_PREFIX,
        ],
    )
    .await?;
    remove_bitmaps_where(qmp, |name| name.starts_with(MIGRATION_BITMAP_PREFIX)).await
}

/// 모든 디스크에서 이름이 `which`에 맞는 dirty bitmap을 지운다.
async fn remove_bitmaps_where(
    qmp: &QmpClient,
    which: impl Fn(&str) -> bool,
) -> Result<(), DriverError> {
    for disk in query_disks(qmp).await? {
        for bitmap in disk.bitmaps.iter().filter(|b| which(&b.name)) {
            let remove = qmp.block_dirty_bitmap_remove(&disk.node_name, &bitmap.name);
            qmp_call(remove, "QMP block-dirty-bitmap-remove").await?;
        }
    }
    Ok(())
}

/// 플리싱을 붙인 디스크 하나. 붙인 단계까지 적어 두고 떼기는 그 반대 순서다.
struct Fleece {
    disk: usize,
    /// 디스크 노드(`disk{i}`)
    node: String,
    /// 디스크 장치(`drive` 속성을 가진 qdev 경로)
    qdev: String,
    /// 플리싱 이미지(VM 상태 디렉터리의 성긴 파일)
    path: PathBuf,
    /// 플리싱 이미지 노드를 열었다.
    tmp: bool,
    /// copy-before-write 필터를 열었다.
    cbw: bool,
    /// 장치가 필터를 쓴다.
    swapped: bool,
    /// snapshot-access 노드를 열었다.
    access: bool,
}

/// 플리싱을 붙이는 동안 게스트를 멈추는 VM([`QemuHypervisor::fleece_halt`]): 멈춘 동안 opld가 죽어도 다시 붙을 때
/// 이어서 실행하게 두는 표시([`PAUSED_MARK`])와, 그동안 지금 멈춰 둔 VM으로 넣어 두는 드라이버의 `pausing`
/// (다시 붙기·`resume_left_stopped`가 필터를 끼우는 중에 이어서 실행하지 않게)
struct FleeceHalt<'a> {
    id: Uuid,
    mark: PathBuf,
    pausing: &'a Mutex<HashSet<Uuid>>,
}

/// 디스크 `disk`의 플리싱 노드 (플리싱 이미지, copy-before-write 필터, snapshot-access)
fn fleece_nodes(disk: usize) -> (String, String, String) {
    (
        format!("{FLEECE_PREFIX}tmp-{disk}"),
        format!("{FLEECE_PREFIX}cbw-{disk}"),
        format!("{FLEECE_PREFIX}acc-{disk}"),
    )
}

/// 디스크 `disk`의 플리싱 이미지 파일
fn fleece_path(dir: &Path, disk: usize) -> PathBuf {
    dir.join(format!("fleece-{disk}.img"))
}

/// 장치에 붙은 블록 노드(`query-block`)마다 (맨 위 노드, 장치의 qdev 경로, 가상 크기)
async fn block_devices(qmp: &QmpClient) -> Result<Vec<(String, String, u64)>, DriverError> {
    let r = qmp_call(qmp.execute("query-block", None), "QMP query-block").await?;
    Ok(r.as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let inserted = b.get("inserted")?;
            Some((
                inserted["node-name"].as_str()?.to_string(),
                b["qdev"].as_str()?.to_string(),
                inserted["image"]["virtual-size"].as_u64().unwrap_or(0),
            ))
        })
        .collect())
}

/// 디스크 `indexes`에 플리싱을 붙인다(붙인 것은 `fleeces`에 적는다 — 실패해도 호출자가 [`stop_fleecing`]으로 뗀다).
/// 디스크마다 VM 상태 디렉터리 `dir`에 디스크 크기의 성긴 플리싱 이미지를 열고, 게스트를 잠시 멈춘 채(모든 디스크가
/// 같은 시점 — crash-consistent; `halt` — 멈춘 동안 opld가 죽어도 다시 붙을 때 이어서 실행하게 표시를 두고, 그동안
/// 다시 붙기·이어서 실행하기가 끼어들지 않게 지금 멈춰 둔 VM으로 둔다) 새 비트맵
/// `new`를 transaction으로 만들고 디스크 위에 copy-before-write 필터를 끼워 장치가 그 필터를 쓰게 한 뒤, 그 시점의
/// 디스크를 읽는 snapshot-access 노드를 연다. 필터는 게스트가 덮어쓰려는 옛 블록을 먼저 로컬 플리싱 이미지에 옮기고,
/// 옮기지 못하면(`break-snapshot`, `cbw-timeout`) 게스트 쓰기는 그대로 하고 시점만 깨 그 회차가 실패한다. 그래서 원격
/// 쓰기(NBD)가 실패해도 게스트 쓰기는 실패하지 않는다(백업 작업이 디스크에서 바로 읽으면 그 작업이 끼우는 필터가
/// 원격 쓰기 실패를 게스트 쓰기 오류로 돌려준다). 남은 공간이 디스크 크기 합의 [`FLEECE_SPACE_PERCENT`]%보다 적으면
/// `error` 오류다.
#[allow(clippy::too_many_arguments)]
async fn start_fleecing(
    qmp: &QmpClient,
    disks: &[BlockNode],
    indexes: &[usize],
    new: &[TransactionAction],
    (dir, halt, freezer): (&Path, &FleeceHalt<'_>, &mut Freezer),
    error: fn(String) -> DriverError,
    fleeces: &mut Vec<Fleece>,
) -> Result<(), DriverError> {
    let devices = block_devices(qmp).await?;
    let mut found = Vec::new();
    for &i in indexes {
        let node = &disks[i].node_name;
        let Some((_, qdev, size)) = devices.iter().find(|(root, _, _)| root == node) else {
            return Err(error(format!("disk{i} is not attached to a device")));
        };
        found.push((i, node.clone(), qdev.clone(), *size));
    }
    std::fs::create_dir_all(dir)?;
    check_fleece_space(dir, found.iter().map(|f| f.3).sum()).map_err(error)?;
    for (disk, node, qdev, size) in found {
        let path = fleece_path(dir, disk);
        // 성긴 파일이라 필터가 옮긴 블록만큼만 자란다.
        std::fs::File::create(&path)?.set_len(size)?;
        fleeces.push(Fleece {
            disk,
            node,
            qdev,
            path: path.clone(),
            tmp: false,
            cbw: false,
            swapped: false,
            access: false,
        });
        let (tmp, _, _) = fleece_nodes(disk);
        let add = qmp.blockdev_add(json!({
            "driver": "file", "node-name": tmp, "filename": path.display().to_string(),
        }));
        qmp_call(add, "QMP blockdev-add").await?;
        if let Some(f) = fleeces.last_mut() {
            f.tmp = true;
        }
    }
    let running = qmp_call(qmp.query_status(), "QMP query-status").await? == RunState::Running;
    // 게스트를 멈추기 전에 얼린다(멈춘 게스트의 에이전트는 답하지 않는다). 이어서 실행한 뒤에 녹인다.
    if running {
        freezer.freeze(halt.id).await;
    }
    let inserted = halt_and_insert(qmp, new, halt, running, fleeces).await;
    freezer.thaw(halt.id).await;
    inserted?;
    for f in fleeces.iter_mut() {
        let (_, cbw, access) = fleece_nodes(f.disk);
        let add = qmp.blockdev_add(json!({
            "driver": "snapshot-access", "node-name": access, "file": cbw,
        }));
        qmp_call(add, "QMP blockdev-add").await?;
        f.access = true;
    }
    Ok(())
}

/// 실행 중이면(`running`) 게스트를 멈추고 필터를 끼운 뒤 이어서 실행한다([`start_fleecing`]).
async fn halt_and_insert(
    qmp: &QmpClient,
    new: &[TransactionAction],
    halt: &FleeceHalt<'_>,
    running: bool,
    fleeces: &mut [Fleece],
) -> Result<(), DriverError> {
    if running {
        halt.pausing.lock().unwrap().insert(halt.id);
        let stopped = match std::fs::write(&halt.mark, b"") {
            Ok(()) => qmp_call(qmp.stop(), "QMP stop").await,
            Err(e) => Err(e.into()),
        };
        if let Err(e) = stopped {
            halt.pausing.lock().unwrap().remove(&halt.id);
            remove_if_exists(&halt.mark)?;
            return Err(e);
        }
    }
    let inserted = insert_filters(qmp, new, fleeces).await;
    if running {
        let continued = qmp_call(qmp.cont(), "QMP cont").await;
        // 이어서 실행하지 못했으면 표시가 남아 다시 붙을 때(`start`) 이어서 실행한다.
        halt.pausing.lock().unwrap().remove(&halt.id);
        match continued {
            Ok(()) => remove_if_exists(&halt.mark)?,
            Err(e) => {
                tracing::error!(error = %e, "cannot continue the VM after inserting the fleecing filters");
                inserted?;
                return Err(e);
            }
        }
    }
    inserted
}

/// 디스크 크기 합이 `bytes`인 플리싱 이미지를 둘 `dir`의 남은 공간이 그 [`FLEECE_SPACE_PERCENT`]% 이상인지 본다.
fn check_fleece_space(dir: &Path, bytes: u64) -> Result<(), String> {
    let need = bytes / 100 * FLEECE_SPACE_PERCENT;
    let st = rustix::fs::statvfs(dir)
        .map_err(|e| format!("cannot read the free space of {}: {e}", dir.display()))?;
    let available = st.f_bavail.saturating_mul(st.f_frsize);
    if available < need {
        return Err(format!(
            "not enough space in {} for the fleecing images: need {need}, available {available}",
            dir.display()
        ));
    }
    Ok(())
}

/// (게스트가 멈춘 동안) 새 비트맵을 만들고 디스크마다 copy-before-write 필터를 끼워 장치가 그것을 쓰게 한다.
async fn insert_filters(
    qmp: &QmpClient,
    new: &[TransactionAction],
    fleeces: &mut [Fleece],
) -> Result<(), DriverError> {
    if !new.is_empty() {
        let r = tokio::time::timeout(SNAPSHOT_TIMEOUT, qmp.transaction(new)).await;
        flatten(r, "QMP transaction")?;
    }
    for f in fleeces.iter_mut() {
        let (tmp, cbw, _) = fleece_nodes(f.disk);
        let add = qmp.blockdev_add(json!({
            "driver": "copy-before-write", "node-name": cbw, "file": f.node, "target": tmp,
            "on-cbw-error": "break-snapshot", "cbw-timeout": CBW_TIMEOUT_SECS,
        }));
        qmp_call(add, "QMP blockdev-add").await?;
        f.cbw = true;
        qmp_call(qmp.qom_set(&f.qdev, "drive", json!(cbw)), "QMP qom-set").await?;
        f.swapped = true;
    }
    Ok(())
}

/// [`start_fleecing`]이 붙인 것을 반대 순서로 떼고 플리싱 이미지를 지운다(그 노드를 읽던 작업은 이미 지웠다). 떼지
/// 못한 디스크는 남기고(다음 회차나 남은 것 치우기가 [`clear_fleecing`]으로 뗀다) 먼저 생긴 오류를 돌려준다.
async fn stop_fleecing(qmp: &QmpClient, fleeces: &mut Vec<Fleece>) -> Result<(), DriverError> {
    let mut result = Ok(());
    for f in fleeces.drain(..).rev() {
        if let Err(e) = unfleece(qmp, &f).await {
            tracing::warn!(disk = f.disk, error = %e, "cannot remove the fleecing of a disk");
            result = result.and(Err(e));
        }
    }
    result
}

async fn unfleece(qmp: &QmpClient, f: &Fleece) -> Result<(), DriverError> {
    let (tmp, cbw, access) = fleece_nodes(f.disk);
    if f.access {
        qmp_call(qmp.blockdev_del(&access), "QMP blockdev-del").await?;
    }
    if f.swapped {
        let back = qmp.qom_set(&f.qdev, "drive", json!(f.node));
        qmp_call(back, "QMP qom-set").await?;
    }
    if f.cbw {
        qmp_call(qmp.blockdev_del(&cbw), "QMP blockdev-del").await?;
    }
    if f.tmp {
        qmp_call(qmp.blockdev_del(&tmp), "QMP blockdev-del").await?;
    }
    remove_if_exists(&f.path)?;
    Ok(())
}

/// 앞선 회차가 남긴 플리싱(opld가 기다리다 죽었다)을 뗀다: 장치가 아직 필터를 쓰면 디스크 노드로 되돌리고 `opl-fl-*`
/// 노드를 닫은 뒤 `dir`의 플리싱 이미지 파일을 지운다. 그 노드를 읽던 작업은 먼저 치웠어야 한다.
async fn clear_fleecing(qmp: &QmpClient, dir: &Path) -> Result<(), DriverError> {
    let nodes = qmp_call(qmp.query_block_nodes(), "QMP query-named-block-nodes").await?;
    let left: Vec<String> = nodes
        .into_iter()
        .map(|n| n.node_name)
        .filter(|n| n.starts_with(FLEECE_PREFIX))
        .collect();
    if !left.is_empty() {
        tracing::warn!(nodes = ?left, "removing the fleecing left by an earlier run");
        let filter = format!("{FLEECE_PREFIX}cbw-");
        for (root, qdev, _) in block_devices(qmp).await? {
            if let Some(i) = root
                .strip_prefix(&filter)
                .and_then(|i| i.parse::<usize>().ok())
            {
                let back = qmp.qom_set(&qdev, "drive", json!(format!("disk{i}")));
                qmp_call(back, "QMP qom-set").await?;
            }
        }
        for kind in ["acc-", "cbw-", "tmp-"] {
            let prefix = format!("{FLEECE_PREFIX}{kind}");
            for node in left.iter().filter(|n| n.starts_with(&prefix)) {
                qmp_call(qmp.blockdev_del(node), "QMP blockdev-del").await?;
            }
        }
    }
    remove_fleece_files(dir)?;
    Ok(())
}

/// VM 상태 디렉터리 `dir`에 남은 플리싱 이미지 파일을 지운다.
fn remove_fleece_files(dir: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        r => r?,
    };
    for entry in entries {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("fleece-") && name.ends_with(".img") {
            remove_if_exists(&dir.join(&*name))?;
        }
    }
    Ok(())
}

/// 디스크 노드 `node`의 비트맵 `base`를 snapshot-access 노드 `access`의 [`FLEECE_BITMAP`]에 옮겨 담는다(회차 시점 뒤의
/// 쓰기까지 담겨 그 시점까지 바뀐 블록의 상위 집합이다).
async fn copy_bitmap(
    qmp: &QmpClient,
    node: &str,
    base: &str,
    access: &str,
) -> Result<(), DriverError> {
    let add = qmp.block_dirty_bitmap_add(access, FLEECE_BITMAP, false);
    qmp_call(add, "QMP block-dirty-bitmap-add").await?;
    let merge = qmp.execute(
        "block-dirty-bitmap-merge",
        Some(json!({
            "node": access, "target": FLEECE_BITMAP, "bitmaps": [{ "node": node, "name": base }],
        })),
    );
    qmp_call(merge, "QMP block-dirty-bitmap-merge")
        .await
        .map(drop)
}

/// 대상마다 NBD 노드를 열고(연 것은 `opened`에 적는다), 디스크마다 플리싱을 붙여(붙인 것은 `fleeces`에 적는다 —
/// 그 순간이 회차의 시점이고 새 비트맵도 그때 만든다; `fleece`는 플리싱 이미지 디렉터리와 멈추는 VM) 그 시점의
/// 디스크를 보내는 백업 작업을 transaction 하나로 시작한 뒤 작업이 모두 끝나기를 기다려 지운다. 보낸 바이트를
/// 돌려준다.
async fn send_replicas(
    qmp: &QmpClient,
    disks: &[BlockNode],
    req: &ReplicateRequest,
    fleece: (&Path, &FleeceHalt<'_>, &mut Freezer),
    opened: &mut Vec<String>,
    fleeces: &mut Vec<Fleece>,
) -> Result<u64, DriverError> {
    let mut actions = Vec::new();
    let mut jobs = Vec::new();
    for t in &req.targets {
        let node = format!("{REPLICA_NBD_PREFIX}{}", t.disk);
        let nbd = NbdBlockdev {
            node_name: node.clone(),
            host: t.host.clone(),
            port: t.port,
            export: t.export.clone(),
            tls_creds: REPLICA_TLS_ID.into(),
            tls_hostname: t.tls_hostname.clone(),
        };
        qmp_call(qmp.blockdev_add_nbd(&nbd), "QMP blockdev-add").await?;
        opened.push(node);
    }
    let new: Vec<TransactionAction> = req
        .targets
        .iter()
        .map(|t| bitmap_add(&disks[t.disk], &req.bitmap))
        .collect();
    let indexes: Vec<usize> = req.targets.iter().map(|t| t.disk).collect();
    let (dir, halt, freezer) = fleece;
    let error = DriverError::Replication;
    start_fleecing(
        qmp,
        disks,
        &indexes,
        &new,
        (dir, halt, freezer),
        error,
        fleeces,
    )
    .await?;
    for t in &req.targets {
        let (_, _, access) = fleece_nodes(t.disk);
        let sync = match &t.base {
            Some(base) => {
                copy_bitmap(qmp, &disks[t.disk].node_name, base, &access).await?;
                BackupSync::Bitmap(FLEECE_BITMAP.into())
            }
            None => BackupSync::Full,
        };
        let job = format!("{REPLICA_JOB_PREFIX}{}", t.disk);
        actions.push(TransactionAction::Backup {
            job_id: job.clone(),
            device: access,
            target: format!("{REPLICA_NBD_PREFIX}{}", t.disk),
            sync,
            speed: req.speed,
        });
        jobs.push(job);
    }
    // blockdev-backup transaction은 게스트의 진행 중인 I/O가 끝나기를 기다린다.
    let r = tokio::time::timeout(SNAPSHOT_TIMEOUT, qmp.transaction(&actions)).await;
    flatten(r, "QMP transaction")?;
    let progress = req.progress.as_ref();
    let limits = Limits {
        stall: req.stall,
        deadline: None,
        fleece: dir,
        reserve: fleece_reserve,
    };
    let sent = wait_sent(
        qmp,
        &jobs,
        &limits,
        progress,
        MirrorProgress::default(),
        &REPLICATION,
    )
    .await;
    // 작업을 모두 지운다. 회차가 실패했으면 지우기 오류보다 그 오류다(남은 작업은 다음 회차가 치운다).
    let mut dismissed = Ok(());
    for job in &jobs {
        if let Err(e) = qmp_call(qmp.job_dismiss(job), "QMP job-dismiss").await {
            tracing::warn!(%job, error = %e, "cannot dismiss replication job");
            dismissed = dismissed.and(Err(e));
        }
    }
    let sent = sent?;
    dismissed.map(|()| sent)
}

/// 대상에 블록을 보내는 작업(복제 회차, 마이그레이션 따라잡기)의 오류 문구와 종류
struct Sending {
    /// 오류 문구의 이름(`{what} job … failed`, `{what} made no progress …`)
    what: &'static str,
    error: fn(String) -> DriverError,
}

const REPLICATION: Sending = Sending {
    what: "replication",
    error: DriverError::Replication,
};

const CATCHING_UP: Sending = Sending {
    what: "disk copy",
    error: DriverError::Migration,
};

/// 보내는 작업을 기다리는 동안 지키는 한도([`wait_sent`])
struct Limits<'a> {
    /// 보낸 양이 이만큼 늘지 않으면 실패한다(끊긴 링크).
    stall: Duration,
    /// 이때까지 끝나지 않으면 실패한다(기한과 그 길이 — 오류 문구).
    deadline: Option<(tokio::time::Instant, Duration)>,
    /// 플리싱 이미지 디렉터리: 그 파일 시스템의 남은 공간이 `reserve(파일 시스템 크기)`보다 적어지면 실패한다.
    fleece: &'a Path,
    reserve: fn(u64) -> u64,
}

/// 크기가 `total`인 파일 시스템에서 보내는 동안 남겨 둘 공간([`FLEECE_RESERVE_BYTES`])
fn fleece_reserve(total: u64) -> u64 {
    FLEECE_RESERVE_BYTES.max(total / 100 * FLEECE_RESERVE_PERCENT)
}

/// 플리싱 이미지 디렉터리 `dir`의 남은 공간이 `reserve(파일 시스템 크기)` 이상인지 본다. 아니면 이유다.
fn fleece_space_left(dir: &Path, reserve: fn(u64) -> u64) -> Result<(), String> {
    let st = rustix::fs::statvfs(dir)
        .map_err(|e| format!("cannot read the free space of {}: {e}", dir.display()))?;
    let total = st.f_blocks.saturating_mul(st.f_frsize);
    let available = st.f_bavail.saturating_mul(st.f_frsize);
    let keep = reserve(total);
    if available < keep {
        return Err(format!(
            "not enough space left in {} for the fleecing images: available {available}, keeping {keep}",
            dir.display()
        ));
    }
    Ok(())
}

/// 보내는 작업 `ids`가 모두 끝나기를 기다리며 진행(`before`에 더해)을 알리고 이 작업들이 보낸 바이트(`current-progress`
/// 합)를 돌려준다. 하나라도 실패하거나, `limits.stall` 동안 보낸 양이 늘지 않거나, 기한이 지나거나, 플리싱 이미지를
/// 둘 공간이 모자라면 아직 도는 나머지를 취소하고, 끝나기를 [`CANCEL_WAIT`]까지 기다린 뒤 먼저 생긴 오류로 실패한다.
async fn wait_sent(
    qmp: &QmpClient,
    ids: &[String],
    limits: &Limits<'_>,
    progress_tx: Option<&watch::Sender<MirrorProgress>>,
    before: MirrorProgress,
    how: &Sending,
) -> Result<u64, DriverError> {
    let what = how.what;
    // 조회와 이벤트 사이에 끝나도 놓치지 않게 먼저 구독한다.
    let mut events = qmp.subscribe();
    let (mut most, mut moved) = (0, tokio::time::Instant::now());
    let mut failed: Option<DriverError> = None;
    let mut cancelled: Option<tokio::time::Instant> = None;
    loop {
        let jobs = query_jobs(qmp).await?;
        let mut progress = MirrorProgress::default();
        let mut running = Vec::new();
        for id in ids {
            let Some(job) = jobs.iter().find(|j| &j.id == id) else {
                failed.get_or_insert_with(|| (how.error)(format!("{what} job {id} disappeared")));
                continue;
            };
            progress.done_bytes += job.current_progress;
            progress.total_bytes += job.total_progress;
            if !job.is_concluded() {
                running.push(id);
            } else if let Some(error) = &job.error {
                failed
                    .get_or_insert_with(|| (how.error)(format!("{what} job {id} failed: {error}")));
            }
        }
        if let Some(tx) = progress_tx {
            let total = MirrorProgress {
                done_bytes: before.done_bytes + progress.done_bytes,
                total_bytes: before.total_bytes + progress.total_bytes,
            };
            tx.send_if_modified(|p| std::mem::replace(p, total) != total);
        }
        if running.is_empty() {
            return match failed {
                Some(e) => Err(e),
                None => Ok(progress.done_bytes),
            };
        }
        let now = tokio::time::Instant::now();
        let stall = limits.stall;
        if progress.done_bytes > most {
            (most, moved) = (progress.done_bytes, now);
        } else if now - moved >= stall {
            failed.get_or_insert_with(|| {
                (how.error)(format!(
                    "{what} made no progress for {}",
                    format_duration(stall)
                ))
            });
        }
        if let Some((at, limit)) = limits.deadline
            && now >= at
        {
            failed.get_or_insert_with(|| {
                (how.error)(format!(
                    "{what} did not finish within {}",
                    format_duration(limit)
                ))
            });
        }
        if failed.is_none()
            && let Err(why) = fleece_space_left(limits.fleece, limits.reserve)
        {
            failed = Some((how.error)(why));
        }
        if let Some(e) = &failed {
            match cancelled {
                None => {
                    tracing::warn!(error = %e, jobs = ?running, "cancelling {what} jobs");
                    for id in &running {
                        if let Err(e) = qmp_call(qmp.job_cancel(id), "QMP job-cancel").await {
                            tracing::warn!(job = %id, error = %e, "cannot cancel {what} job");
                        }
                    }
                    cancelled = Some(now);
                }
                Some(at) if now - at >= CANCEL_WAIT => return Err(failed.unwrap()),
                Some(_) => {}
            }
        }
        tokio::select! {
            ev = events.recv() => {
                if let Err(RecvError::Closed) = ev {
                    return Err(QmpError::Disconnected.into());
                }
            }
            _ = tokio::time::sleep(JOB_POLL) => {}
        }
    }
}

/// 따라잡기 디스크 `disk`의 작업 id와 NBD 대상 노드 이름
fn catch_up_names(disk: usize) -> (String, String) {
    (
        format!("{CATCH_UP_PREFIX}{disk}"),
        format!("{CATCH_UP_NBD_PREFIX}{disk}"),
    )
}

/// 따라잡기 패스를 플리싱으로 할 때의 (플리싱 이미지 디렉터리, 멈추는 VM, 붙인 플리싱)
type CatchUpFleece<'a> = (&'a Path, &'a FleeceHalt<'a>, &'a mut Vec<Fleece>);

/// 따라잡기 패스 하나의 작업들을 시작한다: 디스크마다(`new`가 있으면 새 임시 비트맵을 만들고) `blockdev-backup`(기준
/// 비트맵이 있으면 그 뒤 바뀐 블록, 없으면 전체)을 연 NBD 노드로 시작한다. `fleece`가 있으면(게스트가 도는 패스) 복제
/// 회차처럼 플리싱을 붙여 그 시점의 디스크를 보내고(원격 쓰기가 실패해도 게스트 쓰기는 성공한다), 없으면(게스트가
/// 멈춘 마지막 패스) transaction 하나로 디스크에서 바로 보낸다. 작업 id를 돌려준다.
async fn start_catch_up_pass(
    qmp: &QmpClient,
    disks: &[BlockNode],
    passes: &[(usize, Option<String>)],
    new: Option<&str>,
    speed: Option<u64>,
    fleece: Option<CatchUpFleece<'_>>,
) -> Result<Vec<String>, DriverError> {
    let mut actions = Vec::new();
    let mut jobs = Vec::new();
    // 패스 사이에만 쓰는 비트맵이라 이미지에 저장하지 않는다.
    let new: Vec<TransactionAction> = new
        .into_iter()
        .flat_map(|new| {
            passes
                .iter()
                .map(move |(i, _)| TransactionAction::BitmapAdd {
                    node: disks[*i].node_name.clone(),
                    name: new.to_string(),
                    persistent: false,
                })
        })
        .collect();
    let mut sources = Vec::new();
    match fleece {
        Some((dir, halt, fleeces)) => {
            let indexes: Vec<usize> = passes.iter().map(|(i, _)| *i).collect();
            let error = DriverError::Migration;
            let fleece = (dir, halt, &mut Freezer::off());
            start_fleecing(qmp, disks, &indexes, &new, fleece, error, fleeces).await?;
            for (i, base) in passes {
                let (_, _, access) = fleece_nodes(*i);
                let sync = match base {
                    Some(base) => {
                        copy_bitmap(qmp, &disks[*i].node_name, base, &access).await?;
                        BackupSync::Bitmap(FLEECE_BITMAP.into())
                    }
                    None => BackupSync::Full,
                };
                sources.push((*i, access, sync));
            }
        }
        None => {
            actions.extend(new);
            for (i, base) in passes {
                let sync = match base {
                    Some(base) => BackupSync::Bitmap(base.clone()),
                    None => BackupSync::Full,
                };
                sources.push((*i, disks[*i].node_name.clone(), sync));
            }
        }
    }
    for (i, device, sync) in sources {
        let (job, node) = catch_up_names(i);
        actions.push(TransactionAction::Backup {
            job_id: job.clone(),
            device,
            target: node,
            sync,
            speed,
        });
        jobs.push(job);
    }
    // blockdev-backup transaction은 게스트의 진행 중인 I/O가 끝나기를 기다린다.
    let r = tokio::time::timeout(SNAPSHOT_TIMEOUT, qmp.transaction(&actions)).await;
    flatten(r, "QMP transaction")?;
    Ok(jobs)
}

/// 끝난 작업들을 지운다. 지우지 못하면 오류다(남은 작업은 다음 따라잡기가 치운다).
async fn dismiss_jobs(qmp: &QmpClient, jobs: &[String]) -> Result<(), DriverError> {
    let mut dismissed = Ok(());
    for job in jobs {
        if let Err(e) = qmp_call(qmp.job_dismiss(job), "QMP job-dismiss").await {
            tracing::warn!(%job, error = %e, "cannot dismiss a disk copy job");
            dismissed = dismissed.and(Err(e));
        }
    }
    dismissed
}

/// [`Hypervisor::catch_up_disks`]의 패스들(NBD 노드는 열려 있다). 패스마다 플리싱(`fleece` — 플리싱 이미지 디렉터리와
/// 멈추는 VM)을 붙였다 뗀다. 기한(`deadline` — 시각과 그 길이)이 지나면 그 패스의 작업을 취소하고 플리싱을 뗀 뒤
/// 실패한다.
async fn catch_up_passes(
    qmp: &QmpClient,
    disks: &[BlockNode],
    req: &CatchUp,
    fleece: (&Path, &FleeceHalt<'_>),
    deadline: (tokio::time::Instant, Duration),
) -> Result<CatchUpState, DriverError> {
    let indexes: Vec<usize> = req.targets.iter().map(|t| t.disk).collect();
    let mut passes: Vec<(usize, Option<String>)> = indexes
        .iter()
        .copied()
        .zip(req.base.iter().cloned())
        .collect();
    let mut sent = MirrorProgress::default();
    for k in 1..=req.passes {
        let new = migration_bitmap(req.task, k);
        let mut fleeces = Vec::new();
        let (dir, halt) = fleece;
        let started = start_catch_up_pass(
            qmp,
            disks,
            &passes,
            Some(&new),
            req.speed,
            Some((dir, halt, &mut fleeces)),
        )
        .await;
        let jobs = match started {
            Ok(jobs) => jobs,
            Err(e) => {
                if let Err(stop) = stop_fleecing(qmp, &mut fleeces).await {
                    tracing::warn!(error = %stop, "cannot remove the fleecing");
                }
                return Err(e);
            }
        };
        let limits = Limits {
            stall: req.stall,
            deadline: Some(deadline),
            fleece: dir,
            reserve: fleece_reserve,
        };
        let waited = wait_sent(
            qmp,
            &jobs,
            &limits,
            req.progress.as_ref(),
            sent,
            &CATCHING_UP,
        )
        .await;
        let dismissed = dismiss_jobs(qmp, &jobs).await;
        let unfleeced = stop_fleecing(qmp, &mut fleeces).await;
        let bytes = waited?;
        dismissed?;
        unfleeced?;
        sent.done_bytes += bytes;
        sent.total_bytes += bytes;
        // 앞 패스의 임시 비트맵은 이제 쓸모가 없다(첫 패스의 복제 비트맵은 확정 전까지 둔다).
        if k > 1 {
            let old = migration_bitmap(req.task, k - 1);
            remove_bitmaps_where(qmp, |name| name == old).await?;
        }
        let now = query_disks(qmp).await?;
        let dirty: u64 = now
            .iter()
            .filter(|d| disk_index(&d.node_name).is_some_and(|i| indexes.contains(&i)))
            .flat_map(|d| d.bitmaps.iter().filter(|b| b.name == new))
            .map(|b| b.count)
            .sum();
        tracing::info!(pass = k, bytes, dirty, "caught up the disks once");
        if dirty <= req.limit || k == req.passes {
            return Ok(CatchUpState {
                disks: indexes,
                bitmap: new,
                passes: k,
            });
        }
        for (_, base) in &mut passes {
            *base = Some(new.clone());
        }
    }
    Err(DriverError::InvalidSpec("no catch-up pass to run".into()))
}

/// 스위치오버 직전(게스트가 멈춤)의 마지막 따라잡기 패스: 따라잡은 디스크마다 마지막 비트맵 뒤 바뀐 블록을 보내고
/// 작업을 지운 뒤 NBD 노드를 닫는다(닫을 때 대상에 쓴 것을 내보내므로 닫지 못하면 실패다). 작업이 실패하면 그 오류다.
async fn last_catch_up(qmp: &QmpClient, state: &CatchUpState) -> Result<(), DriverError> {
    let disks = query_disks(qmp).await?;
    if let Some(i) = state.disks.iter().find(|i| **i >= disks.len()) {
        return Err(DriverError::Migration(format!("VM has no disk{i}")));
    }
    let passes: Vec<(usize, Option<String>)> = state
        .disks
        .iter()
        .map(|i| (*i, Some(state.bitmap.clone())))
        .collect();
    let jobs = start_catch_up_pass(qmp, &disks, &passes, None, None, None).await?;
    let failed = wait_jobs(qmp, &jobs).await?;
    let dismissed = dismiss_jobs(qmp, &jobs).await;
    if let Some((id, error)) = failed {
        return Err(DriverError::Migration(format!(
            "disk copy job {id} failed: {error}"
        )));
    }
    dismissed?;
    for i in &state.disks {
        let (_, node) = catch_up_names(*i);
        let del = tokio::time::timeout(SNAPSHOT_TIMEOUT, qmp.blockdev_del(&node)).await;
        flatten(del, "QMP blockdev-del")?;
    }
    Ok(())
}

/// 받는 QEMU에 TLS 자격 증명(받는 쪽, 상대 인증서 확인)을 만들고, 복제본마다 블록 노드(`opl-repl-{i}`)를 연
/// 뒤 `ports`의 빈 포트에서 TLS NBD 서버를 띄워 그 노드들을 이름 `export.name`으로 쓰기 가능하게 내보낸다.
/// 연 포트를 돌려준다.
async fn serve_replicas(
    qmp: &QmpClient,
    tls_dir: &Path,
    exports: &[ReceiverExport],
    host: &str,
    ports: RangeInclusive<u16>,
) -> Result<u16, DriverError> {
    let add = qmp.add_tls_creds(REPLICA_TLS_ID, tls_dir, TlsEndpoint::Server);
    qmp_call(add, "QMP object-add").await?;
    for (i, export) in exports.iter().enumerate() {
        let args = receiver_blockdev(&format!("{RECEIVER_NODE_PREFIX}{i}"), export)?;
        qmp_call(qmp.blockdev_add(args), "QMP blockdev-add").await?;
    }
    let port = pick_port(host, ports.clone(), &[]).ok_or_else(|| {
        DriverError::Replication(format!(
            "no free port in {}-{} on {host}",
            ports.start(),
            ports.end()
        ))
    })?;
    let start = qmp.nbd_server_start(host, port, REPLICA_TLS_ID);
    qmp_call(start, "QMP nbd-server-start").await?;
    for (i, export) in exports.iter().enumerate() {
        let (id, node) = (
            format!("{RECEIVER_EXPORT_PREFIX}{i}"),
            format!("{RECEIVER_NODE_PREFIX}{i}"),
        );
        let add = qmp.block_export_add_nbd(&id, &node, &export.name);
        qmp_call(add, "QMP block-export-add").await?;
    }
    Ok(port)
}

/// 받는 QEMU가 복제본 `export`를 여는 `blockdev-add` 인자. 겹층이 있으면 복제본을 backing으로 둔 qcow2 겹층이다.
fn receiver_blockdev(
    node: &str,
    export: &ReceiverExport,
) -> Result<serde_json::Value, DriverError> {
    let (path, driver) = match &export.source {
        DiskLocation::Path(path, DiskKind::File) => (path, "file"),
        DiskLocation::Path(path, DiskKind::Block) => (path, "host_device"),
        DiskLocation::Path(_, DiskKind::Rbd) | DiskLocation::Rbd(_) => {
            return Err(DriverError::InvalidSpec(
                "replicas cannot be kept in rbd pools".into(),
            ));
        }
    };
    let image = json!({
        "driver": fmt_name(export.format),
        "file": { "driver": driver, "filename": path.display().to_string() },
    });
    Ok(match &export.overlay {
        None => {
            let mut image = image;
            image["node-name"] = json!(node);
            image
        }
        Some(overlay) => {
            if !overlay.is_absolute() {
                return Err(DriverError::InvalidSpec(format!(
                    "overlay must be an absolute path: {overlay:?}"
                )));
            }
            json!({
                "driver": "qcow2",
                "node-name": node,
                "file": { "driver": "file", "filename": overlay.display().to_string() },
                "backing": image,
            })
        }
    })
}

/// 받는 QEMU의 내보내기를 닫고, NBD 서버를 멈추고, 복제본 블록 노드(`opl-repl-{i}`)를 닫아(쓴 것을 이미지에
/// 내보냄) `quit`한다.
async fn close_replicas(qmp: &QmpClient) -> Result<(), DriverError> {
    let nodes = qmp_call(qmp.query_block_nodes(), "QMP query-named-block-nodes").await?;
    let mut opened: Vec<usize> = nodes
        .iter()
        .filter_map(|n| n.node_name.strip_prefix(RECEIVER_NODE_PREFIX)?.parse().ok())
        .collect();
    opened.sort();
    // 다시 부르면 이미 닫은 내보내기와 멈춘 서버는 QEMU 오류다.
    for i in &opened {
        let export = format!("{RECEIVER_EXPORT_PREFIX}{i}");
        match qmp_call(qmp.block_export_del(&export), "QMP block-export-del").await {
            Ok(()) | Err(DriverError::Qmp(QmpError::Command { .. })) => {}
            Err(e) => return Err(e),
        }
    }
    match qmp_call(qmp.nbd_server_stop(), "QMP nbd-server-stop").await {
        Ok(()) | Err(DriverError::Qmp(QmpError::Command { .. })) => {}
        Err(e) => return Err(e),
    }
    // 노드를 닫으면 쓴 것을 이미지에 내보낸다. 내보내기는 클라이언트가 끊긴 뒤 비동기로 사라지므로 잠시
    // 노드를 붙들 수 있다.
    for i in &opened {
        close_receiver_node(qmp, &format!("{RECEIVER_NODE_PREFIX}{i}")).await?;
    }
    qmp_call(qmp.quit(), "QMP quit").await
}

/// 받는 QEMU의 블록 노드 `node`를 닫는다. 내보내기가 아직 붙들고 있으면 [`RECEIVER_CLOSE_WAIT`]까지 다시 해 본다.
async fn close_receiver_node(qmp: &QmpClient, node: &str) -> Result<(), DriverError> {
    let deadline = tokio::time::Instant::now() + RECEIVER_CLOSE_WAIT;
    loop {
        match qmp_call(qmp.blockdev_del(node), "QMP blockdev-del").await {
            Err(DriverError::Qmp(QmpError::Command { .. }))
                if tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => return other,
        }
    }
}

/// 이름이 `prefixes` 중 하나로 시작하는 작업과 블록 노드(증분 백업 `opl-backup-*`, 디스크 미러
/// `opl-mirror-*`)를 치운다. 아직 도는 작업은 취소하고(미러는 ready여도 맞추지 않고 `force`)
/// [`CANCEL_WAIT`]까지 끝나기를 기다린다(넘으면 오류 — 스냅샷이 영원히 막히지 않게).
async fn clear_jobs(qmp: &QmpClient, prefixes: &[&str]) -> Result<(), DriverError> {
    let ours = |name: &str| prefixes.iter().any(|p| name.starts_with(p));
    let jobs = query_jobs(qmp).await?;
    let left: Vec<String> = jobs
        .iter()
        .filter(|j| ours(&j.id))
        .map(|j| j.id.clone())
        .collect();
    if !left.is_empty() {
        tracing::warn!(jobs = ?left, "clearing jobs left by an earlier backup or migration");
        for job in jobs
            .iter()
            .filter(|j| left.contains(&j.id) && !j.is_concluded())
        {
            // 그 사이에 끝난 작업은 취소하지 못한다. 끝났는지는 아래에서 기다려 안다.
            let cancel = if job.id.starts_with(MIRROR_JOB_PREFIX) {
                tokio::time::timeout(QMP_TIMEOUT, qmp.block_job_cancel(&job.id, true)).await
            } else {
                tokio::time::timeout(QMP_TIMEOUT, qmp.job_cancel(&job.id)).await
            };
            if let Err(e) = flatten(cancel, "QMP job-cancel") {
                tracing::warn!(job = %job.id, error = %e, "cannot cancel job");
            }
        }
        match tokio::time::timeout(CANCEL_WAIT, wait_jobs(qmp, &left)).await {
            Ok(r) => r?,
            Err(_) if left.iter().any(|j| j.starts_with(MIRROR_JOB_PREFIX)) => {
                return Err(DriverError::Timeout("cancelled disk mirrors to stop"));
            }
            Err(_) => return Err(DriverError::Timeout("cancelled backup jobs to stop")),
        };
        for id in &left {
            let dismiss = tokio::time::timeout(QMP_TIMEOUT, qmp.job_dismiss(id)).await;
            flatten(dismiss, "QMP job-dismiss")?;
        }
    }
    let nodes = tokio::time::timeout(QMP_TIMEOUT, qmp.query_block_nodes()).await;
    for node in flatten(nodes, "QMP query-named-block-nodes")? {
        if ours(&node.node_name) {
            let del = tokio::time::timeout(QMP_TIMEOUT, qmp.blockdev_del(&node.node_name)).await;
            flatten(del, "QMP blockdev-del")?;
        }
    }
    Ok(())
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
/// `guest_events`면 그 사이 게스트의 재부팅·종료 시작(`Reset`)과 일시 정지·재개(`PauseChanged`)도 알린다.
async fn monitor(
    id: Uuid,
    mut rx: broadcast::Receiver<QmpEvent>,
    guest_events: bool,
    // Weak: 맵이 핸들을, 핸들이 이 태스크를 소유하므로 강한 참조를 쥐면 순환이 생겨
    // 드라이버를 내려도 QMP 연결이 닫히지 않는다.
    vms: Weak<Mutex<HashMap<Uuid, Arc<VmHandle>>>>,
    events: broadcast::Sender<HypervisorEvent>,
    paths: RuntimePaths,
    net: Option<Arc<dyn HostNet>>,
) {
    let mut guest_initiated = None;
    let guest = |kind| {
        if guest_events {
            let _ = events.send(HypervisorEvent { id, kind });
        }
    };
    loop {
        match rx.recv().await {
            Ok(ev) if ev.event == "SHUTDOWN" => {
                guest_initiated = Some(ev.data["guest"].as_bool().unwrap_or(false));
                guest(EventKind::Reset);
            }
            Ok(ev) if ev.event == "RESET" => guest(EventKind::Reset),
            Ok(ev) if ev.event == "STOP" || ev.event == "RESUME" => guest(EventKind::PauseChanged),
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
    // swtpm은 `--terminate`로 QEMU와 함께 끝나지만, 남았으면(주인 확인) 끝낸다.
    if delete_taps(net.as_deref(), id).await
        && exited
        && tpm::terminate(&paths).await
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

/// VM 스코프의 인자: [`scope_args`]에 고정(계획 64)의 `AllowedCPUs`. 고정이 없거나 스코프를 묶지 않으면 지금 그대로다.
fn vm_scope_args(id: Uuid, pinning: Option<&RuntimePinning>) -> Vec<String> {
    let mut args = scope_args(id);
    if let Some(cpus) = pinning.and_then(|p| p.scope.as_ref()) {
        args.push(format!("--property=AllowedCPUs={cpus}"));
    }
    args
}

/// QEMU를 띄우는 시간 한도: 30초 + 미리 잡는 메모리(전용·hugepages VM, 계획 64) GiB마다 1초
fn start_timeout(vm: &VmRuntimeSpec) -> Duration {
    let prealloc = vm
        .numa
        .as_ref()
        .is_some_and(|n| n.page.is_some() || !n.policy.is_shared());
    let gib = if prealloc {
        u64::from(vm.memory_mib).div_ceil(1024)
    } else {
        0
    };
    START_TIMEOUT + Duration::from_secs(gib)
}

/// 자식 프로세스(`fork` 뒤 `exec` 전)의 친화를 `cpus`로 둔다(Linux). 자식은 opld의 친화(예약 CPU)를 물려받으므로 늘
/// 다시 건다.
pub(crate) fn pin_child(cmd: &mut tokio::process::Command, cpus: &CpuSet) {
    #[cfg(target_os = "linux")]
    {
        let mask = crate::affinity::mask(cpus);
        // SAFETY: 닫는 함수는 시스템 호출 하나(sched_setaffinity)만 부른다 — fork 뒤에 불러도 되는 것이다.
        unsafe {
            cmd.pre_exec(move || crate::affinity::set_mask(0, &mask));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (cmd, cpus);
}

/// 실행 중 VM의 스레드를 고정한다: vCPU 스레드(QMP `query-cpus-fast`)는 자리의 CPU, 나머지는 에뮬레이터 집합(자식
/// 친화로 이미 물려받았지만 스코프 cpuset에 들어갈 때 커널이 넓혔을 수 있다). 커널 일꾼 스레드(`PF_USER_WORKER` 등 —
/// `kvm-nx-lpage-recovery`·`vhost-*`)는 건너뛴다(스코프 `AllowedCPUs`가 상한). 친화가 이미 같은 스레드는 건드리지 않고,
/// vCPU는 `/proc/<pid>/task/<tid>/status`로 확인한다. 바꾼 스레드 수를 돌려준다. vCPU를 고정하지 못하면
/// `StartFailed("cannot pin vCPU 3 to CPU 7: …")`다.
async fn pin_threads(handle: &VmHandle, pin: &RuntimePinning) -> Result<usize, DriverError> {
    let vcpus = if pin.vcpus.is_empty() {
        Vec::new()
    } else {
        let r =
            tokio::time::timeout(QMP_TIMEOUT, handle.qmp.execute("query-cpus-fast", None)).await;
        crate::affinity::vcpu_threads(&flatten(r, "QMP query-cpus-fast")?, pin.slots_per_socket)
    };
    let proc = Path::new("/proc");
    // 컨테이너·슬라이스의 cpuset이 자른 친화는 맞은 것으로 본다(리뷰 2 N3 — 아니면 회차마다 다시 건다).
    let allowed = crate::affinity::cgroup_cpus(proc, Path::new("/sys"), handle.pid);
    let mut changed = 0;
    for tid in crate::affinity::threads(proc, handle.pid)? {
        let slot = vcpus.iter().find(|(_, t)| *t == tid).map(|(slot, _)| *slot);
        // 커널 일꾼(`kvm-nx-lpage-recovery`·`vhost-*`)은 커널이 친화를 정한다: 상한은 스코프 `AllowedCPUs`가 쥐고, 다시 걸면
        // 실패하거나 검사 루프가 회차마다 커널과 다툰다.
        if slot.is_none() && crate::affinity::is_kernel_worker(proc, handle.pid, tid) {
            continue;
        }
        let want = slot.map_or(&pin.emulator, |s| pin.vcpu(s));
        let Some(got) = crate::affinity::thread_cpus(proc, handle.pid, tid) else {
            // 그사이 끝난 스레드
            continue;
        };
        let bounded = crate::affinity::effective(want, allowed.as_ref());
        if got == bounded {
            continue;
        }
        let set = crate::affinity::set_thread(tid, want);
        let Some(slot) = slot else {
            match set {
                Ok(()) => changed += 1,
                // 그사이 끝난 스레드(ESRCH)
                Err(e) if e.raw_os_error() == Some(3) => {}
                Err(e) => {
                    return Err(DriverError::Io(std::io::Error::other(format!(
                        "cannot pin thread {tid} to CPU {want}: {e}"
                    ))));
                }
            }
            continue;
        };
        let failed = |why: String| {
            DriverError::StartFailed(format!("cannot pin vCPU {slot} to CPU {want}: {why}"))
        };
        set.map_err(|e| failed(e.to_string()))?;
        match crate::affinity::thread_cpus(proc, handle.pid, tid) {
            Some(got) if got == bounded => changed += 1,
            Some(got) => return Err(failed(format!("the thread runs on CPU {got}"))),
            None => return Err(failed(format!("cannot read thread {tid}"))),
        }
    }
    Ok(changed)
}

/// 스코프 `unit`의 `AllowedCPUs`(systemd). 걸지 않았으면 빈 집합, 읽지 못하면 None
async fn scope_cpus(unit: &str) -> Option<CpuSet> {
    let out = tokio::process::Command::new("systemctl")
        .args(["show", "--property=AllowedCPUs", "--value", unit])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(QMP_TIMEOUT, out).await.ok()?.ok()?;
    out.status.success().then_some(())?;
    crate::affinity::systemd_cpus(&String::from_utf8_lossy(&out.stdout))
}

/// 도는 스코프 `unit`의 `AllowedCPUs`를 바꾼다(`systemctl set-property --runtime`).
async fn set_scope_cpus(unit: &str, cpus: &CpuSet) -> Result<(), DriverError> {
    let out = tokio::process::Command::new("systemctl")
        .args(["set-property", "--runtime", unit])
        .arg(format!("AllowedCPUs={cpus}"))
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(KILL_TIMEOUT, out)
        .await
        .map_err(|_| DriverError::Timeout("systemctl set-property"))??;
    if out.status.success() {
        Ok(())
    } else {
        Err(DriverError::Io(std::io::Error::other(format!(
            "cannot set AllowedCPUs={cpus} on {unit}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))))
    }
}

/// 실행 디렉터리의 고정 기록(`pinning.json`): 실행 중 붙인 vCPU의 고정과 검사 루프의 어긋남 고치기가 읽는다.
fn write_pinning(dir: &Path, pin: &RuntimePinning) -> Result<(), DriverError> {
    let json = serde_json::to_vec(pin).map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
    std::fs::write(dir.join(PINNING_FILE), json)?;
    Ok(())
}

fn read_pinning(dir: &Path) -> Option<RuntimePinning> {
    serde_json::from_slice(&std::fs::read(dir.join(PINNING_FILE)).ok()?).ok()
}

/// QEMU를 감싸는 `systemd-run`의 인자(`--` 앞). 스코프는 QEMU가 끝날 때까지 남고, `--collect`면 실패로
/// 끝나도 남기지 않는다. `Before=opld.service`: 멈출 때는 순서가 거꾸로라 호스트가 꺼질 때 opld가 끝난 뒤에
/// 스코프가 멈춘다(종료 정책이 VM이 살아 있을 때 돈다). `After=network-online.target remote-fs.target`: 망과
/// 원격 파일시스템보다 먼저 멈춘다(NFS·iSCSI 디스크를 쓰는 QEMU가 망이 끊긴 뒤에 남지 않게).
fn scope_args(id: Uuid) -> Vec<String> {
    vec![
        "--scope".into(),
        "--quiet".into(),
        "--collect".into(),
        format!("--unit=opl-vm-{id}"),
        "--property=Before=opld.service".into(),
        "--property=After=network-online.target remote-fs.target".into(),
    ]
}

/// 끝난 QEMU의 `--collect` 스코프는 systemd가 비동기로 회수하므로, 곧바로 다시 띄우면
/// systemd-run이 "Unit ... already exists"로 실패한다. 호출자가 이전 QEMU가 없음을 확인했으므로
/// 남은 스코프를 멈추고 실패 기록을 지운다. 오류와 출력은 무시한다.
pub(crate) async fn clear_stale_scope(unit: &str) {
    for verb in ["stop", "reset-failed"] {
        let status = tokio::process::Command::new("systemctl")
            .args([verb, "--quiet", unit])
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
pub(crate) async fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    tokio::time::timeout(limit, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

/// QMP 명령 하나를 [`QMP_TIMEOUT`]까지 기다린다.
async fn qmp_call<T>(
    fut: impl Future<Output = Result<T, QmpError>>,
    what: &'static str,
) -> Result<T, DriverError> {
    flatten(tokio::time::timeout(QMP_TIMEOUT, fut).await, what)
}

/// 마이그레이션 받는 쪽이 켤 장치 id(`expected`)와 원본 QEMU의 디스크·NIC·붙인 vCPU·virtio-mem 장치
/// (`/machine/peripheral`)가 같은지 본다. 다르면 `Migration` 오류다.
async fn check_devices(qmp: &QmpClient, expected: &[String]) -> Result<(), DriverError> {
    let ids = qmp_call(qmp.peripheral_ids(), "QMP qom-list").await?;
    let running: BTreeSet<String> = ids.into_iter().filter(|id| tracked_device(id)).collect();
    let expected: BTreeSet<String> = expected.iter().cloned().collect();
    if running == expected {
        return Ok(());
    }
    let list = |a: &BTreeSet<String>, b: &BTreeSet<String>| {
        a.difference(b).cloned().collect::<Vec<_>>().join(", ")
    };
    let mut why = Vec::new();
    let extra = list(&running, &expected);
    if !extra.is_empty() {
        why.push(format!("QEMU has {extra} that the record does not"));
    }
    let missing = list(&expected, &running);
    if !missing.is_empty() {
        why.push(format!("the record has {missing} that QEMU does not"));
    }
    Err(DriverError::Migration(format!(
        "the running QEMU devices do not match the VM's hardware record: {}",
        why.join("; ")
    )))
}

/// 마이그레이션 전에 견주는 장치 id인지: `disk{i}-dev`, `nic{i}`, `cpu{i}`, `vmem{i}-dev`, CD-ROM `cd{i}-dev`, cloud-init
/// 씨앗 `cidata-dev`(CD 컨트롤러 `scsi0`는 견주지 않는다)
fn tracked_device(id: &str) -> bool {
    let number = |rest: &str| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    // vNUMA VM(계획 64)은 게스트 노드마다 `vmem<i>-dev`다.
    id.strip_prefix("vmem")
        .and_then(|r| r.strip_suffix("-dev"))
        .is_some_and(number)
        || id == "cidata-dev"
        || ["disk", "cd"].iter().any(|p| {
            id.strip_prefix(p)
                .and_then(|r| r.strip_suffix("-dev"))
                .is_some_and(number)
        })
        || id.strip_prefix("nic").is_some_and(number)
        || id.strip_prefix("cpu").is_some_and(number)
}

/// 장치 `id`를 떼고 게스트가 `timeout` 안에 내려 주기를 기다린다. 뗐으면(이미 없었으면) true
async fn unplug(qmp: &QmpClient, id: &str, timeout: Duration) -> Result<bool, DriverError> {
    let del = qmp.device_del_wait(id, timeout);
    match tokio::time::timeout(timeout + QMP_TIMEOUT, del).await {
        Ok(Ok(removed)) => Ok(removed),
        // QEMU가 거절했다(떼기 요청이 남지 않았다).
        Ok(Err(e @ QmpError::Command { .. })) => Err(e.into()),
        // `device_del`을 보낸 뒤 QMP가 끊기거나 답하지 않았다. QEMU가 받았을 수 있으므로 시간 초과처럼 다룬다(장치가
        // 남았으면 다음 Task가 마저 뗀다).
        Ok(Err(e)) => {
            tracing::warn!(device = id, error = %e, "QMP failed after device_del; treating the unplug as unfinished");
            Ok(false)
        }
        Err(_) => {
            tracing::warn!(
                device = id,
                "QMP did not answer device_del; treating the unplug as unfinished"
            );
            Ok(false)
        }
    }
}

/// QEMU가 vCPU `device_del`을 거절한 까닭이 게스트가 vCPU 떼기를 켜지 않은 것인지(ACPI 옛 CPU 핫플러그 방식:
/// "acpi: device unplug request for not supported device type: max-x86_64-cpu")
fn vcpu_unplug_refused(desc: &str) -> bool {
    desc.contains("device unplug request for not supported device type")
}

/// virtio-mem 장치 수(`vmem<i>-dev`, vNUMA VM은 게스트 노드마다 하나, 계획 64)
async fn virtio_mem_nodes(qmp: &QmpClient) -> Result<usize, DriverError> {
    let ids = qmp_call(qmp.peripheral_ids(), "QMP qom-list").await?;
    Ok(ids
        .iter()
        .filter(|n| {
            n.strip_prefix("vmem")
                .and_then(|r| r.strip_suffix("-dev"))
                .is_some_and(|i| i.parse::<u32>().is_ok())
        })
        .count())
}

/// 게스트 노드마다 장치가 있는 virtio-mem의 꽂을 크기를 바꾼다: 장치마다 자리(백엔드 `vmem<i>`의 크기)를 넘지 않게
/// 나누고([`numa::split_plugged`], 명령줄과 같은 나눔), 줄이기는 게스트가 모두 내려 주기를 기다린다. 시간이 넘으면
/// 장치마다 지금 크기를 요청으로 되돌리고 그 합을 돌려준다.
async fn set_memory_per_node(
    qmp: &QmpClient,
    id: Uuid,
    nodes: usize,
    plugged_mib: u32,
    timeout: Duration,
) -> Result<u32, DriverError> {
    let dev = |i: usize| format!("/machine/peripheral/vmem{i}-dev");
    let mut caps = Vec::with_capacity(nodes);
    for i in 0..nodes {
        let size = qmp_call(
            qmp.qom_get(&format!("/objects/vmem{i}"), "size"),
            "QMP qom-get",
        )
        .await?;
        let bytes = size
            .as_u64()
            .ok_or_else(|| QmpError::Protocol(format!("virtio-mem backend size: {size}")))?;
        caps.push(u32::try_from(bytes >> 20).unwrap_or(u32::MAX));
    }
    let wanted = opl_types::numa::split_plugged(plugged_mib, &caps);
    for (i, mib) in wanted.iter().enumerate() {
        let path = dev(i);
        let set = qmp.qom_set(&path, "requested-size", json!(u64::from(*mib) << 20));
        qmp_call(set, "QMP qom-set").await?;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut sizes = Vec::with_capacity(nodes);
        for i in 0..nodes {
            let size = qmp_call(qmp.qom_get(&dev(i), "size"), "QMP qom-get").await?;
            sizes.push(size.as_u64().unwrap_or(u64::MAX));
        }
        if sizes
            .iter()
            .zip(&wanted)
            .all(|(size, mib)| *size <= u64::from(*mib) << 20)
        {
            return Ok(wanted.iter().sum());
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(%id, ?sizes, ?wanted, ?timeout, "the guest did not release memory");
            let mut total = 0u32;
            for (i, size) in sizes.iter().enumerate() {
                let path = dev(i);
                let set = qmp.qom_set(&path, "requested-size", json!(size));
                qmp_call(set, "QMP qom-set").await?;
                total += u32::try_from(size.div_ceil(1 << 20)).unwrap_or(u32::MAX);
            }
            return Ok(total);
        }
        tokio::time::sleep(VIRTIO_MEM_POLL).await;
    }
}

/// virtio-mem 장치가 게스트에 꽂아 준 크기(`size`, 바이트)
async fn virtio_mem_size(qmp: &QmpClient) -> Result<u64, DriverError> {
    let size = qmp_call(qmp.qom_get(VIRTIO_MEM_PATH, "size"), "QMP qom-get").await?;
    size.as_u64()
        .ok_or_else(|| QmpError::Protocol(format!("virtio-mem size: {size}")).into())
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

/// 런타임 정의의 cloud-init 씨앗을 NoCloud ISO(볼륨 id `cidata`)로 `path`에 쓴다: 임시 파일(0600)에 쓰고 디스크에
/// 내린 뒤 이름을 바꾼다. 씨앗이 없으면 앞선 QEMU가 남긴 파일을 지운다.
fn write_seed(vm: &VmRuntimeSpec, path: &Path) -> Result<(), DriverError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let Some(seed) = &vm.cloud_init else {
        return Ok(remove_if_exists(path)?);
    };
    let files = opl_qemu::iso9660::cidata_files(seed);
    let iso = opl_qemu::iso9660::build("cidata", &files)
        .map_err(|e| DriverError::InvalidSpec(format!("cloud-init seed: {e}")))?;
    let tmp = path.with_extension("iso.tmp");
    remove_if_exists(&tmp)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&iso)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(())
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

    use opl_qemu::{Accel, BitmapInfo};
    use opl_types::cloud_init::CloudInitSeed;
    use opl_types::runtime::RuntimeNic;
    use opl_types::vm::{Arch, CpuModel, CpuSpec, MachineType, NicModel};

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
                secure_boot: None,
            },
            run_dir: root.join("run"),
            state_dir: root.join("state"),
            net: None,
            systemd_scope: false,
            swtpm: Default::default(),
        }
    }

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                emulator: None,
                policy: Default::default(),
                thread_policy: None,
                flags: Vec::new(),
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
                max_vcpus: None,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
            guest_agent: false,
            hotplug: None,
            cdroms: vec![],
            cloud_init: None,
            tpm: None,
            secure_boot: false,
            freeze_fs: false,
            firmware_epoch: 0,
            keep_tpm: false,
            cpu_resolved: None,
            numa: None,
            host_placement: None,
            pinning: None,
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

    /// 다른 프로세스(죽은 노드의 QEMU, NFS 임대가 아직 남은 잠금)가 이미지를 쥐고 있으면 QEMU는
    /// 이 문구로 끝난다(QEMU 8.2·11 같음). 잠시 뒤 다시 시도할 오류로 구분한다.
    #[tokio::test]
    async fn locked_image_is_a_transient_start_error() {
        let tmp = short_tempdir();
        let fake = tmp.path().join("fake-qemu");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             echo 'qemu-system-x86_64: -blockdev driver=qcow2,file=disk0-file,node-name=disk0: Failed to get \"write\" lock' >&2\n\
             echo 'Is another process using the image [/pool/d.qcow2]?' >&2\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hv = QemuHypervisor::new(config(tmp.path(), fake)).await.unwrap();
        let vm = spec();

        let err = hv.start(&vm, None).await.unwrap_err();
        match &err {
            DriverError::ImageLocked(msg) => {
                assert!(msg.contains("Failed to get \"write\" lock"), "{msg}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(err.is_transient());
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
    }

    /// VM 스코프는 opld보다 먼저 시작한 것으로 묶여(`Before=opld.service`) 호스트가 꺼질 때 opld 뒤에 멈추고,
    /// 망·원격 파일시스템보다는 먼저 멈춘다.
    #[test]
    fn the_vm_scope_is_ordered_before_opld() {
        let id = Uuid::nil();
        assert_eq!(
            scope_args(id),
            [
                "--scope",
                "--quiet",
                "--collect",
                "--unit=opl-vm-00000000-0000-0000-0000-000000000000",
                "--property=Before=opld.service",
                "--property=After=network-online.target remote-fs.target",
            ]
        );
    }

    /// 고정이 없으면 VM 스코프의 인자는 지금 그대로이고, 스코프를 묶는 고정은 `AllowedCPUs`를 더한다(계획 64). 미리 잡는
    /// 메모리만큼 띄우는 시간이 늘어난다.
    #[test]
    fn pinned_vms_get_allowed_cpus_and_more_time_to_preallocate() {
        let id = Uuid::nil();
        assert_eq!(vm_scope_args(id, None), scope_args(id));
        let mut pin = RuntimePinning {
            emulator: "1".parse().unwrap(),
            vcpus: vec!["2".parse().unwrap(), "3".parse().unwrap()],
            slots_per_socket: 2,
            scope: None,
        };
        assert_eq!(vm_scope_args(id, Some(&pin)), scope_args(id));
        pin.scope = Some(pin.all());
        let args = vm_scope_args(id, Some(&pin));
        assert_eq!(args[..args.len() - 1], scope_args(id)[..]);
        assert_eq!(args.last().unwrap(), "--property=AllowedCPUs=1-3");

        let mut vm = spec();
        vm.memory_mib = 4097;
        assert_eq!(start_timeout(&vm), START_TIMEOUT);
        vm.numa = Some(opl_types::numa::ResolvedNuma {
            nodes: vec![opl_types::numa::GuestNode {
                sockets: vec![0],
                memory_mib: 4097,
            }],
            distances: Vec::new(),
            page: Some(opl_types::numa::PageKind::Size2M),
            policy: Default::default(),
            thread_policy: None,
            emulator: None,
            memory: Some(opl_types::numa::MemoryPolicy::Strict),
        });
        assert_eq!(start_timeout(&vm), START_TIMEOUT + Duration::from_secs(5));
    }

    #[test]
    fn classifies_image_lock_messages() {
        // 쓰기를 원하는 쪽과 남이 쓰기를 공유하지 않는 쪽 모두
        assert!(image_locked(
            "qemu: -blockdev x: Failed to get \"write\" lock\nIs another process using the image [/d]?"
        ));
        assert!(image_locked(
            "qemu: -blockdev x: Failed to get shared \"write\" lock\nIs another process using the image [/d]?"
        ));
        assert!(!image_locked("qemu: something broke"));
        assert!(!image_locked(
            "qemu: Could not open '/d': No such file or directory"
        ));
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
        /// `query-jobs`에 차례로 줄 답. 비면 `[]`이다.
        jobs: Arc<Mutex<std::collections::VecDeque<serde_json::Value>>>,
        /// `query-status`에 차례로 줄 상태. 비면 `status`다.
        statuses: Arc<Mutex<std::collections::VecDeque<&'static str>>>,
        /// 이 이름의 명령에는 오류로 답한다.
        failing: Arc<Mutex<Vec<&'static str>>>,
        /// `blockdev-add`로 연 NBD 노드(`blockdev-del`이 닫는다). 블록 노드 목록에 드러난다.
        nbd_nodes: Arc<Mutex<Vec<String>>>,
        /// `blockdev-add`로 연 디스크 노드(`disk…`, `blockdev-del`이 닫는다). 블록 노드 목록에 드러난다.
        hot_nodes: Arc<Mutex<Vec<String>>>,
        /// 참이면 `device_del` 뒤에 `DEVICE_DELETED`를 보내지 않는다(게스트가 내려 주지 않음).
        hold_unplug: Arc<Mutex<bool>>,
        /// virtio-mem의 (`requested-size`, 게스트가 멈춘 `size`). `size`가 None이면 요청을 따른다(바이트).
        vmem: Arc<Mutex<(u64, Option<u64>)>>,
        /// 이 이름의 명령을 받으면 답하지 않고 연결을 끊는다(QEMU가 받은 뒤 QMP가 끊김).
        drop_on: Arc<Mutex<Vec<&'static str>>>,
        /// 참이면 vCPU `device_del`을 QEMU 8.2 q35처럼 거절한다(게스트가 ACPI CPU 핫플러그를 켜지 않음).
        refuse_cpu_unplug: Arc<Mutex<bool>>,
        /// 디스크 노드(`disk0`·`disk1`)에 더 드러낼 비트맵
        extra_bitmaps: Arc<Mutex<Vec<(&'static str, serde_json::Value)>>>,
        /// 디스크 장치 둘(`disk0-dev`·`disk1-dev`)이 쓰는 맨 위 노드(`query-block`, `qom-set … drive`가 바꾼다)
        roots: Arc<Mutex<[String; 2]>>,
        /// 이 이름의 명령에는 이만큼 기다린 뒤 답한다.
        delay: Arc<Mutex<Option<(&'static str, Duration)>>>,
        /// CD-ROM 드라이브(`cd0-dev`)에 넣은 매체 파일(`blockdev-insert-medium`이 넣고 `blockdev-remove-medium`이 뺀다)
        cd_medium: Arc<Mutex<Option<String>>>,
        /// `cd0-dev`의 트레이가 열려 있는지(`blockdev-open-tray`가 열고 `blockdev-close-tray`가 닫는다)
        cd_tray: Arc<Mutex<bool>>,
    }

    /// QEMU 대신 QMP 소켓을 여는 가짜 서버. greeting과 capabilities 협상까지 처리하고,
    /// `query-status`에는 `status`를 답한다.
    async fn fake_qmp(dir: &Path, status: &'static str) -> FakeQmp {
        fake_qmp_with(dir, status, "completed").await
    }

    /// [`fake_qmp`]에 더해 `query-migrate`에 `migration` 상태로 답한다(`failed`면 `error-desc`도).
    /// `migrate_cancel`을 받은 뒤에는 `cancelled`로, `migrate-continue`를 받은 뒤에는 `completed`로 답한다.
    async fn fake_qmp_with(dir: &Path, status: &'static str, migration: &'static str) -> FakeQmp {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        std::fs::create_dir_all(dir).unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let (hangup, mut hangup_rx) = tokio::sync::oneshot::channel::<Option<serde_json::Value>>();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        let args = Arc::new(Mutex::new(Vec::new()));
        let arguments = args.clone();
        let jobs = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let job_replies = jobs.clone();
        let statuses = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let status_replies = statuses.clone();
        let failing = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let fail = failing.clone();
        let nbd_nodes = Arc::new(Mutex::new(Vec::<String>::new()));
        let nbd = nbd_nodes.clone();
        let hot_nodes = Arc::new(Mutex::new(Vec::<String>::new()));
        let hot = hot_nodes.clone();
        let hold_unplug = Arc::new(Mutex::new(false));
        let hold = hold_unplug.clone();
        let vmem = Arc::new(Mutex::new((0u64, None::<u64>)));
        let mem = vmem.clone();
        let drop_on = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let dropping = drop_on.clone();
        let refuse_cpu_unplug = Arc::new(Mutex::new(false));
        let refuse_cpu = refuse_cpu_unplug.clone();
        let extra_bitmaps = Arc::new(Mutex::new(Vec::<(&'static str, serde_json::Value)>::new()));
        let extra = extra_bitmaps.clone();
        let roots = Arc::new(Mutex::new(["disk0".to_string(), "disk1".to_string()]));
        let drives = roots.clone();
        let delay = Arc::new(Mutex::new(None::<(&'static str, Duration)>));
        let slow = delay.clone();
        let cd_medium = Arc::new(Mutex::new(None::<String>));
        let medium = cd_medium.clone();
        let cd_tray = Arc::new(Mutex::new(false));
        let tray = cd_tray.clone();
        // `blockdev-add`로 연 CD 노드마다 그 파일(raw 노드는 아래 file 노드의 파일)
        let mut cd_files: HashMap<String, String> = HashMap::new();
        // id 있는 장치: 디스크 두 개와 NIC 하나를 가진 VM. `device_add`가 더하고 `device_del`(내려 줌)이 뺀다.
        let mut devices: Vec<String> = ["disk0-dev", "disk1-dev", "nic0"]
            .map(String::from)
            .to_vec();
        // 네트워크 백엔드 id. 같은 id의 `netdev_add`와 없는 id의 `netdev_del`은 QEMU처럼 거절한다.
        // `net3`은 앞서 중단된 붙이기가 장치 없이 남긴 백엔드다.
        let mut netdevs: Vec<String> = vec!["net0".into(), "net3".into()];
        // 붙인 vCPU 자리 (socket, core). 켤 때 둘(0,0)·(0,1)이 켜져 있다.
        let mut cpus: Vec<(u64, u64)> = vec![(0, 0), (0, 1)];
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write
                .write_all(b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\n")
                .await
                .unwrap();
            let mut migration = migration;
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
                if dropping.lock().unwrap().contains(&name.as_str()) {
                    recorded.lock().unwrap().push(name);
                    arguments.lock().unwrap().push(cmd["arguments"].clone());
                    break;
                }
                let wait = *slow.lock().unwrap();
                if let Some((_, d)) = wait.filter(|(n, _)| *n == name) {
                    tokio::time::sleep(d).await;
                }
                let ret = match name.as_str() {
                    "query-status" => {
                        let status = status_replies.lock().unwrap().pop_front().unwrap_or(status);
                        serde_json::json!({ "running": status == "running", "status": status })
                    }
                    "query-version" => serde_json::json!({
                        "qemu": { "major": 8, "minor": 2, "micro": 2 }, "package": ""
                    }),
                    "migrate_cancel" => {
                        migration = "cancelled";
                        serde_json::json!({})
                    }
                    "migrate-continue" => {
                        migration = "completed";
                        serde_json::json!({})
                    }
                    "blockdev-add" if cmd["arguments"]["driver"] == "nbd" => {
                        let node = cmd["arguments"]["node-name"].as_str().unwrap();
                        nbd.lock().unwrap().push(node.to_string());
                        serde_json::json!({})
                    }
                    "blockdev-add"
                        if cmd["arguments"]["node-name"].as_str().is_some_and(|n| {
                            n.starts_with("disk")
                                || n.starts_with("cd")
                                || n.starts_with("opl-repl-")
                                || n.starts_with("opl-fl-")
                        }) =>
                    {
                        let args = &cmd["arguments"];
                        let node = args["node-name"].as_str().unwrap();
                        let file = match args["file"].as_str() {
                            Some(below) => cd_files.get(below).cloned(),
                            None => args["filename"].as_str().map(String::from),
                        };
                        if let Some(file) = file.filter(|_| node.starts_with("cd")) {
                            cd_files.insert(node.to_string(), file);
                        }
                        hot.lock().unwrap().push(node.to_string());
                        serde_json::json!({})
                    }
                    "blockdev-insert-medium" if !fail.lock().unwrap().contains(&name.as_str()) => {
                        let node = cmd["arguments"]["node-name"].as_str().unwrap();
                        *medium.lock().unwrap() = cd_files.get(node).cloned();
                        serde_json::json!({})
                    }
                    "blockdev-remove-medium" => {
                        *medium.lock().unwrap() = None;
                        serde_json::json!({})
                    }
                    "blockdev-open-tray" | "blockdev-close-tray"
                        if !fail.lock().unwrap().contains(&name.as_str()) =>
                    {
                        *tray.lock().unwrap() = name == "blockdev-open-tray";
                        serde_json::json!({})
                    }
                    "blockdev-del" => {
                        let node = cmd["arguments"]["node-name"].as_str().unwrap();
                        nbd.lock().unwrap().retain(|n| n != node);
                        hot.lock().unwrap().retain(|n| n != node);
                        serde_json::json!({})
                    }
                    // 소켓 2개 × 코어 4개 자리, 켜진 자리만 `qom-path`가 있다.
                    "query-hotpluggable-cpus" => (0..2u64)
                        .flat_map(|socket| (0..4u64).map(move |core| (socket, core)))
                        .map(|(socket, core)| {
                            let mut slot = serde_json::json!({
                                "type": "max-x86_64-cpu", "vcpus-count": 1,
                                "props": { "socket-id": socket, "core-id": core, "thread-id": 0 },
                            });
                            if cpus.contains(&(socket, core)) {
                                slot["qom-path"] = "/machine/peripheral/cpu".into();
                            }
                            slot
                        })
                        .collect(),
                    "device_add" if cmd["arguments"]["socket-id"].is_u64() => {
                        let args = &cmd["arguments"];
                        let slot = (
                            args["socket-id"].as_u64().unwrap(),
                            args["core-id"].as_u64().unwrap(),
                        );
                        cpus.push(slot);
                        serde_json::json!({})
                    }
                    "qom-set" if cmd["arguments"]["property"] == "drive" => {
                        let path = cmd["arguments"]["path"].as_str().unwrap();
                        let i = disk_device_index(path).unwrap();
                        drives.lock().unwrap()[i] =
                            cmd["arguments"]["value"].as_str().unwrap().into();
                        serde_json::json!({})
                    }
                    "qom-set" => {
                        mem.lock().unwrap().0 = cmd["arguments"]["value"].as_u64().unwrap();
                        serde_json::json!({})
                    }
                    // 디스크 장치 둘(64 MiB, 1 GiB)과 CD-ROM(처음에는 매체 없음)
                    "query-block" => {
                        let root = drives.lock().unwrap().clone();
                        let mut cd = serde_json::json!({
                            "device": "", "qdev": "/machine/peripheral/cd0-dev",
                            "tray_open": *tray.lock().unwrap(),
                        });
                        if let Some(file) = medium.lock().unwrap().clone() {
                            cd["inserted"] =
                                serde_json::json!({ "node-name": "cd0", "file": file, "ro": true });
                        }
                        serde_json::json!([
                            { "device": "", "qdev": "/machine/peripheral/disk0-dev/virtio-backend",
                              "inserted": { "node-name": root[0], "image": { "virtual-size": 64u64 << 20 } } },
                            { "device": "", "qdev": "/machine/peripheral/disk1-dev",
                              "inserted": { "node-name": root[1], "image": { "virtual-size": 1u64 << 30 } } },
                            cd,
                        ])
                    }
                    "qom-get" => {
                        let (requested, size) = *mem.lock().unwrap();
                        match cmd["arguments"]["property"].as_str() {
                            Some("size") => size.unwrap_or(requested).into(),
                            _ => requested.into(),
                        }
                    }
                    // id 있는 장치(비디오·USB도 있다)
                    "qom-list" => {
                        let mut list = serde_json::json!([
                            { "name": "type", "type": "string" },
                            { "name": "video0", "type": "child<VGA>" },
                            { "name": "usb", "type": "child<qemu-xhci>" },
                        ]);
                        for d in &devices {
                            let child = serde_json::json!({ "name": d, "type": "child<device>" });
                            list.as_array_mut().unwrap().push(child);
                        }
                        list
                    }
                    "query-migrate" if migration == "failed" => serde_json::json!({
                        "status": "failed", "error-desc": "Connection refused"
                    }),
                    "query-migrate" => serde_json::json!({ "status": migration }),
                    // 디스크 두 개(qcow2, raw)를 가진 VM의 블록 노드와 비트맵, 연 NBD 노드
                    "query-named-block-nodes" => {
                        let mut nodes = serde_json::json!([
                            { "node-name": "disk1", "drv": "raw", "dirty-bitmaps": [
                                { "name": "opl-a", "recording": true, "persistent": false },
                            ] },
                            { "node-name": "disk1-file", "drv": "file" },
                            { "node-name": "disk0", "drv": "qcow2", "dirty-bitmaps": [
                                { "name": "user", "recording": true, "persistent": true },
                                { "name": "opl-a", "recording": true, "persistent": true },
                                { "name": "opl-old", "recording": true, "persistent": true,
                                  "inconsistent": true },
                            ] },
                            { "node-name": "disk0-file", "drv": "file" },
                        ]);
                        for (node, bitmap) in extra.lock().unwrap().iter() {
                            let disk = nodes
                                .as_array_mut()
                                .unwrap()
                                .iter_mut()
                                .find(|n| n["node-name"] == *node)
                                .unwrap();
                            disk["dirty-bitmaps"]
                                .as_array_mut()
                                .unwrap()
                                .push(bitmap.clone());
                        }
                        for node in nbd.lock().unwrap().iter() {
                            let node = serde_json::json!({ "node-name": node, "drv": "nbd" });
                            nodes.as_array_mut().unwrap().push(node);
                        }
                        for node in hot.lock().unwrap().iter() {
                            let node = serde_json::json!({ "node-name": node, "drv": "file" });
                            nodes.as_array_mut().unwrap().push(node);
                        }
                        nodes
                    }
                    // 같은 VM의 블록 백엔드: pflash, virtio 디스크, 미러 중이라 맨 위가 필터 노드인
                    // SATA 디스크
                    "query-blockstats" => {
                        let stats = |rd: u64, wr: u64, rd_ops: u64, wr_ops: u64| {
                            serde_json::json!({ "rd_bytes": rd, "wr_bytes": wr,
                                "rd_operations": rd_ops, "wr_operations": wr_ops })
                        };
                        serde_json::json!([
                            { "device": "pflash0", "node-name": "#block101",
                              "stats": stats(1000, 1000, 10, 10) },
                            { "device": "", "qdev": "/machine/peripheral/disk0-dev/virtio-backend",
                              "node-name": "disk0", "stats": stats(4096, 1024, 3, 2) },
                            { "device": "", "qdev": "/machine/peripheral/disk1-dev",
                              "node-name": "#block202", "stats": stats(100, 200, 1, 4) },
                        ])
                    }
                    "query-jobs" => job_replies
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| serde_json::json!([])),
                    _ => serde_json::json!({}),
                };
                let netdev = cmd["arguments"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let cpu_del = name == "device_del"
                    && cmd["arguments"]["id"]
                        .as_str()
                        .is_some_and(|d| d.starts_with("cpu"));
                let refused = match name.as_str() {
                    "device_del" if cpu_del && *refuse_cpu.lock().unwrap() => Some((
                        "GenericError",
                        "acpi: device unplug request for not supported device type: max-x86_64-cpu"
                            .to_string(),
                    )),
                    "netdev_add" if netdevs.contains(&netdev) => Some((
                        "GenericError",
                        format!("Duplicate ID '{netdev}' for netdev"),
                    )),
                    "netdev_del" if !netdevs.contains(&netdev) => {
                        Some(("DeviceNotFound", format!("Device '{netdev}' not found")))
                    }
                    _ => None,
                };
                let reply = if fail.lock().unwrap().contains(&name.as_str()) {
                    serde_json::json!({
                        "error": { "class": "GenericError", "desc": format!("{name} failed") },
                        "id": cmd["id"]
                    })
                } else if let Some((class, desc)) = refused {
                    serde_json::json!({ "error": { "class": class, "desc": desc }, "id": cmd["id"] })
                } else {
                    match name.as_str() {
                        "netdev_add" => netdevs.push(netdev),
                        "netdev_del" => netdevs.retain(|n| *n != netdev),
                        _ => {}
                    }
                    serde_json::json!({ "return": ret, "id": cmd["id"] })
                };
                if !fail.lock().unwrap().contains(&name.as_str())
                    && let Some(dev) = cmd["arguments"]["id"].as_str()
                {
                    if name == "device_add" {
                        devices.push(dev.to_string());
                    } else if name == "device_del"
                        && !*hold.lock().unwrap()
                        && !(cpu_del && *refuse_cpu.lock().unwrap())
                    {
                        devices.retain(|d| d != dev);
                    }
                }
                let deleted = (name == "device_del"
                    && !*hold.lock().unwrap()
                    && !(cpu_del && *refuse_cpu.lock().unwrap()))
                .then(|| {
                    serde_json::json!({
                        "event": "DEVICE_DELETED", "timestamp": {},
                        "data": { "device": cmd["arguments"]["id"] },
                    })
                });
                recorded.lock().unwrap().push(name);
                arguments.lock().unwrap().push(cmd["arguments"].clone());
                if write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                if let Some(ev) = deleted
                    && write.write_all(format!("{ev}\n").as_bytes()).await.is_err()
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
            jobs,
            statuses,
            failing,
            nbd_nodes,
            hot_nodes,
            hold_unplug,
            vmem,
            drop_on,
            refuse_cpu_unplug,
            extra_bitmaps,
            roots,
            delay,
            cd_medium,
            cd_tray,
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

    /// 스냅샷을 위해 멈춘 채 opld가 죽은 VM은 다시 붙을 때 이어서 실행하고 표시를 지운다.
    #[tokio::test]
    async fn start_continues_vm_left_paused_for_snapshots() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        std::fs::write(dir.join(PAUSED_MARK), b"").unwrap();
        let fake = fake_qmp(&dir, "paused").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        hv.start(&vm, None).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
        assert!(!dir.join(PAUSED_MARK).exists());
    }

    /// 표시 없이 멈춘 VM(QEMU가 스스로 멈춤)은 그대로 둔다.
    #[tokio::test]
    async fn start_leaves_vm_paused_by_qemu() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "paused").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        hv.start(&vm, None).await.unwrap();
        assert!(!fake.commands.lock().unwrap().contains(&"cont".to_string()));
    }

    fn target() -> MigrateTarget {
        MigrateTarget {
            host: "10.0.0.2".into(),
            port: 60005,
            tls_hostname: "node-b".into(),
        }
    }

    /// 보내는 쪽: TLS 자격 증명(client)을 다시 만들고, 능력·매개변수를 정한 뒤 보내고 끝을 기다린다.
    #[tokio::test]
    async fn migrate_sets_up_tls_and_waits_for_completion() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let state_dir = cfg.state_dir.clone();
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        assert_eq!(hv.running_qemu_version(vm.id).await.unwrap(), "8.2.2");
        let opts = MigrateOptions {
            bandwidth: Some(64 << 20),
            timeout: Duration::from_secs(10),
            switchover_mirrors: false,
            switchover_catch_up: None,
            devices: None,
        };
        hv.migrate(vm.id, &target(), &opts).await.unwrap();

        let commands = fake.commands.lock().unwrap().clone();
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        assert!(at("object-del") < at("object-add"));
        assert_eq!(
            arguments[at("object-del")],
            serde_json::json!({ "id": "opl-mig-tls" })
        );
        let add = &arguments[at("object-add")];
        assert_eq!(add["qom-type"], "tls-creds-x509");
        assert_eq!(add["endpoint"], "client");
        assert_eq!(
            add["dir"],
            state_dir.join("migration-tls").display().to_string()
        );
        // raw 디스크(disk1)의 비트맵은 이미지에 없으므로 마이그레이션으로 넘긴다. 쓸 수 없는
        // inconsistent 비트맵(disk0의 opl-old)은 보내기 전에 지운다(남으면 마이그레이션이 실패한다).
        // 미러가 없으면 스위치오버 전에 멈추지 않는다(능력은 QEMU에 남으므로 늘 끈다).
        assert_eq!(
            arguments[at("migrate-set-capabilities")]["capabilities"],
            serde_json::json!([
                { "capability": "events", "state": true },
                { "capability": "auto-converge", "state": true },
                { "capability": "dirty-bitmaps", "state": true },
                { "capability": "pause-before-switchover", "state": false }
            ])
        );
        assert_eq!(
            arguments[at("block-dirty-bitmap-remove")],
            serde_json::json!({ "node": "disk0", "name": "opl-old" })
        );
        assert!(at("block-dirty-bitmap-remove") < at("migrate"));
        assert_eq!(
            arguments[at("migrate-set-parameters")],
            serde_json::json!({
                "tls-creds": "opl-mig-tls", "tls-hostname": "node-b", "max-bandwidth": 64u64 << 20
            })
        );
        assert!(at("migrate-set-parameters") < at("migrate"));
        assert_eq!(
            arguments[at("migrate")],
            serde_json::json!({ "uri": "tcp:10.0.0.2:60005" })
        );
        assert!(at("migrate") < at("query-migrate"));
    }

    /// QEMU 8.2는 마이그레이션을 `completed`로 바꾼 뒤에 원본을 `finish-migrate`에서 `postmigrate`로
    /// 바꾼다. `migrate`는 원본이 `postmigrate`가 된 뒤에 돌아온다.
    #[tokio::test]
    async fn migrate_returns_after_source_reaches_postmigrate() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "postmigrate").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        fake.statuses.lock().unwrap().extend([
            "finish-migrate",
            "finish-migrate",
            "finish-migrate",
        ]);
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
            switchover_mirrors: false,
            switchover_catch_up: None,
            devices: None,
        };
        hv.migrate(vm.id, &target(), &opts).await.unwrap();

        assert!(fake.statuses.lock().unwrap().is_empty());
        let commands = fake.commands.lock().unwrap().clone();
        let at = commands.iter().position(|c| c == "query-migrate").unwrap();
        let queries = commands[at..].iter().filter(|c| *c == "query-status");
        assert_eq!(queries.count(), 4, "commands: {commands:?}");
    }

    /// 받는 쪽이 켤 장치(`devices`)가 원본 QEMU의 장치와 다르면 보내지 않는다. 같으면 보낸다.
    #[tokio::test]
    async fn migrate_refuses_when_the_running_devices_differ_from_the_record() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "postmigrate").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let opts = |devices: &[&str]| MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
            switchover_mirrors: false,
            switchover_catch_up: None,
            devices: Some(devices.iter().map(|d| d.to_string()).collect()),
        };
        // 게스트가 아직 내려 주지 않은 disk1-dev, 기록에만 있는 cpu2
        let err = hv
            .migrate(vm.id, &target(), &opts(&["disk0-dev", "nic0", "cpu2"]))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            DriverError::Migration(
                "the running QEMU devices do not match the VM's hardware record: QEMU has \
                 disk1-dev that the record does not; the record has cpu2 that QEMU does not"
                    .into()
            )
            .to_string()
        );
        assert!(!fake.commands.lock().unwrap().iter().any(|c| c == "migrate"));

        hv.migrate(vm.id, &target(), &opts(&["nic0", "disk1-dev", "disk0-dev"]))
            .await
            .unwrap();
        assert!(fake.commands.lock().unwrap().iter().any(|c| c == "migrate"));
    }

    #[tokio::test]
    async fn failed_migration_reports_qemu_error() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let _fake = fake_qmp_with(&dir, "running", "failed").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
            switchover_mirrors: false,
            switchover_catch_up: None,
            devices: None,
        };
        match hv.migrate(vm.id, &target(), &opts).await.unwrap_err() {
            DriverError::Migration(msg) => assert_eq!(msg, "Connection refused"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 시간 제한이 지나면 취소하고 실패로 끝낸다.
    #[tokio::test]
    async fn migration_is_cancelled_after_timeout() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp_with(&dir, "running", "active").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_millis(200),
            switchover_mirrors: false,
            switchover_catch_up: None,
            devices: None,
        };
        let err = hv.migrate(vm.id, &target(), &opts).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::Migration(m) if m.contains("cancelled")),
            "{err:?}"
        );
        assert!(!err.is_transient());
        let commands = fake.commands.lock().unwrap().clone();
        assert!(commands.contains(&"migrate_cancel".to_string()));
        // 보내지 않은 매개변수는 빠진다.
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = commands
            .iter()
            .position(|c| c == "migrate-set-parameters")
            .unwrap();
        assert!(arguments[at].get("max-bandwidth").is_none());
    }

    /// 보내는 중인 마이그레이션만 취소하고 끝(`cancelled`)을 기다린다.
    #[tokio::test]
    async fn cancel_migration_cancels_only_an_outgoing_migration() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let (sending, idle, receiving) = (spec(), spec(), spec());
        let dirs: Vec<PathBuf> = [&sending, &idle, &receiving]
            .iter()
            .map(|vm| cfg.run_dir.join("vm").join(vm.id.to_string()))
            .collect();
        let mut qemus = Vec::new();
        for dir in &dirs {
            qemus.push(hold_pidfile(dir).await);
        }
        std::fs::write(dirs[2].join(INCOMING_MARK), b"").unwrap();
        let fake_sending = fake_qmp_with(&dirs[0], "running", "active").await;
        let fake_idle = fake_qmp_with(&dirs[1], "running", "none").await;
        let fake_receiving = fake_qmp_with(&dirs[2], "inmigrate", "active").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        hv.cancel_migration(sending.id).await.unwrap();
        let commands = fake_sending.commands.lock().unwrap().clone();
        let cancel = commands.iter().position(|c| c == "migrate_cancel").unwrap();
        assert!(commands[cancel + 1..].contains(&"query-migrate".to_string()));

        hv.cancel_migration(idle.id).await.unwrap();
        hv.cancel_migration(receiving.id).await.unwrap();
        hv.cancel_migration(Uuid::now_v7()).await.unwrap();
        for fake in [&fake_idle, &fake_receiving] {
            let commands = fake.commands.lock().unwrap().clone();
            assert!(
                !commands.contains(&"migrate_cancel".to_string()),
                "{commands:?}"
            );
        }
    }

    fn mirror_job(id: &str, status: &str, current: u64, error: Option<&str>) -> serde_json::Value {
        let mut job = serde_json::json!({
            "id": id, "type": "mirror", "status": status,
            "current-progress": current, "total-progress": 100
        });
        if let Some(error) = error {
            job["error"] = error.into();
        }
        job
    }

    fn mirror_targets() -> Vec<MirrorTarget> {
        [(0, ImageFormat::Raw), (1, ImageFormat::Raw)]
            .map(|(disk, format)| MirrorTarget {
                disk,
                host: "10.0.0.2".into(),
                port: 60007,
                export: format!("disk{disk}"),
                tls_hostname: "node-b".into(),
                format,
            })
            .to_vec()
    }

    fn mirror_opts(
        timeout: Duration,
        stall: Duration,
    ) -> (MirrorOptions, tokio::sync::watch::Receiver<MirrorProgress>) {
        let (tx, rx) = tokio::sync::watch::channel(MirrorProgress::default());
        let opts = MirrorOptions {
            speed: Some(32 << 20),
            timeout,
            stall,
            progress: Some(tx),
        };
        (opts, rx)
    }

    /// 보내는 쪽 복사: 남은 미러 확인 → qcow2를 raw로 옮기는 디스크의 비트맵 지우기 → NBD TLS 객체
    /// (client) → 디스크마다 TLS NBD 노드와 미러 → 모두 ready. 진행은 미러들의 합이다.
    #[tokio::test]
    async fn mirror_disks_starts_one_mirror_per_disk_and_waits_until_ready() {
        let tmp = short_tempdir();
        let state_dir = config(tmp.path(), PathBuf::new()).state_dir;
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                mirror_job("opl-mirror-0", "running", 40, None),
                mirror_job("opl-mirror-1", "ready", 100, None)
            ]),
            serde_json::json!([
                mirror_job("opl-mirror-0", "ready", 100, None),
                mirror_job("opl-mirror-1", "ready", 100, None)
            ]),
        ]);
        let (opts, rx) = mirror_opts(Duration::from_secs(30), Duration::from_secs(30));
        hv.mirror_disks(id, &mirror_targets(), &opts).await.unwrap();
        assert_eq!(
            *rx.borrow(),
            MirrorProgress {
                done_bytes: 200,
                total_bytes: 200
            }
        );

        let sent = sent(
            &fake,
            &[
                "block-dirty-bitmap-remove",
                "object-del",
                "object-add",
                "blockdev-add",
                "blockdev-mirror",
            ],
        );
        let names: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
        // disk0(qcow2 → raw)의 opl- 비트맵만 지운다. disk1은 raw라 비트맵이 이미지에 없다(스트림으로 간다).
        assert_eq!(
            names,
            [
                "block-dirty-bitmap-remove",
                "block-dirty-bitmap-remove",
                "object-del",
                "object-add",
                "blockdev-add",
                "blockdev-mirror",
                "blockdev-add",
                "blockdev-mirror"
            ]
        );
        assert_eq!(
            sent[0].1,
            serde_json::json!({ "node": "disk0", "name": "opl-a" })
        );
        assert_eq!(
            sent[1].1,
            serde_json::json!({ "node": "disk0", "name": "opl-old" })
        );
        assert_eq!(sent[2].1, serde_json::json!({ "id": "opl-nbd-tls" }));
        assert_eq!(sent[3].1["id"], "opl-nbd-tls");
        assert_eq!(sent[3].1["endpoint"], "client");
        assert_eq!(sent[3].1["verify-peer"], true);
        assert_eq!(
            sent[3].1["dir"],
            state_dir.join("migration-tls").display().to_string()
        );
        assert_eq!(
            sent[6].1,
            serde_json::json!({
                "driver": "nbd", "node-name": "opl-mirror-1",
                "server": { "type": "inet", "host": "10.0.0.2", "port": "60007" },
                "export": "disk1", "tls-creds": "opl-nbd-tls", "tls-hostname": "node-b"
            })
        );
        assert_eq!(
            sent[7].1,
            serde_json::json!({
                "job-id": "opl-mirror-1", "device": "disk1", "target": "opl-mirror-1",
                "sync": "full", "copy-mode": "background", "auto-dismiss": false,
                "speed": 32u64 << 20
            })
        );
        // 미러는 스위치오버까지 남는다.
        assert!(
            !fake
                .commands
                .lock()
                .unwrap()
                .contains(&"block-job-cancel".to_string())
        );
        assert_eq!(
            *fake.nbd_nodes.lock().unwrap(),
            ["opl-mirror-0", "opl-mirror-1"]
        );
    }

    /// 미러가 실패하거나, 정체하거나, 시간을 넘기면 미러를 모두 치우고 실패한다.
    #[tokio::test]
    async fn failed_stalled_or_slow_disk_copy_clears_the_mirrors() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let running = |current| mirror_job("opl-mirror-0", "running", current, None);
        let canceled = || {
            serde_json::json!([mirror_job(
                "opl-mirror-0",
                "concluded",
                0,
                Some("Operation canceled")
            )])
        };
        let targets = &mirror_targets()[..1];
        let cases = [
            // 작업 오류(대상 노드가 사라짐)
            (
                vec![
                    serde_json::json!([]),
                    serde_json::json!([mirror_job(
                        "opl-mirror-0",
                        "concluded",
                        10,
                        Some("Connection reset by peer")
                    )]),
                    serde_json::json!([mirror_job(
                        "opl-mirror-0",
                        "concluded",
                        10,
                        Some("Connection reset by peer")
                    )]),
                    serde_json::json!([mirror_job(
                        "opl-mirror-0",
                        "concluded",
                        10,
                        Some("Connection reset by peer")
                    )]),
                ],
                (Duration::from_secs(30), Duration::from_secs(30)),
                "migration failed: disk mirror opl-mirror-0 failed: Connection reset by peer",
            ),
            // 복사량이 늘지 않음
            (
                vec![
                    serde_json::json!([]),
                    serde_json::json!([running(5)]),
                    serde_json::json!([running(5)]),
                    serde_json::json!([running(5)]),
                    canceled(),
                ],
                (Duration::from_secs(30), Duration::from_millis(300)),
                "migration failed: disk copy made no progress for 300ms",
            ),
            // 늘지만 시간 안에 끝나지 않음
            (
                vec![
                    serde_json::json!([]),
                    serde_json::json!([running(5)]),
                    serde_json::json!([running(6)]),
                    serde_json::json!([running(7)]),
                    canceled(),
                ],
                (Duration::from_millis(300), Duration::from_secs(30)),
                "migration failed: disk copy did not finish within 300ms",
            ),
        ];
        for (replies, (timeout, stall), message) in cases {
            fake.jobs.lock().unwrap().extend(replies);
            fake.commands.lock().unwrap().clear();
            fake.arguments.lock().unwrap().clear();
            let (opts, _rx) = mirror_opts(timeout, stall);
            let err = hv.mirror_disks(id, targets, &opts).await.unwrap_err();
            assert_eq!(err.to_string(), message);
            assert!(!err.is_transient());
            assert!(fake.jobs.lock().unwrap().is_empty(), "{message}");
            let sent = sent(&fake, &["block-job-cancel", "job-dismiss", "blockdev-del"]);
            let names: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
            if message.contains("Connection reset") {
                // 이미 끝난 작업은 취소하지 않고 지운다.
                assert_eq!(names, ["job-dismiss", "blockdev-del"]);
            } else {
                assert_eq!(names, ["block-job-cancel", "job-dismiss", "blockdev-del"]);
                assert_eq!(
                    sent[0].1,
                    serde_json::json!({ "device": "opl-mirror-0", "force": true })
                );
            }
            assert_eq!(
                sent.last().unwrap().1,
                serde_json::json!({ "node-name": "opl-mirror-0" })
            );
            assert!(fake.nbd_nodes.lock().unwrap().is_empty());
        }
    }

    /// 스위치오버: `pause-before-switchover`로 보내고, `pre-switchover`에서 ready 미러를 맞춰 끝낸 뒤
    /// 지우고 이어 간다.
    #[tokio::test]
    async fn migrate_completes_mirrors_at_pre_switchover() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp_with(&dir, "running", "pre-switchover").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let ready = serde_json::json!([
            mirror_job("opl-mirror-0", "ready", 100, None),
            mirror_job("opl-mirror-1", "ready", 100, None)
        ]);
        fake.jobs.lock().unwrap().extend([
            ready.clone(),
            ready,
            serde_json::json!([
                mirror_job("opl-mirror-0", "concluded", 100, None),
                mirror_job("opl-mirror-1", "concluded", 100, None)
            ]),
        ]);
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
            switchover_mirrors: true,
            switchover_catch_up: None,
            devices: None,
        };
        hv.migrate(vm.id, &target(), &opts).await.unwrap();

        let sent = sent(
            &fake,
            &[
                "migrate-set-capabilities",
                "migrate",
                "block-job-cancel",
                "job-dismiss",
                "blockdev-del",
                "migrate-continue",
                "migrate_cancel",
            ],
        );
        let names: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "migrate-set-capabilities",
                "migrate",
                "block-job-cancel",
                "block-job-cancel",
                "job-dismiss",
                "blockdev-del",
                "job-dismiss",
                "blockdev-del",
                "migrate-continue"
            ]
        );
        assert_eq!(
            sent[0].1["capabilities"],
            serde_json::json!([
                { "capability": "events", "state": true },
                { "capability": "auto-converge", "state": true },
                { "capability": "dirty-bitmaps", "state": true },
                { "capability": "pause-before-switchover", "state": true }
            ])
        );
        // 게스트가 멈춘 때이므로 맞춰 끝낸다(force가 아니다).
        assert_eq!(
            sent[2].1,
            serde_json::json!({ "device": "opl-mirror-0", "force": false })
        );
        assert_eq!(sent[6].1, serde_json::json!({ "id": "opl-mirror-1" }));
        assert_eq!(
            sent[7].1,
            serde_json::json!({ "node-name": "opl-mirror-1" })
        );
        assert_eq!(sent[8].1, serde_json::json!({ "state": "pre-switchover" }));
    }

    /// 스위치오버에서 미러가 오류로 끝나면 마이그레이션을 취소하고 미러를 치운 뒤 실패한다. 미러가 없으면
    /// 보내지 않는다.
    #[tokio::test]
    async fn mirror_error_at_switchover_cancels_the_migration() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp_with(&dir, "running", "pre-switchover").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
            switchover_mirrors: true,
            switchover_catch_up: None,
            devices: None,
        };
        let err = hv.migrate(vm.id, &target(), &opts).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "migration failed: no disk mirrors to switch over"
        );
        assert!(
            !fake
                .commands
                .lock()
                .unwrap()
                .contains(&"migrate".to_string())
        );

        let ready = serde_json::json!([
            mirror_job("opl-mirror-0", "ready", 100, None),
            mirror_job("opl-mirror-1", "ready", 100, None)
        ]);
        let ended = serde_json::json!([
            mirror_job("opl-mirror-0", "concluded", 100, None),
            mirror_job("opl-mirror-1", "concluded", 90, Some("Input/output error"))
        ]);
        fake.jobs.lock().unwrap().extend([
            ready.clone(),
            ready,
            ended.clone(),
            ended.clone(),
            ended,
        ]);
        let err = hv.migrate(vm.id, &target(), &opts).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "migration failed: disk mirror opl-mirror-1 failed: Input/output error"
        );
        let commands = fake.commands.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        assert!(at("block-job-cancel") < at("migrate_cancel"));
        assert!(at("migrate_cancel") < at("job-dismiss"));
        assert!(!commands.contains(&"migrate-continue".to_string()));
        assert!(fake.jobs.lock().unwrap().is_empty());
    }

    /// 되돌리기는 보내는 중이 아니어도 남은 미러를 force로 취소하고 치운다. 증분 백업 전의 정리도 미러를
    /// 치운다.
    #[tokio::test]
    async fn cancel_migration_and_backup_cleanup_clear_left_mirrors() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        for clear_backup in [false, true] {
            fake.nbd_nodes.lock().unwrap().push("opl-mirror-1".into());
            fake.commands.lock().unwrap().clear();
            fake.arguments.lock().unwrap().clear();
            fake.jobs.lock().unwrap().extend([
                serde_json::json!([mirror_job("opl-mirror-1", "ready", 100, None)]),
                serde_json::json!([mirror_job(
                    "opl-mirror-1",
                    "concluded",
                    100,
                    Some("Operation canceled")
                )]),
            ]);
            if clear_backup {
                hv.clear_backup_leftovers(id).await.unwrap();
            } else {
                hv.cancel_migration(id).await.unwrap();
            }
            let sent = sent(
                &fake,
                &[
                    "migrate_cancel",
                    "block-job-cancel",
                    "job-dismiss",
                    "blockdev-del",
                ],
            );
            assert_eq!(
                sent,
                [
                    (
                        "block-job-cancel".to_string(),
                        serde_json::json!({ "device": "opl-mirror-1", "force": true })
                    ),
                    (
                        "job-dismiss".to_string(),
                        serde_json::json!({ "id": "opl-mirror-1" })
                    ),
                    (
                        "blockdev-del".to_string(),
                        serde_json::json!({ "node-name": "opl-mirror-1" })
                    ),
                ]
            );
        }
    }

    /// 따라잡기 요청: disk0는 복제 비트맵 `opl-a` 뒤 바뀐 블록부터, disk1은 전체부터
    fn catch_up_request(task: Uuid, limit: u64, passes: u8) -> CatchUp {
        let targets = mirror_targets()
            .into_iter()
            .map(|t| MirrorTarget {
                export: format!("vol{}", t.disk),
                ..t
            })
            .collect();
        CatchUp {
            targets,
            base: vec![Some("opl-a".into()), None],
            task,
            limit,
            passes,
            speed: Some(1 << 20),
            stall: Duration::from_secs(600),
            timeout: Duration::from_secs(600),
            progress: None,
        }
    }

    /// 임시 비트맵이 담은 바이트를 디스크 노드에 드러낸다.
    fn dirty(fake: &FakeQmp, node: &'static str, name: &str, count: u64) {
        fake.extra_bitmaps.lock().unwrap().push((
            node,
            serde_json::json!({ "name": name, "recording": true, "persistent": false, "count": count }),
        ));
    }

    const CATCH_UP_COMMANDS: [&str; 11] = [
        "object-add",
        "blockdev-add",
        "transaction",
        "job-cancel",
        "job-dismiss",
        "blockdev-del",
        "block-dirty-bitmap-remove",
        "query-jobs",
        "object-del",
        "qom-set",
        "block-dirty-bitmap-merge",
    ];

    /// 두 디스크에 플리싱을 붙이는 명령(`CATCH_UP_COMMANDS`에 드는 것): 이미지 노드 둘 → 새 비트맵 transaction →
    /// 디스크마다 필터와 장치 바꾸기 → snapshot-access 둘
    const FLEECE_ON: [&str; 9] = [
        "blockdev-add",
        "blockdev-add",
        "transaction",
        "blockdev-add",
        "qom-set",
        "blockdev-add",
        "qom-set",
        "blockdev-add",
        "blockdev-add",
    ];

    /// 두 디스크의 플리싱을 떼는 명령: 디스크마다 snapshot-access → 장치 되돌리기 → 필터 → 이미지
    const FLEECE_OFF: [&str; 8] = [
        "blockdev-del",
        "qom-set",
        "blockdev-del",
        "blockdev-del",
        "blockdev-del",
        "qom-set",
        "blockdev-del",
        "blockdev-del",
    ];

    /// 따라잡기: NBD TLS(client) → 디스크마다 NBD 노드 → 패스마다 플리싱을 붙이고(새 임시 비트맵은 그 시점에) 기준
    /// 비트맵(첫 패스는 복제 비트맵, 다음은 앞 임시 비트맵)을 snapshot-access 노드에 옮겨 담은 뒤 transaction 하나(디스크
    /// 마다 blockdev-backup, 기준이 없으면 전체) → 끝을 기다림 → 작업 지우기 → 플리싱 떼기 → 앞 임시 비트맵 지우기. 새
    /// 비트맵이 담은 바이트가 한도 이하면 멈추고 노드는 열어 둔다. 패스 수의 최댓값에서도 멈춘다.
    #[tokio::test]
    async fn catch_up_runs_passes_until_the_changes_are_small() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let task = Uuid::now_v7();
        let (first, second) = (migration_bitmap(task, 1), migration_bitmap(task, 2));
        dirty(&fake, "disk0", &first, 100 << 20);
        dirty(&fake, "disk1", &first, 0);
        dirty(&fake, "disk0", &second, 1 << 20);
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-catchup-0", "concluded", 1000, None),
                repl_job("opl-catchup-1", "concluded", 2000, None)
            ]),
            serde_json::json!([
                repl_job("opl-catchup-0", "concluded", 10, None),
                repl_job("opl-catchup-1", "concluded", 20, None)
            ]),
        ]);
        let (tx, rx) = tokio::sync::watch::channel(MirrorProgress::default());
        let mut req = catch_up_request(task, 64 << 20, 5);
        req.progress = Some(tx);
        let state = hv.catch_up_disks(id, &req).await.unwrap();
        assert_eq!(
            state,
            CatchUpState {
                disks: vec![0, 1],
                bitmap: second.clone(),
                passes: 2
            }
        );
        assert_eq!(rx.borrow().done_bytes, 3030);

        let sent = sent(&fake, &CATCH_UP_COMMANDS);
        let mut expected = vec![
            "query-jobs",
            "object-del",
            "object-add",
            "blockdev-add",
            "blockdev-add",
        ];
        expected.extend(FLEECE_ON);
        expected.extend([
            "block-dirty-bitmap-merge",
            "transaction",
            "query-jobs",
            "job-dismiss",
            "job-dismiss",
        ]);
        expected.extend(FLEECE_OFF);
        expected.extend(FLEECE_ON);
        expected.extend([
            "block-dirty-bitmap-merge",
            "block-dirty-bitmap-merge",
            "transaction",
            "query-jobs",
            "job-dismiss",
            "job-dismiss",
        ]);
        expected.extend(FLEECE_OFF);
        expected.extend(["block-dirty-bitmap-remove", "block-dirty-bitmap-remove"]);
        assert_eq!(names_of(&sent), expected);
        assert_eq!(sent[1].1, serde_json::json!({ "id": "opl-nbd-tls" }));
        assert_eq!(sent[2].1["endpoint"], "client");
        assert_eq!(
            sent[4].1,
            serde_json::json!({
                "driver": "nbd", "node-name": "opl-catchup-nbd-1",
                "server": { "type": "inet", "host": "10.0.0.2", "port": "60007" },
                "export": "vol1", "tls-creds": "opl-nbd-tls", "tls-hostname": "node-b"
            })
        );
        // 첫 패스: 새 임시 비트맵은 플리싱을 붙일 때, disk0은 복제 비트맵 뒤 바뀐 블록만, disk1은 전체
        assert_eq!(
            sent[7].1["actions"],
            serde_json::json!([
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk0", "name": first, "persistent": false } },
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk1", "name": first, "persistent": false } },
            ])
        );
        assert_eq!(
            sent[14].1,
            serde_json::json!({
                "node": "opl-fl-acc-0", "target": "opl-fl-base",
                "bitmaps": [{ "node": "disk0", "name": "opl-a" }]
            })
        );
        assert_eq!(
            sent[15].1["actions"],
            serde_json::json!([
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-catchup-0", "device": "opl-fl-acc-0", "target": "opl-catchup-nbd-0",
                    "sync": "bitmap", "bitmap": "opl-fl-base", "bitmap-mode": "never",
                    "speed": 1048576, "auto-dismiss": false } },
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-catchup-1", "device": "opl-fl-acc-1", "target": "opl-catchup-nbd-1",
                    "sync": "full", "speed": 1048576, "auto-dismiss": false } },
            ])
        );
        // 둘째 패스는 앞 임시 비트맵 뒤 바뀐 블록만
        let second_pass = 19 + FLEECE_OFF.len();
        assert_eq!(
            sent[second_pass + 2].1["actions"][1]["data"]["name"],
            second
        );
        assert_eq!(
            sent[second_pass + 10].1,
            serde_json::json!({
                "node": "opl-fl-acc-1", "target": "opl-fl-base",
                "bitmaps": [{ "node": "disk1", "name": first }]
            })
        );
        assert_eq!(
            sent[second_pass + 11].1["actions"][1]["data"],
            serde_json::json!({
                "job-id": "opl-catchup-1", "device": "opl-fl-acc-1", "target": "opl-catchup-nbd-1",
                "sync": "bitmap", "bitmap": "opl-fl-base", "bitmap-mode": "never",
                "speed": 1048576, "auto-dismiss": false
            })
        );
        // 첫 패스의 복제 비트맵은 남기고 앞 임시 비트맵만 지운다.
        let n = sent.len();
        assert_eq!(
            sent[n - 2].1,
            serde_json::json!({ "node": "disk0", "name": first })
        );
        assert_eq!(
            sent[n - 1].1,
            serde_json::json!({ "node": "disk1", "name": first })
        );
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        assert_eq!(*fake.roots.lock().unwrap(), ["disk0", "disk1"]);
        assert_eq!(
            *fake.nbd_nodes.lock().unwrap(),
            ["opl-catchup-nbd-0", "opl-catchup-nbd-1"]
        );

        // 한도를 넘어도 패스 수의 최댓값에서 멈춘다.
        fake.nbd_nodes.lock().unwrap().clear();
        fake.commands.lock().unwrap().clear();
        fake.arguments.lock().unwrap().clear();
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-catchup-0", "concluded", 1000, None),
                repl_job("opl-catchup-1", "concluded", 2000, None)
            ]),
        ]);
        let state = hv
            .catch_up_disks(id, &catch_up_request(task, 64 << 20, 1))
            .await
            .unwrap();
        assert_eq!((state.passes, state.bitmap), (1, first));
        assert_eq!(
            fake.commands
                .lock()
                .unwrap()
                .iter()
                .filter(|c| *c == "transaction")
                .count(),
            2
        );
    }

    /// 작업 하나가 실패하면 나머지를 취소하고, 노드를 닫고, 이 Task의 임시 비트맵만 지운다(복제 비트맵 `opl-a`는
    /// 남는다).
    #[tokio::test]
    async fn a_failed_catch_up_clears_the_copies_and_its_bitmaps() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let task = Uuid::now_v7();
        let first = migration_bitmap(task, 1);
        dirty(&fake, "disk0", &first, 0);
        dirty(&fake, "disk1", &first, 0);
        dirty(&fake, "disk1", &migration_bitmap(Uuid::now_v7(), 1), 0);
        let failed = || {
            serde_json::json!([
                repl_job("opl-catchup-0", "concluded", 10, Some("Operation canceled")),
                repl_job(
                    "opl-catchup-1",
                    "concluded",
                    0,
                    Some("Connection reset by peer")
                )
            ])
        };
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-catchup-0", "running", 10, None),
                repl_job(
                    "opl-catchup-1",
                    "concluded",
                    0,
                    Some("Connection reset by peer")
                )
            ]),
            failed(),
            // 지우지 못한 작업이 남지 않았다.
            serde_json::json!([]),
        ]);
        let err = hv
            .catch_up_disks(id, &catch_up_request(task, 64 << 20, 5))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "migration failed: disk copy job opl-catchup-1 failed: Connection reset by peer"
        );
        let sent = sent(
            &fake,
            &[
                "job-cancel",
                "job-dismiss",
                "blockdev-del",
                "block-dirty-bitmap-remove",
            ],
        );
        // 플리싱(디스크마다 노드 셋)과 NBD 노드를 닫고 임시 비트맵을 지운다.
        let mut expected = vec!["job-cancel", "job-dismiss", "job-dismiss"];
        expected.extend(["blockdev-del"; 8]);
        expected.extend(["block-dirty-bitmap-remove"; 2]);
        assert_eq!(names_of(&sent), expected);
        assert_eq!(sent[0].1, serde_json::json!({ "id": "opl-catchup-0" }));
        assert_eq!(
            sent[11].1,
            serde_json::json!({ "node": "disk0", "name": first })
        );
        assert_eq!(
            sent[12].1,
            serde_json::json!({ "node": "disk1", "name": first })
        );
        assert!(fake.nbd_nodes.lock().unwrap().is_empty());
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
    }

    /// 따라잡기가 기한(`timeout`)을 넘기면 드라이버가 작업을 취소하고 플리싱을 뗀 뒤(노드·장치·파일이 남지 않는다)
    /// 실패한다.
    #[tokio::test]
    async fn a_catch_up_past_its_deadline_removes_the_fleecing() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let task = Uuid::now_v7();
        let canceled = || {
            serde_json::json!([
                repl_job("opl-catchup-0", "concluded", 10, Some("Operation canceled")),
                repl_job("opl-catchup-1", "concluded", 10, Some("Operation canceled"))
            ])
        };
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-catchup-0", "running", 10, None),
                repl_job("opl-catchup-1", "running", 10, None)
            ]),
            canceled(),
            serde_json::json!([]),
        ]);
        let mut req = catch_up_request(task, 64 << 20, 5);
        req.timeout = Duration::ZERO;
        let err = hv.catch_up_disks(id, &req).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "migration failed: disk copy did not finish within 0ms"
        );
        assert_eq!(sent(&fake, &["job-cancel"]).len(), 2);
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        assert!(fake.nbd_nodes.lock().unwrap().is_empty());
        assert_eq!(*fake.roots.lock().unwrap(), ["disk0", "disk1"]);
        let fleece = tmp.path().join("state").join("vm").join(id.to_string());
        let left = std::fs::read_dir(&fleece)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("fleece-")
            })
            .count();
        assert_eq!(left, 0);
    }

    /// 보내는 동안 플리싱 이미지를 둘 파일 시스템의 남은 공간이 남겨 둘 공간(1 GiB와 파일 시스템의 5% 가운데 큰
    /// 쪽)보다 줄면 작업을 취소하고 공간 부족으로 실패한다.
    #[tokio::test]
    async fn sending_stops_when_the_fleecing_space_runs_low() {
        assert_eq!(fleece_reserve(0), 1 << 30);
        assert_eq!(fleece_reserve(10 << 30), 1 << 30);
        assert_eq!(fleece_reserve(100 << 30), 5 << 30);
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([repl_job("opl-repl-0", "running", 10, None)]),
            serde_json::json!([repl_job(
                "opl-repl-0",
                "concluded",
                10,
                Some("Operation canceled")
            )]),
        ]);
        let handle = hv.handle(id).unwrap();
        let limits = Limits {
            stall: Duration::from_secs(600),
            deadline: None,
            fleece: tmp.path(),
            reserve: |_| u64::MAX,
        };
        let err = wait_sent(
            &handle.qmp,
            &["opl-repl-0".to_string()],
            &limits,
            None,
            MirrorProgress::default(),
            &REPLICATION,
        )
        .await
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.starts_with(&format!(
                "replication failed: not enough space left in {} for the fleecing images: available ",
                tmp.path().display()
            )),
            "{message}"
        );
        assert_eq!(sent(&fake, &["job-cancel"]).len(), 1);
    }

    /// 스위치오버: 따라잡기면 `dirty-bitmaps` 없이 `pause-before-switchover`로 보내고, `pre-switchover`에서 마지막
    /// 비트맵 뒤 바뀐 블록을 보낸 뒤(새 비트맵 없이) 작업을 지우고 노드를 닫고 이어 간다. 마지막 패스가 실패하면
    /// 취소하고 치운 뒤 실패한다.
    #[tokio::test]
    async fn migrate_sends_the_last_catch_up_pass_at_pre_switchover() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp_with(&dir, "running", "pre-switchover").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        fake.nbd_nodes.lock().unwrap().extend([
            "opl-catchup-nbd-0".to_string(),
            "opl-catchup-nbd-1".to_string(),
        ]);
        fake.jobs.lock().unwrap().extend([serde_json::json!([
            repl_job("opl-catchup-0", "concluded", 100, None),
            repl_job("opl-catchup-1", "concluded", 100, None)
        ])]);
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
            switchover_mirrors: false,
            switchover_catch_up: Some(CatchUpState {
                disks: vec![0, 1],
                bitmap: "opl-mig-x-2".into(),
                passes: 2,
            }),
            devices: None,
        };
        hv.migrate(vm.id, &target(), &opts).await.unwrap();
        let sent = sent(
            &fake,
            &[
                "migrate-set-capabilities",
                "migrate",
                "transaction",
                "job-dismiss",
                "blockdev-del",
                "migrate-continue",
                "migrate_cancel",
            ],
        );
        assert_eq!(
            names_of(&sent),
            [
                "migrate-set-capabilities",
                "migrate",
                "transaction",
                "job-dismiss",
                "job-dismiss",
                "blockdev-del",
                "blockdev-del",
                "migrate-continue"
            ]
        );
        // FakeQmp의 raw 디스크에는 저장되지 않는 `opl-` 비트맵이 있지만 따라잡기는 비트맵을 넘기지 않는다.
        assert_eq!(
            sent[0].1["capabilities"],
            serde_json::json!([
                { "capability": "events", "state": true },
                { "capability": "auto-converge", "state": true },
                { "capability": "dirty-bitmaps", "state": false },
                { "capability": "pause-before-switchover", "state": true }
            ])
        );
        assert_eq!(
            sent[2].1["actions"],
            serde_json::json!([
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-catchup-0", "device": "disk0", "target": "opl-catchup-nbd-0",
                    "sync": "bitmap", "bitmap": "opl-mig-x-2", "bitmap-mode": "never",
                    "auto-dismiss": false } },
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-catchup-1", "device": "disk1", "target": "opl-catchup-nbd-1",
                    "sync": "bitmap", "bitmap": "opl-mig-x-2", "bitmap-mode": "never",
                    "auto-dismiss": false } },
            ])
        );
        assert_eq!(
            sent[6].1,
            serde_json::json!({ "node-name": "opl-catchup-nbd-1" })
        );
        assert!(fake.nbd_nodes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_last_catch_up_pass_cancels_the_migration() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp_with(&dir, "running", "pre-switchover").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        fake.nbd_nodes
            .lock()
            .unwrap()
            .push("opl-catchup-nbd-0".to_string());
        let ended = serde_json::json!([repl_job(
            "opl-catchup-0",
            "concluded",
            10,
            Some("Input/output error")
        )]);
        fake.jobs
            .lock()
            .unwrap()
            .extend([ended.clone(), ended, serde_json::json!([])]);
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
            switchover_mirrors: false,
            switchover_catch_up: Some(CatchUpState {
                disks: vec![0],
                bitmap: "opl-mig-x-1".into(),
                passes: 1,
            }),
            devices: None,
        };
        let err = hv.migrate(vm.id, &target(), &opts).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "migration failed: disk copy job opl-catchup-0 failed: Input/output error"
        );
        let commands = fake.commands.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        assert!(at("transaction") < at("migrate_cancel"));
        assert!(!commands.contains(&"migrate-continue".to_string()));
        assert!(fake.nbd_nodes.lock().unwrap().is_empty());
    }

    /// 되돌리기는 남은 따라잡기 작업·노드를 치우고 따라잡기 임시 비트맵을 지운다(다른 비트맵은 그대로).
    #[tokio::test]
    async fn cancel_migration_clears_a_left_catch_up() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.nbd_nodes
            .lock()
            .unwrap()
            .push("opl-catchup-nbd-0".to_string());
        dirty(&fake, "disk0", "opl-mig-x-1", 0);
        dirty(&fake, "disk0", "opl-repl-x-y", 0);
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([repl_job("opl-catchup-0", "running", 10, None)]),
            serde_json::json!([repl_job(
                "opl-catchup-0",
                "concluded",
                10,
                Some("Operation canceled")
            )]),
        ]);
        hv.cancel_migration(id).await.unwrap();
        let sent = sent(
            &fake,
            &[
                "job-cancel",
                "job-dismiss",
                "blockdev-del",
                "block-dirty-bitmap-remove",
            ],
        );
        assert_eq!(
            sent,
            [
                (
                    "job-cancel".to_string(),
                    serde_json::json!({ "id": "opl-catchup-0" })
                ),
                (
                    "job-dismiss".to_string(),
                    serde_json::json!({ "id": "opl-catchup-0" })
                ),
                (
                    "blockdev-del".to_string(),
                    serde_json::json!({ "node-name": "opl-catchup-nbd-0" })
                ),
                (
                    "block-dirty-bitmap-remove".to_string(),
                    serde_json::json!({ "node": "disk0", "name": "opl-mig-x-1" })
                ),
            ]
        );
    }

    /// 받는 쪽 넘겨받기: 비트맵을 주면 NBD를 멈춘 뒤·이어서 실행하기 전에 모든 디스크에 만든다(qcow2만 저장). 만들지
    /// 못해도 넘겨받는다.
    #[tokio::test]
    async fn finish_incoming_adds_the_reversed_replica_bitmap_before_continuing() {
        for fails in [false, true] {
            let tmp = short_tempdir();
            let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
            let vm = spec();
            let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
            let _qemu = hold_pidfile(&dir).await;
            std::fs::write(dir.join(INCOMING_MARK), format!("{}\n", Uuid::now_v7())).unwrap();
            let fake = fake_qmp(&dir, "paused").await;
            if fails {
                fake.failing.lock().unwrap().push("transaction");
            }
            let hv = QemuHypervisor::new(cfg).await.unwrap();
            hv.finish_incoming(vm.id, Some("opl-repl-back"))
                .await
                .unwrap();
            let sent = sent(&fake, &["nbd-server-stop", "transaction", "cont"]);
            assert_eq!(
                names_of(&sent),
                ["nbd-server-stop", "transaction", "cont"],
                "{fails}"
            );
            assert_eq!(
                sent[1].1["actions"],
                serde_json::json!([
                    { "type": "block-dirty-bitmap-add",
                      "data": { "node": "disk0", "name": "opl-repl-back", "persistent": true } },
                    { "type": "block-dirty-bitmap-add",
                      "data": { "node": "disk1", "name": "opl-repl-back", "persistent": false } },
                ])
            );
            assert!(!dir.join(INCOMING_MARK).exists());
        }
    }

    /// 받는 쪽: 내보낼 디스크가 있으면 TLS NBD 서버(받는 쪽 객체, 상대 확인)와 디스크마다 쓰기 가능
    /// 내보내기를 먼저 열고, 그 포트를 뺀 다른 포트에서 받는다.
    #[tokio::test]
    async fn incoming_exports_disks_on_a_second_port() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let first = free.local_addr().unwrap().port();
        drop(free);
        let ports = hv
            .listen_incoming(id, "127.0.0.1", first..=first.saturating_add(50), &[0, 1])
            .await
            .unwrap();
        let nbd = ports.nbd.unwrap();
        assert_ne!(ports.migration, nbd);

        let sent = sent(
            &fake,
            &[
                "migrate-set-capabilities",
                "object-add",
                "nbd-server-start",
                "block-export-add",
                "migrate-incoming",
            ],
        );
        let names: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "object-add",
                "migrate-set-capabilities",
                "object-add",
                "nbd-server-start",
                "block-export-add",
                "block-export-add",
                "migrate-incoming"
            ]
        );
        assert_eq!(sent[2].1["id"], "opl-nbd-tls");
        assert_eq!(sent[2].1["endpoint"], "server");
        assert_eq!(sent[2].1["verify-peer"], true);
        assert_eq!(
            sent[3].1,
            serde_json::json!({
                "addr": { "type": "inet", "data": { "host": "127.0.0.1", "port": nbd.to_string() } },
                "tls-creds": "opl-nbd-tls"
            })
        );
        assert_eq!(
            sent[5].1,
            serde_json::json!({
                "type": "nbd", "id": "opl-export-1", "node-name": "disk1",
                "name": "disk1", "writable": true
            })
        );
        assert_eq!(
            sent[6].1,
            serde_json::json!({ "uri": format!("tcp:127.0.0.1:{}", ports.migration) })
        );

        // 내보낼 디스크가 없으면 NBD 서버를 열지 않는다.
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let ports = hv
            .listen_incoming(id, "127.0.0.1", first..=first.saturating_add(50), &[])
            .await
            .unwrap();
        assert_eq!(ports.nbd, None);
        let commands = fake.commands.lock().unwrap().clone();
        assert!(!commands.contains(&"nbd-server-start".to_string()));
        assert!(!commands.contains(&"block-export-add".to_string()));
    }

    /// 받은 VM은 멈춰 있으면 이어서 실행하고 표시를 지운다. 아직 받는 중이면 건드리지 않는다.
    #[tokio::test]
    async fn finish_incoming_continues_only_received_vms() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let (received, receiving) = (spec(), spec());
        let dir_a = cfg.run_dir.join("vm").join(received.id.to_string());
        let dir_b = cfg.run_dir.join("vm").join(receiving.id.to_string());
        let _qa = hold_pidfile(&dir_a).await;
        let _qb = hold_pidfile(&dir_b).await;
        // 표시에는 마이그레이션 Task가 있다. 읽을 수 없으면 nil이다.
        let task = Uuid::now_v7();
        std::fs::write(dir_a.join(INCOMING_MARK), format!("{task}\n")).unwrap();
        std::fs::write(dir_b.join(INCOMING_MARK), b"").unwrap();
        let fake_a = fake_qmp(&dir_a, "paused").await;
        let fake_b = fake_qmp_with(&dir_b, "inmigrate", "active").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let mut incoming = hv.list_incoming().await.unwrap();
        incoming.sort();
        let mut expected = vec![(received.id, task), (receiving.id, Uuid::nil())];
        expected.sort();
        assert_eq!(incoming, expected);
        // 받는 중인 VM은 reconciler의 이어 가기에도 이어서 실행되지 않는다.
        assert_eq!(
            hv.inspect(receiving.id).await.unwrap().unwrap().status,
            RuntimeStatus::Other("inmigrate".into())
        );
        hv.resume_left_stopped(receiving.id).await.unwrap();
        hv.resume_left_stopped(received.id).await.unwrap();
        assert!(
            !fake_a
                .commands
                .lock()
                .unwrap()
                .contains(&"cont".to_string())
        );

        hv.incoming_wait(received.id).await.unwrap();
        // 디스크를 내보내지 않았으면 NBD 서버가 없어 멈추기가 실패해도 넘겨받는다.
        fake_a.failing.lock().unwrap().push("nbd-server-stop");
        hv.finish_incoming(received.id, None).await.unwrap();
        let commands = fake_a.commands.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        assert!(at("nbd-server-stop") < at("cont"));
        assert!(!dir_a.join(INCOMING_MARK).exists());
        assert_eq!(
            hv.list_incoming().await.unwrap(),
            vec![(receiving.id, Uuid::nil())]
        );
        // 표시가 없으면 아무것도 하지 않고, 기다릴 것도 없다.
        hv.finish_incoming(received.id, None).await.unwrap();
        assert!(matches!(
            hv.incoming_wait(received.id).await,
            Err(DriverError::Migration(_))
        ));

        let err = hv.finish_incoming(receiving.id, None).await.unwrap_err();
        assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
        assert!(dir_b.join(INCOMING_MARK).exists());
        // 아직 받는 중이면 내보내기(원본이 미러하는 중)도 그대로 둔다.
        let commands = fake_b.commands.lock().unwrap().clone();
        assert!(!commands.contains(&"cont".to_string()));
        assert!(!commands.contains(&"nbd-server-stop".to_string()));
    }

    /// 보낸 원본(`postmigrate`)이 아니면 끄지 않는다.
    #[tokio::test]
    async fn stop_migrated_leaves_running_vm_alone() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!hv.stop_migrated(vm.id).await.unwrap());
        assert!(!hv.stop_migrated(Uuid::now_v7()).await.unwrap());
        assert!(!fake.commands.lock().unwrap().contains(&"quit".to_string()));
    }

    /// 이미 이 노드에서 도는 VM은 받지 않는다(그 QEMU를 건드리지 않는다).
    #[tokio::test]
    async fn start_incoming_refuses_a_running_vm() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let err = hv
            .start_incoming(&vm, Uuid::now_v7(), "127.0.0.1", 60000..=60099, &[])
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
        assert!(dir.join("qemu.pid").exists());
        assert!(!dir.join(INCOMING_MARK).exists());
        assert!(!fake.commands.lock().unwrap().contains(&"quit".to_string()));
    }

    #[tokio::test]
    async fn start_incoming_failure_cleans_run_dir() {
        let tmp = short_tempdir();
        // 곧바로 실패로 끝나는 QEMU
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("false")))
            .await
            .unwrap();
        let vm = spec();
        let err = hv
            .start_incoming(&vm, Uuid::now_v7(), "127.0.0.1", 60000..=60099, &[])
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::StartFailed(_)), "{err:?}");
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
        assert!(hv.list_incoming().await.unwrap().is_empty());
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
        hv.snapshot_save(vm.id, "s1", false, false).await.unwrap();
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
        // inconsistent 비트맵이 있으면 크기를 바꿀 수 없으므로 먼저 지운다.
        assert_eq!(
            arguments[at("block-dirty-bitmap-remove")],
            serde_json::json!({ "node": "disk0", "name": "opl-old" })
        );
        assert!(at("block-dirty-bitmap-remove") < at("block_resize"));
    }

    /// 마이그레이션 능력 `dirty-bitmaps`는 이미지에 저장되지 않는 `opl-` 비트맵이 있을 때만 켠다. qcow2의
    /// persistent 비트맵은 공유 이미지로 넘어가고, 능력을 켠 채 실패하면 이미지에 in-use로 남는다.
    #[test]
    fn only_volatile_opl_bitmaps_need_bitmap_migration() {
        let bitmap = |name: &str, persistent: bool| BitmapInfo {
            name: name.into(),
            recording: true,
            inconsistent: false,
            persistent,
            count: 0,
        };
        let node = |name: &str, bitmaps: Vec<BitmapInfo>| BlockNode {
            node_name: name.into(),
            driver: "qcow2".into(),
            bitmaps,
        };
        assert!(!volatile_bitmaps(&[]));
        assert!(!volatile_bitmaps(&[
            node("disk0", vec![bitmap("opl-a", true)]),
            node("disk1", vec![bitmap("user", false)]),
        ]));
        assert!(volatile_bitmaps(&[
            node("disk0", vec![bitmap("opl-a", true)]),
            node("disk1", vec![bitmap("opl-a", false)]),
        ]));
    }

    /// 미러하는 디스크는 대상이 다른 이미지이므로 persistent `opl-` 비트맵도 스트림으로 보낸다.
    #[test]
    fn mirrored_disks_need_bitmap_migration() {
        let bitmap = |name: &str| BitmapInfo {
            name: name.into(),
            recording: true,
            inconsistent: false,
            persistent: true,
            count: 0,
        };
        let disks = [
            BlockNode {
                node_name: "disk0".into(),
                driver: "qcow2".into(),
                bitmaps: vec![bitmap("user")],
            },
            BlockNode {
                node_name: "disk1".into(),
                driver: "qcow2".into(),
                bitmaps: vec![bitmap("opl-a")],
            },
        ];
        assert!(!migrated_bitmaps(&disks, &[]));
        assert!(!migrated_bitmaps(&disks, &[0]));
        assert!(migrated_bitmaps(&disks, &[1]));
        let job = |id: &str| JobInfo {
            id: id.into(),
            status: "ready".into(),
            error: None,
            current_progress: 0,
            total_progress: 0,
        };
        assert_eq!(
            mirrored_disks(&[
                job("opl-mirror-2"),
                job("opl-backup-0"),
                job("opl-mirror-0")
            ]),
            [0, 2]
        );
    }

    #[test]
    fn disk_block_nodes_are_in_disk_order() {
        let node = |name: &str| BlockNode {
            node_name: name.into(),
            driver: "qcow2".into(),
            bitmaps: vec![],
        };
        let nodes = ["disk10", "disk2-file", "disk2", "opl-backup-0", "disk0"].map(node);
        let names: Vec<String> = disk_block_nodes(nodes.to_vec())
            .into_iter()
            .map(|n| n.node_name)
            .collect();
        assert_eq!(names, ["disk0", "disk2", "disk10"]);
    }

    /// 가짜 QMP에 연결된 실행 중인 VM 하나
    async fn running_vm(tmp: &Path) -> (QemuHypervisor, Uuid, FakeQmp, tokio::process::Child) {
        let cfg = config(tmp, PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        (hv, vm.id, fake, qemu)
    }

    /// 보낸 명령 중 `names`에 든 것만 (이름, 인자)로
    fn sent(fake: &FakeQmp, names: &[&str]) -> Vec<(String, serde_json::Value)> {
        let commands = fake.commands.lock().unwrap().clone();
        let arguments = fake.arguments.lock().unwrap().clone();
        commands
            .into_iter()
            .zip(arguments)
            .filter(|(c, _)| names.contains(&c.as_str()))
            .collect()
    }

    #[tokio::test]
    async fn disk_stats_sum_only_the_vm_disks() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;

        // pflash는 빼고 qdev가 `disk{i}-dev`인 백엔드만 합한다(맨 위 노드 이름과 상관없이).
        assert_eq!(
            hv.disk_stats(id).await.unwrap(),
            DiskCounters {
                rd_bytes: 4196,
                wr_bytes: 1224,
                rd_operations: 4,
                wr_operations: 6,
            }
        );
        assert_eq!(sent(&fake, &["query-blockstats"]).len(), 1);
        assert!(matches!(
            hv.disk_stats(Uuid::now_v7()).await,
            Err(DriverError::NotRunning(_))
        ));

        fake.failing.lock().unwrap().push("query-blockstats");
        assert!(matches!(
            hv.disk_stats(id).await,
            Err(DriverError::Qmp(QmpError::Command { .. }))
        ));
    }

    #[test]
    fn disk_device_index_reads_the_qdev_id() {
        for (qdev, want) in [
            ("/machine/peripheral/disk0-dev/virtio-backend", Some(0)),
            ("/machine/peripheral/disk12-dev", Some(12)),
            ("/machine/peripheral/nic0", None),
            ("/machine/peripheral/disk0", None),
            ("/machine/system.flash0", None),
        ] {
            assert_eq!(disk_device_index(qdev), want, "{qdev}");
        }
    }

    #[tokio::test]
    async fn bitmap_ops_use_disk_nodes_and_disk_formats() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;

        let disks = hv.backup_bitmaps(id).await.unwrap();
        let nodes: Vec<&str> = disks.iter().map(|d| d.node.as_str()).collect();
        assert_eq!(nodes, ["disk0", "disk1"]);
        assert_eq!(disks[0].bitmaps.len(), 3);
        assert!(disks[0].bitmaps[2].inconsistent);
        assert!(!disks[1].bitmaps[0].persistent);

        // qcow2 디스크의 비트맵만 persistent다. 스냅샷과 비트맵은 transaction 하나다.
        hv.snapshot_save_with_bitmap(id, "t", "opl-n", false)
            .await
            .unwrap();
        hv.add_bitmaps(id, "opl-m").await.unwrap();
        let transactions = sent(&fake, &["transaction"]);
        assert_eq!(
            transactions[0].1["actions"],
            serde_json::json!([
                { "type": "blockdev-snapshot-internal-sync", "data": { "device": "disk0", "name": "t" } },
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk0", "name": "opl-n", "persistent": true } },
                { "type": "blockdev-snapshot-internal-sync", "data": { "device": "disk1", "name": "t" } },
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk1", "name": "opl-n", "persistent": false } },
            ])
        );
        assert_eq!(
            transactions[1].1["actions"],
            serde_json::json!([
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk0", "name": "opl-m", "persistent": true } },
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk1", "name": "opl-m", "persistent": false } },
            ])
        );

        // 백업 비트맵 중 keep이 아닌 것만 지운다. 같은 `opl-` 무리의 복제 비트맵과 opl이 만들지 않은 비트맵은
        // 그대로다.
        let (keep, old) = (
            opl_types::backup::bitmap_name(Uuid::now_v7()),
            opl_types::backup::bitmap_name(Uuid::now_v7()),
        );
        let repl = opl_types::replication::replica_bitmap(Uuid::now_v7(), Uuid::now_v7());
        for name in [&keep, &old, &repl] {
            let bitmap = serde_json::json!({ "name": name, "recording": true, "persistent": true });
            fake.extra_bitmaps.lock().unwrap().push(("disk0", bitmap));
        }
        hv.remove_bitmaps(id, Some(&keep)).await.unwrap();
        let removed = || sent(&fake, &["block-dirty-bitmap-remove"]);
        assert_eq!(
            removed(),
            [(
                "block-dirty-bitmap-remove".to_string(),
                serde_json::json!({ "node": "disk0", "name": old })
            )]
        );
        // 이름을 준 비트맵만 디스크마다 지운다. 없는 이름은 건너뛴다.
        hv.remove_named_bitmaps(id, &["opl-a".into(), repl.clone(), "opl-none".into()])
            .await
            .unwrap();
        let args: Vec<serde_json::Value> = removed().into_iter().skip(1).map(|(_, a)| a).collect();
        assert_eq!(
            args,
            [
                serde_json::json!({ "node": "disk0", "name": "opl-a" }),
                serde_json::json!({ "node": "disk0", "name": repl }),
                serde_json::json!({ "node": "disk1", "name": "opl-a" }),
            ]
        );
    }

    fn job(id: &str, status: &str, error: Option<&str>) -> serde_json::Value {
        let mut job = serde_json::json!({ "id": id, "type": "backup", "status": status });
        if let Some(e) = error {
            job["error"] = serde_json::json!(e);
        }
        job
    }

    fn backup_targets() -> Vec<BackupTarget> {
        (0..2)
            .map(|i| BackupTarget {
                node: format!("disk{i}"),
                path: PathBuf::from(format!("/b/x/disk{i}.qcow2.part")),
                size: 1 << 30,
            })
            .collect()
    }

    /// 대상 노드를 열고, 새 비트맵과 백업 작업을 한 transaction으로 시작해 끝을 기다린 뒤 작업을 지우고
    /// 노드를 닫는다. 성공하면 앞 비트맵을 지운다.
    #[tokio::test]
    async fn incremental_backup_runs_one_job_per_disk() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            // 앞선 백업이 남긴 것이 없다.
            serde_json::json!([]),
            serde_json::json!([
                job("opl-backup-0", "concluded", None),
                job("opl-backup-1", "concluded", None)
            ]),
        ]);
        hv.incremental_backup(id, "opl-a", "opl-n", &backup_targets(), false)
            .await
            .unwrap();

        let block = [
            "blockdev-add",
            "transaction",
            "job-dismiss",
            "blockdev-del",
            "block-dirty-bitmap-remove",
            "job-cancel",
        ];
        let sent = sent(&fake, &block);
        let names: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "blockdev-add",
                "blockdev-add",
                "transaction",
                "job-dismiss",
                "job-dismiss",
                "blockdev-del",
                "blockdev-del",
                "block-dirty-bitmap-remove",
                "block-dirty-bitmap-remove",
            ]
        );
        assert_eq!(
            sent[1].1,
            serde_json::json!({
                "driver": "qcow2", "node-name": "opl-backup-1",
                "file": { "driver": "file", "filename": "/b/x/disk1.qcow2.part" },
                "backing": null
            })
        );
        let actions = &sent[2].1["actions"];
        assert_eq!(
            actions[0],
            serde_json::json!({ "type": "block-dirty-bitmap-add",
                "data": { "node": "disk0", "name": "opl-n", "persistent": true } })
        );
        assert_eq!(actions[1]["type"], "blockdev-backup");
        assert_eq!(actions[1]["data"]["device"], "disk0");
        assert_eq!(actions[1]["data"]["target"], "opl-backup-0");
        assert_eq!(actions[1]["data"]["bitmap"], "opl-a");
        assert_eq!(actions[2]["data"]["persistent"], false);
        assert_eq!(actions[3]["data"]["job-id"], "opl-backup-1");
        assert_eq!(sent[3].1, serde_json::json!({ "id": "opl-backup-0" }));
        assert_eq!(
            sent[6].1,
            serde_json::json!({ "node-name": "opl-backup-1" })
        );
        assert_eq!(
            sent[7].1,
            serde_json::json!({ "node": "disk0", "name": "opl-a" })
        );
        assert_eq!(
            sent[8].1,
            serde_json::json!({ "node": "disk1", "name": "opl-a" })
        );
    }

    /// 작업 하나가 실패하면 나머지를 취소하고, 먼저 실패한 작업의 오류로 끝난다. 새 비트맵을 지운다.
    #[tokio::test]
    async fn failed_incremental_backup_cancels_the_other_jobs_and_drops_the_new_bitmap() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                job("opl-backup-0", "concluded", Some("No space left on device")),
                job("opl-backup-1", "running", None)
            ]),
            serde_json::json!([
                job("opl-backup-0", "concluded", Some("No space left on device")),
                job("opl-backup-1", "concluded", Some("Operation canceled"))
            ]),
        ]);
        let err = hv
            .incremental_backup(id, "opl-a", "opl-n", &backup_targets(), false)
            .await
            .unwrap_err();
        match err {
            DriverError::Qmp(QmpError::JobFailed { id, error }) => {
                assert_eq!(id, "opl-backup-0");
                assert_eq!(error, "No space left on device");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let sent = sent(
            &fake,
            &[
                "job-cancel",
                "job-dismiss",
                "blockdev-del",
                "block-dirty-bitmap-remove",
            ],
        );
        let names: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "job-cancel",
                "job-dismiss",
                "job-dismiss",
                "blockdev-del",
                "blockdev-del",
                "block-dirty-bitmap-remove",
                "block-dirty-bitmap-remove",
            ]
        );
        assert_eq!(sent[0].1, serde_json::json!({ "id": "opl-backup-1" }));
        assert_eq!(
            sent[5].1,
            serde_json::json!({ "node": "disk0", "name": "opl-n" })
        );
    }

    /// 앞선 백업이 남긴 작업(opld가 기다리다 죽음)은 취소하고 지운 뒤에 시작한다. 대상이 디스크와
    /// 맞지 않으면 아무것도 하지 않는다.
    #[tokio::test]
    async fn incremental_backup_clears_leftovers_and_checks_targets() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([
                job("opl-backup-0", "running", None),
                job("other", "running", None)
            ]),
            serde_json::json!([job("opl-backup-0", "concluded", Some("Operation canceled"))]),
        ]);
        let err = hv
            .incremental_backup(id, "opl-a", "opl-n", &backup_targets()[..1], false)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        let sent = sent(
            &fake,
            &["job-cancel", "job-dismiss", "blockdev-add", "transaction"],
        );
        assert_eq!(
            sent,
            [
                (
                    "job-cancel".to_string(),
                    serde_json::json!({ "id": "opl-backup-0" })
                ),
                (
                    "job-dismiss".to_string(),
                    serde_json::json!({ "id": "opl-backup-0" })
                ),
            ]
        );
    }

    /// 대상 노드를 닫을 때 QEMU가 이미지 메타데이터를 쓰므로, 닫지 못하면 작업이 성공했어도 백업
    /// 실패다. 앞 비트맵을 남기고 새 비트맵을 지운다(다음도 같은 부모에서 증분).
    #[tokio::test]
    async fn incremental_backup_fails_when_a_target_cannot_be_closed() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                job("opl-backup-0", "concluded", None),
                job("opl-backup-1", "concluded", None)
            ]),
        ]);
        fake.failing.lock().unwrap().push("blockdev-del");
        let err = hv
            .incremental_backup(id, "opl-a", "opl-n", &backup_targets(), false)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::Qmp(QmpError::Command { desc, .. }) if desc == "blockdev-del failed"),
            "{err:?}"
        );
        let sent = sent(&fake, &["blockdev-del", "block-dirty-bitmap-remove"]);
        let names: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
        // 두 노드를 모두 닫아 본 뒤에 비트맵을 고른다.
        assert_eq!(
            names,
            [
                "blockdev-del",
                "blockdev-del",
                "block-dirty-bitmap-remove",
                "block-dirty-bitmap-remove",
            ]
        );
        assert_eq!(
            sent[2].1,
            serde_json::json!({ "node": "disk0", "name": "opl-n" })
        );
        assert_eq!(
            sent[3].1,
            serde_json::json!({ "node": "disk1", "name": "opl-n" })
        );
    }

    /// 작업이 실패했으면 작업을 지우지 못해도 작업의 오류로 끝난다. 작업을 모두 지워 본다.
    #[tokio::test]
    async fn incremental_backup_reports_the_job_error_over_a_dismiss_error() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                job("opl-backup-0", "concluded", Some("No space left on device")),
                job("opl-backup-1", "concluded", None)
            ]),
        ]);
        fake.failing.lock().unwrap().push("job-dismiss");
        let err = hv
            .incremental_backup(id, "opl-a", "opl-n", &backup_targets(), false)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::Qmp(QmpError::JobFailed { id, .. }) if id == "opl-backup-0"),
            "{err:?}"
        );
        assert_eq!(sent(&fake, &["job-dismiss"]).len(), 2);
    }

    /// 남은 작업이 취소하기 전에 끝났으면 `job-cancel`이 실패해도 그대로 지운다.
    #[tokio::test]
    async fn clearing_leftovers_ignores_jobs_that_ended_before_the_cancel() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([job("opl-backup-0", "running", None)]),
            serde_json::json!([job("opl-backup-0", "concluded", None)]),
        ]);
        fake.failing.lock().unwrap().push("job-cancel");
        hv.clear_backup_leftovers(id).await.unwrap();
        let sent = sent(&fake, &["job-cancel", "job-dismiss"]);
        let names: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, ["job-cancel", "job-dismiss"]);
    }

    fn repl_job(id: &str, status: &str, done: u64, error: Option<&str>) -> serde_json::Value {
        let mut job = serde_json::json!({
            "id": id, "type": "backup", "status": status,
            "current-progress": done, "total-progress": done
        });
        if let Some(e) = error {
            job["error"] = serde_json::json!(e);
        }
        job
    }

    /// disk0는 전체, disk1은 기준 비트맵 `opl-a` 뒤 바뀐 블록만
    fn replicate_request(stall: Duration) -> ReplicateRequest {
        let target = |disk: usize, base: Option<&str>| crate::replication::ReplicaDiskTarget {
            disk,
            host: "10.0.0.2".into(),
            port: 60001,
            export: format!("vol{disk}"),
            tls_hostname: "node-b".into(),
            base: base.map(str::to_string),
        };
        ReplicateRequest {
            targets: vec![target(0, None), target(1, Some("opl-a"))],
            bitmap: "opl-repl-new".into(),
            speed: Some(1 << 20),
            stall,
            progress: None,
            quiesce: false,
        }
    }

    const REPLICATION_COMMANDS: [&str; 14] = [
        "object-add",
        "blockdev-add",
        "transaction",
        "job-cancel",
        "job-dismiss",
        "blockdev-del",
        "block-dirty-bitmap-remove",
        "query-jobs",
        "object-del",
        "stop",
        "cont",
        "qom-set",
        "block-dirty-bitmap-add",
        "block-dirty-bitmap-merge",
    ];

    fn names_of(sent: &[(String, serde_json::Value)]) -> Vec<&str> {
        sent.iter().map(|(c, _)| c.as_str()).collect()
    }

    /// 복제 회차: TLS(client) → 디스크마다 NBD 노드 → 플리싱(디스크마다 성긴 플리싱 이미지를 열고, 게스트를 멈춘
    /// 채 새 비트맵을 transaction으로 만들고 copy-before-write 필터(`break-snapshot`)를 끼워 장치가 쓰게 한 뒤 이어서
    /// 실행, snapshot-access 노드를 열고 기준 비트맵을 그곳에 옮겨 담음) → transaction 하나(디스크마다 snapshot-access
    /// 노드에서 blockdev-backup) → 끝을 기다림 → 작업 지우기 → 플리싱 떼기(반대 순서, 이미지 지움) → 노드 닫기. 보낸
    /// 바이트는 작업들의 진행 합이다. 성공하면 비트맵을 지우지 않는다.
    #[tokio::test]
    async fn replicate_disks_sends_one_backup_per_disk_in_one_transaction() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            // 앞선 회차가 남긴 것이 없다.
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-repl-0", "running", 1000, None),
                repl_job("opl-repl-1", "concluded", 65536, None)
            ]),
            serde_json::json!([
                repl_job("opl-repl-0", "concluded", 1 << 26, None),
                repl_job("opl-repl-1", "concluded", 65536, None)
            ]),
        ]);
        let (tx, rx) = tokio::sync::watch::channel(MirrorProgress::default());
        let mut req = replicate_request(Duration::from_secs(600));
        req.progress = Some(tx);
        let (bytes, quiesced) = hv.replicate_disks(id, &req).await.unwrap();
        assert_eq!(bytes, (1 << 26) + 65536);
        assert_eq!(quiesced, Quiesced::No);
        assert_eq!(rx.borrow().done_bytes, bytes);

        let sent = sent(&fake, &REPLICATION_COMMANDS);
        assert_eq!(
            names_of(&sent),
            [
                "query-jobs",
                "object-del",
                "object-add",
                "blockdev-add",
                "blockdev-add",
                "blockdev-add",
                "blockdev-add",
                "stop",
                "transaction",
                "blockdev-add",
                "qom-set",
                "blockdev-add",
                "qom-set",
                "cont",
                "blockdev-add",
                "blockdev-add",
                "block-dirty-bitmap-add",
                "block-dirty-bitmap-merge",
                "transaction",
                "query-jobs",
                "query-jobs",
                "job-dismiss",
                "job-dismiss",
                "blockdev-del",
                "qom-set",
                "blockdev-del",
                "blockdev-del",
                "blockdev-del",
                "qom-set",
                "blockdev-del",
                "blockdev-del",
                "blockdev-del",
                "blockdev-del",
            ]
        );
        assert_eq!(sent[1].1, serde_json::json!({ "id": "opl-repl-tls" }));
        assert_eq!(sent[2].1["id"], "opl-repl-tls");
        assert_eq!(sent[2].1["endpoint"], "client");
        assert_eq!(
            sent[4].1,
            serde_json::json!({
                "driver": "nbd", "node-name": "opl-repl-nbd-1",
                "server": { "type": "inet", "host": "10.0.0.2", "port": "60001" },
                "export": "vol1", "tls-creds": "opl-repl-tls", "tls-hostname": "node-b"
            })
        );
        let fleece = tmp.path().join("state").join("vm").join(id.to_string());
        assert_eq!(
            sent[5].1,
            serde_json::json!({
                "driver": "file", "node-name": "opl-fl-tmp-0",
                "filename": fleece.join("fleece-0.img").display().to_string()
            })
        );
        assert_eq!(
            sent[8].1["actions"],
            serde_json::json!([
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk0", "name": "opl-repl-new", "persistent": true } },
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk1", "name": "opl-repl-new", "persistent": false } },
            ])
        );
        assert_eq!(
            sent[9].1,
            serde_json::json!({
                "driver": "copy-before-write", "node-name": "opl-fl-cbw-0", "file": "disk0",
                "target": "opl-fl-tmp-0", "on-cbw-error": "break-snapshot", "cbw-timeout": 30
            })
        );
        assert_eq!(
            sent[10].1,
            serde_json::json!({
                "path": "/machine/peripheral/disk0-dev/virtio-backend", "property": "drive",
                "value": "opl-fl-cbw-0"
            })
        );
        assert_eq!(
            sent[15].1,
            serde_json::json!({
                "driver": "snapshot-access", "node-name": "opl-fl-acc-1", "file": "opl-fl-cbw-1"
            })
        );
        assert_eq!(
            sent[16].1,
            serde_json::json!({ "node": "opl-fl-acc-1", "name": "opl-fl-base", "persistent": false })
        );
        assert_eq!(
            sent[17].1,
            serde_json::json!({
                "node": "opl-fl-acc-1", "target": "opl-fl-base",
                "bitmaps": [{ "node": "disk1", "name": "opl-a" }]
            })
        );
        assert_eq!(
            sent[18].1["actions"],
            serde_json::json!([
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-repl-0", "device": "opl-fl-acc-0", "target": "opl-repl-nbd-0",
                    "sync": "full", "speed": 1048576, "auto-dismiss": false } },
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-repl-1", "device": "opl-fl-acc-1", "target": "opl-repl-nbd-1",
                    "sync": "bitmap", "bitmap": "opl-fl-base", "bitmap-mode": "never",
                    "speed": 1048576, "auto-dismiss": false } },
            ])
        );
        // 플리싱은 디스크마다 snapshot-access → 장치를 디스크 노드로 되돌림 → 필터 → 이미지 순서로 뗀다.
        let unfleeced: Vec<serde_json::Value> =
            sent[23..31].iter().map(|(_, a)| a.clone()).collect();
        assert_eq!(
            unfleeced,
            [
                serde_json::json!({ "node-name": "opl-fl-acc-1" }),
                serde_json::json!({
                    "path": "/machine/peripheral/disk1-dev", "property": "drive", "value": "disk1"
                }),
                serde_json::json!({ "node-name": "opl-fl-cbw-1" }),
                serde_json::json!({ "node-name": "opl-fl-tmp-1" }),
                serde_json::json!({ "node-name": "opl-fl-acc-0" }),
                serde_json::json!({
                    "path": "/machine/peripheral/disk0-dev/virtio-backend", "property": "drive",
                    "value": "disk0"
                }),
                serde_json::json!({ "node-name": "opl-fl-cbw-0" }),
                serde_json::json!({ "node-name": "opl-fl-tmp-0" }),
            ]
        );
        assert_eq!(
            sent[32].1,
            serde_json::json!({ "node-name": "opl-repl-nbd-1" })
        );
        assert!(fake.nbd_nodes.lock().unwrap().is_empty());
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        assert_eq!(*fake.roots.lock().unwrap(), ["disk0", "disk1"]);
        // 플리싱 이미지와 멈춤 표시가 남지 않는다.
        assert!(std::fs::read_dir(&fleece).unwrap().next().is_none());
        let run = tmp.path().join("run").join("vm").join(id.to_string());
        assert!(!run.join(PAUSED_MARK).exists());
    }

    /// 작업 하나가 실패하면 나머지를 취소하고, 작업을 지우고, 노드를 닫은 뒤 새 비트맵만 지운다(기준 비트맵
    /// `opl-a`는 남는다).
    #[tokio::test]
    async fn failed_replication_cancels_the_rest_and_drops_only_the_new_bitmap() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-repl-0", "running", 10, None),
                repl_job(
                    "opl-repl-1",
                    "concluded",
                    0,
                    Some("Connection reset by peer")
                )
            ]),
            serde_json::json!([
                repl_job("opl-repl-0", "concluded", 10, Some("Operation canceled")),
                repl_job(
                    "opl-repl-1",
                    "concluded",
                    0,
                    Some("Connection reset by peer")
                )
            ]),
        ]);
        let err = hv
            .replicate_disks(id, &replicate_request(Duration::from_secs(600)))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::Replication(m)
                if m == "replication job opl-repl-1 failed: Connection reset by peer"),
            "{err:?}"
        );
        let sent = sent(
            &fake,
            &[
                "job-cancel",
                "job-dismiss",
                "blockdev-del",
                "block-dirty-bitmap-remove",
            ],
        );
        // 플리싱(디스크마다 노드 셋)과 NBD 노드를 닫은 뒤 새 비트맵을 지운다.
        let mut expected = vec!["job-cancel", "job-dismiss", "job-dismiss"];
        expected.extend(["blockdev-del"; 8]);
        expected.extend(["block-dirty-bitmap-remove"; 2]);
        assert_eq!(names_of(&sent), expected);
        assert_eq!(sent[0].1, serde_json::json!({ "id": "opl-repl-0" }));
        assert_eq!(
            sent[11].1,
            serde_json::json!({ "node": "disk0", "name": "opl-repl-new" })
        );
        assert_eq!(
            sent[12].1,
            serde_json::json!({ "node": "disk1", "name": "opl-repl-new" })
        );
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        assert_eq!(*fake.roots.lock().unwrap(), ["disk0", "disk1"]);
    }

    /// 보낸 양이 `stall` 동안 늘지 않으면 작업을 취소하고 실패한다.
    #[tokio::test]
    async fn stalled_replication_is_cancelled() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-repl-0", "running", 0, None),
                repl_job("opl-repl-1", "running", 0, None)
            ]),
            serde_json::json!([
                repl_job("opl-repl-0", "concluded", 0, Some("Operation canceled")),
                repl_job("opl-repl-1", "concluded", 0, Some("Operation canceled"))
            ]),
        ]);
        let err = hv
            .replicate_disks(id, &replicate_request(Duration::ZERO))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::Replication(m) if m.starts_with("replication made no progress")),
            "{err:?}"
        );
        assert_eq!(sent(&fake, &["job-cancel"]).len(), 2);
        assert_eq!(sent(&fake, &["block-dirty-bitmap-remove"]).len(), 2);
    }

    /// 노드를 닫지 못하면(받는 쪽에 내보내지 못함, 플리싱을 떼지 못함) 작업이 성공했어도 실패다. 새 비트맵을 지운다.
    #[tokio::test]
    async fn replication_fails_when_a_target_cannot_be_closed() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-repl-0", "concluded", 5, None),
                repl_job("opl-repl-1", "concluded", 5, None)
            ]),
        ]);
        fake.failing.lock().unwrap().push("blockdev-del");
        let err = hv
            .replicate_disks(id, &replicate_request(Duration::from_secs(600)))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DriverError::Qmp(QmpError::Command { desc, .. }) if desc == "blockdev-del failed"),
            "{err:?}"
        );
        // 플리싱은 디스크마다 snapshot-access 노드에서 멈춘다(남은 것은 다음 회차가 뗀다).
        let sent = sent(&fake, &["blockdev-del", "block-dirty-bitmap-remove"]);
        assert_eq!(
            names_of(&sent),
            [
                "blockdev-del",
                "blockdev-del",
                "blockdev-del",
                "blockdev-del",
                "block-dirty-bitmap-remove",
                "block-dirty-bitmap-remove",
            ]
        );
    }

    /// 앞선 회차가 남긴 작업(opld가 기다리다 죽음)과 NBD 노드는 먼저 치운다. 백업 비트맵을 지우는 남은 것
    /// 치우기(`clear_backup_leftovers`)도 복제 작업과 따라잡기 비트맵을 치운다.
    #[tokio::test]
    async fn replication_clears_what_an_earlier_run_left() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.nbd_nodes
            .lock()
            .unwrap()
            .push("opl-repl-nbd-0".to_string());
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([repl_job("opl-repl-0", "running", 0, None)]),
            serde_json::json!([repl_job(
                "opl-repl-0",
                "concluded",
                0,
                Some("Operation canceled")
            )]),
            serde_json::json!([
                repl_job("opl-repl-0", "concluded", 7, None),
                repl_job("opl-repl-1", "concluded", 7, None)
            ]),
        ]);
        assert_eq!(
            hv.replicate_disks(id, &replicate_request(Duration::from_secs(600)))
                .await
                .unwrap()
                .0,
            14
        );
        let sent_now = sent(
            &fake,
            &["job-cancel", "job-dismiss", "blockdev-del", "object-add"],
        );
        assert_eq!(
            names_of(&sent_now)[..4],
            ["job-cancel", "job-dismiss", "blockdev-del", "object-add"]
        );
        assert_eq!(
            sent_now[2].1,
            serde_json::json!({ "node-name": "opl-repl-nbd-0" })
        );

        let mig = opl_types::replication::migration_bitmap(Uuid::now_v7(), 1);
        let bitmap = serde_json::json!({ "name": mig, "recording": true, "persistent": true });
        fake.extra_bitmaps.lock().unwrap().push(("disk0", bitmap));
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([repl_job("opl-repl-1", "running", 0, None)]),
            serde_json::json!([repl_job(
                "opl-repl-1",
                "concluded",
                0,
                Some("Operation canceled")
            )]),
        ]);
        let before = sent(&fake, &["block-dirty-bitmap-remove"]).len();
        hv.clear_backup_leftovers(id).await.unwrap();
        let removed = sent(&fake, &["block-dirty-bitmap-remove"]);
        assert_eq!(
            removed[before..]
                .iter()
                .map(|(_, a)| a.clone())
                .collect::<Vec<_>>(),
            [serde_json::json!({ "node": "disk0", "name": mig })]
        );
        assert_eq!(
            sent(&fake, &["job-cancel"]).last().unwrap().1,
            serde_json::json!({ "id": "opl-repl-1" })
        );
    }

    /// 앞선 회차가 남긴 플리싱(opld가 기다리다 죽음)은 먼저 뗀다: 장치를 디스크 노드로 되돌리고 snapshot-access →
    /// 필터 → 이미지 노드를 닫고 남은 플리싱 이미지 파일을 지운다. VM을 켤 때도 남은 파일을 지운다.
    #[tokio::test]
    async fn replication_removes_a_fleecing_an_earlier_run_left() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.roots.lock().unwrap()[0] = "opl-fl-cbw-0".into();
        fake.hot_nodes
            .lock()
            .unwrap()
            .extend(["opl-fl-tmp-0", "opl-fl-cbw-0", "opl-fl-acc-0"].map(String::from));
        let fleece = tmp.path().join("state").join("vm").join(id.to_string());
        std::fs::create_dir_all(&fleece).unwrap();
        std::fs::write(fleece.join("fleece-0.img"), b"left").unwrap();
        std::fs::write(fleece.join("fleece-3.img"), b"left").unwrap();
        std::fs::write(fleece.join("uefi-vars.fd"), b"vars").unwrap();
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-repl-0", "concluded", 7, None),
                repl_job("opl-repl-1", "concluded", 7, None)
            ]),
        ]);
        hv.replicate_disks(id, &replicate_request(Duration::from_secs(600)))
            .await
            .unwrap();
        let sent = sent(&fake, &["qom-set", "blockdev-del", "object-add"]);
        assert_eq!(
            sent[..4].iter().map(|(_, a)| a.clone()).collect::<Vec<_>>(),
            [
                serde_json::json!({
                    "path": "/machine/peripheral/disk0-dev/virtio-backend", "property": "drive",
                    "value": "disk0"
                }),
                serde_json::json!({ "node-name": "opl-fl-acc-0" }),
                serde_json::json!({ "node-name": "opl-fl-cbw-0" }),
                serde_json::json!({ "node-name": "opl-fl-tmp-0" }),
            ]
        );
        // 그다음에 이번 회차를 시작한다.
        assert_eq!(sent[4].0, "object-add");
        let mut left: Vec<String> = std::fs::read_dir(&fleece)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["uefi-vars.fd"]);
        assert_eq!(*fake.roots.lock().unwrap(), ["disk0", "disk1"]);
    }

    /// opld가 회차 도중에 죽어 남은 플리싱은 다시 붙을 때 뗀다: 장치를 디스크 노드로 되돌리고 `opl-fl-*` 노드를 닫고
    /// 남은 플리싱 이미지 파일을 지운다(다른 파일은 둔다).
    #[tokio::test]
    async fn reattaching_removes_a_fleecing_left_by_a_dead_opld() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        fake.roots.lock().unwrap()[1] = "opl-fl-cbw-1".into();
        fake.hot_nodes
            .lock()
            .unwrap()
            .extend(["opl-fl-tmp-1", "opl-fl-cbw-1", "opl-fl-acc-1"].map(String::from));
        let fleece = tmp.path().join("state").join("vm").join(vm.id.to_string());
        std::fs::create_dir_all(&fleece).unwrap();
        std::fs::write(fleece.join("fleece-1.img"), b"left").unwrap();
        std::fs::write(fleece.join("uefi-vars.fd"), b"vars").unwrap();

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.inspect(vm.id).await.unwrap().is_some());
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        assert_eq!(*fake.roots.lock().unwrap(), ["disk0", "disk1"]);
        let left: Vec<String> = std::fs::read_dir(&fleece)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["uefi-vars.fd"]);
    }

    /// 메모리 스냅샷 저장 중의 QEMU는 작업이 끝날 때까지 QMP에 답하지 않는다(BQL). 그동안 상태 조회는
    /// QMP를 묻지 않고 곧바로 `Busy`를 알린다(묻는다면 `QMP_TIMEOUT` 뒤 응답 없음이다). 작업을 그만두면
    /// (제한 시간, 취소) 표시가 사라진다.
    #[tokio::test]
    async fn memory_snapshot_reports_busy_without_asking_qmp() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let hv = Arc::new(hv);
        *fake.delay.lock().unwrap() = Some(("snapshot-save", Duration::from_secs(30)));
        let save = tokio::spawn({
            let hv = hv.clone();
            async move { hv.snapshot_save(id, "s", true, false).await }
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while hv.busy_with(id).is_none() {
            assert!(tokio::time::Instant::now() < deadline, "save did not start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let busy = RuntimeStatus::Busy("snapshot-save");
        let asked = tokio::time::Instant::now();
        assert_eq!(hv.inspect(id).await.unwrap().unwrap().status, busy);
        assert_eq!(hv.list().await.unwrap()[0].status, busy);
        assert!(asked.elapsed() < Duration::from_secs(1));

        // 표시 파일도 남는다(리뷰 1 #4 — orbivisord가 다시 켜져도 안다).
        let mark = hv.paths(id).unwrap().dir.join(BUSY_MARK);
        assert_eq!(std::fs::read_to_string(&mark).unwrap(), "snapshot-save");

        save.abort();
        let _ = save.await;
        assert_eq!(hv.busy_with(id), None);
        assert!(!mark.exists());
    }

    /// 리뷰 1 #4: 메모리 스냅샷 작업 중 orbivisord가 다시 켜져 다시 붙지 못한 VM은 표시 파일이 있고 QMP가 답하지 않으면
    /// 바쁨이다(응답 없음이면 멈추라는 요청이 `acpi_timeout` 뒤 저장 중인 VM을 강제로 끈다). 오래된 표시는 보지 않는다.
    #[tokio::test]
    async fn a_restarted_driver_reports_a_marked_vm_busy() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let qemu = unresponsive_vm(&dir).await;
        std::fs::write(dir.join(BUSY_MARK), "snapshot-save").unwrap();
        let busy = ActualState {
            id: vm.id,
            pid: qemu.id().unwrap(),
            status: RuntimeStatus::Busy("snapshot-save"),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(busy.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![busy]);
        // SNAPSHOT_TIMEOUT보다 오래된 표시는 남은 것이다: 예전처럼 응답 없음
        std::fs::File::options()
            .write(true)
            .open(dir.join(BUSY_MARK))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - SNAPSHOT_TIMEOUT - Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            hv.inspect(vm.id).await.unwrap().unwrap().status,
            RuntimeStatus::Other("unresponsive".into())
        );
    }

    /// 리뷰 1 M-7·#4: 붙은 VM의 `query-status`가 시간을 넘겼을 때 그 사이 작업이 시작됐거나 표시 파일이 있으면 바쁨이다.
    /// QMP가 다시 답하면 메모리 표시가 없는 표시 파일은 지운다.
    #[tokio::test]
    async fn a_query_timeout_with_a_busy_mark_is_busy() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let mark = hv.paths(id).unwrap().dir.join(BUSY_MARK);
        std::fs::write(&mark, "snapshot-delete").unwrap();
        *fake.delay.lock().unwrap() = Some(("query-status", QMP_TIMEOUT + Duration::from_secs(2)));
        assert_eq!(
            hv.inspect(id).await.unwrap().unwrap().status,
            RuntimeStatus::Busy("snapshot-delete")
        );
        *fake.delay.lock().unwrap() = None;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            hv.inspect(id).await.unwrap().unwrap().status,
            RuntimeStatus::Running
        );
        assert!(!mark.exists());
    }

    /// 플리싱 필터를 끼우려고 게스트를 멈춘 동안은 지금 멈춰 둔 VM으로 둔다(그 사이 다시 붙기·`resume_left_stopped`가
    /// 이어서 실행하지 않는다). 이어서 실행한 뒤에는 뺀다.
    #[tokio::test]
    async fn fleecing_holds_the_vm_as_paused_while_inserting_the_filters() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let hv = Arc::new(hv);
        *fake.delay.lock().unwrap() = Some(("qom-set", Duration::from_millis(300)));
        fake.jobs.lock().unwrap().extend([
            serde_json::json!([]),
            serde_json::json!([
                repl_job("opl-repl-0", "concluded", 7, None),
                repl_job("opl-repl-1", "concluded", 7, None)
            ]),
        ]);
        let run = tokio::spawn({
            let hv = hv.clone();
            async move {
                hv.replicate_disks(id, &replicate_request(Duration::from_secs(600)))
                    .await
            }
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !fake.commands.lock().unwrap().iter().any(|c| c == "stop") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the VM was not stopped"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(hv.pausing.lock().unwrap().contains(&id));
        run.await.unwrap().unwrap();
        assert!(!hv.pausing.lock().unwrap().contains(&id));
        assert!(fake.commands.lock().unwrap().iter().any(|c| c == "cont"));
    }

    /// 플리싱 이미지 디렉터리의 남은 공간이 디스크 크기 합의 10%보다 적으면 회차를 시작하지 않는다.
    #[test]
    fn fleecing_needs_a_tenth_of_the_disks_free() {
        let tmp = short_tempdir();
        check_fleece_space(tmp.path(), 0).unwrap();
        check_fleece_space(tmp.path(), 1 << 20).unwrap();
        let err = check_fleece_space(tmp.path(), u64::MAX / 2).unwrap_err();
        assert!(
            err.starts_with(&format!(
                "not enough space in {} for the fleecing images: need {}, available ",
                tmp.path().display(),
                u64::MAX / 2 / 100 * 10
            )),
            "{err}"
        );
    }

    /// 받는 QEMU의 인자와 복제본 내보내기: TLS(server) → 디스크마다 블록 노드(겹층이면 복제본을 backing으로) →
    /// NBD 서버 → 내보내기(이름 = 볼륨 id simple). 끝내기는 내보내기 닫기 → 서버 멈춤 → 노드 닫기 → quit.
    #[tokio::test]
    async fn replica_receiver_exports_replicas_and_overlays() {
        let tmp = short_tempdir();
        let fake = fake_qmp(tmp.path(), "prelaunch").await;
        let qmp = QmpClient::connect(&tmp.path().join("qmp.sock"))
            .await
            .unwrap();
        let exports = [
            ReceiverExport {
                name: "a1".into(),
                source: DiskLocation::Path("/pools/b/a.qcow2".into(), DiskKind::File),
                format: ImageFormat::Qcow2,
                overlay: None,
            },
            ReceiverExport {
                name: "b2".into(),
                source: DiskLocation::Path("/dev/vg/opl-b".into(), DiskKind::Block),
                format: ImageFormat::Raw,
                overlay: Some("/var/lib/opl/replicas/overlays/r-b.qcow2".into()),
            },
        ];
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let tls = Path::new("/state/migration-tls");
        assert_eq!(
            serve_replicas(&qmp, tls, &exports, "127.0.0.1", port..=port)
                .await
                .unwrap(),
            port
        );
        let names = [
            "object-add",
            "blockdev-add",
            "nbd-server-start",
            "block-export-add",
        ];
        let got = sent(&fake, &names);
        assert_eq!(
            names_of(&got),
            [
                "object-add",
                "blockdev-add",
                "blockdev-add",
                "nbd-server-start",
                "block-export-add",
                "block-export-add",
            ]
        );
        assert_eq!(got[0].1["id"], "opl-repl-tls");
        assert_eq!(got[0].1["endpoint"], "server");
        assert_eq!(got[0].1["verify-peer"], true);
        assert_eq!(
            got[1].1,
            serde_json::json!({
                "driver": "qcow2", "node-name": "opl-repl-0",
                "file": { "driver": "file", "filename": "/pools/b/a.qcow2" }
            })
        );
        assert_eq!(
            got[2].1,
            serde_json::json!({
                "driver": "qcow2", "node-name": "opl-repl-1",
                "file": { "driver": "file",
                          "filename": "/var/lib/opl/replicas/overlays/r-b.qcow2" },
                "backing": { "driver": "raw",
                             "file": { "driver": "host_device", "filename": "/dev/vg/opl-b" } }
            })
        );
        assert_eq!(got[3].1["tls-creds"], "opl-repl-tls");
        assert_eq!(
            got[5].1,
            serde_json::json!({
                "type": "nbd", "id": "opl-repl-export-1", "node-name": "opl-repl-1",
                "name": "b2", "writable": true
            })
        );
        // 겹층은 절대 경로여야 하고, RBD 풀에는 복제본을 두지 않는다.
        let relative = ReceiverExport {
            overlay: Some("overlay.qcow2".into()),
            ..exports[1].clone()
        };
        assert!(matches!(
            receiver_blockdev("n", &relative),
            Err(DriverError::InvalidSpec(_))
        ));

        close_replicas(&qmp).await.unwrap();
        let names = [
            "block-export-del",
            "nbd-server-stop",
            "blockdev-del",
            "quit",
        ];
        let got = sent(&fake, &names);
        assert_eq!(
            names_of(&got),
            [
                "block-export-del",
                "block-export-del",
                "nbd-server-stop",
                "blockdev-del",
                "blockdev-del",
                "quit",
            ]
        );
        assert_eq!(
            got[0].1,
            serde_json::json!({ "id": "opl-repl-export-0", "mode": "hard" })
        );
        assert_eq!(got[4].1, serde_json::json!({ "node-name": "opl-repl-1" }));
    }

    /// 받는 QEMU가 없는 회차는 끝낼 수 없고, 끄기는 성공한다. 끝난 받는 QEMU의 실행 디렉터리는 다시 켤 때 지운다.
    #[tokio::test]
    async fn replica_receivers_are_found_by_their_pid_files() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let (live, dead) = (Uuid::now_v7(), Uuid::now_v7());
        let live_dir = cfg.run_dir.join(format!("replica-{live}"));
        let _qemu = hold_pidfile(&live_dir).await;
        let dead_dir = cfg.run_dir.join(format!("replica-{dead}"));
        std::fs::create_dir_all(&dead_dir).unwrap();
        std::fs::write(dead_dir.join("qemu.pid"), b"4242\n").unwrap();
        std::fs::create_dir_all(cfg.run_dir.join("replica-not-a-uuid")).unwrap();
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!dead_dir.exists());
        assert_eq!(hv.replica_receivers().await.unwrap(), [live]);
        assert!(matches!(
            hv.finish_replica_receiver(dead).await,
            Err(DriverError::Replication(_))
        ));
        // 같은 회차를 다시 받을 수 없다.
        let export = ReceiverExport {
            name: "a".into(),
            source: DiskLocation::Path("/p/a.raw".into(), DiskKind::File),
            format: ImageFormat::Raw,
            overlay: None,
        };
        assert!(matches!(
            hv.start_replica_receiver(live, &[export], "127.0.0.1", 60000..=60000)
                .await,
            Err(DriverError::Replication(_))
        ));
        hv.stop_replica_receiver(dead).await.unwrap();
        // 살아 있는 받는 QEMU는 SIGKILL로 끄고 디렉터리를 지운다.
        hv.stop_replica_receiver(live).await.unwrap();
        assert!(!live_dir.exists());
        assert!(hv.replica_receivers().await.unwrap().is_empty());
    }

    /// 실행 중인데 지금 멈춰 두지 않은 VM의 표시는 남은 것이다(이어서 실행했지만 지우지 못함, 누가
    /// `cont`함). 상태를 보면 지운다. 그대로 두면 나중에 다른 까닭으로 멈춘 VM을 이어서 실행한다.
    #[tokio::test]
    async fn running_vm_drops_a_left_over_paused_mark() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let _fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mark = dir.join(PAUSED_MARK);

        // 멈춰 두는 동안에는 지우지 않는다(가짜 QMP는 늘 running이라고 답한다).
        hv.pause(vm.id).await.unwrap();
        hv.inspect(vm.id).await.unwrap().unwrap();
        assert!(mark.exists());
        hv.pausing.lock().unwrap().clear();
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);
        assert!(!mark.exists());
    }

    #[tokio::test]
    async fn pause_and_resume_send_stop_and_cont() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let mark = dir.join(PAUSED_MARK);
        hv.pause(vm.id).await.unwrap();
        assert!(mark.exists());
        // 멈춰 둔 동안 reconciler가 다시 붙어도(start) 이어서 실행하지 않는다.
        hv.start(&vm, None).await.unwrap();
        assert!(mark.exists());
        hv.resume(vm.id).await.unwrap();
        assert!(!mark.exists());
        let commands = fake.commands.lock().unwrap().clone();
        let sent: Vec<&str> = commands
            .iter()
            .map(String::as_str)
            .filter(|c| ["stop", "cont"].contains(c))
            .collect();
        assert_eq!(sent, ["stop", "cont"]);

        let err = hv.pause(Uuid::now_v7()).await.unwrap_err();
        assert!(matches!(err, DriverError::NotRunning(_)), "{err:?}");
    }

    #[tokio::test]
    async fn snapshot_ops_on_vm_that_is_not_attached() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let err = hv
            .snapshot_save(Uuid::now_v7(), "s", true, false)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::NotRunning(_)), "{err:?}");

        // 살아 있지만 QMP로 다룰 수 없는 VM을 꺼진 것으로 알리면 호출자가 쓰고 있는 디스크를
        // qemu-img로 건드린다. 다시 시도할 수 있는 오류로 끝낸다.
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = unresponsive_vm(&dir).await;
        for err in [
            hv.snapshot_save(vm.id, "s", true, false).await.unwrap_err(),
            hv.snapshot_delete(vm.id, "s").await.unwrap_err(),
            hv.block_resize(vm.id, 0, 1 << 30).await.unwrap_err(),
            hv.pause(vm.id).await.unwrap_err(),
            hv.resume(vm.id).await.unwrap_err(),
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
    async fn kill_sends_sigkill_without_qmp() {
        for registered in [true, false] {
            let tmp = short_tempdir();
            let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
            let vm = spec();
            let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
            let mut qemu = hold_pidfile(&dir).await;
            // 등록된 VM(QMP 연결 있음)이어도 `quit`을 보내지 않고 곧바로 끝낸다.
            let commands = if registered {
                let fake = fake_qmp(&dir, "running").await;
                let (pidfile, hangup) = (dir.join("qemu.pid"), fake.hangup);
                // 실제 QEMU처럼 프로세스가 끝나면 QMP 연결이 끊긴다.
                tokio::spawn(async move {
                    while liveness(&pidfile) != Liveness::Dead {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    let _ = hangup.send(None);
                });
                Some(fake.commands)
            } else {
                drop(tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap());
                None
            };
            let hv = QemuHypervisor::new(cfg).await.unwrap();
            assert_eq!(hv.handle(vm.id).is_some(), registered);

            let started = std::time::Instant::now();
            hv.stop(vm.id, StopMode::Kill).await.unwrap();
            assert!(
                started.elapsed() < FORCE_STOP_TIMEOUT,
                "registered: {registered}"
            );
            let status = tokio::time::timeout(Duration::from_secs(2), qemu.wait())
                .await
                .expect("QEMU was not killed")
                .unwrap();
            assert!(!status.success());
            if let Some(commands) = commands {
                assert!(!commands.lock().unwrap().iter().any(|c| c == "quit"));
            }
            assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        }
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

    /// 게스트 에이전트 채널을 단 VM(`qga.sock`이 있다)만 게스트의 재부팅(QMP `RESET`)을 알린다.
    #[tokio::test]
    async fn guest_resets_are_reported_only_for_vms_with_a_guest_agent() {
        for agent in [true, false] {
            let tmp = short_tempdir();
            let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
            let vm = spec();
            let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
            let _qemu = hold_pidfile(&dir).await;
            let fake = fake_qmp(&dir, "running").await;
            if agent {
                std::fs::write(dir.join("qga.sock"), "").unwrap();
            }
            let hv = QemuHypervisor::new(cfg).await.unwrap();
            let mut rx = hv.subscribe();

            let reset = serde_json::json!({
                "event": "RESET",
                "data": { "guest": true, "reason": "guest-reset" },
                "timestamp": { "seconds": 0, "microseconds": 0 }
            });
            fake.hangup.send(Some(reset)).unwrap();
            // 연결이 끊긴 뒤에도 QEMU는 살아 있으므로 등록만 풀고 종료로 알리지 않는다.
            tokio::time::timeout(Duration::from_secs(10), async {
                while hv.handle(vm.id).is_some() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("VM was not unregistered");
            let got = rx.try_recv().ok();
            let want = agent.then_some(HypervisorEvent {
                id: vm.id,
                kind: EventKind::Reset,
            });
            assert_eq!(got, want, "agent: {agent}");
            assert!(rx.try_recv().is_err());
        }
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
                mtu: None,
                ovn_port: None,
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
                mtu: None,
                ovn_port: None,
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

    /// tap 만들기·지우기를 기록하는 가짜 호스트 네트워크
    #[derive(Debug, Default)]
    struct RecordingNet {
        calls: Mutex<Vec<String>>,
        /// 참이면 tap 하나 지우기가 실패한다.
        fail_delete: Mutex<bool>,
        /// 참이면 OVN 포트가 켜지지 않는다(`wait_ovn_installed`가 시간을 넘긴다).
        ovn_down: Mutex<bool>,
    }

    #[async_trait]
    impl HostNet for RecordingNet {
        async fn bridges(&self) -> Result<Vec<opl_types::node::BridgeInfo>, DriverError> {
            Ok(vec![])
        }
        async fn create_tap(&self, tap: &Tap) -> Result<(), DriverError> {
            let mut call = format!("create {} {} {:?}", tap.ifname, tap.bridge, tap.vlan);
            if let Some(ovn) = &tap.ovn {
                call.push_str(&format!(" ovn={} {} {:?}", ovn.iface_id, ovn.mac, ovn.mtu));
            }
            self.calls.lock().unwrap().push(call);
            Ok(())
        }
        async fn wait_ovn_installed(
            &self,
            ifname: &str,
            timeout: Duration,
        ) -> Result<(), DriverError> {
            let call = format!("wait {ifname} {}s", timeout.as_secs());
            self.calls.lock().unwrap().push(call);
            if *self.ovn_down.lock().unwrap() {
                return Err(DriverError::Timeout("ovn-installed"));
            }
            Ok(())
        }
        async fn delete_vm_taps(&self, vm: Uuid) -> Result<(), DriverError> {
            self.calls.lock().unwrap().push(format!("delete-vm {vm}"));
            Ok(())
        }
        async fn delete_tap(&self, ifname: &str) -> Result<(), DriverError> {
            self.calls.lock().unwrap().push(format!("delete {ifname}"));
            if *self.fail_delete.lock().unwrap() {
                return Err(DriverError::ToolFailed {
                    tool: "ip",
                    stderr: "Cannot find device".into(),
                });
            }
            Ok(())
        }
        async fn sync_overlays(
            &self,
            _: &str,
            _: &[crate::net::Overlay],
        ) -> Result<crate::net::OverlayChanges, DriverError> {
            unimplemented!()
        }
    }

    /// [`running_vm`]에 tap을 기록하는 호스트 네트워크를 단 것
    async fn running_vm_with_net(
        tmp: &Path,
        net: Arc<RecordingNet>,
    ) -> (QemuHypervisor, Uuid, FakeQmp, tokio::process::Child) {
        let mut cfg = config(tmp, PathBuf::from("/nonexistent/qemu"));
        cfg.net = Some(net);
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        (hv, vm.id, fake, qemu)
    }

    /// 소켓 2개, 켤 때 vCPU 2개(소켓마다 코어 1개), 상한 8(소켓마다 코어 자리 4개)
    fn hot_cpu() -> CpuSpec {
        CpuSpec {
            emulator: None,
            policy: Default::default(),
            thread_policy: None,
            flags: Vec::new(),
            sockets: 2,
            cores: 1,
            model: CpuModel::Max,
            max_vcpus: Some(8),
        }
    }

    const UNPLUG: Duration = Duration::from_millis(200);

    /// vCPU `i`는 소켓마다 코어 `max / sockets`개 자리를 앞에서부터 채운 자리에 붙고, 줄이기는 큰 번호부터 뗀다.
    /// 이미 켜진 자리(앞서 붙임)는 건너뛴다.
    #[tokio::test]
    async fn set_vcpus_adds_and_removes_cpus_at_their_slots() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;

        assert_eq!(
            hv.set_vcpus(id, &hot_cpu(), 2, 5, UNPLUG)
                .await
                .unwrap()
                .reached,
            5
        );
        let added = sent(&fake, &["query-hotpluggable-cpus", "device_add"]);
        let slot = |i: u32, socket: u32, core: u32| {
            serde_json::json!({ "driver": "max-x86_64-cpu", "id": format!("cpu{i}"),
                "socket-id": socket, "core-id": core, "thread-id": 0 })
        };
        assert_eq!(added[0].0, "query-hotpluggable-cpus");
        let args: Vec<_> = added[1..].iter().map(|(_, a)| a.clone()).collect();
        assert_eq!(args, [slot(2, 0, 2), slot(3, 0, 3), slot(4, 1, 0)]);

        assert_eq!(
            hv.set_vcpus(id, &hot_cpu(), 5, 2, UNPLUG)
                .await
                .unwrap()
                .reached,
            2
        );
        let removed: Vec<_> = sent(&fake, &["device_del"])
            .into_iter()
            .map(|(_, a)| a["id"].clone())
            .collect();
        assert_eq!(removed, ["cpu4", "cpu3", "cpu2"]);

        // 가짜 QEMU는 뗀 자리를 그대로 켜진 것으로 답한다: 다시 붙이면 건너뛴다.
        assert_eq!(
            hv.set_vcpus(id, &hot_cpu(), 2, 3, UNPLUG)
                .await
                .unwrap()
                .reached,
            3
        );
        assert_eq!(sent(&fake, &["device_add"]).len(), 3);

        // 켤 때 수 아래·상한 위·상한 없이 켠 VM은 보내지 않고 거절한다.
        for (cpu, to) in [
            (hot_cpu(), 1),
            (hot_cpu(), 9),
            (
                CpuSpec {
                    max_vcpus: None,
                    ..hot_cpu()
                },
                2,
            ),
        ] {
            let err = hv.set_vcpus(id, &cpu, 2, to, UNPLUG).await.unwrap_err();
            assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        }
        assert!(matches!(
            hv.set_vcpus(Uuid::now_v7(), &hot_cpu(), 2, 3, UNPLUG).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    /// 게스트가 vCPU를 내려 주지 않으면 거기서 멈추고 닿은 수(그대로)를 돌려준다. 붙이다 몇 개 뒤에 실패하면 닿은
    /// 수, 처음부터 실패하면 오류다.
    #[tokio::test]
    async fn vcpu_changes_report_how_far_they_got() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        *fake.hold_unplug.lock().unwrap() = true;
        assert_eq!(
            hv.set_vcpus(id, &hot_cpu(), 4, 2, UNPLUG)
                .await
                .unwrap()
                .reached,
            4
        );
        let removed = sent(&fake, &["device_del"]);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].1["id"], "cpu3");

        fake.failing.lock().unwrap().push("device_add");
        let err = hv
            .set_vcpus(id, &hot_cpu(), 2, 4, UNPLUG)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Qmp(QmpError::Command { .. })),
            "{err:?}"
        );
        fake.failing.lock().unwrap().clear();
        assert_eq!(
            hv.set_vcpus(id, &hot_cpu(), 2, 3, UNPLUG)
                .await
                .unwrap()
                .reached,
            3
        );
        fake.failing.lock().unwrap().push("device_add");
        // cpu2는 이제 켜져 있어 건너뛰고 cpu3에서 실패: 닿은 수 3
        assert_eq!(
            hv.set_vcpus(id, &hot_cpu(), 2, 5, UNPLUG)
                .await
                .unwrap()
                .reached,
            3
        );
    }

    /// QEMU 8.2 q35는 게스트가 ACPI CPU 핫플러그를 켜기 전에 vCPU 떼기를 거절한다: 오류가 아니라 지금 수와
    /// `refused`다(진행 중인 떼기 없음). 다른 거절은 오류다.
    #[tokio::test]
    async fn a_refused_vcpu_unplug_keeps_the_count() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        *fake.refuse_cpu_unplug.lock().unwrap() = true;
        let change = hv.set_vcpus(id, &hot_cpu(), 4, 2, UNPLUG).await.unwrap();
        assert_eq!(
            change,
            VcpuChange {
                reached: 4,
                refused: true
            }
        );
        assert_eq!(sent(&fake, &["device_del"]).len(), 1);

        *fake.refuse_cpu_unplug.lock().unwrap() = false;
        fake.failing.lock().unwrap().push("device_del");
        let err = hv
            .set_vcpus(id, &hot_cpu(), 4, 2, UNPLUG)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Qmp(QmpError::Command { .. })),
            "{err:?}"
        );
    }

    /// 같은 수로 맞추기도 켤 때 수부터 빈 자리를 본다: 떼기를 요청한 vCPU를 게스트가 늦게 내려 줬으면 다시 붙인다.
    #[tokio::test]
    async fn vcpus_released_late_are_added_again() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        // 기록은 3개인데 가짜 QEMU에는 켤 때의 두 자리만 켜져 있다(cpu2를 게스트가 늦게 내려 줌).
        assert_eq!(
            hv.set_vcpus(id, &hot_cpu(), 3, 3, UNPLUG)
                .await
                .unwrap()
                .reached,
            3
        );
        let added: Vec<_> = sent(&fake, &["device_add"])
            .into_iter()
            .map(|(_, a)| a["id"].clone())
            .collect();
        assert_eq!(added, ["cpu2"]);
    }

    /// virtio-mem 늘리기는 요청만 바꾸고, 줄이기는 게스트가 내려 주기를 기다린다. 시간이 넘으면 요청을 그때 크기로
    /// 되돌리고 그 크기다.
    #[tokio::test]
    async fn set_memory_moves_the_requested_size() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let mib = |n: u64| serde_json::json!(n << 20);

        assert_eq!(hv.set_memory(id, 4096, UNPLUG).await.unwrap(), 4096);
        assert_eq!(hv.set_memory(id, 2048, UNPLUG).await.unwrap(), 2048);
        fake.vmem.lock().unwrap().1 = Some(2048 << 20);
        assert_eq!(hv.set_memory(id, 0, UNPLUG).await.unwrap(), 2048);

        let sets: Vec<_> = sent(&fake, &["qom-set"])
            .into_iter()
            .map(|(_, a)| a)
            .collect();
        for set in &sets {
            assert_eq!(set["path"], VIRTIO_MEM_PATH);
            assert_eq!(set["property"], "requested-size");
        }
        let values: Vec<_> = sets.iter().map(|a| a["value"].clone()).collect();
        assert_eq!(values, [mib(4096), mib(2048), mib(0), mib(2048)]);
        // 줄이기는 `size`를 읽고, 시간이 넘을 때까지 다시 읽는다.
        let gets = sent(&fake, &["qom-get"]);
        assert!(
            gets.iter().all(|(_, a)| a["property"] == "size"),
            "{gets:?}"
        );
        assert!(gets.len() >= 5, "{gets:?}");
    }

    fn hot_disk(kind: DiskKind) -> RuntimeDisk {
        RuntimeDisk {
            path: "/pools/a/hot.qcow2".into(),
            kind,
            rbd: None,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(2),
        }
    }

    /// 하드웨어 기록은 실행 디렉터리의 `hardware.json`에 남고 읽힌다. 도는 VM이 아니면 쓰지 않는다.
    #[tokio::test]
    async fn the_hardware_record_is_saved_next_to_the_qmp_socket() {
        let tmp = short_tempdir();
        let (hv, id, _fake, _qemu) = running_vm(tmp.path()).await;
        assert_eq!(hv.saved_hardware(id), None);
        let spec: opl_types::vm::VmSpec = serde_json::from_value(
            serde_json::json!({ "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt" }),
        )
        .unwrap();
        let (hw, _) = RunningHardware::at_start(id, "a", &spec, NodeHotplug::default(), true);
        hv.save_hardware(id, &hw).unwrap();
        assert!(hv.paths(id).unwrap().dir.join(HARDWARE_FILE).is_file());
        assert_eq!(hv.saved_hardware(id), Some(hw.clone()));
        assert!(matches!(
            hv.save_hardware(Uuid::now_v7(), &hw),
            Err(DriverError::NotRunning(_))
        ));
    }

    /// 디스크는 파일 노드·형식 노드를 열고 루트 포트에 붙인다. 떼기는 장치를 뗀 뒤 두 노드를 닫고, 다시 떼면 닫을
    /// 노드가 없어 장치 떼기만 보낸다.
    #[tokio::test]
    async fn add_and_remove_disk_use_the_disk_nodes_and_the_port() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;

        hv.add_disk(id, 3, &hot_disk(DiskKind::File), 2)
            .await
            .unwrap();
        let want = [
            (
                "blockdev-add".to_string(),
                serde_json::json!({
                "driver": "file", "filename": "/pools/a/hot.qcow2", "node-name": "disk3-file" }),
            ),
            (
                "blockdev-add".to_string(),
                serde_json::json!({
                "driver": "qcow2", "file": "disk3-file", "node-name": "disk3" }),
            ),
            (
                "device_add".to_string(),
                serde_json::json!({
                "driver": "virtio-blk-pci", "drive": "disk3", "id": "disk3-dev", "bus": "hp2",
                "bootindex": 2 }),
            ),
        ];
        assert_eq!(sent(&fake, &["blockdev-add", "device_add"]), want);
        assert_eq!(*fake.hot_nodes.lock().unwrap(), ["disk3-file", "disk3"]);

        assert!(hv.remove_disk(id, 3, UNPLUG).await.unwrap());
        let removed = sent(&fake, &["device_del", "blockdev-del"]);
        let removed: Vec<_> = removed
            .iter()
            .map(|(c, a)| (c.as_str(), a.clone()))
            .collect();
        assert_eq!(
            removed,
            [
                ("device_del", serde_json::json!({ "id": "disk3-dev" })),
                ("blockdev-del", serde_json::json!({ "node-name": "disk3" })),
                (
                    "blockdev-del",
                    serde_json::json!({ "node-name": "disk3-file" })
                ),
            ]
        );
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        assert!(hv.remove_disk(id, 3, UNPLUG).await.unwrap());
        assert_eq!(sent(&fake, &["device_del"]).len(), 2);
        assert_eq!(sent(&fake, &["blockdev-del"]).len(), 2);
    }

    /// 장치를 붙이지 못하면 연 노드를 거꾸로 닫는다. 블록 장치·RBD 소스는 cmdline과 같은 드라이버 인자다. SATA는
    /// 보내지 않고 거절한다.
    #[tokio::test]
    async fn disk_sources_and_a_failed_disk_add() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.failing.lock().unwrap().push("device_add");
        let err = hv
            .add_disk(id, 3, &hot_disk(DiskKind::File), 0)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Qmp(QmpError::Command { .. })),
            "{err:?}"
        );
        let closed: Vec<_> = sent(&fake, &["blockdev-del"])
            .into_iter()
            .map(|(_, a)| a["node-name"].clone())
            .collect();
        assert_eq!(closed, ["disk3", "disk3-file"]);
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        fake.failing.lock().unwrap().clear();

        hv.add_disk(id, 4, &hot_disk(DiskKind::Block), 0)
            .await
            .unwrap();
        let rbd = RuntimeDisk {
            path: PathBuf::new(),
            rbd: Some(opl_types::runtime::RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image: "opl-a".into(),
                user: "admin".into(),
                conf: Some("/etc/ceph/c.conf".into()),
            }),
            format: ImageFormat::Raw,
            ..hot_disk(DiskKind::Rbd)
        };
        hv.add_disk(id, 5, &rbd, 1).await.unwrap();
        let adds: Vec<_> = sent(&fake, &["blockdev-add"])
            .into_iter()
            .map(|(_, a)| a)
            .collect();
        assert_eq!(
            adds[2..],
            [
                serde_json::json!({ "driver": "host_device", "filename": "/pools/a/hot.qcow2",
                    "node-name": "disk4-file" }),
                serde_json::json!({ "driver": "qcow2", "file": "disk4-file", "node-name": "disk4" }),
                serde_json::json!({ "driver": "rbd", "pool": "rbd", "namespace": "opl",
                    "image": "opl-a", "user": "admin", "conf": "/etc/ceph/c.conf",
                    "node-name": "disk5-file" }),
                serde_json::json!({ "driver": "raw", "file": "disk5-file", "node-name": "disk5" }),
            ]
        );

        let before = fake.commands.lock().unwrap().len();
        let sata = RuntimeDisk {
            bus: DiskBus::Sata,
            ..hot_disk(DiskKind::File)
        };
        let err = hv.add_disk(id, 4, &sata, 3).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        assert_eq!(fake.commands.lock().unwrap().len(), before);
    }

    /// 게스트가 디스크를 내려 주지 않으면 장치와 노드를 그대로 두고 false다.
    #[tokio::test]
    async fn a_disk_the_guest_holds_stays() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        hv.add_disk(id, 2, &hot_disk(DiskKind::File), 0)
            .await
            .unwrap();
        *fake.hold_unplug.lock().unwrap() = true;
        assert!(!hv.remove_disk(id, 2, UNPLUG).await.unwrap());
        assert!(sent(&fake, &["blockdev-del"]).is_empty());
        assert_eq!(fake.hot_nodes.lock().unwrap().len(), 2);
    }

    /// 장치가 빠진 뒤 블록 노드·백엔드·tap을 지우지 못해도 떼기는 된 것이다(경고만).
    #[tokio::test]
    async fn cleanup_failures_after_an_unplug_still_count_as_removed() {
        let tmp = short_tempdir();
        let net = Arc::new(RecordingNet::default());
        let (hv, id, fake, _qemu) = running_vm_with_net(tmp.path(), net.clone()).await;
        hv.add_disk(id, 2, &hot_disk(DiskKind::File), 0)
            .await
            .unwrap();
        hv.add_nic(id, 1, &tap_nic(id, 1), 1).await.unwrap();
        fake.failing
            .lock()
            .unwrap()
            .extend(["blockdev-del", "netdev_del"]);
        *net.fail_delete.lock().unwrap() = true;
        assert!(hv.remove_disk(id, 2, UNPLUG).await.unwrap());
        assert_eq!(sent(&fake, &["blockdev-del"]).len(), 2);
        assert!(hv.remove_nic(id, 1, UNPLUG).await.unwrap());
        assert_eq!(sent(&fake, &["netdev_del"]).len(), 1);
        assert_eq!(
            net.calls.lock().unwrap().last(),
            Some(&format!("delete {}", tap_name(id, 1)))
        );
    }

    /// SDN(OVN) NIC: br-int의 tap과 논리 포트
    fn sdn_nic(id: Uuid, index: usize) -> RuntimeNic {
        let mac = "52:54:00:12:34:58".parse().unwrap();
        RuntimeNic {
            mac,
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: tap_name(id, index),
                bridge: "br-int".into(),
                vlan: None,
                mtu: Some(1442),
                ovn_port: Some(opl_types::sdn::port_name(id, mac)),
            },
        }
    }

    /// 마이그레이션을 받는 쪽은 받는 QEMU를 띄우기 전에 SDN tap을 붙이고 OVN 포트가 켜지기를 30초까지 기다린다. 켜지지
    /// 않으면 QEMU 없이 tap을 지우고 마이그레이션 오류다(원본 Task가 시작 전에 실패한다).
    #[tokio::test]
    async fn incoming_waits_for_the_ovn_port_before_qemu() {
        let tmp = short_tempdir();
        let net = Arc::new(RecordingNet::default());
        let mut cfg = config(tmp.path(), PathBuf::from("false"));
        cfg.net = Some(net.clone());
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mut vm = spec();
        vm.nics.push(tap_nic(vm.id, 0));
        vm.nics.push(sdn_nic(vm.id, 1));
        let (plain, sdn) = (tap_name(vm.id, 0), tap_name(vm.id, 1));
        let port = opl_types::sdn::port_name(vm.id, vm.nics[1].mac);

        *net.ovn_down.lock().unwrap() = true;
        let err = hv
            .start_incoming(&vm, Uuid::now_v7(), "127.0.0.1", 60000..=60099, &[])
            .await
            .unwrap_err();
        match err {
            DriverError::Migration(msg) => {
                assert!(msg.contains("OVN did not activate"), "{msg}");
                assert!(msg.contains(&sdn), "{msg}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(
            *net.calls.lock().unwrap(),
            [
                format!("create {plain} br0 Some(10)"),
                format!("create {sdn} br-int None ovn={port} 52:54:00:12:34:58 Some(1442)"),
                format!("wait {sdn} 30s"),
                format!("delete-vm {}", vm.id),
            ]
        );
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());

        // 포트가 켜지면 QEMU를 띄운다(여기서는 곧바로 끝나는 `false`).
        net.calls.lock().unwrap().clear();
        *net.ovn_down.lock().unwrap() = false;
        let err = hv
            .start_incoming(&vm, Uuid::now_v7(), "127.0.0.1", 60000..=60099, &[])
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::StartFailed(_)), "{err:?}");
        assert_eq!(
            net.calls.lock().unwrap()[2..4],
            [format!("wait {sdn} 30s"), format!("delete-vm {}", vm.id)]
        );
    }

    /// 켤 때는 OVN 바인딩을 기다리지 않는다(뒤에서 10초 지켜보고 경고만). SDN NIC가 없는 VM은 OVN을 보지 않는다.
    #[tokio::test]
    async fn start_does_not_wait_for_ovn_and_plain_taps_never_look() {
        let tmp = short_tempdir();
        let net = Arc::new(RecordingNet::default());
        let mut cfg = config(tmp.path(), PathBuf::from("false"));
        cfg.net = Some(net.clone());
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mut vm = spec();
        vm.nics.push(tap_nic(vm.id, 0));
        let plain = tap_name(vm.id, 0);
        assert!(hv.start(&vm, None).await.is_err());
        assert_eq!(
            *net.calls.lock().unwrap(),
            [
                format!("create {plain} br0 Some(10)"),
                format!("delete-vm {}", vm.id),
            ]
        );

        net.calls.lock().unwrap().clear();
        *net.ovn_down.lock().unwrap() = true;
        vm.nics.push(sdn_nic(vm.id, 1));
        let sdn = tap_name(vm.id, 1);
        // 포트가 켜지지 않아도 켜기는 QEMU까지 간다(`false`라 실패).
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        let wait = format!("wait {sdn} 10s");
        for _ in 0..100 {
            if net.calls.lock().unwrap().contains(&wait) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(net.calls.lock().unwrap().contains(&wait));
    }

    fn tap_nic(id: Uuid, index: usize) -> RuntimeNic {
        RuntimeNic {
            mac: "52:54:00:12:34:57".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: tap_name(id, index),
                bridge: "br0".into(),
                vlan: Some(10),
                mtu: Some(1450),
                ovn_port: None,
            },
        }
    }

    /// 브리지 NIC는 tap을 먼저 만들고 백엔드·장치를 붙인다. 장치를 붙이지 못하면 백엔드와 tap을 지운다. 떼기는
    /// 장치·백엔드·tap 순서로 지운다.
    #[tokio::test]
    async fn bridge_nics_are_added_and_removed_with_their_tap() {
        let tmp = short_tempdir();
        let net = Arc::new(RecordingNet::default());
        let (hv, id, fake, _qemu) = running_vm_with_net(tmp.path(), net.clone()).await;
        let (tap1, tap2) = (tap_name(id, 1), tap_name(id, 2));

        hv.add_nic(id, 1, &tap_nic(id, 1), 1).await.unwrap();
        assert_eq!(
            sent(&fake, &["netdev_add", "device_add"]),
            [
                (
                    "netdev_add".to_string(),
                    serde_json::json!({ "type": "tap", "id": "net1",
                    "ifname": tap1, "script": "no", "downscript": "no" })
                ),
                (
                    "device_add".to_string(),
                    serde_json::json!({ "driver": "virtio-net-pci",
                    "netdev": "net1", "mac": "52:54:00:12:34:57", "id": "nic1", "bus": "hp1",
                    "host_mtu": 1450 })
                ),
            ]
        );

        fake.failing.lock().unwrap().push("device_add");
        assert!(hv.add_nic(id, 2, &tap_nic(id, 2), 2).await.is_err());
        let dels = sent(&fake, &["netdev_del"]);
        assert_eq!(
            dels,
            [(
                "netdev_del".to_string(),
                serde_json::json!({ "id": "net2" })
            )]
        );
        fake.failing.lock().unwrap().clear();

        assert!(hv.remove_nic(id, 1, UNPLUG).await.unwrap());
        let removed: Vec<_> = sent(&fake, &["device_del", "netdev_del"])
            .into_iter()
            .map(|(c, a)| (c, a["id"].clone()))
            .collect();
        assert_eq!(
            removed[1..],
            [
                ("device_del".to_string(), serde_json::json!("nic1")),
                ("netdev_del".to_string(), serde_json::json!("net1")),
            ]
        );
        assert_eq!(
            *net.calls.lock().unwrap(),
            [
                format!("create {tap1} br0 Some(10)"),
                format!("create {tap2} br0 Some(10)"),
                format!("delete {tap2}"),
                format!("delete {tap1}"),
            ]
        );
    }

    /// user NIC는 tap 없이 붙고(e1000e는 `host_mtu`가 없다), 다른 VM 이름의 tap과 호스트 네트워크가 없는 노드의
    /// 브리지 NIC는 거절한다. 게스트가 내려 주지 않으면 백엔드를 지우지 않는다.
    #[tokio::test]
    async fn user_nics_and_refused_nics() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let user = RuntimeNic {
            mac: "52:54:00:12:34:58".parse().unwrap(),
            model: NicModel::E1000e,
            backend: NicBackend::User,
        };
        hv.add_nic(id, 1, &user, 0).await.unwrap();
        assert_eq!(
            sent(&fake, &["netdev_add", "device_add"]),
            [
                (
                    "netdev_add".to_string(),
                    serde_json::json!({ "type": "user", "id": "net1" })
                ),
                (
                    "device_add".to_string(),
                    serde_json::json!({ "driver": "e1000e",
                    "netdev": "net1", "mac": "52:54:00:12:34:58", "id": "nic1", "bus": "hp0" })
                ),
            ]
        );
        // 호스트 네트워크가 없는 노드(macOS)
        let err = hv.add_nic(id, 2, &tap_nic(id, 2), 1).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        let other = tap_nic(Uuid::now_v7(), 2);
        let err = hv.add_nic(id, 2, &other, 1).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        assert_eq!(sent(&fake, &["netdev_add"]).len(), 1);

        *fake.hold_unplug.lock().unwrap() = true;
        assert!(!hv.remove_nic(id, 1, UNPLUG).await.unwrap());
        assert!(sent(&fake, &["netdev_del"]).is_empty());
        assert!(matches!(
            hv.remove_nic(Uuid::now_v7(), 1, UNPLUG).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    /// 앞서 중단된 Task가 붙인 장치가 이미 있으면 아무것도 바꾸지 않는다(tap도 그대로). 장치 없이 남은 블록 노드·
    /// 백엔드는 지우고 다시 만든다.
    #[tokio::test]
    async fn devices_already_present_or_left_half_added() {
        let tmp = short_tempdir();
        let net = Arc::new(RecordingNet::default());
        let (hv, id, fake, _qemu) = running_vm_with_net(tmp.path(), net.clone()).await;
        let present = hv
            .add_disk(id, 1, &hot_disk(DiskKind::File), 0)
            .await
            .unwrap();
        assert_eq!(present, AddOutcome::AlreadyPresent);
        let present = hv.add_nic(id, 0, &tap_nic(id, 0), 1).await.unwrap();
        assert_eq!(present, AddOutcome::AlreadyPresent);
        assert!(
            sent(
                &fake,
                &["blockdev-add", "blockdev-del", "netdev_add", "device_add"]
            )
            .is_empty()
        );
        assert!(net.calls.lock().unwrap().is_empty());

        // 장치 없이 남은 노드 `disk3`·`disk3-file`은 닫고 다시 연다.
        fake.hot_nodes
            .lock()
            .unwrap()
            .extend(["disk3-file".to_string(), "disk3".to_string()]);
        let added = hv
            .add_disk(id, 3, &hot_disk(DiskKind::File), 2)
            .await
            .unwrap();
        assert_eq!(added, AddOutcome::Added);
        let ops: Vec<_> = sent(&fake, &["blockdev-del", "blockdev-add", "device_add"])
            .into_iter()
            .map(|(c, a)| {
                (
                    c,
                    a["node-name"]
                        .as_str()
                        .or(a["id"].as_str())
                        .unwrap()
                        .to_string(),
                )
            })
            .collect();
        let ops: Vec<_> = ops.iter().map(|(c, n)| (c.as_str(), n.as_str())).collect();
        assert_eq!(
            ops,
            [
                ("blockdev-del", "disk3"),
                ("blockdev-del", "disk3-file"),
                ("blockdev-add", "disk3-file"),
                ("blockdev-add", "disk3"),
                ("device_add", "disk3-dev"),
            ]
        );

        // 장치 없이 남은 백엔드 `net3`은 지우고 다시 만든다.
        let added = hv.add_nic(id, 3, &tap_nic(id, 3), 3).await.unwrap();
        assert_eq!(added, AddOutcome::Added);
        let ops: Vec<_> = sent(&fake, &["netdev_add", "netdev_del"])
            .into_iter()
            .map(|(c, a)| (c, a["id"].clone()))
            .collect();
        assert_eq!(
            ops,
            [
                ("netdev_add".to_string(), serde_json::json!("net3")),
                ("netdev_del".to_string(), serde_json::json!("net3")),
                ("netdev_add".to_string(), serde_json::json!("net3")),
            ]
        );
        assert_eq!(sent(&fake, &["device_add"]).len(), 2);
    }

    /// `device_del`을 보낸 뒤 QMP가 끊기면 QEMU가 받았을 수 있으므로 게스트가 내려 주지 않은 것처럼 false다.
    #[tokio::test]
    async fn an_unplug_cut_off_after_device_del_is_unfinished() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        hv.add_disk(id, 2, &hot_disk(DiskKind::File), 0)
            .await
            .unwrap();
        fake.drop_on.lock().unwrap().push("device_del");
        assert!(!hv.remove_disk(id, 2, UNPLUG).await.unwrap());
        assert!(sent(&fake, &["blockdev-del"]).is_empty());
    }

    /// 보내기 전의 장치 확인은 도는 QEMU의 id 있는 장치와 견준다.
    #[tokio::test]
    async fn check_devices_compares_the_running_devices() {
        let tmp = short_tempdir();
        let (hv, id, _fake, _qemu) = running_vm(tmp.path()).await;
        let ids = |l: &[&str]| l.iter().map(|d| d.to_string()).collect::<Vec<_>>();
        hv.check_devices(id, &ids(&["nic0", "disk1-dev", "disk0-dev"]))
            .await
            .unwrap();
        let err = hv
            .check_devices(id, &ids(&["disk0-dev", "nic0"]))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("QEMU has disk1-dev that the record does not"),
            "{err}"
        );
    }

    /// 능력은 `-device help`로 읽는다(x86_64만 vCPU). 읽지 못하면 메모리는 못 하는 것으로 한다.
    #[tokio::test]
    async fn hotplug_caps_read_the_device_list() {
        let tmp = short_tempdir();
        let qemu = tmp.path().join("qemu");
        std::fs::write(
            &qemu,
            "#!/bin/sh\n[ \"$1 $2\" = \"-device virtio-mem-pci,help\" ] && { echo '  prealloc=<bool>'; exit 0; }\n[ \"$1 $2\" = \"-device help\" ] || exit 1\necho 'name \"virtio-mem-pci\", bus PCI'\n",
        )
        .unwrap();
        std::fs::set_permissions(&qemu, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = config(tmp.path(), qemu);
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let caps = hv.hotplug_caps().await;
        assert_eq!(
            caps,
            NodeHotplug {
                cpu: false,
                memory: true,
                memory_prealloc: true
            }
        );

        cfg.host.arch = Arch::X86_64;
        cfg.host.qemu_binary = PathBuf::from("/nonexistent/qemu");
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        // x86_64 vCPU 핫플러그는 Linux만이다(macOS는 HVF).
        assert_eq!(
            hv.hotplug_caps().await,
            NodeHotplug {
                cpu: cfg!(target_os = "linux"),
                memory: false,
                memory_prealloc: false
            }
        );
    }

    /// 견주는 장치에 CD-ROM 드라이브와 씨앗 드라이브가 든다(컨트롤러 `scsi0`·비디오는 아니다).
    #[test]
    fn tracked_devices_include_cdroms_and_the_seed() {
        for id in [
            "disk0-dev",
            "nic1",
            "cpu2",
            "vmem0-dev",
            "cd0-dev",
            "cd3-dev",
            "cidata-dev",
        ] {
            assert!(tracked_device(id), "{id}");
        }
        for id in ["scsi0", "cd-dev", "cdx-dev", "cd0", "video0", "cidata"] {
            assert!(!tracked_device(id), "{id}");
        }
    }

    fn iso(path: &str) -> RuntimeMedium {
        RuntimeMedium { path: path.into() }
    }

    const MEDIUM_COMMANDS: &[&str] = &[
        "blockdev-add",
        "blockdev-del",
        "blockdev-open-tray",
        "blockdev-close-tray",
        "blockdev-remove-medium",
        "blockdev-insert-medium",
    ];

    /// 보낸 매체 명령 이름(`blockdev-del`은 노드 이름을 붙임)을 차례로 꺼내고 기록을 비운다.
    fn take_medium_commands(fake: &FakeQmp) -> Vec<String> {
        let names = sent(fake, MEDIUM_COMMANDS)
            .into_iter()
            .map(|(name, args)| match name.as_str() {
                "blockdev-del" => format!("del {}", args["node-name"].as_str().unwrap()),
                "blockdev-add" => format!("add {}", args["node-name"].as_str().unwrap()),
                _ => name,
            })
            .collect();
        fake.commands.lock().unwrap().clear();
        fake.arguments.lock().unwrap().clear();
        names
    }

    /// 매체 넣기·바꾸기·빼기: 빈 드라이브에는 노드를 연 뒤 트레이를 열어 넣고, 바꾸기는 새 매체의 노드(쓰지 않은
    /// 이름)를 먼저 연 뒤 바꿔 넣고 옛 노드를 닫는다. 빼기는 노드를 닫고 빈 드라이브로 둔다. 같은 매체를 다시 넣거나
    /// 빈 드라이브를 빼면 아무것도 보내지 않고, 그때 트레이가 열려 있으면 닫기만 한다.
    #[tokio::test]
    async fn change_medium_inserts_replaces_and_ejects() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        hv.change_medium(id, 0, Some(&iso("/pools/iso/a.raw")))
            .await
            .unwrap();
        assert_eq!(
            take_medium_commands(&fake),
            [
                "add cd0-file",
                "add cd0",
                "blockdev-open-tray",
                "blockdev-remove-medium",
                "blockdev-insert-medium",
                "blockdev-close-tray",
            ]
        );
        assert_eq!(
            fake.cd_medium.lock().unwrap().as_deref(),
            Some("/pools/iso/a.raw")
        );
        // 같은 매체
        hv.change_medium(id, 0, Some(&iso("/pools/iso/a.raw")))
            .await
            .unwrap();
        assert!(take_medium_commands(&fake).is_empty());
        // 같은 매체인데 트레이가 열려 있으면 닫는다.
        *fake.cd_tray.lock().unwrap() = true;
        hv.change_medium(id, 0, Some(&iso("/pools/iso/a.raw")))
            .await
            .unwrap();
        assert_eq!(take_medium_commands(&fake), ["blockdev-close-tray"]);
        assert!(!*fake.cd_tray.lock().unwrap());

        hv.change_medium(id, 0, Some(&iso("/pools/iso/b.raw")))
            .await
            .unwrap();
        assert_eq!(
            take_medium_commands(&fake),
            [
                "add cd0-1-file",
                "add cd0-1",
                "blockdev-open-tray",
                "blockdev-remove-medium",
                "blockdev-insert-medium",
                "del cd0",
                "del cd0-file",
                "blockdev-close-tray",
            ]
        );
        assert_eq!(
            fake.cd_medium.lock().unwrap().as_deref(),
            Some("/pools/iso/b.raw")
        );

        hv.change_medium(id, 0, None).await.unwrap();
        assert_eq!(
            take_medium_commands(&fake),
            [
                "blockdev-open-tray",
                "blockdev-remove-medium",
                "del cd0-1",
                "del cd0-1-file",
                "blockdev-close-tray",
            ]
        );
        assert_eq!(*fake.cd_medium.lock().unwrap(), None);
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        hv.change_medium(id, 0, None).await.unwrap();
        assert!(take_medium_commands(&fake).is_empty());
    }

    /// 넣다가 실패하면 연 노드를 닫고 트레이를 닫은 뒤 오류다(드라이브는 빈 채). 다시 넣으면 된다. 없는 드라이브는
    /// 명령 없이 `InvalidSpec`이다.
    #[tokio::test]
    async fn a_failed_medium_change_closes_what_it_opened() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        fake.failing.lock().unwrap().push("blockdev-insert-medium");
        let err = hv
            .change_medium(id, 0, Some(&iso("/pools/iso/a.raw")))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Qmp(_)), "{err:?}");
        let sent = take_medium_commands(&fake);
        assert_eq!(
            sent[sent.len() - 3..],
            ["del cd0", "del cd0-file", "blockdev-close-tray"]
        );
        assert!(fake.hot_nodes.lock().unwrap().is_empty());
        assert_eq!(*fake.cd_medium.lock().unwrap(), None);

        fake.failing.lock().unwrap().clear();
        hv.change_medium(id, 0, Some(&iso("/pools/iso/a.raw")))
            .await
            .unwrap();
        assert_eq!(
            fake.cd_medium.lock().unwrap().as_deref(),
            Some("/pools/iso/a.raw")
        );
        take_medium_commands(&fake);

        let err = hv
            .change_medium(id, 1, Some(&iso("/pools/iso/a.raw")))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        assert!(take_medium_commands(&fake).is_empty());
    }

    fn seed() -> CloudInitSeed {
        CloudInitSeed {
            instance_id: "iid-0123456789abcdef".into(),
            meta_data: "{\"instance-id\": \"iid-0123456789abcdef\"}".into(),
            user_data: "#cloud-config\n{}".into(),
            vendor_data: None,
            network_config: Some("{\"version\": 1}".into()),
        }
    }

    /// 켤 때 cloud-init 씨앗을 실행 디렉터리의 `cidata.iso`(0600, 같은 씨앗이면 같은 바이트)로 써 두고 QEMU를 띄운다.
    /// 씨앗이 없으면 앞서 남은 파일을 지운다. 가짜 QEMU가 띄워질 때의 파일을 복사해 두고 실패로 끝난다.
    #[tokio::test]
    async fn launch_writes_the_cloud_init_seed() {
        let tmp = short_tempdir();
        let vm = VmRuntimeSpec {
            cloud_init: Some(seed()),
            ..spec()
        };
        let dir = tmp.path().join("run/vm").join(vm.id.to_string());
        let seen = tmp.path().join("seen.iso");
        let fake = tmp.path().join("fake-qemu");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nif [ -e '{0}/cidata.iso' ]; then cp -p '{0}/cidata.iso' '{1}'; fi\nexit 1\n",
                dir.display(),
                seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hv = QemuHypervisor::new(config(tmp.path(), fake)).await.unwrap();

        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        let want =
            opl_qemu::iso9660::build("cidata", &opl_qemu::iso9660::cidata_files(&seed())).unwrap();
        assert_eq!(std::fs::read(&seen).unwrap(), want);
        let mode = std::fs::metadata(&seen).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        // 씨앗이 없는 VM: 남은 파일(앞선 QEMU의 것)을 지우고 띄운다.
        std::fs::remove_file(&seen).unwrap();
        let plain = VmRuntimeSpec {
            id: vm.id,
            ..spec()
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cidata.iso"), b"stale").unwrap();
        assert!(hv.start(&plain, None).await.is_err());
        assert!(!seen.exists());
    }

    /// 가짜 swtpm·swtpm_setup(`tests/fixtures`, python3)
    fn fake_swtpm() -> SwtpmConfig {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        SwtpmConfig {
            swtpm: dir.join("fake-swtpm"),
            swtpm_setup: dir.join("fake-swtpm-setup"),
        }
    }

    fn failing_qemu(dir: &Path) -> PathBuf {
        let fake = dir.join("fake-qemu");
        std::fs::write(&fake, "#!/bin/sh\necho 'qemu: no' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    /// TPM VM: 새 상태를 만들고 swtpm을 먼저 띄운다. QEMU를 띄우지 못하면 swtpm을 끝내고 실행 디렉터리를 지운다.
    /// 상태 파일은 남는다(같은 TPM으로 다시 켠다).
    #[tokio::test]
    async fn a_failed_qemu_takes_its_swtpm_down() {
        let tmp = short_tempdir();
        let mut cfg = config(tmp.path(), failing_qemu(tmp.path()));
        cfg.swtpm = fake_swtpm();
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let vm = VmRuntimeSpec {
            tpm: Some(opl_types::runtime::RuntimeTpm {
                fresh: true,
                epoch: 0,
            }),
            ..spec()
        };
        let err = hv.start(&vm, None).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::StartFailed(m) if m == "qemu: no"),
            "{err}"
        );
        let state = tmp.path().join("state");
        assert!(crate::tpm::tpm_state_path(&state, vm.id).exists());
        let run = tmp.path().join("run/vm").join(vm.id.to_string());
        assert!(!run.exists());
        // 남은 swtpm이 있으면 상태 디렉터리 잠금이 남는다: 가짜 swtpm은 잠금을 다시 잡을 수 있어야 뜬다.
        let paths = hv.paths(vm.id).unwrap();
        std::fs::create_dir_all(&paths.dir).unwrap();
        crate::tpm::start(
            &fake_swtpm(),
            false,
            vm.id,
            &paths,
            &vm_tpm_dir(&state, vm.id),
            None,
        )
        .await
        .unwrap();
        assert!(crate::tpm::terminate(&paths).await);

        // fresh가 아닌데 상태가 없으면 띄우지 않는다.
        let other = VmRuntimeSpec {
            tpm: Some(opl_types::runtime::RuntimeTpm {
                fresh: false,
                epoch: 0,
            }),
            ..spec()
        };
        let err = hv.start(&other, None).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::InvalidSpec(m) if m.contains("is missing on this node")),
            "{err}"
        );
    }

    /// QEMU 없이 swtpm만 남은 실행 디렉터리(띄우는 도중 opld가 죽음)는 다시 켤 때 swtpm을 끝내고 지운다.
    #[tokio::test]
    async fn restart_stops_a_swtpm_left_without_its_qemu() {
        let tmp = short_tempdir();
        let mut cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        cfg.swtpm = fake_swtpm();
        let id = Uuid::now_v7();
        let paths = RuntimePaths::new(&cfg.run_dir, id).unwrap();
        std::fs::create_dir_all(&paths.dir).unwrap();
        let dir = vm_tpm_dir(&cfg.state_dir, id);
        std::fs::create_dir_all(&dir).unwrap();
        crate::tpm::start(&cfg.swtpm, false, id, &paths, &dir, None)
            .await
            .unwrap();
        assert!(crate::tpm::alive(&paths).is_some());
        let _hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!paths.dir.exists());
        assert!(crate::tpm::alive(&paths).is_none());
    }

    /// Secure Boot VM은 MS 키 변수 원본에서 변수를 만든다. 이 노드에 Secure Boot 펌웨어가 없으면 띄우지 않는다.
    #[tokio::test]
    async fn secure_boot_vars_come_from_the_enrolled_template() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), failing_qemu(tmp.path()));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let vm = VmRuntimeSpec {
            secure_boot: true,
            ..spec()
        };
        let err = hv.start(&vm, None).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::InvalidSpec(m) if m.contains("no Secure Boot firmware")),
            "{err}"
        );

        let mut cfg = cfg;
        let ms = tmp.path().join("ms-vars.fd");
        std::fs::write(&ms, b"ms keys").unwrap();
        cfg.host.secure_boot = Some(opl_qemu::FirmwarePair {
            code: tmp.path().join("sb-code.fd"),
            vars_template: ms,
        });
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.start(&vm, None).await.is_err());
        let vars = uefi_vars_path(&tmp.path().join("state"), vm.id);
        assert_eq!(std::fs::read(vars).unwrap(), b"ms keys");
    }

    #[tokio::test]
    async fn firmware_caps_read_swtpm_and_the_secure_boot_files() {
        let tmp = short_tempdir();
        let qemu = tmp.path().join("fake-qemu");
        std::fs::write(
            &qemu,
            "#!/bin/sh\necho 'Supported TPM types (choose only one):' >&2\necho '    emulator   TPM emulator backend driver' >&2\n",
        )
        .unwrap();
        std::fs::set_permissions(&qemu, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = config(tmp.path(), qemu);
        cfg.swtpm = fake_swtpm();
        let code = tmp.path().join("sb-code.fd");
        cfg.host.secure_boot = Some(opl_qemu::FirmwarePair {
            code: code.clone(),
            vars_template: tmp.path().join("vars-template.fd"),
        });
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        // 코드 파일이 없으면 Secure Boot는 못 한다.
        assert_eq!(
            hv.firmware_caps().await,
            FirmwareCaps {
                tpm: true,
                secure_boot: false
            }
        );
        std::fs::write(&code, b"code").unwrap();
        assert!(hv.firmware_caps().await.secure_boot);
        cfg.swtpm.swtpm = "/nonexistent/swtpm".into();
        cfg.host.secure_boot = None;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert_eq!(hv.firmware_caps().await, FirmwareCaps::default());
    }

    /// 가짜 게스트 에이전트: sync에 답하고, 다른 명령은 `reply`가 정한다(None이면 연결을 끊는다). 받은 명령을 남긴다.
    fn fake_agent(
        path: &Path,
        reply: impl Fn(&str) -> Option<serde_json::Value> + Send + Sync + 'static,
    ) -> Arc<Mutex<Vec<String>>> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let _ = std::fs::remove_file(path);
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let reply = Arc::new(reply);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (log, reply) = (log.clone(), reply.clone());
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut read = BufReader::new(read);
                    let mut buf = Vec::new();
                    loop {
                        buf.clear();
                        if read.read_until(b'\n', &mut buf).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let Some(at) = buf.iter().position(|b| *b == b'{') else {
                            continue;
                        };
                        let cmd: serde_json::Value = serde_json::from_slice(&buf[at..]).unwrap();
                        let name = cmd["execute"].as_str().unwrap().to_string();
                        let out = if name == "guest-sync-delimited" {
                            let mut out = vec![0xFF];
                            out.extend_from_slice(
                                format!("{}\n", json!({ "return": cmd["arguments"]["id"] }))
                                    .as_bytes(),
                            );
                            out
                        } else {
                            log.lock().unwrap().push(name.clone());
                            match reply(&name) {
                                Some(v) => format!("{v}\n").into_bytes(),
                                None => return,
                            }
                        };
                        if write.write_all(&out).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        seen
    }

    /// 에이전트 종료: 에이전트가 받으면(답 없이 끊음) ACPI를 보내지 않는다. 오류로 답하거나 채널이 없으면 ACPI다.
    #[tokio::test]
    async fn agent_stop_falls_back_to_acpi() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let sock = hv.paths(id).unwrap().qga_socket;
        // 채널 없음
        hv.stop(id, StopMode::Agent).await.unwrap();
        assert_eq!(sent(&fake, &["system_powerdown"]).len(), 1);
        let seen = fake_agent(&sock, |_| None);
        hv.stop(id, StopMode::Agent).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), ["guest-shutdown"]);
        assert_eq!(sent(&fake, &["system_powerdown"]).len(), 1);
        let _seen = fake_agent(&sock, |_| {
            Some(json!({ "error": { "class": "GenericError", "desc": "denied" } }))
        });
        hv.stop(id, StopMode::Agent).await.unwrap();
        assert_eq!(sent(&fake, &["system_powerdown"]).len(), 2);
    }

    /// 얼리기: 시점을 찍는 transaction 직전에 얼리고 직후에 녹인다. 얼리지 못해도 찍고 이유를 돌려준다. 묻지 않으면
    /// 에이전트에 연결하지 않는다.
    #[tokio::test]
    async fn quiesced_snapshots_freeze_around_the_transaction() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;
        let paths = hv.paths(id).unwrap();
        let commands = fake.commands.clone();
        let mark = paths.dir.join(agent::FSFROZEN_MARK);
        let frozen_mark = mark.clone();
        let seen = fake_agent(&paths.qga_socket, move |cmd| {
            let snapshots = commands
                .lock()
                .unwrap()
                .iter()
                .filter(|c| *c == "transaction")
                .count();
            match cmd {
                // 얼릴 때는 아직 찍지 않았고 표시가 있다.
                "guest-fsfreeze-freeze" => {
                    assert_eq!(snapshots, 0);
                    assert!(frozen_mark.exists());
                    Some(json!({ "return": 2 }))
                }
                "guest-fsfreeze-thaw" => {
                    assert_eq!(snapshots, 1);
                    Some(json!({ "return": 2 }))
                }
                other => panic!("unexpected {other}"),
            }
        });
        assert_eq!(
            hv.snapshot_save(id, "s1", false, true).await.unwrap(),
            Quiesced::Yes
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["guest-fsfreeze-freeze", "guest-fsfreeze-thaw"]
        );
        assert!(!mark.exists());

        // 묻지 않으면 연결하지 않는다. 메모리 스냅샷도 얼리지 않는다.
        seen.lock().unwrap().clear();
        assert_eq!(
            hv.snapshot_save(id, "s2", false, false).await.unwrap(),
            Quiesced::No
        );
        assert!(seen.lock().unwrap().is_empty());

        let seen = fake_agent(&paths.qga_socket, |cmd| match cmd {
            "guest-fsfreeze-freeze" => Some(
                json!({ "error": { "class": "GenericError", "desc": "fsfreeze is not supported" } }),
            ),
            _ => Some(json!({ "return": 0 })),
        });
        let q = hv
            .snapshot_save_with_bitmap(id, "s3", "opl-x", true)
            .await
            .unwrap();
        assert_eq!(
            q.warning().unwrap(),
            "guest agent did not freeze file systems (guest agent error (GenericError): fsfreeze is not supported); the snapshot is crash-consistent"
        );
        // 실패한 freeze 뒤에도 녹여 둔다.
        assert_eq!(
            *seen.lock().unwrap(),
            ["guest-fsfreeze-freeze", "guest-fsfreeze-thaw"]
        );
        assert!(!mark.exists());
        assert_eq!(sent(&fake, &["transaction"]).len(), 3);
    }

    /// 얼린 채 opld가 죽었으면(표시가 남음) 다시 붙을 때 녹인다.
    #[tokio::test]
    async fn reattaching_thaws_file_systems_left_frozen() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let _fake = fake_qmp(&dir, "running").await;
        std::fs::write(dir.join(agent::FSFROZEN_MARK), b"").unwrap();
        let seen = fake_agent(&dir.join("qga.sock"), |_| Some(json!({ "return": 1 })));
        let _hv = QemuHypervisor::new(cfg).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), ["guest-fsfreeze-thaw"]);
        assert!(!dir.join(agent::FSFROZEN_MARK).exists());
    }

    /// 에이전트 잠금은 얼리기부터 녹이기까지만 쥔다(리뷰 1 C1). 그 사이 ping은 기다리지 않고 답 없음, 정보는 바쁨이며,
    /// 녹인 뒤에는 곧바로 답한다.
    #[tokio::test]
    async fn the_agent_lock_covers_only_the_freeze_window() {
        let tmp = short_tempdir();
        let (hv, id, _fake, _qemu) = running_vm(tmp.path()).await;
        let paths = hv.paths(id).unwrap();
        let seen = fake_agent(&paths.qga_socket, |_| Some(json!({ "return": 0 })));
        let handle = hv.handle(id).unwrap();
        let mut freezer = Freezer::new(true, &handle.paths, &handle.agent);
        freezer.freeze(id).await;
        let start = std::time::Instant::now();
        assert!(!hv.guest_ping(id).await.unwrap());
        let err = hv.guest_info(id).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::GuestAgent(m) if m.contains("busy")),
            "{err}"
        );
        assert!(start.elapsed() < AGENT_LOCK_WAIT * 3);
        freezer.thaw(id).await;
        assert!(hv.guest_ping(id).await.unwrap());
        assert_eq!(freezer.outcome(), Quiesced::Yes);
        assert_eq!(
            *seen.lock().unwrap(),
            ["guest-fsfreeze-freeze", "guest-fsfreeze-thaw", "guest-ping"]
        );
    }

    /// 녹이지 못해 표시가 남으면 다음 ping·정보 수집이 먼저 녹인다(리뷰 1 I4). 녹이기 전에 버려진 얼리기도 녹인다.
    #[tokio::test]
    async fn file_systems_left_frozen_are_thawed_by_the_next_poll() {
        let tmp = short_tempdir();
        let (hv, id, _fake, _qemu) = running_vm(tmp.path()).await;
        let paths = hv.paths(id).unwrap();
        let mark = paths.dir.join(agent::FSFROZEN_MARK);
        let seen = fake_agent(&paths.qga_socket, |_| Some(json!({ "return": 0 })));
        std::fs::write(&mark, b"").unwrap();
        assert!(hv.guest_ping(id).await.unwrap());
        assert_eq!(*seen.lock().unwrap(), ["guest-fsfreeze-thaw", "guest-ping"]);
        assert!(!mark.exists());

        seen.lock().unwrap().clear();
        let handle = hv.handle(id).unwrap();
        let mut freezer = Freezer::new(true, &handle.paths, &handle.agent);
        freezer.freeze(id).await;
        assert!(mark.exists());
        drop(freezer);
        for _ in 0..100 {
            if !mark.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!mark.exists());
        assert_eq!(
            *seen.lock().unwrap(),
            ["guest-fsfreeze-freeze", "guest-fsfreeze-thaw"]
        );
    }

    /// 버려진 얼리기의 녹이기는 끝날 때까지 에이전트 잠금을 쥔다(리뷰 2 #4): 다음 회차가 그사이 얼리지 못한다.
    #[tokio::test]
    async fn an_abandoned_freeze_keeps_the_agent_lock_until_thawed() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let tmp = short_tempdir();
        let (hv, id, _fake, _qemu) = running_vm(tmp.path()).await;
        let paths = hv.paths(id).unwrap();
        let thaws = Arc::new(AtomicU32::new(0));
        let count = thaws.clone();
        let _seen = fake_agent(&paths.qga_socket, move |cmd| {
            if cmd == "guest-fsfreeze-thaw" && count.fetch_add(1, Ordering::SeqCst) < 2 {
                return Some(json!({ "error": { "class": "GenericError", "desc": "busy" } }));
            }
            Some(json!({ "return": 0 }))
        });
        let handle = hv.handle(id).unwrap();
        let mut freezer = Freezer::new(true, &handle.paths, &handle.agent);
        freezer.freeze(id).await;
        drop(freezer);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            handle.agent.try_lock().is_err(),
            "the background thaw does not hold the agent lock"
        );
        let mark = paths.dir.join(agent::FSFROZEN_MARK);
        for _ in 0..200 {
            if handle.agent.try_lock().is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(handle.agent.try_lock().is_ok());
        assert!(!mark.exists());
        assert_eq!(thaws.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn guest_info_and_password_go_through_the_agent() {
        let tmp = short_tempdir();
        let (hv, id, _fake, _qemu) = running_vm(tmp.path()).await;
        let err = hv.guest_info(id).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::InvalidSpec(m) if m.contains("no guest agent channel")),
            "{err}"
        );
        let sock = hv.paths(id).unwrap().qga_socket;
        let seen = fake_agent(&sock, |cmd| match cmd {
            "guest-get-host-name" => Some(json!({ "return": { "host-name": "web-1" } })),
            "guest-set-user-password" => Some(json!({ "return": {} })),
            _ => Some(json!({ "error": { "class": "CommandNotFound", "desc": "no" } })),
        });
        let info = hv.guest_info(id).await.unwrap();
        assert_eq!(info.hostname.as_deref(), Some("web-1"));
        assert!(info.os.is_none());
        assert!(info.collected_at > 0);
        hv.set_user_password(id, "root", "pw").await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 6);

        // 동기화에 답하지 않는 채널(에이전트 없음)
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let err = hv.guest_info(id).await.unwrap_err();
        assert!(matches!(err, DriverError::GuestAgent(_)), "{err}");
        drop(listener);
    }
}
