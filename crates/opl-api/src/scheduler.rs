//! `spec.node`가 비어 있는 VM을 시작할 때 노드를 고른다(설계 3.3).

use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{Network, NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumeSpec, is_shared, pool_usable_on,
};
use opl_types::vm::{PowerState, Vm, VmSpec};
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "no node can run this VM (needs an online node that is not in maintenance, has the architecture of its machine type, can reach its storage pools and has the bridges of its networks)"
)]
pub struct NoNode;

/// 스케줄러가 보는 클러스터 상태
#[derive(Debug, Default)]
pub struct Inventory {
    pub nodes: Vec<Node>,
    pub vms: Vec<Vm>,
    pub volumes: Vec<Volume>,
    pub pools: Vec<StoragePool>,
    pub networks: Vec<Network>,
}

impl Inventory {
    pub fn load(cluster: &Cluster) -> Result<Self, ClusterError> {
        Ok(Self {
            nodes: cluster.list::<NodeSpec>()?.1,
            vms: cluster.list::<VmSpec>()?.1,
            volumes: cluster.list::<VolumeSpec>()?.1,
            pools: cluster.list::<StoragePoolSpec>()?.1,
            networks: cluster.list::<NetworkSpec>()?.1,
        })
    }

    /// 이 VM의 브리지 네트워크가 쓰는 브리지가 모두 이 노드에 있는지. VLAN을 쓰면 VLAN-aware여야 한다.
    fn has_bridges(&self, spec: &VmSpec, node: &Node) -> bool {
        spec.nics
            .iter()
            .filter(|nic| nic.network != USER_NETWORK)
            .all(|nic| {
                let Some(network) = self.networks.iter().find(|n| n.name == nic.network) else {
                    return false;
                };
                node.status.bridges.iter().any(|b| {
                    b.name == network.spec.bridge
                        && (network.spec.vlan.is_none() || b.vlan_filtering)
                })
            })
    }

    /// 이 VM의 디스크가 있는 풀을 모두 쓸 수 있는 노드인지. 공유가 아닌 풀의 볼륨은 그 파일이 있는
    /// 노드(볼륨을 만든 노드)에서만 쓸 수 있다.
    fn reaches_disks(&self, spec: &VmSpec, node: &Node) -> bool {
        spec.disks.iter().all(|disk| {
            let Some(volume) = self.volumes.iter().find(|v| v.name == disk.volume) else {
                return true;
            };
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

/// 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// 가운데 미할당 메모리가 가장 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은
/// 범위 밖). arch를 모르는 노드(옛 버전)는 고르지 않는다.
pub fn schedule(vm: Uuid, spec: &VmSpec, inv: &Inventory) -> Result<Uuid, NoNode> {
    inv.nodes
        .iter()
        .filter(|n| node_fits(spec, inv, n).is_ok())
        .map(|n| (inv.free_memory_mib(n, vm), n.id))
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, id)| id)
        .ok_or(NoNode)
}

/// [`schedule`]가 이 노드를 후보로 보는지. 아니면 그 이유다. 메모리는 보지 않는다.
pub fn node_fits(spec: &VmSpec, inv: &Inventory, node: &Node) -> Result<(), String> {
    let name = &node.name;
    if !node.status.online {
        return Err(format!("node {name:?} is offline"));
    }
    if node.spec.maintenance {
        return Err(format!("node {name:?} is in maintenance"));
    }
    if node.status.arch != Some(spec.machine.arch()) {
        let arch = format!("{:?}", spec.machine.arch()).to_lowercase();
        return Err(format!(
            "node {name:?} does not have the {arch} architecture of the VM's machine type"
        ));
    }
    if !inv.reaches_disks(spec, node) {
        return Err(format!(
            "node {name:?} cannot use the storage pools of the VM's disks"
        ));
    }
    if !inv.has_bridges(spec, node) {
        return Err(format!(
            "node {name:?} does not have the bridges of the VM's networks"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use opl_types::node::{BridgeInfo, NodeStatus, PoolState};
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::PoolBackend;
    use opl_types::vm::{Arch, CpuSpec, DiskBus, DiskSpec, MachineType, NicModel, NicSpec};

    use super::*;

    fn node(memory_mib: u64) -> Node {
        let mut n = Resource::new(
            "n",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
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
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(a.id));

        // a에 6 GiB를 쓰는 VM이 돌면 b가 더 비어 있다. 꺼진 VM은 세지 않는다.
        inv.vms.push(Resource::new(
            "big",
            spec(6144, Some(a.id), PowerState::Running),
        ));
        inv.vms.push(Resource::new(
            "off",
            spec(6144, Some(b.id), PowerState::Stopped),
        ));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(b.id));
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
        };
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(near.id));

        let empty = Inventory::default();
        assert_eq!(schedule(Uuid::now_v7(), &me, &empty), Err(NoNode));
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
        assert_eq!(schedule(Uuid::now_v7(), &me, &local), Ok(small.id));
        // 그 노드에서 풀이 준비되지 않았으면 갈 곳이 없다.
        let local = inv(
            pool("local", false),
            volume("root", "local", Some(small.id)),
            [true, true, false],
        );
        assert_eq!(schedule(Uuid::now_v7(), &me, &local), Err(NoNode));

        // 공유 풀은 준비된 노드 가운데 메모리가 가장 많은 노드로 간다.
        let shared = inv(
            pool("nas", true),
            volume("root", "nas", Some(small.id)),
            [false, true, true],
        );
        assert_eq!(schedule(Uuid::now_v7(), &me, &shared), Ok(mid.id));
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
        assert_eq!(schedule(Uuid::now_v7(), &me, &rbd), Ok(mid.id));
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
        assert_eq!(schedule(Uuid::now_v7(), &me, &unknown), Err(NoNode));
        unknown.pools[0] = pool("nas", true);
        assert_eq!(schedule(Uuid::now_v7(), &me, &unknown), Ok(big.id));
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
                    bridge: "vmbr0".into(),
                    vlan,
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
        };
        let mut me = spec(512, None, PowerState::Running);
        me.nics = vec![nic("user"), nic("lan")];
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(plain.id));
        // VLAN을 쓰려면 VLAN-aware 브리지가 있어야 한다.
        me.nics.push(nic("dmz"));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(aware.id));
        // 없는 네트워크는 어디서도 돌 수 없다.
        me.nics.push(nic("gone"));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
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
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(arm.id));
        me.machine = MachineType::Q35;
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(x86.id));
        let inv = Inventory {
            nodes: vec![arm],
            ..Default::default()
        };
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
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
                    bridge: "vmbr0".into(),
                    vlan: None,
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
        });
        assert_eq!(
            node_fits(&me, &inv, &ready),
            Err("node \"n\" does not have the bridges of the VM's networks".into())
        );
    }
}
