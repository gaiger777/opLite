use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openraft::storage::RaftStateMachine;
use openraft::{
    Entry, EntryPayload, LogId, OptionalSend, RaftSnapshotBuilder, Snapshot, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership,
};
use opl_types::resource::Kind;
use redb::{Database, ReadableDatabase, ReadableTable, Table};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::exclusive::exclusive_values;
use crate::store::{
    KEY_LAST_APPLIED, KEY_LAST_MEMBERSHIP, KEY_SNAPSHOT_DATA, KEY_SNAPSHOT_META, META, NAMES,
    OBJECTS, SCHEMA_VERSION, StoreError, get_json, put_json,
};
use crate::types::{Command, NodeId, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{FeedBatch, WatchEvent, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, openraft::BasicNode>;
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;
/// 상태 전체의 리소스(스냅샷 본문)
pub(crate) type Objects = Vec<(Kind, RawResource)>;

/// 스냅샷 본문의 머리. 본문 전체를 읽기 전에 형식 버전부터 확인한다.
#[derive(Deserialize)]
struct SnapshotHeader {
    schema_version: u32,
}

/// 스냅샷 본문. 상태 머신의 리소스 전체를 담는다.
#[derive(Serialize, Deserialize)]
struct SnapshotData {
    schema_version: u32,
    objects: Vec<(Kind, RawResource)>,
}

/// 리소스 상태 머신. 엔트리 묶음 하나를 redb 트랜잭션 하나로 적용하므로
/// 리소스와 `last_applied`는 항상 함께 기록된다. 쓰기는 저장소 스레드에서 돈다. 적용 묶음을 구독자에게 내는
/// 것도 같은 저장소 스레드의 쓰기 안에서 해, 저장소와 적용 리비전의 차례가 다른 쓰기(전환)와 어긋나지 않는다.
#[derive(Clone)]
pub struct StateMachine {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
    writer: StoreWriter,
    /// 이 상태 머신을 쓰는 Raft가 계획된 전환으로 물러났다. openraft는 `shutdown` 뒤에도 상태 머신 작업이 남은
    /// 적용을 마저 할 수 있어, 전환이 저장소 스레드에서 이것을 세운 뒤의 적용·설치는 쓰지 않고 `Retired`다.
    retired: Arc<AtomicBool>,
}

impl StateMachine {
    pub(crate) fn new(db: Arc<Database>, watch: Arc<WatchHub>, writer: StoreWriter) -> Self {
        Self {
            db,
            watch,
            writer,
            retired: Arc::default(),
        }
    }

    /// 전환이 저장소 스레드의 쓰기 안에서 세우는 표시
    pub(crate) fn retired_flag(&self) -> Arc<AtomicBool> {
        self.retired.clone()
    }

    fn read_applied(&self) -> Result<(Option<LogId<NodeId>>, Membership), StoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        Ok((
            get_json(&meta, KEY_LAST_APPLIED)?,
            get_json(&meta, KEY_LAST_MEMBERSHIP)?.unwrap_or_default(),
        ))
    }

    fn apply_entries(
        db: &Database,
        entries: Vec<Entry<TypeConfig>>,
    ) -> Result<(Vec<Response>, Option<FeedBatch>), StoreError> {
        let mut responses = Vec::with_capacity(entries.len());
        let mut events = Vec::new();
        let mut applied = None;
        let mut membership = None;
        let txn = db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            for entry in entries {
                let index = entry.log_id.index;
                let result = match entry.payload {
                    EntryPayload::Blank => Ok(index),
                    EntryPayload::Membership(m) => {
                        let m = Membership::new(Some(entry.log_id), m);
                        put_json(&mut meta, KEY_LAST_MEMBERSHIP, &m)?;
                        membership = Some(m);
                        Ok(index)
                    }
                    EntryPayload::Normal(cmd) => {
                        apply_command(&mut objects, &mut names, cmd, index)?.map(|event| {
                            events.push(event);
                            index
                        })
                    }
                };
                put_json(&mut meta, KEY_LAST_APPLIED, &entry.log_id)?;
                applied = Some(entry.log_id);
                responses.push(Response(result));
            }
        }
        txn.commit()?;
        // 이벤트가 없는 묶음도 적용 위치를 알린다.
        let batch = applied.map(|applied| FeedBatch {
            events,
            applied,
            membership,
        });
        Ok((responses, batch))
    }

    /// 한 읽기 트랜잭션에서 상태 전체를 떠서 스냅샷 본문을 만든다. 저장은 부른 쪽이 한다.
    fn snapshot_data(db: &Database) -> Result<(Meta, Vec<u8>), StoreError> {
        let (last_log_id, last_membership, objects) = snapshot_objects(db)?;
        let data = serde_json::to_vec(&SnapshotData {
            schema_version: SCHEMA_VERSION,
            objects,
        })?;
        let meta = Meta {
            snapshot_id: format!(
                "{}-{}",
                last_log_id.map_or(0, |l| l.index),
                Uuid::new_v4().simple()
            ),
            last_log_id,
            last_membership,
        };
        Ok((meta, data))
    }

    /// 스냅샷을 저장한다. 이미 저장된 스냅샷이 같거나 더 새로우면 그대로 둔다.
    /// openraft는 스냅샷 빌드를 별도 태스크에서 돌리므로, 느린 빌드가 끝나기 전에
    /// 더 새 스냅샷이 설치될 수 있다.
    fn save_snapshot(db: &Database, meta: &Meta, data: &[u8]) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(META)?;
            if let Some(existing) = get_json::<Meta>(&table, KEY_SNAPSHOT_META)?
                && existing.last_log_id >= meta.last_log_id
            {
                return Ok(());
            }
            put_json(&mut table, KEY_SNAPSHOT_META, meta)?;
            table.insert(KEY_SNAPSHOT_DATA, data)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn current_snapshot(&self) -> Result<Option<Snapshot<TypeConfig>>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(META)?;
        let Some(meta) = get_json::<Meta>(&table, KEY_SNAPSHOT_META)? else {
            return Ok(None);
        };
        let data = table
            .get(KEY_SNAPSHOT_DATA)?
            .map(|v| v.value().to_vec())
            .unwrap_or_default();
        Ok(Some(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        }))
    }

    /// 리소스 전체를 스냅샷 내용으로 바꾼다. 리소스, 적용 위치, 스냅샷을 한 트랜잭션에 기록한다.
    fn install(db: &Database, meta: &Meta, data: Vec<u8>) -> Result<(), StoreError> {
        let header: SnapshotHeader = serde_json::from_slice(&data)?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: header.schema_version,
            });
        }
        let snapshot: SnapshotData = serde_json::from_slice(&data)?;
        let txn = db.begin_write()?;
        {
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            objects.retain(|_, _| false)?;
            names.retain(|_, _| false)?;
            for (kind, resource) in &snapshot.objects {
                objects.insert(
                    (kind.as_str(), resource.id.as_u128()),
                    serde_json::to_vec(resource)?.as_slice(),
                )?;
                names.insert(
                    (kind.as_str(), resource.name.as_str()),
                    resource.id.as_u128(),
                )?;
            }
            let mut table = txn.open_table(META)?;
            put_json(&mut table, KEY_LAST_APPLIED, &meta.last_log_id)?;
            put_json(&mut table, KEY_LAST_MEMBERSHIP, &meta.last_membership)?;
            put_json(&mut table, KEY_SNAPSHOT_META, meta)?;
            table.insert(KEY_SNAPSHOT_DATA, data.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }
}

