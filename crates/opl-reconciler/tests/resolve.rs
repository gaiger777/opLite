//! VM spec의 이름 참조를 드라이버용 경로와 백엔드로 푸는 규칙.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use opl_cluster::memnet::MemRouter;
use opl_driver::{Capacity, DriverError, SourceFormat, Storage, VolumeInfo, VolumeSource};
use opl_reconciler::resolve::{ResolveError, runtime_spec};
use opl_types::network::{Network, NetworkSpec, tap_name};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, RuntimeDisk, RuntimeNic};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{DiskBus, DiskSpec, MacAddr, NicModel, NicSpec, Vm};
use tempfile::TempDir;
use uuid::Uuid;

/// NFS 풀을 마운트하는 디렉터리
const MNT: &str = "/var/lib/opl/pools";

/// 활성화한 볼륨을 적는 저장소. `fail`이 있으면 활성화가 그 오류로 실패한다.
/// resolve는 활성화만 부른다.
#[derive(Default)]
struct Activations {
    calls: Mutex<Vec<(String, Uuid, ImageFormat)>>,
    fail: Mutex<Option<fn() -> DriverError>>,
}

#[async_trait]
impl Storage for Activations {
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls
            .lock()
            .unwrap()
            .push((pool.name.clone(), id, format));
        match *self.fail.lock().unwrap() {
            Some(fail) => Err(fail()),
            None => Ok(()),
        }
    }
    async fn create_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn import_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: &Path,
        _: SourceFormat,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn delete_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_info(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        unreachable!()
    }
    async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
        unreachable!()
    }
    fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
        unreachable!()
    }
    async fn snapshot_create(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        unreachable!()
    }
    async fn resize_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: u64,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        unreachable!()
    }
    async fn release_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn copy_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePool,
        _: Uuid,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
}

async fn resolve(
    cluster: &opl_cluster::Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
) -> Result<opl_types::runtime::VmRuntimeSpec, ResolveError> {
    runtime_spec(cluster, vm, node, mount_root, &Activations::default()).await
}

fn pool(name: &str, nodes: Vec<Uuid>) -> StoragePool {
    Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: format!("/pools/{name}").into(),
            },
            nodes,
            shared: false,
        },
    )
}

fn volume(name: &str, pool: &str) -> Volume {
    Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

fn disk(volume: &str) -> DiskSpec {
    DiskSpec {
        volume: volume.into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    }
}

fn nic(network: &str, mac: Option<MacAddr>) -> NicSpec {
    NicSpec {
        network: network.into(),
        model: NicModel::Virtio,
        mac,
    }
}

#[tokio::test]
async fn resolves_volumes_to_pool_paths_and_user_network() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let root = volume("root", "local");
    cluster.create(&root).await.unwrap();

    let mac: MacAddr = "52:54:00:00:00:01".parse().unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.nics.push(nic("user", Some(mac)));

    let spec = resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
    assert_eq!(spec.id, vm.id);
    assert_eq!(
        spec.disks,
        vec![RuntimeDisk {
            path: format!("/pools/local/{}.qcow2", root.id).into(),
            kind: DiskKind::File,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }]
    );
    assert_eq!(
        spec.nics,
        vec![RuntimeNic {
            mac,
            model: NicModel::Virtio,
            backend: NicBackend::User,
        }]
    );
}

