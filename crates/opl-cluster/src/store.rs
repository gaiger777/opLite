//! redb 파일 하나에 Raft 로그와 상태 머신을 함께 저장한다.

use std::path::Path;
use std::sync::Arc;

use opl_types::resource::Kind;
use redb::backends::InMemoryBackend;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::log_store::LogStore;
use crate::state_machine::StateMachine;
use crate::types::RawResource;
use crate::watch::WatchHub;
use crate::writer::StoreWriter;

/// 저장 형식 버전. redb 파일과 스냅샷에 기록한다. 형식이 바뀌면 올리고 마이그레이션을 추가한다.
pub const SCHEMA_VERSION: u32 = 1;

/// Raft 로그: 인덱스 → 엔트리(JSON)
pub(crate) const LOG: TableDefinition<u64, &[u8]> = TableDefinition::new("raft_log");
/// 메타데이터: 키 → 값(JSON). 키 목록은 아래 `KEY_*` 상수.
pub(crate) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
/// 리소스: (종류, id) → RawResource(JSON)
pub(crate) const OBJECTS: TableDefinition<(&str, u128), &[u8]> = TableDefinition::new("objects");
/// 이름 색인: (종류, 이름) → id
pub(crate) const NAMES: TableDefinition<(&str, &str), u128> = TableDefinition::new("names");

pub(crate) const KEY_SCHEMA_VERSION: &str = "schema_version";
pub(crate) const KEY_VOTE: &str = "vote";
pub(crate) const KEY_COMMITTED: &str = "committed";
pub(crate) const KEY_LAST_PURGED: &str = "last_purged";
pub(crate) const KEY_LAST_APPLIED: &str = "last_applied";
pub(crate) const KEY_LAST_MEMBERSHIP: &str = "last_membership";
pub(crate) const KEY_SNAPSHOT_META: &str = "snapshot_meta";
pub(crate) const KEY_SNAPSHOT_DATA: &str = "snapshot_data";
/// 이 노드가 무엇으로 참여하는지([`NodeMode`]). 없으면 `Member`다(계획 48 전의 저장소).
pub(crate) const KEY_NODE_MODE: &str = "node_mode";
/// 관찰 노드가 되며 지운 Raft 로그의 마지막 인덱스(지금까지 가장 큰 것). 다시 Raft를 띄우기 전에 여기까지 적용한다
/// ([`enter_observer_mode`]).
pub(crate) const KEY_DROPPED_LOG: &str = "dropped_log";

/// 저장소를 어떤 백엔드로 여는지(설계 §6)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeMode {
    /// Raft 구성원(투표 멤버·learner)으로 Raft를 띄운다.
    #[default]
    Member,
    /// Raft 밖의 관찰 노드로 투표 멤버 하나를 따른다. Raft 로그는 비어 있다.
    Observer,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] redb::Error),
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported schema version {found} (this build supports {SCHEMA_VERSION})")]
    UnsupportedSchema { found: u32 },
    #[error("cannot start the store thread: {0}")]
    Thread(std::io::Error),
    #[error("the store thread has stopped")]
    WriterStopped,
    #[error("a store write panicked")]
    WriterPanicked,
    /// 계획된 전환(관찰 노드 ↔ 멤버)으로 물러난 Raft의 상태 머신이 쓰려 했다.
    #[error("the state machine was retired by a mode change")]
    Retired,
}

macro_rules! from_redb {
    ($($t:ty),*) => {$(
        impl From<$t> for StoreError {
            fn from(e: $t) -> Self {
                StoreError::Db(e.into())
            }
        }
    )*};
}
from_redb!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);

/// 노드 하나의 저장소. `log_store()`와 `state_machine()`을 openraft에 넘긴다.
/// 쓰기 트랜잭션은 저장소 스레드(`orbivisor-store`)에서 돈다.
pub struct Store {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
    writer: StoreWriter,
}

