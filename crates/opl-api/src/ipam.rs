//! SDN NIC 주소 할당(계획 65 §9). 주소의 출처는 Raft의 VM 스펙(`NicSpec.ips`)이고, API가 VM을 만들거나 바꿀 때
//! ([`assign`])와 Task가 새 VM을 만들 때(복제·배포·가져오기·복원, [`assign_new`]) 채운다. 쓰고 있는 주소는 그
//! 네트워크를 쓰는 템플릿 아닌 VM 스펙의 `ips`와 예약이다. 두 API 노드가 같은 주소를 동시에 고르면 Raft 상태 머신이
//! 하나를 `InUse { field: "ip" }`로 거절하고([`is_ip_taken`]) 부른 쪽이 다시 고른다([`RETRIES`]번까지).

use std::collections::BTreeSet;
use std::net::IpAddr;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::cloud_init::CloudInitNet;
use opl_types::ipam::{self, IpamError};
use opl_types::network::{Network, NetworkSpec};
use opl_types::vm::{Vm, VmSpec};

use crate::cluster::require_feature;
use crate::error::ApiError;

/// Raft가 주소 겹침으로 거절했을 때 다시 고르는 횟수
pub(crate) const RETRIES: usize = 5;

/// 쓰기가 다른 VM이 그새 잡은 NIC 주소로 거절되었는지(다시 골라 볼 일)
pub(crate) fn is_ip_taken(e: &ClusterError) -> bool {
    matches!(e, ClusterError::Rejected(Rejection::InUse { field, .. }) if field == "ip")
}

/// 이름이 `name`인 SDN 네트워크
fn sdn_network<'a>(networks: &'a [Network], name: &str) -> Option<&'a Network> {
    networks.iter().find(|n| n.name == name && n.spec.is_sdn())
}

/// 네트워크 `network`(이름)에서 VM들 `vms`가 쓰는 주소(템플릿은 주소를 갖지 않는다)
fn used_ips<'a>(vms: impl IntoIterator<Item = &'a Vm>, network: &str) -> BTreeSet<IpAddr> {
    vms.into_iter()
        .filter(|v| !v.spec.template)
        .flat_map(|v| v.spec.nics.iter())
        .filter(|n| n.network == network)
        .flat_map(|n| n.ips.iter().map(|ip| ip.to_canonical()))
        .collect()
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
/// - SDN이 아닌 네트워크의 NIC에 주소가 있으면 400 `ips_without_sdn`. SDN NIC에는 보안 그룹(400
///   `sdn_security_groups` — 계획 66)과 cloud-init 주소 항목(400 `cloud_init_sdn_nic` — 빈 `{}`만 받는다)을 받지 않는다.
/// - 이미 있던 NIC(같은 MAC·같은 네트워크, 템플릿이 아니던 스펙)가 주소 없이 오면 지금 주소를 그대로 둔다(주소를 모르는
///   클라이언트의 PUT이 주소를 지우지 않게). 그 주소는 다시 보지 않는다(풀을 줄여도 남는다).
/// - 새로 준 주소는 그 네트워크의 서브넷 안(400)이고 예약이 아니며(400) 다른 NIC가 쓰지 않아야 한다(409 `ip_in_use`).
/// - **새 NIC**(새 VM, 새 MAC, 바꾼 네트워크, 템플릿에서 되돌림)만 비어 있는 주소 체계마다 서브넷에서 가장 낮은 빈 주소를
///   받는다(다 쓰였으면 409 `ip_exhausted`). 이미 있던 NIC에는 서브넷을 더해도 저절로 주지 않는다(설계 §9).
pub(crate) fn assign(
    spec: &mut VmSpec,
    current: Option<&VmSpec>,
    others: &[&Vm],
    networks: &[Network],
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
        if !nic.security_groups.is_empty() {
            return Err(ApiError::bad_request(format!(
                "NIC {i}: security groups on SDN networks come with plan 66; this NIC is protected by port security only"
            ))
            .with_code("sdn_security_groups"));
        }
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
        let mut used = used_ips(others.iter().copied(), &net.name);
        used.extend(
            spec.nics[..i]
                .iter()
                .filter(|n| n.network == net.name)
                .flat_map(|n| n.ips.iter().copied()),
        );
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
            match ipam::allocate(subnet, &used, Some(*ip)) {
                Ok(_) => {}
                Err(IpamError::InUse(ip)) => {
                    let by = holder(others, &net.name, ip)
                        .map(|v| format!(" by VM {:?}", v.name))
                        .unwrap_or_default();
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
                let ip =
                    ipam::allocate(subnet, &used, None).map_err(|e| exhausted(&net.name, e))?;
                used.insert(ip);
                ips.push(ip);
            }
        }
        ips.sort_by_key(|ip| !ip.is_ipv4());
        spec.nics[i].ips = ips;
    }
    Ok(())
}

/// 복제·배포·가져오기 요청이 NIC 순번별로 준 주소 `requested`(새 VM의 스펙 `spec`)를 미리 본다(설계 §12). 옛 노드가
/// 있으면 409 `cluster_upgrading`, NIC보다 많으면·SDN이 아닌 네트워크·서브넷 밖·예약 주소는 400, 다른 NIC가 쓰면 409
/// `ip_in_use`, 서브넷이 다 쓰였으면 409 `ip_exhausted`. 새 VM의 다른 NIC는 Task가 새로 받으므로 주소 없이 본다.
/// Task가 VM을 만들 때 다시 보고, 그새 쓰였으면 실패한다.
pub(crate) fn check_requested(
    cluster: &Cluster,
    spec: &VmSpec,
    requested: &[Vec<IpAddr>],
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
        nic.ips = requested.get(i).cloned().unwrap_or_default();
    }
    let (_, vms) = cluster.list::<VmSpec>()?;
    let (_, networks) = cluster.list::<NetworkSpec>()?;
    let others: Vec<&Vm> = vms.iter().collect();
    assign(&mut new, None, &others, &networks)
}

/// Task가 만드는 새 VM(복제·템플릿 배포·가져오기·복원·템플릿 되돌리기)의 스펙 `spec`에 주소를 채운다. `vms`는 이미 있는
/// VM들(이 VM은 빼고)이다. 템플릿이면 주소를 지운다. 스펙에 있던 주소(복원)는 SDN 네트워크의 서브넷 안이고 비어 있으면
/// 그대로, 아니면 버리고 새로 받는다 — 바뀐 것마다 경고 한 줄을 돌려준다. 그 뒤 SDN NIC마다 빈 주소 체계를 채운다.
/// 서브넷에 빈 주소가 없으면 오류다.
pub(crate) fn assign_new(
    spec: &mut VmSpec,
    vms: &[&Vm],
    networks: &[Network],
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
        let mut used = used_ips(vms.iter().copied(), &net.name);
        used.extend(
            spec.nics[..i]
                .iter()
                .filter(|n| n.network == net.name)
                .flat_map(|n| n.ips.iter().copied()),
        );
        let mut ips = Vec::new();
        let mut dropped = Vec::new();
        for ip in nic.ips.iter().map(|ip| ip.to_canonical()) {
            let fits = ipam::nic_subnet(&net.spec, ip)
                .is_some_and(|s| ipam::allocate(s, &used, Some(ip)).is_ok())
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
            let ip = ipam::allocate(subnet, &used, None)
                .map_err(|e| format!("network {:?}: {e}", net.name))?;
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

#[cfg(test)]
mod tests;
