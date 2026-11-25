use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openraft::storage::RaftStateMachine;
use openraft::{
    Entry, EntryPayload, LogId, OptionalSend, RaftSnapshotBuilder, Snapshot, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership,
};
use orbivisor_types::resource::Kind;
use redb::{Database, ReadableDatabase, ReadableTable, Table};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::exclusive::{SHARED_IP_KINDS, exclusive_values};
use crate::history::{Prev, Recorder, SnapshotRow, Tables};
use crate::store::{
    HISTORY, HISTORY_SEQ, KEY_LAST_APPLIED, KEY_LAST_MEMBERSHIP, KEY_SNAPSHOT_DATA,
    KEY_SNAPSHOT_META, META, NAMES, OBJECTS, SCHEMA_VERSION, StoreError, get_json, put_json,
};
use crate::types::{Command, NodeId, OrbNode, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{FeedBatch, WatchEvent, WatchHub};
use crate::writer::StoreWriter;
use crate::{folders, hosts};

type Membership = StoredMembership<NodeId, OrbNode>;
type Meta = SnapshotMeta<NodeId, OrbNode>;
/// 상태 전체의 리소스(스냅샷 본문)
pub(crate) type Objects = Vec<(Kind, RawResource)>;

/// 스냅샷 본문의 머리. 본문 전체를 읽기 전에 형식 버전부터 확인한다.
#[derive(Deserialize)]
struct SnapshotHeader {
    schema_version: u32,
}

/// 스냅샷 본문. 상태 머신의 리소스 전체와 설정 이력(계획 73)을 담는다.
#[derive(Serialize, Deserialize)]
struct SnapshotData {
    schema_version: u32,
    objects: Vec<(Kind, RawResource)>,
    /// 이력 줄(revision 차례). 기능 `config-history` 전에는 비어 쓰지 않으므로 옛 노드의 스냅샷과 바이트까지 같고, 옛
    /// 노드는 이 칸을 모른 채 버린다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<SnapshotRow>,
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
            let mut history = txn.open_table(HISTORY)?;
            let mut seq = txn.open_table(HISTORY_SEQ)?;
            let mut recorder = Recorder::new();
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
                    EntryPayload::Normal(Command::Attributed {
                        meta: change,
                        command,
                    }) => {
                        let command = *command;
                        let kind = command.kind();
                        let tracked = command.changes_spec() && kind.history_tracked();
                        // 바꾸기는 앞 값과 견준다(같은 값 쓰기는 줄 없음, 비밀은 바뀜/같음만).
                        let mut prev = None;
                        let result = apply_command_keeping(
                            &mut objects,
                            &mut names,
                            command,
                            index,
                            tracked.then_some(&mut prev),
                        )?;
                        if let (true, Ok(event)) = (tracked, &result) {
                            let mut tables = Tables {
                                objects: &objects,
                                names: &names,
                                meta: &meta,
                                history: &mut history,
                                seq: &mut seq,
                            };
                            recorder.record(&mut tables, &change, index, event, prev.as_ref())?;
                        }
                        if kind == Kind::Settings {
                            recorder.settings_changed();
                        }
                        result.map(|event| {
                            events.push(event);
                            index
                        })
                    }
                    EntryPayload::Normal(cmd) => {
                        let settings = cmd.kind() == Kind::Settings;
                        let result = apply_command(&mut objects, &mut names, cmd, index)?;
                        if settings {
                            recorder.settings_changed();
                        }
                        result.map(|event| {
                            events.push(event);
                            index
                        })
                    }
                };
                put_json(&mut meta, KEY_LAST_APPLIED, &entry.log_id)?;
                applied = Some(entry.log_id);
                responses.push(Response(result));
            }
            recorder.finish(&mut meta)?;
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
        let txn = db.begin_read()?;
        let (last_log_id, last_membership, objects) = snapshot_in(&txn)?;
        let history = crate::history::snapshot_rows(&txn)?;
        drop(txn);
        let data = serde_json::to_vec(&SnapshotData {
            schema_version: SCHEMA_VERSION,
            objects,
            history,
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
            crate::history::install_rows(
                &mut txn.open_table(HISTORY)?,
                &mut txn.open_table(HISTORY_SEQ)?,
                &mut table,
                &snapshot.history,
            )?;
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
    snapshot_in(&db.begin_read()?)
}

/// [`snapshot_objects`]를 주어진 읽기 거래에서(이력과 같은 시점을 뜨려고)
fn snapshot_in(
    txn: &redb::ReadTransaction,
) -> Result<(Option<LogId<NodeId>>, Membership, Objects), StoreError> {
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
    // 관찰 노드는 이력 표를 갖지 않는다(Follow는 리소스만 싣는다 — 이력은 투표 멤버·learner에서 읽는다).
    let data = serde_json::to_vec(&SnapshotData {
        schema_version: SCHEMA_VERSION,
        objects,
        history: Vec::new(),
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
pub(crate) fn apply_command(
    objects: &mut Table<(&'static str, u128), &'static [u8]>,
    names: &mut Table<(&'static str, &'static str), u128>,
    cmd: Command,
    index: u64,
) -> Result<Result<WatchEvent, Rejection>, StoreError> {
    apply_command_keeping(objects, names, cmd, index, None)
}

/// [`apply_command`]와 같되 `keep`이 있으면 바꾸기(`UpdateSpec`) 전 spec·labels를 거기 둔다(이력이 앞 값과 견준다).
fn apply_command_keeping(
    objects: &mut Table<(&'static str, u128), &'static [u8]>,
    names: &mut Table<(&'static str, &'static str), u128>,
    cmd: Command,
    index: u64,
    keep: Option<&mut Option<Prev>>,
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
            let mut quota = folders::quota_check(
                objects,
                names,
                kind,
                resource.id,
                &resource.name,
                &resource.spec,
                None,
            )?;
            let mut host = hosts::host_check(objects, kind, resource.id, &resource.spec, None)?;
            if let Err(r) = check_exclusive(
                objects,
                names,
                kind,
                resource.id,
                &resource.spec,
                &resource.status,
                quota.as_mut(),
                host.as_mut(),
            )? {
                return Ok(Err(r));
            }
            if let Err(r) = folders::check_write(objects, kind, resource.id, &resource.spec, None)?
            {
                return Ok(Err(r));
            }
            if let Some(quota) = quota
                && let Err(r) = quota.finish(objects, names)?
            {
                return Ok(Err(r));
            }
            if let Some(host) = host
                && let Err(r) = host.finish(objects)?
            {
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
            let mut quota = folders::quota_check(
                objects,
                names,
                kind,
                id,
                &resource.name,
                &spec,
                Some(&resource.spec),
            )?;
            let mut host = hosts::host_check(objects, kind, id, &spec, Some(&resource))?;
            if let Err(r) = check_exclusive(
                objects,
                names,
                kind,
                id,
                &spec,
                &resource.status,
                quota.as_mut(),
                host.as_mut(),
            )? {
                return Ok(Err(r));
            }
            if let Err(r) = folders::check_write(objects, kind, id, &spec, Some(&resource.spec))? {
                return Ok(Err(r));
            }
            if let Some(quota) = quota
                && let Err(r) = quota.finish(objects, names)?
            {
                return Ok(Err(r));
            }
            if let Some(host) = host
                && let Err(r) = host.finish(objects)?
            {
                return Ok(Err(r));
            }
            let old_labels = std::mem::replace(&mut resource.labels, labels);
            let old_spec = std::mem::replace(&mut resource.spec, spec);
            if let Some(keep) = keep {
                *keep = Some(Prev {
                    spec: old_spec,
                    labels: old_labels,
                });
            }
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
        // 겹쳐 싼 명령은 안의 것을 적용한다(이력은 바깥 `apply_entries`가 남긴다).
        Command::Attributed { command, .. } => {
            return apply_command_keeping(objects, names, *command, index, keep);
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
            if let Err(r) = folders::check_delete(objects, kind, id, &resource.spec)? {
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

/// `spec`의 독점 값이 같은 종류의 다른 리소스나 같은 spec 안에서 겹치면 거부한다. VM 쓰기의 할당량 검사(`quota`)와
/// 노드 자원 장부 검사(`host`, 계획 64)가 있으면 같은 VM 훑기에서 사용량도 센다(설계 §9: VM을 한 번만 푼다).
#[allow(clippy::too_many_arguments)]
fn check_exclusive(
    objects: &Table<(&'static str, u128), &'static [u8]>,
    names: &Table<(&'static str, &'static str), u128>,
    kind: Kind,
    id: Uuid,
    spec: &serde_json::Value,
    status: &serde_json::Value,
    mut quota: Option<&mut folders::QuotaCheck>,
    mut host: Option<&mut hosts::HostCheck>,
) -> Result<Result<(), Rejection>, StoreError> {
    let wanted = exclusive_values(kind, spec, status);
    if kind != Kind::Vm {
        quota = None;
        host = None;
    }
    if wanted.is_empty() && quota.is_none() && host.is_none() {
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
        if let Some(q) = quota.as_deref_mut() {
            q.add(objects, names, kind, other, &other_spec.spec)?;
        }
        if let Some(h) = host.as_deref_mut() {
            h.add(other, &other_spec);
        }
        for (field, value) in exclusive_values(kind, &other_spec.spec, &other_spec.status) {
            if wanted.iter().any(|(f, v)| *f == field && *v == value) {
                return Ok(Err(in_use(field, &value, other)));
            }
        }
    }
    // 주소 `ip`는 VM NIC·라우터 외부 주소·유동 IP·부하 분산기 주소가 한 키 공간을 쓴다(계획 66·67). 다른 종류는 주소가
    // 있을 때만 훑는다.
    if SHARED_IP_KINDS.contains(&kind) && wanted.iter().any(|(f, _)| *f == "ip") {
        for other_kind in SHARED_IP_KINDS.into_iter().filter(|k| *k != kind) {
            let name = other_kind.as_str();
            for item in objects.range((name, 0u128)..=(name, u128::MAX))? {
                let (key, v) = item?;
                let other_spec: RawResource = serde_json::from_slice(v.value())?;
                for (field, value) in
                    exclusive_values(other_kind, &other_spec.spec, &other_spec.status)
                {
                    if field == "ip" && wanted.iter().any(|(f, v)| *f == field && *v == value) {
                        return Ok(Err(in_use(field, &value, Uuid::from_u128(key.value().1))));
                    }
                }
            }
        }
    }
    if let Some(q) = quota {
        q.scanned = true;
    }
    if let Some(h) = host {
        h.scanned = true;
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
    // ---- 설정 이력(계획 73) ----

    use crate::store::{HISTORY, HISTORY_SEQ, KEY_HISTORY_BYTES, KEY_HISTORY_COUNT, history_of};
    use orbivisor_types::history::{Actor, ActorAuth, ChangeMeta, ChangeSource, HistoryOp};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    fn entry(index: u64, cmd: Command) -> Entry<TypeConfig> {
        Entry {
            log_id: LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), index),
            payload: EntryPayload::Normal(cmd),
        }
    }

    fn meta(at_ms: u64) -> ChangeMeta {
        ChangeMeta {
            at_ms,
            actor: Actor {
                user: Some("admin".into()),
                user_id: Some(Uuid::from_u128(77)),
                auth: ActorAuth::Session,
                task: None,
            },
            source: ChangeSource::Web,
            reason: Some("ticket 1".into()),
            ..ChangeMeta::default()
        }
    }

    fn wrap(at_ms: u64, cmd: Command) -> Command {
        Command::Attributed {
            meta: meta(at_ms),
            command: Box::new(cmd),
        }
    }

    fn create(kind: Kind, id: u128, name: &str, spec: Value) -> Command {
        let mut resource: RawResource = orbivisor_types::resource::Resource::new(name, spec);
        resource.id = Uuid::from_u128(id);
        resource.status = json!({});
        Command::Create { kind, resource }
    }

    fn update(kind: Kind, id: u128, spec: Value) -> Command {
        Command::UpdateSpec {
            kind,
            id: Uuid::from_u128(id),
            expected_revision: None,
            labels: BTreeMap::new(),
            spec,
        }
    }

    fn delete(kind: Kind, id: u128) -> Command {
        Command::Delete {
            kind,
            id: Uuid::from_u128(id),
            expected_revision: None,
        }
    }

    /// 명령들을 1부터 차례로 한 묶음씩 적용한다(실제 쓰기처럼 묶음 하나에 명령 하나).
    fn apply_all(db: &Database, cmds: Vec<Command>) -> Vec<Response> {
        let start = StateMachine::read_applied_index(db);
        cmds.into_iter()
            .enumerate()
            .flat_map(|(i, c)| {
                StateMachine::apply_entries(db, vec![entry(start + i as u64 + 1, c)])
                    .unwrap()
                    .0
            })
            .collect()
    }

    fn open() -> Store {
        Store::open_in_memory().unwrap()
    }

    /// 이력 표 둘과 합계를 바이트로(결정성 비교)
    fn dump(db: &Database) -> Vec<u8> {
        let txn = db.begin_read().unwrap();
        let mut out = Vec::new();
        for item in txn.open_table(HISTORY).unwrap().iter().unwrap() {
            let (k, v) = item.unwrap();
            let (kind, id, rev) = k.value();
            out.extend(format!("{kind}/{id}/{rev}=").bytes());
            out.extend(v.value());
        }
        for item in txn.open_table(HISTORY_SEQ).unwrap().iter().unwrap() {
            let (k, v) = item.unwrap();
            out.extend(format!("{}={:?};", k.value(), v.value()).bytes());
        }
        let meta = txn.open_table(META).unwrap();
        for key in [KEY_HISTORY_COUNT, KEY_HISTORY_BYTES] {
            out.extend(
                get_json::<u64>(&meta, key)
                    .unwrap()
                    .unwrap_or(0)
                    .to_le_bytes(),
            );
        }
        out
    }

    impl StateMachine {
        fn read_applied_index(db: &Database) -> u64 {
            let txn = db.begin_read().unwrap();
            let meta = txn.open_table(META).unwrap();
            get_json::<LogId<NodeId>>(&meta, KEY_LAST_APPLIED)
                .unwrap()
                .map_or(0, |l| l.index)
        }
    }

    #[test]
    fn attributed_writes_leave_history_rows() {
        let store = open();
        let db = store.db();
        let id = Uuid::from_u128(1);
        let r = apply_all(
            db,
            vec![
                wrap(
                    1000,
                    create(Kind::Volume, 1, "disk", json!({ "size_bytes": 1 })),
                ),
                // 같은 값 쓰기는 줄이 없다.
                wrap(1001, update(Kind::Volume, 1, json!({ "size_bytes": 1 }))),
                wrap(1002, update(Kind::Volume, 1, json!({ "size_bytes": 2 }))),
                // status 쓰기·출처 없는 쓰기·기록하지 않는 종류는 줄이 없다.
                Command::UpdateStatus {
                    kind: Kind::Volume,
                    id,
                    status: json!({ "phase": "ready" }),
                },
                update(Kind::Volume, 1, json!({ "size_bytes": 3 })),
                wrap(1005, create(Kind::Task, 2, "t", json!({}))),
                // 거부된 쓰기도 줄이 없다.
                wrap(1006, update(Kind::Volume, 9, json!({}))),
                wrap(1007, delete(Kind::Volume, 1)),
            ],
        );
        assert!(r[6].0.is_err());
        let rows = history_of(db, Kind::Volume, id).unwrap();
        let ops: Vec<_> = rows.iter().map(|r| (r.revision, r.op, r.at_ms)).collect();
        assert_eq!(
            ops,
            vec![
                (1, HistoryOp::Create, 1000),
                (3, HistoryOp::Update, 1002),
                (8, HistoryOp::Delete, 1007)
            ]
        );
        assert_eq!(rows[0].name, "disk");
        assert_eq!(rows[1].spec, json!({ "size_bytes": 2 }));
        // 지우기 줄은 지우기 직전 spec(출처 없이 바꾼 3)을 담는다.
        assert_eq!(rows[2].spec, json!({ "size_bytes": 3 }));
        assert_eq!(rows[0].actor.user.as_deref(), Some("admin"));
        assert_eq!(rows[0].source, ChangeSource::Web);
        assert_eq!(rows[0].reason.as_deref(), Some("ticket 1"));
        assert!(
            history_of(db, Kind::Task, Uuid::from_u128(2))
                .unwrap()
                .is_empty()
        );
        let totals = crate::store::history_totals(db).unwrap();
        assert_eq!(totals.count, 3);
        assert!(totals.bytes > 0);
        // 타임라인은 새것부터, 쪽 열쇠는 revision이다.
        let tl = crate::store::history_timeline(db, None, 2).unwrap();
        assert_eq!(
            tl.iter().map(|r| r.revision).collect::<Vec<_>>(),
            vec![8, 3]
        );
        let tl = crate::store::history_timeline(db, Some(3), 10).unwrap();
        assert_eq!(tl.iter().map(|r| r.revision).collect::<Vec<_>>(), vec![1]);
    }

    /// 라벨만 바꿔도 줄이고, 비밀은 표시만 남는다(평문·해시 없음).
    #[test]
    fn labels_count_and_secrets_never_reach_history() {
        let store = open();
        let db = store.db();
        let spec =
            |pw: &str| json!({ "host": "vc", "user": "u", "password": pw, "fingerprint": "f" });
        let mut relabel = update(Kind::VmwareSource, 1, spec("hunter2"));
        if let Command::UpdateSpec { labels, .. } = &mut relabel {
            labels.insert("team".into(), "a".into());
        }
        apply_all(
            db,
            vec![
                wrap(1, create(Kind::VmwareSource, 1, "vc", spec("hunter2"))),
                wrap(2, relabel),
                wrap(3, update(Kind::VmwareSource, 1, spec("s3cret-NEW"))),
                wrap(4, delete(Kind::VmwareSource, 1)),
            ],
        );
        let rows = history_of(db, Kind::VmwareSource, Uuid::from_u128(1)).unwrap();
        let marks: Vec<&Value> = rows.iter().map(|r| &r.spec["password"]).collect();
        assert_eq!(
            marks,
            vec![
                &json!({ "$secret": "set" }),
                &json!({ "$secret": "same" }),
                &json!({ "$secret": "changed" }),
                &json!({ "$secret": "set" })
            ]
        );
        assert_eq!(rows[1].labels.get("team").map(String::as_str), Some("a"));
        let raw = dump(db);
        let text: Vec<u8> = rows
            .iter()
            .flat_map(|r| serde_json::to_vec(r).unwrap())
            .collect();
        for secret in ["hunter2", "s3cret-NEW"] {
            assert!(!String::from_utf8_lossy(&text).contains(secret));
            assert!(!raw.windows(secret.len()).any(|w| w == secret.as_bytes()));
        }
    }

    fn settings(history: Value) -> Command {
        create(
            Kind::Settings,
            100,
            "cluster",
            json!({ "updates": { "channel": "stable" }, "history": history }),
        )
    }

    #[test]
    fn retention_per_resource_keeps_the_newest_rows() {
        let store = open();
        let db = store.db();
        let mut cmds = vec![
            settings(json!({ "max_per_resource": 10 })),
            wrap(1, create(Kind::Network, 1, "n", json!({ "v": 0 }))),
        ];
        for i in 1..=20 {
            cmds.push(wrap(1 + i, update(Kind::Network, 1, json!({ "v": i }))));
        }
        cmds.push(wrap(100, delete(Kind::Network, 1)));
        apply_all(db, cmds);
        let rows = history_of(db, Kind::Network, Uuid::from_u128(1)).unwrap();
        assert_eq!(rows.len(), 10);
        // 지운 리소스의 마지막 줄(살리기에 씀)은 남는다.
        assert_eq!(rows.last().unwrap().op, HistoryOp::Delete);
        assert_eq!(rows.first().unwrap().spec, json!({ "v": 12 }));
        assert_eq!(crate::store::history_totals(db).unwrap().count, 10);
        let seq = crate::store::history_timeline(db, None, 100).unwrap();
        assert_eq!(seq.len(), 10);
    }

    #[test]
    fn retention_by_age_uses_the_command_time() {
        let store = open();
        let db = store.db();
        let day = 86_400_000;
        let mut cmds = vec![settings(json!({ "max_age_days": 7 }))];
        for i in 0..5u128 {
            cmds.push(wrap(
                day,
                create(Kind::Folder, 10 + i, &format!("f{i}"), json!({})),
            ));
        }
        // 8일 뒤의 쓰기가 오래된 줄을 지운다(노드 시계가 아니라 명령의 시각).
        cmds.push(wrap(9 * day, create(Kind::Folder, 20, "late", json!({}))));
        apply_all(db, cmds);
        let tl = crate::store::history_timeline(db, None, 100).unwrap();
        assert_eq!(tl.len(), 1, "{tl:?}");
        assert_eq!(tl[0].name, "late");
        assert_eq!(crate::store::history_totals(db).unwrap().count, 1);
    }

    #[test]
    fn retention_by_total_bytes_drops_the_oldest() {
        let store = open();
        let db = store.db();
        // 줄지 않는 값(lz4가 거의 못 줄임) 64 KiB씩 200줄 ≈ 12.5 MiB > 8 MiB
        let noise = |seed: u64| -> String {
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            (0..65_536)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    char::from(b'!' + (x % 90) as u8)
                })
                .collect()
        };
        let mut cmds = vec![settings(json!({ "max_total_mib": 8 }))];
        for i in 0..200u128 {
            cmds.push(wrap(
                1,
                create(
                    Kind::AlertRule,
                    1000 + i,
                    &format!("r{i}"),
                    json!({ "x": noise(i as u64) }),
                ),
            ));
        }
        apply_all(db, cmds);
        let totals = crate::store::history_totals(db).unwrap();
        assert!(totals.bytes <= 8 * 1024 * 1024, "{totals:?}");
        assert!(totals.count < 200 && totals.count > 100, "{totals:?}");
        let tl = crate::store::history_timeline(db, None, 1000).unwrap();
        assert_eq!(tl.len() as u64, totals.count);
        assert_eq!(tl[0].name, "r199");
    }

    /// 같은 로그를 적용한 두 상태 머신의 이력 표는 바이트까지 같다. 묶음 나누기가 달라도 같다.
    #[test]
    fn history_is_deterministic() {
        let log = || {
            let mut cmds = vec![settings(json!({ "max_per_resource": 10 }))];
            for i in 0..30u64 {
                let id = 1 + u128::from(i % 3);
                cmds.push(if i < 3 {
                    wrap(
                        i,
                        create(Kind::Network, id, &format!("n{id}"), json!({ "v": i })),
                    )
                } else {
                    wrap(
                        i,
                        update(Kind::Network, id, json!({ "v": i, "password": "x" })),
                    )
                });
            }
            cmds.push(wrap(99, delete(Kind::Network, 2)));
            cmds
        };
        let (a, b) = (open(), open());
        apply_all(a.db(), log());
        // b는 한 묶음에 모두
        let entries: Vec<_> = log()
            .into_iter()
            .enumerate()
            .map(|(i, c)| entry(i as u64 + 1, c))
            .collect();
        StateMachine::apply_entries(b.db(), entries).unwrap();
        assert_eq!(dump(a.db()), dump(b.db()));
        assert!(!dump(a.db()).is_empty());
    }

    #[test]
    fn snapshot_carries_history_and_is_unchanged_without_it() {
        let (a, b) = (open(), open());
        apply_all(a.db(), vec![create(Kind::Volume, 1, "plain", json!({}))]);
        let (_, data) = StateMachine::snapshot_data(a.db()).unwrap();
        let v: Value = serde_json::from_slice(&data).unwrap();
        // 기능 전(출처 없는 쓰기만)의 스냅샷에는 칸이 없다(옛 노드와 같은 바이트).
        assert!(v.get("history").is_none(), "{v}");

        apply_all(
            a.db(),
            vec![
                wrap(5, update(Kind::Volume, 1, json!({ "size_bytes": 9 }))),
                wrap(6, create(Kind::Folder, 2, "f", json!({}))),
                wrap(7, delete(Kind::Folder, 2)),
            ],
        );
        let (meta, data) = StateMachine::snapshot_data(a.db()).unwrap();
        let v: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(v["history"].as_array().unwrap().len(), 3);
        // b에 이력이 먼저 있어도 스냅샷으로 바뀐다.
        apply_all(
            b.db(),
            vec![wrap(1, create(Kind::Folder, 9, "x", json!({})))],
        );
        StateMachine::install(b.db(), &meta, data).unwrap();
        assert_eq!(dump(a.db()), dump(b.db()));
        assert_eq!(
            history_of(b.db(), Kind::Folder, Uuid::from_u128(9)).unwrap(),
            vec![]
        );
        assert_eq!(
            history_of(b.db(), Kind::Folder, Uuid::from_u128(2))
                .unwrap()
                .len(),
            2
        );
        // 관찰 노드 설치(Follow)는 이력을 싣지 않는다.
        let (applied, membership, objects) = snapshot_objects(a.db()).unwrap();
        install_objects(b.db(), applied, membership, objects).unwrap();
        assert_eq!(crate::store::history_totals(b.db()).unwrap().count, 0);
    }

    /// 이력을 남기는 적용 비용(설계 §7.3 목표: 1 000번 바꾸기에 5% 밑). 기본으로 돌지 않는다:
    /// `cargo test --release -p orbivisor-cluster --lib -- --ignored history_overhead --nocapture`
    #[test]
    #[ignore]
    fn history_overhead() {
        // 2 KiB쯤의 VM 비슷한 spec
        let vm_spec = |i: u64| {
            json!({
                "vcpus": 4, "memory_mib": 8192 + i, "arch": "x86_64", "machine": "q35",
                "power": "running",
                "disks": (0..4).map(|d| json!({ "volume": format!("web01-disk-{d}"), "bus": "virtio-blk",
                    "cache": "none", "discard": true, "iops": 1000 })).collect::<Vec<_>>(),
                "nics": (0..3).map(|n| json!({ "network": format!("net-{n}"), "model": "virtio",
                    "mac": format!("52:54:00:00:00:0{n}"), "security_groups": ["web-sg", "ssh"] }))
                    .collect::<Vec<_>>(),
                "cloud_init": { "user": "rocky", "ssh_keys": ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI"
                    .repeat(6)], "password": "$6$rounds=4096$abc$def" },
                "labels": { "tier": "web", "owner": "platform-team", "cost-center": "cc-1234" },
                "description": "x".repeat(900),
            })
        };
        assert!(serde_json::to_vec(&vm_spec(0)).unwrap().len() > 1800);
        const N: u64 = 1000;
        let run = |db: &Database, attributed: bool| -> std::time::Duration {
            let start = StateMachine::read_applied_index(db);
            let first = if start == 0 {
                StateMachine::apply_entries(
                    db,
                    vec![entry(1, create(Kind::Vm, 1, "web01", vm_spec(0)))],
                )
                .unwrap();
                1
            } else {
                start
            };
            let t = std::time::Instant::now();
            for i in 1..=N {
                let cmd = update(Kind::Vm, 1, vm_spec(first + i));
                let cmd = if attributed { wrap(i, cmd) } else { cmd };
                let (r, _) = StateMachine::apply_entries(db, vec![entry(first + i, cmd)]).unwrap();
                assert!(r[0].0.is_ok(), "{:?}", r[0]);
            }
            t.elapsed()
        };
        for (label, file) in [("in-memory", false), ("file (fsync)", true)] {
            let mut plain = Vec::new();
            let mut attributed = Vec::new();
            for round in 0..5 {
                for a in [false, true] {
                    let dir = tempfile::TempDir::new().unwrap();
                    let store = if file {
                        Store::open(&dir.path().join("db")).unwrap()
                    } else {
                        open()
                    };
                    let d = run(store.db(), a);
                    let rows = crate::store::history_totals(store.db()).unwrap().count;
                    assert_eq!(
                        rows,
                        if a { 100 } else { 0 },
                        "rows kept (limit 100 per resource)"
                    );
                    if round > 0 {
                        if a { attributed.push(d) } else { plain.push(d) }
                    }
                }
            }
            plain.sort();
            attributed.sort();
            let (p, a) = (plain[plain.len() / 2], attributed[attributed.len() / 2]);
            println!(
                "{label}: {N} updates plain {p:?}, with history {a:?}, overhead {:+.1}%",
                (a.as_secs_f64() / p.as_secs_f64() - 1.0) * 100.0
            );
        }
    }
}
