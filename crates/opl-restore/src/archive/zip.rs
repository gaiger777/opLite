//! 흘려 쓰는 zip(설계 §11). 항목마다 지역 헤더(크기·CRC 0, 일반 목적 비트 3) + 데이터 + data descriptor, 끝에 중앙
//! 디렉터리. 4 GiB에 가까운 파일·4 GiB 넘는 오프셋·65,535개 넘는 항목은 zip64. 이름은 UTF-8(비트 11)로 쓰고,
//! UTF-8이 아닌 바이트·`\`·제어 문자는 `%XX`로 바꾼다(Windows 압축 풀기가 `\`를 경로 구분자로 읽지 않게).
//! 심볼릭 링크·하드 링크·장치·FIFO는 넣지 않고 보고서에 적는다 — 풀 때 링크가 대상 밖을 가리키는 일이 없다.

use std::io::{self, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

use super::{Archive, PathGuard, Report, dos_datetime, report_name};
use crate::proto::{Attrs, LinkTarget, Name};

const LOCAL_SIG: u32 = 0x0403_4b50;
const DESCRIPTOR_SIG: u32 = 0x0807_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;

const FLAG_DESCRIPTOR: u16 = 1 << 3;
const FLAG_UTF8: u16 = 1 << 11;
const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
/// 만든 곳 Unix(3), 명세 6.3
const MADE_BY: u16 = (3 << 8) | 63;
const MAX32: u64 = 0xFFFF_FFFF;
/// 이 크기부터 지역 헤더를 zip64로(deflate가 조금 부풀어도 32비트를 넘지 않게 16 MiB 여유)
const ZIP64_THRESHOLD: u64 = 0xFF00_0000;
/// 압축하지 않는 확장자(이미 압축된 것)
const STORED_EXT: &[&str] = &[
    "zip", "gz", "tgz", "xz", "txz", "zst", "bz2", "7z", "rar", "lz4", "jpg", "jpeg", "png", "gif",
    "webp", "heic", "avif", "mp3", "mp4", "m4a", "m4v", "mkv", "mov", "avi", "webm", "ogg", "flac",
    "docx", "xlsx", "pptx", "jar", "apk", "deb", "rpm", "cab", "msi",
];

/// 바이트 수를 세는 쓰기
struct Counter<W> {
    w: W,
    n: u64,
}

impl<W: Write> Counter<W> {
    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.n += b.len() as u64;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct CdEntry {
    name: Vec<u8>,
    method: u16,
    flags: u16,
    time: u16,
    date: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    offset: u64,
    external: u32,
    mtime: Option<i32>,
    /// 지역 헤더·data descriptor가 zip64
    zip64_local: bool,
}

enum Body {
    Stored,
    Deflate(DeflateEncoder<Vec<u8>>),
}

struct Current {
    entry: CdEntry,
    path: Vec<Vec<u8>>,
    crc: flate2::Crc,
    body: Body,
    declared: u64,
    written: u64,
}

/// 흘려 쓰는 zip 작성기
pub struct ZipWriter<W: Write> {
    out: Counter<W>,
    entries: Vec<CdEntry>,
    guard: PathGuard,
    report: Report,
    current: Option<Current>,
    force_zip64: bool,
    level: u32,
}

/// zip 안 조각: UTF-8이 아닌 바이트·`\`·제어 문자·`%XX`로
pub(crate) fn zip_component(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    for chunk in name.utf8_chunks() {
        for c in chunk.valid().chars() {
            if c == '\\' || c.is_control() {
                out.extend_from_slice(format!("%{:02X}", c as u32).as_bytes());
            } else {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
        }
        for b in chunk.invalid() {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        }
    }
    out
}

pub(crate) fn stored_by_name(name: &[u8]) -> bool {
    let Some(dot) = name.iter().rposition(|b| *b == b'.') else {
        return false;
    };
    let ext = String::from_utf8_lossy(&name[dot + 1..]).to_ascii_lowercase();
    STORED_EXT.contains(&ext.as_str())
}

fn le16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn le32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn le64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn clamp32(x: u64, zip64: bool) -> u32 {
    if zip64 || x >= MAX32 {
        u32::MAX
    } else {
        x as u32
    }
}

fn ts_extra(v: &mut Vec<u8>, mtime: Option<i32>) {
    if let Some(t) = mtime {
        le16(v, 0x5455);
        le16(v, 5);
        v.push(1);
        v.extend_from_slice(&t.to_le_bytes());
    }
}

impl<W: Write> ZipWriter<W> {
    pub fn new(w: W) -> Self {
        ZipWriter {
            out: Counter { w, n: 0 },
            entries: Vec::new(),
            guard: PathGuard::new(),
            report: Report::default(),
            current: None,
            force_zip64: false,
            level: 6,
        }
    }

    /// 크기와 무관하게 모든 항목을 zip64로 쓴다(시험 — 4 GiB를 흘리지 않고 zip64 머리를 풀어 보는 데).
    pub fn force_zip64(mut self) -> Self {
        self.force_zip64 = true;
        self
    }

    /// 지금까지 쓴 바이트
    pub fn bytes_written(&self) -> u64 {
        self.out.n
    }

    /// 보고서(지금까지)
    pub fn report(&self) -> &Report {
        &self.report
    }

    /// 보고서에 한 줄을 더한다(내려받기가 한도에서 멈춘 까닭 따위)
    pub fn note(&mut self, line: String) {
        self.report.note(line);
    }

    /// 경로를 zip 이름 조각으로 바꾸고 검사한다. 거르면 보고서에 적고 없음. 이름 길이 칸은 16비트라 이음 이름이
    /// 65,535바이트를 넘는 항목(디렉터리면 그 아래도)은 넣지 않는다 — 넣으면 아카이브 전체가 깨진다.
    fn admit(&mut self, path: &[Name], is_dir: bool) -> Option<Vec<Vec<u8>>> {
        let parts: Vec<Vec<u8>> = path.iter().map(|p| zip_component(p.as_bytes())).collect();
        let name_len = parts.iter().map(|p| p.len() + 1).sum::<usize>() - usize::from(!is_dir);
        let too_long = (name_len > usize::from(u16::MAX))
            .then_some("the path is longer than the 65,535 bytes a zip name can hold (use tar.gz)");
        match self.guard.admit_unless(&parts, is_dir, too_long) {
            Ok(()) => {
                if parts.iter().zip(path).any(|(a, b)| a != b.as_bytes()) {
                    self.report.note(format!(
                        "renamed: {} — stored as {} (name not valid UTF-8 or contains \\ or control characters)",
                        super::escape_path(path),
                        super::escape_path(&parts)
                    ));
                }
                Some(parts)
            }
            Err(reason) => {
                self.report.skipped(path, reason);
                None
            }
        }
    }

    fn entry(&self, name: Vec<u8>, attrs: &Attrs, is_dir: bool, method: u16) -> CdEntry {
        let (time, date) = dos_datetime(attrs.mtime);
        let mut flags = if is_dir { 0 } else { FLAG_DESCRIPTOR };
        if !name.is_ascii() {
            flags |= FLAG_UTF8;
        }
        let type_bits: u32 = if is_dir { 0o040000 } else { 0o100000 };
        CdEntry {
            name,
            method,
            flags,
            time,
            date,
            crc: 0,
            csize: 0,
            usize: 0,
            offset: self.out.n,
            external: ((type_bits | (attrs.mode & 0o7777)) << 16) | if is_dir { 0x10 } else { 0 },
            mtime: i32::try_from(attrs.mtime).ok(),
            zip64_local: false,
        }
    }

    fn local_header(&mut self, e: &CdEntry) -> io::Result<()> {
        let mut h = Vec::with_capacity(30 + e.name.len() + 29);
        let mut extra = Vec::new();
        if e.zip64_local {
            le16(&mut extra, 0x0001);
            le16(&mut extra, 16);
            le64(&mut extra, 0);
            le64(&mut extra, 0);
        }
        ts_extra(&mut extra, e.mtime);
        le32(&mut h, LOCAL_SIG);
        le16(&mut h, if e.zip64_local { 45 } else { 20 });
        le16(&mut h, e.flags);
        le16(&mut h, e.method);
        le16(&mut h, e.time);
        le16(&mut h, e.date);
        le32(&mut h, 0);
        let size_field = if e.zip64_local { u32::MAX } else { 0 };
        le32(&mut h, size_field);
        le32(&mut h, size_field);
        le16(&mut h, e.name.len() as u16);
        le16(&mut h, extra.len() as u16);
        h.extend_from_slice(&e.name);
        h.extend_from_slice(&extra);
        self.out.put(&h)
    }

    fn finish_current(&mut self, error: Option<&str>) -> io::Result<()> {
        let Some(mut cur) = self.current.take() else {
            return Ok(());
        };
        if let Body::Deflate(enc) = cur.body {
            let rest = enc.finish()?;
            cur.entry.csize += rest.len() as u64;
            self.out.put(&rest)?;
        }
        cur.entry.crc = cur.crc.sum();
        cur.entry.usize = cur.written;
        if !cur.entry.zip64_local && (cur.entry.csize >= MAX32 || cur.entry.usize >= MAX32) {
            return Err(io::Error::other("zip entry grew past 4 GiB without zip64"));
        }
        let mut d = Vec::with_capacity(24);
        le32(&mut d, DESCRIPTOR_SIG);
        le32(&mut d, cur.entry.crc);
        if cur.entry.zip64_local {
            le64(&mut d, cur.entry.csize);
            le64(&mut d, cur.entry.usize);
        } else {
            le32(&mut d, cur.entry.csize as u32);
            le32(&mut d, cur.entry.usize as u32);
        }
        self.out.put(&d)?;
        if let Some(msg) = error {
            self.report.error(&cur.path, msg);
        }
        self.entries.push(cur.entry);
        Ok(())
    }

    fn start(&mut self, parts: Vec<Vec<u8>>, attrs: &Attrs, declared: u64) -> io::Result<()> {
        let name = parts.join(&b'/');
        let stored = declared == 0 || stored_by_name(parts.last().map_or(&[][..], |p| p));
        let method = if stored {
            METHOD_STORED
        } else {
            METHOD_DEFLATE
        };
        let mut entry = self.entry(name, attrs, false, method);
        entry.zip64_local = self.force_zip64 || declared >= ZIP64_THRESHOLD;
        self.local_header(&entry)?;
        let body = if stored {
            Body::Stored
        } else {
            Body::Deflate(DeflateEncoder::new(
                Vec::new(),
                Compression::new(self.level),
            ))
        };
        self.current = Some(Current {
            entry,
            path: parts,
            crc: flate2::Crc::new(),
            body,
            declared,
            written: 0,
        });
        Ok(())
    }

    /// 끝낸다: 열린 파일을 닫고(오류로 적음), 보고서·중앙 디렉터리·끝 레코드를 쓴다.
    pub fn finish(mut self) -> io::Result<W> {
        if self.current.is_some() {
            self.finish_current(Some("the download stopped inside this file"))?;
        }
        if !self.report.is_empty() {
            let body = self.report.render();
            let name = report_name(&self.guard);
            let attrs = Attrs {
                kind: crate::proto::EntryKind::File,
                size: body.len() as u64,
                mtime: now(),
                mtime_nsec: 0,
                mode: 0o644,
                uid: 0,
                gid: 0,
                nlink: 1,
            };
            self.start(vec![name], &attrs, body.len() as u64)?;
            self.write(&body)?;
            self.finish_current(None)?;
        }
        let cd_offset = self.out.n;
        let force = self.force_zip64;
        let entries = std::mem::take(&mut self.entries);
        let count = entries.len() as u64;
        for e in entries {
            let big_u = force || e.usize >= MAX32;
            let big_c = force || e.csize >= MAX32;
            let big_o = force || e.offset >= MAX32;
            let mut extra = Vec::new();
            if big_u || big_c || big_o {
                let mut z = Vec::new();
                if big_u {
                    le64(&mut z, e.usize);
                }
                if big_c {
                    le64(&mut z, e.csize);
                }
                if big_o {
                    le64(&mut z, e.offset);
                }
                le16(&mut extra, 0x0001);
                le16(&mut extra, z.len() as u16);
                extra.extend_from_slice(&z);
            }
            ts_extra(&mut extra, e.mtime);
            let zip64 = big_u || big_c || big_o || e.zip64_local;
            let mut h = Vec::with_capacity(46 + e.name.len() + extra.len());
            le32(&mut h, CENTRAL_SIG);
            le16(&mut h, MADE_BY);
            le16(&mut h, if zip64 { 45 } else { 20 });
            le16(&mut h, e.flags);
            le16(&mut h, e.method);
            le16(&mut h, e.time);
            le16(&mut h, e.date);
            le32(&mut h, e.crc);
            le32(&mut h, clamp32(e.csize, big_c));
            le32(&mut h, clamp32(e.usize, big_u));
            le16(&mut h, e.name.len() as u16);
            le16(&mut h, extra.len() as u16);
            le16(&mut h, 0); // 주석
            le16(&mut h, 0); // 디스크
            le16(&mut h, 0); // 내부 속성
            le32(&mut h, e.external);
            le32(&mut h, clamp32(e.offset, big_o));
            h.extend_from_slice(&e.name);
            h.extend_from_slice(&extra);
            self.out.put(&h)?;
        }
        let cd_size = self.out.n - cd_offset;
        let need64 = force || count >= 0xFFFF || cd_offset >= MAX32 || cd_size >= MAX32;
        let mut t = Vec::new();
        if need64 {
            let z_offset = self.out.n;
            le32(&mut t, ZIP64_EOCD_SIG);
            le64(&mut t, 44);
            le16(&mut t, MADE_BY);
            le16(&mut t, 45);
            le32(&mut t, 0);
            le32(&mut t, 0);
            le64(&mut t, count);
            le64(&mut t, count);
            le64(&mut t, cd_size);
            le64(&mut t, cd_offset);
            le32(&mut t, ZIP64_LOCATOR_SIG);
            le32(&mut t, 0);
            le64(&mut t, z_offset);
            le32(&mut t, 1);
        }
        let count16 = if need64 && (force || count >= 0xFFFF) {
            0xFFFF
        } else {
            count as u16
        };
        le32(&mut t, EOCD_SIG);
        le16(&mut t, 0);
        le16(&mut t, 0);
        le16(&mut t, count16);
        le16(&mut t, count16);
        le32(&mut t, clamp32(cd_size, force));
        le32(&mut t, clamp32(cd_offset, force));
        le16(&mut t, 0);
        self.out.put(&t)?;
        self.out.w.flush()?;
        Ok(self.out.w)
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl<W: Write> Archive for ZipWriter<W> {
    fn dir(&mut self, path: &[Name], attrs: &Attrs) -> io::Result<()> {
        self.finish_current(Some("the next item started before this file ended"))?;
        let Some(parts) = self.admit(path, true) else {
            return Ok(());
        };
        let mut name = parts.join(&b'/');
        name.push(b'/');
        let e = self.entry(name, attrs, true, METHOD_STORED);
        self.local_header(&e)?;
        self.entries.push(e);
        Ok(())
    }

    fn begin_file(&mut self, path: &[Name], attrs: &Attrs) -> io::Result<bool> {
        self.finish_current(Some("the next item started before this file ended"))?;
        let Some(parts) = self.admit(path, false) else {
            return Ok(false);
        };
        self.start(parts, attrs, attrs.size)?;
        Ok(true)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let Some(cur) = self.current.as_mut() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "no open file"));
        };
        if cur.written + data.len() as u64 > cur.declared {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "more data than the declared size",
            ));
        }
        cur.crc.update(data);
        cur.written += data.len() as u64;
        match &mut cur.body {
            Body::Stored => {
                cur.entry.csize += data.len() as u64;
                self.out.put(data)
            }
            Body::Deflate(enc) => {
                enc.write_all(data)?;
                let out = std::mem::take(enc.get_mut());
                cur.entry.csize += out.len() as u64;
                self.out.put(&out)
            }
        }
    }

    fn end_file(&mut self, error: Option<&str>) -> io::Result<()> {
        let short = self
            .current
            .as_ref()
            .is_some_and(|c| c.written < c.declared);
        match (error, short) {
            (Some(e), _) => self.finish_current(Some(e)),
            (None, true) => self.finish_current(Some("the file ended before its size")),
            (None, false) => self.finish_current(None),
        }
    }

    fn symlink(&mut self, path: &[Name], _attrs: &Attrs, _target: &LinkTarget) -> io::Result<()> {
        self.finish_current(Some("the next item started before this file ended"))?;
        self.report
            .skipped(path, "symbolic link (zip keeps no links — use tar.gz)");
        Ok(())
    }

    fn hardlink(&mut self, path: &[Name], _attrs: &Attrs, _target: &[Name]) -> io::Result<()> {
        self.finish_current(Some("the next item started before this file ended"))?;
        self.report.skipped(
            path,
            "hard link without content (zip copies content instead)",
        );
        Ok(())
    }

    fn skip(&mut self, path: &[Name], reason: &str) {
        self.report.skipped(path, reason);
    }

    fn entries(&self) -> u64 {
        self.entries.len() as u64 + u64::from(self.current.is_some())
    }
}