impl Store {
    /// 파일이 없으면 만들고, 있으면 스키마 버전을 확인한다.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::init(Database::create(path)?)
    }

    /// 메모리에만 두는 저장소(fsync 없음, 닫으면 사라짐). 시험용이다: 규모 시험이 한 디스크에 노드 수백 개의
    /// fsync가 몰리지 않게 모든 노드(투표 멤버도)에 쓴다(실제 노드는 디스크가 따로다).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Database::builder().create_with_backend(InMemoryBackend::new())?)
    }

    fn init(db: Database) -> Result<Self, StoreError> {
        let txn = db.begin_write()?;
        {
            txn.open_table(LOG)?;
            txn.open_table(OBJECTS)?;
            txn.open_table(NAMES)?;
            let mut meta = txn.open_table(META)?;
            let found: Option<u32> = get_json(&meta, KEY_SCHEMA_VERSION)?;
            match found {
                None => put_json(&mut meta, KEY_SCHEMA_VERSION, &SCHEMA_VERSION)?,
                Some(SCHEMA_VERSION) => {}
                Some(found) => return Err(StoreError::UnsupportedSchema { found }),
            }
        }
        txn.commit()?;
        let db = Arc::new(db);
        let applied = last_applied_index(&db)?;
        let writer = StoreWriter::spawn(&db)?;
        Ok(Self {
            db,
            watch: Arc::new(WatchHub::new(applied)),
            writer,
        })
    }

    pub fn log_store(&self) -> LogStore {
        LogStore::new(self.db.clone(), self.writer.clone())
    }

    pub fn state_machine(&self) -> StateMachine {
        StateMachine::new(self.db.clone(), self.watch.clone(), self.writer.clone())
    }

    pub(crate) fn writer(&self) -> &StoreWriter {
        &self.writer
    }

    pub(crate) fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub(crate) fn watch_hub(&self) -> &Arc<WatchHub> {
        &self.watch
    }
}

pub(crate) fn get_json<T: DeserializeOwned>(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    key: &str,
) -> Result<Option<T>, StoreError> {
    match table.get(key)? {
        Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
        None => Ok(None),
    }
}

pub(crate) fn put_json<T: Serialize>(
    table: &mut redb::Table<&'static str, &'static [u8]>,
    key: &str,
    value: &T,
) -> Result<(), StoreError> {
    table.insert(key, serde_json::to_vec(value)?.as_slice())?;
    Ok(())
}

fn last_applied_index(db: &Database) -> Result<u64, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    let applied: Option<openraft::LogId<crate::types::NodeId>> = get_json(&meta, KEY_LAST_APPLIED)?;
    Ok(applied.map_or(0, |l| l.index))
}

/// 저장된 모드. 없으면 `Member`다.
pub(crate) fn read_node_mode(db: &Database) -> Result<NodeMode, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    Ok(get_json(&meta, KEY_NODE_MODE)?.unwrap_or_default())
}