/// 한 읽기 트랜잭션에서 뜬 상태 전체: 적용 위치, 멤버십, 리소스.
pub(crate) fn snapshot_objects(
    db: &Database,
) -> Result<(Option<LogId<NodeId>>, Membership, Objects), StoreError> {
    let txn = db.begin_read()?;
    let meta_table = txn.open_table(META)?;
    let last_log_id: Option<LogId<NodeId>> = get_json(&meta_table, KEY_LAST_APPLIED)?;
    let last_membership: Membership =
        get_json(&meta_table, KEY_LAST_MEMBERSHIP)?.unwrap_or_default();
    let mut objects = Vec::new();
    for item in txn.open_table(OBJECTS)?.iter()? {
        let (key, value) = item?;
        let kind: Kind = serde_json::from_value(key.value().0.into())?;
        objects.push((kind, serde_json::from_slice(value.value())?));
    }
    Ok((last_log_id, last_membership, objects))
}

/// 지금 상태 전체를 스냅샷으로 저장한다(이미 저장된 스냅샷이 같거나 더 새로우면 그대로). 관찰 노드가 다시 Raft를
/// 띄우기 전에 부른다: 지운 로그 대신 openraft가 뒤처진 노드에 보낼 스냅샷이 적용 위치까지 있어야 한다.
pub(crate) fn save_current_snapshot(db: &Database) -> Result<(), StoreError> {
    let (meta, data) = StateMachine::snapshot_data(db)?;
    StateMachine::save_snapshot(db, &meta, &data)
}

