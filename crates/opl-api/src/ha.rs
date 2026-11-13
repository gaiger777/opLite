//! 리더의 HA 복구(설계 2026-11-02-ha-fencing-witness-design.md §4.1·§4.2). 리더만 `tick`마다 노드의
//! 리스를 보고, 리스가 `recover_after`보다 오래 끝난 노드의 HA VM(실행하기로 된 것)을 다른 HA 가능
//! 노드로 옮긴다. 그 노드는 그사이 HA VM을 스스로 껐다(펜싱). 옮기는 동안 그 노드를 펜싱 중으로
//! 표시해 갱신을 거부한다.
//!
//! 대상은 배치 규칙과 HA 그룹([`crate::placement`])을 따른다(설계 §8). 복구할 노드가 없을 때는 그룹의 더
//! 우선인 노드로 되돌아가고(라이브 마이그레이션, 한 번에 하나), 틱마다 자리 잡은 VM의 규칙 위반
//! (`rule_violations`)을 고친다. 재시작 횟수를 넘긴 VM(`status.ha.relocate_from`)은 다른 노드로 옮기고,
//! 창 안에서 `max_relocate`번 옮겼으면 끈다. 호스트 종료 정책 `freeze`로 얼린 오프라인 노드의 HA VM은 옮기지
//! 않는다(그 노드가 돌아와 켠다, `POST /nodes/{key}/unfreeze`가 표시를 지우면 옮긴다).
//!
//! 복구는 우선순위(`ha.priority`) 높은 VM부터, 같으면 이름 순이다. 대상 노드마다 이 리더가 옮겼고 아직
//! 켜지지 않은 VM이 `ha_start_concurrency`개면 그 노드로는 더 옮기지 않는다(그 VM은 다음 틱에 다시 본다).
//! 복구할 노드가 없을 때 `plan_every`마다 노드마다 "그 노드가 죽으면" 옮길 대상을 미리 골라 두고, 복구 때
//! 그 대상이 지금도 맞으면 그대로 쓴다(설계 §8.1).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::types::{Command, LeaderRequest, NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError, Leases, Typed};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{
    DEFAULT_HA_PRIORITY, HaState, PowerState, Vm, VmHaStatus, VmPhase, VmSpec, VmStatus,
};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::auth::unix_now;
use crate::placement::{self, Assign, Candidate};
use crate::scheduler::{Inventory, node_fits};
use crate::state::AppState;
use crate::vms::{RuleCheck, start_migration};

/// 옮길 노드를 찾지 못한 HA VM의 `last_error`
const NO_NODE: &str = "HA cannot find a node for this VM";

/// 옮긴 기록(`status.ha.relocations`)을 세는 창. reconciler가 재시작을 세는 창(`restart.window`)과 같다.
const RELOCATE_WINDOW: Duration = Duration::from_secs(600);

/// 실행 중인 다른 쪽(그 노드의 실행기, 정리 작업)과 엇갈려 Task를 실패로 끝내지 못했을 때 다시 읽는 횟수
const TASK_FAIL_ATTEMPTS: usize = 3;

/// 옮긴 VM이 대상 노드에서 켜지기를 기다리는 시간. 지나면 동시 시작 수에서 뺀다.
const HA_START_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct HaManagerConfig {
    /// 리스를 확인하는 간격
    pub tick: Duration,
    /// 그룹의 더 우선인 노드로 되돌아가기 전에 이 리더가 그 노드의 리스가 살아 있음을 본 시간
    pub failback_after: Duration,
    /// 노드마다 복구 대상을 미리 고르는 간격. 이보다 세 배 넘게 오래된 계획은 쓰지 않는다.
    pub plan_every: Duration,
}

impl Default for HaManagerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            failback_after: Duration::from_secs(30),
            plan_every: Duration::from_secs(10),
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

/// 거절된 되돌아가기를 다시 해 보기까지 기다리는 시간
const FAILBACK_RETRY: Duration = Duration::from_secs(60);

/// 되돌아가기 상태. 리더 자리를 잃으면 비운다.
#[derive(Default)]
struct Failback {
    /// 노드마다 이 리더가 리스 갱신을 받아 살아 있음을 처음 본 때
    alive_since: BTreeMap<Uuid, Instant>,
    /// 진행 중인 되돌아가기 Task(클러스터에 한 번에 하나)
    task: Option<Uuid>,
    /// 되돌아가기가 거절된 VM과 마지막으로 거절된 때. [`FAILBACK_RETRY`] 동안 다시 하지 않고, 로그는 처음
    /// 한 번만 남긴다.
    refused: BTreeMap<Uuid, Instant>,
    /// 되돌아가기 Task가 실패한 VM. 이 리더는 다시 하지 않는다.
    failed: BTreeSet<Uuid>,
}

/// 대상 노드마다 이 리더가 옮겼고 아직 그 노드에서 켜지지 않은 VM과 옮긴 때. 리더 자리를 잃으면 비운다
/// (새 리더는 빈 표로 시작해 한 번 상한을 넘을 수 있다).
#[derive(Debug, Default)]
struct Starting(BTreeMap<Uuid, BTreeMap<Uuid, Instant>>);

impl Starting {
    /// 그 노드에서 켜졌거나, 다른 노드로 갔거나, 꺼졌거나, 없어졌거나, [`HA_START_TIMEOUT`]이 지난 VM을 뺀다.
    fn prune(&mut self, inv: &Inventory, now: Instant) {
        for (target, vms) in &mut self.0 {
            vms.retain(|id, at| {
                now.saturating_duration_since(*at) < HA_START_TIMEOUT
                    && inv
                        .vms
                        .iter()
                        .find(|vm| vm.id == *id)
                        .is_some_and(|vm| starting_on(vm, *target))
            });
        }
        self.0.retain(|_, vms| !vms.is_empty());
    }

