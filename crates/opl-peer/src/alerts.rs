//! 경보 읽기의 노드 사이 RPC(계획 60 §8·§9): 한 노드의 API가 다른 노드가 평가하는 노드 경보와 그 노드가 보낸 알림
//! 기록을 읽는다(`ReadAlerts`). 처리는 opld가 [`AlertsSource`](opl-api의 경보 장부)로 넣는다.

use async_trait::async_trait;
use opl_types::alert::{AlertsQuery, AlertsReply};

/// 이 노드의 경보와 보낸 기록을 읽는 처리
#[async_trait]
pub trait AlertsSource: Send + Sync {
    /// 활성 노드 경보와(`query.history`가 있으면) 그 창의 보낸 기록(새 것부터, 최대 `limit + 1`개)
    async fn read(&self, query: AlertsQuery) -> Result<AlertsReply, String>;
}
