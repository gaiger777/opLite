//! 도우미 에이전트의 요청 처리 몸통(설계 §9.1). 통신 규약 위에서 요청마다 스레드 하나로 답한다: 탐지·마운트는
//! [`Volumes`], 나열·읽기·걷기는 [`Fs`] 위에서. 도우미 VM에서는 `agent::linux`의 실제 볼륨(blkid·lvm·mount)이고,
//! 호스트 시험과 opld의 가짜 도우미는 [`FixedVolumes`](임시 디렉터리나 메모리 나무)다.

pub mod fs;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod mount;
pub mod probe;

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rustix::io::Errno;

pub use fs::{DirFs, Fs, MemFs, Meta};

use crate::proto::{
    AgentError, Entry, EntryKind, ErrorCode, Frame, FrameKind, Hello, Item, MAX_PAGE_ENTRIES,
    MAX_PATH_DEPTH, MAX_PAYLOAD, MAX_TEXT, MAX_WALK_PATHS, Name, Reply, Request, STREAM_WINDOW,
    Volume, read_frame, write_frame,
};

/// 붙인 볼륨
#[derive(Clone)]
pub struct Mounted {
    pub fs: Arc<dyn Fs>,
    /// 덧붙임(예: 저널을 건너뛰고 붙음)
    pub note: Option<String>,
}

/// 볼륨 찾기와 붙이기
pub trait Volumes: Send + Sync {
    fn probe(&self) -> Result<Vec<Volume>, AgentError>;
    /// 볼륨을 붙인다(이미 붙었으면 그대로 — 여러 흐름이 함께 불러도 한 번만 붙인다)
    fn mount(&self, volume: &str) -> Result<Mounted, AgentError>;
}

/// 정해 둔 볼륨들(시험·가짜 도우미)
pub struct FixedVolumes {
    volumes: Vec<(Volume, Option<Arc<dyn Fs>>)>,
}

impl FixedVolumes {
    /// `fs`가 없는 볼륨은 붙일 수 없다(`mountable`도 거짓이어야 맞다).
    pub fn new(volumes: Vec<(Volume, Option<Arc<dyn Fs>>)>) -> Self {
        FixedVolumes { volumes }
    }
}

impl Volumes for FixedVolumes {
    fn probe(&self) -> Result<Vec<Volume>, AgentError> {
        Ok(self.volumes.iter().map(|(v, _)| v.clone()).collect())
    }

    fn mount(&self, volume: &str) -> Result<Mounted, AgentError> {
        let (_, fs) = self
            .volumes
            .iter()
            .find(|(v, _)| v.id == volume)
            .ok_or_else(|| AgentError::new(ErrorCode::NotFound, "no such volume"))?;
        let fs = fs.clone().ok_or_else(|| {
            AgentError::new(
                ErrorCode::NotMountable,
                "this volume has no filesystem to browse",
            )
        })?;
        Ok(Mounted { fs, note: None })
    }
}

/// 몸통 설정
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// 동시에 도는 흐름 상한(opld는 세션마다 4개까지 쓴다)
    pub max_streams: usize,
    /// 걷기가 다른 틀을 보내지 않은 채 이만큼 지나면 `Progress`
    pub progress_every: Duration,
    /// 데이터 틀 하나의 크기
    pub chunk: usize,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            max_streams: 8,
            progress_every: Duration::from_secs(30),
            chunk: MAX_PAYLOAD,
        }
    }
}

/// 여러 스레드가 나눠 쓰는 쓰는 쪽(틀 하나를 잠금 안에서 통째로)
struct Out<W> {
    w: Mutex<W>,
}

impl<W: Write> Out<W> {
    fn send(&self, frame: &Frame) -> io::Result<()> {
        let mut w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        write_frame(&mut *w, frame)?;
        w.flush()
    }
}

/// 흐름 하나의 취소 깃발과 받을 몫(규약 판 2): `Resp`·`Data` 틀은 몫이 있을 때만 보낸다.
struct Ctl {
    cancel: AtomicBool,
    credit: Mutex<u64>,
    wake: Condvar,
}

