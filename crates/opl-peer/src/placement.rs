//! 최적 배치 상태의 노드 사이 RPC(`ReadPlacement`): 리더가 아닌 노드의 API가 리더 메모리의 노드 부하와 균형
//! 조정기 결과를 읽는다. 답하는 처리는 opld가 [`PlacementSource`]로 넣는다.

use async_trait::async_trait;
use opl_types::placement::PlacementStatus;
use serde::{Deserialize, Serialize};

/// 최적 배치 상태 읽기(값 없음)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementRequest {}

/// 리더의 최적 배치 상태를 만드는 처리
#[async_trait]
pub trait PlacementSource: Send + Sync {
    /// 상태를 만든다. 리더가 아니면 오류다(`failed_precondition`).
    async fn status(&self) -> Result<PlacementStatus, String>;
}
