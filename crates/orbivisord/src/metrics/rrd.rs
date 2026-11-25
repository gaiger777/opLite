//! 지표 기록 파일(계획 60 RRD). 시리즈(노드·VM·풀)마다 고정 크기 파일 하나에 네 층(5초·1분·30분·6시간)을 둔다.
//!
//! - 배치: 머리 4 KiB(512 B 사본 A/B) 뒤에 층마다 4 KiB에 맞춘 영역이 온다. 리틀 엔디언이고 모르는 값은 NaN이다.
//!   `Raw5s` 칸은 칸마다 값 하나, 다른 층 칸은 `[AVG f0..fn][MAX f0..fn][MIN f0..fn]`이다.
//! - 칸 주소: 칸 `b = floor(t / step)`(유닉스 기준)는 자리 `b mod rows`에 들어간다. 머리의 층마다 `last`(마지막으로
//!   쓴 칸)가 있고, 칸 `b`는 `last − rows < b ≤ last`일 때만 읽는다. 가리키지 않은 자리의 쓰레기는 다음 쓰기가 덮는다.
//! - 쓰기: 데이터를 제자리에 쓰고([`Rrd::append`]·[`Rrd::close`]) → [`Rrd::sync_data`] → [`Rrd::commit`](머리를
//!   번갈아 쓰고 다시 sync). 머리는 데이터가 디스크에 닿은 뒤에만 그 칸을 가리키므로 끊겨도 잃는 것은 마지막 기록
//!   주기(1분)다. 다만 데이터는 제자리에 쓰므로 새 칸이 옛 머리가 아직 가장 오래된 칸으로 보는 자리를 덮는다. 그 사이에
//!   끊기면 다시 연 뒤 그 자리(층의 가장 오래된 끝, `Raw5s`면 약 1시간 전, `Hour6`이면 약 1년 전)에 새 값이 보이다가
//!   다음 쓰기가 NaN으로 덮는다. 보이는 기간을 줄여 막는 것보다 싸서 그대로 둔다(리뷰 1 #10, 겉모습만의 문제).
//! - 합치기: `Min1`은 그 분의 `Raw5s` 12칸, `Min30`·`Hour6`은 `Min1` 칸을 칸이 닫힐 때 다시 읽어 만든다. 따로
//!   누산기가 없어 다시 열어도 닫히지 않은 칸을 이어서 만든다.
//! - 시각 `t`는 인자로 받는다(시험은 벽시계를 쓰지 않는다).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use orbivisor_types::metrics::{MetricField, NODE_FIELDS, POOL_FIELDS, TIERS, Tier, VM_FIELDS};
use orbivisor_types::metrics_math::{Agg, bucket_window, consolidate, consolidate_aggs};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 머리 사본의 첫 8바이트
pub const MAGIC: [u8; 8] = *b"OPLRRD\0\0";
/// 이 빌드가 쓰는 형식 버전. 칸 배치를 바꾸면 올리고 [`Rrd::open`]이 옮긴다.
pub const VERSION: u16 = 1;
/// 머리 영역(사본 둘과 남는 자리)
pub const HEADER_BYTES: u64 = 4096;
/// 머리 사본 하나(A는 0, B는 512)
pub const HEADER_COPY_BYTES: usize = 512;
/// 층 영역은 이 크기의 배수다
pub const PAGE: u64 = 4096;
/// 한 파일이 담을 수 있는 칸(필드) 수
pub const MAX_FIELDS: usize = 64;
/// 시계가 이만큼(ms) 넘게 뒤로 가면 앞선 시계로 쓴 칸을 버리고 `last`를 되돌린다. 덜 가면 따라잡을 때까지 쓰지 않는다.
pub const REWIND_MS: i64 = 3_600_000;
/// 파일 확장자
pub const EXT: &str = "rrd";

const NO_LAST: i64 = i64::MIN;
const SUM_BYTES: usize = 8;
const FIELDS_AT: usize = 24;
const TIERS_AT: usize = FIELDS_AT + MAX_FIELDS;
const TIER_ENTRY: usize = 32;

/// 시리즈 종류. 머리에 숫자로 남는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesKind {
    Node,
    Vm,
    Pool,
}

impl SeriesKind {
    /// 이 빌드에서 그 종류의 칸 목록(파일 칸 순서)
    pub fn fields(self) -> &'static [MetricField] {
        match self {
            Self::Node => &NODE_FIELDS,
            Self::Vm => &VM_FIELDS,
            Self::Pool => &POOL_FIELDS,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Node => 1,
            Self::Vm => 2,
            Self::Pool => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Node),
            2 => Some(Self::Vm),
            3 => Some(Self::Pool),
            _ => None,
        }
    }
}

/// 머리에 남는 칸 id. 한 번 정한 번호는 바꾸지 않는다(새 칸은 새 번호).
fn field_id(f: MetricField) -> u8 {
    match f {
        MetricField::Cpu => 1,
        MetricField::MemUsed => 2,
        MetricField::MemTotal => 3,
        MetricField::DiskRead => 4,
        MetricField::DiskWrite => 5,
        MetricField::NetRx => 6,
        MetricField::NetTx => 7,
        MetricField::FsUsed => 8,
        MetricField::FsTotal => 9,
        MetricField::MemRss => 10,
        MetricField::DiskReadIops => 11,
        MetricField::DiskWriteIops => 12,
        MetricField::PoolUsed => 13,
        MetricField::PoolTotal => 14,
    }
}

fn field_from_id(id: u8) -> Option<MetricField> {
    const ALL: [MetricField; 14] = [
        MetricField::Cpu,
        MetricField::MemUsed,
        MetricField::MemTotal,
        MetricField::DiskRead,
        MetricField::DiskWrite,
        MetricField::NetRx,
        MetricField::NetTx,
        MetricField::FsUsed,
        MetricField::FsTotal,
        MetricField::MemRss,
        MetricField::DiskReadIops,
        MetricField::DiskWriteIops,
        MetricField::PoolUsed,
        MetricField::PoolTotal,
    ];
    ALL.into_iter().find(|f| field_id(*f) == id)
}

fn tier_code(tier: Tier) -> u8 {
    match tier {
        Tier::Raw5s => 1,
        Tier::Min1 => 2,
        Tier::Min30 => 3,
        Tier::Hour6 => 4,
    }
}

/// `{dir}/node.rrd` (`dir`은 `{data_dir}/metrics`)
pub fn node_path(dir: &Path) -> PathBuf {
    dir.join(format!("node.{EXT}"))
}

/// `{dir}/vm/{id}.rrd`
pub fn vm_path(dir: &Path, vm: Uuid) -> PathBuf {
    dir.join("vm").join(format!("{vm}.{EXT}"))
}

/// `{dir}/pool/{pool_id}.rrd`(풀 id라서 이름을 바꿔도 이어진다)
pub fn pool_path(dir: &Path, pool: Uuid) -> PathBuf {
    dir.join("pool").join(format!("{pool}.{EXT}"))
}

/// 층 칸 하나의 f32 수(`Raw5s`는 값 하나, 나머지는 AVG·MAX·MIN)
fn cell_floats(tier: Tier, nfields: usize) -> usize {
    match tier {
        Tier::Raw5s => nfields,
        _ => nfields * 3,
    }
}

/// 층마다의 오프셋과 파일 전체 크기
fn layout(nfields: usize) -> ([u64; 4], u64) {
    let mut offsets = [0u64; 4];
    let mut at = HEADER_BYTES;
    for (i, tier) in TIERS.into_iter().enumerate() {
        offsets[i] = at;
        let bytes = u64::from(tier.rows()) * (cell_floats(tier, nfields) as u64) * 4;
        at += bytes.div_ceil(PAGE) * PAGE;
    }
    (offsets, at)
}

/// 칸 `nfields`개인 파일의 크기(바이트). VM(8칸)은 336 KiB다.
#[cfg(test)]
pub fn file_size(nfields: usize) -> u64 {
    layout(nfields).1
}

#[derive(Debug, Clone, PartialEq)]
struct Header {
    version: u16,
    kind: SeriesKind,
    seq: u64,
    fields: Vec<MetricField>,
    offsets: [u64; 4],
    /// 층마다 마지막으로 쓴 칸(`TIERS` 순서)
    last: [Option<i64>; 4],
}

impl Header {
    fn new(kind: SeriesKind, fields: &[MetricField]) -> Self {
        Header {
            version: VERSION,
            kind,
            seq: 0,
            fields: fields.to_vec(),
            offsets: layout(fields.len()).0,
            last: [None; 4],
        }
    }

