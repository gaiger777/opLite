//! VM 내보내기 내려받기의 노드 사이 RPC(계획 72 §12.3): 내보내기 파일이 이 노드에서 보이지 않으면(다른 노드의 로컬 dir
//! 풀) API 노드가 파일을 쓴 노드에 서버 흐름 `ExportDownload`로 묻는다. 흐름 메시지는 파일 복원 내려받기와 같다([`
//! FileRestoreChunk`](crate::file_restore::FileRestoreChunk) — 머리 하나 뒤 바이트 조각, 끝에 항목 수). 권한과 링크는
//! API 노드가 본다. 처리는 orbivisor-api가 [`ExportSource`]로 넣는다.

use async_trait::async_trait;
use bytes::{BufMut, Bytes};
use serde::{Deserialize, Serialize};
use tonic::Status;
use tonic::codec::EncodeBuf;
use uuid::Uuid;

use crate::codec::Binary;
use crate::file_restore::{DownloadHead, DownloadStream, FileRestoreFault};

/// `ExportDownload` 요청
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportDownloadRequest {
    /// 내보내기(`vm-export`) id
    pub export: Uuid,
    /// 파일 하나만(OVF 폴더로 받기). 없으면 OVA 전체
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

/// 요청은 JSON을 이진 메시지에 담는다(파일 복원 흐름과 같은 코덱).
impl Binary for ExportDownloadRequest {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        dst.put_slice(&serde_json::to_vec(&self).unwrap_or_default());
    }

    fn decode(src: Bytes) -> Result<Self, Status> {
        serde_json::from_slice(&src)
            .map_err(|e| Status::invalid_argument(format!("bad export download request: {e}")))
    }
}

/// 이 노드의 내보내기 파일(orbivisor-api)
#[async_trait]
pub trait ExportSource: Send + Sync {
    /// 내려받기를 시작한다: 머리와 조각 흐름. 시작 전 거절은 API 오류 그대로(상태 코드·코드·문구)다.
    async fn download(
        &self,
        req: ExportDownloadRequest,
    ) -> Result<(DownloadHead, DownloadStream), FileRestoreFault>;
}

/// 그 노드가 이 RPC를 모른다(409 `cluster_upgrading`)
pub fn export_unsupported() -> FileRestoreFault {
    FileRestoreFault::new(
        409,
        "cluster_upgrading",
        "the node holding the export does not serve VM exports yet; upgrade it first",
    )
}

/// 그 노드에 닿지 못했다(503 `node_unreachable`)
pub fn export_unreachable(why: impl std::fmt::Display) -> FileRestoreFault {
    FileRestoreFault::new(
        503,
        "node_unreachable",
        format!("the node holding the export did not answer: {why}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_json() {
        let req = ExportDownloadRequest {
            export: Uuid::nil(),
            file: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v, serde_json::json!({ "export": Uuid::nil() }));
        let req = ExportDownloadRequest {
            file: Some("web.ovf".into()),
            ..req
        };
        let text = serde_json::to_vec(&req).unwrap();
        assert_eq!(
            ExportDownloadRequest::decode(Bytes::from(text)).unwrap(),
            req
        );
        assert!(ExportDownloadRequest::decode(Bytes::from_static(b"{")).is_err());
    }
}
