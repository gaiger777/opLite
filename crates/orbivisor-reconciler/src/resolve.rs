//! VM spec의 이름 참조(Volume, Network)를 풀어 드라이버가 받는 `VmRuntimeSpec`을 만든다.

use std::path::{Path, PathBuf};

use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_driver::{DiskLocation, DriverError, Storage, disk_location};
use orbivisor_types::cloud_init;
use orbivisor_types::hotplug;
use orbivisor_types::network::{NetworkSpec, USER_NETWORK, tap_name};
use orbivisor_types::node::{Node, NodeSpec};
use orbivisor_types::runtime::{
    DiskKind, ImageFormat, NicBackend, RuntimeCdrom, RuntimeDisk, RuntimeMedium, RuntimeNic,
    RuntimeTpm, VmRuntimeSpec,
};
use orbivisor_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, VolumeContent, VolumeSpec, pool_error_on,
    pool_holds_isos, pool_listed_on, pool_state_on,
};
use orbivisor_types::task::{Task, TaskOp, TaskSpec, TaskState};
use orbivisor_types::vm::{DiskMove, DiskSpec, RunningHardware, Vm, VmSpec, effective_boot};
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
    /// 볼륨(연결 복제)의 평탄화 Task가 실행 중이다(계획 58). 끝나면 풀린다.
    #[error("volume {0:?} is being flattened")]
    VolumeFlattening(String),
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
    /// 스펙에 그 순번의 CD-ROM 드라이브가 없다([`runtime_medium`]).
    #[error("CD-ROM drive {0} is not in the VM spec")]
    NoCdrom(usize),
    /// 마이그레이션으로 받을 VM의 스펙 디스크·NIC가 원본 QEMU의 장치 기록(`status.hardware`)과 다르다(핫플러그가
    /// 끝나지 않았거나 게스트가 아직 내려 주지 않은 장치). 같은 장치로 켤 수 없어 받지 않는다.
    #[error("the VM's devices do not match its running hardware")]
    DevicesChanged,
    /// 마이그레이션으로 받을 VM의 cloud-init 씨앗(지금 스펙으로 만든 것)이 원본 QEMU의 씨앗(기록의 instance-id)과
    /// 다르다(실행 중 cloud-init을 바꿨거나 호스트 이름 없는 VM의 이름을 바꿈). 게스트가 다른 씨앗을 보게 되어 받지 않는다.
    #[error("cloud-init changed while running; restart the VM before migrating it")]
    CloudInitChanged,
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
/// 실행 중인 크기 늘리기 Task가 있는 볼륨은 켜지 않고 `VolumeResizing`이다(평탄화 Task면 `VolumeFlattening`)(늘리는 중인 볼륨을 QEMU가
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

