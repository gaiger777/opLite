//! Mock 하이퍼바이저로 조정 규칙(설계 5.3)을 확인한다.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm};
use opl_driver::{
    Capacity, DriverError, Hypervisor, SourceFormat, StopMode, Storage, VolumeInfo, VolumeSource,
};
use opl_reconciler::ReconcilerConfig;
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
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

/// 멈춘 채 남은 VM(표시가 있는 `pause`)은 디스크를 다시 해석하지 않고 이어서 실행한다(LVM이
/// 응답하지 않아도 된다). 지금 `pause`해 둔 VM은 그대로 두고 응답하지 않는다고 알리지 않는다.
#[tokio::test]
async fn vm_left_paused_continues_without_resolving_disks() {
    let env = Env::start_with(|c| c.resync_interval = Duration::from_millis(100)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;
    // 디스크를 해석할 수 없게 한다(볼륨이 없다).
    let current = env.get(vm.id);
    let mut spec = current.spec.clone();
    spec.disks.push(DiskSpec {
        volume: "missing".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();

    env.hv.pause(vm.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = env.get(vm.id);
    assert_eq!(got.status.last_error, None);
    assert_eq!(got.status.phase, VmPhase::Running);
    assert!(env.hv.is_paused(vm.id));

    env.hv.leave_paused(vm.id);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while env.hv.is_paused(vm.id) {
        assert!(std::time::Instant::now() < deadline, "VM stayed paused");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let got = env.get(vm.id);
    assert_eq!(got.status.last_error, None);
    assert_eq!(got.status.pid, pid);
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

/// 볼륨 켜기·끄기를 적는 저장소. 켜기는 늘 성공한다. reconciler는 켜기·끄기만 부른다.
#[derive(Default)]
struct Activations {
    calls: Mutex<Vec<(&'static str, Uuid)>>,
    /// 켜기 때마다 이 VM의 저장된 phase를 `phases`에 적는다.
    watch: std::sync::OnceLock<(Arc<opl_cluster::Cluster>, Uuid)>,
    phases: Mutex<Vec<VmPhase>>,
}

impl Activations {
    fn deactivated(&self) -> Vec<Uuid> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|(call, _)| *call == "deactivate")
            .map(|&(_, id)| id)
            .collect()
    }
}

#[async_trait]
impl Storage for Activations {
    async fn activate_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("activate", id));
        if let Some((cluster, vm)) = self.watch.get() {
            let vm = cluster.get::<VmSpec>(*vm).unwrap().unwrap();
            self.phases.lock().unwrap().push(vm.status.phase);
        }
        Ok(())
    }
    async fn deactivate_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("deactivate", id));
        Ok(())
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

/// `storage`로 볼륨을 켜고 끄는 reconciler와, dir 풀 볼륨 `root`를 디스크로 쓰는 VM `web`(아직
/// 만들지 않았다)
async fn disk_vm(storage: Arc<Activations>) -> (Env, Volume, opl_types::vm::Vm) {
    disk_vm_with(storage, |_| {}).await
}

/// [`disk_vm`]. 설정을 `tweak`으로 바꾼다.
async fn disk_vm_with(
    storage: Arc<Activations>,
    tweak: impl FnOnce(&mut ReconcilerConfig),
) -> (Env, Volume, opl_types::vm::Vm) {
    let env = Env::start_with(|c| {
        c.storage = storage;
        tweak(c);
    })
    .await;
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
    let root: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&root).await.unwrap();
    let mut vm = vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    (env, root, vm)
}

/// 기다리는 동안 `storage`가 볼륨을 `want`대로 끄기를 기다린다.
async fn wait_deactivated(storage: &Activations, want: &[Uuid]) {
    let deadline = std::time::Instant::now() + common::WAIT;
    while storage.deactivated() != want {
        assert!(
            std::time::Instant::now() < deadline,
            "deactivated {:?}, want {want:?}",
            storage.deactivated()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// VM이 멈추면(사용자가 끔, 크래시) QEMU가 끝난 뒤 디스크 볼륨을 한 번 끈다(공유 LVM의 배타
/// 활성화를 풀어 다른 노드가 쓸 수 있게). 실행 중에는 끄지 않는다.
#[tokio::test]
async fn stopped_vm_deactivates_its_disks() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(*storage.calls.lock().unwrap(), [("activate", root.id)]);

    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    wait_deactivated(&storage, &[root.id]).await;

    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running again", running).await;
    env.hv.crash(vm.id);
    env.wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    wait_deactivated(&storage, &[root.id, root.id]).await;
    // 멈춘 VM을 다시 맞춰도 또 끄지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(storage.deactivated(), [root.id, root.id]);
}

/// 디스크 볼륨의 크기 늘리기 Task가 끝나지 않았으면 디스크를 켜지 않고 시작을 미룬다(실패로 세지
/// 않는다). Task가 끝나면 시작한다.
#[tokio::test]
async fn start_waits_for_a_resize_of_its_disk() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeResize {
                size_bytes: 2 << 30,
            },
            target: root.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    env.create(&vm).await;
    let waiting = env
        .wait(vm.id, "waiting for the resize", |v| {
            v.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("being resized"))
        })
        .await;
    assert_eq!(waiting.status.phase, VmPhase::Starting);
    assert_eq!(waiting.spec.power, PowerState::Running);
    // 여러 번 다시 해 본 뒤에도 포기하지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let still = env.get(vm.id);
    assert_eq!(still.spec.power, PowerState::Running);
    assert_eq!(still.status.phase, VmPhase::Starting);
    assert!(storage.calls.lock().unwrap().is_empty());
    assert!(env.hv.inspect(vm.id).await.unwrap().is_none());

    let status = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
    };
    env.cluster
        .update_status::<TaskSpec>(task.id, &status)
        .await
        .unwrap();
    env.wait(vm.id, "running", running).await;
    assert_eq!(*storage.calls.lock().unwrap(), [("activate", root.id)]);
}

/// 이미 도는 VM에 다시 붙을 때는 크기 늘리기 Task를 기다리지 않고 디스크를 해석한다(QEMU가 이미
/// 장치를 열었다). 기다리면 실행 중 VM의 크기 늘리기 동안 다시 붙지 못한다.
#[tokio::test]
async fn reattach_does_not_wait_for_a_resize() {
    let storage = Arc::new(Activations::default());
    // 응답하지 않는 VM은 재동기화로 알아채고 다시 붙는다.
    let (env, root, vm) = disk_vm_with(storage.clone(), |c| {
        c.resync_interval = Duration::from_millis(100)
    })
    .await;
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeResize {
                size_bytes: 2 << 30,
            },
            target: root.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    // 가짜 하이퍼바이저는 응답하지 않는 VM에 다시 붙지 못한다(디스크를 해석한 뒤 실패한다).
    env.hv.set_unresponsive(vm.id, true);
    let deadline = std::time::Instant::now() + common::WAIT;
    while storage.calls.lock().unwrap().len() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "disks not resolved: {:?}",
            env.get(vm.id).status
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let got = env
        .wait(vm.id, "reported unresponsive", |v| {
            v.status.last_error.is_some()
        })
        .await;
    let error = got.status.last_error.unwrap();
    assert!(!error.contains("being resized"), "{error}");
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert!(
        storage
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|&call| call == ("activate", root.id))
    );
}

