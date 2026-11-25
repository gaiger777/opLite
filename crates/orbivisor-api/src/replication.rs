//! 로컬 디스크 복제(계획 55)의 원본 쪽: 지금 복제 요청([`start_replication`])과 리더의 주기 복제
//! ([`ReplicationScheduler`]). 회차는 VM이 도는 노드의 `VmReplicate` Task가 하고(`tasks::vm_replicate`), 대상마다의
//! 상태는 `status.replication`에 남는다. 스케줄러는 리더만 `tick`마다 주기가 지난 대상의 Task를 만든다. 상태가
//! Raft의 `last_try`·`synced_at`에 있으므로 리더가 바뀌어도 이어진다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_types::node::{NODE_REPLICATION, Node, NodeSpec};
use orbivisor_types::replication::{ReplicaStatus, parse_interval};
use orbivisor_types::task::{Task, TaskOp, TaskSpec, TaskState};
use orbivisor_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::backups::now;
use crate::cluster::{missing_feature, require_feature};
use crate::error::ApiError;
use crate::resources::lookup;
use crate::snapshots::check_not_migrating;
use crate::state::AppState;
use crate::tasks::create_task;

/// VM `vm`을 노드 `node`로 복제할 `VmReplicate` Task를 VM이 도는 노드에 만든다. 두 요청이 함께 검사를 지나지
/// 않게 `vm_lock` 안에서 본다. 모든 노드가 복제를 알아야 하고(409 `cluster_upgrading`), `node`가 VM의 복제 대상이어야
/// 하고(400), VM이 실행 중이어야 하며(409 `vm_not_running`, 옮기는 중이면 409 `vm_busy`), 같은 대상의 복제 Task가
/// 끝나지 않았으면 409 `vm_busy`다. 만들면 그 대상의 `last_try`를 적는다(Task가 곧바로 실패해도 스케줄러가 주기 전에
/// 다시 만들지 않게). `scheduled`는 스케줄러가 부른 것이다(기록만 다르다).
pub(crate) async fn start_replication(
    state: &AppState,
    vm: Uuid,
    node: Uuid,
    scheduled: bool,
) -> Result<Task, ApiError> {
    let _lock = state.vm_lock.lock().await;
    require_feature(&state.cluster, NODE_REPLICATION)?;
    let vm = lookup::<VmSpec>(&state.cluster, &vm.to_string())?;
    let target = lookup::<NodeSpec>(&state.cluster, &node.to_string())?;
    if !vm.spec.replication.iter().any(|t| t.node == target.id) {
        return Err(ApiError::bad_request(format!(
            "node {:?} is not a replication target of VM {:?}",
            target.name, vm.name
        )));
    }
    check_not_migrating(&vm)?;
    // 라이브 복원 중(계획 68)에는 디스크 노드가 아직 다 옮기지 않은 대상이다.
    crate::snapshots::check_not_restoring(&vm)?;
    let running = vm.spec.power == PowerState::Running && vm.status.phase == VmPhase::Running;
    let Some(source) = vm.spec.node.filter(|_| running) else {
        return Err(ApiError::conflict(
            "vm_not_running",
            format!("VM {:?} is not running", vm.name),
        ));
    };
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if let Some(t) = tasks
        .iter()
        .find(|t| t.spec.target == vm.id && replicating_to(t, target.id))
    {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is already being replicated to node {:?} (task {})",
                vm.name, target.name, t.id
            ),
        ));
    }
    let task = create_task(
        &state.cluster,
        TaskOp::VmReplicate { node: target.id },
        vm.id,
        source,
    )
    .await?;
    tracing::info!(vm = %vm.name, node = %target.name, task = %task.id, scheduled, "replicating VM");
    let tried = now();
    if let Err(e) = update_replica(&state.cluster, vm.id, target.id, |r| {
        r.last_try = Some(tried)
    })
    .await
    {
        tracing::warn!(vm = %vm.name, error = %e, "cannot record the replication attempt");
    }
    Ok(task)
}

