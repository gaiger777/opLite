//! UEFI 변수 파일 옮기기: VM을 다른 노드에서 켤 때 변수 파일을 가진 노드(`firmware_node`)에서
//! `UefiVars { vm }`로 받아 온다. 파일은 1 MiB 조각으로 보내고, 0만 있는 조각(AAVMF 변수 파일은
//! 64 MiB 대부분이 0이다)은 길이만 보낸다.

use std::io::SeekFrom;
use std::path::Path;

use bytes::{Buf, BufMut, Bytes};
use futures_util::{Stream, StreamExt};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tonic::codec::EncodeBuf;
use tonic::{Code, Status};
use uuid::Uuid;

use crate::codec::Binary;

/// 한 조각의 크기
pub const CHUNK_BYTES: usize = 1 << 20;

/// 받을 변수 파일의 최대 크기. AAVMF 변수 파일(64 MiB)보다 넉넉히 크고, 상대가 잘못된 길이를 보내도
/// 디스크를 채우지 않는다.
pub const MAX_BYTES: u64 = 256 << 20;

/// 변수 파일 요청: 어느 VM의 파일인지
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UefiVarsRequest {
    pub vm: Uuid,
}

impl Binary for UefiVarsRequest {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        dst.put_slice(self.vm.as_bytes());
    }

    fn decode(src: Bytes) -> Result<Self, Status> {
        Uuid::from_slice(&src)
            .map(|vm| Self { vm })
            .map_err(|_| Status::invalid_argument("bad VM id"))
    }
}

/// 변수 파일의 한 조각. 순서대로 이으면 파일이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UefiVarsChunk {
    Data(Bytes),
    /// 이 길이만큼 0이다.
    Zero(u64),
}

const DATA_TAG: u8 = 0;
const ZERO_TAG: u8 = 1;

impl UefiVarsChunk {
    /// 파일에서 읽은 조각. 0만 있으면 길이만 남긴다.
    pub fn of(data: &[u8]) -> Self {
        if data.iter().all(|b| *b == 0) {
            Self::Zero(data.len() as u64)
        } else {
            Self::Data(Bytes::copy_from_slice(data))
        }
    }
}

impl Binary for UefiVarsChunk {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        match self {
            Self::Data(data) => {
                dst.put_u8(DATA_TAG);
                dst.put(data);
            }
            Self::Zero(len) => {
                dst.put_u8(ZERO_TAG);
                dst.put_u64(len);
            }
        }
    }

    fn decode(mut src: Bytes) -> Result<Self, Status> {
        let bad = || Status::invalid_argument("bad UEFI variables chunk");
        if src.is_empty() {
            return Err(bad());
        }
        match src.get_u8() {
            DATA_TAG => Ok(Self::Data(src)),
            ZERO_TAG if src.len() == 8 => Ok(Self::Zero(src.get_u64())),
            _ => Err(bad()),
        }
    }
}

/// 변수 파일을 받지 못한 이유
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UefiVarsError {
    /// 그 노드에 이 VM의 변수 파일이 없다.
    #[error("the node has no UEFI variables for this VM")]
    NotFound,
    /// 그 노드에 닿지 못했거나 보내는 중에 끊겼다.
    #[error("cannot fetch the UEFI variables: {0}")]
    Unavailable(String),
    /// 받은 파일을 이 노드에 쓰지 못했다.
    #[error("cannot write the UEFI variables: {0}")]
    Io(String),
    /// 받은 파일을 쓸 수 없다(너무 크다 등).
    #[error("bad UEFI variables: {0}")]
    Invalid(String),
}

impl From<Status> for UefiVarsError {
    fn from(s: Status) -> Self {
        match s.code() {
            Code::NotFound => Self::NotFound,
            _ => Self::Unavailable(s.message().to_string()),
        }
    }
}

fn io(e: std::io::Error) -> UefiVarsError {
    UefiVarsError::Io(e.to_string())
}

