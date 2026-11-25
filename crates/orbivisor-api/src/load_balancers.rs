//! 부하 분산기 API(계획 67 §7·§13·§14): `/load-balancers`. 원하는 상태만 Raft에 쓰고 리더 `OvnSync`가 NB의
//! `Load_Balancer`로 옮긴다. VIP·건강 검사 출발 주소는 VIP·멤버 네트워크 서브넷의 풀에서, 바깥 VIP는 라우터 바깥(공급자)
//! 네트워크의 유동 IP 범위(없으면 풀)에서 API가 고른다. 두 API 노드가 같은 주소를 고르면 Raft의 겹침 키
//! `("ip", "<네트워크>/<주소>")`가 하나를 거절하고 다시 고른다(라우터·유동 IP와 같다).
//!
//! 권한(§13): 부하 분산기는 네트워크 폴더에 산다. 쓰기는 그 객체(만들기는 폴더)의 `network.config`와 VIP 네트워크의
//! `network.assign`, NIC 멤버를 더하면 그 VM의 `vm.config`(남의 VM으로 트래픽을 보내지 않게), 주소 멤버는 그 서브넷
//! 네트워크의 `network.assign`, 바깥 VIP는 공급자 네트워크의 `network.assign`(범위 밖 주소는 루트 `cluster.admin`).
//! 보기는 `system.read`이고 읽을 수 없는 VM의 이름은 보이지 않는다. 멤버 건강은 SB `Service_Monitor`를 읽는다(`live`).

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_types::authz::Privilege;
use orbivisor_types::floating_ip::{FloatingIp, FloatingIpSpec};
use orbivisor_types::ipam;
use orbivisor_types::load_balancer::{
    LbExternal, LbListener, LbMember, LbMonitor, LbProtocol, LoadBalancer, LoadBalancerSpec,
    LoadBalancerStatus,
};
use orbivisor_types::network::{Network, NetworkSpec, Subnet};
use orbivisor_types::node::{Node, NodeSpec, SDN_SERVICES};
use orbivisor_types::resource::{Kind, Resource};
use orbivisor_types::router::{Router, RouterSpec};
use orbivisor_types::security_group::{Protocol, SecurityGroup, SecurityGroupSpec, nic_policy};
use orbivisor_types::vm::{MacAddr, Vm, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::authz::AuthzObject;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::ipam::{HeldIps, RETRIES, held_ips, is_ip_taken};
use crate::ovn_sync::{MonitorState, nb};
use crate::resources::{check_name, if_match, lookup};
use crate::routing::{ipam_error, is_admin, present, warning_header};
use crate::state::AppState;

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message).with_code("invalid_load_balancer")
}

fn not_routed(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message).with_code("lb_network_not_routed")
}

fn unreachable(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message).with_code("lb_member_unreachable")
}

/// 부하 분산기를 쓰려면 모든 노드가 `sdn-services`를 알고(409 `cluster_upgrading` — 검증보다 먼저) SDN이
/// 세워졌어야 한다(409 `sdn_not_ready`).
fn require_services(state: &AppState) -> Result<(), ApiError> {
    require_feature(&state.cluster, SDN_SERVICES)?;
    crate::sdn::require_ready(&state.cluster)?;
    Ok(())
}

/// 한 번 떠 쓰는 클러스터
struct World {
    lbs: Vec<LoadBalancer>,
    routers: Vec<Router>,
    fips: Vec<FloatingIp>,
    networks: Vec<Network>,
    vms: Vec<Vm>,
    nodes: Vec<Node>,
    groups: Vec<SecurityGroup>,
    /// 끝나지 않은 옮기기가 대상 네트워크에 잡아 둔 주소(리뷰 1 I2 — VIP·건강 검사 주소로 고르지 않는다)
    migrating: HeldIps,
}

impl World {
    fn load(state: &AppState) -> Result<Self, ApiError> {
        Self::load_from(&state.cluster)
    }

    fn load_from(c: &Cluster) -> Result<Self, ApiError> {
        Ok(World {
            migrating: crate::ipam::load_migration_held(c, None)?,
            lbs: c.list::<LoadBalancerSpec>()?.1,
            routers: c.list::<RouterSpec>()?.1,
            fips: c.list::<FloatingIpSpec>()?.1,
            networks: c.list::<NetworkSpec>()?.1,
            vms: c.list::<VmSpec>()?.1,
            nodes: c.list::<NodeSpec>()?.1,
            groups: c.list::<SecurityGroupSpec>()?.1,
        })
    }

    fn network(&self, name: &str) -> Option<&Network> {
        self.networks.iter().find(|n| n.name == name)
    }

    /// 네트워크 `network`를 인터페이스로 가진 라우터
    fn router_of(&self, network: &str) -> Option<&Router> {
        self.routers
            .iter()
            .find(|r| r.spec.interfaces.iter().any(|i| i.network == network))
    }

    /// 라우터 `router`의 인터페이스 네트워크
    fn interfaces<'a>(&'a self, router: &'a Router) -> impl Iterator<Item = &'a Network> + 'a {
        router
            .spec
            .interfaces
            .iter()
            .filter_map(|i| self.network(&i.network))
    }

    /// 네트워크 `network`에서 VM NIC·라우터 바깥·유동 IP·부하 분산기(`except` 말고)가 쓰는 주소
    fn used(&self, network: &str, except: Option<Uuid>) -> BTreeSet<IpAddr> {
        let mut held = held_ips(&self.routers, &self.fips, &self.lbs, except);
        crate::ipam::merge_held(&mut held, self.migrating.clone());
        crate::ipam::network_used(&self.vms, &held, network)
    }
}

