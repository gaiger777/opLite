//! 검증 엔진(설계 §10.5). 스냅샷마다 매니페스트(인증)·`config` 블롭·인덱스(SHA-256·꼬리 해시)·디스크 수와 크기를 보고,
//! `full`이면 청크를 읽어 AEAD 태그·압축·id를 다시 맞추고 `metadata`면 `HEAD`(있음·크기)만 본다. 한 실행 안에서 확인한
//! id는 다시 읽지 않는다. 깨진 청크는 `bad/<id>.<초>`로 옮긴다(지우지 않음 — 읽기 전용 저장소면 옮기지 않음). 다음
//! 백업이 같은 내용을 만나면 "있음 확인"이 없다고 보고 다시 올린다(스스로 낫기).

use std::collections::HashSet;

use futures_util::{StreamExt, TryStreamExt};
use orbivisor_types::backup_store::{ChunkId, VerifyMode, ZERO_CHUNK};
use uuid::Uuid;

use crate::backend::StoreError;
use crate::format::chunk::{self, min_object_bytes};
use crate::format::manifest::{CONFIG_BLOB, ConfigBlob};
use crate::format::{bad_key, chunk_key};
use crate::store::Store;

/// 리소스가 아는 디스크(매니페스트와 견줌)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedDisk {
    pub name: String,
    pub virtual_bytes: u64,
}

/// 스냅샷 하나의 검증 결과
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotReport {
    pub backup: Uuid,
    /// 이 스냅샷이 가리키는 깨지거나 없는 청크(이 실행에서 앞서 찾은 것 포함)
    pub bad_chunks: Vec<ChunkId>,
    /// 이 스냅샷에서 새로 확인한 청크 수
    pub chunks_checked: u64,
    /// 매니페스트·블롭·인덱스 문제(있으면 실패)
    pub error: Option<String>,
}

impl SnapshotReport {
    pub fn ok(&self) -> bool {
        self.bad_chunks.is_empty() && self.error.is_none()
    }
}

/// 청크 하나를 확인한 결과
enum ChunkState {
    Good,
    Missing,
    Corrupt,
}

/// 한 검증 실행. 실행 안에서 확인한 id·깨진 id를 기억한다.
pub struct Verifier<'a> {
    store: &'a Store,
    mode: VerifyMode,
    /// 깨진 청크를 `bad/`로 옮기나(읽기 전용 저장소면 거짓)
    quarantine: bool,
    now: u64,
    checked: HashSet<ChunkId>,
    bad: HashSet<ChunkId>,
    /// 옮길 깨진 청크([`Verifier::quarantine`]가 옮긴다 — 부르는 쪽이 커밋 구간을 막은 뒤, 리뷰 1 I5)
    pending: Vec<ChunkId>,
    /// 옮긴 청크 수
    pub moved: u64,
}

