//! 리더의 HA 복구(`HaManager`): 단일 노드 클러스터의 저장소에 다른 노드를 직접 기록한다(멤버십에는
//! 없고 리스를 갱신하지 않는다). 리스가 끝난 노드의 HA VM만 이름 순서로 옮기는지, 대상 고르기,
//! status를 spec보다 먼저 쓰는지, 그 노드의 Task를 실패로 끝내고 기록 Task를 남기는지, 대상이 없을
//! 때, 리스가 살아 있는 노드는 옮기지 않는지, 펜싱 표시가 늘 지워지는지, Task 목록을 spec 쓰기 뒤에
//! 읽는지, 결과를 모르는 복구 쓰기 뒤에는 결과를 알 때까지 펜싱을 쥐는지(임기가 바뀌면 풀고 남은 단계를
//! 하는지), 쓰기 오류 뒤에는 펜싱을 푸는지. 배치 규칙과 HA 그룹(그룹 우선순위, restricted, affinity·
//! anti-affinity, 막는 hard 규칙과 `ha_may_violate`), `state: stopped`·`ignored`, 그룹의 더 우선인 노드로
//! 되돌아가기, `rule_violations` 갱신.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, WAIT, vm_request};
use opl_api::{HaManager, HaManagerConfig, RecoveryWriter};
use opl_cluster::types::{Command, LeaderRequest, NodeId, Rejection};
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use opl_types::placement::{
    HaGroupNode, HaGroupSpec, HaGroupStatus, PlacementRuleSpec, PlacementRuleStatus, RuleKind,
};
use opl_types::resource::{Kind, Resource};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{
    Arch, HaState, MigrationStatus, PowerState, Vm, VmHa, VmHaStatus, VmPhase, VmSpec, VmStatus,
};
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
        }),
        underlay: None,
        firewall: false,
        version: None,
        updates: None,
        features: vec![],
        cluster_addresses: vec![],
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
    let mut spec: VmSpec = serde_json::from_value(vm_request(name)["spec"].clone()).unwrap();
    spec.memory_mib = 2048;
    spec.nics.clear();
    spec.node = Some(node);
    spec.ha = ha;
    spec.power = power;
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
        .execute_as_leader(LeaderRequest::RenewLease { node: NodeId(node) })
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
