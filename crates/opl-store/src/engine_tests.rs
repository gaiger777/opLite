//! GC·검증 엔진 시험: 시나리오(메모리·로컬·가짜 S3)와 GC 안전 모형 proptest(설계 §10.3).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use opl_types::backup_store::{ChunkId, VerifyMode, ZERO_CHUNK};
use proptest::prelude::*;
use uuid::Uuid;

use crate::backend::local::LocalBackend;
use crate::backend::mem::MemBackend;
use crate::backend::s3::S3Backend;
use crate::backend::{Backend, StoreError, list_all};
use crate::crypto::MasterKey;
use crate::fake_s3::FakeS3;
use crate::format::chunk::is_zero;
use crate::format::chunk_key;
use crate::format::index::FixedIndex;
use crate::format::manifest::{CONFIG_BLOB, ConfigBlob, VARS_BLOB};
use crate::gc;
use crate::store::Store;
use crate::verify::{ExpectedDisk, Verifier};

/// 시험 청크 크기(형식은 4 MiB지만 인덱스의 chunk_bytes만 맞으면 된다 — 마지막 청크처럼 짧은 청크)
const SMALL: usize = 64;

fn config() -> Vec<u8> {
    let c = ConfigBlob {
        vm_name: "web".into(),
        vm_spec: serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 }, "memory_mib": 512, "machine": "virt"
        }))
        .unwrap(),
        uefi_vars: true,
        tpm_state: false,
    };
    serde_json::to_vec(&c).unwrap()
}

/// 청크들(각 `SMALL` 바이트 이하)로 디스크 하나짜리 스냅샷을 쓴다(청크 → 블롭 → 인덱스 → 매니페스트). 인덱스의 청크
/// 크기가 `SMALL`이라 [`Store::read_index`](매니페스트와 store.json의 청크 크기 확인)로는 읽히지 않는다 — GC 시험용.
async fn write_snapshot(s: &Store, vm: Uuid, backup: Uuid, chunks: &[Vec<u8>]) -> Vec<ChunkId> {
    let mut ix = FixedIndex::new(
        backup,
        (chunks.len() * SMALL) as u64,
        SMALL as u32,
        s.encrypted(),
    );
    for (i, raw) in chunks.iter().enumerate() {
        let mut full = raw.clone();
        full.resize(SMALL, 0);
        if is_zero(&full) {
            continue;
        }
        let c = s.encode_chunk(&full);
        if !s.has_chunk(&c.id).await.unwrap() {
            s.put_chunk(&c).await.unwrap();
        }
        ix.ids[i] = c.id;
    }
    let cfg = s
        .put_blob(vm, backup, CONFIG_BLOB, &config())
        .await
        .unwrap();
    let vars = s
        .put_blob(vm, backup, VARS_BLOB, &[7u8; 100])
        .await
        .unwrap();
    let disk = s.put_index(vm, backup, "disk0", &ix).await.unwrap();
    let mut m = s.new_manifest(vm, backup, 1, vec![disk], vec![cfg, vars]);
    s.put_manifest(&mut m).await.unwrap();
    ix.ids
}

/// 4 MiB 청크 크기의 인덱스로(가상 크기 = 청크 수 × 4 MiB) — 검증 시험용. 청크 내용은 짧게 두지만 검증은 길이를 보지
/// 않고 풀기·id만 본다.
async fn write_snapshot_full(
    s: &Store,
    vm: Uuid,
    backup: Uuid,
    chunks: &[Vec<u8>],
) -> Vec<ChunkId> {
    let cb = opl_types::backup_store::CHUNK_BYTES;
    let mut ix = FixedIndex::new(backup, cb * chunks.len() as u64, cb as u32, s.encrypted());
    for (i, raw) in chunks.iter().enumerate() {
        if is_zero(raw) {
            continue;
        }
        let c = s.encode_chunk(raw);
        if !s.has_chunk(&c.id).await.unwrap() {
            s.put_chunk(&c).await.unwrap();
        }
        ix.ids[i] = c.id;
    }
    let cfg = s
        .put_blob(vm, backup, CONFIG_BLOB, &config())
        .await
        .unwrap();
    let disk = s.put_index(vm, backup, "disk0", &ix).await.unwrap();
    let mut m = s.new_manifest(vm, backup, 1, vec![disk], vec![cfg]);
    s.put_manifest(&mut m).await.unwrap();
    ix.ids
}

