//! 로컬 디스크 복제의 받는 쪽(`IncomingReplicas`, 계획 55): 원본 노드가 RPC로 부르는 처리를 직접 부른다. 이
//! 노드(Env의 노드)가 대상이고, 원본 노드는 저장소에만 있다. 복제본 기록 파일과 청소도 본다.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{Env, FakeStorage, MOUNT_ROOT, WAIT, set_pool_state, vm_request};
use opl_api::{IncomingReplicas, ReplicaConfig};
use opl_driver::{DiskLocation, DriverError, Hypervisor, Storage};
use opl_peer::{
    PrepareMode, PrepareVolume, ReplicaAbortRequest, ReplicaError, ReplicaFinishRequest,
    ReplicaPrepareRequest, ReplicaReceiver,
};
use opl_reconciler::replicas::{self, Overlay, RecordedReplica, ReplicaRecord, ReplicaState, Role};
use opl_types::node::{FEATURES, Node, NodeSpec, PoolState};
use opl_types::replication::ReplicationTarget;
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat};
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{MigrationStatus, Vm, VmSpec};
use serde_json::json;
use uuid::Uuid;

const SIZE: u64 = 1 << 20;

struct Setup {
    env: Env,
    storage: Arc<FakeStorage>,
    receiver: IncomingReplicas,
    /// VM이 도는 원본 노드
    source: Uuid,
    vm: Uuid,
    volume: Uuid,
    /// 복제본 기록 디렉터리
    dir: PathBuf,
}

/// 원본 노드 `source`의 로컬 풀 `a`에 볼륨 `root`를 둔 VM `web`을 저장소에 만든다. VM은 이 노드의 로컬 풀 `b`로
/// 복제한다(이 노드의 reconciler는 손대지 않는다).
async fn setup() -> Setup {
    let env = Env::start().await;
    let mut source: Node = Resource::new(
        "source",
        NodeSpec {
            host_resources: None,
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    source.status.online = true;
    source.status.features = FEATURES.iter().map(|f| f.to_string()).collect();
    env.cluster.create(&source).await.unwrap();
    for (name, node) in [("a", source.id), ("b", env.node)] {
        let pool = Resource::new(
            name,
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: env.dir.path().join(name),
                },
                nodes: vec![node],
                shared: false,
                backups: false,
                folder: None,
            },
        );
        env.cluster.create(&pool).await.unwrap();
    }
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
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    volume.status.node = Some(source.id);
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    let mut spec = vm_request("web")["spec"].clone();
    spec["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    spec["node"] = json!(source.id);
    spec["replication"] = json!([{ "node": env.node, "pool": "b" }]);
    let vm: Vm = Resource::new("web", serde_json::from_value::<VmSpec>(spec).unwrap());
    env.cluster.create(&vm).await.unwrap();

    let storage = Arc::new(FakeStorage::default());
    let dir = env.dir.path().join("replicas");
    let receiver = IncomingReplicas::new(
        env.cluster.clone(),
        env.hv.clone() as Arc<dyn Hypervisor>,
        ReplicaConfig {
            node: env.node,
            mount_root: MOUNT_ROOT.into(),
            storage: storage.clone() as Arc<dyn Storage>,
            listen_host: "127.0.0.1".into(),
            ports: 61000..=61009,
            replica_dir: dir.clone(),
        },
    );
    Setup {
        source: source.id,
        vm: vm.id,
        volume: volume.id,
        env,
        storage,
        receiver,
        dir,
    }
}

impl Setup {
    /// 원본 노드가 하듯 이 노드로의 복제 Task를 만든다. 그 id(회차)를 돌려준다.
    async fn task(&self) -> Uuid {
        let mut task: Task = Resource::new(
            "",
            TaskSpec {
                op: TaskOp::VmReplicate {
                    node: self.env.node,
                },
                target: self.vm,
                node: self.source,
            },
        );
        task.name = task.id.to_string();
        self.env.cluster.create(&task).await.unwrap();
        task.id
    }

    /// Task `id`를 끝낸다.
    async fn end_task(&self, id: Uuid) {
        let status = TaskStatus {
            state: TaskState::Failed,
            error: Some("interrupted".into()),
            progress: None,
        };
        self.env
            .cluster
            .update_status::<TaskSpec>(id, &status)
            .await
            .unwrap();
    }

    fn disk(&self, size_bytes: u64, mode: PrepareMode) -> PrepareVolume {
        PrepareVolume {
            id: self.volume,
            pool: "b".into(),
            format: ImageFormat::Qcow2,
            size_bytes,
            allocated_bytes: 4096,
            mode,
        }
    }

    fn request(&self, run: Uuid, volumes: Vec<PrepareVolume>) -> ReplicaPrepareRequest {
        ReplicaPrepareRequest {
            vm: self.vm,
            task: run,
            run,
            volumes,
            dirty_bytes: 65536,
        }
    }

    async fn prepare(&self, run: Uuid, mode: PrepareMode) -> Result<u16, ReplicaError> {
        let reply = self
            .receiver
            .prepare(self.request(run, vec![self.disk(SIZE, mode)]))
            .await?;
        assert!(reply.refused.is_empty(), "{reply:?}");
        assert_eq!(reply.exports, [self.volume.simple().to_string()]);
        Ok(reply.port)
    }

    async fn finish(&self, run: Uuid, ok: bool, synced_at: u64) -> Result<(), ReplicaError> {
        self.receiver
            .finish(ReplicaFinishRequest {
                vm: self.vm,
                task: run,
                run,
                ok,
                synced_at,
                volumes: vec![],
            })
            .await
    }

    /// 전체 회차 하나를 끝까지 받는다(시점 `synced_at`). 회차를 돌려준다.
    async fn full_run(&self, synced_at: u64) -> Uuid {
        let run = self.task().await;
        self.prepare(run, PrepareMode::Full).await.unwrap();
        self.finish(run, true, synced_at).await.unwrap();
        run
    }

    fn record(&self) -> Option<ReplicaRecord> {
        replicas::read(&self.dir, self.vm).unwrap()
    }

    fn vm(&self) -> Vm {
        self.env.cluster.get::<VmSpec>(self.vm).unwrap().unwrap()
    }

    async fn update_vm(&self, vm: &Vm) {
        self.env
            .cluster
            .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
            .await
            .unwrap();
    }

    /// 원본 노드의 풀 `a`에 볼륨 `name`을 하나 더 만든다. 그 id
    async fn another_volume(&self, name: &str) -> Uuid {
        let mut v: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: "a".into(),
                size_bytes: SIZE,
                format: ImageFormat::Qcow2,
                content: Default::default(),
                base: None,
            },
        );
        self.env.cluster.create(&v).await.unwrap();
        v.status.phase = VolumePhase::Ready;
        v.status.node = Some(self.source);
        self.env
            .cluster
            .update_status::<VolumeSpec>(v.id, &v.status)
            .await
            .unwrap();
        v.id
    }

    /// 원본 노드의 풀 `a`에 볼륨 `volume`을 둔 VM `name`을 하나 더 만든다(이 노드의 풀 `b`로 복제). (VM, 볼륨) id
    async fn another_vm(&self, name: &str, volume: &str) -> (Uuid, Uuid) {
        let id = self.another_volume(volume).await;
        let mut vm = self.vm();
        vm.id = Uuid::now_v7();
        vm.name = name.into();
        vm.spec.disks[0].volume = volume.into();
        self.env.cluster.create(&vm).await.unwrap();
        (vm.id, id)
    }

    /// VM `vm`을 이 노드로 복제하는 Task를 만든다. 그 id(회차)
    async fn task_of(&self, vm: Uuid) -> Uuid {
        let mut task: Task = Resource::new(
            "",
            TaskSpec {
                op: TaskOp::VmReplicate {
                    node: self.env.node,
                },
                target: vm,
                node: self.source,
            },
        );
        task.name = task.id.to_string();
        self.env.cluster.create(&task).await.unwrap();
        task.id
    }

    async fn set_volume_node(&self, node: Uuid) {
        let mut v = self
            .env
            .cluster
            .get::<VolumeSpec>(self.volume)
            .unwrap()
            .unwrap();
        v.status.node = Some(node);
        self.env
            .cluster
            .update_status::<VolumeSpec>(v.id, &v.status)
            .await
            .unwrap();
    }
}

