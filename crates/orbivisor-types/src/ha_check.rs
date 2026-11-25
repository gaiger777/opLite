//! HA 상태 검사(설계 §8.2). 노드가 네트워크 링크·핑·스토리지 풀 경로를 5초마다 검사해 결과를 리스 갱신에 실어
//! 보내고, 리더가 `grace_secs` 넘게 실패한 검사에 `on_fail`대로 대응한다. 검사 하나가 `HaCheck` 리소스 하나다
//! (이름 = 검사 이름). 클러스터 링크의 자동 검사(`cluster-link-<n>`)는 리소스가 아니라 노드의 보고에만 있다.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::network::valid_ifname;
use crate::resource::{Kind, Resource, ResourceSpec};

pub type HaCheck = Resource<HaCheckSpec, HaCheckStatus>;

/// 기본 grace(초)
pub const DEFAULT_GRACE_SECS: u32 = 10;
/// grace의 최솟값(초): 노드의 검사 간격(5초) 두 번. 그보다 짧으면 한 번의 실패로 대응할 수 있다.
pub const MIN_GRACE_SECS: u32 = 10;
/// grace의 최댓값(초)
pub const MAX_GRACE_SECS: u32 = 600;
/// 핑 대상 주소 수 상한
pub const MAX_PING_ADDRESSES: usize = 8;
/// 클러스터의 검사 수 상한. 리스 갱신의 크기를 묶는다.
pub const MAX_HA_CHECKS: usize = 64;
/// 노드가 클러스터 링크마다 스스로 만드는 관리 검사의 이름 접두어(`cluster-link-1`부터)
pub const AUTO_PREFIX: &str = "cluster-link-";
/// 결과의 오류 문구 상한(바이트)
pub const MAX_CHECK_ERROR: usize = 200;

/// 검사 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct HaCheckSpec {
    /// 검사하는 네트워크·장치의 쓰임. 기본 대응과 영향받는 VM을 정한다.
    pub role: HaCheckRole,
    /// 모든 노드의 검사 대상(노드별 덮기가 없을 때)
    pub target: HaCheckTarget,
    /// 실패했을 때의 대응. 없으면 역할의 기본(`storage` → `restart_elsewhere`, `vm` → `migrate`,
    /// `management` → `warn`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub on_fail: Option<HaCheckAction>,
    /// 이만큼(초) 넘게 실패해야 대응한다. 10~600
    #[serde(default = "default_grace", skip_serializing_if = "is_default_grace")]
    #[cfg_attr(feature = "openapi", schema(default = 10))]
    pub grace_secs: u32,
    /// 노드 이름 → 그 노드의 대상(종류는 `target`과 같아야 한다)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nodes: BTreeMap<String, HaCheckTarget>,
    /// 끄면 노드가 검사하지 않는다.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub enabled: bool,
}

impl ResourceSpec for HaCheckSpec {
    const KIND: Kind = Kind::HaCheck;
    type Status = HaCheckStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct HaCheckStatus {}

/// 검사하는 것의 쓰임
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum HaCheckRole {
    /// 관리 네트워크(리스 경로는 따로 처리한다)
    Management,
    /// 스토리지 네트워크·풀. 영향받는 VM은 그 풀(링크·핑이면 공유 풀)에 디스크가 있는 VM
    Storage,
    /// VM 네트워크. 영향받는 VM은 그 인터페이스 뒤의 브리지를 쓰는 VM
    Vm,
}

impl HaCheckRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Management => "management",
            Self::Storage => "storage",
            Self::Vm => "vm",
        }
    }

    /// `on_fail`이 없을 때의 대응
    pub fn default_action(self) -> HaCheckAction {
        match self {
            Self::Management => HaCheckAction::Warn,
            Self::Storage => HaCheckAction::RestartElsewhere,
            Self::Vm => HaCheckAction::Migrate,
        }
    }

    /// 이 역할에 둘 수 있는 대응. 스토리지는 디스크 I/O가 멈추면 라이브 마이그레이션이 끝나지 않으므로
    /// `migrate`를 받지 않고, 관리 네트워크는 VM을 옮길 까닭이 없으므로 경고와 전원 끄기만 받는다.
    pub fn allows(self, action: HaCheckAction) -> bool {
        match self {
            Self::Management => matches!(action, HaCheckAction::Warn | HaCheckAction::Fence),
            Self::Storage => action != HaCheckAction::Migrate,
            Self::Vm => true,
        }
    }
}

