//! HA 복구 뒤의 복제본 승격(계획 55): 리더가 이 노드의 복제본으로 켜라고 표시한(`promote`) VM을 켜기 전에, 복제본
//! 기록을 보고 볼륨 기록을 이 노드의 풀로 옮긴 뒤 켠다. 받은 겹층은 합치고 못 받은 겹층은 버린다. 쓸 복제본이 없으면
//! 켜지 않는다. 표시가 없으면 승격하지 않는다.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use common::Env;
use opl_driver::{
    Capacity, DriverError, Hypervisor, SourceFormat, Storage, VolumeInfo, VolumeSource,
};
use opl_reconciler::replicas::{self, Overlay, RecordedReplica, ReplicaRecord, ReplicaState, Role};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::replication::{ReplicaStatus, ReplicaVolume, ReplicationState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{DiskBus, DiskSpec, PowerState, Vm, VmPhase};
use tempfile::TempDir;
use uuid::Uuid;

const SIZE: u64 = 1 << 30;

/// 볼륨 켜기는 늘 되고, 겹층 합치기(`commit_overlay`)는 적은 뒤 겹층 파일을 지운다(둘째 값만큼은 먼저 실패한다).
#[derive(Default)]
struct Merges(Mutex<Vec<(String, Uuid, PathBuf)>>, Mutex<usize>);

#[async_trait]
impl Storage for Merges {
    async fn activate_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }
    async fn deactivate_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }
    async fn commit_overlay(
        &self,
        pool: &StoragePool,
        id: Uuid,
        _: ImageFormat,
        overlay: &Path,
    ) -> Result<(), DriverError> {
        {
            let mut failing = self.1.lock().unwrap();
            if *failing > 0 {
                *failing -= 1;
                return Err(DriverError::InvalidSpec("qemu-img commit failed".into()));
            }
        }
        self.0
            .lock()
            .unwrap()
            .push((pool.name.clone(), id, overlay.to_path_buf()));
        replicas::discard_overlay(overlay).map_err(DriverError::from)
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

/// 이 노드(B)가 HA 복구로 받은 VM `web`: 디스크 `root`는 죽은 노드 A의 풀 `a`에 있고, B의 풀 `b`에 복제본이 있다.
struct Setup {
    env: Env,
    storage: std::sync::Arc<Merges>,
    replicas: TempDir,
    source: Uuid,
    volume: Volume,
    vm: Vm,
}

fn dir_pool(name: &str, node: Uuid) -> StoragePool {
    Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: format!("/pools/{name}").into(),
            },
            nodes: vec![node],
            shared: false,
            backups: false,
            folder: None,
        },
    )
}

/// `promote`면 리더가 승격 표시를 한 것이다. VM은 아직 만들지 않는다(기록을 먼저 둔다).
async fn setup(promote: bool) -> Setup {
    let storage = std::sync::Arc::new(Merges::default());
    let replicas = TempDir::new().unwrap();
    let (s, dir) = (storage.clone(), replicas.path().to_path_buf());
    let mut env = Env::start_with(move |c| {
        c.storage = s;
        c.replica_dir = dir;
    })
    .await;
    // reconciler는 VM을 만든 뒤 띄운다(기록과 표시를 다 둔 뒤 켜게).
    env.stop_reconciler().await;
    let source = Uuid::now_v7();
    let mut me: Resource<NodeSpec, NodeStatus> = Resource::new(
        "node-b",
        NodeSpec {
            sdn: None,
            host_resources: None,
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    me.id = env.node;
    env.cluster.create(&me).await.unwrap();
    env.cluster.create(&dir_pool("a", source)).await.unwrap();
    env.cluster.create(&dir_pool("b", env.node)).await.unwrap();
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "a".into(),
            size_bytes: SIZE,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    volume.status.node = Some(source);
    env.cluster.create(&volume).await.unwrap();
    let mut vm = common::vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: None,
    });
    vm.status.last_error =
        Some("recovering: node \"node-a\" failed; restarting from the replica".into());
    vm.status.replication = vec![
        ReplicaStatus {
            state: ReplicationState::Ready,
            generation: Some(Uuid::now_v7()),
            synced_at: Some(100),
            volumes: vec![ReplicaVolume {
                id: volume.id,
                size_bytes: SIZE,
            }],
            promote,
            ..ReplicaStatus::pending(env.node)
        },
        ReplicaStatus {
            state: ReplicationState::Stale,
            ..ReplicaStatus::pending(source)
        },
    ];
    Setup {
        env,
        storage,
        replicas,
        source,
        volume,
        vm,
    }
}

