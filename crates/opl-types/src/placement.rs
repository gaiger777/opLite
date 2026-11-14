//! HA 그룹과 배치 규칙(설계 §8). 규칙의 계산(조건·벌점)은 opl-api `placement`가 한다.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec, validate_name};

pub type HaGroup = Resource<HaGroupSpec, HaGroupStatus>;
pub type PlacementRule = Resource<PlacementRuleSpec, PlacementRuleStatus>;

/// HA 그룹 노드 우선순위의 최댓값
pub const MAX_PRIORITY: u32 = 1000;
/// soft 규칙의 weight 범위와 기본값
pub const MAX_WEIGHT: u8 = 100;
pub const DEFAULT_WEIGHT: u8 = 50;

/// HA 그룹: HA VM을 복구할 노드와 그 우선순위. VM은 `ha.group`으로 그룹을 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct HaGroupSpec {
    /// 그룹 노드. 우선순위가 큰 노드에 먼저 놓는다.
    pub nodes: Vec<HaGroupNode>,
    /// 켜면 그룹 밖 노드에는 놓지 않는다(놓을 곳이 없으면 켜지 않는다).
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub restricted: bool,
    /// 켜면 우선순위가 더 높은 노드가 돌아와도 되돌아가지 않는다.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nofailback: bool,
}

/// HA 그룹의 노드 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct HaGroupNode {
    /// 노드 이름
    pub node: String,
    /// 0~1000, 클수록 먼저
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub priority: u32,
}

impl ResourceSpec for HaGroupSpec {
    const KIND: Kind = Kind::HaGroup;
    type Status = HaGroupStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct HaGroupStatus {}

impl HaGroupSpec {
    /// 노드가 있는지는 저장소를 보는 API가 확인한다.
    pub fn validate(&self) -> Result<(), PlacementError> {
        if self.nodes.is_empty() {
            return Err(PlacementError::NoGroupNodes);
        }
        let mut seen = BTreeSet::new();
        for n in &self.nodes {
            if validate_name(&n.node).is_err() {
                return Err(PlacementError::NodeName(n.node.clone()));
            }
            if !seen.insert(&n.node) {
                return Err(PlacementError::DuplicateNode(n.node.clone()));
            }
            if n.priority > MAX_PRIORITY {
                return Err(PlacementError::Priority(n.node.clone()));
            }
        }
        Ok(())
    }

    /// 이 노드의 그룹 우선순위. 그룹 밖이면 None이다.
    pub fn priority(&self, node: &str) -> Option<u32> {
        self.nodes
            .iter()
            .find(|n| n.node == node)
            .map(|n| n.priority)
    }
}

/// 배치 규칙: VM끼리(같은 노드·다른 노드) 또는 VM과 노드(이 노드들 중·이 노드들은 안 됨). hard 규칙은
/// 조건이고 soft 규칙은 벌점(`weight`)이다. 모든 VM에 적용한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PlacementRuleSpec {
    pub kind: RuleKind,
    /// 규칙의 VM 이름(아직 없는 VM이어도 된다). `vm_tag`와 둘 중 하나만.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub vms: Vec<String>,
    /// 이 레이블(`key=value`)을 가진 VM들
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(nullable = false, default, example = "app=web")
    )]
    pub vm_tag: Option<String>,
    /// VM과 노드 규칙의 노드 이름. `node_tag`와 둘 중 하나만.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nodes: Vec<String>,
    /// 이 레이블(`key=value`, 예: `zone=rack-a`)을 가진 노드들
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(nullable = false, default, example = "zone=rack-a")
    )]
    pub node_tag: Option<String>,
    /// soft 규칙의 벌점 1~100. 없으면 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub weight: Option<u8>,
    /// 끄면 없는 것으로 본다.
    #[serde(default = "yes")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub enabled: bool,
    /// hard 규칙만: HA 복구가 지킬 수 없으면 어기고 켠다.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub ha_may_violate: bool,
}

fn yes() -> bool {
    true
}

