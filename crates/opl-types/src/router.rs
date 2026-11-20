//! SDN 라우터(계획 66 §9): 오버레이 SDN 네트워크(서브넷 게이트웨이)를 잇는 OVN 분산 논리 라우터, 정적 경로, 공급자
//! 네트워크로 나가는 외부 게이트웨이(SNAT). 네트워크 폴더에 산다. 원하는 상태만 Raft에 있고 리더 `OvnSync`가 NB의
//! `Logical_Router`·라우터 포트·경로·NAT·`HA_Chassis_Group`으로 옮겨 쓴다. 게이트웨이 주인 같은 살아 있는 상태는 Raft에
//! 쓰지 않고 보기에서 NB를 읽는다.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::security_group::Cidr;

pub type Router = Resource<RouterSpec, RouterStatus>;

/// 라우터 하나의 인터페이스 수 상한
pub const MAX_INTERFACES: usize = 16;
/// 라우터 하나의 정적 경로 수 상한
pub const MAX_ROUTES: usize = 64;
/// 설명의 길이 상한(글자)
pub const MAX_DESCRIPTION: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RouterSpec {
    /// 든 네트워크 폴더(`Folder` id). 없으면 네트워크 트리의 루트다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub folder: Option<Uuid>,
    /// 200자 이하
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub description: Option<String>,
    /// 외부 게이트웨이(공급자 네트워크). 없으면 라우터는 안쪽 네트워크끼리만 잇는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub external: Option<RouterExternal>,
    /// 잇는 오버레이 SDN 네트워크(16개까지). 그 네트워크의 서브넷 게이트웨이가 라우터 포트 주소다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub interfaces: Vec<RouterInterface>,
    /// 정적 경로(64개까지). 외부가 있으면 기본 경로는 외부 서브넷 게이트웨이로 저절로 생긴다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub routes: Vec<StaticRoute>,
}

impl ResourceSpec for RouterSpec {
    const KIND: Kind = Kind::Router;
    type Status = RouterStatus;
}

/// 지금은 칸이 없다(게이트웨이 주인·순위는 보기가 NB에서 읽는다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RouterStatus {}

/// 라우터의 외부 게이트웨이
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RouterExternal {
    /// 공급자 네트워크 이름
    pub network: String,
    /// 라우터의 외부 주소(체계마다 하나). API가 그 네트워크의 유동 IP 풀(없으면 풀)에서 채운다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>, default))]
    pub ips: Vec<IpAddr>,
    /// 안쪽 IPv4 서브넷의 보낸 주소를 외부 주소로 바꾼다(SNAT). 없으면 참이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub snat: Option<bool>,
}

impl RouterExternal {
    /// SNAT를 하는지(없으면 참)
    pub fn snat(&self) -> bool {
        self.snat.unwrap_or(true)
    }
}

/// 라우터 인터페이스 하나(오버레이 SDN 네트워크)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RouterInterface {
    /// 네트워크 이름
    pub network: String,
}

/// 정적 경로
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct StaticRoute {
    #[cfg_attr(feature = "openapi", schema(value_type = String, example = "10.20.0.0/16"))]
    pub destination: Cidr,
    /// 다음 홉(붙은 서브넷 안의 주소, 같은 체계)
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub nexthop: IpAddr,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RouterError {
    #[error("a router can have at most {MAX_INTERFACES} interfaces")]
    TooManyInterfaces,
    #[error("network {0:?} is a router interface twice")]
    DuplicateInterface(String),
    #[error("a router can have at most {MAX_ROUTES} routes")]
    TooManyRoutes,
    #[error("route {0}: the nexthop {1} is not of the destination's address family")]
    RouteFamily(Cidr, IpAddr),
    #[error("route {0} is given twice")]
    DuplicateRoute(Cidr),
    #[error(
        "route {0}: a router with an external gateway gets its default route from the external subnet's gateway"
    )]
    DefaultRouteWithExternal(Cidr),
    #[error("the external gateway can have at most one IPv4 and one IPv6 address")]
    ExternalIps,
    #[error("description must be at most {MAX_DESCRIPTION} characters")]
    LongDescription,
}

