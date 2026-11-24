//! 저장소 백업 디스크를 NBD 내보내기의 원본으로(설계 §11.1): 오프셋 → 청크 번호 → id → 캐시(풀린 청크 LRU) →
//! 저장소 GET(동시 `max_streams`, 같은 id 요청은 하나로 묶음). 연속 읽기를 보면 다음 청크 넷을 미리 읽는다.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::future::try_join_all;
use opl_types::backup_store::{ChunkId, ZERO_CHUNK};
use tokio::sync::{OnceCell, Semaphore};

use super::client::Extent;
use super::server::{ExportSource, SourceError};
use super::{STATE_HOLE, STATE_ZERO};
use crate::format::index::FixedIndex;
use crate::store::Store;

/// 연속 읽기를 보면 미리 읽는 청크 수
pub const PREFETCH: u64 = 4;

type Chunk = Arc<Vec<u8>>;
type Pending = Arc<OnceCell<Result<Chunk, SourceError>>>;

/// 풀린 청크의 LRU 캐시(내보내기들이 함께 쓴다)와 저장소 읽기 동시 한도
pub struct ChunkCache {
    capacity: u64,
    fetch: Semaphore,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    map: HashMap<ChunkId, (Chunk, u64)>,
    order: BTreeMap<u64, ChunkId>,
    tick: u64,
    bytes: u64,
    pending: HashMap<ChunkId, Pending>,
}

impl Inner {
    fn touch(&mut self, id: &ChunkId) -> Option<Chunk> {
        self.tick += 1;
        let tick = self.tick;
        let (c, t) = self.map.get_mut(id)?;
        self.order.remove(t);
        *t = tick;
        self.order.insert(tick, *id);
        Some(c.clone())
    }

    fn insert(&mut self, id: ChunkId, c: Chunk, capacity: u64) {
        if self.map.contains_key(&id) {
            return;
        }
        self.tick += 1;
        self.bytes += c.len() as u64;
        self.order.insert(self.tick, id);
        self.map.insert(id, (c, self.tick));
        while self.bytes > capacity {
            let Some((_, old)) = self.order.pop_first() else {
                break;
            };
            if let Some((c, _)) = self.map.remove(&old) {
                self.bytes -= c.len() as u64;
            }
        }
    }
}