    /// 이 노드로 옮겨 켜지기를 기다리는 VM이 `limit`개 이상이다.
    fn full(&self, target: Uuid, limit: usize) -> bool {
        self.0.get(&target).map_or(0, BTreeMap::len) >= limit
    }

    fn add(&mut self, target: Uuid, vm: Uuid, now: Instant) {
        self.0.entry(target).or_default().insert(vm, now);
    }
}

/// 노드 `target`으로 옮긴 VM이 아직 거기서 켜지기를 기다린다.
fn starting_on(vm: &Vm, target: Uuid) -> bool {
    vm.spec.node == Some(target)
        && vm.spec.power == PowerState::Running
        && !(vm.status.phase == VmPhase::Running && vm.status.node == Some(target))
}

/// 미리 고른 복구 노드: 노드마다 "그 노드가 죽으면" VM마다 옮길 노드. 리더 메모리에만 있다(Raft 아님).
#[derive(Debug, Default)]
struct Plans {
    /// 계산한 때
    at: Option<Instant>,
    /// 노드 → VM → 대상 노드
    targets: BTreeMap<Uuid, BTreeMap<Uuid, Uuid>>,
}

impl Plans {
    /// 다시 계산할 때다.
    fn due(&self, now: Instant, every: Duration) -> bool {
        self.at
            .is_none_or(|at| now.saturating_duration_since(at) >= every)
    }

    /// 노드 `node`의 계획. 없거나 `every`의 세 배보다 오래됐으면 None.
    fn of(&self, node: Uuid, now: Instant, every: Duration) -> Option<&BTreeMap<Uuid, Uuid>> {
        let at = self.at?;
        (now.saturating_duration_since(at) <= every * 3)
            .then(|| self.targets.get(&node))
            .flatten()
    }
}

/// 노드마다 그 노드가 지금 죽으면 HA VM을 옮길 노드를 고른다: 그 노드의 VM을 모두 자리에서 빼고, 옮길 VM을
/// 복구 순서([`recovery_order`])로 [`pick`]해 앞 VM의 자리를 다음 VM에 센다(복구와 같은 셈).
fn plan(inv: &Inventory, leases: &Leases, now: Instant, b: Instant) -> Plans {
    let mut targets = BTreeMap::new();
    for node in inv.nodes.iter().filter(|n| n.status.ha.is_some()) {
        let mut vms: Vec<&Vm> = inv
            .vms
            .iter()
            .filter(|vm| recoverable(vm, node.id))
            .collect();
        if vms.is_empty() {
            continue;
        }
        vms.sort_by(|a, b| recovery_order(a, b));
        let mut assign = Assign::default();
        for vm in inv.vms.iter().filter(|vm| vm.spec.node == Some(node.id)) {
            assign.unplace(vm.id);
        }
        let mut chosen = BTreeMap::new();
        for vm in vms {
            let migrating = vm.status.migration.as_ref().map(|m| m.node);
            let eligible =
                |n: &Node| n.id != node.id && Some(n.id) != migrating && alive(now, b, leases, n);
            let target = match pick(vm, inv, &assign, eligible) {
                Pick::Node(t) | Pick::Violating(t, _) => t,
                Pick::NoNode | Pick::Blocked(_) => continue,
            };
            chosen.insert(vm.id, target);
            if !stays_stopped(vm) {
                assign.place(vm.id, target);
            }
        }
        targets.insert(node.id, chosen);
    }
    Plans {
        at: Some(now),
        targets,
    }
}

/// 복구 순서: `ha.priority`가 높은 VM이 먼저, 같으면 이름 순
fn recovery_order(a: &Vm, b: &Vm) -> Ordering {
    priority(b)
        .cmp(&priority(a))
        .then_with(|| a.name.cmp(&b.name))
}

fn priority(vm: &Vm) -> u8 {
    vm.spec
        .ha
        .as_ref()
        .map_or(DEFAULT_HA_PRIORITY, |ha| ha.priority)
}

/// `state: stopped`인 HA VM: 옮기되 켜지 않는다.
fn stays_stopped(vm: &Vm) -> bool {
    vm.spec
        .ha
        .as_ref()
        .is_some_and(|ha| ha.state == HaState::Stopped)
}

