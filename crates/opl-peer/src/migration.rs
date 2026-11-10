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
    /// 대상 노드에 새로 만들어 복사할 디스크(VM `status.migration.disks`의 차례). 비면 빼고 보내므로
    /// 공유 풀만의 마이그레이션은 옛 노드도 읽는다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<IncomingDisk>,
}

/// 대상 노드에 만들 디스크 하나: 볼륨과 원본의 가상 크기
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingDisk {
    pub volume: Uuid,
    pub size_bytes: u64,
}

/// 받을 준비가 끝났다: 받는 QEMU가 연 포트
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateIncomingResponse {
    pub port: u16,
    /// 복사할 디스크를 내보내는 NBD 서버의 포트(복사할 디스크가 있을 때만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbd_port: Option<u16>,
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
    /// VM을 받을 QEMU를 띄우고 받기 시작한다(복사할 디스크가 있으면 만들어 내보낸다). 연 포트를
    /// 돌려준다.
    async fn incoming(
        &self,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError>;

    /// 받는 QEMU가 받기를 마칠 때까지 기다린다.
    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError>;

    /// 확정 전의 마이그레이션을 버린다: 받는 QEMU를 끄고 켠 디스크를 끄고, 복사하려고 만든 디스크를
    /// 지운다.
    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disks_and_nbd_port_are_sent_only_when_there_are_disks_to_copy() {
        let (vm, task, volume) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let mut req = MigrateIncomingRequest {
            vm,
            task,
            qemu_version: "8.2.2".into(),
            disks: vec![],
        };
        // 옛 노드가 보내고 읽는 모양 그대로다.
        let old = serde_json::json!({ "vm": vm, "task": task, "qemu_version": "8.2.2" });
        assert_eq!(serde_json::to_value(&req).unwrap(), old);
        assert_eq!(
            serde_json::from_value::<MigrateIncomingRequest>(old).unwrap(),
            req
        );
        req.disks.push(IncomingDisk {
            volume,
            size_bytes: 1 << 30,
        });
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["disks"],
            serde_json::json!([{ "volume": volume, "size_bytes": 1u64 << 30 }])
        );
        assert_eq!(
            serde_json::from_value::<MigrateIncomingRequest>(json).unwrap(),
            req
        );

        let reply = MigrateIncomingResponse {
            port: 60000,
            nbd_port: None,
        };
        let old = serde_json::json!({ "port": 60000 });
        assert_eq!(serde_json::to_value(reply).unwrap(), old);
        assert_eq!(
            serde_json::from_value::<MigrateIncomingResponse>(old).unwrap(),
            reply
        );
        let reply = MigrateIncomingResponse {
            port: 60000,
            nbd_port: Some(60001),
        };
        let json = serde_json::to_value(reply).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "port": 60000, "nbd_port": 60001 })
        );
        assert_eq!(
            serde_json::from_value::<MigrateIncomingResponse>(json).unwrap(),
            reply
        );
    }
}
