use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::QmpError;
use opl_types::backup::{BITMAP_PREFIX, is_backup_bitmap};
use opl_types::cloud_init::CloudInitSeed;
use opl_types::guest::GuestInfo;
use opl_types::node::{FirmwareCaps, NodeHotplug};
use opl_types::replication::{MIGRATION_BITMAP_PREFIX, migration_bitmap};
use opl_types::runtime::{ImageFormat, RuntimeDisk, RuntimeMedium, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{CpuSpec, DiskBus, RunningHardware};
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, AddOutcome, BackupTarget, BitmapInfo, DiskBitmaps, DiskCounters, EventKind,
    Hypervisor, HypervisorEvent, Quiesced, RuntimeStatus, StopMode, VcpuChange,
};
use crate::migration::{
    CatchUp, CatchUpState, IncomingPorts, MigrateOptions, MigrateTarget, MirrorOptions,
    MirrorProgress, MirrorTarget, format_duration,
};
use crate::replication::{ReceiverExport, ReplicaDiskTarget, ReplicateRequest};

/// 따로 정하지 않았을 때 `qemu_version`이 돌려주는 버전
pub const MOCK_QEMU_VERSION: &str = "8.2.2";

/// `mirror_disks`가 디스크 하나마다 복사한 것으로 알리는 바이트(가짜 진행률)
pub const MOCK_MIRROR_BYTES: u64 = 1 << 30;

/// `incremental_backup` 호출 한 번: (VM, 앞 비트맵, 새 비트맵, 대상)
pub type IncrementalCall = (Uuid, String, String, Vec<BackupTarget>);

/// `replicate_disks`가 증분(기준 비트맵 있음) 디스크 하나마다 보낸 것으로 알리는 바이트
pub const MOCK_INCREMENT_BYTES: u64 = 1 << 20;

/// `replicate_disks` 호출 하나([`MockHypervisor::replications`]). 실패한 호출 포함
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationCall {
    pub vm: Uuid,
    /// 새 비트맵
    pub bitmap: String,
    /// 디스크마다의 대상(기준 비트맵이 있으면 증분)
    pub targets: Vec<ReplicaDiskTarget>,
    /// 성공했으면 보낸 바이트(전체 디스크는 [`MOCK_MIRROR_BYTES`], 증분은 [`MOCK_INCREMENT_BYTES`])
    pub bytes: Option<u64>,
}

/// `catch_up_disks` 호출 하나([`MockHypervisor::catch_ups`]). 실패한 호출 포함
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUpCall {
    pub vm: Uuid,
    /// 마이그레이션 Task
    pub task: Uuid,
    pub targets: Vec<MirrorTarget>,
    /// 디스크마다 첫 패스의 기준 비트맵
    pub base: Vec<Option<String>>,
    /// 성공했으면 한 패스 수와 보낸 바이트(첫 패스는 기준이 있는 디스크마다 [`MOCK_INCREMENT_BYTES`], 없는 디스크마다
    /// [`MOCK_MIRROR_BYTES`], 다음 패스는 디스크마다 [`MOCK_INCREMENT_BYTES`])
    pub done: Option<(u8, u64)>,
}

/// 핫플러그 호출 하나([`MockHypervisor::hotplug_calls`]). 실패한 호출 포함
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotplugCall {
    SetVcpus {
        id: Uuid,
        from: u16,
        to: u16,
    },
    SetMemory {
        id: Uuid,
        plugged_mib: u32,
    },
    AddDisk {
        id: Uuid,
        index: usize,
        port: u8,
    },
    RemoveDisk {
        id: Uuid,
        index: usize,
    },
    AddNic {
        id: Uuid,
        index: usize,
        port: u8,
    },
    RemoveNic {
        id: Uuid,
        index: usize,
    },
    /// CD-ROM 드라이브 `index`의 매체 바꾸기(None = 빼기)
    ChangeMedium {
        id: Uuid,
        index: usize,
        medium: Option<PathBuf>,
    },
}

/// 실행 중인 VM의 핫플러그 상태(켤 때 런타임 스펙에서 정하고 핫플러그 호출이 바꾼다)
#[derive(Debug, Clone, Default)]
struct HotState {
    vcpus: u16,
    plugged_mib: u32,
    /// virtio-mem 크기(상한 − 켤 때 메모리). 0이면 virtio-mem이 없다.
    vmem_mib: u32,
    /// 빈 루트 포트 수
    ports: u8,
    /// 장치 id(`disk{i}-dev`·`nic{i}`·`cd{i}-dev`·`cidata-dev`)와 그 루트 포트(켤 때 단 장치는 None — `pcie.0`이라 뗄 수
    /// 없다)
    devices: BTreeMap<String, Option<u8>>,
    /// CD-ROM 드라이브마다 넣은 매체 파일(빈 드라이브는 None)
    media: Vec<Option<PathBuf>>,
    /// 켤 때 넣은 cloud-init 씨앗
    seed: Option<CloudInitSeed>,
}

