//! 지표 기록(계획 60 §5·§6·§7): 메모리 저장소의 5초 점을 분마다 RRD 파일([`Rrd`])로 옮기고, 다시 시작하면 지난
//! 1시간을 저장소에 되살리고, 범위 조회(`ReadMetricsHistory`)에 답하고, VM 기록을 VM의 새 주인 노드에 밀어 넘기고
//! (`PushMetricsHistory`), 지운 VM·풀의 파일을 지운다.
//!
//! 파일은 `{data_dir}/metrics`(`node.rrd`, `vm/{id}.rrd`, `pool/{id}.rrd`)에 있고 핸들은 연 채 둔다. 파일 IO는 막히는
//! 호출이므로 비동기 쪽([`HistoryWriter`], 노드 간 서버)은 `spawn_blocking`에서 부른다.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use orbivisor_cluster::Cluster;
use orbivisor_cluster::types::NodeId;
use orbivisor_cluster::watch::{Watch, WatchEvent};
use orbivisor_peer::{HistoryError, MAX_HISTORY_BYTES, Peers};
use orbivisor_types::clock::Clock;
use orbivisor_types::metrics::{
    HistoryQuery, HistoryReply, HistoryScope, MetricField, NodeSample, Tier, TopVm, VmSample,
    pool_values,
};
use orbivisor_types::metrics_math::{
    Agg, bucket_window, choose_tier, downsample, group_size, merge_top, percentiles, stats, to_cf,
};
use orbivisor_types::node::{METRICS_HISTORY, NodeSpec};
use orbivisor_types::resource::Kind;
use orbivisor_types::storage::StoragePoolSpec;
use orbivisor_types::task::TaskSpec;
use orbivisor_types::vm::VmSpec;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::rrd::{self, Append, EXT, Opened, Rrd, SeriesKind};
use super::store::{Marks, MetricsStore};

/// `{data_dir}` 아래 기록 디렉터리
pub const DIR: &str = "metrics";
/// 기록 주기(설정 `[metrics] flush_secs`)의 기본값·범위(초)
pub const DEFAULT_FLUSH_SECS: u64 = 60;
pub const MIN_FLUSH_SECS: u64 = 60;
pub const MAX_FLUSH_SECS: u64 = 600;
/// VM 기록을 주인 노드에 밀어 넘기고 지운 VM·풀의 파일을 정리하는 주기(설계 §6)
pub const SCAN_INTERVAL: Duration = Duration::from_secs(600);
/// 이 노드의 VM이 바뀐 것(떠남, 지움)을 본 뒤 정리를 앞당겨 하기까지 기다리는 시간. 라이브 마이그레이션이 끝난 뒤
/// 원본 QEMU가 멈추고 마지막 점이 기록될 때까지 둔다.
pub const SCAN_AFTER_CHANGE: Duration = Duration::from_secs(30);
/// 시작한 뒤 첫 정리까지(클러스터 상태가 따라잡을 때까지). 첫 정리는 지울 후보만 모은다.
pub const FIRST_SCAN: Duration = Duration::from_secs(60);
/// 한 번에 열어 두는 파일 수. 넘으면 가장 오래 쓰지 않은 VM·풀 파일을 닫는다.
pub const MAX_OPEN: usize = 4096;
/// VM의 마지막 점이 이만큼(ms) 안이면 아직 이 노드에서 재는 중이라 밀어 넘기지도 지우지도 않는다.
pub const BUSY_MS: i64 = 30_000;
/// 밀어 넘기기 한 번을 기다리는 시간
const PUSH_TIMEOUT: Duration = Duration::from_secs(30);
/// 정리 한 차례의 밀어 넘기기를 모두 기다리는 시간(넘으면 남은 것은 다음 차례에 한다)
pub const PUSH_DEADLINE: Duration = Duration::from_secs(120);
/// 동시에 밀어 넘기는 수
const PUSH_CONCURRENCY: usize = 8;
/// 받는 쪽이 `Invalid`(받을 수 없는 파일)나 `Disabled`(기록을 끔)로 거절한 VM 기록을 다시 보내기까지 처음 기다리는
/// 시간. 거절할 때마다 두 배로 늘려 하루까지 기다린다(옛 RRD 형식의 노드나 기록을 끈 노드에 336 KiB를 10분마다 보내지
/// 않게). 기록을 끈 노드가 나중에 켜면 늦어도 하루 안에 넘어간다.
const REFUSED_BACKOFF: Duration = SCAN_INTERVAL;
const MAX_REFUSED_BACKOFF: Duration = Duration::from_secs(86_400);
/// 옆으로 옮긴 파일(`.corrupt`·`.vN`)과 남은 임시 파일(`.tmp`)을 지우는 나이
pub const ASIDE_KEEP: Duration = Duration::from_secs(30 * 86_400);
/// 정리의 리더 확인 읽기를 기다리는 시간
const SWEEP_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 기록은 주기 경계보다 이만큼 뒤에 한다(경계 바로 앞에 잰 점이 저장소에 들어오게).
const FLUSH_DELAY_MS: i64 = 1_000;
/// 같은 종류의 파일 오류(열기·쓰기·닫기·sync·commit)를 다시 경고하기까지(그 사이는 debug, 디스크가 차면 VM 파일마다
/// 분마다 경고해 같은 디스크의 로그를 채우지 않게)
const FAILURE_WARN_EVERY_MS: i64 = 600_000;

/// 기록 시리즈 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Node,
    Vm(Uuid),
    Pool(Uuid),
}

impl Key {
    fn kind(self) -> SeriesKind {
        match self {
            Self::Node => SeriesKind::Node,
            Self::Vm(_) => SeriesKind::Vm,
            Self::Pool(_) => SeriesKind::Pool,
        }
    }

    fn path(self, dir: &Path) -> PathBuf {
        match self {
            Self::Node => rrd::node_path(dir),
            Self::Vm(id) => rrd::vm_path(dir, id),
            Self::Pool(id) => rrd::pool_path(dir, id),
        }
    }
}

type Handle = Arc<Mutex<Rrd>>;

struct Entry {
    rrd: Handle,
    /// 마지막으로 쓴 차례(작을수록 오래 쓰지 않음)
    used: u64,
}

#[derive(Default)]
struct Open {
    files: HashMap<Key, Entry>,
    tick: u64,
}

/// 풀마다 이 노드가 마지막으로 잰 용량(`total_bytes`, `available_bytes`). `PoolMounter`가 새로 고칠 때마다 이 노드에서
/// `ready`인 풀만으로 바꿔 넣고(Raft에 쓰는 1 GiB 거르기 전 값), 기록 쓰기가 읽는다.
#[derive(Debug, Default)]
pub struct PoolLevels(Mutex<HashMap<Uuid, (u64, u64)>>);

impl PoolLevels {
    pub fn set(&self, levels: HashMap<Uuid, (u64, u64)>) {
        *self.0.lock().unwrap() = levels;
    }

    pub fn get(&self) -> HashMap<Uuid, (u64, u64)> {
        self.0.lock().unwrap().clone()
    }
}

/// 한 번 기록하는 동안의 파일 오류: 종류마다 (수, 첫 오류)
#[derive(Debug, Default)]
struct Failures(BTreeMap<&'static str, (usize, String)>);

impl Failures {
    fn note(&mut self, what: &'static str, key: Key, e: &dyn std::fmt::Display) {
        let entry = self
            .0
            .entry(what)
            .or_insert_with(|| (0, format!("{key:?}: {e}")));
        entry.0 += 1;
    }

    fn count(&self) -> usize {
        self.0.values().map(|(n, _)| n).sum()
    }
}

/// 이 노드의 기록 파일들. 기록을 끄면(`dir`이 없음) 파일 없이 메모리 저장소의 1시간만으로 조회에 답한다.
pub struct History {
    dir: Option<PathBuf>,
    store: Arc<MetricsStore>,
    clock: Arc<dyn Clock>,
    /// `Digest`의 상위 VM에서 주인이 이 노드인 VM만 두려고 본다(없으면 거르지 않음).
    owner: Option<(Uuid, Arc<Cluster>)>,
    open: Mutex<Open>,
    /// 파일 열기(만들기)를 한 번에 하나만 한다. `open`은 그동안 잡지 않는다(만들기가 느려도 열린 파일 읽기를 막지
    /// 않게).
    opening: Mutex<()>,
    max_open: usize,
    /// 기록 쓰기가 저장소에서 마지막으로 가져간 자리
    marks: Mutex<Marks>,
    /// 마지막 기록 시각(풀 값을 그 뒤 5초 칸마다 쓴다)
    last_flush: Mutex<Option<i64>>,
    /// 시계가 조금 뒤로 가 쓰지 못한다고 경고한 시리즈(한 번만 경고한다)
    behind: Mutex<HashSet<Key>>,
    /// 파일 오류 종류마다 마지막으로 경고한 때
    warned: Mutex<HashMap<&'static str, i64>>,
}

/// 기록 파일의 `Raw5s` 칸을 점으로: (칸 시작 시각, 칸 값들). 모든 칸을 모르는 칸은 뺀다.
fn raw_points(read: &rrd::TierRead) -> Vec<(i64, Vec<Option<f32>>)> {
    let step = Tier::Raw5s.step_ms();
    let count = read.columns.first().map_or(0, Vec::len);
    (0..count)
        .filter_map(|k| {
            let values: Vec<Option<f32>> = read
                .columns
                .iter()
                .map(|col| (!col[k].is_unknown()).then_some(col[k].avg))
                .collect();
            values
                .iter()
                .any(Option::is_some)
                .then(|| (read.start_ms() + k as i64 * step, values))
        })
        .collect()
}

/// 쉬는 파일은 마지막 원본 칸이 든 6시간 경계까지만 닫는다(멈춘 VM 파일을 분마다 NaN으로 다시 쓰지 않게). 원본이 없으면
/// 닫을 것이 없다.
fn close_until(rrd: &Rrd, now: i64) -> Option<i64> {
    let raw = rrd.last(Tier::Raw5s)?;
    let h = Tier::Hour6.step_ms();
    let end = (raw + 1) * Tier::Raw5s.step_ms();
    Some(now.min((end + h - 1).div_euclid(h) * h))
}

fn io_error(e: io::Error) -> HistoryError {
    match e.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidInput => {
            HistoryError::Invalid(e.to_string())
        }
        _ => HistoryError::Io(e.to_string()),
    }
}

