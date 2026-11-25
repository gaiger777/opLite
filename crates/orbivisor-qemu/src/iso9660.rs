//! 작은 ISO 9660 이미지 쓰개(계획 57): cloud-init NoCloud 씨앗(라벨 `cidata`)을 외부 도구(genisoimage·
//! cloud-localds) 없이 만든다. 뿌리 디렉터리 하나에 파일만 두고, 긴 소문자 이름은 Joliet 보조 기술자로 담는다
//! (Linux iso9660 드라이버·blkid·libarchive가 Joliet 이름과 라벨을 쓴다). 날짜는 고정이라 같은 입력이면 같은
//! 바이트다(마이그레이션 양쪽 노드의 씨앗이 같다).

use orbivisor_types::cloud_init::CloudInitSeed;
use thiserror::Error;

/// 섹터 크기
const SECTOR: usize = 2048;
/// 파일 수·합 크기·이름 길이 상한
pub const MAX_FILES: usize = 30;
pub const MAX_BYTES: usize = 1024 * 1024;
const MAX_NAME: usize = 64;
/// 볼륨 id 상한(Joliet 32바이트 = UCS-2 16자)
const MAX_VOLUME_ID: usize = 16;
/// 기술자 뒤 고정 자리: 경로 표 넷(19~22), 뿌리 디렉터리(23부터)
const PATH_TABLES: u32 = 19;
const ROOT: u32 = 23;
/// 모든 날짜: 2000-01-01 00:00:00 UTC
const DATE17: &[u8; 17] = b"2000010100000000\0";
const DATE7: [u8; 7] = [100, 1, 1, 0, 0, 0, 0];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IsoError {
    #[error("file name {0:?} must be 1-64 letters, digits, '.', '_' or '-' and unique")]
    BadName(String),
    #[error("at most {MAX_FILES} files")]
    TooManyFiles,
    #[error("the files are larger than {MAX_BYTES} bytes in total")]
    TooLarge,
    #[error("volume id {0:?} must be 1-16 letters, digits or '_'")]
    BadVolumeId(String),
}

/// 파일 `files`(이름, 내용)를 뿌리 디렉터리에 담은 ISO 9660 + Joliet 이미지. 기본 기술자의 볼륨 id는 대문자,
/// Joliet 볼륨 id는 `volume_id` 그대로다.
pub fn build(volume_id: &str, files: &[(&str, &[u8])]) -> Result<Vec<u8>, IsoError> {
    let id_ok = |c: char| c.is_ascii_alphanumeric() || c == '_';
    if volume_id.is_empty() || volume_id.len() > MAX_VOLUME_ID || !volume_id.chars().all(id_ok) {
        return Err(IsoError::BadVolumeId(volume_id.into()));
    }
    if files.len() > MAX_FILES {
        return Err(IsoError::TooManyFiles);
    }
    if files.iter().map(|(_, d)| d.len()).sum::<usize>() > MAX_BYTES {
        return Err(IsoError::TooLarge);
    }
    let name_ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    for (i, (name, _)) in files.iter().enumerate() {
        let dup = files[..i].iter().any(|(n, _)| n == name);
        if name.is_empty() || name.len() > MAX_NAME || !name.chars().all(name_ok) || dup {
            return Err(IsoError::BadName((*name).into()));
        }
    }

    // 기본(8.3 대문자)·Joliet(UCS-2 BE) 이름, 각 디렉터리는 이름순
    let primary = primary_names(files);
    let joliet: Vec<Vec<u8>> = files.iter().map(|(n, _)| ucs2(n)).collect();
    let order = |names: &[Vec<u8>]| {
        let mut idx: Vec<usize> = (0..names.len()).collect();
        idx.sort_by(|&a, &b| names[a].cmp(&names[b]));
        idx
    };
    let (p_order, j_order) = (order(&primary), order(&joliet));
    let p_dir = dir_sectors(&primary);
    let j_dir = dir_sectors(&joliet);
    let j_root = ROOT + p_dir;
    let mut next = j_root + j_dir;
    let extents: Vec<u32> = files
        .iter()
        .map(|(_, d)| {
            // 빈 파일은 데이터가 없어 위치 0을 적는다.
            if d.is_empty() {
                return 0;
            }
            let at = next;
            next += sectors(d.len());
            at
        })
        .collect();
    let total = next;

    let mut img = vec![0u8; total as usize * SECTOR];
    let entries = |names: &[Vec<u8>], order: &[usize]| -> Vec<(Vec<u8>, u32, u32)> {
        order
            .iter()
            .map(|&i| (names[i].clone(), extents[i], files[i].1.len() as u32))
            .collect()
    };
    let p_bytes = p_dir * SECTOR as u32;
    let j_bytes = j_dir * SECTOR as u32;
    write_dir(&mut img, ROOT, p_bytes, &entries(&primary, &p_order));
    write_dir(&mut img, j_root, j_bytes, &entries(&joliet, &j_order));
    for ((_, data), &at) in files
        .iter()
        .zip(&extents)
        .filter(|((_, d), _)| !d.is_empty())
    {
        let off = at as usize * SECTOR;
        img[off..off + data.len()].copy_from_slice(data);
    }
    // 경로 표: 기본 L·M, Joliet L·M(뿌리 하나)
    for (i, (root, big)) in [(ROOT, false), (ROOT, true), (j_root, false), (j_root, true)]
        .into_iter()
        .enumerate()
    {
        let off = (PATH_TABLES as usize + i) * SECTOR;
        img[off] = 1;
        let (loc, parent) = if big {
            (root.to_be_bytes(), 1u16.to_be_bytes())
        } else {
            (root.to_le_bytes(), 1u16.to_le_bytes())
        };
        img[off + 2..off + 6].copy_from_slice(&loc);
        img[off + 6..off + 8].copy_from_slice(&parent);
    }
    let primary_id = volume_id.to_ascii_uppercase();
    write_descriptor(&mut img, 16, total, ROOT, p_bytes, false, &primary_id);
    write_descriptor(&mut img, 17, total, j_root, j_bytes, true, volume_id);
    // 종료 기술자
    let off = 18 * SECTOR;
    img[off] = 255;
    img[off + 1..off + 6].copy_from_slice(b"CD001");
    img[off + 6] = 1;
    Ok(img)
}