impl<'a> Verifier<'a> {
    pub fn new(store: &'a Store, mode: VerifyMode, quarantine: bool, now: u64) -> Verifier<'a> {
        Verifier {
            store,
            mode,
            quarantine,
            now,
            checked: HashSet::new(),
            bad: HashSet::new(),
            pending: Vec::new(),
            moved: 0,
        }
    }

    /// 이 실행이 확인한 고유 청크 수
    pub fn chunks_checked(&self) -> u64 {
        self.checked.len() as u64
    }

    /// 이 실행이 찾은 깨진(또는 없는) 청크
    pub fn bad_chunks(&self) -> &HashSet<ChunkId> {
        &self.bad
    }

    /// 옮길 깨진 청크가 있는지([`Verifier::quarantine`])
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// 찾은 깨진 청크를 `bad/`로 옮긴다(지우지 않음). 옮기는 동안 그 청크를 "있음"으로 건너뛴 백업이 커밋하지 않게 부르는
    /// 쪽이 커밋 구간을 막는다(리뷰 1 I5 — 커밋은 `bad/`의 id를 다시 본다). 옮긴 수.
    pub async fn quarantine(&mut self) -> Result<u64, StoreError> {
        let mut moved = 0;
        while let Some(id) = self.pending.pop() {
            match self
                .store
                .backend()
                .rename(&chunk_key(&id), &bad_key(&id, self.now))
                .await
            {
                Ok(()) => moved += 1,
                Err(StoreError::NotFound(_)) => {}
                Err(e) => {
                    self.pending.push(id);
                    return Err(e);
                }
            }
        }
        self.moved += moved;
        Ok(moved)
    }

    /// 스냅샷 하나를 검증한다. 내용 문제는 보고서에, 저장소에 닿지 못함 따위는 `Err`(실행을 멈춘다).
    pub async fn verify_snapshot(
        &mut self,
        vm: Uuid,
        backup: Uuid,
        expected: Option<&[ExpectedDisk]>,
    ) -> Result<SnapshotReport, StoreError> {
        let mut report = SnapshotReport {
            backup,
            ..Default::default()
        };
        let content = |e: StoreError| -> Result<String, StoreError> {
            match e {
                StoreError::NotFound(_) | StoreError::Corrupt(_) | StoreError::KeyMismatch(_) => {
                    Ok(e.to_string())
                }
                e => Err(e),
            }
        };
        let m = match self.store.read_manifest(vm, backup).await {
            Ok(m) => m,
            Err(e) => {
                report.error = Some(format!("manifest: {}", content(e)?));
                return Ok(report);
            }
        };
        match self.store.get_blob(&m, CONFIG_BLOB).await {
            Ok(raw) => {
                if let Err(e) = serde_json::from_slice::<ConfigBlob>(&raw) {
                    report.error = Some(format!("config: {e}"));
                    return Ok(report);
                }
            }
            Err(e) => {
                report.error = Some(content(e)?);
                return Ok(report);
            }
        }
        for b in m.blobs.iter().filter(|b| b.name != CONFIG_BLOB) {
            if let Err(e) = self.store.get_blob(&m, &b.name).await {
                report.error = Some(content(e)?);
                return Ok(report);
            }
        }
        if let Some(exp) = expected {
            let got: Vec<ExpectedDisk> = m
                .disks
                .iter()
                .map(|d| ExpectedDisk {
                    name: d.name.clone(),
                    virtual_bytes: d.virtual_bytes,
                })
                .collect();
            if got != exp {
                report.error = Some("the disks in the store do not match the backup".into());
                return Ok(report);
            }
        }
        let mut ids: Vec<ChunkId> = Vec::new();
        for d in &m.disks {
            match self.store.read_index(&m, d).await {
                Ok(ix) => {
                    if ix.ids.len() as u64 != d.chunks
                        || ix.ids.iter().filter(|i| **i == ZERO_CHUNK).count() as u64
                            != d.zero_chunks
                    {
                        report.error = Some(format!(
                            "index {}: counts do not match the manifest",
                            d.index
                        ));
                        return Ok(report);
                    }
                    ids.extend(ix.ids.into_iter().filter(|i| *i != ZERO_CHUNK));
                }
                Err(e) => {
                    report.error = Some(content(e)?);
                    return Ok(report);
                }
            }
        }
        let mut uniq: HashSet<ChunkId> = HashSet::new();
        ids.retain(|i| uniq.insert(*i));
        let todo: Vec<ChunkId> = ids
            .iter()
            .filter(|i| !self.checked.contains(*i))
            .copied()
            .collect();
        report.chunks_checked = todo.len() as u64;
        let store = self.store;
        let mode = self.mode;
        let results: Vec<(ChunkId, ChunkState)> = futures_util::stream::iter(todo)
            .map(
                |id| async move { Ok::<_, StoreError>((id, check_chunk(store, mode, &id).await?)) },
            )
            .buffer_unordered(store.max_streams())
            .try_collect()
            .await?;
        for (id, state) in results {
            self.checked.insert(id);
            match state {
                ChunkState::Good => {}
                ChunkState::Missing => {
                    self.bad.insert(id);
                }
                ChunkState::Corrupt => {
                    self.bad.insert(id);
                    if self.quarantine {
                        self.pending.push(id);
                    }
                }
            }
        }
        report.bad_chunks = ids.into_iter().filter(|i| self.bad.contains(i)).collect();
        Ok(report)
    }
}

async fn check_chunk(
    store: &Store,
    mode: VerifyMode,
    id: &ChunkId,
) -> Result<ChunkState, StoreError> {
    match mode {
        VerifyMode::Metadata => match store.backend().head(&chunk_key(id)).await? {
            None => Ok(ChunkState::Missing),
            Some(m) if m.size < min_object_bytes(store.encrypted()) => Ok(ChunkState::Corrupt),
            Some(_) => Ok(ChunkState::Good),
        },
        VerifyMode::Full => {
            let obj = match store.get_chunk_object(id).await {
                Ok(o) => o,
                Err(StoreError::NotFound(_)) => return Ok(ChunkState::Missing),
                Err(e) => return Err(e),
            };
            Ok(match chunk::decode(&obj, id, store.keys()) {
                Ok(_) => ChunkState::Good,
                Err(_) => ChunkState::Corrupt,
            })
        }
    }
}
