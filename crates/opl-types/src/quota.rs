//! 폴더 할당량 계산(계획 62 설계 §9). 상태 머신(`opl-cluster` `folders.rs`의 `check_quota`)과 API 사전 확인
//! (`opl-api` `quota.rs`)이 같은 규칙을 쓴다: 사용량은 하위 트리 합(할당 기준, 전원과 무관), 할당량 있는 모든 조상을
//! 지키고, 어느 차원이든 **쓰기 뒤 > 상한이고 쓰기 뒤 > 쓰기 전**일 때만 거절한다(늘리는 쓰기만 막는다).

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::folder::{FolderQuota, FolderUsage, MAX_FOLDER_DEPTH};
use crate::numa::EmulatorPolicy;
use crate::vm::VmSpec;

/// 할당량의 차원
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum QuotaDimension {
    Vcpus,
    MemoryMib,
    DiskBytes,
    Vms,
    /// 전용 VM의 vCPU 자리(계획 64)
    DedicatedVcpus,
    /// 라우터 수(계획 66, 네트워크 폴더)
    Routers,
    /// 유동 IP 수(계획 66, 네트워크 폴더)
    FloatingIps,
}

impl QuotaDimension {
    pub const ALL: [QuotaDimension; 7] = [
        QuotaDimension::Vcpus,
        QuotaDimension::MemoryMib,
        QuotaDimension::DiskBytes,
        QuotaDimension::Vms,
        QuotaDimension::DedicatedVcpus,
        QuotaDimension::Routers,
        QuotaDimension::FloatingIps,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            QuotaDimension::Vcpus => "vcpus",
            QuotaDimension::MemoryMib => "memory_mib",
            QuotaDimension::DiskBytes => "disk_bytes",
            QuotaDimension::Vms => "vms",
            QuotaDimension::DedicatedVcpus => "dedicated_vcpus",
            QuotaDimension::Routers => "routers",
            QuotaDimension::FloatingIps => "floating_ips",
        }
    }

    /// 사람이 읽는 단위(`34 of 32 vCPUs`)
    pub fn unit(self) -> &'static str {
        match self {
            QuotaDimension::Vcpus => "vCPUs",
            QuotaDimension::MemoryMib => "MiB of memory",
            QuotaDimension::DiskBytes => "bytes of disk",
            QuotaDimension::Vms => "VMs",
            QuotaDimension::DedicatedVcpus => "dedicated vCPUs",
            QuotaDimension::Routers => "routers",
            QuotaDimension::FloatingIps => "floating IPs",
        }
    }

    pub fn limit(self, quota: &FolderQuota) -> Option<u64> {
        match self {
            QuotaDimension::Vcpus => quota.vcpus,
            QuotaDimension::MemoryMib => quota.memory_mib,
            QuotaDimension::DiskBytes => quota.disk_bytes,
            QuotaDimension::Vms => quota.vms,
            QuotaDimension::DedicatedVcpus => quota.dedicated_vcpus,
            QuotaDimension::Routers => quota.routers,
            QuotaDimension::FloatingIps => quota.floating_ips,
        }
    }

    pub fn used(self, usage: &FolderUsage) -> u64 {
        match self {
            QuotaDimension::Vcpus => usage.vcpus,
            QuotaDimension::MemoryMib => usage.memory_mib,
            QuotaDimension::DiskBytes => usage.disk_bytes,
            QuotaDimension::Vms => usage.vms,
            QuotaDimension::DedicatedVcpus => usage.dedicated_vcpus,
            QuotaDimension::Routers => usage.routers,
            QuotaDimension::FloatingIps => usage.floating_ips,
        }
    }
}

impl std::fmt::Display for QuotaDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FolderUsage {
    /// 더한다(넘치면 상한에서 멈춘다).
    pub fn add(&mut self, other: &FolderUsage) {
        self.vcpus = self.vcpus.saturating_add(other.vcpus);
        self.memory_mib = self.memory_mib.saturating_add(other.memory_mib);
        self.disk_bytes = self.disk_bytes.saturating_add(other.disk_bytes);
        self.vms = self.vms.saturating_add(other.vms);
        self.dedicated_vcpus = self.dedicated_vcpus.saturating_add(other.dedicated_vcpus);
        self.routers = self.routers.saturating_add(other.routers);
        self.floating_ips = self.floating_ips.saturating_add(other.floating_ips);
    }