/// 씨앗의 NoCloud 파일(이름, 내용): `meta-data`·`user-data`, 있으면 `vendor-data`·`network-config`
pub fn cidata_files(seed: &CloudInitSeed) -> Vec<(&str, &[u8])> {
    let mut files = vec![
        ("meta-data", seed.meta_data.as_bytes()),
        ("user-data", seed.user_data.as_bytes()),
    ];
    if let Some(v) = &seed.vendor_data {
        files.push(("vendor-data", v.as_bytes()));
    }
    if let Some(n) = &seed.network_config {
        files.push(("network-config", n.as_bytes()));
    }
    files
}

/// 기본 디렉터리의 8.3 대문자 이름(`META_DAT.;1`). 겹치면 이름 끝을 숫자로 바꾼다.
fn primary_names(files: &[(&str, &[u8])]) -> Vec<Vec<u8>> {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| match c.to_ascii_uppercase() {
                c @ ('A'..='Z' | '0'..='9') => c,
                _ => '_',
            })
            .collect()
    };
    let mut out: Vec<Vec<u8>> = Vec::new();
    for (name, _) in files {
        let (base, ext) = match name.rsplit_once('.') {
            Some((b, e)) if !b.is_empty() => (clean(b), clean(e)),
            _ => (clean(name), String::new()),
        };
        let ext: String = ext.chars().take(3).collect();
        let full = |base: &str| format!("{base}.{ext};1").into_bytes();
        let short: String = base.chars().take(8).collect();
        let mut candidate = full(&short);
        let mut n = 1;
        while out.contains(&candidate) {
            let stem: String = base.chars().take(8 - n.to_string().len()).collect();
            candidate = full(&format!("{stem}{n}"));
            n += 1;
        }
        out.push(candidate);
    }
    out
}

/// UCS-2 BE 이름(ASCII만 받으므로 그대로 넓힌다)
fn ucs2(s: &str) -> Vec<u8> {
    s.bytes().flat_map(|b| [0, b]).collect()
}

fn sectors(len: usize) -> u32 {
    len.div_ceil(SECTOR) as u32
}

/// 디렉터리 레코드 길이(짝수로 맞춤)
fn record_len(name: &[u8]) -> usize {
    let len = 33 + name.len();
    len + len % 2
}

/// `.`·`..`와 파일 레코드가 드는 섹터 수(레코드는 섹터를 넘지 않는다)
fn dir_sectors(names: &[Vec<u8>]) -> u32 {
    let mut count = 1;
    let mut used = 2 * record_len(&[0]);
    for n in names {
        let len = record_len(n);
        if used + len > SECTOR {
            count += 1;
            used = 0;
        }
        used += len;
    }
    count
}