/// 볼륨 `id`의 기록을 노드 `node`의 풀 `pool`(형식 `format`)에 있는 것으로 고친다: 풀·형식이 다르거나 연결 복제
/// 기록(`base`)이 있으면 spec을, 만든 노드(`status.node`)가 다르면 status를 쓴다(둘 다 revision 확인, 그 사이 바뀌었으면
/// 다시 읽어). 이미 그렇거나 볼륨이 없으면 할 일이 없다(멱등). 마이그레이션 확정과 복제본 승격이 쓴다. 옮긴 디스크는
/// 사슬 전체를 읽어 쓴 독립 볼륨이므로 `base`를 지운다(계획 58 — 마이그레이션이 곧 평탄화다).
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
        let written = if volume.spec.pool != pool
            || volume.spec.format != format
            || volume.spec.base.is_some()
        {
            let mut spec = volume.spec.clone();
            spec.pool = pool.to_string();
            spec.format = format;
            spec.base = None;
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
    if how == Activate::Incoming
        && let Some(hw) = &vm.status.hardware
        && !hotplug::seed_matches(vm.id, &vm.name, &vm.spec, hw)
    {
        return Err(ResolveError::CloudInitChanged);
    }
    // 다시 붙거나 받을 때는 도는 QEMU의 CD-ROM 드라이브(매체를 바꾸는 중이면 기록의 매체)로 푼다.
    let hardware = vm
        .status
        .hardware
        .as_ref()
        .filter(|_| how != Activate::Start);
    let cdrom_specs = hardware.map_or(&vm.spec.cdroms, |hw| &hw.cdroms);
    let lookup = DiskLookup::new(cluster, vm, node, mount_root, storage, how)?;
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
    for disk in &vm.spec.disks {
        disks.push(lookup.disk(disk, activated).await?);
    }
    let mut cdroms = Vec::with_capacity(cdrom_specs.len());
    for cdrom in cdrom_specs {
        let medium = match &cdrom.iso {
            Some(iso) => Some(lookup.medium(iso, activated).await?),
            None => None,
        };
        cdroms.push(RuntimeCdrom {
            medium,
            boot_order: cdrom.boot_order,
        });
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
        // `guest_agent`나 `ha.guest_watch`가 있는 VM만 게스트 에이전트 채널을 단다.
        guest_agent: vm.spec.agent_channel(),
        hotplug: None,
        cdroms,
        // 씨앗은 지금 스펙의 cloud-init으로 만든다. NIC 입력(MAC)은 새로 켜면 스펙의 NIC, 다시 붙거나 받을 때는 기록의
        // 켤 때 단 NIC다(실행 중 붙인 NIC는 원본 QEMU의 씨앗에 없다) — 받는 쪽이 원본과 같은 바이트를 만든다.
        // SDN NIC(계획 65)는 그 네트워크의 서브넷으로 주소 설정을 만든다.
        cloud_init: match vm.spec.cloud_init.as_ref() {
            Some(ci) => {
                let nics = hardware.map_or(&vm.spec.nics[..], |hw| hw.boot_nics());
                Some(cloud_init::render_sdn(
                    vm.id,
                    &vm.name,
                    ci,
                    nics,
                    &sdn_subnets(cluster, nics)?,
                ))
            }
            None => None,
        },
        // TPM(계획 59)은 이 노드에 있는 상태로 켠다(`fresh: false` — 몰래 새로 만들지 않는다). 새로 켤 때는 작업자가 켜기
        // 규칙(`firmware::tpm_source`)으로 상태를 얻은 뒤 바꾼다. 다시 붙거나 받을 때는 기록의 장치를 따른다.
        tpm: (vm.spec.tpm || hardware.is_some_and(|hw| hw.tpm)).then_some(RuntimeTpm {
            fresh: false,
            epoch: vm.status.firmware_epoch,
        }),
        secure_boot: vm.spec.secure_boot,
        freeze_fs: vm.spec.guest_agent.is_some_and(|g| g.freeze_fs),
        firmware_epoch: vm.status.firmware_epoch,
        // 새로 켤 때 작업자가 이 노드의 TPM 파일이 지금 것인지 보고 정한다(리뷰 2).
        keep_tpm: false,
        cpu_resolved: None,
        numa: None,
        host_placement: None,
        pinning: None,
    };
    set_boot_order(&mut spec);
    if let Some(hw) = hardware {
        apply_hardware(&mut spec, hw, &vm.spec);
    }
    Ok(spec)
}

/// 런타임 정의의 디스크·CD-ROM `boot_order`(스펙 값)를 실제 부팅 순서([`effective_boot`])로 바꾼다. CD가 없으면
/// 그대로다.
fn set_boot_order(spec: &mut VmRuntimeSpec) {
    let disks: Vec<_> = spec.disks.iter().map(|d| d.boot_order).collect();
    let cdroms: Vec<_> = spec.cdroms.iter().map(|c| c.boot_order).collect();
    let (disks, cdroms) = effective_boot(&disks, &cdroms);
    for (disk, order) in spec.disks.iter_mut().zip(disks) {
        disk.boot_order = order;
    }
    for (cdrom, order) in spec.cdroms.iter_mut().zip(cdroms) {
        cdrom.boot_order = order;
    }
}

