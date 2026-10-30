//! 라이브 마이그레이션의 노드 사이 RPC: 원본 노드가 대상 노드에 받을 준비(`MigrateIncoming`), 받기
//! 확인(`MigrateIncomingWait`), 취소(`MigrateIncomingAbort`)를 부른다. 받는 쪽 처리는 opld가
//! [`MigrationReceiver`]로 넣는다.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 받을 준비 요청
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateIncomingRequest {
    pub vm: Uuid,
    /// 원본 노드에서 도는 `VmMigrate` Task
    pub task: Uuid,
    /// 원본 QEMU의 버전(`major.minor.micro`). 대상 노드의 QEMU와 같아야 한다.
    pub qemu_version: String,
}

/// 받을 준비가 끝났다: 받는 QEMU가 연 포트
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateIncomingResponse {
    pub port: u16,
}

/// 받기 확인과 취소 요청: 어느 VM의 어느 마이그레이션인지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingMigration {
    pub vm: Uuid,
    pub task: Uuid,
}

/// 받는 쪽이 거절했거나 실패한 이유. 응답 본문에 담아 보낸다(gRPC 상태는 전송 문제만).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum IncomingError {
    /// 두 노드의 QEMU 버전이 다르다(설계의 `migration_incompatible`).
    #[error("{0}")]
    Incompatible(String),
    /// 요청이 지금 상태와 맞지 않는다(VM이 이 노드로 오는 중이 아니다, 이미 확정됐다 등).
    #[error("{0}")]
    Rejected(String),
    /// 받는 노드에서 디스크나 QEMU가 실패했다.
    #[error("{0}")]
    Failed(String),
    /// 받는 노드에 닿지 못했거나 그 노드가 마이그레이션을 받지 않는다.
    #[error("cannot reach the target node: {0}")]
    Unavailable(String),
}

/// 대상 노드에서 마이그레이션을 받는 처리
#[async_trait]
pub trait MigrationReceiver: Send + Sync {
    /// VM을 받을 QEMU를 띄우고 받기 시작한다. 연 포트를 돌려준다.
    async fn incoming(&self, req: MigrateIncomingRequest) -> Result<u16, IncomingError>;

    /// 받는 QEMU가 받기를 마칠 때까지 기다린다.
    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError>;

    /// 확정 전의 마이그레이션을 버린다: 받는 QEMU를 끄고 켠 디스크를 끈다.
    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError>;
}
