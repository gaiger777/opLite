//! GC 표시·쓸기 엔진(설계 §10.2~§10.4). Raft 쪽 걸음(`status.gc` 쓰기 L, W 기다림, `committing`이 빌 때 쓸기 잠금,
//! `sweep_epoch`)은 orbivisor-api의 GC Task가 하고, 이 모듈은 저장소 쪽 걸음만 한다:
//!
//! 1. [`start`] — 표식 `gc/<run>.start`를 쓰고 그 시각(저장소 시계)을 T0로
//! 2. [`Marker::mark`] — `snapshots/` 아래 아직 읽지 않은 모든 `.fidx`(매니페스트 유무와 관계없이)의 id를 Bloom 필터에.
//!    기다림 중에도, 쓸기 잠금 뒤 늦은 표시에도 같은 함수를 다시 부른다(새 인덱스만 읽는다).
//! 3. [`sweep`] — `chunks/`를 나열해 표시된 것과 T0 − 5분 뒤에 올라온 것은 남기고 나머지를 지운다. 하루 넘은 임시
//!    파일도 지운다. `bad/`·`keys/`·`snapshots/`는 건드리지 않는다.
//! 4. [`finish`] — 표식을 지운다.

use std::collections::HashSet;

use futures_util::{StreamExt, TryStreamExt};
use orbivisor_types::backup_store::{ChunkId, ZERO_CHUNK};
use uuid::Uuid;

use crate::backend::{ObjectInfo, StoreError};
use crate::bloom::{Bloom, FALSE_POSITIVE};
use crate::format::index::entries_from_size;
use crate::format::{gc_marker_key, parse_chunk_key};
use crate::store::Store;

/// T0보다 이만큼 앞서 올라온 청크까지 남긴다(노드·저장소 사이 시각 어긋남과 쓰기 지연)
pub const YOUNG_GRACE_SECS: u64 = 300;
/// 남은 임시 파일을 지우는 나이
pub const TMP_MAX_AGE_SECS: u64 = 86_400;

/// GC 실행의 시작(표식과 T0)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcStart {
    pub run: Uuid,
    /// 표식의 `LastModified`(저장소 시계, 유닉스 초)
    pub t0: u64,
}

/// 표식을 쓰고 T0를 읽는다.
pub async fn start(store: &Store, run: Uuid) -> Result<GcStart, StoreError> {
    let key = gc_marker_key(run);
    store
        .backend()
        .put(&key, run.to_string().into_bytes())
        .await?;
    let meta = store.backend().head(&key).await?.ok_or_else(|| {
        StoreError::Invalid("the GC marker vanished right after writing it".into())
    })?;
    Ok(GcStart {
        run,
        t0: meta.modified,
    })
}

/// 표식을 지운다.
pub async fn finish(store: &Store, start: &GcStart) -> Result<(), StoreError> {
    store.backend().delete(&gc_marker_key(start.run)).await
}

/// Bloom 필터의 용량: 앞 GC의 청크 수(¼ 여유)가 있으면 그것, 없으면 지금 인덱스 크기의 합(겹친 id까지 — 넉넉함)
pub async fn capacity(store: &Store, previous_chunks: Option<u64>) -> Result<u64, StoreError> {
    if let Some(n) = previous_chunks {
        return Ok(n + n / 4);
    }
    let snaps = store.list_snapshots().await?;
    Ok(snaps
        .iter()
        .flat_map(|s| s.indexes.iter())
        .map(|o| entries_from_size(o.size))
        .sum())
}

/// 표시 집합과 읽은 인덱스
#[derive(Debug)]
pub struct Marker {
    bloom: Bloom,
    seen: HashSet<String>,
    /// 읽은 인덱스 수·0이 아닌 청크 자리 수·가상 바이트 합(`usage`)
    pub indexes: u64,
    pub entries: u64,
    pub logical_bytes: u64,
    /// 매니페스트가 있는 스냅샷 수(마지막 [`Marker::mark`] 때)
    pub snapshots: u64,
}

