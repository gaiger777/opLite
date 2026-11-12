use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::Arch;

/// 클러스터 노드. 리소스 id는 그 노드의 Raft 노드 id와 같다.
pub type Node = Resource<NodeSpec, NodeStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    /// API 주소 (`host:port`)
    pub address: String,
    /// 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다.
    #[serde(default)]
    pub maintenance: bool,
    /// 위트니스 노드: Raft 투표만 하고 VM을 돌리지 않는다. 합류할 때 정해지고 바뀌지 않는다.
    #[serde(default)]
    pub witness: bool,
    /// 이 노드의 역할. 없으면 기본 역할(위트니스가 아니면 `api`)이다. 옛 노드는 이 필드를 모르므로 기본이면
    /// 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub roles: Option<Vec<NodeRole>>,
}

/// 노드의 역할
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// 앞단(로드 밸런서·VIP)이 부르는 API 노드. 헬스 체크(`?ready`)는 이 역할이 없으면 503이다.
    Api,
}

impl NodeSpec {
    /// 이 노드의 역할. 정하지 않았으면 위트니스가 아닌 노드는 `api`, 위트니스는 없음이다.
    pub fn roles(&self) -> Vec<NodeRole> {
        match &self.roles {
            Some(roles) => roles.clone(),
            None if self.witness => Vec::new(),
            None => vec![NodeRole::Api],
        }
    }

    /// API 노드인지(위트니스는 API가 없으므로 늘 아니다)
    pub fn is_api(&self) -> bool {
        !self.witness && self.roles().contains(&NodeRole::Api)
    }
}

impl ResourceSpec for NodeSpec {
    const KIND: Kind = Kind::Node;
    type Status = NodeStatus;
}

/// 노드가 시작할 때 스스로 기록한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NodeStatus {
    pub online: bool,
    pub cpus: u32,
    pub memory_mib: u64,
    /// 예: `qemu-hvf`, `qemu-kvm`, `qemu-tcg`
    pub hypervisor: String,
    /// 이 노드에 있는 Linux 브리지. 스케줄러가 브리지 네트워크를 쓰는 VM을 배치할 때 본다.
    pub bridges: Vec<BridgeInfo>,
    /// 이 노드의 CPU 아키텍처. 옛 버전 노드는 기록하지 않아 None이다.
    pub arch: Option<Arch>,
    /// 이 노드가 쓸 수 있는 스토리지 풀(풀 이름 → 상태). 노드가 주기적으로 기록한다.
    pub pools: BTreeMap<String, PoolState>,
    /// 이 노드의 HA 펜싱. None이면 펜싱이 꺼져 HA VM을 돌리지 않는다(위트니스는 늘 None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    // 없으면 빼고 쓰므로 API 문서에서도 null·기본값이 아니라 선택 필드다.
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ha: Option<NodeHa>,
    /// VXLAN 패킷을 주고받는 이 노드의 주소. 없으면 이 노드에 VXLAN 장치를 만들지 않는다(위트니스는 늘
    /// None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ip", nullable = false, default)
    )]
    pub underlay: Option<IpAddr>,
    /// 이 노드에 VM 방화벽(nftables)이 있다. 보안 그룹을 건 NIC가 있는 VM은 이런 노드에서만 돈다
    /// (위트니스는 늘 false).
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub firewall: bool,
    /// 이 노드에서 도는 orbivisord의 빌드 버전. 노드가 등록할 때 쓴다(옛 버전 노드는 None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub version: Option<String>,
    /// 이 노드의 APT 업데이트 확인 결과. 업데이트가 켜진 노드는 등록할 때 `Some`(아직 확인 전이면
    /// 빈 값)이고, 꺼진 노드(위트니스, macOS 등)와 옛 노드는 None이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub updates: Option<NodeUpdates>,
    /// 이 노드의 orbivisord가 아는 기능(예: `node-drain`). 노드가 등록할 때 쓴다(옛 노드는 비어 있다). 새 기능을
    /// 쓰는 API는 모든 노드가 그 기능을 적었는지 본다(버전이 섞여도 된다).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub features: Vec<String>,
}

