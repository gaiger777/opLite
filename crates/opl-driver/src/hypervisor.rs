use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
pub use opl_qemu::BitmapInfo;
pub use opl_qemu::CpuProbe;
use opl_types::guest::GuestInfo;
use opl_types::node::{FirmwareCaps, NodeHotplug};
use opl_types::runtime::{RuntimeDisk, RuntimeMedium, RuntimeNic, RuntimePinning, VmRuntimeSpec};
use opl_types::vm::{CpuSpec, RunningHardware};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::DriverError;
use crate::migration::{
    CatchUp, CatchUpState, IncomingPorts, MigrateOptions, MigrateTarget, MirrorOptions,
    MirrorTarget,
};
use crate::replication::{ReceiverExport, ReplicateRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// 게스트에 ACPI 전원 버튼을 보낸다. 게스트가 무시할 수 있다.
    Acpi,
    /// 게스트 에이전트에 `guest-shutdown`(powerdown)을 보낸다(계획 59). 성공하면 에이전트가 답하지 않으므로 시간
    /// 초과는 보낸 것으로 본다. 채널이 없거나, 연결하지 못하거나, 동기화되지 않거나, 오류로 답하면 곧바로 `Acpi`로
    /// 넘어간다. `Acpi`처럼 요청만 보내고 돌아온다.
    Agent,
    /// 프로세스를 즉시 끝낸다.
    Force,
    /// QMP 없이 곧바로 SIGKILL한다. I/O에 묶여 QMP에 답하지 못하는 QEMU에 쓴다(HA 검사의 다른 노드에서 다시 켜기).
    Kill,
}

/// 프로세스가 살아 있는 VM의 상태
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualState {
    pub id: Uuid,
    pub pid: u32,
    pub status: RuntimeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    Running,
    /// 기동 직후 `cont` 전이거나 일시정지된 상태
    Paused,
    /// QMP를 오래 붙잡는 작업(메모리 스냅샷 저장·불러오기, 스냅샷 지우기)이 도는 중이다. QEMU는 그
    /// 작업이 끝날 때까지 QMP에 답하지 않으므로 상태를 묻지 않고 이것을 알린다. 값은 작업 이름이다.
    /// 응답 없음(`Other("unresponsive")`)과 달리 다시 붙거나 끄지 않고, 다른 QMP 요청도 보내지 않는다.
    Busy(&'static str),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HypervisorEvent {
    pub id: Uuid,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Started,
    /// 정상 종료
    Stopped {
        /// 게스트가 스스로 꺼진 경우 true다. opld가 요청한 ACPI 종료도 게스트가 수행하므로
        /// true로 보고된다. 이 값만 보고 spec을 바꾸지 말고 현재 원하는 상태와 비교해야 한다.
        guest_initiated: bool,
    },
    /// 종료 이벤트 없이 프로세스가 사라졌다.
    Crashed,
    /// 게스트가 다시 부팅했거나(QMP `RESET`) 꺼지기 시작했다(QEMU가 살아 있는 채 받은 `SHUTDOWN`). 게스트 에이전트가
    /// 한동안 답하지 않는다. 게스트 에이전트 채널을 단 VM만 알린다.
    Reset,
    /// VM이 멈췄거나(QMP `STOP`) 다시 돌기 시작했다(`RESUME`). 게스트 에이전트 채널을 단 VM만 알린다.
    PauseChanged,
}

impl EventKind {
    /// QEMU가 끝나지 않는 이벤트(게스트 감시만 쓴다)
    pub fn is_guest(self) -> bool {
        matches!(self, EventKind::Reset | EventKind::PauseChanged)
    }
}

/// 실행 중인 VM의 디스크 노드 하나와 그 dirty bitmap들
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskBitmaps {
    /// 디스크 노드 이름(`disk{i}`)
    pub node: String,
    pub bitmaps: Vec<BitmapInfo>,
}

/// 증분 백업([`Hypervisor::incremental_backup`])이 디스크 하나를 쓸 대상
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupTarget {
    /// 디스크 노드 이름(`disk{i}`)
    pub node: String,
    /// 미리 만들어 둔 backing 없는 빈 qcow2 파일(절대 경로)
    pub path: PathBuf,
    /// 그 파일의 가상 크기(디스크와 같다)
    pub size: u64,
}

/// 실행 중인 VM의 모든 디스크(`disk{i}`)를 합한 I/O 누적 카운터(게스트가 본 것, QMP `query-blockstats`).
/// QEMU를 다시 켜면 0부터 다시 센다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskCounters {
    pub rd_bytes: u64,
    pub wr_bytes: u64,
    pub rd_operations: u64,
    pub wr_operations: u64,
}

/// vCPU 바꾸기([`Hypervisor::set_vcpus`])의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcpuChange {
    /// 닿은 켜진 vCPU 수
    pub reached: u16,
    /// QEMU가 vCPU 떼기 요청을 거절했다(게스트의 ACPI가 vCPU 떼기를 켜지 않음 — QEMU 8.2 q35의 옛 CPU 핫플러그
    /// 방식). 진행 중인 떼기는 없다.
    pub refused: bool,
}