/// 네트워크의 IPv4 서브넷(체계마다 하나 — `NetworkSpec` 검증)
fn v4_subnet(network: &Network) -> Option<&Subnet> {
    network.spec.subnets.iter().find(|s| s.is_ipv4())
}

/// VIP 네트워크의 규칙(§7.1): 오버레이 SDN, IPv4 서브넷이 있고 어떤 라우터의 인터페이스. 아니면 400
/// `lb_network_not_routed`. 그 라우터를 돌려준다.
fn vip_router<'a>(world: &'a World, spec: &LoadBalancerSpec) -> Result<&'a Router, ApiError> {
    let Some(n) = world.network(&spec.network) else {
        return Err(invalid(format!("network {:?} not found", spec.network)));
    };
    if !n.spec.is_sdn() || n.spec.is_provider() {
        return Err(not_routed(format!(
            "network {:?} is not an overlay SDN network; a load balancer VIP needs one",
            n.name
        )));
    }
    if v4_subnet(n).is_none() {
        return Err(not_routed(format!(
            "network {:?} has no IPv4 subnet; a load balancer VIP is an IPv4 address",
            n.name
        )));
    }
    world.router_of(&n.name).ok_or_else(|| {
        not_routed(format!(
            "network {:?} is not an interface of a router; attach it to a router first \
             (`orbivisorctl router interface add ROUTER {}`)",
            n.name, n.name
        ))
    })
}

/// 멤버 하나가 닿는 네트워크와 주소(§7.1): NIC 멤버는 그 NIC가 라우터 인터페이스 네트워크에 있고 그 서브넷 IPv4가
/// 있어야 하고, 주소 멤버는 인터페이스 서브넷 안이어야 한다. 아니면 까닭(`label`은 문구의 VM 이름).
fn member_target<'a>(
    world: &'a World,
    router: &'a Router,
    m: &LbMember,
    label: &dyn Fn(&Vm) -> String,
) -> Result<(&'a Network, Ipv4Addr), String> {
    let mut interfaces = world.interfaces(router);
    if let Some((vm, mac)) = m.nic() {
        let Some(v) = world.vms.iter().find(|v| v.id == vm) else {
            return Err(format!("VM {vm} not found"));
        };
        if v.spec.template {
            return Err(format!("{} is a template", label(v)));
        }
        let Some(nic) = v.spec.nics.iter().find(|n| n.mac == Some(mac)) else {
            return Err(format!("{} has no NIC with MAC {mac}", label(v)));
        };
        let Some(net) = interfaces.find(|n| n.name == nic.network) else {
            return Err(format!(
                "the NIC {mac} of {} is on network {:?}, which is not an interface of router {:?}",
                label(v),
                nic.network,
                router.name
            ));
        };
        let ip = nic.ips.iter().find_map(|ip| match ip {
            IpAddr::V4(a) if ipam::nic_subnet(&net.spec, *ip).is_some() => Some(*a),
            _ => None,
        });
        return ip.map(|ip| (net, ip)).ok_or_else(|| {
            format!(
                "the NIC {mac} of {} has no IPv4 address on network {:?}",
                label(v),
                net.name
            )
        });
    }
    let ip = m.ip.unwrap_or(Ipv4Addr::UNSPECIFIED);
    interfaces
        .find(|n| ipam::nic_subnet(&n.spec, IpAddr::V4(ip)).is_some_and(|s| s.is_ipv4()))
        .map(|n| (n, ip))
        .ok_or_else(|| {
            format!(
                "address {ip} is not in a subnet of an interface of router {:?}",
                router.name
            )
        })
}

/// 스펙을 네트워크와 함께 본다(§7.1): 스펙 검증(400 `invalid_load_balancer`), VIP 네트워크(400 `lb_network_not_routed`),
/// 바깥 VIP는 그 라우터의 바깥(400 `lb_network_not_routed`), 멤버가 닿음(400 `lb_member_unreachable`). 라우터를 돌려준다.
fn check_lb<'a>(
    world: &'a World,
    spec: &LoadBalancerSpec,
    label: &dyn Fn(&Vm) -> String,
) -> Result<&'a Router, ApiError> {
    spec.validate().map_err(|e| invalid(e.to_string()))?;
    let router = vip_router(world, spec)?;
    if let Some(e) = &spec.external
        && router.spec.external.as_ref().map(|x| &x.network) != Some(&e.network)
    {
        return Err(not_routed(format!(
            "the router {:?} of network {:?} has no external on {:?}",
            router.name, spec.network, e.network
        )));
    }
    for l in &spec.listeners {
        for m in &l.members {
            member_target(world, router, m, label)
                .map_err(|why| unreachable(format!("listener {}/{}: {why}", l.protocol, l.port)))?;
        }
    }
    Ok(router)
}

/// 건강 검사 출발 주소가 있어야 할 멤버 네트워크(건강 검사가 있는 리스너의 NIC 멤버 — §7.3)
fn monitor_networks(world: &World, router: &Router, spec: &LoadBalancerSpec) -> BTreeSet<String> {
    spec.listeners
        .iter()
        .filter(|l| l.health.is_some())
        .flat_map(|l| l.members.iter())
        .filter(|m| m.nic().is_some())
        .filter_map(|m| member_target(world, router, m, &|_| String::new()).ok())
        .map(|(n, _)| n.name.clone())
        .collect()
}