fn incremental(base_synced_at: u64) -> PrepareMode {
    PrepareMode::Incremental { base_synced_at }
}

/// 처음 회차는 대상 풀에 같은 id로 복제본을 만들어(만들기 전에 기록) 그 자체를 내보내고, 끝내면 기록이 그 회차로
/// `ready`가 된다.
#[tokio::test]
async fn a_first_run_creates_the_replica_and_records_it_when_finished() {
    let s = setup().await;
    let run = s.task().await;
    let port = s.prepare(run, PrepareMode::Full).await.unwrap();
    assert_eq!(port, 61000);
    assert!(s.storage.has_file(s.volume));
    let record = s.record().unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.generation, None);
    assert_eq!(
        record.volumes,
        [RecordedReplica {
            id: s.volume,
            pool: "b".into(),
            format: ImageFormat::Qcow2,
            size_bytes: SIZE,
            created: true,
            state: ReplicaState::Syncing,
            overlay: None,
        }]
    );
    // 받는 QEMU는 복제본 파일 자체를 내보낸다.
    let exports = &s.env.hv.receivers()[&run];
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, s.volume.simple().to_string());
    assert_eq!(exports[0].overlay, None);
    let expected = s
        .env
        .dir
        .path()
        .join("b")
        .join(format!("{}.qcow2", s.volume));
    assert_eq!(
        exports[0].source,
        DiskLocation::Path(expected, DiskKind::File)
    );

    s.finish(run, true, 1700).await.unwrap();
    assert!(s.env.hv.receivers().is_empty());
    let record = s.record().unwrap();
    assert_eq!(record.generation, Some(run));
    assert_eq!(record.synced_at, Some(1700));
    assert_eq!(record.volumes[0].state, ReplicaState::Ready);
}

