//! UEFI 변수 파일 옮기기: VM을 다른 노드에서 켤 때 변수 파일을 가진 노드(`firmware_node`)에서
//! `UefiVars { vm }`로 받아 온다. 파일은 1 MiB 조각으로 보내고, 0만 있는 조각(AAVMF 변수 파일은
//! 64 MiB 대부분이 0이다)은 길이만 보낸다.
//!
//! TPM 상태(계획 59 §5)도 같은 조각 스트림이다: `TpmState { vm }`는 `tpm2-00.permall` 하나를 보내고, 받는 쪽은 해시가
//! VM의 `tpm_sha256`과 같을 때만 0600으로 둔다. 펌웨어 사본은 `PushFirmware`로 vars+TPM을 한 메시지로 밀어 둔다.

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

use sha2::{Digest, Sha256};

use crate::codec::Binary;

/// 한 조각의 크기
pub const CHUNK_BYTES: usize = 1 << 20;

/// 받을 변수 파일의 최대 크기. AAVMF 변수 파일(64 MiB)보다 넉넉히 크고, 상대가 잘못된 길이를 보내도
/// 디스크를 채우지 않는다.
pub const MAX_BYTES: u64 = 256 << 20;

/// 받을 TPM 상태 파일의 최대 크기. swtpm의 `tpm2-00.permall`은 수십 KiB다.
pub const MAX_TPM_BYTES: u64 = 4 << 20;

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

/// TPM 상태 파일 요청: 어느 VM의 파일인지(답은 [`UefiVarsChunk`] 스트림)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TpmStateRequest {
    pub vm: Uuid,
}

impl Binary for TpmStateRequest {
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

/// 펌웨어 사본 밀어 두기(`PushFirmware`, 계획 59 §5): VM의 TPM 상태와 UEFI 변수를 사본 노드에 둔다. 받는 쪽은 그
/// 노드에서 VM의 QEMU가 돌거나 그 노드가 VM의 `firmware_node`면 거절한다(지금 것을 낡은 사본으로 덮지 않게).
#[derive(Clone, PartialEq, Eq)]
pub struct PushFirmwareRequest {
    pub vm: Uuid,
    /// VM의 `firmware_epoch`. 받는 쪽이 파일을 둔 뒤 적는다(켤 때 지워지지 않게).
    pub epoch: u32,
    /// VM의 `secure_boot`: 받는 쪽이 변수 파일의 크기를 어느 템플릿과 견줄지
    pub secure_boot: bool,
    /// TPM 상태 파일의 SHA-256(소문자 16진 64자)
    pub sha256: String,
    /// TPM 상태 파일(`tpm2-00.permall`)
    pub tpm: Bytes,
    /// UEFI 변수 파일의 조각(없으면 None)
    pub vars: Option<Vec<UefiVarsChunk>>,
}

/// 내용(TPM 상태)은 적지 않는다.
impl std::fmt::Debug for PushFirmwareRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushFirmwareRequest")
            .field("vm", &self.vm)
            .field("epoch", &self.epoch)
            .field("secure_boot", &self.secure_boot)
            .field("sha256", &self.sha256)
            .field("tpm_bytes", &self.tpm.len())
            .field("vars_chunks", &self.vars.as_ref().map(Vec::len))
            .finish()
    }
}

const SECURE_BOOT_FLAG: u8 = 1;
const VARS_FLAG: u8 = 2;
const SHA256_HEX: usize = 64;