/// 노드 `node`로 복제하는 끝나지 않은 Task인지
fn replicating_to(t: &Task, node: Uuid) -> bool {
    !t.status.state.is_finished() && t.spec.op == TaskOp::VmReplicate { node }
}

/// VM `vm`의 `status.replication`에서 노드 `node`의 항목(없으면 `pending`으로 새로)을 `f`로 고쳐 쓴다(읽은
/// revision 확인, 엇갈리면 다시 읽어 쓴다). VM이 없거나 `node`가 더는 복제 대상이 아니거나 바뀐 것이 없으면 쓰지
/// 않는다. reconciler·HA·Task 실행기가 함께 쓰는 status라 다른 쪽의 값을 덮지 않는다.
pub(crate) async fn update_replica(
    cluster: &Cluster,
    vm: Uuid,
    node: Uuid,
    f: impl Fn(&mut ReplicaStatus),
) -> Result<(), ClusterError> {
    loop {
        let Some(current) = cluster.get::<VmSpec>(vm)? else {
            return Ok(());
        };
        if !current.spec.replication.iter().any(|t| t.node == node) {
            return Ok(());
        }
        let mut status = current.status.clone();
        let i = match status.replication.iter().position(|r| r.node == node) {
            Some(i) => i,
            None => {
                status.replication.push(ReplicaStatus::pending(node));
                status.replication.len() - 1
            }
        };
        f(&mut status.replication[i]);
        if status.replication == current.status.replication {
            return Ok(());
        }
        match cluster
            .update_status_checked::<VmSpec>(vm, current.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

/// 스펙에서 뺀 대상이나 없는 노드의 항목(HA 복구 뒤의 옛 원본 `stale`도 — 승격을 기다리는 복제본 말고)을 지운다.
/// 읽은 revision을 확인한다.
async fn prune(cluster: &Cluster, vm: Uuid) -> Result<(), ClusterError> {
    loop {
        let Some(current) = cluster.get::<VmSpec>(vm)? else {
            return Ok(());
        };
        let (_, nodes) = cluster.list::<NodeSpec>()?;
        let mut status = current.status.clone();
        status.replication.retain(|r| kept(r, &current, &nodes));
        if status.replication == current.status.replication {
            return Ok(());
        }
        match cluster
            .update_status_checked::<VmSpec>(vm, current.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

/// 남기는 항목: 스펙의 대상이고 그 노드가 아직 있는 항목(HA 복구 뒤의 옛 원본 `stale`은 뒤집힌 대상이다)과, HA 복구가
/// 승격하라고 표시한 복제본(대상에서 이미 빠졌다 — 그 노드가 승격하며 지운다)
fn kept(r: &ReplicaStatus, vm: &Vm, nodes: &[Node]) -> bool {
    r.promote
        || (vm.spec.replication.iter().any(|t| t.node == r.node)
            && nodes.iter().any(|n| n.id == r.node))
}

#[derive(Debug, Clone)]
pub struct ReplicationSchedulerConfig {
    /// 주기가 지난 대상을 확인하는 간격
    pub tick: Duration,
    /// 원본 노드마다 끝나지 않은 복제 Task의 최대 수
    pub max_concurrent: usize,
    /// 일정(`*/N`)의 한 분. 초 단위로 센다(`last_try`·`synced_at`이 초다). 시험만 줄인다.
    pub minute: Duration,
}

impl Default for ReplicationSchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            max_concurrent: 2,
            minute: Duration::from_secs(60),
        }
    }
}

/// 주기 복제 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct ReplicationScheduler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ReplicationScheduler {
    pub fn spawn(state: Arc<AppState>, config: ReplicationSchedulerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    state: Arc<AppState>,
    config: ReplicationSchedulerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config) => {}
        }
    }
}