/// 층 칸들을 답으로: 열마다 `points` 안으로 줄이고, `full`이면 범위 요약·백분위도 셈한다(`Digest`는 AVG 열만).
fn build_reply(
    tier: Tier,
    first: i64,
    fields: &[MetricField],
    columns: &[Vec<Agg>],
    points: u16,
    full: bool,
) -> HistoryReply {
    let step = tier.step_ms();
    let start = first * step;
    let count = columns.first().map_or(0, Vec::len);
    let group = group_size(count, points);
    let mut reply = HistoryReply {
        tier,
        step_ms: step * group as i64,
        start,
        columns: Default::default(),
        stats: Default::default(),
        pct: Default::default(),
        covered_from: None,
        top: vec![],
    };
    for (field, col) in fields.iter().zip(columns) {
        let cf = to_cf(col);
        let name = field.as_str().to_string();
        if full {
            reply.stats.insert(name.clone(), stats(&cf));
            // 5초 층은 원본이라 정확하고, 거친 층은 AVG로 셈한 근사다.
            reply
                .pct
                .insert(name.clone(), percentiles(&cf.avg, tier != Tier::Raw5s));
        }
        let mut cf = downsample(&cf, group);
        if !full {
            cf.max.clear();
            cf.min.clear();
        }
        reply.columns.insert(name, cf);
    }
    // 앞 칸들을 모르면 아는 가장 이른 칸(재시작·마이그레이션 직후)
    let known = (0..count).find(|&k| columns.iter().any(|c| !c[k].is_unknown()));
    if let Some(k) = known.filter(|&k| k > 0) {
        reply.covered_from = Some(start + k as i64 * step);
    }
    reply
}

