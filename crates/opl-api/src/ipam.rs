//! SDN NIC 주소 할당(계획 65 §9). 주소의 출처는 Raft의 VM 스펙(`NicSpec.ips`)이고, API가 VM을 만들거나 바꿀 때
//! ([`assign`])와 Task가 새 VM을 만들 때(복제·배포·가져오기·복원, [`assign_new`]) 채운다. 쓰고 있는 주소는 그
//! 네트워크를 쓰는 템플릿 아닌 VM 스펙의 `ips`와 예약이다. 두 API 노드가 같은 주소를 동시에 고르면 Raft 상태 머신이
//! 하나를 `InUse { field: "ip" }`로 거절하고([`is_ip_taken`]) 부른 쪽이 다시 고른다([`RETRIES`]번까지).
//!
//! 공급자 네트워크(계획 66 §8)에서는 라우터 외부 주소와 유동 IP도 같은 주소 공간을 쓴다([`HeldIps`] — Raft의 겹침 키도
//! 셋이 하나다). NIC는 유동 IP 풀에서 저절로 받지 않고, SLAAC 계열 IPv6 서브넷의 NIC 주소는 MAC의 EUI-64다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::cloud_init::CloudInitNet;
use opl_types::floating_ip::{FloatingIp, FloatingIpSpec};
use opl_types::ipam::{self, IpamError};
use opl_types::network::{Network, NetworkSpec, Subnet};
use opl_types::router::{Router, RouterSpec};
use opl_types::vm::{MacAddr, Vm, VmSpec};

use opl_types::authz::Privilege;

use crate::auth::Principal;
use crate::authz::AuthzObject;
use crate::cluster::require_feature;
use crate::error::ApiError;
use crate::state::AppState;

/// 주체 `principal`이 읽을 수 있는 VM인지(오류 문구에 이름을 댈지). 주체가 없으면(요청 밖) 대지 않는다.
pub(crate) fn readable<'a>(
    state: &'a AppState,
    principal: Option<&'a Principal>,
) -> impl Fn(&Vm) -> bool + 'a {
    move |vm: &Vm| {
        principal.is_some_and(|p| {
            state
                .authz
                .allows(p, Privilege::SystemRead, vm.authz_target())
        })
    }
}

/// Raft가 주소 겹침으로 거절했을 때 다시 고르는 횟수
pub(crate) const RETRIES: usize = 5;

/// 쓰기가 다른 VM이 그새 잡은 NIC 주소로 거절되었는지(다시 골라 볼 일)
pub(crate) fn is_ip_taken(e: &ClusterError) -> bool {
    matches!(e, ClusterError::Rejected(Rejection::InUse { field, .. }) if field == "ip")
}

/// 네트워크 이름 → 라우터 외부 주소·유동 IP가 쓰는 주소(계획 66 §8). NIC 주소와 같은 주소 공간이다.
pub(crate) type HeldIps = BTreeMap<String, BTreeSet<IpAddr>>;

/// 라우터들 `routers`의 외부 주소와 유동 IP들 `fips`의 주소(`except`의 것은 뺀다 — 자기 자신을 고쳐 쓸 때)
pub(crate) fn held_ips(
    routers: &[Router],
    fips: &[FloatingIp],
    except: Option<uuid::Uuid>,
) -> HeldIps {
    let mut out = HeldIps::new();
    for r in routers.iter().filter(|r| Some(r.id) != except) {
        if let Some(e) = &r.spec.external {
            out.entry(e.network.clone())
                .or_default()
                .extend(e.ips.iter().map(|ip| ip.to_canonical()));
        }
    }
    for f in fips.iter().filter(|f| Some(f.id) != except) {
        if let Some(a) = f.spec.address {
            out.entry(f.spec.network.clone())
                .or_default()
                .insert(a.to_canonical());
        }
    }
    out
}

/// 저장소의 라우터·유동 IP가 쓰는 주소
pub(crate) fn load_held(cluster: &Cluster) -> Result<HeldIps, ApiError> {
    let (_, routers) = cluster.list::<RouterSpec>()?;
    let (_, fips) = cluster.list::<FloatingIpSpec>()?;
    Ok(held_ips(&routers, &fips, None))
}

