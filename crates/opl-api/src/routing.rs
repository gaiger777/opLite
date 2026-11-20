//! SDN 라우터와 유동 IP의 API(계획 66 §9·§13): `/routers`, `/floating-ips`. 원하는 상태만 Raft에 쓰고 리더 `OvnSync`가 NB로
//! 옮긴다. 라우터 외부 주소와 유동 IP는 공급자 네트워크의 주소를 API가 고른다(`floating_pools`가 있으면 거기서만, 없으면
//! 풀 — §8). 두 API 노드가 같은 주소를 고르면 Raft의 겹침 키 `("ip", "<네트워크>/<주소>")`가 하나를 거절하고 다시 고른다.
//!
//! 권한(§12): 라우터·유동 IP는 네트워크 폴더에 산다. 쓰기는 그 객체(만들기는 폴더)의 `network.config`, 인터페이스로 잇는
//! 네트워크는 그 네트워크의 `network.config`(그 네트워크 VM 모두의 기본 경로가 바뀐다 — 리뷰 1 #1), 외부 네트워크와 유동
//! IP의 네트워크는 `network.assign`, 유동 IP를 붙이는 VM은 그 VM의 `vm.config`다. 유동 IP 풀(없으면 풀) 밖의 주소를
//! 고르는 것은 루트 `cluster.admin`만이다. 보기는 `system.read`이고, 읽을 수 없는 VM(다른 폴더)의 이름과 루트
//! `cluster.read`가 없는 사용자에게 노드 이름은 보이지 않는다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::authz::Privilege;
use opl_types::authz_eval::AuthzTarget;
use opl_types::floating_ip::{FipTarget, FloatingIp, FloatingIpSpec, FloatingIpStatus};
use opl_types::ipam;
use opl_types::network::{Network, NetworkSpec};
use opl_types::node::{Node, NodeSpec, SDN_ROUTING};
use opl_types::resource::{Kind, Resource};
use opl_types::router::{Router, RouterExternal, RouterInterface, RouterSpec, StaticRoute};
use opl_types::sdn::gateway_ranking;
use opl_types::vm::{Vm, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::authz::AuthzObject;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::ipam::{HeldIps, RETRIES, held_ips, is_ip_taken};
use crate::ovn_sync::nb;
use crate::resources::{check_name, if_match, lookup};
use crate::revision::WARNING;
use crate::state::AppState;

// ---- 공통 ----

fn invalid_router(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message).with_code("invalid_router")
}

/// 라우터·유동 IP를 쓰려면 모든 노드가 `sdn-routing`을 알고(409 `cluster_upgrading` — 검증보다 먼저) SDN이 세워졌어야
/// 한다(409 `sdn_not_ready`).
fn require_routing(state: &AppState) -> Result<(), ApiError> {
    require_feature(&state.cluster, SDN_ROUTING)?;
    crate::sdn::require_ready(&state.cluster)?;
    Ok(())
}

/// 클러스터의 라우터·유동 IP·네트워크·VM·노드(한 번 떠 쓴다)
struct World {
    routers: Vec<Router>,
    fips: Vec<FloatingIp>,
    networks: Vec<Network>,
    vms: Vec<Vm>,
    nodes: Vec<Node>,
}

impl World {
    fn load(state: &AppState) -> Result<Self, ApiError> {
        Ok(World {
            routers: state.cluster.list::<RouterSpec>()?.1,
            fips: state.cluster.list::<FloatingIpSpec>()?.1,
            networks: state.cluster.list::<NetworkSpec>()?.1,
            vms: state.cluster.list::<VmSpec>()?.1,
            nodes: state.cluster.list::<NodeSpec>()?.1,
        })
    }

    /// 유동 IP 보기용: VM은 `fips`가 붙은 것만 읽는다(모든 VM을 풀지 않는다 — 리뷰 1 웹 I1). 주소 고르기(`used`)에는 쓰지
    /// 않는다.
    fn load_for_fips(state: &AppState, fips: &[FloatingIp]) -> Result<Self, ApiError> {
        let targets: BTreeSet<Uuid> = fips
            .iter()
            .filter_map(|f| f.spec.target)
            .map(|t| t.vm)
            .collect();
        let mut vms = Vec::new();
        for id in targets {
            if let Some(vm) = state.cluster.get::<VmSpec>(id)? {
                vms.push(vm);
            }
        }
        Ok(World {
            routers: state.cluster.list::<RouterSpec>()?.1,
            fips: Vec::new(),
            networks: state.cluster.list::<NetworkSpec>()?.1,
            vms,
            nodes: state.cluster.list::<NodeSpec>()?.1,
        })
    }

    fn network(&self, name: &str) -> Option<&Network> {
        self.networks.iter().find(|n| n.name == name)
    }

    fn node_name(&self, id: Uuid) -> String {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
    }

    /// 공급자 네트워크 `network`에서 VM NIC·라우터 외부·유동 IP가 쓰는 주소(`except`의 것은 뺀다)
    fn used(&self, network: &str, except: Option<Uuid>) -> BTreeSet<IpAddr> {
        let held: HeldIps = held_ips(&self.routers, &self.fips, except);
        let mut used: BTreeSet<IpAddr> = held.get(network).cloned().unwrap_or_default();
        used.extend(
            self.vms
                .iter()
                .filter(|v| !v.spec.template)
                .flat_map(|v| v.spec.nics.iter())
                .filter(|n| n.network == network)
                .flat_map(|n| n.ips.iter().map(|ip| ip.to_canonical())),
        );
        used
    }

    /// 네트워크 `network`를 인터페이스로 가진 라우터(`except` 말고)
    fn router_of(&self, network: &str, except: Option<Uuid>) -> Option<&Router> {
        self.routers.iter().find(|r| {
            Some(r.id) != except && r.spec.interfaces.iter().any(|i| i.network == network)
        })
    }
}

/// 응답 헤더 `orbivisor-warning`(있을 때만)
fn warning_header(warnings: &[String]) -> HeaderMap {
    let mut out = HeaderMap::new();
    if !warnings.is_empty()
        && let Ok(v) = HeaderValue::from_str(&warnings.join("; "))
    {
        out.insert(WARNING, v);
    }
    out
}

/// 공급자 네트워크 `external`로 나가는 라우터의 게이트웨이 후보가 없을 때의 문구
fn no_gateway(world: &World, external: &Network) -> Option<String> {
    let physnet = external.spec.physnet()?;
    nb::gateway_candidates(&world.nodes, physnet)
        .is_empty()
        .then(|| format!("no gateway node has physnet {physnet} ready"))
}

/// 주소 고르기의 오류: 다 쓰였으면 409 `ip_exhausted`, 쓰이면 409 `ip_in_use`, 나머지(서브넷 밖·예약)는 400
fn ipam_error(network: &str, e: ipam::IpamError) -> ApiError {
    match e {
        ipam::IpamError::Exhausted(_) => {
            ApiError::conflict("ip_exhausted", format!("network {network:?}: {e}"))
        }
        ipam::IpamError::InUse(_) => {
            ApiError::conflict("ip_in_use", format!("network {network:?}: {e}"))
        }
        _ => ApiError::bad_request(format!("network {network:?}: {e}")),
    }
}

