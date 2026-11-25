//! 파일 복원 세션의 노드 사이 RPC(계획 69 §10.4): 세션 노드가 아닌 API 노드가 세션 노드에 묻는다. 단항
//! `FileRestoreCall`(세션 보기·나열·항목·닫기·내려받기 링크 — JSON)과 서버 흐름 `FileRestoreDownload`(머리 하나 뒤 바이트
//! 조각 ≤ 1 MiB, 끝에 항목 수 — 이진). 권한은 API 노드가 본다. 세션 노드는 내려받기 토큰의 사용자만 다시 맞춘다. 처리는
//! orbivisor-api가 [`FileRestoreSource`]로 넣는다.

use async_trait::async_trait;
use bytes::{Buf, BufMut, Bytes};
use orbivisor_types::file_restore::{
    DownloadLinkRequest, EntryPath, EntryView, FileRestoreSessionView, ListPage,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tonic::Status;
use tonic::codec::EncodeBuf;
use uuid::Uuid;

use crate::codec::Binary;

/// 세션 하나에 할 일
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FileRestoreOp {
    /// 세션 보기(볼륨 포함)
    Info,
    /// 디렉터리 한 쪽
    List {
        volume: String,
        path: EntryPath,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        limit: u32,
    },
    /// 항목 하나
    Stat { volume: String, path: EntryPath },
    /// 닫기(까닭은 Task 결과에)
    Close { reason: String },
    /// 내려받기 토큰 만들기(요청한 사용자에 묶임)
    Link { link: DownloadLinkRequest },
}

/// `FileRestoreCall` 요청
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRestoreCallRequest {
    pub session: Uuid,
    /// 요청한 사용자(토큰이면 토큰 주인) — 내려받기 토큰을 묶는다
    pub user: Uuid,
    pub op: FileRestoreOp,
}

/// 세션 노드의 답
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", content = "value", rename_all = "snake_case")]
pub enum FileRestoreAnswer {
    Session(Box<FileRestoreSessionView>),
    Page(ListPage),
    Entry(EntryView),
    /// 내려받기 토큰과 남은 초
    Link {
        token: String,
        expires_in: u64,
    },
    Closed,
}

/// 하지 못한 까닭(API 오류 그대로 — 상태 코드·코드·문구)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct FileRestoreFault {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl FileRestoreFault {
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        FileRestoreFault {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    /// 그 노드에 닿지 못했다(503 `node_unreachable`)
    pub fn unreachable(why: impl std::fmt::Display) -> Self {
        Self::new(
            503,
            "node_unreachable",
            format!("the file restore session node did not answer: {why}"),
        )
    }

    /// 그 노드가 이 RPC를 모른다(409 `cluster_upgrading`)
    pub fn unsupported() -> Self {
        Self::new(
            409,
            "cluster_upgrading",
            "the file restore session node does not support file restore yet; upgrade it first",
        )
    }
}

/// `FileRestoreCall`의 답
pub type FileRestoreReply = Result<FileRestoreAnswer, FileRestoreFault>;

/// `FileRestoreDownload` 요청
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRestoreDownloadRequest {
    pub session: Uuid,
    pub token: String,
    pub user: Uuid,
}

/// 내려받기 머리: 파일 이름, 길이(`raw`만), 형식, 감사에 적을 고른 것
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadHead {
    /// `Content-Disposition`의 파일 이름
    pub name: String,
    pub content_type: String,
    /// 길이(`raw`면 에이전트가 알린 크기)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 실제 형식(`raw`·`zip`·`tar.gz`)
    pub format: String,
    /// 감사 문구(`<백업> <볼륨>:<경로…>`)
    pub describe: String,
}

/// 내려받기 흐름의 한 조각
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadPiece {
    Data(Bytes),
    /// 성공으로 끝남(넣은 항목 수 — raw는 1)
    Done {
        entries: u64,
    },
}

/// 내려받기 흐름: 조각이나 실패(문구). 끝나기 전에 닫히면 끊긴 것이다.
pub type DownloadStream = mpsc::Receiver<Result<DownloadPiece, String>>;

/// `FileRestoreDownload`의 흐름 메시지(이진: 종류 바이트 + 내용)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRestoreChunk {
    /// 첫 메시지(JSON [`DownloadHead`])
    Head(DownloadHead),
    Data(Bytes),
    /// 끝(항목 수)
    Done {
        entries: u64,
    },
    /// 시작 전 거절(JSON [`FileRestoreFault`])
    Fault(FileRestoreFault),
    /// 도중 실패(문구)
    Failed(String),
}

const TAG_HEAD: u8 = 0;
const TAG_DATA: u8 = 1;
const TAG_DONE: u8 = 2;
const TAG_FAULT: u8 = 3;
const TAG_FAILED: u8 = 4;