/// 이 빌드가 아는 기능. 노드가 [`NodeStatus::features`]에 적는다.
pub const FEATURES: &[&str] = &[
    "node-drain",
    "incremental-backup",
    "incremental-replication",
    "storage-migration",
    "metrics",
    "node-roles",
    "shared-sessions",
];

/// 노드의 APT 업데이트 확인 결과(`apt-get -s dist-upgrade`가 올릴 패키지)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NodeUpdates {
    /// 마지막으로 확인한 시각(유닉스 초). 아직 확인하지 않았으면 None
    pub checked_at: Option<u64>,
    /// 올릴 수 있는 패키지. 너무 많으면 앞의 것만 담고 `truncated`가 true다.
    pub packages: Vec<PackageUpdate>,
    #[serde(skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub truncated: bool,
    /// 확인이 실패한 이유(명령과 오류 출력 끝). 실패하면 `packages`는 비어 있다.
    pub error: Option<String>,
}

/// 올릴 수 있는 패키지 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackageUpdate {
    pub name: String,
    /// 설치된 버전. 새로 설치할 패키지(새 의존성)는 None이다.
    pub current: Option<String>,
    /// 올릴 버전
    pub candidate: String,
}

fn is_false(b: &bool) -> bool {
    !*b
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

/// 한 노드에서 본 스토리지 풀 상태
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct PoolState {
    /// 이 상태를 적을 때의 풀 id. 같은 이름으로 다시 만든 풀에 옛 상태를 쓰지 않게, 쓰는 쪽은 이 값이
    /// 풀 id와 같을 때만 믿는다. 이 필드 전의 상태는 nil이다.
    pub pool_id: Uuid,
    /// 이 노드에서 풀에 볼륨을 만들고 VM 디스크로 쓸 수 있는지
    pub ready: bool,
    /// 준비되지 않은 이유(예: 마운트 실패 메시지)
    pub error: Option<String>,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BridgeInfo {
    pub name: String,
    /// VLAN-aware 브리지(`vlan_filtering 1`)인지. VLAN 태그를 쓰는 네트워크에 필요하다.
    pub vlan_filtering: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_defaults_maintenance_and_status_defaults() {
        let spec: NodeSpec =
            serde_json::from_value(serde_json::json!({ "address": "10.0.0.1:8006" })).unwrap();
        assert!(!spec.maintenance);
        assert!(!spec.witness);
        let witness: NodeSpec = serde_json::from_value(
            serde_json::json!({ "address": "10.0.0.1:8006", "witness": true }),
        )
        .unwrap();
        assert!(witness.witness);
        let status: NodeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(status, NodeStatus::default());
        assert_eq!(status.ha, None);
    }

    #[test]
    fn roles_default_to_api_unless_witness_and_are_omitted_when_unset() {
        let spec: NodeSpec =
            serde_json::from_value(serde_json::json!({ "address": "10.0.0.1:8006" })).unwrap();
        assert_eq!(spec.roles, None);
        assert_eq!(spec.roles(), [NodeRole::Api]);
        assert!(spec.is_api());
        // 기본이면 쓰지 않는다(옛 노드가 읽을 수 있게).
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("roles").is_none(), "{json}");
        let witness = NodeSpec {
            witness: true,
            ..spec.clone()
        };
        assert!(witness.roles().is_empty());
        assert!(!witness.is_api());

        let none = NodeSpec {
            roles: Some(vec![]),
            ..spec.clone()
        };
        assert!(!none.is_api());
        let json = serde_json::to_value(&none).unwrap();
        assert_eq!(json["roles"], serde_json::json!([]));
        let api: NodeSpec = serde_json::from_value(
            serde_json::json!({ "address": "10.0.0.1:8006", "roles": ["api"] }),
        )
        .unwrap();
        assert_eq!(api.roles, Some(vec![NodeRole::Api]));
        assert!(api.is_api());
        // 위트니스는 `api`를 적어도 API 노드가 아니다.
        assert!(
            !NodeSpec {
                witness: true,
                ..api
            }
            .is_api()
        );
        assert!(
            serde_json::from_value::<NodeSpec>(
                serde_json::json!({ "address": "a", "roles": ["observer"] })
            )
            .is_err()
        );
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
    fn status_underlay_round_trips_and_is_omitted_when_none() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("underlay").is_none(), "{none}");
        for ip in ["10.0.0.11", "fd00::11"] {
            let status = NodeStatus {
                underlay: Some(ip.parse().unwrap()),
                ..Default::default()
            };
            let json = serde_json::to_value(&status).unwrap();
            assert_eq!(json["underlay"], ip);
            assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
        }
    }

    #[test]
    fn status_firewall_round_trips_and_is_omitted_when_false() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("firewall").is_none(), "{none}");
        let status = NodeStatus {
            firewall: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["firewall"], true);
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn status_version_round_trips_and_is_omitted_when_none() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("version").is_none(), "{none}");
        let status = NodeStatus {
            version: Some("0.2.0~ci7".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["version"], "0.2.0~ci7");
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn status_updates_round_trip_and_are_omitted_when_none() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("updates").is_none(), "{none}");
        // 켜졌지만 아직 확인하지 않은 노드
        let status = NodeStatus {
            updates: Some(NodeUpdates::default()),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["updates"],
            serde_json::json!({ "checked_at": null, "packages": [], "error": null })
        );
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
        let status = NodeStatus {
            updates: Some(NodeUpdates {
                checked_at: Some(1_793_000_000),
                packages: vec![
                    PackageUpdate {
                        name: "orbivisor".into(),
                        current: Some("0.2.0".into()),
                        candidate: "0.2.1".into(),
                    },
                    PackageUpdate {
                        name: "libnew1".into(),
                        current: None,
                        candidate: "1.0-1".into(),
                    },
                ],
                truncated: true,
                error: None,
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["updates"]["truncated"], true);
        assert_eq!(
            json["updates"]["packages"][1],
            serde_json::json!({ "name": "libnew1", "current": null, "candidate": "1.0-1" })
        );
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn status_features_round_trip_and_are_omitted_when_empty() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("features").is_none(), "{none}");
        let status = NodeStatus {
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["features"],
            serde_json::json!([
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions"
            ])
        );
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
        let mut node: Node = Resource::new(
            "n",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
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

    #[test]
    fn status_arch_is_optional() {
        // 옛 노드는 arch를 기록하지 않았다.
        let old: NodeStatus =
            serde_json::from_value(serde_json::json!({ "online": true, "cpus": 2 })).unwrap();
        assert_eq!(old.arch, None);
        assert!(old.pools.is_empty());
        let status = NodeStatus {
            arch: Some(Arch::X86_64),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["arch"], "x86_64");
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn status_pools_round_trip() {
        let mut status = NodeStatus::default();
        status.pools.insert(
            "nas".into(),
            PoolState {
                pool_id: Uuid::now_v7(),
                ready: false,
                error: Some("mount.nfs: access denied".into()),
                total_bytes: 0,
                available_bytes: 0,
            },
        );
        status.pools.insert(
            "local".into(),
            PoolState {
                pool_id: Uuid::now_v7(),
                ready: true,
                error: None,
                total_bytes: 100 << 30,
                available_bytes: 40 << 30,
            },
        );
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["pools"]["local"]["ready"], true);
        assert_eq!(json["pools"]["nas"]["error"], "mount.nfs: access denied");
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn pool_state_without_pool_id_reads_as_nil() {
        // pool_id를 적기 전의 상태. 어느 풀의 상태인지 모르므로 쓰는 쪽이 믿지 않는다.
        let old: PoolState = serde_json::from_value(serde_json::json!({ "ready": true })).unwrap();
        assert_eq!(old.pool_id, Uuid::nil());
        assert!(old.ready);
    }
}
