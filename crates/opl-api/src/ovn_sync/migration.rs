//! 옮기기 다리(계획 67 §9.5): 다리가 놓인 옮기기마다 대상(오버레이) 스위치에 `l2gateway` 포트 하나 — 다리 노드 chassis에 묶여
//! 그 노드의 임시 physnet `mig<h8>`(원본 Linux 브리지로 가는 veth)로 원본 L2를 잇는다 — 와, 다리가 있는 동안 그 스위치의
//! IPv4 DHCP에 `router`(서브넷 게이트웨이 = 원본 망의 물리 라우터)를 더한다. 다리가 없으면 행이 계획 66·67과 같다.

use opl_types::network::Network;
use opl_types::network_migration::NetworkMigration;
use uuid::Uuid;

use super::nb::{CLUSTER_KEY, ID_KEY, NbState, PortRow};

/// 옮기기 `migration`의 `l2gateway` 포트 이름
pub fn l2gateway_port_name(migration: Uuid) -> String {
    format!("opl-l2gw-{migration}")
}

/// 바라는 NB 상태 `state`에 끝나지 않은 옮기기들 `migrations`의 다리를 더한다. 대상이 오버레이 SDN 스위치로 있고 원본
/// 네트워크가 있는 것만(원본이 VLAN이면 그 VLAN을 `tag`로).
pub fn add_bridges(
    state: &mut NbState,
    migrations: &[NetworkMigration],
    networks: &[Network],
    tag: &str,
) {
    let mut migrations: Vec<&NetworkMigration> = migrations
        .iter()
        .filter(|m| !m.status.is_done() && m.status.bridge.is_some())
        .collect();
    migrations.sort_by_key(|m| m.id);
    for m in migrations {
        let Some(bridge) = &m.status.bridge else {
            continue;
        };
        let network = |name: &str| networks.iter().find(|n| n.name == name);
        let (Some(source), Some(target)) = (network(&m.spec.source), network(&m.spec.target))
        else {
            continue;
        };
        if source.spec.is_sdn() || !target.spec.is_sdn() || target.spec.is_provider() {
            continue;
        }
        let Some(switch) = state.switches.iter_mut().find(|s| s.network == target.id) else {
            continue;
        };
        let name = l2gateway_port_name(m.id);
        if switch.ports.iter().any(|p| p.name == name) {
            continue;
        }
        // 한 스위치에 다리 하나(리뷰 1 I1 — Raft `migration-target`이 막지만, 막기 전에 만든 옮기기가 남았을 때도 고리를
        // 만들지 않게): id가 가장 낮은 옮기기만 놓는다.
        if let Some(other) = switch.ports.iter().find(|p| p.kind == "l2gateway") {
            tracing::warn!(
                migration = %m.id,
                network = %target.name,
                laid = %other.name,
                "another network migration already bridges this network; not laying a second bridge"
            );
            continue;
        }
        switch.ports.push(PortRow {
            name,
            kind: "l2gateway".into(),
            addresses: vec!["unknown".into()],
            port_security: Vec::new(),
            dhcpv4: None,
            dhcpv6: None,
            enabled: None,
            tag: source.spec.vlan,
            options: [
                ("network_name".to_string(), bridge.physnet.clone()),
                ("l2gateway-chassis".to_string(), bridge.node.to_string()),
            ]
            .into(),
            external_ids: [
                (CLUSTER_KEY.to_string(), tag.to_string()),
                (ID_KEY.to_string(), m.id.to_string()),
            ]
            .into(),
            port_group: None,
        });
        switch.ports.sort_by(|a, b| a.name.cmp(&b.name));
        // 옮긴 DHCP 게스트가 임대를 갱신해도 기본 경로(원본 망의 물리 라우터)를 잃지 않게
        for row in &mut switch.dhcp {
            let Some(subnet) = target
                .spec
                .subnets
                .iter()
                .find(|s| s.is_ipv4() && s.cidr.to_string() == row.cidr)
            else {
                continue;
            };
            row.options
                .entry("router".to_string())
                .or_insert_with(|| subnet.gateway().to_string());
        }
    }
}
