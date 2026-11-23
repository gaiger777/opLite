//! 로컬 백엔드(설계 §7.6): 백업 풀 안 디렉터리 `{풀 루트}/store/{저장소 id}/`. 쓰기는 `{이름}.tmp.<무작위>`에 쓰고
//! `sync_all` → `rename` → 디렉터리 fsync(지금 백업 매니페스트와 같은 쓰기 보장). 시각은 파일의 mtime이다.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use super::{Backend, ListPage, ObjectInfo, ObjectMeta, StoreError, check_key, check_prefix};

/// 로컬 디스크 동시 작업 수
const LOCAL_STREAMS: usize = 8;
/// 나열 한 쪽의 크기. 쪽마다 `chunks/`(디렉터리 65,536개)를 다시 읽으므로 S3보다 크게 잡는다.
const LOCAL_PAGE: usize = 10_000;

#[derive(Debug, Clone)]
pub struct LocalBackend {
    root: Arc<PathBuf>,
    page: usize,
}

fn io_err(what: &str, path: &Path, e: std::io::Error) -> StoreError {
    StoreError::Io(format!("{what} {}: {e}", path.display()))
}

fn mtime(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn tmp_name(path: &Path) -> PathBuf {
    let mut r = [0u8; 8];
    getrandom::fill(&mut r).expect("system random source");
    let mut name = path.file_name().expect("object has a name").to_os_string();
    name.push(format!(".tmp.{}", hex::encode(r)));
    path.with_file_name(name)
}

fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io_err("cannot sync directory", dir, e))
}

/// 임시 파일에 쓰고 fsync. 부모 디렉터리가 없으면 만든다.
fn write_tmp(path: &Path, data: &[u8]) -> Result<PathBuf, StoreError> {
    let tmp = tmp_name(path);
    let open = || {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
    };
    // 디렉터리가 없으면 만든다. 같은 때 지우기가 빈 디렉터리를 치웠으면(만든 뒤 열기 전) 몇 번 다시 한다(리뷰 1 M6).
    let mut tries = 0;
    let mut f = loop {
        match open() {
            Err(e) if e.kind() == ErrorKind::NotFound && tries < 3 => {
                tries += 1;
                let parent = path.parent().expect("object has a parent");
                fs::create_dir_all(parent).map_err(|e| io_err("cannot create", parent, e))?;
            }
            r => break r,
        }
    }
    .map_err(|e| io_err("cannot create", &tmp, e))?;
    let res = f.write_all(data).and_then(|_| f.sync_all());
    if let Err(e) = res {
        let _ = fs::remove_file(&tmp);
        return Err(io_err("cannot write", &tmp, e));
    }
    Ok(tmp)
}

impl LocalBackend {
    /// `root`는 저장소 디렉터리(`{풀 루트}/store/{저장소 id}`). 없으면 [`Backend::prepare`]가 만든다.
    pub fn new(root: impl Into<PathBuf>) -> LocalBackend {
        LocalBackend {
            root: Arc::new(root.into()),
            page: LOCAL_PAGE,
        }
    }

    /// 나열 쪽 크기(시험)
    pub fn with_page_size(mut self, n: usize) -> LocalBackend {
        self.page = n.max(1);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, key: &str) -> Result<PathBuf, StoreError> {
        check_key(key)?;
        Ok(self.root.join(key))
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&LocalBackend) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || f(&me))
            .await
            .map_err(|e| StoreError::Io(format!("store task failed: {e}")))?
    }

    fn list_sync(&self, prefix: &str, token: Option<&str>) -> Result<ListPage, StoreError> {
        // 접두사의 마지막 `/`까지가 디렉터리, 나머지는 이름 앞부분
        let (dir, _) = prefix.rsplit_once('/').unwrap_or(("", prefix));
        let base = if dir.is_empty() {
            self.root.to_path_buf()
        } else {
            self.root.join(dir)
        };
        let rel = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let after = token.filter(|t| *t >= prefix);
        let mut out = Vec::new();
        let more = self.walk(&base, &rel, prefix, after, &mut out)?;
        let next = if more {
            out.last().map(|o| o.key.clone())
        } else {
            None
        };
        Ok(ListPage { objects: out, next })
    }

    /// 디렉터리를 이름 차례(디렉터리는 `이름/`로 견줌 — 객체 이름 차례와 같게)로 돈다. 쪽이 차서 멈췄으면 참.
    fn walk(
        &self,
        dir: &Path,
        rel: &str,
        prefix: &str,
        after: Option<&str>,
        out: &mut Vec<ObjectInfo>,
    ) -> Result<bool, StoreError> {
        let rd = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io_err("cannot list", dir, e)),
        };
        let mut entries = Vec::new();
        for e in rd {
            let e = e.map_err(|e| io_err("cannot list", dir, e))?;
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            let ft = e.file_type().map_err(|e| io_err("cannot list", dir, e))?;
            let key = if ft.is_dir() {
                format!("{rel}{name}/")
            } else if ft.is_file() {
                format!("{rel}{name}")
            } else {
                continue;
            };
            entries.push((key, ft.is_dir()));
        }
        entries.sort();
        for (key, is_dir) in entries {
            if is_dir {
                // 접두사와 겹치지 않는 디렉터리, 이미 지난 디렉터리는 건너뜀
                if !(key.starts_with(prefix) || prefix.starts_with(&key)) {
                    continue;
                }
                if let Some(a) = after
                    && a > key.as_str()
                    && !a.starts_with(&key)
                {
                    continue;
                }
                let sub = dir.join(key[rel.len()..].trim_end_matches('/'));
                if self.walk(&sub, &key, prefix, after, out)? {
                    return Ok(true);
                }
            } else {
                if !key.starts_with(prefix) || after.is_some_and(|a| key.as_str() <= a) {
                    continue;
                }
                if out.len() == self.page {
                    return Ok(true);
                }
                let path = dir.join(&key[rel.len()..]);
                let meta = match fs::metadata(&path) {
                    Ok(m) => m,
                    // 나열과 읽기 사이에 지워졌다
                    Err(e) if e.kind() == ErrorKind::NotFound => continue,
                    Err(e) => return Err(io_err("cannot stat", &path, e)),
                };
                out.push(ObjectInfo {
                    key,
                    size: meta.len(),
                    modified: mtime(&meta),
                });
            }
        }
        Ok(false)
    }
}

