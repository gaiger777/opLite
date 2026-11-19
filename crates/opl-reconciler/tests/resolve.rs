//! VM spec의 이름 참조를 드라이버용 경로와 백엔드로 푸는 규칙.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use opl_cluster::memnet::MemRouter;
use opl_driver::{Capacity, DriverError, SourceFormat, Storage, VolumeInfo, VolumeSource};
use opl_reconciler::resolve::{
    ResolveError, apply_disk_moves, apply_hardware, incoming_spec, running_spec, runtime_disk,
    runtime_medium, runtime_nic, runtime_spec, set_disks_shared,
};
use opl_types::cloud_init::{CloudInit, CloudInitNet, IpConfig, render};
use opl_types::network::{Network, NetworkSpec, Vxlan, tap_name};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::{
    DiskKind, HotplugLayout, ImageFormat, NicBackend, RuntimeCdrom, RuntimeDisk, RuntimeMedium,
    RuntimeNic,
};
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeContent, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{
    CdromSpec, CpuSpec, DiskBus, DiskMove, DiskSpec, Firmware, GuestWatch, MacAddr,
    MigrationStatus, NicModel, NicSpec, RunningHardware, Vm, VmHa,
};
use tempfile::TempDir;
use uuid::Uuid;

/// NFS 풀을 마운트하는 디렉터리
const MNT: &str = "/var/lib/opl/pools";

/// 활성화한 볼륨과 끈 볼륨을 적는 저장소. `fail`이 있으면 활성화가 그 오류로 실패한다.
/// resolve는 활성화와 (실패했을 때) 끄기만 부른다. 마이그레이션의 공유(`true`)·배타(`false`)
/// 바꾸기는 `shared`에 적고, `fail`이 있으면 그 오류로 실패한다.
#[derive(Default)]
struct Activations {
    calls: Mutex<Vec<(String, Uuid, ImageFormat)>>,
    deactivated: Mutex<Vec<Uuid>>,
    shared: Mutex<Vec<(Uuid, bool)>>,
    fail: Mutex<Option<fn() -> DriverError>>,
}

impl Activations {
    fn share(&self, id: Uuid, shared: bool) -> Result<(), DriverError> {
        self.shared.lock().unwrap().push((id, shared));
        match *self.fail.lock().unwrap() {
            Some(fail) => Err(fail()),
            None => Ok(()),
        }
    }
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
    async fn deactivate_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.deactivated.lock().unwrap().push(id);
        Ok(())
    }
    async fn share_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.share(id, true)
    }
    async fn exclusive_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.share(id, false)
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
        _: ImageFormat,
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
            backups: false,
            folder: None,
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
            content: Default::default(),
            base: None,
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
        ips: Vec::new(),
        network: network.into(),
        model: NicModel::Virtio,
        mac,
        security_groups: Vec::new(),
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
            rbd: None,
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

/// 게스트 감시(`ha.guest_watch`)가 있는 VM만 게스트 에이전트 채널을 단다. 받는 쪽도 같은 규칙이다.
#[tokio::test]
async fn only_guest_watched_vms_get_a_guest_agent_channel() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let mut vm = common::vm("web", node);
    assert!(
        !resolve(&cluster, &vm, node, Path::new(MNT))
            .await
            .unwrap()
            .guest_agent
    );
    vm.spec.ha = Some(VmHa::default());
    assert!(
        !resolve(&cluster, &vm, node, Path::new(MNT))
            .await
            .unwrap()
            .guest_agent
    );
    vm.spec.ha = Some(VmHa {
        guest_watch: Some(GuestWatch::default()),
        ..Default::default()
    });
    assert!(
        resolve(&cluster, &vm, node, Path::new(MNT))
            .await
            .unwrap()
            .guest_agent
    );
    let storage = Activations::default();
    let incoming = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert!(incoming.guest_agent);
}

/// TPM(계획 59)은 이 노드의 상태로(새로 만들지 않고) VM의 회차와 함께 켠다. 다시 붙을 때는 도는 QEMU의 장치(기록)를
/// 따른다: 실행 중에 TPM·에이전트를 더했으면 없다. 계획 59 전 기록(채널 칸 없음)의 게스트 감시 채널은 그대로다.
#[tokio::test]
async fn tpm_secure_boot_and_agent_channel_follow_the_running_hardware() {
    use opl_types::runtime::RuntimeTpm;
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let mut vm = common::vm("win", node);
    vm.spec.tpm = true;
    vm.spec.secure_boot = true;
    vm.status.firmware_epoch = 4;
    let start = resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
    assert_eq!(
        start.tpm,
        Some(RuntimeTpm {
            fresh: false,
            epoch: 4
        })
    );
    assert!(start.secure_boot);

    // 켤 때는 장치가 없었다(보류): 다시 붙을 때도 도는 QEMU의 장치다(받는 쪽은 장치가 다르면 받지 않는다).
    let mut hw = RunningHardware::legacy(&vm.spec);
    hw.tpm = false;
    hw.secure_boot = false;
    vm.spec.guest_agent = Some(Default::default());
    vm.status.hardware = Some(hw.clone());
    let storage = Activations::default();
    let incoming = running_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(incoming.tpm, None);
    assert!(!incoming.secure_boot);
    assert!(!incoming.guest_agent);
    // 기록에만 TPM이 있다(실행 중에 뺐다).
    vm.spec.tpm = false;
    hw.tpm = true;
    vm.status.hardware = Some(hw.clone());
    let incoming = running_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(
        incoming.tpm,
        Some(RuntimeTpm {
            fresh: false,
            epoch: 4
        })
    );
    // 계획 59 전 기록(guest_agent 없음)의 게스트 감시 채널은 지킨다.
    vm.spec.guest_agent = None;
    vm.spec.ha = Some(VmHa {
        guest_watch: Some(GuestWatch::default()),
        ..Default::default()
    });
    let incoming = running_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert!(incoming.guest_agent);
}