/// NIC(MAC `mac`)의 주소를 서브넷에서 고른다: SLAAC 계열 IPv6 서브넷은 EUI-64(MAC이 아직 없으면 — 복제 미리 보기 —
/// 원하는 주소는 400, 아니면 고르지 않는다), 아니면 유동 IP 풀을 건너뛰는 [`ipam::allocate`]
fn allocate_nic(
    subnet: &Subnet,
    mac: Option<MacAddr>,
    used: &BTreeSet<IpAddr>,
    want: Option<IpAddr>,
) -> Result<Option<IpAddr>, IpamError> {
    match (ipam::slaac_prefix(subnet), mac) {
        (Some(_), Some(mac)) => ipam::allocate_v6_mode(subnet, mac, used, want).map(Some),
        (Some(_), None) => match want {
            Some(w) => Err(IpamError::NotEui64(
                w,
                IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
            )),
            None => Ok(None),
        },
        (None, _) => ipam::allocate(subnet, used, want).map(Some),
    }
}

/// 이름이 `name`인 SDN 네트워크
fn sdn_network<'a>(networks: &'a [Network], name: &str) -> Option<&'a Network> {
    networks.iter().find(|n| n.name == name && n.spec.is_sdn())
}

/// 네트워크 `network`(이름)에서 VM들 `vms`(템플릿은 주소를 갖지 않는다)와 라우터·유동 IP(`held`)가 쓰는 주소
fn used_ips<'a>(
    vms: impl IntoIterator<Item = &'a Vm>,
    held: &HeldIps,
    network: &str,
) -> BTreeSet<IpAddr> {
    let mut used: BTreeSet<IpAddr> = vms
        .into_iter()
        .filter(|v| !v.spec.template)
        .flat_map(|v| v.spec.nics.iter())
        .filter(|n| n.network == network)
        .flat_map(|n| n.ips.iter().map(|ip| ip.to_canonical()))
        .collect();
    used.extend(held.get(network).into_iter().flatten().copied());
    used
}

/// 주소 `ip`를 가진 VM(오류 문구용)
fn holder<'a>(vms: &[&'a Vm], network: &str, ip: IpAddr) -> Option<&'a Vm> {
    vms.iter().copied().find(|v| {
        !v.spec.template
            && v.spec
                .nics
                .iter()
                .any(|n| n.network == network && n.ips.iter().any(|a| a.to_canonical() == ip))
    })
}

/// 주소가 다 쓰인 서브넷: 409 `ip_exhausted`
fn exhausted(network: &str, e: IpamError) -> ApiError {
    ApiError::conflict("ip_exhausted", format!("network {network:?}: {e}"))
}