/// VIP·바깥 VIP·건강 검사 주소를 채운다(§6·§7.3·§7.4). `old`는 지금 저장된 스펙(같은 네트워크의 주소는 그대로 쓴다),
/// `me`는 그 부하 분산기 id(자기 주소를 쓴 것으로 세지 않음). VIP를 직접 주면 서브넷 풀 안의 빈 주소, 바깥 VIP를 직접
/// 주면 유동 IP 범위(없으면 풀) 안이어야 한다(루트 `cluster.admin`은 서브넷 안 어디든 — 라우터 바깥과 같다). API가 하나라도
/// 골랐으면 참(그때만 Raft의 주소 겹침 거절을 다시 고르기로 받는다).
fn fill_addresses(
    world: &World,
    router: &Router,
    spec: &mut LoadBalancerSpec,
    old: Option<&LoadBalancerSpec>,
    me: Option<Uuid>,
    admin: bool,
) -> Result<bool, ApiError> {
    let mut picked = false;
    // 이번에 고른 주소(같은 네트워크의 다음 고르기가 쓴 것으로 센다)
    let mut taken: BTreeMap<String, BTreeSet<IpAddr>> = BTreeMap::new();
    let used = |network: &str, taken: &BTreeMap<String, BTreeSet<IpAddr>>| {
        let mut u = world.used(network, me);
        u.extend(taken.get(network).into_iter().flatten().copied());
        u
    };
    // 안 VIP
    let vip_net = world
        .network(&spec.network)
        .ok_or_else(|| invalid(format!("network {:?} not found", spec.network)))?;
    let subnet = v4_subnet(vip_net).ok_or_else(|| invalid("no IPv4 subnet"))?;
    let keep = old
        .filter(|o| o.network == spec.network)
        .and_then(|o| o.vip);
    let vip = match spec.vip {
        Some(v) if Some(v) == keep => v,
        Some(v) => {
            if !ipam::in_nic_range(subnet, IpAddr::V4(v)) {
                return Err(invalid(format!(
                    "VIP {v} is outside the address pools of subnet {} of network {:?}",
                    subnet.cidr, vip_net.name
                )));
            }
            to_v4(ipam::allocate(
                subnet,
                &used(&vip_net.name, &taken),
                Some(IpAddr::V4(v)),
            ))
            .map_err(|e| ipam_error(&vip_net.name, e))?
        }
        None => match keep {
            Some(k) => k,
            None => {
                picked = true;
                to_v4(ipam::allocate(subnet, &used(&vip_net.name, &taken), None))
                    .map_err(|e| ipam_error(&vip_net.name, e))?
            }
        },
    };
    spec.vip = Some(vip);
    taken
        .entry(vip_net.name.clone())
        .or_default()
        .insert(IpAddr::V4(vip));
    // 바깥 VIP
    if let Some(e) = &mut spec.external {
        let net = world
            .network(&e.network)
            .ok_or_else(|| invalid(format!("network {:?} not found", e.network)))?;
        let Some(subnet) = v4_subnet(net) else {
            return Err(invalid(format!(
                "provider network {:?} has no IPv4 subnet",
                net.name
            )));
        };
        let keep = old
            .and_then(|o| o.external.as_ref())
            .filter(|o| o.network == e.network)
            .and_then(|o| o.ip);
        let ip = match e.ip {
            Some(v) if Some(v) == keep => v,
            Some(v) => {
                if !admin && !ipam::in_floating_range(subnet, IpAddr::V4(v)) {
                    return Err(invalid(format!(
                        "external VIP {v} is outside the {} of subnet {} of network {:?}; only a cluster \
                         administrator can choose an address outside them",
                        if subnet.floating_pools.is_empty() {
                            "address pools"
                        } else {
                            "floating IP ranges"
                        },
                        subnet.cidr,
                        net.name
                    )));
                }
                to_v4(ipam::allocate_from(
                    subnet,
                    &used(&net.name, &taken),
                    Some(IpAddr::V4(v)),
                ))
                .map_err(|e| ipam_error(&net.name, e))?
            }
            None => match keep {
                Some(k) => k,
                None => {
                    picked = true;
                    to_v4(ipam::allocate_from(subnet, &used(&net.name, &taken), None))
                        .map_err(|e| ipam_error(&net.name, e))?
                }
            },
        };
        e.ip = Some(ip);
        taken
            .entry(net.name.clone())
            .or_default()
            .insert(IpAddr::V4(ip));
    }
    // 건강 검사 출발 주소: 필요한 멤버 네트워크마다 하나(있던 것은 그대로, 필요 없어진 것은 놓는다)
    let mut monitors = Vec::new();
    for network in monitor_networks(world, router, spec) {
        let keep = old.and_then(|o| o.monitor_for(&network));
        let ip = match keep {
            Some(k) => k,
            None => {
                let net = world
                    .network(&network)
                    .ok_or_else(|| invalid(format!("network {network:?} not found")))?;
                let subnet = v4_subnet(net).ok_or_else(|| invalid("no IPv4 subnet"))?;
                picked = true;
                to_v4(ipam::allocate(subnet, &used(&network, &taken), None))
                    .map_err(|e| ipam_error(&network, e))?
            }
        };
        taken
            .entry(network.clone())
            .or_default()
            .insert(IpAddr::V4(ip));
        monitors.push(LbMonitor { network, ip });
    }
    spec.monitors = monitors;
    Ok(picked)
}

