//! 저장소 백업(계획 68 설계 §8·§10.1·§10.3·§12.2): VM 백업(실행 중이면 fleecing NBD, 꺼졌으면 `qemu-nbd`)을 4 MiB
//! 청크로 나눠 저장소에 쓰는 파이프라인과 커밋 확인, 저장소 백업의 복원(이 노드 안 NBD 서버 + `qemu-img convert`)·
//! 지우기, 옛 풀 백업 옮기기(`BackupConvert`).
//!
//! 쓰는 차례는 청크 → 블롭 → 인덱스 → 매니페스트다. 매니페스트를 쓰기 전에 저장소의 `committing`에 자기를 넣고(쓸기
//! 중이면 기다림), 시작 때 본 `sweep_epoch`가 바뀌었거나 시작 때 쓸기 중이었으면 "있음 확인"으로 건너뛴 청크를 다시
//! 확인해 사라진 것을 아직 열린 원본에서 다시 올린다(§10.3 (다)).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{StreamExt, TryStreamExt};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{
    FleeceRequest, NbdSource, nbd_chain_source, nbd_source, tpm_state_path, uefi_vars_path,
};
use opl_store::Store;
use opl_store::format::chunk::is_zero;
use opl_store::format::index::FixedIndex;
use opl_store::format::manifest::{
    CONFIG_BLOB, ConfigBlob, Manifest, ManifestBlob, TPM_BLOB, VARS_BLOB,
};
use opl_store::nbd::export::{ChunkCache, StoreExport};
use opl_store::nbd::{BASE_ALLOCATION, NbdClient, NbdServer, STATE_DIRTY, STATE_ZERO};
use opl_types::backup::{
    Backup, BackupDisk, BackupPhase, BackupSource, BackupSourceKind, BackupSpec, BackupStatus,
    BackupTarget, StoreBackupStats, VerifyState, backup_dir, bitmap_name, is_backup_bitmap,
};
use opl_types::backup_store::{
    BackupStore, BackupStoreSpec, BackupStoreStatus, CHUNK_BYTES, ChunkId, StorePhase, ZERO_CHUNK,
    chunk_count, chunk_ranges,
};
use opl_types::resource::Resource;
use opl_types::storage::{StoragePool, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use super::backup::{
    VARS_FILE, available_media, backup_root, get_backup, read_chain, read_manifest, ready_pool,
    regular_file,
};
use super::clone::{create_vm, made, volume_name};
use super::firmware::{current_tpm, place_private_bytes};
use super::snapshot::{
    check_local, check_settled, fetch_vars, get_vm, is_down, now_secs, ready_disks,
};
use super::{Ctx, mark_ready};
use crate::stores;

/// 디스크 하나를 동시에 읽는 NBD 연결 수
const READERS: usize = 4;
/// 읽어 둔 청크 수(읽기와 처리 사이)
const READ_AHEAD: usize = 4;

/// 이 노드에서 저장소 `name`을 연다(리소스와 함께).
pub(super) async fn open_store(ctx: &Ctx, name: &str) -> Result<(BackupStore, Arc<Store>), String> {
    let res = stores::get(&ctx.cluster, name)?;
    let store = stores::open(&ctx.cluster, &res, ctx.config.node, &ctx.config.mount_root).await?;
    Ok((res, Arc::new(store)))
}

/// 쓰기 가능한 준비된 저장소인지
fn check_writable(store: &BackupStore) -> Result<(), String> {
    if store.spec.read_only {
        return Err(format!("backup store {:?} is read-only", store.name));
    }
    if store.status.phase != StorePhase::Ready {
        return Err(format!("backup store {:?} is not ready", store.name));
    }
    Ok(())
}

/// 이 노드의 유닉스 소켓 경로(`{backup_run_dir}/{tag}-{id}.sock` — 경로 길이 한도 안)
pub(super) fn socket_path(ctx: &Ctx, tag: &str, id: Uuid) -> PathBuf {
    ctx.config
        .backup_run_dir
        .join(format!("{tag}-{}.sock", id.simple()))
}

// ---- 저장소 상태(쓰기 잠금) ----

/// [`update_store`]의 한 걸음
pub(super) enum Step {
    /// 바꾼 상태를 쓴다
    Write,
    /// 쓰지 않고 지금 상태를 돌려준다
    Skip,
    /// 기다렸다 다시 읽는다
    Wait,
}

/// 저장소 상태를 읽은 revision에서만 바꾼다(Raft CAS). `f`는 다시 불려도 같은 결과여야 한다(쓰기 응답을 잃으면
/// 다시 읽어 본다). 저장소가 지워졌으면 오류다.
pub(super) async fn update_store(
    ctx: &Ctx,
    id: Uuid,
    mut f: impl FnMut(&mut BackupStoreStatus) -> Result<Step, String>,
) -> Result<BackupStoreStatus, String> {
    loop {
        let store = ctx
            .cluster
            .get::<BackupStoreSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("backup store was deleted")?;
        let mut status = store.status.clone();
        match f(&mut status)? {
            Step::Skip => return Ok(store.status),
            Step::Wait => {
                tokio::time::sleep(ctx.config.store_poll).await;
                continue;
            }
            Step::Write => {}
        }
        match ctx
            .cluster
            .update_status_checked::<BackupStoreSpec>(id, store.revision, &status)
            .await
        {
            Ok(_) => return Ok(status),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(store = %id, error = %e, "cannot write backup store status; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 커밋 구간에 들어간다(§10.3): 쓸기 중(또는 검증이 깨진 청크를 옮기는 중 — 리뷰 1 I5)이면 끝날 때까지 기다리고 `committing`에 `backup`을 넣는다. 들어간 뒤의 상태.
async fn begin_commit(ctx: &Ctx, store: Uuid, backup: Uuid) -> Result<BackupStoreStatus, String> {
    update_store(ctx, store, |s| {
        if s.committing.contains(&backup) {
            Ok(Step::Skip)
        } else if s.sweeping || s.quarantining {
            Ok(Step::Wait)
        } else {
            s.committing.push(backup);
            Ok(Step::Write)
        }
    })
    .await
}

/// 커밋 구간에서 나온다(없으면 할 일이 없다).
pub(super) async fn end_commit(ctx: &Ctx, store: Uuid, backup: Uuid) -> Result<(), String> {
    update_store(ctx, store, |s| {
        if !s.committing.contains(&backup) {
            return Ok(Step::Skip);
        }
        s.committing.retain(|b| *b != backup);
        Ok(Step::Write)
    })
    .await
    .map(|_| ())
}

// ---- 파이프라인(§8.4) ----

/// 읽을 디스크 하나: 유닉스 소켓 NBD 내보내기와(증분이면) 바뀐 곳을 알리는 비트맵 문맥
#[derive(Debug, Clone)]
pub(super) struct DiskSource {
    pub socket: PathBuf,
    pub export: String,
    pub dirty: Option<String>,
}

/// 디스크 하나를 쓴 결과
struct DiskResult {
    index: FixedIndex,
    /// "있음 확인"으로 건너뛴(base에 없던) 청크와 그 청크 번호 — 커밋 확인
    skipped: Vec<(u64, ChunkId)>,
    /// base 인덱스에서 옮긴(있음을 보지 않은) 청크와 그 청크 번호 — 커밋이 `bad/`의 id와 견준다(리뷰 1 I5)
    inherited: Vec<(u64, ChunkId)>,
    read_bytes: u64,
    new_chunks: u64,
    new_bytes: u64,
}

/// 청크 하나를 어떻게 했나
enum Outcome {
    Zero,
    /// base의 같은 자리와 같다(읽었지만 쓰지 않음)
    Unchanged,
    /// 이번 실행에 이미 본 id
    Seen,
    /// 저장소에 이미 있다(남의 청크일 수 있음)
    Exists,
    /// 새로 올렸다(저장 바이트)
    New(u64),
}

fn nbd_err(e: impl std::fmt::Display) -> String {
    format!("cannot read the disk over NBD: {e}")
}

/// `extents`(`(길이, 상태)`)에서 `flag`가 선 범위를 `(오프셋, 길이)`로
fn flagged(extents: &[opl_store::nbd::Extent], flag: u32) -> Vec<(u64, u64)> {
    let mut at = 0;
    let mut out = Vec::new();
    for e in extents {
        if e.flags & flag != 0 {
            out.push((at, e.length));
        }
        at += e.length;
    }
    out
}

/// 범위들이 청크 전체를 덮는 청크 번호(0 범위 → 읽지 않을 청크)
fn covered_chunks(ranges: &[(u64, u64)], chunk: u64, size: u64) -> HashSet<u64> {
    let mut out = HashSet::new();
    for &(off, len) in ranges {
        let end = off + len;
        let mut n = off.div_ceil(chunk);
        while n * chunk < end {
            let chunk_end = ((n + 1) * chunk).min(size);
            if chunk_end > end {
                break;
            }
            out.insert(n);
            n += 1;
        }
    }
    out
}

/// 디스크 하나를 읽어 저장소에 쓴다(인덱스는 쓰지 않는다). base가 있고 비트맵 문맥이 있으면 바뀐 청크만 읽고 나머지는
/// base의 id를 옮긴다(§8.3). 0으로 알린 청크는 읽지 않는다.
async fn write_disk(
    store: &Arc<Store>,
    backup: Uuid,
    src: &DiskSource,
    base: Option<&FixedIndex>,
) -> Result<DiskResult, String> {
    let contexts: Vec<&str> = std::iter::once(BASE_ALLOCATION)
        .chain(src.dirty.as_deref())
        .collect();
    let mut main = NbdClient::connect(&src.socket, &src.export, &contexts)
        .await
        .map_err(nbd_err)?;
    let size = main.size();
    let chunk = CHUNK_BYTES;
    let count = chunk_count(size, chunk);
    let mut index = FixedIndex::new(backup, size, chunk as u32, store.encrypted());
    let zero = if size > 0 && main.has_context(BASE_ALLOCATION) {
        let ext = main
            .extents(BASE_ALLOCATION, 0, size)
            .await
            .map_err(nbd_err)?;
        covered_chunks(&flagged(&ext, STATE_ZERO), chunk, size)
    } else {
        HashSet::new()
    };
    let base = base.filter(|b| b.ids.len() as u64 == count);
    let dirty = src.dirty.as_deref().filter(|c| main.has_context(c));
    let wanted: Vec<u64> = match (base, dirty) {
        (Some(b), Some(ctx)) if size > 0 => {
            index.ids = b.ids.clone();
            let ext = main.extents(ctx, 0, size).await.map_err(nbd_err)?;
            chunk_ranges(&flagged(&ext, STATE_DIRTY), chunk)
        }
        _ => (0..count).collect(),
    };
    let _ = main.disconnect().await;
    let mut to_read = Vec::with_capacity(wanted.len());
    for n in wanted {
        if zero.contains(&n) {
            index.ids[n as usize] = ZERO_CHUNK;
        } else {
            to_read.push(n);
        }
    }
    let to_read = Arc::new(to_read);
    let base_ids: Arc<Vec<ChunkId>> = Arc::new(base.map(|b| b.ids.clone()).unwrap_or_default());

    // 읽는 쪽: 연결 READERS개가 번호를 나눠 읽어 보낸다.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<(u64, Vec<u8>), String>>(READ_AHEAD);
    let cursor = Arc::new(AtomicUsize::new(0));
    let mut readers = Vec::new();
    for _ in 0..READERS.min(to_read.len().max(1)) {
        let (tx, cursor, list) = (tx.clone(), cursor.clone(), to_read.clone());
        let (socket, export) = (src.socket.clone(), src.export.clone());
        readers.push(tokio::spawn(async move {
            let mut c = match NbdClient::connect(&socket, &export, &[]).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(nbd_err(e))).await;
                    return;
                }
            };
            loop {
                let k = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(&n) = list.get(k) else { break };
                let off = n * chunk;
                let len = (size - off).min(chunk) as u32;
                let got = c.read(off, len).await.map(|d| (n, d)).map_err(nbd_err);
                let failed = got.is_err();
                if tx.send(got).await.is_err() || failed {
                    return;
                }
            }
            let _ = c.disconnect().await;
        }));
    }
    drop(tx);

    // 처리하는 쪽: 0 검사 → id → base의 같은 자리 → 이번 실행에 본 id → 있음 확인 → 압축·암호화 → 올림
    let seen: Arc<Mutex<HashSet<ChunkId>>> = Arc::default();
    let workers = std::thread::available_parallelism()
        .map_or(2, |n| n.get() / 2)
        .clamp(1, 4);
    let concurrency = workers + store.max_streams();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let processed: Vec<(u64, ChunkId, Outcome, u64)> = stream
        .map(|item| {
            let (store, seen, base_ids) = (store.clone(), seen.clone(), base_ids.clone());
            async move {
                let (n, raw) = item?;
                let read = raw.len() as u64;
                if is_zero(&raw) {
                    return Ok((n, ZERO_CHUNK, Outcome::Zero, read));
                }
                let s = store.clone();
                let (id, raw) = tokio::task::spawn_blocking(move || (s.chunk_id(&raw), raw))
                    .await
                    .map_err(|e| e.to_string())?;
                if base_ids.get(n as usize) == Some(&id) {
                    return Ok((n, id, Outcome::Unchanged, read));
                }
                if !seen.lock().unwrap().insert(id) {
                    return Ok((n, id, Outcome::Seen, read));
                }
                let name = |e| stores::store_error("", e);
                if store.has_chunk(&id).await.map_err(name)? {
                    return Ok((n, id, Outcome::Exists, read));
                }
                let s = store.clone();
                let encoded = tokio::task::spawn_blocking(move || s.encode_chunk(&raw))
                    .await
                    .map_err(|e| e.to_string())?;
                store.put_chunk(&encoded).await.map_err(name)?;
                Ok::<_, String>((n, id, Outcome::New(encoded.data.len() as u64), read))
            }
        })
        .buffer_unordered(concurrency)
        .try_collect()
        .await?;
    for r in readers {
        let _ = r.await;
    }
    if processed.len() != to_read.len() {
        return Err("the disk was not read completely".into());
    }
    // base에서 옮긴 자리(읽지 않은 것): 아래에서 읽은 자리는 덮인다.
    let read: HashSet<u64> = processed.iter().map(|p| p.0).collect();
    let inherited: Vec<(u64, ChunkId)> = index
        .ids
        .iter()
        .enumerate()
        .filter(|(n, id)| **id != ZERO_CHUNK && !read.contains(&(*n as u64)))
        .map(|(n, id)| (n as u64, *id))
        .collect();
    let mut out = DiskResult {
        index,
        skipped: Vec::new(),
        inherited,
        read_bytes: 0,
        new_chunks: 0,
        new_bytes: 0,
    };
    for (n, id, outcome, read) in processed {
        out.index.ids[n as usize] = id;
        out.read_bytes += read;
        match outcome {
            Outcome::Exists => out.skipped.push((n, id)),
            Outcome::Unchanged => out.inherited.push((n, id)),
            Outcome::New(bytes) => {
                out.new_chunks += 1;
                out.new_bytes += bytes;
            }
            Outcome::Zero | Outcome::Seen => {}
        }
    }
    Ok(out)
}

/// 커밋 확인에서 사라진 청크(`missing`: 디스크 차례·청크 번호·id)를 아직 열린 원본에서 다시 읽어 올린다. 읽은 내용의
/// id가 다르면(원본이 바뀌었다) 오류다.
async fn reupload(
    store: &Arc<Store>,
    sources: &[DiskSource],
    missing: &[(usize, u64, ChunkId)],
) -> Result<u64, String> {
    let mut bytes = 0;
    for (i, src) in sources.iter().enumerate() {
        let mine: Vec<&(usize, u64, ChunkId)> = missing.iter().filter(|m| m.0 == i).collect();
        if mine.is_empty() {
            continue;
        }
        let mut c = NbdClient::connect(&src.socket, &src.export, &[])
            .await
            .map_err(nbd_err)?;
        let size = c.size();
        for (_, n, id) in mine {
            let off = n * CHUNK_BYTES;
            let raw = c
                .read(off, (size - off).min(CHUNK_BYTES) as u32)
                .await
                .map_err(nbd_err)?;
            let s = store.clone();
            let encoded = tokio::task::spawn_blocking(move || s.encode_chunk(&raw))
                .await
                .map_err(|e| e.to_string())?;
            if encoded.id != *id {
                return Err("the backup source changed while the backup was committed".into());
            }
            store
                .put_chunk(&encoded)
                .await
                .map_err(|e| stores::store_error("", e))?;
            bytes += encoded.data.len() as u64;
        }
        let _ = c.disconnect().await;
    }
    Ok(bytes)
}

/// 펌웨어 블롭(UEFI 변수·TPM 상태)의 내용
#[derive(Default)]
pub(super) struct Firmware {
    pub vars: Option<Vec<u8>>,
    pub tpm: Option<Vec<u8>>,
}

/// 저장소 백업 하나를 쓰는 데 필요한 것
pub(super) struct Commit<'a> {
    pub store_res: &'a BackupStore,
    pub store: &'a Arc<Store>,
    pub vm: Uuid,
    pub backup: Uuid,
    pub created_at: u64,
    pub config: ConfigBlob,
    pub firmware: Firmware,
    /// 시작할 때 본 저장소 상태(§10.3)
    pub epoch: u64,
    pub sweeping: bool,
}

/// 디스크를 모두 쓰고(§8.4) 블롭·인덱스를 쓴 뒤 커밋 구간에서 매니페스트를 쓴다(§10.3). 담은 디스크와 통계.
pub(super) async fn write_backup(
    ctx: &Ctx,
    c: &Commit<'_>,
    sources: &[DiskSource],
    bases: Option<&[FixedIndex]>,
) -> Result<(Vec<BackupDisk>, StoreBackupStats), String> {
    let store = c.store;
    let fail = |e| stores::store_error(&c.store_res.name, e);
    let mut stats = StoreBackupStats::default();
    let mut indexes = Vec::with_capacity(sources.len());
    let mut skipped: Vec<(usize, u64, ChunkId)> = Vec::new();
    let mut inherited: Vec<(usize, u64, ChunkId)> = Vec::new();
    let mut disks = Vec::with_capacity(sources.len());
    for (i, src) in sources.iter().enumerate() {
        let base = bases.and_then(|b| b.get(i));
        let r = Box::pin(write_disk(store, c.backup, src, base))
            .await
            .map_err(|e| format!("disk{i}: {e}"))?;
        stats.logical_bytes += r.index.virtual_bytes;
        stats.read_bytes += r.read_bytes;
        stats.new_chunks += r.new_chunks;
        stats.new_bytes += r.new_bytes;
        let zero = r.index.ids.iter().filter(|id| **id == ZERO_CHUNK).count() as u64;
        stats.zero_chunks += zero;
        stats.reused_chunks += r.index.ids.len() as u64 - zero - r.new_chunks;
        skipped.extend(r.skipped.iter().map(|(n, id)| (i, *n, *id)));
        inherited.extend(r.inherited.iter().map(|(n, id)| (i, *n, *id)));
        disks.push(BackupDisk {
            name: format!("disk{i}"),
            file: format!("disk{i}.fidx"),
            virtual_bytes: r.index.virtual_bytes,
            stored_bytes: r.new_bytes,
            sha256: None,
        });
        indexes.push(r.index);
    }
    // 블롭 → 인덱스 → (커밋 구간) 매니페스트
    let mut blobs: Vec<ManifestBlob> = Vec::new();
    let config = serde_json::to_vec(&c.config).map_err(|e| e.to_string())?;
    blobs.push(
        store
            .put_blob(c.vm, c.backup, CONFIG_BLOB, &config)
            .await
            .map_err(fail)?,
    );
    if let Some(vars) = &c.firmware.vars {
        blobs.push(
            store
                .put_blob(c.vm, c.backup, VARS_BLOB, vars)
                .await
                .map_err(fail)?,
        );
    }
    if let Some(tpm) = &c.firmware.tpm {
        blobs.push(
            store
                .put_blob(c.vm, c.backup, TPM_BLOB, tpm)
                .await
                .map_err(fail)?,
        );
    }
    let mut entries = Vec::with_capacity(indexes.len());
    for (i, ix) in indexes.iter().enumerate() {
        entries.push(
            store
                .put_index(c.vm, c.backup, &format!("disk{i}"), ix)
                .await
                .map_err(fail)?,
        );
    }
    let status = begin_commit(ctx, c.store_res.id, c.backup).await?;
    let committed = async {
        // 다시 볼 것: 쓸기가 있었으면 "있음"으로 건너뛴 id, 그리고 검증이 `bad/`로 옮긴 id 가운데 건너뛰었거나 base에서
        // 옮긴 것(리뷰 1 I5 — 커밋 구간 안이라 그새 더 옮겨지지 않는다)
        let swept = status.sweep_epoch != c.epoch || c.sweeping;
        let bad = store.quarantined_chunks().await.map_err(fail)?;
        let check: Vec<(usize, u64, ChunkId)> = skipped
            .iter()
            .filter(|s| swept || bad.contains(&s.2))
            .chain(inherited.iter().filter(|s| bad.contains(&s.2)))
            .copied()
            .collect();
        if !check.is_empty() {
            let ids: Vec<ChunkId> = check.iter().map(|s| s.2).collect();
            let gone: HashSet<ChunkId> = store.missing_chunks(&ids).await.map_err(fail)?.into_iter().collect();
            if !gone.is_empty() {
                let mut again: Vec<(usize, u64, ChunkId)> = Vec::new();
                let mut done = HashSet::new();
                for s in check.iter().filter(|s| gone.contains(&s.2)) {
                    if done.insert(s.2) {
                        again.push(*s);
                    }
                }
                tracing::info!(backup = %c.backup, chunks = again.len(), "re-uploading chunks removed by a GC sweep or a verification");
                let bytes = reupload(store, sources, &again).await?;
                stats.new_chunks += again.len() as u64;
                stats.reused_chunks -= again.len() as u64;
                stats.new_bytes += bytes;
            }
        }
        let mut m: Manifest = store.new_manifest(c.vm, c.backup, c.created_at, entries, blobs);
        store.put_manifest(&mut m).await.map_err(fail)
    }
    .await;
    if let Err(e) = end_commit(ctx, c.store_res.id, c.backup).await {
        tracing::warn!(backup = %c.backup, error = %e, "cannot leave the commit section; the janitor will");
    }
    committed?;
    Ok((disks, stats))
}

// ---- VM 백업 ----

/// 저장소 백업의 base(§8.3)와 그 인덱스
struct Base {
    backup: Backup,
    indexes: Vec<FixedIndex>,
}

/// `backup`에서 base를 거슬러 연속으로 비트맵으로 읽은 백업 수(`backup`이 base를 썼으면 그것도 센다)
fn bitmap_runs(backup: &Backup, backups: &[Backup]) -> u32 {
    let mut n = 0;
    let mut seen = HashSet::new();
    let mut base = backup.status.base;
    while let Some(id) = base
        && seen.insert(id)
    {
        n += 1;
        base = backups
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| b.status.base);
    }
    n
}

