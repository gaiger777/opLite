//! 노드별 용량(계획 75 설계 §3.3): CPU(스레드·예약·전용·vCPU 할당·오버커밋·사용률), 메모리(할당·사용·hugepages),
//! NUMA 노드별, 스토리지 풀(총·사용·프로비전). `GET /capacity`가 쓰고, 웹의 홈 용량 위젯(`CapacityWidget`의 `usage`·
//! `clusterCapacity`)과 같은 규칙이다: VM은 QEMU가 떠 있는 단계(시작·실행·정지 중·옮기는 중)에서 `status.node`에 센다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::node::Node;
use crate::numa::{self, CpuPolicy, CpuSet, HostSets, HostUsage, NodeTopology, PageKind};
use crate::placement::NodeLoadView;
use crate::storage::{
    StoragePool, Volume, is_shared, pool_listed_on, pool_state_on, pool_usable_on,
};
use crate::vm::{Vm, VmPhase};

/// VM이 자리를 잡은 노드: QEMU가 떠 있는 단계(시작·실행·정지 중·옮기는 중 — 원본 노드)의 `status.node`. 실패·멈춘
/// VM은 `status.node`가 남아도 세지 않는다(웹 `CapacityWidget.usage`와 같다).
pub fn placed_on(vm: &Vm) -> Option<Uuid> {
    matches!(
        vm.status.phase,
        VmPhase::Starting | VmPhase::Running | VmPhase::Stopping | VmPhase::Migrating
    )
    .then_some(vm.status.node)
    .flatten()
}

/// `GET /capacity`의 응답
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Capacity {
    /// 위트니스가 아닌 노드(이름 순, 오프라인 포함)
    pub nodes: Vec<NodeCapacity>,
    /// 공유 풀(이름 순)
    pub shared_pools: Vec<SharedPoolCapacity>,
    /// 온라인 노드의 합과 공유 풀
    pub totals: CapacityTotals,
}

/// 노드 하나의 용량
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeCapacity {
    pub id: Uuid,
    /// 노드 이름
    pub node: String,
    pub online: bool,
    pub cpu: CpuCapacity,
    pub memory: MemoryCapacity,
    /// 호스트 NUMA 노드마다(id 순). 토폴로지를 모르면(옛 노드·macOS) 빈다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub numa: Vec<NumaCapacity>,
    /// 이 노드의 로컬 풀(이름 순)
    pub storage: Vec<PoolCapacity>,
}

/// CPU
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CpuCapacity {
    /// 노드의 CPU 스레드(`NodeStatus.cpus`)
    pub threads: u32,
    /// 예약 CPU(호스트·Orbivisor, `host_resources.reserved_cpus`)
    pub reserved: u32,
    /// 전용 CPU(`host_resources.dedicated_cpus`)
    pub dedicated_total: u32,
    /// 그 가운데 도는 전용 VM이 잡은 것
    pub dedicated_used: u32,
    /// 자리 잡은 VM의 vCPU 합(시작·실행·정지 중·옮기는 중)
    pub vcpus_allocated: u32,
    /// 그 가운데 실행 중인 VM
    pub vcpus_running: u32,
    /// 그 가운데 공유 정책 VM(공유 스레드 위에서 돎)
    pub vcpus_shared: u32,
    /// 공유 vCPU ÷ 공유 스레드(`threads − reserved − dedicated_total`). 공유 스레드가 없으면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub overcommit: Option<f64>,
    /// 지난 1분 노드 CPU 사용률 %(배치 부하 요약, 없으면 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub use_pct: Option<f64>,
}

