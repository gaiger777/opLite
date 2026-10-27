use std::io::Cursor;
use std::sync::Arc;

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
use crate::watch::{WatchEvent, WatchHub};

type Membership = StoredMembership<NodeId, openraft::BasicNode>;
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;

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
/// 리소스와 `last_applied`는 항상 함께 기록된다.
#[derive(Clone)]
pub struct StateMachine {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
}

impl StateMachine {
    pub(crate) fn new(db: Arc<Database>, watch: Arc<WatchHub>) -> Self {
        Self { db, watch }
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
        &self,
        entries: Vec<Entry<TypeConfig>>,
    ) -> Result<(Vec<Response>, Vec<WatchEvent>), StoreError> {
        let mut responses = Vec::with_capacity(entries.len());
        let mut events = Vec::new();
        let txn = self.db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            for entry in entries {
                let index = entry.log_id.index;
                let result = match entry.payload {
                    EntryPayload::Blank => Ok(index),
                    EntryPayload::Membership(m) => {
                        put_json(
                            &mut meta,
                            KEY_LAST_MEMBERSHIP,
                            &Membership::new(Some(entry.log_id), m),
                        )?;
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
                responses.push(Response(result));
            }
        }
        txn.commit()?;
        Ok((responses, events))
    }

    /// 한 읽기 트랜잭션에서 상태 전체를 떠서 스냅샷을 만들고 저장한다.
    fn build(&self) -> Result<Snapshot<TypeConfig>, StoreError> {
        let txn = self.db.begin_read()?;
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
        drop(txn);

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
        self.save_snapshot(&meta, &data)?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }

    /// 스냅샷을 저장한다. 이미 저장된 스냅샷이 같거나 더 새로우면 그대로 둔다.
    /// openraft는 스냅샷 빌드를 별도 태스크에서 돌리므로, 느린 빌드가 끝나기 전에
    /// 더 새 스냅샷이 설치될 수 있다.
    fn save_snapshot(&self, meta: &Meta, data: &[u8]) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
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
    fn install(&self, meta: &Meta, data: Vec<u8>) -> Result<(), StoreError> {
        let header: SnapshotHeader = serde_json::from_slice(&data)?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: header.schema_version,
            });
        }
        let snapshot: SnapshotData = serde_json::from_slice(&data)?;
        let txn = self.db.begin_write()?;
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
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        self.build()
            .map_err(|e| StorageIOError::write_snapshot(None, &e).into())
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
        let (responses, events) = self
            .apply_entries(entries.into_iter().collect())
            .map_err(|e| StorageIOError::write_state_machine(&e))?;
        self.watch.publish(events);
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
        self.install(meta, snapshot.into_inner())
            .map_err(|e| StorageIOError::read_snapshot(Some(meta.signature()), &e))?;
        self.watch.reset(meta.last_log_id.map_or(0, |l| l.index));
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
        sm.save_snapshot(&meta_at(10), b"new").unwrap();
        sm.save_snapshot(&meta_at(5), b"old").unwrap();

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
            sm.install(&meta_at(3), data),
            Err(StoreError::UnsupportedSchema { found: 99 })
        ));
    }
}
