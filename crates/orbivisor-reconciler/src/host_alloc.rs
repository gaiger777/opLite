//! 노드 할당기(계획 64 §9의 셋째 층): VM을 켜거나 마이그레이션을 받을 때 이 노드의 정확한 pCPU·호스트 NUMA 노드·
//! hugepages를 잠금 안에서 고른다. 보는 것은 이 노드의 Raft 기록(도는 VM의 `hardware.host_placement`), 켜는 중·받는 중인
//! 예약, `/sys`의 실제 빈 hugepages(다른 프로세스가 쓴 것)다. 한 노드의 QEMU는 그 노드의 opld만 띄우므로 이 잠금 하나로
//! pCPU가 두 번 잡히지 않는다. 예약은 그 VM의 기록이 Raft에 같은 배치로 보이거나, VM이 이 노드를 떠났거나, 작업자·받는
//! 쪽이 풀 때([`HostAllocator::release`]) 없어진다.
//!
//! 검사 루프의 [`repin`]은 도는 VM의 스레드 친화를 기록과 지금 세 집합에 맞춘다(누가 `taskset`을 썼거나 공유 집합이
//! 바뀌었을 때, §7·§10).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_driver::Hypervisor;
use orbivisor_types::node::NodeSpec;
use orbivisor_types::numa::{
    self, HostFree, HostPlacement, HostSets, HostUsage, HostView, NodeTopology, NumaRefusal,
    PageKind, ResolvedNuma,
};
use orbivisor_types::runtime::RuntimePinning;
use orbivisor_types::vm::{CpuSpec, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use crate::cpu::CpuRefusal;

/// 켤 VM의 모양
#[derive(Debug, Clone, Copy)]
pub enum Shape<'a> {
    /// 스펙에서 새로 정한다(켜기·HA 복구). `cpu`는 켤 때의 CPU(`hw.cpu` — 노드가 핫플러그를 못 하면 상한이 없다)다.
    Spec(&'a VmSpec, &'a CpuSpec),
    /// 기록한 모양 그대로 호스트 쪽만 고른다(마이그레이션 받기·메모리 스냅샷·다시 켜기). None은 vNUMA 없이 켠 기록이다.
    /// 셋째는 게스트 노드마다 꽂은 virtio-mem 메모리(MiB, [`numa::plugged_per_node`] — 받는 쪽이 지금 메모리만큼 잡는다,
    /// 리뷰 2 N2)이고 비면 없다.
    Fixed(Option<&'a ResolvedNuma>, &'a CpuSpec, &'a [u32]),
}

/// 고른 것: 기록할 모양·배치와 드라이버에 줄 고정
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claim {
    pub numa: Option<ResolvedNuma>,
    pub placement: Option<HostPlacement>,
    pub pinning: Option<RuntimePinning>,
}

/// 예약 하나(켜는 중이거나 받는 중인 VM의 배치). `numa`는 셈하는 모양(꽂은 메모리 포함)이다.
#[derive(Debug, Clone)]
struct Held {
    numa: ResolvedNuma,
    placement: HostPlacement,
}

/// opld마다 하나. 작업자들과 마이그레이션 받는 쪽이 `Arc`로 나눈다.
pub struct HostAllocator {
    node: Uuid,
    /// `/sys`의 뿌리. 있으면 실제 빈 hugepages(`free_hugepages`)로 빈 수를 줄인다(Linux).
    sys: Option<PathBuf>,
    held: Mutex<BTreeMap<Uuid, Held>>,
}

impl HostAllocator {
    pub fn new(node: Uuid, sys: Option<PathBuf>) -> Self {
        Self {
            node,
            sys,
            held: Mutex::default(),
        }
    }

    /// VM `vm`을 이 노드에서 켤 모양·배치·고정을 고르고, 묶는 VM이면 예약한다(같은 VM의 앞 예약은 바뀐다). 기다리면
    /// 풀리는 거절(켜는 중인 다른 VM의 예약 때문, 노드를 읽지 못함)은 `Waiting`, 아니면 `Cannot`(노드 이름을 담음)이다.
    pub fn claim(
        &self,
        cluster: &Cluster,
        vm: Uuid,
        shape: Shape<'_>,
    ) -> Result<Claim, CpuRefusal> {
        let me = cluster
            .get::<NodeSpec>(self.node)
            .map_err(|e| CpuRefusal::Waiting(format!("cannot read this node: {e}")))?;
        let name = me
            .as_ref()
            .map_or_else(|| self.node.to_string(), |n| n.name.clone());
        let resources = me.as_ref().and_then(|n| n.spec.host_resources.as_ref());
        let topology = me.as_ref().and_then(|n| n.status.topology.as_ref());
        let cannot = |r: NumaRefusal| CpuRefusal::Cannot(r.for_node(&name));
        let (layout, cpu) = match shape {
            Shape::Spec(spec, cpu) => {
                let mut s = spec.clone();
                s.cpu = cpu.clone();
                // 메모리 핫플러그 VM의 `any`는 `layout`이 2M으로 고른다(스케줄러·장부와 같게).
                let layout = numa::layout(&s, topology).map_err(|e| {
                    CpuRefusal::Cannot(format!("the VM's NUMA settings are invalid: {e}"))
                })?;
                (Lay::Spec(layout), cpu)
            }
            Shape::Fixed(numa, cpu, plugged) => (
                Lay::Fixed(numa, numa.map(|n| numa::with_extra(n, plugged))),
                cpu,
            ),
        };
        if !layout.binds() {
            let numa = match &layout {
                Lay::Spec(None) | Lay::Fixed(None, _) => None,
                Lay::Spec(Some(l)) => Some(numa::fit(l, None).map_err(cannot)?.numa),
                Lay::Fixed(Some(n), _) => Some((*n).clone()),
            };
            let pinning = topology.and_then(|t| {
                numa::pinning(numa.as_ref(), None, cpu, t, &HostSets::new(resources, t))
            });
            self.release(vm);
            return Ok(Claim {
                numa,
                placement: None,
                pinning,
            });
        }
        let Some(topology) = topology else {
            return Err(cannot(NumaRefusal::NoTopology));
        };
        let sets = HostSets::new(resources, topology);
        // 잠금을 먼저 잡고 VM을 읽는다: 읽은 뒤에 잠그면 그사이 기록이 Raft에 닿은 VM의 예약을 다른 작업자가 지우고
        // (`prune`), 이 읽기에는 그 기록도 예약도 없어 같은 pCPU를 두 번 준다(리뷰 1).
        let mut held = self.held.lock().unwrap();
        let (_, vms) = cluster
            .list::<VmSpec>()
            .map_err(|e| CpuRefusal::Waiting(format!("cannot read the VMs: {e}")))?;
        self.prune(&mut held, &vms);
        held.remove(&vm);
        let records = numa::usage(
            vms.iter()
                .filter(|v| v.id != vm && !held.contains_key(&v.id))
                .filter_map(|v| {
                    // 꽂은 virtio-mem 메모리까지 센다(리뷰 2 N2).
                    let hw = v.status.hardware.as_ref()?;
                    Some((numa::now(hw)?, hw.host_placement.as_ref()?))
                })
                .collect::<Vec<_>>()
                .iter()
                .map(|(n, p)| (n, *p)),
            self.node,
        );
        let starting = numa::usage(held.values().map(|h| (&h.numa, &h.placement)), self.node);
        let real = self.free_pages(topology);
        let attempt = |with_held: bool| {
            let mut usage = records.clone();
            if with_held {
                usage.add(&starting);
            }
            let mut free = HostFree::new(topology, &sets, &usage);
            clamp_pages(&mut free, &real, with_held.then_some(&starting));
            let view = HostView {
                node: self.node,
                topology,
                sets: &sets,
                free: &free,
            };
            layout.fit(cpu, &view)
        };
        match attempt(true) {
            Ok(fit) => {
                let placement = fit.placement.clone().expect("a bound fit has a placement");
                held.insert(
                    vm,
                    Held {
                        numa: layout.counted(&fit.numa),
                        placement: placement.clone(),
                    },
                );
                let pinning =
                    numa::pinning(Some(&fit.numa), Some(&placement), cpu, topology, &sets);
                Ok(Claim {
                    numa: Some(fit.numa),
                    placement: Some(placement),
                    pinning,
                })
            }
            // 켜는 중인 다른 VM의 예약이 없으면 들어간다: 그것이 켜지거나 실패하기를 기다린다.
            Err(refusal) if !held.is_empty() && attempt(false).is_ok() => Err(CpuRefusal::Waiting(
                format!("waiting for other VMs starting on this node (it {refusal})"),
            )),
            Err(refusal) => Err(cannot(refusal)),
        }
    }

    /// 받는 중인 VM의 배치를 예약해 둔다(opld가 다시 떴을 때 받는 QEMU의 저장된 기록에서).
    pub fn hold(&self, vm: Uuid, numa: ResolvedNuma, placement: HostPlacement) {
        if placement.node == self.node {
            self.held
                .lock()
                .unwrap()
                .insert(vm, Held { numa, placement });
        }
    }

    /// opld가 다시 떴을 때: 이 노드에서 도는 QEMU와 받는 중인 QEMU의 저장된 기록(`hardware.json`)의 배치를 예약으로
    /// 다시 잡는다. 기록이 Raft에 아직 닿지 않은 VM(켠 직후 opld가 죽음)의 pCPU·페이지를 다른 VM에 주지 않게 한다(리뷰
    /// 1). Raft 기록이 같은 배치면 다음 `claim`이 지운다.
    pub async fn hold_saved(&self, hv: &dyn Hypervisor) {
        let mut ids: Vec<Uuid> = hv
            .list()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.id)
            .collect();
        ids.extend(
            hv.list_incoming()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(vm, _)| vm),
        );
        for vm in ids {
            if let Some(hw) = hv.saved_hardware(vm)
                && let (Some(numa), Some(placement)) = (numa::now(&hw), hw.host_placement)
            {
                self.hold(vm, numa, placement);
            }
        }
    }

    /// VM의 예약을 푼다(켜지 못함·QEMU가 끝남·받기 취소).
    pub fn release(&self, vm: Uuid) {
        self.held.lock().unwrap().remove(&vm);
    }

    /// 예약이 있는 VM(시험·진단)
    pub fn held(&self) -> Vec<Uuid> {
        self.held.lock().unwrap().keys().copied().collect()
    }

    /// 넘겨준 예약을 지운다: VM이 없거나 이 노드를 떠났거나(배정도, 받는 마이그레이션도 아님) Raft 기록이 같은 배치다.
    fn prune(&self, held: &mut BTreeMap<Uuid, Held>, vms: &[Vm]) {
        held.retain(|id, h| {
            let Some(v) = vms.iter().find(|v| v.id == *id) else {
                return false;
            };
            let here = v.spec.node == Some(self.node)
                || v.status.migration.as_ref().map(|m| m.node) == Some(self.node);
            let recorded = v
                .status
                .hardware
                .as_ref()
                .and_then(|hw| hw.host_placement.as_ref())
                == Some(&h.placement);
            here && !recorded
        });
    }

    /// 호스트 노드·크기마다 `/sys`의 빈 hugepages. `/sys`를 보지 않거나 읽지 못한 것은 없다.
    fn free_pages(&self, topology: &NodeTopology) -> BTreeMap<(u16, PageKind), u64> {
        let Some(sys) = &self.sys else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        for n in &topology.nodes {
            for kind in [PageKind::Size2M, PageKind::Size1G] {
                if n.pages(kind) > 0
                    && let Some(free) = sys_free_pages(sys, n.id, kind)
                {
                    out.insert((n.id, kind), free);
                }
            }
        }
        out
    }
}

