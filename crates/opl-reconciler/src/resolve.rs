//! VM spec의 이름 참조(Volume, Network)를 풀어 드라이버가 받는 `VmRuntimeSpec`을 만든다.

use std::path::{Path, PathBuf};

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DiskLocation, DriverError, Storage, disk_location};
use opl_types::hotplug;
use opl_types::network::{NetworkSpec, USER_NETWORK, tap_name};
use opl_types::node::{Node, NodeSpec};
use opl_types::runtime::{
    DiskKind, ImageFormat, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec,
};
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, VolumeSpec, pool_error_on, pool_listed_on,
    pool_state_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::{DiskMove, DiskSpec, RunningHardware, Vm};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("volume {0:?} not found")]
    VolumeNotFound(String),
    #[error("storage pool {0:?} not found")]
    PoolNotFound(String),
    #[error("storage pool {0:?} is not available on this node")]
    PoolNotOnNode(String),
    #[error("storage pool {0:?} is not ready on this node: {1}")]
    PoolUnavailable(String, String),
    /// 볼륨의 크기 늘리기 Task가 실행 중이다. 끝나면 풀린다.
    #[error("volume {0:?} is being resized")]
    VolumeResizing(String),
    /// 이 노드의 OS(macOS)는 NFS 풀을 지원하지 않는다. 기다려도 풀리지 않는다.
    #[error("storage pool {0:?} is an nfs pool, which is not supported on this node's OS")]
    NfsUnsupported(String),
    /// 이 노드에서 풀의 백엔드를 쓸 수 없다(이유). 기다려도 풀리지 않는다.
    #[error("storage pool {0:?} cannot be used on this node: {1}")]
    PoolUnsupported(String, &'static str),
    /// 저장소가 볼륨을 디스크로 내주지 못했다(다시 해도 같은 오류).
    #[error("volume {0:?} cannot be used: {1}")]
    VolumeUnusable(String, DriverError),
    #[error("network {0:?} not found")]
    NetworkNotFound(String),
    #[error("nic {0} has no MAC address")]
    MissingMac(usize),
    /// 스펙에 그 순번의 디스크가 없다([`runtime_disk`]).
    #[error("disk {0} is not in the VM spec")]
    NoDisk(usize),
    /// 스펙에 그 순번의 NIC가 없다([`runtime_nic`]).
    #[error("nic {0} is not in the VM spec")]
    NoNic(usize),
    /// 마이그레이션으로 받을 VM의 스펙 디스크·NIC가 원본 QEMU의 장치 기록(`status.hardware`)과 다르다(핫플러그가
    /// 끝나지 않았거나 게스트가 아직 내려 주지 않은 장치). 같은 장치로 켤 수 없어 받지 않는다.
    #[error("the VM's devices do not match its running hardware")]
    DevicesChanged,
    #[error(transparent)]
    Cluster(#[from] ClusterError),
}

/// 로컬 저장소에서 참조를 찾아 런타임 정의를 만든다. `node`는 이 노드의 id이고
/// `mount_root`는 이 노드가 NFS 풀을 마운트하는 디렉터리다. 이 노드가 준비되지 않았다고 알린 풀은
/// `PoolUnavailable`(잠시 뒤 다시 시도할 일)로 거절한다. 아직 알리지 않은 풀(opld가 막 시작해 첫 새로
/// 고침 전)은 막지 않되, NFS 풀은 실제로 마운트돼 있는지 확인한다. Linux가 아닌 노드에서 NFS 풀은
/// `NfsUnsupported`, LVM-thin·ZFS·RBD·공유 LVM 풀은 `PoolUnsupported`(참조 오류)다. 디스크마다 `storage`로 볼륨을
/// 활성화한 뒤 [`disk_location`]으로 위치를 정한다(RBD 풀은 경로 대신 `RuntimeDisk.rbd`의 이미지).
/// 저장소의 일시 오류는 `PoolUnavailable`, 그 밖은 `VolumeUnusable`이다.
/// (다시 해도 안 되는 것은 볼륨이 없거나 정의가 틀린 것뿐이고, 그 밖의 저장소 오류는 일시 오류로 본다.)
/// 실행 중인 크기 늘리기 Task가 있는 볼륨은 켜지 않고 `VolumeResizing`이다(늘리는 중인 볼륨을 QEMU가
/// 열면 안 된다. 공유 LVM은 늘린 부분을 0으로 채우기 전이다). 기다리는(Pending) Task는 막지 않는다.
/// 그 Task가 VM의 줄에서 되돌리기 뒤에 서 있으면 되돌리기가 다시 켜는 VM을 기다리게 되어 서로
/// 기다린다. 실행기는 Task를 Running으로 기록(이 노드에 적용될 때까지 기다린다)한 뒤 VM 상태를 읽어
/// 꺼진 VM의 경로를 고른다. 그 전에 켜기 시작한 VM은 실행기가 원하는 상태가 running인 것을 보고 꺼진
/// VM의 경로를 타지 않고, 그 뒤에 켜는 VM은 여기서 Running Task를 보고 기다린다.
/// 실패하면 그때까지 활성화한 볼륨을 끈다(`deactivate_volume`, 끄지 못하면 경고만 남긴다).
pub async fn runtime_spec(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, Activate::Start).await
}