/// 저장소 백업의 base를 고른다(§8.3 — 계획 40 `choose_parent`와 같은 규칙에 저장소 쪽 조건). 고르지 못하면
/// `Ok(Err(전체로 읽는 이유))`.
async fn choose_base(
    ctx: &Ctx,
    backup: &Backup,
    vm: &Vm,
    store: &Arc<Store>,
    disks: &[(Volume, StoragePool)],
) -> Result<Result<Base, String>, String> {
    let name = backup.spec.store.as_deref().unwrap_or_default();
    let (_, backups) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    let earlier: Vec<&Backup> = backups
        .iter()
        .filter(|b| {
            b.id != backup.id
                && b.spec.vm == vm.id
                && b.spec.target() == Ok(BackupTarget::Store(name))
                && b.status.phase == BackupPhase::Ready
        })
        .collect();
    if earlier.is_empty() {
        return Ok(Err(format!(
            "no earlier backup of this VM in store {name:?}"
        )));
    }
    let found = ctx
        .hv
        .backup_bitmaps(vm.id)
        .await
        .map_err(|e| format!("cannot read dirty bitmaps: {e}"))?;
    let mut bitmap: Option<&str> = None;
    let mut same = found.len() == disks.len() && !found.is_empty();
    for disk in &found {
        let ours: Vec<_> = disk
            .bitmaps
            .iter()
            .filter(|b| is_backup_bitmap(&b.name))
            .collect();
        if ours.is_empty() {
            return Ok(Err(format!("no dirty bitmap on {}", disk.node)));
        }
        if ours.iter().any(|b| b.inconsistent || !b.recording) {
            return Ok(Err(format!(
                "dirty bitmap on {} is inconsistent",
                disk.node
            )));
        }
        match ours[..] {
            [one] if *bitmap.get_or_insert(&one.name) == one.name => {}
            _ => same = false,
        }
    }
    let Some(base) = bitmap
        .filter(|_| same)
        .and_then(|b| earlier.into_iter().find(|e| bitmap_name(e.id) == b))
        .cloned()
    else {
        return Ok(Err(format!(
            "dirty bitmaps do not match a ready backup in store {name:?}"
        )));
    };
    if base
        .status
        .verify
        .as_ref()
        .is_some_and(|v| v.state == VerifyState::Failed)
    {
        return Ok(Err("a chunk of the base backup failed verification".into()));
    }
    let mut sizes = Vec::with_capacity(disks.len());
    for (volume, pool) in disks {
        let info = ctx
            .storage
            .volume_info(pool, volume.id, volume.spec.format)
            .await
            .map_err(|e| format!("cannot read size of volume {:?}: {e}", volume.name))?;
        sizes.push(info.virtual_bytes);
    }
    let volumes =
        |spec: &VmSpec| -> Vec<String> { spec.disks.iter().map(|d| d.volume.clone()).collect() };
    let base_sizes: Vec<u64> = base.status.disks.iter().map(|d| d.virtual_bytes).collect();
    if volumes(&base.spec.vm_spec) != volumes(&vm.spec) || base_sizes != sizes {
        return Ok(Err(format!("disks changed since backup {:?}", base.name)));
    }
    if let Some(max) = backup.spec.max_chain {
        let n = bitmap_runs(&base, &backups) + 1;
        if n > max {
            return Ok(Err(format!(
                "the last {} backups read only changed blocks; reading the whole disks",
                n - 1
            )));
        }
    }
    let indexes = async {
        let m = store.read_manifest(base.spec.vm, base.id).await?;
        let mut out = Vec::with_capacity(m.disks.len());
        for d in &m.disks {
            out.push(store.read_index(&m, d).await?);
        }
        Ok::<_, opl_store::StoreError>(out)
    }
    .await;
    match indexes {
        Ok(indexes) if indexes.len() == disks.len() => Ok(Ok(Base {
            backup: base,
            indexes,
        })),
        Ok(_) => Ok(Err(format!("disks changed since backup {:?}", base.name))),
        Err(e) => Ok(Err(format!(
            "cannot read backup {:?} from the store: {e}",
            base.name
        ))),
    }
}

