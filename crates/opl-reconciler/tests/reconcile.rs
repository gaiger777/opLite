//! Mock 하이퍼바이저로 조정 규칙(설계 5.3)을 확인한다.

mod common;

use std::time::Duration;

use common::{Env, vm};
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{
    DiskBus, DiskSpec, PowerState, RestartPolicy, ScheduleError, VmPhase, VmSpec, VmStatus,
};
use uuid::Uuid;

fn running(v: &opl_types::vm::Vm) -> bool {
    v.status.phase == VmPhase::Running
}

#[tokio::test]
async fn starts_assigned_vm_and_records_status() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;

    let got = env.wait(vm.id, "running", running).await;
    let pid = env.pid(vm.id).await.expect("VM was not started");
    assert_eq!(
        got.status,
        VmStatus {
            phase: VmPhase::Running,
            node: Some(env.node),
            pid: Some(pid),
            last_error: None,
            snapshot: None,
            schedule_error: None,
        }
    );
}

#[tokio::test]
async fn ignores_vms_not_assigned_to_this_node() {
    let env = Env::start().await;
    let other = vm("other", Uuid::now_v7());
    let mut unplaced = vm("unplaced", env.node);
    unplaced.spec.node = None;
    let mine = vm("mine", env.node);
    for v in [&other, &unplaced, &mine] {
        env.create(v).await;
    }

    // 마지막에 만든 VM까지 처리했으니 앞의 둘은 이미 건너뛰었다.
    env.wait(mine.id, "running", running).await;
    for v in [&other, &unplaced] {
        assert_eq!(env.pid(v.id).await, None, "{} was started", v.name);
        assert_eq!(env.get(v.id).status, VmStatus::default());
    }
}

#[tokio::test]
async fn stop_escalates_to_force_when_guest_ignores_acpi() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    // Mock 게스트는 ACPI 요청을 무시하므로 acpi_timeout(300ms) 뒤 강제 종료로만 꺼진다.
    env.set_power(vm.id, PowerState::Stopped).await;
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.pid, None);
    assert_eq!(env.pid(vm.id).await, None);

    // 다시 켤 수 있다.
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running again", running).await;
    assert!(env.pid(vm.id).await.is_some());
}

#[tokio::test]
async fn acpi_stop_completes_when_guest_shuts_down() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    env.hv.guest_shutdown(vm.id);

    // 강제 종료(60초 뒤)를 기다리지 않고 끝난다. 우리가 요청한 종료이므로 오류가 아니다.
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.last_error, None);
}

#[tokio::test]
async fn cancelled_stop_restarts_vm_when_guest_finishes_shutdown() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    // 게스트가 꺼지기 전에 사용자가 마음을 바꿨다.
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    // 게스트는 앞서 받은 ACPI 요청대로 꺼진다. 게스트가 스스로 끈 것으로 보면 안 된다.
    env.hv.guest_shutdown(vm.id);

    let got = env
        .wait(vm.id, "restarted", |v| running(v) && v.status.pid != before)
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
}

#[tokio::test]
async fn new_stop_after_cancelled_stop_sends_acpi_again() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(2)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    // 종료를 요청했다가 취소했다. 게스트는 ACPI 요청을 무시한다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    tokio::time::sleep(Duration::from_millis(2300)).await;

    // acpi_timeout이 지난 뒤의 새 종료 요청은 앞선 요청을 이어받지 않고 다시 ACPI부터 보낸다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(env.pid(vm.id).await.is_some(), "VM was force-stopped");

    env.hv.guest_shutdown(vm.id);
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.last_error, None);
}

#[tokio::test]
async fn unresponsive_vm_is_forced_off_after_acpi_timeout() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    // QMP로 다룰 수 없으니 ACPI 요청은 실패한다. 그래도 acpi_timeout 뒤 강제 종료한다.
    env.hv.set_unresponsive(vm.id, true);
    env.set_power(vm.id, PowerState::Stopped).await;
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.pid, None);
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn unresponsive_vm_is_reattached_and_reported() {
    // 응답하지 않게 되어도 이벤트는 없으므로 재동기화로 알아챈다.
    let env = Env::start_with(|c| c.resync_interval = Duration::from_millis(200)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    env.hv.set_unresponsive(vm.id, true);
    let got = env
        .wait(vm.id, "reported unresponsive", |v| {
            v.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("unresponsive"))
        })
        .await;
    // 끄거나 다시 띄우지 않는다.
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(env.pid(vm.id).await, pid);

    // 다시 붙으면 오류를 지운다.
    env.hv.set_unresponsive(vm.id, false);
    let got = env
        .wait(vm.id, "recovered", |v| v.status.last_error.is_none())
        .await;
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert_eq!(env.pid(vm.id).await, pid);
}

