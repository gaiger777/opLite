//! NBD 클라이언트(설계 §8.1·§8.2): QEMU fleecing 내보내기와 `qemu-nbd`를 읽는다. 요청 하나씩 차례로 보낸다 — 동시에
//! 읽으려면 연결을 여럿 연다(QEMU·`qemu-nbd --shared=0`은 읽기 전용 내보내기에 여러 연결을 받는다).

use std::collections::BTreeMap;
use std::path::Path;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufStream};
use tokio::net::UnixStream;

use super::*;

#[derive(Debug, thiserror::Error)]
pub enum NbdError {
    #[error("NBD connection: {0}")]
    Io(#[from] std::io::Error),
    #[error("NBD protocol: {0}")]
    Protocol(String),
    #[error("NBD export {0}")]
    Export(String),
    /// 서버가 요청에 오류로 답했다(errno 값과 문구)
    #[error("NBD server error {code}{}", if .message.is_empty() { String::new() } else { format!(": {}", .message) })]
    Server { code: u32, message: String },
}

fn proto(msg: impl Into<String>) -> NbdError {
    NbdError::Protocol(msg.into())
}

/// `BLOCK_STATUS`의 범위 하나
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub length: u64,
    /// 문맥의 상태 비트([`STATE_HOLE`]·[`STATE_ZERO`], [`STATE_DIRTY`])
    pub flags: u32,
}

/// 열린 내보내기 하나에 붙은 연결
pub struct NbdClient<S = UnixStream> {
    io: BufStream<S>,
    size: u64,
    flags: u16,
    structured: bool,
    /// 고른 문맥 이름 → 서버가 준 id
    contexts: BTreeMap<String, u32>,
    next_cookie: u64,
}

impl<S> std::fmt::Debug for NbdClient<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NbdClient")
            .field("size", &self.size)
            .field("contexts", &self.contexts)
            .finish()
    }
}

impl NbdClient<UnixStream> {
    /// 유닉스 소켓 `path`의 내보내기 `export`에 붙고 `contexts`(예: [`BASE_ALLOCATION`])를 고른다. 서버가 모르는
    /// 문맥은 빠진다 — [`NbdClient::has_context`]로 본다.
    pub async fn connect(path: &Path, export: &str, contexts: &[&str]) -> Result<Self, NbdError> {
        let s = UnixStream::connect(path).await?;
        Self::handshake(s, export, contexts).await
    }

    /// 소켓 `path`의 서버가 내보내는 이름들(`OPT_LIST`). 서버가 답하는지 보는 데도 쓴다.
    pub async fn list(path: &Path) -> Result<Vec<String>, NbdError> {
        let s = UnixStream::connect(path).await?;
        list_exports(s).await
    }
}

/// 서버 인사를 읽고 클라이언트 깃발을 보낸다.
async fn greet<S: AsyncRead + AsyncWrite + Unpin>(io: &mut BufStream<S>) -> Result<(), NbdError> {
    if io.read_u64().await? != NBDMAGIC || io.read_u64().await? != IHAVEOPT {
        return Err(proto("not a newstyle NBD server"));
    }
    let hs = io.read_u16().await?;
    if hs & FLAG_FIXED_NEWSTYLE == 0 {
        return Err(proto("the server does not speak fixed newstyle"));
    }
    let mut cf = FLAG_C_FIXED_NEWSTYLE;
    if hs & FLAG_NO_ZEROES != 0 {
        cf |= FLAG_C_NO_ZEROES;
    }
    io.write_u32(cf).await?;
    io.flush().await?;
    Ok(())
}

async fn send_option<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut BufStream<S>,
    option: u32,
    data: &[u8],
) -> Result<(), NbdError> {
    io.write_u64(IHAVEOPT).await?;
    io.write_u32(option).await?;
    io.write_u32(data.len() as u32).await?;
    io.write_all(data).await?;
    io.flush().await?;
    Ok(())
}

/// 옵션 응답 하나(종류, 데이터)
async fn read_option_reply<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut BufStream<S>,
    option: u32,
) -> Result<(u32, Vec<u8>), NbdError> {
    if io.read_u64().await? != REPLY_MAGIC {
        return Err(proto("bad option reply magic"));
    }
    let opt = io.read_u32().await?;
    if opt != option {
        return Err(proto(format!("reply to option {opt}, expected {option}")));
    }
    let kind = io.read_u32().await?;
    let len = io.read_u32().await?;
    if len > MAX_OPTION {
        return Err(proto("option reply too long"));
    }
    let mut data = vec![0; len as usize];
    io.read_exact(&mut data).await?;
    Ok((kind, data))
}

