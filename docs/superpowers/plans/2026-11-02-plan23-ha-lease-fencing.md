# opLite 계획 23: HA 리스와 자기 펜싱 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** VM마다 HA를 켤 수 있게 하고, 각 노드가 리더에게 리스를 갱신하지 못하면 스스로 HA VM을 끄게(자기 펜싱) 한다. watchdog가 opld가 멈춘 경우를 막는다. 다른 노드로 옮겨 켜기(복구)는 계획 24다.

**Architecture:** 노드의 `HaGuard`가 `fence_after / 10`마다 `LeaderRequest::RenewLease`를 보내고, 리더는 `ensure_linearizable`로 과반수의 리더임을 확인한 뒤 받은 시각을 임기별 메모리 리스 표에 적는다. 노드는 마지막으로 성공한 갱신을 보낸 시각부터 `fence_after` 안에서만 HA VM을 켜고, 그 뒤면 워커가 강제 종료한다. watchdog는 첫 실제 갱신 뒤 HA VM을 켤 때 열고, 리스가 살아 있거나 도는 HA VM이 없을 때만 쓰다듬는다. 리더는 노드가 알린 `recover_after_secs`(fence_after + watchdog 제한 + 5초)가 지난 뒤에만 옮길 수 있다(계획 24).

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9(`ensure_linearizable`), tokio(멈춘 시계 시험 `test-util`), rustix(watchdog ioctl), axum 0.8, utoipa, clap; 웹은 계획 8~22와 같다. 새 외부 의존성은 없다(opl-reconciler가 workspace의 rustix를 쓴다).

**설계 문서:** `docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md` (§3과 §4.2·§5~§7의 계획 23 부분, "구현(계획 23)" 메모 포함)
**선행 계획:** 계획 1~22 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `VmSpec.ha`, `NodeStatus.ha`, opld `[ha]`, HA 가능 노드 검사(409 `ha_unsupported`) | 리더의 `HaManager`가 펜싱된 노드의 HA VM을 옮겨 켜기 (계획 24) |
| 리더의 리스 표(`RenewLease`, `Fenced`, 펜싱 집합 API) | 기록 Task `VmHaRecover`, 이미지 잠금 기다리기 `ImageLocked` (계획 24) |
| 노드의 `HaGuard`(갱신, fresh/stale, watchdog), 워커의 켜기 문턱과 자기 펜싱 | 외부 펜싱 장치(IPMI), sanlock `wdmd`와 watchdog 나눠 쓰기 |
| oplctl `vm set --ha`·FENCING 열, 웹 HA 스위치·배지·펜싱 열, README | 실제 watchdog 장치 시험(수동 절차만) |

## 이전 계획에서 이어받는 사항

- 계획 22: 위트니스(`ha_capable`은 위트니스를 빼고, 위트니스는 리더가 되지 않아 리더 일은 늘 일반 노드가 한다).
- 계획 20·21: 워커 보류(`begin_migration`/`end_migration`), `postmigrate` 원본 규칙, 마이그레이션 되돌리기, `two_nodes` 하네스.
- 계획 1·5: 워커 상태 기계(재시작 백오프, 게스트 종료, `stopped_by_us`), Starting 기록과 "과반수 없이도 다시 켬"(HA VM은 예외가 된다).

## 실행 환경

계획 22와 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. **진짜 watchdog 장치(`/dev/watchdog*`)는 열지 않는다** — 시험은 멈춘 tokio 시계와 가짜 장치를 쓴다. 장치 구현의 Linux 전용 시험 부분은 CI에서 처음 돈다. 새 CI 단계는 없다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`), `Cargo.lock`(cargo).

## 설계 결정

- **리스는 리더 메모리에 둔다**(Raft에 주기적으로 쓰지 않는다). 리더가 바뀌면 새 리더는 처음부터 센다 — 늦게 옮길 뿐 안전하다.
- **노드는 보내기 전 시각으로 잰다.** 성공한 갱신의 보낸 시각 s ≤ 리더가 받은 시각 r이므로, 노드가 s + fence_after 안에 끄고 watchdog가 그 뒤 제한 시간 안에 재시작하면 리더가 r + recover_after 뒤에 옮긴 VM과 겹치지 않는다.
- **갱신 성공은 리더의 인덱스가 로컬에 적용된 뒤다.** 리스를 되찾은 노드는 그 사이 옮겨진 VM을 이미 다른 노드의 것으로 본다. 그래서 켜기 직전에 로컬 `spec.node`를 다시 본다.
- **watchdog는 이전 프로세스의 타이머를 리스 없이 늘리거나 멈추지 않는다:** 장치는 fresh일 때만 열고, 제한 시간을 정하지 못해도 `V`를 쓰지 않는다.
- **펜싱 종료는 실패가 아니다:** 원하는 상태를 바꾸지 않고 재시작 백오프에도 세지 않는다. 리스가 돌아오면 다시 켠다.

## 파일 구조

```
crates/opl-types/src/{vm.rs, node.rs}                               # ha, NodeHa, Fencing, ha_capable
crates/opl-cluster/src/{lease.rs, cluster.rs, leader.rs, types.rs, memnet.rs}, tests/lease.rs   # 리스 표
crates/opl-reconciler/src/{ha.rs, worker.rs, reconciler.rs, config.rs}, tests/{ha.rs, no_quorum.rs}   # HaGuard, 워커
crates/opl-api/src/{scheduler.rs, vms.rs, cluster.rs, error.rs, tasks/{snapshot,migrate}.rs}   # 검사, 표시, 되돌리기
crates/opld/src/{config.rs, bootstrap.rs, member.rs}                # [ha], 등록, guard 연결
crates/oplctl/src/cli.rs, web/src/{vms, nodes, cluster, pages}/..., web/src/i18n/{ko,en}.json
README.md, docs/superpowers/specs/{2026-11-02-ha-fencing-witness-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: HA 설정: VmSpec.ha, NodeStatus.ha, opld [ha], HA 가능 노드 검사

`VmSpec.ha: bool`(`serde(default)`). `NodeStatus.ha: Option<NodeHa>`(없으면 빼고 쓴다), `NodeHa { fencing: Fencing, recover_after_secs }`, `Fencing { Watchdog, Process }`, `ha_capable(&Node)`(위트니스가 아니고 `status.ha`가 있음). opld `[ha]`: `fencing = watchdog | process | off`(Linux 기본 watchdog, 그 밖 off — 비 Linux의 watchdog는 설정 오류), `watchdog_device`, `watchdog_timeout_secs`(watchdog면 5..=3600), `fence_after_secs`(1 이상); `node_ha()`의 `recover_after_secs = fence_after + (watchdog면 watchdog_timeout) + 5`, 갱신 간격 `fence_after / 10`(200ms..=2s). Node 등록이 `status.ha`를 쓴다. API: HA VM 생성·수정·마이그레이션 대상이 HA 가능 노드가 아니면 409 `ha_unsupported`, 스케줄러 `node_fits`는 HA VM에 HA 가능 노드만(없으면 기존 `no_node`). 스냅샷 되돌리기는 지금 VM의 `ha`를 유지한다.

**Files:**
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/src/schedule.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-api/tests/vms.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opl-types/src/vm.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/config.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/janitor.rs`에서

```rust
                    snapshot_schedule: None,
                },
```

을 다음으로 바꾼다:

```rust
                    snapshot_schedule: None,
                    ha: false,
                },
```

`crates/opl-api/src/schedule.rs`에서

```rust
                }),
            },
```

을 다음으로 바꾼다:

```rust
                }),
                ha: false,
            },
```

`crates/opl-api/src/scheduler.rs` 전체:

```rust
//! `spec.node`가 비어 있는 VM을 시작할 때 노드를 고른다(설계 3.3).

use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{Network, NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec, ha_capable};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumeSpec, is_shared, pool_usable_on,
};
use opl_types::vm::{PowerState, Vm, VmSpec};
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "no node can run this VM (needs an online node that is not a witness or in maintenance, has the architecture of its machine type, can reach its storage pools, has the bridges of its networks and, for an HA VM, has HA fencing)"
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

/// 위트니스가 아니고 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// (HA VM이면 HA 가능 노드) 가운데 미할당 메모리가 가장 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은
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
    if spec.ha && !ha_capable(node) {
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
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, NodeStatus, PoolState};
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
                witness: false,
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
            ha: false,
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
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(small.id));
        inv.nodes.pop();
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }

    #[test]
    fn ha_vms_go_only_to_ha_capable_nodes() {
        // 메모리가 가장 많은 노드도 펜싱이 꺼져 있으면 HA VM을 받지 않는다.
        let off = node(64000);
        let mut fenced = node(1024);
        fenced.status.ha = Some(NodeHa {
            fencing: Fencing::Process,
            recover_after_secs: 7,
        });
        let mut inv = Inventory {
            nodes: vec![off.clone(), fenced.clone()],
            ..Default::default()
        };
        let mut me = spec(512, None, PowerState::Running);
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(off.id));
        me.ha = true;
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(fenced.id));
        assert_eq!(
            node_fits(&me, &inv, &off),
            Err("node \"n\" does not support HA (fencing is off)".into())
        );
        inv.nodes.pop();
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
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
        });
        assert_eq!(
            node_fits(&me, &inv, &ready),
            Err("node \"n\" does not have the bridges of the VM's networks".into())
        );
    }
}
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
        spec.node = vm.spec.node;
        // 예약은 스냅샷 때의 것이 아니라 지금 것을 지킨다.
        spec.snapshot_schedule = vm.spec.snapshot_schedule;
        if snapshot.spec.memory && spec.node.is_none() {
```

을 다음으로 바꾼다:

```rust
        spec.node = vm.spec.node;
        // 예약과 HA는 스냅샷 때의 것이 아니라 지금 것을 지킨다.
        spec.snapshot_schedule = vm.spec.snapshot_schedule;
        spec.ha = vm.spec.ha;
        if snapshot.spec.memory && spec.node.is_none() {
```

`crates/opl-api/src/vms.rs`에서

```rust
use opl_types::network::USER_NETWORK;
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
```

을 다음으로 바꾼다:

```rust
use opl_types::network::USER_NETWORK;
use opl_types::node::{Node, NodeSpec, ha_capable};
use opl_types::resource::{Resource, ResourceSpec};
```

`crates/opl-api/src/vms.rs`에서

```rust
        check_not_witness(node)?;
        check_node_arch(node, spec)?;
```

을 다음으로 바꾼다:

```rust
        check_not_witness(node)?;
        check_ha_capable(node, spec)?;
        check_node_arch(node, spec)?;
```

`crates/opl-api/src/vms.rs`에서

```rust
            format!("node {:?} is a witness node and cannot run VMs", node.name),
        ));
```

을 다음으로 바꾼다:

```rust
            format!("node {:?} is a witness node and cannot run VMs", node.name),
        ));
    }
    Ok(())
}

/// HA VM인데 노드가 HA 가능 노드(펜싱이 켜진 노드)가 아니면 409 `ha_unsupported`.
fn check_ha_capable(node: &Node, spec: &VmSpec) -> Result<(), ApiError> {
    if spec.ha && !ha_capable(node) {
        return Err(ApiError::conflict(
            "ha_unsupported",
            format!(
                "node {:?} does not support HA (fencing is off) and cannot run HA VMs",
                node.name
            ),
        ));
```

`crates/opl-api/src/vms.rs`에서

```rust
/// `migration_unsupported`). 대상 노드가 위트니스이거나 오프라인이거나 유지보수 중이면 409 `node_unavailable`,
/// 스케줄러가 고를 수 없는 노드(풀, 네트워크, 남은 메모리)면 409 `no_capacity`다.
```

을 다음으로 바꾼다:

```rust
/// `migration_unsupported`). 대상 노드가 위트니스이거나 오프라인이거나 유지보수 중이면 409 `node_unavailable`,
/// HA VM인데 대상이 HA 가능 노드가 아니면 409 `ha_unsupported`,
/// 스케줄러가 고를 수 없는 노드(풀, 네트워크, 남은 메모리)면 409 `no_capacity`다.
```

`crates/opl-api/src/vms.rs`에서

```rust
    check_not_witness(&target)?;
    if !target.status.online || target.spec.maintenance {
```

을 다음으로 바꾼다:

```rust
    check_not_witness(&target)?;
    check_ha_capable(&target, &vm.spec)?;
    if !target.status.online || target.spec.maintenance {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            pools: Default::default(),
        };
```

을 다음으로 바꾼다:

```rust
            pools: Default::default(),
            ha: None,
        };
```

`crates/opl-api/tests/migrate.rs`에서

```rust
use common::{Client, Env, mark_pool_ready, vm_request};
use opl_types::node::{Node, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use common::{Client, Env, mark_pool_ready, vm_request};
use opl_types::node::{Fencing, Node, NodeHa, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
```

`crates/opl-api/tests/migrate.rs`에서

```rust
        pools: Default::default(),
    };
```

을 다음으로 바꾼다:

```rust
        pools: Default::default(),
        ha: None,
    };
```

`crates/opl-api/tests/migrate.rs`에서

```rust
    assert_eq!(status, StatusCode::NOT_FOUND);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// HA VM은 HA 가능 노드(펜싱이 켜진 노드)로만 옮긴다.
#[tokio::test]
async fn ha_vms_migrate_only_to_ha_capable_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup(&env, &admin).await;
    let ha = Some(NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 7,
    });
    edit_node(&env, env.node, |n| n.status.ha = ha).await;
    // 실행 중인 VM도 HA로 바꿀 수 있다.
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(true);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_eq!(
        body["message"],
        "node \"node2\" does not support HA (fencing is off) and cannot run HA VMs",
        "{body}"
    );
    assert_conflict((status, body), "ha_unsupported");

    edit_node(&env, node2, |n| n.status.ha = ha).await;
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
}
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
    assert_eq!(env.pid(vm).await, None);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(env.pid(vm).await, None);
}

/// 되돌려도 `ha`는 스냅샷 때의 것이 아니라 지금 것을 지킨다(예약처럼).
#[tokio::test]
async fn revert_keeps_the_current_ha() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "bare", &[]).await;
    succeeded(&snapshot(&admin, "bare", json!({ "name": "cfg" })).await);
    assert!(!get(&env, vm, "cfg").spec.vm_spec.ha);

    // 시험 노드는 HA 가능 노드가 아니므로 API를 거치지 않고 바꾼다(reconciler의 status 쓰기와
    // 부딪치면 다시 읽어 쓴다).
    loop {
        let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
        let mut spec = current.spec.clone();
        spec.ha = true;
        match env
            .cluster
            .update_spec(vm, Some(current.revision), current.labels, &spec)
            .await
        {
            Err(opl_cluster::ClusterError::Rejected(_)) => continue,
            r => {
                r.unwrap();
                break;
            }
        }
    }
    let (status, task) = admin
        .post("/vms/bare/snapshots/cfg/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert!(env.cluster.get::<VmSpec>(vm).unwrap().unwrap().spec.ha);
}
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            pools: Default::default(),
        };
```

을 다음으로 바꾼다:

```rust
            pools: Default::default(),
            ha: None,
        };
```

`crates/opl-api/tests/vms.rs`에서

```rust
use opl_driver::DriverError;
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_driver::DriverError;
use opl_types::node::{Fencing, Node, NodeHa, NodeSpec};
use opl_types::resource::Resource;
```

`crates/opl-api/tests/vms.rs`에서

```rust
}

#[tokio::test]
async fn update_checks_if_match_revision() {
```

을 다음으로 바꾼다:

```rust
}

/// HA VM은 HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. 실행 중에도 `ha`를 바꿀 수 있다.
#[tokio::test]
async fn ha_vms_need_an_ha_capable_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 노드를 정하지 않은 HA VM은 만들 수 있지만, HA 가능 노드가 없으면 켤 수 없다.
    let mut req = vm_request("web");
    req["spec"]["ha"] = json!(true);
    let (status, vm) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["ha"], true);
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node");

    // 펜싱이 꺼진 노드에 두면 만들 때도 고칠 때도 409다.
    req["name"] = json!("db");
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_unsupported");
    assert_eq!(
        body["message"],
        "node \"node1\" does not support HA (fencing is off) and cannot run HA VMs"
    );
    let mut spec = vm["spec"].clone();
    spec["node"] = json!(env.node);
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_unsupported");

    // 이 노드에 펜싱을 켜면 스케줄러가 이 노드를 고른다.
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.ha = Some(NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 7,
    });
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());

    // 실행 중인 VM의 `ha`는 바꿀 수 있다.
    for ha in [false, true] {
        let mut spec = vm["spec"].clone();
        spec["ha"] = json!(ha);
        let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert_eq!(body["spec"]["ha"], ha);
    }
    // 노드의 펜싱이 꺼진 뒤에는 HA로 바꿀 수 없다.
    status.ha = None;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["ha"] = json!(false);
    let (code, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    spec["ha"] = json!(true);
    let (code, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ha_unsupported");
}

#[tokio::test]
async fn update_checks_if_match_revision() {
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
            snapshot_schedule: None,
        },
```

을 다음으로 바꾼다:

```rust
            snapshot_schedule: None,
            ha: false,
        },
```

`crates/opl-types/src/node.rs`에서

```rust
    pub pools: BTreeMap<String, PoolState>,
}
```

을 다음으로 바꾼다:

```rust
    pub pools: BTreeMap<String, PoolState>,
    /// 이 노드의 HA 펜싱. None이면 펜싱이 꺼져 HA VM을 돌리지 않는다(위트니스는 늘 None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    // 없으면 빼고 쓰므로 API 문서에서도 null·기본값이 아니라 선택 필드다.
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ha: Option<NodeHa>,
}

/// 노드가 등록할 때 쓰는 HA 펜싱 설정
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeHa {
    pub fencing: Fencing,
    /// 마지막 리스 갱신 뒤 이만큼 지나면 이 노드의 HA VM이 꺼졌다고 보고 리더가 옮긴다(초).
    pub recover_after_secs: u64,
}

/// 리스를 잃은 노드가 HA VM을 끄는 방법
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Fencing {
    /// opld가 HA VM을 끄고, opld가 멈추면 watchdog가 노드를 재시작한다.
    Watchdog,
    /// opld가 HA VM을 끄는 것만 한다(시험·개발용).
    Process,
}

/// HA 가능 노드: 위트니스가 아니고 펜싱이 켜진 노드. HA VM은 이런 노드에서만 돈다.
pub fn ha_capable(node: &Node) -> bool {
    !node.spec.witness && node.status.ha.is_some()
}
```

`crates/opl-types/src/node.rs`에서

```rust
        assert_eq!(status, NodeStatus::default());
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(status, NodeStatus::default());
        assert_eq!(status.ha, None);
    }

    #[test]
    fn status_ha_round_trips_and_is_omitted_when_off() {
        let off = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(off.get("ha").is_none(), "{off}");
        let status = NodeStatus {
            ha: Some(NodeHa {
                fencing: Fencing::Watchdog,
                recover_after_secs: 65,
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["ha"],
            serde_json::json!({ "fencing": "watchdog", "recover_after_secs": 65 })
        );
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
        assert_eq!(
            serde_json::to_value(Fencing::Process).unwrap(),
            serde_json::json!("process")
        );
    }

    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
        let mut node: Node = Resource::new(
            "n",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
            },
        );
        assert!(!ha_capable(&node));
        node.status.ha = Some(NodeHa {
            fencing: Fencing::Process,
            recover_after_secs: 7,
        });
        assert!(ha_capable(&node));
        node.spec.witness = true;
        assert!(!ha_capable(&node));
    }
```

`crates/opl-types/src/vm.rs`에서

```rust
    pub snapshot_schedule: Option<SnapshotSchedule>,
}
```

을 다음으로 바꾼다:

```rust
    pub snapshot_schedule: Option<SnapshotSchedule>,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. HA 가능 노드(펜싱이 켜진 노드)에서만 돈다.
    #[serde(default)]
    pub ha: bool,
}
```

`crates/opl-types/src/vm.rs`에서

```rust
        assert_eq!(spec.snapshot_schedule, None);
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(spec.snapshot_schedule, None);
        assert!(!spec.ha);
    }
```

`crates/opld/src/bootstrap.rs`에서

```rust

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
```

을 다음으로 바꾼다:

```rust

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱을
/// 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
```

`crates/opld/src/bootstrap.rs`에서

```rust
            pools: std::mem::take(&mut status.pools),
        };
```

을 다음으로 바꾼다:

```rust
            pools: std::mem::take(&mut status.pools),
            // 펜싱이 꺼져 있으면 None이라 리더가 이 노드의 HA VM을 옮기지 않는다.
            ha: config.ha.node_ha(),
        };
```

`crates/opld/src/bootstrap.rs`에서

```rust
    use opl_driver::{DriverError, HostNet, Tap};
    use opl_types::node::{BridgeInfo, PoolState};
```

을 다음으로 바꾼다:

```rust
    use opl_driver::{DriverError, HostNet, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, PoolState};

    use crate::config::FencingSetting;
```

`crates/opld/src/bootstrap.rs`에서

```rust
        }
        cluster.shutdown().await.unwrap();
```

을 다음으로 바꾼다:

```rust
        }
        cluster.shutdown().await.unwrap();
    }

    /// `[ha]`의 펜싱을 기록하고, 설정을 바꿔 다시 등록하면 그 값으로 바꾼다. `off`면 없다.
    #[tokio::test]
    async fn registration_records_the_ha_fencing() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config).unwrap();
        host.net = None;
        config.ha.fencing = FencingSetting::Process;
        config.ha.fence_after_secs = 2;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.ha,
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
            })
        );
        config.ha.fencing = FencingSetting::Off;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.ha, None);
        cluster.shutdown().await.unwrap();
```

`crates/opld/src/config.rs`에서

```rust
use opl_qemu::{Accel, HostProfile, kvm_available};
use serde::Deserialize;
```

을 다음으로 바꾼다:

```rust
use opl_qemu::{Accel, HostProfile, kvm_available};
use opl_types::node::{Fencing, NodeHa};
use serde::Deserialize;
```

`crates/opld/src/config.rs`에서

```rust
    pub migration: MigrationSection,
}
```

을 다음으로 바꾼다:

```rust
    pub migration: MigrationSection,
    #[serde(default)]
    pub ha: HaSection,
}
```

`crates/opld/src/config.rs`에서

```rust

fn default_migration_ports() -> RangeInclusive<u16> {
```

을 다음으로 바꾼다:

```rust

/// HA 리스와 자기 펜싱(설계 §3.1). 위트니스는 쓰지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaSection {
    /// `watchdog`(opld가 HA VM을 끄고, opld가 멈추면 watchdog가 노드를 재시작), `process`(opld가
    /// 끄는 것만 — 시험·개발용), `off`(이 노드는 HA VM을 돌리지 않는다). 기본은 Linux `watchdog`,
    /// macOS `off`다.
    #[serde(default)]
    pub fencing: FencingSetting,
    /// Linux watchdog 장치(하드웨어 또는 `softdog`)
    #[serde(default = "default_watchdog_device")]
    pub watchdog_device: PathBuf,
    /// 장치에 정하는 제한 시간(초, `WDIOC_SETTIMEOUT`)
    #[serde(default = "default_watchdog_timeout_secs")]
    pub watchdog_timeout_secs: u64,
    /// 마지막으로 성공한 리스 갱신 뒤 이만큼 지나면 HA VM을 끈다(초).
    #[serde(default = "default_fence_after_secs")]
    pub fence_after_secs: u64,
}

impl Default for HaSection {
    fn default() -> Self {
        Self {
            fencing: FencingSetting::default(),
            watchdog_device: default_watchdog_device(),
            watchdog_timeout_secs: default_watchdog_timeout_secs(),
            fence_after_secs: default_fence_after_secs(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FencingSetting {
    Watchdog,
    Process,
    Off,
}

impl Default for FencingSetting {
    fn default() -> Self {
        if cfg!(target_os = "linux") {
            Self::Watchdog
        } else {
            Self::Off
        }
    }
}

/// `watchdog_timeout_secs`의 하한. 1초마다 쓰다듬으므로 한두 번 늦어도 재시작되지 않게 둔다.
const MIN_WATCHDOG_TIMEOUT_SECS: u64 = 5;
/// `watchdog_timeout_secs`의 상한(1시간)
const MAX_WATCHDOG_TIMEOUT_SECS: u64 = 3600;
/// 리더가 옮기기 전에 더 기다리는 여유(초)
const RECOVER_MARGIN_SECS: u64 = 5;

impl HaSection {
    /// 등록할 때 쓰는 이 노드의 HA 설정. 펜싱이 꺼져 있으면 None이다.
    pub fn node_ha(&self) -> Option<NodeHa> {
        let (fencing, watchdog) = match self.fencing {
            FencingSetting::Off => return None,
            FencingSetting::Watchdog => (Fencing::Watchdog, self.watchdog_timeout_secs),
            FencingSetting::Process => (Fencing::Process, 0),
        };
        Some(NodeHa {
            fencing,
            recover_after_secs: self
                .fence_after_secs
                .saturating_add(watchdog)
                .saturating_add(RECOVER_MARGIN_SECS),
        })
    }
}

// 노드의 `HaGuard`를 만들 때 쓴다. 그 전까지는 시험만 쓴다.
#[cfg_attr(not(test), expect(dead_code))]
impl HaSection {
    pub fn fence_after(&self) -> Duration {
        Duration::from_secs(self.fence_after_secs)
    }

    /// 리스 갱신 간격: `fence_after`의 10분의 1을 0.2초 이상 2초 이하로 자른 값
    pub fn renew_interval(&self) -> Duration {
        (self.fence_after() / 10).clamp(Duration::from_millis(200), Duration::from_secs(2))
    }
}

fn default_watchdog_device() -> PathBuf {
    "/dev/watchdog".into()
}
fn default_watchdog_timeout_secs() -> u64 {
    30
}
fn default_fence_after_secs() -> u64 {
    30
}

fn default_migration_ports() -> RangeInclusive<u16> {
```

`crates/opld/src/config.rs`에서

```rust
            anyhow::bail!("pools.refresh_secs must be at least 1");
        }
```

을 다음으로 바꾼다:

```rust
            anyhow::bail!("pools.refresh_secs must be at least 1");
        }
        if self.ha.fence_after_secs == 0 {
            anyhow::bail!("ha.fence_after_secs must be at least 1");
        }
        if self.ha.fencing == FencingSetting::Watchdog {
            if !(MIN_WATCHDOG_TIMEOUT_SECS..=MAX_WATCHDOG_TIMEOUT_SECS)
                .contains(&self.ha.watchdog_timeout_secs)
            {
                anyhow::bail!(
                    "ha.watchdog_timeout_secs must be between {MIN_WATCHDOG_TIMEOUT_SECS} and {MAX_WATCHDOG_TIMEOUT_SECS}"
                );
            }
            if !linux {
                anyhow::bail!("ha.fencing = \"watchdog\" needs Linux; use \"process\" or \"off\"");
            }
        }
```

`crates/opld/src/config.rs`에서

```rust
    #[test]
    fn migration_section_parses_ports_and_timeout() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn ha_section_defaults_by_os() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        c.validate().unwrap();
        assert_eq!(c.ha.watchdog_device, Path::new("/dev/watchdog"));
        assert_eq!(c.ha.watchdog_timeout_secs, 30);
        assert_eq!(c.ha.fence_after_secs, 30);
        assert_eq!(c.ha.fence_after(), Duration::from_secs(30));
        if cfg!(target_os = "linux") {
            assert_eq!(c.ha.fencing, FencingSetting::Watchdog);
            assert_eq!(
                c.ha.node_ha(),
                Some(NodeHa {
                    fencing: Fencing::Watchdog,
                    recover_after_secs: 65,
                })
            );
        } else {
            assert_eq!(c.ha.fencing, FencingSetting::Off);
            assert_eq!(c.ha.node_ha(), None);
        }
    }

    #[test]
    fn ha_section_sets_fencing_and_recover_after() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nfence_after_secs = 2",
        );
        c.validate().unwrap();
        // process는 watchdog 제한 시간을 더하지 않는다.
        assert_eq!(
            c.ha.node_ha(),
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
            })
        );
        let c = parse("name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"off\"");
        c.validate().unwrap();
        assert_eq!(c.ha.node_ha(), None);
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"watchdog\"\n\
             watchdog_device = \"/dev/watchdog1\"\nwatchdog_timeout_secs = 10\nfence_after_secs = 20",
        );
        assert_eq!(c.ha.watchdog_device, Path::new("/dev/watchdog1"));
        assert_eq!(
            c.ha.node_ha(),
            Some(NodeHa {
                fencing: Fencing::Watchdog,
                recover_after_secs: 35,
            })
        );
        // watchdog 장치는 Linux에만 있다.
        assert_eq!(c.validate().is_ok(), cfg!(target_os = "linux"));
        for bad in ["fencing = \"ipmi\"", "fence_after = 1"] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[ha]\n{bad}");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn ha_section_validates_times() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nfence_after_secs = 0",
        );
        assert!(c.validate().is_err());
        // 꺼져 있어도 fence_after는 검사한다.
        let c =
            parse("name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"off\"\nfence_after_secs = 0");
        assert!(c.validate().is_err());
        // 제한 시간은 5초 이상이다. 장치를 1초마다 쓰다듬으므로 너무 짧으면 쓰다듬기 한 번만 늦어도
        // 노드가 재시작된다. Linux가 아니면 범위 안이어도 장치가 없어 실패한다.
        for (timeout, ok) in [
            (0, false),
            (4, false),
            (5, true),
            (3600, true),
            (3601, false),
        ] {
            let c = parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"watchdog\"\nwatchdog_timeout_secs = {timeout}"
            ));
            match c.validate() {
                Ok(()) => assert!(ok && cfg!(target_os = "linux"), "{timeout}"),
                Err(e) => assert_eq!(
                    e.to_string().contains("watchdog_timeout_secs"),
                    !ok,
                    "{timeout}: {e}"
                ),
            }
        }
        // watchdog가 아니면 장치 제한 시간은 쓰지 않으므로 보지 않는다.
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nwatchdog_timeout_secs = 0",
        );
        c.validate().unwrap();
    }

    #[test]
    fn renew_interval_is_a_tenth_of_fence_after_within_bounds() {
        let interval = |fence_after_secs| {
            HaSection {
                fence_after_secs,
                ..HaSection::default()
            }
            .renew_interval()
        };
        assert_eq!(interval(1), Duration::from_millis(200));
        assert_eq!(interval(2), Duration::from_millis(200));
        assert_eq!(interval(5), Duration::from_millis(500));
        assert_eq!(interval(20), Duration::from_secs(2));
        assert_eq!(interval(30), Duration::from_secs(2));
        assert_eq!(interval(u64::MAX), Duration::from_secs(2));
    }

    #[test]
    fn migration_section_parses_ports_and_timeout() {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 773개, 실패 0

Run: `cargo test -p opl-types -p opld -p opl-api`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 313개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat: HA 설정 — VmSpec.ha, NodeStatus.ha, opld [ha], HA 가능 노드 검사"
```

---

### Task 2: 리더의 HA 리스 표: RenewLease, Rejection::Fenced

opl-cluster `lease.rs`. `LeaderRequest::RenewLease { node }`를 받은 리더는 임기를 적고 `ensure_linearizable`로 과반수의 리더임을 확인한 뒤, 한 잠금 안에서 같은 임기의 리더인지 다시 보고, 그 노드가 펜싱 중이면 `Rejection::Fenced`, 아니면 (임기, 받은 시각)을 리스 표에 적는다. 응답은 펜싱 확인 뒤 두 번째 `ensure_linearizable`의 인덱스이고, 노드(`Cluster::renew_lease(timeout)`)는 그 인덱스가 자기 저장소에 적용돼야 성공으로 친다. 리스 표(`Cluster::leases() -> Leases { term, renewed }`)는 임기가 바뀌면 비우고, 펜싱 집합(`begin_fence`/`end_fence`)은 임기와 무관하다. memnet: `MemRouter::register(id, &Cluster)`(전달된 갱신이 리스 표에 닿게), `block_raft(id)`. 시험 `tests/lease.rs`: 기록·임기, 적용 기다림, 고립된 팔로워·리더, 펜싱 중 거부, 임기가 바뀌면 비움.

**Files:**
- Create: `crates/opl-cluster/src/lease.rs`
- Create: `crates/opl-cluster/tests/lease.rs`
- Modify: `crates/opl-api/src/error.rs`
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/leader.rs`
- Modify: `crates/opl-cluster/src/lib.rs`
- Modify: `crates/opl-cluster/src/memnet.rs`
- Modify: `crates/opl-cluster/src/types.rs`
- Modify: `crates/opl-cluster/tests/common/mod.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opld/src/bootstrap.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/error.rs`에서

```rust
                    ),
                }
```

을 다음으로 바꾼다:

```rust
                    ),
                    // 리스 갱신만 받는 거부라 API 요청에서는 나오지 않는다.
                    Rejection::Fenced { .. } => ApiError::conflict("node_fenced", message),
                }
```

`crates/opl-api/src/janitor.rs`에서

```rust
            .unwrap();
        router.register(id, cluster.raft().clone());
        cluster
```

을 다음으로 바꾼다:

```rust
            .unwrap();
        router.register(id, &cluster);
        cluster
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let cluster = Arc::new(Cluster::open(config, router.network(id)).await.unwrap());
        router.register(id, cluster.raft().clone());
        cluster
```

을 다음으로 바꾼다:

```rust
        let cluster = Arc::new(Cluster::open(config, router.network(id)).await.unwrap());
        router.register(id, &cluster);
        cluster
```

`crates/opl-cluster/src/cluster.rs`에서

```rust

use crate::leader::{self, Forwarder};
use crate::store::{Store, StoreError, get_object, get_object_by_name, list_objects};
```

을 다음으로 바꾼다:

```rust

use crate::leader::{self, Forwarder, LeaderCtx};
use crate::lease::{LeaseTable, Leases};
use crate::store::{Store, StoreError, get_object, get_object_by_name, list_objects};
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
    forwarder: Option<Arc<dyn Forwarder>>,
}
```

을 다음으로 바꾼다:

```rust
    forwarder: Option<Arc<dyn Forwarder>>,
    /// 리더일 때 받은 HA 리스 갱신(메모리에만 둔다)
    leases: Arc<LeaseTable>,
}
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
            forwarder: None,
        })
```

을 다음으로 바꾼다:

```rust
            forwarder: None,
            leases: Arc::default(),
        })
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
    pub async fn request(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        let deadline = tokio::time::Instant::now() + self.write_timeout;
        // 응답 없이 끝난 전달 시도가 있었다(리더가 처리했을 수 있다).
```

을 다음으로 바꾼다:

```rust
    pub async fn request(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        self.request_within(req, self.write_timeout).await
    }

    /// `request`와 같되 `write_timeout` 대신 `timeout`까지만 기다린다.
    async fn request_within(
        &self,
        req: LeaderRequest,
        timeout: Duration,
    ) -> Result<u64, ClusterError> {
        let deadline = tokio::time::Instant::now() + timeout;
        // 응답 없이 끝난 전달 시도가 있었다(리더가 처리했을 수 있다).
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let result = match leader::execute(&self.raft, req.clone(), remaining).await {
                Err(ClusterError::NotLeader {
```

을 다음으로 바꾼다:

```rust
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let result = match leader::execute(&self.leader_ctx(), req.clone(), remaining).await {
                Err(ClusterError::NotLeader {
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
    pub async fn execute_as_leader(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        leader::execute(&self.raft, req, self.write_timeout).await
    }
```

을 다음으로 바꾼다:

```rust
    pub async fn execute_as_leader(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        leader::execute(&self.leader_ctx(), req, self.write_timeout).await
    }

    pub(crate) fn leader_ctx(&self) -> LeaderCtx {
        LeaderCtx {
            raft: self.raft.clone(),
            leases: self.leases.clone(),
        }
    }

    /// 이 노드의 HA 리스를 갱신한다. 리더가 과반수의 리더인지 확인하고 갱신을 적은 뒤, 리더가 돌려준
    /// 적용 인덱스까지 이 노드에도 적용되어야 성공이다(그 사이 리더가 옮긴 VM이 이미 보인다).
    /// `timeout` 안에 끝나지 않으면 `Timeout`이다. 호출자는 보내기 전에 시각을 재고 성공하면 그
    /// 시각을 마지막 갱신으로 삼는다(설계 §3.2).
    pub async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let index = self
            .request_within(LeaderRequest::RenewLease { node: self.id }, timeout)
            .await?;
        // 전달 경로의 적용 기다림은 제한 시간이 지나도 성공으로 돌려주므로 여기서 다시 확인한다.
        self.raft
            .wait(Some(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            ))
            .applied_index_at_least(Some(index), "lease renewal applied")
            .await
            .map_err(|_| ClusterError::Timeout)?;
        Ok(())
    }

    /// 리더가 지금 임기에 받은 리스 갱신. 리더에서만 뜻이 있다(리더인지는 `read_barrier`로 확인한다).
    pub fn leases(&self) -> Leases {
        self.leases.leases(&self.raft)
    }

    /// `node`를 펜싱 중으로 표시한다. `end_fence`까지 그 노드의 갱신은 `Rejection::Fenced`로
    /// 거부된다. 표시하기 직전에 받은 갱신이 있을 수 있으므로, 표시한 뒤 `leases`를 다시 읽어
    /// 그 노드의 리스가 여전히 끝났는지 확인하고 나서 복구한다.
    pub fn begin_fence(&self, node: NodeId) {
        self.leases.begin_fence(node);
    }

    /// 펜싱 중 표시를 지운다. 복구 쓰기가 끝난(리더에 적용된) 뒤에 부른다.
    pub fn end_fence(&self, node: NodeId) {
        self.leases.end_fence(node);
    }
```

`crates/opl-cluster/src/leader.rs`에서

```rust
use std::future::Future;
use std::time::Duration;
```

을 다음으로 바꾼다:

```rust
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
```

`crates/opl-cluster/src/leader.rs`에서

```rust
use crate::cluster::ClusterError;
use crate::types::{LeaderRequest, NodeId, Rejection, Response, TypeConfig};
```

을 다음으로 바꾼다:

```rust
use crate::cluster::ClusterError;
use crate::lease::LeaseTable;
use crate::types::{LeaderRequest, NodeId, Rejection, Response, TypeConfig};
```

`crates/opl-cluster/src/leader.rs`에서

```rust

/// 이 노드가 리더라고 보고 요청을 처리한다. 리더가 아니면 `NotLeader`다. 성공하면 커밋된 로그 인덱스.
pub(crate) async fn execute(
    raft: &Raft<TypeConfig>,
    req: LeaderRequest,
    timeout: Duration,
) -> Result<u64, ClusterError> {
    match req {
        LeaderRequest::Write { command } => {
```

을 다음으로 바꾼다:

```rust

/// 리더로서 요청을 처리하는 데 쓰는 것: Raft와 리스 표
#[derive(Clone)]
pub(crate) struct LeaderCtx {
    pub(crate) raft: Raft<TypeConfig>,
    pub(crate) leases: Arc<LeaseTable>,
}

/// 이 노드가 리더라고 보고 요청을 처리한다. 리더가 아니면 `NotLeader`다. 성공하면 커밋된 로그 인덱스
/// (리스 갱신은 리더가 적용한 마지막 인덱스).
pub(crate) async fn execute(
    ctx: &LeaderCtx,
    req: LeaderRequest,
    timeout: Duration,
) -> Result<u64, ClusterError> {
    let raft = &ctx.raft;
    match req {
        LeaderRequest::RenewLease { node } => ctx.leases.renew(raft, node, timeout).await,
        LeaderRequest::Write { command } => {
```

`crates/opl-cluster/src/leader.rs`에서

```rust
/// 리더인지 보기 전에 변경을 검사하므로, 팔로워에서는 `ForwardToLeader` 대신 검사 오류가 날 수 있다.
fn not_leader(m: &RaftMetrics<NodeId, BasicNode>) -> ClusterError {
    let leader_addr = m.current_leader.and_then(|l| {
```

을 다음으로 바꾼다:

```rust
/// 리더인지 보기 전에 변경을 검사하므로, 팔로워에서는 `ForwardToLeader` 대신 검사 오류가 날 수 있다.
pub(crate) fn not_leader(m: &RaftMetrics<NodeId, BasicNode>) -> ClusterError {
    let leader_addr = m.current_leader.and_then(|l| {
```

`crates/opl-cluster/src/lease.rs`를 만든다:

```rust
//! 리더의 HA 리스 표. 노드가 보낸 갱신(`RenewLease`)을 받은 시각을 임기별로 메모리에 둔다.
//! 안전 조건은 설계 문서(2026-11-02-ha-fencing-witness-design.md) §3.2·§4.2를 따른다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Duration;

use openraft::error::{CheckIsLeaderError, RaftError};
use openraft::{LogId, Raft, ServerState};
use tokio::time::Instant;

use crate::cluster::ClusterError;
use crate::leader::not_leader;
use crate::types::{NodeId, Rejection, TypeConfig};

/// 리더가 아는 리스. `renewed`는 `term` 임기에 이 리더가 확인해 준 노드별 마지막 갱신 시각이다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leases {
    pub term: u64,
    pub renewed: BTreeMap<NodeId, Instant>,
}

#[derive(Default)]
struct Inner {
    /// `renewed`가 속한 임기. 더 높은 임기를 보면 `renewed`를 비운다.
    term: u64,
    renewed: BTreeMap<NodeId, Instant>,
    /// 펜싱 중인 노드. 이 노드의 갱신은 `Fenced`로 거부한다. 임기가 바뀌어도 지우지 않는다.
    /// 지우면 이전 임기에 시작한 복구 쓰기가 끝나기 전에 그 노드가 리스를 받을 수 있다.
    fencing: BTreeSet<NodeId>,
}

impl Inner {
    /// 임기가 바뀌었으면 갱신 기록을 비운다.
    fn enter(&mut self, term: u64) {
        if term > self.term {
            self.term = term;
            self.renewed.clear();
        }
    }
}

#[derive(Default)]
pub(crate) struct LeaseTable {
    inner: Mutex<Inner>,
}

impl LeaseTable {
    /// 리더로서 `node`의 갱신을 처리한다. 성공하면 리더가 적용한 마지막 로그 인덱스를 돌려준다.
    pub(crate) async fn renew(
        &self,
        raft: &Raft<TypeConfig>,
        node: NodeId,
        timeout: Duration,
    ) -> Result<u64, ClusterError> {
        let term = {
            let m = raft.metrics().borrow().clone();
            if m.state != ServerState::Leader {
                return Err(not_leader(&m));
            }
            m.current_term
        };
        let deadline = Instant::now() + timeout;
        // 아직 과반수의 리더인지 확인한다. 노드는 보내기 전에 시각을 쟀으므로 이 확인 뒤에 적는
        // 시각은 그보다 늦다. 새 임기의 과반수가 투표한 뒤에는 이 확인이 성공할 수 없다.
        confirm_leader(raft, deadline).await?;
        {
            let mut inner = self.inner.lock().unwrap();
            let m = raft.metrics().borrow().clone();
            // 확인하는 사이 임기가 바뀌었으면 그 임기의 표에 적지 않는다.
            if m.state != ServerState::Leader || m.current_term != term {
                return Err(not_leader(&m));
            }
            // 펜싱 검사와 기록은 한 잠금 안에서 한다(`Cluster::begin_fence` 참고).
            if inner.fencing.contains(&node) {
                return Err(ClusterError::Rejected(Rejection::Fenced { node }));
            }
            inner.enter(term);
            inner.renewed.insert(node, Instant::now());
        }
        // 돌려주는 인덱스는 펜싱 검사 뒤에 다시 확인한 커밋 인덱스다. 펜싱 표시를 지우기 전에 끝난
        // 복구 쓰기가 모두 들어 있다. 첫 확인의 인덱스는 그보다 앞설 수 있고, 지표(metrics)의 적용
        // 인덱스는 쓰기 응답보다 늦게 바뀔 수 있어 쓰지 않는다.
        let read = confirm_leader(raft, deadline).await?;
        Ok(read.map_or(0, |l| l.index))
    }

    /// 지금 이 노드의 임기 기준의 리스. 표가 더 낮은 임기의 것이면 비운다.
    pub(crate) fn leases(&self, raft: &Raft<TypeConfig>) -> Leases {
        let mut inner = self.inner.lock().unwrap();
        let term = raft.metrics().borrow().current_term;
        inner.enter(term);
        Leases {
            term,
            renewed: inner.renewed.clone(),
        }
    }

    pub(crate) fn begin_fence(&self, node: NodeId) {
        self.inner.lock().unwrap().fencing.insert(node);
    }

    pub(crate) fn end_fence(&self, node: NodeId) {
        self.inner.lock().unwrap().fencing.remove(&node);
    }
}

/// 과반수에게 아직 리더인지 확인하고, 그 시점의 커밋 인덱스까지 적용되기를 기다린다.
/// `deadline`까지 끝나지 않으면 `Timeout`이다.
async fn confirm_leader(
    raft: &Raft<TypeConfig>,
    deadline: Instant,
) -> Result<Option<LogId<NodeId>>, ClusterError> {
    match tokio::time::timeout_at(deadline, raft.ensure_linearizable()).await {
        Err(_) => Err(ClusterError::Timeout),
        Ok(Err(RaftError::APIError(CheckIsLeaderError::ForwardToLeader(f)))) => {
            Err(ClusterError::NotLeader {
                leader_id: f.leader_id,
                leader_addr: f.leader_node.map(|n| n.addr),
            })
        }
        Ok(Err(e)) => Err(ClusterError::Raft(e.to_string())),
        Ok(Ok(read)) => Ok(read),
    }
}
```

`crates/opl-cluster/src/lib.rs`에서

```rust
mod leader;
mod log_store;
```

을 다음으로 바꾼다:

```rust
mod leader;
mod lease;
mod log_store;
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub use leader::{CATCH_UP_TIMEOUT, Forwarder, LeaderError};
pub use log_store::LogStore;
```

을 다음으로 바꾼다:

```rust
pub use leader::{CATCH_UP_TIMEOUT, Forwarder, LeaderError};
pub use lease::Leases;
pub use log_store::LogStore;
```

`crates/opl-cluster/src/memnet.rs` 전체:

```rust
//! 한 프로세스 안의 노드들을 잇는 Raft 네트워크와 리더 전달자. 클러스터 테스트에 쓴다.
//! 노드 간 실제 통신(gRPC + mTLS)은 `opl-peer`에 있다.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, Raft};

use crate::cluster::{Cluster, ClusterError};
use crate::leader::{self, Forwarder, LeaderCtx};
use crate::types::{LeaderRequest, NodeId, TypeConfig};

/// 전달받은 요청을 리더가 처리하는 최대 시간
const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Routes {
    nodes: HashMap<NodeId, LeaderCtx>,
    isolated: HashSet<NodeId>,
    /// Raft RPC만 끊긴 노드. 리더 전달은 된다.
    raft_blocked: HashSet<NodeId>,
}

/// 노드 등록부. 등록되지 않았거나 격리된 노드로 가는 RPC는 `Unreachable`이 된다.
#[derive(Clone, Default)]
pub struct MemRouter {
    routes: Arc<Mutex<Routes>>,
}

impl MemRouter {
    /// `from` 노드가 쓸 네트워크. `Cluster::open`에 넘긴다.
    pub fn network(&self, from: NodeId) -> MemNetwork {
        MemNetwork {
            from,
            router: self.clone(),
        }
    }

    /// `from` 노드가 쓸 리더 전달자. `Cluster::with_forwarder`에 넘긴다.
    pub fn forwarder(&self, from: NodeId) -> Arc<MemForwarder> {
        Arc::new(MemForwarder {
            from,
            router: self.clone(),
        })
    }

    pub fn register(&self, id: NodeId, cluster: &Cluster) {
        self.routes
            .lock()
            .unwrap()
            .nodes
            .insert(id, cluster.leader_ctx());
    }

    pub fn unregister(&self, id: NodeId) {
        self.routes.lock().unwrap().nodes.remove(&id);
    }

    /// 노드를 네트워크에서 끊는다. 그 노드로 가거나 그 노드에서 나가는 RPC가 모두 실패한다.
    pub fn isolate(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.insert(id);
    }

    /// 그 노드로 가거나 나가는 Raft RPC만 끊는다. 리더 전달은 그대로 된다(복제가 늦은 노드).
    pub fn block_raft(&self, id: NodeId) {
        self.routes.lock().unwrap().raft_blocked.insert(id);
    }

    pub fn heal(&self, id: NodeId) {
        let mut routes = self.routes.lock().unwrap();
        routes.isolated.remove(&id);
        routes.raft_blocked.remove(&id);
    }

    fn route(&self, from: NodeId, to: NodeId) -> Result<LeaderCtx, Unreachable> {
        let routes = self.routes.lock().unwrap();
        if routes.isolated.contains(&from) || routes.isolated.contains(&to) {
            return Err(unreachable("isolated"));
        }
        routes
            .nodes
            .get(&to)
            .cloned()
            .ok_or_else(|| unreachable("not registered"))
    }

    fn raft_route(&self, from: NodeId, to: NodeId) -> Result<Raft<TypeConfig>, Unreachable> {
        let ctx = self.route(from, to)?;
        let routes = self.routes.lock().unwrap();
        if routes.raft_blocked.contains(&from) || routes.raft_blocked.contains(&to) {
            return Err(unreachable("raft blocked"));
        }
        Ok(ctx.raft)
    }
}

fn unreachable(why: &str) -> Unreachable {
    Unreachable::new(&std::io::Error::other(why.to_string()))
}

pub struct MemForwarder {
    from: NodeId,
    router: MemRouter,
}

#[async_trait]
impl Forwarder for MemForwarder {
    async fn forward(
        &self,
        leader: NodeId,
        _addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let ctx = self
            .router
            .route(self.from, leader)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        leader::execute(&ctx, req, FORWARD_TIMEOUT).await
    }
}

pub struct MemNetwork {
    from: NodeId,
    router: MemRouter,
}

impl RaftNetworkFactory<TypeConfig> for MemNetwork {
    type Network = MemConnection;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> MemConnection {
        MemConnection {
            from: self.from,
            target,
            router: self.router.clone(),
        }
    }
}

pub struct MemConnection {
    from: NodeId,
    target: NodeId,
    router: MemRouter,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for MemConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let raft = self.router.raft_route(self.from, self.target)?;
        raft.append_entries(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let raft = self.router.raft_route(self.from, self.target)?;
        raft.install_snapshot(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let raft = self.router.raft_route(self.from, self.target)?;
        raft.vote(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
```

`crates/opl-cluster/src/types.rs`에서

```rust
        holder: Uuid,
    },
}
```

을 다음으로 바꾼다:

```rust
        holder: Uuid,
    },
    /// 리더가 이 노드를 펜싱하는 중이라 리스를 갱신해 주지 않는다(설계 §4.2).
    #[error("node {node} is being fenced")]
    Fenced { node: NodeId },
}
```

`crates/opl-cluster/src/types.rs`에서

```rust
    Remove {
        id: NodeId,
    },
}
```

을 다음으로 바꾼다:

```rust
    Remove {
        id: NodeId,
    },
    /// 노드의 HA 리스를 갱신한다. 리더는 과반수의 리더인지 확인한 뒤 받은 시각을 적고 자기가
    /// 적용한 마지막 로그 인덱스를 돌려준다. 로그에는 아무것도 쓰지 않는다.
    RenewLease {
        node: NodeId,
    },
}
```

`crates/opl-cluster/src/types.rs`에서

```rust
    }

    #[test]
    fn rejection_message_uses_wire_kind_name() {
```

을 다음으로 바꾼다:

```rust
    }

    /// 노드 사이로 오가는 형식이다. 리스 갱신과 그 거부.
    #[test]
    fn lease_wire_format_is_stable() {
        let node = NodeId::from(2);
        let req = LeaderRequest::RenewLease { node };
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value,
            json!({"op": "renew-lease", "node": node.to_string()})
        );
        assert_eq!(serde_json::from_value::<LeaderRequest>(value).unwrap(), req);

        let r = Rejection::Fenced { node };
        let value = serde_json::to_value(&r).unwrap();
        assert_eq!(value, json!({"reason": "fenced", "node": node.to_string()}));
        assert_eq!(serde_json::from_value::<Rejection>(value).unwrap(), r);
    }

    #[test]
    fn rejection_message_uses_wire_kind_name() {
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
        };
        self.router.register(id, cluster.raft().clone());
        self.nodes.insert(id, cluster);
```

을 다음으로 바꾼다:

```rust
        };
        self.router.register(id, &cluster);
        self.nodes.insert(id, cluster);
```

`crates/opl-cluster/tests/lease.rs`를 만든다:

```rust
//! 리더의 HA 리스 표: 갱신, 고립된 노드와 리더, 펜싱 중 거부, 임기가 바뀌면 비우기.

mod common;

use std::time::Duration;

use common::{TestCluster, WAIT, volume};
use openraft::ServerState;
use opl_cluster::ClusterError;
use opl_cluster::types::{NodeId, Rejection};
use opl_types::storage::VolumeSpec;
use tokio::time::Instant;

/// 실패해야 하는 갱신에 주는 제한 시간. 선거 제한 시간(1초 이상)보다 짧아 그 사이 재선거가 없다.
const SHORT: Duration = Duration::from_millis(300);

fn follower_of(tc: &TestCluster, leader: NodeId) -> NodeId {
    *tc.nodes.keys().find(|id| **id != leader).unwrap()
}

fn term(tc: &TestCluster, id: NodeId) -> u64 {
    tc.get(id).raft().metrics().borrow().current_term
}

#[tokio::test]
async fn leader_records_renewals_from_followers_and_itself() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    let before = Instant::now();
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    tc.get(leader).renew_lease(WAIT).await.unwrap();
    let after = Instant::now();

    let leases = tc.get(leader).leases();
    assert_eq!(leases.term, term(&tc, leader));
    assert_eq!(leases.renewed.len(), 2, "{leases:?}");
    for id in [follower, leader] {
        let r = leases.renewed[&id];
        assert!(
            before <= r && r <= after,
            "{id}: {r:?} not in renewal window"
        );
    }
    // 팔로워는 표를 갖지 않는다(갱신은 리더가 받는다).
    assert!(tc.get(follower).leases().renewed.is_empty());
}

/// 응답 인덱스가 이 노드에 적용되어야 성공이다. 복제가 막힌 노드는 리더에 닿아도 실패한다.
#[tokio::test]
async fn renewal_waits_until_the_leaders_index_is_applied_here() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    tc.router.block_raft(follower);
    let v = volume("moved-while-away");
    tc.get(leader).create(&v).await.unwrap();
    let err = tc.get(follower).renew_lease(SHORT).await.unwrap_err();
    assert!(matches!(err, ClusterError::Timeout), "got {err:?}");
    // 리더는 갱신을 받았지만 노드는 그 사이의 쓰기를 아직 보지 못했으므로 성공이 아니다.
    assert!(tc.get(leader).leases().renewed.contains_key(&follower));
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_none());

    tc.router.heal(follower);
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    // 성공했다면 그 사이 리더가 쓴 것이 이미 보인다.
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_some());
}

#[tokio::test]
async fn isolated_follower_cannot_renew() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);

    tc.router.isolate(follower);
    let err = tc.get(follower).renew_lease(SHORT).await.unwrap_err();
    assert!(matches!(err, ClusterError::Timeout), "got {err:?}");
    assert!(!tc.get(leader).leases().renewed.contains_key(&follower));
}

/// 과반수와 끊긴 리더는 아직 자기를 리더로 알지만 자기 리스를 갱신하지 못한다.
#[tokio::test]
async fn isolated_leader_cannot_renew_its_own_lease() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    tc.get(leader).renew_lease(WAIT).await.unwrap();
    let first = tc.get(leader).leases().renewed[&leader];

    tc.router.isolate(leader);
    assert_eq!(
        tc.get(leader).raft().metrics().borrow().state,
        ServerState::Leader
    );
    assert!(tc.get(leader).renew_lease(SHORT).await.is_err());
    assert_eq!(tc.get(leader).leases().renewed[&leader], first);
}

#[tokio::test]
async fn fenced_node_is_rejected_until_the_mark_is_cleared() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    let follower = follower_of(&tc, leader);
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    let first = tc.get(leader).leases().renewed[&follower];

    tc.get(leader).begin_fence(follower);
    let err = tc.get(follower).renew_lease(WAIT).await.unwrap_err();
    assert!(
        matches!(err, ClusterError::Rejected(Rejection::Fenced { node }) if node == follower),
        "got {err:?}"
    );
    assert_eq!(tc.get(leader).leases().renewed[&follower], first);
    // 다른 노드는 그대로 갱신한다.
    tc.get(leader).renew_lease(WAIT).await.unwrap();

    tc.get(leader).end_fence(follower);
    tc.get(follower).renew_lease(WAIT).await.unwrap();
    assert!(tc.get(leader).leases().renewed[&follower] > first);
}

/// 리더십을 잃었다가 더 높은 임기에 다시 얻은 리더는 이전 임기의 갱신을 쓰지 않는다.
#[tokio::test]
async fn table_is_cleared_when_the_term_changes() {
    let tc = TestCluster::start_forwarding(3).await;
    let a = tc.leader().await;
    let others: Vec<_> = tc.nodes.keys().copied().filter(|id| *id != a).collect();
    let (b, c) = (others[0], others[1]);
    tc.get(b).renew_lease(WAIT).await.unwrap();
    let old_term = term(&tc, a);
    assert!(tc.get(a).leases().renewed.contains_key(&b));

    // 선거에 나설 수 있는 노드를 하나씩만 두어 리더를 a → b → a로 정해서 바꾼다.
    tc.get(c).raft().runtime_config().elect(false);
    tc.router.isolate(a);
    assert_eq!(tc.leader_among(&[b, c]).await, b);
    tc.get(b).raft().runtime_config().elect(false);
    tc.router.heal(a);
    // a가 b의 로그를 받아야 c가 a에게 투표한다.
    assert_eq!(tc.leader().await, b);
    let last = tc.get(b).raft().metrics().borrow().last_log_index.unwrap();
    tc.wait_applied(a, last).await;
    tc.router.isolate(b);
    assert_eq!(tc.leader_among(&[a, c]).await, a);

    // 다시 리더가 된 a의 표에는 이전 임기의 b 갱신이 없다.
    tc.get(c).renew_lease(WAIT).await.unwrap();
    let leases = tc.get(a).leases();
    assert!(leases.term > old_term, "{leases:?}");
    assert_eq!(leases.term, term(&tc, a));
    assert_eq!(
        leases.renewed.keys().copied().collect::<Vec<_>>(),
        vec![c],
        "{leases:?}"
    );
}
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
    };
    router.register(id, cluster.raft().clone());
    Arc::new(cluster)
```

을 다음으로 바꾼다:

```rust
    };
    router.register(id, &cluster);
    Arc::new(cluster)
```

`crates/opld/src/bootstrap.rs`에서

```rust
            .unwrap();
        router.register(id, cluster.raft().clone());
        let members = BTreeMap::from([(id, BasicNode::new(id.to_string()))]);
```

을 다음으로 바꾼다:

```rust
            .unwrap();
        router.register(id, &cluster);
        let members = BTreeMap::from([(id, BasicNode::new(id.to_string()))]);
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 780개, 실패 0

Run: `cargo test -p opl-cluster --test lease`
Expected: PASS 6개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(cluster): 리더의 HA 리스 표 — RenewLease, Rejection::Fenced"
```

---

### Task 3: 노드의 HaGuard와 watchdog

opl-reconciler `ha.rs`: `HaGuard::spawn(renewer, HaConfig)`는 갱신 간격마다 리스를 갱신하고(보내기 전 시각, 한 번의 제한은 간격의 두 배), `fresh()`(이 프로세스에서 갱신했고 마지막 성공한 갱신을 보낸 시각부터 `fence_after` 안), `stale()`(시작 시각과 마지막 갱신 중 늦은 것부터 `fence_after` 이상), 바뀜 알림(`subscribe`)을 낸다. `arm()`은 fresh일 때만(같은 잠금) watchdog 장치를 처음 한 번 연다(`DeviceWatchdog`: Linux에서 `WDIOC_SETTIMEOUT`, 거절하거나 더 긴 값이면 `V` 없이 "쥐었지만 쓸 수 없는" 장치로 두고 `ArmError::Rejected`). 1초마다 리스가 살아 있거나 도는 HA VM이 없을 때만 쓰다듬고(`set_running`, `retain_running`), `close()`는 도는 HA VM이 없으면 magic close. `ReconcilerConfig.ha`, opld가 `[ha]`로 guard를 만들어 넣고(위트니스·off는 없음) 종료 때 reconciler 뒤에 닫는다. 시험은 멈춘 tokio 시계와 가짜 장치(진짜 `/dev/watchdog`를 열지 않는다).

**Files:**
- Create: `crates/opl-reconciler/src/ha.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-reconciler/Cargo.toml`
- Modify: `crates/opl-reconciler/src/config.rs`
- Modify: `crates/opl-reconciler/src/lib.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/tests/common/mod.rs`에서

```rust
        firmware: None,
    }
```

을 다음으로 바꾼다:

```rust
        firmware: None,
        ha: None,
    }
```

`crates/opl-reconciler/Cargo.toml`에서

```toml
opl-types.workspace = true
thiserror.workspace = true
```

을 다음으로 바꾼다:

```toml
opl-types.workspace = true
rustix.workspace = true
thiserror.workspace = true
```

`crates/opl-reconciler/Cargo.toml`에서

```toml
tempfile.workspace = true
```

을 다음으로 바꾼다:

```toml
tempfile.workspace = true
tokio = { workspace = true, features = ["test-util"] }
```

`crates/opl-reconciler/src/config.rs`에서

```rust
use crate::firmware::FirmwareFetcher;
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use crate::firmware::FirmwareFetcher;
use crate::ha::HaGuard;
use uuid::Uuid;
```

`crates/opl-reconciler/src/config.rs`에서

```rust
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
}
```

을 다음으로 바꾼다:

```rust
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
    /// 이 노드의 HA 리스와 watchdog. None이면 펜싱이 꺼져 있어 HA VM을 켜지 않는다.
    pub ha: Option<Arc<HaGuard>>,
}
```

`crates/opl-reconciler/src/config.rs`에서

```rust
            firmware: None,
        }
```

을 다음으로 바꾼다:

```rust
            firmware: None,
            ha: None,
        }
```

`crates/opl-reconciler/src/ha.rs`를 만든다:

```rust
//! 노드 쪽 HA 펜싱(`HaGuard`). 리더에게 리스를 갱신하고, 리스가 살아 있는지(`fresh`)와 끝났는지
//! (`stale`)를 알려 주고, watchdog 장치를 쓰다듬는다. 규칙은 설계 문서
//! (2026-11-02-ha-fencing-witness-design.md) §3.2·§3.3을 따른다.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::{Cluster, ClusterError};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;

/// watchdog를 쓰다듬는 간격
const PING_INTERVAL: Duration = Duration::from_secs(1);

/// 리더에게 이 노드의 리스를 갱신한다. 시험은 가짜를 쓴다.
#[async_trait]
pub trait LeaseRenewer: Send + Sync {
    /// `timeout` 안에 리더가 갱신을 확인하고 그 인덱스까지 이 노드에 적용되면 성공이다.
    async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError>;
}

#[async_trait]
impl LeaseRenewer for Cluster {
    async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError> {
        Cluster::renew_lease(self, timeout).await
    }
}

/// 연 watchdog 장치. 쓰다듬지 않고 제한 시간이 지나면 노드가 재시작된다.
pub trait Watchdog: Send {
    fn ping(&mut self) -> io::Result<()>;
    /// `V`를 쓰고 닫아 타이머를 멈춘다(magic close). 커널이 `nowayout`이면 멈추지 않는다.
    fn magic_close(self: Box<Self>) -> io::Result<()>;
    /// 장치가 제한 시간을 받아들이지 않았으면 그 이유. 그런 장치도 닫지 않고 쥐어 평소처럼 쓰다듬지만
    /// HA VM에는 쓰지 않는다(`arm` 실패).
    fn rejected(&self) -> Option<&str> {
        None
    }
}

/// watchdog 장치를 연다. 열면 장치의 타이머가 (다시) 돈다.
pub trait WatchdogOpener: Send + Sync {
    fn open(&self) -> io::Result<Box<dyn Watchdog>>;
}

/// Linux watchdog 장치(`/dev/watchdog`, 하드웨어 또는 `softdog`). 다른 OS에서는 열기가 실패한다.
#[derive(Debug, Clone)]
pub struct DeviceWatchdog {
    pub path: PathBuf,
    /// 장치에 정하는 제한 시간(초, `WDIOC_SETTIMEOUT`)
    pub timeout_secs: u64,
}

/// `WDIOC_SETTIMEOUT` = `_IOWR('W', 6, int)`
const WDIOC_SETTIMEOUT: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read_write::<std::ffi::c_int>(b'W', 6);
/// `WDIOC_GETTIMEOUT` = `_IOR('W', 7, int)`
const WDIOC_GETTIMEOUT: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read::<std::ffi::c_int>(b'W', 7);

impl WatchdogOpener for DeviceWatchdog {
    fn open(&self) -> io::Result<Box<dyn Watchdog>> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "watchdog devices need Linux",
            ));
        }
        let mut timeout = std::ffi::c_int::try_from(self.timeout_secs).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "watchdog timeout too large")
        })?;
        let fd = rustix::fs::open(
            &self.path,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        // SAFETY: WDIOC_SETTIMEOUT는 int 하나를 읽고, 장치가 실제로 정한 값을 그 자리에 쓴다.
        let set = unsafe {
            rustix::ioctl::ioctl(
                &fd,
                rustix::ioctl::Updater::<WDIOC_SETTIMEOUT, std::ffi::c_int>::new(&mut timeout),
            )
        }
        .map(|()| timeout)
        .map_err(io::Error::from);
        // SAFETY: WDIOC_GETTIMEOUT는 지금 제한 시간을 int 하나에 쓴다.
        let get = || unsafe {
            rustix::ioctl::ioctl(
                &fd,
                rustix::ioctl::Getter::<WDIOC_GETTIMEOUT, std::ffi::c_int>::new(),
            )
            .map_err(io::Error::from)
        };
        // 받아들이지 못해도 닫지 않고 쥐어 둔다. 열면 타이머가 돌므로 그냥 닫으면 나중에 노드가 재시작되고,
        // 열기는 리스가 살아 있을 때만 하므로 쥔 채 쓰다듬고 정상 종료 때 멈춰도 된다(`HaGuard`).
        let actual = match accept_timeout(self.timeout_secs, set, get) {
            Ok(actual) => actual,
            Err(e) => {
                let reason = format!(
                    "cannot set the watchdog timeout to {}s: {e}",
                    self.timeout_secs
                );
                return Ok(Box::new(Device {
                    fd,
                    rejected: Some(reason),
                }));
            }
        };
        if actual != self.timeout_secs {
            tracing::warn!(
                device = %self.path.display(),
                requested = self.timeout_secs,
                actual,
                "watchdog device chose a shorter timeout"
            );
        }
        Ok(Box::new(Device { fd, rejected: None }))
    }
}

/// 장치의 제한 시간을 받아들일지 정한다. `set`은 `WDIOC_SETTIMEOUT`의 결과(장치가 정한 값)이고,
/// 실패했으면 `get`(`WDIOC_GETTIMEOUT`)으로 지금 값을 읽는다. 요청보다 길면 리스가 끝난 뒤에도 노드가
/// 늦게 재시작되어 리더의 복구와 겹칠 수 있으므로 받아들이지 않는다.
fn accept_timeout(
    requested: u64,
    set: io::Result<std::ffi::c_int>,
    get: impl FnOnce() -> io::Result<std::ffi::c_int>,
) -> io::Result<u64> {
    let ok = |t: std::ffi::c_int| {
        u64::try_from(t)
            .ok()
            .filter(|t| (1..=requested).contains(t))
    };
    match set {
        Ok(t) => ok(t).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the device chose {t}s"),
            )
        }),
        Err(e) => get().ok().and_then(ok).ok_or(e),
    }
}

struct Device {
    fd: rustix::fd::OwnedFd,
    /// 제한 시간을 받아들이지 않은 이유
    rejected: Option<String>,
}

impl Watchdog for Device {
    fn ping(&mut self) -> io::Result<()> {
        rustix::io::write(&self.fd, b"\0")?;
        Ok(())
    }

    fn magic_close(self: Box<Self>) -> io::Result<()> {
        rustix::io::write(&self.fd, b"V")?;
        Ok(())
    }

    fn rejected(&self) -> Option<&str> {
        self.rejected.as_deref()
    }
}

/// `HaGuard` 설정. opld가 `[ha]`에서 만든다.
#[derive(Clone)]
pub struct HaConfig {
    /// 마지막으로 성공한 갱신(보내기 전 시각) 뒤 이만큼 지나면 리스가 끝난다.
    pub fence_after: Duration,
    /// 갱신 간격. 한 번의 갱신은 이 두 배까지 기다린다.
    pub renew_interval: Duration,
    /// watchdog 장치. None이면 `process` 펜싱(워커가 끄는 것만)이다.
    pub watchdog: Option<Arc<dyn WatchdogOpener>>,
}

/// 리스 상태. 바뀌면 [`HaGuard::subscribe`]로 알린다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaseState {
    /// 이 프로세스에서 갱신에 성공했고 마지막 갱신 뒤 `fence_after`가 지나지 않았다.
    pub fresh: bool,
    /// 시작이나 마지막 갱신 중 늦은 것에서 `fence_after`가 지났다. HA VM을 꺼야 한다.
    pub stale: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ArmError {
    #[error("the HA lease is not fresh")]
    NotFresh,
    #[error("cannot open the watchdog device: {0}")]
    Open(io::Error),
    #[error("the watchdog device is unusable: {0}")]
    Rejected(String),
}

struct State {
    /// 마지막으로 성공한 갱신을 보내기 전 시각
    last_renewed: Option<Instant>,
    /// 이 노드에서 도는 HA VM
    running: HashSet<Uuid>,
    watchdog: Option<Box<dyn Watchdog>>,
}

struct Shared {
    /// 기준점. 다시 시작한 opld가 갱신하기 전에 곧바로 끄지 않게 한다.
    start: Instant,
    fence_after: Duration,
    state: Mutex<State>,
    tx: watch::Sender<LeaseState>,
    /// 갱신에 성공하면 알린다(상태가 바뀔 다음 시각이 늦춰진다).
    renewed: Notify,
}

impl Shared {
    fn lease(&self, state: &State, now: Instant) -> LeaseState {
        let fresh = state
            .last_renewed
            .is_some_and(|s| now.saturating_duration_since(s) < self.fence_after);
        let base = state.last_renewed.map_or(self.start, |s| s.max(self.start));
        LeaseState {
            fresh,
            stale: now.saturating_duration_since(base) >= self.fence_after,
        }
    }

    /// 지금 상태를 알리고, 갱신 없이 상태가 바뀔 다음 시각을 돌려준다. 이미 stale이면 None이다.
    fn publish(&self) -> Option<Instant> {
        let state = self.state.lock().unwrap();
        let now = Instant::now();
        let lease = self.lease(&state, now);
        self.tx.send_if_modified(|old| {
            let changed = *old != lease;
            *old = lease;
            changed
        });
        // 갱신했으면 마지막 갱신이 기준점보다 늦으므로 fresh와 stale이 같은 때 바뀐다.
        let next = state.last_renewed.map_or(self.start, |s| s.max(self.start)) + self.fence_after;
        (next > now).then_some(next)
    }
}

/// 노드의 HA 리스와 watchdog. 갱신 루프와 쓰다듬기 루프를 뒤에서 돌린다. 정상 종료 때 [`close`]한다.
///
/// [`close`]: HaGuard::close
pub struct HaGuard {
    shared: Arc<Shared>,
    opener: Option<Arc<dyn WatchdogOpener>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl HaGuard {
    /// 지금을 기준점으로 삼고 곧바로 첫 갱신을 보낸다.
    pub fn spawn(renewer: Arc<dyn LeaseRenewer>, config: HaConfig) -> Arc<Self> {
        let (tx, _) = watch::channel(LeaseState::default());
        let shared = Arc::new(Shared {
            start: Instant::now(),
            fence_after: config.fence_after,
            state: Mutex::new(State {
                last_renewed: None,
                running: HashSet::new(),
                watchdog: None,
            }),
            tx,
            renewed: Notify::new(),
        });
        let tasks = vec![
            tokio::spawn(renew_loop(shared.clone(), renewer, config.renew_interval)),
            tokio::spawn(watch_loop(shared.clone())),
        ];
        Arc::new(Self {
            shared,
            opener: config.watchdog,
            tasks: Mutex::new(tasks),
        })
    }

    pub fn state(&self) -> LeaseState {
        let state = self.shared.state.lock().unwrap();
        self.shared.lease(&state, Instant::now())
    }

    /// 리스가 살아 있다. HA VM을 켜도 된다.
    pub fn fresh(&self) -> bool {
        self.state().fresh
    }

    /// 리스가 끝났다. 도는 HA VM을 꺼야 한다.
    pub fn stale(&self) -> bool {
        self.state().stale
    }

    /// fresh·stale이 바뀔 때마다 알린다.
    pub fn subscribe(&self) -> watch::Receiver<LeaseState> {
        self.shared.tx.subscribe()
    }

    /// HA VM을 켜거나 도는 HA VM에 붙기 전에 watchdog를 연다(이미 열었으면 그대로). 리스가 지금 살아
    /// 있을 때(`fresh`)만 성공한다. 이전 프로세스가 켜 둔 타이머를 새 리스 없이 늘리면 안 된다.
    /// watchdog가 없으면(`process`) 리스만 본다. 제한 시간을 받아들이지 않은 장치는 쥔 채(다시 열지
    /// 않는다) 실패한다.
    pub fn arm(&self) -> Result<(), ArmError> {
        let mut state = self.shared.state.lock().unwrap();
        if !self.shared.lease(&state, Instant::now()).fresh {
            return Err(ArmError::NotFresh);
        }
        if let (Some(opener), None) = (&self.opener, &state.watchdog) {
            let watchdog = opener.open().map_err(ArmError::Open)?;
            match watchdog.rejected() {
                Some(reason) => {
                    tracing::warn!(
                        reason,
                        "watchdog opened but unusable; HA VMs will not start"
                    )
                }
                None => tracing::info!("watchdog armed"),
            }
            state.watchdog = Some(watchdog);
        }
        match state.watchdog.as_ref().and_then(|w| w.rejected()) {
            Some(reason) => Err(ArmError::Rejected(reason.to_owned())),
            None => Ok(()),
        }
    }

    /// 이 노드에서 HA VM `vm`이 도는지 알린다. 리스가 끝났는데 도는 HA VM이 있으면 쓰다듬지 않는다.
    pub fn set_running(&self, vm: Uuid, running: bool) {
        let mut state = self.shared.state.lock().unwrap();
        if running {
            state.running.insert(vm);
        } else {
            state.running.remove(&vm);
        }
    }

    /// 갱신과 쓰다듬기를 멈춘다. 도는 HA VM이 없으면 watchdog를 magic close로 멈추고 닫는다. 있으면
    /// 그대로 두어 opld가 제한 시간 안에 돌아오지 않으면 노드가 재시작된다.
    pub fn close(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        let mut state = self.shared.state.lock().unwrap();
        let Some(watchdog) = state.watchdog.take() else {
            return;
        };
        if state.running.is_empty() {
            if let Err(e) = watchdog.magic_close() {
                tracing::warn!(error = %e, "cannot stop the watchdog");
            }
        } else {
            tracing::warn!(
                vms = state.running.len(),
                "HA VMs are running; leaving the watchdog armed, so this node restarts unless opld comes back in time"
            );
            drop(watchdog);
        }
    }
}

impl Drop for HaGuard {
    fn drop(&mut self) {
        for task in self.tasks.get_mut().unwrap().drain(..) {
            task.abort();
        }
    }
}

/// 갱신 간격마다 리스를 갱신한다. 한 번은 간격의 두 배까지 기다린다.
async fn renew_loop(shared: Arc<Shared>, renewer: Arc<dyn LeaseRenewer>, interval: Duration) {
    let limit = interval * 2;
    let mut failing = false;
    loop {
        // 보내기 전에 잰다. 리더가 받은 시각은 이보다 늦다(설계 §3.2).
        let sent = Instant::now();
        let result = tokio::time::timeout(limit, renewer.renew_lease(limit))
            .await
            .unwrap_or_else(|_| Err(ClusterError::Timeout));
        match result {
            Ok(()) => {
                {
                    let mut state = shared.state.lock().unwrap();
                    state.last_renewed = Some(state.last_renewed.map_or(sent, |s| s.max(sent)));
                }
                shared.publish();
                shared.renewed.notify_one();
                if failing {
                    tracing::info!("HA lease renewed again");
                    failing = false;
                }
            }
            Err(e) => {
                if !failing {
                    tracing::warn!(error = %e, "cannot renew the HA lease");
                    failing = true;
                }
            }
        }
        tokio::time::sleep_until(sent + interval).await;
    }
}

/// 리스 상태가 바뀌는 때에 알리고, 1초마다 watchdog를 쓰다듬는다. 리스가 살아 있거나 도는 HA VM이
/// 없을 때만 쓰다듬는다.
async fn watch_loop(shared: Arc<Shared>) {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut holding = false;
    loop {
        let next = shared.publish();
        tokio::select! {
            () = async {
                match next {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {}
            () = shared.renewed.notified() => {}
            _ = ping.tick() => {
                let mut guard = shared.state.lock().unwrap();
                let state = &mut *guard;
                let fresh = shared.lease(state, Instant::now()).fresh;
                let Some(watchdog) = state.watchdog.as_mut() else {
                    continue;
                };
                if fresh || state.running.is_empty() {
                    holding = false;
                    if let Err(e) = watchdog.ping() {
                        tracing::warn!(error = %e, "cannot ping the watchdog");
                    }
                } else if !holding {
                    holding = true;
                    tracing::warn!("HA lease lost with HA VMs running; no longer pinging the watchdog");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FENCE_AFTER: Duration = Duration::from_secs(10);
    const INTERVAL: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    /// 정해 둔 대로 갱신에 성공하거나 실패하고, 보낸 시각과 제한 시간을 남긴다.
    #[derive(Default)]
    struct FakeRenewer {
        ok: Mutex<bool>,
        /// 응답까지 걸리는 시간. None이면 끝나지 않는다.
        delay: Mutex<Option<Duration>>,
        /// 받은 갱신(보낸 시각, 제한 시간)
        calls: Mutex<Vec<(Instant, Duration)>>,
        /// 성공한 갱신을 보낸 시각
        oks: Mutex<Vec<Instant>>,
    }

    impl FakeRenewer {
        fn new(ok: bool) -> Arc<Self> {
            let r = Self::default();
            *r.ok.lock().unwrap() = ok;
            *r.delay.lock().unwrap() = Some(Duration::ZERO);
            Arc::new(r)
        }

        fn set_ok(&self, ok: bool) {
            *self.ok.lock().unwrap() = ok;
        }

        /// 마지막으로 성공한 갱신을 보낸 시각
        fn last_ok(&self) -> Instant {
            *self.oks.lock().unwrap().last().unwrap()
        }
    }

    #[async_trait]
    impl LeaseRenewer for FakeRenewer {
        async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError> {
            let sent = Instant::now();
            self.calls.lock().unwrap().push((sent, timeout));
            let delay = *self.delay.lock().unwrap();
            match delay {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
            if *self.ok.lock().unwrap() {
                self.oks.lock().unwrap().push(sent);
                Ok(())
            } else {
                Err(ClusterError::Timeout)
            }
        }
    }

    /// 장치 대신 한 일을 적는다.
    #[derive(Default)]
    struct FakeWatchdog {
        log: Arc<Mutex<Vec<&'static str>>>,
        fail_open: Mutex<bool>,
        /// 열린 장치가 제한 시간을 받아들이지 않은 이유
        reject: Mutex<Option<&'static str>>,
    }

    impl FakeWatchdog {
        fn log(&self) -> Vec<&'static str> {
            self.log.lock().unwrap().clone()
        }

        fn count(&self, what: &str) -> usize {
            self.log().iter().filter(|e| **e == what).count()
        }
    }

    struct FakeDevice(Arc<Mutex<Vec<&'static str>>>, Option<&'static str>);

    impl Watchdog for FakeDevice {
        fn ping(&mut self) -> io::Result<()> {
            self.0.lock().unwrap().push("ping");
            Ok(())
        }

        fn magic_close(self: Box<Self>) -> io::Result<()> {
            self.0.lock().unwrap().push("magic_close");
            Ok(())
        }

        fn rejected(&self) -> Option<&str> {
            self.1
        }
    }

    impl Drop for FakeDevice {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("drop");
        }
    }

    impl WatchdogOpener for FakeWatchdog {
        fn open(&self) -> io::Result<Box<dyn Watchdog>> {
            if *self.fail_open.lock().unwrap() {
                return Err(io::Error::new(io::ErrorKind::ResourceBusy, "busy"));
            }
            self.log.lock().unwrap().push("open");
            let reject = *self.reject.lock().unwrap();
            Ok(Box::new(FakeDevice(self.log.clone(), reject)))
        }
    }

    fn guard(renewer: &Arc<FakeRenewer>, watchdog: Option<&Arc<FakeWatchdog>>) -> Arc<HaGuard> {
        HaGuard::spawn(
            renewer.clone(),
            HaConfig {
                fence_after: FENCE_AFTER,
                renew_interval: INTERVAL,
                watchdog: watchdog.map(|w| w.clone() as Arc<dyn WatchdogOpener>),
            },
        )
    }

    async fn sleep_until(at: Instant) {
        tokio::time::sleep_until(at).await;
    }

    /// 한 번도 갱신하지 못하면 fresh가 아니고, 시작에서 fence_after가 지나야 stale이다.
    #[tokio::test(start_paused = true)]
    async fn without_renewal_stale_starts_counting_from_the_start() {
        let start = Instant::now();
        let renewer = FakeRenewer::new(false);
        let g = guard(&renewer, None);
        let mut rx = g.subscribe();
        assert_eq!(g.state(), LeaseState::default());
        sleep_until(start + FENCE_AFTER - MS).await;
        assert!(!g.fresh());
        assert!(!g.stale());
        rx.changed().await.unwrap();
        assert_eq!(Instant::now(), start + FENCE_AFTER);
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: false,
                stale: true
            }
        );
        assert!(g.stale());
        g.close();
    }

    /// 갱신하면 fresh이고, 마지막으로 성공한 갱신을 보낸 시각에서 fence_after가 지나면 stale이다.
    #[tokio::test(start_paused = true)]
    async fn fresh_until_fence_after_from_the_last_renewal_sent() {
        let renewer = FakeRenewer::new(true);
        // 응답에 시간이 걸려도 보낸 시각부터 센다.
        *renewer.delay.lock().unwrap() = Some(Duration::from_millis(300));
        let g = guard(&renewer, None);
        let mut rx = g.subscribe();
        rx.changed().await.unwrap();
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: true,
                stale: false
            }
        );
        // 몇 번 더 갱신한 뒤 끊는다.
        tokio::time::sleep(Duration::from_millis(3500)).await;
        renewer.set_ok(false);
        tokio::time::sleep(INTERVAL).await;
        let last = renewer.last_ok();
        assert!(last > Instant::now() - INTERVAL * 3);
        sleep_until(last + FENCE_AFTER - MS).await;
        assert!(g.fresh());
        assert!(!g.stale());
        rx.changed().await.unwrap();
        assert_eq!(Instant::now(), last + FENCE_AFTER);
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: false,
                stale: true
            }
        );
        // 다시 갱신되면 곧바로 fresh로 알린다.
        renewer.set_ok(true);
        rx.changed().await.unwrap();
        assert!(rx.borrow_and_update().fresh);
        assert!(!g.stale());
        g.close();
    }

    /// 한 번의 갱신은 간격의 두 배까지 기다리고, 끝나지 않으면 다음 갱신을 보낸다.
    #[tokio::test(start_paused = true)]
    async fn a_hung_renewal_times_out_after_twice_the_interval() {
        let renewer = FakeRenewer::new(true);
        *renewer.delay.lock().unwrap() = None;
        let start = Instant::now();
        let g = guard(&renewer, None);
        sleep_until(start + INTERVAL * 2 - MS).await;
        assert_eq!(renewer.calls.lock().unwrap().len(), 1);
        sleep_until(start + INTERVAL * 2 + MS).await;
        assert_eq!(
            *renewer.calls.lock().unwrap(),
            [(start, INTERVAL * 2), (start + INTERVAL * 2, INTERVAL * 2)]
        );
        assert!(!g.fresh());
        // 두 번째도 끝나지 않고, 세 번째는 성공한다.
        *renewer.delay.lock().unwrap() = Some(Duration::ZERO);
        sleep_until(start + INTERVAL * 4 + MS).await;
        assert!(g.fresh());
        assert_eq!(renewer.last_ok(), start + INTERVAL * 4);
        g.close();
    }

    /// 갱신하기 전에는 watchdog를 열지 않는다. 연 뒤에는 다시 열지 않고, 열지 못하면 다음에 다시 연다.
    #[tokio::test(start_paused = true)]
    async fn arm_needs_a_renewal_and_opens_the_watchdog_once() {
        let renewer = FakeRenewer::new(false);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(INTERVAL).await;
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        assert!(watchdog.log().is_empty());
        renewer.set_ok(true);
        tokio::time::sleep(INTERVAL).await;
        *watchdog.fail_open.lock().unwrap() = true;
        assert!(matches!(g.arm(), Err(ArmError::Open(_))));
        *watchdog.fail_open.lock().unwrap() = false;
        g.arm().unwrap();
        g.arm().unwrap();
        assert_eq!(watchdog.count("open"), 1);
        g.close();
    }

    /// 갱신한 적이 있어도 리스가 끝났으면 watchdog를 열지 않는다(리스 없이 타이머를 늘리지 않는다).
    #[tokio::test(start_paused = true)]
    async fn arm_needs_a_fresh_lease() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        let mut rx = g.subscribe();
        rx.wait_for(|s| s.fresh).await.unwrap();
        renewer.set_ok(false);
        rx.wait_for(|s| s.stale).await.unwrap();
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        assert!(watchdog.log().is_empty());
        renewer.set_ok(true);
        rx.wait_for(|s| s.fresh).await.unwrap();
        g.arm().unwrap();
        assert_eq!(watchdog.count("open"), 1);
        g.close();
    }

    /// watchdog가 없으면(process) 갱신했는지만 본다.
    #[tokio::test(start_paused = true)]
    async fn arm_without_a_watchdog_only_needs_a_renewal() {
        let renewer = FakeRenewer::new(false);
        let g = guard(&renewer, None);
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        renewer.set_ok(true);
        tokio::time::sleep(INTERVAL).await;
        g.arm().unwrap();
        g.close();
    }

    /// 리스가 살아 있거나 도는 HA VM이 없을 때만 1초마다 쓰다듬는다.
    #[tokio::test(start_paused = true)]
    async fn pings_only_while_fresh_or_without_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        g.arm().unwrap();
        let vm = Uuid::new_v4();
        g.set_running(vm, true);
        // 리스가 살아 있으면 HA VM이 돌아도 쓰다듬는다.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let pinged = watchdog.count("ping");
        assert!((4..=6).contains(&pinged), "{pinged}");
        // 리스가 끝나면 HA VM이 도는 동안 쓰다듬지 않는다.
        renewer.set_ok(false);
        let mut rx = g.subscribe();
        rx.wait_for(|s| !s.fresh).await.unwrap();
        let before = watchdog.count("ping");
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(watchdog.count("ping"), before);
        // 도는 HA VM이 없으면 리스가 없어도 쓰다듬는다.
        g.set_running(vm, false);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(watchdog.count("ping") >= before + 2);
        g.close();
    }

    /// 정상 종료 때 도는 HA VM이 없으면 magic close를 한다.
    #[tokio::test(start_paused = true)]
    async fn close_stops_the_watchdog_without_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        g.arm().unwrap();
        let vm = Uuid::new_v4();
        g.set_running(vm, true);
        g.set_running(vm, false);
        g.close();
        let log = watchdog.log();
        assert_eq!(&log[log.len() - 2..], ["magic_close", "drop"]);
        // 닫은 뒤에는 쓰다듬지 않는다.
        let pinged = watchdog.count("ping");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(watchdog.count("ping"), pinged);
    }

    /// 도는 HA VM이 있으면 magic close 없이 닫아 타이머를 그대로 둔다.
    #[tokio::test(start_paused = true)]
    async fn close_leaves_the_watchdog_running_with_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        g.arm().unwrap();
        g.set_running(Uuid::new_v4(), true);
        g.close();
        assert_eq!(watchdog.log().last(), Some(&"drop"));
        assert_eq!(watchdog.count("magic_close"), 0);
    }

    /// 제한 시간을 받아들이지 않은 장치도 닫지 않고 쥔다: 그냥 닫으면 커널 타이머가 남아 나중에 노드가
    /// 재시작된다. HA VM에는 쓰지 않고(`arm` 실패, 다시 열지 않는다) 평소 규칙대로 쓰다듬으며, 정상 종료
    /// 때 도는 HA VM이 없으면 magic close로 멈춘다.
    #[tokio::test(start_paused = true)]
    async fn a_rejected_watchdog_is_held_pinged_and_magic_closed() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        *watchdog.reject.lock().unwrap() = Some("the device chose 60s");
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        let Err(e) = g.arm() else {
            panic!("armed a rejected watchdog");
        };
        assert!(matches!(e, ArmError::Rejected(_)), "{e:?}");
        assert!(e.to_string().contains("the device chose 60s"), "{e}");
        assert!(matches!(g.arm(), Err(ArmError::Rejected(_))));
        assert_eq!(watchdog.log(), ["open"]);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(watchdog.count("ping") >= 2);
        g.close();
        let log = watchdog.log();
        assert_eq!(&log[log.len() - 2..], ["magic_close", "drop"]);
    }

    /// 실제 장치 구현은 watchdog가 아닌 파일을 받아들이지 않는다: Linux는 ioctl이 실패해 쓸 수 없는
    /// 장치로 쥐고(`V`를 쓰지 않는다), 다른 OS는 열기가 실패한다. 진짜 `/dev/watchdog`는 열지 않는다.
    #[test]
    fn device_watchdog_rejects_a_regular_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let device = DeviceWatchdog {
            path: file.path().into(),
            timeout_secs: 30,
        };
        match (cfg!(target_os = "linux"), device.open()) {
            (true, Ok(watchdog)) => {
                let reason = watchdog.rejected().expect("accepted a regular file");
                assert!(
                    reason.starts_with("cannot set the watchdog timeout to 30s"),
                    "{reason}"
                );
            }
            (false, Err(e)) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            (linux, opened) => panic!("linux: {linux}, opened: {}", opened.is_ok()),
        }
        assert_eq!(std::fs::read(file.path()).unwrap(), b"");
    }

    fn einval() -> io::Error {
        io::Error::from_raw_os_error(22)
    }

    /// 장치가 정한 제한 시간이 요청 이하일 때만 받아들인다. 정하지 못했으면 지금 값을 읽어 본다.
    #[test]
    fn device_timeout_must_not_exceed_the_request() {
        let unread = || -> io::Result<std::ffi::c_int> { panic!("read the timeout") };
        // 정한 대로이거나 더 짧으면 받아들인다.
        assert_eq!(accept_timeout(30, Ok(30), unread).unwrap(), 30);
        assert_eq!(accept_timeout(30, Ok(20), unread).unwrap(), 20);
        // 더 길게 골랐거나 0이면 안 된다.
        assert!(accept_timeout(30, Ok(60), unread).is_err());
        assert!(accept_timeout(30, Ok(0), unread).is_err());
        // 정하지 못했으면 지금 값이 요청 이하일 때만 받아들인다.
        assert_eq!(accept_timeout(30, Err(einval()), || Ok(10)).unwrap(), 10);
        assert_eq!(accept_timeout(30, Err(einval()), || Ok(30)).unwrap(), 30);
        let e = accept_timeout(30, Err(einval()), || Ok(60)).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(22));
        let e = accept_timeout(30, Err(einval()), || Err(io::Error::other("no"))).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(22));
    }
}
```

`crates/opl-reconciler/src/lib.rs`에서

```rust
mod firmware;
mod reconciler;
```

을 다음으로 바꾼다:

```rust
mod firmware;
pub mod ha;
mod reconciler;
```

`crates/opl-reconciler/src/lib.rs`에서

```rust
pub use firmware::{FirmwareError, FirmwareFetcher};
pub use reconciler::{Reconciler, ReconcilerHandle};
```

을 다음으로 바꾼다:

```rust
pub use firmware::{FirmwareError, FirmwareFetcher};
pub use ha::{HaConfig, HaGuard};
pub use reconciler::{Reconciler, ReconcilerHandle};
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
        firmware: None,
    }
```

을 다음으로 바꾼다:

```rust
        firmware: None,
        ha: None,
    }
```

`crates/opld/src/config.rs`에서

```rust
use std::path::{Path, PathBuf};
use std::time::Duration;
```

을 다음으로 바꾼다:

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
```

`crates/opld/src/config.rs`에서

```rust
use opl_qemu::{Accel, HostProfile, kvm_available};
use opl_types::node::{Fencing, NodeHa};
```

을 다음으로 바꾼다:

```rust
use opl_qemu::{Accel, HostProfile, kvm_available};
use opl_reconciler::HaConfig;
use opl_reconciler::ha::{DeviceWatchdog, WatchdogOpener};
use opl_types::node::{Fencing, NodeHa};
```

`crates/opld/src/config.rs`에서

```rust

// 노드의 `HaGuard`를 만들 때 쓴다. 그 전까지는 시험만 쓴다.
#[cfg_attr(not(test), expect(dead_code))]
impl HaSection {
```

을 다음으로 바꾼다:

```rust

impl HaSection {
```

`crates/opld/src/config.rs`에서

```rust
        (self.fence_after() / 10).clamp(Duration::from_millis(200), Duration::from_secs(2))
    }
```

을 다음으로 바꾼다:

```rust
        (self.fence_after() / 10).clamp(Duration::from_millis(200), Duration::from_secs(2))
    }

    /// 노드의 `HaGuard` 설정. 펜싱이 꺼져 있으면 None이다.
    pub fn guard_config(&self) -> Option<HaConfig> {
        let watchdog: Option<Arc<dyn WatchdogOpener>> = match self.fencing {
            FencingSetting::Off => return None,
            FencingSetting::Watchdog => Some(Arc::new(DeviceWatchdog {
                path: self.watchdog_device.clone(),
                timeout_secs: self.watchdog_timeout_secs,
            })),
            FencingSetting::Process => None,
        };
        Some(HaConfig {
            fence_after: self.fence_after(),
            renew_interval: self.renew_interval(),
            watchdog,
        })
    }
```

`crates/opld/src/config.rs`에서

```rust
    }

    #[test]
    fn renew_interval_is_a_tenth_of_fence_after_within_bounds() {
```

을 다음으로 바꾼다:

```rust
    }

    /// 펜싱이 꺼져 있으면 guard가 없고, process는 watchdog 없이, watchdog는 장치와 함께 만든다.
    #[test]
    fn guard_config_follows_the_fencing() {
        let mut ha = HaSection {
            fencing: FencingSetting::Off,
            fence_after_secs: 5,
            ..HaSection::default()
        };
        assert!(ha.guard_config().is_none());
        ha.fencing = FencingSetting::Process;
        let config = ha.guard_config().unwrap();
        assert_eq!(config.fence_after, Duration::from_secs(5));
        assert_eq!(config.renew_interval, Duration::from_millis(500));
        assert!(config.watchdog.is_none());
        ha.fencing = FencingSetting::Watchdog;
        assert!(ha.guard_config().unwrap().watchdog.is_some());
    }

    #[test]
    fn renew_interval_is_a_tenth_of_fence_after_within_bounds() {
```

`crates/opld/src/member.rs`에서

```rust
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirmwareFetcher, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
```

을 다음으로 바꾼다:

```rust
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
```

`crates/opld/src/member.rs`에서

```rust
    pools: PoolMounter,
}
```

을 다음으로 바꾼다:

```rust
    pools: PoolMounter,
    /// HA 리스와 watchdog(`[ha] fencing = "off"`면 없다)
    ha: Option<Arc<HaGuard>>,
}
```

`crates/opld/src/member.rs`에서

```rust
        self.reconciler.shutdown().await;
    }
```

을 다음으로 바꾼다:

```rust
        self.reconciler.shutdown().await;
        // 작업자가 모두 멈춘 뒤에 닫아야 도는 HA VM 목록이 맞다.
        if let Some(ha) = &self.ha {
            ha.close();
        }
    }
```

`crates/opld/src/member.rs`에서

```rust
        reconciler_config.firmware = Some(firmware.clone());
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
```

을 다음으로 바꾼다:

```rust
        reconciler_config.firmware = Some(firmware.clone());
        // 펜싱이 켜져 있으면 지금부터 리스를 갱신한다(지금이 stale을 세는 기준점이다).
        let ha = config
            .ha
            .guard_config()
            .map(|ha| HaGuard::spawn(cluster.clone(), ha));
        reconciler_config.ha = ha.clone();
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
```

`crates/opld/src/member.rs`에서

```rust
                pools,
            }),
```

을 다음으로 바꾼다:

```rust
                pools,
                ha,
            }),
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `cargo build -p opl-reconciler`
Expected: `Cargo.lock`이 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 793개, 실패 0

Run: `cargo test -p opl-reconciler --lib ha`
Expected: PASS

Run: `cargo test -p opld`
Expected: PASS

- [ ] **Step 4: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(reconciler): 노드의 HaGuard와 watchdog — 리스 갱신, fresh/stale, 자기 펜싱 준비"
```

---

### Task 4: 워커가 HA 리스를 따른다

HA VM은 켜기와 멈춘 QEMU 재개(받은 마이그레이션, 멈춘 채 남은 VM, 확정되지 않은 원본) 전에 `fresh()`와 `arm()`이 필요하다 — 아니면 실패로 세지 않고 `retry_interval` 뒤("waiting for the cluster majority before starting an HA VM"). 느린 시작 과정 전에 도는 HA VM으로 알리고, `hv.start`·재부착 직전에 리스와 로컬 `spec.node`·원하는 상태를 다시 본다(`ha_recheck`). Starting 기록에 실패한 HA VM은 그래도 켜지 않는다. 펜싱이 꺼진 노드는 HA VM을 켜지 않는다("HA is not available on this node"). 도는 HA VM이 stale이면 강제 종료한다(우리가 요청한 종료, 실패로 세지 않고 원하는 상태도 그대로, "fenced: lost the cluster majority"). 떠나는 VM은 QEMU가 없을 때만 목록에서 빼고, 디스패처가 워커 없는 VM의 멈춤 이벤트와 주기 재동기화로 정리한다. 마이그레이션 되돌리기와 남은 마이그레이션 정리는 HA 원본을 직접 재개하지 않는다. 시험: `tests/ha.rs`, `no_quorum.rs`, opl-api two_nodes·vms.

**Files:**
- Create: `crates/opl-reconciler/tests/ha.rs`
- Modify: `crates/opl-api/src/tasks/migrate.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-api/tests/vms.rs`
- Modify: `crates/opl-reconciler/src/ha.rs`
- Modify: `crates/opl-reconciler/src/reconciler.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/no_quorum.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
/// 끄게 하고, 원본이 아직 보내고 있으면 취소하고(나중에 끝나 원본이 멈추지 않게), 원본이 멈춰
/// 있으면(`postmigrate`, `paused`) 이어서 실행한다(못 하면 보류가 풀린 뒤 reconciler가 한다). 디스크는
/// 보류가 풀린 뒤 reconciler가 다시 이 노드만 쓰게 한다(대상이 아직 끄지 못했으면 될 때까지 다시
```

을 다음으로 바꾼다:

```rust
/// 끄게 하고, 원본이 아직 보내고 있으면 취소하고(나중에 끝나 원본이 멈추지 않게), 원본이 멈춰
/// 있으면(`postmigrate`, `paused`) 이어서 실행한다(못 하면 보류가 풀린 뒤 reconciler가 한다). HA VM은
/// 여기서 이어서 실행하지 않는다: 보류가 풀린 뒤 reconciler가 리스와 watchdog를 확인하고 이어서 실행한다
/// (그 사이 리스가 끝났으면 끈다). 디스크는
/// 보류가 풀린 뒤 reconciler가 다시 이 노드만 쓰게 한다(대상이 아직 끄지 못했으면 될 때까지 다시
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
) -> Outcome {
    loop {
```

을 다음으로 바꾼다:

```rust
) -> Outcome {
    let mut ha = false;
    loop {
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
        }
        let current = vm.status.migration == Some(migration);
```

을 다음으로 바꾼다:

```rust
        }
        ha = vm.spec.ha;
        let current = vm.status.migration == Some(migration);
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
        Ok(Some(state))
            if state.status == postmigrate() || state.status == RuntimeStatus::Paused =>
        {
```

을 다음으로 바꾼다:

```rust
        Ok(Some(state))
            if !ha && (state.status == postmigrate() || state.status == RuntimeStatus::Paused) =>
        {
```

`crates/opl-api/tests/two_nodes.rs` 전체:

```rust
//! 실제 gRPC(mTLS)로 이은 두 노드: 한 노드의 API로 다른 노드에 VM을 띄우고 콘솔을 중계한다.
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기(HA VM 포함), 공유 디스크의 잠금 바꾸기.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{
    AppState, IncomingConfig, IncomingMigrations, TaskRunner, UefiVarsFetcher, hash_password,
};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{
    DriverError, Hypervisor, MigrateTarget, MockHypervisor, RuntimeStatus, Storage, uefi_vars_path,
};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{FirmwareFetcher, HaConfig, HaGuard, Reconciler};
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::{Arch, PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use common::{
    ADMIN_PASSWORD, FakeStorage, MOUNT_ROOT, WAIT, login_at, reconciler_config, runner_config,
    vm_request,
};

/// 받는 노드가 마이그레이션을 받는 포트 범위. Mock 하이퍼바이저는 첫 포트를 연다.
const MIGRATION_PORTS: std::ops::RangeInclusive<u16> = 61000..=61009;

struct Node {
    id: Uuid,
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 볼륨 호출을 적는 저장소(reconciler, Task 실행기, 마이그레이션 받기가 함께 쓴다)
    storage: Arc<FakeStorage>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    base: String,
    /// reconciler의 HA guard. None이면 펜싱이 꺼진 노드다.
    ha: Option<Arc<HaGuard>>,
    _workers: Option<(Reconciler, TaskRunner)>,
}

impl Node {
    async fn open(ca: &ClusterCa, root: &TempDir) -> Node {
        let id = Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(NodeId(id), root.path().join(id.to_string()));
        // 테스트가 동시에 많이 돌면 첫 선거가 몇 초씩 걸린다.
        config.write_timeout = Duration::from_secs(30);
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        Node {
            id,
            addr,
            cluster,
            peers,
            hv: Arc::new(MockHypervisor::default()),
            storage: Arc::new(FakeStorage::default()),
            tls,
            listener: Some(listener),
            base: String::new(),
            ha: None,
            _workers: None,
        }
    }

    /// 노드 간 서버(마이그레이션 받기 포함), reconciler, Task 실행기, API 서버(HTTP)를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let storage: Arc<dyn Storage> = self.storage.clone();
        let incoming = IncomingMigrations::new(
            self.cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node: self.id,
                mount_root: MOUNT_ROOT.into(),
                storage: storage.clone(),
                listen_host: "127.0.0.1".into(),
                ports: MIGRATION_PORTS,
            },
        );
        let work = root.path().join(format!("{}-work", self.id));
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(work.join("state"));
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, std::future::pending())
                .await
                .unwrap();
        });
        // UEFI 변수 파일을 가진 노드에서 받아 온다(opld처럼 reconciler와 Task 실행기가 함께 쓴다).
        // 시험의 변수 파일은 모두 템플릿과 같은 6바이트다.
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(template(root, self), b"vars 0").unwrap();
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            self.cluster.clone(),
            self.peers.clone(),
            work.join("state"),
            template(root, self),
        ));
        let mut reconciler_config = reconciler_config(self.id);
        reconciler_config.storage = storage.clone();
        reconciler_config.firmware = Some(firmware.clone());
        reconciler_config.ha = self.ha.clone();
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        config.firmware = Some(firmware);
        let runner = TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            hv.clone(),
            reconciler.handle(),
            config,
        )
        .await;
        let mut state = AppState::new(self.cluster.clone(), self.id, hv, work.join("uploads"));
        state.peers = Some(self.peers.clone());
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(Arc::new(state));
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
        self._workers = Some((reconciler, runner));
    }
}

/// 두 노드를 띄워 클러스터를 만들고 Node 리소스와 관리자를 채운다. 노드 간 서버가 먼저 떠야
/// 선거가 끝나므로 서버부터 띄운다.
async fn two_nodes() -> (TempDir, Node, Node) {
    two_nodes_with(None).await
}

/// `ha`가 있으면 A의 reconciler에 넣고 두 노드를 HA 가능 노드(`process`)로 등록한다.
async fn two_nodes_with(ha: Option<Arc<HaGuard>>) -> (TempDir, Node, Node) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root).await;
    let mut b = Node::open(&ca, &root).await;
    let node_ha = ha.as_ref().map(|_| NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 60,
    });
    a.ha = ha;
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    for (i, node) in [&a, &b].into_iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node{}", i + 1),
            NodeSpec {
                address: node.base.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id;
        r.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
            ha: node_ha,
        };
        // 리더가 뽑힐 때까지 쓰기가 기다린다.
        a.cluster.create(&r).await.unwrap();
    }
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
            role: Role::Admin,
            session_epoch: 0,
        },
    );
    a.cluster.create(&admin).await.unwrap();
    (root, a, b)
}

#[tokio::test]
async fn console_of_a_vm_on_another_node_is_relayed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    b.hv.set_console_dir(tmp.path());

    // 노드 A의 API로 노드 B에 VM을 만들고 켠다. Task는 B의 실행기가 끝낸다.
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(b.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = b
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert!(a.hv.inspect(vm.id).await.unwrap().is_none());

    let listener =
        tokio::net::UnixListener::bind(tmp.path().join(format!("{}.vnc", vm.id))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });
    let url = format!("{}/api/v1/vms/web/console", a.base).replacen("http", "ws", 1);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", admin.token.as_ref().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let greeting = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    let echo = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echo, Message::binary(b"ping".to_vec()));

    // A의 API로 만든 VM은 B에서도 곧 보인다(같은 저장소).
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let deadline = Instant::now() + WAIT;
    while admin_b.get("/vms/web").await.0 != StatusCode::OK {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 노드의 드라이버 `state_dir`(Task 실행기 설정과 노드 간 서버가 같은 곳을 쓴다)
fn state_dir(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path().join(format!("{}-work", node.id)).join("state")
}

/// 노드의 UEFI 변수 템플릿(받은 변수 파일의 크기를 이와 견준다)
fn template(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path()
        .join(format!("{}-work", node.id))
        .join("vars-template.fd")
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_the_node_that_has_them() {
    let (root, a, b) = two_nodes().await;
    let vm = Uuid::now_v7();
    let src = uefi_vars_path(&state_dir(&root, &b), vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"vars b").unwrap();
    let fetcher = UefiVarsFetcher::new(
        a.cluster.clone(),
        a.peers.clone(),
        state_dir(&root, &a),
        template(&root, &a),
    );
    let dest = uefi_vars_path(&state_dir(&root, &a), vm);

    fetcher.fetch(vm, b.id).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 크기가 이 노드의 템플릿과 다르면(다른 펌웨어 빌드) 받은 것을 버린다.
    std::fs::write(&src, b"other firmware").unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert!(err.0.contains("14 bytes"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 파일이 없거나 멤버가 아닌 노드면 실패하고 이 노드의 파일은 그대로다.
    let err = fetcher.fetch(Uuid::now_v7(), b.id).await.unwrap_err();
    assert!(err.0.contains("no UEFI variables"), "{err}");
    let err = fetcher.fetch(vm, Uuid::now_v7()).await.unwrap_err();
    assert!(err.0.contains("not a cluster member"), "{err}");

    // 오프라인으로 기록된 노드에서는 받지 않는다.
    std::fs::write(&src, b"newer").unwrap();
    let mut node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    node.status.online = false;
    a.cluster
        .update_status::<NodeSpec>(b.id, &node.status)
        .await
        .unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert_eq!(err.0, "node \"node2\" is offline");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");
}

/// 노드의 VM `vm` UEFI 변수 파일에 쓴다.
fn write_vars(root: &TempDir, node: &Node, vm: Uuid, data: &[u8]) {
    let path = uefi_vars_path(&state_dir(root, node), vm);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

fn read_vars(root: &TempDir, node: &Node, vm: Uuid) -> Vec<u8> {
    std::fs::read(uefi_vars_path(&state_dir(root, node), vm)).unwrap()
}

/// 멈춘 UEFI VM을 다른 노드로 옮기면, 그 노드의 스냅샷·복제·시작이 앞 노드의 변수를 받아 쓴다.
/// 되돌린 변수는 그 노드의 것이 된다(다시 받지 않는다).
#[tokio::test]
async fn variables_follow_a_stopped_vm_to_another_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    write_vars(&root, &a, vm.id, b"vars 1");

    // 멈춘 채 B로 옮긴다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = serde_json::json!(b.id);
    let (status, body) = admin
        .put("/vms/web", serde_json::json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 스냅샷은 B에서 찍고 A의 변수를 받아 담는다.
    let (status, task) = admin
        .post("/vms/web/snapshots", serde_json::json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    let s1 = b
        .cluster
        .list::<opl_types::snapshot::SnapshotSpec>()
        .unwrap()
        .1
        .into_iter()
        .find(|s| s.spec.vm == vm.id)
        .unwrap();
    let copy = state_dir(&root, &b).join(format!("vm/{}/snapshots/{}.vars", vm.id, s1.id));
    assert_eq!(std::fs::read(copy).unwrap(), b"vars 1");

    // 복제도 B에서 A의 변수를 받아 복사하고, 새 VM의 변수는 B에 있다.
    write_vars(&root, &a, vm.id, b"vars 2");
    let (status, task) = admin
        .post("/vms/web/clone", serde_json::json!({ "name": "web-2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = b.cluster.get_by_name::<VmSpec>("web-2").unwrap().unwrap();
    assert_eq!(read_vars(&root, &b, copy.id), b"vars 2");
    assert_eq!(copy.status.firmware_node, Some(b.id));

    // 켜면 B의 reconciler가 A의 변수를 받아 켜고 B를 변수 노드로 남긴다.
    write_vars(&root, &a, vm.id, b"vars 3");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 3");
    wait_vm(&b, vm.id, "variables on B", |v| {
        v.status.firmware_node == Some(b.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    // 되돌리기는 스냅샷 때의 변수를 B에 쓰고 B를 변수 노드로 삼는다. 다음 시작은 A에서 받지 않는다.
    let mut moved = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    moved.status.firmware_node = Some(a.id);
    b.cluster
        .update_status::<VmSpec>(vm.id, &moved.status)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", serde_json::Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let reverted = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(reverted.status.firmware_node, Some(b.id));
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 1");
}

/// A에서 도는 VM `web`(디스크 없음)을 만든다.
async fn running_vm(a: &Node, admin: &common::Client) -> opl_types::vm::Vm {
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap()
}

async fn migrate(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": node }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

async fn runtime_status(node: &Node, id: Uuid) -> Option<RuntimeStatus> {
    node.hv.inspect(id).await.unwrap().map(|s| s.status)
}

/// 두 노드에서 VM이 원하는 대로 보일 때까지 기다린다.
async fn wait_vm(node: &Node, id: Uuid, what: &str, pred: impl Fn(&opl_types::vm::Vm) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let vm = node.cluster.get::<VmSpec>(id).unwrap().unwrap();
        if pred(&vm) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {:?}", vm.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 확정 전에 실패한 마이그레이션 뒤: VM은 A에 남아 돌고, B에는 받는 QEMU가 없다.
async fn assert_still_on_a(a: &Node, b: &Node, id: Uuid) {
    let vm = a.cluster.get::<VmSpec>(id).unwrap().unwrap();
    assert_eq!(vm.spec.node, Some(a.id));
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(vm.status.node, Some(a.id));
    assert_eq!(vm.status.migration, None);
    assert_eq!(runtime_status(a, id).await, Some(RuntimeStatus::Running));
    assert_eq!(runtime_status(b, id).await, None);
    assert!(!b.hv.is_incoming(id));
}

#[tokio::test]
async fn running_vm_migrates_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    // B의 reconciler가 넘겨받으며 `firmware_node`를 쓰기 전에, 확정 뒤 기록이 B를 쓰는지 본다.
    let b_reconciler = b._workers.as_ref().unwrap().0.handle();
    b_reconciler.begin_migration(vm.id).await;

    // B의 API로 요청해도 Task는 원본 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = migrate(&admin_b, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], a.id.to_string());

    // A의 QEMU는 B의 클러스터 주소의 호스트, B가 연 포트로 보냈다.
    assert_eq!(
        a.hv.migrations(),
        vec![(
            vm.id,
            MigrateTarget {
                host: "127.0.0.1".into(),
                port: *MIGRATION_PORTS.start(),
                tls_hostname: "127.0.0.1".into(),
            }
        )]
    );
    let vm_now = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(vm_now.spec.node, Some(b.id));
    assert_eq!(vm_now.status.node, Some(b.id));
    assert_eq!(vm_now.status.migration, None);
    // QEMU가 변수를 B로 옮겼다.
    assert_eq!(vm_now.status.firmware_node, Some(b.id));
    assert_eq!(runtime_status(&a, vm.id).await, None);
    b_reconciler.end_migration(vm.id);
    // B의 reconciler가 받은 QEMU를 넘겨받아 이어서 실행한다(새로 띄우지 않는다).
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id) && v.status.pid.is_some()
    })
    .await;
    let deadline = Instant::now() + WAIT;
    while b.hv.is_incoming(vm.id) {
        assert!(Instant::now() < deadline, "B did not take over the VM");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    assert!(b.hv.launches().is_empty(), "{:?}", b.hv.launches());

    // 옮긴 VM은 B에서 다룬다: 끄면 B의 QEMU가 꺼진다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

#[tokio::test]
async fn failed_send_keeps_the_vm_running_on_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration(
        "Failed to connect to '127.0.0.1:61000': Connection refused".into(),
    ));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("Connection refused"), "{error}");
    assert_still_on_a(&a, &b, vm.id).await;
    // 다른 동작이 다시 된다.
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 원본은 다 보냈지만(`postmigrate`) 대상이 받기에 실패했다: 확정하지 않고 원본을 이어서 실행한다.
#[tokio::test]
async fn failed_receive_resumes_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("TLS handshake failed"), "{error}");
    assert_eq!(a.hv.migrations().len(), 1);
    assert_still_on_a(&a, &b, vm.id).await;
}

/// 정해 둔 대로 곧바로 갱신에 성공하거나 실패한다.
struct FakeRenewer(std::sync::Mutex<bool>);

#[async_trait::async_trait]
impl LeaseRenewer for FakeRenewer {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), opl_cluster::ClusterError> {
        if *self.0.lock().unwrap() {
            Ok(())
        } else {
            Err(opl_cluster::ClusterError::Timeout)
        }
    }
}

/// A에 가짜 리스의 HA guard(`process`, 리스는 `fence_after` 뒤에 끝난다)를 두고 HA VM `web`을 켠다.
async fn ha_vm_on_a(
    fence_after: Duration,
) -> (TempDir, Node, Node, Arc<FakeRenewer>, opl_types::vm::Vm) {
    let renewer = Arc::new(FakeRenewer(std::sync::Mutex::new(true)));
    let guard = HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after,
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    );
    let (root, a, b) = two_nodes_with(Some(guard)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["ha"] = serde_json::json!(true);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    wait_vm(&a, vm.id, "running", |v| v.status.phase == VmPhase::Running).await;
    (root, a, b, renewer, vm)
}

/// 되돌린 HA VM의 원본은 reconciler가 리스를 확인한 뒤 이어서 실행한다.
#[tokio::test]
async fn failed_receive_of_an_ha_vm_resumes_the_source_with_the_lease() {
    let (_root, a, b, _renewer, vm) = ha_vm_on_a(Duration::from_secs(60)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let deadline = Instant::now() + WAIT;
    while runtime_status(&a, vm.id).await != Some(RuntimeStatus::Running) {
        assert!(Instant::now() < deadline, "source not resumed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_still_on_a(&a, &b, vm.id).await;
    assert_eq!(a.hv.launches().len(), 1);
}

/// 마이그레이션하는 동안 리스가 끝났으면 되돌려도 원본을 이어서 실행하지 않고 끈다. 리스가 돌아오면
/// 다시 켠다.
#[tokio::test]
async fn failed_receive_of_an_ha_vm_without_the_lease_stops_the_source() {
    let (_root, a, b, renewer, vm) = ha_vm_on_a(Duration::from_millis(500)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    a.hv.delay_migrations(Duration::from_millis(1500));
    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    *renewer.0.lock().unwrap() = false;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    wait_vm(&a, vm.id, "fenced", |v| {
        v.status.last_error.as_deref() == Some("fenced: lost the cluster majority")
    })
    .await;
    assert_eq!(runtime_status(&a, vm.id).await, None);
    assert!(
        !a.hv.pause_calls(vm.id).contains(&"resume"),
        "{:?}",
        a.hv.pause_calls(vm.id)
    );

    *renewer.0.lock().unwrap() = true;
    wait_vm(&a, vm.id, "running again", |v| {
        v.status.phase == VmPhase::Running && v.status.last_error.is_none()
    })
    .await;
    assert_eq!(a.hv.launches().len(), 2);
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

/// 원본 QEMU가 보내는 중인데 기다리기가 실패했다: 되돌리며 그 마이그레이션을 취소한다. 나중에 끝나
/// 원본을 멈추지 않는다.
#[tokio::test]
async fn unfinished_send_is_cancelled_by_the_rollback() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate_unfinished(DriverError::Timeout("QMP query-migrate"));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!a.hv.is_outgoing(vm.id));
    a.hv.complete_outgoing(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_still_on_a(&a, &b, vm.id).await;
}

/// API 검사를 거치지 않고 Task를 만든다(검사와 마이그레이션 기록 사이에 들어온 요청).
async fn put_task(node: &Node, op: TaskOp, target: Uuid, on: Uuid) -> Uuid {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: on,
        },
    );
    task.name = task.id.to_string();
    node.cluster.create(&task).await.unwrap();
    task.id
}

/// Task가 끝날 때까지 기다린다.
async fn finished_task(node: &Node, id: Uuid) -> Task {
    let deadline = Instant::now() + WAIT;
    loop {
        let task = node.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        if task.status.state.is_finished() {
            return task;
        }
        assert!(Instant::now() < deadline, "task {id}: {:?}", task.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// API가 마이그레이션을 받은 뒤 같은 VM의 스냅샷·디스크 크기 Task가 들어왔다: 원본 Task가 기록한 뒤
/// 다시 보고 되돌린다.
#[tokio::test]
async fn task_started_alongside_stops_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    // 실행하는 노드가 없어 끝나지 않는다.
    let elsewhere = Uuid::now_v7();
    let others = [
        (
            TaskOp::SnapshotCreate {
                snapshot: Uuid::now_v7(),
            },
            vm.id,
            "snapshotted",
        ),
        (
            TaskOp::VolumeResize {
                size_bytes: 2 << 20,
            },
            volume,
            "resized",
        ),
    ];
    for (op, target, what) in others {
        let other = put_task(&a, op, target, elsewhere).await;
        let migrate = put_task(&a, TaskOp::VmMigrate { node: b.id }, vm.id, a.id).await;
        let task = finished_task(&a, migrate).await;
        assert_eq!(task.status.state, TaskState::Failed, "{:?}", task.status);
        let error = task.status.error.unwrap();
        assert!(error.contains(what), "{error}");
        assert!(a.hv.migrations().is_empty());
        assert_still_on_a(&a, &b, vm.id).await;
        a.cluster.delete::<TaskSpec>(other, None).await.unwrap();
    }
}

/// 보내는 동안 VM을 껐다(검사와 기록 사이에 들어온 정지): 확정하지 않는다.
#[tokio::test]
async fn vm_stopped_during_the_migration_is_not_committed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    wait_vm(&a, vm.id, "migrating", |v| {
        v.status.phase == VmPhase::Migrating
    })
    .await;
    let current = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.power = PowerState::Stopped;
    a.cluster
        .update_spec(vm.id, None, current.labels, &spec)
        .await
        .unwrap();

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("stopped"), "{error}");
    let got = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(got.spec.node, Some(a.id));
    assert_eq!(runtime_status(&b, vm.id).await, None);
    // A의 reconciler가 원하는 대로 끈다.
    wait_vm(&a, vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped).await;
}

#[tokio::test]
async fn different_qemu_versions_do_not_migrate() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.set_qemu_version("9.0.0");
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("QEMU 9.0.0"), "{error}");
    assert!(a.hv.migrations().is_empty());
    assert_still_on_a(&a, &b, vm.id).await;
}

/// A에서 도는 VM `web`. 디스크는 공유 dir 풀 `nas`의 볼륨 `root`다. 볼륨 id를 함께 돌려준다.
async fn running_vm_with_disk(a: &Node, admin: &common::Client) -> (opl_types::vm::Vm, Uuid) {
    let pool: StoragePool = Resource::new(
        "nas",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/nonexistent/opl-nas".into(),
            },
            nodes: vec![],
            shared: true,
        },
    );
    a.cluster.create(&pool).await.unwrap();
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "nas".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
        },
    );
    volume.status.node = Some(a.id);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["disks"] = serde_json::json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    (vm, volume.id)
}

/// 노드의 볼륨 `id`에 대한 공유·배타·끄기 호출(차례대로)
fn lock_calls(node: &Node, id: Uuid) -> Vec<&'static str> {
    node.storage
        .calls()
        .into_iter()
        .filter(|(op, v)| *v == id && ["share", "exclusive", "deactivate"].contains(op))
        .map(|(op, _)| op)
        .collect()
}

/// 공유 디스크(공유 LVM이면 LV 잠금): 원본은 보내기 전에 공유로 바꾸고 확정 뒤 끈다. 대상은 공유로
/// 켜서 받고, 넘겨받은 뒤 원본이 끌 때까지(일시 오류) 다시 시도해 배타로 바꾼다.
#[tokio::test]
async fn shared_disks_are_shared_during_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    assert!(lock_calls(&a, volume).is_empty());

    b.storage.hold_exclusives(1);
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(lock_calls(&a, volume), ["share", "deactivate"]);
    let deadline = Instant::now() + WAIT;
    while lock_calls(&b, volume).len() < 3 {
        assert!(
            Instant::now() < deadline,
            "B did not make the disk exclusive: {:?}",
            lock_calls(&b, volume)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(lock_calls(&b, volume), ["share", "exclusive", "exclusive"]);
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    // 한 번 배타로 바꾼 뒤에는 다시 하지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&b, volume).len(), 3);
}

/// 확정 전 실패: 대상은 받으려고 켠 디스크를 끄고, 원본은(보류가 풀린 reconciler가) 다시 배타로
/// 바꾼다.
#[tokio::test]
async fn failed_migration_makes_shared_disks_exclusive_again() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration("Connection refused".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_still_on_a(&a, &b, vm.id).await;
    let deadline = Instant::now() + WAIT;
    while lock_calls(&a, volume).len() < 2 {
        assert!(Instant::now() < deadline, "{:?}", lock_calls(&a, volume));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&a, volume), ["share", "exclusive"]);
    assert_eq!(lock_calls(&b, volume), ["share", "deactivate"]);
}
```

`crates/opl-api/tests/vms.rs`에서

```rust

use std::time::Duration;

use common::{Env, vm_request};
use opl_driver::DriverError;
use opl_types::node::{Fencing, Node, NodeHa, NodeSpec};
```

을 다음으로 바꾼다:

```rust

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm_request};
use opl_cluster::ClusterError;
use opl_driver::DriverError;
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{HaConfig, HaGuard};
use opl_types::node::{Fencing, Node, NodeHa, NodeSpec};
```

`crates/opl-api/tests/vms.rs`에서

```rust
async fn ha_vms_need_an_ha_capable_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
```

을 다음으로 바꾼다:

```rust
async fn ha_vms_need_an_ha_capable_node() {
    // reconciler는 `process` 펜싱으로 HA VM을 켠다(리스는 늘 갱신된다).
    struct Renewed;
    #[async_trait]
    impl LeaseRenewer for Renewed {
        async fn renew_lease(&self, _timeout: Duration) -> Result<(), ClusterError> {
            Ok(())
        }
    }
    let env = Env::start_with(
        |c| {
            c.ha = Some(HaGuard::spawn(
                Arc::new(Renewed),
                HaConfig {
                    fence_after: Duration::from_secs(30),
                    renew_interval: Duration::from_secs(1),
                    watchdog: None,
                },
            ))
        },
        |_| async {},
    )
    .await;
    let admin = env.admin().await;
```

`crates/opl-reconciler/src/ha.rs`에서

```rust
            state.running.remove(&vm);
        }
    }
```

을 다음으로 바꾼다:

```rust
            state.running.remove(&vm);
        }
    }

    /// 도는 HA VM 가운데 `keep`에 없는 것을 뺀다. 작업자가 없어 아무도 빼지 않는 VM(옮겨 간 뒤 멈춘
    /// 원본 등)을 reconciler가 정리한다.
    pub fn retain_running(&self, keep: &HashSet<Uuid>) {
        self.shared
            .state
            .lock()
            .unwrap()
            .running
            .retain(|vm| keep.contains(vm));
    }
```

`crates/opl-reconciler/src/reconciler.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{EventKind, Hypervisor};
use opl_types::resource::Kind;
use opl_types::vm::VmSpec;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::ReconcilerConfig;
use crate::ha::LeaseState;
use crate::worker::{Trigger, Worker};

/// 한 노드의 조정 루프. `spawn`으로 시작하고 `shutdown`으로 멈춘다.
///
/// 멈춰도 VM은 건드리지 않는다. 다시 시작하면(하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙은 뒤)
/// 현재 실제 상태에서부터 이어서 맞춘다.
pub struct Reconciler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    commands: mpsc::UnboundedSender<Command>,
    restores: Restores,
    held: Held,
}

/// VM별로 다음 시작에 불러올 스냅샷 태그. 이 노드의 메모리에만 있어 opld가 재시작하면 사라진다.
pub(crate) type Restores = Arc<Mutex<HashMap<Uuid, String>>>;

/// 이 노드의 마이그레이션 Task가 다루는 중인 VM(`ReconcilerHandle::begin_migration`). 작업자는
/// 이 VM에 손대지 않는다. 메모리에만 있으므로 opld가 재시작하면 풀린다.
pub(crate) type Held = Arc<Mutex<HashSet<Uuid>>>;

/// 작업자에게 보내는 일회성 명령
enum Command {
    ForceStop(Uuid),
    /// 그 VM의 작업자가 지금 하던 단계를 마치면 알린다.
    Barrier(Uuid, oneshot::Sender<()>),
    Sync(Uuid),
}

/// 실행 중인 reconciler에 VM별 일회성 명령을 보낸다. 명령은 그 VM의 작업자 큐를 거치므로
/// 같은 VM에 대한 드라이버 호출이 겹치지 않는다.
#[derive(Clone)]
pub struct ReconcilerHandle {
    commands: mpsc::UnboundedSender<Command>,
    restores: Restores,
    held: Held,
}

impl ReconcilerHandle {
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다. 원하는 상태를 먼저
    /// stopped로 기록한 뒤에 부른다. 이 노드에 배정되지 않은 VM이거나 원하는 상태가 running이면
    /// 아무것도 하지 않는다. 결과는 VM status로 확인한다.
    pub fn force_stop(&self, id: Uuid) {
        let _ = self.commands.send(Command::ForceStop(id));
    }

    /// 이 노드에서 VM을 다음에 새로 띄울 때 한 번만 이 스냅샷 태그를 불러와 이어서 실행한다.
    /// 원하는 상태를 running으로 기록하기 전에 부른다. 시작이 실패해도 태그는 쓴 것으로 치고,
    /// 그 시작은 다시 시도하지 않는다(VM은 failed). 이미 맡긴 태그는 바꾼다.
    pub fn restore_on_start(&self, id: Uuid, tag: String) {
        self.restores.lock().unwrap().insert(id, tag);
    }

    /// 맡긴 태그를 쓰지 않고 버린다(시작하기 전에 되돌리기가 실패했을 때).
    pub fn clear_restore(&self, id: Uuid) {
        self.restores.lock().unwrap().remove(&id);
    }

    /// 라이브 마이그레이션을 보내는 동안 작업자가 이 VM에 손대지 않게 한다(status를 덮어쓰거나, 멈춘
    /// 원본을 이어서 실행하거나, 이벤트로 다시 시작하지 않는다). 작업자가 하던 단계를 마칠 때까지
    /// 기다린 뒤 돌아오므로, 돌아온 뒤에 쓴 status를 작업자가 덮지 않는다. 반드시
    /// [`end_migration`](Self::end_migration)으로 푼다.
    pub async fn begin_migration(&self, id: Uuid) {
        self.held.lock().unwrap().insert(id);
        let (tx, rx) = oneshot::channel();
        if self.commands.send(Command::Barrier(id, tx)).is_ok() {
            // 작업자가 없으면 보낸 쪽이 버려져 곧바로 돌아온다.
            let _ = rx.await;
        }
    }

    /// [`begin_migration`](Self::begin_migration)을 풀고 작업자가 VM을 다시 맞추게 한다.
    pub fn end_migration(&self, id: Uuid) {
        self.held.lock().unwrap().remove(&id);
        let _ = self.commands.send(Command::Sync(id));
    }
}

impl Reconciler {
    pub fn spawn(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: ReconcilerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        let (commands, command_rx) = mpsc::unbounded_channel();
        let restores = Restores::default();
        let held = Held::default();
        let dispatcher = Dispatcher {
            cluster,
            hv,
            config: Arc::new(config),
            restores: restores.clone(),
            held: held.clone(),
            workers: HashMap::new(),
            retired: HashMap::new(),
        };
        Self {
            shutdown,
            task: tokio::spawn(dispatcher.run(rx, command_rx)),
            commands,
            restores,
            held,
        }
    }

    pub fn handle(&self) -> ReconcilerHandle {
        ReconcilerHandle {
            commands: self.commands.clone(),
            restores: self.restores.clone(),
            held: self.held.clone(),
        }
    }

    /// 진행 중인 단계가 끝나기를 기다린 뒤 돌아온다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Watch(#[from] WatchError),
}

struct WorkerHandle {
    tx: mpsc::UnboundedSender<Trigger>,
    task: JoinHandle<()>,
}

/// 리스 상태가 바뀔 때까지 기다린다. guard가 없으면 끝나지 않는다.
async fn lease_changed(rx: &mut Option<watch::Receiver<LeaseState>>) {
    if let Some(r) = rx.as_mut()
        && r.changed().await.is_ok()
    {
        return;
    }
    *rx = None;
    std::future::pending().await
}

/// 저장소 변경, 하이퍼바이저 이벤트, 주기적 재동기화를 VM별 작업자에게 나눠 준다.
struct Dispatcher {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReconcilerConfig>,
    restores: Restores,
    held: Held,
    /// 이 노드에 배정된 VM의 작업자
    workers: HashMap<Uuid, WorkerHandle>,
    /// 내보냈지만 아직 끝나지 않았을 수 있는 작업자. 같은 VM이 다시 배정되면 새 작업자가 기다린다.
    retired: HashMap<Uuid, JoinHandle<()>>,
}

impl Dispatcher {
    async fn run(
        mut self,
        mut shutdown: oneshot::Receiver<()>,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
        // 목록을 읽기 전에 구독해야 그 사이에 난 이벤트를 놓치지 않는다.
        let mut events = self.hv.subscribe();
        // 리스가 살아나거나 끝나면 모든 작업자를 깨운다(HA VM을 켜거나 끈다).
        let mut lease = self.config.ha.as_ref().map(|g| g.subscribe());
        'sync: loop {
            let mut watch = match self.full_sync().await {
                Ok(watch) => watch,
                Err(e) => {
                    tracing::warn!(error = %e, "full sync failed; will retry");
                    tokio::select! {
                        _ = &mut shutdown => break,
                        () = tokio::time::sleep(self.config.retry_interval) => continue,
                    }
                }
            };
            let resync = tokio::time::sleep(self.config.resync_interval);
            tokio::pin!(resync);
            loop {
                tokio::select! {
                    _ = &mut shutdown => break 'sync,
                    () = &mut resync => continue 'sync,
                    ev = watch.next() => match ev {
                        Ok(WatchEvent::Put { kind: Kind::Vm, resource })
                        | Ok(WatchEvent::Deleted { kind: Kind::Vm, resource, .. }) => {
                            self.vm_changed(resource.id);
                        }
                        Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                        Ok(_) => {}
                    },
                    ev = events.recv() => match ev {
                        Ok(ev) => {
                            if !self.notify(ev.id, Trigger::Event(ev.kind)) {
                                tracing::debug!(vm = %ev.id, "event for a VM not assigned to this node");
                                // 옮겨 간 HA VM의 원본처럼 작업자가 물러난 뒤 멈춘 QEMU다.
                                if ev.kind != EventKind::Started
                                    && let Some(guard) = &self.config.ha
                                {
                                    guard.set_running(ev.id, false);
                                }
                            }
                        }
                        // 이벤트를 놓쳤다. 실제 상태를 전부 다시 맞춘다.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue 'sync,
                        Err(broadcast::error::RecvError::Closed) => break 'sync,
                    },
                    () = lease_changed(&mut lease) => {
                        for &id in self.workers.keys() {
                            self.notify(id, Trigger::Sync);
                        }
                    }
                    Some(command) = commands.recv() => match command {
                        Command::ForceStop(id) => {
                            if !self.notify(id, Trigger::ForceStop) {
                                tracing::warn!(vm = %id, "force stop for a VM not assigned to this node");
                            }
                        }
                        // 작업자가 없으면 보낸 쪽이 버려진다(기다릴 단계가 없다).
                        Command::Barrier(id, done) => {
                            self.notify(id, Trigger::Barrier(done));
                        }
                        Command::Sync(id) => {
                            self.notify(id, Trigger::Sync);
                        }
                    },
                }
            }
        }
        let workers = self.workers.drain().map(|(_, w)| w.task);
        let tasks: Vec<_> = workers
            .chain(self.retired.drain().map(|(_, t)| t))
            .collect();
        for task in tasks {
            let _ = task.await;
        }
    }

    /// 목록을 다시 읽어 작업자를 맞추고, 그 revision부터 구독한다.
    async fn full_sync(&mut self) -> Result<Watch, SyncError> {
        self.retired.retain(|_, task| !task.is_finished());
        let (revision, vms) = self.cluster.list::<VmSpec>()?;
        let mine: HashSet<Uuid> = vms
            .iter()
            .filter(|vm| vm.spec.node == Some(self.config.node))
            .map(|vm| vm.id)
            .collect();
        let gone: Vec<Uuid> = self
            .workers
            .keys()
            .filter(|id| !mine.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.retire(id);
        }
        for &id in &mine {
            self.sync(id);
        }
        // 경고를 남기고 도는 HA VM 목록을 정리하려는 것뿐이므로 실패해도 동기화는 이어 간다.
        match self.hv.list().await {
            Ok(states) => {
                for state in &states {
                    if !mine.contains(&state.id) {
                        tracing::warn!(vm = %state.id, "VM is running on this node but not assigned to it; leaving it alone");
                    }
                }
                // 이벤트를 놓쳐 남은 것을 뺀다. 작업자가 있는 VM은 작업자가 맞춘다(켜는 중일 수 있다).
                if let Some(guard) = &self.config.ha {
                    let keep: HashSet<Uuid> = states
                        .iter()
                        .map(|s| s.id)
                        .chain(self.workers.keys().copied())
                        .chain(self.retired.keys().copied())
                        .collect();
                    guard.retain_running(&keep);
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot list VMs on this node"),
        }
        Ok(self.cluster.watch(revision)?)
    }

    fn vm_changed(&mut self, id: Uuid) {
        match self.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) if vm.spec.node == Some(self.config.node) => self.sync(id),
            Ok(_) => self.retire(id),
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                self.notify(id, Trigger::Sync);
            }
        }
    }

    fn notify(&self, id: Uuid, trigger: Trigger) -> bool {
        self.workers
            .get(&id)
            .is_some_and(|w| w.tx.send(trigger).is_ok())
    }

    fn sync(&mut self, id: Uuid) {
        if self.notify(id, Trigger::Sync) {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = Worker::new(
            id,
            self.cluster.clone(),
            self.hv.clone(),
            self.config.clone(),
            self.restores.clone(),
            self.held.clone(),
        );
        let previous = self.retired.remove(&id);
        let task = tokio::spawn(worker.run(previous, rx));
        let _ = tx.send(Trigger::Sync);
        self.workers.insert(id, WorkerHandle { tx, task });
    }

    /// 삭제되었거나 다른 노드로 배정된 VM의 작업자를 내보낸다. 작업자는 마지막으로 한 번 확인하고 끝난다.
    fn retire(&mut self, id: Uuid) {
        // 다시 배정되었을 때 묵은 태그로 시작하지 않게 한다.
        self.restores.lock().unwrap().remove(&id);
        if let Some(w) = self.workers.remove(&id) {
            let _ = w.tx.send(Trigger::Sync);
            self.retired.insert(id, w.task);
        }
    }
}
```

`crates/opl-reconciler/src/worker.rs` 전체:

```rust
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, EventKind, Hypervisor, RuntimeStatus, StopMode};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{Firmware, PowerState, RestartPolicy, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::backoff::{FailureTracker, Next};
use crate::config::ReconcilerConfig;
use crate::ha::ArmError;
use crate::reconciler::{Held, Restores};
use crate::resolve::{ResolveError, running_spec, runtime_spec, set_disks_shared};

/// 실행 중이던 VM이 사라졌을 때 그 이유를 알리는 종료 이벤트를 기다리는 시간.
/// 이벤트보다 재동기화가 먼저 오면, 게스트가 스스로 끈 VM을 다시 켰다가 끄게 된다.
/// QEMU 드라이버는 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 최대 5초(`EXIT_GRACE`) 기다린 다음에
/// 크래시를 알리므로 그보다 길게 잡는다. 짧으면 느리게 끝난 크래시를 두 번 세게 된다.
const EVENT_GRACE: Duration = Duration::from_secs(6);

/// HA VM을 켜거나 이어서 실행하기 전에 리스를 기다리는 동안의 status
const HA_WAITING: &str = "waiting for the cluster majority before starting an HA VM";
/// 리스를 잃어 HA VM을 끈 뒤 리스를 기다리는 동안의 status
const HA_FENCED: &str = "fenced: lost the cluster majority";
/// 펜싱이 꺼진 노드(`[ha] fencing = "off"`)의 HA VM status
const HA_UNAVAILABLE: &str = "HA is not available on this node";

/// 작업자를 깨우는 이유.
#[derive(Debug)]
pub(crate) enum Trigger {
    /// spec이 바뀌었거나 전체 재동기화. 원하는 상태와 실제 상태를 다시 맞춘다.
    Sync,
    /// 하이퍼바이저가 알린 수명 이벤트
    Event(EventKind),
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다.
    ForceStop,
    /// 앞의 단계를 마쳤음을 알린다(`ReconcilerHandle::begin_migration`).
    Barrier(tokio::sync::oneshot::Sender<()>),
}

/// HA VM을 켜거나 멈춘 QEMU를 이어서 실행하기 직전(느린 디스크 해석 뒤)에 다시 본 결과
/// ([`Worker::ha_recheck`])
enum Recheck {
    Go,
    /// 리스가 끝났다. 실패로 세지 않고 다시 본다(status에 남길 이유).
    NotFresh(&'static str),
    /// 그 사이 다른 노드로 배정됐거나 원하는 상태가 stopped가 됐거나 지워졌다(다시 읽은 VM). 켜지 않고
    /// status도 쓰지 않은 채 다시 맞춘다.
    Changed(Option<Box<Vm>>),
}

/// [`Worker::start`]의 결과
enum Start {
    Started,
    /// 켜지 않았다(백오프, 포기, 리스 기다림).
    NotStarted,
    /// HA VM이 그 사이 바뀌었다([`Recheck::Changed`]).
    Changed(Option<Box<Vm>>),
}

/// 멈춘 QEMU를 이어서 실행하지 못한 이유
enum ContinueError {
    /// QMP로 다룰 수 없거나 참조를 풀지 못했다.
    Unresponsive(String),
    /// HA VM의 리스가 끝났다([`Recheck::NotFresh`]).
    NotFresh(&'static str),
    /// HA VM이 그 사이 바뀌었다([`Recheck::Changed`]).
    Changed(Option<Box<Vm>>),
}

#[derive(Debug, thiserror::Error)]
enum StepError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Driver(#[from] DriverError),
}

/// VM 하나를 맡는 작업자. 이 VM에 대한 드라이버 호출은 모두 이 태스크에서만 일어난다.
///
/// 매번 로컬 저장소에서 VM을 다시 읽고 실제 상태를 조회해 맞추므로(level-triggered),
/// 같은 트리거를 여러 번 받아도 안전하다. 이벤트는 "게스트가 스스로 꺼졌다"처럼 실제 상태만
/// 봐서는 알 수 없는 사실을 알릴 때만 쓴다.
pub(crate) struct Worker {
    id: Uuid,
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReconcilerConfig>,
    /// 다음 시작에 불러올 스냅샷 태그(`ReconcilerHandle::restore_on_start`)
    restores: Restores,
    /// 마이그레이션 Task가 다루는 중인 VM. 들어 있으면 이 작업자는 아무것도 하지 않는다.
    held: Held,
    /// 진행 중인 ACPI 종료 요청을 보낸 시각. 강제 종료로 넘어갈 시점을 정한다.
    /// 원하는 상태가 다시 running으로 바뀌면 취소하므로, 새 종료는 늘 ACPI부터 다시 시작한다.
    stop_requested: Option<Instant>,
    /// 이 작업자가 마지막으로 종료를 요청한(ACPI 또는 강제) 시각. 뒤이은 종료 이벤트가 우리
    /// 요청의 결과인지 가린다. 요청을 취소했거나 원하는 상태가 다시 running으로 바뀌어도
    /// 게스트(또는 강제 종료)는 요청대로 꺼질 수 있으므로 `acpi_timeout` 동안 유효하다.
    stopped_by_us: Option<Instant>,
    /// 다음 단계에서 ACPI 응답을 기다리지 않고 강제 종료한다. 원하는 상태가 stopped일 때만 켜고,
    /// 그 뒤 running으로 바뀌면 reconcile이 끈다.
    force_requested: bool,
    failures: FailureTracker,
    /// 백오프 중이면 이 시각까지 시작하지 않는다. status에 보일 이유를 함께 둔다.
    backoff: Option<(Instant, String)>,
    /// spec.power를 stopped로 바꿔야 한다(게스트 종료, 재시작 포기). 안쪽 값은 status에 남길 오류다.
    /// 쓰기에 실패하면 남겨 두었다가 다음 단계에서 다시 쓴다.
    power_off: Option<Option<String>>,
    /// 마지막으로 조회했을 때 VM이 실행 중이었는지
    was_running: bool,
    /// 사라진 VM의 종료 이벤트를 이 시각까지 기다린다.
    awaiting_event: Option<Instant>,
    /// 디스크 볼륨이 켜져 있을 수 있다(시작하려고 켰거나 VM이 도는 것을 봤다). QEMU가 끝난 것을
    /// 확인하면 끈다([`Worker::deactivate_disks`]).
    disks_active: bool,
    /// 라이브 마이그레이션이 디스크 볼륨을 원본·대상이 함께 쓰게 해 두었을 수 있다(받은 VM을
    /// 넘겨받았거나, 남은 마이그레이션을 되돌렸거나, 마이그레이션 Task가 보류했다 풀었다). VM이 도는
    /// 동안 다시 이 노드만 쓰게 한다([`Worker::exclusive_disks`], 성공할 때까지). 저장소에 남지 않는
    /// 상태이므로 이 작업자가 직접 켜서 시작한 것이 아니면 참으로 시작한다(opld 재시작이나 넘겨받기
    /// 뒤 이미 도는 VM에 다시 붙으면 한 번 배타로 바꾼다, 멱등).
    disks_shared: bool,
    /// 보류 중에 받은 마지막 수명 이벤트(원본 QEMU의 크래시 등). 보류가 풀리면 처리한다.
    held_event: Option<EventKind>,
    /// 리스를 잃어 이 HA VM을 껐다. 다시 켜거나 원하는 상태가 stopped가 되면 지운다.
    fenced: bool,
    /// guard에 도는 HA VM으로 알려 두었다([`Worker::report_running`]).
    ha_running: bool,
    wake_at: Option<Instant>,
}

impl Worker {
    pub(crate) fn new(
        id: Uuid,
        cluster: Arc<Cluster>,
        hv: Arc<dyn Hypervisor>,
        config: Arc<ReconcilerConfig>,
        restores: Restores,
        held: Held,
    ) -> Self {
        Self {
            id,
            cluster,
            hv,
            config,
            restores,
            held,
            stop_requested: None,
            stopped_by_us: None,
            force_requested: false,
            failures: FailureTracker::default(),
            backoff: None,
            power_off: None,
            was_running: false,
            awaiting_event: None,
            disks_active: false,
            disks_shared: true,
            held_event: None,
            fenced: false,
            ha_running: false,
            wake_at: None,
        }
    }

    /// 보내는 쪽이 모두 사라지면 남은 트리거를 처리하고 끝난다. `previous`는 같은 VM을 맡았던
    /// 작업자다. 같은 VM에 대한 드라이버 호출이 겹치지 않도록 그것이 끝난 뒤에 시작한다.
    pub(crate) async fn run(
        mut self,
        previous: Option<JoinHandle<()>>,
        mut rx: mpsc::UnboundedReceiver<Trigger>,
    ) {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        loop {
            let trigger = match self.wake_at {
                Some(at) => tokio::select! {
                    t = rx.recv() => t,
                    () = tokio::time::sleep_until(at) => Some(Trigger::Sync),
                },
                None => rx.recv().await,
            };
            let Some(trigger) = trigger else {
                self.exit().await;
                return;
            };
            // 트리거는 차례로 처리하므로 앞의 단계는 이미 끝났다. 마이그레이션 Task가 디스크를
            // 공유로 바꿀 수 있으므로 보류가 풀린 뒤 다시 배타로 바꾼다.
            if let Trigger::Barrier(done) = trigger {
                self.disks_shared = true;
                let _ = done.send(());
                continue;
            }
            self.wake_at = None;
            if let Err(e) = self.step(trigger).await {
                tracing::warn!(vm = %self.id, error = %e, "reconcile failed; will retry");
                self.wake(Instant::now() + self.config.retry_interval);
            }
        }
    }

    /// 작업자가 끝난다. VM이 이 노드에서 빠졌고(삭제, 다른 노드로 옮김) QEMU도 없으면 도는 HA VM에서
    /// 뺀다. QEMU가 아직 돌면 남겨 리스가 끝났을 때 watchdog가 시간을 지키게 한다. reconciler가 멈춰
    /// 끝날 때는 그대로 둔다. VM은 계속 돌고, guard는 정상 종료 때 이 목록으로 watchdog를 멈출지 정한다.
    async fn exit(&self) {
        let Some(guard) = &self.config.ha else { return };
        match self.cluster.get::<VmSpec>(self.id) {
            Ok(Some(vm)) if vm.spec.node == Some(self.config.node) => {}
            Ok(_) => match self.hv.inspect(self.id).await {
                Ok(None) => guard.set_running(self.id, false),
                Ok(Some(_)) => {}
                Err(e) => tracing::warn!(vm = %self.id, error = %e, "cannot inspect VM"),
            },
            Err(e) => tracing::warn!(vm = %self.id, error = %e, "cannot read VM"),
        }
    }

    fn wake(&mut self, at: Instant) {
        self.wake_at = Some(self.wake_at.map_or(at, |w| w.min(at)));
    }

    async fn step(&mut self, trigger: Trigger) -> Result<(), StepError> {
        // 마이그레이션 Task가 status와 QEMU를 다룬다. 끝나면(`end_migration`) 다시 깨운다. 그동안
        // QEMU가 끝났으면(크래시 등) 보류가 풀린 뒤 그 이벤트대로 다룬다(재시작 정책).
        if self.held.lock().unwrap().contains(&self.id) {
            if let Trigger::Event(kind) = trigger {
                self.held_event = Some(kind);
            }
            return Ok(());
        }
        let node = self.config.node;
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        let Some(mut vm) = vm.filter(|vm| vm.spec.node == Some(node)) else {
            // 다른 노드로 옮겼다(보낸 원본이 꺼진 이벤트).
            self.held_event = None;
            self.unassigned().await?;
            return Ok(());
        };
        // 남은 마이그레이션을 정리하는 쓰기는 과반수가 있어야 하므로 그 전에 끈다.
        self.fence(&vm).await?;
        if vm.status.migration.is_some() || vm.status.phase == VmPhase::Migrating {
            match self.clear_migration(&vm).await? {
                Some(current) => {
                    self.disks_shared = true;
                    vm = current;
                }
                // 그 사이 바뀌었다. 다시 읽어 맞춘다.
                None => {
                    self.wake(Instant::now());
                    return Ok(());
                }
            }
        }
        if let Some(kind) = self.held_event.take() {
            self.on_event(&vm, kind);
        }
        match trigger {
            Trigger::Event(kind) => self.on_event(&vm, kind),
            // running을 원할 때 온 명령은 버린다. 남겨 두면 나중의 종료가 ACPI 없이 강제 종료된다.
            Trigger::ForceStop if vm.spec.power == PowerState::Stopped => {
                self.force_requested = true
            }
            Trigger::ForceStop => {}
            Trigger::Sync | Trigger::Barrier(_) => {}
        }
        if self.power_off.is_none() {
            let status = self.reconcile(&mut vm).await?;
            if self.power_off.is_none() {
                if status != vm.status {
                    self.write_status(status, vm).await?;
                }
                return Ok(());
            }
        }
        self.write_power_off(vm).await
    }

    fn on_event(&mut self, vm: &Vm, kind: EventKind) {
        let now = Instant::now();
        if kind != EventKind::Started {
            self.was_running = false;
            self.awaiting_event = None;
            self.stop_requested = None;
        }
        let requested = match kind {
            EventKind::Started => return,
            EventKind::Stopped { .. } | EventKind::Crashed => self
                .stopped_by_us
                .take()
                .is_some_and(|at| now < at + self.config.acpi_timeout),
        };
        // 원하던 대로 꺼졌다. 강제 종료의 결과로 Crashed가 와도 실패로 세지 않는다.
        if vm.spec.power == PowerState::Stopped {
            return;
        }
        match kind {
            // 우리가 요청한 종료가, 원하는 상태가 다시 running으로 바뀐 뒤에 끝났다. 다시 시작한다.
            // 강제 종료의 결과로 Crashed가 와도 크래시로 보지 않는다.
            _ if requested => {}
            // 게스트가 스스로 꺼졌다(설계 5.3). `guest_initiated`는 opld가 요청한 ACPI 종료에서도
            // true이므로 우리가 요청했는지로 먼저 가린다.
            EventKind::Stopped {
                guest_initiated: true,
            } => self.power_off = Some(None),
            // 우리가 요청하지 않았고 게스트의 결정도 아니다(호스트의 SIGTERM 등). 원하는 상태는
            // 그대로 두므로 실패로 세지 않고 다시 시작한다.
            EventKind::Stopped {
                guest_initiated: false,
            } => {}
            EventKind::Crashed => match vm.spec.restart {
                RestartPolicy::Never => self.power_off = Some(Some("VM crashed".into())),
                RestartPolicy::OnFailure => self.record_failure("VM crashed".into()),
            },
            EventKind::Started => {}
        }
    }

    /// 리스가 끝났으면(stale) 이 노드에서 도는 HA VM을 강제로 끈다(설계 §3.3). 그 사이 리더가 다른
    /// 노드에서 켤 수 있다. 우리가 요청한 종료로 표시하므로 실패로 세지 않고 원하는 상태도 바꾸지
    /// 않는다. 원하는 상태가 stopped여도 끈다(끄는 중에 과반수 쪽에서 다시 running으로 바뀌었을 수 있다).
    async fn fence(&mut self, vm: &Vm) -> Result<(), StepError> {
        if !vm.spec.ha || !self.config.ha.as_ref().is_some_and(|g| g.stale()) {
            return Ok(());
        }
        if self.hv.inspect(self.id).await?.is_none() {
            return Ok(());
        }
        tracing::warn!(vm = %self.id, "HA lease lost; force stopping the VM");
        self.stopped_by_us = Some(Instant::now());
        self.stop_requested = None;
        self.force_requested = false;
        self.hv.stop(self.id, StopMode::Force).await?;
        // 우리가 껐으므로 종료 이벤트를 기다리지 않는다.
        self.was_running = false;
        self.fenced = true;
        Ok(())
    }

    /// 삭제되었거나 다른 노드로 배정되었다. 돌고 있더라도 멋대로 끄지 않는다. 단 도는 HA VM으로 알린
    /// VM은 리스가 끝났으면 강제로 끈다(그 사이 리더가 다른 노드에서 켤 수 있다). QEMU가 없을 때만 도는
    /// HA VM에서 뺀다.
    async fn unassigned(&mut self) -> Result<(), StepError> {
        let mut actual = self.hv.inspect(self.id).await?;
        let fenced = actual.is_some()
            && self.ha_running
            && self.config.ha.as_ref().is_some_and(|g| g.stale());
        if fenced {
            tracing::warn!(vm = %self.id, "HA lease lost; force stopping the VM assigned elsewhere");
            self.stopped_by_us = Some(Instant::now());
            self.hv.stop(self.id, StopMode::Force).await?;
            self.was_running = false;
            actual = self.hv.inspect(self.id).await?;
        }
        match actual {
            // 방금 강제로 껐다. 그대로 둔다는 로그를 남기지 않는다.
            Some(_) if fenced => {}
            Some(_) => {
                tracing::warn!(vm = %self.id, "VM is running but not assigned to this node; leaving it alone");
            }
            None => {
                if let Some(guard) = &self.config.ha {
                    guard.set_running(self.id, false);
                }
                self.ha_running = false;
            }
        }
        Ok(())
    }

    /// HA VM을 켜거나 멈춘 QEMU를 이어서 실행해도 되는지 본다: 리스가 살아 있고 watchdog가 켜져
    /// 있어야 한다. 안 되면 status에 남길 이유를 돌려주고, 펜싱이 있는 노드면 잠시 뒤 다시 본다
    /// (실패로 세지 않는다). 리스가 바뀌면 reconciler가 따로 깨운다.
    fn ha_blocked(&mut self, vm: &Vm) -> Option<String> {
        if !vm.spec.ha {
            return None;
        }
        let Some(guard) = &self.config.ha else {
            return Some(HA_UNAVAILABLE.into());
        };
        let ready = if guard.fresh() {
            guard.arm().map_err(|e| e.to_string())
        } else if self.fenced {
            Err(HA_FENCED.into())
        } else {
            Err(HA_WAITING.into())
        };
        let reason = ready.err()?;
        self.wake(Instant::now() + self.config.retry_interval);
        Some(reason)
    }

    /// 이 노드에서 HA VM이 도는지 guard에 알린다.
    fn report_running(&mut self, vm: &Vm, running: bool) {
        if let Some(guard) = &self.config.ha {
            self.ha_running = vm.spec.ha && running;
            guard.set_running(self.id, self.ha_running);
        }
    }

    fn record_failure(&mut self, reason: String) {
        let now = Instant::now();
        match self.failures.record(now, &self.config.restart) {
            Next::RetryAfter(delay) => {
                self.backoff = Some((now + delay, format!("{reason}; retrying in {delay:?}")));
            }
            Next::GiveUp => {
                let n = self.config.restart.max_failures;
                self.power_off = Some(Some(format!("{reason}; gave up after {n} failures")));
            }
        }
    }

    /// 원하는 상태를 향해 한 걸음 움직이고, 기록해야 할 status를 돌려준다. 그 전에 status를 먼저
    /// 기록했으면 `vm`을 다시 읽은 것으로 바꾼다.
    async fn reconcile(&mut self, vm: &mut Vm) -> Result<VmStatus, StepError> {
        let mut actual = self.hv.inspect(self.id).await?;
        if let Some(state) = &actual
            && state.status == postmigrate()
            && !(vm.status.node == Some(self.config.node)
                && matches!(
                    vm.status.phase,
                    VmPhase::Running | VmPhase::Migrating | VmPhase::Starting
                ))
        {
            // 확정은 늘 status.node를 대상으로 바꾼다. 이 노드에서 돌던 VM이 아니면 다른 노드로 확정된
            // 마이그레이션의 원본이 정리되지 않고 남은 것이다(그 뒤 VM이 이 노드로 다시 옮겨졌다). 멈춘
            // 메모리는 그 사이 바뀐 디스크와 맞지 않으므로 이어서 실행하지 않고 끈 뒤 새로 시작한다.
            // 리스를 기다리는 HA VM의 원본은 이 작업자가 Starting으로 적어 둔 것이다(아래). 기다리는
            // 동안 HA가 꺼져도 같은 원본이므로 HA 여부는 보지 않는다.
            tracing::warn!(vm = %self.id, "stopping the stale source of a migration committed elsewhere");
            self.stopped_by_us = Some(Instant::now());
            if !self.hv.stop_migrated(self.id).await? {
                self.hv.stop(self.id, StopMode::Force).await?;
            }
            actual = self.hv.inspect(self.id).await?;
        }
        self.report_running(vm, actual.is_some());
        let mut status = vm.status.clone();
        status.node = Some(self.config.node);
        let now = Instant::now();
        if std::mem::replace(&mut self.was_running, actual.is_some()) && actual.is_none() {
            self.awaiting_event = Some(now + EVENT_GRACE);
        }
        if actual.is_some() {
            self.disks_active = true;
            // 이 노드의 QEMU가 이 노드의 변수 파일을 쓰고 있다(다시 붙음, 넘겨받은 마이그레이션 포함).
            self.own_firmware(vm, &mut status);
        }
        match (vm.spec.power, actual) {
            (PowerState::Running, Some(state)) => {
                // 진행 중인 종료 요청을 취소한다. 게스트가 이미 받은 요청대로 꺼지면
                // `stopped_by_us`로 가려 다시 시작한다.
                self.stop_requested = None;
                self.force_requested = false;
                status.phase = VmPhase::Running;
                status.pid = Some(state.pid);
                status.last_error = None;
                if state.status != RuntimeStatus::Running
                    && let Some(reason) = self.ha_blocked(vm)
                {
                    // 멈춘 HA VM(넘겨받은 마이그레이션 등)은 리스가 있어야 이어서 실행한다.
                    status.phase = VmPhase::Starting;
                    status.last_error = Some(reason);
                } else if state.status != RuntimeStatus::Running {
                    // opld가 기동 도중(cont 전)이나 스냅샷으로 멈춘 채 죽었거나 QMP로 다룰 수 없다.
                    // 붙어 있으면 디스크를 다시 해석하지 않고(LVM이 응답하지 않을 수 있다) 이어서
                    // 실행하고, 아니면 start()가 다시 붙는다. 실패해도 돌고 있는 VM이므로 끄거나
                    // 실패로 세지 않는다.
                    let unresponsive = |e: DriverError| ContinueError::Unresponsive(e.to_string());
                    let mut continued = if state.status == postmigrate() {
                        // 마이그레이션을 보내고 멈춘 원본인데 이 노드에서 돌던 VM이고(위에서 가렸다)
                        // 표시가 없고(위에서 지웠다) 마이그레이션 Task도 다루지 않는다(보류가 아니다). 확정 쓰기는 표시가
                        // 있는 revision에서만 적용되므로 이제 확정될 수 없다: 이어서 실행한다(되돌리기의
                        // 재개 실패, 되돌린 뒤 끝난 마이그레이션).
                        tracing::warn!(vm = %self.id, "resuming the source of a migration that was not committed");
                        self.hv.resume(self.id).await.map_err(unresponsive)
                    } else {
                        match self.hv.resume_left_stopped(self.id).await {
                            Ok(true) => Ok(()),
                            Ok(false) => self.reattach(vm).await,
                            Err(e) => Err(unresponsive(e)),
                        }
                    };
                    // 이 노드로 마이그레이션을 받아 멈춰 있는 QEMU다(확정되어 이 노드에 배정됐다).
                    // 받는 중 표시가 있을 때만 이어서 실행하고 표시를 지운다.
                    if continued.is_ok() && state.status == RuntimeStatus::Paused {
                        continued = self.hv.finish_incoming(self.id).await.map_err(unresponsive);
                        self.disks_shared |= continued.is_ok();
                    }
                    match continued {
                        Ok(()) => {}
                        Err(ContinueError::Unresponsive(e)) => {
                            tracing::warn!(vm = %self.id, error = %e, "VM is unresponsive; will retry");
                            status.last_error = Some(format!("VM is unresponsive: {e}"));
                            self.wake(now + self.config.retry_interval);
                        }
                        Err(ContinueError::NotFresh(reason)) => {
                            status.phase = VmPhase::Starting;
                            status.last_error = Some(reason.into());
                            self.wake(now + self.config.retry_interval);
                        }
                        // 켜지 않고 status도 쓰지 않는다. 다시 읽어 맞춘다.
                        Err(ContinueError::Changed(current)) => {
                            if let Some(current) = current {
                                *vm = *current;
                            }
                            self.wake(Instant::now());
                            return Ok(vm.status.clone());
                        }
                    }
                }
                if self.disks_shared && status.last_error.is_none() {
                    self.exclusive_disks(vm).await;
                }
                if vm.spec.ha && status.last_error.is_none() {
                    self.attach_ha(&mut status);
                }
            }
            // 방금까지 돌던 VM이 사라졌다. 왜 꺼졌는지(게스트 종료, 크래시) 이벤트로 안 뒤에 움직인다.
            // 기다려도 오지 않으면(이벤트 유실) 원하는 상태대로 다시 시작한다.
            (PowerState::Running, None)
                if self.awaiting_event.is_some_and(|deadline| now < deadline) =>
            {
                self.wake(self.awaiting_event.unwrap());
            }
            (PowerState::Running, None) => {
                self.awaiting_event = None;
                self.force_requested = false;
                self.deactivate_disks(vm).await;
                if self.backoff.as_ref().is_some_and(|(at, _)| *at <= now) {
                    self.backoff = None;
                }
                if let Some(reason) = self.ha_blocked(vm) {
                    status.phase = if self.config.ha.is_some() {
                        VmPhase::Starting
                    } else {
                        VmPhase::Stopped
                    };
                    status.pid = None;
                    status.last_error = Some(reason);
                    return Ok(status);
                }
                if self.backoff.is_none() {
                    // 켜는 동안(Starting 기록, 디스크 해석, 변수 받아 오기) 리스가 끝나면 watchdog가
                    // 시간을 지키도록 도는 HA VM으로 먼저 알린다. 켜지 못하면 뺀다.
                    self.report_running(vm, true);
                    let started = self.start_stopped(vm, &mut status).await;
                    if !self.was_running {
                        self.report_running(vm, false);
                    }
                    if let Some(current) = started? {
                        return Ok(current);
                    }
                }
                if let Some((at, reason)) = self.backoff.clone() {
                    self.wake(at);
                    status.phase = VmPhase::Starting;
                    status.pid = None;
                    status.last_error = Some(reason);
                }
            }
            (PowerState::Stopped, Some(state)) => {
                self.backoff = None;
                match self.stop_requested {
                    _ if self.force_requested => {
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.force_requested = false;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    None => {
                        // 요청이 실패해도(QMP로 다룰 수 없는 VM) acpi_timeout 뒤 강제 종료한다.
                        self.stop_requested = Some(now);
                        self.stopped_by_us = Some(now);
                        self.wake(now + self.config.acpi_timeout);
                        if let Err(e) = self.hv.stop(self.id, StopMode::Acpi).await {
                            tracing::warn!(vm = %self.id, error = %e, "ACPI shutdown request failed; will force after timeout");
                        }
                    }
                    Some(at) if now >= at + self.config.acpi_timeout => {
                        tracing::warn!(vm = %self.id, "guest ignored ACPI shutdown; forcing");
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    Some(at) => self.wake(at + self.config.acpi_timeout),
                }
                status.phase = VmPhase::Stopping;
                status.pid = Some(state.pid);
            }
            (PowerState::Stopped, None) => {
                self.fenced = false;
                self.stop_requested = None;
                self.force_requested = false;
                self.awaiting_event = None;
                self.deactivate_disks(vm).await;
                self.backoff = None;
                self.failures.clear();
                // 포기해서 멈춘 VM은 사용자가 다시 시작할 때까지 Failed로 남긴다.
                if status.phase != VmPhase::Failed {
                    status.phase = VmPhase::Stopped;
                }
                status.pid = None;
            }
        }
        Ok(status)
    }

    /// 멈춘 VM을 켠다(Starting 기록, [`Worker::start`]). 켰으면 `was_running`이다. 켜지 않고 그대로
    /// 돌려줄 status가 있으면 Some이다.
    async fn start_stopped(
        &mut self,
        vm: &mut Vm,
        status: &mut VmStatus,
    ) -> Result<Option<VmStatus>, StepError> {
        if vm.status.phase == VmPhase::Running {
            // 저장된 phase가 Running인 채 다시 켜면(QEMU가 스스로 끝났다) 그사이 API·Task
            // 실행기가 실행 중 VM으로 보고 디스크를 늘린다(공유 LVM은 늘린 부분을 0으로 채우기
            // 전에 QEMU가 연다). 디스크를 해석하기 전에 Starting을 먼저 기록해, 먼저 들어온
            // 크기 늘리기 Task는 resolve가 보고 나중 것은 API·실행기가 거절하게 한다. 기록하지
            // 못해도(예: 과반수 없음 — 그동안은 Task도 만들 수 없다) VM은 다시 켠다(설계 10절
            // 불변식 4). 기록이 시간 초과로 실패한 사이 과반수가 돌아오면 좁은 틈이 남는다.
            // HA VM은 켜지 않고 다시 시도한다: 과반수가 없으면 리더가 다른 노드에서 켤 수 있다.
            let mut starting = status.clone();
            starting.phase = VmPhase::Starting;
            starting.pid = None;
            match self.write_status(starting, vm.clone()).await {
                Ok(()) => match self.cluster.get::<VmSpec>(self.id)? {
                    // 그 사이 다른 노드로 옮겨졌다. 켜지 않고(status도 쓰지 않고) 다시 읽어 맞춘다.
                    Some(current) if current.spec.node != Some(self.config.node) => {
                        *vm = current;
                        self.wake(Instant::now());
                        return Ok(Some(vm.status.clone()));
                    }
                    Some(current) => *vm = current,
                    None => return Ok(Some(status.clone())),
                },
                Err(e) if vm.spec.ha => return Err(e),
                Err(e) => {
                    tracing::warn!(vm = %self.id, error = %e, "cannot record starting phase; restarting anyway");
                }
            }
        }
        match self.start(vm).await? {
            Start::Started => {}
            Start::NotStarted => return Ok(None),
            // 켜지 않고 status도 쓰지 않는다. 다시 읽어 맞춘다.
            Start::Changed(current) => {
                if let Some(current) = current {
                    *vm = *current;
                }
                self.wake(Instant::now());
                return Ok(Some(vm.status.clone()));
            }
        }
        // 이제 돌고 있다. 곧바로 꺼져도 이벤트보다 먼저 온 재동기화가 다시 시작하지
        // 않고 이벤트를 기다리게 한다.
        self.was_running = true;
        self.fenced = false;
        self.report_running(vm, true);
        // 이 노드의 변수 파일(받아 왔거나 로컬, 없으면 템플릿)로 켰다.
        self.own_firmware(vm, status);
        match self.hv.inspect(self.id).await? {
            Some(state) => {
                status.phase = VmPhase::Running;
                status.pid = Some(state.pid);
                status.last_error = None;
            }
            // 시작하자마자 꺼졌다. 뒤따르는 이벤트에서 다시 맞춘다.
            None => {
                self.was_running = false;
                let deadline = Instant::now() + EVENT_GRACE;
                self.awaiting_event = Some(deadline);
                self.wake(deadline);
                status.phase = VmPhase::Starting;
                status.pid = None;
            }
        }
        Ok(None)
    }

    /// VM을 시작한다. 시작하지 못하고 실패를 기록했으면(백오프 또는 포기) `NotStarted`. HA VM의 리스가
    /// 그 사이 끝났으면 실패로 세지 않고 잠시 뒤 다시 본다(백오프, `NotStarted`). HA VM이 그 사이
    /// 바뀌었으면 `Changed`.
    async fn start(&mut self, vm: &Vm) -> Result<Start, StepError> {
        let spec = match self.resolve(vm).await {
            Ok(spec) => spec,
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 풀이 이 노드에서 아직 준비되지 않았거나(NFS 서버가 멈춤, 마운트 전 등) 디스크를 늘리는
            // 중이다. 끝나면 시작할 수 있으므로 실패로 세지 않고 잠시 뒤 다시 시도한다.
            Err(e @ (ResolveError::PoolUnavailable(..) | ResolveError::VolumeResizing(_))) => {
                let delay = self.config.retry_interval;
                self.backoff = Some((
                    Instant::now() + delay,
                    format!("{e}; retrying in {delay:?}"),
                ));
                return Ok(Start::NotStarted);
            }
            // 참조가 틀렸거나, 이 노드가 지원하지 않는 풀(NFS on macOS)이거나, 저장소가 볼륨을
            // 내주지 못했다. spec을 고치거나 VM을 옮겨야 하므로 다시 시도하지 않는다.
            Err(e) => {
                self.restores.lock().unwrap().remove(&self.id);
                self.power_off = Some(Some(e.to_string()));
                return Ok(Start::NotStarted);
            }
        };
        self.disks_active = true;
        // 방금 이 노드에서 켰다(배타).
        self.disks_shared = false;
        self.fetch_firmware(vm).await;
        // 켜기 문턱을 지난 뒤 여기까지 오는 사이 리스가 끝났거나 VM이 바뀌었을 수 있다.
        match self.ha_recheck(vm)? {
            Recheck::Go => {}
            // 실패로 세지 않고 다시 본다.
            Recheck::NotFresh(reason) => {
                self.deactivate_disks(vm).await;
                self.backoff = Some((Instant::now() + self.config.retry_interval, reason.into()));
                return Ok(Start::NotStarted);
            }
            Recheck::Changed(current) => {
                self.deactivate_disks(vm).await;
                return Ok(Start::Changed(current));
            }
        }
        // 실패해도 다시 쓰지 않는다.
        let restore = self.restores.lock().unwrap().remove(&self.id);
        let started = self.hv.start(&spec, restore.as_deref()).await;
        if started.is_err() {
            self.deactivate_disks(vm).await;
        }
        match (started, restore) {
            (Ok(()), _) => Ok(Start::Started),
            // 태그 없이 다시 시도하면 스냅샷이 아닌 처음부터 부팅하게 되므로 일시 오류라도 멈춘다.
            (Err(e), Some(tag)) => {
                self.power_off = Some(Some(format!("failed to restore snapshot {tag}: {e}")));
                Ok(Start::NotStarted)
            }
            (Err(e), None) if e.is_transient() => {
                self.record_failure(format!("failed to start: {e}"));
                Ok(Start::NotStarted)
            }
            (Err(e), None) => {
                self.power_off = Some(Some(format!("failed to start: {e}")));
                Ok(Start::NotStarted)
            }
        }
    }

    /// HA VM을 켜거나 이어서 실행하기 직전에 다시 본다: 리스가 살아 있어야 하고, 로컬 저장소의 VM이
    /// 여전히 이 노드에 배정되어 있고 원하는 상태가 running이어야 한다. 리스가 살아 있으면 그 갱신
    /// 인덱스까지 이 노드에 적용되어 있으므로 과반수 쪽의 쓰기(다른 노드로의 복구 등)를 본다. HA가 아닌
    /// VM은 늘 `Go`다.
    fn ha_recheck(&self, vm: &Vm) -> Result<Recheck, ClusterError> {
        if !vm.spec.ha {
            return Ok(Recheck::Go);
        }
        if !self.config.ha.as_ref().is_some_and(|g| g.fresh()) {
            let reason = if self.fenced { HA_FENCED } else { HA_WAITING };
            return Ok(Recheck::NotFresh(reason));
        }
        Ok(match self.cluster.get::<VmSpec>(self.id)? {
            Some(current)
                if current.spec.node == Some(self.config.node)
                    && current.spec.power == PowerState::Running =>
            {
                Recheck::Go
            }
            current => Recheck::Changed(current.map(Box::new)),
        })
    }

    /// 이미 도는 HA VM에 붙는다: watchdog를 켠다(opld를 다시 시작한 뒤 등). 리스가 살아 있지 않으면
    /// 리스가 바뀔 때 다시 본다. 펜싱이 꺼진 노드면 끄지 않고 그 사실만 남긴다.
    fn attach_ha(&mut self, status: &mut VmStatus) {
        let Some(guard) = &self.config.ha else {
            status.last_error = Some(HA_UNAVAILABLE.into());
            return;
        };
        match guard.arm() {
            Ok(()) | Err(ArmError::NotFresh) => {}
            Err(e) => {
                status.last_error = Some(e.to_string());
                self.wake(Instant::now() + self.config.retry_interval);
            }
        }
    }

    /// 멈춘 UEFI VM을 새로 켜기 전에, 변수 파일을 가진 노드(`firmware_node`)가 다른 노드면 거기서
    /// 받아 와 이 노드의 파일을 바꾼다(설계 §5). 받지 못하면(그 노드가 오프라인이다, 파일이 없다 등)
    /// 경고만 남기고 이 노드의 파일(없으면 템플릿)로 켠다. 도는 VM에 다시 붙거나 마이그레이션을
    /// 넘겨받을 때는 부르지 않는다(QEMU가 이미 이 노드의 파일을 쓴다).
    async fn fetch_firmware(&self, vm: &Vm) {
        let (Some(fetcher), Some(node)) = (&self.config.firmware, vm.status.firmware_node) else {
            return;
        };
        if vm.spec.firmware != Firmware::Uefi || node == self.config.node {
            return;
        }
        match fetcher.fetch(self.id, node).await {
            Ok(()) => tracing::info!(vm = %self.id, from = %node, "fetched UEFI variables"),
            Err(e) => {
                tracing::warn!(vm = %self.id, from = %node, error = %e, "cannot fetch UEFI variables; starting with this node's copy");
            }
        }
    }

    /// UEFI VM이 이 노드의 변수 파일로 돈다: 이 노드를 `firmware_node`로 남긴다. BIOS VM은 그대로 둔다.
    fn own_firmware(&self, vm: &Vm, status: &mut VmStatus) {
        if vm.spec.firmware == Firmware::Uefi {
            status.firmware_node = Some(self.config.node);
        }
    }

    /// 켜져 있을 수 있는 디스크 볼륨을 끈다(QEMU가 끝난 뒤). 끄지 못하면 경고만 남긴다(켜진 채 남은
    /// 볼륨은 Task 실행기의 정리가 다시 끈다).
    async fn deactivate_disks(&mut self, vm: &Vm) {
        // 끄면 공유도 풀린다.
        self.disks_shared = false;
        if !std::mem::take(&mut self.disks_active) {
            return;
        }
        for disk in &vm.spec.disks {
            let found = self
                .cluster
                .get_by_name::<VolumeSpec>(&disk.volume)
                .and_then(|volume| {
                    let Some(volume) = volume else {
                        return Ok(None);
                    };
                    let pool = self
                        .cluster
                        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                    Ok(pool.map(|pool| (volume, pool)))
                });
            let (volume, pool) = match found {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                    continue;
                }
            };
            if let Err(e) = self
                .config
                .storage
                .deactivate_volume(&pool, volume.id, volume.spec.format)
                .await
            {
                tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
            }
        }
    }

    /// 도는 VM의 디스크 볼륨을 다시 이 노드만 쓰게 한다(공유 LVM은 배타 잠금). 마이그레이션의 다른
    /// 쪽 노드가 아직 볼륨을 끄지 않았으면(원본이 정리 전이다) 잠시 뒤 다시 한다. 그동안에도 VM은
    /// 돈다.
    async fn exclusive_disks(&mut self, vm: &Vm) {
        match set_disks_shared(&self.cluster, vm, &*self.config.storage, false).await {
            Ok(()) => self.disks_shared = false,
            Err(e @ (ResolveError::PoolUnavailable(..) | ResolveError::Cluster(_))) => {
                tracing::info!(vm = %self.id, error = %e, "disks are still shared with another node; will retry");
                self.wake(Instant::now() + self.config.retry_interval);
            }
            Err(e) => {
                tracing::warn!(vm = %self.id, error = %e, "cannot make disks exclusive to this node again");
                self.disks_shared = false;
            }
        }
    }

    /// 끝나지 않은 채 남은 마이그레이션 표시(`Migrating`, `status.migration`)를 지운다. 이 노드의
    /// 마이그레이션 Task가 다루지 않는 VM이므로, 원본 opld가 확정 전에 죽었거나(원본 노드) 확정 뒤
    /// 넘겨받는 중이다(대상 노드). 읽은 revision으로만 쓰므로 그 사이 확정되었으면(남은 확정 쓰기가
    /// 먼저 적용됨) 쓰지 않고 `None`이다. 쓰고 나면 그 확정은 더는 적용되지 않으므로, 이 노드의 QEMU가
    /// 아직 보내고 있으면 취소하고(나중에 끝나 원본이 멈추지 않게), 마이그레이션을 보내고 멈춰 있으면
    /// (`postmigrate`, 취소하는 사이에 끝난 경우 포함) 이어서 실행한다(HA VM은 reconcile이 리스를 확인한
    /// 뒤에). 대상의 QEMU는 확정 뒤에만
    /// 돌므로 두 QEMU가 함께 돌지 않는다. 다시 읽은 VM을 돌려준다.
    async fn clear_migration(&self, vm: &Vm) -> Result<Option<Vm>, StepError> {
        let mut status = vm.status.clone();
        status.migration = None;
        if status.phase == VmPhase::Migrating {
            status.phase = VmPhase::Running;
        }
        match self
            .cluster
            .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => return Ok(None),
            r => r?,
        };
        self.hv.cancel_migration(self.id).await?;
        // HA VM은 reconcile이 리스와 watchdog를 확인한 뒤 이어서 실행한다(이 노드에서 도는 VM의 원본).
        if !vm.spec.ha
            && let Some(state) = self.hv.inspect(self.id).await?
            && state.status == postmigrate()
        {
            tracing::warn!(vm = %self.id, "resuming the source of a migration that did not complete");
            self.hv.resume(self.id).await?;
        }
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        Ok(vm.filter(|vm| vm.spec.node == Some(self.config.node)))
    }

    /// VM의 런타임 정의. 디스크 볼륨을 활성화한다.
    async fn resolve(&self, vm: &Vm) -> Result<VmRuntimeSpec, ResolveError> {
        let c = &self.config;
        runtime_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage).await
    }

    /// 살아 있는 VM에 다시 붙거나 이어서 실행한다(`start`는 멱등이다). 참조 해석 오류도 돌려준다.
    /// QEMU가 이미 디스크를 열었으므로 크기 늘리기 Task를 기다리지 않는다. HA VM은 디스크를 해석한 뒤
    /// 다시 본다([`Worker::ha_recheck`]).
    async fn reattach(&self, vm: &Vm) -> Result<(), ContinueError> {
        let c = &self.config;
        let unresponsive = |e: &dyn std::fmt::Display| ContinueError::Unresponsive(e.to_string());
        let spec = running_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage)
            .await
            .map_err(|e| unresponsive(&e))?;
        match self.ha_recheck(vm) {
            Ok(Recheck::Go) => {}
            Ok(Recheck::NotFresh(reason)) => return Err(ContinueError::NotFresh(reason)),
            Ok(Recheck::Changed(current)) => return Err(ContinueError::Changed(current)),
            Err(e) => return Err(unresponsive(&e)),
        }
        self.hv
            .start(&spec, None)
            .await
            .map_err(|e| unresponsive(&e))
    }

    /// `vm`을 읽은 뒤 계산한 status를 기록한다. `snapshot`(현재 스냅샷 위치)과 `schedule_error`는
    /// Task 실행기·예약 작업이 쓰는 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이
    /// 누가 썼으면 다시 읽어 그 값으로 다시 쓴다. `firmware_node`도 이 작업자가 바꾸지 않았으면
    /// 저장소의 것을 지킨다(스냅샷 되돌리기가 쓴다). 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        let keep_firmware = status.firmware_node == vm.status.firmware_node;
        loop {
            status.snapshot = vm.status.snapshot;
            status.schedule_error = vm.status.schedule_error.clone();
            if keep_firmware {
                status.firmware_node = vm.status.firmware_node;
            }
            match self
                .cluster
                .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
                .await
            {
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                    match self.cluster.get::<VmSpec>(self.id)? {
                        Some(current) => vm = current,
                        None => return Ok(()),
                    }
                }
                r => return r.map(|_| ()).map_err(Into::into),
            }
        }
    }

    /// spec.power를 stopped로 바꾸고 status를 기록한다. 둘 다 성공해야 끝난다.
    async fn write_power_off(&mut self, mut vm: Vm) -> Result<(), StepError> {
        let error = self.power_off.clone().flatten();
        if vm.spec.power == PowerState::Running {
            let mut spec = vm.spec.clone();
            spec.power = PowerState::Stopped;
            // 이 단계에서 읽은 뒤 spec이 바뀌었으면 RevisionMismatch로 거부된다. 다음 단계에서
            // 다시 읽은 revision으로 쓰므로, 그 사이 사용자가 바꾼 spec.power도 stopped로 덮어쓴다.
            // status는 방금 쓴 revision으로 곧바로 이어 쓴다.
            vm.revision = self
                .cluster
                .update_spec(self.id, Some(vm.revision), vm.labels.clone(), &spec)
                .await?;
        }
        let status = VmStatus {
            phase: if error.is_some() {
                VmPhase::Failed
            } else {
                VmPhase::Stopped
            },
            node: Some(self.config.node),
            pid: None,
            last_error: error,
            snapshot: vm.status.snapshot,
            schedule_error: vm.status.schedule_error.clone(),
            migration: None,
            firmware_node: vm.status.firmware_node,
        };
        self.write_status(status, vm).await?;
        self.power_off = None;
        self.backoff = None;
        self.failures.clear();
        Ok(())
    }
}

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}
```

`crates/opl-reconciler/tests/ha.rs`를 만든다:

```rust
//! HA VM과 리스(설계 §3.3 워커 규칙): 켜기·`cont` 문턱, 리스를 잃으면 강제 종료, 펜싱이 꺼진 노드,
//! 도는 HA VM 알림(`set_running`).

mod common;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use async_trait::async_trait;
use common::{Env, vm};
use opl_cluster::ClusterError;
use opl_driver::{
    Capacity, DriverError, Hypervisor, RuntimeStatus, SourceFormat, Storage, VolumeInfo,
    VolumeSource,
};
use opl_reconciler::ha::{LeaseRenewer, Watchdog, WatchdogOpener};
use opl_reconciler::{FirmwareError, FirmwareFetcher, HaConfig, HaGuard, ReconcilerConfig};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{
    DiskBus, DiskSpec, MigrationStatus, PowerState, RestartPolicy, Vm, VmPhase, VmSpec,
};
use uuid::Uuid;

const WAITING: &str = "waiting for the cluster majority before starting an HA VM";
const FENCED: &str = "fenced: lost the cluster majority";
const UNAVAILABLE: &str = "HA is not available on this node";

/// 정해 둔 대로 곧바로 갱신에 성공하거나 실패한다.
struct FakeRenewer(Mutex<bool>);

impl FakeRenewer {
    fn set_ok(&self, ok: bool) {
        *self.0.lock().unwrap() = ok;
    }
}

#[async_trait]
impl LeaseRenewer for FakeRenewer {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), ClusterError> {
        if *self.0.lock().unwrap() {
            Ok(())
        } else {
            Err(ClusterError::Timeout)
        }
    }
}

/// 장치 대신 한 일을 적는다.
#[derive(Default)]
struct FakeWatchdog {
    log: Arc<Mutex<Vec<&'static str>>>,
    fail_open: Mutex<bool>,
}

impl FakeWatchdog {
    fn count(&self, what: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == what)
            .count()
    }
}

struct FakeDevice(Arc<Mutex<Vec<&'static str>>>);

impl Watchdog for FakeDevice {
    fn ping(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().push("ping");
        Ok(())
    }

    fn magic_close(self: Box<Self>) -> io::Result<()> {
        self.0.lock().unwrap().push("magic_close");
        Ok(())
    }
}

impl WatchdogOpener for FakeWatchdog {
    fn open(&self) -> io::Result<Box<dyn Watchdog>> {
        if *self.fail_open.lock().unwrap() {
            return Err(io::Error::new(io::ErrorKind::ResourceBusy, "busy"));
        }
        self.log.lock().unwrap().push("open");
        Ok(Box::new(FakeDevice(self.log.clone())))
    }
}

/// 첫 받아 오기에서 멈춘다: 켜기 문턱을 지난 뒤 `hv.start` 전이다.
#[derive(Default)]
struct GateFetcher {
    calls: Mutex<usize>,
    reached: Notify,
    release: Notify,
}

#[async_trait]
impl FirmwareFetcher for GateFetcher {
    async fn fetch(&self, _vm: Uuid, _node: Uuid) -> Result<(), FirmwareError> {
        let first = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls == 1
        };
        if first {
            self.reached.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// `gate`가 켜져 있으면 다음 볼륨 켜기에서 멈춘다: 도는 VM에 다시 붙으며 디스크를 해석하는 중이다.
#[derive(Default)]
struct GateStorage {
    gate: Mutex<bool>,
    reached: Notify,
    release: Notify,
}

#[async_trait]
impl Storage for GateStorage {
    async fn activate_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        let gated = std::mem::take(&mut *self.gate.lock().unwrap());
        if gated {
            self.reached.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
    async fn create_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn import_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: &Path,
        _: SourceFormat,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn delete_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_info(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        unreachable!()
    }
    async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
        unreachable!()
    }
    fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
        unreachable!()
    }
    async fn snapshot_create(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        unreachable!()
    }
    async fn resize_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: u64,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        unreachable!()
    }
    async fn release_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn copy_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
}

struct Ha {
    renewer: Arc<FakeRenewer>,
    watchdog: Arc<FakeWatchdog>,
    guard: Arc<HaGuard>,
}

const FENCE_AFTER: Duration = Duration::from_millis(300);

/// 가짜 갱신과 가짜 watchdog로 guard를 만들어 reconciler에 넣는다. 리스는 300ms 뒤에 끝난다.
async fn start(ok: bool) -> (Env, Ha) {
    start_with(ok, FENCE_AFTER, |_| {}).await
}

async fn start_with(
    ok: bool,
    fence_after: Duration,
    tweak: impl FnOnce(&FakeWatchdog),
) -> (Env, Ha) {
    start_full(ok, fence_after, tweak, |_| {}).await
}

/// `config`로 reconciler 설정도 바꾼다.
async fn start_full(
    ok: bool,
    fence_after: Duration,
    tweak: impl FnOnce(&FakeWatchdog),
    config: impl FnOnce(&mut ReconcilerConfig),
) -> (Env, Ha) {
    let renewer = Arc::new(FakeRenewer(Mutex::new(ok)));
    let watchdog = Arc::new(FakeWatchdog::default());
    tweak(&watchdog);
    let guard = HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after,
            renew_interval: Duration::from_millis(20),
            watchdog: Some(watchdog.clone()),
        },
    );
    let g = guard.clone();
    let env = Env::start_with(move |c: &mut ReconcilerConfig| {
        c.ha = Some(g);
        config(c);
    })
    .await;
    (
        env,
        Ha {
            renewer,
            watchdog,
            guard,
        },
    )
}

fn ha_vm(name: &str, node: Uuid) -> Vm {
    let mut vm = vm(name, node);
    vm.spec.ha = true;
    vm
}

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

fn error_is(v: &Vm, msg: &str) -> bool {
    v.status.last_error.as_deref() == Some(msg)
}

/// 리스를 갱신하기 전에는 HA VM을 켜지 않고(실패로 세지 않는다) 기다린다. 갱신되면 watchdog를 연 뒤 켠다.
#[tokio::test]
async fn ha_vm_waits_for_the_lease_before_starting() {
    let (env, ha) = start(false).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    // 기다리는 동안 다시 보아도 켜지 않고 원하는 상태도 그대로다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(ha.watchdog.count("open"), 0);
    assert_eq!(env.get(vm.id).spec.power, PowerState::Running);

    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.hv.launches().len(), 1);
    assert_eq!(ha.watchdog.count("open"), 1);
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 리스가 끊기면 켜지 않고 기다린다. 시작하는 동안에는 도는 HA VM으로
/// 알려 두어, 리스가 없으면 watchdog를 쓰다듬지 않는다.
#[tokio::test]
async fn ha_vm_is_not_started_if_the_lease_ends_while_starting() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, ha) = start_full(true, FENCE_AFTER, |_| {}, |c| c.firmware = Some(f)).await;
    // 다른 노드의 UEFI 변수를 받아 온 뒤 켠다.
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    assert_eq!(ha.watchdog.count("open"), 1);

    ha.renewer.set_ok(false);
    ha.guard.subscribe().wait_for(|s| s.stale).await.unwrap();
    let pinged = ha.watchdog.count("ping");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(ha.watchdog.count("ping"), pinged, "pinged while starting");

    fetcher.release.notify_one();
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(got.spec.power, PowerState::Running);
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    // 켜지 않았으므로 도는 HA VM이 아니다: 다시 쓰다듬는다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while ha.watchdog.count("ping") == pinged {
        assert!(std::time::Instant::now() < deadline, "not pinging again");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.hv.launches().len(), 1);
}

/// HA가 아닌 VM은 리스와 상관없이 켠다.
#[tokio::test]
async fn non_ha_vm_starts_without_the_lease() {
    let (env, ha) = start(false).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(ha.watchdog.count("open"), 0);
}

/// watchdog를 열지 못하면 HA VM을 켜지 않고 오류를 남긴다.
#[tokio::test]
async fn ha_vm_does_not_start_when_the_watchdog_cannot_be_opened() {
    let (env, ha) = start_with(true, FENCE_AFTER, |w| *w.fail_open.lock().unwrap() = true).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "watchdog error", |v| {
            v.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.starts_with("cannot open the watchdog device"))
        })
        .await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert!(env.hv.launches().is_empty());

    *ha.watchdog.fail_open.lock().unwrap() = false;
    env.wait(vm.id, "running", running).await;
}

/// 리스를 잃으면 도는 HA VM을 강제로 끈다. 우리가 요청한 종료이므로 실패로 세지 않고(`restart = never`여도
/// 원하는 상태가 그대로다) 리스가 돌아오면 다시 켠다. HA가 아닌 VM은 그대로 둔다.
#[tokio::test]
async fn lost_lease_force_stops_ha_vms_and_restarts_them_when_it_returns() {
    let (env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    vm.spec.restart = RestartPolicy::Never;
    let plain = common::vm("plain", env.node);
    env.create(&vm).await;
    env.create(&plain).await;
    env.wait(vm.id, "running", running).await;
    let plain_pid = env.wait(plain.id, "running", running).await.status.pid;

    ha.renewer.set_ok(false);
    let got = env.wait(vm.id, "fenced", |v| error_is(v, FENCED)).await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(got.status.pid, None);
    assert_eq!(env.pid(vm.id).await, None);
    // 이벤트를 처리한 뒤에도 실패로 남지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = env.get(vm.id);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_ne!(got.status.phase, VmPhase::Failed);
    assert_eq!(env.hv.launches().len(), 2, "only web and plain launched");
    assert_eq!(env.get(plain.id).status.pid, plain_pid);
    assert_eq!(env.pid(plain.id).await, plain_pid);

    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running again", running).await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.hv.launches().len(), 3);
}

/// 펜싱이 꺼진 노드(guard 없음)에서는 HA VM을 켜지 않는다.
#[tokio::test]
async fn ha_vm_does_not_start_on_a_node_without_fencing() {
    let env = Env::start().await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "unavailable", |v| error_is(v, UNAVAILABLE))
        .await;
    assert_ne!(got.status.phase, VmPhase::Running);
    assert!(env.hv.launches().is_empty());
}

/// 펜싱이 꺼진 노드에서 이미 도는 VM을 HA로 바꿔도 끄지 않고 그 사실만 남긴다.
#[tokio::test]
async fn running_ha_vm_on_a_node_without_fencing_is_left_running() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let current = env.get(vm.id);
    vm.spec.ha = true;
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "unavailable", |v| error_is(v, UNAVAILABLE))
        .await;
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert_eq!(env.pid(vm.id).await, pid);
}

/// reconciler가 멈춰도 도는 HA VM은 도는 것으로 남는다: 정상 종료 때 watchdog를 그대로 둔다.
#[tokio::test]
async fn shutdown_keeps_running_ha_vms_armed() {
    let (mut env, ha) = start(true).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 0);
}

/// 멈춘 HA VM은 도는 것에서 빠진다: 정상 종료 때 watchdog를 멈춘다.
#[tokio::test]
async fn stopped_ha_vm_is_no_longer_running() {
    let (mut env, ha) = start(true).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 다른 노드로 옮긴 HA VM은 작업자가 물러나며 도는 것에서 뺀다.
#[tokio::test]
async fn ha_vm_moved_away_is_no_longer_running() {
    let (mut env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 이미 도는 VM이 HA가 되면 리스가 살아 있을 때 watchdog를 연다. 그 전에는 오류 없이 기다렸다가
/// 리스가 살아나면 연다.
#[tokio::test]
async fn running_ha_vm_arms_the_watchdog_once_the_lease_is_fresh() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (env, ha) = start_with(false, Duration::from_secs(60), |_| {}).await;
    let mut vm = common::vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let current = env.get(vm.id);
    vm.spec.ha = true;
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = env.get(vm.id);
    assert!(got.spec.ha);
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.last_error, None);
    assert_eq!(ha.watchdog.count("open"), 0);

    ha.renewer.set_ok(true);
    let deadline = std::time::Instant::now() + common::WAIT;
    while ha.watchdog.count("open") == 0 {
        assert!(std::time::Instant::now() < deadline, "watchdog not armed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(env.pid(vm.id).await, pid);
    assert_eq!(env.get(vm.id).status.last_error, None);
}

/// 다른 노드로 옮겨졌어도 QEMU가 아직 돌면 도는 HA VM으로 남긴다: 정상 종료 때 watchdog를 그대로 둔다.
#[tokio::test]
async fn ha_vm_moved_away_while_running_stays_running() {
    let (mut env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.pid(vm.id).await, pid);
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 0);
}

/// HA VM을 다른 노드로 옮긴 마이그레이션이 성공했다: 작업자는 보류 중에 물러나고(원본 QEMU가 아직
/// 있다) 원본은 그 뒤에 멈춘다. 작업자 없이 온 그 종료 이벤트로 도는 HA VM에서 빼므로 정상 종료 때
/// watchdog를 멈춘다.
#[tokio::test]
async fn ha_vm_migrated_away_is_dropped_when_its_source_stops() {
    let (mut env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    let handle = env.handle();
    handle.begin_migration(vm.id).await;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 마이그레이션 Task가 원본을 끄고 보류를 푼다.
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    handle.end_migration(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 옮겨 간 HA VM의 QEMU가 이벤트 없이 사라졌어도 주기 재동기화가 도는 HA VM에서 뺀다.
#[tokio::test]
async fn resync_drops_ha_vms_no_longer_on_this_node() {
    let (mut env, ha) = start_full(
        true,
        FENCE_AFTER,
        |_| {},
        |c| c.resync_interval = Duration::from_millis(200),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // QMP로 다룰 수 없는 VM의 강제 종료는 이벤트 없이 사라진다.
    env.hv.set_unresponsive(vm.id, true);
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    assert_eq!(env.pid(vm.id).await, None);
    tokio::time::sleep(Duration::from_millis(600)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 다른 노드로 배정을 바꾼다.
async fn reassign(env: &Env, id: Uuid) {
    let current = env.get(id);
    let mut spec = current.spec.clone();
    spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 다른 노드로 배정됐으면(리스는 살아 있다: 과반수 쪽의 복구 쓰기가
/// 이미 적용됐다) 켜지 않고 status도 덮어쓰지 않는다.
#[tokio::test]
async fn ha_vm_reassigned_while_starting_is_not_started() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, _ha) = start_full(
        true,
        Duration::from_secs(60),
        |_| {},
        |c| c.firmware = Some(f),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    reassign(&env, vm.id).await;
    fetcher.release.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(env.get(vm.id).status.node, None);
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 원하는 상태가 stopped로 바뀌었으면 켜지 않는다.
#[tokio::test]
async fn ha_vm_stopped_while_starting_is_not_started() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, _ha) = start_full(
        true,
        Duration::from_secs(60),
        |_| {},
        |c| c.firmware = Some(f),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    env.set_power(vm.id, PowerState::Stopped).await;
    fetcher.release.notify_one();
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}

/// 다룰 수 없는 HA VM에 다시 붙는 동안(디스크 해석) 다른 노드로 배정되고 QEMU가 사라졌으면 새로 띄우지
/// 않는다.
#[tokio::test]
async fn ha_vm_reassigned_while_reattaching_is_not_started() {
    let storage = Arc::new(GateStorage::default());
    let s: Arc<dyn Storage> = storage.clone();
    let (env, _ha) = start_full(true, Duration::from_secs(60), |_| {}, |c| c.storage = s).await;
    let pool: StoragePool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let mut vm = ha_vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches().len(), 1);

    // QMP로 다룰 수 없게 되면 작업자가 다시 붙는다(`start`).
    env.hv.set_unresponsive(vm.id, true);
    *storage.gate.lock().unwrap() = true;
    env.handle().end_migration(vm.id);
    storage.reached.notified().await;
    reassign(&env, vm.id).await;
    // 이벤트 없이 사라진다.
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    storage.release.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.hv.launches().len(), 1, "{:?}", env.hv.launches());
    assert_eq!(env.pid(vm.id).await, None);
}

/// 켜는 동안 리스가 끝나고 다른 노드로 옮겨졌으면, 켜진 QEMU를 강제로 끄고 도는 HA VM에서 뺀다.
#[tokio::test]
async fn ha_vm_moved_away_after_the_lease_ended_is_force_stopped() {
    let (env, ha) = start(true).await;
    env.hv.delay_starts(Duration::from_secs(1));
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    // 켜기 문턱을 지나 hv.start 안에 있다.
    tokio::time::sleep(Duration::from_millis(100)).await;
    ha.renewer.set_ok(false);
    ha.guard.subscribe().wait_for(|s| s.stale).await.unwrap();
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.launches().is_empty() || env.pid(vm.id).await.is_some() {
        assert!(std::time::Instant::now() < deadline, "still running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 끈 뒤에는 도는 HA VM이 없으므로 리스가 없어도 다시 쓰다듬는다.
    let pinged = ha.watchdog.count("ping");
    while ha.watchdog.count("ping") == pinged {
        assert!(std::time::Instant::now() < deadline, "not pinging again");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 이 노드로 확정된 HA VM의 마이그레이션은 리스가 살아 있어야 이어서 실행한다(`cont`).
#[tokio::test]
async fn received_ha_vm_is_continued_only_with_the_lease() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (env, ha) = start_with(false, Duration::from_secs(60), |_| {}).await;
    let source = Uuid::now_v7();
    let mut vm = ha_vm("web", source);
    env.create(&vm).await;
    let mut status = env.get(vm.id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node: env.node,
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    let spec = VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    env.hv
        .start_incoming(&spec, Uuid::now_v7(), "127.0.0.1", 60000..=60099)
        .await
        .unwrap();
    env.hv.incoming_wait(vm.id).await.unwrap();

    // 원본이 확정한다. 리스를 갱신하지 못했으므로(아직 stale은 아니다) 멈춘 채 기다린다.
    let current = env.get(vm.id);
    vm.spec.node = Some(env.node);
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Paused)
    );
    assert!(env.hv.is_incoming(vm.id));

    ha.renewer.set_ok(true);
    let got = env
        .wait(vm.id, "taken over", |v| {
            running(v) && v.status.migration.is_none() && v.status.last_error.is_none()
        })
        .await;
    assert!(got.status.pid.is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.is_incoming(vm.id) {
        assert!(std::time::Instant::now() < deadline, "still incoming");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(ha.watchdog.count("open"), 1);
}

/// opld가 HA VM의 마이그레이션을 보내다 확정 전에 죽고 다시 떴다. 다시 뜬 opld의 guard는 아직 갱신하지
/// 못했다: 남은 표시를 지우고 `postmigrate`로 멈춘 원본을 리스를 기다리며(Starting) 그대로 둔다.
/// VM과 원본의 pid, 새 guard의 갱신을 돌려준다.
async fn ha_source_left_by_a_restart(env: &mut Env) -> (Vm, Option<u32>, Arc<FakeRenewer>) {
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    env.stop_reconciler().await;
    let mut status = env.get(vm.id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node: Uuid::now_v7(),
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    let target = opl_driver::MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = opl_driver::MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    };
    env.hv.migrate(vm.id, &target, &opts).await.unwrap();
    let renewer = Arc::new(FakeRenewer(Mutex::new(false)));
    env.config.ha = Some(HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after: Duration::from_secs(60),
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    ));
    env.start_reconciler();

    let got = env
        .wait(vm.id, "waiting", |v| {
            error_is(v, WAITING) && v.status.migration.is_none()
        })
        .await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let postmigrate = RuntimeStatus::Other("postmigrate".into());
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(postmigrate)
    );
    (vm, pid, renewer)
}

/// 남은 마이그레이션의 HA 원본은 리스를 갱신한 뒤에 이어서 실행한다.
#[tokio::test]
async fn ha_source_left_by_a_restart_is_resumed_only_with_the_lease() {
    let (mut env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let (vm, pid, renewer) = ha_source_left_by_a_restart(&mut env).await;

    renewer.set_ok(true);
    let got = env
        .wait(vm.id, "resumed", |v| {
            running(v) && v.status.last_error.is_none()
        })
        .await;
    assert_eq!(got.status.pid, pid);
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert_eq!(env.hv.launches().len(), 1);
}

/// 리스를 기다리는 원본의 VM이 HA가 아니게 되어도 낡은 원본으로 보고 끄지 않고 이어서 실행한다.
#[tokio::test]
async fn waiting_ha_source_is_resumed_when_ha_is_turned_off() {
    let (mut env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let (vm, pid, _renewer) = ha_source_left_by_a_restart(&mut env).await;

    let current = env.get(vm.id);
    let mut spec = current.spec.clone();
    spec.ha = false;
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "resumed", |v| {
            running(v) && v.status.last_error.is_none()
        })
        .await;
    assert_eq!(got.status.pid, pid);
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert_eq!(env.hv.launches().len(), 1, "{:?}", env.hv.launches());
}
```

`crates/opl-reconciler/tests/no_quorum.rs`에서

```rust
//! Raft 과반수를 잃어도 실행 중인 VM은 그대로 돌고(설계 10절 불변식 4),
//! 못 쓴 status는 과반수가 돌아오면 기록된다.
```

을 다음으로 바꾼다:

```rust
//! Raft 과반수를 잃어도 실행 중인 VM은 그대로 돌고(설계 10절 불변식 4),
//! 못 쓴 status는 과반수가 돌아오면 기록된다. HA VM은 Starting을 기록해야 다시 켠다.
```

`crates/opl-reconciler/tests/no_quorum.rs`에서

```rust
use opl_driver::{Hypervisor, MockHypervisor};
use opl_reconciler::Reconciler;
use opl_types::vm::{RestartPolicy, VmPhase, VmSpec};
```

을 다음으로 바꾼다:

```rust
use opl_driver::{Hypervisor, MockHypervisor};
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{HaConfig, HaGuard, Reconciler};
use opl_types::vm::{RestartPolicy, VmPhase, VmSpec};
```

`crates/opl-reconciler/tests/no_quorum.rs`에서

```rust
    reconciler.shutdown().await;
}
```

을 다음으로 바꾼다:

```rust
    reconciler.shutdown().await;
}

/// 늘 갱신에 성공하는 리스(과반수와 상관없이 fresh다). Starting 기록 규칙만 본다.
struct AlwaysRenewed;

#[async_trait::async_trait]
impl LeaseRenewer for AlwaysRenewed {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), opl_cluster::ClusterError> {
        Ok(())
    }
}

/// HA VM은 Starting을 기록하지 못하면 다시 켜지 않는다(과반수 쪽에서 리더가 다른 노드에서 켤 수
/// 있다). 과반수가 돌아오면 켠다.
#[tokio::test]
async fn ha_vm_is_not_restarted_without_recording_starting() {
    let dir = TempDir::new().unwrap();
    let router = MemRouter::default();
    let write_timeout = Duration::from_millis(300);
    let (n1, n2) = (NodeId(Uuid::now_v7()), NodeId(Uuid::now_v7()));
    let follower = openraft::Config {
        election_timeout_min: 60_000,
        election_timeout_max: 120_000,
        ..raft_config()
    };
    let c1 = open_node(
        &router,
        n1,
        &dir.path().join("n1"),
        raft_config(),
        write_timeout,
    )
    .await;
    let c2 = open_node(
        &router,
        n2,
        &dir.path().join("n2"),
        follower.clone(),
        write_timeout,
    )
    .await;
    let members = BTreeMap::from([(n1, BasicNode::new("n1")), (n2, BasicNode::new("n2"))]);
    c1.initialize(members).await.unwrap();
    wait_leader(&c1).await;

    let guard = HaGuard::spawn(
        Arc::new(AlwaysRenewed),
        HaConfig {
            fence_after: Duration::from_secs(60),
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    );
    let mut config = common::config(n1.0);
    config.ha = Some(guard);
    let hv = Arc::new(MockHypervisor::default());
    let dyn_hv: Arc<dyn Hypervisor> = hv.clone();
    let reconciler = Reconciler::spawn(c1.clone(), dyn_hv, config);
    let mut vm = vm("web", n1.0);
    vm.spec.restart = RestartPolicy::OnFailure;
    vm.spec.ha = true;
    c1.create(&vm).await.unwrap();
    let before = wait_vm(&c1, vm.id, "running", |v| {
        v.status.phase == VmPhase::Running
    })
    .await
    .status
    .pid;

    router.unregister(n2);
    c2.shutdown().await.unwrap();
    drop(c2);
    hv.crash(vm.id);
    tokio::time::sleep(write_timeout * 4).await;
    assert_eq!(
        hv.inspect(vm.id).await.unwrap(),
        None,
        "restarted without quorum"
    );
    assert_eq!(hv.launches().len(), 1);

    let _c2 = open_node(&router, n2, &dir.path().join("n2"), follower, write_timeout).await;
    let got = wait_vm(&c1, vm.id, "running again", |v| {
        v.status.phase == VmPhase::Running && v.status.pid != before
    })
    .await;
    assert_eq!(
        hv.inspect(vm.id).await.unwrap().map(|s| s.pid),
        got.status.pid
    );
    assert_eq!(hv.launches().len(), 2);
    reconciler.shutdown().await;
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 817개, 실패 0

Run: `cargo test -p opl-reconciler --test ha --test no_quorum`
Expected: PASS

Run: `cargo test -p opl-api --test two_nodes --test vms`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(reconciler): 워커가 HA 리스를 따른다 — 켜기 문턱, 리스를 잃으면 강제 종료"
```

---

### Task 5: HA 표시: oplctl vm set --ha, FENCING 열, 웹 HA 스위치와 배지

`GET /cluster` 멤버에 `fencing`. oplctl `vm create --ha`, `vm set VM --ha | --no-ha`(If-Match로 spec 다시 쓰기 — 예약 스냅샷 갱신과 같은 함수), `vm list`의 HA 열, `node list`·`cluster status`의 FENCING 열(`watchdog`·`process`·`-`). 웹: VM 폼의 HA 스위치(고른 노드가 HA 가능하지 않으면 이유와 함께 막고, 노드를 고르지 않았으면 HA 가능 노드가 하나라도 있어야), VM 목록·상세의 HA 배지, 노드·대시보드·클러스터 표의 펜싱 열, `errors.ha_unsupported`. en·ko.

**Files:**
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `web/src/cluster/ClusterPage.test.tsx`
- Modify: `web/src/cluster/ClusterPage.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/nodes/NodesPage.test.tsx`
- Modify: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/pages/DashboardPage.test.tsx`
- Modify: `web/src/pages/DashboardPage.tsx`
- Modify: `web/src/test/fixtures.ts`
- Modify: `web/src/vms/VmDetailPage.test.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmFormPage.test.tsx`
- Modify: `web/src/vms/VmListPage.test.tsx`
- Modify: `web/src/vms/VmListPage.tsx`
- Modify: `web/src/vms/form.test.ts`
- Modify: `web/src/vms/form.ts`
- Modify: `web/src/vms/form/GeneralSection.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs`에서

```rust
use opl_types::cluster::{ClusterInfo, ClusterSpec, JoinToken, JoinTokenSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec};
```

을 다음으로 바꾼다:

```rust
use opl_types::cluster::{ClusterInfo, ClusterSpec, JoinToken, JoinTokenSpec};
use opl_types::node::{Fencing, NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec};
```

`crates/opl-api/src/cluster.rs`에서

```rust
    /// 위트니스 노드인지(VM을 돌리지 않는 투표 멤버)
    pub witness: bool,
}
```

을 다음으로 바꾼다:

```rust
    /// 위트니스 노드인지(VM을 돌리지 않는 투표 멤버)
    pub witness: bool,
    /// 노드의 HA 펜싱. 펜싱이 꺼졌거나 Node가 아직 없으면 null이다.
    pub fencing: Option<Fencing>,
}
```

`crates/opl-api/src/cluster.rs`에서

```rust
            witness: node.is_some_and(|n| n.spec.witness),
        }
```

을 다음으로 바꾼다:

```rust
            witness: node.is_some_and(|n| n.spec.witness),
            fencing: node.and_then(|n| n.status.ha).map(|ha| ha.fencing),
        }
```

`crates/opl-api/tests/cluster.rs`에서

```rust
use opl_types::cluster::JoinTokenSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::cluster::JoinTokenSpec;
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use reqwest::StatusCode;
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    assert_eq!(view["members"][0]["online"], true);
```

을 다음으로 바꾼다:

```rust
    assert_eq!(view["members"][0]["online"], true);
    // 펜싱이 꺼진 노드는 null이다.
    assert_eq!(view["members"][0]["fencing"], json!(null), "{view}");
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.ha = Some(NodeHa {
        fencing: Fencing::Watchdog,
        recover_after_secs: 65,
    });
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();
    let (_, view) = viewer.get("/cluster").await;
    assert_eq!(view["members"][0]["fencing"], "watchdog", "{view}");
```

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, MIGRATE_WAIT, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin")]
    user: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get { node: String },
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀, 공유 LVM 풀을 만든다.
    Create(Box<PoolCreate>),
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
struct PoolSet {
    pool: String,
    #[command(flatten)]
    sharing: Sharing,
}

/// `pool set`의 공유 여부. 둘 중 하나를 꼭 준다.
#[derive(Args)]
#[group(required = true, multiple = false)]
struct Sharing {
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long)]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`)
fn pool_set_body(sharing: &Sharing) -> Value {
    json!({ "shared": sharing.shared })
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd", "lvm_shared"])))]
struct PoolCreate {
    name: String,
    /// 풀 디렉터리(절대 경로)
    #[arg(long, conflicts_with = "nfs")]
    path: Option<PathBuf>,
    /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
    #[arg(long, value_parser = parse_nfs)]
    nfs: Option<NfsSource>,
    /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
    #[arg(long, requires = "nfs", conflicts_with = "path")]
    nfs_options: Option<String>,
    /// 관리자가 준비한 VG와 thin pool `VG/THINPOOL`. 노드 로컬 풀이고 볼륨은 raw만 담는다.
    #[arg(long, value_parser = parse_lvm_thin, conflicts_with_all = ["path", "nfs", "nfs_options", "shared"])]
    lvm_thin: Option<LvmThinSource>,
    /// 관리자가 준비한 ZFS 데이터셋 `POOL/DATASET`. 노드 로컬 풀이고 볼륨은 raw zvol만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "shared"])]
    zfs: Option<String>,
    /// 관리자가 준비한 Ceph 풀과 RBD 네임스페이스 `POOL[/NAMESPACE]`. 늘 공유 풀이고 볼륨은 raw
    /// 이미지만 담는다.
    #[arg(long, value_parser = parse_rbd, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "shared"])]
    rbd: Option<RbdSource>,
    /// RBD 풀에 쓸 Ceph 클라이언트 id(`client.` 없이). 생략하면 admin.
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_user: Option<String>,
    /// RBD 풀에 쓸 ceph.conf 절대 경로. 생략하면 Ceph 기본(/etc/ceph/ceph.conf).
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_conf: Option<String>,
    /// 관리자가 준비한 공유 VG(lvmlockd). 늘 공유 풀이고 볼륨은 raw 두꺼운 LV만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "rbd", "rbd_user", "rbd_conf", "shared"])]
    lvm_shared: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃의 포털 `HOST[:PORT]`(IPv6는 `[ADDR][:PORT]`). 노드가 로그인한다.
    #[arg(long, requires_all = ["lvm_shared", "iscsi_target"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_portal: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃 이름(IQN)
    #[arg(long, requires_all = ["lvm_shared", "iscsi_portal"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_target: Option<String>,
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD·공유 LVM 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
}

/// `pool create`의 요청 본문. NFS·RBD·공유 LVM 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
fn pool_create_body(args: PoolCreate) -> Value {
    let PoolCreate {
        name,
        path,
        nfs,
        nfs_options,
        lvm_thin,
        zfs,
        rbd,
        rbd_user,
        rbd_conf,
        lvm_shared,
        iscsi_portal,
        iscsi_target,
        shared,
        nodes,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd, lvm_shared) {
        (Some(n), _, _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _, _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r), _) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None, Some(vg)) => {
            // 포털과 타깃은 clap이 함께 받게 한다.
            let iscsi = iscsi_portal
                .zip(iscsi_target)
                .map(|(portal, target)| json!({ "portal": portal, "target": target }));
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": iscsi })
        }
        (None, None, None, None, None) => json!({ "kind": "dir", "path": path }),
    };
    let mut spec = json!({ "backend": backend, "nodes": nodes });
    if shared {
        spec["shared"] = json!(true);
    }
    json!({ "name": name, "spec": spec })
}

/// `--nfs`로 받은 NFS 서버와 내보내기 경로
#[derive(Debug, Clone, PartialEq, Eq)]
struct NfsSource {
    server: String,
    export: String,
}

/// `nas:/srv/vm`, `[fd00::1]:/srv/vm`처럼 쓴다. 서버 이름의 나머지 검사는 API가 한다.
fn parse_nfs(s: &str) -> Result<NfsSource, String> {
    let split = match s.strip_prefix('[') {
        Some(rest) => rest.split_once("]:"),
        None => s.split_once(':'),
    };
    match split {
        Some((server, export)) if !server.is_empty() && export.starts_with('/') => Ok(NfsSource {
            server: server.into(),
            export: export.into(),
        }),
        _ => Err(format!(
            "invalid nfs source {s:?}: use SERVER:/EXPORT ([ADDR]:/EXPORT for IPv6)"
        )),
    }
}

/// `--lvm-thin`으로 받은 VG와 thin pool 이름
#[derive(Debug, Clone, PartialEq, Eq)]
struct LvmThinSource {
    vg: String,
    thinpool: String,
}

/// `vg0/tp`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_lvm_thin(s: &str) -> Result<LvmThinSource, String> {
    match s.split_once('/') {
        Some((vg, thinpool))
            if !vg.is_empty() && !thinpool.is_empty() && !thinpool.contains('/') =>
        {
            Ok(LvmThinSource {
                vg: vg.into(),
                thinpool: thinpool.into(),
            })
        }
        _ => Err(format!("invalid lvm-thin pool {s:?}: use VG/THINPOOL")),
    }
}

/// `--rbd`로 받은 Ceph 풀과 RBD 네임스페이스
#[derive(Debug, Clone, PartialEq, Eq)]
struct RbdSource {
    pool: String,
    namespace: Option<String>,
}

/// `rbd`, `rbd/opl`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_rbd(s: &str) -> Result<RbdSource, String> {
    let (pool, namespace) = match s.split_once('/') {
        Some((pool, namespace)) => (pool, Some(namespace)),
        None => (s, None),
    };
    let bad = |n: &str| n.is_empty() || n.contains('/');
    if bad(pool) || namespace.is_some_and(bad) {
        return Err(format!(
            "invalid rbd pool {s:?}: use POOL or POOL/NAMESPACE"
        ));
    }
    Ok(RbdSource {
        pool: pool.into(),
        namespace: namespace.map(Into::into),
    })
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크를 만든다.
    Create {
        name: String,
        /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
        #[arg(long)]
        bridge: String,
        /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
        #[arg(long)]
        vlan: Option<u16>,
    },
    Delete {
        network: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
    },
    Delete {
        volume: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    /// VM 설정을 바꾼다. 실행 중이어도 된다.
    Set(VmSet),
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(디스크가 모두 공유 풀에 있어야 한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
    },
    Delete {
        vm: String,
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다.
    #[arg(long = "nic")]
    nics: Vec<String>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. 펜싱이 켜진 노드에서만 돈다.
    #[arg(long)]
    ha: bool,
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).args(["ha", "no_ha"])))]
struct VmSet {
    vm: String,
    /// HA VM으로 만든다. VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
    ha: bool,
    /// HA를 끈다.
    #[arg(long)]
    no_ha: bool,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 볼륨 형식. 주지 않았으면 풀을 읽어 그 종류의 기본값으로 정한다.
async fn volume_format(
    client: &Client,
    pool: &str,
    format: Option<Format>,
) -> anyhow::Result<Format> {
    match format {
        Some(f) => Ok(f),
        None => {
            let read = client.get(&format!("/storage-pools/{pool}")).await;
            let pool = read.with_context(|| {
                format!(
                    "cannot read storage pool {pool:?} to choose a volume format; pass --format"
                )
            })?;
            Ok(pool_default_format(&pool))
        }
    }
}

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw(스냅샷은 블록 스냅샷,
/// 공유 LVM은 스냅샷 없음), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" || kind == "lvm-shared" {
        Format::Raw
    } else {
        Format::Qcow2
    }
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let token = login(&server, args, ca_pem.clone()).await?;
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        if let Some((old, old_server)) = previous
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(token), ca_pem)?;
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        writeln!(out, "logged in to {server} as {}", args.user)?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            if let Some(id) = config.token.as_deref().and_then(token_id) {
                // 이미 지워진 토큰이어도 로컬에서는 지운다.
                let _ = client.delete(&format!("/tokens/{id}")).await;
            }
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                let nodes = if json_out {
                    Vec::new()
                } else {
                    items(&client.get("/nodes").await?)
                };
                print_list(out, json_out, &list, POOL_HEADERS, |p| pool_row(p, &nodes))?;
            }
            PoolCmd::Get { pool } => {
                let pool = client.get(&format!("/storage-pools/{pool}")).await?;
                if json_out {
                    print_json(out, &pool)?;
                } else {
                    let nodes = items(&client.get("/nodes").await?);
                    write!(out, "{}", describe_pool(&pool))?;
                    table(out, POOL_NODE_HEADERS, &pool_node_rows(&pool, &nodes))?;
                }
            }
            PoolCmd::Create(args) => {
                let name = args.name.clone();
                client
                    .post("/storage-pools", &pool_create_body(*args))
                    .await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Set(PoolSet { pool, sharing }) => {
                client
                    .patch(&format!("/storage-pools/{pool}"), &pool_set_body(&sharing))
                    .await?;
                let state = if sharing.shared {
                    "shared"
                } else {
                    "node-local"
                };
                writeln!(out, "storage pool {pool}: {state}")?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, &["NAME", "BRIDGE", "VLAN"], |n| {
                    vec![
                        cell(&n["name"]),
                        cell(&n["spec"]["bridge"]),
                        cell(&n["spec"]["vlan"]),
                    ]
                })?;
            }
            NetworkCmd::Get { network } => {
                print_json(out, &client.get(&format!("/networks/{network}")).await?)?
            }
            NetworkCmd::Create { name, bridge, vlan } => {
                let body = json!({ "name": name, "spec": { "bridge": bridge, "vlan": vlan } });
                client.post("/networks", &body).await?;
                writeln!(out, "network {name} created")?;
            }
            NetworkCmd::Delete { network } => {
                client.delete(&format!("/networks/{network}")).await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => {
                print_json(out, &client.get(&format!("/volumes/{volume}")).await?)?
            }
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{volume}/resize"), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{volume}")).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    |t| {
                        vec![
                            cell(&t["id"]),
                            cell(&t["spec"]["op"]["kind"]),
                            cell(&t["spec"]["target"]),
                            cell(&t["status"]["state"]),
                            cell(&t["status"]["error"]),
                        ]
                    },
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{task}")).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{name}"), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{name}")).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{token}")).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
    }
    Ok(())
}

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            print_cluster_status(out, &view)?;
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{node}")).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"],
                vm_row,
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{vm}")).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args
                .nics
                .iter()
                .map(|network| json!({ "network": network, "model": "virtio" }))
                .collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                    "ha": args.ha,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Set(args) => {
            put_spec_field(client, &args.vm, "ha", json!(args.ha)).await?;
            let state = if args.ha { "on" } else { "off" };
            writeln!(out, "vm {}: HA {state}", args.vm)?;
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Migrate { vm, node } => {
            let task = client
                .post(&format!("/vms/{vm}/migrate"), &json!({ "node": node }))
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{vm}")).await?;
                let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{vm}")).await?;
                    let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{vm}/snapshots/{}", cell(&s["id"]));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client.post(&format!("/vms/{vm}/clone"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{vm}")).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        // 옛 서버는 보내지 않는다 — HA VM이 아니다.
        (v["spec"]["ha"] == true).to_string(),
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client.post(&format!("/vms/{vm}/snapshots"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current = client.get(&format!("/vms/{vm}")).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{vm}/snapshots/{name}/revert");
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{vm}/snapshots/{name}"))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", args.vm);
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다).
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    put_spec_field(
        client,
        vm,
        "snapshot_schedule",
        schedule.unwrap_or(Value::Null),
    )
    .await
}

/// VM `vm`의 spec 필드 `field`를 `value`로 바꾼다. VM을 읽어 그 revision으로(`If-Match`)
/// spec 전체를 다시 쓴다.
async fn put_spec_field(
    client: &Client,
    vm: &str,
    field: &str,
    value: Value,
) -> anyhow::Result<()> {
    let path = format!("/vms/{vm}");
    let mut retries = SpecRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&spec_update(&current, field, value.clone()));
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// VM 수정 PUT을 다시 할 횟수
#[derive(Default)]
struct SpecRetries {
    mismatch: u32,
    busy: u32,
}

impl SpecRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 spec 필드 `field`만 `value`로 바꾼다.
fn spec_update(vm: &Value, field: &str, value: Value) -> Value {
    let mut spec = vm["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(Method::POST, &format!("/vms/{vm}/actions/{action}")))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
    "FENCING",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
        // 펜싱이 꺼진 노드(옛 서버도)는 `ha`가 없다.
        cell(&n["status"]["ha"]["fencing"]),
    ]
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
        .iter()
        .map(|m| {
            vec![
                cell(&m["name"]),
                cell(&m["id"]),
                cell(&m["address"]),
                if m["voter"] == true {
                    "voter"
                } else {
                    "learner"
                }
                .into(),
                cell(&m["online"]),
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
                cell(&m["fencing"]),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS", "FENCING",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
    Ok(())
}

/// 목록 응답의 항목들
fn items(list: &Value) -> Vec<Value> {
    list["items"].as_array().cloned().unwrap_or_default()
}

/// `pool list`의 열
const POOL_HEADERS: &[&str] = &["NAME", "KIND", "LOCATION", "NODES", "READY"];

/// `pool list`의 한 행. READY는 이 풀을 쓸 수 있는 노드(없는 노드 포함) 가운데 온라인이고 준비된
/// 노드 수다.
fn pool_row(pool: &Value, nodes: &[Value]) -> Vec<String> {
    let listed = pool["spec"]["nodes"].as_array().map_or(0, Vec::len);
    let usable = pool_nodes(pool, nodes);
    let ready = usable
        .iter()
        .filter(|n| n.is_ok_and(|n| node_online(n) && pool_state(pool, n)["ready"] == true))
        .count();
    vec![
        cell(&pool["name"]),
        cell(&pool["spec"]["backend"]["kind"]),
        pool_location(pool),
        if listed == 0 {
            "all".into()
        } else {
            listed.to_string()
        },
        format!("{ready}/{}", usable.len()),
    ]
}

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`, 공유 LVM 풀은 `vg`(iSCSI면
/// `vg @ portal target`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    if b["kind"] == "lvm-shared" {
        let iscsi = &b["iscsi"];
        if iscsi.is_object() {
            return format!(
                "{} @ {} {}",
                cell(&b["vg"]),
                cell(&iscsi["portal"]),
                cell(&iscsi["target"])
            );
        }
        return cell(&b["vg"]);
    }
    if b["kind"] == "rbd" {
        return match b["namespace"].as_str() {
            Some(namespace) => format!("{}/{namespace}", cell(&b["pool"])),
            None => cell(&b["pool"]),
        };
    }
    if let Some(vg) = b["vg"].as_str() {
        return format!("{vg}/{}", cell(&b["thinpool"]));
    }
    if let Some(dataset) = b["dataset"].as_str() {
        return dataset.into();
    }
    match b["server"].as_str() {
        Some(server) if server.contains(':') => format!("[{server}]:{}", cell(&b["export"])),
        Some(server) => format!("{server}:{}", cell(&b["export"])),
        None => cell(&b["path"]),
    }
}

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
    }
}

fn node_online(node: &Value) -> bool {
    node["status"]["online"] == true
}

/// 노드가 알린 풀 상태. 알리지 않았거나 같은 이름의 다른 풀(`pool_id`가 다름)의 상태면 `null`이다.
fn pool_state<'a>(pool: &Value, node: &'a Value) -> &'a Value {
    let name = pool["name"].as_str().unwrap_or_default();
    let state = &node["status"]["pools"][name];
    if state["pool_id"] == pool["id"] {
        state
    } else {
        &Value::Null
    }
}

/// `pool get`의 머리 줄들
fn describe_pool(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    let mut text = format!(
        "pool: {}\nkind: {}\nlocation: {}\n",
        cell(&pool["name"]),
        cell(&b["kind"]),
        pool_location(pool)
    );
    if let Some(options) = b["options"].as_str() {
        text.push_str(&format!("options: {options}\n"));
    }
    // RBD 풀의 Ceph 사용자·conf(생략하면 Ceph 기본이라 적지 않는다)
    for key in ["user", "conf"] {
        if let Some(value) = b[key].as_str() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!("shared: {}\n", cell(&pool["spec"]["shared"])));
    text
}

/// `pool get`의 노드별 상태 열
const POOL_NODE_HEADERS: &[&str] = &["NODE", "READY", "SIZE", "AVAILABLE", "ERROR"];

/// 풀을 쓸 수 있는 노드마다 상태·용량·오류. 용량은 온라인이고 준비된 노드만 보인다.
fn pool_node_rows(pool: &Value, nodes: &[Value]) -> Vec<Vec<String>> {
    pool_nodes(pool, nodes)
        .into_iter()
        .map(|n| {
            let n = match n {
                Ok(n) => n,
                Err(id) => {
                    return vec![
                        cell(id),
                        "false".into(),
                        "-".into(),
                        "-".into(),
                        "node not found".into(),
                    ];
                }
            };
            let state = pool_state(pool, n);
            let online = node_online(n);
            let ready = online && state["ready"] == true;
            let size = |v: &Value| if ready { bytes(v) } else { "-".into() };
            let error = if !online {
                "node offline".into()
            } else if state.is_null() {
                "no status reported yet".into()
            } else {
                cell(&state["error"])
            };
            vec![
                cell(&n["name"]),
                ready.to_string(),
                size(&state["total_bytes"]),
                size(&state["available_bytes"]),
                error,
            ]
        })
        .collect()
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다.
async fn login(server: &str, args: &LoginArgs, ca_pem: Option<String>) -> anyhow::Result<String> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None, ca_pem)?;
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&json!({ "username": args.user, "password": password }))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let name = format!("oplctl-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", schedule.clone()),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", Value::Null),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
        // `vm set --ha`는 ha만 바꾼다.
        assert_eq!(
            spec_update(&vm, "ha", json!(true))["spec"],
            json!({ "memory_mib": 256, "snapshot_schedule": vm["spec"]["snapshot_schedule"], "ha": true })
        );
    }

    #[test]
    fn parses_vm_ha_commands() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web", "--ha"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if !c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Set(VmSet { vm, ha: true, no_ha: false }) } if vm == "web"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--no-ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Set(VmSet {
                    ha: false,
                    no_ha: true,
                    ..
                })
            }
        ));
        // 바꿀 것이 하나는 있어야 하고, 둘을 함께 줄 수 없다.
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha", "--no-ha"]).is_err());
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node } } if vm == "web" && node == "n2"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            [
                "web",
                "running",
                "migrating",
                "id-a -> id-b",
                "false",
                "42",
                "-"
            ]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "false", "42", "-"]
        );
        // HA VM
        vm["spec"]["ha"] = json!(true);
        assert_eq!(vm_row(&vm)[4], "true");
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn spec_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = SpecRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = SpecRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool, pool_set_body(&s.sharing)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }))
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            ("nas".into(), json!({ "shared": false }))
        );
        // 둘 중 하나만, 꼭 하나
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
    }

    #[test]
    fn parses_pool_create_commands() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.path, c.nfs, c.nfs_options, c.shared),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None, false)
        );
        // 디렉터리 풀은 공유 스토리지(밖에서 마운트한 공유 디렉터리)라고 적을 수 있다.
        assert!(create(&["--path", "/mnt/shared", "--shared"]).unwrap().3);
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into()),
                false
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로, NFS 풀은 늘 공유라 --shared가 없다
        for args in [
            &[][..],
            &["--nfs", "nas:/srv/vm", "--shared"],
            &["--shared"],
            &["--path", "/srv/pool", "--nfs", "nas:/srv/vm"],
            &["--path", "/srv/pool", "--nfs-options", "hard"],
            &["--nfs", "nas"],
            &["--nfs", "nas:srv"],
            &["--nfs", ":/srv"],
            &["--nfs", "fd00::1:/srv"],
            &["--nfs", "[fd00::1/srv"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_lvm_thin_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.lvm_thin,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-thin", "vg0/tp"]).unwrap(),
            Some(LvmThinSource {
                vg: "vg0".into(),
                thinpool: "tp".into()
            })
        );
        // 다른 종류·--shared와 함께 못 쓰고, VG와 thin pool이 모두 있어야 한다
        for args in [
            &["--lvm-thin", "vg0/tp", "--path", "/srv/pool"][..],
            &["--lvm-thin", "vg0/tp", "--nfs", "nas:/srv/vm"],
            &["--lvm-thin", "vg0/tp", "--shared"],
            &["--lvm-thin", "vg0/tp", "--nfs-options", "hard"],
            &["--lvm-thin", "vg0"],
            &["--lvm-thin", "/tp"],
            &["--lvm-thin", "vg0/"],
            &["--lvm-thin", "vg0/tp/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd, --lvm-shared) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in [
            "--path",
            "--nfs",
            "--lvm-thin",
            "--zfs",
            "--rbd",
            "--lvm-shared",
        ] {
            assert!(msg.contains(kind), "{msg}");
        }
        assert!(create(&["--node", "a"]).is_err());
        assert!(create(&["--path", "/srv/p"]).is_ok());
        assert!(create(&["--nfs", "nas:/srv/vm"]).is_ok());
    }

    #[test]
    fn parses_zfs_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.zfs,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--zfs", "tank/opl"]).unwrap(),
            Some("tank/opl".to_string())
        );
        assert_eq!(
            create(&["--zfs", "tank"]).unwrap(),
            Some("tank".to_string())
        );
        // 다른 종류·--shared와 함께 못 쓴다. 데이터셋 이름의 검사는 API가 한다.
        for args in [
            &["--zfs", "tank/opl", "--path", "/srv/pool"][..],
            &["--zfs", "tank/opl", "--nfs", "nas:/srv/vm"],
            &["--zfs", "tank/opl", "--nfs-options", "hard"],
            &["--zfs", "tank/opl", "--lvm-thin", "vg0/tp"],
            &["--zfs", "tank/opl", "--shared"],
            &["--zfs"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_rbd_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.rbd, c.rbd_user, c.rbd_conf),
                _ => unreachable!(),
            })
        };
        let rbd = |pool: &str, namespace: Option<&str>| RbdSource {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
        };
        assert_eq!(
            create(&["--rbd", "rbd"]).unwrap(),
            (Some(rbd("rbd", None)), None, None)
        );
        assert_eq!(
            create(&[
                "--rbd",
                "rbd/opl",
                "--rbd-user",
                "opl",
                "--rbd-conf",
                "/etc/ceph/opl.conf"
            ])
            .unwrap(),
            (
                Some(rbd("rbd", Some("opl"))),
                Some("opl".into()),
                Some("/etc/ceph/opl.conf".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 사용자·conf는 --rbd와 함께만 쓴다.
        // 이름 규칙의 나머지 검사는 API가 한다.
        for args in [
            &["--rbd", "rbd", "--path", "/srv/pool"][..],
            &["--rbd", "rbd", "--nfs", "nas:/srv/vm"],
            &["--rbd", "rbd", "--nfs-options", "hard"],
            &["--rbd", "rbd", "--lvm-thin", "vg0/tp"],
            &["--rbd", "rbd", "--zfs", "tank/opl"],
            &["--rbd", "rbd", "--shared"],
            &["--path", "/srv/pool", "--rbd-user", "opl"],
            &["--path", "/srv/pool", "--rbd-conf", "/etc/ceph/ceph.conf"],
            &["--rbd"],
            &["--rbd", ""],
            &["--rbd", "/opl"],
            &["--rbd", "rbd/"],
            &["--rbd", "rbd/opl/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// `pool create`가 보내는 요청 본문. RBD 풀은 네임스페이스·사용자·conf를 담고 `shared`는 적지
    /// 않는다.
    #[test]
    fn pool_create_sends_the_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        let args = [
            "--rbd",
            "rbd/opl",
            "--rbd-user",
            "opl",
            "--rbd-conf",
            "/etc/ceph/x.conf",
        ];
        assert_eq!(
            body(&args),
            json!({
                "name": "ceph",
                "spec": {
                    "backend": {
                        "kind": "rbd", "pool": "rbd", "namespace": "opl",
                        "user": "opl", "conf": "/etc/ceph/x.conf"
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--rbd", "rbd", "--node", "n1"])["spec"],
            json!({
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": null, "user": null, "conf": null
                },
                "nodes": ["n1"]
            })
        );
    }

    #[test]
    fn parses_lvm_shared_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.lvm_shared, c.iscsi_portal, c.iscsi_target),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-shared", "san"]).unwrap(),
            (Some("san".into()), None, None)
        );
        assert_eq!(
            create(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "10.0.0.5:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ])
            .unwrap(),
            (
                Some("san".into()),
                Some("10.0.0.5:3260".into()),
                Some("iqn.2003-01.org.example:san".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 포털·타깃은 --lvm-shared와 함께, 둘이 함께
        // 쓴다. 이름 규칙의 검사는 API가 한다.
        for args in [
            &["--lvm-shared", "san", "--path", "/srv/pool"][..],
            &["--lvm-shared", "san", "--nfs", "nas:/srv/vm"],
            &["--lvm-shared", "san", "--nfs-options", "hard"],
            &["--lvm-shared", "san", "--lvm-thin", "vg0/tp"],
            &["--lvm-shared", "san", "--zfs", "tank/opl"],
            &["--lvm-shared", "san", "--rbd", "rbd"],
            &["--lvm-shared", "san", "--rbd-user", "opl"],
            &["--lvm-shared", "san", "--shared"],
            &["--lvm-shared", "san", "--iscsi-portal", "nas"],
            &[
                "--lvm-shared",
                "san",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &[
                "--path",
                "/srv/pool",
                "--iscsi-portal",
                "nas",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &["--lvm-shared"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 공유 LVM 풀은 VG와 iSCSI 타깃(없으면 null)을 담고 `shared`는 적지 않는다.
    #[test]
    fn pool_create_sends_the_lvm_shared_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            body(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "[fd00::5]:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ]),
            json!({
                "name": "san",
                "spec": {
                    "backend": {
                        "kind": "lvm-shared", "vg": "san",
                        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.example:san" }
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--lvm-shared", "san", "--node", "n1"])["spec"],
            json!({
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": ["n1"]
            })
        );
    }

    /// 풀을 읽지 못하면 --format을 주라고 알린다. 주면 풀을 읽지 않는다.
    #[tokio::test]
    async fn volume_format_explains_an_unreadable_pool() {
        // 아무도 듣지 않는 포트
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        let err = volume_format(&client, "fast", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.starts_with(
                "cannot read storage pool \"fast\" to choose a volume format; pass --format: "
            ),
            "{msg}"
        );
        let format = volume_format(&client, "fast", Some(Format::Raw)).await;
        assert_eq!(format.unwrap(), Format::Raw);
    }

    #[test]
    fn volume_format_defaults_to_pool_kind() {
        let pool = |kind: &str| json!({ "spec": { "backend": { "kind": kind } } });
        assert_eq!(pool_default_format(&pool("lvm-thin")), Format::Raw);
        assert_eq!(pool_default_format(&pool("zfs")), Format::Raw);
        assert_eq!(pool_default_format(&pool("rbd")), Format::Raw);
        assert_eq!(pool_default_format(&pool("lvm-shared")), Format::Raw);
        assert_eq!(pool_default_format(&pool("dir")), Format::Qcow2);
        assert_eq!(pool_default_format(&pool("nfs")), Format::Qcow2);
        // --format를 주지 않으면 풀을 읽어 정하고, 주면 그대로 쓴다.
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "create", "root", "--pool", "fast", "--size", "1G",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Create { format: None, .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "import", "root", "--pool", "fast", "--file", "a.vmdk", "--format",
            "raw",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Import {
                    format: Some(Format::Raw),
                    ..
                }
            }
        ));
    }

    #[test]
    fn pool_list_and_details_show_each_node() {
        let node = |name: &str, online: bool, pools: Value| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "status": { "online": online, "pools": pools },
            })
        };
        let gib = 1u64 << 30;
        let state = |ready: bool, error: Value, total: u64, available: u64| json!({ "pool_id": "id-nas", "ready": ready, "error": error, "total_bytes": total, "available_bytes": available });
        let nodes = [
            node(
                "a",
                true,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            node(
                "b",
                true,
                json!({ "nas": state(false, json!("mount failed"), 0, 0) }),
            ),
            node("c", true, json!({})),
            // 준비됐다고 알렸지만 꺼진 노드는 준비로 세지 않는다.
            node(
                "d",
                false,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
            node(
                "e",
                true,
                json!({ "nas": { "pool_id": "id-old", "ready": true, "error": null } }),
            ),
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
        ];
        let nas = json!({
            "id": "id-nas",
            "name": "nas",
            "spec": {
                "backend": { "kind": "nfs", "server": "fd00::1", "export": "/srv/vm", "options": "hard" },
                "nodes": [],
                "shared": true,
            },
        });
        // 노드 목록의 노드를 그 순서대로 본다. 없는 노드도 센다.
        let local = json!({
            "id": "id-local",
            "name": "local",
            "spec": {
                "backend": { "kind": "dir", "path": "/srv/pool" },
                "nodes": ["id-gone", "id-b"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&nas, &nodes),
            ["nas", "nfs", "[fd00::1]:/srv/vm", "all", "1/5"]
        );
        assert_eq!(
            pool_row(&local, &nodes),
            ["local", "dir", "/srv/pool", "2", "0/2"]
        );

        let mut out = describe_pool(&nas).into_bytes();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&nas, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "pool: nas\n\
             kind: nfs\n\
             location: [fd00::1]:/srv/vm\n\
             options: hard\n\
             shared: true\n\
             NODE  READY  SIZE      AVAILABLE  ERROR\n\
             a     true   100.0GiB  40.0GiB    -\n\
             b     false  -         -          mount failed\n\
             c     false  -         -          no status reported yet\n\
             d     false  -         -          node offline\n\
             e     false  -         -          no status reported yet\n"
        );
        assert_eq!(
            describe_pool(&local),
            "pool: local\nkind: dir\nlocation: /srv/pool\nshared: false\n"
        );
        let fast = json!({
            "id": "id-fast",
            "name": "fast",
            "spec": {
                "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&fast, &nodes),
            ["fast", "lvm-thin", "vg0/tp", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&fast),
            "pool: fast\nkind: lvm-thin\nlocation: vg0/tp\nshared: false\n"
        );
        let tank = json!({
            "id": "id-tank",
            "name": "tank",
            "spec": {
                "backend": { "kind": "zfs", "dataset": "tank/opl" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&tank, &nodes),
            ["tank", "zfs", "tank/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&tank),
            "pool: tank\nkind: zfs\nlocation: tank/opl\nshared: false\n"
        );
        // RBD 풀의 위치는 `POOL[/NAMESPACE]`, 상세는 사용자·conf가 있으면 보인다.
        let ceph = json!({
            "id": "id-ceph",
            "name": "ceph",
            "spec": {
                "backend": { "kind": "rbd", "pool": "rbd" },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&ceph, &nodes),
            ["ceph", "rbd", "rbd", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&ceph),
            "pool: ceph\nkind: rbd\nlocation: rbd\nshared: true\n"
        );
        let opl = json!({
            "id": "id-opl",
            "name": "opl",
            "spec": {
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                    "conf": "/etc/ceph/opl.conf"
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&opl, &nodes),
            ["opl", "rbd", "rbd/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&opl),
            "pool: opl\nkind: rbd\nlocation: rbd/opl\nuser: opl\nconf: /etc/ceph/opl.conf\n\
             shared: true\n"
        );
        // 공유 LVM 풀의 위치는 `VG`, iSCSI면 `VG @ PORTAL TARGET`
        let fc = json!({
            "id": "id-fc",
            "name": "fc",
            "spec": {
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&fc, &nodes),
            ["fc", "lvm-shared", "san", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&fc),
            "pool: fc\nkind: lvm-shared\nlocation: san\nshared: true\n"
        );
        let san = json!({
            "id": "id-san",
            "name": "san",
            "spec": {
                "backend": {
                    "kind": "lvm-shared", "vg": "san",
                    "iscsi": { "portal": "10.0.0.5:3260", "target": "iqn.2003-01.org.example:san" }
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&san, &nodes),
            [
                "san",
                "lvm-shared",
                "san @ 10.0.0.5:3260 iqn.2003-01.org.example:san",
                "1",
                "0/1"
            ]
        );
        assert_eq!(
            describe_pool(&san),
            "pool: san\nkind: lvm-shared\nlocation: san @ 10.0.0.5:3260 \
             iqn.2003-01.org.example:san\nshared: true\n"
        );
        let mut out = Vec::new();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&local, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE     READY  SIZE  AVAILABLE  ERROR\n\
             id-gone  false  -     -          node not found\n\
             b        false  -     -          no status reported yet\n"
        );
    }

    #[test]
    fn node_list_shows_witnesses_and_fencing() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let mut a = node("a", Some(false));
        a["status"]["ha"] = json!({ "fencing": "watchdog", "recover_after_secs": 65 });
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
            .iter()
            .map(node_row)
            .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다. 펜싱이 꺼진 노드는 `ha`가 없다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  true    4     1.0GiB  false        false    -\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다. 펜싱이 꺼졌으면(null) 또는 옛
        // 서버라 `fencing`이 없으면 `-`다.
        let mut a = member("a", true, false);
        a["fencing"] = json!("process");
        let mut b = member("b", true, false);
        b["fencing"] = Value::Null;
        assert_eq!(
            status(vec![a, b, member("w", true, true)]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS  FENCING\n\
             a     id-a  a:8006   voter  true    true    false    process\n\
             b     id-b  b:8006   voter  true    false   false    -\n\
             w     id-w  w:8006   voter  true    false   true     -\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
        members: [
          member("n1", "node-a"),
          member("n2", "node-b"),
          member("n3", null, { voter: false, online: false, address: null }),
```

을 다음으로 바꾼다:

```tsx
        members: [
          member("n1", "node-a", { fencing: "watchdog" }),
          member("n2", "node-b", { fencing: null }),
          member("n3", null, { voter: false, online: false, address: null }),
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
  expect(within(table).queryAllByText("Witness")).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Issue a join token" })).not.toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
  expect(within(table).queryAllByText("Witness")).toHaveLength(1);
  // 펜싱이 꺼졌거나(null) 모르면(옛 서버) 대시다.
  const column = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Fencing");
  const fencing = (name: string) =>
    within(within(table).getByText(name).closest("tr")!).getAllByRole("cell")[column];
  expect(fencing("node-a")).toHaveTextContent("Watchdog");
  expect(fencing("node-b")).toHaveTextContent("—");
  expect(fencing("witness-w")).toHaveTextContent("—");
  expect(screen.queryByRole("button", { name: "Issue a join token" })).not.toBeInTheDocument();
```

`web/src/cluster/ClusterPage.tsx`에서

```tsx
              <TableHead>{t("cluster.role")}</TableHead>
              <TableHead>{t("nodes.address")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
              <TableHead>{t("cluster.role")}</TableHead>
              <TableHead>{t("nodes.fencing")}</TableHead>
              <TableHead>{t("nodes.address")}</TableHead>
```

`web/src/cluster/ClusterPage.tsx`에서

```tsx
                </TableCell>
                <TableCell className="font-mono text-xs">{m.address ?? "—"}</TableCell>
```

을 다음으로 바꾼다:

```tsx
                </TableCell>
                <TableCell>{m.fencing ? t(`nodes.fencingMode.${m.fencing}`) : "—"}</TableCell>
                <TableCell className="font-mono text-xs">{m.address ?? "—"}</TableCell>
```

`web/src/i18n/en.json`에서

```json
    "task_failed": "Task error: {{message}}",
    "internal": "The server failed: {{message}}"
  },
```

을 다음으로 바꾼다:

```json
    "task_failed": "Task error: {{message}}",
    "internal": "The server failed: {{message}}",
    "ha_unsupported": "The node has fencing off and cannot run HA VMs."
  },
```

`web/src/i18n/en.json`에서

```json
    "noDisks": "No disks.",
    "noNics": "No network interfaces."
  },
```

을 다음으로 바꾼다:

```json
    "noDisks": "No disks.",
    "noNics": "No network interfaces.",
    "ha": "HA"
  },
```

`web/src/i18n/en.json`에서

```json
      "online": "The node is online. Run oplctl cluster leave on it."
    }
```

을 다음으로 바꾼다:

```json
      "online": "The node is online. Run oplctl cluster leave on it."
    },
    "fencing": "Fencing",
    "fencingMode": {
      "watchdog": "Watchdog",
      "process": "Process"
    }
```

`web/src/i18n/en.json`에서

```json
    "scheduleMixed": "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools, Ceph RBD pools) are mixed, so scheduled snapshots will fail.",
    "scheduleSharedLvm": "{{volume}} is in a shared LVM pool, so scheduled snapshots will fail."
  },
```

을 다음으로 바꾼다:

```json
    "scheduleMixed": "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools, Ceph RBD pools) are mixed, so scheduled snapshots will fail.",
    "scheduleSharedLvm": "{{volume}} is in a shared LVM pool, so scheduled snapshots will fail.",
    "ha": "High availability (HA)",
    "haHint": "Runs only on nodes with fencing on. If its node loses the cluster majority, the node stops the VM.",
    "haNodeOff": "{{node}} has fencing off and cannot run HA VMs.",
    "haNoNode": "No node has fencing on, so HA VMs cannot run."
  },
```

`web/src/i18n/ko.json`에서

```json
    "task_failed": "작업 오류: {{message}}",
    "internal": "서버 오류: {{message}}"
  },
```

을 다음으로 바꾼다:

```json
    "task_failed": "작업 오류: {{message}}",
    "internal": "서버 오류: {{message}}",
    "ha_unsupported": "노드의 펜싱이 꺼져 있어 HA VM을 돌릴 수 없습니다."
  },
```

`web/src/i18n/ko.json`에서

```json
    "noDisks": "디스크가 없습니다.",
    "noNics": "네트워크 인터페이스가 없습니다."
  },
```

을 다음으로 바꾼다:

```json
    "noDisks": "디스크가 없습니다.",
    "noNics": "네트워크 인터페이스가 없습니다.",
    "ha": "HA"
  },
```

`web/src/i18n/ko.json`에서

```json
      "online": "노드가 온라인입니다. 그 노드에서 oplctl cluster leave를 실행하세요."
    }
```

을 다음으로 바꾼다:

```json
      "online": "노드가 온라인입니다. 그 노드에서 oplctl cluster leave를 실행하세요."
    },
    "fencing": "펜싱",
    "fencingMode": {
      "watchdog": "Watchdog",
      "process": "프로세스"
    }
```

`web/src/i18n/ko.json`에서

```json
    "scheduleMixed": "스냅샷 방식(qcow2, LVM-thin 풀, ZFS 풀, Ceph RBD 풀)이 다른 디스크가 섞여 있어 예약 스냅샷이 실패합니다.",
    "scheduleSharedLvm": "{{volume}}이(가) 공유 LVM 풀에 있어 예약 스냅샷이 실패합니다."
  },
```

을 다음으로 바꾼다:

```json
    "scheduleMixed": "스냅샷 방식(qcow2, LVM-thin 풀, ZFS 풀, Ceph RBD 풀)이 다른 디스크가 섞여 있어 예약 스냅샷이 실패합니다.",
    "scheduleSharedLvm": "{{volume}}이(가) 공유 LVM 풀에 있어 예약 스냅샷이 실패합니다.",
    "ha": "고가용성(HA)",
    "haHint": "펜싱이 켜진 노드에서만 돕니다. 노드가 클러스터 과반과 끊기면 그 노드가 VM을 끕니다.",
    "haNodeOff": "{{node}}의 펜싱이 꺼져 있어 HA VM을 돌릴 수 없습니다.",
    "haNoNode": "펜싱이 켜진 노드가 없어 HA VM을 돌릴 수 없습니다."
  },
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
  expect(within(a).queryByText("Witness")).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(within(a).queryByText("Witness")).not.toBeInTheDocument();
});

it("shows how each node fences", async () => {
  cluster();
  const fenced = {
    ...NODE,
    status: { ...NODE.status, ha: { fencing: "watchdog" as const, recover_after_secs: 65 } },
  };
  mockLists({ node: [fenced, DOWN] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  const column = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Fencing");
  expect(within(a).getAllByRole("cell")[column]).toHaveTextContent("Watchdog");
  // 펜싱이 꺼진 노드는 `ha`가 없다.
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(within(b).getAllByRole("cell")[column]).toHaveTextContent("—");
});
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
            <TableHead>{t("nodes.arch")}</TableHead>
            <TableHead>{t("nodes.cpus")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableHead>{t("nodes.arch")}</TableHead>
            <TableHead>{t("nodes.fencing")}</TableHead>
            <TableHead>{t("nodes.cpus")}</TableHead>
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 11 : 10} query={nodes} empty={t("dashboard.noNodes")} />
          )}
```

을 다음으로 바꾼다:

```tsx
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 12 : 11} query={nodes} empty={t("dashboard.noNodes")} />
          )}
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
              <TableCell>{node.status.arch ?? "—"}</TableCell>
              <TableCell>{node.status.cpus}</TableCell>
```

을 다음으로 바꾼다:

```tsx
              <TableCell>{node.status.arch ?? "—"}</TableCell>
              {/* 펜싱이 꺼진 노드(위트니스 포함)는 HA VM을 돌리지 않는다. */}
              <TableCell>
                {node.status.ha ? t(`nodes.fencingMode.${node.status.ha.fencing}`) : "—"}
              </TableCell>
              <TableCell>{node.status.cpus}</TableCell>
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx

it("shows loading instead of zeros, then errors", async () => {
```

을 다음으로 바꾼다:

```tsx

it("shows how each node fences", async () => {
  const fenced = {
    ...NODE,
    status: { ...NODE.status, ha: { fencing: "process" as const, recover_after_secs: 35 } },
  };
  const off = { ...NODE, id: "n2", name: "node-b" };
  mockLists({ node: [fenced, off], vm: [] });
  server.use(cluster("n1"));
  renderPage(<DashboardPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const column = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Fencing");
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  expect(within(a).getAllByRole("cell")[column]).toHaveTextContent("Process");
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(within(b).getAllByRole("cell")[column]).toHaveTextContent("—");
});

it("shows loading instead of zeros, then errors", async () => {
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
              <TableHead>{t("dashboard.hypervisor")}</TableHead>
              <TableHead>{t("dashboard.activeVms")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
              <TableHead>{t("dashboard.hypervisor")}</TableHead>
              <TableHead>{t("nodes.fencing")}</TableHead>
              <TableHead>{t("dashboard.activeVms")}</TableHead>
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
              <TableRow>
                <TableCell colSpan={6} className="text-center text-muted-foreground">
                  {nodes.data && vms.data ? t("dashboard.noNodes") : waiting(t, nodes, vms)}
```

을 다음으로 바꾼다:

```tsx
              <TableRow>
                <TableCell colSpan={7} className="text-center text-muted-foreground">
                  {nodes.data && vms.data ? t("dashboard.noNodes") : waiting(t, nodes, vms)}
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
                  <TableCell>{node.status.hypervisor}</TableCell>
                  <TableCell>{used.vms}</TableCell>
```

을 다음으로 바꾼다:

```tsx
                  <TableCell>{node.status.hypervisor}</TableCell>
                  <TableCell>
                    {node.status.ha ? t(`nodes.fencingMode.${node.status.ha.fencing}`) : "—"}
                  </TableCell>
                  <TableCell>{used.vms}</TableCell>
```

`web/src/test/fixtures.ts`에서

```ts
      node: "n1",
    },
```

을 다음으로 바꾼다:

```ts
      node: "n1",
      ha: false,
    },
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
  expect(link).toHaveFocus();
});
```

을 다음으로 바꾼다:

```tsx
  expect(link).toHaveFocus();
});

it("marks an HA VM", async () => {
  mockLists({
    vm: [web, { ...web, id: "x", name: "app", spec: { ...web.spec, ha: true } }],
    node: [NODE],
  });
  const { unmount } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByRole("heading", { name: "web" })).toBeInTheDocument();
  expect(screen.queryByText("HA")).not.toBeInTheDocument();
  unmount();
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/app" });
  expect(await screen.findByRole("heading", { name: "app" })).toBeInTheDocument();
  expect(screen.getByText("HA")).toBeInTheDocument();
});
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { ReasonButton, ReasonList } from "@/components/Reason";
import { Button } from "@/components/ui/button";
```

을 다음으로 바꾼다:

```tsx
import { ReasonButton, ReasonList } from "@/components/Reason";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
        <VmPhaseBadge vm={vm} />
        {node && (
```

을 다음으로 바꾼다:

```tsx
        <VmPhaseBadge vm={vm} />
        {vm.spec.ha && <Badge variant="outline">{t("vms.ha")}</Badge>}
        {node && (
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
    spec: { pool: "fast", size_bytes: 16 * 1024 ** 3, format: "raw" },
  });
});
```

을 다음으로 바꾼다:

```tsx
    spec: { pool: "fast", size_bytes: 16 * 1024 ** 3, format: "raw" },
  });
});

const FENCED = {
  ...NODE,
  status: { ...NODE.status, ha: { fencing: "watchdog" as const, recover_after_secs: 65 } },
};
const UNFENCED = { ...NODE, id: "n2", name: "node-b" };

it("turns HA on only for nodes with fencing on and says why not", async () => {
  mockLists({ node: [FENCED, UNFENCED] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/vms"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ code: "ha_unsupported", message: "node is off" }, { status: 409 });
    }),
  );
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  const ha = screen.getByRole("switch", { name: "High availability (HA)" });
  // 자동이면 HA 가능 노드가 하나라도 있으면 켤 수 있다.
  expect(ha).toBeEnabled();
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(ha).toBeDisabled();
  expect(screen.getByText("node-b has fencing off and cannot run HA VMs.")).toBeInTheDocument();
  await user.selectOptions(screen.getByLabelText("Node"), "node-a");
  expect(ha).toBeEnabled();
  expect(screen.queryByText(/has fencing off/)).not.toBeInTheDocument();
  await user.click(ha);
  expect(ha).toBeChecked();
  await user.click(screen.getByRole("button", { name: "Create" }));
  // 서버가 거부하면 번역한 이유를 보인다.
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The node has fencing off and cannot run HA VMs.",
  );
  expect(body).toMatchObject({ name: "web", spec: { node: "n1", ha: true } });
});

it("says when no node can run HA VMs but lets an HA VM turn HA off", async () => {
  const app = vm("app", "running", {
    spec: { ...vm("app", "running").spec, node: "n2", ha: true },
  });
  mockLists({ vm: [app], node: [UNFENCED] });
  let body: { spec: { ha: boolean } } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${app.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(app);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/app/edit" });
  const user = userEvent.setup();
  const ha = await screen.findByRole("switch", { name: "High availability (HA)" });
  // 실행 중이어도 HA는 바꿀 수 있고, 켜진 HA는 끌 수 있다.
  expect(ha).toBeChecked();
  expect(ha).toBeEnabled();
  expect(
    await screen.findByText("node-b has fencing off and cannot run HA VMs."),
  ).toBeInTheDocument();
  await user.click(ha);
  expect(ha).toBeDisabled();
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(body?.spec.ha).toBe(false));
});

it("says when no node has fencing on", async () => {
  mockLists({ node: [UNFENCED] });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  expect(await screen.findByRole("switch", { name: "High availability (HA)" })).toBeDisabled();
  expect(screen.getByText("No node has fencing on, so HA VMs cannot run.")).toBeInTheDocument();
});
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx
  expect(await screen.findByText("Start: 1 requested, 1 failed, 1 skipped.")).toBeInTheDocument();
  expect(started).toEqual([db.id]);
});
```

을 다음으로 바꾼다:

```tsx
  expect(await screen.findByText("Start: 1 requested, 1 failed, 1 skipped.")).toBeInTheDocument();
  expect(started).toEqual([db.id]);
});

it("marks HA VMs", async () => {
  const ha = vm("app", "running", { spec: { ...vm("app", "running").spec, ha: true } });
  mockLists({ vm: [web, ha], node: [NODE] });
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "app" });
  expect(within(rowOf("app")).getByText("HA")).toBeInTheDocument();
  expect(within(rowOf("web")).queryByText("HA")).not.toBeInTheDocument();
});
```

`web/src/vms/VmListPage.tsx`에서

```tsx
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
```

을 다음으로 바꾼다:

```tsx
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
```

`web/src/vms/VmListPage.tsx`에서

```tsx
        cell: ({ row }) => (
          <Link
            to={`/vms/${encodeURIComponent(row.original.name)}`}
            className="font-medium underline-offset-4 hover:underline"
          >
            {row.original.name}
          </Link>
        ),
```

을 다음으로 바꾼다:

```tsx
        cell: ({ row }) => (
          <>
            <Link
              to={`/vms/${encodeURIComponent(row.original.name)}`}
              className="font-medium underline-offset-4 hover:underline"
            >
              {row.original.name}
            </Link>
            {row.original.spec.ha && (
              <Badge variant="outline" className="ml-2">
                {t("vms.ha")}
              </Badge>
            )}
          </>
        ),
```

`web/src/vms/form.test.ts`에서

```ts
  gibToBytes,
  toSpec,
```

을 다음으로 바꾼다:

```ts
  gibToBytes,
  haUnavailable,
  toSpec,
```

`web/src/vms/form.test.ts`에서

```ts
      snapshot_schedule: { every: "daily", keep: 7, memory: true },
    },
```

을 다음으로 바꾼다:

```ts
      snapshot_schedule: { every: "daily", keep: 7, memory: true },
      ha: true,
    },
```

`web/src/vms/form.test.ts`에서

```ts
    "disks.0.size": "vmForm.errors.size",
  });
});
```

을 다음으로 바꾼다:

```ts
    "disks.0.size": "vmForm.errors.size",
  });
});

it("says why HA cannot be turned on for the chosen node", () => {
  const fenced = {
    ...NODE,
    status: { ...NODE.status, ha: { fencing: "process" as const, recover_after_secs: 7 } },
  };
  const off = { ...NODE, id: "n2", name: "node-b" };
  const witness = { ...fenced, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  // 노드 목록을 모르면 서버에 맡긴다.
  expect(haUnavailable(undefined, "n2")).toBeNull();
  expect(haUnavailable([fenced, off], "n1")).toBeNull();
  expect(haUnavailable([fenced, off], "n2")).toEqual({
    key: "vmForm.haNodeOff",
    values: { node: "node-b" },
  });
  expect(haUnavailable([witness], "n3")).toEqual({
    key: "vmForm.haNodeOff",
    values: { node: "node-w" },
  });
  // 자동이면 HA 가능 노드가 하나라도 있으면 된다(위트니스는 세지 않는다).
  expect(haUnavailable([fenced, off], "")).toBeNull();
  expect(haUnavailable([off, witness], "")).toEqual({ key: "vmForm.haNoNode" });
});
```

`web/src/vms/form.ts`에서

```ts
  snapshotSchedule: ScheduleRow;
  /** 만든 뒤 곧바로 시작한다(만들 때만). */
```

을 다음으로 바꾼다:

```ts
  snapshotSchedule: ScheduleRow;
  /** HA VM: 펜싱이 켜진 노드에서만 돈다. */
  ha: boolean;
  /** 만든 뒤 곧바로 시작한다(만들 때만). */
```

`web/src/vms/form.ts`에서

```ts
    snapshotSchedule: { every: "off", keep: "7", memory: false },
    start: true,
```

을 다음으로 바꾼다:

```ts
    snapshotSchedule: { every: "off", keep: "7", memory: false },
    ha: false,
    start: true,
```

`web/src/vms/form.ts`에서

```ts
      : { every: "off", keep: "7", memory: false },
    start: false,
```

을 다음으로 바꾼다:

```ts
      : { every: "off", keep: "7", memory: false },
    ha: s.ha ?? false,
    start: false,
```

`web/src/vms/form.ts`에서

```ts
          },
  };
}
```

을 다음으로 바꾼다:

```ts
          },
    ha: v.ha,
  };
}

/** 서버의 `ha_capable`: 위트니스가 아니고 펜싱이 켜진 노드. HA VM은 이런 노드에서만 돈다. */
export function haCapable(node: Node): boolean {
  return !node.spec.witness && node.status.ha != null;
}

/**
 * HA를 켤 수 없는 이유(번역 키와 값). 서버처럼 지정 노드(`node`, 빈 문자열이면 자동)만 보고, 자동이면 HA 가능
 * 노드가 하나도 없을 때다. 노드 목록을 아직 모르면 막지 않는다.
 */
export function haUnavailable(
  nodes: Node[] | undefined,
  node: string,
): { key: string; values?: Record<string, string> } | null {
  if (!nodes) {
    return null;
  }
  if (node !== "") {
    const chosen = nodes.find((n) => n.id === node);
    return chosen && !haCapable(chosen)
      ? { key: "vmForm.haNodeOff", values: { node: chosen.name } }
      : null;
  }
  return nodes.some(haCapable) ? null : { key: "vmForm.haNoNode" };
}
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { defaultMachine, type VmFormValues } from "@/vms/form";

/** 이름·CPU·메모리·펌웨어·머신·노드·재시작 정책 */
export function GeneralSection({
```

을 다음으로 바꾼다:

```tsx
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import { defaultMachine, haUnavailable, type VmFormValues } from "@/vms/form";

/** 이름·CPU·메모리·펌웨어·머신·노드·재시작 정책·HA */
export function GeneralSection({
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
  const { t } = useTranslation();
  return (
```

을 다음으로 바꾼다:

```tsx
  const { t } = useTranslation();
  const noHa = haUnavailable(nodes, values.node);
  return (
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
        </Field>
      </CardContent>
```

을 다음으로 바꾼다:

```tsx
        </Field>
        <div className="flex flex-col gap-1.5 sm:col-span-2">
          <div className="flex items-center gap-2">
            {/* 켤 수 없는 노드여도 이미 켜진 HA는 끌 수 있다(실행 중이어도 바꿀 수 있다). */}
            <Switch
              id="vm-ha"
              aria-describedby="vm-ha-hint"
              disabled={noHa !== null && !values.ha}
              checked={values.ha}
              onCheckedChange={(ha) => onChange({ ha })}
            />
            <Label htmlFor="vm-ha">{t("vmForm.ha")}</Label>
          </div>
          <p id="vm-ha-hint" className="text-xs text-muted-foreground">
            {t("vmForm.haHint")}
          </p>
          {noHa && (
            <p className={values.ha ? "text-sm text-destructive" : "text-sm text-muted-foreground"}>
              {t(noHa.key, noHa.values)}
            </p>
          )}
        </div>
      </CardContent>
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 818개, 실패 0

Run: `cargo test -p oplctl -p opl-api --test cluster`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 321개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat: HA 표시 — oplctl vm set --ha, FENCING 열, 웹 HA 스위치와 배지"
```

---

### Task 6: 문서

README에 "HA VM과 자기 펜싱" 절(HA 켜기, `[ha]`와 펜싱 방식, HA 가능 노드, 리스를 잃으면, watchdog 준비 — softdog·sanlock `wdmd`·nowayout, opld를 멈추면 노드가 재시작될 수 있음, 한계, 수동 watchdog 시험), 설계 문서 §3~§7의 "구현(계획 23)" 메모, 처음 분해 문서 §1.1.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown

## 웹 UI
```

을 다음으로 바꾼다:

````markdown

## HA VM과 자기 펜싱

HA VM은 노드가 클러스터 과반수와 끊기면 그 노드가 스스로 끈다(자기 펜싱, Proxmox HA의 watchdog 펜싱과 같은
자리). 노드는 리더에게 몇 초마다 **리스**를 갱신하고, 마지막으로 성공한 갱신 뒤 `fence_after_secs`가 지나면 그
노드의 HA VM을 강제로 끈다. opld 자신이 멈추거나 멈춰 서도 끄도록 Linux watchdog 장치를 쥐고 있다가, 리스를 잃은
채 HA VM이 돌면 쓰다듬기를 멈춰 노드를 재시작시킨다. **죽은 노드의 HA VM을 다른 노드에서 다시 켜는 복구는 계획
24에서 더한다.** 지금은 펜싱된 HA VM이 그 노드에 남아 있다가 리스가 돌아오면 그 노드에서 다시 켜진다.

```bash
oplctl vm create web --memory 1024 --disk root --ha --restart on-failure --start
oplctl vm set web --no-ha            # 끄기(--ha로 다시 켠다). 실행 중이어도 바꿀 수 있다.
oplctl vm list                       # HA 열(true/false)
oplctl node list                     # FENCING 열: watchdog · process · -(꺼짐)
oplctl cluster status                # 멤버마다 FENCING 열
```

웹은 VM 만들기·편집의 "고가용성(HA)" 스위치(그 노드의 펜싱이 꺼져 있으면 이유와 함께 막는다), VM 목록·상세의
HA 배지, 노드·클러스터·대시보드 노드 표의 "펜싱" 열이다. API는 VM `spec.ha`(생성·`PUT /api/v1/vms/{key}`),
Node `status.ha`(`{"fencing": "watchdog"|"process", "recover_after_secs": …}`, 펜싱이 꺼져 있으면 없다),
`GET /api/v1/cluster`의 멤버 `fencing`(꺼져 있으면 null)이다. 같은 노드에서의 크래시 재시작은 지금처럼
`restart`가 정하므로 HA VM에는 `on-failure`를 권한다.

```toml
# /etc/opl/opld.toml (노드마다, 생략하면 아래 기본값)
[ha]
fencing = "watchdog"              # watchdog | process | off (기본: Linux watchdog, macOS off)
watchdog_device = "/dev/watchdog" # Linux watchdog 장치(하드웨어 또는 softdog)
watchdog_timeout_secs = 30        # 장치에 정하는 제한 시간(5~3600초, WDIOC_SETTIMEOUT)
fence_after_secs = 30             # 마지막 리스 갱신 뒤 이만큼 지나면 HA VM을 끈다(1초 이상)
```

- **펜싱 방식.** `watchdog`: opld가 HA VM을 끄고, opld가 멈추면 watchdog가 노드를 재시작한다(운영용). `process`:
  opld가 끄는 것만 한다 — opld가 죽거나 멈추면 아무것도 끄지 않으므로 **시험·개발용**이다(macOS 노드도 쓸 수 있다).
  `off`: 이 노드는 HA VM을 돌리지 않는다. Linux가 아니면 `watchdog`는 설정 오류다. 위트니스는 `[ha]`를 쓰지 않는다.
- **HA 가능 노드.** 위트니스가 아니고 펜싱이 켜진 노드만 HA VM을 돌린다. HA VM의 노드(`--node`, 편집)나
  마이그레이션 대상이 펜싱이 꺼진 노드면 **409** `ha_unsupported`, 노드를 정하지 않은 HA VM은 스케줄러가 HA 가능
  노드만 고른다(없으면 **409** `no_node`). 노드는 등록할 때마다 `[ha]`를 `status.ha`에 쓴다. `recover_after_secs`는
  `fence_after_secs + watchdog_timeout_secs(watchdog일 때) + 5`(기본 65초)로, 계획 24의 리더가 이만큼 기다린 뒤 옮긴다.
- **리스.** 갱신 간격은 `fence_after_secs`의 10분의 1(0.2초 이상 2초 이하, 기본 2초)이다. 리더는 아직 과반수의
  리더인지 확인한 뒤에만 갱신을 받아 준다. 그래서 과반수와 끊긴 노드는 물론, 고립된 리더 자신도 갱신하지 못한다.
  리스는 opld가 시작한 때부터 센다(다시 시작한 직후 곧바로 끄지 않는다).
- **리스를 잃으면.** `fence_after_secs`가 지나면 그 노드의 HA VM을 강제로 끈다(실패로 세지 않고 원하는 상태는
  running 그대로, status "fenced: lost the cluster majority"). HA 아닌 VM은 그대로 돈다. 리스가 돌아오면 그 노드에서
  다시 켠다. 리스 없이는 HA VM을 켜지도, 멈춘 채 받은 마이그레이션을 이어서 실행하지도 않는다("waiting for the
  cluster majority before starting an HA VM", 실패로 세지 않고 다시 본다). 켜기 바로 앞에서 리스와 VM을 다시 보아,
  그 사이 다른 노드로 배정됐거나 멈추라고 했으면 켜지 않는다. 짧은 네트워크 장애라도 `fence_after_secs`
  보다 길면 HA VM이 꺼진다.
- **watchdog.** 이 노드에서 HA VM을 켜거나 이미 도는 HA VM에 붙을 때 장치를 처음 열고(그 전에는 열지 않는다 —
  HA VM이 없는 Linux 노드는 장치를 건드리지 않는다), 그 뒤로 opld가 끝날 때까지 쥔 채 1초마다 쓰다듬는다. 리스가
  끝났는데 HA VM이 돌면 쓰다듬기를 멈춘다. opld가 제한 시간 안에 HA VM을 끄면 다시 쓰다듬으므로 노드는 재시작되지
  않고, opld가 멈춰 끄지 못하면 노드가 재시작된다. 장치를 열지 못하면(없음, 다른 프로세스가 쥠) HA VM을 켜지 않고
  status에 "cannot open the watchdog device: …"를 남긴다. 장치가 요청보다 긴 제한 시간을 고르거나 제한 시간을 정하지
  못하면 HA VM에 쓰지 않는다(status "the watchdog device is unusable: …", 짧은 값을 고르면 경고만 남기고 쓴다). 그런
  장치도 이미 열어 타이머가 돌므로 닫지 않고 쥔 채 같은 규칙으로 쓰다듬고, 정상 종료 때 magic close로 멈춘다.
- **opld를 멈추면 노드가 재시작될 수 있다.** 정상 종료(SIGTERM) 때 이 노드에 도는 HA VM이 없으면 watchdog를 멈추고
  닫는다(magic close). **도는 HA VM이 있으면 그대로 둔다**(경고 "HA VMs are running; leaving the watchdog armed …") —
  QEMU는 opld 없이도 계속 돌므로, opld가 `watchdog_timeout_secs` 안에 다시 떠 리스를 갱신하지 못하면 노드가
  재시작된다. 업그레이드·점검으로 opld를 오래 멈출 때는 먼저 HA VM을 끄거나 다른 노드로 옮기거나 `vm set --no-ha`를
  한다. 펜싱을 `off`로 바꿔 다시 띄운 노드의 이미 도는 HA VM은 끄지 않고 status에 "HA is not available on this
  node"만 남긴다(멈춘 HA VM은 켜지 않는다).
- **watchdog 준비(Linux).** 하드웨어 watchdog(`iTCO_wdt`, IPMI 등)가 없으면 `softdog`를 올린다:
  `echo softdog | sudo tee /etc/modules-load.d/softdog.conf && sudo modprobe softdog`.
  `ls /sys/class/watchdog/`와 `cat /sys/class/watchdog/watchdog0/identity`로 장치를 확인한다. watchdog 장치는 한
  프로세스만 열 수 있다. **공유 LVM 풀의 sanlock `wdmd`가 이미 쥔 장치는 쓸 수 없으므로** 그 노드에는 다른 장치(예:
  하드웨어 watchdog를 `watchdog_device = "/dev/watchdog1"`로)가 있어야 한다. softdog는 장치를 하나만 만든다.
  **`nowayout` 장치는 쓰지 않는다** — magic close가 듣지 않아 HA VM이 없어도 opld를 멈추면 노드가 재시작된다
  (`cat /sys/class/watchdog/watchdog0/nowayout`이 0이어야 한다. 커널 `CONFIG_WATCHDOG_NOWAYOUT`이나 softdog의
  `nowayout=1`이면 1이다).
- **한계.** 라이브 마이그레이션이 진행 중인(보류 중인) HA VM은 리스를 잃어도 마이그레이션이 끝나거나 풀릴 때까지
  끄지 않는다(`process`에서는 긴 마이그레이션이 `fence_after_secs`를 넘길 수 있다. `watchdog`면 쓰다듬기를 멈추므로
  노드가 재시작된다). 실패한 마이그레이션이 원본을 멈춘(`paused`) 채로 남기면 HA VM은 이어서 실행되지 않는다(드묾 —
  끄고 다시 켠다).

**수동 watchdog 시험(Linux, root, 직접 할 때만 — 자동 시험·CI는 진짜 watchdog를 쓰지 않는다).** 버려도 되는 시험
노드에서 한다. `soft_noboot=1`이면 softdog는 제한 시간이 지나도 재시작하지 않고 커널 로그에
"softdog: Triggered - Reboot ignored"만 남긴다. 다른 프로세스(`wdmd` 등)가 watchdog를 쓰지 않는지 먼저 본다.

```bash
sudo modprobe -r softdog; sudo modprobe softdog soft_noboot=1   # 이미 올라와 있으면 내렸다가 다시
cat /sys/class/watchdog/watchdog0/identity                      # Software Watchdog
# opld.toml: [ha] fencing = "watchdog", watchdog_timeout_secs = 10, fence_after_secs = 10
sudo opld --config /etc/opl/opld.toml &
oplctl vm create hatest --memory 256 --ha --start
cat /sys/class/watchdog/watchdog0/state                         # active (opld가 열었다)
# 1) opld가 멈춰 서면: 쓰다듬기가 멈춰 약 10초 뒤 "Triggered - Reboot ignored"
sudo kill -STOP "$(pidof opld)"; sleep 15; sudo dmesg | grep softdog; sudo kill -CONT "$(pidof opld)"
# 2) HA VM이 돌 때 정상 종료: 경고 로그와 "watchdog did not stop!", 약 10초 뒤 Triggered
sudo kill -TERM "$(pidof opld)"; sleep 15; sudo dmesg | tail
# 3) HA VM이 없을 때 정상 종료: magic close로 state가 inactive, Triggered가 없다
sudo opld --config /etc/opl/opld.toml &                         # 다시 띄우면 도는 hatest에 붙어 장치를 다시 연다
oplctl vm stop hatest; oplctl vm list                           # PHASE stopped를 확인한 뒤
sudo kill -TERM "$(pidof opld)"; sleep 15; cat /sys/class/watchdog/watchdog0/state
# 4) (셋 이상의 클러스터) 이 노드와 다른 멤버 사이의 cluster_listen(8007)을 막으면 약 10초 뒤 HA VM이
#    "fenced: lost the cluster majority"로 꺼지고 Triggered는 없다. 풀면 리스가 돌아와 다시 켜진다.
sudo modprobe -r softdog                                        # 끝나면
```

## 웹 UI
````

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 5는 계획 22(위트니스 노드: VM을 돌리지 않는 Raft 투표 멤버, 완료), 계획 23(HA 리스와 watchdog 자기 펜싱), 계획 24(HA VM 복구)로 나눴다(`2026-11-02-ha-fencing-witness-design.md`). 23·24는 아직이다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 5는 계획 22(위트니스 노드: VM을 돌리지 않는 Raft 투표 멤버, 완료), 계획 23(HA 리스와 watchdog 자기 펜싱: `VmSpec.ha`, 리더의 리스 표, 노드의 `HaGuard`, 리스를 잃으면 HA VM을 끔, 완료), 계획 24(HA VM 복구)로 나눴다(`2026-11-02-ha-fencing-witness-design.md`). 24는 아직이다.
```

`docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md` 전체:

```markdown
# opLite HA·펜싱·위트니스 설계 (하위 프로젝트 5: 계획 22·23·24)

작성일: 2026-11-02
상위 문서: `2026-10-20-opl-node-core-design.md`(하위 프로젝트 분해, 노드 감시 §8, reconciler 규칙 §5.3), `2026-10-30-live-migration-design.md`(revision 검사 확정, UEFI 변수 위치), `2026-10-30-shared-storage-iscsi-lvm-design.md`(sanlock 잠금)

## 1. 목표와 범위

처음 분해에서 하위 프로젝트 5는 HA와 펜싱, 2노드 위트니스(ha-manager·vSphere HA)다. 지금은 노드가 죽으면 그 노드의 VM이 멈춘 채 남는다. `spec.node`는 죽은 노드를 가리키고, status는 낡은 `Running`이며, API도 실행 중으로 보여 노드를 바꾸지 못한다. 2노드 클러스터는 한 노드만 죽어도 과반수를 잃는다.

| 계획 | 내용 |
|---|---|
| 22 | 위트니스 노드: VM을 돌리지 않는 Raft 투표 멤버(QEMU 불필요, 선거에 나서지 않음), 합류·상태 표시, 배치에서 빼기 |
| 23 | HA 리스와 자기 펜싱: `VmSpec.ha`, 노드의 펜싱 설정(watchdog·프로세스), 리더가 확인하는 리스, 리스를 잃은 노드는 HA VM을 끈다 |
| 24 | 복구: 리더가 리스가 끝난 노드의 HA VM을 다른 노드로 옮겨 켠다. 이미지 잠금 기다리기, 기록 Task, 실제 시험 |

넣지 않는 것: IPMI·BMC 같은 외부 펜싱 장치, 노드 유지보수 때 모두 옮기기(유지보수 모드는 지금처럼 배치만 막는다), HA 그룹·노드 선호·우선순위·반친화성, 복구 뒤 원래 노드로 되돌리기, HA 아닌 VM의 자동 복구, 죽은 노드에만 있던 UEFI 변수 살리기, RBD 배타 잠금·blocklist, sanlock `wdmd`와 watchdog 나눠 쓰기, 노드 하나에 여러 클러스터의 위트니스.

### 견준 길

- **펜싱: 외부 장치(IPMI, STONITH) vs watchdog 자기 펜싱(채택).** 외부 장치는 확실하지만 하드웨어와 비밀번호 설정이 필요하고 목표 고객(중소 규모)이 늘 갖추고 있지 않다. Proxmox ha-manager처럼 각 노드가 리스를 갱신하지 못하면 스스로 HA VM을 끄고, 그러지 못하면(opld가 멈춤) watchdog가 노드를 재시작한다. 리더는 노드가 스스로 펜싱을 마쳤을 시간이 지난 뒤에만 VM을 옮긴다.
- **리스: Raft에 주기적으로 기록 vs 리더 메모리(채택).** 기록하면 노드 수만큼 초당 쓰기가 생기고 모든 watch가 깨어난다. 노드가 리더에게 갱신을 요청하면 리더가 `ensure_linearizable`로 자기가 과반수의 리더임을 확인한 뒤 받은 시각을 메모리에 적는다. 리더가 바뀌면 새 리더는 기록이 없으므로 리더가 된 때부터 센다(더 늦게 옮길 뿐 안전하다).
- **위트니스: 외부 중재자(corosync-qnetd) vs Raft 투표 멤버(채택).** Raft가 이미 있으므로 VM을 돌리지 않는 opld를 투표 멤버로 두면 따로 만들 프로토콜이 없다(vSAN 위트니스와 같은 모양). 선거에 나서지 않게 해(openraft `enable_elect = false`) 리더 일(감시, 정리, HA 복구)은 늘 일반 노드가 한다.
- **복구 지휘: 리더 루프(채택) vs 대상 노드의 Task.** 펜싱 판단(리스 표)과 배치가 리더에 있으므로 리더가 revision 검사로 `spec.node`를 바꾼다. 마이그레이션 확정과 같은 방식이다.

## 2. 위트니스 (계획 22)

- 설정 `witness = true`(opld.toml 최상위, 기본 false)인 opld는 위트니스다. `[hypervisor]`·`[pools]`·`[migration]`·`[ha]`는 쓰지 않고 QEMU를 찾지 않는다.
- **합류만 된다.** `oplctl cluster init`은 위트니스에서 거부한다("a witness cannot start a cluster"). `oplctl cluster join`은 지금과 같고, 합류 요청에 `witness: true`가 실린다. 받는 쪽은 Node를 `spec.witness = true`로 만든다. 승격까지 일반 노드와 같다(learner → voter).
- **선거에 나서지 않는다.** 위트니스의 Raft 설정은 `enable_elect = false`다. 투표는 하지만 후보가 되지 않으므로 리더가 되지 않는다. 그래서 리더만 하는 일(노드 감시, 정리, 예약 스냅샷, HA 복구)을 띄우지 않는다.
- **도는 것:** 클러스터 저장소, 노드 간 서버(Raft RPC, 리더 전달, Ping — 하이퍼바이저 없음), 승격, Node 등록. 이 노드는 HTTPS API, reconciler, Task 실행기, 풀 마운터, 받는 마이그레이션을 띄우지 않는다. Node 등록은 `online = true`와 아키텍처만 기록하고, cpus·메모리·하이퍼바이저·브리지·풀은 비운다.
- **배치에서 빠진다.** `node_fits`는 위트니스를 "witness node"로 거부한다. 따라서 생성·시작·마이그레이션 대상·Task 노드 선택·HA 복구 대상에서 빠지고, API가 VM의 노드로 위트니스를 받으면 409 `node_unavailable`이다. 풀의 노드 목록과 공유 풀 확인(모든 노드에서 준비됐는지)도 위트니스를 세지 않는다.
- **상태 표시:** `oplctl cluster status`와 `oplctl node list`, 웹 노드 목록에 역할(위트니스)을 보인다. 2노드 경고는 지금처럼 투표 멤버 수가 2일 때다. 일반 노드 둘과 위트니스면 3이므로 경고가 없다. 투표 멤버가 3 이상이어도 일반 노드가 하나뿐이면 "HA needs at least two non-witness nodes"를 보인다.
- 탈퇴·제거는 일반 노드와 같다(배정된 VM이 없다).

구현: 설정은 opld `Config.witness`다. 쓰지 않는 `[hypervisor]`도 읽고 검사는 그대로 거친다(`accel`·`systemd_scope`의 운영체제 규칙 — 잘못된 값이면 시작하지 않는다. `qemu_binary`는 찾지 않으므로 없는 경로여도 된다). `cluster init`은 제어 소켓에서 409 "a witness cannot start a cluster; join an existing cluster instead"(`member::WITNESS_CANNOT_INIT`)이고, init/join 전 포트 검사는 `cluster_listen`만 본다. 멤버로 돌 때 `Member`의 `services`(HTTPS API, reconciler, Task 실행기, 노드 감시, 예약 스냅샷, 정리, 풀 마운터)가 없고 `Host::from_config`·`QemuHypervisor::new`를 부르지 않는다. 노드 간 서버는 `PeerService::new(cluster, None)`이라 마이그레이션·UEFI 변수를 받지 않고, 브리지 새로 고침과 마이그레이션 TLS 파일 쓰기도 없다. Raft 설정은 `raft_config(witness)`의 `enable_elect: !witness`다. Node 등록은 `bootstrap::register_witness`(`online`, `arch`만)이고, 다시 시작했을 때는 일반 노드처럼 뒤에서 될 때까지 다시 해 본다. `spec.address`는 일반 노드처럼 `address:listen 포트`이지만 그 포트는 열리지 않는다(`node list`·`cluster status`의 `ADDRESS`도 이 값이다). 다시 시작할 때 로컬 저장소의 이 노드 Node `spec.witness`가 설정의 `witness`와 다르면 아무것도 띄우지 않고 실패한다(`bootstrap::check_role`). 탈퇴는 빠진 뒤 남는 투표 멤버가 없거나 모두 위트니스면 거절한다(`member::check_leave`, "the remaining voting members are all witnesses, which cannot lead; add another non-witness node first") — 위트니스만 남으면 리더가 생기지 않는다. 역할은 합류 요청의 `witness`로만 정해지고 `PUT /nodes/{key}`는 저장된 값을 그대로 둔다(요청 값 무시). 배치에서 빼기는 `node_fits`("node {name:?} is a witness node")와 `opl_types::storage::pool_usable_on`(위트니스면 false, 이유 "witness nodes do not use storage pools")에 모았다 — 스케줄러, Task 노드 선택, 복제, 공유 풀 확인이 함께 막힌다. VM 생성·수정의 `spec.node`와 마이그레이션 대상은 `vms.rs`의 `check_not_witness`가 409 `node_unavailable`로 거절한다. 위트니스를 이름으로 적은 풀 `spec.nodes`는 받아들인다(쓰이지 않을 뿐 해가 없다). 웹·oplctl의 풀 노드 목록, 웹 VM 폼의 노드 목록·기본 머신 종류, 마이그레이션 대상 목록도 위트니스를 뺀다. 표시: `GET /cluster`의 members에 `witness`, oplctl `node list`·`cluster status`에 `WITNESS` 열(`true`/`false`), `node get`은 JSON의 `spec.witness`다. HA 경고는 투표 멤버가 3 이상이고 위트니스가 아닌 투표 멤버가 2 미만일 때다. 웹은 노드 목록, 클러스터 멤버, 대시보드 노드 표에 "위트니스" 배지를 붙이고, 대시보드의 노드 수는 위트니스도 센다.

## 3. HA 리스와 자기 펜싱 (계획 23)

### 3.1 VM과 노드의 HA 설정

- `VmSpec.ha: bool`(기본 false). HA VM이 원하는 상태가 running인데 그 노드가 펜싱됐으면 다른 노드에서 켠다(§4). 같은 노드에서의 크래시 재시작은 지금처럼 `restart`가 정한다(HA VM에는 `on-failure`를 권한다). 게스트가 스스로 끈 VM은 원하는 상태가 stopped가 되므로 복구하지 않는다.
- 노드 설정 `[ha]`:

| 키 | 기본 | 뜻 |
|---|---|---|
| `fencing` | Linux `watchdog`, macOS `off` | `watchdog`(아래 둘 다), `process`(opld가 HA VM을 끄는 것만 — watchdog 없음, 시험·개발용), `off`(이 노드는 HA VM을 돌리지 않는다) |
| `watchdog_device` | `/dev/watchdog` | Linux watchdog 장치(하드웨어 또는 `softdog`) |
| `watchdog_timeout_secs` | 30 | 장치에 정하는 제한 시간(`WDIOC_SETTIMEOUT`) |
| `fence_after_secs` | 30 | 마지막으로 성공한 리스 갱신 뒤 이만큼 지나면 HA VM을 끈다 |

- 노드는 등록할 때(`register_node`) `NodeStatus.ha: Option<NodeHa>`를 쓴다. `fencing`이 `off`면 None이다. `NodeHa { fencing: Watchdog | Process, recover_after_secs }`이고, `recover_after_secs = fence_after + (watchdog면 watchdog_timeout, 아니면 0) + 5`(여유)다. 리더는 이 노드의 값을 써서 옮길 때를 정한다.
- 갱신 간격은 `fence_after / 10`이다(0.2초 이상 2초 이하).
- **HA 가능 노드** = 위트니스가 아니고 `status.ha`가 있는 노드. API 검사:
  - HA VM을 만들거나 바꿀 때 `spec.node`가 HA 가능 노드가 아니면 409 `ha_unsupported`.
  - 노드가 없는 HA VM을 시작할 때는 스케줄러가 HA 가능 노드만 고른다(없으면 409 `no_capacity`).
  - HA VM의 마이그레이션 대상이 HA 가능 노드가 아니면 409 `ha_unsupported`.
  - 실행 중인 VM의 `ha`는 바꿀 수 있다.

구현(계획 23): 타입은 `opl_types::node`의 `NodeHa`, `Fencing { Watchdog, Process }`(`snake_case`), `ha_capable(&Node)`이고 `NodeStatus.ha`는 없으면 빼고 쓴다(OpenAPI에서도 선택 필드). opld 설정은 `Config.ha: HaSection`(`FencingSetting { Watchdog, Process, Off }`, `deny_unknown_fields`)이고, Linux가 아니면 `watchdog`는 설정 오류다. 검증은 `fence_after_secs ≥ 1`(펜싱이 꺼져 있어도), watchdog면 `watchdog_timeout_secs`가 **5..=3600**이다(1초마다 쓰다듬으므로 너무 짧으면 한 번만 늦어도 재시작된다). `HaSection::node_ha()`·`renew_interval()`·`guard_config()`가 등록 값·갱신 간격·`HaGuard` 설정을 만든다. API 검사는 `vms::check_ha_capable`(생성·수정의 `spec.node`, 마이그레이션 대상 — 마이그레이션은 `node_fits`보다 먼저)이고, 스케줄러는 `node_fits`가 HA VM에 "node {name:?} does not support HA (fencing is off)"로 HA 가능 노드만 남긴다. **HA 가능 노드가 없을 때의 오류는 설계의 `no_capacity`가 아니라 기존 스케줄러 오류인 409 `no_node`다**(`no_capacity`는 마이그레이션 대상 검사의 코드다). 실행 중 VM의 `ha` 수정은 원래 막는 필드가 아니라 바꿀 것이 없었다. 스냅샷 되돌리기는 예약처럼 지금 VM의 `ha`를 지킨다(`write_reverted_spec`).

### 3.2 리스

- **갱신(노드):** HA 가능 노드의 `HaGuard`가 갱신 간격마다 `LeaderRequest::RenewLease { node }`를 보낸다(리더면 자기에게, 아니면 기존 전달 경로). 한 번의 제한은 갱신 간격의 두 배다. 보내기 직전의 시각 s를 재고, 성공하면 `last_renewed = s`. 응답은 리더가 적용한 로그 인덱스이고, 노드는 그 인덱스까지 자기 저장소에 적용된 뒤에 성공으로 친다. 그러면 리스가 다시 살아난 노드는 그 사이 리더가 옮긴 VM을 이미 다른 노드의 것으로 본다(§4.2).
- **처리(리더, opl-cluster):** `ensure_linearizable`로 아직 과반수의 리더인지 확인한다. 그 노드가 펜싱 중(§4.2)이면 `Rejection::Fenced`로 거부한다. 아니면 리스 표에 (현재 임기, 받은 시각 r)을 적고 적용한 인덱스를 돌려준다. 리스 표는 메모리에만 있고 임기가 바뀌면 비운다.
- **안전 조건:** 갱신이 성공했다면 s ≤ r이다(노드는 보내기 전에 쟀다). 노드는 s + fence_after 안에 HA VM을 끄고, 그러지 못해도 watchdog가 마지막 쓰다듬기 + watchdog_timeout 안에 노드를 재시작한다. 노드는 리스가 살아 있을 때(now − s < fence_after)만 쓰다듬는다. 리더는 r + recover_after가 지난 뒤에만 옮긴다. 따라서 옮긴 VM이 켜질 때 원래 노드의 VM은 꺼져 있다. 새 리더는 리더가 된 것을 처음 본 시각 b부터 센다(max(b, r)). 이전 리더가 확인해 준 갱신은 모두 b 전에 일어났다. 새 임기의 과반수가 투표한 뒤에는 이전 리더의 `ensure_linearizable`가 과반수의 응답을 받을 수 없기 때문이다.

구현(계획 23): 리스 표는 `opl-cluster`의 `lease.rs`(`LeaseTable`, `Cluster` 안의 메모리 `Mutex`)다. `LeaderRequest::RenewLease { node }`(전송 형식 `renew-lease`)는 로그에 아무것도 쓰지 않는다. 리더는 `ensure_linearizable`를 **두 번** 한다. 첫 번째로 과반수의 리더인지 확인하고, 같은 잠금 안에서 임기가 그대로인지·펜싱 중인지 본 뒤 (임기, `tokio::time::Instant`)를 적고, 두 번째 확인의 커밋 인덱스를 돌려준다(펜싱 표시를 지우기 전에 끝난 복구 쓰기가 모두 들어 있다. 지표의 적용 인덱스는 쓰기 응답보다 늦게 바뀔 수 있어 쓰지 않는다). 노드 쪽은 `Cluster::renew_lease(timeout)`이고 그 인덱스가 로컬에 적용돼야 성공이다. 리더 쪽 조회는 `Cluster::leases() -> Leases { term, renewed }`(리더에서만 의미가 있고, 표가 더 낮은 임기의 것이면 비운다)다. `Rejection::Fenced { node }`는 opl-api에서 409 `node_fenced`이지만 API 요청으로는 생기지 않는다. 시험 `opl-cluster/tests/lease.rs`(`MemRouter::register(id, &Cluster)`, `MemRouter::block_raft(id)`).

### 3.3 노드 쪽 펜싱 (`HaGuard`, opl-reconciler)

- **상태:**
  - 프로세스 시작 시각을 기준점으로 둔다. 이 덕분에 opld를 다시 시작한 직후 바로 끄지 않는다.
  - `renewed`: 이 프로세스에서 한 번이라도 갱신에 성공했는지.
  - `fresh()`: `renewed`이고 now − last_renewed < fence_after.
  - `stale()`: now − max(시작, last_renewed) ≥ fence_after.
  - fresh/stale이 바뀌면 `watch`로 알린다.
- **워커 규칙**(HA VM만, 원하는 상태가 running일 때):
  - 켜기(`start()`)와 넘겨받은 마이그레이션의 `cont` 전에 `fresh()`이고 watchdog가 켜져 있어야 한다(`arm()`). 아니면 실패로 세지 않고 `retry_interval` 뒤 다시 본다. status: `Starting`, "waiting for the cluster majority before starting an HA VM". 이 노드에 펜싱이 없으면(`fencing = off`) 켜지 않는다. status: "HA is not available on this node".
  - 실행 중인데 `stale()`이면 강제 종료한다. 우리가 요청한 종료로 표시하므로 실패로 세지 않고 원하는 상태도 바꾸지 않는다. status: "fenced: lost the cluster majority". 리스가 다시 살아나고 `spec.node`가 여전히 이 노드면 다시 켠다.
  - reconciler는 stale로 바뀌면 모든 워커를 깨운다.
  - 펜싱이 꺼진 노드에서 이미 도는 HA VM(설정을 바꾸고 다시 시작한 경우)은 끄지 않는다. status에 그 사실을 남기고, 리더도 그 노드에서는 옮기지 않는다(`status.ha`가 없다).
- **watchdog**(`fencing = watchdog`):
  - 이 노드에서 HA VM을 켜거나 이미 도는 HA VM에 붙을 때 처음 연다(`arm()`). `renewed` 전에는 열지 않는다. 장치를 열면 타이머가 다시 시작되므로, 이전 프로세스가 켜 둔 타이머를 새 리스 없이 늘리면 안 되기 때문이다.
  - 연 뒤에는 `WDIOC_SETTIMEOUT`을 하고 1초마다 쓰다듬는다. 단, 리스가 살아 있거나 이 노드에 도는 HA VM이 없을 때만이다. 워커가 도는 HA VM을 알린다.
  - 닫지 않고 opld가 끝날 때까지 쥔다. 정상 종료 때 도는 HA VM이 없으면 `V`를 쓰고 닫는다(magic close). 있으면 그대로 두어 opld가 watchdog_timeout 안에 돌아오지 않으면 노드가 재시작된다(경고 로그).
  - 장치를 열지 못하면(없음, 다른 프로세스가 쥠 — 예: sanlock `wdmd`) HA VM을 켜지 않는다. status에 오류를 남긴다.
  - 커널이 `nowayout`이면 magic close가 듣지 않는다. 그런 장치는 쓰지 말라고 문서에 적는다.
- **`process`:** watchdog 없이 워커가 끄는 것만 한다. opld가 멈추거나 죽으면 아무것도 끄지 않으므로 안전하지 않다. 시험과 개발에만 쓴다. 등록하는 `NodeHa.fencing`이 `Process`라 상태 표시에 드러난다.

구현(계획 23): `opl-reconciler`의 `ha.rs`. `HaGuard::spawn(renewer: Arc<dyn LeaseRenewer>, HaConfig { fence_after, renew_interval, watchdog })`의 spawn 시각이 기준점이고, 갱신 루프(한 번은 간격의 두 배까지)와 상태 알림·쓰다듬기 루프가 뒤에서 돈다. `state() -> LeaseState { fresh, stale }`, `fresh()`, `stale()`, `subscribe()`(`watch`), `arm()`, `set_running(vm, bool)`, `retain_running(ids)`(그 밖의 VM을 뺀다), `close()`. `arm()`은 **같은 잠금 안에서 지금 fresh인지 확인한다**(`renewed`만이 아니다. 아니면 `ArmError::NotFresh`, 장치를 열지 못하면 `ArmError::Open`, 쓸 수 없는 장치면 `ArmError::Rejected`) — `process`도 리스를 본다. watchdog는 trait(`Watchdog { ping, magic_close, rejected }`, `WatchdogOpener`)이고 장치 구현은 `DeviceWatchdog { path, timeout_secs }`다(rustix로 `O_WRONLY|O_CLOEXEC`, `WDIOC_SETTIMEOUT`, 쓰다듬기는 `write(b"\0")`, Linux가 아니면 열기가 `Unsupported`). `WDIOC_SETTIMEOUT`이 실패하면 `WDIOC_GETTIMEOUT`으로 지금 값을 읽어 요청 이하면 받아들이고, 장치가 요청보다 긴 값을 고르거나 받아들일 수 없으면(`accept_timeout`) **fd를 닫지 않고** "쓸 수 없는 장치"(`rejected()`가 이유)로 돌려준다. 열면 타이머가 돌므로 그냥 닫으면 나중에 노드가 재시작되기 때문이다. guard는 그 장치를 쥔 채 평소 규칙대로 쓰다듬고, `arm()`은 다시 열지 않고 `Rejected`로 실패하며(HA VM을 켜지 않는다), `close()`는 도는 HA VM이 없으면 magic close한다. 처음 열기 자체가 fresh일 때만이므로 이전 프로세스의 타이머를 리스 없이 멈추는 일은 없다. 짧은 값은 경고만 남긴다. `LeaseRenewer`는 `Cluster`가 구현한다. opld는 일반 노드에서 reconciler 앞에서 guard를 만들어 `ReconcilerConfig.ha`에 넣고(`off`·위트니스는 None), 정상 종료 때 reconciler가 멈춘 뒤 `close()`한다.

구현(계획 23, 워커): 켜기 문턱은 `fresh()`+`arm()`이고 멈춘 QEMU를 이어서 실행하는 모든 경로(받은 마이그레이션, 멈춘 채 남은 VM, `postmigrate` 원본)에 같다. 아니면 실패로 세지 않고 `retry_interval` 뒤 다시(status `Starting`, "waiting for the cluster majority before starting an HA VM", 펜싱 뒤면 "fenced: lost the cluster majority", 열기 오류면 그 메시지). 문턱과 실제 시작 사이의 틈을 막으려고 HA VM은 느린 시작 과정(Starting 기록, 디스크 해석·활성화, UEFI 변수 받기) 전에 `set_running(true)`(못 켜면 false로 되돌린다)로 쓰다듬기를 막고, `hv.start`(새로 켜기와 다룰 수 없는 QEMU에 다시 붙는 `reattach`) 바로 앞에서 `fresh()`와 로컬 저장소의 VM을 다시 본다(`ha_recheck`: `spec.node`가 이 노드이고 원하는 상태가 running). 리스가 살아 있으면 그 갱신 인덱스까지 적용되어 있으므로 `process`에서 문턱 뒤 리스를 잃었다 되찾은 사이의 복구 쓰기를 본다. 아니면 켜지 않고(디스크를 끈다) status를 쓰지 않은 채 다시 맞춘다. 저장된 phase가 Running인 채 다시 켤 때 HA VM은 Starting 기록이 실패하면 "restarting anyway"하지 않고 다시 시도하며(과반수가 없으면 계획 24의 리더가 다른 노드에서 켤 수 있다), 기록 뒤 다시 읽은 VM이 다른 노드로 배정됐으면(HA가 아니어도) 켜지 않는다. stale이면 강제 종료(우리가 요청한 종료, 세지 않음, 원하는 상태가 stopped여도). 이미 도는 HA VM에 붙을 때 `arm()`하고 `NotFresh`는 조용히 넘긴다. 배정되지 않은 VM(지워짐·다른 노드로 옮김)과 물러나는 워커는 `hv.inspect`로 QEMU가 없을 때만 목록에서 빼고, 목록에 있던 VM이 stale이면 끈다. reconciler 종료 때는 목록을 그대로 둔다(`close`가 본다). 옮겨 간 HA VM의 원본처럼 워커가 물러난 뒤 멈춘 QEMU는 디스패처가 정리한다: 작업자 없는 VM의 종료 이벤트에 `set_running(false)`, 주기 재동기화 때 하이퍼바이저 목록·작업자에 없는 VM을 `retain_running`으로 뺀다. 낡은 `postmigrate` 원본 판단의 예외(status.node가 이 노드이고 phase가 Running·Migrating·Starting)는 HA 여부를 보지 않는다(리스를 기다리는 동안 HA를 꺼도 원본을 끄지 않는다). 디스패처는 guard의 바뀜 알림에 모든 워커를 깨운다. 펜싱이 꺼진 노드에서 멈춘 HA VM은 phase `Stopped`, "HA is not available on this node"로 켜지 않는다. 마이그레이션 되돌리기(`tasks/migrate.rs`)는 HA VM 원본을 직접 `resume`하지 않고 보류가 풀린 뒤 워커가 문턱을 거쳐 이어서 실행한다. 시험 `opl-reconciler/tests/ha.rs`, `src/ha.rs` 단위 시험(멈춘 시계).

## 4. 복구 (계획 24)

### 4.1 리더의 `HaManager` (opl-api, 리더만)

1초마다:

1. 리더가 아니면 리스 표와 기준점을 잊는다. 리더면 `read_barrier`를 한다.
2. 리더가 된 뒤 처음이면 기준점 b = 지금으로 둔다.
3. 펜싱할 노드를 고른다. 아래를 모두 만족하는 노드 N이다.
   - 이 노드가 아니다.
   - `status.ha`가 있다.
   - now − max(b, r_N) ≥ recover_after_N.
   - `spec.node == N`, `spec.ha`, `spec.power == Running`인 VM이 있다.
4. N마다 리스 표에 펜싱 중 표시를 한다(§4.2). 그 VM들을 이름 순서로 처리한다.
   - **대상 고르기:** 스케줄러로 고른다. 후보는 HA 가능 노드이고, 리스가 살아 있고(now − max(b, r) < recover_after), 유지보수가 아니고, `node_fits`를 통과하며, N과 `status.migration.node`가 아닌 노드다. 받는 쪽 QEMU와 같은 실행 디렉터리를 쓰지 않게 하려는 것이다.
   - **status를 먼저 쓴다**(revision 검사). `phase: Stopped`, `node: None`, `pid: None`, `migration: None`, `last_error: "recovering: node {N} failed"`로 쓰고 `firmware_node`는 그대로 둔다.
   - **그 다음 `spec.node = T`를 쓴다**(revision 검사). 사이에 리더가 바뀌면 새 리더가 다시 한다(아직 `spec.node == N`).
   - 그 VM을 대상으로 하는 N의 끝나지 않은 Task를 실패로 기록한다("node {N} failed; the VM was recovered by HA"). 그러지 않으면 `vm_busy`가 풀리지 않는다.
   - 기록 Task `VmHaRecover { from: N }`(대상 VM, 노드 T)를 끝난 상태(Succeeded)로 만든다. 실행기는 이것을 실행하지 않는다.
   - 대상이 없으면 status(`last_error: "HA cannot find a node for this VM"`)만 쓰고 다음 틱에 다시 본다.
5. 펜싱 중 표시를 지운다.

T의 reconciler는 `spec.node == T`가 되면 평소처럼 새로 켠다(UEFI 변수는 계획 21대로 N에서 받아 보고, 안 되면 T의 파일이나 템플릿).

### 4.2 펜싱 중 표시

리더가 N을 펜싱하기로 정한 뒤 쓰기가 끝나기 전에 N의 갱신이 도착하면 N이 낡은 `spec.node`를 보고 VM을 켤 수 있다. 그래서 리스 표에 `fencing: set<NodeId>`를 두고, 그 사이 N의 갱신을 `Fenced`로 거부한다. 표시를 지운 뒤의 갱신은 복구 쓰기를 포함하는 인덱스를 돌려준다(§3.2). N은 그것을 적용한 뒤에야 fresh가 되므로 옮겨진 VM을 켜지 않는다. 다른 임기에서 쓴 복구도 새 리더의 적용 인덱스에 들어 있다.

구현(계획 23): 펜싱 표시는 `Cluster::begin_fence(node)`·`end_fence(node)`로 리스 표의 펜싱 집합에 넣고 뺀다(API와 시험만, 쓰는 것은 계획 24의 `HaManager`). 펜싱 검사와 갱신 기록이 한 잠금 안이라 `begin_fence` 뒤에는 그 노드의 새 갱신이 기록되지 않는다. 집합은 **임기가 바뀌어도 지우지 않는다**(지우면 이전 임기에 시작한 복구 쓰기가 끝나기 전에 그 노드가 리스를 받을 수 있다). 그래서 `HaManager`는 모든 경로에서 `end_fence`해야 하고, `begin_fence` 뒤 `leases()`를 다시 읽어 아직 만료인지 확인해야 한다.

### 4.3 이미지와 디스크 잠금

죽은 노드가 쥐고 있던 잠금은 곧바로 풀리지 않을 수 있다.

- **공유 LVM(sanlock 임대가 끝날 때까지):** 이미 일시 오류(`PoolUnavailable`)라 세지 않고 다시 시도한다.
- **NFS(QEMU의 OFD 잠금, NFSv4 임대 90초):** QEMU 시작이 `Failed to get "write" lock`으로 실패하면 새 `DriverError::ImageLocked`로 구분한다(QEMU stderr). 워커는 이것을 실패로 세지 않고 `retry_interval`마다 다시 시도한다. status: "disk image is locked by another process; retrying". 이 규칙은 HA가 아닌 VM에도 적용된다. 전에는 곧바로 `Failed`였다. 사용자는 정지로 멈출 수 있다.

### 4.4 그 밖의 경우

- **N이 돌아온 경우:** reconciler는 배정되지 않은 VM을 지금처럼 건드리지 않는다. 펜싱됐다면 그 VM은 이미 꺼져 있다. N의 Task 실행기는 실패로 기록된 Task를 다시 실행하지 않는다.
- **HA 아닌 VM:** 그대로 둔다. 지금처럼 N에 묶여 있다.
- **마이그레이션 중:** N이 원본이면 확정 전에는 `spec.node == N`이고 위대로 복구한다. 받던 노드는 `status.migration`이 지워졌으므로 다음 정리에서 받은 QEMU를 끈다(계획 20 규칙). N이 대상이면 원본의 Task가 실패를 처리한다.

## 5. API·oplctl·웹

- **API**
  - `VmSpec.ha`: 생성·수정에 쓰고 OpenAPI에 보인다.
  - 오류 `ha_unsupported`(409).
  - `NodeSpec.witness`, `NodeStatus.ha`.
  - Task 종류 `vm_ha_recover`.
- **oplctl**
  - `vm create --ha`, `vm set VM --ha|--no-ha`(기존 VM 수정 API).
  - `vm list`·`vm get`에 HA 표시.
  - `node list`·`cluster status`에 역할(위트니스)과 펜싱(`watchdog`·`process`·`-`)을 보인다.
- **웹**
  - VM 만들기·설정에 HA 스위치를 두고, 그 노드가 HA 가능하지 않으면 이유와 함께 막는다.
  - VM 목록과 상세에 HA 표시.
  - 노드 목록에 위트니스 표시와 펜싱 열.
  - Task 목록에 "HA 복구 (노드 {from}에서)".
  - 문구는 한국어와 영어 둘 다 둔다.

구현(계획 23): `GET /cluster`의 멤버에 `fencing`(`watchdog`·`process`, 꺼졌거나 Node가 아직 없으면 null)을 더했다. oplctl `vm set VM --ha|--no-ha`는 VM을 읽어 그 revision으로(`If-Match`) spec 전체를 다시 쓴다(예약 스냅샷 바꾸기와 같은 `put_spec_field`). `vm list`의 HA 열은 `true`/`false`, `vm get`은 JSON의 `spec.ha`다. 웹은 VM 폼의 HA 스위치(`haUnavailable` — 노드가 HA 가능하지 않으면 켜기만 막고 이미 켜진 HA는 끌 수 있다), VM 목록·상세의 HA 배지, 노드·클러스터·대시보드 노드 표의 "펜싱" 열, `errors.ha_unsupported`다. 마이그레이션 대화상자는 HA 불가 대상을 거르지 않는다(서버가 409). `vm_ha_recover`와 Task 문구는 계획 24다.

## 6. 테스트

- **단위**
  - 리스 표: 임기, 펜싱 중 거부, 기준점.
  - `HaGuard`의 fresh·stale 경계. 가짜 시계와 가짜 클러스터를 쓴다.
  - watchdog: 가짜 장치를 쓴다. 쓰다듬기, magic close, 리스가 죽고 HA VM이 돌 때 쓰다듬지 않는지.
  - 워커: 가짜 guard로 HA VM이 리스 없이 켜지지 않는지, stale에서 꺼지고 실패로 세지 않는지, 다시 fresh가 되면 켜지는지, `ImageLocked`를 세지 않는지.
  - `HaManager`: 단일 노드 하네스에 노드를 직접 기록해, 대상 고르기·status/spec 순서·Task 실패와 기록·대상 없음을 본다.
  - 위트니스: Raft 설정, 스케줄러가 빼는지, `cluster status` 경고.
- **클러스터(opl-cluster `memnet`)**
  - 셋 중 하나를 고립시키면 그 노드의 갱신이 실패하고 리더의 표에서 나이가 늘어나는지 본다.
  - 리더를 고립시키면 그 노드 자신도 갱신하지 못하는지 본다.
  - 위트니스(`enable_elect = false`)가 리더가 되지 않는지 본다. 일반 노드 둘과 위트니스에서 리더를 죽이면 남은 일반 노드가 리더가 되는지 본다.
- **실제 프로세스(opld 통합)**
  - **계획 22:** 노드 A, B와 위트니스 W(QEMU 경로가 없는 설정)로 클러스터를 만든다. 경고가 없는지, W가 배치에서 빠지는지(409) 본다. 리더를 `kill -9`하면 남은 일반 노드가 리더가 되고 쓰기가 되는지 본다.
  - **계획 24, 기능 `ha-it`:** A, B는 `fencing = process`, `fence_after_secs = 2`로 둔다. W도 함께 둔다. 공유 디렉터리 풀 위에 HA VM(실제 QEMU, macOS는 TCG, CI는 KVM)과 HA 아닌 VM을 A에서 켠다. A의 opld와 QEMU를 `kill -9`하면 HA VM만 B에서 켜지는지 본다. 기록 Task와, A를 다시 띄웠을 때 VM을 건드리지 않는지도 본다.
  - CI `linux-kvm`에 `opld/ha-it`를 더한다.

구현(계획 22): memnet 시험은 `opl-cluster/tests/witness.rs`(헬퍼 `TestCluster::start_with_witnesses(n, &[ids])`)로, 위트니스가 리더가 되지 않는 것과 일반 노드 둘·위트니스에서 리더를 멈추면 남은 일반 노드가 리더가 되는 것을 본다. 실제 시험은 `crates/opld/tests/cluster.rs`의 `a_witness_votes_but_never_leads_or_runs_vms`다. 위트니스(`Daemon::witness`, 없는 `qemu_binary`)는 API가 없으므로 하네스는 합류·시작 뒤 HTTPS API 대신 제어 소켓의 `state == "member"`를 기다린다. init 거부, API 포트가 닫혔는지, 투표 멤버 셋·경고 없음, `vm create --node wit`의 `node_unavailable`, 리더 `kill -9` 뒤 남은 일반 노드가 리더가 되고 `pool create`가 되는지, 위트니스를 다시 시작하면 온라인 위트니스 멤버로 돌아오는지, 죽은 노드를 `remove-node --force`로 뺀 뒤 일반 노드의 `cluster leave`는 거절되고 위트니스의 탈퇴는 되는지 본다. QEMU가 필요 없어 이 Mac에서도 돈다.

- watchdog 장치를 실제로 쓰는 시험은 CI에 두지 않는다. 터지면 러너가 재시작되고, CI의 `softdog`는 sanlock `wdmd`가 쥐고 있다. 대신 수동 시험 절차를 문서에 남긴다(`softdog soft_noboot=1`).

구현(계획 23): 리스 표 시험은 `opl-cluster/tests/lease.rs`(고립된 노드와 고립된 리더의 갱신 실패, 펜싱 중 거부, 임기가 바뀌면 표가 비는지 등), `HaGuard`는 `ha.rs` 단위 시험(`start_paused`: fence_after 직전·직후, 시작 기준점, 리스 없이 `arm` 실패와 장치를 열지 않는지, 쓰다듬기 규칙, magic close 조건, `accept_timeout`), 워커는 `opl-reconciler/tests/ha.rs`(가짜 guard·MockHypervisor), opld 설정·등록은 `config.rs`·`bootstrap.rs` 단위 시험, API는 `opl-api/tests/vms.rs`·`two_nodes.rs`·`migrate.rs`·`snapshot_tasks.rs`다. 실제 프로세스 시험(`ha-it`)은 계획 24다. 수동 watchdog 시험 절차는 README "HA VM과 자기 펜싱"에 있다. Linux opld(CI 하네스 포함)는 기본으로 watchdog 펜싱을 등록하지만 HA VM이 있을 때만 장치를 연다.

## 7. 알려진 한계

- 펜싱은 노드의 watchdog를 믿는다. `process`는 opld가 멈추면 지켜 주지 못한다. sanlock(공유 LVM)을 쓰는 노드는 `wdmd`가 기본 watchdog를 쥐므로 다른 watchdog 장치가 있어야 한다.
- 노드가 죽고 VM을 다른 노드에서 켜기까지 걸리는 시간은 대략 `fence_after + watchdog_timeout + 5`초(기본 65초)에 부팅 시간을 더한 것이다. 리더도 함께 죽었으면 새 리더가 된 때부터 그만큼이다.
- 리스를 갱신하지 못하는 노드(과반수와 끊김)는 HA VM을 끈다. 짧은 네트워크 장애가 `fence_after`보다 길면 HA VM이 다른 노드에서 다시 부팅된다.
- 죽은 노드에만 있던 UEFI 변수는 잃는다. 다른 노드의 낡은 복사본이나 템플릿으로 켠다(계획 21과 같다).
- RBD는 디스크 잠금이 없어 펜싱만 믿는다. NFSv3에서는 죽은 노드의 잠금이 그 노드가 돌아올 때까지 남을 수 있다. 그동안 VM은 "locked"로 기다린다.
- HA 아닌 VM은 죽은 노드에 묶여 있다(지금과 같다).
- 구현(계획 23): 리더의 복구가 아직 없으므로 펜싱된 HA VM은 그 노드에 남아 있다가 리스가 돌아오면 그 노드에서 다시 켜진다. `recover_after_secs`는 등록만 한다.
- 구현(계획 23): 라이브 마이그레이션이 보류 중인 HA VM은 보류가 풀릴 때까지 강제 종료하지 않는다(`process`에서 긴 마이그레이션이 `fence_after`를 넘길 수 있다. `watchdog`면 쓰다듬기가 멈춰 노드가 재시작된다). 실패한 마이그레이션이 원본을 그냥 `Paused`로 남기면 HA VM은 이어서 실행되지 않는다(드묾).
- 구현(계획 23): 정상 종료 때 도는 HA VM이 있으면 watchdog를 그대로 두므로 opld를 `watchdog_timeout` 안에 다시 띄우지 않으면 노드가 재시작된다.
- 위트니스는 한 클러스터에만 속하고, 위트니스 하나로는 클러스터를 만들 수 없다. 구현: 위트니스는 HTTPS API를 열지 않으므로 그 노드에서는 제어 소켓 명령(`cluster join`·`leave`)만 되고, 상태는 일반 노드의 API로 본다. 역할은 합류할 때 정해지므로 바꾸려면 탈퇴한 뒤 설정을 고치고 opld를 다시 띄워 다시 합류한다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 818개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 321개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: HA 리스와 자기 펜싱 — README HA 켜기·[ha]·watchdog 준비·수동 시험, 설계 구현 메모"
```

---

## 완료 기준 (계획 23)

- VM의 `ha`를 API·oplctl·웹으로 켜고 끌 수 있고, HA 가능 노드가 아니면 409 `ha_unsupported`다.
- 노드는 리스가 살아 있을 때만 HA VM을 켜거나 재개하고, 리스를 잃으면 실패로 세지 않고 강제 종료하며, 되찾으면 다시 켠다.
- 리더는 과반수의 리더임을 확인한 뒤에만 리스를 적고, 펜싱 중인 노드의 갱신을 거부한다.
- watchdog는 실제 갱신 뒤에만 열리고, 리스가 없는데 HA VM이 돌면 쓰다듬지 않으며, 정상 종료 때 HA VM이 없으면 magic close한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **아직 다른 노드로 옮겨 켜지 않는다**(계획 24). 리스를 잃은 노드의 HA VM은 그 노드가 리스를 되찾을 때까지 꺼져 있다.
- **`process` 펜싱은 opld가 멈추면 지켜 주지 못한다**(시험·개발용).
- **HA VM이 도는 노드에서 opld를 멈추면** `watchdog_timeout` 안에 다시 띄우지 않을 때 노드가 재시작된다.
- **마이그레이션 보류 중인 HA VM은 보류가 풀릴 때까지 펜싱하지 않는다**(watchdog가 시간을 지킨다).
- **실패한 마이그레이션이 HA 원본을 그냥 `Paused`로 남기면** 다시 실행되지 않는다(드묾).
- 그 밖의 제한은 설계 문서 §7과 README HA 절에 있다.