/// API의 VM 스펙 `spec`(지금 저장된 스펙 `current`, 다른 VM들 `others`)의 SDN NIC 주소를 검사하고 채운다.
///
/// - 템플릿은 주소를 갖지 않는다(지운다).
/// - SDN이 아닌 네트워크의 NIC에 주소가 있으면 400 `ips_without_sdn`. SDN NIC에는 cloud-init 주소 항목(400
///   `cloud_init_sdn_nic` — 빈 `{}`만 받는다)을 받지 않는다. SDN NIC의 보안 그룹은 계획 66부터 OVN ACL이다(기능 검사는
///   [`crate::vms::check_security_groups`]).
/// - 이미 있던 NIC(같은 MAC·같은 네트워크, 템플릿이 아니던 스펙)가 주소 없이 오면 지금 주소를 그대로 둔다(주소를 모르는
///   클라이언트의 PUT이 주소를 지우지 않게). 그 주소는 다시 보지 않는다(풀을 줄여도 남는다).
/// - 새로 준 주소는 그 네트워크의 서브넷 안(400)이고 예약이 아니며(400) 다른 NIC가 쓰지 않아야 한다(409 `ip_in_use`).
/// - **새 NIC**(새 VM, 새 MAC, 바꾼 네트워크, 템플릿에서 되돌림)만 비어 있는 주소 체계마다 서브넷에서 가장 낮은 빈 주소를
///   받는다(다 쓰였으면 409 `ip_exhausted`, 유동 IP 풀은 건너뜀). SLAAC 계열 IPv6 서브넷(계획 66 §10)은 MAC의 EUI-64이고
///   다른 주소를 주면 400이다. 라우터 외부 주소·유동 IP(`held`)도 쓴 주소다. 이미 있던 NIC는 여기서 채우지 않는다(설계 §9 — 주소가 하나도 없는 NIC는 서브넷을 더하는 `PATCH`가 채운다, 리뷰 2 N-4).
///
/// `sees`는 부른 사용자가 읽을 수 있는 VM인지다: 409 `ip_in_use`는 그런 VM만 이름으로 알린다(리뷰 1 I-2 — 다른
/// 폴더의 VM 이름을 드러내지 않게).
pub(crate) fn assign(
    spec: &mut VmSpec,
    current: Option<&VmSpec>,
    others: &[&Vm],
    networks: &[Network],
    held: &HeldIps,
    sees: &dyn Fn(&Vm) -> bool,
) -> Result<(), ApiError> {
    for (i, nic) in spec.nics.iter().enumerate() {
        let Some(net) = sdn_network(networks, &nic.network) else {
            if !nic.ips.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "NIC {i}: addresses (ips) can be given only on SDN networks; network {:?} is not one",
                    nic.network
                ))
                .with_code("ips_without_sdn"));
            }
            continue;
        };
        if spec
            .cloud_init
            .as_ref()
            .and_then(|ci| ci.networks.get(i))
            .is_some_and(|n| *n != CloudInitNet::default())
        {
            return Err(ApiError::bad_request(format!(
                "cloud_init.networks[{i}]: NIC {i} is on SDN network {:?}, whose addresses come from IPAM; leave this entry empty ({{}})",
                net.name
            ))
            .with_code("cloud_init_sdn_nic"));
        }
    }
    if spec.template {
        spec.nics.iter_mut().for_each(|n| n.ips.clear());
        return Ok(());
    }
    let current = current.filter(|c| !c.template);
    for i in 0..spec.nics.len() {
        let nic = &spec.nics[i];
        let Some(net) = sdn_network(networks, &nic.network) else {
            continue;
        };
        let kept: Option<Vec<IpAddr>> = current.and_then(|c| {
            c.nics
                .iter()
                .find(|n| n.mac.is_some() && n.mac == nic.mac && n.network == nic.network)
                .map(|n| n.ips.clone())
        });
        // 이 스펙의 앞 NIC가 같은 네트워크에서 쓰는 주소도 쓴 것이다.
        let mut used = used_ips(others.iter().copied(), held, &net.name);
        used.extend(
            spec.nics[..i]
                .iter()
                .filter(|n| n.network == net.name)
                .flat_map(|n| n.ips.iter().copied()),
        );
        let mac = nic.mac;
        let mut ips: Vec<IpAddr> = if nic.ips.is_empty() {
            kept.clone().unwrap_or_default()
        } else {
            nic.ips.iter().map(|ip| ip.to_canonical()).collect()
        };
        for ip in &ips {
            if kept.as_ref().is_some_and(|k| k.contains(ip)) {
                continue;
            }
            let Some(subnet) = ipam::nic_subnet(&net.spec, *ip) else {
                return Err(ApiError::bad_request(format!(
                    "NIC {i}: address {ip} is not in a subnet of network {:?}",
                    net.name
                )));
            };
            match allocate_nic(subnet, mac, &used, Some(*ip)) {
                Ok(_) => {}
                Err(IpamError::InUse(ip)) => {
                    let by = match holder(others, &net.name, ip) {
                        Some(v) if sees(v) => format!(" by VM {:?}", v.name),
                        None if held.get(&net.name).is_some_and(|h| h.contains(&ip)) => {
                            " by a router or floating IP".to_string()
                        }
                        _ => String::new(),
                    };
                    return Err(ApiError::conflict(
                        "ip_in_use",
                        format!(
                            "NIC {i}: address {ip} on network {:?} is already in use{by}",
                            net.name
                        ),
                    ));
                }
                Err(e) => return Err(ApiError::bad_request(format!("NIC {i}: {e}"))),
            }
            used.insert(*ip);
        }
        if kept.is_none() {
            for subnet in &net.spec.subnets {
                if ips.iter().any(|ip| ip.is_ipv4() == subnet.is_ipv4()) {
                    continue;
                }
                let Some(ip) =
                    allocate_nic(subnet, mac, &used, None).map_err(|e| exhausted(&net.name, e))?
                else {
                    continue;
                };
                used.insert(ip);
                ips.push(ip);
            }
        }
        ips.sort_by_key(|ip| !ip.is_ipv4());
        spec.nics[i].ips = ips;
    }
    Ok(())
}