impl Ctl {
    fn new() -> Self {
        Ctl {
            cancel: AtomicBool::new(false),
            credit: Mutex::new(STREAM_WINDOW),
            wake: Condvar::new(),
        }
    }

    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _guard = self.credit.lock().unwrap_or_else(|e| e.into_inner());
        self.wake.notify_all();
    }

    /// opld가 돌려준 몫(몫은 창보다 커지지 않는다 — 넘게 돌려주면 opld 잘못이라 창에서 자른다)
    fn grant(&self, bytes: u32) {
        let mut c = self.credit.lock().unwrap_or_else(|e| e.into_inner());
        *c = (*c + u64::from(bytes)).min(STREAM_WINDOW);
        self.wake.notify_all();
    }

    /// 몫 `cost`를 쓴다(모자라면 opld가 돌려줄 때까지 기다림, 취소되면 멈춤)
    fn take(&self, cost: u64) -> Result<(), Stop> {
        let mut c = self.credit.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(AgentError::new(ErrorCode::Cancelled, "cancelled").into());
            }
            if *c >= cost {
                *c -= cost;
                return Ok(());
            }
            c = self.wake.wait(c).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// 흐름 하나의 보내기
struct Stream<'a, W> {
    id: u32,
    out: &'a Out<W>,
    ctl: &'a Ctl,
    last_sent: Instant,
}

/// 흐름 처리 중 멈춤: 규약 오류로 답하거나(Agent), 보낼 수 없음(연결이 끊겼다)
enum Stop {
    Agent(AgentError),
    Disconnected,
}

impl From<AgentError> for Stop {
    fn from(e: AgentError) -> Self {
        Stop::Agent(e)
    }
}

impl From<io::Error> for Stop {
    fn from(_: io::Error) -> Self {
        Stop::Disconnected
    }
}

impl<W: Write> Stream<'_, W> {
    fn send(&mut self, kind: FrameKind, payload: Vec<u8>) -> Result<(), Stop> {
        self.put(&Frame::new(self.id, kind, payload))
    }

    fn reply(&mut self, reply: &Reply) -> Result<(), Stop> {
        self.put(&Frame::json(self.id, FrameKind::Resp, reply))
    }

    fn put(&mut self, frame: &Frame) -> Result<(), Stop> {
        self.ctl.take(frame.cost())?;
        self.out.send(frame)?;
        self.last_sent = Instant::now();
        Ok(())
    }

    fn check_cancel(&self) -> Result<(), Stop> {
        if self.ctl.cancel.load(Ordering::Relaxed) {
            return Err(AgentError::new(ErrorCode::Cancelled, "cancelled").into());
        }
        Ok(())
    }
}