#[async_trait]
impl Backend for LocalBackend {
    fn describe(&self) -> String {
        format!("directory {}", self.root.display())
    }

    fn max_streams(&self) -> usize {
        LOCAL_STREAMS
    }

    async fn prepare(&self) -> Result<(), StoreError> {
        self.blocking(|me| {
            let chunks = me.root.join("chunks");
            fs::create_dir_all(&chunks).map_err(|e| io_err("cannot create", &chunks, e))?;
            for i in 0..=0xffffu32 {
                let d = chunks.join(format!("{i:04x}"));
                match fs::create_dir(&d) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(io_err("cannot create", &d, e)),
                }
            }
            sync_dir(&chunks)?;
            sync_dir(&me.root)
        })
        .await
    }

    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), StoreError> {
        let path = self.path(key)?;
        self.blocking(move |_| {
            let tmp = write_tmp(&path, &data)?;
            fs::rename(&tmp, &path).map_err(|e| {
                let _ = fs::remove_file(&tmp);
                io_err("cannot rename to", &path, e)
            })?;
            sync_dir(path.parent().expect("object has a parent"))
        })
        .await
    }

    async fn put_if_absent(&self, key: &str, data: Vec<u8>) -> Result<bool, StoreError> {
        let path = self.path(key)?;
        self.blocking(move |_| {
            if path.exists() {
                return Ok(false);
            }
            let tmp = write_tmp(&path, &data)?;
            // 하드 링크는 대상이 있으면 실패한다(NFS에서도 원자적)
            let res = fs::hard_link(&tmp, &path);
            let _ = fs::remove_file(&tmp);
            match res {
                Ok(()) => {
                    sync_dir(path.parent().expect("object has a parent"))?;
                    Ok(true)
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(io_err("cannot link", &path, e)),
            }
        })
        .await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.path(key)?;
        let k = key.to_string();
        self.blocking(move |_| match fs::read(&path) {
            Ok(d) => Ok(d),
            Err(e) if e.kind() == ErrorKind::NotFound => Err(StoreError::NotFound(k)),
            Err(e) => Err(io_err("cannot read", &path, e)),
        })
        .await
    }

    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, StoreError> {
        let path = self.path(key)?;
        self.blocking(move |_| match fs::metadata(&path) {
            Ok(m) if m.is_file() => Ok(Some(ObjectMeta {
                size: m.len(),
                modified: mtime(&m),
            })),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err("cannot stat", &path, e)),
        })
        .await
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        let path = self.path(key)?;
        let snapshot = key.starts_with("snapshots/");
        let root = self.root.clone();
        self.blocking(move |_| {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(io_err("cannot remove", &path, e)),
            }
            // 빈 스냅샷 디렉터리(`snapshots/<vm>/<backup>`, `snapshots/<vm>`)를 치운다(비어 있지 않으면 그대로)
            if snapshot {
                let stop = root.join("snapshots");
                let mut dir = path.parent().map(Path::to_path_buf);
                while let Some(d) = dir {
                    if d == stop || !d.starts_with(&stop) || fs::remove_dir(&d).is_err() {
                        break;
                    }
                    dir = d.parent().map(Path::to_path_buf);
                }
            }
            Ok(())
        })
        .await
    }

    async fn list(&self, prefix: &str, token: Option<String>) -> Result<ListPage, StoreError> {
        check_prefix(prefix)?;
        let prefix = prefix.to_string();
        self.blocking(move |me| me.list_sync(&prefix, token.as_deref()))
            .await
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), StoreError> {
        let src = self.path(from)?;
        let dst = self.path(to)?;
        let k = from.to_string();
        self.blocking(move |_| {
            if !src.exists() {
                return Err(StoreError::NotFound(k));
            }
            let parent = dst.parent().expect("object has a parent");
            fs::create_dir_all(parent).map_err(|e| io_err("cannot create", parent, e))?;
            fs::rename(&src, &dst).map_err(|e| io_err("cannot rename", &src, e))?;
            sync_dir(parent)?;
            sync_dir(src.parent().expect("object has a parent"))
        })
        .await
    }

    async fn touch(&self, key: &str) -> Result<(), StoreError> {
        let path = self.path(key)?;
        let k = key.to_string();
        self.blocking(move |_| {
            let f = match fs::OpenOptions::new().write(true).open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == ErrorKind::NotFound => return Err(StoreError::NotFound(k)),
                Err(e) => return Err(io_err("cannot open", &path, e)),
            };
            f.set_modified(std::time::SystemTime::now())
                .map_err(|e| io_err("cannot touch", &path, e))
        })
        .await
    }
}
