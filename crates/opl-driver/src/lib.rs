//! 하이퍼바이저, 스토리지, 호스트 네트워크 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.

pub mod error;
pub mod hypervisor;
pub mod mock;
pub mod mounts;
pub mod net;
pub mod qemu;
pub mod storage;

pub use error::DriverError;
pub use hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
pub use mock::MockHypervisor;
pub use net::{HostNet, IpRoute, Tap};
pub use opl_types::storage::SourceFormat;
pub use qemu::{QemuConfig, QemuHypervisor, uefi_vars_path, vm_state_dir};
pub use storage::{
    Capacity, DirStorage, DiskLocation, LvInfo, LvmOps, LvmThinStorage, RBD_DEFAULT_USER, RbdOps,
    RbdSnapshot, RbdSpace, RbdStorage, Storage, Storages, SystemLvm, SystemRbd, SystemZfs,
    VolumeInfo, VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo,
    check_pool_root, disk_location, parse_ceph_df, parse_lvs, parse_zfs_dataset, pool_root,
};