/// VM `vm`의 NIC가 네트워크를 바꾼 뒤(`PUT /vms`) 그 VM의 NIC를 멤버로 가진 부하 분산기의 건강 검사 출발 주소를 다시
/// 고른다(리뷰 2 m4 — 멤버가 옮겨 간 인터페이스 네트워크에 출발 주소가 없으면 OVN이 그 멤버를 늘 건강하다고 보고, 비게 된
/// 네트워크의 주소는 IPAM에 잡힌 채 남는다). 필요한 네트워크 집합이 같으면 쓰지 않는다. 고르지 못하면 경고 로그만 남긴다
/// (VM 쓰기는 이미 됐다 — 다음 부하 분산기 쓰기가 다시 고른다).
pub(crate) async fn refresh_monitors(cluster: &Cluster, vm: Uuid) {
    for _ in 0..RETRIES {
        let world = match World::load_from(cluster) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e.message, "cannot refresh load balancer health-check sources");
                return;
            }
        };
        let mut retry = false;
        for lb in world.lbs.iter().filter(|l| {
            l.spec
                .listeners
                .iter()
                .flat_map(|l| l.members.iter())
                .any(|m| m.nic().is_some_and(|(v, _)| v == vm))
        }) {
            let Ok(router) = vip_router(&world, &lb.spec) else {
                continue;
            };
            let want = monitor_networks(&world, router, &lb.spec);
            let have: BTreeSet<String> =
                lb.spec.monitors.iter().map(|m| m.network.clone()).collect();
            if want == have {
                continue;
            }
            let mut spec = lb.spec.clone();
            if let Err(e) =
                fill_addresses(&world, router, &mut spec, Some(&lb.spec), Some(lb.id), true)
            {
                tracing::warn!(lb = %lb.name, error = %e.message, "cannot pick load balancer health-check sources");
                continue;
            }
            match cluster
                .update_spec(lb.id, Some(lb.revision), lb.labels.clone(), &spec)
                .await
            {
                Ok(_) => {
                    tracing::info!(lb = %lb.name, "moved load balancer health-check sources with a member NIC")
                }
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => retry = true,
                Err(e) if is_ip_taken(&e) => retry = true,
                Err(e) => {
                    tracing::warn!(lb = %lb.name, error = %e, "cannot store load balancer health-check sources")
                }
            }
        }
        if !retry {
            return;
        }
    }
}

fn to_v4(r: Result<IpAddr, ipam::IpamError>) -> Result<Ipv4Addr, ipam::IpamError> {
    r.map(|ip| match ip.to_canonical() {
        IpAddr::V4(a) => a,
        // IPv4 서브넷에서 고른 주소다.
        IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
    })
}

/// 보안 그룹 경고(§7.3): 건강 검사 주소 → 멤버 포트와 클라이언트 → 멤버 포트를 멤버 NIC의 그룹이 막으면 문구(막지는
/// 않는다 — 일부러 닫아 둘 수 있다). 그룹이 없는 NIC는 거르지 않는다. 멤버 하나의 경고들.
fn member_warnings(
    world: &World,
    spec: &LoadBalancerSpec,
    l: &LbListener,
    m: &LbMember,
    network: &str,
) -> Vec<String> {
    let Some((vm, mac)) = m.nic() else {
        return Vec::new();
    };
    let Some(nic) = world
        .vms
        .iter()
        .find(|v| v.id == vm)
        .and_then(|v| v.spec.nics.iter().find(|n| n.mac == Some(mac)))
    else {
        return Vec::new();
    };
    if nic.security_groups.is_empty() {
        return Vec::new();
    }
    let policy = nic_policy(&nic.security_groups, &world.groups);
    let proto = match l.protocol {
        LbProtocol::Tcp => Protocol::Tcp,
        LbProtocol::Udp => Protocol::Udp,
    };
    let mut out = Vec::new();
    // 클라이언트: 이 포트를 받는 `in accept` 규칙의 출발 범위(없으면 아무 주소) 가운데 하나라도 받는지
    let candidates: Vec<IpAddr> = policy
        .rules
        .iter()
        .filter(|r| r.direction == orbivisor_types::security_group::Direction::In)
        .filter(|r| r.action == orbivisor_types::security_group::Action::Accept)
        .filter(|r| r.protocol.is_none_or(|p| p == proto))
        .filter(|r| r.ports.is_none_or(|p| (p.start..=p.end).contains(&m.port)))
        .map(|r| {
            r.remote
                .map_or(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), |c| c.addr)
        })
        .collect();
    if !candidates
        .iter()
        .any(|src| policy.admits_in(proto, m.port, *src))
    {
        out.push(format!(
            "the security groups of the member NIC {mac} do not admit {}/{} from clients",
            l.protocol, m.port
        ));
    }
    if l.health.is_some()
        && let Some(src) = spec.monitor_for(network)
        && !policy.admits_in(proto, m.port, IpAddr::V4(src))
    {
        out.push(format!(
            "the security groups of the member NIC {mac} block health checks from {src} to {}/{}",
            l.protocol, m.port
        ));
    }
    out
}

/// 쓰기 응답의 경고: 멤버 보안 그룹, 바깥 VIP의 게이트웨이 후보 없음
fn write_warnings(world: &World, router: &Router, spec: &LoadBalancerSpec) -> Vec<String> {
    let mut out = Vec::new();
    for l in &spec.listeners {
        for m in &l.members {
            if let Ok((net, _)) = member_target(world, router, m, &|_| String::new()) {
                out.extend(member_warnings(world, spec, l, m, &net.name));
            }
        }
    }
    if let Some(reason) = external_unrouted(world, router, spec) {
        out.push(format!("the external VIP is not reachable yet: {reason}"));
    }
    out
}

/// 바깥 VIP가 닿지 않는 까닭(§7.4 — 게이트웨이 후보가 없으면 DGP가 꺼져 있다)
fn external_unrouted(world: &World, router: &Router, spec: &LoadBalancerSpec) -> Option<String> {
    let e = spec.external.as_ref()?;
    if router.spec.external.as_ref().map(|x| &x.network) != Some(&e.network) {
        return Some(format!(
            "the router {:?} no longer has an external on {:?}",
            router.name, e.network
        ));
    }
    let physnet = world.network(&e.network)?.spec.physnet()?;
    nb::gateway_candidates(&world.nodes, physnet)
        .is_empty()
        .then(|| format!("no gateway node has physnet {physnet} ready"))
}

