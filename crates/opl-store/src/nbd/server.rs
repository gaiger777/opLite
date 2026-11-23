//! 읽기 전용 NBD 서버(설계 §11.1): fixed newstyle 협상(`OPT_GO`·`OPT_INFO`·`OPT_LIST`·`OPT_EXPORT_NAME`), 구조화
//! 응답, `base:allocation` 문맥, 명령 `READ`·`BLOCK_STATUS`·`FLUSH`(할 일 없음)·`DISC`. 쓰기 명령은 `EPERM`.
//! 한 연결의 요청은 나란히 처리하고(QEMU·`qemu-img convert`는 여러 요청을 한꺼번에 보낸다) 끝나는 대로 답한다.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::UnixListener;
use tokio::sync::{Semaphore, mpsc};

use super::client::{Extent, NbdError};
use super::*;

/// 원본이 요청에 실패한 까닭(errno 값과 문구)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceError {
    pub code: u32,
    pub message: String,
}

impl SourceError {
    pub fn io(message: impl Into<String>) -> Self {
        SourceError {
            code: EIO,
            message: message.into(),
        }
    }
}

/// 내보내기 하나의 원본
#[async_trait]
pub trait ExportSource: Send + Sync + 'static {
    /// 크기(바이트)
    fn size(&self) -> u64;
    /// `offset`에서 `len` 바이트(범위는 서버가 확인했다)
    async fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, SourceError>;
    /// `offset`부터의 `base:allocation` 범위들(앞에서부터 이어짐, 적어도 하나). 서버가 요청 범위로 자른다.
    async fn allocation(&self, offset: u64, len: u32) -> Result<Vec<Extent>, SourceError>;
    /// `base:allocation` 밖에 이 원본이 주는 문맥들(예: `qemu:dirty-bitmap:x` — 시험의 가짜 하이퍼바이저)
    fn extra_contexts(&self) -> Vec<String> {
        Vec::new()
    }
    /// [`ExportSource::extra_contexts`]의 문맥 `name`으로 `offset`부터의 범위들
    async fn context(&self, name: &str, offset: u64, len: u32) -> Result<Vec<Extent>, SourceError> {
        let _ = (offset, len);
        Err(SourceError {
            code: EINVAL,
            message: format!("no meta context {name}"),
        })
    }
}

/// 원본이 주는 모든 문맥(`base:allocation`이 먼저)
fn contexts_of(source: &dyn ExportSource) -> Vec<String> {
    let mut all = vec![BASE_ALLOCATION.to_string()];
    all.extend(source.extra_contexts());
    all
}

/// 한 연결에서 동시에 처리하는 요청 수
const IN_FLIGHT: usize = 16;

/// 이름 붙은 읽기 전용 내보내기들
#[derive(Default)]
pub struct NbdServer {
    exports: BTreeMap<String, Arc<dyn ExportSource>>,
}