impl ResourceSpec for PlacementRuleSpec {
    const KIND: Kind = Kind::PlacementRule;
    type Status = PlacementRuleStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct PlacementRuleStatus {}

/// 배치 규칙의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RuleKind {
    /// 반드시 같은 노드
    Affinity,
    /// 반드시 서로 다른 노드
    AntiAffinity,
    /// 되도록 같은 노드
    SoftAffinity,
    /// 되도록 서로 다른 노드
    SoftAntiAffinity,
    /// 반드시 이 노드들 중
    NodeAffinity,
    /// 이 노드들은 안 됨
    NodeAntiAffinity,
    /// 되도록 이 노드들 중
    SoftNodeAffinity,
    /// 되도록 이 노드들은 피함
    SoftNodeAntiAffinity,
}

impl RuleKind {
    /// 반드시(hard) 규칙인지. 아니면 벌점(soft) 규칙이다.
    pub fn is_hard(self) -> bool {
        matches!(
            self,
            RuleKind::Affinity
                | RuleKind::AntiAffinity
                | RuleKind::NodeAffinity
                | RuleKind::NodeAntiAffinity
        )
    }

    /// VM끼리 규칙인지. 아니면 VM과 노드 규칙이다.
    pub fn is_vm_rule(self) -> bool {
        matches!(
            self,
            RuleKind::Affinity
                | RuleKind::AntiAffinity
                | RuleKind::SoftAffinity
                | RuleKind::SoftAntiAffinity
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::Affinity => "affinity",
            RuleKind::AntiAffinity => "anti-affinity",
            RuleKind::SoftAffinity => "soft-affinity",
            RuleKind::SoftAntiAffinity => "soft-anti-affinity",
            RuleKind::NodeAffinity => "node-affinity",
            RuleKind::NodeAntiAffinity => "node-anti-affinity",
            RuleKind::SoftNodeAffinity => "soft-node-affinity",
            RuleKind::SoftNodeAntiAffinity => "soft-node-anti-affinity",
        }
    }
}

impl PlacementRuleSpec {
    /// 노드가 있는지는 저장소를 보는 API가 확인한다.
    pub fn validate(&self) -> Result<(), PlacementError> {
        let kind = self.kind.as_str();
        match (&self.vm_tag, self.vms.is_empty()) {
            (Some(_), false) | (None, true) => return Err(PlacementError::VmSelector),
            (Some(tag), true) => {
                parse_tag(tag).ok_or_else(|| PlacementError::Tag(tag.clone()))?;
            }
            (None, false) => {
                if self.kind.is_vm_rule() && self.vms.len() < 2 {
                    return Err(PlacementError::TooFewVms(kind));
                }
                check_names(
                    &self.vms,
                    PlacementError::VmName,
                    PlacementError::DuplicateVm,
                )?;
            }
        }
        if self.kind.is_vm_rule() {
            if !self.nodes.is_empty() || self.node_tag.is_some() {
                return Err(PlacementError::NodesOnVmRule(kind));
            }
        } else {
            match (&self.node_tag, self.nodes.is_empty()) {
                (Some(_), false) | (None, true) => return Err(PlacementError::NodeSelector(kind)),
                (Some(tag), true) => {
                    parse_tag(tag).ok_or_else(|| PlacementError::Tag(tag.clone()))?;
                }
                (None, false) => check_names(
                    &self.nodes,
                    PlacementError::NodeName,
                    PlacementError::DuplicateNode,
                )?,
            }
        }
        match self.weight {
            Some(_) if self.kind.is_hard() => return Err(PlacementError::WeightOnHard(kind)),
            Some(w) if !(1..=MAX_WEIGHT).contains(&w) => return Err(PlacementError::Weight),
            _ => {}
        }
        if self.ha_may_violate && !self.kind.is_hard() {
            return Err(PlacementError::HaMayViolateOnSoft(kind));
        }
        Ok(())
    }

    /// soft 규칙의 벌점(없으면 [`DEFAULT_WEIGHT`])
    pub fn weight(&self) -> u32 {
        u32::from(self.weight.unwrap_or(DEFAULT_WEIGHT))
    }

    /// 이 이름·레이블의 VM이 규칙의 VM인지
    pub fn has_vm(&self, name: &str, labels: &BTreeMap<String, String>) -> bool {
        match &self.vm_tag {
            Some(tag) => tag_matches(tag, labels),
            None => self.vms.iter().any(|v| v == name),
        }
    }

