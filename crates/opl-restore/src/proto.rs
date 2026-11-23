//! opld ↔ 도우미 에이전트 통신 규약(판 1, 설계 §9.2). virtio-serial 위 바이트 흐름에 틀을 붙인다:
//! `u32 길이(BE, 머리 9바이트 포함)` · `u32 흐름 번호(BE)` · `u8 종류` · 내용. `Req`·`Resp`·`Err`의 내용은 JSON,
//! `Data`는 날 바이트, `End`는 비어 있다. 내용은 1 MiB를 넘지 않는다.
//!
//! 흐름: opld가 요청마다 새 흐름 번호(1 이상)로 `Req` 하나를 보내고, 에이전트는 그 흐름에 `Resp`·`Data`를 0개 이상
//! 보낸 뒤 `End`(성공)나 `Err`(실패)로 끝낸다. 흐름 0은 에이전트가 처음 보내는 `Hello`만 쓴다.
//!
//! 에이전트가 보낸 것은 백업 안 파일시스템(믿을 수 없는 입력)을 해석한 결과다. opld는 [`StreamCheck`]로 흐름마다
//! 차례·크기·한도를 검사하고, 어긋나면 세션을 닫는다(`agent protocol violation: …`).

use std::fmt;
use std::io::{self, Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// 통신 규약 판. 커널 명령줄 `opl.proto=`·`Hello.proto`·이미지 `manifest.json`이 같아야 한다.
pub const PROTO_VERSION: u32 = 1;
/// 도우미 VM의 virtio-serial 포트 이름
pub const PORT_NAME: &str = "org.orbivisor.restore.0";

/// 틀 머리 길이(길이 4 + 흐름 4 + 종류 1)
pub const HEADER_LEN: usize = 9;
/// 틀 내용 상한(JSON·데이터 모두)
pub const MAX_PAYLOAD: usize = 1 << 20;
/// 틀 전체 상한
pub const MAX_FRAME: usize = HEADER_LEN + MAX_PAYLOAD;

/// 경로 조각(파일 이름) 하나의 최대 길이(바이트). `opl_types::file_restore::MAX_NAME_BYTES`와 같다.
pub const MAX_NAME_BYTES: usize = 1024;
/// 경로 깊이 상한. `opl_types::file_restore::MAX_PATH_DEPTH`와 같다.
pub const MAX_PATH_DEPTH: usize = 256;
/// 심볼릭 링크 대상 길이 상한. `opl_types::file_restore::MAX_LINK_TARGET`과 같다.
pub const MAX_LINK_TARGET: usize = 4096;
/// 세션 하나의 볼륨 수 상한. `opl_types::file_restore::MAX_VOLUMES`와 같다.
pub const MAX_VOLUMES: usize = 256;
/// 나열 한 쪽의 항목 수 상한. `opl_types::file_restore::MAX_PAGE_ENTRIES`와 같다.
pub const MAX_PAGE_ENTRIES: usize = 1000;
/// 볼륨의 글자 칸(파일시스템 종류·레이블·UUID·파티션 종류·덧붙임)과 `Hello`·오류 문구의 길이 상한(바이트).
/// 에이전트는 [`MAX_LABEL`]에서 자르고, opld는 이것을 넘으면 위반으로 본다.
pub const MAX_TEXT: usize = 1024;
/// 에이전트가 볼륨 글자 칸을 자르는 길이(바이트)
pub const MAX_LABEL: usize = 256;
/// 한 번에 붙여 보내는 요청 경로 수 상한(걷기)
pub const MAX_WALK_PATHS: usize = 1000;

// ---------------------------------------------------------------------------------------------------------------
// 틀
// ---------------------------------------------------------------------------------------------------------------

/// 틀 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrameKind {
    /// 요청(opld → 에이전트, JSON [`Request`])
    Req = 1,
    /// 답(에이전트 → opld, JSON [`Reply`])
    Resp = 2,
    /// 파일 데이터(날 바이트)
    Data = 3,
    /// 흐름 끝(성공, 내용 없음)
    End = 4,
    /// 흐름 끝(실패, JSON [`AgentError`])
    Err = 5,
}

impl FrameKind {
    pub fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            1 => FrameKind::Req,
            2 => FrameKind::Resp,
            3 => FrameKind::Data,
            4 => FrameKind::End,
            5 => FrameKind::Err,
            _ => return None,
        })
    }
}