/// 런타임 정의를 실행 중 하드웨어 기록의 모양(켤 때의 cpu와 해석한 CPU·메모리·펌웨어·머신과 핫플러그 배치, CD-ROM 부팅 순서,
/// cloud-init 씨앗 드라이브 유무, TPM·Secure Boot·게스트 에이전트 채널)으로 덮는다. 스펙은 실행 중에 바뀌어도(다음 재시작 때 적용) 마이그레이션 대상은 원본
/// QEMU와 같은 장치로 켜야 한다. CD 매체는 [`runtime_spec`]이 기록의 것으로 이미 풀었다(드라이브 수가 다르면 부팅
/// 순서는 덮지 않는다).
///
/// 게스트 에이전트 채널: 계획 59 전 기록에는 `guest_agent`가 없다. VM 스펙에 `guest_agent`가 없고 기록이 거짓이면
/// 채널은 실행 중에 바꿀 수 없는 `ha.guest_watch`가 정한 그대로라 `vm`의 채널로 본다(`hotplug::devices_match`와 같은 규칙).
pub fn apply_hardware(spec: &mut VmRuntimeSpec, hw: &RunningHardware, vm: &VmSpec) {
    spec.cpu = hw.cpu.clone();
    // 켤 때 해석한 CPU(계획 63): 받는 쪽은 이 노드의 기준·숨김 목록이 달라도 원본과 같은 `-cpu`로 켠다.
    spec.cpu_resolved = hw.cpu_resolved.clone();
    // 켤 때의 게스트 NUMA 모양(계획 64): 받는 쪽도 같은 메모리 백엔드·노드로 켠다. 배치(`host-nodes`)는 받는 쪽이 다시
    // 고른다(작업자·받는 쪽이 덮는다).
    spec.numa = hw.numa.clone();
    spec.host_placement = hw.host_placement.clone();
    spec.memory_mib = hw.memory_mib;
    spec.firmware = hw.firmware;
    spec.machine = hw.machine;
    spec.hotplug = hw.layout.clone();
    // 장치 셋(계획 59). TPM이 기록에만 있으면 이 노드의 상태로 켠다(회차는 스펙에서 정한 값, 없으면 0).
    spec.tpm = hw.tpm.then(|| {
        spec.tpm.unwrap_or(RuntimeTpm {
            fresh: false,
            epoch: 0,
        })
    });
    spec.secure_boot = hw.secure_boot;
    spec.guest_agent = if vm.guest_agent.is_none() && !hw.guest_agent {
        vm.agent_channel()
    } else {
        hw.guest_agent
    };
    if !hw.cdroms.is_empty()
        && hw.cdroms.len() == spec.cdroms.len()
        && hw.disks.len() == spec.disks.len()
    {
        // 원본 QEMU의 `bootindex`: 켤 때 단 디스크와 CD로 셈한 순서이고, 실행 중 붙인 디스크는 스펙 값 그대로다.
        let boot = hw
            .layout
            .as_ref()
            .map_or(hw.disks.len(), |l| usize::from(l.boot_disks))
            .min(hw.disks.len());
        let disks: Vec<_> = hw.disks[..boot].iter().map(|d| d.boot_order).collect();
        let cdroms: Vec<_> = hw.cdroms.iter().map(|c| c.boot_order).collect();
        let (disks, cdroms) = effective_boot(&disks, &cdroms);
        let hot = hw.disks[boot..].iter().map(|d| d.boot_order);
        for (disk, order) in spec.disks.iter_mut().zip(disks.into_iter().chain(hot)) {
            disk.boot_order = order;
        }
        for (cdrom, order) in spec.cdroms.iter_mut().zip(cdroms) {
            cdrom.boot_order = order;
        }
    }
    if hw.cloud_init.is_none() {
        spec.cloud_init = None;
    }
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

/// 실행 중 VM의 CD-ROM 드라이브 하나(`vm.spec.cdroms[index]`)에 넣을 매체를 VM을 켤 때처럼 푼다(풀 검사·볼륨
/// 켜기·위치). 빈 드라이브면 None이다. ISO는 늘리지 않으므로 크기 늘리기 Task를 기다리지 않는다. 오류는
/// [`runtime_spec`]과 같고, 실패하면 켠 볼륨을 끈다.
pub async fn runtime_medium(
    cluster: &Cluster,
    vm: &Vm,
    index: usize,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
) -> Result<Option<RuntimeMedium>, ResolveError> {
    let cdrom = vm
        .spec
        .cdroms
        .get(index)
        .ok_or(ResolveError::NoCdrom(index))?;
    let Some(iso) = &cdrom.iso else {
        return Ok(None);
    };
    let lookup = DiskLookup::new(cluster, vm, node, mount_root, storage, Activate::Running)?;
    let mut activated = Vec::new();
    let result = lookup.medium(iso, &mut activated).await;
    if result.is_err() {
        deactivate(vm, storage, activated).await;
    }
    result.map(Some)
}

/// NIC들 `nics`가 쓰는 SDN 네트워크마다 그 스펙과 라우터 인터페이스인지(cloud-init network-config용, 계획 65·66). 단순
/// 네트워크는 넣지 않는다.
fn sdn_subnets(
    cluster: &Cluster,
    nics: &[orbivisor_types::vm::NicSpec],
) -> Result<cloud_init::SdnSubnets, ResolveError> {
    let mut out = cloud_init::SdnSubnets::new();
    let mut routers: Option<Vec<orbivisor_types::router::Router>> = None;
    for nic in nics.iter().filter(|n| n.network != USER_NETWORK) {
        if out.contains_key(&nic.network) {
            continue;
        }
        if let Some(net) = cluster.get_by_name::<NetworkSpec>(&nic.network)?
            && net.spec.is_sdn()
        {
            // 라우터 인터페이스인 오버레이 네트워크(계획 66): 고정 주소에 게이트웨이, IPv6 모드에 RA가 있다.
            let routers = match &mut routers {
                Some(r) => r,
                None => routers.insert(cluster.list::<orbivisor_types::router::RouterSpec>()?.1),
            };
            let routed = !net.spec.is_provider()
                && routers
                    .iter()
                    .any(|r| r.spec.interfaces.iter().any(|i| i.network == nic.network));
            out.insert(
                nic.network.clone(),
                cloud_init::SdnNicNetwork {
                    spec: net.spec,
                    routed,
                },
            );
        }
    }
    Ok(out)
}

/// NIC 하나(`vm.spec.nics[index]`)의 런타임 정의. tap 이름은 순번으로 정한다([`tap_name`]). SDN(OVN) 네트워크면
/// `bridge`가 `br-int`이고 논리 포트 이름(`ovn_port`, OVS 포트의 `iface-id`)을 단다(계획 65). 단순 NIC의 속도 제한(`qos`)은
/// 드라이버가 tap의 tc로 건다(계획 67 §8.2).
pub fn runtime_nic(cluster: &Cluster, vm: &Vm, index: usize) -> Result<RuntimeNic, ResolveError> {
    let nic = vm.spec.nics.get(index).ok_or(ResolveError::NoNic(index))?;
    let mac = || nic.mac.ok_or(ResolveError::MissingMac(index));
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
            ovn_port: network
                .spec
                .is_sdn()
                .then(|| mac().map(|m| orbivisor_types::sdn::port_name(vm.id, m)))
                .transpose()?,
            // 단순 NIC의 속도 제한만 tap의 tc로(계획 67 §8.2). SDN NIC는 OVN `QoS` 행이 한다.
            qos: nic.qos.filter(|q| q.has_rate() && !network.spec.is_sdn()),
        }
    };
    Ok(RuntimeNic {
        mac: mac()?,
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
        let (cluster, mount_root, storage, how) =
            (self.cluster, self.mount_root, self.storage, self.how);
        let (tasks, moves) = (&self.tasks, self.moves);
        let volume = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        let moved = moves.iter().find(|m| m.volume == volume.id);
        if how != Activate::Running {
            let running = |op: fn(&TaskOp) -> bool| {
                tasks.iter().any(|t| {
                    t.spec.target == volume.id
                        && t.status.state == TaskState::Running
                        && op(&t.spec.op)
                })
            };
            if running(|op| matches!(op, TaskOp::VolumeResize { .. })) {
                return Err(ResolveError::VolumeResizing(disk.volume.clone()));
            }
            // 평탄화하는 동안 QEMU가 열면 평탄화가 옮기는 데이터 뒤에 쓴 것을 잃는다(ZFS는 원본을 지운다).
            if running(|op| *op == TaskOp::VolumeFlatten) {
                return Err(ResolveError::VolumeFlattening(disk.volume.clone()));
            }
        }
        let pool_name = moved.map_or(&volume.spec.pool, |m| &m.pool);
        let pool = self.usable_pool(pool_name)?;
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

    /// 볼륨이 든 풀 `pool_name`을 이 노드에서 쓸 수 있는지 본다: 풀이 있고 이 노드에 나열되고, 이 OS가 그 백엔드를
    /// 쓰고(macOS는 dir만), 이 노드가 준비되지 않았다고 알리지 않았다.
    fn usable_pool(&self, pool_name: &str) -> Result<StoragePool, ResolveError> {
        let (cluster, node, me) = (self.cluster, self.node, &self.me);
        let pool = cluster
            .get_by_name::<StoragePoolSpec>(pool_name)?
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.to_string()))?;
        if !pool_listed_on(&pool, node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.to_string()));
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM(공유 LVM 포함)·ZFS·RBD를 쓴다(Homebrew QEMU에는
        // rbd가 없다).
        if !cfg!(target_os = "linux") {
            match pool.spec.backend {
                PoolBackend::Nfs { .. } => {
                    return Err(ResolveError::NfsUnsupported(pool_name.to_string()));
                }
                PoolBackend::LvmThin { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.to_string(),
                        "lvm-thin pools are not supported on macOS",
                    ));
                }
                PoolBackend::Zfs { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.to_string(),
                        "zfs pools are not supported on macOS",
                    ));
                }
                PoolBackend::Rbd { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.to_string(),
                        "rbd pools are not supported on macOS",
                    ));
                }
                PoolBackend::LvmShared { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.to_string(),
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
            return Err(ResolveError::PoolUnavailable(pool_name.to_string(), error));
        }
        Ok(pool)
    }

    /// CD-ROM 매체(ISO 볼륨 `iso`)를 켜고 위치를 정한다. 켠 볼륨을 `activated`에 적는다. ISO가 아니거나 파일 풀(dir·
    /// NFS)이 아닌 풀의 볼륨이면 `VolumeUnusable`이다. 매체는 읽기 전용이라 여러 VM이 함께 쓰고, 받는 마이그레이션도
    /// 그대로 켠다(원본과 같은 파일을 함께 연다).
    async fn medium(
        &self,
        iso: &str,
        activated: &mut Vec<(StoragePool, Uuid, ImageFormat)>,
    ) -> Result<RuntimeMedium, ResolveError> {
        let volume = self
            .cluster
            .get_by_name::<VolumeSpec>(iso)?
            .ok_or_else(|| ResolveError::VolumeNotFound(iso.to_string()))?;
        let unusable = |why: String| {
            ResolveError::VolumeUnusable(iso.to_string(), DriverError::InvalidSpec(why))
        };
        if volume.spec.content != VolumeContent::Iso {
            return Err(unusable(format!("volume {iso:?} is not an ISO image")));
        }
        let pool = self.usable_pool(&volume.spec.pool)?;
        if !pool_holds_isos(&pool.spec.backend) {
            return Err(unusable(
                "ISO images can be kept only in dir or nfs pools".into(),
            ));
        }
        let format = volume.spec.format;
        let located = match self.storage.activate_volume(&pool, volume.id, format).await {
            Ok(()) => {
                activated.push((pool.clone(), volume.id, format));
                disk_location(&pool, self.mount_root, volume.id, format)
            }
            Err(e) => Err(e),
        };
        match located {
            Ok(DiskLocation::Path(path, DiskKind::File)) => Ok(RuntimeMedium { path }),
            Ok(_) => Err(unusable("ISO images must be files".into())),
            Err(e @ (DriverError::VolumeNotFound(_) | DriverError::InvalidSpec(_))) => {
                Err(ResolveError::VolumeUnusable(iso.to_string(), e))
            }
            Err(e) => Err(ResolveError::PoolUnavailable(
                volume.spec.pool.clone(),
                e.to_string(),
            )),
        }
    }
}