/// 오류 문구를 규약 길이 안으로
fn short(mut s: String) -> String {
    if s.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// 파일시스템 오류 → 규약 오류
pub fn fs_error(e: &io::Error) -> AgentError {
    let errno = e.raw_os_error().map(Errno::from_raw_os_error);
    let code = match errno {
        Some(Errno::NOENT) => ErrorCode::NotFound,
        Some(Errno::NOTDIR) => ErrorCode::NotDir,
        Some(Errno::LOOP) => ErrorCode::Symlink,
        _ if e.kind() == io::ErrorKind::NotFound => ErrorCode::NotFound,
        _ if e.kind() == io::ErrorKind::InvalidInput => ErrorCode::NotFile,
        _ => ErrorCode::Io,
    };
    let message = match code {
        ErrorCode::NotFound => "no such file or directory".to_string(),
        ErrorCode::NotDir => "not a directory".to_string(),
        ErrorCode::Symlink => {
            "a path component is a symbolic link (links are not followed)".to_string()
        }
        ErrorCode::NotFile => "not a regular file".to_string(),
        _ => short(e.to_string()),
    };
    AgentError::new(code, message)
}

fn bad_request(msg: &str) -> AgentError {
    AgentError::new(ErrorCode::BadRequest, msg)
}

/// 커널 명령줄의 `key=value` 값(`opl.proto=1`)
pub fn cmdline_value<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// 흐름 표(취소 깃발·받을 몫)
type Active = Arc<Mutex<HashMap<u32, Arc<Ctl>>>>;

/// 연결 하나를 처리한다: `Hello`를 보내고, 요청을 읽어 흐름마다 스레드로 답한다. `Shutdown`이나 읽는 쪽 끝(EOF)에서
/// 돌아온다(도는 흐름은 취소한다). 규약을 어긴 틀은 연결을 끝낸다(opld가 세션을 닫는다).
pub fn serve<V, R, W>(
    volumes: Arc<V>,
    hello: Hello,
    mut reader: R,
    writer: W,
    opts: ServeOptions,
) -> io::Result<()>
where
    V: Volumes + 'static,
    R: Read,
    W: Write + Send + 'static,
{
    let out = Arc::new(Out {
        w: Mutex::new(writer),
    });
    out.send(&Frame::json(0, FrameKind::Resp, &Reply::Hello(hello)))?;
    let active: Active = Arc::new(Mutex::new(HashMap::new()));
    let result = loop {
        let frame = match read_frame(&mut reader) {
            Ok(Some(f)) => f,
            Ok(None) => break Ok(()),
            Err(crate::proto::ReadError::Io(e)) => break Err(e),
            Err(crate::proto::ReadError::Frame(e)) => {
                break Err(io::Error::new(io::ErrorKind::InvalidData, e));
            }
        };
        if frame.kind == FrameKind::Ack && frame.stream != 0 {
            let Some(bytes) = frame.ack_bytes() else {
                break Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "an ack frame must carry 4 bytes",
                ));
            };
            // 이미 끝난 흐름의 몫은 버린다(끝과 엇갈려 온다).
            if let Some(ctl) = active.lock().unwrap().get(&frame.stream) {
                ctl.grant(bytes);
            }
            continue;
        }
        if frame.kind != FrameKind::Req || frame.stream == 0 {
            break Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected a request frame on a stream above 0",
            ));
        }
        let id = frame.stream;
        let req: Request = match frame.parse() {
            Ok(r) => r,
            Err(e) => {
                out.send(&Frame::json(
                    id,
                    FrameKind::Err,
                    &bad_request(&short(format!("bad request: {e}"))),
                ))?;
                continue;
            }
        };
        match req {
            Request::Cancel { stream } => {
                if let Some(ctl) = active.lock().unwrap().get(&stream) {
                    ctl.cancel();
                }
                out.send(&Frame::end(id))?;
            }
            Request::Shutdown => {
                out.send(&Frame::end(id))?;
                break Ok(());
            }
            req => {
                let ctl = Arc::new(Ctl::new());
                {
                    let mut a = active.lock().unwrap();
                    let refuse = if a.contains_key(&id) {
                        Some(bad_request("stream id in use"))
                    } else if a.len() >= opts.max_streams {
                        Some(AgentError::new(
                            ErrorCode::Busy,
                            "too many concurrent requests",
                        ))
                    } else {
                        None
                    };
                    if let Some(e) = refuse {
                        drop(a);
                        out.send(&Frame::json(id, FrameKind::Err, &e))?;
                        continue;
                    }
                    a.insert(id, ctl.clone());
                }
                let (volumes, out, active, opts) =
                    (volumes.clone(), out.clone(), active.clone(), opts.clone());
                std::thread::spawn(move || {
                    let mut s = Stream {
                        id,
                        out: &out,
                        ctl: &ctl,
                        last_sent: Instant::now(),
                    };
                    let r = handle(&*volumes, &req, &mut s, &opts);
                    // 표에서 먼저 빼고 끝 틀을 보낸다 — 끝을 받은 opld가 같은 번호를 다시 써도 겹치지 않게.
                    active.lock().unwrap().remove(&id);
                    let _ = match r {
                        Ok(()) => out.send(&Frame::end(id)),
                        Err(Stop::Agent(e)) => out.send(&Frame::json(id, FrameKind::Err, &e)),
                        Err(Stop::Disconnected) => Ok(()),
                    };
                });
            }
        }
    };
    for ctl in active.lock().unwrap().values() {
        ctl.cancel();
    }
    result
}