fn data(tag: u8) -> Vec<u8> {
    (0..SMALL)
        .map(|i| tag.wrapping_mul(31).wrapping_add(i as u8))
        .collect()
}

#[test]
fn config_sample_parses() {
    // VmSpec 표본이 바뀌면 시험 도우미부터 알린다
    let _: ConfigBlob = serde_json::from_slice(&config()).unwrap();
}

#[tokio::test]
async fn verify_full_quarantines_and_heals() {
    for enc in [false, true] {
        let b = Arc::new(MemBackend::new());
        b.set_now(1_000_000);
        let mk = MasterKey::generate();
        let s = Store::init(b.clone(), Uuid::from_u128(1), enc.then_some(&mk), 1)
            .await
            .unwrap();
        let vm = Uuid::from_u128(5);
        let (b1, b2) = (Uuid::from_u128(11), Uuid::from_u128(12));
        let ids1 = write_snapshot_full(&s, vm, b1, &[data(1), data(2), vec![0; 8]]).await;
        let ids2 = write_snapshot_full(&s, vm, b2, &[data(2), data(3)]).await;
        let exp = vec![ExpectedDisk {
            name: "disk0".into(),
            virtual_bytes: 3 * opl_types::backup_store::CHUNK_BYTES,
        }];

        let mut v = Verifier::new(&s, VerifyMode::Full, true, 2_000_000);
        let r = v.verify_snapshot(vm, b1, Some(&exp)).await.unwrap();
        assert!(r.ok(), "{r:?}");
        assert_eq!(r.chunks_checked, 2);
        let r = v.verify_snapshot(vm, b2, None).await.unwrap();
        assert!(r.ok());
        assert_eq!(r.chunks_checked, 1, "shared chunk is checked once per run");
        assert_eq!(v.chunks_checked(), 3);

        // 공유 청크를 망가뜨림 → 두 백업 모두 실패, bad/로 옮겨짐(지우지 않음)
        let shared = ids1[1];
        assert_eq!(shared, ids2[0]);
        b.tamper(&chunk_key(&shared), |d| {
            let n = d.len();
            d[n - 1] ^= 1
        });
        let mut v = Verifier::new(&s, VerifyMode::Full, true, 2_000_000);
        let r1 = v.verify_snapshot(vm, b1, None).await.unwrap();
        assert_eq!(r1.bad_chunks, vec![shared]);
        let r2 = v.verify_snapshot(vm, b2, None).await.unwrap();
        assert_eq!(r2.bad_chunks, vec![shared], "found earlier in the run");
        assert_eq!(v.moved, 1);
        assert!(!s.has_chunk(&shared).await.unwrap());
        let bad = list_all(b.as_ref(), "bad/").await.unwrap();
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].key, format!("bad/{}.2000000", hex::encode(shared)));

        // 스스로 낫기: 다음 백업이 같은 내용을 다시 올린다
        write_snapshot_full(&s, vm, Uuid::from_u128(13), &[data(2)]).await;
        let mut v = Verifier::new(&s, VerifyMode::Full, true, 2_000_001);
        assert!(v.verify_snapshot(vm, b1, None).await.unwrap().ok());
        assert!(v.verify_snapshot(vm, b2, None).await.unwrap().ok());

        // 디스크가 리소스와 다름
        let other = vec![ExpectedDisk {
            name: "disk0".into(),
            virtual_bytes: 1,
        }];
        let r = v.verify_snapshot(vm, b1, Some(&other)).await.unwrap();
        assert!(r.error.unwrap().contains("do not match"));
        // 없는 스냅샷
        let r = v
            .verify_snapshot(vm, Uuid::from_u128(99), None)
            .await
            .unwrap();
        assert!(r.error.unwrap().contains("manifest"));
    }
}