impl Binary for FileRestoreChunk {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        match self {
            FileRestoreChunk::Head(h) => {
                dst.put_u8(TAG_HEAD);
                dst.put_slice(&serde_json::to_vec(&h).unwrap_or_default());
            }
            FileRestoreChunk::Data(d) => {
                dst.put_u8(TAG_DATA);
                dst.put(d);
            }
            FileRestoreChunk::Done { entries } => {
                dst.put_u8(TAG_DONE);
                dst.put_u64(entries);
            }
            FileRestoreChunk::Fault(f) => {
                dst.put_u8(TAG_FAULT);
                dst.put_slice(&serde_json::to_vec(&f).unwrap_or_default());
            }
            FileRestoreChunk::Failed(m) => {
                dst.put_u8(TAG_FAILED);
                dst.put_slice(m.as_bytes());
            }
        }
    }

    fn decode(mut src: Bytes) -> Result<Self, Status> {
        if !src.has_remaining() {
            return Err(Status::internal("empty file restore chunk"));
        }
        let bad = |e: serde_json::Error| Status::internal(format!("bad file restore chunk: {e}"));
        Ok(match src.get_u8() {
            TAG_HEAD => FileRestoreChunk::Head(serde_json::from_slice(&src).map_err(bad)?),
            TAG_DATA => FileRestoreChunk::Data(src),
            TAG_DONE if src.remaining() == 8 => FileRestoreChunk::Done {
                entries: src.get_u64(),
            },
            TAG_FAULT => FileRestoreChunk::Fault(serde_json::from_slice(&src).map_err(bad)?),
            TAG_FAILED => FileRestoreChunk::Failed(String::from_utf8_lossy(&src).into_owned()),
            _ => return Err(Status::internal("unknown file restore chunk")),
        })
    }
}

/// `FileRestoreDownload` 요청은 JSON을 이진 메시지에 담는다(같은 코덱).
impl Binary for FileRestoreDownloadRequest {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        dst.put_slice(&serde_json::to_vec(&self).unwrap_or_default());
    }

    fn decode(src: Bytes) -> Result<Self, Status> {
        serde_json::from_slice(&src)
            .map_err(|e| Status::invalid_argument(format!("bad download request: {e}")))
    }
}

/// 이 노드의 파일 복원 세션(orbivisor-api의 세션 등록부)
#[async_trait]
pub trait FileRestoreSource: Send + Sync {
    async fn call(&self, req: FileRestoreCallRequest) -> FileRestoreReply;

    /// 토큰을 쓰고(한 번) 내려받기를 시작한다.
    async fn download(
        &self,
        req: FileRestoreDownloadRequest,
    ) -> Result<(DownloadHead, DownloadStream), FileRestoreFault>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(chunk: FileRestoreChunk) -> FileRestoreChunk {
        // EncodeBuf는 tonic 안에서만 만들어지므로 같은 바이트를 손으로 만든다.
        let mut buf = bytes::BytesMut::new();
        match &chunk {
            FileRestoreChunk::Head(h) => {
                buf.put_u8(TAG_HEAD);
                buf.put_slice(&serde_json::to_vec(h).unwrap());
            }
            FileRestoreChunk::Data(d) => {
                buf.put_u8(TAG_DATA);
                buf.put_slice(d);
            }
            FileRestoreChunk::Done { entries } => {
                buf.put_u8(TAG_DONE);
                buf.put_u64(*entries);
            }
            FileRestoreChunk::Fault(f) => {
                buf.put_u8(TAG_FAULT);
                buf.put_slice(&serde_json::to_vec(f).unwrap());
            }
            FileRestoreChunk::Failed(m) => {
                buf.put_u8(TAG_FAILED);
                buf.put_slice(m.as_bytes());
            }
        }
        FileRestoreChunk::decode(buf.freeze()).unwrap()
    }

    #[test]
    fn chunks_round_trip() {
        let head = DownloadHead {
            name: "web-1-etc.zip".into(),
            content_type: "application/zip".into(),
            size: None,
            format: "zip".into(),
            describe: "web-1 disk0p1:/etc".into(),
        };
        for c in [
            FileRestoreChunk::Head(head),
            FileRestoreChunk::Data(Bytes::from_static(b"PK\x03\x04")),
            FileRestoreChunk::Done { entries: 12 },
            FileRestoreChunk::Fault(FileRestoreFault::new(410, "link_expired", "gone")),
            FileRestoreChunk::Failed("io".into()),
        ] {
            assert_eq!(round_trip(c.clone()), c);
        }
        assert!(FileRestoreChunk::decode(Bytes::new()).is_err());
        assert!(FileRestoreChunk::decode(Bytes::from_static(&[9])).is_err());
        assert!(FileRestoreChunk::decode(Bytes::from_static(&[TAG_DONE, 1])).is_err());
    }

    #[test]
    fn calls_and_answers_are_json() {
        let req = FileRestoreCallRequest {
            session: Uuid::nil(),
            user: Uuid::nil(),
            op: FileRestoreOp::List {
                volume: "disk0p1".into(),
                path: EntryPath::parse("/etc").unwrap(),
                cursor: None,
                limit: 100,
            },
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["op"]["op"], "list");
        assert_eq!(json["op"]["path"], serde_json::json!(["etc"]));
        assert_eq!(
            serde_json::from_value::<FileRestoreCallRequest>(json).unwrap(),
            req
        );
        let reply: FileRestoreReply = Err(FileRestoreFault::new(404, "entry_not_found", "x"));
        let text = serde_json::to_string(&reply).unwrap();
        assert_eq!(
            serde_json::from_str::<FileRestoreReply>(&text).unwrap(),
            reply
        );
        let ok: FileRestoreReply = Ok(FileRestoreAnswer::Link {
            token: "t".into(),
            expires_in: 60,
        });
        let text = serde_json::to_string(&ok).unwrap();
        assert_eq!(serde_json::from_str::<FileRestoreReply>(&text).unwrap(), ok);
    }
}