impl History {
    /// 기록을 연다. `dir`이 있으면 디렉터리를 만들고(0700) 노드·VM 파일의 지난 1시간(`Raw5s`)을 `store`에 되살린다
    /// (저장소에 점이 들어오기 전, 측정기를 띄우기 전에 부른다). 디렉터리를 만들지 못하면 경고하고 기록을 끈다. 막히는
    /// 호출이다.
    pub fn open(dir: Option<PathBuf>, store: Arc<MetricsStore>, clock: Arc<dyn Clock>) -> Self {
        let dir = dir.filter(|d| {
            match std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(d)
            {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(dir = %d.display(), error = %e, "cannot create the metrics history directory; history is off");
                    false
                }
            }
        });
        let history = Self {
            dir,
            store,
            clock,
            owner: None,
            open: Mutex::default(),
            opening: Mutex::default(),
            max_open: MAX_OPEN,
            marks: Mutex::default(),
            last_flush: Mutex::default(),
            behind: Mutex::default(),
            warned: Mutex::default(),
        };
        history.restore();
        history
    }

    /// `Digest`의 상위 VM을 이 노드(`node`)가 주인인 VM으로 거른다.
    pub fn with_owner(mut self, node: Uuid, cluster: Arc<Cluster>) -> Self {
        self.owner = Some((node, cluster));
        self
    }

    /// 파일을 쓰는지(`[metrics] history`)
    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }

    fn restore(&self) {
        if self.dir.is_none() {
            return;
        }
        let now = self.clock.now_ms();
        let from = now - Tier::Raw5s.span_ms();
        let mut restored = 0usize;
        let mut keys = vec![Key::Node];
        keys.extend(self.files(SeriesKind::Vm).into_iter().map(Key::Vm));
        for key in keys {
            let read = match self.handle(key, false) {
                Ok(Some(h)) => h.lock().unwrap().read(Tier::Raw5s, from, now),
                Ok(None) => continue,
                Err(e) => Err(e),
            };
            let points = match read {
                Ok(read) => raw_points(&read),
                Err(e) => {
                    tracing::warn!(series = ?key, error = %e, "cannot restore metrics history");
                    continue;
                }
            };
            restored += points.len();
            for (t, values) in points {
                match key {
                    Key::Node => self.store.push_node(NodeSample::from_values(t, &values)),
                    Key::Vm(id) => self.store.push_vm(id, VmSample::from_values(t, &values)),
                    Key::Pool(_) => {}
                }
            }
        }
        // 되살린 점은 이미 파일에 있다.
        let _ = self.store.take_pending(&mut self.marks.lock().unwrap());
        if restored > 0 {
            tracing::info!(
                points = restored,
                "restored the last hour of metrics from history"
            );
        }
    }

    /// 열린 핸들(쓴 차례를 새로 함)
    fn opened(&self, key: Key) -> Option<Handle> {
        let mut open = self.open.lock().unwrap();
        open.tick += 1;
        let tick = open.tick;
        let entry = open.files.get_mut(&key)?;
        entry.used = tick;
        Some(entry.rrd.clone())
    }

    /// 열린 핸들을 주거나 파일을 연다. `create`가 아니면 없는 파일은 `None`이다. 파일을 여는(만드는) 동안은 열린
    /// 목록을 잡지 않는다(리뷰 2 #6).
    fn handle(&self, key: Key, create: bool) -> io::Result<Option<Handle>> {
        let Some(dir) = &self.dir else {
            return Ok(None);
        };
        if let Some(h) = self.opened(key) {
            return Ok(Some(h));
        }
        let _opening = self.opening.lock().unwrap();
        // 기다리는 사이 다른 쪽이 열었을 수 있다.
        if let Some(h) = self.opened(key) {
            return Ok(Some(h));
        }
        let path = key.path(dir);
        if !create && !path.exists() {
            return Ok(None);
        }
        let (rrd, opened) = Rrd::open(&path, key.kind())?;
        // 연 파일은 그 종류의 지금 칸 순서다(값은 `values()` 순서로 쓴다).
        debug_assert_eq!(rrd.kind(), key.kind());
        debug_assert_eq!(rrd.fields(), key.kind().fields());
        match &opened {
            Opened::Existing | Opened::Created => {}
            Opened::Migrated { from } => {
                tracing::info!(file = %path.display(), ?from, "moved a metrics history file to the current layout");
            }
            Opened::Corrupt { moved_to } => {
                tracing::warn!(file = %path.display(), moved_to = %moved_to.display(), "a metrics history file was corrupt; starting it over");
            }
            Opened::Newer { version, moved_to } => {
                tracing::warn!(file = %path.display(), version, moved_to = %moved_to.display(), "a metrics history file is from a newer version; starting it over");
            }
        }
        let mut open = self.open.lock().unwrap();
        open.tick += 1;
        let tick = open.tick;
        if open.files.len() >= self.max_open
            && let Some(oldest) = (open.files.iter())
                .filter(|(k, _)| **k != Key::Node)
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| *k)
        {
            open.files.remove(&oldest);
        }
        let rrd = Arc::new(Mutex::new(rrd));
        open.files.insert(
            key,
            Entry {
                rrd: rrd.clone(),
                used: tick,
            },
        );
        Ok(Some(rrd))
    }

    /// 기록 디렉터리의 그 종류 파일들(`{id}.rrd`만, 옆으로 옮긴 파일과 임시 파일은 빼고)
    fn files(&self, kind: SeriesKind) -> Vec<Uuid> {
        let Some(dir) = &self.dir else {
            return vec![];
        };
        let sub = match kind {
            SeriesKind::Vm => "vm",
            SeriesKind::Pool => "pool",
            SeriesKind::Node => return vec![],
        };
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            return vec![];
        };
        let mut ids: Vec<Uuid> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let path = e.path();
                (path.extension()? == EXT).then_some(())?;
                path.file_stem()?.to_str()?.parse().ok()
            })
            .collect();
        ids.sort();
        ids
    }

    /// 한 번 기록한다: 저장소에서 앞서 가져간 뒤의 점을 노드·VM 파일에, `pools`(풀 id → 전체·남은 바이트)를 지난 기록
    /// 뒤 5초 칸마다 풀 파일에 쓰고, 끝난 거친 층 칸을 닫은 뒤 모든 파일을 `fdatasync` → 머리 commit 한다(설계 §5
    /// 순서). 파일 하나가 실패해도 나머지는 쓴다. 파일 오류는 종류마다 한 줄로 모아 10분에 한 번까지 경고하고, 그
    /// 수를 돌려준다. 막히는 호출이다.
    pub fn flush(&self, pools: &HashMap<Uuid, (u64, u64)>) -> usize {
        if self.dir.is_none() {
            return 0;
        }
        let mut failures = Failures::default();
        let now = self.clock.now_ms();
        let pending = self.store.take_pending(&mut self.marks.lock().unwrap());
        let node: Vec<_> = (pending.node.iter())
            .map(|s| (s.t, s.values().to_vec()))
            .collect();
        self.append(Key::Node, &node, &mut failures);
        for (id, samples) in &pending.vms {
            let points: Vec<_> = samples.iter().map(|s| (s.t, s.values().to_vec())).collect();
            self.append(Key::Vm(*id), &points, &mut failures);
        }
        // 풀은 5초마다 재지 않으므로 지난 기록 뒤의 칸마다 지금 값을 쓴다(처음은 지금 칸 하나).
        let step = Tier::Raw5s.step_ms();
        let now_b = now.div_euclid(step);
        let since_b = {
            let mut last = self.last_flush.lock().unwrap();
            let prev = last.replace(now);
            prev.map_or(now_b, |p| p.div_euclid(step) + 1)
                .clamp(now_b - i64::from(Tier::Raw5s.rows()) + 1, now_b)
        };
        for (id, (total, available)) in pools {
            let values = pool_values(*total, *available).to_vec();
            // 이미 쓴 칸(같은 5초 안에 두 번 기록함, 시계가 뒤로 감)은 건너뛴다.
            let last = match self.handle(Key::Pool(*id), false) {
                Ok(Some(h)) => h.lock().unwrap().last(Tier::Raw5s),
                _ => None,
            };
            let from_b = last.map_or(since_b, |l| since_b.max(l + 1));
            let points: Vec<_> = (from_b..=now_b)
                .map(|b| (b * step, values.clone()))
                .collect();
            self.append(Key::Pool(*id), &points, &mut failures);
        }
        let handles: Vec<(Key, Handle)> = {
            let open = self.open.lock().unwrap();
            (open.files.iter())
                .map(|(k, e)| (*k, e.rrd.clone()))
                .collect()
        };
        let mut dirty = Vec::new();
        for (key, handle) in handles {
            let mut rrd = handle.lock().unwrap();
            if let Some(until) = close_until(&rrd, now)
                && let Err(e) = rrd.close(until)
            {
                failures.note("close", key, &e);
            }
            if rrd.is_dirty() {
                drop(rrd);
                dirty.push((key, handle));
            }
        }
        let mut synced = Vec::new();
        for (key, handle) in dirty {
            let r = handle.lock().unwrap().sync_data();
            match r {
                Ok(()) => synced.push((key, handle)),
                Err(e) => failures.note("sync", key, &e),
            }
        }
        for (key, handle) in synced {
            if let Err(e) = handle.lock().unwrap().commit() {
                failures.note("commit", key, &e);
            }
        }
        self.report(&failures, now);
        failures.count()
    }

    /// 파일 오류를 종류마다 한 줄로 경고한다. 같은 종류는 [`FAILURE_WARN_EVERY_MS`]에 한 번까지이고 그 사이는 debug다.
    /// 경고한 종류 수를 돌려준다.
    fn report(&self, failures: &Failures, now: i64) -> usize {
        let mut warned = self.warned.lock().unwrap();
        let mut n = 0;
        for (what, (count, first)) in &failures.0 {
            let due = warned
                .get(what)
                .is_none_or(|at| now - at >= FAILURE_WARN_EVERY_MS || now < *at);
            if due {
                warned.insert(what, now);
                n += 1;
                tracing::warn!(op = what, files = count, first = %first, "metrics history file operations failed (repeats are logged at debug for 10 minutes)");
            } else {
                tracing::debug!(op = what, files = count, first = %first, "metrics history file operations failed");
            }
        }
        n
    }

    /// 점들을 시각 순으로 그 시리즈 파일에 쓴다(데이터만). 시계가 조금 뒤로 가 쓰지 못하면 한 번만 경고한다. 밀어
    /// 넘긴 뒤 지운 핸들을 잡았으면 새 파일에 쓴다(지운 파일에 쓰면 잃는다).
    fn append(&self, key: Key, points: &[(i64, Vec<Option<f32>>)], failures: &mut Failures) {
        if points.is_empty() {
            return;
        }
        for _ in 0..2 {
            let handle = match self.handle(key, true) {
                Ok(Some(h)) => h,
                Ok(None) => return,
                Err(e) => {
                    failures.note("open", key, &e);
                    return;
                }
            };
            let mut rrd = handle.lock().unwrap();
            if rrd.is_removed() {
                continue;
            }
            self.write_points(key, &mut rrd, points, failures);
            return;
        }
        failures.note("open", key, &"the file was removed while writing");
    }

    fn write_points(
        &self,
        key: Key,
        rrd: &mut Rrd,
        points: &[(i64, Vec<Option<f32>>)],
        failures: &mut Failures,
    ) {
        for (t, values) in points {
            match rrd.append(*t, values) {
                Ok(Append::Written) => {
                    self.behind.lock().unwrap().remove(&key);
                }
                Ok(Append::Behind) => {
                    if self.behind.lock().unwrap().insert(key) {
                        tracing::warn!(file = %rrd.path().display(), t, "the clock went back; not writing metrics history until it catches up");
                    }
                }
                Ok(Append::Rewound) => {
                    self.behind.lock().unwrap().remove(&key);
                    tracing::warn!(file = %rrd.path().display(), t, "the clock went back more than an hour; dropped later metrics history");
                }
                Err(e) => {
                    failures.note("write", key, &e);
                    return;
                }
            }
        }
    }

    /// 범위 조회(설계 §7). 층을 고르고(`choose_tier`) 파일을 읽은 뒤, 5초 층이면 아직 파일에 쓰지 않은 저장소 점으로
    /// 빈 칸을 메운다. 막히는 호출이다.
    pub fn read(&self, q: &HistoryQuery) -> Result<HistoryReply, HistoryError> {
        q.validate()
            .map_err(|e| HistoryError::Invalid(e.to_string()))?;
        let now = self.clock.now_ms();
        match q.scope {
            HistoryScope::Node => self.series(Key::Node, q, now, true),
            HistoryScope::Vm { id } => self.series(Key::Vm(id), q, now, true),
            HistoryScope::Pool { id } => self.series(Key::Pool(id), q, now, true),
            HistoryScope::Digest { top } => {
                let mut reply = self.series(Key::Node, q, now, false)?;
                reply.top = self.top_vms(q, now, usize::from(top))?;
                Ok(reply)
            }
        }
    }

    /// 시리즈 하나의 층 칸들(칸 목록은 그 종류의 지금 순서)
    fn columns(
        &self,
        key: Key,
        tier: Tier,
        from: i64,
        until: i64,
    ) -> Result<(i64, Vec<Vec<Agg>>), HistoryError> {
        let fields = key.kind().fields();
        let (first, count) = bucket_window(tier, from, until);
        let mut columns = vec![vec![Agg::UNKNOWN; count]; fields.len()];
        if let Some(handle) = self.handle(key, false).map_err(io_error)? {
            let read = handle
                .lock()
                .unwrap()
                .read(tier, from, until)
                .map_err(io_error)?;
            for (f, field) in fields.iter().enumerate() {
                if let Some(col) = read.column(*field) {
                    columns[f].copy_from_slice(col);
                }
            }
        }
        if tier == Tier::Raw5s {
            let step = tier.step_ms();
            let mut fill = |t: i64, values: &[Option<f32>]| {
                let Ok(k) = usize::try_from(t.div_euclid(step) - first) else {
                    return;
                };
                for (col, v) in columns.iter_mut().zip(values) {
                    if let (Some(cell), Some(v)) = (col.get_mut(k), v)
                        && cell.is_unknown()
                    {
                        *cell = Agg::single(*v);
                    }
                }
            };
            match key {
                Key::Node => {
                    for s in self.store.node_window(from, until) {
                        fill(s.t, &s.values());
                    }
                }
                Key::Vm(id) => {
                    for s in self.store.vm_window(id, from, until) {
                        fill(s.t, &s.values());
                    }
                }
                Key::Pool(_) => {}
            }
        }
        Ok((first, columns))
    }

    fn series(
        &self,
        key: Key,
        q: &HistoryQuery,
        now: i64,
        full: bool,
    ) -> Result<HistoryReply, HistoryError> {
        let tier = choose_tier(q.from, q.until, now);
        let (first, columns) = self.columns(key, tier, q.from, q.until)?;
        Ok(build_reply(
            tier,
            first,
            key.kind().fields(),
            &columns,
            q.points,
            full,
        ))
    }

    /// 범위 평균 CPU 상위 VM `top`개(이 노드가 주인인 VM, 파일이나 저장소에 있는 것)
    fn top_vms(&self, q: &HistoryQuery, now: i64, top: usize) -> Result<Vec<TopVm>, HistoryError> {
        let mut ids: HashSet<Uuid> = self.files(SeriesKind::Vm).into_iter().collect();
        ids.extend(self.store.vm_ids());
        if let Some((node, cluster)) = &self.owner {
            let owned: HashSet<Uuid> = cluster
                .list::<VmSpec>()
                .map(|(_, vms)| {
                    vms.into_iter()
                        .filter(|vm| vm.status.node.or(vm.spec.node) == Some(*node))
                        .map(|vm| vm.id)
                        .collect()
                })
                .unwrap_or_default();
            ids.retain(|id| owned.contains(id));
        }
        let tier = choose_tier(q.from, q.until, now);
        let fields = SeriesKind::Vm.fields();
        let at = |f: MetricField| fields.iter().position(|x| *x == f);
        let (cpu, rss) = (at(MetricField::Cpu), at(MetricField::MemRss));
        let mut list = Vec::new();
        for id in ids {
            let (_, columns) = self.columns(Key::Vm(id), tier, q.from, q.until)?;
            let avg = |i: Option<usize>| i.and_then(|i| stats(&to_cf(&columns[i])).avg);
            list.push(TopVm {
                vm: id,
                cpu: avg(cpu),
                mem_rss: avg(rss),
            });
        }
        Ok(merge_top([list], top))
    }

    /// 다른 노드가 밀어 넘긴 VM 기록 파일을 이 노드의 그 VM 파일(없으면 만듦)과 칸 단위로 합친다. 막히는 호출이다.
    pub fn receive(&self, vm: Uuid, bytes: &[u8]) -> Result<(), HistoryError> {
        if self.dir.is_none() {
            return Err(HistoryError::Disabled);
        }
        if bytes.len() > MAX_HISTORY_BYTES {
            return Err(HistoryError::Invalid(format!(
                "a metrics history file is at most {MAX_HISTORY_BYTES} bytes"
            )));
        }
        // 밀어 넘긴 뒤 지운 핸들을 잡았으면 새 파일로 한 번 더 한다(지운 파일에 합치면 잃는다).
        for _ in 0..2 {
            let handle = self
                .handle(Key::Vm(vm), true)
                .map_err(io_error)?
                .ok_or(HistoryError::Disabled)?;
            let mut rrd = handle.lock().unwrap();
            if rrd.is_removed() {
                continue;
            }
            rrd.merge(bytes).map_err(io_error)?;
            tracing::info!(vm = %vm, "merged metrics history pushed by another node");
            return Ok(());
        }
        Err(HistoryError::Io(
            "the metrics history file was removed while merging".into(),
        ))
    }

    /// VM 파일을 디스크에 닿게 한 뒤 통째로 읽는다(밀어 넘기기): (바이트, 머리 차례). 파일이 없으면 `None`이다.
    /// 막히는 호출이다.
    fn file_bytes(&self, vm: Uuid) -> io::Result<Option<(Vec<u8>, u64)>> {
        let Some(handle) = self.handle(Key::Vm(vm), false)? else {
            return Ok(None);
        };
        let mut rrd = handle.lock().unwrap();
        rrd.flush()?;
        Ok(Some((rrd.to_bytes()?, rrd.seq())))
    }

    /// VM에 아직 기록 파일에 쓰지 않은 저장소 점이 있는지(있으면 밀어 넘겨도 다음 기록이 파일을 다시 만든다)
    fn vm_pending(&self, vm: Uuid) -> bool {
        self.store.vm_pending(vm, &self.marks.lock().unwrap())
    }

    /// 밀어 넘긴 VM 파일을 읽은 뒤 바뀌지 않았으면(쓰지 않은 것이 없고 머리 차례 `seq`가 같으면) 닫고 지운다.
    /// 그사이 다른 노드가 합쳐 넣었거나 새 점을 썼으면 두고 false다. 막히는 호출이다.
    fn remove_pushed(&self, vm: Uuid, seq: u64) -> io::Result<bool> {
        let key = Key::Vm(vm);
        let Some(handle) = self.handle(key, false)? else {
            return Ok(true);
        };
        // 핸들을 잡은 채 지워 그사이 합치기가 끼어들지 못하게 한다(잠금 순서: 핸들 → 열린 목록).
        let mut rrd = handle.lock().unwrap();
        if rrd.is_dirty() || rrd.seq() != seq {
            return Ok(false);
        }
        {
            let mut open = self.open.lock().unwrap();
            if open
                .files
                .get(&key)
                .is_some_and(|e| Arc::ptr_eq(&e.rrd, &handle))
            {
                open.files.remove(&key);
            }
        }
        rrd.unlink()?;
        Ok(true)
    }

    /// 기록 디렉터리에서 `keep`보다 오래된, 옆으로 옮긴 파일과 남은 임시 파일을 지운다([`rrd::is_aside`]: 이 형식의
    /// 파일 이름 옆의 것만). 지운 수를 돌려준다. 막히는 호출이다.
    fn sweep_aside(&self, keep: Duration) -> usize {
        let Some(dir) = &self.dir else {
            return 0;
        };
        // (디렉터리, 노드 파일 자리인지): 노드 자리는 `node.rrd`, 나머지는 `{uuid}.rrd` 옆의 것만 본다.
        let places = [
            (dir.clone(), true),
            (dir.join("vm"), false),
            (dir.join("pool"), false),
        ];
        let now = std::time::SystemTime::now();
        let mut removed = 0;
        for (place, node) in places {
            let stem_ok = |s: &str| {
                if node {
                    s == "node"
                } else {
                    s.parse::<Uuid>().is_ok()
                }
            };
            let Ok(entries) = std::fs::read_dir(&place) else {
                continue;
            };
            for e in entries.filter_map(Result::ok) {
                let Some(name) = e.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let old = e
                    .metadata()
                    .ok()
                    .filter(|m| m.is_file())
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age > keep);
                if !old || !rrd::is_aside(&name, stem_ok) {
                    continue;
                }
                match std::fs::remove_file(e.path()) {
                    Ok(()) => {
                        removed += 1;
                        tracing::info!(file = %e.path().display(), "removed an old set-aside metrics history file");
                    }
                    Err(err) => {
                        tracing::warn!(file = %e.path().display(), error = %err, "cannot remove an old set-aside metrics history file")
                    }
                }
            }
        }
        removed
    }

    /// 시리즈 파일을 닫고 지운다. 막히는 호출이다.
    fn remove(&self, key: Key) -> io::Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        // 지우는 동안 다른 쪽이 같은 파일을 열어 지운 파일의 핸들을 목록에 두지 않게 한다.
        let _opening = self.opening.lock().unwrap();
        self.open.lock().unwrap().files.remove(&key);
        match std::fs::remove_file(key.path(dir)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// VM 기록 파일을 다른 노드에 밀어 넘기는 쪽(시험은 가짜를 넣는다)
#[async_trait]
pub trait HistoryPusher: Send + Sync {
    /// 노드 `node`에 VM `vm`의 기록 파일을 보낸다. 받은 쪽이 합쳐 디스크에 썼으면 `Ok`다.
    async fn push(&self, node: Uuid, vm: Uuid, bytes: Vec<u8>) -> Result<(), HistoryError>;
}

/// 노드 간 RPC(`PushMetricsHistory`)로 밀어 넘긴다.
pub struct PeerPusher {
    pub cluster: Arc<Cluster>,
    pub peers: Arc<Peers>,
}

#[async_trait]
impl HistoryPusher for PeerPusher {
    async fn push(&self, node: Uuid, vm: Uuid, bytes: Vec<u8>) -> Result<(), HistoryError> {
        let addr = (self
            .cluster
            .members()
            .addr(NodeId(node))
            .map(str::to_string))
        .ok_or_else(|| HistoryError::Unreachable(format!("node {node} is not a cluster member")))?;
        self.peers
            .push_metrics_history(NodeId(node), &addr, vm, bytes.into(), PUSH_TIMEOUT)
            .await
    }
}

/// 기록 쓰기 설정
#[derive(Debug, Clone, Copy)]
pub struct WriterConfig {
    /// 기록 주기(벽시계 경계에 맞춘다)
    pub flush: Duration,
    /// 밀어 넘기기·정리 주기
    pub scan: Duration,
    /// 시작한 뒤 첫 정리까지
    pub first_scan: Duration,
    /// VM이 바뀐 것을 본 뒤 정리를 앞당기기까지
    pub after_change: Duration,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            flush: Duration::from_secs(DEFAULT_FLUSH_SECS),
            scan: SCAN_INTERVAL,
            first_scan: FIRST_SCAN,
            after_change: SCAN_AFTER_CHANGE,
        }
    }
}