impl Binary for PushFirmwareRequest {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        dst.put_slice(self.vm.as_bytes());
        dst.put_u32(self.epoch);
        let mut flags = 0;
        if self.secure_boot {
            flags |= SECURE_BOOT_FLAG;
        }
        if self.vars.is_some() {
            flags |= VARS_FLAG;
        }
        dst.put_u8(flags);
        let mut sha = [b'0'; SHA256_HEX];
        let n = self.sha256.len().min(SHA256_HEX);
        sha[..n].copy_from_slice(&self.sha256.as_bytes()[..n]);
        dst.put_slice(&sha);
        dst.put_u32(self.tpm.len() as u32);
        dst.put(self.tpm);
        if let Some(vars) = self.vars {
            dst.put_u32(vars.len() as u32);
            for chunk in vars {
                match chunk {
                    UefiVarsChunk::Data(data) => {
                        dst.put_u8(DATA_TAG);
                        dst.put_u32(data.len() as u32);
                        dst.put(data);
                    }
                    UefiVarsChunk::Zero(len) => {
                        dst.put_u8(ZERO_TAG);
                        dst.put_u64(len);
                    }
                }
            }
        }
    }

    fn decode(mut src: Bytes) -> Result<Self, Status> {
        let bad = || Status::invalid_argument("bad firmware copy");
        let need = |src: &Bytes, n: usize| if src.len() < n { Err(bad()) } else { Ok(()) };
        need(&src, 16 + 4 + 1 + SHA256_HEX + 4)?;
        let vm = Uuid::from_slice(&src.split_to(16)).map_err(|_| bad())?;
        let epoch = src.get_u32();
        let flags = src.get_u8();
        let sha256 = String::from_utf8(src.split_to(SHA256_HEX).to_vec()).map_err(|_| bad())?;
        if !sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(bad());
        }
        let n = src.get_u32() as usize;
        need(&src, n)?;
        let tpm = src.split_to(n);
        let vars = if flags & VARS_FLAG != 0 {
            need(&src, 4)?;
            let count = src.get_u32() as usize;
            let mut vars = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                need(&src, 1)?;
                match src.get_u8() {
                    DATA_TAG => {
                        need(&src, 4)?;
                        let n = src.get_u32() as usize;
                        need(&src, n)?;
                        vars.push(UefiVarsChunk::Data(src.split_to(n)));
                    }
                    ZERO_TAG => {
                        need(&src, 8)?;
                        vars.push(UefiVarsChunk::Zero(src.get_u64()));
                    }
                    _ => return Err(bad()),
                }
            }
            Some(vars)
        } else {
            None
        };
        if !src.is_empty() {
            return Err(bad());
        }
        Ok(Self {
            vm,
            epoch,
            secure_boot: flags & SECURE_BOOT_FLAG != 0,
            sha256,
            tpm,
            vars,
        })
    }
}

/// `PushFirmware`의 답(내용 없음)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PushFirmwareReply;

impl Binary for PushFirmwareReply {
    fn encode(self, _dst: &mut EncodeBuf<'_>) {}

    fn decode(_src: Bytes) -> Result<Self, Status> {
        Ok(Self)
    }
}

/// 펌웨어 파일 지우기(`PurgeFirmware`, 리뷰 1 I5): 초기화 Task가 다른 노드에 남은 초기화 전 TPM 상태·UEFI 변수(비밀)를,
/// 스냅샷 지우기 Task가 다른 노드에 둔 스냅샷의 TPM·변수 사본을 지운다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PurgeFirmwareRequest {
    pub vm: Uuid,
    /// 초기화 뒤의 회차. 있으면 이 노드의 회차가 이와 다를 때(초기화 전 파일) `uefi_vars`·`tpm`을 지운다. 이 노드에서
    /// VM이 돌면 거절한다.
    #[serde(default)]
    pub epoch: Option<u32>,
    #[serde(default)]
    pub uefi_vars: bool,
    #[serde(default)]
    pub tpm: bool,
    /// 있으면 이 스냅샷의 변수·TPM 사본(`{state}/vm/{vm}/snapshots/{id}.vars|.tpm`)을 지운다.
    #[serde(default)]
    pub snapshot: Option<Uuid>,
}

/// `PurgeFirmware`의 답(내용 없음)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PurgeFirmwareReply {}

/// 받는 쪽: [`PurgeFirmwareRequest`]대로 이 노드의 파일을 지운다(없으면 성공).
pub(crate) fn purge(state_dir: &Path, req: &PurgeFirmwareRequest) -> std::io::Result<()> {
    let gone = |r: std::io::Result<()>| match r {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    let dir = opl_driver::vm_state_dir(state_dir, req.vm);
    if let Some(snapshot) = req.snapshot {
        for ext in ["vars", "tpm"] {
            gone(std::fs::remove_file(
                dir.join("snapshots").join(format!("{snapshot}.{ext}")),
            ))?;
        }
    }
    if let Some(epoch) = req.epoch
        && opl_driver::read_firmware_epoch(state_dir, req.vm).ok() != Some(epoch)
    {
        if req.uefi_vars {
            gone(std::fs::remove_file(opl_driver::uefi_vars_path(
                state_dir, req.vm,
            )))?;
        }
        if req.tpm {
            gone(std::fs::remove_dir_all(opl_driver::vm_tpm_dir(
                state_dir, req.vm,
            )))?;
        }
    }
    Ok(())
}

/// 받는 노드의 UEFI 변수 템플릿. 밀어 둔 변수 파일의 크기가 VM에 맞는 템플릿과 다르면(다른 펌웨어 빌드) 받지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarsTemplates {
    pub uefi: std::path::PathBuf,
    /// Secure Boot 펌웨어의 변수 템플릿(MS 키 등록). 없으면 Secure Boot VM의 변수를 받지 않는다.
    pub secure_boot: Option<std::path::PathBuf>,
}