/// 시점을 찍는 동작(`quiesce` 인자)에서 게스트 파일 시스템을 얼린 결과(계획 59)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Quiesced {
    /// 얼리지 않았다(요청하지 않음, 메모리 스냅샷).
    #[default]
    No,
    /// 시점을 고정하는 QMP 직전에 얼리고 직후에 녹였다.
    Yes,
    /// 얼리려 했지만 못 했다(이유). 시점은 crash-consistent다.
    Skipped(String),
}

impl Quiesced {
    /// 얼리지 못했을 때 Task에 남길 경고
    pub fn warning(&self) -> Option<String> {
        match self {
            Quiesced::Skipped(why) => Some(format!(
                "guest agent did not freeze file systems ({why}); the snapshot is crash-consistent"
            )),
            Quiesced::No | Quiesced::Yes => None,
        }
    }
}

/// 실행 중 붙이기([`Hypervisor::add_disk`]·[`Hypervisor::add_nic`])의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// 새로 붙였다.
    Added,
    /// 같은 id의 장치가 이미 붙어 있었다(앞서 중단된 Task가 붙였다). 아무것도 바꾸지 않았다.
    AlreadyPresent,
}

/// VM 프로세스 수명을 관리한다. 같은 VM에 대한 호출을 직렬화하는 것은 호출자(reconciler)의 책임이다.
#[async_trait]
pub trait Hypervisor: Send + Sync {
    /// VM을 시작한다. 이미 실행 중이면 아무것도 하지 않고 성공한다.
    ///
    /// `restore`가 있으면 새로 띄운 VM을 실행하기 전에 그 태그의 내부 스냅샷(디스크와 메모리,
    /// 메모리는 첫 디스크)을 불러온다. 불러오지 못하면 VM을 끝내고 오류를 돌려준다.
    /// 이미 실행 중인 VM에는 쓰지 않는다. 새로 띄운 VM의 inconsistent인 `opl-` dirty bitmap(QEMU가
    /// 비정상으로 끝나 이미지에 남은 것)은 지운다.
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError>;

    /// `Acpi`는 요청만 보내고 바로 돌아온다. `Force`는 프로세스가 끝날 때까지 기다린다.
    /// 실행 중이 아니면 아무것도 하지 않고 성공한다.
    /// `Force`에서 프로세스가 응답하지 않아 강제로 죽여야 했다면 종료 이벤트가 없으므로
    /// `Stopped` 대신 `Crashed`로 보고된다.
    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError>;

    /// 실행 중이 아니면 `None`
    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError>;

    async fn list(&self) -> Result<Vec<ActualState>, DriverError>;

    /// VNC 유닉스 소켓 경로
    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError>;

    /// 실행 중인 VM의 모든 디스크에 내부 스냅샷 `tag`를 만든다. `memory`면 메모리 상태도
    /// 첫 디스크에 담는다(저장하는 동안 VM이 멈춘다). 아니면 VM을 멈추지 않고 디스크만 찍는다.
    /// `quiesce`면(메모리 없을 때만) 찍기 직전에 게스트 에이전트로 파일 시스템을 얼리고 직후에 녹인다. 얼리지
    /// 못해도 찍고 `Skipped`를 돌려준다.
    async fn snapshot_save(
        &self,
        id: Uuid,
        tag: &str,
        memory: bool,
        quiesce: bool,
    ) -> Result<Quiesced, DriverError>;

    /// 메모리 없이 실행 중인 VM의 모든 디스크에 내부 스냅샷 `tag`와 dirty bitmap `bitmap`을 한 시점에
    /// 만든다(transaction 하나). 비트맵은 qcow2 디스크면 이미지에 저장된다(persistent). `quiesce`는
    /// [`snapshot_save`](Self::snapshot_save)와 같다.
    async fn snapshot_save_with_bitmap(
        &self,
        id: Uuid,
        tag: &str,
        bitmap: &str,
        quiesce: bool,
    ) -> Result<Quiesced, DriverError>;

    /// 실행 중인 VM의 디스크 노드(`disk{i}`)마다 dirty bitmap들을 디스크 순서로 읽는다.
    async fn backup_bitmaps(&self, id: Uuid) -> Result<Vec<DiskBitmaps>, DriverError>;

    /// 실행 중인 VM의 모든 디스크에 dirty bitmap `name`을 한 시점에 만든다(qcow2면 persistent).
    /// VM을 멈춘 동안 블록 풀 스냅샷과 같은 시점을 잡는 데 쓴다.
    async fn add_bitmaps(&self, id: Uuid, name: &str) -> Result<(), DriverError>;