impl NbdServer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: impl Into<String>, source: Arc<dyn ExportSource>) {
        self.exports.insert(name.into(), source);
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.exports.keys().map(String::as_str)
    }

    /// `listener`에서 받은 연결마다 [`NbdServer::handle`]을 띄운다. `shutdown`이 끝나면 더 받지 않고 돌아온다(열린
    /// 연결은 남는다).
    pub async fn serve(
        self: Arc<Self>,
        listener: UnixListener,
        shutdown: impl Future<Output = ()>,
    ) {
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => return,
                accepted = listener.accept() => match accepted {
                    Ok((s, _)) => {
                        let me = self.clone();
                        tokio::spawn(async move {
                            if let Err(e) = me.handle(s).await {
                                tracing::debug!(error = %e, "NBD connection ended");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot accept an NBD connection");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                },
            }
        }
    }

    /// 연결 하나를 끝까지 처리한다.
    pub async fn handle<S>(self: Arc<Self>, s: S) -> Result<(), NbdError>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (r, w) = tokio::io::split(s);
        let mut r = BufReader::new(r);
        let mut w = BufWriter::new(w);
        let Some(conn) = self.negotiate(&mut r, &mut w).await? else {
            return Ok(());
        };
        transmit(conn, r, w).await
    }

    /// 협상. 내보내기를 골랐으면 그 연결 상태, 클라이언트가 그만뒀으면 `None`.
    async fn negotiate<R, W>(&self, r: &mut R, w: &mut W) -> Result<Option<Conn>, NbdError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        w.write_u64(NBDMAGIC).await?;
        w.write_u64(IHAVEOPT).await?;
        w.write_u16(FLAG_FIXED_NEWSTYLE | FLAG_NO_ZEROES).await?;
        w.flush().await?;
        let cflags = r.read_u32().await?;
        if cflags & FLAG_C_FIXED_NEWSTYLE == 0 {
            return Err(NbdError::Protocol(
                "the client does not speak fixed newstyle".into(),
            ));
        }
        let no_zeroes = cflags & FLAG_C_NO_ZEROES != 0;
        let mut structured = false;
        let mut meta: Vec<String> = Vec::new();
        loop {
            if r.read_u64().await? != IHAVEOPT {
                return Err(NbdError::Protocol("bad option magic".into()));
            }
            let option = r.read_u32().await?;
            let len = r.read_u32().await?;
            if len > MAX_OPTION {
                // 읽어 버리지 않고 끊는다(이름·질의에 64 KiB면 넉넉하다).
                return Err(NbdError::Protocol("option data too long".into()));
            }
            let mut data = vec![0; len as usize];
            r.read_exact(&mut data).await?;
            match option {
                OPT_EXPORT_NAME => {
                    let name = String::from_utf8_lossy(&data).into_owned();
                    let Some(source) = self.exports.get(&name) else {
                        // 이 옵션에는 오류 응답이 없다 — 끊는다.
                        return Ok(None);
                    };
                    w.write_u64(source.size()).await?;
                    w.write_u16(TRANSMISSION_FLAGS).await?;
                    if !no_zeroes {
                        w.write_all(&[0; 124]).await?;
                    }
                    w.flush().await?;
                    return Ok(Some(Conn {
                        source: source.clone(),
                        structured,
                        meta: Vec::new(),
                    }));
                }
                OPT_ABORT => {
                    reply(w, option, REP_ACK, &[]).await?;
                    return Ok(None);
                }
                OPT_LIST => {
                    if !data.is_empty() {
                        reply(w, option, REP_ERR_INVALID, b"LIST takes no data").await?;
                        continue;
                    }
                    for name in self.exports.keys() {
                        let mut d = Vec::new();
                        put_str(&mut d, name);
                        reply(w, option, REP_SERVER, &d).await?;
                    }
                    reply(w, option, REP_ACK, &[]).await?;
                }
                OPT_STRUCTURED_REPLY => {
                    if !data.is_empty() {
                        reply(w, option, REP_ERR_INVALID, b"no data expected").await?;
                        continue;
                    }
                    structured = true;
                    reply(w, option, REP_ACK, &[]).await?;
                }
                OPT_LIST_META_CONTEXT | OPT_SET_META_CONTEXT => {
                    if !structured {
                        let msg = b"structured replies must be negotiated first";
                        reply(w, option, REP_ERR_INVALID, msg).await?;
                        continue;
                    }
                    let Some((name, queries)) = parse_meta(&data) else {
                        reply(w, option, REP_ERR_INVALID, b"bad meta context request").await?;
                        continue;
                    };
                    let Some(source) = self.exports.get(&name) else {
                        reply(w, option, REP_ERR_UNKNOWN, b"no such export").await?;
                        continue;
                    };
                    let set = option == OPT_SET_META_CONTEXT;
                    let all = contexts_of(source.as_ref());
                    let chosen: Vec<String> = all
                        .into_iter()
                        .filter(|c| {
                            queries.iter().any(|q| {
                                q == c || (!set && q.ends_with(':') && c.starts_with(q.as_str()))
                            }) || (!set && queries.is_empty())
                        })
                        .collect();
                    for (i, c) in chosen.iter().enumerate() {
                        let mut d = Vec::new();
                        // 고른 문맥의 id는 1부터(차례), 나열은 0
                        let id = if set { i as u32 + 1 } else { 0 };
                        d.extend_from_slice(&id.to_be_bytes());
                        d.extend_from_slice(c.as_bytes());
                        reply(w, option, REP_META_CONTEXT, &d).await?;
                    }
                    if set {
                        meta = chosen;
                    }
                    reply(w, option, REP_ACK, &[]).await?;
                }
                OPT_INFO | OPT_GO => {
                    let Some(name) = parse_go(&data) else {
                        reply(w, option, REP_ERR_INVALID, b"bad export request").await?;
                        continue;
                    };
                    let Some(source) = self.exports.get(&name) else {
                        let msg = format!("no export named {name:?}");
                        reply(w, option, REP_ERR_UNKNOWN, msg.as_bytes()).await?;
                        continue;
                    };
                    let mut d = Vec::new();
                    d.extend_from_slice(&INFO_EXPORT.to_be_bytes());
                    d.extend_from_slice(&source.size().to_be_bytes());
                    d.extend_from_slice(&TRANSMISSION_FLAGS.to_be_bytes());
                    reply(w, option, REP_INFO, &d).await?;
                    let mut d = Vec::new();
                    d.extend_from_slice(&INFO_BLOCK_SIZE.to_be_bytes());
                    d.extend_from_slice(&1u32.to_be_bytes());
                    d.extend_from_slice(&4096u32.to_be_bytes());
                    d.extend_from_slice(&MAX_REQUEST.to_be_bytes());
                    reply(w, option, REP_INFO, &d).await?;
                    reply(w, option, REP_ACK, &[]).await?;
                    if option == OPT_GO {
                        return Ok(Some(Conn {
                            source: source.clone(),
                            structured,
                            meta: std::mem::take(&mut meta),
                        }));
                    }
                }
                _ => {
                    reply(w, option, REP_ERR_UNSUP, b"unsupported option").await?;
                }
            }
        }
    }
}

