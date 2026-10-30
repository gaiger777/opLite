//! 라이브 마이그레이션의 받는 쪽(`IncomingMigrations`): 원본 노드가 RPC로 부르는 처리를 직접 부른다.
//! 이 노드(Env의 노드)가 대상이고, 원본 노드는 저장소에만 있다. Task 실행기의 남은 마이그레이션
//! QEMU 정리도 본다.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Env, FakeStorage, MOUNT_ROOT, vm_request};
use opl_api::{IncomingConfig, IncomingMigrations};
use opl_driver::mock::MOCK_QEMU_VERSION;
use opl_driver::{DriverError, Hypervisor, MigrateOptions, MigrateTarget, RuntimeStatus, Storage};
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
use opl_types::vm::{MigrationStatus, Vm, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

struct Setup {
    env: Env,
    storage: Arc<FakeStorage>,
    receiver: IncomingMigrations,
    /// 원본 노드
    source: Uuid,
    vm: Uuid,
    volume: Uuid,
}

/// 공유 풀 `nas`의 볼륨 `root`를 디스크로 쓰는 VM `web`을 원본 노드에 배정해 둔다(이 노드의
/// reconciler는 손대지 않는다).
async fn setup() -> Setup {
    let env = Env::start().await;
    let admin = env.admin().await;
    let source: Node = Resource::new(
        "node2",
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
        },
    );
    env.cluster.create(&source).await.unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": "nas",
                "spec": {
                    "backend": { "kind": "dir", "path": env.dir.path().join("nas") },
                    "shared": true
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("nas").await;
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "nas".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(source.id);
    volume.status.phase = VolumePhase::Ready;
    env.cluster.create(&volume).await.unwrap();
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    let mut req = vm_request("web");
    req["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut vm = vm_named(&env, "web");
    vm.spec.node = Some(source.id);
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();

    let storage = Arc::new(FakeStorage::default());
    let receiver = IncomingMigrations::new(
        env.cluster.clone(),
        env.hv.clone() as Arc<dyn Hypervisor>,
        IncomingConfig {
            node: env.node,
            mount_root: MOUNT_ROOT.into(),
            storage: storage.clone() as Arc<dyn Storage>,
            listen_host: "127.0.0.1".into(),
            ports: 61000..=61009,
        },
    );
    Setup {
        source: source.id,
        vm: vm.id,
        volume: volume.id,
        env,
        storage,
        receiver,
    }
}

fn vm_named(env: &Env, name: &str) -> Vm {
    env.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap()
}

impl Setup {
    /// 원본 노드가 하듯 VM을 `Migrating`으로, 대상을 `node`로 기록한다.
    async fn mark_migrating(&self, node: Uuid, task: Uuid) {
        let mut vm = self.env.cluster.get::<VmSpec>(self.vm).unwrap().unwrap();
        vm.status.phase = VmPhase::Migrating;
        vm.status.node = Some(self.source);
        vm.status.migration = Some(MigrationStatus { node, task });
        self.env
            .cluster
            .update_status::<VmSpec>(self.vm, &vm.status)
            .await
            .unwrap();
    }

    fn request(&self, task: Uuid) -> MigrateIncomingRequest {
        MigrateIncomingRequest {
            vm: self.vm,
            task,
            qemu_version: MOCK_QEMU_VERSION.into(),
        }
    }

    fn deactivations(&self) -> usize {
        self.storage
            .calls()
            .iter()
            .filter(|(op, id)| *op == "deactivate" && *id == self.volume)
            .count()
    }
}

#[tokio::test]
async fn receives_waits_and_aborts() {
    let s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;

    let port = s.receiver.incoming(s.request(task)).await.unwrap();
    assert_eq!(port, 61000);
    assert!(s.env.hv.is_incoming(s.vm));
    assert_eq!(s.env.hv.list_incoming().await.unwrap(), vec![s.vm]);
    // 원본 노드에 배정된 VM이므로 이 노드의 reconciler는 받는 QEMU를 그대로 둔다.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_some());
    assert_eq!(s.deactivations(), 0);

    let migration = IncomingMigration { vm: s.vm, task };
    s.receiver.wait(migration).await.unwrap();
    // 다른 Task의 확인은 거절한다.
    let other = IncomingMigration {
        vm: s.vm,
        task: Uuid::now_v7(),
    };
    let err = s.receiver.wait(other).await.unwrap_err();
    assert!(matches!(err, IncomingError::Rejected(_)), "{err:?}");
    let err = s.receiver.abort(other).await.unwrap_err();
    assert!(matches!(err, IncomingError::Rejected(_)), "{err:?}");
    assert!(s.env.hv.is_incoming(s.vm));

    // 확정 전 취소: 받는 QEMU를 끄고 디스크를 끈다. 다시 불러도 된다.
    s.receiver.abort(migration).await.unwrap();
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert!(s.env.hv.list_incoming().await.unwrap().is_empty());
    assert_eq!(s.deactivations(), 1);
    s.receiver.abort(migration).await.unwrap();
}

#[tokio::test]
async fn waits_for_the_migrating_status_to_arrive() {
    let s = setup().await;
    let task = Uuid::now_v7();
    // 원본이 상태를 쓰기 전에 요청이 먼저 와도 상태가 보이면 받는다.
    let incoming = s.receiver.incoming(s.request(task));
    let mark = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        s.mark_migrating(s.env.node, task).await;
    };
    let (port, ()) = tokio::join!(incoming, mark);
    assert_eq!(port.unwrap(), 61000);
    assert!(s.env.hv.is_incoming(s.vm));
}

#[tokio::test]
async fn refuses_a_vm_not_migrating_here_or_another_qemu() {
    let s = setup().await;
    let task = Uuid::now_v7();

    // 다른 QEMU 버전
    s.mark_migrating(s.env.node, task).await;
    s.env.hv.set_qemu_version("9.0.0");
    let err = s.receiver.incoming(s.request(task)).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Incompatible(m) if m.contains("9.0.0") && m.contains(MOCK_QEMU_VERSION)),
        "{err:?}"
    );
    s.env.hv.set_qemu_version(MOCK_QEMU_VERSION);

    // 다른 노드로 가는 중이다(기다려도 이 노드로 오는 상태가 보이지 않는다).
    s.mark_migrating(s.source, task).await;
    let err = s.receiver.incoming(s.request(task)).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Rejected(m) if m.contains("not migrating to this node")),
        "{err:?}"
    );
    // 다른 Task로 오는 중이다.
    s.mark_migrating(s.env.node, Uuid::now_v7()).await;
    let err = s
        .receiver
        .wait(IncomingMigration { vm: s.vm, task })
        .await
        .unwrap_err();
    assert!(matches!(err, IncomingError::Rejected(_)), "{err:?}");
    assert!(s.env.hv.launches().is_empty());
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert_eq!(s.deactivations(), 0);
}