/// 메모리
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MemoryCapacity {
    /// 노드 메모리(`NodeStatus.memory_mib`)
    pub total_mib: u64,
    /// hugepages 풀로 떼어 둔 메모리(보통 VM은 쓰지 못함)
    pub reserved_mib: u64,
    /// 자리 잡은 VM에 할당한 메모리(`memory_mib` 합, hugepages VM 포함)
    pub allocated_mib: u64,
    /// 그 가운데 hugepages 풀에서 받는 VM의 몫(계획 75 리뷰 1)
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub allocated_hugepages_mib: u64,
    /// 실제 사용(배치 부하 요약, 없으면 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub used_mib: Option<u64>,
    /// 크기마다 hugepages(풀이 있거나 쓰는 크기만)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub hugepages: Vec<HugepageCapacity>,
    /// 보통 메모리의 오버커밋: hugepages 아닌 VM의 할당 ÷ (노드 메모리 − hugepages 풀). 보통 VM은 그 풀을 쓰지 못한다.
    /// 노드 메모리를 모르면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub overcommit: Option<f64>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// VM 메모리가 hugepages에서 오는지: 켤 때 정한 NUMA 모양의 페이지, 기록이 없으면 스펙
fn on_hugepages(vm: &Vm) -> bool {
    match vm.status.hardware.as_ref().and_then(numa::now) {
        Some(n) => n.page.is_some(),
        None => vm.spec.hugepages.is_some(),
    }
}

/// 크기 하나의 hugepages(페이지 수)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HugepageCapacity {
    pub size_kib: u32,
    /// 풀의 페이지 수(노드가 알린 예약)
    pub total: u64,
    /// Orbivisor VM이 잡은 페이지 수(도는 VM의 기록)
    pub used: u64,
}

/// 호스트 NUMA 노드 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NumaCapacity {
    pub id: u16,
    /// 이 NUMA 노드의 온라인 CPU
    pub cpus: CpuSet,
    /// 그 가운데 예약
    pub reserved: CpuSet,
    /// 그 가운데 전용
    pub dedicated: CpuSet,
    /// 전용 가운데 잡힌 것
    pub dedicated_used: CpuSet,
    /// 노드 메모리 전체(hugepages 풀 포함)
    pub memory_mib: u64,
    /// hugepages 없이 이 노드에 묶인 보통 메모리(MiB)
    pub bound_mib: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub hugepages: Vec<HugepageCapacity>,
    /// 이 노드의 CPU에 고정된 vCPU 수
    pub pinned_vcpus: u32,
}

/// 노드 로컬 풀 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PoolCapacity {
    pub pool: String,
    /// 이 노드에서 쓸 수 있다.
    pub ready: bool,
    pub total_bytes: u64,
    /// `total − available`
    pub used_bytes: u64,
    /// 이 노드에 있는 그 풀 볼륨의 `size_bytes` 합(씬 할당)
    pub provisioned_bytes: u64,
    pub volumes: u32,
}

/// 공유 풀 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SharedPoolCapacity {
    pub pool: String,
    /// 풀 상태의 값(없으면 준비된 노드가 알린 값, 그것도 없으면 0)
    pub total_bytes: u64,
    pub used_bytes: u64,
    /// 그 풀 볼륨의 `size_bytes` 합
    pub provisioned_bytes: u64,
    pub volumes: u32,
    /// 이 풀을 쓸 수 있는 노드 수
    pub nodes_ready: u32,
    /// 이 풀을 허용하는 노드 수(위트니스 빼고)
    pub nodes: u32,
}

/// 합(온라인 노드와 공유 풀)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CapacityTotals {
    /// 합에 든 온라인 노드 수
    pub nodes: u32,
    /// 노드 값의 합(오버커밋은 합으로 다시 나눔, 사용률은 스레드 수로 가중 평균)
    pub cpu: CpuCapacity,
    pub memory: MemoryCapacity,
    /// 온라인 노드의 로컬 풀과 공유 풀의 합
    pub storage: StorageTotals,
}

/// 스토리지 합
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StorageTotals {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub provisioned_bytes: u64,
    pub volumes: u32,
}

fn ratio(a: u64, b: u64) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

/// 보통 메모리의 오버커밋(노드 메모리를 모르면 없음, 풀이 노드 메모리를 다 가져가면 보통 VM이 있을 때 무한이 아니라
/// 할당 그대로 ÷ 1MiB로 크게 보인다)
fn normal_overcommit(allocated: u64, on_pages: u64, total: u64, reserved: u64) -> Option<f64> {
    (total > 0).then(|| {
        allocated.saturating_sub(on_pages) as f64 / total.saturating_sub(reserved).max(1) as f64
    })
}

/// 크기 순(2M, 1G) hugepages 목록: 풀이 있거나 쓰는 크기만
fn hugepages(
    total: impl Fn(PageKind) -> u64,
    used: impl Fn(PageKind) -> u64,
) -> Vec<HugepageCapacity> {
    [PageKind::Size2M, PageKind::Size1G]
        .into_iter()
        .filter(|k| total(*k) > 0 || used(*k) > 0)
        .map(|k| HugepageCapacity {
            size_kib: k.size_kib(),
            total: total(k),
            used: used(k),
        })
        .collect()
}