/// 틀 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub stream: u32,
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

/// 틀이 잘못됐다(세션을 닫는다).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameError {
    #[error("frame length {0} is shorter than the header")]
    Short(u32),
    #[error("frame length {0} is over the limit of {MAX_FRAME} bytes")]
    Long(u32),
    #[error("unknown frame kind {0}")]
    Kind(u8),
    #[error("an end frame carries {0} bytes")]
    EndPayload(usize),
    #[error("the stream ended inside a frame")]
    Truncated,
}

impl Frame {
    pub fn new(stream: u32, kind: FrameKind, payload: Vec<u8>) -> Self {
        Frame {
            stream,
            kind,
            payload,
        }
    }

    /// JSON 내용 틀
    pub fn json<T: Serialize>(stream: u32, kind: FrameKind, value: &T) -> Self {
        // 우리 타입의 직렬화는 실패하지 않는다(맵 열쇠가 모두 문자열).
        let payload = serde_json::to_vec(value).expect("protocol message serializes");
        Frame::new(stream, kind, payload)
    }

    pub fn end(stream: u32) -> Self {
        Frame::new(stream, FrameKind::End, Vec::new())
    }

    /// 머리를 붙인 바이트(내용이 [`MAX_PAYLOAD`]를 넘으면 부르는 쪽 잘못 — 보내기 전에 나눈다)
    pub fn encode(&self) -> Vec<u8> {
        assert!(self.payload.len() <= MAX_PAYLOAD, "frame payload too large");
        let len = (HEADER_LEN + self.payload.len()) as u32;
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&self.stream.to_be_bytes());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.payload);
        out
    }

    /// JSON 내용 풀기
    pub fn parse<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.payload)
    }
}

/// 머리 검사: (길이, 흐름, 종류)
fn parse_header(h: &[u8; HEADER_LEN]) -> Result<(usize, u32, FrameKind), FrameError> {
    let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
    let stream = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
    if (len as usize) < HEADER_LEN {
        return Err(FrameError::Short(len));
    }
    if len as usize > MAX_FRAME {
        return Err(FrameError::Long(len));
    }
    let kind = FrameKind::from_u8(h[8]).ok_or(FrameError::Kind(h[8]))?;
    let payload = len as usize - HEADER_LEN;
    if kind == FrameKind::End && payload != 0 {
        return Err(FrameError::EndPayload(payload));
    }
    Ok((payload, stream, kind))
}

/// 밀어 넣은 바이트에서 틀을 꺼내는 해석기(입출력 없음). 머리가 잘못되면 그 뒤로는 늘 같은 오류다.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    start: usize,
    failed: Option<FrameError>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 받은 바이트를 붙인다.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// 아직 틀이 되지 않은 바이트 수
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }

    /// 다음 틀. 바이트가 모자라면 `None`.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        let rest = &self.buf[self.start..];
        let Some(header) = rest.first_chunk::<HEADER_LEN>() else {
            return Ok(None);
        };
        let (len, stream, kind) = match parse_header(header) {
            Ok(h) => h,
            Err(e) => {
                self.failed = Some(e.clone());
                return Err(e);
            }
        };
        if rest.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let payload = rest[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.start += HEADER_LEN + len;
        // 다 쓴 앞부분이 커지면 당겨 온다(버퍼가 끝없이 자라지 않게).
        if self.start >= MAX_FRAME {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        Ok(Some(Frame::new(stream, kind, payload)))
    }

    /// 흐름이 끝났을 때: 틀 중간에서 끝났으면 오류
    pub fn finish(&self) -> Result<(), FrameError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        if self.pending() > 0 {
            return Err(FrameError::Truncated);
        }
        Ok(())
    }
}

/// 틀 읽기 오류
#[derive(Debug, Error)]
pub enum ReadError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Frame(#[from] FrameError),
}

/// 막히는 읽기에서 틀 하나(에이전트 쪽). 틀 경계에서 끝나면 `None`. 길이는 할당하기 전에 검사한다.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Option<Frame>, ReadError> {
    let mut header = [0u8; HEADER_LEN];
    let mut got = 0;
    while got < HEADER_LEN {
        match r.read(&mut header[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let (len, stream, kind) = parse_header(&header)?;
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            ReadError::Frame(FrameError::Truncated)
        } else {
            ReadError::Io(e)
        }
    })?;
    Ok(Some(Frame::new(stream, kind, payload)))
}