#[tokio::test]
async fn host_kill_does_not_turn_power_off() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    // 호스트의 SIGTERM처럼 우리가 요청하지 않은, 게스트 밖에서 일어난 종료
    env.hv.stop(vm.id, StopMode::Force).await.unwrap();

    let got = env
        .wait(vm.id, "restarted", |v| running(v) && v.status.pid != before)
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.pid(vm.id).await, got.status.pid);
}

#[tokio::test]
async fn force_stop_does_not_wait_for_acpi() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    // Mock 게스트는 ACPI를 무시한다. 강제 종료가 없으면 60초 뒤에야 꺼진다.
    env.force_stop(vm.id);

    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn force_stop_is_ignored_while_power_is_running() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    env.force_stop(vm.id);
    // 명령이 처리될 시간을 준다.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(env.pid(vm.id).await, before);
    assert_eq!(env.get(vm.id).status.phase, VmPhase::Running);

    // 무시한 명령이 남아 있다가 뒤이은 종료를 강제 종료로 바꾸지 않는다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.get(vm.id).status.phase, VmPhase::Stopping);
    assert_eq!(env.pid(vm.id).await, before, "VM was force-stopped");
}

#[tokio::test]
async fn guest_shutdown_turns_power_off() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.hv.guest_shutdown(vm.id);

    // 다시 켜지 않고 원하는 상태를 꺼짐으로 바꾼다.
    let got = env
        .wait(vm.id, "powered off", |v| {
            v.spec.power == PowerState::Stopped && v.status.phase == VmPhase::Stopped
        })
        .await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.pid(vm.id).await, None);
}

/// 예약 작업이 남긴 오류
fn busy() -> ScheduleError {
    ScheduleError {
        code: "vm_busy".into(),
        message: "vm busy".into(),
    }
}

#[tokio::test]
async fn power_off_keeps_current_snapshot() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    let snapshot = Uuid::now_v7();
    vm.status.snapshot = Some(snapshot);
    vm.status.schedule_error = Some(busy());
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.hv.guest_shutdown(vm.id);

    // 상태를 새로 써도 스냅샷 위치와 예약 오류는 reconciler가 지우지 않는다.
    let got = env
        .wait(vm.id, "powered off", |v| {
            v.spec.power == PowerState::Stopped && v.status.phase == VmPhase::Stopped
        })
        .await;
    assert_eq!(got.status.snapshot, Some(snapshot));
    assert_eq!(got.status.schedule_error, Some(busy()));
}

#[tokio::test]
async fn crash_restarts_vm_with_on_failure_policy() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    vm.spec.restart = RestartPolicy::OnFailure;
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    env.hv.crash(vm.id);

    let got = env
        .wait(vm.id, "restarted", |v| running(v) && v.status.pid != before)
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.pid(vm.id).await, got.status.pid);
}

#[tokio::test]
async fn crash_without_restart_policy_marks_failed() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.hv.crash(vm.id);

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.last_error.as_deref(), Some("VM crashed"));
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn repeated_crashes_give_up() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    vm.spec.restart = RestartPolicy::OnFailure;
    env.create(&vm).await;

    // 하네스 설정은 1분 안에 3번 실패하면 포기한다.
    let mut last = None;
    for _ in 0..3 {
        let got = env
            .wait(vm.id, "running", |v| running(v) && v.status.pid != last)
            .await;
        last = got.status.pid;
        env.hv.crash(vm.id);
    }

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("VM crashed; gave up after 3 failures")
    );
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn permanent_start_failure_marks_failed() {
    let env = Env::start().await;
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));
    let vm = vm("web", env.node);
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to start: QEMU failed to start: no firmware")
    );
}

#[tokio::test]
async fn transient_start_failure_is_retried() {
    let env = Env::start().await;
    env.hv.fail_next_start(DriverError::Timeout("qmp"));
    let vm = vm("web", env.node);
    env.create(&vm).await;

    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(got.spec.power, PowerState::Running);
}

#[tokio::test]
async fn unresolvable_volume_marks_failed() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "missing".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("volume \"missing\" not found")
    );
    assert_eq!(env.pid(vm.id).await, None);
}

/// NFS를 지원하지 않는 노드(macOS)에 놓인 NFS 풀 디스크의 VM은 기다려도 시작할 수 없으므로
/// 다시 시도하지 않고 끄고 이유를 남긴다(노드는 풀을 준비되지 않았다고 알린다).
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn nfs_disk_on_a_node_without_nfs_powers_off() {
    let env = Env::start().await;
    let pool: StoragePool = Resource::new(
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
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "nas".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let mut me: Node = Resource::new(
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
        },
    );
    me.id = env.node;
    let unsupported = PoolState {
        pool_id: pool.id,
        ready: false,
        error: Some("nfs pools are not supported on macOS".into()),
        ..Default::default()
    };
    me.status.pools.insert("nas".into(), unsupported);
    env.cluster.create(&me).await.unwrap();
    let mut vm = vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert!(
        got.status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("not supported")),
        "{:?}",
        got.status.last_error
    );
    assert_eq!(env.pid(vm.id).await, None);
}

