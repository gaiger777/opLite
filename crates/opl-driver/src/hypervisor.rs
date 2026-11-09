use std::ops::RangeInclusive;
use std::path::PathBuf;

use async_trait::async_trait;
pub use opl_qemu::BitmapInfo;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::DriverError;
use crate::migration::{MigrateOptions, MigrateTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// 게스트에 ACPI 전원 버튼을 보낸다. 게스트가 무시할 수 있다.
    Acpi,
    /// 프로세스를 즉시 끝낸다.
    Force,
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
    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError>;

    /// 메모리 없이 실행 중인 VM의 모든 디스크에 내부 스냅샷 `tag`와 dirty bitmap `bitmap`을 한 시점에
    /// 만든다(transaction 하나). 비트맵은 qcow2 디스크면 이미지에 저장된다(persistent).
    async fn snapshot_save_with_bitmap(
        &self,
        id: Uuid,
        tag: &str,
        bitmap: &str,
    ) -> Result<(), DriverError>;

    /// 실행 중인 VM의 디스크 노드(`disk{i}`)마다 dirty bitmap들을 디스크 순서로 읽는다.
    async fn backup_bitmaps(&self, id: Uuid) -> Result<Vec<DiskBitmaps>, DriverError>;

    /// 실행 중인 VM의 모든 디스크에 dirty bitmap `name`을 한 시점에 만든다(qcow2면 persistent).
    /// VM을 멈춘 동안 블록 풀 스냅샷과 같은 시점을 잡는 데 쓴다.
    async fn add_bitmaps(&self, id: Uuid, name: &str) -> Result<(), DriverError>;

    /// 실행 중인 VM의 모든 디스크에서 이름이 `opl-`로 시작하는 dirty bitmap 중 `keep`이 아닌 것을
    /// 모두 지운다.
    async fn remove_bitmaps(&self, id: Uuid, keep: Option<&str>) -> Result<(), DriverError>;

    /// 실행 중인 VM의 디스크마다 비트맵 `parent` 뒤에 바뀐 블록만 `targets`(디스크 순서)에 압축해
    /// 쓴다. 쓰기 시작하는 시점에 모든 디스크에 새 비트맵 `new`를 만든다(transaction 하나). 작업이
    /// 하나라도 실패하면 나머지를 취소한다. 대상 노드는 끝나면 닫는다(닫지 못하면 대상 파일이 온전하지
    /// 않으므로 실패). 성공하면 `parent`를 지우고, 실패하면 `new`를 지워 `parent`만 남긴다(대상 파일은
    /// 호출자가 지운다).
    async fn incremental_backup(
        &self,
        id: Uuid,
        parent: &str,
        new: &str,
        targets: &[BackupTarget],
    ) -> Result<(), DriverError>;

    /// 실행 중인 VM에 앞선 증분 백업이 남긴 작업과 대상 노드(`opl-backup-*`, opld가 기다리다 죽었다)를
    /// 치운다. 아직 도는 작업은 취소하고 잠시 기다린다(끝나지 않으면 오류). 남은 작업이 있으면 디스크의
    /// 내부 스냅샷이 실패한다.
    async fn clear_backup_leftovers(&self, id: Uuid) -> Result<(), DriverError>;

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

    /// 라이브 마이그레이션을 받을 QEMU를 `-incoming defer`로 띄우고, 받는 중 표시(마이그레이션 Task
    /// `task`)를 남기고, TLS
    /// 자격 증명(받는 쪽, 상대 인증서 확인)과 능력을 정한 뒤 `ports`에서 빈 포트로 `listen_host`에서
    /// 받기 시작한다. 연 포트를 돌려준다. 실패하면 띄운 QEMU를 끈다. 이미 이 노드에서 돌고 있는
    /// VM이면 실패한다. 받은 뒤에도 멈춰 있다([`finish_incoming`](Self::finish_incoming)이 이어서 실행).
    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        task: Uuid,
        listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError>;

    /// 받는 중인 VM의 마이그레이션이 끝나기를 기다린다. 완료면 성공, 실패했거나 받는 QEMU가
    /// 사라졌으면 `Migration` 오류다. 받는 중 표시가 없는 VM이면 실패한다.
    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError>;

    /// 받기를 마친 VM을 넘겨받는다: 멈춰 있으면 이어서 실행하고 받는 중 표시를 지운다. 표시가
    /// 없으면(이미 넘겨받음) 아무것도 하지 않는다. 아직 받는 중이면 `Migration` 오류다. 마이그레이션이
    /// 확정된 뒤(VM이 이 노드로 옮겨진 뒤)에만 부른다.
    async fn finish_incoming(&self, id: Uuid) -> Result<(), DriverError>;

    /// 받는 중 표시가 있는 살아 있는 VM과 그 표시의 마이그레이션 Task(읽을 수 없으면 nil). 확정되지
    /// 않은 채 남은 받는 QEMU를 찾아 끄는 데 쓴다.
    async fn list_incoming(&self) -> Result<Vec<(Uuid, Uuid)>, DriverError>;

    /// 실행 중인 VM을 `target`으로 보낸다(TLS, 능력 `events`·`auto-converge`, 이미지에 저장되지 않는
    /// `opl-` 비트맵이 있으면 `dirty-bitmaps`). 보내기 전에 inconsistent인 `opl-` 비트맵을 지운다. 끝날
    /// 때까지 기다린다. 성공하면 원본은 `postmigrate`로 멈춰
    /// 있다([`stop_migrated`](Self::stop_migrated)로 끈다). 실패하거나 `opts.timeout`이 지나 취소하면
    /// `Migration` 오류이고 원본은 대개 계속 돈다(멈췄으면 [`resume`](Self::resume)으로 이어서 실행한다).
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
    /// 나중에 끝나 원본을 멈추지 않게 한다.
    async fn cancel_migration(&self, id: Uuid) -> Result<(), DriverError>;

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent>;
}
