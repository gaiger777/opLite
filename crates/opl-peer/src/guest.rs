//! 게스트 에이전트의 노드 사이 RPC(`GuestAgent`, 계획 59 §8): 한 노드의 API가 VM이 도는 노드의 게스트 정보 캐시를
//! 읽거나(필요하면 지금 묻기) 게스트 비밀번호를 설정한다. 처리는 opld가 [`GuestAgentSource`]로 넣는다.

use std::fmt;

use async_trait::async_trait;
use opl_types::guest::GuestInfoResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// VM 하나에 할 일
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GuestAgentOp {
    /// 게스트 정보 읽기. `refresh`면 캐시 대신 지금 묻는다(VM마다 5초에 한 번).
    Info { refresh: bool },
    /// 게스트 사용자의 비밀번호 설정. 비밀번호는 저장하지도 적지도 않는다.
    SetPassword { user: String, password: String },
}

/// 비밀번호는 적지 않는다.
impl fmt::Debug for GuestAgentOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info { refresh } => f.debug_struct("Info").field("refresh", refresh).finish(),
            Self::SetPassword { user, .. } => f
                .debug_struct("SetPassword")
                .field("user", user)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

/// `GuestAgent` 요청
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestAgentRequest {
    pub vm: Uuid,
    pub op: GuestAgentOp,
}

/// 게스트 에이전트 일을 하지 못한 이유
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "error", content = "detail", rename_all = "snake_case")]
pub enum GuestAgentError {
    /// VM이 그 노드에서 돌고 있지 않다.
    #[error("the VM is not running")]
    NotRunning,
    /// VM에 게스트 에이전트 채널이 없다.
    #[error("the VM has no guest agent channel")]
    NoAgent,
    /// 에이전트가 답하지 않았다(API는 409 `guest_agent_unavailable`).
    #[error("guest agent unavailable: {0}")]
    Unavailable(String),
    /// 잘못된 사용자 이름·비밀번호(문구에 비밀번호를 넣지 않는다)
    #[error("{0}")]
    Invalid(String),
    /// 그 노드가 이 RPC를 모른다(옛 노드). 보내는 쪽에서만 생긴다.
    #[error("the node does not support the guest agent")]
    Unsupported,
    /// 그 노드에 닿지 못했다. 보내는 쪽에서만 생긴다.
    #[error("cannot reach the node: {0}")]
    Unreachable(String),
}

/// `GuestAgent`의 답: 정보 읽기면 `info`, 비밀번호 설정이 됐으면 둘 다 None이다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuestAgentReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<GuestInfoResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<GuestAgentError>,
}

/// 이 노드에서 도는 VM의 게스트 에이전트를 다루는 처리(opld의 게스트 정보 캐시)
#[async_trait]
pub trait GuestAgentSource: Send + Sync {
    /// VM의 게스트 정보. 이 노드에서 돌지 않으면 `not_running`, 채널이 없거나 에이전트가 답한 적이 없으면 `no_agent`다.
    async fn info(&self, vm: Uuid, refresh: bool) -> GuestInfoResponse;

    /// 게스트 사용자 `user`의 비밀번호를 바꾼다.
    async fn set_password(
        &self,
        vm: Uuid,
        user: &str,
        password: &str,
    ) -> Result<(), GuestAgentError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_password_never_shows_in_debug_output() {
        let op = GuestAgentOp::SetPassword {
            user: "Administrator".into(),
            password: "hunter2".into(),
        };
        let shown = format!(
            "{:?}",
            GuestAgentRequest {
                vm: Uuid::nil(),
                op
            }
        );
        assert!(shown.contains("Administrator"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn requests_and_errors_round_trip_as_json() {
        let req = GuestAgentRequest {
            vm: Uuid::nil(),
            op: GuestAgentOp::Info { refresh: true },
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["op"]["op"], "info");
        assert_eq!(
            serde_json::from_value::<GuestAgentRequest>(json).unwrap(),
            req
        );
        let reply = GuestAgentReply {
            info: None,
            error: Some(GuestAgentError::Unavailable("timed out".into())),
        };
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(
            json,
            r#"{"error":{"error":"unavailable","detail":"timed out"}}"#
        );
        assert_eq!(
            serde_json::from_str::<GuestAgentReply>(&json).unwrap(),
            reply
        );
    }
}