/// 디스크로 쓰기 전에 저장소가 볼륨을 활성화한다. 활성화가 실패하면 다시 시도할 `PoolUnavailable`이고,
/// 볼륨이 없거나 정의가 틀려 다시 해도 안 되면 VM을 끌 `VolumeUnusable`이다.
#[tokio::test]
async fn activates_volumes_before_use() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let (root, data) = (volume("root", "local"), volume("data", "local"));
    cluster.create(&root).await.unwrap();
    cluster.create(&data).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));

    let storage = Activations::default();
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(spec.disks.len(), 2);
    let qcow2 = ImageFormat::Qcow2;
    assert_eq!(
        *storage.calls.lock().unwrap(),
        vec![
            ("local".to_string(), root.id, qcow2),
            ("local".to_string(), data.id, qcow2)
        ]
    );

    *storage.fail.lock().unwrap() = Some(|| DriverError::Timeout("lvchange"));
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "local" && e.contains("lvchange")),
        "{err:?}"
    );
    *storage.fail.lock().unwrap() = Some(|| DriverError::VolumeNotFound(Uuid::nil()));
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            ResolveError::VolumeUnusable(n, DriverError::VolumeNotFound(_)) if n == "root"
        ),
        "{err:?}"
    );
    assert!(err.to_string().contains("not found"), "{err}");
    *storage.fail.lock().unwrap() = Some(|| DriverError::InvalidSpec("bad".into()));
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            ResolveError::VolumeUnusable(n, DriverError::InvalidSpec(_)) if n == "root"
        ),
        "{err:?}"
    );
    // 볼륨이 없거나 정의가 틀린 것 말고는 저장소가 실패해도(명령 실패 등) 잠시 뒤 다시 해 본다.
    *storage.fail.lock().unwrap() = Some(|| DriverError::ToolFailed {
        tool: "lvchange",
        stderr: "device busy".into(),
    });
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "local" && e.contains("device busy")),
        "{err:?}"
    );
}

/// NFS 풀 `nas`와 그 볼륨 `root`를 쓰는 VM을 만든다.
async fn nfs_vm(cluster: &opl_cluster::Cluster, node: Uuid) -> (StoragePool, opl_types::vm::Vm) {
    let nas: StoragePool = Resource::new(
        "nas",
        StoragePoolSpec {
            backend: PoolBackend::Nfs {
                server: "nas".into(),
                export: "/srv/vm".into(),
                options: None,
            },
            nodes: vec![],
            shared: true,
        },
    );
    cluster.create(&nas).await.unwrap();
    cluster.create(&volume("root", "nas")).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    (nas, vm)
}

/// NFS를 지원하지 않는 노드(macOS)에서는 NFS 풀의 디스크를 쓸 수 없다. 기다려도 풀리지 않으므로
/// 다시 시도할 `PoolUnavailable`이 아니라 참조 오류다(VM을 끄고 이유를 남긴다).
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn refuses_nfs_volumes_where_nfs_is_unsupported() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (_, vm) = nfs_vm(&cluster, node).await;
    let err = resolve(&cluster, &vm, node, Path::new(MNT))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::NfsUnsupported(n) if n == "nas"),
        "{err:?}"
    );
    assert!(err.to_string().contains("not supported"), "{err}");
}

/// NFS 풀의 디스크는 마운트 지점 아래에 있다. 마운트되지 않았으면(빈 마운트 지점) 노드가 아직
/// 상태를 알리지 않았어도 잠시 뒤 다시 시도할 `PoolUnavailable`이다. 경로는 `pool_root`가 정한다.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn refuses_nfs_volumes_until_mounted() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (nas, vm) = nfs_vm(&cluster, node).await;
    let mnt = TempDir::new().unwrap();
    std::fs::create_dir(mnt.path().join(nas.id.to_string())).unwrap();
    let err = resolve(&cluster, &vm, node, mnt.path()).await.unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "nas" && e.contains("not mounted")),
        "{err:?}"
    );
}

/// LVM-thin 풀의 디스크는 아직 쓸 수 없다. 기다려도 풀리지 않으므로 참조 오류다(VM을 끄고 이유를 남긴다).
#[tokio::test]
async fn refuses_lvm_thin_volumes_for_now() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let vg: StoragePool = Resource::new(
        "vg",
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    cluster.create(&vg).await.unwrap();
    cluster.create(&volume("root", "vg")).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    let err = resolve(&cluster, &vm, node, Path::new(MNT))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, _) if n == "vg"),
        "{err:?}"
    );
    assert!(err.to_string().contains("not supported"), "{err}");
}