/// 공급자 네트워크에서 유동 IP·라우터 외부 주소를 고른다(`v4`면 IPv4, 아니면 IPv6). `want`가 있으면 그것을 본다:
/// 서브넷 안의 예약이 아닌 빈 주소이고, 루트 `cluster.admin`이 아니면(`admin` 거짓) 유동 IP 풀(없으면 풀) 안이어야 한다
/// (§9.5, 리뷰 1 #2 — 물리 망의 다른 장비 주소를 테넌트가 가로채지 못하게. Neutron도 고른 주소는 관리자만). 없으면 그
/// 체계의 서브넷에서 고른다. `keep`은 이미 가진 주소(다시 보지 않는다).
fn pick_address(
    world: &World,
    network: &Network,
    v4: bool,
    want: Option<IpAddr>,
    keep: Option<IpAddr>,
    except: Option<Uuid>,
    admin: bool,
) -> Result<Option<IpAddr>, ApiError> {
    if let Some(w) = want.map(|w| w.to_canonical())
        && keep == Some(w)
    {
        return Ok(Some(w));
    }
    if let Some(w) = want.map(|w| w.to_canonical()) {
        let Some(subnet) = ipam::nic_subnet(&network.spec, w) else {
            return Err(ApiError::bad_request(format!(
                "address {w} is not in a subnet of network {:?}",
                network.name
            )));
        };
        if !admin && !ipam::in_floating_range(subnet, w) {
            return Err(ApiError::bad_request(format!(
                "address {w} is outside the {} of subnet {} of network {:?}; only a cluster administrator \
                 can choose an address outside them",
                if subnet.floating_pools.is_empty() {
                    "address pools"
                } else {
                    "floating IP ranges"
                },
                subnet.cidr,
                network.name
            )));
        }
        let used = world.used(&network.name, except);
        return ipam::allocate_from(subnet, &used, Some(w))
            .map(Some)
            .map_err(|e| ipam_error(&network.name, e));
    }
    // 네트워크는 체계마다 서브넷이 하나다(`NetworkSpec` 검증).
    let Some(subnet) = network.spec.subnets.iter().find(|s| s.is_ipv4() == v4) else {
        return Ok(None);
    };
    let used = world.used(&network.name, except);
    ipam::allocate_from(subnet, &used, None)
        .map(Some)
        .map_err(|e| ipam_error(&network.name, e))
}

/// 부른 사용자가 루트 `cluster.admin`인지(유동 IP 풀 밖의 주소를 고를 수 있다)
fn is_admin(state: &AppState, principal: &Principal) -> bool {
    state
        .authz
        .allows(principal, Privilege::ClusterAdmin, AuthzTarget::Root)
}

/// 채운 외부 주소 중 API가 고른 것이 있는지(준 주소도 지금 주소도 아닌 것). 그때만 Raft의 주소 겹침 거절을 다시
/// 고르기로 받는다(리뷰 1 M-6 — 준 주소의 겹침은 곧바로 409).
fn auto_picked(filled: &RouterExternal, given: &[IpAddr], old: Option<&RouterExternal>) -> bool {
    filled.ips.iter().any(|ip| {
        !given.iter().any(|g| g.to_canonical() == *ip)
            && !old.is_some_and(|o| o.network == filled.network && o.ips.contains(ip))
    })
}

// ---- 라우터 ----

/// 라우터 스펙을 네트워크와 함께 본다(§6·§9.1): 스펙 검증(400 `invalid_router`), 인터페이스는 서브넷이 있는 오버레이 SDN
/// 네트워크(400)이고 다른 라우터가 갖지 않음(409 `network_in_use`), 외부는 공급자 네트워크(400), 인터페이스·외부 서브넷
/// CIDR이 서로 겹치지 않음(400), 경로의 다음 홉은 붙은 서브넷 안(400).
fn check_router(world: &World, spec: &RouterSpec, me: Option<Uuid>) -> Result<(), ApiError> {
    spec.validate().map_err(|e| invalid_router(e.to_string()))?;
    let mut cidrs: Vec<(opl_types::security_group::Cidr, &str)> = Vec::new();
    for i in &spec.interfaces {
        let Some(n) = world.network(&i.network) else {
            return Err(invalid_router(format!("network {:?} not found", i.network)));
        };
        if !n.spec.is_sdn() || n.spec.is_provider() {
            return Err(invalid_router(format!(
                "network {:?} is not an overlay SDN network; a router interface must be one",
                n.name
            )));
        }
        if n.spec.subnets.is_empty() {
            return Err(invalid_router(format!(
                "network {:?} has no subnet; its subnet gateways are the router's addresses",
                n.name
            )));
        }
        if let Some(other) = world.router_of(&n.name, me) {
            return Err(ApiError::conflict(
                "network_in_use",
                format!(
                    "network {:?} is already an interface of router {:?}",
                    n.name, other.name
                ),
            ));
        }
        cidrs.extend(n.spec.subnets.iter().map(|s| (s.cidr, n.name.as_str())));
    }
    if let Some(e) = &spec.external {
        let Some(n) = world.network(&e.network) else {
            return Err(invalid_router(format!("network {:?} not found", e.network)));
        };
        if !n.spec.is_provider() {
            return Err(invalid_router(format!(
                "network {:?} is not a provider network; a router's external gateway must be one",
                n.name
            )));
        }
        if n.spec.subnets.is_empty() {
            return Err(invalid_router(format!(
                "provider network {:?} has no subnet",
                n.name
            )));
        }
        cidrs.extend(n.spec.subnets.iter().map(|s| (s.cidr, n.name.as_str())));
    }
    for (i, (a, an)) in cidrs.iter().enumerate() {
        if let Some((b, bn)) = cidrs[..i].iter().find(|(b, _)| a.overlaps(b)) {
            return Err(invalid_router(format!(
                "subnet {a} of network {an:?} overlaps subnet {b} of network {bn:?} on the same router"
            )));
        }
    }
    for r in &spec.routes {
        let hop = r.nexthop.to_canonical();
        if !cidrs.iter().any(|(c, _)| c.contains(hop)) {
            return Err(invalid_router(format!(
                "route {}: the nexthop {hop} is not in a subnet attached to the router",
                r.destination
            )));
        }
    }
    Ok(())
}

