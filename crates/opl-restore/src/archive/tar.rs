//! 흘려 쓰는 tar.gz(설계 §11): POSIX ustar 머리 + 필요할 때 pax `x` 머리(긴 이름·긴 링크 대상·8 GiB 넘는 크기·큰
//! uid/gid·나노초 시각·1970 전 시각, UTF-8이 아닌 이름은 `hdrcharset=BINARY`로 바이트 그대로), gzip 수준 6.
//! 모드·uid·gid·mtime을 보존하고 심볼릭 링크는 링크로, 하드 링크는 앞선 파일로의 링크로 쓴다. 장치·FIFO는 넣지 않고
//! 보고서에 적는다. 읽기 오류로 짧게 끝난 파일은 선언한 크기까지 0으로 채우고(tar 머리는 이미 나갔다) 보고서에 적는다.

use std::collections::HashSet;
use std::io::{self, Write};

use flate2::Compression;
use flate2::write::GzEncoder;

use super::{Archive, PathGuard, Report, report_name};
use crate::proto::{Attrs, EntryKind, LinkTarget, Name};

const BLOCK: usize = 512;
/// ustar 크기·시각 칸(11자리 8진)의 상한
const MAX_OCTAL_11: u64 = 0o777_7777_7777;
/// ustar uid·gid 칸(7자리 8진)의 상한
const MAX_OCTAL_7: u64 = 0o777_7777;

/// 흘려 쓰는 tar.gz 작성기
pub struct TarGzWriter<W: Write> {
    gz: GzEncoder<W>,
    guard: PathGuard,
    report: Report,
    /// 하드 링크가 가리킬 수 있는 파일(링크 수 2 이상으로 넣은 것)
    linkable: HashSet<Vec<u8>>,
    /// 지금 파일: (경로, 선언한 크기, 쓴 바이트)
    current: Option<(Vec<Vec<u8>>, u64, u64)>,
    entries: u64,
}

/// ustar 머리 하나
struct Header<'a> {
    name: &'a [u8],
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    mtime: i64,
    typeflag: u8,
    linkname: &'a [u8],
}

fn put_octal(field: &mut [u8], value: u64) {
    // 칸 길이 - 1 자리 8진 + NUL. 넘치면 0(pax가 덮는다).
    let digits = field.len() - 1;
    let max = (1u64 << (3 * digits as u32)) - 1;
    let v = if value > max { 0 } else { value };
    let s = format!("{v:0digits$o}");
    field[..digits].copy_from_slice(s.as_bytes());
    field[digits] = 0;
}

fn put_bytes(field: &mut [u8], b: &[u8]) {
    let n = b.len().min(field.len());
    field[..n].copy_from_slice(&b[..n]);
}

impl Header<'_> {
    fn encode(&self) -> [u8; BLOCK] {
        let mut h = [0u8; BLOCK];
        put_bytes(&mut h[0..100], self.name);
        put_octal(&mut h[100..108], u64::from(self.mode & 0o7777));
        put_octal(&mut h[108..116], u64::from(self.uid));
        put_octal(&mut h[116..124], u64::from(self.gid));
        put_octal(&mut h[124..136], self.size);
        put_octal(&mut h[136..148], u64::try_from(self.mtime).unwrap_or(0));
        h[148..156].fill(b' ');
        h[156] = self.typeflag;
        put_bytes(&mut h[157..257], self.linkname);
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        let s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(s.as_bytes());
        h
    }
}

