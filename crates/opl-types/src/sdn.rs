//! SDN(OVN) 설치·구성(계획 65): Orbivisor 클러스터 하나에 Orbivisor가 노드에 설치·구성하는 OVN 배포 하나.
//! `Sdn` 리소스(이름 `sdn` 하나뿐)가 중앙 노드(NB·SB ovsdb 클러스터와 northd)와 리더 `OvnSync`의 동기화 기록을
//! 담는다. 노드마다의 chassis 상태는 [`NodeSdn`](`NodeStatus.sdn`)이다. 원하는 논리 스위치·포트는 네트워크·VM
//! 리소스에 있고 NB는 그 파생물이다.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::node::Node;
use crate::resource::{Kind, Resource, ResourceSpec};

pub type Sdn = Resource<SdnSpec, SdnStatus>;

/// `Sdn` 리소스의 이름(클러스터에 하나뿐)
pub const SDN_NAME: &str = "sdn";
/// 중앙 노드 수의 상한
pub const MAX_CENTRALS: usize = 5;
/// NB ovsdb의 TLS 포트
pub const NB_PORT: u16 = 6641;
/// SB ovsdb의 TLS 포트
pub const SB_PORT: u16 = 6642;
/// NB ovsdb 클러스터의 Raft 포트
pub const NB_RAFT_PORT: u16 = 6643;
/// SB ovsdb 클러스터의 Raft 포트
pub const SB_RAFT_PORT: u16 = 6644;
/// Geneve UDP 포트(IANA)
pub const GENEVE_PORT: u16 = 6081;
/// SDN 네트워크의 기본 MTU: underlay 1500에서 OVN Geneve 머리(`max_header_size` 38 + 바깥 IPv4 20)를 뺀 값
pub const DEFAULT_SDN_MTU: u16 = 1442;
/// 구성할 수 있는 OVN의 가장 낮은 판(24.03 LTS)
pub const MIN_OVN: (u32, u32) = (24, 3);

/// 지금은 칸이 없다(계획 66이 게이트웨이 노드를 더한다).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdnSpec {}

impl ResourceSpec for SdnSpec {
    const KIND: Kind = Kind::Sdn;
    type Status = SdnStatus;
}

/// Task가 중앙을 실제로 세웠음을 확인한 뒤에만 중앙을 더한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SdnStatus {
    pub phase: SdnPhase,
    /// NB·SB ovsdb 클러스터와 northd를 돌리는 노드
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub centrals: Vec<SdnCentral>,
    /// 설치한 OVN의 판(예: `24.03.4`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// 처음 세우는(세운) 노드
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_node: Option<Uuid>,
    /// 처음 세우기가 실패한 이유
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 도는(돌) 중앙 Task(`sdn-init`·`sdn-central-add`·`sdn-central-remove`)의 자리. 클러스터 전체에서 한 번에 하나다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<SdnPending>,
    /// 리더 `OvnSync`의 마지막 NB 동기화. 바뀔 때만 쓴다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SdnSync>,
    /// OVN NB Raft 클러스터 id(`sdn-init`이 DB를 만들며 적는다). 노드에 남은 DB가 이 클러스터 것인지 본다(§7.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nb_cid: Option<Uuid>,
    /// OVN SB Raft 클러스터 id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sb_cid: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SdnPhase {
    /// 첫 중앙을 세우는 중(실패했으면 다시 할 수 있다)
    #[default]
    Bootstrapping,
    /// NB·SB에 리더가 있다. 중앙·chassis 더하기와 SDN 네트워크 만들기를 할 수 있다.
    Ready,
}

/// 중앙 노드 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnCentral {
    pub node: Uuid,
    /// NB·SB·Raft 포트가 듣는 주소(노드 클러스터 주소 하나)
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub address: IpAddr,
}

/// 클러스터 전체에서 한 번에 하나만 도는 중앙 Task의 자리. API가 Task를 만들기 전에 리비전 확인 쓰기로 잡고, Task가
/// 끝나면 지운다(`CephPending`과 같다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdnPending {
    pub op: crate::task::TaskOp,
    /// Task를 돌리는 노드
    pub node: Uuid,
    pub task: Uuid,
}

