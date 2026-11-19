//! 노드 토폴로지의 지금 값 RPC(`ReadTopology`, 계획 64 §13): API가 노드 상세 "토폴로지"에 그 노드의 빈 메모리·빈
//! hugepages·orbivisord 친화를 묻는다. 바뀔 때마다 Raft에 쓰지 않으려는 값이다. 답하는 처리는 opld가
//! [`TopologySource`]로 넣는다(Linux만 — 없으면 `unimplemented`).

use async_trait::async_trait;
use opl_types::numa::TopologyLive;
use serde::{Deserialize, Serialize};

/// 지금 값 읽기(값 없음)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyRequest {}

/// 이 노드의 지금 값을 읽는 처리
#[async_trait]
pub trait TopologySource: Send + Sync {
    /// 지금 값. 읽지 못하면 오류다(`failed_precondition`).
    async fn live(&self) -> Result<TopologyLive, String>;
}