/// 틀 하나를 한 번에 쓴다(여러 스레드가 쓰는 쪽은 잠금 안에서 부른다).
pub fn write_frame<W: Write>(w: &mut W, frame: &Frame) -> io::Result<()> {
    w.write_all(&frame.encode())
}

// ---------------------------------------------------------------------------------------------------------------
// 이름·바이트
// ---------------------------------------------------------------------------------------------------------------

/// JSON의 바이트열: UTF-8이면 문자열, 아니면 `{ "b64": … }`(표준 base64). `opl_types::file_restore::PathPart`와
/// 같은 꼴이다.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RawBytes {
    Text(String),
    Raw(B64),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct B64 {
    b64: String,
}

fn ser_bytes<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
    match std::str::from_utf8(b) {
        Ok(t) => RawBytes::Text(t.to_string()).serialize(s),
        Err(_) => RawBytes::Raw(B64 {
            b64: STANDARD.encode(b),
        })
        .serialize(s),
    }
}

fn de_bytes<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    match RawBytes::deserialize(d)? {
        RawBytes::Text(t) => Ok(t.into_bytes()),
        RawBytes::Raw(B64 { b64 }) => STANDARD
            .decode(b64)
            .map_err(|_| serde::de::Error::custom("invalid base64")),
    }
}

/// 이름이 잘못됐다.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NameError {
    #[error("empty name")]
    Empty,
    #[error("name \".\" or \"..\"")]
    Dot,
    #[error("name contains '/' or NUL")]
    Separator,
    #[error("name is longer than {MAX_NAME_BYTES} bytes")]
    Long,
}

/// 파일 이름 하나(검사한 바이트열 — 비지 않음, `.`·`..` 아님, `/`·NUL 없음, [`MAX_NAME_BYTES`] 안). JSON으로 풀 때도
/// 검사하므로 에이전트가 보낸 이름은 이 타입이 된 순간 안전한 경로 조각이다.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(Vec<u8>);

impl Name {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, NameError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(NameError::Empty);
        }
        if bytes == b"." || bytes == b".." {
            return Err(NameError::Dot);
        }
        if bytes.iter().any(|b| *b == b'/' || *b == 0) {
            return Err(NameError::Separator);
        }
        if bytes.len() > MAX_NAME_BYTES {
            return Err(NameError::Long);
        }
        Ok(Name(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// 보이기용(UTF-8이 아닌 바이트는 U+FFFD)
    pub fn lossy(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

impl AsRef<[u8]> for Name {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.lossy())
    }
}

impl Serialize for Name {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        ser_bytes(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Name::new(de_bytes(d)?).map_err(serde::de::Error::custom)
    }
}

/// 경로 보이기 `/a/b`(로그·보고서용, 루트는 `/`)
pub fn display_path(path: &[Name]) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    path.iter().fold(String::new(), |mut s, p| {
        s.push('/');
        s.push_str(&p.lossy());
        s
    })
}

/// 심볼릭 링크 대상(바이트열, `/` 들어감). JSON은 [`Name`]과 같은 꼴.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkTarget(Vec<u8>);

