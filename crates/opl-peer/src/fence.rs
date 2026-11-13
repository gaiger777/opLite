//! 전원 끄기 펜싱 확인의 노드 사이 RPC(`FenceStatus`): 한 노드가 이웃에게 자기 BMC(펜싱 방법)로 자기 전원을
//! 읽어 달라고 한다. 리더가 그 노드를 끌 수 있는지 미리 알려고 쓴다. 읽는 쪽 처리는 opld가 [`FenceSource`]로
//! 넣는다.

use async_trait::async_trait;
use opl_types::fence::FenceMethodStatus;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 전원을 읽을 노드
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FenceStatusRequest {
    pub node: Uuid,
}

/// 그 노드의 `NodeFence` 방법마다의 결과(방법 순서)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FenceStatusReply {
    pub methods: Vec<FenceMethodStatus>,
}

/// 노드의 전원을 그 노드의 펜싱 방법으로 읽는 처리
#[async_trait]
pub trait FenceSource: Send + Sync {
    /// 방법마다 전원을 읽는다. 노드나 그 `NodeFence`가 없으면 오류다(`failed_precondition`).
    async fn status(&self, node: Uuid) -> Result<Vec<FenceMethodStatus>, String>;
}