// ---- 보기 ----

/// VIP·바깥 VIP의 지금 모양(Raft에서 셈)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum LbState {
    /// 라우터에 붙어 닿는다.
    Ready,
    /// 닿지 않는다(라우터를 뗐거나 게이트웨이 후보가 없음).
    Unrouted { reason: String },
}

/// 멤버의 건강(SB `Service_Monitor`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MemberHealth {
    /// 건강 검사가 답을 받는다.
    Online,
    /// 건강 검사가 실패해 흐름에서 빠졌다.
    Offline,
    /// 검사를 할 수 없다(OVN이 알린 오류).
    Error,
    /// 건강 검사가 없거나(주소 멤버 포함) 아직 결과가 없거나 OVN을 읽지 않았다 — 멤버는 쓰인다.
    Unchecked,
    /// 멤버가 닿지 않아 흐름에서 빠졌다(VM·NIC가 없음, 주소 없음, 라우터 밖 — `reason`).
    Missing,
}

/// 멤버 하나의 보기
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LbMemberView {
    /// 리스너 `tcp/80`
    pub listener: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub vm: Option<Uuid>,
    /// VM 이름(부른 사용자가 읽을 수 있을 때만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub vm_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, nullable = false, default)]
    pub mac: Option<MacAddr>,
    /// 멤버 주소(NIC 멤버는 그 NIC의 IPv4, 찾지 못하면 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, format = "ipv4", nullable = false, default)]
    pub ip: Option<Ipv4Addr>,
    pub port: u16,
    pub health: MemberHealth,
    /// `missing`의 까닭
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub reason: Option<String>,
    /// 보안 그룹이 클라이언트·건강 검사를 막는다는 경고
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub warning: Option<String>,
}

/// `GET /load-balancers/{key}`: 부하 분산기와 VIP·바깥 VIP·멤버의 지금 모양
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LoadBalancerView {
    #[serde(flatten)]
    pub lb: Resource<LoadBalancerSpec, LoadBalancerStatus>,
    pub vip_state: LbState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub external_state: Option<LbState>,
    /// 리스너 차례의 멤버
    pub members: Vec<LbMemberView>,
    /// SB를 읽지 못한 까닭(멤버 건강이 `unchecked`로 남는다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub live_error: Option<String>,
}

/// `GET /load-balancers`
#[derive(Debug, Serialize, ToSchema)]
pub struct LoadBalancerList {
    pub revision: u64,
    pub items: Vec<LoadBalancerView>,
}

/// 보기의 질의
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct LbLiveQuery {
    /// 거짓이면 OVN SB를 읽지 않는다(건강 검사 멤버가 `unchecked`). 기본 참
    #[serde(default = "yes")]
    pub live: bool,
}

fn yes() -> bool {
    true
}

/// SB 건강 검사 결과의 열쇠(멤버 주소, 포트, 프로토콜, 논리 포트) → 상태
type Monitors = BTreeMap<(String, u16, String, String), String>;

fn monitor_map(rows: Vec<MonitorState>) -> Monitors {
    rows.into_iter()
        .map(|m| {
            let proto = if m.protocol.is_empty() {
                "tcp".to_string()
            } else {
                m.protocol
            };
            ((m.ip, m.port, proto, m.logical_port), m.status)
        })
        .collect()
}

async fn read_monitors(state: &AppState) -> Result<Monitors, String> {
    let (config, centrals, _) = crate::sdn::ovn_reader(state)?;
    Box::pin(crate::ovn_sync::read_monitors(config, &centrals))
        .await
        .map(monitor_map)
        .map_err(|e| e.to_string())
}

fn lb_view(
    world: &World,
    sees_vm: &dyn Fn(&Vm) -> bool,
    monitors: Option<&Monitors>,
    lb: LoadBalancer,
) -> LoadBalancerView {
    let label = |v: &Vm| {
        if sees_vm(v) {
            format!("VM {:?}", v.name)
        } else {
            "the member VM".to_string()
        }
    };
    let router = vip_router(world, &lb.spec).ok();
    let vip_state = match (&router, lb.spec.vip) {
        (Some(_), Some(_)) => LbState::Ready,
        (None, _) => LbState::Unrouted {
            reason: vip_router(world, &lb.spec)
                .err()
                .map_or_else(String::new, |e| e.message),
        },
        (Some(_), None) => LbState::Unrouted {
            reason: "no VIP".into(),
        },
    };
    let external_state = lb.spec.external.as_ref().map(|_| match router {
        Some(r) => match external_unrouted(world, r, &lb.spec) {
            None => LbState::Ready,
            Some(reason) => LbState::Unrouted { reason },
        },
        None => LbState::Unrouted {
            reason: "the VIP network is not routed".into(),
        },
    });
    let mut members = Vec::new();
    for l in &lb.spec.listeners {
        for m in &l.members {
            let vm = m.vm.and_then(|id| world.vms.iter().find(|v| v.id == id));
            let mut view = LbMemberView {
                listener: format!("{}/{}", l.protocol, l.port),
                vm: m.vm,
                vm_name: vm.filter(|v| sees_vm(v)).map(|v| v.name.clone()),
                mac: m.mac,
                ip: m.ip,
                port: m.port,
                health: MemberHealth::Unchecked,
                reason: None,
                warning: None,
            };
            let target = match router {
                Some(r) => member_target(world, r, m, &label),
                None => Err("the VIP network is not routed".into()),
            };
            match target {
                Err(reason) => {
                    view.health = MemberHealth::Missing;
                    view.reason = Some(reason);
                }
                Ok((net, ip)) => {
                    view.ip = Some(ip);
                    let warnings = member_warnings(world, &lb.spec, l, m, &net.name);
                    view.warning = (!warnings.is_empty()).then(|| warnings.join("; "));
                    if let (Some((vmid, mac)), Some(_), Some(seen)) = (m.nic(), l.health, monitors)
                    {
                        let key = (
                            ip.to_string(),
                            m.port,
                            l.protocol.as_str().to_string(),
                            orbivisor_types::sdn::port_name(vmid, mac),
                        );
                        view.health = match seen.get(&key).map(String::as_str) {
                            Some("online") => MemberHealth::Online,
                            Some("offline") => MemberHealth::Offline,
                            Some("error") => MemberHealth::Error,
                            _ => MemberHealth::Unchecked,
                        };
                    }
                }
            }
            // 읽을 수 없는 VM의 멤버는 VM id·MAC·주소를 내지 않는다(리뷰 2 m1 — 토폴로지와 같게). 건강은 남긴다.
            if vm.is_some_and(|v| !sees_vm(v)) {
                view.vm = None;
                view.mac = None;
                view.ip = None;
            }
            members.push(view);
        }
    }
    LoadBalancerView {
        lb,
        vip_state,
        external_state,
        members,
        live_error: None,
    }
}

