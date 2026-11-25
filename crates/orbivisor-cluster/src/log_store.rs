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
use crate::writer::StoreWriter;

/// Raft 로그 저장소. 쓰기는 저장소 스레드에서 돌고, 커밋(fsync)이 끝난 뒤에 완료를 알린다.
/// `save_committed`만 기다리지 않는다(아래 참고).
#[derive(Clone)]
pub struct LogStore {
    db: Arc<Database>,
    writer: StoreWriter,
}

impl LogStore {
    pub(crate) fn new(db: Arc<Database>, writer: StoreWriter) -> Self {
        Self { db, writer }
    }

    fn read_meta<T: serde::de::DeserializeOwned>(
        db: &Database,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        let txn = db.begin_read()?;
        let meta = txn.open_table(META)?;
        get_json(&meta, key)
    }

    fn write_meta<T: serde::Serialize>(
        db: &Database,
        key: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
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

    fn append_entries(db: &Database, entries: Vec<Entry<TypeConfig>>) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
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
    fn truncate_from(db: &Database, index: u64) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut log = txn.open_table(LOG)?;
            log.retain_in(index.., |_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// `log_id`까지의 엔트리를 지우고 마지막으로 지운 id를 기록한다.
    fn purge_upto(db: &Database, log_id: &LogId<NodeId>) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
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
        let last_purged: Option<LogId<NodeId>> = Self::read_meta(&self.db, KEY_LAST_PURGED)
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
        let vote = *vote;
        self.writer
            .run(move |db| Self::write_meta(db, KEY_VOTE, &vote))
            .await
            .map_err(|e| StorageIOError::write_vote(&e).into())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        Self::read_meta(&self.db, KEY_VOTE).map_err(|e| StorageIOError::read_vote(&e).into())
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogId<NodeId>>,
    ) -> Result<(), StorageError<NodeId>> {
        // 기다리지 않는다. openraft는 이것을 Raft 루프 안에서 부르므로, 앞서 넣은 느린 쓰기(상태 머신
        // 적용 등) 뒤에서 기다리면 그동안 하트비트도 멈춘다. 저장된 커밋 위치는 다시 시작할 때 적용을
        // 앞당기는 데만 쓰고 늦어도 안전하다(openraft는 저장하지 않는 것이 기본이다). 차례는 큐가
        // 지키므로 이 위치보다 앞의 로그 쓰기가 먼저 커밋된다.
        self.writer
            .submit(move |db| {
                if let Err(e) = Self::write_meta(db, KEY_COMMITTED, &committed) {
                    tracing::warn!(error = %e, "cannot save the committed log id");
                }
            })
            .map_err(|e| StorageIOError::write(&e).into())
    }

    async fn read_committed(&mut self) -> Result<Option<LogId<NodeId>>, StorageError<NodeId>> {
        // 기다리지 않고 넣은 `save_committed`보다 뒤에 읽도록 저장소 스레드의 큐를 거친다.
        let committed: Option<Option<LogId<NodeId>>> = self
            .writer
            .run(|db| Self::read_meta(db, KEY_COMMITTED))
            .await
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
        // openraft 0.9는 돌아온 뒤 곧바로 엔트리를 읽을 수 있어야 하고 콜백까지 기다리므로, 커밋을 마친
        // 뒤에 돌아온다. 콜백은 저장소 스레드가 커밋 뒤에 부른다.
        let entries: Vec<_> = entries.into_iter().collect();
        self.writer
            .run(move |db| {
                Self::append_entries(db, entries)?;
                callback.log_io_completed(Ok(()));
                Ok(())
            })
            .await
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.writer
            .run(move |db| Self::truncate_from(db, log_id.index))
            .await
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.writer
            .run(move |db| Self::purge_upto(db, &log_id))
            .await
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }
}