    /// 실행 중인 VM의 모든 디스크에서 백업 비트맵([`opl_types::backup::is_backup_bitmap`]) 중 `keep`이 아닌 것을
    /// 모두 지운다. 같은 `opl-` 무리의 복제·마이그레이션 비트맵은 남긴다.
    async fn remove_bitmaps(&self, id: Uuid, keep: Option<&str>) -> Result<(), DriverError>;

    /// 실행 중인 VM의 모든 디스크에서 이름이 `names`에 든 dirty bitmap을 지운다. 없는 것은 건너뛴다.
    async fn remove_named_bitmaps(&self, id: Uuid, names: &[String]) -> Result<(), DriverError>;

    /// 실행 중인 VM의 디스크마다 비트맵 `parent` 뒤에 바뀐 블록만 `targets`(디스크 순서)에 압축해
    /// 쓴다. 쓰기 시작하는 시점에 모든 디스크에 새 비트맵 `new`를 만든다(transaction 하나). 작업이
    /// 하나라도 실패하면 나머지를 취소한다. 대상 노드는 끝나면 닫는다(닫지 못하면 대상 파일이 온전하지
    /// 않으므로 실패). 성공하면 `parent`를 지우고, 실패하면 `new`를 지워 `parent`만 남긴다(대상 파일은
    /// 호출자가 지운다). `quiesce`면 작업을 시작하는 transaction 앞뒤로 게스트 파일 시스템을 얼리고 녹인다.
    async fn incremental_backup(
        &self,
        id: Uuid,
        parent: &str,
        new: &str,
        targets: &[BackupTarget],
        quiesce: bool,
    ) -> Result<Quiesced, DriverError>;

    /// 실행 중인 VM에 앞선 증분 백업이 남긴 작업과 대상 노드(`opl-backup-*`, opld가 기다리다 죽었다)와
    /// 남은 디스크 미러(`opl-mirror-*`), 복제 회차의 작업과 NBD 노드(`opl-repl-*`)를 치우고, 남은 마이그레이션
    /// 따라잡기 비트맵(`opl-mig-*`)을 지운다. 아직 도는 작업은 취소하고 잠시 기다린다(끝나지 않으면 오류). 남은
    /// 작업이 있으면 디스크의 내부 스냅샷이 실패한다. 복제 회차가 남긴 플리싱(`opl-fl-*` 노드와 VM 상태 디렉터리의
    /// 플리싱 이미지)도 뗀다.
    async fn clear_backup_leftovers(&self, id: Uuid) -> Result<(), DriverError>;

    /// 실행 중인 VM의 디스크들을 `req.targets`(받는 QEMU의 NBD 내보내기)에 한 시점으로 복제하고 보낸 바이트를
    /// 돌려준다. 앞선 회차가 남긴 작업과 NBD 노드(`opl-repl-*`)와 플리싱을 먼저 치우고, TLS 자격 증명(보내는 쪽,
    /// 상대 인증서 확인)을 만든 뒤 대상마다 NBD 노드를 연다. 그다음 대상 디스크마다 플리싱을 붙인다: VM 상태
    /// 디렉터리의 성긴 플리싱 이미지(남은 공간이 디스크 크기 합의 10% 이상이어야), 게스트를 잠시 멈춘 채 새 비트맵
    /// `req.bitmap`(qcow2면 persistent)을 만들고 디스크 위에 copy-before-write 필터(`on-cbw-error: break-snapshot`)를
    /// 끼운 뒤, 그 시점의 디스크를 읽는 snapshot-access 노드에서 `blockdev-backup`(기준 비트맵이 있으면 그 뒤 바뀐
    /// 블록만, 없으면 전체)을 시작한다 — 받는 쪽이 사라져 원격 쓰기가 실패해도 게스트 쓰기는 성공하고 회차만
    /// 실패한다. 모두 끝나면 작업을 지우고 플리싱을 떼고(이미지 지움) NBD 노드를 닫는다(닫을 때 받는 쪽에 쓴 것을
    /// 내보내므로, 닫지 못하면 실패). 작업이 실패하거나 `req.stall` 동안 보낸 양이 늘지 않으면 나머지를 취소하고,
    /// 플리싱을 떼고, 노드를 닫고, 새 비트맵을 지워 실패한다. 기준 비트맵은 성공해도 실패해도 지우지 않는다(호출자가
    /// 고른다). `req.quiesce`면 게스트를 멈추기 직전에 파일 시스템을 얼리고 이어서 실행한 직후에 녹인다(전송 내내
    /// 얼리지 않는다). 보낸 바이트와 얼림 결과를 돌려준다.
    async fn replicate_disks(
        &self,
        id: Uuid,
        req: &ReplicateRequest,
    ) -> Result<(u64, Quiesced), DriverError>;

