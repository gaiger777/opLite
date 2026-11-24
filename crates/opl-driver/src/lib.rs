//! 하이퍼바이저, 스토리지, 호스트 네트워크 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.

pub mod affinity;
pub mod backup_serve;
pub mod ceph;
pub mod error;
pub mod file_restore;
pub mod firewall;
pub mod hostcheck;
pub mod hypervisor;
pub mod migration;
pub mod mock;
pub mod mounts;
pub mod net;
pub mod ovn;
pub mod qemu;
pub mod replication;
pub mod storage;
pub mod tpm;
pub mod vip;

pub use backup_serve::{
    BACKUP_SERVE_COMMAND, ServeHelper, backup_serve_alive, backup_serve_unit, start_backup_serve,
    stop_backup_serve,
};
pub use error::DriverError;
pub use firewall::{Firewall, FirewallNic, Nft, ruleset};
pub use hostcheck::{
    IpLink, LinkProbe, NetFs, PING_TIMEOUT, PROBE_TIMEOUT, Paths, ProbeError, ProcBonding,
    ProcPort, RBD_PROBE_TIMEOUT, bond_detail, bond_status, interfaces, iscsi_logged_in, link_state,
    parse_ip_addr, parse_proc_bonding, parse_pvs, ping, ping_args, probe_dir, pv_paths, pvs, rados,
    rados_args, rados_result, refresh_interfaces, reported_interfaces,
};
pub use hypervisor::{
    ActualState, AddOutcome, BackupTarget, BitmapInfo, CpuProbe, DiskBitmaps, DiskCounters,
    EventKind, FleeceDisk, FleeceExport, FleeceRequest, Hypervisor, HypervisorEvent,
    LiveRestoreJob, Quiesced, RuntimeStatus, StopMode, VcpuChange,
};
pub use migration::{
    CatchUp, CatchUpState, IncomingPorts, MIGRATION_TLS_ID, MigrateOptions, MigrateTarget,
    MirrorOptions, MirrorProgress, MirrorTarget, NBD_TLS_ID, format_duration, migration_tls_dir,
    tcp_uri, write_migration_tls,
};
pub use mock::{
    CatchUpCall, GuestAnswer, HotplugCall, IncrementalCall, MOCK_INCREMENT_BYTES, MockGuestFs,
    MockHypervisor, ReplicationCall, mock_cpu_probe,
};
pub use net::{HostNet, IpRoute, Overlay, OverlayChanges, Tap};
pub use opl_types::runtime::{LiveRestoreDisk, LiveRestoreSource};
pub use opl_types::storage::SourceFormat;
pub use qemu::{QemuConfig, QemuHypervisor, uefi_vars_path, vm_state_dir};
pub use replication::{REPLICA_TLS_ID, ReceiverExport, ReplicaDiskTarget, ReplicateRequest};
pub use storage::{
    Capacity, DirStorage, DiskLocation, ISO_HEADER_LEN, LvInfo, LvmOps, LvmSharedStorage,
    LvmThinStorage, NbdSource, RBD_DEFAULT_USER, RbdOps, RbdSnapshot, RbdSpace, RbdStorage,
    Storage, Storages, SystemLvm, SystemRbd, SystemZfs, VgInfo, VolumeInfo, VolumeSource,
    ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo, check_pool_root,
    create_overlay, disk_location, is_iso_image, nbd_chain_source, nbd_source, parse_ceph_df,
    parse_lvs, parse_vgs, parse_zfs_dataset, pool_root,
};
pub use tpm::{
    KeepFirmware, SwtpmConfig, adopt_firmware_epoch, firmware_epoch_differs, read_firmware_epoch,
    tpm_state_path, vm_tpm_dir, write_firmware_epoch,
};
pub use vip::{IpVip, VipAddress, VipNet, VipRecord, release_recorded};