#[tokio::test]
async fn resolves_bridge_networks_to_taps() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let lan: Network = Resource::new(
        "lan",
        NetworkSpec {
            bridge: "vmbr0".into(),
            vlan: Some(10),
        },
    );
    cluster.create(&lan).await.unwrap();

    let mut vm = common::vm("web", node);
    vm.spec.nics.push(nic("user", Some(MacAddr::generate())));
    vm.spec.nics.push(nic("lan", Some(MacAddr::generate())));
    let spec = resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
    assert_eq!(spec.nics[0].backend, NicBackend::User);
    // tap 이름은 NIC 순번으로 정한다.
    assert_eq!(
        spec.nics[1].backend,
        NicBackend::Tap {
            ifname: tap_name(vm.id, 1),
            bridge: "vmbr0".into(),
            vlan: Some(10),
        }
    );
}

#[tokio::test]
async fn reports_unresolvable_references() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster
        .create(&pool("elsewhere", vec![Uuid::now_v7()]))
        .await
        .unwrap();
    cluster
        .create(&volume("remote", "elsewhere"))
        .await
        .unwrap();
    cluster
        .create(&volume("orphan", "deleted-pool"))
        .await
        .unwrap();

    let check = async |disks: Vec<DiskSpec>, nics: Vec<NicSpec>| {
        let mut vm = common::vm("web", node);
        vm.spec.disks = disks;
        vm.spec.nics = nics;
        resolve(&cluster, &vm, node, Path::new(MNT))
            .await
            .unwrap_err()
    };
    assert!(matches!(
        check(vec![disk("nope")], vec![]).await,
        ResolveError::VolumeNotFound(n) if n == "nope"
    ));
    assert!(matches!(
        check(vec![disk("orphan")], vec![]).await,
        ResolveError::PoolNotFound(n) if n == "deleted-pool"
    ));
    assert!(matches!(
        check(vec![disk("remote")], vec![]).await,
        ResolveError::PoolNotOnNode(n) if n == "elsewhere"
    ));
    assert!(matches!(
        check(vec![], vec![nic("lan", Some(MacAddr::generate()))]).await,
        ResolveError::NetworkNotFound(n) if n == "lan"
    ));
    assert!(matches!(
        check(vec![], vec![nic("user", None)]).await,
        ResolveError::MissingMac(0)
    ));
}

#[tokio::test]
async fn refuses_pools_this_node_reports_not_ready() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let mut pools = Vec::new();
    for name in ["down", "unknown", "stale"] {
        let p = pool(name, vec![]);
        cluster.create(&p).await.unwrap();
        pools.push(p);
        cluster
            .create(&volume(&format!("{name}-root"), name))
            .await
            .unwrap();
    }
    let mut me: Node = Resource::new(
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
        },
    );
    me.id = node;
    let down = PoolState {
        pool_id: pools[0].id,
        ready: false,
        error: Some("mount.nfs: access denied".into()),
        ..Default::default()
    };
    me.status.pools.insert("down".into(), down.clone());
    // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
    let stale = PoolState {
        pool_id: Uuid::now_v7(),
        ..down
    };
    me.status.pools.insert("stale".into(), stale);
    cluster.create(&me).await.unwrap();

    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("down-root"));
    let err = resolve(&cluster, &vm, node, Path::new(MNT))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e)
            if n == "down" && e == "mount.nfs: access denied"),
        "{err:?}"
    );
    assert!(err.to_string().contains("access denied"), "{err}");
    // 상태를 아직 알리지 않은 풀(opld가 막 시작함)은 막지 않는다. 파일이 없으면 시작이 실패한다.
    for disk_name in ["unknown-root", "stale-root"] {
        vm.spec.disks = vec![disk(disk_name)];
        resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
    }
}