/// 라우터 외부 주소를 채운다(체계마다 하나): 준 주소는 서브넷 안의 예약이 아닌 빈 주소이고 루트 `cluster.admin`이
/// 아니면 유동 IP 풀(없으면 풀) 안이어야 하며, 없으면 유동 IP 풀(없으면 풀)에서 고른다. `old`는 지금 저장된 외부(같은 네트워크면 주소를 주지 않은 체계는 그
/// 주소를 그대로 쓴다).
fn fill_external(
    world: &World,
    external: &mut RouterExternal,
    old: Option<&RouterExternal>,
    me: Option<Uuid>,
    admin: bool,
) -> Result<(), ApiError> {
    let network = world
        .network(&external.network)
        .ok_or_else(|| invalid_router(format!("network {:?} not found", external.network)))?;
    let old = old.filter(|o| o.network == external.network);
    let kept: Vec<IpAddr> = old.map(|o| o.ips.clone()).unwrap_or_default();
    let given = external.ips.clone();
    let mut ips = Vec::new();
    for v4 in [true, false] {
        if !network.spec.subnets.iter().any(|s| s.is_ipv4() == v4) {
            if let Some(w) = given.iter().find(|ip| ip.to_canonical().is_ipv4() == v4) {
                return Err(invalid_router(format!(
                    "address {w} is not in a subnet of network {:?}",
                    network.name
                )));
            }
            continue;
        }
        let keep = kept.iter().copied().find(|ip| ip.is_ipv4() == v4);
        let want = given
            .iter()
            .copied()
            .find(|ip| ip.to_canonical().is_ipv4() == v4)
            .or(keep);
        if let Some(ip) = pick_address(world, network, v4, want, keep, me, admin)? {
            ips.push(ip);
        }
    }
    external.ips = ips;
    Ok(())
}

/// 라우터 `spec`을 지나 라우팅되는 유동 IP(대상 NIC의 네트워크가 인터페이스이고 유동 IP의 네트워크가 외부)
fn routed_fips<'a>(world: &'a World, spec: &RouterSpec) -> Vec<&'a FloatingIp> {
    let Some(e) = &spec.external else {
        return Vec::new();
    };
    world
        .fips
        .iter()
        .filter(|f| f.spec.network == e.network)
        .filter(|f| {
            target_nic(world, f)
                .is_some_and(|(_, nic)| spec.interfaces.iter().any(|i| i.network == nic.network))
        })
        .collect()
}

/// 유동 IP가 붙은 VM과 NIC
fn target_nic<'a>(
    world: &'a World,
    f: &FloatingIp,
) -> Option<(&'a Vm, &'a opl_types::vm::NicSpec)> {
    let t = f.spec.target?;
    let vm = world.vms.iter().find(|v| v.id == t.vm)?;
    let nic = vm.spec.nics.iter().find(|n| n.mac == Some(t.mac))?;
    Some((vm, nic))
}

/// 유동 IP 이름들(오류 문구)
fn names(fips: &[&FloatingIp]) -> String {
    fips.iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// 게이트웨이 노드 하나(Raft의 순위 — `HA_Chassis`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RouterGateway {
    /// 노드 이름(루트 `cluster.read`가 없으면 빈 글)
    pub node: String,
    /// `HA_Chassis.priority`(32767부터 내림 — 가장 높은 살아 있는 chassis가 주인)
    pub priority: u16,
    /// 노드의 게이트웨이 우선순위(`NodeSpec.sdn.gateway`)
    pub node_priority: u16,
    /// NB의 분산 게이트웨이 포트를 지금 이 노드가 가졌다(`status:hosting-chassis`). NB를 읽지 않았으면 거짓
    pub active: bool,
}

/// `GET /routers/{key}`: 라우터와 게이트웨이(Raft의 순위와 NB의 지금 주인)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RouterView {
    #[serde(flatten)]
    pub router: Resource<RouterSpec, opl_types::router::RouterStatus>,
    /// 게이트웨이 후보의 순위(외부가 있을 때 — 우선순위가 있고 유지보수가 아니며 외부 physnet이 준비된 노드, 앞 5개)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub gateways: Vec<RouterGateway>,
    /// 외부가 있는데 게이트웨이 후보가 없다(그동안 외부 게이트웨이 포트는 꺼져 있고 SNAT·유동 IP가 없다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub gateway_error: Option<String>,
    /// 분산 게이트웨이 포트를 지금 가진 노드(NB `status:hosting-chassis`). 없거나 NB를 읽지 않았으면 없다. 루트
    /// `cluster.read`가 없으면 빈 글이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub hosting: Option<String>,
    /// NB를 읽지 못한 까닭
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub live_error: Option<String>,
    /// 물리 라우터에 있어야 할 경로(NAT 없이 라우팅되는 안쪽 접두사 → 라우터 외부 주소): IPv6 서브넷은 늘(NAT66 없음),
    /// SNAT를 끈 라우터는 IPv4 서브넷도. 예 `fd00:20::/64 via 2001:db8::5`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub upstream_routes: Vec<String>,
}

/// 보기의 질의
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct LiveQuery {
    /// 거짓이면 OVN NB를 읽지 않는다(`hosting`·`active`가 비고 `live_error`도 없다). 기본 참
    #[serde(default = "yes")]
    pub live: bool,
}

fn yes() -> bool {
    true
}

/// `GET /routers`
#[derive(Debug, Serialize, ToSchema)]
pub struct RouterList {
    pub revision: u64,
    pub items: Vec<RouterView>,
}

/// NB에서 읽은 라우터 id → 게이트웨이 포트를 가진 chassis 이름(노드 UUID)
type Hosting = BTreeMap<Uuid, String>;

/// 게이트웨이 포트의 지금 주인(`router`가 있으면 그 라우터 하나). `Logical_Router_Port`의 세 칸만 읽는다(리뷰 1 웹 I1).
async fn read_hosting(state: &AppState, router: Option<Uuid>) -> Result<Hosting, String> {
    let (config, centrals, _) = crate::sdn::ovn_reader(state)?;
    Box::pin(crate::ovn_sync::read_hosting(config, &centrals, router))
        .await
        .map_err(|e| e.to_string())
}

/// 노드 이름을 볼 수 있는지(루트 `cluster.read` — `/nodes`와 같다). 아니면 보기의 노드 이름을 비운다(리뷰 1 M-4).
fn sees_nodes(state: &AppState, principal: &Principal) -> bool {
    state
        .authz
        .allows(principal, Privilege::ClusterRead, AuthzTarget::Root)
}

/// 보기의 노드 이름: 볼 수 없으면 빈 글
fn node_label(world: &World, id: Uuid, sees_nodes: bool) -> String {
    if sees_nodes {
        world.node_name(id)
    } else {
        String::new()
    }
}

fn router_view(
    world: &World,
    router: Router,
    hosting: Option<&Hosting>,
    sees_nodes: bool,
) -> RouterView {
    let mut view = RouterView {
        gateways: Vec::new(),
        gateway_error: None,
        hosting: None,
        live_error: None,
        upstream_routes: Vec::new(),
        router,
    };
    let host = hosting.and_then(|h| h.get(&view.router.id));
    view.hosting = host.map(|c| match c.parse::<Uuid>() {
        Ok(id) => node_label(world, id, sees_nodes),
        Err(_) if sees_nodes => c.clone(),
        Err(_) => String::new(),
    });
    let Some(e) = &view.router.spec.external else {
        return view;
    };
    let Some(ext) = world.network(&e.network) else {
        return view;
    };
    let physnet = ext.spec.physnet().unwrap_or_default();
    let candidates = nb::gateway_candidates(&world.nodes, physnet);
    view.gateway_error = no_gateway(world, ext);
    view.gateways = gateway_ranking(view.router.id, &candidates)
        .into_iter()
        .map(|(node, priority)| RouterGateway {
            node: node_label(world, node, sees_nodes),
            priority,
            node_priority: candidates
                .iter()
                .find(|(n, _)| *n == node)
                .map_or(0, |(_, p)| *p),
            active: host.is_some_and(|c| *c == node.to_string()),
        })
        .collect();
    for i in &view.router.spec.interfaces {
        let Some(n) = world.network(&i.network) else {
            continue;
        };
        for s in &n.spec.subnets {
            if s.is_ipv4() && e.snat() {
                continue;
            }
            if let Some(via) = e.ips.iter().find(|ip| ip.is_ipv4() == s.is_ipv4()) {
                view.upstream_routes.push(format!("{} via {via}", s.cidr));
            }
        }
    }
    view
}

