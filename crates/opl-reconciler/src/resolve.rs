//! VM spec의 이름 참조(Volume, Network)를 풀어 드라이버가 받는 `VmRuntimeSpec`을 만든다.

use std::path::Path;

use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DirStorage, check_pool_root};
use opl_types::network::{NetworkSpec, USER_NETWORK, tap_name};
use opl_types::node::NodeSpec;
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{
    PoolBackend, StoragePoolSpec, VolumeSpec, pool_error_on, pool_listed_on, pool_state_on,
};
use opl_types::vm::Vm;
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
    /// 이 노드의 OS(macOS)는 NFS 풀을 지원하지 않는다. 기다려도 풀리지 않는다.
    #[error("storage pool {0:?} is an nfs pool, which is not supported on this node's OS")]
    NfsUnsupported(String),
    #[error("network {0:?} not found")]
    NetworkNotFound(String),
    #[error("nic {0} has no MAC address")]
    MissingMac(usize),
    #[error(transparent)]
    Cluster(#[from] ClusterError),
}

/// 로컬 저장소에서 참조를 찾아 런타임 정의를 만든다. `node`는 이 노드의 id이고
/// `mount_root`는 이 노드가 NFS 풀을 마운트하는 디렉터리다. 이 노드가 준비되지 않았다고 알린 풀은
/// `PoolUnavailable`(잠시 뒤 다시 시도할 일)로 거절한다. 아직 알리지 않은 풀(opld가 막 시작해 첫 새로
/// 고침 전)은 막지 않되, NFS 풀은 실제로 마운트돼 있는지 확인한다. NFS를 지원하지 않는 노드에서
/// NFS 풀은 `NfsUnsupported`(참조 오류)다.
pub fn runtime_spec(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
) -> Result<VmRuntimeSpec, ResolveError> {
    let me = cluster.get::<NodeSpec>(node)?;
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
    for disk in &vm.spec.disks {
        let volume = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        let pool_name = &volume.spec.pool;
        let pool = cluster
            .get_by_name::<StoragePoolSpec>(pool_name)?
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        if !pool_listed_on(&pool, node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
        // opld는 Linux에서만 NFS 풀을 마운트한다.
        if !cfg!(target_os = "linux") && matches!(pool.spec.backend, PoolBackend::Nfs { .. }) {
            return Err(ResolveError::NfsUnsupported(pool_name.clone()));
        }
        if let Some(me) = &me
            && pool_state_on(&pool, me).is_some_and(|s| !s.ready)
        {
            let error = pool_error_on(&pool, me);
            return Err(ResolveError::PoolUnavailable(pool_name.clone(), error));
        }
        if matches!(pool.spec.backend, PoolBackend::Nfs { .. })
            && let Err(e) = check_pool_root(&pool, mount_root)
        {
            return Err(ResolveError::PoolUnavailable(
                pool_name.clone(),
                e.to_string(),
            ));
        }
        disks.push(RuntimeDisk {
            path: DirStorage::volume_path(&pool, mount_root, volume.id, volume.spec.format),
            format: volume.spec.format,
            bus: disk.bus,
            boot_order: disk.boot_order,
        });
    }
    let mut nics = Vec::with_capacity(vm.spec.nics.len());
    for (i, nic) in vm.spec.nics.iter().enumerate() {
        let backend = if nic.network == USER_NETWORK {
            NicBackend::User
        } else {
            let network = cluster
                .get_by_name::<NetworkSpec>(&nic.network)?
                .ok_or_else(|| ResolveError::NetworkNotFound(nic.network.clone()))?;
            NicBackend::Tap {
                ifname: tap_name(vm.id, i),
                bridge: network.spec.bridge,
                vlan: network.spec.vlan,
            }
        };
        nics.push(RuntimeNic {
            mac: nic.mac.ok_or(ResolveError::MissingMac(i))?,
            model: nic.model,
            backend,
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