/// VM의 UEFI 변수·TPM 상태(있으면)를 읽는다. TPM 상태를 얻지 못하면 실패한다(풀 백업의 `copy_firmware`와 같은 규칙).
async fn read_firmware(ctx: &Ctx, vm: &Vm) -> Result<Firmware, String> {
    let Some(state) = &ctx.config.state_dir else {
        return Ok(Firmware::default());
    };
    fetch_vars(ctx, vm).await;
    let path = uefi_vars_path(state, vm.id);
    let vars = match tokio::fs::read(&path).await {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot copy UEFI variables: {e}")),
    };
    let tpm = match current_tpm(ctx, vm).await? {
        Some(p) => Some(
            tokio::fs::read(&p)
                .await
                .map_err(|e| format!("cannot copy the TPM state: {e}"))?,
        ),
        None => None,
    };
    Ok(Firmware { vars, tpm })
}

/// fleecing 읽기 요청(§8.1)
fn fleece_request(ctx: &Ctx, vm: &Vm, backup: Uuid, base: Option<Uuid>) -> FleeceRequest {
    FleeceRequest {
        run: backup,
        parent: base.map(bitmap_name),
        bitmap: bitmap_name(backup),
        quiesce: vm.spec.guest_agent.as_ref().is_some_and(|g| g.freeze_fs),
        reserve_min: ctx.config.fleece_reserve_min,
        min_free: ctx.config.fleece_min_free,
    }
}

