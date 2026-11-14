//! 균형 조정 계산(설계 §8.3, 순수). 리더가 아는 노드 부하로 노드 CPU·메모리 사용률의 불균형을 재고, 불균형을
//! 가장 많이 줄이는 옮기기를 욕심쟁이 방식으로 고른다. 그보다 먼저 지금 노드에서 hard 규칙을 어기는 VM을 맞는
//! 노드로 옮긴다(규칙 바로잡기). 리더의 균형 조정기 작업이 쓴다.
//!
//! 사용률은 점수 함수([`node_use`])와 같은 셈이다(끝나지 않은 마이그레이션의 VM은 대상 노드에 센다). 대상 노드는 신선한
//! 부하 요약이 있는 노드뿐이고, 옮길 수 있는 VM과 대상 조건은 마이그레이션 API(`start_migration`)가 거절할 것을 미리
//! 거른다. 균형 옮기기는 불균형이 문턱을 넘으면 시작해 문턱의 [`STOP_RATIO`] 아래가 될 때까지 계속하고(히스테리시스),
//! 받는 노드는 리더가 오래 살아 있음을 본 노드뿐이다.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use opl_cluster::Leases;
use opl_types::node::Node;
use opl_types::placement::{MoveReason, RuleKind};
use opl_types::settings::{PlacementMode, PlacementPolicy, PlacementSettings};
use opl_types::task::{Task, TaskOp};
use opl_types::vm::{PowerState, Vm, VmPhase};
use tokio::time::Instant;
use uuid::Uuid;

use crate::ha::{RELOCATE_WINDOW, ready};
use crate::ha_health::Health;
use crate::placement::{self, Assign};
use crate::scheduler::{Inventory, disk_moves, node_fits};
use crate::score::{LOAD_STALE, Loads, NodeUse, Scorer, node_use, vm_cpu_share};

/// 균형 옮기기의 원본으로 보는 노드 수(사용률이 큰 것부터)
pub const SOURCE_NODES: usize = 5;
/// 균형 옮기기의 대상으로 보는 노드 수(사용률이 작은 것부터)
pub const TARGET_NODES: usize = 10;
/// 받는 옮기기의 가장 작은 효과(불균형이 줄어드는 양, 노드 둘 기준). VM 하나를 옮기면 노드 둘의 사용률만 바뀌어
/// 표준편차가 줄어드는 양이 노드 수 n에 반비례하므로, 노드 n개에서는 효과 × n / 2를 이것과 견준다.
pub const MIN_GAIN: f64 = 0.005;
/// 받는 옮기기의 비용(GiB)당 가장 작은 효과(노드 둘 기준, [`MIN_GAIN`]처럼 견준다)
pub const MIN_GAIN_PER_GIB: f64 = 0.002;
/// 한 VM을 다시 옮기기까지 기다리는 시간(마이그레이션 Task·HA 옮기기·이 리더의 옮기기 뒤)
pub const VM_COOLDOWN: Duration = Duration::from_secs(3600);
/// 클러스터에서 끝나지 않은 마이그레이션이 이만큼이면 쉰다.
pub const MAX_CONCURRENT: usize = 2;
/// 한 회차의 추천 수 상한
pub const MAX_RECOMMENDATIONS: usize = 5;
/// 균형 옮기기를 시작한 뒤 멈추는 불균형(문턱에 곱함). 문턱 근처의 잡음으로 옮기기를 되풀이하지 않게 한다.
pub const STOP_RATIO: f64 = 0.8;
/// 한 노드의 사용률(CPU나 메모리)이 평균보다 문턱의 이만큼 넘게 높으면 불균형이 문턱 아래여도 그 노드에서 옮긴다(노드가
/// 많으면 한 노드만 뜨거워도 표준편차가 작다).
pub const HOT_RATIO: f64 = 2.0;
/// 마이그레이션이 끝난 원본·대상 노드는 이 동안 균형 옮기기의 원본·대상이 아니다(5분 평균 CPU가 옮긴 VM을 아직 원본에
/// 센다). Task는 끝난 때를 적지 않으므로 만든 때(UUIDv7)로 센다.
pub const MIGRATION_SETTLE: Duration = Duration::from_secs(300);
/// HA 되돌아가기를 기다리는 가장 긴 시간. 더 우선인 노드가 살아 있음을 이 리더가 본 지 이보다 오래됐으면 HA가 되돌리지
/// 못하는 것으로 보고 기다리지 않는다.
pub const FAILBACK_WAIT: Duration = Duration::from_secs(600);

/// HVF(macOS) 노드의 hypervisor 이름. 마이그레이션을 못 한다.
const HVF: &str = "qemu-hvf";

/// 노드 사용률의 불균형: CPU·메모리 사용률의 모표준편차와 그 평균(`total`)
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Imbalance {
    pub cpu: f64,
    pub mem: f64,
    pub total: f64,
}

/// 사용률의 합과 제곱합(노드 둘을 바꿔 넣어 불균형을 O(1)로 다시 센다)
#[derive(Debug, Clone, Copy, Default)]
struct Sums {
    n: f64,
    cpu: f64,
    cpu2: f64,
    mem: f64,
    mem2: f64,
}

impl Sums {
    fn of(uses: impl IntoIterator<Item = NodeUse>) -> Self {
        let mut sums = Self::default();
        for u in uses {
            sums.add(u, 1.0);
        }
        sums
    }

    /// `sign`이 1이면 더하고 -1이면 뺀다.
    fn add(&mut self, u: NodeUse, sign: f64) {
        self.n += sign;
        self.cpu += sign * u.cpu;
        self.cpu2 += sign * u.cpu * u.cpu;
        self.mem += sign * u.mem;
        self.mem2 += sign * u.mem * u.mem;
    }

    fn imbalance(&self) -> Imbalance {
        if self.n < 1.0 {
            return Imbalance::default();
        }
        let sd = |sum: f64, sum2: f64| {
            let mean = sum / self.n;
            (sum2 / self.n - mean * mean).max(0.0).sqrt()
        };
        let cpu = sd(self.cpu, self.cpu2);
        let mem = sd(self.mem, self.mem2);
        Imbalance {
            cpu,
            mem,
            total: (cpu + mem) / 2.0,
        }
    }
}

/// 노드 사용률들의 불균형
pub fn imbalance(uses: &[NodeUse]) -> Imbalance {
    Sums::of(uses.iter().copied()).imbalance()
}

/// 대상 노드 하나의 사용률 재료([`node_use`]와 같은 셈을 옮기기마다 고친다)
#[derive(Debug, Clone)]
struct Slot {
    /// `inv.nodes`의 색인
    node: usize,
    /// CPU 사용률(0 아래로 내려갈 수 있고 사용률에서 자른다)
    cpu: f64,
    /// 요약의 사용 메모리 ± 옮긴 VM의 RSS(MiB, 요약에 없으면 None)
    actual: Option<f64>,
    /// 자리 잡은 VM의 `memory_mib` 합(MiB)
    allocated: f64,
    total: f64,
    /// 균형 옮기기를 받는다(리더가 오래 살아 있음을 봄)
    settled: bool,
    /// [`MIGRATION_SETTLE`] 안에 끝난 마이그레이션의 원본이나 대상이다(균형 옮기기의 원본·대상이 아님).
    migrated: bool,
}

impl Slot {
    fn usage(&self) -> NodeUse {
        let used = self.actual.unwrap_or(0.0).max(self.allocated);
        NodeUse {
            cpu: self.cpu.max(0.0),
            mem: (used / self.total).max(0.0),
        }
    }

    /// VM 하나(이 노드에서의 CPU 몫 `share`, RSS `rss`, 할당 `mem`)를 더하거나(`sign` 1) 뺀(-1) 것
    fn with(&self, share: f64, rss: f64, mem: f64, sign: f64) -> Self {
        Self {
            cpu: self.cpu + sign * share,
            actual: self.actual.map(|a| a + sign * rss),
            allocated: self.allocated + sign * mem,
            ..*self
        }
    }
}

/// 균형 조정이 보는 클러스터: 대상 노드와 그 사용률, 옮길 수 있는 VM
pub struct Model<'a> {
    inv: &'a Inventory,
    loads: &'a Loads,
    health: &'a Health<'a>,
    /// 살아 있는 노드와 균형 옮기기를 받을 만큼 오래 살아 있는지([`Model::new`])
    live: &'a BTreeMap<Uuid, bool>,
    /// 끝나지 않은 마이그레이션의 VM → 대상 노드
    assign: Assign,
    slots: Vec<Slot>,
    /// 노드 id → `slots` 색인
    index: BTreeMap<Uuid, usize>,
    /// 옮길 수 있는 VM(`inv.vms` 색인, 이름 순)
    movable: Vec<usize>,
}

/// 균형 조정의 대상 노드: 위트니스 아님·온라인·유지보수 아님·종료 표시 없음·HVF 아님·신선한 부하 요약이 있음·CPU 수와
/// 메모리를 앎
fn target_node(node: &Node, loads: &Loads) -> bool {
    !node.spec.witness
        && node.status.online
        && !node.spec.maintenance
        && node.status.shutdown.is_none()
        && node.status.hypervisor != HVF
        && loads.nodes.contains_key(&node.id)
        && node.status.cpus > 0
        && node.status.memory_mib > 0
}

