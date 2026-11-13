//! 배치 규칙과 HA 그룹의 계산(설계 §8). 처음 놓기, HA 복구, 비우기(그리고 계획 53의 균형 조정기)가 같은
//! 조건·점수를 쓴다.
//!
//! "자리 잡은 VM"은 `spec.power == Running`이고 `spec.node`가 있는 VM이다. 마이그레이션 중이면 원본과 대상
//! (`status.migration.node`) 둘 다 차지한다. [`Assign`]이 이것을 덮어쓴다(복구 중 옮긴 VM, 함께 옮기는 묶음).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use opl_types::node::Node;
use opl_types::placement::{HaGroup, PlacementRule, RuleKind};
use opl_types::vm::{PowerState, Vm};
use uuid::Uuid;

use crate::scheduler::{Inventory, node_fits};

/// 자리 덮어쓰기: VM id → 그 VM이 차지한 노드(None이면 자리 잡지 않은 것으로 본다)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assign(BTreeMap<Uuid, Option<Uuid>>);

impl Assign {
    /// 이 VM이 이 노드 하나에 자리 잡은 것으로 본다.
    pub fn place(&mut self, vm: Uuid, node: Uuid) {
        self.0.insert(vm, Some(node));
    }

    /// 이 VM을 자리 잡지 않은 것으로 본다.
    pub fn unplace(&mut self, vm: Uuid) {
        self.0.insert(vm, None);
    }
}

/// 이 VM이 차지한 노드
fn occupied(vm: &Vm, assign: &Assign) -> Vec<Uuid> {
    if let Some(node) = assign.0.get(&vm.id) {
        return node.iter().copied().collect();
    }
    let Some(node) = vm
        .spec
        .node
        .filter(|_| vm.spec.power == PowerState::Running)
    else {
        return Vec::new();
    };
    let mut nodes = vec![node];
    if let Some(m) = &vm.status.migration
        && m.node != node
    {
        nodes.push(m.node);
    }
    nodes
}

/// 놓을 수 있는 노드 하나와 그 점수
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub node: Uuid,
    /// HA 그룹에서 이 노드의 우선순위(그룹 VM만). 그룹 밖 노드와 그룹 없는 VM은 None(가장 낮음)이다.
    pub priority: Option<u32>,
    /// soft 규칙 벌점의 합
    pub penalty: u32,
    /// 실행하기로 된 다른 VM에 배정되지 않은 메모리. 계획 53이 부하 점수로 바꾼다.
    pub free_mib: i64,
}

/// 나은 후보가 앞(`Less`): 우선순위 높은 것 → 벌점 적은 것 → 메모리 많은 것 → id 작은 것
pub fn compare(a: &Candidate, b: &Candidate) -> Ordering {
    b.priority
        .cmp(&a.priority)
        .then(a.penalty.cmp(&b.penalty))
        .then(b.free_mib.cmp(&a.free_mib))
        .then(a.node.cmp(&b.node))
}

/// 가장 나은 후보
pub fn best(candidates: &[Candidate]) -> Option<Candidate> {
    candidates.iter().copied().min_by(compare)
}

/// 노드 조건([`node_fits`])에 맞고 hard 규칙을 어기지 않는 노드들(나은 것부터)
pub fn candidates(vm: &Vm, inv: &Inventory, assign: &Assign) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = inv
        .nodes
        .iter()
        .filter(|n| node_fits(&vm.spec, inv, n).is_ok())
        .filter(|n| hard_violations(vm, n, inv, assign).is_empty())
        .map(|n| score(vm, n, inv, assign))
        .collect();
    found.sort_by(compare);
    found
}

/// VM을 이 노드에 둘 때의 점수(노드 조건과 hard 규칙은 보지 않는다). 비우기가 묶음의 대상을 고를 때도 쓴다.
pub fn score(vm: &Vm, node: &Node, inv: &Inventory, assign: &Assign) -> Candidate {
    Candidate {
        node: node.id,
        priority: group(vm, inv).and_then(|g| g.spec.priority(&node.name)),
        penalty: soft(vm, node, inv, assign).0,
        free_mib: free_mib(vm, node, inv, assign),
    }
}