/// 풀다가 실패하면 그때까지 활성화한 볼륨을 끈다(VM을 시작하지 않으니 켜 둘 까닭이 없다).
/// 성공하면 끄지 않는다.
#[tokio::test]
async fn failed_resolve_deactivates_the_volumes_it_activated() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let (root, data) = (volume("root", "local"), volume("data", "local"));
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));

    // 둘째 디스크의 볼륨이 없다.
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::VolumeNotFound(_)), "{err:?}");
    assert_eq!(*storage.deactivated.lock().unwrap(), [root.id]);

    // 디스크를 모두 활성화한 뒤 NIC에서 실패해도 모두 끈다.
    cluster.create(&data).await.unwrap();
    vm.spec.nics.push(nic("missing", None));
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::NetworkNotFound(_)), "{err:?}");
    assert_eq!(*storage.deactivated.lock().unwrap(), [root.id, data.id]);

    vm.spec.nics.clear();
    let storage = Activations::default();
    runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert!(storage.deactivated.lock().unwrap().is_empty());
}

/// 마이그레이션을 받을 때는 볼륨을 활성화하지 않고 공유로 켠다(원본이 아직 쓴다). 실패하면 켠
/// 볼륨을 끈다.
#[tokio::test]
async fn incoming_spec_shares_the_volumes() {
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
    let spec = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(spec.disks.len(), 2);
    assert!(storage.calls.lock().unwrap().is_empty());
    assert_eq!(
        *storage.shared.lock().unwrap(),
        [(root.id, true), (data.id, true)]
    );
    assert!(storage.deactivated.lock().unwrap().is_empty());

    vm.spec.nics.push(nic("missing", None));
    let storage = Activations::default();
    let err = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::NetworkNotFound(_)), "{err:?}");
    assert_eq!(*storage.deactivated.lock().unwrap(), [root.id, data.id]);
}

/// 받는 마이그레이션이 복사하는 디스크는 이 노드에 새로 만든 볼륨이다: 계획의 풀·형식으로 켜고
/// (공유가 아니다) 그 경로·형식을 쓴다. 나머지 디스크는 지금처럼 공유로 켠다.
#[tokio::test]
async fn incoming_spec_activates_the_copied_disks_in_the_planned_pool() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    cluster.create(&pool("fast", vec![node])).await.unwrap();
    let (root, data) = (volume("root", "local"), volume("data", "local"));
    cluster.create(&root).await.unwrap();
    cluster.create(&data).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));
    vm.status.migration = Some(MigrationStatus {
        node,
        task: Uuid::now_v7(),
        disks: vec![DiskMove {
            volume: data.id,
            pool: "fast".into(),
            format: ImageFormat::Raw,
            existing: false,
        }],
    });

    let storage = Activations::default();
    let spec = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(spec.disks[0].format, ImageFormat::Qcow2);
    assert_eq!(
        spec.disks[1].path,
        PathBuf::from(format!("/pools/fast/{}.raw", data.id))
    );
    assert_eq!(spec.disks[1].format, ImageFormat::Raw);
    assert_eq!(*storage.shared.lock().unwrap(), [(root.id, true)]);
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [("fast".to_string(), data.id, ImageFormat::Raw)]
    );

    // 시작(받기가 아님)은 계획을 보지 않는다.
    let storage = Activations::default();
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(spec.disks[1].format, ImageFormat::Qcow2);
    assert_eq!(storage.calls.lock().unwrap()[1].0, "local");
}

/// 계획의 노드로 확정된 VM이면 볼륨 기록(풀·형식·만든 노드)을 옮긴 위치로 고친다. 두 번 해도 같고,
/// 확정되지 않았으면 고치지 않는다.
#[tokio::test]
async fn apply_disk_moves_rewrites_the_volumes_once_committed() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let (source, target) = (Uuid::now_v7(), Uuid::now_v7());
    let mut root = volume("root", "local");
    root.status.node = Some(source);
    let mut data = volume("data", "local");
    data.status.node = Some(source);
    cluster.create(&root).await.unwrap();
    cluster.create(&data).await.unwrap();
    let mut vm = common::vm("web", source);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));
    vm.status.migration = Some(MigrationStatus {
        node: target,
        task: Uuid::now_v7(),
        disks: vec![
            DiskMove {
                volume: root.id,
                pool: "local".into(),
                format: ImageFormat::Qcow2,
                existing: false,
            },
            DiskMove {
                volume: data.id,
                pool: "fast".into(),
                format: ImageFormat::Raw,
                existing: false,
            },
        ],
    });
    let read = |id| cluster.get::<VolumeSpec>(id).unwrap().unwrap();

    // 확정 전에는 그대로다.
    let before = read(root.id).revision;
    apply_disk_moves(&cluster, &vm).await.unwrap();
    assert_eq!(read(root.id).revision, before);
    assert_eq!(read(data.id).status.node, Some(source));

    vm.spec.node = Some(target);
    apply_disk_moves(&cluster, &vm).await.unwrap();
    let (r, d) = (read(root.id), read(data.id));
    assert_eq!(r.spec, root.spec);
    assert_eq!(r.status.node, Some(target));
    assert_eq!(d.spec.pool, "fast");
    assert_eq!(d.spec.format, ImageFormat::Raw);
    assert_eq!(d.spec.size_bytes, data.spec.size_bytes);
    assert_eq!(d.status.node, Some(target));
    apply_disk_moves(&cluster, &vm).await.unwrap();
    assert_eq!(read(root.id).revision, r.revision);
    assert_eq!(read(data.id).revision, d.revision);
}