#[tokio::test]
async fn verify_metadata_and_read_only() {
    let b = Arc::new(MemBackend::new());
    let s = Store::init(b.clone(), Uuid::from_u128(1), None, 1)
        .await
        .unwrap();
    let vm = Uuid::from_u128(5);
    let backup = Uuid::from_u128(6);
    let ids = write_snapshot_full(&s, vm, backup, &[data(1), data(2)]).await;
    // metadata: 내용이 바뀐 것은 못 보지만 없는 것·잘린 것은 본다
    b.tamper(&chunk_key(&ids[0]), |d| d[20] ^= 1);
    let mut v = Verifier::new(&s, VerifyMode::Metadata, true, 5);
    assert!(v.verify_snapshot(vm, backup, None).await.unwrap().ok());
    b.delete(&chunk_key(&ids[1])).await.unwrap();
    b.tamper(&chunk_key(&ids[0]), |d| d.truncate(4));
    let mut v = Verifier::new(&s, VerifyMode::Metadata, false, 5);
    let r = v.verify_snapshot(vm, backup, None).await.unwrap();
    assert_eq!(r.bad_chunks.len(), 2);
    // 읽기 전용: 옮기지 않는다
    assert_eq!(v.moved, 0);
    assert!(s.has_chunk(&ids[0]).await.unwrap());

    // 매니페스트 망가뜨림
    let mk = MasterKey::generate();
    let eb = Arc::new(MemBackend::new());
    let es = Store::init(eb.clone(), Uuid::from_u128(2), Some(&mk), 1)
        .await
        .unwrap();
    write_snapshot_full(&es, vm, backup, &[data(1)]).await;
    let key = format!("{}index.json", crate::format::snapshot_prefix(vm, backup));
    let raw = eb.get(&key).await.unwrap();
    let text = String::from_utf8(raw)
        .unwrap()
        .replace("\"created_at\": 1", "\"created_at\": 2");
    eb.put(&key, text.into_bytes()).await.unwrap();
    let mut v = Verifier::new(&es, VerifyMode::Full, true, 5);
    let r = v.verify_snapshot(vm, backup, None).await.unwrap();
    assert!(r.error.unwrap().contains("authentication failed"));
}

#[tokio::test]
async fn verify_stops_when_store_unreachable() {
    let f = FakeS3::start("bk", "AK", "SK").await;
    let backend = Arc::new(
        S3Backend::new(&f.target(None), 2)
            .unwrap()
            .with_retry_budget(std::time::Duration::from_millis(300)),
    );
    let s = Store::init(backend, Uuid::from_u128(1), None, 1)
        .await
        .unwrap();
    let vm = Uuid::from_u128(5);
    let backup = Uuid::from_u128(6);
    write_snapshot_full(&s, vm, backup, &[data(1)]).await;
    f.fail_next(1000, 503);
    let mut v = Verifier::new(&s, VerifyMode::Full, true, 5);
    assert!(matches!(
        v.verify_snapshot(vm, backup, None).await,
        Err(StoreError::Unavailable(_))
    ));
}

/// 한 번의 GC(동시 백업 없음)
async fn run_gc(s: &Store, run: u128, prev: Option<u64>) -> gc::SweepStats {
    let run = Uuid::from_u128(run);
    let start = gc::start(s, run).await.unwrap();
    let cap = gc::capacity(s, prev).await.unwrap();
    let mut m = gc::Marker::new(cap, gc::seed_for(run));
    m.mark(s).await.unwrap();
    m.mark(s).await.unwrap();
    let stats = gc::sweep(s, &m, &start).await.unwrap();
    gc::finish(s, &start).await.unwrap();
    stats
}

