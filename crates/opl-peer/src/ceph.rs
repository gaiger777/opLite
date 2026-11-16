//! Ceph 자동 구성(계획 56)의 노드 사이 RPC: 다른 노드의 디스크 목록(`NodeDisks`)과, mon 노드에서 읽는 Ceph
//! 상태(`CephStatus`). 답하는 처리는 opld가 [`CephSource`]로 넣는다(위트니스는 넣지 않는다).

use async_trait::async_trait;
use opl_types::ceph::{CephReport, NodeDisk};
use serde::{Deserialize, Serialize};

/// 디스크 목록 읽기(값 없음)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDisksRequest {}

/// 그 노드의 디스크마다 OSD로 쓸 수 있는지
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDisksReply {
    pub disks: Vec<NodeDisk>,
}

/// Ceph 상태 읽기(값 없음)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CephStatusRequest {}

/// 그 노드에서 읽은 Ceph 상태
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CephStatusReply {
    pub report: CephReport,
}

/// 이 노드의 디스크와 Ceph 상태를 읽는 처리
#[async_trait]
pub trait CephSource: Send + Sync {
    /// 이 노드의 디스크 목록. 읽지 못하면 오류다(`failed_precondition`).
    async fn disks(&self) -> Result<Vec<NodeDisk>, String>;
    /// 이 노드(admin 키링이 있는 mon 노드)에서 읽은 Ceph 상태. 읽지 못하면 오류다(`failed_precondition`).
    async fn status(&self) -> Result<CephReport, String>;
}