/// 섹터 `at`부터 뿌리 디렉터리(크기 `size`, 파일 `entries` = 이름·위치·길이)
fn write_dir(img: &mut [u8], at: u32, size: u32, entries: &[(Vec<u8>, u32, u32)]) {
    let mut off = at as usize * SECTOR;
    for name in [[0u8], [1u8]] {
        let r = record(&name, at, size, true);
        img[off..off + r.len()].copy_from_slice(&r);
        off += r.len();
    }
    for (name, extent, len) in entries {
        let r = record(name, *extent, *len, false);
        // 섹터 끝을 넘으면 다음 섹터에서 시작한다.
        if off % SECTOR + r.len() > SECTOR {
            off = off.next_multiple_of(SECTOR);
        }
        img[off..off + r.len()].copy_from_slice(&r);
        off += r.len();
    }
}

/// 디렉터리 레코드 하나
fn record(name: &[u8], extent: u32, len: u32, dir: bool) -> Vec<u8> {
    let mut r = vec![0u8; record_len(name)];
    r[0] = r.len() as u8;
    both32(&mut r[2..10], extent);
    both32(&mut r[10..18], len);
    r[18..25].copy_from_slice(&DATE7);
    r[25] = if dir { 2 } else { 0 };
    both16(&mut r[28..32], 1);
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
    r
}

/// 섹터 `at`의 기본(`joliet`이면 Joliet 보조) 볼륨 기술자
fn write_descriptor(
    img: &mut [u8],
    at: usize,
    total: u32,
    root: u32,
    root_size: u32,
    joliet: bool,
    volume_id: &str,
) {
    let d = &mut img[at * SECTOR..(at + 1) * SECTOR];
    d[0] = if joliet { 2 } else { 1 };
    d[1..6].copy_from_slice(b"CD001");
    d[6] = 1;
    // 문자 칸(시스템·볼륨·볼륨 집합·발행자·준비자·응용·파일 id 셋)은 공백으로 채운다.
    for (start, len) in [
        (8, 32),
        (40, 32),
        (190, 128),
        (318, 128),
        (446, 128),
        (574, 128),
        (702, 37),
        (739, 37),
        (776, 37),
    ] {
        fill_text(&mut d[start..start + len], "", joliet);
    }
    fill_text(&mut d[40..72], volume_id, joliet);
    both32(&mut d[80..88], total);
    if joliet {
        // UCS-2 수준 3
        d[88..91].copy_from_slice(b"%/E");
    }
    both16(&mut d[120..124], 1);
    both16(&mut d[124..128], 1);
    both16(&mut d[128..132], SECTOR as u16);
    // 경로 표: 뿌리 레코드 하나(10바이트)
    both32(&mut d[132..140], 10);
    let tables = PATH_TABLES + if joliet { 2 } else { 0 };
    d[140..144].copy_from_slice(&tables.to_le_bytes());
    d[148..152].copy_from_slice(&(tables + 1).to_be_bytes());
    d[156..190].copy_from_slice(&record(&[0], root, root_size, true));
    d[813..830].copy_from_slice(DATE17);
    d[830..847].copy_from_slice(DATE17);
    // 만료·유효 날짜는 "정하지 않음"(숫자 0)
    for start in [847, 864] {
        d[start..start + 16].fill(b'0');
        d[start + 16] = 0;
    }
    d[881] = 1;
}

/// 문자 칸을 `text`와 공백으로 채운다(Joliet은 UCS-2 BE, 홀수 길이 칸의 끝 바이트는 0).
fn fill_text(field: &mut [u8], text: &str, joliet: bool) {
    if joliet {
        let mut bytes = ucs2(text);
        while bytes.len() + 2 <= field.len() {
            bytes.extend([0, b' ']);
        }
        bytes.resize(field.len(), 0);
        field.copy_from_slice(&bytes);
    } else {
        field.fill(b' ');
        field[..text.len()].copy_from_slice(text.as_bytes());
    }
}

/// 양쪽 바이트 순서(LE 다음 BE) 32비트·16비트
fn both32(f: &mut [u8], v: u32) {
    f[..4].copy_from_slice(&v.to_le_bytes());
    f[4..8].copy_from_slice(&v.to_be_bytes());
}

