//! OVA 읽기. OVA는 tar(ustar)다. 헤더만 훑어 항목 표([`OvaIndex`])를 만들고, OVF와 `.mf`만 읽는다.
//! 디스크는 실행 때 항목을 그대로 복사하면서 `.mf`의 해시를 확인한다([`extract`]).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use opl_types::vm_import::SourceVm;
use sha2::Digest as _;

use super::ovf::parse_ovf;

const BLOCK: u64 = 512;
/// `.ovf` 파일의 최대 크기
const MAX_OVF_BYTES: u64 = 4 << 20;
/// `.mf` 파일과 pax·GNU 긴 이름 헤더 본문의 최대 크기
const MAX_SMALL_BYTES: u64 = 1 << 20;
/// tar 항목 이름의 최대 길이(pax `path`·GNU 긴 이름 포함)
const MAX_NAME_BYTES: usize = 4 << 10;
/// tar 헤더(항목)의 최대 개수
const MAX_ENTRIES: usize = 10_000;
/// 해석 경고의 최대 개수(넘는 것은 개수만 알린다)
const MAX_WARNINGS: usize = 50;

/// OVA·OVF를 쓸 수 없는 이유. 메시지는 사용자에게 그대로 보인다.
#[derive(Debug, thiserror::Error)]
pub enum OvaError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl OvaError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

fn truncated() -> OvaError {
    OvaError::invalid("the OVA is truncated")
}

fn not_tar() -> OvaError {
    OvaError::invalid("the file is not an OVA (tar) archive")
}

/// tar 안의 파일 하나. `offset`은 본문이 시작하는 위치다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvaEntry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct OvaIndex {
    /// tar 안의 순서
    pub entries: Vec<OvaEntry>,
    /// 이름 → `entries`의 위치
    by_name: HashMap<String, usize>,
}

impl OvaIndex {
    pub fn get(&self, name: &str) -> Option<&OvaEntry> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }
}

/// tar 헤더를 훑어 파일 항목 표를 만든다. 디렉터리와 pax 전역 헤더는 건너뛰고, pax 확장 헤더(`path`,
/// `size`)와 GNU 긴 이름은 다음 파일·디렉터리 항목에 쓴다(사이의 다른 pax·GNU 긴 이름 헤더에는 쓰지
/// 않는다). 끝은 0 블록 또는 파일 끝이다. 이름이 4 KiB를 넘거나 항목이 10 000개를 넘으면 오류다.
pub fn read_index(path: &Path) -> Result<OvaIndex, OvaError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut index = OvaIndex::default();
    let mut pax_name: Option<String> = None;
    let mut pax_size: Option<u64> = None;
    let mut long_name: Option<String> = None;
    let mut header = [0u8; BLOCK as usize];
    let mut pos = 0;
    let mut count = 0;
    while pos < len {
        if len - pos < BLOCK {
            return Err(truncated());
        }
        file.seek(SeekFrom::Start(pos))?;
        file.read_exact(&mut header)?;
        if header.iter().all(|&b| b == 0) {
            break;
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(OvaError::invalid(format!(
                "the OVA has more than {MAX_ENTRIES} entries"
            )));
        }
        check_checksum(&header)?;
        let typeflag = header[156];
        // pax·GNU 긴 이름 값은 그것이 가리키는 항목(파일·디렉터리)에만 쓴다.
        let entry = matches!(typeflag, b'0' | 0 | b'5');
        let header_size = parse_size(&header[124..136])?;
        let (name, size) = match entry {
            true => (
                pax_name
                    .take()
                    .or(long_name.take())
                    .unwrap_or_else(|| header_name(&header)),
                pax_size.take().unwrap_or(header_size),
            ),
            false => (header_name(&header), header_size),
        };
        if name.len() > MAX_NAME_BYTES {
            return Err(OvaError::invalid(
                "a file name in the OVA is longer than 4 KiB",
            ));
        }
        let data = pos + BLOCK;
        if size > len - data {
            return Err(truncated());
        }
        pos = data + size.next_multiple_of(BLOCK);
        match typeflag {
            b'0' | 0 => {
                let name = name.strip_prefix("./").unwrap_or(&name).to_string();
                if index.by_name.contains_key(&name) {
                    return Err(OvaError::invalid(format!(
                        "duplicate file {name:?} in the OVA"
                    )));
                }
                index.by_name.insert(name.clone(), index.entries.len());
                index.entries.push(OvaEntry {
                    name,
                    offset: data,
                    size,
                });
            }
            b'5' | b'g' => {}
            b'x' => {
                let records = read_header_data(&mut file, data, size)?;
                let (path, size) = parse_pax(&records)?;
                pax_name = path.or(pax_name);
                pax_size = size.or(pax_size);
            }
            b'L' => long_name = Some(c_str(&read_header_data(&mut file, data, size)?)),
            t => {
                return Err(OvaError::invalid(format!(
                    "unsupported tar entry type '{}' for {name:?}",
                    t as char
                )));
            }
        }
    }
    Ok(index)
}