/// 스케줄러가 한 틱에 할 일([`plan`])
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// 스펙에서 뺀 대상의 항목이 남은 VM
    prune: Vec<Uuid>,
    /// 대상 노드가 오프라인이라 Task 없이 항목에 남길 오류(VM, 대상, 문구)
    offline: Vec<(Uuid, Uuid, String)>,
    /// 주기가 지난 대상(오래 기다린 것 먼저)
    due: Vec<Due>,
}

/// 주기가 지난 대상 하나
#[derive(Debug, Clone, PartialEq, Eq)]
struct Due {
    vm: Uuid,
    node: Uuid,
    /// VM이 도는 노드(동시 수를 센다)
    source: Uuid,
}

/// 복제가 있는 VM의 대상마다 할 일을 고른다(`now`·`minute`는 초). 모든 노드가 복제를 아는지는 부르는 쪽이 본다.
/// VM이 실행 중이고 옮기는 중이 아니며 VM의 노드가 온라인이고 유지보수 중이 아니고, 그 VM의 끝나지 않은 백업·
/// 마이그레이션·핫플러그·복제 Task가 없고(VM 줄에서 기다릴 Task를 더 쌓지 않는다), 대상 노드가 온라인이고(아니면
/// 오류만), 앞 시도(또는 마지막 회차) 뒤 주기가 지났으면 Task를 만들 대상이다. 앞 시도가 오래된 것 먼저다.
fn plan(now: u64, minute: u64, vms: &[Vm], nodes: &[Node], tasks: &[Task]) -> Plan {
    let mut out = Plan::default();
    let mut due = Vec::new();
    let node = |id: Uuid| nodes.iter().find(|n| n.id == id);
    for vm in vms {
        let stale = vm.status.replication.iter().any(|r| !kept(r, vm, nodes));
        if stale {
            out.prune.push(vm.id);
        }
        if vm.spec.replication.is_empty() {
            continue;
        }
        let running = vm.spec.power == PowerState::Running
            && vm.status.phase == VmPhase::Running
            && vm.status.migration.is_none();
        let Some(source) = vm.spec.node.filter(|_| running) else {
            continue;
        };
        if !node(source).is_some_and(|n| n.status.online && !n.spec.maintenance) {
            continue;
        }
        let queued = tasks.iter().any(|t| {
            t.spec.target == vm.id
                && !t.status.state.is_finished()
                && matches!(
                    t.spec.op,
                    TaskOp::VmBackup { .. }
                        | TaskOp::VmMigrate { .. }
                        | TaskOp::VmHotplug
                        | TaskOp::VmReplicate { .. }
                )
        });
        for target in &vm.spec.replication {
            let Some(t) = node(target.node).filter(|_| target.node != source) else {
                continue;
            };
            if !t.status.online {
                out.offline
                    .push((vm.id, t.id, format!("node {:?} is offline", t.name)));
                continue;
            }
            if queued {
                continue;
            }
            let Ok(every) = parse_interval(&target.schedule) else {
                continue;
            };
            let entry = vm.status.replication.iter().find(|r| r.node == t.id);
            let last_try = entry.and_then(|r| r.last_try);
            let last = last_try.max(entry.and_then(|r| r.synced_at)).unwrap_or(0);
            if now < last.saturating_add(u64::from(every) * minute) {
                continue;
            }
            due.push((
                last_try.unwrap_or(0),
                Due {
                    vm: vm.id,
                    node: t.id,
                    source,
                },
            ));
        }
    }
    due.sort_by_key(|(last_try, _)| *last_try);
    out.due = due.into_iter().map(|(_, d)| d).collect();
    out
}

