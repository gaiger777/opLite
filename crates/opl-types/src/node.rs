use serde::{Deserialize, Serialize};

use crate::resource::{Kind, Resource, ResourceSpec};

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
    /// 예: `qemu-hvf`, `qemu-kvm`
    pub hypervisor: String,
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
}