/// 볼 수 있는 라우터(`network.config`가 아닌 읽기)와 게이트웨이. `live=false`면 NB를 읽지 않는다.
#[utoipa::path(get, path = "/routers", tag = "routers", params(LiveQuery),
    responses((status = 200, body = RouterList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_routers(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(query): Query<LiveQuery>,
) -> Result<Json<RouterList>, ApiError> {
    let (revision, routers) = state.cluster.list::<RouterSpec>()?;
    let mut routers = state.authz.filter(&principal, routers)?;
    routers.sort_by(|a, b| a.name.cmp(&b.name));
    let world = World::load(&state)?;
    let (hosting, error) = if query.live && !routers.is_empty() {
        match Box::pin(read_hosting(&state, None)).await {
            Ok(h) => (Some(h), None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, None)
    };
    let nodes = sees_nodes(&state, &principal);
    let items = routers
        .into_iter()
        .map(|r| {
            let mut v = router_view(&world, r, hosting.as_ref(), nodes);
            v.live_error.clone_from(&error);
            v
        })
        .collect();
    Ok(Json(RouterList { revision, items }))
}

/// 라우터 하나와 게이트웨이 순위·지금 주인. `live=false`면 NB를 읽지 않는다.
#[utoipa::path(get, path = "/routers/{key}", tag = "routers",
    params(("key" = String, Path, description = "id or name"), LiveQuery),
    responses((status = 200, body = RouterView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_router(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(query): Query<LiveQuery>,
) -> Result<Json<RouterView>, ApiError> {
    // 경로표가 이 라우터를 읽을 수 있는지 봤다.
    let router = lookup::<RouterSpec>(&state.cluster, &key)?;
    let world = World::load(&state)?;
    let (hosting, error) = if query.live {
        match Box::pin(read_hosting(&state, Some(router.id))).await {
            Ok(h) => (Some(h), None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, None)
    };
    let mut view = router_view(
        &world,
        router,
        hosting.as_ref(),
        sees_nodes(&state, &principal),
    );
    view.live_error = error;
    Ok(Json(view))
}

/// `POST /routers`
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRouter {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub spec: RouterSpec,
}

/// 라우터를 만든다(계획 66 §9): 폴더의 `network.config`, 인터페이스 네트워크마다 `network.config`, 외부 네트워크의
/// `network.assign`. 외부 주소는 API가 외부 네트워크의 체계마다 고른다(유동 IP 풀이 있으면 거기서만, 없으면 풀 — 준
/// 주소는 서브넷 안의 빈 주소이고 루트 `cluster.admin`이 아니면 그 범위 안). 옛 노드가
/// 남았으면 409 `cluster_upgrading`, SDN이 없으면 409 `sdn_not_ready`, 스펙·네트워크 검사는 400 `invalid_router`, 이미 다른
/// 라우터의 인터페이스인 네트워크는 409 `network_in_use`, 주소는 409 `ip_in_use`·`ip_exhausted`, 폴더 할당량
/// `routers`는 409 `quota_exceeded`. 게이트웨이 후보가 없으면 만들고 `orbivisor-warning`으로 알린다.
#[utoipa::path(post, path = "/routers", tag = "routers",
    request_body = CreateRouter,
    responses((status = 201, body = Resource<RouterSpec, opl_types::router::RouterStatus>,
        headers(("orbivisor-warning" = String, description = "no gateway node for the external network (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_router(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateRouter>,
) -> Result<(StatusCode, HeaderMap, Json<Router>), ApiError> {
    require_routing(&state)?;
    check_name(&req.name)?;
    state.authz.require_new(
        &principal,
        Privilege::NetworkConfig,
        Kind::Router,
        req.spec.folder,
    )?;
    // 인터페이스는 그 네트워크의 모든 VM 기본 경로(DHCP `router`)를 바꾸므로 NIC 붙이기(`network.assign`)가 아니라
    // `network.config`다(리뷰 1 #1). 외부(공급자) 네트워크는 `network.assign`.
    state.authz.require_networks_with(
        &principal,
        Privilege::NetworkConfig,
        req.spec.interfaces.iter().map(|i| i.network.as_str()),
    )?;
    state.authz.require_networks(
        &principal,
        req.spec.external.iter().map(|e| e.network.as_str()),
    )?;
    let admin = is_admin(&state, &principal);
    for _ in 0..RETRIES {
        let world = World::load(&state)?;
        let mut spec = req.spec.clone();
        check_router(&world, &spec, None)?;
        if let Some(e) = &mut spec.external {
            fill_external(&world, e, None, None, admin)?;
        }
        let retry = match (&spec.external, &req.spec.external) {
            (Some(filled), Some(given)) => auto_picked(filled, &given.ips, None),
            _ => false,
        };
        let mut router = Resource::new(req.name.clone(), spec);
        router.labels = req.labels.clone();
        match state.cluster.create(&router).await {
            Ok(revision) => router.revision = revision,
            Err(e) if is_ip_taken(&e) && retry => continue,
            Err(e) => return Err(e.into()),
        }
        let mut detail = format!("router create {}", router.name);
        let mut warnings = Vec::new();
        if let Some(e) = &router.spec.external {
            detail.push_str(&format!(" --external {}", e.network));
            if let Some(w) = world
                .network(&e.network)
                .and_then(|n| no_gateway(&world, n))
            {
                warnings.push(format!(
                    "{w}; the router's external gateway stays off until one has it \
                     (`orbivisorctl node sdn NODE --physnet NAME=BRIDGE --gateway`)"
                ));
            }
        }
        audit.detail(detail);
        return Ok((StatusCode::CREATED, warning_header(&warnings), Json(router)));
    }
    Err(ApiError::conflict(
        "ip_in_use",
        "other requests kept taking the chosen addresses; try again",
    ))
}

/// 적은 값은 `null`이어도 `Some`이다(적지 않으면 `serde(default)`의 `None`).
fn present<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// `PATCH /routers/{key}`: 준 칸만 바꾼다(목록은 통째로).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RouterPatch {
    /// 빈 글이면 설명을 지운다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub description: Option<String>,
    /// 외부 게이트웨이, `null`은 지우기. 같은 네트워크에 `ips`를 주지 않으면 지금 주소를 그대로 쓴다.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<RouterExternal>)]
    pub external: Option<Option<RouterExternal>>,
    /// 인터페이스 전체
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub interfaces: Option<Vec<RouterInterface>>,
    /// 정적 경로 전체
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub routes: Option<Vec<StaticRoute>>,
}

/// 라우터의 설명·외부·인터페이스·경로를 바꾼다(`If-Match`로 리비전 확인, 없으면 읽은 리비전에 쓰고 그새 바뀌었으면 다시
/// 읽는다). 라우터의 `network.config`(경로표), 새로 잇는 인터페이스 네트워크의 `network.config`, 새 외부 네트워크의
/// `network.assign`. 유동 IP가 이
/// 라우터로 라우팅되는 인터페이스를 빼거나 외부를 바꾸면 409 `fip_in_use`(먼저 떼라). 검사는 만들기와 같다.
#[utoipa::path(patch, path = "/routers/{key}", tag = "routers",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = RouterPatch,
    responses((status = 200, body = Resource<RouterSpec, opl_types::router::RouterStatus>,
        headers(("orbivisor-warning" = String, description = "no gateway node for the external network (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_router(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RouterPatch>,
) -> Result<(HeaderMap, Json<Router>), ApiError> {
    require_routing(&state)?;
    let expected = if_match(&headers)?;
    for _ in 0..RETRIES * 2 {
        let router = lookup::<RouterSpec>(&state.cluster, &key)?;
        let world = World::load(&state)?;
        let old = &router.spec;
        let mut spec = old.clone();
        if let Some(d) = &req.description {
            spec.description = (!d.is_empty()).then(|| d.clone());
        }
        if let Some(e) = &req.external {
            spec.external.clone_from(e);
        }
        if let Some(i) = &req.interfaces {
            spec.interfaces.clone_from(i);
        }
        if let Some(r) = &req.routes {
            spec.routes.clone_from(r);
        }
        // 새 인터페이스 네트워크는 `network.config`(리뷰 1 #1), 새 외부 네트워크는 `network.assign`.
        state.authz.require_networks_with(
            &principal,
            Privilege::NetworkConfig,
            spec.interfaces
                .iter()
                .filter(|i| !old.interfaces.contains(i))
                .map(|i| i.network.as_str()),
        )?;
        state.authz.require_networks(
            &principal,
            spec.external
                .iter()
                .filter(|e| old.external.as_ref().map(|o| &o.network) != Some(&e.network))
                .map(|e| e.network.as_str()),
        )?;
        // 유동 IP가 지금 이 라우터로 라우팅되는데 그 길이 사라지면 막는다(§9.5 — 먼저 떼라).
        let lost: Vec<&FloatingIp> = routed_fips(&world, old)
            .into_iter()
            .filter(|f| !routed_fips(&world, &spec).iter().any(|g| g.id == f.id))
            .collect();
        if !lost.is_empty() {
            return Err(ApiError::conflict(
                "fip_in_use",
                format!(
                    "floating IPs {} are routed through router {:?} this way; detach them first",
                    names(&lost),
                    router.name
                ),
            ));
        }
        check_router(&world, &spec, Some(router.id))?;
        if let Some(e) = &mut spec.external {
            fill_external(
                &world,
                e,
                old.external.as_ref(),
                Some(router.id),
                is_admin(&state, &principal),
            )?;
        }
        let given: Vec<IpAddr> = match &req.external {
            Some(Some(e)) => e.ips.clone(),
            _ => Vec::new(),
        };
        let retry = spec
            .external
            .as_ref()
            .is_some_and(|e| auto_picked(e, &given, old.external.as_ref()));
        let revision = expected.unwrap_or(router.revision);
        match state
            .cluster
            .update_spec(router.id, Some(revision), router.labels.clone(), &spec)
            .await
        {
            Ok(written) => {
                let mut warnings = Vec::new();
                if let Some(e) = &spec.external
                    && let Some(w) = world
                        .network(&e.network)
                        .and_then(|n| no_gateway(&world, n))
                {
                    warnings.push(w);
                }
                audit.detail(router_change(&router.name, old, &spec));
                let mut out = router;
                out.spec = spec;
                out.revision = written;
                return Ok((warning_header(&warnings), Json(out)));
            }
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() => {}
            Err(e) if is_ip_taken(&e) && retry => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the router kept changing while being updated; try again",
    ))
}

/// 감사 설명: 바뀐 것(인터페이스 더하기·빼기, 외부, 경로)
fn router_change(name: &str, old: &RouterSpec, new: &RouterSpec) -> String {
    let mut parts = Vec::new();
    for i in new
        .interfaces
        .iter()
        .filter(|i| !old.interfaces.contains(i))
    {
        parts.push(format!("router interface add {name} {}", i.network));
    }
    for i in old
        .interfaces
        .iter()
        .filter(|i| !new.interfaces.contains(i))
    {
        parts.push(format!("router interface remove {name} {}", i.network));
    }
    if old.external != new.external {
        parts.push(match &new.external {
            Some(e) => format!("router external set {name} {}", e.network),
            None => format!("router external clear {name}"),
        });
    }
    if old.routes != new.routes {
        parts.push(format!("router routes {name} ({})", new.routes.len()));
    }
    if parts.is_empty() {
        parts.push(format!("router update {name}"));
    }
    parts.join(", ")
}

/// 라우터를 지운다. 인터페이스가 남았으면 409 `router_in_use`(먼저 빼라).
#[utoipa::path(delete, path = "/routers/{key}", tag = "routers",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_router(
    State(state): State<Arc<AppState>>,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let router = lookup::<RouterSpec>(&state.cluster, &key)?;
    if !router.spec.interfaces.is_empty() {
        let list: Vec<&str> = router
            .spec
            .interfaces
            .iter()
            .map(|i| i.network.as_str())
            .collect();
        return Err(ApiError::conflict(
            "router_in_use",
            format!(
                "router {:?} still has interfaces {}; remove them first",
                router.name,
                list.join(", ")
            ),
        ));
    }
    // `If-Match`가 없어도 읽은 리비전으로 지운다: 그새 인터페이스가 더해졌으면 함께 지우지 않고 409(리뷰 1 M-5).
    let revision = if_match(&headers)?.unwrap_or(router.revision);
    state
        .cluster
        .delete::<RouterSpec>(router.id, Some(revision))
        .await?;
    crate::permissions::drop_orphans(&state.cluster).await;
    audit.detail(format!("router delete {}", router.name));
    Ok(StatusCode::NO_CONTENT)
}

// ---- 유동 IP ----

/// 유동 IP의 지금 모양(Raft에서 셈 — OvnSync가 쓰는 NAT 행과 같은 규칙, §9.5)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FipState {
    /// 붙지 않았다.
    Unattached,
    /// VM이 도는 노드에서 바로 나간다(그 노드들이 외부 physnet을 가졌다).
    Distributed {
        /// VM이 도는 노드(마이그레이션 중이면 둘, 쉼표로). 루트 `cluster.read`가 없으면 빈 글
        node: String,
    },
    /// 게이트웨이 노드가 NAT를 한다.
    Centralized { reason: String },
    /// NAT가 없다(닿지 않는다).
    Unrouted { reason: String },
}

/// `GET /floating-ips/{key}`
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FloatingIpView {
    #[serde(flatten)]
    pub fip: Resource<FloatingIpSpec, FloatingIpStatus>,
    pub state: FipState,
    /// 붙은 VM의 이름(부른 사용자가 읽을 수 있을 때만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub vm_name: Option<String>,
}

/// `GET /floating-ips`
#[derive(Debug, Serialize, ToSchema)]
pub struct FloatingIpList {
    pub revision: u64,
    pub items: Vec<FloatingIpView>,
}

/// 유동 IP를 붙일 수 있는 NIC인지(§9.5): 그 NIC의 네트워크가 라우터 인터페이스이고 그 라우터의 외부가 유동 IP의
/// 네트워크이며 NIC에 그 네트워크의 IPv4 주소가 있다. 아니면 까닭.
/// `label`은 문구에 쓰는 VM 이름(읽을 수 없는 사용자에게는 "the target VM" — 리뷰 1 M-4).
fn routable<'a>(
    world: &'a World,
    network: &str,
    vm: &Vm,
    mac: opl_types::vm::MacAddr,
    label: &str,
) -> Result<(&'a Router, IpAddr), String> {
    if vm.spec.template {
        return Err(format!("{label} is a template"));
    }
    let Some(nic) = vm.spec.nics.iter().find(|n| n.mac == Some(mac)) else {
        return Err(format!("{label} has no NIC with MAC {mac}"));
    };
    let Some(router) = world.routers.iter().find(|r| {
        r.spec.interfaces.iter().any(|i| i.network == nic.network)
            && r.spec
                .external
                .as_ref()
                .is_some_and(|e| e.network == network)
    }) else {
        return Err(format!(
            "network {:?} of the NIC is not an interface of a router whose external network is {network:?}",
            nic.network
        ));
    };
    let Some(ip) = world.network(&nic.network).and_then(|n| {
        nic.ips
            .iter()
            .copied()
            .find(|ip| ip.is_ipv4() && ipam::nic_subnet(&n.spec, *ip).is_some())
    }) else {
        return Err(format!(
            "the NIC has no IPv4 address on network {:?}",
            nic.network
        ));
    };
    Ok((router, ip))
}

/// VM을 읽을 수 없으면(`sees_vm` 거짓) 까닭에 VM 이름을, 루트 `cluster.read`가 없으면(`sees_nodes` 거짓) 노드 이름을
/// 넣지 않는다(리뷰 1 M-4).
fn fip_state(world: &World, f: &FloatingIp, sees_vm: bool, sees_nodes: bool) -> FipState {
    let Some(t) = f.spec.target else {
        return FipState::Unattached;
    };
    let unrouted = |reason: String| FipState::Unrouted { reason };
    let Some(vm) = world.vms.iter().find(|v| v.id == t.vm) else {
        return unrouted("the VM no longer exists".into());
    };
    let label = if sees_vm {
        format!("VM {:?}", vm.name)
    } else {
        "the target VM".to_string()
    };
    if let Err(reason) = routable(world, &f.spec.network, vm, t.mac, &label) {
        return unrouted(reason);
    }
    let Some(ext) = world.network(&f.spec.network) else {
        return unrouted(format!("network {:?} not found", f.spec.network));
    };
    if let Some(reason) = no_gateway(world, ext) {
        return unrouted(reason);
    }
    let physnet = ext.spec.physnet().unwrap_or_default();
    let ids: BTreeSet<Uuid> = world.nodes.iter().map(|n| n.id).collect();
    let on = nb::chassis_nodes(vm, &ids);
    if on.is_empty() {
        return FipState::Centralized {
            reason: "the VM is not running".into(),
        };
    }
    let missing: Vec<String> = on
        .iter()
        .filter(|id| {
            !world
                .nodes
                .iter()
                .find(|n| n.id == **id)
                .and_then(|n| n.status.sdn.as_ref())
                .is_some_and(|s| s.physnet_ready(physnet))
        })
        .map(|id| world.node_name(*id))
        .collect();
    if missing.is_empty() {
        FipState::Distributed {
            node: if sees_nodes {
                on.iter()
                    .map(|id| world.node_name(*id))
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                String::new()
            },
        }
    } else if sees_nodes {
        FipState::Centralized {
            reason: format!(
                "node {} does not have physnet {physnet} ready",
                missing.join(", ")
            ),
        }
    } else {
        FipState::Centralized {
            reason: format!("a node of the VM does not have physnet {physnet} ready"),
        }
    }
}

fn fip_view(
    state: &AppState,
    principal: &Principal,
    world: &World,
    sees_nodes: bool,
    f: FloatingIp,
) -> FloatingIpView {
    let vm_name = f
        .spec
        .target
        .and_then(|t| world.vms.iter().find(|v| v.id == t.vm))
        .filter(|vm| {
            state
                .authz
                .allows(principal, Privilege::SystemRead, vm.authz_target())
        })
        .map(|vm| vm.name.clone());
    FloatingIpView {
        state: fip_state(world, &f, vm_name.is_some(), sees_nodes),
        vm_name,
        fip: f,
    }
}

/// 볼 수 있는 유동 IP와 지금 모양(분산·중앙·라우팅 안 됨)
#[utoipa::path(get, path = "/floating-ips", tag = "floating-ips",
    responses((status = 200, body = FloatingIpList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_floating_ips(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<FloatingIpList>, ApiError> {
    let (revision, fips) = state.cluster.list::<FloatingIpSpec>()?;
    let mut fips = state.authz.filter(&principal, fips)?;
    if fips.is_empty() {
        // 유동 IP가 없는 클러스터의 VM 화면 읽기(리뷰 1 웹 I1)는 다른 목록을 읽지 않는다.
        return Ok(Json(FloatingIpList {
            revision,
            items: Vec::new(),
        }));
    }
    fips.sort_by(|a, b| a.name.cmp(&b.name));
    let world = World::load_for_fips(&state, &fips)?;
    let nodes = sees_nodes(&state, &principal);
    let items = fips
        .into_iter()
        .map(|f| fip_view(&state, &principal, &world, nodes, f))
        .collect();
    Ok(Json(FloatingIpList { revision, items }))
}

#[utoipa::path(get, path = "/floating-ips/{key}", tag = "floating-ips",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = FloatingIpView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_floating_ip(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<FloatingIpView>, ApiError> {
    let f = lookup::<FloatingIpSpec>(&state.cluster, &key)?;
    let world = World::load_for_fips(&state, std::slice::from_ref(&f))?;
    let nodes = sees_nodes(&state, &principal);
    Ok(Json(fip_view(&state, &principal, &world, nodes, f)))
}

/// `POST /floating-ips`
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateFloatingIp {
    /// 없으면 주소의 점을 `-`로 바꾼 것(`203-0-113-120`)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub name: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub spec: FloatingIpSpec,
}

/// 대상 VM NIC를 본다: VM이 있고(400) 부른 사용자가 그 VM의 `vm.config`를 가졌고 NIC가 라우팅되며(409
/// `fip_not_routable`) 다른 유동 IP가 붙지 않았다(409 `fip_in_use`).
fn check_target(
    state: &AppState,
    principal: &Principal,
    world: &World,
    network: &str,
    target: FipTarget,
    me: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(vm) = world.vms.iter().find(|v| v.id == target.vm) else {
        return Err(ApiError::bad_request(format!("VM {} not found", target.vm)));
    };
    state.authz.require(principal, Privilege::VmConfig, vm)?;
    routable(world, network, vm, target.mac, &format!("VM {:?}", vm.name))
        .map_err(|reason| ApiError::conflict("fip_not_routable", reason))?;
    if let Some(other) = world.fips.iter().find(|f| {
        Some(f.id) != me
            && f.spec
                .target
                .is_some_and(|t| t.vm == target.vm && t.mac == target.mac)
    }) {
        return Err(ApiError::conflict(
            "fip_in_use",
            format!(
                "the NIC {} of VM {:?} already has floating IP {:?}",
                target.mac, vm.name, other.name
            ),
        ));
    }
    Ok(())
}

/// 유동 IP를 만든다(계획 66 §9.5): 폴더의 `network.config`와 공급자 네트워크의 `network.assign`, 붙이면 그 VM의
/// `vm.config`. 주소는 API가 유동 IP 풀(없으면 풀)의 IPv4에서 고른다(준 주소는 서브넷 안의 빈 IPv4이고, 루트
/// `cluster.admin`이 아니면 그 범위 안 — 아니면 400). 공급자 네트워크가
/// 아니면 400, 주소는 409 `ip_in_use`·`ip_exhausted`, 붙일 NIC가 라우팅되지 않으면 409 `fip_not_routable`, 이미 유동 IP가
/// 붙은 NIC는 409 `fip_in_use`, 폴더 할당량 `floating_ips`는 409 `quota_exceeded`.
#[utoipa::path(post, path = "/floating-ips", tag = "floating-ips",
    request_body = CreateFloatingIp,
    responses((status = 201, body = Resource<FloatingIpSpec, FloatingIpStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_floating_ip(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateFloatingIp>,
) -> Result<(StatusCode, Json<FloatingIp>), ApiError> {
    require_routing(&state)?;
    if let Some(name) = &req.name {
        check_name(name)?;
    }
    state.authz.require_new(
        &principal,
        Privilege::NetworkConfig,
        Kind::FloatingIp,
        req.spec.folder,
    )?;
    state
        .authz
        .require_networks(&principal, [req.spec.network.as_str()])?;
    req.spec
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    for _ in 0..RETRIES {
        let world = World::load(&state)?;
        let Some(network) = world.network(&req.spec.network) else {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                req.spec.network
            )));
        };
        if !network.spec.is_provider() {
            return Err(ApiError::bad_request(format!(
                "network {:?} is not a provider network; floating IPs come from provider networks",
                network.name
            )));
        }
        let mut spec = req.spec.clone();
        spec.address = pick_address(
            &world,
            network,
            true,
            spec.address,
            None,
            None,
            is_admin(&state, &principal),
        )?;
        let Some(address) = spec.address else {
            return Err(ApiError::bad_request(format!(
                "provider network {:?} has no IPv4 subnet",
                network.name
            )));
        };
        if let Some(t) = spec.target {
            check_target(&state, &principal, &world, &network.name, t, None)?;
        }
        let name = req
            .name
            .clone()
            .unwrap_or_else(|| address.to_string().replace('.', "-"));
        let mut fip = Resource::new(name, spec);
        fip.labels = req.labels.clone();
        match state.cluster.create(&fip).await {
            Ok(revision) => fip.revision = revision,
            Err(e) if is_ip_taken(&e) && req.spec.address.is_none() => continue,
            Err(e) => return Err(e.into()),
        }
        audit.detail(format!("fip create {} -> {address}", fip.spec.network));
        return Ok((StatusCode::CREATED, Json(fip)));
    }
    Err(ApiError::conflict(
        "ip_in_use",
        "other requests kept taking the chosen addresses; try again",
    ))
}

/// `PATCH /floating-ips/{key}`
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FloatingIpPatch {
    /// 붙일 VM NIC, `null`은 떼기
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<FipTarget>)]
    pub target: Option<Option<FipTarget>>,
    /// 빈 글이면 설명을 지운다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub description: Option<String>,
}

/// 유동 IP를 붙이거나 뗀다(유동 IP의 `network.config` — 경로표, 붙이면 대상 VM의 `vm.config`). 붙일 NIC의 검사는
/// 만들기와 같다. `If-Match`가 없으면 읽은 리비전에 쓰고 그새 바뀌었으면 다시 읽는다.
#[utoipa::path(patch, path = "/floating-ips/{key}", tag = "floating-ips",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = FloatingIpPatch,
    responses((status = 200, body = Resource<FloatingIpSpec, FloatingIpStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_floating_ip(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<FloatingIpPatch>,
) -> Result<Json<FloatingIp>, ApiError> {
    require_routing(&state)?;
    let expected = if_match(&headers)?;
    for _ in 0..RETRIES * 2 {
        let f = lookup::<FloatingIpSpec>(&state.cluster, &key)?;
        let world = World::load(&state)?;
        let mut spec = f.spec.clone();
        if let Some(d) = &req.description {
            spec.description = (!d.is_empty()).then(|| d.clone());
        }
        if let Some(t) = req.target {
            spec.target = t;
        }
        spec.validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if let Some(t) = spec.target.filter(|t| Some(*t) != f.spec.target) {
            check_target(&state, &principal, &world, &spec.network, t, Some(f.id))?;
        }
        let revision = expected.unwrap_or(f.revision);
        match state
            .cluster
            .update_spec(f.id, Some(revision), f.labels.clone(), &spec)
            .await
        {
            Ok(written) => {
                let address = f.spec.address.map(|a| a.to_string()).unwrap_or_default();
                audit.detail(match (&f.spec.target, &spec.target) {
                    (_, Some(t)) if f.spec.target != spec.target => {
                        let vm = world
                            .vms
                            .iter()
                            .find(|v| v.id == t.vm)
                            .map_or_else(|| t.vm.to_string(), |v| v.name.clone());
                        format!("fip attach {} {vm}/{}", f.name, t.mac)
                    }
                    (Some(_), None) => format!("fip detach {} ({address})", f.name),
                    _ => format!("fip update {}", f.name),
                });
                let mut out = f;
                out.spec = spec;
                out.revision = written;
                return Ok(Json(out));
            }
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the floating IP kept changing while being updated; try again",
    ))
}

/// 유동 IP를 지운다(붙어 있어도 — NAT가 사라진다).
#[utoipa::path(delete, path = "/floating-ips/{key}", tag = "floating-ips",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_floating_ip(
    State(state): State<Arc<AppState>>,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let f = lookup::<FloatingIpSpec>(&state.cluster, &key)?;
    state
        .cluster
        .delete::<FloatingIpSpec>(f.id, if_match(&headers)?)
        .await?;
    crate::permissions::drop_orphans(&state.cluster).await;
    audit.detail(format!(
        "fip delete {} ({})",
        f.name,
        f.spec.address.map(|a| a.to_string()).unwrap_or_default()
    ));
    Ok(StatusCode::NO_CONTENT)
}

// ---- 다른 자리에서 쓰는 검사 ----

/// 네트워크를 지우기 전에(§8): 라우터의 인터페이스·외부이거나 유동 IP가 있으면 409 `network_in_use`(이름들).
pub(crate) fn check_network_unused(state: &AppState, network: &Network) -> Result<(), ApiError> {
    let (_, routers) = state.cluster.list::<RouterSpec>()?;
    let (_, fips) = state.cluster.list::<FloatingIpSpec>()?;
    let mut users: Vec<String> = routers
        .iter()
        .filter(|r| {
            r.spec.interfaces.iter().any(|i| i.network == network.name)
                || r.spec
                    .external
                    .as_ref()
                    .is_some_and(|e| e.network == network.name)
        })
        .map(|r| format!("router {:?}", r.name))
        .collect();
    users.extend(
        fips.iter()
            .filter(|f| f.spec.network == network.name)
            .map(|f| format!("floating IP {:?}", f.name)),
    );
    if users.is_empty() {
        return Ok(());
    }
    Err(ApiError::conflict(
        "network_in_use",
        format!("network {:?} is used by {}", network.name, users.join(", ")),
    ))
}

/// 서브넷을 빼기 전에(§8): 라우터 외부 주소·유동 IP가 그 서브넷 주소를 쓰면 409 `subnet_in_use`
pub(crate) fn check_subnet_unused(
    state: &AppState,
    network: &str,
    cidr: &opl_types::security_group::Cidr,
) -> Result<(), ApiError> {
    let (_, routers) = state.cluster.list::<RouterSpec>()?;
    let (_, fips) = state.cluster.list::<FloatingIpSpec>()?;
    let held = held_ips(&routers, &fips, None);
    if !held
        .get(network)
        .is_some_and(|h| h.iter().any(|ip| cidr.contains(*ip)))
    {
        return Ok(());
    }
    Err(ApiError::conflict(
        "subnet_in_use",
        format!(
            "subnet {cidr} of network {network:?} has addresses of routers or floating IPs; remove those first"
        ),
    ))
}

/// 라우터 인터페이스인 네트워크에 서브넷을 더하면 그 라우터의 다른 서브넷과 겹치지 않아야 한다(400 `invalid_subnet`).
pub(crate) fn check_router_overlap(
    state: &AppState,
    network: &Network,
    subnets: &[opl_types::network::Subnet],
) -> Result<(), ApiError> {
    let (_, routers) = state.cluster.list::<RouterSpec>()?;
    let Some(router) = routers
        .iter()
        .find(|r| r.spec.interfaces.iter().any(|i| i.network == network.name))
    else {
        return Ok(());
    };
    let (_, networks) = state.cluster.list::<NetworkSpec>()?;
    let others = router
        .spec
        .interfaces
        .iter()
        .map(|i| i.network.as_str())
        .chain(router.spec.external.iter().map(|e| e.network.as_str()))
        .filter(|n| *n != network.name)
        .filter_map(|n| networks.iter().find(|x| x.name == n));
    for other in others {
        for s in subnets {
            if let Some(o) = other.spec.subnets.iter().find(|o| o.cidr.overlaps(&s.cidr)) {
                return Err(ApiError::bad_request(format!(
                    "subnet {} overlaps subnet {} of network {:?} on router {:?}",
                    s.cidr, o.cidr, other.name, router.name
                ))
                .with_code("invalid_subnet"));
            }
        }
    }
    Ok(())
}

/// VM을 지운 뒤 그 VM에 붙은 유동 IP를 뗀다(§9.5 — 리비전 확인 쓰기, 실패하면 남겨 두고 보기가 "VM 없음"으로 보인다).
pub(crate) async fn detach_vm(state: &AppState, vm: Uuid) {
    let Ok((_, fips)) = state.cluster.list::<FloatingIpSpec>() else {
        return;
    };
    for f in fips
        .iter()
        .filter(|f| f.spec.target.is_some_and(|t| t.vm == vm))
    {
        let mut spec = f.spec.clone();
        spec.target = None;
        if let Err(e) = state
            .cluster
            .update_spec(f.id, Some(f.revision), f.labels.clone(), &spec)
            .await
        {
            tracing::warn!(fip = %f.name, error = %e, "cannot detach the floating IP of a deleted VM");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_changes_read_like_the_cli() {
        let old: RouterSpec = serde_json::from_value(serde_json::json!({
            "interfaces": [{ "network": "a" }]
        }))
        .unwrap();
        let new: RouterSpec = serde_json::from_value(serde_json::json!({
            "interfaces": [{ "network": "b" }],
            "external": { "network": "ext" }
        }))
        .unwrap();
        assert_eq!(
            router_change("r1", &old, &new),
            "router interface add r1 b, router interface remove r1 a, router external set r1 ext"
        );
        assert_eq!(router_change("r1", &old, &old), "router update r1");
    }

    fn world(vms: Vec<Vm>, routers: Vec<Router>) -> World {
        World {
            routers,
            fips: Vec::new(),
            networks: Vec::new(),
            vms,
            nodes: Vec::new(),
        }
    }

    /// 리뷰 1 M-4: VM을 읽을 수 없는 사용자에게는 유동 IP 상태의 까닭에 VM 이름이 없다.
    #[test]
    fn fip_reasons_hide_unreadable_vm_names() {
        let vm: Vm = Resource::new(
            "secret-db",
            serde_json::from_value(serde_json::json!({
                "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt", "nics": []
            }))
            .unwrap(),
        );
        let f: FloatingIp = Resource::new(
            "f1",
            serde_json::from_value(serde_json::json!({
                "network": "ext", "address": "203.0.113.100",
                "target": { "vm": vm.id, "mac": "52:54:00:00:00:01" }
            }))
            .unwrap(),
        );
        let w = world(vec![vm], Vec::new());
        let FipState::Unrouted { reason } = fip_state(&w, &f, false, false) else {
            panic!("not unrouted");
        };
        assert!(!reason.contains("secret-db"), "{reason}");
        assert!(reason.starts_with("the target VM"), "{reason}");
        let FipState::Unrouted { reason } = fip_state(&w, &f, true, true) else {
            panic!("not unrouted");
        };
        assert!(reason.contains("\"secret-db\""), "{reason}");
    }

    /// 리뷰 1 M-6: 준 주소나 지금 주소만 쓴 외부는 Raft의 주소 겹침을 다시 고르기로 받지 않는다(곧바로 409).
    #[test]
    fn only_auto_picked_external_addresses_retry() {
        let ext = |ips: &[&str]| RouterExternal {
            network: "ext".into(),
            ips: ips.iter().map(|i| i.parse().unwrap()).collect(),
            snat: None,
        };
        let given: Vec<IpAddr> = vec!["203.0.113.5".parse().unwrap()];
        assert!(!auto_picked(&ext(&["203.0.113.5"]), &given, None));
        assert!(auto_picked(
            &ext(&["203.0.113.5", "2001:db8::2"]),
            &given,
            None
        ));
        let old = ext(&["2001:db8::2"]);
        assert!(!auto_picked(
            &ext(&["203.0.113.5", "2001:db8::2"]),
            &given,
            Some(&old)
        ));
    }
}