    fn encode(&self) -> [u8; HEADER_COPY_BYTES] {
        let mut b = [0u8; HEADER_COPY_BYTES];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..10].copy_from_slice(&self.version.to_le_bytes());
        b[10] = self.kind.code();
        b[11] = self.fields.len() as u8;
        b[16..24].copy_from_slice(&self.seq.to_le_bytes());
        for (i, f) in self.fields.iter().enumerate() {
            b[FIELDS_AT + i] = field_id(*f);
        }
        for (i, tier) in TIERS.into_iter().enumerate() {
            let e = TIERS_AT + i * TIER_ENTRY;
            b[e] = tier_code(tier);
            b[e + 4..e + 8].copy_from_slice(&tier.rows().to_le_bytes());
            b[e + 8..e + 16].copy_from_slice(&tier.step_ms().to_le_bytes());
            b[e + 16..e + 24].copy_from_slice(&self.offsets[i].to_le_bytes());
            b[e + 24..e + 32].copy_from_slice(&self.last[i].unwrap_or(NO_LAST).to_le_bytes());
        }
        let sum = Sha256::digest(&b[..HEADER_COPY_BYTES - SUM_BYTES]);
        b[HEADER_COPY_BYTES - SUM_BYTES..].copy_from_slice(&sum[..SUM_BYTES]);
        b
    }
}

/// 머리 사본 하나를 읽은 결과
#[derive(Debug, PartialEq)]
enum Copy {
    Valid(Header),
    /// 이 빌드보다 새 형식(검사합은 보지 않는다 — 새 형식의 머리 배치를 모른다)
    Newer(u16),
    Invalid,
}

fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b.try_into().unwrap())
}
fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().unwrap())
}
fn le_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().unwrap())
}
fn le_i64(b: &[u8]) -> i64 {
    i64::from_le_bytes(b.try_into().unwrap())
}

fn decode(b: &[u8]) -> Copy {
    if b.len() < HEADER_COPY_BYTES || b[0..8] != MAGIC {
        return Copy::Invalid;
    }
    let version = le_u16(&b[8..10]);
    if version > VERSION {
        return Copy::Newer(version);
    }
    let sum = Sha256::digest(&b[..HEADER_COPY_BYTES - SUM_BYTES]);
    if version == 0 || b[HEADER_COPY_BYTES - SUM_BYTES..HEADER_COPY_BYTES] != sum[..SUM_BYTES] {
        return Copy::Invalid;
    }
    let Some(kind) = SeriesKind::from_code(b[10]) else {
        return Copy::Invalid;
    };
    let n = usize::from(b[11]);
    if n == 0 || n > MAX_FIELDS {
        return Copy::Invalid;
    }
    let mut fields = Vec::with_capacity(n);
    for &id in &b[FIELDS_AT..FIELDS_AT + n] {
        match field_from_id(id) {
            Some(f) if !fields.contains(&f) => fields.push(f),
            _ => return Copy::Invalid,
        }
    }
    let (want_offsets, _) = layout(n);
    let mut offsets = [0u64; 4];
    let mut last = [None; 4];
    for (i, tier) in TIERS.into_iter().enumerate() {
        let e = TIERS_AT + i * TIER_ENTRY;
        if b[e] != tier_code(tier)
            || le_u32(&b[e + 4..e + 8]) != tier.rows()
            || le_i64(&b[e + 8..e + 16]) != tier.step_ms()
            || le_u64(&b[e + 16..e + 24]) != want_offsets[i]
        {
            return Copy::Invalid;
        }
        offsets[i] = want_offsets[i];
        let l = le_i64(&b[e + 24..e + 32]);
        last[i] = (l != NO_LAST).then_some(l);
    }
    Copy::Valid(Header {
        version,
        kind,
        seq: le_u64(&b[16..24]),
        fields,
        offsets,
        last,
    })
}

/// 두 사본 중 쓸 것: 새 형식이 하나라도 있으면 새 형식, 아니면 검사합이 맞고 `seq`가 큰 것
fn pick(head: &[u8]) -> Copy {
    let a = decode(&head[..HEADER_COPY_BYTES.min(head.len())]);
    let b = if head.len() >= 2 * HEADER_COPY_BYTES {
        decode(&head[HEADER_COPY_BYTES..2 * HEADER_COPY_BYTES])
    } else {
        Copy::Invalid
    };
    match (a, b) {
        (Copy::Newer(v), _) | (_, Copy::Newer(v)) => Copy::Newer(v),
        (Copy::Valid(x), Copy::Valid(y)) => Copy::Valid(if y.seq > x.seq { y } else { x }),
        (Copy::Valid(x), Copy::Invalid) | (Copy::Invalid, Copy::Valid(x)) => Copy::Valid(x),
        (Copy::Invalid, Copy::Invalid) => Copy::Invalid,
    }
}

/// 위치를 주고 읽는 곳(열린 파일이나 받은 파일 바이트)
trait ReadAt {
    fn read_at_exact(&self, buf: &mut [u8], off: u64) -> io::Result<()>;
}

impl ReadAt for File {
    fn read_at_exact(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
        self.read_exact_at(buf, off)
    }
}

impl ReadAt for [u8] {
    fn read_at_exact(&self, buf: &mut [u8], off: u64) -> io::Result<()> {
        let start = usize::try_from(off).map_err(|_| short())?;
        let src = self.get(start..start + buf.len()).ok_or_else(short)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "기록 파일이 짧다")
}

fn slot(bucket: i64, rows: i64) -> u64 {
    bucket.rem_euclid(rows) as u64
}

/// 칸 `[first, first + count)`를 자리 순서의 연속 구간으로 나눈다: `(첫 칸, 칸 수)`
fn runs(first: i64, count: usize, rows: i64) -> impl Iterator<Item = (i64, usize)> {
    let mut b = first;
    let end = first + count as i64;
    std::iter::from_fn(move || {
        if b >= end {
            return None;
        }
        let room = rows - b.rem_euclid(rows);
        let n = room.min(end - b);
        let run = (b, n as usize);
        b += n;
        Some(run)
    })
}

/// 층 `i`의 칸 `[first, first + count)`를 읽는다(칸마다 `cell_floats`개, 보이지 않는 칸은 NaN).
fn read_cells(
    src: &(impl ReadAt + ?Sized),
    hdr: &Header,
    i: usize,
    first: i64,
    count: usize,
) -> io::Result<Vec<f32>> {
    let tier = TIERS[i];
    let per = cell_floats(tier, hdr.fields.len());
    let mut out = vec![f32::NAN; count * per];
    let Some(last) = hdr.last[i] else {
        return Ok(out);
    };
    let rows = i64::from(tier.rows());
    let lo = first.max(last - rows + 1);
    let hi = (first + count as i64 - 1).min(last);
    if lo > hi {
        return Ok(out);
    }
    let cell = per * 4;
    for (b, n) in runs(lo, (hi - lo + 1) as usize, rows) {
        let mut buf = vec![0u8; n * cell];
        src.read_at_exact(&mut buf, hdr.offsets[i] + slot(b, rows) * cell as u64)?;
        let at = (b - first) as usize * per;
        // `chunks_exact(4)`는 새 clippy가 `as_chunks`를 권하고 옛 툴체인에는 `as_chunks`가 없어 인덱스로 돈다.
        for k in 0..buf.len() / 4 {
            let b: [u8; 4] = buf[k * 4..k * 4 + 4].try_into().unwrap();
            out[at + k] = f32::from_le_bytes(b);
        }
    }
    Ok(out)
}

/// 열면서 있었던 일(쓰는 쪽이 로그를 남긴다)
#[derive(Debug, Clone, PartialEq)]
pub enum Opened {
    /// 있던 파일을 그대로 열었다
    Existing,
    /// 없어서 새로 만들었다
    Created,
    /// 칸 목록이 달라(옛 형식) 새 배치로 옮겼다. 같은 칸은 남고 새 칸은 NaN이다.
    Migrated { from: Vec<MetricField> },
    /// 두 머리 사본이 다 깨졌거나 크기·종류가 맞지 않아 옆으로 옮기고 새로 만들었다
    Corrupt { moved_to: PathBuf },
    /// 이 빌드보다 새 형식(내려 깐 opld)이라 옆으로 옮기고 새로 만들었다
    Newer { version: u16, moved_to: PathBuf },
}

/// [`Rrd::append`]의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Append {
    Written,
    /// 칸이 `last` 이하이고 1시간 미만 뒤라서 쓰지 않았다(시계가 따라잡으면 이어 쓴다)
    Behind,
    /// 1시간 넘게 뒤로 가서 그보다 뒤 칸을 지우고 `last`를 되돌린 뒤 썼다
    Rewound,
}

