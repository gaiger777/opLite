//! QEMU 커맨드라인 생성과 QMP 클라이언트.

pub mod cmdline;
pub mod cpu;
pub mod firmware;
pub mod host;
pub mod iso9660;
pub mod paths;
pub mod qga;
pub mod qmp;

pub use cmdline::{
    CmdlineError, HELPER_PROTO, HELPER_SANDBOX, HELPER_SANDBOX_DROPPED, HelperDisk, HelperDrop,
    LIVE_RESTORE_RECONNECT_SECS, LiveRestoreDisk, LiveRestoreSource, MAX_HELPER_DISKS,
    RestoreHelperArgs, build_cmdline, build_incoming_cmdline, build_live_restore_cmdline,
    build_replica_receiver_cmdline, build_restore_helper_cmdline,
};
pub use cpu::{CpuProbe, CpuProbeError, CpuProbeReplies};
pub use firmware::FirmwarePair;
pub use host::{Accel, HostProfile, hotplug_caps, kvm_available};
pub use paths::{PathError, RuntimePaths};
pub use qmp::{
    BackupSync, BitmapInfo, BlockNode, BlockStats, HotpluggableCpu, JobInfo, MigrationInfo,
    MigrationParameters, MigrationState, NbdBlockdev, QmpClient, QmpError, QmpEvent, RunState,
    TlsEndpoint, TransactionAction, change_medium,
};