impl Marker {
    /// `seed`는 실행마다 다르게(예: 실행 id에서)
    pub fn new(capacity: u64, seed: u64) -> Marker {
        Marker {
            bloom: Bloom::new(capacity, FALSE_POSITIVE, seed),
            seen: HashSet::new(),
            indexes: 0,
            entries: 0,
            logical_bytes: 0,
            snapshots: 0,
        }
    }

    pub fn contains(&self, id: &ChunkId) -> bool {
        self.bloom.contains(id)
    }

    /// id를 직접 더한다(부르는 쪽이 아는 인덱스)
    pub fn insert(&mut self, id: &ChunkId) {
        if *id != ZERO_CHUNK {
            self.bloom.insert(id);
        }
    }

    /// 아직 읽지 않은 모든 `.fidx`를 읽어 표시한다. 읽은 인덱스 수를 돌려준다. 인덱스 하나라도 읽지 못하면 오류 —
    /// 그 인덱스의 청크를 지우게 되므로 GC를 멈춘다.
    pub async fn mark(&mut self, store: &Store) -> Result<u64, StoreError> {
        let snaps = store.list_snapshots().await?;
        self.snapshots = snaps.iter().filter(|s| s.complete).count() as u64;
        let todo: Vec<String> = snaps
            .iter()
            .flat_map(|s| s.indexes.iter().map(|o| o.key.clone()))
            .filter(|k| !self.seen.contains(k))
            .collect();
        let read: Vec<(String, Option<crate::format::index::FixedIndex>)> =
            futures_util::stream::iter(todo)
                .map(|key| async move {
                    match store.read_index_key(&key).await {
                        Ok(ix) => Ok((key, Some(ix))),
                        // 나열과 읽기 사이에 지워졌다(백업 지우기) — 그 청크는 지켜야 할 것이 아니다
                        Err(StoreError::NotFound(_)) => Ok((key, None)),
                        Err(e) => Err(e),
                    }
                })
                .buffer_unordered(store.max_streams())
                .try_collect()
                .await?;
        let n = read.len() as u64;
        for (key, ix) in read {
            if let Some(ix) = ix {
                for id in &ix.ids {
                    if *id != ZERO_CHUNK {
                        self.bloom.insert(id);
                        self.entries += 1;
                    }
                }
                self.indexes += 1;
                self.logical_bytes += ix.virtual_bytes;
            }
            self.seen.insert(key);
        }
        Ok(n)
    }
}

/// 쓸기에서 객체 하나를 어떻게 하나
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepDecision {
    /// 표시됨(참조 — 또는 Bloom 오탐)
    Keep,
    /// 표시되지 않았지만 T0 − 5분 뒤에 올라왔다
    KeepYoung,
    /// 지운다
    Remove,
    /// 하루 넘은 임시 파일 — 지운다
    RemoveTmp,
    /// 청크도 임시 파일도 아니다(또는 젊은 임시 파일) — 건드리지 않는다
    Ignore,
}

/// 순수 판정(설계 §10.2 5번)
pub fn sweep_decision(o: &ObjectInfo, marked: impl Fn(&ChunkId) -> bool, t0: u64) -> SweepDecision {
    match parse_chunk_key(&o.key) {
        Some(id) => {
            if marked(&id) {
                SweepDecision::Keep
            } else if o.modified >= t0.saturating_sub(YOUNG_GRACE_SECS) {
                SweepDecision::KeepYoung
            } else {
                SweepDecision::Remove
            }
        }
        None if o.key.contains(".tmp.") && o.modified < t0.saturating_sub(TMP_MAX_AGE_SECS) => {
            SweepDecision::RemoveTmp
        }
        None => SweepDecision::Ignore,
    }
}