    /// 복제 회차 `run`을 받을 QEMU를 게스트 장치 없이(`-machine none`, opld의 자식, 실행 디렉터리
    /// `{run_dir}/replica-{run}`의 pid 파일) 띄우고, `exports`마다 블록 노드(겹층이 있으면 복제본을 backing으로 둔
    /// 겹층)를 열어 `ports`에서 고른 빈 포트의 TLS NBD 서버(받는 쪽, 상대 인증서 확인)에 쓰기 가능하게 내보낸다.
    /// 연 포트를 돌려준다. 실패하면 띄운 QEMU를 끄고 실행 디렉터리를 지운다. 같은 회차의 받는 QEMU가 이미 살아
    /// 있으면 실패한다.
    async fn start_replica_receiver(
        &self,
        run: Uuid,
        exports: &[ReceiverExport],
        listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError>;

    /// 회차 `run`의 받는 QEMU를 끝낸다: 내보내기를 닫고, NBD 서버를 멈추고, 블록 노드를 닫아(쓴 것을 내보냄)
    /// `quit`한 뒤 끝나기를 기다린다. 하나라도 실패하면 오류다(받는 QEMU는 남을 수 있다 —
    /// [`stop_replica_receiver`](Self::stop_replica_receiver)로 끈다). 받는 QEMU가 없으면 오류다.
    async fn finish_replica_receiver(&self, run: Uuid) -> Result<(), DriverError>;

    /// 회차 `run`의 받는 QEMU를 SIGKILL로 끄고 실행 디렉터리를 지운다(회차를 버린다). 없으면 성공이다.
    async fn stop_replica_receiver(&self, run: Uuid) -> Result<(), DriverError>;

    /// 살아 있는 받는 QEMU의 회차들(실행 디렉터리의 pid 파일로). 끝난 회차의 받는 QEMU를 찾아 끄는 데 쓴다.
    async fn replica_receivers(&self) -> Result<Vec<Uuid>, DriverError>;

    /// 실행 중인 VM의 모든 디스크에서 내부 스냅샷 `tag`를 지운다. 태그가 없어도 성공한다.
    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError>;

    /// 실행 중인 VM의 `disk`번째 디스크(스펙의 `disks` 순서) 크기를 바꾼다. 게스트가 바로 본다.
    /// inconsistent인 `opl-` dirty bitmap이 있으면 먼저 지운다(있으면 크기를 바꿀 수 없다).
    async fn block_resize(&self, id: Uuid, disk: usize, size_bytes: u64)
    -> Result<(), DriverError>;

    /// 실행 중인 VM을 멈춘다(QMP `stop`). 돌아오면 게스트의 블록 I/O가 끝나 있으므로 디스크를
    /// 바깥에서 찍을 수 있다. 반드시 `resume`으로 이어서 실행한다. 그러지 못한 채 opld가 끝나면
    /// 다음에 다시 붙을 때(`start`) 이어서 실행한다.
    async fn pause(&self, id: Uuid) -> Result<(), DriverError>;

    /// `pause`로 멈춘 VM을 이어서 실행한다(QMP `cont`). 실행 중이어도 성공한다.
    async fn resume(&self, id: Uuid) -> Result<(), DriverError>;

    /// 이미 붙어 있는 VM이 멈춘 채 남았으면(기동 도중 `-S`, 표시가 남은 `pause`) `start`처럼
    /// 이어서 실행하고 true. 지금 `pause`해 둔 VM과 QEMU가 스스로 멈춘 VM은 그대로 두고 true다.
    /// 붙어 있지 않으면 아무것도 하지 않고 false(다시 붙는 것은 `start`가 한다). 디스크를 다시
    /// 해석하지 않으므로 저장소가 응답하지 않아도 된다.
    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError>;

    /// 이 노드가 새로 띄울 QEMU의 버전(`major.minor.micro`). 받는 쪽이 원본과 같은지 본다.
    async fn qemu_version(&self) -> Result<String, DriverError>;

    /// 실행 중인 VM의 QEMU 버전(`major.minor.micro`, QMP `query-version`). 바이너리를 바꾼 뒤에도
    /// 이미 떠 있는 QEMU의 버전이다.
    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError>;

    /// 실행 중인 VM의 모든 디스크(`disk{i}`)를 합한 I/O 누적 카운터. 지표 측정기가 직전 값과의
    /// 차이로 비율을 낸다.
    async fn disk_stats(&self, id: Uuid) -> Result<DiskCounters, DriverError>;

    /// 실행 중인 VM의 게스트 에이전트에 `guest-ping`을 보낸다(QGA 채널, 3초). 답하면 true, 답이 없으면
    /// false(게스트 에이전트가 없거나 멈춤). 채널 없이 띄운 VM(`guest_agent`가 아님)이면 오류다.
    async fn guest_ping(&self, id: Uuid) -> Result<bool, DriverError>;

    /// 실행 중인 VM의 게스트 에이전트에서 게스트 정보(에이전트 버전, OS, 호스트 이름, NIC/IP, 파일 시스템)를 모은다
    /// (계획 59). 오류로 답한 명령의 칸은 비운다. 결과는 상한으로 자른다. 채널이 없으면 `InvalidSpec`, 에이전트가
    /// 동기화에 답하지 않으면 `GuestAgent`다.
    async fn guest_info(&self, id: Uuid) -> Result<GuestInfo, DriverError>;

    /// 게스트 사용자 `user`의 비밀번호를 바꾼다(`guest-set-user-password`). 비밀번호는 기록하지 않고 오류에도 넣지
    /// 않는다. 에이전트가 답하지 않거나 오류로 답하면 `GuestAgent`다.
    async fn set_user_password(
        &self,
        id: Uuid,
        user: &str,
        password: &str,
    ) -> Result<(), DriverError>;

    /// 라이브 마이그레이션을 받을 QEMU를 `-incoming defer`로 띄우고, 받는 중 표시(마이그레이션 Task
    /// `task`)를 남기고, TLS
    /// 자격 증명(받는 쪽, 상대 인증서 확인)과 능력을 정한 뒤 `ports`에서 빈 포트로 `listen_host`에서
    /// 받기 시작한다. `exports`(디스크 순서 번호)가 있으면 그 디스크들을 `ports`의 다른 빈 포트에서
    /// TLS NBD 서버로 쓰기 가능하게 내보낸다(이름 `disk{i}`, 원본이 미러한다). 연 포트를 돌려준다.
    /// 실패하면 띄운 QEMU를 끈다. 이미 이 노드에서 돌고 있는 VM이면 실패한다. 받은 뒤에도 멈춰
    /// 있다([`finish_incoming`](Self::finish_incoming)이 이어서 실행).
    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        task: Uuid,
        listen_host: &str,
        ports: RangeInclusive<u16>,
        exports: &[usize],
    ) -> Result<IncomingPorts, DriverError>;