/// 게스트 에이전트가 `guest_ping`에 어떻게 답하는지([`MockHypervisor::set_guest_agent`])
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestAnswer {
    /// 채널이 없다(오류).
    None,
    /// 답한다(true).
    Ok,
    /// 답하지 않는다(false, 게스트 에이전트 없음·멈춤).
    Hang,
}

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
    /// 멈춘 `snapshot_save`가 들어왔다
    save_reached: Notify,
    /// 멈춘 `start`가 들어왔다
    start_reached: Notify,
    /// 멈춘 `start`를 놓아준다
    start_released: Notify,
    /// 멈춘 `mirror_disks`가 들어왔다
    mirror_reached: Notify,
    /// 멈춘 `mirror_disks`를 놓아준다
    mirror_released: Notify,
    /// 멈춘 `incoming_wait`가 들어왔다
    incoming_wait_reached: Notify,
    /// 멈춘 `incoming_wait`를 놓아준다
    incoming_wait_released: Notify,
    /// 멈춘 `set_vcpus`가 들어왔다
    hotplug_reached: Notify,
    /// 멈춘 `set_vcpus`를 놓아준다
    hotplug_released: Notify,
    /// 멈춘 `replicate_disks`가 들어왔다
    replicate_reached: Notify,
    /// 멈춘 `replicate_disks`를 놓아준다
    replicate_released: Notify,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
    /// 이후의 `start`를 차례로 실패시킬 오류들(`fail_next_start` 다음에 쓴다)
    fail_starts: VecDeque<DriverError>,
    /// `start`가 이만큼 기다린 뒤에 동작한다.
    start_delay: Option<Duration>,
    /// 다음 `start` 한 번을 `release_start`까지 멈춘다.
    hold_next_start: bool,
    /// 다음 `snapshot_save`·`snapshot_delete` 한 번을 실패시킬 오류
    fail_next_snapshot: Option<DriverError>,
    /// 다음 `snapshot_save` 한 번을 VM이 꺼질 때까지 멈춘다.
    hang_next_save: bool,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
    unresponsive: HashSet<Uuid>,
    /// SIGKILL에도 끝나지 않는(D 상태) VM. `stop(Kill)`이 시간 초과로 실패한다.
    unkillable: HashSet<Uuid>,
    console_dir: Option<PathBuf>,
    /// 새로 띄우려 한 VM과 불러오려 한 스냅샷 태그 (실패한 시도 포함)
    launches: Vec<(Uuid, Option<String>)>,
    /// VM별 내부 스냅샷 (태그, 메모리 포함)
    snapshots: HashMap<Uuid, Vec<(String, bool)>>,
    /// `block_resize`로 바꾼 (VM, 디스크 번호)별 크기
    disk_sizes: HashMap<(Uuid, usize), u64>,
    /// `pause`로 멈추고 아직 `resume`하지 않은 VM
    paused: HashSet<Uuid>,
    /// 멈춘 채 남은 표시(`paused-by-opl`)가 있는 VM(이전 opld가 멈춘 채 죽었다)
    marked: HashSet<Uuid>,
    /// `pause`·`resume` 호출 (VM, 이름)
    pauses: Vec<(Uuid, &'static str)>,
    /// 조회에 드러낼 상태(QEMU가 스스로 멈춘 상태 등)
    statuses: HashMap<Uuid, RuntimeStatus>,
    /// `set_qemu_version`으로 정한 QEMU 버전
    qemu_version: Option<String>,
    /// 받는 중 표시가 있는 VM(`start_incoming`으로 띄우고 아직 `finish_incoming`하지 않음)과 그
    /// 마이그레이션 Task
    incoming: HashMap<Uuid, Uuid>,
    /// 마이그레이션이 만든 상태: 받는 쪽 `inmigrate`·받은 뒤 멈춤, 보낸 원본 `postmigrate`.
    /// VM이 꺼지면 사라진다.
    migration_status: HashMap<Uuid, RuntimeStatus>,
    /// 다음 `migrate` 한 번을 실패시킬 오류
    fail_next_migrate: Option<DriverError>,
    /// 다음 `incoming_wait` 한 번을 실패시킬 오류. 실제 QEMU처럼 받는 QEMU는 끝난다.
    fail_next_incoming: Option<DriverError>,
    /// `migrate` 호출 (VM, 보낸 곳). 실패한 호출 포함
    migrations: Vec<(Uuid, MigrateTarget)>,
    /// 보내는 중인 마이그레이션이 있는 VM(누구도 끝을 기다리지 않는다). 원본은 돈다.
    outgoing: HashSet<Uuid>,
    /// 다음 `migrate` 한 번을 이 오류로 실패시키되 마이그레이션은 보내는 중으로 남긴다.
    fail_next_migrate_unfinished: Option<DriverError>,
    /// `cancel_migration`이 취소하는 사이에 마이그레이션이 끝난다(원본은 `postmigrate`).
    complete_on_cancel: bool,
    /// `migrate`가 이만큼 기다린 뒤에 동작한다.
    migrate_delay: Option<Duration>,
    /// VM별 디스크 형식(마지막으로 띄운 스펙의 디스크 순서). 비트맵이 persistent인지 정한다.
    disk_formats: HashMap<Uuid, Vec<ImageFormat>>,
    /// VM별 디스크마다의 dirty bitmap. 실행 중일 때만 다룬다. VM이 꺼지면 persistent인 것만 남는다
    /// (qcow2 이미지에 저장된 것처럼).
    bitmaps: HashMap<Uuid, Vec<Vec<BitmapInfo>>>,
    /// 다음 `incremental_backup` 한 번을 실패시킬 오류. 실제 드라이버처럼 비트맵은 그대로다.
    fail_next_incremental: Option<DriverError>,
    /// `incremental_backup` 호출(실패 포함)
    incrementals: Vec<IncrementalCall>,
    /// `clear_backup_leftovers`를 부른 VM(실패 포함)
    leftover_clears: Vec<Uuid>,
    /// 받는 QEMU가 NBD로 내보내는 디스크(`start_incoming`의 `exports`, `finish_incoming`까지)
    exports: HashMap<Uuid, Vec<usize>>,
    /// `mirror_disks` 호출 (VM, 대상). 실패한 호출 포함
    mirror_calls: Vec<(Uuid, Vec<MirrorTarget>)>,
    /// ready인 미러가 남은 VM과 그 디스크 순서 번호
    mirroring: HashMap<Uuid, Vec<usize>>,
    /// 스위치오버에서 미러를 끝내고 보낸 마이그레이션의 VM
    switchovers: Vec<Uuid>,
    /// 다음 `mirror_disks` 한 번을 실패시킬 오류
    fail_next_mirror: Option<DriverError>,
    /// 다음 `mirror_disks` 한 번이 복사하지 못하고 정체로 실패한다.
    stall_next_mirror: bool,
    /// 다음 스위치오버 한 번에 미러가 이 오류로 끝난다(마이그레이션은 취소된다).
    fail_next_switchover: Option<String>,
    /// 다음 `mirror_disks` 한 번을 미러를 시작한 채 `release_mirror`까지 멈춘다.
    hold_next_mirror: bool,
    /// 다음 `incoming_wait` 한 번을 받기를 마친 뒤 `release_incoming_wait`까지 멈춘다.
    hold_next_incoming_wait: bool,
    /// 다음 `cancel_migration`들을 차례로 실패시킬 오류
    fail_next_cancel: VecDeque<DriverError>,
    /// `set_disk_stats`로 정한 VM별 디스크 카운터. VM이 꺼지면 사라진다(다시 켜면 0).
    disk_stats: HashMap<Uuid, DiskCounters>,
    /// `disk_stats`가 시간 초과로 실패하는 VM. VM이 꺼지면 사라진다.
    failing_disk_stats: HashSet<Uuid>,
    /// `list`가 이만큼 기다린 뒤에 답한다.
    list_delay: Option<Duration>,
    /// `disk_stats`가 이만큼 기다린 뒤에 답한다.
    disk_stats_delay: Option<Duration>,
    /// 게스트 에이전트 채널(`guest_agent`)을 달고 띄운 VM. VM이 꺼지면 사라진다.
    guest_channels: HashSet<Uuid>,
    /// `set_guest_agent`로 정한 VM별 답. 다시 켜도 남는다.
    guest_answers: HashMap<Uuid, GuestAnswer>,
    /// `guest_ping` 호출(실패 포함)
    guest_pings: Vec<Uuid>,
    /// 새로 띄우려 한 VM의 런타임 스펙(`launches`와 같은 순서, 실패한 시도 포함)
    launch_specs: Vec<VmRuntimeSpec>,
    /// `start_incoming`으로 받으려 한 VM의 런타임 스펙(실패한 시도 포함)
    incoming_specs: Vec<VmRuntimeSpec>,
    /// 실행 중인 VM의 핫플러그 상태. VM이 꺼지면 사라진다.
    hot: HashMap<Uuid, HotState>,
    /// `set_hotplug_caps`로 정한 능력(없으면 vCPU·메모리 모두)
    hotplug_caps: Option<NodeHotplug>,
    /// 떼기·줄이기를 게스트가 내려 주지 않는 VM(시간 제한 길)
    hold_unplug: HashSet<Uuid>,
    /// QEMU가 vCPU 떼기를 거절하는 VM(게스트가 vCPU 떼기를 켜지 않은 q35)
    refuse_vcpu_unplug: HashSet<Uuid>,
    /// 다음 핫플러그 호출 한 번을 이 문구의 QMP 오류로 실패시킨다(아무것도 바꾸지 않는다).
    fail_next_hotplug: Option<String>,
    /// 다음 `set_vcpus` 한 번이 `release_hotplug`까지 멈춘다.
    hold_next_hotplug: bool,
    /// 핫플러그 호출(실패 포함)
    hotplug_calls: Vec<HotplugCall>,
    /// `save_hardware`로 남긴 하드웨어 기록. VM이 꺼지면 사라진다.
    saved_hardware: HashMap<Uuid, RunningHardware>,
    /// `replicate_disks` 호출(실패 포함)
    replications: Vec<ReplicationCall>,
    /// 다음 `replicate_disks` 한 번을 실패시킬 오류. 새 비트맵은 만들지 않는다.
    fail_next_replicate: Option<DriverError>,
    /// 다음 `replicate_disks` 한 번을 `release_replicate`까지 멈춘다.
    hold_next_replicate: bool,
    /// 살아 있는 받는 QEMU(회차별 내보내기)
    receivers: BTreeMap<Uuid, Vec<ReceiverExport>>,
    /// 다음 `start_replica_receiver` 한 번을 실패시킬 오류(받는 QEMU는 남지 않는다)
    fail_next_receiver_start: Option<DriverError>,
    /// 다음 `finish_replica_receiver` 한 번을 실패시킬 오류(받는 QEMU는 남는다)
    fail_next_receiver_finish: Option<DriverError>,
    /// `catch_up_disks` 호출(실패 포함)
    catch_ups: Vec<CatchUpCall>,
    /// 다음 `catch_up_disks` 한 번을 실패시킬 오류(임시 비트맵은 남지 않는다)
    fail_next_catch_up: Option<DriverError>,
    /// `catch_up_disks`가 할 패스 수(요청의 최댓값까지, 정하지 않으면 1)
    catch_up_passes: Option<u8>,
    /// 따라잡기를 마치고 스위치오버를 기다리는 VM
    catching_up: HashMap<Uuid, CatchUpState>,
    /// `set_guest_info`로 정한 VM별 게스트 정보. 다시 켜도 남는다.
    guest_infos: HashMap<Uuid, GuestInfo>,
    /// `guest_info` 호출(실패 포함)
    guest_info_calls: Vec<Uuid>,
    /// `set_user_password` 호출의 (VM, 사용자). 비밀번호는 남기지 않는다.
    password_sets: Vec<(Uuid, String)>,
    /// `stop(Acpi|Agent)`가 실제로 보낸 종료 요청(에이전트가 답하지 않으면 `Acpi`)
    stop_requests: Vec<(Uuid, StopMode)>,
    /// 얼리기가 이 이유로 실패하는 VM(`fail_freeze`)
    freeze_failures: HashMap<Uuid, String>,
    /// 시점을 찍는 호출의 (VM, `quiesce`)(실패 포함)
    quiesce_calls: Vec<(Uuid, bool)>,
    /// `set_firmware_caps`로 정한 능력(없으면 TPM·Secure Boot 모두)
    firmware_caps: Option<FirmwareCaps>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
            save_reached: Notify::new(),
            start_reached: Notify::new(),
            start_released: Notify::new(),
            mirror_reached: Notify::new(),
            mirror_released: Notify::new(),
            incoming_wait_reached: Notify::new(),
            incoming_wait_released: Notify::new(),
            hotplug_reached: Notify::new(),
            hotplug_released: Notify::new(),
            replicate_reached: Notify::new(),
            replicate_released: Notify::new(),
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다. 이미 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 이후의 `start`를 차례로 이 오류들로 실패시킨다(한 번에 하나씩). `fail_next_start`처럼 이미
    /// 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_starts(&self, errs: impl IntoIterator<Item = DriverError>) {
        self.state.lock().unwrap().fail_starts.extend(errs);
    }

    /// 이후의 `start`가 `delay`만큼 기다린 뒤에 동작하게 한다. 시작 도중에 일어나는 일을 흉내 낸다.
    pub fn delay_starts(&self, delay: Duration) {
        self.state.lock().unwrap().start_delay = Some(delay);
    }

    /// 다음 `start`(또는 `start_incoming`) 한 번을 `release_start`까지 멈춘다. 시작 도중에 일어나는
    /// 일을 시간에 기대지 않고 흉내 낸다.
    pub fn hold_next_start(&self) {
        self.state.lock().unwrap().hold_next_start = true;
    }

    /// 멈춘 `start`가 들어올 때까지 기다린다.
    pub async fn start_held(&self) {
        self.start_reached.notified().await;
    }

    /// 멈춘 `start`를 놓아준다.
    pub fn release_start(&self) {
        self.start_released.notify_one();
    }

    /// 다음 `snapshot_save`나 `snapshot_delete` 한 번을 이 오류로 실패시킨다. `snapshot_save`는
    /// 태그를 남긴 채 실패한다(일부 디스크에 만들었거나, 시간 초과로 작업이 아직 돌고 있는 것처럼).
    pub fn fail_next_snapshot(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_snapshot = Some(err);
    }

    /// 다음 `snapshot_save` 한 번이 VM이 꺼질 때까지 끝나지 않게 한다. 실제 QEMU의 메모리 저장처럼
    /// 그동안 VM을 강제로 끄면 QMP가 끊긴 오류로 실패하고 태그를 남기지 않는다.
    pub fn hang_next_snapshot_save(&self) {
        self.state.lock().unwrap().hang_next_save = true;
    }

    /// 멈춘 `snapshot_save`가 들어올 때까지 기다린다.
    pub async fn snapshot_save_reached(&self) {
        self.save_reached.notified().await;
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다. `stop(Acpi)` 요청에 게스트가 응한 경우도 이것으로 흉내 낸다.
    pub fn guest_shutdown(&self, id: Uuid) {
        self.remove(
            id,
            EventKind::Stopped {
                guest_initiated: true,
            },
        );
    }

    /// 도는 VM의 게스트가 다시 부팅하거나(`Reset`) 멈췄다 다시 돈 것(`PauseChanged`)처럼 이벤트만 보낸다(VM은
    /// 그대로 돈다).
    pub fn guest_event(&self, id: Uuid, kind: EventKind) {
        assert!(kind.is_guest(), "{kind:?} is not a guest event");
        let _ = self.events.send(HypervisorEvent { id, kind });
    }

    /// 프로세스가 비정상 종료한 것처럼 만든다.
    pub fn crash(&self, id: Uuid) {
        self.remove(id, EventKind::Crashed);
    }

    /// 실행 중인 VM을 살아 있지만 QMP로 다룰 수 없는 VM으로 만들거나(true) 되돌린다(false).
    /// 실제 드라이버처럼 조회에는 `unresponsive`로 드러나고, `start`와 `stop(Acpi)`는 다시 붙지
    /// 못해 실패하며, `stop(Force)`는 프로세스를 죽이므로 종료 이벤트 없이 사라진다.
    pub fn set_unresponsive(&self, id: Uuid, unresponsive: bool) {
        let mut st = self.state.lock().unwrap();
        if unresponsive {
            st.unresponsive.insert(id);
        } else {
            st.unresponsive.remove(&id);
        }
    }

    /// VM을 SIGKILL에도 끝나지 않는(I/O에 묶인 D 상태) VM으로 만들거나(true) 되돌린다(false).
    /// `stop(Kill)`이 실제 드라이버처럼 `Timeout`으로 실패하고 VM은 그대로 돈다.
    pub fn set_unkillable(&self, id: Uuid, unkillable: bool) {
        let mut st = self.state.lock().unwrap();
        if unkillable {
            st.unkillable.insert(id);
        } else {
            st.unkillable.remove(&id);
        }
    }

    /// `console_endpoint`가 `{dir}/{id}.vnc`를 돌려주게 한다. 테스트가 그 경로에 가짜 VNC 서버를 띄운다.
    pub fn set_console_dir(&self, dir: impl Into<PathBuf>) {
        self.state.lock().unwrap().console_dir = Some(dir.into());
    }

    /// 새로 띄우려 한 VM과 `start`에 준 `restore` 태그를 순서대로 돌려준다. 이미 실행 중이어서
    /// 띄우지 않은 `start`는 빠지고, 주입한 오류로 실패한 시도는 들어간다.
    pub fn launches(&self) -> Vec<(Uuid, Option<String>)> {
        self.state.lock().unwrap().launches.clone()
    }

    /// 실행 중에 `snapshot_save`로 만들고 지우지 않은 스냅샷 (태그, 메모리 포함). VM이 꺼져도 남는다.
    pub fn snapshots(&self, id: Uuid) -> Vec<(String, bool)> {
        let st = self.state.lock().unwrap();
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }

    /// 실행 중인 VM의 `disk_stats`가 돌려줄 카운터를 정한다(정하지 않으면 0).
    pub fn set_disk_stats(&self, id: Uuid, counters: DiskCounters) {
        self.state.lock().unwrap().disk_stats.insert(id, counters);
    }

    /// 실행 중인 VM의 `disk_stats`만 시간 초과로 실패하게 하거나(true) 되돌린다(false). 다른 메서드와
    /// `list()`의 상태는 그대로다(QMP가 `query-blockstats`에만 늦게 답한 것처럼).
    pub fn fail_disk_stats(&self, id: Uuid, fail: bool) {
        let mut st = self.state.lock().unwrap();
        if fail {
            st.failing_disk_stats.insert(id);
        } else {
            st.failing_disk_stats.remove(&id);
        }
    }

    /// 이후 `list`가 이만큼 기다린 뒤에 답한다(응답 없는 VM 때문에 조회가 늦은 것처럼).
    pub fn delay_lists(&self, delay: Duration) {
        self.state.lock().unwrap().list_delay = Some(delay);
    }

    /// 이후 `disk_stats`가 이만큼 기다린 뒤에 답한다(QMP가 늦게 답한 것처럼).
    pub fn delay_disk_stats(&self, delay: Duration) {
        self.state.lock().unwrap().disk_stats_delay = Some(delay);
    }

    /// 이후 이 VM의 `guest_ping`이 `answer`대로 답한다(다시 켜도 그대로). 정하지 않으면 채널을 달고 띄운
    /// VM은 `Hang`(게스트 OS 없음), 아니면 `None`이다.
    pub fn set_guest_agent(&self, id: Uuid, answer: GuestAnswer) {
        self.state.lock().unwrap().guest_answers.insert(id, answer);
    }

    /// 이 VM에 부른 `guest_ping` 수(실패 포함)
    pub fn guest_pings(&self, id: Uuid) -> usize {
        let st = self.state.lock().unwrap();
        st.guest_pings.iter().filter(|vm| **vm == id).count()
    }

    /// 이후 이 VM의 `guest_info`가 이 정보를 돌려준다(에이전트가 `Ok`로 답할 때, 다시 켜도 그대로).
    pub fn set_guest_info(&self, id: Uuid, info: GuestInfo) {
        self.state.lock().unwrap().guest_infos.insert(id, info);
    }

    /// 이 VM에 부른 `guest_info` 수(실패 포함)
    pub fn guest_info_calls(&self, id: Uuid) -> usize {
        let st = self.state.lock().unwrap();
        st.guest_info_calls.iter().filter(|vm| **vm == id).count()
    }

    /// 이 VM에 성공한 `set_user_password`의 사용자들(비밀번호는 남기지 않는다)
    pub fn password_sets(&self, id: Uuid) -> Vec<String> {
        let st = self.state.lock().unwrap();
        st.password_sets
            .iter()
            .filter(|(vm, _)| *vm == id)
            .map(|(_, user)| user.clone())
            .collect()
    }

    /// 이 VM에 실제로 보낸 종료 요청(`Acpi`·`Agent`, 에이전트가 답하지 않아 ACPI로 넘어간 것은 `Acpi`)
    pub fn stop_requests(&self, id: Uuid) -> Vec<StopMode> {
        let st = self.state.lock().unwrap();
        st.stop_requests
            .iter()
            .filter(|(vm, _)| *vm == id)
            .map(|(_, mode)| *mode)
            .collect()
    }

    /// 이후 이 VM의 파일 시스템 얼리기가 이 이유로 실패한다(None이면 되돌린다).
    pub fn fail_freeze(&self, id: Uuid, reason: Option<&str>) {
        let mut st = self.state.lock().unwrap();
        match reason {
            Some(r) => st.freeze_failures.insert(id, r.to_string()),
            None => st.freeze_failures.remove(&id),
        };
    }

    /// 이 VM에 시점을 찍는 호출(스냅샷·증분 백업·복제 회차)이 넘긴 `quiesce`들(실패 포함)
    pub fn quiesce_calls(&self, id: Uuid) -> Vec<bool> {
        let st = self.state.lock().unwrap();
        st.quiesce_calls
            .iter()
            .filter(|(vm, _)| *vm == id)
            .map(|(_, q)| *q)
            .collect()
    }

    /// 이후 `firmware_caps`가 이 능력을 돌려준다.
    pub fn set_firmware_caps(&self, caps: FirmwareCaps) {
        self.state.lock().unwrap().firmware_caps = Some(caps);
    }

    /// 에이전트가 답하는지: 정한 답, 없으면 채널을 달고 띄운 VM은 `Hang`, 아니면 `None`
    fn agent_answer(st: &MockState, id: Uuid) -> GuestAnswer {
        let channel = st.guest_channels.contains(&id);
        st.guest_answers.get(&id).copied().unwrap_or(if channel {
            GuestAnswer::Hang
        } else {
            GuestAnswer::None
        })
    }

    /// 에이전트 세션을 열 수 있는지(실제 드라이버의 채널 확인과 동기화)
    fn agent_session(st: &MockState, id: Uuid) -> Result<(), DriverError> {
        match Self::agent_answer(st, id) {
            GuestAnswer::None => Err(DriverError::InvalidSpec(format!(
                "VM {id} has no guest agent channel"
            ))),
            GuestAnswer::Hang => Err(DriverError::GuestAgent(
                "the guest agent did not answer guest-sync-delimited in time".into(),
            )),
            GuestAnswer::Ok => Ok(()),
        }
    }

    /// 시점을 찍는 호출의 얼림 결과: 에이전트가 답하고 주입한 실패가 없으면 `Yes`다.
    fn quiesce(st: &mut MockState, id: Uuid, quiesce: bool) -> Quiesced {
        st.quiesce_calls.push((id, quiesce));
        if !quiesce {
            return Quiesced::No;
        }
        if let Some(why) = st.freeze_failures.get(&id) {
            return Quiesced::Skipped(why.clone());
        }
        match Self::agent_session(st, id) {
            Ok(()) => Quiesced::Yes,
            Err(DriverError::InvalidSpec(_)) => {
                Quiesced::Skipped("the VM has no guest agent channel".into())
            }
            Err(e) => Quiesced::Skipped(e.to_string()),
        }
    }

    /// `block_resize`로 마지막에 바꾼 크기
    pub fn disk_size(&self, id: Uuid, disk: usize) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .disk_sizes
            .get(&(id, disk))
            .copied()
    }

    /// `pause`로 멈추고 아직 `resume`하지 않았는지
    pub fn is_paused(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().paused.contains(&id)
    }

    /// 이전 opld가 `pause`한 채 죽은 것처럼 VM을 표시와 함께 멈춘다. `resume_left_stopped`가
    /// 이어서 실행한다(지금 `pause`해 둔 VM은 표시가 없는 것으로 본다).
    pub fn leave_paused(&self, id: Uuid) {
        let mut st = self.state.lock().unwrap();
        st.paused.insert(id);
        st.marked.insert(id);
    }

    /// VM에 부른 `pause`·`resume`(실패한 호출 포함)을 순서대로 돌려준다.
    pub fn pause_calls(&self, id: Uuid) -> Vec<&'static str> {
        let st = self.state.lock().unwrap();
        st.pauses
            .iter()
            .filter(|(vm, _)| *vm == id)
            .map(|(_, call)| *call)
            .collect()
    }

    /// 조회(`inspect`·`list`)가 VM의 상태를 `status`로 알리게 한다(`None`이면 되돌린다). QEMU가 스스로
    /// 멈춘 상태(`io-error` 등)를 흉내 낸다. QMP로 다룰 수 없는 VM은 그대로 `unresponsive`다.
    pub fn set_status(&self, id: Uuid, status: Option<RuntimeStatus>) {
        let mut st = self.state.lock().unwrap();
        match status {
            Some(status) => st.statuses.insert(id, status),
            None => st.statuses.remove(&id),
        };
    }

    /// `qemu_version`·`running_qemu_version`이 돌려줄 버전(기본 [`MOCK_QEMU_VERSION`])
    pub fn set_qemu_version(&self, version: &str) {
        self.state.lock().unwrap().qemu_version = Some(version.to_string());
    }

    /// 다음 `migrate` 한 번을 이 오류로 실패시킨다. 원본은 그대로 돈다.
    pub fn fail_next_migrate(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_migrate = Some(err);
    }

    /// 다음 `migrate` 한 번을 이 오류로 실패시킨다. QMP 연결이 끊긴 것처럼 마이그레이션은 보내는
    /// 중으로 남는다([`complete_outgoing`](Self::complete_outgoing)이 끝낸다).
    pub fn fail_next_migrate_unfinished(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_migrate_unfinished = Some(err);
    }

    /// 이후 `migrate`가 이만큼 기다린 뒤에 동작한다(보내는 동안 다른 일이 끼어드는 시험).
    pub fn delay_migrations(&self, delay: Duration) {
        self.state.lock().unwrap().migrate_delay = Some(delay);
    }

    /// 도는 VM이 보내는 중인 마이그레이션을 남긴다(opld가 보내다 죽었다).
    pub fn start_outgoing(&self, id: Uuid) {
        self.state.lock().unwrap().outgoing.insert(id);
    }

    /// 보내는 중인 마이그레이션이 있는지
    pub fn is_outgoing(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().outgoing.contains(&id)
    }

    /// 보내는 중인 마이그레이션이 있으면 끝낸다(원본은 `postmigrate`로 멈춘다).
    pub fn complete_outgoing(&self, id: Uuid) {
        let mut st = self.state.lock().unwrap();
        if st.outgoing.remove(&id) {
            st.migration_status.insert(id, postmigrate());
        }
    }

    /// 이후 `cancel_migration`은 취소하는 사이에 마이그레이션이 끝난 것으로 한다.
    pub fn complete_on_cancel(&self) {
        self.state.lock().unwrap().complete_on_cancel = true;
    }

    /// 다음 `incoming_wait` 한 번을 이 오류로 실패시킨다. 받는 QEMU는 크래시로 끝난다.
    pub fn fail_next_incoming_wait(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_incoming = Some(err);
    }

    /// `migrate` 호출(실패 포함)을 순서대로 돌려준다.
    pub fn migrations(&self) -> Vec<(Uuid, MigrateTarget)> {
        self.state.lock().unwrap().migrations.clone()
    }

    /// 받는 중 표시가 있는지
    pub fn is_incoming(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().incoming.contains_key(&id)
    }

    /// VM의 디스크마다의 dirty bitmap(꺼져 있어도 남은 것을 본다)
    pub fn bitmaps(&self, id: Uuid) -> Vec<Vec<BitmapInfo>> {
        let st = self.state.lock().unwrap();
        st.bitmaps.get(&id).cloned().unwrap_or_default()
    }

    /// VM의 디스크마다의 dirty bitmap을 바꾼다(없는 비트맵, 남은 비트맵 등).
    pub fn set_bitmaps(&self, id: Uuid, bitmaps: Vec<Vec<BitmapInfo>>) {
        self.state.lock().unwrap().bitmaps.insert(id, bitmaps);
    }

    /// `disk`번째 디스크의 비트맵 `name`을 QEMU가 비정상으로 끝난 뒤 연 것처럼 inconsistent로 만든다.
    pub fn set_bitmap_inconsistent(&self, id: Uuid, disk: usize, name: &str) {
        let mut st = self.state.lock().unwrap();
        let bitmaps = st.bitmaps.get_mut(&id).and_then(|d| d.get_mut(disk));
        for b in bitmaps.into_iter().flatten().filter(|b| b.name == name) {
            b.inconsistent = true;
        }
    }

    /// 다음 `incremental_backup` 한 번을 이 오류로 실패시킨다. 비트맵은 그대로 남는다.
    pub fn fail_next_incremental(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_incremental = Some(err);
    }

    /// `incremental_backup` 호출(실패 포함)을 순서대로 돌려준다. 대상 파일에는 아무것도 쓰지 않는다.
    pub fn incremental_backups(&self) -> Vec<IncrementalCall> {
        self.state.lock().unwrap().incrementals.clone()
    }

    /// `replicate_disks` 호출(실패 포함)을 순서대로 돌려준다.
    pub fn replications(&self) -> Vec<ReplicationCall> {
        self.state.lock().unwrap().replications.clone()
    }

    /// 다음 `replicate_disks` 한 번을 이 문구의 복제 오류로 실패시킨다. 실제 드라이버처럼 새 비트맵은 남지 않고
    /// 기준 비트맵은 그대로다.
    pub fn fail_next_replicate(&self, msg: &str) {
        self.state.lock().unwrap().fail_next_replicate = Some(DriverError::Replication(msg.into()));
    }

    /// 다음 `replicate_disks` 한 번을 들어온 곳에서 멈춘다(`replicate_held`로 기다리고 `release_replicate`로
    /// 놓는다).
    pub fn hold_next_replicate(&self) {
        self.state.lock().unwrap().hold_next_replicate = true;
    }

    /// 멈춘 `replicate_disks`가 들어올 때까지 기다린다.
    pub async fn replicate_held(&self) {
        self.replicate_reached.notified().await;
    }

    /// 멈춘 `replicate_disks`를 놓아준다.
    pub fn release_replicate(&self) {
        self.replicate_released.notify_one();
    }

    /// 살아 있는 받는 QEMU의 회차와 그 내보내기
    pub fn receivers(&self) -> BTreeMap<Uuid, Vec<ReceiverExport>> {
        self.state.lock().unwrap().receivers.clone()
    }

    /// 다음 `start_replica_receiver` 한 번을 이 오류로 실패시킨다.
    pub fn fail_next_receiver_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_receiver_start = Some(err);
    }

    /// 다음 `finish_replica_receiver` 한 번을 이 오류로 실패시킨다. 받는 QEMU는 남는다.
    pub fn fail_next_receiver_finish(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_receiver_finish = Some(err);
    }

    /// `catch_up_disks` 호출(실패 포함)을 순서대로 돌려준다.
    pub fn catch_ups(&self) -> Vec<CatchUpCall> {
        self.state.lock().unwrap().catch_ups.clone()
    }

    /// 다음 `catch_up_disks` 한 번을 이 오류로 실패시킨다.
    pub fn fail_next_catch_up(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_catch_up = Some(err);
    }

    /// 이후 `catch_up_disks`가 이만큼의 패스를 한 것으로 한다(요청의 최댓값까지).
    pub fn set_catch_up_passes(&self, passes: u8) {
        self.state.lock().unwrap().catch_up_passes = Some(passes);
    }

    /// 따라잡기를 마치고 스위치오버를 기다리는 VM의 상태
    pub fn catching_up(&self, id: Uuid) -> Option<CatchUpState> {
        self.state.lock().unwrap().catching_up.get(&id).cloned()
    }

    /// `clear_backup_leftovers`를 부른 VM(실패 포함)을 순서대로 돌려준다.
    pub fn leftover_clears(&self) -> Vec<Uuid> {
        self.state.lock().unwrap().leftover_clears.clone()
    }

    /// 다음 `mirror_disks` 한 번을 이 오류로 실패시킨다. 미러는 남지 않는다.
    pub fn fail_next_mirror(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_mirror = Some(err);
    }

    /// 다음 `mirror_disks` 한 번이 진행 없이 정체로 실패한다(`opts.stall`을 기다리지 않고 곧바로).
    pub fn stall_next_mirror(&self) {
        self.state.lock().unwrap().stall_next_mirror = true;
    }

    /// 다음 `switchover_mirrors` 마이그레이션 한 번에서 미러가 `error`로 끝난다. 실제 드라이버처럼
    /// 마이그레이션을 취소하고 미러를 치운 뒤 실패한다(원본은 계속 돈다).
    pub fn fail_next_switchover(&self, error: &str) {
        self.state.lock().unwrap().fail_next_switchover = Some(error.to_string());
    }

    /// 다음 `mirror_disks` 한 번이 미러를 시작하고(`mirroring`에 보인다) 진행 없이 `release_mirror`까지
    /// 멈춘다(복사 중에 일어나는 일).
    pub fn hold_next_mirror(&self) {
        self.state.lock().unwrap().hold_next_mirror = true;
    }

    /// 멈춘 `mirror_disks`가 들어올 때까지 기다린다.
    pub async fn mirror_held(&self) {
        self.mirror_reached.notified().await;
    }

    /// 멈춘 `mirror_disks`를 놓아준다.
    pub fn release_mirror(&self) {
        self.mirror_released.notify_one();
    }

    /// 다음 `incoming_wait` 한 번이 받기를 마친 뒤(멈춘 채 받은 QEMU) `release_incoming_wait`까지
    /// 멈춘다(받기 확인과 확정 사이에 일어나는 일).
    pub fn hold_next_incoming_wait(&self) {
        self.state.lock().unwrap().hold_next_incoming_wait = true;
    }

    /// 멈춘 `incoming_wait`가 들어올 때까지 기다린다.
    pub async fn incoming_wait_held(&self) {
        self.incoming_wait_reached.notified().await;
    }

    /// 멈춘 `incoming_wait`를 놓아준다.
    pub fn release_incoming_wait(&self) {
        self.incoming_wait_released.notify_one();
    }

    /// 다음 `cancel_migration` 한 번을 이 오류로 실패시킨다(아무것도 치우지 않는다). 여러 번 부르면
    /// 그 수만큼 차례로 실패한다.
    pub fn fail_next_cancel(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_cancel.push_back(err);
    }

    /// `mirror_disks` 호출(실패 포함)을 순서대로 돌려준다. 디스크 내용은 복사하지 않는다.
    pub fn mirrors(&self) -> Vec<(Uuid, Vec<MirrorTarget>)> {
        self.state.lock().unwrap().mirror_calls.clone()
    }

    /// ready인 미러가 남은 디스크(없으면 빈 목록)
    pub fn mirroring(&self, id: Uuid) -> Vec<usize> {
        let st = self.state.lock().unwrap();
        st.mirroring.get(&id).cloned().unwrap_or_default()
    }

    /// 스위치오버에서 미러를 끝내고 성공한 마이그레이션의 VM을 순서대로 돌려준다.
    pub fn switchovers(&self) -> Vec<Uuid> {
        self.state.lock().unwrap().switchovers.clone()
    }

    /// 받는 QEMU가 NBD로 내보내는 디스크(없으면 빈 목록)
    pub fn exports(&self, id: Uuid) -> Vec<usize> {
        let st = self.state.lock().unwrap();
        st.exports.get(&id).cloned().unwrap_or_default()
    }

    /// 새로 띄우려 한 VM의 런타임 스펙(`launches`와 같은 순서, 주입한 오류로 실패한 시도 포함)
    pub fn launch_specs(&self) -> Vec<VmRuntimeSpec> {
        self.state.lock().unwrap().launch_specs.clone()
    }

    /// `start_incoming`으로 받으려 한 VM의 런타임 스펙(실패한 시도 포함)
    pub fn incoming_specs(&self) -> Vec<VmRuntimeSpec> {
        self.state.lock().unwrap().incoming_specs.clone()
    }

    /// 다음 `set_vcpus` 한 번을 들어온 곳에서 멈춘다(`hotplug_reached`로 기다리고 `release_hotplug`로 놓는다). 그
    /// 사이 VM을 다시 켜는 일을 흉내 낸다.
    pub fn hold_next_hotplug(&self) {
        self.state.lock().unwrap().hold_next_hotplug = true;
    }

    /// 멈춘 `set_vcpus`가 들어올 때까지 기다린다.
    pub async fn hotplug_reached(&self) {
        self.hotplug_reached.notified().await;
    }

    /// 멈춘 `set_vcpus`를 놓아준다.
    pub fn release_hotplug(&self) {
        self.hotplug_released.notify_one();
    }

    /// `hotplug_caps`가 돌려줄 능력을 정한다(기본은 vCPU·메모리 모두).
    pub fn set_hotplug_caps(&self, caps: NodeHotplug) {
        self.state.lock().unwrap().hotplug_caps = Some(caps);
    }

    /// `save_hardware`로 남긴 기록을 지운다(기록 파일 없이 도는 VM — 계획 54 전에 켬).
    pub fn forget_saved_hardware(&self, id: Uuid) {
        self.state.lock().unwrap().saved_hardware.remove(&id);
    }

    /// 이 VM의 QEMU가 vCPU 떼기를 거절하게 하거나(true, 게스트가 vCPU 떼기를 켜지 않은 q35) 되돌린다(false).
    pub fn refuse_vcpu_unplug(&self, id: Uuid, refuse: bool) {
        let mut st = self.state.lock().unwrap();
        if refuse {
            st.refuse_vcpu_unplug.insert(id);
        } else {
            st.refuse_vcpu_unplug.remove(&id);
        }
    }

    /// 이 VM의 게스트가 떼기·줄이기를 내려 주지 않게 하거나(true) 되돌린다(false). `set_vcpus`·`set_memory`의
    /// 줄이기는 바꾸지 않은 값을, `remove_disk`·`remove_nic`은 false를 돌려준다(장치는 그대로).
    pub fn hold_unplug(&self, id: Uuid, hold: bool) {
        let mut st = self.state.lock().unwrap();
        if hold {
            st.hold_unplug.insert(id);
        } else {
            st.hold_unplug.remove(&id);
        }
    }

    /// 다음 핫플러그 호출 한 번을 이 문구의 QMP 오류(`GenericError`)로 실패시킨다. 실제 드라이버가 붙이다 실패한
    /// 장치를 되돌리듯 아무것도 바꾸지 않는다.
    pub fn fail_next_hotplug(&self, msg: &str) {
        self.state.lock().unwrap().fail_next_hotplug = Some(msg.to_string());
    }

    /// 핫플러그 호출(실패 포함)을 순서대로 돌려준다.
    pub fn hotplug_calls(&self) -> Vec<HotplugCall> {
        self.state.lock().unwrap().hotplug_calls.clone()
    }

    /// 실행 중인 VM의 켜진 vCPU 수(꺼져 있으면 None)
    pub fn vcpus(&self, id: Uuid) -> Option<u16> {
        self.state.lock().unwrap().hot.get(&id).map(|h| h.vcpus)
    }

    /// 실행 중인 VM의 virtio-mem에 꽂은 크기(MiB, 꺼져 있으면 None)
    pub fn plugged_mib(&self, id: Uuid) -> Option<u32> {
        self.state
            .lock()
            .unwrap()
            .hot
            .get(&id)
            .map(|h| h.plugged_mib)
    }

    /// 실행 중인 VM의 CD-ROM 드라이브마다 넣은 매체 파일(빈 드라이브는 None). 꺼져 있으면 비어 있다.
    pub fn media(&self, id: Uuid) -> Vec<Option<PathBuf>> {
        let st = self.state.lock().unwrap();
        st.hot.get(&id).map(|h| h.media.clone()).unwrap_or_default()
    }

    /// 실행 중인 VM에 켤 때 넣은 cloud-init 씨앗(없거나 꺼져 있으면 None)
    pub fn seed(&self, id: Uuid) -> Option<CloudInitSeed> {
        let st = self.state.lock().unwrap();
        st.hot.get(&id).and_then(|h| h.seed.clone())
    }

    /// 실행 중인 VM의 장치 id(`disk{i}-dev`·`nic{i}`·`cd{i}-dev`·`cidata-dev`, 이름 순). 꺼져 있으면 비어 있다.
    pub fn devices(&self, id: Uuid) -> Vec<String> {
        let st = self.state.lock().unwrap();
        st.hot
            .get(&id)
            .map(|h| h.devices.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 핫플러그 호출을 기록하고, 실행 중인 VM이 아니거나 주입한 실패가 있으면 오류다.
    fn hotplug_call(st: &mut MockState, call: HotplugCall) -> Result<(), DriverError> {
        let id = match call {
            HotplugCall::SetVcpus { id, .. }
            | HotplugCall::SetMemory { id, .. }
            | HotplugCall::AddDisk { id, .. }
            | HotplugCall::RemoveDisk { id, .. }
            | HotplugCall::AddNic { id, .. }
            | HotplugCall::RemoveNic { id, .. }
            | HotplugCall::ChangeMedium { id, .. } => id,
        };
        st.hotplug_calls.push(call);
        Self::controllable(st, id)?;
        match st.fail_next_hotplug.take() {
            Some(desc) => Err(qmp_error("GenericError", desc)),
            None => Ok(()),
        }
    }

    /// 도는 VM에 장치 `dev`가 있는지
    fn present(st: &MockState, id: Uuid, dev: &str) -> bool {
        st.hot.get(&id).is_some_and(|h| h.devices.contains_key(dev))
    }

    /// 실제 QEMU처럼 장치 `dev`를 빈 루트 포트 `hp{port}`에 붙인다.
    fn plug(st: &mut MockState, id: Uuid, dev: String, port: u8) -> Result<(), DriverError> {
        let hot = st.hot.entry(id).or_default();
        if port >= hot.ports {
            return Err(qmp_error(
                "GenericError",
                format!("Bus 'hp{port}' not found"),
            ));
        }
        if hot.devices.contains_key(&dev) {
            return Err(qmp_error(
                "GenericError",
                format!("Duplicate device ID '{dev}'"),
            ));
        }
        if hot.devices.values().any(|p| *p == Some(port)) {
            return Err(qmp_error(
                "GenericError",
                format!("PCI: slot 0 function 0 not available for hp{port}"),
            ));
        }
        hot.devices.insert(dev, Some(port));
        Ok(())
    }

    /// 실제 QEMU처럼 장치 `dev`를 뗀다. 없으면 뗀 것이고(true), 게스트가 내려 주지 않으면 false다. 켤 때 단 장치는
    /// `pcie.0`에 있어 뗄 수 없다.
    fn unplug(st: &mut MockState, id: Uuid, dev: &str) -> Result<bool, DriverError> {
        let held = st.hold_unplug.contains(&id);
        let hot = st.hot.entry(id).or_default();
        match hot.devices.get(dev) {
            None => Ok(true),
            Some(None) => Err(qmp_error(
                "GenericError",
                "Bus 'pcie.0' does not support hotplugging",
            )),
            Some(Some(_)) if held => Ok(false),
            Some(Some(_)) => {
                hot.devices.remove(dev);
                Ok(true)
            }
        }
    }

    /// 새로 띄운 VM의 디스크 형식을 적고, 디스크 수에 맞춰 비트맵 목록을 늘리거나 줄인다.
    fn launched(st: &mut MockState, vm: &VmRuntimeSpec) {
        let layout = vm.hotplug.as_ref();
        // 켤 때 단 장치는 `pcie.0`, 배치의 순번 뒤 장치는 그 루트 포트에 있다.
        let port = |boot: Option<u8>, ports: Option<&Vec<u8>>, i: usize| {
            let boot = usize::from(boot?);
            ports?.get(i.checked_sub(boot)?).copied()
        };
        let mut devices = BTreeMap::new();
        for i in 0..vm.disks.len() {
            let p = port(
                layout.map(|l| l.boot_disks),
                layout.map(|l| &l.disk_ports),
                i,
            );
            devices.insert(format!("disk{i}-dev"), p);
        }
        for i in 0..vm.nics.len() {
            let p = port(layout.map(|l| l.boot_nics), layout.map(|l| &l.nic_ports), i);
            devices.insert(format!("nic{i}"), p);
        }
        // CD-ROM·씨앗 드라이브는 컨트롤러 포트에 있어 뗄 수 없다.
        for i in 0..vm.cdroms.len() {
            devices.insert(format!("cd{i}-dev"), None);
        }
        if vm.cloud_init.is_some() {
            devices.insert("cidata-dev".into(), None);
        }
        let boot_vcpus = u16::try_from(vm.cpu.vcpus()).unwrap_or(u16::MAX);
        st.hot.insert(
            vm.id,
            HotState {
                vcpus: layout.map_or(boot_vcpus, |l| l.vcpus),
                plugged_mib: layout.map_or(0, |l| l.plugged_mib),
                vmem_mib: layout.map_or(0, |l| l.max_memory_mib.saturating_sub(vm.memory_mib)),
                ports: layout.map_or(0, |l| l.ports),
                devices,
                media: vm
                    .cdroms
                    .iter()
                    .map(|c| c.medium.as_ref().map(|m| m.path.clone()))
                    .collect(),
                seed: vm.cloud_init.clone(),
            },
        );
        let formats: Vec<ImageFormat> = vm.disks.iter().map(|d| d.format).collect();
        st.bitmaps
            .entry(vm.id)
            .or_default()
            .resize(formats.len(), Vec::new());
        st.disk_formats.insert(vm.id, formats);
        if vm.guest_agent {
            st.guest_channels.insert(vm.id);
        } else {
            st.guest_channels.remove(&vm.id);
        }
    }

    /// VM이 꺼지면 이미지에 저장되지 않은 비트맵은 사라진다.
    fn forget_volatile_bitmaps(st: &mut MockState, id: Uuid) {
        for disk in st.bitmaps.get_mut(&id).into_iter().flatten() {
            disk.retain(|b| b.persistent);
        }
    }

    /// 모든 디스크에 비트맵 `name`을 만든다. 하나라도 이미 있으면 아무것도 만들지 않는다(QEMU의
    /// transaction처럼).
    fn add_bitmaps_to(st: &mut MockState, id: Uuid, name: &str) -> Result<(), DriverError> {
        let formats = st.disk_formats.get(&id).cloned().unwrap_or_default();
        let disks = st.bitmaps.entry(id).or_default();
        disks.resize(formats.len(), Vec::new());
        if disks.iter().flatten().any(|b| b.name == name) {
            return Err(DriverError::Qmp(QmpError::Command {
                class: "GenericError".into(),
                desc: format!("Bitmap already exists: {name}"),
            }));
        }
        for (disk, format) in disks.iter_mut().zip(formats) {
            disk.push(BitmapInfo {
                name: name.to_string(),
                recording: true,
                inconsistent: false,
                persistent: format == ImageFormat::Qcow2,
                count: 0,
            });
        }
        Ok(())
    }

    fn version(st: &MockState) -> String {
        st.qemu_version
            .clone()
            .unwrap_or_else(|| MOCK_QEMU_VERSION.to_string())
    }

    /// 실제 드라이버처럼 실행 중인 VM만 다룬다. 살아 있지만 QMP로 다룰 수 없으면 다시 시도할 오류다.
    fn controllable(st: &MockState, id: Uuid) -> Result<(), DriverError> {
        if !st.vms.contains_key(&id) {
            Err(DriverError::NotRunning(id))
        } else if st.unresponsive.contains(&id) {
            Err(DriverError::NotAttached(id))
        } else {
            Ok(())
        }
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        let mut st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Self::forget_volatile_bitmaps(&mut st, id);
        }
        st.unresponsive.remove(&id);
        st.paused.remove(&id);
        st.marked.remove(&id);
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        st.outgoing.remove(&id);
        st.exports.remove(&id);
        st.mirroring.remove(&id);
        st.catching_up.remove(&id);
        st.disk_stats.remove(&id);
        st.failing_disk_stats.remove(&id);
        st.guest_channels.remove(&id);
        st.hot.remove(&id);
        st.saved_hardware.remove(&id);
        if st.vms.remove(&id).is_some() {
            drop(st);
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(st: &MockState, id: Uuid, pid: u32) -> ActualState {
        let status = if st.unresponsive.contains(&id) {
            RuntimeStatus::Other("unresponsive".into())
        } else if let Some(status) = st.statuses.get(&id) {
            status.clone()
        } else if let Some(status) = st.migration_status.get(&id) {
            status.clone()
        } else if st.paused.contains(&id) {
            RuntimeStatus::Paused
        } else {
            RuntimeStatus::Running
        };
        ActualState { id, pid, status }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        let (delay, hold) = {
            let mut st = self.state.lock().unwrap();
            (st.start_delay, std::mem::take(&mut st.hold_next_start))
        };
        if hold {
            self.start_reached.notify_one();
            self.start_released.notified().await;
        }
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        {
            let mut st = self.state.lock().unwrap();
            // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 주입한 오류도 쓰지 않는다.
            if st.vms.contains_key(&vm.id) {
                if st.unresponsive.contains(&vm.id) {
                    return Err(DriverError::Timeout("reattach to running QEMU"));
                }
                return Ok(());
            }
            st.launches.push((vm.id, restore.map(str::to_string)));
            st.launch_specs.push(vm.clone());
            if let Some(err) = st
                .fail_next_start
                .take()
                .or_else(|| st.fail_starts.pop_front())
            {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
            Self::launched(&mut st, vm);
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if mode == StopMode::Kill && st.vms.contains_key(&id) && st.unkillable.contains(&id) {
                return Err(DriverError::Timeout("QEMU to exit"));
            }
            if st.vms.contains_key(&id) && st.unresponsive.contains(&id) {
                // 실제 드라이버는 다시 붙지 못한 VM에 ACPI 요청을 보낼 수 없고, 강제 종료는
                // 프로세스를 죽이므로 종료 이벤트가 없다.
                return match mode {
                    StopMode::Acpi | StopMode::Agent => {
                        Err(DriverError::Timeout("reattach to running QEMU"))
                    }
                    StopMode::Force | StopMode::Kill => {
                        Self::forget_volatile_bitmaps(&mut st, id);
                        st.unresponsive.remove(&id);
                        st.paused.remove(&id);
                        st.incoming.remove(&id);
                        st.migration_status.remove(&id);
                        st.outgoing.remove(&id);
                        st.guest_channels.remove(&id);
                        st.hot.remove(&id);
                        st.vms.remove(&id);
                        Ok(())
                    }
                };
            }
        }
        match mode {
            // 실제 QEMU처럼 요청만 보낸다. 게스트는 무시할 수 있으므로 실제 종료는
            // 테스트가 `guest_shutdown`으로 일으킨다. 에이전트가 답하지 않으면 ACPI로 넘어간다.
            StopMode::Acpi | StopMode::Agent => {
                let mut st = self.state.lock().unwrap();
                if st.vms.contains_key(&id) {
                    let sent = match mode {
                        StopMode::Agent if Self::agent_session(&st, id).is_ok() => StopMode::Agent,
                        _ => StopMode::Acpi,
                    };
                    st.stop_requests.push((id, sent));
                }
            }
            StopMode::Force => self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            ),
            // 실제 드라이버처럼 SIGKILL은 QMP 연결이 끊긴 크래시로 알려진다.
            StopMode::Kill => self.remove(id, EventKind::Crashed),
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(&st, id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let delay = self.state.lock().unwrap().list_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(&st, id, pid))
            .collect())
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        let st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Ok(match &st.console_dir {
                Some(dir) => dir.join(format!("{id}.vnc")),
                None => PathBuf::from(format!("/mock/{id}/vnc.sock")),
            })
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    async fn snapshot_save(
        &self,
        id: Uuid,
        tag: &str,
        memory: bool,
        quiesce: bool,
    ) -> Result<Quiesced, DriverError> {
        let hang = {
            let mut st = self.state.lock().unwrap();
            Self::controllable(&st, id)?;
            std::mem::take(&mut st.hang_next_save)
        };
        if hang {
            self.save_reached.notify_one();
            while self.state.lock().unwrap().vms.contains_key(&id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            return Err(DriverError::Qmp(QmpError::Disconnected));
        }
        let mut st = self.state.lock().unwrap();
        st.snapshots
            .entry(id)
            .or_default()
            .push((tag.to_string(), memory));
        let quiesced = Self::quiesce(&mut st, id, quiesce && !memory);
        match st.fail_next_snapshot.take() {
            Some(err) => Err(err),
            None => Ok(quiesced),
        }
    }

    /// `snapshot_save`처럼 태그를 남기고, 주입한 실패가 없으면 비트맵도 만든다.
    async fn snapshot_save_with_bitmap(
        &self,
        id: Uuid,
        tag: &str,
        bitmap: &str,
        quiesce: bool,
    ) -> Result<Quiesced, DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.snapshots
            .entry(id)
            .or_default()
            .push((tag.to_string(), false));
        let quiesced = Self::quiesce(&mut st, id, quiesce);
        if let Some(err) = st.fail_next_snapshot.take() {
            return Err(err);
        }
        Self::add_bitmaps_to(&mut st, id, bitmap).map(|()| quiesced)
    }

    async fn backup_bitmaps(&self, id: Uuid) -> Result<Vec<DiskBitmaps>, DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        let disks = st.disk_formats.get(&id).map_or(0, Vec::len);
        let bitmaps = st.bitmaps.get(&id);
        Ok((0..disks)
            .map(|i| DiskBitmaps {
                node: format!("disk{i}"),
                bitmaps: bitmaps.and_then(|b| b.get(i)).cloned().unwrap_or_default(),
            })
            .collect())
    }

    async fn add_bitmaps(&self, id: Uuid, name: &str) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Self::add_bitmaps_to(&mut st, id, name)
    }

    async fn remove_bitmaps(&self, id: Uuid, keep: Option<&str>) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        for disk in st.bitmaps.get_mut(&id).into_iter().flatten() {
            disk.retain(|b| !is_backup_bitmap(&b.name) || keep == Some(b.name.as_str()));
        }
        Ok(())
    }

    async fn remove_named_bitmaps(&self, id: Uuid, names: &[String]) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        for disk in st.bitmaps.get_mut(&id).into_iter().flatten() {
            disk.retain(|b| !names.contains(&b.name));
        }
        Ok(())
    }

    /// 실제 QEMU처럼 디스크마다 `parent`가 있고 inconsistent가 아니어야 한다. 성공하면 `new`를 만들고
    /// `parent`를 지운다. 대상 파일에는 쓰지 않는다.
    async fn incremental_backup(
        &self,
        id: Uuid,
        parent: &str,
        new: &str,
        targets: &[BackupTarget],
        quiesce: bool,
    ) -> Result<Quiesced, DriverError> {
        let mut st = self.state.lock().unwrap();
        st.incrementals
            .push((id, parent.to_string(), new.to_string(), targets.to_vec()));
        Self::controllable(&st, id)?;
        let quiesced = Self::quiesce(&mut st, id, quiesce);
        let disks = st.disk_formats.get(&id).map_or(0, Vec::len);
        let wanted: Vec<String> = (0..disks).map(|i| format!("disk{i}")).collect();
        let nodes: Vec<String> = targets.iter().map(|t| t.node.clone()).collect();
        if nodes != wanted {
            return Err(DriverError::InvalidSpec(format!(
                "backup targets {nodes:?} do not match the disks {wanted:?} of VM {id}"
            )));
        }
        let command = |desc: String| {
            DriverError::Qmp(QmpError::Command {
                class: "GenericError".into(),
                desc,
            })
        };
        for i in 0..disks {
            let found = st
                .bitmaps
                .get(&id)
                .and_then(|d| d.get(i))
                .and_then(|d| d.iter().find(|b| b.name == parent));
            match found {
                None => return Err(command(format!("Bitmap '{parent}' could not be found"))),
                Some(b) if b.inconsistent => {
                    return Err(command(format!(
                        "Bitmap '{parent}' is inconsistent and cannot be used"
                    )));
                }
                Some(_) => {}
            }
        }
        if let Some(err) = st.fail_next_incremental.take() {
            return Err(err);
        }
        Self::add_bitmaps_to(&mut st, id, new)?;
        for disk in st.bitmaps.get_mut(&id).into_iter().flatten() {
            disk.retain(|b| b.name != parent);
        }
        Ok(quiesced)
    }

    /// 남은 증분 백업 작업은 없다. 호출을 기록하고 남은 미러와 따라잡기 비트맵(`opl-mig-*`)을 치운다.
    async fn clear_backup_leftovers(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.leftover_clears.push(id);
        Self::controllable(&st, id)?;
        st.mirroring.remove(&id);
        st.catching_up.remove(&id);
        for disk in st.bitmaps.get_mut(&id).into_iter().flatten() {
            disk.retain(|b| !b.name.starts_with(MIGRATION_BITMAP_PREFIX));
        }
        Ok(())
    }

    /// 실제 QEMU처럼 증분 디스크에는 기준 비트맵이 있고 inconsistent가 아니어야 한다. 성공하면 대상 디스크마다 새
    /// 비트맵을 만들고(기준 비트맵은 그대로) 가짜 바이트를 알린다. 받는 쪽에는 쓰지 않는다.
    async fn replicate_disks(
        &self,
        id: Uuid,
        req: &ReplicateRequest,
    ) -> Result<(u64, Quiesced), DriverError> {
        let record = |st: &mut MockState, bytes| {
            st.replications.push(ReplicationCall {
                vm: id,
                bitmap: req.bitmap.clone(),
                targets: req.targets.clone(),
                bytes,
            });
        };
        let hold = {
            let mut st = self.state.lock().unwrap();
            if let Err(e) = Self::controllable(&st, id) {
                record(&mut st, None);
                return Err(e);
            }
            let disks = st.disk_formats.get(&id).map_or(0, Vec::len);
            let mut error = req
                .targets
                .iter()
                .find(|t| t.disk >= disks)
                .map(|t| DriverError::InvalidSpec(format!("VM {id} has no disk{}", t.disk)));
            let command = |desc: String| {
                DriverError::Qmp(QmpError::Command {
                    class: "GenericError".into(),
                    desc,
                })
            };
            for t in &req.targets {
                let disk = st.bitmaps.get(&id).and_then(|d| d.get(t.disk));
                if disk.is_some_and(|d| d.iter().any(|b| b.name == req.bitmap)) {
                    error.get_or_insert(command(format!("Bitmap already exists: {}", req.bitmap)));
                }
                let Some(base) = &t.base else { continue };
                let found = disk.and_then(|d| d.iter().find(|b| &b.name == base));
                match found {
                    None => {
                        error.get_or_insert(command(format!("Bitmap '{base}' could not be found")));
                    }
                    Some(b) if b.inconsistent => {
                        error.get_or_insert(command(format!(
                            "Bitmap '{base}' is inconsistent and cannot be used"
                        )));
                    }
                    Some(_) => {}
                }
            }
            if let Some(e) = error.or_else(|| st.fail_next_replicate.take()) {
                record(&mut st, None);
                return Err(e);
            }
            std::mem::take(&mut st.hold_next_replicate)
        };
        if hold {
            self.replicate_reached.notify_one();
            self.replicate_released.notified().await;
        }
        let mut st = self.state.lock().unwrap();
        if let Err(e) = Self::controllable(&st, id) {
            record(&mut st, None);
            return Err(e);
        }
        let formats = st.disk_formats.get(&id).cloned().unwrap_or_default();
        let disks = st.bitmaps.entry(id).or_default();
        disks.resize(formats.len(), Vec::new());
        for t in &req.targets {
            disks[t.disk].push(BitmapInfo {
                name: req.bitmap.clone(),
                recording: true,
                inconsistent: false,
                persistent: formats[t.disk] == ImageFormat::Qcow2,
                count: 0,
            });
        }
        let bytes = req
            .targets
            .iter()
            .map(|t| match t.base {
                Some(_) => MOCK_INCREMENT_BYTES,
                None => MOCK_MIRROR_BYTES,
            })
            .sum();
        if let Some(tx) = &req.progress {
            tx.send_replace(MirrorProgress {
                done_bytes: bytes,
                total_bytes: bytes,
            });
        }
        record(&mut st, Some(bytes));
        let quiesced = Self::quiesce(&mut st, id, req.quiesce);
        Ok((bytes, quiesced))
    }

    async fn start_replica_receiver(
        &self,
        run: Uuid,
        exports: &[ReceiverExport],
        _listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        let mut st = self.state.lock().unwrap();
        if let Some(err) = st.fail_next_receiver_start.take() {
            return Err(err);
        }
        if st.receivers.contains_key(&run) {
            return Err(DriverError::Replication(format!(
                "a replica receiver for run {run} is already running"
            )));
        }
        st.receivers.insert(run, exports.to_vec());
        Ok(*ports.start())
    }

    async fn finish_replica_receiver(&self, run: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        if !st.receivers.contains_key(&run) {
            return Err(DriverError::Replication(format!(
                "no replica receiver for run {run} on this node"
            )));
        }
        if let Some(err) = st.fail_next_receiver_finish.take() {
            return Err(err);
        }
        st.receivers.remove(&run);
        Ok(())
    }

    async fn stop_replica_receiver(&self, run: Uuid) -> Result<(), DriverError> {
        self.state.lock().unwrap().receivers.remove(&run);
        Ok(())
    }

    async fn replica_receivers(&self) -> Result<Vec<Uuid>, DriverError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .receivers
            .keys()
            .copied()
            .collect())
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_snapshot.take() {
            return Err(err);
        }
        if let Some(snapshots) = st.snapshots.get_mut(&id) {
            snapshots.retain(|(t, _)| t != tag);
        }
        Ok(())
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.disk_sizes.insert((id, disk), size_bytes);
        Ok(())
    }

    async fn pause(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "pause"));
        Self::controllable(&st, id)?;
        st.paused.insert(id);
        Ok(())
    }

    async fn resume(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "resume"));
        Self::controllable(&st, id)?;
        st.paused.remove(&id);
        st.marked.remove(&id);
        // 마이그레이션을 보낸 원본도 `cont`로 다시 돈다.
        if st.migration_status.get(&id) == Some(&postmigrate()) {
            st.migration_status.remove(&id);
        }
        Ok(())
    }

    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError> {
        let mut st = self.state.lock().unwrap();
        if Self::controllable(&st, id).is_err() {
            return Ok(false);
        }
        if st.marked.remove(&id) {
            st.paused.remove(&id);
        }
        Ok(true)
    }

    async fn qemu_version(&self) -> Result<String, DriverError> {
        Ok(Self::version(&self.state.lock().unwrap()))
    }

    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Ok(Self::version(&st))
    }

    async fn disk_stats(&self, id: Uuid) -> Result<DiskCounters, DriverError> {
        let delay = self.state.lock().unwrap().disk_stats_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if st.failing_disk_stats.contains(&id) {
            return Err(DriverError::Timeout("query-blockstats"));
        }
        Ok(st.disk_stats.get(&id).copied().unwrap_or_default())
    }

    /// 범위의 첫 포트(내보낼 디스크가 있으면 NBD에 그다음 포트)를 돌려준다. `fail_next_start`로 주입한
    /// 오류로 실패할 수 있다.
    async fn guest_ping(&self, id: Uuid) -> Result<bool, DriverError> {
        let mut st = self.state.lock().unwrap();
        st.guest_pings.push(id);
        Self::controllable(&st, id)?;
        match Self::agent_answer(&st, id) {
            GuestAnswer::None => Err(DriverError::InvalidSpec(format!(
                "VM {id} has no guest agent channel"
            ))),
            GuestAnswer::Ok => Ok(true),
            GuestAnswer::Hang => Ok(false),
        }
    }

    /// 에이전트가 `Ok`로 답하면 `set_guest_info`로 정한 정보(없으면 빈 정보)다.
    async fn guest_info(&self, id: Uuid) -> Result<GuestInfo, DriverError> {
        let mut st = self.state.lock().unwrap();
        st.guest_info_calls.push(id);
        Self::controllable(&st, id)?;
        Self::agent_session(&st, id)?;
        let mut info = st.guest_infos.get(&id).cloned().unwrap_or_default();
        info.truncate();
        Ok(info)
    }

    async fn set_user_password(
        &self,
        id: Uuid,
        user: &str,
        _password: &str,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Self::agent_session(&st, id)?;
        st.password_sets.push((id, user.to_string()));
        Ok(())
    }

    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        task: Uuid,
        _listen_host: &str,
        ports: RangeInclusive<u16>,
        exports: &[usize],
    ) -> Result<IncomingPorts, DriverError> {
        let hold = std::mem::take(&mut self.state.lock().unwrap().hold_next_start);
        if hold {
            self.start_reached.notify_one();
            self.start_released.notified().await;
        }
        {
            let mut st = self.state.lock().unwrap();
            st.incoming_specs.push(vm.clone());
            if st.vms.contains_key(&vm.id) {
                return Err(DriverError::Migration(format!(
                    "VM {} is already running on this node",
                    vm.id
                )));
            }
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
            Self::launched(&mut st, vm);
            st.incoming.insert(vm.id, task);
            st.migration_status.insert(vm.id, inmigrate());
            if !exports.is_empty() {
                st.exports.insert(vm.id, exports.to_vec());
            }
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(IncomingPorts {
            migration: *ports.start(),
            nbd: (!exports.is_empty()).then(|| ports.start() + 1),
        })
    }

    /// 주입한 실패가 없으면 받기를 마친 것으로 본다(멈춘 채).
    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError> {
        let hold = {
            let mut st = self.state.lock().unwrap();
            Self::controllable(&st, id)?;
            if !st.incoming.contains_key(&id) {
                return Err(DriverError::Migration(format!(
                    "VM {id} is not receiving a migration"
                )));
            }
            if let Some(err) = st.fail_next_incoming.take() {
                drop(st);
                self.remove(id, EventKind::Crashed);
                return Err(err);
            }
            st.migration_status.insert(id, RuntimeStatus::Paused);
            std::mem::take(&mut st.hold_next_incoming_wait)
        };
        if hold {
            self.incoming_wait_reached.notify_one();
            self.incoming_wait_released.notified().await;
        }
        Ok(())
    }

    /// 실제 드라이버처럼 `bitmap`을 모든 디스크에 만든다(만들지 못해도 넘겨받는다).
    async fn finish_incoming(&self, id: Uuid, bitmap: Option<&str>) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains_key(&id) {
            return Ok(());
        }
        if st.migration_status.get(&id) == Some(&inmigrate()) {
            return Err(DriverError::Migration(format!(
                "incoming migration of VM {id} has not completed"
            )));
        }
        if let Some(name) = bitmap
            && let Err(e) = Self::add_bitmaps_to(&mut st, id, name)
        {
            tracing::warn!(%id, error = %e, "cannot add the dirty bitmap of the reversed replication");
        }
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        st.exports.remove(&id);
        Ok(())
    }

    async fn list_incoming(&self) -> Result<Vec<(Uuid, Uuid)>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .incoming
            .iter()
            .filter(|(id, _)| st.vms.contains_key(id))
            .map(|(&id, &task)| (id, task))
            .collect())
    }

    /// 가짜 진행률(디스크마다 [`MOCK_MIRROR_BYTES`])을 알리고 곧바로 모두 ready가 된다. 실제 드라이버처럼
    /// qcow2 디스크를 raw로 옮기면 그 디스크의 `opl-` 비트맵을 지운다.
    async fn mirror_disks(
        &self,
        id: Uuid,
        targets: &[MirrorTarget],
        opts: &MirrorOptions,
    ) -> Result<(), DriverError> {
        let total = MOCK_MIRROR_BYTES * targets.len() as u64;
        let send = |done_bytes| {
            if let Some(tx) = &opts.progress {
                tx.send_replace(MirrorProgress {
                    done_bytes,
                    total_bytes: total,
                });
            }
        };
        let hold = {
            let mut st = self.state.lock().unwrap();
            st.mirror_calls.push((id, targets.to_vec()));
            Self::controllable(&st, id)?;
            st.mirroring.remove(&id);
            let formats = st.disk_formats.get(&id).cloned().unwrap_or_default();
            for target in targets {
                if formats.get(target.disk) == Some(&ImageFormat::Qcow2)
                    && target.format == ImageFormat::Raw
                    && let Some(disk) = st.bitmaps.get_mut(&id).and_then(|d| d.get_mut(target.disk))
                {
                    disk.retain(|b| !b.name.starts_with(BITMAP_PREFIX));
                }
            }
            send(0);
            if let Some(err) = st.fail_next_mirror.take() {
                return Err(err);
            }
            if std::mem::take(&mut st.stall_next_mirror) {
                return Err(DriverError::Migration(format!(
                    "disk copy made no progress for {}",
                    format_duration(opts.stall)
                )));
            }
            st.mirroring
                .insert(id, targets.iter().map(|t| t.disk).collect());
            std::mem::take(&mut st.hold_next_mirror)
        };
        if hold {
            self.mirror_reached.notify_one();
            self.mirror_released.notified().await;
            // 멈춘 사이 치워졌으면(`cancel_migration`) 실제 드라이버처럼 실패한다.
            if !self.state.lock().unwrap().mirroring.contains_key(&id) {
                return Err(DriverError::Migration(format!(
                    "disk mirror opl-mirror-{} disappeared",
                    targets.first().map_or(0, |t| t.disk)
                )));
            }
        }
        send(total);
        Ok(())
    }

    /// 실제 QEMU처럼 기준 비트맵이 있고 inconsistent가 아니어야 한다. 정한 패스 수(없으면 1)만큼 한 것으로 하고 가짜
    /// 진행률을 알린 뒤 마지막 패스의 임시 비트맵(저장하지 않음)을 대상 디스크에 남긴다. 받는 쪽에는 쓰지 않는다.
    async fn catch_up_disks(&self, id: Uuid, req: &CatchUp) -> Result<CatchUpState, DriverError> {
        let mut st = self.state.lock().unwrap();
        let mut call = CatchUpCall {
            vm: id,
            task: req.task,
            targets: req.targets.clone(),
            base: req.base.clone(),
            done: None,
        };
        if let Err(e) = Self::controllable(&st, id) {
            st.catch_ups.push(call);
            return Err(e);
        }
        st.catching_up.remove(&id);
        let formats = st.disk_formats.get(&id).cloned().unwrap_or_default();
        let mut error = (req.targets.is_empty() || req.base.len() != req.targets.len())
            .then(|| DriverError::InvalidSpec(format!("nothing to catch up for VM {id}")));
        for (t, base) in req.targets.iter().zip(&req.base) {
            if t.disk >= formats.len() {
                error.get_or_insert(DriverError::InvalidSpec(format!(
                    "VM {id} has no disk{}",
                    t.disk
                )));
                continue;
            }
            let Some(base) = base else { continue };
            let found = st
                .bitmaps
                .get(&id)
                .and_then(|d| d.get(t.disk))
                .and_then(|d| d.iter().find(|b| &b.name == base));
            match found {
                None => {
                    error.get_or_insert(qmp_error(
                        "GenericError",
                        format!("Bitmap '{base}' could not be found"),
                    ));
                }
                Some(b) if b.inconsistent => {
                    error.get_or_insert(qmp_error(
                        "GenericError",
                        format!("Bitmap '{base}' is inconsistent and cannot be used"),
                    ));
                }
                Some(_) => {}
            }
        }
        if let Some(e) = error.or_else(|| st.fail_next_catch_up.take()) {
            st.catch_ups.push(call);
            return Err(e);
        }
        let passes = st.catch_up_passes.unwrap_or(1).clamp(1, req.passes.max(1));
        let first: u64 = req
            .base
            .iter()
            .map(|b| match b {
                Some(_) => MOCK_INCREMENT_BYTES,
                None => MOCK_MIRROR_BYTES,
            })
            .sum();
        let bytes = first + u64::from(passes - 1) * MOCK_INCREMENT_BYTES * req.targets.len() as u64;
        let bitmap = migration_bitmap(req.task, passes);
        let disks = st.bitmaps.entry(id).or_default();
        disks.resize(formats.len(), Vec::new());
        for t in &req.targets {
            disks[t.disk].push(BitmapInfo {
                name: bitmap.clone(),
                recording: true,
                inconsistent: false,
                persistent: false,
                count: 0,
            });
        }
        if let Some(tx) = &req.progress {
            tx.send_replace(MirrorProgress {
                done_bytes: bytes,
                total_bytes: bytes,
            });
        }
        let state = CatchUpState {
            disks: req.targets.iter().map(|t| t.disk).collect(),
            bitmap,
            passes,
        };
        st.catching_up.insert(id, state.clone());
        call.done = Some((passes, bytes));
        st.catch_ups.push(call);
        Ok(state)
    }

    /// 주입한 실패가 없으면 원본을 `postmigrate`로 멈춘다. `switchover_mirrors`면 남은 미러를 끝낸다.
    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        opts: &MigrateOptions,
    ) -> Result<(), DriverError> {
        let delay = self.state.lock().unwrap().migrate_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let mut st = self.state.lock().unwrap();
        st.migrations.push((id, target.clone()));
        Self::controllable(&st, id)?;
        if opts.switchover_mirrors && !st.mirroring.contains_key(&id) {
            return Err(DriverError::Migration(
                "no disk mirrors to switch over".into(),
            ));
        }
        if opts.switchover_catch_up.is_some() && !st.catching_up.contains_key(&id) {
            return Err(DriverError::Migration(
                "no disk copies to switch over".into(),
            ));
        }
        if let Some(err) = st.fail_next_migrate.take() {
            return Err(err);
        }
        if let Some(err) = st.fail_next_migrate_unfinished.take() {
            st.outgoing.insert(id);
            return Err(err);
        }
        if opts.switchover_mirrors {
            if let Some(error) = st.fail_next_switchover.take() {
                let disk = st.mirroring.remove(&id).unwrap_or_default();
                return Err(DriverError::Migration(format!(
                    "disk mirror opl-mirror-{} failed: {error}",
                    disk.first().copied().unwrap_or_default()
                )));
            }
            st.mirroring.remove(&id);
            st.switchovers.push(id);
        }
        if let Some(state) = &opts.switchover_catch_up {
            if let Some(error) = st.fail_next_switchover.take() {
                st.catching_up.remove(&id);
                return Err(DriverError::Migration(format!(
                    "disk copy job opl-catchup-{} failed: {error}",
                    state.disks.first().copied().unwrap_or_default()
                )));
            }
            st.catching_up.remove(&id);
            st.switchovers.push(id);
        }
        st.migration_status.insert(id, postmigrate());
        Ok(())
    }

    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError> {
        let migrated = {
            let st = self.state.lock().unwrap();
            // 끝나지 않는 QEMU(`set_unkillable`)는 실제 드라이버처럼 끄지 못한다.
            if st.unkillable.contains(&id) {
                return Err(DriverError::Timeout("QEMU to exit"));
            }
            st.vms.contains_key(&id) && st.migration_status.get(&id) == Some(&postmigrate())
        };
        if migrated {
            self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            );
        }
        Ok(migrated)
    }

    /// 실제 드라이버처럼 도는 VM의 장치와 `expected`를 견준다. Mock은 디스크·NIC·CD-ROM·씨앗 장치(`disk{i}-dev`·
    /// `nic{i}`·`cd{i}-dev`·`cidata-dev`)만 따라가므로 그것만 견준다.
    async fn check_devices(&self, id: Uuid, expected: &[String]) -> Result<(), DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        let tracked = |d: &&String| {
            d.starts_with("nic")
                || *d == "cidata-dev"
                || d.ends_with("-dev") && (d.starts_with("disk") || d.starts_with("cd"))
        };
        let want: BTreeSet<&String> = expected.iter().filter(tracked).collect();
        let running: BTreeSet<&String> = st
            .hot
            .get(&id)
            .map(|h| h.devices.keys().collect())
            .unwrap_or_default();
        if want != running {
            return Err(DriverError::Migration(format!(
                "the running QEMU devices do not match the VM's hardware record: QEMU has {running:?}, the record has {want:?}"
            )));
        }
        Ok(())
    }

    async fn hotplug_caps(&self) -> NodeHotplug {
        let caps = self.state.lock().unwrap().hotplug_caps;
        caps.unwrap_or(NodeHotplug {
            cpu: true,
            memory: true,
        })
    }

    async fn firmware_caps(&self) -> FirmwareCaps {
        let caps = self.state.lock().unwrap().firmware_caps;
        caps.unwrap_or(FirmwareCaps {
            tpm: true,
            secure_boot: true,
        })
    }

    /// 실제 드라이버처럼 켤 때 수 아래·상한 위는 `InvalidSpec`이다. 지금 수에서 `to`로 바꾼다(`from`은 기록만).
    async fn set_vcpus(
        &self,
        id: Uuid,
        cpu: &CpuSpec,
        from: u16,
        to: u16,
        _timeout: Duration,
    ) -> Result<VcpuChange, DriverError> {
        let hold = std::mem::take(&mut self.state.lock().unwrap().hold_next_hotplug);
        if hold {
            self.hotplug_reached.notify_one();
            self.hotplug_released.notified().await;
        }
        let mut st = self.state.lock().unwrap();
        Self::hotplug_call(&mut st, HotplugCall::SetVcpus { id, from, to })?;
        let (boot, max) = (cpu.vcpus(), cpu.max());
        if max <= boot || !(boot..=max).contains(&u32::from(to)) {
            return Err(DriverError::InvalidSpec(format!(
                "VM {id} can run {boot} to {max} vCPUs, not {to}"
            )));
        }
        let held = st.hold_unplug.contains(&id);
        let refused = st.refuse_vcpu_unplug.contains(&id);
        let hot = st.hot.entry(id).or_default();
        let shrink = to < hot.vcpus;
        if !shrink || !held && !refused {
            hot.vcpus = to;
        }
        Ok(VcpuChange {
            reached: hot.vcpus,
            refused: shrink && refused,
        })
    }

    /// virtio-mem이 없는 VM은 실제 QEMU처럼 `DeviceNotFound`, 크기를 넘으면 오류다.
    async fn set_memory(
        &self,
        id: Uuid,
        plugged_mib: u32,
        _timeout: Duration,
    ) -> Result<u32, DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::hotplug_call(&mut st, HotplugCall::SetMemory { id, plugged_mib })?;
        let held = st.hold_unplug.contains(&id);
        let hot = st.hot.entry(id).or_default();
        if hot.vmem_mib == 0 {
            return Err(qmp_error("DeviceNotFound", "Device 'vmem0-dev' not found"));
        }
        if plugged_mib > hot.vmem_mib {
            return Err(qmp_error(
                "GenericError",
                format!(
                    "'requested-size' cannot exceed the memory backend size ({plugged_mib} MiB)"
                ),
            ));
        }
        if plugged_mib > hot.plugged_mib || !held {
            hot.plugged_mib = plugged_mib;
        }
        Ok(hot.plugged_mib)
    }

    /// 디스크 형식·비트맵 목록도 그 순번까지 늘린다(새 디스크에는 비트맵이 없다). 실제 드라이버처럼 장치가 이미
    /// 있으면 `AlreadyPresent`다.
    async fn add_disk(
        &self,
        id: Uuid,
        index: usize,
        disk: &RuntimeDisk,
        port: u8,
    ) -> Result<AddOutcome, DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::hotplug_call(&mut st, HotplugCall::AddDisk { id, index, port })?;
        if disk.bus != DiskBus::VirtioBlk {
            return Err(DriverError::InvalidSpec(
                "only virtio-blk disks can be added while the VM is running".into(),
            ));
        }
        let dev = format!("disk{index}-dev");
        if Self::present(&st, id, &dev) {
            return Ok(AddOutcome::AlreadyPresent);
        }
        Self::plug(&mut st, id, dev, port)?;
        let formats = st.disk_formats.entry(id).or_default();
        formats.truncate(index);
        formats.push(disk.format);
        st.bitmaps
            .entry(id)
            .or_default()
            .resize(index + 1, Vec::new());
        Ok(AddOutcome::Added)
    }

    /// 떼면 디스크 형식·비트맵 목록도 그 순번 앞까지 줄인다.
    async fn remove_disk(
        &self,
        id: Uuid,
        index: usize,
        _timeout: Duration,
    ) -> Result<bool, DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::hotplug_call(&mut st, HotplugCall::RemoveDisk { id, index })?;
        let removed = Self::unplug(&mut st, id, &format!("disk{index}-dev"))?;
        if removed {
            st.disk_formats.entry(id).or_default().truncate(index);
            st.bitmaps.entry(id).or_default().truncate(index);
        }
        Ok(removed)
    }

    async fn add_nic(
        &self,
        id: Uuid,
        index: usize,
        _nic: &RuntimeNic,
        port: u8,
    ) -> Result<AddOutcome, DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::hotplug_call(&mut st, HotplugCall::AddNic { id, index, port })?;
        let dev = format!("nic{index}");
        if Self::present(&st, id, &dev) {
            return Ok(AddOutcome::AlreadyPresent);
        }
        Self::plug(&mut st, id, dev, port)?;
        Ok(AddOutcome::Added)
    }

    async fn remove_nic(
        &self,
        id: Uuid,
        index: usize,
        _timeout: Duration,
    ) -> Result<bool, DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::hotplug_call(&mut st, HotplugCall::RemoveNic { id, index })?;
        Self::unplug(&mut st, id, &format!("nic{index}"))
    }

    async fn cancel_migration(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        if let Some(err) = st.fail_next_cancel.pop_front() {
            return Err(err);
        }
        if st.outgoing.remove(&id) && st.complete_on_cancel {
            st.migration_status.insert(id, postmigrate());
        }
        st.mirroring.remove(&id);
        st.catching_up.remove(&id);
        for disk in st.bitmaps.get_mut(&id).into_iter().flatten() {
            disk.retain(|b| !b.name.starts_with(MIGRATION_BITMAP_PREFIX));
        }
        Ok(())
    }

    /// 실제 드라이버처럼 없는 드라이브는 `InvalidSpec`, 같은 매체는 할 일이 없다.
    async fn change_medium(
        &self,
        id: Uuid,
        index: usize,
        medium: Option<&RuntimeMedium>,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        let path = medium.map(|m| m.path.clone());
        let call = HotplugCall::ChangeMedium {
            id,
            index,
            medium: path.clone(),
        };
        Self::hotplug_call(&mut st, call)?;
        let drive = st
            .hot
            .get_mut(&id)
            .and_then(|h| h.media.get_mut(index))
            .ok_or_else(|| {
                DriverError::InvalidSpec(format!("the VM has no CD-ROM drive {index}"))
            })?;
        *drive = path;
        Ok(())
    }

    fn save_hardware(&self, id: Uuid, hw: &RunningHardware) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        if !st.vms.contains_key(&id) {
            return Err(DriverError::NotRunning(id));
        }
        st.saved_hardware.insert(id, hw.clone());
        Ok(())
    }

    fn saved_hardware(&self, id: Uuid) -> Option<RunningHardware> {
        self.state.lock().unwrap().saved_hardware.get(&id).cloned()
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

/// QEMU가 명령에 오류로 답한 것
fn qmp_error(class: &str, desc: impl Into<String>) -> DriverError {
    DriverError::Qmp(QmpError::Command {
        class: class.into(),
        desc: desc.into(),
    })
}

fn inmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("inmigrate".into())
}

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