impl LinkTarget {
    /// 비지 않고 NUL이 없으며 [`MAX_LINK_TARGET`] 안
    pub fn new(bytes: impl Into<Vec<u8>>) -> Option<Self> {
        let bytes = bytes.into();
        (!bytes.is_empty() && bytes.len() <= MAX_LINK_TARGET && !bytes.contains(&0))
            .then_some(LinkTarget(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for LinkTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(&self.0))
    }
}

impl Serialize for LinkTarget {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        ser_bytes(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for LinkTarget {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        LinkTarget::new(de_bytes(d)?).ok_or_else(|| {
            serde::de::Error::custom("link target is empty, contains NUL or is too long")
        })
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 메시지
// ---------------------------------------------------------------------------------------------------------------

/// opld → 에이전트 요청. 경로는 볼륨 루트에서 시작하는 이름 목록(빈 목록은 루트).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 디스크·파티션·LVM 볼륨 찾기 → [`Reply::Volumes`]
    Probe,
    /// 볼륨 마운트(이미 붙었으면 그대로) → [`Reply::Mounted`]
    Mount { volume: String },
    /// 디렉터리 한 쪽(이름의 바이트 차례, `cursor` 다음부터) → [`Reply::Page`]
    List {
        volume: String,
        path: Vec<Name>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<Name>,
        limit: u32,
    },
    /// 항목 하나(심볼릭 링크는 따라가지 않음) → [`Reply::Entry`]
    Stat { volume: String, path: Vec<Name> },
    /// 일반 파일 하나 → [`Reply::FileHead`] + 데이터
    Read { volume: String, path: Vec<Name> },
    /// 고른 항목들을 깊이 우선으로 → [`Reply::Item`]·데이터·[`Reply::FileError`]·[`Reply::Skipped`]·[`Reply::Progress`].
    /// 아카이브 안 경로는 고른 항목의 이름에서 시작한다(루트를 고르면 그 아래 항목들). `hardlinks`이면 같은 inode의
    /// 둘째 파일부터 데이터 없이 [`Item::hardlink`]로 알린다(tar), 아니면 내용을 다시 보낸다(zip).
    Walk {
        volume: String,
        paths: Vec<Vec<Name>>,
        #[serde(default)]
        hardlinks: bool,
    },
    /// 흐름 `stream`을 멈춤(그 흐름은 `Err cancelled`로 끝남) → `End`
    Cancel { stream: u32 },
    /// 끝냄(`End` 뒤 에이전트는 전원을 끈다)
    Shutdown,
}

/// 항목 종류(장치·FIFO·소켓은 `other`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// 항목 속성
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attrs {
    pub kind: EntryKind,
    /// 바이트(일반 파일만 뜻이 있음)
    pub size: u64,
    /// 수정 시각(유닉스 초)
    pub mtime: i64,
    /// 수정 시각의 나노초(0..10⁹)
    #[serde(default)]
    pub mtime_nsec: u32,
    /// 권한 비트(`st_mode & 0o7777`)
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// 하드 링크 수(걷기의 tar 하드 링크 판단)
    #[serde(default)]
    pub nlink: u32,
}

/// 디렉터리 항목(나열·`Stat`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: Name,
    pub attrs: Attrs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<LinkTarget>,
}

/// 걷기 항목 하나. 일반 파일(하드 링크 아님)이면 뒤에 `attrs.size` 바이트의 데이터가 온다(읽기 오류면 그보다 적은
/// 데이터 뒤 [`Reply::FileError`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// 아카이브 안 경로(고른 항목의 이름부터)
    pub path: Vec<Name>,
    pub attrs: Attrs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<LinkTarget>,
    /// 같은 내용의 앞선 파일(아카이브 안 경로). 있으면 데이터가 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardlink: Option<Vec<Name>>,
}

impl Item {
    /// 뒤따를 데이터 바이트
    pub fn data_len(&self) -> u64 {
        if self.attrs.kind == EntryKind::File && self.hardlink.is_none() {
            self.attrs.size
        } else {
            0
        }
    }
}

/// 파티션 표 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionTable {
    Mbr,
    Gpt,
}

/// 볼륨 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    Disk,
    Partition,
    LogicalVolume,
}

/// 파티션 정보
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partition {
    pub table: PartitionTable,
    pub number: u32,
    pub type_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

/// 에이전트가 찾은 볼륨(`opl_types::file_restore::VolumeView`에서 마운트 상태를 뺀 것 — 상태는 opld가 든다)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub id: String,
    pub disk: u32,
    pub kind: VolumeKind,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<Partition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub mountable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 에이전트 첫 인사
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub proto: u32,
    pub image_version: String,
    pub kernel: String,
}

/// 에이전트 → opld 답(`Resp` 틀)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Hello(Hello),
    Volumes {
        volumes: Vec<Volume>,
    },
    Mounted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    Page {
        entries: Vec<Entry>,
        /// 다음 쪽 커서(이 쪽 마지막 이름). 없으면 마지막 쪽
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<Name>,
    },
    Entry(Entry),
    /// 읽기의 머리: 뒤에 `size` 바이트가 온다
    FileHead {
        size: u64,
    },
    Item(Item),
    /// 지금 파일의 데이터가 끝까지 오지 못했다(읽기 오류). 그 파일만 실패
    FileError {
        message: String,
    },
    /// 건너뛴 항목(읽을 수 없는 디렉터리, 너무 깊은 경로 …)
    Skipped {
        path: Vec<Name>,
        reason: String,
    },
    /// 걷기 중 살아 있음(30초마다)
    Progress {
        entries: u64,
        bytes: u64,
    },
}

