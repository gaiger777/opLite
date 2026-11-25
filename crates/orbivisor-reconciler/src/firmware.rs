//! 다른 노드에서 UEFI 변수·TPM 상태 파일 받아 오기와 펌웨어 사본 밀어 두기(계획 59 §5). 노드 사이 RPC는 opld가
//! [`FirmwareFetcher`]로 넣는다(reconciler는 노드 사이 통신을 모른다).

use async_trait::async_trait;
use uuid::Uuid;

/// VM의 펌웨어 파일을 다른 노드와 주고받는다.
#[async_trait]
pub trait FirmwareFetcher: Send + Sync {
    /// `node`에 있는 `vm`의 변수 파일을 받아 이 노드의 변수 파일을 바꾼다. 실패하면 이 노드의
    /// 파일은 그대로다. 받으면 이 노드의 펌웨어 회차를 VM의 `firmware_epoch`로 적는다(켤 때 지워지지 않게).
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError>;

    /// `node`에 있는 `vm`의 TPM 상태 파일을 받아 이 노드의 파일을 바꾼다. 받은 파일의 SHA-256이 `sha256`과 다르면
    /// 버린다(이 노드의 파일은 그대로). 받으면 이 노드의 펌웨어 회차를 `epoch`로 적는다.
    async fn fetch_tpm(
        &self,
        vm: Uuid,
        node: Uuid,
        sha256: &str,
        epoch: u32,
    ) -> Result<(), FirmwareError> {
        let _ = (vm, node, sha256, epoch);
        Err(FirmwareError("this node cannot fetch TPM state".into()))
    }

    /// 이 노드에 있는 VM의 펌웨어 파일(TPM 상태 + UEFI 변수)을 사본 노드 `copy.to`에 밀어 둔다. 이 노드의 TPM 상태
    /// 해시가 `copy.sha256`과 다르면(그 사이 바뀜) 보내지 않는다.
    async fn push(&self, copy: &FirmwareCopy) -> Result<(), FirmwareError> {
        let _ = copy;
        Err(FirmwareError(
            "this node cannot push firmware copies".into(),
        ))
    }

    /// 노드 `to`의 펌웨어 파일을 `purge`대로 지운다(리뷰 1 I5: 초기화 전 TPM 상태, 지운 스냅샷의 사본).
    async fn purge(&self, to: Uuid, purge: &FirmwarePurge) -> Result<(), FirmwareError> {
        let _ = (to, purge);
        Err(FirmwareError(
            "this node cannot remove firmware files on other nodes".into(),
        ))
    }
}

/// 다른 노드에서 지울 펌웨어 파일
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FirmwarePurge {
    pub vm: Uuid,
    /// 초기화 뒤의 회차: 그 노드의 회차가 이와 다르면 `uefi_vars`·`tpm`을 지운다(그 노드에서 VM이 돌면 거절).
    pub epoch: Option<u32>,
    pub uefi_vars: bool,
    pub tpm: bool,
    /// 이 스냅샷의 변수·TPM 사본을 지운다.
    pub snapshot: Option<Uuid>,
}

/// 밀어 둘 펌웨어 사본 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareCopy {
    pub vm: Uuid,
    /// 받을 노드
    pub to: Uuid,
    /// 보낼 TPM 상태의 SHA-256(VM의 `tpm_sha256`)
    pub sha256: String,
    /// VM의 `firmware_epoch`
    pub epoch: u32,
    /// VM의 `secure_boot`(받는 쪽이 변수 파일 크기를 견줄 템플릿)
    pub secure_boot: bool,
}

/// 파일을 주고받지 못한 이유(그 노드가 오프라인이다, 파일이 없다, 전송이 끊겼다, 해시가 다르다 등)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FirmwareError(pub String);
