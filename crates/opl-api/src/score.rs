//! 부하를 본 배치 점수(설계 §8.3). 노드가 리스 갱신에 실어 보낸 부하 요약(리더 메모리)으로 노드 사용률을 셈하고
//! 하나의 점수 함수로 처음 놓기·HA 복구·재시작 옮기기·상태 검사·되돌아가기·비우기 대상의 순서를 정한다.
//! 노드 조건과 hard 규칙은 그대로 거르는 쪽이고, 점수는 순서만 바꾼다.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::http::StatusCode;
use opl_cluster::Leases;
use opl_cluster::types::NodeId;
use opl_types::node::{Node, NodeSpec};
use opl_types::placement::{PlacementStatus, VmLoad};
use opl_types::settings::PlacementPolicy;
use opl_types::vm::Vm;
use tokio::time::Instant;
use uuid::Uuid;

use crate::error::ApiError;
use crate::placement::{self, Assign};
use crate::placement_board::placement_status;
use crate::scheduler::Inventory;
use crate::state::AppState;

/// 이보다 오래된 부하 요약은 쓰지 않는다(그 노드는 요약이 없는 것으로 본다).
pub const LOAD_STALE: Duration = Duration::from_secs(30);
/// 요약 속 VM 부하는 이보다 오래되지 않았으면 쓴다(노드 부하는 [`LOAD_STALE`]). 죽은 노드의 VM을 다른 노드에서 다시 켤
/// 때 리스가 끊긴 지 30초가 넘어도(`fence_after`가 긴 프로필) 그 VM의 CPU를 센다.
pub const VM_LOAD_STALE: Duration = Duration::from_secs(300);

/// `balance` 점수에서 CPU 여유의 가중치
pub const CPU_WEIGHT: f64 = 1.0;
/// `balance` 점수에서 메모리 여유의 가중치
pub const MEMORY_WEIGHT: f64 = 1.0;
/// `pack`이 넘치지 않는다고 보는 CPU 사용률 상한
pub const PACK_CPU_LIMIT: f64 = 0.8;
/// `pack`이 넘치지 않는다고 보는 메모리 사용률 상한
pub const PACK_MEMORY_LIMIT: f64 = 0.9;

/// 한 노드의 부하(요약에서)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeLoad {
    /// 지난 1분·5분 노드 CPU 사용률(0~1, 모든 코어 합)
    pub cpu1: Option<f64>,
    pub cpu5: Option<f64>,
    /// 사용 메모리
    pub mem_used_mib: Option<u64>,
    /// 이 노드에서 도는 VM들의 부하
    pub vms: BTreeMap<Uuid, VmLoad>,
}

/// 점수가 보는 노드별 부하. 요약이 없는 노드는 없다. 기본값은 아무것도 모름이다(할당 메모리만 본다).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Loads {
    pub nodes: BTreeMap<Uuid, NodeLoad>,
    /// `cpu5`가 있는 노드들의 평균(없으면 0). 요약이 없는 노드의 CPU로 쓴다.
    mean_cpu5: f64,
    /// VM id → 그 VM의 부하
    by_vm: BTreeMap<Uuid, VmLoad>,
}

/// 아무것도 모르는 부하([`Scorer::default`])
static UNKNOWN: Loads = Loads {
    nodes: BTreeMap::new(),
    mean_cpu5: 0.0,
    by_vm: BTreeMap::new(),
};

impl Loads {
    /// 노드 CPU(`cpu5`)도 사용 메모리도 없는 노드(시작 직후·`/proc` 읽기 실패)는 요약이 없는 것으로 본다(VM 부하는 쓴다).
    pub fn new(mut nodes: BTreeMap<Uuid, NodeLoad>) -> Self {
        let by_vm = nodes
            .values()
            .flat_map(|n| n.vms.iter().map(|(id, l)| (*id, l.clone())))
            .collect();
        nodes.retain(|_, n| n.cpu5.is_some() || n.mem_used_mib.is_some());
        let known: Vec<f64> = nodes.values().filter_map(|n| n.cpu5).collect();
        let mean_cpu5 = if known.is_empty() {
            0.0
        } else {
            known.iter().sum::<f64>() / known.len() as f64
        };
        Self {
            nodes,
            mean_cpu5,
            by_vm,
        }
    }