    /// 어느 차원도 `other`보다 크지 않다.
    pub fn within(&self, other: &FolderUsage) -> bool {
        QuotaDimension::ALL
            .iter()
            .all(|d| d.used(self) <= d.used(other))
    }

    /// `n`배
    pub fn times(&self, n: u64) -> FolderUsage {
        FolderUsage {
            vcpus: self.vcpus.saturating_mul(n),
            memory_mib: self.memory_mib.saturating_mul(n),
            disk_bytes: self.disk_bytes.saturating_mul(n),
            vms: self.vms.saturating_mul(n),
            dedicated_vcpus: self.dedicated_vcpus.saturating_mul(n),
            routers: self.routers.saturating_mul(n),
            floating_ips: self.floating_ips.saturating_mul(n),
        }
    }

    /// 네트워크 폴더에 드는 리소스 하나의 몫(계획 66): 라우터 하나, 유동 IP 하나. 다른 종류는 0이다.
    pub fn of_network_member(kind: crate::resource::Kind) -> FolderUsage {
        use crate::resource::Kind;
        FolderUsage {
            routers: u64::from(kind == Kind::Router),
            floating_ips: u64::from(kind == Kind::FloatingIp),
            ..FolderUsage::default()
        }
    }

    /// VM 하나의 몫: vCPU(`sockets × cores`)·메모리·VM 하나, 그리고 붙은 볼륨의 크기 합 `disk_bytes`. 전용 VM은
    /// vCPU 자리(핫플러그 상한 + 에뮬레이터 `isolate` 1)도 센다. 템플릿은 디스크만 센다.
    pub fn of_vm(spec: &VmSpec, disk_bytes: u64) -> FolderUsage {
        let dedicated = if spec.cpu.policy.is_shared() {
            0
        } else {
            u64::from(spec.cpu.max())
                + u64::from(spec.cpu.emulator == Some(EmulatorPolicy::Isolate))
        };
        vm_usage(
            spec.template,
            u64::from(spec.cpu.sockets),
            u64::from(spec.cpu.cores),
            u64::from(spec.memory_mib),
            disk_bytes,
            dedicated,
        )
    }

    /// [`FolderUsage::of_vm`]의 JSON 판. 상태 머신은 스펙을 JSON으로 보고, 버전이 다른 노드가 같은 답을 내도록 모르는
    /// 칸이 있어도 푼다. `size`는 볼륨 이름 → 크기(없는 볼륨은 0)다.
    pub fn of_vm_value(spec: &Value, mut size: impl FnMut(&str) -> u64) -> FolderUsage {
        let disk_bytes = disk_names(spec).fold(0u64, |sum, name| sum.saturating_add(size(name)));
        let cpu = &spec["cpu"];
        let sockets = cpu["sockets"].as_u64().unwrap_or(1);
        let cores = cpu["cores"].as_u64().unwrap_or(0);
        let dedicated = if cpu["policy"].as_str() == Some("dedicated") {
            cpu["max_vcpus"]
                .as_u64()
                .unwrap_or(sockets.saturating_mul(cores))
                .saturating_add(u64::from(cpu["emulator"].as_str() == Some("isolate")))
        } else {
            0
        };
        vm_usage(
            spec["template"].as_bool().unwrap_or(false),
            sockets,
            cores,
            spec["memory_mib"].as_u64().unwrap_or(0),
            disk_bytes,
            dedicated,
        )
    }
}

fn vm_usage(
    template: bool,
    sockets: u64,
    cores: u64,
    memory: u64,
    disk_bytes: u64,
    dedicated_vcpus: u64,
) -> FolderUsage {
    if template {
        return FolderUsage {
            disk_bytes,
            ..FolderUsage::default()
        };
    }
    FolderUsage {
        vcpus: sockets.saturating_mul(cores),
        memory_mib: memory,
        disk_bytes,
        vms: 1,
        dedicated_vcpus,
        ..FolderUsage::default()
    }
}

/// VM 스펙(JSON)의 디스크 볼륨 이름
pub fn disk_names(spec: &Value) -> impl Iterator<Item = &str> {
    spec["disks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d["volume"].as_str())
}

/// 할당량 계산이 보는 폴더 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaNode {
    pub parent: Option<Uuid>,
    pub quota: Option<FolderQuota>,
}

/// 폴더 id → 트리 정보
pub type QuotaTree = HashMap<Uuid, QuotaNode>;
/// 폴더 id → 그 폴더에 바로 든 VM의 사용량 합(자손 폴더 제외)
pub type DirectUsage = HashMap<Uuid, FolderUsage>;

