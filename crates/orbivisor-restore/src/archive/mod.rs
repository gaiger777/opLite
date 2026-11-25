//! 여러 항목 내려받기의 흘려 쓰기 작성기(설계 §11): [`zip::ZipWriter`](기본 — Windows 탐색기가 바로 엶)와
//! [`tar::TarGzWriter`](권한·소유자·심볼릭 링크 보존). 둘 다 크기를 미리 모르는 흐름에 바로 쓰고 되감지 않는다.
//!
//! 들어오는 항목은 도우미(믿을 수 없는 입력)가 걸은 결과다. 작성기는 [`PathGuard`]로 다시 거른다: 이름은 이미 검사한
//! [`Name`](비지 않음, `.`·`..`·`/`·NUL 없음)이고, 항목은 깊이 우선 차례여야 하며(부모는 앞서 받은 디렉터리), 같은
//! 경로는 한 번만 — 그래서 아카이브를 풀 때 앞선 심볼릭 링크나 파일을 지나 다른 곳에 쓰는 항목이 생기지 않는다.
//! 넣지 못한 항목과 읽기 오류는 아카이브 끝의 `ORBIVISOR-RESTORE-REPORT.txt`에 적는다.

pub mod tar;
pub mod zip;

use std::collections::HashSet;
use std::io;

use crate::proto::{Attrs, EntryKind, Event, Item, LinkTarget, MAX_PATH_DEPTH, Name, Reply};

/// 보고서 파일 이름(아카이브 맨 위)
pub const REPORT_NAME: &str = "ORBIVISOR-RESTORE-REPORT.txt";

/// 아카이브 안 경로의 차례·겹침 검사(깊이 우선). 조각은 아카이브에 쓰는 바이트(zip은 바꾼 이름)로 견준다.
#[derive(Debug, Default)]
pub struct PathGuard {
    /// 지금 열린 디렉터리들(이름, 그 아래 받은 이름들, 건너뛴 디렉터리인지)
    stack: Vec<(Vec<u8>, HashSet<Vec<u8>>, bool)>,
    top: HashSet<Vec<u8>>,
}

impl PathGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 항목 하나를 받을지. 받으면 `Ok`, 아니면 까닭. 건너뛴 디렉터리의 아래 항목도 건너뛴다.
    pub fn admit(&mut self, path: &[Vec<u8>], is_dir: bool) -> Result<(), &'static str> {
        self.admit_unless(path, is_dir, None)
    }

    /// [`admit`](Self::admit)과 같되, 차례·겹침 검사를 지난 항목도 `reject`가 있으면 그 까닭으로 건너뛴다(디렉터리면 그
    /// 아래도). 작성기만 아는 까닭(zip 이름 길이)에 쓴다.
    pub fn admit_unless(
        &mut self,
        path: &[Vec<u8>],
        is_dir: bool,
        reject: Option<&'static str>,
    ) -> Result<(), &'static str> {
        let Some((name, parent)) = path.split_last() else {
            return Err("empty path");
        };
        if path.len() > MAX_PATH_DEPTH {
            return Err("path is nested too deeply");
        }
        if path
            .iter()
            .any(|p| p.is_empty() || p == b"." || p == b".." || p.contains(&b'/') || p.contains(&0))
        {
            return Err("invalid name");
        }
        let k = parent.len();
        let parent_ok = self.stack.len() >= k
            && self.stack[..k]
                .iter()
                .zip(parent)
                .all(|((n, _, _), p)| n == p);
        if !parent_ok {
            return Err("item is out of order (its parent directory was not sent before it)");
        }
        self.stack.truncate(k);
        let result = if self.stack.iter().any(|(_, _, skipped)| *skipped) {
            Err("its parent directory was skipped")
        } else {
            let siblings = match self.stack.last_mut() {
                Some((_, children, _)) => children,
                None => &mut self.top,
            };
            if !siblings.insert(name.clone()) {
                Err("another item has the same path")
            } else if let Some(why) = reject {
                Err(why)
            } else {
                Ok(())
            }
        };
        if is_dir {
            self.stack
                .push((name.clone(), HashSet::new(), result.is_err()));
        }
        result
    }

    /// 맨 위에 이 이름이 이미 있는지(보고서 이름 고르기)
    pub fn has_top(&self, name: &[u8]) -> bool {
        self.top.contains(name)
    }
}