async fn gc_scenario(b: Arc<dyn Backend>, clock: &dyn Fn(u64)) {
    let s = Store::init(b.clone(), Uuid::from_u128(1), None, 1)
        .await
        .unwrap();
    let vm = Uuid::from_u128(5);
    let (a, bb) = (Uuid::from_u128(11), Uuid::from_u128(12));
    let ida = write_snapshot(&s, vm, a, &[data(1), data(2), vec![0; SMALL]]).await;
    let idb = write_snapshot(&s, vm, bb, &[data(2), data(3)]).await;
    clock(1000);
    // 아무것도 지우지 않았으면 아무것도 지우지 않는다
    let st = run_gc(&s, 1, None).await;
    assert_eq!((st.removed_chunks, st.chunks), (0, 3));
    // A를 지우면 A만 가진 청크가 지워진다
    s.delete_snapshot(vm, a).await.unwrap();
    clock(1000);
    let st = run_gc(&s, 2, Some(3)).await;
    assert_eq!(st.removed_chunks, 1);
    assert_eq!(st.chunks, 2);
    assert!(!s.has_chunk(&ida[0]).await.unwrap());
    assert!(s.has_chunk(&idb[0]).await.unwrap());
    assert!(s.has_chunk(&idb[1]).await.unwrap());
    // 표식은 남지 않는다
    assert!(list_all(b.as_ref(), "gc/").await.unwrap().is_empty());
    // 갓 올라온 청크(T0 − 5분 뒤)는 아무도 가리키지 않아도 남는다
    let young = s.encode_chunk(&data(9));
    s.put_chunk(&young).await.unwrap();
    let st = run_gc(&s, 3, None).await;
    assert_eq!((st.removed_chunks, st.kept_young), (0, 1));
    assert!(s.has_chunk(&young.id).await.unwrap());
    clock(1000);
    let st = run_gc(&s, 4, None).await;
    assert_eq!(st.removed_chunks, 1);
    assert!(!s.has_chunk(&young.id).await.unwrap());
}

#[tokio::test]
async fn gc_mem() {
    let b = Arc::new(MemBackend::new());
    b.set_now(1_000_000);
    b.set_page_size(2);
    let c = b.clone();
    gc_scenario(b, &move |d| c.advance(d)).await;
}

#[tokio::test]
async fn gc_fake_s3() {
    let f = FakeS3::start("bk", "AK", "SK").await;
    f.set_page_size(2);
    f.objects().set_now(1_000_000);
    let backend = Arc::new(S3Backend::new(&f.target(Some("p")), 4).unwrap());
    let objs = f.objects();
    gc_scenario(backend, &|d| objs.advance(d)).await;
    assert!(f.count("list") > 3, "paged");
}

#[tokio::test]
async fn gc_local_and_tmp_files() {
    let dir = tempfile::tempdir().unwrap();
    let b = Arc::new(LocalBackend::new(dir.path().join("st")).with_page_size(2));
    let s = Store::init(b.clone(), Uuid::from_u128(1), None, 1)
        .await
        .unwrap();
    let vm = Uuid::from_u128(5);
    write_snapshot(&s, vm, Uuid::from_u128(11), &[data(1)]).await;
    let orphan = s.encode_chunk(&data(2));
    s.put_chunk(&orphan).await.unwrap();
    // 하루 넘은 임시 파일과 오래된 고아 청크
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    let orphan_path = dir.path().join("st").join(chunk_key(&orphan.id));
    std::fs::File::options()
        .write(true)
        .open(&orphan_path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let tmp = dir.path().join("st/chunks/0000/x.tmp.abcd");
    std::fs::write(&tmp, b"partial").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&tmp)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let st = run_gc(&s, 1, None).await;
    assert_eq!((st.removed_chunks, st.removed_tmp, st.chunks), (1, 1, 1));
    assert!(!orphan_path.exists() && !tmp.exists());
}

#[tokio::test]
async fn gc_stops_on_unreadable_index() {
    let b = Arc::new(MemBackend::new());
    let s = Store::init(b.clone(), Uuid::from_u128(1), None, 1)
        .await
        .unwrap();
    let vm = Uuid::from_u128(5);
    let backup = Uuid::from_u128(11);
    write_snapshot(&s, vm, backup, &[data(1)]).await;
    b.tamper(
        &format!("{}disk0.fidx", crate::format::snapshot_prefix(vm, backup)),
        |d| d[70] ^= 1,
    );
    let mut m = gc::Marker::new(10, 1);
    assert!(matches!(m.mark(&s).await, Err(StoreError::Corrupt(_))));
}