/// 파일 하나를 읽어 [`UefiVarsChunk`] 목록으로 만든다(0만 있는 조각은 길이만). 파일이 없으면 None이다.
pub async fn read_chunks(path: &Path) -> std::io::Result<Option<Vec<UefiVarsChunk>>> {
    let mut file = match File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut chunks = Vec::new();
    loop {
        let n = read_full(&mut file, &mut buf).await?;
        if n == 0 {
            break;
        }
        chunks.push(UefiVarsChunk::of(&buf[..n]));
        if n < buf.len() {
            break;
        }
    }
    Ok(Some(chunks))
}

/// 받는 쪽: 밀어 둔 사본을 이 노드의 `state_dir`에 둔다. TPM 상태는 해시가 맞을 때만, 변수 파일은 크기가 VM에 맞는
/// 템플릿과 같을 때만 두고, 둘 다 둔 뒤 펌웨어 회차를 적는다. 실패하면 이 노드의 파일은 그대로다(앞서 둔 것은 남는다).
pub(crate) async fn receive_push(
    state_dir: &Path,
    templates: &VarsTemplates,
    req: PushFirmwareRequest,
) -> Result<(), Status> {
    let invalid = |e: UefiVarsError| match e {
        UefiVarsError::Io(m) => Status::internal(format!("cannot write the firmware copy: {m}")),
        e => Status::invalid_argument(e.to_string()),
    };
    let tpm_len = req.tpm.len() as u64;
    if tpm_len > MAX_TPM_BYTES {
        return Err(Status::invalid_argument(format!(
            "the TPM state is larger than {MAX_TPM_BYTES} bytes"
        )));
    }
    if sha256_hex(&req.tpm) != req.sha256 {
        return Err(Status::invalid_argument(
            "the TPM state does not match its SHA-256",
        ));
    }
    let has_vars = req.vars.is_some();
    if let Some(vars) = req.vars {
        let template = if req.secure_boot {
            templates.secure_boot.as_deref().ok_or_else(|| {
                Status::failed_precondition("this node has no Secure Boot firmware")
            })?
        } else {
            &templates.uefi
        };
        let len = tokio::fs::metadata(template)
            .await
            .map_err(|e| {
                Status::internal(format!(
                    "cannot read the UEFI variables template {}: {e}",
                    template.display()
                ))
            })?
            .len();
        let chunks = futures_util::stream::iter(vars.into_iter().map(Ok));
        write_file(
            chunks,
            &opl_driver::uefi_vars_path(state_dir, req.vm),
            Some(len),
        )
        .await
        .map_err(invalid)?;
    }
    let chunks = futures_util::stream::iter([Ok(UefiVarsChunk::Data(req.tpm))]);
    write_checked(
        chunks,
        &opl_driver::tpm_state_path(state_dir, req.vm),
        Expect {
            sha256: Some(&req.sha256),
            max: Some(MAX_TPM_BYTES),
            private: true,
            ..Default::default()
        },
    )
    .await
    .map_err(invalid)?;
    // 회차가 바뀌면 받지 않은 낡은 변수를 지운다(리뷰 1 C2).
    let keep = opl_driver::KeepFirmware {
        vars: has_vars,
        tpm: true,
    };
    opl_driver::adopt_firmware_epoch(state_dir, req.vm, req.epoch, keep)
        .map_err(|e| Status::internal(format!("cannot record the firmware epoch: {e}")))
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

/// TPM 상태를 받거나 사본을 밀어 두지 못한 이유
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TpmStateError {
    /// 그 노드에 이 VM의 TPM 상태가 없다.
    #[error("the node has no TPM state for this VM")]
    NotFound,
    /// 그 노드가 이 RPC를 모른다(옛 노드).
    #[error("the node does not support TPM state transfers")]
    Unsupported,
    /// 그 노드에 닿지 못했거나 보내는 중에 끊겼다.
    #[error("cannot transfer the TPM state: {0}")]
    Unavailable(String),
    /// 받은 파일을 이 노드에 쓰지 못했다.
    #[error("cannot write the TPM state: {0}")]
    Io(String),
    /// 받은 것을 쓸 수 없다(해시가 다르다, 너무 크다, 받는 쪽이 거절했다 등).
    #[error("bad TPM state: {0}")]
    Invalid(String),
}

impl From<Status> for TpmStateError {
    fn from(s: Status) -> Self {
        match s.code() {
            Code::NotFound => Self::NotFound,
            Code::Unimplemented => Self::Unsupported,
            Code::InvalidArgument | Code::FailedPrecondition => {
                Self::Invalid(s.message().to_string())
            }
            _ => Self::Unavailable(s.message().to_string()),
        }
    }
}

impl From<UefiVarsError> for TpmStateError {
    fn from(e: UefiVarsError) -> Self {
        match e {
            UefiVarsError::NotFound => Self::NotFound,
            UefiVarsError::Unavailable(m) => Self::Unavailable(m),
            UefiVarsError::Io(m) => Self::Io(m),
            UefiVarsError::Invalid(m) => Self::Invalid(m),
        }
    }
}

/// 바이트들의 SHA-256(소문자 16진)
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// 받은 파일을 두기 전에 볼 것
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Expect<'a> {
    /// 크기가 이와 달라야 하면 쓰지 않는다(이 노드의 펌웨어 템플릿 크기).
    pub len: Option<u64>,
    /// SHA-256(소문자 16진)이 이와 달라야 하면 쓰지 않는다.
    pub sha256: Option<&'a str>,
    /// 최대 크기(없으면 [`MAX_BYTES`])
    pub max: Option<u64>,
    /// 비밀 파일(TPM 상태): 디렉터리 0700, 파일 0600
    pub private: bool,
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
    chunks: S,
    dest: &Path,
    expect_len: Option<u64>,
) -> Result<u64, UefiVarsError>
where
    S: Stream<Item = Result<UefiVarsChunk, Status>> + Unpin,
{
    write_checked(
        chunks,
        dest,
        Expect {
            len: expect_len,
            ..Default::default()
        },
    )
    .await
}

