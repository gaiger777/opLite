//! 리더의 HA 복구(설계 2026-11-02-ha-fencing-witness-design.md §4.1·§4.2). 리더만 `tick`마다 노드의
//! 리스를 보고, 리스가 `recover_after`보다 오래 끝난 노드의 HA VM(실행하기로 된 것)을 다른 HA 가능
//! 노드로 옮긴다. 그 노드는 그사이 HA VM을 스스로 껐다(펜싱). 옮기는 동안 그 노드를 펜싱 중으로
//! 표시해 갱신을 거부한다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::types::{Command, LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterError, Leases, Typed};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::scheduler::{Inventory, node_fits};
use crate::state::AppState;

/// 옮길 노드를 찾지 못한 HA VM의 `last_error`
const NO_NODE: &str = "HA cannot find a node for this VM";

/// 실행 중인 다른 쪽(그 노드의 실행기, 정리 작업)과 엇갈려 Task를 실패로 끝내지 못했을 때 다시 읽는 횟수
const TASK_FAIL_ATTEMPTS: usize = 3;

#[derive(Debug, Clone)]
pub struct HaManagerConfig {
    /// 리스를 확인하는 간격
    pub tick: Duration,
}

impl Default for HaManagerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
        }
    }
}

/// HA 복구 반복 작업. 모든 노드(위트니스 빼고)에서 띄우고, 리더인 노드에서만 일한다.
pub struct HaManager {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl HaManager {
    pub fn spawn(state: Arc<AppState>, config: HaManagerConfig) -> Self {
        Self::spawn_with_writer(state, config, Arc::new(LeaderWriter))
    }

    /// 복구 쓰기 경로를 바꿔 끼워 띄운다(시험).
    pub fn spawn_with_writer(
        state: Arc<AppState>,
        config: HaManagerConfig,
        writer: Arc<dyn RecoveryWriter>,
    ) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, writer, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 리더로 본 임기와 그 임기에서 리더인 것을 처음 본 시각(기준점 b). 리스를 받은 적 없는 노드의
/// 리스는 b에 끝난 것으로 본다(리더가 바뀌면 리스 표가 비므로).
type Base = Option<(u64, Instant)>;

/// 결과를 모르는 복구 쓰기(`Timeout` 등). 리더 로그에 남아 나중에 커밋될 수 있으므로, 같은 임기에서는
/// 결과를 알 때까지 그 노드의 펜싱 표시를 지우지 않고 같은 쓰기(revision 검사라 다시 해도 된다)를 다시
/// 보낸다. 임기가 바뀌면 새 임기의 리더가 커밋한 인덱스가 살아남은 옛 쓰기를 포함하므로 지워도 된다
/// (spec 쓰기였으면 남은 단계는 `Moved`로 이어 간다).
struct Unknown {
    /// 쓰기가 실패로 돌아온 뒤 본 임기(쓰기의 임기보다 작지 않다)
    term: u64,
    vm: Uuid,
    command: Command,
    /// spec 쓰기였으면 옮긴 노드(결과를 안 뒤 남은 단계를 한다)
    moved_to: Option<Uuid>,
}

/// 결과를 모르는 쓰기가 있는 노드
type Unknowns = BTreeMap<Uuid, Unknown>;

/// 결과를 모른 채 임기 변경으로 정리한 spec 쓰기. 이 노드가 리더일 때 VM이 `to`로 가 있으면 남은 단계를
/// 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Moved {
    vm: Uuid,
    from: Uuid,
    to: Uuid,
}

async fn run(
    state: Arc<AppState>,
    config: HaManagerConfig,
    writer: Arc<dyn RecoveryWriter>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut base: Base = None;
    let mut unknown = Unknowns::new();
    let mut moved: Vec<Moved> = vec![];
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다. 멈추면 펜싱 표시는
        // `Fence`가 지운다. 쓰는 중이었거나 결과를 모르는 쓰기가 있으면 지우지 않는다(리더 자리는 곧
        // 없어지고 새 리더는 빈 표에서 다시 센다).
        let ctx = Ctx {
            cluster: &state.cluster,
            writer: writer.as_ref(),
        };
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(ctx, &mut base, &mut unknown, &mut moved) => {}
        }
    }
}

