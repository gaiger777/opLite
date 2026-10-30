//! 다른 노드에서 UEFI 변수 파일 받아 오기. 노드 사이 RPC는 opld가 [`FirmwareFetcher`]로 넣는다
//! (reconciler는 노드 사이 통신을 모른다).

use async_trait::async_trait;
use uuid::Uuid;

/// VM의 UEFI 변수 파일을 그 파일을 가진 노드(`status.firmware_node`)에서 받아 온다.
#[async_trait]
pub trait FirmwareFetcher: Send + Sync {
    /// `node`에 있는 `vm`의 변수 파일을 받아 이 노드의 변수 파일을 바꾼다. 실패하면 이 노드의
    /// 파일은 그대로다.
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError>;
}

/// 변수 파일을 받지 못한 이유(그 노드가 오프라인이다, 파일이 없다, 전송이 끊겼다 등)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FirmwareError(pub String);
