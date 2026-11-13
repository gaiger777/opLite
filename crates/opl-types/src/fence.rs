//! 노드 전원 끄기 펜싱(설계 §8.1). 리더가 리스를 잃은 노드를 BMC(IPMI·Redfish)나 명령으로 끈 뒤 HA VM을
//! 옮긴다. 노드마다 `NodeFence` 리소스 하나(id·이름 = 노드 id·이름)에 방법을 차례로 둔다. `NodeSpec`은
//! `deny_unknown_fields`라 옛 노드가 풀지 못하므로 따로 둔다.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::node::{FAST_HA, Node};
use crate::resource::{Kind, Resource, ResourceSpec};

pub type NodeFence = Resource<NodeFenceSpec, NodeFenceStatus>;

/// 노드 하나의 방법 수 상한
pub const MAX_FENCE_METHODS: usize = 4;
/// IPMI over LAN의 기본 포트
pub const DEFAULT_IPMI_PORT: u16 = 623;

/// 노드를 끄는 방법들. 앞의 것부터 해 보고, 꺼진 것을 확인하지 못하면 다음 것을 한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeFenceSpec {
    /// 1~4개
    pub methods: Vec<FenceMethod>,
}

impl ResourceSpec for NodeFenceSpec {
    const KIND: Kind = Kind::NodeFence;
    type Status = NodeFenceStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NodeFenceStatus {}

/// 노드를 끄는 방법 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FenceMethod {
    Ipmi(IpmiFence),
    Redfish(RedfishFence),
    Command(CommandFence),
}

/// 방법의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FenceKind {
    Ipmi,
    Redfish,
    Command,
}

impl FenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ipmi => "ipmi",
            Self::Redfish => "redfish",
            Self::Command => "command",
        }
    }
}

impl FenceMethod {
    pub fn kind(&self) -> FenceKind {
        match self {
            Self::Ipmi(_) => FenceKind::Ipmi,
            Self::Redfish(_) => FenceKind::Redfish,
            Self::Command(_) => FenceKind::Command,
        }
    }
}

/// `ipmitool [-I lanplus] -H address -p port -U user -E chassis power off|status`. 비밀번호는 명령 인자가 아니라
/// `IPMI_PASSWORD` 환경 변수로 넘긴다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct IpmiFence {
    /// BMC 주소(호스트 이름이나 IP)
    pub address: String,
    #[serde(default = "default_ipmi_port")]
    #[cfg_attr(feature = "openapi", schema(default = 623))]
    pub port: u16,
    pub user: String,
    /// BMC 비밀번호. API는 내보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub password: Option<String>,
    /// IPMI 2.0(`-I lanplus`)
    #[serde(default = "yes")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub lanplus: bool,
}

// 비밀번호가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for IpmiFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpmiFence")
            .field("address", &self.address)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("lanplus", &self.lanplus)
            .finish()
    }
}

/// Redfish BMC. Basic 인증으로 `ComputerSystem.Reset`(`ForceOff`)을 부르고 `PowerState`로 확인한다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RedfishFence {
    /// `https://bmc[:port]`(`http://`도 된다)
    pub url: String,
    pub user: String,
    /// BMC 비밀번호. API는 내보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub password: Option<String>,
    /// `/redfish/v1/Systems/<id>`의 id. 없으면 시스템이 하나일 때 그것을 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub system_id: Option<String>,
    /// BMC 인증서를 확인할 CA(PEM). 없으면 시스템 루트
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ca_pem: Option<String>,
    /// BMC 인증서를 확인하지 않는다(자체 서명 인증서).
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub insecure: bool,
}

// 비밀번호가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for RedfishFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedfishFence")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("system_id", &self.system_id)
            .field("ca_pem", &self.ca_pem)
            .field("insecure", &self.insecure)
            .finish()
    }
}

/// 리더 노드에서 `/bin/sh -c`로 돌리는 명령. 환경 변수 `ORBIVISOR_NODE_NAME`·`ORBIVISOR_NODE_ID`·
/// `ORBIVISOR_NODE_ADDRESS`(노드 API 주소의 호스트)가 끌 노드를 알린다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CommandFence {
    /// 노드를 끈다. 0이 아닌 종료는 실패다.
    pub off: String,
    /// 첫 줄에 `off`나 `on`을 찍는다(그 밖은 모름).
    pub status: String,
}