fn handle<V: Volumes + ?Sized, W: Write>(
    volumes: &V,
    req: &Request,
    s: &mut Stream<'_, W>,
    opts: &ServeOptions,
) -> Result<(), Stop> {
    match req {
        Request::Probe => {
            let volumes = volumes.probe()?;
            s.reply(&Reply::Volumes { volumes })
        }
        Request::Mount { volume } => {
            let m = volumes.mount(volume)?;
            s.reply(&Reply::Mounted { note: m.note })
        }
        Request::List {
            volume,
            path,
            cursor,
            limit,
        } => {
            let fs = volumes.mount(volume)?.fs;
            let (entries, next) = list(&*fs, path, cursor.as_ref(), *limit)?;
            s.reply(&Reply::Page { entries, next })
        }
        Request::Stat { volume, path } => {
            let fs = volumes.mount(volume)?.fs;
            let meta = fs.stat(path).map_err(|e| fs_error(&e))?;
            let name = path.last().cloned().unwrap_or_else(root_name);
            s.reply(&Reply::Entry(entry(&*fs, path, name, meta)))
        }
        Request::Read { volume, path } => {
            let fs = volumes.mount(volume)?.fs;
            let meta = fs.stat(path).map_err(|e| fs_error(&e))?;
            if meta.attrs.kind != EntryKind::File {
                return Err(AgentError::new(ErrorCode::NotFile, "not a regular file").into());
            }
            let mut file = fs.open_file(path).map_err(|e| fs_error(&e))?;
            s.reply(&Reply::FileHead {
                size: meta.attrs.size,
            })?;
            match copy_data(&mut *file, meta.attrs.size, s, opts) {
                Ok(()) => Ok(()),
                Err(CopyErr::Stop(stop)) => Err(stop),
                Err(CopyErr::Read(msg)) => Err(AgentError::new(ErrorCode::Io, msg).into()),
            }
        }
        Request::Walk {
            volume,
            paths,
            hardlinks,
        } => {
            if paths.len() > MAX_WALK_PATHS {
                return Err(bad_request("too many paths").into());
            }
            let fs = volumes.mount(volume)?.fs;
            Walker {
                fs: &*fs,
                s,
                opts,
                hardlinks: *hardlinks,
                seen: HashMap::new(),
                entries: 0,
                bytes: 0,
            }
            .run(paths)
        }
        Request::Cancel { .. } | Request::Shutdown => Ok(()),
    }
}

/// 루트 항목의 이름(보이기용 — 경로 조각으로 되보내지 않는다)
fn root_name() -> Name {
    Name::new("(root)").expect("valid name")
}

fn entry(fs: &dyn Fs, path: &[Name], name: Name, meta: Meta) -> Entry {
    let mut attrs = meta.attrs;
    let link_target = if attrs.kind == EntryKind::Symlink {
        let t = fs::link_target(fs, path);
        if t.is_none() {
            // 대상을 읽지 못한 링크(너무 긺)는 `other`로 보인다 — 규약은 링크에 대상을 요구한다.
            attrs.kind = EntryKind::Other;
        }
        t
    } else {
        None
    };
    Entry {
        name,
        attrs,
        link_target,
    }
}

