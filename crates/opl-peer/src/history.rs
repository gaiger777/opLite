//! 설정 이력 읽기 RPC(`ReadHistory`, 계획 73 §12.1): 이력 표는 Raft 구성원의 상태 머신에만 있다(관찰 노드는 리소스만
//! 따라온다). 관찰 노드의 API가 이력을 물으면 구성원에게 이 RPC로 읽는다. 관찰 노드가 받으면 `failed_precondition`이다.

use opl_types::history::HistoryRecord;
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 한 번에 읽는 줄 수 한도(타임라인 쪽 하나)
pub const MAX_HISTORY_READ: usize = 5_000;

/// 무엇을 읽나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "read", rename_all = "snake_case")]
pub enum HistoryRequest {
    /// 리소스 하나의 줄(오래된 것부터)
    Resource { kind: Kind, id: Uuid },
    /// 클러스터 전체를 새것부터 `limit`줄(`before` revision 앞에서)
    Timeline {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<u64>,
        limit: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryReadReply {
    pub records: Vec<HistoryRecord>,
}