/// 관찰 노드가 Follow로 받은 스냅샷(리소스 전체)을 `StateMachine::install`로 설치한다.
pub(crate) fn install_objects(
    db: &Database,
    applied: Option<LogId<NodeId>>,
    membership: Membership,
    objects: Objects,
) -> Result<(), StoreError> {
    let data = serde_json::to_vec(&SnapshotData {
        schema_version: SCHEMA_VERSION,
        objects,
    })?;
    let meta = Meta {
        snapshot_id: format!(
            "{}-{}",
            applied.map_or(0, |l| l.index),
            Uuid::new_v4().simple()
        ),
        last_log_id: applied,
        last_membership: membership,
    };
    StateMachine::install(db, &meta, data)
}

/// 관찰 노드가 Follow로 받은 적용 묶음을 쓴다. 리소스·이름, `last_applied`, 멤버십을 한 쓰기 트랜잭션에 기록한다.
/// 같은 묶음을 로컬 구독자에게 내는 것(`WatchHub::publish`)은 부른 쪽이 커밋 뒤에 한다.
pub(crate) fn apply_feed(db: &Database, batch: &FeedBatch) -> Result<(), StoreError> {
    let txn = db.begin_write()?;
    {
        let mut objects = txn.open_table(OBJECTS)?;
        let mut names = txn.open_table(NAMES)?;
        for event in &batch.events {
            match event {
                WatchEvent::Put { kind, resource } => {
                    let key = (kind.as_str(), resource.id.as_u128());
                    // 이름이 바뀐 리소스면 옛 이름 색인을 지운다.
                    let old: Option<RawResource> = match objects.get(key)? {
                        Some(v) => Some(serde_json::from_slice(v.value())?),
                        None => None,
                    };
                    if let Some(old) = old
                        && old.name != resource.name
                    {
                        names.remove((kind.as_str(), old.name.as_str()))?;
                    }
                    objects.insert(key, serde_json::to_vec(resource)?.as_slice())?;
                    names.insert(
                        (kind.as_str(), resource.name.as_str()),
                        resource.id.as_u128(),
                    )?;
                }
                WatchEvent::Deleted { kind, resource, .. } => {
                    objects.remove((kind.as_str(), resource.id.as_u128()))?;
                    names.remove((kind.as_str(), resource.name.as_str()))?;
                }
                // 피드 묶음에는 오지 않는다(스냅샷 설치는 `install_objects`).
                WatchEvent::Reset { .. } => {}
            }
        }
        let mut meta = txn.open_table(META)?;
        put_json(&mut meta, KEY_LAST_APPLIED, &batch.applied)?;
        if let Some(m) = &batch.membership {
            put_json(&mut meta, KEY_LAST_MEMBERSHIP, m)?;
        }
    }
    txn.commit()?;
    Ok(())
}