fn sees_vm<'a>(state: &'a AppState, principal: &'a Principal) -> impl Fn(&Vm) -> bool + 'a {
    move |v: &Vm| {
        state
            .authz
            .allows(principal, Privilege::SystemRead, v.authz_target())
    }
}

/// SB의 건강 검사를 읽을지와 결과(`live`이고 건강 검사가 있는 부하 분산기가 있을 때만 읽는다)
async fn live_monitors(
    state: &AppState,
    live: bool,
    lbs: &[LoadBalancer],
) -> (Option<Monitors>, Option<String>) {
    if !live
        || !lbs
            .iter()
            .any(|l| l.spec.listeners.iter().any(|x| x.health.is_some()))
    {
        return (None, None);
    }
    match Box::pin(read_monitors(state)).await {
        Ok(m) => (Some(m), None),
        Err(e) => (None, Some(e)),
    }
}

/// 부하 분산기들의 보기(토폴로지 — 계획 67 §12.3). `live`면 SB 건강 검사를 한 번 읽는다.
pub(crate) async fn views(
    state: &AppState,
    principal: &Principal,
    lbs: Vec<LoadBalancer>,
    live: bool,
) -> Result<Vec<LoadBalancerView>, ApiError> {
    if lbs.is_empty() {
        return Ok(Vec::new());
    }
    let world = World::load(state)?;
    let (monitors, _) = live_monitors(state, live, &lbs).await;
    let sees = sees_vm(state, principal);
    Ok(lbs
        .into_iter()
        .map(|l| lb_view(&world, &sees, monitors.as_ref(), l))
        .collect())
}