#[cfg(test)]
mod tests {
    use opl_types::vm::{CpuModel, Firmware, MachineType};

    use super::*;

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
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
        }
    }

    /// 채널 없이 띄운 VM은 오류, 채널을 단 VM은 정한 대로 답한다(정하지 않으면 답하지 않음). 꺼진 VM은 오류다.
    #[tokio::test]
    async fn guest_ping_follows_the_channel_and_the_set_answer() {
        let hv = MockHypervisor::default();
        let plain = spec();
        let watched = VmRuntimeSpec {
            guest_agent: true,
            ..spec()
        };
        assert!(hv.guest_ping(watched.id).await.is_err());
        hv.start(&plain, None).await.unwrap();
        hv.start(&watched, None).await.unwrap();
        assert!(hv.guest_ping(plain.id).await.is_err());
        assert!(!hv.guest_ping(watched.id).await.unwrap());
        hv.set_guest_agent(watched.id, GuestAnswer::Ok);
        assert!(hv.guest_ping(watched.id).await.unwrap());
        hv.set_guest_agent(watched.id, GuestAnswer::Hang);
        assert!(!hv.guest_ping(watched.id).await.unwrap());
        hv.set_guest_agent(watched.id, GuestAnswer::None);
        assert!(hv.guest_ping(watched.id).await.is_err());
        assert_eq!(hv.guest_pings(watched.id), 5);
        assert_eq!(hv.guest_pings(plain.id), 1);
    }

    /// 게스트 정보·비밀번호·에이전트 종료·얼리기는 `guest_ping`과 같은 답(채널, `set_guest_agent`)을 따른다.
    #[tokio::test]
    async fn agent_calls_follow_the_set_answer() {
        let hv = MockHypervisor::default();
        let watched = VmRuntimeSpec {
            guest_agent: true,
            ..spec()
        };
        hv.start(&watched, None).await.unwrap();
        let id = watched.id;
        assert!(matches!(
            hv.guest_info(id).await,
            Err(DriverError::GuestAgent(_))
        ));
        hv.stop(id, StopMode::Agent).await.unwrap();
        assert_eq!(
            hv.snapshot_save(id, "a", false, true).await.unwrap(),
            Quiesced::Skipped(
                "guest agent unavailable: the guest agent did not answer guest-sync-delimited in time"
                    .into()
            )
        );
        hv.set_guest_agent(id, GuestAnswer::Ok);
        let info = GuestInfo {
            hostname: Some("web-1".into()),
            ..GuestInfo::default()
        };
        hv.set_guest_info(id, info.clone());
        assert_eq!(hv.guest_info(id).await.unwrap(), info);
        assert_eq!(hv.guest_info_calls(id), 2);
        hv.set_user_password(id, "root", "secret").await.unwrap();
        assert_eq!(hv.password_sets(id), ["root"]);
        hv.stop(id, StopMode::Agent).await.unwrap();
        hv.stop(id, StopMode::Acpi).await.unwrap();
        assert_eq!(
            hv.stop_requests(id),
            [StopMode::Acpi, StopMode::Agent, StopMode::Acpi]
        );
        assert_eq!(
            hv.snapshot_save(id, "b", false, true).await.unwrap(),
            Quiesced::Yes
        );
        // 메모리 스냅샷은 얼리지 않는다.
        assert_eq!(
            hv.snapshot_save(id, "c", true, true).await.unwrap(),
            Quiesced::No
        );
        hv.fail_freeze(id, Some("frozen already"));
        let q = hv.snapshot_save(id, "d", false, true).await.unwrap();
        assert_eq!(q, Quiesced::Skipped("frozen already".into()));
        assert!(q.warning().unwrap().contains("crash-consistent"));
        assert_eq!(hv.quiesce_calls(id), [true, true, false, true]);
        assert_eq!(
            hv.firmware_caps().await,
            FirmwareCaps {
                tpm: true,
                secure_boot: true
            }
        );
        hv.set_firmware_caps(FirmwareCaps::default());
        assert_eq!(hv.firmware_caps().await, FirmwareCaps::default());
    }

    #[tokio::test]
    async fn start_is_idempotent_and_emits_one_event() {
        let hv = MockHypervisor::default();
        let mut rx = hv.subscribe();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Started
            }
        );
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stop_removes_vm_and_is_idempotent() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: false
            }
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(matches!(
            hv.console_endpoint(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn disk_stats_return_counters_set_by_the_test() {
        let hv = MockHypervisor::default();
        let vm = spec();
        assert!(matches!(
            hv.disk_stats(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
        hv.start(&vm, None).await.unwrap();
        assert_eq!(hv.disk_stats(vm.id).await.unwrap(), DiskCounters::default());

        let counters = DiskCounters {
            rd_bytes: 4096,
            wr_bytes: 512,
            rd_operations: 2,
            wr_operations: 1,
        };
        hv.set_disk_stats(vm.id, counters);
        assert_eq!(hv.disk_stats(vm.id).await.unwrap(), counters);
        hv.set_unresponsive(vm.id, true);
        assert!(matches!(
            hv.disk_stats(vm.id).await,
            Err(DriverError::NotAttached(_))
        ));
        hv.set_unresponsive(vm.id, false);
        hv.fail_disk_stats(vm.id, true);
        assert!(matches!(
            hv.disk_stats(vm.id).await,
            Err(DriverError::Timeout(_))
        ));
        assert_eq!(hv.list().await.unwrap()[0].status, RuntimeStatus::Running);
        hv.fail_disk_stats(vm.id, false);
        assert_eq!(hv.disk_stats(vm.id).await.unwrap(), counters);
        hv.fail_disk_stats(vm.id, true);

        // 실제 QEMU처럼 다시 켜면 0부터 센다(실패시킨 것도 지워진다).
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(hv.disk_stats(vm.id).await.unwrap(), DiskCounters::default());
    }

    #[tokio::test]
    async fn console_dir_sets_endpoint() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.set_console_dir("/tmp/consoles");
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.console_endpoint(vm.id).await.unwrap(),
            PathBuf::from(format!("/tmp/consoles/{}.vnc", vm.id))
        );
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm, None).await.unwrap();
    }

    #[tokio::test]
    async fn injected_failure_does_not_apply_to_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 실패하지 않는다.
        hv.start(&vm, None).await.unwrap();
        // 주입한 오류는 소비되지 않고 다음 실제 기동에 적용된다.
        hv.crash(vm.id);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
    }

    #[tokio::test]
    async fn acpi_stop_only_requests_shutdown() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Acpi).await.unwrap();
        // 게스트가 요청을 무시할 수 있으므로 VM은 그대로 돌고 이벤트도 없다.
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);

        hv.guest_shutdown(vm.id);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: true
            }
        );
    }

    #[tokio::test]
    async fn kill_ends_the_vm_as_a_crash_unless_it_is_unkillable() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.set_unkillable(vm.id, true);
        // D 상태 QEMU는 SIGKILL에도 끝나지 않는다. 다시 시도할 수 있는 오류이고 VM은 그대로 돈다.
        let err = hv.stop(vm.id, StopMode::Kill).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
        assert!(hv.inspect(vm.id).await.unwrap().is_some());
        assert!(rx.try_recv().is_err());

        hv.set_unkillable(vm.id, false);
        hv.stop(vm.id, StopMode::Kill).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert_eq!(rx.recv().await.unwrap().kind, EventKind::Crashed);
        // 이미 꺼진 VM은 그대로 성공한다.
        hv.stop(vm.id, StopMode::Kill).await.unwrap();

        // QMP로 다룰 수 없는 VM도 끝낸다.
        hv.start(&vm, None).await.unwrap();
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Kill).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn unresponsive_vm_cannot_be_controlled_but_can_be_killed() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
        let mut rx = hv.subscribe();
        hv.set_unresponsive(vm.id, true);

        let unresponsive = ActualState {
            id: vm.id,
            pid,
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(unresponsive.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![unresponsive]);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::Timeout(_))
        ));
        assert!(matches!(
            hv.stop(vm.id, StopMode::Acpi).await,
            Err(DriverError::Timeout(_))
        ));

        // 다시 붙을 수 있게 되면 그대로 돌고 있다.
        hv.set_unresponsive(vm.id, false);
        hv.start(&vm, None).await.unwrap();
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!((st.pid, st.status), (pid, RuntimeStatus::Running));

        // 강제 종료는 프로세스를 죽이므로 종료 이벤트가 없다.
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn records_restore_tag_of_each_launch() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, Some("s1")).await.unwrap();
        // 이미 실행 중이면 새로 띄우지 않으므로 기록하지 않는다.
        hv.start(&vm, None).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.launches(),
            vec![(vm.id, Some("s1".to_string())), (vm.id, None)]
        );
    }

    #[tokio::test]
    async fn snapshot_ops_need_a_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        for res in [
            hv.snapshot_save(vm.id, "s", true, false).await.map(drop),
            hv.snapshot_delete(vm.id, "s").await,
            hv.block_resize(vm.id, 0, 1 << 30).await,
            hv.pause(vm.id).await,
            hv.resume(vm.id).await,
        ] {
            assert!(matches!(res, Err(DriverError::NotRunning(_))), "{res:?}");
        }

        hv.start(&vm, None).await.unwrap();
        hv.snapshot_save(vm.id, "mem", true, false).await.unwrap();
        hv.snapshot_save(vm.id, "disk", false, false).await.unwrap();
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        // 없는 태그를 지워도 성공한다.
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        assert_eq!(hv.snapshots(vm.id), vec![("disk".to_string(), false)]);
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        assert_eq!(hv.disk_size(vm.id, 1), Some(2 << 30));
        assert_eq!(hv.disk_size(vm.id, 0), None);
        hv.pause(vm.id).await.unwrap();
        assert!(hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Paused);
        hv.resume(vm.id).await.unwrap();
        assert!(!hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // QMP로 다룰 수 없는 VM은 다시 시도할 수 있는 오류로 끝난다.
        hv.set_unresponsive(vm.id, true);
        let err = hv
            .snapshot_save(vm.id, "x", false, false)
            .await
            .unwrap_err();
        assert!(err.is_transient(), "{err:?}");
    }

    #[tokio::test]
    async fn injected_snapshot_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        // 저장은 태그를 남긴 채 실패한다.
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_save(vm.id, "s", true, false).await.is_err());
        assert_eq!(hv.snapshots(vm.id), vec![("s".to_string(), true)]);
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_delete(vm.id, "s").await.is_err());
        assert_eq!(hv.snapshots(vm.id).len(), 1);
        hv.snapshot_delete(vm.id, "s").await.unwrap();
        assert!(hv.snapshots(vm.id).is_empty());
    }

    /// qcow2 디스크 하나와 raw 디스크 하나를 가진 VM
    fn spec_with_disks() -> VmRuntimeSpec {
        use opl_types::runtime::{DiskKind, RuntimeDisk};
        use opl_types::vm::DiskBus;
        let disk = |format| RuntimeDisk {
            path: PathBuf::from("/p/d"),
            kind: DiskKind::File,
            rbd: None,
            format,
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        };
        VmRuntimeSpec {
            disks: vec![disk(ImageFormat::Qcow2), disk(ImageFormat::Raw)],
            ..spec()
        }
    }

    fn names(hv: &MockHypervisor, id: Uuid) -> Vec<Vec<String>> {
        hv.bitmaps(id)
            .iter()
            .map(|d| d.iter().map(|b| b.name.clone()).collect())
            .collect()
    }

    fn targets() -> Vec<BackupTarget> {
        (0..2)
            .map(|i| BackupTarget {
                node: format!("disk{i}"),
                path: PathBuf::from(format!("/b/disk{i}.qcow2.part")),
                size: 1 << 30,
            })
            .collect()
    }

    #[tokio::test]
    async fn bitmaps_follow_backups_and_only_persistent_ones_survive_a_restart() {
        let hv = MockHypervisor::default();
        let vm = spec_with_disks();
        for res in [
            hv.add_bitmaps(vm.id, "opl-a").await,
            hv.remove_bitmaps(vm.id, None).await,
            hv.snapshot_save_with_bitmap(vm.id, "t", "opl-a", false)
                .await
                .map(drop),
            hv.incremental_backup(vm.id, "opl-a", "opl-b", &targets(), false)
                .await
                .map(drop),
            hv.clear_backup_leftovers(vm.id).await,
        ] {
            assert!(matches!(res, Err(DriverError::NotRunning(_))), "{res:?}");
        }
        assert_eq!(hv.leftover_clears(), [vm.id]);
        assert!(hv.backup_bitmaps(vm.id).await.is_err());

        hv.start(&vm, None).await.unwrap();
        // 전체 백업: 스냅샷과 비트맵을 한 시점에. qcow2 디스크의 비트맵만 persistent다.
        hv.snapshot_save_with_bitmap(vm.id, "t", "opl-a", false)
            .await
            .unwrap();
        assert_eq!(hv.snapshots(vm.id), [("t".to_string(), false)]);
        let disks = hv.backup_bitmaps(vm.id).await.unwrap();
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[1].node, "disk1");
        let a = &disks[0].bitmaps[0];
        assert_eq!(
            (a.name.as_str(), a.recording, a.inconsistent, a.persistent),
            ("opl-a", true, false, true)
        );
        assert!(!disks[1].bitmaps[0].persistent);
        // 같은 이름은 다시 만들 수 없다.
        assert!(hv.add_bitmaps(vm.id, "opl-a").await.is_err());

        // 증분: 성공하면 새 비트맵만 남는다.
        hv.incremental_backup(vm.id, "opl-a", "opl-b", &targets(), false)
            .await
            .unwrap();
        assert_eq!(names(&hv, vm.id), [["opl-b"], ["opl-b"]]);
        // 실패하면 그대로다.
        hv.fail_next_incremental(DriverError::Qmp(QmpError::JobFailed {
            id: "opl-backup-0".into(),
            error: "No space left on device".into(),
        }));
        assert!(
            hv.incremental_backup(vm.id, "opl-b", "opl-c", &targets(), false)
                .await
                .is_err()
        );
        assert_eq!(names(&hv, vm.id), [["opl-b"], ["opl-b"]]);
        // 없는 비트맵, 디스크와 맞지 않는 대상, inconsistent 비트맵은 실패한다.
        assert!(
            hv.incremental_backup(vm.id, "opl-x", "opl-c", &targets(), false)
                .await
                .is_err()
        );
        assert!(
            hv.incremental_backup(vm.id, "opl-b", "opl-c", &targets()[..1], false)
                .await
                .is_err()
        );
        hv.set_bitmap_inconsistent(vm.id, 1, "opl-b");
        assert!(hv.backup_bitmaps(vm.id).await.unwrap()[1].bitmaps[0].inconsistent);
        assert!(
            hv.incremental_backup(vm.id, "opl-b", "opl-c", &targets(), false)
                .await
                .is_err()
        );
        assert_eq!(names(&hv, vm.id), [["opl-b"], ["opl-b"]]);
        // 꺼진 VM에 부른 것까지 모두 기록한다.
        let calls = hv.incremental_backups();
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[1], (vm.id, "opl-a".into(), "opl-b".into(), targets()));

        // 백업 비트맵 중 keep만 남긴다. 복제 비트맵과 opl이 만들지 않은 비트맵은 건드리지 않는다.
        let old = opl_types::backup::bitmap_name(Uuid::now_v7());
        let keep = opl_types::backup::bitmap_name(Uuid::now_v7());
        let repl = opl_types::replication::replica_bitmap(Uuid::now_v7(), Uuid::now_v7());
        let user = BitmapInfo {
            name: "user".into(),
            recording: true,
            inconsistent: false,
            persistent: false,
            count: 0,
        };
        let mut with_old = hv.bitmaps(vm.id)[0].clone();
        with_old[0].name = old;
        hv.set_bitmaps(vm.id, vec![with_old, vec![user]]);
        hv.add_bitmaps(vm.id, &keep).await.unwrap();
        hv.add_bitmaps(vm.id, &repl).await.unwrap();
        hv.remove_bitmaps(vm.id, Some(&keep)).await.unwrap();
        let (k, r) = (keep.as_str(), repl.as_str());
        assert_eq!(names(&hv, vm.id), [vec![k, r], vec!["user", k, r]]);
        // 이름을 준 것만 지운다(없는 이름은 건너뛴다).
        hv.remove_named_bitmaps(vm.id, &[repl.clone(), "opl-none".into()])
            .await
            .unwrap();
        assert_eq!(names(&hv, vm.id), [vec![k], vec!["user", k]]);

        // 껐다 켜면 persistent인 것(qcow2 디스크)만 남는다.
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(names(&hv, vm.id), [vec![k], vec![]]);
        assert!(
            hv.remove_named_bitmaps(vm.id, std::slice::from_ref(&keep))
                .await
                .is_err()
        );
        hv.start(&vm, None).await.unwrap();
        let disks = hv.backup_bitmaps(vm.id).await.unwrap();
        assert_eq!(disks[0].bitmaps[0].name, keep);
        assert!(disks[1].bitmaps.is_empty());
        hv.crash(vm.id);
        assert_eq!(names(&hv, vm.id), [vec![k], vec![]]);
    }

    fn replica_target(disk: usize, base: Option<&str>) -> ReplicaDiskTarget {
        ReplicaDiskTarget {
            disk,
            host: "10.0.0.2".into(),
            port: 60000,
            export: format!("v{disk}"),
            tls_hostname: "node-b".into(),
            base: base.map(str::to_string),
        }
    }

    fn replicate(bitmap: &str, targets: Vec<ReplicaDiskTarget>) -> ReplicateRequest {
        ReplicateRequest {
            targets,
            bitmap: bitmap.into(),
            speed: None,
            stall: Duration::from_secs(600),
            progress: None,
            quiesce: false,
        }
    }

    /// 복제 회차: 전체는 디스크마다 [`MOCK_MIRROR_BYTES`], 증분은 [`MOCK_INCREMENT_BYTES`]다. 새 비트맵은 대상
    /// 디스크에만 생기고 기준 비트맵은 그대로다. 실패하면(주입, 없는 기준 비트맵) 새 비트맵이 없다.
    #[tokio::test]
    async fn replication_adds_its_bitmap_to_the_target_disks_only() {
        let hv = MockHypervisor::default();
        let vm = spec_with_disks();
        assert!(
            hv.replicate_disks(
                vm.id,
                &replicate("opl-repl-1", vec![replica_target(0, None)])
            )
            .await
            .is_err()
        );
        hv.start(&vm, None).await.unwrap();

        let (tx, rx) = tokio::sync::watch::channel(MirrorProgress::default());
        let mut req = replicate("opl-repl-1", vec![replica_target(0, None)]);
        req.progress = Some(tx);
        assert_eq!(
            hv.replicate_disks(vm.id, &req).await.unwrap(),
            (MOCK_MIRROR_BYTES, Quiesced::No)
        );
        assert_eq!(rx.borrow().done_bytes, MOCK_MIRROR_BYTES);
        assert_eq!(names(&hv, vm.id), [vec!["opl-repl-1"], vec![]]);
        assert!(hv.bitmaps(vm.id)[0][0].persistent);

        let req = replicate(
            "opl-repl-2",
            vec![
                replica_target(0, Some("opl-repl-1")),
                replica_target(1, None),
            ],
        );
        assert_eq!(
            hv.replicate_disks(vm.id, &req).await.unwrap().0,
            MOCK_INCREMENT_BYTES + MOCK_MIRROR_BYTES
        );
        assert_eq!(
            names(&hv, vm.id),
            [vec!["opl-repl-1", "opl-repl-2"], vec!["opl-repl-2"]]
        );

        hv.fail_next_replicate("link down");
        let req = replicate("opl-repl-3", vec![replica_target(0, Some("opl-repl-2"))]);
        let err = hv.replicate_disks(vm.id, &req).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::Replication(m) if m == "link down"),
            "{err:?}"
        );
        let req = replicate("opl-repl-3", vec![replica_target(1, Some("opl-repl-1"))]);
        assert!(hv.replicate_disks(vm.id, &req).await.is_err());
        let req = replicate("opl-repl-3", vec![replica_target(2, None)]);
        assert!(hv.replicate_disks(vm.id, &req).await.is_err());
        assert_eq!(
            names(&hv, vm.id),
            [vec!["opl-repl-1", "opl-repl-2"], vec!["opl-repl-2"]]
        );
        let calls = hv.replications();
        let bytes: Vec<Option<u64>> = calls.iter().map(|c| c.bytes).collect();
        assert_eq!(
            bytes,
            [
                None,
                Some(MOCK_MIRROR_BYTES),
                Some(MOCK_INCREMENT_BYTES + MOCK_MIRROR_BYTES),
                None,
                None,
                None
            ]
        );
        assert_eq!(calls[2].targets[0].base.as_deref(), Some("opl-repl-1"));

        // 멈춘 회차는 놓아줄 때 끝난다.
        hv.hold_next_replicate();
        let req = replicate("opl-repl-4", vec![replica_target(1, None)]);
        let (held, ()) = tokio::join!(hv.replicate_disks(vm.id, &req), async {
            hv.replicate_held().await;
            assert_eq!(hv.replications().len(), 6);
            hv.release_replicate();
        });
        assert_eq!(held.unwrap().0, MOCK_MIRROR_BYTES);
    }

    /// 받는 QEMU: 회차마다 하나, 끝내기 실패는 받는 QEMU를 남기고, 끄기는 없어도 성공한다.
    #[tokio::test]
    async fn replica_receivers_are_kept_per_run() {
        let hv = MockHypervisor::default();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let export = ReceiverExport {
            name: "v".into(),
            source: crate::storage::DiskLocation::Path(
                "/p/v.qcow2".into(),
                opl_types::runtime::DiskKind::File,
            ),
            format: ImageFormat::Qcow2,
            overlay: None,
        };
        let exports = vec![export];
        assert_eq!(
            hv.start_replica_receiver(a, &exports, "10.0.0.2", 60000..=60099)
                .await
                .unwrap(),
            60000
        );
        assert!(
            hv.start_replica_receiver(a, &exports, "10.0.0.2", 60000..=60099)
                .await
                .is_err()
        );
        hv.fail_next_receiver_start(DriverError::StartFailed("boom".into()));
        assert!(
            hv.start_replica_receiver(b, &exports, "10.0.0.2", 60000..=60099)
                .await
                .is_err()
        );
        assert_eq!(hv.replica_receivers().await.unwrap(), [a]);
        assert_eq!(hv.receivers()[&a], exports);

        hv.fail_next_receiver_finish(DriverError::Timeout("QMP blockdev-del"));
        assert!(hv.finish_replica_receiver(a).await.is_err());
        assert_eq!(hv.replica_receivers().await.unwrap(), [a]);
        hv.finish_replica_receiver(a).await.unwrap();
        assert!(hv.finish_replica_receiver(a).await.is_err());
        hv.start_replica_receiver(b, &exports, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        hv.stop_replica_receiver(b).await.unwrap();
        hv.stop_replica_receiver(b).await.unwrap();
        assert!(hv.replica_receivers().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_snapshot_with_bitmap_adds_no_bitmap() {
        let hv = MockHypervisor::default();
        let vm = spec_with_disks();
        hv.start(&vm, None).await.unwrap();
        hv.fail_next_snapshot(DriverError::Timeout("QMP transaction"));
        assert!(
            hv.snapshot_save_with_bitmap(vm.id, "t", "opl-a", false)
                .await
                .is_err()
        );
        assert_eq!(names(&hv, vm.id), [Vec::<String>::new(), vec![]]);
    }

    fn target() -> MigrateTarget {
        MigrateTarget {
            host: "10.0.0.2".into(),
            port: 60000,
            tls_hostname: "10.0.0.2".into(),
        }
    }

    fn opts() -> MigrateOptions {
        MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(60),
            switchover_mirrors: false,
            switchover_catch_up: None,
            devices: None,
        }
    }

    /// 원본 노드와 대상 노드의 하이퍼바이저로 한 번의 마이그레이션을 흉내 낸다.
    #[tokio::test]
    async fn migration_between_two_mocks() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        let task = Uuid::now_v7();
        assert_eq!(src.qemu_version().await.unwrap(), MOCK_QEMU_VERSION);
        assert!(matches!(
            src.running_qemu_version(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
        src.start(&vm, None).await.unwrap();
        assert_eq!(
            src.running_qemu_version(vm.id).await.unwrap(),
            dst.qemu_version().await.unwrap()
        );

        let mut events = dst.subscribe();
        let port = dst
            .start_incoming(&vm, task, "10.0.0.2", 60000..=60099, &[])
            .await
            .unwrap();
        assert_eq!(
            port,
            IncomingPorts {
                migration: 60000,
                nbd: None
            }
        );
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Started);
        assert!(dst.is_incoming(vm.id));
        assert_eq!(dst.list_incoming().await.unwrap(), vec![(vm.id, task)]);
        let status = |hv: &MockHypervisor| {
            let st = hv.state.lock().unwrap();
            st.vms
                .get(&vm.id)
                .map(|&pid| MockHypervisor::state_of(&st, vm.id, pid).status)
        };
        assert_eq!(status(&dst), Some(inmigrate()));
        // 아직 받는 중이면 넘겨받을 수 없다.
        assert!(matches!(
            dst.finish_incoming(vm.id, None).await,
            Err(DriverError::Migration(_))
        ));
        // 이미 돌고 있는 VM은 받을 수 없다.
        assert!(matches!(
            src.start_incoming(&vm, task, "10.0.0.1", 60000..=60099, &[])
                .await,
            Err(DriverError::Migration(_))
        ));

        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(src.migrations(), vec![(vm.id, target())]);
        assert_eq!(status(&src), Some(postmigrate()));
        dst.incoming_wait(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Paused));
        dst.finish_incoming(vm.id, None).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Running));
        assert!(!dst.is_incoming(vm.id));
        assert!(dst.list_incoming().await.unwrap().is_empty());
        // 두 번째 넘겨받기는 아무것도 하지 않는다.
        dst.finish_incoming(vm.id, None).await.unwrap();
        // 받는 중 표시가 없으면 기다릴 것이 없다.
        assert!(matches!(
            dst.incoming_wait(vm.id).await,
            Err(DriverError::Migration(_))
        ));

        // 돌고 있는 VM은 끄지 않는다.
        assert!(!dst.stop_migrated(vm.id).await.unwrap());
        assert!(src.stop_migrated(vm.id).await.unwrap());
        assert_eq!(src.inspect(vm.id).await.unwrap(), None);
        assert!(!src.stop_migrated(vm.id).await.unwrap());
    }

    #[tokio::test]
    async fn failed_migration_keeps_source_and_receiver_exits() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        let task = Uuid::now_v7();
        src.start(&vm, None).await.unwrap();
        src.fail_next_migrate(DriverError::Migration("Connection refused".into()));
        assert!(matches!(
            src.migrate(vm.id, &target(), &opts()).await,
            Err(DriverError::Migration(_))
        ));
        let state = src.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // 받기에 실패한 QEMU는 크래시로 끝난다.
        dst.start_incoming(&vm, task, "10.0.0.2", 60000..=60099, &[])
            .await
            .unwrap();
        let mut events = dst.subscribe();
        dst.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
        assert!(dst.incoming_wait(vm.id).await.is_err());
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Crashed);
        assert!(dst.list_incoming().await.unwrap().is_empty());

        // 시작 실패를 주입하면 받기도 실패한다.
        dst.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            dst.start_incoming(&vm, task, "10.0.0.2", 60000..=60099, &[])
                .await,
            Err(DriverError::StartFailed(_))
        ));
        assert_eq!(dst.inspect(vm.id).await.unwrap(), None);

        // 보낸 뒤 확정 전에 실패하면 원본을 이어서 실행한다.
        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            postmigrate()
        );
        src.resume(vm.id).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            RuntimeStatus::Running
        );
        assert!(!src.stop_migrated(vm.id).await.unwrap());

        // 받는 쪽에서 버전을 다르게 보이게 할 수 있다.
        dst.set_qemu_version("9.0.0");
        assert_eq!(dst.qemu_version().await.unwrap(), "9.0.0");
    }

    #[tokio::test]
    async fn cancelled_migration_does_not_complete_later() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let status = |hv: &MockHypervisor| {
            let st = hv.state.lock().unwrap();
            MockHypervisor::state_of(&st, vm.id, st.vms[&vm.id]).status
        };
        // 끝을 기다리지 못한 마이그레이션은 보내는 중으로 남고, 나중에 끝나면 원본이 멈춘다.
        hv.fail_next_migrate_unfinished(DriverError::Timeout("QMP query-migrate"));
        assert!(hv.migrate(vm.id, &target(), &opts()).await.is_err());
        assert!(hv.is_outgoing(vm.id));
        assert_eq!(status(&hv), RuntimeStatus::Running);
        hv.complete_outgoing(vm.id);
        assert_eq!(status(&hv), postmigrate());
        hv.resume(vm.id).await.unwrap();

        // 취소하면 끝나지 않는다.
        hv.start_outgoing(vm.id);
        hv.cancel_migration(vm.id).await.unwrap();
        assert!(!hv.is_outgoing(vm.id));
        hv.complete_outgoing(vm.id);
        assert_eq!(status(&hv), RuntimeStatus::Running);
        // 보내는 중이 아니거나 없는 VM이면 아무것도 하지 않는다.
        hv.cancel_migration(vm.id).await.unwrap();
        hv.cancel_migration(Uuid::now_v7()).await.unwrap();

        // 취소하는 사이에 끝날 수 있다.
        hv.complete_on_cancel();
        hv.start_outgoing(vm.id);
        hv.cancel_migration(vm.id).await.unwrap();
        assert_eq!(status(&hv), postmigrate());
    }

    fn mirror_target(disk: usize, format: ImageFormat) -> MirrorTarget {
        MirrorTarget {
            disk,
            host: "10.0.0.2".into(),
            port: 60001,
            export: format!("disk{disk}"),
            tls_hostname: "10.0.0.2".into(),
            format,
        }
    }

    fn mirror_opts() -> (MirrorOptions, tokio::sync::watch::Receiver<MirrorProgress>) {
        let (tx, rx) = tokio::sync::watch::channel(MirrorProgress::default());
        let opts = MirrorOptions {
            speed: None,
            timeout: Duration::from_secs(60),
            stall: Duration::from_secs(600),
            progress: Some(tx),
        };
        (opts, rx)
    }

    /// 로컬 디스크 마이그레이션: 받는 쪽 내보내기와 NBD 포트, 미러 기록·가짜 진행률·비트맵 규칙,
    /// 스위치오버
    #[tokio::test]
    async fn local_disk_migration_between_two_mocks() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec_with_disks();
        let task = Uuid::now_v7();
        src.start(&vm, None).await.unwrap();
        src.add_bitmaps(vm.id, "opl-a").await.unwrap();

        let ports = dst
            .start_incoming(&vm, task, "10.0.0.2", 60000..=60099, &[0, 1])
            .await
            .unwrap();
        assert_eq!(
            ports,
            IncomingPorts {
                migration: 60000,
                nbd: Some(60001)
            }
        );
        assert_eq!(dst.exports(vm.id), [0, 1]);

        // 미러 없이 스위치오버를 바라면 실패한다.
        let switchover = MigrateOptions {
            switchover_mirrors: true,
            switchover_catch_up: None,
            devices: None,
            ..opts()
        };
        assert!(src.migrate(vm.id, &target(), &switchover).await.is_err());

        // qcow2 디스크(disk0)를 raw로 옮기면 그 디스크의 opl- 비트맵을 지운다. raw 디스크(disk1)는 그대로.
        let targets = [
            mirror_target(0, ImageFormat::Raw),
            mirror_target(1, ImageFormat::Raw),
        ];
        let (mirror, rx) = mirror_opts();
        src.mirror_disks(vm.id, &targets, &mirror).await.unwrap();
        assert_eq!(src.mirrors(), vec![(vm.id, targets.to_vec())]);
        assert_eq!(src.mirroring(vm.id), [0, 1]);
        assert_eq!(
            *rx.borrow(),
            MirrorProgress {
                done_bytes: 2 * MOCK_MIRROR_BYTES,
                total_bytes: 2 * MOCK_MIRROR_BYTES
            }
        );
        let bitmaps = src.bitmaps(vm.id);
        assert!(bitmaps[0].is_empty(), "{bitmaps:?}");
        assert_eq!(bitmaps[1].len(), 1);

        src.migrate(vm.id, &target(), &switchover).await.unwrap();
        assert_eq!(src.switchovers(), [vm.id]);
        assert!(src.mirroring(vm.id).is_empty());
        dst.incoming_wait(vm.id).await.unwrap();
        dst.finish_incoming(vm.id, None).await.unwrap();
        assert!(dst.exports(vm.id).is_empty());
        assert!(src.stop_migrated(vm.id).await.unwrap());
    }

    /// 따라잡기: 기준 비트맵이 있어야 하고, 정한 패스 수만큼 한 것으로 하며 마지막 임시 비트맵을 남긴다. 스위치오버는
    /// 따라잡기를 끝내고, 받는 쪽 넘겨받기는 뒤집힌 복제의 비트맵을 만든다. 실패 주입·스위치오버 오류·되돌리기는
    /// 따라잡기를 남기지 않는다.
    #[tokio::test]
    async fn catch_up_between_two_mocks() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec_with_disks();
        let task = Uuid::now_v7();
        src.start(&vm, None).await.unwrap();
        src.add_bitmaps(vm.id, "opl-repl-a").await.unwrap();
        dst.start_incoming(&vm, task, "10.0.0.2", 60000..=60099, &[0, 1])
            .await
            .unwrap();
        let targets = vec![
            mirror_target(0, ImageFormat::Qcow2),
            mirror_target(1, ImageFormat::Raw),
        ];
        let req = |base: Vec<Option<String>>| CatchUp {
            targets: targets.clone(),
            base,
            task,
            limit: 64 << 20,
            passes: 5,
            speed: None,
            stall: Duration::from_secs(600),
            timeout: Duration::from_secs(600),
            progress: None,
        };
        let missing = req(vec![Some("opl-nope".into()), None]);
        assert!(src.catch_up_disks(vm.id, &missing).await.is_err());
        src.fail_next_catch_up(DriverError::Migration("Connection reset by peer".into()));
        let good = req(vec![Some("opl-repl-a".into()), None]);
        assert!(src.catch_up_disks(vm.id, &good).await.is_err());
        assert_eq!(src.catching_up(vm.id), None);

        src.set_catch_up_passes(2);
        let state = src.catch_up_disks(vm.id, &good).await.unwrap();
        assert_eq!(
            state,
            CatchUpState {
                disks: vec![0, 1],
                bitmap: migration_bitmap(task, 2),
                passes: 2
            }
        );
        let calls = src.catch_ups();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[2].done,
            Some((
                2,
                MOCK_INCREMENT_BYTES + MOCK_MIRROR_BYTES + 2 * MOCK_INCREMENT_BYTES
            ))
        );
        assert!(
            src.bitmaps(vm.id)[1]
                .iter()
                .any(|b| b.name == state.bitmap && !b.persistent)
        );

        // 스위치오버 오류는 마이그레이션을 실패시키고 따라잡기를 남기지 않는다. 되돌리기는 임시 비트맵을 지운다.
        let switchover = MigrateOptions {
            switchover_catch_up: Some(state.clone()),
            devices: None,
            ..opts()
        };
        src.fail_next_switchover("Input/output error");
        let err = src
            .migrate(vm.id, &target(), &switchover)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("opl-catchup-0"), "{err}");
        assert!(src.migrate(vm.id, &target(), &switchover).await.is_err());
        src.cancel_migration(vm.id).await.unwrap();
        assert!(
            src.bitmaps(vm.id)
                .iter()
                .flatten()
                .all(|b| !b.name.starts_with("opl-mig-"))
        );

        src.catch_up_disks(vm.id, &good).await.unwrap();
        src.migrate(vm.id, &target(), &switchover).await.unwrap();
        assert_eq!(src.switchovers(), [vm.id]);
        assert_eq!(src.catching_up(vm.id), None);
        dst.incoming_wait(vm.id).await.unwrap();
        dst.finish_incoming(vm.id, Some("opl-repl-back"))
            .await
            .unwrap();
        let back = dst.bitmaps(vm.id);
        assert!(
            back[0]
                .iter()
                .any(|b| b.name == "opl-repl-back" && b.persistent)
        );
        assert!(
            back[1]
                .iter()
                .any(|b| b.name == "opl-repl-back" && !b.persistent)
        );
    }

    /// 미러 실패 주입: 복사 실패·정체는 미러를 남기지 않고, 스위치오버의 미러 오류는 마이그레이션을
    /// 실패시키고 원본은 돈다. 되돌리기(`cancel_migration`)와 VM 끄기는 남은 미러를 치운다.
    #[tokio::test]
    async fn mirror_failures_leave_the_source_running() {
        let hv = MockHypervisor::default();
        let vm = spec_with_disks();
        let targets = [mirror_target(1, ImageFormat::Raw)];
        let (mirror, _rx) = mirror_opts();
        assert!(matches!(
            hv.mirror_disks(vm.id, &targets, &mirror).await,
            Err(DriverError::NotRunning(_))
        ));
        hv.start(&vm, None).await.unwrap();

        hv.fail_next_mirror(DriverError::Migration("Connection reset by peer".into()));
        assert!(hv.mirror_disks(vm.id, &targets, &mirror).await.is_err());
        assert!(hv.mirroring(vm.id).is_empty());
        hv.stall_next_mirror();
        let err = hv.mirror_disks(vm.id, &targets, &mirror).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "migration failed: disk copy made no progress for 10m"
        );
        assert!(hv.mirroring(vm.id).is_empty());
        assert_eq!(hv.mirrors().len(), 3);

        let switchover = MigrateOptions {
            switchover_mirrors: true,
            switchover_catch_up: None,
            devices: None,
            ..opts()
        };
        hv.mirror_disks(vm.id, &targets, &mirror).await.unwrap();
        hv.fail_next_switchover("Input/output error");
        let err = hv.migrate(vm.id, &target(), &switchover).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "migration failed: disk mirror opl-mirror-1 failed: Input/output error"
        );
        assert!(hv.mirroring(vm.id).is_empty());
        assert!(hv.switchovers().is_empty());
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // 메모리 단계가 실패하면 미러가 남고, 되돌리기가 치운다.
        hv.mirror_disks(vm.id, &targets, &mirror).await.unwrap();
        hv.fail_next_migrate(DriverError::Migration("Connection refused".into()));
        assert!(hv.migrate(vm.id, &target(), &switchover).await.is_err());
        assert_eq!(hv.mirroring(vm.id), [1]);
        hv.cancel_migration(vm.id).await.unwrap();
        assert!(hv.mirroring(vm.id).is_empty());

        hv.mirror_disks(vm.id, &targets, &mirror).await.unwrap();
        hv.clear_backup_leftovers(vm.id).await.unwrap();
        assert!(hv.mirroring(vm.id).is_empty());
        hv.mirror_disks(vm.id, &targets, &mirror).await.unwrap();
        hv.crash(vm.id);
        assert!(hv.mirroring(vm.id).is_empty());
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a, None).await.unwrap();
        hv.start(&b, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.crash(a.id);
        hv.guest_shutdown(b.id);
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: a.id,
                kind: EventKind::Crashed
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: b.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.list().await.unwrap().is_empty());
    }

    fn disk(format: ImageFormat) -> RuntimeDisk {
        RuntimeDisk {
            path: "/pools/a/d.qcow2".into(),
            kind: opl_types::runtime::DiskKind::File,
            rbd: None,
            format,
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        }
    }

    /// 핫플러그 배치로 켠 VM: 켤 때 vCPU 2개·실행 중 붙인 vCPU 하나, virtio-mem 3072 MiB 중 1024 꽂음, 디스크 둘
    /// (둘째는 실행 중 붙여 `hp2`), NIC 하나
    fn hot_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            cpu: CpuSpec {
                flags: Vec::new(),
                sockets: 1,
                cores: 2,
                model: CpuModel::Max,
                max_vcpus: Some(8),
            },
            memory_mib: 1024,
            disks: vec![disk(ImageFormat::Qcow2), disk(ImageFormat::Qcow2)],
            nics: vec![RuntimeNic {
                mac: "52:54:00:12:34:56".parse().unwrap(),
                model: opl_types::vm::NicModel::Virtio,
                backend: opl_types::runtime::NicBackend::User,
            }],
            hotplug: Some(opl_types::runtime::HotplugLayout {
                vcpus: 3,
                max_memory_mib: 4096,
                plugged_mib: 1024,
                ports: opl_types::runtime::HOTPLUG_PORTS,
                boot_disks: 1,
                boot_nics: 1,
                disk_ports: vec![2],
                nic_ports: vec![],
            }),
            ..spec()
        }
    }

    /// 켤 때 배치에서 vCPU·virtio-mem·장치를 정하고 핫플러그 호출이 실제 QEMU처럼 바꾼다(켤 때 단 장치는 뗄 수 없고,
    /// 쓰는 포트에는 붙일 수 없다). 끄면 사라진다.
    #[tokio::test]
    async fn hotplug_state_starts_from_the_layout_and_follows_the_calls() {
        let hv = MockHypervisor::default();
        let vm = hot_spec();
        let t = Duration::from_millis(1);
        hv.start(&vm, None).await.unwrap();
        assert_eq!(hv.launch_specs(), std::slice::from_ref(&vm));
        assert_eq!(hv.vcpus(vm.id), Some(3));
        assert_eq!(hv.plugged_mib(vm.id), Some(1024));
        assert_eq!(hv.devices(vm.id), ["disk0-dev", "disk1-dev", "nic0"]);

        assert_eq!(
            hv.set_vcpus(vm.id, &vm.cpu, 3, 5, t).await.unwrap().reached,
            5
        );
        assert!(hv.set_vcpus(vm.id, &vm.cpu, 5, 1, t).await.is_err());
        assert_eq!(hv.set_memory(vm.id, 3072, t).await.unwrap(), 3072);
        assert!(hv.set_memory(vm.id, 3074, t).await.is_err());
        // 포트 2는 실행 중 붙인 디스크가 쓴다.
        assert!(
            hv.add_disk(vm.id, 2, &disk(ImageFormat::Raw), 2)
                .await
                .is_err()
        );
        hv.add_disk(vm.id, 2, &disk(ImageFormat::Raw), 0)
            .await
            .unwrap();
        assert!(hv.remove_disk(vm.id, 0, t).await.is_err());
        hv.add_nic(vm.id, 1, &vm.nics[0], 1).await.unwrap();
        assert_eq!(
            hv.devices(vm.id),
            ["disk0-dev", "disk1-dev", "disk2-dev", "nic0", "nic1"]
        );
        assert_eq!(hv.backup_bitmaps(vm.id).await.unwrap().len(), 3);

        // 게스트가 내려 주지 않으면 떼기·줄이기는 그대로다(늘리기는 된다).
        hv.hold_unplug(vm.id, true);
        assert!(!hv.remove_disk(vm.id, 2, t).await.unwrap());
        assert!(!hv.remove_nic(vm.id, 1, t).await.unwrap());
        assert_eq!(
            hv.set_vcpus(vm.id, &vm.cpu, 5, 2, t).await.unwrap().reached,
            5
        );
        assert_eq!(hv.set_memory(vm.id, 0, t).await.unwrap(), 3072);
        assert_eq!(hv.devices(vm.id).len(), 5);
        hv.hold_unplug(vm.id, false);
        assert!(hv.remove_disk(vm.id, 2, t).await.unwrap());
        assert!(hv.remove_nic(vm.id, 1, t).await.unwrap());
        // 이미 없는 장치는 뗀 것이다.
        assert!(hv.remove_nic(vm.id, 1, t).await.unwrap());
        assert_eq!(
            hv.set_vcpus(vm.id, &vm.cpu, 5, 2, t).await.unwrap().reached,
            2
        );
        assert_eq!(hv.backup_bitmaps(vm.id).await.unwrap().len(), 2);

        // 주입한 실패는 아무것도 바꾸지 않는다.
        hv.fail_next_hotplug("boom");
        let err = hv.add_nic(vm.id, 1, &vm.nics[0], 1).await.unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
        assert_eq!(hv.devices(vm.id), ["disk0-dev", "disk1-dev", "nic0"]);
        assert_eq!(hv.hotplug_calls().len(), 17);
        assert_eq!(
            hv.hotplug_calls()[0],
            HotplugCall::SetVcpus {
                id: vm.id,
                from: 3,
                to: 5
            }
        );

        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.vcpus(vm.id), None);
        assert!(hv.devices(vm.id).is_empty());
        assert!(matches!(
            hv.add_nic(vm.id, 1, &vm.nics[0], 1).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    /// 배치 없이 켠 VM은 vCPU가 `sockets × cores`이고 virtio-mem·루트 포트가 없다. 받는 쪽 스펙도 남는다.
    /// 이미 붙은 장치는 그대로 두고 `AlreadyPresent`다. 보내기 전 확인은 디스크·NIC 장치만 견준다.
    #[tokio::test]
    async fn present_devices_and_the_device_check() {
        let hv = MockHypervisor::default();
        let vm = hot_spec();
        hv.start(&vm, None).await.unwrap();
        let ids = |l: &[&str]| l.iter().map(|d| d.to_string()).collect::<Vec<_>>();
        hv.check_devices(vm.id, &ids(&["disk0-dev", "disk1-dev", "nic0", "cpu2"]))
            .await
            .unwrap();
        assert!(matches!(
            hv.check_devices(vm.id, &ids(&["disk0-dev", "nic0"])).await,
            Err(DriverError::Migration(_))
        ));
        let added = hv.add_disk(vm.id, 2, &disk(ImageFormat::Qcow2), 0).await;
        assert_eq!(added.unwrap(), AddOutcome::Added);
        let again = hv.add_disk(vm.id, 2, &disk(ImageFormat::Qcow2), 1).await;
        assert_eq!(again.unwrap(), AddOutcome::AlreadyPresent);
        assert_eq!(hv.devices(vm.id).len(), 4);
    }

    #[tokio::test]
    async fn a_vm_without_a_layout_has_nothing_to_hotplug() {
        let hv = MockHypervisor::default();
        let vm = VmRuntimeSpec {
            hotplug: None,
            ..hot_spec()
        };
        hv.start(&vm, None).await.unwrap();
        assert_eq!(hv.vcpus(vm.id), Some(2));
        assert_eq!(hv.plugged_mib(vm.id), Some(0));
        let t = Duration::from_millis(1);
        let err = hv.set_memory(vm.id, 128, t).await.unwrap_err();
        assert!(err.to_string().contains("vmem0-dev"), "{err}");
        assert!(
            hv.add_disk(vm.id, 2, &disk(ImageFormat::Raw), 0)
                .await
                .is_err()
        );
        assert_eq!(
            hv.hotplug_caps().await,
            NodeHotplug {
                cpu: true,
                memory: true
            }
        );
        hv.set_hotplug_caps(NodeHotplug::default());
        assert_eq!(hv.hotplug_caps().await, NodeHotplug::default());

        let incoming = hot_spec();
        hv.start_incoming(&incoming, Uuid::now_v7(), "127.0.0.1", 1..=2, &[])
            .await
            .unwrap();
        assert_eq!(hv.incoming_specs(), std::slice::from_ref(&incoming));
        assert_eq!(hv.vcpus(incoming.id), Some(3));
    }

    /// CD-ROM 드라이브 둘(하나는 빈)과 씨앗을 단 VM: 매체·씨앗을 기록하고 장치 목록·장치 확인에 드러낸다. 매체를
    /// 바꾸고 빼면 기록이 바뀌고, 없는 드라이브는 오류다. 꺼지면 사라진다.
    #[tokio::test]
    async fn cdrom_media_and_the_seed_are_recorded() {
        use opl_types::runtime::RuntimeCdrom;

        let hv = MockHypervisor::default();
        let seed = CloudInitSeed {
            instance_id: "iid-1".into(),
            meta_data: "m".into(),
            user_data: "u".into(),
            vendor_data: None,
            network_config: None,
        };
        let medium = |p: &str| RuntimeMedium { path: p.into() };
        let vm = VmRuntimeSpec {
            cdroms: vec![
                RuntimeCdrom {
                    medium: Some(medium("/pools/iso/a.raw")),
                    boot_order: Some(2),
                },
                RuntimeCdrom {
                    medium: None,
                    boot_order: None,
                },
            ],
            cloud_init: Some(seed.clone()),
            ..spec()
        };
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.media(vm.id),
            [Some(PathBuf::from("/pools/iso/a.raw")), None]
        );
        assert_eq!(hv.seed(vm.id), Some(seed));
        assert_eq!(hv.devices(vm.id), ["cd0-dev", "cd1-dev", "cidata-dev"]);
        let ids = |l: &[&str]| l.iter().map(|d| d.to_string()).collect::<Vec<_>>();
        hv.check_devices(vm.id, &ids(&["cd0-dev", "cd1-dev", "cidata-dev"]))
            .await
            .unwrap();
        assert!(
            hv.check_devices(vm.id, &ids(&["cd0-dev", "cd1-dev"]))
                .await
                .is_err()
        );

        hv.change_medium(vm.id, 1, Some(&medium("/pools/iso/b.raw")))
            .await
            .unwrap();
        hv.change_medium(vm.id, 0, None).await.unwrap();
        assert_eq!(
            hv.media(vm.id),
            [None, Some(PathBuf::from("/pools/iso/b.raw"))]
        );
        let err = hv.change_medium(vm.id, 2, None).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        assert_eq!(
            hv.hotplug_calls()[1],
            HotplugCall::ChangeMedium {
                id: vm.id,
                index: 0,
                medium: None
            }
        );

        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert!(hv.media(vm.id).is_empty());
        assert_eq!(hv.seed(vm.id), None);
        assert!(hv.change_medium(vm.id, 0, None).await.is_err());
    }
}