fn option_error(kind: u32, data: &[u8]) -> String {
    let text = String::from_utf8_lossy(data);
    let what = match kind {
        REP_ERR_UNSUP => "unsupported",
        REP_ERR_INVALID => "invalid",
        REP_ERR_UNKNOWN => "unknown",
        REP_ERR_TOO_BIG => "too big",
        _ => "refused",
    };
    if text.is_empty() {
        what.to_string()
    } else {
        format!("{what}: {text}")
    }
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().expect("4 bytes"))
}

fn be_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().expect("8 bytes"))
}

async fn list_exports<S: AsyncRead + AsyncWrite + Unpin>(s: S) -> Result<Vec<String>, NbdError> {
    let mut io = BufStream::new(s);
    greet(&mut io).await?;
    send_option(&mut io, OPT_LIST, &[]).await?;
    let mut names = Vec::new();
    loop {
        let (kind, data) = read_option_reply(&mut io, OPT_LIST).await?;
        match kind {
            REP_SERVER if data.len() >= 4 => {
                let n = be_u32(&data) as usize;
                let name = data
                    .get(4..4 + n)
                    .ok_or_else(|| proto("short export name"))?;
                names.push(String::from_utf8_lossy(name).into_owned());
            }
            REP_ACK => break,
            k => {
                return Err(NbdError::Export(format!(
                    "list: {}",
                    option_error(k, &data)
                )));
            }
        }
    }
    let _ = send_option(&mut io, OPT_ABORT, &[]).await;
    Ok(names)
}

impl<S: AsyncRead + AsyncWrite + Unpin> NbdClient<S> {
    /// 열린 스트림에서 협상한다.
    pub async fn handshake(s: S, export: &str, contexts: &[&str]) -> Result<Self, NbdError> {
        let mut io = BufStream::new(s);
        greet(&mut io).await?;
        send_option(&mut io, OPT_STRUCTURED_REPLY, &[]).await?;
        let (kind, data) = read_option_reply(&mut io, OPT_STRUCTURED_REPLY).await?;
        let structured = match kind {
            REP_ACK => true,
            REP_ERR_UNSUP => false,
            k => {
                return Err(proto(format!(
                    "structured reply: {}",
                    option_error(k, &data)
                )));
            }
        };
        let mut chosen = BTreeMap::new();
        if !contexts.is_empty() {
            if !structured {
                return Err(proto(
                    "the server has no structured replies (needed for block status)",
                ));
            }
            let mut q = Vec::new();
            put_str(&mut q, export);
            q.extend_from_slice(&(contexts.len() as u32).to_be_bytes());
            for c in contexts {
                put_str(&mut q, c);
            }
            send_option(&mut io, OPT_SET_META_CONTEXT, &q).await?;
            loop {
                let (kind, data) = read_option_reply(&mut io, OPT_SET_META_CONTEXT).await?;
                match kind {
                    REP_META_CONTEXT if data.len() >= 4 => {
                        let id = be_u32(&data);
                        let name = String::from_utf8_lossy(&data[4..]).into_owned();
                        chosen.insert(name, id);
                    }
                    REP_ACK => break,
                    k => {
                        return Err(NbdError::Export(format!(
                            "{export}: meta context: {}",
                            option_error(k, &data)
                        )));
                    }
                }
            }
        }
        let mut go = Vec::new();
        put_str(&mut go, export);
        go.extend_from_slice(&1u16.to_be_bytes());
        go.extend_from_slice(&INFO_BLOCK_SIZE.to_be_bytes());
        send_option(&mut io, OPT_GO, &go).await?;
        let mut info = None;
        loop {
            let (kind, data) = read_option_reply(&mut io, OPT_GO).await?;
            match kind {
                REP_INFO if data.len() >= 2 => {
                    let t = u16::from_be_bytes([data[0], data[1]]);
                    if t == INFO_EXPORT {
                        if data.len() < 12 {
                            return Err(proto("short export info"));
                        }
                        info = Some((be_u64(&data[2..]), u16::from_be_bytes([data[10], data[11]])));
                    }
                }
                REP_ACK => break,
                k => {
                    return Err(NbdError::Export(format!(
                        "{export}: {}",
                        option_error(k, &data)
                    )));
                }
            }
        }
        let (size, flags) = info.ok_or_else(|| proto("the server sent no export info"))?;
        Ok(NbdClient {
            io,
            size,
            flags,
            structured,
            contexts: chosen,
            next_cookie: 1,
        })
    }