async fn tick(ctx: Ctx<'_>, base: &mut Base, unknown: &mut Unknowns, moved: &mut Vec<Moved>) {
    let cluster = ctx.cluster;
    for node in settled_by_term(unknown, ctx.writer.term(cluster), moved) {
        cluster.end_fence(NodeId(node));
    }
    if cluster.members().leader != Some(cluster.id()) {
        *base = None;
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let leases = cluster.leases();
    let now = Instant::now();
    let b = base_at(base, leases.term, now);
    finish_moved(ctx, moved).await;
    resend(ctx, unknown).await;
    let Ok(inv) = Inventory::load(cluster) else {
        return;
    };
    let me = cluster.id().0;
    let failed: Vec<Uuid> = inv
        .nodes
        .iter()
        .filter(|n| n.id != me && !unknown.contains_key(&n.id))
        .filter(|n| expired(now, b, &leases, n))
        .filter(|n| inv.vms.iter().any(|vm| recoverable(vm, n.id)))
        .map(|n| n.id)
        .collect();
    for node in failed {
        if let Some(u) = recover_node(ctx, leases.term, b, node).await {
            unknown.insert(node, u);
        }
    }
}

/// 임기 `term`의 기준점 b. 그 임기에서 처음이면(임기가 바뀌었거나 리더가 아니었으면) `now`로 잡는다.
fn base_at(base: &mut Base, term: u64, now: Instant) -> Instant {
    match *base {
        Some((t, b)) if t == term => b,
        _ => {
            *base = Some((term, now));
            now
        }
    }
}

/// 임기가 바뀌어 펜싱을 풀어도 되는 노드를 `unknown`에서 빼 돌려준다. spec 쓰기였으면 남은 단계를 하도록
/// `moved`에 옮긴다(그 쓰기는 커밋됐을 수도 버려졌을 수도 있다).
fn settled_by_term(unknown: &mut Unknowns, term: u64, moved: &mut Vec<Moved>) -> Vec<Uuid> {
    let nodes: Vec<Uuid> = unknown
        .iter()
        .filter(|(_, u)| u.term != term)
        .map(|(node, _)| *node)
        .collect();
    for node in &nodes {
        if let Some(Unknown {
            vm,
            moved_to: Some(to),
            ..
        }) = unknown.remove(node)
        {
            moved.push(Moved {
                vm,
                from: *node,
                to,
            });
        }
    }
    nodes
}

/// 임기 변경으로 정리한 spec 쓰기의 남은 단계. 장벽 뒤라 그 쓰기의 결과가 정해져 있다. VM이 그 노드로 가
/// 있으면 남은 단계를 하고, 아니면(쓰기가 버려졌거나, 다른 리더가 끝냈거나, 사용자가 옮겼거나, VM이 없다)
/// 그냥 지운다. 버려졌으면 VM은 아직 죽은 노드에 있어 평소대로 다시 옮긴다.
async fn finish_moved(ctx: Ctx<'_>, moved: &mut Vec<Moved>) {
    let mut kept = vec![];
    for m in moved.drain(..) {
        let (vm, failed) = match (
            ctx.cluster.get::<VmSpec>(m.vm),
            ctx.cluster.get::<NodeSpec>(m.from),
        ) {
            (Ok(vm), Ok(failed)) => (vm, failed),
            // 읽지 못하면 다음 tick에 다시 본다.
            _ => {
                kept.push(m);
                continue;
            }
        };
        if let (Some(vm), Some(failed)) = (vm, failed)
            && vm.spec.node == Some(m.to)
        {
            finish(ctx, &vm, &failed, m.to).await;
        }
    }
    *moved = kept;
}

/// 결과를 모르는 쓰기를 다시 보낸다. 결과를 알면(이번에 적용됐거나, 앞의 것이 커밋됐거나 다른 쓰기가
/// 이겨 revision이 다르다) 펜싱을 풀고, spec 쓰기였으면 남은 단계를 한다. 나머지는 다음 tick에 평소대로
/// VM을 다시 읽어 이어 간다.
async fn resend(ctx: Ctx<'_>, unknown: &mut Unknowns) {
    let nodes: Vec<Uuid> = unknown.keys().copied().collect();
    for node in nodes {
        let command = unknown[&node].command.clone();
        let result = ctx.writer.write(ctx.cluster, command).await;
        if !settled(&result) {
            tracing::warn!(node = %node, result = ?result, "HA recovery write has no result yet; keeping the node fenced");
            if let Some(u) = unknown.get_mut(&node) {
                u.term = ctx.writer.term(ctx.cluster);
            }
            continue;
        }
        let Some(u) = unknown.remove(&node) else {
            continue;
        };
        if let Some(target) = u.moved_to {
            let vm = ctx.cluster.get::<VmSpec>(u.vm);
            let failed = ctx.cluster.get::<NodeSpec>(node);
            if let (Ok(Some(vm)), Ok(Some(failed))) = (vm, failed)
                && vm.spec.node == Some(target)
            {
                finish(ctx, &vm, &failed, target).await;
            }
        }
        ctx.cluster.end_fence(NodeId(node));
    }
}

/// 쓰기의 결과를 안다(펜싱을 풀어도 된다): 적용됐거나(`Ok`), 상태 머신이 거부했거나(`Rejected`), 리더를
/// 잃었다(`NotLeader`). `NotLeader`여도 항목은 이미 로그에 들어갔을 수 있다. 하지만 리더를 잃었다는 것은
/// 임기가 바뀐다는 뜻이고, 다음 임기의 리더는 자기 임기의 첫 항목을 커밋한 뒤에야 — 곧 그 항목을 커밋하거나
/// 버린 뒤에야 — 갱신을 확인해 주며 그 인덱스를 돌려준다. 죽은 노드는 그것을 적용한 뒤에야 fresh가 되므로
/// 그 결과를 본 뒤에 VM을 켠다. 그 밖(`Timeout` 등)은 같은 임기의 리더 로그에 남아 이 리더가 나중에 커밋할 수
/// 있으므로, 그 사이 이 리더가 확인해 준 갱신이 그 항목을 포함하지 않을 수 있다.
fn settled(result: &Result<u64, ClusterError>) -> bool {
    matches!(
        result,
        Ok(_) | Err(ClusterError::Rejected(_) | ClusterError::NotLeader { .. })
    )
}

/// 노드 `node`를 펜싱 중으로 표시하고, 리스가 여전히 끝났으면 그 HA VM을 이름 순서로 옮긴다.
/// 결과를 모르는 쓰기가 생기면 펜싱 표시를 지우지 않고 그 쓰기를 돌려준다.
async fn recover_node(ctx: Ctx<'_>, term: u64, b: Instant, node: Uuid) -> Option<Unknown> {
    let cluster = ctx.cluster;
    let mut fence = Fence::begin(cluster, node);
    // 표시하기 직전에 받은 갱신이 있을 수 있으므로 표시한 뒤 다시 본다.
    let leases = cluster.leases();
    if leases.term != term {
        return None;
    }
    let Ok(mut inv) = Inventory::load(cluster) else {
        return None;
    };
    let failed = inv.nodes.iter().find(|n| n.id == node).cloned()?;
    if !expired(Instant::now(), b, &leases, &failed) {
        return None;
    }
    let mut vms: Vec<Vm> = inv
        .vms
        .iter()
        .filter(|vm| recoverable(vm, node))
        .cloned()
        .collect();
    vms.sort_by(|a, b| a.name.cmp(&b.name));
    let names: Vec<&str> = vms.iter().map(|vm| vm.name.as_str()).collect();
    tracing::warn!(node = %failed.name, vms = ?names, "recovering HA VMs from a failed node");
    for vm in &vms {
        let result = recover_vm(ctx, &mut fence, &mut inv, &leases, b, &failed, vm).await;
        if let Some((command, moved_to)) = fence.unknown.take() {
            tracing::warn!(vm = %vm.name, result = ?result, "HA recovery write has no result; keeping the node fenced");
            return Some(Unknown {
                term: ctx.writer.term(cluster),
                vm: vm.id,
                command,
                moved_to,
            });
        }
        match result {
            Ok(()) => {}
            // 그사이 바뀌었으면 다음 tick에 다시 본다.
            Err(ClusterError::Rejected(e)) => {
                tracing::info!(vm = %vm.name, error = %e, "HA recovery not applied; will retry");
            }
            // 리더를 잃었거나 쓰지 못한다. 나머지도 다음 tick(이나 새 리더)에 다시 한다.
            Err(e) => {
                tracing::warn!(vm = %vm.name, error = %e, "cannot recover HA VM; will retry");
                return None;
            }
        }
    }
    None
}

/// VM 하나를 옮긴다: status를 먼저 쓰고 spec의 노드를 바꾼 뒤, 죽은 노드의 그 VM Task를 실패로
/// 끝내고 기록 Task를 남긴다. 대상이 없으면 그 이유만 status에 남긴다.
async fn recover_vm(
    ctx: Ctx<'_>,
    fence: &mut Fence<'_>,
    inv: &mut Inventory,
    leases: &Leases,
    b: Instant,
    failed: &Node,
    vm: &Vm,
) -> Result<(), ClusterError> {
    let migrating = vm.status.migration.as_ref().map(|m| m.node);
    let now = Instant::now();
    // 스케줄러처럼 미할당 메모리가 가장 많은 노드, 같으면 id가 작은 노드다. 죽은 노드와 마이그레이션을
    // 받던 노드(같은 실행 디렉터리를 쓰는 QEMU가 남아 있을 수 있다)는 빼고, 리스가 살아 있어야 한다.
    let target = inv
        .nodes
        .iter()
        .filter(|n| n.id != failed.id && Some(n.id) != migrating)
        .filter(|n| alive(now, b, leases, n) && node_fits(&vm.spec, inv, n).is_ok())
        .map(|n| (inv.free_memory_mib(n, vm.id), n.id))
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, id)| id);
    let Some(target) = target else {
        if vm.status.last_error.as_deref() != Some(NO_NODE) {
            let status = VmStatus {
                last_error: Some(NO_NODE.into()),
                ..vm.status.clone()
            };
            update_status_checked::<VmSpec>(ctx, vm.id, vm.revision, &status).await?;
            tracing::warn!(vm = %vm.name, "HA cannot find a node for the VM");
        }
        return Ok(());
    };
    // 노드를 바꾸기 전에 실행 상태를 지운다. 사이에 리더가 바뀌면 새 리더가 다시 한다(아직 그 노드).
    let status = VmStatus {
        phase: VmPhase::Stopped,
        node: None,
        pid: None,
        migration: None,
        last_error: Some(format!("recovering: node {} failed", failed.name)),
        ..vm.status.clone()
    };
    let command = status_command::<VmSpec>(vm.id, vm.revision, &status)?;
    let revision = fence.write(ctx, command, None).await?;
    let mut spec = vm.spec.clone();
    spec.node = Some(target);
    let command = Command::UpdateSpec {
        kind: VmSpec::KIND,
        id: vm.id,
        expected_revision: Some(revision),
        labels: vm.labels.clone(),
        spec: serde_json::to_value(&spec)?,
    };
    fence.write(ctx, command, Some(target)).await?;
    // 다음 VM의 미할당 메모리에 센다.
    if let Some(moved) = inv.vms.iter_mut().find(|v| v.id == vm.id) {
        moved.spec.node = Some(target);
    }
    finish(ctx, vm, failed, target).await;
    Ok(())
}

