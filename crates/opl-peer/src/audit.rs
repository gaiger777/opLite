//! 감사 로그 읽기의 노드 사이 RPC: 한 노드의 API가 다른 노드의 감사 로그를 읽는다(`ReadAudit`).
//! 읽는 쪽 처리는 opld가 [`AuditSource`]로 넣는다.

use async_trait::async_trait;
use opl_types::audit::{AuditEntry, AuditQuery};
use serde::{Deserialize, Serialize};

/// 읽은 항목(새 것부터, 최대 `limit + 1`개)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReply {
    pub entries: Vec<AuditEntry>,
}

/// 이 노드의 감사 로그를 읽는 처리
#[async_trait]
pub trait AuditSource: Send + Sync {
    /// 조건에 맞는 항목을 새 것부터 `limit + 1`개까지 돌려준다.
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String>;

    /// 감사 로그가 꺼져 있으면 거짓이다. 읽기 요청은 `failed_precondition`으로 거부한다.
    fn enabled(&self) -> bool {
        true
    }
}