impl fmt::Display for HaCheckRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 검사가 grace 넘게 실패했을 때의 대응
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum HaCheckAction {
    /// 경고만
    Warn,
    /// 영향받는 HA VM을 다른 노드로 라이브 마이그레이션
    Migrate,
    /// 영향받는 HA VM을 이 노드에서 강제로 끄고 다른 노드에서 켬
    RestartElsewhere,
    /// 노드 전원 끄기(펜싱) 뒤 모든 HA VM 복구
    Fence,
}

impl HaCheckAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Migrate => "migrate",
            Self::RestartElsewhere => "restart_elsewhere",
            Self::Fence => "fence",
        }
    }
}

impl fmt::Display for HaCheckAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 검사 대상
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HaCheckTarget {
    /// 인터페이스(본딩이면 본딩)의 링크·carrier
    Link { iface: String },
    /// 주소마다 ICMP 하나. `need`개 이상 답하면 통과다.
    Ping {
        /// 1~8개
        #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>, format = "ip"))]
        addresses: Vec<IpAddr>,
        /// 이 인터페이스로 보낸다(Linux만).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        via: Option<String>,
        /// 1~주소 수
        #[serde(default = "one")]
        #[cfg_attr(feature = "openapi", schema(default = 1))]
        need: u8,
    },
    /// 스토리지 풀의 경로(NFS·공유 dir의 파일 읽기, `lvm-shared`의 iSCSI 세션·multipath 경로, RBD의 `rados`)
    Pool { name: String },
}

impl HaCheckTarget {
    /// `link`·`ping`·`pool`
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Link { .. } => "link",
            Self::Ping { .. } => "ping",
            Self::Pool { .. } => "pool",
        }
    }

    /// 대상 하나의 형식
    fn validate(&self, role: HaCheckRole) -> Result<(), HaCheckError> {
        match self {
            Self::Link { iface } => ifname(iface),
            Self::Ping {
                addresses,
                via,
                need,
            } => {
                if addresses.is_empty() || addresses.len() > MAX_PING_ADDRESSES {
                    return Err(HaCheckError::Addresses(addresses.len()));
                }
                let mut seen = BTreeSet::new();
                for a in addresses {
                    if !seen.insert(a) {
                        return Err(HaCheckError::DuplicateAddress(*a));
                    }
                }
                if *need == 0 || usize::from(*need) > addresses.len() {
                    return Err(HaCheckError::Need {
                        need: *need,
                        addresses: addresses.len(),
                    });
                }
                via.as_deref().map_or(Ok(()), ifname)
            }
            Self::Pool { .. } if role != HaCheckRole::Storage => Err(HaCheckError::PoolRole(role)),
            Self::Pool { .. } => Ok(()),
        }
    }
}

fn ifname(name: &str) -> Result<(), HaCheckError> {
    if valid_ifname(name) {
        Ok(())
    } else {
        Err(HaCheckError::Iface(name.to_string()))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HaCheckError {
    #[error("grace_secs {0} is out of range: use {MIN_GRACE_SECS}-{MAX_GRACE_SECS}")]
    Grace(u32),
    #[error("invalid interface name {0:?}: use 1-15 characters without '/', ':' or spaces")]
    Iface(String),
    #[error("give 1-{MAX_PING_ADDRESSES} ping addresses, not {0}")]
    Addresses(usize),
    #[error("ping address {0} is given twice")]
    DuplicateAddress(IpAddr),
    #[error("need {need} is out of range: use 1-{addresses} (the number of addresses)")]
    Need { need: u8, addresses: usize },
    #[error("a pool target needs role storage, not {0}")]
    PoolRole(HaCheckRole),
    #[error("on_fail {action} is not allowed for role {role}")]
    Action {
        role: HaCheckRole,
        action: HaCheckAction,
    },
    #[error(
        "the target for node {node:?} must be a {want} target like the check's target, not {got}"
    )]
    OverrideKind {
        node: String,
        want: &'static str,
        got: &'static str,
    },
}