/// 한 층의 칸들을 칸(필드)마다 읽은 것
#[derive(Debug, Clone, PartialEq)]
pub struct TierRead {
    pub tier: Tier,
    /// 첫 칸 번호(시작 시각 = `first × step`)
    pub first: i64,
    pub fields: Vec<MetricField>,
    /// `columns[f][k]`는 칸 `fields[f]`의 칸 `first + k`. `Raw5s`는 AVG=MAX=MIN이고, 모르면 NaN이다.
    pub columns: Vec<Vec<Agg>>,
}

impl TierRead {
    pub fn start_ms(&self) -> i64 {
        self.first * self.tier.step_ms()
    }

    pub fn column(&self, field: MetricField) -> Option<&[Agg]> {
        let i = self.fields.iter().position(|f| *f == field)?;
        Some(&self.columns[i])
    }
}

/// 열린 기록 파일 하나
#[derive(Debug)]
pub struct Rrd {
    path: PathBuf,
    file: File,
    /// 쓰는 중인 머리(마지막 [`Rrd::commit`] 뒤 바뀌었으면 `dirty`)
    hdr: Header,
    dirty: bool,
    /// [`Rrd::unlink`]로 지웠다(더 쓰면 사라지는 파일에 쓰는 것이다).
    removed: bool,
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn ensure_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir),
        _ => Ok(()),
    }
}