/// 로컬 디스크 마이그레이션은 사슬 전체를 읽어 쓴 독립 볼륨을 만드므로 확정이 연결 복제 기록(`base`)을 지운다(계획 58).
/// 같은 풀·형식이어도 그렇다.
#[tokio::test]
async fn apply_disk_moves_drops_the_base_of_a_linked_clone() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let (source, target) = (Uuid::now_v7(), Uuid::now_v7());
    let mut root = volume("root", "local");
    root.spec.base = Some(Uuid::now_v7());
    root.status.node = Some(source);
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", target);
    vm.spec.disks.push(disk("root"));
    vm.status.migration = Some(MigrationStatus {
        node: target,
        task: Uuid::now_v7(),
        disks: vec![DiskMove {
            volume: root.id,
            pool: "local".into(),
            format: root.spec.format,
            existing: false,
        }],
    });
    apply_disk_moves(&cluster, &vm).await.unwrap();
    let r = cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!(r.spec.base, None);
    assert_eq!(r.spec.pool, "local");
    assert_eq!(r.status.node, Some(target));
}

/// 디스크마다 공유·배타로 바꾼다. 처음 실패에서 멈추고, 저장소의 일시 오류는 `PoolUnavailable`,
/// 없는 볼륨은 `VolumeNotFound`다.
#[tokio::test]
async fn set_disks_shared_switches_every_disk() {
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
    set_disks_shared(&cluster, &vm, &storage, true)
        .await
        .unwrap();
    set_disks_shared(&cluster, &vm, &storage, false)
        .await
        .unwrap();
    assert_eq!(
        *storage.shared.lock().unwrap(),
        [
            (root.id, true),
            (data.id, true),
            (root.id, false),
            (data.id, false)
        ]
    );

    let storage = Activations::default();
    *storage.fail.lock().unwrap() = Some(|| DriverError::Timeout("another node"));
    let err = set_disks_shared(&cluster, &vm, &storage, false)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::PoolUnavailable(..)), "{err:?}");
    assert_eq!(*storage.shared.lock().unwrap(), [(root.id, false)]);

    vm.spec.disks.push(disk("missing"));
    let storage = Activations::default();
    let err = set_disks_shared(&cluster, &vm, &storage, true)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::VolumeNotFound(_)), "{err:?}");
}

/// 디스크 볼륨에 실행 중인 크기 늘리기 Task가 있으면 그 볼륨을 켜지 않고 기다릴
/// `VolumeResizing`이다. 시작과 겹치면 QEMU가 늘리는 중인 볼륨(공유 LVM은 0으로 채우기 전의 늘린
/// 부분)을 열 수 있다. 다른 볼륨의 Task, 끝난 Task, 크기 늘리기가 아닌 Task와 아직 기다리는(Pending)
/// 크기 늘리기는 막지 않는다(기다리는 Task가 VM을 다시 켜야 끝나는 되돌리기 뒤에 서 있을 수 있다).
#[tokio::test]
async fn waits_for_resizes_of_its_disks() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let (root, data, other) = (
        volume("root", "local"),
        volume("data", "local"),
        volume("other", "local"),
    );
    for v in [&root, &data, &other] {
        cluster.create(v).await.unwrap();
    }
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));
    let resize = TaskOp::VolumeResize {
        size_bytes: 2 << 30,
    };
    for (op, target, state) in [
        (resize.clone(), other.id, TaskState::Running),
        (resize.clone(), data.id, TaskState::Failed),
        (TaskOp::VolumeDelete, data.id, TaskState::Pending),
    ] {
        let mut task: Task = Resource::new("", TaskSpec { op, target, node });
        task.name = task.id.to_string();
        task.status = TaskStatus {
            state,
            error: None,
            progress: None,
        };
        cluster.create(&task).await.unwrap();
    }
    resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();

    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: resize,
            target: data.id,
            node,
        },
    );
    task.name = task.id.to_string();
    cluster.create(&task).await.unwrap();
    assert_eq!(task.status.state, TaskState::Pending);
    resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();

    let status = TaskStatus {
        state: TaskState::Running,
        error: None,
        progress: None,
    };
    cluster
        .update_status::<TaskSpec>(task.id, &status)
        .await
        .unwrap();
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ResolveError::VolumeResizing(ref v) if v == "data"),
        "{err:?}"
    );
    assert!(err.to_string().contains("being resized"), "{err}");
    let activated: Vec<Uuid> = storage.calls.lock().unwrap().iter().map(|c| c.1).collect();
    assert!(!activated.contains(&data.id), "{activated:?}");

    let status = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
        progress: None,
    };
    cluster
        .update_status::<TaskSpec>(task.id, &status)
        .await
        .unwrap();
    resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();

    // 연결 복제의 평탄화도 같다(계획 58): 실행 중이면 켜지 않는다.
    let mut flatten: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeFlatten,
            target: root.id,
            node,
        },
    );
    flatten.name = flatten.id.to_string();
    flatten.status = TaskStatus {
        state: TaskState::Running,
        error: None,
        progress: None,
    };
    cluster.create(&flatten).await.unwrap();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &Activations::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ResolveError::VolumeFlattening(ref v) if v == "root"),
        "{err:?}"
    );
    assert!(err.to_string().contains("being flattened"), "{err}");
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
    ); // LVM 명령의 실패(영구 오류로 분류되는 `Lvm`)도 VM을 끄지 않고 다시 해 본다.
    *storage.fail.lock().unwrap() =
        Some(|| DriverError::Lvm("lvchange: Volume group \"vg0\" not found".into()));
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "local" && e.contains("vg0")),
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
            backups: false,
            folder: None,
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

