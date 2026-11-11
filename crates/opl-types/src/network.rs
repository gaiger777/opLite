//! VM NIC가 연결하는 네트워크: 관리자가 미리 만든 Linux 브리지나 opld가 만드는 VXLAN 네트워크.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

/// 예약된 네트워크 이름. 리소스가 없고 QEMU user-mode NAT로 연결한다(macOS 개발·데모용).
pub const USER_NETWORK: &str = "user";

/// VXLAN 네트워크의 기본 MTU: underlay 1500에서 VXLAN 머리 50바이트를 뺀 값
pub const DEFAULT_VXLAN_MTU: u16 = 1450;

/// VXLAN UDP 포트(IANA)
pub const VXLAN_PORT: u16 = 4789;

pub type Network = Resource<NetworkSpec, NetworkStatus>;

/// `bridge`와 `vxlan` 중 정확히 하나를 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bridge: Option<String>,
    /// VLAN 태그(1~4094). `bridge`와만 쓰고, 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan: Option<u16>,
    /// orbivisord가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만드는 네트워크
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vxlan: Option<Vxlan>,
    /// VXLAN 네트워크의 MTU(1280~9000, 기본 1450). `vxlan`과만 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub mtu: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Vxlan {
    /// VXLAN 네트워크 식별자(1~16777215). 클러스터 안에서 겹치지 않는다.
    pub vni: u32,
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
        match (&self.bridge, &self.vxlan) {
            (Some(bridge), None) => {
                if !valid_ifname(bridge) {
                    return Err(NetworkError::BadBridge(bridge.clone()));
                }
                if is_managed_ifname(bridge) {
                    return Err(NetworkError::ReservedBridge(bridge.clone()));
                }
                if self.mtu.is_some() {
                    return Err(NetworkError::MtuWithBridge);
                }
                if let Some(vlan) = self.vlan
                    && !(1..=4094).contains(&vlan)
                {
                    return Err(NetworkError::BadVlan(vlan));
                }
            }
            (None, Some(vxlan)) => {
                if self.vlan.is_some() {
                    return Err(NetworkError::VlanWithVxlan);
                }
                if !(1..=16_777_215).contains(&vxlan.vni) {
                    return Err(NetworkError::BadVni(vxlan.vni));
                }
                if let Some(mtu) = self.mtu
                    && !(1280..=9000).contains(&mtu)
                {
                    return Err(NetworkError::BadMtu(mtu));
                }
            }
            _ => return Err(NetworkError::KindRequired),
        }
        Ok(())
    }

    /// VM NIC가 붙을 브리지 이름: `bridge` 또는 `oplv<vni>`. 검증을 통과한 스펙에서만 뜻이 있다.
    pub fn bridge_name(&self) -> String {
        match (&self.bridge, self.vni()) {
            (Some(bridge), _) => bridge.clone(),
            (None, Some(vni)) => vxlan_bridge(vni),
            (None, None) => String::new(),
        }
    }

    /// VXLAN 네트워크의 MTU(없으면 기본값). 브리지 네트워크는 None.
    pub fn mtu(&self) -> Option<u16> {
        self.vxlan.map(|_| self.mtu.unwrap_or(DEFAULT_VXLAN_MTU))
    }

    pub fn vni(&self) -> Option<u32> {
        self.vxlan.map(|v| v.vni)
    }
}

/// VXLAN 네트워크의 브리지 이름(`oplv<vni>`, 최대 12자)
pub fn vxlan_bridge(vni: u32) -> String {
    format!("oplv{vni}")
}

/// VXLAN 네트워크의 VXLAN 장치 이름(`oplx<vni>`, 최대 12자)
pub fn vxlan_device(vni: u32) -> String {
    format!("oplx{vni}")
}

/// opld가 관리하는 장치 이름 모양(`^opl[vx][0-9]+$`)인지
pub fn is_managed_ifname(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("oplv")
        .or_else(|| name.strip_prefix("oplx"))
    else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_digit())
}

/// VM의 `index`번째 NIC가 쓰는 tap 장치 이름. 노드 안에서 VM마다 겹치지 않고 15자를 넘지 않는다.
/// UUIDv7의 앞부분은 생성 시각이므로 무작위인 뒷부분 10자리를 쓴다.
pub fn tap_name(vm: Uuid, index: usize) -> String {
    format!("{}{index}", tap_prefix(vm))
}