async fn tick(state: &AppState, config: &ReplicationSchedulerConfig) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    // 옛 노드는 복제 Task와 복제 RPC를 모른다. 모든 노드가 올라오면 시작한다.
    if !missing_feature(cluster, NODE_REPLICATION).is_ok_and(|old| old.is_empty()) {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<VmSpec>()?.1,
            cluster.list::<NodeSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
        ))
    })();
    let Ok((vms, nodes, tasks)) = lists else {
        return;
    };
    let plan = plan(now(), config.minute.as_secs(), &vms, &nodes, &tasks);
    for vm in plan.prune {
        if let Err(e) = prune(cluster, vm).await {
            tracing::warn!(vm = %vm, error = %e, "cannot remove replication entries of removed targets");
        }
    }
    // 같은 문구면 쓰지 않는다(`update_replica`가 바뀐 것이 없으면 쓰지 않는다).
    for (vm, node, error) in plan.offline {
        if let Err(e) = update_replica(cluster, vm, node, |r| r.error = Some(error.clone())).await {
            tracing::warn!(vm = %vm, error = %e, "cannot record a replication error");
        }
    }
    let mut running = running_per_node(&tasks);
    for due in plan.due {
        let count = running.entry(due.source).or_default();
        if *count >= config.max_concurrent {
            continue;
        }
        match start_replication(state, due.vm, due.node, true).await {
            Ok(_) => *count += 1,
            // 검사에 걸렸다(그 사이 꺼졌거나 대상에서 뺐다). 다음 틱에 다시 본다.
            Err(e) if e.status.is_client_error() => {
                tracing::info!(vm = %due.vm, node = %due.node, error = %e.message, "scheduled replication refused");
            }
            Err(e) => {
                tracing::warn!(vm = %due.vm, node = %due.node, error = %e.message, "cannot request scheduled replication");
            }
        }
    }
}

