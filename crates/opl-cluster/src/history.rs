//! 상태 머신의 설정 이력 표(계획 73 §7.2·§7.3). 출처를 단 명령(`Command::Attributed`)을 적용하면서 기록 종류의 spec
//! 바뀜마다 줄 하나를 같은 redb 거래에 쓴다. 보존 한도도 적용 때 명령의 시각(리더가 찍음)과 Raft의 설정으로 정하므로
//! 모든 노드의 표가 바이트까지 같다.

use base64::Engine;
use opl_types::history::{ChangeMeta, HistoryOp, HistoryRecord, HistorySettings, history_mask};
use opl_types::resource::Kind;
use opl_types::settings::SETTINGS_NAME;
use redb::{ReadableTable, Table};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::{
    HISTORY, HISTORY_SEQ, KEY_HISTORY_BYTES, KEY_HISTORY_COUNT, StoreError, get_json, put_json,
};
use crate::watch::WatchEvent;

/// 묶음(명령) 하나가 나이·바이트 한도로 지우는 줄의 최대 수(설계 §7.3 — 적용 비용을 묶는다)
pub(crate) const TRIM_SCAN: usize = 256;

pub(crate) type HistoryTable<'t> = Table<'t, (&'static str, u128, u64), &'static [u8]>;
pub(crate) type SeqTable<'t> = Table<'t, u64, (&'static str, u128, u64, u64)>;

/// 이력 전체의 수와 바이트(압축 뒤). 메타 표에 둔다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryTotals {
    pub count: u64,
    pub bytes: u64,
}

/// 스냅샷에 싣는 이력 줄: (종류, id, revision, 시각, lz4 줄 base64)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SnapshotRow(pub Kind, pub Uuid, pub u64, pub u64, pub String);

/// 한 적용 묶음 동안 쓰는 이력 상태(한도와 합계는 묶음에 한 번 읽는다).
pub(crate) struct Recorder {
    limits: Option<HistorySettings>,
    totals: Option<HistoryTotals>,
    dirty: bool,
}

impl Recorder {
    pub(crate) fn new() -> Self {
        Self {
            limits: None,
            totals: None,
            dirty: false,
        }
    }

    /// 설정 리소스가 바뀌었다(다음 줄부터 새 한도).
    pub(crate) fn settings_changed(&mut self) {
        self.limits = None;
    }

    fn limits(
        &mut self,
        objects: &Table<(&'static str, u128), &'static [u8]>,
        names: &Table<(&'static str, &'static str), u128>,
    ) -> Result<HistorySettings, StoreError> {
        if let Some(l) = self.limits {
            return Ok(l);
        }
        let l = read_limits(objects, names)?;
        self.limits = Some(l);
        Ok(l)
    }

    fn totals(
        &mut self,
        meta: &Table<&'static str, &'static [u8]>,
    ) -> Result<HistoryTotals, StoreError> {
        if let Some(t) = self.totals {
            return Ok(t);
        }
        let t = HistoryTotals {
            count: get_json(meta, KEY_HISTORY_COUNT)?.unwrap_or(0),
            bytes: get_json(meta, KEY_HISTORY_BYTES)?.unwrap_or(0),
        };
        self.totals = Some(t);
        Ok(t)
    }

    /// 적용한 명령의 결과로 줄을 남긴다. `prev`는 바꾸기 전 spec·labels(바꾸기만). 같은 값 쓰기(spec·labels 그대로)는
    /// 줄이 없다.
    pub(crate) fn record(
        &mut self,
        tables: &mut Tables<'_, '_>,
        meta: &ChangeMeta,
        index: u64,
        event: &WatchEvent,
        prev: Option<&Prev>,
    ) -> Result<(), StoreError> {
        let (kind, resource, op) = match event {
            WatchEvent::Put { kind, resource } => match prev {
                None => (*kind, resource, HistoryOp::Create),
                Some(p) if p.spec == resource.spec && p.labels == resource.labels => {
                    return Ok(());
                }
                Some(_) => (*kind, resource, HistoryOp::Update),
            },
            WatchEvent::Deleted { kind, resource, .. } => (*kind, resource, HistoryOp::Delete),
            WatchEvent::Reset { .. } => return Ok(()),
        };
        if !kind.history_tracked() {
            return Ok(());
        }
        let spec = match op {
            HistoryOp::Update => history_mask(kind, prev.map(|p| &p.spec), &resource.spec),
            HistoryOp::Create | HistoryOp::Delete => history_mask(kind, None, &resource.spec),
        };
        let record = HistoryRecord::new(
            meta,
            index,
            kind,
            resource.id,
            resource.name.clone(),
            op,
            resource.labels.clone(),
            spec,
        );
        let bytes = encode(&record)?;
        let len = bytes.len() as u64;
        let key = (kind.as_str(), resource.id.as_u128());
        tables
            .history
            .insert((key.0, key.1, index), bytes.as_slice())?;
        tables.seq.insert(index, (key.0, key.1, meta.at_ms, len))?;
        let mut totals = self.totals(tables.meta)?;
        totals.count += 1;
        totals.bytes += len;

        let limits = self.limits(tables.objects, tables.names)?;
        // 리소스마다 줄 수: 가장 오래된 것부터 지운다(가장 새 줄 — 지운 리소스의 `Delete` — 은 남는다).
        let old: Vec<u64> = tables
            .history
            .range((key.0, key.1, 0)..=(key.0, key.1, u64::MAX))?
            .rev()
            .skip(limits.max_per_resource as usize)
            .map(|r| r.map(|(k, _)| k.value().2))
            .collect::<Result<_, _>>()?;
        for revision in old {
            remove_row(tables, revision, &mut totals)?;
        }
        // 나이·전체 바이트: 시간 차례 앞에서 몇 줄만 본다. "지금"은 명령의 시각이다.
        let cutoff = meta.at_ms.saturating_sub(limits.max_age_ms());
        let max_bytes = limits.max_total_bytes();
        for _ in 0..TRIM_SCAN {
            let Some((revision, at_ms)) =
                tables.seq.first()?.map(|(k, v)| (k.value(), v.value().2))
            else {
                break;
            };
            if revision == index || (at_ms >= cutoff && totals.bytes <= max_bytes) {
                break;
            }
            remove_row(tables, revision, &mut totals)?;
        }
        self.totals = Some(totals);
        self.dirty = true;
        Ok(())
    }

    /// 묶음 끝: 바뀐 합계를 메타 표에 쓴다.
    pub(crate) fn finish(
        self,
        meta: &mut Table<&'static str, &'static [u8]>,
    ) -> Result<(), StoreError> {
        if let (true, Some(t)) = (self.dirty, self.totals) {
            put_json(meta, KEY_HISTORY_COUNT, &t.count)?;
            put_json(meta, KEY_HISTORY_BYTES, &t.bytes)?;
        }
        Ok(())
    }
}

/// 바꾸기 전 값(상태 머신이 바꾸기를 적용하며 넘긴다 — 다시 읽지 않게)
pub(crate) struct Prev {
    pub(crate) spec: serde_json::Value,
    pub(crate) labels: std::collections::BTreeMap<String, String>,
}

/// 이력을 쓰는 데 필요한 표들
pub(crate) struct Tables<'a, 't> {
    pub(crate) objects: &'a Table<'t, (&'static str, u128), &'static [u8]>,
    pub(crate) names: &'a Table<'t, (&'static str, &'static str), u128>,
    pub(crate) meta: &'a Table<'t, &'static str, &'static [u8]>,
    pub(crate) history: &'a mut HistoryTable<'t>,
    pub(crate) seq: &'a mut SeqTable<'t>,
}

fn remove_row(
    tables: &mut Tables<'_, '_>,
    revision: u64,
    totals: &mut HistoryTotals,
) -> Result<(), StoreError> {
    let Some(entry) = tables.seq.remove(revision)? else {
        return Ok(());
    };
    let (kind, id, _, len) = entry.value();
    let key = (kind.to_string(), id);
    drop(entry);
    tables.history.remove((key.0.as_str(), key.1, revision))?;
    totals.count = totals.count.saturating_sub(1);
    totals.bytes = totals.bytes.saturating_sub(len);
    Ok(())
}

/// Raft의 설정 리소스에서 보존 한도를 읽는다(없거나 범위 밖이면 기본값). 상태 머신 안이라 모든 노드가 같은 값을 본다.
fn read_limits(
    objects: &Table<(&'static str, u128), &'static [u8]>,
    names: &Table<(&'static str, &'static str), u128>,
) -> Result<HistorySettings, StoreError> {
    #[derive(Deserialize)]
    struct Spec {
        #[serde(default)]
        history: Option<HistorySettings>,
    }
    #[derive(Deserialize)]
    struct Settings {
        spec: Spec,
    }
    let kind = Kind::Settings.as_str();
    let Some(id) = names.get((kind, SETTINGS_NAME))?.map(|v| v.value()) else {
        return Ok(HistorySettings::default());
    };
    let Some(raw) = objects.get((kind, id))? else {
        return Ok(HistorySettings::default());
    };
    // 모르는 꼴(시험이 쓴 아무 JSON)이면 기본값이다.
    let limits = serde_json::from_slice::<Settings>(raw.value())
        .ok()
        .and_then(|s| s.spec.history)
        .filter(HistorySettings::valid);
    Ok(limits.unwrap_or_default())
}

/// 줄 하나 → lz4로 줄인 JSON
pub(crate) fn encode(record: &HistoryRecord) -> Result<Vec<u8>, StoreError> {
    Ok(lz4_flex::compress_prepend_size(&serde_json::to_vec(
        record,
    )?))
}

/// lz4 줄 → 줄 하나
pub(crate) fn decode(bytes: &[u8]) -> Result<HistoryRecord, StoreError> {
    let raw = lz4_flex::decompress_size_prepended(bytes).map_err(|e| {
        StoreError::Json(serde::de::Error::custom(format!(
            "corrupt history row: {e}"
        )))
    })?;
    Ok(serde_json::from_slice(&raw)?)
}

/// 스냅샷에 실을 이력 전체(revision 차례)
pub(crate) fn snapshot_rows(txn: &redb::ReadTransaction) -> Result<Vec<SnapshotRow>, StoreError> {
    let history = txn.open_table(HISTORY)?;
    let seq = txn.open_table(HISTORY_SEQ)?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut out = Vec::new();
    for item in seq.iter()? {
        let (rev, v) = item?;
        let (kind, id, at_ms, _) = v.value();
        let Some(bytes) = history.get((kind, id, rev.value()))? else {
            continue;
        };
        let kind: Kind = serde_json::from_value(kind.into())?;
        out.push(SnapshotRow(
            kind,
            Uuid::from_u128(id),
            rev.value(),
            at_ms,
            b64.encode(bytes.value()),
        ));
    }
    Ok(out)
}

/// 스냅샷의 이력으로 표를 바꾼다(지우고 채움 — 합계도 다시 센다).
pub(crate) fn install_rows(
    history: &mut HistoryTable<'_>,
    seq: &mut SeqTable<'_>,
    meta: &mut Table<&'static str, &'static [u8]>,
    rows: &[SnapshotRow],
) -> Result<(), StoreError> {
    history.retain(|_, _| false)?;
    seq.retain(|_, _| false)?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut totals = HistoryTotals::default();
    for SnapshotRow(kind, id, revision, at_ms, data) in rows {
        let bytes = b64.decode(data).map_err(|e| {
            StoreError::Json(serde::de::Error::custom(format!(
                "corrupt history row in snapshot: {e}"
            )))
        })?;
        history.insert((kind.as_str(), id.as_u128(), *revision), bytes.as_slice())?;
        seq.insert(
            *revision,
            (kind.as_str(), id.as_u128(), *at_ms, bytes.len() as u64),
        )?;
        totals.count += 1;
        totals.bytes += bytes.len() as u64;
    }
    put_json(meta, KEY_HISTORY_COUNT, &totals.count)?;
    put_json(meta, KEY_HISTORY_BYTES, &totals.bytes)?;
    Ok(())
}