fn both16(f: &mut [u8], v: u16) {
    f[..2].copy_from_slice(&v.to_le_bytes());
    f[2..4].copy_from_slice(&v.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 시험 읽개: 기술자 `at`(16 기본, 17 Joliet)의 뿌리 디렉터리를 따라 (이름, 내용)을 꺼낸다. 경로 표가 뿌리를
    /// 가리키는지도 본다.
    fn read(img: &[u8], at: usize) -> Vec<(String, Vec<u8>)> {
        let d = &img[at * SECTOR..(at + 1) * SECTOR];
        assert_eq!(&d[1..6], b"CD001");
        let le32 = |b: &[u8]| u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
        let be32 = |b: &[u8]| u32::from_be_bytes(b[..4].try_into().unwrap()) as usize;
        let root = le32(&d[156 + 2..]);
        let root_size = le32(&d[156 + 10..]);
        assert_eq!(be32(&d[156 + 6..]), root);
        // L·M 경로 표의 첫 레코드 = 뿌리
        let l = le32(&d[140..]) * SECTOR;
        let m = be32(&d[148..]) * SECTOR;
        assert_eq!(le32(&img[l + 2..]), root);
        assert_eq!(be32(&img[m + 2..]), root);
        let joliet = d[0] == 2;
        let mut out = Vec::new();
        let mut off = root * SECTOR;
        let end = off + root_size;
        while off < end {
            let len = img[off] as usize;
            if len == 0 {
                off = (off / SECTOR + 1) * SECTOR;
                continue;
            }
            let r = &img[off..off + len];
            let name = &r[33..33 + r[32] as usize];
            if r[25] & 2 == 0 {
                let name = if joliet {
                    name.chunks(2).map(|c| char::from(c[1])).collect()
                } else {
                    String::from_utf8(name.to_vec()).unwrap()
                };
                let (extent, size) = (le32(&r[2..]), le32(&r[10..]));
                assert_eq!(be32(&r[6..]), extent);
                let data = img[extent * SECTOR..extent * SECTOR + size].to_vec();
                out.push((name, data));
            }
            off += len;
        }
        out
    }

    fn seed_files() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("meta-data", b"{\"instance-id\": \"iid-1\"}\n".as_slice()),
            ("user-data", b"#cloud-config\n{}\n".as_slice()),
            ("vendor-data", b"#cloud-config\nruncmd: [true]\n".as_slice()),
            ("network-config", vec![b'x'; 3000].leak()),
        ]
    }

    #[test]
    fn round_trips_through_the_joliet_and_primary_directories() {
        let files = seed_files();
        let img = build("cidata", &files).unwrap();
        let mut got = read(&img, 17);
        got.sort();
        let mut want: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(n, d)| (n.to_string(), d.to_vec()))
            .collect();
        want.sort();
        assert_eq!(got, want);
        let names: Vec<String> = read(&img, 16).into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            ["META_DAT.;1", "NETWORK_.;1", "USER_DAT.;1", "VENDOR_D.;1"]
        );
    }

    #[test]
    fn descriptors_sit_at_fixed_places() {
        let img = build("cidata", &seed_files()).unwrap();
        assert_eq!(img.len() % SECTOR, 0);
        assert!(img[..16 * SECTOR].iter().all(|&b| b == 0));
        // 기본 기술자: 종류 1, CD001, 대문자 볼륨 id
        let pvd = &img[16 * SECTOR..];
        assert_eq!(pvd[0], 1);
        assert_eq!(&pvd[1..6], b"CD001");
        assert_eq!(&pvd[40..72], format!("{:<32}", "CIDATA").as_bytes());
        // 볼륨 크기 = 길이 / 2048(양쪽 바이트 순서)
        let total = (img.len() / SECTOR) as u32;
        assert_eq!(&pvd[80..84], total.to_le_bytes());
        assert_eq!(&pvd[84..88], total.to_be_bytes());
        // Joliet: 종류 2, escape `%/E`, UCS-2 볼륨 id `cidata`
        let svd = &img[17 * SECTOR..];
        assert_eq!(svd[0], 2);
        assert_eq!(&svd[1..6], b"CD001");
        assert_eq!(&svd[88..91], b"%/E");
        assert_eq!(&svd[40..52], b"\0c\0i\0d\0a\0t\0a");
        assert_eq!(&svd[52..54], b"\0 ");
        // 종료 기술자
        assert_eq!(&img[18 * SECTOR..18 * SECTOR + 7], b"\xffCD001\x01");
        // 작은 씨앗은 뿌리 디렉터리가 23·24, 파일이 25부터다.
        assert_eq!(&pvd[158..162], 23u32.to_le_bytes());
        assert_eq!(&svd[158..162], 24u32.to_le_bytes());
    }

    #[test]
    fn same_input_gives_the_same_bytes() {
        let a = build("cidata", &seed_files()).unwrap();
        let b = build("cidata", &seed_files()).unwrap();
        assert_eq!(a, b);
        let other = build("cidata", &[("meta-data", b"x".as_slice())]).unwrap();
        assert_ne!(a, other);
    }

    #[test]
    fn long_names_and_many_files_span_directory_sectors() {
        let names: Vec<String> = (0..MAX_FILES)
            .map(|i| format!("{i:02}-{}", "n".repeat(61)))
            .collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), n.as_bytes())).collect();
        let img = build("cidata", &files).unwrap();
        let got = read(&img, 17);
        assert_eq!(got.len(), MAX_FILES);
        assert!(got.iter().all(|(n, d)| n.as_bytes() == d.as_slice()));
        // 8.3 이름이 겹쳐 끝을 숫자로 바꿨다.
        let primary: Vec<String> = read(&img, 16).into_iter().map(|(n, _)| n).collect();
        let unique: std::collections::BTreeSet<&String> = primary.iter().collect();
        assert_eq!(unique.len(), MAX_FILES);
    }

    #[test]
    fn rejects_bad_names_counts_sizes_and_volume_ids() {
        for bad in ["", "a/b", "a b", "ü", &"a".repeat(65)] {
            assert_eq!(
                build("cidata", &[(bad, b"x".as_slice())]),
                Err(IsoError::BadName(bad.into())),
                "{bad}"
            );
        }
        assert_eq!(
            build("cidata", &[("a", b"x".as_slice()), ("a", b"y".as_slice())]),
            Err(IsoError::BadName("a".into()))
        );
        let names: Vec<String> = (0..=MAX_FILES).map(|i| format!("f{i}")).collect();
        let files: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), b"".as_slice())).collect();
        assert_eq!(build("cidata", &files), Err(IsoError::TooManyFiles));
        let big = vec![0u8; MAX_BYTES + 1];
        assert_eq!(
            build("cidata", &[("big", big.as_slice())]),
            Err(IsoError::TooLarge)
        );
        for bad in ["", "ci-data", &"c".repeat(17)] {
            assert_eq!(
                build(bad, &[]),
                Err(IsoError::BadVolumeId(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn cidata_files_lists_what_the_seed_has() {
        let mut seed = CloudInitSeed {
            instance_id: "iid-1".into(),
            meta_data: "m".into(),
            user_data: "u".into(),
            vendor_data: None,
            network_config: None,
        };
        let names = |s: &CloudInitSeed| {
            cidata_files(s)
                .into_iter()
                .map(|(n, _)| n.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&seed), ["meta-data", "user-data"]);
        seed.vendor_data = Some("v".into());
        seed.network_config = Some("n".into());
        assert_eq!(
            names(&seed),
            ["meta-data", "user-data", "vendor-data", "network-config"]
        );
        assert_eq!(cidata_files(&seed)[3].1, b"n");
    }

    /// 다른 구현(libarchive `bsdtar`)이 목록과 내용을 읽는지. bsdtar가 없으면(Linux CI) 건너뛴다.
    #[test]
    fn bsdtar_reads_the_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seed.iso");
        std::fs::write(&path, build("cidata", &seed_files()).unwrap()).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("bsdtar")
                .args(args)
                .arg(&path)
                .output()
        };
        let Ok(list) = run(&["-tf"]) else {
            eprintln!("bsdtar not found; skipping");
            return;
        };
        assert!(list.status.success(), "{list:?}");
        let mut names: Vec<String> = String::from_utf8(list.stdout)
            .unwrap()
            .lines()
            .map(String::from)
            .filter(|n| n != ".")
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["meta-data", "network-config", "user-data", "vendor-data"]
        );
        let path_str = path.to_str().unwrap().to_string();
        let meta = std::process::Command::new("bsdtar")
            .args(["-xOf", &path_str, "meta-data"])
            .output()
            .unwrap();
        assert!(meta.status.success(), "{meta:?}");
        assert_eq!(meta.stdout, seed_files()[0].1);
    }
}