/// 분마다 기록하고, 주기마다 VM 기록을 주인 노드에 밀어 넘기고 지운 VM·풀의 파일을 정리하는 작업(위트니스에는 없다)
pub struct HistoryWriter {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl HistoryWriter {
    pub fn spawn(
        history: Arc<History>,
        cluster: Arc<Cluster>,
        pools: Arc<PoolLevels>,
        pusher: Arc<dyn HistoryPusher>,
        config: WriterConfig,
    ) -> Self {
        let scanner = Scanner::new(history.clone(), cluster, pusher);
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_writer(history, scanner, pools, config, rx)),
        }
    }

    /// 마지막으로 한 번 기록하고 멈춘다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 다음 기록까지 기다릴 시간: 벽시계 `now`의 다음 주기 경계 + 1초. 시계가 뛰어도 주기 하나보다 길지 않다.
fn until_next_flush(now: i64, period: Duration) -> Duration {
    let period = i64::try_from(period.as_millis()).unwrap_or(i64::MAX).max(1);
    let next = (now - FLUSH_DELAY_MS).div_euclid(period) * period + period + FLUSH_DELAY_MS;
    Duration::from_millis(u64::try_from(next - now).unwrap_or(0).min(period as u64))
}

async fn flush(history: &Arc<History>, pools: &PoolLevels) {
    let (history, pools) = (history.clone(), pools.get());
    if let Err(e) = tokio::task::spawn_blocking(move || history.flush(&pools)).await {
        tracing::error!(error = %e, "the metrics history flush panicked");
    }
}

async fn run_writer(
    history: Arc<History>,
    scanner: Scanner,
    pools: Arc<PoolLevels>,
    config: WriterConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let (cluster, pusher) = (scanner.cluster.clone(), scanner.pusher.clone());
    let mut next_scan = tokio::time::Instant::now() + config.first_scan;
    let mut watch: Option<Watch> = None;
    // 정리는 따로 돈다(밀어 넘기기가 늦어도 기록 주기를 놓치지 않게). 도는 동안 정리기는 작업이 가진다.
    let mut idle = Some(scanner);
    let mut scanning: Option<JoinHandle<Scanner>> = None;
    // 정리가 도는 동안 VM이 바뀌어 앞당길 다음 정리
    let mut soon: Option<tokio::time::Instant> = None;
    loop {
        let wait = until_next_flush(history.clock.now_ms(), config.flush);
        if watch.is_none() {
            watch = cluster
                .list::<VmSpec>()
                .ok()
                .and_then(|(revision, _)| cluster.watch(revision).ok());
        }
        tokio::select! {
            _ = &mut shutdown => {
                if let Some(task) = scanning.take() {
                    task.abort();
                }
                flush(&history, &pools).await;
                return;
            }
            () = tokio::time::sleep(wait) => flush(&history, &pools).await,
            () = tokio::time::sleep_until(next_scan), if idle.is_some() => {
                let mut scanner = idle.take().unwrap();
                scanning = Some(tokio::spawn(async move {
                    scanner.scan().await;
                    scanner
                }));
            }
            done = async { scanning.as_mut().unwrap().await }, if scanning.is_some() => {
                scanning = None;
                idle = Some(match done {
                    Ok(scanner) => scanner,
                    Err(e) => {
                        tracing::error!(error = %e, "the metrics history scan panicked");
                        Scanner::new(history.clone(), cluster.clone(), pusher.clone())
                    }
                });
                let regular = tokio::time::Instant::now() + config.scan;
                next_scan = soon.take().map_or(regular, |t| t.min(regular));
            }
            changed = vm_changed(&mut watch, &history) => if changed {
                let at = tokio::time::Instant::now() + config.after_change;
                next_scan = next_scan.min(at);
                if scanning.is_some() {
                    soon = Some(soon.map_or(at, |t| t.min(at)));
                }
            },
        }
    }
}

/// 이 노드에 기록 파일이 있는 VM이 바뀌거나 지워질 때까지 기다린다(true). 구독이 끊기면 버리고 false로 돌아가 다시
/// 구독하게 한다.
async fn vm_changed(watch: &mut Option<Watch>, history: &History) -> bool {
    let Some(w) = watch else {
        return std::future::pending().await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Vm,
                    resource,
                }
                | WatchEvent::Deleted {
                    kind: Kind::Vm,
                    resource,
                    ..
                },
            ) => {
                if let Some(dir) = &history.dir
                    && rrd::vm_path(dir, resource.id).exists()
                {
                    return true;
                }
            }
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return false;
            }
            Ok(_) => {}
        }
    }
}

/// 밀어 넘길 (주인 노드, VM)을 주인마다 돌아가며 늘어놓는다(리뷰 2 #8: 답하지 않는 노드의 VM이 앞에 몰려 한 차례
/// 기한을 다 쓰고 다른 노드의 VM이 늘 남지 않게). 주인 순서는 처음 나온 순서, 한 주인 안은 받은 순서다.
fn by_owner(pushes: Vec<(Uuid, Uuid)>) -> Vec<(Uuid, Uuid)> {
    let mut owners: Vec<(Uuid, std::collections::VecDeque<Uuid>)> = Vec::new();
    for (owner, vm) in pushes {
        match owners.iter_mut().find(|(o, _)| *o == owner) {
            Some((_, vms)) => vms.push_back(vm),
            None => owners.push((owner, [vm].into())),
        }
    }
    let mut out = Vec::new();
    while !owners.is_empty() {
        owners.retain_mut(|(owner, vms)| {
            if let Some(vm) = vms.pop_front() {
                out.push((*owner, vm));
            }
            !vms.is_empty()
        });
    }
    out
}

/// 앞 정리 차례에 지울 후보였던 파일(두 차례 연속 후보여야 지운다)
#[derive(Debug, Default)]
struct Seen {
    vms: HashSet<Uuid>,
    pools: HashSet<Uuid>,
}

struct Scanner {
    history: Arc<History>,
    cluster: Arc<Cluster>,
    pusher: Arc<dyn HistoryPusher>,
    seen: Seen,
    /// 받는 쪽이 `Invalid`나 `Disabled`로 거절한 VM: (연속 거절 수, 다시 보낼 수 있는 때)
    refused: Mutex<HashMap<Uuid, (u32, tokio::time::Instant)>>,
    /// 한 차례의 밀어 넘기기를 모두 기다리는 시간
    push_deadline: Duration,
}

impl Scanner {
    fn new(history: Arc<History>, cluster: Arc<Cluster>, pusher: Arc<dyn HistoryPusher>) -> Self {
        Self {
            history,
            cluster,
            pusher,
            seen: Seen::default(),
            refused: Mutex::default(),
            push_deadline: PUSH_DEADLINE,
        }
    }