    /// 받는 중인 VM의 마이그레이션이 끝나기를 기다린다. 완료면 성공, 실패했거나 받는 QEMU가
    /// 사라졌으면 `Migration` 오류다. 받는 중 표시가 없는 VM이면 실패한다.
    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError>;

    /// 받기를 마친 VM을 넘겨받는다: NBD 내보내기를 멈추고, 멈춰 있으면 이어서 실행하고 받는 중 표시를 지운다. `bitmap`이
    /// 있으면 이어서 실행하기 전에 모든 디스크에 그 이름의 dirty bitmap(qcow2면 persistent)을 만든다(복제본으로 옮긴
    /// VM이 옛 원본으로 뒤집힌 복제의 기준, 계획 55 — 만들지 못하면 경고만 남긴다). 표시가 없으면(이미 넘겨받음)
    /// 아무것도 하지 않는다. 아직 받는 중이면 `Migration` 오류다. 마이그레이션이 확정된 뒤(VM이 이 노드로 옮겨진 뒤)에만
    /// 부른다.
    async fn finish_incoming(&self, id: Uuid, bitmap: Option<&str>) -> Result<(), DriverError>;

    /// 받는 중 표시가 있는 살아 있는 VM과 그 표시의 마이그레이션 Task(읽을 수 없으면 nil). 확정되지
    /// 않은 채 남은 받는 QEMU를 찾아 끄는 데 쓴다.
    async fn list_incoming(&self) -> Result<Vec<(Uuid, Uuid)>, DriverError>;

    /// 실행 중인 VM의 디스크들을 `targets`(받는 QEMU의 NBD 내보내기)로 미러하기 시작하고 모두 `ready`
    /// (한 번 다 복사하고 원본의 쓰기를 따라감)가 되기를 기다린다. 미러는 그대로 남아
    /// [`migrate`](Self::migrate)의 `switchover_mirrors`가 끝낸다. 남은 미러는 먼저 치운다. qcow2 디스크를
    /// raw로 옮기면 미러 전에 그 디스크의 `opl-` 비트맵을 지운다. 작업이 실패하거나, `opts.timeout`이
    /// 지나거나, `opts.stall` 동안 복사량이 늘지 않으면 미러를 모두 치우고 `Migration` 오류다.
    async fn mirror_disks(
        &self,
        id: Uuid,
        targets: &[MirrorTarget],
        opts: &MirrorOptions,
    ) -> Result<(), DriverError>;