// ---- GC 안전 모형(설계 §10.3) ----

#[derive(Clone, Debug)]
enum Step {
    /// 백업 시작(VM 번호, base를 쓸지)
    Begin(u8, bool),
    /// 진행 중 백업 하나가 청크 하나를 처리(내용 번호 — 0은 0 청크)
    Write(u8, u8),
    /// base 인덱스에서 id 옮기기
    CopyBase(u8),
    Commit(u8),
    Abort(u8),
    Delete(u8),
    Advance(u16),
    GcStart,
    GcWait,
    GcLock,
    GcSweep,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0u8..3, any::<bool>()).prop_map(|(v, b)| Step::Begin(v, b)),
        8 => (any::<u8>(), 0u8..10).prop_map(|(b, c)| Step::Write(b, c)),
        2 => any::<u8>().prop_map(Step::CopyBase),
        3 => any::<u8>().prop_map(Step::Commit),
        1 => any::<u8>().prop_map(Step::Abort),
        2 => any::<u8>().prop_map(Step::Delete),
        3 => (0u16..700).prop_map(Step::Advance),
        2 => Just(Step::GcStart),
        2 => Just(Step::GcWait),
        2 => Just(Step::GcLock),
        2 => Just(Step::GcSweep),
    ]
}

struct Pending {
    id: Uuid,
    vm: Uuid,
    base: Option<Uuid>,
    ids: Vec<ChunkId>,
    /// "있음 확인"으로 건너뛴 (id, 원본) — 소스는 커밋까지 열려 있다
    skipped: Vec<(ChunkId, Vec<u8>)>,
    start_epoch: u64,
    start_sweeping: bool,
}

#[derive(PartialEq)]
enum GcPhase {
    Marked,
    Waited,
    Locked,
}

struct GcRun {
    start: gc::GcStart,
    marker: gc::Marker,
    w: BTreeSet<Uuid>,
    phase: GcPhase,
}

/// Raft 쪽 상태의 모형
#[derive(Default)]
struct Raft {
    committing: BTreeSet<Uuid>,
    sweeping: bool,
    epoch: u64,
}

struct Model {
    b: Arc<MemBackend>,
    s: Store,
    raft: Raft,
    pending: Vec<Pending>,
    /// 커밋된 스냅샷: 백업 → (vm, 인덱스 id)
    done: BTreeMap<Uuid, (Uuid, Vec<ChunkId>)>,
    gc: Option<GcRun>,
    next: u128,
}

fn raw_for(c: u8) -> Vec<u8> {
    if c == 0 { vec![0; SMALL] } else { data(c) }
}

impl Model {
    fn uuid(&mut self) -> Uuid {
        self.next += 1;
        Uuid::from_u128(self.next)
    }

    fn pick(&self, n: u8) -> Option<usize> {
        (!self.pending.is_empty()).then(|| n as usize % self.pending.len())
    }