    /// VM 파일을 훑어 주인이 다른 노드인 VM의 기록을 밀어 넘기고(받은 쪽이 합치면 지움), Raft에 없는 VM·풀의 파일을
    /// 계획 59 상태 디렉터리 정리와 같은 규칙으로 지운다: 두 차례 연속 후보, 리더 확인 읽기(`read_index`) 성공, 그
    /// id를 대상으로 하는 끝나지 않은 Task 없음, 지우기 바로 전에 다시 읽어도 없음. 이 노드에서 아직 재는 VM은 두지
    /// 않는다. 목록을 읽지 못하면 아무것도 하지 않는다. 밀어 넘기기는 동시에 여럿을 하고 모두 합쳐
    /// `push_deadline`까지만 기다린다(남은 것은 다음 차례). 30일 넘은 옆으로 옮긴 파일·임시 파일도 지운다.
    async fn scan(&mut self) {
        let history = self.history.clone();
        let files = tokio::task::spawn_blocking(move || {
            history.sweep_aside(ASIDE_KEEP);
            (
                history.files(SeriesKind::Vm),
                history.files(SeriesKind::Pool),
            )
        })
        .await;
        let Ok((vm_files, pool_files)) = files else {
            return;
        };
        if vm_files.is_empty() && pool_files.is_empty() {
            self.seen = Seen::default();
            return;
        }
        let caught_up = match self.cluster.read_index(SWEEP_READ_TIMEOUT).await {
            Ok(_) => true,
            Err(e) => {
                tracing::info!(error = %e, "cannot confirm the cluster state with the leader; not removing metrics history");
                false
            }
        };
        let (Ok((_, vms)), Ok((_, tasks)), Ok((_, pools)), Ok((_, nodes))) = (
            self.cluster.list::<VmSpec>(),
            self.cluster.list::<TaskSpec>(),
            self.cluster.list::<StoragePoolSpec>(),
            self.cluster.list::<NodeSpec>(),
        ) else {
            return;
        };
        let me = self.cluster.id().0;
        let now = self.history.clock.now_ms();
        let busy = |id: Uuid| {
            tasks
                .iter()
                .any(|t| t.spec.target == id && !t.status.state.is_finished())
        };
        let mut seen = Seen::default();
        let mut pushes = Vec::new();
        let clock = tokio::time::Instant::now();
        for id in vm_files {
            // 아직 재는 중이거나 기록하지 않은 점이 있으면 다음 차례에 본다(리뷰 2 #3: 밀어 넘긴 뒤 다음 기록이 파일을
            // 다시 만들지 않게).
            if busy(id)
                || (self.history.store.last_vm_t(id)).is_some_and(|t| now - t < BUSY_MS)
                || self.history.vm_pending(id)
            {
                continue;
            }
            let Some(vm) = vms.iter().find(|vm| vm.id == id) else {
                seen.vms.insert(id);
                if caught_up && self.seen.vms.contains(&id) {
                    self.remove_gone(Key::Vm(id)).await;
                }
                continue;
            };
            let Some(owner) = vm.status.node.or(vm.spec.node).filter(|o| *o != me) else {
                continue;
            };
            // 받는 쪽이 온라인이고 기록을 알 때만 보낸다(옛 노드는 다음 차례에 다시 본다).
            let ready = nodes.iter().any(|n| {
                n.id == owner
                    && n.status.online
                    && n.status.features.iter().any(|f| f == METRICS_HISTORY)
            });
            let refused =
                (self.refused.lock().unwrap().get(&id)).is_some_and(|(_, at)| *at > clock);
            if ready && !refused {
                pushes.push((owner, id));
            }
        }
        for id in pool_files {
            if busy(id) || pools.iter().any(|p| p.id == id) {
                continue;
            }
            seen.pools.insert(id);
            if caught_up && self.seen.pools.contains(&id) {
                self.remove_gone(Key::Pool(id)).await;
            }
        }
        self.seen = seen;
        self.refused
            .lock()
            .unwrap()
            .retain(|id, _| vms.iter().any(|vm| vm.id == *id));
        let this = &*self;
        let all = futures_util::stream::iter(by_owner(pushes))
            .for_each_concurrent(PUSH_CONCURRENCY, |(owner, id)| this.push(owner, id));
        if tokio::time::timeout(self.push_deadline, all).await.is_err() {
            tracing::info!(
                "pushing metrics history took too long; the rest waits for the next scan"
            );
        }
    }

    /// VM 기록을 주인 노드 `owner`에 밀어 넘기고, 받은 쪽이 합쳤으면 이 노드의 파일을 지운다.
    async fn push(&self, owner: Uuid, vm: Uuid) {
        let history = self.history.clone();
        let (bytes, seq) = match tokio::task::spawn_blocking(move || history.file_bytes(vm)).await {
            Ok(Ok(Some(read))) => read,
            Ok(Ok(None)) | Err(_) => return,
            Ok(Err(e)) => {
                tracing::warn!(vm = %vm, error = %e, "cannot read metrics history to push");
                return;
            }
        };
        let result = self.pusher.push(owner, vm, bytes).await;
        if !matches!(
            result,
            Err(HistoryError::Invalid(_) | HistoryError::Disabled)
        ) {
            self.refused.lock().unwrap().remove(&vm);
        }
        match result {
            Ok(()) => {
                let history = self.history.clone();
                match tokio::task::spawn_blocking(move || history.remove_pushed(vm, seq)).await {
                    Ok(Ok(true)) => {
                        tracing::info!(vm = %vm, node = %owner, "pushed metrics history to the VM's node")
                    }
                    // 읽은 뒤 바뀌었다(합쳐 넣었거나 새 점): 두고 다음 차례에 다시 보낸다.
                    Ok(Ok(false)) => {
                        tracing::info!(vm = %vm, node = %owner, "metrics history changed while pushing; keeping it for the next scan")
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(vm = %vm, error = %e, "cannot remove pushed metrics history")
                    }
                    Err(_) => {}
                }
            }
            // 옛 노드: 파일을 두고 다음 차례에 다시 본다(기록을 알리는 노드에만 보내므로 드물다).
            Err(e @ HistoryError::Unsupported) => {
                tracing::debug!(vm = %vm, node = %owner, error = %e, "the VM's node does not take metrics history; keeping it");
            }
            // 받는 쪽이 이 파일을 받을 수 없거나(옛 형식 등) 기록을 껐다: 파일은 두고(나중에 켜면 넘어가게) 거절할
            // 때마다 두 배로 늦춰 다시 본다(리뷰 2 #1).
            Err(e @ (HistoryError::Invalid(_) | HistoryError::Disabled)) => {
                let mut refused = self.refused.lock().unwrap();
                let n = refused.get(&vm).map_or(0, |(n, _)| *n).saturating_add(1);
                let wait = REFUSED_BACKOFF
                    .saturating_mul(1 << (n - 1).min(16))
                    .min(MAX_REFUSED_BACKOFF);
                refused.insert(vm, (n, tokio::time::Instant::now() + wait));
                if e == HistoryError::Disabled {
                    tracing::info!(vm = %vm, node = %owner, retry_in_secs = wait.as_secs(), "the VM's node has metrics history turned off; keeping it");
                } else {
                    tracing::warn!(vm = %vm, node = %owner, error = %e, retry_in_secs = wait.as_secs(), "the VM's node refused its metrics history; keeping it");
                }
            }
            Err(e) => {
                tracing::warn!(vm = %vm, node = %owner, error = %e, "cannot push metrics history; will retry");
            }
        }
    }