    /// 실행 중인 VM의 디스크들을 `req.targets`(받는 QEMU가 내보낸 대상 노드의 복제본, 계획 55)로 따라잡게 한다: 패스마다
    /// 복제 회차처럼 플리싱을 붙여(그 시점에 새 임시 비트맵 `opl-mig-{task}-{k}`, 저장하지 않음 — 원격 쓰기가 실패해도
    /// 게스트 쓰기는 성공한다) 그 시점의 디스크에서 `blockdev-backup`(첫 패스는 `req.base`의 복제 비트맵 뒤 바뀐 블록,
    /// 기준이 없으면 전체, 그다음은 앞 패스의 비트맵 뒤 바뀐 블록)을 시작하고 끝나기를 기다린 뒤 플리싱을 떼고 앞 임시
    /// 비트맵을 지운다(복제 비트맵은 그대로 둔다). 새 비트맵이 담은 바이트의 합이
    /// `req.limit` 이하이거나 `req.passes`번째 패스면 멈춘다. 대상 NBD 노드는 열어 둔 채 마지막 비트맵을 돌려준다
    /// ([`migrate`](Self::migrate)의 `switchover_catch_up`이 마지막 패스를 보내고 닫는다). 앞선 따라잡기가 남긴 작업과
    /// 노드는 먼저 치운다. 작업이 실패하거나, `req.stall` 동안 보낸 양이 늘지 않거나, `req.timeout`이 지나거나, 플리싱
    /// 이미지를 둘 공간이 모자라면 나머지를 취소하고, 노드를 닫고, 플리싱을 떼고, 임시 비트맵을 지워 `Migration`
    /// 오류다.
    async fn catch_up_disks(&self, id: Uuid, req: &CatchUp) -> Result<CatchUpState, DriverError>;