/// 쓸기 결과
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepStats {
    pub removed_chunks: u64,
    pub removed_bytes: u64,
    pub kept_young: u64,
    /// 남은(표시된) 청크와 그 저장 바이트(`usage`)
    pub chunks: u64,
    pub stored_bytes: u64,
    pub removed_tmp: u64,
}

/// `chunks/`를 쓴다. 쓸기 잠금(`sweeping`)을 잡은 뒤 늦은 표시까지 한 다음에만 부른다.
pub async fn sweep(
    store: &Store,
    marker: &Marker,
    start: &GcStart,
) -> Result<SweepStats, StoreError> {
    let mut stats = SweepStats::default();
    let mut token = None;
    loop {
        let page = store.backend().list("chunks/", token).await?;
        let mut doomed = Vec::new();
        for o in page.objects {
            match sweep_decision(&o, |id| marker.contains(id), start.t0) {
                SweepDecision::Keep => {
                    stats.chunks += 1;
                    stats.stored_bytes += o.size;
                }
                SweepDecision::KeepYoung => {
                    stats.kept_young += 1;
                    stats.chunks += 1;
                    stats.stored_bytes += o.size;
                }
                SweepDecision::Remove => {
                    stats.removed_chunks += 1;
                    stats.removed_bytes += o.size;
                    doomed.push(o.key);
                }
                SweepDecision::RemoveTmp => {
                    stats.removed_tmp += 1;
                    doomed.push(o.key);
                }
                SweepDecision::Ignore => {}
            }
        }
        futures_util::stream::iter(doomed)
            .map(|k| async move { store.backend().delete(&k).await })
            .buffer_unordered(store.max_streams())
            .try_collect::<Vec<()>>()
            .await?;
        match page.next {
            Some(t) => token = Some(t),
            None => return Ok(stats),
        }
    }
}

/// 실행 id에서 Bloom 씨앗
pub fn seed_for(run: Uuid) -> u64 {
    let b = run.as_bytes();
    u64::from_le_bytes(b[..8].try_into().expect("8 bytes"))
        ^ u64::from_le_bytes(b[8..].try_into().expect("8 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::chunk_key;

    fn obj(key: &str, modified: u64) -> ObjectInfo {
        ObjectInfo {
            key: key.into(),
            size: 10,
            modified,
        }
    }

    #[test]
    fn decisions() {
        let id = [0xab; 32];
        let k = chunk_key(&id);
        let t0 = 1_000_000;
        assert_eq!(
            sweep_decision(&obj(&k, 0), |_| true, t0),
            SweepDecision::Keep
        );
        assert_eq!(
            sweep_decision(&obj(&k, t0 - 300), |_| false, t0),
            SweepDecision::KeepYoung
        );
        assert_eq!(
            sweep_decision(&obj(&k, t0 + 5), |_| false, t0),
            SweepDecision::KeepYoung
        );
        assert_eq!(
            sweep_decision(&obj(&k, t0 - 301), |_| false, t0),
            SweepDecision::Remove
        );
        let tmp = format!("{k}.tmp.0011");
        assert_eq!(
            sweep_decision(&obj(&tmp, t0 - 86_401), |_| false, t0),
            SweepDecision::RemoveTmp
        );
        assert_eq!(
            sweep_decision(&obj(&tmp, t0 - 100), |_| false, t0),
            SweepDecision::Ignore
        );
        // 시각을 읽지 못한 S3 목록 항목(`u64::MAX`, 리뷰 1 I6)은 새것으로 남긴다
        assert_eq!(
            sweep_decision(&obj(&k, u64::MAX), |_| false, t0),
            SweepDecision::KeepYoung
        );
        assert_eq!(
            sweep_decision(&obj(&tmp, u64::MAX), |_| false, t0),
            SweepDecision::Ignore
        );
        assert_eq!(
            sweep_decision(&obj("chunks/x/readme", 0), |_| false, t0),
            SweepDecision::Ignore
        );
        assert_ne!(seed_for(Uuid::from_u128(1)), seed_for(Uuid::from_u128(2)));
    }
}