/// 리더 `OvnSync`가 NB에 쓴 마지막 기록
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct SdnSync {
    /// 쓴 리더의 Raft 임기(펜싱 토큰의 앞)
    pub term: u64,
    /// 읽은 Raft 로그 인덱스(펜싱 토큰의 뒤)
    pub index: u64,
    /// 마지막으로 맞춘 시각(유닉스 초). 성공이 이어지면 60초 넘게 낡을 때만 바꾼다.
    pub at: u64,
    /// 마지막 차례가 실패한 이유
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// NB `NB_Global.nb_cfg`(쓸 때마다 1 올림)
    pub nb_cfg: i64,
    /// NB `NB_Global.hv_cfg`: 모든 chassis가 반영한 `nb_cfg`
    pub hv_cfg: i64,
}

/// 노드의 SDN 상태(`NodeStatus.sdn`). 노드의 SDN 에이전트가 바뀔 때만, 모든 노드가 `sdn`을 알릴 때만 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NodeSdn {
    pub chassis: ChassisState,
    /// `ovn-controller --version`의 판
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ovn_version: Option<String>,
    /// `ovs-vsctl --version`의 판
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ovs_version: Option<String>,
    /// Geneve 터널의 이 노드 주소(`ovn-encap-ip`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ip", nullable = false, default)
    )]
    pub encap_ip: Option<IpAddr>,
    /// 이 노드가 중앙이면 NB·SB Raft 역할과 northd
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub central: Option<CentralHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ChassisState {
    /// chassis로 구성하지 않았다.
    #[default]
    Missing,
    /// 구성했지만 SB에 붙지 않았거나 SB에 이 노드의 `Chassis` 행이 아직 없다.
    Configuring,
    /// SB에 붙었고 `Chassis` 행이 있다. SDN NIC가 있는 VM을 돌릴 수 있다.
    Ready,
    /// ovn-controller가 돌지 않는다.
    Down,
    /// 이 빌드가 모르는 값(뒤 버전이 더한 상태)
    #[serde(other)]
    Unknown,
}

/// 중앙 노드의 ovsdb Raft 역할과 northd
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CentralHealth {
    pub nb: RaftRole,
    pub sb: RaftRole,
    /// 이 노드의 northd가 SB 잠금을 가져 일하고 있다(나머지는 대기).
    #[serde(default)]
    pub northd_active: bool,
}

/// ovsdb 클러스터에서 이 서버의 역할(`cluster/status`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RaftRole {
    Leader,
    Follower,
    Candidate,
    /// 클러스터에 붙는 중(`join-cluster` 뒤 따라잡기 전)
    Joining,
    /// 서버가 돌지 않거나 답하지 않는다.
    Down,
    /// 이 빌드가 모르는 값
    #[serde(other)]
    Unknown,
}

/// `POST /sdn`: 노드 `node`에 첫 중앙을 세운다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SdnInitRequest {
    /// 이름이나 id
    pub node: String,
    /// NB·SB가 들을 주소. 없으면 노드 주소의 IP
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ip", nullable = false, default)
    )]
    pub address: Option<IpAddr>,
}

/// `POST /sdn/centrals`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SdnNodeRequest {
    /// 이름이나 id
    pub node: String,
}

/// `POST /sdn/chassis`: `nodes`의 노드나(`all`이면) chassis가 아닌 모든 Linux 노드에 chassis를 설치한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SdnChassisRequest {
    /// 이름이나 id
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nodes: Vec<String>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub all: bool,
}

impl SdnChassisRequest {
    /// `nodes`와 `all` 가운데 정확히 하나
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.nodes.is_empty() == self.all {
            Ok(())
        } else {
            Err("give either nodes or all")
        }
    }
}

/// `GET /sdn`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnView {
    /// 없으면 SDN을 아직 설정하지 않았다(`sdn init` 전).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub phase: Option<SdnPhase>,
    pub centrals: Vec<SdnCentralView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub release: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub init_node: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub sync: Option<SdnSync>,
    /// 위트니스가 아닌 노드마다(이름 순)의 chassis 상태
    pub chassis: Vec<SdnChassisView>,
    /// 끝나지 않은 SDN Task
    pub tasks: Vec<Uuid>,
}

/// 중앙 하나와 그 노드가 알린 Raft 역할
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnCentralView {
    pub node: Uuid,
    /// 노드 이름(노드가 없어졌으면 id)
    pub name: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub address: IpAddr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub health: Option<CentralHealth>,
}

/// 노드 하나의 chassis 상태
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnChassisView {
    pub node: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub sdn: Option<NodeSdn>,
}