/// 실패 까닭 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 볼륨·항목이 없다
    NotFound,
    /// 디렉터리가 아니다
    NotDir,
    /// 일반 파일이 아니다
    NotFile,
    /// 경로 조각이 심볼릭 링크다(따라가지 않는다)
    Symlink,
    /// 마운트할 수 없는 볼륨(LVM PV·암호화·모르는 것)
    NotMountable,
    /// 마운트 실패
    MountFailed,
    /// 읽기 오류
    Io,
    /// 요청이 잘못됐다
    BadRequest,
    /// 동시 흐름이 너무 많다
    Busy,
    /// `Cancel`로 멈췄다
    Cancelled,
}

/// 흐름 실패(`Err` 틀)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{message}")]
pub struct AgentError {
    pub code: ErrorCode,
    pub message: String,
}

impl AgentError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AgentError {
            code,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// opld 쪽 검사
// ---------------------------------------------------------------------------------------------------------------

/// 에이전트가 규약을 어겼다(세션을 닫는다).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("agent protocol violation: {0}")]
pub struct Violation(pub String);

fn violation(msg: impl Into<String>) -> Violation {
    Violation(msg.into())
}

fn check_text(what: &str, s: &str) -> Result<(), Violation> {
    if s.len() > MAX_TEXT {
        return Err(violation(format!("{what} is longer than {MAX_TEXT} bytes")));
    }
    Ok(())
}

fn check_opt_text(what: &str, s: &Option<String>) -> Result<(), Violation> {
    s.as_deref().map_or(Ok(()), |s| check_text(what, s))
}

fn check_path(path: &[Name]) -> Result<(), Violation> {
    if path.len() > MAX_PATH_DEPTH {
        return Err(violation(format!(
            "path is deeper than {MAX_PATH_DEPTH} components"
        )));
    }
    Ok(())
}

fn check_attrs(a: &Attrs) -> Result<(), Violation> {
    if a.mode > 0o7777 {
        return Err(violation("mode has bits outside 0o7777"));
    }
    if a.mtime_nsec >= 1_000_000_000 {
        return Err(violation("mtime nanoseconds out of range"));
    }
    Ok(())
}

fn check_entry(e: &Entry) -> Result<(), Violation> {
    check_attrs(&e.attrs)?;
    if e.link_target.is_some() != (e.attrs.kind == EntryKind::Symlink) {
        return Err(violation(
            "link target on a non-symlink or symlink without one",
        ));
    }
    Ok(())
}

/// LVM 이름 규칙(`opl_types::file_restore`와 같음)
fn lvm_name_ok(s: &str) -> bool {
    (1..=127).contains(&s.len())
        && !s.starts_with('-')
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'_' | b'.' | b'-'))
}

fn canonical_u32(s: &str) -> Option<u32> {
    s.parse::<u32>().ok().filter(|n| n.to_string() == s)
}

/// 볼륨 id 꼴: `disk{i}`·`disk{i}p{n}`·`lvm/{vg}/{lv}`(`opl_types::file_restore::valid_volume_id`와 같음)
pub fn valid_volume_id(id: &str) -> bool {
    if let Some(rest) = id.strip_prefix("lvm/") {
        return rest
            .split_once('/')
            .is_some_and(|(vg, lv)| lvm_name_ok(vg) && lvm_name_ok(lv));
    }
    let Some(rest) = id.strip_prefix("disk") else {
        return false;
    };
    match rest.split_once('p') {
        Some((disk, part)) => canonical_u32(disk).is_some() && canonical_u32(part).is_some(),
        None => canonical_u32(rest).is_some(),
    }
}

/// 볼륨 id `disk{i}`
pub fn disk_volume_id(disk: u32) -> String {
    format!("disk{disk}")
}

/// 볼륨 id `disk{i}p{n}`
pub fn partition_volume_id(disk: u32, number: u32) -> String {
    format!("disk{disk}p{number}")
}