    /// Raft에 없는 VM·풀의 파일을 지우기 바로 전에 다시 읽어 여전히 없으면 지운다.
    async fn remove_gone(&self, key: Key) {
        let still_gone = match key {
            Key::Vm(id) => matches!(self.cluster.get::<VmSpec>(id), Ok(None)),
            Key::Pool(id) => matches!(self.cluster.get::<StoragePoolSpec>(id), Ok(None)),
            Key::Node => false,
        };
        if !still_gone {
            return;
        }
        let history = self.history.clone();
        match tokio::task::spawn_blocking(move || history.remove(key)).await {
            Ok(Ok(())) => {
                tracing::info!(series = ?key, "removed the metrics history of a deleted resource")
            }
            Ok(Err(e)) => {
                tracing::warn!(series = ?key, error = %e, "cannot remove metrics history of a deleted resource")
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use orbivisor_types::clock::ManualClock;
    use orbivisor_types::metrics::DIGEST_POINTS;
    use orbivisor_types::node::NodeStatus;
    use orbivisor_types::resource::Resource;
    use orbivisor_types::storage::PoolBackend;
    use orbivisor_types::task::TaskOp;

    use super::*;
    use crate::bootstrap::tests::single_node;

    /// 6시간 경계(모든 층의 칸 경계)
    const T0: i64 = 21_600_000 * 83_334;
    const GIB: u64 = 1 << 30;

    fn node_at(t: i64, cpu: f64) -> NodeSample {
        NodeSample {
            t,
            cpu: Some(cpu),
            mem_used_mib: Some(100),
            mem_total_mib: None,
            disk_read_bps: None,
            disk_write_bps: None,
            net_rx_bps: None,
            net_tx_bps: None,
            fs_used_mib: None,
            fs_total_mib: None,
        }
    }

    fn vm_at(t: i64, cpu: f64) -> VmSample {
        VmSample {
            t,
            cpu: Some(cpu),
            mem_rss_mib: Some(256),
            disk_read_bps: None,
            disk_write_bps: None,
            disk_read_iops: None,
            disk_write_iops: None,
            net_rx_bps: None,
            net_tx_bps: None,
        }
    }

    struct Fx {
        dir: tempfile::TempDir,
        store: Arc<MetricsStore>,
        clock: Arc<ManualClock>,
        history: History,
    }

    impl Fx {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MetricsStore::default());
            let clock = Arc::new(ManualClock::new(T0));
            let history = History::open(Some(dir.path().join(DIR)), store.clone(), clock.clone());
            Self {
                dir,
                store,
                clock,
                history,
            }
        }

        fn metrics_dir(&self) -> PathBuf {
            self.dir.path().join(DIR)
        }

        /// 같은 디렉터리를 빈 저장소로 다시 연다(재시작).
        fn reopen(&self) -> (Arc<MetricsStore>, History) {
            let store = Arc::new(MetricsStore::default());
            let history =
                History::open(Some(self.metrics_dir()), store.clone(), self.clock.clone());
            (store, history)
        }

        /// `T0`부터 5초마다 `n`점(노드 CPU = k, VM CPU = `vm_cpu`)을 넣는다.
        fn push(&self, n: i64, vm: Option<(Uuid, f64)>) {
            for k in 0..n {
                let t = T0 + k * 5_000;
                self.store.push_node(node_at(t, k as f64));
                if let Some((id, cpu)) = vm {
                    self.store.push_vm(id, vm_at(t, cpu));
                }
            }
        }
    }

    fn query(scope: HistoryScope, from: i64, until: i64, points: u16) -> HistoryQuery {
        HistoryQuery {
            scope,
            from,
            until,
            points,
        }
    }

    fn raw(history: &History, key: Key, from: i64, until: i64) -> Vec<Option<f32>> {
        let h = history.handle(key, false).unwrap().unwrap();
        let read = h.lock().unwrap().read(Tier::Raw5s, from, until).unwrap();
        read.column(MetricField::Cpu)
            .unwrap()
            .iter()
            .map(|a| (!a.is_unknown()).then_some(a.avg))
            .collect()
    }

    fn cpu(reply: &HistoryReply) -> &[Option<f32>] {
        &reply.columns["cpu"].avg
    }

    fn counted(n: i64) -> Vec<Option<f32>> {
        (0..n).map(|k| Some(k as f32)).collect()
    }

    /// 2분 동안의 점을 쓰고 다시 열면 저장소에 지난 1시간이 되살아난다. 1시간 조회는 5초 원본이고 백분위가 정확하다.
    #[test]
    fn flushed_points_survive_a_restart() {
        let f = Fx::new();
        let vm = Uuid::from_u128(1);
        f.push(24, Some((vm, 40.0)));
        f.clock.set(T0 + 121_000);
        f.history.flush(&HashMap::new());
        assert!(rrd::node_path(&f.metrics_dir()).exists());
        assert!(rrd::vm_path(&f.metrics_dir(), vm).exists());

        let (store, history) = f.reopen();
        let node = store.node_window(T0, T0 + 115_000);
        assert_eq!(node.len(), 24);
        assert_eq!(node[5], node_at(T0 + 25_000, 5.0));
        assert_eq!(store.vm_window(vm, T0, T0 + 115_000).len(), 24);

        let q = query(HistoryScope::Node, T0, T0 + 115_000, 300);
        let reply = history.read(&q).unwrap();
        assert_eq!(
            (reply.tier, reply.step_ms, reply.start),
            (Tier::Raw5s, 5_000, T0)
        );
        assert_eq!(cpu(&reply), counted(24));
        // 5초 층은 AVG=MAX=MIN
        assert_eq!(reply.columns["cpu"].max, counted(24));
        assert_eq!(reply.columns["mem_used"].avg, vec![Some(100.0); 24]);
        let pct = reply.pct["cpu"];
        assert_eq!(
            (pct.p50, pct.p95, pct.approx),
            (Some(11.0), Some(22.0), false)
        );
        assert_eq!(reply.stats["cpu"].max, Some(23.0));
        assert_eq!(reply.covered_from, None);
        // 되살린 점은 다시 쓰지 않는다(시계 뒤로 경고 없음).
        history.flush(&HashMap::new());
        assert!(history.behind.lock().unwrap().is_empty());
    }

    /// 1시간보다 긴 범위는 1분 층(AVG·MAX·MIN)이고 백분위는 근사다. 앞 칸을 모르면 `covered_from`이 알린다.
    #[test]
    fn longer_ranges_read_minute_buckets() {
        let f = Fx::new();
        f.push(24, None);
        f.clock.set(T0 + 121_000);
        f.history.flush(&HashMap::new());
        let q = query(HistoryScope::Node, T0 - 7_200_000, T0 + 121_000, 300);
        let reply = f.history.read(&q).unwrap();
        assert_eq!((reply.tier, reply.step_ms), (Tier::Min1, 60_000));
        assert_eq!(reply.start, T0 - 7_200_000);
        let col = &reply.columns["cpu"];
        assert_eq!(col.avg.len(), 123);
        assert_eq!(&col.avg[120..], [Some(5.5), Some(17.5), None]);
        assert_eq!(&col.max[120..], [Some(11.0), Some(23.0), None]);
        assert_eq!(&col.min[120..], [Some(0.0), Some(12.0), None]);
        assert!(col.avg[..120].iter().all(Option::is_none));
        assert_eq!(reply.covered_from, Some(T0));
        let stat = reply.stats["cpu"];
        assert_eq!(
            (stat.avg, stat.max, stat.min),
            (Some(11.5), Some(23.0), Some(0.0))
        );
        assert!(reply.pct["cpu"].approx);

        // 점 수 상한으로 묶는다.
        let reply = f.history.read(&HistoryQuery { points: 50, ..q }).unwrap();
        assert_eq!(reply.step_ms, 3 * 60_000);
        assert_eq!(cpu(&reply).len(), 41);
        assert_eq!(cpu(&reply)[40], Some(11.5));
    }

    /// 아직 파일에 쓰지 않은 점은 저장소에서 메운다. 기록을 끈 노드도 1시간은 메모리로 답하고 밀어 넘기기는 거절한다.
    #[test]
    fn unflushed_points_come_from_memory() {
        let f = Fx::new();
        f.push(6, None);
        f.clock.set(T0 + 30_000);
        let q = query(HistoryScope::Node, T0, T0 + 25_000, 300);
        assert_eq!(cpu(&f.history.read(&q).unwrap()), counted(6));

        let store = Arc::new(MetricsStore::default());
        let off = History::open(None, store.clone(), f.clock.clone());
        assert!(!off.enabled());
        store.push_node(node_at(T0, 3.0));
        off.flush(&HashMap::new());
        let reply = off.read(&q).unwrap();
        assert_eq!(cpu(&reply)[0], Some(3.0));
        assert_eq!(off.receive(Uuid::nil(), b"x"), Err(HistoryError::Disabled));
        let vm = off
            .read(&query(
                HistoryScope::Vm { id: Uuid::nil() },
                T0,
                T0 + 25_000,
                300,
            ))
            .unwrap();
        assert!(cpu(&vm).iter().all(Option::is_none));
        // 잘못된 조회
        let bad = off
            .read(&query(HistoryScope::Node, T0, T0, 300))
            .unwrap_err();
        assert!(matches!(bad, HistoryError::Invalid(_)), "{bad:?}");
    }

    /// 시계가 1시간 안으로 뒤로 가면 쓰지 않고(한 번 경고), 1시간 넘게 뒤로 가면 뒤 칸을 지우고 이어 쓴다.
    #[test]
    fn a_clock_going_back_is_skipped_or_rewound() {
        let f = Fx::new();
        f.push(24, None);
        f.clock.set(T0 + 121_000);
        f.history.flush(&HashMap::new());

        f.store.push_node(node_at(T0 + 60_000, 999.0));
        f.history.flush(&HashMap::new());
        assert!(f.history.behind.lock().unwrap().contains(&Key::Node));
        assert_eq!(raw(&f.history, Key::Node, T0, T0 + 115_000), counted(24));

        f.clock.set(T0 - 7_200_000 + 1_000);
        f.store.push_node(node_at(T0 - 7_200_000, 7.0));
        f.history.flush(&HashMap::new());
        assert!(f.history.behind.lock().unwrap().is_empty());
        assert_eq!(
            raw(&f.history, Key::Node, T0 - 7_200_000, T0 - 7_200_000),
            [Some(7.0)]
        );
        let h = f.history.handle(Key::Node, false).unwrap().unwrap();
        assert_eq!(
            h.lock().unwrap().last(Tier::Raw5s),
            Some((T0 - 7_200_000) / 5_000)
        );
    }

    /// 풀은 지난 기록 뒤의 5초 칸마다 지금 값(MiB)을 쓴다(처음은 지금 칸 하나).
    #[test]
    fn pools_are_written_for_every_bucket_since_the_last_flush() {
        let f = Fx::new();
        let pool = Uuid::from_u128(5);
        let levels = HashMap::from([(pool, (10 * GIB, 4 * GIB))]);
        f.clock.set(T0 + 61_000);
        f.history.flush(&levels);
        f.clock.set(T0 + 121_000);
        f.history.flush(&levels);
        let reply = f
            .history
            .read(&query(
                HistoryScope::Pool { id: pool },
                T0,
                T0 + 121_000,
                300,
            ))
            .unwrap();
        let used = &reply.columns["pool_used"].avg;
        assert_eq!(used.len(), 25);
        assert!(used[..12].iter().all(Option::is_none));
        assert!(used[12..].iter().all(|v| *v == Some(6_144.0)));
        assert_eq!(reply.columns["pool_total"].avg[24], Some(10_240.0));
        // 같은 5초 안에 다시 기록해도(멈출 때) 시계가 뒤로 갔다고 보지 않는다.
        f.history.flush(&levels);
        assert!(f.history.behind.lock().unwrap().is_empty());
        assert_eq!(reply.covered_from, Some(T0 + 60_000));
        // 닫힌 1분 칸
        let day = f
            .history
            .read(&query(
                HistoryScope::Pool { id: pool },
                T0 - 7_200_000,
                T0 + 121_000,
                300,
            ))
            .unwrap();
        assert_eq!(day.columns["pool_used"].avg[121], Some(6_144.0));
        // 이 노드에 없는 풀
        let none = f
            .history
            .read(&query(
                HistoryScope::Pool { id: Uuid::nil() },
                T0,
                T0 + 60_000,
                300,
            ))
            .unwrap();
        assert!(none.columns["pool_used"].avg.iter().all(Option::is_none));
    }

    /// 밀어 넘긴 파일은 받는 쪽 파일과 합쳐지고, 잘못된 파일은 거절한다.
    #[test]
    fn pushed_history_is_merged_and_bad_files_are_refused() {
        let a = Fx::new();
        let vm = Uuid::from_u128(2);
        a.push(24, Some((vm, 30.0)));
        a.clock.set(T0 + 121_000);
        a.history.flush(&HashMap::new());
        let (bytes, _) = a.history.file_bytes(vm).unwrap().unwrap();
        assert!(bytes.len() < MAX_HISTORY_BYTES);
        assert_eq!(a.history.file_bytes(Uuid::nil()).unwrap(), None);

        let b = Fx::new();
        b.clock.set(T0 + 121_000);
        b.history.receive(vm, &bytes).unwrap();
        // 두 번 받아도 같다.
        b.history.receive(vm, &bytes).unwrap();
        let q = query(HistoryScope::Vm { id: vm }, T0, T0 + 115_000, 300);
        assert_eq!(cpu(&b.history.read(&q).unwrap()), vec![Some(30.0); 24]);

        let invalid = |r: Result<(), HistoryError>| matches!(r, Err(HistoryError::Invalid(_)));
        assert!(invalid(b.history.receive(vm, b"not a history file")));
        let node = std::fs::read(rrd::node_path(&a.metrics_dir())).unwrap();
        assert!(invalid(b.history.receive(vm, &node)));
        assert!(invalid(
            b.history.receive(vm, &vec![0; MAX_HISTORY_BYTES + 1])
        ));
        // 받은 쪽 파일은 그대로다.
        assert_eq!(cpu(&b.history.read(&q).unwrap()), vec![Some(30.0); 24]);

        a.history.remove(Key::Vm(vm)).unwrap();
        assert!(!rrd::vm_path(&a.metrics_dir(), vm).exists());
        assert_eq!(a.history.file_bytes(vm).unwrap(), None);
    }

    /// 모으기는 노드 열의 AVG와 범위 평균 CPU 상위 VM이다.
    #[test]
    fn digests_have_node_averages_and_the_top_vms() {
        let f = Fx::new();
        let (low, high) = (Uuid::from_u128(3), Uuid::from_u128(4));
        f.push(24, Some((low, 10.0)));
        for k in 0..24 {
            f.store.push_vm(high, vm_at(T0 + k * 5_000, 50.0));
        }
        f.clock.set(T0 + 121_000);
        f.history.flush(&HashMap::new());
        let q = query(HistoryScope::Digest { top: 1 }, T0, T0 + 115_000, 12);
        let reply = f.history.read(&q).unwrap();
        let col = &reply.columns["cpu"];
        assert_eq!(col.avg.len(), 12);
        assert_eq!(col.avg[0], Some(0.5));
        assert!(col.max.is_empty() && col.min.is_empty());
        assert!(reply.stats.is_empty() && reply.pct.is_empty());
        assert_eq!(
            reply.top,
            [TopVm {
                vm: high,
                cpu: Some(50.0),
                mem_rss: Some(256.0),
            }]
        );
        let json = serde_json::to_value(&reply).unwrap();
        assert!(json["columns"]["cpu"].get("max").is_none(), "{json}");
        assert_eq!(DIGEST_POINTS, 120);
    }

    /// 열린 파일이 상한을 넘으면 가장 오래 쓰지 않은 VM·풀 파일을 닫는다(노드 파일은 두다).
    #[test]
    fn the_least_recently_used_files_are_closed() {
        let mut f = Fx::new();
        f.history.max_open = 2;
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        for key in [Key::Node, Key::Vm(a), Key::Vm(b), Key::Pool(a)] {
            f.history.handle(key, true).unwrap().unwrap();
        }
        let open = f.history.open.lock().unwrap();
        assert_eq!(open.files.len(), 2);
        assert!(open.files.contains_key(&Key::Node));
        assert!(open.files.contains_key(&Key::Pool(a)));
        drop(open);
        // 닫은 파일도 디스크에 있다.
        assert!(f.history.handle(Key::Vm(a), false).unwrap().is_some());
        assert_eq!(f.history.files(SeriesKind::Vm), [a, b]);
        // 옆으로 옮긴 파일·임시 파일은 시리즈가 아니다.
        let vm_dir = f.metrics_dir().join("vm");
        std::fs::write(vm_dir.join(format!("{a}.rrd.corrupt")), b"").unwrap();
        std::fs::write(vm_dir.join(format!("{b}.rrd.tmp")), b"").unwrap();
        std::fs::write(vm_dir.join("junk.rrd"), b"").unwrap();
        assert_eq!(f.history.files(SeriesKind::Vm), [a, b]);
    }

    #[test]
    fn flushes_follow_the_wall_clock_period() {
        let minute = Duration::from_secs(60);
        assert_eq!(until_next_flush(T0, minute), Duration::from_millis(1_000));
        assert_eq!(until_next_flush(T0 + 1_000, minute), minute);
        assert_eq!(
            until_next_flush(T0 + 30_000, minute),
            Duration::from_millis(31_000)
        );
        let ten = Duration::from_secs(600);
        assert_eq!(
            until_next_flush(T0 + 61_000, ten),
            Duration::from_millis(540_000)
        );
    }

    /// 쉬는 파일은 마지막 원본 칸이 든 6시간 경계까지만 닫는다.
    #[test]
    fn idle_files_close_up_to_their_six_hour_edge() {
        let f = Fx::new();
        let h = f.history.handle(Key::Node, true).unwrap().unwrap();
        let mut rrd = h.lock().unwrap();
        assert_eq!(close_until(&rrd, T0), None);
        rrd.append(T0 + 5_000, &[Some(1.0); 9]).unwrap();
        assert_eq!(close_until(&rrd, T0 + 60_000), Some(T0 + 60_000));
        assert_eq!(close_until(&rrd, T0 + 86_400_000), Some(T0 + 21_600_000));
    }

    #[derive(Default)]
    struct FakePusher {
        pushed: Mutex<Vec<(Uuid, Uuid)>>,
        /// 해 본 밀어 넘기기(거절·멈춤 포함)
        tried: Mutex<Vec<(Uuid, Uuid)>>,
        /// 이 노드는 옛 노드처럼 거절한다.
        old: Option<Uuid>,
        /// 이 노드는 받을 수 없는 파일이라 `Invalid`로 거절한다.
        invalid: Option<Uuid>,
        /// 이 노드는 답하지 않는다.
        hung: Option<Uuid>,
        /// 이 노드는 기록을 꺼 `Disabled`로 거절한다.
        disabled: Option<Uuid>,
    }

    #[async_trait]
    impl HistoryPusher for FakePusher {
        async fn push(&self, node: Uuid, vm: Uuid, bytes: Vec<u8>) -> Result<(), HistoryError> {
            assert!(!bytes.is_empty());
            self.tried.lock().unwrap().push((node, vm));
            if self.old == Some(node) {
                return Err(HistoryError::Unsupported);
            }
            if self.invalid == Some(node) {
                return Err(HistoryError::Invalid("unknown field id".into()));
            }
            if self.hung == Some(node) {
                return std::future::pending().await;
            }
            if self.disabled == Some(node) {
                return Err(HistoryError::Disabled);
            }
            self.pushed.lock().unwrap().push((node, vm));
            Ok(())
        }
    }

    async fn add_node(cluster: &Cluster, name: &str, features: &[&str]) -> Uuid {
        let node = Resource::new(
            name,
            NodeSpec {
                sdn: None,
                host_resources: None,
                address: format!("{name}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        cluster.create(&node).await.unwrap();
        let status = NodeStatus {
            online: true,
            features: features.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        };
        cluster
            .update_status::<NodeSpec>(node.id, &status)
            .await
            .unwrap();
        node.id
    }

    async fn add_vm(cluster: &Cluster, name: &str, node: Option<Uuid>) -> Uuid {
        let spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 },
            "memory_mib": 256,
            "machine": "q35",
            "disks": [],
            "nics": [],
            "power": "stopped",
            "node": node,
        }))
        .unwrap();
        let vm = Resource::new(name, spec);
        cluster.create(&vm).await.unwrap();
        vm.id
    }