    /// 이 이름·레이블의 노드가 규칙의 노드 집합에 드는지(VM과 노드 규칙)
    pub fn has_node(&self, name: &str, labels: &BTreeMap<String, String>) -> bool {
        match &self.node_tag {
            Some(tag) => tag_matches(tag, labels),
            None => self.nodes.iter().any(|n| n == name),
        }
    }
}

fn check_names(
    names: &[String],
    bad: fn(String) -> PlacementError,
    dup: fn(String) -> PlacementError,
) -> Result<(), PlacementError> {
    let mut seen = BTreeSet::new();
    for name in names {
        if validate_name(name).is_err() {
            return Err(bad(name.clone()));
        }
        if !seen.insert(name) {
            return Err(dup(name.clone()));
        }
    }
    Ok(())
}

/// 태그 `key=value`를 나눈다. 키가 비었거나 `=`가 없으면 None이다(값은 비어도 된다).
pub fn parse_tag(tag: &str) -> Option<(&str, &str)> {
    tag.split_once('=').filter(|(key, _)| !key.is_empty())
}

/// 레이블에 태그의 키가 있고 값이 같은지
pub fn tag_matches(tag: &str, labels: &BTreeMap<String, String>) -> bool {
    parse_tag(tag).is_some_and(|(key, value)| labels.get(key).is_some_and(|v| v == value))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlacementError {
    #[error("nodes must not be empty")]
    NoGroupNodes,
    #[error("node {0:?} is not a valid node name")]
    NodeName(String),
    #[error("node {0:?} is listed more than once")]
    DuplicateNode(String),
    #[error("the priority of node {0:?} must be between 0 and {MAX_PRIORITY}")]
    Priority(String),
    #[error("give exactly one of vms and vm_tag")]
    VmSelector,
    #[error("{0} rules need at least two VMs")]
    TooFewVms(&'static str),
    #[error("VM {0:?} is not a valid VM name")]
    VmName(String),
    #[error("VM {0:?} is listed more than once")]
    DuplicateVm(String),
    #[error("tag {0:?} must be key=value")]
    Tag(String),
    #[error("{0} rules are between VMs; they take no nodes or node_tag")]
    NodesOnVmRule(&'static str),
    #[error("{0} rules need exactly one of nodes and node_tag")]
    NodeSelector(&'static str),
    #[error("weight is only for soft rules, not {0}")]
    WeightOnHard(&'static str),
    #[error("weight must be between 1 and {MAX_WEIGHT}")]
    Weight,
    #[error("ha_may_violate is only for hard rules, not {0}")]
    HaMayViolateOnSoft(&'static str),
}

/// 리스 갱신 하나에 싣는 VM 부하 수 상한. 넘으면 RSS가 큰 것부터 둔다(노드와 리더가 둘 다 자른다).
pub const MAX_LOAD_VMS: usize = 256;

/// 노드가 리스 갱신에 실어 리더에 보내는 부하 요약(설계 §8.3). 리더는 메모리에만 둔다. 모두 정수다(리더의
/// 리스 표가 `Eq`). 모르는 필드는 버린다(뒤 버전이 더한 필드를 옛 리더가 무시).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadSummary {
    /// 지난 1분 노드 CPU 사용률 ‰(모든 코어 합 1000)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu1: Option<u16>,
    /// 지난 5분 노드 CPU 사용률 ‰
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu5: Option<u16>,
    /// 사용 메모리 MiB(마지막 점)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<u64>,
    /// 이 노드에서 도는 VM들의 부하
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vms: Vec<VmLoad>,
}

/// VM 하나의 부하
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmLoad {
    pub id: Uuid,
    /// 지난 5분 CPU 사용률 ‰(vCPU 대비, 1000을 넘을 수 있음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u16>,
    /// RSS MiB(마지막 점)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<u64>,
}

impl LoadSummary {
    /// VM이 `MAX_LOAD_VMS`보다 많으면 RSS가 큰 것부터 그만큼만 둔다(RSS를 모르는 VM이 먼저 빠진다).
    pub fn truncate_vms(&mut self) {
        if self.vms.len() > MAX_LOAD_VMS {
            self.vms.sort_by_key(|v| std::cmp::Reverse(v.mem));
            self.vms.truncate(MAX_LOAD_VMS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: RuleKind) -> PlacementRuleSpec {
        PlacementRuleSpec {
            kind,
            vms: vec!["a".into(), "b".into()],
            vm_tag: None,
            nodes: vec![],
            node_tag: None,
            weight: None,
            enabled: true,
            ha_may_violate: false,
        }
    }

    #[test]
    fn kinds_are_listed_by_their_str() {
        assert_eq!(serde_json::to_value(Kind::HaGroup).unwrap(), "ha-group");
        assert_eq!(Kind::HaGroup.as_str(), "ha-group");
        assert_eq!(
            serde_json::to_value(Kind::PlacementRule).unwrap(),
            "placement-rule"
        );
        assert_eq!(Kind::PlacementRule.as_str(), "placement-rule");
        for kind in [
            RuleKind::Affinity,
            RuleKind::AntiAffinity,
            RuleKind::SoftAffinity,
            RuleKind::SoftAntiAffinity,
            RuleKind::NodeAffinity,
            RuleKind::NodeAntiAffinity,
            RuleKind::SoftNodeAffinity,
            RuleKind::SoftNodeAntiAffinity,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
            let hard = !kind.as_str().starts_with("soft-");
            let vm = !kind.as_str().contains("node");
            assert_eq!(kind.is_hard(), hard, "{kind:?}");
            assert_eq!(kind.is_vm_rule(), vm, "{kind:?}");
        }
    }

    #[test]
    fn rule_fills_missing_fields_and_rejects_unknown() {
        let spec: PlacementRuleSpec = serde_json::from_value(
            serde_json::json!({ "kind": "anti-affinity", "vms": ["a", "b"] }),
        )
        .unwrap();
        assert_eq!(spec, rule(RuleKind::AntiAffinity));
        assert!(spec.enabled);
        assert_eq!(spec.weight(), u32::from(DEFAULT_WEIGHT));
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "anti-affinity",
                "vms": ["a", "b"],
                "nodes": [],
                "enabled": true,
                "ha_may_violate": false
            })
        );
        assert_eq!(
            serde_json::from_value::<PlacementRuleSpec>(json).unwrap(),
            spec
        );
        for bad in [
            serde_json::json!({ "kind": "anti-affinity", "vms": ["a", "b"], "priority": 1 }),
            serde_json::json!({ "kind": "together", "vms": ["a", "b"] }),
            serde_json::json!({ "vms": ["a", "b"] }),
        ] {
            assert!(
                serde_json::from_value::<PlacementRuleSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn group_fills_missing_fields_and_rejects_unknown() {
        let spec: HaGroupSpec =
            serde_json::from_value(serde_json::json!({ "nodes": [{ "node": "n1" }] })).unwrap();
        assert_eq!(
            spec,
            HaGroupSpec {
                nodes: vec![HaGroupNode {
                    node: "n1".into(),
                    priority: 0
                }],
                restricted: false,
                nofailback: false,
            }
        );
        assert_eq!(
            serde_json::from_value::<HaGroupSpec>(serde_json::to_value(&spec).unwrap()).unwrap(),
            spec
        );
        for bad in [
            serde_json::json!({ "nodes": [], "failback": true }),
            serde_json::json!({ "nodes": [{ "node": "n1", "weight": 1 }] }),
        ] {
            assert!(
                serde_json::from_value::<HaGroupSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn validates_groups() {
        let group = |nodes: &[(&str, u32)]| HaGroupSpec {
            nodes: nodes
                .iter()
                .map(|(n, p)| HaGroupNode {
                    node: n.to_string(),
                    priority: *p,
                })
                .collect(),
            restricted: true,
            nofailback: false,
        };
        assert_eq!(group(&[("n1", 0), ("n2", MAX_PRIORITY)]).validate(), Ok(()));
        assert_eq!(group(&[]).validate(), Err(PlacementError::NoGroupNodes));
        assert_eq!(
            group(&[("n1", 1), ("n1", 2)]).validate(),
            Err(PlacementError::DuplicateNode("n1".into()))
        );
        assert_eq!(
            group(&[("n1", MAX_PRIORITY + 1)]).validate(),
            Err(PlacementError::Priority("n1".into()))
        );
        assert_eq!(
            group(&[("N1", 1)]).validate(),
            Err(PlacementError::NodeName("N1".into()))
        );
        let g = group(&[("n1", 1), ("n2", 5)]);
        assert_eq!(g.priority("n2"), Some(5));
        assert_eq!(g.priority("n3"), None);
    }

    #[test]
    fn validates_rules() {
        assert_eq!(rule(RuleKind::Affinity).validate(), Ok(()));
        let with = |f: &dyn Fn(&mut PlacementRuleSpec), kind| {
            let mut r = rule(kind);
            f(&mut r);
            r.validate()
        };
        use PlacementError as E;
        use RuleKind as K;
        // VM 고르기는 둘 중 하나만
        assert_eq!(with(&|r| r.vms.clear(), K::Affinity), Err(E::VmSelector));
        assert_eq!(
            with(&|r| r.vm_tag = Some("app=web".into()), K::Affinity),
            Err(E::VmSelector)
        );
        assert_eq!(
            with(
                &|r| {
                    r.vms.clear();
                    r.vm_tag = Some("app=web".into());
                },
                K::Affinity
            ),
            Ok(())
        );
        assert_eq!(
            with(
                &|r| {
                    r.vms.clear();
                    r.vm_tag = Some("app".into());
                },
                K::Affinity
            ),
            Err(E::Tag("app".into()))
        );
        // VM끼리 규칙의 목록은 둘 이상, 노드 규칙은 하나도 된다.
        assert_eq!(
            with(&|r| r.vms.truncate(1), K::AntiAffinity),
            Err(E::TooFewVms("anti-affinity"))
        );
        assert_eq!(
            with(
                &|r| {
                    r.vms.truncate(1);
                    r.nodes = vec!["n1".into()];
                },
                K::NodeAffinity
            ),
            Ok(())
        );
        assert_eq!(
            with(&|r| r.vms[1] = "a".into(), K::Affinity),
            Err(E::DuplicateVm("a".into()))
        );
        assert_eq!(
            with(&|r| r.vms[1] = "B".into(), K::Affinity),
            Err(E::VmName("B".into()))
        );
        // VM끼리 규칙은 노드를 받지 않고, 노드 규칙은 노드 고르기 둘 중 하나만
        assert_eq!(
            with(&|r| r.nodes = vec!["n1".into()], K::SoftAffinity),
            Err(E::NodesOnVmRule("soft-affinity"))
        );
        assert_eq!(
            with(&|r| r.node_tag = Some("zone=a".into()), K::Affinity),
            Err(E::NodesOnVmRule("affinity"))
        );
        assert_eq!(
            with(&|_| {}, K::NodeAntiAffinity),
            Err(E::NodeSelector("node-anti-affinity"))
        );
        assert_eq!(
            with(
                &|r| {
                    r.nodes = vec!["n1".into()];
                    r.node_tag = Some("zone=a".into());
                },
                K::NodeAffinity
            ),
            Err(E::NodeSelector("node-affinity"))
        );
        assert_eq!(
            with(&|r| r.node_tag = Some("zone=a".into()), K::SoftNodeAffinity),
            Ok(())
        );
        assert_eq!(
            with(&|r| r.node_tag = Some("=a".into()), K::NodeAffinity),
            Err(E::Tag("=a".into()))
        );
        // weight는 soft에만 1~100, ha_may_violate는 hard에만
        assert_eq!(
            with(&|r| r.weight = Some(10), K::AntiAffinity),
            Err(E::WeightOnHard("anti-affinity"))
        );
        assert_eq!(with(&|r| r.weight = Some(100), K::SoftAntiAffinity), Ok(()));
        assert_eq!(
            with(&|r| r.weight = Some(0), K::SoftAntiAffinity),
            Err(E::Weight)
        );
        assert_eq!(
            with(&|r| r.weight = Some(101), K::SoftAffinity),
            Err(E::Weight)
        );
        assert_eq!(with(&|r| r.ha_may_violate = true, K::Affinity), Ok(()));
        assert_eq!(
            with(&|r| r.ha_may_violate = true, K::SoftAffinity),
            Err(E::HaMayViolateOnSoft("soft-affinity"))
        );
    }

    #[test]
    fn tags_match_labels_by_key_and_value() {
        assert_eq!(parse_tag("zone=rack-a"), Some(("zone", "rack-a")));
        assert_eq!(parse_tag("k=a=b"), Some(("k", "a=b")));
        assert_eq!(parse_tag("k="), Some(("k", "")));
        assert_eq!(parse_tag("zone"), None);
        assert_eq!(parse_tag("=x"), None);
        let labels = BTreeMap::from([("zone".to_string(), "rack-a".to_string())]);
        assert!(tag_matches("zone=rack-a", &labels));
        assert!(!tag_matches("zone=rack-b", &labels));
        assert!(!tag_matches("rack=rack-a", &labels));

        let mut r = rule(RuleKind::NodeAffinity);
        r.vms.clear();
        r.vm_tag = Some("app=db".into());
        r.node_tag = Some("zone=rack-a".into());
        let db = BTreeMap::from([("app".to_string(), "db".to_string())]);
        assert!(r.has_vm("anything", &db));
        assert!(!r.has_vm("a", &BTreeMap::new()));
        assert!(r.has_node("n9", &labels));
        assert!(!r.has_node("n9", &BTreeMap::new()));
        let r = rule(RuleKind::Affinity);
        assert!(r.has_vm("a", &BTreeMap::new()));
        assert!(!r.has_vm("c", &db));
    }

    /// 빈 요약은 `{}`이고 값은 왕복하며, 모르는 필드는 버린다.
    #[test]
    fn load_summary_wire_format() {
        let empty = LoadSummary::default();
        assert_eq!(serde_json::to_value(&empty).unwrap(), serde_json::json!({}));
        assert_eq!(
            serde_json::from_value::<LoadSummary>(serde_json::json!({})).unwrap(),
            empty
        );
        let id = Uuid::from_u128(7);
        let full = LoadSummary {
            cpu1: Some(120),
            cpu5: Some(95),
            mem: Some(4096),
            vms: vec![
                VmLoad {
                    id,
                    cpu: Some(1250),
                    mem: Some(2048),
                },
                VmLoad {
                    id,
                    cpu: None,
                    mem: None,
                },
            ],
        };
        let value = serde_json::to_value(&full).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"cpu1": 120, "cpu5": 95, "mem": 4096, "vms": [
                {"id": id.to_string(), "cpu": 1250, "mem": 2048},
                {"id": id.to_string()}
            ]})
        );
        assert_eq!(serde_json::from_value::<LoadSummary>(value).unwrap(), full);
        let newer =
            serde_json::json!({"cpu1": 5, "disk": 3, "vms": [{"id": id.to_string(), "net": 1}]});
        assert_eq!(
            serde_json::from_value::<LoadSummary>(newer).unwrap(),
            LoadSummary {
                cpu1: Some(5),
                vms: vec![VmLoad {
                    id,
                    cpu: None,
                    mem: None
                }],
                ..Default::default()
            }
        );
    }

    /// VM이 상한을 넘으면 RSS가 큰 256개만 남는다.
    #[test]
    fn truncate_vms_keeps_the_largest_rss() {
        let vms: Vec<_> = (0..300u64)
            .map(|i| VmLoad {
                id: Uuid::from_u128(i.into()),
                cpu: Some(1),
                // RSS를 모르는 VM 하나는 가장 먼저 빠진다.
                mem: (i != 299).then_some((i * 7919) % 300),
            })
            .collect();
        let mut s = LoadSummary {
            vms: vms.clone(),
            ..Default::default()
        };
        s.truncate_vms();
        assert_eq!(s.vms.len(), MAX_LOAD_VMS);
        let kept: BTreeSet<_> = s.vms.iter().map(|v| v.id).collect();
        let mut want = vms;
        want.sort_by_key(|v| std::cmp::Reverse(v.mem));
        assert!(want[..MAX_LOAD_VMS].iter().all(|v| kept.contains(&v.id)));
        assert!(!kept.contains(&Uuid::from_u128(299)));
        let smallest_kept = s.vms.iter().filter_map(|v| v.mem).min().unwrap();
        assert!(
            want[MAX_LOAD_VMS..]
                .iter()
                .all(|v| v.mem.is_none_or(|m| m <= smallest_kept))
        );

        // 상한 안이면 그대로 둔다.
        let mut small = LoadSummary {
            vms: s.vms[..3].to_vec(),
            ..Default::default()
        };
        let before = small.clone();
        small.truncate_vms();
        assert_eq!(small, before);
    }
}