    /// 리더의 리스 표에서 `now`에 [`LOAD_STALE`]보다 오래되지 않은 요약만. 위트니스(`nodes`에서 본다)의 요약은 쓰지
    /// 않는다(VM을 받지 않고, 다른 노드가 RPC로 읽는 상태에도 없어 계산이 같게). VM 부하는 [`VM_LOAD_STALE`]보다
    /// 오래되지 않은 요약에서도 가져온다(새 요약의 것이 앞선다).
    pub fn from_leases(leases: &Leases, nodes: &[Node], now: Instant) -> Self {
        let permille = |v: Option<u16>| v.map(|p| f64::from(p) / 1000.0);
        let witness = |id: &NodeId| nodes.iter().any(|n| n.id == id.0 && n.spec.witness);
        let nodes = leases
            .loads
            .iter()
            .filter(|(id, r)| now.saturating_duration_since(r.at) < LOAD_STALE && !witness(id))
            .map(|(node, r)| {
                let load = NodeLoad {
                    cpu1: permille(r.load.cpu1),
                    cpu5: permille(r.load.cpu5),
                    mem_used_mib: r.load.mem,
                    vms: r.load.vms.iter().map(|v| (v.id, v.clone())).collect(),
                };
                (node.0, load)
            })
            .collect();
        let mut loads = Self::new(nodes);
        let older = leases.loads.values().filter(|r| {
            let age = now.saturating_duration_since(r.at);
            age >= LOAD_STALE && age < VM_LOAD_STALE
        });
        for v in older.flat_map(|r| &r.load.vms) {
            loads.by_vm.entry(v.id).or_insert_with(|| v.clone());
        }
        loads
    }

    /// VM 부하를 모두 뺀 것(노드 부하만). 비우기는 옮기는 VM 자신의 부하를 모름으로 둔다.
    pub fn without_vms(self) -> Self {
        let nodes = self
            .nodes
            .into_iter()
            .map(|(id, n)| {
                let load = NodeLoad {
                    vms: BTreeMap::new(),
                    ..n
                };
                (id, load)
            })
            .collect();
        Self::new(nodes)
    }

    /// 이 VM의 부하(모르면 None)
    pub fn vm(&self, id: Uuid) -> Option<&VmLoad> {
        self.by_vm.get(&id)
    }

    /// 리더의 최적 배치 상태(노드 사이 RPC의 답)에서. 신선한 요약이 있는 노드의 노드 부하만 있다(VM 부하는 없다).
    pub fn from_status(status: &PlacementStatus) -> Self {
        let permille = |v: Option<u16>| v.map(|p| f64::from(p) / 1000.0);
        let nodes = status
            .nodes
            .iter()
            .filter(|n| n.age_secs.is_some())
            .map(|n| {
                let load = NodeLoad {
                    cpu1: permille(n.cpu1_pct),
                    cpu5: permille(n.cpu5_pct),
                    mem_used_mib: n.mem_used_mib,
                    vms: BTreeMap::new(),
                };
                (n.node, load)
            })
            .collect();
        Self::new(nodes)
    }
}

/// 배치 점수를 매기는 것. 기본값은 아무것도 모르는 부하와 `balance`다.
#[derive(Debug, Clone, Copy)]
pub struct Scorer<'a> {
    pub loads: &'a Loads,
    pub policy: PlacementPolicy,
}

impl Default for Scorer<'_> {
    fn default() -> Self {
        Self {
            loads: &UNKNOWN,
            policy: PlacementPolicy::default(),
        }
    }
}

