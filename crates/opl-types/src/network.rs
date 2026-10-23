//! VM NIC가 연결하는 네트워크. 지금은 관리자가 미리 만든 Linux 브리지만 있다.

use serde::{Deserialize, Serialize};

use crate::resource::{Kind, Resource, ResourceSpec};

/// 예약된 네트워크 이름. 리소스가 없고 QEMU user-mode NAT로 연결한다(macOS 개발·데모용).
pub const USER_NETWORK: &str = "user";

pub type Network = Resource<NetworkSpec, NetworkStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`)
    pub bridge: String,
    /// VLAN 태그(1~4094). 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
    #[serde(default)]
    pub vlan: Option<u16>,
}

impl ResourceSpec for NetworkSpec {
    const KIND: Kind = Kind::Network;
    type Status = NetworkStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NetworkStatus {}

impl NetworkSpec {
    pub fn validate(&self) -> Result<(), NetworkError> {
        if !valid_ifname(&self.bridge) {
            return Err(NetworkError::BadBridge(self.bridge.clone()));
        }
        if let Some(vlan) = self.vlan
            && !(1..=4094).contains(&vlan)
        {
            return Err(NetworkError::BadVlan(vlan));
        }
        Ok(())
    }
}

/// Linux 인터페이스 이름 규칙: 1~15바이트, `/`·공백·`:` 없음, `.`·`..`가 아님.
pub fn valid_ifname(name: &str) -> bool {
    (1..=15).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|c| c.is_ascii_graphic() && c != b'/' && c != b':')
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NetworkError {
    #[error("invalid bridge name {0:?}: use 1-15 characters without '/', ':' or spaces")]
    BadBridge(String),
    #[error("invalid VLAN tag {0}: use 1-4094")]
    BadVlan(u16),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(bridge: &str, vlan: Option<u16>) -> NetworkSpec {
        NetworkSpec {
            bridge: bridge.into(),
            vlan,
        }
    }

    #[test]
    fn validates_bridge_and_vlan() {
        assert!(spec("vmbr0", None).validate().is_ok());
        assert!(spec("br-lan.10", Some(4094)).validate().is_ok());
        for bad in ["", "a/b", "a b", "a:b", ".", "..", "abcdefghijklmnop"] {
            assert_eq!(
                spec(bad, None).validate(),
                Err(NetworkError::BadBridge(bad.into()))
            );
        }
        assert_eq!(
            spec("vmbr0", Some(0)).validate(),
            Err(NetworkError::BadVlan(0))
        );
        assert_eq!(
            spec("vmbr0", Some(4095)).validate(),
            Err(NetworkError::BadVlan(4095))
        );
    }

    #[test]
    fn vlan_is_optional() {
        let s: NetworkSpec =
            serde_json::from_value(serde_json::json!({ "bridge": "vmbr0" })).unwrap();
        assert_eq!(s, spec("vmbr0", None));
    }
}