/// 이름 차례로 정렬한 한 쪽. 커서는 앞 쪽 마지막 이름(그보다 큰 이름부터).
fn list(
    fs: &dyn Fs,
    dir: &[Name],
    cursor: Option<&Name>,
    limit: u32,
) -> Result<(Vec<Entry>, Option<Name>), AgentError> {
    let limit = (limit as usize).clamp(1, MAX_PAGE_ENTRIES);
    let meta = fs.stat(dir).map_err(|e| fs_error(&e))?;
    if meta.attrs.kind != EntryKind::Dir {
        return Err(AgentError::new(ErrorCode::NotDir, "not a directory"));
    }
    let mut names = fs.read_dir(dir).map_err(|e| fs_error(&e))?;
    names.sort();
    names.dedup();
    let start = cursor.map_or(0, |c| names.partition_point(|n| n <= c));
    let page: Vec<Name> = names.iter().skip(start).take(limit).cloned().collect();
    let more = start + page.len() < names.len();
    let mut path = dir.to_vec();
    let entries = page
        .into_iter()
        .map(|name| {
            path.push(name.clone());
            let e = match fs.stat(&path) {
                Ok(meta) => entry(fs, &path, name, meta),
                // 나열과 stat 사이에 읽지 못한 항목(I/O 오류)은 `other`로 보인다.
                Err(_) => Entry {
                    name,
                    attrs: crate::proto::Attrs {
                        kind: EntryKind::Other,
                        size: 0,
                        mtime: 0,
                        mtime_nsec: 0,
                        mode: 0,
                        uid: 0,
                        gid: 0,
                        nlink: 0,
                    },
                    link_target: None,
                },
            };
            path.pop();
            e
        })
        .collect::<Vec<_>>();
    let next = if more {
        entries.last().map(|e| e.name.clone())
    } else {
        None
    };
    Ok((entries, next))
}

enum CopyErr {
    Stop(Stop),
    /// 읽기 오류(보낸 바이트가 선언한 크기보다 적음)
    Read(String),
}