impl<'a> Model<'a> {
    /// `tasks`는 클러스터의 모든 Task, `moved`는 이 리더가 옮긴 VM과 그 때(유닉스 초), `now`는 지금(유닉스 초)이다.
    /// `live`는 이 리더가 살아 있다고 보는 노드([`live`])와 그 노드가 균형 옮기기를 받을 만큼 오래(HA 되돌아가기 지연 +
    /// 60초) 살아 있는지다. 없는 노드는 규칙 바로잡기도 받지 않는다.
    pub fn new(
        inv: &'a Inventory,
        loads: &'a Loads,
        health: &'a Health<'a>,
        tasks: &[Task],
        moved: &BTreeMap<Uuid, u64>,
        now: u64,
        live: &'a BTreeMap<Uuid, bool>,
    ) -> Self {
        // 끝나지 않은 마이그레이션(Task나 `status.migration`)의 VM은 대상 노드에 있는 것으로 센다(같은 자리를 다시 약속하지
        // 않게).
        let migrating: BTreeMap<Uuid, Uuid> = tasks
            .iter()
            .filter(|t| !t.status.state.is_finished())
            .filter_map(|t| match t.spec.op {
                TaskOp::VmMigrate { node, .. } => Some((t.spec.target, node)),
                _ => None,
            })
            .collect();
        // 지난 [`MIGRATION_SETTLE`] 안에 만들어 끝난 마이그레이션의 원본·대상 노드
        let migrated: BTreeSet<Uuid> = tasks
            .iter()
            .filter(|t| {
                t.status.state.is_finished()
                    && created(t.id).saturating_add(MIGRATION_SETTLE.as_secs()) > now
            })
            .filter_map(|t| match t.spec.op {
                TaskOp::VmMigrate { node, .. } => Some([t.spec.node, node]),
                _ => None,
            })
            .flatten()
            .collect();
        let mut assign = Assign::default();
        for vm in inv
            .vms
            .iter()
            .filter(|v| v.spec.power == PowerState::Running)
        {
            let to = vm
                .status
                .migration
                .as_ref()
                .map(|m| m.node)
                .or_else(|| migrating.get(&vm.id).copied());
            if let Some(to) = to
                && vm.spec.node.is_some_and(|n| n != to)
            {
                assign.place(vm.id, to);
            }
        }
        let mut slots = Vec::new();
        let mut index = BTreeMap::new();
        for (i, node) in inv.nodes.iter().enumerate() {
            if !target_node(node, loads) {
                continue;
            }
            let used = node_use(node, inv, &assign, loads, None, None);
            index.insert(node.id, slots.len());
            slots.push(Slot {
                node: i,
                cpu: used.cpu,
                actual: loads.nodes[&node.id].mem_used_mib.map(|m| m as f64),
                allocated: placement::allocated_mib(node, inv, &assign, &[]) as f64,
                total: node.status.memory_mib as f64,
                settled: live.get(&node.id).copied().unwrap_or(false),
                migrated: migrated.contains(&node.id),
            });
        }
        // 끝나지 않았거나 쉬는 시간 안에 만든 Task만 VM마다 볼 필요가 있다.
        let relevant: Vec<Task> = tasks
            .iter()
            .filter(|t| {
                !t.status.state.is_finished()
                    || (matches!(
                        t.spec.op,
                        TaskOp::VmMigrate { .. } | TaskOp::VmHaRecover { .. }
                    ) && recent(created(t.id), now))
            })
            .cloned()
            .collect();
        let mut movable: Vec<usize> = (0..inv.vms.len())
            .filter(|&i| movable(&inv.vms[i], inv, &relevant, moved, now).is_ok())
            .collect();
        movable.sort_by(|a, b| inv.vms[*a].name.cmp(&inv.vms[*b].name));
        Self {
            inv,
            loads,
            health,
            live,
            assign,
            slots,
            index,
            movable,
        }
    }

    /// 대상 노드(신선한 요약이 있는 노드)의 id
    pub fn targets(&self) -> Vec<Uuid> {
        self.slots
            .iter()
            .map(|s| self.inv.nodes[s.node].id)
            .collect()
    }

    /// 옮길 수 있는 VM의 id(이름 순)
    pub fn movable(&self) -> Vec<Uuid> {
        self.movable.iter().map(|&i| self.inv.vms[i].id).collect()
    }

    /// 지금 사용률(대상 노드 순)
    pub fn uses(&self) -> Vec<NodeUse> {
        self.slots.iter().map(Slot::usage).collect()
    }

    fn rss(&self, vm: &Vm) -> f64 {
        self.loads.vm(vm.id).and_then(|l| l.mem).unwrap_or(0) as f64
    }

    fn share(&self, vm: &Vm, node: &Node) -> f64 {
        vm_cpu_share(vm, self.loads.vm(vm.id), node)
    }
}

/// UUIDv7 id에 담긴 만든 시각(유닉스 초). 시각이 없는 id는 가장 오래된 것으로 본다.
fn created(id: Uuid) -> u64 {
    id.get_timestamp().map_or(0, |ts| ts.to_unix().0)
}

/// `at`(유닉스 초)이 `now`에서 [`VM_COOLDOWN`] 안이다.
fn recent(at: u64, now: u64) -> bool {
    at.saturating_add(VM_COOLDOWN.as_secs()) > now
}

/// 균형 조정기가 이 VM을 옮길 수 있는지. 아니면 그 이유다(시험·debug 로그).
/// 자리 잡고 실행 중이고 마이그레이션 중이 아니며, 고정(`pinned`)이 아니고, 로컬 풀 디스크가 없고, 끝나지 않은
/// 스냅샷·되돌리기·복제·백업·전원·마이그레이션·디스크 크기 늘리기 Task가 없고, HA 끄기 요청·옮겨 달라는 요청이 없고,
/// 지난 [`VM_COOLDOWN`] 안에 만든 마이그레이션 Task·HA 복구 기록(`vm-ha-recover`)·HA 옮기기(`ha.relocations`)·이
/// 리더의 옮기기(`moved`)가 없고, 자리 잡은 hard `affinity` 상대가 없다(묶음은 옮기지 않는다).
pub fn movable(
    vm: &Vm,
    inv: &Inventory,
    tasks: &[Task],
    moved: &BTreeMap<Uuid, u64>,
    now: u64,
) -> Result<(), &'static str> {
    if vm.spec.power != PowerState::Running || vm.spec.node.is_none() {
        return Err("not placed");
    }
    if vm.status.phase != VmPhase::Running || vm.status.migration.is_some() {
        return Err("not running");
    }
    if vm.spec.pinned() {
        return Err("pinned");
    }
    if !disk_moves(&vm.spec, inv, None).is_empty() {
        return Err("has disks on a node-local pool");
    }
    let disks: BTreeSet<Uuid> = inv
        .volumes
        .iter()
        .filter(|v| vm.spec.disks.iter().any(|d| d.volume == v.name))
        .map(|v| v.id)
        .collect();
    let busy = tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && ((t.spec.target == vm.id
                && matches!(
                    t.spec.op,
                    TaskOp::SnapshotCreate { .. }
                        | TaskOp::SnapshotRevert { .. }
                        | TaskOp::VmClone { .. }
                        | TaskOp::VmBackup { .. }
                        | TaskOp::VmStart
                        | TaskOp::VmStop
                        | TaskOp::VmForceStop
                        | TaskOp::VmReboot
                        | TaskOp::VmMigrate { .. }
                ))
                || (disks.contains(&t.spec.target)
                    && matches!(t.spec.op, TaskOp::VolumeResize { .. })))
    });
    if busy {
        return Err("busy with a task");
    }
    let ha = vm.status.ha.as_ref();
    if ha.is_some_and(|h| h.evict.is_some() || h.relocate_from.is_some()) {
        return Err("being moved by HA");
    }
    let migrated = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && matches!(t.spec.op, TaskOp::VmMigrate { .. })
            && recent(created(t.id), now)
    });
    if migrated {
        return Err("migrated recently");
    }
    let recovered = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && matches!(t.spec.op, TaskOp::VmHaRecover { .. })
            && recent(created(t.id), now)
    });
    if recovered {
        return Err("recovered by HA recently");
    }
    if ha.is_some_and(|h| h.relocations.iter().any(|at| recent(*at, now))) {
        return Err("relocated by HA recently");
    }
    if moved.get(&vm.id).is_some_and(|at| recent(*at, now)) {
        return Err("moved by the balancer recently");
    }
    if partnered(vm, inv) {
        return Err("kept together with other VMs by an affinity rule");
    }
    Ok(())
}

/// 자리 잡은 hard `affinity` 상대가 있다(혼자 옮길 수 없다).
fn partnered(vm: &Vm, inv: &Inventory) -> bool {
    // hard `affinity` 규칙이 없는 VM(대부분)은 VM 목록을 훑지 않는다.
    let bound = inv.rules.iter().any(|r| {
        r.spec.enabled && r.spec.kind == RuleKind::Affinity && r.spec.has_vm(&vm.name, &vm.labels)
    });
    bound
        && placement::affinity_partners(vm, inv)
            .iter()
            .any(|v| v.spec.power == PowerState::Running && v.spec.node.is_some())
}

/// 이 리더가 노드를 살아 있다고 본다: HA 가능 노드는 리스가 `fence_after`보다 새롭고(HA의 `ready`), 아닌 노드는 리스를
/// [`LOAD_STALE`] 안에 갱신했다. `b`는 이 리더의 리스 기준점이다.
pub fn live(node: &Node, leases: &Leases, now: Instant, b: Instant) -> bool {
    if node.status.ha.is_some() {
        return ready(now, b, leases, node);
    }
    leases
        .renewed
        .get(&opl_cluster::types::NodeId(node.id))
        .is_some_and(|at| now.saturating_duration_since(*at) < LOAD_STALE)
}

/// 균형 옮기기의 대상 조건: 원본·대상이 HVF가 아니고, 대상에 VM 메모리만큼 미할당 메모리(`free_mib` — 계획한
/// 옮기기 반영)가 있고, 노드 조건([`node_fits`])에 맞고, 이 VM에 영향을 주는 HA 검사가 실패한 노드가 아니고,
/// `assign`(계획한 옮기기)으로 hard 규칙을 어기지 않고, soft 벌점이 지금보다 늘지 않고, HA 그룹 VM은 그룹 우선순위가
/// 지금 노드 이상이다(되돌아가기와 핑퐁하지 않게). `ha_reserve`는 메모리 합이라 옮겨도 바뀌지 않아 보지 않는다.
pub fn target_ok(
    vm: &Vm,
    from: &Node,
    to: &Node,
    inv: &Inventory,
    assign: &Assign,
    health: &Health,
    free_mib: i64,
) -> Result<(), &'static str> {
    if from.id == to.id {
        return Err("same node");
    }
    if from.status.hypervisor == HVF || to.status.hypervisor == HVF {
        return Err("macOS (HVF) node");
    }
    if free_mib < i64::from(vm.spec.memory_mib) {
        return Err("not enough unassigned memory");
    }
    if node_fits(&vm.spec, inv, to).is_err() {
        return Err("node does not fit");
    }
    if health.blocks(vm, to, inv) {
        return Err("a failing HA check affects the VM");
    }
    if !placement::hard_violations(vm, to, inv, assign).is_empty() {
        return Err("breaks a placement rule");
    }
    if placement::soft_penalty(vm, to, inv, assign) > placement::soft_penalty(vm, from, inv, assign)
    {
        return Err("raises the soft rule penalty");
    }
    if placement::priority(vm, to, inv) < placement::priority(vm, from, inv) {
        return Err("lower HA group priority");
    }
    Ok(())
}