/// pax 레코드 `"<len> <key>=<value>\n"`(len은 자기 자리수를 포함한 전체 길이)
pub(crate) fn pax_record(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    let body = 1 + key.len() + 1 + value.len() + 1;
    let mut len = body + 1;
    while len != body + len.to_string().len() {
        len = body + len.to_string().len();
    }
    out.extend_from_slice(len.to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(key.as_bytes());
    out.push(b'=');
    out.extend_from_slice(value);
    out.push(b'\n');
}

/// ustar 칸에 그대로 들어가는 이름(100바이트 안, 출력 가능한 ASCII만)
pub(crate) fn fits_ustar(b: &[u8]) -> bool {
    b.len() <= 100 && b.iter().all(|c| (0x20..0x7f).contains(c))
}

/// pax 시각 값 `초[.나노초]`(1970 전이면 음수 — 소수부는 0 쪽이 아니라 값 그대로)
pub(crate) fn pax_time(sec: i64, nsec: u32) -> String {
    if nsec == 0 {
        return sec.to_string();
    }
    if sec >= 0 {
        return format!("{sec}.{nsec:09}");
    }
    // sec + nsec/1e9 (sec < 0, 0 < nsec): 예 -2 + 0.5 = -1.5
    let whole = sec + 1;
    let frac = 1_000_000_000 - nsec;
    if whole == 0 {
        format!("-0.{frac:09}")
    } else {
        format!("{whole}.{frac:09}")
    }
}

impl<W: Write> TarGzWriter<W> {
    pub fn new(w: W) -> Self {
        TarGzWriter {
            gz: GzEncoder::new(w, Compression::new(6)),
            guard: PathGuard::new(),
            report: Report::default(),
            linkable: HashSet::new(),
            current: None,
            entries: 0,
        }
    }

    /// 보고서(지금까지)
    pub fn report(&self) -> &Report {
        &self.report
    }

    /// 내려받기가 멈춘 까닭(한도)을 보고서에 적는다 — 줄 수 한도와 무관하게 늘 남는다([`Report::stop`]).
    pub fn stop(&mut self, line: String) {
        self.report.stop(line);
    }

    fn admit(&mut self, path: &[Name], is_dir: bool) -> Option<Vec<Vec<u8>>> {
        let parts: Vec<Vec<u8>> = path.iter().map(|p| p.as_bytes().to_vec()).collect();
        match self.guard.admit(&parts, is_dir) {
            Ok(()) => Some(parts),
            Err(reason) => {
                self.report.skipped(path, reason);
                None
            }
        }
    }

    /// 항목 머리(필요하면 pax 머리 먼저)
    fn header(
        &mut self,
        name: &[u8],
        attrs: &Attrs,
        typeflag: u8,
        size: u64,
        link: &[u8],
    ) -> io::Result<()> {
        let mut pax = Vec::new();
        let binary = std::str::from_utf8(name).is_err() || std::str::from_utf8(link).is_err();
        if binary {
            pax_record(&mut pax, "hdrcharset", b"BINARY");
        }
        if !fits_ustar(name) {
            pax_record(&mut pax, "path", name);
        }
        if !link.is_empty() && !fits_ustar(link) {
            pax_record(&mut pax, "linkpath", link);
        }
        if size > MAX_OCTAL_11 {
            pax_record(&mut pax, "size", size.to_string().as_bytes());
        }
        if u64::from(attrs.uid) > MAX_OCTAL_7 {
            pax_record(&mut pax, "uid", attrs.uid.to_string().as_bytes());
        }
        if u64::from(attrs.gid) > MAX_OCTAL_7 {
            pax_record(&mut pax, "gid", attrs.gid.to_string().as_bytes());
        }
        if attrs.mtime_nsec != 0 || u64::try_from(attrs.mtime).map_or(true, |t| t > MAX_OCTAL_11) {
            pax_record(
                &mut pax,
                "mtime",
                pax_time(attrs.mtime, attrs.mtime_nsec).as_bytes(),
            );
        }
        if !pax.is_empty() {
            // pax 머리 이름: 출력 가능한 ASCII로 줄인 것(풀 때 쓰이지 않는다)
            let mut pname = b"PaxHeader/".to_vec();
            pname.extend(
                name.iter()
                    .map(|c| if (0x20..0x7f).contains(c) { *c } else { b'_' })
                    .take(100 - pname.len()),
            );
            let h = Header {
                name: &pname,
                mode: 0o644,
                uid: 0,
                gid: 0,
                size: pax.len() as u64,
                mtime: attrs.mtime.clamp(0, MAX_OCTAL_11 as i64),
                typeflag: b'x',
                linkname: b"",
            };
            self.gz.write_all(&h.encode())?;
            self.gz.write_all(&pax)?;
            self.pad(pax.len() as u64)?;
        }
        let h = Header {
            name,
            mode: attrs.mode,
            uid: attrs.uid,
            gid: attrs.gid,
            size,
            mtime: attrs.mtime,
            typeflag,
            linkname: link,
        };
        self.gz.write_all(&h.encode())?;
        self.entries += 1;
        Ok(())
    }

    fn pad(&mut self, len: u64) -> io::Result<()> {
        let rem = (len % BLOCK as u64) as usize;
        if rem != 0 {
            self.gz.write_all(&[0u8; BLOCK][..BLOCK - rem])?;
        }
        Ok(())
    }

    /// 지금 파일을 닫는다(모자란 데이터는 0으로 채움).
    fn close_current(&mut self, error: Option<&str>) -> io::Result<()> {
        let Some((path, declared, written)) = self.current.take() else {
            return Ok(());
        };
        let mut left = declared - written;
        let zeros = [0u8; 64 * 1024];
        while left > 0 {
            let n = left.min(zeros.len() as u64) as usize;
            self.gz.write_all(&zeros[..n])?;
            left -= n as u64;
        }
        self.pad(declared)?;
        match error {
            Some(e) => self.report.error(&path, e),
            None if written < declared => {
                self.report.error(&path, "the file ended before its size")
            }
            None => {}
        }
        Ok(())
    }

    /// 끝낸다: 열린 파일을 닫고, 보고서와 끝 블록 둘을 쓰고, gzip을 닫는다.
    pub fn finish(mut self) -> io::Result<W> {
        if self.current.is_some() {
            self.close_current(Some("the download stopped inside this file"))?;
        }
        if !self.report.is_empty() {
            let body = self.report.render();
            let name = report_name(&self.guard);
            let attrs = Attrs {
                kind: EntryKind::File,
                size: body.len() as u64,
                mtime: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64),
                mtime_nsec: 0,
                mode: 0o644,
                uid: 0,
                gid: 0,
                nlink: 1,
            };
            self.header(&name, &attrs, b'0', body.len() as u64, b"")?;
            self.gz.write_all(&body)?;
            self.pad(body.len() as u64)?;
        }
        self.gz.write_all(&[0u8; 2 * BLOCK])?;
        let mut w = self.gz.finish()?;
        w.flush()?;
        Ok(w)
    }
}