fn default_ipmi_port() -> u16 {
    DEFAULT_IPMI_PORT
}

fn yes() -> bool {
    true
}

/// 방법으로 읽은 노드의 전원
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Power {
    On,
    Off,
    Unknown,
}

/// 방법 하나로 전원을 읽은 결과(`fence-test`, 이웃 확인)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FenceMethodStatus {
    pub kind: FenceKind,
    /// 읽지 못했으면 `unknown`
    pub power: Power,
    /// 읽지 못한 이유
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FenceError {
    #[error("give 1-{MAX_FENCE_METHODS} fencing methods")]
    Methods,
    #[error("method {0}: {1} must not be empty")]
    Empty(usize, &'static str),
    #[error("method {0}: {1} must not contain spaces or control characters")]
    Spaces(usize, &'static str),
    #[error("method {0}: port must not be 0")]
    Port(usize),
    #[error("method {0}: url must start with http:// or https://")]
    Url(usize),
    #[error("method {0}: url must not contain a user or password; use user and password")]
    UrlUser(usize),
    #[error("method {0}: url must be only the scheme, host and port (no path, query or fragment)")]
    UrlPath(usize),
    #[error(
        "method {0}: system_id may hold only letters, digits, '.', '_' and '-', and must not be '.' or '..'"
    )]
    SystemId(usize),
}

impl NodeFenceSpec {
    /// 형식만 본다(노드가 있는지·위트니스인지는 API가 본다). 방법 번호는 1부터다.
    pub fn validate(&self) -> Result<(), FenceError> {
        if self.methods.is_empty() || self.methods.len() > MAX_FENCE_METHODS {
            return Err(FenceError::Methods);
        }
        for (i, m) in self.methods.iter().enumerate() {
            let n = i + 1;
            match m {
                FenceMethod::Ipmi(ipmi) => {
                    word(n, "address", &ipmi.address)?;
                    word(n, "user", &ipmi.user)?;
                    if ipmi.port == 0 {
                        return Err(FenceError::Port(n));
                    }
                }
                FenceMethod::Redfish(redfish) => {
                    word(n, "url", &redfish.url)?;
                    let rest = redfish
                        .url
                        .strip_prefix("https://")
                        .or_else(|| redfish.url.strip_prefix("http://"));
                    let Some(rest) = rest.filter(|r| !r.is_empty()) else {
                        return Err(FenceError::Url(n));
                    };
                    // `https://user:pw@bmc`: 응답·오류에 비밀번호가 드러난다.
                    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                    if authority.contains('@') {
                        return Err(FenceError::UrlUser(n));
                    }
                    // 요청 경로(`/redfish/v1/…`)는 실행기가 붙인다.
                    if !matches!(&rest[authority.len()..], "" | "/") {
                        return Err(FenceError::UrlPath(n));
                    }
                    word(n, "user", &redfish.user)?;
                    if let Some(id) = &redfish.system_id {
                        word(n, "system_id", id)?;
                        // 경로 한 조각으로 붙는다(`…/Systems/<id>`).
                        let plain = id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
                        if !plain || id == "." || id == ".." {
                            return Err(FenceError::SystemId(n));
                        }
                    }
                }
                FenceMethod::Command(command) => {
                    if command.off.trim().is_empty() {
                        return Err(FenceError::Empty(n, "off"));
                    }
                    if command.status.trim().is_empty() {
                        return Err(FenceError::Empty(n, "status"));
                    }
                }
            }
        }
        Ok(())
    }
}

/// 빈 글·공백·제어 문자가 없는 한 낱말
fn word(n: usize, field: &'static str, value: &str) -> Result<(), FenceError> {
    if value.is_empty() {
        return Err(FenceError::Empty(n, field));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(FenceError::Spaces(n, field));
    }
    Ok(())
}