/// VM을 이 노드에 두면 어기는 hard 규칙의 이름(이름 순). restricted HA 그룹 밖이면 `ha-group:<이름>`.
pub fn hard_violations(vm: &Vm, node: &Node, inv: &Inventory, assign: &Assign) -> Vec<String> {
    let mut names = BTreeSet::new();
    for rule in rules_of(vm, inv).filter(|r| r.spec.kind.is_hard()) {
        let broken = match rule.spec.kind {
            RuleKind::Affinity => others(vm, rule, inv, assign).any(|n| n != node.id),
            RuleKind::AntiAffinity => others(vm, rule, inv, assign).any(|n| n == node.id),
            RuleKind::NodeAffinity => !rule.spec.has_node(&node.name, &node.labels),
            RuleKind::NodeAntiAffinity => rule.spec.has_node(&node.name, &node.labels),
            _ => false,
        };
        if broken {
            names.insert(rule.name.clone());
        }
    }
    if let Some(g) = group(vm, inv)
        && g.spec.restricted
        && g.spec.priority(&node.name).is_none()
    {
        names.insert(format!("ha-group:{}", g.name));
    }
    names.into_iter().collect()
}

/// VM을 이 노드에 두면 벌점이 생기는 soft 규칙의 이름(이름 순)
pub fn soft_violations(vm: &Vm, node: &Node, inv: &Inventory, assign: &Assign) -> Vec<String> {
    soft(vm, node, inv, assign).1
}

/// soft 벌점의 합과 벌점이 생긴 규칙 이름
fn soft(vm: &Vm, node: &Node, inv: &Inventory, assign: &Assign) -> (u32, Vec<String>) {
    let mut total = 0;
    let mut names = BTreeSet::new();
    for rule in rules_of(vm, inv).filter(|r| !r.spec.kind.is_hard()) {
        let count = |on: bool| {
            others(vm, rule, inv, assign)
                .filter(|n| (*n == node.id) == on)
                .count()
        };
        let units = match rule.spec.kind {
            RuleKind::SoftAffinity => count(false),
            RuleKind::SoftAntiAffinity => count(true),
            RuleKind::SoftNodeAffinity => {
                usize::from(!rule.spec.has_node(&node.name, &node.labels))
            }
            RuleKind::SoftNodeAntiAffinity => {
                usize::from(rule.spec.has_node(&node.name, &node.labels))
            }
            _ => 0,
        };
        if units > 0 {
            total += rule.spec.weight() * units as u32;
            names.insert(rule.name.clone());
        }
    }
    (total, names.into_iter().collect())
}

/// 켜진 hard `affinity` 규칙으로 이 VM과 묶인 다른 VM(바로 이웃만, 자리와 상관없이)
pub fn affinity_partners<'a>(vm: &Vm, inv: &'a Inventory) -> Vec<&'a Vm> {
    let rules: Vec<&PlacementRule> = rules_of(vm, inv)
        .filter(|r| r.spec.kind == RuleKind::Affinity)
        .collect();
    inv.vms
        .iter()
        .filter(|v| v.id != vm.id)
        .filter(|v| rules.iter().any(|r| r.spec.has_vm(&v.name, &v.labels)))
        .collect()
}

/// 이 VM이 든 켜진 규칙
fn rules_of<'a>(vm: &'a Vm, inv: &'a Inventory) -> impl Iterator<Item = &'a PlacementRule> {
    inv.rules
        .iter()
        .filter(move |r| r.spec.enabled && r.spec.has_vm(&vm.name, &vm.labels))
}

/// 규칙의 다른 구성원이 차지한 노드(구성원이 마이그레이션 중이면 둘)
fn others<'a>(
    vm: &'a Vm,
    rule: &'a PlacementRule,
    inv: &'a Inventory,
    assign: &'a Assign,
) -> impl Iterator<Item = Uuid> + 'a {
    inv.vms
        .iter()
        .filter(move |v| v.id != vm.id && rule.spec.has_vm(&v.name, &v.labels))
        .flat_map(move |v| occupied(v, assign))
}