/// [`Scorer`]가 빌리는 부하와 정책(API가 잠그기 전에 읽어 둔다)
#[derive(Debug, Clone, Default)]
pub struct Scoring {
    pub loads: Loads,
    pub policy: PlacementPolicy,
}

impl Scoring {
    pub fn scorer(&self) -> Scorer<'_> {
        Scorer {
            loads: &self.loads,
            policy: self.policy,
        }
    }
}

/// 노드 사용률(0~1, 넘칠 수 있음)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeUse {
    pub cpu: f64,
    pub mem: f64,
}

/// VM이 이 노드의 CPU에서 차지할 몫: VM의 5분 CPU 사용률 × vCPU / 노드 CPU 수. 부하나 CPU 수를 모르면 0이다.
pub fn vm_cpu_share(vm: &Vm, load: Option<&VmLoad>, node: &Node) -> f64 {
    let Some(cpu) = load.and_then(|l| l.cpu) else {
        return 0.0;
    };
    if node.status.cpus == 0 {
        return 0.0;
    }
    let vcpus = f64::from(vm.spec.cpu.sockets) * f64::from(vm.spec.cpu.cores);
    f64::from(cpu) / 1000.0 * vcpus / f64::from(node.status.cpus)
}

/// `adding`을 놓고 `without`을 뺀 뒤의 노드 사용률.
/// CPU는 노드의 `cpu5`(없으면 요약이 있는 노드들의 평균, 아무도 없으면 0)에 VM 몫을 더하고 뺀다. [`Assign`]으로 이 노드에
/// 놓인 다른 VM의 몫도 더하고, 이 노드에서 다른 곳으로 놓인 VM의 몫은 뺀다(한 번에 여러 VM을 놓을 때 — 메모리와 같다).
/// 메모리는 `max(실제, 할당) / 노드 메모리`다. 실제는 요약의 사용 메모리 ± VM RSS(요약이 없으면 할당만),
/// 할당은 자리 잡은 VM의 `memory_mib` 합([`Assign`]을 따름) ± 놓을 VM의 `memory_mib`이다.
pub fn node_use(
    node: &Node,
    inv: &Inventory,
    assign: &Assign,
    loads: &Loads,
    adding: Option<&Vm>,
    without: Option<&Vm>,
) -> NodeUse {
    let load = loads.nodes.get(&node.id);
    let share = |vm: &Vm| vm_cpu_share(vm, loads.vm(vm.id), node);
    let rss = |vm: &Vm| loads.vm(vm.id).and_then(|l| l.mem).unwrap_or(0) as f64;
    let mut cpu = load.and_then(|l| l.cpu5).unwrap_or(loads.mean_cpu5);
    cpu += adding.map_or(0.0, share);
    cpu -= without.map_or(0.0, share);
    let skip: Vec<Uuid> = adding.iter().chain(without.iter()).map(|v| v.id).collect();
    for (id, placed) in assign.iter().filter(|(id, _)| !skip.contains(id)) {
        let Some(vm) = inv.vms.iter().find(|v| v.id == id) else {
            continue;
        };
        // 이 노드에서 도는 VM의 몫은 이미 노드 `cpu5`에 있다.
        let here =
            vm.spec.node == Some(node.id) && vm.spec.power == opl_types::vm::PowerState::Running;
        match (here, placed == Some(node.id)) {
            (false, true) => cpu += share(vm),
            (true, false) => cpu -= share(vm),
            _ => {}
        }
    }

    let allocated = placement::allocated_mib(node, inv, assign, &skip) as f64
        + adding.map_or(0.0, |v| f64::from(v.spec.memory_mib));
    let used = match load.and_then(|l| l.mem_used_mib) {
        Some(actual) => {
            let actual = actual as f64 + adding.map_or(0.0, rss) - without.map_or(0.0, rss);
            actual.max(allocated)
        }
        None => allocated,
    };
    let mem = match node.status.memory_mib {
        0 => 1.0,
        total => used / total as f64,
    };
    NodeUse {
        cpu: cpu.max(0.0),
        mem: mem.max(0.0),
    }
}