/// 한 노드를 복구하는 동안 쓰는 것: 그 노드의 계획, 대상 노드마다의 진행 중 시작과 상한, 셈(로그)
struct Queue<'a> {
    plan: Option<&'a BTreeMap<Uuid, Uuid>>,
    starting: &'a mut Starting,
    limit: usize,
    /// 계획대로 옮긴 수
    planned: usize,
    /// 계획이 없거나 맞지 않아 다시 고른 수
    repicked: usize,
    /// 대상 노드가 상한에 걸려 다음 틱으로 미룬 수
    waiting: usize,
    /// 이 틱에 옮긴 VM의 이름(로그)
    moved: Vec<String>,
}

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
    let mut failback = Failback::default();
    let mut starting = Starting::default();
    let mut plans = Plans::default();
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
            state: &state,
            cluster: &state.cluster,
            writer: writer.as_ref(),
        };
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(ctx, &config, &mut base, &mut unknown, &mut moved, &mut failback, &mut starting, &mut plans) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn tick(
    ctx: Ctx<'_>,
    config: &HaManagerConfig,
    base: &mut Base,
    unknown: &mut Unknowns,
    moved: &mut Vec<Moved>,
    failback: &mut Failback,
    starting: &mut Starting,
    plans: &mut Plans,
) {
    let cluster = ctx.cluster;
    for node in settled_by_term(unknown, ctx.writer.term(cluster), moved) {
        cluster.end_fence(NodeId(node));
    }
    if cluster.members().leader != Some(cluster.id()) {
        *base = None;
        *failback = Failback::default();
        *starting = Starting::default();
        *plans = Plans::default();
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
        // 호스트 종료 정책으로 얼린 노드의 HA VM은 그 노드가 돌아와 켠다.
        .filter(|n| !n.status.frozen())
        .filter(|n| expired(now, b, &leases, n))
        .filter(|n| inv.vms.iter().any(|vm| recoverable(vm, n.id)))
        .map(|n| n.id)
        .collect();
    let recovering = !failed.is_empty();
    starting.prune(&inv, now);
    if !recovering && plans.due(now, config.plan_every) {
        *plans = plan(&inv, &leases, now, b);
    }
    for node in failed {
        let plan = plans.of(node, now, config.plan_every);
        if let Some(u) = recover_node(ctx, leases.term, b, node, plan, starting).await {
            unknown.insert(node, u);
        }
    }
    let inv = if recovering {
        match Inventory::load(cluster) {
            Ok(inv) => inv,
            Err(_) => return,
        }
    } else {
        inv
    };
    let relocated = relocate(ctx, &inv, &leases, now, b).await;
    // 옮긴 뒤의 자리로 위반을 센다.
    let inv = if relocated {
        match Inventory::load(cluster) {
            Ok(inv) => inv,
            Err(_) => return,
        }
    } else {
        inv
    };
    update_violations(ctx, &inv).await;
    failback.observe(now, &leases, &inv);
    if !recovering && unknown.is_empty() {
        fail_back(ctx, failback, &inv, now, config.failback_after).await;
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

/// 노드 `node`를 펜싱 중으로 표시하고, 리스가 여전히 끝났으면 그 HA VM을 [`recovery_order`]로 옮긴다
/// (대상은 `plan`이 맞으면 그것, 아니면 그 자리에서 고른다). 대상 노드가 동시 시작 상한에 걸린 VM은 다음
/// 틱으로 미룬다. 결과를 모르는 쓰기가 생기면 펜싱 표시를 지우지 않고 그 쓰기를 돌려준다.
async fn recover_node(
    ctx: Ctx<'_>,
    term: u64,
    b: Instant,
    node: Uuid,
    plan: Option<&BTreeMap<Uuid, Uuid>>,
    starting: &mut Starting,
) -> Option<Unknown> {
    let cluster = ctx.cluster;
    let mut fence = Fence::begin(cluster, node);
    // 표시하기 직전에 받은 갱신이 있을 수 있으므로 표시한 뒤 다시 본다.
    let leases = cluster.leases();
    if leases.term != term {
        return None;
    }
    let Ok(inv) = Inventory::load(cluster) else {
        return None;
    };
    let failed = inv.nodes.iter().find(|n| n.id == node).cloned()?;
    if !expired(Instant::now(), b, &leases, &failed) || failed.status.frozen() {
        return None;
    }
    let Ok(settings) = crate::settings::current(cluster) else {
        return None;
    };
    let mut queue = Queue {
        plan,
        starting,
        limit: settings.spec.ha_start_concurrency(),
        planned: 0,
        repicked: 0,
        waiting: 0,
        moved: vec![],
    };
    let mut vms: Vec<Vm> = inv
        .vms
        .iter()
        .filter(|vm| recoverable(vm, node))
        .cloned()
        .collect();
    vms.sort_by(recovery_order);
    // 죽은 노드의 VM은 펜싱으로 꺼졌으므로 자리로 세지 않는다. 옮긴 VM은 새 자리로 센다(같은 노드의 affinity
    // 상대는 먼저 옮긴 VM을 따라간다).
    let mut assign = Assign::default();
    for vm in inv.vms.iter().filter(|vm| vm.spec.node == Some(node)) {
        assign.unplace(vm.id);
    }
    for vm in &vms {
        let result = recover_vm(
            ctx,
            &mut fence,
            &inv,
            &mut assign,
            &mut queue,
            &leases,
            b,
            &failed,
            vm,
        )
        .await;
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
    // 동시 시작 상한·놓을 노드를 기다리는 동안은 틱마다 다시 보므로 실제로 옮긴 틱에만 알린다.
    if queue.moved.is_empty() {
        tracing::debug!(node = %failed.name, waiting = queue.waiting, "HA VMs of a failed node are waiting to be recovered");
    } else {
        tracing::warn!(node = %failed.name, vms = ?queue.moved, "recovered HA VMs from a failed node");
    }
    tracing::debug!(node = %failed.name, planned = queue.planned, repicked = queue.repicked, waiting = queue.waiting, "HA recovery targets");
    None
}

/// VM 하나를 옮긴다: status를 먼저 쓰고 spec의 노드를 바꾼 뒤, 죽은 노드의 그 VM Task를 실패로
/// 끝내고 기록 Task를 남긴다. 대상이 없으면 그 이유만 status에 남긴다. `state: stopped`는 옮기고 끈다.
#[allow(clippy::too_many_arguments)]
async fn recover_vm(
    ctx: Ctx<'_>,
    fence: &mut Fence<'_>,
    inv: &Inventory,
    assign: &mut Assign,
    queue: &mut Queue<'_>,
    leases: &Leases,
    b: Instant,
    failed: &Node,
    vm: &Vm,
) -> Result<(), ClusterError> {
    let migrating = vm.status.migration.as_ref().map(|m| m.node);
    let now = Instant::now();
    // 죽은 노드와 마이그레이션을 받던 노드(같은 실행 디렉터리를 쓰는 QEMU가 남아 있을 수 있다)는 빼고,
    // 리스가 살아 있어야 한다.
    let eligible =
        |n: &Node| n.id != failed.id && Some(n.id) != migrating && alive(now, b, leases, n);
    let starts = !stays_stopped(vm);
    let limit = queue.limit;
    let full = |starting: &Starting, t: Uuid| starts && starting.full(t, limit);
    // 미리 고른 대상이 지금도 조건·hard 규칙에 맞고 상한에 걸리지 않으면 그대로 쓴다.
    let planned = queue
        .plan
        .and_then(|p| p.get(&vm.id))
        .and_then(|t| inv.nodes.iter().find(|n| n.id == *t))
        .filter(|n| eligible(n) && node_fits(&vm.spec, inv, n).is_ok())
        .filter(|n| placement::hard_violations(vm, n, inv, assign).is_empty())
        .filter(|n| !full(queue.starting, n.id))
        .map(|n| n.id);
    let choice = match planned {
        Some(t) => {
            queue.planned += 1;
            Pick::Node(t)
        }
        None => {
            queue.repicked += 1;
            pick(vm, inv, assign, eligible)
        }
    };
    let Some(target) = placed(ctx, vm, choice).await? else {
        return Ok(());
    };
    // 미리 고른 대상이 상한이면 위에서 다시 골랐다(다른 노드가 나오면 그리로 옮긴다). 고른 노드가 상한이면 다음
    // 후보는 쓰지 않고 다음 틱에 다시 본다(그 노드에서 순서가 지켜지게).
    if full(queue.starting, target) {
        queue.waiting += 1;
        return Ok(());
    }
    // 노드를 바꾸기 전에 실행 상태를 지운다. 사이에 리더가 바뀌면 새 리더가 다시 한다(아직 그 노드).
    // 놓지 못한 이유(`ha.error`)와 옮겨 달라는 요청은 지운다(노드 장애 복구는 옮긴 횟수에 세지 않는다).
    let ha = vm
        .status
        .ha
        .clone()
        .map(|ha| VmHaStatus {
            error: None,
            relocate_from: None,
            ..ha
        })
        .filter(|ha| *ha != VmHaStatus::default());
    let status = VmStatus {
        phase: VmPhase::Stopped,
        node: None,
        pid: None,
        migration: None,
        last_error: Some(format!("recovering: node {} failed", failed.name)),
        ha,
        ..vm.status.clone()
    };
    let command = status_command::<VmSpec>(vm.id, vm.revision, &status)?;
    let revision = fence.write(ctx, command, None).await?;
    let mut spec = vm.spec.clone();
    spec.node = Some(target);
    if stays_stopped(vm) {
        spec.power = PowerState::Stopped;
    }
    let command = Command::UpdateSpec {
        kind: VmSpec::KIND,
        id: vm.id,
        expected_revision: Some(revision),
        labels: vm.labels.clone(),
        spec: serde_json::to_value(&spec)?,
    };
    fence.write(ctx, command, Some(target)).await?;
    queue.moved.push(vm.name.clone());
    // 다음 VM의 규칙과 미할당 메모리에 센다.
    if spec.power == PowerState::Running {
        queue.starting.add(target, vm.id, Instant::now());
        assign.place(vm.id, target);
    } else {
        assign.unplace(vm.id);
    }
    finish(ctx, vm, failed, target).await;
    Ok(())
}

/// 창 안에서 이미 `max_relocate`번 옮긴 VM을 끈다(`error`): spec power를 Stopped로 쓴 뒤 status에 phase
/// Failed와 `ha.error`를 쓰고 요청을 지운다. spec을 먼저 쓴다: 요청을 먼저 지우면 그 노드가 spec을 쓰기 전에
/// 다시 켤 수 있다. 그 사이 그 노드가 꺼진 VM의 요청을 지우며 status를 썼으면 다시 읽어 쓴다. 그 사이 리더를
/// 잃으면 오류 표시 없이 꺼진 채 남는다(안전한 쪽).
async fn give_up(ctx: Ctx<'_>, vm: &Vm, from: &Node, ha: VmHaStatus) -> Result<(), ClusterError> {
    let error = format!("gave up after {} relocations", ha.relocations.len());
    tracing::warn!(vm = %vm.name, node = %from.name, error = %error, "HA VM keeps failing; leaving it stopped");
    let mut spec = vm.spec.clone();
    spec.power = PowerState::Stopped;
    let mut revision = write(ctx, spec_command(vm, vm.revision, &spec)?).await?;
    let mut current = vm.status.clone();
    for _ in 0..TASK_FAIL_ATTEMPTS {
        let status = VmStatus {
            phase: VmPhase::Failed,
            pid: None,
            last_error: Some(format!("HA {error}")),
            ha: Some(VmHaStatus {
                relocate_from: None,
                error: Some(error.clone()),
                ..ha.clone()
            }),
            ..current
        };
        match update_status_checked::<VmSpec>(ctx, vm.id, revision, &status).await {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                match ctx.cluster.get::<VmSpec>(vm.id)? {
                    Some(v)
                        if v.spec.power == PowerState::Stopped && v.spec.node == vm.spec.node =>
                    {
                        revision = v.revision;
                        current = v.status;
                    }
                    // 사용자가 다시 켰거나 옮겼다.
                    _ => return Ok(()),
                }
            }
            r => return r.map(|_| ()),
        }
    }
    Ok(())
}

/// [`pick`]의 결과에서 옮길 노드. 놓을 곳이 없으면 그 이유를 status에 남기고(같으면 다시 쓰지 않는다)
/// None이다.
async fn placed(ctx: Ctx<'_>, vm: &Vm, pick: Pick) -> Result<Option<Uuid>, ClusterError> {
    match pick {
        Pick::Node(target) => Ok(Some(target)),
        Pick::Violating(target, rules) => {
            tracing::warn!(vm = %vm.name, rules = ?rules, "HA recovery breaks placement rules that allow it (ha_may_violate)");
            Ok(Some(target))
        }
        Pick::NoNode => {
            if vm.status.last_error.as_deref() != Some(NO_NODE) {
                let status = VmStatus {
                    last_error: Some(NO_NODE.into()),
                    ..vm.status.clone()
                };
                update_status_checked::<VmSpec>(ctx, vm.id, vm.revision, &status).await?;
                tracing::warn!(vm = %vm.name, "HA cannot find a node for the VM");
            }
            Ok(None)
        }
        Pick::Blocked(rules) => {
            let error = format!(
                "HA cannot place this VM: placement rule {}",
                rules.join(", ")
            );
            let ha = vm.status.ha.clone().unwrap_or_default();
            if vm.status.last_error.as_ref() != Some(&error) || ha.error.as_ref() != Some(&error) {
                let status = VmStatus {
                    last_error: Some(error.clone()),
                    ha: Some(VmHaStatus {
                        error: Some(error),
                        ..ha
                    }),
                    ..vm.status.clone()
                };
                update_status_checked::<VmSpec>(ctx, vm.id, vm.revision, &status).await?;
                tracing::warn!(vm = %vm.name, rules = ?rules, "placement rules leave no node for the HA VM");
            }
            Ok(None)
        }
    }
}

/// 옮겨 달라고 한 노드: 재시작 횟수를 넘긴 HA VM이 아직 그 노드에 배정되어 있고 켜져 있다.
fn relocating(vm: &Vm) -> Option<Uuid> {
    let from = vm.status.ha.as_ref()?.relocate_from?;
    (vm.spec.node == Some(from) && vm.spec.power == PowerState::Running && vm.spec.ha_active())
        .then_some(from)
}

/// 옮기기 창 안에서 다른 노드로 옮긴 기록이 있는 VM
fn relocated_recently(vm: &Vm, unix: u64) -> bool {
    vm.status.ha.as_ref().is_some_and(|ha| {
        ha.relocations
            .iter()
            .any(|at| at.saturating_add(RELOCATE_WINDOW.as_secs()) > unix)
    })
}

/// 재시작 횟수를 넘겨 옮겨 달라고 한 HA VM(이름 순)을 그 노드를 뺀 곳으로 옮긴다(복구와 같은 순서·쓰기:
/// status를 먼저 쓰고 spec의 노드를 바꾼다). 창 안에서 이미 `max_relocate`번 옮겼으면 옮기지 않고 끈다
/// ([`give_up`]). 그 노드가 살아 있을 때만 한다(죽었으면 복구가 옮긴다). 그 노드의 reconciler는 요청이
/// 있는 동안 VM을 켜지 않고 status도 쓰지 않으므로 펜싱하지 않는다. 쓰기는 revision을 확인하므로 결과를
/// 모르면 다음 tick에 다시 읽어 남은 단계를 한다: 옮기는 status(`node` 없음)를 이미 썼으면 기록을 다시
/// 세지 않고 spec만 쓴다. 옮기거나 껐으면 true.
async fn relocate(
    ctx: Ctx<'_>,
    inv: &Inventory,
    leases: &Leases,
    now: Instant,
    b: Instant,
) -> bool {
    let mut vms: Vec<(&Vm, &Node)> = inv
        .vms
        .iter()
        .filter_map(|vm| {
            let from = relocating(vm)?;
            let node = inv.nodes.iter().find(|n| n.id == from)?;
            alive(now, b, leases, node).then_some((vm, node))
        })
        .collect();
    vms.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    let mut assign = Assign::default();
    let mut changed = false;
    for (vm, from) in vms {
        match relocate_vm(ctx, inv, &mut assign, leases, now, b, vm, from).await {
            Ok(done) => changed |= done,
            Err(e) => {
                tracing::info!(vm = %vm.name, error = %e, "cannot relocate HA VM; will retry");
            }
        }
    }
    changed
}

/// VM 하나를 옮기거나 끈다([`relocate`]). 옮기거나 껐으면 true.
#[allow(clippy::too_many_arguments)]
async fn relocate_vm(
    ctx: Ctx<'_>,
    inv: &Inventory,
    assign: &mut Assign,
    leases: &Leases,
    now: Instant,
    b: Instant,
    vm: &Vm,
    from: &Node,
) -> Result<bool, ClusterError> {
    let policy = vm.spec.ha.clone().unwrap_or_default();
    let mut ha = vm.status.ha.clone().unwrap_or_default();
    // 옮기는 status를 이미 썼다(spec 쓰기만 남았다).
    let prepared = vm.status.node.is_none();
    let mut revision = vm.revision;
    if !prepared {
        let unix = unix_now();
        ha.relocations
            .retain(|at| at.saturating_add(RELOCATE_WINDOW.as_secs()) > unix);
        if ha.relocations.len() >= usize::from(policy.max_relocate) {
            give_up(ctx, vm, from, ha).await?;
            return Ok(true);
        }
        ha.relocations.push(unix);
        ha.relocated_from = Some(from.id);
    }
    let migrating = vm.status.migration.as_ref().map(|m| m.node);
    let eligible =
        |n: &Node| n.id != from.id && Some(n.id) != migrating && alive(now, b, leases, n);
    let Some(target) = placed(ctx, vm, pick(vm, inv, assign, eligible)).await? else {
        return Ok(false);
    };
    if !prepared {
        // 요청(`relocate_from`)은 남긴다: spec을 바꾸기 전에 그 노드가 다시 켜지 않게. 받은 노드가 켤 때 지운다.
        let status = VmStatus {
            phase: VmPhase::Stopped,
            node: None,
            pid: None,
            migration: None,
            last_error: Some(format!(
                "relocating: the VM failed too often on node {}",
                from.name
            )),
            ha: Some(VmHaStatus { error: None, ..ha }),
            ..vm.status.clone()
        };
        revision = update_status_checked::<VmSpec>(ctx, vm.id, revision, &status).await?;
    }
    let mut spec = vm.spec.clone();
    spec.node = Some(target);
    if policy.state == HaState::Stopped {
        spec.power = PowerState::Stopped;
    }
    write(ctx, spec_command(vm, revision, &spec)?).await?;
    tracing::warn!(vm = %vm.name, from = %from.name, to = %target, "relocated HA VM that failed too often");
    if spec.power == PowerState::Running {
        assign.place(vm.id, target);
    }
    Ok(true)
}

/// [`pick`]의 결과
#[derive(Debug, PartialEq, Eq)]
enum Pick {
    /// 조건과 hard 규칙에 맞는 가장 나은 노드
    Node(Uuid),
    /// hard 규칙에 맞는 노드는 없지만, 막는 규칙이 모두 `ha_may_violate`라 어기고 놓을 노드(위반이 가장
    /// 적은 노드)와 그 규칙
    Violating(Uuid, Vec<String>),
    /// 노드 조건부터 맞는 노드가 없다.
    NoNode,
    /// 노드 조건에 맞는 노드를 모두 막는 hard 규칙(이름 순)
    Blocked(Vec<String>),
}

/// 복구할 노드를 고른다: `eligible`이고 노드 조건에 맞는 노드 가운데 [`placement::candidates`] 순서로 가장
/// 나은 것.
fn pick(vm: &Vm, inv: &Inventory, assign: &Assign, eligible: impl Fn(&Node) -> bool) -> Pick {
    let fits: Vec<&Node> = inv
        .nodes
        .iter()
        .filter(|n| eligible(n) && node_fits(&vm.spec, inv, n).is_ok())
        .collect();
    if fits.is_empty() {
        return Pick::NoNode;
    }
    let ids: BTreeSet<Uuid> = fits.iter().map(|n| n.id).collect();
    let found: Vec<Candidate> = placement::candidates(vm, inv, assign)
        .into_iter()
        .filter(|c| ids.contains(&c.node))
        .collect();
    if let Some(c) = placement::best(&found) {
        return Pick::Node(c.node);
    }
    let may_violate = |name: &String| {
        inv.rules
            .iter()
            .any(|r| &r.name == name && r.spec.ha_may_violate)
    };
    let mut blocking = BTreeSet::new();
    let mut fallback: Option<(Vec<String>, Candidate)> = None;
    for node in fits {
        let broken = placement::hard_violations(vm, node, inv, assign);
        if broken.iter().all(may_violate) {
            let c = placement::score(vm, node, inv, assign);
            let better = fallback.as_ref().is_none_or(|(rules, best)| {
                broken
                    .len()
                    .cmp(&rules.len())
                    .then(placement::compare(&c, best))
                    == Ordering::Less
            });
            if better {
                fallback = Some((broken.clone(), c));
            }
        }
        blocking.extend(broken);
    }
    match fallback {
        Some((rules, c)) => Pick::Violating(c.node, rules),
        None => Pick::Blocked(blocking.into_iter().collect()),
    }
}

/// 자리 잡은 VM(꺼지지 않았고 노드가 있음)이 지금 노드에서 어기는 hard·soft 규칙의 이름(이름 순)
fn violations(vm: &Vm, inv: &Inventory) -> Vec<String> {
    let node = vm
        .spec
        .node
        .filter(|_| vm.spec.power == PowerState::Running)
        .and_then(|id| inv.nodes.iter().find(|n| n.id == id));
    let Some(node) = node else {
        return Vec::new();
    };
    let assign = Assign::default();
    let mut names: BTreeSet<String> = placement::hard_violations(vm, node, inv, &assign)
        .into_iter()
        .collect();
    names.extend(placement::soft_violations(vm, node, inv, &assign));
    names.into_iter().collect()
}

/// 달라진 VM의 `rule_violations`만 쓴다. 그사이 바뀌었으면(revision) 다음 tick에 다시 본다.
async fn update_violations(ctx: Ctx<'_>, inv: &Inventory) {
    for vm in &inv.vms {
        let found = violations(vm, inv);
        if found == vm.status.rule_violations {
            continue;
        }
        let status = VmStatus {
            rule_violations: found,
            ..vm.status.clone()
        };
        if let Err(e) = update_status_checked::<VmSpec>(ctx, vm.id, vm.revision, &status).await {
            tracing::debug!(vm = %vm.name, error = %e, "cannot update rule violations; will retry");
        }
    }
}

impl Failback {
    /// 리스 갱신을 받은 지 `recover_after`가 지나지 않은 HA 가능 노드를 살아 있다고 본다. 이 리더가 갱신을
    /// 받지 않은 노드(기준점 b로 센 노드)는 보지 않는다.
    fn observe(&mut self, now: Instant, leases: &Leases, inv: &Inventory) {
        for n in &inv.nodes {
            let fresh = n.status.ha.is_some_and(|ha| {
                leases.renewed.get(&NodeId(n.id)).is_some_and(|renewed| {
                    now.saturating_duration_since(*renewed)
                        < Duration::from_secs(ha.recover_after_secs)
                })
            });
            if fresh {
                self.alive_since.entry(n.id).or_insert(now);
            } else {
                self.alive_since.remove(&n.id);
            }
        }
    }

    /// 거절된 되돌아가기를 아직 다시 하지 않을 VM
    fn waiting(&self, vm: Uuid, now: Instant) -> bool {
        self.refused
            .get(&vm)
            .is_some_and(|at| now.saturating_duration_since(*at) < FAILBACK_RETRY)
    }

    /// 이 VM이 되돌아갈 노드: 그룹이 있고 `nofailback`이 아니면, 지금 노드보다 그룹 우선순위가 높고
    /// `after` 넘게 살아 있으며 노드 조건과 hard 규칙에 맞는 노드 가운데 가장 나은 것.
    fn target(&self, vm: &Vm, inv: &Inventory, now: Instant, after: Duration) -> Option<Uuid> {
        let name = vm.spec.ha.as_ref()?.group.as_ref()?;
        let group = inv.groups.iter().find(|g| &g.name == name)?;
        if group.spec.nofailback {
            return None;
        }
        let current = inv.nodes.iter().find(|n| Some(n.id) == vm.spec.node)?;
        let here = group.spec.priority(&current.name);
        let assign = Assign::default();
        let found: Vec<Candidate> = inv
            .nodes
            .iter()
            .filter(|n| n.id != current.id && group.spec.priority(&n.name) > here)
            .filter(|n| {
                self.alive_since
                    .get(&n.id)
                    .is_some_and(|since| now.saturating_duration_since(*since) >= after)
            })
            .filter(|n| node_fits(&vm.spec, inv, n).is_ok())
            .filter(|n| placement::hard_violations(vm, n, inv, &assign).is_empty())
            .map(|n| placement::score(vm, n, inv, &assign))
            .collect();
        placement::best(&found).map(|c| c.node)
    }
}

/// 되돌아가기: 진행 중인 되돌아가기 Task가 없으면 실행 중인 HA VM(이름 순) 가운데 그룹의 더 우선인 노드가
/// 있는 첫 VM을 그 노드로 라이브 마이그레이션한다(규칙을 지킨다). 거절되면 VM마다 한 번 로그를 남기고
/// 다음 VM을 본다(유지보수·HVF 노드, affinity 상대가 있는 VM 등은 `start_migration`이 거절한다). 거절된
/// VM은 [`FAILBACK_RETRY`] 뒤에 다시 본다. 옮겨 떠난 노드로 되돌아가면 옮기기 한 번으로 센다([`count_failback`]).
async fn fail_back(
    ctx: Ctx<'_>,
    failback: &mut Failback,
    inv: &Inventory,
    now: Instant,
    after: Duration,
) {
    if let Some(task) = failback.task {
        match ctx.cluster.get::<TaskSpec>(task) {
            Ok(Some(t)) if !t.status.state.is_finished() => return,
            Err(_) => return,
            Ok(Some(t)) => {
                if t.status.state == TaskState::Failed {
                    tracing::warn!(vm = %t.spec.target, error = ?t.status.error, "HA failback migration failed; not retrying it");
                    failback.failed.insert(t.spec.target);
                }
                failback.task = None;
            }
            Ok(None) => failback.task = None,
        }
    }
    let unix = unix_now();
    let mut vms: Vec<&Vm> = inv
        .vms
        .iter()
        .filter(|vm| vm.spec.ha_active() && vm.spec.power == PowerState::Running)
        .filter(|vm| vm.status.phase == VmPhase::Running && vm.status.migration.is_none())
        .filter(|vm| !failback.failed.contains(&vm.id))
        .filter(|vm| !failback.waiting(vm.id, now))
        // 재시작 횟수를 넘겨 옮겨 온 VM은 옮기기 창 안에서 되돌리지 않는다(그 노드에서 다시 크래시하면 error다).
        .filter(|vm| !relocated_recently(vm, unix))
        .collect();
    vms.sort_by(|a, b| a.name.cmp(&b.name));
    for vm in vms {
        let Some(target) = failback.target(vm, inv, now, after) else {
            continue;
        };
        match start_migration(
            ctx.state,
            vm.id,
            target,
            None,
            vm.spec.node,
            RuleCheck::Enforce,
        )
        .await
        {
            Ok((task, _)) => {
                tracing::info!(vm = %vm.name, node = %target, task = %task.id, "HA failback: migrating the VM to a preferred node of its group");
                failback.task = Some(task.id);
                failback.refused.remove(&vm.id);
                if vm.status.ha.as_ref().and_then(|ha| ha.relocated_from) == Some(target) {
                    count_failback(ctx, vm, unix).await;
                }
                return;
            }
            Err(e) => {
                if failback.refused.insert(vm.id, now).is_none() {
                    tracing::info!(vm = %vm.name, node = %target, error = %e.message, "HA failback refused");
                }
            }
        }
    }
}

/// 옮겨 떠난 노드(`relocated_from`)로의 되돌아가기를 옮기기 한 번으로 센다: 창 밖 기록을 버리고 지금 시각을
/// 더하고 `relocated_from`을 지운다. 그 노드에서 다시 크래시하면 `max_relocate`에 닿는다(되돌아가기와
/// 옮기기가 창마다 끝없이 돌지 않게). 마이그레이션 Task와 엇갈리면 다시 읽어 쓴다.
async fn count_failback(ctx: Ctx<'_>, vm: &Vm, unix: u64) {
    for _ in 0..TASK_FAIL_ATTEMPTS {
        // 마이그레이션 시작이 status를 썼으므로 다시 읽는다.
        let Ok(Some(current)) = ctx.cluster.get::<VmSpec>(vm.id) else {
            return;
        };
        let mut ha = current.status.ha.clone().unwrap_or_default();
        ha.relocations
            .retain(|at| at.saturating_add(RELOCATE_WINDOW.as_secs()) > unix);
        ha.relocations.push(unix);
        ha.relocated_from = None;
        let status = VmStatus {
            ha: Some(ha),
            ..current.status.clone()
        };
        match update_status_checked::<VmSpec>(ctx, vm.id, current.revision, &status).await {
            Ok(_) => return,
            Err(e) => {
                tracing::info!(vm = %vm.name, error = %e, "cannot count HA failback as a relocation");
            }
        }
    }
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

/// 노드 `node`에 배정된 HA VM으로 실행하기로 된 것. `state: stopped`는 꺼져 있어도 노드를 옮긴다.
fn recoverable(vm: &Vm, node: Uuid) -> bool {
    vm.spec.node == Some(node)
        && vm.spec.ha_active()
        && (vm.spec.power == PowerState::Running || stays_stopped(vm))
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

/// 복구에 쓰는 것: 앱 상태(되돌아가기의 마이그레이션), 클러스터와 쓰기 경로
#[derive(Clone, Copy)]
struct Ctx<'a> {
    state: &'a AppState,
    cluster: &'a Cluster,
    writer: &'a dyn RecoveryWriter,
}

async fn write(ctx: Ctx<'_>, command: Command) -> Result<u64, ClusterError> {
    ctx.writer.write(ctx.cluster, command).await
}

fn spec_command(vm: &Vm, expected_revision: u64, spec: &VmSpec) -> Result<Command, ClusterError> {
    Ok(Command::UpdateSpec {
        kind: VmSpec::KIND,
        id: vm.id,
        expected_revision: Some(expected_revision),
        labels: vm.labels.clone(),
        spec: serde_json::to_value(spec)?,
    })
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
    use opl_types::vm::{CpuSpec, MachineType, VmHa};

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

    fn vm(name: &str, priority: Option<u8>, node: Uuid) -> Vm {
        Resource::new(
            name,
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 1,
                    model: Default::default(),
                },
                memory_mib: 1024,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![],
                nics: vec![],
                power: PowerState::Running,
                restart: Default::default(),
                node: Some(node),
                snapshot_schedule: None,
                ha: priority.map(|priority| VmHa {
                    priority,
                    ..Default::default()
                }),
            },
        )
    }

    fn inventory(vms: Vec<Vm>) -> Inventory {
        Inventory {
            nodes: vec![],
            vms,
            volumes: vec![],
            pools: vec![],
            networks: vec![],
            rules: vec![],
            groups: vec![],
        }
    }

    #[test]
    fn recovery_order_is_priority_then_name() {
        let node = Uuid::now_v7();
        let mut vms = [
            vm("b", Some(50), node),
            vm("a", Some(10), node),
            vm("d", Some(90), node),
            // 정책이 `true`인 VM(기본 50)
            vm("c", None, node),
            vm("e", Some(90), node),
        ];
        vms.sort_by(recovery_order);
        let names: Vec<&str> = vms.iter().map(|vm| vm.name.as_str()).collect();
        assert_eq!(names, ["d", "e", "b", "c", "a"]);
    }

    #[test]
    fn starting_vms_leave_when_running_moved_stopped_gone_or_timed_out() {
        let (target, other) = (Uuid::now_v7(), Uuid::now_v7());
        let t0 = Instant::now();
        let t1 = t0 + HA_START_TIMEOUT;
        let waiting = vm("waiting", None, target);
        let mut running = vm("running", None, target);
        running.status.phase = VmPhase::Running;
        running.status.node = Some(target);
        // 다른 노드에서 실행 중이라고 남은 status는 아직 기다린다.
        let mut stale = vm("stale", None, target);
        stale.status.phase = VmPhase::Running;
        stale.status.node = Some(other);
        let moved = vm("moved", None, other);
        let mut off = vm("off", None, target);
        off.spec.power = PowerState::Stopped;
        let late = vm("late", None, target);
        let gone = Uuid::now_v7();
        let mut starting = Starting::default();
        for v in [&waiting, &running, &stale, &moved, &off] {
            starting.add(target, v.id, t1);
        }
        starting.add(target, gone, t1);
        starting.add(target, late.id, t0);
        assert!(starting.full(target, 7));
        let inv = inventory(vec![
            waiting.clone(),
            running,
            stale.clone(),
            moved,
            off,
            late,
        ]);
        starting.prune(&inv, t1);
        let left: Vec<Uuid> = starting.0[&target].keys().copied().collect();
        let mut expected = vec![waiting.id, stale.id];
        expected.sort();
        assert_eq!(left, expected);
        assert!(starting.full(target, 2));
        assert!(!starting.full(target, 3));
        assert!(!starting.full(other, 1));
        // 120초가 지나면 기다리던 VM도 빠지고 빈 노드는 지운다.
        starting.prune(&inv, t1 + HA_START_TIMEOUT);
        assert!(starting.0.is_empty());
    }

    #[test]
    fn plans_are_made_again_after_plan_every_and_not_used_when_old() {
        let every = Duration::from_secs(10);
        let t0 = Instant::now();
        let (node, vm_id, target) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let mut plans = Plans::default();
        assert!(plans.due(t0, every));
        assert!(plans.of(node, t0, every).is_none());
        plans = Plans {
            at: Some(t0),
            targets: BTreeMap::from([(node, BTreeMap::from([(vm_id, target)]))]),
        };
        assert!(!plans.due(t0 + Duration::from_secs(9), every));
        assert!(plans.due(t0 + every, every));
        assert_eq!(
            plans.of(node, t0 + every * 3, every).unwrap()[&vm_id],
            target
        );
        assert!(
            plans
                .of(node, t0 + every * 3 + Duration::from_secs(1), every)
                .is_none()
        );
        assert!(plans.of(Uuid::now_v7(), t0, every).is_none());
    }

    #[test]
    fn a_refused_failback_waits_before_it_is_tried_again() {
        let (vm, other) = (Uuid::now_v7(), Uuid::now_v7());
        let t0 = Instant::now();
        let mut failback = Failback::default();
        failback.refused.insert(vm, t0);
        assert!(failback.waiting(vm, t0 + Duration::from_secs(59)));
        assert!(!failback.waiting(vm, t0 + FAILBACK_RETRY));
        assert!(!failback.waiting(other, t0));
    }
}