/// 계획한 옮기기 하나
#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub vm: Uuid,
    pub from: Uuid,
    pub to: Uuid,
    pub reason: MoveReason,
    /// 불균형이 줄어든 양(규칙 바로잡기는 음수일 수 있다)
    pub gain: f64,
    /// 비용: `max(RSS, memory_mib)`
    pub cost_mib: u64,
    /// 규칙 바로잡기가 바로잡는 hard 규칙(이름 순)
    pub rules: Vec<String>,
}

/// 한 회차의 계획
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub before: Imbalance,
    pub after: Imbalance,
    pub moves: Vec<Move>,
    /// 균형 옮기기 중이다(불균형이 문턱을 넘어 시작했고 아직 문턱의 [`STOP_RATIO`] 아래가 아니다). 다음 회차의 `plan`에
    /// 넘긴다.
    pub balancing: bool,
}

/// 계획하는 동안 바뀌는 것: 사용률 재료, 계획한 자리, 계획한 VM
struct Work {
    slots: Vec<Slot>,
    assign: Assign,
    done: BTreeSet<usize>,
}

impl Work {
    fn imbalance(&self) -> Imbalance {
        Sums::of(self.slots.iter().map(Slot::usage)).imbalance()
    }
}

/// 균형 옮기기 후보(같은 효과면 비용 작은 것 → VM 이름 → 노드 id)
struct Best {
    gain: f64,
    cost_mib: u64,
    vm: usize,
    from: usize,
    to: usize,
}

impl Best {
    /// 나은 것이 `Less`
    fn compare(&self, other: &Best, inv: &Inventory) -> Ordering {
        other
            .gain
            .total_cmp(&self.gain)
            .then(self.cost_mib.cmp(&other.cost_mib))
            .then_with(|| inv.vms[self.vm].name.cmp(&inv.vms[other.vm].name))
            .then_with(|| {
                let id = |s: usize| inv.nodes[s].id;
                id(self.to).cmp(&id(other.to))
            })
    }
}

/// 옮기기를 많아야 `max_moves`개 고른다. 먼저 지금 노드에서 hard 규칙을 어기는 VM을 [`placement::candidates_with`]의
/// 가장 나은 노드로 옮기고(불균형·문턱과 상관없이), 그다음 정책이 `balance`이면 균형 옮기기를 한다: 불균형이 문턱을
/// 넘으면(지난 회차부터 균형 옮기기 중이면(`balancing`) 문턱의 [`STOP_RATIO`] 이상이면) 그 아래가 될 때까지 사용률이 큰
/// 노드 [`SOURCE_NODES`]개의 VM × 작은 노드 [`TARGET_NODES`]개(오래 살아 있는 노드만, 둘 다 마이그레이션이 막 끝난 노드는
/// 빼고) 가운데, 그렇지 않아도 사용률이
/// 평균보다 문턱의 [`HOT_RATIO`]배 넘게 높은 노드가 있으면 그 노드의 VM만 견줘, 원본·대상의 사용률 차이(CPU·메모리 가운데
/// 큰 것)가 문턱 이상이고 [`MIN_GAIN`]·[`MIN_GAIN_PER_GIB`]를 넘으며(노드 둘 기준) 불균형을 가장 많이 줄이는 옮기기를
/// 하나씩 고른다. 대상 노드가 둘 미만이면 빈 계획이다.
pub fn plan(
    model: &Model,
    settings: &PlacementSettings,
    max_moves: usize,
    balancing: bool,
) -> Plan {
    let mut work = Work {
        slots: model.slots.clone(),
        assign: model.assign.clone(),
        done: BTreeSet::new(),
    };
    let before = work.imbalance();
    let mut moves = Vec::new();
    if model.slots.len() < 2 {
        return Plan {
            before,
            after: before,
            moves,
            balancing: false,
        };
    }
    fix_rules(model, settings, max_moves, &mut work, &mut moves);
    let mut balancing = balancing;
    if settings.policy == PlacementPolicy::Balance {
        let threshold = f64::from(settings.threshold) / 100.0;
        let start = work.imbalance().total;
        balancing = if balancing {
            start >= threshold * STOP_RATIO
        } else {
            start > threshold
        };
        while moves.len() < max_moves {
            let now = work.imbalance();
            let source = if balancing && now.total >= threshold * STOP_RATIO {
                None
            } else if let Some(hot) = hot_slot(&work, threshold) {
                Some(hot)
            } else {
                break;
            };
            let Some(best) = best_move(model, &work, now, threshold, source) else {
                break;
            };
            let vm = &model.inv.vms[best.vm];
            let (from, to) = (&model.inv.nodes[best.from], &model.inv.nodes[best.to]);
            apply(model, &mut work, best.vm, from, to);
            moves.push(Move {
                vm: vm.id,
                from: from.id,
                to: to.id,
                reason: MoveReason::Balance,
                gain: best.gain,
                cost_mib: best.cost_mib,
                rules: Vec::new(),
            });
        }
    } else {
        balancing = false;
    }
    Plan {
        before,
        after: work.imbalance(),
        moves,
        balancing,
    }
}

/// 사용률(CPU나 메모리)이 평균보다 문턱의 [`HOT_RATIO`]배 넘게 높은 노드 가운데 가장 많이 넘는 것(슬롯 색인)
fn hot_slot(work: &Work, threshold: f64) -> Option<usize> {
    let uses: Vec<NodeUse> = work.slots.iter().map(Slot::usage).collect();
    let n = uses.len() as f64;
    let cpu = uses.iter().map(|u| u.cpu).sum::<f64>() / n;
    let mem = uses.iter().map(|u| u.mem).sum::<f64>() / n;
    uses.iter()
        .enumerate()
        .filter(|(i, _)| !work.slots[*i].migrated)
        .map(|(i, u)| (i, (u.cpu - cpu).max(u.mem - mem)))
        .filter(|(_, over)| *over > threshold * HOT_RATIO)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// 규칙 바로잡기: 옮길 수 있는 VM 가운데 살아 있는 노드에서 hard 규칙을 어기는 것을 이름 순으로 가장 나은 후보 노드(HVF
/// 아님·살아 있음·미할당 메모리 넉넉·HA 검사가 막지 않음)로 옮긴다. soft 위반은 바로잡지 않는다.
fn fix_rules(
    model: &Model,
    settings: &PlacementSettings,
    max_moves: usize,
    work: &mut Work,
    moves: &mut Vec<Move>,
) {
    let inv = model.inv;
    let scorer = Scorer {
        loads: model.loads,
        policy: settings.policy,
    };
    for &i in &model.movable {
        if moves.len() >= max_moves {
            return;
        }
        let vm = &inv.vms[i];
        let Some(from) = inv.nodes.iter().find(|n| Some(n.id) == vm.spec.node) else {
            continue;
        };
        // 살아 있지 않은 노드의 VM은 HA가 다룬다.
        if !model.live.contains_key(&from.id) {
            continue;
        }
        let rules = placement::hard_violations(vm, from, inv, &work.assign);
        if rules.is_empty() || from.status.hypervisor == HVF {
            continue;
        }
        let to = placement::candidates_with(vm, inv, &work.assign, &scorer)
            .into_iter()
            .filter_map(|c| inv.nodes.iter().find(|n| n.id == c.node))
            .find(|to| {
                let free = to.status.memory_mib as i64
                    - placement::allocated_mib(to, inv, &work.assign, &[vm.id]) as i64;
                to.id != from.id
                    && to.status.hypervisor != HVF
                    && model.live.contains_key(&to.id)
                    && free >= i64::from(vm.spec.memory_mib)
                    && !model.health.blocks(vm, to, inv)
            });
        let Some(to) = to else {
            continue;
        };
        let before = work.imbalance();
        apply(model, work, i, from, to);
        moves.push(Move {
            vm: vm.id,
            from: from.id,
            to: to.id,
            reason: MoveReason::Rule,
            gain: before.total - work.imbalance().total,
            cost_mib: cost_mib(model, vm),
            rules,
        });
    }
}

fn cost_mib(model: &Model, vm: &Vm) -> u64 {
    (model.rss(vm) as u64).max(u64::from(vm.spec.memory_mib))
}

/// 계획한 옮기기를 반영한다(대상 노드가 아닌 원본·대상은 사용률이 없다).
fn apply(model: &Model, work: &mut Work, vm_index: usize, from: &Node, to: &Node) {
    let vm = &model.inv.vms[vm_index];
    let (rss, mem) = (model.rss(vm), f64::from(vm.spec.memory_mib));
    if let Some(&s) = model.index.get(&from.id) {
        work.slots[s] = work.slots[s].with(model.share(vm, from), rss, mem, -1.0);
    }
    if let Some(&s) = model.index.get(&to.id) {
        work.slots[s] = work.slots[s].with(model.share(vm, to), rss, mem, 1.0);
    }
    work.assign.place(vm.id, to.id);
    work.done.insert(vm_index);
}

/// 받을 수 있는 가장 나은 균형 옮기기. `source`가 있으면 그 슬롯의 VM만 본다. 원본·대상의 사용률 차이(CPU·메모리 가운데
/// 큰 것)가 `threshold`(0~1)보다 작은 옮기기, 오래 살아 있지 않은 대상, 마이그레이션이 막 끝난 원본·대상은 보지 않는다.
fn best_move(
    model: &Model,
    work: &Work,
    now: Imbalance,
    threshold: f64,
    source: Option<usize>,
) -> Option<Best> {
    let inv = model.inv;
    let busy = |s: &Slot| {
        let u = s.usage();
        u.cpu + u.mem
    };
    // 사용률 순 슬롯 색인(같으면 노드 id 순)
    let mut order: Vec<usize> = (0..work.slots.len()).collect();
    order.sort_by(|a, b| {
        busy(&work.slots[*b])
            .total_cmp(&busy(&work.slots[*a]))
            .then_with(|| {
                inv.nodes[work.slots[*a].node]
                    .id
                    .cmp(&inv.nodes[work.slots[*b].node].id)
            })
    });
    let sources: Vec<usize> = match source {
        Some(s) => vec![s],
        None => order
            .iter()
            .filter(|s| !work.slots[**s].migrated)
            .take(SOURCE_NODES)
            .copied()
            .collect(),
    };
    let targets: Vec<usize> = order
        .iter()
        .rev()
        .filter(|t| work.slots[**t].settled && !work.slots[**t].migrated)
        .take(TARGET_NODES)
        .copied()
        .collect();
    let sums = Sums::of(work.slots.iter().map(Slot::usage));
    // 노드 둘 기준으로 견준다([`MIN_GAIN`]).
    let scale = work.slots.len() as f64 / 2.0;
    let mut best: Option<Best> = None;
    for &s in &sources {
        let from = &inv.nodes[work.slots[s].node];
        let vms = model
            .movable
            .iter()
            .filter(|i| !work.done.contains(i) && inv.vms[**i].spec.node == Some(from.id));
        for &i in vms {
            let vm = &inv.vms[i];
            let (rss, mem) = (model.rss(vm), f64::from(vm.spec.memory_mib));
            let cost_mib = cost_mib(model, vm);
            let left = work.slots[s].with(model.share(vm, from), rss, mem, -1.0);
            for &t in targets.iter().filter(|t| **t != s) {
                let (su, tu) = (work.slots[s].usage(), work.slots[t].usage());
                if (su.cpu - tu.cpu).max(su.mem - tu.mem) < threshold {
                    continue;
                }
                let to = &inv.nodes[work.slots[t].node];
                let came = work.slots[t].with(model.share(vm, to), rss, mem, 1.0);
                let mut after = sums;
                after.add(work.slots[s].usage(), -1.0);
                after.add(work.slots[t].usage(), -1.0);
                after.add(left.usage(), 1.0);
                after.add(came.usage(), 1.0);
                let gain = now.total - after.imbalance().total;
                let judged = gain * scale;
                if judged < MIN_GAIN || judged / (cost_mib as f64 / 1024.0) < MIN_GAIN_PER_GIB {
                    continue;
                }
                let found = Best {
                    gain,
                    cost_mib,
                    vm: i,
                    from: work.slots[s].node,
                    to: work.slots[t].node,
                };
                if best
                    .as_ref()
                    .is_some_and(|b| found.compare(b, inv) != Ordering::Less)
                {
                    continue;
                }
                let free = (work.slots[t].total - work.slots[t].allocated) as i64;
                if target_ok(vm, from, to, inv, &work.assign, model.health, free).is_ok() {
                    best = Some(found);
                }
            }
        }
    }
    best
}

/// 균형 조정기가 쉬는 이유(차례대로 첫 것): 모드 `off`, 기능 `placement`를 모르는 노드(`missing`), 유지보수(비우기)
/// 중이거나 종료 표시가 있는 노드, 복구가 남았을 수 있는 노드(HA VM이 배정된 HA 가능 노드인데 리스가 그 노드의
/// `fence_after`보다 오래됨 — HA의 `ready`의 반대), HA 되돌아가기를 기다리는 VM([`awaits_failback`]), 신선한 부하
/// 요약이 있는 대상 노드가 둘 미만, 끝나지 않은 마이그레이션 Task가 [`MAX_CONCURRENT`]개 이상. `b`는 이 리더의 리스
/// 기준점이고 `alive_since`는 노드마다 이 리더가 살아 있음을 계속 본 처음 때다.
#[allow(clippy::too_many_arguments)]
pub fn paused(
    inv: &Inventory,
    leases: &Leases,
    now: Instant,
    b: Instant,
    settings: &PlacementSettings,
    tasks: &[Task],
    missing: &[String],
    health: &Health,
    alive_since: &BTreeMap<Uuid, Instant>,
) -> Option<String> {
    if settings.mode == PlacementMode::Off {
        return Some("mode is off".into());
    }
    if !missing.is_empty() {
        return Some(format!(
            "some nodes do not support placement yet ({})",
            missing.join(", ")
        ));
    }
    let mut nodes: Vec<&Node> = inv.nodes.iter().collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(n) = nodes.iter().find(|n| n.spec.maintenance) {
        return Some(format!("node {:?} is in maintenance", n.name));
    }
    if let Some(n) = nodes.iter().find(|n| n.status.shutdown.is_some()) {
        return Some(format!("node {:?} is shutting down", n.name));
    }
    let recovering = nodes.iter().find(|n| {
        n.status.ha.is_some()
            && !ready(now, b, leases, n)
            && inv.vms.iter().any(|v| {
                v.spec.node == Some(n.id)
                    && v.spec.ha_active()
                    && v.spec.power == PowerState::Running
            })
    });
    if let Some(n) = recovering {
        return Some(format!(
            "node {:?} has not renewed its lease and its HA VMs may be recovering",
            n.name
        ));
    }
    let mut vms: Vec<&Vm> = inv.vms.iter().collect();
    vms.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(vm) = vms
        .into_iter()
        .find(|vm| awaits_failback(vm, inv, leases, now, b, health, alive_since))
    {
        return Some(format!("waiting for HA failback of VM {:?}", vm.name));
    }
    let loads = Loads::from_leases(leases, &inv.nodes, now);
    if nodes.iter().filter(|n| target_node(n, &loads)).count() < 2 {
        return Some("fewer than two nodes have a recent load summary".into());
    }
    let migrating = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished() && matches!(t.spec.op, TaskOp::VmMigrate { .. }))
        .count();
    if migrating >= MAX_CONCURRENT {
        return Some(format!("{migrating} migrations are in progress"));
    }
    None
}