#[tokio::test]
async fn failures_turn_disks_off_again() {
    let s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;

    // QEMU를 띄우지 못하면 켠 디스크를 끈다.
    s.env
        .hv
        .fail_next_start(DriverError::StartFailed("no such machine".into()));
    let err = s.receiver.incoming(s.request(task)).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Failed(m) if m.contains("no such machine")),
        "{err:?}"
    );
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert_eq!(s.deactivations(), 1);

    // 받기에 실패하면 받는 QEMU는 스스로 끝난다. 취소가 디스크를 끈다.
    s.receiver.incoming(s.request(task)).await.unwrap();
    s.env
        .hv
        .fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let migration = IncomingMigration { vm: s.vm, task };
    let err = s.receiver.wait(migration).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Failed(m) if m.contains("TLS handshake failed")),
        "{err:?}"
    );
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    s.receiver.abort(migration).await.unwrap();
    assert_eq!(s.deactivations(), 2);
}

#[tokio::test]
async fn abort_leaves_a_committed_migration_alone() {
    let s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;
    s.receiver.incoming(s.request(task)).await.unwrap();
    let migration = IncomingMigration { vm: s.vm, task };
    s.receiver.wait(migration).await.unwrap();

    // 원본이 확정했다(spec.node가 이 노드). 늦게 온 취소는 받은 QEMU를 끄지 않는다.
    let mut vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    vm.spec.node = Some(s.env.node);
    s.env
        .cluster
        .update_spec(s.vm, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    let err = s.receiver.abort(migration).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Rejected(m) if m.contains("committed")),
        "{err:?}"
    );
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_some());
    assert_eq!(s.deactivations(), 0);
}

impl Setup {
    /// 남은 마이그레이션 QEMU를 자주 정리하는 Task 실행기로 바꾼다(디스크는 `storage`로 끈다).
    async fn sweep_often(&mut self) {
        self.env.stop_runner().await;
        let storage = self.storage.clone() as Arc<dyn Storage>;
        self.env
            .start_runner_config(storage, |c| c.tag_sweep = Duration::from_millis(50))
            .await;
    }

    async fn running(&self, id: Uuid) -> Option<RuntimeStatus> {
        self.env.hv.inspect(id).await.unwrap().map(|s| s.status)
    }

    async fn wait_stopped(&self, id: Uuid) {
        let deadline = std::time::Instant::now() + common::WAIT;
        while self.running(id).await.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "QEMU of {id} was not stopped"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// 원본이 마이그레이션을 되돌렸는데(이 노드로 오는 표시가 없다) 받는 QEMU가 남았다: 정리가 끄고
/// 디스크를 끈다. 이 노드로 오는 중이면 그대로 둔다.
#[tokio::test]
async fn sweep_stops_a_receiving_qemu_left_behind() {
    let mut s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;
    s.receiver.incoming(s.request(task)).await.unwrap();
    s.sweep_often().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s.env.hv.is_incoming(s.vm));
    assert_eq!(s.deactivations(), 0);

    let mut vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    vm.status.phase = VmPhase::Running;
    vm.status.migration = None;
    s.env
        .cluster
        .update_status::<VmSpec>(s.vm, &vm.status)
        .await
        .unwrap();
    s.wait_stopped(s.vm).await;
    assert!(s.env.hv.list_incoming().await.unwrap().is_empty());
    assert_eq!(s.deactivations(), 1);
}

/// 이 노드에서 보낸 원본(`postmigrate`)이 남았는데 VM은 다른 노드로 확정됐다: 정리가 끄고 디스크를
/// 끈다. 이 노드에 배정된 VM의 원본(확정 전)은 그대로 둔다.
#[tokio::test]
async fn sweep_stops_a_source_qemu_committed_elsewhere() {
    let mut s = setup().await;
    let admin = s.env.admin().await;
    let target = MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    };
    // `web`은 원본 노드에 배정돼 있다(이 노드에서 보냈고 원본 노드로 확정된 것과 같다).
    let vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    let spec = VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    s.env.hv.start(&spec, None).await.unwrap();
    s.env.hv.migrate(vm.id, &target, &opts).await.unwrap();
    // `db`는 이 노드에 배정돼 있고 확정 전이다.
    let mut req = vm_request("db");
    req["spec"]["node"] = json!(s.env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let db = vm_named(&s.env, "db");
    s.env.hv.migrate(db.id, &target, &opts).await.unwrap();

    s.sweep_often().await;
    s.wait_stopped(s.vm).await;
    assert_eq!(s.deactivations(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        s.running(db.id).await,
        Some(RuntimeStatus::Other("postmigrate".into()))
    );
}
