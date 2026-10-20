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
    /// 정상 종료. `guest_initiated`는 게스트 OS 안에서 끈 경우 true다.
    Stopped {
        guest_initiated: bool,
    },
    /// 종료 이벤트 없이 프로세스가 사라졌다.
    Crashed,
}

/// VM 프로세스 수명을 관리한다. 같은 VM에 대한 호출을 직렬화하는 것은 호출자(reconciler)의 책임이다.
#[async_trait]
pub trait Hypervisor: Send + Sync {
    /// VM을 시작한다. 이미 실행 중이면 아무것도 하지 않고 성공한다.
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError>;

    /// `Acpi`는 요청만 보내고 바로 돌아온다. `Force`는 프로세스가 끝날 때까지 기다린다.
    /// 실행 중이 아니면 아무것도 하지 않고 성공한다.
    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError>;

    /// 실행 중이 아니면 `None`
    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError>;

    async fn list(&self) -> Result<Vec<ActualState>, DriverError>;

    /// VNC 유닉스 소켓 경로
    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError>;

    /// 이 시점 이후의 VM 수명 이벤트를 받는다.
    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent>;
}
