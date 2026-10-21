//! VM spec의 이름 참조(Volume, Network)를 풀어 드라이버가 받는 `VmRuntimeSpec`을 만든다.

use opl_cluster::{Cluster, ClusterError};
use opl_driver::DirStorage;
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::Vm;
use uuid::Uuid;

/// `Network` 리소스가 생기기 전까지 쓸 수 있는 유일한 네트워크 이름. QEMU user-mode NAT다.
pub const USER_NETWORK: &str = "user";

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("volume {0:?} not found")]
    VolumeNotFound(String),
    #[error("storage pool {0:?} not found")]
    PoolNotFound(String),
    #[error("storage pool {0:?} is not available on this node")]
    PoolNotOnNode(String),
    #[error("network {0:?} not found (only {USER_NETWORK:?} is supported for now)")]
    NetworkNotFound(String),
    #[error("nic {0} has no MAC address")]
    MissingMac(usize),
    #[error(transparent)]
    Cluster(#[from] ClusterError),
}

/// 로컬 저장소에서 참조를 찾아 런타임 정의를 만든다. `node`는 이 노드의 id다.
pub fn runtime_spec(cluster: &Cluster, vm: &Vm, node: Uuid) -> Result<VmRuntimeSpec, ResolveError> {
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
    for disk in &vm.spec.disks {
        let volume = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        let pool_name = &volume.spec.pool;
        let pool = cluster
            .get_by_name::<StoragePoolSpec>(pool_name)?
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        if !pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
        disks.push(RuntimeDisk {
            path: DirStorage::volume_path(&pool.spec, volume.id, volume.spec.format),
            format: volume.spec.format,
            bus: disk.bus,
            boot_order: disk.boot_order,
        });
    }
    let mut nics = Vec::with_capacity(vm.spec.nics.len());
    for (i, nic) in vm.spec.nics.iter().enumerate() {
        if nic.network != USER_NETWORK {
            return Err(ResolveError::NetworkNotFound(nic.network.clone()));
        }
        nics.push(RuntimeNic {
            mac: nic.mac.ok_or(ResolveError::MissingMac(i))?,
            model: nic.model,
            backend: NicBackend::User,
        });
    }
    Ok(VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks,
        nics,
    })
}