/// SDN NIC에 보안 그룹을 걸 수 있는지(모든 노드가 `sdn-routing`을 앎 — 계획 66 §11.4)
pub(crate) fn sdn_groups_allowed(cluster: &Cluster) -> Result<bool, ApiError> {
    Ok(crate::cluster::missing_feature(cluster, opl_types::node::SDN_ROUTING)?.is_empty())
}

/// 복제·배포·가져오기 요청이 NIC 순번별로 준 주소 `requested`(새 VM의 스펙 `spec`)를 미리 본다(설계 §12). 옛 노드가
/// 있으면 409 `cluster_upgrading`, NIC보다 많으면·SDN이 아닌 네트워크·서브넷 밖·예약 주소는 400, 다른 NIC가 쓰면 409
/// `ip_in_use`, 서브넷이 다 쓰였으면 409 `ip_exhausted`. 새 VM의 다른 NIC는 Task가 새로 받으므로 주소 없이 본다.
/// Task가 VM을 만들 때 다시 보고, 그새 쓰였으면 실패한다.
/// 권한 검사 뒤에 부른다(리뷰 1 I-2 — 권한 없는 요청이 주소의 쓰임을 캐 보지 못하게). `sees`는 [`assign`]과 같다.
pub(crate) fn check_requested(
    cluster: &Cluster,
    spec: &VmSpec,
    requested: &[Vec<IpAddr>],
    sees: &dyn Fn(&Vm) -> bool,
) -> Result<(), ApiError> {
    if requested.iter().all(Vec::is_empty) {
        return Ok(());
    }
    require_feature(cluster, opl_types::node::SDN)?;
    if requested.len() > spec.nics.len() {
        return Err(ApiError::bad_request(format!(
            "ips: {} given but the VM has {} NIC(s)",
            requested.len(),
            spec.nics.len()
        )));
    }
    let mut new = spec.clone();
    new.template = false;
    for (i, nic) in new.nics.iter_mut().enumerate() {
        nic.mac = None;
        nic.ips = requested
            .get(i)
            .map(|ips| ips.iter().map(|ip| ip.to_canonical()).collect())
            .unwrap_or_default();
    }
    // 주소 체계마다 하나(대응 IPv6를 IPv4로 바꾼 뒤 — 리뷰 1 I-3)
    for (i, nic) in new.nics.iter().enumerate() {
        if nic.ips.len() > 2 || (nic.ips.len() == 2 && nic.ips[0].is_ipv4() == nic.ips[1].is_ipv4())
        {
            return Err(ApiError::bad_request(format!(
                "ips: {}",
                opl_types::vm::ValidationError::NicIps(i)
            )));
        }
    }
    let (_, vms) = cluster.list::<VmSpec>()?;
    let (_, networks) = cluster.list::<NetworkSpec>()?;
    // Task가 SDN NIC의 cloud-init 주소 항목(섞인 클러스터면 보안 그룹도)을 뺀다(아래 [`strip_sdn_nic_config`]) — 미리
    // 보기도 같게.
    let groups = sdn_groups_allowed(cluster)?;
    strip_sdn_nic_config(&mut new, &networks, groups);
    let others: Vec<&Vm> = vms.iter().collect();
    assign(
        &mut new,
        None,
        &others,
        &networks,
        &load_held(cluster)?,
        sees,
    )
}

