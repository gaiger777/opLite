//! redb 파일 하나에 Raft 로그와 상태 머신을 함께 저장한다.

use std::path::Path;
use std::sync::Arc;

use opl_types::resource::Kind;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::log_store::LogStore;
use crate::state_machine::StateMachine;
use crate::types::RawResource;
use crate::watch::WatchHub;

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

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] redb::Error),
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported schema version {found} (this build supports {SCHEMA_VERSION})")]
    UnsupportedSchema { found: u32 },
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
pub struct Store {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
}

impl Store {
    /// 파일이 없으면 만들고, 있으면 스키마 버전을 확인한다.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let db = Database::create(path)?;
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
        Ok(Self {
            db,
            watch: Arc::new(WatchHub::new(applied)),
        })
    }

    pub fn log_store(&self) -> LogStore {
        LogStore::new(self.db.clone())
    }

    pub fn state_machine(&self) -> StateMachine {
        StateMachine::new(self.db.clone(), self.watch.clone())
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