impl HaCheckSpec {
    /// 실패했을 때의 대응(없으면 역할의 기본)
    pub fn action(&self) -> HaCheckAction {
        self.on_fail.unwrap_or_else(|| self.role.default_action())
    }

    /// 노드 `node`(이름)의 검사 대상: 덮기가 있으면 그것, 없으면 `target`
    pub fn target_for(&self, node: &str) -> &HaCheckTarget {
        self.nodes.get(node).unwrap_or(&self.target)
    }

    /// 형식만 본다(노드·풀이 있는지, 풀이 공유인지는 API가 본다).
    pub fn validate(&self) -> Result<(), HaCheckError> {
        if !(MIN_GRACE_SECS..=MAX_GRACE_SECS).contains(&self.grace_secs) {
            return Err(HaCheckError::Grace(self.grace_secs));
        }
        if let Some(action) = self.on_fail
            && !self.role.allows(action)
        {
            return Err(HaCheckError::Action {
                role: self.role,
                action,
            });
        }
        self.target.validate(self.role)?;
        for (node, target) in &self.nodes {
            if target.kind() != self.target.kind() {
                return Err(HaCheckError::OverrideKind {
                    node: node.clone(),
                    want: self.target.kind(),
                    got: target.kind(),
                });
            }
            target.validate(self.role)?;
        }
        Ok(())
    }
}

fn default_grace() -> u32 {
    DEFAULT_GRACE_SECS
}

fn is_default_grace(secs: &u32) -> bool {
    *secs == DEFAULT_GRACE_SECS
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn one() -> u8 {
    1
}

/// 한 노드에서 검사 하나의 결과. 노드가 리스 갱신에 실어 리더에 보내고(판단용), 노드 상태에도 쓴다(화면용).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CheckReport {
    /// 검사 이름(`HaCheck` 이름이나 자동 검사 `cluster-link-<n>`)
    pub name: String,
    pub state: CheckState,
    /// 실패·degraded·unknown의 이유(200바이트까지)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 노드 시계로 실패·degraded가 시작된 때(유닉스 초, 화면용. 리더는 자기 시계로 grace를 잰다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub since: Option<u64>,
}

impl CheckReport {
    /// 오류 문구는 200바이트(글자 경계)까지 자른다.
    pub fn new(
        name: impl Into<String>,
        state: CheckState,
        error: Option<String>,
        since: Option<u64>,
    ) -> Self {
        Self {
            name: name.into(),
            state,
            error: error.map(truncate_error),
            since,
        }
    }
}

fn truncate_error(mut error: String) -> String {
    if error.len() > MAX_CHECK_ERROR {
        let mut end = MAX_CHECK_ERROR;
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
    }
    error
}