/// 볼륨 id `lvm/{vg}/{lv}`, 이름이 규칙에 어긋나면 UUID로, UUID도 어긋나면 없음
pub fn lv_volume_id(vg: &str, lv: &str, vg_uuid: &str, lv_uuid: &str) -> Option<String> {
    if lvm_name_ok(vg) && lvm_name_ok(lv) {
        Some(format!("lvm/{vg}/{lv}"))
    } else if lvm_name_ok(vg_uuid) && lvm_name_ok(lv_uuid) {
        Some(format!("lvm/{vg_uuid}/{lv_uuid}"))
    } else {
        None
    }
}

/// 볼륨 목록 검사(수·id 꼴·겹침·글자 길이)
pub fn check_volumes(volumes: &[Volume]) -> Result<(), Violation> {
    if volumes.len() > MAX_VOLUMES {
        return Err(violation(format!("more than {MAX_VOLUMES} volumes")));
    }
    let mut seen = std::collections::HashSet::new();
    for v in volumes {
        if !valid_volume_id(&v.id) {
            return Err(violation("invalid volume id"));
        }
        if !seen.insert(v.id.as_str()) {
            return Err(violation("duplicate volume id"));
        }
        check_opt_text("filesystem type", &v.fs)?;
        check_opt_text("label", &v.label)?;
        check_opt_text("uuid", &v.uuid)?;
        check_opt_text("note", &v.note)?;
        if let Some(p) = &v.partition {
            check_text("partition type", &p.type_name)?;
            check_opt_text("partition uuid", &p.uuid)?;
        }
    }
    Ok(())
}

/// `Hello` 검사(판이 같은지는 부르는 쪽이 이미지 판과 함께 본다)
pub fn check_hello(h: &Hello) -> Result<(), Violation> {
    check_text("image version", &h.image_version)?;
    check_text("kernel", &h.kernel)?;
    if h.proto != PROTO_VERSION {
        return Err(violation(format!(
            "agent speaks protocol {} (expected {PROTO_VERSION})",
            h.proto
        )));
    }
    Ok(())
}

/// 흐름 하나에서 opld가 받은 것
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Reply(Reply),
    Data(Vec<u8>),
    /// 성공으로 끝남
    End,
    /// 실패로 끝남(그 요청만)
    Failed(AgentError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Probe,
    Mount,
    List(usize),
    Stat,
    Read,
    Walk,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// 첫 답을 기다림
    Start,
    /// 답을 받았고 `End`만 남음
    Answered,
    /// 데이터 `remaining` 바이트를 기다림(읽기·걷기 파일)
    Data {
        remaining: u64,
    },
    /// 걷기: 다음 항목이나 끝
    Walking,
    Done,
}

/// opld 쪽 흐름 검사기: 보낸 요청에 맞는 답만, 정한 차례로, 한도 안에서 받는다.
#[derive(Debug, Clone)]
pub struct StreamCheck {
    op: Op,
    state: State,
    max_entries: u64,
    entries: u64,
}

impl StreamCheck {
    /// `max_entries`는 걷기 항목 상한(`download_max_entries`)
    pub fn new(req: &Request, max_entries: u64) -> Self {
        let op = match req {
            Request::Probe => Op::Probe,
            Request::Mount { .. } => Op::Mount,
            Request::List { limit, .. } => Op::List((*limit as usize).clamp(1, MAX_PAGE_ENTRIES)),
            Request::Stat { .. } => Op::Stat,
            Request::Read { .. } => Op::Read,
            Request::Walk { .. } => Op::Walk,
            Request::Cancel { .. } | Request::Shutdown => Op::Control,
        };
        StreamCheck {
            op,
            state: State::Start,
            max_entries,
            entries: 0,
        }
    }

    pub fn done(&self) -> bool {
        self.state == State::Done
    }

    /// 걷기에서 지금까지 받은 항목 수
    pub fn entries(&self) -> u64 {
        self.entries
    }