/// HA 활성 VM의 HA 그룹
fn group<'a>(vm: &Vm, inv: &'a Inventory) -> Option<&'a HaGroup> {
    if !vm.spec.ha_active() {
        return None;
    }
    let name = vm.spec.ha.as_ref()?.group.as_ref()?;
    inv.groups.iter().find(|g| &g.name == name)
}

/// [`Inventory::free_memory_mib`]과 같되 [`Assign`]의 자리를 따른다.
fn free_mib(vm: &Vm, node: &Node, inv: &Inventory, assign: &Assign) -> i64 {
    let used: i64 = inv
        .vms
        .iter()
        .filter(|v| v.id != vm.id)
        .filter(|v| match assign.0.get(&v.id) {
            Some(placed) => *placed == Some(node.id),
            None => v.spec.node == Some(node.id) && v.spec.power == PowerState::Running,
        })
        .map(|v| i64::from(v.spec.memory_mib))
        .sum();
    node.status.memory_mib as i64 - used
}

/// 켜진 hard 규칙끼리의 모순(태그는 지금의 VM·노드 레이블로 푼다). 모순이 있으면 관련된 규칙 이름(이름
/// 순)이다:
/// (a) 같은 두 VM이 `affinity` 묶음(추이적)과 `anti-affinity`에 함께 있음,
/// (b) 한 `affinity` 묶음(규칙이 없는 VM은 혼자)의 허용 노드(구성원들의 `node-affinity` 교집합 −
/// `node-anti-affinity`, 위트니스 제외)가 비어 있음.
pub fn conflicts(rules: &[PlacementRule], inv: &Inventory) -> Result<(), Vec<String>> {
    let hard: Vec<&PlacementRule> = rules
        .iter()
        .filter(|r| r.spec.enabled && r.spec.kind.is_hard())
        .collect();
    let vms_of = |r: &PlacementRule| -> BTreeSet<String> {
        match &r.spec.vm_tag {
            Some(_) => inv
                .vms
                .iter()
                .filter(|v| r.spec.has_vm(&v.name, &v.labels))
                .map(|v| v.name.clone())
                .collect(),
            None => r.spec.vms.iter().cloned().collect(),
        }
    };
    let members: Vec<(&PlacementRule, BTreeSet<String>)> =
        hard.iter().map(|r| (*r, vms_of(r))).collect();

    // affinity 묶음: 이름 → 대표(union-find)
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    fn find(parent: &mut BTreeMap<String, String>, x: &str) -> String {
        let mut root = x.to_string();
        while let Some(p) = parent.get(&root).filter(|p| **p != root) {
            root = p.clone();
        }
        parent.insert(x.to_string(), root.clone());
        root
    }
    for (_, vms) in members
        .iter()
        .filter(|(r, _)| r.spec.kind == RuleKind::Affinity)
    {
        let mut it = vms.iter();
        if let Some(first) = it.next() {
            let root = find(&mut parent, first);
            for vm in it {
                let other = find(&mut parent, vm);
                parent.insert(other, root.clone());
            }
        }
    }
    // 묶음마다 그 묶음을 만든 affinity 규칙
    let mut joined: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (rule, vms) in members
        .iter()
        .filter(|(r, _)| r.spec.kind == RuleKind::Affinity)
    {
        if let Some(vm) = vms.iter().next() {
            let root = find(&mut parent, vm);
            joined.entry(root).or_default().insert(rule.name.clone());
        }
    }

    let mut bad = BTreeSet::new();
    // (a)
    for (rule, vms) in members
        .iter()
        .filter(|(r, _)| r.spec.kind == RuleKind::AntiAffinity)
    {
        let mut seen: BTreeMap<String, &str> = BTreeMap::new();
        for vm in vms {
            let root = find(&mut parent, vm);
            if seen.insert(root.clone(), vm).is_some() {
                bad.insert(rule.name.clone());
                bad.extend(joined.get(&root).into_iter().flatten().cloned());
            }
        }
    }
    // (b)
    let universe: BTreeSet<&str> = inv
        .nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .map(|n| n.name.as_str())
        .collect();
    let nodes_of = |r: &PlacementRule| -> BTreeSet<&str> {
        inv.nodes
            .iter()
            .filter(|n| r.spec.has_node(&n.name, &n.labels))
            .map(|n| n.name.as_str())
            .collect()
    };
    let mut allowed: BTreeMap<String, (BTreeSet<&str>, BTreeSet<String>)> = BTreeMap::new();
    for (rule, vms) in members.iter().filter(|(r, _)| !r.spec.kind.is_vm_rule()) {
        let roots: BTreeSet<String> = vms.iter().map(|vm| find(&mut parent, vm)).collect();
        for root in roots {
            let (nodes, why) = allowed
                .entry(root)
                .or_insert_with(|| (universe.clone(), BTreeSet::new()));
            let set = nodes_of(rule);
            if rule.spec.kind == RuleKind::NodeAffinity {
                nodes.retain(|n| set.contains(n));
            } else {
                nodes.retain(|n| !set.contains(n));
            }
            why.insert(rule.name.clone());
        }
    }
    for (root, (nodes, why)) in allowed {
        if nodes.is_empty() {
            bad.extend(why);
            bad.extend(joined.get(&root).into_iter().flatten().cloned());
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
    use opl_types::placement::{HaGroupNode, HaGroupSpec, PlacementRuleSpec};
    use opl_types::resource::Resource;
    use opl_types::vm::{
        Arch, CpuSpec, HaState, MachineType, MigrationStatus, VmHa, VmSpec, VmStatus,
    };

    use super::*;

    fn node(name: &str, memory_mib: u64, labels: &[(&str, &str)]) -> Node {
        let mut n = Resource::new(
            name,
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.labels = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        n.status = NodeStatus {
            online: true,
            memory_mib,
            arch: Some(Arch::Aarch64),
            // HA VM(그룹 시험)도 받는다.
            ha: Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
                fence_after_secs: None,
            }),
            ..Default::default()
        };
        n
    }

    fn vm(name: &str, node: Option<&Node>, power: PowerState) -> Vm {
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
                power,
                restart: Default::default(),
                node: node.map(|n| n.id),
                snapshot_schedule: None,
                ha: None,
            },
        )
    }

    fn running(name: &str, node: &Node) -> Vm {
        vm(name, Some(node), PowerState::Running)
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

    fn group(name: &str, nodes: &[(&str, u32)], restricted: bool) -> HaGroup {
        Resource::new(
            name,
            HaGroupSpec {
                nodes: nodes
                    .iter()
                    .map(|(n, p)| HaGroupNode {
                        node: n.to_string(),
                        priority: *p,
                    })
                    .collect(),
                restricted,
                nofailback: false,
            },
        )
    }

    fn in_group(mut vm: Vm, group: &str) -> Vm {
        vm.spec.ha = Some(VmHa {
            group: Some(group.into()),
            ..Default::default()
        });
        vm
    }

    fn none() -> Assign {
        Assign::default()
    }

    #[test]
    fn hard_rules_of_each_kind() {
        let (n1, n2, n3) = (
            node("n1", 8192, &[]),
            node("n2", 8192, &[]),
            node("n3", 8192, &[]),
        );
        let me = vm("me", None, PowerState::Running);
        let mut inv = Inventory {
            nodes: vec![n1.clone(), n2.clone(), n3.clone()],
            vms: vec![running("web", &n1)],
            ..Default::default()
        };
        let hard = |inv: &Inventory, n: &Node| hard_violations(&me, n, inv, &none());

        inv.rules = vec![rule("together", RuleKind::Affinity, &["me", "web"], &[])];
        assert!(hard(&inv, &n1).is_empty());
        assert_eq!(hard(&inv, &n2), ["together"]);

        inv.rules = vec![rule("apart", RuleKind::AntiAffinity, &["me", "web"], &[])];
        assert_eq!(hard(&inv, &n1), ["apart"]);
        assert!(hard(&inv, &n2).is_empty());

        inv.rules = vec![rule("only", RuleKind::NodeAffinity, &["me"], &["n2", "n3"])];
        assert_eq!(hard(&inv, &n1), ["only"]);
        assert!(hard(&inv, &n3).is_empty());

        inv.rules = vec![rule("never", RuleKind::NodeAntiAffinity, &["me"], &["n2"])];
        assert!(hard(&inv, &n1).is_empty());
        assert_eq!(hard(&inv, &n2), ["never"]);
        assert_eq!(
            candidates(&me, &inv, &none())
                .iter()
                .map(|c| c.node)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([n1.id, n3.id])
        );

        // 규칙에 들지 않은 VM은 상관없다. soft 규칙은 hard 위반이 아니다.
        let other = vm("other", None, PowerState::Running);
        assert!(hard_violations(&other, &n2, &inv, &none()).is_empty());
        inv.rules = vec![rule(
            "pref",
            RuleKind::SoftNodeAntiAffinity,
            &["me"],
            &["n2"],
        )];
        assert!(hard(&inv, &n2).is_empty());
    }

    #[test]
    fn soft_penalties_use_weight_and_member_counts() {
        let (n1, n2) = (node("n1", 8192, &[]), node("n2", 8192, &[]));
        let me = vm("me", None, PowerState::Running);
        let mut inv = Inventory {
            nodes: vec![n1.clone(), n2.clone()],
            vms: vec![running("a", &n1), running("b", &n1), running("c", &n2)],
            ..Default::default()
        };
        let pen = |inv: &Inventory, n: &Node| soft(&me, n, inv, &none()).0;

        let mut near = rule("near", RuleKind::SoftAffinity, &["me", "a", "b", "c"], &[]);
        near.spec.weight = Some(10);
        inv.rules = vec![near];
        // N 밖의 구성원 수 × weight
        assert_eq!(pen(&inv, &n1), 10);
        assert_eq!(pen(&inv, &n2), 20);
        assert_eq!(soft_violations(&me, &n1, &inv, &none()), ["near"]);

        // N의 구성원 수 × weight(없으면 50)
        inv.rules = vec![rule(
            "spread",
            RuleKind::SoftAntiAffinity,
            &["me", "a", "b", "c"],
            &[],
        )];
        assert_eq!(pen(&inv, &n1), 100);
        assert_eq!(pen(&inv, &n2), 50);

        let mut pref = rule("pref", RuleKind::SoftNodeAffinity, &["me"], &["n2"]);
        pref.spec.weight = Some(7);
        let mut avoid = rule("avoid", RuleKind::SoftNodeAntiAffinity, &["me"], &["n2"]);
        avoid.spec.weight = Some(3);
        inv.rules = vec![pref, avoid];
        assert_eq!(pen(&inv, &n1), 7);
        assert_eq!(pen(&inv, &n2), 3);
        assert_eq!(soft_violations(&me, &n2, &inv, &none()), ["avoid"]);
        // soft 규칙은 후보를 빼지 않고 벌점으로 순서를 정한다.
        let found = candidates(&me, &inv, &none());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].node, n2.id);
        assert_eq!(found[0].penalty, 3);
    }

    #[test]
    fn tags_pick_vms_by_label_and_nodes_by_zone() {
        let a = node("n1", 8192, &[("zone", "rack-a")]);
        let b = node("n2", 8192, &[("zone", "rack-b")]);
        let mut me = vm("me", None, PowerState::Running);
        me.labels.insert("app".into(), "db".into());
        let mut peer = running("peer", &a);
        peer.labels.insert("app".into(), "db".into());
        let mut zone = rule("in-a", RuleKind::NodeAffinity, &[], &[]);
        zone.spec.vm_tag = Some("app=db".into());
        zone.spec.node_tag = Some("zone=rack-b".into());
        let mut apart = rule("apart", RuleKind::AntiAffinity, &[], &[]);
        apart.spec.vm_tag = Some("app=db".into());
        let inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            vms: vec![peer, running("plain", &b)],
            rules: vec![zone, apart],
            ..Default::default()
        };
        assert_eq!(hard_violations(&me, &a, &inv, &none()), ["apart", "in-a"]);
        assert!(hard_violations(&me, &b, &inv, &none()).is_empty());
        // 레이블이 없는 VM은 태그 규칙에 들지 않는다.
        let plain = vm("plain2", None, PowerState::Running);
        assert!(hard_violations(&plain, &a, &inv, &none()).is_empty());
    }

    #[test]
    fn stopped_vms_do_not_count_and_migrating_vms_hold_both_nodes() {
        let (n1, n2, n3) = (
            node("n1", 8192, &[]),
            node("n2", 8192, &[]),
            node("n3", 8192, &[]),
        );
        let me = vm("me", None, PowerState::Running);
        let mut inv = Inventory {
            nodes: vec![n1.clone(), n2.clone(), n3.clone()],
            vms: vec![vm("web", Some(&n1), PowerState::Stopped)],
            rules: vec![rule("apart", RuleKind::AntiAffinity, &["me", "web"], &[])],
            ..Default::default()
        };
        // 꺼진 VM은 마지막 노드에 붙어 있어도 자리를 차지하지 않는다.
        assert!(hard_violations(&me, &n1, &inv, &none()).is_empty());

        let mut web = running("web", &n1);
        web.status = VmStatus {
            migration: Some(MigrationStatus {
                node: n2.id,
                task: Uuid::now_v7(),
                disks: vec![],
            }),
            ..Default::default()
        };
        inv.vms = vec![web];
        assert_eq!(hard_violations(&me, &n1, &inv, &none()), ["apart"]);
        assert_eq!(hard_violations(&me, &n2, &inv, &none()), ["apart"]);
        assert!(hard_violations(&me, &n3, &inv, &none()).is_empty());
    }

    #[test]
    fn assign_overrides_where_vms_are() {
        let (n1, n2) = (node("n1", 8192, &[]), node("n2", 8192, &[]));
        let me = vm("me", None, PowerState::Running);
        let web = running("web", &n1);
        let off = vm("off", None, PowerState::Stopped);
        let inv = Inventory {
            nodes: vec![n1.clone(), n2.clone()],
            vms: vec![web.clone(), off.clone()],
            rules: vec![rule(
                "apart",
                RuleKind::AntiAffinity,
                &["me", "web", "off"],
                &[],
            )],
            ..Default::default()
        };
        assert_eq!(hard_violations(&me, &n1, &inv, &none()), ["apart"]);
        assert!(hard_violations(&me, &n2, &inv, &none()).is_empty());

        // 복구가 web을 n2로 옮겼다고 본다: 이제 n1이 비고 n2를 차지한다. 메모리도 따라간다.
        let mut moved = Assign::default();
        moved.place(web.id, n2.id);
        assert!(hard_violations(&me, &n1, &inv, &moved).is_empty());
        assert_eq!(hard_violations(&me, &n2, &inv, &moved), ["apart"]);
        assert_eq!(free_mib(&me, &n1, &inv, &moved), 8192);
        assert_eq!(free_mib(&me, &n2, &inv, &moved), 8192 - 1024);
        // 꺼진 VM도 자리를 주면 차지하고, 자리를 빼면 아무 데도 없다.
        moved.place(off.id, n1.id);
        assert_eq!(hard_violations(&me, &n1, &inv, &moved), ["apart"]);
        moved.unplace(web.id);
        moved.unplace(off.id);
        assert!(hard_violations(&me, &n1, &inv, &moved).is_empty());
        assert!(hard_violations(&me, &n2, &inv, &moved).is_empty());
        assert_eq!(free_mib(&me, &n1, &inv, &moved), 8192);
    }

    #[test]
    fn disabled_rules_are_ignored() {
        let (n1, n2) = (node("n1", 8192, &[]), node("n2", 8192, &[]));
        let me = vm("me", None, PowerState::Running);
        let mut never = rule("never", RuleKind::NodeAntiAffinity, &["me"], &["n1"]);
        let mut avoid = rule("avoid", RuleKind::SoftNodeAntiAffinity, &["me"], &["n1"]);
        never.spec.enabled = false;
        avoid.spec.enabled = false;
        let inv = Inventory {
            nodes: vec![n1.clone(), n2],
            rules: vec![never, avoid],
            ..Default::default()
        };
        assert!(hard_violations(&me, &n1, &inv, &none()).is_empty());
        assert!(soft_violations(&me, &n1, &inv, &none()).is_empty());
    }

    #[test]
    fn order_is_priority_then_penalty_then_memory_then_id() {
        let c = |priority, penalty, free_mib, node: u128| Candidate {
            node: Uuid::from_u128(node),
            priority,
            penalty,
            free_mib,
        };
        let mut list = vec![
            c(None, 0, 9000, 1),
            c(Some(0), 50, 100, 2),
            c(Some(2), 50, 100, 3),
            c(Some(2), 10, 100, 4),
            c(Some(2), 10, 900, 5),
            c(Some(2), 10, 900, 6),
        ];
        assert_eq!(best(&list), Some(c(Some(2), 10, 900, 5)));
        list.sort_by(compare);
        let order: Vec<u128> = list.iter().map(|c| c.node.as_u128()).collect();
        assert_eq!(order, [5, 6, 4, 3, 2, 1]);
        assert_eq!(best(&[]), None);

        // 우선순위가 같으면 벌점, 벌점도 같으면 메모리가 많은 노드
        let (n1, n2, n3) = (
            node("n1", 4096, &[]),
            node("n2", 8192, &[]),
            node("n3", 2048, &[]),
        );
        let me = vm("me", None, PowerState::Running);
        let mut avoid = rule("avoid", RuleKind::SoftNodeAntiAffinity, &["me"], &["n2"]);
        avoid.spec.weight = Some(1);
        let mut inv = Inventory {
            nodes: vec![n1.clone(), n2.clone(), n3.clone()],
            ..Default::default()
        };
        assert_eq!(best(&candidates(&me, &inv, &none())).unwrap().node, n2.id);
        inv.rules = vec![avoid];
        assert_eq!(best(&candidates(&me, &inv, &none())).unwrap().node, n1.id);
        // 그룹 우선순위가 벌점보다 앞선다.
        inv.groups = vec![group("g", &[("n2", 5), ("n3", 1)], false)];
        let me = in_group(me, "g");
        let found = candidates(&me, &inv, &none());
        let order: Vec<_> = found.iter().map(|c| (c.node, c.priority)).collect();
        assert_eq!(order, [(n2.id, Some(5)), (n3.id, Some(1)), (n1.id, None)]);
    }

    #[test]
    fn restricted_groups_keep_vms_on_their_nodes() {
        let (n1, n2, n3) = (
            node("n1", 8192, &[]),
            node("n2", 8192, &[]),
            node("n3", 8192, &[]),
        );
        let mut offline = n2.clone();
        offline.status.online = false;
        let open = group("open", &[("n2", 1)], false);
        let closed = group("closed", &[("n2", 1)], true);
        let mut inv = Inventory {
            nodes: vec![n1.clone(), offline, n3.clone()],
            groups: vec![open, closed],
            ..Default::default()
        };
        // 그룹 노드가 꺼져 있으면, restricted가 아닌 그룹은 다른 노드로 가고 restricted 그룹은 갈 곳이 없다.
        let free = in_group(vm("free", None, PowerState::Running), "open");
        assert_eq!(candidates(&free, &inv, &none()).len(), 2);
        let kept = in_group(vm("kept", None, PowerState::Running), "closed");
        assert!(candidates(&kept, &inv, &none()).is_empty());
        assert_eq!(
            hard_violations(&kept, &n1, &inv, &none()),
            ["ha-group:closed"]
        );
        inv.nodes[1] = n2.clone();
        assert_eq!(
            candidates(&kept, &inv, &none())
                .iter()
                .map(|c| c.node)
                .collect::<Vec<_>>(),
            [n2.id]
        );
        // HA를 잠시 끈(ignored) VM에는 그룹을 쓰지 않는다.
        let mut ignored = kept.clone();
        ignored.spec.ha.as_mut().unwrap().state = HaState::Ignored;
        assert!(hard_violations(&ignored, &n1, &inv, &none()).is_empty());
    }

    #[test]
    fn conflicts_find_affinity_against_anti_affinity_transitively() {
        let inv = Inventory {
            nodes: vec![node("n1", 1, &[]), node("n2", 1, &[])],
            ..Default::default()
        };
        let ab = rule("ab", RuleKind::Affinity, &["a", "b"], &[]);
        let bc = rule("bc", RuleKind::Affinity, &["b", "c"], &[]);
        let ac = rule("ac-apart", RuleKind::AntiAffinity, &["a", "c"], &[]);
        let ad = rule("ad-apart", RuleKind::AntiAffinity, &["a", "d"], &[]);
        assert_eq!(
            conflicts(&[ab.clone(), ac.clone(), ad.clone()], &inv),
            Ok(())
        );
        assert_eq!(
            conflicts(&[ab.clone(), bc.clone(), ac.clone(), ad.clone()], &inv),
            Err(vec!["ab".into(), "ac-apart".into(), "bc".into()])
        );
        // 꺼진 규칙과 soft 규칙은 모순으로 보지 않는다.
        let mut off = bc.clone();
        off.spec.enabled = false;
        assert_eq!(conflicts(&[ab.clone(), off, ac.clone()], &inv), Ok(()));
        let soft = rule("soft", RuleKind::SoftAntiAffinity, &["a", "b"], &[]);
        assert_eq!(conflicts(&[ab.clone(), soft], &inv), Ok(()));
        // 태그는 지금의 VM 레이블로 푼다.
        let mut tagged = rule("tagged", RuleKind::AntiAffinity, &[], &[]);
        tagged.spec.vm_tag = Some("tier=db".into());
        let mut a = vm("a", None, PowerState::Stopped);
        let mut b = vm("b", None, PowerState::Stopped);
        a.labels.insert("tier".into(), "db".into());
        b.labels.insert("tier".into(), "db".into());
        let inv = Inventory {
            vms: vec![a, b],
            ..inv
        };
        assert_eq!(
            conflicts(&[ab, tagged], &inv),
            Err(vec!["ab".into(), "tagged".into()])
        );
    }

    #[test]
    fn conflicts_find_affinity_groups_with_no_allowed_node() {
        let mut witness = node("w", 1, &[]);
        witness.spec.witness = true;
        let inv = Inventory {
            nodes: vec![
                node("n1", 1, &[("zone", "a")]),
                node("n2", 1, &[("zone", "b")]),
                witness,
            ],
            ..Default::default()
        };
        let ab = rule("ab", RuleKind::Affinity, &["a", "b"], &[]);
        let a_n1 = rule("a-n1", RuleKind::NodeAffinity, &["a"], &["n1"]);
        let b_n2 = rule("b-n2", RuleKind::NodeAffinity, &["b"], &["n2"]);
        assert_eq!(conflicts(&[a_n1.clone(), b_n2.clone()], &inv), Ok(()));
        assert_eq!(
            conflicts(&[ab.clone(), a_n1.clone(), b_n2.clone()], &inv),
            Err(vec!["a-n1".into(), "ab".into(), "b-n2".into()])
        );
        // 혼자인 VM도: 허용 노드에서 금지 노드를 빼면 빈다(위트니스는 세지 않는다).
        let mut not_a = rule("not-zone-a", RuleKind::NodeAntiAffinity, &["c"], &[]);
        not_a.spec.node_tag = Some("zone=a".into());
        let c_n1 = rule("c-n1", RuleKind::NodeAffinity, &["c"], &["n1"]);
        assert_eq!(
            conflicts(&[not_a.clone(), c_n1], &inv),
            Err(vec!["c-n1".into(), "not-zone-a".into()])
        );
        let not_b = rule("not-n2", RuleKind::NodeAntiAffinity, &["c"], &["n2"]);
        assert_eq!(
            conflicts(&[not_a, not_b], &inv),
            Err(vec!["not-n2".into(), "not-zone-a".into()])
        );
        assert_eq!(conflicts(&[ab, a_n1], &inv), Ok(()));
    }
}