/// 이 서버의 내보내기 전송 깃발: 읽기 전용, `FLUSH`(할 일 없음)·`DF` 받음, 여러 연결 됨
const TRANSMISSION_FLAGS: u16 =
    TFLAG_HAS_FLAGS | TFLAG_READ_ONLY | TFLAG_SEND_FLUSH | TFLAG_SEND_DF | TFLAG_CAN_MULTI_CONN;

struct Conn {
    source: Arc<dyn ExportSource>,
    structured: bool,
    /// 고른 문맥(id = 차례 + 1)
    meta: Vec<String>,
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn take_str(data: &[u8], at: &mut usize) -> Option<String> {
    let n = u32::from_be_bytes(data.get(*at..*at + 4)?.try_into().ok()?) as usize;
    *at += 4;
    let s = data.get(*at..*at + n)?;
    *at += n;
    Some(String::from_utf8_lossy(s).into_owned())
}

/// `OPT_*_META_CONTEXT`의 (내보내기 이름, 질의들)
fn parse_meta(data: &[u8]) -> Option<(String, Vec<String>)> {
    let mut at = 0;
    let name = take_str(data, &mut at)?;
    let n = u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?);
    at += 4;
    let mut queries = Vec::new();
    for _ in 0..n {
        queries.push(take_str(data, &mut at)?);
    }
    (at == data.len()).then_some((name, queries))
}

/// `OPT_INFO`·`OPT_GO`의 내보내기 이름(정보 요청 목록은 보지 않는다 — 늘 크기와 블록 크기를 준다)
fn parse_go(data: &[u8]) -> Option<String> {
    let mut at = 0;
    let name = take_str(data, &mut at)?;
    let n = u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?) as usize;
    (data.len() == at + 2 + 2 * n).then_some(name)
}

async fn reply<W: AsyncWrite + Unpin>(
    w: &mut W,
    option: u32,
    kind: u32,
    data: &[u8],
) -> Result<(), NbdError> {
    w.write_u64(REPLY_MAGIC).await?;
    w.write_u32(option).await?;
    w.write_u32(kind).await?;
    w.write_u32(data.len() as u32).await?;
    w.write_all(data).await?;
    w.flush().await?;
    Ok(())
}

fn simple_reply(cookie: u64, error: u32, data: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(16 + data.len());
    f.extend_from_slice(&SIMPLE_REPLY_MAGIC.to_be_bytes());
    f.extend_from_slice(&error.to_be_bytes());
    f.extend_from_slice(&cookie.to_be_bytes());
    f.extend_from_slice(data);
    f
}

fn chunk(cookie: u64, kind: u16, done: bool, payload: &[&[u8]]) -> Vec<u8> {
    let len: usize = payload.iter().map(|p| p.len()).sum();
    let mut f = Vec::with_capacity(20 + len);
    f.extend_from_slice(&STRUCTURED_REPLY_MAGIC.to_be_bytes());
    let flags = if done { REPLY_FLAG_DONE } else { 0 };
    f.extend_from_slice(&flags.to_be_bytes());
    f.extend_from_slice(&kind.to_be_bytes());
    f.extend_from_slice(&cookie.to_be_bytes());
    f.extend_from_slice(&(len as u32).to_be_bytes());
    for p in payload {
        f.extend_from_slice(p);
    }
    f
}

fn error_chunk(cookie: u64, e: &SourceError) -> Vec<u8> {
    let msg = &e.message.as_bytes()[..e.message.len().min(4096)];
    chunk(
        cookie,
        REPLY_TYPE_ERROR,
        true,
        &[
            &e.code.to_be_bytes(),
            &(msg.len() as u16).to_be_bytes(),
            msg,
        ],
    )
}

/// 요청 하나의 답(구조화면 조각, 아니면 단순 응답)
fn error_reply(structured: bool, cookie: u64, e: &SourceError) -> Vec<u8> {
    if structured {
        error_chunk(cookie, e)
    } else {
        simple_reply(cookie, e.code, &[])
    }
}

fn invalid(message: &str) -> SourceError {
    SourceError {
        code: EINVAL,
        message: message.into(),
    }
}