/// `folder`와 그 조상(자기 먼저). 없는 폴더·순환·깊이 상한에서 멈춘다.
pub fn ancestors(tree: &QuotaTree, folder: Option<Uuid>) -> Vec<Uuid> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut cur = folder;
    while let Some(id) = cur {
        let Some(node) = tree.get(&id) else { break };
        if !seen.insert(id) || out.len() > MAX_FOLDER_DEPTH {
            break;
        }
        out.push(id);
        cur = node.parent;
    }
    out
}

/// `folder`와 그 조상 가운데 할당량이 있는 폴더(자기 먼저)
pub fn quota_ancestors(tree: &QuotaTree, folder: Option<Uuid>) -> Vec<Uuid> {
    ancestors(tree, folder)
        .into_iter()
        .filter(|id| tree[id].quota.is_some_and(|q| !q.is_empty()))
        .collect()
}

/// 폴더 `folder`의 하위 트리 사용량(할당량과 관계없이)
pub fn subtree_usage(tree: &QuotaTree, direct: &DirectUsage, folder: Uuid) -> FolderUsage {
    let mut sum = FolderUsage::default();
    for (f, usage) in direct {
        if ancestors(tree, Some(*f)).contains(&folder) {
            sum.add(usage);
        }
    }
    sum
}

/// 할당량이 있는 폴더마다 하위 트리 사용량
pub fn quota_usage(tree: &QuotaTree, direct: &DirectUsage) -> BTreeMap<Uuid, FolderUsage> {
    let mut out: BTreeMap<Uuid, FolderUsage> = tree
        .iter()
        .filter(|(_, n)| n.quota.is_some_and(|q| !q.is_empty()))
        .map(|(id, _)| (*id, FolderUsage::default()))
        .collect();
    for (folder, usage) in direct {
        for q in quota_ancestors(tree, Some(*folder)) {
            if let Some(sum) = out.get_mut(&q) {
                sum.add(usage);
            }
        }
    }
    out
}

/// 할당량을 넘는 쓰기
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaExceeded {
    pub folder: Uuid,
    pub dimension: QuotaDimension,
    pub limit: u64,
    pub used: u64,
}