fn sync_dir(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => File::open(dir)?.sync_all(),
        _ => Ok(()),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// `name`이 이 형식의 파일(`{stem}.rrd`, `stem_ok(stem)`) 옆으로 옮긴 파일이나 남은 임시 파일인지: `.rrd` 뒤에
/// `.tmp`, `.corrupt`, `.corrupt.N`, `.vN`, `.vN.M`이 붙은 이름이다.
pub fn is_aside(name: &str, stem_ok: impl Fn(&str) -> bool) -> bool {
    let Some((stem, rest)) = name.split_once(&format!(".{EXT}.")) else {
        return false;
    };
    if !stem_ok(stem) {
        return false;
    }
    let num = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if rest == "tmp" {
        return true;
    }
    let (head, tail) = match rest.split_once('.') {
        Some((h, t)) => (h, Some(t)),
        None => (rest, None),
    };
    let head_ok = head == "corrupt" || head.strip_prefix('v').is_some_and(num);
    head_ok && tail.is_none_or(num)
}

/// 지우지 않고 옆으로 옮긴다. 같은 이름이 있으면 `.1`, `.2`…를 붙인다.
fn move_aside(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let mut to = with_suffix(path, suffix);
    let mut n = 1;
    while to.exists() {
        to = with_suffix(path, &format!("{suffix}.{n}"));
        n += 1;
    }
    std::fs::rename(path, &to)?;
    Ok(to)
}

/// `hdr`(seq는 덮어씀)와 데이터로 새 파일을 `path.tmp`에 만들고 fsync → rename → 디렉터리 fsync 한다.
/// 머리는 두 사본(seq 0·1)을 같게 쓴다.
fn write_new(path: &Path, hdr: &mut Header, data: &[u8]) -> io::Result<File> {
    let (_, total) = layout(hdr.fields.len());
    debug_assert_eq!(data.len() as u64, total);
    let mut buf = data.to_vec();
    for seq in 0..2u64 {
        hdr.seq = seq;
        let at = seq as usize * HEADER_COPY_BYTES;
        buf[at..at + HEADER_COPY_BYTES].copy_from_slice(&hdr.encode());
    }
    let tmp = with_suffix(path, ".tmp");
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    sync_dir(path)?;
    OpenOptions::new().read(true).write(true).open(path)
}

/// 머리 자리는 0, 층 영역은 NaN인 데이터
fn blank_data(nfields: usize) -> Vec<u8> {
    let (offsets, total) = layout(nfields);
    let mut buf = vec![0u8; total as usize];
    let nan = f32::NAN.to_le_bytes();
    for (i, tier) in TIERS.into_iter().enumerate() {
        let len = tier.rows() as usize * cell_floats(tier, nfields);
        let start = offsets[i] as usize;
        for k in 0..len {
            buf[start + k * 4..start + k * 4 + 4].copy_from_slice(&nan);
        }
    }
    buf
}

impl Rrd {
    /// 그 종류의 지금 칸 목록으로 연다. 없으면 만들고(디렉터리 0700, 파일 0600), 옛 칸 목록이면 옮기고, 깨졌거나
    /// 더 새 형식이면 옆으로 옮긴 뒤 새로 만든다.
    pub fn open(path: &Path, kind: SeriesKind) -> io::Result<(Rrd, Opened)> {
        Self::open_fields(path, kind, kind.fields())
    }

    /// [`Rrd::open`]과 같되 칸 목록을 준다(시험이 칸을 더한 형식을 흉내 낸다).
    pub(crate) fn open_fields(
        path: &Path,
        kind: SeriesKind,
        fields: &[MetricField],
    ) -> io::Result<(Rrd, Opened)> {
        assert!(!fields.is_empty() && fields.len() <= MAX_FIELDS);
        ensure_parent(path)?;
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok((Self::create(path, kind, fields)?, Opened::Created));
            }
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        let mut head = vec![0u8; (2 * HEADER_COPY_BYTES).min(len as usize)];
        file.read_exact_at(&mut head, 0)?;
        let hdr = match pick(&head) {
            Copy::Newer(version) => {
                drop(file);
                let moved_to = move_aside(path, &format!(".v{version}"))?;
                let rrd = Self::create(path, kind, fields)?;
                return Ok((rrd, Opened::Newer { version, moved_to }));
            }
            Copy::Valid(h) if h.kind == kind && len >= layout(h.fields.len()).1 => h,
            _ => {
                drop(file);
                let moved_to = move_aside(path, ".corrupt")?;
                let rrd = Self::create(path, kind, fields)?;
                return Ok((rrd, Opened::Corrupt { moved_to }));
            }
        };
        if hdr.fields == fields && hdr.version == VERSION {
            let rrd = Rrd {
                path: path.to_path_buf(),
                file,
                hdr,
                dirty: false,
                removed: false,
            };
            return Ok((rrd, Opened::Existing));
        }
        let from = hdr.fields.clone();
        let rrd = Self::migrate(path, &file, &hdr, fields)?;
        Ok((rrd, Opened::Migrated { from }))
    }

    fn create(path: &Path, kind: SeriesKind, fields: &[MetricField]) -> io::Result<Rrd> {
        let mut hdr = Header::new(kind, fields);
        let file = write_new(path, &mut hdr, &blank_data(fields.len()))?;
        Ok(Rrd {
            path: path.to_path_buf(),
            file,
            hdr,
            dirty: false,
            removed: false,
        })
    }

    /// 옛 배치 `old`를 칸 목록 `fields`의 지금 배치로 옮긴다. 층·자리는 같으므로 자리마다 같은 칸 id를 복사하고
    /// 새 칸은 NaN으로 둔다. `last`는 그대로다.
    fn migrate(path: &Path, file: &File, old: &Header, fields: &[MetricField]) -> io::Result<Rrd> {
        let mut hdr = Header::new(old.kind, fields);
        hdr.last = old.last;
        let mut data = blank_data(fields.len());
        let map: Vec<Option<usize>> = fields
            .iter()
            .map(|f| old.fields.iter().position(|o| o == f))
            .collect();
        let (on, nn) = (old.fields.len(), fields.len());
        for (i, tier) in TIERS.into_iter().enumerate() {
            let rows = tier.rows() as usize;
            let mut src = vec![0u8; rows * cell_floats(tier, on) * 4];
            file.read_exact_at(&mut src, old.offsets[i])?;
            let cfs = cell_floats(tier, 1);
            for r in 0..rows {
                for c in 0..cfs {
                    for (j, m) in map.iter().enumerate() {
                        let Some(o) = m else { continue };
                        let s = (r * cfs * on + c * on + o) * 4;
                        let d = hdr.offsets[i] as usize + (r * cfs * nn + c * nn + j) * 4;
                        data[d..d + 4].copy_from_slice(&src[s..s + 4]);
                    }
                }
            }
        }
        let file = write_new(path, &mut hdr, &data)?;
        Ok(Rrd {
            path: path.to_path_buf(),
            file,
            hdr,
            dirty: false,
            removed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> SeriesKind {
        self.hdr.kind
    }

    pub fn fields(&self) -> &[MetricField] {
        &self.hdr.fields
    }

    /// 층의 마지막으로 쓴 칸(쓴 적이 없으면 `None`)
    pub fn last(&self, tier: Tier) -> Option<i64> {
        self.hdr.last[tier_index(tier)]
    }

    /// 마지막으로 commit 한 머리의 차례(commit 할 때마다 1씩 는다)
    pub fn seq(&self) -> u64 {
        self.hdr.seq
    }

    /// 파일을 지운다(없으면 그만). 그 뒤 이 핸들은 [`Rrd::is_removed`]다.
    pub fn unlink(&mut self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => {
                self.removed = true;
                Ok(())
            }
        }
    }

    pub fn is_removed(&self) -> bool {
        self.removed
    }

    /// 마지막 [`Rrd::commit`] 뒤 바뀐 것이 있는지
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 5초 점 하나(`values`는 [`Rrd::fields`] 순서, `None`·NaN은 모름)를 `Raw5s` 칸 `floor(t / 5 s)`에 쓰고, `t`까지
    /// 닫힌 거친 층 칸을 만든다. 점은 시각 순으로 준다. 데이터만 쓰고 머리는 [`Rrd::commit`]이 쓴다.
    ///
    /// - 앞으로 뛰면 건너뛴 칸은 NaN이다(층 기간보다 길면 층 전체).
    /// - 칸이 `last` 이하이고 1시간 미만 뒤면 쓰지 않는다([`Append::Behind`]).
    /// - 1시간 이상 뒤면 층마다 그 시각 뒤 칸을 지우고 `last`를 되돌린 뒤 쓴다([`Append::Rewound`]).
    pub fn append(&mut self, t: i64, values: &[Option<f32>]) -> io::Result<Append> {
        if values.len() != self.hdr.fields.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("값 {}개, 칸 {}개", values.len(), self.hdr.fields.len()),
            ));
        }
        let raw = Tier::Raw5s;
        let rows = i64::from(raw.rows());
        let b0 = t.div_euclid(raw.step_ms());
        let mut outcome = Append::Written;
        if let Some(last) = self.hdr.last[0]
            && b0 <= last
        {
            if last * raw.step_ms() - t < REWIND_MS {
                return Ok(Append::Behind);
            }
            self.rewind(t)?;
            outcome = Append::Rewound;
        }
        // 원본 자리를 덮기 전에 닫힐 칸을 먼저 닫는다(공백이 1시간보다 길면 덮인다)
        self.close_through(t)?;
        if let Some(last) = self.hdr.last[0]
            && b0 > last + 1
        {
            let start = (last + 1).max(b0 - rows + 1);
            self.fill_nan(0, start, (b0 - start) as usize)?;
        }
        let cell: Vec<f32> = values.iter().map(|v| v.unwrap_or(f32::NAN)).collect();
        self.write_cells(0, b0, &cell)?;
        self.hdr.last[0] = Some(b0);
        self.dirty = true;
        Ok(outcome)
    }

    /// `now`까지 끝난 거친 층 칸(`Min1`·`Min30`·`Hour6`)을 닫는다. 기록 주기마다 그 주기의 점을 모두
    /// [`Rrd::append`]한 뒤 부른다. 다시 연 뒤 처음 부르면 머리의 `last` 뒤에 닫힌 칸을 고운 층에서 메운다.
    pub fn close(&mut self, now: i64) -> io::Result<()> {
        self.close_through(now)
    }

    /// `t_end`까지 끝난 거친 층 칸을 닫는다. 크게 건너뛸 때 `Min1`이 먼저 밀려 나가면 닫히지 않은 30분·6시간 칸의
    /// 재료를 잃으므로, 원본 끝을 6시간 경계로 올린 시각까지 먼저 닫는다.
    fn close_through(&mut self, t_end: i64) -> io::Result<()> {
        if let Some(last) = self.hdr.last[0] {
            let h = Tier::Hour6.step_ms();
            let end = (last + 1) * Tier::Raw5s.step_ms();
            let edge = (end + h - 1).div_euclid(h) * h;
            if edge < t_end {
                self.close_tiers(edge)?;
            }
        }
        self.close_tiers(t_end)
    }

    fn close_tiers(&mut self, t_end: i64) -> io::Result<()> {
        for (i, tier) in TIERS.into_iter().enumerate().skip(1) {
            let target = t_end.div_euclid(tier.step_ms()) - 1;
            let Some(last) = self.hdr.last[i] else {
                // 처음 쓰는 파일: 지금 칸 앞은 비어 있다(만들 때 NaN)
                self.hdr.last[i] = Some(target);
                self.dirty = true;
                continue;
            };
            if target <= last {
                continue;
            }
            let rows = i64::from(tier.rows());
            let start = (last + 1).max(target - rows + 1);
            let count = (target - start + 1) as usize;
            // Min1은 Raw5s에서, Min30·Hour6은 Min1에서
            let src = if i == 1 { 0 } else { 1 };
            let ratio = tier.step_ms() / TIERS[src].step_ms();
            let n = self.hdr.fields.len();
            let per = cell_floats(tier, n);
            let mut out = vec![f32::NAN; count * per];
            for k in 0..count {
                let b = start + k as i64;
                let cells = read_cells(&self.file, &self.hdr, src, b * ratio, ratio as usize)?;
                let src_per = cell_floats(TIERS[src], n);
                for f in 0..n {
                    let agg = if src == 0 {
                        let vals: Vec<f32> = (0..ratio as usize)
                            .map(|c| cells[c * src_per + f])
                            .collect();
                        consolidate(&vals)
                    } else {
                        let aggs: Vec<Agg> = (0..ratio as usize)
                            .map(|c| agg_at(&cells[c * src_per..(c + 1) * src_per], n, f))
                            .collect();
                        consolidate_aggs(&aggs)
                    };
                    out[k * per + f] = agg.avg;
                    out[k * per + n + f] = agg.max;
                    out[k * per + 2 * n + f] = agg.min;
                }
            }
            self.write_cells(i, start, &out)?;
            self.hdr.last[i] = Some(target);
            self.dirty = true;
        }
        Ok(())
    }

    /// 층마다 `t`가 든 칸부터 `last`까지 지우고 `last`를 그 앞 칸으로 되돌린다.
    fn rewind(&mut self, t: i64) -> io::Result<()> {
        for (i, tier) in TIERS.into_iter().enumerate() {
            let new_last = t.div_euclid(tier.step_ms()) - 1;
            let Some(old) = self.hdr.last[i] else {
                continue;
            };
            if new_last >= old {
                continue;
            }
            let rows = i64::from(tier.rows());
            let lo = (new_last + 1).max(old - rows + 1);
            self.fill_nan(i, lo, (old - lo + 1) as usize)?;
            self.hdr.last[i] = Some(new_last);
        }
        self.dirty = true;
        Ok(())
    }

    fn fill_nan(&self, i: usize, first: i64, count: usize) -> io::Result<()> {
        let per = cell_floats(TIERS[i], self.hdr.fields.len());
        self.write_cells(i, first, &vec![f32::NAN; count * per])
    }

    /// 층 `i`의 칸 `first..`에 `cells`(칸마다 `cell_floats`개)를 제자리에 쓴다.
    fn write_cells(&self, i: usize, first: i64, cells: &[f32]) -> io::Result<()> {
        let tier = TIERS[i];
        let per = cell_floats(tier, self.hdr.fields.len());
        let rows = i64::from(tier.rows());
        let count = cells.len() / per;
        for (b, n) in runs(first, count, rows) {
            let at = (b - first) as usize * per;
            let buf: Vec<u8> = cells[at..at + n * per]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            self.file
                .write_all_at(&buf, self.hdr.offsets[i] + slot(b, rows) * (per * 4) as u64)?;
        }
        Ok(())
    }

    /// 쓴 데이터를 디스크에 닿게 한다(`fdatasync`). 여러 파일을 쓸 때는 모두 이것을 부른 뒤 [`Rrd::commit`]한다.
    pub fn sync_data(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// 바뀐 것이 있으면 머리를 다른 사본 자리(`seq + 1`의 짝홀)에 쓰고 sync 한다. 데이터는 먼저
    /// [`Rrd::sync_data`]로 디스크에 닿아 있어야 한다.
    pub fn commit(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let mut next = self.hdr.clone();
        next.seq += 1;
        let at = (next.seq % 2) * HEADER_COPY_BYTES as u64;
        self.file.write_all_at(&next.encode(), at)?;
        self.file.sync_data()?;
        self.hdr = next;
        self.dirty = false;
        Ok(())
    }

    /// 파일 하나만 쓸 때: [`Rrd::sync_data`] 뒤 [`Rrd::commit`]
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.sync_data()?;
        self.commit()
    }

    /// 층 `tier`에서 `[from, until]`(ms)이 걸치는 칸을 읽는다. 기록 밖이나 모르는 칸은 NaN이다. 칸이 층 칸 수의
    /// 두 배보다 많으면 `InvalidInput`이다(층을 먼저 고른다).
    pub fn read(&self, tier: Tier, from: i64, until: i64) -> io::Result<TierRead> {
        let (first, count) = bucket_window(tier, from, until);
        if count > 2 * tier.rows() as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} 층에서 칸 {count}개는 너무 많다", tier.as_str()),
            ));
        }
        let i = tier_index(tier);
        let n = self.hdr.fields.len();
        let per = cell_floats(tier, n);
        let cells = read_cells(&self.file, &self.hdr, i, first, count)?;
        let columns = (0..n)
            .map(|f| {
                (0..count)
                    .map(|k| {
                        let cell = &cells[k * per..(k + 1) * per];
                        if tier == Tier::Raw5s {
                            Agg::single(cell[f])
                        } else {
                            agg_at(cell, n, f)
                        }
                    })
                    .collect()
            })
            .collect();
        Ok(TierRead {
            tier,
            first,
            fields: self.hdr.fields.clone(),
            columns,
        })
    }

    /// 파일 전체(머리는 마지막으로 commit 한 것). 다른 노드로 밀어 넘길 때 쓴다.
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        let len = self.file.metadata()?.len();
        let mut buf = vec![0u8; len as usize];
        self.file.read_exact_at(&mut buf, 0)?;
        Ok(buf)
    }

    /// 다른 노드가 보낸 같은 시리즈의 파일 바이트와 칸 단위로 합치고 commit 한다. 자기 값이 NaN이면 받은 값을,
    /// 둘 다 있으면 자기 값을 둔다. `last`는 큰 쪽이다. 같은 입력이면 결과가 같아 두 번 받아도 된다. 받은 파일의
    /// 칸 목록이 다르면 같은 칸 id만 합친다.
    pub fn merge(&mut self, bytes: &[u8]) -> io::Result<()> {
        let theirs = match pick(&bytes[..(2 * HEADER_COPY_BYTES).min(bytes.len())]) {
            Copy::Valid(h) => h,
            Copy::Newer(v) => {
                return Err(invalid(&format!("받은 기록 파일이 더 새 형식(v{v})이다")));
            }
            Copy::Invalid => return Err(invalid("받은 기록 파일의 머리가 깨졌다")),
        };
        if theirs.kind != self.hdr.kind {
            return Err(invalid("받은 기록 파일의 종류가 다르다"));
        }
        if (bytes.len() as u64) < layout(theirs.fields.len()).1 {
            return Err(short());
        }
        let n = self.hdr.fields.len();
        let map: Vec<Option<usize>> = self
            .hdr
            .fields
            .iter()
            .map(|f| theirs.fields.iter().position(|o| o == f))
            .collect();
        let tn = theirs.fields.len();
        for (i, tier) in TIERS.into_iter().enumerate() {
            let new_last = match (self.hdr.last[i], theirs.last[i]) {
                (None, None) => continue,
                (a, b) => a.max(b).unwrap(),
            };
            let rows = i64::from(tier.rows());
            let first = new_last - rows + 1;
            let cfs = cell_floats(tier, 1);
            let mine = read_cells(&self.file, &self.hdr, i, first, rows as usize)?;
            let other = read_cells(bytes, &theirs, i, first, rows as usize)?;
            let mut out = mine;
            for r in 0..rows as usize {
                for c in 0..cfs {
                    for (j, m) in map.iter().enumerate() {
                        let d = r * cfs * n + c * n + j;
                        if out[d].is_nan()
                            && let Some(o) = m
                        {
                            out[d] = other[r * cfs * tn + c * tn + o];
                        }
                    }
                }
            }
            self.write_cells(i, first, &out)?;
            self.hdr.last[i] = Some(new_last);
            self.dirty = true;
        }
        self.flush()
    }
}