/// 옮긴 VM의 남은 일: 죽은 노드의 그 VM Task를 실패로 끝내고 기록 Task를 남긴다.
async fn finish(ctx: Ctx<'_>, vm: &Vm, failed: &Node, target: Uuid) {
    // 그 노드의 끝나지 않은 Task가 남으면 VM 조작이 막힌다(`vm_busy`). spec을 쓴 뒤에 읽어야 그
    // 사이에 만든 Task도 본다.
    let error = format!("node {} failed; the VM was recovered by HA", failed.name);
    match ctx.cluster.list::<TaskSpec>() {
        Ok((_, tasks)) => {
            for task in tasks.iter().filter(|t| {
                t.spec.node == failed.id && t.spec.target == vm.id && !t.status.state.is_finished()
            }) {
                fail_task(ctx, task, &error).await;
            }
        }
        Err(e) => tracing::warn!(vm = %vm.name, error = %e, "cannot list tasks of a failed node"),
    }
    let mut record: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmHaRecover { from: failed.id },
            target: vm.id,
            node: target,
        },
    );
    record.name = record.id.to_string();
    // 실행기는 끝난 Task를 실행하지 않는다.
    record.status.state = TaskState::Succeeded;
    if let Err(e) = create(ctx, &record).await {
        tracing::warn!(vm = %vm.name, error = %e, "cannot record HA recovery task");
    }
}