/// 볼 수 있는 부하 분산기와 VIP·멤버의 지금 모양. `live=false`면 OVN을 읽지 않는다.
#[utoipa::path(get, path = "/load-balancers", tag = "load-balancers", params(LbLiveQuery),
    responses((status = 200, body = LoadBalancerList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_load_balancers(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(query): Query<LbLiveQuery>,
) -> Result<Json<LoadBalancerList>, ApiError> {
    let (revision, lbs) = state.cluster.list::<LoadBalancerSpec>()?;
    let mut lbs = state.authz.filter(&principal, lbs)?;
    if lbs.is_empty() {
        return Ok(Json(LoadBalancerList {
            revision,
            items: Vec::new(),
        }));
    }
    lbs.sort_by(|a, b| a.name.cmp(&b.name));
    let world = World::load(&state)?;
    let (monitors, error) = live_monitors(&state, query.live, &lbs).await;
    let sees = sees_vm(&state, &principal);
    let items = lbs
        .into_iter()
        .map(|l| {
            let mut v = lb_view(&world, &sees, monitors.as_ref(), l);
            v.live_error.clone_from(&error);
            v
        })
        .collect();
    Ok(Json(LoadBalancerList { revision, items }))
}

/// 부하 분산기 하나와 VIP·멤버의 지금 모양(멤버 건강은 SB `Service_Monitor`). `live=false`면 OVN을 읽지 않는다.
#[utoipa::path(get, path = "/load-balancers/{key}", tag = "load-balancers",
    params(("key" = String, Path, description = "id or name"), LbLiveQuery),
    responses((status = 200, body = LoadBalancerView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_load_balancer(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(query): Query<LbLiveQuery>,
) -> Result<Json<LoadBalancerView>, ApiError> {
    // 경로표가 이 부하 분산기를 읽을 수 있는지 봤다.
    let lb = lookup::<LoadBalancerSpec>(&state.cluster, &key)?;
    let world = World::load(&state)?;
    let (monitors, error) = live_monitors(&state, query.live, std::slice::from_ref(&lb)).await;
    let sees = sees_vm(&state, &principal);
    let mut view = lb_view(&world, &sees, monitors.as_ref(), lb);
    view.live_error = error;
    Ok(Json(view))
}

/// `POST /load-balancers`
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLoadBalancer {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// `monitors`는 API가 채운다(주면 400).
    pub spec: LoadBalancerSpec,
}

/// 멤버를 더하는 권한(§13): 새 NIC 멤버의 VM마다 `vm.config`, 새 주소 멤버는 그 서브넷 네트워크의 `network.assign`
fn require_members(
    state: &AppState,
    principal: &Principal,
    world: &World,
    router: &Router,
    spec: &LoadBalancerSpec,
    old: Option<&LoadBalancerSpec>,
) -> Result<(), ApiError> {
    let had = |m: &LbMember| {
        old.is_some_and(|o| {
            o.listeners
                .iter()
                .flat_map(|l| l.members.iter())
                .any(|x| x.nic() == m.nic() && x.ip == m.ip)
        })
    };
    let mut vms = BTreeSet::new();
    let mut networks = BTreeSet::new();
    for m in spec.listeners.iter().flat_map(|l| l.members.iter()) {
        if had(m) {
            continue;
        }
        match m.nic() {
            Some((vm, _)) => {
                vms.insert(vm);
            }
            None => {
                if let Ok((n, _)) = member_target(world, router, m, &|_| String::new()) {
                    networks.insert(n.name.clone());
                }
            }
        }
    }
    for id in vms {
        if let Some(vm) = world.vms.iter().find(|v| v.id == id) {
            state.authz.require(principal, Privilege::VmConfig, vm)?;
        }
    }
    state
        .authz
        .require_networks(principal, networks.iter().map(String::as_str))
}

/// 감사의 리스너 설명: `tcp/80 → web1,web2`
fn listener_note(world: &World, l: &LbListener) -> String {
    let members: Vec<String> = l
        .members
        .iter()
        .map(|m| match m.nic() {
            Some((vm, mac)) => world
                .vms
                .iter()
                .find(|v| v.id == vm)
                .map_or_else(|| format!("{vm}/{mac}"), |v| v.name.clone()),
            None => m.ip.map(|ip| ip.to_string()).unwrap_or_default(),
        })
        .collect();
    format!("{}/{} → {}", l.protocol, l.port, members.join(","))
}

/// 부하 분산기를 만든다(계획 67 §7): 폴더의 `network.config`, VIP 네트워크의 `network.assign`, NIC 멤버의 VM마다
/// `vm.config`, 주소 멤버의 네트워크와 바깥 VIP 네트워크의 `network.assign`. VIP·건강 검사 주소·바깥 VIP는 API가
/// 고른다(§6). 옛 노드가 남았으면 409 `cluster_upgrading`, SDN이 없으면 409 `sdn_not_ready`, 스펙은 400
/// `invalid_load_balancer`, VIP 네트워크·바깥이 라우터와 맞지 않으면 400 `lb_network_not_routed`, 닿지 않는 멤버는 400
/// `lb_member_unreachable`, 주소는 409 `ip_in_use`·`ip_exhausted`, 같은 VIP·포트는 409, 폴더 할당량 `load_balancers`(바깥
/// VIP는 `floating_ips`도)는 409 `quota_exceeded`. 멤버 보안 그룹이 클라이언트·건강 검사를 막거나 바깥 VIP의 게이트웨이
/// 후보가 없으면 만들고 `orbivisor-warning`으로 알린다.
#[utoipa::path(post, path = "/load-balancers", tag = "load-balancers",
    request_body = CreateLoadBalancer,
    responses((status = 201, body = Resource<LoadBalancerSpec, LoadBalancerStatus>,
        headers(("orbivisor-warning" = String, description = "security groups block clients or health checks, or no gateway for the external VIP (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_load_balancer(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateLoadBalancer>,
) -> Result<(StatusCode, HeaderMap, Json<LoadBalancer>), ApiError> {
    require_services(&state)?;
    check_name(&req.name)?;
    state.authz.require_new(
        &principal,
        Privilege::NetworkConfig,
        Kind::LoadBalancer,
        req.spec.folder,
    )?;
    if !req.spec.monitors.is_empty() {
        return Err(invalid("monitors are chosen by the server; leave them out"));
    }
    state.authz.require_networks(
        &principal,
        std::iter::once(req.spec.network.as_str())
            .chain(req.spec.external.iter().map(|e| e.network.as_str())),
    )?;
    let admin = is_admin(&state, &principal);
    let sees = sees_vm(&state, &principal);
    let label = |v: &Vm| {
        if sees(v) {
            format!("VM {:?}", v.name)
        } else {
            "the member VM".to_string()
        }
    };
    for _ in 0..RETRIES {
        let world = World::load(&state)?;
        let mut spec = req.spec.clone();
        let router = check_lb(&world, &spec, &label)?;
        require_members(&state, &principal, &world, router, &spec, None)?;
        let picked = fill_addresses(&world, router, &mut spec, None, None, admin)?;
        let mut lb = Resource::new(req.name.clone(), spec);
        lb.labels = req.labels.clone();
        match state.cluster.create(&lb).await {
            Ok(revision) => lb.revision = revision,
            Err(e) if is_ip_taken(&e) && picked => continue,
            Err(e) => return Err(e.into()),
        }
        let mut detail = format!(
            "lb create {} {} vip {}",
            lb.name,
            lb.spec.network,
            lb.spec.vip.map(|v| v.to_string()).unwrap_or_default()
        );
        for l in &lb.spec.listeners {
            detail.push_str(&format!(" {}", listener_note(&world, l)));
        }
        if let Some(e) = &lb.spec.external {
            detail.push_str(&format!(
                ", lb external {} {} {}",
                lb.name,
                e.network,
                e.ip.map(|v| v.to_string()).unwrap_or_default()
            ));
        }
        audit.detail(detail);
        let warnings = write_warnings(&world, router, &lb.spec);
        return Ok((StatusCode::CREATED, warning_header(&warnings), Json(lb)));
    }
    Err(ApiError::conflict(
        "ip_in_use",
        "other requests kept taking the chosen addresses; try again",
    ))
}

/// `PATCH /load-balancers/{key}`: 준 칸만 바꾼다(리스너는 통째로). VIP 네트워크는 바꿀 수 없다(새로 만든다).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadBalancerPatch {
    /// 빈 글이면 설명을 지운다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub description: Option<String>,
    /// 바깥 VIP, `null`은 떼기. 같은 네트워크에 `ip`를 주지 않으면 지금 주소를 그대로 쓴다.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<LbExternal>)]
    pub external: Option<Option<LbExternal>>,
    /// 리스너 전체
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub listeners: Option<Vec<LbListener>>,
    /// 출발지 고정(초), `null`은 끄기
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<u32>)]
    pub affinity_secs: Option<Option<u32>>,
}

/// 감사 설명: 바뀐 것
fn lb_change(world: &World, name: &str, old: &LoadBalancerSpec, new: &LoadBalancerSpec) -> String {
    let mut parts = Vec::new();
    if old.listeners != new.listeners {
        let notes: Vec<String> = new
            .listeners
            .iter()
            .map(|l| listener_note(world, l))
            .collect();
        parts.push(format!("lb listeners {name} {}", notes.join(" ")));
    }
    if old.external != new.external {
        parts.push(match &new.external {
            Some(e) => format!(
                "lb external {name} {} {}",
                e.network,
                e.ip.map(|v| v.to_string()).unwrap_or_default()
            ),
            None => format!("lb external clear {name}"),
        });
    }
    if old.affinity_secs != new.affinity_secs {
        parts.push(match new.affinity_secs {
            Some(a) => format!("lb affinity {name} {a}"),
            None => format!("lb affinity clear {name}"),
        });
    }
    if parts.is_empty() {
        parts.push(format!("lb update {name}"));
    }
    parts.join(", ")
}

/// 부하 분산기의 설명·바깥 VIP·리스너·출발지 고정을 바꾼다(`If-Match`로 리비전 확인, 없으면 읽은 리비전에 쓰고 그새
/// 바뀌었으면 다시 읽는다). 그 객체의 `network.config`(경로표), VIP 네트워크의 `network.assign`, 새 NIC 멤버의 VM마다
/// `vm.config`, 새 주소 멤버·새 바깥 네트워크의 `network.assign`. 검사·주소·경고는 만들기와 같다. 건강 검사 주소는
/// 필요한 멤버 네트워크만 남긴다.
#[utoipa::path(patch, path = "/load-balancers/{key}", tag = "load-balancers",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = LoadBalancerPatch,
    responses((status = 200, body = Resource<LoadBalancerSpec, LoadBalancerStatus>,
        headers(("orbivisor-warning" = String, description = "security groups block clients or health checks, or no gateway for the external VIP (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_load_balancer(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<LoadBalancerPatch>,
) -> Result<(HeaderMap, Json<LoadBalancer>), ApiError> {
    require_services(&state)?;
    let expected = if_match(&headers)?;
    let admin = is_admin(&state, &principal);
    let sees = sees_vm(&state, &principal);
    let label = |v: &Vm| {
        if sees(v) {
            format!("VM {:?}", v.name)
        } else {
            "the member VM".to_string()
        }
    };
    for _ in 0..RETRIES * 2 {
        let lb = lookup::<LoadBalancerSpec>(&state.cluster, &key)?;
        let world = World::load(&state)?;
        let old = &lb.spec;
        let mut spec = old.clone();
        if let Some(d) = &req.description {
            spec.description = (!d.is_empty()).then(|| d.clone());
        }
        if let Some(e) = &req.external {
            spec.external.clone_from(e);
        }
        if let Some(l) = &req.listeners {
            spec.listeners.clone_from(l);
        }
        if let Some(a) = req.affinity_secs {
            spec.affinity_secs = a;
        }
        state.authz.require_networks(
            &principal,
            std::iter::once(spec.network.as_str()).chain(
                spec.external
                    .iter()
                    .filter(|e| old.external.as_ref().map(|o| &o.network) != Some(&e.network))
                    .map(|e| e.network.as_str()),
            ),
        )?;
        let router = check_lb(&world, &spec, &label)?;
        require_members(&state, &principal, &world, router, &spec, Some(old))?;
        let picked = fill_addresses(&world, router, &mut spec, Some(old), Some(lb.id), admin)?;
        let revision = expected.unwrap_or(lb.revision);
        match state
            .cluster
            .update_spec(lb.id, Some(revision), lb.labels.clone(), &spec)
            .await
        {
            Ok(written) => {
                audit.detail(lb_change(&world, &lb.name, old, &spec));
                let warnings = write_warnings(&world, router, &spec);
                let mut out = lb;
                out.spec = spec;
                out.revision = written;
                return Ok((warning_header(&warnings), Json(out)));
            }
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() => {}
            Err(e) if is_ip_taken(&e) && picked => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the load balancer kept changing while being updated; try again",
    ))
}

/// 부하 분산기를 지운다(언제든 — VIP·바깥 VIP·건강 검사 주소가 풀린다).
#[utoipa::path(delete, path = "/load-balancers/{key}", tag = "load-balancers",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_load_balancer(
    State(state): State<Arc<AppState>>,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let lb = lookup::<LoadBalancerSpec>(&state.cluster, &key)?;
    state
        .cluster
        .delete::<LoadBalancerSpec>(lb.id, if_match(&headers)?)
        .await?;
    crate::permissions::drop_orphans(&state.cluster).await;
    audit.detail(format!(
        "lb delete {} ({})",
        lb.name,
        lb.spec.vip.map(|v| v.to_string()).unwrap_or_default()
    ));
    Ok(StatusCode::NO_CONTENT)
}