/// 이 노드에 자리 잡은 VM의 기록(켤 때 정한 NUMA 모양 + 이 노드의 배치)으로 잡힌 전용 CPU·페이지·묶인 메모리와
/// 고정된 pCPU 목록
fn recorded_usage(vms: &[&Vm], node: Uuid) -> (HostUsage, Vec<u16>) {
    let mut records = Vec::new();
    let mut pinned = Vec::new();
    for vm in vms {
        let Some(hw) = vm.status.hardware.as_ref() else {
            continue;
        };
        let (Some(n), Some(p)) = (numa::now(hw), hw.host_placement.as_ref()) else {
            continue;
        };
        if p.node == node {
            pinned.extend(p.vcpus.iter().copied());
            records.push((n, p));
        }
    }
    let usage = numa::usage(records.iter().map(|(n, p)| (n, *p)), node);
    (usage, pinned)
}

fn node_capacity(
    node: &Node,
    here: &[&Vm],
    volumes: &[Volume],
    pools: &[&StoragePool],
    load: Option<&NodeLoadView>,
) -> NodeCapacity {
    let threads = node.status.cpus;
    let topology: Option<&NodeTopology> = node.status.topology.as_ref();
    let resources = node.spec.host_resources.as_ref();
    let sets = topology.map(|t| HostSets::new(resources, t));
    let (reserved, dedicated) = match (&sets, resources) {
        (Some(s), _) => (s.reserved.clone(), s.dedicated.clone()),
        (None, Some(r)) => (
            r.reserved_cpus.clone(),
            r.dedicated_cpus.difference(&r.reserved_cpus),
        ),
        (None, None) => (CpuSet::new(), CpuSet::new()),
    };
    let (usage, pinned) = recorded_usage(here, node.id);

    let vcpus = |f: &dyn Fn(&Vm) -> bool| -> u32 {
        here.iter()
            .filter(|v| f(v))
            .map(|v| v.spec.cpu.vcpus())
            .sum()
    };
    let vcpus_shared = vcpus(&|v| v.spec.cpu.policy == CpuPolicy::Shared);
    let shared_threads = u64::from(threads)
        .saturating_sub(reserved.len() as u64)
        .saturating_sub(dedicated.len() as u64);
    let cpu = CpuCapacity {
        threads,
        reserved: reserved.len() as u32,
        dedicated_total: dedicated.len() as u32,
        dedicated_used: usage.dedicated.intersection(&dedicated).len() as u32,
        vcpus_allocated: vcpus(&|_| true),
        vcpus_running: vcpus(&|v| v.status.phase == VmPhase::Running),
        vcpus_shared,
        overcommit: ratio(u64::from(vcpus_shared), shared_threads),
        use_pct: load.and_then(|l| l.cpu1_pct).map(|p| f64::from(p) / 10.0),
    };

    let mut used_pages: BTreeMap<PageKind, u64> = BTreeMap::new();
    for ((_, k), n) in &usage.pages {
        *used_pages.entry(*k).or_default() += n;
    }
    let total_pages = |k: PageKind| topology.map_or(0, |t| t.pages(k));
    let reserved_mib = [PageKind::Size2M, PageKind::Size1G]
        .into_iter()
        .map(|k| total_pages(k) * u64::from(k.size_mib()))
        .sum();
    let allocated_mib: u64 = here.iter().map(|v| u64::from(v.spec.memory_mib)).sum();
    let allocated_hugepages_mib: u64 = here
        .iter()
        .filter(|v| on_hugepages(v))
        .map(|v| u64::from(v.spec.memory_mib))
        .sum();
    let memory = MemoryCapacity {
        total_mib: node.status.memory_mib,
        reserved_mib,
        allocated_mib,
        allocated_hugepages_mib,
        used_mib: load.and_then(|l| l.mem_used_mib),
        hugepages: hugepages(total_pages, |k| used_pages.get(&k).copied().unwrap_or(0)),
        overcommit: normal_overcommit(
            allocated_mib,
            allocated_hugepages_mib,
            node.status.memory_mib,
            reserved_mib,
        ),
    };

    let numa = match (topology, &sets) {
        (Some(t), Some(s)) => t
            .nodes
            .iter()
            .map(|h| {
                let dedicated = s.dedicated.intersection(&h.cpus);
                NumaCapacity {
                    id: h.id,
                    cpus: h.cpus.clone(),
                    reserved: s.reserved.intersection(&h.cpus),
                    dedicated_used: usage.dedicated.intersection(&dedicated),
                    dedicated,
                    memory_mib: h.memory_mib,
                    bound_mib: usage.memory.get(&h.id).copied().unwrap_or(0),
                    hugepages: hugepages(
                        |k| h.pages(k),
                        |k| usage.pages.get(&(h.id, k)).copied().unwrap_or(0),
                    ),
                    pinned_vcpus: pinned.iter().filter(|c| h.cpus.contains(**c)).count() as u32,
                }
            })
            .collect(),
        _ => Vec::new(),
    };

    let storage = pools
        .iter()
        .filter(|p| !is_shared(p) && pool_listed_on(p, node.id))
        .map(|p| {
            let state = pool_state_on(p, node);
            let (total, available) = state.map_or((0, 0), |s| (s.total_bytes, s.available_bytes));
            let vols: Vec<&Volume> = volumes
                .iter()
                .filter(|v| v.spec.pool == p.name && v.status.node == Some(node.id))
                .collect();
            PoolCapacity {
                pool: p.name.clone(),
                ready: pool_usable_on(p, node),
                total_bytes: total,
                used_bytes: total.saturating_sub(available),
                provisioned_bytes: vols.iter().map(|v| v.spec.size_bytes).sum(),
                volumes: vols.len() as u32,
            }
        })
        .collect();

    NodeCapacity {
        id: node.id,
        node: node.name.clone(),
        online: node.status.online,
        cpu,
        memory,
        numa,
        storage,
    }
}