/// 노드 `node`의 OVN 중앙 주소(NB·SB가 듣고 Raft가 쓰는 주소): 노드 주소(`NodeSpec.address`)의 호스트가 IP면 그것 —
/// 노드 인증서의 SAN이다 — 아니면 underlay(`NodeStatus.underlay`). 둘 다 없으면 None
pub fn central_address(node: &Node) -> Option<IpAddr> {
    crate::ceph::address_ip(&node.spec.address).or(node.status.underlay)
}

impl SdnView {
    /// `sdn`이 없으면 설정 전 모양(중앙 없음)이다. 노드 표는 설정 전에도 채운다.
    pub fn new(sdn: Option<&Sdn>, nodes: &[Node], tasks: Vec<Uuid>) -> Self {
        let node = |id: Uuid| nodes.iter().find(|n| n.id == id);
        let centrals = sdn
            .map(|s| s.status.centrals.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|c| SdnCentralView {
                node: c.node,
                name: node(c.node).map_or_else(|| c.node.to_string(), |n| n.name.clone()),
                address: c.address,
                health: node(c.node)
                    .and_then(|n| n.status.sdn.as_ref())
                    .and_then(|s| s.central),
            })
            .collect();
        let mut chassis: Vec<SdnChassisView> = nodes
            .iter()
            .filter(|n| !n.spec.witness)
            .map(|n| SdnChassisView {
                node: n.id,
                name: n.name.clone(),
                sdn: n.status.sdn.clone(),
            })
            .collect();
        chassis.sort_by(|a, b| a.name.cmp(&b.name));
        let status = sdn.map(|s| &s.status);
        Self {
            phase: status.map(|s| s.phase),
            centrals,
            release: status.and_then(|s| s.release.clone()),
            init_node: status.and_then(|s| s.init_node),
            last_error: status.and_then(|s| s.last_error.clone()),
            sync: status.and_then(|s| s.sync.clone()),
            chassis,
            tasks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeSpec, NodeStatus};
    use serde_json::json;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn kind_and_empty_shapes() {
        assert_eq!(SdnSpec::KIND, Kind::Sdn);
        assert_eq!(Kind::Sdn.as_str(), "sdn");
        assert_eq!(serde_json::to_value(Kind::Sdn).unwrap(), "sdn");
        assert_eq!(serde_json::to_value(SdnSpec {}).unwrap(), json!({}));
        assert!(serde_json::from_value::<SdnSpec>(json!({ "gateways": [] })).is_err());
        let json = serde_json::to_value(SdnStatus::default()).unwrap();
        assert_eq!(json, json!({ "phase": "bootstrapping" }));
        assert_eq!(
            serde_json::from_value::<SdnStatus>(json!({})).unwrap(),
            SdnStatus::default()
        );
    }

    #[test]
    fn status_round_trips() {
        let n = Uuid::now_v7();
        let status = SdnStatus {
            phase: SdnPhase::Ready,
            centrals: vec![SdnCentral {
                node: n,
                address: ip("10.0.0.1"),
            }],
            release: Some("24.03.4".into()),
            init_node: Some(n),
            last_error: None,
            pending: Some(SdnPending {
                op: crate::task::TaskOp::SdnCentralAdd,
                node: n,
                task: Uuid::now_v7(),
            }),
            sync: Some(SdnSync {
                term: 3,
                index: 120,
                at: 1_700_000_000,
                error: None,
                nb_cfg: 7,
                hv_cfg: 6,
            }),
            nb_cid: Some(Uuid::now_v7()),
            sb_cid: None,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["centrals"][0]["address"], "10.0.0.1");
        assert!(json.get("sb_cid").is_none(), "{json}");
        assert_eq!(json["pending"]["op"], json!({ "kind": "sdn-central-add" }));
        assert!(json["sync"].get("error").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<SdnStatus>(json).unwrap(), status);
    }

    #[test]
    fn node_sdn_writes_only_what_it_has_and_reads_unknown_states() {
        assert_eq!(
            serde_json::to_value(NodeSdn::default()).unwrap(),
            json!({ "chassis": "missing" })
        );
        let full = NodeSdn {
            chassis: ChassisState::Ready,
            ovn_version: Some("24.03.4".into()),
            ovs_version: Some("3.3.0".into()),
            encap_ip: Some(ip("10.0.0.2")),
            central: Some(CentralHealth {
                nb: RaftRole::Leader,
                sb: RaftRole::Follower,
                northd_active: true,
            }),
            error: None,
        };
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["central"]["nb"], "leader");
        assert_eq!(serde_json::from_value::<NodeSdn>(json).unwrap(), full);
        // 뒤 버전이 더한 값은 모름으로 읽는다(노드 상태 전체를 못 읽게 되지 않게).
        let later: NodeSdn = serde_json::from_value(json!({
            "chassis": "draining",
            "central": { "nb": "learner", "sb": "leader" }
        }))
        .unwrap();
        assert_eq!(later.chassis, ChassisState::Unknown);
        let central = later.central.unwrap();
        assert_eq!(central.nb, RaftRole::Unknown);
        assert!(!central.northd_active);
    }

