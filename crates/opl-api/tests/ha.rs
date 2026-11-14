//! 리더의 HA 복구(`HaManager`): 단일 노드 클러스터의 저장소에 다른 노드를 직접 기록한다(멤버십에는
//! 없고 리스를 갱신하지 않는다). 리스가 끝난 노드의 HA VM만 이름 순서로 옮기는지, 대상 고르기,
//! status를 spec보다 먼저 쓰는지, 그 노드의 Task를 실패로 끝내고 기록 Task를 남기는지, 대상이 없을
//! 때, 리스가 살아 있는 노드는 옮기지 않는지, 펜싱 표시가 늘 지워지는지, Task 목록을 spec 쓰기 뒤에
//! 읽는지, 결과를 모르는 복구 쓰기 뒤에는 결과를 알 때까지 펜싱을 쥐는지(임기가 바뀌면 풀고 남은 단계를
//! 하는지), 쓰기 오류 뒤에는 펜싱을 푸는지. 배치 규칙과 HA 그룹(그룹 우선순위, restricted, affinity·
//! anti-affinity, 막는 hard 규칙과 `ha_may_violate`), `state: stopped`·`ignored`, 그룹의 더 우선인 노드로
//! 되돌아가기, `rule_violations` 갱신. 호스트 종료 정책으로 얼린 노드는 풀기 전까지 옮기지 않는다.
//! 우선순위 순서, 대상 노드마다의 동시 시작 상한(`ha_start_concurrency`), 미리 고른 복구 노드.
//! 전원 끄기 펜싱(가짜 `PowerFence`): `fence_after`가 지난 노드를 한 번 끄고 꺼짐을 확인하면 `recover_after` 전에
//! 복구하는지, 끄지 못하면 `recover_after`를 기다리는지, 끄지 않을 노드, 요청받은 끄기, 기록 Task.
//! HA 상태 검사(리스 갱신에 실은 결과): grace 뒤 경고, 과반 실패면 경고만, `migrate`·`fence` 대응, 자동 관리 검사,
//! 옛 노드가 있으면 대응하지 않음, 검사가 실패한 노드로 옮기지 않는 대상 거르기. `restart_elsewhere`: 끄기 요청
//! (`ha.evict`), 노드가 거둔 뒤 옮기기, `max_relocate`, 거두지 않는 노드의 전원 끄기·경고.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, WAIT, vm_request};
use opl_api::{HaManager, HaManagerConfig, LEASE_EXPIRED, PowerFence, RecoveryWriter};
use opl_cluster::types::{Command, LeaderRequest, NodeId, Rejection};
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_types::fence::{CommandFence, FenceKind, FenceMethod, NodeFenceSpec, Power};
use opl_types::ha_check::{
    CheckReport, CheckState, HaCheckAction, HaCheckRole, HaCheckSpec, HaCheckTarget,
};
use opl_types::network::NetworkSpec;
use opl_types::node::{
    BridgeInfo, FEATURES, Fencing, InterfaceKind, Node, NodeHa, NodeInterface, NodeShutdown,
    NodeSpec, NodeStatus, ShutdownAction,
};
use opl_types::placement::{
    HaGroupNode, HaGroupSpec, HaGroupStatus, PlacementRuleSpec, PlacementRuleStatus, RuleKind,
};
use opl_types::resource::{Kind, Resource};
use opl_types::runtime::ImageFormat;
use opl_types::settings::{SETTINGS_NAME, Settings, SettingsSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{
    Arch, HaState, MigrationStatus, PowerState, Vm, VmHa, VmHaStatus, VmPhase, VmSpec, VmStatus,
};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

fn fast_manager(env: &Env) -> HaManager {
    HaManager::spawn(
        env.state.clone(),
        HaManagerConfig {
            tick: Duration::from_millis(100),
            ..Default::default()
        },
    )
}

fn manager_with(env: &Env, writer: Arc<dyn RecoveryWriter>) -> HaManager {
    HaManager::spawn_with_writer(
        env.state.clone(),
        HaManagerConfig {
            tick: Duration::from_millis(100),
            ..Default::default()
        },
        writer,
    )
}

/// 되돌아가기 전에 노드가 `failback_after` 동안 살아 있어야 하는 관리자
fn failback_manager(env: &Env, failback_after: Duration) -> HaManager {
    HaManager::spawn(
        env.state.clone(),
        HaManagerConfig {
            tick: Duration::from_millis(100),
            failback_after,
            ..Default::default()
        },
    )
}

/// 리더로서 쓴다(`HaManager`의 쓰기 경로).
async fn leader_write(cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
    cluster
        .execute_as_leader(LeaderRequest::Write { command })
        .await
}

/// 노드 `name`을 저장소에 직접 만든다. `recover_after`가 있으면 HA 가능 노드다.
async fn node(env: &Env, name: &str, memory_mib: u64, recover_after: Option<u64>) -> Uuid {
    node_with(env, name, memory_mib, recover_after, |_| {}).await
}

async fn node_with(
    env: &Env,
    name: &str,
    memory_mib: u64,
    recover_after: Option<u64>,
    tweak: impl FnOnce(&mut Resource<NodeSpec, NodeStatus>),
) -> Uuid {
    let mut node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.status = NodeStatus {
        online: true,
        cpus: 4,
        memory_mib,
        hypervisor: "mock".into(),
        bridges: vec![],
        arch: Some(Arch::Aarch64),
        pools: Default::default(),
        ha: recover_after.map(|secs| NodeHa {
            fencing: Fencing::Process,
            recover_after_secs: secs,
            fence_after_secs: None,
        }),
        underlay: None,
        firewall: false,
        version: None,
        updates: None,
        features: vec![],
        cluster_addresses: vec![],
        shutdown: None,
        fence_check: None,
        interfaces: vec![],
        checks: vec![],
    };
    tweak(&mut node);
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 노드 `node`에 배정된 VM을 직접 만든다. 실행 중이면 status도 그 노드에서 도는 것으로 둔다.
async fn vm(env: &Env, name: &str, node: Uuid, ha: bool, power: PowerState) -> Vm {
    vm_ha(env, name, node, ha.then(VmHa::default), power).await
}

/// [`vm`]과 같되 HA 정책을 준다.
async fn vm_ha(env: &Env, name: &str, node: Uuid, ha: Option<VmHa>, power: PowerState) -> Vm {
    vm_with(env, name, node, ha, power, |_| {}).await
}

/// [`vm_ha`]와 같되 `tweak`로 spec(디스크·NIC)을 바꾼다.
async fn vm_with(
    env: &Env,
    name: &str,
    node: Uuid,
    ha: Option<VmHa>,
    power: PowerState,
    tweak: impl FnOnce(&mut VmSpec),
) -> Vm {
    let mut spec: VmSpec = serde_json::from_value(vm_request(name)["spec"].clone()).unwrap();
    spec.memory_mib = 2048;
    spec.nics.clear();
    spec.node = Some(node);
    spec.ha = ha;
    spec.power = power;
    tweak(&mut spec);
    let mut vm: Vm = Resource::new(name, spec);
    if power == PowerState::Running {
        vm.status = VmStatus {
            phase: VmPhase::Running,
            node: Some(node),
            pid: Some(4242),
            firmware_node: Some(node),
            ..Default::default()
        };
    }
    vm.revision = env.cluster.create(&vm).await.unwrap();
    vm
}

fn get(env: &Env, id: Uuid) -> Vm {
    env.cluster.get::<VmSpec>(id).unwrap().unwrap()
}

/// 이 노드의 이름으로 리더에게 리스를 갱신한다.
async fn renew(cluster: &Cluster, node: Uuid) -> Result<u64, ClusterError> {
    cluster
        .execute_as_leader(LeaderRequest::RenewLease {
            node: NodeId(node),
            checks: vec![],
        })
        .await
}

/// 노드 `node`에서 VM `target`을 켜는 끝나지 않은 Task를 만든다.
async fn pending_task(cluster: &Cluster, target: Uuid, node: Uuid) -> Uuid {
    let mut t: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmStart,
            target,
            node,
        },
    );
    t.name = t.id.to_string();
    cluster.create(&t).await.unwrap();
    t.id
}

/// 기록 Task들(대상 VM별)
fn recover_tasks(env: &Env) -> Vec<Task> {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    tasks
        .into_iter()
        .filter(|t| matches!(t.spec.op, TaskOp::VmHaRecover { .. }))
        .collect()
}

async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !ok() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn recovers_ha_vms_of_a_failed_node_in_name_order() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    // 대상 후보: 메모리가 많은 쪽부터 고른다. 옮긴 VM의 메모리를 세어 두 번째 VM은 다른 노드로 간다.
    let small = node(&env, "small", 4096, Some(60)).await;
    let big = node(&env, "big", 5000, Some(60)).await;
    // 후보가 아닌 노드: HA 불가, 리스가 끝남, 유지보수, 위트니스, 마이그레이션 대상
    node(&env, "no-ha", 64000, None).await;
    node(&env, "expired", 64000, Some(1)).await;
    node_with(&env, "maint", 64000, Some(60), |n| {
        n.spec.maintenance = true
    })
    .await;
    node_with(&env, "wit", 64000, Some(60), |n| n.spec.witness = true).await;
    let receiving = node(&env, "receiving", 4500, Some(60)).await;

    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", failed, true, PowerState::Running).await;
    let off = vm(&env, "off", failed, true, PowerState::Stopped).await;
    let plain = vm(&env, "plain", failed, false, PowerState::Running).await;
    // b-vm은 receiving으로 마이그레이션하던 중이었다.
    let mut status = b.status.clone();
    status.migration = Some(MigrationStatus {
        node: receiving,
        task: Uuid::now_v7(),
        disks: vec![],
    });
    env.cluster
        .update_status::<VmSpec>(b.id, &status)
        .await
        .unwrap();
    // 죽은 노드의 끝나지 않은 Task(대상 VM의 것만 실패로 끝낸다)와 다른 노드의 Task
    let mut tasks = vec![];
    for (target, on, state) in [
        (a.id, failed, TaskState::Pending),
        (b.id, failed, TaskState::Running),
        (plain.id, failed, TaskState::Pending),
        (a.id, small, TaskState::Pending),
    ] {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op: TaskOp::VmStart,
                target,
                node: on,
            },
        );
        t.name = t.id.to_string();
        t.status.state = state;
        env.cluster.create(&t).await.unwrap();
        tasks.push(t.id);
    }

    let (since, _) = env.cluster.list::<VmSpec>().unwrap();
    let mut watch = env.cluster.watch(since).unwrap();
    let manager = fast_manager(&env);
    eventually("HA VMs moved", || {
        get(&env, a.id).spec.node != Some(failed) && get(&env, b.id).spec.node != Some(failed)
    })
    .await;
    eventually("record tasks", || recover_tasks(&env).len() == 2).await;

    // 이름 순서: a-vm이 먼저 메모리가 많은 big으로, 그 뒤 b-vm은 남은 메모리가 많은 small로 간다
    // (receiving은 b-vm의 마이그레이션 대상이라 빠진다).
    let (a_now, b_now) = (get(&env, a.id), get(&env, b.id));
    assert_eq!(a_now.spec.node, Some(big));
    assert_eq!(b_now.spec.node, Some(small));
    for vm in [&a_now, &b_now] {
        assert_eq!(vm.spec.power, PowerState::Running);
        assert_eq!(vm.status.phase, VmPhase::Stopped);
        assert_eq!(vm.status.node, None);
        assert_eq!(vm.status.pid, None);
        assert_eq!(vm.status.migration, None);
        assert_eq!(vm.status.firmware_node, Some(failed));
        assert_eq!(
            vm.status.last_error.as_deref(),
            Some("recovering: node failed failed")
        );
    }
    // HA가 아니거나 꺼진 VM은 그대로다.
    assert_eq!(get(&env, off.id).spec.node, Some(failed));
    assert_eq!(get(&env, plain.id), plain);

    // 죽은 노드의 대상 VM Task만 실패로 끝난다.
    let task = |id: Uuid| env.cluster.get::<TaskSpec>(id).unwrap().unwrap();
    for id in &tasks[..2] {
        assert_eq!(
            task(*id).status,
            TaskStatus {
                state: TaskState::Failed,
                error: Some("node failed failed; the VM was recovered by HA".into()),
                progress: None,
            }
        );
    }
    assert_eq!(task(tasks[2]).status.state, TaskState::Pending);
    assert_eq!(task(tasks[3]).status.state, TaskState::Pending);

    // 기록 Task: 대상 VM, 받은 노드, 처음부터 Succeeded
    let mut records: Vec<(Uuid, Uuid, TaskOp, TaskStatus)> = recover_tasks(&env)
        .into_iter()
        .map(|t| (t.spec.target, t.spec.node, t.spec.op, t.status))
        .collect();
    records.sort_by_key(|r| (r.0 != a.id, r.0));
    let done = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
        progress: None,
    };
    let op = TaskOp::VmHaRecover { from: failed };
    assert_eq!(
        records,
        vec![
            (a.id, big, op.clone(), done.clone()),
            (b.id, small, op, done)
        ]
    );

    // VM마다 status(아직 죽은 노드에 배정)를 쓴 뒤에 spec을 바꾼다.
    for id in [a.id, b.id] {
        let mut writes = vec![];
        while writes.len() < 2 {
            let event = tokio::time::timeout(WAIT, watch.next())
                .await
                .unwrap()
                .unwrap();
            if let WatchEvent::Put {
                kind: Kind::Vm,
                resource,
            } = event
                && resource.id == id
            {
                writes.push(resource);
            }
        }
        assert_eq!(writes[0].spec["node"], failed.to_string());
        assert_eq!(writes[0].status["phase"], "stopped");
        assert_ne!(writes[1].spec["node"], failed.to_string());
    }

    // 펜싱 표시가 지워져 죽은 노드가 다시 리스를 받을 수 있다.
    renew(&env.cluster, failed).await.unwrap();
    manager.shutdown().await;
}