/// 클러스터 용량(설계 §3.3). `loads`는 `GET /placement`의 노드 부하(없는 노드는 사용률·사용 메모리가 빈다).
pub fn build(
    nodes: &[Node],
    vms: &[Vm],
    volumes: &[Volume],
    pools: &[StoragePool],
    loads: &[NodeLoadView],
) -> Capacity {
    let mut hosts: Vec<&Node> = nodes.iter().filter(|n| !n.spec.witness).collect();
    hosts.sort_by(|a, b| a.name.cmp(&b.name));
    let mut pools: Vec<&StoragePool> = pools.iter().collect();
    pools.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = Capacity::default();
    for node in &hosts {
        let here: Vec<&Vm> = vms
            .iter()
            .filter(|v| placed_on(v) == Some(node.id))
            .collect();
        let load = loads.iter().find(|l| l.node == node.id);
        out.nodes
            .push(node_capacity(node, &here, volumes, &pools, load));
    }

    out.shared_pools = pools
        .iter()
        .filter(|p| is_shared(p))
        .map(|p| {
            let listed: Vec<&&Node> = hosts.iter().filter(|n| pool_listed_on(p, n.id)).collect();
            let (total, available) = match (p.status.total_bytes, p.status.available_bytes) {
                (Some(t), Some(a)) => (t, a),
                _ => listed
                    .iter()
                    .filter_map(|n| pool_state_on(p, n).filter(|s| s.ready))
                    .map(|s| (s.total_bytes, s.available_bytes))
                    .next()
                    .unwrap_or((0, 0)),
            };
            let vols: Vec<&Volume> = volumes.iter().filter(|v| v.spec.pool == p.name).collect();
            SharedPoolCapacity {
                pool: p.name.clone(),
                total_bytes: total,
                used_bytes: total.saturating_sub(available),
                provisioned_bytes: vols.iter().map(|v| v.spec.size_bytes).sum(),
                volumes: vols.len() as u32,
                nodes_ready: listed.iter().filter(|n| pool_usable_on(p, n)).count() as u32,
                nodes: listed.len() as u32,
            }
        })
        .collect();

    out.totals = totals(&out.nodes, &out.shared_pools);
    out
}