/// `pending` 저장소 백업 `backup`을 채운다(§8): 실행 중인 VM은 fleecing NBD(증분이면 비트맵 문맥으로 바뀐 청크만), 꺼진
/// VM은 `qemu-nbd`로 디스크 전체를 읽는다. 커밋한 뒤 `ready`로 바꾼다. 게스트 에이전트가 얼리지 못했으면 그 경고.
pub(super) async fn backup(
    ctx: &Ctx,
    task: &Task,
    backup: Backup,
) -> Result<Option<String>, String> {
    let id = backup.id;
    let name = backup.spec.store.clone().unwrap_or_default();
    let vm = get_vm(ctx, task.spec.target)?;
    check_settled(&vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let stopped = vm.spec.power == PowerState::Stopped && is_down(&vm);
    if !running && !stopped {
        return Err("VM is being started; try again when it has settled".into());
    }
    let (store_res, store) = open_store(ctx, &name).await?;
    check_writable(&store_res)?;
    let disks = ready_disks(ctx, &vm.spec)?;
    for (volume, pool) in &disks {
        check_local(ctx, volume, pool)?;
    }
    if backup.spec.vm_spec != vm.spec || backup.spec.vm_name != vm.name {
        let spec = BackupSpec {
            vm_name: vm.name.clone(),
            vm_spec: vm.spec.clone(),
            ..backup.spec.clone()
        };
        ctx.cluster
            .update_spec(id, Some(backup.revision), backup.labels.clone(), &spec)
            .await
            .map_err(|e| e.to_string())?;
    }
    // base는 만드는 동안 보인다(지우기를 막는다 — §8.3).
    let live = running && !disks.is_empty();
    let base = if !live {
        Err(backup.spec.incremental.then(|| "VM is stopped".to_string()))
    } else if backup.spec.incremental {
        choose_base(ctx, &backup, &vm, &store, &disks)
            .await?
            .map_err(Some)
    } else {
        Err(None)
    };
    let record = async |base: Option<Uuid>, full_reason: Option<String>| {
        let status = BackupStatus {
            base,
            full_reason,
            ..backup.status.clone()
        };
        ctx.cluster
            .update_status::<BackupSpec>(id, &status)
            .await
            .map_err(|e| e.to_string())
    };
    let (base_id, full_reason) = match &base {
        Ok(b) => (Some(b.backup.id), None),
        Err(reason) => (None, reason.clone()),
    };
    if base_id.is_some() || full_reason.is_some() {
        record(base_id, full_reason.clone()).await?;
    }
    // base를 적기 전에 시작한 base 지우기는 막지 못했다. 그런 base 위에는 쌓지 않는다.
    let (base, full_reason) = match &base {
        Ok(b) => match super::backup::parent_going(ctx, &b.backup)? {
            Some(reason) => {
                record(None, Some(reason.clone())).await?;
                (None, Some(reason))
            }
            None => (base.ok(), None),
        },
        Err(_) => (None, full_reason),
    };
    let firmware = read_firmware(ctx, &vm).await?;
    let commit = Commit {
        store_res: &store_res,
        store: &store,
        vm: vm.id,
        backup: id,
        created_at: now_secs(),
        config: ConfigBlob {
            vm_name: vm.name.clone(),
            vm_spec: vm.spec.clone(),
            uefi_vars: firmware.vars.is_some(),
            tpm_state: firmware.tpm.is_some(),
        },
        firmware,
        epoch: store_res.status.sweep_epoch,
        sweeping: store_res.status.sweeping,
    };
    let bases = base.as_ref().map(|b| b.indexes.as_slice());
    let (written, warning) = if live {
        let req = fleece_request(ctx, &vm, id, base.as_ref().map(|b| b.backup.id));
        let export = ctx
            .hv
            .fleece_begin(vm.id, &req)
            .await
            .map_err(|e| format!("cannot freeze the disks for the backup: {e}"))?;
        let sources: Vec<DiskSource> = export
            .disks
            .iter()
            .map(|d| DiskSource {
                socket: export.socket.clone(),
                export: d.export.clone(),
                dirty: d.dirty_context.clone(),
            })
            .collect();
        let written = Box::pin(write_backup(ctx, &commit, &sources, bases)).await;
        // 커밋까지 원본을 열어 둔다(§8.4). 성공이면 부모 비트맵을, 실패면 새 비트맵을 지운다.
        let ended = ctx.hv.fleece_end(vm.id, &req, written.is_ok()).await;
        let written = match (written, ended) {
            (Ok(w), Err(e)) => {
                tracing::warn!(backup = %id, error = %e, "the backup is complete but closing its disk export failed");
                Ok(w)
            }
            (Ok(w), Ok(())) => {
                // 남은 다른 백업 비트맵(전체 백업이 쌓은 것, 풀 백업의 것)은 다음 증분을 막을 뿐이다 — 풀 백업처럼
                // 이 백업의 것만 남긴다(리뷰 1 I1).
                if let Err(e) = ctx.hv.remove_bitmaps(vm.id, Some(&req.bitmap)).await {
                    tracing::warn!(backup = %id, error = %e, "cannot remove older dirty bitmaps");
                }
                Ok(w)
            }
            (r, _) => r,
        };
        (written, export.quiesced.warning())
    } else {
        let mut opened: Vec<NbdSource> = Vec::new();
        let mut sources = Vec::new();
        let mut released = Vec::new();
        let mut failed = None;
        for (i, (volume, pool)) in disks.iter().enumerate() {
            let src = match ctx
                .storage
                .volume_source(pool, volume.id, volume.spec.format, None)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    failed = Some(format!("cannot read volume {:?}: {e}", volume.name));
                    break;
                }
            };
            released.push((volume, pool));
            let socket = socket_path(ctx, &format!("bk{i}"), id);
            match nbd_source(&ctx.config.qemu_nbd, &src, &socket).await {
                Ok(s) => {
                    sources.push(DiskSource {
                        socket: s.socket.clone(),
                        export: s.export.clone(),
                        dirty: None,
                    });
                    opened.push(s);
                }
                Err(e) => {
                    failed = Some(format!("cannot read volume {:?}: {e}", volume.name));
                    break;
                }
            }
        }
        let written = match failed {
            Some(e) => Err(e),
            None => Box::pin(write_backup(ctx, &commit, &sources, None)).await,
        };
        for s in opened {
            s.close().await;
        }
        for (volume, pool) in released {
            if let Err(e) = ctx
                .storage
                .release_source(pool, volume.id, volume.spec.format, None)
                .await
            {
                tracing::warn!(volume = %volume.name, error = %e, "cannot release backup source");
            }
        }
        // 꺼진 VM은 잠금을 쥐고 읽었다. qcow2 디스크에 비트맵을 두어 다음 실행 중 백업이 증분이 되게 한다.
        if written.is_ok() && !vm.spec.template {
            for (volume, pool) in &disks {
                if let Err(e) = ctx
                    .storage
                    .set_backup_bitmap(pool, volume.id, volume.spec.format, &bitmap_name(id))
                    .await
                {
                    tracing::warn!(backup = %id, volume = %volume.name, error = %e, "cannot set dirty bitmap of backup");
                }
            }
        }
        (written, None)
    };
    let (disks, stats) = written?;
    let status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(commit.created_at),
        disks,
        uefi_vars: commit.config.uefi_vars,
        error: None,
        node: None,
        replicas: Vec::new(),
        received_at: None,
        parent: None,
        full_reason,
        tpm_state: commit.config.tpm_state,
        base: base.as_ref().map(|b| b.backup.id),
        store_stats: Some(stats),
        verify: None,
    };
    mark_backup(ctx, id, &status).await?;
    Ok(warning)
}

