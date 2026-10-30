//! 라이브 마이그레이션과 reconciler: 원본 Task가 다루는 동안의 보류, 확정 전에 남은 마이그레이션의
//! 되돌리기, 받은 VM 넘겨받기(설계 §2).

mod common;

use std::time::Duration;

use common::{Env, vm};
use opl_driver::{Hypervisor, MigrateOptions, MigrateTarget, RuntimeStatus};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{MigrationStatus, Vm, VmPhase, VmSpec};
use uuid::Uuid;

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

/// 원본 Task처럼 `Migrating`과 대상 노드를 기록한다.
async fn mark_migrating(env: &Env, id: Uuid, node: Uuid) {
    let mut status = env.get(id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node,
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(id, &status)
        .await
        .unwrap();
}

fn target() -> MigrateTarget {
    MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    }
}

fn opts() -> MigrateOptions {
    MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    }
}

async fn status_of(env: &Env, id: Uuid) -> Option<RuntimeStatus> {
    env.hv.inspect(id).await.unwrap().map(|s| s.status)
}

#[tokio::test]
async fn held_vm_is_left_alone_until_the_migration_ends() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    let handle = env.handle();
    handle.begin_migration(vm.id).await;
    mark_migrating(&env, vm.id, Uuid::now_v7()).await;
    // 원본이 보낸 뒤 멈췄다. 보류 중이면 작업자가 status를 덮거나 원본을 이어서 실행하지 않는다.
    env.hv.migrate(vm.id, &target(), &opts()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = env.get(vm.id);
    assert_eq!(got.status.phase, VmPhase::Migrating);
    assert!(got.status.migration.is_some());
    assert_eq!(
        status_of(&env, vm.id).await,
        Some(RuntimeStatus::Other("postmigrate".into()))
    );

    // Task 없이 끝나면(Task가 되돌리지 못함) 작업자가 표시를 지우고 원본을 이어서 실행한다.
    handle.end_migration(vm.id);
    let got = env
        .wait(vm.id, "migration cleared", |v| {
            running(v) && v.status.migration.is_none()
        })
        .await;
    assert_eq!(got.spec.node, Some(env.node));
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
}

/// opld가 마이그레이션을 보내다 확정 전에 죽고 다시 떴다: `begin_migration`이 없으므로 작업자가
/// `Migrating`을 되돌리고 `postmigrate`로 멈춘 원본을 이어서 실행한다.
#[tokio::test]
async fn migration_left_by_a_restart_is_rolled_back() {
    let mut env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    env.stop_reconciler().await;
    mark_migrating(&env, vm.id, Uuid::now_v7()).await;
    env.hv.migrate(vm.id, &target(), &opts()).await.unwrap();
    env.start_reconciler();

    let got = env
        .wait(vm.id, "migration rolled back", |v| {
            running(v) && v.status.migration.is_none()
        })
        .await;
    assert_eq!(got.status.pid, pid);
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
}

/// 마이그레이션이 이 노드로 확정됐다: 받는 중 표시가 있는 멈춘 QEMU를 이어서 실행하고 표시를 지운다.
/// 새로 띄우지 않는다.
#[tokio::test]
async fn received_vm_is_continued_after_the_commit() {
    let env = Env::start().await;
    let source = Uuid::now_v7();
    let mut vm = vm("web", source);
    env.create(&vm).await;
    mark_migrating(&env, vm.id, env.node).await;

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
    env.hv
        .start_incoming(&spec, "127.0.0.1", 60000..=60099)
        .await
        .unwrap();
    env.hv.incoming_wait(vm.id).await.unwrap();
    // 확정 전에는 원본 노드의 VM이므로 손대지 않는다.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Paused));
    assert!(env.hv.is_incoming(vm.id));

    // 원본이 확정한다(spec.node만 먼저 바뀌고 status는 아직 원본의 것이다).
    let current = env.get(vm.id);
    vm.spec.node = Some(env.node);
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "taken over", |v| {
            running(v) && v.status.migration.is_none() && v.status.node == Some(env.node)
        })
        .await;
    assert!(got.status.pid.is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.is_incoming(vm.id) {
        assert!(std::time::Instant::now() < deadline, "still incoming");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}