/// 온라인 노드의 합(웹 `clusterCapacity`와 같이 위트니스·오프라인 노드는 뺀다)과 공유 풀
fn totals(nodes: &[NodeCapacity], shared: &[SharedPoolCapacity]) -> CapacityTotals {
    let online: Vec<&NodeCapacity> = nodes.iter().filter(|n| n.online).collect();
    let sum = |f: &dyn Fn(&NodeCapacity) -> u32| -> u32 { online.iter().map(|n| f(n)).sum() };
    let mut cpu = CpuCapacity {
        threads: sum(&|n| n.cpu.threads),
        reserved: sum(&|n| n.cpu.reserved),
        dedicated_total: sum(&|n| n.cpu.dedicated_total),
        dedicated_used: sum(&|n| n.cpu.dedicated_used),
        vcpus_allocated: sum(&|n| n.cpu.vcpus_allocated),
        vcpus_running: sum(&|n| n.cpu.vcpus_running),
        vcpus_shared: sum(&|n| n.cpu.vcpus_shared),
        overcommit: None,
        use_pct: None,
    };
    let shared_threads: u64 = online
        .iter()
        .map(|n| {
            u64::from(n.cpu.threads)
                .saturating_sub(u64::from(n.cpu.reserved))
                .saturating_sub(u64::from(n.cpu.dedicated_total))
        })
        .sum();
    cpu.overcommit = ratio(u64::from(cpu.vcpus_shared), shared_threads);
    let known: Vec<(f64, f64)> = online
        .iter()
        .filter_map(|n| n.cpu.use_pct.map(|p| (p, f64::from(n.cpu.threads))))
        .collect();
    let weight: f64 = known.iter().map(|(_, w)| w).sum();
    cpu.use_pct = (weight > 0.0).then(|| known.iter().map(|(p, w)| p * w).sum::<f64>() / weight);

    let mut pages: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    for h in online.iter().flat_map(|n| &n.memory.hugepages) {
        let e = pages.entry(h.size_kib).or_default();
        e.0 += h.total;
        e.1 += h.used;
    }
    let total_mib = online.iter().map(|n| n.memory.total_mib).sum();
    let allocated_mib = online.iter().map(|n| n.memory.allocated_mib).sum();
    let allocated_hugepages_mib = online
        .iter()
        .map(|n| n.memory.allocated_hugepages_mib)
        .sum();
    let reserved_mib = online.iter().map(|n| n.memory.reserved_mib).sum();
    let used: Vec<u64> = online.iter().filter_map(|n| n.memory.used_mib).collect();
    let memory = MemoryCapacity {
        total_mib,
        reserved_mib,
        allocated_mib,
        allocated_hugepages_mib,
        used_mib: (!used.is_empty()).then(|| used.iter().sum()),
        hugepages: pages
            .into_iter()
            .map(|(size_kib, (total, used))| HugepageCapacity {
                size_kib,
                total,
                used,
            })
            .collect(),
        overcommit: normal_overcommit(
            allocated_mib,
            allocated_hugepages_mib,
            total_mib,
            reserved_mib,
        ),
    };

    let mut storage = StorageTotals::default();
    for p in online.iter().flat_map(|n| &n.storage) {
        storage.total_bytes += p.total_bytes;
        storage.used_bytes += p.used_bytes;
        storage.provisioned_bytes += p.provisioned_bytes;
        storage.volumes += p.volumes;
    }
    for p in shared {
        storage.total_bytes += p.total_bytes;
        storage.used_bytes += p.used_bytes;
        storage.provisioned_bytes += p.provisioned_bytes;
        storage.volumes += p.volumes;
    }
    CapacityTotals {
        nodes: online.len() as u32,
        cpu,
        memory,
        storage,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::node::{NodeSpec, PoolState};
    use crate::resource::Resource;
    use crate::storage::{StoragePoolSpec, VolumeSpec};
    use crate::vm::VmSpec;

    const GIB: u64 = 1 << 30;

    fn node(name: &str, cpus: u32, memory_mib: u64, online: bool) -> Node {
        let mut n: Node = Resource::new(
            name,
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
                host_resources: None,
                sdn: None,
            },
        );
        n.status.online = online;
        n.status.cpus = cpus;
        n.status.memory_mib = memory_mib;
        n
    }

    fn vm(
        name: &str,
        cpu: serde_json::Value,
        memory_mib: u32,
        on: Option<&Node>,
        phase: VmPhase,
    ) -> Vm {
        let spec: VmSpec = serde_json::from_value(
            json!({ "cpu": cpu, "memory_mib": memory_mib, "machine": "q35" }),
        )
        .unwrap();
        let mut v: Vm = Resource::new(name, spec);
        v.status.node = on.map(|n| n.id);
        v.status.phase = phase;
        v
    }

    fn load(n: &Node, cpu1_pct: Option<u16>, used: Option<u64>) -> NodeLoadView {
        NodeLoadView {
            node: n.id,
            name: n.name.clone(),
            cpu1_pct,
            cpu5_pct: None,
            mem_used_mib: used,
            mem_allocated_mib: 0,
            mem_total_mib: n.status.memory_mib,
            cpu_use_pct: 0,
            mem_use_pct: 0,
            vms: 0,
            age_secs: None,
        }
    }

    /// 웹 `CapacityWidget`의 `usage`·`clusterCapacity`와 같은 값: QEMU가 떠 있는 단계만 `status.node`에 세고, 합은 온라인
    /// 노드(위트니스 빼고)만이다.
    #[test]
    fn matches_the_web_capacity_widget() {
        let a = node("a", 8, 16384, true);
        let b = node("b", 4, 8192, true);
        let c = node("c", 16, 32768, false);
        let mut w = node("w", 2, 1024, true);
        w.spec.witness = true;
        let two = json!({ "cores": 2 });
        let vms = [
            vm("run", two.clone(), 2048, Some(&a), VmPhase::Running),
            vm(
                "stopping",
                json!({ "sockets": 2, "cores": 2 }),
                1024,
                Some(&a),
                VmPhase::Stopping,
            ),
            vm("starting", two.clone(), 512, Some(&a), VmPhase::Starting),
            vm(
                "migrating",
                json!({ "cores": 3 }),
                4096,
                Some(&b),
                VmPhase::Migrating,
            ),
            vm("stopped", two.clone(), 8192, Some(&a), VmPhase::Stopped),
            vm("failed", two.clone(), 8192, Some(&a), VmPhase::Failed),
            vm("nowhere", two.clone(), 8192, None, VmPhase::Running),
            vm(
                "offline",
                json!({ "cores": 4 }),
                1024,
                Some(&c),
                VmPhase::Running,
            ),
        ];
        let cap = build(
            &[c.clone(), a.clone(), w, b.clone()],
            &vms,
            &[],
            &[],
            &[load(&a, Some(250), Some(6000))],
        );
        assert_eq!(
            cap.nodes
                .iter()
                .map(|n| n.node.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        let na = &cap.nodes[0];
        // usage(a): run + stopping + starting
        assert_eq!(
            na.cpu,
            CpuCapacity {
                threads: 8,
                reserved: 0,
                dedicated_total: 0,
                dedicated_used: 0,
                vcpus_allocated: 2 + 4 + 2,
                vcpus_running: 2,
                vcpus_shared: 8,
                overcommit: Some(1.0),
                use_pct: Some(25.0),
            }
        );
        assert_eq!(na.memory.allocated_mib, 2048 + 1024 + 512);
        assert_eq!(na.memory.used_mib, Some(6000));
        assert_eq!(na.memory.overcommit, Some(3584.0 / 16384.0));
        assert!(na.numa.is_empty());
        assert_eq!(cap.nodes[1].cpu.vcpus_allocated, 3);
        assert_eq!(cap.nodes[1].cpu.use_pct, None);
        assert_eq!(
            (cap.nodes[2].online, cap.nodes[2].cpu.vcpus_allocated),
            (false, 4)
        );
        // clusterCapacity: cpus 8 + 4, vcpus 8 + 3, memoryMib 16384 + 8192, usedMib 3584 + 4096
        let t = &cap.totals;
        assert_eq!(t.nodes, 2);
        assert_eq!((t.cpu.threads, t.cpu.vcpus_allocated), (12, 11));
        assert_eq!((t.memory.total_mib, t.memory.allocated_mib), (24576, 7680));
        assert_eq!(t.cpu.overcommit, Some(11.0 / 12.0));
        // 사용률은 아는 노드만 스레드로 가중
        assert_eq!(t.cpu.use_pct, Some(25.0));
        assert_eq!(t.memory.used_mib, Some(6000));
    }

    /// 예약·전용 CPU와 hugepages: 오버커밋은 공유 vCPU ÷ 공유 스레드, 전용·페이지 사용은 도는 VM의 기록, NUMA 노드마다
    /// 나눈다.
    #[test]
    fn reserved_dedicated_and_hugepages() {
        let mut n = node("a", 8, 16384, true);
        n.spec.host_resources = Some(
            serde_json::from_value(json!({
                "reserved_cpus": "0", "dedicated_cpus": "4-7",
                "hugepages": [{ "size": "2M", "count": 512, "host_node": 1 }]
            }))
            .unwrap(),
        );
        n.status.topology = Some(
            serde_json::from_value(json!({
                "sockets": 1, "threads_per_core": 1, "cpus": "0-7",
                "nodes": [
                    { "id": 0, "cpus": "0-3", "memory_mib": 8192 },
                    { "id": 1, "cpus": "4-7", "memory_mib": 8192, "hugepages": [{ "size_kib": 2048, "total": 512 }] }
                ],
                "pinning": true, "cpuset": true, "probed_at": 1
            }))
            .unwrap(),
        );
        let mut pinned = vm(
            "db",
            json!({ "cores": 2, "policy": "dedicated" }),
            512,
            Some(&n),
            VmPhase::Running,
        );
        pinned.status.hardware = Some(
            serde_json::from_value(json!({
                "cpu": { "cores": 2, "policy": "dedicated" }, "memory_mib": 512, "firmware": "uefi", "machine": "q35",
                "numa": { "nodes": [{ "sockets": [0], "memory_mib": 512 }], "page": "2M", "policy": "dedicated" },
                "host_placement": { "node": n.id, "vcpus": [4, 5], "host_nodes": [1] }
            }))
            .unwrap(),
        );
        let shared = vm(
            "web",
            json!({ "cores": 6 }),
            2048,
            Some(&n),
            VmPhase::Running,
        );
        let cap = build(std::slice::from_ref(&n), &[pinned, shared], &[], &[], &[]);
        let c = &cap.nodes[0];
        assert_eq!(
            (c.cpu.reserved, c.cpu.dedicated_total, c.cpu.dedicated_used),
            (1, 4, 2)
        );
        assert_eq!((c.cpu.vcpus_allocated, c.cpu.vcpus_shared), (8, 6));
        // 공유 스레드 8 − 1 − 4 = 3에 공유 vCPU 6
        assert_eq!(c.cpu.overcommit, Some(2.0));
        assert_eq!(c.memory.reserved_mib, 1024);
        // 보통 메모리 오버커밋은 hugepages VM(db 512)과 풀(1024)을 뺀다: 2048 ÷ (16384 − 1024)
        assert_eq!(c.memory.allocated_hugepages_mib, 512);
        assert_eq!(c.memory.overcommit, Some(2048.0 / 15360.0));
        assert_eq!(cap.totals.memory.overcommit, c.memory.overcommit);
        assert_eq!(
            c.memory.hugepages,
            [HugepageCapacity {
                size_kib: 2048,
                total: 512,
                used: 256,
            }]
        );
        assert_eq!(c.numa.len(), 2);
        assert_eq!(c.numa[0].reserved.to_string(), "0");
        assert!(c.numa[0].dedicated.is_empty() && c.numa[0].hugepages.is_empty());
        assert_eq!(c.numa[1].dedicated.to_string(), "4-7");
        assert_eq!(c.numa[1].dedicated_used.to_string(), "4-5");
        assert_eq!((c.numa[1].pinned_vcpus, c.numa[0].pinned_vcpus), (2, 0));
        assert_eq!(c.numa[1].hugepages[0].used, 256);
        assert_eq!(cap.totals.cpu.overcommit, Some(2.0));
        assert_eq!(cap.totals.memory.hugepages, c.memory.hugepages);
        // JSON: NUMA가 없으면 칸을 쓰지 않는다.
        let json =
            serde_json::to_value(build(&[node("b", 2, 1024, true)], &[], &[], &[], &[])).unwrap();
        assert!(json["nodes"][0].get("numa").is_none(), "{json}");
    }

    /// hugepages를 크게 떼어 둔 노드(64G 가운데 32G)에 보통 VM 40G: 보통 메모리는 넘쳤다(×1.25, ×0.63이 아님).
    #[test]
    fn memory_overcommit_leaves_out_the_hugepage_pool() {
        let mut n = node("a", 8, 65536, true);
        n.status.topology = Some(
            serde_json::from_value(json!({
                "sockets": 1, "threads_per_core": 1, "cpus": "0-7",
                "nodes": [{ "id": 0, "cpus": "0-7", "memory_mib": 65536,
                            "hugepages": [{ "size_kib": 1048576, "total": 32 }] }],
                "pinning": true, "cpuset": true, "probed_at": 1
            }))
            .unwrap(),
        );
        let v = vm(
            "big",
            json!({ "cores": 2 }),
            40960,
            Some(&n),
            VmPhase::Running,
        );
        let cap = build(std::slice::from_ref(&n), &[v], &[], &[], &[]);
        let m = &cap.nodes[0].memory;
        assert_eq!((m.reserved_mib, m.allocated_hugepages_mib), (32768, 0));
        assert_eq!(m.overcommit, Some(1.25));
        let json = serde_json::to_value(m).unwrap();
        assert!(json.get("allocated_hugepages_mib").is_none(), "{json}");
    }

    fn pool(name: &str, backend: serde_json::Value) -> StoragePool {
        let spec: StoragePoolSpec = serde_json::from_value(json!({ "backend": backend })).unwrap();
        Resource::new(name, spec)
    }

    fn volume(pool: &str, size: u64, on: Option<&Node>) -> Volume {
        let spec: VolumeSpec =
            serde_json::from_value(json!({ "pool": pool, "size_bytes": size, "format": "qcow2" }))
                .unwrap();
        let mut v: Volume = Resource::new("v", spec);
        v.status.node = on.map(|n| n.id);
        v
    }

    /// 씬 프로비전: 로컬 풀은 노드마다(볼륨이 있는 노드), 공유 풀은 한 번, 사용 = 총 − 남음. 합은 둘 다 더한다.
    #[test]
    fn thin_provisioning_local_and_shared_pools() {
        let mut a = node("a", 4, 4096, true);
        let mut b = node("b", 4, 4096, true);
        let local = pool("local", json!({ "kind": "dir", "path": "/srv/a" }));
        let nas = pool(
            "nas",
            json!({ "kind": "nfs", "server": "n", "export": "/x" }),
        );
        let state = |p: &StoragePool, ready: bool, total: u64, available: u64| PoolState {
            pool_id: p.id,
            ready,
            error: None,
            total_bytes: total,
            available_bytes: available,
        };
        a.status
            .pools
            .insert("local".into(), state(&local, true, 100 * GIB, 40 * GIB));
        b.status
            .pools
            .insert("local".into(), state(&local, true, 50 * GIB, 50 * GIB));
        a.status
            .pools
            .insert("nas".into(), state(&nas, true, 1000 * GIB, 900 * GIB));
        b.status
            .pools
            .insert("nas".into(), state(&nas, false, 0, 0));
        let volumes = [
            volume("local", 50 * GIB, Some(&a)),
            volume("local", 50 * GIB, Some(&a)),
            volume("local", 50 * GIB, Some(&a)),
            volume("local", 10 * GIB, Some(&b)),
            volume("nas", 300 * GIB, Some(&b)),
            volume("nas", 200 * GIB, None),
        ];
        let cap = build(&[a, b], &[], &volumes, &[nas, local], &[]);
        assert_eq!(
            cap.nodes[0].storage,
            [PoolCapacity {
                pool: "local".into(),
                ready: true,
                total_bytes: 100 * GIB,
                used_bytes: 60 * GIB,
                provisioned_bytes: 150 * GIB,
                volumes: 3,
            }]
        );
        assert_eq!(cap.nodes[1].storage[0].provisioned_bytes, 10 * GIB);
        assert_eq!(
            cap.shared_pools,
            [SharedPoolCapacity {
                pool: "nas".into(),
                total_bytes: 1000 * GIB,
                used_bytes: 100 * GIB,
                provisioned_bytes: 500 * GIB,
                volumes: 2,
                nodes_ready: 1,
                nodes: 2,
            }]
        );
        assert_eq!(
            cap.totals.storage,
            StorageTotals {
                total_bytes: 1150 * GIB,
                used_bytes: 160 * GIB,
                provisioned_bytes: 660 * GIB,
                volumes: 6,
            }
        );
    }

    /// 공유 풀 상태가 있으면 그것을 쓴다(노드 보고보다 먼저).
    #[test]
    fn shared_pool_prefers_the_pool_status() {
        let mut nas = pool(
            "nas",
            json!({ "kind": "nfs", "server": "n", "export": "/x" }),
        );
        nas.status.total_bytes = Some(10 * GIB);
        nas.status.available_bytes = Some(4 * GIB);
        let cap = build(&[node("a", 1, 1, true)], &[], &[], &[nas], &[]);
        assert_eq!(
            (
                cap.shared_pools[0].total_bytes,
                cap.shared_pools[0].used_bytes
            ),
            (10 * GIB, 6 * GIB)
        );
        assert_eq!(cap.shared_pools[0].nodes_ready, 0);
    }
}