/// 다 쓴 백업의 상태를 적는다. 실패로 끝내면 정리가 지우게 되므로 기록될 때까지 다시 시도한다.
async fn mark_backup(ctx: &Ctx, id: Uuid, status: &BackupStatus) -> Result<(), String> {
    loop {
        match ctx.cluster.update_status::<BackupSpec>(id, status).await {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot mark backup ready; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 실패한 저장소 백업의 흔적을 지운다: 커밋 구간에서 빼고, 저장소의 스냅샷 객체를 지우고(청크는 GC가 거둔다),
/// `failed`와 `error`를 남긴다. opld가 다시 떠 끊긴 백업이면(`unfinished`) VM에 남은 fleecing도 치운다.
pub(super) async fn discard(ctx: &Ctx, backup: Backup, error: &str, unfinished: bool) {
    let id = backup.id;
    let name = backup.spec.store.clone().unwrap_or_default();
    if let Ok(store) = stores::get(&ctx.cluster, &name)
        && let Err(e) = end_commit(ctx, store.id, id).await
    {
        tracing::warn!(backup = %id, error = %e, "cannot leave the commit section of a failed backup");
    }
    if unfinished
        && let Ok(Some(vm)) = ctx.cluster.get::<VmSpec>(backup.spec.vm)
        && matches!(ctx.hv.inspect(vm.id).await, Ok(Some(_)))
    {
        let req = fleece_request(ctx, &vm, id, backup.status.base);
        if let Err(e) = ctx.hv.fleece_end(vm.id, &req, false).await {
            tracing::warn!(backup = %id, error = %e, "cannot clear the disk export of a failed backup");
        }
    }
    match open_store(ctx, &name).await {
        Ok((res, store)) if !res.spec.read_only => {
            if let Err(e) = store.delete_snapshot(backup.spec.vm, id).await {
                tracing::warn!(backup = %id, error = %e, "cannot clean up failed backup in the store");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(backup = %id, error = %e, "cannot clean up failed backup"),
    }
    let status = BackupStatus {
        phase: BackupPhase::Failed,
        error: Some(error.to_string()),
        ..backup.status
    };
    loop {
        match ctx.cluster.update_status::<BackupSpec>(id, &status).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot mark backup failed; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

// ---- 지우기 ----

/// 저장소 백업 `backup`이 만드는 중인(`pending`) 백업의 base인지(§10.1). 자식이 base 인덱스의 청크를 다시 쓰지 않고
/// 가리키므로 그동안은 지우지 못한다.
pub(crate) fn base_in_use(backups: &[Backup], backup: &Backup) -> Option<String> {
    let n = backups
        .iter()
        .filter(|b| b.status.base == Some(backup.id) && b.status.phase == BackupPhase::Pending)
        .count();
    (n > 0).then(|| {
        format!(
            "backup {:?} is the base of {n} backup(s) being made; try again when they have finished",
            backup.name
        )
    })
}

/// 저장소 백업을 지운다(§10.1): 매니페스트 → 인덱스·블롭(청크는 GC가 거둔다) → 리소스. 체인이 없으므로 순서 제약이
/// 없다(만드는 중인 백업의 base만 막는다). 저장소 리소스가 없어졌으면 리소스만 지운다. 읽기 전용 저장소의 스냅샷은
/// 지우지 않는다.
pub(super) async fn delete(ctx: &Ctx, backup: Backup) -> Result<Option<String>, String> {
    let (_, backups) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    if let Some(e) = base_in_use(&backups, &backup) {
        return Err(e);
    }
    let name = backup.spec.store.clone().unwrap_or_default();
    match ctx
        .cluster
        .get_by_name::<BackupStoreSpec>(&name)
        .map_err(|e| e.to_string())?
    {
        None => {
            tracing::warn!(backup = %backup.name, store = %name, "backup store is gone; deleting only the backup");
        }
        Some(res) if res.spec.read_only => {
            return Err(format!("backup store {name:?} is read-only"));
        }
        Some(_) => {
            let (_, store) = open_store(ctx, &name).await?;
            store
                .delete_snapshot(backup.spec.vm, backup.id)
                .await
                .map_err(|e| stores::store_error(&name, e))?;
        }
    }
    let left = super::replicate::delete_copies(ctx, &backup).await;
    match ctx.cluster.delete::<BackupSpec>(backup.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
        Err(e) => return Err(e.to_string()),
    }
    Ok((!left.is_empty()).then(|| {
        format!(
            "backup deleted, but remote copies may be left: {}",
            left.join("; ")
        )
    }))
}

// ---- 복원(§8.6) ----

/// 이 노드 안에서 저장소 백업을 유닉스 소켓 NBD로 내보내는 서버(`backup-serve` 헬퍼와 같은 몸통). drop되면 멈춘다.
pub(super) struct Serving {
    socket: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Serving {
    /// 서버가 아직 도는지
    pub(super) fn alive(&self) -> bool {
        !self.task.is_finished()
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}

pub(super) async fn serve(
    ctx: &Ctx,
    store: &Arc<Store>,
    m: &Manifest,
    socket: PathBuf,
) -> Result<Serving, String> {
    let cache = Arc::new(ChunkCache::new(
        ctx.config.serve_cache_mib.max(8) << 20,
        store.max_streams(),
    ));
    let mut server = NbdServer::new();
    for disk in &m.disks {
        let index = store
            .read_index(m, disk)
            .await
            .map_err(|e| stores::store_error("", e))?;
        server.add(
            disk.name.clone(),
            Arc::new(StoreExport::new(store.clone(), index, cache.clone())),
        );
    }
    let listener = opl_store::nbd::serve::bind(&socket).map_err(|e| e.to_string())?;
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(Arc::new(server).serve(listener, async {
        let _ = rx.await;
    }));
    Ok(Serving {
        socket,
        stop: Some(stop),
        task,
    })
}

/// 저장소 백업의 매니페스트와 `config` 블롭을 읽고 리소스와 맞춰 본다. VM 스펙은 리소스의 것이 기준이다: 받은 사본·
/// scan한 백업의 `config`는 보낸(만든) 클러스터의 원래 스펙(폴더·복제 대상·옛 노드가 버린 칸이 든)이라 같지 않을 수
/// 있다 — 디스크 수만 맞춰 본다(리뷰 1 C1).
pub(super) async fn read_snapshot(
    store: &Store,
    backup: &Backup,
) -> Result<(Manifest, ConfigBlob), String> {
    let name = backup.spec.store.as_deref().unwrap_or_default();
    let fail = |e| stores::store_error(name, e);
    let m = store
        .read_manifest(backup.spec.vm, backup.id)
        .await
        .map_err(fail)?;
    let raw = store.get_blob(&m, CONFIG_BLOB).await.map_err(fail)?;
    let config: ConfigBlob =
        serde_json::from_slice(&raw).map_err(|e| format!("invalid config blob: {e}"))?;
    let disks = backup.spec.vm_spec.disks.len();
    if config.vm_spec.disks.len() != disks || m.disks.len() != disks {
        return Err("backup manifest does not match the backup record".into());
    }
    Ok((m, config))
}

/// 저장소 백업 `backup`을 새 VM `name`으로 되살린다(§8.6): 이 노드 안 NBD 서버로 디스크를 내보내 디스크마다 `pool`에 새
/// 볼륨을 만들어 읽어 넣고(`qemu-img convert`, 0 청크는 쓰지 않음), UEFI 변수·TPM 상태는 블롭에서 받는다. 그 뒤는 풀
/// 백업 복원과 같다.
///
/// `live`(계획 68 §11.3)면 디스크를 읽어 넣지 않고 빈 대상 볼륨만 만들고 새 VM을 라이브 복원 상태로 만든다(풀 백업
/// 복원과 같다 — [`super::backup::restore`]).
pub(super) async fn restore(
    ctx: &Ctx,
    task: &Task,
    backup: Backup,
    name: &str,
    pool: &str,
    folder: Option<Uuid>,
    live: bool,
) -> Result<Option<String>, String> {
    let store_name = backup.spec.store.clone().unwrap_or_default();
    let target = ready_pool(ctx, pool)?;
    let (_, store) = open_store(ctx, &store_name).await?;
    let (m, config) = read_snapshot(&store, &backup).await?;
    let spec = &backup.spec.vm_spec;
    if config.tpm_state || spec.tpm || spec.secure_boot {
        let old = crate::cluster::missing_feature(&ctx.cluster, opl_types::node::VTPM)
            .map_err(|e| e.to_string())?;
        if !old.is_empty() {
            return Err(format!(
                "some nodes do not support vtpm yet ({}); upgrade them first",
                old.join(", ")
            ));
        }
    }
    let serving = if live {
        None
    } else {
        Some(serve(ctx, &store, &m, socket_path(ctx, "rs", task.id)).await?)
    };
    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let format = if crate::volumes::block_pool_kind(&target).is_some() {
        opl_types::runtime::ImageFormat::Raw
    } else {
        opl_types::runtime::ImageFormat::Qcow2
    };
    let mut copies: Vec<Volume> = Vec::with_capacity(m.disks.len());
    for disk in &m.disks {
        let copy_name = volume_name(name, &disk.name, |n| taken.contains(n));
        taken.insert(copy_name.clone());
        let mut copy: Volume = Resource::new(
            copy_name,
            VolumeSpec {
                pool: target.name.clone(),
                size_bytes: disk.virtual_bytes,
                format,
                content: Default::default(),
                base: None,
            },
        );
        copy.status.node = Some(ctx.config.node);
        made(ctx, task.id, |m| m.volumes.push(copy.id));
        ctx.cluster
            .create(&copy)
            .await
            .map_err(|e| format!("cannot create volume {:?}: {e}", copy.name))?;
        let imported = match &serving {
            Some(serving) => {
                ctx.storage
                    .import_nbd(&target, copy.id, &serving.socket, &disk.name, format)
                    .await
            }
            // 라이브 복원: 빈 대상 볼륨(mirror가 채운다)
            None => ctx
                .storage
                .create_volume(&target, copy.id, disk.virtual_bytes, format)
                .await
                .map(|_| Default::default()),
        };
        imported.map_err(|e| format!("cannot restore disk {:?}: {e}", disk.name))?;
        copies.push(copy);
    }
    let mut new: Vm = Resource::new(name, backup.spec.vm_spec.clone());
    new.id = task.spec.target;
    new.spec.folder = folder;
    let (store_ref, m_ref, sn) = (&store, &m, &store_name);
    let blob = |n: &'static str| async move {
        store_ref
            .get_blob(m_ref, n)
            .await
            .map_err(|e| stores::store_error(sn, e))
    };
    if config.uefi_vars
        && let Some(state) = &ctx.config.state_dir
    {
        let vars = blob(VARS_BLOB).await?;
        let to = uefi_vars_path(state, new.id);
        made(ctx, task.id, |m| m.vars = Some(to.clone()));
        if let Some(dir) = to.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        }
        tokio::fs::write(&to, vars)
            .await
            .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        new.status.firmware_node = Some(ctx.config.node);
    }
    if config.tpm_state {
        let state = ctx
            .config
            .state_dir
            .as_ref()
            .ok_or("this node cannot keep the TPM state of the backup")?;
        let tpm = blob(TPM_BLOB).await?;
        let to = tpm_state_path(state, new.id);
        made(ctx, task.id, |m| m.tpm = to.parent().map(Path::to_path_buf));
        let sha256 = place_private_bytes(tpm, &to)
            .await
            .map_err(|e| format!("cannot restore the TPM state: {e}"))?;
        new.status.tpm_sha256 = Some(sha256);
        new.status.firmware_node = Some(ctx.config.node);
        new.status.tpm_node = Some(ctx.config.node);
    }
    drop(serving);
    for copy in &copies {
        mark_ready(ctx, copy, &target).await;
    }
    for (disk, copy) in new.spec.disks.iter_mut().zip(&copies) {
        disk.volume = copy.name.clone();
    }
    new.spec.ha = None;
    for nic in &mut new.spec.nics {
        nic.mac = None;
    }
    let mut warnings = available_media(ctx, &mut new.spec)?;
    warnings.extend(super::clone::identity_warning("the restored VM", &new.spec));
    new.spec
        .validate()
        .map_err(|e| format!("invalid VM spec in backup: {e}"))?;
    let node = super::live_restore::begin(ctx, task, &mut new, backup.id, live);
    create_vm(ctx, task, new, node, live, false, &mut warnings).await?;
    Ok((!warnings.is_empty()).then(|| warnings.join("; ")))
}

// ---- 옛 백업 옮기기(§12.2) ----

/// 옮긴 백업의 이름: 원본 이름에 `-st`(겹치면 `-st2`…), 63자 안
fn convert_name(original: &str, taken: impl Fn(&str) -> bool) -> String {
    (1..)
        .map(|n| {
            let suffix = if n == 1 {
                "-st".to_string()
            } else {
                format!("-st{n}")
            };
            let keep = original.len().min(63 - suffix.len());
            format!("{}{suffix}", original[..keep].trim_end_matches('-'))
        })
        .find(|name| !taken(name))
        .expect("some suffix is free")
}

/// 원본 `source`를 저장소 `store`로 옮긴 백업(이 Task가 만든 것 — 끝나지 않은 것 포함)
fn converted<'a>(backups: &'a [Backup], source: Uuid, store: &str) -> Option<&'a Backup> {
    backups.iter().find(|b| {
        b.spec.store.as_deref() == Some(store)
            && b.spec
                .source
                .as_ref()
                .is_some_and(|s| s.backup == source && s.kind == BackupSourceKind::Convert)
            && b.status.phase != BackupPhase::Failed
    })
}

/// 옛 풀 백업 `source`(대상, 증분이면 체인)를 저장소 `store`로 옮긴다(§12.2): 디스크마다 체인을 `qemu-nbd`로 열어
/// 파이프라인으로 쓰고, 같은 `vm`·`vm_spec`·`job`·`created_at`의 새 Backup(`spec.source = {source, convert}`)을 만든다.
/// 원본은 남긴다. 같은 원본을 같은 저장소로 이미 옮겼으면 실패한다.
pub(super) async fn convert(
    ctx: &Ctx,
    task: &Task,
    store_name: &str,
) -> Result<Option<String>, String> {
    let source = get_backup(ctx, task.spec.target)?.ok_or("backup was deleted")?;
    if source.status.phase != BackupPhase::Ready {
        return Err("backup is not ready".into());
    }
    let pool_name = match source.spec.target() {
        Ok(BackupTarget::Pool(p)) => p.to_string(),
        _ => return Err("only backups in a backup pool can be moved to a backup store".into()),
    };
    let (store_res, store) = open_store(ctx, store_name).await?;
    check_writable(&store_res)?;
    let (_, backups) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    if let Some(b) = converted(&backups, source.id, store_name) {
        return Err(format!(
            "backup {:?} was already moved to store {store_name:?} as {:?}",
            source.name, b.name
        ));
    }
    if let Some(node) = source.status.node.filter(|n| *n != ctx.config.node) {
        return Err(format!("backup is stored on node {node}"));
    }
    let (_, root) = backup_root(ctx, &pool_name)?;
    let dir = backup_dir(&root, source.id);
    let manifest = read_manifest(&dir).await?;
    if manifest.backup != source.id || manifest.disks.len() != source.spec.vm_spec.disks.len() {
        return Err("backup manifest does not match the backup record".into());
    }
    let ancestors = read_chain(ctx, &source, &root, &manifest).await?;
    // 새 백업(pending) — 커밋할 때까지 GC의 기다림 집합에 든다.
    let mut copy: Backup = Resource::new(
        convert_name(&source.name, |n| backups.iter().any(|b| b.name == n)),
        BackupSpec {
            pool: String::new(),
            store: Some(store_name.to_string()),
            source: Some(BackupSource {
                backup: source.id,
                kind: BackupSourceKind::Convert,
            }),
            incremental: false,
            max_chain: None,
            ..source.spec.clone()
        },
    );
    copy.labels = source.labels.clone();
    ctx.cluster.create(&copy).await.map_err(|e| e.to_string())?;
    let mut files_per_disk: Vec<Vec<PathBuf>> = Vec::with_capacity(manifest.disks.len());
    for (i, disk) in manifest.disks.iter().enumerate() {
        let mut chain = Vec::with_capacity(ancestors.len() + 1);
        for (d, m) in
            std::iter::once((&dir, &manifest)).chain(ancestors.iter().map(|(d, m)| (d, m)))
        {
            let file = m.disks[i].file.as_str();
            if Path::new(file).file_name() != Some(std::ffi::OsStr::new(file)) {
                return Err(format!("invalid file name {file:?}"));
            }
            let file = regular_file(d, file).await?;
            ctx.storage
                .check_import_source(&file, opl_types::storage::SourceFormat::Qcow2)
                .await
                .map_err(|e| format!("cannot read disk {:?}: {e}", disk.name))?;
            chain.push(file);
        }
        files_per_disk.push(chain);
    }
    let mut opened = Vec::new();
    let mut sources = Vec::new();
    for (i, (files, disk)) in files_per_disk.iter().zip(&manifest.disks).enumerate() {
        let socket = socket_path(ctx, &format!("cv{i}"), copy.id);
        let s = nbd_chain_source(&ctx.config.qemu_nbd, files, disk.virtual_bytes, &socket)
            .await
            .map_err(|e| format!("cannot read disk {:?}: {e}", disk.name))?;
        sources.push(DiskSource {
            socket: s.socket.clone(),
            export: s.export.clone(),
            dirty: None,
        });
        opened.push(s);
    }
    let file = |name: &str| {
        let dir = dir.clone();
        let name = name.to_string();
        async move {
            let path = regular_file(&dir, &name).await?;
            tokio::fs::read(path)
                .await
                .map_err(|e| format!("cannot read {name}: {e}"))
        }
    };
    let firmware = Firmware {
        vars: if manifest.uefi_vars {
            Some(file(VARS_FILE).await?)
        } else {
            None
        },
        tpm: if manifest.tpm_state {
            Some(file(opl_types::backup::TPM_STATE_FILE).await?)
        } else {
            None
        },
    };
    let created_at = source.status.created_at.unwrap_or(manifest.created_at);
    let commit = Commit {
        store_res: &store_res,
        store: &store,
        vm: source.spec.vm,
        backup: copy.id,
        created_at,
        config: ConfigBlob {
            vm_name: manifest.vm_name.clone(),
            vm_spec: manifest.vm_spec.clone(),
            uefi_vars: firmware.vars.is_some(),
            tpm_state: firmware.tpm.is_some(),
        },
        firmware,
        epoch: store_res.status.sweep_epoch,
        sweeping: store_res.status.sweeping,
    };
    let written = Box::pin(write_backup(ctx, &commit, &sources, None)).await;
    for s in opened {
        s.close().await;
    }
    let (disks, stats) = written?;
    let status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(created_at),
        disks,
        uefi_vars: commit.config.uefi_vars,
        tpm_state: commit.config.tpm_state,
        store_stats: Some(stats),
        ..Default::default()
    };
    mark_backup(ctx, copy.id, &status).await?;
    Ok(None)
}

/// 실패한 옮기기가 만든 `pending` 백업을 [`discard`]로 치운다.
pub(super) async fn discard_convert(ctx: &Ctx, task: &Task, store: &str, error: &str) {
    let Ok((_, backups)) = ctx.cluster.list::<BackupSpec>() else {
        return;
    };
    let made = backups
        .into_iter()
        .filter(|b| b.status.phase == BackupPhase::Pending)
        .find(|b| {
            b.spec.store.as_deref() == Some(store)
                && b.spec.source.as_ref().is_some_and(|s| {
                    s.backup == task.spec.target && s.kind == BackupSourceKind::Convert
                })
        });
    if let Some(b) = made {
        discard(ctx, b, error, false).await;
    }
}

// ---- 저장소 사이 복사(§12.5) ----

/// 원본 `source`를 저장소 `store`로 복사한 백업(실패하지 않은 것)
fn copied<'a>(backups: &'a [Backup], source: Uuid, store: &str) -> Option<&'a Backup> {
    backups.iter().find(|b| {
        b.spec.store.as_deref() == Some(store)
            && b.spec
                .source
                .as_ref()
                .is_some_and(|s| s.backup == source && s.kind == BackupSourceKind::Copy)
            && b.status.phase != BackupPhase::Failed
    })
}

/// 복사한 백업의 이름: 원본 이름에 `-{저장소}`(겹치면 번호), 63자 안
fn copy_name(original: &str, store: &str, taken: impl Fn(&str) -> bool) -> String {
    (1..)
        .map(|n| {
            let suffix = if n == 1 {
                format!("-{store}")
            } else {
                format!("-{store}{n}")
            };
            let suffix = &suffix[..suffix.len().min(40)];
            let keep = original.len().min(63 - suffix.len());
            format!("{}{suffix}", original[..keep].trim_end_matches('-'))
        })
        .find(|name| !taken(name))
        .expect("some suffix is free")
}

/// 저장소 백업 `source`(대상)를 다른 저장소 `store`로 복사한다(§12.5): 원본을 이 노드 안 NBD 서버로 내보내 파이프라인으로
/// 대상 저장소에 쓴다 — 대상에 이미 있는 청크는 올리지 않고(있음 확인), 키가 다르면 대상 키로 다시 암호화한다(키가 같으면
/// 결정적 nonce라 객체 바이트가 같다). 새 Backup(`spec.source = {source, copy}`, 같은 `vm`·`vm_spec`·`job`·`created_at`)을
/// 만든다. 같은 원본을 같은 저장소로 이미 복사했으면 실패한다.
pub(super) async fn copy(
    ctx: &Ctx,
    task: &Task,
    store_name: &str,
) -> Result<Option<String>, String> {
    let source = get_backup(ctx, task.spec.target)?.ok_or("backup was deleted")?;
    if source.status.phase != BackupPhase::Ready {
        return Err("backup is not ready".into());
    }
    let from = source
        .spec
        .store
        .clone()
        .ok_or("only backups in a backup store can be copied to another store")?;
    let (_, src) = open_store(ctx, &from).await?;
    let (dst_res, dst) = open_store(ctx, store_name).await?;
    check_writable(&dst_res)?;
    let (_, backups) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    if let Some(b) = copied(&backups, source.id, store_name) {
        return Err(format!(
            "backup {:?} was already copied to store {store_name:?} as {:?}",
            source.name, b.name
        ));
    }
    let (m, config) = read_snapshot(&src, &source).await?;
    let fail = |e| stores::store_error(&from, e);
    let firmware = Firmware {
        vars: match m.blob(VARS_BLOB) {
            Some(_) => Some(src.get_blob(&m, VARS_BLOB).await.map_err(fail)?),
            None => None,
        },
        tpm: match m.blob(TPM_BLOB) {
            Some(_) => Some(src.get_blob(&m, TPM_BLOB).await.map_err(fail)?),
            None => None,
        },
    };
    let mut copy: Backup = Resource::new(
        copy_name(&source.name, store_name, |n| {
            backups.iter().any(|b| b.name == n)
        }),
        BackupSpec {
            pool: String::new(),
            store: Some(store_name.to_string()),
            source: Some(BackupSource {
                backup: source.id,
                kind: BackupSourceKind::Copy,
            }),
            incremental: false,
            max_chain: None,
            origin: None,
            ..source.spec.clone()
        },
    );
    copy.labels = source.labels.clone();
    ctx.cluster.create(&copy).await.map_err(|e| e.to_string())?;
    let serving = serve(ctx, &src, &m, socket_path(ctx, "cp", copy.id)).await?;
    let sources: Vec<DiskSource> = m
        .disks
        .iter()
        .map(|d| DiskSource {
            socket: socket_path(ctx, "cp", copy.id),
            export: d.name.clone(),
            dirty: None,
        })
        .collect();
    let created_at = source.status.created_at.unwrap_or(m.created_at);
    let commit = Commit {
        store_res: &dst_res,
        store: &dst,
        vm: source.spec.vm,
        backup: copy.id,
        created_at,
        config: ConfigBlob {
            uefi_vars: firmware.vars.is_some(),
            tpm_state: firmware.tpm.is_some(),
            ..config
        },
        firmware,
        epoch: dst_res.status.sweep_epoch,
        sweeping: dst_res.status.sweeping,
    };
    let written = Box::pin(write_backup(ctx, &commit, &sources, None)).await;
    drop(serving);
    let (disks, stats) = written?;
    let status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(created_at),
        disks,
        uefi_vars: commit.config.uefi_vars,
        tpm_state: commit.config.tpm_state,
        store_stats: Some(stats),
        ..Default::default()
    };
    mark_backup(ctx, copy.id, &status).await?;
    Ok(None)
}

/// 실패한 복사가 만든 `pending` 백업을 [`discard`]로 치운다.
pub(super) async fn discard_copy(ctx: &Ctx, task: &Task, store: &str, error: &str) {
    let Ok((_, backups)) = ctx.cluster.list::<BackupSpec>() else {
        return;
    };
    let made = backups
        .into_iter()
        .filter(|b| b.status.phase == BackupPhase::Pending)
        .find(|b| {
            b.spec.store.as_deref() == Some(store)
                && b.spec.source.as_ref().is_some_and(|s| {
                    s.backup == task.spec.target && s.kind == BackupSourceKind::Copy
                })
        });
    if let Some(b) = made {
        discard(ctx, b, error, false).await;
    }
}

/// 끝나지 않은 옮기기 Task가 이 저장소 백업을 채우는지(janitor·GC가 pending 백업을 볼 때)
pub(crate) fn filling(tasks: &[Task], backups: &[Backup], backup: &Backup) -> bool {
    tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .any(|t| match &t.spec.op {
            TaskOp::VmBackup { backup: b } => *b == backup.id,
            TaskOp::BackupConvert { store } => {
                backup.spec.store.as_deref() == Some(store.as_str())
                    && backup
                        .spec
                        .source
                        .as_ref()
                        .is_some_and(|s| s.backup == t.spec.target)
                    && converted(backups, t.spec.target, store).is_some_and(|c| c.id == backup.id)
            }
            TaskOp::BackupCopy { store } => {
                copied(backups, t.spec.target, store).is_some_and(|c| c.id == backup.id)
            }
            _ => false,
        })
}

/// 받는 사본(원격 복제)의 커밋 구간을 쥔 채 이만큼 아무것도 받지 않았으면 남은 잠금으로 본다(완료 요청이 죽었다).
pub(crate) const RECEIVE_COMMIT_STALE: u64 = 600;

/// 쓰기 잠금을 쥔 채 남은 것(§10.3 끝): 커밋 구간의 백업 가운데 더는 `pending`이 아니거나 채우는 Task가 없는 것(받는
/// 사본은 [`RECEIVE_COMMIT_STALE`] 동안 받은 것이 없는 것), 끝난(없는) GC Task의 `gc`·`sweeping`, 검증 Task가 모두 끝난
/// 저장소의 `quarantining`. 바꿀 것이 있으면 고친 상태.
pub(crate) fn stale_locks(
    store: &BackupStore,
    backups: &[Backup],
    tasks: &[Task],
    now: u64,
) -> Option<BackupStoreStatus> {
    let mut status = store.status.clone();
    status.committing.retain(|id| {
        backups.iter().find(|b| b.id == *id).is_some_and(|b| {
            (b.status.phase == BackupPhase::Pending && filling(tasks, backups, b))
                || (b.status.phase == BackupPhase::Receiving
                    && b.status.received_at.unwrap_or(0) + RECEIVE_COMMIT_STALE >= now)
        })
    });
    if let Some(gc) = &status.gc {
        let alive = tasks
            .iter()
            .any(|t| t.id == gc.task && !t.status.state.is_finished());
        if !alive {
            status.gc = None;
            status.sweeping = false;
        }
    } else if status.sweeping {
        status.sweeping = false;
    }
    // 검증이 깨진 청크를 옮기는 잠금(리뷰 1 I5): 이 저장소의 검증 Task가 모두 끝났으면 놓는다.
    if status.quarantining
        && !tasks.iter().any(|t| {
            t.spec.target == store.id
                && matches!(t.spec.op, TaskOp::BackupStoreVerify { .. })
                && !t.status.state.is_finished()
        })
    {
        status.quarantining = false;
    }
    (status != store.status).then_some(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::task::{TaskSpec, TaskState};

    fn backup(id: u128, phase: BackupPhase, base: Option<u128>) -> Backup {
        let spec: BackupSpec = serde_json::from_value(serde_json::json!({
            "vm": Uuid::from_u128(99), "vm_name": "web", "store": "st",
            "vm_spec": {"cpu": {"cores": 1}, "memory_mib": 256, "machine": "virt"}
        }))
        .unwrap();
        let mut b: Backup = Resource::new(format!("b{id}"), spec);
        b.id = Uuid::from_u128(id);
        b.status.phase = phase;
        b.status.base = base.map(Uuid::from_u128);
        b
    }

    fn task(op: TaskOp, target: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "t",
            TaskSpec {
                op,
                target,
                node: Uuid::nil(),
            },
        );
        t.status.state = state;
        t
    }

    #[test]
    fn covered_chunks_are_only_whole_chunks() {
        let c = 4u64;
        // [2, 14): 청크 1·2 전체, 청크 0·3은 일부
        assert_eq!(covered_chunks(&[(2, 12)], c, 16), HashSet::from([1, 2]));
        // 마지막 짧은 청크(크기 10 → 청크 2는 8..10)
        assert_eq!(covered_chunks(&[(8, 2)], c, 10), HashSet::from([2]));
        assert!(covered_chunks(&[(1, 2)], c, 16).is_empty());
    }

    #[test]
    fn bitmap_runs_follow_the_base_chain() {
        let all = vec![
            backup(1, BackupPhase::Ready, None),
            backup(2, BackupPhase::Ready, Some(1)),
            backup(3, BackupPhase::Ready, Some(2)),
        ];
        assert_eq!(bitmap_runs(&all[0], &all), 0);
        assert_eq!(bitmap_runs(&all[2], &all), 2);
        // 고리에서 멈춘다
        let cyc = vec![
            backup(1, BackupPhase::Ready, Some(2)),
            backup(2, BackupPhase::Ready, Some(1)),
        ];
        assert_eq!(bitmap_runs(&cyc[0], &cyc), 2);
    }

    #[test]
    fn base_is_in_use_only_while_a_child_is_pending() {
        let base = backup(1, BackupPhase::Ready, None);
        let pending = backup(2, BackupPhase::Pending, Some(1));
        let ready = backup(3, BackupPhase::Ready, Some(1));
        assert!(base_in_use(&[base.clone(), pending], &base).is_some());
        assert!(base_in_use(&[base.clone(), ready], &base).is_none());
    }

    #[test]
    fn convert_names_fit_and_do_not_clash() {
        assert_eq!(convert_name("web-1", |_| false), "web-1-st");
        assert_eq!(convert_name("web-1", |n| n == "web-1-st"), "web-1-st2");
        let long = "a".repeat(63);
        let n = convert_name(&long, |_| false);
        assert_eq!(n.len(), 63);
        assert!(n.ends_with("-st"));
    }

    #[test]
    fn stale_locks_drop_dead_commits_and_gc_runs() {
        let spec: BackupStoreSpec =
            serde_json::from_value(serde_json::json!({"backend": {"pool": "bk"}})).unwrap();
        let mut store: BackupStore = Resource::new("st", spec);
        let live = backup(1, BackupPhase::Pending, None);
        let dead = backup(2, BackupPhase::Pending, None);
        let done = backup(3, BackupPhase::Ready, None);
        let gc_task = task(TaskOp::BackupStoreGc, store.id, TaskState::Running);
        store.status.committing = vec![live.id, dead.id, done.id, Uuid::from_u128(4)];
        store.status.gc = Some(opl_types::backup_store::GcRun {
            task: gc_task.id,
            started_at: 1,
            marker_at: 1,
            phase: opl_types::backup_store::GcPhase::Sweep,
        });
        store.status.sweeping = true;
        let backups = vec![live.clone(), dead, done];
        let mut tasks = vec![
            task(
                TaskOp::VmBackup { backup: live.id },
                Uuid::nil(),
                TaskState::Running,
            ),
            gc_task.clone(),
        ];
        let fixed = stale_locks(&store, &backups, &tasks, 0).unwrap();
        assert_eq!(fixed.committing, vec![live.id]);
        assert!(fixed.sweeping && fixed.gc.is_some());
        // GC Task가 끝났으면 잠금을 푼다
        tasks[1].status.state = TaskState::Failed;
        store.status = fixed;
        let fixed = stale_locks(&store, &backups, &tasks, 0).unwrap();
        assert!(!fixed.sweeping && fixed.gc.is_none());
        store.status = fixed;
        assert_eq!(stale_locks(&store, &backups, &tasks, 0), None);
        // 검증의 옮기기 잠금(리뷰 1 I5): 그 저장소의 검증 Task가 돌면 두고, 모두 끝났으면 놓는다
        store.status.quarantining = true;
        let verify = TaskOp::BackupStoreVerify { backups: vec![] };
        tasks.push(task(verify, store.id, TaskState::Running));
        assert_eq!(stale_locks(&store, &backups, &tasks, 0), None);
        tasks[2].status.state = TaskState::Succeeded;
        assert!(
            !stale_locks(&store, &backups, &tasks, 0)
                .unwrap()
                .quarantining
        );
    }
}