/// LVM-thin 풀 `vg`와 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
async fn lvm_vm(cluster: &opl_cluster::Cluster, node: Uuid) -> (Volume, Vm) {
    let vg: StoragePool = Resource::new(
        "vg",
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    cluster.create(&vg).await.unwrap();
    let mut root = volume("root", "vg");
    root.spec.format = ImageFormat::Raw;
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    (root, vm)
}

/// LVM을 지원하지 않는 노드(macOS)에서는 LVM-thin 풀의 디스크를 쓸 수 없다. 기다려도 풀리지 않으므로
/// 참조 오류다(VM을 끄고 이유를 남긴다). 볼륨을 활성화하지 않는다.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn refuses_lvm_thin_volumes_on_macos() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (_, vm) = lvm_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "vg" && why.contains("macOS")),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// RBD 풀 `r`(Ceph 풀 `rbd`, 네임스페이스 `opl`)과 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
async fn rbd_vm(cluster: &opl_cluster::Cluster, node: Uuid) -> (Volume, Vm) {
    let r: StoragePool = Resource::new(
        "r",
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                user: None,
                conf: Some("/etc/ceph/opl.conf".into()),
            },
            nodes: vec![],
            shared: true,
            backups: false,
            folder: None,
        },
    );
    cluster.create(&r).await.unwrap();
    let mut root = volume("root", "r");
    root.spec.format = ImageFormat::Raw;
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    (root, vm)
}

/// QEMU에 rbd 드라이버가 없는 노드(macOS)에서는 RBD 풀의 디스크를 쓸 수 없다. 기다려도 풀리지
/// 않으므로 참조 오류이고, 볼륨을 활성화하지 않는다.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn refuses_rbd_volumes_on_macos() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (_, vm) = rbd_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "r" && why == &"rbd pools are not supported on macOS"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// Linux에서는 RBD 풀의 디스크를 볼륨을 활성화(이미지 확인)한 뒤 경로 대신 RBD 이미지로 푼다.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn resolves_rbd_volumes_to_images() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (root, vm) = rbd_vm(&cluster, node).await;
    let storage = Activations::default();
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(
        *storage.calls.lock().unwrap(),
        vec![("r".to_string(), root.id, ImageFormat::Raw)]
    );
    assert_eq!(
        spec.disks,
        vec![RuntimeDisk {
            path: PathBuf::new(),
            kind: DiskKind::Rbd,
            rbd: Some(opl_types::runtime::RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image: format!("opl-{}", root.id),
                user: "admin".into(),
                conf: Some("/etc/ceph/opl.conf".into()),
            }),
            format: ImageFormat::Raw,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }]
    );
}

/// 공유 LVM 풀 `s`(VG `san`)와 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
async fn lvm_shared_vm(cluster: &opl_cluster::Cluster, node: Uuid) -> (Volume, Vm) {
    let s: StoragePool = Resource::new(
        "s",
        StoragePoolSpec {
            backend: PoolBackend::LvmShared {
                vg: "san".into(),
                iscsi: None,
            },
            nodes: vec![],
            shared: true,
            backups: false,
            folder: None,
        },
    );
    cluster.create(&s).await.unwrap();
    let mut root = volume("root", "s");
    root.spec.format = ImageFormat::Raw;
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    (root, vm)
}

/// 공유 LVM을 지원하지 않는 노드(macOS)에서는 공유 LVM 풀의 디스크를 쓸 수 없다. 기다려도 풀리지
/// 않으므로 참조 오류이고, 볼륨을 활성화하지 않는다.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn refuses_lvm_shared_volumes_on_macos() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (_, vm) = lvm_shared_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "s" && why == &"lvm-shared pools are not supported on macOS"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// Linux에서는 공유 LVM 볼륨을 (배타) 활성화한 뒤 LV 장치를 디스크로 쓴다. 활성화한 뒤에도 장치
/// 노드가 없으면 잠시 뒤 다시 시도할 `PoolUnavailable`이다.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn activates_lvm_shared_volumes_and_waits_for_the_device() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (root, vm) = lvm_shared_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert_eq!(
        *storage.calls.lock().unwrap(),
        vec![("s".to_string(), root.id, ImageFormat::Raw)]
    );
    let device = format!("/dev/san/opl-{}", root.id);
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "s" && e.contains(&device)),
        "{err:?}"
    );
}

