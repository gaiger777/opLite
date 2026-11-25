//! 에이전트가 읽는 파일시스템(설계 §9.1). 요청 처리 몸통은 [`Fs`] 위에 짠다: 도우미 VM에서는 마운트한 디렉터리
//! ([`DirFs`]), 호스트 시험에서는 임시 디렉터리([`DirFs`])나 메모리 나무([`MemFs`] — UTF-8이 아닌 이름·장치 파일·
//! 읽기 오류처럼 호스트에서 만들기 어려운 것). 경로는 늘 검사한 [`Name`] 목록이고 어느 조각도 심볼릭 링크를 따라가지
//! 않는다 — 볼륨 루트 밖으로 나갈 길이 없다.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::Mutex;

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use crate::proto::{Attrs, EntryKind, LinkTarget, Name};

/// 항목 속성과 하드 링크 판단용 inode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub attrs: Attrs,
    pub ino: u64,
}

/// 에이전트가 읽는 파일시스템 하나(볼륨 루트). 경로는 루트에서 시작하는 이름 목록이다.
pub trait Fs: Send + Sync {
    /// 디렉터리의 이름들(`.`·`..` 빼고, 차례는 아무렇게나)
    fn read_dir(&self, dir: &[Name]) -> io::Result<Vec<Name>>;
    /// 항목 속성(마지막 조각이 심볼릭 링크면 링크 자체)
    fn stat(&self, path: &[Name]) -> io::Result<Meta>;
    /// 심볼릭 링크 대상
    fn read_link(&self, path: &[Name]) -> io::Result<Vec<u8>>;
    /// 일반 파일 읽기(다른 종류면 오류)
    fn open_file(&self, path: &[Name]) -> io::Result<Box<dyn Read + Send>>;
}

/// 링크 대상을 규약 한도 안으로(넘으면 없음 — 링크 자체는 항목으로 보인다)
pub(crate) fn link_target(fs: &dyn Fs, path: &[Name]) -> Option<LinkTarget> {
    fs.read_link(path).ok().and_then(LinkTarget::new)
}

fn not_file() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
}

// ---------------------------------------------------------------------------------------------------------------
// 디렉터리
// ---------------------------------------------------------------------------------------------------------------

/// 디렉터리 하나를 루트로 하는 [`Fs`]. 조각마다 `openat(O_NOFOLLOW|O_DIRECTORY)`로 내려가므로(조각에는 `/`·`..`가
/// 없다) 심볼릭 링크를 지나 루트 밖으로 나가지 못한다 — Linux의 `openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS)`와
/// 같은 뜻을 macOS에서도 같은 코드로 시험한다.
#[derive(Debug)]
pub struct DirFs {
    root: OwnedFd,
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

impl DirFs {
    pub fn open(root: &Path) -> io::Result<Self> {
        let root = rustix::fs::open(root, dir_flags(), Mode::empty())?;
        Ok(DirFs { root })
    }

    /// 디렉터리 `parts`의 fd(조각마다 링크를 따라가지 않음)
    fn dir(&self, parts: &[Name]) -> io::Result<OwnedFd> {
        let mut fd = rustix::fs::openat(&self.root, c".", dir_flags(), Mode::empty())?;
        for p in parts {
            fd = rustix::fs::openat(&fd, p.as_bytes(), dir_flags(), Mode::empty())?;
        }
        Ok(fd)
    }

    /// (부모 fd, 마지막 이름). 루트는 없음
    fn parent<'a>(&self, path: &'a [Name]) -> io::Result<Option<(OwnedFd, &'a Name)>> {
        match path.split_last() {
            None => Ok(None),
            Some((last, parent)) => Ok(Some((self.dir(parent)?, last))),
        }
    }
}