    async fn apply(&mut self, st: &Step) {
        match *st {
            Step::Begin(v, use_base) => {
                let vm = Uuid::from_u128(1000 + v as u128);
                let base = if use_base {
                    self.done
                        .iter()
                        .find(|(_, (bvm, _))| *bvm == vm)
                        .map(|(b, _)| *b)
                } else {
                    None
                };
                let id = self.uuid();
                self.pending.push(Pending {
                    id,
                    vm,
                    base,
                    ids: Vec::new(),
                    skipped: Vec::new(),
                    start_epoch: self.raft.epoch,
                    start_sweeping: self.raft.sweeping,
                });
            }
            Step::Write(n, c) => {
                let Some(i) = self.pick(n) else { return };
                let raw = raw_for(c);
                if is_zero(&raw) {
                    self.pending[i].ids.push(ZERO_CHUNK);
                    return;
                }
                let ch = self.s.encode_chunk(&raw);
                if self.s.has_chunk(&ch.id).await.unwrap() {
                    self.pending[i].skipped.push((ch.id, raw));
                } else {
                    self.s.put_chunk(&ch).await.unwrap();
                }
                self.pending[i].ids.push(ch.id);
            }
            Step::CopyBase(n) => {
                let Some(i) = self.pick(n) else { return };
                let Some(base) = self.pending[i].base else {
                    return;
                };
                let (_, ids) = &self.done[&base];
                if let Some(id) = ids
                    .get(self.pending[i].ids.len() % ids.len().max(1))
                    .copied()
                {
                    self.pending[i].ids.push(id);
                }
            }
            Step::Commit(n) => {
                let Some(i) = self.pick(n) else { return };
                if self.raft.sweeping {
                    return; // 쓸기 중에는 기다린다
                }
                let p = self.pending.remove(i);
                self.raft.committing.insert(p.id);
                // 그 사이 쓸기가 있었거나 시작할 때 쓸기 중이었으면 건너뛴 청크를 다시 확인해 올린다
                if p.start_sweeping || p.start_epoch != self.raft.epoch {
                    let ids: Vec<ChunkId> = p.skipped.iter().map(|(id, _)| *id).collect();
                    let missing = self.s.missing_chunks(&ids).await.unwrap();
                    for (id, raw) in &p.skipped {
                        if missing.contains(id) {
                            let ch = self.s.encode_chunk(raw);
                            self.s.put_chunk(&ch).await.unwrap();
                        }
                    }
                }
                let ix = FixedIndex {
                    backup: p.id,
                    virtual_bytes: (p.ids.len() * SMALL) as u64,
                    chunk_bytes: SMALL as u32,
                    keyed: false,
                    ids: p.ids.clone(),
                };
                let disk = self.s.put_index(p.vm, p.id, "disk0", &ix).await.unwrap();
                let mut m = self.s.new_manifest(p.vm, p.id, 1, vec![disk], vec![]);
                self.s.put_manifest(&mut m).await.unwrap();
                self.raft.committing.remove(&p.id);
                self.done.insert(p.id, (p.vm, p.ids));
            }
            Step::Abort(n) => {
                if let Some(i) = self.pick(n) {
                    self.pending.remove(i);
                }
            }
            Step::Delete(n) => {
                if self.done.is_empty() {
                    return;
                }
                let k = *self.done.keys().nth(n as usize % self.done.len()).unwrap();
                // base는 자식이 pending인 동안 지우지 못한다(409 backup_in_use)
                if self.pending.iter().any(|p| p.base == Some(k)) {
                    return;
                }
                let (vm, _) = self.done.remove(&k).unwrap();
                self.s.delete_snapshot(vm, k).await.unwrap();
            }
            Step::Advance(d) => self.b.advance(d as u64),
            Step::GcStart => {
                if self.gc.is_some() {
                    return;
                }
                let run = self.uuid();
                let start = gc::start(&self.s, run).await.unwrap();
                // L 바로 뒤의 pending이 W
                let w = self.pending.iter().map(|p| p.id).collect();
                let cap = gc::capacity(&self.s, None).await.unwrap();
                let mut marker = gc::Marker::new(cap, gc::seed_for(run));
                marker.mark(&self.s).await.unwrap();
                self.gc = Some(GcRun {
                    start,
                    marker,
                    w,
                    phase: GcPhase::Marked,
                });
            }
            Step::GcWait => {
                let pending: BTreeSet<Uuid> = self.pending.iter().map(|p| p.id).collect();
                if let Some(g) = &mut self.gc
                    && g.phase == GcPhase::Marked
                    && g.w.is_disjoint(&pending)
                {
                    g.marker.mark(&self.s).await.unwrap();
                    g.phase = GcPhase::Waited;
                }
            }
            Step::GcLock => {
                if let Some(g) = &mut self.gc
                    && g.phase == GcPhase::Waited
                    && self.raft.committing.is_empty()
                {
                    self.raft.sweeping = true;
                    self.raft.epoch += 1;
                    g.marker.mark(&self.s).await.unwrap(); // 늦은 표시
                    g.phase = GcPhase::Locked;
                }
            }
            Step::GcSweep => {
                if self.gc.as_ref().is_some_and(|g| g.phase == GcPhase::Locked) {
                    let g = self.gc.take().unwrap();
                    gc::sweep(&self.s, &g.marker, &g.start).await.unwrap();
                    gc::finish(&self.s, &g.start).await.unwrap();
                    self.raft.sweeping = false;
                }
            }
        }
    }