/// 보고서에 적는 줄 수 상한(넘는 줄은 세기만 한다 — 도우미가 끝없이 건너뛰어도 opld 메모리가 늘지 않게)
pub const MAX_REPORT_LINES: usize = 1000;
/// 보고서 본문 바이트 상한
pub const MAX_REPORT_BYTES: usize = 256 << 10;

/// 보고서(건너뛴 항목·오류). 처음 [`MAX_REPORT_LINES`]줄·[`MAX_REPORT_BYTES`]바이트만 들고 나머지는 센다.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// 내려받기가 멈춘 까닭(첫 줄만 — 줄 수 한도 밖이라 건너뜀 줄이 넘쳐도 남는다)
    stopped: Option<String>,
    lines: Vec<String>,
    bytes: usize,
    omitted: u64,
}

/// 보고서·로그에 쓰는 경로: UTF-8이 아닌 바이트와 제어 문자는 `\xNN`
pub fn escape_path(path: &[impl AsRef<[u8]>]) -> String {
    let mut s = String::new();
    for p in path {
        s.push('/');
        for chunk in p.as_ref().utf8_chunks() {
            for c in chunk.valid().chars() {
                if c.is_control() {
                    s.push_str(&format!("\\x{:02x}", c as u32));
                } else {
                    s.push(c);
                }
            }
            for b in chunk.invalid() {
                s.push_str(&format!("\\x{b:02x}"));
            }
        }
    }
    s
}

impl Report {
    fn push(&mut self, line: impl FnOnce() -> String) {
        if self.lines.len() >= MAX_REPORT_LINES || self.bytes >= MAX_REPORT_BYTES {
            self.omitted += 1;
            return;
        }
        let line = line();
        if self.bytes + line.len() + 1 > MAX_REPORT_BYTES {
            self.omitted += 1;
            self.bytes = MAX_REPORT_BYTES;
            return;
        }
        self.bytes += line.len() + 1;
        self.lines.push(line);
    }

    pub fn skipped(&mut self, path: &[impl AsRef<[u8]>], reason: &str) {
        self.push(|| format!("skipped: {} — {reason}", escape_path(path)));
    }

    pub fn error(&mut self, path: &[impl AsRef<[u8]>], message: &str) {
        self.push(|| {
            format!(
                "incomplete: {} — {message} (the copy in this archive is cut short; download it again)",
                escape_path(path)
            )
        });
    }

    pub fn note(&mut self, line: String) {
        self.push(|| line);
    }

    /// 내려받기가 멈춘 까닭(`stopped: …`). 처음 한 줄만 들고 보고서 맨 앞에 적는다.
    pub fn stop(&mut self, line: String) {
        self.stopped.get_or_insert(line);
    }

    pub fn is_empty(&self) -> bool {
        self.stopped.is_none() && self.lines.is_empty() && self.omitted == 0
    }

    /// 적은 줄과 세기만 한 줄의 합
    pub fn len(&self) -> usize {
        usize::from(self.stopped.is_some()) + self.lines.len() + self.omitted as usize
    }

    /// 보고서 본문
    pub fn render(&self) -> Vec<u8> {
        let mut s = String::from(
            "Orbivisor file restore: items that are missing from this archive or incomplete\n\n",
        );
        if let Some(l) = &self.stopped {
            s.push_str(l);
            s.push('\n');
        }
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        if self.omitted > 0 {
            s.push_str(&format!(
                "… and {} more line(s) not listed (the report keeps the first {} lines)\n",
                self.omitted,
                self.lines.len()
            ));
        }
        s.into_bytes()
    }
}

/// 보고서 파일 이름(맨 위에 같은 이름이 있으면 `-2`, `-3` …)
pub(crate) fn report_name(guard: &PathGuard) -> Vec<u8> {
    let mut name = REPORT_NAME.as_bytes().to_vec();
    let mut n = 2;
    while guard.has_top(&name) {
        name = format!("ORBIVISOR-RESTORE-REPORT-{n}.txt").into_bytes();
        n += 1;
    }
    name
}