/// ZFS 풀 `z`(데이터셋 `tank/opl`)와 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
async fn zfs_vm(cluster: &opl_cluster::Cluster, node: Uuid) -> (Volume, Vm) {
    let z: StoragePool = Resource::new(
        "z",
        StoragePoolSpec {
            backend: PoolBackend::Zfs {
                dataset: "tank/opl".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    cluster.create(&z).await.unwrap();
    let mut root = volume("root", "z");
    root.spec.format = ImageFormat::Raw;
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    (root, vm)
}

/// ZFS를 지원하지 않는 노드(macOS)에서는 ZFS 풀의 디스크를 쓸 수 없다. 기다려도 풀리지 않으므로
/// 참조 오류이고, 볼륨을 활성화하지 않는다.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn refuses_zfs_volumes_on_macos() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (_, vm) = zfs_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "z" && why == &"zfs pools are not supported on macOS"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// Linux에서는 ZFS 볼륨을 활성화(장치 노드 확인)한 뒤 zvol 장치를 디스크로 쓴다. 장치 노드가 없으면
/// 잠시 뒤 다시 시도할 `PoolUnavailable`이다.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn activates_zfs_volumes_and_waits_for_the_device() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (root, vm) = zfs_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert_eq!(
        *storage.calls.lock().unwrap(),
        vec![("z".to_string(), root.id, ImageFormat::Raw)]
    );
    let device = format!("/dev/zvol/tank/opl/opl-{}", root.id);
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "z" && e.contains(&device)),
        "{err:?}"
    );
}

/// Linux에서는 LVM-thin 볼륨을 활성화한 뒤 LV 장치를 디스크로 쓴다. 활성화한 뒤에도 장치 노드가
/// 없으면 잠시 뒤 다시 시도할 `PoolUnavailable`이다.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn activates_lvm_thin_volumes_and_waits_for_the_device() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (root, vm) = lvm_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert_eq!(
        *storage.calls.lock().unwrap(),
        vec![("vg".to_string(), root.id, ImageFormat::Raw)]
    );
    let device = format!("/dev/vg0/opl-{}", root.id);
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "vg" && e.contains(&device)),
        "{err:?}"
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
            sdn: None,
            subnets: Vec::new(),
            bridge: Some("vmbr0".into()),
            vlan: Some(10),
            vxlan: None,
            mtu: None,
            folder: None,
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
            mtu: None,
        }
    );
}

#[tokio::test]
async fn cloud_init_of_sdn_nics_uses_the_subnets() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let tenant: Network = Resource::new(
        "tenant",
        serde_json::from_value(serde_json::json!({
            "sdn": {}, "subnets": [{ "cidr": "10.0.0.0/24", "dhcp": false }]
        }))
        .unwrap(),
    );
    cluster.create(&tenant).await.unwrap();
    let mut vm = common::vm("web", node);
    let mut sdn_nic = nic("tenant", Some(MacAddr::generate()));
    sdn_nic.ips = vec!["10.0.0.5".parse().unwrap()];
    vm.spec.nics = vec![nic("user", Some(MacAddr::generate())), sdn_nic];
    let ci = CloudInit::default();
    vm.spec.cloud_init = Some(ci.clone());
    let spec = resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
    let seed = spec.cloud_init.unwrap();
    let net: serde_json::Value =
        serde_json::from_str(seed.network_config.as_deref().unwrap()).unwrap();
    assert_eq!(
        net["config"][1]["subnets"],
        serde_json::json!([{ "type": "static", "address": "10.0.0.5/24" }])
    );
    // instance-id는 핫플러그의 견주기(서브넷 없이)와 같다.
    assert_eq!(
        seed.instance_id,
        render(vm.id, "web", &ci, &vm.spec.nics).instance_id
    );
}