/// Task를 실패로 끝낸다. 그사이 바뀌었으면 다시 읽어 끝나지 않았을 때만 다시 쓴다.
async fn fail_task(ctx: Ctx<'_>, task: &Task, error: &str) {
    let status = TaskStatus {
        state: TaskState::Failed,
        error: Some(error.into()),
        progress: None,
    };
    let mut revision = task.revision;
    for _ in 0..TASK_FAIL_ATTEMPTS {
        match update_status_checked::<TaskSpec>(ctx, task.id, revision, &status).await {
            Ok(_) => return,
            Err(e) => {
                tracing::info!(task = %task.id, error = %e, "cannot fail task of a failed node");
                match ctx.cluster.get::<TaskSpec>(task.id) {
                    Ok(Some(t)) if !t.status.state.is_finished() => revision = t.revision,
                    _ => return,
                }
            }
        }
    }
}

/// 노드 `node`에 배정된 HA VM으로 실행하기로 된 것
fn recoverable(vm: &Vm, node: Uuid) -> bool {
    vm.spec.node == Some(node) && vm.spec.ha && vm.spec.power == PowerState::Running
}

/// 노드의 마지막 리스 갱신(없으면 기준점 b) 뒤 지난 시간
fn lease_age(now: Instant, b: Instant, leases: &Leases, node: Uuid) -> Duration {
    let last = leases
        .renewed
        .get(&NodeId(node))
        .map_or(b, |renewed| (*renewed).max(b));
    now.saturating_duration_since(last)
}