/// 관찰 노드가 된다: 한 트랜잭션에서 모드를 `Observer`로 적고 Raft 로그와 저장된 커밋 위치를 지운다. 투표(vote)는
/// 남긴다(다시 투표 멤버가 되어도 같은 임기에 두 번 투표하지 않게). 상태 머신(리소스·`last_applied`·멤버십·스냅샷)은
/// 그대로라 복제본이 그 적용 위치부터 이어 받는다. 커밋 위치를 지우는 것은 다시 Raft를 띄울 때 openraft가 지운
/// 로그를 다시 적용하려 하지 않게 하려는 것이다(없으면 적용 위치를 커밋으로 본다).
///
/// 지운 로그의 마지막 인덱스는 `KEY_DROPPED_LOG`에 남긴다. 리더는 이 노드가 받았다고 답한 로그까지를 진행
/// (`matching`)으로 쥐고, 이 노드가 멤버십에서 빠지기 전에 관찰 노드가 되면 그 진행을 그대로 쥔다. 다시 Raft를 띄운
/// 이 노드의 로그(적용 위치까지)가 그보다 짧으면 openraft 0.9.25 리더가 그것을 로그가 뒤로 간 것으로 본다(디버그
/// 빌드는 `follower log reversion` panic, 그 밖에는 이 노드가 따라잡지 못할 수 있다).
pub(crate) fn enter_observer_mode(db: &Database) -> Result<(), StoreError> {
    let txn = db.begin_write()?;
    {
        let mut meta = txn.open_table(META)?;
        put_json(&mut meta, KEY_NODE_MODE, &NodeMode::Observer)?;
        meta.remove(KEY_COMMITTED)?;
        let mut log = txn.open_table(LOG)?;
        let last = log.last()?.map_or(0, |(index, _)| index.value());
        let dropped = get_json::<u64>(&meta, KEY_DROPPED_LOG)?
            .unwrap_or(0)
            .max(last);
        put_json(&mut meta, KEY_DROPPED_LOG, &dropped)?;
        log.retain(|_, _| false)?;
    }
    txn.commit()?;
    Ok(())
}

/// 관찰 노드가 되며 지운 Raft 로그의 마지막 인덱스. 지운 적이 없으면 0이다.
pub(crate) fn read_dropped_log(db: &Database) -> Result<u64, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    Ok(get_json(&meta, KEY_DROPPED_LOG)?.unwrap_or(0))
}

/// 모드를 적는다.
pub(crate) fn write_node_mode(db: &Database, mode: NodeMode) -> Result<(), StoreError> {
    let txn = db.begin_write()?;
    put_json(&mut txn.open_table(META)?, KEY_NODE_MODE, &mode)?;
    txn.commit()?;
    Ok(())
}

pub(crate) fn get_object(
    db: &Database,
    kind: Kind,
    id: Uuid,
) -> Result<Option<RawResource>, StoreError> {
    let txn = db.begin_read()?;
    let objects = txn.open_table(OBJECTS)?;
    match objects.get((kind.as_str(), id.as_u128()))? {
        Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
        None => Ok(None),
    }
}

pub(crate) fn get_object_by_name(
    db: &Database,
    kind: Kind,
    name: &str,
) -> Result<Option<RawResource>, StoreError> {
    let txn = db.begin_read()?;
    let names = txn.open_table(NAMES)?;
    let Some(id) = names.get((kind.as_str(), name))? else {
        return Ok(None);
    };
    let objects = txn.open_table(OBJECTS)?;
    match objects.get((kind.as_str(), id.value()))? {
        Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
        None => Ok(None),
    }
}

/// 한 종류의 리소스 전체와, 그 목록이 반영하는 revision(마지막 적용 인덱스).
pub(crate) fn list_objects(
    db: &Database,
    kind: Kind,
) -> Result<(u64, Vec<RawResource>), StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    let applied: Option<openraft::LogId<crate::types::NodeId>> = get_json(&meta, KEY_LAST_APPLIED)?;
    let objects = txn.open_table(OBJECTS)?;
    let mut out = Vec::new();
    for item in objects.range((kind.as_str(), 0u128)..=(kind.as_str(), u128::MAX))? {
        let (_, v) = item?;
        out.push(serde_json::from_slice(v.value())?);
    }
    Ok((applied.map_or(0, |l| l.index), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_store_records_schema_version_and_reopens() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("db");
        drop(Store::open(&path).unwrap());
        drop(Store::open(&path).unwrap());
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("db");
        drop(Store::open(&path).unwrap());
        {
            let db = Database::create(&path).unwrap();
            let txn = db.begin_write().unwrap();
            put_json(
                &mut txn.open_table(META).unwrap(),
                KEY_SCHEMA_VERSION,
                &99u32,
            )
            .unwrap();
            txn.commit().unwrap();
        }
        assert!(matches!(
            Store::open(&path),
            Err(StoreError::UnsupportedSchema { found: 99 })
        ));
    }
}