/// 쓰기 전·뒤의 트리와 사용량을 견줘, 쓰기 뒤 상한을 넘고 쓰기 전보다 늘어난 첫 (폴더, 차원). 폴더 id 차례로 보므로
/// 모든 노드가 같은 답을 낸다.
pub fn quota_exceeded(
    before: (&QuotaTree, &DirectUsage),
    after: (&QuotaTree, &DirectUsage),
) -> Option<QuotaExceeded> {
    let was = quota_usage(before.0, before.1);
    for (folder, used) in quota_usage(after.0, after.1) {
        let Some(quota) = after.0.get(&folder).and_then(|n| n.quota) else {
            continue;
        };
        let prev = was.get(&folder).copied().unwrap_or_default();
        for d in QuotaDimension::ALL {
            if let Some(limit) = d.limit(&quota)
                && d.used(&used) > limit
                && d.used(&used) > d.used(&prev)
            {
                return Some(QuotaExceeded {
                    folder,
                    dimension: d,
                    limit,
                    used: d.used(&used),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{DiskBus, DiskSpec};
    use serde_json::json;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn node(parent: Option<Uuid>, quota: Option<FolderQuota>) -> QuotaNode {
        QuotaNode { parent, quota }
    }

    fn vcpus(n: u64) -> Option<FolderQuota> {
        Some(FolderQuota {
            vcpus: Some(n),
            ..FolderQuota::default()
        })
    }

    fn cpu(n: u64) -> FolderUsage {
        FolderUsage {
            vcpus: n,
            ..FolderUsage::default()
        }
    }

    #[test]
    fn vm_usage_counts_allocation_and_templates_count_disks_only() {
        let mut spec: VmSpec = serde_json::from_value(json!({
            "cpu": {"sockets": 2, "cores": 3}, "memory_mib": 1024, "machine": "q35",
            "disks": [{"volume": "a", "bus": "virtio-blk"}, {"volume": "b", "bus": "virtio-blk"}]
        }))
        .unwrap();
        let size = |n: &str| if n == "a" { 10 } else { 5 };
        let want = FolderUsage {
            routers: 0,
            floating_ips: 0,
            dedicated_vcpus: 0,
            vcpus: 6,
            memory_mib: 1024,
            disk_bytes: 15,
            vms: 1,
        };
        assert_eq!(FolderUsage::of_vm(&spec, 15), want);
        // JSON 판은 같은 답이고 모르는 칸이 있어도 푼다.
        let mut value = serde_json::to_value(&spec).unwrap();
        assert_eq!(FolderUsage::of_vm_value(&value, size), want);
        value["future_field"] = json!(1);
        assert_eq!(FolderUsage::of_vm_value(&value, size), want);
        spec.template = true;
        spec.disks = vec![DiskSpec {
            volume: "a".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        }];
        let value = serde_json::to_value(&spec).unwrap();
        let disk_only = FolderUsage {
            disk_bytes: 10,
            ..FolderUsage::default()
        };
        assert_eq!(FolderUsage::of_vm(&spec, 10), disk_only);
        assert_eq!(FolderUsage::of_vm_value(&value, size), disk_only);
    }

    /// 사용량은 하위 트리 합이고, 할당량 있는 모든 조상을 지킨다.
    #[test]
    fn nested_quotas_all_hold() {
        // 1(vcpus 10) ⊃ 2(할당량 없음) ⊃ 3(vcpus 100)
        let tree: QuotaTree = [
            (id(1), node(None, vcpus(10))),
            (id(2), node(Some(id(1)), None)),
            (id(3), node(Some(id(2)), vcpus(100))),
        ]
        .into();
        assert_eq!(quota_ancestors(&tree, Some(id(3))), vec![id(3), id(1)]);
        assert_eq!(quota_ancestors(&tree, None), Vec::<Uuid>::new());
        let before: DirectUsage = [(id(2), cpu(4)), (id(3), cpu(4))].into();
        let usage = quota_usage(&tree, &before);
        assert_eq!(usage[&id(1)].vcpus, 8);
        assert_eq!(usage[&id(3)].vcpus, 4);
        // 3에 4를 더 넣으면 3은 넉넉하지만 1(10)을 넘는다.
        let after: DirectUsage = [(id(2), cpu(4)), (id(3), cpu(8))].into();
        assert_eq!(
            quota_exceeded((&tree, &before), (&tree, &after)),
            Some(QuotaExceeded {
                folder: id(1),
                dimension: QuotaDimension::Vcpus,
                limit: 10,
                used: 12
            })
        );
        let after: DirectUsage = [(id(2), cpu(4)), (id(3), cpu(6))].into();
        assert_eq!(quota_exceeded((&tree, &before), (&tree, &after)), None);
    }

    /// 이미 넘은 폴더도 늘리지 않는 쓰기(줄이기, 그대로)는 받는다.
    #[test]
    fn only_growth_is_refused() {
        let tree: QuotaTree = [(id(1), node(None, vcpus(2)))].into();
        let before: DirectUsage = [(id(1), cpu(8))].into();
        for n in [8, 5] {
            let after: DirectUsage = [(id(1), cpu(n))].into();
            assert_eq!(quota_exceeded((&tree, &before), (&tree, &after)), None);
        }
        let after: DirectUsage = [(id(1), cpu(9))].into();
        assert!(quota_exceeded((&tree, &before), (&tree, &after)).is_some());
    }

    /// 폴더를 할당량 있는 폴더 아래로 옮기면 그 하위 트리 사용량이 더해진다.
    #[test]
    fn moving_a_folder_counts_its_subtree() {
        let before: QuotaTree = [
            (id(1), node(None, vcpus(4))),
            (id(2), node(None, None)),
            (id(3), node(Some(id(2)), None)),
        ]
        .into();
        let mut after = before.clone();
        after.insert(id(2), node(Some(id(1)), None));
        let direct: DirectUsage = [(id(1), cpu(2)), (id(3), cpu(3))].into();
        assert_eq!(
            quota_exceeded((&before, &direct), (&after, &direct)).map(|e| e.used),
            Some(5)
        );
    }

    #[test]
    fn ancestor_walk_stops_on_cycles_and_missing_folders() {
        let tree: QuotaTree = [
            (id(1), node(Some(id(2)), vcpus(1))),
            (id(2), node(Some(id(1)), None)),
            (id(3), node(Some(id(99)), vcpus(1))),
        ]
        .into();
        assert_eq!(quota_ancestors(&tree, Some(id(2))), vec![id(1)]);
        assert_eq!(quota_ancestors(&tree, Some(id(3))), vec![id(3)]);
        assert_eq!(quota_ancestors(&tree, Some(id(99))), Vec::<Uuid>::new());
        assert_eq!(ancestors(&tree, Some(id(2))), vec![id(2), id(1)]);
        let direct: DirectUsage = [(id(2), cpu(1)), (id(3), cpu(2))].into();
        assert_eq!(subtree_usage(&tree, &direct, id(1)).vcpus, 1);
        assert_eq!(subtree_usage(&tree, &direct, id(3)).vcpus, 2);
    }

    #[test]
    fn empty_quotas_bind_nothing() {
        let tree: QuotaTree = [(id(1), node(None, Some(FolderQuota::default())))].into();
        assert!(quota_ancestors(&tree, Some(id(1))).is_empty());
        let after: DirectUsage = [(id(1), cpu(100))].into();
        assert_eq!(
            quota_exceeded((&tree, &DirectUsage::new()), (&tree, &after)),
            None
        );
    }

    #[test]
    fn dimension_names() {
        for d in QuotaDimension::ALL {
            assert_eq!(serde_json::to_value(d).unwrap(), d.as_str());
        }
        let u = FolderUsage {
            dedicated_vcpus: 0,
            vcpus: 1,
            memory_mib: 2,
            disk_bytes: 3,
            vms: 4,
            routers: 5,
            floating_ips: 6,
        };
        assert!(u.within(&u.times(2)));
        assert!(!u.times(2).within(&u));
        // 라우터 수만 늘어도 넘는다(계획 66).
        let more = FolderUsage { routers: 6, ..u };
        assert!(!more.within(&u));
    }

    /// 계획 66: 네트워크 폴더의 라우터·유동 IP 수
    #[test]
    fn network_members_count_routers_and_floating_ips() {
        use crate::resource::Kind;
        assert_eq!(FolderUsage::of_network_member(Kind::Router).routers, 1);
        assert_eq!(
            FolderUsage::of_network_member(Kind::FloatingIp).floating_ips,
            1
        );
        assert_eq!(
            FolderUsage::of_network_member(Kind::Network),
            FolderUsage::default()
        );
        let quota = FolderQuota {
            floating_ips: Some(1),
            ..FolderQuota::default()
        };
        let tree: QuotaTree = [(id(1), node(None, Some(quota)))].into();
        let one: DirectUsage = [(id(1), FolderUsage::of_network_member(Kind::FloatingIp))].into();
        let mut two = one.clone();
        two.get_mut(&id(1))
            .unwrap()
            .add(&FolderUsage::of_network_member(Kind::FloatingIp));
        assert_eq!(
            quota_exceeded((&tree, &DirectUsage::new()), (&tree, &one)),
            None
        );
        assert_eq!(
            quota_exceeded((&tree, &one), (&tree, &two)).map(|e| (e.dimension, e.limit, e.used)),
            Some((QuotaDimension::FloatingIps, 1, 2))
        );
    }

    #[test]
    fn dedicated_vcpus_count_slots_and_isolated_emulator() {
        let spec = |cpu: serde_json::Value| -> VmSpec {
            serde_json::from_value(json!({ "cpu": cpu, "memory_mib": 1024, "machine": "q35" }))
                .unwrap()
        };
        for (cpu, want) in [
            (json!({ "sockets": 2, "cores": 2 }), 0),
            (
                json!({ "sockets": 2, "cores": 2, "policy": "dedicated" }),
                4,
            ),
            (
                json!({ "sockets": 2, "cores": 2, "max_vcpus": 8, "policy": "dedicated", "emulator": "isolate" }),
                9,
            ),
        ] {
            let s = spec(cpu.clone());
            assert_eq!(FolderUsage::of_vm(&s, 0).dedicated_vcpus, want, "{cpu}");
            let v = serde_json::to_value(&s).unwrap();
            assert_eq!(
                FolderUsage::of_vm_value(&v, |_| 0),
                FolderUsage::of_vm(&s, 0)
            );
        }
        let mut t = spec(json!({ "cores": 2, "policy": "dedicated" }));
        t.template = true;
        assert_eq!(FolderUsage::of_vm(&t, 0).dedicated_vcpus, 0);
        assert_eq!(QuotaDimension::DedicatedVcpus.as_str(), "dedicated_vcpus");
        let quota = FolderQuota {
            dedicated_vcpus: Some(4),
            ..FolderQuota::default()
        };
        assert_eq!(QuotaDimension::DedicatedVcpus.limit(&quota), Some(4));
    }
}