#[tokio::test]
async fn leaves_vms_of_a_node_whose_lease_is_alive() {
    let env = Env::start().await;
    let alive = node(&env, "alive", 8192, Some(1)).await;
    node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", alive, true, PowerState::Running).await;
    // 갱신 결과는 채널로 받아 단언한다(뒤에서 조용히 죽지 않게).
    let (results, mut renewed) = tokio::sync::mpsc::unbounded_channel();
    let renewer = {
        let cluster = env.cluster.clone();
        tokio::spawn(async move {
            loop {
                if results.send(renew(&cluster, alive).await).is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    };
    let manager = fast_manager(&env);
    // recover_after(1초)를 여러 번 넘기도록 기다린다.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(get(&env, a.id), a);
    assert!(recover_tasks(&env).is_empty());
    manager.shutdown().await;
    renewer.abort();
    let mut count = 0;
    while let Ok(result) = renewed.try_recv() {
        result.unwrap();
        count += 1;
    }
    assert!(count >= 10, "renewed {count} times");
}

#[tokio::test]
async fn records_the_error_once_when_no_node_can_take_the_vm() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    // 유일한 다른 HA 노드도 리스가 끝났다.
    node(&env, "expired", 8192, Some(1)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    eventually("error recorded", || {
        get(&env, a.id).status.last_error.as_deref() == Some("HA cannot find a node for this VM")
    })
    .await;
    let recorded = get(&env, a.id);
    assert_eq!(recorded.spec, a.spec);
    assert_eq!(
        recorded.status,
        VmStatus {
            last_error: Some("HA cannot find a node for this VM".into()),
            ..a.status.clone()
        }
    );
    // 같은 오류는 다시 쓰지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(get(&env, a.id).revision, recorded.revision);
    assert!(recover_tasks(&env).is_empty());
    // 대상이 없어도 펜싱 표시는 지운다.
    renew(&env.cluster, failed).await.unwrap();
    manager.shutdown().await;
}

#[tokio::test]
async fn a_node_is_not_fenced_before_recover_after_from_becoming_leader() {
    // 리더가 된 뒤(기준점)부터 recover_after가 지나야 펜싱한다. 그 전에는 갱신이 거부되지 않는다.
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(3)).await;
    node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(get(&env, a.id), a);
    eventually("HA VM moved", || get(&env, a.id).spec.node != Some(failed)).await;
    manager.shutdown().await;
}

#[tokio::test]
async fn the_task_runner_does_not_run_a_record_task() {
    let env = Env::start().await;
    let a = vm(&env, "a-vm", env.node, true, PowerState::Stopped).await;
    let mut t: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmHaRecover {
                from: Uuid::now_v7(),
            },
            target: a.id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    t.status.state = TaskState::Succeeded;
    let revision = env.cluster.create(&t).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let now = env.cluster.get::<TaskSpec>(t.id).unwrap().unwrap();
    assert_eq!(now.revision, revision);
    assert_eq!(now.status.state, TaskState::Succeeded);
}

/// spec 쓰기 바로 앞에 죽은 노드의 그 VM Task를 하나 만든다.
struct TaskBeforeSpec {
    failed: Uuid,
    created: OnceLock<Uuid>,
}

#[async_trait]
impl RecoveryWriter for TaskBeforeSpec {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        if let Command::UpdateSpec { id, .. } = &command
            && self.created.get().is_none()
        {
            let task = pending_task(cluster, *id, self.failed).await;
            self.created.set(task).unwrap();
        }
        leader_write(cluster, command).await
    }
}

#[tokio::test]
async fn fails_a_task_of_the_failed_node_created_before_the_spec_write() {
    // Task 목록은 spec 쓰기 뒤에 읽는다. 그 전에 읽으면 그 사이에 만든 Task가 남아 VM 조작이 막힌다.
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let writer = Arc::new(TaskBeforeSpec {
        failed,
        created: OnceLock::new(),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("record task", || recover_tasks(&env).len() == 1).await;
    assert_ne!(get(&env, a.id).spec.node, Some(failed));
    let task = *writer.created.get().unwrap();
    assert_eq!(
        env.cluster.get::<TaskSpec>(task).unwrap().unwrap().status,
        TaskStatus {
            state: TaskState::Failed,
            error: Some("node failed failed; the VM was recovered by HA".into()),
            progress: None,
        }
    );
    manager.shutdown().await;
}

/// `pick`이 처음 고른 쓰기의 결과를 모르게 한다(`applied`면 적용한 뒤 `Timeout`, 아니면 적용하지 않고
/// `Timeout`). 그 뒤의 쓰기는 `release`까지 적용하지 않고 `Timeout`이다.
struct Unsure {
    pick: fn(&Command) -> bool,
    applied: bool,
    hit: AtomicBool,
    resent: AtomicUsize,
    release: AtomicBool,
}

#[async_trait]
impl RecoveryWriter for Unsure {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        if self.release.load(Ordering::SeqCst) {
            return leader_write(cluster, command).await;
        }
        if self.hit.load(Ordering::SeqCst) {
            self.resent.fetch_add(1, Ordering::SeqCst);
            return Err(ClusterError::Timeout);
        }
        if (self.pick)(&command) {
            if self.applied {
                leader_write(cluster, command).await?;
            }
            self.hit.store(true, Ordering::SeqCst);
            return Err(ClusterError::Timeout);
        }
        leader_write(cluster, command).await
    }
}

fn is_status_write(command: &Command) -> bool {
    matches!(command, Command::UpdateStatusIf { kind: Kind::Vm, .. })
}

fn is_spec_write(command: &Command) -> bool {
    matches!(command, Command::UpdateSpec { kind: Kind::Vm, .. })
}

fn is_fenced(result: &Result<u64, ClusterError>) -> bool {
    matches!(
        result,
        Err(ClusterError::Rejected(Rejection::Fenced { .. }))
    )
}

/// 결과가 나올 때까지 갱신을 다시 시도한다(펜싱이 풀리기를 기다린다).
async fn renew_eventually(cluster: &Cluster, node: Uuid) {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        match renew(cluster, node).await {
            Ok(_) => return,
            Err(e) => assert!(std::time::Instant::now() < deadline, "renew: {e}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 고른 복구 쓰기가 `Timeout`으로 끝나면 결과를 알 때까지 죽은 노드의 갱신을 거부하고, 결과를 안 뒤
/// 남은 단계를 이어 한다.
async fn holds_the_fence_until_an_unknown_write_settles(
    pick: fn(&Command) -> bool,
    applied: bool,
    moved_meanwhile: bool,
) {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    let spare = node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let task = pending_task(&env.cluster, a.id, failed).await;
    let writer = Arc::new(Unsure {
        pick,
        applied,
        hit: AtomicBool::new(false),
        resent: AtomicUsize::new(0),
        release: AtomicBool::new(false),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("write resent", || writer.resent.load(Ordering::SeqCst) >= 2).await;
    // 결과를 모르는 동안(같은 임기) 펜싱 표시를 지우지 않는다.
    for _ in 0..5 {
        let result = renew(&env.cluster, failed).await;
        assert!(is_fenced(&result), "renew while unknown: {result:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let expected = if moved_meanwhile { spare } else { failed };
    assert_eq!(get(&env, a.id).spec.node, Some(expected));
    assert!(recover_tasks(&env).is_empty());

    // 다시 보낸 쓰기가 결과를 내면 남은 단계를 하고 펜싱을 푼다.
    writer.release.store(true, Ordering::SeqCst);
    eventually("record task", || recover_tasks(&env).len() == 1).await;
    renew_eventually(&env.cluster, failed).await;
    let moved = get(&env, a.id);
    assert_eq!(moved.spec.node, Some(spare));
    assert_eq!(moved.status.phase, VmPhase::Stopped);
    assert_eq!(
        moved.status.last_error.as_deref(),
        Some("recovering: node failed failed")
    );
    assert_eq!(
        env.cluster
            .get::<TaskSpec>(task)
            .unwrap()
            .unwrap()
            .status
            .state,
        TaskState::Failed
    );
    let records = recover_tasks(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].spec.node, spare);
    manager.shutdown().await;
}

#[tokio::test]
async fn holds_the_fence_after_an_applied_status_write_times_out() {
    holds_the_fence_until_an_unknown_write_settles(is_status_write, true, false).await;
}

#[tokio::test]
async fn holds_the_fence_after_a_dropped_status_write_times_out() {
    holds_the_fence_until_an_unknown_write_settles(is_status_write, false, false).await;
}

#[tokio::test]
async fn holds_the_fence_after_an_applied_spec_write_times_out() {
    holds_the_fence_until_an_unknown_write_settles(is_spec_write, true, true).await;
}

#[tokio::test]
async fn holds_the_fence_after_a_dropped_spec_write_times_out() {
    // 적용되지 않은 spec 쓰기: 다시 보낸 쓰기가 Ok로 적용된 뒤 남은 단계를 한다.
    holds_the_fence_until_an_unknown_write_settles(is_spec_write, false, false).await;
}

/// 처음 spec 쓰기는 적용한 뒤 `Timeout`, 그 뒤의 spec 쓰기는 적용하지 않고 `Timeout`이다. 다른 쓰기는
/// 그대로 한다. 결과를 모르는 쓰기를 판정하는 임기는 실제 임기에 `bump`를 더한 것이다(단일 노드에서는
/// 임기를 바꿀 수 없어 임기 변경을 흉내 낸다).
struct SpecUnknownThenNewTerm {
    hit: AtomicBool,
    resent: AtomicUsize,
    bump: AtomicU64,
}

#[async_trait]
impl RecoveryWriter for SpecUnknownThenNewTerm {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        if !is_spec_write(&command) {
            return leader_write(cluster, command).await;
        }
        if self.hit.swap(true, Ordering::SeqCst) {
            self.resent.fetch_add(1, Ordering::SeqCst);
        } else {
            leader_write(cluster, command).await?;
        }
        Err(ClusterError::Timeout)
    }

    fn term(&self, cluster: &Cluster) -> u64 {
        cluster.leases().term + self.bump.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn finishes_a_spec_write_settled_by_a_new_term() {
    // 결과를 모르는 spec 쓰기가 임기 변경으로 정리되면 펜싱을 풀고, 다시 리더일 때 VM이 그 노드로 가
    // 있으면 남은 단계(Task 실패, 기록 Task)를 한 번 한다.
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    let spare = node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let task = pending_task(&env.cluster, a.id, failed).await;
    let writer = Arc::new(SpecUnknownThenNewTerm {
        hit: AtomicBool::new(false),
        resent: AtomicUsize::new(0),
        bump: AtomicU64::new(0),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("spec write resent", || {
        writer.resent.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert!(is_fenced(&renew(&env.cluster, failed).await));
    assert_eq!(get(&env, a.id).spec.node, Some(spare));
    assert!(recover_tasks(&env).is_empty());

    writer.bump.store(1, Ordering::SeqCst);
    eventually("record task", || recover_tasks(&env).len() == 1).await;
    renew_eventually(&env.cluster, failed).await;
    let records = recover_tasks(&env);
    assert_eq!(records[0].spec.target, a.id);
    assert_eq!(records[0].spec.node, spare);
    assert_eq!(
        env.cluster.get::<TaskSpec>(task).unwrap().unwrap().status,
        TaskStatus {
            state: TaskState::Failed,
            error: Some("node failed failed; the VM was recovered by HA".into()),
            progress: None,
        }
    );
    // 한 번만 한다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(recover_tasks(&env).len(), 1);
    manager.shutdown().await;
}

/// 처음 쓰기를 끝내지 않는다.
struct Stuck {
    started: AtomicBool,
}

#[async_trait]
impl RecoveryWriter for Stuck {
    async fn write(&self, _: &Cluster, _: Command) -> Result<u64, ClusterError> {
        self.started.store(true, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[tokio::test]
async fn keeps_the_fence_when_stopped_during_a_write() {
    // 쓰는 중에 멈추면 그 쓰기가 나중에 커밋될 수 있으므로 펜싱 표시를 지우지 않는다.
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    node(&env, "spare", 8192, Some(60)).await;
    vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let writer = Arc::new(Stuck {
        started: AtomicBool::new(false),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("write started", || writer.started.load(Ordering::SeqCst)).await;
    manager.shutdown().await;
    let result = renew(&env.cluster, failed).await;
    assert!(is_fenced(&result), "renew after stop: {result:?}");
}

/// 모든 쓰기가 `error`로 끝난다(적용하지 않는다).
struct Failing {
    error: fn() -> ClusterError,
    writes: AtomicUsize,
}

#[async_trait]
impl RecoveryWriter for Failing {
    async fn write(&self, _: &Cluster, _: Command) -> Result<u64, ClusterError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err((self.error)())
    }
}

/// 복구 쓰기가 결과를 아는 오류로 끝나면 펜싱 표시를 지워 그 노드가 다시 리스를 받을 수 있다.
async fn ends_the_fence_after_a_write_error(error: fn() -> ClusterError) {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let writer = Arc::new(Failing {
        error,
        writes: AtomicUsize::new(0),
    });
    let manager = manager_with(&env, writer.clone());
    eventually("write tried", || writer.writes.load(Ordering::SeqCst) >= 1).await;
    renew_eventually(&env.cluster, failed).await;
    assert_eq!(get(&env, a.id), a);
    manager.shutdown().await;
}

#[tokio::test]
async fn ends_the_fence_after_losing_leadership_during_a_write() {
    ends_the_fence_after_a_write_error(|| ClusterError::NotLeader {
        leader_id: None,
        leader_addr: None,
    })
    .await;
}

#[tokio::test]
async fn ends_the_fence_after_a_rejected_write() {
    ends_the_fence_after_a_write_error(|| {
        ClusterError::Rejected(Rejection::RevisionMismatch {
            expected: 1,
            actual: 2,
        })
    })
    .await;
}

/// HA 그룹을 저장소에 직접 만든다(노드 이름과 우선순위).
async fn group(env: &Env, name: &str, nodes: &[(&str, u32)], restricted: bool, nofailback: bool) {
    let g: Resource<HaGroupSpec, HaGroupStatus> = Resource::new(
        name,
        HaGroupSpec {
            nodes: nodes
                .iter()
                .map(|(node, priority)| HaGroupNode {
                    node: node.to_string(),
                    priority: *priority,
                })
                .collect(),
            restricted,
            nofailback,
        },
    );
    env.cluster.create(&g).await.unwrap();
}

/// 배치 규칙을 저장소에 직접 만든다(VM 이름과 노드 이름).
async fn rule(env: &Env, name: &str, kind: RuleKind, vms: &[&str], nodes: &[&str]) -> Uuid {
    rule_with(env, name, kind, vms, nodes, |_| {}).await
}

async fn rule_with(
    env: &Env,
    name: &str,
    kind: RuleKind,
    vms: &[&str],
    nodes: &[&str],
    tweak: impl FnOnce(&mut PlacementRuleSpec),
) -> Uuid {
    let mut spec = PlacementRuleSpec {
        kind,
        vms: vms.iter().map(|v| v.to_string()).collect(),
        vm_tag: None,
        nodes: nodes.iter().map(|n| n.to_string()).collect(),
        node_tag: None,
        weight: None,
        enabled: true,
        ha_may_violate: false,
    };
    tweak(&mut spec);
    let r: Resource<PlacementRuleSpec, PlacementRuleStatus> = Resource::new(name, spec);
    env.cluster.create(&r).await.unwrap();
    r.id
}

/// 그룹 `name`의 기본 HA 정책
fn in_group(name: &str) -> Option<VmHa> {
    Some(VmHa {
        group: Some(name.into()),
        ..Default::default()
    })
}

fn with_state(state: HaState) -> Option<VmHa> {
    Some(VmHa {
        state,
        ..Default::default()
    })
}

/// 멈출 때까지 이 노드의 리스를 갱신한다(펜싱 중이면 거부돼도 계속한다).
fn keep_alive(env: &Env, node: Uuid) -> tokio::task::JoinHandle<()> {
    let cluster = env.cluster.clone();
    tokio::spawn(async move {
        loop {
            let _ = renew(&cluster, node).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
}

/// 실행하기로 된 마이그레이션 Task들(대상 VM, 원본 노드, 대상 노드)
fn migrate_tasks(env: &Env) -> Vec<(Uuid, Uuid, Uuid)> {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    tasks
        .into_iter()
        .filter_map(|t| match t.spec.op {
            TaskOp::VmMigrate { node, .. } => Some((t.spec.target, t.spec.node, node)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn recovers_to_the_highest_priority_node_of_the_group() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    // 메모리로는 low·other가 낫지만 그룹 우선순위가 먼저다. 그룹이 없는 VM은 메모리로 고른다.
    node(&env, "low", 16000, Some(60)).await;
    let high = node(&env, "high", 4096, Some(60)).await;
    let other = node(&env, "other", 64000, Some(60)).await;
    group(
        &env,
        "g",
        &[("failed", 10), ("low", 1), ("high", 5)],
        false,
        false,
    )
    .await;
    let a = vm_ha(&env, "a-vm", failed, in_group("g"), PowerState::Running).await;
    let b = vm(&env, "b-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    eventually("HA VMs moved", || recover_tasks(&env).len() == 2).await;
    assert_eq!(get(&env, a.id).spec.node, Some(high));
    assert_eq!(get(&env, b.id).spec.node, Some(other));
    manager.shutdown().await;
}

#[tokio::test]
async fn a_restricted_group_waits_for_its_own_node() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    // 그룹의 다른 노드는 리스가 끝났고, 그룹 밖 노드만 살아 있다.
    let member = node(&env, "member", 8192, Some(1)).await;
    node(&env, "outside", 64000, Some(60)).await;
    group(&env, "g", &[("failed", 2), ("member", 1)], true, false).await;
    let a = vm_ha(&env, "a-vm", failed, in_group("g"), PowerState::Running).await;
    let manager = fast_manager(&env);
    let error = "HA cannot place this VM: placement rule ha-group:g";
    eventually("error recorded", || {
        get(&env, a.id).status.last_error.as_deref() == Some(error)
    })
    .await;
    let blocked = get(&env, a.id);
    assert_eq!(blocked.spec, a.spec);
    assert_eq!(
        blocked.status.ha.as_ref().and_then(|h| h.error.as_deref()),
        Some(error)
    );
    // 같은 오류는 다시 쓰지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(get(&env, a.id).revision, blocked.revision);
    assert!(recover_tasks(&env).is_empty());

    // 그룹 노드가 살아나면 그리로 옮기고 오류를 지운다.
    let renewer = keep_alive(&env, member);
    eventually("HA VM moved", || get(&env, a.id).spec.node == Some(member)).await;
    let moved = get(&env, a.id);
    assert_eq!(moved.spec.power, PowerState::Running);
    assert_eq!(moved.status.ha, None);
    assert_eq!(
        moved.status.last_error.as_deref(),
        Some("recovering: node failed failed")
    );
    renewer.abort();
    manager.shutdown().await;
}

#[tokio::test]
async fn anti_affinity_recovers_two_vms_to_different_nodes() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    // 규칙이 없으면 둘 다 메모리가 많은 n1로 간다.
    let n1 = node(&env, "n1", 16000, Some(60)).await;
    let n2 = node(&env, "n2", 8000, Some(60)).await;
    rule(
        &env,
        "apart",
        RuleKind::AntiAffinity,
        &["a-vm", "b-vm"],
        &[],
    )
    .await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    eventually("HA VMs moved", || recover_tasks(&env).len() == 2).await;
    assert_eq!(get(&env, a.id).spec.node, Some(n1));
    assert_eq!(get(&env, b.id).spec.node, Some(n2));
    manager.shutdown().await;
}

#[tokio::test]
async fn affinity_recovers_two_vms_to_the_same_node() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    // 규칙이 없으면 a-vm은 n1, b-vm은 남은 메모리가 많은 n2로 간다.
    let n1 = node(&env, "n1", 5000, Some(60)).await;
    node(&env, "n2", 4500, Some(60)).await;
    rule(&env, "together", RuleKind::Affinity, &["a-vm", "b-vm"], &[]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    eventually("HA VMs moved", || recover_tasks(&env).len() == 2).await;
    assert_eq!(get(&env, a.id).spec.node, Some(n1));
    assert_eq!(get(&env, b.id).spec.node, Some(n1));
    manager.shutdown().await;
}

#[tokio::test]
async fn a_hard_rule_blocks_recovery_unless_it_may_be_violated() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    let spare = node(&env, "spare", 64000, Some(60)).await;
    rule(
        &env,
        "keep-off",
        RuleKind::NodeAntiAffinity,
        &["a-vm"],
        &["spare"],
    )
    .await;
    rule_with(
        &env,
        "may",
        RuleKind::NodeAntiAffinity,
        &["b-vm"],
        &["spare"],
        |r| r.ha_may_violate = true,
    )
    .await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    // 어겨도 되는 규칙만 막으면 어기고 켜며 위반으로 보인다.
    eventually("b-vm moved with a violation", || {
        let b = get(&env, b.id);
        b.spec.node == Some(spare) && b.status.rule_violations == ["may"]
    })
    .await;
    assert_eq!(get(&env, b.id).spec.power, PowerState::Running);
    // 막는 규칙이 있으면 켜지 않고 이유를 남긴다.
    let error = "HA cannot place this VM: placement rule keep-off";
    eventually("a-vm blocked", || {
        get(&env, a.id).status.last_error.as_deref() == Some(error)
    })
    .await;
    let blocked = get(&env, a.id);
    assert_eq!(blocked.spec, a.spec);
    assert_eq!(
        blocked.status.ha.as_ref().and_then(|h| h.error.as_deref()),
        Some(error)
    );
    assert_eq!(recover_tasks(&env).len(), 1);
    manager.shutdown().await;
}

#[tokio::test]
async fn a_stopped_ha_vm_moves_without_starting_and_an_ignored_one_stays() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    let spare = node(&env, "spare", 64000, Some(60)).await;
    let on = vm_ha(
        &env,
        "on",
        failed,
        with_state(HaState::Stopped),
        PowerState::Running,
    )
    .await;
    let off = vm_ha(
        &env,
        "off",
        failed,
        with_state(HaState::Stopped),
        PowerState::Stopped,
    )
    .await;
    let ignored = vm_ha(
        &env,
        "ign",
        failed,
        with_state(HaState::Ignored),
        PowerState::Running,
    )
    .await;
    let manager = fast_manager(&env);
    eventually("stopped HA VMs moved", || recover_tasks(&env).len() == 2).await;
    for id in [on.id, off.id] {
        let moved = get(&env, id);
        assert_eq!(moved.spec.node, Some(spare));
        assert_eq!(moved.spec.power, PowerState::Stopped);
        assert_eq!(moved.status.phase, VmPhase::Stopped);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(get(&env, ignored.id), ignored);
    manager.shutdown().await;
}

#[tokio::test]
async fn fails_back_to_a_preferred_node_that_stayed_alive() {
    let env = Env::start().await;
    // low는 기준점부터 리스가 살아 있는 것으로 본다(recover_after 60초). high는 리스를 갱신한다.
    let low = node(&env, "low", 8192, Some(60)).await;
    let high = node(&env, "high", 8192, Some(60)).await;
    group(&env, "g", &[("low", 1), ("high", 5)], false, false).await;
    let a = vm_ha(&env, "a-vm", low, in_group("g"), PowerState::Running).await;
    let b = vm_ha(&env, "b-vm", low, in_group("g"), PowerState::Running).await;
    let renewer = keep_alive(&env, high);
    let manager = failback_manager(&env, Duration::from_secs(3));
    // failback_after 전에는 옮기지 않는다.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(migrate_tasks(&env).is_empty());
    eventually("failback task", || !migrate_tasks(&env).is_empty()).await;
    // 한 번에 하나: 첫 VM(이름 순)의 Task가 끝나지 않는 동안 다른 Task를 만들지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(migrate_tasks(&env), vec![(a.id, low, high)]);
    assert_eq!(get(&env, b.id).spec.node, Some(low));
    // 옮겨 떠난 노드가 아닌 곳(노드 장애 복구 뒤 등)으로의 되돌아가기는 옮기기로 세지 않는다.
    assert_eq!(get(&env, a.id).status.ha, None);
    renewer.abort();
    manager.shutdown().await;
}

#[tokio::test]
async fn does_not_fail_back_without_a_group_or_with_nofailback() {
    let env = Env::start().await;
    let low = node(&env, "low", 8192, Some(60)).await;
    let high = node(&env, "high", 8192, Some(60)).await;
    group(&env, "g", &[("low", 1), ("high", 5)], false, true).await;
    vm_ha(&env, "a-vm", low, in_group("g"), PowerState::Running).await;
    vm(&env, "b-vm", low, true, PowerState::Running).await;
    let renewer = keep_alive(&env, high);
    let manager = failback_manager(&env, Duration::from_millis(200));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(migrate_tasks(&env).is_empty());
    renewer.abort();
    manager.shutdown().await;
}

/// 거절된 되돌아가기는 거절 이유가 사라져도 1초마다 다시 하지 않는다(60초 뒤에 다시 본다).
#[tokio::test]
async fn a_refused_failback_is_not_retried_every_tick() {
    let env = Env::start().await;
    let low = node(&env, "low", 8192, Some(60)).await;
    let high = node(&env, "high", 8192, Some(60)).await;
    group(&env, "g", &[("low", 1), ("high", 5)], false, false).await;
    let a = vm_ha(&env, "a-vm", low, in_group("g"), PowerState::Running).await;
    // 끝나지 않은 켜기 Task가 있으면 마이그레이션은 거절된다(vm_busy).
    let busy = pending_task(&env.cluster, a.id, low).await;
    let renewer = keep_alive(&env, high);
    let manager = failback_manager(&env, Duration::from_millis(200));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(migrate_tasks(&env).is_empty());
    let done = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
        progress: None,
    };
    env.cluster
        .update_status::<TaskSpec>(busy, &done)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(migrate_tasks(&env).is_empty());
    renewer.abort();
    manager.shutdown().await;
}

#[tokio::test]
async fn rule_violations_appear_and_disappear() {
    let env = Env::start().await;
    let n1 = node(&env, "n1", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", n1, false, PowerState::Running).await;
    let b = vm(&env, "b-vm", n1, false, PowerState::Running).await;
    let off = vm(&env, "off", n1, false, PowerState::Stopped).await;
    let apart = rule(
        &env,
        "apart",
        RuleKind::AntiAffinity,
        &["a-vm", "b-vm", "off"],
        &[],
    )
    .await;
    rule(
        &env,
        "spread",
        RuleKind::SoftAntiAffinity,
        &["a-vm", "b-vm"],
        &[],
    )
    .await;
    let manager = fast_manager(&env);
    eventually("violations shown", || {
        [a.id, b.id]
            .iter()
            .all(|id| get(&env, *id).status.rule_violations == ["apart", "spread"])
    })
    .await;
    // 꺼진 VM은 자리 잡지 않아 위반이 없다.
    assert!(get(&env, off.id).status.rule_violations.is_empty());
    let shown = get(&env, a.id).revision;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(get(&env, a.id).revision, shown);
    // 규칙을 지우면 위반이 사라진다.
    env.cluster
        .delete::<PlacementRuleSpec>(apart, None)
        .await
        .unwrap();
    eventually("hard violation gone", || {
        [a.id, b.id]
            .iter()
            .all(|id| get(&env, *id).status.rule_violations == ["spread"])
    })
    .await;
    manager.shutdown().await;
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 노드 `node`의 reconciler가 재시작 횟수를 넘긴 VM을 옮겨 달라고 한 것처럼 status를 쓴다.
async fn ask_relocate(env: &Env, id: Uuid, node: Uuid) {
    let mut status = get(env, id).status;
    status.phase = VmPhase::Failed;
    status.node = Some(node);
    status.pid = None;
    status.last_error = Some("VM crashed 2 times on this node; asking HA to move it".into());
    let mut ha = status.ha.unwrap_or_default();
    ha.relocate_from = Some(node);
    status.ha = Some(ha);
    env.cluster
        .update_status::<VmSpec>(id, &status)
        .await
        .unwrap();
}

/// 옮겨 달라는 요청을 받으면 그 노드를 뺀 곳으로 옮기고 옮긴 기록을 하나 더한다. 받은 노드에서 다시 요청하면
/// 창 안에서 이미 `max_relocate`(1)번 옮겼으므로 옮기지 않고 끈다(`error`). 사용자가 다시 켜도 리더는 다시
/// 끄지 않는다(오류 표시는 받은 노드의 reconciler가 켤 때 지운다).
#[tokio::test]
async fn relocates_a_vm_that_failed_too_often_then_gives_up() {
    let env = Env::start().await;
    let from = node(&env, "from", 64000, Some(60)).await;
    let spare = node(&env, "spare", 8192, Some(60)).await;
    // 다른 노드의 리스는 살아 있지만 요청이 없는 VM은 그대로다.
    let quiet = vm(&env, "quiet", from, true, PowerState::Running).await;
    let a = vm(&env, "a-vm", from, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    let start = unix_now();
    ask_relocate(&env, a.id, from).await;
    eventually("relocated", || get(&env, a.id).spec.node == Some(spare)).await;
    let moved = get(&env, a.id);
    assert_eq!(moved.spec.power, PowerState::Running);
    assert_eq!(moved.status.phase, VmPhase::Stopped);
    assert_eq!(moved.status.node, None);
    assert_eq!(
        moved.status.last_error.as_deref(),
        Some("relocating: the VM failed too often on node from")
    );
    let ha = moved.status.ha.clone().unwrap();
    // 받은 노드가 켤 때까지 요청을 남긴다.
    assert_eq!(ha.relocate_from, Some(from));
    assert_eq!(ha.relocations.len(), 1);
    assert!(ha.relocations[0] >= start && ha.relocations[0] <= unix_now());
    assert_eq!(ha.relocated_from, Some(from));
    assert_eq!(ha.error, None);
    assert_eq!(get(&env, quiet.id).spec.node, Some(from));

    // 받은 노드에서 켠 뒤(요청을 지움) 다시 크래시해 옮겨 달라고 한다.
    let mut status = get(&env, a.id).status;
    status.phase = VmPhase::Running;
    status.node = Some(spare);
    status.ha.as_mut().unwrap().relocate_from = None;
    env.cluster
        .update_status::<VmSpec>(a.id, &status)
        .await
        .unwrap();
    ask_relocate(&env, a.id, spare).await;
    eventually("gave up", || {
        get(&env, a.id).status.ha.is_some_and(|h| h.error.is_some())
    })
    .await;
    let failed = get(&env, a.id);
    assert_eq!(failed.spec.node, Some(spare));
    assert_eq!(failed.spec.power, PowerState::Stopped);
    assert_eq!(failed.status.phase, VmPhase::Failed);
    assert_eq!(
        failed.status.last_error.as_deref(),
        Some("HA gave up after 1 relocations")
    );
    assert_eq!(
        failed.status.ha.as_ref().unwrap(),
        &VmHaStatus {
            relocate_from: None,
            relocations: ha.relocations.clone(),
            relocated_from: Some(from),
            error: Some("gave up after 1 relocations".into()),
            evict: None,
        }
    );

    // 사용자가 다시 켠다: 요청이 없으므로 리더는 그대로 둔다.
    let current = get(&env, a.id);
    let mut spec = current.spec.clone();
    spec.power = PowerState::Running;
    env.cluster
        .update_spec(a.id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let restarted = get(&env, a.id);
    assert_eq!(restarted.spec.power, PowerState::Running);
    assert_eq!(restarted.spec.node, Some(spare));
    manager.shutdown().await;
}

/// 재시작 횟수를 넘겨 그룹의 더 우선인 노드에서 옮겨 온 VM은 옮기기 창 안에서 그 노드로 되돌아가지 않는다.
#[tokio::test]
async fn a_relocated_vm_does_not_fail_back_within_the_window() {
    let env = Env::start().await;
    let a = node(&env, "a", 8192, Some(60)).await;
    let b = node(&env, "b", 8192, Some(60)).await;
    group(&env, "g", &[("a", 2), ("b", 1)], false, false).await;
    let moved = vm_ha(&env, "moved", b, in_group("g"), PowerState::Running).await;
    let mut status = moved.status.clone();
    status.ha = Some(VmHaStatus {
        relocations: vec![unix_now()],
        ..Default::default()
    });
    env.cluster
        .update_status::<VmSpec>(moved.id, &status)
        .await
        .unwrap();
    let renewer = keep_alive(&env, a);
    let manager = failback_manager(&env, Duration::from_millis(200));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(migrate_tasks(&env).is_empty());
    renewer.abort();
    manager.shutdown().await;
}

/// 창이 지나 옮겨 떠난 노드로 되돌아가면 옮기기 한 번으로 센다. 그 노드에서 다시 크래시하면 창 안에서
/// 이미 `max_relocate`(1)번 옮겼으므로 옮기지 않고 끈다(되돌아가기와 옮기기가 창마다 끝없이 돌지 않는다).
#[tokio::test]
async fn failing_back_to_the_node_it_left_counts_as_a_relocation() {
    let env = Env::start().await;
    let a = node(&env, "a", 8192, Some(60)).await;
    let b = node(&env, "b", 8192, Some(60)).await;
    group(&env, "g", &[("a", 2), ("b", 1)], false, false).await;
    let moved = vm_ha(&env, "moved", b, in_group("g"), PowerState::Running).await;
    // a에서 옮겨 온 지 창(600초)보다 오래 지났다.
    let old = unix_now() - 700;
    let mut status = moved.status.clone();
    status.ha = Some(VmHaStatus {
        relocations: vec![old],
        relocated_from: Some(a),
        ..Default::default()
    });
    env.cluster
        .update_status::<VmSpec>(moved.id, &status)
        .await
        .unwrap();
    let renewer = keep_alive(&env, a);
    let start = unix_now();
    let manager = failback_manager(&env, Duration::from_millis(200));
    eventually("failback task", || !migrate_tasks(&env).is_empty()).await;
    assert_eq!(migrate_tasks(&env)[0], (moved.id, b, a));
    eventually("failback counted", || {
        get(&env, moved.id)
            .status
            .ha
            .is_some_and(|h| h.relocations.iter().any(|at| *at >= start))
    })
    .await;
    let ha = get(&env, moved.id).status.ha.unwrap();
    assert_eq!(ha.relocations.len(), 1);
    assert_eq!(ha.relocated_from, None);

    // 마이그레이션이 끝난 것처럼 a에서 실행 중으로 쓰고, a에서 다시 크래시해 옮겨 달라고 한다.
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let task = tasks
        .iter()
        .find(|t| matches!(t.spec.op, TaskOp::VmMigrate { .. }))
        .unwrap()
        .id;
    let done = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
        progress: None,
    };
    env.cluster
        .update_status::<TaskSpec>(task, &done)
        .await
        .unwrap();
    let current = get(&env, moved.id);
    let mut spec = current.spec.clone();
    spec.node = Some(a);
    env.cluster
        .update_spec(moved.id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();
    let mut status = get(&env, moved.id).status;
    status.phase = VmPhase::Running;
    status.node = Some(a);
    status.migration = None;
    env.cluster
        .update_status::<VmSpec>(moved.id, &status)
        .await
        .unwrap();
    ask_relocate(&env, moved.id, a).await;
    eventually("gave up", || {
        get(&env, moved.id)
            .status
            .ha
            .is_some_and(|h| h.error.is_some())
    })
    .await;
    let failed = get(&env, moved.id);
    assert_eq!(failed.spec.node, Some(a));
    assert_eq!(failed.spec.power, PowerState::Stopped);
    assert_eq!(failed.status.phase, VmPhase::Failed);
    renewer.abort();
    manager.shutdown().await;
}

/// 창 밖의 옮긴 기록은 버리고 세지 않는다. `max_relocate` 0이면 첫 요청에 끈다.
#[tokio::test]
async fn relocations_outside_the_window_are_dropped() {
    let env = Env::start().await;
    let from = node(&env, "from", 64000, Some(60)).await;
    let spare = node(&env, "spare", 8192, Some(60)).await;
    let a = vm(&env, "a-vm", from, true, PowerState::Running).await;
    let never = vm_ha(
        &env,
        "never",
        from,
        Some(VmHa {
            max_relocate: 0,
            ..Default::default()
        }),
        PowerState::Running,
    )
    .await;
    let old = unix_now() - 3600;
    let mut status = a.status.clone();
    status.ha = Some(VmHaStatus {
        relocations: vec![old],
        ..Default::default()
    });
    env.cluster
        .update_status::<VmSpec>(a.id, &status)
        .await
        .unwrap();
    let manager = fast_manager(&env);
    ask_relocate(&env, a.id, from).await;
    ask_relocate(&env, never.id, from).await;
    eventually("relocated", || get(&env, a.id).spec.node == Some(spare)).await;
    let relocations = get(&env, a.id).status.ha.unwrap().relocations;
    assert_eq!(relocations.len(), 1);
    assert_ne!(relocations[0], old);
    // spec을 먼저 쓰고 status에 오류를 쓴다.
    eventually("gave up", || {
        get(&env, never.id)
            .status
            .ha
            .is_some_and(|h| h.error.is_some())
    })
    .await;
    let failed = get(&env, never.id);
    assert_eq!(failed.spec.power, PowerState::Stopped);
    assert_eq!(failed.spec.node, Some(from));
    assert_eq!(
        failed.status.ha.and_then(|h| h.error).as_deref(),
        Some("gave up after 0 relocations")
    );
    manager.shutdown().await;
}

/// 호스트 종료 정책 `freeze`로 얼린 오프라인 노드의 HA VM은 `recover_after`가 지나도 옮기지 않는다
/// (`failover` 표시면 평소대로 옮긴다). `POST /nodes/{key}/unfreeze`가 표시를 지우면 옮긴다. 온라인 노드는 409,
/// 운영자는 403이다.
#[tokio::test]
async fn a_frozen_offline_node_is_not_recovered_until_unfrozen() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let marked = |action: ShutdownAction| {
        move |n: &mut Resource<NodeSpec, NodeStatus>| {
            n.status.online = false;
            n.status.shutdown = Some(NodeShutdown {
                action,
                maintenance: false,
                at: 1,
            });
        }
    };
    let frozen = node_with(
        &env,
        "frozen",
        8192,
        Some(1),
        marked(ShutdownAction::Freeze),
    )
    .await;
    let failover = node_with(
        &env,
        "failover",
        8192,
        Some(1),
        marked(ShutdownAction::Failover),
    )
    .await;
    let spare = node(&env, "spare", 64000, Some(60)).await;
    let a = vm(&env, "a-vm", frozen, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", failover, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    eventually("failover node recovered", || {
        get(&env, b.id).spec.node == Some(spare)
    })
    .await;
    // recover_after(1초)를 여러 번 넘겨도 얼린 노드의 VM은 그대로다.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(get(&env, a.id), a);
    assert_eq!(recover_tasks(&env).len(), 1);

    let operator = env.user("op", "operator").await;
    let (status, got) = operator.post("/nodes/frozen/unfreeze", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{got}");
    let (status, got) = admin.post("/nodes/spare/unfreeze", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    assert_eq!(got["code"], "node_online", "{got}");

    let (status, got) = admin.post("/nodes/frozen/unfreeze", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert!(got["status"].get("shutdown").is_none(), "{got}");
    assert_eq!(got["status"]["online"], false, "{got}");
    eventually("frozen node recovered after unfreeze", || {
        get(&env, a.id).spec.node == Some(spare)
    })
    .await;
    // 표시가 없으면 그대로 돌려준다.
    let (status, again) = admin.post("/nodes/frozen/unfreeze", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["revision"], got["revision"]);
    manager.shutdown().await;
}

/// spec 쓰기(VM을 옮기는 쓰기)의 VM 순서를 적는 쓰기 경로
#[derive(Default)]
struct Recording {
    moved: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl RecoveryWriter for Recording {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        let id = match &command {
            Command::UpdateSpec {
                kind: Kind::Vm, id, ..
            } => Some(*id),
            _ => None,
        };
        let result = leader_write(cluster, command).await;
        if let (Some(id), Ok(_)) = (id, &result) {
            self.moved.lock().unwrap().push(id);
        }
        result
    }
}

/// 우선순위 `priority`의 기본 HA 정책
fn with_priority(priority: u8) -> Option<VmHa> {
    Some(VmHa {
        priority,
        ..Default::default()
    })
}

#[tokio::test]
async fn recovers_higher_priority_vms_first_then_by_name() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    node(&env, "spare", 64000, Some(60)).await;
    let low = vm_ha(
        &env,
        "a-low",
        failed,
        with_priority(10),
        PowerState::Running,
    )
    .await;
    let high = vm_ha(
        &env,
        "z-high",
        failed,
        with_priority(90),
        PowerState::Running,
    )
    .await;
    let c = vm(&env, "c-vm", failed, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", failed, true, PowerState::Running).await;
    let writer = Arc::new(Recording::default());
    let manager = manager_with(&env, writer.clone());
    eventually("HA VMs moved", || recover_tasks(&env).len() == 4).await;
    // 우선순위가 높은 것 → 같으면(기본 50) 이름 순
    assert_eq!(
        *writer.moved.lock().unwrap(),
        vec![high.id, b.id, c.id, low.id]
    );
    manager.shutdown().await;
}

/// 이 VM이 노드 `node`에서 실행 중이라고 status에 쓴다(그 노드의 reconciler 대신).
async fn mark_running(env: &Env, id: Uuid, node: Uuid) {
    let mut status = get(env, id).status;
    status.phase = VmPhase::Running;
    status.node = Some(node);
    env.cluster
        .update_status::<VmSpec>(id, &status)
        .await
        .unwrap();
}

/// 클러스터 설정의 대상 노드당 동시 시작 수를 바꾼다.
async fn set_start_concurrency(env: &Env, n: u8) {
    let spec = SettingsSpec {
        ha_start_concurrency: Some(n),
        ..Default::default()
    };
    match env
        .cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
    {
        Some(s) => {
            env.cluster
                .update_spec(s.id, Some(s.revision), s.labels, &spec)
                .await
                .unwrap();
        }
        None => {
            let s: Settings = Resource::new(SETTINGS_NAME, spec);
            env.cluster.create(&s).await.unwrap();
        }
    }
}

#[tokio::test]
async fn starts_at_most_the_concurrency_limit_on_one_target_node() {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(1)).await;
    let spare = node(&env, "spare", 64000, Some(60)).await;
    let mut vms = vec![];
    for i in 1..=6 {
        vms.push(vm(&env, &format!("vm-{i}"), failed, true, PowerState::Running).await);
    }
    let moved = |env: &Env| -> Vec<Uuid> {
        vms.iter()
            .filter(|v| get(env, v.id).spec.node == Some(spare))
            .map(|v| v.id)
            .collect()
    };
    let manager = fast_manager(&env);
    // 기본 상한 4: 이름 순으로 넷만 옮기고, 나머지는 죽은 노드에 남아 기다린다.
    eventually("four moved", || moved(&env).len() == 4).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ids: Vec<Uuid> = vms.iter().map(|v| v.id).collect();
    assert_eq!(moved(&env), ids[..4]);
    assert_eq!(get(&env, ids[4]).spec.node, Some(failed));

    // 하나가 그 노드에서 켜지면 다음 하나를 옮긴다.
    mark_running(&env, ids[0], spare).await;
    eventually("fifth moved", || moved(&env).len() == 5).await;
    assert_eq!(moved(&env), ids[..5]);

    // 상한을 2로 줄이면 진행 중(vm-2~5, 넷)이 2 아래로 내려가야 옮긴다.
    set_start_concurrency(&env, 2).await;
    mark_running(&env, ids[1], spare).await;
    mark_running(&env, ids[2], spare).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(moved(&env).len(), 5);
    mark_running(&env, ids[3], spare).await;
    eventually("sixth moved", || moved(&env).len() == 6).await;
    manager.shutdown().await;
}

/// 미리 고른 복구 노드 시험: `failed`(리스 3초)의 a-vm은 처음 틱에 메모리가 많은 x로 계획된다. 1초 뒤
/// `change`가 상태를 바꾸고, 리스가 끝나면 옮긴 노드를 돌려준다.
async fn recovered_after_planning(change: impl AsyncFnOnce(&Env, Uuid)) -> (Uuid, Uuid, Uuid) {
    let env = Env::start().await;
    let failed = node(&env, "failed", 8192, Some(3)).await;
    let x = node(&env, "x", 5000, Some(60)).await;
    let y = node(&env, "y", 4096, Some(60)).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let manager = fast_manager(&env);
    tokio::time::sleep(Duration::from_secs(1)).await;
    change(&env, x).await;
    eventually("HA VM moved", || get(&env, a.id).spec.node != Some(failed)).await;
    let to = get(&env, a.id).spec.node.unwrap();
    manager.shutdown().await;
    (to, x, y)
}

#[tokio::test]
async fn recovers_to_the_target_chosen_before_the_failure() {
    // 계획 뒤 x에 다른 VM이 놓여 지금 고르면 y가 낫지만, 계획한 x가 여전히 맞으므로 그대로 쓴다.
    let (to, x, _) = recovered_after_planning(async |env: &Env, x: Uuid| {
        vm(env, "filler", x, false, PowerState::Running).await;
    })
    .await;
    assert_eq!(to, x);
}

#[tokio::test]
async fn a_planned_target_in_maintenance_is_picked_again() {
    let (to, _, y) = recovered_after_planning(async |env: &Env, x: Uuid| {
        let n = env.cluster.get::<NodeSpec>(x).unwrap().unwrap();
        let mut spec = n.spec.clone();
        spec.maintenance = true;
        env.cluster
            .update_spec(x, Some(n.revision), n.labels, &spec)
            .await
            .unwrap();
    })
    .await;
    assert_eq!(to, y);
}

#[tokio::test]
async fn a_hard_rule_made_after_planning_is_followed() {
    let (to, _, y) = recovered_after_planning(async |env: &Env, _: Uuid| {
        rule(
            env,
            "keep-off",
            RuleKind::NodeAntiAffinity,
            &["a-vm"],
            &["x"],
        )
        .await;
    })
    .await;
    assert_eq!(to, y);
}

/// 가짜 전원 끄기 실행기: 명령 방법의 `off`가 `fail`이면 끄기가 실패하고(`slow-fail`은 4초 뒤 실패), 아니면 그 노드가
/// 꺼진다(그 뒤 `status`가 off). 끄기 호출(노드 이름, `off`, 때)을 적는다.
#[derive(Default)]
struct FakeFence {
    calls: Mutex<Vec<(String, String, std::time::Instant)>>,
    off: Mutex<Vec<Uuid>>,
}

impl FakeFence {
    fn calls(&self) -> Vec<(String, String)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(node, off, _)| (node.clone(), off.clone()))
            .collect()
    }
}

#[async_trait]
impl PowerFence for FakeFence {
    async fn off(&self, node: &Node, method: &FenceMethod) -> Result<(), String> {
        let FenceMethod::Command(c) = method else {
            panic!("unexpected method {method:?}");
        };
        self.calls.lock().unwrap().push((
            node.name.clone(),
            c.off.clone(),
            std::time::Instant::now(),
        ));
        if c.off == "slow-fail" {
            tokio::time::sleep(Duration::from_secs(4)).await;
        }
        if c.off.ends_with("fail") {
            return Err("BMC unreachable".into());
        }
        self.off.lock().unwrap().push(node.id);
        Ok(())
    }

    async fn status(&self, node: &Node, _: &FenceMethod) -> Result<Power, String> {
        Ok(if self.off.lock().unwrap().contains(&node.id) {
            Power::Off
        } else {
            Power::On
        })
    }
}

fn fence_manager(env: &Env, fence: Arc<FakeFence>) -> HaManager {
    HaManager::spawn_with_fence(
        env.state.clone(),
        HaManagerConfig {
            tick: Duration::from_millis(100),
            ..Default::default()
        },
        fence,
    )
}

/// 계획 51 노드: 리스가 `fence_after`초 끊기면 HA VM을 끄고 `recover_after`초 뒤 리더가 옮긴다. 모든 기능을 안다.
async fn fast_node(env: &Env, name: &str, fence_after: Option<u64>, recover_after: u64) -> Uuid {
    fast_node_with(env, name, fence_after, recover_after, |_| {}).await
}

async fn fast_node_with(
    env: &Env,
    name: &str,
    fence_after: Option<u64>,
    recover_after: u64,
    tweak: impl FnOnce(&mut Resource<NodeSpec, NodeStatus>),
) -> Uuid {
    node_with(env, name, 8192, Some(recover_after), |n| {
        n.status.ha.as_mut().unwrap().fence_after_secs = fence_after;
        n.status.features = FEATURES.iter().map(|f| f.to_string()).collect();
        tweak(n);
    })
    .await
}

/// 노드 `node`(이름 `name`)의 전원 끄기 펜싱: 명령 방법들(`off`마다 하나)
async fn fence_node(env: &Env, node: Uuid, name: &str, offs: &[&str]) {
    let mut fence = Resource::new(
        name,
        NodeFenceSpec {
            methods: offs
                .iter()
                .map(|off| {
                    FenceMethod::Command(CommandFence {
                        off: off.to_string(),
                        status: "echo on".into(),
                    })
                })
                .collect(),
        },
    );
    fence.id = node;
    env.cluster.create(&fence).await.unwrap();
}

/// 기록 Task `node-power-off`(대상 노드, op, status)
fn power_off_tasks(env: &Env) -> Vec<(Uuid, TaskOp, TaskStatus)> {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    tasks
        .into_iter()
        .filter(|t| matches!(t.spec.op, TaskOp::NodePowerOff { .. }))
        .map(|t| {
            assert_eq!(t.spec.node, t.spec.target);
            (t.spec.target, t.spec.op, t.status)
        })
        .collect()
}

#[tokio::test]
async fn powers_off_a_node_past_fence_after_once_and_recovers_without_waiting() {
    let env = Env::start().await;
    // recover_after(600초)를 기다리면 시험이 끝나지 않는다.
    let failed = fast_node(&env, "failed", Some(1), 600).await;
    let spare = fast_node(&env, "spare", Some(1), 600).await;
    // 리더 자신과 대상 노드의 리스는 살아 있다.
    keep_alive(&env, env.node);
    keep_alive(&env, spare);
    // 첫 방법은 실패하고 둘째가 끈다.
    fence_node(&env, failed, "failed", &["fail", "ok"]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("HA VM moved", || get(&env, a.id).spec.node == Some(spare)).await;
    eventually("record tasks", || {
        recover_tasks(&env).len() == 1 && power_off_tasks(&env).len() == 1
    })
    .await;
    assert_eq!(
        power_off_tasks(&env),
        vec![(
            failed,
            TaskOp::NodePowerOff {
                method: Some(FenceKind::Command),
                reason: LEASE_EXPIRED.into(),
            },
            TaskStatus {
                state: TaskState::Succeeded,
                error: None,
                progress: None,
            }
        )]
    );
    // 같은 임기에 다시 끄지 않는다. VM이 없는 spare는 끄지 않는다(펜싱도 없다).
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        fence.calls(),
        [
            ("failed".into(), "fail".into()),
            ("failed".into(), "ok".into())
        ]
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn a_failed_power_off_waits_for_recover_after_and_is_not_retried() {
    let env = Env::start().await;
    let failed = fast_node(&env, "failed", Some(1), 4).await;
    let spare = fast_node(&env, "spare", Some(1), 600).await;
    keep_alive(&env, env.node);
    keep_alive(&env, spare);
    fence_node(&env, failed, "failed", &["fail", "fail"]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let fence = Arc::new(FakeFence::default());
    let started = std::time::Instant::now();
    let manager = fence_manager(&env, fence.clone());
    eventually("power-off failed", || power_off_tasks(&env).len() == 1).await;
    let (target, op, status) = power_off_tasks(&env).remove(0);
    assert_eq!(target, failed);
    assert_eq!(
        op,
        TaskOp::NodePowerOff {
            method: None,
            reason: LEASE_EXPIRED.into()
        }
    );
    assert_eq!(status.state, TaskState::Failed);
    assert_eq!(
        status.error.as_deref(),
        Some("command: BMC unreachable; command: BMC unreachable")
    );
    // recover_after(4초)가 지나야 옮긴다.
    eventually("HA VM moved", || get(&env, a.id).spec.node != Some(failed)).await;
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert_eq!(fence.calls().len(), 2);
    manager.shutdown().await;
}

#[tokio::test]
async fn does_not_power_off_nodes_without_fencing_old_frozen_or_without_ha_vms() {
    let env = Env::start().await;
    let nofence = fast_node(&env, "nofence", Some(1), 600).await;
    // 계획 51 전 노드는 `fence_after_secs`를 알리지 않는다.
    let old = fast_node(&env, "old", None, 600).await;
    let frozen = fast_node_with(&env, "frozen", Some(1), 600, |n| {
        n.status.online = false;
        n.status.shutdown = Some(NodeShutdown {
            action: ShutdownAction::Freeze,
            maintenance: false,
            at: 1,
        });
    })
    .await;
    let empty = fast_node(&env, "empty", Some(1), 600).await;
    let parked = fast_node(&env, "parked", Some(1), 600).await;
    fast_node(&env, "spare", Some(1), 600).await;
    for (id, name) in [
        (old, "old"),
        (frozen, "frozen"),
        (empty, "empty"),
        (parked, "parked"),
    ] {
        fence_node(&env, id, name, &["ok"]).await;
    }
    let mut vms = vec![];
    for (name, node) in [("v-nofence", nofence), ("v-old", old), ("v-frozen", frozen)] {
        vms.push((vm(&env, name, node, true, PowerState::Running).await, node));
    }
    // HA가 아니거나 꺼진 VM만 있다.
    vm(&env, "plain", empty, false, PowerState::Running).await;
    vm(&env, "off", empty, true, PowerState::Stopped).await;
    // 꺼진 `state: stopped` HA VM만 있다(`recover_after` 뒤 자리만 옮긴다).
    let stopped = VmHa {
        state: HaState::Stopped,
        ..Default::default()
    };
    let p = vm_ha(&env, "v-parked", parked, Some(stopped), PowerState::Stopped).await;
    vms.push((p, parked));
    keep_alive(&env, env.node);
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(fence.calls().is_empty(), "{:?}", fence.calls());
    for (vm, node) in vms {
        assert_eq!(get(&env, vm.id).spec.node, Some(node), "{}", vm.name);
    }
    assert!(power_off_tasks(&env).is_empty());
    manager.shutdown().await;
}

#[tokio::test]
async fn a_new_leader_powers_off_fence_after_from_its_base() {
    // 이 리더는 리스를 받은 적이 없으므로 처음 리더임을 본 때부터 `fence_after`(2초)를 센다.
    let env = Env::start().await;
    let failed = fast_node(&env, "failed", Some(2), 600).await;
    let spare = fast_node(&env, "spare", Some(2), 600).await;
    keep_alive(&env, env.node);
    keep_alive(&env, spare);
    fence_node(&env, failed, "failed", &["ok"]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let fence = Arc::new(FakeFence::default());
    let started = std::time::Instant::now();
    let manager = fence_manager(&env, fence.clone());
    eventually("HA VM moved", || get(&env, a.id).spec.node != Some(failed)).await;
    let calls = fence.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].2.duration_since(started) >= Duration::from_secs(2));
    manager.shutdown().await;
}

#[tokio::test]
async fn a_requested_power_off_recovers_a_node_whose_lease_is_alive() {
    let env = Env::start().await;
    let failed = fast_node(&env, "failed", Some(600), 600).await;
    let spare = fast_node(&env, "spare", Some(600), 600).await;
    let nofence = fast_node(&env, "nofence", Some(600), 600).await;
    fence_node(&env, failed, "failed", &["ok"]).await;
    // 이 노드 자신은 끄지 않는다.
    fence_node(&env, env.node, "node1", &["ok"]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", nofence, true, PowerState::Running).await;
    renew(&env.cluster, failed).await.unwrap();
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    manager.power_off(env.node, "health check failed");
    manager.power_off(nofence, "health check failed");
    manager.power_off(failed, "health check failed");
    eventually("HA VM moved", || get(&env, a.id).spec.node == Some(spare)).await;
    eventually("record task", || power_off_tasks(&env).len() == 1).await;
    assert_eq!(
        power_off_tasks(&env)[0].1,
        TaskOp::NodePowerOff {
            method: Some(FenceKind::Command),
            reason: "health check failed".into(),
        }
    );
    assert_eq!(fence.calls(), [("failed".into(), "ok".into())]);
    assert_eq!(get(&env, b.id).spec.node, Some(nofence));
    manager.shutdown().await;
}

#[tokio::test]
async fn no_power_off_task_is_recorded_while_an_old_node_remains() {
    let env = Env::start().await;
    let failed = fast_node(&env, "failed", Some(1), 600).await;
    // `fast-ha`를 모르는 노드가 있으면 기록 Task를 쓰지 않는다(그 노드가 풀지 못한다).
    node(&env, "spare", 8192, Some(600)).await;
    keep_alive(&env, env.node);
    fence_node(&env, failed, "failed", &["ok"]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("HA VM moved", || get(&env, a.id).spec.node != Some(failed)).await;
    assert_eq!(fence.calls().len(), 1);
    assert!(power_off_tasks(&env).is_empty());
    manager.shutdown().await;
}

/// 함께 죽은 노드(리스가 `fence_after`보다 오래 끊겼지만 아직 `recover_after` 전)로는 옮기지 않는다. 그 노드의 리스가
/// 살아나면 그리로 옮긴다.
#[tokio::test]
async fn does_not_recover_to_a_node_that_died_too() {
    let env = Env::start().await;
    let failed = fast_node(&env, "failed", Some(1), 600).await;
    // 함께 죽었고 끌 방법이 없다(`recover_after`까지 기다린다).
    let other = fast_node(&env, "other", Some(1), 600).await;
    fence_node(&env, failed, "failed", &["ok"]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    keep_alive(&env, env.node);
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("powered off", || power_off_tasks(&env).len() == 1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(get(&env, a.id).spec.node, Some(failed));
    keep_alive(&env, other);
    eventually("HA VM moved", || get(&env, a.id).spec.node == Some(other)).await;
    manager.shutdown().await;
}

/// 이 리더의 리스가 `fence_after`보다 오래 갱신되지 않았으면(리더 쪽 문제) 리스가 끝난 노드를 끄지 않는다. 리더가
/// 다시 갱신하면 끈다.
#[tokio::test]
async fn a_leader_with_a_stale_lease_powers_off_nobody() {
    let env = Env::start().await;
    let failed = fast_node(&env, "failed", Some(1), 600).await;
    let spare = fast_node(&env, "spare", Some(1), 600).await;
    keep_alive(&env, spare);
    fence_node(&env, failed, "failed", &["ok"]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    // 리더는 한 번 갱신하고 멈춘다.
    renew(&env.cluster, env.node).await.unwrap();
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(fence.calls().is_empty(), "{:?}", fence.calls());
    assert_eq!(get(&env, a.id).spec.node, Some(failed));
    keep_alive(&env, env.node);
    eventually("HA VM moved", || get(&env, a.id).spec.node == Some(spare)).await;
    assert_eq!(fence.calls(), [("failed".into(), "ok".into())]);
    manager.shutdown().await;
}

/// 한 임기에 HA 가능 노드의 소수(4대면 1대)까지만 끈다. 요청받은 끄기도 같은 상한을 센다.
#[tokio::test]
async fn powers_off_at_most_a_minority_of_the_nodes_in_a_term() {
    let env = Env::start().await;
    let f1 = fast_node(&env, "f1", Some(1), 600).await;
    let f2 = fast_node(&env, "f2", Some(1), 600).await;
    for (id, name) in [(f1, "f1"), (f2, "f2")] {
        fence_node(&env, id, name, &["ok"]).await;
        vm(&env, &format!("{name}-vm"), id, true, PowerState::Running).await;
    }
    for name in ["s1", "s2"] {
        let spare = fast_node(&env, name, Some(1), 600).await;
        keep_alive(&env, spare);
    }
    keep_alive(&env, env.node);
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("one node powered off", || fence.calls().len() == 1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(fence.calls().len(), 1, "{:?}", fence.calls());
    let first = fence.calls()[0].0.clone();
    let rest = if first == "f1" { f2 } else { f1 };
    manager.power_off(rest, "health check failed");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(fence.calls().len(), 1, "{:?}", fence.calls());
    manager.shutdown().await;
}

/// 끄기가 실패한 결과가 watchdog 기다림(`recover_after`)으로 이미 옮긴 뒤에 오면 기록 Task가 그렇게 말한다.
#[tokio::test]
async fn a_power_off_failing_after_the_watchdog_wait_says_the_vms_were_recovered() {
    let env = Env::start().await;
    let failed = fast_node(&env, "failed", Some(1), 3).await;
    let spare = fast_node(&env, "spare", Some(1), 600).await;
    keep_alive(&env, env.node);
    keep_alive(&env, spare);
    fence_node(&env, failed, "failed", &["slow-fail"]).await;
    let a = vm(&env, "a-vm", failed, true, PowerState::Running).await;
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("HA VM moved", || get(&env, a.id).spec.node == Some(spare)).await;
    assert!(power_off_tasks(&env).is_empty());
    eventually("power-off failed", || power_off_tasks(&env).len() == 1).await;
    let (_, _, status) = power_off_tasks(&env).remove(0);
    assert_eq!(status.state, TaskState::Failed);
    assert_eq!(
        status.error.as_deref(),
        Some(
            "power off failed; the node's VMs were already recovered after the watchdog wait: command: BMC unreachable"
        )
    );
    manager.shutdown().await;
}

/// 노드 `node`의 이름으로 리스를 갱신하며 HA 검사 결과를 싣는다(그 노드의 검사기 대신).
async fn renew_with(cluster: &Cluster, node: Uuid, checks: Vec<CheckReport>) {
    cluster
        .execute_as_leader(LeaderRequest::RenewLease {
            node: NodeId(node),
            checks,
        })
        .await
        .unwrap();
}

fn passing(name: &str) -> CheckReport {
    CheckReport::new(name, CheckState::Ok, None, None)
}

fn failing(name: &str) -> CheckReport {
    CheckReport::new(
        name,
        CheckState::Failing,
        Some(format!("{name} is down")),
        Some(1),
    )
}

/// HA 검사를 저장소에 직접 만든다(grace 1초).
async fn ha_check(
    env: &Env,
    name: &str,
    role: HaCheckRole,
    target: HaCheckTarget,
    on_fail: Option<HaCheckAction>,
) {
    let check = Resource::new(
        name,
        HaCheckSpec {
            role,
            target,
            on_fail,
            grace_secs: 1,
            nodes: Default::default(),
            enabled: true,
        },
    );
    env.cluster.create(&check).await.unwrap();
}

fn link(iface: &str) -> HaCheckTarget {
    HaCheckTarget::Link {
        iface: iface.into(),
    }
}

/// 기록 Task `ha-check`(대상 노드, op, status)
fn check_tasks(env: &Env) -> Vec<(Uuid, TaskOp, TaskStatus)> {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    tasks
        .into_iter()
        .filter(|t| matches!(t.spec.op, TaskOp::HaCheck { .. }))
        .map(|t| {
            assert_eq!(t.spec.node, t.spec.target);
            (t.spec.target, t.spec.op, t.status)
        })
        .collect()
}

fn succeeded() -> TaskStatus {
    TaskStatus {
        state: TaskState::Succeeded,
        error: None,
        progress: None,
    }
}

/// 실패한 검사는 grace(1초) 뒤에 한 번 경고하고(기록 Task), 회복한 뒤 다시 실패하면 또 경고한다.
#[tokio::test]
async fn a_failing_check_warns_once_after_its_grace() {
    let env = Env::start().await;
    let n1 = fast_node(&env, "n1", None, 60).await;
    let n2 = fast_node(&env, "n2", None, 60).await;
    ha_check(&env, "mgmt", HaCheckRole::Management, link("eth0"), None).await;
    renew_with(&env.cluster, n1, vec![failing("mgmt")]).await;
    renew_with(&env.cluster, n2, vec![passing("mgmt")]).await;
    let manager = fast_manager(&env);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(check_tasks(&env).is_empty());
    eventually("warned", || check_tasks(&env).len() == 1).await;
    let warned = (
        n1,
        TaskOp::HaCheck {
            check: "mgmt".into(),
            action: HaCheckAction::Warn,
            vms: vec![],
            detail: "mgmt is down".into(),
        },
        succeeded(),
    );
    assert_eq!(check_tasks(&env), std::slice::from_ref(&warned));
    // 같은 실패에는 다시 쓰지 않는다.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(check_tasks(&env).len(), 1);
    // 회복한 뒤 다시 실패하면 새 실패다.
    renew_with(&env.cluster, n1, vec![passing("mgmt")]).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    renew_with(&env.cluster, n1, vec![failing("mgmt")]).await;
    eventually("warned again", || check_tasks(&env).len() == 2).await;
    assert_eq!(check_tasks(&env), [warned.clone(), warned]);
    manager.shutdown().await;
}

/// 자동 관리 검사(`cluster-link-*`)는 리소스가 없어도 기본 grace(10초) 뒤에 경고만 한다.
#[tokio::test]
async fn a_failing_cluster_link_only_warns() {
    let env = Env::start().await;
    let n1 = fast_node(&env, "n1", None, 60).await;
    let n2 = fast_node(&env, "n2", None, 60).await;
    fence_node(&env, n1, "n1", &["ok"]).await;
    vm(&env, "a-vm", n1, true, PowerState::Running).await;
    renew_with(&env.cluster, n1, vec![failing("cluster-link-1")]).await;
    renew_with(&env.cluster, n2, vec![passing("cluster-link-1")]).await;
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(check_tasks(&env).is_empty());
    eventually("warned", || check_tasks(&env).len() == 1).await;
    assert_eq!(
        check_tasks(&env)[0].1,
        TaskOp::HaCheck {
            check: "cluster-link-1".into(),
            action: HaCheckAction::Warn,
            vms: vec![],
            detail: "cluster-link-1 is down".into(),
        }
    );
    assert!(fence.calls().is_empty());
    manager.shutdown().await;
}

/// `fence`: 전원 끄기 펜싱이 있는 노드는 끄고 그 HA VM을 복구한다. 없는 노드는 실패 기록 Task만 남긴다. 네 노드
/// 가운데 둘이 실패하는 것은 과반이 아니다.
#[tokio::test]
async fn a_fence_check_powers_off_the_node_or_records_that_it_cannot() {
    let env = Env::start().await;
    let fenced = fast_node(&env, "fenced", None, 600).await;
    let nofence = fast_node(&env, "nofence", None, 600).await;
    let s1 = fast_node(&env, "s1", None, 600).await;
    let s2 = fast_node(&env, "s2", None, 600).await;
    fence_node(&env, fenced, "fenced", &["ok"]).await;
    let a = vm(&env, "a-vm", fenced, true, PowerState::Running).await;
    let b = vm(&env, "b-vm", nofence, true, PowerState::Running).await;
    ha_check(
        &env,
        "mgmt",
        HaCheckRole::Management,
        link("eth0"),
        Some(HaCheckAction::Fence),
    )
    .await;
    for (node, report) in [
        (fenced, failing("mgmt")),
        (nofence, failing("mgmt")),
        (s1, passing("mgmt")),
        (s2, passing("mgmt")),
    ] {
        renew_with(&env.cluster, node, vec![report]).await;
    }
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("HA VM recovered", || {
        let node = get(&env, a.id).spec.node;
        node == Some(s1) || node == Some(s2)
    })
    .await;
    assert_eq!(fence.calls(), [("fenced".into(), "ok".into())]);
    eventually("power-off recorded", || power_off_tasks(&env).len() == 1).await;
    assert_eq!(
        power_off_tasks(&env)[0].1,
        TaskOp::NodePowerOff {
            method: Some(FenceKind::Command),
            reason: "HA check mgmt failed".into(),
        }
    );
    let mut records = check_tasks(&env);
    records.sort_by_key(|(node, _, _)| *node != fenced);
    assert_eq!(
        records,
        [
            (
                fenced,
                TaskOp::HaCheck {
                    check: "mgmt".into(),
                    action: HaCheckAction::Fence,
                    vms: vec!["a-vm".into()],
                    detail: "mgmt is down; requested a power-off of the node".into(),
                },
                succeeded(),
            ),
            (
                nofence,
                TaskOp::HaCheck {
                    check: "mgmt".into(),
                    action: HaCheckAction::Fence,
                    vms: vec!["b-vm".into()],
                    detail: "mgmt is down; no power fencing configured".into(),
                },
                TaskStatus {
                    state: TaskState::Failed,
                    error: Some("mgmt is down; no power fencing configured".into()),
                    progress: None,
                },
            ),
        ]
    );
    assert_eq!(get(&env, b.id).spec.node, Some(nofence));
    manager.shutdown().await;
}

/// 세 노드 가운데 둘이 실패하면 공유 장비 문제로 보고 기록 Task 하나만 남기고 끄지 않는다. 하나가 회복해 1/3이
/// 되면 남은 노드는 grace를 다시 기다린 뒤 대응한다.
#[tokio::test]
async fn a_check_failing_on_most_nodes_only_warns() {
    let env = Env::start().await;
    let n1 = fast_node(&env, "n1", None, 600).await;
    let n2 = fast_node(&env, "n2", None, 600).await;
    let n3 = fast_node(&env, "n3", None, 600).await;
    for (id, name) in [(n1, "n1"), (n2, "n2")] {
        fence_node(&env, id, name, &["ok"]).await;
        vm(&env, &format!("{name}-vm"), id, true, PowerState::Running).await;
    }
    ha_check(
        &env,
        "mgmt",
        HaCheckRole::Management,
        link("eth0"),
        Some(HaCheckAction::Fence),
    )
    .await;
    renew_with(&env.cluster, n1, vec![failing("mgmt")]).await;
    renew_with(&env.cluster, n2, vec![failing("mgmt")]).await;
    renew_with(&env.cluster, n3, vec![passing("mgmt")]).await;
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("shared failure recorded", || check_tasks(&env).len() == 1).await;
    let (node, op, status) = check_tasks(&env).remove(0);
    assert!(node == n1 || node == n2);
    assert_eq!(
        op,
        TaskOp::HaCheck {
            check: "mgmt".into(),
            action: HaCheckAction::Warn,
            vms: vec![],
            detail: "failing on 2 of 3 nodes; treating it as a shared failure".into(),
        }
    );
    assert_eq!(status, succeeded());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(fence.calls().is_empty());
    assert_eq!(check_tasks(&env).len(), 1);
    // n2가 회복한다: n1만 실패하므로 grace(1초) 뒤에 끈다.
    renew_with(&env.cluster, n2, vec![passing("mgmt")]).await;
    let recovered = std::time::Instant::now();
    eventually("powered off", || !fence.calls().is_empty()).await;
    assert!(recovered.elapsed() >= Duration::from_millis(900));
    assert_eq!(fence.calls(), [("n1".into(), "ok".into())]);
    manager.shutdown().await;
}

/// 브리지 `vmbr0`에 묶인 `eth0`(VM 네트워크)를 가진 계획 52 노드
async fn vm_net_node(env: &Env, name: &str, memory_mib: u64) -> Uuid {
    fast_node_with(env, name, None, 60, |n| {
        n.status.memory_mib = memory_mib;
        n.status.bridges = vec![BridgeInfo {
            name: "vmbr0".into(),
            vlan_filtering: false,
        }];
        n.status.interfaces = vec![
            NodeInterface {
                name: "eth0".into(),
                kind: InterfaceKind::Ethernet,
                state: "up".into(),
                carrier: Some(true),
                speed_mbps: None,
                mac: None,
                master: Some("vmbr0".into()),
                addresses: vec![],
                bond: None,
            },
            NodeInterface {
                name: "vmbr0".into(),
                kind: InterfaceKind::Bridge,
                state: "up".into(),
                carrier: Some(true),
                speed_mbps: None,
                mac: None,
                master: None,
                addresses: vec![],
                bond: None,
            },
        ];
    })
    .await
}

/// VM 네트워크 `lan`(브리지 `vmbr0`)
async fn lan(env: &Env) {
    let network = Resource::new(
        "lan",
        NetworkSpec {
            bridge: Some("vmbr0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    env.cluster.create(&network).await.unwrap();
}

fn on_lan(spec: &mut VmSpec) {
    spec.nics = serde_json::from_value(json!([{ "network": "lan", "model": "virtio" }])).unwrap();
}

/// `vm` 검사의 `migrate`: 그 브리지를 쓰는 HA VM만 이름 순으로, 원본 노드마다 `ha_start_concurrency`개까지 같은
/// 검사가 실패하지 않는 노드로 옮기고 옮긴 기록을 더한다. 창 안에서 `max_relocate`에 닿은 VM은 오류만 남긴다.
#[tokio::test]
async fn a_vm_network_check_migrates_the_ha_vms_on_its_bridge() {
    let env = Env::start().await;
    lan(&env).await;
    let src = vm_net_node(&env, "src", 8192).await;
    // 메모리가 가장 많지만 같은 검사가 실패한다.
    let bad = vm_net_node(&env, "bad", 64000).await;
    let ok1 = vm_net_node(&env, "ok1", 16000).await;
    let ok2 = vm_net_node(&env, "ok2", 16000).await;
    set_start_concurrency(&env, 2).await;
    let ha = || Some(VmHa::default());
    let a = vm_with(&env, "a", src, ha(), PowerState::Running, on_lan).await;
    let b = vm_with(&env, "b", src, ha(), PowerState::Running, on_lan).await;
    let c = vm_with(&env, "c", src, ha(), PowerState::Running, on_lan).await;
    let e = vm_with(&env, "e", src, ha(), PowerState::Running, on_lan).await;
    // 브리지를 쓰지 않는 HA VM과 HA가 아닌 VM은 옮기지 않는다.
    let quiet = vm(&env, "quiet", src, true, PowerState::Running).await;
    let plain = vm_with(&env, "plain", src, None, PowerState::Running, on_lan).await;
    // c는 창 안에서 이미 한 번(max_relocate) 옮겼다.
    let mut status = c.status.clone();
    status.ha = Some(VmHaStatus {
        relocations: vec![unix_now()],
        ..Default::default()
    });
    env.cluster
        .update_status::<VmSpec>(c.id, &status)
        .await
        .unwrap();
    ha_check(&env, "vmnet", HaCheckRole::Vm, link("eth0"), None).await;
    for (node, report) in [
        (src, failing("vmnet")),
        (bad, failing("vmnet")),
        (ok1, passing("vmnet")),
        (ok2, passing("vmnet")),
    ] {
        renew_with(&env.cluster, node, vec![report]).await;
    }
    let started = unix_now();
    let manager = fast_manager(&env);
    eventually("two migrations", || {
        migrate_tasks(&env)
            .iter()
            .filter(|(_, from, _)| *from == src)
            .count()
            == 2
    })
    .await;
    // 원본 노드마다 둘까지: e는 앞 Task가 끝날 때까지 기다린다(시험의 Task는 실행되지 않는다).
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut tasks = migrate_tasks(&env);
    tasks.sort_by_key(|(vm, _, _)| get(&env, *vm).name);
    assert_eq!(tasks.len(), 2, "{tasks:?}");
    assert_eq!(tasks[0].0, a.id);
    assert_eq!(tasks[1].0, b.id);
    for (_, from, to) in &tasks {
        assert_eq!(*from, src);
        assert!(*to == ok1 || *to == ok2, "migrated to {to}");
    }
    for id in [a.id, b.id] {
        let ha = get(&env, id).status.ha.unwrap();
        assert_eq!(ha.relocations.len(), 1);
        assert!(ha.relocations[0] >= started);
        assert_eq!(ha.relocated_from, Some(src));
    }
    assert_eq!(
        get(&env, c.id).status.ha.unwrap().error.as_deref(),
        Some("HA check vmnet failed; not moving the VM: gave up after 1 relocations")
    );
    for v in [&e, &quiet, &plain] {
        assert_eq!(get(&env, v.id).status.ha, None, "{}", v.name);
    }
    let records: Vec<TaskOp> = check_tasks(&env)
        .into_iter()
        .filter(|(node, _, _)| *node == src)
        .map(|(_, op, _)| op)
        .collect();
    assert_eq!(
        records,
        [TaskOp::HaCheck {
            check: "vmnet".into(),
            action: HaCheckAction::Migrate,
            vms: vec!["a".into(), "b".into()],
            detail: "vmnet is down; also affects 1 VMs without HA".into(),
        }]
    );
    manager.shutdown().await;
}

/// `ha-checks`를 모르는 노드가 남아 있으면 대응하지 않는다(기록 Task·마이그레이션 없이 경고 로그만).
#[tokio::test]
async fn an_old_node_keeps_failing_checks_from_acting() {
    let env = Env::start().await;
    lan(&env).await;
    let src = vm_net_node(&env, "src", 8192).await;
    let ok = vm_net_node(&env, "ok", 16000).await;
    node(&env, "old", 8192, Some(60)).await;
    vm_with(
        &env,
        "a",
        src,
        Some(VmHa::default()),
        PowerState::Running,
        on_lan,
    )
    .await;
    ha_check(&env, "vmnet", HaCheckRole::Vm, link("eth0"), None).await;
    renew_with(&env.cluster, src, vec![failing("vmnet")]).await;
    renew_with(&env.cluster, ok, vec![passing("vmnet")]).await;
    let manager = fast_manager(&env);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(migrate_tasks(&env).is_empty());
    assert!(check_tasks(&env).is_empty());
    manager.shutdown().await;
}

/// 공유 dir 풀 `nas`와 그 풀의 볼륨 `disk-<vm>`을 만든다.
async fn nas(env: &Env, vms: &[&str]) {
    let pool = Resource::new(
        "nas",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/srv/nas".into(),
            },
            nodes: vec![],
            shared: true,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    for vm in vms {
        let volume = Resource::new(
            format!("disk-{vm}"),
            VolumeSpec {
                pool: "nas".into(),
                size_bytes: 1 << 30,
                format: ImageFormat::Qcow2,
            },
        );
        env.cluster.create(&volume).await.unwrap();
    }
}

/// 디스크 `disk-<name>`(풀 `nas`)을 가진 VM
fn on_nas(name: &str) -> impl FnOnce(&mut VmSpec) {
    let disk = json!([{ "volume": format!("disk-{name}"), "bus": "virtio-blk" }]);
    move |spec: &mut VmSpec| spec.disks = serde_json::from_value(disk).unwrap()
}

/// 노드 장애 복구는 `storage` 검사가 실패한 노드로 그 풀의 VM을 옮기지 않는다(그 풀을 쓰지 않는 VM은 간다).
#[tokio::test]
async fn recovery_avoids_a_node_failing_a_storage_check() {
    let env = Env::start().await;
    nas(&env, &["on-nas"]).await;
    let failed = fast_node(&env, "failed", None, 1).await;
    let bad = fast_node_with(&env, "bad", None, 60, |n| n.status.memory_mib = 64000).await;
    let good = fast_node(&env, "good", None, 60).await;
    let ha = || Some(VmHa::default());
    let on = vm_with(
        &env,
        "on-nas",
        failed,
        ha(),
        PowerState::Running,
        on_nas("on-nas"),
    )
    .await;
    let local = vm(&env, "local", failed, true, PowerState::Running).await;
    ha_check(
        &env,
        "nas",
        HaCheckRole::Storage,
        HaCheckTarget::Pool { name: "nas".into() },
        Some(HaCheckAction::Warn),
    )
    .await;
    renew_with(&env.cluster, bad, vec![failing("nas")]).await;
    renew_with(&env.cluster, good, vec![passing("nas")]).await;
    let manager = fast_manager(&env);
    eventually("HA VMs moved", || recover_tasks(&env).len() == 2).await;
    assert_eq!(get(&env, on.id).spec.node, Some(good));
    assert_eq!(get(&env, local.id).spec.node, Some(bad));
    manager.shutdown().await;
}

/// 재시작 횟수를 넘긴 VM 옮기기와 그룹의 더 우선인 노드로 되돌아가기도 `storage` 검사가 실패한 노드를 고르지 않는다.
#[tokio::test]
async fn relocation_and_failback_avoid_a_node_failing_a_storage_check() {
    let env = Env::start().await;
    nas(&env, &["r", "f"]).await;
    let from = fast_node(&env, "from", None, 60).await;
    let bad = fast_node_with(&env, "bad", None, 60, |n| n.status.memory_mib = 64000).await;
    let good = fast_node(&env, "good", None, 60).await;
    group(&env, "g", &[("bad", 5), ("good", 1)], false, false).await;
    let r = vm_with(
        &env,
        "r",
        from,
        Some(VmHa::default()),
        PowerState::Running,
        on_nas("r"),
    )
    .await;
    let f = vm_with(
        &env,
        "f",
        good,
        in_group("g"),
        PowerState::Running,
        on_nas("f"),
    )
    .await;
    let f2 = vm_ha(&env, "f2", good, in_group("g"), PowerState::Running).await;
    ha_check(
        &env,
        "nas",
        HaCheckRole::Storage,
        HaCheckTarget::Pool { name: "nas".into() },
        Some(HaCheckAction::Warn),
    )
    .await;
    renew_with(&env.cluster, bad, vec![failing("nas")]).await;
    renew_with(&env.cluster, good, vec![passing("nas")]).await;
    renew_with(&env.cluster, from, vec![passing("nas")]).await;
    ask_relocate(&env, r.id, from).await;
    let manager = failback_manager(&env, Duration::from_millis(200));
    eventually("relocated", || get(&env, r.id).spec.node != Some(from)).await;
    assert_eq!(get(&env, r.id).spec.node, Some(good));
    // f2(그 풀을 쓰지 않음)만 되돌아간다.
    eventually("failback task", || !migrate_tasks(&env).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(migrate_tasks(&env), [(f2.id, good, bad)]);
    assert_eq!(get(&env, f.id).spec.node, Some(good));
    manager.shutdown().await;
}

/// `restart_elsewhere` 끄기 요청(`ha.evict`)이 노드 `node`에서 온 것인지
fn evicted_from(env: &Env, id: Uuid, node: Uuid) -> bool {
    get(env, id)
        .status
        .ha
        .and_then(|ha| ha.evict)
        .is_some_and(|e| e.from == node)
}

/// 그 노드의 reconciler 대신: QEMU를 끄고 요청을 거두며 옮겨 달라고 한다.
async fn stopped_for_check(env: &Env, id: Uuid, node: Uuid, check: &str) {
    let mut status = get(env, id).status;
    status.phase = VmPhase::Failed;
    status.pid = None;
    status.last_error = Some(format!(
        "HA check {check} failed on this node; restarting elsewhere"
    ));
    let mut ha = status.ha.unwrap_or_default();
    ha.relocate_from = Some(node);
    ha.evict = None;
    status.ha = Some(ha);
    env.cluster
        .update_status::<VmSpec>(id, &status)
        .await
        .unwrap();
}

/// `storage` 검사(기본 대응 `restart_elsewhere`): grace 뒤 그 풀에 디스크가 있는 HA VM에만 끄기 요청을 쓰고(HA
/// 아닌 VM은 기록 Task의 수만), 창 안에서 `max_relocate`에 닿은 VM은 요청 없이 오류만 남긴다. 노드가 끄고 거두면
/// 같은 검사가 통과한 노드로 옮기고 옮긴 기록을 하나 더한다.
#[tokio::test]
async fn a_failing_storage_check_restarts_the_ha_vms_of_its_pool_elsewhere() {
    let env = Env::start().await;
    nas(&env, &["on-nas", "plain", "capped"]).await;
    let src = fast_node(&env, "src", None, 600).await;
    let dst = fast_node(&env, "dst", None, 600).await;
    let ha = || Some(VmHa::default());
    let run = PowerState::Running;
    let on = vm_with(&env, "on-nas", src, ha(), run, on_nas("on-nas")).await;
    let plain = vm_with(&env, "plain", src, None, run, on_nas("plain")).await;
    let local = vm(&env, "local", src, true, run).await;
    let capped = vm_with(&env, "capped", src, ha(), run, on_nas("capped")).await;
    let mut status = capped.status.clone();
    status.ha = Some(VmHaStatus {
        relocations: vec![unix_now()],
        ..Default::default()
    });
    env.cluster
        .update_status::<VmSpec>(capped.id, &status)
        .await
        .unwrap();
    ha_check(
        &env,
        "nas",
        HaCheckRole::Storage,
        HaCheckTarget::Pool { name: "nas".into() },
        None,
    )
    .await;
    renew_with(&env.cluster, src, vec![failing("nas")]).await;
    renew_with(&env.cluster, dst, vec![passing("nas")]).await;
    let manager = fast_manager(&env);
    eventually("evict requested", || evicted_from(&env, on.id, src)).await;
    let evict = get(&env, on.id).status.ha.unwrap().evict.unwrap();
    assert_eq!(evict.check, "nas");
    assert_eq!(evict.reason, "HA check nas failed: nas is down");
    eventually("recorded", || !check_tasks(&env).is_empty()).await;
    assert_eq!(
        check_tasks(&env),
        [(
            src,
            TaskOp::HaCheck {
                check: "nas".into(),
                action: HaCheckAction::RestartElsewhere,
                vms: vec!["on-nas".into()],
                detail: "nas is down; also affects 1 VMs without HA".into(),
            },
            succeeded(),
        )]
    );
    assert_eq!(
        get(&env, capped.id).status.ha.unwrap(),
        VmHaStatus {
            relocations: status.ha.unwrap().relocations,
            error: Some(
                "HA check nas failed; not moving the VM: gave up after 1 relocations".into()
            ),
            ..Default::default()
        }
    );
    for v in [&plain, &local] {
        assert_eq!(get(&env, v.id).status.ha, None, "{}", v.name);
    }
    // 노드가 끄기 전에는 옮기지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(get(&env, on.id).spec.node, Some(src));
    assert_eq!(get(&env, capped.id).spec.node, Some(src));

    stopped_for_check(&env, on.id, src, "nas").await;
    eventually("relocated", || get(&env, on.id).spec.node == Some(dst)).await;
    let moved = get(&env, on.id);
    assert_eq!(moved.spec.power, PowerState::Running);
    let ha = moved.status.ha.unwrap();
    assert_eq!(ha.relocations.len(), 1);
    assert_eq!(ha.relocated_from, Some(src));
    assert_eq!(ha.evict, None);
    assert_eq!(
        moved.status.last_error.as_deref(),
        Some("relocating: HA check nas failed on this node; restarting elsewhere")
    );
    // 다시 요청하거나 기록하지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(check_tasks(&env).len(), 1);
    assert_eq!(get(&env, capped.id).spec.node, Some(src));
    // 검사가 회복하면 옮기지 않는다는 오류를 지운다.
    renew_with(&env.cluster, src, vec![passing("nas")]).await;
    eventually("HA error cleared", || {
        get(&env, capped.id)
            .status
            .ha
            .is_some_and(|h| h.error.is_none())
    })
    .await;
    assert_eq!(get(&env, capped.id).spec.node, Some(src));
    manager.shutdown().await;
}

/// 노드가 `EVICT_TIMEOUT`(20초) 안에 끄기 요청을 거두지 않으면 전원 끄기 펜싱이 있는 노드는 노드마다 한 번(VM
/// 둘에 요청 하나·기록 Task 하나) 끄고 그 HA VM을 복구한다. 없는 노드는 실패 기록 Task만 남기고 요청을 그대로
/// 둔다: 나중에 거두면 그때 옮긴다.
#[tokio::test]
async fn a_node_that_does_not_stop_the_vm_is_powered_off_or_left_alone() {
    let env = Env::start().await;
    nas(&env, &["a", "a2", "b"]).await;
    let fenced = fast_node(&env, "fenced", None, 600).await;
    let nofence = fast_node(&env, "nofence", None, 600).await;
    let s1 = fast_node(&env, "s1", None, 600).await;
    let s2 = fast_node(&env, "s2", None, 600).await;
    fence_node(&env, fenced, "fenced", &["ok"]).await;
    let ha = || Some(VmHa::default());
    let run = PowerState::Running;
    let a = vm_with(&env, "a", fenced, ha(), run, on_nas("a")).await;
    let a2 = vm_with(&env, "a2", fenced, ha(), run, on_nas("a2")).await;
    let b = vm_with(&env, "b", nofence, ha(), run, on_nas("b")).await;
    ha_check(
        &env,
        "nas",
        HaCheckRole::Storage,
        HaCheckTarget::Pool { name: "nas".into() },
        None,
    )
    .await;
    // 넷 가운데 둘이 실패하는 것은 과반이 아니다.
    for (node, report) in [
        (fenced, failing("nas")),
        (nofence, failing("nas")),
        (s1, passing("nas")),
        (s2, passing("nas")),
    ] {
        renew_with(&env.cluster, node, vec![report]).await;
    }
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("evict requested", || {
        evicted_from(&env, a.id, fenced)
            && evicted_from(&env, a2.id, fenced)
            && evicted_from(&env, b.id, nofence)
    })
    .await;
    let requested = std::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(18)).await;
    assert!(fence.calls().is_empty());
    eventually("powered off", || !fence.calls().is_empty()).await;
    assert!(requested.elapsed() >= Duration::from_secs(19));
    assert_eq!(fence.calls(), [("fenced".into(), "ok".into())]);
    eventually("HA VMs recovered", || {
        [a.id, a2.id].iter().all(|id| {
            let node = get(&env, *id).spec.node;
            node == Some(s1) || node == Some(s2)
        })
    })
    .await;
    eventually("power-off recorded", || power_off_tasks(&env).len() == 1).await;
    assert_eq!(
        power_off_tasks(&env)[0].1,
        TaskOp::NodePowerOff {
            method: Some(FenceKind::Command),
            reason: "HA check nas failed and the node did not stop VMs a, a2".into(),
        }
    );
    let escalated: Vec<_> = check_tasks(&env)
        .into_iter()
        .filter(|(_, op, _)| matches!(op, TaskOp::HaCheck { detail, .. } if detail.starts_with("the node did not")))
        .collect();
    assert_eq!(escalated.len(), 2, "{escalated:?}");
    let detail = "the node did not stop VMs a, a2 within 20s; requested a power-off of the node";
    assert!(escalated.contains(&(
        fenced,
        TaskOp::HaCheck {
            check: "nas".into(),
            action: HaCheckAction::RestartElsewhere,
            vms: vec!["a".into(), "a2".into()],
            detail: detail.into(),
        },
        succeeded(),
    )));
    let detail = "the node did not stop the VM and has no power fencing; leaving it";
    assert!(escalated.contains(&(
        nofence,
        TaskOp::HaCheck {
            check: "nas".into(),
            action: HaCheckAction::RestartElsewhere,
            vms: vec!["b".into()],
            detail: detail.into(),
        },
        TaskStatus {
            state: TaskState::Failed,
            error: Some(detail.into()),
            progress: None,
        },
    )));
    // 펜싱이 없는 노드의 VM은 요청을 남긴 채 그대로다.
    assert_eq!(get(&env, b.id).spec.node, Some(nofence));
    assert!(evicted_from(&env, b.id, nofence));
    // 나중에 노드가 끄고 거두면 옮긴다.
    stopped_for_check(&env, b.id, nofence, "nas").await;
    eventually("b relocated", || {
        let node = get(&env, b.id).spec.node;
        node == Some(s1) || node == Some(s2)
    })
    .await;
    assert_eq!(get(&env, b.id).status.ha.unwrap().relocations.len(), 1);
    manager.shutdown().await;
}

/// 받을 노드가 없는 VM에는 끄기 요청을 쓰지 않는다(끄면 켤 곳 없이 꺼진 채 남는다 — 경고만, 빈 기록 Task도 없음).
/// 앞 VM(앞서 요청한 VM 포함)이 갈 곳을 세므로, 떨어져야 하는 두 VM(anti-affinity)에 받을 노드가 하나면 첫 VM만
/// 요청한다. 받을 노드가 생기면 그때 요청한다.
#[tokio::test]
async fn restart_elsewhere_needs_a_node_that_can_take_the_vm() {
    let env = Env::start().await;
    nas(&env, &["a", "b"]).await;
    let src = fast_node(&env, "src", None, 600).await;
    // 다른 검사(`san`)가 실패하는 노드는 받을 노드가 아니다.
    let bad = fast_node(&env, "bad", None, 600).await;
    rule(&env, "apart", RuleKind::AntiAffinity, &["a", "b"], &[]).await;
    let ha = || Some(VmHa::default());
    let run = PowerState::Running;
    let a = vm_with(&env, "a", src, ha(), run, on_nas("a")).await;
    let b = vm_with(&env, "b", src, ha(), run, on_nas("b")).await;
    ha_check(
        &env,
        "nas",
        HaCheckRole::Storage,
        HaCheckTarget::Pool { name: "nas".into() },
        None,
    )
    .await;
    ha_check(&env, "san", HaCheckRole::Storage, link("eth9"), None).await;
    renew_with(&env.cluster, src, vec![failing("nas"), passing("san")]).await;
    renew_with(&env.cluster, bad, vec![passing("nas"), failing("san")]).await;
    let manager = fast_manager(&env);
    let nas_task = || {
        check_tasks(&env).iter().any(|(node, op, _)| {
            *node == src && matches!(op, TaskOp::HaCheck { check, .. } if check == "nas")
        })
    };
    // `san`의 경고 기록이 생기면 `nas`의 grace도 지났다.
    eventually("san recorded", || {
        check_tasks(&env)
            .iter()
            .any(|(_, op, _)| matches!(op, TaskOp::HaCheck { check, .. } if check == "san"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!evicted_from(&env, a.id, src));
    assert!(!evicted_from(&env, b.id, src));
    assert!(!nas_task());
    // 받을 노드가 하나 생기면 첫 VM만 요청한다(b는 a와 떨어져야 한다).
    let one = fast_node(&env, "one", None, 600).await;
    renew_with(&env.cluster, one, vec![passing("nas"), passing("san")]).await;
    eventually("evict requested", || evicted_from(&env, a.id, src)).await;
    eventually("recorded", nas_task).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!evicted_from(&env, b.id, src));
    let more = fast_node(&env, "more", None, 600).await;
    renew_with(&env.cluster, more, vec![passing("nas"), passing("san")]).await;
    eventually("second evict requested", || evicted_from(&env, b.id, src)).await;
    manager.shutdown().await;
}

/// 20초가 지나도 거두지 않은 끄기 요청은 이유를 다시 본다: 공유 실패로 바뀌었거나 검사를 지웠거나 대응을
/// `restart_elsewhere`가 아닌 것으로 바꿨으면 전원을 끄지 않고 요청을 거둔다(VM은 그 노드에서 그대로 돈다).
#[tokio::test]
async fn an_evict_request_whose_reason_is_gone_is_withdrawn_instead_of_powering_off() {
    let env = Env::start().await;
    nas(&env, &["a", "b", "c"]).await;
    let n1 = fast_node(&env, "n1", None, 600).await;
    let n2 = fast_node(&env, "n2", None, 600).await;
    let n3 = fast_node(&env, "n3", None, 600).await;
    let s1 = fast_node(&env, "s1", None, 600).await;
    let s2 = fast_node(&env, "s2", None, 600).await;
    fence_node(&env, n1, "n1", &["ok"]).await;
    fence_node(&env, n2, "n2", &["ok"]).await;
    fence_node(&env, n3, "n3", &["ok"]).await;
    let ha = || Some(VmHa::default());
    let run = PowerState::Running;
    let a = vm_with(&env, "a", n1, ha(), run, on_nas("a")).await;
    let b = vm_with(&env, "b", n2, ha(), run, on_nas("b")).await;
    let c = vm_with(&env, "c", n3, ha(), run, on_nas("c")).await;
    let pool = || HaCheckTarget::Pool { name: "nas".into() };
    ha_check(&env, "nas", HaCheckRole::Storage, pool(), None).await;
    ha_check(&env, "san", HaCheckRole::Storage, link("eth9"), None).await;
    ha_check(&env, "tank", HaCheckRole::Storage, link("eth8"), None).await;
    let report = |nas: bool, san: bool, tank: bool| {
        let r = |ok, name| if ok { passing(name) } else { failing(name) };
        vec![r(nas, "nas"), r(san, "san"), r(tank, "tank")]
    };
    for (node, checks) in [
        (n1, report(false, true, true)),
        (n2, report(true, false, true)),
        (n3, report(true, true, false)),
        (s1, report(true, true, true)),
        (s2, report(true, true, true)),
    ] {
        renew_with(&env.cluster, node, checks).await;
    }
    let fence = Arc::new(FakeFence::default());
    let manager = fence_manager(&env, fence.clone());
    eventually("evict requested", || {
        evicted_from(&env, a.id, n1) && evicted_from(&env, b.id, n2) && evicted_from(&env, c.id, n3)
    })
    .await;
    // `nas`가 모든 노드에서 실패한다(공유 실패), `san`은 지우고, `tank`는 경고만 하게 바꾼다.
    for node in [n1, n2, n3, s1, s2] {
        let san_ok = node != n2;
        let tank_ok = node != n3;
        renew_with(&env.cluster, node, report(false, san_ok, tank_ok)).await;
    }
    let tank = env
        .cluster
        .get_by_name::<HaCheckSpec>("tank")
        .unwrap()
        .unwrap();
    let warn = HaCheckSpec {
        on_fail: Some(HaCheckAction::Warn),
        ..tank.spec.clone()
    };
    env.cluster
        .update_spec(tank.id, Some(tank.revision), tank.labels.clone(), &warn)
        .await
        .unwrap();
    let san = env
        .cluster
        .get_by_name::<HaCheckSpec>("san")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<HaCheckSpec>(san.id, None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(19)).await;
    eventually("requests withdrawn", || {
        [a.id, b.id, c.id]
            .iter()
            .all(|id| get(&env, *id).status.ha.is_none())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(fence.calls().is_empty(), "{:?}", fence.calls());
    assert_eq!(get(&env, a.id).spec.node, Some(n1));
    assert_eq!(get(&env, b.id).spec.node, Some(n2));
    assert_eq!(get(&env, c.id).spec.node, Some(n3));
    manager.shutdown().await;
}

/// 넘긴 뒤에도 끄기 요청은 이유를 따라간다: 노드마다 한 번만 넘기고(같은 노드에 뒤에 낸 요청은 기록 Task·전원
/// 끄기 없이 넘긴 것으로만 둔다), 검사가 회복하면 넘긴 요청도 거둔다.
#[tokio::test]
async fn escalated_requests_are_escalated_once_per_node_and_withdrawn_on_recovery() {
    let env = Env::start().await;
    nas(&env, &["a", "b"]).await;
    let src = fast_node(&env, "src", None, 600).await;
    let s1 = fast_node(&env, "s1", None, 600).await;
    let s2 = fast_node(&env, "s2", None, 600).await;
    let ha = || Some(VmHa::default());
    let run = PowerState::Running;
    let a = vm_with(&env, "a", src, ha(), run, on_nas("a")).await;
    let pool = HaCheckTarget::Pool { name: "nas".into() };
    ha_check(&env, "nas", HaCheckRole::Storage, pool, None).await;
    for (node, report) in [
        (src, failing("nas")),
        (s1, passing("nas")),
        (s2, passing("nas")),
    ] {
        renew_with(&env.cluster, node, vec![report]).await;
    }
    let manager = fast_manager(&env);
    eventually("evict requested", || evicted_from(&env, a.id, src)).await;
    let escalations = || {
        check_tasks(&env)
            .into_iter()
            .filter(|(_, op, _)| {
                matches!(op, TaskOp::HaCheck { detail, .. } if detail.starts_with("the node did not"))
            })
            .count()
    };
    tokio::time::sleep(Duration::from_secs(19)).await;
    eventually("escalated", || escalations() == 1).await;
    // 같은 노드에 뒤에 낸 요청은 다시 넘기지 않는다.
    let b = vm_with(&env, "b", src, ha(), run, on_nas("b")).await;
    eventually("second evict requested", || evicted_from(&env, b.id, src)).await;
    tokio::time::sleep(Duration::from_secs(21)).await;
    assert_eq!(escalations(), 1);
    assert!(evicted_from(&env, a.id, src) && evicted_from(&env, b.id, src));
    // 회복하면 넘긴 요청도 거둔다.
    renew_with(&env.cluster, src, vec![passing("nas")]).await;
    eventually("requests withdrawn", || {
        [a.id, b.id]
            .iter()
            .all(|id| get(&env, *id).status.ha.is_none())
    })
    .await;
    assert_eq!(escalations(), 1);
    manager.shutdown().await;
}

/// HA 검사의 끄기 요청(`ha.evict`)이 있는 VM은 되돌아가기가 건드리지 않는다(요청이 사라지면 곧바로 되돌아간다 —
/// 거절로 60초를 기다리지 않는다).
#[tokio::test]
async fn failback_skips_a_vm_being_restarted_elsewhere() {
    let env = Env::start().await;
    let low = node(&env, "low", 8192, Some(60)).await;
    let high = node(&env, "high", 8192, Some(60)).await;
    group(&env, "g", &[("low", 1), ("high", 5)], false, false).await;
    let a = vm_ha(&env, "a-vm", low, in_group("g"), PowerState::Running).await;
    let mut status = a.status.clone();
    status.ha = Some(VmHaStatus {
        evict: Some(opl_types::vm::HaEvict {
            from: low,
            check: "nas".into(),
            reason: "HA check nas failed: nas is down".into(),
            at: unix_now(),
        }),
        ..Default::default()
    });
    env.cluster
        .update_status::<VmSpec>(a.id, &status)
        .await
        .unwrap();
    let renewer = keep_alive(&env, high);
    let manager = failback_manager(&env, Duration::from_millis(200));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(migrate_tasks(&env).is_empty());
    status.ha = None;
    env.cluster
        .update_status::<VmSpec>(a.id, &status)
        .await
        .unwrap();
    eventually("failback task", || !migrate_tasks(&env).is_empty()).await;
    assert_eq!(migrate_tasks(&env), [(a.id, low, high)]);
    renewer.abort();
    manager.shutdown().await;
}
