//! `spec.node`가 비어 있는 VM을 시작할 때 노드를 고른다(설계 3.3).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use opl_cluster::{Cluster, ClusterError};
use opl_reconciler::cpu::{CpuContext, CpuRefusal};
use opl_types::cluster::ClusterSpec;
use opl_types::cpu::{self, Baseline, CpuCatalogSpec};
use opl_types::hotplug;
use opl_types::network::{Network, NetworkSpec, USER_NETWORK};
use opl_types::node::{CPU_MODELS, Node, NodeSpec, ShutdownAction, all_have, ha_capable};
use opl_types::numa::{
    self, Demand, HostFree, HostLimits, HostPlacement, HostSets, HostUsage, HostView, NodeTopology,
    NumaRefusal, ResolvedNuma,
};
use opl_types::placement::{HaGroup, HaGroupSpec, PlacementRule, PlacementRuleSpec};
use opl_types::replication::{ReplicaVolume, replica_ready};
use opl_types::runtime::ImageFormat;
use opl_types::settings::{HaReserve, SETTINGS_NAME, SettingsSpec};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumeSpec, is_shared, pool_usable_on,
};
use opl_types::vm::{Arch, DiskMove, HaState, PowerState, RunningHardware, Vm, VmSpec};
use uuid::Uuid;

use crate::placement::{self, Assign};
use crate::score::Scorer;
use crate::volumes::block_pool_kind;

/// [`schedule`]가 노드를 고르지 못한 이유
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleError {
    /// 노드 조건([`node_fits`])에 맞는 노드가 없다.
    #[error(
        "no node can run this VM (needs an online node that is not a witness or in maintenance, has the architecture of its machine type, can reach its storage pools, has the bridges of its networks, has a firewall (nftables) for VMs with security groups, can run its CPU model, has room for its dedicated CPUs, hugepages and NUMA layout and, for an HA VM, has HA fencing)"
    )]
    NoNode,
    /// 노드 조건에 맞는 노드는 있지만 모두 이 hard 규칙(이름 순)을 어긴다.
    #[error("placement rules leave no node for this VM: {}", .0.join(", "))]
    PlacementRule(Vec<String>),
}

/// 스케줄러가 보는 클러스터 상태
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub nodes: Vec<Node>,
    pub vms: Vec<Vm>,
    pub volumes: Vec<Volume>,
    pub pools: Vec<StoragePool>,
    pub networks: Vec<Network>,
    /// 배치 규칙과 HA 그룹([`crate::placement`])
    pub rules: Vec<PlacementRule>,
    pub groups: Vec<HaGroup>,
    /// CPU 카탈로그(이름 = 해시, 계획 63). 모든 노드가 `cpu-models`를 알릴 때만 읽는다.
    pub cpu_catalogs: BTreeMap<String, Arc<CpuCatalogSpec>>,
    /// 아키텍처마다 클러스터 CPU 기준(`ClusterStatus.cpu_baselines`)
    pub cpu_baselines: BTreeMap<Arch, Baseline>,
    /// 클러스터 숨김 목록(설정 `cpu.mask`)
    pub cpu_mask: Vec<String>,
}

/// 풀어 둔 CPU 카탈로그(이름 = 내용의 해시라 한 번 풀면 바뀌지 않는다). `Inventory::load`는 VM API·HA·균형 조정·건강
/// tick마다 불리고 x86 카탈로그 하나가 40 KB이므로 해시마다 한 번만 푼다(리뷰 2 #9).
static CATALOG_CACHE: std::sync::LazyLock<std::sync::Mutex<BTreeMap<String, Arc<CpuCatalogSpec>>>> =
    std::sync::LazyLock::new(Default::default);

/// 캐시가 이보다 커지면 지금 노드가 알린 것만 남긴다.
const CATALOG_CACHE_MAX: usize = 64;

/// 노드들이 알린 카탈로그(이름 = 해시). 노드가 알리지 않은 카탈로그(정리 작업이 1시간 뒤 지움)는 읽지 않는다. 리소스가
/// 아직 없는 해시는 빠진다(작업자처럼 "카탈로그가 아직 없음"으로 본다). 풀어 둔 것은 다시 풀지 않는다.
pub(crate) fn node_catalogs(
    cluster: &Cluster,
    nodes: &[Node],
) -> Result<BTreeMap<String, Arc<CpuCatalogSpec>>, ClusterError> {
    let hashes: BTreeSet<&str> = nodes
        .iter()
        .filter_map(|n| n.status.cpu.as_ref())
        .map(|c| c.catalog.as_str())
        .filter(|h| !h.is_empty())
        .collect();
    let mut out = BTreeMap::new();
    let mut missing = Vec::new();
    {
        let cache = CATALOG_CACHE.lock().unwrap();
        for h in &hashes {
            match cache.get(*h) {
                Some(c) => {
                    out.insert(h.to_string(), c.clone());
                }
                None => missing.push(*h),
            }
        }
    }
    let mut found = Vec::new();
    for h in missing {
        if let Some(c) = cluster.get_by_name::<CpuCatalogSpec>(h)? {
            found.push((c.name, Arc::new(c.spec)));
        }
    }
    if !found.is_empty() {
        let mut cache = CATALOG_CACHE.lock().unwrap();
        for (name, c) in found {
            cache.insert(name.clone(), c.clone());
            out.insert(name, c);
        }
        if cache.len() > CATALOG_CACHE_MAX {
            cache.retain(|k, _| hashes.contains(k.as_str()));
        }
    }
    Ok(out)
}

/// 노드 조건이 보는 VM의 CPU
#[derive(Debug, Clone, Copy)]
enum VmCpu<'a> {
    /// 새로 켠다(켜기·HA 복구·복제): 스펙을 그 노드에서 지금 기준·숨김 목록으로 해석한다.
    Start,
    /// 도는 VM을 옮긴다: 켤 때 기록한 CPU 그대로다(받는 쪽은 기록으로 켠다). 둘째는 지금 도는 노드다.
    Running(&'a RunningHardware, Option<Uuid>),
}

impl<'a> VmCpu<'a> {
    /// 하드웨어 기록이 있으면(도는 VM) 그것, 없으면 새로 켜는 것으로 본다.
    fn of(vm: &'a Vm) -> Self {
        vm.status
            .hardware
            .as_ref()
            .map_or(VmCpu::Start, |hw| VmCpu::Running(hw, vm.status.node))
    }
}

impl Inventory {
    pub fn load(cluster: &Cluster) -> Result<Self, ClusterError> {
        let mut inv = Self {
            nodes: cluster.list::<NodeSpec>()?.1,
            vms: cluster.list::<VmSpec>()?.1,
            volumes: cluster.list::<VolumeSpec>()?.1,
            pools: cluster.list::<StoragePoolSpec>()?.1,
            networks: cluster.list::<NetworkSpec>()?.1,
            rules: cluster.list::<PlacementRuleSpec>()?.1,
            groups: cluster.list::<HaGroupSpec>()?.1,
            ..Self::default()
        };
        // 업그레이드 중에는 작업자도 CPU를 보지 않는다(카탈로그·기준도 아직 없다).
        if all_have(&inv.nodes, CPU_MODELS) {
            inv.cpu_catalogs = node_catalogs(cluster, &inv.nodes)?;
            inv.cpu_baselines = cluster
                .list::<ClusterSpec>()?
                .1
                .into_iter()
                .next()
                .map(|c| c.status.cpu_baselines)
                .unwrap_or_default();
            inv.cpu_mask = cluster
                .get_by_name::<SettingsSpec>(SETTINGS_NAME)?
                .and_then(|s| s.spec.cpu)
                .map(|c| c.mask)
                .unwrap_or_default();
        }
        Ok(inv)
    }

    /// 노드 `node`의 CPU 해석 문맥. 작업자가 켤 때 쓰는 것과 같은 규칙이다([`CpuContext::from_parts`]) — 그래야 고른
    /// 노드가 켜기를 거절하지 않는다. 업그레이드 중이거나 노드의 아키텍처·가속기를 모르면 None(작업자도 CPU를 보지
    /// 않는다).
    pub fn cpu_context(&self, node: &Node) -> Option<CpuContext> {
        CpuContext::from_parts(
            &self.nodes,
            node,
            |hash| self.cpu_catalogs.get(hash).cloned(),
            &self.cpu_baselines,
            &self.cpu_mask,
        )
    }

    /// VM의 CPU를 노드가 켤 수 있는지(계획 63 §10). `host`·`max`는 늘 된다. 그 밖에는 노드가 카탈로그를 알렸고 그
    /// 리소스가 있으며 `can_run`이어야 한다 — `NodeStatus.cpu`가 없거나 빈 해시이거나 카탈로그가 아직 없는 노드는
    /// `host`·`max`만 켠다.
    fn cpu_fits(&self, spec: &VmSpec, node: &Node, cpu: VmCpu<'_>) -> Result<(), String> {
        // 라이브 마이그레이션을 막는 플래그(`+invtsc`)로 도는 VM은 어디로도 옮기지 못한다(리뷰 2 #3). 비우기·균형
        // 조정·HA 검사가 이 까닭을 보인다.
        if let VmCpu::Running(hw, Some(from)) = cpu
            && from != node.id
            && let Some(r) = &hw.cpu_resolved
        {
            let blockers = r.migration_blockers();
            if !blockers.is_empty() {
                return Err(migration_blocked(&blockers));
            }
        }
        let Some(ctx) = self.cpu_context(node) else {
            return Ok(());
        };
        let name = &node.name;
        let resolved = match cpu {
            VmCpu::Start => ctx.resolve(&spec.cpu),
            VmCpu::Running(hw, _) => match &hw.cpu_resolved {
                Some(r) => Ok(r.clone()),
                // 계획 63 전 기록: 명령줄처럼 모델만 편다.
                None => cpu::resolve(&hw.cpu, ctx.arch, ctx.accel, None, &[], None),
            },
        }
        .map_err(|e| format!("node {name:?} cannot use the VM's CPU: {e}"))?;
        ctx.check(&resolved).map_err(|e| match e {
            CpuRefusal::Cannot(message) => message,
            CpuRefusal::Waiting(_) => format!(
                "node {name:?} has no CPU catalog yet; only the host and max CPU models can run there"
            ),
        })?;
        // 켤 때 그 노드의 QEMU가 몰라 뺀 숨김 이름을 대상의 QEMU가 알면(또는 대상의 카탈로그를 모르면) 받는 쪽의
        // `-cpu host`가 숨긴 기능을 준다(리뷰 2 #1 — 제자리 QEMU 올리기 뒤 원본의 지금 카탈로그로는 알 수 없다).
        if let VmCpu::Running(_, Some(from)) = cpu
            && from != node.id
        {
            ctx.check_exposed(&resolved, "if the VM moved there")?;
        }
        Ok(())
    }

    /// 이 VM의 네트워크가 쓰는 브리지(VXLAN이면 `oplv<vni>`)가 모두 이 노드에 있는지.
    /// VLAN을 쓰면 VLAN-aware여야 한다.
    fn has_bridges(&self, spec: &VmSpec, node: &Node) -> bool {
        spec.nics
            .iter()
            .filter(|nic| nic.network != USER_NETWORK)
            .all(|nic| {
                let Some(network) = self.networks.iter().find(|n| n.name == nic.network) else {
                    return false;
                };
                node.status.bridges.iter().any(|b| {
                    b.name == network.spec.bridge_name()
                        && (network.spec.vlan.is_none() || b.vlan_filtering)
                })
            })
    }

    /// 이 VM의 디스크가 있는 풀을 모두 쓸 수 있는 노드인지. 공유가 아닌 풀의 볼륨은 그 파일이 있는
    /// 노드(볼륨을 만든 노드)에서만 쓸 수 있다. 그 노드를 모르면 풀이 그 노드 하나에만 있을 때만 쓸 수 있다(파일이 어느
    /// 노드에 있는지 모르는 볼륨을 다른 노드에서 켜지 않는다). `moves`의 볼륨(옮길 디스크)은 보지 않는다. CD-ROM에 넣은
    /// ISO도 같다(노드 로컬 풀의 ISO는 그 노드에서만).
    fn reaches_disks(&self, spec: &VmSpec, node: &Node, moves: &[DiskMove]) -> bool {
        let media = spec.cdroms.iter().filter_map(|c| c.iso.as_ref());
        spec.disks
            .iter()
            .map(|d| &d.volume)
            .chain(media)
            .all(|name| {
                let Some(volume) = self.volumes.iter().find(|v| &v.name == name) else {
                    return true;
                };
                if moves.iter().any(|m| m.volume == volume.id) {
                    return true;
                }
                let Some(pool) = self.pools.iter().find(|p| p.name == volume.spec.pool) else {
                    return true;
                };
                let here = match volume.status.node {
                    Some(n) => n == node.id,
                    None => pool.spec.nodes == [node.id],
                };
                pool_usable_on(pool, node) && (is_shared(pool) || here)
            })
    }

    /// 실행하기로 된 다른 VM에 배정되지 않은 보통 메모리. hugepages 풀은 노드 메모리에서 빼고 hugepages VM의 메모리는
    /// 풀에서 센다(계획 64 §7 — [`small_memory_mib`]·[`vm_small_mib`]).
    pub fn free_memory_mib(&self, node: &Node, vm: Uuid) -> i64 {
        let used: i64 = self
            .vms
            .iter()
            .filter(|v| v.id != vm && v.spec.node == Some(node.id))
            .filter(|v| v.spec.power == PowerState::Running)
            .map(|v| i64::from(vm_small_mib(v)))
            .sum();
        small_memory_mib(node) as i64 - used
    }

    /// 노드 `node`의 장부(계획 64 §9의 둘째 층, 어림): 그 노드에 자리 잡은 VM(`skip` 빼고, [`Assign`]을 따름 — 그
    /// 노드로 오는 마이그레이션 포함)이 잡은 것과 그 요구 합. 도는 VM은 기록된 배치(`hardware.host_placement`, 정확)로,
    /// 아직 켜지 않은 VM(놓였으나 켜지 않음, HA 회차의 자리)과 오는 마이그레이션은 그 모양을 지금 빈 자원에 맞춰 본
    /// 배치로 센다. 합은 Raft 장부와 같이 스펙의 [`numa::demand`]다.
    pub fn host_usage(
        &self,
        node: &Node,
        topology: &NodeTopology,
        sets: &HostSets,
        skip: Uuid,
        assign: &Assign,
    ) -> (HostUsage, Demand) {
        let mut total = Demand::default();
        let mut records: Vec<(&ResolvedNuma, &HostPlacement)> = Vec::new();
        let mut pending = Vec::new();
        for v in self.vms.iter().filter(|v| v.id != skip) {
            if !placement::occupied(v, assign).contains(&node.id) {
                continue;
            }
            total.add(&numa::demand(&v.spec, Some(topology)));
            match &v.status.hardware {
                Some(hw) => match (&hw.numa, &hw.host_placement) {
                    (Some(n), Some(p)) if p.node == node.id => records.push((n, p)),
                    // 다른 노드에서 도는 VM(이 노드로 오는 마이그레이션)은 기록된 모양으로 다시 맞춘다.
                    (Some(n), _) if n.memory.is_some() && v.status.node != Some(node.id) => {
                        pending.push(VmShape::Fixed(n, &hw.cpu));
                    }
                    _ => {}
                },
                None => pending.push(VmShape::Spec(&v.spec)),
            }
        }
        let mut usage = numa::usage(records, node.id);
        for shape in pending {
            let free = HostFree::new(topology, sets, &usage);
            let view = HostView {
                node: node.id,
                topology,
                sets,
                free: &free,
            };
            let fit = match shape {
                VmShape::Fixed(n, cpu) => numa::fit_fixed(n, cpu, Some(&view)).ok(),
                VmShape::Spec(spec) => match numa::layout(spec, Some(topology)) {
                    Ok(Some(l)) if l.binds() => numa::fit(&l, Some(&view)).ok(),
                    _ => None,
                },
            };
            if let Some(numa::Fit {
                numa,
                placement: Some(p),
            }) = fit
            {
                usage.add(&numa::usage([(&numa, &p)], node.id));
            }
        }
        (usage, total)
    }