    /// 실행 중인 VM을 `target`으로 보낸다(TLS, 능력 `events`·`auto-converge`, 이미지에 저장되지 않는
    /// `opl-` 비트맵이나 미러하는 디스크의 `opl-` 비트맵이 있으면 `dirty-bitmaps`). 보내기 전에
    /// inconsistent인 `opl-` 비트맵을 지운다. 끝날 때까지 기다린다. `opts.switchover_mirrors`면
    /// `pause-before-switchover`로 보내 게스트가 멈춘 `pre-switchover`에서 미러를 끝내고(대상 디스크가
    /// 원본과 같아진다) 이어 간다. 미러가 실패했으면 취소한다. `opts.switchover_catch_up`이면
    /// 같은 때에 따라잡은 디스크에 마지막 패스(마지막 비트맵 뒤 바뀐 블록)를 보내고 대상 노드를 닫은 뒤(실패하면 취소)
    /// 이어 간다. 따라잡기에는 `dirty-bitmaps`를 켜지 않는다(넘기는 비트맵은 마이그레이션 동안 다른 작업이 쓸 수 없어
    /// 마지막 패스가 그 비트맵을 쓰지 못한다). 성공하면 원본은 `postmigrate`로 멈춰 있다([`stop_migrated`](Self::stop_migrated)로 끈다). 실패하거나 `opts.timeout`이 지나 취소하면
    /// `Migration` 오류이고 원본은 대개 계속 돈다(멈췄으면 [`resume`](Self::resume)으로 이어서 실행한다).
    /// 남은 미러는 [`cancel_migration`](Self::cancel_migration)이 치운다.
    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        opts: &MigrateOptions,
    ) -> Result<(), DriverError>;

    /// 마이그레이션을 보내고 `postmigrate`로 멈춘 원본 QEMU를 끄고 true. 다른 상태이거나 실행 중이
    /// 아니면 아무것도 하지 않고 false.
    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError>;

    /// 이 VM이 보내는 중인 마이그레이션을 취소하고 끝날 때까지 기다린다(QMP `migrate_cancel`). 보내는
    /// 중이 아니면(끝났거나, 받는 쪽이거나, VM이 없다) 아무것도 하지 않는다. 취소하는 사이에 끝났으면
    /// 원본은 `postmigrate`로 멈춰 있다. opld가 기다리기를 그만둔 마이그레이션(재시작, 되돌리기)이
    /// 나중에 끝나 원본을 멈추지 않게 한다. 남은 디스크 미러(`opl-mirror-*`)와 따라잡기의 작업·노드(`opl-catchup-*`)는
    /// 보내는 중이 아니어도 취소하고 치우고, 따라잡기 임시 비트맵(`opl-mig-*`)을 지운다.
    async fn cancel_migration(&self, id: Uuid) -> Result<(), DriverError>;

    /// 마이그레이션을 보내기 전에(받는 쪽 QEMU를 켜거나 디스크를 복사하기 전에) 도는 QEMU의 디스크·NIC·붙인 vCPU·
    /// virtio-mem 장치(`/machine/peripheral`)가 받는 쪽이 켤 장치 id(`expected` —
    /// [`opl_types::hotplug::qemu_devices`])와 같은지 본다. 다르면 `Migration` 오류다.
    async fn check_devices(&self, id: Uuid, expected: &[String]) -> Result<(), DriverError>;

    /// 이 노드의 핫플러그 능력(계획 54). vCPU는 x86_64만, 메모리는 QEMU에 `virtio-mem-pci`가 있을 때다
    /// (`-device help`, 5초). 장치 목록을 읽지 못하면 메모리는 못 하는 것으로 하고 경고만 남긴다.
    async fn hotplug_caps(&self) -> NodeHotplug;

    /// 이 노드의 펌웨어 능력(계획 59). TPM은 `swtpm`·`swtpm_setup`이 있고 swtpm이 TPM 2.0을 하며 QEMU에
    /// `emulator` TPM 백엔드가 있을 때, Secure Boot는 MS 키를 등록한 펌웨어(firmware descriptor·설정)가 있을 때다.
    async fn firmware_caps(&self) -> FirmwareCaps;

    /// 이 노드 QEMU의 CPU 모델·플래그(계획 63 §6). QEMU를 VM과 같은 머신·가속기로 `-S`로 한 번 띄워 QMP로 묻고 끈다
    /// (보통 1초 안쪽, 30초 제한). 카탈로그와 호스트 CPU(KVM의 `full host`)를 돌려준다.
    async fn cpu_catalog(&self) -> Result<CpuProbe, DriverError>;

    /// 실행 중인 VM의 vCPU마다(켜진 것 모두, QMP `query-cpus-fast` 순서) CPU 속성 `names`의 값(QMP `qom-get`)을 읽는다
    /// (계획 63: 숨긴 플래그가 실제로 꺼졌는지). 불이 아닌 속성과 없는 속성은 빠진다.
    async fn vcpu_props(
        &self,
        id: Uuid,
        names: &[String],
    ) -> Result<Vec<BTreeMap<String, bool>>, DriverError>;

    /// 핫플러그 배치로 켠 VM의 켜진 vCPU 수를 `from`에서 `to`로 바꾸고 닿은 수를 돌려준다. `cpu`는 켤 때의 CPU
    /// 모양(소켓·상한으로 vCPU `i`의 자리를 정한다)이다. 늘리기(같은 수 포함)는 켤 때 수부터 `to` 아래의 빈 자리에
    /// `cpu{i}`를 붙이고(이미 켜진 자리는 건너뜀 — 게스트가 늦게 내려 준 자리도 다시 붙인다), 줄이기는 큰 번호부터 떼며 게스트가 `timeout` 안에 내려 주지 않으면 거기서 멈춘다(그때 `to`보다
    /// 크다). QEMU가 떼기 요청을 거절하면(게스트가 vCPU 떼기를 켜지 않음) 거기서 멈추고 `refused`다. 아무것도 바꾸지 못한
    /// 오류만 `Err`이고, 몇 개를 바꾼 뒤의 오류는 경고와 함께 닿은 수다. 켤 때 수
    /// 아래나 상한 위는 `InvalidSpec`이다.
    async fn set_vcpus(
        &self,
        id: Uuid,
        cpu: &CpuSpec,
        from: u16,
        to: u16,
        timeout: Duration,
    ) -> Result<VcpuChange, DriverError>;

    /// virtio-mem에 꽂을 크기(`requested-size`)를 `plugged_mib`로 바꾸고 닿은 크기(MiB)를 돌려준다. 늘리기는
    /// QEMU가 받으면 그 값이다(게스트 드라이버가 없으면 게스트가 보는 크기는 늘지 않는다). 줄이기는 게스트가
    /// `timeout` 안에 내려 주기를 기다리고, 넘으면 `requested-size`를 그때 크기로 되돌려 그 크기다.
    async fn set_memory(
        &self,
        id: Uuid,
        plugged_mib: u32,
        timeout: Duration,
    ) -> Result<u32, DriverError>;

    /// 실행 중인 VM의 루트 포트 `hp{port}`에 `index`번째 디스크(`disk{index}`, virtio-blk만)를 붙인다. 볼륨은
    /// 호출자가 켜 둔다. 장치 `disk{index}-dev`가 이미 있으면 아무것도 하지 않고 `AlreadyPresent`이고, 장치 없이
    /// 남은 블록 노드(앞서 중단된 붙이기)는 먼저 닫는다. 중간에 실패하면 연 블록 노드를 닫고 오류다.
    async fn add_disk(
        &self,
        id: Uuid,
        index: usize,
        disk: &RuntimeDisk,
        port: u8,
    ) -> Result<AddOutcome, DriverError>;

    /// 실행 중 붙인 `index`번째 디스크를 떼고 블록 노드를 닫는다. 게스트가 `timeout` 안에 내려 주지 않으면
    /// 장치를 그대로 두고 false다(요청은 남아 게스트가 나중에 내려 줄 수 있다). `device_del`을 보낸 뒤 QMP가 끊기거나
    /// 답하지 않아도 QEMU가 받았을 수 있으므로 false다. 이미 없으면 true다.
    async fn remove_disk(
        &self,
        id: Uuid,
        index: usize,
        timeout: Duration,
    ) -> Result<bool, DriverError>;

    /// 실행 중인 VM의 루트 포트 `hp{port}`에 `index`번째 NIC(`net{index}`·`nic{index}`)를 붙인다. 브리지 NIC는
    /// tap을 먼저 만든다. 장치 `nic{index}`가 이미 있으면 tap도 건드리지 않고 `AlreadyPresent`이고, 장치 없이 남은
    /// 백엔드(앞서 중단된 붙이기)는 먼저 지운다. 중간에 실패하면 만든 백엔드와 tap을 지우고 오류다.
    async fn add_nic(
        &self,
        id: Uuid,
        index: usize,
        nic: &RuntimeNic,
        port: u8,
    ) -> Result<AddOutcome, DriverError>;

    /// 실행 중 붙인 `index`번째 NIC를 떼고 백엔드와 tap을 지운다. 게스트가 `timeout` 안에 내려 주지 않으면
    /// 장치를 그대로 두고 false다. 이미 없으면 true다.
    async fn remove_nic(
        &self,
        id: Uuid,
        index: usize,
        timeout: Duration,
    ) -> Result<bool, DriverError>;

    /// 실행 중인 VM의 CD-ROM 드라이브 `index`(`cd{index}-dev`)에 매체 `medium`을 넣고, None이면 빼서 빈 드라이브로 둔다.
    /// 매체는 읽기 전용으로 열고 트레이는 강제로 연다(게스트가 잠가 두어도 바뀐다). 그 드라이브에 이미 같은 파일이
    /// 있으면(빼기는 이미 비었으면) 아무것도 하지 않는다. 앞서 실패하고 남은 블록 노드는 먼저 닫고, 중간에 실패하면 연
    /// 노드를 닫고 오류다. 드라이브가 없으면 `InvalidSpec`이다.
    async fn change_medium(
        &self,
        id: Uuid,
        index: usize,
        medium: Option<&RuntimeMedium>,
    ) -> Result<(), DriverError>;

    /// 실행 중인 VM의 `index`번째 NIC 속도 제한을 런타임 정의 `nic`의 것으로 맞춘다(계획 67 §8.2): Linux 브리지 tap이면
    /// tc를 바꾸고(QEMU는 그대로), SDN tap(OVN `QoS` 행이 함)과 user NIC는 할 일이 없다. tap 이름은 이 VM의 것이어야 한다.
    async fn set_nic_qos(
        &self,
        id: Uuid,
        index: usize,
        nic: &RuntimeNic,
    ) -> Result<(), DriverError>;

    /// 실행 중인 VM의 `index`번째 NIC를 런타임 정의 `from`(지금 도는 것)에서 `to`로 옮긴다(계획 67 §9.4 `RelinkNic`): QEMU
    /// 장치는 그대로 두고 tap을 Linux 브리지에서 OVS `br-int`로(또는 거꾸로) 옮긴다. SDN 쪽으로 옮기면 ovn-controller가 포트를
    /// 켤 때까지(`ovn-installed`) `timeout`까지 기다리고, 오지 않으면 tap을 원래 브리지로 되돌리고 실패한다. 옮긴 뒤 QMP
    /// `set_link`로 링크를 내렸다 올려 게스트가 DHCP를 다시 묻게 한다. 둘 다 이 VM의 같은 순번 tap이어야 한다.
    async fn relink_nic(
        &self,
        id: Uuid,
        index: usize,
        from: &RuntimeNic,
        to: &RuntimeNic,
        timeout: Duration,
    ) -> Result<(), DriverError>;

    /// 이 노드가 켠 VM의 하드웨어 기록(`status.hardware`, 핫플러그 배치 포함)을 VM 실행 디렉터리에 남긴다(QMP
    /// 소켓 옆 `hardware.json`). QEMU가 끝나면 실행 디렉터리와 함께 지워진다. VM이 없으면 `NotRunning`.
    fn save_hardware(&self, id: Uuid, hw: &RunningHardware) -> Result<(), DriverError>;

    /// [`save_hardware`](Self::save_hardware)로 남긴 기록. 없거나 읽지 못하면 None이다(기록 없이 다시 붙은 VM).
    fn saved_hardware(&self, id: Uuid) -> Option<RunningHardware>;

    /// 도는 VM의 스레드 고정(계획 64)을 `pin`에 맞춘다: 스코프 `AllowedCPUs`가 다르면 바꾸고, 스레드마다 친화가 다르면
    /// 다시 건다(vCPU는 자리의 CPU, 나머지는 에뮬레이터 집합). 바꾼 것의 수를 돌려준다(0이면 이미 맞았다). 검사 루프의
    /// 어긋남 고치기와 공유 집합이 바뀐 뒤의 다시 걸기가 부른다.
    async fn repin(&self, id: Uuid, pin: &RuntimePinning) -> Result<usize, DriverError> {
        let _ = (id, pin);
        Ok(0)
    }

    /// 켤 때나 [`repin`](Self::repin)이 남긴 고정. 고정 없이 켰으면 None이다.
    fn saved_pinning(&self, id: Uuid) -> Option<RuntimePinning> {
        let _ = id;
        None
    }

    /// 남긴 고정을 지운다: 그 뒤 검사 루프는 이 VM의 스레드를 건드리지 않는다(노드 설정을 지워 고정을 푼 VM, 리뷰 2 N3).
    fn forget_pinning(&self, id: Uuid) {
        let _ = id;
    }

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent>;
}