/// 증분 회차는 복제본 위의 겹층에 받고, 끝내면 합쳐 지운다. 원본이 실패를 알리면 겹층을 버리고 기록은 앞 회차
/// 그대로다.
#[tokio::test]
async fn increments_land_in_an_overlay_that_is_merged_or_discarded() {
    let s = setup().await;
    let first = s.full_run(1700).await;

    let run = s.task().await;
    s.prepare(run, incremental(1700)).await.unwrap();
    let path = replicas::overlay_path(&s.dir, run, s.volume);
    assert!(path.exists());
    assert_eq!(s.storage.overlays(), [("create", s.volume)]);
    let record = s.record().unwrap();
    assert_eq!(
        record.volumes[0].overlay,
        Some(Overlay {
            run,
            path: path.clone(),
            received: false
        })
    );
    // 증분 중에도 복제본은 앞 회차로 온전하다.
    assert_eq!(record.volumes[0].state, ReplicaState::Ready);
    assert_eq!(s.env.hv.receivers()[&run][0].overlay, Some(path.clone()));
    s.finish(run, true, 1800).await.unwrap();
    assert!(!path.exists());
    assert_eq!(
        s.storage.overlays(),
        [("create", s.volume), ("commit", s.volume)]
    );
    let record = s.record().unwrap();
    assert_eq!(record.generation, Some(run));
    assert_eq!(record.synced_at, Some(1800));
    assert_eq!(record.volumes[0].overlay, None);

    // 원본이 실패를 알렸다: 겹층을 버리고 합치지 않는다.
    let failed = s.task().await;
    s.prepare(failed, incremental(1800)).await.unwrap();
    let path = replicas::overlay_path(&s.dir, failed, s.volume);
    assert!(path.exists());
    s.finish(failed, false, 1900).await.unwrap();
    assert!(!path.exists());
    assert!(s.env.hv.receivers().is_empty());
    assert_eq!(s.storage.overlays().last(), Some(&("create", s.volume)));
    let record = s.record().unwrap();
    assert_eq!(record.generation, Some(run));
    assert_eq!(record.synced_at, Some(1800));
    assert_eq!(record.volumes[0].overlay, None);
    assert_ne!(record.generation, Some(first));

    // 버리기도 받는 QEMU를 끄고 겹층을 지운다.
    let aborted = s.task().await;
    s.prepare(aborted, incremental(1800)).await.unwrap();
    let path = replicas::overlay_path(&s.dir, aborted, s.volume);
    s.receiver
        .abort(ReplicaAbortRequest {
            vm: s.vm,
            task: aborted,
            run: aborted,
        })
        .await
        .unwrap();
    assert!(!path.exists());
    assert!(s.env.hv.receivers().is_empty());
    assert_eq!(s.record().unwrap().volumes[0].overlay, None);
}

/// 기록에 없는데 같은 id의 볼륨이 이미 있으면(공유인 풀을 로컬로 설정했다) 거절하고 그 파일을 지우지도 쓰지도
/// 않는다.
#[tokio::test]
async fn an_existing_volume_not_recorded_here_is_refused() {
    let s = setup().await;
    s.storage.place(s.volume, SIZE);
    let run = s.task().await;
    let err = s.prepare(run, PrepareMode::Full).await.unwrap_err();
    assert_eq!(
        err,
        ReplicaError::Failed(format!(
            "volume {} already exists on node \"{}\" in pool \"b\" but is not a replica recorded by this node; is the pool really node-local?",
            s.volume,
            node_name(&s.env)
        ))
    );
    assert!(s.storage.has_file(s.volume));
    assert!(s.storage.deleted().is_empty());
    assert_eq!(s.record(), None);
    assert!(s.env.hv.receivers().is_empty());
}

/// 증분의 기준 시점이 이 노드의 복제본보다 새로우면(기록을 잃었다) 아무것도 하지 않고 거절한다. 이 노드가 더
/// 새로우면(끝내기 응답을 잃었다) 받는다.
#[tokio::test]
async fn increments_on_an_older_replica_are_refused() {
    let s = setup().await;
    s.full_run(1700).await;
    let run = s.task().await;
    let reply = s
        .receiver
        .prepare(s.request(run, vec![s.disk(SIZE, incremental(1800))]))
        .await
        .unwrap();
    assert_eq!(reply.refused, [s.volume]);
    assert_eq!(reply.port, 0);
    assert!(s.env.hv.receivers().is_empty());
    assert!(s.storage.overlays().is_empty());
    // 기록이 없어도 거절한다.
    std::fs::remove_file(s.dir.join(format!("{}.json", s.vm))).unwrap();
    let reply = s
        .receiver
        .prepare(s.request(run, vec![s.disk(SIZE, incremental(1700))]))
        .await
        .unwrap();
    assert_eq!(reply.refused, [s.volume]);
}

#[tokio::test]
async fn increments_on_a_newer_replica_are_received() {
    let s = setup().await;
    s.full_run(1800).await;
    let run = s.task().await;
    s.prepare(run, incremental(1700)).await.unwrap();
    assert_eq!(s.storage.overlays(), [("create", s.volume)]);
}