/// 한 VM의 tap 장치 이름이 모두 이것으로 시작한다(`op<10자리>n`).
pub fn tap_prefix(vm: Uuid) -> String {
    let hex = vm.simple().to_string();
    format!("op{}n", &hex[hex.len() - 10..])
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
    #[error("set exactly one of bridge or vxlan")]
    KindRequired,
    #[error("vlan can only be used with bridge, not with vxlan")]
    VlanWithVxlan,
    #[error("mtu can only be used with vxlan; a bridge network uses the bridge's MTU")]
    MtuWithBridge,
    #[error("invalid VNI {0}: use 1-16777215")]
    BadVni(u32),
    #[error("invalid MTU {0}: use 1280-9000")]
    BadMtu(u16),
    #[error("bridge name {0:?} is reserved for Orbivisor VXLAN networks")]
    ReservedBridge(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(bridge: &str, vlan: Option<u16>) -> NetworkSpec {
        NetworkSpec {
            bridge: Some(bridge.into()),
            vlan,
            vxlan: None,
            mtu: None,
        }
    }

    fn vxlan(vni: u32, mtu: Option<u16>) -> NetworkSpec {
        NetworkSpec {
            bridge: None,
            vlan: None,
            vxlan: Some(Vxlan { vni }),
            mtu,
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
    fn tap_names_fit_and_share_the_vm_prefix() {
        let vm = Uuid::now_v7();
        let last = tap_name(vm, crate::vm::MAX_NICS - 1);
        assert!(valid_ifname(&last), "{last}");
        assert_eq!(last.len(), 15);
        assert!(tap_name(vm, 0).starts_with(&tap_prefix(vm)));
        assert_ne!(tap_prefix(vm), tap_prefix(Uuid::now_v7()));
        // 같은 VM의 1번과 10번 NIC가 접두사로 겹치지 않는다(`...n1`과 `...n10`은 다른 이름).
        assert_ne!(tap_name(vm, 1), tap_name(vm, 10));
    }

    #[test]
    fn vlan_is_optional() {
        let s: NetworkSpec =
            serde_json::from_value(serde_json::json!({ "bridge": "vmbr0" })).unwrap();
        assert_eq!(s, spec("vmbr0", None));
    }

    #[test]
    fn old_bridge_networks_read_and_write_the_same_json() {
        for old in [
            serde_json::json!({ "bridge": "vmbr0" }),
            serde_json::json!({ "bridge": "vmbr0", "vlan": 10 }),
        ] {
            let s: NetworkSpec = serde_json::from_value(old.clone()).unwrap();
            assert_eq!(serde_json::to_value(&s).unwrap(), old);
        }
        // 지금까지 쓰던 `"vlan": null`도 읽힌다.
        let s: NetworkSpec =
            serde_json::from_value(serde_json::json!({ "bridge": "vmbr0", "vlan": null })).unwrap();
        assert_eq!(s, spec("vmbr0", None));
    }

    #[test]
    fn vxlan_networks_round_trip() {
        let json = serde_json::json!({ "vxlan": { "vni": 1000 }, "mtu": 1400 });
        let s: NetworkSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(s, vxlan(1000, Some(1400)));
        assert_eq!(serde_json::to_value(&s).unwrap(), json);
        assert!(
            serde_json::from_value::<NetworkSpec>(
                serde_json::json!({ "vxlan": { "vni": 1, "group": "x" } })
            )
            .is_err()
        );
    }

    #[test]
    fn validates_the_kind_and_its_fields() {
        let both = NetworkSpec {
            bridge: Some("vmbr0".into()),
            ..vxlan(10, None)
        };
        assert_eq!(both.validate(), Err(NetworkError::KindRequired));
        let neither = NetworkSpec {
            vxlan: None,
            ..vxlan(10, None)
        };
        assert_eq!(neither.validate(), Err(NetworkError::KindRequired));
        let tagged = NetworkSpec {
            vlan: Some(10),
            ..vxlan(10, None)
        };
        assert_eq!(tagged.validate(), Err(NetworkError::VlanWithVxlan));
        let sized = NetworkSpec {
            mtu: Some(1450),
            ..spec("vmbr0", None)
        };
        assert_eq!(sized.validate(), Err(NetworkError::MtuWithBridge));

        for ok in [1, 16_777_215] {
            assert_eq!(vxlan(ok, None).validate(), Ok(()));
        }
        for bad in [0, 16_777_216] {
            assert_eq!(vxlan(bad, None).validate(), Err(NetworkError::BadVni(bad)));
        }
        for ok in [1280, 9000] {
            assert_eq!(vxlan(10, Some(ok)).validate(), Ok(()));
        }
        for bad in [1279, 9001] {
            assert_eq!(
                vxlan(10, Some(bad)).validate(),
                Err(NetworkError::BadMtu(bad))
            );
        }
        for reserved in ["oplv10", "oplx7"] {
            assert_eq!(
                spec(reserved, None).validate(),
                Err(NetworkError::ReservedBridge(reserved.into()))
            );
        }
        assert_eq!(spec("oplv", None).validate(), Ok(()));
    }

    #[test]
    fn names_the_bridge_and_mtu_of_each_kind() {
        let lan = spec("vmbr0", Some(10));
        assert_eq!(lan.bridge_name(), "vmbr0");
        assert_eq!(lan.mtu(), None);
        assert_eq!(lan.vni(), None);
        let overlay = vxlan(16_777_215, None);
        assert_eq!(overlay.bridge_name(), "oplv16777215");
        assert_eq!(overlay.mtu(), Some(DEFAULT_VXLAN_MTU));
        assert_eq!(overlay.vni(), Some(16_777_215));
        assert_eq!(vxlan(7, Some(9000)).mtu(), Some(9000));
        // 가장 큰 VNI로도 인터페이스 이름 규칙에 맞는다.
        assert!(valid_ifname(&vxlan_device(16_777_215)));
        assert_eq!(vxlan_device(7), "oplx7");
    }

    #[test]
    fn recognizes_managed_interface_names() {
        for yes in ["oplv1", "oplx16777215", "oplv007"] {
            assert!(is_managed_ifname(yes), "{yes}");
        }
        for no in [
            "oplv",
            "oplx",
            "opl1",
            "oplz1",
            "oplv1a",
            "xoplv1",
            "vmbr0",
            "op0123456789n0",
        ] {
            assert!(!is_managed_ifname(no), "{no}");
        }
        assert!(is_managed_ifname(&vxlan_bridge(42)));
        // tap 이름과 겹치지 않는다.
        assert!(!is_managed_ifname(&tap_name(Uuid::now_v7(), 0)));
    }
}