/// HA 가능 노드이고 리스가 `recover_after`보다 오래 끝났다.
fn expired(now: Instant, b: Instant, leases: &Leases, node: &Node) -> bool {
    node.status.ha.is_some_and(|ha| {
        lease_age(now, b, leases, node.id) >= Duration::from_secs(ha.recover_after_secs)
    })
}

/// HA 가능 노드이고 리스가 살아 있다.
fn alive(now: Instant, b: Instant, leases: &Leases, node: &Node) -> bool {
    node.status.ha.is_some_and(|ha| {
        lease_age(now, b, leases, node.id) < Duration::from_secs(ha.recover_after_secs)
    })
}

/// 펜싱 중 표시. 버려질 때(오류, 리더를 잃음, 종료로 멈춤 포함) 지운다. 표시는 리스 표에 있어 임기가
/// 바뀌어도 남으므로 꼭 지워야 한다. 단, 복구 쓰기의 결과를 모르면(쓰는 중 멈춤, `Timeout` 등) 지우지
/// 않는다. 그 쓰기가 나중에 커밋될 수 있기 때문이다.
struct Fence<'a> {
    cluster: &'a Cluster,
    node: NodeId,
    /// 결과를 모르는 쓰기가 있다(쓰는 중이거나 결과를 모름).
    hold: bool,
    /// 결과를 모르는 쓰기와 그것이 spec 쓰기였으면 옮긴 노드
    unknown: Option<(Command, Option<Uuid>)>,
}

impl<'a> Fence<'a> {
    fn begin(cluster: &'a Cluster, node: Uuid) -> Self {
        let node = NodeId(node);
        cluster.begin_fence(node);
        Self {
            cluster,
            node,
            hold: false,
            unknown: None,
        }
    }

    /// 복구 쓰기. 결과를 알 때까지 표시를 쥔다.
    async fn write(
        &mut self,
        ctx: Ctx<'_>,
        command: Command,
        moved_to: Option<Uuid>,
    ) -> Result<u64, ClusterError> {
        self.hold = true;
        let result = ctx.writer.write(ctx.cluster, command.clone()).await;
        if settled(&result) {
            self.hold = false;
        } else {
            self.unknown = Some((command, moved_to));
        }
        result
    }
}

impl Drop for Fence<'_> {
    fn drop(&mut self) {
        if !self.hold {
            self.cluster.end_fence(self.node);
        }
    }
}

/// 복구 쓰기를 적용한다. 시험에서 결과를 모르는 쓰기(`Timeout`)와 임기 변경을 흉내 내려고 바꿔 끼운다.
#[async_trait]
pub trait RecoveryWriter: Send + Sync {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError>;

    /// 결과를 모르는 쓰기를 판정하는 임기(이 임기가 바뀌면 그 쓰기를 정리한다)
    fn term(&self, cluster: &Cluster) -> u64 {
        cluster.leases().term
    }
}

/// 복구 쓰기는 이 노드가 리더일 때만 한다. 다른 노드로 전달하지 않으므로 리더를 잃었으면 `NotLeader`로
/// 끝난다. 펜싱 표시는 이 노드의 리스 표에만 있어, 새 리더에 전달된 쓰기는 그 노드의 갱신(새 리더는
/// 거부하지 않는다)과 엇갈릴 수 있다.
struct LeaderWriter;

#[async_trait]
impl RecoveryWriter for LeaderWriter {
    async fn write(&self, cluster: &Cluster, command: Command) -> Result<u64, ClusterError> {
        cluster
            .execute_as_leader(LeaderRequest::Write { command })
            .await
    }
}