/// 헤더 체크섬(체크섬 칸을 공백으로 두고 바이트를 더한 값)이 맞는지 본다.
fn check_checksum(header: &[u8]) -> Result<(), OvaError> {
    let stored = parse_octal(&header[148..156]).ok_or_else(not_tar)?;
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(b)
            }
        })
        .sum();
    if stored != sum {
        return Err(not_tar());
    }
    Ok(())
}

/// 이름. ustar면 `prefix/name`이다(GNU tar는 prefix 자리를 다른 데 쓴다).
fn header_name(header: &[u8]) -> String {
    let name = c_str(&header[..100]);
    let prefix = c_str(&header[345..500]);
    if &header[257..263] == b"ustar\0" && !prefix.is_empty() {
        format!("{prefix}/{name}")
    } else {
        name
    }
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field).ok()?;
    let text = text.trim_matches(|c| c == '\0' || c == ' ');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

/// 크기 칸. 8진수, 또는 첫 바이트의 맨 위 비트가 켜져 있으면 base-256(큰 파일).
fn parse_size(field: &[u8]) -> Result<u64, OvaError> {
    let invalid = || OvaError::invalid("invalid tar header");
    if field[0] & 0x80 == 0 {
        return parse_octal(field).ok_or_else(invalid);
    }
    if field[0] != 0x80 {
        return Err(invalid());
    }
    field[1..].iter().try_fold(0u64, |acc, &b| {
        acc.checked_mul(256)
            .map(|v| v | u64::from(b))
            .ok_or_else(invalid)
    })
}

fn read_at(file: &mut File, offset: u64, size: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0; size as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// pax·GNU 긴 이름 헤더의 본문
fn read_header_data(file: &mut File, offset: u64, size: u64) -> Result<Vec<u8>, OvaError> {
    if size > MAX_SMALL_BYTES {
        return Err(OvaError::invalid("a tar header is too large"));
    }
    Ok(read_at(file, offset, size)?)
}

/// pax 레코드(`길이 키=값\n`)에서 `path`와 `size`를 읽는다.
fn parse_pax(mut data: &[u8]) -> Result<(Option<String>, Option<u64>), OvaError> {
    let invalid = || OvaError::invalid("invalid pax header");
    let (mut path, mut size) = (None, None);
    while !data.is_empty() {
        let space = data.iter().position(|&b| b == b' ').ok_or_else(invalid)?;
        let len: usize = std::str::from_utf8(&data[..space])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(invalid)?;
        if len <= space + 1 || len > data.len() || data[len - 1] != b'\n' {
            return Err(invalid());
        }
        let record = std::str::from_utf8(&data[space + 1..len - 1]).map_err(|_| invalid())?;
        let (key, value) = record.split_once('=').ok_or_else(invalid)?;
        match key {
            "path" => path = Some(value.to_string()),
            "size" => size = Some(value.parse().map_err(|_| invalid())?),
            _ => {}
        }
        data = &data[len..];
    }
    Ok((path, size))
}

/// `.mf`의 해시(16진수 소문자)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Digest {
    Sha1(String),
    Sha256(String),
    Sha384(String),
    Sha512(String),
}