/// 검사 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Ok,
    /// 통과지만 일부가 나쁨(본딩의 일부 포트, multipath의 일부 경로)
    Degraded,
    Failing,
    /// 이 노드에 해당 없음(풀을 쓰지 않는 노드, 없는 풀, 이 OS에서 볼 수 없음). 실패도 통과도 아니다.
    /// 이 빌드가 모르는 값(뒤 버전이 더한 상태)도 이것으로 읽는다(리스 갱신을 깨지 않게).
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn spec(v: serde_json::Value) -> HaCheckSpec {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn kind_is_ha_check() {
        assert_eq!(HaCheckSpec::KIND, Kind::HaCheck);
        assert_eq!(serde_json::to_value(Kind::HaCheck).unwrap(), "ha-check");
        assert_eq!(Kind::HaCheck.as_str(), "ha-check");
    }

    /// 빠진 선택 필드는 기본값이고, 기본값은 다시 쓸 때 빠진다.
    #[test]
    fn missing_fields_take_their_defaults_and_defaults_are_not_written() {
        let text = json!({ "role": "storage", "target": { "kind": "link", "iface": "bond1" } });
        let s = spec(text.clone());
        assert_eq!(
            s,
            HaCheckSpec {
                role: HaCheckRole::Storage,
                target: HaCheckTarget::Link {
                    iface: "bond1".into()
                },
                on_fail: None,
                grace_secs: 10,
                nodes: BTreeMap::new(),
                enabled: true,
            }
        );
        assert_eq!(serde_json::to_value(&s).unwrap(), text);

        let full = json!({
            "role": "vm",
            "target": { "kind": "ping", "addresses": ["10.0.0.1", "fd00::1"], "via": "bond0", "need": 2 },
            "on_fail": "restart_elsewhere",
            "grace_secs": 30,
            "nodes": { "b": { "kind": "ping", "addresses": ["10.0.1.1"], "need": 1 } },
            "enabled": false
        });
        let s = spec(full.clone());
        assert_eq!(s.on_fail, Some(HaCheckAction::RestartElsewhere));
        assert_eq!(
            s.nodes["b"],
            HaCheckTarget::Ping {
                addresses: vec!["10.0.1.1".parse().unwrap()],
                via: None,
                need: 1,
            }
        );
        assert_eq!(serde_json::to_value(&s).unwrap(), full);
        // `need`가 빠지면 1이다.
        let s = spec(json!({
            "role": "management",
            "target": { "kind": "ping", "addresses": ["10.0.0.1"] }
        }));
        assert!(matches!(s.target, HaCheckTarget::Ping { need: 1, .. }));
        let s = spec(json!({ "role": "storage", "target": { "kind": "pool", "name": "nfs1" } }));
        assert_eq!(
            s.target,
            HaCheckTarget::Pool {
                name: "nfs1".into()
            }
        );
    }

    #[test]
    fn unknown_fields_and_kinds_are_rejected() {
        let bad = [
            json!({ "role": "vm", "target": { "kind": "link", "iface": "a" }, "onfail": "warn" }),
            json!({ "role": "vm", "target": { "kind": "link", "iface": "a", "via": "b" } }),
            json!({ "role": "vm", "target": { "kind": "ping", "addresses": ["10.0.0.1"], "count": 1 } }),
            json!({ "role": "storage", "target": { "kind": "pool", "name": "p", "path": "/x" } }),
            json!({ "role": "vm", "target": { "kind": "arp", "iface": "a" } }),
            json!({ "role": "network", "target": { "kind": "link", "iface": "a" } }),
            json!({ "role": "vm", "target": { "kind": "link", "iface": "a" }, "on_fail": "reboot" }),
            json!({ "role": "vm", "target": { "kind": "ping", "addresses": ["host.example"] } }),
            json!({ "role": "vm", "target": { "kind": "link", "iface": "a" },
                    "nodes": { "b": { "kind": "link", "iface": "a", "x": 1 } } }),
        ];
        for b in bad {
            assert!(
                serde_json::from_value::<HaCheckSpec>(b.clone()).is_err(),
                "{b}"
            );
        }
    }

    #[test]
    fn validates_the_spec() {
        let link = |iface: &str| json!({ "kind": "link", "iface": iface });
        let ping = |addresses: serde_json::Value, need: u8| json!({ "kind": "ping", "addresses": addresses, "need": need });
        let check = |v: serde_json::Value| spec(v).validate();
        let ok = json!({ "role": "vm", "target": link("bond0") });
        assert_eq!(check(ok.clone()), Ok(()));

        let mut grace = ok.clone();
        grace["grace_secs"] = json!(0);
        assert_eq!(check(grace.clone()), Err(HaCheckError::Grace(0)));
        grace["grace_secs"] = json!(9);
        assert_eq!(check(grace.clone()), Err(HaCheckError::Grace(9)));
        assert_eq!(
            HaCheckError::Grace(9).to_string(),
            "grace_secs 9 is out of range: use 10-600"
        );
        grace["grace_secs"] = json!(10);
        assert_eq!(check(grace.clone()), Ok(()));
        grace["grace_secs"] = json!(601);
        assert_eq!(check(grace.clone()), Err(HaCheckError::Grace(601)));
        grace["grace_secs"] = json!(600);
        assert_eq!(check(grace), Ok(()));

        for bad in ["", "a/b", "a:1", "a b", "sixteen-chars-xx"] {
            assert_eq!(
                check(json!({ "role": "vm", "target": link(bad) })),
                Err(HaCheckError::Iface(bad.into())),
                "{bad}"
            );
        }
        assert_eq!(
            check(json!({ "role": "vm", "target": {
                "kind": "ping", "addresses": ["10.0.0.1"], "via": "a/b" } })),
            Err(HaCheckError::Iface("a/b".into()))
        );

        assert_eq!(
            check(json!({ "role": "vm", "target": ping(json!([]), 1) })),
            Err(HaCheckError::Addresses(0))
        );
        let nine: Vec<String> = (1..=9).map(|i| format!("10.0.0.{i}")).collect();
        assert_eq!(
            check(json!({ "role": "vm", "target": ping(json!(nine), 1) })),
            Err(HaCheckError::Addresses(9))
        );
        assert_eq!(
            check(json!({ "role": "vm", "target": ping(json!(nine[..8]), 8) })),
            Ok(())
        );
        assert_eq!(
            check(
                json!({ "role": "vm", "target": ping(json!(["10.0.0.1", "fd00::1", "10.0.0.1"]), 1) })
            ),
            Err(HaCheckError::DuplicateAddress("10.0.0.1".parse().unwrap()))
        );
        assert_eq!(
            check(json!({ "role": "vm", "target": ping(json!(["10.0.0.1"]), 0) })),
            Err(HaCheckError::Need {
                need: 0,
                addresses: 1
            })
        );
        assert_eq!(
            check(json!({ "role": "vm", "target": ping(json!(["10.0.0.1", "10.0.0.2"]), 3) })),
            Err(HaCheckError::Need {
                need: 3,
                addresses: 2
            })
        );

        let pool = json!({ "kind": "pool", "name": "nfs1" });
        assert_eq!(
            check(json!({ "role": "vm", "target": pool })),
            Err(HaCheckError::PoolRole(HaCheckRole::Vm))
        );
        assert_eq!(
            check(json!({ "role": "management", "target": pool })),
            Err(HaCheckError::PoolRole(HaCheckRole::Management))
        );
        assert_eq!(check(json!({ "role": "storage", "target": pool })), Ok(()));

        // 역할마다 둘 수 있는 대응
        let with = |role: &str, action: &str| {
            check(json!({ "role": role, "target": link("eth0"), "on_fail": action }))
        };
        let refused = |role, action| Err(HaCheckError::Action { role, action });
        assert_eq!(
            with("management", "migrate"),
            refused(HaCheckRole::Management, HaCheckAction::Migrate)
        );
        assert_eq!(
            with("management", "restart_elsewhere"),
            refused(HaCheckRole::Management, HaCheckAction::RestartElsewhere)
        );
        assert_eq!(
            with("storage", "migrate"),
            refused(HaCheckRole::Storage, HaCheckAction::Migrate)
        );
        for (role, action) in [
            ("management", "warn"),
            ("management", "fence"),
            ("storage", "warn"),
            ("storage", "restart_elsewhere"),
            ("storage", "fence"),
            ("vm", "warn"),
            ("vm", "migrate"),
            ("vm", "restart_elsewhere"),
            ("vm", "fence"),
        ] {
            assert_eq!(with(role, action), Ok(()), "{role} {action}");
        }

        // 노드 덮기는 같은 종류여야 하고 그 대상도 검증한다.
        assert_eq!(
            check(json!({ "role": "vm", "target": link("bond0"),
                          "nodes": { "b": ping(json!(["10.0.0.1"]), 1) } })),
            Err(HaCheckError::OverrideKind {
                node: "b".into(),
                want: "link",
                got: "ping"
            })
        );
        assert_eq!(
            check(json!({ "role": "vm", "target": link("bond0"), "nodes": { "b": link("a:b") } })),
            Err(HaCheckError::Iface("a:b".into()))
        );
        assert_eq!(
            check(
                json!({ "role": "vm", "target": link("bond0"), "nodes": { "b": link("bond1") } })
            ),
            Ok(())
        );
        // 문구에 값이 있다.
        let e = check(json!({ "role": "storage", "target": link("e"), "on_fail": "migrate" }))
            .unwrap_err()
            .to_string();
        assert_eq!(e, "on_fail migrate is not allowed for role storage");
    }

    #[test]
    fn action_defaults_to_the_roles_action() {
        let mut s = spec(json!({ "role": "storage", "target": { "kind": "link", "iface": "a" } }));
        assert_eq!(s.action(), HaCheckAction::RestartElsewhere);
        s.role = HaCheckRole::Vm;
        assert_eq!(s.action(), HaCheckAction::Migrate);
        s.role = HaCheckRole::Management;
        assert_eq!(s.action(), HaCheckAction::Warn);
        s.on_fail = Some(HaCheckAction::Fence);
        assert_eq!(s.action(), HaCheckAction::Fence);
        assert_eq!(
            serde_json::to_value(HaCheckAction::RestartElsewhere).unwrap(),
            "restart_elsewhere"
        );
    }

    #[test]
    fn target_for_uses_the_node_override() {
        let s = spec(json!({
            "role": "vm",
            "target": { "kind": "link", "iface": "bond0" },
            "nodes": { "b": { "kind": "link", "iface": "bond1" } }
        }));
        assert_eq!(
            s.target_for("a"),
            &HaCheckTarget::Link {
                iface: "bond0".into()
            }
        );
        assert_eq!(
            s.target_for("b"),
            &HaCheckTarget::Link {
                iface: "bond1".into()
            }
        );
    }

    #[test]
    fn reports_skip_empty_fields_and_cut_long_errors() {
        let ok = CheckReport::new("stor", CheckState::Ok, None, None);
        let json = serde_json::to_value(&ok).unwrap();
        assert_eq!(json, json!({ "name": "stor", "state": "ok" }));
        assert_eq!(serde_json::from_value::<CheckReport>(json).unwrap(), ok);
        let bad = CheckReport::new(
            "cluster-link-1",
            CheckState::Failing,
            Some("no interface eth9".into()),
            Some(1_793_000_000),
        );
        let json = serde_json::to_value(&bad).unwrap();
        assert_eq!(
            json,
            json!({ "name": "cluster-link-1", "state": "failing",
                    "error": "no interface eth9", "since": 1_793_000_000u64 })
        );
        assert_eq!(serde_json::from_value::<CheckReport>(json).unwrap(), bad);
        // 200바이트로 자르되 글자 중간을 자르지 않는다.
        let long = CheckReport::new("x", CheckState::Unknown, Some("가".repeat(100)), None);
        let error = long.error.unwrap();
        assert_eq!(error.len(), 198);
        assert!(error.chars().all(|c| c == '가'));
        assert_eq!(
            serde_json::to_value(CheckState::Degraded).unwrap(),
            "degraded"
        );
    }

    /// 뒤 버전이 더한 상태는 `unknown`으로 읽는다(옛 리더가 리스 갱신을 거절하지 않게).
    #[test]
    fn unknown_states_read_as_unknown() {
        let r: CheckReport =
            serde_json::from_value(json!({ "name": "x", "state": "flapping" })).unwrap();
        assert_eq!(r.state, CheckState::Unknown);
        assert_eq!(
            serde_json::from_value::<CheckState>(json!("failing")).unwrap(),
            CheckState::Failing
        );
    }
}
