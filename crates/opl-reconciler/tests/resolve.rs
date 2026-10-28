//! VM spec의 이름 참조를 드라이버용 경로와 백엔드로 푸는 규칙.

mod common;

use std::path::Path;

use opl_cluster::memnet::MemRouter;
use opl_reconciler::resolve::{ResolveError, runtime_spec};
use opl_types::network::{Network, NetworkSpec, tap_name};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, NicBackend, RuntimeDisk, RuntimeNic};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{DiskBus, DiskSpec, MacAddr, NicModel, NicSpec};
use tempfile::TempDir;
use uuid::Uuid;

/// NFS 풀을 마운트하는 디렉터리
const MNT: &str = "/var/lib/opl/pools";

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

    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap();
    assert_eq!(spec.id, vm.id);
    assert_eq!(
        spec.disks,
        vec![RuntimeDisk {
            path: format!("/pools/local/{}.qcow2", root.id).into(),
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

#[tokio::test]
async fn resolves_nfs_volumes_under_mount_point() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
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
    let root = volume("root", "nas");
    cluster.create(&root).await.unwrap();

    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap();
    assert_eq!(
        spec.disks[0].path,
        Path::new(MNT)
            .join(nas.id.to_string())
            .join(format!("{}.qcow2", root.id))
    );
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
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap();
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

    let check = |disks: Vec<DiskSpec>, nics: Vec<NicSpec>| {
        let mut vm = common::vm("web", node);
        vm.spec.disks = disks;
        vm.spec.nics = nics;
        runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap_err()
    };
    assert!(matches!(
        check(vec![disk("nope")], vec![]),
        ResolveError::VolumeNotFound(n) if n == "nope"
    ));
    assert!(matches!(
        check(vec![disk("orphan")], vec![]),
        ResolveError::PoolNotFound(n) if n == "deleted-pool"
    ));
    assert!(matches!(
        check(vec![disk("remote")], vec![]),
        ResolveError::PoolNotOnNode(n) if n == "elsewhere"
    ));
    assert!(matches!(
        check(vec![], vec![nic("lan", Some(MacAddr::generate()))]),
        ResolveError::NetworkNotFound(n) if n == "lan"
    ));
    assert!(matches!(
        check(vec![], vec![nic("user", None)]),
        ResolveError::MissingMac(0)
    ));
}
