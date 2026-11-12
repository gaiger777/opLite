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
use opl_peer::{
    IncomingDisk, IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver,
};
use opl_reconciler::FirewallGate;
use opl_types::network::NetworkSpec;
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
use opl_types::vm::{DiskMove, MigrationStatus, Vm, VmPhase, VmSpec};
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
            witness: false,
            roles: None,
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
            migration_disks: env.dir.path().join("migration-disks"),
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
        vm.status.migration = Some(MigrationStatus {
            node,
            task,
            disks: vec![],
        });
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
            disks: vec![],
        }
    }

    fn deactivations(&self) -> usize {
        self.count("deactivate")
    }

    /// 볼륨에 대한 저장소 호출 `op`의 수
    fn count(&self, op: &str) -> usize {
        self.storage
            .calls()
            .iter()
            .filter(|(o, id)| *o == op && *id == self.volume)
            .count()
    }
}

#[tokio::test]
async fn receives_waits_and_aborts() {
    let s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;

    let reply = s.receiver.incoming(s.request(task)).await.unwrap();
    assert_eq!(reply.port, 61000);
    // 복사할 디스크가 없으면 NBD 서버를 열지 않는다.
    assert_eq!(reply.nbd_port, None);
    assert!(s.env.hv.is_incoming(s.vm));
    // 원본이 아직 쓰는 디스크를 공유로 켰다.
    assert_eq!(s.count("share"), 1);
    assert_eq!(s.env.hv.list_incoming().await.unwrap(), vec![(s.vm, task)]);
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
    assert_eq!(port.unwrap().port, 61000);
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

/// 앞선 마이그레이션(다른 Task)의 받는 QEMU가 남아 있으면 끄고 새로 받는다. 취소는 그 Task의 받는
/// QEMU만 끈다.
#[tokio::test]
async fn receiving_qemu_of_another_task_is_replaced() {
    let s = setup().await;
    let old = Uuid::now_v7();
    s.mark_migrating(s.env.node, old).await;
    s.receiver.incoming(s.request(old)).await.unwrap();

    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;
    s.receiver.incoming(s.request(task)).await.unwrap();
    assert_eq!(s.env.hv.list_incoming().await.unwrap(), vec![(s.vm, task)]);
    assert_eq!(s.env.hv.launches().len(), 0);

    // 마이그레이션 표시가 없어도 다른 Task의 취소는 이 받는 QEMU를 끄지 않는다(정리가 끈다).
    let mut vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    vm.status.phase = VmPhase::Running;
    vm.status.migration = None;
    s.env
        .cluster
        .update_status::<VmSpec>(s.vm, &vm.status)
        .await
        .unwrap();
    s.receiver
        .abort(IncomingMigration {
            vm: s.vm,
            task: old,
        })
        .await
        .unwrap();
    assert!(s.env.hv.is_incoming(s.vm));
    assert_eq!(s.deactivations(), 0);
    s.receiver
        .abort(IncomingMigration { vm: s.vm, task })
        .await
        .unwrap();
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert_eq!(s.deactivations(), 1);
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

/// 이 노드로 오는 중이지만 다른 Task다(앞선 마이그레이션의 받는 QEMU가 남았다): 정리가 끈다.
#[tokio::test]
async fn sweep_stops_a_receiving_qemu_of_another_task() {
    let mut s = setup().await;
    let old = Uuid::now_v7();
    s.mark_migrating(s.env.node, old).await;
    s.receiver.incoming(s.request(old)).await.unwrap();
    s.mark_migrating(s.env.node, Uuid::now_v7()).await;
    s.sweep_often().await;
    s.wait_stopped(s.vm).await;
    assert!(s.env.hv.list_incoming().await.unwrap().is_empty());
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
        switchover_mirrors: false,
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
    // 정리는 끄지 않는다(이 노드의 reconciler가 이어서 실행할 수는 있다).
    assert!(s.running(db.id).await.is_some());
}

/// 보낸 원본(`postmigrate`)이 남았는데 VM이 지워졌다(다른 노드로 옮긴 뒤 끄고 지웠다): 정리가 끈다.
#[tokio::test]
async fn sweep_stops_a_source_qemu_of_a_deleted_vm() {
    let mut s = setup().await;
    let target = MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
        switchover_mirrors: false,
    };
    let vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    let spec = VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "gone".into(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    s.env.hv.start(&spec, None).await.unwrap();
    s.env.hv.migrate(spec.id, &target, &opts).await.unwrap();
    s.sweep_often().await;
    s.wait_stopped(spec.id).await;
}

/// 이 노드에 배정된 VM의 원본(`postmigrate`)이 남았는데 이 노드에서 돌던 VM이 아니다(다른 노드로
/// 확정된 뒤 거기서 꺼지고 이 노드로 다시 옮겨졌다): 낡은 원본이므로 정리가 끈다. 디스크는 이 노드의
/// reconciler가 다루므로 끄지 않는다.
#[tokio::test]
async fn sweep_stops_a_stale_source_qemu_assigned_here() {
    let mut s = setup().await;
    let admin = s.env.admin().await;
    let mut req = vm_request("db");
    req["spec"]["node"] = json!(s.env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let db = vm_named(&s.env, "db");
    // 정리만 보도록 reconciler는 손대지 않게 한다.
    s.env.reconciler().begin_migration(db.id).await;
    let target = MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
        switchover_mirrors: false,
    };
    s.env.hv.migrate(db.id, &target, &opts).await.unwrap();
    let mut status = db.status.clone();
    status.node = Some(s.source);
    status.phase = VmPhase::Stopped;
    status.pid = None;
    s.env
        .cluster
        .update_status::<VmSpec>(db.id, &status)
        .await
        .unwrap();

    s.sweep_often().await;
    s.wait_stopped(db.id).await;
    assert_eq!(s.deactivations(), 0);
}

/// 부를 때마다 `fail`이면 실패하는 방화벽
struct Gate {
    fail: bool,
}

#[async_trait::async_trait]
impl FirewallGate for Gate {
    async fn sync(&self) -> Result<(), String> {
        if self.fail {
            Err("nft: boom".into())
        } else {
            Ok(())
        }
    }
}

/// 보안 그룹을 건 NIC가 있는 VM은 방화벽 규칙을 넣은 뒤에만 받는다. 넣지 못하면(방화벽이 없는 노드
/// 포함) 디스크도 켜지 않고 거절한다.
#[tokio::test]
async fn a_vm_with_security_groups_needs_the_firewall() {
    let s = setup().await;
    let lan = Resource::new(
        "lan",
        NetworkSpec {
            bridge: Some("vmbr0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    s.env.cluster.create(&lan).await.unwrap();
    let mut vm = vm_named(&s.env, "web");
    vm.spec.nics[0].network = "lan".into();
    vm.spec.nics[0].security_groups = vec!["web".into()];
    s.env
        .cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;
    let receiver = |gate: Option<Gate>| {
        IncomingMigrations::new(
            s.env.cluster.clone(),
            s.env.hv.clone() as Arc<dyn Hypervisor>,
            IncomingConfig {
                node: s.env.node,
                mount_root: MOUNT_ROOT.into(),
                storage: s.storage.clone() as Arc<dyn Storage>,
                listen_host: "127.0.0.1".into(),
                ports: 61000..=61009,
                migration_disks: s.env.dir.path().join("migration-disks"),
            },
        )
        .with_firewall(gate.map(|g| Arc::new(g) as Arc<dyn FirewallGate>))
    };

    for (gate, why) in [
        (None, "firewall: this node has no firewall (nftables)"),
        (Some(Gate { fail: true }), "firewall: nft: boom"),
    ] {
        let err = receiver(gate).incoming(s.request(task)).await.unwrap_err();
        assert!(
            matches!(&err, IncomingError::Failed(e) if e == why),
            "{err:?}"
        );
        assert!(!s.env.hv.is_incoming(s.vm));
        assert_eq!(s.count("share"), 0);
    }
    let port = receiver(Some(Gate { fail: false }))
        .incoming(s.request(task))
        .await
        .unwrap();
    assert_eq!(port.port, 61000);
    assert!(s.env.hv.is_incoming(s.vm));
}

/// 원본 노드의 로컬 디스크(복사해 옮기는 디스크)
const LOCAL_SIZE: u64 = 1 << 20;

impl Setup {
    /// 로컬 디렉터리 풀 `local`(공유 아님)에 원본 노드의 볼륨 `data`(qcow2)를 만들어 VM의 둘째 디스크로
    /// 붙인다.
    async fn local_disk(&self) -> Uuid {
        let admin = self.env.admin().await;
        let (status, body) = admin
            .post(
                "/storage-pools",
                json!({
                    "name": "local",
                    "spec": {
                        "backend": { "kind": "dir", "path": self.env.dir.path().join("local") }
                    }
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        self.env.pool_ready("local").await;
        let mut volume: Volume = Resource::new(
            "data",
            VolumeSpec {
                pool: "local".into(),
                size_bytes: LOCAL_SIZE,
                format: ImageFormat::Qcow2,
            },
        );
        volume.status.node = Some(self.source);
        volume.status.phase = VolumePhase::Ready;
        self.env.cluster.create(&volume).await.unwrap();
        let mut vm = vm_named(&self.env, "web");
        vm.spec.disks.push(opl_types::vm::DiskSpec {
            volume: "data".into(),
            bus: opl_types::vm::DiskBus::VirtioBlk,
            boot_order: None,
        });
        self.env
            .cluster
            .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
            .await
            .unwrap();
        volume.id
    }

    /// 원본 노드가 하듯 `Migrating`과 이 노드로 복사할 디스크 `volume`(같은 풀, 같은 형식)을 기록한다.
    async fn mark_copying(&self, task: Uuid, volume: Uuid) {
        self.mark_migrating(self.env.node, task).await;
        let mut vm = self.env.cluster.get::<VmSpec>(self.vm).unwrap().unwrap();
        vm.status.migration.as_mut().unwrap().disks = vec![DiskMove {
            volume,
            pool: "local".into(),
            format: ImageFormat::Qcow2,
        }];
        self.env
            .cluster
            .update_status::<VmSpec>(self.vm, &vm.status)
            .await
            .unwrap();
    }

    fn copy_request(&self, task: Uuid, volume: Uuid) -> MigrateIncomingRequest {
        MigrateIncomingRequest {
            disks: vec![IncomingDisk {
                volume,
                size_bytes: LOCAL_SIZE,
            }],
            ..self.request(task)
        }
    }

    /// 이 노드의 마이그레이션 디스크 기록 파일
    fn record(&self, task: Uuid) -> Option<serde_json::Value> {
        let data = std::fs::read(self.record_path(task)).ok()?;
        Some(serde_json::from_slice(&data).unwrap())
    }

    fn record_path(&self, task: Uuid) -> std::path::PathBuf {
        self.env
            .dir
            .path()
            .join("migration-disks")
            .join(format!("{task}.json"))
    }

    /// 받는 쪽처럼 VM `vm`의 Task `task`의 기록 파일(`target`, 풀 `local`의 볼륨 `volume`)을 적는다.
    fn write_record(&self, vm: Uuid, task: Uuid, volume: Uuid, created: bool) {
        let record = json!({
            "vm": vm,
            "task": task,
            "role": "target",
            "volumes": [{ "id": volume, "pool": "local", "format": "qcow2", "created": created }],
        });
        std::fs::create_dir_all(self.record_path(task).parent().unwrap()).unwrap();
        std::fs::write(self.record_path(task), record.to_string()).unwrap();
    }

    /// 원본이 되돌린 것처럼 마이그레이션 표시를 지운다.
    async fn roll_back(&self) {
        let mut vm = self.env.cluster.get::<VmSpec>(self.vm).unwrap().unwrap();
        vm.status.phase = VmPhase::Running;
        vm.status.migration = None;
        self.env
            .cluster
            .update_status::<VmSpec>(self.vm, &vm.status)
            .await
            .unwrap();
    }
}

/// 복사할 디스크: 이 노드에 원본 크기로 만들고 기록 파일에 적은 뒤, 공유하지 않고 켜서 NBD로
/// 내보낸다(VM의 둘째 디스크). 취소하면 만든 디스크와 기록을 지운다.
#[tokio::test]
async fn creates_exports_and_deletes_the_copied_disks() {
    let s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;

    let reply = s
        .receiver
        .incoming(s.copy_request(task, data))
        .await
        .unwrap();
    assert_eq!(reply.port, 61000);
    assert_eq!(reply.nbd_port, Some(61001));
    assert_eq!(s.env.hv.exports(s.vm), [1]);
    assert!(s.storage.has_file(data));
    let record = s.record(task).unwrap();
    assert_eq!(record["role"], "target");
    assert_eq!(record["vm"], s.vm.to_string());
    assert_eq!(
        record["volumes"],
        json!([{ "id": data, "pool": "local", "format": "qcow2", "created": true }])
    );
    // 공유 디스크만 공유로 켰다.
    assert_eq!(s.count("share"), 1);
    assert!(!s.storage.calls().contains(&("share", data)));

    // 원본이 되돌린 뒤(표시를 지움) 취소한다.
    let mut vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    vm.status.phase = VmPhase::Running;
    vm.status.migration = None;
    s.env
        .cluster
        .update_status::<VmSpec>(s.vm, &vm.status)
        .await
        .unwrap();
    s.receiver
        .abort(IncomingMigration { vm: s.vm, task })
        .await
        .unwrap();
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert_eq!(s.storage.deleted(), [data]);
    assert!(!s.storage.has_file(data));
    assert_eq!(s.record(task), None);
}

/// 요청의 복사할 디스크가 VM의 마이그레이션 계획과 다르면 거절한다(아무것도 만들지 않는다).
#[tokio::test]
async fn refuses_disks_that_differ_from_the_migration() {
    let s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;
    for req in [s.request(task), s.copy_request(task, s.volume)] {
        let err = s.receiver.incoming(req).await.unwrap_err();
        assert!(
            matches!(&err, IncomingError::Rejected(m) if m.contains("do not match")),
            "{err:?}"
        );
    }
    assert!(!s.storage.has_file(data));
    assert!(!s.env.hv.is_incoming(s.vm));
    assert_eq!(s.record(task), None);
}

/// 만든 디스크의 크기가 원본과 다르면(백엔드가 올림) 지우고 실패한다. 이미 있는 볼륨(남의 파일일 수
/// 있다)은 지우지 않고 실패한다.
#[tokio::test]
async fn refuses_a_changed_size_or_an_existing_volume() {
    let s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;

    s.storage.round_creates(4 << 20);
    let err = s
        .receiver
        .incoming(s.copy_request(task, data))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        IncomingError::Failed(
            "disk \"data\" would change size on pool \"local\" (1048576 → 4194304 bytes)".into()
        )
    );
    assert_eq!(s.storage.deleted(), [data]);
    assert!(!s.storage.has_file(data));
    assert_eq!(s.record(task), None);
    assert!(!s.env.hv.is_incoming(s.vm));

    s.storage.round_creates(0);
    s.storage.place(data, LOCAL_SIZE);
    let err = s
        .receiver
        .incoming(s.copy_request(task, data))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Failed(m)
            if m.contains("volume \"data\" already exists on node")
                && m.ends_with("in pool \"local\"; is the pool really node-local?")),
        "{err:?}"
    );
    assert_eq!(s.storage.deleted(), [data]);
    assert!(s.storage.has_file(data));
    assert_eq!(s.record(task), None);
    assert!(!s.env.hv.is_incoming(s.vm));
}

/// 받는 QEMU를 띄우지 못하면 만든 디스크를 지운다.
#[tokio::test]
async fn failed_start_deletes_the_copied_disks() {
    let s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;
    s.env
        .hv
        .fail_next_start(DriverError::Migration("cannot listen".into()));
    let err = s
        .receiver
        .incoming(s.copy_request(task, data))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Failed(m) if m.contains("cannot listen")),
        "{err:?}"
    );
    assert_eq!(s.storage.deleted(), [data]);
    assert_eq!(s.record(task), None);
}

/// 원본이 확정 전에 되돌렸는데(취소가 오지 않았다) 받는 QEMU와 만든 디스크가 남았다: 정리가 QEMU를
/// 끄고 기록대로 디스크를 지운다. 이 노드로 오는 중이면 그대로 둔다.
#[tokio::test]
async fn sweep_deletes_copied_disks_left_behind() {
    let mut s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;
    s.receiver
        .incoming(s.copy_request(task, data))
        .await
        .unwrap();
    s.sweep_often().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s.storage.has_file(data));
    assert!(s.record(task).is_some());

    let mut vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    vm.status.phase = VmPhase::Running;
    vm.status.migration = None;
    s.env
        .cluster
        .update_status::<VmSpec>(s.vm, &vm.status)
        .await
        .unwrap();
    s.wait_stopped(s.vm).await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while s.record(task).is_some() {
        assert!(std::time::Instant::now() < deadline, "record left");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(s.storage.deleted(), [data]);
    assert!(!s.storage.has_file(data));
}

/// 만들기 전에 적은 항목(만들다 멈췄거나 이미 있던 남의 볼륨)은 취소도 정리도 지우지 않고 기록에서만
/// 뺀다.
#[tokio::test]
async fn disks_not_created_here_are_never_deleted() {
    let mut s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;
    s.write_record(s.vm, task, data, false);
    s.storage.place(data, LOCAL_SIZE);
    s.roll_back().await;
    s.receiver
        .abort(IncomingMigration { vm: s.vm, task })
        .await
        .unwrap();
    assert!(s.storage.has_file(data));
    assert!(s.storage.deleted().is_empty());
    assert_eq!(s.record(task), None);

    // 정리도 그렇다(다른 VM의 고아 기록).
    let orphan = Uuid::now_v7();
    s.write_record(Uuid::now_v7(), orphan, data, false);
    s.sweep_often().await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while s.record(orphan).is_some() {
        assert!(std::time::Instant::now() < deadline, "record left");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(s.storage.has_file(data));
    assert!(s.storage.deleted().is_empty());
}

/// 복사할 디스크를 만들고 받는 QEMU를 띄우는 동안 정리는 기다린다(막 만든 같은 id의 볼륨을 지우지
/// 않게 이 노드의 디스크 기록 잠금을 함께 쓴다).
#[tokio::test]
async fn sweep_waits_while_disks_are_being_received() {
    let mut s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;
    s.sweep_often().await;
    // 정리가 지울 다른 VM의 고아 사본
    let (orphan, orphan_task) = (Uuid::now_v7(), Uuid::now_v7());
    s.env.hv.hold_next_start();
    let receiving = s.receiver.incoming(s.copy_request(task, data));
    let (received, ()) = tokio::join!(receiving, async {
        s.env.hv.start_held().await;
        s.storage.place(orphan, LOCAL_SIZE);
        s.write_record(Uuid::now_v7(), orphan_task, orphan, true);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(s.storage.has_file(orphan), "the sweep did not wait");
        s.env.hv.release_start();
    });
    received.unwrap();
    let deadline = std::time::Instant::now() + common::WAIT;
    while s.storage.has_file(orphan) {
        assert!(std::time::Instant::now() < deadline, "orphan copy left");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(s.record(orphan_task), None);
    assert!(s.storage.has_file(data));
    assert!(s.record(task).is_some());
}

/// 디스크 기록 잠금을 기다리는 사이 원본이 되돌렸으면(표시를 지움) 잠금 뒤 다시 보고 거절한다(되돌려진
/// 마이그레이션의 볼륨·QEMU를 만들지 않는다).
#[tokio::test]
async fn a_migration_rolled_back_while_waiting_for_the_lock_is_refused() {
    let s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;
    s.env.hv.hold_next_start();
    let first = s.receiver.incoming(s.copy_request(task, data));
    let (first, second) = tokio::join!(first, async {
        // 앞의 받기가 잠금을 쥐고 받는 QEMU를 띄우는 중이다.
        s.env.hv.start_held().await;
        let second = s.receiver.incoming(s.copy_request(task, data));
        tokio::pin!(second);
        let waited = tokio::time::timeout(Duration::from_millis(200), &mut second).await;
        assert!(waited.is_err(), "did not wait for the lock: {waited:?}");
        s.roll_back().await;
        s.env.hv.release_start();
        second.await
    });
    first.unwrap();
    let err = second.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Rejected(m) if m.contains("is not migrating to this node")),
        "{err:?}"
    );
}

/// 원본의 RPC가 끊겨 호출이 버려져도 받기는 끝까지 한다: 만든 디스크의 기록(`created`)과 받는 QEMU가
/// 함께 남아 원본의 되돌리기와 정리가 치운다. 취소도 그렇다.
#[tokio::test]
async fn receiving_and_aborting_finish_when_the_call_is_dropped() {
    let s = setup().await;
    let data = s.local_disk().await;
    let task = Uuid::now_v7();
    s.mark_copying(task, data).await;
    s.env.hv.hold_next_start();
    {
        let receiving = s.receiver.incoming(s.copy_request(task, data));
        tokio::select! {
            r = receiving => panic!("received while held: {r:?}"),
            () = s.env.hv.start_held() => {}
        }
    }
    s.env.hv.release_start();
    let deadline = std::time::Instant::now() + common::WAIT;
    while !s.env.hv.is_incoming(s.vm) {
        assert!(
            std::time::Instant::now() < deadline,
            "receiving QEMU not started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(s.record(task).unwrap()["volumes"][0]["created"], true);

    // 다른 받기가 잠금을 쥔 동안 온 취소가 버려져도 잠금 뒤 끝까지 한다.
    let other = Uuid::now_v7();
    s.mark_copying(other, data).await;
    s.env.hv.hold_next_start();
    let receiving = s.receiver.incoming(s.copy_request(other, data));
    let (received, ()) = tokio::join!(receiving, async {
        s.env.hv.start_held().await;
        s.roll_back().await;
        let aborting = s.receiver.abort(IncomingMigration {
            vm: s.vm,
            task: other,
        });
        let waited = tokio::time::timeout(Duration::from_millis(200), aborting).await;
        assert!(waited.is_err(), "did not wait for the lock: {waited:?}");
        s.env.hv.release_start();
    });
    received.unwrap();
    let deadline = std::time::Instant::now() + common::WAIT;
    while s.record(other).is_some() || s.env.hv.is_incoming(s.vm) {
        assert!(std::time::Instant::now() < deadline, "abort did not finish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!s.storage.has_file(data));
}

/// 받기 전 정리가 같은 VM의 옛 사본(같은 볼륨·풀)을 지우지 못해 그 기록이 남았으면 받지 않는다(옛 기록의
/// 정리가 같은 위치의 새 디스크를 지우지 않게). 다음 받기는 옛 사본을 지우고 받는다.
#[tokio::test]
async fn an_earlier_copy_left_in_place_fails_the_receive() {
    let s = setup().await;
    let data = s.local_disk().await;
    let (old, task) = (Uuid::now_v7(), Uuid::now_v7());
    s.storage.place(data, LOCAL_SIZE);
    s.write_record(s.vm, old, data, true);
    s.mark_copying(task, data).await;
    s.storage.fail_deletes_unmounted(1);
    let err = s
        .receiver
        .incoming(s.copy_request(task, data))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Failed(m)
            if m.contains("an earlier copy of disk \"data\" in pool \"local\"")),
        "{err:?}"
    );
    assert!(s.record(old).is_some());
    assert_eq!(s.record(task), None);
    assert!(!s.env.hv.is_incoming(s.vm));

    s.receiver
        .incoming(s.copy_request(task, data))
        .await
        .unwrap();
    assert_eq!(s.storage.deleted(), [data]);
    assert!(s.storage.has_file(data));
    assert_eq!(s.record(old), None);
    assert!(s.record(task).is_some());
}
