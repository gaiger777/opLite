//! 지표 읽기의 노드 사이 RPC: 한 노드의 API가 다른 노드의 메모리에 있는 지표를 읽는다(`ReadMetrics`).
//! 읽는 쪽 처리는 opld가 [`MetricsSource`]로 넣는다.

use opl_types::metrics::{MetricsQuery, NodeSeries, VmSeries};
use serde::{Deserialize, Serialize};

/// 한 노드가 돌려준 지표
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsReply {
    /// `Nodes`를 물었을 때 그 노드의 점들
    pub node: Option<NodeSeries>,
    /// `Vm`을 물었을 때 그 노드에서 잰 VM의 점들(잰 적이 없으면 비어 있다)
    pub vms: Vec<VmSeries>,
}

/// 이 노드의 지표를 읽는 처리
pub trait MetricsSource: Send + Sync {
    /// `since`보다 뒤의 점을 돌려준다.
    fn read(&self, q: &MetricsQuery) -> MetricsReply;
}