/// 켜진 HA 그룹 VM을 HA가 곧 되돌린다(HA 되돌아가기의 거름과 같다): 그룹이 되돌아가기를 하고(`nofailback` 아님), VM에
/// 끄기 요청(`ha.evict`)·자리 잡은 hard `affinity` 상대가 없고, 지금 노드보다 그룹 우선순위가 높고 살아 있고(HA의
/// `ready`) HVF가 아니고 미할당 메모리가 넉넉하고 노드 조건·hard 규칙에 맞고 이 VM에 영향을 주는 HA 검사가 실패하지
/// 않은 노드가 있으며, 이 리더가 그 노드가 살아 있음을 본 지 [`FAILBACK_WAIT`]가 지나지 않았다. 그런 VM이 있으면 균형
/// 조정기는 HA가 되돌리기를 기다린다(돌아온 노드를 먼저 채우지 않게). 창 안에서 옮긴 VM(HA가 되돌리지 않음)은 기다리지
/// 않는다.
fn awaits_failback(
    vm: &Vm,
    inv: &Inventory,
    leases: &Leases,
    now: Instant,
    b: Instant,
    health: &Health,
    alive_since: &BTreeMap<Uuid, Instant>,
) -> bool {
    if !vm.spec.ha_active()
        || vm.spec.power != PowerState::Running
        || vm.status.phase != VmPhase::Running
        || vm.status.migration.is_some()
        || vm.status.ha.as_ref().is_some_and(|ha| ha.evict.is_some())
    {
        return false;
    }
    let unix = crate::auth::unix_now();
    let relocated = vm.status.ha.as_ref().is_some_and(|ha| {
        ha.relocations
            .iter()
            .any(|at| at.saturating_add(RELOCATE_WINDOW.as_secs()) > unix)
    });
    let Some(name) = vm.spec.ha.as_ref().and_then(|ha| ha.group.as_ref()) else {
        return false;
    };
    let Some(group) = inv.groups.iter().find(|g| &g.name == name) else {
        return false;
    };
    let Some(current) = inv.nodes.iter().find(|n| Some(n.id) == vm.spec.node) else {
        return false;
    };
    if relocated || group.spec.nofailback || partnered(vm, inv) {
        return false;
    }
    let here = group.spec.priority(&current.name);
    let none = Assign::default();
    inv.nodes.iter().any(|n| {
        n.id != current.id
            && group.spec.priority(&n.name) > here
            && ready(now, b, leases, n)
            && alive_since
                .get(&n.id)
                .is_some_and(|since| now.saturating_duration_since(*since) < FAILBACK_WAIT)
            && n.status.hypervisor != HVF
            && inv.free_memory_mib(n, vm.id) >= i64::from(vm.spec.memory_mib)
            && node_fits(&vm.spec, inv, n).is_ok()
            && placement::hard_violations(vm, n, inv, &none).is_empty()
            && !health.blocks(vm, n, inv)
    })
}

#[cfg(test)]
mod tests {
    use opl_cluster::LoadReport;
    use opl_cluster::types::NodeId;
    use opl_types::ha_check::{CheckReport, CheckState, HaCheckRole, HaCheckSpec, HaCheckTarget};
    use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
    use opl_types::placement::{
        HaGroupNode, HaGroupSpec, LoadSummary, PlacementRule, PlacementRuleSpec, VmLoad,
    };
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
    use opl_types::task::{TaskSpec, TaskState};
    use opl_types::vm::{
        Arch, CpuSpec, DiskBus, DiskSpec, HaEvict, MachineType, VmHa, VmHaStatus, VmPlacement,
        VmSpec,
    };

    use super::*;
    use crate::auth::unix_now;
    use crate::score::NodeLoad;

