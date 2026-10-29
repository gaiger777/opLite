use std::path::PathBuf;

use async_trait::async_trait;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::DriverError;

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

/// VM 프로세스 수명을 관리한다. 같은 VM에 대한 호출을 직렬화하는 것은 호출자(reconciler)의 책임이다.
#[async_trait]
pub trait Hypervisor: Send + Sync {
    /// VM을 시작한다. 이미 실행 중이면 아무것도 하지 않고 성공한다.
    ///
    /// `restore`가 있으면 새로 띄운 VM을 실행하기 전에 그 태그의 내부 스냅샷(디스크와 메모리,
    /// 메모리는 첫 디스크)을 불러온다. 불러오지 못하면 VM을 끝내고 오류를 돌려준다.
    /// 이미 실행 중인 VM에는 쓰지 않는다.
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

    /// 실행 중인 VM의 모든 디스크에서 내부 스냅샷 `tag`를 지운다. 태그가 없어도 성공한다.
    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError>;

    /// 실행 중인 VM의 `disk`번째 디스크(스펙의 `disks` 순서) 크기를 바꾼다. 게스트가 바로 본다.
    async fn block_resize(&self, id: Uuid, disk: usize, size_bytes: u64)
    -> Result<(), DriverError>;

    /// 실행 중인 VM을 멈춘다(QMP `stop`). 돌아오면 게스트의 블록 I/O가 끝나 있으므로 디스크를
    /// 바깥에서 찍을 수 있다. 반드시 `resume`으로 이어서 실행한다. 그러지 못한 채 opld가 끝나면
    /// 다음에 다시 붙을 때(`start`) 이어서 실행한다.
    async fn pause(&self, id: Uuid) -> Result<(), DriverError>;

    /// `pause`로 멈춘 VM을 이어서 실행한다(QMP `cont`). 실행 중이어도 성공한다.
    async fn resume(&self, id: Uuid) -> Result<(), DriverError>;

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent>;
}