impl RouterSpec {
    /// 스펙만으로 보는 검사. 네트워크를 알아야 하는 검사(인터페이스가 서브넷 있는 오버레이 SDN, 외부가 공급자 네트워크,
    /// 서브넷끼리 겹치지 않음, 다음 홉이 붙은 서브넷 안)는 API가 한다.
    pub fn validate(&self) -> Result<(), RouterError> {
        if self
            .description
            .as_ref()
            .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION)
        {
            return Err(RouterError::LongDescription);
        }
        if self.interfaces.len() > MAX_INTERFACES {
            return Err(RouterError::TooManyInterfaces);
        }
        for (i, a) in self.interfaces.iter().enumerate() {
            if self.interfaces[..i].iter().any(|b| b.network == a.network) {
                return Err(RouterError::DuplicateInterface(a.network.clone()));
            }
        }
        if self.routes.len() > MAX_ROUTES {
            return Err(RouterError::TooManyRoutes);
        }
        for (i, r) in self.routes.iter().enumerate() {
            if r.destination.addr.is_ipv4() != r.nexthop.to_canonical().is_ipv4() {
                return Err(RouterError::RouteFamily(r.destination, r.nexthop));
            }
            if self.routes[..i]
                .iter()
                .any(|o| o.destination == r.destination)
            {
                return Err(RouterError::DuplicateRoute(r.destination));
            }
            if self.external.is_some() && r.destination.prefix == 0 {
                return Err(RouterError::DefaultRouteWithExternal(r.destination));
            }
        }
        if let Some(ext) = &self.external {
            let v4 = ext.ips.iter().filter(|ip| ip.to_canonical().is_ipv4());
            if v4.count() > 1 || ext.ips.iter().filter(|ip| ip.is_ipv6()).count() > 1 {
                return Err(RouterError::ExternalIps);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(v: serde_json::Value) -> RouterSpec {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn storage_format_round_trips() {
        assert_eq!(serde_json::to_string(&RouterSpec::default()).unwrap(), "{}");
        let json = serde_json::json!({
            "folder": "00000000-0000-0000-0000-000000000003",
            "description": "edge",
            "external": { "network": "ext", "ips": ["203.0.113.10", "2001:db8::10"], "snat": false },
            "interfaces": [{ "network": "t1" }, { "network": "t2" }],
            "routes": [{ "destination": "10.20.0.0/16", "nexthop": "10.10.0.5" }]
        });
        let r = spec(json.clone());
        assert_eq!(serde_json::to_value(&r).unwrap(), json);
        assert!(!r.external.as_ref().unwrap().snat());
        assert_eq!(r.validate(), Ok(()));
        let ext: RouterExternal =
            serde_json::from_value(serde_json::json!({ "network": "ext" })).unwrap();
        assert!(ext.snat());
        assert_eq!(
            serde_json::to_value(&ext).unwrap(),
            serde_json::json!({ "network": "ext" })
        );
        assert!(serde_json::from_value::<RouterSpec>(serde_json::json!({ "peer": "r2" })).is_err());
        assert_eq!(RouterSpec::KIND.as_str(), "router");
    }

    #[test]
    fn validates() {
        let cases = [
            (
                serde_json::json!({ "interfaces": [{ "network": "a" }, { "network": "a" }] }),
                RouterError::DuplicateInterface("a".into()),
            ),
            (
                serde_json::json!({
                    "interfaces": (0..17).map(|i| serde_json::json!({ "network": format!("n{i}") })).collect::<Vec<_>>()
                }),
                RouterError::TooManyInterfaces,
            ),
            (
                serde_json::json!({ "routes": [{ "destination": "10.0.0.0/8", "nexthop": "2001:db8::1" }] }),
                RouterError::RouteFamily(
                    "10.0.0.0/8".parse().unwrap(),
                    "2001:db8::1".parse().unwrap(),
                ),
            ),
            (
                serde_json::json!({ "routes": [
                    { "destination": "10.0.0.0/8", "nexthop": "10.1.0.1" },
                    { "destination": "10.0.0.0/8", "nexthop": "10.1.0.2" }
                ] }),
                RouterError::DuplicateRoute("10.0.0.0/8".parse().unwrap()),
            ),
            (
                serde_json::json!({
                    "external": { "network": "ext" },
                    "routes": [{ "destination": "::/0", "nexthop": "2001:db8::1" }]
                }),
                RouterError::DefaultRouteWithExternal("::/0".parse().unwrap()),
            ),
            (
                serde_json::json!({ "external": { "network": "ext", "ips": ["203.0.113.1", "203.0.113.2"] } }),
                RouterError::ExternalIps,
            ),
            (
                serde_json::json!({ "description": "x".repeat(201) }),
                RouterError::LongDescription,
            ),
        ];
        for (v, err) in cases {
            assert_eq!(spec(v.clone()).validate(), Err(err), "{v}");
        }
        // 외부가 없으면 기본 경로를 직접 줄 수 있다.
        let r = spec(serde_json::json!({
            "routes": [{ "destination": "0.0.0.0/0", "nexthop": "10.0.0.254" }]
        }));
        assert_eq!(r.validate(), Ok(()));
        let routes = (0..65)
            .map(|i| serde_json::json!({ "destination": format!("10.{i}.0.0/16"), "nexthop": "10.0.0.1" }))
            .collect::<Vec<_>>();
        assert_eq!(
            spec(serde_json::json!({ "routes": routes })).validate(),
            Err(RouterError::TooManyRoutes)
        );
    }
}