/// 파일 데이터를 정확히 `size` 바이트 보낸다(파일이 그보다 길면 거기서 멈춘다, 짧으면 오류).
fn copy_data<W: Write>(
    file: &mut dyn Read,
    size: u64,
    s: &mut Stream<'_, W>,
    opts: &ServeOptions,
) -> Result<(), CopyErr> {
    let chunk = opts.chunk.clamp(1, MAX_PAYLOAD);
    let mut buf = vec![
        0u8;
        chunk
            .min(usize::try_from(size).unwrap_or(usize::MAX))
            .max(1)
    ];
    let mut left = size;
    while left > 0 {
        s.check_cancel().map_err(CopyErr::Stop)?;
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = match file.read(&mut buf[..want]) {
            Ok(0) => {
                return Err(CopyErr::Read(
                    "the file is shorter than its size".to_string(),
                ));
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(CopyErr::Read(short(format!("read error: {e}")))),
        };
        s.send(FrameKind::Data, buf[..n].to_vec())
            .map_err(CopyErr::Stop)?;
        left -= n as u64;
    }
    Ok(())
}

struct Walker<'a, 'b, W> {
    fs: &'a dyn Fs,
    s: &'a mut Stream<'b, W>,
    opts: &'a ServeOptions,
    hardlinks: bool,
    /// inode → 처음 보낸 아카이브 경로(하드 링크)
    seen: HashMap<u64, Vec<Name>>,
    entries: u64,
    bytes: u64,
}

impl<W: Write> Walker<'_, '_, W> {
    fn progress(&mut self) -> Result<(), Stop> {
        if self.s.last_sent.elapsed() >= self.opts.progress_every {
            let (entries, bytes) = (self.entries, self.bytes);
            self.s.reply(&Reply::Progress { entries, bytes })?;
        }
        Ok(())
    }

    fn skipped(&mut self, path: &[Name], reason: String) -> Result<(), Stop> {
        self.s.reply(&Reply::Skipped {
            path: path.to_vec(),
            reason: short(reason),
        })
    }

    fn run(mut self, paths: &[Vec<Name>]) -> Result<(), Stop> {
        // 같은 이름을 두 번 고르면 둘째는 건너뜀(아카이브 안 경로가 겹침)
        let mut top: HashSet<Name> = HashSet::new();
        for sel in paths {
            self.s.check_cancel()?;
            match sel.split_last() {
                Some((name, _)) => {
                    if !top.insert(name.clone()) {
                        self.skipped(
                            std::slice::from_ref(name),
                            "another selected item has the same name".to_string(),
                        )?;
                        continue;
                    }
                    self.visit(sel, vec![name.clone()])?;
                }
                // 루트를 고르면 그 아래 항목들
                None => {
                    let mut names = match self.fs.read_dir(&[]) {
                        Ok(n) => n,
                        Err(e) => return Err(fs_error(&e).into()),
                    };
                    names.sort();
                    for n in names {
                        if !top.insert(n.clone()) {
                            continue;
                        }
                        self.visit(std::slice::from_ref(&n), vec![n.clone()])?;
                    }
                }
            }
        }
        Ok(())
    }

    /// 깊이 우선(디렉터리 항목 뒤 그 아래). 재귀 대신 쌓기 — 깊이는 [`MAX_PATH_DEPTH`]까지.
    fn visit(&mut self, src: &[Name], arc: Vec<Name>) -> Result<(), Stop> {
        // (원본 경로, 아카이브 경로)
        let mut stack: Vec<(Vec<Name>, Vec<Name>)> = vec![(src.to_vec(), arc)];
        while let Some((src, arc)) = stack.pop() {
            self.s.check_cancel()?;
            self.progress()?;
            let meta = match self.fs.stat(&src) {
                Ok(m) => m,
                Err(e) => {
                    self.skipped(&arc, fs_error(&e).message)?;
                    continue;
                }
            };
            let mut item = Item {
                path: arc.clone(),
                attrs: meta.attrs,
                link_target: None,
                hardlink: None,
            };
            match meta.attrs.kind {
                EntryKind::Dir => {
                    let mut names = match self.fs.read_dir(&src) {
                        Ok(n) => n,
                        Err(e) => {
                            self.skipped(&arc, fs_error(&e).message)?;
                            continue;
                        }
                    };
                    self.entries += 1;
                    self.s.reply(&Reply::Item(item))?;
                    if arc.len() >= MAX_PATH_DEPTH {
                        if !names.is_empty() {
                            self.skipped(&arc, "directory is nested too deeply".to_string())?;
                        }
                        continue;
                    }
                    names.sort();
                    // 쌓기에서 이름 차례로 나오게 거꾸로 넣는다.
                    for n in names.into_iter().rev() {
                        let mut s2 = src.clone();
                        s2.push(n.clone());
                        let mut a2 = arc.clone();
                        a2.push(n);
                        stack.push((s2, a2));
                    }
                }
                EntryKind::Symlink => match fs::link_target(self.fs, &src) {
                    Some(t) => {
                        item.link_target = Some(t);
                        self.entries += 1;
                        self.s.reply(&Reply::Item(item))?;
                    }
                    None => self.skipped(
                        &arc,
                        "symbolic link target is unreadable or too long".to_string(),
                    )?,
                },
                EntryKind::Other => {
                    self.entries += 1;
                    self.s.reply(&Reply::Item(item))?;
                }
                EntryKind::File => {
                    if self.hardlinks
                        && meta.attrs.nlink > 1
                        && let Some(first) = self.seen.get(&meta.ino)
                    {
                        item.hardlink = Some(first.clone());
                        self.entries += 1;
                        self.s.reply(&Reply::Item(item))?;
                        continue;
                    }
                    let mut file = match self.fs.open_file(&src) {
                        Ok(f) => f,
                        Err(e) => {
                            self.skipped(&arc, fs_error(&e).message)?;
                            continue;
                        }
                    };
                    if self.hardlinks && meta.attrs.nlink > 1 {
                        self.seen.insert(meta.ino, arc.clone());
                    }
                    let size = meta.attrs.size;
                    self.entries += 1;
                    self.s.reply(&Reply::Item(item))?;
                    match copy_data(&mut *file, size, self.s, self.opts) {
                        Ok(()) => self.bytes += size,
                        Err(CopyErr::Stop(stop)) => return Err(stop),
                        Err(CopyErr::Read(message)) => {
                            // 하드 링크 원본이 깨졌으면 뒤 링크가 가리키지 않게 뺀다.
                            self.seen.retain(|_, p| *p != arc);
                            self.s.reply(&Reply::FileError { message })?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