/// 사용률의 점수(백만분의 일 점, 클수록 나음).
/// `balance`: CPU·메모리 여유의 가중 합. `pack`: 넘치지 않는 노드(CPU [`PACK_CPU_LIMIT`]·메모리
/// [`PACK_MEMORY_LIMIT`] 이하)는 많이 쓴 것이 앞(`cpu + mem`), 넘치는 노드는 모두 그 뒤에 덜 쓴 것이 앞이다.
pub fn score(after: NodeUse, policy: PlacementPolicy) -> i64 {
    let points = match policy {
        PlacementPolicy::Balance => {
            CPU_WEIGHT * (1.0 - after.cpu) + MEMORY_WEIGHT * (1.0 - after.mem)
        }
        PlacementPolicy::Pack if after.cpu <= PACK_CPU_LIMIT && after.mem <= PACK_MEMORY_LIMIT => {
            after.cpu + after.mem
        }
        PlacementPolicy::Pack => -10.0 + (2.0 - after.cpu - after.mem),
    };
    (points * 1_000_000.0).round() as i64
}

/// 리더가 아닌 노드가 점수용 부하를 리더에 묻는 제한 시간(넘으면 부하 없이 놓는다)
const SCORER_READ_TIMEOUT: Duration = Duration::from_secs(1);
/// 리더에 묻지 못하면 이 동안 다시 묻지 않고 부하 없이 놓는다(리더가 느릴 때 VM 시작마다 기다리지 않게).
const SCORER_READ_BACKOFF: Duration = Duration::from_secs(5);
/// `GET /placement`가 리더에 묻는 제한 시간
const STATUS_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// 이 노드의 API가 점수에 쓸 부하와 정책. 부하는 노드를 고를 수 있을 때(`picks`)만 읽는다: 리더면 리스에서, 아니면
/// 리더에 노드 사이 RPC로 묻고, 묻지 못하면 모름(할당 메모리만)이고 [`SCORER_READ_BACKOFF`] 동안 다시 묻지 않는다.
/// 잠그기(`vm_lock`) 전에 읽는다.
pub async fn scorer(state: &AppState, picks: bool) -> Scoring {
    let policy = crate::settings::current(&state.cluster)
        .map(|s| s.spec.placement().policy)
        .unwrap_or_default();
    let cluster = &state.cluster;
    let loads = if !picks {
        Loads::default()
    } else if cluster.members().leader == Some(cluster.id()) {
        let nodes = cluster
            .list::<NodeSpec>()
            .map(|(_, n)| n)
            .unwrap_or_default();
        Loads::from_leases(&cluster.leases(), &nodes, Instant::now())
    } else if state
        .placement_backoff
        .lock()
        .unwrap()
        .is_some_and(|until| Instant::now() < until)
    {
        Loads::default()
    } else {
        match read_leader(state, SCORER_READ_TIMEOUT).await {
            Ok(status) => Loads::from_status(&status),
            Err(e) => {
                tracing::debug!("placing without load summaries: {e}");
                *state.placement_backoff.lock().unwrap() =
                    Some(Instant::now() + SCORER_READ_BACKOFF);
                Loads::default()
            }
        }
    };
    Scoring { loads, policy }
}

/// 최적 배치 상태: 리더면 직접 만들고, 아니면 리더에 노드 사이 RPC로 묻는다(못 읽으면 503 `placement_unavailable`).
pub async fn status(state: &AppState) -> Result<PlacementStatus, ApiError> {
    let cluster = &state.cluster;
    if cluster.members().leader == Some(cluster.id()) {
        return Ok(placement_status(cluster, &state.placement)?);
    }
    read_leader(state, STATUS_READ_TIMEOUT).await.map_err(|e| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "placement_unavailable",
            format!("cannot read the placement status from the leader: {e}"),
        )
    })
}