    /// 주인이 다른 노드인 VM 기록은 그 노드가 기록을 알 때 밀어 넘기고 지운다. Raft에 없는 VM·풀의 파일은 두 차례
    /// 연속 후보일 때 지우고, 끝나지 않은 Task의 대상이거나 아직 이 노드에서 재는 VM은 두다.
    #[tokio::test]
    async fn scans_push_moved_vms_and_remove_deleted_ones() {
        let f = Fx::new();
        let (cluster, NodeId(me)) = single_node(&f.dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let new = add_node(&cluster, "new", &[METRICS_HISTORY]).await;
        let old = add_node(&cluster, "old", &[]).await;
        let refusing = add_node(&cluster, "refusing", &[METRICS_HISTORY]).await;
        let here = add_vm(&cluster, "here", Some(me)).await;
        let moved = add_vm(&cluster, "moved", Some(new)).await;
        let on_old = add_vm(&cluster, "on-old", Some(old)).await;
        let on_refusing = add_vm(&cluster, "on-refusing", Some(refusing)).await;
        let (gone, gone_task, gone_busy) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let task = Resource::new(
            "t",
            TaskSpec {
                op: TaskOp::VmStart,
                target: gone_task,
                node: me,
            },
        );
        cluster.create(&task).await.unwrap();
        let pool = Resource::new(
            "local",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/var/lib/orbivisor/images".into(),
                },
                nodes: vec![],
                shared: false,
                backups: false,
                folder: None,
            },
        );
        cluster.create(&pool).await.unwrap();
        let gone_pool = Uuid::now_v7();
        for key in [here, moved, on_old, on_refusing, gone, gone_task, gone_busy]
            .map(Key::Vm)
            .into_iter()
            .chain([Key::Pool(pool.id), Key::Pool(gone_pool)])
        {
            f.history.handle(key, true).unwrap().unwrap();
        }
        f.clock.set(T0 + 600_000);
        f.store.push_vm(gone_busy, vm_at(T0 + 590_000, 1.0));
        let pusher = Arc::new(FakePusher {
            old: Some(refusing),
            ..Default::default()
        });
        let history = Arc::new(f.history);
        let mut scanner = Scanner::new(history.clone(), cluster.clone(), pusher.clone());
        let vms = |h: &History| h.files(SeriesKind::Vm).into_iter().collect::<HashSet<_>>();

        scanner.scan().await;
        assert_eq!(*pusher.pushed.lock().unwrap(), [(new, moved)]);
        assert_eq!(
            vms(&history),
            HashSet::from([here, on_old, on_refusing, gone, gone_task, gone_busy])
        );
        assert_eq!(history.files(SeriesKind::Pool).len(), 2);

        scanner.scan().await;
        assert_eq!(pusher.pushed.lock().unwrap().len(), 1);
        assert_eq!(
            vms(&history),
            HashSet::from([here, on_old, on_refusing, gone_task, gone_busy])
        );
        assert_eq!(history.files(SeriesKind::Pool), [pool.id]);