    /// 내보내기의 크기(바이트)
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn read_only(&self) -> bool {
        self.flags & TFLAG_READ_ONLY != 0
    }

    /// 서버가 고른 문맥인가
    pub fn has_context(&self, name: &str) -> bool {
        self.contexts.contains_key(name)
    }

    async fn request(
        &mut self,
        cmd: u16,
        flags: u16,
        offset: u64,
        len: u32,
    ) -> Result<u64, NbdError> {
        let cookie = self.next_cookie;
        self.next_cookie += 1;
        let io = &mut self.io;
        io.write_u32(REQUEST_MAGIC).await?;
        io.write_u16(flags).await?;
        io.write_u16(cmd).await?;
        io.write_u64(cookie).await?;
        io.write_u64(offset).await?;
        io.write_u32(len).await?;
        io.flush().await?;
        Ok(cookie)
    }

    fn check_range(&self, offset: u64, len: u64) -> Result<(), NbdError> {
        if len > MAX_REQUEST as u64 || offset.checked_add(len).is_none_or(|e| e > self.size) {
            return Err(proto(format!(
                "request {offset}+{len} is out of range (size {})",
                self.size
            )));
        }
        Ok(())
    }

    /// 구조화 응답 조각의 머리(깃발, 종류, 길이). 단순 응답이면 `None`과 그 오류 값.
    async fn reply_head(&mut self, cookie: u64) -> Result<Head, NbdError> {
        let magic = self.io.read_u32().await?;
        match magic {
            SIMPLE_REPLY_MAGIC => {
                let error = self.io.read_u32().await?;
                let c = self.io.read_u64().await?;
                if c != cookie {
                    return Err(proto(format!("reply for cookie {c}, expected {cookie}")));
                }
                Ok(Head::Simple(error))
            }
            STRUCTURED_REPLY_MAGIC => {
                let flags = self.io.read_u16().await?;
                let kind = self.io.read_u16().await?;
                let c = self.io.read_u64().await?;
                let len = self.io.read_u32().await?;
                if c != cookie {
                    return Err(proto(format!("reply for cookie {c}, expected {cookie}")));
                }
                if len > MAX_REQUEST + 64 {
                    return Err(proto("reply chunk too long"));
                }
                Ok(Head::Chunk {
                    done: flags & REPLY_FLAG_DONE != 0,
                    kind,
                    len,
                })
            }
            m => Err(proto(format!("bad reply magic {m:#x}"))),
        }
    }

    async fn read_payload(&mut self, len: u32) -> Result<Vec<u8>, NbdError> {
        let mut data = vec![0; len as usize];
        self.io.read_exact(&mut data).await?;
        Ok(data)
    }

    /// 오류 조각의 몸통 → 오류
    fn chunk_error(data: &[u8]) -> NbdError {
        if data.len() < 6 {
            return proto("short error chunk");
        }
        let code = be_u32(data);
        let n = u16::from_be_bytes([data[4], data[5]]) as usize;
        let message = data
            .get(6..6 + n)
            .map(|m| String::from_utf8_lossy(m).into_owned())
            .unwrap_or_default();
        NbdError::Server { code, message }
    }

    /// `offset`에서 `len` 바이트를 읽는다(구멍은 0).
    pub async fn read(&mut self, offset: u64, len: u32) -> Result<Vec<u8>, NbdError> {
        self.check_range(offset, len as u64)?;
        let mut buf = vec![0u8; len as usize];
        if len == 0 {
            return Ok(buf);
        }
        let cookie = self.request(CMD_READ, 0, offset, len).await?;
        let mut error = None;
        loop {
            match self.reply_head(cookie).await? {
                Head::Simple(0) => {
                    self.io.read_exact(&mut buf).await?;
                    return Ok(buf);
                }
                Head::Simple(code) => {
                    return Err(NbdError::Server {
                        code,
                        message: String::new(),
                    });
                }
                Head::Chunk { done, kind, len } => {
                    let data = self.read_payload(len).await?;
                    match kind {
                        REPLY_TYPE_OFFSET_DATA => {
                            if data.len() < 8 {
                                return Err(proto("short data chunk"));
                            }
                            let at = be_u64(&data);
                            let body = &data[8..];
                            let start = at
                                .checked_sub(offset)
                                .filter(|s| s + body.len() as u64 <= buf.len() as u64)
                                .ok_or_else(|| proto("data chunk outside the request"))?
                                as usize;
                            buf[start..start + body.len()].copy_from_slice(body);
                        }
                        REPLY_TYPE_OFFSET_HOLE => {
                            if data.len() < 12 {
                                return Err(proto("short hole chunk"));
                            }
                            let at = be_u64(&data);
                            let n = be_u32(&data[8..]) as u64;
                            let start = at
                                .checked_sub(offset)
                                .filter(|s| s + n <= buf.len() as u64)
                                .ok_or_else(|| proto("hole chunk outside the request"))?
                                as usize;
                            buf[start..start + n as usize].fill(0);
                        }
                        REPLY_TYPE_NONE => {}
                        REPLY_TYPE_ERROR | REPLY_TYPE_ERROR_OFFSET => {
                            error.get_or_insert(Self::chunk_error(&data));
                        }
                        k => return Err(proto(format!("unexpected reply chunk {k} to a read"))),
                    }
                    if done {
                        return match error {
                            Some(e) => Err(e),
                            None => Ok(buf),
                        };
                    }
                }
            }
        }
    }