impl Setup {
    /// B의 풀 `b`에 있는 `root` 복제본 기록
    fn record(&self, overlay: Option<Overlay>) -> ReplicaRecord {
        ReplicaRecord {
            vm: self.vm.id,
            role: Role::Replica,
            generation: self.vm.status.replication[0].generation,
            synced_at: Some(100),
            volumes: vec![RecordedReplica {
                id: self.volume.id,
                pool: "b".into(),
                format: ImageFormat::Qcow2,
                size_bytes: SIZE,
                created: true,
                state: ReplicaState::Ready,
                overlay,
            }],
        }
    }

    /// VM을 만들고 reconciler를 띄운다.
    async fn start(&mut self) {
        self.env.create(&self.vm).await;
        self.env.start_reconciler();
    }

    fn volume(&self) -> Volume {
        self.env
            .cluster
            .get::<VolumeSpec>(self.volume.id)
            .unwrap()
            .unwrap()
    }

    fn read_record(&self) -> Option<ReplicaRecord> {
        replicas::read(self.replicas.path(), self.vm.id).unwrap()
    }

    /// 켠 VM의 디스크 경로
    fn launched_disk(&self) -> PathBuf {
        let specs = self.env.hv.launch_specs();
        let spec = specs.last().expect("the VM was not launched");
        spec.disks[0].path.clone()
    }
}

