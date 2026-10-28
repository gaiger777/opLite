use std::collections::BTreeMap;

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
        let status: NodeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(status, NodeStatus::default());
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