#[tokio::test]
async fn resolves_vxlan_networks_to_taps_on_the_managed_bridge() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let blue: Network = Resource::new(
        "blue",
        NetworkSpec {
            sdn: None,
            subnets: Vec::new(),
            bridge: None,
            vlan: None,
            vxlan: Some(Vxlan { vni: 1000 }),
            mtu: None,
            folder: None,
        },
    );
    cluster.create(&blue).await.unwrap();
    let jumbo: Network = Resource::new(
        "jumbo",
        NetworkSpec {
            sdn: None,
            subnets: Vec::new(),
            bridge: None,
            vlan: None,
            vxlan: Some(Vxlan { vni: 2000 }),
            mtu: Some(8950),
            folder: None,
        },
    );
    cluster.create(&jumbo).await.unwrap();

    let mut vm = common::vm("web", node);
    vm.spec.nics.push(nic("blue", Some(MacAddr::generate())));
    vm.spec.nics.push(nic("jumbo", Some(MacAddr::generate())));
    let spec = resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
    // MTU를 정하지 않으면 기본 1450이다.
    assert_eq!(
        spec.nics[0].backend,
        NicBackend::Tap {
            ifname: tap_name(vm.id, 0),
            bridge: "oplv1000".into(),
            vlan: None,
            mtu: Some(1450),
        }
    );
    assert_eq!(
        spec.nics[1].backend,
        NicBackend::Tap {
            ifname: tap_name(vm.id, 1),
            bridge: "oplv2000".into(),
            vlan: None,
            mtu: Some(8950),
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
            host_resources: None,
            address: "a".into(),
            maintenance: false,
            witness: false,
            roles: None,
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

/// 실행 중 하드웨어 기록(`status.hardware` — 원본이 켠 배치·실행 중 바꾼 vCPU·메모리·붙인 장치의 포트)
fn running_hardware(vm: &Vm) -> RunningHardware {
    let mut hw = RunningHardware::legacy(&vm.spec);
    hw.cpu = CpuSpec {
        max_vcpus: Some(8),
        ..vm.spec.cpu.clone()
    };
    hw.memory_mib = 1024;
    hw.firmware = Firmware::Bios;
    hw.layout = Some(HotplugLayout {
        vcpus: 4,
        max_memory_mib: 4096,
        plugged_mib: 2048,
        ports: 8,
        boot_disks: 1,
        boot_nics: 0,
        disk_ports: vec![0],
        nic_ports: Vec::new(),
    });
    hw
}

/// 마이그레이션 대상은 원본이 도는 모양(`status.hardware`)으로 켠다: 실행 중 바뀐 스펙(cpu·메모리·펌웨어)이
/// 아니라 원본 QEMU의 장치와 같아야 받을 수 있다. 기록이 없으면(옛 노드가 켠 VM) 스펙 그대로다. 다시 붙을 때도
/// 같고, 새로 켤 때는 스펙이다.
#[tokio::test]
async fn incoming_spec_starts_the_running_hardware() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    cluster.create(&volume("root", "local")).await.unwrap();
    cluster.create(&volume("data", "local")).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks = vec![disk("root"), disk("data")];

    let storage = Activations::default();
    let plain = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(plain.cpu, vm.spec.cpu);
    assert_eq!(plain.memory_mib, vm.spec.memory_mib);
    assert_eq!(plain.firmware, Firmware::Uefi);
    assert_eq!(plain.hotplug, None);

    // 실행 중에 스펙이 바뀌었다(다음 재시작 때 적용).
    let hw = running_hardware(&vm);
    vm.spec.cpu.cores = 2;
    vm.spec.memory_mib = 512;
    vm.status.hardware = Some(hw.clone());
    let storage = Activations::default();
    let incoming = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(incoming.cpu, hw.cpu);
    assert_eq!(incoming.memory_mib, 1024);
    assert_eq!(incoming.firmware, Firmware::Bios);
    assert_eq!(incoming.hotplug, hw.layout);
    assert_eq!(incoming.disks.len(), 2);

    let running = running_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(running.hotplug, hw.layout);
    assert_eq!(running.memory_mib, 1024);
    // 새로 켜는 것은 스펙이다(배치는 작업자가 정한다).
    let start = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(start.cpu, vm.spec.cpu);
    assert_eq!(start.memory_mib, 512);
    assert_eq!(start.hotplug, None);

    let mut applied = start.clone();
    apply_hardware(&mut applied, &hw, &vm.spec);
    assert_eq!(applied, incoming);

    // 스펙의 디스크가 원본 QEMU의 장치와 다르면(떼는 중인 디스크) 볼륨을 켜지 않고 받지 않는다.
    vm.spec.disks.pop();
    let storage = Activations::default();
    let err = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::DevicesChanged), "{err:?}");
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// 실행 중 붙일 디스크·NIC 하나는 켤 때와 같이 푼다: 볼륨을 켜고 위치를 정한다. NIC는 순번의 tap이다.
#[tokio::test]
async fn resolves_one_disk_and_one_nic() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let data = volume("data", "local");
    cluster.create(&volume("root", "local")).await.unwrap();
    cluster.create(&data).await.unwrap();
    let net: Network = Resource::new(
        "lan",
        NetworkSpec {
            sdn: None,
            subnets: Vec::new(),
            bridge: Some("br0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
            folder: None,
        },
    );
    cluster.create(&net).await.unwrap();
    let mac: MacAddr = "52:54:00:00:00:02".parse().unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks = vec![disk("root"), disk("data")];
    vm.spec.nics = vec![
        nic("user", Some("52:54:00:00:00:01".parse().unwrap())),
        nic("lan", Some(mac)),
    ];

    let storage = Activations::default();
    let one = runtime_disk(&cluster, &vm, 1, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(
        one.path,
        PathBuf::from(format!("/pools/local/{}.qcow2", data.id))
    );
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [("local".to_string(), data.id, ImageFormat::Qcow2)]
    );
    let all = resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
    assert_eq!(all.disks[1], one);

    let nic = runtime_nic(&cluster, &vm, 1).unwrap();
    assert_eq!(nic, all.nics[1]);
    assert!(
        matches!(&nic.backend, NicBackend::Tap { ifname, .. } if *ifname == tap_name(vm.id, 1))
    );
    assert_eq!(nic.mac, mac);

    // 저장소의 일시 오류
    let storage = Activations::default();
    *storage.fail.lock().unwrap() = Some(|| DriverError::Timeout("lvchange"));
    let err = runtime_disk(&cluster, &vm, 1, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::PoolUnavailable(..)), "{err:?}");
    // 스펙에 없는 순번
    assert!(matches!(
        runtime_disk(&cluster, &vm, 2, node, Path::new(MNT), &storage).await,
        Err(ResolveError::NoDisk(2))
    ));
    assert!(matches!(
        runtime_nic(&cluster, &vm, 2),
        Err(ResolveError::NoNic(2))
    ));
}

/// ISO 볼륨(raw, `content: iso`)
fn iso(name: &str, pool: &str) -> Volume {
    let mut v = volume(name, pool);
    v.spec.format = ImageFormat::Raw;
    v.spec.content = VolumeContent::Iso;
    v
}

fn cd(iso: Option<&str>, boot_order: Option<u8>) -> CdromSpec {
    CdromSpec {
        iso: iso.map(String::from),
        boot_order,
    }
}

