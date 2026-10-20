//! 하이퍼바이저와 스토리지 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.

pub mod error;
pub mod hypervisor;
pub mod mock;
pub mod qemu;
pub mod storage;

pub use error::DriverError;
pub use hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
pub use mock::MockHypervisor;
pub use qemu::{QemuConfig, QemuHypervisor};
pub use storage::{Capacity, DirStorage, SourceFormat, Storage, VolumeInfo};