/// 리더에 최적 배치 상태를 묻는다. 실패 이유를 돌려준다.
async fn read_leader(state: &AppState, timeout: Duration) -> Result<PlacementStatus, String> {
    let members = state.cluster.members();
    let leader = members.leader.ok_or("no leader")?;
    let addr = members.addr(leader).ok_or("the leader has no address")?;
    let peers = state.peers.as_ref().ok_or("no node-to-node connection")?;
    match tokio::time::timeout(timeout, peers.read_placement(leader, addr, timeout)).await {
        Ok(Ok(Some(status))) => Ok(status),
        Ok(Ok(None)) => Err("the leader does not support placement".into()),
        Ok(Err(status)) => Err(status.message().to_string()),
        Err(_) => Err("timed out".into()),
    }
}

#[cfg(test)]
mod tests {
    use opl_cluster::LoadReport;
    use opl_cluster::types::NodeId;
    use opl_types::node::{NodeSpec, NodeStatus};
    use opl_types::placement::{LoadSummary, NodeLoadView};
    use opl_types::resource::Resource;
    use opl_types::vm::{Arch, CpuSpec, MachineType, PowerState, VmSpec};

    use super::*;

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
            ..Default::default()
        };
        n
    }

    fn vm(name: &str, vcpus: u16, memory_mib: u32, node: Option<&Node>) -> Vm {
        Resource::new(
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
                node: node.map(|n| n.id),
                snapshot_schedule: None,
                ha: None,
                placement: None,
            },
        )
    }

    fn load(cpu5: Option<f64>, mem_used_mib: Option<u64>, vms: &[(Uuid, u16, u64)]) -> NodeLoad {
        NodeLoad {
            cpu1: cpu5,
            cpu5,
            mem_used_mib,
            vms: vms
                .iter()
                .map(|(id, cpu, mem)| {
                    let l = VmLoad {
                        id: *id,
                        cpu: Some(*cpu),
                        mem: Some(*mem),
                    };
                    (*id, l)
                })
                .collect(),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn memory_is_the_larger_of_actual_and_allocated() {
        let n = node("n", 4, 8192);
        let me = vm("me", 1, 1024, None);
        let inv = Inventory {
            nodes: vec![n.clone()],
            vms: vec![vm("web", 1, 2048, Some(&n))],
            ..Default::default()
        };
        let none = Assign::default();
        // 실제(6144 + 놓을 VM의 RSS 512)가 할당(2048 + 1024)보다 크다.
        let mut nodes = BTreeMap::new();
        nodes.insert(n.id, load(Some(0.0), Some(6144), &[]));
        // 놓을 VM은 다른 노드(9)에서 돈다.
        nodes.insert(Uuid::from_u128(9), load(None, None, &[(me.id, 0, 512)]));
        let loads = Loads::new(nodes);
        let u = node_use(&n, &inv, &none, &loads, Some(&me), None);
        assert!(close(u.mem, (6144.0 + 512.0) / 8192.0), "{u:?}");
        // 할당(2048 + 1024)이 실제(1000 + 512)보다 크다.
        let mut nodes = loads.nodes.clone();
        nodes.insert(n.id, load(Some(0.0), Some(1000), &[]));
        let loads = Loads::new(nodes);
        let u = node_use(&n, &inv, &none, &loads, Some(&me), None);
        assert!(close(u.mem, 3072.0 / 8192.0), "{u:?}");
        // 요약이 없으면 할당만 본다. 빼는 VM은 할당과 RSS에서 빠진다.
        let u = node_use(&n, &inv, &none, &Loads::default(), None, None);
        assert!(close(u.mem, 2048.0 / 8192.0), "{u:?}");
        let web = inv.vms[0].clone();
        let u = node_use(&n, &inv, &none, &Loads::default(), None, Some(&web));
        assert!(close(u.mem, 0.0), "{u:?}");
        // 메모리를 모르는 노드는 가득 찬 것으로 본다.
        let zero = node("zero", 4, 0);
        let u = node_use(&zero, &inv, &none, &Loads::default(), None, None);
        assert!(close(u.mem, 1.0), "{u:?}");
    }

    #[test]
    fn a_node_without_a_summary_gets_the_mean_cpu_of_the_others() {
        let (a, b, c) = (node("a", 4, 8192), node("b", 4, 8192), node("c", 4, 8192));
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone(), c.clone()],
            ..Default::default()
        };
        let none = Assign::default();
        let mut nodes = BTreeMap::new();
        nodes.insert(a.id, load(Some(0.2), Some(1000), &[]));
        nodes.insert(b.id, load(Some(0.6), Some(1000), &[]));
        let loads = Loads::new(nodes);
        let cpu = |n: &Node| node_use(n, &inv, &none, &loads, None, None).cpu;
        assert!(close(cpu(&a), 0.2));
        assert!(close(cpu(&c), 0.4));
        // 아무도 요약이 없으면 0이다.
        let u = node_use(&c, &inv, &none, &Loads::default(), None, None);
        assert!(close(u.cpu, 0.0));
    }

    #[test]
    fn without_any_summary_the_score_is_the_allocated_memory_ratio() {
        // 점수만 보면 할당 메모리 비율이 낮은 노드가 낫다. 후보 순서는 요약이 하나도 없으면 점수를 쓰지 않고 남은
        // 메모리로 정한다(`placement::level_scores`).
        let (big, small) = (node("big", 4, 16384), node("small", 4, 4096));
        let me = vm("me", 2, 1024, None);
        let mut inv = Inventory {
            nodes: vec![big.clone(), small.clone()],
            vms: vec![vm("web", 1, 8192, Some(&big))],
            ..Default::default()
        };
        let none = Assign::default();
        let at = |inv: &Inventory, n: &Node| {
            score(
                node_use(n, inv, &none, &Loads::default(), Some(&me), None),
                PlacementPolicy::Balance,
            )
        };
        // big: (8192 + 1024) / 16384 = 0.5625, small: 1024 / 4096 = 0.25
        assert!(at(&inv, &small) > at(&inv, &big));
        inv.vms.clear();
        assert!(at(&inv, &big) > at(&inv, &small));
    }

    #[test]
    fn the_vm_share_is_its_cpu_times_vcpus_over_node_cpus() {
        let n = node("n", 16, 8192);
        let me = vm("me", 4, 1024, None);
        let half = VmLoad {
            id: me.id,
            cpu: Some(500),
            mem: None,
        };
        assert!(close(vm_cpu_share(&me, Some(&half), &n), 0.125));
        assert!(close(vm_cpu_share(&me, None, &n), 0.0));
        assert!(close(vm_cpu_share(&me, Some(&half), &node("x", 0, 1)), 0.0));
        // 놓으면 더하고 빼면 뺀다(0 아래로는 가지 않는다).
        let inv = Inventory {
            nodes: vec![n.clone()],
            ..Default::default()
        };
        let mut nodes = BTreeMap::new();
        nodes.insert(n.id, load(Some(0.5), None, &[(me.id, 500, 256)]));
        let loads = Loads::new(nodes);
        let none = Assign::default();
        let u = node_use(&n, &inv, &none, &loads, Some(&me), None);
        assert!(close(u.cpu, 0.625));
        let u = node_use(&n, &inv, &none, &loads, None, Some(&me));
        assert!(close(u.cpu, 0.375));
        // 비우기는 VM 부하를 모름으로 둔다.
        let u = node_use(
            &n,
            &inv,
            &none,
            &loads.clone().without_vms(),
            Some(&me),
            None,
        );
        assert!(close(u.cpu, 0.5));
    }

    #[test]
    fn vms_placed_elsewhere_by_assign_move_their_cpu_share() {
        let (a, b) = (node("a", 4, 8192), node("b", 4, 8192));
        let v = vm("v", 1, 1024, Some(&a));
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            vms: vec![v.clone()],
            ..Default::default()
        };
        let mut nodes = BTreeMap::new();
        nodes.insert(a.id, load(Some(0.5), None, &[(v.id, 1000, 256)]));
        nodes.insert(b.id, load(Some(0.1), None, &[]));
        let loads = Loads::new(nodes);
        let cpu = |n: &Node, assign: &Assign| node_use(n, &inv, assign, &loads, None, None).cpu;
        assert!(close(cpu(&a, &Assign::default()), 0.5));
        assert!(close(cpu(&b, &Assign::default()), 0.1));
        // v(vCPU 하나를 다 씀 — 노드 CPU 4개의 25%)를 b에 놓은 것으로 보면 a에서 빠지고 b에 더해진다.
        let mut moved = Assign::default();
        moved.place(v.id, b.id);
        assert!(close(cpu(&a, &moved), 0.25));
        assert!(close(cpu(&b, &moved), 0.35));
        // 자리에서 뺀 VM은 어디에도 세지 않는다.
        let mut gone = Assign::default();
        gone.unplace(v.id);
        assert!(close(cpu(&a, &gone), 0.25));
        assert!(close(cpu(&b, &gone), 0.1));
    }

    #[test]
    fn stale_summaries_are_ignored() {
        let now = Instant::now();
        let (fresh, old) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut leases = Leases::default();
        let summary = LoadSummary {
            cpu1: Some(100),
            cpu5: Some(250),
            mem: Some(2048),
            vms: vec![VmLoad {
                id: Uuid::from_u128(3),
                cpu: Some(700),
                mem: Some(512),
            }],
        };
        let report = |age: u64| LoadReport {
            at: now - Duration::from_secs(age),
            load: summary.clone(),
        };
        leases.loads.insert(NodeId(fresh), report(29));
        leases.loads.insert(NodeId(old), report(31));
        // 위트니스의 요약은 신선해도 쓰지 않는다(평균 CPU에도 들지 않는다).
        let mut witness = node("w", 4, 8192);
        witness.spec.witness = true;
        let mut busy = summary.clone();
        busy.cpu5 = Some(900);
        leases.loads.insert(
            NodeId(witness.id),
            LoadReport {
                at: now,
                load: busy,
            },
        );
        let loads = Loads::from_leases(&leases, &[witness], now);
        assert_eq!(loads.nodes.keys().copied().collect::<Vec<_>>(), [fresh]);
        assert!(close(loads.mean_cpu5, 0.25));
        let n = &loads.nodes[&fresh];
        assert_eq!(
            (n.cpu1, n.cpu5, n.mem_used_mib),
            (Some(0.1), Some(0.25), Some(2048))
        );
        assert_eq!(loads.vm(Uuid::from_u128(3)).unwrap().cpu, Some(700));
    }

    /// 죽은 노드(리스가 30초 넘게 끊김)의 VM 부하는 5분까지 쓴다: 다른 노드에서 다시 켤 때 그 VM의 CPU를 센다.
    #[test]
    fn vm_loads_of_a_dead_node_count_for_five_minutes() {
        let now = Instant::now();
        let (dead, gone, alive) = (
            node("dead", 4, 8192),
            node("gone", 4, 8192),
            node("alive", 4, 8192),
        );
        let (v, w) = (vm("v", 4, 1024, Some(&dead)), vm("w", 4, 1024, Some(&gone)));
        let report = |vm: &Vm, cpu: u16, age: u64| LoadReport {
            at: now - Duration::from_secs(age),
            load: LoadSummary {
                cpu5: Some(500),
                vms: vec![VmLoad {
                    id: vm.id,
                    cpu: Some(cpu),
                    mem: Some(512),
                }],
                ..Default::default()
            },
        };
        let mut leases = Leases::default();
        leases.loads.insert(NodeId(dead.id), report(&v, 500, 40));
        leases.loads.insert(NodeId(gone.id), report(&w, 500, 300));
        leases.loads.insert(NodeId(alive.id), report(&w, 900, 1));
        let loads = Loads::from_leases(&leases, &[], now);
        // 노드 부하는 30초 규칙 그대로다.
        assert_eq!(loads.nodes.keys().copied().collect::<Vec<_>>(), [alive.id]);
        assert_eq!(loads.vm(v.id).unwrap().cpu, Some(500));
        // 새 요약이 앞선다.
        assert_eq!(loads.vm(w.id).unwrap().cpu, Some(900));
        let inv = Inventory {
            nodes: vec![dead.clone(), alive.clone()],
            vms: vec![v.clone()],
            ..Default::default()
        };
        // v(vCPU 4, 50%)를 alive(CPU 4)에 놓으면 50%를 더한다.
        let u = node_use(&alive, &inv, &Assign::default(), &loads, Some(&v), None);
        assert!(close(u.cpu, 1.0), "{u:?}");
        // 5분이 지난 요약의 VM 부하는 쓰지 않는다.
        leases.loads.insert(NodeId(dead.id), report(&v, 500, 300));
        let loads = Loads::from_leases(&leases, &[], now);
        assert!(loads.vm(v.id).is_none());
    }

    #[test]
    fn balance_prefers_free_cpu_and_memory() {
        let u = |cpu, mem| score(NodeUse { cpu, mem }, PlacementPolicy::Balance);
        assert_eq!(u(0.0, 0.0), 2_000_000);
        assert_eq!(u(0.25, 0.5), 1_250_000);
        assert!(u(0.2, 0.5) > u(0.7, 0.5));
        assert!(u(0.5, 0.2) > u(0.5, 0.7));
        // 넘친 노드는 음수까지 내려간다.
        assert!(u(1.2, 1.1) < 0);
    }

    #[test]
    fn pack_prefers_the_busiest_node_that_does_not_overflow() {
        let u = |cpu, mem| score(NodeUse { cpu, mem }, PlacementPolicy::Pack);
        // 넘치지 않는 노드끼리는 많이 쓴 것이 앞이다(경계는 넘치지 않음).
        assert!(u(0.7, 0.5) > u(0.2, 0.5));
        assert!(u(0.2, 0.8) > u(0.2, 0.3));
        assert!(u(0.8, 0.9) > u(0.7, 0.9));
        // 넘치는 노드는 넘치지 않는 어떤 노드보다 뒤다.
        assert!(u(0.0, 0.0) > u(0.81, 0.1));
        assert!(u(0.0, 0.0) > u(0.1, 0.91));
        // 모두 넘치면 덜 쓴 노드가 앞이다.
        assert!(u(0.85, 0.5) > u(0.95, 0.5));
        assert!(u(0.5, 0.95) > u(0.5, 1.2));
    }

    #[test]
    fn loads_from_the_leader_status_keep_only_fresh_node_loads() {
        let view = |n: u128, age: Option<u32>| NodeLoadView {
            node: Uuid::from_u128(n),
            name: format!("n{n}"),
            cpu1_pct: Some(100),
            cpu5_pct: Some(250),
            mem_used_mib: Some(2048),
            mem_allocated_mib: 0,
            mem_total_mib: 8192,
            cpu_use_pct: 250,
            mem_use_pct: 250,
            vms: 0,
            age_secs: age,
        };
        let status = PlacementStatus {
            leader: Uuid::from_u128(1),
            settings: Default::default(),
            last_run: None,
            next_run: None,
            imbalance: None,
            paused: None,
            nodes: vec![view(1, Some(3)), view(2, None)],
            recommendations: vec![],
        };
        let loads = Loads::from_status(&status);
        assert_eq!(
            loads.nodes.keys().copied().collect::<Vec<_>>(),
            [Uuid::from_u128(1)]
        );
        let n = &loads.nodes[&Uuid::from_u128(1)];
        assert_eq!(
            (n.cpu1, n.cpu5, n.mem_used_mib),
            (Some(0.1), Some(0.25), Some(2048))
        );
        assert!(n.vms.is_empty());
    }
}