/// 명령 하나를 적용한다. 거부는 결정적이므로 모든 노드가 같은 결과를 낸다.
/// 바깥 `Err`는 저장소 오류이고, 안쪽 `Err`는 명령 거부다.
fn apply_command(
    objects: &mut Table<(&'static str, u128), &'static [u8]>,
    names: &mut Table<(&'static str, &'static str), u128>,
    cmd: Command,
    index: u64,
) -> Result<Result<WatchEvent, Rejection>, StoreError> {
    let load = |objects: &Table<(&'static str, u128), &'static [u8]>,
                kind: Kind,
                id: Uuid|
     -> Result<Option<RawResource>, StoreError> {
        match objects.get((kind.as_str(), id.as_u128()))? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    };
    let check = |expected: Option<u64>, actual: u64| match expected {
        Some(expected) if expected != actual => {
            Err(Rejection::RevisionMismatch { expected, actual })
        }
        _ => Ok(()),
    };

    let (kind, resource) = match cmd {
        Command::Create { kind, mut resource } => {
            if load(objects, kind, resource.id)?.is_some() {
                return Ok(Err(Rejection::AlreadyExists {
                    kind,
                    id: resource.id,
                }));
            }
            if names
                .get((kind.as_str(), resource.name.as_str()))?
                .is_some()
            {
                return Ok(Err(Rejection::NameTaken {
                    kind,
                    name: resource.name,
                }));
            }
            if let Err(r) = check_exclusive(objects, kind, resource.id, &resource.spec)? {
                return Ok(Err(r));
            }
            names.insert(
                (kind.as_str(), resource.name.as_str()),
                resource.id.as_u128(),
            )?;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateSpec {
            kind,
            id,
            expected_revision,
            labels,
            spec,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(expected_revision, resource.revision) {
                return Ok(Err(r));
            }
            if let Err(r) = check_exclusive(objects, kind, id, &spec)? {
                return Ok(Err(r));
            }
            resource.labels = labels;
            resource.spec = spec;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateStatus { kind, id, status } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateStatusIf {
            kind,
            id,
            expected_revision,
            status,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(Some(expected_revision), resource.revision) {
                return Ok(Err(r));
            }
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::Delete {
            kind,
            id,
            expected_revision,
        } => {
            let Some(resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(expected_revision, resource.revision) {
                return Ok(Err(r));
            }
            objects.remove((kind.as_str(), id.as_u128()))?;
            names.remove((kind.as_str(), resource.name.as_str()))?;
            return Ok(Ok(WatchEvent::Deleted {
                revision: index,
                kind,
                resource,
            }));
        }
    };
    objects.insert(
        (kind.as_str(), resource.id.as_u128()),
        serde_json::to_vec(&resource)?.as_slice(),
    )?;
    Ok(Ok(WatchEvent::Put { kind, resource }))
}

/// `spec`의 독점 값이 같은 종류의 다른 리소스나 같은 spec 안에서 겹치면 거부한다.
fn check_exclusive(
    objects: &Table<(&'static str, u128), &'static [u8]>,
    kind: Kind,
    id: Uuid,
    spec: &serde_json::Value,
) -> Result<Result<(), Rejection>, StoreError> {
    let wanted = exclusive_values(kind, spec);
    if wanted.is_empty() {
        return Ok(Ok(()));
    }
    let in_use = |field: &str, value: &str, holder: Uuid| Rejection::InUse {
        kind,
        field: field.to_string(),
        value: value.to_string(),
        holder,
    };
    for (i, (field, value)) in wanted.iter().enumerate() {
        if wanted[..i].contains(&(field, value.clone())) {
            return Ok(Err(in_use(field, value, id)));
        }
    }
    for item in objects.range((kind.as_str(), 0u128)..=(kind.as_str(), u128::MAX))? {
        let (key, v) = item?;
        let other = Uuid::from_u128(key.value().1);
        if other == id {
            continue;
        }
        let other_spec: RawResource = serde_json::from_slice(v.value())?;
        for (field, value) in exclusive_values(kind, &other_spec.spec) {
            if wanted.iter().any(|(f, v)| *f == field && *v == value) {
                return Ok(Err(in_use(field, &value, other)));
            }
        }
    }
    Ok(Ok(()))
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    /// 큰 읽기는 `spawn_blocking`에서, 저장은 저장소 스레드에서 한다.
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let db = self.db.clone();
        let (meta, data) = tokio::task::spawn_blocking(move || Self::snapshot_data(&db))
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        let (meta, data) = self
            .writer
            .run(move |db| Self::save_snapshot(db, &meta, &data).map(|()| (meta, data)))
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<NodeId>>, Membership), StorageError<NodeId>> {
        self.read_applied()
            .map_err(|e| StorageIOError::read_state_machine(&e).into())
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Response>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let (watch, retired) = (self.watch.clone(), self.retired.clone());
        let responses = self
            .writer
            .run(move |db| {
                if retired.load(Ordering::SeqCst) {
                    return Err(StoreError::Retired);
                }
                let (responses, batch) = Self::apply_entries(db, entries)?;
                if let Some(batch) = batch {
                    watch.publish(batch);
                }
                Ok(responses)
            })
            .await
            .map_err(|e| StorageIOError::write_state_machine(&e))?;
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &Meta,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let (m, data) = (meta.clone(), snapshot.into_inner());
        let (watch, retired) = (self.watch.clone(), self.retired.clone());
        self.writer
            .run(move |db| {
                if retired.load(Ordering::SeqCst) {
                    return Err(StoreError::Retired);
                }
                Self::install(db, &m, data)?;
                watch.reset(m.last_log_id.map_or(0, |l| l.index));
                Ok(())
            })
            .await
            .map_err(|e| StorageIOError::read_snapshot(Some(meta.signature()), &e))?;
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        self.current_snapshot()
            .map_err(|e| StorageIOError::read_snapshot(None, &e).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use openraft::CommittedLeaderId;

    fn meta_at(index: u64) -> Meta {
        Meta {
            last_log_id: Some(LogId::new(
                CommittedLeaderId::new(1, NodeId::from(1)),
                index,
            )),
            last_membership: Membership::default(),
            snapshot_id: format!("{index}"),
        }
    }

    /// openraft는 스냅샷을 다른 태스크에서 만들므로, 느린 빌드가 그 사이 설치된
    /// 더 새 스냅샷을 덮어쓰면 안 된다.
    #[test]
    fn older_snapshot_does_not_replace_newer_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let sm = Store::open(&dir.path().join("db")).unwrap().state_machine();
        StateMachine::save_snapshot(&sm.db, &meta_at(10), b"new").unwrap();
        StateMachine::save_snapshot(&sm.db, &meta_at(5), b"old").unwrap();

        let snap = sm.current_snapshot().unwrap().unwrap();
        assert_eq!(snap.meta, meta_at(10));
        assert_eq!(snap.snapshot.into_inner(), b"new");
    }

    #[test]
    fn snapshot_with_newer_schema_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let sm = Store::open(&dir.path().join("db")).unwrap().state_machine();
        // 미래 형식은 본문 구조도 다를 수 있다. 헤더만 보고 거부해야 한다.
        let data = br#"{"schema_version": 99, "objects": {"format": "v99"}}"#.to_vec();
        assert!(matches!(
            StateMachine::install(&sm.db, &meta_at(3), data),
            Err(StoreError::UnsupportedSchema { found: 99 })
        ));
    }
}