/// 게스트 모양: 스펙에서 뽑은 것이나 기록(켤 때 기록, 꽂은 메모리를 더한 지금 모양)
enum Lay<'a> {
    Spec(Option<numa::Layout>),
    Fixed(Option<&'a ResolvedNuma>, Option<ResolvedNuma>),
}

impl Lay<'_> {
    fn binds(&self) -> bool {
        match self {
            Lay::Spec(l) => l.as_ref().is_some_and(numa::Layout::binds),
            Lay::Fixed(n, _) => n.is_some_and(|n| n.memory.is_some()),
        }
    }

    /// 지금 모양으로 맞추고 기록할 모양은 켤 때 기록 그대로 둔다(명령줄의 `ram-node<i>`는 켤 때 나눔이다).
    fn fit(&self, cpu: &CpuSpec, view: &HostView<'_>) -> Result<numa::Fit, NumaRefusal> {
        match self {
            Lay::Spec(Some(l)) => numa::fit(l, Some(view)),
            Lay::Fixed(Some(boot), Some(now)) => {
                let mut fit = numa::fit_fixed(now, cpu, Some(view))?;
                fit.numa = (*boot).clone();
                Ok(fit)
            }
            // 묶지 않는 모양은 여기 오지 않는다.
            _ => Err(NumaRefusal::NoTopology),
        }
    }

    /// 예약에 셀 모양: 받는 VM은 꽂은 메모리까지
    fn counted(&self, fit: &ResolvedNuma) -> ResolvedNuma {
        match self {
            Lay::Fixed(_, Some(now)) => now.clone(),
            _ => fit.clone(),
        }
    }
}