impl ChunkCache {
    /// `capacity` 바이트까지 담고 저장소 읽기는 동시에 `streams`개까지
    pub fn new(capacity: u64, streams: usize) -> Self {
        ChunkCache {
            capacity,
            fetch: Semaphore::new(streams.max(1)),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// 담긴 청크 수(시험)
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 청크 `id`(풀린 길이 `len`)를 캐시나 저장소에서
    pub async fn get(&self, store: &Store, id: &ChunkId, len: usize) -> Result<Chunk, SourceError> {
        let cell = {
            let mut g = self.inner.lock().unwrap();
            if let Some(c) = g.touch(id) {
                return Ok(c);
            }
            g.pending.entry(*id).or_default().clone()
        };
        let r = cell
            .get_or_init(|| async {
                let _permit = self.fetch.acquire().await.expect("semaphore open");
                store
                    .get_chunk(id, len)
                    .await
                    .map(Arc::new)
                    .map_err(|e| SourceError::io(e.to_string()))
            })
            .await
            .clone();
        let mut g = self.inner.lock().unwrap();
        // 같은 셀을 기다린 다른 요청이 이미 치웠을 수 있다.
        if g.pending.get(id).is_some_and(|p| Arc::ptr_eq(p, &cell)) {
            g.pending.remove(id);
        }
        if let Ok(c) = &r {
            g.insert(*id, c.clone(), self.capacity);
        }
        r
    }
}

/// 저장소 백업의 디스크 하나(고정 인덱스)
pub struct StoreExport {
    store: Arc<Store>,
    ids: Arc<Vec<ChunkId>>,
    size: u64,
    chunk: u64,
    cache: Arc<ChunkCache>,
    /// 앞 읽기가 끝난 청크 번호(연속 읽기 알아보기)
    last: Mutex<Option<u64>>,
}

impl StoreExport {
    pub fn new(store: Arc<Store>, index: FixedIndex, cache: Arc<ChunkCache>) -> Self {
        StoreExport {
            store,
            size: index.virtual_bytes,
            chunk: index.chunk_bytes as u64,
            ids: Arc::new(index.ids),
            cache,
            last: Mutex::new(None),
        }
    }

    fn chunk_len(size: u64, chunk: u64, n: u64) -> usize {
        (size - n * chunk).min(chunk) as usize
    }

    fn prefetch(&self, from: u64) {
        let count = self.ids.len() as u64;
        for n in from..(from + PREFETCH).min(count) {
            let id = self.ids[n as usize];
            if id == ZERO_CHUNK {
                continue;
            }
            let (store, cache) = (self.store.clone(), self.cache.clone());
            let len = Self::chunk_len(self.size, self.chunk, n);
            tokio::spawn(async move {
                let _ = cache.get(&store, &id, len).await;
            });
        }
    }
}

#[async_trait]
impl ExportSource for StoreExport {
    fn size(&self) -> u64 {
        self.size
    }

    async fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, SourceError> {
        let end = offset + len as u64;
        let first = offset / self.chunk;
        let last = (end - 1) / self.chunk;
        let sequential = {
            let mut g = self.last.lock().unwrap();
            let seq = g.is_some_and(|l| l == first || l + 1 == first);
            *g = Some(last);
            seq
        };
        if sequential {
            self.prefetch(last + 1);
        }
        let parts = try_join_all((first..=last).map(|n| {
            let id = self.ids[n as usize];
            let clen = Self::chunk_len(self.size, self.chunk, n);
            async move {
                if id == ZERO_CHUNK {
                    Ok(None)
                } else {
                    self.cache.get(&self.store, &id, clen).await.map(Some)
                }
            }
        }))
        .await?;
        let mut out = vec![0u8; len as usize];
        for (n, part) in (first..=last).zip(parts) {
            let Some(data) = part else { continue };
            let start = n * self.chunk;
            let from = offset.max(start);
            let to = end.min(start + data.len() as u64);
            out[(from - offset) as usize..(to - offset) as usize]
                .copy_from_slice(&data[(from - start) as usize..(to - start) as usize]);
        }
        Ok(out)
    }

    async fn allocation(&self, offset: u64, len: u32) -> Result<Vec<Extent>, SourceError> {
        Ok(allocation(
            &self.ids, self.chunk, self.size, offset, len as u64,
        ))
    }
}

/// 인덱스 `ids`에서 `offset..offset+len`의 `base:allocation` 범위(0 청크 = 구멍·0)
pub fn allocation(ids: &[ChunkId], chunk: u64, size: u64, offset: u64, len: u64) -> Vec<Extent> {
    let end = (offset + len).min(size);
    let mut out: Vec<Extent> = Vec::new();
    let mut at = offset;
    while at < end {
        let n = at / chunk;
        let stop = ((n + 1) * chunk).min(end);
        let flags = if ids[n as usize] == ZERO_CHUNK {
            STATE_HOLE | STATE_ZERO
        } else {
            0
        };
        match out.last_mut() {
            Some(e) if e.flags == flags => e.length += stop - at,
            _ => out.push(Extent {
                length: stop - at,
                flags,
            }),
        }
        at = stop;
    }
    out
}

/// 메모리의 디스크 이미지(가짜 하이퍼바이저가 fleecing 내보내기 대신 연다). 64 KiB마다 모두 0이면 구멍·0으로 알리고,
/// `bitmaps`의 문맥(`qemu:dirty-bitmap:<이름>`)은 주어진 범위만 바뀜([`super::STATE_DIRTY`])으로 알린다.
pub struct MemoryExport {
    data: Vec<u8>,
    bitmaps: BTreeMap<String, Vec<(u64, u64)>>,
}

impl MemoryExport {
    /// `bitmaps`: 문맥 이름 → 바뀐 (시작, 길이) 범위들
    pub fn new(data: Vec<u8>, bitmaps: BTreeMap<String, Vec<(u64, u64)>>) -> Self {
        MemoryExport { data, bitmaps }
    }
}

/// `offset..offset+len`을 `granule` 칸으로 나눠 `flag(칸 시작, 칸 끝)`으로 상태를 매기고 이웃한 같은 상태를 합친다.
fn runs(offset: u64, len: u64, granule: u64, flag: impl Fn(u64, u64) -> u32) -> Vec<Extent> {
    let end = offset + len;
    let mut out: Vec<Extent> = Vec::new();
    let mut at = offset;
    while at < end {
        let stop = ((at / granule + 1) * granule).min(end);
        let flags = flag(at, stop);
        match out.last_mut() {
            Some(e) if e.flags == flags => e.length += stop - at,
            _ => out.push(Extent {
                length: stop - at,
                flags,
            }),
        }
        at = stop;
    }
    out
}

#[async_trait]
impl ExportSource for MemoryExport {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    async fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, SourceError> {
        Ok(self.data[offset as usize..offset as usize + len as usize].to_vec())
    }

    async fn allocation(&self, offset: u64, len: u32) -> Result<Vec<Extent>, SourceError> {
        Ok(runs(offset, len as u64, 65536, |a, b| {
            if self.data[a as usize..b as usize].iter().all(|x| *x == 0) {
                STATE_HOLE | STATE_ZERO
            } else {
                0
            }
        }))
    }

    fn extra_contexts(&self) -> Vec<String> {
        self.bitmaps.keys().cloned().collect()
    }

    async fn context(&self, name: &str, offset: u64, len: u32) -> Result<Vec<Extent>, SourceError> {
        let ranges = self.bitmaps.get(name).ok_or_else(|| SourceError {
            code: super::EINVAL,
            message: format!("no meta context {name}"),
        })?;
        Ok(runs(offset, len as u64, 512, |a, b| {
            let dirty = ranges.iter().any(|(s, l)| *s < b && a < s + l);
            if dirty { super::STATE_DIRTY } else { 0 }
        }))
    }
}

/// 쓰기를 받는 메모리 내보내기(시험의 가짜 대상 볼륨 — 계획 72 VMware 이전이 `qemu-nbd` 없이 쓴다). 바이트는 함께 쥔
/// `Arc<Mutex<Vec<u8>>>`에 있어 시험이 쓴 뒤 읽는다. `WRITE_ZEROES`는 0으로 채우고 셌다(`zeroed`).
pub struct MemoryTarget {
    data: Arc<Mutex<Vec<u8>>>,
    zeroed: Arc<std::sync::atomic::AtomicU64>,
    flushes: Arc<std::sync::atomic::AtomicU64>,
}

impl MemoryTarget {
    pub fn new(data: Arc<Mutex<Vec<u8>>>) -> Self {
        MemoryTarget {
            data,
            zeroed: Arc::default(),
            flushes: Arc::default(),
        }
    }

    /// `WRITE_ZEROES`로 0을 쓴 바이트 수
    pub fn zeroed(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.zeroed.clone()
    }

    /// `FLUSH` 수
    pub fn flushes(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.flushes.clone()
    }

    fn bytes(&self) -> std::sync::MutexGuard<'_, Vec<u8>> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl ExportSource for MemoryTarget {
    fn size(&self) -> u64 {
        self.bytes().len() as u64
    }

    async fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, SourceError> {
        Ok(self.bytes()[offset as usize..offset as usize + len as usize].to_vec())
    }

    async fn allocation(&self, offset: u64, len: u32) -> Result<Vec<Extent>, SourceError> {
        let data = self.bytes();
        Ok(runs(offset, len as u64, 65536, |a, b| {
            if data[a as usize..b as usize].iter().all(|x| *x == 0) {
                STATE_HOLE | STATE_ZERO
            } else {
                0
            }
        }))
    }

    fn writable(&self) -> bool {
        true
    }

    async fn write(&self, offset: u64, data: &[u8]) -> Result<(), SourceError> {
        let at = offset as usize;
        self.bytes()[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    async fn write_zeroes(&self, offset: u64, len: u32) -> Result<(), SourceError> {
        let at = offset as usize;
        self.bytes()[at..at + len as usize].fill(0);
        self.zeroed
            .fetch_add(u64::from(len), std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn flush(&self) -> Result<(), SourceError> {
        self.flushes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}