async fn transmit<R, W>(conn: Conn, mut r: R, mut w: W) -> Result<(), NbdError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(IN_FLIGHT * 2);
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            w.write_all(&frame).await?;
            if rx.is_empty() {
                w.flush().await?;
            }
        }
        w.flush().await?;
        w.shutdown().await
    });
    let slots = Arc::new(Semaphore::new(IN_FLIGHT));
    let conn = Arc::new(conn);
    let size = conn.source.size();
    let result: Result<(), NbdError> = async {
        loop {
            if r.read_u32().await? != REQUEST_MAGIC {
                return Err(NbdError::Protocol("bad request magic".into()));
            }
            let flags = r.read_u16().await?;
            let cmd = r.read_u16().await?;
            let cookie = r.read_u64().await?;
            let offset = r.read_u64().await?;
            let len = r.read_u32().await?;
            let in_range =
                len <= MAX_REQUEST && offset.checked_add(len as u64).is_some_and(|e| e <= size);
            let send = |frame: Vec<u8>| {
                let tx = tx.clone();
                async move {
                    tx.send(frame)
                        .await
                        .map_err(|_| NbdError::Protocol("the reply writer stopped".into()))
                }
            };
            match cmd {
                CMD_DISC => return Ok(()),
                CMD_FLUSH => send(simple_reply(cookie, 0, &[])).await?,
                CMD_WRITE => {
                    if len > MAX_REQUEST {
                        return Err(NbdError::Protocol("write payload too long".into()));
                    }
                    let mut sink = vec![0; len as usize];
                    r.read_exact(&mut sink).await?;
                    send(simple_reply(cookie, EPERM, &[])).await?;
                }
                CMD_READ | CMD_BLOCK_STATUS => {
                    if !in_range {
                        let e = invalid("request out of range");
                        send(error_reply(conn.structured, cookie, &e)).await?;
                        continue;
                    }
                    if cmd == CMD_BLOCK_STATUS && (!conn.structured || conn.meta.is_empty()) {
                        send(simple_reply(cookie, EINVAL, &[])).await?;
                        continue;
                    }
                    let permit = slots.clone().acquire_owned().await.expect("semaphore open");
                    let conn = conn.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let frame = if cmd == CMD_READ {
                            read_reply(&conn, cookie, offset, len).await
                        } else {
                            status_reply(&conn, cookie, offset, len, flags & CMD_FLAG_REQ_ONE != 0)
                                .await
                        };
                        let _ = tx.send(frame).await;
                        drop(permit);
                    });
                }
                _ => send(simple_reply(cookie, EINVAL, &[])).await?,
            }
        }
    }
    .await;
    // 남은 요청이 다 답하면 쓰는 쪽이 끝난다.
    drop(tx);
    let written = writer
        .await
        .map_err(|e| NbdError::Protocol(format!("reply writer: {e}")))?;
    result?;
    written.map_err(NbdError::Io)
}

async fn read_reply(conn: &Conn, cookie: u64, offset: u64, len: u32) -> Vec<u8> {
    let data = if len == 0 {
        Ok(Vec::new())
    } else {
        conn.source.read(offset, len).await
    };
    match data {
        Ok(d) if d.len() == len as usize => {
            if conn.structured {
                chunk(
                    cookie,
                    REPLY_TYPE_OFFSET_DATA,
                    true,
                    &[&offset.to_be_bytes(), &d],
                )
            } else {
                simple_reply(cookie, 0, &d)
            }
        }
        Ok(_) => error_reply(conn.structured, cookie, &SourceError::io("short read")),
        Err(e) => {
            tracing::warn!(offset, len, error = %e.message, "NBD read failed");
            error_reply(conn.structured, cookie, &e)
        }
    }
}

async fn status_reply(conn: &Conn, cookie: u64, offset: u64, len: u32, one: bool) -> Vec<u8> {
    if len == 0 {
        return error_reply(true, cookie, &invalid("empty block status request"));
    }
    let mut frames = Vec::new();
    for (i, name) in conn.meta.iter().enumerate() {
        let extents = if name == BASE_ALLOCATION {
            conn.source.allocation(offset, len).await
        } else {
            conn.source.context(name, offset, len).await
        };
        let ext = match extents {
            Ok(ext) => ext,
            Err(e) => return error_reply(true, cookie, &e),
        };
        let mut body = Vec::new();
        body.extend_from_slice(&(i as u32 + 1).to_be_bytes());
        let mut left = len as u64;
        for e in ext {
            if left == 0 {
                break;
            }
            let n = e.length.min(left);
            if n == 0 {
                continue;
            }
            left -= n;
            body.extend_from_slice(&(n as u32).to_be_bytes());
            body.extend_from_slice(&e.flags.to_be_bytes());
            if one {
                break;
            }
        }
        if body.len() == 4 {
            return error_reply(true, cookie, &SourceError::io("no extents"));
        }
        let last = i + 1 == conn.meta.len();
        frames.extend(chunk(cookie, REPLY_TYPE_BLOCK_STATUS, last, &[&body]));
    }
    frames
}
