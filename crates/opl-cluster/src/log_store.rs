use std::fmt::Debug;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

use openraft::storage::{LogFlushed, RaftLogStorage};
use openraft::{
    Entry, LogId, LogState, OptionalSend, RaftLogReader, StorageError, StorageIOError, Vote,
};
use redb::{Database, ReadableDatabase, ReadableTable};

use crate::store::{
    KEY_COMMITTED, KEY_LAST_PURGED, KEY_VOTE, LOG, META, StoreError, get_json, put_json,
};
use crate::types::{NodeId, TypeConfig};

/// Raft 로그 저장소. 모든 쓰기는 커밋(fsync)이 끝난 뒤에 완료를 알린다.
#[derive(Clone)]
pub struct LogStore {
    db: Arc<Database>,
}

impl LogStore {
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn read_meta<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        get_json(&meta, key)
    }

    fn write_meta<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            put_json(&mut meta, key, value)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn read_entries(
        &self,
        range: (Bound<u64>, Bound<u64>),
    ) -> Result<Vec<Entry<TypeConfig>>, StoreError> {
        let txn = self.db.begin_read()?;
        let log = txn.open_table(LOG)?;
        let mut out = Vec::new();
        for item in log.range::<u64>(range)? {
            let (_, v) = item?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    fn last_entry_id(&self) -> Result<Option<LogId<NodeId>>, StoreError> {
        let txn = self.db.begin_read()?;
        let log = txn.open_table(LOG)?;
        match log.last()? {
            Some((_, v)) => {
                let entry: Entry<TypeConfig> = serde_json::from_slice(v.value())?;
                Ok(Some(entry.log_id))
            }
            None => Ok(None),
        }
    }

    fn append_entries(&self, entries: Vec<Entry<TypeConfig>>) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut log = txn.open_table(LOG)?;
            for entry in entries {
                log.insert(entry.log_id.index, serde_json::to_vec(&entry)?.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// `index` 이상의 엔트리를 지운다(충돌한 로그 잘라내기).
    fn truncate_from(&self, index: u64) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut log = txn.open_table(LOG)?;
            log.retain_in(index.., |_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// `log_id`까지의 엔트리를 지우고 마지막으로 지운 id를 기록한다.
    fn purge_upto(&self, log_id: &LogId<NodeId>) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            put_json(&mut meta, KEY_LAST_PURGED, log_id)?;
            let mut log = txn.open_table(LOG)?;
            log.retain_in(..=log_id.index, |_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<NodeId>> {
        let range = (range.start_bound().cloned(), range.end_bound().cloned());
        self.read_entries(range)
            .map_err(|e| StorageIOError::read_logs(&e).into())
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let last_purged: Option<LogId<NodeId>> = self
            .read_meta(KEY_LAST_PURGED)
            .map_err(|e| StorageIOError::read_logs(&e))?;
        let last = self
            .last_entry_id()
            .map_err(|e| StorageIOError::read_logs(&e))?
            .or(last_purged);
        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id: last,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.write_meta(KEY_VOTE, vote)
            .map_err(|e| StorageIOError::write_vote(&e).into())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        self.read_meta(KEY_VOTE)
            .map_err(|e| StorageIOError::read_vote(&e).into())
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogId<NodeId>>,
    ) -> Result<(), StorageError<NodeId>> {
        self.write_meta(KEY_COMMITTED, &committed)
            .map_err(|e| StorageIOError::write(&e).into())
    }

    async fn read_committed(&mut self) -> Result<Option<LogId<NodeId>>, StorageError<NodeId>> {
        let committed: Option<Option<LogId<NodeId>>> = self
            .read_meta(KEY_COMMITTED)
            .map_err(|e| StorageIOError::read(&e))?;
        Ok(committed.flatten())
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        self.append_entries(entries.into_iter().collect())
            .map_err(|e| StorageIOError::write_logs(&e))?;
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.truncate_from(log_id.index)
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.purge_upto(&log_id)
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }
}