fn tier_index(tier: Tier) -> usize {
    TIERS.iter().position(|t| *t == tier).unwrap()
}

/// 거친 층 칸(`[AVG..][MAX..][MIN..]`)에서 칸 `f`의 값
fn agg_at(cell: &[f32], n: usize, f: usize) -> Agg {
    Agg {
        avg: cell[f],
        max: cell[n + f],
        min: cell[2 * n + f],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const MIN: i64 = 60_000;
    /// 칸 경계에 맞춘 시작 시각(6시간·30분 경계)
    const T0: i64 = 1_700_000_000_000 / (6 * 3_600_000) * (6 * 3_600_000);

    fn two() -> [MetricField; 2] {
        [MetricField::Cpu, MetricField::MemRss]
    }

    fn open2(path: &Path) -> (Rrd, Opened) {
        Rrd::open_fields(path, SeriesKind::Vm, &two()).unwrap()
    }

    /// `[from, until)`에 5초마다 `f(t)` 값(두 칸: v, v×2)을 넣는다.
    fn fill(rrd: &mut Rrd, from: i64, until: i64, f: impl Fn(i64) -> Option<f32>) {
        let mut t = from;
        while t < until {
            let v = f(t);
            assert_eq!(
                rrd.append(t, &[v, v.map(|x| x * 2.0)]).unwrap(),
                Append::Written
            );
            t += 5_000;
        }
    }

    fn raw_at(rrd: &Rrd, t: i64) -> f32 {
        rrd.read(Tier::Raw5s, t, t).unwrap().columns[0][0].avg
    }

    fn cell(rrd: &Rrd, tier: Tier, t: i64) -> Agg {
        rrd.read(tier, t, t).unwrap().columns[0][0]
    }

    fn same(a: f32, b: f32) -> bool {
        (a.is_nan() && b.is_nan()) || a == b
    }

    #[test]
    fn file_sizes_match_design() {
        assert_eq!(file_size(VM_FIELDS.len()), 336 * 1024);
        assert_eq!(file_size(NODE_FIELDS.len()), 376 * 1024);
        assert_eq!(file_size(POOL_FIELDS.len()), 92 * 1024);
        let dir = tempfile::tempdir().unwrap();
        for (kind, path) in [
            (SeriesKind::Node, node_path(dir.path())),
            (SeriesKind::Vm, vm_path(dir.path(), Uuid::nil())),
            (SeriesKind::Pool, pool_path(dir.path(), Uuid::nil())),
        ] {
            let (mut rrd, opened) = Rrd::open(&path, kind).unwrap();
            assert_eq!(opened, Opened::Created);
            let vals = vec![Some(1.0); kind.fields().len()];
            for k in 0..2000 {
                rrd.append(T0 + k * 5_000, &vals).unwrap();
            }
            rrd.close(T0 + 400 * 24 * 3_600_000).unwrap();
            rrd.flush().unwrap();
            // 쓰기로 크기가 바뀌지 않는다
            assert_eq!(
                std::fs::metadata(&path).unwrap().len(),
                file_size(kind.fields().len())
            );
        }
        assert!(
            dir.path()
                .join("vm")
                .join(format!("{}.rrd", Uuid::nil()))
                .exists()
        );
    }

    #[test]
    fn permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = vm_path(&dir.path().join("metrics"), Uuid::nil());
        Rrd::open(&path, SeriesKind::Vm).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert!(!with_suffix(&path, ".tmp").exists());
    }

    #[test]
    fn round_trip_and_consolidation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rrd");
        let (mut rrd, _) = open2(&path);
        // 분 0: 0..11, 분 1: 100, 분 2: 한 점(분 1을 닫는다)
        fill(&mut rrd, T0, T0 + MIN, |t| Some(((t - T0) / 5_000) as f32));
        fill(&mut rrd, T0 + MIN, T0 + 2 * MIN, |_| Some(100.0));
        rrd.append(T0 + 2 * MIN, &[Some(7.0), None]).unwrap();
        rrd.flush().unwrap();

        assert_eq!(rrd.last(Tier::Raw5s), Some((T0 + 2 * MIN) / 5_000));
        assert_eq!(rrd.last(Tier::Min1), Some((T0 + MIN) / MIN));
        let r = rrd.read(Tier::Raw5s, T0, T0 + 2 * MIN).unwrap();
        assert_eq!(r.first, T0 / 5_000);
        assert_eq!(r.start_ms(), T0);
        assert_eq!(r.columns[0].len(), 25);
        for k in 0..12 {
            assert_eq!(r.columns[0][k], Agg::single(k as f32));
            assert_eq!(
                r.column(MetricField::MemRss).unwrap()[k],
                Agg::single(k as f32 * 2.0)
            );
        }
        assert_eq!(r.columns[0][24].avg, 7.0);
        assert!(r.columns[1][24].is_unknown());

        let m = rrd.read(Tier::Min1, T0, T0 + MIN).unwrap();
        assert_eq!(
            m.columns[0][0],
            Agg {
                avg: 5.5,
                max: 11.0,
                min: 0.0
            }
        );
        assert_eq!(
            m.columns[1][0],
            Agg {
                avg: 11.0,
                max: 22.0,
                min: 0.0
            }
        );
        assert_eq!(m.columns[0][1], Agg::single(100.0));
        // 분 2는 아직 닫히지 않았다
        assert!(cell(&rrd, Tier::Min1, T0 + 2 * MIN).is_unknown());
        // 닫으면 아는 값 1개 < 6 → 모름
        rrd.close(T0 + 3 * MIN).unwrap();
        assert_eq!(rrd.last(Tier::Min1), Some((T0 + 2 * MIN) / MIN));
        assert!(cell(&rrd, Tier::Min1, T0 + 2 * MIN).is_unknown());
    }

    #[test]
    fn coarse_tiers_and_catch_up_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rrd");
        let (mut rrd, _) = open2(&path);
        // 40분 동안 분마다 값 = 분 번호
        fill(&mut rrd, T0, T0 + 40 * MIN, |t| {
            Some(((t - T0) / MIN) as f32)
        });
        rrd.flush().unwrap();
        // 30분 칸 0은 닫혔다: 평균 14.5, 최대 29, 최소 0
        assert_eq!(
            cell(&rrd, Tier::Min30, T0),
            Agg {
                avg: 14.5,
                max: 29.0,
                min: 0.0
            }
        );
        assert_eq!(rrd.last(Tier::Min30), Some(T0 / (30 * MIN)));
        drop(rrd);

        // 다시 열면 그대로이고, 닫히지 않은 칸은 Min1에서 메운다
        let (mut rrd, opened) = open2(&path);
        assert_eq!(opened, Opened::Existing);
        assert_eq!(raw_at(&rrd, T0 + 39 * MIN), 39.0);
        assert_eq!(cell(&rrd, Tier::Min1, T0 + 38 * MIN).avg, 38.0);
        rrd.close(T0 + 6 * 60 * MIN).unwrap();
        rrd.flush().unwrap();
        // 30분 칸 1(분 30..59): 아는 칸 10개 < 15 → 모름
        assert!(cell(&rrd, Tier::Min30, T0 + 30 * MIN).is_unknown());
        // 6시간 칸 0: 아는 1분 칸 40개 < 180 → 모름
        assert!(cell(&rrd, Tier::Hour6, T0).is_unknown());
        assert_eq!(rrd.last(Tier::Hour6), Some(T0 / (360 * MIN)));
    }

    #[test]
    fn hour6_consolidates_from_min1() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        // 6시간 중 앞 4시간(240분 ≥ 180)만 값이 있다
        fill(&mut rrd, T0, T0 + 240 * MIN, |t| {
            Some(if t < T0 + 120 * MIN { 1.0 } else { 3.0 })
        });
        rrd.close(T0 + 360 * MIN).unwrap();
        assert_eq!(
            cell(&rrd, Tier::Hour6, T0),
            Agg {
                avg: 2.0,
                max: 3.0,
                min: 1.0
            }
        );
        // 30분 칸 8..11은 모름
        assert!(cell(&rrd, Tier::Min30, T0 + 300 * MIN).is_unknown());
        assert_eq!(cell(&rrd, Tier::Min30, T0 + 90 * MIN).avg, 1.0);
    }

    #[test]
    fn nan_gaps_and_xff() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        // 분 0: 아는 값 6개(절반) → 값, 분 1: 5개 → 모름
        fill(&mut rrd, T0, T0 + MIN, |t| {
            ((t - T0) / 5_000 < 6).then_some(2.0)
        });
        fill(&mut rrd, T0 + MIN, T0 + 2 * MIN, |t| {
            ((t - T0 - MIN) / 5_000 < 5).then_some(2.0)
        });
        rrd.close(T0 + 2 * MIN).unwrap();
        assert_eq!(cell(&rrd, Tier::Min1, T0).avg, 2.0);
        assert!(cell(&rrd, Tier::Min1, T0 + MIN).is_unknown());
        assert!(raw_at(&rrd, T0 + 55_000).is_nan());

        // 3분 공백(opld 멈춤) 뒤 이어 쓰기: 공백 칸은 NaN
        rrd.append(T0 + 5 * MIN, &[Some(9.0), Some(9.0)]).unwrap();
        let r = rrd.read(Tier::Raw5s, T0 + 2 * MIN, T0 + 5 * MIN).unwrap();
        assert!(r.columns[0][..36].iter().all(|a| a.is_unknown()));
        assert_eq!(r.columns[0][36].avg, 9.0);
        for m in 2..5 {
            assert!(cell(&rrd, Tier::Min1, T0 + m * MIN).is_unknown());
        }
    }

    #[test]
    fn wrap_around() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        // Raw5s 800점(720칸 넘음), Min1 1500분(1440칸 넘음)
        let end = T0 + 1500 * MIN;
        fill(&mut rrd, T0, end, |t| Some(((t - T0) / 5_000) as f32));
        rrd.flush().unwrap();
        let last = (end - 5_000) / 5_000;
        assert_eq!(rrd.last(Tier::Raw5s), Some(last));
        // 보이는 원본은 마지막 720칸뿐이다
        assert!(raw_at(&rrd, (last - 720) * 5_000).is_nan());
        let r = rrd
            .read(Tier::Raw5s, (last - 719) * 5_000, last * 5_000)
            .unwrap();
        for (k, a) in r.columns[0].iter().enumerate() {
            assert_eq!(
                a.avg,
                ((last - 719 + k as i64) * 5_000 - T0) as f32 / 5_000.0
            );
        }
        // Min1: 마지막 닫힌 분은 1498, 1440칸 앞(분 58)은 밖, 분 59는 안
        assert_eq!(rrd.last(Tier::Min1), Some((T0 + 1498 * MIN) / MIN));
        assert!(cell(&rrd, Tier::Min1, T0 + 58 * MIN).is_unknown());
        let m59 = cell(&rrd, Tier::Min1, T0 + 59 * MIN);
        assert_eq!(m59.min, (59 * 12) as f32);
        assert_eq!(m59.max, (59 * 12 + 11) as f32);
        // 다시 열어도 같다
        drop(rrd);
        let (rrd, _) = open2(&dir.path().join("a.rrd"));
        assert_eq!(cell(&rrd, Tier::Min1, T0 + 59 * MIN), m59);
    }

    #[test]
    fn gap_longer_than_tier_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        fill(&mut rrd, T0, T0 + 30 * MIN, |_| Some(1.0));
        // 2일 뒤(Raw5s·Min1 기간보다 긴 공백)
        let t = T0 + 2 * 24 * 60 * MIN;
        assert_eq!(
            rrd.append(t, &[Some(5.0), Some(5.0)]).unwrap(),
            Append::Written
        );
        let raw = rrd.read(Tier::Raw5s, t - 3_595_000, t).unwrap();
        assert!(raw.columns[0][..719].iter().all(|a| a.is_unknown()));
        assert_eq!(raw.columns[0][719].avg, 5.0);
        let day = rrd.read(Tier::Min1, t - 24 * 60 * MIN, t).unwrap();
        assert!(day.columns[0].iter().all(|a| a.is_unknown()));
        // 30분 층(1주)에는 앞 기록이 남는다
        assert_eq!(cell(&rrd, Tier::Min30, T0).avg, 1.0);
    }

    #[test]
    fn clock_small_backward_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        fill(&mut rrd, T0, T0 + 10 * MIN, |_| Some(1.0));
        let last = rrd.last(Tier::Raw5s).unwrap();
        // 같은 칸, 바로 앞 칸, 1시간 − 1 ms 뒤는 쓰지 않는다
        for t in [
            last * 5_000,
            last * 5_000 - 5_000,
            last * 5_000 - REWIND_MS + 1,
        ] {
            assert_eq!(rrd.append(t, &[Some(9.0), None]).unwrap(), Append::Behind);
        }
        assert_eq!(rrd.last(Tier::Raw5s), Some(last));
        assert_eq!(raw_at(&rrd, last * 5_000 - 5_000), 1.0);
        // 시계가 따라잡으면 이어 쓴다
        assert_eq!(
            rrd.append((last + 1) * 5_000, &[Some(2.0), None]).unwrap(),
            Append::Written
        );
        assert_eq!(raw_at(&rrd, (last + 1) * 5_000), 2.0);
    }

    #[test]
    fn clock_large_backward_rewinds() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        // 옳은 시계로 30분, 그 뒤 RTC가 3시간 앞서 10분
        fill(&mut rrd, T0, T0 + 30 * MIN, |_| Some(1.0));
        let future = T0 + 210 * MIN;
        fill(&mut rrd, future, future + 10 * MIN, |_| Some(8.0));
        rrd.flush().unwrap();
        let old_last = rrd.last(Tier::Raw5s).unwrap();
        // 정확히 1시간 뒤 → 되돌림
        let t = T0 + 31 * MIN;
        assert!(old_last * 5_000 - t >= REWIND_MS);
        assert_eq!(rrd.append(t, &[Some(3.0), None]).unwrap(), Append::Rewound);
        assert_eq!(rrd.last(Tier::Raw5s), Some(t / 5_000));
        assert_eq!(rrd.last(Tier::Min1), Some(t / MIN - 1));
        assert_eq!(rrd.last(Tier::Min30), Some(t / (30 * MIN) - 1));
        // 미래 칸은 버렸고, 앞 기록은 남는다(원본은 3시간 공백으로 이미 비었다)
        assert!(raw_at(&rrd, future).is_nan());
        assert!(raw_at(&rrd, T0 + 29 * MIN).is_nan());
        assert_eq!(cell(&rrd, Tier::Min1, T0 + 29 * MIN).avg, 1.0);
        assert_eq!(cell(&rrd, Tier::Min30, T0).avg, 1.0);
        assert!(cell(&rrd, Tier::Min30, T0 + 180 * MIN).is_unknown());
        assert_eq!(raw_at(&rrd, t), 3.0);
        rrd.flush().unwrap();
        // 다시 열어 그 앞에서 이어 쓰면 미래 칸은 다시 나타나지 않는다
        drop(rrd);
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        fill(&mut rrd, t + 5_000, t + 3 * MIN, |_| Some(4.0));
        assert_eq!(cell(&rrd, Tier::Min1, T0 + 32 * MIN).avg, 4.0);
        let r = rrd.read(Tier::Min1, future, future + 10 * MIN).unwrap();
        assert!(r.columns[0].iter().all(|a| a.is_unknown()));

        // 경계 바로 안쪽(1시간 − 1 ms)은 되돌리지 않는다
        let (mut rrd2, _) = open2(&dir.path().join("b.rrd"));
        fill(&mut rrd2, T0, T0 + 90 * MIN, |_| Some(1.0));
        let l = rrd2.last(Tier::Raw5s).unwrap();
        assert_eq!(
            rrd2.append(l * 5_000 - REWIND_MS + 1, &[None, None])
                .unwrap(),
            Append::Behind
        );
        assert_eq!(
            rrd2.append(l * 5_000 - REWIND_MS, &[None, None]).unwrap(),
            Append::Rewound
        );
    }

    #[test]
    fn clock_forward_jump_is_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        fill(&mut rrd, T0, T0 + 5 * MIN, |_| Some(1.0));
        let t = T0 + 50 * MIN;
        rrd.append(t, &[Some(2.0), None]).unwrap();
        let r = rrd.read(Tier::Min1, T0, t).unwrap();
        assert!(r.columns[0][..5].iter().all(|a| a.avg == 1.0));
        assert!(r.columns[0][5..50].iter().all(|a| a.is_unknown()));
        // 30분 칸 0: 아는 1분 칸 5개 < 15 → 모름
        assert!(cell(&rrd, Tier::Min30, T0).is_unknown());
    }

    fn header_bytes(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()[..2 * HEADER_COPY_BYTES].to_vec()
    }

    fn newest_copy(path: &Path) -> usize {
        let head = header_bytes(path);
        let seq = |i: usize| match decode(&head[i * HEADER_COPY_BYTES..(i + 1) * HEADER_COPY_BYTES])
        {
            Copy::Valid(h) => h.seq,
            other => panic!("{other:?}"),
        };
        if seq(1) > seq(0) { 1 } else { 0 }
    }

    fn patch(path: &Path, at: u64, bytes: &[u8]) {
        let f = OpenOptions::new().write(true).open(path).unwrap();
        f.write_all_at(bytes, at).unwrap();
    }

    /// 분 0·1을 commit 하고 분 2를 쓴 뒤 데이터만 sync 하고 머리를 쓰지 못한 파일
    fn interrupted(path: &Path) {
        let (mut rrd, _) = open2(path);
        fill(&mut rrd, T0, T0 + MIN, |_| Some(1.0));
        rrd.flush().unwrap();
        fill(&mut rrd, T0 + MIN, T0 + 2 * MIN, |_| Some(2.0));
        rrd.flush().unwrap();
        fill(&mut rrd, T0 + 2 * MIN, T0 + 3 * MIN, |_| Some(3.0));
        rrd.sync_data().unwrap();
    }

    /// 머리를 쓰기 전에 끊기면 마지막 기록 주기를 잃는다. 덮인 가장 오래된 자리의 새 값이 잠깐 옛 시각으로 보이는
    /// 것은 모듈 문서대로 그대로 둔다(리뷰 1 #10).
    #[test]
    fn interrupted_before_header_loses_last_minute() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rrd");
        interrupted(&path);
        let (mut rrd, opened) = open2(&path);
        assert_eq!(opened, Opened::Existing);
        assert_eq!(rrd.last(Tier::Raw5s), Some((T0 + 2 * MIN - 5_000) / 5_000));
        assert_eq!(raw_at(&rrd, T0 + MIN), 2.0);
        assert!(
            raw_at(&rrd, T0 + 2 * MIN).is_nan(),
            "가리키지 않은 칸은 읽지 않는다"
        );
        // 공백 뒤 쓰면 가리키지 않던 쓰레기 칸은 NaN으로 덮인다
        rrd.append(T0 + 4 * MIN, &[Some(4.0), None]).unwrap();
        let r = rrd
            .read(Tier::Raw5s, T0 + 2 * MIN, T0 + 4 * MIN - 5_000)
            .unwrap();
        assert!(r.columns[0].iter().all(|a| a.is_unknown()));
        assert!(cell(&rrd, Tier::Min1, T0 + 2 * MIN).is_unknown());
        assert_eq!(cell(&rrd, Tier::Min1, T0 + MIN).avg, 2.0);
    }

    #[test]
    fn corrupt_or_truncated_newest_header_falls_back() {
        for damage in ["flip", "zero"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("a.rrd");
            interrupted(&path);
            {
                let (mut rrd, _) = open2(&path);
                // 분 2를 다시 쓰고 commit 해 둔다
                fill(&mut rrd, T0 + 2 * MIN, T0 + 3 * MIN, |_| Some(3.0));
                rrd.flush().unwrap();
            }
            let newest = newest_copy(&path);
            let at = (newest * HEADER_COPY_BYTES) as u64;
            match damage {
                "flip" => {
                    let mut b = header_bytes(&path)[newest * HEADER_COPY_BYTES + 40];
                    b ^= 0xff;
                    patch(&path, at + 40, &[b]);
                }
                _ => patch(&path, at + 100, &[0u8; HEADER_COPY_BYTES - 100]),
            }
            let (mut rrd, opened) = open2(&path);
            assert_eq!(opened, Opened::Existing, "{damage}");
            // 잃은 것은 마지막 commit(1분)뿐
            assert_eq!(
                rrd.last(Tier::Raw5s),
                Some((T0 + 2 * MIN - 5_000) / 5_000),
                "{damage}"
            );
            assert_eq!(raw_at(&rrd, T0 + MIN), 2.0);
            assert_eq!(raw_at(&rrd, T0), 1.0);
            // 다음 commit은 깨진 사본 자리에 쓴다
            fill(&mut rrd, T0 + 2 * MIN, T0 + 3 * MIN, |_| Some(5.0));
            rrd.flush().unwrap();
            assert_eq!(newest_copy(&path), newest);
            drop(rrd);
            let (rrd, _) = open2(&path);
            assert_eq!(raw_at(&rrd, T0 + 2 * MIN), 5.0);
        }
    }

    #[test]
    fn both_headers_corrupt_moves_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rrd");
        interrupted(&path);
        let before = std::fs::read(&path).unwrap();
        patch(&path, 0, &[0u8; 2 * HEADER_COPY_BYTES]);
        let (rrd, opened) = open2(&path);
        let moved = dir.path().join("a.rrd.corrupt");
        assert_eq!(
            opened,
            Opened::Corrupt {
                moved_to: moved.clone()
            }
        );
        assert_eq!(rrd.last(Tier::Raw5s), None);
        assert_eq!(
            std::fs::read(&moved).unwrap()[2 * HEADER_COPY_BYTES..],
            before[2 * HEADER_COPY_BYTES..]
        );
        // 또 깨지면 이름이 겹치지 않는다
        drop(rrd);
        patch(&path, 0, &[0u8; 2 * HEADER_COPY_BYTES]);
        let (_, opened) = open2(&path);
        assert_eq!(
            opened,
            Opened::Corrupt {
                moved_to: dir.path().join("a.rrd.corrupt.1")
            }
        );
    }

    #[test]
    fn short_file_or_wrong_kind_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rrd");
        interrupted(&path);
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(file_size(2) - 4096).unwrap();
        drop(f);
        let (_, opened) = open2(&path);
        assert!(matches!(opened, Opened::Corrupt { .. }));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), file_size(2));

        let (_, opened) = Rrd::open_fields(&path, SeriesKind::Pool, &two()).unwrap();
        assert!(matches!(opened, Opened::Corrupt { .. }));
    }

    #[test]
    fn migrates_added_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rrd");
        {
            let (mut rrd, _) = open2(&path);
            fill(&mut rrd, T0, T0 + 3 * MIN, |t| {
                Some(((t - T0) / MIN) as f32)
            });
            rrd.flush().unwrap();
        }
        let three = [MetricField::NetRx, MetricField::Cpu, MetricField::MemRss];
        let (mut rrd, opened) = Rrd::open_fields(&path, SeriesKind::Vm, &three).unwrap();
        assert_eq!(
            opened,
            Opened::Migrated {
                from: two().to_vec()
            }
        );
        assert_eq!(rrd.fields(), &three);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), file_size(3));
        assert!(!with_suffix(&path, ".tmp").exists());
        let r = rrd.read(Tier::Min1, T0, T0 + MIN).unwrap();
        assert_eq!(
            r.column(MetricField::Cpu).unwrap(),
            &[Agg::single(0.0), Agg::single(1.0)]
        );
        assert_eq!(r.column(MetricField::MemRss).unwrap()[1], Agg::single(2.0));
        assert!(
            r.column(MetricField::NetRx)
                .unwrap()
                .iter()
                .all(|a| a.is_unknown())
        );
        assert_eq!(
            rrd.read(Tier::Raw5s, T0 + 2 * MIN, T0 + 2 * MIN)
                .unwrap()
                .column(MetricField::Cpu)
                .unwrap()[0]
                .avg,
            2.0
        );
        // 옮긴 파일에 이어 쓰고 다시 열면 그대로다
        rrd.append(T0 + 3 * MIN, &[Some(7.0), Some(3.0), None])
            .unwrap();
        rrd.flush().unwrap();
        drop(rrd);
        let (rrd, opened) = Rrd::open_fields(&path, SeriesKind::Vm, &three).unwrap();
        assert_eq!(opened, Opened::Existing);
        assert!(
            cell(&rrd, Tier::Min1, T0 + 2 * MIN).is_unknown(),
            "NetRx 칸(첫 칸)은 옛 기록이 없다"
        );
        assert_eq!(
            rrd.read(Tier::Min1, T0 + 2 * MIN, T0 + 2 * MIN)
                .unwrap()
                .columns[1][0]
                .avg,
            2.0
        );
    }

    #[test]
    fn newer_version_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rrd");
        interrupted(&path);
        let before = std::fs::read(&path).unwrap();
        // 한 사본만 v2라도 새 형식으로 본다
        patch(&path, 8, &(VERSION + 1).to_le_bytes());
        let (rrd, opened) = open2(&path);
        let moved = dir.path().join("a.rrd.v2");
        assert_eq!(
            opened,
            Opened::Newer {
                version: VERSION + 1,
                moved_to: moved.clone()
            }
        );
        assert_eq!(rrd.last(Tier::Raw5s), None);
        assert_eq!(
            std::fs::read(&moved).unwrap()[HEADER_COPY_BYTES..],
            before[HEADER_COPY_BYTES..]
        );
    }

    #[test]
    fn header_round_trip_and_checksum() {
        let mut h = Header::new(SeriesKind::Node, &NODE_FIELDS);
        h.seq = 42;
        h.last = [Some(1), None, Some(-3), Some(i64::MAX)];
        let b = h.encode();
        assert_eq!(decode(&b), Copy::Valid(h.clone()));
        for i in [0, 9, 11, 30, 200, 503, 511] {
            let mut bad = b;
            bad[i] ^= 1;
            assert_ne!(decode(&bad), Copy::Valid(h.clone()), "byte {i}");
        }
    }

    #[test]
    fn merge_fills_unknown_keeps_own_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        // 옛 노드: 분 0..10 값 1, 새 노드(주인): 분 5..20 값 2
        let (mut old, _) = open2(&dir.path().join("old.rrd"));
        fill(&mut old, T0, T0 + 10 * MIN, |_| Some(1.0));
        old.flush().unwrap();
        let (mut own, _) = open2(&dir.path().join("own.rrd"));
        fill(&mut own, T0 + 5 * MIN, T0 + 20 * MIN, |_| Some(2.0));
        own.flush().unwrap();
        let bytes = old.to_bytes().unwrap();

        own.merge(&bytes).unwrap();
        let once = own.to_bytes().unwrap();
        assert!(!own.is_dirty());
        let r = own.read(Tier::Min1, T0, T0 + 19 * MIN).unwrap();
        assert!(
            r.columns[0][..5].iter().all(|a| a.avg == 1.0),
            "받은 값으로 메움"
        );
        assert!(
            r.columns[0][5..19].iter().all(|a| a.avg == 2.0),
            "둘 다 있으면 자기 것"
        );
        assert_eq!(raw_at(&own, T0), 1.0);
        assert_eq!(own.last(Tier::Raw5s), Some((T0 + 20 * MIN - 5_000) / 5_000));

        own.merge(&bytes).unwrap();
        assert_eq!(
            own.to_bytes().unwrap()[2 * HEADER_COPY_BYTES..],
            once[2 * HEADER_COPY_BYTES..]
        );
        // 다시 열어도 남는다
        drop(own);
        let (own, _) = open2(&dir.path().join("own.rrd"));
        assert_eq!(cell(&own, Tier::Min1, T0).avg, 1.0);

        // 받는 쪽이 비었으면 last도 받는다(앞선 쪽이 이김)
        let (mut empty, _) = open2(&dir.path().join("empty.rrd"));
        empty.merge(&bytes).unwrap();
        assert_eq!(empty.last(Tier::Raw5s), old.last(Tier::Raw5s));
        for (a, b) in [(Tier::Raw5s, T0), (Tier::Min1, T0 + 3 * MIN)] {
            assert!(same(cell(&empty, a, b).avg, cell(&old, a, b).avg));
        }
        // 종류가 다르거나 깨진 바이트는 받지 않는다
        let (mut pool, _) =
            Rrd::open_fields(&dir.path().join("p.rrd"), SeriesKind::Pool, &two()).unwrap();
        assert!(pool.merge(&bytes).is_err());
        assert!(empty.merge(&bytes[..1000]).is_err());
        assert!(empty.merge(&bytes[..2 * HEADER_COPY_BYTES]).is_err());
    }

    #[test]
    fn rejects_wrong_value_count_and_huge_read() {
        let dir = tempfile::tempdir().unwrap();
        let (mut rrd, _) = open2(&dir.path().join("a.rrd"));
        assert!(rrd.append(T0, &[Some(1.0)]).is_err());
        assert!(rrd.read(Tier::Raw5s, T0, T0 + 24 * 60 * MIN).is_err());
        // 366일 범위의 Hour6은 된다
        assert!(rrd.read(Tier::Hour6, T0, T0 + 366 * 24 * 60 * MIN).is_ok());
    }

    #[test]
    fn aside_names_are_only_our_moved_and_temporary_files() {
        let uuid = |s: &str| s.parse::<uuid::Uuid>().is_ok();
        let id = "0192f0c2-0000-7000-8000-000000000001";
        for name in [
            format!("{id}.rrd.corrupt"),
            format!("{id}.rrd.corrupt.2"),
            format!("{id}.rrd.v2"),
            format!("{id}.rrd.v3.1"),
            format!("{id}.rrd.tmp"),
        ] {
            assert!(is_aside(&name, uuid), "{name}");
        }
        for name in [
            format!("{id}.rrd"),
            format!("{id}.rrd.bak"),
            format!("{id}.rrd.v"),
            format!("{id}.rrd.corrupt.x"),
            format!("{id}.rrd.tmp.1"),
            "notes.rrd.corrupt".to_string(),
            "x.txt".to_string(),
        ] {
            assert!(!is_aside(&name, uuid), "{name}");
        }
        assert!(is_aside("node.rrd.corrupt", |s| s == "node"));
    }
}