/// 풀이 이 노드에서 준비되지 않았으면(NFS 서버가 멈춤 등) 시작을 미루고 다시 시도한다. 실패로 세지
/// 않으므로 오래 걸려도 포기하고 끄지 않는다.
#[tokio::test]
async fn unavailable_pool_is_retried_without_powering_off() {
    let env = Env::start().await;
    let pool: StoragePool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let mut me: Node = Resource::new(
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
        },
    );
    me.id = env.node;
    let down = PoolState {
        pool_id: pool.id,
        ready: false,
        error: Some("pool directory not responding".into()),
        ..Default::default()
    };
    me.status.pools.insert("local".into(), down);
    env.cluster.create(&me).await.unwrap();
    let mut vm = vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "waiting for the pool", |v| {
            v.status.phase == VmPhase::Starting
                && v.status
                    .last_error
                    .as_deref()
                    .is_some_and(|e| e.contains("pool directory not responding"))
        })
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
    // 재시작 포기 횟수(3번)보다 훨씬 많이 다시 시도해도 끄지 않는다.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let got = env.get(vm.id);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(env.pid(vm.id).await, None);

    // 풀이 준비되면 시작한다.
    let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    me.status.pools.get_mut("local").unwrap().ready = true;
    env.cluster
        .update_status::<NodeSpec>(env.node, &me.status)
        .await
        .unwrap();
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
}

#[tokio::test]
async fn restarted_reconciler_keeps_running_vms() {
    let mut env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await;

    env.stop_reconciler().await;
    env.start_reconciler();

    // 새 reconciler가 첫 동기화를 마쳤는지는 새 VM이 뜨는 것으로 확인한다.
    let probe = common::vm("probe", env.node);
    env.create(&probe).await;
    env.wait(probe.id, "probe running", running).await;

    assert_eq!(env.pid(vm.id).await, before.status.pid);
    assert_eq!(env.get(vm.id).status, before.status);
}

/// 정지한 VM을 만들고 reconciler가 처리할 때까지 기다린다.
async fn stopped_vm(env: &Env) -> opl_types::vm::Vm {
    let mut vm = vm("web", env.node);
    vm.spec.power = PowerState::Stopped;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    vm
}

#[tokio::test]
async fn restore_tag_is_used_by_next_start_only() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    env.restore_on_start(vm.id, "snap-1");
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches(), vec![(vm.id, Some("snap-1".into()))]);

    // 한 번 쓰면 사라진다. 다음 시작은 처음부터 부팅한다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running again", running).await;
    assert_eq!(
        env.hv.launches(),
        vec![(vm.id, Some("snap-1".into())), (vm.id, None)]
    );
}

#[tokio::test]
async fn cleared_restore_tag_is_not_used() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    env.restore_on_start(vm.id, "snap-1");
    env.clear_restore(vm.id);
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches(), vec![(vm.id, None)]);
}

#[tokio::test]
async fn failed_restore_marks_failed_and_consumes_tag() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    // 일시 오류라도 다시 시도하지 않는다. 태그 없이 다시 시작하면 처음부터 부팅하게 된다.
    env.hv.fail_next_start(DriverError::Timeout("qmp"));
    env.restore_on_start(vm.id, "snap-1");
    env.set_power(vm.id, PowerState::Running).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to restore snapshot snap-1: timed out waiting for qmp")
    );
    assert_eq!(env.pid(vm.id).await, None);

    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(
        env.hv.launches(),
        vec![(vm.id, Some("snap-1".into())), (vm.id, None)]
    );
}

#[tokio::test]
async fn status_writes_keep_snapshot_position_written_meanwhile() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    // 작업자가 VM을 읽고 시작하는 사이에 Task 실행기가 현재 스냅샷 위치를, 예약 작업이 예약 오류를 쓴다.
    env.hv.delay_starts(Duration::from_millis(300));
    env.set_power(vm.id, PowerState::Running).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let current = env.get(vm.id);
    assert_ne!(current.status.phase, VmPhase::Running);
    let snapshot = Uuid::now_v7();
    let status = VmStatus {
        snapshot: Some(snapshot),
        schedule_error: Some(busy()),
        ..current.status.clone()
    };
    env.cluster
        .update_status_checked::<VmSpec>(vm.id, current.revision, &status)
        .await
        .unwrap();

    // 작업자가 쓴 Running status가 그 값을 덮지 않는다.
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.get(vm.id).status.snapshot, Some(snapshot));
    assert_eq!(env.get(vm.id).status.schedule_error, Some(busy()));
}