/// 원본 노드마다 도는(`running`) 복제 Task 수. VM 줄에서 기다리는 Task는 세지 않는다(백업 뒤에 선 복제가 다른 VM의
/// 복제를 막지 않게).
fn running_per_node(tasks: &[Task]) -> BTreeMap<Uuid, usize> {
    let mut running: BTreeMap<Uuid, usize> = BTreeMap::new();
    for t in tasks.iter().filter(|t| {
        t.status.state == TaskState::Running && matches!(t.spec.op, TaskOp::VmReplicate { .. })
    }) {
        *running.entry(t.spec.node).or_default() += 1;
    }
    running
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbivisor_types::replication::{ReplicationState, ReplicationTarget};
    use orbivisor_types::resource::Resource;
    use orbivisor_types::task::{TaskState, TaskStatus};
    use orbivisor_types::vm::MigrationStatus;

    const MIN: u64 = 60;

    fn node(name: &str) -> Node {
        let mut n: Node = Resource::new(
            name,
            NodeSpec {
                sdn: None,
                host_resources: None,
                address: "127.0.0.1:0".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status.online = true;
        n
    }

    /// `source`에서 도는 VM. `targets`마다 `*/15`로 복제한다.
    fn vm(name: &str, source: &Node, targets: &[&Node]) -> Vm {
        let mut spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt"
        }))
        .unwrap();
        spec.node = Some(source.id);
        spec.power = PowerState::Running;
        spec.replication = targets
            .iter()
            .map(|t| ReplicationTarget {
                node: t.id,
                schedule: "*/15".into(),
                pool: None,
            })
            .collect();
        let mut vm: Vm = Resource::new(name, spec);
        vm.status.phase = VmPhase::Running;
        vm
    }

    fn entry(node: &Node, last_try: Option<u64>, synced_at: Option<u64>) -> ReplicaStatus {
        ReplicaStatus {
            last_try,
            synced_at,
            ..ReplicaStatus::pending(node.id)
        }
    }

    fn due(vm: &Vm, node: &Node, source: &Node) -> Due {
        Due {
            vm: vm.id,
            node: node.id,
            source: source.id,
        }
    }

    #[test]
    fn targets_are_due_after_their_interval_since_the_last_try_or_run() {
        let (a, b, c) = (node("a"), node("b"), node("c"));
        let nodes = [a.clone(), b.clone(), c.clone()];
        let mut web = vm("web", &a, &[&b, &c]);
        // b: 앞 시도 1000초 → 1000 + 15분 뒤. c: 시도는 옛것이지만 마지막 회차가 새롭다.
        web.status.replication = vec![
            entry(&b, Some(1000), Some(900)),
            entry(&c, Some(100), Some(1500)),
        ];
        let at = |now| plan(now, MIN, std::slice::from_ref(&web), &nodes, &[]).due;
        assert!(at(1000 + 15 * MIN - 1).is_empty());
        assert_eq!(at(1000 + 15 * MIN), [due(&web, &b, &a)]);
        assert_eq!(
            at(1500 + 15 * MIN),
            [due(&web, &c, &a), due(&web, &b, &a)],
            "the target tried longest ago comes first"
        );
        // 항목이 없으면(처음) 시각 0에서 센다 — 곧바로 한다.
        let fresh = vm("db", &a, &[&b]);
        assert_eq!(
            plan(15 * MIN, MIN, std::slice::from_ref(&fresh), &nodes, &[]).due,
            [due(&fresh, &b, &a)]
        );
        // 시험의 짧은 분(1초)
        assert_eq!(
            plan(1000 + 15, 1, std::slice::from_ref(&web), &nodes, &[]).due,
            [due(&web, &b, &a)]
        );
    }

    #[test]
    fn vms_that_cannot_replicate_now_are_skipped() {
        let (a, b) = (node("a"), node("b"));
        let nodes = [a.clone(), b.clone()];
        let base = vm("web", &a, &[&b]);
        let skipped = |vm: Vm, nodes: &[Node]| {
            let p = plan(1 << 40, MIN, &[vm], nodes, &[]);
            assert!(p.due.is_empty(), "{p:?}");
            assert!(p.offline.is_empty(), "{p:?}");
        };
        let mut stopped = base.clone();
        stopped.spec.power = PowerState::Stopped;
        stopped.status.phase = VmPhase::Stopped;
        skipped(stopped, &nodes);
        let mut migrating = base.clone();
        migrating.status.phase = VmPhase::Migrating;
        migrating.status.migration = Some(MigrationStatus {
            node: b.id,
            task: Uuid::now_v7(),
            disks: vec![],
        });
        skipped(migrating, &nodes);
        let mut maintenance = a.clone();
        maintenance.spec.maintenance = true;
        skipped(base.clone(), &[maintenance, b.clone()]);
        let mut down = a.clone();
        down.status.online = false;
        skipped(base.clone(), &[down, b.clone()]);
        // VM이 대상 노드로 옮겨 갔다(그 대상은 쓸모가 없다).
        let mut moved = base.clone();
        moved.spec.node = Some(b.id);
        skipped(moved, &nodes);
        let mut bad = base.clone();
        bad.spec.replication[0].schedule = "15".into();
        skipped(bad, &nodes);
        // 같은 대상의 복제 Task가 끝나지 않았다(다른 대상의 Task는 상관없다).
        let mut task: Task = Resource::new(
            "",
            TaskSpec {
                op: TaskOp::VmReplicate { node: b.id },
                target: base.id,
                node: a.id,
            },
        );
        let p = plan(
            1 << 40,
            MIN,
            std::slice::from_ref(&base),
            &nodes,
            &[task.clone()],
        );
        assert!(p.due.is_empty());
        task.status = TaskStatus {
            state: TaskState::Succeeded,
            error: None,
            progress: None,
        };
        let p = plan(
            1 << 40,
            MIN,
            std::slice::from_ref(&base),
            &nodes,
            std::slice::from_ref(&task),
        );
        assert_eq!(p.due, [due(&base, &b, &a)]);
        // VM 줄에서 기다리거나 도는 그 VM의 다른 Task(백업·마이그레이션·핫플러그·다른 대상의 복제)가 있으면 이번
        // 틱에는 만들지 않는다. 다른 VM의 Task는 상관없다.
        for op in [
            TaskOp::VmBackup {
                backup: Uuid::now_v7(),
            },
            TaskOp::VmHotplug,
            TaskOp::VmReplicate {
                node: Uuid::now_v7(),
            },
        ] {
            let mut other = task.clone();
            other.spec.op = op;
            other.status = TaskStatus::default();
            let p = plan(
                1 << 40,
                MIN,
                std::slice::from_ref(&base),
                &nodes,
                std::slice::from_ref(&other),
            );
            assert!(p.due.is_empty(), "{:?}", other.spec.op);
            other.spec.target = Uuid::now_v7();
            let p = plan(1 << 40, MIN, std::slice::from_ref(&base), &nodes, &[other]);
            assert_eq!(p.due, [due(&base, &b, &a)]);
        }
    }

    /// 동시 수는 도는 복제 Task만 센다(VM 줄에서 기다리는 것은 세지 않는다).
    #[test]
    fn only_running_replications_count_toward_the_limit() {
        let (a, b) = (node("a"), node("b"));
        let task = |node: &Node, state: TaskState| {
            let mut t: Task = Resource::new(
                "",
                TaskSpec {
                    op: TaskOp::VmReplicate { node: b.id },
                    target: Uuid::now_v7(),
                    node: node.id,
                },
            );
            t.status.state = state;
            t
        };
        let tasks = [
            task(&a, TaskState::Running),
            task(&a, TaskState::Pending),
            task(&a, TaskState::Succeeded),
            task(&b, TaskState::Pending),
        ];
        assert_eq!(running_per_node(&tasks), BTreeMap::from([(a.id, 1)]));
    }

    #[test]
    fn an_offline_target_gets_an_error_instead_of_a_task() {
        let (a, mut b) = (node("a"), node("b"));
        b.status.online = false;
        let web = vm("web", &a, &[&b]);
        let p = plan(
            1 << 40,
            MIN,
            std::slice::from_ref(&web),
            &[a, b.clone()],
            &[],
        );
        assert!(p.due.is_empty());
        assert_eq!(
            p.offline,
            [(web.id, b.id, "node \"b\" is offline".to_string())]
        );
    }

    #[test]
    fn entries_of_removed_targets_and_nodes_are_pruned_but_promoted_ones_stay() {
        let (a, b, c) = (node("a"), node("b"), node("c"));
        let nodes = [a.clone(), b.clone(), c.clone()];
        let mut web = vm("web", &a, &[&b]);
        web.status.replication = vec![entry(&b, None, None)];
        let prune =
            |web: &Vm, nodes: &[Node]| plan(0, MIN, std::slice::from_ref(web), nodes, &[]).prune;
        assert!(prune(&web, &nodes).is_empty());
        // HA 복구 뒤의 옛 원본(`stale`)은 뒤집힌 대상이면 남고, 대상이 아니면 지운다.
        web.status.replication.push(ReplicaStatus {
            state: ReplicationState::Stale,
            ..ReplicaStatus::pending(c.id)
        });
        assert_eq!(prune(&web, &nodes), [web.id]);
        web.spec.replication.push(ReplicationTarget {
            node: c.id,
            schedule: "*/15".into(),
            pool: None,
        });
        assert!(prune(&web, &nodes).is_empty());
        // 그 노드가 없어지면 지운다.
        assert_eq!(prune(&web, &[a.clone(), b.clone()]), [web.id]);
        web.spec.replication.pop();
        // HA 복구가 승격하라고 표시한 복제본(대상에서 이미 빠짐)은 승격할 때까지 남긴다.
        web.status.replication[1].state = ReplicationState::Ready;
        web.status.replication[1].promote = true;
        assert!(prune(&web, &nodes).is_empty());
        web.status.replication[1].promote = false;
        assert_eq!(prune(&web, &nodes), [web.id]);
        // 복제를 모두 뺀 VM도 지운다.
        web.spec.replication.clear();
        let p = plan(1 << 40, MIN, std::slice::from_ref(&web), &nodes, &[]);
        assert_eq!(p.prune, [web.id]);
        assert!(p.due.is_empty());
    }
}