    /// VM의 NUMA 모양이 노드에 맞는지(계획 64 §9). 묶지 않는 VM(`numa` 없는 공유 VM, 공유 vNUMA)은 늘 된다. 새로 켜면
    /// 스펙의 모양을 빈 자원에 [`numa::fit`]으로 맞춰 본 뒤 Raft 장부와 같은 합계([`numa::exceeded`])도 보고, 도는 VM을
    /// 옮기면 켤 때 기록한 모양을 [`numa::fit_fixed`]로 맞춰 본다 — 노드 할당기와 같은 함수다. `skip`은 이 VM(자리로 세지 않는다).
    fn numa_fits(
        &self,
        skip: Uuid,
        spec: &VmSpec,
        node: &Node,
        cpu: VmCpu<'_>,
        assign: &Assign,
    ) -> Result<(), String> {
        self.numa_plan(skip, spec, node, cpu, assign).map(drop)
    }

    /// [`Self::numa_fits`]이고 맞춘 배치(묶는 VM만, 아니면 None)를 돌려준다(`GET /vms/{key}/numa`의 계획).
    fn numa_plan(
        &self,
        skip: Uuid,
        spec: &VmSpec,
        node: &Node,
        cpu: VmCpu<'_>,
        assign: &Assign,
    ) -> Result<Option<numa::Fit>, String> {
        let name = &node.name;
        let topology = node.status.topology.as_ref();
        let shape = match cpu {
            VmCpu::Running(hw, _) => match &hw.numa {
                Some(n) if n.memory.is_some() => VmShape::Fixed(n, &hw.cpu),
                _ => return Ok(None),
            },
            VmCpu::Start => match numa::layout(spec, topology) {
                Ok(Some(l)) if l.binds() => VmShape::Spec(spec),
                Ok(_) => return Ok(None),
                Err(e) => return Err(format!("the VM's NUMA settings are invalid: {e}")),
            },
        };
        let Some(topology) = topology else {
            return Err(NumaRefusal::NoTopology.for_node(name));
        };
        let sets = HostSets::new(node.spec.host_resources.as_ref(), topology);
        let (usage, total) = self.host_usage(node, topology, &sets, skip, assign);
        let free = HostFree::new(topology, &sets, &usage);
        let view = HostView {
            node: node.id,
            topology,
            sets: &sets,
            free: &free,
        };
        let fit = match shape {
            VmShape::Fixed(n, cpu) => numa::fit_fixed(n, cpu, Some(&view)),
            VmShape::Spec(spec) => match numa::layout(spec, Some(topology)) {
                Ok(Some(l)) => numa::fit(&l, Some(&view)),
                Ok(None) => return Ok(None),
                Err(e) => return Err(format!("the VM's NUMA settings are invalid: {e}")),
            },
        };
        let fit = fit.map_err(|e| e.for_node(name))?;
        // 새로 켜면 Raft 장부가 볼 합계도(맞춰 보지 못한 앞 VM, `isolate`·`any`의 셈이 배치와 다를 때)
        if let VmShape::Spec(spec) = shape {
            let mut after = total.clone();
            after.add(&numa::demand(spec, Some(topology)));
            let limits = HostLimits::new(node.spec.host_resources.as_ref(), Some(topology));
            if let Some(e) = numa::exceeded(&limits, &total, &after) {
                return Err(format!("node {name:?} {e}"));
            }
        }
        Ok(Some(fit))
    }
}

/// [`Inventory::host_usage`]·[`Inventory::numa_fits`]가 맞추는 VM의 모양
#[derive(Clone, Copy)]
enum VmShape<'a> {
    /// 켤 때 기록한 모양(도는 VM)
    Fixed(&'a ResolvedNuma, &'a opl_types::vm::CpuSpec),
    /// 스펙에서 새로 정함(켜기)
    Spec(&'a VmSpec),
}

/// 노드의 보통 메모리: 노드 메모리에서 hugepages 풀을 뺀 것(계획 64 §7). 토폴로지가 없으면 노드 메모리 그대로다.
pub fn small_memory_mib(node: &Node) -> u64 {
    let pools: u64 = node.status.topology.as_ref().map_or(0, |t| {
        t.nodes
            .iter()
            .map(|n| n.memory_mib.saturating_sub(n.small_memory_mib()))
            .sum()
    });
    node.status.memory_mib.saturating_sub(pools)
}

/// VM이 쓰는 보통 메모리: hugepages VM(도는 VM은 기록, 아니면 스펙)은 풀에서 쓰므로 0, 아니면
/// [`hotplug::allocated_memory_mib`]다.
pub fn vm_small_mib(vm: &Vm) -> u32 {
    let hugepages = match &vm.status.hardware {
        Some(hw) => hw.numa.as_ref().is_some_and(|n| n.page.is_some()),
        None => vm.spec.hugepages.is_some(),
    };
    if hugepages {
        0
    } else {
        hotplug::allocated_memory_mib(vm)
    }
}

