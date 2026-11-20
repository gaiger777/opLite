//! 유동 IP(계획 66 §9.5): 공급자 네트워크의 IPv4 주소 하나를 VM NIC 하나에 1:1로 잇는다(OVN `dnat_and_snat`). 주소는
//! VM보다 오래 산다(떼었다 다른 VM에 붙인다). 네트워크 폴더에 산다.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::MacAddr;

pub type FloatingIp = Resource<FloatingIpSpec, FloatingIpStatus>;

/// 설명의 길이 상한(글자)
pub const MAX_DESCRIPTION: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct FloatingIpSpec {
    /// 공급자 네트워크 이름
    pub network: String,
    /// 유동 IP(IPv4). API가 그 네트워크의 유동 IP 풀(없으면 풀)에서 채운다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ip", nullable = false, default)
    )]
    pub address: Option<IpAddr>,
    /// 붙은 VM NIC. 없으면 떼어 둔 것이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub target: Option<FipTarget>,
    /// 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub folder: Option<Uuid>,
    /// 200자 이하
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub description: Option<String>,
}

impl ResourceSpec for FloatingIpSpec {
    const KIND: Kind = Kind::FloatingIp;
    type Status = FloatingIpStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct FloatingIpStatus {}

/// 유동 IP가 붙은 NIC. NIC는 MAC으로 가리킨다(순서가 바뀌어도 같은 NIC — OVN 포트 이름과 같다).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct FipTarget {
    pub vm: Uuid,
    pub mac: MacAddr,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FloatingIpError {
    #[error("a floating IP must be an IPv4 address, not {0} (OVN NAT is IPv4 only)")]
    NotIpv4(IpAddr),
    #[error("description must be at most {MAX_DESCRIPTION} characters")]
    LongDescription,
}

impl FloatingIpSpec {
    /// 스펙만으로 보는 검사. 주소가 공급자 네트워크 서브넷 안인지, 대상 NIC가 라우팅되는지는 API가 본다.
    pub fn validate(&self) -> Result<(), FloatingIpError> {
        if let Some(a) = self.address
            && !a.to_canonical().is_ipv4()
        {
            return Err(FloatingIpError::NotIpv4(a));
        }
        if self
            .description
            .as_ref()
            .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION)
        {
            return Err(FloatingIpError::LongDescription);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_format_round_trips_and_validates() {
        let json = serde_json::json!({
            "network": "ext",
            "address": "203.0.113.120",
            "target": { "vm": "00000000-0000-0000-0000-000000000007", "mac": "52:54:00:00:00:01" },
            "folder": "00000000-0000-0000-0000-000000000003",
            "description": "web"
        });
        let f: FloatingIpSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&f).unwrap(), json);
        assert_eq!(f.validate(), Ok(()));
        let bare: FloatingIpSpec =
            serde_json::from_value(serde_json::json!({ "network": "ext" })).unwrap();
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"network":"ext"}"#
        );
        let v6 = FloatingIpSpec {
            address: Some("2001:db8::5".parse().unwrap()),
            ..bare.clone()
        };
        assert_eq!(
            v6.validate(),
            Err(FloatingIpError::NotIpv4("2001:db8::5".parse().unwrap()))
        );
        let long = FloatingIpSpec {
            description: Some("x".repeat(201)),
            ..bare
        };
        assert_eq!(long.validate(), Err(FloatingIpError::LongDescription));
        assert!(
            serde_json::from_value::<FloatingIpSpec>(
                serde_json::json!({ "network": "ext", "port": 80 })
            )
            .is_err()
        );
        assert_eq!(FloatingIpSpec::KIND.as_str(), "floating-ip");
    }
}