/// 보내는 쪽: `file`을 끝까지 조각으로 보낸다. 받는 쪽이 끊으면 멈춘다.
pub(crate) async fn send_file(mut file: File, tx: mpsc::Sender<Result<UefiVarsChunk, Status>>) {
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        let n = match read_full(&mut file, &mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = tx
                    .send(Err(Status::internal(format!(
                        "cannot read the UEFI variables: {e}"
                    ))))
                    .await;
                break;
            }
        };
        if tx.send(Ok(UefiVarsChunk::of(&buf[..n]))).await.is_err() || n < buf.len() {
            break;
        }
    }
}

/// `buf`가 차거나 파일이 끝날 때까지 읽는다.
async fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]).await? {
            0 => break,
            m => n += m,
        }
    }
    Ok(n)
}

/// 받는 쪽: 조각을 `dest`와 같은 디렉터리의 임시 파일에 쓰고 fsync한 뒤 `dest`로 이름을 바꾼다.
/// `expect_len`을 주면 받은 파일의 크기가 그와 달라도 실패한다. 받다가 실패하면 `dest`는 그대로다.
/// 받은 파일의 크기를 돌려준다.
pub(crate) async fn write_file<S>(
    mut chunks: S,
    dest: &Path,
    expect_len: Option<u64>,
) -> Result<u64, UefiVarsError>
where
    S: Stream<Item = Result<UefiVarsChunk, Status>> + Unpin,
{
    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
        return Err(UefiVarsError::Io(format!("bad path {}", dest.display())));
    };
    tokio::fs::create_dir_all(dir).await.map_err(io)?;
    // 같은 VM을 동시에 받아도 임시 파일이 겹치지 않게 이름마다 다르게 둔다.
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        Uuid::now_v7()
    ));
    let written = async {
        let mut file = File::create_new(&tmp).await.map_err(io)?;
        let mut len = 0u64;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            let n = match &chunk {
                UefiVarsChunk::Data(data) => data.len() as u64,
                UefiVarsChunk::Zero(n) => *n,
            };
            len = len
                .checked_add(n)
                .filter(|len| *len <= MAX_BYTES)
                .ok_or_else(|| UefiVarsError::Invalid(format!("larger than {MAX_BYTES} bytes")))?;
            match chunk {
                UefiVarsChunk::Data(data) => file.write_all(&data).await.map_err(io)?,
                // 건너뛴 자리는 0으로 읽힌다(끝의 0은 아래 `set_len`이 채운다).
                UefiVarsChunk::Zero(n) => {
                    let n = i64::try_from(n).map_err(|_| {
                        UefiVarsError::Invalid(format!("larger than {MAX_BYTES} bytes"))
                    })?;
                    file.seek(SeekFrom::Current(n)).await.map_err(io)?;
                }
            }
        }
        if let Some(want) = expect_len
            && len != want
        {
            return Err(UefiVarsError::Invalid(format!(
                "{len} bytes, but this node's firmware uses {want} bytes"
            )));
        }
        // tokio `File`은 쓰기를 뒤에서 하므로, 그 오류는 flush가 돌려준다. `set_len`·`sync_all`은
        // 남은 쓰기를 기다리기만 하고 그 오류를 버린다.
        file.flush().await.map_err(io)?;
        file.set_len(len).await.map_err(io)?;
        file.sync_all().await.map_err(io)?;
        drop(file);
        tokio::fs::rename(&tmp, dest).await.map_err(io)?;
        // 이름 바꾸기가 전원이 나가도 남게 디렉터리도 fsync한다.
        File::open(dir)
            .await
            .map_err(io)?
            .sync_all()
            .await
            .map_err(io)?;
        Ok(len)
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    written
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::*;

    #[test]
    fn zero_chunks_carry_only_their_length() {
        assert_eq!(UefiVarsChunk::of(&[0u8; 4096]), UefiVarsChunk::Zero(4096));
        let mut data = vec![0u8; 4096];
        data[4095] = 7;
        assert_eq!(
            UefiVarsChunk::of(&data),
            UefiVarsChunk::Data(Bytes::from(data))
        );
    }

    #[test]
    fn chunks_decode_and_reject_garbage() {
        let mut data = BytesMut::new();
        data.put_u8(ZERO_TAG);
        data.put_u64(CHUNK_BYTES as u64);
        assert_eq!(
            UefiVarsChunk::decode(data.freeze()).unwrap(),
            UefiVarsChunk::Zero(CHUNK_BYTES as u64)
        );
        assert_eq!(
            UefiVarsChunk::decode(Bytes::from_static(b"\0abc")).unwrap(),
            UefiVarsChunk::Data(Bytes::from_static(b"abc"))
        );
        for bad in [&b""[..], b"\x01\0\0", b"\x02abc"] {
            assert!(UefiVarsChunk::decode(Bytes::copy_from_slice(bad)).is_err());
        }
        assert!(UefiVarsRequest::decode(Bytes::from_static(b"short")).is_err());
    }

    fn stream(
        items: Vec<Result<UefiVarsChunk, Status>>,
    ) -> impl Stream<Item = Result<UefiVarsChunk, Status>> + Unpin {
        futures_util::stream::iter(items)
    }

    #[tokio::test]
    async fn received_chunks_replace_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("vm").join("uefi-vars.fd");
        let chunks = vec![
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"head"))),
            Ok(UefiVarsChunk::Zero(10)),
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"mid"))),
            // 끝의 0도 파일 크기에 든다.
            Ok(UefiVarsChunk::Zero(5)),
        ];
        assert_eq!(write_file(stream(chunks), &dest, None).await.unwrap(), 22);
        let mut want = b"head".to_vec();
        want.extend([0u8; 10]);
        want.extend(b"mid");
        want.extend([0u8; 5]);
        assert_eq!(std::fs::read(&dest).unwrap(), want);

        // 있던 파일을 바꾼다.
        let chunks = vec![Ok(UefiVarsChunk::Data(Bytes::from_static(b"new")))];
        assert_eq!(write_file(stream(chunks), &dest, None).await.unwrap(), 3);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(
            std::fs::read_dir(dest.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[tokio::test]
    async fn oversized_transfer_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("uefi-vars.fd");
        std::fs::write(&dest, b"old").unwrap();
        for chunks in [
            // 합이 최대 크기를 넘는다.
            vec![
                Ok(UefiVarsChunk::Data(Bytes::from_static(b"x"))),
                Ok(UefiVarsChunk::Zero(MAX_BYTES)),
            ],
            // i64로 seek할 수 없는 길이
            vec![Ok(UefiVarsChunk::Zero(u64::MAX))],
            // 합이 u64를 넘친다.
            vec![
                Ok(UefiVarsChunk::Zero(1)),
                Ok(UefiVarsChunk::Zero(u64::MAX)),
            ],
        ] {
            let err = write_file(stream(chunks), &dest, None).await.unwrap_err();
            assert!(matches!(err, UefiVarsError::Invalid(_)), "{err:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), b"old");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
        // 최대 크기까지는 받는다.
        let chunks = vec![Ok(UefiVarsChunk::Zero(MAX_BYTES))];
        assert_eq!(
            write_file(stream(chunks), &dest, None).await.unwrap(),
            MAX_BYTES
        );
    }

    #[tokio::test]
    async fn transfer_of_another_size_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("uefi-vars.fd");
        std::fs::write(&dest, b"old").unwrap();
        let chunks = vec![Ok(UefiVarsChunk::Zero(4))];
        let err = write_file(stream(chunks), &dest, Some(3))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            UefiVarsError::Invalid("4 bytes, but this node's firmware uses 3 bytes".into())
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let chunks = vec![Ok(UefiVarsChunk::Data(Bytes::from_static(b"new")))];
        assert_eq!(write_file(stream(chunks), &dest, Some(3)).await.unwrap(), 3);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    }

    #[tokio::test]
    async fn broken_transfer_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("uefi-vars.fd");
        std::fs::write(&dest, b"old").unwrap();
        let chunks = vec![
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"partial"))),
            Err(Status::unavailable("connection reset")),
        ];
        let err = write_file(stream(chunks), &dest, None).await.unwrap_err();
        assert_eq!(err, UefiVarsError::Unavailable("connection reset".into()));
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        // 임시 파일도 남기지 않는다.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