    /// 문맥 `context`로 `offset`부터 범위들을 묻는다. 서버는 `len`보다 적게(또는 마지막 범위를 더 길게) 줄 수
    /// 있다 — 받은 그대로 돌려준다(앞에서부터 이어짐). 모든 범위를 채우려면 [`NbdClient::extents`].
    pub async fn block_status(
        &mut self,
        context: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<Extent>, NbdError> {
        let id = *self
            .contexts
            .get(context)
            .ok_or_else(|| NbdError::Export(format!("meta context {context} is not available")))?;
        if !self.structured {
            return Err(proto("block status needs structured replies"));
        }
        if offset.checked_add(len as u64).is_none_or(|e| e > self.size) {
            return Err(proto("block status request out of range"));
        }
        let cookie = self.request(CMD_BLOCK_STATUS, 0, offset, len).await?;
        let mut out = Vec::new();
        let mut error = None;
        loop {
            match self.reply_head(cookie).await? {
                Head::Simple(code) => {
                    return Err(NbdError::Server {
                        code: if code == 0 { EIO } else { code },
                        message: "block status got a simple reply".into(),
                    });
                }
                Head::Chunk { done, kind, len } => {
                    let data = self.read_payload(len).await?;
                    match kind {
                        REPLY_TYPE_BLOCK_STATUS => {
                            if data.len() < 4 || (data.len() - 4) % 8 != 0 {
                                return Err(proto("bad block status chunk"));
                            }
                            if be_u32(&data) == id {
                                for d in data[4..].chunks_exact(8) {
                                    out.push(Extent {
                                        length: be_u32(d) as u64,
                                        flags: be_u32(&d[4..]),
                                    });
                                }
                            }
                        }
                        REPLY_TYPE_NONE => {}
                        REPLY_TYPE_ERROR | REPLY_TYPE_ERROR_OFFSET => {
                            error.get_or_insert(Self::chunk_error(&data));
                        }
                        k => {
                            return Err(proto(format!(
                                "unexpected reply chunk {k} to block status"
                            )));
                        }
                    }
                    if done {
                        if let Some(e) = error {
                            return Err(e);
                        }
                        if out.is_empty() || out.iter().any(|e| e.length == 0) {
                            return Err(proto("block status without extents"));
                        }
                        return Ok(out);
                    }
                }
            }
        }
    }

    /// `offset..offset+len`을 빈틈없이 덮는 범위들(문맥 `context`). 이웃한 같은 상태는 합치고 끝을 자른다.
    pub async fn extents(
        &mut self,
        context: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<Extent>, NbdError> {
        const STEP: u64 = 1 << 30;
        let end = offset
            .checked_add(len)
            .filter(|e| *e <= self.size)
            .ok_or_else(|| proto("extents request out of range"))?;
        let mut at = offset;
        let mut out: Vec<Extent> = Vec::new();
        while at < end {
            let ask = (end - at).min(STEP) as u32;
            let got = self.block_status(context, at, ask).await?;
            for e in got {
                if at >= end {
                    break;
                }
                let length = e.length.min(end - at);
                at += length;
                match out.last_mut() {
                    Some(last) if last.flags == e.flags => last.length += length,
                    _ => out.push(Extent {
                        length,
                        flags: e.flags,
                    }),
                }
            }
        }
        Ok(out)
    }

    /// 연결을 닫는다(`DISC`).
    pub async fn disconnect(mut self) -> Result<(), NbdError> {
        self.request(CMD_DISC, 0, 0, 0).await?;
        self.io.shutdown().await?;
        Ok(())
    }
}

enum Head {
    Simple(u32),
    Chunk { done: bool, kind: u16, len: u32 },
}
