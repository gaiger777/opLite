//! 시험용 최소 zip·tar.gz 읽기 검사기(설계 §14 "우리가 짠 최소 읽기 검사기"). 작성기가 낸 바이트를 명세대로 다시
//! 읽어 머리·크기·CRC·오프셋·zip64 칸이 서로 맞는지 엄격하게 본다.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::{DeflateDecoder, GzDecoder};

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    /// 유닉스 모드(외부 속성 위 16비트)
    pub mode: u32,
    /// 0x5455 확장 시각
    pub mtime: Option<i32>,
    pub zip64_local: bool,
    pub zip64_central: bool,
}

#[derive(Debug)]
pub struct Zip {
    pub entries: Vec<ZipEntry>,
    pub zip64_eocd: bool,
}

fn extras(b: &[u8]) -> BTreeMap<u16, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i + 4 <= b.len() {
        let tag = u16_at(b, i);
        let len = u16_at(b, i + 2) as usize;
        assert!(i + 4 + len <= b.len(), "extra field overruns");
        out.insert(tag, b[i + 4..i + 4 + len].to_vec());
        i += 4 + len;
    }
    assert_eq!(i, b.len(), "trailing bytes in extra");
    out
}

/// zip 전체를 읽는다(어긋나면 패닉).
pub fn read_zip(z: &[u8]) -> Zip {
    let eocd = z.len() - 22;
    assert_eq!(
        u32_at(z, eocd),
        0x0605_4b50,
        "EOCD at the very end (no comment)"
    );
    let mut count = u64::from(u16_at(z, eocd + 10));
    let mut cd_size = u64::from(u32_at(z, eocd + 12));
    let mut cd_offset = u64::from(u32_at(z, eocd + 16));
    let mut zip64_eocd = false;
    if eocd >= 20 && u32_at(z, eocd - 20) == 0x0706_4b50 {
        zip64_eocd = true;
        let zpos = u64_at(z, eocd - 20 + 8) as usize;
        assert_eq!(u32_at(z, zpos), 0x0606_4b50);
        assert_eq!(u64_at(z, zpos + 4), 44);
        assert_eq!(zpos + 56, eocd - 20, "zip64 EOCD right before the locator");
        let n = u64_at(z, zpos + 24);
        assert_eq!(n, u64_at(z, zpos + 32));
        if count == 0xFFFF {
            count = n;
        }
        assert_eq!(count, n);
        if cd_size == 0xFFFF_FFFF {
            cd_size = u64_at(z, zpos + 40);
        }
        if cd_offset == 0xFFFF_FFFF {
            cd_offset = u64_at(z, zpos + 48);
        }
        assert_eq!(cd_offset + cd_size, zpos as u64);
    } else {
        assert_eq!(cd_offset + cd_size, eocd as u64);
    }
    let mut entries = Vec::new();
    let mut p = cd_offset as usize;
    let mut next_local = 0u64;
    for _ in 0..count {
        assert_eq!(u32_at(z, p), 0x0201_4b50, "central header");
        let flags = u16_at(z, p + 8);
        let method = u16_at(z, p + 10);
        let crc = u32_at(z, p + 16);
        let mut csize = u64::from(u32_at(z, p + 20));
        let mut usize = u64::from(u32_at(z, p + 24));
        let nlen = u16_at(z, p + 28) as usize;
        let xlen = u16_at(z, p + 30) as usize;
        let clen = u16_at(z, p + 32) as usize;
        let external = u32_at(z, p + 38);
        let mut offset = u64::from(u32_at(z, p + 42));
        let name = z[p + 46..p + 46 + nlen].to_vec();
        let ex = extras(&z[p + 46 + nlen..p + 46 + nlen + xlen]);
        let zip64_central = ex.contains_key(&1);
        if let Some(z64) = ex.get(&1) {
            let mut i = 0;
            if usize == 0xFFFF_FFFF {
                usize = u64_at(z64, i);
                i += 8;
            }
            if csize == 0xFFFF_FFFF {
                csize = u64_at(z64, i);
                i += 8;
            }
            if offset == 0xFFFF_FFFF {
                offset = u64_at(z64, i);
                i += 8;
            }
            assert_eq!(
                i,
                z64.len(),
                "zip64 extra has exactly the overflowing fields"
            );
        }
        let mtime = ex.get(&0x5455).map(|t| {
            assert_eq!(t[0], 1);
            i32::from_le_bytes(t[1..5].try_into().unwrap())
        });
        p += 46 + nlen + xlen + clen;
        // 지역 헤더
        assert_eq!(offset, next_local, "local headers are back to back");
        let l = offset as usize;
        assert_eq!(u32_at(z, l), 0x0403_4b50, "local header");
        assert_eq!(u16_at(z, l + 6), flags);
        assert_eq!(u16_at(z, l + 8), method);
        let lnlen = u16_at(z, l + 26) as usize;
        let lxlen = u16_at(z, l + 28) as usize;
        assert_eq!(&z[l + 30..l + 30 + lnlen], &name[..]);
        let lex = extras(&z[l + 30 + lnlen..l + 30 + lnlen + lxlen]);
        let zip64_local = lex.contains_key(&1);
        let data_start = l + 30 + lnlen + lxlen;
        let raw = &z[data_start..data_start + csize as usize];
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut out = Vec::new();
                DeflateDecoder::new(raw).read_to_end(&mut out).unwrap();
                out
            }
            m => panic!("method {m}"),
        };
        assert_eq!(data.len() as u64, usize, "uncompressed size");
        let mut c = flate2::Crc::new();
        c.update(&data);
        assert_eq!(c.sum(), crc, "crc of {:?}", String::from_utf8_lossy(&name));
        let mut end = data_start + csize as usize;
        if flags & 8 != 0 {
            assert_eq!(u32_at(z, end), 0x0807_4b50, "data descriptor");
            assert_eq!(u32_at(z, end + 4), crc);
            if zip64_local {
                assert_eq!(u64_at(z, end + 8), csize);
                assert_eq!(u64_at(z, end + 16), usize);
                end += 24;
            } else {
                assert_eq!(u64::from(u32_at(z, end + 8)), csize);
                assert_eq!(u64::from(u32_at(z, end + 12)), usize);
                end += 16;
            }
        } else {
            assert_eq!(csize, 0, "no descriptor only for empty entries");
        }
        next_local = end as u64;
        entries.push(ZipEntry {
            name,
            data,
            method,
            flags,
            mode: external >> 16,
            mtime,
            zip64_local,
            zip64_central,
        });
    }
    assert_eq!(
        next_local, cd_offset,
        "central directory follows the last entry"
    );
    assert_eq!(p as u64, cd_offset + cd_size);
    Zip {
        entries,
        zip64_eocd,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarEntry {
    pub path: Vec<u8>,
    pub typeflag: u8,
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
    pub mtime: String,
    pub link: Vec<u8>,
    pub data: Vec<u8>,
    pub pax: BTreeMap<String, Vec<u8>>,
}

fn octal(f: &[u8]) -> u64 {
    let s: String = f
        .iter()
        .take_while(|b| **b != 0 && **b != b' ')
        .map(|b| *b as char)
        .collect();
    if s.is_empty() {
        0
    } else {
        u64::from_str_radix(&s, 8).unwrap()
    }
}

fn cstr(f: &[u8]) -> Vec<u8> {
    f.iter().take_while(|b| **b != 0).copied().collect()
}

fn parse_pax(mut b: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    while !b.is_empty() {
        let sp = b.iter().position(|c| *c == b' ').unwrap();
        let len: usize = std::str::from_utf8(&b[..sp]).unwrap().parse().unwrap();
        let rec = &b[..len];
        assert_eq!(rec[len - 1], b'\n', "pax record ends with newline");
        let kv = &rec[sp + 1..len - 1];
        let eq = kv.iter().position(|c| *c == b'=').unwrap();
        out.insert(
            String::from_utf8(kv[..eq].to_vec()).unwrap(),
            kv[eq + 1..].to_vec(),
        );
        b = &b[len..];
    }
    out
}

/// tar.gz 전체를 읽는다(어긋나면 패닉).
pub fn read_tar_gz(gz: &[u8]) -> Vec<TarEntry> {
    let mut t = Vec::new();
    GzDecoder::new(gz).read_to_end(&mut t).unwrap();
    assert_eq!(t.len() % 512, 0);
    let mut out = Vec::new();
    let mut i = 0;
    let mut pax: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    loop {
        let h = &t[i..i + 512];
        if h.iter().all(|b| *b == 0) {
            assert!(
                t[i..].iter().all(|b| *b == 0),
                "only zero blocks after the end"
            );
            assert!(t.len() - i >= 1024, "two end blocks");
            break;
        }
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(k, b)| {
                if (148..156).contains(&k) {
                    32
                } else {
                    u64::from(*b)
                }
            })
            .sum();
        assert_eq!(octal(&h[148..156]), sum, "header checksum");
        assert_eq!(&h[257..263], b"ustar\0");
        let typeflag = h[156];
        let mut size = octal(&h[124..136]);
        if typeflag != b'x'
            && let Some(s) = pax.get("size")
        {
            size = std::str::from_utf8(s).unwrap().parse().unwrap();
        }
        let data = t[i + 512..i + 512 + size as usize].to_vec();
        i += 512 + (size as usize).div_ceil(512) * 512;
        if typeflag == b'x' {
            pax = parse_pax(&data);
            continue;
        }
        let path = pax.get("path").cloned().unwrap_or_else(|| cstr(&h[0..100]));
        let link = pax
            .get("linkpath")
            .cloned()
            .unwrap_or_else(|| cstr(&h[157..257]));
        let uid = pax
            .get("uid")
            .map(|v| std::str::from_utf8(v).unwrap().parse().unwrap())
            .unwrap_or_else(|| octal(&h[108..116]));
        let gid = pax
            .get("gid")
            .map(|v| std::str::from_utf8(v).unwrap().parse().unwrap())
            .unwrap_or_else(|| octal(&h[116..124]));
        let mtime = pax
            .get("mtime")
            .map(|v| String::from_utf8(v.clone()).unwrap())
            .unwrap_or_else(|| octal(&h[136..148]).to_string());
        out.push(TarEntry {
            path,
            typeflag,
            mode: octal(&h[100..108]) as u32,
            uid,
            gid,
            mtime,
            link,
            data,
            pax: std::mem::take(&mut pax),
        });
    }
    out
}