    /// 이 흐름의 틀 하나를 검사해 사건으로 바꾼다.
    pub fn feed(&mut self, frame: Frame) -> Result<Event, Violation> {
        if self.state == State::Done {
            return Err(violation("frame after the end of the stream"));
        }
        match frame.kind {
            FrameKind::Req => Err(violation("the agent sent a request frame")),
            FrameKind::Err => {
                let err: AgentError = frame
                    .parse()
                    .map_err(|e| violation(format!("bad error frame: {e}")))?;
                check_text("error message", &err.message)?;
                self.state = State::Done;
                Ok(Event::Failed(err))
            }
            FrameKind::End => {
                let complete = match self.state {
                    State::Answered | State::Walking => true,
                    // 걷기는 항목이 하나도 없을 수 있다(빈 디렉터리의 루트).
                    State::Start => matches!(self.op, Op::Control | Op::Walk),
                    State::Data { .. } | State::Done => false,
                };
                if !complete {
                    return Err(violation("the stream ended early"));
                }
                self.state = State::Done;
                Ok(Event::End)
            }
            FrameKind::Data => {
                let State::Data { remaining } = self.state else {
                    return Err(violation("unexpected data frame"));
                };
                let n = frame.payload.len() as u64;
                if n == 0 || n > remaining {
                    return Err(violation("more data than the declared size"));
                }
                let remaining = remaining - n;
                self.state = match (remaining, self.op) {
                    (0, Op::Walk) => State::Walking,
                    (0, _) => State::Answered,
                    (r, _) => State::Data { remaining: r },
                };
                Ok(Event::Data(frame.payload))
            }
            FrameKind::Resp => {
                let reply: Reply = frame
                    .parse()
                    .map_err(|e| violation(format!("bad reply: {e}")))?;
                self.reply(&reply)?;
                Ok(Event::Reply(reply))
            }
        }
    }

    fn reply(&mut self, reply: &Reply) -> Result<(), Violation> {
        let unexpected = || violation("unexpected reply for this request");
        match (self.op, self.state, reply) {
            (Op::Probe, State::Start, Reply::Volumes { volumes }) => {
                check_volumes(volumes)?;
                self.state = State::Answered;
            }
            (Op::Mount, State::Start, Reply::Mounted { note }) => {
                check_opt_text("note", note)?;
                self.state = State::Answered;
            }
            (Op::List(limit), State::Start, Reply::Page { entries, next }) => {
                if entries.len() > limit {
                    return Err(violation("more entries than the page limit"));
                }
                let mut prev: Option<&Name> = None;
                for e in entries {
                    check_entry(e)?;
                    // 쪽 안 이름은 바이트 차례로 늘어난다(겹침 없음 — 커서가 앞으로만 감).
                    if prev.is_some_and(|p| p >= &e.name) {
                        return Err(violation("page entries are not sorted"));
                    }
                    prev = Some(&e.name);
                }
                if next.is_some() && next.as_ref() != prev {
                    return Err(violation("page cursor is not the last name"));
                }
                self.state = State::Answered;
            }
            (Op::Stat, State::Start, Reply::Entry(e)) => {
                check_entry(e)?;
                self.state = State::Answered;
            }
            (Op::Read, State::Start, Reply::FileHead { size }) => {
                self.state = if *size == 0 {
                    State::Answered
                } else {
                    State::Data { remaining: *size }
                };
            }
            (Op::Walk, State::Start | State::Walking, Reply::Item(item)) => {
                check_attrs(&item.attrs)?;
                if item.path.is_empty() {
                    return Err(violation("walk item without a path"));
                }
                check_path(&item.path)?;
                if item.link_target.is_some() != (item.attrs.kind == EntryKind::Symlink) {
                    return Err(violation(
                        "link target on a non-symlink or symlink without one",
                    ));
                }
                if let Some(h) = &item.hardlink {
                    if item.attrs.kind != EntryKind::File || h.is_empty() {
                        return Err(violation("bad hard link item"));
                    }
                    check_path(h)?;
                }
                self.entries += 1;
                if self.entries > self.max_entries {
                    return Err(violation(format!(
                        "more than {} entries in one download",
                        self.max_entries
                    )));
                }
                self.state = match item.data_len() {
                    0 => State::Walking,
                    n => State::Data { remaining: n },
                };
            }
            (Op::Walk, State::Data { .. }, Reply::FileError { message }) => {
                check_text("error message", message)?;
                self.state = State::Walking;
            }
            (Op::Walk, State::Start | State::Walking, Reply::Skipped { path, reason }) => {
                check_path(path)?;
                check_text("reason", reason)?;
                self.state = State::Walking;
            }
            (
                Op::Walk,
                State::Start | State::Walking | State::Data { .. },
                Reply::Progress { .. },
            ) => {}
            _ => return Err(unexpected()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