/// 라이브 마이그레이션을 막는 플래그로 도는 VM의 까닭(리뷰 2 #3)
pub fn migration_blocked(flags: &[&str]) -> String {
    format!(
        "the VM runs with the CPU flags {} that QEMU cannot live-migrate; power cycle it on another node instead",
        flags
            .iter()
            .map(|f| format!("+{f}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// [`schedule_with`]와 같되 부하를 모른다(할당 메모리만 본다).
pub fn schedule(vm: &Vm, inv: &Inventory) -> Result<Uuid, ScheduleError> {
    schedule_with(vm, inv, &Scorer::default())
}

/// 위트니스가 아니고 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// (HA VM이면 HA 가능 노드, 보안 그룹을 건 NIC가 있으면 방화벽이 있는 노드) 가운데 hard 배치 규칙을 어기지 않는 노드를
/// [`placement::compare`] 순서(HA 그룹 우선순위 → soft 벌점 → 부하 점수 → 미할당 메모리 → id)로 고른다. 메모리가
/// 모자라도 고른다(오버커밋 정책은 범위 밖). arch를 모르는 노드(옛 버전)는 고르지 않는다. 저장소의 이 VM(같은 id)은
/// 자리로 세지 않는다.
pub fn schedule_with(vm: &Vm, inv: &Inventory, scorer: &Scorer) -> Result<Uuid, ScheduleError> {
    let assign = Assign::default();
    match placement::best(&placement::candidates_with(vm, inv, &assign, scorer)) {
        Some(c) => Ok(c.node),
        None => Err(blocked(vm, inv, &assign)),
    }
}

/// 놓을 노드가 없을 때의 이유: 노드 조건에 맞는 노드가 있으면 그 노드들에서 어기는 hard 규칙이다.
pub fn blocked(vm: &Vm, inv: &Inventory, assign: &Assign) -> ScheduleError {
    let fits: Vec<&Node> = inv
        .nodes
        .iter()
        .filter(|n| vm_fits(vm, inv, n, assign).is_ok())
        .collect();
    if fits.is_empty() {
        return ScheduleError::NoNode;
    }
    let rules: BTreeSet<String> = fits
        .into_iter()
        .flat_map(|n| placement::hard_violations(vm, n, inv, assign))
        .collect();
    ScheduleError::PlacementRule(rules.into_iter().collect())
}

/// 스펙 `spec`의 VM을 이 노드에서 켤 수 있는지. 아니면 그 이유다. 메모리는 보지 않는다. 전용 CPU·hugepages는 저장소의
/// 어느 VM도 이 VM으로 보지 않고 센다 — 저장된 VM은 [`vm_fits`]를 쓴다.
pub fn node_fits(spec: &VmSpec, inv: &Inventory, node: &Node) -> Result<(), String> {
    node_fits_with(spec, inv, node, &[])
}

/// [`schedule`]가 이 노드를 후보로 보는지(새로 켬): [`node_fits`]와 같되 저장소의 이 VM(같은 id)은 자리로 세지 않고
/// 전용 CPU·hugepages·NUMA는 `assign`(HA 회차·묶음의 자리)을 따른다(계획 64 §9).
pub fn vm_fits(vm: &Vm, inv: &Inventory, node: &Node, assign: &Assign) -> Result<(), String> {
    fits(&vm.spec, inv, node, &[], VmCpu::Start, vm.id, assign)
}

/// 마이그레이션이 옮길 디스크(노드 로컬 풀의 디스크): 디스크마다 대상 풀(`pool`, 없으면 같은 이름의
/// 풀)과 대상 형식. 대상 풀이 블록 풀(raw만 담음)이면 raw, 아니면 지금 형식이다. 공유 풀의 디스크와
/// 볼륨·풀 기록이 없는 디스크는 옮기지 않는다.
pub fn disk_moves(spec: &VmSpec, inv: &Inventory, pool: Option<&StoragePool>) -> Vec<DiskMove> {
    spec.disks
        .iter()
        .filter_map(|disk| {
            let volume = inv.volumes.iter().find(|v| v.name == disk.volume)?;
            let source = inv.pools.iter().find(|p| p.name == volume.spec.pool)?;
            if is_shared(source) {
                return None;
            }
            let target = pool.unwrap_or(source);
            let format = if block_pool_kind(target).is_some() {
                ImageFormat::Raw
            } else {
                volume.spec.format
            };
            Some(DiskMove {
                volume: volume.id,
                pool: target.name.clone(),
                format,
                existing: false,
            })
        })
        .collect()
}

/// 노드 `node`에 VM의 쓸 수 있는 복제본([`replica_ready`])이 있으면 노드 로컬 풀 디스크마다 그 복제본으로 옮기는
/// 이동(`existing`): 대상 풀은 그 복제 대상의 `pool`(없으면 같은 이름), 형식은 [`disk_moves`]의 규칙이다. 복제본의 크기
/// 비교는 볼륨의 `spec.size_bytes`(회차가 적는 가상 크기와 같다)로 한다. `node`가 복제 대상이 아니거나, 대상 풀이 없거나,
/// 쓸 수 있는 복제본이 없거나, 옮길 볼륨 가운데 어느 노드에 있는지 모르는 것(`status.node` 없음 — 승격이 거절한다)이
/// 있으면 None이다.
pub fn replica_moves(vm: &Vm, inv: &Inventory, node: Uuid) -> Option<Vec<DiskMove>> {
    let target = vm.spec.replication.iter().find(|t| t.node == node)?;
    let pool = match &target.pool {
        Some(name) => Some(inv.pools.iter().find(|p| &p.name == name)?),
        None => None,
    };
    let mut moves = disk_moves(&vm.spec, inv, pool);
    let volumes: Vec<&Volume> = moves
        .iter()
        .filter_map(|m| inv.volumes.iter().find(|v| v.id == m.volume))
        .collect();
    if volumes.iter().any(|v| v.status.node.is_none()) {
        return None;
    }
    let local: Vec<ReplicaVolume> = volumes
        .iter()
        .map(|v| ReplicaVolume {
            id: v.id,
            size_bytes: v.spec.size_bytes,
        })
        .collect();
    replica_ready(vm, node, &local)?;
    for m in &mut moves {
        m.existing = true;
    }
    Some(moves)
}

/// VM을 노드 `node`로 옮길 때의 디스크 이동. `node`가 VM의 복제 대상이면(계획 55) 노드 로컬 풀 디스크마다 그 대상의 풀(없으면
/// 같은 이름)에 있는 복제본으로의 이동(`existing` — 새로 만들지 않고 복제 뒤 바뀐 것만 보낸다)이다. 그때 `pool`이 있으면 그
/// 복제본의 풀과 같아야 한다(아니면 오류 문구). 대상 풀을 찾지 못하면 복제본이 아닌 것처럼 본다. 복제 대상이 아니면
/// [`disk_moves`]와 같다.
pub fn migration_moves(
    vm: &Vm,
    inv: &Inventory,
    node: Uuid,
    pool: Option<&StoragePool>,
) -> Result<Vec<DiskMove>, String> {
    let replica = vm.spec.replication.iter().find(|t| t.node == node);
    let replica_pool = match replica.map(|t| &t.pool) {
        None => return Ok(disk_moves(&vm.spec, inv, pool)),
        Some(None) => None,
        Some(Some(name)) => match inv.pools.iter().find(|p| &p.name == name) {
            Some(p) => Some(p),
            None => return Ok(disk_moves(&vm.spec, inv, pool)),
        },
    };
    let mut moves = disk_moves(&vm.spec, inv, replica_pool);
    if let Some(pool) = pool
        && let Some(m) = moves.iter().find(|m| m.pool != pool.name)
    {
        let name = inv
            .nodes
            .iter()
            .find(|n| n.id == node)
            .map_or_else(|| node.to_string(), |n| n.name.clone());
        return Err(format!(
            "node {name:?} holds the replica of VM {:?} in pool {:?}; migrate without --pool or remove the node from the replication targets",
            vm.name, m.pool
        ));
    }
    for m in &mut moves {
        m.existing = true;
    }
    Ok(moves)
}

/// [`node_fits`]와 같되 노드 로컬 풀 디스크가 있어 그 노드에서 쓸 수 없는 VM은 그 노드에 쓸 수 있는 복제본이 있으면
/// 그 복제본의 풀로 본다([`replica_moves`]). HA 복구가 쓴다(새로 켜므로 CPU는 스펙을 해석한다). 도는 VM을 옮기는
/// 되돌아가기·균형 조정기는 [`replica_migration_fits`]다.
pub fn replica_fits(vm: &Vm, inv: &Inventory, node: &Node) -> Result<(), String> {
    replica_fits_with(vm, inv, node, &Assign::default())
}

/// [`replica_fits`]와 같되 전용 CPU·hugepages·NUMA는 `assign`(HA 회차의 자리)을 따른다.
pub fn replica_fits_with(
    vm: &Vm,
    inv: &Inventory,
    node: &Node,
    assign: &Assign,
) -> Result<(), String> {
    replica_fits_as(vm, inv, node, VmCpu::Start, assign)
}

/// [`replica_fits`]와 같되 CPU·NUMA는 도는 VM의 기록이다([`migration_fits`]).
pub fn replica_migration_fits(vm: &Vm, inv: &Inventory, node: &Node) -> Result<(), String> {
    replica_migration_fits_with(vm, inv, node, &Assign::default())
}

/// [`replica_migration_fits`]와 같되 전용 CPU·hugepages·NUMA는 `assign`(계획한 옮기기)을 따른다.
pub fn replica_migration_fits_with(
    vm: &Vm,
    inv: &Inventory,
    node: &Node,
    assign: &Assign,
) -> Result<(), String> {
    replica_fits_as(vm, inv, node, VmCpu::of(vm), assign)
}

fn replica_fits_as(
    vm: &Vm,
    inv: &Inventory,
    node: &Node,
    cpu: VmCpu<'_>,
    assign: &Assign,
) -> Result<(), String> {
    let plain = fits(&vm.spec, inv, node, &[], cpu, vm.id, assign);
    if plain.is_ok() {
        return plain;
    }
    match replica_moves(vm, inv, node.id) {
        Some(moves) if !moves.is_empty() => fits(&vm.spec, inv, node, &moves, cpu, vm.id, assign),
        _ => plain,
    }
}

/// [`node_fits`]와 같되 `moves`의 디스크([`disk_moves`])는 볼륨이 있는 노드 대신 대상 풀을 이 노드에서
/// 쓸 수 있는지 본다.
pub fn node_fits_with(
    spec: &VmSpec,
    inv: &Inventory,
    node: &Node,
    moves: &[DiskMove],
) -> Result<(), String> {
    fits(
        spec,
        inv,
        node,
        moves,
        VmCpu::Start,
        Uuid::nil(),
        &Assign::default(),
    )
}

/// 도는 VM을 옮길 대상(라이브 마이그레이션·비우기·균형 조정·되돌아가기): [`node_fits_with`]와 같되 CPU는 스펙을 새로
/// 해석하지 않고 켤 때 기록한 것(`status.hardware.cpu_resolved`)을 본다 — 받는 쪽은 원본과 같은 CPUID로 켠다. 하드웨어
/// 기록이 없으면(꺼진 VM) [`node_fits_with`]와 같다. NUMA도 켤 때 기록한 모양(`hardware.numa`)을 대상에 맞춰 본다(계획 64
/// §11 — 받는 쪽 할당기가 같은 함수로 고른다).
pub fn migration_fits(
    vm: &Vm,
    inv: &Inventory,
    node: &Node,
    moves: &[DiskMove],
) -> Result<(), String> {
    migration_fits_with(vm, inv, node, moves, &Assign::default())
}

/// [`migration_fits`]와 같되 전용 CPU·hugepages·NUMA는 `assign`(함께 옮기는 묶음)을 따른다.
pub fn migration_fits_with(
    vm: &Vm,
    inv: &Inventory,
    node: &Node,
    moves: &[DiskMove],
    assign: &Assign,
) -> Result<(), String> {
    fits(&vm.spec, inv, node, moves, VmCpu::of(vm), vm.id, assign)
}

/// `host`·`max` VM을 `from`에서 `to`로 옮길 때의 경고(계획 63 §10, 막지 않는다): 두 노드의 카탈로그가 다르고 대상
/// `supported`에 원본 `supported`의 플래그(VM이 끈 것 빼고)가 빠졌으면 `target node "n3" lacks avx512vbmi, amx-tile that
/// the guest may be using`이다. 게스트는 켤 때 본 CPUID를 쓰므로 대상이 원본의 상위 집합이면 경고가 없다. 같은 해시,
/// 카탈로그를 모르는 노드, 명명 모델·수준·`cluster` VM(대상이 켤 수 있는지를 노드 조건이 본다)도 None이다.
pub fn host_cpu_warning(vm: &Vm, inv: &Inventory, from: &Node, to: &Node) -> Option<String> {
    let (model, off): (&str, Vec<&str>) = match &vm.status.hardware {
        Some(hw) => match &hw.cpu_resolved {
            Some(r) => (
                r.model.as_str(),
                r.flags
                    .iter()
                    .filter(|(_, on)| !**on)
                    .map(|(f, _)| f.as_str())
                    .collect(),
            ),
            None => (hw.cpu.model.as_str(), Vec::new()),
        },
        None => (vm.spec.cpu.model.as_str(), Vec::new()),
    };
    if !matches!(model, "host" | "max") {
        return None;
    }
    let hash = |n: &Node| n.status.cpu.as_ref().map(|c| c.catalog.clone());
    let (source, target) = (hash(from)?, hash(to)?);
    if source == target {
        return None;
    }
    let source = inv.cpu_catalogs.get(&source)?;
    let target = inv.cpu_catalogs.get(&target)?;
    let there: BTreeSet<&str> = target.names(&target.supported).collect();
    let lacks: Vec<&str> = source
        .names(&source.supported)
        .filter(|f| !there.contains(f) && !off.contains(f))
        .collect();
    if lacks.is_empty() {
        return None;
    }
    Some(format!(
        "target node {:?} lacks {} that the guest may be using",
        to.name,
        lacks.join(", ")
    ))
}

/// VM의 스펙을 노드에서 새로 켤 수 있는지 CPU만 본다(껐다 켜기 — 리뷰 1 I2). 다른 조건은 그 노드에서 이미 도는
/// VM이라 보지 않는다(메모리를 두 번 세지 않게).
pub fn cpu_starts(spec: &VmSpec, inv: &Inventory, node: &Node) -> Result<(), String> {
    inv.cpu_fits(spec, node, VmCpu::Start)
}

/// 도는 VM의 기록된 CPU(꺼진 VM이면 스펙의 해석)를 노드가 켤 수 있는지만 본다([`migration_fits`]의 CPU 부분). 다른
/// 조건을 따로 보는 라이브 마이그레이션 후보 거름(HA 검사의 옮기기)이 쓴다.
pub fn cpu_receives(vm: &Vm, inv: &Inventory, node: &Node) -> Result<(), String> {
    inv.cpu_fits(&vm.spec, node, VmCpu::of(vm))
}

/// 도는 VM의 기록된 NUMA 모양(꺼진 VM이면 스펙)을 노드의 빈 전용 CPU·hugepages에 맞출 수 있는지만 본다([`migration_fits`]의
/// NUMA 부분, 계획 64). [`cpu_receives`]와 함께 쓴다.
pub fn numa_receives(vm: &Vm, inv: &Inventory, node: &Node) -> Result<(), String> {
    inv.numa_fits(vm.id, &vm.spec, node, VmCpu::of(vm), &Assign::default())
}

/// [`numa_receives`]와 같은 판정이고 맞춘 배치(묶는 VM만)다: 도는 VM은 기록 모양으로 옮길 때, 아니면 스펙으로 켤 때
/// (`GET /vms/{key}/numa`).
pub fn numa_plan(vm: &Vm, inv: &Inventory, node: &Node) -> Result<Option<numa::Fit>, String> {
    inv.numa_plan(vm.id, &vm.spec, node, VmCpu::of(vm), &Assign::default())
}

/// VM의 스펙을 노드에서 새로 켤 때 NUMA 모양이 맞는지만 본다(껐다 켜기, 계획 64). 이 VM이 잡은 것은 세지 않는다.
pub fn numa_starts(vm: &Vm, inv: &Inventory, node: &Node) -> Result<(), String> {
    inv.numa_fits(vm.id, &vm.spec, node, VmCpu::Start, &Assign::default())
}

/// 도는 VM을 노드 `from`에서 옮길 수 있는 다른 노드(온라인, 유지보수·위트니스가 아님, 머신의 아키텍처)가 모두 CPU나
/// NUMA(전용 CPU·hugepages, 계획 64) 때문에 받지 못하면 그 첫 까닭이다(리뷰 2 #5). 비우기·호스트 종료가 "no node with
/// capacity" 대신 보인다. 받을 수 있는 노드가 하나라도 있거나 후보가 없으면 None이다(다른 까닭이다).
pub fn blocks_every_move(vm: &Vm, inv: &Inventory, from: Uuid) -> Option<String> {
    let arch = vm.spec.machine.arch();
    let mut first = None;
    for n in inv.nodes.iter().filter(|n| {
        n.id != from
            && !n.spec.witness
            && !n.spec.maintenance
            && n.status.online
            && n.status.arch == Some(arch)
    }) {
        match cpu_receives(vm, inv, n).and_then(|()| numa_receives(vm, inv, n)) {
            Ok(()) => return None,
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    first
}

/// 노드 조건. `skip`은 자리로 세지 않을 VM(이 VM), `assign`은 전용 CPU·hugepages·NUMA를 셀 때 따를 자리다.
fn fits(
    spec: &VmSpec,
    inv: &Inventory,
    node: &Node,
    moves: &[DiskMove],
    cpu: VmCpu<'_>,
    skip: Uuid,
    assign: &Assign,
) -> Result<(), String> {
    // 템플릿(계획 58)은 켜지 않는다: 어느 노드에도 놓지 않는다(스케줄러·HA·균형 조정기·마이그레이션 대상 모두).
    // API가 켜기를 막지만 방어로.
    if spec.template {
        return Err("the VM is a template and is never started".into());
    }
    let name = &node.name;
    // 위트니스는 투표만 하는 노드다. 다른 조건과 상관없이 VM을 돌리지 않는다.
    if node.spec.witness {
        return Err(format!("node {name:?} is a witness node"));
    }
    if !node.status.online {
        return Err(format!("node {name:?} is offline"));
    }
    if node.spec.maintenance {
        return Err(format!("node {name:?} is in maintenance"));
    }
    // 호스트 종료 중인 노드는 `mark_offline`까지 온라인·리스가 살아 있다. 다시 등록하면 표시가 지워진다.
    if node.status.shutdown.is_some() {
        return Err(format!("node {name:?} is shutting down"));
    }
    if spec.ha_active() && !ha_capable(node) {
        return Err(format!(
            "node {name:?} does not support HA (fencing is off)"
        ));
    }
    if node.status.arch != Some(spec.machine.arch()) {
        let arch = format!("{:?}", spec.machine.arch()).to_lowercase();
        return Err(format!(
            "node {name:?} does not have the {arch} architecture of the VM's machine type"
        ));
    }
    if !inv.reaches_disks(spec, node, moves) {
        return Err(format!(
            "node {name:?} cannot use the storage pools of the VM's disks"
        ));
    }
    for m in moves {
        let usable = inv
            .pools
            .iter()
            .find(|p| p.name == m.pool)
            .is_some_and(|p| pool_usable_on(p, node));
        if !usable {
            let disk = inv
                .volumes
                .iter()
                .find(|v| v.id == m.volume)
                .map_or_else(|| m.volume.to_string(), |v| v.name.clone());
            return Err(format!(
                "node {name:?} cannot use storage pool {:?} for disk {disk:?}",
                m.pool
            ));
        }
    }
    if !inv.has_bridges(spec, node) {
        return Err(format!(
            "node {name:?} does not have the bridges of the VM's networks"
        ));
    }
    if spec.has_firewall() && !node.status.firewall {
        return Err(format!(
            "node {name:?} does not have a firewall (nftables) for the VM's security groups"
        ));
    }
    // 계획 59: TPM·Secure Boot 능력. 능력을 알리지 않은 노드(옛 노드)는 없는 것으로 본다.
    let caps = node.status.firmware_caps.unwrap_or_default();
    if spec.tpm && !caps.tpm {
        return Err(format!(
            "node {name:?} has no swtpm for the VM's TPM (install swtpm and swtpm-tools)"
        ));
    }
    if spec.secure_boot && !caps.secure_boot {
        return Err(format!(
            "node {name:?} has no Secure Boot firmware for the VM (install ovmf with Secure Boot keys)"
        ));
    }
    // 계획 63: CPU 모델·플래그(작업자의 켤 때 확인과 같은 규칙)
    inv.cpu_fits(spec, node, cpu)?;
    // 계획 64: 전용 CPU·hugepages·NUMA 모양(노드 할당기와 같은 맞춤)
    inv.numa_fits(skip, spec, node, cpu, assign)
}

/// HA 용량 예약을 넘는다: 예약을 뺀 메모리에 이 VM이 들어가지 않는다(MiB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaCapacity {
    /// HA 가능·온라인·유지보수 아님·종료 표시 없는 노드의 메모리 합
    pub capacity_mib: u64,
    /// 복구 자리로 비워 두는 몫
    pub reserve_mib: u64,
    /// 그 노드들에 자리 잡은 다른 VM(켜진 것, HA 아닌 것도)과 유지보수·종료 중 노드에서 곧 옮겨 올 HA VM의
    /// 메모리 합
    pub used_mib: u64,
    /// 이 VM의 메모리
    pub need_mib: u64,
}

/// HA를 켠(`started`) VM을 켜도 HA 용량 예약(설정 `ha_reserve`)이 남는지 메모리 합으로 본다(조합은 보지
/// 않는다). 예약이 없거나 HA로 켜지 않는 VM은 늘 통과한다. 대상 노드는 HA 가능·온라인·유지보수 아님·종료
/// 표시 없는 노드이고, `used`는 그 노드들에 놓인 켜진 VM(이 VM 빼고, 마이그레이션 중이면 원본에 한 번)과
/// 유지보수(비우기)·종료 중인(얼리기 빼고) HA 가능 노드에 놓인 켜진 `started` HA VM(곧 대상 노드로 온다)이다.
pub fn ha_capacity(inv: &Inventory, settings: &SettingsSpec, vm: &Vm) -> Result<(), HaCapacity> {
    let Some(reserve) = settings.ha_reserve else {
        return Ok(());
    };
    if !vm
        .spec
        .ha
        .as_ref()
        .is_some_and(|ha| ha.state == HaState::Started)
    {
        return Ok(());
    }
    let targets: Vec<&Node> = inv
        .nodes
        .iter()
        .filter(|n| {
            ha_capable(n) && n.status.online && !n.spec.maintenance && n.status.shutdown.is_none()
        })
        .collect();
    let capacity_mib: u64 = targets.iter().map(|n| n.status.memory_mib).sum();
    let reserve_mib = match reserve {
        HaReserve::Nodes(count) => {
            let mut sizes: Vec<u64> = targets.iter().map(|n| n.status.memory_mib).collect();
            sizes.sort_unstable_by(|a, b| b.cmp(a));
            sizes.into_iter().take(usize::from(count)).sum()
        }
        HaReserve::Percent(p) => capacity_mib * u64::from(p) / 100,
    };
    let leaving = |id: Uuid| {
        inv.nodes.iter().any(|n| {
            n.id == id
                && ha_capable(n)
                && (n.spec.maintenance
                    || n.status
                        .shutdown
                        .is_some_and(|s| s.action != ShutdownAction::Freeze))
        })
    };
    let started = |v: &Vm| {
        v.spec
            .ha
            .as_ref()
            .is_some_and(|ha| ha.state == HaState::Started)
    };
    let used_mib: u64 = inv
        .vms
        .iter()
        .filter(|v| v.id != vm.id && v.spec.power == PowerState::Running)
        .filter(|v| {
            v.spec
                .node
                .is_some_and(|id| targets.iter().any(|n| n.id == id) || (started(v) && leaving(id)))
        })
        .map(|v| u64::from(hotplug::allocated_memory_mib(v)))
        .sum();
    let need_mib = u64::from(vm.spec.memory_mib);
    if used_mib + need_mib <= capacity_mib.saturating_sub(reserve_mib) {
        return Ok(());
    }
    Err(HaCapacity {
        capacity_mib,
        reserve_mib,
        used_mib,
        need_mib,
    })
}

#[cfg(test)]
mod tests {
    use opl_types::network::Vxlan;
    use opl_types::node::{
        BridgeInfo, Fencing, NodeHa, NodeShutdown, NodeStatus, PoolState, ShutdownAction,
    };
    use opl_types::placement::RuleKind;
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::settings::PlacementPolicy;
    use opl_types::storage::PoolBackend;
    use opl_types::vm::{
        Arch, CdromSpec, CpuSpec, DiskBus, DiskSpec, MachineType, NicModel, NicSpec,
        RunningHardware, VmHa,
    };

    use super::ScheduleError::NoNode;
    use super::*;

    /// 이 spec을 가진 새 VM(규칙에 들지 않는 이름)의 노드를 고른다.
    fn sched(spec: &VmSpec, inv: &Inventory) -> Result<Uuid, ScheduleError> {
        schedule(&Resource::new("me", spec.clone()), inv)
    }

    fn node(memory_mib: u64) -> Node {
        let mut n = Resource::new(
            "n",
            NodeSpec {
                host_resources: None,
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status = NodeStatus {
            topology: None,
            cpu: None,
            online: true,
            cpus: 4,
            memory_mib,
            hypervisor: "mock".into(),
            bridges: vec![],
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
            ha: None,
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
            hotplug: None,
            firmware_caps: None,
        };
        n
    }

    /// 노드가 알린 풀 상태
    fn with_pool(mut node: Node, pool: &StoragePool, ready: bool) -> Node {
        let state = PoolState {
            pool_id: pool.id,
            ready,
            ..Default::default()
        };
        node.status.pools.insert(pool.name.clone(), state);
        node
    }

    fn pool(name: &str, shared: bool) -> StoragePool {
        Resource::new(
            name,
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/pool".into(),
                },
                nodes: vec![],
                shared,
                backups: false,
                folder: None,
            },
        )
    }

    fn volume(name: &str, pool: &str, node: Option<Uuid>) -> Volume {
        let mut v: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1,
                format: ImageFormat::Qcow2,
                content: Default::default(),
                base: None,
            },
        );
        v.status.node = node;
        v
    }

    fn spec(memory_mib: u32, node: Option<Uuid>, power: PowerState) -> VmSpec {
        VmSpec {
            hugepages: None,
            numa: None,
            cpu: CpuSpec {
                emulator: None,
                policy: Default::default(),
                thread_policy: None,
                flags: Vec::new(),
                sockets: 1,
                cores: 1,
                model: Default::default(),
                max_vcpus: None,
            },
            memory_mib,
            firmware: Default::default(),
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
            power,
            restart: Default::default(),
            node,
            snapshot_schedule: None,
            ha: None,
            placement: None,
            max_memory_mib: None,
            replication: Vec::new(),
            cdroms: vec![],
            cloud_init: None,
            template: false,
            tpm: false,
            secure_boot: false,
            guest_agent: None,
            folder: None,
        }
    }

    #[test]
    fn picks_node_with_most_unassigned_memory() {
        let (a, b) = (node(8192), node(4096));
        let mut inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        let me = spec(1024, None, PowerState::Running);
        assert_eq!(sched(&me, &inv), Ok(a.id));

        // a에 6 GiB를 쓰는 VM이 돌면 b가 더 비어 있다. 꺼진 VM은 세지 않는다.
        inv.vms.push(Resource::new(
            "big",
            spec(6144, Some(a.id), PowerState::Running),
        ));
        inv.vms.push(Resource::new(
            "off",
            spec(6144, Some(b.id), PowerState::Stopped),
        ));
        assert_eq!(sched(&me, &inv), Ok(b.id));
    }

    /// 메모리 줄이기가 끝나지 않아 도는 메모리가 스펙보다 큰 VM은 도는 메모리로 센다(할당 셈 모두).
    #[test]
    fn running_memory_above_the_spec_counts_as_allocated() {
        let a = node(8192);
        let mut big: Vm = Resource::new("big", spec(2048, Some(a.id), PowerState::Running));
        let (mut hw, _) =
            RunningHardware::at_start(big.id, &big.name, &big.spec, Default::default(), true);
        hw.memory_mib = 1024;
        hw.layout.as_mut().unwrap().plugged_mib = 3072;
        big.status.hardware = Some(hw);
        let inv = Inventory {
            nodes: vec![a.clone()],
            vms: vec![big.clone()],
            ..Default::default()
        };
        assert_eq!(inv.free_memory_mib(&a, Uuid::now_v7()), 8192 - 4096);
        assert_eq!(
            crate::placement::allocated_mib(&a, &inv, &Default::default(), &[]),
            4096
        );

        // 스펙이 더 크면 스펙으로 센다.
        let mut inv = inv;
        inv.vms[0].spec.memory_mib = 6144;
        assert_eq!(inv.free_memory_mib(&a, Uuid::now_v7()), 8192 - 6144);
    }

    #[test]
    fn picks_the_node_with_less_cpu_load_when_memory_is_equal() {
        let (a, b) = (node(8192), node(8192));
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        let me = Resource::new("me", spec(1024, None, PowerState::Running));
        // 부하를 모르면 id 순이다. id가 큰 노드가 한가하면 그 노드다.
        let (first, second) = (a.id.min(b.id), a.id.max(b.id));
        assert_eq!(schedule(&me, &inv), Ok(first));
        let mut nodes = std::collections::BTreeMap::new();
        for (n, cpu) in [(first, 0.7), (second, 0.2)] {
            let load = crate::score::NodeLoad {
                cpu5: Some(cpu),
                mem_used_mib: Some(1000),
                ..Default::default()
            };
            nodes.insert(n, load);
        }
        let loads = crate::score::Loads::new(nodes);
        let balance = Scorer {
            loads: &loads,
            ..Scorer::default()
        };
        assert_eq!(schedule_with(&me, &inv, &balance), Ok(second));
        // `pack`은 넘치지 않는 한 바쁜 노드에 모은다.
        let pack = Scorer {
            policy: PlacementPolicy::Pack,
            ..balance
        };
        assert_eq!(schedule_with(&me, &inv, &pack), Ok(first));
        // 바쁜 노드가 넘치면(CPU 90%) 덜 바쁜 노드다.
        let mut nodes = loads.nodes.clone();
        nodes.get_mut(&first).unwrap().cpu5 = Some(0.9);
        let loads = crate::score::Loads::new(nodes);
        let pack = Scorer {
            loads: &loads,
            policy: PlacementPolicy::Pack,
        };
        assert_eq!(schedule_with(&me, &inv, &pack), Ok(second));
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

    #[test]
    fn hard_rules_steer_the_choice_and_name_the_rules_when_no_node_is_left() {
        let mut big = node(8192);
        big.name = "big".into();
        let mut small = node(4096);
        small.name = "small".into();
        let mut inv = Inventory {
            nodes: vec![big.clone(), small.clone()],
            ..Default::default()
        };
        let me: Vm = Resource::new("me", spec(512, None, PowerState::Running));
        assert_eq!(schedule(&me, &inv), Ok(big.id));

        // anti-affinity 상대가 big에서 돌면 메모리가 적어도 small로 간다.
        inv.vms.push(Resource::new(
            "peer",
            spec(512, Some(big.id), PowerState::Running),
        ));
        inv.rules = vec![rule("apart", RuleKind::AntiAffinity, &["me", "peer"], &[])];
        assert_eq!(schedule(&me, &inv), Ok(small.id));
        // small도 막히면 두 노드에서 어긴 규칙의 이름이다.
        inv.rules.push(rule(
            "not-small",
            RuleKind::NodeAntiAffinity,
            &["me"],
            &["small"],
        ));
        assert_eq!(
            schedule(&me, &inv),
            Err(ScheduleError::PlacementRule(vec![
                "apart".into(),
                "not-small".into()
            ]))
        );
        assert_eq!(
            schedule(&me, &inv).unwrap_err().to_string(),
            "placement rules leave no node for this VM: apart, not-small"
        );
        // 노드 조건이 먼저다: 조건에 맞는 노드가 없으면 규칙과 상관없이 NoNode.
        let mut offline = inv.nodes.clone();
        offline.iter_mut().for_each(|n| n.status.online = false);
        let dark = Inventory {
            nodes: offline,
            vms: inv.vms.clone(),
            rules: inv.rules.clone(),
            ..Default::default()
        };
        assert_eq!(schedule(&me, &dark), Err(NoNode));
        // 조건에 맞지 않는 노드의 규칙 위반은 이유에 넣지 않는다.
        inv.nodes[1].status.online = false;
        assert_eq!(
            schedule(&me, &inv),
            Err(ScheduleError::PlacementRule(vec!["apart".into()]))
        );

        // soft 규칙은 벌점이라 어길 수밖에 없으면 어기고 고른다.
        inv.nodes[1].status.online = true;
        inv.rules = vec![
            rule(
                "avoid-big",
                RuleKind::SoftNodeAntiAffinity,
                &["me"],
                &["big"],
            ),
            rule(
                "avoid-small",
                RuleKind::SoftNodeAntiAffinity,
                &["me"],
                &["small"],
            ),
        ];
        inv.rules[0].spec.weight = Some(10);
        inv.rules[1].spec.weight = Some(20);
        assert_eq!(schedule(&me, &inv), Ok(big.id));
        inv.rules[0].spec.weight = Some(30);
        assert_eq!(schedule(&me, &inv), Ok(small.id));
    }

    #[test]
    fn skips_offline_maintenance_and_unreachable_pools() {
        let mut offline = node(64000);
        offline.status.online = false;
        let mut maint = node(64000);
        maint.spec.maintenance = true;
        let near = node(1024);
        let pool = Resource::new(
            "local",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/pool".into(),
                },
                nodes: vec![near.id],
                shared: false,
                backups: false,
                folder: None,
            },
        );
        let far = with_pool(node(32000), &pool, true);
        let near = with_pool(near, &pool, true);
        let volume = Resource::new(
            "root",
            VolumeSpec {
                pool: "local".into(),
                size_bytes: 1,
                format: ImageFormat::Qcow2,
                content: Default::default(),
                base: None,
            },
        );
        let inv = Inventory {
            nodes: vec![offline, maint, far, near.clone()],
            vms: vec![],
            volumes: vec![volume],
            pools: vec![pool],
            networks: vec![],
            rules: vec![],
            groups: vec![],
            ..Inventory::default()
        };
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        assert_eq!(sched(&me, &inv), Ok(near.id));

        let empty = Inventory::default();
        assert_eq!(sched(&me, &empty), Err(NoNode));
    }

    #[test]
    fn never_places_a_template() {
        let n = node(64000);
        let inv = Inventory {
            nodes: vec![n.clone()],
            ..Default::default()
        };
        let mut me = spec(512, None, PowerState::Running);
        assert_eq!(sched(&me, &inv), Ok(n.id));
        me.template = true;
        assert_eq!(sched(&me, &inv), Err(NoNode));
        assert_eq!(
            node_fits(&me, &inv, &n),
            Err("the VM is a template and is never started".into())
        );
    }

    #[test]
    fn skips_witness_nodes() {
        // 위트니스는 메모리가 많고 다른 조건을 갖춰도 VM을 돌리지 않는다.
        let mut witness = node(64000);
        witness.spec.witness = true;
        let small = node(1024);
        let mut inv = Inventory {
            nodes: vec![witness, small.clone()],
            ..Default::default()
        };
        let me = spec(512, None, PowerState::Running);
        assert_eq!(sched(&me, &inv), Ok(small.id));
        inv.nodes.pop();
        assert_eq!(sched(&me, &inv), Err(NoNode));
    }

    #[test]
    fn ha_vms_go_only_to_ha_capable_nodes() {
        // 메모리가 가장 많은 노드도 펜싱이 꺼져 있으면 HA VM을 받지 않는다.
        let off = node(64000);
        let mut fenced = node(1024);
        fenced.status.ha = Some(NodeHa {
            fencing: Fencing::Process,
            recover_after_secs: 7,
            fence_after_secs: None,
        });
        let mut inv = Inventory {
            nodes: vec![off.clone(), fenced.clone()],
            ..Default::default()
        };
        let mut me = spec(512, None, PowerState::Running);
        assert_eq!(sched(&me, &inv), Ok(off.id));
        me.ha = Some(VmHa::default());
        assert_eq!(sched(&me, &inv), Ok(fenced.id));
        assert_eq!(
            node_fits(&me, &inv, &off),
            Err("node \"n\" does not support HA (fencing is off)".into())
        );
        inv.nodes.pop();
        assert_eq!(sched(&me, &inv), Err(NoNode));
    }

    fn ha_node(memory_mib: u64) -> Node {
        let mut n = node(memory_mib);
        n.status.ha = Some(NodeHa {
            fencing: Fencing::Process,
            recover_after_secs: 12,
            fence_after_secs: Some(10),
        });
        n
    }

    fn placed(memory_mib: u32, node: &Node, ha: bool) -> Vm {
        let mut s = spec(memory_mib, Some(node.id), PowerState::Running);
        s.ha = ha.then(VmHa::default);
        Resource::new("other", s)
    }

    fn reserve(r: Option<HaReserve>) -> SettingsSpec {
        SettingsSpec {
            ha_reserve: r,
            ..SettingsSpec::default()
        }
    }

    #[test]
    fn ha_capacity_passes_without_a_reservation_or_for_non_ha_vms() {
        let a = ha_node(1000);
        let inv = Inventory {
            nodes: vec![a.clone()],
            vms: vec![placed(1000, &a, false)],
            ..Default::default()
        };
        let mut me: Vm = Resource::new("me", spec(4000, None, PowerState::Running));
        me.spec.ha = Some(VmHa::default());
        assert_eq!(ha_capacity(&inv, &reserve(None), &me), Ok(()));
        // HA가 아니거나 `stopped`·`ignored` 정책이면 예약을 보지 않는다.
        let nodes = reserve(Some(HaReserve::Nodes(1)));
        assert!(ha_capacity(&inv, &nodes, &me).is_err());
        for ha in [
            None,
            Some(VmHa {
                state: HaState::Stopped,
                ..Default::default()
            }),
            Some(VmHa {
                state: HaState::Ignored,
                ..Default::default()
            }),
        ] {
            me.spec.ha = ha;
            assert_eq!(ha_capacity(&inv, &nodes, &me), Ok(()));
        }
    }

    #[test]
    fn ha_capacity_reserves_the_largest_nodes_or_a_percentage() {
        let a = ha_node(4000);
        let b = ha_node(2000);
        let c = ha_node(2000);
        // HA 아닌 VM도 자리를 차지한다.
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone(), c.clone()],
            vms: vec![placed(1000, &b, false), placed(1000, &c, true)],
            ..Default::default()
        };
        let mut me: Vm = Resource::new("me", spec(2000, None, PowerState::Running));
        me.spec.ha = Some(VmHa::default());
        // 8000 - 가장 큰 4000 = 4000: 2000 + 2000이 들어간다.
        let one = reserve(Some(HaReserve::Nodes(1)));
        assert_eq!(ha_capacity(&inv, &one, &me), Ok(()));
        me.spec.memory_mib = 2001;
        assert_eq!(
            ha_capacity(&inv, &one, &me),
            Err(HaCapacity {
                capacity_mib: 8000,
                reserve_mib: 4000,
                used_mib: 2000,
                need_mib: 2001,
            })
        );
        // 두 대: 4000 + 2000을 비운다.
        me.spec.memory_mib = 1;
        let two = reserve(Some(HaReserve::Nodes(2)));
        assert_eq!(ha_capacity(&inv, &two, &me).unwrap_err().reserve_mib, 6000);
        // 노드보다 많이 예약하면 아무것도 켜지 않는다.
        let many = reserve(Some(HaReserve::Nodes(8)));
        assert_eq!(ha_capacity(&inv, &many, &me).unwrap_err().reserve_mib, 8000);
        // 25% = 2000: 8000 - 2000 - 2000 = 4000까지
        let quarter = reserve(Some(HaReserve::Percent(25)));
        me.spec.memory_mib = 4000;
        assert_eq!(ha_capacity(&inv, &quarter, &me), Ok(()));
        me.spec.memory_mib = 4001;
        assert_eq!(
            ha_capacity(&inv, &quarter, &me).unwrap_err().reserve_mib,
            2000
        );
        // 이 VM이 이미 자리를 잡고 있어도 한 번만 센다.
        let mut inv = inv;
        me.spec.memory_mib = 2000;
        me.spec.node = Some(a.id);
        inv.vms.push(me.clone());
        assert_eq!(ha_capacity(&inv, &one, &me), Ok(()));
    }

    #[test]
    fn ha_capacity_counts_only_usable_ha_nodes() {
        let a = ha_node(4000);
        let mut maintenance = ha_node(4000);
        maintenance.spec.maintenance = true;
        let mut offline = ha_node(4000);
        offline.status.online = false;
        let mut witness = ha_node(4000);
        witness.spec.witness = true;
        let mut shutting = ha_node(4000);
        shutting.status.shutdown = Some(NodeShutdown {
            action: ShutdownAction::Failover,
            maintenance: false,
            at: 1,
        });
        let plain = node(64000);
        let inv = Inventory {
            nodes: vec![
                a.clone(),
                maintenance.clone(),
                offline.clone(),
                witness,
                shutting,
                plain.clone(),
            ],
            // 쓸 수 없는 노드에 놓인 VM은 `used`에 들지 않는다. 꺼진 VM도 세지 않는다.
            vms: vec![
                placed(3000, &maintenance, false),
                placed(3000, &plain, false),
                Resource::new("off", spec(3000, Some(a.id), PowerState::Stopped)),
                placed(1000, &a, false),
            ],
            ..Default::default()
        };
        let mut me: Vm = Resource::new("me", spec(1000, None, PowerState::Running));
        me.spec.ha = Some(VmHa::default());
        let half = reserve(Some(HaReserve::Percent(50)));
        assert_eq!(ha_capacity(&inv, &half, &me), Ok(()));
        me.spec.memory_mib = 1001;
        assert_eq!(
            ha_capacity(&inv, &half, &me),
            Err(HaCapacity {
                capacity_mib: 4000,
                reserve_mib: 2000,
                used_mib: 1000,
                need_mib: 1001,
            })
        );
    }

    /// 유지보수(비우기)·종료 중인 노드의 켜진 `started` HA VM은 곧 대상 노드로 오므로 `used`에 센다. 그 노드는
    /// `capacity`에 들지 않고, 그 노드의 HA 아닌 VM·`stopped` 정책 VM·꺼진 VM, 얼리는 노드와 오프라인 노드의 VM은
    /// 세지 않는다.
    #[test]
    fn ha_capacity_counts_ha_vms_leaving_maintenance_or_shutting_down_nodes() {
        let a = ha_node(8000);
        let mut maintenance = ha_node(4000);
        maintenance.spec.maintenance = true;
        let mut shutting = ha_node(4000);
        shutting.status.shutdown = Some(NodeShutdown {
            action: ShutdownAction::Migrate,
            maintenance: true,
            at: 1,
        });
        let mut offline = ha_node(4000);
        offline.status.online = false;
        let mut freezing = ha_node(4000);
        freezing.status.shutdown = Some(NodeShutdown {
            action: ShutdownAction::Freeze,
            maintenance: false,
            at: 1,
        });
        let mut parked = placed(500, &maintenance, true);
        parked.spec.ha = Some(VmHa {
            state: HaState::Stopped,
            ..Default::default()
        });
        let inv = Inventory {
            nodes: vec![
                a.clone(),
                maintenance.clone(),
                shutting.clone(),
                offline.clone(),
                freezing.clone(),
            ],
            vms: vec![
                placed(1000, &maintenance, true),
                placed(2000, &shutting, true),
                placed(3000, &maintenance, false),
                parked,
                Resource::new("off", {
                    let mut s = spec(3000, Some(shutting.id), PowerState::Stopped);
                    s.ha = Some(VmHa::default());
                    s
                }),
                placed(3000, &offline, true),
                placed(3000, &freezing, true),
            ],
            ..Default::default()
        };
        let mut me: Vm = Resource::new("me", spec(1000, None, PowerState::Running));
        me.spec.ha = Some(VmHa::default());
        // 8000 - 25% 2000 - 옮겨 올 1000 + 2000 = 3000이 남는다.
        let quarter = reserve(Some(HaReserve::Percent(25)));
        assert_eq!(ha_capacity(&inv, &quarter, &me), Ok(()));
        me.spec.memory_mib = 3001;
        assert_eq!(
            ha_capacity(&inv, &quarter, &me),
            Err(HaCapacity {
                capacity_mib: 8000,
                reserve_mib: 2000,
                used_mib: 3000,
                need_mib: 3001,
            })
        );
    }

    /// 마이그레이션 중인 VM은 원본 노드(`spec.node`)에서 한 번만 센다.
    #[test]
    fn ha_capacity_counts_a_migrating_vm_once() {
        let a = ha_node(4000);
        let b = ha_node(4000);
        let mut moving = placed(2000, &a, true);
        moving.status.migration = Some(opl_types::vm::MigrationStatus {
            node: b.id,
            task: Uuid::nil(),
            disks: vec![],
        });
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            vms: vec![moving],
            ..Default::default()
        };
        let mut me: Vm = Resource::new("me", spec(2000, None, PowerState::Running));
        me.spec.ha = Some(VmHa::default());
        // 8000 - 4000 예약 - 2000 = 2000이 남는다.
        let one = reserve(Some(HaReserve::Nodes(1)));
        assert_eq!(ha_capacity(&inv, &one, &me), Ok(()));
        me.spec.memory_mib = 2001;
        assert_eq!(ha_capacity(&inv, &one, &me).unwrap_err().used_mib, 2000);
    }

    #[test]
    fn vms_with_security_groups_go_only_to_nodes_with_a_firewall() {
        // 메모리가 가장 많은 노드도 방화벽이 없으면 보안 그룹 NIC가 있는 VM을 받지 않는다.
        let bare = node(64000);
        let mut guarded = node(1024);
        guarded.status.firewall = true;
        let mut inv = Inventory {
            nodes: vec![bare.clone(), guarded.clone()],
            networks: vec![Resource::new(
                "lan",
                NetworkSpec {
                    bridge: Some("vmbr0".into()),
                    vlan: None,
                    vxlan: None,
                    mtu: None,
                    folder: None,
                },
            )],
            ..Default::default()
        };
        for n in &mut inv.nodes {
            n.status.bridges = vec![BridgeInfo {
                name: "vmbr0".into(),
                vlan_filtering: false,
            }];
        }
        let mut me = spec(512, None, PowerState::Running);
        me.nics = vec![NicSpec {
            network: "lan".into(),
            model: NicModel::Virtio,
            mac: None,
            security_groups: Vec::new(),
        }];
        assert_eq!(sched(&me, &inv), Ok(bare.id));
        me.nics[0].security_groups = vec!["web".into()];
        assert_eq!(sched(&me, &inv), Ok(guarded.id));
        assert_eq!(
            node_fits(&me, &inv, &inv.nodes[0]),
            Err(
                "node \"n\" does not have a firewall (nftables) for the VM's security groups"
                    .into()
            )
        );
        inv.nodes.pop();
        assert_eq!(sched(&me, &inv), Err(NoNode));
    }

    #[test]
    fn local_volumes_stay_on_their_node_and_shared_pools_need_a_ready_node() {
        let big = node(64000);
        let mid = node(32000);
        let small = node(1024);
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        let inv = |pool: StoragePool, volume: Volume, ready: [bool; 3]| Inventory {
            nodes: [&big, &mid, &small]
                .into_iter()
                .zip(ready)
                .map(|(n, r)| with_pool(n.clone(), &pool, r))
                .collect(),
            volumes: vec![volume],
            pools: vec![pool],
            ..Default::default()
        };

        // 공유가 아닌 풀은 모든 노드가 쓸 수 있어도 볼륨 파일이 있는 노드로 간다.
        let local = inv(
            pool("local", false),
            volume("root", "local", Some(small.id)),
            [true; 3],
        );
        assert_eq!(sched(&me, &local), Ok(small.id));
        // 그 노드에서 풀이 준비되지 않았으면 갈 곳이 없다.
        let local = inv(
            pool("local", false),
            volume("root", "local", Some(small.id)),
            [true, true, false],
        );
        assert_eq!(sched(&me, &local), Err(NoNode));

        // 공유 풀은 준비된 노드 가운데 메모리가 가장 많은 노드로 간다.
        let shared = inv(
            pool("nas", true),
            volume("root", "nas", Some(small.id)),
            [false, true, true],
        );
        assert_eq!(sched(&me, &shared), Ok(mid.id));
        // RBD 풀은 `shared` 값과 상관없이 공유다. 볼륨을 만든 노드에 묶이지 않는다.
        let mut ceph = pool("ceph", false);
        ceph.spec.backend = PoolBackend::Rbd {
            pool: "rbd".into(),
            namespace: None,
            user: None,
            conf: None,
        };
        let rbd = inv(
            ceph,
            volume("root", "ceph", Some(small.id)),
            [false, true, true],
        );
        assert_eq!(sched(&me, &rbd), Ok(mid.id));
        // 상태를 알리지 않은 노드(옛 노드, 첫 새로 고침 전)는 NFS 풀에는 후보가 아니고, 디렉터리
        // 풀에는 후보다.
        let mut nfs = pool("nas", true);
        nfs.spec.backend = PoolBackend::Nfs {
            server: "nas".into(),
            export: "/srv/vm".into(),
            options: None,
        };
        let mut unknown = inv(nfs, volume("root", "nas", None), [true; 3]);
        for n in &mut unknown.nodes {
            n.status.pools.clear();
        }
        assert_eq!(sched(&me, &unknown), Err(NoNode));
        unknown.pools[0] = pool("nas", true);
        assert_eq!(sched(&me, &unknown), Ok(big.id));
    }

    /// 노드 로컬 풀의 볼륨인데 어느 노드에 있는지 모르면(`status.node` 없음) 아무 노드에서나 켜지 않는다. 풀이 한
    /// 노드에만 있으면 그 노드다.
    #[test]
    fn local_volumes_on_unknown_nodes_are_reached_only_where_the_pool_is() {
        let (a, b) = (node(4096), node(4096));
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        });
        let mut inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            volumes: vec![volume("root", "local", None)],
            pools: vec![pool("local", false)],
            ..Default::default()
        };
        assert!(node_fits(&me, &inv, &a).is_err());
        assert!(node_fits(&me, &inv, &b).is_err());
        inv.pools[0].spec.nodes = vec![b.id];
        assert!(node_fits(&me, &inv, &a).is_err());
        assert_eq!(node_fits(&me, &inv, &b), Ok(()));
    }

    /// CD-ROM의 ISO도 디스크처럼 본다: 노드 로컬 풀의 ISO는 그 노드에서만, 공유 풀의 ISO는 어디서나. 빈 드라이브는
    /// 노드를 묶지 않는다.
    #[test]
    fn local_isos_keep_the_vm_on_their_node() {
        let (a, b) = (node(64000), node(1024));
        let mut me = spec(512, None, PowerState::Running);
        me.cdroms = vec![
            CdromSpec {
                iso: Some("installer".into()),
                boot_order: None,
            },
            CdromSpec::default(),
        ];
        let local = pool("local", false);
        let nas = pool("nas", true);
        let mut inv = Inventory {
            nodes: [&a, &b]
                .into_iter()
                .map(|n| with_pool(with_pool(n.clone(), &local, true), &nas, true))
                .collect(),
            volumes: vec![volume("installer", "local", Some(b.id))],
            pools: vec![local, nas],
            ..Default::default()
        };
        assert_eq!(sched(&me, &inv), Ok(b.id));
        assert_eq!(
            node_fits(&me, &inv, &inv.nodes[0]),
            Err("node \"n\" cannot use the storage pools of the VM's disks".into())
        );
        inv.volumes[0].spec.pool = "nas".into();
        assert_eq!(sched(&me, &inv), Ok(a.id));
        me.cdroms[0].iso = None;
        inv.volumes[0].spec.pool = "local".into();
        assert_eq!(sched(&me, &inv), Ok(a.id));
    }

    #[test]
    fn disk_moves_copy_only_local_disks_to_the_same_or_given_pool() {
        let node1 = node(4096);
        let mut me = spec(512, Some(node1.id), PowerState::Running);
        for name in ["root", "data", "shared"] {
            me.disks.push(DiskSpec {
                volume: name.into(),
                bus: DiskBus::VirtioBlk,
                boot_order: None,
            });
        }
        let mut raw = volume("data", "local", Some(node1.id));
        raw.spec.format = ImageFormat::Raw;
        let root = volume("root", "local", Some(node1.id));
        let mut thin = pool("thin", false);
        thin.spec.backend = PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "tp".into(),
        };
        let inv = Inventory {
            nodes: vec![node1],
            volumes: vec![root.clone(), raw.clone(), volume("shared", "nas", None)],
            pools: vec![pool("local", false), pool("nas", true), thin.clone()],
            ..Default::default()
        };
        let mv = |v: &Volume, pool: &str, format| DiskMove {
            volume: v.id,
            pool: pool.into(),
            format,
            existing: false,
        };
        // 공유 풀의 디스크는 옮기지 않는다. 풀을 주지 않으면 같은 이름의 풀, 같은 형식이다.
        assert_eq!(
            disk_moves(&me, &inv, None),
            [
                mv(&root, "local", ImageFormat::Qcow2),
                mv(&raw, "local", ImageFormat::Raw)
            ]
        );
        // 블록 풀은 raw만 담는다. 바뀌는 것은 qcow2 → raw뿐이다(파일 풀의 raw는 raw 그대로).
        assert_eq!(
            disk_moves(&me, &inv, Some(&thin)),
            [
                mv(&root, "thin", ImageFormat::Raw),
                mv(&raw, "thin", ImageFormat::Raw)
            ]
        );
        let nas = pool("nas", true);
        assert_eq!(
            disk_moves(&me, &inv, Some(&nas)),
            [
                mv(&root, "nas", ImageFormat::Qcow2),
                mv(&raw, "nas", ImageFormat::Raw)
            ]
        );
        // 로컬 디스크가 없으면 계획이 비고, 풀을 줘도 그렇다.
        me.disks.clear();
        assert_eq!(disk_moves(&me, &inv, Some(&nas)), []);
    }

    /// TPM·Secure Boot VM은 그 능력을 알린 노드에만 놓는다(옛 노드는 능력이 없는 것으로 본다).
    #[test]
    fn tpm_and_secure_boot_vms_need_the_node_capabilities() {
        use opl_types::node::FirmwareCaps;
        let mut n = node(4096);
        n.name = "a".into();
        let inv = Inventory::default();
        let mut me = spec(512, None, PowerState::Running);
        me.firmware = opl_types::vm::Firmware::Uefi;
        assert_eq!(node_fits(&me, &inv, &n), Ok(()));
        me.tpm = true;
        assert_eq!(
            node_fits(&me, &inv, &n),
            Err("node \"a\" has no swtpm for the VM's TPM (install swtpm and swtpm-tools)".into())
        );
        n.status.firmware_caps = Some(FirmwareCaps {
            tpm: true,
            secure_boot: false,
        });
        assert_eq!(node_fits(&me, &inv, &n), Ok(()));
        me.secure_boot = true;
        assert!(
            node_fits(&me, &inv, &n)
                .unwrap_err()
                .contains("has no Secure Boot firmware"),
        );
        n.status.firmware_caps = Some(FirmwareCaps {
            tpm: true,
            secure_boot: true,
        });
        assert_eq!(node_fits(&me, &inv, &n), Ok(()));
    }

    #[test]
    fn node_fits_with_moves_checks_the_target_pool_instead_of_the_volume_node() {
        let mut source = node(4096);
        source.name = "node1".into();
        let mut target = node(4096);
        target.name = "node2".into();
        let mut me = spec(512, Some(source.id), PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        let local = pool("local", false);
        let mut fast = pool("fast", false);
        fast.spec.nodes = vec![source.id];
        let root = volume("root", "local", Some(source.id));
        let mut inv = Inventory {
            nodes: vec![source.clone(), target.clone()],
            volumes: vec![root.clone()],
            pools: vec![local, fast],
            ..Default::default()
        };
        // 옮기지 않으면 로컬 볼륨은 그 파일이 있는 노드에서만 쓸 수 있다.
        assert_eq!(
            node_fits(&me, &inv, &target),
            Err("node \"node2\" cannot use the storage pools of the VM's disks".into())
        );
        let moves = disk_moves(&me, &inv, None);
        assert_eq!(node_fits_with(&me, &inv, &target, &moves), Ok(()));
        // 대상 풀을 그 노드에서 쓸 수 없으면 그 풀과 디스크를 알린다.
        inv.nodes[1] = with_pool(target.clone(), &inv.pools[0], false);
        assert_eq!(
            node_fits_with(&me, &inv, &inv.nodes[1], &moves),
            Err("node \"node2\" cannot use storage pool \"local\" for disk \"root\"".into())
        );
        let fast = inv.pools[1].clone();
        let moves = disk_moves(&me, &inv, Some(&fast));
        assert_eq!(
            node_fits_with(&me, &inv, &target, &moves),
            Err("node \"node2\" cannot use storage pool \"fast\" for disk \"root\"".into())
        );
        inv.pools[1].spec.nodes.clear();
        let moves = disk_moves(&me, &inv, Some(&inv.pools[1]));
        assert_eq!(node_fits_with(&me, &inv, &target, &moves), Ok(()));
    }

    /// 복제 대상으로 옮기면(계획 55) 로컬 디스크는 그 대상 풀의 복제본으로의 이동(`existing`)이고, 다른 풀을 주면 거절한다.
    /// 복제 대상이 아니면 [`disk_moves`]와 같다.
    #[test]
    fn migration_moves_use_the_replica_of_a_replication_target() {
        let mut source = node(4096);
        source.name = "node1".into();
        let mut target = node(4096);
        target.name = "node2".into();
        let other = node(4096);
        let mut me = spec(512, Some(source.id), PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        me.replication = vec![opl_types::replication::ReplicationTarget {
            node: target.id,
            schedule: "*/15".into(),
            pool: Some("b".into()),
        }];
        let vm: Vm = Resource::new("web", me.clone());
        let root = volume("root", "a", Some(source.id));
        let inv = Inventory {
            nodes: vec![source, target.clone(), other.clone()],
            volumes: vec![root.clone()],
            pools: vec![pool("a", false), pool("b", false), pool("c", false)],
            ..Default::default()
        };
        let mv = |pool: &str, existing| DiskMove {
            volume: root.id,
            pool: pool.into(),
            format: ImageFormat::Qcow2,
            existing,
        };
        assert_eq!(
            migration_moves(&vm, &inv, target.id, None),
            Ok(vec![mv("b", true)])
        );
        assert_eq!(
            migration_moves(&vm, &inv, target.id, Some(&inv.pools[1])),
            Ok(vec![mv("b", true)])
        );
        assert_eq!(
            migration_moves(&vm, &inv, target.id, Some(&inv.pools[2])),
            Err("node \"node2\" holds the replica of VM \"web\" in pool \"b\"; migrate without --pool or remove the node from the replication targets".into())
        );
        // 복제 대상이 아닌 노드는 계획 42 그대로(같은 이름의 풀 또는 준 풀에 새로 만든다).
        assert_eq!(
            migration_moves(&vm, &inv, other.id, None),
            Ok(vec![mv("a", false)])
        );
        assert_eq!(
            migration_moves(&vm, &inv, other.id, Some(&inv.pools[2])),
            Ok(vec![mv("c", false)])
        );
        // 풀을 두지 않은 대상은 디스크마다 같은 이름의 풀이다.
        let mut same = vm.clone();
        same.spec.replication[0].pool = None;
        assert_eq!(
            migration_moves(&same, &inv, target.id, Some(&inv.pools[0])),
            Ok(vec![mv("a", true)])
        );
    }

    #[test]
    fn needs_the_bridges_of_its_networks() {
        let bridge = |name: &str, vlan_filtering| BridgeInfo {
            name: name.into(),
            vlan_filtering,
        };
        let mut none = node(64000);
        none.status.bridges = vec![bridge("vmbr1", true)];
        let mut plain = node(32000);
        plain.status.bridges = vec![bridge("vmbr0", false)];
        let mut aware = node(1024);
        aware.status.bridges = vec![bridge("vmbr0", true)];
        let network = |name: &str, vlan| -> Network {
            Resource::new(
                name,
                NetworkSpec {
                    bridge: Some("vmbr0".into()),
                    vlan,
                    vxlan: None,
                    mtu: None,
                    folder: None,
                },
            )
        };
        let inv = Inventory {
            nodes: vec![none, plain.clone(), aware.clone()],
            networks: vec![network("lan", None), network("dmz", Some(20))],
            ..Default::default()
        };
        let nic = |network: &str| NicSpec {
            network: network.into(),
            model: NicModel::Virtio,
            mac: None,
            security_groups: Vec::new(),
        };
        let mut me = spec(512, None, PowerState::Running);
        me.nics = vec![nic("user"), nic("lan")];
        assert_eq!(sched(&me, &inv), Ok(plain.id));
        // VLAN을 쓰려면 VLAN-aware 브리지가 있어야 한다.
        me.nics.push(nic("dmz"));
        assert_eq!(sched(&me, &inv), Ok(aware.id));
        // 없는 네트워크는 어디서도 돌 수 없다.
        me.nics.push(nic("gone"));
        assert_eq!(sched(&me, &inv), Err(NoNode));
    }

    #[test]
    fn needs_the_managed_bridge_of_a_vxlan_network() {
        let bridge = |name: &str| BridgeInfo {
            name: name.into(),
            vlan_filtering: false,
        };
        // 이 노드에는 아직 VXLAN 장치가 없다(underlay가 없는 노드 등).
        let mut bare = node(64000);
        bare.status.bridges = vec![bridge("vmbr0")];
        let mut ready = node(1024);
        ready.status.bridges = vec![bridge("vmbr0"), bridge("oplv1000")];
        let inv = Inventory {
            nodes: vec![bare, ready.clone()],
            networks: vec![Resource::new(
                "overlay",
                NetworkSpec {
                    bridge: None,
                    vlan: None,
                    vxlan: Some(Vxlan { vni: 1000 }),
                    mtu: None,
                    folder: None,
                },
            )],
            ..Default::default()
        };
        let mut me = spec(512, None, PowerState::Running);
        me.nics = vec![NicSpec {
            network: "overlay".into(),
            model: NicModel::Virtio,
            mac: None,
            security_groups: Vec::new(),
        }];
        assert_eq!(sched(&me, &inv), Ok(ready.id));
    }

    #[test]
    fn needs_the_architecture_of_its_machine() {
        let x86 = {
            let mut n = node(64000);
            n.status.arch = Some(Arch::X86_64);
            n
        };
        let unknown = {
            let mut n = node(32000);
            n.status.arch = None;
            n
        };
        let arm = node(1024);
        let inv = Inventory {
            nodes: vec![x86.clone(), unknown, arm.clone()],
            ..Default::default()
        };
        // virt는 aarch64 노드에서만 돈다. arch를 모르는 노드(옛 버전)는 후보가 아니다.
        let mut me = spec(512, None, PowerState::Running);
        assert_eq!(sched(&me, &inv), Ok(arm.id));
        me.machine = MachineType::Q35;
        assert_eq!(sched(&me, &inv), Ok(x86.id));
        let inv = Inventory {
            nodes: vec![arm],
            ..Default::default()
        };
        assert_eq!(sched(&me, &inv), Err(NoNode));
    }

    #[test]
    fn node_fits_names_why_a_node_cannot_run_the_vm() {
        let pool = pool("nas", true);
        let ready = with_pool(node(8192), &pool, true);
        let inv = Inventory {
            nodes: vec![ready.clone()],
            volumes: vec![volume("root", "nas", None)],
            pools: vec![pool.clone()],
            networks: vec![Resource::new(
                "lan",
                NetworkSpec {
                    bridge: Some("vmbr0".into()),
                    vlan: None,
                    vxlan: None,
                    mtu: None,
                    folder: None,
                },
            )],
            ..Default::default()
        };
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        assert_eq!(node_fits(&me, &inv, &ready), Ok(()));

        let mut offline = ready.clone();
        offline.status.online = false;
        assert_eq!(
            node_fits(&me, &inv, &offline),
            Err("node \"n\" is offline".into())
        );
        let mut maint = ready.clone();
        maint.spec.maintenance = true;
        assert_eq!(
            node_fits(&me, &inv, &maint),
            Err("node \"n\" is in maintenance".into())
        );
        let mut closing = ready.clone();
        closing.status.shutdown = Some(NodeShutdown {
            action: ShutdownAction::Freeze,
            maintenance: false,
            at: 1,
        });
        assert_eq!(
            node_fits(&me, &inv, &closing),
            Err("node \"n\" is shutting down".into())
        );
        // 위트니스는 다른 조건보다 먼저 거절한다.
        let mut witness = offline.clone();
        witness.spec.witness = true;
        assert_eq!(
            node_fits(&me, &inv, &witness),
            Err("node \"n\" is a witness node".into())
        );
        let mut x86 = ready.clone();
        x86.status.arch = Some(Arch::X86_64);
        assert_eq!(
            node_fits(&me, &inv, &x86),
            Err(
                "node \"n\" does not have the aarch64 architecture of the VM's machine type".into()
            )
        );
        let not_ready = with_pool(node(8192), &pool, false);
        assert_eq!(
            node_fits(&me, &inv, &not_ready),
            Err("node \"n\" cannot use the storage pools of the VM's disks".into())
        );
        me.nics.push(NicSpec {
            network: "lan".into(),
            model: NicModel::Virtio,
            mac: None,
            security_groups: Vec::new(),
        });
        assert_eq!(
            node_fits(&me, &inv, &ready),
            Err("node \"n\" does not have the bridges of the VM's networks".into())
        );
    }

    // ---- 계획 63: CPU 모델 ----

    /// Mock 카탈로그(`Icelake-Server-v7`은 `avx512vbmi`가 없어 못 켬)와, 그것을 켤 수 있는 강한 카탈로그
    fn catalogs() -> (CpuCatalogSpec, CpuCatalogSpec) {
        let weak = opl_driver::mock_cpu_probe().catalog;
        let mut strong = weak.clone();
        for m in &mut strong.models {
            m.missing = cpu::Bits::default();
        }
        strong.supported = cpu::Bits::from_indices(0..strong.flags.len());
        (strong, weak)
    }

    /// `cpu-models`를 아는 x86_64 KVM 노드. `catalog`가 None이면 `NodeStatus.cpu`가 없다.
    fn x86(name: &str, catalog: Option<&CpuCatalogSpec>) -> Node {
        let mut n = node(8192);
        n.name = name.into();
        n.status.arch = Some(Arch::X86_64);
        n.status.hypervisor = "qemu-kvm".into();
        n.status.features = opl_types::node::FEATURES
            .iter()
            .map(|f| f.to_string())
            .collect();
        n.status.cpu = catalog.map(|c| cpu::NodeCpu {
            catalog: c.hash(),
            qemu: c.qemu.clone(),
            accel: "kvm".into(),
            vendor: Some("GenuineIntel".into()),
            model_id: None,
            probed_at: 0,
            error: None,
        });
        n
    }

    fn cpu_inv(nodes: Vec<Node>, catalogs: &[&CpuCatalogSpec]) -> Inventory {
        Inventory {
            nodes,
            cpu_catalogs: catalogs
                .iter()
                .map(|c| (c.hash(), Arc::new((*c).clone())))
                .collect(),
            ..Default::default()
        }
    }

    fn x86_spec(model: &str, flags: &[&str]) -> VmSpec {
        let mut me = spec(512, None, PowerState::Running);
        me.machine = MachineType::Q35;
        me.cpu.model = model.parse().unwrap();
        me.cpu.flags = flags.iter().map(|f| f.parse().unwrap()).collect();
        me
    }

    fn baseline(model: &str) -> Baseline {
        Baseline {
            model: model.into(),
            flags: Default::default(),
            mode: cpu::BaselineSource::Auto,
            nodes: vec![],
            limited_by: vec![],
            since: 0,
            previous: None,
            warnings: vec![],
        }
    }

    #[test]
    fn named_cpu_models_need_a_node_whose_catalog_can_run_them() {
        let (strong, weak) = catalogs();
        let mut missing = x86("missing", Some(&strong));
        missing.status.cpu.as_mut().unwrap().catalog = "0".repeat(64);
        let mut empty = x86("empty", Some(&strong));
        empty.status.cpu.as_mut().unwrap().catalog = String::new();
        let nodes = vec![
            x86("strong", Some(&strong)),
            x86("weak", Some(&weak)),
            x86("none", None),
            missing,
            empty,
        ];
        let inv = cpu_inv(nodes.clone(), &[&strong, &weak]);
        let icelake = x86_spec("Icelake-Server-v7", &[]);
        assert_eq!(node_fits(&icelake, &inv, &nodes[0]), Ok(()));
        assert_eq!(
            node_fits(&icelake, &inv, &nodes[1]),
            Err("node \"weak\" cannot run CPU model Icelake-Server-v7 (missing avx512vbmi)".into())
        );
        assert_eq!(
            node_fits(&icelake, &inv, &nodes[2]),
            Err(
                "node \"none\" has no CPU catalog; only the host and max CPU models can run there"
                    .into()
            )
        );
        assert_eq!(
            node_fits(&icelake, &inv, &nodes[3]),
            Err("node \"missing\" has no CPU catalog yet; only the host and max CPU models can run there".into())
        );
        assert!(node_fits(&icelake, &inv, &nodes[4]).is_err());
        // 스케줄러는 켤 수 있는 노드만 고른다.
        assert_eq!(sched(&icelake, &inv), Ok(nodes[0].id));
        // 빠진 기능을 끄면 약한 노드도 켠다.
        let hidden = x86_spec("Icelake-Server-v7", &["-avx512vbmi"]);
        assert_eq!(node_fits(&hidden, &inv, &nodes[1]), Ok(()));
        // host·max는 카탈로그가 없어도 어디서나 된다.
        for model in ["host", "max"] {
            let me = x86_spec(model, &[]);
            for n in &nodes {
                assert_eq!(node_fits(&me, &inv, n), Ok(()), "{model} on {}", n.name);
            }
        }
        // 지원하지 않는 플래그를 켜면 못 켠다.
        let plus = x86_spec("Skylake-Server-v5", &["+avx512vbmi"]);
        assert!(node_fits(&plus, &inv, &nodes[1]).is_err());
        assert_eq!(node_fits(&plus, &inv, &nodes[0]), Ok(()));
        // 업그레이드 중(한 노드가 `cpu-models`를 모름)에는 작업자처럼 CPU를 보지 않는다.
        let mut old = inv.clone();
        old.nodes[2].status.features.clear();
        assert_eq!(node_fits(&icelake, &old, &old.nodes[1]), Ok(()));
        // 수준은 qemu64-v1 위의 플래그다(Mock 카탈로그에는 qemu64-v1이 없다).
        let level = x86_spec("x86-64-v3", &[]);
        assert!(
            node_fits(&level, &inv, &nodes[0])
                .unwrap_err()
                .contains("cannot run CPU model qemu64-v1")
        );
    }

    #[test]
    fn cluster_vms_resolve_the_recorded_baseline_and_mask_on_each_node() {
        let (strong, weak) = catalogs();
        let nodes = vec![x86("strong", Some(&strong)), x86("weak", Some(&weak))];
        let mut inv = cpu_inv(nodes.clone(), &[&strong, &weak]);
        let me = x86_spec("cluster", &[]);
        // 기준이 없으면 host처럼 어디서나 켠다.
        assert_eq!(node_fits(&me, &inv, &nodes[1]), Ok(()));
        inv.cpu_baselines
            .insert(Arch::X86_64, baseline("Icelake-Server-v7"));
        assert_eq!(node_fits(&me, &inv, &nodes[0]), Ok(()));
        assert!(
            node_fits(&me, &inv, &nodes[1])
                .unwrap_err()
                .contains("cannot run CPU model Icelake-Server-v7")
        );
        // 클러스터 숨김 목록이 빠진 기능을 끄면 약한 노드도 켠다.
        inv.cpu_mask = vec!["avx512vbmi".into()];
        assert_eq!(node_fits(&me, &inv, &nodes[1]), Ok(()));
        inv.cpu_mask.clear();
        inv.cpu_baselines
            .insert(Arch::X86_64, baseline("Skylake-Server-v5"));
        assert_eq!(node_fits(&me, &inv, &nodes[1]), Ok(()));
    }

    #[test]
    fn running_vms_move_only_where_their_recorded_cpu_runs() {
        let (strong, weak) = catalogs();
        let nodes = vec![x86("strong", Some(&strong)), x86("weak", Some(&weak))];
        let mut inv = cpu_inv(nodes.clone(), &[&strong, &weak]);
        // 약한 노드가 들어와 기준이 내려갔다. VM은 앞 기준(Icelake)으로 돈다.
        inv.cpu_baselines
            .insert(Arch::X86_64, baseline("Skylake-Server-v5"));
        let mut vm: Vm = Resource::new("web", x86_spec("cluster", &[]));
        let mut hw = RunningHardware::legacy(&vm.spec);
        hw.cpu_resolved = Some(cpu::ResolvedCpu {
            model: "Icelake-Server-v7".into(),
            flags: Default::default(),
            enforce: true,
            source: cpu::CpuSource::Cluster,
            dropped_mask: Vec::new(),
        });
        vm.status.hardware = Some(hw);
        // 새로 켜면(HA 복구) 새 기준이라 약한 노드도 되지만, 도는 VM은 기록대로 옮긴다.
        assert_eq!(node_fits(&vm.spec, &inv, &nodes[1]), Ok(()));
        assert_eq!(replica_fits(&vm, &inv, &nodes[1]), Ok(()));
        assert!(migration_fits(&vm, &inv, &nodes[1], &[]).is_err());
        assert!(replica_migration_fits(&vm, &inv, &nodes[1]).is_err());
        assert!(cpu_receives(&vm, &inv, &nodes[1]).is_err());
        assert_eq!(migration_fits(&vm, &inv, &nodes[0], &[]), Ok(()));
        // 계획 63 전 기록(host)은 어디로든 옮긴다.
        let mut legacy: Vm = Resource::new("old", x86_spec("host", &[]));
        legacy.status.hardware = Some(RunningHardware::legacy(&legacy.spec));
        assert_eq!(migration_fits(&legacy, &inv, &nodes[1], &[]), Ok(()));
        // 꺼진 VM(기록 없음)은 스펙을 해석한다.
        let mut off = vm.clone();
        off.status.hardware = None;
        assert_eq!(migration_fits(&off, &inv, &nodes[1], &[]), Ok(()));
    }

    /// 리뷰 1 C1: QEMU를 차례로 올리는 중 숨김 목록에 새 이름(`avx512vbmi`)이 있고 옛 노드의 QEMU는 그 이름을 모른다.
    #[test]
    fn hidden_flags_unknown_to_an_old_qemu_neither_break_starts_nor_leak_on_migration() {
        let (strong, _) = catalogs();
        let mut old_qemu = strong.clone();
        old_qemu.flags.retain(|f| f != "avx512vbmi");
        old_qemu.supported = cpu::Bits::from_indices(0..old_qemu.flags.len());
        old_qemu.qemu = "9.2.0".into();
        let nodes = vec![
            x86("new", Some(&strong)),
            x86("old", Some(&old_qemu)),
            x86("none", None),
            x86("twin", Some(&strong)),
        ];
        let mut inv = cpu_inv(nodes.clone(), &[&strong, &old_qemu]);
        inv.cpu_mask = vec!["avx512vbmi".into()];
        // 새로 켜기: 옛 노드는 모르는 숨김 이름을 빼고 켠다. 카탈로그가 없는 노드는 숨김을 확인할 수 없어 거절한다.
        let host = x86_spec("host", &[]);
        assert_eq!(node_fits(&host, &inv, &nodes[0]), Ok(()));
        assert_eq!(node_fits(&host, &inv, &nodes[1]), Ok(()));
        assert!(
            node_fits(&host, &inv, &nodes[2])
                .unwrap_err()
                .contains("has no CPU catalog, so the CPU flags avx512vbmi cannot be checked")
        );
        // VM이 스스로 끈 이름은 빼지 않는다 — 옛 노드는 못 켠다.
        let own = x86_spec("host", &["-avx512vbmi"]);
        assert!(
            node_fits(&own, &inv, &nodes[1])
                .unwrap_err()
                .contains("unknown flag avx512vbmi")
        );
        let running = |on: &Node, flags: &[(&str, bool)], dropped: &[&str]| {
            let mut vm: Vm = Resource::new("web", x86_spec("host", &[]));
            let mut hw = RunningHardware::legacy(&vm.spec);
            hw.cpu_resolved = Some(cpu::ResolvedCpu {
                model: "host".into(),
                flags: flags.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                enforce: false,
                source: cpu::CpuSource::Spec,
                dropped_mask: dropped.iter().map(|d| d.to_string()).collect(),
            });
            vm.status.hardware = Some(hw);
            vm.status.node = Some(on.id);
            vm
        };
        // 새 노드에서 켠 기록(avx512vbmi=off)은 옛 노드의 QEMU가 거절한다.
        let on_new = running(&nodes[0], &[("avx512vbmi", false)], &[]);
        assert!(
            migration_fits(&on_new, &inv, &nodes[1], &[])
                .unwrap_err()
                .contains("unknown flag avx512vbmi")
        );
        assert_eq!(migration_fits(&on_new, &inv, &nodes[3], &[]), Ok(()));
        // 옛 노드에서 켠 기록(이름을 뺌)을 새 노드로 옮기면 -cpu host가 숨긴 기능을 준다 — 막는다.
        let on_old = running(&nodes[1], &[], &["avx512vbmi"]);
        let e = migration_fits(&on_old, &inv, &nodes[0], &[]).unwrap_err();
        assert!(
            e.starts_with(
                "node \"new\" would expose the hidden CPU flags avx512vbmi if the VM moved there"
            ),
            "{e}"
        );
        assert!(cpu_receives(&on_old, &inv, &nodes[0]).is_err());
        // 리뷰 2 #1 경우 B: 카탈로그가 없는 대상은 확인할 수 없으니 막는다.
        assert!(
            migration_fits(&on_old, &inv, &nodes[2], &[])
                .unwrap_err()
                .contains("cannot be checked")
        );
        // 리뷰 2 #1 경우 A(제자리 QEMU 올리기): 옛 QEMU로 켠 VM이 도는 노드가 이제 그 이름을 아는 카탈로그를 알려도
        // 기록이 뺀 이름을 기억하므로 같은 새 QEMU 노드로도 옮기지 않는다.
        let upgraded = running(&nodes[3], &[], &["avx512vbmi"]);
        assert!(migration_fits(&upgraded, &inv, &nodes[0], &[]).is_err());
        // 숨김을 켜기 전에 켠 VM(뺀 이름 없음)은 이미 드러나 있다 — 옮겨도 달라지지 않는다(보류로 껐다 켜기를 알린다).
        let before_mask = running(&nodes[0], &[], &[]);
        assert_eq!(migration_fits(&before_mask, &inv, &nodes[3], &[]), Ok(()));
        assert_eq!(migration_fits(&before_mask, &inv, &nodes[2], &[]), Ok(()));
    }

    /// 리뷰 2 #3: 라이브 마이그레이션을 막는 플래그로 도는 VM은 어느 대상에도 맞지 않고 까닭을 말한다.
    #[test]
    fn a_vm_running_with_invtsc_is_not_live_migratable() {
        let (mut strong, _) = catalogs();
        strong.flags.push("invtsc".into());
        strong.flags.sort();
        strong.supported = cpu::Bits::from_indices(0..strong.flags.len());
        let nodes = vec![x86("a", Some(&strong)), x86("b", Some(&strong))];
        let inv = cpu_inv(nodes.clone(), &[&strong]);
        let mut vm: Vm = Resource::new("tsc", x86_spec("host", &["+invtsc"]));
        let mut hw = RunningHardware::legacy(&vm.spec);
        hw.cpu_resolved = Some(cpu::ResolvedCpu {
            model: "host".into(),
            flags: [("invtsc".to_string(), true)].into(),
            enforce: false,
            source: cpu::CpuSource::Spec,
            dropped_mask: Vec::new(),
        });
        vm.status.hardware = Some(hw);
        vm.status.node = Some(nodes[0].id);
        let e = migration_fits(&vm, &inv, &nodes[1], &[]).unwrap_err();
        assert_eq!(e, migration_blocked(&["invtsc"]));
        assert!(e.contains("+invtsc that QEMU cannot live-migrate"), "{e}");
        assert!(replica_migration_fits(&vm, &inv, &nodes[1]).is_err());
        assert!(cpu_receives(&vm, &inv, &nodes[1]).is_err());
        // 꺼진 뒤 다시 켜기(HA 복구)는 막지 않는다.
        assert_eq!(node_fits(&vm.spec, &inv, &nodes[1]), Ok(()));
        // 리뷰 2 #5: 비우기는 "no node with capacity" 대신 CPU 까닭을 보인다.
        assert_eq!(
            blocks_every_move(&vm, &inv, nodes[0].id),
            Some(migration_blocked(&["invtsc"]))
        );
        if let Some(hw) = &mut vm.status.hardware {
            hw.cpu_resolved.as_mut().unwrap().flags.clear();
        }
        assert_eq!(blocks_every_move(&vm, &inv, nodes[0].id), None);
    }

    #[test]
    fn ha_recovery_picks_only_nodes_that_can_run_the_cpu() {
        let (strong, weak) = catalogs();
        let mut big_weak = x86("weak", Some(&weak));
        big_weak.status.memory_mib = 65536;
        let nodes = vec![x86("strong", Some(&strong)), big_weak];
        let inv = cpu_inv(nodes.clone(), &[&strong, &weak]);
        let vm: Vm = Resource::new("web", x86_spec("Icelake-Server-v7", &[]));
        let pick = crate::ha::pick(
            &vm,
            &inv,
            &Assign::default(),
            |_| true,
            &Scorer::default(),
            true,
        );
        assert_eq!(pick, crate::ha::Pick::Node(nodes[0].id));
        // 메모리가 넉넉한 약한 노드라도 host VM이면 고른다(CPU 확인이 막는 것은 명명 모델뿐).
        let vm: Vm = Resource::new("web", x86_spec("host", &[]));
        let pick = crate::ha::pick(
            &vm,
            &inv,
            &Assign::default(),
            |_| true,
            &Scorer::default(),
            true,
        );
        assert_eq!(pick, crate::ha::Pick::Node(nodes[1].id));
        // 리뷰 1 I3: HA 검사의 라이브 옮기기는 켤 때 기록한 CPU로 본다. Skylake로 도는 VM의 스펙이 Icelake로 바뀌었어도
        // (보류) 약한 노드로 옮길 수 있다. 새로 켜는 복구는 스펙을 본다.
        let mut vm: Vm = Resource::new("db", x86_spec("Icelake-Server-v7", &[]));
        let mut hw = RunningHardware::legacy(&vm.spec);
        hw.cpu_resolved = Some(cpu::ResolvedCpu {
            model: "Skylake-Server-v5".into(),
            flags: Default::default(),
            enforce: true,
            source: cpu::CpuSource::Spec,
            dropped_mask: Vec::new(),
        });
        vm.status.hardware = Some(hw);
        vm.status.node = Some(nodes[0].id);
        let away = |n: &Node| n.id != nodes[0].id;
        let scorer = Scorer::default();
        assert!(matches!(
            crate::ha::pick(&vm, &inv, &Assign::default(), away, &scorer, true),
            crate::ha::Pick::NoNode(_)
        ));
        assert_eq!(
            crate::ha::pick_live(&vm, &inv, &Assign::default(), away, &scorer),
            crate::ha::Pick::Node(nodes[1].id)
        );
    }

    #[test]
    fn host_vms_warn_when_the_target_lacks_flags_of_the_source() {
        let (strong, weak) = catalogs();
        let nodes = vec![
            x86("strong", Some(&strong)),
            x86("weak", Some(&weak)),
            x86("twin", Some(&weak)),
            x86("none", None),
        ];
        let inv = cpu_inv(nodes.clone(), &[&strong, &weak]);
        let mut vm: Vm = Resource::new("web", x86_spec("host", &[]));
        vm.status.hardware = Some(RunningHardware::legacy(&vm.spec));
        assert_eq!(
            host_cpu_warning(&vm, &inv, &nodes[0], &nodes[1]).as_deref(),
            Some("target node \"weak\" lacks avx512vbmi that the guest may be using")
        );
        // 대상이 상위 집합, 같은 카탈로그, 카탈로그를 모르는 노드는 경고하지 않는다.
        assert_eq!(host_cpu_warning(&vm, &inv, &nodes[1], &nodes[0]), None);
        assert_eq!(host_cpu_warning(&vm, &inv, &nodes[1], &nodes[2]), None);
        assert_eq!(host_cpu_warning(&vm, &inv, &nodes[0], &nodes[3]), None);
        // 게스트에게 숨긴 플래그는 쓰지 않는다.
        let mut masked = vm.clone();
        masked.status.hardware.as_mut().unwrap().cpu_resolved = Some(cpu::ResolvedCpu {
            model: "host".into(),
            flags: [("avx512vbmi".to_string(), false)].into(),
            enforce: false,
            source: cpu::CpuSource::Spec,
            dropped_mask: Vec::new(),
        });
        assert_eq!(host_cpu_warning(&masked, &inv, &nodes[0], &nodes[1]), None);
        // 명명 모델은 노드 조건이 본다.
        let mut named: Vm = Resource::new("db", x86_spec("Skylake-Server-v5", &[]));
        named.status.hardware = Some(RunningHardware::legacy(&named.spec));
        assert_eq!(host_cpu_warning(&named, &inv, &nodes[0], &nodes[1]), None);
    }

    // -------------------------------------------------------------------------------------------------------------
    // 계획 64: 전용 CPU·hugepages·NUMA
    // -------------------------------------------------------------------------------------------------------------

    /// CPU 0-7(호스트 노드 하나, 16 GiB, 2M 풀 512개 = 1 GiB), 전용 `dedicated`
    fn numa_node(name: &str, dedicated: &str) -> Node {
        let mut n = node(16384);
        n.name = name.into();
        n.spec.host_resources = Some(numa::HostResources {
            reserved_cpus: Default::default(),
            dedicated_cpus: dedicated.parse().unwrap(),
            hugepages: Vec::new(),
        });
        n.status.topology = Some(
            serde_json::from_value(serde_json::json!({
                "sockets": 1, "threads_per_core": 1, "cpus": "0-7", "pinning": true, "cpuset": true,
                "probed_at": 1,
                "nodes": [{"id": 0, "cpus": "0-7", "memory_mib": 16384,
                           "hugepages": [{"size_kib": 2048, "total": 512}]}],
            }))
            .unwrap(),
        );
        n
    }

    /// `cores`개 vCPU의 전용 VM
    fn dedicated_spec(cores: u16, node: Option<Uuid>, power: PowerState) -> VmSpec {
        let mut s = spec(512, node, power);
        s.cpu.cores = cores;
        s.cpu.policy = numa::CpuPolicy::Dedicated;
        s
    }

    /// 노드에서 켠 것처럼 기록을 채운다(노드 할당기 대신 같은 맞춤 함수로).
    fn started_on(vm: &mut Vm, node: &Node, inv: &Inventory) {
        let topo = node.status.topology.as_ref().unwrap();
        let sets = HostSets::new(node.spec.host_resources.as_ref(), topo);
        let (usage, _) = inv.host_usage(node, topo, &sets, vm.id, &Assign::default());
        let free = HostFree::new(topo, &sets, &usage);
        let view = HostView {
            node: node.id,
            topology: topo,
            sets: &sets,
            free: &free,
        };
        let layout = numa::layout(&vm.spec, Some(topo)).unwrap().unwrap();
        let fit = numa::fit(&layout, Some(&view)).unwrap();
        let mut hw = RunningHardware::legacy(&vm.spec);
        hw.numa = Some(fit.numa);
        hw.host_placement = fit.placement;
        vm.status.hardware = Some(hw);
        vm.status.node = Some(node.id);
        vm.status.phase = opl_types::vm::VmPhase::Running;
        vm.spec.node = Some(node.id);
        vm.spec.power = PowerState::Running;
    }

    /// 스케줄러는 전용 CPU가 남은 노드만 고르고(토폴로지 없는 노드·전용 집합 없는 노드는 빼고), 도는 VM의 기록된
    /// pCPU와 놓였으나 아직 켜지 않은 VM의 요구를 센다. 남은 노드가 없으면 노드마다 까닭이다.
    #[test]
    fn scheduler_places_dedicated_vms_only_where_cpus_are_free() {
        let plain = node(16384);
        let n1 = numa_node("n1", "4-7");
        let n2 = numa_node("n2", "");
        let mut inv = Inventory {
            nodes: vec![plain.clone(), n1.clone(), n2.clone()],
            ..Inventory::default()
        };
        let want = dedicated_spec(2, None, PowerState::Stopped);
        assert_eq!(sched(&want, &inv), Ok(n1.id));
        assert_eq!(
            node_fits(&want, &inv, &plain),
            Err(r#"node "n" cannot pin vCPUs or bind memory (no topology)"#.into())
        );
        assert_eq!(
            node_fits(&want, &inv, &n2),
            Err(r#"node "n2" has no dedicated CPUs"#.into())
        );
        // n1에서 도는 2 vCPU 전용 VM(기록 4-5)과 놓였으나 아직 켜지 않은 1 vCPU VM
        let mut a: Vm = Resource::new("a", dedicated_spec(2, None, PowerState::Stopped));
        started_on(&mut a, &n1, &inv);
        assert_eq!(
            a.status
                .hardware
                .as_ref()
                .unwrap()
                .host_placement
                .as_ref()
                .unwrap()
                .vcpus,
            vec![4, 5]
        );
        inv.vms.push(a);
        assert_eq!(sched(&want, &inv), Ok(n1.id));
        let b: Vm = Resource::new("b", dedicated_spec(1, Some(n1.id), PowerState::Running));
        inv.vms.push(b);
        assert_eq!(sched(&want, &inv), Err(NoNode));
        assert_eq!(
            node_fits(&want, &inv, &n1),
            Err(
                r#"node "n1" has 1 free dedicated CPUs on host NUMA node 0; the VM needs 2 on one"#
                    .into()
            )
        );
        // 1 vCPU는 아직 들어간다.
        assert_eq!(
            sched(&dedicated_spec(1, None, PowerState::Stopped), &inv),
            Ok(n1.id)
        );
        // 공유 VM은 이 모든 것과 상관없다.
        assert!(sched(&spec(512, None, PowerState::Stopped), &inv).is_ok());
    }

    /// HA 회차의 자리(`Assign`)도 센다: 앞서 옮긴 전용 VM이 잡은 CPU에 다음 VM을 놓지 않는다.
    #[test]
    fn assigned_vms_take_their_dedicated_cpus() {
        let n1 = numa_node("n1", "4-7");
        let dead = numa_node("dead", "4-7");
        let a: Vm = Resource::new("a", dedicated_spec(3, Some(dead.id), PowerState::Running));
        let b: Vm = Resource::new("b", dedicated_spec(2, Some(dead.id), PowerState::Running));
        let inv = Inventory {
            nodes: vec![n1.clone()],
            vms: vec![a.clone(), b.clone()],
            ..Inventory::default()
        };
        let mut assign = Assign::default();
        assign.unplace(a.id);
        assign.unplace(b.id);
        assert!(vm_fits(&b, &inv, &n1, &assign).is_ok());
        assign.place(a.id, n1.id);
        assert_eq!(
            vm_fits(&b, &inv, &n1, &assign),
            Err(
                r#"node "n1" has 1 free dedicated CPUs on host NUMA node 0; the VM needs 2 on one"#
                    .into()
            )
        );
        assert!(replica_fits_with(&b, &inv, &n1, &assign).is_err());
    }

    /// 도는 VM을 옮길 때는 켤 때 기록한 모양을 대상에 맞춘다(`fit_fixed`): 대상의 전용 CPU가 모자라면 그 까닭이고, 그
    /// 노드로 오는 다른 마이그레이션의 몫도 센다. 다른 노드가 모두 막으면 비우기가 그 첫 까닭을 보인다.
    #[test]
    fn migrations_fit_the_recorded_shape() {
        let n1 = numa_node("n1", "4-7");
        let n2 = numa_node("n2", "6-7");
        let n3 = numa_node("n3", "4-7");
        let mut inv = Inventory {
            nodes: vec![n1.clone(), n2.clone(), n3.clone()],
            ..Inventory::default()
        };
        let mut a: Vm = Resource::new("a", dedicated_spec(2, None, PowerState::Stopped));
        started_on(&mut a, &n1, &inv);
        // 기록 뒤 스펙을 3 vCPU로 바꿔도(보류) 옮기기는 기록(2)으로 본다.
        a.spec.cpu.cores = 3;
        inv.vms.push(a.clone());
        assert_eq!(migration_fits(&a, &inv, &n2, &[]), Ok(()));
        assert_eq!(numa_receives(&a, &inv, &n2), Ok(()));
        // 같은 2 vCPU VM이 n2로 옮겨 가는 중이면 n2에 자리가 없다.
        let mut b: Vm = Resource::new("b", dedicated_spec(2, None, PowerState::Stopped));
        started_on(&mut b, &n3, &inv);
        b.status.migration = Some(opl_types::vm::MigrationStatus {
            node: n2.id,
            task: Uuid::nil(),
            disks: Vec::new(),
        });
        inv.vms.push(b);
        assert_eq!(
            numa_receives(&a, &inv, &n2),
            Err(
                r#"node "n2" has 0 free dedicated CPUs on host NUMA node 0; the VM needs 2 on one"#
                    .into()
            )
        );
        assert!(migration_fits(&a, &inv, &n2, &[]).is_err());
        // n3는 b가 떠나는 중이라 아직 잡혀 있고(4-5) 6-7이 남는다.
        assert_eq!(migration_fits(&a, &inv, &n3, &[]), Ok(()));
        let mut full3 = n3.clone();
        full3.spec.host_resources.as_mut().unwrap().dedicated_cpus = "4-5".parse().unwrap();
        inv.nodes = vec![n1.clone(), n2.clone(), full3];
        assert_eq!(
            blocks_every_move(&a, &inv, n1.id),
            Some(
                r#"node "n2" has 0 free dedicated CPUs on host NUMA node 0; the VM needs 2 on one"#
                    .into()
            )
        );
    }

    /// hugepages: 크기 풀이 없는 노드는 빼고, 보통 메모리(`free_memory_mib`)는 풀을 빼고 hugepages VM을 세지 않는다.
    #[test]
    fn hugepages_need_a_pool_and_leave_small_memory_alone() {
        let n1 = numa_node("n1", "");
        let mut big = spec(1024, None, PowerState::Stopped);
        big.hugepages = Some(numa::PageSize::Size1G);
        let inv = Inventory {
            nodes: vec![n1.clone()],
            ..Inventory::default()
        };
        assert_eq!(
            node_fits(&big, &inv, &n1),
            Err(r#"node "n1" has no 1G hugepages"#.into())
        );
        let mut small = spec(1024, Some(n1.id), PowerState::Running);
        small.hugepages = Some(numa::PageSize::Size2M);
        let mut inv = inv;
        assert_eq!(small_memory_mib(&n1), 16384 - 1024);
        assert_eq!(inv.free_memory_mib(&n1, Uuid::nil()), 16384 - 1024);
        inv.vms.push(Resource::new("hp", small.clone()));
        inv.vms.push(Resource::new(
            "plain",
            spec(2048, Some(n1.id), PowerState::Running),
        ));
        assert_eq!(inv.free_memory_mib(&n1, Uuid::nil()), 16384 - 1024 - 2048);
        // 풀(1 GiB)은 이미 다 잡혔다.
        let mut more = spec(2, None, PowerState::Stopped);
        more.hugepages = Some(numa::PageSize::Size2M);
        assert_eq!(
            node_fits(&more, &inv, &n1),
            Err(
                r#"node "n1" has 0 free 2M hugepages on host NUMA node 0; the VM needs 1 on one"#
                    .into()
            )
        );
        // 기록된 배치 없이 놓인 VM이 맞춰지지 않아도(이미 넘친 노드) 합계는 Raft 장부처럼 센다.
        let mut over = spec(2048, Some(n1.id), PowerState::Running);
        over.hugepages = Some(numa::PageSize::Size2M);
        inv.vms.retain(|v| v.name != "hp");
        inv.vms.push(Resource::new("over", over));
        assert_eq!(
            node_fits(&more, &inv, &n1),
            Err(r#"node "n1" would use 2050 of 1024 MiB of 2M hugepages"#.into())
        );
    }
}