/// 돌던 QEMU가 끝나 곧바로(백오프 없이) 다시 켤 때 디스크를 해석하기 전에 phase를 Starting으로
/// 먼저 기록한다. 저장된 phase가 Running인 채면 그사이 API·Task 실행기가 실행 중 VM으로 보고 디스크를
/// 늘린다(공유 LVM은 늘린 부분을 0으로 채우기 전에 QEMU가 연다).
#[tokio::test]
async fn restart_records_starting_before_resolving_disks() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;
    storage
        .watch
        .set((env.cluster.clone(), vm.id))
        .unwrap_or_else(|_| unreachable!());

    // 호스트의 SIGTERM처럼 우리가 요청하지 않은 종료
    env.hv.stop(vm.id, StopMode::Force).await.unwrap();
    let got = env
        .wait(vm.id, "restarted", |v| running(v) && v.status.pid != before)
        .await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(*storage.phases.lock().unwrap(), [VmPhase::Starting]);
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [
            ("activate", root.id),
            ("deactivate", root.id),
            ("activate", root.id)
        ]
    );
}

/// 디스크를 켠 뒤 QEMU가 시작하지 못하면 켠 볼륨을 끈다.
#[tokio::test]
async fn failed_start_deactivates_its_disks() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));
    env.create(&vm).await;
    env.wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    wait_deactivated(&storage, &[root.id]).await;
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [("activate", root.id), ("deactivate", root.id)]
    );
}