/// 승격 표시가 있고 복제본 기록이 `ready`면 볼륨 기록을 이 노드의 풀 `b`로 옮기고 기록을 `primary`로 바꾼 뒤, 이
/// 노드의 항목(표시)을 지우고 복제본으로 켠다. 옛 원본(`stale`) 항목은 남는다.
#[tokio::test]
async fn a_marked_replica_is_promoted_before_the_vm_starts() {
    let mut s = setup(true).await;
    replicas::write(s.replicas.path(), &s.record(None)).unwrap();
    s.start().await;
    let vm = s
        .env
        .wait(s.vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    let volume = s.volume();
    assert_eq!(volume.spec.pool, "b");
    assert_eq!(volume.spec.format, ImageFormat::Qcow2);
    assert_eq!(volume.spec.size_bytes, SIZE);
    assert_eq!(volume.status.node, Some(s.env.node));
    assert_eq!(
        s.launched_disk(),
        PathBuf::from(format!("/pools/b/{}.qcow2", s.volume.id))
    );
    let record = s.read_record().unwrap();
    assert_eq!(record.role, Role::Primary);
    assert_eq!(record.volumes, s.record(None).volumes);
    assert_eq!(
        vm.status.replication,
        [ReplicaStatus {
            state: ReplicationState::Stale,
            ..ReplicaStatus::pending(s.source)
        }]
    );
    assert!(s.storage.0.lock().unwrap().is_empty());
}

/// 끝까지 받은 겹층(끝난 회차)은 합친 뒤 켠다.
#[tokio::test]
async fn a_received_increment_is_merged_before_promotion() {
    let mut s = setup(true).await;
    let run = Uuid::now_v7();
    let path = replicas::overlay_path(s.replicas.path(), run, s.volume.id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "").unwrap();
    let overlay = Overlay {
        run,
        path: path.clone(),
        received: true,
    };
    replicas::write(s.replicas.path(), &s.record(Some(overlay))).unwrap();
    s.start().await;
    s.env
        .wait(s.vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(
        *s.storage.0.lock().unwrap(),
        [("b".to_string(), s.volume.id, path.clone())]
    );
    assert!(!path.exists());
    let record = s.read_record().unwrap();
    assert_eq!(record.role, Role::Primary);
    assert_eq!(record.volumes[0].overlay, None);
    assert_eq!(s.volume().status.node, Some(s.env.node));
}

/// 승격의 일시 오류(겹층을 합치지 못함)는 VM을 끄지 않고 잠시 뒤 다시 해 본다.
#[tokio::test]
async fn a_transient_promotion_error_is_retried() {
    let mut s = setup(true).await;
    *s.storage.1.lock().unwrap() = 1;
    let run = Uuid::now_v7();
    let path = replicas::overlay_path(s.replicas.path(), run, s.volume.id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "").unwrap();
    let overlay = Overlay {
        run,
        path: path.clone(),
        received: true,
    };
    replicas::write(s.replicas.path(), &s.record(Some(overlay))).unwrap();
    s.start().await;
    let vm = s
        .env
        .wait(s.vm.id, "running or failed", |v| {
            matches!(v.status.phase, VmPhase::Running | VmPhase::Failed)
        })
        .await;
    assert_eq!(vm.status.phase, VmPhase::Running, "{:?}", vm.status);
    assert_eq!(vm.spec.power, PowerState::Running);
    assert_eq!(*s.storage.1.lock().unwrap(), 0);
    assert_eq!(s.storage.0.lock().unwrap().len(), 1);
    assert_eq!(s.read_record().unwrap().role, Role::Primary);
    assert_eq!(s.volume().status.node, Some(s.env.node));
}

/// 끝까지 받지 못한 겹층은 그 받는 QEMU를 끄고 버린 뒤(앞 회차의 시점으로) 켠다.
#[tokio::test]
async fn an_unfinished_increment_is_discarded_before_promotion() {
    let mut s = setup(true).await;
    let run = Uuid::now_v7();
    s.env
        .hv
        .start_replica_receiver(run, &[], "127.0.0.1", 61200..=61201)
        .await
        .unwrap();
    let path = replicas::overlay_path(s.replicas.path(), run, s.volume.id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "").unwrap();
    let overlay = Overlay {
        run,
        path: path.clone(),
        received: false,
    };
    replicas::write(s.replicas.path(), &s.record(Some(overlay))).unwrap();
    s.start().await;
    s.env
        .wait(s.vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert!(s.storage.0.lock().unwrap().is_empty());
    assert!(!path.exists());
    assert!(s.env.hv.receivers().is_empty());
    assert_eq!(s.read_record().unwrap().volumes[0].overlay, None);
    assert_eq!(s.volume().spec.pool, "b");
}

/// 쓸 복제본이 없으면(기록이 없음, 복사 중) 켜지 않고 볼륨 기록을 그대로 둔다.
#[tokio::test]
async fn without_a_complete_replica_the_vm_does_not_start() {
    for syncing in [false, true] {
        let mut s = setup(true).await;
        if syncing {
            let mut record = s.record(None);
            record.volumes[0].state = ReplicaState::Syncing;
            replicas::write(s.replicas.path(), &record).unwrap();
        }
        s.start().await;
        let vm = s
            .env
            .wait(s.vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
            .await;
        assert_eq!(
            vm.status.last_error.as_deref(),
            Some("failed to start: disk \"root\" has no complete replica on node \"node-b\"")
        );
        assert_eq!(vm.spec.power, PowerState::Stopped);
        assert!(s.env.hv.launch_specs().is_empty());
        let volume = s.volume();
        assert_eq!(volume.spec, s.volume.spec);
        assert_eq!(volume.status.node, Some(s.source));
        assert!(vm.status.replication[0].promote);
    }
}

/// 노드 로컬 풀 디스크 가운데 어느 노드에 있는지 모르는 볼륨이 있으면 승격하지 않고 켜지 않는다.
#[tokio::test]
async fn a_volume_on_an_unknown_node_is_not_promoted() {
    let mut s = setup(true).await;
    replicas::write(s.replicas.path(), &s.record(None)).unwrap();
    s.volume.status.node = None;
    s.env
        .cluster
        .update_status::<VolumeSpec>(s.volume.id, &s.volume.status)
        .await
        .unwrap();
    s.start().await;
    let vm = s
        .env
        .wait(s.vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(
        vm.status.last_error.as_deref(),
        Some("failed to start: disk \"root\" is on node-local pool \"a\" but its node is unknown")
    );
    assert!(s.env.hv.launch_specs().is_empty());
    let volume = s.volume();
    assert_eq!(volume.spec, s.volume.spec);
    assert_eq!(volume.status.node, None);
    assert_eq!(s.read_record().unwrap().role, Role::Replica);
}

/// 승격 표시가 없으면 다른 노드의 볼륨 기록을 고치지 않는다(그 풀은 이 노드에서 쓸 수 없어 켜지 않는다 — 지금처럼).
#[tokio::test]
async fn without_the_mark_nothing_is_promoted() {
    let mut s = setup(false).await;
    replicas::write(s.replicas.path(), &s.record(None)).unwrap();
    s.start().await;
    let vm = s
        .env
        .wait(s.vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(
        vm.status.last_error.as_deref(),
        Some("storage pool \"a\" is not available on this node")
    );
    let volume = s.volume();
    assert_eq!(volume.spec.pool, "a");
    assert_eq!(volume.status.node, Some(s.source));
    assert_eq!(s.read_record().unwrap().role, Role::Replica);
    assert_eq!(vm.status.replication.len(), 2);
}