        // 옛 노드가 기록을 알리면 다음 차례에 밀어 넘긴다.
        let status = NodeStatus {
            online: true,
            features: vec![METRICS_HISTORY.into()],
            ..Default::default()
        };
        cluster
            .update_status::<NodeSpec>(old, &status)
            .await
            .unwrap();
        scanner.scan().await;
        assert_eq!(pusher.pushed.lock().unwrap().last(), Some(&(old, on_old)));
        assert!(!vms(&history).contains(&on_old));
        cluster.shutdown().await.unwrap();
    }

    async fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(tokio::time::Instant::now() < deadline, "{what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 쓰는 작업은 벽시계 주기 경계(+1초)에 기록하고, 멈출 때 한 번 더 기록한다. 이 노드에 기록이 있는 VM이 바뀌면
    /// 정리를 앞당겨 새 주인에게 밀어 넘긴다.
    #[tokio::test]
    async fn the_writer_flushes_on_the_period_and_pushes_after_a_vm_moves() {
        let Fx {
            dir,
            store,
            clock,
            history,
        } = Fx::new();
        let (cluster, _) = single_node(&dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let new = add_node(&cluster, "new", &[METRICS_HISTORY]).await;
        let vm = add_vm(&cluster, "web", None).await;
        let history = Arc::new(history);
        history.handle(Key::Vm(vm), true).unwrap().unwrap();
        // 다음 경계(+1초)까지 1.5초
        clock.set(T0 + 59_500);
        store.push_node(node_at(T0, 1.0));
        let pusher = Arc::new(FakePusher::default());
        let hour = Duration::from_secs(3600);
        let writer = HistoryWriter::spawn(
            history.clone(),
            cluster.clone(),
            Arc::new(PoolLevels::default()),
            pusher.clone(),
            WriterConfig {
                flush: Duration::from_secs(60),
                scan: hour,
                first_scan: hour,
                after_change: Duration::from_millis(100),
            },
        );
        let node_file = rrd::node_path(&dir.path().join(DIR));
        wait_until("no flush on the period", || node_file.exists()).await;

        // VM이 다른 노드로 옮겨 갔다.
        let mut moved = cluster.get::<VmSpec>(vm).unwrap().unwrap();
        moved.spec.node = Some(new);
        cluster
            .update_spec(vm, None, Default::default(), &moved.spec)
            .await
            .unwrap();
        wait_until("the VM history was not pushed", || {
            pusher.pushed.lock().unwrap().as_slice() == [(new, vm)]
        })
        .await;
        wait_until("the pushed file was not removed", || {
            !rrd::vm_path(&dir.path().join(DIR), vm).exists()
        })
        .await;

        store.push_node(node_at(T0 + 5_000, 2.0));
        writer.shutdown().await;
        assert_eq!(
            raw(&history, Key::Node, T0, T0 + 5_000),
            [Some(1.0), Some(2.0)]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 답하지 않는 노드로 밀어 넘기는 동안에도 기록 주기는 돌고, 멈출 때는 정리를 끊고 마지막으로 기록한다
    /// (리뷰 1 #3).
    #[tokio::test]
    async fn flushes_continue_while_a_scan_is_stuck() {
        let Fx {
            dir,
            store,
            clock,
            history,
        } = Fx::new();
        let (cluster, _) = single_node(&dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let hung = add_node(&cluster, "hung", &[METRICS_HISTORY]).await;
        let vm = add_vm(&cluster, "web", Some(hung)).await;
        let history = Arc::new(history);
        history.handle(Key::Vm(vm), true).unwrap().unwrap();
        clock.set(T0 + 59_500);
        store.push_node(node_at(T0, 1.0));
        let pusher = Arc::new(FakePusher {
            hung: Some(hung),
            ..Default::default()
        });
        let hour = Duration::from_secs(3600);
        let writer = HistoryWriter::spawn(
            history.clone(),
            cluster.clone(),
            Arc::new(PoolLevels::default()),
            pusher.clone(),
            WriterConfig {
                flush: Duration::from_secs(60),
                scan: hour,
                first_scan: Duration::from_millis(10),
                after_change: hour,
            },
        );
        wait_until("the scan never pushed", || {
            !pusher.tried.lock().unwrap().is_empty()
        })
        .await;
        let node_file = rrd::node_path(&dir.path().join(DIR));
        wait_until("no flush while the scan was stuck", || node_file.exists()).await;
        store.push_node(node_at(T0 + 5_000, 2.0));
        tokio::time::timeout(Duration::from_secs(5), writer.shutdown())
            .await
            .expect("shutting down waited for the stuck scan");
        assert_eq!(
            raw(&history, Key::Node, T0, T0 + 5_000),
            [Some(1.0), Some(2.0)]
        );
        assert!(rrd::vm_path(&dir.path().join(DIR), vm).exists());
        cluster.shutdown().await.unwrap();
    }

    /// 밀어 넘기기는 동시에 하고 한 차례 기한이 있어 답하지 않는 노드가 다른 VM의 밀어 넘기기를 막지 않는다. `Invalid`로
    /// 거절된 VM은 바로 다시 보내지 않는다(리뷰 1 #3·#11).
    #[tokio::test]
    async fn pushes_run_together_under_a_deadline_and_refusals_back_off() {
        let f = Fx::new();
        let (cluster, _) = single_node(&f.dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let hung = add_node(&cluster, "hung", &[METRICS_HISTORY]).await;
        let new = add_node(&cluster, "new", &[METRICS_HISTORY]).await;
        let picky = add_node(&cluster, "picky", &[METRICS_HISTORY]).await;
        let stuck = add_vm(&cluster, "stuck", Some(hung)).await;
        let moved = add_vm(&cluster, "moved", Some(new)).await;
        let refused = add_vm(&cluster, "refused", Some(picky)).await;
        for vm in [stuck, moved, refused] {
            f.history.handle(Key::Vm(vm), true).unwrap().unwrap();
        }
        let pusher = Arc::new(FakePusher {
            hung: Some(hung),
            invalid: Some(picky),
            ..Default::default()
        });
        let history = Arc::new(f.history);
        let mut scanner = Scanner::new(history.clone(), cluster.clone(), pusher.clone());
        scanner.push_deadline = Duration::from_millis(500);
        let vms = |h: &History| h.files(SeriesKind::Vm).into_iter().collect::<HashSet<_>>();
        tokio::time::timeout(Duration::from_secs(5), scanner.scan())
            .await
            .expect("the scan did not stop at its deadline");
        assert_eq!(*pusher.pushed.lock().unwrap(), [(new, moved)]);
        assert_eq!(vms(&history), HashSet::from([stuck, refused]));
        let tried = |vm| {
            (pusher.tried.lock().unwrap().iter())
                .filter(|(_, v)| *v == vm)
                .count()
        };
        assert_eq!(tried(refused), 1);
        scanner.scan().await;
        assert_eq!(tried(refused), 1, "a refused push was retried at once");
        assert_eq!(tried(stuck), 2);
        cluster.shutdown().await.unwrap();
    }

    /// 밀어 넘긴 뒤 그사이 합쳐 넣거나 새로 쓴 파일은 지우지 않는다(리뷰 1 #5).
    #[test]
    fn a_pushed_file_that_changed_meanwhile_is_kept() {
        let f = Fx::new();
        let vm = Uuid::now_v7();
        f.push(12, Some((vm, 5.0)));
        f.history.flush(&HashMap::new());
        let (bytes, seq) = f.history.file_bytes(vm).unwrap().unwrap();
        // 그사이 다른 노드가 같은 VM 기록을 합쳐 넣었다.
        f.history.receive(vm, &bytes).unwrap();
        assert!(!f.history.remove_pushed(vm, seq).unwrap());
        assert!(rrd::vm_path(&f.metrics_dir(), vm).exists());
        let (_, seq) = f.history.file_bytes(vm).unwrap().unwrap();
        assert!(f.history.remove_pushed(vm, seq).unwrap());
        assert!(!rrd::vm_path(&f.metrics_dir(), vm).exists());
        // 지운 뒤 받은 것은 새 파일에 합친다.
        f.history.receive(vm, &bytes).unwrap();
        assert!(rrd::vm_path(&f.metrics_dir(), vm).exists());
    }

    /// 30일 넘은, 이 형식 파일 옆의 `.corrupt*`·`.v*`·`.tmp`만 지운다(리뷰 1 #11).
    #[test]
    fn old_set_aside_files_are_removed() {
        let f = Fx::new();
        let dir = f.metrics_dir();
        let id = Uuid::now_v7();
        std::fs::create_dir_all(dir.join("vm")).unwrap();
        std::fs::create_dir_all(dir.join("pool")).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(31 * 86_400);
        let make = |path: PathBuf, modified: std::time::SystemTime| {
            let file = std::fs::File::create(&path).unwrap();
            file.set_modified(modified).unwrap();
            path
        };
        let gone = [
            make(dir.join("node.rrd.corrupt"), old),
            make(dir.join("vm").join(format!("{id}.rrd.v2")), old),
            make(dir.join("pool").join(format!("{id}.rrd.tmp")), old),
            make(dir.join("vm").join(format!("{id}.rrd.corrupt.1")), old),
        ];
        let kept = [
            make(dir.join("vm").join(format!("{id}.rrd")), old),
            make(
                dir.join("vm").join(format!("{id}.rrd.corrupt.2")),
                std::time::SystemTime::now(),
            ),
            make(dir.join("vm").join("notes.rrd.corrupt"), old),
            make(dir.join("vm").join(format!("{id}.rrd.bak")), old),
        ];
        assert_eq!(f.history.sweep_aside(ASIDE_KEEP), gone.len());
        assert!(gone.iter().all(|p| !p.exists()));
        assert!(kept.iter().all(|p| p.exists()));
    }

    /// 기록을 끈 노드로 옮긴 VM은 거절(`Disabled`)될 때마다 늦춰 다시 보내고 파일은 둔다(리뷰 2 #1).
    #[tokio::test]
    async fn a_node_with_history_off_backs_off_and_keeps_the_file() {
        let f = Fx::new();
        let (cluster, _) = single_node(&f.dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let sd = add_node(&cluster, "sd", &[METRICS_HISTORY]).await;
        let vm = add_vm(&cluster, "web", Some(sd)).await;
        f.history.handle(Key::Vm(vm), true).unwrap().unwrap();
        let pusher = Arc::new(FakePusher {
            disabled: Some(sd),
            ..Default::default()
        });
        let history = Arc::new(f.history);
        let mut scanner = Scanner::new(history.clone(), cluster.clone(), pusher.clone());
        scanner.scan().await;
        scanner.scan().await;
        assert_eq!(
            pusher.tried.lock().unwrap().len(),
            1,
            "pushed again at once"
        );
        assert!(rrd::vm_path(&history.dir.clone().unwrap(), vm).exists());
        let (n, at) = scanner.refused.lock().unwrap()[&vm];
        assert_eq!(n, 1);
        assert!(at > tokio::time::Instant::now() + REFUSED_BACKOFF - Duration::from_secs(5));
        cluster.shutdown().await.unwrap();
    }

    /// 기록하지 않은 점이 남은 VM은 밀어 넘기지 않고, 기록한 뒤에 밀어 넘긴다(리뷰 2 #3).
    #[tokio::test]
    async fn vms_with_unflushed_points_are_pushed_after_the_flush() {
        let f = Fx::new();
        let (cluster, _) = single_node(&f.dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let new = add_node(&cluster, "new", &[METRICS_HISTORY]).await;
        let vm = add_vm(&cluster, "web", Some(new)).await;
        f.store.push_vm(vm, vm_at(T0, 1.0));
        f.history.flush(&HashMap::new());
        // 떠나기 바로 전의 점(BUSY_MS보다 오래됐지만 아직 파일에 쓰지 않음)
        f.store.push_vm(vm, vm_at(T0 + 5_000, 2.0));
        f.clock.set(T0 + 5_000 + BUSY_MS + 1);
        let pusher = Arc::new(FakePusher::default());
        let history = Arc::new(f.history);
        let mut scanner = Scanner::new(history.clone(), cluster.clone(), pusher.clone());
        scanner.scan().await;
        assert!(pusher.tried.lock().unwrap().is_empty());
        history.flush(&HashMap::new());
        scanner.scan().await;
        assert_eq!(*pusher.pushed.lock().unwrap(), [(new, vm)]);
        assert!(!rrd::vm_path(&history.dir.clone().unwrap(), vm).exists());
        // 그 뒤의 기록은 파일을 다시 만들지 않는다.
        history.flush(&HashMap::new());
        assert!(!rrd::vm_path(&history.dir.clone().unwrap(), vm).exists());
        cluster.shutdown().await.unwrap();
    }

    /// 파일을 여는(만드는) 동안에도 열린 파일은 바로 준다(리뷰 2 #6).
    #[test]
    fn open_files_are_served_while_another_file_is_being_created() {
        let f = Fx::new();
        f.push(1, None);
        f.history.flush(&HashMap::new());
        let history = Arc::new(f.history);
        let creating = history.opening.lock().unwrap();
        let h = history.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(h.handle(Key::Node, false).unwrap().is_some()));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
        drop(creating);
    }

    /// 파일 오류는 종류마다 모아 10분에 한 번까지만 경고한다(리뷰 2 #7).
    #[test]
    fn file_failures_are_summarized_and_rate_limited() {
        let f = Fx::new();
        // `vm`이 파일이라 VM 파일을 만들 수 없다(디스크가 찬 것처럼 모든 VM 파일이 실패).
        std::fs::write(f.metrics_dir().join("vm"), b"").unwrap();
        let vms: Vec<Uuid> = (0..3).map(|_| Uuid::now_v7()).collect();
        for vm in &vms {
            f.store.push_vm(*vm, vm_at(T0, 1.0));
        }
        assert_eq!(f.history.flush(&HashMap::new()), 3);
        assert!(f.history.warned.lock().unwrap().contains_key("open"));
        let mut failures = Failures::default();
        failures.note("open", Key::Vm(vms[0]), &"no space left on device");
        failures.note("sync", Key::Node, &"no space left on device");
        // "open"은 방금 경고했으므로 "sync"만
        assert_eq!(f.history.report(&failures, T0 + 60_000), 1);
        assert_eq!(f.history.report(&failures, T0 + 120_000), 0);
        assert_eq!(
            f.history
                .report(&failures, T0 + 60_000 + FAILURE_WARN_EVERY_MS),
            2
        );
    }

    /// 밀어 넘기기는 주인 노드마다 돌아가며 한다(리뷰 2 #8).
    #[test]
    fn pushes_are_interleaved_by_owner() {
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let vm = Uuid::from_u128;
        let pushes = vec![
            (a, vm(10)),
            (a, vm(11)),
            (a, vm(12)),
            (b, vm(20)),
            (c, vm(30)),
            (b, vm(21)),
        ];
        assert_eq!(
            by_owner(pushes),
            [
                (a, vm(10)),
                (b, vm(20)),
                (c, vm(30)),
                (a, vm(11)),
                (b, vm(21)),
                (a, vm(12)),
            ]
        );
    }
}