impl Digest {
    fn hex(&self) -> &str {
        match self {
            Digest::Sha1(hex) | Digest::Sha256(hex) | Digest::Sha384(hex) | Digest::Sha512(hex) => {
                hex
            }
        }
    }
}

/// 해석한 `.mf`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// 파일 → 해시
    pub digests: BTreeMap<String, Digest>,
    /// 모르는 알고리즘으로 적힌 파일(확인하지 않는다)
    pub unsupported: BTreeSet<String>,
}

/// `.mf` 해석. 줄마다 `SHA256(file)= hex`처럼 `SHA1`·`SHA256`·`SHA384`·`SHA512`(공백 차이는 허용).
/// 모르는 알고리즘의 줄은 오류가 아니라 그 파일을 `unsupported`에 넣는다.
pub fn parse_manifest(text: &str) -> Result<Manifest, OvaError> {
    let mut manifest = Manifest::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let bad = || OvaError::invalid(format!("invalid manifest line {line:?}"));
        let (algorithm, rest) = line.split_once('(').ok_or_else(bad)?;
        let (file, hex) = rest.rsplit_once(')').ok_or_else(bad)?;
        let hex = hex
            .trim_start()
            .strip_prefix('=')
            .ok_or_else(bad)?
            .trim()
            .to_ascii_lowercase();
        let (digest, len): (fn(String) -> Digest, usize) =
            match algorithm.trim().to_ascii_uppercase().as_str() {
                "SHA1" => (Digest::Sha1, 40),
                "SHA256" => (Digest::Sha256, 64),
                "SHA384" => (Digest::Sha384, 96),
                "SHA512" => (Digest::Sha512, 128),
                _ => {
                    manifest.unsupported.insert(file.to_string());
                    continue;
                }
            };
        if hex.len() != len || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        manifest.digests.insert(file.to_string(), digest(hex));
    }
    Ok(manifest)
}

/// 해석한 OVA
#[derive(Debug, Clone)]
pub struct Ova {
    pub vm: SourceVm,
    /// 해석 경고(가져오지 않는 장치, 서명, 확인하지 못하는 디스크). 겹치지 않게 50개까지
    pub warnings: Vec<String>,
    pub index: OvaIndex,
    /// `.mf`의 파일 → 해시(`.mf`가 없으면 비어 있다)
    pub manifest: BTreeMap<String, Digest>,
}

/// OVA를 해석한다. `.ovf`는 정확히 하나(4 MiB 이하), `.mf`는 없거나 하나여야 하고, OVF가 가리키는
/// 파일이 모두 tar 안에 있어야 한다. `.mf`는 형식만 보고 해시는 [`extract`]가 확인한다.
pub fn parse_ova(path: &Path) -> Result<Ova, OvaError> {
    let index = read_index(path)?;
    let with_ext = |ext: &str| -> Vec<&OvaEntry> {
        index
            .entries
            .iter()
            .filter(|e| e.name.to_ascii_lowercase().ends_with(ext))
            .collect()
    };
    let ovf = match with_ext(".ovf")[..] {
        [ovf] => ovf,
        [] => return Err(OvaError::invalid("the OVA has no .ovf file")),
        _ => return Err(OvaError::invalid("the OVA has more than one .ovf file")),
    };
    if ovf.size > MAX_OVF_BYTES {
        return Err(OvaError::invalid("the .ovf file is larger than 4 MiB"));
    }
    let manifest = match with_ext(".mf")[..] {
        [] => None,
        [mf] if mf.size > MAX_SMALL_BYTES => {
            return Err(OvaError::invalid("the .mf file is larger than 1 MiB"));
        }
        [mf] => Some(parse_manifest(&read_text(path, mf)?)?),
        _ => return Err(OvaError::invalid("the OVA has more than one .mf file")),
    };
    let parsed = parse_ovf(&read_text(path, ovf)?)?;
    if let Some(file) = parsed.files.iter().find(|f| index.get(f).is_none()) {
        return Err(OvaError::invalid(format!(
            "file {file:?} is not in the OVA"
        )));
    }
    let mut warnings = parsed.warnings;
    if !with_ext(".cert").is_empty() {
        warnings.push("the OVA signature (.cert) is not verified".into());
    }
    // `.mf`가 있으면 확인하지 못하는 디스크를 알린다.
    for file in manifest.iter().flat_map(|mf| {
        parsed
            .vm
            .disks
            .iter()
            .map(|d| &d.file)
            .filter(|f| !mf.digests.contains_key(*f))
            .map(|f| (f, mf.unsupported.contains(f)))
    }) {
        warnings.push(match file {
            (f, true) => {
                format!("the manifest uses an unsupported digest for {f}; it is not verified")
            }
            (f, false) => format!("{f} is not listed in the manifest; it is not verified"),
        });
    }
    Ok(Ova {
        vm: parsed.vm,
        warnings: cap_warnings(warnings),
        index,
        manifest: manifest.map(|mf| mf.digests).unwrap_or_default(),
    })
}