    #[test]
    fn requests_parse() {
        let r: SdnInitRequest = serde_json::from_value(json!({ "node": "n1" })).unwrap();
        assert_eq!(r.address, None);
        let r: SdnInitRequest =
            serde_json::from_value(json!({ "node": "n1", "address": "10.0.0.1" })).unwrap();
        assert_eq!(r.address, Some(ip("10.0.0.1")));
        assert!(serde_json::from_value::<SdnNodeRequest>(json!({ "node": "n1", "x": 1 })).is_err());
        let ok = |v| {
            serde_json::from_value::<SdnChassisRequest>(v)
                .unwrap()
                .validate()
        };
        assert_eq!(ok(json!({ "nodes": ["n1", "n2"] })), Ok(()));
        assert_eq!(ok(json!({ "all": true })), Ok(()));
        assert!(ok(json!({})).is_err());
        assert!(ok(json!({ "nodes": ["n1"], "all": true })).is_err());
    }

    fn node(name: &str, witness: bool, sdn: Option<NodeSdn>) -> Node {
        let mut n = Resource::new(
            name,
            NodeSpec {
                address: format!("{name}:8006"),
                maintenance: false,
                witness,
                roles: None,
                host_resources: None,
            },
        );
        n.status = NodeStatus {
            sdn,
            ..NodeStatus::default()
        };
        n
    }

    /// 중앙 주소: 노드 주소의 IP, 이름이면 underlay, 둘 다 없으면 None
    #[test]
    fn central_addresses() {
        let mut n = node("n1", false, None);
        assert_eq!(central_address(&n), None);
        n.status.underlay = Some(ip("10.1.0.5"));
        assert_eq!(central_address(&n), Some(ip("10.1.0.5")));
        n.spec.address = "10.0.0.1:8006".into();
        assert_eq!(central_address(&n), Some(ip("10.0.0.1")));
        n.spec.address = "[fd00::1]:8006".into();
        assert_eq!(central_address(&n), Some(ip("fd00::1")));
    }

    #[test]
    fn view_before_and_after_init() {
        let central = NodeSdn {
            chassis: ChassisState::Ready,
            central: Some(CentralHealth {
                nb: RaftRole::Leader,
                sb: RaftRole::Leader,
                northd_active: true,
            }),
            ..NodeSdn::default()
        };
        let nodes = vec![
            node("n2", false, None),
            node("n1", false, Some(central.clone())),
            node("w", true, None),
        ];
        let before = SdnView::new(None, &nodes, Vec::new());
        assert_eq!(before.phase, None);
        assert!(before.centrals.is_empty());
        let names: Vec<_> = before.chassis.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["n1", "n2"]);
        let json = serde_json::to_value(&before).unwrap();
        assert!(json.get("phase").is_none(), "{json}");

        let gone = Uuid::now_v7();
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec {});
        sdn.status.phase = SdnPhase::Ready;
        sdn.status.centrals = vec![
            SdnCentral {
                node: nodes[1].id,
                address: ip("10.0.0.1"),
            },
            SdnCentral {
                node: gone,
                address: ip("10.0.0.9"),
            },
        ];
        let after = SdnView::new(Some(&sdn), &nodes, vec![gone]);
        assert_eq!(after.phase, Some(SdnPhase::Ready));
        assert_eq!(after.centrals[0].name, "n1");
        assert_eq!(after.centrals[0].health, central.central);
        assert_eq!(after.centrals[1].name, gone.to_string());
        assert_eq!(after.centrals[1].health, None);
        assert_eq!(after.chassis[0].sdn, Some(central));
    }
}