/// 복구에 쓰는 것: 클러스터와 쓰기 경로
#[derive(Clone, Copy)]
struct Ctx<'a> {
    cluster: &'a Cluster,
    writer: &'a dyn RecoveryWriter,
}

async fn write(ctx: Ctx<'_>, command: Command) -> Result<u64, ClusterError> {
    ctx.writer.write(ctx.cluster, command).await
}

fn status_command<S: ResourceSpec>(
    id: Uuid,
    expected_revision: u64,
    status: &S::Status,
) -> Result<Command, ClusterError> {
    Ok(Command::UpdateStatusIf {
        kind: S::KIND,
        id,
        expected_revision,
        status: serde_json::to_value(status)?,
    })
}

async fn update_status_checked<S: ResourceSpec>(
    ctx: Ctx<'_>,
    id: Uuid,
    expected_revision: u64,
    status: &S::Status,
) -> Result<u64, ClusterError> {
    write(ctx, status_command::<S>(id, expected_revision, status)?).await
}

async fn create<S: ResourceSpec>(ctx: Ctx<'_>, resource: &Typed<S>) -> Result<u64, ClusterError> {
    let command = Command::Create {
        kind: S::KIND,
        resource: serde_json::from_value(serde_json::to_value(resource)?)?,
    };
    write(ctx, command).await
}

#[cfg(test)]
mod tests {
    use opl_cluster::types::Rejection;

    use super::*;

    fn unknown_at(term: u64) -> Unknown {
        Unknown {
            term,
            vm: Uuid::now_v7(),
            command: Command::Delete {
                kind: VmSpec::KIND,
                id: Uuid::now_v7(),
                expected_revision: None,
            },
            moved_to: None,
        }
    }

    #[test]
    fn the_base_is_taken_again_when_the_term_changes() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(5);
        let mut base: Base = None;
        assert_eq!(base_at(&mut base, 3, t0), t0);
        // 같은 임기에서는 처음 본 시각 그대로다.
        assert_eq!(base_at(&mut base, 3, later), t0);
        // 임기가 바뀌면 다시 잡는다.
        let next = later + Duration::from_secs(5);
        assert_eq!(base_at(&mut base, 4, next), next);
        assert_eq!(base, Some((4, next)));
    }

    #[test]
    fn unknown_writes_of_another_term_are_settled() {
        let (old, same) = (Uuid::now_v7(), Uuid::now_v7());
        let mut unknown = Unknowns::from([(old, unknown_at(3)), (same, unknown_at(4))]);
        let mut moved = vec![];
        assert_eq!(settled_by_term(&mut unknown, 4, &mut moved), vec![old]);
        assert_eq!(unknown.keys().copied().collect::<Vec<_>>(), vec![same]);
        assert!(settled_by_term(&mut unknown, 4, &mut moved).is_empty());
        // 이 노드가 리더가 아니어도 임기가 바뀌면 푼다.
        assert_eq!(settled_by_term(&mut unknown, 5, &mut moved), vec![same]);
        assert!(unknown.is_empty());
        // status 쓰기였으면 남은 단계가 없다.
        assert!(moved.is_empty());
    }

    #[test]
    fn a_spec_write_settled_by_term_keeps_its_remaining_steps() {
        let (node, to) = (Uuid::now_v7(), Uuid::now_v7());
        let u = Unknown {
            moved_to: Some(to),
            ..unknown_at(3)
        };
        let vm = u.vm;
        let mut unknown = Unknowns::from([(node, u)]);
        let mut moved = vec![];
        assert_eq!(settled_by_term(&mut unknown, 4, &mut moved), vec![node]);
        assert!(unknown.is_empty());
        assert_eq!(moved, vec![Moved { vm, from: node, to }]);
    }

    #[test]
    fn only_ok_rejected_and_not_leader_are_settled() {
        assert!(settled(&Ok(1)));
        assert!(settled(&Err(ClusterError::Rejected(
            Rejection::RevisionMismatch {
                expected: 1,
                actual: 2
            }
        ))));
        assert!(settled(&Err(ClusterError::NotLeader {
            leader_id: None,
            leader_addr: None
        })));
        assert!(!settled(&Err(ClusterError::Timeout)));
        assert!(!settled(&Err(ClusterError::Raft("shutting down".into()))));
        assert!(!settled(&Err(ClusterError::Unreachable("x".into()))));
    }
}