/// `stat` 칸의 타입이 플랫폼마다 달라(macOS `u16` 모드·`i64` 나노초, Linux `u32`·`u64`) `as`로 맞춘다.
#[allow(clippy::unnecessary_cast)]
fn meta_from_stat(st: &rustix::fs::Stat) -> Meta {
    let kind = match FileType::from_raw_mode(st.st_mode as _) {
        FileType::RegularFile => EntryKind::File,
        FileType::Directory => EntryKind::Dir,
        FileType::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    let nsec = st.st_mtime_nsec as i64;
    Meta {
        attrs: Attrs {
            kind,
            size: if kind == EntryKind::File {
                st.st_size.max(0) as u64
            } else {
                0
            },
            mtime: st.st_mtime as i64,
            mtime_nsec: if (0..1_000_000_000).contains(&nsec) {
                nsec as u32
            } else {
                0
            },
            mode: (st.st_mode as u32) & 0o7777,
            uid: st.st_uid as u32,
            gid: st.st_gid as u32,
            nlink: (st.st_nlink as u64).min(u64::from(u32::MAX)) as u32,
        },
        ino: st.st_ino as u64,
    }
}

impl Fs for DirFs {
    fn read_dir(&self, dir: &[Name]) -> io::Result<Vec<Name>> {
        let fd = self.dir(dir)?;
        let mut names = Vec::new();
        for entry in rustix::fs::Dir::read_from(&fd)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            // 커널이 준 이름은 늘 맞는 조각이다(길이 255 안, `/`·NUL 없음).
            if let Ok(n) = Name::new(name) {
                names.push(n);
            }
        }
        Ok(names)
    }

    fn stat(&self, path: &[Name]) -> io::Result<Meta> {
        let st = match self.parent(path)? {
            None => rustix::fs::fstat(&self.root)?,
            Some((dir, last)) => {
                rustix::fs::statat(&dir, last.as_bytes(), AtFlags::SYMLINK_NOFOLLOW)?
            }
        };
        Ok(meta_from_stat(&st))
    }

    fn read_link(&self, path: &[Name]) -> io::Result<Vec<u8>> {
        let Some((dir, last)) = self.parent(path)? else {
            return Err(Errno::INVAL.into());
        };
        let target = rustix::fs::readlinkat(&dir, last.as_bytes(), Vec::new())?;
        Ok(target.into_bytes())
    }

    fn open_file(&self, path: &[Name]) -> io::Result<Box<dyn Read + Send>> {
        let Some((dir, last)) = self.parent(path)? else {
            return Err(not_file());
        };
        // FIFO에서 막히지 않게 O_NONBLOCK, 링크는 O_NOFOLLOW(ELOOP). 연 뒤 종류를 다시 본다.
        let fd = rustix::fs::openat(
            &dir,
            last.as_bytes(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
            Mode::empty(),
        )?;
        let st = rustix::fs::fstat(fd.as_fd())?;
        if meta_from_stat(&st).attrs.kind != EntryKind::File {
            return Err(not_file());
        }
        Ok(Box::new(File::from(fd)))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 메모리 나무
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Node {
    Dir,
    File {
        data: std::sync::Arc<Vec<u8>>,
        /// 이 바이트 뒤 읽기 오류(EIO)
        fail_after: Option<usize>,
    },
    Symlink(Vec<u8>),
    Other,
}

#[derive(Debug, Clone)]
struct MemNode {
    node: Node,
    meta: Meta,
}

/// 메모리 안 파일시스템(시험과 opld의 가짜 도우미). 루트 디렉터리는 처음부터 있다. 하드 링크는 inode를 나눈 두 경로.
#[derive(Debug)]
pub struct MemFs {
    nodes: Mutex<BTreeMap<Vec<Name>, MemNode>>,
    next_ino: Mutex<u64>,
}

impl Default for MemFs {
    fn default() -> Self {
        Self::new()
    }
}

fn attrs(kind: EntryKind, size: u64, mode: u32) -> Attrs {
    Attrs {
        kind,
        size,
        mtime: 1_700_000_000,
        mtime_nsec: 0,
        mode,
        uid: 0,
        gid: 0,
        nlink: 1,
    }
}

/// `"a/b"` → 조각(시험 편의, 잘못된 조각은 패닉)
pub fn path_of(s: &str) -> Vec<Name> {
    s.split('/')
        .filter(|p| !p.is_empty())
        .map(|p| Name::new(p).expect("valid test name"))
        .collect()
}

impl MemFs {
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            Vec::new(),
            MemNode {
                node: Node::Dir,
                meta: Meta {
                    attrs: attrs(EntryKind::Dir, 0, 0o755),
                    ino: 1,
                },
            },
        );
        MemFs {
            nodes: Mutex::new(nodes),
            next_ino: Mutex::new(2),
        }
    }

    fn insert(&self, path: Vec<Name>, node: Node, mut a: Attrs) {
        assert!(!path.is_empty(), "the root exists");
        let mut nodes = self.nodes.lock().unwrap();
        // 부모 디렉터리를 만든다.
        for i in 1..path.len() {
            let parent = path[..i].to_vec();
            if let std::collections::btree_map::Entry::Vacant(e) = nodes.entry(parent) {
                e.insert(MemNode {
                    node: Node::Dir,
                    meta: Meta {
                        attrs: attrs(EntryKind::Dir, 0, 0o755),
                        ino: self.ino(),
                    },
                });
            }
        }
        if let Node::File { data, .. } = &node {
            a.size = data.len() as u64;
        }
        let ino = self.ino();
        nodes.insert(
            path,
            MemNode {
                node,
                meta: Meta { attrs: a, ino },
            },
        );
    }

    fn ino(&self) -> u64 {
        let mut n = self.next_ino.lock().unwrap();
        *n += 1;
        *n
    }

    /// 디렉터리(부모도 만듦)
    pub fn dir(&self, path: &[Name]) -> &Self {
        self.insert(path.to_vec(), Node::Dir, attrs(EntryKind::Dir, 0, 0o755));
        self
    }

    /// 일반 파일(모드 0644)
    pub fn file(&self, path: &[Name], data: impl Into<Vec<u8>>) -> &Self {
        self.file_with(path, data, None, attrs(EntryKind::File, 0, 0o644))
    }

    /// 일반 파일, 속성을 정함(크기는 내용 길이), `fail_after` 바이트 뒤 읽기 오류
    pub fn file_with(
        &self,
        path: &[Name],
        data: impl Into<Vec<u8>>,
        fail_after: Option<usize>,
        a: Attrs,
    ) -> &Self {
        let node = Node::File {
            data: std::sync::Arc::new(data.into()),
            fail_after,
        };
        self.insert(
            path.to_vec(),
            node,
            Attrs {
                kind: EntryKind::File,
                ..a
            },
        );
        self
    }

    pub fn symlink(&self, path: &[Name], target: impl Into<Vec<u8>>) -> &Self {
        self.insert(
            path.to_vec(),
            Node::Symlink(target.into()),
            attrs(EntryKind::Symlink, 0, 0o777),
        );
        self
    }

    /// 장치·FIFO 같은 특수 파일
    pub fn special(&self, path: &[Name]) -> &Self {
        self.insert(
            path.to_vec(),
            Node::Other,
            attrs(EntryKind::Other, 0, 0o600),
        );
        self
    }

    /// `path`를 `existing`(일반 파일)의 하드 링크로
    pub fn hardlink(&self, path: &[Name], existing: &[Name]) -> &Self {
        let mut nodes = self.nodes.lock().unwrap();
        let mut node = nodes.get(existing).expect("existing file").clone();
        node.meta.attrs.nlink += 1;
        let ino = node.meta.ino;
        let nlink = node.meta.attrs.nlink;
        for n in nodes.values_mut().filter(|n| n.meta.ino == ino) {
            n.meta.attrs.nlink = nlink;
        }
        nodes.insert(path.to_vec(), node);
        self
    }

    /// `path`의 모든 조상이 디렉터리인지(링크를 따라가지 않음)
    fn walk_to<'a>(
        nodes: &'a BTreeMap<Vec<Name>, MemNode>,
        path: &[Name],
    ) -> io::Result<&'a MemNode> {
        for i in 0..path.len() {
            match nodes.get(&path[..i]).map(|n| &n.node) {
                Some(Node::Dir) => {}
                Some(Node::Symlink(_)) => return Err(Errno::LOOP.into()),
                Some(_) => return Err(Errno::NOTDIR.into()),
                None => return Err(Errno::NOENT.into()),
            }
        }
        nodes.get(path).ok_or_else(|| Errno::NOENT.into())
    }
}