/// 새 복제본이 이 노드가 알린 풀의 남은 공간보다 크거나, 증분이 기록 디렉터리의 파일 시스템에 들어가지 않으면
/// 아무것도 만들지 않고 실패한다.
#[tokio::test]
async fn replicas_need_space() {
    let s = setup().await;
    let state = PoolState {
        ready: true,
        available_bytes: 1000,
        total_bytes: 1 << 30,
        ..Default::default()
    };
    set_pool_state(&s.env.cluster, s.env.node, "b", state).await;
    let run = s.task().await;
    let err = s.prepare(run, PrepareMode::Full).await.unwrap_err();
    assert_eq!(
        err,
        ReplicaError::Failed(format!(
            "not enough space in storage pool \"b\" on node \"{}\": need 4096, available 1000",
            node_name(&s.env)
        ))
    );
    assert!(!s.storage.has_file(s.volume));
    assert_eq!(s.record(), None);

    let state = PoolState {
        ready: true,
        available_bytes: u64::MAX,
        total_bytes: u64::MAX,
        ..Default::default()
    };
    set_pool_state(&s.env.cluster, s.env.node, "b", state).await;
    s.full_run(1700).await;
    let run = s.task().await;
    let mut req = s.request(run, vec![s.disk(SIZE, incremental(1700))]);
    req.dirty_bytes = u64::MAX / 2;
    let err = s.receiver.prepare(req).await.unwrap_err();
    let ReplicaError::Failed(message) = err else {
        panic!("{err:?}");
    };
    let overlays = replicas::overlay_dir(&s.dir);
    assert!(
        message.starts_with(&format!(
            "not enough space in {} on node \"{}\" for the increment: need {}, available ",
            overlays.display(),
            node_name(&s.env),
            u64::MAX / 2 + (1 << 30)
        )),
        "{message}"
    );
    assert!(s.storage.overlays().is_empty());
}

/// 원본 디스크가 커졌으면 전체 회차가 복제본을 먼저 늘린다.
#[tokio::test]
async fn a_grown_disk_grows_its_replica() {
    let s = setup().await;
    s.full_run(1700).await;
    let run = s.task().await;
    let reply = s
        .receiver
        .prepare(s.request(run, vec![s.disk(2 * SIZE, PrepareMode::Full)]))
        .await
        .unwrap();
    assert!(reply.refused.is_empty());
    assert!(s.storage.calls().contains(&("resize", s.volume)));
    let record = s.record().unwrap();
    assert_eq!(record.volumes[0].size_bytes, 2 * SIZE);
    assert_eq!(record.volumes[0].state, ReplicaState::Syncing);
    // 크기가 다른 복제본에는 증분을 받지 않는다.
    s.finish(run, true, 1800).await.unwrap();
    let run = s.task().await;
    let reply = s
        .receiver
        .prepare(s.request(run, vec![s.disk(SIZE, incremental(1800))]))
        .await
        .unwrap();
    assert_eq!(reply.refused, [s.volume]);
}

/// 받는 QEMU를 띄우지 못하면 이 호출이 만든 복제본을 지우고 기록을 정리한다.
#[tokio::test]
async fn a_failed_receiver_start_undoes_what_it_made() {
    let s = setup().await;
    let run = s.task().await;
    s.env
        .hv
        .fail_next_receiver_start(DriverError::StartFailed("no qemu".into()));
    let err = s.prepare(run, PrepareMode::Full).await.unwrap_err();
    assert!(
        matches!(&err, ReplicaError::Failed(m) if m.contains("no qemu")),
        "{err:?}"
    );
    assert!(!s.storage.has_file(s.volume));
    assert_eq!(s.storage.deleted(), [s.volume]);
    assert_eq!(s.record(), None);

    // 증분이면 만든 겹층을 지우고 복제본은 그대로다.
    s.full_run(1700).await;
    let run = s.task().await;
    s.env
        .hv
        .fail_next_receiver_start(DriverError::StartFailed("no qemu".into()));
    s.prepare(run, incremental(1700)).await.unwrap_err();
    assert!(!replicas::overlay_path(&s.dir, run, s.volume).exists());
    let record = s.record().unwrap();
    assert_eq!(record.volumes[0].overlay, None);
    assert_eq!(record.volumes[0].state, ReplicaState::Ready);
    assert!(s.storage.has_file(s.volume));
}

/// 받은 겹층을 합치다 끊겼으면 회차는 끝난 것이고(기록), 다음 준비가 먼저 합친다.
#[tokio::test]
async fn a_received_overlay_is_merged_before_the_next_run() {
    let s = setup().await;
    s.full_run(1700).await;
    let run = s.task().await;
    s.prepare(run, incremental(1700)).await.unwrap();
    let path = replicas::overlay_path(&s.dir, run, s.volume);
    s.storage.fail_commits(1);
    s.finish(run, true, 1800).await.unwrap();
    assert!(path.exists());
    let record = s.record().unwrap();
    assert_eq!(record.generation, Some(run));
    assert_eq!(record.synced_at, Some(1800));
    assert!(record.volumes[0].overlay.as_ref().unwrap().received);

    let next = s.task().await;
    s.prepare(next, incremental(1800)).await.unwrap();
    assert!(!path.exists());
    assert_eq!(
        s.storage.overlays(),
        [
            ("create", s.volume),
            ("commit", s.volume),
            ("commit", s.volume),
            ("create", s.volume),
        ]
    );
    let record = s.record().unwrap();
    assert_eq!(record.volumes[0].overlay.as_ref().unwrap().run, next);
}