/// [`write_file`]과 같되 `expect`의 크기·해시가 맞을 때만 `dest`를 바꾼다.
pub(crate) async fn write_checked<S>(
    mut chunks: S,
    dest: &Path,
    expect: Expect<'_>,
) -> Result<u64, UefiVarsError>
where
    S: Stream<Item = Result<UefiVarsChunk, Status>> + Unpin,
{
    use std::os::unix::fs::PermissionsExt;

    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
        return Err(UefiVarsError::Io(format!("bad path {}", dest.display())));
    };
    tokio::fs::create_dir_all(dir).await.map_err(io)?;
    if expect.private {
        tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(io)?;
    }
    let max = expect.max.unwrap_or(MAX_BYTES);
    let mut hasher = expect.sha256.map(|_| Sha256::new());
    // 같은 VM을 동시에 받아도 임시 파일이 겹치지 않게 이름마다 다르게 둔다.
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        Uuid::now_v7()
    ));
    let written = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if expect.private { 0o600 } else { 0o644 })
            .open(&tmp)
            .await
            .map_err(io)?;
        let mut len = 0u64;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            let n = match &chunk {
                UefiVarsChunk::Data(data) => data.len() as u64,
                UefiVarsChunk::Zero(n) => *n,
            };
            len = len
                .checked_add(n)
                .filter(|len| *len <= max)
                .ok_or_else(|| UefiVarsError::Invalid(format!("larger than {max} bytes")))?;
            match chunk {
                UefiVarsChunk::Data(data) => {
                    if let Some(h) = &mut hasher {
                        h.update(&data);
                    }
                    file.write_all(&data).await.map_err(io)?
                }
                // 건너뛴 자리는 0으로 읽힌다(끝의 0은 아래 `set_len`이 채운다).
                UefiVarsChunk::Zero(n) => {
                    if let Some(h) = &mut hasher {
                        let zeros = [0u8; 4096];
                        let mut left = n;
                        while left > 0 {
                            let k = left.min(zeros.len() as u64) as usize;
                            h.update(&zeros[..k]);
                            left -= k as u64;
                        }
                    }
                    let n = i64::try_from(n)
                        .map_err(|_| UefiVarsError::Invalid(format!("larger than {max} bytes")))?;
                    file.seek(SeekFrom::Current(n)).await.map_err(io)?;
                }
            }
        }
        if let Some(want) = expect.len
            && len != want
        {
            return Err(UefiVarsError::Invalid(format!(
                "{len} bytes, but this node's firmware uses {want} bytes"
            )));
        }
        if let (Some(h), Some(want)) = (hasher.take(), expect.sha256) {
            let got = hex::encode(h.finalize());
            if got != want {
                return Err(UefiVarsError::Invalid(format!(
                    "SHA-256 {got} differs from the recorded {want}"
                )));
            }
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