/// 흘려 쓰는 아카이브. 차례: `begin_file` → `write`… → `end_file`, 그 사이에 다른 항목 없음.
pub trait Archive {
    fn dir(&mut self, path: &[Name], attrs: &Attrs) -> io::Result<()>;
    /// 파일 시작. 거른 항목이면 거짓(그 데이터는 버리고 `end_file`도 부르지 않아도 된다)
    fn begin_file(&mut self, path: &[Name], attrs: &Attrs) -> io::Result<bool>;
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    /// 파일 끝. `error`면 데이터가 다 오지 못했다(보고서에 적음)
    fn end_file(&mut self, error: Option<&str>) -> io::Result<()>;
    fn symlink(&mut self, path: &[Name], attrs: &Attrs, target: &LinkTarget) -> io::Result<()>;
    fn hardlink(&mut self, path: &[Name], attrs: &Attrs, target: &[Name]) -> io::Result<()>;
    /// 넣지 않는 항목(장치·FIFO 따위, 도우미가 건너뛴 것)
    fn skip(&mut self, path: &[Name], reason: &str);
    /// 넣은 항목 수(보고서 빼고)
    fn entries(&self) -> u64;
}

/// 걷기 흐름의 사건을 아카이브에 옮긴다(데이터가 선언한 크기에 닿으면 파일을 닫는다). 사건은 [`crate::proto::StreamCheck`]를
/// 지난 것이어야 한다.
#[derive(Debug, Default)]
pub struct Feeder {
    /// 지금 파일에 남은 데이터(넣는 중인지)
    open: Option<(u64, bool)>,
}

impl Feeder {
    pub fn new() -> Self {
        Self::default()
    }

    fn item<A: Archive>(&mut self, a: &mut A, item: &Item) -> io::Result<()> {
        let (path, attrs) = (&item.path, &item.attrs);
        match (attrs.kind, &item.hardlink, &item.link_target) {
            (EntryKind::Dir, _, _) => a.dir(path, attrs),
            (EntryKind::File, Some(target), _) => a.hardlink(path, attrs, target),
            (EntryKind::File, None, _) => {
                let writing = a.begin_file(path, attrs)?;
                if attrs.size == 0 {
                    if writing {
                        a.end_file(None)?;
                    }
                } else {
                    self.open = Some((attrs.size, writing));
                }
                Ok(())
            }
            (EntryKind::Symlink, _, Some(t)) => a.symlink(path, attrs, t),
            (EntryKind::Symlink, _, None) => {
                a.skip(path, "symbolic link without a target");
                Ok(())
            }
            (EntryKind::Other, _, _) => {
                a.skip(path, "special file (device, FIFO or socket)");
                Ok(())
            }
        }
    }

    /// 사건 하나
    pub fn feed<A: Archive>(&mut self, a: &mut A, event: &Event) -> io::Result<()> {
        match event {
            Event::Reply(Reply::Item(item)) => self.item(a, item),
            Event::Reply(Reply::Skipped { path, reason }) => {
                a.skip(path, reason);
                Ok(())
            }
            Event::Reply(Reply::FileError { message }) => match self.open.take() {
                Some((_, true)) => a.end_file(Some(message)),
                _ => Ok(()),
            },
            Event::Data(data) => match &mut self.open {
                Some((left, writing)) => {
                    let n = data.len() as u64;
                    if n > *left {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "more data than the declared size",
                        ));
                    }
                    *left -= n;
                    if *writing {
                        a.write(data)?;
                    }
                    if *left == 0 {
                        let writing = *writing;
                        self.open = None;
                        if writing {
                            a.end_file(None)?;
                        }
                    }
                    Ok(())
                }
                None => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "data outside a file",
                )),
            },
            _ => Ok(()),
        }
    }
}

/// DOS 날짜·시각(zip). 1980 전은 1980-01-01, 2107 뒤는 2107-12-31.
pub(crate) fn dos_datetime(unix: i64) -> (u16, u16) {
    let (y, mo, d, h, mi, s) = civil(unix);
    if y < 1980 {
        return (0, (1 << 5) | 1);
    }
    if y > 2107 {
        return (
            (23 << 11) | (59 << 5) | 29,
            ((127 << 9) | (12 << 5) | 31) as u16,
        );
    }
    let time = ((h << 11) | (mi << 5) | (s / 2)) as u16;
    let date = (((y - 1980) << 9) | (mo << 5) | d) as u16;
    (time, date)
}

/// 유닉스 초 → (년, 월, 일, 시, 분, 초) UTC (Howard Hinnant의 civil_from_days)
fn civil(unix: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

#[cfg(test)]
mod tests;