    fn node(name: &str, cpus: u32, memory_mib: u64) -> Node {
        let mut n = Resource::new(
            name,
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status = NodeStatus {
            online: true,
            cpus,
            memory_mib,
            arch: Some(Arch::Aarch64),
            ha: Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
                fence_after_secs: Some(10),
            }),
            ..Default::default()
        };
        n
    }

    /// 노드 `node`에서 실행 중인 VM
    fn vm(name: &str, vcpus: u16, memory_mib: u32, node: &Node) -> Vm {
        let mut v: Vm = Resource::new(
            name,
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: vcpus,
                    model: Default::default(),
                },
                memory_mib,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![],
                nics: vec![],
                power: PowerState::Running,
                restart: Default::default(),
                node: Some(node.id),
                snapshot_schedule: None,
                ha: None,
                placement: None,
            },
        );
        v.status.phase = VmPhase::Running;
        v.status.node = Some(node.id);
        v
    }

    fn with_disk(mut v: Vm, volume: &str) -> Vm {
        v.spec.disks.push(DiskSpec {
            volume: volume.into(),
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        });
        v
    }

    fn pool(name: &str, shared: bool) -> StoragePool {
        Resource::new(
            name,
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: format!("/srv/{name}").into(),
                },
                nodes: vec![],
                shared,
                backups: false,
            },
        )
    }

    fn volume(name: &str, pool: &str) -> Volume {
        Resource::new(
            name,
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1,
                format: ImageFormat::Qcow2,
            },
        )
    }

    fn rule(name: &str, kind: RuleKind, vms: &[&str], nodes: &[&str]) -> PlacementRule {
        Resource::new(
            name,
            PlacementRuleSpec {
                kind,
                vms: vms.iter().map(|v| v.to_string()).collect(),
                vm_tag: None,
                nodes: nodes.iter().map(|n| n.to_string()).collect(),
                node_tag: None,
                weight: None,
                enabled: true,
                ha_may_violate: false,
            },
        )
    }

    fn task(op: TaskOp, target: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target,
                node: Uuid::nil(),
            },
        );
        t.status.state = state;
        t
    }

    /// `ago`초 전에 만든 Task
    fn task_made(op: TaskOp, target: Uuid, state: TaskState, ago: u64) -> Task {
        let mut t = task(op, target, state);
        t.id = Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            unix_now() - ago,
            0,
        ));
        t
    }

    fn migrate() -> TaskOp {
        TaskOp::VmMigrate {
            node: Uuid::nil(),
            pool: None,
        }
    }

    /// 노드 부하: 5분 CPU, 사용 메모리, VM마다 (VM, CPU ‰, RSS)
    fn load(cpu5: f64, mem: Option<u64>, vms: &[(&Vm, u16, u64)]) -> NodeLoad {
        NodeLoad {
            cpu1: Some(cpu5),
            cpu5: Some(cpu5),
            mem_used_mib: mem,
            vms: vms
                .iter()
                .map(|(v, cpu, rss)| {
                    let l = VmLoad {
                        id: v.id,
                        cpu: Some(*cpu),
                        mem: Some(*rss),
                    };
                    (v.id, l)
                })
                .collect(),
        }
    }

    fn loads(nodes: Vec<(&Node, NodeLoad)>) -> Loads {
        Loads::new(nodes.into_iter().map(|(n, l)| (n.id, l)).collect())
    }

    fn settings(threshold: u8) -> PlacementSettings {
        PlacementSettings {
            threshold,
            ..Default::default()
        }
    }

    /// 모든 노드가 오래 살아 있다.
    fn all_live(inv: &Inventory) -> BTreeMap<Uuid, bool> {
        inv.nodes.iter().map(|n| (n.id, true)).collect()
    }

    fn plan_of(inv: &Inventory, loads: &Loads, settings: &PlacementSettings, max: usize) -> Plan {
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        let live = all_live(inv);
        let model = Model::new(
            inv,
            loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        plan(&model, settings, max, false)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn imbalance_is_the_mean_of_the_cpu_and_memory_deviations() {
        let u = |cpu, mem| NodeUse { cpu, mem };
        let same = imbalance(&[u(0.5, 0.3), u(0.5, 0.3), u(0.5, 0.3)]);
        assert!(same.total < 1e-6, "{same:?}");
        let i = imbalance(&[u(0.2, 0.4), u(0.8, 0.4)]);
        assert!(close(i.cpu, 0.3) && i.mem < 1e-6, "{i:?}");
        assert!(close(i.total, (i.cpu + i.mem) / 2.0), "{i:?}");
        let i = imbalance(&[u(0.2, 0.1), u(0.8, 0.5)]);
        assert!(close(i.cpu, 0.3) && close(i.mem, 0.2) && close(i.total, 0.25));
        assert_eq!(imbalance(&[]), Imbalance::default());
    }

    #[test]
    fn a_vm_moves_from_the_busy_node_until_the_imbalance_is_under_the_threshold() {
        let (a, b) = (node("a", 4, 8192), node("b", 4, 8192));
        let (a1, a2) = (vm("a1", 1, 1024, &a), vm("a2", 1, 1024, &a));
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            vms: vec![a2.clone(), a1.clone()],
            ..Default::default()
        };
        let loads = loads(vec![
            (
                &a,
                load(0.8, Some(2048), &[(&a1, 800, 512), (&a2, 800, 512)]),
            ),
            (&b, load(0.2, Some(2048), &[])),
        ]);
        // 모델의 사용률은 점수 함수와 같은 셈이다.
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        let live = all_live(&inv);
        let model = Model::new(
            &inv,
            &loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        let none = Assign::default();
        for (n, u) in [&a, &b].into_iter().zip(model.uses()) {
            assert_eq!(u, node_use(n, &inv, &none, &loads, None, None));
        }
        let p = plan(&model, &settings(10), 5, false);
        // CPU 0.8/0.2 → 0.6/0.4, 메모리 0.25/0.25 → 0.1875/0.3125: 불균형 0.15 → 0.08125.
        assert!(close(p.before.total, 0.15), "{p:?}");
        assert!(close(p.after.total, 0.08125), "{p:?}");
        assert_eq!(
            p.moves,
            [Move {
                vm: a1.id,
                from: a.id,
                to: b.id,
                reason: MoveReason::Balance,
                gain: p.moves[0].gain,
                cost_mib: 1024,
                rules: vec![],
            }]
        );
        assert!(close(p.moves[0].gain, 0.15 - 0.08125));
        // 문턱 아래로 내려갔으므로 둘째 VM은 옮기지 않는다(옮기면 더 나빠지기도 한다).
        assert!(p.after.total * 100.0 <= 10.0);
    }

    #[test]
    fn a_small_vm_wins_over_a_big_one_whose_gain_per_gib_is_too_low() {
        let (a, b) = (node("a", 64, 262144), node("b", 64, 262144));
        let big = vm("big", 8, 32768, &a);
        let small = vm("small", 4, 1024, &a);
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            vms: vec![big.clone(), small.clone()],
            ..Default::default()
        };
        let loads = loads(vec![
            (
                &a,
                load(0.6, None, &[(&big, 1000, 1000), (&small, 1000, 500)]),
            ),
            (&b, load(0.4, None, &[])),
        ]);
        let p = plan_of(&inv, &loads, &settings(5), 5);
        // 큰 VM은 효과(약 0.0395)가 더 크지만 32 GiB에 견주면 GiB당 0.0012라 받지 않는다.
        assert_eq!(p.moves.len(), 1, "{p:?}");
        assert_eq!((p.moves[0].vm, p.moves[0].cost_mib), (small.id, 1024));
        assert!(p.moves[0].gain > MIN_GAIN);
    }

    #[test]
    fn moves_below_the_minimum_gain_are_not_taken() {
        let (a, b) = (node("a", 64, 262144), node("b", 64, 262144));
        let v = vm("v", 1, 1024, &a);
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            vms: vec![v.clone()],
            ..Default::default()
        };
        let loads = loads(vec![
            (&a, load(0.53, None, &[(&v, 200, 512)])),
            (&b, load(0.47, None, &[])),
        ]);
        let p = plan_of(&inv, &loads, &settings(1), 5);
        // 불균형(약 1.6%)은 문턱을 넘지만 옮겨도 0.0016만 줄어든다.
        assert!(p.before.total * 100.0 > 1.0, "{p:?}");
        assert!(p.moves.is_empty(), "{p:?}");
        assert_eq!(p.before, p.after);
    }

    #[test]
    fn a_balanced_cluster_gets_an_empty_plan() {
        let (a, b) = (node("a", 4, 8192), node("b", 4, 8192));
        let v = vm("v", 1, 1024, &a);
        let w = vm("w", 1, 1024, &b);
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            vms: vec![v.clone(), w.clone()],
            ..Default::default()
        };
        let loads = loads(vec![
            (&a, load(0.5, Some(2048), &[(&v, 500, 512)])),
            (&b, load(0.5, Some(2048), &[(&w, 500, 512)])),
        ]);
        let p = plan_of(&inv, &loads, &settings(1), 5);
        assert!(p.moves.is_empty(), "{p:?}");
    }

    /// 16코어 노드 A(CPU 90%)에 2 vCPU VM 여섯, 한가한 B·C
    fn crowded() -> (Inventory, Loads) {
        let (a, b, c) = (
            node("a", 16, 65536),
            node("b", 16, 65536),
            node("c", 16, 65536),
        );
        let vms: Vec<Vm> = (0..6).map(|i| vm(&format!("v{i}"), 2, 512, &a)).collect();
        let on_a: Vec<(&Vm, u16, u64)> = vms.iter().map(|v| (v, 1000, 256)).collect();
        let loads = loads(vec![
            (&a, load(0.9, None, &on_a)),
            (&b, load(0.1, None, &[])),
            (&c, load(0.1, None, &[])),
        ]);
        let inv = Inventory {
            nodes: vec![a, b, c],
            vms,
            ..Default::default()
        };
        (inv, loads)
    }

    #[test]
    fn at_most_max_moves_are_planned_and_each_vm_once() {
        let (inv, loads) = crowded();
        let a = inv.nodes[0].id;
        let p = plan_of(&inv, &loads, &settings(1), 2);
        assert_eq!(p.moves.len(), 2, "{p:?}");
        assert!(p.moves.iter().all(|m| m.from == a && m.to != a));
        assert_ne!(p.moves[0].vm, p.moves[1].vm);
        // 옮긴 뒤 다시 셌다: 둘째는 첫째가 간 노드가 아닌 다른 한가한 노드로 간다.
        assert_ne!(p.moves[0].to, p.moves[1].to);
        assert!(p.after.total < p.before.total);
        assert!(plan_of(&inv, &loads, &settings(1), 0).moves.is_empty());
    }

    #[test]
    fn vms_the_balancer_must_not_move() {
        let now = unix_now();
        let (a, b) = (node("a", 4, 8192), node("b", 4, 8192));
        let mut inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            pools: vec![pool("nas", true), pool("local", false)],
            volumes: vec![volume("d-nas", "nas"), volume("d-local", "local")],
            rules: vec![rule(
                "together",
                RuleKind::Affinity,
                &["bound", "mate"],
                &[],
            )],
            ..Default::default()
        };
        let none = BTreeMap::new();
        let check = |inv: &Inventory, v: &Vm, tasks: &[Task], moved: &BTreeMap<Uuid, u64>| {
            movable(v, inv, tasks, moved, now)
        };
        let ok = with_disk(vm("ok", 1, 1024, &a), "d-nas");
        assert_eq!(check(&inv, &ok, &[], &none), Ok(()));

        let mut stopped = ok.clone();
        stopped.spec.power = PowerState::Stopped;
        assert_eq!(check(&inv, &stopped, &[], &none), Err("not placed"));
        let mut starting = ok.clone();
        starting.status.phase = VmPhase::Starting;
        assert_eq!(check(&inv, &starting, &[], &none), Err("not running"));

        let mut pinned = ok.clone();
        pinned.spec.placement = Some(VmPlacement { pinned: true });
        assert_eq!(check(&inv, &pinned, &[], &none), Err("pinned"));

        let local = with_disk(vm("local", 1, 1024, &a), "d-local");
        assert_eq!(
            check(&inv, &local, &[], &none),
            Err("has disks on a node-local pool")
        );

        // 끝나지 않은 백업·스냅샷·크기 늘리기 Task가 있으면 옮기지 않고, 끝난 것은 상관없다.
        let backup = TaskOp::VmBackup {
            backup: Uuid::nil(),
        };
        let snapshot = TaskOp::SnapshotCreate {
            snapshot: Uuid::nil(),
        };
        for op in [backup.clone(), snapshot] {
            let t = task(op, ok.id, TaskState::Running);
            assert_eq!(check(&inv, &ok, &[t], &none), Err("busy with a task"));
        }
        let resize = TaskOp::VolumeResize { size_bytes: 2 };
        let t = task(resize, inv.volumes[0].id, TaskState::Pending);
        assert_eq!(check(&inv, &ok, &[t], &none), Err("busy with a task"));
        let done = task(backup, ok.id, TaskState::Succeeded);
        assert_eq!(check(&inv, &ok, &[done], &none), Ok(()));

        // 1시간 안에 만든 마이그레이션 Task(끝났어도), HA 옮기기, 이 리더의 옮기기
        let t = task_made(migrate(), ok.id, TaskState::Succeeded, 59 * 60);
        assert_eq!(check(&inv, &ok, &[t], &none), Err("migrated recently"));
        let t = task_made(migrate(), ok.id, TaskState::Succeeded, 61 * 60);
        assert_eq!(check(&inv, &ok, &[t], &none), Ok(()));
        // HA 복구 기록(처음부터 끝난 Task)도 1시간 안이면 옮기지 않는다. 모델도 끝난 기록을 본다.
        let recover = TaskOp::VmHaRecover { from: b.id };
        let t = task_made(recover.clone(), ok.id, TaskState::Succeeded, 30 * 60);
        assert_eq!(
            check(&inv, &ok, std::slice::from_ref(&t), &none),
            Err("recovered by HA recently")
        );
        let mut with_ok = inv.clone();
        with_ok.vms = vec![ok.clone()];
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        let live = all_live(&with_ok);
        let empty = Loads::default();
        let model = Model::new(&with_ok, &empty, &health, &[t], &none, now, &live);
        assert!(model.movable().is_empty());
        let t = task_made(recover, ok.id, TaskState::Succeeded, 61 * 60);
        assert_eq!(check(&inv, &ok, &[t], &none), Ok(()));
        let mut relocated = ok.clone();
        relocated.status.ha = Some(VmHaStatus {
            relocations: vec![now - 30 * 60],
            ..Default::default()
        });
        assert_eq!(
            check(&inv, &relocated, &[], &none),
            Err("relocated by HA recently")
        );
        let moved = BTreeMap::from([(ok.id, now - 10 * 60)]);
        assert_eq!(
            check(&inv, &ok, &[], &moved),
            Err("moved by the balancer recently")
        );
        let moved = BTreeMap::from([(ok.id, now - 2 * 3600)]);
        assert_eq!(check(&inv, &ok, &[], &moved), Ok(()));

        let mut evicted = ok.clone();
        evicted.status.ha = Some(VmHaStatus {
            evict: Some(HaEvict {
                from: a.id,
                check: "nas".into(),
                reason: "down".into(),
                at: now,
            }),
            ..Default::default()
        });
        assert_eq!(check(&inv, &evicted, &[], &none), Err("being moved by HA"));

        // 자리 잡은 hard affinity 상대가 있으면 옮기지 않는다(꺼진 상대는 상관없다).
        let bound = vm("bound", 1, 1024, &a);
        let mut mate = vm("mate", 1, 1024, &b);
        inv.vms = vec![bound.clone(), mate.clone()];
        assert_eq!(
            check(&inv, &bound, &[], &none),
            Err("kept together with other VMs by an affinity rule")
        );
        mate.spec.power = PowerState::Stopped;
        inv.vms = vec![bound.clone(), mate];
        assert_eq!(check(&inv, &bound, &[], &none), Ok(()));
    }

    #[test]
    fn a_pinned_vm_on_the_busy_node_stays() {
        let (mut inv, loads) = crowded();
        for v in &mut inv.vms {
            v.spec.placement = Some(VmPlacement { pinned: true });
        }
        assert!(plan_of(&inv, &loads, &settings(1), 5).moves.is_empty());
    }

    #[test]
    fn targets_that_break_rules_fail_checks_or_lack_memory_are_not_taken() {
        let (a, b, c) = (node("a", 4, 8192), node("b", 4, 8192), node("c", 4, 8192));
        let v = with_disk(vm("v", 1, 1024, &a), "d-nas");
        let mut inv = Inventory {
            nodes: vec![a.clone(), b.clone(), c.clone()],
            pools: vec![pool("nas", true)],
            volumes: vec![volume("d-nas", "nas")],
            vms: vec![v.clone(), vm("w", 1, 1024, &b)],
            ..Default::default()
        };
        let reports = BTreeMap::new();
        let healthy = Health::new(vec![], &reports);
        let none = Assign::default();
        let ok = |inv: &Inventory, to: &Node, health: &Health, free| {
            target_ok(&inv.vms[0], &a, to, inv, &none, health, free)
        };
        assert_eq!(ok(&inv, &b, &healthy, 8192), Ok(()));
        assert_eq!(ok(&inv, &a, &healthy, 8192), Err("same node"));
        assert_eq!(
            ok(&inv, &b, &healthy, 1023),
            Err("not enough unassigned memory")
        );
        let mut hvf = b.clone();
        hvf.status.hypervisor = HVF.into();
        assert_eq!(ok(&inv, &hvf, &healthy, 8192), Err("macOS (HVF) node"));
        let mut down = b.clone();
        down.spec.maintenance = true;
        assert_eq!(ok(&inv, &down, &healthy, 8192), Err("node does not fit"));

        // anti-affinity 짝(w)이 있는 노드 b
        inv.rules = vec![rule("apart", RuleKind::AntiAffinity, &["v", "w"], &[])];
        assert_eq!(ok(&inv, &b, &healthy, 8192), Err("breaks a placement rule"));
        assert_eq!(ok(&inv, &c, &healthy, 8192), Ok(()));

        // 이 VM의 풀을 보는 storage 검사가 c에서 실패한다.
        let nas = HaCheckSpec {
            role: HaCheckRole::Storage,
            target: HaCheckTarget::Pool { name: "nas".into() },
            on_fail: None,
            grace_secs: 10,
            nodes: BTreeMap::new(),
            enabled: true,
        };
        let failing = BTreeMap::from([(
            NodeId(c.id),
            vec![CheckReport::new("nas", CheckState::Failing, None, None)],
        )]);
        let sick = Health::new(vec![Resource::new("nas", nas)], &failing);
        assert_eq!(
            ok(&inv, &c, &sick, 8192),
            Err("a failing HA check affects the VM")
        );

        // soft 벌점이 느는 노드: v는 a를 바란다.
        inv.rules = vec![rule("near-a", RuleKind::SoftNodeAffinity, &["v"], &["a"])];
        assert_eq!(
            ok(&inv, &c, &healthy, 8192),
            Err("raises the soft rule penalty")
        );

        // HA 그룹 우선순위가 지금 노드보다 낮은 노드(그룹 밖 노드는 가장 낮다)
        inv.rules.clear();
        inv.groups = vec![Resource::new(
            "g",
            HaGroupSpec {
                nodes: vec![
                    HaGroupNode {
                        node: "a".into(),
                        priority: 2,
                    },
                    HaGroupNode {
                        node: "b".into(),
                        priority: 2,
                    },
                    HaGroupNode {
                        node: "c".into(),
                        priority: 1,
                    },
                ],
                restricted: false,
                nofailback: false,
            },
        )];
        inv.vms[0].spec.ha = Some(VmHa {
            group: Some("g".into()),
            ..Default::default()
        });
        assert_eq!(ok(&inv, &b, &healthy, 8192), Ok(()));
        assert_eq!(ok(&inv, &c, &healthy, 8192), Err("lower HA group priority"));

        // 계획: 가장 한가한 c가 HA 검사로 막히면 b로 간다.
        let (b_load, c_load) = (load(0.3, None, &[]), load(0.1, None, &[]));
        let loads = loads(vec![
            (&a, load(0.9, None, &[(&inv.vms[0], 1000, 512)])),
            (&b, b_load),
            (&c, c_load),
        ]);
        inv.groups.clear();
        inv.vms[0].spec.ha = None;
        let live = all_live(&inv);
        let model = Model::new(
            &inv,
            &loads,
            &sick,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        let p = plan(&model, &settings(1), 1, false);
        assert_eq!(
            p.moves.iter().map(|m| (m.vm, m.to)).collect::<Vec<_>>(),
            [(v.id, b.id)]
        );
        let live = all_live(&inv);
        let model = Model::new(
            &inv,
            &loads,
            &healthy,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        assert_eq!(plan(&model, &settings(1), 1, false).moves[0].to, c.id);
    }

    #[test]
    fn a_vm_on_a_node_its_hard_rules_forbid_moves_regardless_of_the_threshold() {
        let (mut inv, loads) = crowded();
        let (a, c) = (inv.nodes[0].clone(), inv.nodes[2].clone());
        inv.rules = vec![rule("on-c", RuleKind::NodeAffinity, &["v3"], &["c"])];
        let v3 = inv.vms[3].id;
        let fix = Move {
            vm: v3,
            from: a.id,
            to: c.id,
            reason: MoveReason::Rule,
            gain: 0.0,
            cost_mib: 512,
            rules: vec!["on-c".into()],
        };
        let same = |m: &Move| Move {
            gain: 0.0,
            ..m.clone()
        };
        // 문턱이 높아(50%) 균형 옮기기는 없고 규칙 바로잡기만 있다.
        let p = plan_of(&inv, &loads, &settings(50), 5);
        assert_eq!(p.moves.len(), 1, "{p:?}");
        assert_eq!(same(&p.moves[0]), fix);
        // 바로잡기가 먼저이고 그다음 균형 옮기기
        let p = plan_of(&inv, &loads, &settings(1), 5);
        assert!(p.moves.len() > 1, "{p:?}");
        assert_eq!(same(&p.moves[0]), fix);
        assert!(p.moves[1..].iter().all(|m| m.reason == MoveReason::Balance));
        // pack이면 균형 옮기기 없이 바로잡기만
        let pack = PlacementSettings {
            policy: PlacementPolicy::Pack,
            ..settings(1)
        };
        let p = plan_of(&inv, &loads, &pack, 5);
        assert_eq!(p.moves.iter().map(same).collect::<Vec<_>>(), [fix]);
    }

    #[test]
    fn nodes_without_a_summary_are_neither_sources_nor_targets() {
        let (inv, loads) = crowded();
        let (a, b, c) = (&inv.nodes[0], &inv.nodes[1], &inv.nodes[2]);
        let mut nodes = loads.nodes.clone();
        nodes.remove(&b.id);
        let partial = Loads::new(nodes);
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        let live = all_live(&inv);
        let model = Model::new(
            &inv,
            &partial,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        assert_eq!(model.targets(), [a.id, c.id]);
        let p = plan(&model, &settings(1), 5, false);
        assert!(!p.moves.is_empty() && p.moves.iter().all(|m| m.to == c.id));
        // 대상이 하나뿐이면 아무것도 하지 않는다(규칙 바로잡기도).
        let mut inv = inv.clone();
        inv.rules = vec![rule("on-c", RuleKind::NodeAffinity, &["v3"], &["c"])];
        let mut nodes = loads.nodes.clone();
        nodes.retain(|id, _| *id == a.id);
        let p = plan_of(&inv, &Loads::new(nodes), &settings(1), 5);
        assert_eq!(p, Plan::default());
    }

    #[test]
    fn the_balancer_pauses_for_these_reasons() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(100);
        let (mut a, b) = (node("a", 4, 8192), node("b", 4, 8192));
        let mut ha_vm = vm("h", 1, 1024, &a);
        ha_vm.spec.ha = Some(VmHa::default());
        let mut inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            vms: vec![ha_vm],
            ..Default::default()
        };
        let mut leases = Leases::default();
        for n in [&a, &b] {
            leases
                .renewed
                .insert(NodeId(n.id), now - Duration::from_secs(1));
            let report = LoadReport {
                at: now - Duration::from_secs(1),
                load: LoadSummary {
                    cpu5: Some(100),
                    ..Default::default()
                },
            };
            leases.loads.insert(NodeId(n.id), report);
        }
        let ok = PlacementSettings::default();
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        // 이 리더가 노드들이 살아 있음을 본 지 1분
        let mut alive_since: BTreeMap<Uuid, Instant> = [&a, &b]
            .iter()
            .map(|n| (n.id, now - Duration::from_secs(60)))
            .collect();
        let at = |inv: &Inventory,
                  leases: &Leases,
                  s: &PlacementSettings,
                  tasks: &[Task],
                  missing: &[String],
                  alive_since: &BTreeMap<Uuid, Instant>| {
            paused(
                inv,
                leases,
                now,
                t0,
                s,
                tasks,
                missing,
                &health,
                alive_since,
            )
        };
        assert_eq!(at(&inv, &leases, &ok, &[], &[], &alive_since), None);

        let off = PlacementSettings {
            mode: PlacementMode::Off,
            ..ok
        };
        assert_eq!(
            at(&inv, &leases, &off, &[], &[], &alive_since).as_deref(),
            Some("mode is off")
        );
        assert_eq!(
            at(&inv, &leases, &ok, &[], &["c".into()], &alive_since).as_deref(),
            Some("some nodes do not support placement yet (c)")
        );

        let mut drained = inv.clone();
        drained.nodes[1].spec.maintenance = true;
        assert_eq!(
            at(&drained, &leases, &ok, &[], &[], &alive_since).as_deref(),
            Some("node \"b\" is in maintenance")
        );

        // HA VM이 있는 노드의 리스가 fence_after(10초)보다 오래됐다. HA VM이 없으면 상관없다.
        let mut stale = leases.clone();
        stale
            .renewed
            .insert(NodeId(a.id), now - Duration::from_secs(11));
        assert_eq!(
            at(&inv, &stale, &ok, &[], &[], &alive_since).as_deref(),
            Some("node \"a\" has not renewed its lease and its HA VMs may be recovering")
        );
        let mut empty = inv.clone();
        empty.vms.clear();
        assert_eq!(at(&empty, &stale, &ok, &[], &[], &alive_since), None);

        // 신선한 요약이 있는 대상 노드가 하나뿐이다.
        let mut old = leases.clone();
        old.loads.get_mut(&NodeId(b.id)).unwrap().at = now - Duration::from_secs(31);
        assert_eq!(
            at(&inv, &old, &ok, &[], &[], &alive_since).as_deref(),
            Some("fewer than two nodes have a recent load summary")
        );
        a.spec.witness = true;
        inv.nodes[0] = a;
        assert!(at(&inv, &leases, &ok, &[], &[], &alive_since).is_some());
        inv.nodes[0].spec.witness = false;

        // HA 그룹의 더 우선인 산 노드(b)로 되돌아갈 VM이 있으면 HA를 기다린다. nofailback이면 기다리지 않는다.
        let mut grouped = inv.clone();
        grouped.groups = vec![Resource::new(
            "g",
            HaGroupSpec {
                nodes: vec![
                    HaGroupNode {
                        node: "a".into(),
                        priority: 1,
                    },
                    HaGroupNode {
                        node: "b".into(),
                        priority: 2,
                    },
                ],
                restricted: false,
                nofailback: false,
            },
        )];
        grouped.vms[0].spec.ha = Some(VmHa {
            group: Some("g".into()),
            ..Default::default()
        });
        assert_eq!(
            at(&grouped, &leases, &ok, &[], &[], &alive_since).as_deref(),
            Some("waiting for HA failback of VM \"h\"")
        );
        // HA가 되돌리지 못하면 기다리지 않는다: 더 우선인 노드에 메모리가 모자람, HVF 노드, 끄기 요청이 있는 VM, b가 살아
        // 있음을 본 지 10분이 넘음(HA가 되돌리지 못하는 것으로 봄).
        let mut full = grouped.clone();
        full.vms.push(vm("big", 1, 7680, &b));
        assert_eq!(at(&full, &leases, &ok, &[], &[], &alive_since), None);
        let mut hvf = grouped.clone();
        hvf.nodes[1].status.hypervisor = HVF.into();
        // (HVF 노드는 대상 노드도 아니다.)
        assert_eq!(
            at(&hvf, &leases, &ok, &[], &[], &alive_since).as_deref(),
            Some("fewer than two nodes have a recent load summary")
        );
        let mut evicted = grouped.clone();
        evicted.vms[0].status.ha = Some(VmHaStatus {
            evict: Some(HaEvict {
                from: b.id,
                check: "c".into(),
                reason: "down".into(),
                at: 0,
            }),
            ..Default::default()
        });
        assert_eq!(at(&evicted, &leases, &ok, &[], &[], &alive_since), None);
        alive_since.insert(b.id, now - FAILBACK_WAIT);
        assert_eq!(at(&grouped, &leases, &ok, &[], &[], &alive_since), None);
        alive_since.insert(b.id, now - Duration::from_secs(60));
        grouped.groups[0].spec.nofailback = true;
        assert_eq!(at(&grouped, &leases, &ok, &[], &[], &alive_since), None);

        // 끝나지 않은 마이그레이션 둘(누가 만들었든)
        let one = task(migrate(), Uuid::now_v7(), TaskState::Running);
        let two = task(migrate(), Uuid::now_v7(), TaskState::Pending);
        let done = task(migrate(), Uuid::now_v7(), TaskState::Succeeded);
        let tasks = [one.clone(), done];
        assert_eq!(at(&inv, &leases, &ok, &tasks, &[], &alive_since), None);
        assert_eq!(
            at(&inv, &leases, &ok, &[one, two], &[], &alive_since).as_deref(),
            Some("2 migrations are in progress")
        );
    }

    /// 노드 둘(CPU 4개, 메모리 64 GiB): a에 VM 하나(vCPU 하나를 다 씀 — 몫 25%)
    fn pair(cpu_a: f64, cpu_b: f64) -> (Inventory, Loads) {
        let (a, b) = (node("a", 4, 65536), node("b", 4, 65536));
        let v = vm("v", 1, 1024, &a);
        let loads = loads(vec![
            (&a, load(cpu_a, None, &[(&v, 1000, 512)])),
            (&b, load(cpu_b, None, &[])),
        ]);
        let inv = Inventory {
            nodes: vec![a, b],
            vms: vec![v],
            ..Default::default()
        };
        (inv, loads)
    }

    fn plan_with(inv: &Inventory, loads: &Loads, threshold: u8, balancing: bool) -> Plan {
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        let live = all_live(inv);
        let model = Model::new(
            inv,
            loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        plan(&model, &settings(threshold), 5, balancing)
    }

    #[test]
    fn balancing_starts_over_the_threshold_and_goes_on_to_four_fifths_of_it() {
        // CPU 78%·22%: 불균형 약 14.4%. 문턱 15%를 넘지 않아 시작하지 않는다(문턱 근처 잡음).
        let (inv, loads) = pair(0.78, 0.22);
        let p = plan_with(&inv, &loads, 15, false);
        assert!(p.before.total < 0.15 && p.before.total > 0.12, "{p:?}");
        assert!(p.moves.is_empty() && !p.balancing, "{p:?}");
        // 지난 회차부터 균형 옮기기 중이면 문턱의 0.8배(12%)까지 계속한다.
        let p = plan_with(&inv, &loads, 15, true);
        assert_eq!(p.moves.len(), 1, "{p:?}");
        assert!(p.balancing);
        // 문턱을 넘으면 시작한다.
        let (inv, loads) = pair(0.85, 0.15);
        let p = plan_with(&inv, &loads, 15, false);
        assert_eq!(p.moves.len(), 1, "{p:?}");
        assert!(p.balancing);
        // 0.8배 아래면 균형 옮기기를 마친다.
        let (inv, loads) = pair(0.7, 0.3);
        let p = plan_with(&inv, &loads, 15, true);
        assert!(p.moves.is_empty() && !p.balancing, "{p:?}");
    }

    #[test]
    fn a_move_between_nodes_closer_than_the_threshold_is_not_taken() {
        // CPU·메모리 사용률: a 95%(옮길 VM 없음), b 50%(VM v — CPU·메모리 6%), c 38%. 불균형(약 24.5%)은 문턱을 넘고
        // b → c는 효과가 받는 만큼이지만 b·c의 차이(12%)가 문턱(15%)보다 작다.
        let mib = 16384.0;
        let (a, b, c) = (
            node("a", 4, 16384),
            node("b", 4, 16384),
            node("c", 4, 16384),
        );
        let v = vm("v", 1, 983, &b);
        let used = |u: f64| Some((u * mib) as u64);
        let loads = loads(vec![
            (&a, load(0.95, used(0.95), &[])),
            (&b, load(0.5, used(0.5), &[(&v, 240, 983)])),
            (&c, load(0.38, used(0.38), &[])),
        ]);
        let inv = Inventory {
            nodes: vec![a, b, c.clone()],
            vms: vec![v.clone()],
            ..Default::default()
        };
        let p = plan_with(&inv, &loads, 15, false);
        assert!(p.before.total > 0.15, "{p:?}");
        assert!(p.moves.is_empty(), "{p:?}");
        // 문턱이 10%면 차이가 넘으므로 옮긴다.
        let p = plan_with(&inv, &loads, 10, false);
        assert_eq!(
            p.moves.iter().map(|m| (m.vm, m.to)).collect::<Vec<_>>(),
            [(v.id, c.id)]
        );
    }

    #[test]
    fn one_hot_node_among_many_is_relieved() {
        // 노드 열: 하나만 CPU 90%(VM 넷이 vCPU를 다 씀), 나머지 아홉은 30%. 표준편차가 작아 불균형(약 9%)은 문턱(15%)
        // 아래지만 그 노드는 평균(36%)보다 30%p 넘게 높다.
        let nodes: Vec<Node> = (0..10).map(|i| node(&format!("n{i}"), 4, 65536)).collect();
        let hot = nodes[0].clone();
        let vms: Vec<Vm> = (0..4)
            .map(|i| vm(&format!("v{i}"), 1, 1024, &hot))
            .collect();
        let on_hot: Vec<(&Vm, u16, u64)> = vms.iter().map(|v| (v, 1000, 512)).collect();
        let mut entries = vec![(&nodes[0], load(0.9, None, &on_hot))];
        for n in &nodes[1..] {
            entries.push((n, load(0.3, None, &[])));
        }
        let loads = loads(entries);
        let inv = Inventory {
            nodes: nodes.clone(),
            vms,
            ..Default::default()
        };
        let p = plan_with(&inv, &loads, 15, false);
        assert!(p.before.total < 0.15, "{p:?}");
        assert!(!p.moves.is_empty(), "{p:?}");
        assert!(p.moves.iter().all(|m| m.from == hot.id), "{p:?}");
        assert!(!p.balancing);
    }

    #[test]
    fn nodes_not_alive_long_enough_take_no_moves() {
        let (inv, loads) = crowded();
        let (b, c) = (inv.nodes[1].id, inv.nodes[2].id);
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        // b는 막 돌아왔다(살아 있지만 오래되지 않음): 균형 옮기기는 c로만 간다.
        let mut live = all_live(&inv);
        live.insert(b, false);
        let model = Model::new(
            &inv,
            &loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        let p = plan(&model, &settings(1), 5, false);
        assert!(
            !p.moves.is_empty() && p.moves.iter().all(|m| m.to == c),
            "{p:?}"
        );
        // 살아 있지 않은 노드(c)는 규칙 바로잡기도 받지 않는다.
        let mut inv = inv.clone();
        inv.rules = vec![rule("on-c", RuleKind::NodeAffinity, &["v3"], &["c"])];
        let mut live = all_live(&inv);
        live.remove(&c);
        let model = Model::new(
            &inv,
            &loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        let p = plan(&model, &settings(50), 5, false);
        assert!(p.moves.is_empty(), "{p:?}");
        // 살아 있지 않은 원본(a)의 VM도 바로잡지 않는다(HA가 다룬다).
        let mut live = all_live(&inv);
        live.remove(&inv.nodes[0].id);
        let model = Model::new(
            &inv,
            &loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        let p = plan(&model, &settings(50), 5, false);
        assert!(p.moves.is_empty(), "{p:?}");
    }

    /// 마이그레이션이 끝난 지 5분이 지나지 않은 원본·대상 노드는 균형 옮기기의 원본·대상이 아니다(5분 평균 CPU가 옮긴 VM을
    /// 아직 원본에 센다). 규칙 바로잡기는 그대로다.
    #[test]
    fn nodes_of_a_just_finished_migration_take_no_balance_moves() {
        let (inv, loads) = crowded();
        let (a, b, c) = (inv.nodes[0].id, inv.nodes[1].id, inv.nodes[2].id);
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        let live = all_live(&inv);
        let plan_after = |tasks: &[Task], inv: &Inventory, threshold: u8| {
            let model = Model::new(
                inv,
                &loads,
                &health,
                tasks,
                &BTreeMap::new(),
                unix_now(),
                &live,
            );
            plan(&model, &settings(threshold), 5, false)
        };
        // 다른 VM(x)이 b → a로 옮겨 막 끝났다: a(원본 후보)와 b가 빠져 옮기기가 없다.
        let finished = |ago: u64| {
            let mut t = task_made(
                TaskOp::VmMigrate {
                    node: a,
                    pool: None,
                },
                Uuid::now_v7(),
                TaskState::Succeeded,
                ago,
            );
            t.spec.node = b;
            t
        };
        assert!(plan_after(&[], &inv, 1).moves.iter().any(|m| m.from == a));
        let p = plan_after(&[finished(60)], &inv, 1);
        assert!(p.moves.is_empty(), "{p:?}");
        // 5분이 지났으면 다시 옮긴다.
        let p = plan_after(&[finished(301)], &inv, 1);
        assert!(!p.moves.is_empty(), "{p:?}");
        // c → b로 끝났으면 받을 노드가 없고, c만 대상이었으면(원본은 모름) b로 간다.
        let mut to_b = finished(60);
        to_b.spec.op = TaskOp::VmMigrate {
            node: b,
            pool: None,
        };
        to_b.spec.node = c;
        let p = plan_after(&[to_b.clone()], &inv, 1);
        assert!(p.moves.is_empty(), "{p:?}");
        let mut c_only = to_b;
        c_only.spec.node = Uuid::nil();
        c_only.spec.op = TaskOp::VmMigrate {
            node: c,
            pool: None,
        };
        let p = plan_after(&[c_only.clone()], &inv, 1);
        assert!(
            !p.moves.is_empty() && p.moves.iter().all(|m| m.from == a && m.to == b),
            "{p:?}"
        );
        // 규칙 바로잡기는 그대로다.
        let mut ruled = inv.clone();
        ruled.rules = vec![rule("on-c", RuleKind::NodeAffinity, &["v3"], &["c"])];
        let p = plan_after(&[finished(60)], &ruled, 50);
        assert_eq!(
            p.moves.iter().map(|m| m.reason).collect::<Vec<_>>(),
            [MoveReason::Rule]
        );
    }

    #[test]
    fn a_migration_in_flight_holds_memory_on_its_target() {
        // a(바쁨)의 v를 옮기면 가장 한가한 b가 낫지만, b로 가는 중인 x(7 GiB)가 b의 메모리를 거의 다 차지한다.
        let (a, b, c) = (node("a", 4, 8192), node("b", 4, 8192), node("c", 4, 8192));
        let v = vm("v", 1, 1024, &a);
        let x = vm("x", 1, 7168, &a);
        let loads = loads(vec![
            (&a, load(0.9, None, &[(&v, 1000, 512)])),
            (&b, load(0.1, None, &[])),
            (&c, load(0.3, None, &[])),
        ]);
        let inv = Inventory {
            nodes: vec![a, b.clone(), c.clone()],
            vms: vec![v.clone(), x.clone()],
            ..Default::default()
        };
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        let live = all_live(&inv);
        let going = task(
            TaskOp::VmMigrate {
                node: b.id,
                pool: None,
            },
            x.id,
            TaskState::Running,
        );
        let model = Model::new(
            &inv,
            &loads,
            &health,
            &[going],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        let p = plan(&model, &settings(1), 1, false);
        assert_eq!(
            p.moves.iter().map(|m| (m.vm, m.to)).collect::<Vec<_>>(),
            [(v.id, c.id)]
        );
        // `status.migration`도 같다.
        let mut inv = inv.clone();
        inv.vms[1].status.migration = Some(opl_types::vm::MigrationStatus {
            node: b.id,
            task: Uuid::nil(),
            disks: vec![],
        });
        let model = Model::new(
            &inv,
            &loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        assert_eq!(plan(&model, &settings(1), 1, false).moves[0].to, c.id);
        // 마이그레이션이 없으면 b다.
        inv.vms[1].status.migration = None;
        let model = Model::new(
            &inv,
            &loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        assert_eq!(plan(&model, &settings(1), 1, false).moves[0].to, b.id);
    }

    #[test]
    fn planning_300_nodes_and_6000_vms_is_fast() {
        let nodes: Vec<Node> = (0..300)
            .map(|i| node(&format!("n{i:03}"), 64, 262144))
            .collect();
        let mut vms = Vec::new();
        let mut entries = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let on: Vec<Vm> = (0..20)
                .map(|j| vm(&format!("v{i:03}-{j:02}"), 4, 4096 + 1024 * (j % 4), n))
                .collect();
            let cpu5 = 0.1 + 0.8 * ((i * 37) % 300) as f64 / 300.0;
            let mem = 40960 + 2048 * ((i * 53) % 60) as u64;
            let l = load(
                cpu5,
                Some(mem),
                &on.iter()
                    .enumerate()
                    .map(|(j, v)| (v, ((i * 7 + j * 131) % 1000) as u16, 2048))
                    .collect::<Vec<_>>(),
            );
            entries.push((n.id, l));
            vms.extend(on);
        }
        let inv = Inventory {
            nodes,
            vms,
            ..Default::default()
        };
        let loads = Loads::new(entries.into_iter().collect());
        let reports = BTreeMap::new();
        let health = Health::new(vec![], &reports);
        let started = std::time::Instant::now();
        let live = all_live(&inv);
        let model = Model::new(
            &inv,
            &loads,
            &health,
            &[],
            &BTreeMap::new(),
            unix_now(),
            &live,
        );
        let built = started.elapsed();
        let started = std::time::Instant::now();
        let p = plan(&model, &settings(1), MAX_RECOMMENDATIONS, false);
        let planned = started.elapsed();
        eprintln!(
            "balance: model {built:?}, plan {planned:?} for 300 nodes and 6000 VMs, {} moves, imbalance {:.4} -> {:.4}",
            p.moves.len(),
            p.before.total,
            p.after.total
        );
        // 노드가 많아 옮기기 하나의 효과가 작아도 노드 둘 기준으로 견주므로 옮기기를 찾는다.
        assert_eq!(p.moves.len(), MAX_RECOMMENDATIONS, "{p:?}");
        assert!(p.after.total < p.before.total);
        assert!(planned < Duration::from_secs(2), "{planned:?}");
    }
}