/// CD-ROM 드라이브는 ISO 파일(읽기 전용 매체)로 풀고 빈 드라이브는 매체가 없다. 부팅 순서는 실제 순서
/// (`effective_boot`)다: 아무 값이 없으면 디스크 뒤에 CD를 차례로 둔다. cloud-init이 있으면 지금 스펙으로 씨앗을 만든다.
#[tokio::test]
async fn resolves_cdrom_media_the_boot_order_and_the_seed() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    cluster.create(&pool("isos", vec![node])).await.unwrap();
    cluster.create(&volume("root", "local")).await.unwrap();
    let ubuntu = iso("ubuntu", "isos");
    cluster.create(&ubuntu).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks = vec![DiskSpec {
        boot_order: None,
        ..disk("root")
    }];
    vm.spec.cdroms = vec![cd(Some("ubuntu"), None), cd(None, None)];
    vm.spec.nics = vec![nic("user", Some("52:54:00:00:00:01".parse().unwrap()))];
    let ci = CloudInit {
        user: Some("orbi".into()),
        ..Default::default()
    };
    vm.spec.cloud_init = Some(ci.clone());

    let storage = Activations::default();
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(spec.disks[0].boot_order, Some(1));
    assert_eq!(
        spec.cdroms,
        [
            RuntimeCdrom {
                medium: Some(RuntimeMedium {
                    path: format!("/pools/isos/{}.raw", ubuntu.id).into()
                }),
                boot_order: Some(2),
            },
            RuntimeCdrom {
                medium: None,
                boot_order: Some(3),
            },
        ]
    );
    assert_eq!(
        storage.calls.lock().unwrap()[1],
        ("isos".to_string(), ubuntu.id, ImageFormat::Raw)
    );
    let seed = spec.cloud_init.expect("no seed");
    assert_eq!(seed, render(vm.id, "web", &ci, &vm.spec.nics));
    let meta: serde_json::Value = serde_json::from_str(&seed.meta_data).unwrap();
    assert_eq!(meta["local-hostname"], "web");

    // 순서가 CD에만 있으면 디스크가 그 뒤(매체를 뺀 뒤 디스크로 켜짐), 순서 없는 CD는 그다음이다.
    vm.spec.cdroms[1].boot_order = Some(1);
    vm.spec.cloud_init = None;
    let spec = resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
    assert_eq!(spec.disks[0].boot_order, Some(2));
    let order: Vec<_> = spec.cdroms.iter().map(|c| c.boot_order).collect();
    assert_eq!(order, [Some(3), Some(1)]);
    assert_eq!(spec.cloud_init, None);

    // 실행 중 넣을 매체 하나
    let storage = Activations::default();
    let medium = runtime_medium(&cluster, &vm, 0, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(
        medium.unwrap().path,
        PathBuf::from(format!("/pools/isos/{}.raw", ubuntu.id))
    );
    assert_eq!(storage.calls.lock().unwrap().len(), 1);
    let empty = runtime_medium(&cluster, &vm, 1, node, Path::new(MNT), &storage).await;
    assert_eq!(empty.unwrap(), None);
    assert!(matches!(
        runtime_medium(&cluster, &vm, 2, node, Path::new(MNT), &storage).await,
        Err(ResolveError::NoCdrom(2))
    ));
}

/// CD 매체는 파일 풀의 ISO 볼륨이어야 한다: 없는 볼륨, 디스크 볼륨, 블록 풀(LVM-thin)의 볼륨은 켜지 않는다(실패하면
/// 켠 디스크를 끈다).
#[tokio::test]
async fn cdrom_media_must_be_isos_in_file_pools() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let root = volume("root", "local");
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks = vec![disk("root")];
    vm.spec.cdroms = vec![cd(Some("ubuntu"), None)];

    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::VolumeNotFound(v) if v == "ubuntu"),
        "{err:?}"
    );
    assert_eq!(*storage.deactivated.lock().unwrap(), [root.id]);

    cluster.create(&volume("ubuntu", "local")).await.unwrap();
    let err = resolve(&cluster, &vm, node, Path::new(MNT))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::VolumeUnusable(v, _) if v == "ubuntu"),
        "{err:?}"
    );
    assert!(err.to_string().contains("not an ISO image"), "{err}");

    let vg: StoragePool = Resource::new(
        "vg",
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    cluster.create(&vg).await.unwrap();
    cluster.create(&iso("alpine", "vg")).await.unwrap();
    vm.spec.cdroms = vec![cd(Some("alpine"), None)];
    let err = resolve(&cluster, &vm, node, Path::new(MNT))
        .await
        .unwrap_err();
    if cfg!(target_os = "linux") {
        assert!(
            err.to_string().contains("only in dir or nfs pools"),
            "{err:?}"
        );
    } else {
        assert!(
            matches!(&err, ResolveError::PoolUnsupported(p, _) if p == "vg"),
            "{err:?}"
        );
    }
}

/// NFS 풀의 ISO는 NFS 디스크와 같다: NFS를 못 쓰는 노드(macOS)에서는 참조 오류, Linux에서는 마운트될 때까지 기다린다.
#[tokio::test]
async fn nfs_isos_follow_the_nfs_rules() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let nas: StoragePool = Resource::new(
        "nas",
        StoragePoolSpec {
            backend: PoolBackend::Nfs {
                server: "nas".into(),
                export: "/srv/iso".into(),
                options: None,
            },
            nodes: vec![],
            shared: true,
            backups: false,
            folder: None,
        },
    );
    cluster.create(&nas).await.unwrap();
    cluster.create(&iso("ubuntu", "nas")).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.cdroms = vec![cd(Some("ubuntu"), None)];
    let mnt = TempDir::new().unwrap();
    std::fs::create_dir(mnt.path().join(nas.id.to_string())).unwrap();
    let err = resolve(&cluster, &vm, node, mnt.path()).await.unwrap_err();
    if cfg!(target_os = "linux") {
        assert!(
            matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "nas" && e.contains("not mounted")),
            "{err:?}"
        );
    } else {
        assert!(
            matches!(&err, ResolveError::NfsUnsupported(n) if n == "nas"),
            "{err:?}"
        );
    }
}