/// 복제·배포·가져오기·복원이 만드는 새 VM `spec`에서 SDN 네트워크 NIC의 cloud-init 주소 항목을 뺀다(리뷰 1 웹 #2 —
/// 배포의 네트워크 바꾸기가 고정 주소를 가진 템플릿 NIC를 SDN으로 옮기면 그 VM은 저장할 수 없는 스펙이 된다). 보안 그룹은
/// `keep_groups`(모든 노드가 `sdn-routing`을 앎 — 계획 66 §11.4, OVN ACL로 그대로 집행)가 거짓일 때만 뺀다(섞인
/// 클러스터는 계획 65 동작). 뺀 것마다 경고 한 줄을 돌려준다.
pub(crate) fn strip_sdn_nic_config(
    spec: &mut VmSpec,
    networks: &[Network],
    keep_groups: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for i in 0..spec.nics.len() {
        let Some(net) = sdn_network(networks, &spec.nics[i].network) else {
            continue;
        };
        let nic = &mut spec.nics[i];
        if !keep_groups && !nic.security_groups.is_empty() {
            warnings.push(format!(
                "NIC {i}: dropped security groups {} — SDN network {:?} protects NICs with port security only",
                nic.security_groups.join(", "),
                net.name
            ));
            nic.security_groups.clear();
        }
        if let Some(entry) = spec
            .cloud_init
            .as_mut()
            .and_then(|ci| ci.networks.get_mut(i))
            .filter(|n| **n != CloudInitNet::default())
        {
            *entry = CloudInitNet::default();
            warnings.push(format!(
                "NIC {i}: dropped its cloud-init address settings — SDN network {:?} gives addresses from IPAM",
                net.name
            ));
        }
    }
    warnings
}

/// Task가 만드는 새 VM(복제·템플릿 배포·가져오기·복원·템플릿 되돌리기)의 스펙 `spec`에 주소를 채운다. `vms`는 이미 있는
/// VM들(이 VM은 빼고)이다. 템플릿이면 주소를 지운다. 스펙에 있던 주소(복원)는 SDN 네트워크의 서브넷 안이고 비어 있으면
/// 그대로, 아니면 버리고 새로 받는다 — 바뀐 것마다 경고 한 줄을 돌려준다. 그 뒤 SDN NIC마다 빈 주소 체계를 채운다.
/// 서브넷에 빈 주소가 없으면 오류다.
pub(crate) fn assign_new(
    spec: &mut VmSpec,
    vms: &[&Vm],
    networks: &[Network],
    held: &HeldIps,
) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    if spec.template {
        spec.nics.iter_mut().for_each(|n| n.ips.clear());
        return Ok(warnings);
    }
    for i in 0..spec.nics.len() {
        let nic = &spec.nics[i];
        let Some(net) = sdn_network(networks, &nic.network) else {
            if !nic.ips.is_empty() {
                warnings.push(format!(
                    "NIC {i}: network {:?} is not an SDN network; dropped its addresses",
                    nic.network
                ));
                spec.nics[i].ips.clear();
            }
            continue;
        };
        let mut used = used_ips(vms.iter().copied(), held, &net.name);
        used.extend(
            spec.nics[..i]
                .iter()
                .filter(|n| n.network == net.name)
                .flat_map(|n| n.ips.iter().copied()),
        );
        let mac = nic.mac;
        let mut ips = Vec::new();
        let mut dropped = Vec::new();
        for ip in nic.ips.iter().map(|ip| ip.to_canonical()) {
            let fits = ipam::nic_subnet(&net.spec, ip)
                .is_some_and(|s| allocate_nic(s, mac, &used, Some(ip)).is_ok())
                && !ips.iter().any(|a: &IpAddr| a.is_ipv4() == ip.is_ipv4());
            if fits {
                used.insert(ip);
                ips.push(ip);
            } else {
                dropped.push(ip);
            }
        }
        for subnet in &net.spec.subnets {
            if ips.iter().any(|ip| ip.is_ipv4() == subnet.is_ipv4()) {
                continue;
            }
            let Some(ip) = allocate_nic(subnet, mac, &used, None)
                .map_err(|e| format!("network {:?}: {e}", net.name))?
            else {
                continue;
            };
            used.insert(ip);
            ips.push(ip);
        }
        ips.sort_by_key(|ip| !ip.is_ipv4());
        if !dropped.is_empty() {
            let list = |v: &[IpAddr]| {
                v.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let gave = if ips.is_empty() {
                "no address".to_string()
            } else {
                list(&ips)
            };
            warnings.push(format!(
                "NIC {i}: address {} on network {:?} is taken or no longer in a subnet; the VM got {gave}",
                list(&dropped),
                net.name
            ));
        }
        spec.nics[i].ips = ips;
    }
    Ok(warnings)
}