    /// 끝난 인덱스가 가리키는 청크는 늘 있다
    async fn check(&self) -> Result<(), String> {
        for (b, (_, ids)) in &self.done {
            for id in ids.iter().filter(|i| **i != ZERO_CHUNK) {
                if !self.s.has_chunk(id).await.unwrap() {
                    return Err(format!("backup {b} lost chunk {}", hex::encode(id)));
                }
            }
        }
        Ok(())
    }
}

async fn run_model(steps: Vec<Step>) -> Result<(), String> {
    let b = Arc::new(MemBackend::new());
    b.set_now(1_000_000);
    let s = Store::init(b.clone(), Uuid::from_u128(1), None, 1)
        .await
        .unwrap();
    let mut m = Model {
        b,
        s,
        raft: Raft::default(),
        pending: Vec::new(),
        done: BTreeMap::new(),
        gc: None,
        next: 10,
    };
    for st in &steps {
        m.apply(st).await;
        m.check().await.map_err(|e| format!("after {st:?}: {e}"))?;
    }
    // 끝: 진행 중 백업은 실패로 치우고, 걸린 GC를 마친 뒤 시간을 넘겨 GC 한 번 — 아무도 가리키지 않는 청크가 없다
    m.pending.clear();
    for st in [Step::GcWait, Step::GcLock, Step::GcSweep] {
        m.apply(&st).await;
    }
    m.b.advance(1000);
    for st in [Step::GcStart, Step::GcWait, Step::GcLock, Step::GcSweep] {
        m.apply(&st).await;
    }
    m.check().await?;
    let referenced: BTreeSet<ChunkId> = m
        .done
        .values()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    for o in list_all(m.b.as_ref(), "chunks/").await.unwrap() {
        let id = crate::format::parse_chunk_key(&o.key).unwrap();
        if !referenced.contains(&id) {
            return Err(format!("garbage chunk {} survived", o.key));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1500, ..ProptestConfig::default() })]

    #[test]
    fn gc_never_loses_committed_chunks(steps in proptest::collection::vec(step(), 1..90)) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let r = rt.block_on(Box::pin(run_model(steps)));
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}

/// 모형이 실제로 위험한 경우를 지나는지 — 커밋 확인을 빼면 잃는 순서가 있다
#[tokio::test]
async fn model_without_recheck_would_lose_chunks() {
    // 청크 X를 가진 백업 A를 지운 뒤, 새 백업 B가 X를 "있음"으로 건너뛰고, GC가 X를 지운 뒤 B가 커밋한다.
    let b = Arc::new(MemBackend::new());
    b.set_now(1_000_000);
    let s = Store::init(b.clone(), Uuid::from_u128(1), None, 1)
        .await
        .unwrap();
    let vm = Uuid::from_u128(5);
    let ids = write_snapshot(&s, vm, Uuid::from_u128(11), &[data(4)]).await;
    s.delete_snapshot(vm, Uuid::from_u128(11)).await.unwrap();
    b.advance(1000);
    let x = s.encode_chunk(&{
        let mut d = data(4);
        d.resize(SMALL, 0);
        d
    });
    assert_eq!(x.id, ids[0]);
    assert!(s.has_chunk(&x.id).await.unwrap(), "B sees X and skips it");
    run_gc(&s, 1, None).await;
    assert!(
        !s.has_chunk(&x.id).await.unwrap(),
        "GC removed the unreferenced X"
    );
    // 그래서 B는 커밋할 때(sweep_epoch가 바뀜) X를 다시 올려야 한다
    assert_eq!(s.missing_chunks(&[x.id]).await.unwrap(), vec![x.id]);
}