/// 마이그레이션 대상은 원본 QEMU의 CD-ROM 드라이브(기록의 매체·부팅 순서)로 켠다: 실행 중 바꾼 CD `boot_order`(보류)는
/// 따르지 않는다. 매체를 바꾸는 중이면(스펙 ≠ 기록) 받지 않고, 다시 붙을 때는 기록의 매체다. 씨앗 드라이브 유무도
/// 기록대로다.
#[tokio::test]
async fn incoming_spec_starts_the_running_cdroms() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    cluster.create(&volume("root", "local")).await.unwrap();
    let ubuntu = iso("ubuntu", "local");
    cluster.create(&ubuntu).await.unwrap();
    cluster.create(&iso("alpine", "local")).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks = vec![DiskSpec {
        boot_order: None,
        ..disk("root")
    }];
    vm.spec.cdroms = vec![cd(Some("ubuntu"), None), cd(None, None)];
    let hw = RunningHardware::legacy(&vm.spec);
    vm.status.hardware = Some(hw.clone());
    // 실행 중에 둘째 CD를 첫 부팅 장치로 바꿨다(다음 재시작 때 적용).
    vm.spec.cdroms[1].boot_order = Some(1);

    let storage = Activations::default();
    let incoming = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    let order = |s: &opl_types::runtime::VmRuntimeSpec| {
        let disks: Vec<_> = s.disks.iter().map(|d| d.boot_order).collect();
        let cds: Vec<_> = s.cdroms.iter().map(|c| c.boot_order).collect();
        (disks, cds)
    };
    assert_eq!(order(&incoming), (vec![Some(1)], vec![Some(2), Some(3)]));
    assert_eq!(
        incoming.cdroms[0].medium.as_ref().unwrap().path,
        PathBuf::from(format!("/pools/local/{}.raw", ubuntu.id))
    );
    let start = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(order(&start), (vec![Some(2)], vec![Some(3), Some(1)]));
    let mut applied = start.clone();
    apply_hardware(&mut applied, &hw, &vm.spec);
    assert_eq!(applied, incoming);

    // 매체를 바꾸는 중: 받지 않는다. 다시 붙을 때는 도는 QEMU의 매체다.
    vm.spec.cdroms[0].iso = Some("alpine".into());
    let err = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::DevicesChanged), "{err:?}");
    let running = running_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(running.cdroms, incoming.cdroms);

    // 씨앗 없이 켠 QEMU에 다시 붙으면 씨앗 드라이브가 없다(실행 중 켠 cloud-init은 다음 재시작 때).
    vm.spec.cdroms[0].iso = Some("ubuntu".into());
    vm.spec.cloud_init = Some(CloudInit::default());
    let running = running_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(running.cloud_init, None);
    let start = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert!(start.cloud_init.is_some());

    // 씨앗을 넣고 켠 QEMU: 실행 중 cloud-init을 바꿨으면(또는 호스트 이름 없는 VM의 이름을 바꿈) 받는 쪽이 다른 씨앗을
    // 만들게 되므로 받지 않는다.
    vm.status.hardware =
        Some(RunningHardware::at_start(vm.id, &vm.name, &vm.spec, Default::default(), false).0);
    incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    vm.spec.cloud_init = Some(CloudInit {
        hostname: Some("web2".into()),
        ..CloudInit::default()
    });
    let err = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::CloudInitChanged), "{err:?}");
    assert_eq!(
        err.to_string(),
        "cloud-init changed while running; restart the VM before migrating it"
    );
    vm.spec.cloud_init = Some(CloudInit::default());
    vm.name = "web-new".into();
    let err = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::CloudInitChanged), "{err:?}");
}

/// 주소 설정이 있는 cloud-init VM에 실행 중 NIC를 붙인 뒤에도 받는 쪽은 원본 QEMU와 같은 씨앗(켤 때 단 NIC로 만든
/// 것, 같은 instance-id)을 만든다. 새로 켜면 붙인 NIC까지 든 씨앗이다.
#[tokio::test]
async fn incoming_spec_keeps_the_seed_after_a_nic_hotplug() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    cluster.create(&volume("root", "local")).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks = vec![disk("root")];
    vm.spec.nics = vec![nic("user", Some("52:54:00:00:00:01".parse().unwrap()))];
    vm.spec.cloud_init = Some(CloudInit {
        networks: vec![CloudInitNet {
            ipv4: Some(IpConfig::Static {
                address: "10.0.0.5/24".into(),
                gateway: None,
            }),
            ipv6: None,
        }],
        ..CloudInit::default()
    });
    let (mut hw, _) =
        RunningHardware::at_start(vm.id, &vm.name, &vm.spec, Default::default(), true);
    // 핫플러그 Task가 둘째 NIC를 붙였다.
    let added = nic("user", Some("52:54:00:00:00:02".parse().unwrap()));
    vm.spec.nics.push(added.clone());
    hw.nics.push(added);
    hw.layout.as_mut().unwrap().nic_ports.push(0);
    vm.status.hardware = Some(hw.clone());

    let storage = Activations::default();
    let incoming = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(incoming.nics.len(), 2);
    let seed = incoming.cloud_init.expect("no seed");
    assert_eq!(Some(&seed.instance_id), hw.cloud_init.as_ref());
    assert!(!seed.network_config.as_deref().unwrap().contains("eth1"));
    let start = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    let fresh = start.cloud_init.expect("no seed");
    assert_ne!(Some(&fresh.instance_id), hw.cloud_init.as_ref());
    assert!(fresh.network_config.as_deref().unwrap().contains("eth1"));
}
