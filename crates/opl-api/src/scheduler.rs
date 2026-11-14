//! `spec.node`가 비어 있는 VM을 시작할 때 노드를 고른다(설계 3.3).

use std::collections::BTreeSet;

use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{Network, NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec, ShutdownAction, ha_capable};
use opl_types::placement::{HaGroup, HaGroupSpec, PlacementRule, PlacementRuleSpec};
use opl_types::runtime::ImageFormat;
use opl_types::settings::{HaReserve, SettingsSpec};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumeSpec, is_shared, pool_usable_on,
};
use opl_types::vm::{DiskMove, HaState, PowerState, Vm, VmSpec};
use uuid::Uuid;

use crate::placement::{self, Assign};
use crate::score::Scorer;
use crate::volumes::block_pool_kind;

/// [`schedule`]가 노드를 고르지 못한 이유
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleError {
    /// 노드 조건([`node_fits`])에 맞는 노드가 없다.
    #[error(
        "no node can run this VM (needs an online node that is not a witness or in maintenance, has the architecture of its machine type, can reach its storage pools, has the bridges of its networks, has a firewall (nftables) for VMs with security groups and, for an HA VM, has HA fencing)"
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
}

impl Inventory {
    pub fn load(cluster: &Cluster) -> Result<Self, ClusterError> {
        Ok(Self {
            nodes: cluster.list::<NodeSpec>()?.1,
            vms: cluster.list::<VmSpec>()?.1,
            volumes: cluster.list::<VolumeSpec>()?.1,
            pools: cluster.list::<StoragePoolSpec>()?.1,
            networks: cluster.list::<NetworkSpec>()?.1,
            rules: cluster.list::<PlacementRuleSpec>()?.1,
            groups: cluster.list::<HaGroupSpec>()?.1,
        })
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
    /// 노드(볼륨을 만든 노드)에서만 쓸 수 있다. `moves`의 볼륨(옮길 디스크)은 보지 않는다.
    fn reaches_disks(&self, spec: &VmSpec, node: &Node, moves: &[DiskMove]) -> bool {
        spec.disks.iter().all(|disk| {
            let Some(volume) = self.volumes.iter().find(|v| v.name == disk.volume) else {
                return true;
            };
            if moves.iter().any(|m| m.volume == volume.id) {
                return true;
            }
            let Some(pool) = self.pools.iter().find(|p| p.name == volume.spec.pool) else {
                return true;
            };
            pool_usable_on(pool, node)
                && (is_shared(pool) || volume.status.node.is_none_or(|n| n == node.id))
        })
    }

    /// 실행하기로 된 다른 VM에 배정되지 않은 메모리
    pub fn free_memory_mib(&self, node: &Node, vm: Uuid) -> i64 {
        let used: i64 = self
            .vms
            .iter()
            .filter(|v| v.id != vm && v.spec.node == Some(node.id))
            .filter(|v| v.spec.power == PowerState::Running)
            .map(|v| i64::from(v.spec.memory_mib))
            .sum();
        node.status.memory_mib as i64 - used
    }
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
        .filter(|n| node_fits(&vm.spec, inv, n).is_ok())
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

/// [`schedule`]가 이 노드를 후보로 보는지. 아니면 그 이유다. 메모리는 보지 않는다.
pub fn node_fits(spec: &VmSpec, inv: &Inventory, node: &Node) -> Result<(), String> {
    node_fits_with(spec, inv, node, &[])
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
            })
        })
        .collect()
}

/// [`node_fits`]와 같되 `moves`의 디스크([`disk_moves`])는 볼륨이 있는 노드 대신 대상 풀을 이 노드에서
/// 쓸 수 있는지 본다.
pub fn node_fits_with(
    spec: &VmSpec,
    inv: &Inventory,
    node: &Node,
    moves: &[DiskMove],
) -> Result<(), String> {
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
    Ok(())
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
        .map(|v| u64::from(v.spec.memory_mib))
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
    use opl_types::vm::{Arch, CpuSpec, DiskBus, DiskSpec, MachineType, NicModel, NicSpec, VmHa};

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
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status = NodeStatus {
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
            },
        );
        v.status.node = node;
        v
    }

    fn spec(memory_mib: u32, node: Option<Uuid>, power: PowerState) -> VmSpec {
        VmSpec {
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: Default::default(),
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
}