/// 겹치는 경고를 빼고 [`MAX_WARNINGS`]개까지 남긴다. 넘는 것은 마지막에 개수만 알린다.
fn cap_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut unique: Vec<String> = warnings
        .into_iter()
        .filter(|w| seen.insert(w.clone()))
        .collect();
    if unique.len() > MAX_WARNINGS {
        let more = unique.len() - MAX_WARNINGS;
        unique.truncate(MAX_WARNINGS);
        unique.push(format!("… and {more} more"));
    }
    unique
}

fn read_text(path: &Path, entry: &OvaEntry) -> Result<String, OvaError> {
    let bytes = read_at(&mut File::open(path)?, entry.offset, entry.size)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| OvaError::invalid(format!("{} is not UTF-8 text", entry.name)))?;
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

/// 항목을 `dest`로 복사(fsync)하면서 해시를 확인한다. 실패하면 `dest`를 지운다.
pub fn extract(
    path: &Path,
    entry: &OvaEntry,
    dest: &Path,
    digest: Option<&Digest>,
) -> Result<(), OvaError> {
    let result = copy_entry(path, entry, dest, digest);
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

fn copy_entry(
    path: &Path,
    entry: &OvaEntry,
    dest: &Path,
    digest: Option<&Digest>,
) -> Result<(), OvaError> {
    let mut src = File::open(path)?;
    src.seek(SeekFrom::Start(entry.offset))?;
    let mut src = src.take(entry.size);
    let mut out = File::create(dest)?;
    let mut hasher = match digest {
        None => Hasher::None,
        Some(Digest::Sha1(_)) => Hasher::Sha1(sha1::Sha1::new()),
        Some(Digest::Sha256(_)) => Hasher::Sha256(sha2::Sha256::new()),
        Some(Digest::Sha384(_)) => Hasher::Sha384(sha2::Sha384::new()),
        Some(Digest::Sha512(_)) => Hasher::Sha512(sha2::Sha512::new()),
    };
    let mut buf = vec![0; 1 << 20];
    let mut copied = 0;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        copied += n as u64;
    }
    if copied != entry.size {
        return Err(truncated());
    }
    out.sync_all()?;
    if let Some(expected) = digest
        && hasher.hex() != expected.hex()
    {
        return Err(OvaError::invalid(format!(
            "checksum mismatch for {}",
            entry.name
        )));
    }
    Ok(())
}

enum Hasher {
    None,
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
}

impl Hasher {
    fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::None => {}
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha384(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    fn hex(self) -> String {
        match self {
            Hasher::None => String::new(),
            Hasher::Sha1(h) => hex::encode(h.finalize()),
            Hasher::Sha256(h) => hex::encode(h.finalize()),
            Hasher::Sha384(h) => hex::encode(h.finalize()),
            Hasher::Sha512(h) => hex::encode(h.finalize()),
        }
    }
}
