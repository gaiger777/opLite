//! 지표 읽기의 노드 사이 RPC: 한 노드의 API가 다른 노드의 메모리에 있는 지표를 읽는다(`ReadMetrics`).
//! 계획 60: 그 노드의 기록 파일을 범위로 읽고(`ReadMetricsHistory`), VM 기록 파일을 VM의 새 주인 노드에 밀어
//! 넘긴다(`PushMetricsHistory`). 읽는 쪽 처리는 opld가 [`MetricsSource`]로 넣는다.

use bytes::{BufMut, Bytes};
use orbivisor_types::metrics::{HistoryQuery, HistoryReply, MetricsQuery, NodeSeries, VmSeries};
use serde::{Deserialize, Serialize};
use tonic::codec::EncodeBuf;
use tonic::{Code, Status};
use uuid::Uuid;

use crate::codec::Binary;

/// 한 노드가 돌려준 지표
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsReply {
    /// `Nodes`를 물었을 때 그 노드의 점들
    pub node: Option<NodeSeries>,
    /// `Vm`을 물었을 때 그 노드에서 잰 VM의 점들(잰 적이 없으면 비어 있다)
    pub vms: Vec<VmSeries>,
}

/// 밀어 넘기는 VM 기록 파일 하나의 최대 크기. VM 파일은 336 KiB다(설계 §6).
pub const MAX_HISTORY_BYTES: usize = 1 << 20;

/// 기록을 읽거나 받지 못한 이유
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    /// 그 노드가 기록을 모른다(옛 노드, 기록 소스가 없는 노드).
    #[error("the node does not keep metrics history")]
    Unsupported,
    /// 그 노드가 기록 파일을 쓰지 않는다(`[metrics] history = false`). 밀어 넘기기만 이것으로 거절한다.
    #[error("the node does not write metrics history files")]
    Disabled,
    /// 잘못된 조회나 받은 파일(종류가 다름, 깨짐, 너무 큼)
    #[error("{0}")]
    Invalid(String),
    /// 그 노드의 파일을 읽거나 쓰지 못했다.
    #[error("metrics history I/O: {0}")]
    Io(String),
    /// 그 노드에 닿지 못했다. 보내는 쪽에서만 생긴다.
    #[error("cannot reach the node: {0}")]
    Unreachable(String),
}

impl From<HistoryError> for Status {
    fn from(e: HistoryError) -> Self {
        match e {
            HistoryError::Unsupported => Status::unimplemented(e.to_string()),
            HistoryError::Disabled => Status::failed_precondition(e.to_string()),
            HistoryError::Invalid(m) => Status::invalid_argument(m),
            HistoryError::Io(m) => Status::internal(m),
            HistoryError::Unreachable(m) => Status::unavailable(m),
        }
    }
}

impl From<Status> for HistoryError {
    fn from(s: Status) -> Self {
        match s.code() {
            Code::Unimplemented => Self::Unsupported,
            Code::FailedPrecondition => Self::Disabled,
            Code::InvalidArgument => Self::Invalid(s.message().to_string()),
            Code::Internal => Self::Io(s.message().to_string()),
            _ => Self::Unreachable(s.message().to_string()),
        }
    }
}

/// 이 노드의 지표를 읽는 처리
pub trait MetricsSource: Send + Sync {
    /// `since`보다 뒤의 점을 돌려준다.
    fn read(&self, q: &MetricsQuery) -> MetricsReply;

    /// 이 노드의 기록을 범위로 읽는다(계획 60 §7). 파일을 읽으므로 막히는 호출이다(서버는 `spawn_blocking`에서
    /// 부른다). 조회는 [`HistoryQuery::validate`]를 거친 것이다. 기본은 기록이 없는 노드(`Unsupported`)다.
    fn history(&self, q: &HistoryQuery) -> Result<HistoryReply, HistoryError> {
        let _ = q;
        Err(HistoryError::Unsupported)
    }

    /// 다른 노드가 밀어 넘긴 VM 기록 파일(`bytes`)을 이 노드의 그 VM 파일과 칸 단위로 합친다(계획 60 §6). 막히는
    /// 호출이다. 기본은 `Unsupported`다.
    fn push_history(&self, vm: Uuid, bytes: &[u8]) -> Result<(), HistoryError> {
        let _ = (vm, bytes);
        Err(HistoryError::Unsupported)
    }
}

/// `PushMetricsHistory` 요청: VM id(16바이트) 뒤에 기록 파일 바이트를 그대로 붙인다(JSON 숫자 배열로 부풀리지 않는다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushMetricsHistoryRequest {
    pub vm: Uuid,
    pub bytes: Bytes,
}

impl Binary for PushMetricsHistoryRequest {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        dst.put_slice(self.vm.as_bytes());
        dst.put(self.bytes);
    }

    fn decode(src: Bytes) -> Result<Self, Status> {
        if src.len() < 16 {
            return Err(Status::invalid_argument("bad metrics history push"));
        }
        let vm = Uuid::from_slice(&src[..16])
            .map_err(|_| Status::invalid_argument("bad metrics history push"))?;
        Ok(Self {
            vm,
            bytes: src.slice(16..),
        })
    }
}

/// `PushMetricsHistory`의 답(받은 쪽이 합치고 디스크에 썼다)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushMetricsHistoryReply;

impl Binary for PushMetricsHistoryReply {
    fn encode(self, _: &mut EncodeBuf<'_>) {}

    fn decode(_: Bytes) -> Result<Self, Status> {
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_survive_the_status_round_trip() {
        for e in [
            HistoryError::Unsupported,
            HistoryError::Disabled,
            HistoryError::Invalid("bad kind".into()),
            HistoryError::Io("disk full".into()),
            HistoryError::Unreachable("timed out".into()),
        ] {
            assert_eq!(HistoryError::from(Status::from(e.clone())), e);
        }
    }

    #[test]
    fn push_requests_decode_their_vm_and_bytes() {
        let src = {
            let vm = Uuid::from_u128(7);
            let mut b = vm.as_bytes().to_vec();
            b.extend_from_slice(b"OPLRRD");
            Bytes::from(b)
        };
        let req = PushMetricsHistoryRequest::decode(src).unwrap();
        assert_eq!(req.vm, Uuid::from_u128(7));
        assert_eq!(&req.bytes[..], b"OPLRRD");
        let err = PushMetricsHistoryRequest::decode(Bytes::from_static(b"short")).unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument);
    }
}