/// 장부의 빈 페이지를 실제 빈 수(`real`)로 줄인다. 켜는 중인 예약(`starting`)은 아직 잡지 않았을 수 있으므로 실제 빈
/// 수에서도 뺀다.
fn clamp_pages(
    free: &mut HostFree,
    real: &BTreeMap<(u16, PageKind), u64>,
    starting: Option<&HostUsage>,
) {
    for (key, pages) in free.pages.iter_mut() {
        if let Some(r) = real.get(key) {
            let taken = starting
                .and_then(|s| s.pages.get(key))
                .copied()
                .unwrap_or(0);
            *pages = (*pages).min(r.saturating_sub(taken));
        }
    }
}

/// `/sys`의 빈 hugepages: NUMA 커널은 노드마다(`devices/system/node/node<N>/hugepages/…/free_hugepages`), 아니면 커널
/// 전체(`kernel/mm/hugepages/…`)
pub fn sys_free_pages(sys: &Path, node: u16, kind: PageKind) -> Option<u64> {
    let pool = format!("hugepages-{}kB/free_hugepages", kind.size_kib());
    let nodes = sys.join("devices/system/node");
    let path = if nodes.is_dir() {
        nodes.join(format!("node{node}/hugepages/{pool}"))
    } else {
        sys.join("kernel/mm/hugepages").join(pool)
    };
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// 검사 루프(§7·§10): 이 노드에서 도는 VM마다 스레드 고정을 기록과 지금 세 집합으로 셈해 드라이버로 맞춘다. 기록은
/// 실행 디렉터리의 것(`hardware.json` — 지금 도는 QEMU의 것)이다: 다시 켠 직후에는 Raft 기록이 아직 앞 QEMU의 배치라
/// 그것으로 걸면 다른 VM에 준 pCPU에 고정한다(리뷰 1). 실행 디렉터리 기록이 없으면(켜는 중) 건너뛴다. 걸 것이 없는
/// VM은 opld가 고정했던 것(`pinning.json`이 있음 — 설정을 지움)만 한 번 풀어(opld가 뜰 때의 친화, 모르면 공유 집합)
/// 고정 기록을 지우고, 고정한 적 없는 VM은 건드리지 않는다 — 관리자가 `taskset`·`CPUAffinity=`로 좁힌 VM을 넓히지
/// 않는다(리뷰 2 N3). 바꾼 VM 수를 돌려준다(경고 로그를 남긴다).
pub async fn repin(
    cluster: &Cluster,
    hv: &dyn Hypervisor,
    node: Uuid,
) -> Result<usize, ClusterError> {
    let Some(me) = cluster.get::<NodeSpec>(node)? else {
        return Ok(0);
    };
    let Some(topology) = &me.status.topology else {
        return Ok(0);
    };
    let sets = HostSets::new(me.spec.host_resources.as_ref(), topology);
    let (_, vms) = cluster.list::<VmSpec>()?;
    let mut fixed = 0;
    for vm in vms {
        if vm.status.node != Some(node) || vm.status.phase != VmPhase::Running {
            continue;
        }
        let Some(hw) = hv.saved_hardware(vm.id) else {
            continue;
        };
        let placement = hw.host_placement.as_ref().filter(|p| p.node == node);
        let (want, release) =
            match numa::pinning(hw.numa.as_ref(), placement, &hw.cpu, topology, &sets) {
                Some(want) => (want, false),
                None => {
                    let Some(old) = hv.saved_pinning(vm.id) else {
                        continue;
                    };
                    let wide = orbivisor_driver::affinity::start_cpus()
                        .map(|c| c.intersection(&topology.cpus))
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| sets.shared.clone());
                    let want = RuntimePinning {
                        emulator: wide.clone(),
                        vcpus: Vec::new(),
                        slots_per_socket: numa::slots_per_socket(&hw.cpu),
                        // 스코프를 걸었던 VM만 넓힌다.
                        scope: old.scope.map(|_| wide),
                    };
                    (want, true)
                }
            };
        let result = hv.repin(vm.id, &want).await;
        if release && result.is_ok() {
            hv.forget_pinning(vm.id);
        }
        match result {
            Ok(0) => {}
            Ok(n) => {
                tracing::warn!(vm = %vm.id, changed = n, emulator = %want.emulator, "re-pinned the threads of the VM (they drifted or the host CPU sets changed)");
                fixed += 1;
            }
            Err(e) => {
                tracing::warn!(vm = %vm.id, error = %e, "cannot check the CPU pinning of the VM")
            }
        }
    }
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_free_pages_come_from_the_node_or_the_kernel_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path();
        let pool = |dir: PathBuf, free: &str| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("free_hugepages"), free).unwrap();
        };
        pool(sys.join("kernel/mm/hugepages/hugepages-2048kB"), "7\n");
        assert_eq!(sys_free_pages(sys, 0, PageKind::Size2M), Some(7));
        assert_eq!(sys_free_pages(sys, 0, PageKind::Size1G), None);
        pool(
            sys.join("devices/system/node/node1/hugepages/hugepages-1048576kB"),
            "3\n",
        );
        assert_eq!(sys_free_pages(sys, 1, PageKind::Size1G), Some(3));
        // NUMA 커널이면 노드의 것만 본다.
        assert_eq!(sys_free_pages(sys, 0, PageKind::Size2M), None);
    }

    #[test]
    fn starting_reservations_count_against_the_real_free_pages() {
        let key = (0, PageKind::Size2M);
        let mut free = HostFree {
            dedicated: Default::default(),
            pages: [(key, 100)].into(),
            memory: Default::default(),
        };
        clamp_pages(&mut free, &[(key, 40)].into(), None);
        assert_eq!(free.pages[&key], 40);
        let starting = HostUsage {
            dedicated: Default::default(),
            pages: [(key, 30)].into(),
            memory: Default::default(),
        };
        clamp_pages(&mut free, &[(key, 40)].into(), Some(&starting));
        assert_eq!(free.pages[&key], 10);
    }
}