/// 모든 노드가 복제를 알아야 받는다. 대상이 아니거나 VM이 이 노드에서 돌면 거절한다.
#[tokio::test]
async fn prepare_needs_every_node_and_a_target() {
    let s = setup().await;
    let run = s.task().await;
    let old: Node = Resource::new(
        "old",
        NodeSpec {
            host_resources: None,
            address: "127.0.0.3:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    s.env.cluster.create(&old).await.unwrap();
    let err = s.prepare(run, PrepareMode::Full).await.unwrap_err();
    assert_eq!(
        err,
        ReplicaError::Rejected(
            "some nodes do not support node replication yet (old); upgrade them first".into()
        )
    );
    s.env
        .cluster
        .delete::<NodeSpec>(old.id, None)
        .await
        .unwrap();

    let mut vm = s.vm();
    vm.spec.replication.clear();
    s.update_vm(&vm).await;
    let err = s.prepare(run, PrepareMode::Full).await.unwrap_err();
    assert_eq!(
        err,
        ReplicaError::Rejected(format!(
            "node \"{}\" is not a replication target of VM \"web\"",
            node_name(&s.env)
        ))
    );
    assert!(!s.storage.has_file(s.volume));
}

/// 청소: 지운 VM의 복제본, 대상에서 뺀 VM의 복제본(끝나지 않은 복제 Task가 없을 때), 뗀 디스크의 복제본을 지운다.
#[tokio::test]
async fn sweep_deletes_replicas_no_longer_wanted() {
    // 대상에서 뺐다.
    let s = Box::pin(setup()).await;
    let run = s.full_run(1700).await;
    let mut vm = s.vm();
    vm.spec.replication.clear();
    s.update_vm(&vm).await;
    // 끝나지 않은 복제 Task가 있으면 기다린다.
    Box::pin(s.receiver.sweep()).await;
    assert!(s.record().is_some());
    s.end_task(run).await;
    Box::pin(s.receiver.sweep()).await;
    assert_eq!(s.record(), None);
    assert_eq!(s.storage.deleted(), [s.volume]);

    // VM을 지웠다.
    let s = Box::pin(setup()).await;
    let run = s.full_run(1700).await;
    s.end_task(run).await;
    s.env.cluster.delete::<VmSpec>(s.vm, None).await.unwrap();
    Box::pin(s.receiver.sweep()).await;
    assert_eq!(s.record(), None);
    assert_eq!(s.storage.deleted(), [s.volume]);

    // 디스크 하나를 뗐다: 그 복제본만 지운다.
    let s = Box::pin(setup()).await;
    let mut other: Volume = Resource::new(
        "data",
        VolumeSpec {
            pool: "a".into(),
            size_bytes: SIZE,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    other.status.node = Some(s.source);
    s.env.cluster.create(&other).await.unwrap();
    let mut vm = s.vm();
    vm.spec
        .disks
        .push(serde_json::from_value(json!({ "volume": "data", "bus": "virtio-blk" })).unwrap());
    s.update_vm(&vm).await;
    let run = s.task().await;
    let mut second = s.disk(SIZE, PrepareMode::Full);
    second.id = other.id;
    s.receiver
        .prepare(s.request(run, vec![s.disk(SIZE, PrepareMode::Full), second]))
        .await
        .unwrap();
    s.finish(run, true, 1700).await.unwrap();
    vm.spec.disks.pop();
    s.update_vm(&vm).await;
    Box::pin(s.receiver.sweep()).await;
    assert_eq!(s.storage.deleted(), [other.id]);
    let record = s.record().unwrap();
    assert_eq!(record.volumes.len(), 1);
    assert_eq!(record.volumes[0].id, s.volume);
}

/// 청소: 회차 Task가 끝났거나 없는 받는 QEMU를 끄고 그 회차의 받지 못한 겹층을 버린다. 도는 회차는 그대로다.
#[tokio::test]
async fn sweep_discards_the_overlays_and_receivers_of_finished_runs() {
    let s = setup().await;
    s.full_run(1700).await;
    let run = s.task().await;
    s.prepare(run, incremental(1700)).await.unwrap();
    let path = replicas::overlay_path(&s.dir, run, s.volume);
    s.receiver.sweep().await;
    assert!(path.exists());
    assert!(s.env.hv.receivers().contains_key(&run));

    s.end_task(run).await;
    s.receiver.sweep().await;
    assert!(!path.exists());
    assert!(s.env.hv.receivers().is_empty());
    let record = s.record().unwrap();
    assert_eq!(record.volumes[0].overlay, None);
    assert_eq!(record.volumes[0].state, ReplicaState::Ready);
    // Task가 없는 회차의 받는 QEMU도 끈다.
    let orphan = Uuid::now_v7();
    s.env
        .hv
        .start_replica_receiver(orphan, &[], "127.0.0.1", 61000..=61009)
        .await
        .unwrap();
    s.receiver.sweep().await;
    assert!(s.env.hv.receivers().is_empty());
}

/// 청소: 이 노드가 원본이던 VM의 볼륨이 다른 노드로 옮겨 갔으면(다른 노드가 승격했다) 이 노드가 대상이면 옛
/// 복제본(`stale`)으로 바꾸고, 아니면 옛 디스크를 지운다. 마이그레이션이 끝나지 않았으면 그대로 둔다.
#[tokio::test]
async fn sweep_turns_an_old_primary_into_a_stale_replica() {
    let s = setup().await;
    let primary = ReplicaRecord {
        vm: s.vm,
        role: Role::Primary,
        generation: None,
        synced_at: None,
        volumes: vec![RecordedReplica {
            id: s.volume,
            pool: "b".into(),
            format: ImageFormat::Qcow2,
            size_bytes: SIZE,
            created: false,
            state: ReplicaState::Ready,
            overlay: None,
        }],
    };
    replicas::write(&s.dir, &primary).unwrap();
    // 볼륨이 아직 이 노드에 있으면 이 노드가 원본이다.
    s.set_volume_node(s.env.node).await;
    s.receiver.sweep().await;
    assert_eq!(s.record(), Some(primary.clone()));
    // 볼륨은 옮겨 갔지만 마이그레이션이 끝나지 않았다.
    s.set_volume_node(s.source).await;
    let mut vm = s.vm();
    vm.status.migration = Some(MigrationStatus {
        node: s.source,
        task: Uuid::now_v7(),
        disks: vec![],
    });
    s.env
        .cluster
        .update_status::<VmSpec>(s.vm, &vm.status)
        .await
        .unwrap();
    s.receiver.sweep().await;
    assert_eq!(s.record(), Some(primary.clone()));

    vm.status.migration = None;
    s.env
        .cluster
        .update_status::<VmSpec>(s.vm, &vm.status)
        .await
        .unwrap();
    s.receiver.sweep().await;
    let record = s.record().unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.generation, None);
    assert_eq!(record.volumes[0].state, ReplicaState::Stale);
    assert!(record.volumes[0].created);
    assert!(s.storage.deleted().is_empty());
    // 옛 복제본에는 증분을 받지 않는다.
    let run = s.task().await;
    let reply = s
        .receiver
        .prepare(s.request(run, vec![s.disk(SIZE, incremental(0))]))
        .await
        .unwrap();
    assert_eq!(reply.refused, [s.volume]);

    // 이 노드가 대상이 아니면 옛 디스크를 지운다.
    replicas::write(&s.dir, &primary).unwrap();
    let mut vm = s.vm();
    vm.spec.replication = vec![ReplicationTarget {
        node: Uuid::now_v7(),
        schedule: "*/15".into(),
        pool: None,
    }];
    s.update_vm(&vm).await;
    s.end_task(run).await;
    s.receiver.sweep().await;
    assert_eq!(s.record(), None);
    assert_eq!(s.storage.deleted(), [s.volume]);
}

/// 돌아온 옛 원본: 받을 준비가 `primary` 기록을 옛 복제본으로 바꾸고 그 위에 전체로 받는다.
#[tokio::test]
async fn prepare_takes_over_an_old_primary_record() {
    let s = setup().await;
    replicas::write(
        &s.dir,
        &ReplicaRecord {
            vm: s.vm,
            role: Role::Primary,
            generation: None,
            synced_at: None,
            volumes: vec![RecordedReplica {
                id: s.volume,
                pool: "b".into(),
                format: ImageFormat::Qcow2,
                size_bytes: SIZE,
                created: false,
                state: ReplicaState::Ready,
                overlay: None,
            }],
        },
    )
    .unwrap();
    s.storage.place(s.volume, SIZE);
    let run = s.task().await;
    s.prepare(run, PrepareMode::Full).await.unwrap();
    let record = s.record().unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.volumes[0].state, ReplicaState::Syncing);
    assert!(record.volumes[0].created);
    s.finish(run, true, 1700).await.unwrap();
    assert_eq!(s.record().unwrap().volumes[0].state, ReplicaState::Ready);
}

/// 끝내기는 받은 겹층을 합치기 전에 답하고 RPC 밖에서 합친다. 합치는 동안 다른 VM의 받기는 기다리지 않고, 같은 VM의
/// 다음 받기는 합치기가 끝나기를 기다린다.
#[tokio::test]
async fn a_slow_merge_holds_up_only_its_own_vm() {
    let s = setup().await;
    s.full_run(1700).await;
    let (db, data) = s.another_vm("db", "data").await;
    let gate = s.storage.hold_commits();
    let run = s.task().await;
    s.prepare(run, incremental(1700)).await.unwrap();
    tokio::time::timeout(WAIT, s.finish(run, true, 1800))
        .await
        .expect("finishing does not wait for the merge")
        .unwrap();
    let record = s.record().unwrap();
    assert_eq!(record.generation, Some(run));
    assert!(record.volumes[0].overlay.as_ref().unwrap().received);
    let deadline = tokio::time::Instant::now() + WAIT;
    while !s.storage.overlays().contains(&("commit", s.volume)) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the merge did not start"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // 다른 VM의 받기는 합치기를 기다리지 않는다.
    let other = s.task_of(db).await;
    let mut disk = s.disk(SIZE, PrepareMode::Full);
    disk.id = data;
    let req = ReplicaPrepareRequest {
        vm: db,
        task: other,
        run: other,
        volumes: vec![disk],
        dirty_bytes: 0,
    };
    tokio::time::timeout(WAIT, s.receiver.prepare(req))
        .await
        .expect("another VM's prepare does not wait for the merge")
        .unwrap();

    // 같은 VM의 다음 받기는 합치기가 끝나기를 기다린다.
    let next = s.task().await;
    let receiver = s.receiver.clone();
    let req = s.request(next, vec![s.disk(SIZE, incremental(1800))]);
    let pending = tokio::spawn(async move { receiver.prepare(req).await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!pending.is_finished());
    gate.add_permits(1);
    let reply = tokio::time::timeout(WAIT, pending)
        .await
        .expect("prepare after the merge")
        .unwrap()
        .unwrap();
    assert!(reply.refused.is_empty(), "{reply:?}");
    assert_eq!(
        s.storage.overlays(),
        [
            ("create", s.volume),
            ("commit", s.volume),
            ("create", s.volume)
        ]
    );
    let record = s.record().unwrap();
    assert_eq!(record.volumes[0].overlay.as_ref().unwrap().run, next);
}

/// 받기 준비가 남은 겹층을 합치는(`qemu-img commit`) 동안에도 기록 디렉터리를 쥐지 않는다: 다른 VM의 받기는 기다리지
/// 않고, 합치기가 끝나면 그 VM의 받기가 이어진다.
#[tokio::test]
async fn a_merge_in_prepare_holds_up_only_its_own_vm() {
    let s = setup().await;
    s.full_run(1700).await;
    let (db, data) = s.another_vm("db", "data").await;
    let run = s.task().await;
    s.prepare(run, incremental(1700)).await.unwrap();
    // 끝내기 뒤의 합치기가 실패해 받은 겹층이 남는다(다음 받기가 먼저 합친다).
    s.storage.fail_commits(1);
    s.finish(run, true, 1800).await.unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    while !s.storage.overlays().contains(&("commit", s.volume)) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the merge did not start"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let gate = s.storage.hold_commits();
    let next = s.task().await;
    let receiver = s.receiver.clone();
    let req = s.request(next, vec![s.disk(SIZE, incremental(1800))]);
    let pending = tokio::spawn(async move { receiver.prepare(req).await });
    let deadline = tokio::time::Instant::now() + WAIT;
    while s
        .storage
        .overlays()
        .iter()
        .filter(|o| **o == ("commit", s.volume))
        .count()
        < 2
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the prepare did not start merging"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // 다른 VM의 받기는 그 합치기를 기다리지 않는다.
    let other = s.task_of(db).await;
    let mut disk = s.disk(SIZE, PrepareMode::Full);
    disk.id = data;
    let req = ReplicaPrepareRequest {
        vm: db,
        task: other,
        run: other,
        volumes: vec![disk],
        dirty_bytes: 0,
    };
    tokio::time::timeout(WAIT, s.receiver.prepare(req))
        .await
        .expect("another VM's prepare does not wait for the merge")
        .unwrap();
    assert!(!pending.is_finished());
    gate.add_permits(1);
    let reply = tokio::time::timeout(WAIT, pending)
        .await
        .expect("prepare after the merge")
        .unwrap()
        .unwrap();
    assert!(reply.refused.is_empty(), "{reply:?}");
    assert_eq!(
        s.record().unwrap().volumes[0].overlay.as_ref().unwrap().run,
        next
    );
}

/// 끝내기는 원본이 이번 회차에 보낸 디스크만 온전한 것으로 적는다(목록이 없는 옛 원본의 요청이면 모두).
#[tokio::test]
async fn finishing_marks_only_the_disks_of_the_run() {
    let s = setup().await;
    let data = s.another_volume("data").await;
    let mut vm = s.vm();
    vm.spec
        .disks
        .push(serde_json::from_value(json!({ "volume": "data", "bus": "virtio-blk" })).unwrap());
    s.update_vm(&vm).await;
    let run = s.task().await;
    let mut second = s.disk(SIZE, PrepareMode::Full);
    second.id = data;
    s.receiver
        .prepare(s.request(run, vec![s.disk(SIZE, PrepareMode::Full), second]))
        .await
        .unwrap();
    s.receiver
        .finish(ReplicaFinishRequest {
            vm: s.vm,
            task: run,
            run,
            ok: true,
            synced_at: 1700,
            volumes: vec![s.volume],
        })
        .await
        .unwrap();
    let record = s.record().unwrap();
    let state = |id| record.volumes.iter().find(|v| v.id == id).unwrap().state;
    assert_eq!(state(s.volume), ReplicaState::Ready);
    assert_eq!(state(data), ReplicaState::Syncing);
}

/// 이 노드가 만든 복제본 파일이 없어졌으면(기록은 남음) 다음 전체 회차가 같은 id로 다시 만든다.
#[tokio::test]
async fn a_lost_replica_file_is_created_again() {
    let s = setup().await;
    s.full_run(1700).await;
    s.storage.lose_file(s.volume);
    let run = s.task().await;
    s.prepare(run, PrepareMode::Full).await.unwrap();
    assert!(s.storage.has_file(s.volume));
    s.finish(run, true, 1800).await.unwrap();
    let record = s.record().unwrap();
    assert_eq!(record.volumes[0].state, ReplicaState::Ready);
    assert!(record.volumes[0].created);
}

/// 증분을 합치면 복제본이 바뀐 만큼 자랄 수 있으므로 증분도 복제본 풀의 남은 공간을 본다.
#[tokio::test]
async fn increments_need_space_in_the_replica_pool() {
    let s = setup().await;
    s.full_run(1700).await;
    let state = PoolState {
        ready: true,
        available_bytes: 1000,
        total_bytes: 1 << 30,
        ..Default::default()
    };
    set_pool_state(&s.env.cluster, s.env.node, "b", state).await;
    let run = s.task().await;
    let err = s.prepare(run, incremental(1700)).await.unwrap_err();
    assert_eq!(
        err,
        ReplicaError::Failed(format!(
            "not enough space in storage pool \"b\" on node \"{}\": need 65536, available 1000",
            node_name(&s.env)
        ))
    );
    assert!(s.storage.overlays().is_empty());
}

/// 옛 원본의 기록에 있지만 다른 VM이 쓰는 볼륨(원본이 떼어 다른 VM에 붙였다)은 옛 복제본으로 바꾸거나 지우지 않고
/// 기록에서 뺀다. 그 VM의 볼륨으로 다른 노드에 옮겨 간 것만 옛 복제본이다.
#[tokio::test]
async fn an_old_primary_leaves_out_disks_of_other_vms() {
    let s = setup().await;
    // data는 원본 노드에서 도는 다른 VM db의 디스크다.
    let (_, data) = s.another_vm("db", "data").await;
    let entry = |id| RecordedReplica {
        id,
        pool: "b".into(),
        format: ImageFormat::Qcow2,
        size_bytes: SIZE,
        created: false,
        state: ReplicaState::Ready,
        overlay: None,
    };
    let primary = ReplicaRecord {
        vm: s.vm,
        role: Role::Primary,
        generation: None,
        synced_at: None,
        volumes: vec![entry(s.volume), entry(data)],
    };
    replicas::write(&s.dir, &primary).unwrap();
    s.receiver.sweep().await;
    let record = s.record().unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.volumes.len(), 1);
    assert_eq!(record.volumes[0].id, s.volume);
    assert!(record.volumes[0].created);
    assert_eq!(record.volumes[0].state, ReplicaState::Stale);

    // 이 노드가 대상이 아니면 옛 사본만 지운다.
    replicas::write(&s.dir, &primary).unwrap();
    let mut vm = s.vm();
    vm.spec.replication.clear();
    s.update_vm(&vm).await;
    s.receiver.sweep().await;
    assert_eq!(s.record(), None);
    assert_eq!(s.storage.deleted(), [s.volume]);
}

/// 돌아온 옛 원본의 기록에 아직 이 노드의 볼륨인 디스크(떼어 둔 디스크 — 볼륨 기록이 이 노드)가 있으면 받을 준비는
/// 그 디스크를 옛 복제본으로 바꾸지 않고 기록에서 뺀다(지우지 않는다).
#[tokio::test]
async fn prepare_leaves_out_old_primary_disks_still_on_this_node() {
    let s = setup().await;
    let data = s.another_volume("data").await;
    let mut v = s.env.cluster.get::<VolumeSpec>(data).unwrap().unwrap();
    v.status.node = Some(s.env.node);
    s.env
        .cluster
        .update_status::<VolumeSpec>(data, &v.status)
        .await
        .unwrap();
    let entry = |id| RecordedReplica {
        id,
        pool: "b".into(),
        format: ImageFormat::Qcow2,
        size_bytes: SIZE,
        created: false,
        state: ReplicaState::Ready,
        overlay: None,
    };
    replicas::write(
        &s.dir,
        &ReplicaRecord {
            vm: s.vm,
            role: Role::Primary,
            generation: None,
            synced_at: None,
            volumes: vec![entry(s.volume), entry(data)],
        },
    )
    .unwrap();
    s.storage.place(s.volume, SIZE);
    s.storage.place(data, SIZE);
    let run = s.task().await;
    s.prepare(run, PrepareMode::Full).await.unwrap();
    let record = s.record().unwrap();
    assert_eq!(record.role, Role::Replica);
    assert_eq!(record.volumes.len(), 1);
    assert_eq!(record.volumes[0].id, s.volume);
    assert!(record.volumes[0].created);
    assert!(s.storage.has_file(data));
    assert!(s.storage.deleted().is_empty());
}

fn node_name(env: &Env) -> String {
    env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap().name
}