/// 네트워크 `network`의 NIC 가운데 주소가 하나도 없는 것에 새로 더한 서브넷 `added`(체계마다 하나)의 가장 낮은 빈 주소를
/// 넣는다(리뷰 2 N-4 — 서브넷을 더하는 `PATCH`가 채운다). 고른 주소는 `used`에 넣고, 채운 NIC 수를 돌려준다. 이미 주소가
/// 있는 NIC는 건드리지 않는다(켜진 VM에서 바꾸면 게스트가 쓰는 주소가 막힌다). 빈 주소가 없으면 오류다.
pub(crate) fn fill_empty(
    spec: &mut VmSpec,
    network: &str,
    added: &[Subnet],
    used: &mut BTreeSet<IpAddr>,
) -> Result<usize, IpamError> {
    if spec.template {
        return Ok(0);
    }
    let mut filled = 0;
    for nic in spec
        .nics
        .iter_mut()
        .filter(|n| n.network == network && n.ips.is_empty())
    {
        let mut ips = Vec::new();
        for subnet in added {
            let Some(ip) = allocate_nic(subnet, nic.mac, used, None)? else {
                continue;
            };
            used.insert(ip);
            ips.push(ip);
        }
        if !ips.is_empty() {
            ips.sort_by_key(|ip| !ip.is_ipv4());
            nic.ips = ips;
            filled += 1;
        }
    }
    Ok(filled)
}

/// 네트워크 `network`에서 VM들 `vms`와 라우터·유동 IP(`held`)가 쓰는 주소([`fill_empty`]의 `used`)
pub(crate) fn network_used(vms: &[Vm], held: &HeldIps, network: &str) -> BTreeSet<IpAddr> {
    used_ips(vms, held, network)
}

/// 주소가 하나도 없는 NIC(템플릿 아닌 VM) 수와, 더한 서브넷 `added` 가운데 그만큼 줄 수 없는 것마다 "CIDR (n free)".
/// 비어 있으면 모두 채울 수 있다.
pub(crate) fn fill_shortages(
    vms: &[Vm],
    held: &HeldIps,
    network: &str,
    added: &[Subnet],
) -> (usize, Vec<String>) {
    let needed = vms
        .iter()
        .filter(|v| !v.spec.template)
        .flat_map(|v| v.spec.nics.iter())
        .filter(|n| n.network == network && n.ips.is_empty())
        .count();
    let mut short = Vec::new();
    if needed == 0 {
        return (0, short);
    }
    // EUI-64 서브넷(MAC에서 정함)은 모자라지 않는다.
    for subnet in added.iter().filter(|s| ipam::slaac_prefix(s).is_none()) {
        let mut used = used_ips(vms, held, network);
        let mut free = 0;
        while free < needed {
            match ipam::allocate(subnet, &used, None) {
                Ok(ip) => {
                    used.insert(ip);
                    free += 1;
                }
                Err(_) => break,
            }
        }
        if free < needed {
            short.push(format!("{} ({free} free)", subnet.cidr));
        }
    }
    (needed, short)
}

#[cfg(test)]
mod tests;