/// 이미 도는 VM에 다시 붙을 때의 [`runtime_spec`]. 크기 늘리기 Task를 기다리지 않는다(QEMU가 이미
/// 장치를 열었다).
pub async fn running_spec(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, Activate::Running).await
}

/// 라이브 마이그레이션을 받을 때의 [`runtime_spec`]. 볼륨을 `activate_volume` 대신
/// `share_volume`으로 켠다(원본 노드가 아직 쓰고 있다). 복사하는 디스크(`status.migration.disks`)는
/// 이 노드에 새로 만든 볼륨이므로 계획의 풀·형식으로 `activate_volume`한다.
pub async fn incoming_spec(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, Activate::Incoming).await
}

/// VM 디스크의 볼륨을 이 노드에서 `share_volume`(`shared`) 또는 `exclusive_volume`으로 바꾼다(라이브
/// 마이그레이션). 처음 실패에서 멈춘다. 오류는 [`runtime_spec`]처럼 나눈다: 저장소의 일시 오류(다른
/// 노드가 아직 잠금을 쥐고 있다 등)는 `PoolUnavailable`이다.
pub async fn set_disks_shared(
    cluster: &Cluster,
    vm: &Vm,
    storage: &dyn Storage,
    shared: bool,
) -> Result<(), ResolveError> {
    for disk in &vm.spec.disks {
        let volume = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        let pool_name = &volume.spec.pool;
        let pool = cluster
            .get_by_name::<StoragePoolSpec>(pool_name)?
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        let format = volume.spec.format;
        let done = if shared {
            storage.share_volume(&pool, volume.id, format).await
        } else {
            storage.exclusive_volume(&pool, volume.id, format).await
        };
        match done {
            Ok(()) => {}
            Err(e @ (DriverError::VolumeNotFound(_) | DriverError::InvalidSpec(_))) => {
                return Err(ResolveError::VolumeUnusable(disk.volume.clone(), e));
            }
            Err(e) => {
                return Err(ResolveError::PoolUnavailable(
                    pool_name.clone(),
                    e.to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// VM이 마이그레이션 계획의 노드로 확정됐으면(`spec.node`가 `status.migration.node`) 계획의 디스크마다
/// 볼륨 기록을 옮긴 위치로 고친다: 풀·형식이 다르면 spec을, 만든 노드(`status.node`)가 다르면 status를
/// (둘 다 revision 확인, 그 사이 바뀌었으면 다시 읽어). 이미 고쳤으면 할 일이 없다(멱등). 확정되지
/// 않은 VM, 계획이 없는 VM, 없어진 볼륨에는 아무것도 하지 않는다.
pub async fn apply_disk_moves(cluster: &Cluster, vm: &Vm) -> Result<(), ClusterError> {
    let Some(migration) = &vm.status.migration else {
        return Ok(());
    };
    if vm.spec.node != Some(migration.node) {
        return Ok(());
    }
    for m in &migration.disks {
        move_volume(cluster, m.volume, &m.pool, m.format, migration.node).await?;
    }
    Ok(())
}

/// 볼륨 `id`의 기록을 노드 `node`의 풀 `pool`(형식 `format`)에 있는 것으로 고친다: 풀·형식이 다르면 spec을, 만든
/// 노드(`status.node`)가 다르면 status를 쓴다(둘 다 revision 확인, 그 사이 바뀌었으면 다시 읽어). 이미 그렇거나 볼륨이
/// 없으면 할 일이 없다(멱등). 마이그레이션 확정과 복제본 승격이 쓴다.
pub async fn move_volume(
    cluster: &Cluster,
    id: Uuid,
    pool: &str,
    format: ImageFormat,
    node: Uuid,
) -> Result<(), ClusterError> {
    loop {
        let Some(volume) = cluster.get::<VolumeSpec>(id)? else {
            return Ok(());
        };
        let written = if volume.spec.pool != pool || volume.spec.format != format {
            let mut spec = volume.spec.clone();
            spec.pool = pool.to_string();
            spec.format = format;
            cluster
                .update_spec(volume.id, Some(volume.revision), volume.labels, &spec)
                .await
        } else if volume.status.node != Some(node) {
            let mut status = volume.status.clone();
            status.node = Some(node);
            cluster
                .update_status_checked::<VolumeSpec>(volume.id, volume.revision, &status)
                .await
        } else {
            return Ok(());
        };
        match written {
            Ok(_) | Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(e),
        }
    }
}

/// 볼륨을 켜는 방식
#[derive(Clone, Copy, PartialEq)]
enum Activate {
    /// VM을 시작한다. 실행 중인 크기 늘리기 Task를 기다린다.
    Start,
    /// 이미 도는 VM에 다시 붙는다. 크기 늘리기 Task를 보지 않는다.
    Running,
    /// 마이그레이션을 받는다. 공유로 켜고(`share_volume`) 크기 늘리기 Task를 기다린다.
    Incoming,
}

/// [`runtime_spec`]. `how`에 따라 볼륨을 켠다.
async fn resolve_activated(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
    how: Activate,
) -> Result<VmRuntimeSpec, ResolveError> {
    let mut activated = Vec::new();
    let result = resolve(cluster, vm, node, mount_root, storage, how, &mut activated).await;
    if result.is_err() {
        deactivate(vm, storage, activated).await;
    }
    result
}

/// 활성화한 볼륨을 끈다(끄지 못하면 경고만 남긴다).
async fn deactivate(
    vm: &Vm,
    storage: &dyn Storage,
    activated: Vec<(StoragePool, Uuid, ImageFormat)>,
) {
    for (pool, id, format) in activated {
        if let Err(e) = storage.deactivate_volume(&pool, id, format).await {
            tracing::warn!(vm = %vm.id, volume = %id, error = %e, "cannot deactivate volume");
        }
    }
}

/// [`runtime_spec`]. 활성화한 볼륨을 `activated`에 적는다. 새로 켜는 것이 아니면(다시 붙음, 마이그레이션 받기)
/// 실행 중 하드웨어 기록이 있을 때 그 모양으로 덮는다([`apply_hardware`]).
async fn resolve(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
    how: Activate,
    activated: &mut Vec<(StoragePool, Uuid, ImageFormat)>,
) -> Result<VmRuntimeSpec, ResolveError> {
    // 받는 쪽은 스펙의 디스크·NIC로 켜므로 원본 QEMU의 장치와 같아야 한다.
    if how == Activate::Incoming
        && let Some(hw) = &vm.status.hardware
        && !hotplug::devices_match(&vm.spec, hw)
    {
        return Err(ResolveError::DevicesChanged);
    }
    let lookup = DiskLookup::new(cluster, vm, node, mount_root, storage, how)?;
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
    for disk in &vm.spec.disks {
        disks.push(lookup.disk(disk, activated).await?);
    }
    let mut nics = Vec::with_capacity(vm.spec.nics.len());
    for i in 0..vm.spec.nics.len() {
        nics.push(runtime_nic(cluster, vm, i)?);
    }
    let mut spec = VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks,
        nics,
        // `ha.guest_watch`가 있는 VM만 게스트 에이전트 채널을 단다(API는 멈춘 VM에서만 켜고 끈다).
        guest_agent: vm
            .spec
            .ha
            .as_ref()
            .is_some_and(|ha| ha.guest_watch.is_some()),
        hotplug: None,
        cdroms: vec![],
    };
    if how != Activate::Start
        && let Some(hw) = &vm.status.hardware
    {
        apply_hardware(&mut spec, hw);
    }
    Ok(spec)
}

/// 런타임 정의를 실행 중 하드웨어 기록의 모양(켤 때의 cpu·메모리·펌웨어·머신과 핫플러그 배치)으로 덮는다. 스펙은
/// 실행 중에 바뀌어도(다음 재시작 때 적용) 마이그레이션 대상은 원본 QEMU와 같은 장치로 켜야 한다.
pub fn apply_hardware(spec: &mut VmRuntimeSpec, hw: &RunningHardware) {
    spec.cpu = hw.cpu.clone();
    spec.memory_mib = hw.memory_mib;
    spec.firmware = hw.firmware;
    spec.machine = hw.machine;
    spec.hotplug = hw.layout.clone();
}

/// 실행 중 VM에 붙일 디스크 하나(`vm.spec.disks[index]`)를 VM을 켤 때처럼 푼다: 크기 늘리기 Task를 기다리고
/// 볼륨을 켠 뒤 위치를 정한다. 오류는 [`runtime_spec`]과 같고, 실패하면 켠 볼륨을 끈다.
pub async fn runtime_disk(
    cluster: &Cluster,
    vm: &Vm,
    index: usize,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
) -> Result<RuntimeDisk, ResolveError> {
    let disk = vm
        .spec
        .disks
        .get(index)
        .ok_or(ResolveError::NoDisk(index))?;
    let lookup = DiskLookup::new(cluster, vm, node, mount_root, storage, Activate::Start)?;
    let mut activated = Vec::new();
    let result = lookup.disk(disk, &mut activated).await;
    if result.is_err() {
        deactivate(vm, storage, activated).await;
    }
    result
}

/// NIC 하나(`vm.spec.nics[index]`)의 런타임 정의. tap 이름은 순번으로 정한다([`tap_name`]).
pub fn runtime_nic(cluster: &Cluster, vm: &Vm, index: usize) -> Result<RuntimeNic, ResolveError> {
    let nic = vm.spec.nics.get(index).ok_or(ResolveError::NoNic(index))?;
    let backend = if nic.network == USER_NETWORK {
        NicBackend::User
    } else {
        let network = cluster
            .get_by_name::<NetworkSpec>(&nic.network)?
            .ok_or_else(|| ResolveError::NetworkNotFound(nic.network.clone()))?;
        NicBackend::Tap {
            ifname: tap_name(vm.id, index),
            bridge: network.spec.bridge_name(),
            vlan: network.spec.vlan,
            mtu: network.spec.mtu(),
        }
    };
    Ok(RuntimeNic {
        mac: nic.mac.ok_or(ResolveError::MissingMac(index))?,
        model: nic.model,
        backend,
    })
}

/// 디스크를 풀 때 VM마다 한 번 읽는 것
struct DiskLookup<'a> {
    cluster: &'a Cluster,
    node: Uuid,
    mount_root: &'a Path,
    storage: &'a dyn Storage,
    how: Activate,
    /// 이 노드의 기록(풀 준비 상태)
    me: Option<Node>,
    tasks: Vec<Task>,
    /// 받는 마이그레이션이 이 노드로 복사하는 디스크
    moves: &'a [DiskMove],
}

impl<'a> DiskLookup<'a> {
    fn new(
        cluster: &'a Cluster,
        vm: &'a Vm,
        node: Uuid,
        mount_root: &'a Path,
        storage: &'a dyn Storage,
        how: Activate,
    ) -> Result<Self, ResolveError> {
        let me = cluster.get::<NodeSpec>(node)?;
        let (_, tasks) = cluster.list::<TaskSpec>()?;
        let moves: &[DiskMove] = match (&vm.status.migration, how) {
            (Some(m), Activate::Incoming) => &m.disks,
            _ => &[],
        };
        Ok(Self {
            cluster,
            node,
            mount_root,
            storage,
            how,
            me,
            tasks,
            moves,
        })
    }

    /// 디스크 하나의 볼륨을 켜고 위치를 정한다. 켠 볼륨을 `activated`에 적는다.
    async fn disk(
        &self,
        disk: &DiskSpec,
        activated: &mut Vec<(StoragePool, Uuid, ImageFormat)>,
    ) -> Result<RuntimeDisk, ResolveError> {
        let (cluster, node, mount_root, storage, how) = (
            self.cluster,
            self.node,
            self.mount_root,
            self.storage,
            self.how,
        );
        let (me, tasks, moves) = (&self.me, &self.tasks, self.moves);
        let volume = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        let moved = moves.iter().find(|m| m.volume == volume.id);
        if how != Activate::Running
            && tasks.iter().any(|t| {
                t.spec.target == volume.id
                    && t.status.state == TaskState::Running
                    && matches!(t.spec.op, TaskOp::VolumeResize { .. })
            })
        {
            return Err(ResolveError::VolumeResizing(disk.volume.clone()));
        }
        let pool_name = moved.map_or(&volume.spec.pool, |m| &m.pool);
        let pool = cluster
            .get_by_name::<StoragePoolSpec>(pool_name)?
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        if !pool_listed_on(&pool, node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM(공유 LVM 포함)·ZFS·RBD를 쓴다(Homebrew QEMU에는
        // rbd가 없다).
        if !cfg!(target_os = "linux") {
            match pool.spec.backend {
                PoolBackend::Nfs { .. } => {
                    return Err(ResolveError::NfsUnsupported(pool_name.clone()));
                }
                PoolBackend::LvmThin { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.clone(),
                        "lvm-thin pools are not supported on macOS",
                    ));
                }
                PoolBackend::Zfs { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.clone(),
                        "zfs pools are not supported on macOS",
                    ));
                }
                PoolBackend::Rbd { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.clone(),
                        "rbd pools are not supported on macOS",
                    ));
                }
                PoolBackend::LvmShared { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.clone(),
                        "lvm-shared pools are not supported on macOS",
                    ));
                }
                PoolBackend::Dir { .. } => {}
            }
        }
        if let Some(me) = &me
            && pool_state_on(&pool, me).is_some_and(|s| !s.ready)
        {
            let error = pool_error_on(&pool, me);
            return Err(ResolveError::PoolUnavailable(pool_name.clone(), error));
        }
        let format = moved.map_or(volume.spec.format, |m| m.format);
        let activation = match how {
            Activate::Incoming if moved.is_none() => {
                storage.share_volume(&pool, volume.id, format).await
            }
            Activate::Incoming | Activate::Start | Activate::Running => {
                storage.activate_volume(&pool, volume.id, format).await
            }
        };
        let located = match activation {
            Ok(()) => {
                let located = disk_location(&pool, mount_root, volume.id, format);
                activated.push((pool.clone(), volume.id, format));
                located
            }
            Err(e) => Err(e),
        };
        let (path, kind, rbd) = match located {
            Ok(DiskLocation::Path(path, kind)) => (path, kind, None),
            Ok(DiskLocation::Rbd(target)) => (PathBuf::new(), DiskKind::Rbd, Some(target)),
            // 볼륨이 없거나 정의가 틀리면 다시 해도 같다.
            Err(e @ (DriverError::VolumeNotFound(_) | DriverError::InvalidSpec(_))) => {
                return Err(ResolveError::VolumeUnusable(disk.volume.clone(), e));
            }
            // 그 밖(장치 명령 실패 등)은 풀 쪽 사정일 수 있으므로 잠시 뒤 다시 해 본다.
            Err(e) => {
                return Err(ResolveError::PoolUnavailable(
                    pool_name.clone(),
                    e.to_string(),
                ));
            }
        };
        Ok(RuntimeDisk {
            path,
            kind,
            rbd,
            format,
            bus: disk.bus,
            boot_order: disk.boot_order,
        })
    }
}