impl Fs for MemFs {
    fn read_dir(&self, dir: &[Name]) -> io::Result<Vec<Name>> {
        let nodes = self.nodes.lock().unwrap();
        match &Self::walk_to(&nodes, dir)?.node {
            Node::Dir => {}
            Node::Symlink(_) => return Err(Errno::LOOP.into()),
            _ => return Err(Errno::NOTDIR.into()),
        }
        Ok(nodes
            .keys()
            .filter(|k| k.len() == dir.len() + 1 && k.starts_with(dir))
            .map(|k| k[dir.len()].clone())
            .collect())
    }

    fn stat(&self, path: &[Name]) -> io::Result<Meta> {
        let nodes = self.nodes.lock().unwrap();
        Ok(Self::walk_to(&nodes, path)?.meta)
    }

    fn read_link(&self, path: &[Name]) -> io::Result<Vec<u8>> {
        let nodes = self.nodes.lock().unwrap();
        match &Self::walk_to(&nodes, path)?.node {
            Node::Symlink(t) => Ok(t.clone()),
            _ => Err(Errno::INVAL.into()),
        }
    }

    fn open_file(&self, path: &[Name]) -> io::Result<Box<dyn Read + Send>> {
        let nodes = self.nodes.lock().unwrap();
        match &Self::walk_to(&nodes, path)?.node {
            Node::File { data, fail_after } => Ok(Box::new(MemReader {
                data: data.clone(),
                pos: 0,
                fail_after: *fail_after,
            })),
            Node::Symlink(_) => Err(Errno::LOOP.into()),
            _ => Err(not_file()),
        }
    }
}

struct MemReader {
    data: std::sync::Arc<Vec<u8>>,
    pos: usize,
    fail_after: Option<usize>,
}

impl Read for MemReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let end = self.fail_after.unwrap_or(usize::MAX).min(self.data.len());
        if self.pos >= end && self.fail_after.is_some_and(|f| f < self.data.len()) {
            return Err(Errno::IO.into());
        }
        let n = buf.len().min(end - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