/// 노드 `me`의 전원을 대신 읽어 줄 이웃: 온라인·위트니스 아님·`fast-ha`를 아는 다른 노드 가운데 id 순으로
/// `me` 다음(끝이면 처음). 없으면 None.
pub fn neighbour(nodes: &[Node], me: Uuid) -> Option<&Node> {
    let mut able: Vec<&Node> = nodes
        .iter()
        .filter(|n| {
            n.id != me
                && n.status.online
                && !n.spec.witness
                && n.status.features.iter().any(|f| f == FAST_HA)
        })
        .collect();
    able.sort_by_key(|n| n.id);
    able.iter()
        .find(|n| n.id > me)
        .or_else(|| able.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::node::{NodeSpec, NodeStatus};

    fn spec(methods: serde_json::Value) -> Result<NodeFenceSpec, serde_json::Error> {
        serde_json::from_value(json!({ "methods": methods }))
    }

    #[test]
    fn kind_is_node_fence() {
        assert_eq!(NodeFenceSpec::KIND, Kind::NodeFence);
        assert_eq!(serde_json::to_value(Kind::NodeFence).unwrap(), "node-fence");
        assert_eq!(Kind::NodeFence.as_str(), "node-fence");
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let s = spec(json!([
            { "kind": "ipmi", "address": "10.0.0.9", "user": "admin", "password": "pw" },
            { "kind": "redfish", "url": "https://bmc", "user": "root" },
            { "kind": "command", "off": "poweroff-node", "status": "node-power" },
        ]))
        .unwrap();
        assert_eq!(
            s.methods[0],
            FenceMethod::Ipmi(IpmiFence {
                address: "10.0.0.9".into(),
                port: 623,
                user: "admin".into(),
                password: Some("pw".into()),
                lanplus: true,
            })
        );
        assert_eq!(
            s.methods[1],
            FenceMethod::Redfish(RedfishFence {
                url: "https://bmc".into(),
                user: "root".into(),
                password: None,
                system_id: None,
                ca_pem: None,
                insecure: false,
            })
        );
        assert_eq!(s.methods[2].kind(), FenceKind::Command);
        assert_eq!(s.validate(), Ok(()));
        // 다시 쓰면 없는 선택 필드는 빠지고 다시 읽힌다.
        let text = serde_json::to_value(&s).unwrap();
        assert!(text["methods"][1].get("password").is_none(), "{text}");
        assert_eq!(serde_json::from_value::<NodeFenceSpec>(text).unwrap(), s);
    }

    #[test]
    fn unknown_fields_and_kinds_are_rejected() {
        for bad in [
            json!([{ "kind": "ipmi", "address": "a", "user": "u", "pasword": "x" }]),
            json!([{ "kind": "redfish", "url": "https://b", "user": "u", "verify": false }]),
            json!([{ "kind": "command", "off": "a", "status": "b", "on": "c" }]),
            json!([{ "kind": "ilo", "address": "a" }]),
        ] {
            assert!(spec(bad.clone()).is_err(), "{bad}");
        }
        assert!(
            serde_json::from_value::<NodeFenceSpec>(json!({ "methods": [], "order": 1 })).is_err()
        );
    }

    #[test]
    fn validates_methods() {
        let ipmi = json!({ "kind": "ipmi", "address": "bmc", "user": "u" });
        let check = |methods: serde_json::Value| spec(methods).unwrap().validate();
        assert_eq!(check(json!([])), Err(FenceError::Methods));
        assert_eq!(
            check(json!([ipmi, ipmi, ipmi, ipmi, ipmi])),
            Err(FenceError::Methods)
        );
        assert_eq!(check(json!([ipmi, ipmi, ipmi, ipmi])), Ok(()));
        let bad = [
            (
                json!({ "kind": "ipmi", "address": "", "user": "u" }),
                FenceError::Empty(1, "address"),
            ),
            (
                json!({ "kind": "ipmi", "address": "a b", "user": "u" }),
                FenceError::Spaces(1, "address"),
            ),
            (
                json!({ "kind": "ipmi", "address": "a", "user": "" }),
                FenceError::Empty(1, "user"),
            ),
            (
                json!({ "kind": "ipmi", "address": "a", "user": "u", "port": 0 }),
                FenceError::Port(1),
            ),
            (
                json!({ "kind": "redfish", "url": "ftp://bmc", "user": "u" }),
                FenceError::Url(1),
            ),
            (
                json!({ "kind": "redfish", "url": "https://", "user": "u" }),
                FenceError::Url(1),
            ),
            (
                json!({ "kind": "redfish", "url": "https://b", "user": "u", "system_id": "1/2" }),
                FenceError::SystemId(1),
            ),
            (
                json!({ "kind": "redfish", "url": "https://root:pw@bmc/", "user": "u" }),
                FenceError::UrlUser(1),
            ),
            (
                json!({ "kind": "redfish", "url": "http://root@bmc", "user": "u" }),
                FenceError::UrlUser(1),
            ),
            (
                json!({ "kind": "command", "off": " ", "status": "s" }),
                FenceError::Empty(1, "off"),
            ),
            (
                json!({ "kind": "command", "off": "o", "status": "" }),
                FenceError::Empty(1, "status"),
            ),
        ];
        for (method, err) in bad {
            assert_eq!(check(json!([method])), Err(err), "{method}");
        }
        // 방법 번호는 1부터다.
        assert_eq!(
            check(json!([ipmi, { "kind": "redfish", "url": "bmc", "user": "u" }])),
            Err(FenceError::Url(2))
        );
        assert_eq!(
            check(
                json!([{ "kind": "redfish", "url": "http://10.0.0.1:8000", "user": "u", "system_id": "1" }])
            ),
            Ok(())
        );
        // 경로의 `@`는 사용자 정보가 아니지만 경로는 둘 수 없다.
        assert_eq!(
            check(json!([{ "kind": "redfish", "url": "https://bmc/a@b", "user": "u" }])),
            Err(FenceError::UrlPath(1))
        );
        // url은 scheme·호스트·포트(끝의 `/`는 된다)뿐이다.
        let url = |url: &str| check(json!([{ "kind": "redfish", "url": url, "user": "u" }]));
        assert_eq!(url("https://bmc:8443/"), Ok(()));
        for bad in [
            "https://bmc/redfish/v1",
            "https://bmc//",
            "https://bmc?x=1",
            "https://bmc/?x=1",
            "https://bmc#top",
        ] {
            assert_eq!(url(bad), Err(FenceError::UrlPath(1)), "{bad}");
        }
        // system_id는 경로 한 조각이 되는 글자만 받는다.
        let id = |id: &str| {
            check(
                json!([{ "kind": "redfish", "url": "https://bmc", "user": "u", "system_id": id }]),
            )
        };
        assert_eq!(id("System.Embedded_1-a"), Ok(()));
        for bad in [".", "..", "1/2", "a%2F", "a?b", "a#b", "x\\y"] {
            assert_eq!(id(bad), Err(FenceError::SystemId(1)), "{bad}");
        }
    }

    #[test]
    fn debug_hides_passwords() {
        let s = spec(json!([
            { "kind": "ipmi", "address": "a", "user": "u", "password": "ipmi-s3cret" },
            { "kind": "redfish", "url": "https://b", "user": "u", "password": "rf-s3cret" },
        ]))
        .unwrap();
        let shown = format!("{s:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains("***"), "{shown}");
    }

    fn node(id: u128, online: bool, witness: bool, fast_ha: bool) -> Node {
        let mut n = Resource::new(
            format!("n{id}"),
            NodeSpec {
                address: "h:1".into(),
                maintenance: false,
                witness,
                roles: None,
            },
        );
        n.id = Uuid::from_u128(id);
        n.status = NodeStatus {
            online,
            features: if fast_ha {
                vec![FAST_HA.into()]
            } else {
                vec![]
            },
            ..Default::default()
        };
        n
    }

    #[test]
    fn the_neighbour_is_the_next_able_node_by_id() {
        let id = |n: &Node| n.id.as_u128();
        let nodes = vec![
            node(3, true, false, true),
            node(1, true, false, true),
            node(2, true, false, true),
        ];
        assert_eq!(neighbour(&nodes, Uuid::from_u128(1)).map(id), Some(2));
        assert_eq!(neighbour(&nodes, Uuid::from_u128(2)).map(id), Some(3));
        // 끝이면 처음으로 돈다.
        assert_eq!(neighbour(&nodes, Uuid::from_u128(3)).map(id), Some(1));
        // 꺼진 노드·위트니스·옛 노드는 건너뛴다.
        let nodes = vec![
            node(1, true, false, true),
            node(2, false, false, true),
            node(3, true, true, true),
            node(4, true, false, false),
            node(5, true, false, true),
        ];
        assert_eq!(neighbour(&nodes, Uuid::from_u128(1)).map(id), Some(5));
        assert_eq!(neighbour(&nodes, Uuid::from_u128(5)).map(id), Some(1));
        // 나 말고 없으면 없다.
        assert_eq!(neighbour(&nodes[..1], Uuid::from_u128(1)).map(id), None);
    }
}
