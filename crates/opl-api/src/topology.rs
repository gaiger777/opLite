//! 네트워크 토폴로지(계획 67 §12.3): `GET /network-topology` — 2.5D 그림 하나를 그리는 데 필요한 네트워크·라우터·
//! 게이트웨이·부하 분산기·유동 IP·VM을 한 응답으로. 읽을 수 없는 것은 빼고 `hidden`에 센다(계획 66 I-1·M-4의 감추기 —
//! 노드 이름은 루트 `cluster.read`가 없으면 비운다). `live`면 NB 게이트웨이 주인과 SB 건강 검사를 한 번씩 읽는다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use axum::extract::State;
use opl_types::authz::Privilege;
use opl_types::floating_ip::FloatingIpSpec;
use opl_types::folder::FolderSpec;
use opl_types::load_balancer::LoadBalancerSpec;
use opl_types::network::NetworkSpec;
use opl_types::network_migration::{MigrationPhase, MoveState, NetworkMigrationSpec};
use opl_types::node::NodeSpec;
use opl_types::router::RouterSpec;
use opl_types::vm::{VmPhase, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::Principal;
use crate::authz::AuthzObject;
use crate::error::{ApiError, Json, Query};
use crate::load_balancers::MemberHealth;
use crate::routing::sees_nodes;
use crate::state::AppState;

/// 네트워크의 모양
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TopoNetworkKind {
    Bridge,
    Vlan,
    Vxlan,
    /// 오버레이 SDN
    Sdn,
    /// SDN 공급자(물리 망)
    Provider,
}

/// 진행 중인 옮기기(원본 네트워크에)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoMigration {
    pub id: Uuid,
    pub name: String,
    /// 대상 네트워크 id(읽을 수 없거나 없으면 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub target: Option<Uuid>,
    pub phase: MigrationPhase,
    /// 옮긴 NIC 수
    pub moved: usize,
    /// 옮길 NIC 수(막힌 것 빼고)
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoNetwork {
    pub id: Uuid,
    pub name: String,
    pub kind: TopoNetworkKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub folder: Option<Uuid>,
    /// 서브넷 CIDR
    pub cidrs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub mtu: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub physnet: Option<String>,
    /// VLAN 태그(VLAN·공급자)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub vlan: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub migration: Option<TopoMigration>,
}

/// 라우터 게이트웨이의 지금 모양
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayState {
    /// 게이트웨이 포트를 노드가 가졌다(`node`).
    Active,
    /// 바깥이 있는데 게이트웨이 후보가 없다.
    NoCandidate,
    /// 바깥이 없거나 NB를 읽지 않았다.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoGateway {
    /// 게이트웨이 포트를 가진 노드 이름(루트 `cluster.read`가 없으면 빈 글)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub node: Option<String>,
    pub state: GatewayState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoExternal {
    /// 공급자 네트워크 id
    pub network: Uuid,
    #[schema(value_type = Vec<String>)]
    pub ips: Vec<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoRouter {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub folder: Option<Uuid>,
    /// 인터페이스 네트워크 id(읽을 수 있는 것만)
    pub interfaces: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub external: Option<TopoExternal>,
    pub gateway: TopoGateway,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoLbMember {
    /// 멤버 VM(읽을 수 있을 때만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub vm: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, format = "ipv4", nullable = false, default)]
    pub ip: Option<Ipv4Addr>,
    pub health: MemberHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoLb {
    pub id: Uuid,
    pub name: String,
    /// VIP 네트워크 id
    pub network: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, format = "ipv4", nullable = false, default)]
    pub vip: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, format = "ipv4", nullable = false, default)]
    pub external_ip: Option<Ipv4Addr>,
    pub members: Vec<TopoLbMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoFip {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, format = "ipv4", nullable = false, default)]
    pub address: Option<Ipv4Addr>,
    /// 공급자 네트워크 id
    pub network: Uuid,
    /// 붙은 VM(읽을 수 있을 때만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub vm: Option<Uuid>,
    /// `unattached`·`distributed`·`centralized`·`unrouted`
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoNic {
    pub network: Uuid,
    #[schema(value_type = Vec<String>)]
    pub ips: Vec<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoVm {
    pub id: Uuid,
    pub name: String,
    pub power: VmPhase,
    /// 도는 노드 이름(루트 `cluster.read`가 없으면 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub node: Option<String>,
    /// 그림에 있는 네트워크의 NIC
    pub nics: Vec<TopoNic>,
}

/// 읽을 수 없어 뺀 수
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopoHidden {
    pub networks: usize,
    pub routers: usize,
    pub load_balancers: usize,
    pub floating_ips: usize,
    pub vms: usize,
}

/// `GET /network-topology`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NetworkTopology {
    pub networks: Vec<TopoNetwork>,
    pub routers: Vec<TopoRouter>,
    pub load_balancers: Vec<TopoLb>,
    pub floating_ips: Vec<TopoFip>,
    pub vms: Vec<TopoVm>,
    pub hidden: TopoHidden,
}

/// 질의
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct TopologyQuery {
    /// 이 네트워크 폴더(하위 포함)의 네트워크·라우터·부하 분산기·유동 IP만
    #[serde(default)]
    pub folder: Option<Uuid>,
    /// 참이면 NB 게이트웨이 주인과 SB 건강 검사를 읽는다(기본 거짓 — 화면은 watch로 다시 읽고 살아 있는 상태는 가끔).
    #[serde(default)]
    pub live: bool,
}

/// 폴더 `root`와 그 아래 폴더들(부모를 따라 올라가 `root`를 만나는 폴더)
fn subtree(folders: &[opl_types::folder::Folder], root: Uuid) -> BTreeSet<Uuid> {
    let parent: BTreeMap<Uuid, Option<Uuid>> =
        folders.iter().map(|f| (f.id, f.spec.parent)).collect();
    folders
        .iter()
        .map(|f| f.id)
        .filter(|id| {
            let mut at = Some(*id);
            let mut seen = BTreeSet::new();
            while let Some(f) = at {
                if f == root {
                    return true;
                }
                if !seen.insert(f) {
                    return false;
                }
                at = parent.get(&f).copied().flatten();
            }
            false
        })
        .collect()
}

fn network_kind(spec: &NetworkSpec) -> TopoNetworkKind {
    if spec.is_provider() {
        TopoNetworkKind::Provider
    } else if spec.is_sdn() {
        TopoNetworkKind::Sdn
    } else if spec.vxlan.is_some() {
        TopoNetworkKind::Vxlan
    } else if spec.vlan.is_some() {
        TopoNetworkKind::Vlan
    } else {
        TopoNetworkKind::Bridge
    }
}

/// 네트워크·라우터·게이트웨이·부하 분산기·유동 IP·VM을 한 그림으로(계획 67 §12.3). 읽을 수 있는 것만 담고 나머지는
/// `hidden`에 센다. `folder`면 그 네트워크 폴더(하위 포함)의 것과 그 네트워크에 NIC가 있는 VM만.
#[utoipa::path(get, path = "/network-topology", tag = "networks", params(TopologyQuery),
    responses((status = 200, body = NetworkTopology), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn network_topology(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(query): Query<TopologyQuery>,
) -> Result<Json<NetworkTopology>, ApiError> {
    let c = &state.cluster;
    let reads = |o: &dyn AuthzObjectRef| {
        state
            .authz
            .allows(&principal, Privilege::SystemRead, o.target())
    };
    let within: Option<BTreeSet<Uuid>> = match query.folder {
        Some(root) => Some(subtree(&c.list::<FolderSpec>()?.1, root)),
        None => None,
    };
    let in_scope = |folder: Option<Uuid>| {
        within
            .as_ref()
            .is_none_or(|w| folder.is_some_and(|f| w.contains(&f)))
    };
    let mut out = NetworkTopology::default();
    let sees_nodes = sees_nodes(&state, &principal);
    let nodes = c.list::<NodeSpec>()?.1;
    let node_name = |id: Uuid| {
        sees_nodes.then(|| {
            nodes
                .iter()
                .find(|n| n.id == id)
                .map_or_else(|| id.to_string(), |n| n.name.clone())
        })
    };

    // 네트워크: 범위 안의 것과, 범위 안의 라우터·부하 분산기·유동 IP가 가리키는 것(폴더로 거를 때 그림이 끊기지 않게)
    let referenced: BTreeSet<String> = if within.is_some() {
        let mut r = BTreeSet::new();
        for x in c
            .list::<RouterSpec>()?
            .1
            .iter()
            .filter(|x| in_scope(x.spec.folder))
        {
            r.extend(x.spec.interfaces.iter().map(|i| i.network.clone()));
            r.extend(x.spec.external.iter().map(|e| e.network.clone()));
        }
        for x in c
            .list::<LoadBalancerSpec>()?
            .1
            .iter()
            .filter(|x| in_scope(x.spec.folder))
        {
            r.insert(x.spec.network.clone());
            r.extend(x.spec.external.iter().map(|e| e.network.clone()));
        }
        for x in c
            .list::<FloatingIpSpec>()?
            .1
            .iter()
            .filter(|x| in_scope(x.spec.folder))
        {
            r.insert(x.spec.network.clone());
        }
        r
    } else {
        BTreeSet::new()
    };
    let networks: Vec<_> = c
        .list::<NetworkSpec>()?
        .1
        .into_iter()
        .filter(|n| in_scope(n.spec.folder) || referenced.contains(&n.name))
        .collect();
    let mut shown: BTreeMap<String, Uuid> = BTreeMap::new();
    let migrations = c.list::<NetworkMigrationSpec>()?.1;
    for n in &networks {
        if !reads(n) {
            out.hidden.networks += 1;
            continue;
        }
        shown.insert(n.name.clone(), n.id);
    }
    for n in networks.iter().filter(|n| shown.contains_key(&n.name)) {
        let migration = migrations
            .iter()
            .filter(|m| !m.status.is_done() && m.spec.source == n.name)
            .find(|m| reads(*m))
            .map(|m| TopoMigration {
                id: m.id,
                name: m.name.clone(),
                target: shown.get(&m.spec.target).copied(),
                phase: m.status.phase,
                moved: m
                    .status
                    .vms
                    .iter()
                    .filter(|v| v.state == MoveState::Moved)
                    .count(),
                total: m
                    .status
                    .vms
                    .iter()
                    .filter(|v| v.mode != opl_types::network_migration::PlannedMode::Blocked)
                    .count(),
            });
        out.networks.push(TopoNetwork {
            id: n.id,
            name: n.name.clone(),
            kind: network_kind(&n.spec),
            folder: n.spec.folder,
            cidrs: n.spec.subnets.iter().map(|s| s.cidr.to_string()).collect(),
            mtu: n.spec.mtu,
            physnet: n.spec.physnet().map(String::from),
            vlan: n.spec.vlan,
            migration,
        });
    }
    let net_id = |name: &str| shown.get(name).copied();

    // 라우터
    let routers: Vec<_> = c
        .list::<RouterSpec>()?
        .1
        .into_iter()
        .filter(|r| in_scope(r.spec.folder))
        .collect();
    let (visible, hidden): (Vec<_>, Vec<_>) = routers.into_iter().partition(|r| reads(r));
    out.hidden.routers = hidden.len();
    let hosting = if query.live && visible.iter().any(|r| r.spec.external.is_some()) {
        crate::routing::hosting_all(&state).await.ok()
    } else {
        None
    };
    let no_gateway = crate::routing::routers_without_gateway(&state, &visible)?;
    for r in &visible {
        let host = hosting
            .as_ref()
            .and_then(|h| h.get(&r.id))
            .and_then(|c| c.parse::<Uuid>().ok());
        let gateway = match (&r.spec.external, host) {
            (Some(_), Some(node)) => TopoGateway {
                node: Some(node_name(node).unwrap_or_default()),
                state: GatewayState::Active,
            },
            (Some(_), None) if no_gateway.contains(&r.id) => TopoGateway {
                node: None,
                state: GatewayState::NoCandidate,
            },
            _ => TopoGateway {
                node: None,
                state: GatewayState::Unknown,
            },
        };
        out.routers.push(TopoRouter {
            id: r.id,
            name: r.name.clone(),
            folder: r.spec.folder,
            interfaces: r
                .spec
                .interfaces
                .iter()
                .filter_map(|i| net_id(&i.network))
                .collect(),
            external: r.spec.external.as_ref().and_then(|e| {
                Some(TopoExternal {
                    network: net_id(&e.network)?,
                    ips: e.ips.clone(),
                })
            }),
            gateway,
        });
    }

    // VM(읽을 수 있는 것, 그림의 네트워크에 NIC가 있는 것)
    let vms = c.list::<VmSpec>()?.1;
    let mut readable_vms = BTreeSet::new();
    for vm in vms.iter().filter(|v| !v.spec.template) {
        let nics: Vec<TopoNic> = vm
            .spec
            .nics
            .iter()
            .filter_map(|n| {
                Some(TopoNic {
                    network: net_id(&n.network)?,
                    ips: n.ips.clone(),
                })
            })
            .collect();
        let on_networks = vm
            .spec
            .nics
            .iter()
            .any(|n| networks.iter().any(|x| x.name == n.network));
        if !on_networks {
            continue;
        }
        if !reads(vm) {
            out.hidden.vms += 1;
            continue;
        }
        readable_vms.insert(vm.id);
        out.vms.push(TopoVm {
            id: vm.id,
            name: vm.name.clone(),
            power: vm.status.phase,
            node: vm
                .status
                .node
                .filter(|_| vm.status.phase != VmPhase::Stopped)
                .and_then(node_name),
            nics,
        });
    }

    // 부하 분산기
    let lbs: Vec<_> = c
        .list::<LoadBalancerSpec>()?
        .1
        .into_iter()
        .filter(|l| in_scope(l.spec.folder))
        .collect();
    let (visible, hidden): (Vec<_>, Vec<_>) = lbs.into_iter().partition(|l| reads(l));
    out.hidden.load_balancers = hidden.len();
    for v in crate::load_balancers::views(&state, &principal, visible, query.live).await? {
        let Some(network) = net_id(&v.lb.spec.network) else {
            continue;
        };
        out.load_balancers.push(TopoLb {
            id: v.lb.id,
            name: v.lb.name.clone(),
            network,
            vip: v.lb.spec.vip,
            external_ip: v.lb.spec.external.as_ref().and_then(|e| e.ip),
            members: v
                .members
                .iter()
                .map(|m| TopoLbMember {
                    vm: m.vm.filter(|id| readable_vms.contains(id)),
                    ip: m.ip,
                    health: m.health,
                })
                .collect(),
        });
    }

    // 유동 IP
    let fips: Vec<_> = c
        .list::<FloatingIpSpec>()?
        .1
        .into_iter()
        .filter(|f| in_scope(f.spec.folder))
        .collect();
    let (visible, hidden): (Vec<_>, Vec<_>) = fips.into_iter().partition(|f| reads(f));
    out.hidden.floating_ips = hidden.len();
    let states = crate::routing::fip_state_names(&state, &principal, &visible)?;
    for f in &visible {
        let Some(network) = net_id(&f.spec.network) else {
            continue;
        };
        out.floating_ips.push(TopoFip {
            id: f.id,
            address: f.spec.address.and_then(|a| match a {
                IpAddr::V4(v) => Some(v),
                IpAddr::V6(_) => None,
            }),
            network,
            vm: f
                .spec
                .target
                .map(|t| t.vm)
                .filter(|id| readable_vms.contains(id)),
            state: states.get(&f.id).copied().unwrap_or("unrouted").to_string(),
        });
    }
    Ok(Json(out))
}

/// 객체 하나의 평가 대상(여러 리소스 타입을 한 닫힘에 넘기려고)
trait AuthzObjectRef {
    fn target(&self) -> opl_types::authz_eval::AuthzTarget;
}

impl<T: AuthzObject> AuthzObjectRef for T {
    fn target(&self) -> opl_types::authz_eval::AuthzTarget {
        self.authz_target()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::resource::Resource;

    #[test]
    fn subtrees_follow_parents_and_stop_on_cycles() {
        let folder = |name: &str, parent: Option<Uuid>| {
            let mut f: opl_types::folder::Folder = Resource::new(
                name,
                serde_json::from_value(serde_json::json!({ "name": name, "kind": "network" }))
                    .unwrap(),
            );
            f.spec.parent = parent;
            f
        };
        let a = folder("a", None);
        let b = folder("b", Some(a.id));
        let c = folder("c", Some(b.id));
        let d = folder("d", None);
        let mut e = folder("e", None);
        let f = folder("f", Some(e.id));
        e.spec.parent = Some(f.id);
        let all = vec![a.clone(), b.clone(), c.clone(), d, e, f];
        assert_eq!(subtree(&all, b.id), BTreeSet::from([b.id, c.id]));
        assert_eq!(subtree(&all, a.id).len(), 3);
    }

    #[test]
    fn network_kinds() {
        let spec = |v: serde_json::Value| -> NetworkSpec { serde_json::from_value(v).unwrap() };
        assert_eq!(
            network_kind(&spec(serde_json::json!({ "bridge": "br0" }))),
            TopoNetworkKind::Bridge
        );
        assert_eq!(
            network_kind(&spec(serde_json::json!({ "bridge": "br0", "vlan": 10 }))),
            TopoNetworkKind::Vlan
        );
        assert_eq!(
            network_kind(&spec(serde_json::json!({ "sdn": {} }))),
            TopoNetworkKind::Sdn
        );
    }
}