impl<W: Write> Archive for TarGzWriter<W> {
    fn dir(&mut self, path: &[Name], attrs: &Attrs) -> io::Result<()> {
        self.close_current(Some("the next item started before this file ended"))?;
        let Some(parts) = self.admit(path, true) else {
            return Ok(());
        };
        let mut name = parts.join(&b'/');
        name.push(b'/');
        self.header(&name, attrs, b'5', 0, b"")
    }

    fn begin_file(&mut self, path: &[Name], attrs: &Attrs) -> io::Result<bool> {
        self.close_current(Some("the next item started before this file ended"))?;
        let Some(parts) = self.admit(path, false) else {
            return Ok(false);
        };
        let name = parts.join(&b'/');
        self.header(&name, attrs, b'0', attrs.size, b"")?;
        if attrs.nlink > 1 {
            self.linkable.insert(name);
        }
        self.current = Some((parts, attrs.size, 0));
        Ok(true)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let Some((_, declared, written)) = self.current.as_mut() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "no open file"));
        };
        if *written + data.len() as u64 > *declared {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "more data than the declared size",
            ));
        }
        *written += data.len() as u64;
        self.gz.write_all(data)
    }

    fn end_file(&mut self, error: Option<&str>) -> io::Result<()> {
        self.close_current(error)
    }

    fn symlink(&mut self, path: &[Name], attrs: &Attrs, target: &LinkTarget) -> io::Result<()> {
        self.close_current(Some("the next item started before this file ended"))?;
        let Some(parts) = self.admit(path, false) else {
            return Ok(());
        };
        let name = parts.join(&b'/');
        self.header(&name, attrs, b'2', 0, target.as_bytes())
    }

    fn hardlink(&mut self, path: &[Name], attrs: &Attrs, target: &[Name]) -> io::Result<()> {
        self.close_current(Some("the next item started before this file ended"))?;
        let target: Vec<u8> = target
            .iter()
            .map(|p| p.as_bytes())
            .collect::<Vec<_>>()
            .join(&b'/');
        // 앞서 넣은(링크 수 2 이상) 파일만 가리킨다 — 아카이브 밖이나 뒤의 항목을 가리키는 링크는 쓰지 않는다.
        if !self.linkable.contains(&target) {
            self.report
                .skipped(path, "hard link to a file that is not in this archive");
            return Ok(());
        }
        let Some(parts) = self.admit(path, false) else {
            return Ok(());
        };
        let name = parts.join(&b'/');
        self.header(&name, attrs, b'1', 0, &target)
    }

    fn skip(&mut self, path: &[Name], reason: &str) {
        self.report.skipped(path, reason);
    }

    fn entries(&self) -> u64 {
        self.entries
    }
}
