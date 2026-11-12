//! 내장 VIP(설계 §4). 리더가 VIP마다 그것을 가질 노드를 정해 Raft에 적는다. 리스 만료는 리더의 메모리
//! (`RenewVip` 갱신을 받은 시각)로 판단하므로 여기에는 누가 가졌는지만 있다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

/// VIP를 가진 노드. 리소스 이름은 VIP의 IP 주소(`192.168.0.200`)다. 리더만 쓴다.
pub type VipHolder = Resource<VipHolderSpec, VipHolderStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VipHolderSpec {
    /// VIP를 가질 노드(Node id)
    pub node: Uuid,
}

impl ResourceSpec for VipHolderSpec {
    const KIND: Kind = Kind::VipHolder;
    type Status = VipHolderStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct VipHolderStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vip_holder_storage_format_is_stable() {
        let node = Uuid::from_u128(1);
        let spec = VipHolderSpec { node };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json, serde_json::json!({ "node": node.to_string() }));
        assert_eq!(serde_json::from_value::<VipHolderSpec>(json).unwrap(), spec);
        assert_eq!(serde_json::to_value(Kind::VipHolder).unwrap(), "vip-holder");
    }
}
